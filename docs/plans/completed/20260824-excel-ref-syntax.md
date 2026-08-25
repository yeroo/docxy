# Excel reference syntax in the suite's range fields

## Overview

Every range field in the suite spreadsheet shows and accepts references the way
Excel writes them: a leading `=`, `$` anchors, and a sheet qualifier —
`=Budget!$A$1:$D$5` rather than today's bare `A1:D5`.

The problem this solves came out of putting docxy's Chart panel beside Excel's
Select Data Source dialog on the same workbook. Excel's `Chart data range` reads
`=Budget!$A$1:$D$4`; docxy's DATA RANGE reads `A1:D5`. That is not only a
cosmetic difference — `parse_ref_text` (suite/docxy/src/main.rs:1519) *accepts* a
`Sheet!` prefix and then throws it away:

```rust
let cells = t.rsplit_once('!').map(|(_, r)| r).unwrap_or(t);
```

So typing `Budget!A1:D5` while looking at Sheet2 silently plots Sheet2's A1:D5.
Discarding a qualifier the user typed is worse than refusing it.

After this plan a reference names a sheet, that sheet is looked up in the
workbook, and its cells are what gets read. A reference to a sheet that isn't
there is refused with a message instead of being silently redirected.

**Deliberately out of scope** (each is a candidate for its own plan, and none is
a prerequisite for this one):

- Point-across-sheets. Mouse-pointing keeps picking on the sheet in front of
  you; it just writes that sheet's qualifier into the field. Clicking a sheet
  tab while a field has focus behaves as it does today.
- Switch Row/Column on charts (`chart_from_range` only ever builds series from
  columns).
- Header darkening across the affected range, marching-ants selection, resolved
  series/category label lists, and chart-selected source outlines.

### Key benefits

- A qualifier the user types is honoured or refused, never ignored.
- One reference style across the whole app, matching what Excel shows for the
  same data, so a ref can be copied between the two.
- A chart can finally read from a sheet other than the one it floats over.

## Context (from discovery)

### Files and components involved

- `suite/docxy/src/main.rs` — the whole suite UI. The range-field primitive
  (`ref_field`, `RangeEdit`, `RefTarget`), the parse/format helpers, the chart
  panel, the four sheet entry bars, and the grid overlay all live here.
- `gridcore/src/sheet.rs` — `ChartSource`, `quote_sheet_name`,
  `parse_range_name`, `cell_name`, `col_name`.
- `gridcore/src/formula.rs` — the formula parser, for reference only; no change
  expected.

### What already exists and should be reused

- **`ChartSource::f_ref` (gridcore/src/sheet.rs:363) already emits exactly the
  target form** — `Budget!$A$1:$D$5`, with sheet-name quoting handled by
  `prefix()` → `quote_sheet_name` (already `pub`, sheet.rs:332). The model has
  always carried `sheet: String`. The display half of this plan is largely
  "route through what the writer already does" rather than new formatting.
- **The formula engine already parses `Sheet!A1`** — `Parser::sheet_ref`
  (gridcore/src/formula.rs:850), reached from formula.rs:795 and :807. Cross-sheet
  is a UI-layer gap only as far as SYNTAX goes: nothing in `gridcore` needs to
  learn a new one. ⚠️ It did need to learn the new POLICY — see the ➕ task
  below; `parse_chart` decides which sheet a chart's box names, and the panel's
  `rebuild_source` has to fold the same refs in the same order or a chart reads
  one way before a save and another after it.
- **Sheet-by-name lookup has precedent** in this file —
  `v.pkg.workbook.sheets.iter().position(|s| s.name == sn)` at main.rs:4972 and
  main.rs:5018.
- **Refusing a foreign sheet is already written** for the entry bars, in the
  helper at main.rs:1809-1830: `"{named}" is another sheet; this acts on {sheet}`.
  That path stays; it grows a sibling that resolves instead of refusing.
- **`ref_msg: Option<(RefTarget, bool, String)>`** already drives the inline
  red/brand message under a field. Every new refusal reports through it — no new
  error surface.

### Callers that must move together

`parse_ref_text` has four non-test callers. Changing its return type is the
spine of this plan, so they are listed here rather than rediscovered per task:

| Site | Function | What it does with the result |
|---|---|---|
| main.rs:1510 | `series_name_commit` | `NameCommit::Ref(range)` — decides a typed series name is a reference, not a literal |
| main.rs:1534 | `chart_range_of` | validates and applies the cell cap (`MAX_CHART_CELLS`) |
| main.rs:1826 | the entry-bar range helper | already rejects a foreign sheet by name |
| main.rs:3516 | `range_preview` | the wash drawn over the pointed cells |

### Dependencies

None outside the repo. Both workspaces are already building; `gridcore` has 329
tests and the suite crate 40.

## Development Approach

- **Testing approach**: Regular — code first, tests in the same task, before
  that task closes.
- Complete each task fully before moving to the next.
- Make small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for code changes in
  that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behaviour changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting the next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

### Build and test commands — read this before Task 1

This repository holds **two separate cargo workspaces**. A green suite build
says nothing about the root workspace, and this has broken CI before: growing
`ChartData`/`ChartSeries` for the previous plan compiled fine in `suite/` and
broke `xlsxy`, `gridwasm` and the TUI `docxy`, which build struct literals of
those types.

