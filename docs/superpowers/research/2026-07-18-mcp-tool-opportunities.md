# What's worth adding to the docxy/xlsxy MCP surface — research report

*2026-07-18. Inputs: full capability inventories of docxy/docxcore and xlsxy/gridcore
(code locations cited), plus a survey of the external MCP office-tooling landscape
(per-server tool lists, demand signals; sources at the end).*

## The four examples, answered first

| Idea | Verdict | Why |
|---|---|---|
| **docx → md** | **Build now — trivial** | `docxcore::markdown::to_markdown` already exists (headings, bold/italic, lists, tables, links, math, even mermaid) — it's wired only to the CLI `--md` flag and save-as, never to a ctl verb. Reading docs as markdown is THE default agent ingestion path (Anthropic's own docx skill routes all reads through pandoc→markdown). Bonus nobody else has: our export reads the **live buffer including unsaved edits**. |
| **md → docx (write)** | **Build now — the single highest-value item** | `from_markdown` parses everything above. Today agents can only write *plain text* (`Clip::from_text`); a `doc.insert-markdown` / `replace-range` markdown mode makes agents able to author **formatted** documents (headings, bold, lists, tables, hyperlinks) through the live editor with real undo. Moderate effort: splice `from_markdown(text).body` instead of the text clip. This turns docxy from "notepad with extra steps" into a report-writing surface. |
| **xlsx → csv and back** | **Build now — trivial both directions** | Export: `sheet_to_csv` (real display formatting, RFC-4180). Import: `Frame::from_csv` + xlsxy's own `csv_to_pkg`. Both core-level, wired only to CLI/backstage. Matches the strong "spreadsheet-as-dataframe" demand (pandas/CSV MCP servers are their own thriving niche). |
| **Charts** | **Read: cheap, do it. Authoring: skip for now** | Reading existing charts is trivial (`parse_chart` → kind/title/categories/series). *Creating* charts is HARD — gridcore has no authoring model at all (charts survive save only via byte-preservation). Notably, Anthropic's own xlsx skill also skips chart creation entirely; third-party servers headline it but implement it shallowly. Revisit only with real user pull. |
| **Pivot tables** | **Ad-hoc pivot: build now. Persistent pivot-create: later** | The gem: `frame::pivot` is a complete standalone pivot engine (multi-level grouping, 11 aggregations, filters, subtotals, calculated fields). A `sheet.pivot {range, rows, cols, values}` verb that returns computed results **without touching workbook XML** serves the dominant agent workflow ("aggregate via executed code, never LLM arithmetic" — SheetAgent, LlamaIndex Spreadsheet Agent). Refresh of existing pivots is already exposed via `wb.recalc`. Creating a *persistent* workbook pivot is MODERATE (building blocks exist: `add_pivot`, `rewrite_pivot_definition`) — a good second step. |

## Why the live-editor model matters (positioning)

The landscape splits cleanly: **reading/ingestion is converter territory** (pandoc,
MarkItDown — no one needs a live session to read), but a live editor is the *only*
answer for three things file-based servers can't do:

1. **Co-editing an open document** — the user watches agent edits land, dirty flag and
   Ctrl+Z included (microsoft/mcp#2531 and word-mcp-live both name "agent and human on
   the same open document" as the unfilled gap).
2. **Operations that need a running engine** — recalc, pivot refresh, formula
   evaluation against live state.
3. **Live-state reads** — exporting/reading *unsaved* work.

So: prioritize verbs that exploit liveness (formatted writes with real undo, in-place
analysis, comments, eval-preview) over pure file conversion, where pandoc already
exists — though built-in md/csv export is still worth it for zero-setup and live-buffer
access.

## Priority tiers

### Tier 1 — free wins (core code exists; wiring only; ~a verb each)

**docxy:** `doc.export {format: md|pdf|txt}` · `doc.comments` (read — parse_comments is
complete and unexposed) · `doc.notes` (footnotes/endnotes read) · `doc.replace-all`
(global find/replace — `Editor::replace_all` exists) · `doc.stats` (word/char/para
count) · `doc.undo`/`doc.redo` (agents currently can't revert their own edits) ·
`doc.header`/`doc.footer` (read) · `doc.metadata` (read core.xml props) · surface
`protection()`/`watermark()` in `doc.path`.

**xlsxy:** `comment.list/add/remove` (**fully implemented read+write core incl.
threaded comments** — the single most shovel-ready feature in either app; comments are
a named requirement in the landscape: Word IQ, docx-mcp) · `range.set` (batch multi-cell
set over `apply_on` — agents filling tables via N single `cell.set` calls today is the
kind of friction issue trackers complain about) · `wb.export-csv` / `wb.import-csv` ·
`sheet.pivot` (ad-hoc, via `frame::pivot`) · `wb.replace-all` · `sheet.add/remove/rename`
(`add_sheet`/`remove_sheet`/`rename_sheet` all core, reference-rewriting included) ·
`row.insert/delete`, `col.insert/delete` (`edit::insert_rows` etc.; needs the mechanical
engine-rebuild glue) · `formula.eval` (side-effect-free preview via `eval_formula_at` —
lets agents test before committing) · `sheet.stats` (selection Sum/Count/Average) ·
chart read-out (`parse_chart`) · pivot listing (walk `workbook.pivots`).

### Tier 2 — the two keystones (moderate; each unlocks a whole tier)

1. **docxy range-selection primitive** (`agent::select(start, end)` internal plumbing):
   today only `replace_range` builds selections. Once it exists, ~15 formatting verbs
   become one-line wrappers over existing `Editor` methods: bold/italic/underline/
   strike/font/size/color/highlight, alignment, indent, **paragraph style (headings)**,
   lists, sort-paragraphs, comment-anchoring (write). Formatting tools are table stakes
   in every Word MCP server (Office-Word-MCP-Server's most-demoed group).
   Recommendation: one `doc.format {start, end, patch}` verb + `doc.set-style`, not 15
   verbs.
2. **docxy markdown write path** (`doc.insert-markdown` / markdown flag on
   replace/insert/append) — see table above. Arguably do this *before* the formatting
   primitive: it covers 80% of formatting demand (headings/bold/lists/tables/links)
   with zero new verb-surface complexity, in the format agents already speak.

**xlsxy keystone:** `cell.format {range, patch}` — number formats, bold/italic, colors,
alignment all round-trip already (`Styles::intern`, `apply_format`); needs only a
JSON patch schema. Column widths too (`set_col_width`). This is the #1 gap vs
haris-musa/excel-mcp-server's `format_range`.

### Tier 3 — worthwhile, needs real design or persistence work first

- **xlsxy persistence caveat (one structural theme, not five bugs):** `save_xlsx` only
  regenerates `sheetData/dimension/cols`; merges, hyperlinks, freeze panes, CF rules,
  and defined names ride through as original bytes — in-memory mutations of those
  **would not survive save** until `xlsx.rs` learns to splice them. Any verb touching
  these must fund that save-path work first. (Merge/unmerge, hyperlink-write,
  freeze-write, named-range-write, CF-write all live here.)
- Persistent pivot-table creation (`SheetPackage::add_pivot` exists; needs an ergonomic
  constructor + verb schema).
- DataModel verbs (relate/measure/model-pivot — clean core API, niche audience).
- docxy: header/footer *write* (extract the TUI's splice pattern), page-setup write
  (extract sectPr builder), insert-field, metadata write, comment write (needs keystone
  1), hyperlink insert (free once markdown-write lands), precise table-cell
  read/edit (needs path addressing in `agent.rs`).
- xlsxy: dependents/precedents tracing (data exists internally; agents debugging
  spreadsheets would love it), circular-ref listing, fill-down/right, sheet reorder.

### Not worth it now (engine work, weak or unproven demand)

- **Chart authoring** (no model; even Anthropic's skill skips it).
- **docx tracked changes accept/reject/record** — genuinely demanded in the landscape
  (docx-mcp, word-mcp-live differentiate on it) but a whole new subsystem in docxcore
  (model is read-only today). The one to revisit first if docxy targets review
  workflows.
- Sort/AutoFilter in xlsxy (no engine support at all), cell borders / row heights
  (not modeled), regex find (docxcore is deliberately std-only), bookmarks (unmodeled),
  image insertion (needs DrawingML generation), watermark/protection write, ODS/xls.

## Suggested build order (if you want a starting plan)

1. **Wave 1 (all Tier 1):** ~20 trivial verbs + MCP tools, split docxy/xlsxy. Biggest
   visible jump in capability per effort ever available in this codebase.
2. **Wave 2:** docxy markdown-write; xlsxy `cell.format` + column widths.
3. **Wave 3:** docxy selection primitive + `doc.format`/`doc.set-style`; xlsxy
   persistent pivot creation.
4. **Backlog gate:** xlsx save-path splicing theme (unlocks merges/links/freeze/CF/names
   writes as a batch); tracked changes as its own project.

## Sources (landscape)

Office-Word-MCP-Server (GongRzhe) · haris-musa/excel-mcp-server (TOOLS.md; ~4k stars) ·
negokaz/excel-mcp-server · sbroenne/mcp-server-excel (live-COM, 232 ops) ·
ykarapazar/word-mcp-live · SecurityRonin/docx-mcp (tracked-changes XML surgery) ·
Softeria/ms-365-mcp-server · google_workspace_mcp · vivekVells/mcp-pandoc ·
Microsoft MarkItDown MCP · anthropics/skills docx+xlsx SKILL.md (primary) ·
microsoft/mcp#2531 (in-place editing demand) · Microsoft Work IQ MCP docs ·
Arcade.dev O365 launch · LlamaIndex Spreadsheet Agent · SpreadsheetBench.
Code citations: see `.superpowers/sdd/` inventory transcripts; key locations named
inline above.
