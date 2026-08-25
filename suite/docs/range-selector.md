# The range selector

Every input in the suite that asks for cells is the same input. Focus it and it
takes the keyboard; click or drag on the grid and it writes what you pointed at;
the cells it names are outlined while you work. This document
describes that field, the inputs that use it, and the reference syntax it
accepts.

Implementation: `suite/docxy/src/main.rs` (`RefTarget`, `RangeEdit`, `ref_field`,
`ref_commit`, `cell_at`, `formula_pick_to`, `formula_ref_tokens`, `edit_runs`,
and for the syntax `RefText`/`parse_ref_text`, `ref_a1`, `sheet_index_of`).

## Pointing

A range field is *pointable* when its target names cells rather than text
(`RefTarget::is_range`). While one is focused, the grid stops selecting and
starts pointing:

- **Press** plants the anchor on the cell under the pointer — the cell you
  pressed on, not the first one you move into.
- **Drag** grows the reference from that anchor; the field's text is rewritten
  on every move, so a drag produces one reference, not one per cell crossed.
- **Release** after a real drag commits the range straight away, the way
  dragging a new source range does in Excel. A plain click only writes the cell
  into the field — press Enter to commit it.
- A pick **replaces** the field's whole buffer. Nothing is appended.
- A click on the grid never *leaves* point mode — while a range field is
  focused the grid points, full stop. Escape gives the field up. (Ending point
  mode on a click instead would make the whole interaction depend on whether the
  pointer twitched between press and release: a one-pixel move inside the cell
  you pressed already goes through `sheet_drag_over` and picks.) That covers
  what a click would *otherwise* have done, too: a double-click doesn't open an
  in-cell editor and a hyperlinked cell isn't followed, both of which would act
  on a selection the click deliberately didn't move.

Every field that names cells **outlines** them while you type
(`range_preview`) — a dashed border in the brand teal, at Excel's border width
and in Excel's shape, drawn by the edge cells themselves (`range_edges_at`,
below). A series' values and the category labels need that most, since that is
where you can least afford to be wrong about what you picked. The pointed range
has no wash of its own: the dashed outline is unmistakable alone, and two
indicators for one thing is exactly the doubling this grid tries to avoid. The
same border draws the **selection** when no field is pointing and it spans more
than one cell — one code path, one look (`border_range`); a lone selected cell
already wears its ring, and the `range_tint` wash stays under it because a
multi-cell selection with nothing but one cell's ring says too little.

A pointed range that is scrolled off-screen is revealed (`reveal_range`), in
either direction: leftwards directly, rightwards on the next render, which is
the first point that knows how wide the grid is. The chart title isn't a range
(`RefTarget::is_range`), so its text is never parsed as one.

A reference naming **another sheet** gets no outline (`preview_range`):
bordering this sheet's `A1:D5` for a ref that means Budget's `A1:D5` would draw
the very
lie — same-named cells standing in for the ones actually read — that keeping the
qualifier exists to remove. The grid stays in point mode regardless, because
`GridOverlay::picking` reads the field's *focus* and never the preview, so a
drag can always re-point a foreign ref at cells you can see.

A gesture that has already claimed the pointer wins over point mode, and
`sheet_drag_over` tests for one first. A chart's resize grips straddle the
card's edge, so a resize drag is over ordinary cells from its very first move —
checked the other way round, that drag would rewrite the focused field with
whatever cells the pointer swept and `grid_release` would then commit it. For
the same reason the **auto-fill handle is hidden** while a field is pointable
(`GridOverlay::handle_hidden`): on the selection's bottom-right corner a drag
means "sweep a range", not "fill these cells".

⚠️ `cell_at` returns `None` on sheets with **frozen rows**: they render outside
the virtualized list, so the list's measured bounds can't locate a press there.
Those sheets fall back to the older behaviour — the drag anchors on the first
cell the pointer moves into. Frozen columns are fine; column hit-testing is
arithmetic over the same widths the renderer uses.

## Typing in a field

The field owns its own buffer — there is no `gpui-component` `InputState`
anywhere in this app — and receives keys through `sheet_key` → `range_edit_key`:

| Key | Does |
|-----|------|
| click an idle field | focus it and select all, so typing replaces the value |
| click / drag inside the text | place the caret · select a run |
| ← → Home End (+Shift) | move the caret · extend the selection |
| Ctrl+A | select all — the field takes this ahead of the sheet's select-all |
| Backspace · Delete | delete the selection, else one character |
| Enter | commit through `ref_commit` |
| Escape | abandon the edit, leaving the committed value |

`Ctrl+A` is the *only* chord the field takes. Every other one falls through to
the sheet even while a field is focused, so `Ctrl+C`/`X`/`V` copy, cut and paste
**cells**, `Ctrl+Z`/`Y` undo the **sheet**, and `Ctrl+S` saves — selecting text
in a field and pressing `Ctrl+C` copies the grid selection, not the text. For a
field inside a bar that means routing *past* the bar as well (`to_bar` in
`sheet_key`): the bar's own buffer takes plain typing, not chords, so a chord the
field declined would otherwise be dropped there rather than reaching the sheet.