```bash
# the suite (GPUI desktop app) — its own workspace
cargo build  --manifest-path suite/Cargo.toml
cargo test   --manifest-path suite/Cargo.toml

# the root workspace: gridcore, docxcore, xlsxy, gridwasm, lookxy, TUI docxy
cargo build --all-targets
cargo test  -p gridcore

# before any task closes, both of the above must be clean, plus:
cargo clippy -p gridcore --all-targets -- -D warnings
cargo fmt --check
```

Test placement follows what is already there: pure free functions are tested in
the `#[cfg(test)]` module at the bottom of `suite/docxy/src/main.rs` (gpui
`#[test]` works for pure logic; constructing views or elements blows up the
render macro, so **keep every new helper a pure free function**). Model-level
behaviour is tested in `gridcore/src/sheet.rs`.

## Testing Strategy

- **Unit tests**: required for every task (see Development Approach above).
- **E2E tests**: this project has no browser-based e2e harness. The GPUI app is
  exercised by the pure-logic unit tests above; on-screen verification is a
  manual step and belongs in Post-Completion, not in a task checkbox.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): tasks achievable within this
  codebase — code changes, tests, documentation updates
- **Post-Completion** (no checkboxes): items requiring external action — manual
  on-screen testing, installer builds, verification against real Excel

## Implementation Steps

### Task 1: Parse a reference into its sheet and its cells

- [x] add `struct RefText { sheet: Option<String>, range: (u32, u32, u32, u32) }`
      near `parse_ref_text` in `suite/docxy/src/main.rs`, deriving
      `Clone, Debug, PartialEq, Eq`
- [x] change `parse_ref_text` (main.rs:1519) to return `Option<RefText>`: strip a
      leading `=`, split the sheet qualifier on the **last** `!`, unquote a
      `'...'`-wrapped name (turning a doubled `''` back into one `'`), and parse
      the remainder with `gridcore::sheet::parse_range_name` as today
- [x] rewrite the doc comment — it currently states the prefix is "accepted and
      dropped", which is the behaviour being removed
- [x] update the four callers to compile against the new type without changing
      their behaviour yet (main.rs:1510, :1534, :1826, :3516) — resolution
      arrives in Task 4, so for now each takes `.range` and ignores `.sheet`
- [x] write tests: `=Budget!$A$1:$D$5`, `Budget!A1:D5`, `'My Sheet'!A1:D5`,
      `'Bob''s Data'!A1`, and the bare forms `A1:D5` / `  a1:d5 ` / `C3` all
      parse, with `sheet` set only where one was written
- [x] write tests for the rejections that must survive: `A1:B5A1:D5` (the
      concatenation a field used to produce), `""`, `total`, `A0`, and a lone
      `Budget!` with no cells
- [x] run `cargo test --manifest-path suite/Cargo.toml` and `cargo build --all-targets` — must pass before Task 2

### Task 2: Format a reference the way Excel writes it

- [x] add `fn ref_a1(sheet: Option<&str>, range: (u32, u32, u32, u32)) -> String`
      producing `=Budget!$A$1:$D$5`, or `=$A$1:$D$5` when `sheet` is `None`,
      quoting through `gridcore::sheet::quote_sheet_name`
- [x] keep `range_a1` (main.rs:1798) as-is for the places that want the bare
      form — the name box and the drag-in-progress readout — and note in its doc
      comment which form belongs where, so the two don't get confused later
- [x] write a round-trip test: for a table of ranges and sheet names (including
      `My Sheet` and `Bob's Data`), `parse_ref_text(&ref_a1(s, r))` returns
      exactly that sheet and range
- [x] write tests for the shapes: a one-cell range renders `$C$3:$C$3`, `None`
      renders no `!`, and a name needing no quotes gets none
- [x] run tests — must pass before Task 3

### Task 3: Every range field shows the Excel form

- [x] chart panel DATA RANGE: replace the hand-built `format!("{}:{}", …)` in
      `range_shown` (main.rs, in the chart-panel renderer) with `ref_a1` over
      `source.sheet` and `source.range` — via the new `source_ref_text`, which
      maps an empty sheet name to `ref_a1`'s `None`
- [x] series VALUES and NAME fields: render through `ref_a1` with the series'
      `values_ref` / `name_ref` sheet — NAME goes through the new
      `series_name_shown`, and `series_apply_name` now measures "unchanged"
      against that same text (the field shows the ref, the literal name only
      when there is no ref)
- [x] CATEGORY LABELS field: same, from `categories_ref`
- [x] the four entry bars (`CondFormat`, `Validation`, `Sort`, `TextToColumns`):
      seed and re-render their field through `ref_a1` with the active sheet's
      name; `bar_range_text` strips the seed's leading `=` before reading the
      qualifier and answers in the same qualified form, and all four 110px
      field boxes grew to 160px to fit a sheet name
- [x] update `range_text` (main.rs:1847) so the text written while dragging
      carries the active sheet's qualifier, matching what the field will hold
      when the drag ends — ⚠️ deviation: `range_text` is SHARED with
      `formula_pick_to`, where a pick writes into a cell's formula and must stay
      bare. So `range_text` is unchanged and a sibling `ref_pick_text(sheet,
      anchor, to)` was added for the field path; `range_a1`'s doc records which
      is which
- [x] update the field placeholders (`"e.g. A1:D5"`, `"e.g. B2:B5"`,
      `"e.g. A2:A5"`) to the qualified form, so the hint matches what the field
      produces — and the matching `chart_range_of` examples, so the "isn't a
      range" message quotes the same shape
