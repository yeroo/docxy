# docxy — Architecture & Build Plan

A terminal editor for Microsoft Word `.docx` documents, written in Rust.

> **Name / crate / binary:** `docxy` (so `cargo install docxy` installs
> the `docxy` command).

docxy deliberately does **not** reproduce Word's visual fidelity. It shows
*structure and emphasis* — bold/italic/underline, colors (quantized to the
terminal theme), links, tables, lists, paragraph/character styles — and offers a
clean, distraction-free way to read and edit documents in a black-on-terminal
world. Page geometry is optional pseudographics, not a pixel-accurate page.

---

## 1. Goals / non-goals

**Goals**
- Open, view, edit, create, and save `.docx` without corrupting them.
- Render meaningful formatting in the terminal: b/i/u/strike, reduced color,
  hyperlinks, tables (with an optional borderless mode), lists, headings.
- Toggle **page view** (pseudographic page borders/margins) and **show
  invisibles** (¶, ·, →, ↵).
- Pick and apply paragraph/character **styles** from the document's style sheet.
- **Print / export to PDF** — a faithful, paginated PDF with real fonts and
  sizes, designed in from the start (see §7b). This is the one output that *does*
  honor font families/sizes and page geometry.
- Stay responsive on large documents.

**Non-goals (at least initially)**
- Reproducing Word's exact *on-screen* fonts/sizes **in the terminal** — the TUI
  is a reduced, theme-quantized view by design. (Fidelity lives in the PDF
  output, §7b, not the terminal.)
- Tracked changes, comments, footnotes/endnotes editing, fields recalculation,
  embedded OLE objects (these are *preserved* on save, just not *edited*).
- Mail-merge, macros.

---

## 2. Technology choices

- **Rust** (edition 2024), same toolchain as rustchm/rust365.
- **TUI:** `ratatui` (widgets/layout) over `crossterm` (cross-platform raw mode,
  input, styling). This is the one deliberate break from the std-only ethos of
  rustchm/rust365 — justified because the zero-dep stance was about *binary
  distribution trust* (Defender), which doesn't apply to an interactive tool the
  user runs themselves. Cross-platform Windows console handling from scratch is
  not where the value is.
- **DOCX I/O:** ported from `rust365` (see §4), extended with a writer.
- **PDF export:** a from-scratch writer in `docxcore` (std-only) using the
  standard-14 base fonts — no new runtime dependency, fully unit-testable (§7b).
- **Images:** `ratatui-image` (+ the `image` crate for decoding) to render inline
  pictures using whatever graphics protocol the host terminal supports —
  Kitty, iTerm2, or Sixel — with a Unicode half-block fallback and a text
  placeholder as the floor (see §7a). This is a *capability*, gated behind
  detection; it never breaks terminals that lack graphics.
- No other runtime dependencies if avoidable. Keep the tree small and auditable.

---

## 3. Reuse from rust365

`rust365` already implements, from scratch, the entire **read** path:

| rust365 module | Role in docxy |
| --- | --- |
| `zip.rs` | ZIP central-directory reader → unchanged |
| `inflate.rs` | DEFLATE decompressor → unchanged |
| `xml.rs` | XML tokenizer/parser → reused, plus a writer is added |
| `docx.rs` / `docx_run.rs` | WordprocessingML interpretation → becomes the basis of the editable model |
| `htmlutil.rs` | HTML escaping → not needed (terminal render replaces it) |

**Plan:** extract the reusable I/O + model code into a **library crate**
(`docxcore`) that both `rust365` and `docxy` depend on, rather than
copy-pasting. Short-term we can vendor the modules into docxy and refactor into
a shared crate once the model stabilizes.

What is **new** (not in rust365, which is read-only/one-way):
- A **ZIP writer** (STORED method — see §5) and an **XML serializer**.
- An **editable** document model (rust365 streams straight to HTML; we need a
  mutable tree with cursor addressing).
- The terminal **render** and **edit** engines and the **TUI**.

---

## 4. Crate / module layout

