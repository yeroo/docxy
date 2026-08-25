# Selection you can see and trust

## Overview

Four related complaints about what the grid shows you is selected, reported
side by side against Excel. They are one plan because they are one question —
*what is selected right now, and what does it read from?* — and fixing them
apart would produce four unrelated answers.

1. **A pointed range is a flat wash.** Excel draws a dashed border around it.
   docxy fills it with `range_tint` (`BRAND` at alpha 0.14, main.rs:16179) and
   draws no edge at all.
2. **A selected chart says nothing about its sources.** Excel outlines the cells
   a chart reads, each slot in its own colour. docxy has the machinery —
   `ref_color`, `ref_index_at`, `GridOverlay::formula_refs` — but it fires only
   while a formula is being typed.
3. **A chart and a cell are selected at once.** Selecting a chart leaves the
   cell ring where it was, so two things claim to be selected and neither is
   clearly the one the keyboard will act on.
4. **The Chart panel vanishes the moment you click a cell.** It is gated
   directly on `chart_sel.is_some()` (main.rs:15889), so any click on the grid
   closes it mid-edit.

### What it should look like

- A pointed range gets a **dashed border in docxy's brand teal** (`0x2AA79B`),
  at Excel's border width. Excel's own green is deliberately not copied — the
  shape is Excel's, the colour is ours.
- Selecting a chart outlines each source area in **Excel's own mapping**: values
  blue, categories purple, series names green. This is the one place matching
  Excel beats matching docxy, because the mapping is what a user already knows.
- **Selecting a chart clears the cell selection**, and **clicking a cell
  deselects the chart**. One selection at a time.
- **The panel is sticky**: clicking a cell drops the chart's handles but leaves
  the panel open on that chart, so a range edit survives a click on the grid. It
  closes on its `×`, or swaps when another chart is selected. No timer — nothing
  disappears while you are reaching for it.

### Key benefits

- What is selected is legible at a glance, and only one thing ever is.
- A chart's inputs are visible without hunting through the panel's fields.
- Editing a chart's ranges stops being interrupted by the panel closing.

## Context (from discovery)

### GPUI cannot draw a dashed border

Established before writing this plan, not assumed:

- `grep -c dash` in `gpui/src/style.rs` is **0**. There is no dashed border
  style, and `div()` exposes no dash option.
- `gpui::linear_gradient(angle, from, to)` exists (`color.rs:865`) but takes
  **two stops**, so it cannot express a repeating dash pattern either.

So a dash has to be a discrete element, and "how many elements does a wide
selection cost" is the central design question of Task 2 rather than a detail.
The plan does not pre-decide it; it requires a bound and a fallback.

⚠️ **The relevant hard-won lesson**: overlay geometry that reconstructs row
positions from `logical_scroll_top` × a uniform row height **drifts**, because
row heights are content-driven. The exact technique that works in this codebase
is **per-cell rendering plus `deferred()`**, which escapes the cell's
`overflow_hidden` clip and paints above everything. `ListState::bounds_for_item`
and `col_at_x` give exact window coordinates where a range's pixel bounds are
genuinely needed (`cell_at` already uses both). Whichever approach Task 2
takes, it must not reintroduce reconstructed geometry.

### Files and components

- `suite/docxy/src/main.rs` — `sheet_row` and `GridOverlay` (:16139) where the
  wash is drawn; `sheet_col_header`; `ref_color`/`ref_index_at`; `chart_sel` and
  its assignment sites (:3437, :3500, :3546, :7430, :7819, :7830); the panel
  gate (:15889); `chart_panel` (:7066).
- `BRAND` is `0x2AA79B` (main.rs:1164).

### What already exists and should be reused

- **`GridOverlay`** already threads per-frame grid state into `sheet_row`
  (`fill_preview`, `range_preview`, `picking`, `formula_refs`,
  `handle_hidden`). The chart's source areas belong there too, not in a new
  parallel channel.
- **`ref_index_at`** already answers "which reference owns this cell, smallest
  first" for the formula colouring. The chart's slots need the same question
  asked of a different list.
- **`Escape` already clears `chart_sel`** (main.rs:7819) and the panel's `×`
  clears it (main.rs:7430). Both become "close the panel too" under the sticky
  rule, and are the two places that must still fully dismiss.

