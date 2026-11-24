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


# ---- slice 2 (#2255): links, annotations, forms, a signed file ------------
#
# Written object by object by `RawPdf`: neither cairo nor pdfwrite writes link
# destinations, appearance streams or AcroForm fields as a test needs them.
# Everything is deterministic (no dates but the fixed ones below), so a run
# writes the same bytes again.


class RawPdf:
    """A PDF written by hand: numbered objects, a classic xref table."""

    def __init__(self):
        self.objs = []

    def reserve(self):
        self.objs.append(None)
        return len(self.objs)

    def set(self, n, body):
        self.objs[n - 1] = body.encode("latin-1") if isinstance(body, str) else body

    def add(self, body):
        n = self.reserve()
        self.set(n, body)
        return n

    def stream(self, data, extra=""):
        if isinstance(data, str):
            data = data.encode("latin-1")
        head = f"<< {extra} /Length {len(data)} >>\nstream\n".encode("latin-1")
        return self.add(head + data + b"\nendstream")

    def form(self, w, h, ops, resources=""):
        """A form XObject `w` x `h` (an appearance stream)."""
        res = f" /Resources << {resources} >>" if resources else ""
        return self.stream(ops, f"/Type /XObject /Subtype /Form /BBox [0 0 {w:g} {h:g}]{res}")

    def write(self, path, root, info=None):
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = []
        for i, body in enumerate(self.objs, 1):
            assert body is not None, f"object {i} reserved and never set"
            offsets.append(len(out))
            out += f"{i} 0 obj\n".encode() + body + b"\nendobj\n"
        xref = len(out)
        out += f"xref\n0 {len(self.objs) + 1}\n0000000000 65535 f \n".encode()
        for o in offsets:
            out += f"{o:010d} 00000 n \n".encode()
        info_ref = f" /Info {info} 0 R" if info else ""
        out += (
            f"trailer\n<< /Size {len(self.objs) + 1} /Root {root} 0 R{info_ref}"
            f" /ID [<{'2255' * 8}> <{'2255' * 8}>] >>\nstartxref\n{xref}\n%%EOF\n"
        ).encode()
        with open(path, "wb") as f:
            f.write(out)


def pdf_text(s):
    """A PDF literal string."""
    return "(" + s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)") + ")"


HELV = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"


def labels(lines, size=11):
    """A content stream showing `(x, y, text)` lines in Helvetica."""
    ops = ["0 g BT", f"/F1 {size} Tf"]
    for x, y, s in lines:
        ops.append(f"1 0 0 1 {x} {y} Tm {pdf_text(s)} Tj")
    ops.append("ET")
    return "\n".join(ops)


def page(pdf, pages_ref, content, annots=(), extra="", size=A4, font=None):
    res = f"/Resources << /Font << /F1 {font} 0 R >> >>" if font else "/Resources << >>"
    c = pdf.stream(content)
    a = " ".join(f"{r} 0 R" for r in annots)
    return pdf.add(
        f"<< /Type /Page /Parent {pages_ref} 0 R /MediaBox [0 0 {size[0]:g} {size[1]:g}]"
        f" {res} /Contents {c} 0 R /Annots [{a}] {extra} >>"
    )


