# PDF view corpus (#2245)

PDFs the suite's PDF view is checked against, and their reference renderings.
`scripts/make-pdf-view-corpus.py` makes all of them; the tests read what is
committed and never run the tools.

| File | Pages | What it tests |
|---|---|---|
| `text.pdf` | 2 (A4) | Running text in embedded DejaVu subsets with ToUnicode: Latin, Cyrillic (`Привет, мир! …`) and Greek (`Καλημέρα κόσμε …`); text continuing on page 2 (a selection across pages, a search hit on each page); the document title and author. |
| `columns.pdf` | 1 (Letter) | Two columns of 9 pt text: the left column is read whole before the right one. |
| `vector.pdf` | 1 (A4) | Paths, fills, strokes, a dashed curve, a clipped circle, line joins and caps, in DeviceCMYK, DeviceGray and DeviceRGB; embedded Helvetica. |
| `scanned.pdf` | 1 (A4) | A "scanned" page: a DCT (JPEG) DeviceGray picture and a Flate DeviceRGB picture, and a caption. |
| `rotated.pdf` | 1 (A4, `/Rotate 90`) | A page stored upright and shown turned a quarter right: the view shows it landscape (842 x 595). |
| `slides.pdf` | 3 (960 x 540) | Landscape slides with nested bookmarks (Review › Quarterly review, Revenue by region; Next steps). |
| `../word-import/source.pdf` | 1 | The Word import fixture: Word's own PDF of `source.docx` (Cyrillic and `€`). |

`ref/<name>-<page>.png` is `pdftoppm -r 72 -png` of each page: one pixel per
point, what the test draws at scale 1. The comparison and its tolerance are
in `suite/docxy/src/pdf_view/tests.rs` (`differing`).

## How it was made

- pycairo 1.27.0 (cairo with DejaVu fonts) writes `text`, `columns`,
  `scanned` and `slides`: Ghostscript's base fonts have no Cyrillic or Greek
  a PostScript program can show simply, and cairo also writes outlines and
  embeds a JPEG as it is.
- Ghostscript 10.06.0 (`pdfwrite`, `-dPDFSETTINGS=/prepress
  -dEmbedAllFonts=true -dAutoRotatePages=/None`) writes `vector` and
  `rotated`: the CMYK and grey operators and the page's `/Rotate`, which
  cairo cannot write. The base-14 fonts are embedded, so the references do
  not depend on the substitute fonts of the machine that made them.
- Pillow 12.1.1 makes the scanned page's JPEG.
- pdftoppm 26.01.0 (poppler) makes the references.

## Not here yet

The issue's corpus also names forms, encrypted, damaged and
PowerPoint-exported files; they come with the later slices of #2245
(forms and annotations, hard inputs). `slides.pdf` is made by cairo, not
exported from PowerPoint.
