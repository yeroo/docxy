# CLAUDE.md

**Where notes go.** This file holds only what every session needs: how work is done, the
rules that span crates, and an index of area notes. A feature's notes go into its area file
under `docs/claude/` (or a new area file, added to the index below), never as a new section
here. Read an area's file before changing that area: those files are not loaded
automatically. This file stays under 40,000 characters, and CI checks it
(`python3 scripts/check-claude-md.py`): the budget, the index, and that this file's `##`
sections are exactly the `KEPT` list in that script.

## How work is done (every machine, every session)

- **Wind-down (owner, 2026-11-17): file no new follow-up or leftovers issues.** The project is
  finishing a short final list. Fix a minor finding in the same PR when it is small and in scope.
  Otherwise list it under "Not done" in the PR body and drop it. No `Leftovers from` issue, no
  `follow-up` issue, and no new issue for a split, unless the PR cannot merge without it. Issues
  labelled `parked` are out of scope: never pick them up or reopen them.
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
  (#1653, #1728, #1792, #1814, #1815), Bullets and Numbering never joining a
  document's own numId 1 or 2, on and off by kind (#2180), manual hyphenation
  (#1266), Page Setup / Columns Apply to Selected text (#1272), Layout settings.xml undo steps (#1273), Header & Footer
  leftovers: `w:ptab`, bound Company/Manager properties, Date & Time (#1433), live page
  fields (#1437), Web Layout and DocView fitting tables to the view width, horizontal
  scroll (#1895), Print Layout's y-only caret follow (#2080), the flow view drawing an
  imported mail: exact spacing and line height, list numbers in the mark's look, the
  layout dump (#1986), list markers in their level's `w:rPr` (#2002), nested
  tables widening their fitted cells and wide pictures scaled to the room (#1963), list level and style indents drawn, HTML
  import's pinned list indents (#2231), list markers from the numbering
  part, counted across a table's cells (#2087), fields inside tracked
  changes: loaded `w:del`, tracked insertion, Compare (#1625), plain text
  pasted with only the characters XML can hold, `\u{0B}` a line break (#1993),
  an alignment tab deleted out of a field marker's run (#1463), a
  tracked clear of table cells keeping equations, Reject All (#1471),
  the suite's markers from each level's `w:start` (#1989), an item's
  continuation keeping the count (#2151).
- `docs/claude/suite-document.md`: before changing the suite's document tab: save-as
  names (#807), Home Font / Font Size boxes (#1361), File > Info Properties (#1364), the
  comment editor and its draft per tab (#1362, #1329), inline pictures (#1432), run
  styles (#1427), font substitution
  (#1900), link colours (#1906), tables placed by `w:jc` (#1922), rounded
  picture crops (#1926), per corner (#1936), the document views' scrollbars and Print Layout's
  sideways scroll (#2080), Word's dark mode for the document and View >
  Switch Modes (#2079), the Find bar's keys, clicks and the one-line fields'
  shared key map (#2116), unnamed runs in the document's theme font and a
  family with a real bold face (#1972), dark-page highlights, paragraph
  borders and their grouping, Segoe UI's substitutes (#2231), comment
  balloons shown on open, resolved hidden, no inline glyph (#2232), cell
  list numbers, All caps and Small caps drawn, Table Styles tiles on a
  dark page, Switch Modes not persisted (#2087), a pasted picture in a
  link or a tracked change bringing its media (#1853), a placeholder
  picture's Alt Text drawn in its frame (#1898), `w:tblInd` drawn (#2151).
- `docs/claude/pdf-export.md`: before changing PDF export: embedded fonts (#1508),
  borders, picture watermarks and unequal columns (#797), tables placed by
  their `w:jc` (#1930).
- `docs/claude/pdf-view.md`: before changing the suite's PDF view (#2245): the
  pdfcraft-render dependency and the patch table a rev bump moves, the
  `--pdf-worker` child and its protocol, timeouts and watchdog, the tab's
  routing, session row and read-only rules, the harness's `pdf-read`, the
  corpus and its reference tolerance; links, comment pop-ups and AcroForm
  filling, the display copy, Save as an incremental update, replay in a
  new child, the signed-PDF question and quit (#2255).
- `docs/claude/html-import.md`: before changing HTML import: tables (#1878), CSS (#1872),
  images (#1880), links (#1879), centring (#1922), in cells (#1930),
  bookmarks, the Hyperlink style, relative links, titles and targets,
  Markdown link destinations (#1912), rounded images (#1926), corners
  that differ, rounded wrappers and the editable page's crop (#1936),
  nested cards (#1927), cards in the body, adjacent body tables (#1940),
  page margins (#1928), a page's own `@page` size and margins,
  Outlook's mail keeping the Outlook-like page (#1944), Outlook mail: CSS-only spacing,
  quirks-mode edges, list indents, item continuations, marker look (#1979),
  list and parent margins, trailing `<br>`, wide hangs, wrapper tables (#2114),
  a picture in a link keeping it as its click link (#1999), hidden blocks,
  `text-decoration:none` links and span-only columns as Word draws them (#2249),
  quirks tables' font, an explicit italic off, rtl, letter spacing, caps,
  visibility, white-space, page breaks, block borders as paragraph
  borders, `+`, attribute and `:first-child` selectors (#1883), table
  and cell borders, `frame`/`rules`, `border-collapse`, row heights,
  `col` widths, `max-width`, `th`, the page colour (#1894), image
  percentage sizes and `max-width`, `<base href>`, `srcset` and
  `<picture>`, MHTML, SVG and WebP through a converter (#1898),
  whitespace-only paragraphs keeping a look drawn on spaces, an empty
  line's size on its mark (#1973), each `<ol>` its own count, `start`
  and `value` (#1989), a block's box beside a child table, margins
  through empty blocks, tables in indented blocks, a padding-less card's
  quirky edges (#2151).
- `docs/claude/email-html.md`: before changing `docxcore::export_email` (a `Document` as an
  e-mail body): inline styles only, lists and their `start`, safe links, `cid:` pictures and
  their media, the import round trip (#1948).
- `docs/claude/clipboard.md`: before changing copy and paste with other apps: rich (RTF)
  document copy (#1074), sheet HTML tables (#1248), the X11 clipboard and a document
  paste of HTML (#1950), a test X client's last request (#2008), HTML and RTF on
  Wayland through the suite's patched gpui_linux (#1966), another app's lists
  kept by a document paste (#1967), in the document's own lists (#2087).
- `docs/claude/macos.md`: before changing macOS-only behaviour: the menu bar (#1071), ⌘/⌥
  text navigation (#1073).
- `docs/claude/suite-shell.md`: before changing the suite's window chrome: the start
  screen (#806), the title label and accent per tab type, and where the theme button
  lives (#1473), Open File's native picker off the App borrow on Windows
  (#2192), every other native dialog the same way, a save's `AfterSave` and
  the workbook save's steps (#2194), the real Open picker in the Windows
  sweep, a harness's Open refused off Windows (#2195).
- `docs/claude/suite-extensions.md`: before changing the suite as a library
  (`run(ext::Extensions)`), extension surfaces (Mail tabs), tab tear-off or the embedded
  `ext::DocView` (#1873), its editing mode (#1947), host insert, copy and cut (#1977),
  pictures from bytes and paste (#1953),
  host hyperlinks (#1952), link hover and the hand cursor (#1916),
  page inset and page background (#2003), picture Alt Text tooltips
  (#1998), linked pictures followed as links (#1999), an imported mail's Outlook-like
  inset (#1987), its scrollbars (#2080), its Find
  box (#1887), its Print Layout (#1888), an extension's ribbon
  drawn by the suite and the suite's theme (#1870), File backstage pages and CloseTab
  (#1923), File opened on an extension's own page (#2204), Bullets/Numbering
  on the document's lists (#2087), hidden text, run shading and
  plain-text links in an imported mail (#2249), further tab kinds
  besides Mail: their cards, accent, session rows (#2253), a tab refusing
  its close (#2260).
- `docs/claude/ribbon.md`: before changing the ribbon: contributed group launchers
  (#1669), ribbon fit per ribbon (#1020, #1608, #1685, #1763), swatch galleries, their
  sections, More button and KeyTips (#1761, #1762, #1823). A new ribbon command also needs
  a help target (`every_ribbon_command_has_a_help_target`: `docs/claude/help.md`), and
  a new command draw path its F1 probe, `ribbon-cmd:<id>` (#1869, same file).
- `docs/claude/help.md`: before changing the help manuals or the Documentation tab:
  manual bundles (#1022), the Help tab and F1 (#1855), the manual revision in
  About and the Help footer (#1857), F1 on a hovered ribbon command (#1869),
  F1 on a focused pane, `pane:<id>` (#2077).
- `docs/claude/ui-plugins.md`: before changing the UI plugin ABI, the plan's formatting
  model (`projcore::formatting`, `formatting.xml`) or the plugin dialogs and their layout:
  UI plugins (#1532, #1553, #1572, #1609), dialog values that follow (#1670), overlay text
  style (#1668), the rich-text control (#1557), grid `cell` columns (#1558).
- `docs/claude/project-formats.md`: before changing Project file reading or writing:
  format plugins (#1702, #1801), MPX import (#309, #1805), `.mpp` custom fields (#292),
  characters XML 1.0 forbids dropped from the save and the closed-editor paste (#1120),
  prefixed or attributed header leaves kept raw through a save (#429).
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
  (#1536, #1682), the note a save that drops Display on Timeline or Marked
  gives (#1618).
- `docs/claude/project-scheduling.md`: before changing Project scheduling, or how MSPDI
  and `.mpp` carry scheduled dates: leveling as a plan edit (#1594), elapsed durations and
  Must Start On before opening (#484).
- `docs/claude/sheet-ui.md`: before changing the sheet's tab rename (#1365), Home Font /
  Font Size boxes (#1344) or Save As names (#804, #1807), or the number-format runtime's
  fractions and seconds: sheet names Excel refuses on rename, fixed-denominator fractions,
  `mm:ss.0` tenths (#2115), or the cell editor's commit on a protected sheet and Protect
  Sheet committing an open entry first (#1054), unlocked cells taking values on a
  protected sheet (#2230), or floating pictures: their anchors, an
  unedited save keeping the drawing part, where the sheet view draws them, their
  cache and selection (#2228), or text spilling over empty neighbours: the
  rules shared with the sheet PDF, freeze panes, the text layer (#2240), or a
  row or column insert entry or Subtotal: every host refuses one that pushes data
  off (#1175), or a new workbook's Ctrl+S opening Save As, never replacing an
  `untitled.xlsx` (#907), or a sheet added or renamed from an outside name: the one
  check, taken names in any case, the deduped name, a file stem (#2153).
- `docs/claude/sheet-file.md`: before changing a sheet's notes, slicers, data validation
  or hyperlinks in `gridcore` (and the dialogs and Undo over them): notes that follow
  row/column insert and delete (#1335), table slicers on save and Undo of a table step
  (#1694, #1741), x14 rules in the Data Validation dialog (#1817, #1734), pivot slicer and
  timeline caches on save (#1797), `.xls` hyperlinks and file URIs (#1726), a quote-led
  custom validation formula shifts, inline list by kind (#2115), an absent
  cell's inherited row/column style in typing, paste, copy, fill, cut and
  spill output, `CopyReader`'s used-extent bound, `set_cell` never storing
  it (#1660), a cut paste's formulas: source-sheet local names, bare table
  refs, absolute refs in rules left in place, the snapshot's dry run (#1687),
  `.xls` page setup, page breaks and outlines read and written (#1440),
  `.xls`/`.xlsb`/`.ods` formats, widths, merges, comments, validation and
  conditional formats imported, styled blanks and their cap (#884),
  references into other workbooks kept: links, names' definitions, cached
  cells, quoted refs keeping their cache (#903), page setup, breaks, print
  area and titles from `.xlsb`/`.ods` (#901), a cut splitting a
  conditional-format rule its cells read differently, saved as clones
  (#1458), a paste into another workbook adopting the source's formats and
  saving their raw parts (#1011), rels helpers that write every rId they
  return, a damaged rels part rebuilt from what loaded (#831).
- `docs/claude/terminal-editors.md`: before changing terminal `docxy`, `xlsxy` or
  `yppxy`, or `backstagecore`'s Save As name field: docxy's Layout tab (#1274), prompts
  editing at a caret (#1372, #1373, #1374, #1375, #1379), Ctrl+C/X/V (#1377, #1378),
  Ctrl+A (#1376), the F1 help reader (#1856).
- `docs/claude/mail.md`: before changing `mailcore` or Mail: drafts keep their `local:`
  id (#1884, #1881), the body format (#1901), sync one folder, the fast cycle and Sent
  Items after a send (#1885), folders and counts on every tick, deleted folders (#1886),
  a failed send back to Drafts (#1874), the signed-in user's profile, photo and Sign
  out (#1920), their company and organisation name (#1997), another user's
  photo by address, a 403 told apart (#2157), presence of users by
  address, a 403 told apart (#2158), a different mailbox
  signing in resets the store, its outbox dropped (#1946), granted scopes and a
  refused sign-in's `error_description` (#1875), a refresh asking only for the granted
  scopes (#2164),
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
  a contact's photo fetched, set through the outbox, no delete on Graph (#2065),
  contact updates and deletes sent with `If-Match` on the stored changeKey,
  a 412 reported with Graph's version (#2067),
  cancelling a meeting with a message and forwarding it (#1985),
  a series' occurrences, its master and editing either (#2022),
  a series created with any Graph recurrence (#2034), a pending series'
  recurrence edited through its row (#2036),
  event times in a named time zone, zone-preserving series edits (#2039, #2030),
  respond options: no response, a proposal on tentative, attendees' proposals (#2023),
  meeting message types, the referred event and Remove from Calendar (#2024),
  dismiss and snooze reminders on the server (#2025),
  a moved message followed to its new id, its old ids still resolving (#1896),
  server message search and opening a hit not in the store (#2006),
  the organisation's directory searched on demand, a 403 told apart (#1942),
  its mail-enabled groups and a group's members, nested groups among them (#2058),
  a message's list row by id, batched, without its body (#2053),
  a flagged message's reminder, read, stored, snoozed and dismissed (#2054),
  an event's own response time and created time (#2068),
  a draft's unpushed body kept against a fetched one (#2082),
  a reply/forward draft's DraftReady names its original and kind (#2085),
  each message's meeting message type on its row (#2081),
  each message's protection (sensitivity, label, IRM) on its row (#2088),
  a meeting message's own reminder on its row (#2089),
  a reply draft's In-Reply-To, each message's Internet message id, `isDraft` kept (#2096),
  folders created, renamed and moved through the outbox (#1910), a folder deleted only
  after a walk of Graph finds nothing the store shows elsewhere (#2107),
  a folder create retried after a lost answer adopting Graph's folder on a 409 (#2109),
  a whole folder marked read on the server through `$batch` (#1913),
  a folder emptied on the server through `$batch`, unsent mail kept, FTS rows in bulk (#2110),
  folder-wide ops' cutoff on Graph's clock from its `Date` header (#2121),
  a folder copied with its contents through the outbox, synced after (#2118),
  every message of a folder moved on the server through `$batch` (Delete All), rows
  following Graph's answers, unsent mail kept, a dropped move reported (#2126),
  message sizes, the size fill, folder sizes on the server (#2123),
  progress of a folder's mark-read or empty, its end always `finished` (#2128),
  shutdown persists the writes queued before it, without draining (#2127),
  a meeting answer, Remove from Calendar, Automatic Replies or a save kept at a
  shutdown, queued for the next start (#2132),
  a folder's backfill walk run next on request, remembered (#2133),
  one folder's offline items cleared locally and synced again (#2124),
  a folder's last sync time and stored message count (#2125),
  lookxy's status bar: upload and backfill progress, one line cut to fit, and Esc on a
  reminder dismissing it on the server (#2117),
  OAuth tokens in the platform secret store, a plaintext fallback and its
  warning (#1877), contact search folded in Rust, word-prefix rank, `all_contacts`
  (#1890), a sign-out's back-off, an expired delta link re-running phase 1, a 404
  `DeleteEvent`, `local:` ids in message ops, the store's busy timeout, one-line
  token-endpoint errors (#2113), a transient token-endpoint failure kept as offline, a saved
  sign-in in a store that cannot be reached (#2167), the reason a sign-in is asked for, in
  lookxy's prompt (#2169), a meeting message's type filled when the class expand is refused
  (#2168), a draft's importance and follow-up flag, its flag on a PATCH only once set (#1955),
  Remove from Calendar and a meeting message's read when its event is gone, every failure
  answered (#2208), the optimistic message writes returning a busy error (#2209), a local
  reply or forward draft created through createReply/createForward (#2212), a sign-in
  refused for consent to `optional_scope` retried with `scope` alone, the optional
  scopes in `missing` and in a refresh only once granted (#2218), a meeting request's
  update facts in FetchMeetingMessage's answer (#2185), national clouds: the login
  host and Graph root per `AuthConfig::cloud`, a saved token's cloud (#2225), a failed
  sign-in with a token held keeping it, no fallback while it holds the required scopes
  (#2213), a failed mark-sent, local RSVP, outbox drop or attempt count told (#2215),
  a person's details by address from /me/people (#2239), a calendar round keeping
  a pending event write, the commands waiting during a refresh too (#2031), a
  host write made while it fetched and a delta removal (#2262), a
  deselected calendar keeping an event with a pending edit until it is
  sent (#2017),
  an edit during an event's in-flight create queued by its reconcile (#2005),
  lookxy's bounded wait for the engine at quit (#2131), a queued SaveDraft
  or SendDraft following its draft's move (#2265), a save kept at a
  shutdown never replacing a file made at its path (#2267), each
  message's last verb (replied, replied all, forwarded) on its row, and
  an answer sent here marking its original with one PATCH (#2301).

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
- `suite/vendor/gpui_linux` is gpui_linux plus `suite/vendor/gpui_linux.patch` (#1966):
  `python3 scripts/vendor-gpui-linux.py --check`, then `cargo test --manifest-path
  suite/Cargo.toml --locked -p gpui_linux --lib wayland::`. A gpui bump re-vendors
  (`--update`; `suite/vendor/README.md`).

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
  arrives nowhere else. WM_CHARs come in key-down order but may come late
  (gpui drains input alone under load), so all Windows text is typed from
  them through one queue (`text_input::WmChars`, #1277). While the root has
  the focus and the app takes text, a plain or AltGr (Ctrl+Alt with
  `prefer_character_input`) key that owes text (`owed_text`) is withheld
  from `on_key`: a plain key is replayed unchanged at its first character,
  and an AltGr key types its characters (never its Ctrl+Alt chord; under a
  dialog or Project edit only while one is open). A withheld key holds every
  later key-down and the Tab/Shift+Tab actions (`queued_tab`) until its
  character comes, at most `WM_CHAR_WAIT` (250 ms; then a plain key is
  replayed). A mouse-down anywhere (a window listener from `key_routing`)
  flushes the queue first, AltGr keys typing then: a press within
  `WM_CHAR_WAIT` of an AltGr dead key types its accent (a key-down cannot
  tell it from a character) unless the plain key right after it carries it.
  Other keys reach `on_key` at once and owe
  their text; without the root's focus they owe nothing (no handler, no
  WM_CHAR). A character's candidates are the leading entries not sure to be
  paid (`may_not_come`, or past `WM_CHAR_WAIT`) and the first sure one; it
  pays the sure one if it can, else the first other that matches. Before the
  paid one, withheld and waiting entries are dropped, typed debts stay until
  they lapse. Matching none, it is a packet's and is typed. No count cap; `WM_CHAR_KEEP`
  (60 s) is hygiene. A plain dead key never reaches `on_key`
  (`passes_to_on_key`): the next key-down's `key_char` carries the composed
  text whole ("´" on Space, "´x"). The handler's `accepts_text_input` is
  false there, so the Windows IME stays off. Linux registers none. The
  harness's `win-char` sends WM_CHAR, and `real-type`'s `"wm_char":true`
  (always on Windows) or `"late"` sends each key's twin while the root has
  the focus.
- A dialog field takes the character ⌥ made on macOS: `modal_takes_key`
  hands the dialog `text_input::dialog_stroke`'s plain typed key (#1162);
  the Project cell editor and prompt do the same (`project_stroke`, #1312).
  `edit_key` still swallows every ⌥ key, and Windows' plain Alt is never text.

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
