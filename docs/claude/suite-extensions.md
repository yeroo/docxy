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
  ribbon row (the view draws its own), its root focus goes on to the view
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

- `ext::DocView` (`suite/docxy/src/doc_view.rs`, with `ext::LinkClick`) is a
  public gpui entity an extension's view embeds: `cx.new(|cx|
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
- Read-only: no caret is drawn (`caret_path` empty), nothing edits.
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
  is put back (as the Project chart does). Table selection zones do
  nothing. Every other key bubbles to the suite; Tab and Shift+Tab (global
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
  `set_content_dark(true)` on a dark theme, which draws the theme's own.
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
  content_dark, focused, links_followed, link_calls), `ext-doc-set
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
- Out of scope: Find (#1887), Print Layout (#1888), a caret and keyboard
  selection, table selection zones, a context menu, anchor links scrolling
  inside the view (they reach the callback as `#name`), macOS ⌘-key
  routing for the view (untested).
