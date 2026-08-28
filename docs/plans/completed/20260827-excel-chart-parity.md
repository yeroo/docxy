# Excel chart and selection parity follow-ups

## Overview

Finish the user-visible items left from the August Excel comparison in the
suite spreadsheet UI:

1. selected and pointed ranges darken every affected row/column header in a
   neutral Excel-like grey instead of Docxy brand teal;
2. the Chart panel shows the values resolved from series-name and category
   references, beside the editable reference boxes;
3. chart cards gain a readable value scale, gridlines, collision-resistant
   category labels and bars sized for the available plot area.

This is display work. It must not change chart references, chart serialization,
selection ownership, formula-reference colours, or undo/package behavior. The
separate `SheetSnapshot`/`pkg.parts` audit remains a follow-on plan.

## Context

- `suite/docxy/src/main.rs`
  - `sheet_col_header` and `sheet_row` currently highlight the full selected
    range, but use `BRAND` with white text and do not let an active
    `range_preview` choose the highlighted headers.
  - `chart_panel` edits series-name/category references but does not show their
    cached resolved values beside those boxes.
  - `chart_card` already renders cached names/categories and simple bars, lines
    and a pie preview. Its plot has no value ticks or gridlines, column bars use
    a fixed 11px width, and category/legend layout can collide or clip.
- `suite/docs/range-selector.md` records the selection, reference and chart
  source rules that this work must preserve.
- `docs/ui-test-harness.md` and `uiharness/cases/sheet-selection.uit` provide a
  sandboxed live UI path. The harness deliberately has no OCR, so text layout
  logic needs pure helper tests plus a captured/manual live check.
- Existing tests are colocated in the `#[cfg(test)]` module at the end of
  `suite/docxy/src/main.rs`.

## Development approach

- **Testing approach:** regular — extract deterministic helpers, implement the
  UI against them, then add focused tests before moving to the next task.
- Complete and test one task before starting the next.
- Keep chart model and OOXML writers unchanged; use the already resolved
  `ChartData`/`ChartSeries` caches.
- Keep drawing work bounded: no animation, OCR, golden-image framework,
  secondary axes, negative-value redesign, or new chart kinds.
- Update this plan immediately when review or live verification changes scope.

## Acceptance criteria

- A normal `B2:D5` selection uses neutral grey styling on headers B:D and 2:5;
  headers outside the range retain the ordinary header style.
- While a reference field points at cells, the preview range controls the
  affected headers even when a selected chart hides the ordinary cell
  selection. Formula-reference colours and chart-source outline colours remain
  unchanged.
- A referenced series name shows its resolved name without replacing the
  editable `=Sheet!$A$1` text. Category labels show a bounded, readable preview
  including an omitted-count marker when truncated. Literal names do not get a
  misleading duplicate “resolved” row.
- Column and line previews show a value axis and horizontal gridlines; bar
  previews show a value scale appropriate to their horizontal direction; pie
  previews do not gain fake axes.
- Category labels are thinned/truncated deterministically when space is tight,
  first/last context is retained, legend space is reserved, and column bars use
  the available plot width rather than a fixed width.
- Existing selection, chart editing, save/round-trip and live harness behavior
  stays green.

## Implementation steps

### Task 1: Define neutral header-selection state

- [x] extract a pure helper that chooses the header range from ordinary
      selection, active range preview and `sel_hidden`; preview wins, a hidden
      ordinary selection contributes no range
- [x] define neutral selected-header background/foreground constants with
      enough contrast and no reuse of `BRAND`
- [x] update column and row header rendering to consume the same chosen range
      so their behavior cannot drift
- [x] test ordinary selection, pointed preview, chart-hidden selection, and the
      pointed-preview-while-chart-selected combination
- [x] run suite tests before Task 2

### Task 2: Show resolved values in the Chart panel

- [x] extract a Unicode-safe bounded label-preview helper that handles empty
      labels, truncation, omitted counts and a small configurable limit
- [x] show a referenced series name as a compact resolved value beneath its
      reference field; keep literal names single and editable without a
      duplicate row
- [x] show the chart's resolved category labels beneath the CATEGORY LABELS
      reference field, using the bounded preview and an explicit empty state
- [x] ensure previews are derived only from the current `ChartData` caches and
      update immediately after re-pointing, switching orientation or re-reading
      a chart
- [x] test referenced/literal series names plus empty, short, long and Unicode
      category lists
- [x] run suite tests before Task 3

### Task 3: Model chart-card scale and available space

- [x] extract pure chart-scale helpers for the current non-negative preview
      semantics, including stable zero/mid/max ticks and compact tick labels
- [x] extract a layout calculation for title, plot, axes and legend that clamps
      safely for tiny cards and reserves rather than overlaps those regions
- [x] calculate deterministic category-label stride/truncation while retaining
      first and last context
- [x] calculate clustered column-bar width/gap from plot width, category count
      and plotted series count with explicit usable minimum/maximum bounds
- [x] test zero/empty data, fractional and large values, one/many categories,
      one/many series, and tiny/normal/wide cards
- [x] run suite tests before Task 4

### Task 4: Render axes, gridlines and collision-resistant plots

- [x] render a left value-axis gutter and horizontal gridlines behind column
      and line marks, using the Task 3 scale
- [x] render the bar chart's horizontal value scale/gridlines without moving
      category labels into the plot
- [x] keep pie free of numeric axes and preserve its per-category legend
- [x] apply calculated column width/gaps and category-label thinning/truncation;
      add tooltips where visible text is shortened
- [x] constrain/reserve legend layout so it cannot cover the plot on small
      cards, while keeping every plotted series identifiable