def make_links(path):
    """Three pages of links: URIs, explicit and named destinations, GoTo
    actions, links that do nothing here, a link by its quads, and a link on
    a `/Rotate 90` page."""
    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    font = pdf.add(HELV)
    p1, p2, p3 = pdf.reserve(), pdf.reserve(), pdf.reserve()

    def link(rect, what):
        r = " ".join(f"{v:g}" for v in rect)
        return pdf.add(f"<< /Type /Annot /Subtype /Link /Rect [{r}] /Border [0 0 0] {what} >>")

    rows = [
        ("Open https://example.com/docxy", "/A << /S /URI /URI (https://example.com/docxy) >>"),
        ("Write to docs@example.com", "/A << /S /URI /URI (mailto:docs@example.com) >>"),
        ("Go to page 2 at 200 %", f"/Dest [{p2} 0 R /XYZ 72 600 2]"),
        ("Go to chapter 3 (a named destination)", "/Dest (chapter3)"),
        ("Go to the old section (GoTo, /Dests)", "/A << /S /GoTo /D /old >>"),
        ("Go to page 2, fitted", f"/A << /S /GoTo /D [{p2} 0 R /Fit] >>"),
        ("Run a script", "/A << /S /JavaScript /JS (app.alert\\(1\\)) >>"),
        ("Launch notes.txt", "/A << /S /Launch /F (notes.txt) >>"),
        ("A javascript: address", "/A << /S /URI /URI (javascript:alert\\(1\\)) >>"),
    ]
    lines = [(72, 780, "Links")]
    annots = []
    for i, (label, what) in enumerate(rows):
        y = 740 - i * 30
        lines.append((76, y + 4, label))
        annots.append(link([72, y, 330, y + 18], what))
    # A link active only over its quadrilateral: the top-left part of its
    # rectangle (upper left, upper right, lower left, lower right).
    lines.append((76, 434, "Quad link (only this part)"))
    annots.append(
        link(
            [72, 400, 400, 450],
            "/QuadPoints [72 450 250 450 72 428 250 428]"
            " /A << /S /URI /URI (https://example.com/quad) >>",
        )
    )
    page(pdf, pages, labels(lines), annots, size=A4, font=font)
    pdf.set(p1, pdf.objs.pop())  # the page dict just added is page 1
    page(pdf, pages, labels([(72, 780, "Page two"), (72, 600, "XYZ lands here")]), size=A4, font=font)
    pdf.set(p2, pdf.objs.pop())
    back = link([72, 700, 300, 716], f"/Dest [{p1} 0 R /XYZ null null 0]")
    page(
        pdf,
        pages,
        labels([(72, 760, "Chapter 3, on a page turned a quarter right"), (76, 704, "Back to page 1")]),
        [back],
        extra="/Rotate 90",
        size=A4,
        font=font,
    )
    pdf.set(p3, pdf.objs.pop())
    names = pdf.add(f"<< /Names [(chapter3) [{p3} 0 R /FitH 400]] >>")
    pdf.set(pages, f"<< /Type /Pages /Kids [{p1} 0 R {p2} 0 R {p3} 0 R] /Count 3 >>")
    pdf.set(
        catalog,
        f"<< /Type /Catalog /Pages {pages} 0 R /Names << /Dests {names} 0 R >>"
        f" /Dests << /old [{p2} 0 R /FitR 100 100 300 300] >> >>",
    )
    info = pdf.add("<< /Title (Links corpus) /Author (docxy) >>")
    pdf.write(path, catalog, info)


# The annotations of `annots.pdf` and `annots-noap.pdf`: (subtype, rect, the
# dictionary's own entries, the appearance's operators in its BBox or None).
REVIEWER = "/T (Reviewer) /M (D:20261124120000Z)"
ANNOTS = [
    (
        "Highlight",
        [72, 758, 272, 774],
        f"/QuadPoints [72 774 272 774 72 758 272 758] /C [1 1 0] /Contents (Highlighted words) {REVIEWER}",
        "/GS0 gs 1 1 0 rg 0 0 200 16 re f",
    ),
    (
        "Underline",
        [72, 724, 272, 742],
        "/QuadPoints [72 742 272 742 72 726 272 726] /C [0 0 1]",
        "0 0 1 RG 1.5 w 0 2 m 200 2 l S",
    ),
    (
        "StrikeOut",
        [72, 694, 272, 712],
        f"/QuadPoints [72 712 272 712 72 696 272 696] /C [1 0 0] /Contents (Cut this) {REVIEWER}",
        "1 0 0 RG 1.5 w 0 9 m 200 9 l S",
    ),
    (
        "Squiggly",
        [72, 664, 272, 682],
        "/QuadPoints [72 682 272 682 72 666 272 666] /C [0 0.6 0]",
        "0 0.6 0 RG 1 w 0 2 m " + " ".join(f"{x} {2 + 3 * (i % 2)} l" for i, x in enumerate(range(4, 204, 4))) + " S",
    ),
    (
        "Text",
        [320, 740, 344, 764],
        f"/Name /Note /C [1 0.82 0] /Contents (A sticky note) {REVIEWER}",
        "1 0.82 0 rg 0 0 0 RG 1 w 0.5 0.5 23 23 re B 4 17 m 20 17 l 4 12 m 20 12 l 4 7 m 14 7 l S",
    ),
    (
        "FreeText",
        [320, 650, 520, 700],
        "/DA (/Helv 12 Tf 0 0 1 rg) /Contents (Free text) /C [0 0 1]",
        "0 0 1 RG 1 w 0.5 0.5 199 49 re S 0 0 1 rg 10 30 120 6 re f 10 16 80 6 re f",
    ),
    (
        "Ink",
        [72, 560, 272, 640],
        "/InkList [[80 570 120 630 160 580 200 630 260 570]] /C [0.5 0 0.5] /BS << /W 3 >>",
        "0.5 0 0.5 RG 3 w 1 J 1 j 8 10 m 48 70 l 88 20 l 128 70 l 188 10 l S",
    ),
    (
        "Stamp",
        [320, 560, 520, 630],
        "/Name /Approved /C [0.8 0 0]",
        "0.8 0 0 RG 4 w 2 2 196 66 re S 0.8 0 0 rg 20 28 160 14 re f",
    ),
    (
        "Square",
        [72, 460, 172, 540],
        "/C [0 0.4 0.8] /IC [0.8 0.9 1] /BS << /W 2 >>",
        "0 0.4 0.8 RG 0.8 0.9 1 rg 2 w 1 1 98 78 re B",
    ),
    (
        "Circle",
        [200, 460, 300, 540],
        "/C [0.8 0.4 0] /BS << /W 2 >>",
        "0.8 0.4 0 RG 2 w 50 79 m 77.6 79 99 61.3 99 40 c 99 18.7 77.6 1 50 1 c"
        " 22.4 1 1 18.7 1 40 c 1 61.3 22.4 79 50 79 c S",
    ),
    (
        "Line",
        [320, 470, 520, 530],
        "/L [324 480 516 520] /C [0 0 0] /BS << /W 2 >>",
        "0 0 0 RG 2 w 4 10 m 196 50 l S",
    ),
    (
        "Polygon",
        [72, 340, 272, 440],
        "/Vertices [80 350 260 350 230 430 110 430] /C [0 0.5 0] /BS << /W 2 >>",
        "0 0.5 0 RG 2 w 8 10 m 188 10 l 158 90 l 38 90 l h S",
    ),
    (
        "PolyLine",
        [320, 340, 520, 440],
        "/Vertices [330 350 380 430 430 350 510 430] /C [0 0 0.6] /BS << /W 2 >>",
        "0 0 0.6 RG 2 w 10 10 m 60 90 l 110 10 l 190 90 l S",
    ),
]
# Drawn by neither file's reader: Hidden (`/F` bit 2), text and all.
HIDDEN = ("Square", [72, 200, 172, 280], "/F 2 /C [1 0 0] /IC [1 0 0] /Contents (Hidden from view)", "1 0 0 rg 0 0 100 80 re f")


