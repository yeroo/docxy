# Chart orientation — Switch Row/Column

A chart built from a worksheet range can read that range either way round. Given

| | A | B | C | D |
|---|---|---|---|---|
| 1 | Item | Qty | Unit price | Total |
| 2 | Laptop | 2 | 1199 | 2398 |
| 3 | Monitor | 4 | 249.5 | 998 |
| 4 | Keyboard | 6 | 39.99 | 239.94 |

`A1:D4` reads as three series *Qty / Unit price / Total* against categories
*Laptop / Monitor / Keyboard* — or as three series *Laptop / Monitor /
Keyboard* against categories *Qty / Unit price / Total*. Both are the same
numbers; which one is the chart is the user's call, and Excel puts that call
behind the `Switch Row/Column` button in its Select Data Source dialog. The
suite's Chart panel has the same button, under the type row.

Implementation: `ChartData::by_row` and `chart_from_range` /
`chart_from_columns` / `chart_from_rows` (`gridcore/src/sheet.rs`),
`chart_space_xml` (`gridcore/src/xlsx.rs`), `infer_by_row` and `parse_chart`
(`gridcore/src/drawing.rs`), and in the panel `chart_switch_row_column`,
`chart_switched`, `chart_switch_orientation`, `series_values_shape_err`,
`series_set_values` (`suite/docxy/src/main.rs`).

## What each orientation means

| | `by_row: false` (the default) | `by_row: true` |
|---|---|---|
| A series is | a column | a row |
| Series named from | the header cell above the column | the label cell left of the row |
| Categories from | the first non-numeric column | the first non-numeric row |
| `values_ref` shape | `$B$2:$B$5` | `$B$2:$D$2` |
| `ChartSeries::col` | the column index | `None` |
| `ChartSource::cat_col` | the column the LABELS come from | the column the series NAMES come from |

`by_row: false` is what docxy did before orientation existed and what
`ChartData::default()` still yields, so every existing struct literal and
`..Default::default()` site is unchanged in meaning. A test pins that default,
because flipping it would silently flip every chart in the suite.

Both readings are produced by one function, `chart_from_range(sheet,
sheet_name, range, kind, by_row)` — the same call the Insert button, the DATA
RANGE field and the Switch Row/Column button all make, so exactly one piece of
code decides what a range means and a flipped chart is indistinguishable from
one authored that way round in the first place.

### Why `col: None` for a row series

`ChartSeries::col` feeds two column-shaped decisions: the writer's fallback ref
(`src.f_ref(s.col?, s.col?, true)`) and `claimed_col`'s "has a series already
taken this column?". A row series occupies every column of its ref, so a row
index stored there would make both quietly **wrong** rather than inapplicable.
A row series always carries a `values_ref`, so the fallback is never reached.
`series_set_values` clears it for the same reason when a row series is
re-pointed.

## Orientation is derived, never stored

There is no orientation element in SpreadsheetML. Excel infers it from the shape
of the refs the chart holds, and so does docxy:

- **Saving writes nothing new.** `chart_space_xml` prefers a series' own
  `values_ref` via `ChartSource::to_ref`, which formats whatever rectangle it is
  given — `$B$2:$D$2` serialises correctly with no writer change. A test pins the
  exact `<c:f>` strings a row chart writes, and a second one pins that a
  column-oriented chart's output is **byte-for-byte** what it was before
  orientation existed.
- **Loading infers the flag** (below), so the button comes back in the right
  state.
- The flag on `ChartData` therefore exists for the panel and for re-derivation,
  not for the file.

### `<c:cat>` for a row chart

A chart that has a `categories_ref` writes it and that is that, either way
round. The interesting case is the **fallback**, which derives `<c:cat>` from
`source.cat_col` when there is no `categories_ref`: both halves of it ask a
column question (`f_ref(cat_col, cat_col)`, and `claimed_col`), and a row chart
has no column answer — `cat_col` names the *series-name* column there, so the
fallback would hand Excel a column of series names or of plotted numbers as the
category labels, and `claimed_col` would answer "yes" for every column in the
box, on a chart where that says nothing about the labels.

So the fallback **does not run** when `by_row`: a row chart with no
`categories_ref` writes its labels as literals (`<c:strLit>`), which is the same
answer the all-numeric table already gets in the column reading.

## How the loader infers orientation

`infer_by_row` (`gridcore/src/drawing.rs`) measures each series' `<c:val>` ref
and counts votes: one row across several columns votes **row**, one column down
several rows votes **column**. A row reading wins only if it is **unanimous** —
`rows > 0 && cols == 0`. Only when *nothing* had a shape of its own does the
arrangement of the single-cell series get a say (see below).

The ambiguous cases, decided explicitly so the next reader doesn't have to
rediscover them:

- **A single cell** (`$B$2`) is one row and one column at once. It fits both
  readings, so it votes for neither. A 1×1 chart therefore comes back
  column-oriented, and a round-trip test pins exactly that.
