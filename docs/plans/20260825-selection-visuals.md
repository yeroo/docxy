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

### ⚠️ CORRECTED in Task 1: GPUI *can* draw a dashed border

The claim below was wrong, and Task 1's first checkbox — "confirm from the gpui
source" — is why. It is kept because every later task was written against it.

> ~~`grep -c dash` in `gpui/src/style.rs` is **0**. There is no dashed border
> style, and `div()` exposes no dash option.~~
> ~~`gpui::linear_gradient(angle, from, to)` exists (`color.rs:865`) but takes
> two stops, so it cannot express a repeating dash pattern either.~~

`style.rs` is the wrong file to grep — the setter lives in `styled.rs` and the
enum in `scene.rs`. At the gpui rev this workspace pins (zed `8276687`, per
`suite/Cargo.lock`):

- **`Styled::border_dashed()`** — `crates/gpui/src/styled.rs:500` — sets
  `style.border_style = Some(BorderStyle::Dashed)`. It is on the `Styled`
  trait, so plain `div()` has it.
- **`BorderStyle::{Solid, Dashed}`** — `crates/gpui/src/scene.rs:597`.
- The dashes are drawn **in the quad shader**, on all three backends we ship:
  `gpui_windows/src/shaders.hlsl:664`, `gpui_wgpu/src/shaders.wgsl:693`, and
  `gpui_macos/src/shaders.metal`.
- `PathBuilder::dash_array()` — `crates/gpui/src/path_builder.rs:108` — exists
  too, for stroked paths. Not needed: a quad border is cheaper and lays out
  with the cell.

So **a dash is not an element**. "How many elements does a wide selection cost"
is no longer the central design question — a dashed border costs exactly what a
solid one costs. The bound and the fallback are still recorded below, because
the plan asked for them and they are still the honest answer to "what is the
worst case", but they are a backstop rather than a constraint.

The `linear_gradient` two-stop observation is correct and now moot.

#### The shader's dash geometry (what we get, not what we choose)

From `shaders.hlsl:664-831`, for an unrounded quad, with `W` = border width:

- Pattern is **dash `2W`, gap `1W`** — pitch `3W`. The size is derived from the
  border width; there is no separate dash-length knob.
- Dashes are laid out **per straight side, not around the perimeter**, and the
  side is made to **start and end with a dash** by reserving one dash's length
  and then stretching the gap so the rest divides evenly.
- An edge of **`4W` or less is painted solid** — the shader's `dash_gap > 0.0`
  test fails and it silently skips dashing. At `W = 2` that is 8px.

`dash_fit` (main.rs) mirrors this arithmetic exactly so the numbers below are
derived rather than eyeballed, and so "does this edge dash at all" is a
question a unit test can ask.

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

- [x] confirm from the gpui source that no dashed border or repeating gradient
      exists, and record the citation — every later task depends on it
      → ⚠️ **the premise was false**; gpui draws dashed borders natively. The
      citations and the shader's dash geometry are in Context above.
- [x] choose between per-cell edge segments (each boundary cell renders the
      dashes along the edges it owns) and one `deferred` overlay sized from
      `bounds_for_item`/`col_at_x`, and write the reasoning into this plan.
      Weigh it against the drift lesson above, which favours per-cell
      → **per-cell**; reasoning in "The decision" below
- [x] establish the cost bound: dashes per cell edge at the app's column widths,
      worst-case element count for a full-width selection, and the cap beyond
      which the border falls back to solid → "The cost bound" below
- [x] write the pure geometry as free functions — given a range and the visible
      window, which cells are on the boundary and which edges each owns; given
      an edge length and a dash pitch, how many dashes and their offsets
      → `range_edges_at`, `range_border_plan`, `dash_fit` in
      `suite/docxy/src/main.rs`, beside the existing pure grid geometry
- [x] write tests for the boundary function: a one-cell range (all four edges on
      one cell), a single row, a single column, a range partly scrolled out of
      view, and a range wider than the cap → 6 tests in `grid_geom_tests`
- [x] write tests for the dash-fitting function, including an edge shorter than
      one dash pitch → 2 tests, incl. the `4W` solid threshold from both sides
- [x] ⚠️ no rendering change lands in this task — it is the decision and its
      arithmetic. If the cost bound turns out unacceptable at realistic widths,
      STOP and record that here before writing Task 2
      → nothing rendering-side changed; the cost bound is fine, see below
- [x] run tests in both workspaces — must pass before Task 2
      → suite 91 pass, gridcore 370+1+4 pass, both clippy clean, both fmt clean

#### The decision: per-cell edge segments

Each boundary cell draws the sides of the range's border that it owns, using
gpui's own `border_dashed()`. Reasons, in order of weight:

1. **It cannot drift.** A `deferred` overlay would have to know where row *N*
   is in window coordinates. `bounds_for_item` can answer that, but the moment
   anything reconstructs a row position from `logical_scroll_top` × a uniform
   height it is wrong, because row heights are content-driven. A per-cell
   border is positioned by the cell it is on, so it is right by construction.
2. **The element cost that made the overlay tempting no longer exists.** With
   the dashes in the shader, per-cell costs one quad per boundary cell — the
   same as a solid border would, and the same as the overlay's own quad ×
   perimeter. There is nothing left to trade for the geometry risk.
3. **It reuses `GridOverlay`**, the channel that already carries `range_preview`
   into `sheet_row`, rather than opening a parallel path into the render pass.

The one thing per-cell gives up: because the shader lays dashes out **per
quad**, the dash phase restarts at every column boundary. A long horizontal
edge is therefore a run of per-cell dash groups rather than one continuous
rhythm. Each group starts and ends flush with its cell (the shader reserves a
dash for the far end), so the seam falls exactly on the gridline where the eye
already expects one. Judged acceptable; it is on the Post-Completion list to
confirm against Excel by eye.

#### The cost bound

Dashes per edge, from `dash_fit` at `RANGE_BORDER_W = 2px` (pitch `3W` = 6px):

| Edge | Length | Dashes | Pitch |
|---|---|---|---|
| Default column (8.43 units) | 65.01px | 11 | 6.10px |
| Narrowest column (`col_px` clamp) | 28px | 4–5 | 6.0–8.0px |
| Widest column (`col_px` clamp) | 320px | 54 | 6.02px |
| Default row (`SHEET_ROW_H`) | 21px | 3 | 8.5px |

None of these are elements — they are shader output, so the count is free.

Worst-case **element** count: one quad per visible boundary cell. Crucially
that is bounded by the **viewport, not the range**, because the grid only
renders visible cells. At the narrowest column (28px) and shortest row (21px) a
1920×1200 grid shows ≈69 × ≈55 cells, so:

- a full-row selection ≈ **69** quads,
- a full-column selection ≈ **55**,
- select-all (A1:XFD1048576) ≈ **123** — its perimeter is mostly off screen.

`RANGE_BORDER_CELL_CAP = 512`, past which `range_border_plan` reports
`dashed: false` and the same edges are drawn solid. At realistic viewport sizes
it cannot be reached; it is a backstop against a display nobody has, and the
tested guarantee is that exceeding it degrades to solid rather than to
stuttering. Separately and unavoidably, the shader draws any edge of `≤ 4W`
(8px) solid on its own — which only a clipped sliver of a column can be.

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
