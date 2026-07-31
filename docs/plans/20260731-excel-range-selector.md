# Excel-style range selector

## Overview

One range selector, used everywhere a range is asked for.

Today exactly one input in the suite can point at cells: the Chart panel's data-range
field, built ad hoc over the last few commits. Everything else that needs a range —
chart series, conditional formatting, data validation, sort, and formula editing —
either takes the selection implicitly or can't take a range at all. Typing `=SUM(`
and then clicking a cell commits the edit and moves the selection instead of writing
the reference in, which is the opposite of what a spreadsheet is supposed to do.

This plan extracts the pointing behaviour into one primitive and gives it three
consumers:

- **A shared range field.** Focus, select-all, caret, drag-select inside the text,
  point mode on the grid, and the wash + outline of the referenced cells — one
  implementation, reused.
- **A chart series UI.** Excel's Select Data Source: a list of series, each with its
  own name and values reference, plus the category-axis labels reference; add,
  remove, reorder, and edit each by pointing at the grid.
- **Formula range selection.** With the caret after `=SUM(`, clicking or dragging
  cells writes the reference in, and every range the formula mentions is outlined on
  the grid in its own colour.

Benefits: a spreadsheet that behaves like one when you build a formula; charts whose
series can be set up rather than inferred from one rectangle; and a single place
where reference editing is fixed or improved, instead of four.

## Context (from discovery)

Files/components involved:

- `suite/docxy/src/main.rs` — the whole suite UI. Relevant regions: `ChartField` /
  `ChartFieldEdit` (the field state to be generalised), `chart_field_row` /
  `chart_field_segment` (the selectable text runs), `chart_field_key`,
  `range_field_active` / `range_pick_to` / `range_pick_end` (point mode),
  `select_cell` / `extend_to` / `sheet_drag_over` (the three grid entry points),
  `GridOverlay` (what `sheet_row` needs to know), `sheet_row`'s per-cell edge
  rendering, `chart_panel`, `sheet_key`, and the cell-edit path
  (`sheet_begin_edit`, `edit_insert`, `sheet_commit`, `fx_edit_row`).
- `gridcore/src/sheet.rs` — `ChartData`, `ChartSeries`, `ChartSource`,
  `chart_from_range`, `parse_range_name`, `cell_name`.
- `gridcore/src/drawing.rs` — `parse_chart` (reads `<c:f>` refs), `rewrite_anchors`.
- `gridcore/src/xlsx.rs` — `chart_space_xml` (writes per-series refs + caches),
  the save path that regenerates edited chart parts.
- `gridcore/src/formula.rs` — `parse`, `translate_formula`, the AST used to find the
  ranges a formula mentions.

Related patterns found:

- **Enum-keyed UI state** is the house style: `SheetPick`, `PickKind`, `ChartField`,
  `FindField`. The chosen approach follows it.
- **Self-managed text input** — no `gpui-component` `InputState` anywhere; every bar
  and field owns a `String` buffer and receives keys through `sheet_key`. The
  primitive keeps that.
- **Per-cell edge rendering with `deferred`** — the range outline and the fill handle
  are drawn by the cells themselves because the chart overlay reconstructs row
  positions from a uniform row height and drifts on content-tall rows. Formula ref
  outlines must use the same technique.
- **Pure functions for testable logic** — `range_text`, `buf_insert`, `char_to_byte`,
  `last_visible_col`, `chart_from_range`. gpui's test harness cannot render this
  crate, so anything worth testing gets extracted as a free function.

Dependencies identified:

- The virtualized `list` swallows child `on_mouse_down`, so cells only see `on_click`
  and `on_mouse_move`. That is why a drag anchors on the first cell the pointer
  *moves into* rather than the one it pressed on — a papercut for selection, but a
  correctness problem for pointing, so it is a task here.
- `gridcore` types are built as literals by `xlsxy`, `gridwasm` and the TUI, in the
  ROOT workspace; the suite is a SEPARATE workspace. Any change to `ChartSeries` /
  `ChartData` breaks them, and only `cargo build --all-targets` at the root notices.
- Chart parts round-trip verbatim unless `ChartData::edited` is set; per-series refs
  must keep that contract.

## Development Approach

- **Testing approach**: Regular — code first, tests immediately after, within the
  same task.
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy

- **Unit tests**: required for every task. The suite crate compiles plain `#[test]`
  fine for pure logic (`cargo test --manifest-path suite/Cargo.toml`); anything that
  constructs a view or returns an element blows up gpui's render macro, so logic that
  deserves a test gets extracted as a free function first.
- **Engine tests**: `cargo test -p gridcore` for the model, parser and serializer work.
- **Cross-workspace build**: any task touching a `gridcore` public type must run
  `cargo build --all-targets` at the root AND `cargo build --manifest-path
  suite/Cargo.toml`. A green suite says nothing about `xlsxy`/`gridwasm`/TUI.
