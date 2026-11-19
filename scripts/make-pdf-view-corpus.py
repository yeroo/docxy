#!/usr/bin/env python3
"""Make the PDF view's corpus (#2245): corpus/pdf-view/*.pdf and their
reference renderings, corpus/pdf-view/ref/<name>-<page>.png.

The suite's tests never run this: they read what it wrote, which is
committed. Run it again only to change the corpus, then commit every file
it writes (and say why in the commit).

  python3 scripts/make-pdf-view-corpus.py

Tools (the versions the committed files were made with are in
corpus/pdf-view/README.md):

- pycairo, for the files with Unicode text (DejaVu, embedded as subsets
  with ToUnicode maps), columns, pictures and bookmarks;
- Ghostscript's pdfwrite, for what cairo cannot write: DeviceCMYK and
  DeviceGray colours, and a page's /Rotate. `-dPDFSETTINGS=/prepress
  -dEmbedAllFonts=true` embeds the base-14 fonts too, so a reference does
  not depend on the substitute fonts of the machine that made it, and
  `-dAutoRotatePages=/None` keeps gs from rotating pages by itself;
- Pillow, for the JPEG of the "scanned" page;
- pdftoppm (poppler), for the references: `pdftoppm -r 72 -png`, one pixel
  per point, which is what the test renders at scale 1.
"""

import io
import os
import shutil
import subprocess
import sys

import cairo
from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "corpus", "pdf-view")
REF = os.path.join(OUT, "ref")
SOURCE_PDF = os.path.join(ROOT, "corpus", "word-import", "source.pdf")

SERIF = "DejaVu Serif"
SANS = "DejaVu Sans"

A4 = (595.0, 842.0)
LETTER = (612.0, 792.0)
SLIDE = (960.0, 540.0)


def text_lines(ctx, x, y, lines, size, leading=1.35, face=SERIF, bold=False):
    ctx.select_font_face(
        face,
        cairo.FONT_SLANT_NORMAL,
        cairo.FONT_WEIGHT_BOLD if bold else cairo.FONT_WEIGHT_NORMAL,
    )
    ctx.set_font_size(size)
    for line in lines:
        ctx.move_to(x, y)
        ctx.show_text(line)
        y += size * leading
    return y


def make_text(path):
    """Two A4 pages of running text: Latin, Cyrillic and Greek."""
    s = cairo.PDFSurface(path, *A4)
    s.set_metadata(cairo.PDFMetadata.TITLE, "Text corpus")
    s.set_metadata(cairo.PDFMetadata.AUTHOR, "docxy")
    ctx = cairo.Context(s)
    ctx.set_source_rgb(0, 0, 0)
    y = text_lines(ctx, 72, 90, ["The PDF view corpus"], 20, face=SANS, bold=True)
    y = text_lines(
        ctx,
        72,
        y + 10,
        [
            "Hello from the first page. The quick brown fox jumps over",
            "the lazy dog, and the five boxing wizards jump quickly.",
            "Pack my box with five dozen liquor jugs; how vexingly quick",
            "daft zebras jump. Sphinx of black quartz, judge my vow.",
        ],
        11,
    )
    y = text_lines(ctx, 72, y + 14, ["Русский текст"], 14, face=SANS, bold=True)
    y = text_lines(
        ctx,
        72,
        y + 4,
        [
            "Привет, мир! Съешь же ещё этих мягких французских булок,",
            "да выпей чаю. Широкая электрификация южных губерний.",
        ],
        11,
    )
    y = text_lines(ctx, 72, y + 14, ["Ελληνικό κείμενο"], 14, face=SANS, bold=True)
    text_lines(
        ctx,
        72,
        y + 4,
        [
            "Καλημέρα κόσμε. Ξεσκεπάζω την ψυχοφθόρα βδελυγμία.",
            "Γαζέες καὶ μυρτιὲς δὲν θὰ βρῶ πιὰ στὸ χρυσαφὶ ξέφωτο.",
        ],
        11,
    )
    s.show_page()
    y = text_lines(ctx, 72, 90, ["Second page"], 16, face=SANS, bold=True)
    text_lines(
        ctx,
        72,
        y + 8,
        [
            "This line starts the second page, so a selection can run",
            "from the first page onto this one. Searching for wizard",
            "finds it on both pages: the five boxing wizards again.",
        ]
        # Enough on the page that a blank one fails the comparison.
        + [
            f"Line {n} of the filler, so the page has ink: a quiet river runs past the mill."
            for n in range(1, 41)
        ],
        11,
    )
    s.finish()


