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
  per point, which is what the test renders at scale 1;
- cryptography (AES) and pdftotext (poppler), for the encrypted files of
  slice 3 (#2256): written by the generator's own security handler, and
  checked by poppler with each password as they are made.
"""

import io
import os
import re
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

    def build(self, root, info=None, security=None, file_id=None, version="1.7"):
        """The file's bytes and its xref's offset. `security` (a [`Security`])
        encrypts every object but its own `/Encrypt` dictionary (#2256)."""
        file_id = file_id or "2255" * 8
        enc_ref = None
        if security:
            enc_ref = self.add(security.dictionary())
        out = bytearray(f"%PDF-{version}\n%\xe2\xe3\xcf\xd3\n".encode("latin-1"))
        offsets = []
        for i, body in enumerate(self.objs, 1):
            assert body is not None, f"object {i} reserved and never set"
            if security and i != enc_ref:
                body = security.encrypt_object(i, body)
            offsets.append(len(out))
            out += f"{i} 0 obj\n".encode() + body + b"\nendobj\n"
        xref = len(out)
        out += f"xref\n0 {len(self.objs) + 1}\n0000000000 65535 f \n".encode()
        for o in offsets:
            out += f"{o:010d} 00000 n \n".encode()
        info_ref = f" /Info {info} 0 R" if info else ""
        enc = f" /Encrypt {enc_ref} 0 R" if enc_ref else ""
        out += (
            f"trailer\n<< /Size {len(self.objs) + 1} /Root {root} 0 R{info_ref}{enc}"
            f" /ID [<{file_id}> <{file_id}>] >>\nstartxref\n{xref}\n%%EOF\n"
        ).encode()
        return bytes(out), xref

    def write(self, path, root, info=None, **kw):
        out, _ = self.build(root, info, **kw)
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


# ---- slice 3 (#2256): encrypted, incremental, object-stream and damaged files ----
#
# The standard security handler written by hand (ISO 32000-2 §7.6.4), so the
# files test PdfCraft against an independent writer; their references are
# drawn by poppler from the encrypted files (`pdftoppm -upw`), so a mistake
# here fails when the corpus is made, not in the suite's tests. Every salt,
# IV and key is fixed: a run writes the same bytes again.

PAD = bytes.fromhex("28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A")


def rc4(key, data):
    s = list(range(256))
    j = 0
    for i in range(256):
        j = (j + s[i] + key[i % len(key)]) % 256
        s[i], s[j] = s[j], s[i]
    out = bytearray()
    i = j = 0
    for b in data:
        i = (i + 1) % 256
        j = (j + s[i]) % 256
        s[i], s[j] = s[j], s[i]
        out.append(b ^ s[(s[i] + s[j]) % 256])
    return bytes(out)


def aes_cbc(key, iv, data):
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

    e = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    return e.update(data) + e.finalize()


def aes_ecb(key, data):
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

    e = Cipher(algorithms.AES(key), modes.ECB()).encryptor()
    return e.update(data) + e.finalize()


def pkcs7(data):
    n = 16 - len(data) % 16
    return data + bytes([n]) * n


def md5(*parts):
    import hashlib

    h = hashlib.md5()
    for p in parts:
        h.update(p)
    return h.digest()


def padded(pw):
    return (pw.encode("latin-1") + PAD)[:32]


def hash_2b(pw, salt, udata):
    """Algorithm 2.B (revision 6)."""
    import hashlib

    k = hashlib.sha256(pw + salt + udata).digest()
    i = 0
    while True:
        k1 = (pw + k + udata) * 64
        e = aes_cbc(k[:16], k[16:32], k1)
        h = (hashlib.sha256, hashlib.sha384, hashlib.sha512)[int.from_bytes(e[:16], "big") % 3]
        k = h(e).digest()
        i += 1
        if i >= 64 and e[-1] <= i - 32:
            return k[:32]


# Print (bit 3), modify (bit 4) and copy (bit 5) withheld; annotate (6) and
# fill forms (9) kept. Bits 7-8 and 13-32 are 1 (Table 22).
RESTRICTED = -32


class Security:
    """One file's standard security handler: `method` is rc4-40, rc4-128,
    aes-128 or aes-256."""

    def __init__(self, method, user, owner, file_id, p=RESTRICTED):
        self.method, self.user, self.owner, self.p = method, user, owner, p
        self.id0 = bytes.fromhex(file_id)
        self.aes = method.startswith("aes")
        if method == "aes-256":
            self._r6()
        else:
            self.r = {"rc4-40": 2, "rc4-128": 3, "aes-128": 4}[method]
            self.n = 5 if self.r == 2 else 16
            self.o = self._o()
            self.key = self._key(user)
            self.u = self._u()

    # Algorithm 3: /O.
    def _o(self):
        k = md5(padded(self.owner or self.user))
        if self.r >= 3:
            for _ in range(50):
                k = md5(k[: self.n])
        k = k[: self.n]
        o = rc4(k, padded(self.user))
        if self.r >= 3:
            for i in range(1, 20):
                o = rc4(bytes(b ^ i for b in k), o)
        return o

    # Algorithm 2: the file key.
    def _key(self, pw):
        k = md5(padded(pw), self.o, (self.p & 0xFFFFFFFF).to_bytes(4, "little"), self.id0)
        if self.r >= 3:
            for _ in range(50):
                k = md5(k[: self.n])
        return k[: self.n]

    # Algorithms 4 and 5: /U.
    def _u(self):
        if self.r == 2:
            return rc4(self.key, PAD)
        u = rc4(self.key, md5(PAD, self.id0))
        for i in range(1, 20):
            u = rc4(bytes(b ^ i for b in self.key), u)
        return u + bytes(16)

    # Algorithms 8, 9 and 10 (revision 6).
    def _r6(self):
        self.r = 6
        self.key = md5(b"2256 file key a") + md5(b"2256 file key b")
        user, owner = self.user.encode("utf-8"), (self.owner or self.user).encode("utf-8")
        uvs, uks = b"uvsalt01", b"ukssalt1"
        self.u = hash_2b(user, uvs, b"") + uvs + uks
        self.ue = aes_cbc(hash_2b(user, uks, b""), bytes(16), self.key)
        ovs, oks = b"ovsalt01", b"okssalt1"
        self.o = hash_2b(owner, ovs, self.u) + ovs + oks
        self.oe = aes_cbc(hash_2b(owner, oks, self.u), bytes(16), self.key)
        perms = (self.p & 0xFFFFFFFF).to_bytes(4, "little") + b"\xff\xff\xff\xff" + b"Tadb" + b"2256"
        self.perms = aes_ecb(self.key, perms)

    def dictionary(self):
        hx = lambda b: "<" + b.hex() + ">"
        head = f"<< /Filter /Standard /P {self.p} /O {hx(self.o)} /U {hx(self.u)}"
        if self.method == "rc4-40":
            return head + " /V 1 /R 2 >>"
        if self.method == "rc4-128":
            return head + " /V 2 /R 3 /Length 128 >>"
        if self.method == "aes-128":
            cf = "/CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV2 /Length 16 >> >>"
            return head + f" /V 4 /R 4 /Length 128 {cf} /StmF /StdCF /StrF /StdCF >>"
        cf = "/CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV3 /Length 32 >> >>"
        return (
            head + f" /OE {hx(self.oe)} /UE {hx(self.ue)} /Perms {hx(self.perms)}"
            f" /V 5 /R 6 /Length 256 {cf} /StmF /StdCF /StrF /StdCF >>"
        )

    # Algorithm 1 (and 1.A for revision 6): one string or stream.
    def encrypt(self, num, data, nth):
        if self.r == 6:
            key = self.key
        else:
            extra = b"sAlT" if self.aes else b""
            key = md5(self.key, num.to_bytes(3, "little"), b"\0\0", extra)[: min(self.n + 5, 16)]
        if not self.aes:
            return rc4(key, data)
        iv = md5(b"iv", num.to_bytes(4, "little"), nth.to_bytes(4, "little"))
        return iv + aes_cbc(key, iv, pkcs7(data))

    def encrypt_object(self, num, body):
        """`body` with each string and its stream's data encrypted: strings
        are written as hex, a stream's `/Length` follows its new data."""
        data = None
        if b"\nstream\n" in body:
            cut = body.index(b"\nstream\n")
            head, data = body[:cut], body[cut + 8 :]
            assert data.endswith(b"\nendstream"), num
            data = data[: -len(b"\nendstream")]
        else:
            head = body
        out, nth = bytearray(), 0
        i = 0
        while i < len(head):
            c = head[i : i + 1]
            if head[i : i + 2] in (b"<<", b">>"):
                out += head[i : i + 2]
                i += 2
            elif c == b"<":
                end = head.index(b">", i)
                raw = bytes.fromhex(head[i + 1 : end].decode())
                nth += 1
                out += b"<" + self.encrypt(num, raw, nth).hex().encode() + b">"
                i = end + 1
            elif c == b"(":
                raw, i = literal(head, i)
                nth += 1
                out += b"<" + self.encrypt(num, raw, nth).hex().encode() + b">"
            else:
                out += c
                i += 1
        if data is None:
            return bytes(out)
        data = self.encrypt(num, data, 0)
        head = re.sub(rb"/Length \d+", f"/Length {len(data)}".encode(), bytes(out), count=1)
        return head + b"\nstream\n" + data + b"\nendstream"


def literal(b, i):
    """The literal string starting at `b[i]` (a `(`): its bytes and the index past it."""
    assert b[i : i + 1] == b"("
    i += 1
    depth, out = 1, bytearray()
    esc = {b"n": b"\n", b"r": b"\r", b"t": b"\t", b"b": b"\b", b"f": b"\f"}
    while True:
        c = b[i : i + 1]
        if c == b"\\":
            n = b[i + 1 : i + 2]
            if n in esc:
                out += esc[n]
                i += 2
            elif n.isdigit():
                j = i + 1
                while j < i + 4 and b[j : j + 1].isdigit():
                    j += 1
                out.append(int(b[i + 1 : j], 8) & 0xFF)
                i = j
            else:
                out += n
                i += 2
            continue
        if c == b"(":
            depth += 1
        elif c == b")":
            depth -= 1
            if depth == 0:
                return bytes(out), i + 1
        out += c
        i += 1


# Every slice 3 page: large colour blocks (so a reference comparison has
# enough on it: a blank page fails it) and Helvetica lines the text tests read.
def blocks_page(pdf, pages_ref, font, lines, size=A4):
    ops = [
        "0.15 0.35 0.75 rg 72 560 220 140 re f",
        "0.85 0.25 0.2 rg 320 560 200 140 re f",
        "0.2 0.65 0.3 rg 72 380 448 120 re f",
        "0 G 3 w 72 330 m 520 330 l S",
        labels(lines, 14),
    ]
    return page(pdf, pages_ref, "\n".join(ops), font=font, size=size)


def one_page(lines):
    """A RawPdf with one blocks page: (pdf, catalog)."""
    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    font = pdf.add(HELV)
    p1 = blocks_page(pdf, pages, font, lines)
    pdf.set(pages, f"<< /Type /Pages /Kids [{p1} 0 R] /Count 1 >>")
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>")
    return pdf, catalog


# The made-up passwords of the encrypted files (corpus/pdf-view/README.md);
# the suite's tests hold the same table.
ENCRYPTED = {
    "enc-rc4-40": ("rc4-40", "user-rc4-40", "owner-rc4-40", "RC4 40"),
    "enc-rc4-128": ("rc4-128", "user-rc4-128", "owner-rc4-128", "RC4 128"),
    "enc-aes-128": ("aes-128", "user-aes-128", "owner-aes-128", "AES 128"),
    "enc-aes-256": ("aes-256", "user-aes-256", "owner-aes-256", "AES 256"),
    "enc-owner-only": ("aes-128", "", "owner-only", "AES 128, owner password only"),
}
ENC_ID = "2256" * 8


def make_encrypted(name):
    method, user, owner, label = ENCRYPTED[name]

    def make(path):
        pdf, catalog = one_page([(72, 760, f"Encrypted with {label}"), (72, 735, "Protected page text")])
        info = pdf.add("<< /Title (Encrypted corpus) /Author (docxy) >>")
        pdf.write(path, catalog, info, security=Security(method, user, owner, ENC_ID), file_id=ENC_ID)
        check_encrypted(path, name)

    return make


def run_ok(args):
    return subprocess.run(args, capture_output=True).returncode == 0


def text_by_poppler(path, *pw):
    r = subprocess.run(["pdftotext", *pw, path, "-"], capture_output=True)
    return r.stdout.decode("utf-8", "replace") if r.returncode == 0 else None


def check_encrypted(path, name):
    """poppler, an independent reader, opens the file with each password the
    tests use and refuses it without one (but the owner-only file)."""
    method, user, owner, label = ENCRYPTED[name]
    want = f"Encrypted with {label}"
    if user:
        assert text_by_poppler(path) is None, f"{name} opens without a password"
        assert text_by_poppler(path, "-upw", "wrong") is None, f"{name} opens with a wrong password"
        assert want in (text_by_poppler(path, "-upw", user) or ""), f"{name}: user password"
    else:
        assert want in (text_by_poppler(path) or ""), f"{name} needs a password"
    assert want in (text_by_poppler(path, "-opw", owner) or ""), f"{name}: owner password"


def make_incremental(path):
    """A file and one update appended to it (§7.5.6): the update rewrites
    page 1's content and the page tree, and adds a page. A reader of the
    first revision shows one page saying "First revision"."""
    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    font = pdf.add(HELV)
    p1 = blocks_page(pdf, pages, font, [(72, 760, "First revision")])
    pdf.set(pages, f"<< /Type /Pages /Kids [{p1} 0 R] /Count 1 >>")
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>")
    base, prev = pdf.build(catalog)
    first = len(pdf.objs)
    # The update: objects past the first revision's, and new versions of
    # the page tree and page 1.
    upd = RawPdf()
    upd.objs = [None] * first
    p2 = blocks_page(upd, pages, font, [(72, 760, "Added in the update")])
    content = upd.stream(
        "\n".join(
            [
                "0.15 0.35 0.75 rg 72 560 220 140 re f",
                "0.85 0.25 0.2 rg 320 560 200 140 re f",
                "0.6 0.2 0.7 rg 72 380 448 120 re f",
                labels([(72, 760, "Latest revision"), (72, 735, "Written by an incremental update")], 14),
            ]
        )
    )
    page1 = (
        f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 {A4[0]:g} {A4[1]:g}]"
        f" /Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R /Annots [] >>"
    )
    changed = {pages: f"<< /Type /Pages /Kids [{p1} 0 R {p2} 0 R] /Count 2 >>", p1: page1}
    out = bytearray(base)
    offsets = {}
    for n, body in changed.items():
        offsets[n] = len(out)
        out += f"{n} 0 obj\n{body}\nendobj\n".encode()
    for n in range(first + 1, len(upd.objs) + 1):
        offsets[n] = len(out)
        out += f"{n} 0 obj\n".encode() + upd.objs[n - 1] + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n"
    for n in sorted(offsets):
        out += f"{n} 1\n{offsets[n]:010d} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {len(upd.objs) + 1} /Root {catalog} 0 R /Prev {prev}"
        f" /ID [<{'2255' * 8}> <{'2256' * 8}>] >>\nstartxref\n{xref}\n%%EOF\n"
    ).encode()
    with open(path, "wb") as f:
        f.write(out)
    t = text_by_poppler(path) or ""
    assert "Latest revision" in t and "Added in the update" in t and "First revision" not in t, t


