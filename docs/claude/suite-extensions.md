# Suite extensions

Notes moved from `CLAUDE.md` (#1903). Read them before changing the suite as a library (`run(ext::Extensions)`), its extension surfaces (Mail tabs, `ext::DocView`) and tab tear-off. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Extension surfaces and torn-off tabs

- The suite is a library (package `docxy-suite`, lib `docxy_suite`, still
  rooted at `suite/docxy/src/main.rs`) with `run(ext::Extensions)`; the
  `suite` binary (`src/bin/suite.rs`) passes `Extensions::default()`, so the public build is
  unchanged. A build that links an extension calls `run` itself; no
  extension, edition or private repository is named here.
- `ext::Extensions::mail` is a factory (`Rc<dyn Fn(&mut App) ->
  Result<Box<dyn TabSurface>, String>>`) for Mail tabs. `Surface::Ext` holds
  the `TabSurface` (`view`, `focus_handle`, `title`, `set_dark`); a Mail tab
  still drawn as the placeholder gets it in `attach_ext_surfaces` (render, and
  File > New > Mail), and a refusal stays on the tab's status line
  (`Mail: ...`), not asked again. It saves as nothing (`hot_bytes`, the
  session's placeholder rule). While the tab is active the suite draws no
  ribbon row (the view draws its own) unless the surface describes one
  (#1870, below), its root focus goes on to the view
  (`focus_ext_surface`), keys the view does not stop bubble to `route_key`,
  and `ext::ShowBackstage` opens File. The theme's light/dark reaches it in
  `apply_theme`.
- Tearing a tab off (`tab_tear.rs`): a chip's drag (#545) noted by
  `on_drag_move` and let go away from the strip (more than `MARGIN` above or
  below the chip, or out of the window) moves the tab through
  `new_window_at` (New Window, #587, with the new window's top-left near the
  release); a release on the strip row still reorders. The window's mouse-up
  listener (`tab_tear_release`, registered with the gesture listeners) takes
  that release. One tab cannot tear (New Window's refusal on the status
  line). Not done: dropping a tab onto another window's strip (gpui drags do
  not cross windows), a harness verb for it.

## Embedded document view (#1873)

- `ext::DocView` (`suite/docxy/src/doc_view.rs`, with `ext::LinkClick`; its
  editing mode is below, #1947) is a public gpui entity an extension's view embeds: `cx.new(|cx|
  DocView::new(doc, Some(pkg), cx))`, `set_document`, `set_link_click`,
  `on_link`, `set_content_dark`, `zoom`/`set_zoom`, `selected_text`,
  `focus_handle` (give it as the tab's `TabSurface::focus_handle`). It fills
  its parent and draws the document with the suite's own renderer in the
  flow (Web Layout) style: `block_el` with `RenderCtx::ent` a `DocTarget`,
  `Suite(Entity<Docxy>)` for every suite story (each arm forwards to exactly
  the `Docxy` method the closures called before; only it fills `DOC_WORDS`
  and `DOC_ROWS`) or `Embed(view, words)`, whose painted pieces go to the
  view's own `Words`. The shared renderer records no links: the view
  derives its link spans from its words (`link_at`, `resolve_para`).
- Read-only (the default; editable, see "Editing mode (#1947)"): no caret
  is drawn (`caret_path` empty), nothing edits.
  Press, drag and Shift+press select (`place_click_caret`); Ctrl+A selects
  all; Ctrl+C writes the text and RTF through the document tabs' own path
  (`clip_payload`, `ClipboardStore::write_rich` with its `rtf_follows`
  gate), to the run's private clipboard in a harness instance
  (`doc_view::set_harness`, set in `run()`).
  Up/Down, Page Up/Down and (Ctrl+)Home/End scroll; Ctrl+=, Ctrl+-, Ctrl+0
  and Ctrl+wheel zoom by a tenth within 50%..300% (`step_zoom`, rounded to a
  tenth; the Project chart's `project::wheel_px` / `wheel_step`: a whole
  line steps, a fractional line counts as its pixels, pixels sum to 40). gpui's scroll
  area has already moved by a Ctrl+wheel, so the offset of the last render
  is put back (as the Project chart does). The view draws no table
  selection zones (`table_el`'s `selects` needs `DocTarget::is_suite`,
  #2003). Every other key bubbles to the suite; Tab and Shift+Tab (global
  actions) do nothing on an extension tab.
- Hyperlinks: a secondary press (Ctrl, ⌘ on macOS) on a link calls
  `on_link` with the URL (or `#bookmark`) and selects nothing, in either
  mode; in `LinkClick::Click` mode a plain press released without a drag
  does too. Only a press on a painted word or unit is on a link
  (`DocTarget::begin_select`'s `on_piece`): a press beside a row lands on
  the row's nearest stop, never a link. The callback runs deferred (`window.defer`), outside the view's
  update. The view never opens a link.
- Colours: the chrome (scroll bar, selection) is the theme's; the document
  is a white page in Print Layout's palette (`content_look`, `page_pal`) unless
  `set_content_dark(true)` on a dark theme, which draws the theme's own (on
  the host's page colour if it set one, #2003).
  A link draws in its run's colour (#1906); only a link without one takes
  the palette's `Pal::link`: `LINK` `#2F6FDB` on the white page, `LINK_DARK`
  `#6CA0F0` on the dark one (7.5:1 on the stock `#0A0A0A`). HTML import
  colours every link `0563C1` and Word's Hyperlink style does too, so with
  dark mode for content and no adaptation by the embedder such a link draws
  `0563C1`, about 3.4:1 on `#0A0A0A`, like any dark authored ink there.
- Pictures: the view's `pictures::Cache` is registered with
  `pictures::register_extra` and never reads `Docxy`: `set_document` and
  its release queue what it gave up (`queue_release`), the windows'
  `release_pictures` / `release_all_pictures` drain the queue, and
  `other_windows_picture_ids` counts every registered cache's live ids, so
  neither side drops a decode the other still draws.
- Harness: a script's `ext fixture` line (`uiharness::launch::request_ext_fixture`,
  marker `<root>/docxy/ext-fixture`, read under `--harness` only) installs
  `ext_fixture` as Mail when the build links none: a header over a DocView
  of a document built in code (`fixture_document`), recording each link.
  `ext-doc-read` (blocks, tables, pictures, links, selection,
  selected_paragraphs, zoom, scroll_y, max_scroll_y, scroll_x,
  max_scroll_x, text_width, table_widths, link_click,
  content_dark, focused, links_followed, link_calls; the page fields are
  #2003's, below), `ext-doc-set
  {link_click, content_dark}`, the regions `ext-doc-view`,
  `ext-doc-link:<n>[:left]` (`:left` 8 px left of the span, at mid-height)
  and `ext-doc-word:<text>[:start|:end]` (`:end` a pixel
  past the last character), and `pointer-click`'s `ctrl`.
  `uiharness/cases/ext-doc-view.uit` drives it (run on Linux under
  `scripts/ui-linux.py`). `suite/docxy/tests/ext_doc_view_api.rs` holds the
  public API to what an extension crate needs, from outside the crate.
- Tables fit the view (#1895), as in a document tab's Web Layout (see
  `docs/claude/docx-editing.md`'s "Web Layout fits tables"): the view's
  text width comes from a last in-flow `w_full` zero-height probe in
  `ext-doc-scroll` (`text_w`), and `ext-doc-scroll` scrolls on both axes
  under a horizontal bar (`ext-doc-hscrollbar`, a 12 px strip along the
  bottom). gpui's X11, Wayland and Windows clients turn a shifted wheel
  into `delta.x` (macOS sends it), so the view has no shift handling;
  Ctrl+wheel's put-back restores both axes. `ext-doc-read` adds `scroll_x`
  / `max_scroll_x` (clamped as the next paint will), `text_width` and
  `table_widths` (each top-level table: `block`, `width`, `left` / `right`
  from the view's left, `fits_text`, `right_in_view`); `pointer-wheel`
  takes `"shift": true` (a horizontal delta, as the platforms deliver it).
  The fixture adds, after the 2x2 table, a 100% table imported from HTML
  (`fitted_html`, with a link to `CELL_URL`; in the model only) and a fixed
  12000-twip table.
- Out of scope: Find (#1887), Print Layout (#1888), keyboard selection in
  read-only mode (the editing mode has it), table selection zones, a
  context menu, anchor links scrolling inside the view (they reach the
  callback as `#name`), macOS ⌘-key routing for the view (untested).

## Editing mode (#1947)

- `DocView::set_editable(true)` turns the view into an editor (read-only
  stays the default, and its key path is exactly #1873's). Public:
  `set_editable`, `editable`, `format(ext::DocFormat)`, `undo`, `redo`,
  `can_undo`, `can_redo`, `caret_format() -> ext::CaretFormat`,
  `on_change(Fn(&mut App))`, `document()` (`export_doc`), `package()` (a
  clone, with the numbering part a first list added). Host calls on a
  read-only view return false and change nothing. `DocFormat` and
  `CaretFormat` are `#[non_exhaustive]` (#1952 adds links).
- The decisions are `doc_view/edit.rs`, over a plain `Editor` and
  `Option<&mut Package>` (unit-tested without a window): `edit_key` (the
  document tab's `on_key` editing map: navigation with Shift extending,
  Ctrl word/document moves, macOS ⌘/⌥ through `mac_nav`, Backspace,
  Delete, Enter, Shift+Enter, Ctrl+B/I/U/Z/Y/Shift+Z/A/C/X/M, Ctrl+Tab in
  a cell, Ctrl+Shift+Space, typed text with AltGr allowed and plain Alt
  never text off macOS; no AutoCorrect, Repeat, fields or breaks), `tab`
  (the action-bound Tab and Shift+Tab, taken by the view's root div:
  `tab_key` / `shift_tab_key`'s document rule, so cells move, Tab types a
  tab and Shift+Tab is Decrease Indent; Tab does not change a list's
  level, as in a document tab), `apply_format` (`apply_doc_act` and the
  font boxes' calls; Bullets/Numbering toggle with `all_in_list` and give a
  package without numbering `add_markdown_numbering`, whose ids 1 and 2
  are `NUM_BULLET`/`NUM_DECIMAL`) and `caret_format` (`caret_props_shown`,
  pending toggles included, #854; the caret end's, no mixed state; font,
  size and colour over the seeded styles, `None` only when neither the run
  nor its styles set one).
  Increase/Decrease Indent by any path repairs the package's list levels
  after a change (`repair_list_indents`, #1728). Every host command and
  Enter, Ctrl+B/I/U/M break the typing group first; moves end it
  (docxcore's `last`), so Ctrl+Z after a click takes back only the typing
  since.
- A change is told by comparing, before and after, the body's length and
  the top-level blocks the selection spans with one on each side
  (`edit_window`): typing and deleting coalesce into one undo step, and a
  step may change nothing (`set_align` checkpoints on a paragraph already
  aligned), so the history cannot tell. A key costs the blocks around the
  caret. Undo and Redo (Ctrl+Z/Y and the host's) compare the whole body
  (`edit::history`): undoing a step that changed nothing is no change.
  `on_change` fires deferred (`cx.defer`) only on a change: never for a
  move, a selection, scroll, zoom, a toggle at a collapsed caret, an undo
  of a step that changed nothing, `set_document` or `set_editable`. Styles are seeded into the editor
  (`set_styles_xml`, as `sync_editor_styles`) on `set_editable(true)`,
  `set_document` while editable and before each edit.
- The caret draws (`RenderCtx::caret_path`) while editable and focused.
  After a key or host command (`reveal`) the `viewport` canvas's paint,
  which runs after `ext-doc-scroll` has painted this frame's `Words`,
  scrolls the caret's piece (`caret_piece`) into view in y (`reveal_dy`),
  one frame late; a caret whose paragraph painted no piece follows its
  top-level block (`follow_item_y`). Piece-level, not block-level: a
  paragraph taller than a short compose pane would otherwise show its top
  on every key. Ctrl+Home/End also scroll to the very top and bottom.
- Links: editable, a plain press places the caret (never follows, whatever
  `LinkClick`); Ctrl+click follows.
- Dialogs: while a dialog takes the keys (`top_takes_keys`) or the
  more-tabs list is open, `focus_ext_surface` keeps the suite root focused
  and takes the focus back from the surface, so an editable view never
  types under a dialog; it gets the focus back when the dialog closes.
- Harness: `ext-doc-set {editable}`; `ext-doc-read` (and `ext-doc-set`) add
  `editable`, `caret` (`path`, `offset`), `caret_in_view` (null with no
  painted piece), `caret_format` (all fields, `align` by name),
  `can_undo`, `can_redo`, `change_calls` (the fixture's `on_change`
  count); `ext-doc-call` takes one of `format` (bold, italic, underline,
  bullets, numbering, indent, outdent, align-left/center/right/justify),
  `font`, `size`, `color`, `highlight` (null clears), `undo`, `redo`, and
  replies `done` plus `ext-doc-read`'s view fields; `ext-doc-document`
  replies `document()`'s top-level paragraphs (`text`, `num_id`, `ilvl`,
  `align`, `runs` with `text`/`bold`/`italic`/`underline`) and
  `package_has_numbering`. `uiharness/cases/ext-doc-edit.uit` drives it.
- Not done here: hyperlinks (#1952); pictures from bytes and paste of
  pictures or the suite's formatted clipboard (#1953), so Ctrl+V bubbles
  as before; line-wise Up/Down (paragraph-wise, `move_vert`, as a document
  tab); AutoCorrect, Repeat; IME, dead keys and Windows' WM_CHAR packet
  text (the suite's input handler is the root's, so a focused view gets
  plain key-downs only).

## Host insert, copy and cut (#1977)

- `DocView::insert_text(&str)`, `copy`, `cut`, `can_copy`: a host ribbon's
  Insert Signature / Quick Part and its Clipboard group's Copy and Cut,
  without `document()` / `set_document()` (which lose the caret, the
  selection and the history). The decisions are `doc_view/edit.rs`'s
  `insert_text` and `cut`, tested without a window.
- `insert_text` is Paste Text Only's path (`Editor::paste_text_only`):
  it replaces the selection, newlines split paragraphs (`\r\n` and a lone
  `\r` are newlines too), tabs are tab inlines, every run takes the
  formatting typing at the caret would give (a format toggled at a
  collapsed caret too), the caret ends after it. `\u{0B}` (`LINE_BREAK`,
  Word's plain-text manual line break) is a line break inside the same
  step: the text is split on it, `paste_text_only` per piece and
  `insert_line_break` between them, in one outer `one_step`. It drops what
  a `w:t` cannot hold (`insertable`: other C0 controls, U+FFFE, U+FFFF,
  `FIELD_CHAR`, the field stand-in), as `serialize::esc_text` escapes only
  `& < >`. One undo
  step of its own (`one_step` ends the typing run before and after). A
  text with nothing left runs nothing and returns false. Inside a
  hyperlink the text lands between the link's two halves (`paste`'s
  `split_content`), as Paste Text Only does in a document tab.
- `cut` is Ctrl+X's path (`edit::cut`, which the key uses too): the
  document tab's `one_step("Cut", ...)` run through `edit()`, so it says
  whether the document changed. The clip goes on the clipboard through
  `write_clip` (text and RTF). A selection from body text into a table
  cell is copied, but `Editor::cut` only collapses it (no step), so
  `on_change` does not fire; `KeyOutcome::Cut` carries that flag. True
  when there was something to cut, false with nothing to cut.
- `copy` works in either mode, as Ctrl+C does in the read-only view
  (`write_copy`): a reader host's Copy button copies. This departs from
  the issue's "each returns false on a read-only view", which holds only
  for `insert_text` and `cut`. `can_copy` is `has_selection()` in either
  mode. A host enables Cut with `can_copy() && editable()`. A copy never
  fires `on_change`.
- Harness: `ext-doc-call` takes `{"insert_text": <text>}`,
  `{"copy": true}`, `{"cut": true}`; `ext-doc-read` (and `ext-doc-set`,
  `ext-doc-call`) add `can_copy`. The `issue 1977` test in
  `uiharness/cases/ext-doc-edit.uit` copies "Beta", then cuts the whole
  paragraph, so the second clipboard read can only be the cut's.

## Page inset and page background (#2003)

- `DocView::set_page_inset(Option<Edges<Pixels>>)` / `page_inset()`: the
  padding around the document inside `ext-doc-scroll`, all four edges, in
  logical px; `None` is `DEFAULT_PAGE_INSET` (48 left/right, 28 top/bottom,
  the old fixed padding). `resolve_page_inset` clamps each edge at 0. It
  does not scale with the zoom (the default never did): a host aligns the
  body with its own header in fixed pixels. The `w_full` width probe sits
  inside the padding, so `text_w` (what tables fit) and `column` follow it;
  the root's `on_mouse_down` covers the padding, so a press there focuses
  the view. That holds beside a table row too because the view draws no
  row-selection strips. Each strip is 12 px left of its row and stops its
  press; in the view they did nothing, so a press on one never focused it.
  CI's fonts wrapped the fitted table's cell and put its row under the
  mid-height press. The 12 px scroll-bar strips overlay the right and bottom
  padding: an edge under 12 px lets them cover the text's edge.
- `DocView::set_page_background(Option<Hsla>)` / `page_background()`: the
  dark page's colour (`content_look`'s `page_bg`, used only when content
  is dark on a dark theme); `None` is the theme's `background`. The light
  page stays white and the palette is unchanged: a host recolours the
  document's text in the document. The colour paints the view's root, so
  the padding is that colour too.
- Both setters notify only when the stored value changes (`set_if_changed`);
  the getters return what the host set, `None` included.
- Harness: `ext-doc-set` takes `page_inset` (`null` or
  `{top,right,bottom,left}`, all four, px) and `page_background` (`null` or
  `"rrggbb"`, a leading `#` allowed); `ext-doc-read` adds `page_inset`
  (resolved), `page_background` (`rrggbb` or null), `page_color` (what the
  last frame painted) and `column_left` (the text column's left from the
  view's left), the last two read after a `shot`. Colours have no `#`: a
  case's `#` starts a comment. Regions `ext-doc-inset:left[:<word>]`
  (middle of the left padding, at mid-height or beside the painted word's
  row: the case presses beside `Alpha` and the table's `A1`, which do not
  depend on fonts) and `ext-doc-header` (the fixture's header
  line, 6 px above the view). The fixture's header has its own focus
  handle and takes it on a press. With nothing else focusable on a Mail tab,
  `focus_ext_surface` hands the root's focus straight back to the view, so
  this is the only way a case can start a press with the view unfocused.
  The `issue 2003` test in `uiharness/cases/ext-doc-view.uit` (a 1000 px
  window) checks 904 → 956 px of text, column at 48 → 22 and the page
  `262626` dark / `ffffff` light, and shoots each.

## Extension ribbons and the suite theme (#1870)

- `TabSurface::ribbon(&self, cx) -> Option<ext::Ribbon>` (default `None`),
  `ribbon_action(id, window, cx)` and `theme_changed(&SuiteTheme, cx)`, all
  default methods. The data types (`Ribbon`, `RibbonTab`, `RibbonGroup`
  with `priority` 50 by default, `RibbonControl::{Large, Small, Toggle,
  Split(RibbonSplit), Dropdown(RibbonDropdown), Gallery(RibbonGallery)}`,
  `RibbonCommand` enabled and unchecked by default, `RibbonIcon::svg`) own
  their strings, are `#[non_exhaustive]` and are built through their
  constructors (no struct-like variants, so a later field breaks no
  extension). A surface with a ribbon has the suite draw File plus its tabs
  with the document ribbon's code: `render_group` / `render_control`,
  `ribbon_fit::fit_with` under `LabelsFirst`, Ctrl+F1 and the `ribbon-min`
  chevron, flyouts, menus and KeyTips; File and its `F` are the backstage.
  File's `F` is first in `ext_ribbon::tab_keytip_pick`, so an extension
  tab's KeyTip starting with `F` (any case) could never be typed: it is
  dropped at conversion (`tab_key_tip`), the tab kept with no badge.
  Double-click on a ribbon tab collapses no ribbon in the suite (a follow-up
  for every ribbon). A ribbon with nothing left to draw is `None`.
- `ext_ribbon.rs`: `convert` makes `rs::Tab<Act>`s whose acts are
  `Act::Ext(id)`, every string interned (`plugin_ui::intern`, a `HashSet`
  now, as this interns on every refresh; each distinct string is kept for
  the process's life, said on `ribbon()`), with a state map (enabled,
  checked) and the drop-downs' ids. Left out: an empty id, `qat-undo` /
  `qat-redo` (which `open_split_menu` and `split_menu_anchor` read as the
  QAT's), a gallery `tablestyles` (`render_control`'s Table Styles), then
  empty groups and tabs. A `Small` of more than three is several
  `Column`s; Small and Toggle labels are cut with `capped_label`, the full
  text the tip's title. A gallery's tiles are the bare swatch tiles
  (`is_swatch_gallery` takes `Act::Ext`), each showing its label.
- The cache is on the tab: `Surface::Ext(ExtTab)` holds the surface (it
  derefs to it) and `(generation, Option<ExtRibbon>)`, so it travels with a
  torn-off tab. `ext::ribbon_changed(cx)` needs no window: it bumps the
  `RibbonGeneration` global and refreshes the windows; it never calls into
  `Docxy` (`ribbon_action` runs inside a `Docxy` update).
  `refresh_ext_ribbons` reads a stale tab again at the top of `render`, at
  every harness verb's entry, and (forced) after `ribbon_action`; on the
  active tab the selected `RibbonTab::Ext(i)` follows its tab's name
  (`reselect`), else the first. Never converted per frame.
- `ext_dispatch` is the one guard: a disabled id (or one the ribbon does not
  hold) sets the status line and runs nothing, whether it came from a
  click, a KeyTip (`tab_keytip_cmd` checks no state), a menu or gallery
  item or `ribbon-click`. A drop-down's own id (its KeyTip runs the
  button's act, as a built-in's does) opens its menu instead, and that
  menu is always its own items (`dropdown_menu`'s `Act::Ext` arm comes
  before the id-keyed built-in menus: an id `hf-header` lists the
  extension's items, not Word's Header menu).
- Icons: `RibbonIcon` is SVG source, registered by content hash
  (`ext_ribbon::ICONS`) and named `ext/<hash>`; `DocxyAssets::load` serves
  `icons/ext/<hash>.svg` from it. It is drawn as a mask in the ribbon's ink,
  as the Fluent icons are (no colour icons). A command with no icon: the
  large button and drop-down draw the label alone and are estimated so
  (`ribbon_fit::icon_w`), a split keeps a blank 26 px upper half, the
  collapsed group's button takes the first icon there is
  (`model_group_icon`).
- Focus: on an extension's tab with a ribbon, `focus_ext_surface` takes the
  focus to the root while the KeyTips are up, a menu or a ribbon flyout is
  open, from whatever had it (the view, or a field of the extension's own
  beside it), remembered in `ext_focus_back` with the surface's view; it is
  called at once where the KeyTips go up (Alt/F10) and after each KeyTip
  letter, as gpui may run the next key before a frame. When they close,
  render gives the remembered handle back (while the same surface is
  active; else its `focus_handle`). The dialog and more-tabs rule is
  #1947's (only a focused view is taken from). A menu or flyout opened by the pointer is
  taken at the next frame. A tab without a ribbon keeps #1873's rule (a
  menu leaves the focus alone). A bare Alt already bubbles from `DocView`.
- Theme: `ext::suite_theme(cx)` is `render`'s colours (`background`,
  `secondary` as `panel`, `border`, `foreground`, `muted_foreground`,
  `selection`) plus `accent::for_kind(Some(Kind::Look), dark)`.
  `apply_theme` calls `theme_changed` beside `set_dark` (on the first frame
  too, as `applied` starts `None`); `attach_ext_surfaces` calls it once a
  theme is drawn.
- Harness: `ext fixture ribbon` (uiharness: the marker reads `ribbon`;
  `harness::ext_fixture_ribbon_requested`) has the fixture describe
  `fixture_ribbon`: Message (Respond: Reply with an SVG icon, a column,
  the Flag toggle; Move: the Move split, Archive disabled until set; Tags,
  the lowest priority, which collapses into a flyout at 460 px) and
  View (Arrange: the Sort drop-down, the density gallery). `ext-ribbon-read`
  replies `ribbon`, `actions` (ids run), `flag`, `archive_enabled`,
  `density`, `theme_calls` and `theme` (`dark` and `rrggbb` colours);
  `ext-ribbon-set {archive_enabled}` changes it through
  `ext::ribbon_changed`; `ext-ribbon-read` adds `header_focused` (the
  fixture's header line has the keys). The region `ext-ribbon-cmd:<id>` is
  a command: a button, a split's upper half or a drop-down, in place or in
  an open group flyout, or a gallery tile in place (only `Act::Ext`
  commands draw its probe). `ribbon-layout {"tab"}`, `ribbon-click` and
  `menu-open {"ribbon": ..}` name the extension's tabs (`ribbon_tab_named`,
  `ribbon_tab_def`); `ribbon-read` lists every one of them, and
  `ribbon-flyout` acts on the tab shown. `uiharness/cases/ext-ribbon.uit`
  drives it.
- Not done: contextual tabs, group launchers, Rows and combo cells, QAT
  entries, gallery live preview and colour icons for an extension; macOS
  menu-bar entries for its commands.
