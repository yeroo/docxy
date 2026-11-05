# Project table

Notes moved from `CLAUDE.md` (#1903). Read them before changing Project's Entry table: columns, AutoFilter, the summary row, the prompt bar, selection and the row menu. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Project table: resizable columns and AutoFilter (#1533)

- Column widths are the plan's: `projcore::tableview::TableView` (px by field
  name, 24 to 600), `Project::table_view`, one undo step through
  `Editor::edit_table_view`, saved in the `.yppx` part `tableview.xml` (own
  content type, omitted when empty, never in MSPDI). It is not the issue's
  "views part": `views.xml` is an opaque retained part we do not own. A
  `tableview.xml` this build cannot read keeps default widths and stays a
  retained extra until a width replaces it. The suite's `ProjectView::widths()`
  is the drag's live width, else the plan's, else `DEFAULT_WIDTHS`; every
  width user (headings, cells, rules, scroll range, the default pane width,
  Best Fit) reads it, `TABLE_W` is only the default sum.
- A heading's right 6 px (`project-heading-edge:<Name>`) drag-resizes (window
  mouse move/up in `main.rs`, `WidthDrag`; one undo step on release) and
  double-click is Best Fit (`columns::best_fit`); the 16 px arrow left of it
  (`project-heading-arrow:<Name>`) opens the AutoFilter. The edge is drawn
  last so it wins; neither press selects the column.
- Sort and filters are view state on `Editor` (like `collapsed`; not saved, not
  undone, cleared by `replace_project`). `visible_rows()` stays in storage
  order (collapse only) and every edit acts on task indexes; what a grid draws
  and a cursor steps through is `Editor::display_rows()`: filtered (a summary
  stays while it or a task below passes; blank rows and UID 0 always pass),
  then sorted among siblings with each subtree kept together (text
  case-insensitive, numbers, dates by value, blanks last; UID 0 stays first).
  Insert above/below, indent and outdent are refused while sorted ("Clear the
  sort first"); filtered, they act normally. Paste, fill and range selection
  walk the displayed rows.
  One invariant keeps the cursor honest: after any operation the selection is
  a displayed task. `snapshot()`/`edit_structure()` capture the drawn rows
  (uids) before an edit (`Editor::pre_view`); `sel_into_display`, run by
  `changed()` and `toggle_level`, then takes the row drawn after the selection
  there that is still drawn, else the one before, by uid, so an edit that
  re-sorts the task cannot move the cursor to its new neighbours. With no
  capture (a filter just set, undo/redo) it takes the next displayed row in the
  sorted, unfiltered order. Delete uses the same rule (`reselect_near`), so a
  summary shown only through the deleted task drops out cleanly.
  `select_shown` is the post-edit select. The suite's cursor is the entry row
  when nothing is displayed or an edit's new task is hidden
  (`ProjectView::place_cursor`, `complete_project`); after a commit that hides
  the edited row, Enter/Tab keep the cursor where it landed, or go to the entry
  row when no drawn task took the row's place (`project_cell_input` compares
  the landing row with the pre-commit list). Find and Delete walk the drawn
  rows; yppxy's actions go through `shown_selected_uid`.
- The suite's drop-down is a dialog like the sheet's filter button
  (`project-filter-menu`, `DialogOwner::ProjectFilter`, `project/autofilter.rs`),
  not a `MenuTarget`, drawn as Project's popup (#1607, `Dialog::popup`): no
  title and an undimmed backdrop (it blocks the window like any modal), 250 px wide under the heading's arrow (`place`: flipped
  above when the room below cannot hold the menu and a usable list, clamped into
  the window both ways so OK / Cancel stay on screen; anchored on the frame on
  screen's `project-heading:<col>` probe, its left edge clamped into the table pane), `Apply` buttons as
  menu rows (icon, `▸`, rule above) in Project's order (Sort asc/desc, Group by
  ▸, No Group, Clear Filter, Filters ▸), the value list with a scrollbar, then
  OK / Cancel; a press outside presses Cancel and Escape closes it.
  `menu-read` and `menu-open` answer alike from the dialog (`items` with
  `{"separator":true}` entries, `controls`, `buttons` = OK / Cancel).
  Sort items (words by column type) apply and close, Group
  by, No Group and Filters are drawn disabled (follow-ups), `Clear Filter from <Field>`
  is enabled only while filtered, the checklist is the dialog check list (a
  Year > Month > Day tree on dates, two-state boxes), OK applies (all checked
  is no filter; none is refused), Cancel discards. `menu-open {"target":
  {"filter":"Finish"}}` opens it; `rows` reports `filter`, `filters`, `sort`,
  `sort_descending` and `widths`, the Project state reports `widths` and
  `filtered`; the status bar adds `AutoFilter Applied` while filtered.
- yppxy (terminal) has View > Data > Sort... / Filter... prompts, `:sort` and
  `:filter`, and the control verbs `proj.view`, `proj.sort`, `proj.filter`
  (`Editor::filter_texts` resolves a value, `(Blanks)` or `YYYY[-MM[-DD]]`);
  no mouse resize.
- `uiharness/cases/project-columns.uit` and `project-autofilter.uit` drive it;
  the sweeps run them (macOS cannot drive the window, so they were not run
  there).

## Project summary row (UID 0) is not a grid row (#1511)