- [x] write tests for the seeding and drag helpers — a drag from an anchor
      renders `=Sheet1!$B$2:$B$5`, and that text parses back to the same sheet
      and range
- [x] run tests — must pass before Task 4

### Task 4: Resolve a named sheet to a sheet index

- [x] add `fn ref_sheet_index(&self, sheet: Option<&str>) -> Result<usize, String>`
      on `Docxy`: `None` resolves to the active sheet index; a name matches a
      workbook sheet case-insensitively (Excel is case-insensitive here) and
      returns its index
- [x] return `Err` naming the sheet that isn't there — the message goes straight
      into `ref_msg`, so word it for the user, e.g.
      `there's no sheet called "Budget"`
- [x] write tests for the pure part: factor the lookup itself into a free
      function over a `&[String]` of sheet names so it is testable without
      building a view, and test exact match, case-insensitive match, a name with
      spaces, an unknown name, and `None`
- [x] write a test that a duplicate-cased name (`budget` and `Budget` both
      present, which Excel forbids but a hand-built file can contain) resolves to
      the first rather than panicking
- [x] run tests — must pass before Task 5

### Task 5: Chart fields read from the sheet their reference names

- [x] `series_apply_values` (main.rs:2847): resolve the ref's sheet via
      `ref_sheet_index` instead of the hardcoded `self.active_sheet()`, and read
      the numbers from that sheet; report an unknown sheet through `ref_msg`
      against `RefTarget::SeriesValues(i)` — via the new `chart_ref`, which is
      `chart_range_of` renamed to `chart_ref_of` and grown a sheet lookup, so
      "isn't a range", "too many cells" and "no such sheet" all arrive as one
      `Err` the caller drops straight into `ref_msg`
- [x] `series_apply_name`: same, reading the label through
      `gridcore::sheet::range_labels` on the resolved sheet — `NameCommit::Ref`
      now carries the whole `RefText` rather than just its cells, and the sheet
      is resolved through `ref_sheet_index`
- [x] `categories_apply`: same, against `RefTarget::Categories`
- [x] the DATA RANGE commit path: same, against `RefTarget::ChartRange`
- [x] make sure the `ChartSource` written back carries the **resolved** sheet's
      name, not the active sheet's — this is what makes the ref persist as a
      real `<c:f>` pointing at the other sheet (`chart_from_range` is handed the
      resolved sheet and stamps its name on every ref it builds)
- [x] keep the existing guards intact: the one-column rule for a series
      (main.rs:2871) and the `MAX_CHART_CELLS` cap (main.rs:1529) — the cap is
      weighed BEFORE the sheet lookup, so a range too big to plot reports its
      size rather than a missing sheet the user would then fix twice
- [x] ➕ delete `ref_elsewhere` / `ref_block_elsewhere`: the stopgap that refused
      to commit a slot already reading another sheet, added in Task 3 when the
      fields began SHOWING a qualifier the commit paths still ignored. Resolution
      replaces it — the four paths now honour the qualifier instead of refusing
      it, which is the whole point of this plan
- [x] write tests for the pure decision — given a ref's sheet, the workbook's
      sheet names, and the active index, which sheet index is read and what
      message (if any) is produced
- [x] write tests for the error cases: unknown sheet, and a foreign sheet
      combined with a too-large range (the cell cap must still fire)
- [x] run tests — must pass before Task 6

### Task 6: Data validation accepts a foreign sheet; sort and text-to-columns refuse one

- [x] `Validation`: allow a qualifier naming another sheet — resolving through
      the new `bar_ref_text`, which answers with the sheet index the rule lands
      on and the ref spelled the workbook's way, and persisting that qualified
      ref in `bar_range`. The rule is written to that sheet via the new
      `bar_sheet_index` (derived from `bar_range`, not stored beside it)
      — ⚠️ deviation from the stated rationale: the Validation bar's range field
      is the APPLIES-TO range, not the list source (the bar's own text is a
      literal comma-separated list, `add_data_validation(…, "list", …)`). So what
      a qualifier buys is building the rule where the boxes are while looking at
      the sheet holding the list — the same cross-sheet case, from the other end.
      A range-valued list source is separate work and stays out of scope
- [x] `Sort` and `TextToColumns`: keep the existing refusal — `bar_ref_text`
      routes them straight back through `bar_range_text`, message unchanged;
      tested that a foreign sheet is refused even when it EXISTS (the objection
      is where they act, not an unknown name), and `no_bar_refuses_its_own_seeded_value`
      covers the seeding regression for all four bars on every sheet
- [x] `CondFormat`: refuses, same as Sort — it paints the cells in front of you.
      Recorded with the rest of the per-target reasoning in the doc comment on
      `target_takes_foreign_sheet`
- [x] write tests per target: `each_range_target_says_whether_it_reads_another_sheet`
      pins the policy for all nine `RefTarget` variants; the bar tests cover a
      foreign sheet resolved (Validation) and refused (the other three), an
      unknown sheet, a name needing quotes, a non-range, and an active-sheet ref
      accepted for every bar
- [x] run tests — must pass before Task 7

### Task 7: The wash and the pointing agree about which sheet you're on

