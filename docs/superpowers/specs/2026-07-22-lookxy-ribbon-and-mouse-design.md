# lookxy — Ribbon + Mouse Support

**Date:** 2026-07-22
**Status:** design

## Goal

Add mouse support and a Word/Outlook-style ribbon to lookxy, **mirroring the
existing docxy/xlsxy/yppxy pattern** (`docxy/src/ribbon.rs`,
`docxy/src/backstage.rs`, and the mouse plumbing in `docxy/src/main.rs`). The
ribbon exposes the features lookxy already has as clickable, discoverable
buttons; nothing new happens under the hood — every button triggers an existing
`App` method. Built in two phases: **A. mouse support**, then **B. the ribbon +
File backstage**.

## Global constraints

- Rust 2024, MSRV 1.88; `lookxy` (TUI) only.
- Full workspace green, `clippy --all-targets -D warnings` clean, `fmt` clean.
- TDD, task-by-task, each committed. No new dependencies (crossterm already
  provides mouse events; ratatui provides `Rect::contains`).
- **Reuse the docxy conventions verbatim where they fit** — the `Ribbon`/`Act`/
  `Focus`/`Hit`/`Dir` shapes, the collapsed-by-default + F9 model, the hint bar,
  the backstage layout — so a reader who knows docxy's ribbon knows lookxy's.
- Link clicking / link hit-testing stays **Phase 3** (the separate links spec).

---

## Phase A — Mouse support

### Enabling mouse

`main.rs`: wrap terminal setup with `EnableMouseCapture` / `DisableMouseCapture`
(as docxy does), and add a `Event::Mouse(m) => app.on_mouse(m)` arm to the event
loop next to `Event::Key`.

### Recording hit regions during draw

ratatui doesn't track element positions, so — exactly like docxy — `ui::draw`
records the screen `Rect` of each interactive region into `App` fields as it
lays them out:

- `rail_rect: Rect` and the two section rows (Mail row y, Calendar row y).
- `folders_rect: Rect` — plus the first visible row's y, so a click row maps to
  `folder_index` (`visible_folders[row]`); a click in the chevron column
  (x within the indent+chevron cells) toggles expand/collapse instead of just
  selecting.
- `list_rect: Rect` — click row → `msg_index`/`row_index`.
- `reading_rect: Rect`.
- (Phase B adds `ribbon` rects.)

Default all rects to `Rect::ZERO` in `App::new`; `contains` on a zero rect is
always false, so mouse events before the first draw are safely ignored.

### `App::on_mouse(MouseEvent)`

Dispatch by region and kind, following docxy's `on_mouse` structure (modals
first, then regions):

1. If a modal/overlay owns the screen (signin, compose, oof, backstage, pickers,
   help), route its own mouse handling or ignore — return early.
2. **Left-click** in:
   - `rail_rect` → set the clicked section (`set_mode`) and `focus = Rail`.
   - `folders_rect` → `focus = Folders`, select the clicked folder; a click in
     the chevron cell expands/collapses it.
   - `list_rect` → `focus = List`, select the clicked message and preview it; a
     click on the already-selected row **activates** it (open + mark read +
     `focus = Reading`) — the terminal-friendly "double-click" (click-to-select,
     click-again-to-open).
   - `reading_rect` → `focus = Reading`.
3. **Wheel** (`ScrollUp`/`ScrollDown`): scroll the region under the pointer — the
   reading pane scrolls the body (`reading_scroll_by`), the list/folders move
   the selection (± a few rows). Wheel works regardless of focus (docxy lets
   wheel fall through anywhere).

### Testing (Phase A)

- Clicking a folder row selects that folder (drives `App::on_mouse` with a
  synthesized `MouseEvent` at a known row after a draw sets the rects).
- Clicking a message row selects + previews (unread preserved); clicking the
  selected row again activates (focus Reading, marked read).
- Clicking the rail's Calendar row switches to Calendar.
- Wheel over the reader scrolls; over the list moves selection.
- A click before any draw (zero rects) is a no-op (no panic).

---

## Phase B — The ribbon + File backstage

Port `docxy/src/ribbon.rs` into `lookxy/src/ui/ribbon.rs`, keeping the same
`Ribbon`/`Act`/`Seg`/`Group`/`Placed`/`Focus`/`Hit`/`Dir` shapes and the same
render/hit/nav methods, and `docxy/src/backstage.rs` into
`lookxy/src/ui/backstage.rs`. Only the command set (`Act`) and the dispatch
differ — they're lookxy's.

### Activation & navigation (same as docxy)