Under the field sits whatever the last commit said about it — `"Applies to
Sheet1!$B$2:$D$5"` (the field's `=` dropped so it reads as a sentence, the
qualifier kept because it is what says *which sheet* the rule lands on), or
`"\"total\" isn't a range like =Sheet1!$A$1:$D$5"` — falling back to the field's
static help line when there's nothing to report (`ref_msg`, keyed by target so
fields can't show each other's messages). Every field words the refusal through
one function (`not_a_range_msg`), so no failure reads as a different *kind* of
failure depending on which field met it, and the example it holds up is spelled
with the sheet actually open (`Docxy::ref_example`) rather than a hardcoded
`Sheet1` the workbook may not have.

## Which inputs accept a range

One `RefTarget` variant per input, one `ref_commit` arm per variant:

| Target | Where | Commit does |
|--------|-------|-------------|
| `ChartRange` | Chart panel | replots the chart from the box |
| `ChartTitle` | Chart panel | plain text — **not** pointable |
| `SeriesName(i)` | series card | the field **shows the reference** (`=Budget!$B$1:$B$1`), not the name it resolved to, as Excel's Series name box does (`series_name_shown`); a series with no ref shows its literal name. A ref reads that **cell** — the top-left one, since the cache beside it holds a single point, and the ref is narrowed to it before anything is read — and is kept live; anything else is a literal name, and only if you **changed** the text, measured against whichever of the two was displayed (`series_name_commit`) |
| `SeriesValues(i)` | series card | re-reads **only** that series' numbers, from **one line** — one column on a column-oriented chart, one row on a row-oriented one (`series_values_shape_err`; see [`chart-orientation.md`](chart-orientation.md)). Excel splits a two-dimensional pick into a series per line and reads such a ref the long way, while `range_numbers` flattens row-major, so a wider pick is refused rather than written out in the wrong order |
| `Categories` | Chart panel | the category-axis labels, from **one line** of cells — one column on a column-oriented chart (`A2:A5`), one row on a row-oriented one (`B1:D1`), exactly as `SeriesValues(i)` follows the orientation (`categories_shape_err`). Labels name a series' *points*, and a series' points run down rows one way round and along columns the other, so the labels run the same way each derivation writes them. A **single cell** is one row and one column at once and goes through either way, which is what a row chart two columns wide has. A rectangle is refused by the same check on both orientations, for the reason that first motivated it: `range_labels` flattens row-major while Excel derives its own list from the ref itself, so the cache written beside the ref would contradict it the moment Excel refreshes. Taking either line on either orientation would also put the field at odds with `infer_by_row`, which reads a chart's orientation back *out* of the shape of its `<c:cat>` — see [`chart-orientation.md`](chart-orientation.md). `parse_chart` shuts the same door on import — a multi-level `<c:cat>` makes the chart `complex` rather than arriving in a state the field would not accept |
| `CondFormat` | Conditional Formatting bar | the cells the rule applies to |
| `Validation` | Data Validation bar | the cells the list applies to |
| `Sort` | Sort bar | the rows to sort |
| `TextToColumns` | Text to Columns bar | the cells to split |

The four bars *display* the current selection in their field until you pin a
range into it (`bar_target` → `bar_open` → `bar_seed`; see "The entry bars follow
the selection until you pin them" below), so leaving the field alone does exactly
what the bar did before it had one. Sort shows the *region it would find* —
header already dropped — for the same reason. At apply time `bar_cells()` is the
field's range when it names one, else the selection.

A bar owns the keyboard while it is open, so `sheet_key` asks its range field
first (`RefTarget::is_bar`) — otherwise what you type lands in the bar's own
buffer.

Every chart slot — `ChartRange` as much as `SeriesName`, `SeriesValues` and
`Categories` — **resolves** a reference naming another sheet (`chart_ref` →
`chart_ref_of` → `sheet_index_of`) and reads that sheet's cells: a chart floats
over one sheet and plots another's numbers all the time, which is the whole
point of a `<c:f>` carrying a sheet name. The `ChartSource` written back carries
the **resolved** sheet's name, so the ref persists into the file as a real
cross-sheet `<c:f>` rather than a bare box the next open would read as local.
A sheet the workbook hasn't got is refused by name under the field instead.

Re-pointing keeps the chart's `complex` flag (`chart_apply_range`): a stacked or
combo plot area still round-trips verbatim, and only **picking a type**
(`chart_set_kind`) says "author this one afresh" — which for a chart carrying no
numbers the writer could plot, an imported scatter or bubble, means exactly that:
the box goes back through `chart_from_range` (`chart_reauthored`) instead of the
chart being relabelled into one the save would empty. The box itself is **rebuilt**
rather than grown (`rebuild_source`): the fold starts empty and walks every slot
— each series' values (and, for a scatter or bubble, its `point_refs`, since
those kinds plot from `<c:xVal>`/`<c:yVal>` and carry no `<c:val>` at all), then
the categories, then the series' name cells — so the box shrinks as readily as
it stretches, and once every slot has moved to another sheet the box follows
them there. Growing the box it already had would strand it naming a sheet no
slot reads.

Within that fold the **first sheet wins** and a slot naming a different one is
skipped: `ChartSource::union` keeps the receiver's sheet, so unioning across
sheets would leave the box naming one and covering the other's cells, and
replacing would make the DATA RANGE field describe that one slot instead of the
chart — Enter on the field the user never touched would then replot everything
from a single foreign column. The **numbers** therefore go in **first**, and the
categories and the series' NAME cells only after them: both are labels rather
than data, both may legally sit on another sheet (`target_takes_foreign_sheet`
says so for CATEGORY LABELS and SERIES NAME alike), and folded first a single
foreign `<c:cat>` or `<c:tx>` would seed the box and get every local slot after
it skipped, collapsing a chart over `A1:D5` onto that one line. Folded after,
they only stretch the box the numbers chose — or seed it when there were no
numbers to choose it, which is why the categories go in before the names. The
loader folds in exactly this order and settles the clash the same way
(`parse_chart`, `fold_source`), so a chart reads identically before and after a
save.