- **Screenshot verification**: the GPUI input and render wiring can only be checked by
  driving the real binary. Harness lives in the session scratchpad
  (`pointmode.ps1`/`deselect.ps1` pattern): launch `suite/target/debug/suite.exe`,
  assert the window owns the foreground before sending any input, drive Win32
  mouse/keys, screenshot, crop. **The foreground guard is mandatory** — without it a
  stray drag lands in whatever window the user is working in.
- **E2E**: `suite/docxy/tests/ui_e2e.ps1` covers the docx editor; extend it only if a
  task changes something it already asserts.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code, tests, docs in this repo
- **Post-Completion** (no checkboxes): manual checks and anything outside the repo

## Implementation Steps

### Task 1: Generalise the field state behind a target enum
- [x] add `RefTarget` and `fn RefTarget::is_range(self) -> bool` in `suite/docxy/src/main.rs`
  - ➕ variants land with their consumers instead of up front: `ChartRange`/`ChartTitle` now, series refs in Task 5, the other bars in Task 10. Adding unconstructed variants early only buys dead-code warnings.
- [x] rename `ChartFieldEdit` → `RangeEdit` and `ChartField` → `RefTarget`, keeping `buf`/`caret`/`anchor`/`dragging` and the selection helpers
- [x] point `Docxy::chart_field` at the new type as `range_edit`, updating `range_edit_key` (was `chart_field_key`), `ref_field_row`/`ref_field_segment`, `chart_panel`, `select_cell`, `extend_to`, `sheet_drag_over`, `range_field_active`
- [x] make `range_field_active` mean "the focused target is a range" via `is_range`
  - ⚠️ the plan claimed a focused Title put the grid in point mode today. It did not — the old check was `f.which == ChartField::Range`, which already excluded Title. `is_range` keeps that correct as variants are added; there was no bug to fix.
- [x] write tests for `is_range` per variant and for the selection helpers (`selection`, `delete_selection`, `set_caret` with/without extend)
- [x] run `cargo test --manifest-path suite/Cargo.toml` - 11 passed

### Task 2: One reusable field renderer and commit dispatch
- [x] extract `ref_field(id, target, value, hint, help, pal, cx)` as a method on `Docxy` (it reads `range_edit`/`ref_msg` itself, which is cleaner than passing them), returning the focused (selectable runs + caret) or idle (select-all on focus click) form
- [x] extract `fn ref_commit(&mut self, target: RefTarget, text: &str, cx)` with one arm per target, replacing the `match f.target` inside the Enter handler
- [x] move the inline validity message onto the field: `chart_msg` became `ref_msg: Option<(RefTarget, bool, String)>`, so any field renders its own message and falls back to its `help` line
- [x] rebuild the Chart panel's two fields on `ref_field` with no behaviour change
- [x] write tests for the commit dispatch on a non-UI seam
  - ➕ the seam turned out to be reference PARSING: extracted `parse_ref_text` (trim, drop a `Sheet!` prefix, `$` ignored) which both the field commit and the live outline now share, and which formula pointing will need in Task 7. `ref_commit` itself needs `&mut Docxy` + `Context`, so it can't be unit-tested.
- [x] run tests and screenshot the Chart panel to confirm it looks and behaves as before - 12 passed, panel unchanged