- [x] add helper/invariant tests that distinguish all four rendered kind paths
      (`bar`, `line`, `pie`, column/default)
- [x] run suite tests before Task 5

### Task 5: Verify the live user-visible result

- [x] add only the minimal harness probe/state needed for deterministic checks;
      do not add OCR or a golden-image subsystem
- [x] run the existing five-case selection script and confirm it remains 5/5
- [x] run a sandboxed chart fixture, capture the grid, chart card and Chart
      panel, and record the evidence path in this plan
- [x] verify grey full-range headers, preview-driven headers, resolved panel
      values, axes/gridlines, label spacing and non-thin bars in the captures
- [x] run all root and suite tests, both clippy commands with `-D warnings`, both
      format checks and `git diff --check`

Live evidence, 2026-08-27:

- No new harness protocol was needed. Existing state keys (`range`,
  `range_preview`, `sel_hidden`, `chart_sel`, `panel_chart`) and the existing
  `window`, `grid`, `chart:0` and `chart-panel` regions cover the checks. The
  focused `uiharness/cases/excel-chart-parity.uit` case remains screenshot-only
  for text layout; the harness still has no OCR or golden-image machinery.
- The unchanged five cases in `uiharness/cases/sheet-selection.uit` passed 5/5.
  Evidence: `uiharness-runs/20260827-excel-chart-parity-selection-final/`.
- The range-backed chart fixture case passed 2/2. Evidence:
  `uiharness-runs/20260827-excel-chart-parity/`; the ordinary-selection window
  is `a-selected-range-darkens-every-affected-header/012-window.png`, and the
  pointed grid, card and panel are under
  `a-pointed-range-owns-headers-while-a-chart-is-selected/` as
  `028-window.png`, `029-grid.png`, `030-chart-0.png` and
  `031-chart-panel.png`.
- Inspection confirmed neutral grey B:D and 2:5 headers for `B2:D5`; while the
  chart hid that selection, only preview-driven A and 2:5 headers darkened for
  `A2:A5`. The card showed 0/20/40 scale ticks and gridlines, retained spaced
  first/last category context, readable-width bars and a separate legend. The
  panel kept `=Sheet1!$B$1:$B$1` and `=Sheet1!$A$2:$A$5` editable while
  showing resolved `Q1` and `North · South · East · West` values.
- `cargo test`, `cargo test --manifest-path suite/Cargo.toml` (199 passed), both
  root/suite `cargo clippy --all-targets -- -D warnings`, both root/suite
  `cargo fmt --check`, and `git diff --check` passed.

### Task 6: [Final] Document the finished behavior

- [x] update `suite/docs/range-selector.md` with the neutral header rule and the
      resolved-name/category previews
- [x] document chart-card layout/scale limits and the deliberate non-negative
      preview semantics near the renderer or in a focused suite document
- [x] record the final test counts, live evidence and any intentional deferrals
      in this plan

Final documentation and validation, 2026-08-27:

- `suite/docs/range-selector.md` now records the shared preview-first header
  range, neutral header colours and cache-only resolved series/category rows.
  `suite/docs/chart-preview.md` records the kind-specific axes, three-tick
  non-negative scale, reserved layout, label/column sizing and display-only
  limits.
- Final automated counts: root `cargo test` passed 2,323 tests with one existing
  ignored test; `cargo test --manifest-path suite/Cargo.toml` passed 199/199.
  Both root/suite `cargo clippy --all-targets -- -D warnings`, both root/suite
  `cargo fmt --check`, and `git diff --check` passed.
- Final live evidence remains the Task 5 run: the unchanged selection script
  passed 5/5 under
  `uiharness-runs/20260827-excel-chart-parity-selection-final/`, and the chart
  fixture passed 2/2 under `uiharness-runs/20260827-excel-chart-parity/` with
  the window/grid/card/panel captures listed above. Task 6 changed
  documentation only, so no replacement capture was needed.
- Intentional deferrals remain: side-by-side Excel and resize/long-Unicode
  manual checks; negative and mixed-sign, secondary/log and exact Excel axes;
  golden-image/OCR infrastructure; and the separate `SheetSnapshot`/
  `SheetPackage::parts` undo/redo audit. Chart references, model caches and
  OOXML/package behavior were not changed by this display work.

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*

## Technical details

### Header-range precedence

Use one answer for both axes:

1. an active `range_preview`, including while a chart hides ordinary selection;
2. otherwise the normalized ordinary selection when it is visible;
3. otherwise no highlighted headers.

This changes only the headers. The existing dashed border, formula washes,
chart-source outlines, active-cell ring and one-selection rule remain separate.

### Resolved panel previews

The editable fields remain references because that is what Excel's Select Data
Source dialog edits. The new rows explain what those references currently
resolve to. They must use cached `ChartSeries::name` and `ChartData::categories`
rather than re-reading a sheet during render.

### Chart-card limits

The suite card is a lightweight preview, not an Excel chart engine. This plan
adds a legible zero-to-positive-max scale and bounded layout without promising
negative axes, log scales, secondary axes, custom number formats or exact Excel
tick selection. Imported chart data and saved OOXML are not rewritten by these
display calculations.

## Post-completion

### Manual verification

- Compare the same selected range and chart side by side with Excel at normal
  Windows scaling.
- Resize a chart from its minimum size through a wide card and confirm axes,
  labels, marks and legend remain distinct.
- Check a chart with long Unicode category and series names.

### Deferred to separate plans

- Undo/redo coverage for raw `SheetPackage::parts` mutations.
- Negative and mixed-sign chart axes, secondary/log axes, exact Excel “nice”
  ticks and number formats.
- Golden-image/OCR infrastructure for text rendering.
