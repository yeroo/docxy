# Docxy

[![CI](https://github.com/yeroo/docxy/actions/workflows/ci.yml/badge.svg)](https://github.com/yeroo/docxy/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/yeroo/docxy/graph/badge.svg)](https://codecov.io/gh/yeroo/docxy)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/yeroo/docxy/badge)](https://scorecard.dev/viewer/?uri=github.com/yeroo/docxy)
[![crates.io](https://img.shields.io/crates/v/docxy.svg)](https://crates.io/crates/docxy)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**A fast terminal (TUI) viewer and editor for Microsoft Word `.docx` and Markdown — right where you live, in the terminal.**

Docxy opens real `.docx` files — text, tables, lists, styles, even images —
renders them faithfully in a character grid, and lets you **edit and save** them
losslessly. It reads and writes **Markdown** too, converts between the two, and
exports to **PDF**. No Office, no browser, no network: it's a single static
binary on top of a small, dependency-free OOXML engine.

<p align="center">
  <img src="assets/screenshot.png" alt="docxy editing a Word document in the terminal" width="860">
</p>

> Docxy deliberately doesn't reproduce Word's pixel-perfect layout — it renders a
> faithful, readable view of the document in a character grid, with a familiar
> ribbon, mouse support, and an optional Vim mode.

## Why docxy?

- **Stay in the terminal.** Read and edit Word documents over SSH, in tmux, or
  from your editor's shell — no GUI required.
- **Lossless by design.** Anything docxy doesn't model (bookmarks, fields,
  content controls, section properties) is preserved byte-for-faithful on save.
- **Zero-dependency core.** The `docxcore` crate is pure `std` — its own
  ZIP/DEFLATE, XML parser, renderer, and PDF writer — so it's auditable and
  trivially embeddable.
- **One file does it all.** `.docx` ⇄ `.md` conversion and `.docx → .pdf` export
  are built in, scriptable, and headless.

## Quick start

```sh
cargo install docxy          # or grab a prebuilt binary (see Install)

docxy report.docx            # open a Word document
docxy notes.md               # open / edit Markdown
docxy                        # launch the welcome screen (new file or open)
```

Want to see it immediately? Generate the showcase document and open it:

```sh
cargo run -p docxcore --example gen_sample   # writes assets/sample.docx
docxy assets/sample.docx
```

## Features

### Documents & editing
- **View & edit** paragraphs and runs (bold / italic / underline / strike /
  color / highlight / sub- & superscript) and **tables**, including merged
  cells — navigate and type directly into cells.
- **Styles** resolved from `styles.xml`; **lists** numbered from `numbering.xml`;
  headings, indents, alignment, tab stops, and horizontal rules.
- **Lossless save** — unmodeled parts are preserved exactly.
- **Find & replace**, full **clipboard** (syncs with the OS clipboard),
  **selection + formatting**, word navigation, and **show-invisibles**.
- **Headers & footers**, multi-section page layout, and **print/page view**.

### Markdown
- Open and edit `.md` files directly; **Save As** to a `.md` or `.docx` name
  converts between the two.
- **View ▸ Markdown** toggles a `.md` file between the rendered document and its
  raw source.
- Headings, **bold/italic/strike**, inline `code`, fenced code blocks,
  blockquotes, links, bullet/ordered lists, thematic rules, and pipe tables all
  map across — and round-trip through `.docx` via real Word styles.
- **Scientific formulas**: `$…$` inline and `$$…$$` display math (LaTeX) convert
  to and from native Word equations (OMML) — fractions, roots, `\sum`/`\int`
  with limits, Greek, scripts, `\left…\right`, and named functions.
- **Mermaid diagrams**: a ```` ```mermaid ```` block becomes a native Word
  drawing (DrawingML shapes + connectors, laid out automatically); the Mermaid
  source is embedded so Word → Markdown restores the exact block. Flowcharts are
  laid out fully; other diagram types are best-effort.

### Images
- Raster (PNG / JPEG / GIF / BMP / TIFF) rendered as **real pixels** via
  kitty / iTerm2 / **Sixel** graphics.
- Legacy **WMF/EMF vector** images rasterized through the OS (Windows).
- Floating, frame-anchored images projected to their real page positions.

### Niceties
- **Welcome screen** on launch with no file: create a `.docx`/`.md` or open one —
  keyboard- and mouse-driven.
- **Mouse** everywhere: click to move, click a link to open, wheel/drag to
  scroll/select, and a fully clickable ribbon and File menu.
- Safe **clickable links** — only `http(s)`, shown for confirmation, opened
  without a shell.
- **Vim mode** (`--vim`): motions, operators, visual mode, `/` search, `:w`/`:q`.
- **PDF export**, including headless.

## Usage

```sh
docxy <file.docx|.md>           # open a Word or Markdown file
docxy                           # welcome screen (new .docx/.md, or open)
docxy <file> --vim              # open with Vim keybindings

# Headless conversion / export (no UI):
docxy in.docx  --pdf  out.pdf   # export to PDF
docxy in.docx  --md   out.md    # convert Word → Markdown
docxy in.md    --docx out.docx  # convert Markdown → Word
```

### Keys

| Keys | Action |
|------|--------|
| type · Enter · Backspace · Delete | edit text |
| arrows · Home/End · PgUp/PgDn | move (Ctrl-←/→ by word) |
| Shift + move | select (Esc clears) |
| Ctrl-B / Ctrl-I / Ctrl-U | bold / italic / underline (over selection) |
| Ctrl-L / Ctrl-E / Ctrl-R | align left / center / right |
| Ctrl-A · Ctrl-C · Ctrl-X · Ctrl-V | select all · copy · cut · paste |
| Ctrl-F | find / replace (Tab toggles replace, Ctrl-A replaces all) |
| Ctrl-S · Ctrl-Z · Ctrl-Y | save · undo · redo |
| Ctrl-Q / Esc | quit |
| F2 · F3 · F4 | page view · show marks · table borders |
| F6 · F7 | edit header · edit footer (Esc returns) |
| F8 · F9 | insert landscape · portrait section at cursor |
| mouse | click to move · click a link to open · wheel/drag to scroll/select |

## Xlsxy — spreadsheets too

The workspace now ships a sibling app: **`xlsxy`**, a terminal editor for
Microsoft Excel `.xlsx` workbooks built on `gridcore`, a dependency-free
SpreadsheetML engine with a real **recalculation engine** — a dependency
graph over your formulas, ~170 Excel functions, Excel-faithful semantics
(error values, coercions, the 1900 leap-year quirk), whole-column
references, defined names, structured table references, 3D sheet spans,
`INDIRECT`/`OFFSET`, `XLOOKUP` and the `*IFS` family, the full
number-format runtime, **dynamic arrays** (`FILTER`/`SORT`/`UNIQUE`/
`SEQUENCE` spill into neighboring cells, `A1#` spill references, `@`,
`LET`, `#SPILL!` blocking and recovery), **`LAMBDA`** (custom functions via
defined names, `MAP`/`REDUCE`/`SCAN`/`BYROW`/`BYCOL`/`MAKEARRAY`,
elementwise lifting like `ABS(A1:A3)`), **pivot-table refresh and editing** (a
columnar group-by/aggregate engine recomputes pivots from current data —
`F9` in the TUI, automatic under `--recalc`; `Ctrl-P` edits a pivot's
fields — or creates a new pivot from the selected data range), a **data model**
(`gridcore::model`: multiple tables with relationships, Excel-formula
measures plus DAX-style row-context iterators like
`SUMX(Sales,[@Qty]*[@Price])`, filter context through star schemas, CSV
sources — `xlsxy data.csv` imports directly; `Ctrl-M` manages the model in
the TUI and materializes reports, with definitions persisted in the file), and the
same lossless
round-trip guarantee: anything it doesn't model (charts, pivots, conditional
formatting…) is preserved byte-for-byte. Formulas it can't evaluate yet keep
Excel's cached results and are saved untouched.

```sh
xlsxy book.xlsx                   # open a workbook (grid, formula bar, tabs)
xlsxy in.xlsx --recalc out.xlsx   # headless: recalculate everything, save
xlsxy in.xlsx --csv out.csv       # headless: export the first sheet as CSV
xlsxy corpus/xlsx/*.xlsx --verify # conformance scoreboard: recalc + diff
                                  # against cached values (461/461 = 100%
                                  # on the LibreOffice-oracle corpus)
```

Type to replace, `F2` to edit, `=` starts a formula; copy/paste and
fill-down translate relative references like Excel; insert/delete rows and
columns rewrites every affected formula workbook-wide; find, Save As, and
sheet add/rename/delete round out the basics; range selections show
Sum/Average/Count in the status bar. Try it: `cargo run -p gridcore --example gen_sample_xlsx &&
xlsxy assets/sample.xlsx`. The design and roadmap (conformance scoreboard,
dynamic arrays, pivot engine) live in [SPREADSHEET.md](SPREADSHEET.md).

## Yppxy — project schedules too

Completing the trilogy (`doc→docx`, `xls→xlsx`, **`mpp→yppx`**), the workspace
ships **`yppxy`**, a terminal editor for **project schedules** built on
`projcore`, a dependency-free scheduling engine. It reads Microsoft Project's
open **MSPDI** XML (what Project produces via *Save As → XML*), schedules it with
a real **Critical Path Method** engine — forward/backward passes over
working-time calendars, computing early/late start & finish, total/free slack,
and the critical path — and saves to a native **`.yppx`** package (an OPC
ZIP container, the project analog of `.docx`/`.xlsx`). The TUI is a task outline
beside a **live terminal Gantt chart** that reschedules on every edit; critical
tasks show in amber, summaries roll up over their children, milestones as
diamonds. Dependencies (FS/SS/FF/SF with lag/lead) and hard constraints
(SNET/FNET/MSO/MFO…) are modeled; a schedule exports to a **Markdown/Mermaid
Gantt** block that renders anywhere docxy's diagrams do.

```sh
yppxy plan.xml                    # open MSPDI XML (or a .yppx package)
yppxy plan.yppx --gantt-md out.md # headless: export a Markdown Gantt chart
yppxy plan.xml  --save out.yppx   # headless: convert to the native package
```

Like docxy and xlsxy, yppxy has the same **ribbon** (File · Task · Schedule ·
View — `F9` to engage), the same **File backstage** (`Alt-F`: New / Open / Info
/ Save / Save As / Export / Exit with a folder browser and live preview), a
start screen, a light/dark theme toggle, and mouse support. Try it:
`yppxy corpus/mspdi/10-summary.xml`.

### Keys

| Keys | Action |
|------|--------|
| ↑ ↓ · j / k · g / G | move the selection (top / bottom) |
| ← → · h / l | scroll the Gantt timeline |
| n · Insert | add a task below |
| x · Delete | delete the task |
| Tab · Shift-Tab | indent / outdent (auto-forms summary tasks) |
| Enter · F2 | rename the task |
| d | set duration (`3d` / `4h` / `2w`) |
| p | add a predecessor by task ID |
| c | set a date constraint (`SNET 2026-03-05`, `MSO …`, `none`) |
| a | assign a resource to the task (created on first use; empty clears) |
| b | set the baseline (planned-vs-current variance in the header) |
| L | toggle resource leveling (delay bars to fit resource capacity) |
| Ctrl-F · F3 | find task by name · repeat |
| Ctrl-Z · Ctrl-Y | undo · redo |
| F9 · Alt-F | engage the ribbon · open the File menu |
| Ctrl-S · Ctrl-E | save · export a Markdown Gantt |
| Ctrl-Q · q | quit (q warns on unsaved changes) |
| mouse | click a tab/button, click a task row, wheel to scroll/pan |

Launch with `--vim` for a modal mode (`:w`/`:q`/`:wq`/`:q!`, `u` undo, `/`
search). The light/dark theme persists between sessions.

A separate crate, **`mppread`**, reads the OLE2 Compound File container of
legacy binary `.mpp` files. It decodes the documented metadata (title/author/
company/dates via OLE property sets) plus each task's **name, start/finish dates,
outline level, and predecessor links** from the version-specific var-data,
fixed-record, and constraint blocks (auto-detected across MPP9, MPP12/14, and the
newest generation, verified on real Microsoft Project, ProjectLibre, and
Project-98 files), so `yppxy legacy.mpp` opens with the real WBS tree, schedule,
and dependency network. The newest generation decodes names and dates; its
outline/link tables and link lag remain to be reversed.

The design, the CPM engine, resource leveling, and the format landscape are
written up in [PROJECT.md](PROJECT.md).

## Offxy in VS Code — the same engines, in an editor tab

Because `docxcore` and `gridcore` are pure `std` with no third-party crates,
they compile straight to **WebAssembly** — so both engines (parse → render →
edit → **lossless save**) run inside a **VS Code editor tab**. The
[`offxy-vscode`](offxy-vscode) extension opens a `.docx` or `.xlsx` on the same
faithful character-grid rendering the terminal apps use, at the editor's own
font and size and honoring your color theme — no ribbon, just the keyboard and
command palette, like editing code. Each format is a binary custom editor
(`offxy.docxEditor` / `offxy.gridEditor`) with native dirty state, undo/redo,
Save/Save As, and hot-exit backups, and because it edits the real OOXML model
(not HTML or an intermediate form), it keeps the document/workbook's structure
intact on save — the lossless round-trip that the crowded field of HTML-based
`.docx`/`.xlsx` extensions lacks. The Excel editor adds a virtualized grid,
formula bar, and full `gridcore` recalculation on edit.

The engines are two small `.wasm` builds — [`docxwasm`](docxwasm) and
[`gridwasm`](gridwasm), each a hand-written C-ABI bridge, no `wasm-bindgen`.
The architecture — the wasm ABIs, the host ↔ webview split, and how VS Code's
edit events stay in lockstep with each engine's own undo stack — is written up
in [VSCODE.md](VSCODE.md).

## Install

```sh
cargo install docxy   # the document editor
cargo install xlsxy   # the spreadsheet editor
cargo install yppxy   # the project scheduler
```

Or grab prebuilt binaries (Linux / Windows / macOS) from the
[latest release](https://github.com/yeroo/docxy/releases/latest) — both apps
ship with every release, each checksummed, cosign-signed, and carrying a
build-provenance attestation.

## Image support

Real image pixels need a graphics-capable terminal:

- **WezTerm** (kitty + Sixel + iTerm2) — best.
- **Windows Terminal ≥ 1.22** (Sixel).
- Most other terminals fall back to a labeled placeholder box.

WMF/EMF vector images are rasterized via the OS GDI on Windows; on other platforms
they show as boxes.

## Building from source

```sh
git clone https://github.com/yeroo/docxy
cd docxy
cargo build --release
cargo test
```

The workspace has eight crates:

- **`opccore`** — pure, `std`-only OPC container plumbing (ZIP read/write,
  DEFLATE, XML pull parser) shared by every engine.
- **`docxcore`** — the WordprocessingML engine (document model, rendering,
  and the from-scratch PDF writer). No third-party dependencies.
- **`gridcore`** — the SpreadsheetML engine (workbook model, formula
  parser/evaluator, dependency-graph recalculation, lossless xlsx I/O).
- **`projcore`** — the project-scheduling engine (task/calendar model, MSPDI
  read/write, Critical Path Method scheduler, Markdown/Mermaid Gantt export,
  native `.yppx` OPC package). `std`-only, on top of `opccore`.
- **`mppread`** — `std`-only reader for the OLE2 Compound File container of
  legacy binary `.mpp`/`.doc`/`.xls` files (MS-CFB).
- **`docxy`** — the document TUI (ratatui), clipboard (arboard), and image
  rendering (ratatui-image).
- **`xlsxy`** — the spreadsheet TUI (ratatui + arboard).
- **`yppxy`** — the project-scheduler TUI with a live terminal Gantt chart
  (ratatui).

### Examples

```sh
cargo run -p docxcore --example gen_sample [out.docx]   # build the showcase doc
cargo run -p docxcore --example dump_doc -- assets/sample.docx   # inspect a .docx
cargo run -p gridcore --example gen_sample_xlsx   # build the showcase workbook
cargo run -p projcore --example gantt_md -- corpus/mspdi/10-summary.xml  # Gantt → Markdown
cargo run -p projcore --example convert  -- in.xml out.yppx   # MSPDI ⇄ .yppx
cargo run -p mppread  --example streams  -- some.mpp   # list a .mpp's streams
cargo run -p mppread  --example tasknames -- some.mpp  # decode a .mpp's tasks + dates
```

## License

MIT © yeroo