Series can also be added, removed and reordered from the panel. A reorder closes
the gap behind the series rather than swapping it with its destination — the
arrows only ever send ±1, where the two agree, but the helper is written for what
it says. Both **rebuild the box**, like every other slot-mutating path: a
deleted series takes its slots with it, so the box shrinks off its column when
that column is at an EDGE of the box (the box is a rectangle, so deleting a
MIDDLE series leaves it exactly as wide — DATA RANGE goes on offering the
deleted column, and Enter there re-derives it); and a reorder changes which
values ref folds FIRST, which is what decides the sheet the box names. `parse_chart` rebuilds from the surviving refs in document order on the
next open either way, so skipping it would only make the panel read one way
before a save and another after it. A pie is no exception: it may hold
several series, `chart_space_xml` writes every one, and the panel says only the
first is drawn ([`pie-series.md`](pie-series.md)) — `+ Add series` used to
refuse there, back when the save dropped the extras. The
last series can't be removed (a chart with none is not renderable, and Excel won't let you
get there either), and a series' colour lives on the series, so it travels
through a reorder.

Which way round the box is read — each column a series, or each row — is the
chart's own `by_row`, flipped by the panel's `Switch Row/Column` button and
inferred from the refs when a chart is loaded. It decides what the DATA RANGE
field replots, what a series' values field will accept, and what the hints
under both suggest; it is written up in
[`chart-orientation.md`](chart-orientation.md).

## Reference syntax

Every range field **shows** a reference the way Excel writes one:
`=Budget!$A$1:$D$5` — a leading `=`, `$` anchors, and the sheet qualifier — so a
ref can be copied between this app and Excel's own dialogs and mean the same
thing in both. That is `ref_a1`, and every reference a field displays goes through it:
the chart panel's four slots, the four entry bars' seeds, and the text a drag
writes while it is in progress (`ref_pick_text`). The one thing a field shows
that isn't a reference is a series name that came from none — that is a literal,
and shows as itself (`series_name_shown`).

Input is deliberately **more permissive** than that output, as Excel's is.
`parse_ref_text` accepts, returning a `RefText { sheet, range }`:

- `A1:D5`, in any case, with surrounding space — `sheet: None`
- `C3` — a single cell is a one-cell range
- `$A$1:$D$5` — `$` anchors are optional and ignored
- `=A1:D5` — the leading `=` is optional
- `Budget!A1:D5`, `'My Sheet'!A1:D5`, `'Bob''s Data'!A1` — the qualifier is
  optional; quoting is optional when the name doesn't need it, and a `''` inside
  a quoted name unquotes to one `'`
- `D5:A1` — corners in either order name the same box

The split is on the **last** `!`: a quoted sheet name may contain one and the
cells never can.

What it refuses, beyond text that isn't cells at all: an **empty** qualifier.
`!A1:D5` and `''!A1:D5` are typos, not "this sheet" — Excel refuses both, and
taking them would land a reference on the sheet in front of you that pointedly
named none.

