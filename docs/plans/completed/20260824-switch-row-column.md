# Switch Row/Column for charts

## Overview

A chart built from a worksheet range can read that range either way round:
each **column** is a series (the current and only behaviour), or each **row**
is. Excel exposes the choice as the `Switch Row/Column` button in its Select
Data Source dialog. docxy has no such control, and `chart_from_range`
(gridcore/src/sheet.rs:464) classifies **columns** as numeric-or-label — numeric
columns become series, the first text column supplies the category labels.

The consequence is not a preference: given the table

| | A | B | C | D |
|---|---|---|---|---|
| 1 | Item | Qty | Unit price | Total |
| 2 | Laptop | 2 | 1199 | 2398 |
| 3 | Monitor | 4 | 249.5 | 998 |
| 4 | Keyboard | 6 | 39.99 | 239.94 |

Excel over `A1:D4` produces series *Laptop / Monitor / Keyboard* against
categories *Qty / Unit price / Total*. docxy produces the transpose, and **no
sequence of edits in docxy can reach Excel's chart**. Re-pointing each series
by hand cannot do it either: `series_apply_values` refuses a range whose
`range.1 != range.3`, because a series is assumed to plot one column.

After this plan the chart carries an orientation, the panel has a button that
flips it, and a chart authored either way round survives a save and reload.

### Orientation is derived, never stored

There is no orientation element in SpreadsheetML. Excel infers it from the
shape of the refs a chart holds: `<c:val>` spanning `$B$2:$B$5` is a column
series, `$B$2:$D$2` is a row series. So:

- **Saving** writes nothing new. `chart_space_xml` already prefers a series'
  own `values_ref` via `ChartSource::to_ref` (gridcore/src/xlsx.rs:2233-2237),
  and `to_ref` formats whatever rectangle it is given — a row range serialises
  correctly today with no writer change.
- **Loading** must infer the flag, so the button comes back in the right state.
- The flag on `ChartData` exists for the panel and for re-derivation, not for
  the file.

### Key benefits

- The chart in the user's Excel screenshot becomes reachable.
- A range whose rows are the interesting axis (one row per product, one column
  per month) can be charted the way it reads.
- Re-pointing a series accepts a row when the chart is row-oriented, instead of
  refusing every range wider than one column.

## Context (from discovery)

### Files and components involved

- `gridcore/src/sheet.rs` — `chart_from_range` (:464), `ChartData`,
  `ChartSeries` (:567), `ChartSource` (:320) and its ref formatters `to_ref`
  (:382), `header_ref` (:395), `f_ref` (:363).
- `gridcore/src/xlsx.rs` — `chart_space_xml` (:2196): the `<c:tx>`/`<c:cat>`/
  `<c:val>` refs and the `claimed_col` heuristic (:2245-2258).
- `gridcore/src/drawing.rs` — `parse_chart`: the box fold (:628) and the
  `cat_col` fixup (:674-680).
- `suite/docxy/src/main.rs` — the Chart panel (TYPE row, where the button
  goes), `series_apply_values` and its one-column guard, `rebuild_source`.
- `xlsxy/src/main.rs:3666` — the TUI's own chart insert.

### The signature change crosses both workspaces

`chart_from_range` has three non-test callers, in **two different cargo
workspaces**:

| Caller | Workspace |
|---|---|
| `suite/docxy/src/main.rs:2938` (`chart_apply_range`) | `suite/` |
| `suite/docxy/src/main.rs:5908` (insert chart) | `suite/` |
| `xlsxy/src/main.rs:3666` | root |

This is exactly the shape that broke CI on an earlier plan: the suite compiled
green while `xlsxy` did not. Both must be built for every task that touches
this signature.

### What already works and must not be broken

- **The writer needs no ref change for row series.** `to_ref` (sheet.rs:382)
  emits the rectangle verbatim, so `$B$2:$D$2` comes out right. The
  column-shaped fallback `src?.f_ref(s.col?, s.col?, true)` (xlsx.rs:2237) only
  fires for a series with **no** `values_ref`, which row series always set.
