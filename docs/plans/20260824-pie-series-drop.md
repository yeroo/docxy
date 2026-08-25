# A pie chart stops discarding series on save

## Overview

Give a chart three series, click **Pie**, save, reopen: two series are gone
from the file. Nothing warns at any point — the panel goes on listing all
three, each pointed at cells and colourable, and no status line is set.

The loss happens in one place. `chart_space_xml`'s pie arm
(gridcore/src/xlsx.rs) writes `data.series.first()` and discards the
rest:

```rust
"pie" => {
    // Pie takes a single series; extra series are invalid (that's doughnut).
    let pie_ser = data.series.first().map(|s| ser_xml(0, s)).unwrap_or_default();
```

**That comment is the bug.** `CT_PieChart` in ECMA-376 declares `ser` with
`maxOccurs="unbounded"` — several `<c:ser>` in a `<c:pieChart>` is
schema-valid. Excel simply *plots* the first one. So the writer destroys user
data to satisfy a constraint the format does not impose.

Three separate reviewers reported this from three angles during the
Switch Row/Column review, each proposing to refuse the state at the UI doors
that reach it. This plan does the opposite: the state is legal, so **keep the
data and stop lying about it**. Nothing is refused; nothing is lost.

### The doors, as they stand at HEAD

**All five are shut.** `chart_kind_series_err`
(suite/docxy/src/main.rs) is called by `chart_apply_range`,
`chart_switched`, `chart_set_kind` and `sheet_insert_chart`;
`series_add` applies the same rule in its own words
(`data.kind == "pie" && n > 0`, asked before the push). The two this plan was
first drafted against — `chart_set_kind`, the widest door, and
`sheet_insert_chart` — were closed by commit 02654cd, each with a comment saying
why, and the guard's own doc comment enumerates all five.

So the Overview's scenario **cannot be reached from the panel today**: clicking
**Pie** on a three-series chart is refused with a message. What can still reach
it is a FILE — the schema allows a multi-series `<c:pieChart>` even though
Excel's own UI will not author one, and `parse_chart` therefore marks such a
chart `complex` (gridcore/src/drawing.rs, the `cd.kind == "pie" &&
cd.series.len() > 1` term of `cd.complex`) so its part round-trips verbatim rather than being
regenerated one slice group short.

That is the state this plan changes: once saving preserves every series, the
refusals stop earning their keep and the `complex` hold-back stops being needed
for pie. What must change alongside is that the panel says a pie plots the first
series only, rather than implying all three are drawn.

### Key benefits

- Editing a chart never silently deletes work.
- A chart converted to Pie and back keeps its series.
- The panel stops implying that series it will not draw are drawn.

### Deliberately out of scope

- **Doughnut.** Rendering multiple rings is a separate feature; this plan only
  stops discarding the data a doughnut would later need.

*(The multi-level-category finding this plan originally deferred was FIXED
before the plan was written: `parse_chart` keys on the `<c:multiLvlStrRef>`
element rather than the shape of its `<c:f>` — the `multiLvlStrRef` arm of
`parse_chart`'s `Event::Start`, read at its `let held` filter — and `a_multi_level_category_over_one_category_is_refused_too`
(drawing.rs) pins the one-line case. Nothing left to plan.)*

## Context (from discovery)

### Files and components involved

- `gridcore/src/xlsx.rs` — the pie arm of `chart_space_xml`, where
  the data is dropped.
- `gridcore/src/drawing.rs` — `parse_chart`, which must read every `<c:ser>`
  back out of a `<c:pieChart>`.
- `suite/docxy/src/main.rs` — `chart_kind_series_err` and its four
  callers `chart_apply_range`, `chart_switched`,
  `chart_set_kind` and `sheet_insert_chart`; `series_add`,
  which asks the same question inline; the type buttons and the series
  list.
- `xlsxy/src/main.rs` — the TUI's chart insert, same entry point shape.
- The chart renderer in the suite, which must draw only the first series for a
  pie even though several are now present.

### Provenance

The data loss in `chart_space_xml` is **pre-existing** — it was not introduced
by the Switch Row/Column branch. It surfaced there because that branch added
`chart_kind_series_err`, whose three original call sites made the codebase
*look* protected where it was not; the remaining two doors were closed in the
same branch (02654cd) once the reviewers named them.

| Reviewer | Confidence | Angle |
|---|---|---|
| bugs+impl | 85 | `chart_kind_series_err` guards only the re-derivation paths |
| arch+quality | 85 | `chart_set_kind` is the fourth and widest door |
| adversarial | 94 | pie insertion bypasses the guard entirely |

### Task 1 finding — the format does permit it (2026-08-25)

