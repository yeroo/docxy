# Corpus-driven gap analysis — docxy & xlsxy

_Generated 2026-07-12. Method: every file in the two test corpora was classified
by scanning its OPC parts and body XML (`corpus/tools/classify.py` /
`classify_xlsx.py`), producing per-file feature tags — and, for xlsx, the exact
set of worksheet functions each file calls. Those manifests
(`corpus/classification.json`, `corpus/classification-xlsx.json`) tell us **what
the corpus stresses**. A source audit of `docxcore`/`docxy` and
`gridcore`/`xlsxy` tells us **what each engine currently does** with it. The gap
is the cross-product, ranked by corpus frequency × severity._

- **docx corpus:** 248 real "tricky files" (OpenXML SDK test assets).
- **xlsx corpus:** 555 files — 538 real (LibreOffice `sc/qa` + Apache OpenOffice)
  + 17 synthetic oracles. 88 carry formulas calling **244 distinct functions**.

Support levels used throughout:
**FULL** = modeled + shown + lossless round-trip · **DISPLAY** = shown but not
losslessly saved and/or not editable · **PRESERVED** = round-trips byte-faithful
but not understood/shown · **MISSING** = dropped or never parsed.

---

## Part 1 — docxy (.docx / WordprocessingML)

### The systemic issue that dominates the docx gaps

`save_package` **regenerates `word/document.xml` from the semantic model**
(`package.rs:557`) and keeps every *other* part byte-for-byte. Whole unmodeled
elements (bookmarks, `w:sym`, drawings, fields) survive because the loader
captures them as opaque `Raw` nodes. Tracked-change wrappers are now modeled
separately. But any **property child of a
modeled element** — `pPr`, `rPr`, `tblPr`, `trPr`, `tcPr` — that isn't in the
parser's whitelist is **silently dropped on save, even when the user never
touched that paragraph.** This one architectural fact is the root cause of the
top-ranked docx gaps.

### What's already solid (no action needed)

Structural text editing, runs (b/i/u/strike/caps/color/size/font/highlight),
**tables** (grid, gridSpan, vMerge), **lists/numbering** (Word-accurate markers),
**styles** (resolved for display), **headers/footers** (default/first/even,
editable, creatable), **sections** (multi-section, landscape, page geometry),
**multi-column** newspaper layout, **images** (raster + WMF/EMF via GDI, inline
& floating), **charts** (text bar/pie), **text boxes** (editable), **math**
(OMML → Unicode; LaTeX authoring), **comments** (add/delete/show), **simple
fields**, **external hyperlinks**, and **bidirectional DOCX layout** (UBA visual
order for `w:bidi`, resolved `w:rtl`, Unicode controls, logical copy/export).

### Document protection and watermark guarantees

`docxcore` parses `w:documentProtection` plus recommendation-only and
password-backed `w:writeProtection` as structured metadata. docxy applies one
policy to TUI, control, and MCP mutation routes before they touch the document,
package parts, history, dirty flag, or save state:

| Protection state | Content | Structure | Formatting | Comments | Package metadata |
|---|---:|---:|---:|---:|---:|
| absent, disabled, or recommendation-only write protection | allow | allow | allow | allow | allow |
| password-backed write protection | deny | deny | deny | deny | deny |
| enforced read-only | deny | deny | deny | deny | deny |
| enforced comments-only | deny | deny | deny | allow | deny |
| enforced formatting-only | allow | allow | deny | allow | allow |
| enforced forms-only | deny | deny | deny | deny | deny |
| enforced tracked-changes-only | deny | deny | deny | deny | deny |
| enforced unknown mode | deny | deny | deny | deny | deny |

Password-backed write protection and the last three modes deliberately fail
closed. docxy cannot yet verify a password, restrict edits
to modeled form fields or automatically emit tracked revisions, so allowing an
ordinary edit would violate the document's declared protection. Advisory write
protection remains editable and is shown as `read-only (recommended)` rather
than being treated as enforced protection. Navigation, selection, copy, find,
inspection, same-format save, and export remain available in every mode.
Untouched same-format saves preserve the original main document XML verbatim.
The
complete mutation classification and stable denial codes are in
[`docs/docx-mutation-inventory.md`](docs/docx-mutation-inventory.md).

Applied VML text watermarks render as muted, centered labels on each applicable
page in page view. Header inheritance and default/first/even variants determine
which pages receive a label. The overlay prefers an empty page row and falls
back to the page frame when content is dense; Unicode and narrow pages are
clipped safely. It is a screen layer only: it never enters document text, hit
testing, caret/selection maps, copy/export output, or saved OOXML. Picture and
unrecognized watermarks show explicit `preview unavailable` labels because the
terminal renderer does not rasterize those header shapes. Continuous view keeps
the status indicator but does not paint page overlays.

