# Project Gantt chart

Notes moved from `CLAUDE.md` (#1903). Read them before changing Project's Gantt chart drawing: timescale, Timeline pane, gridlines, bar styles, layout, palette. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Timescale tier text styles (#1666)

- The Gantt header's two tiers draw by the plan's text styles, as its
  gridlines do (#1575): the upper (major) tier by `Middle Timescale Tier`,
  the lower (minor) by `Bottom Timescale Tier`; `Top Timescale Tier` is
  stored only until a three-tier header exists (the issue had Middle stored
  only). `plugin_ui::tier_styles` is `style_for(["All", <tier>])`: the tiers
  take `All`, deliberately (the issue asks it), while the titles style
  (#1665) does not. `tier_cell_style` puts the plan's style over the
  defaults (7.5 pt = the old 10 px, the major tier bold), so `bold: false`
  wins, and `header_cells` draws it through `styled` and `pattern_layer`.
- A cell's background or pattern covers the header's non-working shading
  there. A large size is clipped: the tier stays 14 px (Project grows it).
  Labels fit at `gantt::tier_char_w` px a character, from the drawn cell
  style: `zoom::CHAR_W` (6) is the bold 10 px text (the header row sets
  bold, so both tiers inherit it), scaled by the drawn size, a tenth
  narrower only for an explicit `bold: false`; a colour, italic or pattern
  style fits as the plain header.
- The harness state reports `timescale_styles` `{middle, bottom}` (`null`
  for an unstyled tier). The fixture's `fixture.format` sets both with its
  gridlines and clears them on the next press (`project-plugin-ui.uit`, not
  run on macOS).

## Project gridlines from the formatting model (#1575)

- `projcore::formatting::gridlines` is Project's Gridlines dialog as data:
  `Line` (the fourteen "Line to change" entries, Project's names and order),
  `LineType` (none, solid, dotted, short-dashed, long-dashed; `segments` is
  the one dash pattern: Project's own at 96 DPI, on/off px Dotted 1/1,
  Short dashed 1/3, Long dashed 4/4, #1759), `Gridline` (type, colour or Automatic, and for the
  row and column lines an interval with its own type and colour) and
  `Gridlines` (only lines set away from `Line::default_style` are stored:
  solid Automatic for Gantt Rows, Bottom Tier Column, Sheet Rows, Sheet
  Columns, Title Vertical and Title Horizontal, what the hosts drew before,
  and Current Date, solid as Project 2024 draws it (#1688; its green is not
  measured, so Automatic; the hosts draw it only where the plan's
  `CurrentDate` option is set, and a new docxy plan sets none); none for the
  rest, so a Current Date set to none is stored). The default moved without
  a format marker: a solid Automatic Current Date an older build stored is
  now the default and is dropped on the next save, and a plugin's `null` on
  Current Date now means solid; it sends `{"type":"none"}` to turn it off). `Formatting::gridlines` replaces the old
  opaque string; `formatting.xml` carries `<gridline>` children (an older
  root `gridlines` attribute is ignored). A plugin command's `view.gridlines`
  is an object of line name to `{type, color, interval, interval_type,
  interval_color}` (partial objects keep the rest; null is the default; a
  null reply clears all), and every request's `view.gridlines` carries all
  fourteen as drawn.
- The suite draws from it in `project/gantt.rs`: `body_grid` paints the
  table's Sheet Rows and Sheet Columns, the chart's Gantt Rows, the tier
  columns (`tier_lines`: bottom = the minor tier, middle = the major, the
  header's two tiers being Project's middle and bottom, so the Top Tier
  Column is kept undrawn like Bar Rows until a three-tier header exists;
  each line's interval index counted from the scale's origin so the pattern
  stays put as the chart scrolls; `bottom_tier_lines` follows the bottom
  tier's unit, so at the Weeks level it draws a line per week where the old
  chart drew one per day, and a bottom tier left at its default keeps the
  old rule of no lines under 8 px per day) and the date lines (`date_lines`: Current Date from the
  `CurrentDate` option, Project Start and Finish from the schedule, Status
  Date), each rule through `paint_rule` (`LineType::segments` as quads, an
  interval line's style on every Nth row or column, `gridline_color` falling
  back to the host's colour). The timescale's title lines are the header
  cells' borders (solid whatever the type, in the line's colour). Bar Rows
  (one bar row per task) and Page Breaks (no pagination) are kept, never
  drawn. The chart menu's Gridlines... item stays disabled: no built-in
  dialog yet. The harness state reports `gridlines` and `gridlines_set`.
- yppxy draws the date lines only (`gridline_marks`: `│` or `┆` in an empty
  day cell, in the line's colour), as a terminal cell is a day and a row a
  task. The fixture's `fixture.format` sets dotted red Gantt rows (every
  second solid blue), no sheet rows, green sheet columns and a red project
  start line, and clears them on the next press;
  `uiharness/cases/project-plugin-ui.uit` asserts the state, reads the green
  Sheet Columns rule off an unselected cell's right edge with a `border`
  assertion (the one step whose `#` colour is not a comment; an `is` value
  cannot carry one, so the colours are pinned by `projcore/tests/plugins.rs`)
  and shoots the rest (the default rules are too faint for a `border`).

## Bar style rows (#1696)

- `Formatting::bars` (`projcore::formatting::bars`) is Project's Bar Styles
  table for the view: `BarRow` (`name`, `show_for`, `row` 1 to 4, `from`/`to`,
  `start`/`end` `Mark`, `middle` `Middle`, five `BarText` fields) in table
  order, and `Formatting::task_bars` are Format Bar's `TaskBar` overrides per
  UID (a set part wins over every row drawn for the task; a part with nothing
  set, or `"reset": true`, removes the entry). Both are in the view JSON
  (`view.bars`, `view.task_bars`: absent keeps, null clears, `bars` an array
  replaces the table, `task_bars` entries replace per task) and in
  `formatting.xml` (`<bar>`, `<task-bar>`, fixed attribute order, absent when
  empty so an old plan saves byte-identically). Names stay as typed: an
  unknown shape, pattern or category is drawn as the default / never matches,
  a wrongly typed JSON value is ignored, `row` clamps to 1..=4
  (`Formatting::set_bars`). One `edit_formatting` is one undo step for both.
- `show_for` is a conjunction, as Project's comma list is (its default Task
  row is `Normal, Active, Not Manually Scheduled`): every listed category must
  match and no `Not` one may (`BarRow::applies`, `Facts::is`); an empty list
  shows for every task, and a category with no plan data never matches, so a
  row listing one shows for no task. A JSON array entry holding a comma list
  is split like the XML attribute. `Facts` is built by `Editor::bar_facts`
  (Normal, Milestone, Summary, Critical, Noncritical, Active, Inactive,
  Manual, Auto, External, Late = Status text, Marked = `Task::marked`
  (#1667), Flag1..20 read only when a row names one). The Task Path kinds
  have no plan data: they parse and never match. `Editor::bar_date` resolves From/To (empty = Start /
  Finish; `Task Start`/`Task Finish`; any `Field` date name; `CompleteThrough`
  = the task's `stop`, else start plus its percent complete of the span, which
  is unmeasured against Project); a task without the date draws no such row.
  `BarDateKind::of` says how a date lands on the day scale: start-type
  fields (Start, Actual/Early/Late Start, Baseline Start, Start1..10) are the
  start of their day and finish-type ones the end of it, so a Start-Finish row
  equals `gantt_bar`; `CompleteThrough` is an instant (the task's `stop`, else
  its start plus its percent complete of the working time to its finish on its
  calendar, kept within start..finish) placed within its day by
  `Editor::bar_day_fraction` (0 at the day's first working minute, or the
  bar's start on its first day, 1 at its last, or the bar's finish on its last, so 100% ends where the bar does and a stop at the end of day 1 of 2 is
  exactly one day; the weekend before it counts in the calendar-day frame as
  it does for the bar); every other date (Deadline, ...) is the instant
  (`instant_x`). Only a milestone task's row whose From and To are its own
  Start/Finish (or empty) and without a span sits in the middle of its day (as
  the palette's diamond does); any other row without a span is a point at its
  date by kind. `Facts::is` takes Project's own names too (`Manually
  Scheduled`, `Automatically Scheduled`, `External Tasks`). A bar's right-press
  is on each painted middle and mark (`hit_box`), not the span between.
- The suite draws it in `suite/docxy/src/project/barstyle.rs`: `bar_rows`
  (the plan against one task: `None` means the palette drawing, as with no
  table and no override; an override with no table restyles a plain leaf bar
  only, never a summary or milestone; that override keeps the palette bar's
  place, size, outline for an inactive task, and its baseline and delay marks,
  until it names a middle shape) -> `bar_parts` (pure geometry, tested)
  -> `styled_children`. A band is `ROW_H` tall, `row` picks the band; the bar
  zone is `BAR_Y`..+`BAR_H` (full), medium 8 px and narrow 4 px at its top,
  middle or bottom; marks are 10 px (solid, dashed outline, framed = filled
  with a black outline); the middle's pattern goes through `plugin_ui`'s
  `pattern_layer`; the five texts are 10 px (left/right beside the marks,
  top/bottom in the bands above and below the zone, inside clipped to the
  middle). `show_critical` does not apply to rows (the category decides);
  the palette's baseline and leveling-delay marks are not drawn for a task the
  table covers; an override over no table keeps them.
- `ProjectView::row_h()` is `ROW_H * max(1, max_bar_row)`: every task row,
  chart strip, pane, rule (`ruled_rows_of`), filler (`filler_rows_of`) and page
  (`page_rows_of`) uses it; the headers stay `ROW_H`. With no table everything
  is as before. `OverlayCtx.row_h` and `OverlayRow.bar` follow: the bar is the
  first drawn row's middle. The harness state reports `row_height` and
  `bars_drawn` (`<uid>:<row>:<name>` for the shown rows, in drawn order); the fixture's `fixture.bars` sets two
  rows and an override (and its overlay stops writing Resource Names when a row
  names them). Not drawn: terminal `yppxy` (it sends the JSON), the built-in
  Bar Styles / Format Bar dialogs, printing.

## Layout fields (#1695)

- `Formatting::layout` (`projcore::formatting::layout::Layout`) is Project's
  Layout dialog, Bars and Other sections: `bar_height` (points, 6 to 24,
  clamped on read; unset draws the built-in 14 px bar, which is not 12 pt =
  16 px), `rollup`, `hide_rollup_expanded`, `round_bars`, `show_splits`,
  `show_drawings` and `bar_text_date_format` (an opaque string, no validation).
  `view.layout` is an object (a key set to null clears it, unknown keys are
  ignored) or null (clears all); all keys are checked before any applies, so a
  wrong type (`view.layout.bar_height is not a number or null`) leaves the
  layout as it was. It is the `<layout>` element of `formatting.xml` (absent
  when empty, so an old plan saves byte-identically) and every state, overlay
  and command request's `view.layout` / `flags.layout` (all seven keys, null
  for unset), one undo step through `edit_formatting`.
- The suite reads it in `gantt_bar`: `GanttBar::geom` (`BarGeom`) carries the
  height and, with round bars off, the start and finish as fractional days.
  `bar_boxes(height)` is the one geometry: a leaf bar centred on y 12, a
  summary and a thin bar on y 14, a milestone's diamond on y 12, scaled by
  `height/14`, capped at the 28 px row and kept inside it (the default gives
  the original 5/14, 9/10, 11/6, 5/14). `gantt_strip`, the inactive outline,
  `bar_rect` (so the overlay's `OverlayRow.bar`) and the palette stand-in
  under an override with no Bar Styles table all read it. Round bars off
  places a bar on its instants (`span_x`) and a milestone's centre on its start
  instant. The Bar Styles table (#1696) is unchanged: with round bars off,
  table-drawn and override bars (and `OverlayRow.bar` for those tasks) keep
  their whole-day Start/Finish edges, and baseline and leveling-delay marks
  stay whole-day, so they no longer line up with a palette bar that starts or
  ends mid-day (known mismatches).
- Roll-up is `project/rolled.rs` (`rolled_parts`, per frame for the summary
  rows drawn): with `rollup` on and the summary collapsed, or expanded unless
  `hide_rollup_expanded`, the row also draws its descendant leaves and
  milestones (inactive ones skipped, nested summaries only through their
  leaves, rows an AutoFilter hides still roll up), 4s px bars and 8s px
  diamonds centred on y 14 (`s` = bar height / 14) with a 1 px panel outline,
  in the kind's palette colour (the view's critical toggle applies; a
  milestone child sits on a panel-coloured diamond one px larger). Under a
  Bar Styles table or an override roll-up is not drawn (nor listed in
  `rolled_bars`). The summary is painted first, its children over it. They take
  the place of a manual summary's own rolled-up strip (`GanttBar::rollup`, a
  different thing) while roll-up is on. The task's own `Rollup` flag (Task
  Information) is still not drawn.
- `show_splits`, `show_drawings` and `bar_text_date_format` are stored and
  round-tripped only (no `Task.Split`, no drawings, and Project's 21
  `PjDateFormat` samples are not in this repository). The harness state
  reports `layout`, `bar_rects` (`<uid>:<x>:<y>:<w>:<h>`, chart px from the
  first row's top) and `rolled_bars` (`<summary uid>:<children drawn>`); the
  fixture's `fixture.layout` sets and clears the object
  (`project-plugin-ui.uit`, not run on macOS). Not drawn: terminal `yppxy`,
  row height following the bar height, the Layout dialog itself.

## Palette manual and inactive bars (#1760)

- A palette's `manual` entry colours (and shapes) the bar of a manually
  scheduled task that is neither a summary nor a milestone: `GanttBar::manual`
  (set in `gantt_bar`) beside a `kind` that stays Critical/OnTrack, so the
  critical toggle and the overlay's `kind`/`critical` are unchanged.
  Without the entry the bar keeps its `critical` / `on-track` look. An
  inactive bar's outline takes the palette's `inactive` colour when there is
  one, in the shape it had. `bar_look_of` is the one look of a drawn bar
  (`gantt_strip`, `PaletteLook::of`); Bar Styles rows (#1696) still win, and
  rolled-up children keep their kind's look. `GanttBar::state` adds
  ` manual` before ` inactive`. yppxy draws no palette, so it has nothing to
  change.

## Project timescale and Timeline pane (#1535)

- `GanttScale` carries `day_w` (px per day); `DEFAULT_DAY_W` (22, the Days
  level) is only its initial value, and no geometry reads it. The view's zoom is
  `ProjectView::zoom_day_w` (`None` = default): view state like `split`, never
  saved or undone. Every change goes through `ProjectView::set_zoom(day_w,
  anchor_day, anchor_px)`, which clamps to 0.25..96, refreshes the scale and
  re-derives `gantt_x` from the anchor date; the plugin overlay's cache key
  includes `day_w`. `gantt_w` never depends on the zoom.
- `project/zoom.rs` holds the pure maths: `LEVELS` (Years, Half Years, Quarters,
  Months, Thirds of Months, Weeks, Days) with their (major, minor) `Unit`s,
  `nearest`/`step`/`fit`, and `tier`/`tiers` (header cells, labels fitted to
  their width). The header is two 14 px tiers inside the old 28 px row. Hours
  and Minutes are listed in the Timescale list and cannot be chosen (bars are
  whole-day geometry); day lines draw from 8 px/day and non-working days are
  shaded as merged runs from 3 px/day. Zoom In/Out step one level around the
  left edge; Ctrl+wheel (Ctrl on every platform; the rows' scroll is put back)
  steps around the pointer; Entire Project and Selected Tasks fit a span to the
  chart. A free width shows as `Custom (<nearest level>)`.
- Dragging a Timeline view-box edge (`BoxEdge`, 6 px strips drawn last, region
  `project-timeline-box-left/right`) moves that boundary and sets the zoom so
  the box's span fills the chart (`drag_edge`; the other edge stays put, also at
  the zoom clamp, except that a Left drag on a chart showing days past the
  scale's end moves its real right edge to that end while the drawn edge stays put); the middle still pans. The grip under the pane (`project-timeline-grip`)
  sets `ProjectView::timeline_h` (`MIN_TIMELINE_H` = one band plus padding, 60;
  at most half the window, `ProjectView::height`); a double-click restores 84.
  Like `split` it lives for the window session and is not written to disk. The
  Timeline's "Add tasks with dates to the timeline" placeholder now shows only
  for a plan with no tasks: it used to sit clipped below the bar row, which a
  taller pane would have exposed over a plan that has tasks.
- Ctrl+wheel: every delta sums in `wheel_acc` and steps one level per
  `WHEEL_STEP` (40 px); only a whole-line delta (|lines| >= 1) is a notch that
  steps at once, fractional lines (hi-res wheels, touchpads) sum, and touch
  phases Started/Ended clear the sum. The chart takes every Ctrl+wheel event
  over it (`WheelZoom::Absorbed`/`Stepped`) and puts the rows' scroll back, so a
  swipe never scrolls the rows. A view-box edge drag is measured against the edges as the box
  draws them (clamped to the Timeline's span) and moves within the span.
- The chart menu opens the Timescale and Zoom dialogs (`project/zoom_dialogs.rs`,
  `DialogOwner::Zoom`/`Timescale`, catalogue ids `zoom`, `timescale`). The
  Timescale dialog's tier rows are read-only (Count is 1): the tier editor and
  the non-working-time page are not built.