- [x] `range_preview` (main.rs:3507): return `None` when the parsed ref names a
      sheet other than the active one — drawing the wash over the visible sheet's
      A1:D5 for a ref that means Budget's A1:D5 is exactly the lie this plan
      removes — the decision itself is the new pure function — ⚠️ deviation:
      it is `preview_range(text, names, active)`, not `preview_range(text,
      here)`. It RESOLVES the qualifier through `sheet_index_of` and asks
      whether the sheet found is the active one, rather than comparing the
      qualifier against the active sheet's NAME. On a workbook holding both
      `Budget` and `budget` the two answers differ, and the resolving one is the
      sheet a commit will act on — which is the whole point of the plan.
      `the_wash_resolves_a_qualifier_the_way_a_commit_does` pins it
- [x] `range_pick_end`: write the picked range through `ref_a1` with the active
      sheet's name, so a mouse pick replaces a foreign qualifier with the sheet
      actually picked from — already so since Task 3: `range_pick_to` REPLACES
      the buffer with `ref_pick_text(active_sheet, …)`, and `range_pick_end`
      commits that text, so no change was needed beyond the test that pins it
- [x] check `GridOverlay.picking` still behaves: a field holding a foreign ref
      has no wash, but the grid must still be in point mode so a drag can
      re-point it — `picking` reads `range_field_active` (focus), never the
      preview, so it does; the comment claiming the two coincide was corrected
- [x] write tests for the preview decision as a pure function — same sheet →
      `Some(range)`, other sheet → `None`, no qualifier → `Some(range)`
- [x] write a test that a pick over an existing foreign ref yields text naming
      the active sheet
- [x] run tests — must pass before Task 8

### Task 8: Verify acceptance criteria

- [x] verify every requirement in the Overview is implemented: `=`, `$` anchors
      and sheet qualifier shown in all eight range fields; a typed qualifier is
      resolved or refused, never ignored — the four chart fields render through
      `source_ref_text`/`series_name_shown` (main.rs:6071, :6164, :6188, :6346)
      and the four entry bars share one renderer seeding through `ref_a1`
      (main.rs:12217); every commit path resolves via `sheet_index_of`, and the
      `let cells = t.rsplit_once('!')…` line that dropped a qualifier is gone
- [x] verify the out-of-scope list is still out of scope — no Switch
      Row/Column, no header restyle, no marching ants, no chart-source outlines;
      its only added `ref_color`/`ref_index_at` mention is a test-module `use`.
      ⚠️ deviation: the diff is NOT `suite/docxy/src/main.rs` plus this file. It
      also touches `gridcore/src/drawing.rs` and `gridcore/src/sheet.rs` (the
      ➕ task below), `suite/docs/range-selector.md`,
      `suite/docs/chart-orientation.md`, `scripts/revmux-review.sh` and
      `docs/plans/20260824-pie-series-drop.md`
- [x] verify edge cases: sheet names needing quotes, an apostrophe in a name, a
      one-cell range, a reversed range (`D5:A1`), an unknown sheet, and the
      concatenation `A1:B5A1:D5` — each is pinned by a test:
      `parse_ref_text_accepts_what_a_range_field_is_typed` (`'My Sheet'!`,
      `'Bob''s Data'!A1`, `C3`, `D5:A1`),
      `parse_ref_text_refuses_what_isnt_a_range` (`A1:B5A1:D5`, `""`, `total`,
      `A0`, `Budget!`), `ref_a1_writes_the_form_excel_shows` (`$C$3:$C$3` and
      the quoting), and `sheet_index_of_refuses_a_sheet_that_isnt_there`
- [x] run `cargo test --manifest-path suite/Cargo.toml` — all suite tests pass
      (60 passed, 0 failed)
- [x] run `cargo test -p gridcore` — all gridcore tests pass (329 + 1 + 4)
- [x] run `cargo build --all-targets` at the repo root — the root workspace
      (xlsxy, gridwasm, lookxy, TUI docxy) still builds — clean
- [x] run `cargo clippy -p gridcore --all-targets -- -D warnings` and
      `cargo fmt --check` — both clean, nothing to fix

### ➕ Task 8b: The loader had to learn the same policy (out of the original scope)

Discovered during review of Task 5's `rebuild_source`. Not in the Overview and
not in the "Files and components involved" list, which named `gridcore` as
reference-only — recorded here because the diff is real and behavioural, and
because `gridcore` is the SHARED crate: `xlsxy`, `gridwasm`, `lookxy` and the
TUI `docxy` all read charts through `parse_chart`, so a green `suite/` build
says nothing about them.

- [x] `gridcore/src/drawing.rs` — `fold_source`: the fold `parse_chart` shares
      with the panel, comparing sheet names case-insensitively as
      `sheet_index_of` does, so `Budget!$B$2` and `budget!$B$3` are one sheet
- [x] `parse_chart` holds its `<c:cat>` (mode 2) and `<c:tx>` (mode 1) refs back
      in `cat_boxes`/`name_boxes` and folds them AFTER the loop — values, then
      categories, then names. Folded in document order (`{tx}{cat}{val}`) a
      single cross-sheet label ref seeded the box and every local `<c:val>`
      after it was skipped for the sheet mismatch, collapsing a chart plotting
      `A1:D5` onto one foreign cell. The panel's `rebuild_source` folds the same
      four slot kinds in the same order, which is what makes a chart read the
      same before and after a save
- [x] `infer_by_row` compares the SHEET as well as the coordinates, so two
      single cells that merely line up across sheets are not read as a stack
- [x] `cat_col` is assigned outright rather than inherited from whichever
      `<c:f>` seeded the box: from `<c:cat>` for a column chart, and from the
      first series' NAME cell for a row chart, whose labels run along a row and
      so name no column