### Ranked docx gaps

| # | Gap | Corpus weight | Current | Target | Severity |
|---|-----|--------------|---------|--------|----------|
| **D1** | **Table / cell / paragraph *properties* dropped on save** — `tblPr`, `trPr`, most of `tcPr`, `w:shd`, `w:pBdr` sides, widths, `vAlign`, `w:spacing`, `outlineLvl` | tables **47** + shading **21** + ParaPr | MISSING (lost on save) | round-trip + render | **Critical** (silent data loss) |
| **D2** | **Footnotes & endnotes** — `footnotes.xml`/`endnotes.xml` never read; reference run emitted empty → **anchor lost on save**, part orphaned | 9 + 5 = **14** | MISSING | load + render markers + panel | High |
| **D3** | **Tracked changes** — imported inline and property revisions are modeled, rendered, navigable, and accept/reject review is undoable; unsupported records remain lossless and reportable. New edits are not automatically tracked | **22** | REVIEWABLE | authoring (deferred) | Low |
| **D4** | **Content controls** — block, inline, and table-row `w:sdt` wrappers and arbitrary properties round-trip while their content remains visible/editable; authoring and form UX are not exposed | **27** | PRESERVED + editable payload | property/form UI (deferred) | Low |
| **D5** | **Symbols** (`w:sym`) — preserved but glyph never rendered → symbol chars invisible | **11** | PRESERVED-but-hidden | map to Unicode/font glyph | Medium |
| **D6** | **Internal links & bookmarks** — anchor hyperlinks unwrapped to plain text; bookmarks round-trip but aren't navigation targets | bookmarks **30**, hyperlinks 28 | DISPLAY (inert) | clickable in-doc nav | Medium |
| **D7** | **RTL / bidi** — `w:bidi`, style-inherited direction, resolved `w:rtl`, and Unicode controls render in UBA visual order while copy/export stay logical; `w:dir`/`w:bdo` containers and complex-script shaping remain deferred | **6** | SUPPORTED (terminal UBA) | shaping/container polish | Low |
| **D8** | **Page borders** — preserved byte-faithful and surfaced by a status indicator, but not drawn on the page | 3 | PRESERVED | render | Low |
| — | **Encrypted docx** — detected and refused | 2 | MISSING (rejected) | out of scope (needs crypto) | — |

#### D4 row-level content-control contract

Row-level controls (including `w15:repeatingSection` and
`w15:repeatingSectionItem`) use a mixed logical table-child representation.
`Table::rows` remains the ordinary visible/editable row list, while
`Table::row_boundaries` stores invisible children at row gaps. An `SdtOpen`
boundary contains the captured `<w:sdt>...<w:sdtContent>` prefix, an
`SdtClose` contains the captured suffix, and `Raw` retains any otherwise
unknown child in its original position. This preserves wrapper attributes,
`w:sdtPr`, `w:sdtEndPr`, unknown XML, nesting, adjacency, and empty controls.
Boundary anchors must be ordered, in range, balanced, and properly nested;
serialization validates that invariant and never emits an invalid boundary
stream. Malformed or truncated input is recovered to a balanced model without
dropping visible rows.

All boundaries at a row gap are applied before the row at that position. Thus
inserting at a control's first row inherits that row's active controls;
inserting after its closing boundary remains outside; and a shared
close-then-open gap deterministically assigns the inserted row to the following
control. Deletion collapses the following gap onto the preceding one. Deleting
the first or last row keeps the remaining rows in the same group, and deleting
the final visible row retains the definition as a balanced empty control.
Cloning and equality include boundary metadata, while plain text and rendering
ignore it. Callers that change row count must use `Table::insert_row` and
`Table::remove_row` rather than mutate `Table::rows` directly.

Validation on 2026-08-29 covered 470 `docxcore` unit tests and 10 package
integration tests, followed by the full workspace test suite, all-target
Clippy with warnings denied, formatting, and whitespace checks. The fixture is
`docxcore/tests/fixtures/row-content-controls.docx`; its reproducible generator
is `docxcore/tests/fixtures/generate-row-content-controls.ps1`, and the package
assertions are in `docxcore/tests/docx_integration.rs`. They cover repeating
sections, nested/adjacent/empty controls, unknown XML, row properties, merged
cells, targeted cell editing, and existing block/inline wrapper behavior.

Deliberate deferrals are a form-filling UI, content-control/data-binding
property editing, and protection enforcement. These properties are preserved
but remain inert; wrapper metadata is not rendered as document text.

---

## Part 2 — xlsxy (.xlsx / SpreadsheetML)

xlsxy's stated north star (SPREADSHEET.md) is **calculation fidelity first**;
visual formatting *editing*, charts, pivots, and embedded objects are explicit
non-goals to *edit* — they must **preserve** losslessly but need not render. The
gap analysis is framed against that: **calc-engine gaps are in-scope and high
priority; display/preservation gaps matter mainly for the compare tool.**

