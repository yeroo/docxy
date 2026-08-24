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
  is a UI-layer gap only. Nothing in `gridcore` needs to learn a new syntax.
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
      qualifier and answers in the same qualified form, and the three 110px
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
      removes — the decision itself is the new pure `preview_range(text, here)`,
      compared case-insensitively as `sheet_index_of` compares
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

- [ ] verify every requirement in the Overview is implemented: `=`, `$` anchors
      and sheet qualifier shown in all eight range fields; a typed qualifier is
      resolved or refused, never ignored
- [ ] verify the out-of-scope list is still out of scope — no Switch
      Row/Column, no header restyle, no marching ants, no chart-source outlines
- [ ] verify edge cases: sheet names needing quotes, an apostrophe in a name, a
      one-cell range, a reversed range (`D5:A1`), an unknown sheet, and the
      concatenation `A1:B5A1:D5`
- [ ] run `cargo test --manifest-path suite/Cargo.toml` — all suite tests pass
- [ ] run `cargo test -p gridcore` — all gridcore tests pass
- [ ] run `cargo build --all-targets` at the repo root — the root workspace
      (xlsxy, gridwasm, lookxy, TUI docxy) still builds
- [ ] run `cargo clippy -p gridcore --all-targets -- -D warnings` and
      `cargo fmt --check` — all issues fixed

### Task 9: [Final] Update documentation

- [ ] document the reference syntax the fields accept, and which targets take a
      foreign sheet, wherever the suite's spreadsheet behaviour is already
      described
- [ ] if a new pattern was established (the `RefText` split, `ref_a1` vs
      `range_a1`), record it so the next range field added follows it rather
      than re-inventing a third form

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
