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
  open), letters stay with `on_key`. Off macOS the root never stops a key's
  propagation: a handled key-down on Windows is never translated (#1072).
- Windows registers the handler too, for WM_CHAR only (#1139): Unicode-packet
  text (`SendInput` `KEYEVENTF_UNICODE`, a `VK_PACKET` key-down gpui drops)
  arrives nowhere else. Each key-down `on_key` typed owes its WM_CHAR text
  (`owed_text`: printable, no Alt without Ctrl), and each WM_CHAR character
  pays only the oldest debt that starts with it, preferring one sure to be
  paid (not `may_not_come`), or, matching none, is typed: matched by text, not
  counted. A packet whose character a pending debt starts with pays that debt,
  and the key's own WM_CHAR is then typed. No match drops another debt. A
  plain dead key owes nothing; an AltGr dead key owes its accent until it
  lapses. A WM_CHAR can come after its key-up and later key-downs (gpui drains
  input alone under load), so no key-up ends a debt; it waits up to
  `WM_CHAR_KEEP` (60 s, with a 4096-debt cap), hygiene only. Packet text
  queued behind typed keys under load may land after their text. The handler's
  `accepts_text_input` is false there, so the Windows IME stays off. Linux
  registers none (it would get every key `on_key` typed). The harness's
  `win-char` sends WM_CHAR, and `real-type`'s `"wm_char":true` (always on
  Windows) or `"late"` replays each typed key's twin.
- A dialog field takes the character ⌥ made on macOS: `modal_takes_key`
  hands the dialog `text_input::dialog_stroke`'s plain typed key (#1162).
  On Windows an AltGr key (Ctrl+Alt with `prefer_character_input`) under a
  dialog owes the dialog (`dialog_altgr`, decided before `on_key`): its
  WM_CHAR types it while a dialog is open and nothing once none is, and an
  AltGr dead key, which sends none, types nothing. Under load that WM_CHAR
  may land after later keys' text, or in the next field after a Tab. A plain
  Windows dead key still types its accent into a dialog field.
  `edit_key` still swallows every ⌥ key, and Windows' plain Alt is never text.

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
  Font colour and highlight cross both ways: highlight goes out as
  `\highlightN` (Word) and `\cbN` (Cocoa), and comes in as the nearest Word
  name (`docxcore::highlight`, the one name → RGB table); white is none.
- The harness's private clipboard holds the RTF beside the text, and any
  plain write drops it. `clipboard {"action":"write","rtf":…}` plays another
  app's rich copy; `read` reports `rtf` and `rtf_bold`.

## Sheet copy and paste with other apps (#1248)

- A sheet copy or cut puts an HTML table (`gridcore::clip_html::html_table`:
  display text and inline cell styles, never formulas) beside its TSV, through
  `rich_clip::write_html` (macOS `public.html`, Windows "HTML Format" as CF_HTML,
  nothing on Linux), under the same `rtf_follows` read-back gate as the RTF. On
  a Windows OS clipboard the text ends records with CRLF (`os_text`); the
  recorded text and the harness's private clipboard stay LF.
- A sheet paste takes the live grid clip, else another app's HTML table, else
  its RTF table (`foreign_table`: cell texts only, each read as typed through
  `fields_block`), else the text as TSV. A table counts only when it is the
  whole copy (no other text, one table), and docxy's own table (marked
  `data-source=docxy`) never does: its exact TSV, with the `'` a text cell like
  `007` needs, is what pastes. Paste Special still reads the text.

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

## Title label and accent per tab type (#1473)

- The suite's title bar names the selected tab's type in one word (`doc`,
  `sheet`, `project`; Mail `mail`, no tab `doc`), not the document, whose name
  stays on its tab. The OS window title is unchanged (`doc_name::window_title`).
- `suite/docxy/src/accent.rs` holds the accent per type and theme (doc keeps
  `BRAND`, sheet green, project yellow; the sheet and project shades hold white
  File text at 4.5:1). `Docxy::render` calls `accent::set` before building any
  chrome and the ribbon, backstage and title bar read `accent::brand()`; every
  other `BRAND` use (dialogs, comment and find bars, carets, Gantt) is not
  accent. A `.hover` style is built at once, so it reads the accent during
  render too. `accent_now` takes the theme from the preference, as render sets
  the accent before `apply_theme`. `tab-list` reports `title_label`, `accent`
  (from the tab) and `drawn_accent` (what this window's last render set); `uiharness/cases/title-accent.uit` asserts them.
- The Light/Dark/Auto theme button is not in the title bar: View > Theme
  (doc `Act::DarkMode`, `SheetAct::Theme`, `ProjectAct::Theme`) and Backstage's
  Settings Theme row (always reachable: the File button is drawn in every state,
  and the only place for Mail, a failed load, no tab, Protected View and a
  minimised ribbon, where `ribbon_body_hidden` draws no View tab) call
  `cycle_theme`, which says the new setting on the status line.
  `title_left_caps` no longer reserves a theme width.

## Manual hyphenation (#1266)

- Layout > Hyphenation > Manual (`suite/docxy/src/manual_hyphen.rs`) walks the
  words that may take an optional hyphen, from the caret or over the selection,
  one "Manual Hyphenation" dialog each (Yes inserts, No skips, Cancel stops; the
  `-` in the "Hyphenate at:" box can be moved). It proposes every eligible word,
  not only those at a line end as Word does: docxy's layout never hyphenates
  and wrapping differs per host. The candidate is the editor's selection; the
  dialog keeps the walk's end and the count in hidden labels.
- `docxcore::hyphenate` is a dependency-free English-oriented syllable
  heuristic, not a dictionary. `Editor::insert_soft_hyphen` puts U+00AD in a run
  of its own with `hyphen_elements`, so it saves as `w:softHyphen` (a plain run's
  U+00AD would stay literal text), as one undo step.

## Page Setup / Columns: Apply to Selected text (#1272)

- `Editor::wrap_selection_in_section` wraps a text selection inside one section
  in section breaks and edits only the middle section, as one undo step with
  the selection kept on its text. The middle and the section after start as
  the dialog says (Continuous for Columns, NextPage for Page Setup, whose
  Section start overrides the middle only when the person changed it). A
  selection edge at a paragraph edge cuts by the neighbouring paragraph's mark,
  never an empty paragraph (except beside a table, before the text or
  ending it, which cannot carry a sectPr); an edge already at the section's edge adds no
  break (and keeps the section's existing start type).
- "Selected text" is offered, and the default, only when
  `selection_section_wrappable` (not a whole section, not across sections or in
  a table cell). Line Numbers keeps its own Apply to without it.

## Layout settings.xml edits are undo steps (#1273)

- `DocSettings` (docxcore) holds `w:evenAndOddHeaders`, `w:autoHyphenation`,
  `w:mirrorMargins` and `w:gutterAtTop`; snapshots carry them and
  `DocTab::set_dirty` writes them to the package. Layout > Margins, Hyphenation
  and Page Setup's OK write the package first (a settings part docxy cannot edit
  refuses with nothing changed), then set the editor inside one `one_step`. The
  Hyphenation dialog's caps, zone and limit still write the package without
  undo; terminal `docxy` has no settings sync.