A qualifier is **kept, never dropped**. Whoever typed `Budget!A1:D5` gets
Budget's cells or a message saying there is no such sheet — never this sheet's
cells of the same name. (The old behaviour was the opposite: the prefix was
parsed off and discarded, so typing `Budget!A1:D5` while looking at Sheet2
silently plotted Sheet2's `A1:D5`.) Resolution is `sheet_index_of`, matching
**case-insensitively for ASCII names** because Excel does; `None` means the
sheet in front of you. The fold is `eq_ignore_ascii_case`, so a name outside
ASCII matches only at its own case: `бюджет!A1` does not find `Бюджет`. The
same fold decides `preview_range` (which delegates to `sheet_index_of`), the
sheet-name uniqueness check, and `bar_range_text` — the **preview** that spells
a reference back out — so changing it here alone would let resolution and the
preview disagree about one reference. It is not, however, universal: the
in-workbook hyperlink jump (`sheet_follow_hyperlink`) and a validation list's
range source (`dv_list_values`) still match a sheet name byte for byte, so
`=Budget!A1:A9` as a DV source finds nothing if the sheet is spelt `budget`.
That is a separate, older inconsistency this reference syntax didn't reach, not
a counter-rule. Two sheets differing only in case — which Excel forbids but a
hand-built file can carry — resolve to the first, in each of the folding
lookups above.

Whether a foreign sheet is resolved or refused is per target
(`target_takes_foreign_sheet`), and turns on what the field *feeds*, not on the
field:

| Target | Foreign sheet | Why |
|--------|---------------|-----|
| `ChartRange`, `SeriesName`, `SeriesValues`, `Categories` | resolved | a chart plots numbers that needn't live on the sheet it floats over |
| `Validation` | resolved | the rule is built while looking at the lookup sheet holding the list, and applies to the entry sheet holding the boxes |
| `CondFormat`, `Sort`, `TextToColumns` | refused | each acts on the rows in front of you — a rule paints *these* cells, a sort reorders *these* rows, a split rewrites *these* columns |
| `ChartTitle` | n/a | not a range at all |

The refusal is the pre-existing message, unchanged:
`"Budget" is another sheet; this acts on Sheet1`. It fires on a foreign name
whether or not that sheet exists — the objection is *where the bar acts*, not an
unknown name. A bar that resolves answers instead with the sheet actually found,
spelled the way the workbook spells it, so `budget!a1:a9` comes back
`=Budget!$A$1:$A$9` and the field stops disagreeing with the tab it names
(`bar_ref_text`; the sheet a bar acts on is then derived from its pinned range by
`bar_sheet_index`, never stored beside it, so the two cannot drift).

⚠️ The Validation bar's range field is the **applies-to** range, not the list
source — the bar's own text is a literal comma-separated list. What a qualifier
buys there is building the rule where the boxes are while looking at the sheet
holding the list. A range-valued list source is separate work.

Anything else is rejected with a message quoting what was typed. Multi-area,
whole-column (`A:C`), whole-row, 3D and structured references still work **in
formulas** — they just can't be built by pointing, and the outline skips them.
Cross-sheet refs join them there: a field takes one typed, but pointing only
ever writes the sheet you pointed at. Pointing next to one *inserts* rather than replaces
(`ref_token_at` declines a token preceded by `!`), so a click can never swing
`=Sheet2!A1` onto this sheet's cell behind your back.

`ref_token_at` is a *caret-local* scan while `formula_ref_tokens` walks the
whole buffer, but they apply the same four rules — a token followed by `(` is a
function name, one followed by `[` is a table name, one touching a `!` is the
wrong half of another sheet's reference, and anything inside `"…"` or `'…'` is
text (`quoted_at`). A test drives both over one corpus and asserts they agree:
every span the scan reports, the caret-local one claims from its end, and where
it reports nothing a pick inserts rather than replaces.

A chart field additionally caps what it will read at `MAX_CHART_CELLS`
(`chart_ref_of`). `A1:A1048576` parses perfectly well and would ask the
renderer for a million elements every frame, so the field declines it with a
count instead of hanging the window. The cap is weighed **before** the sheet is
looked up: a range too big to plot is too big on every sheet, and reporting a
missing sheet first would only send the user back to fix the same field twice.
The entry bars have no such cap — a
conditional format over a whole column is a normal thing to want. What *is*
capped is where the reveal can leave the grid: `reconcile_sheet_hscroll` clamps
`col0` to `MAX_VISIBLE_COL`, the same bound `sheet_el` renders to and `cell_at`
hit-tests to. Past it the grid would draw a window nothing could click, and —
since the wheel and the thumb move `col0` one column at a time from wherever it
is — appear frozen.

### Which form belongs where

There are four ways to spell a rectangle in this app and exactly one right
place for each. **A new range field uses `ref_a1`.** Anything else loses a
qualifier the moment the field is re-shown, which is the bug this syntax exists
to remove.

| Form | Example | Written by | Used by |
|------|---------|-----------|---------|
| Qualified, anchored, `=` | `=Budget!$A$1:$D$5` | `ref_a1` | every range **field** |
| Anchored, no sheet | `=$A$1:$D$5` | `ref_a1` with `sheet: None` | a field on a source that names no sheet (a chart authored before refs carried one) |
| Bare A1 | `B2:B5` | `range_a1` | a pick written into a **cell's formula** (`range_text`), where naming this very sheet is noise Excel doesn't write either. The name box shows the same *form* but builds its own text inline in `sheet_el` |
| `<c:f>` ref | `Budget!$A$1:$D$5` | `ChartSource::to_ref` | the OOXML writer |

`ref_a1` *is* the `<c:f>` form plus the leading `=`: it builds a `ChartSource`
and calls `to_ref`, so the sheet-name quoting rules live in the writer alone
rather than being re-implemented in the UI. `parse_ref_text` reads back
everything `ref_a1` writes, and a round-trip test pins that.

The one place the bare and qualified forms sit side by side is a drag:
`range_text` (bare, for a formula) and `ref_pick_text` (qualified, for a field)
normalise the same rectangle through the same `sel_range`, so what a drag shows
while it is in progress is what the field keeps when the mouse comes up.

Parsing is likewise one function. `parse_ref_text` returns the sheet and the
cells **separately** (`RefText`) instead of a bare rectangle, so no caller can
quietly drop half the answer; resolving the sheet half is then `sheet_index_of`
alone, wrapped as `Docxy::ref_sheet_index` for the view and reached from
`chart_ref_of`, `bar_ref_text` and `bar_sheet_index`.

### The entry bars follow the selection until you pin them

`bar_range` is `None` until you type a range into the field or point at one. Up
to that moment the field *displays* the live selection (`bar_seed`) and the bar
acts on it, which is exactly what Conditional Formatting, Data Validation,
Custom Sort and Text-to-Columns did before they had a field at all. Seeding the
field on open instead would freeze it: you would open the bar, drag out the
cells you meant, and Apply would still use whatever was selected beforehand.

A sort's seed is not the selection but the region it would find on its own
(`sheet_sort_bounds`), and only a *pinned* range overrides that.

The Apply button flushes the focused field first (`bar_flush`), so a range typed
but not yet Entered still counts. After a pick with the mouse the field keeps
the keyboard, so the next thing typed goes into the *range* — Enter or Escape
hands it back to the bar's own buffer.

⚠️ The four bars share **one** `bar_field`/`bar_range` pair, so only one may be
open at a time: `bar_open` closes the others (and `bar_close` closes the bars,
not just their fields). Two on screen would aim the first at cells pinned for
the second — `sheet_key` routes to whichever opened first, while `bar_seed` and
`bar_cells` read the slot the second one overwrote.

⚠️ `bar_open` also clears `range_edit`/`range_pick` outright, not just a field
belonging to a bar. A Chart panel field left focused keeps `range_field_active`
true, so the first drag meant for the bar would be committed by `range_pick_end`
→ `ref_commit` to the **chart** — replotting it — while the bar's own range
stayed unpinned and its rule landed on the untouched selection.

⚠️ And the other direction, which is not symmetric: the Chart panel renders off
`chart_sel` alone, so its fields stay clickable while a bar is open. `sheet_key`
asks the bars *before* a field that isn't one of theirs, so focusing a panel
field would draw a focused border and a caret while every keystroke went to the
bar — and a drag on the grid still rewrote and committed the chart's field. So
`ref_field`'s focus handler calls `typing_bars_close` for any non-bar target:
whatever swallows typing (the four bars, the comment/filter/row-height bars, the
find bar) loses it to the field the user just clicked.

## Pointing while typing a formula

Formula pointing shares the pointing service but not the field: the cell edit
already owns its buffer. It is active whenever the live edit buffer starts with
`=` (`formula_pick_active`), so ordinary cell editing is untouched — click a
cell while typing text and it still commits and moves, as before.

With `=SUM(` typed and the caret at the end, clicking or dragging cells writes
the reference in. Whether a pick **inserts** or **replaces** is `ref_token_at`:

- after an operator, a comma or an open bracket → insert
- inside or immediately after a reference → replace it
- a token followed by `(` is a **function name**, not a reference: `LOG10(`
  parses as column LOG row 10 otherwise, the same ambiguity Excel resolves this
  way
- a trailing `:` belongs to the reference under the caret, so pointing after a
  half-typed `=SUM(D2:` completes it to `=SUM(D2:D5` instead of leaving
  `=SUM(D2:D2:D5`

Each move during a drag re-splices from the buffer and caret **as they were at
the press**, so the drag rewrites one reference. The edit itself stays intact
throughout, so Enter and Escape still commit and cancel the formula, and the fx
bar and the in-cell editor read the same buffer.

## Reference colours

`formula_ref_tokens` scans the buffer once for every reference, returning where
each sits in the text and which cells it names. That one scan feeds both the
grid and the text, so they cannot disagree. It skips function names, other
sheets' cells (this grid can't outline them) and anything inside a string
literal.

