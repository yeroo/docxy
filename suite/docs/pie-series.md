# A pie keeps every series and plots the first

Give a chart three series and click **Pie**. All three stay: in the panel, in
the model, and in `xl/charts/chartN.xml`. One is drawn — the first — and the
panel says so, beside the type buttons and on each card that isn't plotted.

That is a change of answer. The panel used to **refuse** the click, because
`chart_space_xml` wrote `data.series.first()` and dropped the rest on save, so
every door to a multi-series pie was shut to stop the loss. The writer keeps
them now, so there is nothing to refuse and the doors are open.

Implementation: `chart_space_xml`'s pie arm (`gridcore/src/xlsx.rs`),
`parse_chart` (`gridcore/src/drawing.rs`), and in the panel
`chart_plotted_series`, `series_is_plotted`, `chart_unplotted_note`,
`chart_card`, and in `chart_panel` both the series list and the `CHART TYPE`
row (`suite/docxy/src/main.rs`).

## Several `<c:ser>` in one `<c:pieChart>` is legal

**ECMA-376 Part 1, DrawingML Charts (`dml-chart.xsd`).** `CT_PieChart` takes
its content from the group `EG_PieChartShared`, which declares

```xsd
<xsd:element name="ser" type="CT_PieSer" minOccurs="0" maxOccurs="unbounded"/>
```

so more than one `<c:ser>` inside one `<c:pieChart>` is **schema-valid**. Excel
plotting the first series only is a *plotting* rule, not a format rule — the
file may carry the others and Excel lists them under Select Data Source.

The citation sits in the code as well, beside the pie arm of
`chart_space_xml`, because that is where the wrong answer used to live: the arm
carried a comment reading "extra series are invalid (that's doughnut)", and it
is what justified deleting the user's work on every save. **Do not reinstate the
drop.** If a future reader wants a pie to hold one series, the format is not the
reason.

Real files do it. Of the 29 `<c:pieChart>` elements in the 555-workbook corpus,
four hold more than one `<c:ser>` and every one of them was written by
Microsoft Excel (`docProps/app.xml`):

| File | Part | `<c:ser>` |
|---|---|---|
| `corpus/xlsx-ext/openoffice/test/testgui/data/pvt/complex_29s.xlsx` | `xl/charts/chart3.xml` | 7 |
| `corpus/xlsx-ext/libreoffice/chart2/qa/extras/data/xlsx/chart-hatch-fill.xlsx` | `xl/charts/chart1.xml` | 2 |
| `corpus/xlsx-ext/libreoffice/chart2/qa/extras/data/xlsx/strict_chart.xlsx` | `xl/charts/chart1.xml` | 2 |
| `corpus/xlsx-ext/libreoffice/chart2/qa/extras/data/xlsx/tdf111173.xlsx` | `xl/charts/chart1.xml` | 2 |

The last is a **combo** part — a `<c:doughnutChart>` and a `<c:pieChart>` in one
`<c:plotArea>` — so it reads back as a doughnut and stays `complex` for the
unrelated `groups > 1` reason. Three of the four are freed by this; the fourth
is out of scope with doughnut rendering. It taught us one thing anyway: its two
series are `<c:idx>` **0 and 2**, so indices need not be contiguous and the
reader must not assume they are (`a_pie_whose_series_indices_skip_a_number_still_reads_as_two`).

## What the writer and the loader do

- **Writing.** The pie arm interpolates the same `{sers}` the bar, column and
  line arms do, so a pie emits one `<c:ser>` per series with sequential
  `<c:idx>`/`<c:order>` from `ser_xml`'s `enumerate`. A one-series pie's output
  is byte-for-byte what it was before, which is the case that must not move
  (`a_one_series_pie_is_written_exactly_as_before`).
- **Reading.** `parse_chart` already walked `<c:ser>` generically: it pushes one
  `ChartSeries` per element in **document order** and never reads
  `<c:ser><c:idx>` at all (the only `idx` it touches is `<c:pt idx=…>`). So the
  gapped indices above cost nothing.