def make_objstm(path):
    """PDF 1.5 (§7.5.7, §7.5.8): the catalog, page tree, page and font in
    one Flate-compressed object stream, found through a cross-reference
    stream; only the page's content is a plain object."""
    import zlib

    pdf, catalog = one_page([(72, 760, "Object and cross-reference streams"), (72, 735, "PDF 1.5")])
    # objects: 1 catalog, 2 pages, 3 font, 4 content, 5 page.
    assert [b.startswith(b"<< /Type /Page ") for b in pdf.objs] == [False, False, False, False, True]
    members = [1, 2, 3, 5]
    head, body = [], bytearray()
    for n in members:
        head.append(f"{n} {len(body)}")
        body += pdf.objs[n - 1] + b"\n"
    head = (" ".join(head) + "\n").encode()
    packed = zlib.compress(head + bytes(body), 9)
    objstm = 6
    out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    offsets[4] = len(out)
    out += b"4 0 obj\n" + pdf.objs[3] + b"\nendobj\n"
    offsets[objstm] = len(out)
    out += (
        f"{objstm} 0 obj\n<< /Type /ObjStm /N {len(members)} /First {len(head)}"
        f" /Filter /FlateDecode /Length {len(packed)} >>\nstream\n"
    ).encode() + packed + b"\nendstream\nendobj\n"
    xref_num = 7
    offsets[xref_num] = len(out)
    rows = bytearray()
    for n in range(xref_num + 1):
        if n == 0:
            rows += bytes([0]) + (0).to_bytes(4, "big") + (65535).to_bytes(2, "big")
        elif n in members:
            rows += bytes([2]) + objstm.to_bytes(4, "big") + members.index(n).to_bytes(2, "big")
        else:
            rows += bytes([1]) + offsets[n].to_bytes(4, "big") + (0).to_bytes(2, "big")
    rows = zlib.compress(bytes(rows), 9)
    out += (
        f"{xref_num} 0 obj\n<< /Type /XRef /Size {xref_num + 1} /W [1 4 2] /Root 1 0 R"
        f" /ID [<{'2256' * 8}> <{'2256' * 8}>] /Filter /FlateDecode /Length {len(rows)} >>\nstream\n"
    ).encode() + rows + b"\nendstream\nendobj\n"
    out += f"startxref\n{offsets[xref_num]}\n%%EOF\n".encode()
    with open(path, "wb") as f:
        f.write(out)
    assert "Object and cross-reference streams" in (text_by_poppler(path) or "")