### Dependencies

None outside the repo. Both workspaces build clean; `gridcore` has 370 tests
and the suite crate 82.

## Development Approach

- **Testing approach**: Regular — code first, tests in the same task, before
  that task closes.
- Complete each task fully before moving to the next.
- **CRITICAL: every task MUST include new/updated tests** for code changes in
  that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new and modified functions
  - cover both success and error scenarios
- **CRITICAL: all tests must pass before starting the next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**

⚠️ **This plan is mostly rendering, which gpui `#[test]` cannot exercise.**
Constructing views or elements blows up the render macro. So every decision must
be extracted into a **pure free function** that takes plain data and returns
plain data — which cells are on a range's boundary, which edges each owns, how
many dashes fit, which source slot owns a cell, whether the panel should be
open. Those are what the tests cover. A task whose logic is unreachable from a
unit test has been written wrong; extract the decision and test that.

### Build and test commands

Two separate cargo workspaces. A green `suite/` says nothing about the root one.

```bash
cargo build  --manifest-path suite/Cargo.toml
cargo test   --manifest-path suite/Cargo.toml
cargo build --all-targets
cargo test  -p gridcore
cargo clippy -p gridcore --all-targets -- -D warnings
cargo fmt --check
```

## Testing Strategy

- **Unit tests**: required per task, over the pure decision functions above.
- **E2E tests**: none here. On-screen appearance is verified manually against an
  installer build and belongs in Post-Completion — it is the only way to judge
  whether the dashes actually *look* like Excel's.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Keep plan in sync with actual work done

## Implementation Steps

### Task 1: Decide how a dashed edge is drawn, and bound its cost

- [ ] confirm from the gpui source that no dashed border or repeating gradient
      exists, and record the citation — every later task depends on it
- [ ] choose between per-cell edge segments (each boundary cell renders the
      dashes along the edges it owns) and one `deferred` overlay sized from
      `bounds_for_item`/`col_at_x`, and write the reasoning into this plan.
      Weigh it against the drift lesson above, which favours per-cell
- [ ] establish the cost bound: dashes per cell edge at the app's column widths,
      worst-case element count for a full-width selection, and the cap beyond
      which the border falls back to solid
- [ ] write the pure geometry as free functions — given a range and the visible
      window, which cells are on the boundary and which edges each owns; given
      an edge length and a dash pitch, how many dashes and their offsets
- [ ] write tests for the boundary function: a one-cell range (all four edges on
      one cell), a single row, a single column, a range partly scrolled out of
      view, and a range wider than the cap
- [ ] write tests for the dash-fitting function, including an edge shorter than
      one dash pitch
- [ ] ⚠️ no rendering change lands in this task — it is the decision and its
      arithmetic. If the cost bound turns out unacceptable at realistic widths,
      STOP and record that here before writing Task 2
- [ ] run tests in both workspaces — must pass before Task 2

### Task 2: Draw the pointed range with a dashed brand border

- [ ] render the dashed border for `GridOverlay::range_preview` using Task 1's
      geometry, in `BRAND` (`0x2AA79B`) at Excel's border width
- [ ] decide and record whether the existing `range_tint` wash stays under the
      dashes or is replaced by them — Excel shows the border alone, and two
      indicators for one thing is what this plan is trying to stop
- [ ] apply the cap from Task 1: past it, draw a solid border rather than
      thousands of elements
- [ ] keep `handle_hidden` behaviour intact — the auto-fill handle is still
      hidden while a range field is pointable
- [ ] write tests for the pure part: given a preview range and a viewport, the
      list of edge segments to draw, and that the capped case yields solid
- [ ] run tests — must pass before Task 3

### Task 3: A selected chart outlines the cells it reads

- [ ] add the chart's source areas to `GridOverlay` — one entry per slot with
      its role (values / categories / name), derived from the selected chart's
      `values_ref`, `categories_ref`, `name_ref` and `point_refs`
- [ ] colour by role in **Excel's mapping**: values blue, categories purple,
      names green. Define them as named constants beside `ref_color` with a
      comment saying they are deliberately Excel's and not the `ref_color`
      palette, so a later reader does not "unify" them
- [ ] resolve overlap the way `ref_index_at` already does — smallest area wins,
      earliest on a tie — so a name cell inside the values box still reads as a
      name
