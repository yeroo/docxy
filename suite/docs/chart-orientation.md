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
`categories_shape_err`, `chart_kind_series_err`, `chart_field_examples`,
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

A scatter and a bubble have no `<c:val>` at all — their numbers arrive under
`<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` — so their `ChartSeries::point_refs` are
measured the same way and vote alongside. Without that every one of those charts
answered "column" by default however it was laid out, which stopped being just
the panel's reading once picking a type began *re-deriving* them
(`chart_reauthored`, below): a scatter along rows came back as N one-point
series. Only **multi-cell** point refs vote — a one-point scatter is two single
cells, laid out side by side or stacked down one column, and neither says
anything about orientation. The stacked layout is why the guard is needed rather
than merely tidy: without it those two cells satisfy every clause of the
stacked-cell rule below and the chart comes back `by_row`, to be re-derived the
wrong way round on a type click. Points the loader could not hold at all (a
`<c:numLit>`, a whole-column `<c:f>`) leave no shape to measure and are silent
here, like any other unreadable ref; `ChartSeries::points_unheld` records them
for the separate question of whether the chart can be relabelled, and its
narrower half `points_ref_unheld` for whether the box is short of the plot.

The ambiguous cases, decided explicitly so the next reader doesn't have to
rediscover them:

- **A single cell** (`$B$2`) is one row and one column at once. It fits both
  readings, so it votes for neither. A 1×1 chart therefore comes back
  column-oriented, and a round-trip test pins exactly that.
- **Several single cells stacked down one column** (`$B$2`, `$B$3`, `$B$4`) is
  the one case that looks ambiguous cell by cell and isn't, so it is **not** a
  default: it counts as row evidence. It is what a row chart over a range one
  label column plus one numeric column wide comes to — every row series is one
  cell — and the column *derivation* cannot produce it, since that emits one
  series *per column* and two series therefore never share a column. The model
  can still be walked into the shape by hand, though: `series_values_shape_err`
  refuses only a ref spanning several columns, so a user may point two column
  series at single cells one above the other. That is why the **categories are
  asked first**: labels running down a column are the column reading's, labels
  along a row are the row reading's, and `categories_shape_err` refuses the
  other line on either orientation, so nothing the panel commits can contradict
  what the loader reads back out of it.
  "Stacked" is a claim about ONE grid, so the cells must also name the **same
  sheet** — two single-cell series on different sheets are not above one another
  however their coordinates line up, and a chart with cross-sheet refs would
  otherwise flip orientation on a reload.
  The stacked-cell rule then decides only when the categories are **one cell or
  absent**. *One cell* is the genuine row case, whose labels *are* one cell when
  its range is two columns wide. *Absent* is not: `chart_from_columns` leaves
  `categories_ref` `None` whenever every column in the range is numeric
  (`Year | Sales`), so an all-numeric **column** chart whose every series has
  been re-pointed at a single cell still lands on the stacked-cell rule and is
  still read as a row. The residue is narrow — one series left with a multi-row
  ref votes column and the unanimity check settles it before the tiebreak runs —
  but the rule is a guess there, not a proof.
  Either way, the LOAD survives the wrong answer — `parse_chart` builds one
  series per `<c:ser>` whichever way round it reads — but the orientation does
  not: the panel comes back on the wrong reading, and committing DATA RANGE,
  which re-derives the box the way the chart already reads it, folds the N
  one-point series into one N-point series. That is the cost the rule is
  weighed against, and why stacked cells count as row evidence despite the
  residue. The button is *not* that path: it inverts `by_row` instead of
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
- **No votes at all** — no series, or none with a ref. The **categories** are
  asked next: a column of labels is the column reading's, a row of them the row
  reading's, and only a `<c:cat>` with no line shape of its own (one cell, or
  absent) leaves the answer column.