```
docxy/                      # the binary crate
  Cargo.toml                     # [[bin]] name = "docxy"
  src/
    main.rs                      # arg parsing, terminal setup/teardown, run loop
    app.rs                       # App state: open doc, mode, toggles, status
    event.rs                     # key/resize event → editor commands
    keymap.rs                    # keybinding table (nano/micro-style, modeless)
    ui/
      mod.rs                     # ratatui frame composition
      document_view.rs           # scrollable rendered document widget
      style_picker.rs            # styles list popup
      status_bar.rs              # mode, filename, dirty flag, position
      command_line.rs            # :open / :save / prompts
    render/
      mod.rs                     # model → Vec<RenderedLine> (styled cells)
      inline.rs                  # runs → spans (SGR), color quantization, OSC8 links
      block.rs                   # paragraphs, headings, lists
      table.rs                   # box-drawing table layout + borderless mode
      page.rs                    # page-border / margin pseudographics
      invisibles.rs              # ¶ · → ↵ rendering
      image.rs                   # ratatui-image bridge (§7a)
    editor/
      mod.rs                     # cursor, selection, dirty tracking
      cursor.rs                  # position model (block, run, char offset)
      ops.rs                     # edit operations (insert, delete, split, apply style…)
      history.rs                 # undo/redo command stack
  docxcore/ (library, shared with rust365 eventually)
    src/
      zip_read.rs                # from rust365
      inflate.rs                 # from rust365
      zip_write.rs               # NEW — STORED entries
      xml_read.rs                # from rust365
      xml_write.rs               # NEW — serializer
      model.rs                   # editable OOXML AST (§6)
      load.rs                    # .docx → (Document, PreservedParts)
      save.rs                    # (Document, PreservedParts) → .docx
      styles.rs                  # styles.xml parse/apply
      numbering.rs               # numbering.xml (lists)
      template.rs                # minimal blank-doc parts for "create new"
      export/
        pdf_layout.rs            # model → laid-out pages (line breaking, AFM metrics)
        pdf_write.rs             # laid-out pages → PDF bytes (§7b)
        afm.rs                   # standard-14 font glyph-width tables
```

---

## 5. Document model (editable OOXML AST)

A mutable tree that is rich enough to render and edit, while everything we don't
model is preserved verbatim (see §6).

```
Document
  body: Vec<Block>
  styles: StyleSheet        # from styles.xml
  numbering: Numbering      # from numbering.xml
  rels: Relationships       # hyperlink/image targets
  sect_pr: SectionProps     # page size/margins (for page view)

Block = Paragraph | Table

Paragraph
  style: Option<StyleId>    # pStyle
  props: ParProps           # alignment, indent, spacing, list ref (numId/ilvl)
  runs: Vec<Inline>

Inline = Run | Hyperlink | Break | Tab | FieldPlaceholder
Run
  text: String
  props: RunProps           # bold, italic, underline, strike, color, vertAlign, rStyle,
                            #   size (w:sz) + font (w:rFonts) — kept for PDF, ignored by TUI
Hyperlink { rel_id, runs: Vec<Run> }

Table
  grid: Vec<ColWidth>
  rows: Vec<Row>
Row -> Vec<Cell>; Cell { props, blocks: Vec<Block> }   # cells hold blocks (nesting)
```

`RunProps`/`ParProps` keep an **`other: RawXml`** bucket for attributes/elements
we don't interpret, so re-serialization is lossless for the parts we *do* touch.

---

## 6. Load & save — the round-trip strategy

The single most important design decision for not corrupting documents:

**On load:** unzip the `.docx`, parse `word/document.xml` (+ `styles.xml`,
`numbering.xml`, `document.xml.rels`) into the model, and keep **every other part
byte-for-byte** in a `PreservedParts` map (headers, footers, media, settings,
themes, content-types, custom XML, etc.).

**On save:** re-serialize only the parts we edited (primarily
`word/document.xml`, and `*.rels`/`[Content_Types].xml` *only when* we add a new
relationship such as a hyperlink), then write a fresh ZIP containing the
preserved parts plus our regenerated ones.

**ZIP writing without a compressor:** entries are written with the **STORED**
(uncompressed) method. A STORED-only ZIP is a fully valid `.docx` that Word and
every other reader opens normally. This means **no DEFLATE *compressor* is needed
to start saving** (rust365 only has the *de*compressor). STORED files are larger,
so a from-scratch **DEFLATE encoder is a committed follow-up** — real ZIP
compression on save, added once the STORED-based save path is solid. Deferred,
not skipped.

**Create new:** `template.rs` ships a minimal set of parts (`[Content_Types].xml`,
`_rels/.rels`, `word/document.xml`, `word/styles.xml`, `word/_rels/document.xml.rels`)
for a blank document.