Each reference gets a colour by index from a six-entry palette (`ref_color`,
wrapping past the end). On the grid its cells are outlined and lightly tinted in
that colour — kept distinguishable from the pointed range's dashed border. In
the text,
`edit_runs` splits the buffer at the caret *and* at every token boundary, each
run carrying its reference index, so a caret standing inside a reference splits
it without either half losing its colour, and clicking any run still places the
caret. A buffer that isn't a formula gets no colouring: `A1` typed as text stays
text.

References nest — `=SUM(B2:B5)/B3` covers B3 twice — and the two sides then have
to choose the same one. The text has no choice: a run belongs to the token it
sits in, the inner one. So the grid asks `ref_index_at`, which picks the
**smallest** covering reference (earliest index on a tie). Taking the first
covering one instead left the second reference with no cell anywhere in its
colour.

## What is selected, and what it reads

The grid answers *what is selected right now* in one voice: exactly one thing
looks selected at a time, and a selected chart says which cells it reads.

Implementation: `press_selection`, `SelectTarget`, `SelectionAfter`,
`cell_selection_shown`, `chart_source_areas`, `chart_slot_color`,
`chart_area_at`, `border_range`, `range_edges_at`, `chart_panel_after`,
`chart_panel_shown` — all pure free functions, all beside the grid geometry.

### One selection at a time

A chart card and a cell could both claim it, so `press_selection` decides which
one does, for every press and for the navigation keys alike:

- A press on a **cell** takes the selection back from any chart — its handles
  and its source outlines go, and the cell ring returns.
- A press on a **chart** takes it the other way. Pressing the *same* chart again
  is a no-op rather than a re-selection, or every press on a selected card would
  drop the panel field you were about to type in. A resize grip counts as a
  press on its chart, and the selection moves on mouse-**down**, as in Excel.
- **Navigation keys** (the arrows, Enter) are a press on the cells: they move
  the cell selection, so they hand it back first. Escape and Delete still belong
  to the chart.
- While **pointing** — a range field or a half-typed formula has the keyboard —
  a press on a cell writes a reference and changes *nothing* about what is
  selected. That is what lets the Chart panel's own range fields work: the chart
  being edited survives the clicks that edit it. Pressing another chart still
  swaps, pointing or not, because the focused field belongs to the chart being
  left (`drop_field`).

"Selecting a chart clears the cell selection" is implemented as *stops drawing
it*, not as clearing it. `SheetView::sel` is a `(row, col)` rather than an
`Option`, and every keyboard path, the Name Box and the formula bar read it;
making it optional would ripple through the whole grid to express something
nobody asked for. So `cell_selection_shown` → `GridOverlay::sel_hidden` turns
off every indicator keyed to the selection together — the ring, the
`range_tint` wash, both headers' highlight, the point-mode wash, the auto-fill
handle and the selection's own border — and dismissing the chart brings the ring
back exactly where it was, which is also what Excel does.