COLUMN_A = [
    "Left column, first line of text.",
    "The left column is read first,",
    "top to bottom, as a reader",
    "follows a newsletter's page.",
    "It ends with the word alpha.",
]
COLUMN_B = [
    "Right column starts here and",
    "is read after the whole left",
    "column, never line by line",
    "across the gutter between them.",
    "It ends with the word omega.",
]


def make_columns(path):
    """A Letter page in two columns of 9 pt text."""
    s = cairo.PDFSurface(path, *LETTER)
    ctx = cairo.Context(s)
    ctx.set_source_rgb(0, 0, 0)
    text_lines(ctx, 54, 72, ["Two columns"], 18, face=SANS, bold=True)
    text_lines(ctx, 54, 110, COLUMN_A * 6, 9)
    text_lines(ctx, 324, 110, COLUMN_B * 6, 9)
    s.finish()


def make_scanned(path):
    """A page that is two pictures: a DCT (JPEG) grey "scan" and a Flate
    RGB picture, with a caption drawn as text."""
    w, h = 360, 240
    scan = Image.new("L", (w, h), 245)
    d = ImageDraw.Draw(scan)
    for i in range(10):
        d.rectangle([20, 20 + i * 20, 340 - (i * 13) % 90, 30 + i * 20], fill=40)
    scan = scan.filter(ImageFilter.GaussianBlur(0.6))
    jpeg = io.BytesIO()
    scan.save(jpeg, "JPEG", quality=85)
    jpeg = jpeg.getvalue()
    # cairo draws the decoded surface; the JPEG bytes are what it embeds.
    gray = cairo.ImageSurface(cairo.FORMAT_RGB24, w, h)
    data = gray.get_data()
    stride = gray.get_stride()
    px = Image.open(io.BytesIO(jpeg)).convert("L").load()
    for yy in range(h):
        for xx in range(w):
            v = px[xx, yy]
            o = yy * stride + xx * 4
            data[o : o + 4] = bytes([v, v, v, 255])
    gray.mark_dirty()
    gray.set_mime_data(cairo.MIME_TYPE_JPEG, jpeg)

    rgb = cairo.ImageSurface(cairo.FORMAT_RGB24, 240, 160)
    c = cairo.Context(rgb)
    grad = cairo.LinearGradient(0, 0, 240, 160)
    grad.add_color_stop_rgb(0, 0.9, 0.2, 0.2)
    grad.add_color_stop_rgb(1, 0.1, 0.3, 0.9)
    c.set_source(grad)
    c.paint()
    c.set_source_rgb(1, 1, 0.2)
    c.arc(120, 80, 45, 0, 6.2832)
    c.fill()

    s = cairo.PDFSurface(path, *A4)
    ctx = cairo.Context(s)
    ctx.save()
    ctx.translate(72, 72)
    ctx.set_source_surface(gray, 0, 0)
    ctx.paint()
    ctx.restore()
    ctx.save()
    ctx.translate(72, 360)
    ctx.set_source_surface(rgb, 0, 0)
    ctx.paint()
    ctx.restore()
    ctx.set_source_rgb(0, 0, 0)
    text_lines(ctx, 72, 560, ["A scanned page and a picture."], 12)
    s.finish()


def make_slides(path):
    """Three 16:9 slides with nested bookmarks."""
    s = cairo.PDFSurface(path, *SLIDE)
    s.set_metadata(cairo.PDFMetadata.TITLE, "Slides corpus")
    ctx = cairo.Context(s)
    titles = ["Quarterly review", "Revenue by region", "Next steps"]
    colors = [(0.12, 0.31, 0.62), (0.13, 0.52, 0.29), (0.74, 0.26, 0.10)]
    for i, (title, col) in enumerate(zip(titles, colors)):
        ctx.set_source_rgb(*col)
        ctx.rectangle(0, 0, SLIDE[0], 90)
        ctx.fill()
        ctx.set_source_rgb(1, 1, 1)
        text_lines(ctx, 40, 60, [title], 32, face=SANS, bold=True)
        ctx.set_source_rgb(0.15, 0.15, 0.15)
        text_lines(
            ctx,
            60,
            160,
            [f"Slide {i + 1} of 3", "A point worth making", "Another point"],
            22,
            face=SANS,
        )
        for b in range(4):
            ctx.set_source_rgb(*col)
            hgt = 40 + 30 * ((b + i) % 4)
            ctx.rectangle(560 + b * 80, 460 - hgt, 50, hgt)
            ctx.fill()
        s.show_page()
    top = s.add_outline(cairo.PDF_OUTLINE_ROOT, "Review", "page=1", cairo.PDFOutlineFlags.OPEN)
    s.add_outline(top, "Quarterly review", "page=1", 0)
    s.add_outline(top, "Revenue by region", "page=2", 0)
    s.add_outline(cairo.PDF_OUTLINE_ROOT, "Next steps", "page=3", 0)
    s.finish()