def damaged(lines):
    """A plain one-page file before it is damaged: its bytes (the
    reference is drawn from them, as poppler cannot rebuild a file without
    a trailer) and its xref's offset."""
    pdf, catalog = one_page(lines)
    info = pdf.add("<< /Title (Damaged corpus) /Producer (make-pdf-view-corpus.py) >>")
    out, xref = pdf.build(catalog, info)
    return out, xref


def make_damaged_offsets(path):
    """Every xref offset 7 bytes past its object. PdfCraft finds the objects
    near their offsets without rebuilding the table; poppler rebuilds it."""
    out, xref = damaged([(72, 760, "Damaged: every offset is wrong")])
    table = out[xref:]
    fixed = re.sub(rb"(\d{10}) 00000 n", lambda m: b"%010d 00000 n" % (int(m.group(1)) + 7), table)
    with open(path, "wb") as f:
        f.write(out[:xref] + fixed)
    return out


def make_damaged_noxref(path):
    """No xref table, trailer or startxref: the objects only."""
    out, xref = damaged([(72, 760, "Damaged: no cross-reference table")])
    with open(path, "wb") as f:
        f.write(out[:xref])
    return out


def make_damaged_truncated(path):
    """Cut in the middle of its last object (the /Info dictionary), so no
    xref or trailer: the page's objects are all whole."""
    out, xref = damaged([(72, 760, "Damaged: the file is cut short")])
    last = out.rindex(b" 0 obj\n", 0, xref)
    with open(path, "wb") as f:
        f.write(out[: last + 20])
    return out