- The ribbon is **collapsed to its tab strip** by default (1 row at the top).
- **F9** focuses the tab strip (and expands the body); **←/→** move across tabs;
  **↓** enters the button rows; **←/→/↑/↓** move between buttons; **↑** from the
  top button row returns to the tabs; **Enter** activates the focused button;
  **Esc** returns focus to the panes (and collapses the body).
- **Mouse:** click a tab header to switch/expand; click a button to run it.
- A **hint bar** (black-on-yellow) along the bottom shows the focused/hovered
  button's description.
- The tab strip lives above the rail+panes; when expanded, the button rows push
  the panes down (`ribbon_h` grows), same as docxy.

### Tabs & buttons (minimum for existing features)

Every `Act` maps to one existing `App` method (in parentheses). Home is
**context-aware** — mail actions in Mail mode, event actions in Calendar mode.

- **File** → opens the **backstage** (no in-ribbon body), see below.
- **Home (Mail):** New (`compose_new`) · Reply (`compose_reply(false)`) · Reply
  All (`compose_reply(true)`) · Forward (`compose_forward`) · Delete
  (`delete_selected`) · Flag (`toggle_flag`) · Read/Unread (`mark_read`) · Move
  (`open_move_picker`) · Categorize (`open_category_picker(Assign)`) · Find
  (`start_search`).
- **Home (Calendar):** New Event (`open_new_event`) · Edit (`open_edit_event`) ·
  Delete (`delete_selected_event`) · Accept/Decline/Tentative (`start_rsvp`).
- **Send / Receive:** Send & Receive — trigger a manual sync (a
  `SyncCommand` refresh; reuse the mail delta/`RefreshCalendar` path). Shows the
  sync status.
- **Folder:** Expand All · Collapse All — new cheap folder-tree ops
  (`expand_all_folders`/`collapse_all_folders`, setting every folder's
  `is_expanded` and rebuilding the visible tree).
- **View:** Threaded/Flat (`toggle_threaded`, a toggle button) · Category Filter
  (`open_category_picker(Filter)`).
- **Help:** Keyboard Shortcuts (`open_help`) · About (`Act::Todo` for now, or a
  one-line status).

Icons: styled single-width letters and plain symbols where they align exactly
(docxy's approach), emoji only where a genuinely single-or-double-width glyph
reads well and the layout math stays exact. Text labels otherwise.

### File backstage

Port `backstage.rs`: a full-frame overlay (opened by the File tab / `Esc`
closes) with a left menu of entries and a right content/preview area. Entries,
minimal:

- **Automatic Replies** → `open_oof_form` (closes the backstage, opens the OOF
  editor).
- **Settings** → toggles for `threaded` and `reminders_notify` (persisted via
  the existing `persist_*_to` helpers), shown as on/off rows.
- **Exit** → sets the quit flag.

### Dispatch

A single `App::run_ribbon_act(Act)` (mirroring docxy's ribbon-act match) maps
each `Act` to its method, and `Act::Todo(name)` sets a `"<name> not implemented
yet"` status. Mouse `Hit::Button(act)` and keyboard Enter both call it; toggle
buttons reflect state via the ribbon's `active_toggles`.

### Interaction with the spatial nav model

- F9/ribbon focus is a distinct focus state from the pane `Pane` focus; while
  the ribbon has focus, arrows drive the ribbon, and Esc/click-in-a-pane hands
  focus back to the panes. This is exactly docxy's `ribbon_focus` vs document
  focus split.
- All existing keyboard shortcuts keep working unchanged (the ribbon is
  additive).

### Testing (Phase B)

- Ribbon starts collapsed (`ribbon_h` = 1 tab row); F9 expands + focuses tabs.
- `nav` across tabs and into/out of buttons matches docxy's rules (port its
  tests).
- `hit(x, y)` returns the right `Tab`/`Button`/`Outside`.
- `run_ribbon_act` for a representative button (e.g. Home▸New) opens the
  composer; a Calendar-context button (New Event) opens the event form.
- Home shows mail buttons in Mail mode and event buttons in Calendar mode.
- Backstage: opening from File shows Automatic Replies/Settings/Exit; clicking
  Automatic Replies opens the OOF form; a Settings toggle flips + persists;
  Exit sets quit.
- Clicking a tab header (mouse) switches tabs and expands.

## Out of scope

- Link hit-testing / clicking (Phase 3, links spec).
- New mail/calendar features — the ribbon only surfaces what already exists.
- Drag-select, resize-by-drag, right-click menus.