The default in every ambiguous case is column-oriented, which is what **every**
chart written before this feature is. That is the backward-compatibility
guarantee, and it is guarded by a column round-trip test as well as the writer's
byte-for-byte one; treat a failure in either as a blocker rather than an
expectation to update.

`parse_chart` says which column the box calls its label column **outright**,
rather than letting whichever `<c:f>` seeded the box decide it — the fold order
is about which *sheet* wins, and leaning on it for `cat_col` too made one answer
hostage to the other. A **column** chart takes `cat_col` from `<c:cat>`, where
the labels really live. A **row** chart cannot: `categories_ref.range.1` is
merely the left end of the label row, the first *category's* column and never
the labels' own, so it would move `cat_col` off the series-name column onto a
plotted one. It is taken from the first series' **name cell** instead, which is
that column — exactly what `chart_from_rows` puts there.

A chart with no `<c:cat>` ref at all has told us nothing, and `cat_col` then
falls back to a column some series plots. The writer's `claimed_col` guard is
what stops that being written out as a label ref: labels typed as `<c:strLit>`
came from nobody's cells, and handing Excel a ref to refresh them from would
replace them with whatever those cells hold.

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
  `<c:f>` may legally name a whole column. The count is taken TWICE, and the
  second one is of the WIDENED box (`chart_cells_within_cap` again, after
  `chart_box_with_header` below), so a box that fits by a line still refuses
  here and the number in the message can be a line more than the cell count
  DATA RANGE shows;
- a box naming a sheet the workbook hasn't got (one the user has since
  deleted or renamed);
- a box KNOWN to be short of the plot — a scatter or bubble naming point cells
  the fold took none of, either an `<c:f>` the loader refused or a held ref on
  another sheet. `chart_from_range` reads the box, so flipping would come back
  plotting one coordinate with the other silently gone; the re-author door
  refuses the same shape in the same words (`chart_points_off_box`);
- a points-leading box with no line to widen into — the same class of chart
  read the other way round: a box folded out of the points whose plot then
  starts at column A (or row 1), so there is no label column beside (or header
  row above) it for the flipped reading to name the series from
  (`chart_box_with_header`). The note says which line is missing and that
  inserting one, or pointing DATA RANGE at the cells to read, is the way out;
- a range with no line of numbers the other way round;
- a **pie** whose flipped range would read as more than one series, which is
  the usual case — see below.

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

There is one chart that pick cannot merely relabel. A scatter's and a bubble's
points arrive as `<c:xVal>`/`<c:yVal>` REFS with no cached numbers, so such a
series has nothing `chart_space_xml` could write a `<c:val>` from —
relabelling it `column` would make `chart_is_writable` true and the next save
would overwrite its part with a series of zero points. `chart_would_lose_points`
asks that per SERIES, because the writer writes each one independently: a
scatter whose first series a re-point gave a `values_ref` and whose second still
carries nothing but points would otherwise keep the half the user touched and
lose the half nobody did. One such series sends the whole chart through
`chart_reauthored`, which re-derives it from its own box through
`chart_from_range` — literally the "author this one afresh" the note promises —
or refuses, naming the shape this chart reads, when that box holds no numbers to
plot. A series with no points and no numbers is not that case: "+ Series" pushes
one, and it has nothing to lose, while re-deriving over it would discard the
hand edits on every other series — which is why the loader marks the series it
*could not read points for* (`points_unheld`), so a literal or whole-column
scatter is told apart from that empty one rather than relabelled.

The box handed to `chart_from_range` there is not taken on trust, because an
imported scatter's satisfies neither thing that call assumes of a box **it**
derived. Its leading line may not be a HEADER: `parse_chart` folds a scatter's
box out of the point refs — its numbers — and a series named by a literal
`<c:v>` (no `<c:f>`) leaves nothing to stretch the box up over a header row, so
it arrives as `A2:B4` where the authored equivalent is `A1:B4`.
`chart_from_columns` would then eat row 2 as headings and hand back a plot one
point short of the one on screen, so `chart_box_with_header` widens the box by a
line whenever a plotted ref reaches its edge, and refuses when there is no line
to widen into. And its ORIENTATION is `data.by_row`, which is why `infer_by_row`
reads point refs at all — the paragraph above.