def make_annots(path, with_ap):
    """One A4 page with every annotation kind slice 2 draws, with their
    appearance streams (`annots.pdf`) or without (`annots-noap.pdf`, whose
    page is otherwise blank, so every pixel in a rectangle is the
    annotation's)."""
    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    multiply = pdf.add("<< /Type /ExtGState /BM /Multiply >>")
    refs = []
    for subtype, rect, entries, ops in [*ANNOTS, HIDDEN]:
        r = " ".join(f"{v:g}" for v in rect)
        ap = ""
        if with_ap:
            res = f"/ExtGState << /GS0 {multiply} 0 R >>" if "/GS0" in ops else ""
            form = pdf.form(rect[2] - rect[0], rect[3] - rect[1], ops, res)
            ap = f"/AP << /N {form} 0 R >>"
        flags = "" if "/F " in entries else "/F 4"
        refs.append(pdf.add(f"<< /Type /Annot /Subtype /{subtype} /Rect [{r}] {flags} {entries} {ap} >>"))
    # Grey bars where text would be, under the text markup (annots.pdf only).
    bars = "0.6 g " + " ".join(f"72 {y} 200 12 re f" for y in (760, 728, 698, 668)) if with_ap else ""
    p = page(pdf, pages, bars, refs)
    pdf.set(pages, f"<< /Type /Pages /Kids [{p} 0 R] /Count 1 >>")
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>")
    pdf.write(path, catalog)


def check_ap(pdf, on, w, h, round_=False):
    """A check box's or radio button's on and off appearances."""
    if round_:
        r = min(w, h) / 2
        cx, cy = w / 2, h / 2
        k = 0.5523 * (r - 4)
        dot = (
            f"0 g {cx} {cy + r - 4} m {cx + k} {cy + r - 4} {cx + r - 4} {cy + k} {cx + r - 4} {cy} c"
            f" {cx + r - 4} {cy - k} {cx + k} {cy - r + 4} {cx} {cy - r + 4} c"
            f" {cx - k} {cy - r + 4} {cx - r + 4} {cy - k} {cx - r + 4} {cy} c"
            f" {cx - r + 4} {cy + k} {cx - k} {cy + r - 4} {cx} {cy + r - 4} c f"
        )
        frame = f"0 G 1 w 0.5 0.5 {w - 1} {h - 1} re S"
        on_ops, off_ops = f"{frame} {dot}", frame
    else:
        frame = f"0 G 1 w 0.5 0.5 {w - 1} {h - 1} re S"
        on_ops = f"{frame} 0 G 2 w 3 {h / 2} m {w * 0.4} 3 l {w - 3} {h - 3} l S"
        off_ops = frame
    a = pdf.form(w, h, on_ops)
    b = pdf.form(w, h, off_ops)
    return f"/AP << /N << /{on} {a} 0 R /Off {b} 0 R >> >>"


