# Excel chart parity second-review fixes

## Overview

Close the four in-scope minor findings from Revmux round
`ralphex-20260827-excel-chart-parity/02-after-fix`, then run one final
supervised review. This pass fixes vertical category-row clipping, completes
the stable fixture projection, and aligns the chart-preview document with the
orientation-aware implementation and its conditional gap floor.

## Scope boundaries

- Preserve chart data, references, OOXML serialization, axes, marks, legend,
  horizontal label behavior and selection behavior.
- Do not change the pre-existing shortened source comment at
  `suite/docxy/src/main.rs:3341` in this plan.
- Do not address the pre-existing pie/non-finite or 512-point scale findings,
  the stale `chart_grips` comment, undo/package behavior, OCR, or golden images.
- Keep changes limited to `suite/docxy/src/main.rs`,
  `uiharness/tests/fixture.rs`, `suite/docs/chart-preview.md`, this plan, and
  directly related tests.

## Acceptance criteria

- Every retained vertical category label renders in a fixed 12-pixel row;
  edge clamping and forced context cannot make retained rows overlap or shrink.
- Ordinary four-category bar cards retain all four labels, while denser cards
  thin deterministically to rows that actually fit.
- Fixture drift comparison includes parsed `ChartData::complex` and a focused
  regression distinguishes otherwise-identical simple and complex charts.
- Documentation distinguishes width-planned horizontal labels from
  height-planned bar labels and accurately describes the conditional 4-pixel
  category-gap target.
- Focused tests cover the reported 150×132/11-category geometry, ordinary and
  tiny bar cases, and the `complex` fixture projection.
- Root and suite tests, both Clippy invocations with `-D warnings`, both format
  checks, and `git diff --check` pass.

## Implementation steps

### Task 1: Keep vertical category rows fixed and disjoint

- [x] sweep the vertical planner and renderer together, including every use of
      `axis_start`, `axis_extent`, and the 12-pixel row-height constant
- [x] choose retained vertical indices only when their edge-clamped fixed
      12-pixel rows fit without overlap; do not reuse horizontal midpoint box
      extents as rendered vertical row heights
- [x] render every retained vertical row at 12 pixels while keeping horizontal
      midpoint partitions and truncation unchanged
- [x] test the 150×132 card's approximately 49.5-pixel plot with 11 categories,
      the four-label fixture, and zero/tiny/one-category boundaries; assert
      retained row bounds are fixed-height, in-range, ordered and disjoint
- [x] run suite tests before Task 2

### Task 2: Include parsed complexity in fixture parity

- [x] re-enumerate every stable field in `StableFixtureChart` and confirm which
      loaded fields affect chart editing or saving
- [x] include `ChartData::complex` in the stable projection while continuing to
      exclude runtime-only `edited`
- [x] add a focused regression that distinguishes otherwise-identical simple
      and complex chart data
- [x] run the UI harness fixture tests before Task 3

### Task 3: Correct the remaining chart-preview contracts

- [x] document that horizontal labels use plot width and bar labels use plot
      height with 12-pixel row capacity; do not claim rendered cards exercise a
      helper-only sub-12-pixel fallback
- [x] document that the nominal 4–24-pixel category-gap target can fall below
      4 pixels when the 40%-of-slot cap is smaller
- [x] sweep the focused chart-preview documentation for equivalent absolute
      orientation and gap-floor claims
- [x] run documentation/search and whitespace checks before Task 4

### Task 4: [Final] Verify and archive the second-review fixes

- [ ] re-read all four round-02 findings against the resulting diff and verify
      the mechanisms rather than only the quoted examples
- [ ] run `cargo test` and `cargo test --manifest-path suite/Cargo.toml`
- [ ] run root and suite `cargo clippy --all-targets -- -D warnings`
- [ ] run root and suite `cargo fmt --check` and `git diff --check`
- [ ] record final counts and deliberate exclusions here, then move this plan
      to `docs/plans/completed/`

## Review record

- Round-02 report:
  `.revmux/tasks/ralphex-20260827-excel-chart-parity/02-after-fix/report.md`
- Actioned findings: one vertical-label geometry correction, one fixture
  projection correction, and two chart-preview documentation corrections.
- Excluded findings: all entries classified as `pre_existing` and every
  previously deferred chart/package/testing item.