**Fidelity test:** load → save → reload must produce a semantically identical
model; and Word must open every saved file. Validated against the fast365 corpus
(§13).

---

## 7. Rendering engine (model → terminal)

The renderer turns the model into a list of styled lines that the
`document_view` widget scrolls. It is pure (model + viewport width + toggles →
lines), which makes it snapshot-testable.

- **Inline attributes → SGR:** bold→bold, italic→italic, underline→underline,
  strike→crossed-out, super/subscript→indicated with styling (terminals can't
  raise glyphs).
- **Color (reduced):** map Word's RGB to the nearest terminal palette entry
  (16-color by default, 256 when available), and **respect the theme** — never
  emit a foreground that collides with the background; clamp low-contrast colors.
- **Hyperlinks:** OSC 8 escape (`\e]8;;URL\e\\text\e]8;;\e\\`) so modern
  terminals make them clickable, plus underline + a link color.
- **Tables:** column widths from the grid (proportional to viewport), Unicode
  box-drawing (`┌─┬─┐ │ ├─┼─┤ └─┴─┘`), cell content wrapped and clipped;
  **borderless mode** drops the glyphs and uses padding only. gridSpan/vMerge →
  merged cells.
- **Page view:** when on, wrap the body in a box sized from `sect_pr`
  (page width minus margins, clamped to the terminal); when off, text fills the
  width. This is the "switch for page view" toggle.
- **Show invisibles:** render `¶` at paragraph ends, `·` for spaces, `→` for
  tabs, `↵` for line breaks, in a dim color.
- **Headings/styles:** style id drives emphasis (e.g. Heading 1 → bold + accent
  color + spacing). Lists render bullets/numbers from `numbering`.
- **Width correctness:** use Unicode width (wide CJK = 2 cells, zero-width marks
  = 0) so wrapping and tables line up.

---

## 7a. Image rendering (graphics protocols + fallback)

Word images live as media parts (PNG/JPEG/etc.) referenced from `document.xml`
via a `<w:drawing>` → `a:blip r:embed` relationship id. docxy resolves the
rId to the media bytes, decodes with the `image` crate, and renders through
`ratatui-image`, which picks the best path the host terminal supports:

| Protocol | Terminals (representative) |
| --- | --- |
| **Kitty graphics** | kitty, **Ghostty**, Konsole, WezTerm (partial) |
| **iTerm2 inline** (OSC 1337) | iTerm2, WezTerm |
| **Sixel** | foot, xterm (+sixel), WezTerm, **Windows Terminal** (recent) |
| **Unicode half-blocks** | universal fallback (works over SSH / dumb terms) |
| **Text placeholder** | floor: `[image: name.png 640×480]` |

Design constraints and decisions:
- **Capability detection at startup**; choose protocol once, expose the chosen
  mode in the status bar. Never emit graphics escapes to a terminal that didn't
  advertise support — that's what corrupts dumb-terminal output.
- **Block-level placement.** Images render on their own lines (a sized cell box,
  max width = viewport, aspect-preserved), not flowed mid-line. This avoids the
  hardest part of mixing pixel graphics with a reflowing character grid.
- **Compositing churn.** Graphics protocols draw over the cell grid that ratatui
  repaints; `ratatui-image`'s stateful widget handles redraw/placement so images
  don't smear on scroll/resize. Scrolling redraws affected image cells.
- **Windows:** the realistic path is **Sixel on Windows Terminal** (recent
  versions); elsewhere on Windows it degrades to half-blocks/placeholder.
- **Cost control:** decode lazily (only images in/near the viewport), cache
  decoded/encoded results by rId, and cap source dimensions.

This is a *capability layer*, not a core dependency of viewing text — if it's
disabled or unsupported, everything else renders exactly the same.

---

## 7b. PDF export / print (first-class, from the start)

Unlike the terminal view, the PDF output is a **faithful, paginated render** that
honors font families, sizes, colors, and page geometry. It is a separate render
target fed from the same document model, and it lives in **`docxcore`** so it's
pure and unit-testable (PDF bytes are deterministic — ideal for golden tests).

**No font embedding needed.** PDF guarantees 14 **standard base fonts** present
in every reader: Helvetica / Times / Courier, each in regular, **bold**,
*italic*, and bold-italic, plus Symbol/ZapfDingbats. We map the document's fonts
to a base family (sans→Helvetica, serif→Times, mono→Courier) and pick the
variant from run properties. The only data we ship is the **AFM glyph-width
tables** for those base fonts (fixed, well-known) — needed for line breaking.
This keeps the PDF writer from-scratch and dependency-free.