**Schema.** ECMA-376 Part 1, DrawingML Charts (`dml-chart.xsd`): `CT_PieChart`
takes its content from the group `EG_PieChartShared`, which declares

```xsd
<xsd:element name="ser" type="CT_PieSer" minOccurs="0" maxOccurs="unbounded"/>
```

so more than one `<c:ser>` inside one `<c:pieChart>` is schema-valid. The
citation now sits beside the pie arm in `chart_space_xml`
(gridcore/src/xlsx.rs). Excel plotting only the first series is a *plotting*
rule, not a format rule — the plan's premise holds and Tasks 2–7 stand.

**Corpus.** Scanned all 555 workbooks under `corpus/` (192 chart parts,
29 `<c:pieChart>` elements). **Four** hold more than one `<c:ser>`, and every
one of them was written by real Excel (`docProps/app.xml` says
`Microsoft Excel`):

| File | Part | `<c:ser>` |
|---|---|---|
| `corpus/xlsx-ext/openoffice/test/testgui/data/pvt/complex_29s.xlsx` | `xl/charts/chart3.xml` | 7 |
| `corpus/xlsx-ext/libreoffice/chart2/qa/extras/data/xlsx/chart-hatch-fill.xlsx` | `xl/charts/chart1.xml` | 2 |
| `corpus/xlsx-ext/libreoffice/chart2/qa/extras/data/xlsx/strict_chart.xlsx` | `xl/charts/chart1.xml` | 2 |
| `corpus/xlsx-ext/libreoffice/chart2/qa/extras/data/xlsx/tdf111173.xlsx` | `xl/charts/chart1.xml` | 2 |

So multi-series pies are not merely legal but present in the wild — today each
of these four is held back as `complex` by `parse_chart`, which is exactly the
hold-back Task 2 removes.

➕ Noted for Task 2: `<c:idx>`/`<c:order>` need NOT be contiguous. `complex_29s`
runs 0–6 sequentially, but `tdf111173` uses idx 0 and idx **2**. The writer
emitting sequential indices is fine; the READER (Task 3) must not assume they
are.

### Dependencies

None outside the repo. Both workspaces build clean at the branch head;
`gridcore` has 351 tests and the suite crate 74.

## Development Approach

- **Testing approach**: Regular — code first, tests in the same task, before
  that task closes.
- Complete each task fully before moving to the next.
- Make small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for code changes in
  that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new and for modified functions
  - cover both success and error scenarios
- **CRITICAL: all tests must pass before starting the next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change

### Build and test commands — read this before Task 1

Two separate cargo workspaces. A green `suite/` says nothing about the root one.

```bash
cargo build  --manifest-path suite/Cargo.toml
cargo test   --manifest-path suite/Cargo.toml
cargo build --all-targets
cargo test  -p gridcore
cargo clippy -p gridcore --all-targets -- -D warnings
cargo fmt --check
```

Pure free functions in `suite/docxy/src/main.rs` are tested in the
`#[cfg(test)]` module at the bottom of that file — gpui `#[test]` works for
pure logic, but constructing views or elements blows up the render macro, so
**keep every new helper a pure free function**.

## Testing Strategy

- **Unit tests**: required for every task.
- **Round-trip tests**: the defect is a save that loses data, so
  build → write → parse → assert-all-series-survive is the test that actually
  pins it. A test that only checks the writer's output string would pass while
  the loader still dropped series.
- **E2E tests**: none here. On-screen and real-Excel checks are manual and
  belong in Post-Completion.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Keep plan in sync with actual work done

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code, tests, docs in this repo
- **Post-Completion** (no checkboxes): manual verification against real Excel,
  installer builds

## Implementation Steps

### Task 1: Confirm the format actually permits it

- [x] verify against ECMA-376 that `CT_PieChart` declares `ser` with
      `maxOccurs="unbounded"`, and record the citation in a comment beside the
      pie arm — the whole plan rests on this, and the code currently carries the
      opposite claim as a comment (CONFIRMED: `EG_PieChartShared`,
      `dml-chart.xsd`; citation now in the pie arm)
- [x] check what the OOXML corpus holds: search the sample workbooks for a
      `<c:pieChart>` containing more than one `<c:ser>`, and note whether real
      files in the wild do this (4 of 29 pie charts, all Excel-authored —
      table in Context)
- [x] ⚠️ if the format does NOT permit it, STOP and update this plan: the
      approach changes to guarding the doors instead, and the remaining tasks
      are wrong. Do not proceed on the assumption (not triggered — the format
      permits it)
- [x] no code change in this task; record the finding in the plan (only the
      citation comment beside the pie arm; no behaviour change)