- **Several single cells stacked down one column** (`$B$2`, `$B$3`, `$B$4`) is
  the one case that looks ambiguous cell by cell and isn't, so it is **not** a
  default: it counts as row evidence. It is what a row chart over a range one
  label column plus one numeric column wide comes to — every row series is one
  cell — and the column reading cannot produce it, since that emits one series
  *per column* and two series therefore never share a column. Reading them as
  columns costs the load nothing — `parse_chart` builds one series per
  `<c:ser>` either way round — but it loses the orientation, so the chart comes
  back column-oriented and committing DATA RANGE, which re-derives the box the
  way the chart already reads it, folds the N one-point series into one
  N-point series. The button is *not* that path: it inverts `by_row` instead of
  keeping it, so on a wrongly-inferred chart the first press hands back the
  reading the chart should have had, appearing to do nothing, and only a second
  press folds.
  The mirror shape, single cells side by side **along** a row, is what a
  column chart one data row deep comes to, so it stays column-oriented.
- **A series with no readable ref** — `<c:numLit>` values, or a ref this model
  cannot hold (`Sheet1!$B:$B`, a defined name). There is no shape to measure, so
  no vote.
- **A rectangle spanning both ways** (`$B$2:$D$5`) is a shape neither reading
  produces — a hand-authored or foreign chart. No vote.
- **Series that disagree** — a chart this model cannot re-derive either way
  round. Column wins, being the safer of the two to hand the user, since
  `ChartSeries::col` and `ChartSource::cat_col` both assume it.
- **No votes at all** — no series, or none with a ref. Column, for the same
  reason.

The default in every ambiguous case is column-oriented, which is what **every**
chart written before this feature is. That is the backward-compatibility
guarantee, and it is guarded by a column round-trip test as well as the writer's
byte-for-byte one; treat a failure in either as a blocker rather than an
expectation to update.

`parse_chart`'s `cat_col` fixup — "whichever `<c:f>` landed in the box first set
`cat_col`, so correct it from `<c:cat>`" — is **skipped** for a row chart.
`categories_ref.range.1` is merely the left end of the label row, the first
*category's* column and never the labels' own, so the fixup would move `cat_col`
off the series-name column onto a plotted one. Left alone it keeps what the
first ref gave it, which is the series-name column — exactly what
`chart_from_rows` puts there.

## In the panel

**The button** sits under the TYPE row. Clicking it re-derives the chart from
its `source` box with the orientation flipped, through `chart_from_range`. It is
enabled exactly when clicking it would do something: `chart_switched()` is the
same call the click makes, so it can never look live and then do nothing. When
it is greyed, the note under it is `chart_switched`'s own error text, so it
always names the reason rather than a class of them. With a chart selected
those are:

- no `source` box to re-read (an imported chart whose refs the model can't
  hold);
- a box over the cell cap — this is the one chart range nothing has ever
  bounded, because it came from the FILE rather than from a field, and a
  `<c:f>` may legally name a whole column;
- a box naming a sheet the workbook hasn't got (one the user has since
  deleted or renamed);
- a range with no line of numbers the other way round.

Undo is `chart_set_data`'s existing snapshot. A flip always differs from what is
there (the orientation, if nothing else), so that call's "committed nothing"
early return can't swallow it, and a second snapshot would cost two undos per
click.

**What rides along a flip and what doesn't**: the title, the `part` and the
`complex` flag are kept, for the reasons `chart_apply_range` keeps them. Series
**colours are not** — the flipped series are different data (Laptop/Monitor/
Keyboard where they were Qty/Unit price/Total, usually not even the same number
of them), so matching by position would paint "Laptop" with the colour chosen
for "Qty". For the same reason hand edits to the plot do not survive a flip: the
range is the source of truth again. Flipping twice therefore returns the chart
the range describes, which **is** the original for a chart derived from its
range and never hand-edited.

A chart whose refs the model can only PARTLY hold is the one case where
"re-derive from the box" quietly loses a series: `parse_chart` unions the refs it
could read into `source` and marks the chart `complex` for the one it couldn't,
so the box covers less than the chart plots. Flipping such a chart redraws it
from that partial box, and the unreadable series is not in the picture any more.
That is not the button's doing — committing DATA RANGE re-derives from the same
partial box and drops it identically — and while `complex` stands the file is
untouched, because `chart_is_writable` vetoes regenerating the part. Picking a
type (`chart_set_kind`) is what clears `complex`, and that is the documented
"author this one afresh" escape hatch: after it the incomplete plot is what gets
written.

**Re-pointing a series** (`SeriesValues(i)`) checks the shape the *chart* wants,
not a constant: `series_values_shape_err` refuses `range.1 != range.3` on a
column chart and `range.0 != range.2` on a row one. The message names that
shape — a row chart says "a series plots one row — point at cells like B2:D2" —
because sending the user to `B2:B5` on a row chart is worse than not checking at
all, the range it asks for being one that would be refused again. The example
seeding the "that isn't a range" message flips with it.

`rebuild_source` needs no orientation of its own: it unions rectangles, so a row
series' ref grows the box the same way a column's does. A test pins that rather
than leaving it assumed.

## Testing

```sh
cargo test --manifest-path suite/Cargo.toml   # the pure helpers + the panel logic
cargo test -p gridcore                        # the model, the writer, the loader
```

The load→save→load path is where orientation actually lives, since nothing
stores it, so the round-trips are the tests that matter: a row chart that comes
back column-oriented is the primary failure mode, and a column chart that comes
back row-oriented is the regression. Both are pinned in
`gridcore/src/drawing.rs`, along with the awkward 2×2 range where one row and
one column are equally plausible readings.
