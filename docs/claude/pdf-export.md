# PDF export

Notes moved from `CLAUDE.md` (#1903). Read them before changing the PDF export (`docxcore` `to_pdf`, `pdffont`). This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## PDF export fonts (#1508)

- `PdfOptions::fonts` (a `docxcore::pdf_font::PdfFontProvider`) and `bidi` (the
  `BidiProjector` trait again) switch `to_pdf` from the Courier/WinAnsi file to
  embedded fonts; `docxcore` stays dependency-free: it only subsets the sfnt
  bytes a `PdfFace` hands it (`pdf_font::subset_sfnt`, retain-GID, `glyf`
  only), writes the Type0/CIDFontType2/`/W`/ToUnicode objects
  (`export/cid.rs`) and Flate-compresses with its own `deflate`. Parsing fonts
  and finding them is `pdffont` (ttf-parser, system directories, Office
  aliases, script fallbacks). A face is indexed under both its legacy (ID 1)
  and typographic (ID 16) family; Macintosh Roman names follow the Windows
  ones, so an English name kept only on the Mac platform still matches. Italic
  is macStyle, OS/2 fsSelection, `post` angle or the style name. A request
  takes, in order: the slant, the width nearest normal, the weight nearest
  400 (700 when bold), the face whose legacy name is the request, then the
  first path. CFF faces, faces the subsetter cannot read and faces whose OS/2
  `fsType` forbids embedding or subsetting are not offered.
- Layout measures through `export::Metrics`: Courier is 0.6 em per cell
  (unchanged, pinned by `docxcore/tests/pdf_legacy_bytes.rs`), a provider gives
  each cell its face's advance. Per character: the run's family face, a
  fallback face, Courier when WinAnsi covers it, else `.notdef` of the run's
  face, or a Courier `?` when the run has no usable face (counted in
  `to_pdf_report`'s `PdfReport::missing_chars`; terminal `docxy` says so on
  the status line). `cid::Checked` wraps the provider for one export: a face
  whose font file does not subset counts as absent for layout, writer and
  report alike. If no embedded face showed anything, the file is the plain
  Courier one.
- Bidi: a line wraps in logical order, then `place_line_fonts` puts its cells
  in the projector's visual order. A right-to-left stretch is one show-text
  with `TJ` adjustments in logical order, so extraction reads logically and
  the glyphs sit visually. Known limits: Arabic is not joined (isolated
  forms), brackets are not mirrored in right-to-left text.
- Terminal `docxy` embeds the machine's fonts (`$DOCXY_PDF_FONT_DIR`, a path
  list, replaces them, indexed once per value; empty means none, so the plain
  Courier file). The suite has no PDF
  export yet and `docxwasm` passes `None`. Unit tests set
  `TEST_PDF_FONTS`; the machine's fonts never reach them.
- Tests build their TrueType in code (`docxcore/tests/common/synth_font.rs`)
  and read PDFs back with `common/pdf_read.rs` (xref/length/reference
  validation and ToUnicode extraction), so nothing depends on `qpdf` (used too
  when installed) or on fonts being installed.

## PDF export: borders, picture watermarks, unequal columns (#797)

- `StyleSheet::{effective_run, effective_align, paragraph_rtl_override,
  effective_tabs, effective_borders}` fall back to the default paragraph style
  (`w:default="1"`) for a paragraph without a `pStyle`, as `effective_vanish`
  and `effective_ppr_flag` did; so a Normal that sets alignment, colour or
  bold now reaches the terminal render, the RTF writer (rich copy, Save As)
  and `docxwasm`'s richdoc. The PDF export gets only the run properties and
  the paragraph direction from them: `emit_paragraph` reads alignment from
  `p.props.align`, not through the styles (unchanged here). `effective_align`
  takes the paragraph's `ParProps`, so a direct `w:jc="left"` (kept in
  `raw_props`) still beats a centred style. An editor-chosen Align Left is
  explicit too (#1752): `ParProps::set_align_direct` (`Editor::set_align`,
  `agent::set_style_range`, `Editor::set_cell_alignment`, wordcomshim's
  Alignment put) keeps a raw `w:jc` saying `left`/`start`, else
  puts `<w:jc w:val="left"/>` in place of any other; other alignments drop
  it. A raw `w:jc` supersedes the loaded (shadow) one on save while the
  paragraph is left, so a loaded `thaiDistribute` or `start` is never written
  beside it, and a raw `w:jc` left behind by a direct `align` write is
  superseded itself. `shown_run` (the suite) still
  skips Normal.
- Page borders: `BorderStyle::{Wave, ThreeD, Motif}`. `border_rules` draws
  `wave`/`doubleWave` as a sine polyline (amplitude one width, wavelength four),
  the 3-D styles as a light and a dark half stroke (raised: light inside on the
  top and left), and the art names `basicBlack/WhiteDots`,
  `basicBlack/WhiteSquares`, `stars`, `hearts`, `circlesLines`, `triangles` as
  tiled vector motifs of side `sz` points. Word's artwork is bitmap, so the
  motifs are approximations (no oracle); every other art name stays a dashed
  band. Tables still stroke all of these as one solid line (`strokes`).
- Picture watermarks: `package::picture_watermarks` reads a header's VML
  `v:imagedata r:id` or DrawingML `a:blip r:embed` (size from the style or
  `wp:extent`); `PdfOptions::from_package` resolves it through the header's
  rels into `PdfOptions::images` (`export/pic.rs`, `PdfImage`) and
  `picture_watermarks`. Nothing is decoded: JPEG (1 or 3 components) goes in
  as `DCTDecode`, a non-interlaced 8-bit grey/RGB or 1-8-bit palette PNG as its
  concatenated `IDAT` under `FlateDecode` with `/Predictor 15` (`tRNS` ignored).
  Anything else (alpha, 16-bit, interlaced, GIF/BMP/WMF/EMF) draws nothing. The
  picture is centred on the margin box, washed out with `/ca` (the shape's
  opacity, else 0.5), and its page resources appear only on pages that draw one,
  so other files are byte-identical.
- Unequal-width columns balance too (`Pager::balance_unequal`): lines wrap per
  column, so the set's paragraphs are laid out again from a `RegionSnap` taken
  before its first line, at a bisected trial height (`Trial`; running out of
  columns fails it). The unlimited layout must reproduce the lines already
  placed, and the result must be shorter, or the set is left as it was; a set
  that begins mid-paragraph (spilled from the page before) is not balanced.
