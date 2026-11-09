# CLAUDE.md

**Where notes go.** This file holds only what every session needs: how work is done, the
rules that span crates, and an index of area notes. A feature's notes go into its area file
under `docs/claude/` (or a new area file, added to the index below), never as a new section
here. Read an area's file before changing that area: those files are not loaded
automatically. This file stays under 40,000 characters, and CI checks it
(`python3 scripts/check-claude-md.py`): the budget, the index, and that this file's `##`
sections are exactly the `KEPT` list in that script.

## How work is done (every machine, every session)

- Read `docs/WORKFLOW.md` before starting an issue. In short: one issue → one `issue-<n>-*` branch → one
  PR merged by `merge-check`, through agworkbench.
- Before starting an issue, skip it if it has the `in-progress` label, a **Grabbed** comment, a PR or a
  remote branch. Otherwise claim it: add the label and comment `Grabbed by <machine>`.
- Claude only, never OpenAI models. Auto permission mode. Cross-machine comments start with `**[<machine>]**`.
- Stuck on another machine? Ask the coordinator, the Mac session `docxy-6c`: the user talks to the machines only
  through it.

## Area notes

Each file holds the notes of one area, moved here from this file in #1903. Grep for an
issue number to find its notes.

- `docs/claude/docx-editing.md`: before changing the document editor's lists, indents,
  hyphenation, sections, settings.xml or headers/footers: lists, numId 0 and indents
  (#1653, #1728, #1792, #1814, #1815), manual hyphenation (#1266), Page Setup / Columns
  Apply to Selected text (#1272), Layout settings.xml undo steps (#1273), Header & Footer
  leftovers: `w:ptab`, bound Company/Manager properties, Date & Time (#1433), live page
  fields (#1437), Web Layout and DocView fitting tables to the view width, horizontal
  scroll (#1895), Print Layout's y-only caret follow (#2080), the flow view drawing an
  imported mail: exact spacing and line height, list numbers in the mark's look, the
  layout dump (#1986).
- `docs/claude/suite-document.md`: before changing the suite's document tab: save-as
  names (#807), Home Font / Font Size boxes (#1361), File > Info Properties (#1364), the
  comment editor (#1362), inline pictures (#1432), run styles (#1427), font substitution
  (#1900), link colours (#1906), tables placed by `w:jc` (#1922), rounded
  picture crops (#1926), the document views' scrollbars and Print Layout's
  sideways scroll (#2080).
- `docs/claude/pdf-export.md`: before changing PDF export: embedded fonts (#1508),
  borders, picture watermarks and unequal columns (#797).
- `docs/claude/html-import.md`: before changing HTML import: tables (#1878), CSS (#1872),
  images (#1880), links (#1879), centring (#1922), rounded images (#1926),
  nested cards (#1927), page margins (#1928), Outlook mail: CSS-only spacing,
  quirks-mode edges, list indents, item continuations, marker look (#1979).
- `docs/claude/email-html.md`: before changing `docxcore::export_email` (a `Document` as an
  e-mail body): inline styles only, lists and their `start`, safe links, `cid:` pictures and
  their media, the import round trip (#1948).
- `docs/claude/clipboard.md`: before changing copy and paste with other apps: rich (RTF)
  document copy (#1074), sheet HTML tables (#1248), the X11 clipboard and a document
  paste of HTML (#1950), a test X client's last request (#2008).
- `docs/claude/macos.md`: before changing macOS-only behaviour: the menu bar (#1071), ⌘/⌥
  text navigation (#1073).
- `docs/claude/suite-shell.md`: before changing the suite's window chrome: the start
  screen (#806), the title label and accent per tab type, and where the theme button
  lives (#1473).
- `docs/claude/suite-extensions.md`: before changing the suite as a library
  (`run(ext::Extensions)`), extension surfaces (Mail tabs), tab tear-off or the embedded
  `ext::DocView` (#1873), its editing mode (#1947), host insert, copy and cut (#1977),
  page inset and page background (#2003), its scrollbars (#2080), an extension's ribbon
  drawn by the suite and the suite's theme (#1870), File backstage pages and CloseTab
  (#1923).
- `docs/claude/ribbon.md`: before changing the ribbon: contributed group launchers
  (#1669), ribbon fit per ribbon (#1020, #1608, #1685, #1763), swatch galleries, their
  sections, More button and KeyTips (#1761, #1762, #1823). A new ribbon command also needs
  a help target (`every_ribbon_command_has_a_help_target`: `docs/claude/help.md`), and
  a new command draw path its F1 probe, `ribbon-cmd:<id>` (#1869, same file).
- `docs/claude/help.md`: before changing the help manuals or the Documentation tab:
  manual bundles (#1022), the Help tab and F1 (#1855), the manual revision in
  About and the Help footer (#1857), F1 on a hovered ribbon command (#1869).
- `docs/claude/ui-plugins.md`: before changing the UI plugin ABI, the plan's formatting
  model (`projcore::formatting`, `formatting.xml`) or the plugin dialogs and their layout:
  UI plugins (#1532, #1553, #1572, #1609), dialog values that follow (#1670), overlay text
  style (#1668), the rich-text control (#1557), grid `cell` columns (#1558).
- `docs/claude/project-formats.md`: before changing Project file reading or writing:
  format plugins (#1702, #1801), MPX import (#309, #1805), `.mpp` custom fields (#292).
- `docs/claude/project-text-styles.md`: before changing Project's text styles or the
  task facts they style: strikethrough and pattern (#1664), categories the host computes
  (#1665), categories that need a model feature: Marked, Highlight, changed cells (#1667).
  The timescale tier styles (#1666) are in `docs/claude/project-gantt.md`.
- `docs/claude/project-gantt.md`: before changing Project's Gantt chart drawing or zoom:
  timescale tier text styles (#1666), gridlines (#1575, #1688, #1759), bar style rows
  (#1696), layout fields (#1695), palette manual and inactive bars (#1760), timescale,
  zoom, the Zoom and Timescale dialogs and the Timeline pane (#1535).
- `docs/claude/project-table.md`: before changing Project's Entry table, or the
  `Editor`'s sort, filters and displayed rows (yppxy's too): resizable columns and
  AutoFilter (#1533, #1607), the summary row (#1511), the prompt bar (#1370), the
  selection look (#1534), the row menu and mini toolbar (#1537).
- `docs/claude/project-task-info.md`: before changing Task Information or task notes:
  rich task notes (#1801, #1803), the dialog (#1368), its top row, grids and date combos
  (#1536, #1682).
- `docs/claude/project-scheduling.md`: before changing Project scheduling, or how MSPDI
  and `.mpp` carry scheduled dates: leveling as a plan edit (#1594), elapsed durations and
  Must Start On before opening (#484).
- `docs/claude/sheet-ui.md`: before changing the sheet's tab rename (#1365), Home Font /
  Font Size boxes (#1344) or Save As names (#804, #1807).
- `docs/claude/sheet-file.md`: before changing a sheet's notes, slicers, data validation
  or hyperlinks in `gridcore` (and the dialogs and Undo over them): notes that follow
  row/column insert and delete (#1335), table slicers on save and Undo of a table step
  (#1694, #1741), x14 rules in the Data Validation dialog (#1817, #1734), pivot slicer and
  timeline caches on save (#1797), `.xls` hyperlinks and file URIs (#1726).
- `docs/claude/terminal-editors.md`: before changing terminal `docxy`, `xlsxy` or
  `yppxy`, or `backstagecore`'s Save As name field: docxy's Layout tab (#1274), prompts
  editing at a caret (#1372, #1373, #1374, #1375, #1379), Ctrl+C/X/V (#1377, #1378),
  Ctrl+A (#1376), the F1 help reader (#1856).
- `docs/claude/mail.md`: before changing `mailcore` or Mail: drafts keep their `local:`
  id (#1884, #1881), the body format (#1901), sync one folder, the fast cycle and Sent
  Items after a send (#1885), folders and counts on every tick, deleted folders (#1886),
  a failed send back to Drafts (#1874), the signed-in user's profile, photo and Sign
  out (#1920), granted scopes and a refused sign-in's `error_description` (#1875),
  `Content-Length` on bodiless POSTs and one-line HTTP error text (#1954), inline outbound
  attachments (#1949), attachment upload progress and resumed upload sessions (#1941),
  the whole mailbox newest first, backfilled in idle time (#1871),
  new mail with its body, recent bodies prefetched, FetchBody served between folders (#1978),
  Exchange's AuthAs header and the `msip_labels` sensitivity label stored
  with a fetched body (#1933, #2015),
  calendar sync keeps events' reminder settings (#1937), event fields a calendar client
  shows (#1983), creating and updating them, an update PATCHes only what changed (#1994),
  an event created in a chosen calendar (#2048),
  every calendar, `SetCalendars`, events in a range and days with events (#1984),
  a calendar range on demand, fetched once while fresh (#2014),
  a 30/180-day window kept by calendarView delta per calendar (#2013),
  occurrences filled from their series master, masters off the grid (#2064),
  the Contacts folder as records, `is_contact`, contacts created, updated and
  deleted through the outbox, a refused write reported and reverted (#1934, #2063),
  cancelling a meeting with a message and forwarding it (#1985),
  a series' occurrences, its master and editing either (#2022),
  a series created with any Graph recurrence (#2034),
  event times in a named time zone, zone-preserving series edits (#2039, #2030),
  respond options: no response, a proposal on tentative, attendees' proposals (#2023),
  meeting message types, the referred event and Remove from Calendar (#2024),
  dismiss and snooze reminders on the server (#2025),
  a moved message followed to its new id, its old ids still resolving (#1896),
  server message search and opening a hit not in the store (#2006),
  a message's list row by id, batched, without its body (#2053),
  a flagged message's reminder, read, stored, snoozed and dismissed (#2054),
  an event's own response time and created time (#2068),
  OAuth tokens in the platform secret store, a plaintext fallback and its
  warning (#1877).

## Build and test

What CI runs (`.github/workflows/ci.yml`):

- The root workspace (`Cargo.toml` members: opccore, docxcore, pdffont, gridcore,
  ctlcore, ribboncore, ribbonspec, backstagecore, docxy, xlsxy, projcore, projctl, yppxy,
  mppread, docxwasm, gridwasm, htmlbundle, mailcore, lookxy, editcore, comshimcore,
  xlcomshim, wordcomshim, uiharness, buildinfo, helpdocs, projcore/tests/plugin_fixture):
  `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy -p docxy --all-targets --features html-export -- -D warnings`,
  `cargo test --workspace --locked --features docxy/html-export`.
- `suite/` is its own workspace, which `--workspace` does not reach:
  `cargo fmt --all --check --manifest-path suite/Cargo.toml`,
  `cargo clippy --manifest-path suite/Cargo.toml --all-targets --locked -- -D warnings`,
  `cargo test --manifest-path suite/Cargo.toml --locked` (on Linux under `xvfb-run -a`).
- The UI sweep runs every `uiharness/cases/*.uit` against release builds
  (`cargo build --release --locked --manifest-path suite/Cargo.toml`, then
  `cargo build --release --locked -p uiharness -p docxy -p xlsxy -p yppxy -p plugin_fixture`):
  `python3 scripts/ui-linux-sweep.py`. The scripts' unit tests:
  `python3 -m unittest discover -s scripts -p 'test_*.py' -v`.
- `python3 scripts/check-claude-md.py`: this file's budget and the area-note index.

## Cross-crate invariants

Restated from the area notes, which hold the detail:

- No extension, product edition or private repository is named in this repository; the
  fixture is the only plugin it knows (`docs/claude/suite-extensions.md`,
  `docs/claude/ui-plugins.md`).
- `projcore`'s default and wasm builds stay dependency-free; plugin loading is behind its
  `plugins` feature, which only native hosts enable (`docs/claude/project-formats.md`).
- `docxcore` stays dependency-free in PDF export too: font parsing and lookup are
  `pdffont`'s (`docs/claude/pdf-export.md`).
- `helpdocs` is dependency-free, like `buildinfo` (`docs/claude/help.md`).
- `backstagecore` stays clipboard-free; the hosts own the clipboard
  (`docs/claude/terminal-editors.md`).

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
  Windows dead key never reaches `on_key` (`passes_to_on_key`): the next
  key-down carries the composed text whole in its `key_char` (Space with
  "´", a letter with "´x"), which a dialog field types in full.
  `edit_key` still swallows every ⌥ key, and Windows' plain Alt is never text.
- The Project cell editor and prompt take ⌥ and AltGr text the same way
  (#1312): `project_key` runs `text_input::dialog_stroke` while an edit is
  open (`project_stroke`; with none open the key is untouched), and the AltGr
  debt is the field's (`text_field_open`) when a dialog or a Project edit
  is open at the key-down.
  On Windows a plain dead key (`plain_dead_key`: `prefer_character_input`, no
  Ctrl/Alt/Win) never reaches `on_key`: it owes nothing and its accent comes
  composed with the next key, so no field types it twice.

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
- A pushed plain modal form opens with its first editable control focused
  (`Dialog::focus_on_open`, from `DialogStack::push` and a button's child
  dialog; message boxes, modeless, popup and plugin dialogs and a dialog built
  with a focus are left as they are), and `Dialog::focus_step` (open, Tab,
  Shift+Tab) selects a single-line field's whole value, so typing replaces it
  as in Word; a click places the caret (#1314). A dialog opened on a later
  page focuses that page's own first control (`focus_own_page`) ahead of the
  top-row ones that belong to every page, and a tab switch from a top-row
  control does the same (the first page keeps it); only Task Information's
  Notes (`Control::append_on_open`) opens with the caret at the end to append,
  the others select. A generated "focus by click" /
  "focus by Tab" case first moves the focus to a neighbour field
  (`cases.rs` `tab_start`: a typed, dropdown or list neighbour shown by the same
  prep, or the control an entry names with `Field::leave`), as the field may
  open focused.
- The generated cases type with `real-key`/`real-type` and focus by real click
  or Tab only. An input outside dialogs goes in `suite/docxy/src/inputs.rs` with
  the issue that covers it. See `qa/inputs-typing.md`.