- [x] `gridcore/src/sheet.rs` — `ChartSeries::point_refs`, holding a
      scatter's/bubble's `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` refs. Those
      kinds carry no `<c:val>`, so `rebuild_source` was blind to their numbers
      and rebuilt their box from label cells alone
- [x] tests: `a_cross_sheet_label_ref_does_not_take_the_box_from_the_cells_plotted`,
      `single_cells_on_different_sheets_are_not_stacked`,
      `a_scatters_point_refs_are_kept_on_its_series`,
      `the_source_box_takes_its_label_column_from_the_category_ref`, and the two
      row-chart round-trip guards, mirrored in the suite by
      `a_charts_box_is_rebuilt_from_the_references_its_slots_hold`
- [x] documented in `suite/docs/chart-orientation.md` and
      `suite/docs/range-selector.md`

### ➕ Task 8c: `point_refs` had to obey the rules every other `ChartSource` does

Raised by the review round after 8b landed, and recorded here because 8b's
checklist closed without them: the new slot was folded by the panel but was the
one `ChartSource` nothing re-based, and nothing cleared.

- [x] `gridcore/src/edit.rs` — `rename_sheet_in_chart` and `shift_chart_refs`
      walk `ser.point_refs` too. A scatter's points are the only refs its series
      has, and `rebuild_source` folds them FIRST, so a stale one seeded the box
      with the old sheet and got every correctly-renamed slot after it skipped
      for the mismatch — DATA RANGE naming a sheet the workbook no longer has.
      A wholly-deleted range drops its point ref rather than dangling, the same
      rule `values_ref` follows. This is what SPREADSHEET.md's "structural edits
      re-base **every** `ChartSource`" always claimed
- [x] `suite/docxy/src/main.rs` — `chart_take_kind` (extracted out of
      `chart_set_kind`, and testable) clears `point_refs` when the picked kind
      is one `chart_kind_is_writable` accepts: the writer's bar/column/line/pie
      arms emit no `<c:xVal>`, so carried across a conversion they left
      `rebuild_source` stretching the box back over the obsolete X column the
      next time any field committed. NOT the writer's `<c:cat>` fallback, which
      reads `data.source`'s `cat_col` and never these — the first draft of this
      comment claimed it did. A scatter that stays a scatter keeps them — its
      part round-trips verbatim, so the next `parse_chart` reads those very
      refs back
- [x] `chart_set_kind` AUTHORS AFRESH rather than relabels, when the chart it is
      converting has nothing the writer could plot (`chart_has_plottable_values`
      — no `values_ref`, no `col`, no cached numbers, which is every imported
      scatter and bubble). Relabelling one `column` made
      `chart_is_writable` true, and the next save then overwrote its part with
      `<c:val><c:numLit><c:ptCount val="0"/>` per series: a 50-point scatter
      destroyed in one click, the outcome `chart_kind_is_writable`'s own comment
      calls irreversible. `chart_reauthored` runs the box back through
      `chart_from_range` — the same call DATA RANGE and Insert make — keeping
      the title and the part (the writer only overwrites a part it knows) and
      refusing with the chart's own shape when the box holds no numbers to
      plot. It also settles the box: a relabelled scatter's only foldable slot
      was its `<c:tx>` name cell, so the next `series_apply_name` /
      `series_delete` / `series_reorder` / `categories_apply` collapsed a box
      over `A1:B4` onto `B1` and DATA RANGE stopped re-deriving. Re-derived,
      every series carries a `values_ref` for `rebuild_source` to fold
- [x] `chart_range_sheet` extracted out of `chart_switched`: resolving the sheet
      a chart's BOX names (not the one on screen), under `chart_ref_of`'s cell
      cap, is now asked by both doors that re-derive a chart from its own box,
      with the same three sentences when it can't be
- [x] `fold_source` is `pub` and the suite's `rebuild_source` CALLS it rather
      than keeping a byte-for-byte copy. The two deciding a cross-sheet ref
      identically is what makes a chart read the same before and after a save,
      which is not an invariant to hand-maintain in two crates
- [x] Switch Row/Column moved BELOW the not-writable note: the flip carries
      `complex` and `part`, so on a stacked or scatter chart it shows and isn't
      saved — exactly what the note's "edits below" says, and above it the
      button was the one control the wording excluded
- [x] the delete-shrinks-the-box claim narrowed, in the `series_delete` comment
      and in `range-selector.md`: the box is a rectangle, so it only shrinks off
      a column at its ENDS. A middle delete leaves it as wide, and the rebuild's
      real justification is that the panel must show the box `parse_chart` will
      rebuild on the next open
- [x] the "every by-name lookup folds case" claim narrowed in
      `range-selector.md` and in `sheet_index_of`'s own doc comment:
      `sheet_follow_hyperlink` and `dv_list_values` still match byte for byte.
      The load-bearing half — resolution and the wash (`bar_range_text`) must
      not disagree — stands
- [x] `SPREADSHEET.md` and `chart_kind_is_writable`'s doc comment no longer say
      `parse_chart` doesn't read `<c:xVal>`/`<c:yVal>`. It reads their REFS (the
      box, and `point_refs`); what it never reads is their cached numbers, which
      is the reason the kind isn't writable
