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

## Terminal docxy Layout tab (#1274)

- `docxy/src/layout_tab.rs` is terminal `docxy`'s Layout ribbon tab (after
  Design): Margins, Orientation, Size, Columns, Breaks, Line Numbers and
  Hyphenation are `PickerKind` list pickers (dialog-only rows of the suite's
  menus are absent). Insert has no Layout group. The presets duplicate
  `suite/docxy/src/layout_tab.rs`; keep their values the same.
- Section picks edit `body_editor_mut().target_sections()` through
  `edit_section_setups` (the section(s) at the caret or selection, even with
  a header open), then `sync_page_parts` and `after_edit`. A picker starts on
  the caret section's current choice. Mirrored margins and Hyphenation write
  the package only (a settings part docxy cannot edit refuses) and have no
  undo in the terminal.

## Format plugins

- A format plugin is a `yppx_*` shared library (`.dll`/`.so`/`.dylib`)
  implementing `projcore::plugin_abi` (C ABI, MSPDI XML text across it) that
  adds project file types. `projcore::formats::Registry` holds the built-in
  `.yppx` and `.xml`, then the host's `mppread::project::MppImport` (read-only
  `.mpp`), then the plugins; a later registration replaces an extension's
  reader and/or writer in place. The loader is `projcore::formats::plugin`,
  behind the `plugins` feature (it adds `libloading`), which only native hosts
  enable; projcore's default and wasm builds stay dependency-free.
- `yppxy` loads them once at start from `<config>/yppxy/plugins`, then
  `$YPPX_PLUGIN_DIR` (`yppxy/src/formats.rs`). The suite loads them into one
  process-wide registry (`suite/docxy/src/project_formats.rs`, `global()`,
  first asked in `main`) from `<config root>/docxy/plugins` (`DOCXY_CONFIG_DIR`
  when set), then `$YPPX_PLUGIN_DIR`. That registry decides `is_project_path`,
  the Open dialog's lists (`open_extensions()`), every read and write, the
  Save As dialog's types (`save_types(path)`, the proposed type first, then
  registry order), the harness's `save-as` and `save-types`, and control
  `proj.save`. Unit tests load none; `project_formats::tests::Override` swaps
  in a stand-in per thread.
- A format plugin may be the default save type: `"default": true` on a
  formats JSON entry (`FormatDecl::default`, `ProjectFormat::is_default`;
  ABI still 1). Save As proposes `Registry::default_save_extension(opened)`:
  the plan's own type when something writes it, else the last registered
  default that still writes, else `.yppx`. Both hosts put it first
  (`save_types_default_first`) and in the suggested name (`Project1.<ext>`,
  `plan.<ext>`); Save in place and an extensionless `save_target` are
  unchanged. The harness's `save-types` reports `default`, `proj.path`
  `default_save_ext`; the fixture declares `.fakeproj` the default.
- Imported-status rule: a Project type nothing writes is an import (status
  "…, imported from .<ext>; Save As .yppx or MSPDI to keep edits", Save goes
  to Save As, `proj.path`/`tab-list` `imported: true`). With a plugin that
  writes `.mpp`, an `.mpp` tab saves in place. The opened file (re-read for a
  sidecar restore, refreshed by `proj.reload`) is `ProjectView::source`, passed
  to a plugin's save of the same extension only.
- A plugin that fails to load is skipped: stderr and the crash log at start,
  the first window's status line, never a dialog. The public build ships no
  plugin; without one every list, status and refusal is the built-ins'.
