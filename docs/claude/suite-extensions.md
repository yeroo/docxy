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
  and `ext::ShowBackstage` opens File (where the tab may add its own pages,
  and `ext::CloseTab` closes it: #1923, below). The theme's light/dark reaches it in
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
  update. The view never opens a link. The target is docxcore's
  `link_url` (`target#anchor` for a link with both, #1916). The link
  under the pointer and the hand cursor: "Link hover and the hand cursor (#1916)" below.
- Colours: the chrome (scroll bar, selection) is the theme's; the document
  is a white page in Print Layout's palette (`content_look`, `page_pal`) unless
  `set_content_dark(true)` on a dark theme, which draws on the host's page
  colour if it set one (#2003), else the theme's background, with the
  document's colours mapped against it as Word's dark mode maps them
  (`dark_page_pal`, #2079: `docs/claude/suite-document.md`).
  A link draws in its run's colour (#1906); only a link without one takes
  the palette's `Pal::link`: `LINK` `#2F6FDB` on the white page, `LINK_DARK`
  `#6CA0F0` on the dark one (7.5:1 on the stock `#0A0A0A`). HTML import
  colours every link `0563C1` and Word's Hyperlink style does too (about
  3.4:1 on `#0A0A0A`), so with dark mode for content such a link is
  lightened, hue kept, to 4.5:1 on the page (#2079), as any dark authored
  ink is.
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
  under the suite's document scrollbars (#2080, `doc_scrollbar`: 12 px
  strips over the view's right and bottom edges, shown while their axis
  overflows, the caret reveal keeping clear of the horizontal one; `ext-doc-vbar`, `ext-doc-hbar-thumb`, ... regions, and
  `ext-doc-read`'s `vbar` / `hbar`, see `docs/claude/suite-document.md`).
  A press on a bar stops there: it does not focus the view. gpui's X11, Wayland and Windows clients turn a shifted wheel
  into `delta.x` (macOS sends it), so the view has no shift handling;
  Ctrl+wheel's put-back restores both axes. `ext-doc-read` adds `scroll_x`
  / `max_scroll_x` (clamped as the next paint will), `text_width` and
  `table_widths` (each top-level table: `block`, `width`, `left` / `right`
  from the view's left, `fits_text`, `right_in_view`); `pointer-wheel`
  takes `"shift": true` (a horizontal delta, as the platforms deliver it).
  The fixture adds, after the 2x2 table, a 100% table imported from HTML
  (`fitted_html`, with a link to `CELL_URL`; in the model only) and a fixed
  12000-twip table.
- Out of scope: keyboard selection in
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
  `CaretFormat` are `#[non_exhaustive]` (#1952 added links).
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
- Not done here: pictures from bytes and paste of
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

## Host hyperlinks (#1952)

- `DocFormat::InsertLink { text, url }`, `EditLink { text, url }`,
  `RemoveLink` through `DocView::format`, and `CaretFormat::link:
  Option<DocLink>` (`DocLink { text, url }`, `#[non_exhaustive]`, `url` the
  target, `#bookmark`, or `target#anchor` for a link with both, which is how
  Word stores a URL's fragment): a host's link dialog prefills from
  `selected_text()` or `caret_format().link`. Each runs as one undo step
  ("Hyperlink", "Edit Hyperlink", "Remove Hyperlink") through
  `edit.rs`'s `link_step`, whose `ran` is the editor method's own answer
  (a refused URL, or Edit/Remove off a link, do not run); `changed` is the
  usual body comparison. Read-only refuses all three; `link` still reads
  (the read-only view places a caret).
- The decisions are docxcore's (`editor/links.rs`, unit-tested there):
  `Editor::insert_hyperlink`, `edit_hyperlink`, `remove_hyperlink`,
  `caret_link`, `link_text`, `LINK_COLOR`. A URL is trimmed; `#name` is the
  bookmark `name`; empty, a bare `#` or one with a control character or
  U+FFFE/U+FFFF is refused. `link_text` makes the text one line: line ends,
  tabs and `LINE_BREAK` a space, other controls and `FIELD_CHAR` dropped
  (`selected_text()` across paragraphs joins them with `\n`); blank text is
  the URL, as in Word.
- The link at the caret is the top-level link typing there goes into
  (`locate`: a caret between two inlines is the one before's), so a caret
  just after a link is on it and one at its start after other text is not
  (a link opening the paragraph holds a caret at 0);
  a selection is on a link only inside it (its whole text too). This
  differs on purpose from `doc_view::link_at` (`[start, end)`), which names
  painted words and follows links. Links inside tracked changes or content
  controls are not found.
- Insert wraps when the text is the selection's own and the selection is
  one paragraph's runs (whole bookmark, comment-range, proofing and
  permission markers strictly inside allowed; a link holding one is a
  complex link, `content`): the runs keep their formatting. Decided before
  anything is split; zero-width inlines at either edge stay outside. Else,
  and always while tracking (a paste records it), the selection is
  replaced by one run of the text in the typing props (pending toggles
  included) through `paste`, so inside a link it lands between the link's
  halves: old half, new link, old half, never nested (Word would edit the
  old link instead). A paste at an empty content control inside a link
  goes into the link, so insert refuses (false, no step) when the caret or
  either selection end is at a top-level link holding a content control
  (`pastes_into_a_link`, asked before anything is deleted; #1952 r2). The link look is direct formatting, underline plus
  `0563C1` unless the run has a colour, not a `Hyperlink` style, which the
  package may lack. Remove takes off a `0563C1` colour with its underline
  and a `Hyperlink` character style, nothing else.
- Edit keeps a loaded link's `raw`: its opener is rewritten
  (`package::with_opener_attr`: `r:id` removed, `w:anchor` set or removed)
  and `content_changed` set, so its tooltip, target frame and namespace
  declarations stay; `rel_id` is `None` and `save_package` now writes the id
  it mints into a kept opener (`with_opener_rel_id`), as
  `link_part_hyperlinks` (now over `with_opener_attr` too) does. Both mint
  for the links in text boxes, tracked changes and target-less links'
  content too (`collect_inline_links`), and a text box loads with its
  part's relationships (`load::textbox_blocks`), so a link inserted in a
  text box keeps its URL (#1952 r2). The link's
  own URL and text as `caret_link` reads them are no change (true, no
  step): the URL is compared with `link_url` (so `target#anchor` keeps its
  fragment), the text after `link_text` on both sides (a TOC-style link's
  `\t` and field stay; #1952 r1). New text replaces the link's text (runs,
  tabs, breaks, fields) with one run in the first run's props and keeps its
  zero-width inlines in place (`retexted_content`: a bookmark the wrap put
  inside stays). Edit and remove are not recorded while tracking.
- Harness: `ext-doc-call` takes `{"insert_link": {"text", "url"}}`,
  `{"edit_link": {"text", "url"}}`, `{"remove_link": true}`;
  `caret_format` adds `link` (`null` or `{text, url}`); `ext-doc-document`'s
  runs add `color` and `link` (a complex link's runs in `content` too). The
  `issue 1952` test in `uiharness/cases/ext-doc-edit.uit` links bold
  "Beta", edits and removes the fixture's link, undoes each in turn and
  checks read-only.
- Not done: a Ctrl+K key or a dialog in the view (Ctrl+K bubbles), the
  document tab's own Insert > Link (it could call the same methods), a
  ScreenTip.

## Link hover and the hand cursor (#1916)

- `DocView::on_link_hover(Fn(Option<&LinkHover>, &mut Window, &mut App))`:
  called, deferred (`window.defer`), only when the link under the pointer
  changes (entering one, moving onto another `Inline::Hyperlink` even with
  the same target, `None` off every link, out of the view or the window).
  `DocView::on_link_with_text(Fn(&LinkHover, ..))` is called on every
  follow after `on_link`. `ext::LinkHover { url, text }`
  (`#[non_exhaustive]`): `url` as `on_link` gives it, `text` the whole
  link's, both from docxcore's `hyperlink_shown` (what
  `Editor::caret_link` reads too), so a tab or line break in the link
  stays `\t` / `\u{0B}`. `link_at` is now `link_hover_at` over `para_links`, and
  a link with a target and an anchor gives `target#anchor` everywhere
  (#1873 dropped the fragment).
- Read-only, the pointer is `CursorStyle::PointingHand` over each painted
  piece of a link, in both `LinkClick` modes; editable, no hand (a press
  places the caret) but the hover is still reported. The shared renderer
  is untouched: the view's overlay canvas (after `ext-doc-scroll`, before
  the bars and the Find box) inserts a `Normal` hitbox per painted link
  piece in its prepaint (`link_hitboxes`: the pieces' bounds are set in
  `ext-doc-scroll`'s prepaint, which runs first), clipped by `find_view`,
  and in paint asks for the hand on each. The hand comes after the words'
  I-beam, and the bars and the Find box come after the hand.
- The link under the pointer (`LinkHoverState::under_pointer`) is the
  first piece hitbox passing `should_handle_scroll`, not `is_hovered`,
  which is false after any key. That would report `None` under a still
  pointer on every key. `BlockMouse` overlays still hide it, and the Find
  box (`BlockMouseExceptScroll`) is clipped out. gpui keeps its last mouse
  position on `MouseExited`, so a `pointer_out` latch (set by the exit,
  cleared by a move, as #1869's `pointer_in_window`) keeps a later frame
  from finding the link again. On Windows outside the harness
  `window.is_window_hovered()` must hold too (no `MouseExited` there).
- It is looked at again from Capture-phase `MouseMoveEvent` /
  `MouseExitEvent` window listeners registered in the canvas's paint
  (never stopping an event), and at the end of every paint, so content
  moving under a still pointer (wheel, keys, zoom, a new document) is
  reported after its frame.
- Not reported: a view that stops being drawn while hovered (out of the
  tree, its tab hidden). The host clears its own status then. On Windows,
  leaving the window straight from a link reports `None` at the next
  event or frame. Not done: a hand while Ctrl is held in editable mode,
  links in Print Layout's headers and footers (not in `Words`), the
  document tabs' own link hover.
- Harness: the fixture records `on_link_hover` and `on_link_with_text`.
  `ext-doc-read` (and `ext-doc-set`, as a snapshot) add `hover_count`,
  `hover_calls` (`null` / `{url, text}`), `link_texts`, `hovered` (the
  view's current one) and `link_hand` (read-only and a link piece under
  the pointer: the view's own request, since gpui cannot read the cursor
  back). The `issue 1916` tests in `uiharness/cases/ext-doc-view.uit`
  cover pointer moves, `pointer-leave` and frames after it, a wheel under a
  still pointer, and editable mode with arrow keys. gpui keeps the pointer
  where the previous test left it, so a new tab may report a link at its
  first frame: compare against a snapshot.

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
  page stays white. The palette maps the document's colours against the
  host's page colour (`dark_page_pal`, #2079), so a host need not recolour
  the document's text. The colour paints the view's root, so the padding
  is that colour too.
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

## Find in the embedded view (#1887)

- Ctrl+F (⌘F on macOS) in a focused `DocView` opens the view's own Find
  box at its top right (`doc_view/find.rs`; drawn by `DocView::find_box`
  over the scroll bars, in the theme's chrome palette), in either mode; it
  never reaches the suite. With the box open, Ctrl+F gives its field the
  keys again. Opening, or that, selects the field's whole text. Opening seeds the query from a
  one-line selection, as a document tab's Find does, and stays on the
  match the editor is already on (a reopen after Escape), else steps from
  the caret. Escape (whatever the field focus) closes it; the match stays
  selected and the query is kept. With the box closed Escape is the
  editing map's again (it collapses a selection).
- Matching and stepping are the document tab's: `Editor::find_visible`
  (case-insensitive) and `find_step_in`, by index while `is_at_found`,
  else from the caret; typing finds again from the top. An empty query or
  one with no match changes nothing in the document. The current match is
  the editor's selection; the other hits paint as amber quads at 35% over
  the glyphs (`find::hit_quads` over this frame's `Words`, in the
  `viewport` canvas, clipped to the view clear of the bars only,
  `find::visible_hits`: the box, drawn after, covers its own part, and a hit
  on the top line left of it shows), not in the shared
  renderer. A step reveals the match's start piece in y and, unlike the
  caret's reveal, in x too (`find::reveal_dx`: a match in a table wider
  than the view). `find::find_view` (clear of both bars, and below the open
  box's `BOX_BAND`, across the view's whole width) is what every reveal,
  `caret_in_view` and `current_in_view` use, so neither a match nor the
  caret is revealed under the box; a caret typed on the top line left of
  the box costs one band's scroll.
- Keys (`find::find_key`, run before `edit_key`): Escape and F3 /
  Shift+F3 while open; while the field has the keys, Enter / Shift+Enter
  step, typing (`edit::typed_text`, the editing map's rule: AltGr types,
  a plain Alt off macOS never does), Backspace/Delete (Ctrl too),
  Left/Right/Home/End (Shift moves as the plain key), Ctrl+A/C/X/V on the
  field (the view's `ClipboardStore`; Ctrl+C/X copy nothing without a
  field selection), the scroll and zoom keys to the read-only map
  (`view_key`), Tab / Shift+Tab nothing (`tab_action`), any other unmodified key
  dropped and any other chord (Ctrl+W, Ctrl+S, Ctrl+Tab, Ctrl+B, Alt,
  F-keys) on to the suite without the editing map. A press in the document
  gives the keys back (`begin_select`, `set_caret`, the root's press); the
  box stays with its hits. A press on the box never reaches the document
  (`block_mouse_except_scroll`, the wheel still scrolls).
- The suite's root never opens the document tab's Find bar on an extension
  tab, nor routes keys to it there (`on_key`'s two `active_is_ext` checks):
  that bar is drawn on document tabs only, and a bar left open by one used
  to take the letters and Escape a read-only view passes on.
- Harness: `ext-doc-read`'s `find` (`open`, `query`, `field_focused`,
  `count`, `current` 1-based or null, `current_path`, `current_in_view`,
  `hits_painted`: quads painted in view in the last frame, the current
  match excluded), the regions `ext-doc-find:<field|prev|next|close>` and
  the registered input `ext-doc-find` (`inputs.rs`, `Surface::Ext`;
  `inputs::press` / `read` take the `App` to reach the view). The
  `issue 1887` tests in `uiharness/cases/ext-doc-view.uit`.
- Not done: Replace, Match case and whole words, a host API to open Find
  from an extension's ribbon, F3 with the box closed (it bubbles as before),
  IME and dead keys in the box (the view's plain key-downs, as #1947).

## Print Layout in the embedded view (#1888)

- `DocView::set_layout(DocLayout)` / `layout()`: `DocLayout::Web` (the
  default, the flow) or `Print` (`#[non_exhaustive]`). The setter notifies
  only on a change and reveals the caret; the selection, zoom, editing
  mode and Find box stay. `page_count()` is the Print Layout page count in
  either layout (`print_sheet::page_count`, the live body, all markup: a
  document tab's `doc {}` `scroll.page_count` flows the markup view's
  body, equal for a file without revisions).
- Sharing, not a second renderer: `print_sheet.rs` holds what both Print
  Layouts use: `band_el` (Docxy's `build_band` closure moved verbatim; its
  line-number `slot_for` is a parameter, the view passes `|_| None`),
  `frame_geom` (a header/footer frame's `FrameGeom`, #767),
  `canvas_color`, `CANVAS_PAD` (24 px), `SHEET_GAP` (18 px) and
  `SHEET_BORDER`. Docxy's sheets (header/footer editing, rulers, line
  numbers, double-clicks, `page_follow`) stay in `Docxy::render`.
- The view's plan is `print_sheet::page_plan` over its live editor: the
  flow, slots, parts, the shown parts' blocks and page values. Its last
  section is `print_sheet::last_sect`: the body's trailing sectPr, else
  the package's, else `""`. That differs from a tab's `final_sect_pr`,
  which reads none without a package: a package-less HTML mail keeps its
  225-twip margins (#1928). The unit tests in `print_sheet/tests.rs` hold
  the view's plan equal to a document tab's (`tab_page_flow`,
  `hf::slots_of`, `hf::resolve`) for `header-inherit.docx`,
  `fixture_document()` and `sections_document()`.
- `doc_view::print_sheets` draws each page as Docxy does: a sheet of its
  section's size (`content_look`'s page colour, so the host's
  `page_background` paints the dark sheet), the header in the top margin
  at the header distance, the footer in the bottom margin, the bands with
  their own section's margins and columns, framed header/footer
  paragraphs at their frame. The body goes through the view's
  `RenderCtx` with `fit_width: None, flow: false, paras: None`, so
  selection, links, Find and the caret reveal work on pages unchanged;
  headers and footers are `hf_els` / `hf_para_el`, drawn only (not in
  `Words`: not selectable, not followed). Canvas: `CANVAS_DARK` under a
  dark sheet, `2b2b2b` for a light sheet on a dark theme, else `9a9a9a`.
  `page_inset` pads Web Layout only. A sheet is at least its page's
  height (`min_h`): one whose content runs past the flow's estimate
  grows, as in a document tab.
- In Print, `text_w`, `column` and `painted_gen` are left as Web set them
  (Web's table fit wants `text_w` for the switch back); `harness_json`
  reports `text_width`, `column_left` and `table_widths`' fit against
  `null`, and `layout_json` answers `painted: false`.
- Harness: `ext-doc-set {"layout":"web"|"print"}`; `ext-doc-read` adds
  `layout`, `page_count`, `pages` (each sheet painted last frame: `page`
  1-based, view-relative `x`/`y`/`w`/`h`, `header`/`footer` part names or
  null, `frames` relative to the sheet with `header` and `right`) and
  `canvas_color`; `ext-doc-word {"text"}` gives a painted word's box and,
  in Print, its `page` and `page_x`/`page_y` on the sheet (the sheet's
  1 px border counts: a 1" margin is `page_x` 97);
  `ext-doc-load {"fixture":"sections"|"header-inherit"}`
  (`ext_fixture::named_document`; `header-inherit` is `include_bytes!` of
  the fixture, loaded by `load_bytes`). `sections_document()`: Letter
  with a header and a footer whose PAGE number is framed at the right
  margin, a landscape section with 0.5" margins, two 3" columns 0.5"
  apart with `w:sep`, then a continuous one-column section. The
  `issue 1888` tests in `uiharness/cases/ext-doc-view.uit`.
- Not done: rulers, line numbers, header/footer editing, selection or
  links in headers/footers, the document's page colour (#651), the page
  tip, page navigation or a current-page API, side-by-side pages.

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
  View (Arrange: the Sort drop-down, the density gallery; Window: Exit,
  #1923's `ext::CloseTab`). `ext-ribbon-read`
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

## Backstage pages and closing a tab (#1923)

- `TabSurface::backstage_pages(&self, cx) -> Vec<ext::BackstagePage>`
  (default empty) adds pages to File while the tab is active. A
  `BackstagePage` (`#[non_exhaustive]`, built by `new(id, label, view)`,
  `.icon(RibbonIcon)`, `.group(BackstageGroup)`) is a rail entry and the
  view drawn as the page body. `BackstageGroup::Top` (the default) entries
  go right after Back, ahead of the suite's items (Outlook's Info leads its
  rail; a document's Info would follow them, but an extension tab is never
  a document); `Bottom` entries go after Account, before Close (Outlook's
  Office Account, Options, Exit). An empty id or one an earlier page used
  is left out. `backstage_rail` (pure, `backstage_rail_tests`) composes the
  list; with no pages it is exactly `backstage_rail_items`, so the public
  `suite` binary's rail is unchanged.
- Read per draw, not cached like the ribbon (#1870): a rail entry needs no
  `&'static` string (element ids and probes own theirs), and the icon goes
  through `ext_ribbon::register_icon`, interned per content hash, so a
  re-read costs a hash. So no "pages changed" call and no stale entry. It
  is read any number of times while the tab is active (each File draw,
  twice a frame: `backstage_rail_now` and `shown_ext_page`, each key while
  File is up, the harness's verbs): the contract is that it is cheap, free
  of side effects, and returns the same views (`AnyView` clones), not new
  entities. The icon is drawn as a 16 px mask in the entry's ink
  (`group_hover` gives it the hover's).
- State: `bs_ext: Option<(EntityId, String)>`, the surface's view entity
  and the page id, set by `select_ext_backstage_page` (the entry's click).
  Every other page setter (`show_backstage_open_page`,
  `show_backstage_new_page`, `open_account`, Info, `open_export`) clears
  it, and it clears their flags, so `open_backstage` and Back reset it.
  `shown_ext_page` draws it only while the active surface is that entity
  and still lists the id; else File shows its default page. A cancelled
  Open… pick (`finish_open_file`) leaves it, so File stays on the page
  (#1078).
- The page body is not in `backstage_scroll_column`: a `backstage-content`
  column (same id and probe) holding the view `size_full`, which scrolls
  itself as a tab body does. `backstage_scroll_key` is skipped under it
  (`bs_scroll` is not drawn). Keys the view does not stop bubble to
  `on_key`'s backstage branch as on the suite's pages; Esc does nothing
  there, as on theirs (Esc → Back for every File page is a follow-up).
  `focus_ext_surface` returns early while File is up, so the tab's view
  does not take the focus back from the page.
- `ext::CloseTab` (an action) is handled on the window root
  (`KeyRouting`, both renders) by `close_ext_tab` (`close.rs`): on an
  extension tab it is `backstage_close` (the Info edit applied, File
  closed, `close_tab` of the active tab: the close button's path), on any
  other tab nothing. Only the active tab's view or File page is drawn, so
  a dispatch from the view, its page or `ribbon_action` (where the focus
  may be the suite root under KeyTips) is the active tab's. A late
  dispatch (a spawned task, after the user switched tabs) closes whichever
  extension tab is active then: the doc says to dispatch in response to
  the user's own input. Rejected: an `ext::close_tab(entity_id)` naming
  the tab (more API; the issue asks for an action).
- Fixture: every variant adds `fx.account` "Fixture Account" (Top, the
  Reply icon) and `fx.options` "Fixture Options" (Bottom), each a
  `FixturePage` (made once in `factory`) counting its renders and
  recording where it and its "Close tab" button (which dispatches
  `CloseTab`) painted. `ext fixture ribbon`'s View tab has Window: Exit
  (`fx.exit`), whose `ribbon_action` dispatches `CloseTab`.
- Harness: `backstage` replies `items` (the composed rail), `ext_items`
  (`id`, `label`, `group` top/bottom, `icon`) and `ext_page` (the shown
  page's id, or null); `backstage-layout` (and `backstage-page`) add
  `ext_page` and report `page` `ext` while one shows (their scroll fields
  are as the suite's page last drew them). `ext-page-read {"page"}`
  replies `renders`, `bounds` and `close_bounds` (null until painted),
  reached through the surface's own `backstage_pages`. `ext-close-tab {}`
  dispatches `CloseTab` from the focused element (`window.dispatch_action`),
  which on the fixture's tab must be inside its view (refused otherwise),
  and replies `from` (`view` or `suite`); gpui runs it after the reply, so
  a case draws a frame before reading the tabs. Regions: `bs-ext:<id>`
  (`Region::BackstageExt`, the entry's probe) and `ext-page-close:<id>`
  (the page's button, from the fixture). `uiharness/cases/ext-backstage.uit`
  drives it, and `ext-ribbon.uit`'s `issue 1923` test the ribbon's Exit.
- Not done: opening File straight on an extension's page, a selected state
  drawn in the rail (the suite's items draw none), colour icons, macOS
  menu-bar entries, extension entries on the Start screen (no tab).