- [x] tests: `a_rename_follows_a_scatters_point_refs`,
      `a_row_insert_moves_a_scatters_point_refs`,
      `deleting_a_scatters_x_cells_drops_that_point_ref_instead_of_dangling`,
      `a_scatter_on_another_sheet_keeps_its_point_refs`,
      `picking_a_writable_type_clears_a_scatters_point_refs`,
      `picking_a_writable_type_authors_a_valueless_scatter_afresh`, and a
      middle-delete case in
      `a_charts_box_is_rebuilt_from_the_references_its_slots_hold`

### ➕ Task 8d: the re-author door asked its question of the wrong thing

Raised by the review round after 8c landed. The door 8c opened in
`chart_set_kind` was right about WHAT to do and wrong about WHEN and about what
else moves when it does.

- [x] the gate is asked per SERIES, not per chart:
      `chart_has_plottable_values` (chart-wide `any`) became
      `chart_would_lose_points` — does any series hold `point_refs` the writer
      could not emit (`values_ref`, `col` and `values` all absent). The `any`
      let a HALF re-pointed scatter relabel: ser0 given a `values_ref` through
      SERIES VALUES made the whole chart look plottable, and the save then
      wrote `<c:val><c:numLit><c:ptCount val="0"/>` over ser1 — the series
      nobody touched, lost with no `complex` to hold the part back. Asking
      about points-to-lose rather than values-to-emit is also what keeps
      `series_add`'s empty new series (no refs, and no `values` until the chart
      has categories) from forcing a re-derivation that would discard the hand
      edits on every other series
- [x] `chart_set_kind` clears `range_edit` / `ref_msg` / `range_pick` on the
      branch that took `chart_reauthored`'s output. A re-derivation REPLACES the
      series (a scatter's one X/Y pair comes back as a series per numeric
      column), and both are keyed by bare series index, so the panel's own
      "type below, then pick a type above" left an open field to commit its
      buffer onto whichever series inherited the number — or to go on taking
      keystrokes while no longer drawn, when the count shrank. The same three
      lines `chart_press` clears for that hazard, and the two of them
      `chart_switch_orientation` and `series_delete` clear: `range_pick` is the
      one of the three not keyed by series index, so those two leave it to
      `range_pick_end`, which returns early once `range_edit` is `None`
- [x] the comments that said this door "does not re-derive" — in
      `chart_set_kind`, in `chart_kind_series_err`'s list of the five doors to a
      multi-series pie, and in `chart-orientation.md` — now say that it usually
      doesn't, and that it therefore asks `chart_kind_series_err` TWICE: once on
      the count on screen, once on the count `chart_reauthored` reads out of the
      box, whose refusal can name a count the panel is not showing
- [x] `chart_take_kind`'s doc, `chart_kind_is_writable`'s doc,
      `chart-orientation.md` and `SPREADSHEET.md` no longer state the guarantee
      per series while the gate was per chart. With the gate per series the
      claim is simply true, and each says so in one place rather than three
- [x] tests: the mixed case (`chart_would_lose_points` on a two-series scatter
      with only one re-pointed) and the `series_add` case (an empty series is
      not a loss) in
      `picking_a_writable_type_clears_a_scatters_point_refs`. Clearing the
      panel's per-series state is view code, which the render macro forbids
      constructing in a `#[test]`, so it is covered by the comment naming the
      two siblings that do the same thing rather than by an assertion

### ➕ Task 8e: what the re-author door hands `chart_from_range`

Raised by the review round after 8d landed. 8c/8d settled WHEN the door
re-derives; this settles what it re-derives FROM, and what the panel says about
it.

- [x] `chart_box_with_header`: the box an imported scatter arrives with sits ON
      its points. `parse_chart` folds it out of the `<c:xVal>`/`<c:yVal>` refs
      and only then lets `<c:cat>`/`<c:tx>` stretch it, so a series whose name
      came as a literal `<c:v>Speed` (which is how Excel writes a typed one, and
      what the `an_edited_scatter_chart_is_kept_verbatim_rather_than_flattened`
      fixture has) leaves nothing to stretch it upward and the box is `A2:B4`
      where an authored chart's would be `A1:B4`. `chart_from_range` reads every
      box the other way — `chart_from_columns` names the series from row `r0`
      and plots `r0 + 1..=r1` — so re-deriving that one unchanged ate row 2 as
      headings: a three-point scatter came back TWO points named `1` and `10`,
      the numbers it consumed, and the next save wrote that over the part. The
      box is now widened one line when any series' `point_refs`/`values_ref`
      reaches its leading edge, and refused (naming the edge) when there is no
      line to widen into. Switch Row/Column re-derives from the same box and so
      asks the same question, for the orientation the FLIP is about to read it
      as — only for a chart whose box came out of point refs at all
      (`chart_box_from_points`, asked of the refs rather than of what a relabel
      would cost, so a half re-pointed scatter is still one), since every other
      box is one the user set in DATA RANGE and widening it would break the
      double flip. Widening happens after `chart_range_sheet`'s cell cap, so
      both doors re-count (`chart_cells_within_cap`)
- [x] `infer_by_row` reads `point_refs` shapes alongside `values_ref`. It had
      only `<c:val>` to measure, which a scatter has none of, so EVERY scatter
      answered "column" — no longer just the panel's reading, since 8c made
      `by_row` decide how the re-derivation reads the box. A scatter laid out
      along rows (`$B$2:$F$2` / `$B$3:$F$3`) came back as five one-point series,
      and then as a pie refused on a count of five. Only multi-cell point refs
      vote; a one-point scatter is two single cells side by side and must not
      reach the stacked-cell fallback
