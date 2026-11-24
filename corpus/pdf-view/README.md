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
| `links.pdf` | 3 (A4, page 3 `/Rotate 90`) | Links (#2255): URI and mailto, an explicit `/XYZ` destination with a zoom, a named destination in the `/Names` tree (`/FitH`), a GoTo naming a `/Dests` entry (`/FitR`), a GoTo with `/Fit`, a script, a launch, a `javascript:` URI, a link active over its `/QuadPoints` only, and a link on the turned page. |
| `annots.pdf` | 1 (A4) | Every annotation kind slice 2 draws, each with its appearance stream: highlight, underline, strike-out, squiggly, note, free text, ink, stamp, square, circle, line, polygon, polyline; three with text, author and date for pop-ups; a Hidden square that is never drawn. |
| `annots-noap.pdf` | 1 (A4) | The same annotations without `/AP`, on an otherwise blank page: drawn only by PdfCraft's appearances (but a highlight, which hayro draws by itself). |
| `forms.pdf` | 2 (A4) | An AcroForm: text fields (MaxLen with a tooltip, required, multi-line, password, comb, read-only with a value), a check box and a radio group with on/off appearances, combo boxes (plain and editable), list boxes (single and multi-select, export values apart from the texts), and a field on page 2. |
| `signed.pdf` | 1 (A4) | A signature field whose `/V` is a signature dictionary (placeholders, no real signature) and a text field. |

`uiharness/fixtures/pdf-view-slides.pdf`, `pdf-view-text.pdf`,
`pdf-view-forms.pdf`, `pdf-view-links.pdf`, `pdf-view-annots.pdf` and
`pdf-view-signed.pdf` are byte-identical copies of those files (the
generator writes them too): the UI cases may open only the harness's own
files.

`ref/<name>-<page>.png` is `pdftoppm -r 72 -png` of each page: one pixel per
point, what the test draws at scale 1 (of the slice 2 files, `annots` only;
the others are checked by what they hold). The comparison and its tolerance are
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
- The slice 2 files are written object by object by the generator's
  `RawPdf` (#2255): neither cairo nor pdfwrite writes link destinations,
  appearance streams or AcroForm fields as the tests need them. Their text
  is non-embedded Helvetica; only `annots.pdf` (no text) has a reference.

## Not here yet

The issue's corpus also names encrypted, damaged and PowerPoint-exported
files; they come with the later slices of #2245 (hard inputs). The
damaged-file save test makes its broken copy of `forms.pdf` at test time. `slides.pdf` is made by cairo, not
exported from PowerPoint.
