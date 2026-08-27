# Excel chart parity review fixes

## Overview

Close the six in-scope minor findings from Revmux round
`ralphex-20260827-excel-chart-parity/01-initial` without expanding into the
separately reported pre-existing chart issues.

The follow-up corrects two category-label layout mechanisms, makes fixture
drift detection include the serialized category reference, and aligns the
documentation with the shipped implementation and archived plan location.

## Scope boundaries

- Preserve chart data, references, OOXML serialization, selection behavior,
  axes, gridlines and legend behavior.
- Do not address the pre-existing pie/non-finite bug, the 512-point
  scale/render cap mismatch, or the stale `chart_grips` comment in this plan.
- Do not add OCR or golden-image infrastructure.
- Keep changes limited to `suite/docxy/src/main.rs`,
  `uiharness/tests/fixture.rs`, the affected suite documentation, this plan,
  and directly related tests.

## Acceptance criteria

- Retained horizontal category-label boxes cannot overlap when the forced last
  label produces a shorter final stride.
- Bar charts use vertical row capacity rather than the horizontal 56-pixel
  label-width rule; all four labels in the committed fixture remain visible.
- Fixture generator drift checks compare the stable serialized category
  reference as well as the existing chart fields.
- Documentation points to the archived plan, describes wide-chart unused
  category space accurately, and shows the actual single-cell reference text.
- Focused regression tests distinguish the triggering examples and relevant
  boundary cases.
- Root and suite tests, both Clippy invocations with `-D warnings`, both format
  checks, and `git diff --check` pass.

## Implementation steps

### Task 1: Make category-label geometry orientation-aware

- [ ] sweep every use of `chart_category_label_plan` and identify horizontal
      versus vertical capacity rules before changing the helper contract
- [ ] keep horizontal first/last retention while sizing each visible label box
      from adjacent retained-category centres so the shortened final stride
      cannot overlap its neighbour
- [ ] give bar category labels a vertical plan based on the rendered 12-pixel
      row height rather than the horizontal 56-pixel width heuristic
- [ ] test the 11-category/224-pixel `[0, 4, 8, 10]` case for non-overlap, the
      four-category bar fixture for full retention, plus zero/one/tiny inputs
- [ ] run the suite tests before Task 2

### Task 2: Strengthen fixture drift comparison

- [ ] enumerate stable serialized `ChartData` fields used by the generated and
      committed fixture and document why runtime-only `edited` is excluded
- [ ] compare loaded `categories_ref` content in
      `the_committed_fixture_matches_the_generator` without relying on the
      non-serialized runtime `edited` flag
- [ ] add or refine a test that fails when the category reference changes while
      cached labels and series stay the same
- [ ] run the UI harness fixture tests before Task 3

### Task 3: Correct the reviewed documentation contracts

- [ ] change the validation-record link to
      `docs/plans/completed/20260827-excel-chart-parity.md` and sweep for other
      live references to the moved plan
- [ ] describe the column-layout target gaps separately from unused space that
      remains when bars hit their maximum width
- [ ] update referenced single-cell series-name examples to the formatter's
      inclusive `:$B$1` endpoint form in both suite documentation and the
      completed plan, sweeping for equivalent shortened examples
- [ ] run focused documentation/search checks before Task 4

### Task 4: [Final] Verify and record the review fixes

- [ ] re-read all six Revmux findings against the resulting diff and confirm
      the mechanisms, not only the examples, are fixed
- [ ] run `cargo test` and `cargo test --manifest-path suite/Cargo.toml`
- [ ] run root and suite `cargo clippy --all-targets -- -D warnings`
- [ ] run root and suite `cargo fmt --check` and `git diff --check`
- [ ] record final counts and any deliberate limits in this plan, then move it
      to `docs/plans/completed/`

## Review record

- Initial report:
  `.revmux/tasks/ralphex-20260827-excel-chart-parity/01-initial/report.md`
- Actioned findings: three documentation corrections, two chart-label layout
  corrections, and one fixture drift comparison correction.
- Excluded findings: all entries classified by Revmux as `pre_existing` or
  `immaterial`.
