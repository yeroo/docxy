# CLAUDE.md

## DOCX Bidi Rendering

- Keep `docxcore` dependency-free. It exposes `RenderOptions::bidi` and the
  `BidiProjector` trait, but it must not depend on `unicode-bidi`.
- Keep `unicode-bidi` in `docxy`. Terminal `docxy` passes the projector into
  rendering so wrapped DOCX lines are projected into Unicode bidi visual order.
- Editor storage, copy/export, save offsets, undo/redo, and automation offsets
  remain logical. Visual arrow movement, Home/End, vertical movement, clicks, and
  drags use `LineMap` visual caret stops.
- Hosts that do not provide a projector, including the current wasm-backed Offxy
  editors, should pass `bidi: None` and will render DOCX text in identity order.
- The editable-HTML page (`htmlbundle/web/`) renders DOCX as DOM from
  `docx_doc`, not as projected grid lines, so the browser does bidi itself;
  its selection maps to logical editor offsets through `data-o` segments.

## Text input (dead keys, IME)

- On macOS the window root (`KeyRouting`) leaves printable keys to AppKit and
  registers `text_input`'s `EntityInputHandler`, which replays a commit of the
  key's own text as that key and types composed text as keys into `on_key`;
  marked text types nothing. While KeyTips or a menu is up (and no dialog is
  open), letters stay with `on_key`. The handler is macOS-only (Windows and
  Linux would hand it every character `on_key` already typed), and off macOS
  the root never stops a key's propagation: a handled key-down on Windows is
  never translated (#1072).

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

## Rich copy and paste with other apps (#1074)

- A document copy puts RTF beside its plain text (`Clip::to_rtf` in
  `docxcore/src/clip_rtf.rs`, the Save As writer with the tab's styles), added
  after gpui's text write by `suite/docxy/src/rich_clip.rs`: macOS
  `public.rtf`, Windows "Rich Text Format", nothing on Linux. It is written
  only when the text write took (`rtf_follows`). A document paste takes the
  window's own clip while the clipboard still holds its text, else the
  clipboard's RTF, else its text (`doc_paste_clip`). A partial copy's RTF ends
  without `\par`; the paste takes the trailing mark from the plain text.
- The harness's private clipboard holds the RTF beside the text, and any
  plain write drops it. `clipboard {"action":"write","rtf":…}` plays another
  app's rich copy; `read` reports `rtf` and `rtf_bold`.

## Build info

- `buildinfo` is the one place that stamps binaries (commit, last merged PR, kind,
  manual build). Keep it dependency-free; hosts parse its `json()` text into their own
  JSON type and pass their own `CARGO_PKG_VERSION` (the suite, xlsxy, yppxy and lookxy are 0.1.0, docxy 0.5.0).
- `ci.yml` sets `DOCXY_BUILD_KIND: ci` per job and `release.yml` sets `release`, but
  never on `ui-sweep-linux`: `uiharness/cases/build-info.uit` asserts `local`.

## Dialogs and typed input (#1029)

- Every dialog is in `suite/docxy/src/dialog/catalog/entries.rs`: a `DialogId`
  exists only there, so every dialog carries a catalogued id. Give a new
  dialog its own entry (how a person opens it, its editable controls, samples,
  what OK shows); reusing another dialog's id is caught only by review and by
  `dialog-catalog-check` where a case opens it. Regenerate the cases with
  `UPDATE_INPUTS_TYPING=1 cargo test --manifest-path suite/Cargo.toml
  inputs_typing_case_is_current` and commit `uiharness/cases/inputs-typing-*.uit`;
  never edit those by hand.
- The generated cases type with `real-key`/`real-type` and focus by real click
  or Tab only. An input outside dialogs goes in `suite/docxy/src/inputs.rs` with
  the issue that covers it. See `qa/inputs-typing.md`.