def make_damaged_unreadable(path):
    """A PDF header and nothing a reader can use: no object, no xref."""
    import hashlib

    junk = bytearray()
    seed = b"2256"
    while len(junk) < 4096:
        seed = hashlib.sha256(seed).digest()
        junk += seed
    junk = bytes(junk).replace(b"obj", b"ojb").replace(b"trailer", b"trailor")
    with open(path, "wb") as f:
        f.write(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n" + junk + b"\n%%EOF\n")


SLICE3 = {
    **{name: make_encrypted(name) for name in ENCRYPTED},
    "incremental": make_incremental,
    "objstm": make_objstm,
    "damaged-offsets": make_damaged_offsets,
    "damaged-noxref": make_damaged_noxref,
    "damaged-truncated": make_damaged_truncated,
    "damaged-unreadable": make_damaged_unreadable,
}


# ---- slice 4 (#2257): a document for Convert to editable document ----
#
# Three Letter pages in the base-14 fonts (not embedded), each with the same
# running header and a "Page N of 3" footer: page 1 a title, two paragraphs
# (one red word) around a picture and a ruled 3 x 3 table; page 2 a heading
# across the page over two columns; page 3 a paragraph and Courier lines.
# Checked by what it converts to (pdf_view/convert/tests.rs), not by pixels.

CONVERT_FONTS = {"F1": "Times-Roman", "F2": "Helvetica", "F3": "Helvetica-Bold", "F4": "Courier"}

CONVERT_LEFT = [
    "The north meadow held the most birds this",
    "year, most of them early in the morning",
    "when the grass was still wet with dew and",
    "the light came low across the field from",
    "the east. Swallows came first, then larks,",
    "and by noon the hedges were full of finches.",
    "We counted from the gate every hour and",
    "wrote down each kind we saw or heard. The",
    "counts rose through May and fell slowly",
    "after the hay was cut in the second week",
    "of June, when the meadow lay open and the",
    "birds moved on to the hedges and the wood.",
]

CONVERT_RIGHT = [
    "The south marsh was quieter but held the",
    "rarer kinds: two bitterns, a water rail",
    "and, for one week only, a pair of spoonbills",
    "that fed in the shallows by the old sluice.",
    "Water stood high until the end of April,",
    "then dropped a hand's width each week. The",
    "reed beds grew thick along the eastern bank",
    "and the warblers sang there from dawn. We",
    "kept to the boardwalk, counting from the",
    "hide at each hour as on the meadow, and",
    "noted the water level on the post by the",
    "sluice gate before we left every evening.",
]


def convert_text(font, size, x, y, s, rgb=None):
    colour = f"{rgb[0]:g} {rgb[1]:g} {rgb[2]:g} rg" if rgb else "0 g"
    return f"BT {colour} /{font} {size:g} Tf 1 0 0 1 {x:g} {y:g} Tm {pdf_text(s)} Tj ET"


def convert_running(n):
    """The header and footer every page has, centred (Helvetica's widths are
    estimated; every page uses the same, so they line up)."""
    header, footer = "Annual Field Notes", f"Page {n} of 3"
    return [
        convert_text("F2", 10, 306 - len(header) * 2.6, 756, header),
        convert_text("F2", 10, 306 - len(footer) * 2.6, 36, footer),
    ]


def make_convert(path):
    import zlib

    pdf = RawPdf()
    catalog, pages = pdf.reserve(), pdf.reserve()
    fonts = {
        name: pdf.add(f"<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>")
        for name, base in CONVERT_FONTS.items()
    }
    font_res = " ".join(f"/{name} {ref} 0 R" for name, ref in fonts.items())
    w, h = 60, 40
    pixels = bytearray()
    for yy in range(h):
        for xx in range(w):
            pixels += bytes([40 + xx * 3, 90 + yy * 3, 200 - xx * 2])
    packed = zlib.compress(bytes(pixels), 9)
    picture = pdf.stream(
        packed,
        f"/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB"
        " /BitsPerComponent 8 /Filter /FlateDecode",
    )

    def convert_page(ops, xobjects=""):
        c = pdf.stream("\n".join(ops))
        xo = f" /XObject << {xobjects} >>" if xobjects else ""
        return pdf.add(
            f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 {LETTER[0]:g} {LETTER[1]:g}]"
            f" /Resources << /Font << {font_res} >>{xo} >> /Contents {c} 0 R >>"
        )

    # Page 1: title, paragraph, picture, paragraph, ruled table.
    ops = convert_running(1)
    ops.append(convert_text("F3", 20, 72, 690, "Field Notes for the Year"))
    ops += [
        convert_text("F1", 11, 72, 655, "The survey began in the first week of spring and ran"),
        "BT 0 g /F1 11 Tf 1 0 0 1 72 641 Tm (until the autumn rains, when the ) Tj"
        " 1 0 0 rg (heron) Tj 0 g ( returned to the lake.) Tj ET",
        convert_text("F1", 11, 72, 627, "Each visit followed the same path along the water."),
    ]
    ops.append("q 180 0 0 120 72 480 cm /Im1 Do Q")
    ops += [
        convert_text("F1", 11, 72, 455, "The table below gives the counts for the three sites"),
        convert_text("F1", 11, 72, 441, "we visited most often over the whole of the season."),
    ]
    # A 3 x 3 table: its grid stroked, a cell's text inset in it.
    xs, ys = [72, 222, 372, 522], [410, 380, 350, 320]
    grid = ["0 G 0.75 w"]
    grid += [f"{x} {ys[0]} m {x} {ys[-1]} l S" for x in xs]
    grid += [f"{xs[0]} {y} m {xs[-1]} {y} l S" for y in ys]
    ops += grid
    cells = [["Site", "Visits", "Kinds"], ["Meadow", "42", "31"], ["Marsh", "38", "27"]]
    for r, row in enumerate(cells):
        for c, text in enumerate(row):
            ops.append(convert_text("F2", 10, xs[c] + 8, ys[r] - 19, text))
    p1 = convert_page(ops, f"/Im1 {picture} 0 R")

    # Page 2: a heading across the page, then two columns.
    ops = convert_running(2)
    ops.append(convert_text("F3", 16, 72, 690, "Observations by Site across the Whole Valley"))
    for i, (a, b) in enumerate(zip(CONVERT_LEFT, CONVERT_RIGHT)):
        y = 655 - i * 13
        ops.append(convert_text("F1", 10, 72, y, a))
        ops.append(convert_text("F1", 10, 324, y, b))
    p2 = convert_page(ops)

    # Page 3: a paragraph and three Courier lines.
    ops = convert_running(3)
    ops += [
        convert_text("F1", 11, 72, 690, "The counts were kept in a plain text file, one line for"),
        convert_text("F1", 11, 72, 676, "each hour, as the three lines below show for one day."),
        convert_text("F4", 10, 72, 640, "06:00 meadow larks=4 swallows=9"),
        convert_text("F4", 10, 72, 626, "07:00 meadow larks=6 swallows=12"),
        convert_text("F4", 10, 72, 612, "08:00 marsh bitterns=1 rails=1"),
    ]
    p3 = convert_page(ops)

    pdf.set(pages, f"<< /Type /Pages /Kids [{p1} 0 R {p2} 0 R {p3} 0 R] /Count 3 >>")
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>")
    info = pdf.add("<< /Title (Annual Field Notes) /Producer (make-pdf-view-corpus.py) >>")
    pdf.write(path, catalog, info, file_id="2257" * 8)
    text = text_by_poppler(path) or ""
    for want in ("Annual Field Notes", "Page 3 of 3", "heron", "Meadow", "spoonbills", "rails=1"):
        assert want in text, want


