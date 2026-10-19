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

## PDF export fonts (#1508)

- `PdfOptions::fonts` (a `docxcore::pdf_font::PdfFontProvider`) and `bidi` (the
  `BidiProjector` trait again) switch `to_pdf` from the Courier/WinAnsi file to
  embedded fonts; `docxcore` stays dependency-free: it only subsets the sfnt
  bytes a `PdfFace` hands it (`pdf_font::subset_sfnt`, retain-GID, `glyf`
  only), writes the Type0/CIDFontType2/`/W`/ToUnicode objects
  (`export/cid.rs`) and Flate-compresses with its own `deflate`. Parsing fonts
  and finding them is `pdffont` (ttf-parser, system directories, Office
  aliases, script fallbacks). A face is indexed under both its legacy (ID 1)
  and typographic (ID 16) family; Macintosh Roman names follow the Windows
  ones, so an English name kept only on the Mac platform still matches. Italic
  is macStyle, OS/2 fsSelection, `post` angle or the style name. A request
  takes, in order: the slant, the width nearest normal, the weight nearest
  400 (700 when bold), the face whose legacy name is the request, then the
  first path. CFF faces, faces the subsetter cannot read and faces whose OS/2
  `fsType` forbids embedding or subsetting are not offered.
- Layout measures through `export::Metrics`: Courier is 0.6 em per cell
  (unchanged, pinned by `docxcore/tests/pdf_legacy_bytes.rs`), a provider gives
  each cell its face's advance. Per character: the run's family face, a
  fallback face, Courier when WinAnsi covers it, else `.notdef` of the run's
  face, or a Courier `?` when the run has no usable face (counted in
  `to_pdf_report`'s `PdfReport::missing_chars`; terminal `docxy` says so on
  the status line). `cid::Checked` wraps the provider for one export: a face
  whose font file does not subset counts as absent for layout, writer and
  report alike. If no embedded face showed anything, the file is the plain
  Courier one.
- Bidi: a line wraps in logical order, then `place_line_fonts` puts its cells
  in the projector's visual order. A right-to-left stretch is one show-text
  with `TJ` adjustments in logical order, so extraction reads logically and
  the glyphs sit visually. Known limits: Arabic is not joined (isolated
  forms), brackets are not mirrored in right-to-left text.
- Terminal `docxy` embeds the machine's fonts (`$DOCXY_PDF_FONT_DIR`, a path
  list, replaces them, indexed once per value; empty means none, so the plain
  Courier file). The suite has no PDF
  export yet and `docxwasm` passes `None`. Unit tests set
  `TEST_PDF_FONTS`; the machine's fonts never reach them.
- Tests build their TrueType in code (`docxcore/tests/common/synth_font.rs`)
  and read PDFs back with `common/pdf_read.rs` (xref/length/reference
  validation and ToUnicode extraction), so nothing depends on `qpdf` (used too
  when installed) or on fonts being installed.

## Build info

- `buildinfo` is the one place that stamps binaries (commit, last merged PR, kind,
  manual build). Keep it dependency-free; hosts parse its `json()` text into their own
  JSON type and pass their own `CARGO_PKG_VERSION` (the suite, xlsxy, yppxy and lookxy are 0.1.0, docxy 0.5.0).
- `ci.yml` sets `DOCXY_BUILD_KIND: ci` per job and `release.yml` sets `release`, but
  never on `ui-sweep-linux` or `ui-sweep-windows`: `uiharness/cases/build-info.uit`
  asserts `local`.

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

## Header & Footer leftovers (#1433)

- `docxcore::ptab` reads a `w:ptab` (alignment, relativeTo, leader) and holds
  the stop and gap maths both hosts share, in the unit each counts: cells in
  `render.rs`, px in the suite (`ptab_stop`/`ptab_gap` wrap it with the 4 px
  right inset). The terminal fills the gap with the leader glyph (cells carry
  no editor offset). The suite lays a ptab out wherever the line width is
  known: `RenderCtx::hf_width` (header/footer) or `line_width` (a Print
  Layout column or a table cell, which wins in a header table cell); the flow view has none, so a body ptab
  there stays a zero-width object. `ptab_gap_el` draws the leader.
