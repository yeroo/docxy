# xlsxy — Spreadsheets in the terminal: architecture & build plan

A terminal editor for Microsoft Excel `.xlsx` workbooks, built on a headless,
near-complete Excel **calculation engine**. Sibling of `docxy` (same repo, same
philosophy: dependency-free cores, thin TUI shells, lossless round-trips).

> **Name / crate / binary:** `xlsxy` (so `cargo install xlsxy` installs the
> `xlsxy` command). The engine lives in the `gridcore` library crate.

Unlike docxy — where the terminal view is deliberately a *reduced* rendering —
xlsxy's ambition is **calculation fidelity**: the goal is near-100% of Excel as
a calculation engine. Formatting is display-level only (number formats,
bold/italic/color); reproducing Excel's visual styling is a non-goal.

---

## 1. Goals / non-goals

**Goals**
- Open, view, edit, create, and save `.xlsx` without corrupting them.
- A **headless-first calculation engine** (`gridcore`): dependency-graph
  recalculation, Excel-compatible semantics, embeddable as a library.
- **Measured conformance**: recalculate real workbooks and diff against Excel's
  own cached values — a scoreboard, not a claim (§8).
- Grid editing UX with Excel muscle memory: formula bar, A1 navigation, range
  selection, fill-down semantics, ref-translating copy/paste.
- Lossless save: everything unmodeled (charts, pivots, conditional formatting,
  print setup…) preserved byte-for-byte.
- Headless CLI: `xlsxy in.xlsx --recalc out.xlsx`, `xlsxy in.xlsx --csv out.csv`.

**Non-goals (at least initially)**
- Full visual formatting *editing* (fonts, fills, borders); v1 renders what the
  file specifies, it doesn't restyle.
- VBA/macros (preserved, never executed).
- Excel's pixel layout, embedded objects, form controls (preserved, not shown).

**Long-range direction** (details in §9): pivot tables as a real aggregation
engine, then a multi-table data model with relationships and measures — the
seed of a BI product with `gridcore` as its core.

---

## 2. Workspace layout

```
opc/        shared OPC/container plumbing: zip, inflate, zipwrite, xml
            (extracted from docxcore; std-only, zero deps)
docxcore/   WordprocessingML engine (unchanged; re-exports opc modules so its
            public API stays stable)
gridcore/   SpreadsheetML engine: model, xlsx I/O, formulas, recalc, numfmt
            (std-only, zero deps beyond opc)
docxy/      document TUI (existing binary)
xlsxy/      spreadsheet TUI + headless CLI (ratatui + arboard, like docxy)
```

Versioning: `docxcore`/`docxy` keep the shared workspace version; `opc`,
`gridcore`, and `xlsxy` version independently (young crates move fast, docxy
releases must not be forced by them).

Why one repo: atomic cross-crate refactors while the shared layer stabilizes,
one CI/release/corpus infrastructure. A future split (e.g. if `gridcore` grows
its own community) is cheap with `git filter-repo`; the reverse is not.

---

## 3. gridcore: the model

```
Workbook
  sheets: Vec<Sheet>
  styles: Styles            # xf table → number format + bold/italic/color
  shared: SharedStrings
  date1904: bool
Sheet
  name: String
  cells: BTreeMap<(row, col), Cell>    # sparse; (0-based row, col)
  col_defs: Vec<ColDef>                # widths etc., preserved + editable width
  row_attrs: BTreeMap<row, RawAttrs>   # heights etc., preserved verbatim
  merges: Vec<Range>                   # rendered read-only, preserved on save
Cell
  value: CellValue           # Empty | Number(f64) | Text | Bool | Error
  formula: Option<Formula>   # source text + parsed AST
  style: u32                 # xf index, preserved
```

Numbers are `f64` (as in Excel); dates are serial numbers plus the workbook's
1900/1904 flag, interpreted at display/function level. Text cells remember
whether they came from a shared string so unedited rich-text cells keep their
original `si` entry.

---

## 4. Load & save — the round-trip strategy

Same strategy that keeps docx files safe, adapted to SpreadsheetML:

**On load:** unzip; parse `xl/workbook.xml` (sheet list, 1904 flag),
`xl/_rels/workbook.xml.rels`, `xl/sharedStrings.xml`, `xl/styles.xml`
(display subset), and each worksheet's `<sheetData>` (+ `<cols>`,
`<mergeCells>`). Keep **every part byte-for-byte**, including the original
worksheet XML sources.