The one indicator deliberately still drawn under `sel_hidden` is the **pointed
range's** border. A selected chart's range fields point at cells; hiding it
would blind the very interaction the panel exists for. `border_range` therefore
takes the selection as an `Option`, so "the pointed range" and "the selection"
are told apart at the type rather than by a flag at each call site.

### What a selected chart's outlines mean

Selecting a chart outlines the cells it reads, each slot in its own colour:

| Outline | Colour | The cells are |
|---|---|---|
| `CHART_VALUES_COLOR` | blue `0x4472c4` | the numbers plotted (`values_ref`, and a scatter's or bubble's `point_refs`) |
| `CHART_CATEGORIES_COLOR` | purple `0x7030a0` | the category labels (`categories_ref`) |
| `CHART_NAME_COLOR` | green `0x00b050` | a series' name (`name_ref`) |

These are **Excel's mapping, deliberately**, and not the `ref_color` palette
above them. The two answer different questions: `ref_color` says "the Nth
reference of the formula you are typing", so its colours mean an *order* and
cycle once they run out; these three say what the cells *are* to the chart, and
anyone arriving from Excel knows them by sight. Matching Excel beats matching
docxy for exactly that reason — please don't unify them with the palette.

**Slots, not the box.** `ChartData::source` — the union the panel's DATA RANGE
shows — is *not* outlined. It is one rectangle around everything and answers
none of what selecting a chart asks: which cells are the numbers, which are the
labels. `chart_source_areas` walks the four slots the panel edits instead, so a
cell inside the box but in no slot (`A1` of an `A1:C5` chart) is drawn nothing.
`point_refs` is in that list because a scatter's and a bubble's numbers live
there and never in `values_ref`.

**Smallest wins**, the same rule the formula colours use, and now literally the
same function: `ref_index_at` and `chart_area_at` both call `smallest_ref_at`.
Two lists asking "who owns this cell" have to answer the same way, and a shared
rule cannot drift apart the way two copies would. It earns its keep here more
than for formulas, because the slots nest *by construction* — a series' name
cell is the header of the column its values read — so without it every green
name cell would be swallowed by the blue box it heads.

**Outline only, no wash.** A formula's references tint their cells as well;
these do not. A chart's sources are read while looking straight at the grid, and
a third wash over cells that may also carry `range_tint` is the doubled-up
indicator this work set out to remove. **Only the sheet in front of you** is
outlined, too: a ref naming another sheet gets nothing, exactly as
`preview_range` already refuses the wash. An unqualified ref means the chart's
own sheet.

Duplicate areas are folded — two series pointed at one cell would otherwise draw
the same box twice. The same cells in a *different* slot is not a duplicate:
both claims are real, and smallest-wins picks between them.

### The Chart panel is sticky

The panel used to be gated straight on `chart_sel`, so any click on the grid
closed it mid-edit. It now has its own state (`Docxy::panel_chart`), moved only
through `chart_panel_after`, and only four things move it:

- `Select(idx)` — a chart card was pressed; the panel swaps to it.
- `Deselect` — the chart lost the selection to the grid. The panel **keeps
  showing that chart**. This is the whole point: you glance at a cell, the chart
  deselects, and the fields you were about to click into are still there.
- `Dismiss` — the panel's `×`, or Escape. The two deliberate ways out.
- `Invalidate` — the chart list underneath changed. The panel **closes**.

That splits one question into two, and every site that read `chart_sel` belongs
to one of them:

- **Selected** — what is drawn on the cells: the card's handles, the source
  outlines (`chart_refs`), `sel_hidden`, and what Delete removes.
- **Shown** — what the panel *edits*: `chart_data`/`chart_set_data`, and through
  them every field commit, the type buttons and Switch Row/Column.

Getting that backwards is what would break the stickiness it exists for: a field
committed while the chart is deselected would find no chart to write to and
silently drop the edit.

**Gone means shut.** The panel must never show a chart that no longer exists, so
there are two independent routes to closing it. `Invalidate` fires from
`chart_drop_selection` — the choke point every list change already goes through
(delete, sheet switch, tab switch, undo, redo, insert) — and, as a second line
of defence, `chart_panel_shown(panel_chart, chart_count())` bounds-checks the
index at render. Both the render gate and the `sheet_grid_w` reservation ask it,
so the panel can never be drawn in a slot the grid also laid itself out over, or
reserve a slot it doesn't fill. Closing rather than clamping is deliberate:
clamping to the nearest surviving chart would leave the panel open on a chart
the user never selected, with a focused field about to re-point it. The panel is
sticky against **deselection**, never against the list moving underneath it —
that is the whole line between sticky and stale.

## Traps this rests on

Properties of the GPUI grid that this feature depends on. All are easy to undo
by accident.

**The virtualized `list` swallows child `on_mouse_down`.** Cells only ever see
`on_click` and `on_mouse_move`. That is why a press can't be handled by the cell
it lands on, and why drags used to anchor on the first cell the pointer *moved
into* — a papercut for selection, but a correctness bug for pointing. The fix is
a grid-level `on_mouse_down` that hit-tests the position itself (`cell_at`, →
`grid_press`): columns by summing `col_px(col_width(c))` from `col0`, rows from
`ListState::bounds_for_item` + `viewport_bounds`. Don't move anchoring back into
the cells; it will silently stop firing.

