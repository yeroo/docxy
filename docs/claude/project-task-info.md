# Project Task Information

Notes moved from `CLAUDE.md` (#1903). Read them before changing Project's Task Information dialog and rich task notes. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Rich task notes (#1801)

- `Task::rich_notes` is the notes' `projcore::richtext` markup beside the
  plain `notes` every reader uses (MSPDI `<Notes>`, the grid, search): `Some` only when it formats something
  (bold, italic, a non-left alignment, a bullet), and then its plain text is
  `notes` (`richtext::split_notes` is the one rule). `Editor::set_rich_notes`
  sets both as one undo step (a style-only change is a step, the value held
  none); `set_notes` with new text drops the markup, with the same text
  keeps it, so a host rewriting unchanged notes loses nothing.
- It travels where the formatting does: `<note uid markup/>` children of
  `formatting.xml` after the formatting's own (`formatting::part_xml`, the
  one writer and "is there a part" test: newlines `&#10;`, tabs `&#9;`), so
  a `.yppx` save keeps it and a format plugin's `_ex` symbols carry it both
  ways with no new ABI symbol. A plan without rich notes writes the part
  byte-identically (or none). On read (`formatting::apply_notes`) a note
  lands only on a non-blank task whose plain notes it still describes; an
  older build's `Formatting::from_xml` ignores `<note>`.
- projctl `task.set {rich_notes}` takes the markup verbatim (never the
  one-line `notes` form), not with `notes` in one call; every task reply
  with notes carries `rich_notes` (the stored markup, else the plain notes'
  own markup, so plain text is never mistaken for markup). UI plugins read
  it from the read-only `view.rich_notes` (`{uid: markup}`, tasks with
  formatting only; `formatting::rich_notes_json`) in the suite's
  `view_json` and yppxy's `run_command`. The fixture's `fixture.richnote`
  (a command with no control) is the loop, `yppxy/tests/plugins.rs`.
- The suite's Task Information Notes page edits it (#1803): a filling
  `richtext` control prefilled with `rich_notes`, else the plain notes as
  `Rich::plain` (never parsed, so a note starting `<p>` reads as typed),
  written with `set_rich_notes` from its markup, so an edit keeps the
  formatting of what it did not touch. Enter is a new line there; the
  generated cases press OK for a multiline field (`accept_from`).
- Out of scope: resource and assignment notes, RTF (the plugin converts).

## Project Task Information dialog (#1368)

- Task > Properties > Information..., Notes... (same dialog, on its Notes
  page), the bar and row menus' items, and a double-click on a task's ID cell
  open `suite/docxy/src/project/task_info.rs`: the catalogued `task-information`
  dialog (General, Predecessors, Resources, Advanced, Notes). The entry row and
  a blank row open nothing. `PromptKind::Constraint` and `Notes` are gone; the
  prompt bar keeps Predecessor, Move, Assign, Hyperlink and Find. A double-click
  on a Gantt bar does not open it (the bar has only a right-press handler).