### Task 2: Write every series a pie holds

- [x] change `chart_space_xml`'s pie arm (gridcore/src/xlsx.rs) to
      emit a `<c:ser>` for every series, as the bar and line arms do, rather
      than `data.series.first()` (the arm now interpolates the shared `{sers}`)
- [x] drop the `cd.kind == "pie" && cd.series.len() > 1` term from
      `parse_chart`'s `cd.complex` (gridcore/src/drawing.rs): it holds an
      imported multi-series pie's part back from regeneration precisely because
      the writer would lose series, so it must go in the same task the writer
      stops losing them — otherwise such a chart stays uneditable
- [x] update the tests that pin that hold-back (drawing.rs, the `pie` case
      around the `complex` assertions) to expect a regenerable chart
      (`a_pie_that_arrives_with_two_series_is_kept_as_excel_wrote_it` renamed
      to `…_stays_editable` and inverted; `chart_space_xml_per_kind`'s
      "pie takes a single series" assertion folded into the shared
      both-series-expected one)
- [x] replace the "extra series are invalid (that's doughnut)" comment with
      what is actually true: the format allows several, Excel plots the first,
      and we keep them so a save never deletes the user's work
- [x] check the `<c:idx>`/`<c:order>` each series gets are sequential, the way
      the multi-series arms already produce them (`ser_xml`'s `si` comes from
      `enumerate`, so 0,1,2 — pinned by the new test)
- [x] write a test asserting a three-series pie writes three `<c:ser>` blocks
      with distinct refs (`a_pie_writes_every_series_it_holds`)
- [x] write a test that a one-series pie's output is UNCHANGED — the common
      case must not move (`a_one_series_pie_is_written_exactly_as_before`)
- [x] run tests in both workspaces — must pass before Task 3 (gridcore 362 +
      1 + 4, suite 80; clippy and `cargo fmt --check` clean)

### Task 3: Read every series back

