# The range selector

Every input in the suite that asks for cells is the same input. Focus it and it
takes the keyboard; click or drag on the grid and it writes what you pointed at;
the cells it names are washed and outlined while you work. This document
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

Every field that names cells **washes and outlines** them while you type
(`range_preview`) — a series' values and the category labels above all, since
that is where you most need to see what you picked. A pointed range that is
scrolled off-screen is revealed (`reveal_range`), in either direction: leftwards
directly, rightwards on the next render, which is the first point that knows how
wide the grid is. The chart title isn't a range (`RefTarget::is_range`), so its
text is never parsed as one.

A reference naming **another sheet** gets no wash (`preview_range`): washing
this sheet's `A1:D5` for a ref that means Budget's `A1:D5` would draw the very
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
| `SeriesValues(i)` | series card | re-reads **only** that series' numbers, from **one column**: Excel splits a two-dimensional pick into a series per column and reads such a ref column-major, while `range_numbers` flattens row-major, so a wider pick is refused rather than written out in the wrong order |
| `Categories` | Chart panel | the category-axis labels |
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
(`chart_set_kind`) says "author this one afresh". Growing the box as a slot moves
only stretches it over the sheet it already names (`union_source`) —
`ChartSource::union` keeps the receiver's sheet, so unioning across sheets would
leave the box naming one and covering the other's cells. A slot re-pointed at
**another** sheet leaves the box alone rather than replacing it: replacing would
make the DATA RANGE field describe that one slot instead of the chart, and Enter
on the field the user never touched would then replot everything from a single
foreign column. The loader settles the same clash the same way (`parse_chart`
keeps the box it already has), so a chart reads identically before and after a
save.

Series can also be added, removed and reordered from the panel. A reorder closes
the gap behind the series rather than swapping it with its destination — the
arrows only ever send ±1, where the two agree, but the helper is written for what
it says. A pie takes one series and `chart_space_xml` writes only the first, so
`+ Add series` refuses there rather than listing one the save would drop. The
last series can't be removed (a chart with none is not renderable, and Excel won't let you
get there either), and a series' colour lives on the series, so it travels
through a reorder.

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
**case-insensitively** because Excel does; `None` means the sheet in front of
you. Two sheets differing only in case — which Excel forbids but a hand-built
file can carry — resolve to the first, as every other by-name lookup in the app
does.

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
that colour — kept distinguishable from the picked-range wash. In the text,
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
chart run through it would come back as a clustered column chart, and (since
`parse_chart` reads `<c:cat>`/`<c:val>` but never `<c:xVal>`/`<c:yVal>`) an
empty one. The *plot area* has the same limit: `chart_space_xml` writes one
group, clustered (bar/column) or standard (line), so a stacked chart would come
back clustered and a combo chart — bars and a line sharing a plot area — would
fold every series onto the bar axis. `parse_chart` records that as
`ChartData::complex`, and `chart_is_writable` gates the regeneration on both:
those parts round-trip verbatim instead, and the panel says so under the type
buttons. The cost is that a rename or a row insert can't follow their refs
either — a stale ref beats a destroyed chart, and picking a type we can author
(which clears `complex`) fixes both.

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
`chart_ref_of`, `union_source`, `target_takes_foreign_sheet`, `bar_ref_text`,
`preview_range`, `sel_range`, `col_at_x`, `row_at_index`/`row_index_of`,
`series_remove`/`series_move`, `ref_token_at`, `replace_ref`,
`formula_ref_tokens`, `edit_runs`, `ref_color`, `ref_index_at`,
`sort_rows_from`, `bar_range_text`.

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