**Per-cell edge rendering is the only exact way to outline a range.** The range
outline, the fill handle and the formula ref outlines are drawn by the *edge
cells themselves* — an absolutely-positioned child, `deferred` to escape the
cell's overflow clip. The chart overlay's approach (reconstructing row positions
from a uniform row height) drifts on rows whose height comes from their content,
which is already visible on the sample sheet. Any new overlay that has to line
up with cells must use the per-cell technique.

**GPUI draws dashed borders, in the quad shader.** `grep -c dash` in
`gpui/src/style.rs` is 0, which is how this got recorded as impossible once
already — the wrong file. The setter is `Styled::border_dashed()`
(`crates/gpui/src/styled.rs:500`, on the `Styled` trait, so plain `div()` has
it), the enum is `BorderStyle::{Solid, Dashed}` (`crates/gpui/src/scene.rs:597`),
and the dashes are drawn in the quad shader on all three backends we ship
(`gpui_windows/src/shaders.hlsl:664`, `gpui_wgpu/src/shaders.wgsl:693`,
`gpui_macos/src/shaders.metal`). `PathBuilder::dash_array()`
(`path_builder.rs:108`) exists too, for stroked paths — not needed here, since a
quad border is cheaper and lays out with the cell. Line numbers are at the gpui
rev this workspace pins (zed `8276687`, per `suite/Cargo.lock`).

So **a dash is not an element**: a dashed border costs exactly what a solid one
costs, and there is nothing to trade for hand-rolling one. What the shader gives
us is not chosen, though, and `dash_fit` mirrors its arithmetic so the numbers
are derived rather than eyeballed:

- the pattern is dash `2W`, gap `1W` — pitch `3W`, derived from the border
  width, with no separate dash-length knob;
- dashes are laid out **per straight side, not around the perimeter**, and the
  side is made to start *and* end with a dash by reserving one dash's length and
  stretching the gap so the rest divides evenly;
- an edge of `4W` or less is painted **solid** — the shader's `dash_gap > 0.0`
  test fails and it silently skips dashing. At `RANGE_BORDER_W = 2` that is 8px,
  which only a clipped sliver of a column can be.

Because the layout is per quad, the dash phase restarts at every column
boundary: a long horizontal edge is a run of per-cell dash groups rather than
one continuous rhythm. Each group starts and ends flush with its cell, so the
seam falls on the gridline where the eye already expects one. That is the price
of per-cell rendering, and it is worth paying — see the drift trap above.

**The dashed border's cost is bounded by the viewport, not by the range.** The
grid only renders visible cells, so a range's border costs one quad per *visible*
boundary cell however large the range is: at the narrowest column (28px) and
shortest row (21px) a 1920×1200 grid shows ≈69 × ≈55 cells, so a full-row
selection is ≈69 quads, a full-column one ≈55, and select-all
(`A1:XFD1048576`) ≈123 — its perimeter is mostly off screen.
`RANGE_BORDER_CELL_CAP = 512` is the backstop: past it `range_border_plan`
reports `dashed: false` and **the same edges are drawn solid**. Degrading to
solid rather than to stuttering is the tested guarantee; on a hypothetical
ultrawide showing ~180 minimum-width columns the cap genuinely is reachable and
select-all there goes solid, which is the backstop working.

Only `sheet_el` knows the visible column window, so it decides the cap once per
frame (`range_border_dashed` → `GridOverlay::range_dashed`) and `sheet_row`
reads it. Two deliberate over-counts, both in the direction where the cap fires
*sooner*, never later: the frozen band and the scrolled window are counted as
one span, and the row side is bounded by `GRID_MAX_VISIBLE_ROWS = 128` rather
than measured, because `sheet_el` is handed the grid's width but not its height.
Don't raise that constant to be safe — two full columns at 256 would clear the
cap between them and drop an ordinary tall selection to solid.

**A drag can be released anywhere, so it must be ended everywhere.**
`sheet_dragging`, `drag_anchor`, `sheet_fill`, `chart_drag`, `formula_pick` and
`range_pick` are all armed by the grid but the button can come up over the
ribbon, over the Chart panel (which `stop_propagation`s mouse-up), or outside
the window entirely. They are therefore ended by one idempotent `grid_release`,
called from the grid, from the panel, and from the window root. Left on the grid
alone, a release over the ribbon leaves `sheet_fill` armed and the *next* drag
anywhere commits an auto-fill nobody asked for, undo entry and all.

**Anything holding a chart index has to be dropped when the chart list moves.**
`chart_sel`, `chart_drag`, `range_edit`, `ref_msg` and `range_pick` are bare
indices into *one sheet's* charts, resolved lazily by `chart_locate`. A sheet
switch, a tab switch or an undo re-points them at a different chart, so all of
them go through `chart_drop_selection`. Without it, selecting a chart on Sheet1
and clicking Sheet2's tab leaves the panel open and bound to Sheet2's chart 0 —
and Delete removes *that* one. The list itself moving counts too: `chart_locate`
resolves UI-authored charts *before* the file's drawings, so `sheet_insert_chart`
shifts every drawing-backed index by one and `chart_delete_selected` closes a
gap. Both call `chart_drop_selection` before they touch `v.charts`.