VECTOR_PS = r"""
%!PS
/Helvetica-Bold findfont 18 scalefont setfont
0 setgray 72 770 moveto (Vector diagram) show
% A CMYK fill, a grey fill, an RGB stroke.
0.9 0.1 0.0 0.0 setcmykcolor 72 560 180 140 rectfill
0.55 setgray 300 560 200 140 rectfill
0.8 0.1 0.1 setrgbcolor 4 setlinewidth
newpath 72 520 moveto 520 420 lineto stroke
% A dashed curve.
0 0 0.6 0.2 setcmykcolor 2 setlinewidth [8 4] 0 setdash
newpath 72 380 moveto 200 480 360 280 520 380 curveto stroke
[] 0 setdash
% A clipped circle: only its left half shows.
gsave
newpath 150 100 moveto 300 100 lineto 300 300 lineto 150 300 lineto closepath clip
0.2 0.6 0.2 setrgbcolor newpath 300 200 90 0 360 arc fill
grestore
% Line joins and caps.
0 setgray 10 setlinewidth 1 setlinejoin 1 setlinecap
newpath 360 120 moveto 420 220 lineto 480 120 lineto stroke
/Helvetica findfont 11 scalefont setfont
72 60 moveto (Paths, fills, strokes, dashes, clipping, CMYK and grey.) show
showpage
"""

ROTATED_PS = r"""
%!PS
[ {ThisPage} << /Rotate 90 >> /PUT pdfmark
/Times-Roman findfont 24 scalefont setfont
0 setgray 72 720 moveto (Rotated page) show
/Times-Roman findfont 12 scalefont setfont
72 690 moveto (This page is stored upright and shown turned a quarter right.) show
0.2 0.4 0.8 setrgbcolor 72 500 200 120 rectfill
showpage
"""


def gs(ps, path):
    src = path + ".ps"
    with open(src, "w") as f:
        f.write(ps)
    try:
        subprocess.run(
            [
                "gs",
                "-q",
                "-dNOPAUSE",
                "-dBATCH",
                "-dSAFER",
                "-sDEVICE=pdfwrite",
                "-dPDFSETTINGS=/prepress",
                "-dEmbedAllFonts=true",
                "-dAutoRotatePages=/None",
                "-sPAPERSIZE=a4",
                "-dFIXEDMEDIA",
                f"-sOutputFile={path}",
                src,
            ],
            check=True,
        )
    finally:
        os.remove(src)


def references(pdf, name):
    """pdftoppm -r 72: <name>-<page>.png, the page number unpadded."""
    tmp = os.path.join(REF, f".{name}")
    subprocess.run(["pdftoppm", "-r", "72", "-png", pdf, tmp], check=True)
    made = sorted(f for f in os.listdir(REF) if f.startswith(f".{name}-"))
    for f in made:
        page = int(f[len(name) + 2 : -4])
        os.replace(os.path.join(REF, f), os.path.join(REF, f"{name}-{page}.png"))


def main():
    for tool in ("gs", "pdftoppm"):
        if not shutil.which(tool):
            sys.exit(f"{tool} is needed")
    os.makedirs(REF, exist_ok=True)
    for f in os.listdir(REF):
        os.remove(os.path.join(REF, f))
    made = {
        "text": make_text,
        "columns": make_columns,
        "scanned": make_scanned,
        "slides": make_slides,
    }
    for name, make in made.items():
        make(os.path.join(OUT, f"{name}.pdf"))
    gs(VECTOR_PS, os.path.join(OUT, "vector.pdf"))
    gs(ROTATED_PS, os.path.join(OUT, "rotated.pdf"))
    for name in [*made, "vector", "rotated"]:
        references(os.path.join(OUT, f"{name}.pdf"), name)
    references(SOURCE_PDF, "source")
    # pdf-view.uit opens only the harness's own files (uiharness/tests/
    # cases.rs): byte-identical copies of the two it uses.
    fixtures = os.path.join(ROOT, "uiharness", "fixtures")
    for name in ("slides", "text"):
        shutil.copyfile(
            os.path.join(OUT, f"{name}.pdf"),
            os.path.join(fixtures, f"pdf-view-{name}.pdf"),
        )


if __name__ == "__main__":
    main()
