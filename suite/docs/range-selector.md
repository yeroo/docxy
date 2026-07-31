# The range selector

Every input in the suite that asks for cells is the same input. Focus it and it
takes the keyboard; click or drag on the grid and it writes what you pointed at;
the cells it names are washed and outlined while you work. This document
describes that field, the inputs that use it, and the reference syntax it
accepts.

Implementation: `suite/docxy/src/main.rs` (`RefTarget`, `RangeEdit`, `ref_field`,
`ref_commit`, `cell_at`, `formula_pick_to`, `formula_ref_tokens`, `edit_runs`).

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

Under the field sits whatever the last commit said about it — `"Applies to
D2:D5"`, or `"\"total\" isn't a range like A1:D5"` — falling back to the field's
static help line when there's nothing to report (`ref_msg`, keyed by target so
fields can't show each other's messages).

## Which inputs accept a range

One `RefTarget` variant per input, one `ref_commit` arm per variant:

| Target | Where | Commit does |
|--------|-------|-------------|
| `ChartRange` | Chart panel | replots the chart from the box |
| `ChartTitle` | Chart panel | plain text — **not** pointable |
| `SeriesName(i)` | series card | a ref reads that cell and is kept live; anything else is a literal name — but only if you **changed** the text (`series_name_commit`) |
| `SeriesValues(i)` | series card | re-reads **only** that series' numbers |
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

Series can also be added, removed and reordered from the panel. The last series
can't be removed (a chart with none is not renderable, and Excel won't let you
get there either), and a series' colour lives on the series, so it travels
through a reorder.

## Reference syntax

Same-sheet rectangles in A1 form. `parse_ref_text` accepts:

- `A1:D5`, in any case, with surrounding space
- `C3` — a single cell is a one-cell range
- `$A$1:$D$5` — `$` anchors are accepted and ignored
- `Budget!A1:D5`, `'My Sheet'!A1:D5` — the sheet prefix is dropped, since
  pointing can't reach another sheet
- `D5:A1` — corners in either order name the same box

Anything else is rejected with a message quoting what was typed. Cross-sheet,
multi-area, whole-column (`A:C`), whole-row, 3D and structured references still
work **in formulas** — they just can't be built by pointing, and the outline
skips them. Pointing next to one *inserts* rather than replaces
(`ref_token_at` declines a token preceded by `!`), so a click can never swing
`=Sheet2!A1` onto this sheet's cell behind your back.

`ref_token_at` is a *caret-local* scan and, unlike `formula_ref_tokens`, knows
nothing about string literals: with the caret after `="A1`, pointing replaces
the `A1` inside the string. Half-typed formulas are the common case here and a
literal that looks like a reference is not, so the simpler scan wins.

A chart field additionally caps what it will read at `MAX_CHART_CELLS`
(`chart_range_of`). `A1:A1048576` parses perfectly well and would ask the
renderer for a million elements every frame, so the field declines it with a
count instead of hanging the window. The entry bars have no such cap — a
conditional format over a whole column is a normal thing to want. What *is*
capped is where the reveal can leave the grid: `reconcile_sheet_hscroll` clamps
`col0` to `MAX_VISIBLE_COL`, the same bound `sheet_el` renders to and `cell_at`
hit-tests to. Past it the grid would draw a window nothing could click, and —
since the wheel and the thumb move `col0` one column at a time from wherever it
is — appear frozen.

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

Covered that way: `parse_ref_text`, `range_a1`, `sel_range`, `col_at_x`,
`row_at_index`/`row_index_of`, `series_remove`/`series_move`, `ref_token_at`,
`replace_ref`, `formula_ref_tokens`, `edit_runs`, `ref_color`, `ref_index_at`,
`sort_rows_from`, `bar_range_text`.

Everything else — input routing, point mode, the outlines — can only be checked
by driving the real binary: launch `suite/target/debug/suite.exe`, assert the
window owns the foreground **before sending any input** (without that guard a
stray drag lands in whatever window you are actually working in), then drive
Win32 mouse/keys and screenshot.

⚠️ `SendKeys` is unusable against gpui: its `+`/`^`/`%` modifiers arrive as real
shift presses and the shifted characters come out wrong (`=B2*C2+SUM(D2:D5`
types as `=B2*C2Sum9d2;d5`). Send virtual-key down/up pairs via `SendInput`
with shift held per `VkKeyScanW` instead.
