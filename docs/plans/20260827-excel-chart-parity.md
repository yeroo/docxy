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

- [ ] extract pure chart-scale helpers for the current non-negative preview
      semantics, including stable zero/mid/max ticks and compact tick labels
- [ ] extract a layout calculation for title, plot, axes and legend that clamps
      safely for tiny cards and reserves rather than overlaps those regions
- [ ] calculate deterministic category-label stride/truncation while retaining
      first and last context
- [ ] calculate clustered column-bar width/gap from plot width, category count
      and plotted series count with explicit usable minimum/maximum bounds
- [ ] test zero/empty data, fractional and large values, one/many categories,
      one/many series, and tiny/normal/wide cards
- [ ] run suite tests before Task 4

### Task 4: Render axes, gridlines and collision-resistant plots

- [ ] render a left value-axis gutter and horizontal gridlines behind column
      and line marks, using the Task 3 scale
- [ ] render the bar chart's horizontal value scale/gridlines without moving
      category labels into the plot
- [ ] keep pie free of numeric axes and preserve its per-category legend
- [ ] apply calculated column width/gaps and category-label thinning/truncation;
      add tooltips where visible text is shortened
- [ ] constrain/reserve legend layout so it cannot cover the plot on small
      cards, while keeping every plotted series identifiable
- [ ] add helper/invariant tests that distinguish all four rendered kind paths
      (`bar`, `line`, `pie`, column/default)
- [ ] run suite tests before Task 5

### Task 5: Verify the live user-visible result

- [ ] add only the minimal harness probe/state needed for deterministic checks;
      do not add OCR or a golden-image subsystem
- [ ] run the existing five-case selection script and confirm it remains 5/5
- [ ] run a sandboxed chart fixture, capture the grid, chart card and Chart
      panel, and record the evidence path in this plan
- [ ] verify grey full-range headers, preview-driven headers, resolved panel
      values, axes/gridlines, label spacing and non-thin bars in the captures
- [ ] run all root and suite tests, both clippy commands with `-D warnings`, both
      format checks and `git diff --check`

### Task 6: [Final] Document the finished behavior

- [ ] update `suite/docs/range-selector.md` with the neutral header rule and the
      resolved-name/category previews
- [ ] document chart-card layout/scale limits and the deliberate non-negative
      preview semantics near the renderer or in a focused suite document
- [ ] record the final test counts, live evidence and any intentional deferrals
      in this plan

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