- The fixture is `projcore/tests/plugin_fixture` (lib `yppx_fixture`, a fake
  `.fakeproj` type). `uiharness/cases/project-plugin.uit` starts with
  `plugin yppx_fixture`: the runner copies it from beside uiharness into the
  sandbox's `docxy/plugins` before launch (build with `cargo build --release
  -p uiharness -p plugin_fixture`; `ui-sweep-linux` does). Harness instances
  never inherit `$YPPX_PLUGIN_DIR`.

## Sheet Home ribbon Font / Font Size boxes (#1344)

- The two boxes are text fields: `Docxy::font_box` (`FontBoxEdit`, tied to the
  tab and sheet like the Name Box) opened by a click on `sheet_combo`, keyed by
  `font_box_key`; Enter applies the name (`Font` undo step) or a size of 1 to
  409 (`Font Size`; a bad one keeps the box and says so), Escape or a press
  elsewhere restores, and a protected sheet refuses Enter (Protected View and a final document hide
  the ribbon body, so the box cannot open there). `sheet_field_live`
  (Name Box or font box) is what the key gates ask; the box is live only while
  the Home tab's body is drawn (a tab switch or Esc on its flyout closes it). Unfocused they show the
  active cell's font and size (`font_box.rs`). `uiharness/cases/sheet-font-box.uit`
  covers what `sheet-inputs-typing.uit` does not.

## Document Home ribbon Font / Font Size boxes (#1361)

- The document's boxes are the same `Docxy::font_box` (`FontBoxEdit` with
  `sheet: None`, tied to the tab only), opened by a click on `combo_box` or by
  `Act::FontName` / `FontSize` (the harness's `ribbon-click`; the combos have no KeyTips) through `font_box_open_doc`,
  which closes the Find and comment bars. `font_box_live` is surface-aware, so
  `sheet_field_live` and `entry_owns_keys` cover it. `on_key` hands it every
  key after the KeyTips and `document_key` (Ctrl+W, Ctrl+Tab, F12 still act),
  Ctrl+S saving and closing it. `dispatch_act` closes it for any other command;
  Find and a new comment close it too.
- Enter sets `set_font` (name cut to 31 characters) or `set_font_size`
  (`font_box::parse_doc_size`: 1 to 1638 pt, rounded to a half point; a bad one
  keeps the box and says so), on the selection as one undo step (a bare caret has no run to format and
  the editor keeps no pending font or size: the box closes and the status says
  to select text), and the document gets the keyboard back, as it does on
  Escape. Enter on a box nothing was typed in (`edited`
  false: the fill is the caret run's direct font or size, Calibri / 11 when it has
  none) is Escape too, so it never writes a style's size or a theme font as
  direct formatting; typing the value shown is an edit and applies. The old chip strips (`PickKind::FontName`
  / `FontSize`) are gone. `uiharness/cases/doc-font-box.uit` covers what
  `doc-inputs-typing.uit` does not.

## Document comment editor (#1362)

- The comment bar (`comment_key` / `comment_bar`) edits at a caret through
  `field_edit` like the Find bar: `comment_caret/sel/len` with `comment_state`
  and `comment_edit`, Ctrl+A/C/X/V in `comment_chord` (paste is one line),
  and `field-read comment-editor` reports the caret and the `[0,len]`
  selection. Click-to-place and Shift+arrows are not there.

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

## Project Task Information dialog (#1368)

- Task > Properties > Information..., Notes... (same dialog, on its Notes
  page), the bar and row menus' items, and a double-click on a task's ID cell
  open `suite/docxy/src/project/task_info.rs`: the catalogued `task-information`
  dialog (General, Predecessors, Resources, Advanced, Notes). The entry row and
  a blank row open nothing. `PromptKind::Constraint` and `Notes` are gone; the
  prompt bar keeps Predecessor, Move, Assign, Hyperlink and Find. A double-click
  on a Gantt bar does not open it (the bar has only a right-press handler).
- Each field is the grid cell's own text (`cell_edit_text` to prefill,
  `apply_cell` to write), so Predecessors and Resources are one text field each
  in the column's grammar, not tables. Notes is one line, `\n` = new line (no
  multiline control). Constraint type + date go through `Editor::set_constraint_typed`
  (the date resolved by `parse_stamp`); the
  date follows the type (ASAP and ALAP disable it).
- OK writes only the fields that changed (`Dialog::changed`), inside
  `Editor::batch`: one undo step, an unchanged OK leaves the plan clean, and a
  refused field applies nothing and keeps the dialog open with the message in
  the status line. Duration, Start and Finish of an auto summary, and
  % Complete of any summary, are disabled. A bare Deadline or constraint date
  is the day's last working time (FNLT, MFO, Deadline) or first (the other
  types), `YYYY-MM-DDThh:mm` takes a time, and the field shows a stored
  instant bare only when it is that boundary. What the field shows is what
  applies, also when only the type changed (`parse_stamp`): a bare date takes
  the new type's boundary, a time keeps its instant. Start and Finish together on an auto task
  are refused (each would replace the other's constraint). Task type and
  Effort driven apply before the rest, since a resource added in the same OK
  rescales by them. The new projcore setters (`set_percent_complete`,
  `set_priority`, `set_deadline`, `set_task_type`, `set_effort_driven`) are in
  `projcore/src/editor/props.rs`. The old `task-info` input of `inputs.rs` is
  gone; `uiharness/cases/project-task-info.uit` drives the dialog by hand.

## Terminal docxy prompts edit at a caret (#1372)

- Find, Replace, the comment bar, Compare's two paths and the vim `:` line are
  `docxy/src/line_edit.rs`'s `LineEdit` (text plus a char caret, shared key set
  in `handle`): Left, Right, Home, End, Delete and Backspace act at the caret,
  with no Ctrl or Alt (`LineEdit::is_edit_key`, so Ctrl chords keep their
  meaning), and typing inserts there (each prompt's Char arm keeps its own
  modifier rule). Only the
  focused field draws the `▏` mark, at its caret; `replace_drawn` feeds both
  the Replace status text and the `docxy-replace` `at`, and `compare_window`
  keeps a long Compare path's caret in view. `field-read` reports the real
  caret (`Field::with_caret`). `LineEdit` also holds a selection
  (`select_all`, `selection`, `delete_selection`; typing, Backspace and
  Delete replace or remove it, the arrows, Home and End drop it) and
  `field-read` reports it. In Find and Replace (#1375) Ctrl+A selects the
  focused field and Replace All is Alt+A (nothing in find-only mode). The
  comment bar and Compare's two paths take Ctrl+A too (#1376); the vim `:`
  line does not. Find, Replace and Compare copy and paste (#1377, below);
  the comment bar, the vim `:` line (#1378) and click-to-place (#1379)
  are still not there.

## Terminal editors: Ctrl+C/X/V in Find, Replace, Compare, the minibuffer and the cell editor (#1377)

- Ctrl+C copies the field's selection, Ctrl+X cuts it, Ctrl+V pastes the first
  line of the clipboard (control characters dropped) at the caret, over a
  selection. With no selection Ctrl+C and Ctrl+X do nothing, and none types a
  letter. An empty paste (the clipboard starts with a line break or holds only
  control characters) changes nothing in every editor, a selection included. The text comes from the OS clipboard, else from `clip_text` (the last
  text the editor put there: a headless Linux has no `os_clip`, which is the
  harness's path). docxy's `LineEdit` has `selected_text`, `cut`, `paste`;
  terminal docxy's `App::field_copy`/`field_paste_text` serve Find (a paste or cut in the
  query re-runs the match), Replace and Compare. xlsxy's `Prompt` has
  `selected_text`/`paste`, `App::field_copy`/`field_paste_line`; without an OS
  clipboard a grid paste after a field copy falls back to `clip_text`.
- A field copy replaces the other clip with the field's plain text: docxy sets
  `clipboard` to `Clip::from_text`; xlsxy clears `clip`, so a grid paste of that
  text is external TSV and not the old cells (`paste_from` treats an OS text
  equal to `clip_text` as its own clip).
- The xlsxy cell editor has only a select-all selection, so Ctrl+C/X take the
  text only while it is all selected (Ctrl+C keeps the selection); Ctrl+V
  inserts at the caret, replacing the text under select-all; a paste is not
  typing (`typed_tail` false, so no AutoCorrect at commit, and no AutoComplete
  proposal), and an empty paste changes nothing.

## Terminal editors: Ctrl+A in the other prompts (#1376)

- Ctrl+A selects the whole field, the caret at its end, and typing, Backspace
  and Delete replace or remove it; Left/Right collapse it to an edge and
  Home/End drop it; `field-read` reports `selection`. An empty field selects
  nothing, and the key never types an `a`. Each editor keeps its own small
  anchor (no shared line-edit crate): docxy's comment bar and Compare use
  `LineEdit`; xlsxy's minibuffer `Prompt` and yppxy's `Prompt` have
  `anchor`/`selection`/`delete_selection`; the cell editor reuses
  `EditState.select_all` (Ctrl+A sets it, Left collapses to 0; the
  end-of-branch reset keeps it for that key, and Ctrl+A skips the tail that
  ends a correction and refreshes the formula list). Left/Right collapse
  only when editing in place (F2); in type-over mode they still commit and
  move, as in Excel. Backstage Save As's name, shared by the
  three, has `Backstage::name_anchor`, drawn reversed and reported only while
  the name is focused (Tab away hides it, Tab back shows it again); every
  write of `name_cursor` clears it. yppxy's vim `:` line is a `Prompt`, so it
  takes Ctrl+A. The xlsxy vim `:` line has no select-all.

## xlsxy terminal vim `:` line edits at a caret (#1373)

- `VimState::cmdline` is `xlsxy/src/main.rs`'s `CmdLine` (text plus a char
  caret): Left, Right, Home, End, Delete and Backspace act at the caret, with
  no Ctrl or Alt (`vim_key` passes `ctrl || alt`), and typing inserts there
  (any modifier, as before). The hint line draws a reversed block at the
  caret, as the minibuffer prompt does, and `field-read xlsxy-vim-command`
  reports the real caret. Copy/paste and click-to-place are not there;
  nor is select-all (#1376 covers the minibuffer prompt, not this line).

## yppxy terminal prompt edits at a caret (#1374)

- `Prompt` in `yppxy/src/main.rs` (Rename, Duration, Predecessor, Constraint,
  Assign, Find, Notes, Save As on a new project and the vim `:` line) is text
  plus a char caret, built through `Prompt::new` (caret at the end of the
  seed). Left, Right, Home, End and Delete act at the caret with no Ctrl or Alt
  (such chords are still swallowed), Backspace and typing act there, and Enter
  commits the whole text wherever the caret is. The edits clamp the caret, as
  tests assign `buf` directly. The drawn `▏` sits at the caret and `field-read
  yppxy-prompt` reports it. Ctrl+A selects the buffer (#1376; typing and the
  edit keys replace or collapse it). Copy/paste and click-to-place are not
  there (#1378); Backstage Save As's name field is separate.