TX, BTN, CH, SIG = "/FT /Tx", "/FT /Btn", "/FT /Ch", "/FT /Sig"


def make_forms(path):
    """An AcroForm over two A4 pages: text fields (plain with MaxLen,
    required, multi-line, password, comb, read-only), a check box, a radio
    group, combo boxes (plain and editable) and list boxes (single and
    multi-select), and a field on page 2."""
    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    font = pdf.add(HELV)
    p1, p2 = pdf.reserve(), pdf.reserve()
    fields, page1_annots, lines = [], [], [(72, 790, "A form")]
    da = "/DA (/Helv 11 Tf 0 g)"
    mk = "/MK << /BC [0.5 0.5 0.5] /BG [1 1 1] >>"

    def widget_of(field_entries, rect, extra="", page_ref=p1, annots=page1_annots):
        r = " ".join(f"{v:g}" for v in rect)
        n = pdf.add(
            f"<< /Type /Annot /Subtype /Widget /Rect [{r}] /F 4 /P {page_ref} 0 R {mk}"
            f" {field_entries} {extra} >>"
        )
        fields.append(n)
        annots.append(n)
        return n

    # Rows from the top down: `top` is a row's top edge.
    top = 770
    for label, name, entries, h in [
        ("Name", "name", f"{TX} /MaxLen 20 /TU (Your name) {da}", 20),
        ("Email", "email", f"{TX} /Ff 2 /TU (Email address) {da}", 20),
        ("Notes", "notes", f"{TX} /Ff 4096 {da}", 50),
        ("PIN", "pin", f"{TX} /Ff 8192 {da}", 20),
        ("Code", "code", f"{TX} /Ff 16777216 /MaxLen 6 {da}", 20),
        ("Member id", "id", f"{TX} /Ff 1 /V (A-17) {da}", 20),
    ]:
        lines.append((72, top - 15, label))
        widget_of(f"/T ({name}) {entries}", [200, top - h, 450, top])
        top -= h + 16
    y = top - 16
    # A check box.
    lines.append((72, y + 4, "I agree"))
    widget_of(f"/T (agree) {BTN} /V /Off", [200, y, 216, y + 16], "/AS /Off " + check_ap(pdf, "Yes", 16, 16))
    y -= 32
    # A radio group: one field, three widgets.
    group = pdf.reserve()
    kids = []
    lines.append((72, y + 4, "Colour"))
    for i, colour in enumerate(("Red", "Green", "Blue")):
        x = 200 + i * 80
        lines.append((x + 20, y + 4, colour))
        r = f"{x} {y} {x + 16} {y + 16}"
        k = pdf.add(
            f"<< /Type /Annot /Subtype /Widget /Rect [{r}] /F 4 /P {p1} 0 R {mk} /Parent {group} 0 R"
            f" /AS /Off {check_ap(pdf, colour, 16, 16, round_=True)} >>"
        )
        kids.append(k)
        page1_annots.append(k)
    pdf.set(
        group,
        f"<< /T (colour) {BTN} /Ff 49152 /V /Off /Kids [{' '.join(f'{k} 0 R' for k in kids)}] >>",
    )
    fields.append(group)
    top = y - 20
    for label, name, entries, h in [
        ("Size", "size", f"{CH} /Ff 131072 /Opt [(Small) (Medium) (Large)] {da}", 20),
        ("City", "city", f"{CH} /Ff 393216 /Opt [(Paris) (Berlin) (Tokyo)] {da}", 20),
        ("Fruit", "fruit", f"{CH} /Opt [(Apple) (Banana) (Cherry) (Date)] {da}", 56),
        ("Tags", "tags", f"{CH} /Ff 2097152 /Opt [[(r) (Red)] [(g) (Green)] [(b) (Blue)]] {da}", 56),
    ]:
        lines.append((72, top - 15, label))
        widget_of(f"/T ({name}) {entries}", [200, top - h, 450, top])
        top -= h + 16
    page(pdf, pages, labels(lines), page1_annots, font=font)
    pdf.set(p1, pdf.objs.pop())
    page2_annots = []
    widget_of(f"/T (page2) {TX} {da}", [200, 700, 450, 720], page_ref=p2, annots=page2_annots)
    page(pdf, pages, labels([(72, 705, "On page 2")]), page2_annots, font=font)
    pdf.set(p2, pdf.objs.pop())
    pdf.set(pages, f"<< /Type /Pages /Kids [{p1} 0 R {p2} 0 R] /Count 2 >>")
    acro = (
        f"<< /Fields [{' '.join(f'{f} 0 R' for f in fields)}] /DA (/Helv 0 Tf 0 g)"
        f" /DR << /Font << /Helv {font} 0 R >> >> >>"
    )
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R /AcroForm {acro} >>")
    info = pdf.add("<< /Title (Forms corpus) /Author (docxy) >>")
    pdf.write(path, catalog, info)