SLICE4 = {"convert": make_convert}


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


def references(pdf, name, password=None):
    """pdftoppm -r 72: <name>-<page>.png, the page number unpadded."""
    tmp = os.path.join(REF, f".{name}")
    pw = ["-upw", password] if password else []
    subprocess.run(["pdftoppm", "-r", "72", "-png", *pw, pdf, tmp], check=True)
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
    unknown = only - {"text", "columns", "scanned", "slides", "vector", "rotated", "source", *SLICE2, *SLICE3, *SLICE4}
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
    # A damaged file's maker hands back the file as it was before the
    # damage: its reference is drawn from that.
    intacts = {}
    for name, make in {**SLICE2, **SLICE3, **SLICE4}.items():
        if want(name):
            intact = make(os.path.join(OUT, f"{name}.pdf"))
            if name in SLICE3 and intact:
                intacts[name] = intact
    # References for the files drawn against poppler; the slice 2 files
    # other than annots.pdf are checked by what they hold, not by pixels.
    for name in [*made, "vector", "rotated", "annots"]:
        if want(name):
            references(os.path.join(OUT, f"{name}.pdf"), name)
    if want("source"):
        references(SOURCE_PDF, "source")
    # Slice 3 (#2256): every file a reader opens, the encrypted ones drawn
    # with their user password; damaged-unreadable has nothing to draw.
    for name in SLICE3:
        if want(name) and name != "damaged-unreadable":
            password = ENCRYPTED[name][1] if name in ENCRYPTED else None
            pdf = os.path.join(OUT, f"{name}.pdf")
            intact = intacts.get(name)
            if intact:
                pdf = os.path.join(REF, f".{name}.intact.pdf")
                with open(pdf, "wb") as f:
                    f.write(intact)
            references(pdf, name, password)
            if intact:
                os.remove(pdf)
    # The UI cases open only the harness's own files (uiharness/tests/
    # cases.rs): byte-identical copies of the ones they use.
    fixtures = os.path.join(ROOT, "uiharness", "fixtures")
    for name in ("slides", "text", "forms", "links", "annots", "signed", "enc-aes-256", "enc-owner-only"):
        if want(name):
            shutil.copyfile(
                os.path.join(OUT, f"{name}.pdf"),
                os.path.join(fixtures, f"pdf-view-{name}.pdf"),
            )


if __name__ == "__main__":
    main()