**On save:**
- Regenerate only `<sheetData>` (and `<cols>`/`<dimension>` when touched) and
  **splice** it into the original worksheet XML — sheet-level features we don't
  model (conditional formatting, data validation, drawings, sheet views) ride
  along untouched. This is the spreadsheet analogue of docxy's `sectPr` splice.
- Append new strings to `sharedStrings.xml` (existing entries untouched, so
  unedited rich-text strings survive), update its counts.
- Drop `xl/calcChain.xml` (its content-type override and relationship too) and
  set `<calcPr fullCalcOnLoad="1"/>` — Excel rebuilds the chain and recalcs,
  so a stale chain can never corrupt the file.
- Shared formulas: cells that belonged to a shared-formula group are written as
  ordinary per-cell formulas (expanded via ref translation, §5).
- Write a STORED zip via `opc::zipwrite`, all other parts verbatim.

**Fidelity gate:** load → save → reload is semantically identical; saved files
open cleanly in Excel; a corpus round-trip test enforces it (§8).

---

## 5. The formula language

`gridcore::formula` — lexer → parser → AST → evaluator, plus a **serializer**
(AST → text), because several features are ref rewriting in disguise:

- **Shared formulas** (`<f t="shared">`): expand the master by shifting
  relative refs.
- **Copy/paste**: Excel translates relative refs by the paste offset.
- **Row/column insert/delete** (later): shift every affected ref, `#REF!` the
  deleted ones.

AST covers: numbers, strings, booleans, errors; refs with `$` anchoring and
sheet qualifiers (`Sheet1!A1`, `'My Sheet'!A1`); ranges (incl. whole-row/col
`A:A`, `1:1` — phase B); operators `+ - * / ^ % & = <> < <= > >=`, unary `±`,
range/union/intersection; function calls. The node design leaves room for
`LET`/`LAMBDA` closures (phase C) so dynamic arrays are additive, not a rewrite.

**Value semantics** follow Excel: the eight error values (`#DIV/0!`, `#N/A`,
`#NAME?`, `#NULL!`, `#NUM!`, `#REF!`, `#VALUE!`, `#SPILL!`) propagate through
operators; empty cells coerce to `0`/`""` by context; booleans coerce to 1/0;
text→number coercion in arithmetic; 15-significant-digit display.

---

## 6. The recalculation engine

Dependency-graph recalc **from day one** (this is the spine; a full-recalc
placeholder would be painful to retrofit):

- Parse each formula once; extract its reference set (cells + ranges).
- Maintain forward/reverse dependency edges. Editing a cell dirties its
  dependents transitively; only dirty cells re-evaluate, in topological order.