- [x] `ChartSeries::points_unheld`, and `chart_would_lose_points` asking it as
      well as `point_refs`. That vec is filled only when the loader could parse
      an `<c:f>` out of the point elements — a `<c:numLit>` scatter has no
      `<c:f>` to fail on, and one naming `Sheet1!$A:$A` fails `parse_f_ref` under
      a `mode` of 0, so the `unparsed_ref` arm never sees it either. Both
      arrived with all four slots empty, indistinguishable from the empty series
      `series_add` pushes, walked past 8d's gate and were relabelled — the
      silent `<c:ptCount val="0"/>` this whole change exists to prevent, and the
      one shape it had left open. Marked at the CLOSE of each point element (the
      only place that knows the element yielded nothing), cleared by
      `chart_take_kind` with the refs beside it. Per ELEMENT, so a series with
      one readable half carries the mark AND a ref — and only when the element
      held points at all, so a schema-legal empty one
      (`<c:numLit><c:ptCount val="0"/>`) is left alone rather than sent down the
      re-author door to be refused for a box it never needed
- [x] the not-writable note says what picking a type does to the edits below it,
      because for these charts it does not save them: a re-read replaces the
      series wholesale, keeping only the title. Which of the two a click takes
      turns on per-series state the panel doesn't draw, so the note names the
      re-read outright and `chart_set_kind` sets a status line saying it
      happened, with how many series and the kind the chart HAD (a bubble chart
      reaches the same branch, so the sentence cannot say "scatter"). The
      "Switch Row/Column sits below the note" comment now cites the note's
      second half too — the flip is the one edit below it that DOES survive the
      click, because it re-derives there and then and so leaves the points-only
      class entirely, not because the later re-read honours `data.by_row`
- [x] the note asks the click's own question by MAKING the call (`reread` =
      `chart_range_sheet` + `chart_reauthored("column", …)`, the way the switch
      button already derives its flip every frame) rather than by the proxy
      `data.source.is_some()`. That proxy was unsound in both directions: it
      promised a re-read for every refusal `chart_reauthored` can return with
      the box still in place (a plot half the box misses, a points-leading box
      with no line above it, a widened box past the cap, a box with no line of
      numbers, a box naming a missing sheet), and with no box at all it fell
      back to the bare first half — which claims the edits get saved once a type
      is picked, on the one chart where picking a type can only say
      `CHART_NO_BOX`. So there are three sentences now, not two, and the third
      names the remedy the five refusals share (point DATA RANGE at cells).
      `"column"` answers for all four buttons: the only kind-dependent refusal
      is `chart_kind_series_err`, which is a pie-only count
- [x] `chart_reauthored` refuses a series that NAMES point cells the fold took
      none of (`chart_points_off_box`, asked with the box in hand). Two shapes
      reach it, and both leave the box provably short of the plot:
      - an `<c:f>` `parse_f_ref` refused (`<c:xVal>` naming a whole column beside
        a held `<c:yVal>`). The marks are per point ELEMENT, so one series
        carries both, and `rebuild_source` folds only the half it holds
      - a HELD ref naming ANOTHER SHEET than the box. `fold_source` SKIPS it
        rather than unioning across sheets, and nothing else records the skip —
        `unparsed_ref` is only set for `mode != 0`, never under a point element —
        so the sheets are compared at the door. `chart_from_range` reads ONE
        sheet, so re-deriving would drop the foreign half outright

      Either way the re-derivation plots one coordinate and drops the other,
      which the status line's "the series below are the range's" does not say.
      LITERAL points are deliberately let through: they are in no cells at all,
      so no box could have covered them and the chart's own is the best that
      exists. That exemption is per ELEMENT, which is why the loader keeps
      `ChartSeries::points_ref_unheld` beside the wider `points_unheld` — asking
      the wider mark here would refuse an ordinary bubble whose
      `<c:bubbleSize>` is a `<c:numLit>` beside held X/Y refs, a chart whose box
      covers every cell its plot names

      Both doors that re-derive from the box ask it: `chart_reauthored`, where
      the loss reaches the FILE (the re-derived chart is a writable kind and the
      next save regenerates the part), and `chart_switch_row_column`, where it
      reaches the screen and then the file two clicks later — the flip leaves
      every series carrying a `values_ref`, so the type click after it is a plain
      relabel with the missing half already gone
- [x] the stale "`parse_chart` never reads `<c:xVal>`/`<c:yVal>`" in
      `an_edited_scatter_chart_is_kept_verbatim_rather_than_flattened` and in
      `range-selector.md`'s "Only four chart kinds can be written back" — the
      two sites 8c's sweep missed, both now saying what the other three say (the
      refs are read, the numbers are not). `SPREADSHEET.md`'s `complex` sentence
      says which slots it covers, now that the points have a mark of their own
