# Help

Notes moved from `CLAUDE.md` (#1903). Read them before changing the help manuals (`helpdocs`) and the suite's Documentation tab. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Help manual bundles (#1022, slice 1)

- `helpdocs` (root workspace, dependency-free like `buildinfo`) reads an
  editor's manual bundle (`docs/manual/<word|excel|project>/`: `REVISION`,
  `contents.txt`, `<id>.md` with front matter, `images/`; the format is
  `docs/manual/README.md`). `Manual::from_files` is the one reader (`load`
  goes through it; `\` reads as `/`), and a bundle always loads, its faults
  in `problems`. `md` parses the fixed markdown subset (anything else is
  literal text), `search` ranks title 10 / heading 5 / body 1 per hit up to
  5 per term, all terms required, ties in contents order, `resolve` gives
  the exact `keys` page, else the area's (`slug` of the tab name), else the
  root, and `coverage` is the report.
- The suite's `every_ribbon_command_has_a_help_target`
  (`help_tab/tests.rs`) resolves every command id (all `Control` kinds,
  contextual tabs, the table/H&F/Gantt Format tabs, the sheet's
  `SHEET_RIBBON`) against `helpdocs/tests/fixture/<editor>` until real
  manuals are published. It fails only for a strict area
  (`# strict: help` in the word fixture). Group launchers are not in the
  walk: a launcher runs an act and carries no id. Their dialogs resolve as
  `dialog:<id>`: `every_dialog_has_a_help_target` reports every catalogued
  dialog's (#1855). `helpdocs::resolve_area` is F1's area-only resolve (the
  area page, else the root; never a key). The suite shows a bundle's
  `REVISION` in About and the Help footer (#1857, below).
- The terminal editors' F1 reader (#1856, notes in
  `docs/claude/terminal-editors.md`; its ratatui side is
  `backstagecore::help`) uses four more modules, all dependency-free. `text::lay` lays a page out as wrapped lines of spans
  with a look, the width measured by the host's closure: headings with
  their slug's line, hanging list indents (an item's children of every kind
  under it), verbatim hard-wrapped code, tables padded and aligned, else
  stacked `Header: cell` lines, `[image: alt]`. Links are numbered in
  `md::links` order. `reader::Reader` is the viewer's state: scroll, Tab
  links, follow, Back/Forward, a synthetic Contents page. `locate` finds
  the installed manual, else the built-in one. `embed` writes a build
  script's `include_bytes!` list.

## Documentation tab (#1855)

