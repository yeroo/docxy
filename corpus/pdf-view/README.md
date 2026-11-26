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
| `enc-rc4-40.pdf` | 1 (A4) | Encrypted (#2256), standard handler V1 R2, RC4 40-bit. User password `user-rc4-40`, owner `owner-rc4-40`. |
| `enc-rc4-128.pdf` | 1 (A4) | V2 R3, RC4 128-bit. `user-rc4-128`, `owner-rc4-128`. |
| `enc-aes-128.pdf` | 1 (A4) | V4 R4, AES-128 (`/AESV2` crypt filters). `user-aes-128`, `owner-aes-128`. |
| `enc-aes-256.pdf` | 1 (A4) | V5 R6, AES-256 (`/AESV3`). `user-aes-256`, `owner-aes-256`. |
| `enc-owner-only.pdf` | 1 (A4) | V4 R4, AES-128 with an empty user password (opens with no prompt) and owner password `owner-only`. |
| `incremental.pdf` | 2 (A4) | A file whose page says "First revision", and one update appended to it (§7.5.6): page 1 rewritten ("Latest revision"), the page tree rewritten, a page added ("Added in the update"). |
| `objstm.pdf` | 1 (A4) | PDF 1.5: the catalog, page tree, page and font in one Flate-compressed object stream, found through a cross-reference stream (`/W [1 4 2]`). |
| `damaged-offsets.pdf` | 1 (A4) | Every xref entry's offset 7 bytes past its object. |
| `damaged-noxref.pdf` | 1 (A4) | The objects only: no xref table, trailer or `startxref`. |
| `damaged-truncated.pdf` | 1 (A4) | Cut in the middle of its last object (the `/Info` dictionary): no xref or trailer, every page object whole. |
| `damaged-unreadable.pdf` | - | A `%PDF-` header and 4 KB of bytes no reader can use (no object, no xref): it says why it cannot be opened. |
| `convert.pdf` | 3 (Letter) | Convert to editable document (#2257), written by hand in the base-14 fonts (not embedded): every page the same running header ("Annual Field Notes") and a "Page N of 3" footer; page 1 a title, two paragraphs (one red word) around a Flate RGB picture, and a ruled 3 x 3 table; page 2 a heading across the page over two columns of lines; page 3 a paragraph and three Courier lines. |

The passwords are made up for the corpus. Every encrypted file restricts
printing, copying and changes (`/P -32`) to its user password; its owner
password lifts them.

`uiharness/fixtures/pdf-view-slides.pdf`, `pdf-view-text.pdf`,
`pdf-view-forms.pdf`, `pdf-view-links.pdf`, `pdf-view-annots.pdf`,
`pdf-view-signed.pdf`, `pdf-view-enc-aes-256.pdf` and
`pdf-view-enc-owner-only.pdf` are byte-identical copies of those files (the
generator writes them too): the UI cases may open only the harness's own
files.

`ref/<name>-<page>.png` is `pdftoppm -r 72 -png` of each page: one pixel per
point, what the test draws at scale 1 (of the slice 2 files, `annots` only;
the others are checked by what they hold; every slice 3 file but
`damaged-unreadable`; not `convert`, checked by what it converts to). The comparison and its tolerance are
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
- The slice 3 files (#2256) are written by `RawPdf` too, with large
  colour blocks so their references mean something (a blank page fails
  them) and short Helvetica lines the text tests read. The standard
  security handler is the generator's own (ISO 32000-2 §7.6.4, Algorithms
  1-10 and 2.B; Python's `hashlib`, cryptography 46.0.5's AES): poppler
  opens each encrypted file with its passwords and refuses it without
  (`pdftotext`), checked as the corpus is made, and draws its reference
  (`pdftoppm -upw`). The damaged files' references are drawn from the file
  before its damage: poppler cannot rebuild a file with no trailer.
  `python3 scripts/make-pdf-view-corpus.py enc-rc4-40 enc-rc4-128
  enc-aes-128 enc-aes-256 enc-owner-only incremental objstm
  damaged-offsets damaged-noxref damaged-truncated damaged-unreadable`
  writes only them, byte for byte the same.
- `convert.pdf` (#2257) is written by `RawPdf` too: its columns are lines
  side by side at the same baselines (the layout PdfCraft's export takes
  for a table), and its picture a zlib-compressed RGB image. poppler reads
  its text back as it is made (`pdftotext`). `python3
  scripts/make-pdf-view-corpus.py convert` writes only it, byte for byte
  the same.

## Not here yet

The issue's corpus also names PowerPoint-exported files: `slides.pdf` is
made by cairo, not exported from PowerPoint. The damaged-file save test
makes its broken copy of `forms.pdf` at test time, and the encrypted-form
tests encrypt `forms.pdf` at test time with PdfCraft's own writer.