- [x] tests: `re_authoring_a_scatter_whose_box_sits_on_its_points_keeps_every_point`
      (widen / no room / by-row / already-headed). The by-row case drives the
      whole of `chart_reauthored`, not the widening helper alone: both halves of
      the orientation fix are on that one path — the box widens LEFT and
      `chart_from_range` reads a series per numeric ROW — and pinning only the
      helper leaves a `false` in either call green,
      `switching_a_scatter_whose_box_sits_on_its_points_does_not_eat_a_line`
      (widen sideways / no room / a `<c:val>` chart's box left alone),
      `a_scatter_whose_points_cannot_be_held_says_so_on_the_series`,
      `a_row_laid_scatter_is_inferred_by_row_from_its_point_refs`, and the
      unheld/cleared cases in
      `picking_a_writable_type_clears_a_scatters_point_refs`,
      `re_authoring_refuses_a_scatter_whose_box_covers_only_the_held_half`
      (a refused ref refused / a held ref on another sheet refused, case-folded
      / literal points still go through, beside a held ref or not / a re-point
      puts the series beyond the question)

### Task 9: [Final] Update documentation

- [x] document the reference syntax the fields accept, and which targets take a
      foreign sheet, wherever the suite's spreadsheet behaviour is already
      described — `suite/docs/range-selector.md` is where that already lives, so
      its "Reference syntax" section was rewritten: the qualified/anchored form
      every field now shows, the permissive input grammar (optional `=`, `$`,
      qualifier and quoting; split on the LAST `!`; either corner first), the
      case-insensitive `sheet_index_of` resolution and its refusal, and a
      per-target table of resolved-vs-refused with the reason for each. The
      stale paragraph claiming chart fields drop the prefix and refuse another
      sheet (`ref_elsewhere`, now deleted) is replaced by what resolution
      actually does, including that the written-back `ChartSource` carries the
      RESOLVED sheet's name. `SPREADSHEET.md`'s pointer to that doc gained a
      two-line summary so the chart section isn't left describing the old form
- [x] if a new pattern was established (the `RefText` split, `ref_a1` vs
      `range_a1`), record it so the next range field added follows it rather
      than re-inventing a third form — new "Which form belongs where" section:
      a table of the four spellings and who writes each, the rule that a new
      range field uses `ref_a1`, why `ref_a1` goes through `ChartSource::to_ref`
      rather than re-implementing the quoting, why `range_text` stays bare
      beside `ref_pick_text`, and why `parse_ref_text` returns sheet and cells
      separately so no caller can drop half the answer. The Testing section's
      covered list grew the new helpers plus the reason they're free functions
      over `&[String]` rather than methods on the view

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### The parse result

```rust
/// A reference as a field holds it: the sheet it names, if it named one, and
/// the cell box. `None` means the sheet in front of you — a bare `A1:D5`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefText {
    sheet: Option<String>,
    range: (u32, u32, u32, u32),
}
```

Splitting on the **last** `!` is deliberate and matches the existing code: a
quoted sheet name can itself contain `!`, and the cells never can.

### The forms in play, and where each belongs

| Form | Example | Used by |
|---|---|---|
| Qualified, anchored | `=Budget!$A$1:$D$5` | every range field; `ref_a1` |
| Anchored, no sheet | `=$A$1:$D$5` | `ref_a1` when `sheet` is `None` |
| Bare A1 | `A1:D5` | the name box, the drag readout; `range_a1` |
| `<c:f>` ref | `Budget!$A$1:$D$5` | the OOXML writer; `ChartSource::f_ref` |

`ref_a1` is the qualified form *plus* the leading `=`; `f_ref` is the same
without it. Keep `f_ref` as the single source of the quoting rules rather than
duplicating `quote_sheet_name` handling in the UI.

### Accepted on input

Input is deliberately more permissive than output, as Excel's is: a leading `=`
is optional, `$` anchors are optional and ignored, the sheet qualifier is
optional, quoting is optional when the name doesn't need it, and either corner
may come first (`D5:A1` names the same box as `A1:D5`).

### Sheet resolution

Case-insensitive, because Excel treats sheet names that way — `budget!A1` finds
the `Budget` sheet. A name that matches nothing is an error carried to the user
through `ref_msg`, never a silent fall back to the active sheet: silently
redirecting a qualifier is the specific bug this plan exists to remove.

### Per-target policy

| Target | Foreign sheet |
|---|---|
| `ChartRange`, `SeriesValues`, `SeriesName`, `Categories` | resolved |
| `Validation` | resolved — a list commonly lives on a lookup sheet |
| `CondFormat`, `Sort`, `TextToColumns` | refused, with the existing message |
| `ChartTitle` | not a range; unaffected |

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes,
informational only*

**Manual verification**:

- Open `sample.xlsx` in the suite, select the chart, and confirm DATA RANGE
  reads `=Budget!$A$1:$D$5` rather than `A1:D5`.
- Type `Budget!A1:D5` into a series VALUES field while looking at a different
  sheet; confirm it plots Budget's cells and that no wash is drawn over the
  visible sheet.
- Type a sheet name that doesn't exist; confirm the red message names it.
- Drag-pick a range while a field holds a foreign qualifier; confirm the field
  ends up naming the sheet actually picked from.
- Save, reopen in real Excel, and confirm the chart's Select Data Source dialog
  shows the same reference the panel showed.

**Build and distribution**:

- Dispatch the release workflow (`gh workflow run release.yml --ref <branch>`)
  and install the resulting `docxy-suite-setup.exe` for on-screen testing — the
  suite's behaviour cannot be checked by the unit tests alone.

**Follow-on work deferred out of this plan** (each wants its own plan):

- Switch Row/Column on charts.
- Point-across-sheets: a field keeping focus while you click a sheet tab.
- Header darkening across the whole affected range, in Excel's grey rather than
  the brand fill.
- Marching-ants styling for a pointed range.
- Resolved series-name and category-label lists in the panel, beside the range
  boxes.
- Chart-selected source outlines, reusing `ref_color` / `ref_index_at`.