- **Cycles** → the affected cells evaluate to a cycle error rather than hanging
  (Excel's iterative-calculation opt-in comes in phase B).
- **Graceful degradation, never corruption:** a formula the engine can't parse
  or evaluate (unknown function, unsupported construct) keeps Excel's cached
  value, is never re-evaluated, and is saved byte-faithful. With
  `fullCalcOnLoad` set, Excel recalculates on open. Partial coverage can make
  a value *stale*, never *wrong by our hand*.
- Volatile functions (`NOW`, `TODAY`, `RAND`…) and dynamic dependencies
  (`INDIRECT`, `OFFSET`) are phase B: volatile cells dirty on every recalc;
  dynamic refs report their resolved dependencies back to the graph after each
  evaluation.

Phase A ships ~100 core functions (math, aggregation, logic, text, basic
lookup, date display); the registry is a table so growing toward Excel's ~500
is data entry plus semantics tests, not architecture.

---

## 7. xlsxy: the TUI + headless CLI

- **Grid view:** A/B/C column headers, row-number gutter, column widths from
  the file, frozen formula bar (`A1 ▸ =SUM(B1:B9)`), sheet tabs, status bar
  with Sum/Average/Count for the selection (like Excel's status bar).
- **Editing:** type to replace, `F2` to edit in place, Enter/Tab commit and
  move, Esc cancels; `=` starts a formula; Del clears; undo/redo.
- **Navigation:** arrows, PgUp/PgDn, Ctrl-arrows (data-edge jump), Ctrl-Home,
  mouse click/drag/wheel, click sheet tabs.
- **Clipboard:** ranges copy as TSV to the OS clipboard (arboard, like docxy);
  internal paste translates relative refs Excel-style.
- **Display fidelity (read-only in v1):** number formats (General, dates,
  percent), bold/italic/color resolved from `styles.xml`.
- **Headless:** `--recalc out.xlsx` (load → full recalc → save) and
  `--csv out.csv` — the engine with no terminal, scriptable and CI-testable.
- Cross-suggestion: `docxy book.xlsx` says "try xlsxy"; `xlsxy report.docx`
  says "try docxy".

---

## 8. Testing & the conformance scoreboard

The strategic piece: **conformance is measured, not claimed.**

- **Oracle harness:** every real `.xlsx` stores Excel's computed value cached
  next to each formula. The harness loads a corpus, recalculates with
  `gridcore`, and diffs against those cached values → a scoreboard
  ("N% of M formula cells match") that drives function priorities and catches
  semantic regressions. Starts in phase B; the phase-A fixture tests are its
  embryo.
- **Round-trip goldens:** load → save → reload, semantically identical; saved
  bytes stay a valid OPC package; unmodeled parts byte-identical.
- **Engine unit/property tests:** ref math (A1 ⇄ (row,col), translation),
  coercion tables, per-function semantics incl. error propagation.
- **Manual gate:** saved workbooks must open cleanly in real Excel.

---

## 9. Phased roadmap

- **Phase A — Foundation (this branch)** ✅ *shipped.* `opc` extraction; `gridcore` model +
  lossless xlsx I/O; formula parser/serializer; dependency-graph recalc;
  ~100 functions; `xlsxy` grid TUI; headless `--recalc`/`--csv`; fixtures +
  round-trip tests. *Acceptance:* real workbooks open/edit/save/reopen cleanly
  in Excel; recalc matches cached values on fixtures; docxy untouched.
- **Phase B — Conformance push** 🔄 *in progress: oracle harness (`--verify` + corpus/xlsx CI gate), whole-row/col refs, defined names, INDIRECT/OFFSET, XLOOKUP/*IFS, date/financial/statistical batch, best-effort TEXT. Corpus: 14 oracle workbooks (415 formula cells, 100% match) generated via LibreOffice Calc headless + full round-trip/robustness suite in CI. Remaining: 3D refs, structured table refs, iterative calc, full TEXT()/number-format runtime, real-Excel corpus files.* Corpus oracle harness + scoreboard; function
  coverage to ~300 (date/time, statistical, financial, text); defined names,
  whole-row/col refs, 3D refs, structured table refs; `INDIRECT`/`OFFSET`
  dynamic deps; volatile functions; iterative calculation; `TEXT()` and the
  full number-format runtime.
- **Phase C — Dynamic arrays.** Spill semantics + `#SPILL!`; `FILTER`, `SORT`,
  `UNIQUE`, `SEQUENCE`, `XLOOKUP`; `LET`/`LAMBDA` (closures).
- **Phase D — Pivot engine.** Pivot parts parsed (already preserved from A);
  a **columnar snapshot + group-by/aggregate query layer**, deliberately
  format-independent; pivot refresh/edit in the TUI. The query layer — not the
  XML — is the point: it is the aggregation core everything later builds on.
- **Phase E — Data model.** Multiple tables, relationships, measures over the
  phase-D query core; sources beyond xlsx (CSV first). Headless-first: by this
  point `gridcore` is a small BI engine that happens to have a terminal
  frontend.

Each phase ships independently through the existing signed-release pipeline.

---

## 10. Risks & mitigations

- **Excel semantics are deep** (coercions, date bug, criteria matching,
  floating-point display). Mitigation: the oracle scoreboard makes gaps
  visible and rankable instead of anecdotal.
- **Regenerating `<sheetData>` near unmodeled features** (tables, data
  validation ranges) — mitigated by splicing into the original XML and
  preserving all sibling elements; corpus round-trips guard it.
- **Recalc performance on large sheets** — dirty propagation bounds work to
  the affected subgraph; sparse storage keeps memory proportional to content;
  columnar snapshots (phase D) are the long-term answer for aggregation loads.
- **Two TUIs drift apart** — accepted for now; shared chrome is extracted
  *after* xlsxy exists, when what's actually common is known.