- `help_manual` finds an editor's manual (`Editor`: word for documents, Mail
  and no tab; excel; project): `<config root>/docxy/manual/<editor>/`, else
  the one built in from `docs/manual/<editor>/` (rust-embed), read once per
  process; an installed one through `helpdocs::bundle::files`, the walker
  `Manual::load` uses (no link is followed). A harness instance reads only its sandbox (a script's `manual
  <editor> <dir>` line copies a bundle there, `launch::install_manuals`), so
  no case depends on `docs/manual`; unit tests set `help_manual::tests::Override`
  (none by default). A bundle with no page is none, and then Help and F1 say
  `DOCS_COMING` as before.
- `Kind::Help` / `Surface::Help(HelpView)` (`help_view`): one Help tab per
  editor per window, a new one right after the active tab, reused (a history
  step) by the next F1. It is never dirty, saves nothing (Ctrl+S and Save As
  say `HELP_NO_SAVE`), is left out of `session.json` (`session_active`
  remaps the active index) and moves with New Window/tear-off like any tab.
  The ribbon strip shows File only (Backstage's Theme row stays reachable);
  there are no KeyTips. `valid_ribbon_tab` keeps `ribbon_tab` as it is for
  `Kind::Help`, so the tab behind (Layout, Resource, ...) is there on return. `tab-list` kind `help`, title label `help`, title
  `Help: <page title>`.
- `help_view::lay` lays a page out as one row per top-level block (so an
  anchor's row index is its block's): list markers, padded tables, images
  found only for PNG/JPEG/GIF/BMP bytes, inline runs with their look and link
  index (`md::links` order, the `help-link:<n>` numbering). `render.rs` only
  draws rows; colours are `Pal`'s. Pictures go through the tab's own
  `pictures::Cache::file_image` under `help:<editor>`, so the release paths
  see them. An anchor scrolls with gpui's `scroll_to_top_of_item`, which lands
  in the next frame's prepaint (a harness read waits with a `shot`);
  `help_view::host::top_block` reads the block at the top (gpui's `top_item`
  answers the one above when a top sits on the edge).
- Keys (`help_key`, after `document_key` and F1 in `on_key`): Alt+Left/Right
  and ⌘[ / ⌘] walk the history; Ctrl+F or a click focuses the search box
  (`field_edit`, Ctrl+A/C/X/V, Enter opens the first hit, Escape clears, then
  gives the keys back); the arrows, Page Up/Down, Space, Home and End scroll.
  Tab and Shift+Tab are actions (`tab_key`/`shift_tab_key`), so they take the
  keys from the box there (`help_search_release`), before the find bar's branch: a
  document's Find bar left open behind the Help tab never moves. The box is
  not in `entry_owns_keys`, so F1 still acts there.
- F1 (`help_tab::help_f1`): the area page of `ribbon_tab_name(ribbon_tab)`, or
  on a Help tab its manual's root. Help › Help asks for `help`/`Help`, in
  the Help tab's own manual when one is active (the macOS menu bar). F1 over
  a tab dialog (`dialog_f1`, first in `dialog_takes_key`) opens `dialog:<id>`
  only over a plain form (`dialog_takes_help`: not a message box, popup,
  reload/recovery/close/quit prompt, User name or About, no quit asking) and
  only with a manual; otherwise the dialog swallows it as before. Task
  Information's Help and a URL-less plugin Help do the same from
  `dialog_press`. The dialog stays open on its tab. F1 under an app dialog,
  in the backstage or with a menu open is unchanged.
- Harness: `help-open` (`page`+`anchor`, `command`+`area`, `dialog`+`area`,
  or none for the root; `editor` picks the manual; refused under a dialog,
  without a manual, for an unknown page) and `help-read` (page, title, via,
  headings, links, images, contents, query, hits, back/forward, problems,
  `top_block`, `top_heading`, `scroll_y`; `{"open":false}` off a Help tab).
  `pointer-click {"help":"back|forward|contents:<id>|hit:<n>|link:<n>"}`;
  the search box is the registered input `help-search` (`inputs.rs`).
  `tab-list` reports `count`. `uiharness/cases/help-docs.uit` drives it;
  `qa/help-docs.md` is the offline manual case. F1 on a hovered ribbon
  command is #1869 (below); on panes, #2077.

## Build-pinned manual revision (#1857)

- `Manual::revision` is `REVISION`'s first line, trimmed; a blank one is
  `None` (`from_files`), so no reader shows an empty commit.
  `Manual::short_revision` cuts a commit (all ASCII hex, longer than 7) to
  7, as `buildinfo`'s `short_commit` does, and keeps anything else (a tag,
  the fixtures' `...fixture`) whole.
- `help_manual::Bundle::source` is where it was read from:
  `Source::Installed` (`read_dir`: `<config root>/docxy/manual/<editor>/`,
  a harness sandbox, the test fixtures) or `Source::BuiltIn` (the embedded
  files). `name()` is the harness word (`installed`, `built-in`), `words()`
  what a person reads (`installed`, `built in`).
- About docxy suite ends with `word manual`, `excel manual` and
  `project manual` label rows (`about::manual_rows`): `<revision> (built
  in)`, `<revision> (installed)`, `unknown revision (<source>)`, or `none`.
  Copy appends them after `summary:`, labels padded to 16 among themselves.
  `--version`, the crash log and `buildinfo` are unchanged.
- Both About and `app-info`'s `manuals` (`about::manuals_json`: per editor
  `revision`, `short`, `source`, or null) go through the app's
  `help_bundle`, so they name the manual Help shows, and a harness instance
  sees only its sandbox. Scripts assert `app-info`, not `dialog-read`: the
  About rows' indices shift with the build's optional `issue` row.
- The Help tab's page pane ends in a one-line footer,
  `help_view::footer_text`: `Word manual · revision 0123abc · built in`
  (`revision unknown` without one). The body and its scrollbar sit in a
  relative wrapper above it; the `help-page` probe stays on the whole pane.
  `help-read` reports `revision`, `source` and `footer`. Asserted in
  `uiharness/cases/help-docs.uit` and `build-info.uit`.
- The terminal editors' F1 reader and File › Info do not show it yet:
  `helpdocs::locate` returns a bare `Manual` with no source, and the
  reader's footer row holds hints and link notes (a follow-up issue).

## F1 on a hovered ribbon command (#1869)

- F1 with the pointer resting on a ribbon command opens that command's page
  (`help_tab::f1_command_ask`: its id, the ribbon tab shown as the area, so
  `helpdocs::resolve` falls back to the area page, then the root). No
  ScreenTip has to be up, unlike Word. Off every command, and on a Help tab,
  F1 is as before (`f1_ask`). F1 under a dialog, with an entry owning the
  keys, in the backstage or with a menu open (`menu_key` spends it, so an
  open split menu's items have no F1) is unchanged.
- No hover state: every ribbon command element draws a `ribbon-cmd:<id>`
  probe (`help_tab::ribbon_cmd_probe`), and `help_f1` hit-tests the pointer
  against the probes of the frame on screen (`ribbon_cmd_at`: the last drawn
  wins). F1 clears `ribbon_flyout` first, so `help_f1` must read the probes,
  never the flyout state: a command in an open flyout is still on screen.
  Hover listeners would re-render the whole root on every move and go stale
  under a still pointer. gpui's hitbox hover is false after a key, so it
  cannot answer for F1.
- The probes: `cmd_probe_els` (with an extension's `ext-ribbon-cmd:<id>`) in
  `icon_btn`, `large_btn` and `dropdown_btn`; the whole split (both halves)
  in `split_btn`; `combo_box`; a gallery by its id on its container, in
  place and in a flyout, and on a swatch gallery's More button
  (`swatch_gallery`, `style_gallery`, `table_style_gallery`); the sheet's
  every shape (`sheet_cmd_probe`: `sheet_ib`, `sheet_gb`, `sheet_rb`,
  `sheet_check`, `sheet_lb`, `sheet_combo`, `sheet_numfmt_combo`,
  `sheet_dropdown_btn`, a `Shape::Split` whole). A probe fills its nearest
  positioned ancestor, so each of these is `relative`. A disabled command
  keeps its probe: F1 opens its page, as in Word. Group launchers carry no
  id and draw none.
- gpui keeps `mouse_position` after the pointer leaves the window.
  `pointer_in_window` is set by a window-level `MouseMoveEvent` listener and
  cleared by a `MouseExitEvent` one (the root's `gesture_listeners` canvas,
  first, in the capture phase; they never stop the event and never notify).
  gpui on Windows sends no `MouseExited` (`WM_MOUSELEAVE` only clears the
  window's hovered flag), so `help_pointer` there, outside the harness, also
  needs `window.is_window_hovered()`. It is read nowhere else: on macOS it
  means the window is active, and the harness's dispatched events never set
  it.
- Harness: the region `ribbon-cmd:<id>` (a command drawn twice, a swatch
  gallery with its More flyout open, is the first on screen) and the verb
  `pointer-leave` (a dispatched `MouseExited`). Cases: the test "F1 with
  focus on a ribbon command opens that command's page" in
  `uiharness/cases/help-docs.uit` (word fixture), and
  `uiharness/cases/help-f1-sheet.uit` (excel fixture: help-docs.uit asserts
  no Excel manual is installed).