- Each field is the grid cell's own text (`cell_edit_text` to prefill,
  `apply_cell` to write), so Predecessors and Resources are one text field each
  in the column's grammar, not tables (until #1536, below). Notes is a rich-text
  box (#1803, under Rich task notes). Constraint type + date go through `Editor::set_constraint_typed`
  (the date resolved by `parse_stamp`); the
  date follows the type (ASAP and ALAP disable it).
- OK writes only the fields that changed (`Dialog::changed`), inside
  `Editor::batch`: one undo step, an unchanged OK leaves the plan clean, and a
  refused field applies nothing and keeps the dialog open with the message in
  the status line. Duration, Start and Finish of an auto summary, and
  % Complete of any summary, are disabled. A bare Deadline or constraint date
  is the day's last working time (FNLT, MFO, Deadline) or first (the other
  types), `YYYY-MM-DDThh:mm` takes a time, and the field shows a stored
  instant bare only when it is that boundary. What the field shows is what
  applies, also when only the type changed (`parse_stamp`): a bare date takes
  the new type's boundary, a time keeps its instant. Start and Finish together on an auto task
  are refused (each would replace the other's constraint). Task type and
  Effort driven apply before the rest, since a resource added in the same OK
  rescales by them. The new projcore setters (`set_percent_complete`,
  `set_priority`, `set_deadline`, `set_task_type`, `set_effort_driven`) are in
  `projcore/src/editor/props.rs`. The old `task-info` input of `inputs.rs` is
  gone; `uiharness/cases/project-task-info.uit` drives the dialog by hand.

## Task Information: top row, grids and date combos (#1536)

- Name, Duration and Estimated have `page: None`: one control each, drawn on
  every page. Estimated is the duration's `?`, not a value of its own: ticking
  it adds the mark to the Duration text, clearing it removes it, typing the
  mark ticks the box (`follow_estimated`, `react`), so only `duration` is
  written (a blank duration refuses the tick). General adds Schedule Mode
  (`set_manual`, applied first), Inactive (`set_active`, which refuses under
  an inactive summary), Display on Timeline, Hide Bar and Rollup
  (`Editor::set_display_on_timeline`/`set_hide_bar`/`set_rollup`, one undo step
  each, off when absent) and a Help button (`id == "help"`, drawn bottom
  left) that says what F1 says (`help_tab::DOCS_COMING`, #1022) and writes
  nothing. Display on Timeline, Hide Bar and Rollup are stored; nothing draws
  or reads them yet (the Timeline pane does not consult Display on Timeline). MSPDI has no Display on Timeline element, so it is saved only in
  the `.yppx` package's `timeline.xml` part (`projcore::timeline`, written only
  when a task states a value, an unreadable one kept as a retained extra like
  `tableview.xml`).
- `Control::editable_grid` makes a built-in grid interactive as a UI plugin's
  is (#1602): rows select, cells are typed into, Enter commits. It keeps
  exactly one blank row last (`keep_blank_row`, so an untouched grid equals
  the opened one and `Dialog::changed` is false), and Delete removes the
  selected row (a plugin grid's clears the cell). Column kinds: `Combo`
  (`Control::column_items`: typing narrows the list drawn under the grid, the
  arrows reach into it (`pick`), Enter takes the pick, a click on an item
  commits it, a text naming one item exactly (case aside) becomes it, any
  other text stays as typed: a new resource named Ann beside a Joanne is new;
  only a pick completes it), `Dropdown` (Space or a click steps the
  cell; typed text must name one item, or be contained in only one), and `Label` (read-only: `dialog-set`
  refuses it and typing starts nothing). All three apply to an editable grid
  only; a plugin grid's columns store what is typed. A cell being typed into
  is committed by Up/Down, a click elsewhere, Tab, a tab change and OK
  (`commit_pending`; a refusal keeps the edit and the dialog open). A pick
  carries its item's index (`Control::picked`), so of two tasks with one name
  the one picked is linked.
- `ControlKind::DateCombo` is typed as `Date`; its ▾ button, Alt+Down or F4
  opens `dialog::calendar::Calendar` (month grid, Sunday first) under the
  field: arrows move the cursor a day or a week, Page Up/Down a month, Enter
  or a click on a day writes `YYYY-MM-DD` into the field, Escape or Tab closes
  it (Enter and Escape are the calendar's while it is open,
  `DialogStack::key_button`). `dialog-read` reports `calendar_open` and
  `calendar` (`control`, `month`, `day`). `pointer-click` takes
  `dialog-calendar` (`button:<field>`, `prev:<field>`, `next:<field>`,
  `day:<field>:<n>`) and `dialog-cell` (`name[row].column heading in lower
  case`, or `name[suggest].<n>` for a narrowed combo list's item). A control
  with `beside` shares the row of the one before it (Duration/Estimated,
  Start/Finish).
- The Predecessors grid (`project/task_info/preds.rs`) has ID, Task Name,
  Type and Lag. A name fills the ID and an ID the name (`follow`, from the
  ID column's `column_items`, which line up with the names); OK reads the rows
  with the cell's own grammar (`parse_task_predecessors` for the ID, so a
  cross-project reference and `?uid` work; `parse_lag` for the lag) and keeps
  a link whose task, type and lag are as stored exactly as stored (lag format,
  cross-project fields) when its lag is spelled as shown or parses to the
  stored format (`16h` for `2d`, or `2d?`, is an edit). `set_predecessors` refuses a cycle, a duplicate or a
  self link, and the dialog stays open. The Resources grid
  (`project/task_info/resources.rs`) has Resource Name, Assignment Owner
  (`assn_owner`) and Cost read-only, Units typed; OK turns the rows into the
  Resource Names cell's tokens (`Ann`, `Bo[50%]`, `Cement[5 tons]`; 100% is the
  bare name) and applies them through `set_resources`, so a row left as shown
  changes nothing and a new name makes a resource. Rows without a name are
  ignored. The old text fields (`predecessors`, `resources`) are gone from
  the catalogue; the grids are not catalogued fields (`Grid` is not an editable
  kind). The `project-task-info.uit` cases were written without being run on
  macOS, like the others.
- Task Information's own top row (#1682) is `header: true` on `name`,
  `duration` and `estimated`: `Dialog::header_columns` leaves them out of the
  sum (in a `DialogOwner::TaskInfo` dialog only plugin header controls count),
  so the band is the plugin header's columns wide, one without any, and Name
  (`span` 255, clamped) fills it with Duration/Estimated on the next row (a
  `beside` control closes its row, so a plugin header control starts the
  next). The dialog has one `d.size` (`TASK_INFO_SIZE`, 633x520: Project's
  633x393 is too low for General, by estimate, not measured) on every page,
  so a plugin page's `fill` multiline fills it (`Dialog::fills`). What still
  does not fit scrolls: the Task Information page body is a scrolling
  container with a `Scrollbar` (`dialog-body`), and the Predecessors and
  Resources grids have `min_rows` 6 (empty rows drawn, more scroll). Known
  limits: the page-body scroll does not follow Tab focus or the open
  calendar, and a wheel over a nested scrolling grid or multiline also
  scrolls the page body when the page overflows.