- **`ChartSeries::col`** is a column index used by that fallback and by
  `claimed_col`. It is meaningless for a row series and must be `None` there,
  not a row number in disguise.
- **`ChartSource::cat_col`** is the *column* the labels come from. Row
  orientation takes its labels from the header **row**, so `cat_col` cannot
  express it; the plan must decide what `<c:cat>` derivation does instead of
  quietly reusing it.
- **`rebuild_source`** (suite main.rs) folds each series' name and values refs
  plus the categories to rebuild the chart's box; it is orientation-agnostic
  already, since it only unions rectangles.

### Dependencies

None outside the repo. Both workspaces build clean at the branch head;
`gridcore` has 329 tests and the suite crate 62.

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
- Maintain backward compatibility: a column-oriented chart must behave exactly
  as it does today, byte-for-byte in what it writes — with one carve-out, taken
  knowingly: a BOOLEAN label now reads `TRUE`/`FALSE` rather than Rust's
  `true`/`false`, because the column branch's private `text_of` was lifted into
  the shared `cell_text` so one cell cannot read two ways. That is the spelling
  Excel shows and the one the panel's fields already produced.
  A SECOND carve-out was taken during implementation, in the round answering the
  first review (`c138a7e`): a multi-level `<c:cat>` — Excel's
  `<c:multiLvlStrRef>` — is now refused at import, so a **column-oriented**
  chart with grouped category labels comes back `categories_ref = None`,
  `complex = true`, a narrower `source` box and its part kept verbatim instead
  of regenerated. The **element** is what refuses the ref, not the shape of its
  `<c:f>`: the usual multi-level ref is a rectangle, but two levels over ONE
  category name the line `$A$2:$B$2`, which nothing in the range tells from an
  ordinary row of labels — so a multi-level ref naming a line is refused
  alongside the rectangle. (A `<c:cat>` rectangle stays refused on its own
  account whoever wrote it.) It is the import-side twin of the panel's new
  `categories_shape_err`, and it is deliberate: regenerating such a chart would
  write a one-level `<c:strRef>` naming every level's cells beside a one-level
  cache. Pinned by `a_multi_level_category_ref_is_one_this_model_cannot_hold`
  and `a_multi_level_category_over_one_category_is_refused_too`.
  A THIRD carve-out was taken in the round answering the third review: a
  `<c:pieChart>` holding more than one `<c:ser>` is marked `complex` at import
  (`parse_chart`), so a **column-oriented** multi-series pie — pie is a writable
  kind, and such a chart cleared all three of the older `complex` conditions —
  now round-trips verbatim instead of being regenerated on edit.
  `chart_space_xml`'s pie arm emits `series.first()` only, so regenerating one
  dropped every series after the first without a word. The schema permits the
  shape even though Excel's own UI won't author it, and no panel door builds one
  (Task 10's `chart_kind_series_err` bullet), so it only ever arrives from a
  foreign file. Pinned by
  `a_pie_that_arrives_with_two_series_is_kept_as_excel_wrote_it`. Nothing else on
  the column path moves.

### Build and test commands — read this before Task 1

Two separate cargo workspaces. A green `suite/` says nothing about the root
one, and this plan changes a signature both of them call.

```bash
# the suite (GPUI desktop app) — its own workspace
cargo build  --manifest-path suite/Cargo.toml
cargo test   --manifest-path suite/Cargo.toml

# the root workspace: gridcore, docxcore, xlsxy, gridwasm, lookxy, TUI docxy
cargo build --all-targets
cargo test  -p gridcore

# before any task closes, all of the above must be clean, plus:
cargo clippy -p gridcore --all-targets -- -D warnings
cargo fmt --check
```

Pure free functions in `suite/docxy/src/main.rs` are tested in the
`#[cfg(test)]` module at the bottom of that file — gpui `#[test]` works for
pure logic, but constructing views or elements blows up the render macro, so
**keep every new helper a pure free function**. Model behaviour is tested in
`gridcore/src/sheet.rs`, the writer in `gridcore/src/xlsx.rs`, the loader in
`gridcore/src/drawing.rs`.

## Testing Strategy

- **Unit tests**: required for every task (see Development Approach).
- **Round-trip tests**: the load→save→load path is where orientation actually
  lives, since nothing stores it. A row-oriented chart that comes back as
  column-oriented is the primary failure mode and must be pinned explicitly.
- **E2E tests**: no browser e2e harness here. On-screen verification is manual
  and belongs in Post-Completion.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code, tests, docs in this repo
- **Post-Completion** (no checkboxes): manual on-screen testing, installer
  builds, verification against real Excel

## Implementation Steps

### Task 1: Give a chart an orientation

- [x] add `by_row: bool` to `ChartData` in `gridcore/src/sheet.rs` (`false` =
      today's behaviour, series from columns), documenting that it is derived
      from the refs rather than stored in the file
- [x] confirm `Default` still yields the current behaviour, so every existing
      struct literal and `..Default::default()` site is unchanged in meaning
- [x] build BOTH workspaces — `ChartData` literals exist in `xlsxy`,
      `gridwasm`, `lookxy` and the TUI `docxy` as well as the suite
- [x] write a test that a defaulted `ChartData` is column-oriented, so a later
      change of default is caught rather than silently flipping every chart
- [x] run tests in both workspaces — must pass before Task 2

### Task 2: Build a chart from rows

- [x] add the orientation parameter to `chart_from_range`
      (gridcore/src/sheet.rs:464) and implement the row branch: classify **rows**
      as numeric-or-label, the first text row supplies the category labels, each
      numeric row becomes a series named from the first column's cell
- [x] set `values_ref` to the row rectangle and `name_ref` to the row's label
      cell; leave `col: None` for a row series, since it names a column and the
      writer's fallback and `claimed_col` both read it as one
- [x] keep the column branch byte-for-byte as it is — the row branch is added
      beside it, not folded into a shared generic path, unless that falls out
      cleanly
- [x] update the three callers (suite main.rs:2938, main.rs:5908,
      xlsxy/src/main.rs:3666) to pass the current orientation
- [x] write tests for the row branch mirroring
      `chart_from_range_picks_labels_and_numeric_series`: series names, values,
      categories, and the refs each slot holds
- [x] write a test that the same range built both ways is a true transpose —
      same numbers, series and categories swapped
- [x] write a test for the all-numeric row table, the row analogue of
      `an_all_numeric_table_writes_literal_categories_not_a_plotted_column`
- [x] run tests in BOTH workspaces — must pass before Task 3

### Task 3: Write a row-oriented chart to OOXML

- [x] verify (with a test, not by reading) that `chart_space_xml` already emits
      correct `<c:val>`/`<c:tx>` refs for row series via
      `ChartSource::to_ref` — the expectation is that it does, and the test
      pins it so a later refactor of the ref path cannot regress it silently
- [x] fix `<c:cat>` derivation for row orientation: `cat_col` names a column and
      cannot express a label row, so decide and implement — either a row
      analogue alongside it, or deriving `<c:cat>` only from
      `categories_ref` when `by_row` — and record the reasoning in a comment
- [x] review `claimed_col` (xlsx.rs:2245-2258) under row orientation: it asks
      whether any series occupies a column, which is the wrong question when
      series are rows. Make it answer the right one or not run
- [x] write a test asserting the exact `<c:f>` strings a row-oriented chart
      writes, for values, name and categories
- [x] write a test that a column-oriented chart's output is UNCHANGED by this
      task — the regression that matters most
- [x] run tests — must pass before Task 4

### Task 4: Infer orientation when loading a chart

- [x] in `parse_chart` (gridcore/src/drawing.rs), infer `by_row` from the shape
      of the series' value refs: a `<c:val>` spanning one row and several
      columns is row-oriented; one column and several rows is column-oriented
- [x] decide the ambiguous cases explicitly and comment them: a single cell, a
      series with no ref at all, and a chart whose series disagree. Default to
      column-oriented, which is what every chart written before this plan is
- [x] check the `cat_col` fixup (drawing.rs:674-680) still does the right thing
      for a row chart, or is skipped
- [x] write tests parsing a hand-written row-oriented `chartSpace` and
      asserting `by_row`, the series, and the categories
- [x] write tests for each ambiguous case named above
- [x] run tests — must pass before Task 5

### Task 5: Round-trip a row-oriented chart

- [x] write a test building a row-oriented chart with `chart_from_range`,
      writing it with `chart_space_xml`, parsing it back with `parse_chart`, and
      asserting the orientation, series names, values and categories all survive
- [x] write the same round-trip for a column-oriented chart, asserting it is
      still column-oriented — orientation inference must not flip existing charts
- [x] write a round-trip for the awkward shape: a 2x2 range, where one row and
      one column are equally plausible readings
- [x] run tests — must pass before Task 6

### Task 6: Switch Row/Column in the Chart panel

- [x] add a `Switch Row/Column` button to the Chart panel in
      `suite/docxy/src/main.rs`, beside the TYPE row where Excel puts it
- [x] on click: flip `by_row` and re-derive the chart from its `source` range
      with the new orientation, through the same `chart_from_range` path the
      DATA RANGE field uses, so one code path decides what a range means
- [x] take an undo snapshot before applying — ⚠️ the premise was stale:
      `chart_set_data` DOES take one (`self.sheet_snapshot()`, main.rs), and a
      flip always differs from what is there, so its "committed nothing" early
      return can't swallow it. Verified rather than duplicated — a second
      snapshot would cost two undos per click.
- [x] disable or hide the button when the chart has no `source` box to
      re-derive from (an imported chart whose refs the model cannot hold), and
      say why in the panel rather than failing silently on click — the button is
      enabled on `chart_switched().is_some()`, the same call the click makes, and
      the note under it is that call's own error text, so it names the reason —
      ⚠️ four of them by the end of the branch, not the two this box was
      written against: the missing box, the cell cap, a sheet the workbook has
      lost, and a range with no line of numbers the other way round
- [x] write tests for the pure part: given a `ChartData` and its sheet, the
      flipped chart's series names, values and categories
- [x] write a test that flipping twice returns the original chart
- [x] run tests — must pass before Task 7

### Task 7: Re-pointing a series accepts a row when the chart is row-oriented

- [x] change `series_apply_values`' one-column guard in
      `suite/docxy/src/main.rs`: reject `range.1 != range.3` only when the chart
      is column-oriented, and reject `range.0 != range.2` when it is row-oriented
      — lifted into the pure free function `series_values_shape_err`
- [x] update the message to name the shape that target actually wants, so a
      row-oriented chart does not tell the user to point at `B2:B5` — a row
      chart says "a series plots one row — point at cells like B2:D2", and the
      `ref_example` seeding the "that isn't a range" message flips too
- [x] check `rebuild_source` still produces the right box for row series — it
      unions rectangles, so it should need no change, but pin that with a test
      rather than assuming — confirmed unchanged, pinned in
      `re_pointing_a_row_series_moves_its_ref_and_grows_the_charts_box`
- [x] write tests for the guard in both orientations: the accepted shape and
      the refused one, with the message asserted
- [x] write a test that re-pointing a row series updates its `values_ref` and
      the chart's box — ➕ the mutation moved into a pure `series_set_values`
      so it could be tested at all; it also leaves `col: None` for a row
      series, which the old inline code did not (it stored `range.1`, arming
      the writer's fallback ref and `claimed_col` with a column-shaped answer)
- [x] run tests — must pass before Task 8

### Task 8: Verify acceptance criteria

- [x] verify the Overview's worked example: `A1:D4` on that table, switched,
      yields series Laptop/Monitor/Keyboard against categories Qty/Unit
      price/Total — pinned by
      `switch_row_column_replots_the_range_the_other_way_round`
      (suite/docxy/src/main.rs), which builds exactly the Overview's table,
      charts `(0,0,3,3)` by column, switches it, and asserts both the plot and
      the refs (`Budget!$B$2:$D$2`, `Budget!$A$2`, cats `Budget!$B$1:$D$1`)
- [x] verify a column-oriented chart is unchanged end to end — built, written,
      parsed and displayed exactly as before this plan. ➕ Verified against the
      PRE-PLAN BUILD, not only against this branch's own tests: a throwaway
      example built a workbook holding column/bar/line/pie charts over the
      Overview table plus an all-numeric block, saved it with `save_xlsx`, and
      ran under a git worktree at `af22bcf` (the last commit before the plan)
      and at HEAD. Both files are byte-identical
      (sha256 `dc56be5f…9366bf`, 25470 bytes) — note the fixture holds no
      boolean labels, which is the one carve-out above and the one way a column
      chart's bytes CAN move — and the derived model
      (series names, `col`, values, every `values_ref`/`name_ref`,
      `categories`, `categories_ref`, `source`/`cat_col`) matches line for
      line. Loading that same file on both builds also parses identically. The
      scratch examples were removed afterwards; the in-repo guards remain
      `a_column_oriented_chart_writes_exactly_what_it_wrote_before_orientation`
      (xlsx.rs) and `a_column_oriented_chart_survives_a_write_and_reload`
      (drawing.rs)
- [x] verify the deferred items are still deferred: no header darkening, no
      marching ants, no resolved label lists, no chart source outlines — the
      branch diff since the plan commit contains no `ref_color` /
      `ref_index_at` / darkening / ants code anywhere (the only hits are this
      plan's own sentence describing the check). `git diff --name-only edddfdb`
      (the plan commit) returns thirteen paths: the five expected code files
      (drawing.rs, sheet.rs, xlsx.rs, suite main.rs, xlsxy main.rs), this plan,
      five documentation files (SPREADSHEET.md, README.md, CONTRIBUTING.md,
      `suite/docs/chart-orientation.md`, `suite/docs/range-selector.md` —
      Task 9's, added after this box was ticked), and
      `scripts/revmux-review.{sh,cmd}`, unrelated review tooling that landed on
      this branch in ece3b81 and touches nothing this plan owns
- [x] run `cargo test --manifest-path suite/Cargo.toml` — all pass (70, up
      from 62 at the branch head)
- [x] run `cargo test -p gridcore` — all pass (343 + 1 + 4, up from 329)
- [x] run `cargo build --all-targets` at the repo root — root workspace builds
      (the only warnings are the pre-existing bin/lib `.pdb` filename
      collisions in the COM shims, untouched by this plan)
- [x] run `cargo clippy -p gridcore --all-targets -- -D warnings` and
      `cargo fmt --check` — clean; both were also run for the `suite/`
      workspace, since a green root says nothing about it

### Task 9: [Final] Update documentation

- [x] document the orientation: what it means, that it is derived from the refs
      rather than stored, and how the loader infers it — wherever the suite's
      chart behaviour is already described (`suite/docs/`) — new
      `suite/docs/chart-orientation.md`: both readings side by side, why
      `col: None` for a row series, why saving needs no writer change and the
      `<c:cat>` fallback must not run when `by_row`, the panel button (what
      rides along a flip, what doesn't, and where undo comes from), and the
      re-pointing shape check
- [x] note the ambiguous-case decisions from Task 4, so the next reader does
      not rediscover them from the code — "How the loader infers orientation"
      lists all five (single cell, no readable ref, both-ways rectangle,
      disagreeing series, no votes), the unanimity rule, and why the `cat_col`
      fixup is skipped for a row chart
- [x] ➕ linked from the three places chart behaviour is already described, and
      fixed the one claim this plan made stale: `range-selector.md`'s
      `SeriesValues(i)` row said a series reads "one column" full stop, which
      is no longer true of a row chart. Also linked from `CONTRIBUTING.md` and
      `SPREADSHEET.md`'s chart section

### Task 10: Review rounds (added after Task 9)

Work the review rounds turned up, recorded here because the checklist above was
already complete when it landed.

- [x] ➕ `parse_chart` refuses a multi-level `<c:cat>` (Excel's
      `<c:multiLvlStrRef>`), calling it a ref this model cannot hold. The
      ELEMENT is what refuses it, not the shape of its `<c:f>` — a multi-level
      ref naming a LINE (two levels over one category, `$A$2:$B$2`) is refused
      alongside the usual rectangle, since no shape test tells that line from an
      ordinary row of labels. See the second carve-out under *Development
      Approach* and `SPREADSHEET.md` §4a. Tests:
      `a_multi_level_category_ref_is_one_this_model_cannot_hold`,
      `a_multi_level_category_over_one_category_is_refused_too`
- [x] ➕ the category-ref cache pairs FIRST-wins per index, matching the cached
      name above it: last-wins handed every series the last series' `<c:f>`
      beside the first series' `<c:strCache>` — a cache contradicting its own
      ref
- [x] ➕ `categories_shape_err` in the panel: the CATEGORY LABELS field refuses
      a genuine rectangle, for the same order mismatch the loader now refuses.
      Documented in `range-selector.md` and `chart-orientation.md`
- [x] ➕ `chart_field_examples` / `chart_range_help`: every hint in the panel
      flips with the chart, so no field offers an example the next commit would
      refuse
- [x] ➕ `infer_by_row` asks the CATEGORIES before the stacked-single-cell
      rule. Re-pointing two column series at single cells one above the other
      is reachable by hand (`series_values_shape_err` refuses only a
      several-column ref), and that shape is otherwise read as row evidence, so
      a column chart flipped itself on reload. Test:
      `a_column_chart_re_pointed_at_stacked_cells_stays_a_column_chart`
- [x] ➕ `chart_kind_series_err`: a pie is refused a plot with more than one
      series at every door to that state, since `chart_space_xml` writes only a
      pie's first series — `chart_switched` (so the button greys out with the
      reason under it), `chart_apply_range`, `chart_set_kind` (picking **Pie**
      on a chart that already has N series: it does NOT re-derive, so guarding
      the re-derivations alone missed the widest door of the lot) and
      `sheet_insert_chart` (Insert ▸ Pie over several numeric columns). Matches
      `series_add`'s existing refusal, which applies the same rule in its own
      words (`n > 0` before the push) rather than calling the helper. Test:
      `a_pie_is_refused_a_re_derived_plot_with_more_than_one_series`
- [x] ➕ the IMPORT side of the same rule: `parse_chart` marks a `<c:pieChart>`
      holding more than one `<c:ser>` `complex`, so a foreign multi-series pie
      is kept verbatim rather than regenerated one slice group short on the next
      edit. This one MOVES a column-oriented chart — see the third carve-out
      under *Development Approach* and *Backward compatibility*. Test:
      `a_pie_that_arrives_with_two_series_is_kept_as_excel_wrote_it`
- [x] ➕ `categories_shape_err` follows the ORIENTATION, like
      `series_values_shape_err`: one column on a column chart, one row on a row
      one, a single cell either way, a rectangle never. It first refused only
      the rectangle and took either line on either orientation, which
      contradicted the tiebreak above — `infer_by_row` reads the orientation
      back OUT of `<c:cat>`'s shape, so committing a row of labels onto a column
      chart (or a column onto a row one) flipped the chart on reload, the mirror
      of the hole the tiebreak was added to close. The panel's own hints
      (`chart_field_examples`) already offered the orientation-matching shape,
      so this is the guard catching up with its hint. Tests:
      `the_category_labels_field_takes_the_line_this_chart_reads`,
      `re_pointing_the_labels_the_way_the_chart_reads_keeps_its_orientation`
- [x] ➕ narrowed two claims that overstated what the tiebreak covers: the
      stacked-cell rule fires when `<c:cat>` is one cell OR ABSENT, and absent
      is not only the genuine row case — `chart_from_columns` leaves
      `categories_ref` `None` on an all-numeric range, so such a column chart
      with every series re-pointed at a single cell is still a guess. Said so in
      `infer_by_row` and `chart-orientation.md` rather than restructuring the
      inference, since one series left with a multi-row ref settles it before
      the tiebreak runs. Also dropped SPREADSHEET.md §4a's "unlike the three
      above it can fire on a column-oriented chart" — all of them do (a
      `<c:val>` of `Sheet1!$B:$B` most of all); the real contrast is the symptom

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### What each orientation means

| | `by_row: false` (today) | `by_row: true` |
|---|---|---|
| A series is | a column | a row |
| Series named from | the header cell above the column | the label cell left of the row |
| Categories from | the first text column | the first text row |
| `values_ref` shape | `$B$2:$B$5` | `$B$2:$D$2` |
| `ChartSeries::col` | the column index | `None` |

### Why `col: None` for a row series

`ChartSeries::col` feeds two column-shaped decisions: the writer's fallback ref
`src?.f_ref(s.col?, s.col?, true)` and `claimed_col`'s "does a series already
occupy this column?". Storing a row index there would make both silently wrong
rather than inapplicable. A row series always carries `values_ref`, so the
fallback is never needed.

### Backward compatibility

Every chart in every existing file is column-oriented, and Task 4's inference
must default to that. Task 3's "column output unchanged" test and Task 5's
column round-trip are the two guards on this; treat a failure in either as a
blocker rather than an expectation to update. There are three deliberate
exceptions, all recorded in the last backward-compatibility bullet of
*Development Approach*: the boolean-label spelling (`TRUE`/`FALSE` rather than
`true`/`false`), pinned by
`a_derived_label_reads_the_same_as_the_one_a_field_would_show`; the
multi-level `<c:cat>` refusal at import, pinned by
`a_multi_level_category_ref_is_one_this_model_cannot_hold` and
`a_multi_level_category_over_one_category_is_refused_too`; and the multi-series
`<c:pieChart>` ⇒ `complex` marking at import, pinned by
`a_pie_that_arrives_with_two_series_is_kept_as_excel_wrote_it`. All three keep a
column-oriented chart's part VERBATIM rather than writing it differently, which
is the direction a data-preservation fix may move in. Anything else that
moves on the column path is a regression, not an exception to re-bless.

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes*

**Manual verification**:

- Build the table from the Overview in the suite, insert a column chart over
  `A1:D4`, and press Switch Row/Column. Confirm the legend becomes
  Laptop/Monitor/Keyboard and the axis becomes Qty/Unit price/Total.
- Save, reopen in the suite, and confirm the button is still showing the
  row-oriented state.
- Save, open in real Excel, open Select Data Source, and confirm Excel agrees
  about which entries are series and which are category labels.
- Press Switch Row/Column in Excel, save there, reopen in the suite, and
  confirm the suite infers the orientation Excel wrote.

**Build and distribution**:

- Dispatch the release workflow and install `docxy-suite-setup.exe` — the GPUI
  panel cannot be checked by unit tests alone.

**Still deferred** (each wants its own plan):

- Header darkening across the whole affected range, in Excel's grey.
- Marching-ants styling for a pointed range.
- Resolved series-name and category-label lists beside the range boxes.
- Chart-selected source outlines, reusing `ref_color` / `ref_index_at`.