def make_signed(path):
    """A signed form: a signature field whose /V is a signature dictionary
    (placeholders, no real signature), and a text field to fill."""
    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    font = pdf.add(HELV)
    p1 = pdf.reserve()
    sig_v = pdf.add(
        "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached"
        f" /ByteRange [0 0 0 0] /Contents <{'00' * 64}> /M (D:20261124120000Z) >>"
    )
    box = pdf.form(200, 40, "0 G 1 w 0.5 0.5 199 39 re S")
    sig = pdf.add(
        f"<< /Type /Annot /Subtype /Widget /Rect [72 600 272 640] /F 4 /P {p1} 0 R"
        f" /T (Signature1) {SIG} /V {sig_v} 0 R /AP << /N {box} 0 R >> >>"
    )
    name = pdf.add(
        f"<< /Type /Annot /Subtype /Widget /Rect [72 700 322 720] /F 4 /P {p1} 0 R"
        f" /T (name) {TX} /DA (/Helv 11 Tf 0 g) /MK << /BC [0.5 0.5 0.5] >> >>"
    )
    page(pdf, pages, labels([(72, 760, "A signed form"), (72, 650, "Signed here")]), [name, sig], font=font)
    pdf.set(p1, pdf.objs.pop())
    pdf.set(pages, f"<< /Type /Pages /Kids [{p1} 0 R] /Count 1 >>")
    pdf.set(
        catalog,
        f"<< /Type /Catalog /Pages {pages} 0 R /AcroForm << /Fields [{name} 0 R {sig} 0 R]"
        f" /SigFlags 3 /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv {font} 0 R >> >> >> >>",
    )
    pdf.write(path, catalog)


SLICE2 = {
    "links": make_links,
    "annots": lambda p: make_annots(p, True),
    "annots-noap": lambda p: make_annots(p, False),
    "forms": make_forms,
    "signed": make_signed,
}


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
    """With no arguments, every file; with names (`links forms ...`), only
    those and their references: a run for the slice 2 files leaves the
    slice 1 files, which cairo and gs do not write byte for byte again,
    as they are."""
    for tool in ("gs", "pdftoppm"):
        if not shutil.which(tool):
            sys.exit(f"{tool} is needed")
    only = set(sys.argv[1:])
    unknown = only - {"text", "columns", "scanned", "slides", "vector", "rotated", "source", *SLICE2}
    if unknown:
        sys.exit(f"no such corpus file: {', '.join(sorted(unknown))}")
    want = lambda name: not only or name in only
    os.makedirs(REF, exist_ok=True)
    for f in os.listdir(REF):
        if want(f.rsplit("-", 1)[0]):
            os.remove(os.path.join(REF, f))
    made = {
        "text": make_text,
        "columns": make_columns,
        "scanned": make_scanned,
        "slides": make_slides,
    }
    for name, make in made.items():
        if want(name):
            make(os.path.join(OUT, f"{name}.pdf"))
    if want("vector"):
        gs(VECTOR_PS, os.path.join(OUT, "vector.pdf"))
    if want("rotated"):
        gs(ROTATED_PS, os.path.join(OUT, "rotated.pdf"))
    for name, make in SLICE2.items():
        if want(name):
            make(os.path.join(OUT, f"{name}.pdf"))
    # References for the files drawn against poppler; the slice 2 files
    # other than annots.pdf are checked by what they hold, not by pixels.
    for name in [*made, "vector", "rotated", "annots"]:
        if want(name):
            references(os.path.join(OUT, f"{name}.pdf"), name)
    if want("source"):
        references(SOURCE_PDF, "source")
    # The UI cases open only the harness's own files (uiharness/tests/
    # cases.rs): byte-identical copies of the ones they use.
    fixtures = os.path.join(ROOT, "uiharness", "fixtures")
    for name in ("slides", "text", "forms", "links", "annots", "signed"):
        if want(name):
            shutil.copyfile(
                os.path.join(OUT, f"{name}.pdf"),
                os.path.join(fixtures, f"pdf-view-{name}.pdf"),
            )


if __name__ == "__main__":
    main()