### What's already solid (better than expected)

Full lexer→parser→AST→evaluator with **dependency-graph recalc**; shared &
array/CSE formulas; **dynamic arrays / spill** (`#SPILL!`, `A1#`, `@`); structured
table refs (`Table[Col]`, `[@x]`, `[#Totals]`); cross-sheet & **3-D refs**;
**defined names**; **LAMBDA/LET** + MAP/REDUCE/SCAN/BYROW/BYCOL/MAKEARRAY;
**custom number formats** (full runtime); merged cells; 1904 dates; legacy +
threaded **comments**; **~325 builtins → 190/244 (78%) of corpus functions.**
Pivot tables are read + refreshable (partial).

### Ranked xlsxy gaps

#### 2a. Calculation engine (in-scope — highest priority)

| # | Gap | Corpus weight | Current | Notes |
|---|-----|--------------|---------|-------|
| **X1** | **`SUBTOTAL`** | blocks **8 files** (5 solely) | MISSING | Single highest-value function. Not just a sum: filter/hidden-row aware, 1xx vs 10x codes. |
| **X2** | **`FORMULATEXT`** | blocks **4** (1 sole) | MISSING | Cheap — AST serializer already exists; return a cell's formula source. |
| **X3** | **`.PRECISE` / `ISO` / `AGGREGATE` cluster** — CEILING.PRECISE, FLOOR.PRECISE, ISO.CEILING, AGGREGATE | ~2-3 files (co-occur) | MISSING | First three are trivial wrappers over existing rounding; AGGREGATE is larger (19 sub-fns × ignore-options). Ship together. |
| **X4** | **`CELL`, `FREQUENCY`** | 1 file each (both sole blockers) | MISSING | Guaranteed +2 files. Also add `CELL`/`INFO` to the volatile set. |
| **X5** | **Statistical distributions** — NORM/T/F/CHISQ/BETA/GAMMA/BINOM/POISSON/WEIBULL/… (42 functions) | high count, **~2 files** | MISSING | 78% of the missing-function *count* but almost no file-unblock yield (concentrated in ~2 "distribution catalog" workbooks that also call other missing fns). High numeric cost (erf/gamma/incomplete-beta). **Defer.** |

> Net: **X1–X4 (~8 functions) move ~12–13 of the 15 function-blocked files to full coverage.** X5 is completeness-for-its-own-sake.

#### 2b. Feature evaluation & display (mostly PRESERVED-only today)

| # | Gap | Corpus weight | Current | In-scope? |
|---|-----|--------------|---------|-----------|
| **X6** | **Conditional formatting** — parsed-through, not evaluated/rendered | **51** | PRESERVED | Borderline (display) — high value for the compare tool |
| **X7** | **AutoFilter** state not applied | **35** | PRESERVED | Borderline |
| **X8** | **Data validation** not enforced/surfaced | **16** | PRESERVED | Borderline |
| **X9** | **Frozen/split panes** not driven from the file's `pane` state | **19** | PARTIAL | In-scope (grid UX) — cheap |
| **X10** | **Hyperlinks** not modeled/clickable | 8 | PRESERVED | In-scope — cheap |
| **X11** | **Images** preserved but **not rendered** in the TUI | 16 | PRESERVED | Display — can reuse docxy's `ratatui-image` pipeline |
| **X12** | **Charts / drawings** not rendered | 167 / 224 | PRESERVED | Non-goal to edit; text-box rendering (à la docxy) is optional polish |
| **X13** | **Pivot tables** — page filters, hidden items, calculated fields not refreshed | 69 | PARTIAL | In-scope longer-term (BI direction) |
| — | **External links** — reference other workbooks | 20 | PRESERVED | Out of scope (no sibling files) |
| — | **Protection** not enforced | 16 | PRESERVED | Low (preserve is correct) |

---

## Part 3 — Remediation plan

Phases are ordered by value/effort. Each lists the corpus impact and the touch
points found in the audit.

### docxy

**Phase D-1 — Stop silent save-loss of properties _(Critical; unblocks tables 47 + shading 21 + ParaPr)_**
Two complementary moves:
1. **Safety net (generalizes):** in the loader, capture *unknown* child elements
   of `pPr`/`rPr`/`tblPr`/`trPr`/`tcPr` as raw slices and re-emit them in
   `serialize` in schema order — same trick already used for whole-element
   `Raw`, applied one level deeper. Guarantees nothing is lost on save.