Switch Row/Column re-derives from that same box, so it asks the same question —
for the orientation the FLIP is about to read it as, not the one the chart has:
a box that leads with a header row need not lead with a label column, and read
by row the scatter's `A2:B4` would have column A eaten as the series names.
Only a chart `chart_box_from_points` accepts is widened, which is the set whose
box was folded out of point refs, asked of the refs themselves rather than of
what a relabel would cost: re-pointing a series fills `values_ref` and leaves
`point_refs` where they are, so a half re-pointed scatter still has its box
sitting on its points and `chart_would_lose_points` would already have gone
false for it. Every other box is the one the user can see in DATA RANGE, and
pulling a line in beside it would move the field under their hands and stop a
double flip returning the chart the range describes — a `Year | Sales` box is
all numbers, so its first series starts in the box's own leading column and the
plotted-line test alone would fire on it. What the widening does not promise is
a way back to the imported scatter: the flip re-derives, and the chart that
comes out carries the box the FIRST flip needed rather than one the other
reading can use, which is the answer every hand edit gets here. Undo, not a
second flip, is what puts the scatter back. A flip made before the click
therefore survives it — but not because the re-read honours `data.by_row`. The
flip re-derives there and then, so every series comes back carrying a
`values_ref` and the chart has left the points-only class altogether; the later
click finds nothing to lose and merely relabels, which is also why the note's
second half stops being shown once a flip has landed.

Because the rest do not, the not-writable note says so: picking a type on a
chart that still holds nothing but points *re-reads* it from its DATA RANGE and
replaces the series below — names, colours, re-points, additions — keeping only
the title. Which of the THREE things a click can do turns on per-series state
the panel doesn't draw, so `chart_set_kind` also sets a status line saying the
re-read happened and with how many series.

The note picks between the three by MAKING the call the click would make
(`chart_range_sheet` + `chart_reauthored(…, "column", …)`, the way the Switch
Row/Column button already derives its flip every frame) rather than by a proxy:

- **A relabel** — re-point *every* series and the chart has left the points-only
  class, so the note stops after its first half ("are not saved until you pick a
  type above").
- **A re-read** — `chart_reauthored` comes back `Ok`, so the note adds that the
  click re-reads the chart from its data range and replaces the series below.
- **A refusal** — `chart_reauthored` comes back `Err`, so the note says the
  edits are not saved and that picking a type can't save them either until DATA
  RANGE names cells the chart can be re-read from. That is the branch a chart
  with no box at all takes: a scatter whose points and whose name are both
  literals folds nothing into `source`, and `chart_range_sheet`'s first line
  refuses without one (`CHART_NO_BOX`). It is also the branch for the four
  refusals that leave the box in PLACE — a plot half the box doesn't cover, a
  points-leading box with no line to widen into, a widened box past the cell
  cap, a box with no line of numbers — and for a box naming a sheet the workbook
  hasn't got.

`data.source.is_some()` used to stand in for that call, and was unsound in both
directions: it promised a re-read for every one of those five refusals, and with
no box at all it fell back to the bare first half — which claims the edits get
saved once a type is picked, on the one chart where picking a type can only
refuse. `"column"` answers for all four buttons, because the only kind-dependent
refusal is `chart_kind_series_err`, a pie-only count.

**Re-pointing a series** (`SeriesValues(i)`) checks the shape the *chart* wants,
not a constant: `series_values_shape_err` refuses `range.1 != range.3` on a
column chart and `range.0 != range.2` on a row one. The message names that
shape — a row chart says "a series plots one row — point at cells like B2:D2" —
because sending the user to `B2:B5` on a row chart is worse than not checking at
all, the range it asks for being one that would be refused again. The example
seeding the "that isn't a range" message flips with it.

**Re-pointing the categories** (`Categories`) follows the orientation for the
same reason the values do, plus one of its own. `<c:cat>` holds one LINE of
labels, and those labels name a series' POINTS — which run down rows on a column
chart and along columns on a row one — so `categories_shape_err` takes one
column (`A2:A5`) one way round and one row (`B1:D1`) the other, which is exactly
the shape each derivation writes. A single CELL is one row and one column at
once and goes through either way, which is what a row chart two columns wide
has. A rectangle differs both ways, so the same check refuses it on either
orientation, for the order mismatch that first motivated it: `range_labels`
flattens row-major while Excel derives its own list from the ref.

The reason of its own is the paragraph above: `infer_by_row` reads a chart's
orientation back OUT of the shape of its `<c:cat>` when no series has a shape.
Accepting either line on either orientation would put the panel and the loader
in contradiction — commit a row of labels onto a column chart and the file comes
back row-oriented, the VALUES fields refusing the very refs the series hold and
the next DATA RANGE commit folding N series into one.

`parse_chart` shuts the import door on the remaining shape — Excel's multi-level
`<c:multiLvlStrRef>` — by calling such a ref one this model cannot hold, so a
chart docxy derives never arrives in a state the field would refuse; see
[`SPREADSHEET.md`](../../SPREADSHEET.md) §4a. (A hand-authored file whose series
shapes decide the orientation *and* whose `<c:cat>` runs the other way is not
reachable through the panel and can still land there. Its label field then
refuses its own ref — the same asymmetry `series_values_shape_err` has always
had for a foreign chart mixing the two series shapes.)

**No pie the panel builds holds more than one series**, because
`chart_space_xml` writes only the first and the preview draws only the first, so
a second would be listed in the panel, pointed at cells, coloured, and then
dropped on save without a word. `chart_kind_series_err` is the rule all five
doors to that state apply:

- `series_add` — the "+ Series" button, which has always refused. It is the one
  door that does not call the helper: it tests `n > 0` before pushing, which is
  the same rule on the count the push would leave, and keeps its own wording
  because it is about to add a series rather than commit a whole plot.
- **Switching** a pie — the flip usually reads N categories as N one-point
  series. Asked of the DERIVED plot, so the button greys out with the reason
  under it rather than failing at the click. Its note is worded for that door —
  the count belongs to the flip, not to the chart on screen.
- `chart_apply_range` — a wide DATA RANGE on a pie is the same hole by another
  door.
- `chart_set_kind` — picking **Pie** on a chart that already has N series. It
  usually does not re-derive; it keeps the series and rewrites the kind, which
  is why guarding the re-derivations alone left it open. It is also the widest
  door, being what a user reaches by clicking the word *Pie*. It asks twice: the
  one chart it does re-derive (the paragraph above) comes back with a series per
  numeric column of its box, so `chart_reauthored` re-checks the rule on that
  count — and its refusal can then name a count the panel is not showing.
- `sheet_insert_chart` — Insert ▸ Pie over a range with several numeric columns.

(A file that arrives already holding a multi-series `<c:pieChart>` is not
reachable through the panel — the schema permits one even though Excel's own UI
will not author it. `parse_chart` marks such a chart `complex`, so it round-trips
as Excel wrote it instead of losing its extra series on the next edit, the same
escape hatch a stacked or combo plot area takes.)

All five refuse rather than silently keeping the first, which is the answer
`series_add` has always given and the only one that cannot lose work the panel
is already showing.

`rebuild_source` needs no orientation of its own: it unions rectangles, so a row
series' ref grows the box the same way a column's does. A test pins that rather
than leaving it assumed. It does care about ORDER, though — the series' values go
in before the categories and the name cells, so that one cross-sheet label ref
cannot take the box off the sheet the chart plots — and `parse_chart` folds in
that same order, which is what keeps the box identical before and after a save.

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