- [ ] draw only the areas on the sheet in front of you: a ref naming another
      sheet gets nothing, exactly as `preview_range` already refuses the wash
- [ ] write tests for the pure part: given a `ChartData` and the active sheet
      name, the areas and their roles; the overlap rule; and the foreign-sheet
      case
- [ ] run tests — must pass before Task 4

### Task 4: One selection at a time

- [ ] selecting a chart clears the cell selection, so the cell ring does not
      compete with the chart's handles
- [ ] clicking a cell deselects the chart (drops its handles and its source
      outlines) and selects that cell
- [ ] preserve what already works: `Escape` and the panel's `×` still fully
      dismiss, and a drag that starts on a chart's resize grip is still a resize
      rather than a cell selection
- [ ] check the range-picking path is unaffected — while a range field has the
      keyboard the grid is in point mode, and a click there points rather than
      selecting, which must not now also deselect the chart being edited
- [ ] write tests for the pure decision: given a click target and the current
      selection state, what is selected afterwards — covering chart→cell,
      cell→chart, chart→same chart, and a click while point mode is active
- [ ] run tests — must pass before Task 5

### Task 5: The Chart panel is sticky

- [ ] replace the panel's gate (main.rs:15889) with state that outlives
      `chart_sel`: the panel shows the last chart selected and stays open after
      deselection
- [ ] close it on the `×` (main.rs:7430) and on `Escape`, and swap it when a
      different chart is selected
- [ ] decide and record what the panel does when the chart it shows is deleted,
      or its sheet is left — it must not display a chart that no longer exists
- [ ] make sure a range field inside a sticky panel still points at the grid:
      that is the whole reason for stickiness, so a field focused while the
      chart is deselected must still work
- [ ] write tests for the pure decision: given chart selection events, deletes,
      sheet switches and dismissals, whether the panel is open and which chart
      it shows
- [ ] run tests — must pass before Task 6

### Task 6: Verify acceptance criteria

- [ ] verify each of the four numbered complaints in the Overview is addressed
- [ ] verify the deliberate non-goals are still absent: no animation on the
      dashes, no Excel green, no header-darkening work, no chart axis work
- [ ] verify edge cases: a one-cell range, a range scrolled out of view, a chart
      whose refs name another sheet, a chart deleted while its panel is sticky
- [ ] run `cargo test --manifest-path suite/Cargo.toml` — all pass
- [ ] run `cargo test -p gridcore` — all pass
- [ ] run `cargo build --all-targets` — root workspace builds
- [ ] run clippy on both workspaces and `cargo fmt --check` — clean

### Task 7: [Final] Update documentation

- [ ] document the selection rules in `suite/docs/range-selector.md`: one
      selection at a time, the sticky panel, and what each source colour means
- [ ] record the dashed-border technique and its cost bound, so the next person
      wanting a dashed anything does not re-derive that gpui has no such style

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Colours

| What | Colour | Why |
|---|---|---|
| Pointed range border | `BRAND` `0x2AA79B` | docxy's teal, Excel's shape |
| Chart values | blue | Excel's mapping, deliberately |
| Chart categories | purple | " |
| Chart series names | green | " |

The chart-source colours are **not** `ref_color`'s palette. They mean specific
roles rather than "the Nth reference", and a user coming from Excel already
knows them.

### Deliberately out of scope

- Animation on the dashes. The reporter said explicitly it is not needed.
- Header darkening across the affected range, and Excel's grey header style.
- The chart card's missing value axis, gridlines, colliding category labels and
  thin bars.
- Resolved series-name and category-label lists in the panel.

## Post-Completion

*Manual verification — the appearance cannot be judged by tests*

- Point a range and compare the dashes against Excel side by side: pitch,
  thickness and how they read against gridlines. The colour is deliberately
  different; the *shape* should not be.
- Select a chart and confirm each source area is outlined in the right colour,
  and that nothing is outlined for a chart reading another sheet.
- Click between a chart and cells and confirm exactly one thing looks selected
  at any moment.
- Edit a chart's DATA RANGE, click a cell mid-edit, and confirm the panel
  survives and the field still points.
- Check a wide selection (a full row) for the cost bound: it must stay
  responsive, falling back to a solid border rather than stuttering.
