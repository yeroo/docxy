# UI plugins

Notes moved from `CLAUDE.md` (#1903). Read them before changing the `yppx_*` UI plugin ABI, the plan's formatting model, and the suite's plugin dialogs and controls. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## UI plugins (#1532)

- A `yppx_*` library may carry, beside the six format symbols, a version
  target and five UI symbols (`projcore::plugin_abi`: `yppx_plugin_target`,
  `yppx_ui_abi`, `yppx_ui_manifest`, `yppx_ui_state`, `yppx_ui_command`,
  `yppx_ui_overlay`; JSON over `YppxBuf`). The hosts own the extension points
  and call whatever library is present; no particular plugin, product
  edition or private repository is ever named here: the fixture is the only
  plugin this repository knows.
- `load_plugins_for(dirs, Some(Host {name, version}))` checks the target for
  the host's name (`suite`, `yppxy`) against `CARGO_PKG_VERSION` before any
  other symbol: a mismatch skips the library with its reason among `errors`
  (stderr, crash log, first window's status line) and in `skipped`; a library
  without the target symbol is formats-only and its UI symbols are not looked
  up. `projcore::uiplugin` holds the typed JSON (`Target`,
  `version_satisfies`, `Manifest`, `StateCtx`/`State`, `Command`/
  `CommandResult`, `OverlayCtx`/`Shape`); `projcore::json` is the value type
  (strict parser, compact writer; the formats declaration uses it too).
- The state and command requests' `selection {uids, cols}` lists the
  cursor's task first and the cursor's column first (#1756), then the
  range's others in order (`Selection::cursor_first`), so the cursor cell is
  the first uid's first column; no field says it apart.
- Command ops are `projctl` editor verbs applied by `projctl::apply_ops` in
  one `Editor::batch`. yppxy's `--command <id>` runs one headless (first
  task selected) and prints the status. Both hosts' `app-info` reports
  `plugins: {loaded, ui, skipped}`. The fixture
  (`projcore/tests/plugin_fixture/src/ui.rs`) exports one of everything;
  `projcore/tests/plugins.rs` and `yppxy/tests/plugins.rs` cover it.
- The plan's formatting (`projcore::formatting::Formatting`: palette id,
  text styles per category and per cell, gridlines, link style, and the
  named `switches` a plugin owns, #1553: a command's `view.switches`
  `{name: bool|null}` sets or clears them, the state and overlay requests'
  view/flags carry them as `switches`, so a toggle's `checked` and what the
  overlay draws follow the plan; the view also carries every text style
  category as `text_styles` and the selected cells' styles as `cells`,
  #1572, so a Font toggle can show the cursor cell's style) is
  `Project::formatting`, edited as one undo step by `Editor::edit_formatting`
  (a command's `view` reply goes through `Formatting::apply_view`; yppxy's
  `--command` applies it too), saved in
  the `.yppx` part `formatting.xml` (own content-type override, never kept as
  an extra part, absent when empty) and never in MSPDI. `Editor::revision()`
  moves on every change of the plan, for hosts caching what they derive from
  it (plugin state, the overlay).
- The suite's side is `suite/docxy/src/plugin_ui.rs`, asked from the Project
  code paths only: `project_ribbon()` and `gantt_format_tab()` end with
  `extend_tab` (a contributed control whose id is a built-in command's
  replaces that command in place, keeping the control's shape; the rest go
  into the group of that title or a new group after the tab's own, or
  before/after the built-in group the manifest's `before`/`after` names,
  #1574; small
  buttons stack in columns of three; a gallery's swatches carry their colour
  in the tile preview as `#RRGGBB` and draw as `Docxy::swatch_gallery`; a
  combo draws as a drop-down); the row, bar, column and the new chart menu
  (`project_chart_menu`, `MenuTarget::Chart`, a right-press on the strip
  beside the bar, `menu-open "chart"`) end with `extend_menu` (replace by
  id, else after the `after` label, else appended after a separator);
  `task_info_dialog_for` adds the `task-information` spec's pages (replace
  by name, else appended); a command's dialog is `catalog::PLUGIN`
  (`"plugin"`) with `DialogOwner::Plugin { command }`. Every contributed
  command is `ProjectAct::Plugin(id)` (interned), run by `run_command` in
  `apply_project_act`: ops and `view` in one `Editor::batch`, the view's
  switches set, the status and dialog returned. `plugin_ui::dialog_click`
  runs before the generic dialog path: a plugin dialog's OK sends its command
  again with `values_of` (every `plugin` control but a `sample`, which is
  read-only); Task Information with
  plugin pages applies the built-in fields (`apply_task_info_in`) and the
  `task-information` command's ops in one step.
- State (`enabled`, `checked`, `swatch_checked`) comes from `yppx_ui_state`,
  cached in `ProjectView::plugin_cache` by `Editor::revision`, the cursor and
  the view flags (never per frame); `act_enabled_now` and
  `project_act_active` read it. A dialog about to open asks it once more
  with `dialog`: the reply's `values` prefill the plugin's controls, its
  `items` replace a dropdown, radio or list control's items for that
  opening and its `enabled` greys controls (#1556, `plugin_ui::Prefill`). The overlay (`gantt_overlay` in `gantt.rs`,
  a canvas after the rows, clipped to the chart, scrolled with it) paints
  `overlay_shapes`, cached by revision, scale, flags, the shown rows and the
  selection (#1565); `bar_look` draws the
  bars in the palette the plan's formatting names (`plugin_ui::palette`),
  and `row_style`/`cell_style`/`titles_style` draw the formatting's text
  styles (`Row & Column Titles` over `Column Titles` on the headings and ID
  cells, #1665). The harness reports contributed
  items with `"plugin": true` in `ribbon-read`, `menu-read` and
  `dialog-read`, `project-overlay` lists the shapes, and `dialog-set`
  addresses a grid cell as `name[row].column` (checked against the column's
  kind). `ControlKind::Multiline` (Enter is a new line; `key_button` leaves
  Enter alone with one focused) and `Grid` (per-column `column_kinds`, drawn
  as a table; a plugin's grid is interactive, `Control::interactive`, a
  built-in dialog's stays read-only: a click, Up/Down or `dialog-select` picks a row (`Control::row`),
  Left/Right the active column, typing edits the cell (`Control::edit`, Enter
  commits through the input handler, Escape drops it), Space toggles a
  `Check` cell; a `commit_command` grid's commit sends the command with
  `edit` through `Dialog::committed`) are generic; the catalogue check skips
  `plugin` controls.
  A plugin dialog may be laid out as Project's (#1609): `Dialog::size`
  (fixed, every page), `buttons_right` (else Help bottom left, the rest
  bottom right), `page_columns` (controls per row; `Dialog::layout_rows`
  lays the bands out and `Docxy::dialog_body` draws them: the header band
  first (in Task Information: Name, then Duration/Estimated, then the
  plugin's header controls), consecutive same-`Control::group` controls as a titled
  box, rows of `columns` with `span` each, the labels of a column one
  width), `Control::min_rows` (a grid's visible rows; a plugin grid draws
  8, more scroll behind a `Scrollbar`), `Control::fill` (`Dialog::fills`: a
  multiline fills a sized dialog and scrolls its lines, the caret's kept in
  view; grids and filling multilines share `GridScrolls`, keyed by
  `Dialog::epoch` so a reopened dialog starts afresh, each entry with the
  row height it was drawn with; the dialog verbs report them as
  `first_visible_row` / `first_visible_line` while the control scrolls),
  `ControlKind::Sample` (a read-only box drawn by `plugin_ui::styled` from
  the formatting model's `TextStyle`) and `Control::header` (a spec's
  header control, listed last, drawn first on every page and first in the
  Tab order; Task Information's own top row is header too, #1682, so there
  it comes before the plugin's, in the drawing and the Tab order). A plugin dialog may declare its own buttons (`Button::id`; a
  press sends the command with `button`, the dialog stays open unless the
  reply's `close`); `help` is the host's: the window's press opens the
  spec's `help` URL through `open_help_url` (recorded for `last-url`), else
  says `DOCS_COMING`, any direct `dialog_click` press says `DOCS_COMING`, and it is
  never the default or the cancel button, so it never reaches the plugin. It
  may be `modeless` (no backdrop, `Dialog::active` decides who has
  the keys: a press in the table takes them, a dialog press gives them
  back; mutating control verbs `clear_modal` and keep it; `refuse_under_dialog`
  ignores it), and every plugin control change marks the dialog `stale`:
  `plugin_ui::refresh_dialog` asks `yppx_ui_state` again with `dialog.values`
  from `complete_project`, the harness dialog verbs, typing and clicks
  (#1602, #1567) and applies its items, enabled flags, `Label` and `Sample`
  values and `name.selected_row`; every value applies when the plan or the selection
  moved since `Dialog::seen` (set at open, and by `extend_task_info`), modal
  or not (#1610). A rolled-back `Editor::batch` restores the revision with
  the rest of the editor's state (a refused plugin OK is no move).
  `uiharness/cases/project-plugin-ui.uit` drives
  the fixture through all of it; nothing changes without a UI plugin.

## Dialog values that follow (#1670)

- A state reply may name editable dialog controls in `"follow": [names]`
  (`uiplugin::State::follow`; non-strings skipped, any other value none,
  written only when non-empty). On a re-ask (`refresh_dialog`) a named
  control takes the reply's value like a label does, though neither the
  plan nor the selection moved; without a value it is untouched. The plugin
  decides when (Text Styles: when `Item to Change` changed, seen in
  `dialog.values`), so a re-ask the field itself caused overwrites nothing;
  the host only obeys, per re-ask (each typed digit of a followed-from
  field is one). A name follows only for the plugin whose value is taken
  (`Prefill::merge`). `apply_refresh(d, prefill, moved)` is the apply step,
  unit-tested without a plugin. The fixture's Fixture Options style follows
  a number changed since its echo label last showed it
  (`project-plugin-ui.uit`, not run on macOS).

## Overlay text style (#1668)

- `projcore::uiplugin::Shape::Text` carries `font`, `bold`, `italic`,
  `underline` and `strikethrough` (`Option`s) beside `x, y, text, color,
  size` (`size` stays pixels). `shapes_json` writes each only when set,
  after the old keys, so an unstyled text is byte-identical to before;
  `parse_shapes` reads them, a wrongly typed value or a blank font is
  `None`. The suite's `paint_shape` builds the run with
  `gantt::overlay_text_run` (the shape's family, weight and slant over the
  overlay's Segoe UI, a 1 px underline and strikethrough in its colour; an
  explicit off is plain, nothing is inherited) and `project-overlay`
  reports the keys when set. The fixture's names label takes the view's
  `Bar Text - Right` style; its `selected N` label is italic and
  underlined. No background or pattern on overlay text.

## Rich-text dialog control (#1557)

- A UI plugin control `"kind":"richtext"` (`ControlKind::Richtext`) is a
  multiline with a toolbar (Bold, Italic, Align Left/Center/Right, Bulleted
  List). Its wire value (spec `value`, a prefill, the state's `values`, the
  command's `values`, `dialog-read`/`dialog-set`) is the markup of
  `projcore::richtext` (dependency-free; a plugin converts it to RTF
  itself): one `<p>` or `<li>` (`align="center|right"`) per line, inline
  `<b>`/`<i>`, `&amp; &lt; &gt;`. The writer is canonical; a value is markup
  only when it starts with `<p`/`<li` followed by `>` or a space, anything
  else is verbatim plain text, so `if a<b>c` is safe. A host without the kind
  draws plain text.
- `Control::value` stays `Value::Text(plain)`, so every caret, selection and
  paste path of a multiline works unchanged (`ControlKind::is_multiline`
  covers both kinds at every such site). The attributes are
  `Control::rich_state` (`dialog/richtext.rs` `RichState`: a style byte per
  character, a `Para` per line, the pending style), always read through
  `rich()`/`rich_mut()`, which pad it to the text. Typing goes through
  `Control::rich_replace` (splice in step, never re-read as markup); only
  `set_markup` (dialog-set, prefill, refresh) parses. `snapshot()` (markup)
  is what `changed`/`mark_opened` compare, so a style-only edit counts.
- Typed text takes the style before the caret (the line's first character at
  a line start, plain in an empty line) unless the toolbar set a pending style
  at a bare caret; the pending style is dropped by a caret move, a focus
  change, a typed character and `set_markup`. A refresh that gives the markup
  the control holds leaves it alone (`plugin_ui::follow_value`).
- The host draws pieces from `line_segments` (the caret and selection cut
  the runs) and offsets clicks by `line_offset` (bullet gutter, alignment);
  `char_at_rich_x` measures runs in their own bold/italic. Toolbar buttons
  are probes `dialog-richtool:<field>:<tool>` (`pointer-click
  {"dialog-richtool":"<field>:<tool>"}`); Ctrl/Cmd+B, I, L, E, R do the same.
  `dialog-read` adds `richtext {bold, italic, align, bullets, pending_*}`.
  Out of scope: font, size, colour, underline, numbered lists, indents, undo
  inside the control, Shift+word moves.

## Grid column `cell` (#1558)

- A plugin grid column of kind `cell` (`ControlKind::Cell`, a column kind
  only) lets each row's cell have its own kind: the value in the prefill, the
  manifest or a state refresh is `{"kind","items","value"}` (dropdown, combo,
  checkbox, check, number, date; anything else, a bare scalar included, is
  text; an object without `value` is empty). `Value::Rows` stays plain
  strings, so `values`, `dialog-read` rows and every consumer are unchanged;
  the kinds live in `Control::cell_specs` (`[row][col]`, `CellSpec`), read
  through `cell_kind(row, col)` / `cell_items(row, col)`, which the cell's
  set, toggle, step, click, Space and drawing use instead of `column_kind`.
- `dialog-set name[row].column` checks the cell's own kind (a dropdown cell
  names an item as an editable grid's dropdown column does, a combo keeps any
  text; `resolve_cell` is otherwise still for editable grids only), and a
  click or Space on a dropdown cell steps its items, also in a plugin grid
  (its other columns are unchanged). `dialog-read` adds `cell_kinds` and
  `cell_items` per row for the first `cell` column. `dialog-set`'s
  `insert_row`/`delete_row` move the specs; a refresh with objects replaces
  them and one with plain strings keeps them by row index (a plugin that
  reorders rows resends the objects). Out of scope: colour, font, list,
  richtext and multiline cells (text), a calendar or combo list in a plugin
  grid's cell.