The same holds for everything else keyed to one grid — the open entry bar
(`bar_field`/`bar_range`), a fill drag, a formula's pick — so `drop_grid_state`
bundles the lot, and **every** path that changes the grid underneath them calls
it: `select_sheet`, `select_tab`, `sheet_add`, `sheet_delete`,
`sheet_insert_pivot`, `add_tab`, `close_tab`, `open_file`, `open_args`. Picking
a chart card is the narrower case of the same rule: `chart_press` drops the
panel's focused field when the selection moves to a *different* chart, since
`RefTarget::SeriesValues(i)` counts series within the selected one.

**Only four chart kinds can be written back.** Re-pointing a range marks the
chart `edited`, and an edited chart's part is *regenerated* from our model on
save — which is how a new `<c:f>` reaches the file at all. `chart_space_xml`
authors bar, column, line and pie; a scatter, area, doughnut, radar or bubble
chart run through it would come back as a clustered column chart, and an empty
one: `parse_chart` reads a scatter's and a bubble's
`<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` *refs* (they fold into the box and stay
on the series as `ChartSeries::point_refs`) but caches no numbers from them, so
the writer finds nothing to put in a `<c:val>`. The *plot area* has the same limit: `chart_space_xml` writes one
group, clustered (bar/column) or standard (line), so a stacked chart would come
back clustered and a combo chart — bars and a line sharing a plot area — would
fold every series onto the bar axis. `parse_chart` records that as
`ChartData::complex`, and `chart_is_writable` gates the regeneration on both:
those parts round-trip verbatim instead, and the panel says so under the type
buttons. The cost is that a rename or a row insert can't follow their refs
either — a stale ref beats a destroyed chart, and picking a type we can author
fixes both. Picking one is not always a relabel: a chart whose series still hold
nothing but points is *re-read* from its data range instead (`chart_reauthored`),
because relabelling that one would hand the writer the empty `<c:val>` above.
The note under the type buttons says which of the three the click will do —
relabel, re-read, or refuse — by making the re-read call itself rather than
guessing from whether the chart has a box.

Chart refs are now real refs in the file, so **they have to follow the grid**:
`structural_edit` runs every `ChartSource` through the same `span`/`point`
helpers as formulas and tables (`shift_chart_refs`), and the suite applies it to
the charts it authored, which live outside the workbook until they're saved. A
range whose rows or columns are wholly deleted loses its ref rather than keeping
a dangling one. Which refs are *ours* to shift takes two facts, not one: a
sheet-qualified ref matches by name, but an unqualified one means the chart's
own sheet — so `shift_chart_refs` takes a `home` flag saying whether the drawing
lives on the edited sheet. Without it, deleting rows on `Data` walked the refs
of a chart sitting on `Report`.

A structural one: the suite is a **separate cargo workspace**. `gridcore`
types are also built as literals by `xlsxy`, `gridwasm` and the TUIs in the root
workspace, and only `cargo build --all-targets` at the root notices when a model
change breaks them. A green suite build says nothing.

## Testing

gpui's headless harness can't compile this crate (see
[`../docxy/tests/README.md`](../docxy/tests/README.md)), so anything worth
testing is extracted as a free function and covered by plain `#[test]`s in
`main.rs`:

```sh
cargo test --manifest-path suite/Cargo.toml   # the pure helpers
cargo test -p gridcore                        # the chart model + xlsx round-trip
```

Covered that way: `parse_ref_text`, `range_a1`, `ref_a1`, `source_ref_text`,
`series_name_shown`, `ref_pick_text`, `sheet_index_of`, `ref_source`,
`chart_ref_of`, `rebuild_source`, `target_takes_foreign_sheet`, `bar_ref_text`,
`preview_range`, `sel_range`, `col_at_x`, `row_at_index`/`row_index_of`,
`series_remove`/`series_move`, `ref_token_at`, `replace_ref`,
`formula_ref_tokens`, `edit_runs`, `ref_color`, `ref_index_at`,
`sort_rows_from`, `bar_range_text`, `series_values_shape_err`,
`categories_shape_err`, `chart_field_examples`, `chart_plotted_series`,
`series_is_plotted`, `chart_unplotted_note`, `smallest_ref_at`,
`range_edges_at`, `range_border_plan`, `range_border_dashed`, `dash_fit`,
`border_range`, `chart_source_areas`, `chart_area_at`, `chart_slot_color`,
`press_selection`, `cell_selection_shown`, `chart_panel_after`,
`chart_panel_shown`.

That list is why every helper here is a **pure free function** taking the
workbook's sheet names as a `&[String]` rather than reading them off the view:
`sheet_index_of` is testable, `Docxy::ref_sheet_index` is the one-line wrapper
that isn't. Keep new ones on the same side of that line.

Everything else — input routing, point mode, the outlines — can only be checked
by driving the real binary: launch `suite/target/debug/suite.exe`, assert the
window owns the foreground **before sending any input** (without that guard a
stray drag lands in whatever window you are actually working in), then drive
Win32 mouse/keys and screenshot.

⚠️ `SendKeys` is unusable against gpui: its `+`/`^`/`%` modifiers arrive as real
shift presses and the shifted characters come out wrong (`=B2*C2+SUM(D2:D5`
types as `=B2*C2Sum9d2;d5`). Send virtual-key down/up pairs via `SendInput`
with shift held per `VkKeyScanW` instead.