- **No `complex` hold-back.** `parse_chart` used to mark a multi-series pie
  `complex` so its part round-tripped verbatim rather than being regenerated one
  slice group short. That term is gone with the reason for it: such a chart is
  now editable, and its refs follow renames and row inserts like any other's
  (`a_pie_that_arrives_with_two_series_stays_editable`,
  `a_seven_series_pie_reads_back_all_seven`).

The tests that matter here are round-trips, not writer-output assertions — the
defect was a save that lost data, and a test reading only the emitted string
would have passed while the loader still dropped series. See *Testing* below.

## What an imported pie trades for that

`chart_is_writable` is `chart_kind_is_writable && !complex`, so dropping the
term does not only make such a chart editable — it makes its part
**regenerable**, and `chart_space_xml` writes far less than Excel does. A series
carries `<c:idx>`/`<c:order>`/`<c:tx>`/`<c:spPr>`/`<c:cat>`/`<c:val>` and
nothing else, wrapped in a fixed `<c:varyColors val="1"/>` …
`<c:firstSliceAng val="0"/>`. Per-slice `<c:dPt>` fills, `<c:dLbls>`, the
legend's position, `explosion`, a non-zero start angle and any `<c:extLst>` are
gone the first time the chart is regenerated; before, that part was copied
byte-for-byte.

`chart-hatch-fill.xlsx` in the table above is the sharp case — the hatch fills
it is named for live in `<c:dPt><c:spPr>` and do not survive an edit. Nor does
it take a Chart-panel edit to trigger: `shift_chart_refs` and
`rename_sheet_in_chart` both set `edited` when a row insert or a sheet rename
moves a ref, so an edit elsewhere in the workbook is enough.

This is the ordinary trade every writable chart already makes (`SPREADSHEET.md`
§4a) — single-series pies included — and it is recorded here because this change
moved a class of chart from one side of it to the other. It is the right side:
the old behaviour did not preserve that formatting so much as freeze the chart,
and the moment the user did reach it through the panel the SERIES went, which is
the user's own work rather than the writer's. But "editable like any other" cuts
both ways, and the next reader should not have to discover which.

## What the panel says

`chart_plotted_series(kind, series)` is the single answer to "how many of these
reach the screen": `series.min(1)` for a pie, all of them otherwise. Everything
else derives from it, so the card and the panel can never disagree.

- **The plot** (`chart_card`) takes `nser` from it and slices `plotted` before
  measuring anything. `maxv`, `ncat`, every per-series loop and the pie arm's
  slice proportions therefore measure the **drawn** series only — an undrawn one
  can no longer stretch the axis or add an empty slice. Before, that was true by
  accident, because the writer had already thrown the extras away.
- **The series cards** — built inline in `chart_panel`'s series loop, at the
  card's `NAME` row — tag every unplotted one `NOT PLOTTED` beside that label,
  asked via `series_is_plotted`. Grep for `series_is_plotted` to find the site;
  there is no `series_card` function. Presenting all of them identically is
  exactly what made the old loss invisible.
- **The type row** (`chart_panel`) shows `chart_unplotted_note` under the
  buttons when a chart holds more than it draws: *"A pie plots the first series
  only — the other 2 are kept in the file but not drawn."* The sentence says
  **kept**, because they now are; a refusal would be the wrong thing to promise
  and a silent panel is what the bug looked like. A one-series pie, and every
  other kind, draw the lot, so the note is `None` and nothing is shown.

## The retired guard

`chart_kind_series_err` is **deleted**, along with every one of its call sites
and `series_add`'s inline equivalent (`data.kind == "pie" && n > 0`). Removing a
guard is the fix here rather than a regression: the state it refused is legal
(above) and no longer lossy, so refusing it only stopped the user doing
something the format and the file both allow. All five doors now go through:

| Door | What it does now |
|---|---|
| `chart_set_kind` — clicking **Pie** | keeps the series, rewrites the kind; picking the old kind back returns the chart intact. On a chart the writer would save empty it re-derives instead, through `chart_reauthored` — `chart_set_kind`'s second ask rather than a door of its own |
| `sheet_insert_chart` — Insert ▸ Pie | inserts over a range with several numeric columns |
| `chart_apply_range` — a wide DATA RANGE | re-derives a series per numeric column, pie included |
| `chart_switched` — the flip | allowed even though a flipped pie is usually one one-point series per category ([`chart-orientation.md`](chart-orientation.md)) |
| `series_add` — **+ Series** | pushes onto a pie like any other kind |

Each site carries a comment saying what the guard used to buy and why it stopped
buying it. `the_multi_series_pie_each_door_hands_over_is_described_not_refused`
covers four of them — the flip, `chart_set_kind`, `sheet_insert_chart` and
`series_add`'s push — asserting each yields the multi-series pie *and* that
`chart_plotted_series` / `chart_unplotted_note` describe it. Be precise about
what that pins: all four are `Docxy` methods, so a unit test cannot call them
for want of a constructed view whose Chart panel holds a chart (`panel_chart`,
not `chart_sel` — the two came apart when the panel went sticky, see
[`range-selector.md`](range-selector.md)) — `chart_set_kind`,
`sheet_insert_chart` and `series_add` take `&mut self` and a `Context<Self>`,
and `chart_switched` is `&self` because the render path calls it each frame to
grey the button. The test reaches only the PURE half each delegates to
(`chart_switch_row_column`, `chart_from_range`, and `series_add`'s push written
out). It pins the shape they produce and the words said about it — **not** that
they are unguarded; a refusal re-added inside one of those bodies would leave it
green. The one door that is a free function, `chart_reauthored`, IS walked, by
`picking_a_writable_type_authors_a_valueless_scatter_afresh`, whose pie arm now
expects a two-series pie where it expected an error. The save half of
`series_add` is `a_series_added_to_a_pie_survives_a_save`, across the crate
wall — the suite's tests cannot reach the writer.

**Why not guard the doors instead.** It is the smaller change, and it was
rejected twice over: it forbids something the format allows, so Excel can hand
us a workbook we refuse to represent — and doors keep appearing. The guard added
during the Switch Row/Column work covered three call sites and missed two within
one change. **Why not cap the model at one series for a pie.** That is the same
data loss moved earlier: the second series would vanish the moment the kind
changed, and Pie → Column would become destructive, which is worse than the bug.

## Deliberately not done

**Doughnut rendering.** Drawing multiple rings is a separate feature. This only
stops discarding the data a doughnut would later need; a `<c:doughnutChart>`
still round-trips verbatim.

## Testing

```sh
cargo test --manifest-path suite/Cargo.toml   # the panel's pure helpers
cargo test -p gridcore                        # the writer, the loader, the round-trips
```

- `gridcore/src/xlsx.rs` — `a_pie_writes_every_series_it_holds`,
  `a_one_series_pie_is_written_exactly_as_before`,
  `a_three_series_pie_survives_a_write_and_a_read`,
  `a_one_series_pie_survives_a_write_and_a_read`,
  `a_series_added_to_a_pie_survives_a_save`,
  `three_series_clicked_to_pie_survive_a_save_and_a_reopen` (the Overview
  scenario, at PACKAGE level — through `add_chart`, the regeneration gate, the
  zip and `load_xlsx`), `a_pie_converted_to_column_and_back_keeps_every_series`,
  `a_one_series_pie_reopens_unchanged`.
- `gridcore/src/drawing.rs` — `a_pie_that_arrives_with_two_series_stays_editable`,
  `a_pie_whose_series_indices_skip_a_number_still_reads_as_two`,
  `a_seven_series_pie_reads_back_all_seven`.
- `suite/docxy/src/main.rs` — `a_pie_draws_one_series_however_many_it_holds`,
  `the_panel_says_a_pie_plots_the_first_series_only`,
  `the_multi_series_pie_each_door_hands_over_is_described_not_refused`.

The one check that is **not** here is real Excel: build a 3-series pie, save,
and open it in Excel. It should open with no repair prompt, plot the first
series, and list all three under Select Data Source. A repair prompt would mean
the citation above is being misread, and is a reason to change the approach
rather than work around it.