- A delete over a floating drawing or `w:ptab` (zero-width: no offset reaches
  it) records each as its own `w:del` while tracking
  (`Editor::drop_or_record_objects`, from `spanned_object_indices`). A cut
  over 3+ paragraphs keeps the zero-width objects of the paragraphs it
  removes whole, re-inserted at the seam: the clip cannot carry them.
- `sdt::BoundProp` (was `CoreProp`) adds Company and Manager, bound to
  `docProps/app.xml` under the extended-properties store item
  (`{55AF091B-…}`; unmeasured against Word). `DocTab::sync_bound_property`
  (in `set_dirty`) writes only an edit of a control's text
  (`Editor::take_bound_edits`: each control, paired by its `w:id`, whose text
  differs from the host's last look, one per property; formatting, inserting,
  removing or reordering controls and package-only changes such as Inspect
  Document's Remove All are not), from the open header/footer and the body,
  to the part (`BoundProp::write`). The editor the edit came from shows it in
  its other controls with `Editor::set_bound_property` as part of the edit's
  own undo step; the other editor takes it as an undo step of its own (so
  its Ctrl+Z, like the source's, undoes the property everywhere through the
  next sync). That step is one per step of the source editor, whatever properties it
  changed (`PropSource`: reused in place while it is the newest, no redo
  waits and the source's newest step is the one it answered; undo and redo forget it), and it
  advances the serial counter like any step, so the host refreshes the source's
  Repeat record after the sync (`keep_current`). Controls
  in a header or footer that is not open, and a package without the part,
  are not reached; terminal `docxy` has no package sync.
- Insert > Text has Date & Time... and Quick Parts (the H&F tab's commands,
  acting on the body when no header or footer is open; KeyTips D and K, as Q
  is Field's). Alt+Shift+D / T insert a DATE / TIME field
  (`doc_fields::doc_insert_key`).

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
  and `row_style`/`cell_style`/`category_style` draw the formatting's text
  styles (`Column Titles` on the headings). The harness reports contributed
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
  first as one row, consecutive same-`Control::group` controls as a titled
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
  Tab order). A plugin dialog may declare its own buttons (`Button::id`; a
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

## Project table: resizable columns and AutoFilter (#1533)

- Column widths are the plan's: `projcore::tableview::TableView` (px by field
  name, 24 to 600), `Project::table_view`, one undo step through
  `Editor::edit_table_view`, saved in the `.yppx` part `tableview.xml` (own
  content type, omitted when empty, never in MSPDI). It is not the issue's
  "views part": `views.xml` is an opaque retained part we do not own. A
  `tableview.xml` this build cannot read keeps default widths and stays a
  retained extra until a width replaces it. The suite's `ProjectView::widths()`
  is the drag's live width, else the plan's, else `DEFAULT_WIDTHS`; every
  width user (headings, cells, rules, scroll range, the default pane width,
  Best Fit) reads it, `TABLE_W` is only the default sum.
- A heading's right 6 px (`project-heading-edge:<Name>`) drag-resizes (window
  mouse move/up in `main.rs`, `WidthDrag`; one undo step on release) and
  double-click is Best Fit (`columns::best_fit`); the 16 px arrow left of it
  (`project-heading-arrow:<Name>`) opens the AutoFilter. The edge is drawn
  last so it wins; neither press selects the column.
- Sort and filters are view state on `Editor` (like `collapsed`; not saved, not
  undone, cleared by `replace_project`). `visible_rows()` stays in storage
  order (collapse only) and every edit acts on task indexes; what a grid draws
  and a cursor steps through is `Editor::display_rows()`: filtered (a summary
  stays while it or a task below passes; blank rows and UID 0 always pass),
  then sorted among siblings with each subtree kept together (text
  case-insensitive, numbers, dates by value, blanks last; UID 0 stays first).
  Insert above/below, indent and outdent are refused while sorted ("Clear the
  sort first"); filtered, they act normally. Paste, fill and range selection
  walk the displayed rows.
  One invariant keeps the cursor honest: after any operation the selection is
  a displayed task. `snapshot()`/`edit_structure()` capture the drawn rows
  (uids) before an edit (`Editor::pre_view`); `sel_into_display`, run by
  `changed()` and `toggle_level`, then takes the row drawn after the selection
  there that is still drawn, else the one before, by uid, so an edit that
  re-sorts the task cannot move the cursor to its new neighbours. With no
  capture (a filter just set, undo/redo) it takes the next displayed row in the
  sorted, unfiltered order. Delete uses the same rule (`reselect_near`), so a
  summary shown only through the deleted task drops out cleanly.
  `select_shown` is the post-edit select. The suite's cursor is the entry row
  when nothing is displayed or an edit's new task is hidden
  (`ProjectView::place_cursor`, `complete_project`); after a commit that hides
  the edited row, Enter/Tab keep the cursor where it landed, or go to the entry
  row when no drawn task took the row's place (`project_cell_input` compares
  the landing row with the pre-commit list). Find and Delete walk the drawn
  rows; yppxy's actions go through `shown_selected_uid`.
- The suite's drop-down is a dialog like the sheet's filter button
  (`project-filter-menu`, `DialogOwner::ProjectFilter`, `project/autofilter.rs`),
  not a `MenuTarget`, drawn as Project's popup (#1607, `Dialog::popup`): no
  title and an undimmed backdrop (it blocks the window like any modal), 250 px wide under the heading's arrow (`place`: flipped
  above when the room below cannot hold the menu and a usable list, clamped into
  the window both ways so OK / Cancel stay on screen; anchored on the frame on
  screen's `project-heading:<col>` probe, its left edge clamped into the table pane), `Apply` buttons as
  menu rows (icon, `▸`, rule above) in Project's order (Sort asc/desc, Group by
  ▸, No Group, Clear Filter, Filters ▸), the value list with a scrollbar, then
  OK / Cancel; a press outside presses Cancel and Escape closes it.
  `menu-read` and `menu-open` answer alike from the dialog (`items` with
  `{"separator":true}` entries, `controls`, `buttons` = OK / Cancel).
  Sort items (words by column type) apply and close, Group
  by, No Group and Filters are drawn disabled (follow-ups), `Clear Filter from <Field>`
  is enabled only while filtered, the checklist is the dialog check list (a
  Year > Month > Day tree on dates, two-state boxes), OK applies (all checked
  is no filter; none is refused), Cancel discards. `menu-open {"target":
  {"filter":"Finish"}}` opens it; `rows` reports `filter`, `filters`, `sort`,
  `sort_descending` and `widths`, the Project state reports `widths` and
  `filtered`; the status bar adds `AutoFilter Applied` while filtered.
