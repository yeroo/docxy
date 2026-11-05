# macOS

Notes moved from `CLAUDE.md` (#1903). Read them before changing macOS-only behaviour: the menu bar and the ⌘/⌥ text navigation keys. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## macOS menu bar (#1071)

- `suite/docxy/src/macos_menu.rs` holds the bar as data (`MENUS`). A chord
  reaches one path: `text_input::route` stops the chords `on_key` owns
  (`Role::Window`) and leaves menu-only ones (`Role::Menu`: ⌘Q, ⌘H, ⌘M, ⌘O,
  ⌘, and ⌃⌘F) to AppKit without `on_key`, so on macOS ⌘M minimizes instead
  of indenting (⌃M still indents). Menu bindings live in a key context
  nothing sets, so gpui never matches them in the window; a click on a
  window-owned item types its chord into `on_key`. ⌘⇧Z redoes and never
  repeats.
- gpui's menu callbacks `borrow_mut` the app, so a synchronous native dialog
  (rfd) must run through `macos_menu::native_modal`, which swaps in a plain
  AppKit menu bar meanwhile whose Edit items target an object of its own:
  a nil target falls back to gpui's app delegate, which deadlocks on an
  item gpui did not make. A unit test fails on a bare dialog.
- Dock > Quit, the Quit Apple Event and logout call `terminate:` directly
  (#1229). `install` adds `applicationShouldTerminate:` to gpui's app
  delegate: it reads only an atomic (never the app, which a native dialog
  may hold) and cancels, and a task then runs `quit` once the app is free
  (or ends the process when gpui has no live window: the last window's
  close keeps its registry entry). Every exit docxy starts itself goes
  through `macos_menu::end_process` (a unit test scans for a bare
  `cx.quit()`), or `resume_quit`'s apply phase, so its terminate goes ahead.

## Mac text navigation (#1073)

- `suite/docxy/src/mac_nav.rs` maps ⌘←/→/↑/↓ (line start/end, document start/end),
  ⌥←/→ (word start / just after the word, `Editor::move_word_end_caret`) and ⌥⌫
  to editor moves on macOS only; `on_key` tries it before the Ctrl split, and
  lowers the KeyTips the ⌥ key-down raised. Home/End are paragraph offsets, so
  ⌘←/→ are too, not visual lines. Protected View and final documents take the
  ⌥←/→ word moves (caret only); ⌥⌫ stays refused.