### Task 3: Anchor a pick on the cell that was pressed
- [x] add `fn cell_at(&self, pos: Point<Pixels>) -> Option<(u32, u32)>`: columns via the new pure `col_at_x`, rows from `ListState::bounds_for_item` + `viewport_bounds` (exact, unlike the overlay's uniform-row arithmetic)
  - ➕ needed `SheetView::row_at_list_index`, the inverse of `row_list_index`, because hidden rows collapse out of the list
  - ⚠️ returns `None` on sheets with frozen ROWS: those render outside the list, so its bounds can't locate a press there. Those sheets keep the old first-moved-cell behaviour; the fallback is in `sheet_drag_over`.
- [x] handle `on_mouse_down` on the grid container to plant the anchor at `cell_at(pos)`, since the virtualized list never delivers mouse-down to cells
- [x] make `sheet_drag_over`/`range_pick_to` extend from that anchor instead of adopting the first moved-into cell
  - ➕ this fixed ORDINARY drag-select too, not just pointing — `drag_anchor` feeds `select_cell`/`extend_to` the pressed cell
- [x] write tests for the column half of `cell_at` (pure: x → column, gutter, frozen band, boundary belongs to the column it opens)
- [x] write tests for `range_text` covering an anchor below/right of the target (already covered by the existing test — no change needed)
- [x] run tests, then screenshot a fast drag from A1 to D5 - 13 passed; the drag that previously produced A2:D5 now produces A1:D5 in point mode, and A1:C4 for plain drag-select

### Task 4: Per-series references in the chart model
- [x] give `ChartSeries` its own `values_ref: Option<ChartSource>` and `name_ref: Option<String>`, and `ChartData` a `categories_ref: Option<ChartSource>`, keeping `source` as the box the panel shows
  - ⚠️ named `values_ref`, not `values`: `ChartSeries::values` is already the cached numbers
  - ➕ added `ChartSource::to_ref()` — a per-series range names itself, unlike `f_ref` which derives a column out of the chart's box and skips a header row
- [x] fill them in `chart_from_range` (each numeric column becomes that series' values ref; the label column becomes `categories_ref`)
- [x] read them in `parse_chart`: the `<c:f>` inside each `<c:ser>`'s `<c:val>`/`<c:tx>` belongs to that series, the one in `<c:cat>` to the categories, and their union stays the chart's box
- [x] write per-series refs in `chart_space_xml` from the series' own ranges, falling back to the derived form when absent
- [x] write tests in `gridcore` for parse → model → serialize → parse round-tripping two series with different value ranges, plus the derived fallback
- [x] run `cargo test -p gridcore` (273 passed), then `cargo build --all-targets` at the root and the suite build — both green (the fixtures broken by the last model change already use `..Default::default()`)

### Task 5: Series list in the Chart panel
- [x] render the series as a list: a card per series with its name, values range and colour swatch row
  - ➕ `ref_field_dyn` alongside `ref_field`: repeated fields need ids built per series, not `&'static str`
  - ➕ `range_a1` — a range as the text a field shows, tested to round-trip through `parse_ref_text`
- [x] make each series' name and values editable through `ref_field` (`SeriesName(i)` / `SeriesValues(i)`)
  - a name field takes a ref OR literal text, as Excel's does: a ref reads the cell and keeps `name_ref`, anything else is the name
- [x] apply an edited series range by re-reading those cells into that series only, leaving the others alone
- [x] show the category-axis labels range as its own `Categories` field
- [x] write tests for the "re-read one series" operation — `gridcore::sheet::range_numbers`/`range_labels` (pure), plus target identity and `range_a1` round-trip in the suite
- [x] run tests and screenshot the panel with a three-series chart - 14 suite + 274 gridcore passed
  - ⚠️ FOUND AND FIXED a real bug: `range_pick_end` still committed every pick through `chart_apply_range`, so pointing a SERIES field replotted the whole chart from those cells. It now commits through `ref_commit(target, …)`. Verified: pointing Qty at B2:B4 reports "3 points" and leaves Unit price, Total and the chart's own range alone.

### Task 6: Add, remove and reorder series
- [ ] add an "Add series" action that appends a series pointed at an empty range and focuses its values field
- [ ] add per-series remove, keeping at least one series (a chart with none is not renderable)
- [ ] add move-up/move-down, reordering both the model and the drawn order
- [ ] mark the chart edited so a save regenerates its part, and keep colours attached to their series across reorders
- [ ] write tests for add/remove/reorder on the series vector (pure), including the last-series guard
- [ ] run tests, then screenshot add → point at a range → remove - must pass before task 7

### Task 7: Reference tokens in a formula buffer
- [ ] add pure helpers: `ref_token_at(buf, caret) -> Option<Range<usize>>` (the A1/A1:D5 token the caret sits in or immediately after) and `replace_ref(buf, caret, text) -> (String, usize)`
- [ ] decide insert-vs-replace: after an operator, `(` or `,` insert; inside or right after a reference, replace it
- [ ] add `refs_in(buf) -> Vec<(u32,u32,u32,u32)>` over `gridcore::formula::parse`, ignoring text that doesn't parse yet (a half-typed formula is the normal case)
- [ ] write tests for `ref_token_at` (caret before/inside/after a ref, no ref, multiple refs)
- [ ] write tests for `replace_ref` (insert after `(`, replace an existing ref, caret lands after the written ref) and for `refs_in` (nested calls, ranges and single cells, unparseable input → empty)
- [ ] run tests - must pass before task 8

### Task 8: Point mode while editing a formula
- [ ] route grid clicks and drags to `replace_ref` when the active cell edit holds a formula, instead of committing the edit and moving the selection
- [ ] keep pointing out of the way when the buffer is not a formula: a click still commits and moves, as it does today
- [ ] leave the cell edit intact through the drag, so Enter/Escape still commit/cancel the formula
- [ ] make the fx bar and the in-cell editor agree — both read the same buffer, so both must show the written reference
- [ ] write tests for the decision (buffer + caret + picked range → new buffer + caret), covering "not a formula" and "replace the ref under the caret"
- [ ] run tests, then screenshot typing `=SUM(` and dragging A2:A5 - must pass before task 9

### Task 9: Colour the ranges a formula mentions
- [ ] thread `refs_in(buf)` for the live edit buffer into `GridOverlay` alongside the existing range preview
- [ ] give each ref an index-keyed colour from a small palette and draw its border with the per-cell edge technique (`deferred`, exact placement)
- [ ] tint the referenced cells lightly in the same colour, keeping the picked-range wash distinguishable
- [ ] show the same colour on the reference inside the formula text (fx bar and in-cell), so text and grid agree
- [ ] write tests for the colour assignment (stable per ref index, wraps past the palette length)
- [ ] run tests, then screenshot `=B2*C2+SUM(D2:D5)` mid-edit showing three coloured ranges - must pass before task 10

### Task 10: Retrofit the other range inputs
- [ ] add `CondFormat`, `Validation`, `Sort` and `TextToColumns` targets, each seeded from the current selection when its bar opens
- [ ] give those bars a `ref_field` for their range, so they can be pointed at cells instead of only using the selection
- [ ] apply through `ref_commit`, keeping each bar's existing behaviour when the range is left alone
- [ ] write tests for seeding (selection → field text) and for commit routing per target
- [ ] run tests, then screenshot conditional formatting applied to a pointed range - must pass before task 11

### Task 11: Verify acceptance criteria
- [ ] verify every requirement in Overview is implemented: shared field, series UI, formula pointing, retrofit
- [ ] verify edge cases: an invalid range explains itself; a chart with one series can't lose it; pointing across a hidden row/column; a formula that never parses still edits normally
- [ ] verify a chart whose series were edited round-trips: save, reopen from disk, series and category refs intact
- [ ] run the full unit suite (`cargo test` at the root and `cargo test --manifest-path suite/Cargo.toml`)
- [ ] run `cargo build --all-targets` at the root and the suite build
- [ ] run `cargo clippy --all-targets` (root and suite) - all issues fixed

### Task 12: [Final] Update documentation
- [ ] document the range selector in the suite's docs: how pointing works, which inputs accept it, and the reference syntax supported (same-sheet `A1:D5`)
- [ ] update README.md if the feature list mentions charting or formula editing
- [ ] record in the project knowledge doc the two traps this work depends on: the list swallowing mouse-down, and per-cell edge rendering being the only exact way to outline a range

## Technical Details

**State.** One slot on `Docxy`:

```rust
struct RangeEdit { target: RefTarget, buf: String, caret: usize, anchor: usize, dragging: bool }
enum RefTarget { ChartRange, ChartTitle, SeriesName(usize), SeriesValues(usize), Categories,
                 CondFormat, Validation, Sort, TextToColumns }
```

`RefTarget::is_range()` gates point mode and the outline, so a focused Title behaves
like plain text. Formula pointing does NOT use this slot — the cell edit already owns
its buffer; it shares the *pointing* service and the outline renderer.

**Pointing.** `cell_at(pos)` maps a pointer position to a cell: columns by summing
`col_px(col_width(c))` from `col0` (exact), rows via `ListState::bounds_for_item`
(exact). A grid-level `on_mouse_down` plants the anchor — the virtualized list never
delivers mouse-down to cells, which is why drags currently anchor on the first cell
the pointer moves into.

**Reference syntax.** Same-sheet `A1:D5` only, `$` accepted and ignored (this is what
`parse_range_name` already does). Cross-sheet and multi-area refs still work in
formulas — they just can't be built by pointing, and the outline skips them.

**Chart model.** `ChartSeries` gains its own `values`/`name_ref`; `ChartData` gains
`categories_ref`. `chart_space_xml` writes each series' own `<c:f>`, so Excel sees
exactly what the panel shows. Chart parts stay verbatim unless `edited` is set.

**Outlines.** Drawn by the edge cells themselves (absolute child, `deferred` to escape
the cell's overflow clip). The chart overlay's uniform-row arithmetic is NOT usable —
it drifts on rows whose height comes from their content, which is already visible on
the sample sheet.

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- Point at a range in a workbook with frozen panes and hidden rows; check the outline
  lands on the right cells in both.
- Build a formula against a chart's source data and confirm the chart's own outline
  and the formula's ref colours don't fight each other visually.
- Open a workbook whose chart series were edited here in real Excel: series names,
  values and category labels should all be live references, not literals.

**External system updates**:
- `xlsxy` (TUI) shares `gridcore`: after the per-series model change, decide whether
  its chart insert should set per-series refs too, or keep deriving them.
- The comshims (`xlcomshim`) expose gridcore workbooks over COM; per-series refs may
  be worth surfacing there if a client asks for chart data.