- yppxy (terminal) has View > Data > Sort... / Filter... prompts, `:sort` and
  `:filter`, and the control verbs `proj.view`, `proj.sort`, `proj.filter`
  (`Editor::filter_texts` resolves a value, `(Blanks)` or `YYYY[-MM[-DD]]`);
  no mouse resize.
- `uiharness/cases/project-columns.uit` and `project-autofilter.uit` drive it;
  the sweeps run them (macOS cannot drive the window, so they were not run
  there).

## Project summary row (UID 0) is not a grid row (#1511)

- `Editor::visible_rows` omits `Task::is_project_summary` (UID 0, summary)
  unless the view flag `show_summary_task` is on (default off, no UI yet; it is
  view state like `collapsed`, bumps `revision`). Hosts and the harness's
  `rows` go through `visible_rows`, so the first task with UID != 0 is row 1,
  as in a plan without UID 0. The row stays in the model, in saves and in
  `task.list`; `rows` `total` still counts it.
- While hidden, UID 0 cannot be collapsed (`set_collapsed` refuses), and
  hiding it again expands it, so it never hides its tasks. A new session, and
  an edit that leaves the selection on it, start on the first shown row; an
  explicit `select` of it (automation) stays. `uiharness/cases/project-summary-row.uit`
  asserts the same layout with and without it (not run on macOS).

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
  in the column's grammar, not tables (until #1536, below). Notes is one line, `\n` = new line (no
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

## Task Information: top row, grids and date combos (#1536)

