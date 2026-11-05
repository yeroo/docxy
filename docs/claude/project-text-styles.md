# Project text styles

Notes moved from `CLAUDE.md` (#1903). Read them before changing Project's text styles and the row categories they apply to, including the model's Marked, Highlight and changed cells. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Text style strikethrough and pattern (#1664)

- `projcore::formatting::TextStyle` has `strikethrough: Option<bool>` and
  `pattern: Option<String>`, carried by the view JSON (`text_styles`, `cells`),
  `formatting.xml` (`strikethrough="1"/"0"`, `pattern="..."`) and `over()`.
  `PATTERNS` lists Project's `PjBackgroundPattern` names in enum order; the
  name is not validated, an unknown one is kept as text and drawn solid. A
  wrongly typed JSON value is ignored, as for the other fields.
- The suite draws it in `plugin_ui`: `styled` adds the line-through and the
  base fill (`base_fill`: `hollow` none, `light/medium/dark-fill` the
  background, white without one, blended 25/50/75% towards the text colour),
  and `pattern_layer` is an absolute child, to be the box's first child, for
  the hatches and dithers (`pattern_look` decides the family). gpui draws one
  45-degree slash and a checkerboard, so the left/right slants and the
  vertical stripes share the slash and the dithers differ in cell size only.
  Without a text colour the pattern is black, without a background it sits on
  white. The harness state reports `cell_styles`; the fixture's Bold command
  sets both keys.

## Text style categories the host computes (#1665)

- `plugin_ui::task_categories` (facts from the plan as it is, so a row
  entering one restyles on the next frame) is the one category list the
  grid (`row_style`), the toolbar's ticks (`shown_cell_style`) and the
  harness state's `row_categories` (`<uid>:<cat>|<cat>...` per shown task
  row, while a UI plugin is loaded) read. `Late Tasks` (Status reads
  `Late`, so a status date is needed) and `Placeholder Tasks`
  (`Task::is_placeholder`: a manual leaf with no manual or stored start,
  Project's TBD task) are computed only when a loaded manifest names them in
  `text_style_categories` (`Optins`, exact string, read once per process);
  order: ..., Manually Scheduled, Placeholder, Late, Inactive. Only a
  missing start is representable: `duration_min` is always a number, so a
  TBD duration or finish is not.
- The titles style (`titles_style`) is `Row & Column Titles` over the older
  key `Column Titles`, field by field. The headings and the task rows' ID
  cells draw by it: with one set, the ID cell takes it instead of the row's
  category style (`id_cell_style`, the cell's own on top), so the row box
  carries no style and every cell is styled whole, its own pattern layer
  included (a hatch may jog at column rules). Without one the row draws as
  before. The inactive dim and strike stay on the row.

## Text style categories that need a model feature (#1667)

- `Task::marked` (Project's Marked, `Field::Marked` Yes/No,
  `Editor::set_marked`, projctl `task.set {marked}`) is saved only in the
  `.yppx` part `marked.xml`, as Display on Timeline is in `timeline.xml`:
  both are a `flagpart::TaskFlagPart` (one `<task uid attr>` per stating
  task, written only when one states a value, an unreadable part kept as a
  retained extra) and `yppx::FLAG_PARTS` writes them in order, so a plan
  without marks saves byte-identically. Bar Styles `Marked` reads it.
- Highlight (View > Highlight, no UI yet) is view state beside the
  AutoFilter: `Editor::set_highlight` / `clear_highlight` /
  `is_highlighted(_by)` / `highlighted_rows`, the filters' checklists
  without the summary rule or any hiding; blank rows and UID 0 never.
  projctl `proj.highlight` (`{field, values}` / `values: null` / `{clear:
  true}` / `{}` reads, `filter_texts`' spec grammar) is an editor verb not
  in `MUTATING`: a plugin command's op or control, no dirty mark, the suite
  repaints. A batch that ends Ok with the plan unchanged keeps it and moves
  the revision; a refused batch drops it.
- Changed cells: every change of the plan (edit, batch step, undo, redo)
  keeps the plan it started from, with that moment's schedule and leveling
  overlay, as a read-only editor (`Editor::frozen`, `last_change`), and
  `Editor::changed_cells(fields)` lists each field whose display text
  differs, for tasks not blank before and now (a new task has none). A
  change of only the formatting or the table view keeps the record (the
  Text Styles OK that styles Changed Cells must not clear them), a refused
  batch restores it, a save keeps it, `replace_project` drops it; a
  leveling toggle does not touch it (known limit: the record keeps the
  overlay it had). Unmeasured against Project, undo included.
- The suite (`plugin_ui`): `Optins` adds `external`, `marked`,
  `highlighted`, `changed_cells`; row order ends Placeholder, Late,
  External (`external_task == Some(true)`, summaries too), Marked,
  Highlighted, Inactive, and `row_style` narrows each to a styled one.
  `Changed Cells` is per cell: `cell_style` lays it over the row's style,
  under the cell's own (`changed_cell_style`), for the grid columns but ID
  (the row header, which also renumbers), read through `changed_cells(v)`
  once per `Editor::revision`. The harness state's `changed_cells`
  (`<uid>:<column>`) is computed whenever a plugin opts in, styled or not.
  The fixture's row-menu `fixture.mark` marks, lengthens and highlights the
  first selected task and styles the three (`project-plugin-ui.uit`, not
  run on macOS; pinned by `the_fixture_mark_command_on_the_harness_plan`).
  Not done: the Highlight dropdown and named filters, a Marked column,
  Marked from `.mpp`, ghost rows for cross-project links with no
  ExternalTask row, terminal `yppxy` drawing.
