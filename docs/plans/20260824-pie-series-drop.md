# A pie chart stops discarding series on save

## Overview

Give a chart three series, click **Pie**, save, reopen: two series are gone
from the file. Nothing warns at any point — the panel goes on listing all
three, each pointed at cells and colourable, and no status line is set.

The loss happens in one place. `chart_space_xml`'s pie arm
(gridcore/src/xlsx.rs:2355-2361) writes `data.series.first()` and discards the
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

### The doors, for completeness

`chart_kind_series_err` (suite/docxy/src/main.rs:1790) already refuses a second
series from `series_add`, `chart_apply_range` and `chart_switched`. Two paths
reach the same state unguarded:

- **`chart_set_kind`** (main.rs:3281-3294) — select a 3-series column chart,
  click **Pie**. The widest door, and the one reached by clicking the word.
- **`sheet_insert_chart`** (main.rs:6200) — select `A1:D5`, click **Pie** on the
  ribbon; `chart_from_range` returns three series.

Once saving preserves every series, those doors stop being holes: reaching the
state is fine. What must change is that the panel says a pie plots the first
series only, rather than implying all three are drawn.

### Key benefits

- Editing a chart never silently deletes work.
- A chart converted to Pie and back keeps its series.
- The panel stops implying that series it will not draw are drawn.

### Deliberately out of scope

- **Doughnut.** Rendering multiple rings is a separate feature; this plan only
  stops discarding the data a doughnut would later need.
- **Multi-level categories.** A related pre-existing finding
  (gridcore/src/drawing.rs:693) uses range shape as a proxy for
  `<c:multiLvlStrRef>`, so a one-line multi-level ref is accepted and flattened
  on save. Real, unrelated, and wants its own plan.

## Context (from discovery)

### Files and components involved

- `gridcore/src/xlsx.rs:2355-2361` — the pie arm of `chart_space_xml`, where
  the data is dropped.
- `gridcore/src/drawing.rs` — `parse_chart`, which must read every `<c:ser>`
  back out of a `<c:pieChart>`.
- `suite/docxy/src/main.rs` — `chart_kind_series_err` (:1790) and its three
  callers, `chart_set_kind` (:3281), `sheet_insert_chart` (:6200), the type
  buttons (:6448-6479) and the series list (:6558).
- `xlsxy/src/main.rs` — the TUI's chart insert, same entry point shape.
- The chart renderer in the suite, which must draw only the first series for a
  pie even though several are now present.

### Provenance

Every finding below is **pre-existing** — none was introduced by the Switch
Row/Column branch. They surfaced there because that branch added
`chart_kind_series_err`, which made the codebase *look* protected where it was
not.

| Reviewer | Confidence | Angle |
|---|---|---|
| bugs+impl | 85 | `chart_kind_series_err` guards only the re-derivation paths |
| arch+quality | 85 | `chart_set_kind` is the fourth and widest door |
| adversarial | 94 | pie insertion bypasses the guard entirely |

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

- [ ] verify against ECMA-376 that `CT_PieChart` declares `ser` with
      `maxOccurs="unbounded"`, and record the citation in a comment beside the
      pie arm — the whole plan rests on this, and the code currently carries the
      opposite claim as a comment
- [ ] check what the OOXML corpus holds: search the sample workbooks for a
      `<c:pieChart>` containing more than one `<c:ser>`, and note whether real
      files in the wild do this
- [ ] ⚠️ if the format does NOT permit it, STOP and update this plan: the
      approach changes to guarding the doors instead, and the remaining tasks
      are wrong. Do not proceed on the assumption
- [ ] no code change in this task; record the finding in the plan

### Task 2: Write every series a pie holds

- [ ] change `chart_space_xml`'s pie arm (gridcore/src/xlsx.rs:2355-2361) to
      emit a `<c:ser>` for every series, as the bar and line arms do, rather
      than `data.series.first()`
- [ ] replace the "extra series are invalid (that's doughnut)" comment with
      what is actually true: the format allows several, Excel plots the first,
      and we keep them so a save never deletes the user's work
- [ ] check the `<c:idx>`/`<c:order>` each series gets are sequential, the way
      the multi-series arms already produce them
- [ ] write a test asserting a three-series pie writes three `<c:ser>` blocks
      with distinct refs
- [ ] write a test that a one-series pie's output is UNCHANGED — the common
      case must not move
- [ ] run tests in both workspaces — must pass before Task 3

### Task 3: Read every series back

- [ ] check `parse_chart` returns all series from a `<c:pieChart>` — it walks
      `<c:ser>` generically, so this may already hold; pin it with a test rather
      than assume
- [ ] write a round-trip test: build a three-series pie, write it, parse it,
      and assert all three survive with their names, values and refs
- [ ] write a round-trip test for the one-series pie
- [ ] run tests — must pass before Task 4

### Task 4: Draw and describe only what a pie plots

- [ ] make the suite's chart renderer draw only the first series for a pie,
      explicitly and in one place, rather than relying on the writer having
      discarded the others
- [ ] in the Chart panel, mark the series beyond the first as not plotted —
      the series cards (main.rs:6558) currently present all of them
      identically, which is what makes the loss invisible
- [ ] add a note beside the type buttons (main.rs:6448-6479) saying a pie plots
      the first series only, shown when a pie has more than one
- [ ] write tests for the pure part: given a kind and a series count, what the
      panel should say
- [ ] run tests — must pass before Task 5

### Task 5: Retire the refusal

- [ ] `chart_kind_series_err` (main.rs:1790) refuses a state that is now legal
      and lossless. Remove it, along with its calls in `series_add` (:3564),
      `chart_apply_range` (:3152) and `chart_switched` (:3242)
- [ ] leave `chart_set_kind` (:3281) and `sheet_insert_chart` (:6200) unguarded
      — that is now correct, and is the point of the change
- [ ] update or delete the tests that pinned the refusal, and say in the commit
      why a removed guard is the fix rather than a regression
- [ ] write a test that adding a second series to a pie now succeeds and
      survives a save
- [ ] run tests — must pass before Task 6

### Task 6: Verify acceptance criteria

- [ ] verify the Overview's scenario: three series, click Pie, save, reopen —
      all three series are still there, and only the first is drawn
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

- Multi-level categories: `drawing.rs:693` uses range shape as a proxy for
  `<c:multiLvlStrRef>`, so a one-line multi-level ref is accepted and flattened
  on an edited save. Wants its own plan.
- Doughnut rendering, which would actually draw the extra series this plan
  preserves.