- Name, Duration and Estimated have `page: None`: one control each, drawn on
  every page. Estimated is the duration's `?`, not a value of its own: ticking
  it adds the mark to the Duration text, clearing it removes it, typing the
  mark ticks the box (`follow_estimated`, `react`), so only `duration` is
  written (a blank duration refuses the tick). General adds Schedule Mode
  (`set_manual`, applied first), Inactive (`set_active`, which refuses under
  an inactive summary), Display on Timeline, Hide Bar and Rollup
  (`Editor::set_display_on_timeline`/`set_hide_bar`/`set_rollup`, one undo step
  each, off when absent) and a Help button (`id == "help"`, drawn bottom
  left) that says what F1 says (`help_tab::DOCS_COMING`, #1022) and writes
  nothing. Display on Timeline, Hide Bar and Rollup are stored; nothing draws
  or reads them yet (the Timeline pane does not consult Display on Timeline). MSPDI has no Display on Timeline element, so it is saved only in
  the `.yppx` package's `timeline.xml` part (`projcore::timeline`, written only
  when a task states a value, an unreadable one kept as a retained extra like
  `tableview.xml`).
- `Control::editable_grid` makes a built-in grid interactive as a UI plugin's
  is (#1602): rows select, cells are typed into, Enter commits. It keeps
  exactly one blank row last (`keep_blank_row`, so an untouched grid equals
  the opened one and `Dialog::changed` is false), and Delete removes the
  selected row (a plugin grid's clears the cell). Column kinds: `Combo`
  (`Control::column_items`: typing narrows the list drawn under the grid, the
  arrows reach into it (`pick`), Enter takes the pick, a click on an item
  commits it, a text naming one item exactly (case aside) becomes it, any
  other text stays as typed: a new resource named Ann beside a Joanne is new;
  only a pick completes it), `Dropdown` (Space or a click steps the
  cell; typed text must name one item, or be contained in only one), and `Label` (read-only: `dialog-set`
  refuses it and typing starts nothing). All three apply to an editable grid
  only; a plugin grid's columns store what is typed. A cell being typed into
  is committed by Up/Down, a click elsewhere, Tab, a tab change and OK
  (`commit_pending`; a refusal keeps the edit and the dialog open). A pick
  carries its item's index (`Control::picked`), so of two tasks with one name
  the one picked is linked.
- `ControlKind::DateCombo` is typed as `Date`; its ▾ button, Alt+Down or F4
  opens `dialog::calendar::Calendar` (month grid, Sunday first) under the
  field: arrows move the cursor a day or a week, Page Up/Down a month, Enter
  or a click on a day writes `YYYY-MM-DD` into the field, Escape or Tab closes
  it (Enter and Escape are the calendar's while it is open,
  `DialogStack::key_button`). `dialog-read` reports `calendar_open` and
  `calendar` (`control`, `month`, `day`). `pointer-click` takes
  `dialog-calendar` (`button:<field>`, `prev:<field>`, `next:<field>`,
  `day:<field>:<n>`) and `dialog-cell` (`name[row].column heading in lower
  case`, or `name[suggest].<n>` for a narrowed combo list's item). A control
  with `beside` shares the row of the one before it (Duration/Estimated,
  Start/Finish).
- The Predecessors grid (`project/task_info/preds.rs`) has ID, Task Name,
  Type and Lag. A name fills the ID and an ID the name (`follow`, from the
  ID column's `column_items`, which line up with the names); OK reads the rows
  with the cell's own grammar (`parse_task_predecessors` for the ID, so a
  cross-project reference and `?uid` work; `parse_lag` for the lag) and keeps
  a link whose task, type and lag are as stored exactly as stored (lag format,
  cross-project fields) when its lag is spelled as shown or parses to the
  stored format (`16h` for `2d`, or `2d?`, is an edit). `set_predecessors` refuses a cycle, a duplicate or a
  self link, and the dialog stays open. The Resources grid
  (`project/task_info/resources.rs`) has Resource Name, Assignment Owner
  (`assn_owner`) and Cost read-only, Units typed; OK turns the rows into the
  Resource Names cell's tokens (`Ann`, `Bo[50%]`, `Cement[5 tons]`; 100% is the
  bare name) and applies them through `set_resources`, so a row left as shown
  changes nothing and a new name makes a resource. Rows without a name are
  ignored. The old text fields (`predecessors`, `resources`) are gone from
  the catalogue; the grids are not catalogued fields (`Grid` is not an editable
  kind). The `project-task-info.uit` cases were written without being run on
  macOS, like the others.

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
  line does not. Find, Replace and Compare copy and paste (#1377, below), and
  so does the comment bar (#1378, below); the vim `:` line and
  click-to-place are still not there. A click on the Find bar's Find or
  Replace box focuses it (#1379): `replace_hit` records the status row and the
  cells of the two boxes as drawn (the label, the ` → ` and the cells after
  the Replace box focus nothing); the click only switches `editing_replacement`
  and does not place the caret, and a drag or release on that row is consumed.

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

## Terminal editors: a Ctrl chord types nothing; copy and paste in the comment bar, Save As name and yppxy's prompt (#1378)

- Ctrl+C/X/V (no Alt) copy, cut and paste as in #1377 in docxy's comment
  bar (`comment_field_clip`), Backstage Save As's file name in all three
  editors, and yppxy's `Prompt` (every prompt, the vim `:` line included). Any
  other Ctrl chord without Alt types nothing there; Ctrl+Alt (AltGr) still
  types. docxy's and xlsxy's vim `:` lines are unchanged.
- `backstagecore` stays clipboard-free: `Backstage::name_has_keyboard`,
  `name_selected_text`, `name_cut`, `name_paste` and `backstagecore::first_line`
  (the one first-line, controls-dropped rule) serve the host, whose
  `backstage_name_clip` runs before `backstage_key` takes the backstage and
  passes every other key on. docxy and yppxy use `field_copy` /
  `field_paste_text`; xlsxy `field_copy` / `field_paste_line`.
- yppxy now has a clipboard (`arboard`, `os_clip`, else `clip_text`), and Ctrl+C
  on the grid copies the selected task's name as plain text. There is no task
  paste.

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
  edit keys replace or collapse it). Copy/paste is there (#1378, below);
  click-to-place is not; Backstage Save As's name field is separate.

## Project selection look (#1534)

- `suite/docxy/src/project/selection.rs`: one pure `Look` per frame
  (`ProjectView::look`, from `selection()` or the cursor) decides the Entry
  table's selection drawing, as Project 2024 does. A selected cell is filled
  itself (a wash of the accent, `accent::wash`, 0.30) with the cursor's 1px
  outline; the ID cell of every selected row and the heading of every selected
  column carry a lighter wash (0.16). A row selection (ID press, Shift+Space)
  is a range over all eight columns, so it fills the row; the table no longer
  highlights the whole cursor row (the chart side still does, and the entry
  row keeps its row background).
- A heading's left press selects the column (`project_column_select`, as the
  right press); the arrow and edge stop their own presses. Ctrl+A (whole
  table, cursor on the first cell, anchor on the last), Ctrl+Space (column)
  and Shift+Space (row, `row_header`) are in `project_input` before the Ctrl
  navigation; Ctrl+Home/End go to the first/last shown task's first/last
  column. Ctrl+Space may be the OS's input-source switch on macOS.
- Defaults: `DEFAULT_WIDTHS` sum 847 and `DEFAULT_TABLE_SHARE` 0.75 make the
  whole Entry table visible at the default 1180 px window, every column
  holding its heading beside the arrow and its usual cell text (unit tests pin
  both, the second from per-character estimates). The table
  scrollbar was already always shown. The harness `state` reports
  `selected_columns`, `selected_rows`, `filled_cells` (`"<row>:<column>"`),
  `hot_headings`, `cursor_border` and `table_fits`;
  `uiharness/cases/project-selection.uit` (not run on macOS).

## Project timescale and Timeline pane (#1535)

- `GanttScale` carries `day_w` (px per day); `DEFAULT_DAY_W` (22, the Days
  level) is only its initial value, and no geometry reads it. The view's zoom is
  `ProjectView::zoom_day_w` (`None` = default): view state like `split`, never
  saved or undone. Every change goes through `ProjectView::set_zoom(day_w,
  anchor_day, anchor_px)`, which clamps to 0.25..96, refreshes the scale and
  re-derives `gantt_x` from the anchor date; the plugin overlay's cache key
  includes `day_w`. `gantt_w` never depends on the zoom.
- `project/zoom.rs` holds the pure maths: `LEVELS` (Years, Half Years, Quarters,
  Months, Thirds of Months, Weeks, Days) with their (major, minor) `Unit`s,
  `nearest`/`step`/`fit`, and `tier`/`tiers` (header cells, labels fitted to
  their width). The header is two 14 px tiers inside the old 28 px row. Hours
  and Minutes are listed in the Timescale list and cannot be chosen (bars are
  whole-day geometry); day lines draw from 8 px/day and non-working days are
  shaded as merged runs from 3 px/day. Zoom In/Out step one level around the
  left edge; Ctrl+wheel (Ctrl on every platform; the rows' scroll is put back)
  steps around the pointer; Entire Project and Selected Tasks fit a span to the
  chart. A free width shows as `Custom (<nearest level>)`.
- Dragging a Timeline view-box edge (`BoxEdge`, 6 px strips drawn last, region
  `project-timeline-box-left/right`) moves that boundary and sets the zoom so
  the box's span fills the chart (`drag_edge`; the other edge stays put, also at
  the zoom clamp, except that a Left drag on a chart showing days past the
  scale's end moves its real right edge to that end while the drawn edge stays put); the middle still pans. The grip under the pane (`project-timeline-grip`)
  sets `ProjectView::timeline_h` (`MIN_TIMELINE_H` = one band plus padding, 60;
  at most half the window, `ProjectView::height`); a double-click restores 84.
  Like `split` it lives for the window session and is not written to disk. The
  Timeline's "Add tasks with dates to the timeline" placeholder now shows only
  for a plan with no tasks: it used to sit clipped below the bar row, which a
  taller pane would have exposed over a plan that has tasks.
- Ctrl+wheel: every delta sums in `wheel_acc` and steps one level per
  `WHEEL_STEP` (40 px); only a whole-line delta (|lines| >= 1) is a notch that
  steps at once, fractional lines (hi-res wheels, touchpads) sum, and touch
  phases Started/Ended clear the sum. The chart takes every Ctrl+wheel event
  over it (`WheelZoom::Absorbed`/`Stepped`) and puts the rows' scroll back, so a
  swipe never scrolls the rows. A view-box edge drag is measured against the edges as the box
  draws them (clamped to the Timeline's span) and moves within the span.
- The chart menu opens the Timescale and Zoom dialogs (`project/zoom_dialogs.rs`,
  `DialogOwner::Zoom`/`Timescale`, catalogue ids `zoom`, `timescale`). The
  Timescale dialog's tier rows are read-only (Count is 1): the tier editor and
  the non-working-time page are not built.

## Project row menu and mini toolbar (#1537)

- `project_row_menu` lists Project 2024's task row items in its order, Notes...
  kept: Cut, Copy, Paste, Paste Special..., Scroll to Task, Insert/Delete/
  Inactivate Task, Manually/Auto Schedule, Assign Resources..., Text Styles...,
  Information..., Notes..., Add to Timeline, Link. Fill Down, Clear Contents
  and Font... are not row items (Ctrl+D stays; `ProjectAct::ClearContents` was
  removed, nothing else reached it); `Link` is `ProjectAct::Hyperlink` (the bar menu keeps
  `Hyperlink...`). Paste Special..., Text Styles... and Add to Timeline are
  disabled placeholders until #1389/#1390/#1391.
- `Menu::toolbar` (`menu::ToolbarItem`: an `Entry` plus `ToolbarKind`
  combo/button/arrow and a combo's `value`) is the mini formatting toolbar
  drawn above the row and column-heading menus (`project_toolbar`, opened by
  `open_menu_with_toolbar`); every other menu has none. `menu-read`/`menu-open`
  report `toolbar: [{id,label,kind,enabled,checked,value?,submenu?}]` (`[]`
  when absent). `menu-click {"label"}` or `{"path":["Font Color","Red"]}`
  finds a toolbar label first; a drop-down's choices replace the items
  (`Menu::enter_toolbar`, depth 1, the toolbar hidden) and Esc/Left backs out.
- Formatting items are `ProjectAct::CellStyle(StyleEdit)`: one
  `Editor::edit_formatting` over the selected cells (the range, else the
  cursor cell, real tasks only; the range survives), merged into each cell's
  direct `TextStyle`; a choice that changes no cell records no step.
  Bold/Italic set every cell to the opposite of the cursor cell's effective
  value (row categories under the cell's own, a summary bold), which is also
  what the toolbar's ticks and combo values show. The lists are fixed (8 fonts, sizes 8 to 72, 8 colours): the
  suite has no font enumeration. The task buttons dispatch AddLink,
  UnlinkTasks, Outdent, Indent and Task Details (`Constraint`).

## Suite inline pictures (#1432)

- The suite's `paragraph_el` Raw arm draws an inline `w:drawing` picture as
  `gpui::img` (`ObjectFit::Fill`) inside the same one-offset box #768 drew as a
  frame; box size, offsets, caret and `block_height_est` are unchanged. The
  border shows only when no pixels do: the rid does not resolve, or the bytes
  are not PNG/JPEG/GIF/BMP by `docxcore::image::sniff` (WMF/EMF/SVG/TIFF keep
  the frame). Pixels that sniff but fail to decode in gpui draw nothing.
- `docxcore::image::inline_embed` is the rid (same inline-only guards as
  `inline_extent`, and only when `a:graphicData` is a picture: a group or shape
  holding blips is not one). `suite/docxy/src/pictures.rs` resolves it through
  the story part's own relationships (`Package::part_rel_target`: relative to
  the part's own folder, `..`, absolute; external or escaping targets none): the same rid names
  different media in `document.xml.rels` and a header's, so `RenderCtx::pics` /
  `pictures::Scope` carry the story's part name (the body's, the open
  header/footer's `HfEdit::part_name`, or the page's header/footer part).
- `DocTab::pictures` keeps the `Arc<gpui::Image>`s per (part, rid) across
  frames, so only a miss reads, copies and hashes media. It is valid for one
  `Package::parts_stamp` (a fresh value from a process-wide counter on every
  mutable access to the parts, equal in an unchanged clone): any package change
  empties it, so a header edit re-reads that tab's pictures once. Decoded
  images stay in gpui's asset cache for the process (no `remove_asset`).
  A run whose rid is missing from its story's relationships (pasted from
  another story keeps its source rid) keeps its frame.
- Out of scope: anchored pictures, `a:srcRect` cropping, rotation, alt text,
  the flow view's non-inline cases, PDF export, wasm hosts. The harness reports
  no pictures, so there is no `.uit` case.

## Run styles (#1427)

- The suite draws a run through `run_look::RunStyler` (sheet, the cell's table
  `RunLook`, the paragraph's style), i.e. `StyleSheet::shown_run`: table style <
  paragraph style (basedOn) < character style (basedOn) < direct, for bold,
  italic, underline, strike, colour, size and font family. It starts from
  nothing: `docDefaults` and the default paragraph style (Normal) are NOT
  applied, except to a paragraph whose `pStyle` names Normal; the paragraph
  chain stops before Normal when reached through `basedOn` (a visible change to every document, left for its own issue). A
  direct explicit-off (`<w:b w:val="0"/>`, in `raw_props`) beats every style;
  the character style beats the paragraph style outright (Word's toggle XOR
  between them is not modelled). `emit_words` and `Measurer::width_in` use the
  run's font family and `run_look::run_size_px` (a style's or direct `w:sz`,
  scaled by zoom), so tab positions match what is drawn; an unavailable
  family falls back in gpui (`resolve_font`).
- `DocTab::run_styles` (`run_look::Cache`) parses `word/styles.xml` once per
  change of the part (a moved `Package::parts_stamp` with other part text does
  not reparse). `Docxy::render` hands the same text to the body and open
  header/footer editor with `Editor::set_styles_xml`.
- With a styles context, Bold/Italic read and write over the styles
  (`Editor::toggle_run_prop`, per run): off over a style that makes the text
  bold writes `<w:b w:val="0"/>` (also at a caret, via `PendingFormat`'s
  `over`: `Over::Off`), on again drops it and inherits. Without a context (terminal
  `docxy`, wasm hosts) toggles are unchanged. `Editor::caret_props_shown` is
  the ribbon on-state; the harness `run` reports `shown_bold`/`shown_italic`.
  Twins: turning on writes `w:b` with `w:bCs` when the run had the twin; off
  over a style keeps the twin's `w:val="0"` in the shadow only (the primary's
  also in `raw_props`, as the loader keeps it); inheriting drops both. The
  sheet has no complex-script model, so off then on under a style with `w:b`
  but no `w:bCs` drops the run's direct `w:bCs` (known gap).
  Out of scope: underline/strike explicit-off, caps/highlight/spacing from
  styles, table style size/font, `block_height_est` for styled sizes.
  `uiharness/cases/doc-run-styles.uit` was written without being run (macOS
  cannot drive the window).