- [x] check `parse_chart` returns all series from a `<c:pieChart>` — it walks
      `<c:ser>` generically, so this may already hold; pin it with a test rather
      than assume (CONFIRMED: the `"ser"` Start arm pushes one `ChartSeries` per
      element in DOCUMENT ORDER and `<c:ser><c:idx>` is never read — the only
      `idx` the parser touches is `<c:pt idx=…>`. Pinned by
      `a_pie_whose_series_indices_skip_a_number_still_reads_as_two`
      (the `tdf111173` idx 0/2 shape from Task 1's ➕ note) and
      `a_seven_series_pie_reads_back_all_seven` (`complex_29s`), drawing.rs)
- [x] write a round-trip test: build a three-series pie, write it, parse it,
      and assert all three survive with their names, values and refs
      (`a_three_series_pie_survives_a_write_and_a_read`, xlsx.rs — also asserts
      the chart comes back non-`complex` and writable, and that rewriting what
      was read reproduces the same part)
- [x] write a round-trip test for the one-series pie
      (`a_one_series_pie_survives_a_write_and_a_read`, xlsx.rs)
- [x] run tests — must pass before Task 4 (gridcore 366, suite 80; root
      workspace builds; clippy and `cargo fmt --check` clean)

### Task 4: Draw and describe only what a pie plots

- [x] make the suite's chart renderer draw only the first series for a pie,
      explicitly and in one place, rather than relying on the writer having
      discarded the others (`chart_plotted_series` in main.rs is the one place;
      `chart_card` derives `nser` from it and slices `plotted`, so `maxv`,
      `ncat`, every per-series loop and the pie arm's proportions all measure
      the DRAWN series only — an undrawn one can no longer stretch the axis or
      add an empty slice)
- [x] in the Chart panel, mark the series beyond the first as not plotted —
      the series cards (`series_card` in main.rs) currently present all of them
      identically, which is what makes the loss invisible (a `NOT PLOTTED` tag
      beside the card's `NAME` label, asked via `series_is_plotted`)
- [x] add a note beside the type buttons (the `CHART TYPE` row in main.rs) saying a pie plots
      the first series only, shown when a pie has more than one
      (`chart_unplotted_note`, rendered under `types` in `chart_panel`; it says
      the others are KEPT in the file, since they now are)
- [x] write tests for the pure part: given a kind and a series count, what the
      panel should say (`a_pie_draws_one_series_however_many_it_holds` and
      `the_panel_says_a_pie_plots_the_first_series_only`, main.rs)
- [x] run tests — must pass before Task 5 (suite 82; gridcore 366 + 1 + 4; root
      workspace builds; `cargo clippy -p gridcore --all-targets -- -D warnings`
      and `cargo fmt --check` clean in both workspaces)

### Task 5: Retire the refusal

- [x] `chart_kind_series_err` (main.rs) refuses a state that is now legal
      and lossless. Remove it and **all four** of its call sites:
      `chart_apply_range`, `chart_switched`, `chart_set_kind`
      and `sheet_insert_chart`. Deleting the function while any
      caller stands does not compile (all four gone; the door the plan calls
      `chart_apply_range` is `chart_reauthored`, which is what that commit path
      actually asks — the range commit in `chart_apply_range` asked it too, so
      five sites in all, each replaced by a comment saying what it used to buy)
- [x] remove `series_add`'s inline equivalent too (`data.kind == "pie"
      && n > 0`) — it is the same rule in its own words, and leaving it would
      keep the "+ Series" button refusing what every other door now allows
- [x] the guard's doc comment enumerates all five doors;
      it goes with the function (deleted with it)
- [x] update or delete the tests that pinned the refusal, and say in the commit
      why a removed guard is the fix rather than a regression
      (`a_pie_is_refused_a_re_derived_plot_with_more_than_one_series` inverted
      into `every_door_to_a_multi_series_pie_is_open_and_says_what_it_draws`,
      which walks the same four doors and asserts each yields the multi-series
      pie and describes it via `chart_plotted_series`/`chart_unplotted_note`;
      `picking_a_writable_type_authors_a_valueless_scatter_afresh`'s pie arm
      inverted from `expect_err("two series")` to a two-series pie)
- [x] write a test that adding a second series to a pie now succeeds and
      survives a save (`a_series_added_to_a_pie_survives_a_save`, xlsx.rs —
      `series_add`'s exact push, written, read back, then pointed at cells and
      round-tripped again; the panel half is in the suite test above, which
      cannot reach the writer across the crate wall)
- [x] run tests — must pass before Task 6 (suite 82, gridcore 367 + 1 + 4; root
      workspace builds; `cargo clippy` clean in both and `cargo fmt --check`
      clean in both. ⚠️ `cargo build --manifest-path suite/Cargo.toml` cannot
      relink `suite.exe` while a suite window is open — "Access is denied";
      `cargo test`/`cargo clippy --all-targets` compile the same code and pass)

### Task 6: Verify acceptance criteria

- [ ] verify the Overview's scenario: three series, click Pie, save, reopen —
      all three series are still there, and only the first is drawn. (This is
      the step that proves Task 5 landed: before it, the click is REFUSED by
      `chart_kind_series_err` and the scenario cannot be reached at all)
- [ ] verify a workbook that arrives holding a multi-series `<c:pieChart>` is
      now editable rather than held back as `complex`
- [ ] verify a chart converted to Pie and back to Column keeps all its series
- [ ] verify the one-series pie is unchanged end to end
- [ ] run `cargo test --manifest-path suite/Cargo.toml` — all pass
- [ ] run `cargo test -p gridcore` — all pass
- [ ] run `cargo build --all-targets` — root workspace builds
- [ ] run `cargo clippy -p gridcore --all-targets -- -D warnings` and
      `cargo fmt --check` — clean

### Task 7: [Final] Update documentation

- [ ] document that a pie keeps every series and plots the first, wherever the
      suite's chart behaviour is described (`suite/docs/`)
- [ ] record the ECMA-376 citation from Task 1 so the next reader does not
      reinstate the drop

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Why not guard the doors

Refusing the state is what all three reviewers proposed, and it is the smaller
change. It was rejected because it forbids something the file format allows,
which means Excel can hand us a workbook we refuse to represent — and because
doors keep appearing: the guard added during the Switch Row/Column work covered
three call sites and missed two, within one change.

Making the save lossless removes the class of bug rather than the two known
instances of it.

### Why not enforce it in the model

Capping `ChartData.series` at one for a pie is the same data loss moved earlier
— the user's second series would vanish the moment the kind changed, before
any save. It also makes converting Pie → Column destructive, which is worse
than the bug being fixed.

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes*

**Manual verification**:

- Build a 3-series table, insert a Pie in the suite, save, and open in **real
  Excel**: confirm Excel opens it without a repair prompt, plots the first
  series, and lists all three under Select Data Source.
- Convert that chart to Column in Excel and confirm all three plot.
- Save from Excel, reopen in the suite, and confirm all three survive.

⚠️ The real-Excel check is the one that matters. If Excel reports the workbook
as needing repair, Task 1's premise was wrong and the approach must change —
report that rather than working around it.

**Build and distribution**:

- Dispatch the release workflow and install `docxy-suite-setup.exe`.

**Related, deferred**:

- Doughnut rendering, which would actually draw the extra series this plan
  preserves.