- `Editor::visible_rows` omits `Task::is_project_summary` (UID 0, summary)
  unless the view flag `show_summary_task` is on (default off, no UI yet; it is
  view state like `collapsed`, bumps `revision`). Hosts and the harness's
  `rows` go through `visible_rows`, so the first task with UID != 0 is row 1,
  as in a plan without UID 0. The row stays in the model, in saves and in
  `task.list`; `rows` `total` still counts it.
- While hidden, UID 0 cannot be collapsed (`set_collapsed` refuses), and
  hiding it again expands it, so it never hides its tasks. A new session, and
  an edit that leaves the selection on it, start on the first shown row; an
  explicit `select` of it (automation) stays. `uiharness/cases/project-summary-row.uit`
  asserts the same layout with and without it (not run on macOS).

## Project prompt bar editing (#1370)

- The prompt bar (Predecessor, Move, Assign, Hyperlink, Find) edits at a caret
  like the cell editor: both embed `LineEdit` (`project/cell.rs`: `buf`,
  `caret`, `anchor`, `key`, `select_all`, `paste`, `copy_text`, `cut`,
  `scroll_x`) and deref to it. Left/Right/Home/End/Delete/Backspace move and
  edit, Ctrl+A selects the buffer, and typing or paste replaces a selection.
  Ctrl+C/X/V come back from `project_input` as acts and `project_act` hands
  them to `project_cell_clipboard`, which edits the open cell's, else the
  prompt's, buffer on the system clipboard (the cut read-back gate applies)
  and leaves the prompt open. Other chords are still dropped. Shift+arrows and
  click-to-place are not there. The state JSON still reports `kind:buf`.

## Project selection look (#1534)

- `suite/docxy/src/project/selection.rs`: one pure `Look` per frame
  (`ProjectView::look`, from `selection()` or the cursor) decides the Entry
  table's selection drawing, as Project 2024 does. A selected cell is filled
  itself (a wash of the accent, `accent::wash`, 0.30) with the cursor's 1px
  outline; the ID cell of every selected row and the heading of every selected
  column carry a lighter wash (0.16). A row selection (ID press, Shift+Space)
  is a range over all eight columns, so it fills the row; the table no longer
  highlights the whole cursor row (the chart side still does, and the entry
  row keeps its row background).
- A heading's left press selects the column (`project_column_select`, as the
  right press); the arrow and edge stop their own presses. Ctrl+A (whole
  table, cursor on the first cell, anchor on the last), Ctrl+Space (column)
  and Shift+Space (row, `row_header`) are in `project_input` before the Ctrl
  navigation; Ctrl+Home/End go to the first/last shown task's first/last
  column. Ctrl+Space may be the OS's input-source switch on macOS.
- Defaults: `DEFAULT_WIDTHS` sum 847 and `DEFAULT_TABLE_SHARE` 0.75 make the
  whole Entry table visible at the default 1180 px window, every column
  holding its heading beside the arrow and its usual cell text (unit tests pin
  both, the second from per-character estimates). The table
  scrollbar was already always shown. The harness `state` reports
  `selected_columns`, `selected_rows`, `filled_cells` (`"<row>:<column>"`),
  `hot_headings`, `cursor_border` and `table_fits`;
  `uiharness/cases/project-selection.uit` (not run on macOS).

## Project row menu and mini toolbar (#1537)

- `project_row_menu` lists Project 2024's task row items in its order, Notes...
  kept: Cut, Copy, Paste, Paste Special..., Scroll to Task, Insert/Delete/
  Inactivate Task, Manually/Auto Schedule, Assign Resources..., Text Styles...,
  Information..., Notes..., Add to Timeline, Link. Fill Down, Clear Contents
  and Font... are not row items (Ctrl+D stays; `ProjectAct::ClearContents` was
  removed, nothing else reached it); `Link` is `ProjectAct::Hyperlink` (the bar menu keeps
  `Hyperlink...`). Paste Special..., Text Styles... and Add to Timeline are
  disabled placeholders until #1389/#1390/#1391.
- `Menu::toolbar` (`menu::ToolbarItem`: an `Entry` plus `ToolbarKind`
  combo/button/arrow and a combo's `value`) is the mini formatting toolbar
  drawn above the row and column-heading menus (`project_toolbar`, opened by
  `open_menu_with_toolbar`); every other menu has none. `menu-read`/`menu-open`
  report `toolbar: [{id,label,kind,enabled,checked,value?,submenu?}]` (`[]`
  when absent). `menu-click {"label"}` or `{"path":["Font Color","Red"]}`
  finds a toolbar label first; a drop-down's choices replace the items
  (`Menu::enter_toolbar`, depth 1, the toolbar hidden) and Esc/Left backs out.
- Formatting items are `ProjectAct::CellStyle(StyleEdit)`: one
  `Editor::edit_formatting` over the selected cells (the range, else the
  cursor cell, real tasks only; the range survives), merged into each cell's
  direct `TextStyle`; a choice that changes no cell records no step.
  Bold/Italic set every cell to the opposite of the cursor cell's effective
  value (row categories under the cell's own, a summary bold), which is also
  what the toolbar's ticks and combo values show. The lists are fixed (8 fonts, sizes 8 to 72, 8 colours): the
  suite has no font enumeration. The task buttons dispatch AddLink,
  UnlinkTasks, Outdent, Indent and Task Details (`Constraint`).
