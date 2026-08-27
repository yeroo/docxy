# Spreadsheet chart-card preview

The spreadsheet suite renders each chart as a lightweight card over the grid.
It is a readable preview of the current `ChartData` caches, not an Excel chart
engine. The renderer and its pure layout helpers live in
`suite/docxy/src/main.rs` (`chart_scale`, `chart_card_layout`,
`chart_category_label_plan`, `chart_column_layout`, `chart_render_path` and
`chart_card`). None of these helpers changes chart references, cached values or
the OOXML written on save.

## Value scale and chart kinds

Column and line cards have a vertical value axis on the left; bar cards have a
horizontal value axis below the plot. Each uses three stable ticks at zero, the
midpoint and the positive maximum, with matching gridlines behind the marks.
Tick labels use compact `K`, `M`, `B` and `T` suffixes where useful. Pie cards
have neither numeric axes nor category axes and retain their per-category
legend.

The scale deliberately preserves the card's non-negative preview semantics:

- negative and non-finite values draw at zero;
- the top of the scale is the largest finite positive value, with a floor of
  one for empty, zero-only and fractional-only data;
- marks are clamped to the zero-to-maximum plot extent;
- the card does not attempt negative or mixed-sign axes, logarithmic or
  secondary axes, exact Excel “nice” tick selection, custom number formats or
  exact Excel rendering.

This is display behavior only. Negative values and every other imported value
remain in `ChartSeries::values`, and saving does not rewrite them to match the
preview.

## Reserved layout

`chart_card_layout` clamps invalid or tiny dimensions to non-negative
rectangles, then reserves disjoint title, plot, axis and legend regions. The
title receives at most 20% of the inner height. A wrapped legend is measured
from bounded label estimates and receives at most 35% of the space below the
title; the plot and axes use what remains. Column and line charts reserve the
left gutter for values and the bottom gutter for categories. Bar charts reserve
the left gutter for categories and the bottom gutter for values. Pie charts use
the body as their plot and reserve no axis gutters.

Horizontal category labels on column and line cards are planned from the
actual plot width. Bar-card category labels are planned from the plot height:
each retained label receives a fixed 12-pixel row, and dense categories are
thinned deterministically to rows that fit without overlap. Horizontal labels
are shortened by Unicode scalar values to a per-slot limit capped at 18
characters, while bar labels are shortened to the available category-axis
width. Shortened labels expose the full text in a tooltip.

Clustered columns are also sized from the plot width rather than a fixed bar
width. Each category owns one slot; within it, the target bar width is 2–24 px,
the target gap between series is 1–4 px, and the nominal target gap between
categories is 4–24 px. That category-gap target is also capped at 40% of its
slot, so it falls below 4 px when that cap is smaller. Once bars reach their
24 px maximum, any remaining slot width stays as unused space between clusters,
so the actual space can exceed that target gap on wide charts. Extremely dense
charts may compress below the 2 px target instead of overflowing their plot.
The renderer bounds an imported card to 512 points and 32 plotted series so
malformed caches cannot produce an unbounded element tree.

## Intentional limits

The card has four rendering paths: horizontal bar, line, pie and
column/default. It does not promise stacked or combo layout, negative axes,
secondary or logarithmic axes, animation, OCR/golden-image matching or exact
Excel tick and number-format parity. Those limits affect only the suite's card;
the chart model, references, cached data and untouched OOXML retain their own
round-trip behavior.

The pure helper tests in `suite/docxy/src/main.rs` cover empty, zero,
fractional and large scales; tiny, normal and wide cards; one and many
categories and series; category thinning/truncation; column geometry; and all
four kind paths. Live evidence and the full validation record are kept in
`docs/plans/completed/20260827-excel-chart-parity.md`.