**Layout engine** (`export/pdf_layout.rs` → `export/pdf_write.rs`):
- Page size + margins from `sect_pr` (defaults: Letter/A4, 1" margins).
- Greedy line breaking using AFM advance widths at the run's font size; honor
  alignment, indentation, spacing, and page breaks; flow to new pages.
- **Runs:** bold/italic via the base-font variant; **underline/strike** drawn as
  rules; color via PDF `rg`/`RG`; super/subscript via baseline shift + smaller
  size.
- **Headings/styles:** resolved from the stylesheet into concrete size/weight.
- **Tables:** measured column widths, drawn cell borders (or none), cell text
  wrapped; row splitting across pages later.
- **Hyperlinks:** PDF `Link` annotations with `URI` actions over the run rect.
- **Images:** embedded as XObjects — JPEG via `DCTDecode` passthrough (raw
  bytes, no re-encode); PNG/other decoded (the `image` crate) to an RGB stream.
  Streams may be written **uncompressed** initially (valid PDF); FlateDecode
  reuses the Phase-3 DEFLATE encoder once it exists.
- **Output:** a minimal but valid PDF (header, object table, xref, trailer) —
  exactly the kind of structured byte format we already write well for ZIP/OOXML.

**Why this is feasible from scratch:** PDF text with the standard-14 fonts is a
known, bounded problem; the hard parts (font embedding, shaping) are avoided by
using base fonts. The model already retains `sz`, `rFonts`, color, and `sect_pr`
(the terminal ignores them; the PDF target uses them) — so no model changes are
needed beyond *not discarding* that data.

Invocation: a `:export pdf [path]` command and a headless `docxy in.docx
--pdf out.pdf` mode (the headless path makes PDF export trivially testable and
scriptable without a terminal).

---

## 8. Editor core

- **Cursor** addresses a position as `(block path, run index, char offset)` and
  is normalized after every edit. Selection is an anchor+caret pair.
- **Edit operations** (`ops.rs`): insert/delete text, split/merge paragraphs,
  toggle a run property over a selection (splitting/merging runs as needed),
  apply a paragraph or character **style**, insert/remove list membership,
  insert hyperlink, table row/column ops. Each op returns an inverse op.
- **Undo/redo** (`history.rs`): a command stack of (op, inverse). Coalesce
  consecutive single-character inserts into one undo unit.
- **Dirty tracking** for the save prompt; autosave optional later.

---

## 9. TUI / UX

- **Modeless, approachable** (nano/micro-style), since the pitch is "a friendlier
  Word in the terminal" — not a modal vim. Ctrl-key bindings shown in a hint bar.
  (A vim-style mode can be a later option.)
- **Panels:** the document view (main), a status bar (filename, dirty `*`,
  cursor position, current style, active toggles), and a transient command line
  for `:open`, `:save`, prompts.
- **Style picker:** popup list of paragraph + character styles from the
  stylesheet; apply to the selection/paragraph.
- **Draft keybindings:** `Ctrl-O` open · `Ctrl-S` save · `Ctrl-Q` quit ·
  `Ctrl-Z/Ctrl-Y` undo/redo · `Ctrl-B/I/U` toggle bold/italic/underline ·
  `Ctrl-P` style picker · `F2` toggle page view · `F3` toggle invisibles ·
  `Ctrl-F` find · arrows/PgUp/PgDn/Home/End navigation.

---

## 10. Feature → implementation map (your requested list)

| You asked for | Where it lives |
| --- | --- |
| Open any `.docx` | `docxcore::load` (rust365 reader) |
| No font sizes/families | renderer simply ignores `sz`/`rFonts` |
| Page-view borders (toggle) | `render::page` + `F2` |
| Tables | `render::table` + table edit ops |
| Bold/italic (and u/strike) | `render::inline` SGR + `Ctrl-B/I/U` |
| Links | `render::inline` OSC 8 + insert-hyperlink op |
| Reduced, theme-matched colors | `render::inline` color quantizer |
| Create/view/update `.docx` | `docxcore::{template,load,save}` + editor |
| Select styles | `ui::style_picker` + apply-style op |
| Show invisible symbols | `render::invisibles` + `F3` |
| Images (kitty/ghostty/sixel…) | `render::image` + `ratatui-image` (§7a) |
| Print / export to PDF | `docxcore::export` (standard-14 fonts) + `--pdf` / `:export pdf` (§7b) |

Every requested feature maps to a concrete module — nothing in the list is
blocked or infeasible.

---

## 11. Phased roadmap

- **Phase 0 — Viewer + PDF (proves the stack).** Port `docxcore` read path;
  render a `.docx` read-only with b/i/u, color, links, tables, headings, lists;
  page-view and show-invisibles toggles; scrolling. **Plus a headless
  `docxy in.docx --pdf out.pdf`** covering paragraphs, runs (b/i/u/color),
  headings, links, and pagination (tables/images in PDF follow later). No
  editing. Acceptance: opens the fast365 corpus without panics, renders right,
  and produces valid PDFs (open in a PDF reader; golden-byte tests pass).
- **Phase 1 — Edit text + save.** Cursor, text insert/delete, paragraph
  split/merge, undo/redo; `save` via preserve-original-ZIP + STORED writer;
  "create new" from template. Acceptance: edit → save → reopen in Word intact.
- **Phase 2 — Formatting & styles.** Toggle run properties over selections,
  apply paragraph/character styles, insert hyperlinks. Acceptance: round-trip
  fidelity of applied formatting.
- **Phase 2.5 — Inline images (terminal) + images/tables in PDF.** Terminal-
  graphics rendering via `ratatui-image` (Kitty/iTerm2/Sixel + half-block
  fallback), capability detection, lazy decode + cache (§7a); and extend the PDF
  exporter with tables and embedded images (§7b). Phase 0/1 show the text
  placeholder in the terminal until this lands. Acceptance: images visible in
  kitty/Ghostty and Windows Terminal (Sixel) with clean fallback; PDFs include
  tables and pictures.
- **Phase 3 — Structure & polish.** Table editing (row/col/merge), list
  editing, find/replace, config (theme, keymap), the **DEFLATE encoder** (real
  ZIP compression on save), and an **optional vim-style modal mode**.

Each phase is independently shippable as a `docxy` release using the same
signed-release pipeline as the other repos.

---

## 12. Testing & fidelity

- **Corpus:** reuse the fast365 `.docx` corpus for parse + round-trip tests.
- **Round-trip golden tests:** load → save → reload; assert the model is
  semantically equal and that `document.xml` is well-formed and OPC-valid.
- **Render snapshots:** fixed-width render of fixture docs compared to golden
  text (with toggles on/off).
- **Edit-op property tests:** every op's inverse restores the prior model.
- **PDF golden tests:** deterministic PDF bytes for fixture docs; structural
  checks (valid xref/trailer, expected page count, embedded font references).
- **Manual gate:** a saved file must open cleanly in real Microsoft Word, and
  exported PDFs must open in a standard PDF reader.

---

## 13. Risks & open questions

- **OOXML surface area** is huge; mitigated by the *preserve-don't-understand*
  strategy and a `RawXml` bucket — but some edits near unmodeled structures
  (fields, sectPr boundaries, sdt content controls) need care.
- **Terminal capability variance:** OSC 8 links, 256-color, Unicode width, and
  image protocols (Kitty/iTerm2/Sixel) all differ across terminals; detect at
  startup and degrade gracefully (graphics → half-blocks → text placeholder).
- **Graphics-in-TUI compositing:** pixel images over a repainting cell grid can
  smear on scroll/resize; rely on `ratatui-image`'s stateful widget and keep
  images block-level (§7a).
- **Bidi / complex scripts:** out of scope initially; render LTR.
- **Large documents:** lazy layout / viewport-only rendering if needed.
- **Shared `docxcore`:** extracting it cleanly from rust365 without disturbing
  that shipped tool — do it behind tests, keep rust365's behavior byte-identical.

---

## 14. Open decisions for you

1. **Editing style — DECIDED:** modeless (nano/micro) now; a vim-style modal
   mode is a planned future addition (Phase 3).
2. **Extract a shared `docxcore` crate now, or vendor modules into docxy first**
   and refactor later — plan leans "vendor now, extract once stable."
3. **Images — DECIDED:** render inline via terminal graphics protocols
   (Kitty/iTerm2/Sixel) with half-block and text-placeholder fallbacks (§7a),
   targeting kitty, Ghostty, and Windows Terminal (Sixel) among others. Built in
   Phase 2.5; placeholder shown until then.