2. **Model the high-frequency display props** so they also *render*: `w:shd`
   (para/cell/table), `w:spacing` (before/after/line), `w:pBdr`/`tblBorders`/
   `tcBorders` (all sides + sz/color), cell/table widths, `w:vAlign`,
   `outlineLvl`. Touch: `model.rs` ParProps/table structs, `load.rs:638-698,
   930, 1007-1033`, `serialize.rs:81-219, 358-400`, `render.rs`.

**Phase D-2 — Footnotes & endnotes _(High; 14 files)_**
Parse `footnotes.xml`/`endnotes.xml`, keep the reference marker on save (fixes
the orphaned-part bug), render superscript markers + a notes panel (mirror the
comments UI). Touch: new `docxcore::notes`, `load.rs:794-801`, render + app.

**Phase D-3 — Tracked changes _(completed review surface; 22 files)_**
`w:ins`/`w:del`/`delText` and run/paragraph/table/row/cell/section property
changes are modeled and rendered. The Review ribbon and control/MCP routes
navigate and accept/reject supported records with native undo, structured
outcomes, and package round-trip coverage. Unsupported move/custom records are
enumerated and preserved. Automatically tracking new edits remains deferred.

**Phase D-4 — Visibility fixes _(Medium; symbols 11 + internal nav 30)_**
Render `w:sym` glyphs (symbol-font → Unicode map) and make internal
hyperlinks/bookmarks navigable (jump to anchor). RTL/bidi visual layout is
complete for the supported terminal contract; remaining work is complex-script
shaping and OOXML directional-container polish.

**Phase D-5 — Structure preservation & inert features _(Lower)_**
`w:sdt` wrapper reconstruction is complete for block, inline, and row-level
controls (see the D4 contract above). Remaining optional work is authoring
content-control properties/forms and rendering page borders. Watermark previews
and protection enforcement are complete for the supported contract above. SVG
decode remains optional if a raster path is added for picture watermarks.

### xlsxy

**Phase X-1 — Function ROI cluster _(High; ~12-13 of 15 blocked files)_**
Implement `SUBTOTAL`, `FORMULATEXT`, `CELL`, `FREQUENCY`, `CEILING.PRECISE`,
`FLOOR.PRECISE`, `ISO.CEILING`, `AGGREGATE`; add `CELL`/`INFO` to the volatile
set. Touch: dispatch `formula.rs:4710`, `is_volatile` `formula.rs:2137`. Verify
each against the corpus oracle values (`xlsxy … --verify`).

**Phase X-2 — Cheap in-scope UX wins _(Medium; frozen panes 19 + hyperlinks 8)_**
Drive freeze from the file's `sheetViews/pane`; model + click hyperlinks.

**Phase X-3 — Conditional formatting & validation _(Medium-High for compare fidelity; 51 + 16 + 35)_**
Evaluate conditional-formatting rules and reflect them in the grid; surface data
validation and AutoFilter state. These are the biggest untapped display surface
and make side-by-side-vs-Excel far more meaningful.

**Phase X-4 — Rendering of preserved objects _(Optional polish; images 16, charts 167)_**
Render embedded images in the TUI (reuse docxy's `ratatui-image` pipeline); draw
charts as text boxes (à la docxy). Non-goal to *edit*, but high corpus presence.

**Phase X-5 — Statistical distribution family _(Completeness; ~2 files)_**
Only if targeting function-count completeness — 42 functions, heavy numerics,
low file yield. Defer behind everything above.

**Phase X-6 — Pivot depth & BI direction _(Long-range)_**
Full pivot refresh (page filters, hidden items, calculated fields), then the
multi-table data model per SPREADSHEET.md §9.

---

## Appendix — corpus feature frequency (top tags)

**docx (248):** custom-xml 47 · tables 47 · headers-footers 46 · images 42 ·
numbering 35 · drawing 32 · vml 32 · bookmarks 30 · lists 30 · hyperlinks 28 ·
sdt 27 · section-breaks 26 · ole 24 · tracked-changes 22 · shading 21 ·
smarttag 18 · fields 15 · comments 14 · chart 12 · merged-cells 12 ·
title-page 12 · multi-column 11 · symbols 11 · wmf-emf 11 · footnotes 9 ·
landscape 8 · textbox 8 · watermark 8 · toc 7 · rtl 6 · endnotes 5.

**xlsx (555):** empty 296 · drawings 224 · charts 167 · number-formats 165 ·
multi-sheet 149 · defined-names 108 · pivot-cache 69 · pivot-tables 69 ·
conditional-formatting 51 · tables 37 · auto-filter 35 · cross-sheet-refs 32 ·
merged-cells 31 · shared-formulas 30 · external-links 20 · frozen-panes 19 ·
data-validation 16 · images 16 · protected 16 · array-formulas 12 · volatile 12
· dynamic-arrays 7. Function families: math 42 · lookup 29 · logical 27 ·
stat 23 · text 16 · info 12 · datetime 8 · financial 4 · dynamic-array 4.
