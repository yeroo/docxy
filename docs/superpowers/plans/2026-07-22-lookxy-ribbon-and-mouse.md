# lookxy Ribbon + Mouse — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development
> or superpowers:executing-plans. Steps use `- [ ]`. Executed inline (subagent
> cap reached).

**Goal:** Mouse support (click panes, wheel-scroll) and a collapsible ribbon
(File/Home/Send-Receive/Folder/View/Help) with a File backstage, porting the
docxy pattern. Phase A = mouse, Phase B = ribbon.

**Architecture:** `lookxy` only. `App` records interactive `Rect`s during draw;
`App::on_mouse` hit-tests them. `ui/ribbon.rs` + `ui/backstage.rs` port
`docxy/src/ribbon.rs` + `docxy/src/backstage.rs`, adapted to lookxy's `Act` set.

**Tech Stack:** ratatui/crossterm (mouse events already available). No new deps.

## Global Constraints

- MSRV 1.88; `lookxy` only. Full workspace green, `clippy --all-targets
  -D warnings` clean, `fmt` clean. Build via `bash "$LCARGO" …`. Each task committed.
- Spec: `docs/superpowers/specs/2026-07-22-lookxy-ribbon-and-mouse-design.md`.
- Reference templates: `docxy/src/ribbon.rs`, `docxy/src/backstage.rs`,
  `docxy/src/main.rs` (`on_mouse`, mouse-capture setup, `ribbon_*` state) — keep
  the same shapes/method names where they fit.
- Link hit-testing is out of scope (Phase 3).

---

## Task A1: Mouse capture + rect recording + click-to-focus/select

**Files:** `lookxy/src/main.rs` (capture + `Event::Mouse` arm), `lookxy/src/app.rs`
(rect fields, `on_mouse`), `lookxy/src/ui/mod.rs` + pane modules (record rects).

**Interfaces:** Produces `App::on_mouse(MouseEvent)`, and `App` rect fields
`rail_rect`/`folders_rect`/`folders_row0`/`list_rect`/`list_row0`/`reading_rect`
(all `Rect`/`u16`, default zero).

- [ ] **Step 1 — failing test** (`app.rs` tests):

```rust
#[test]
fn clicking_a_folder_row_selects_it() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut app = App::for_test_with_folder_tree();
    // Simulate the layout draw() would produce.
    app.folders_rect = ratatui::layout::Rect::new(5, 0, 20, 10);
    app.folders_row0 = 1; // first folder row (inside the border)
    // Row 1 on screen = first visible folder (Inbox); row for "sent" is index 1.
    let sent_row = app.folders_row0 + app.visible_folders.iter().position(|v| v.row.id=="sent").unwrap() as u16;
    app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left),
        column: 7, row: sent_row, modifiers: Default::default() });
    assert_eq!(app.selected_folder.as_deref(), Some("sent"));
    assert_eq!(app.focus, Pane::Folders);
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.**
  - `app.rs`: add the rect fields (init `Rect::ZERO`/`0`), and `pub fn
    on_mouse(&mut self, m: MouseEvent)`. For a left-click (`MouseEventKind::
    Down(MouseButton::Left)`), test the point against each rect (skip if any
    full-frame modal is open — mirror docxy's early returns):
    - rail_rect → `set_mode` by clicked row, `focus = Rail`.
    - folders_rect → `focus = Folders`; `idx = row - folders_row0`; if
      `idx < visible_folders.len()` select it (set `folder_index`,
      `selected_folder`, `reload_messages`).
    - list_rect → `focus = List`; select the clicked message row + preview.
    - reading_rect → `focus = Reading`.
  - `ui/mod.rs`/panes: set `app.rail_rect`, `folders_rect`, `folders_row0`
    (area.y + 1 for the border), `list_rect`, `list_row0`, `reading_rect` as
    each is laid out. (Draw takes `&mut App` already.)
  - `main.rs`: `execute!(EnableMouseCapture)` on setup, `DisableMouseCapture` on
    teardown; add `Event::Mouse(m) => app.on_mouse(m)` to the event loop.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: mouse capture + click a pane to focus/select`.

---

## Task A2: Click message rows (preview / activate), chevron toggle, wheel scroll

**Files:** `lookxy/src/app.rs` (`on_mouse` extensions).

- [ ] **Step 1 — failing tests** (`app.rs`):

```rust
#[test]
fn clicking_a_message_previews_then_activates_on_second_click() {
    let mut app = App::for_test_with_seeded_store();
    app.threaded = false; app.reload_messages();
    app.list_rect = Rect::new(25, 0, 30, 10); app.list_row0 = 1;
    let click = |app: &mut App| app.on_mouse(MouseEvent { kind:
        MouseEventKind::Down(MouseButton::Left), column: 27, row: 1,
        modifiers: Default::default() });
    click(&mut app); // first click on m1: preview, still unread, focus List
    assert_eq!(app.selected_msg.as_deref(), Some("m1"));
    assert_eq!(app.focus, Pane::List);
    assert!(app.messages.iter().find(|m|m.id=="m1").unwrap().is_read == false);
    click(&mut app); // second click on the selected row: activate
    assert_eq!(app.focus, Pane::Reading);
    assert!(app.messages.iter().find(|m|m.id=="m1").unwrap().is_read);
}

#[test]
fn wheel_over_the_reader_scrolls_the_body() {
    let mut app = App::for_test_with_seeded_store();
    app.reading_rect = Rect::new(55, 0, 40, 20);
    app.open_message("m1");
    app.reading_content_rows = 100; app.reading_viewport = 10; // scrollable
    app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: 60, row: 5, modifiers: Default::default() });
    assert!(app.reading_scroll > 0);
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.**
  - List left-click: when the clicked row is already `msg_index`/selected,
    call `activate_selected` (mark read + Reading); else select + preview.
  - Folders left-click in the chevron column (x within
    `folders_rect.x + 1 + 2*depth ..= +chevron cell`) → `expand_selected`/
    `collapse_or_parent` instead of only selecting. (Compute from the clicked
    row's `VisibleFolder`.)
  - Wheel: `ScrollUp`/`ScrollDown` → if pointer in `reading_rect`
    `reading_scroll_by(∓3)`; if in `list_rect`/`folders_rect` move that
    selection by ∓1 (previewing for the list).
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: click-to-open messages, chevron toggle, wheel scroll`.

---

## Task B1: Ribbon data model + rendering (port docxy/src/ribbon.rs)

**Files:** Create `lookxy/src/ui/ribbon.rs`; `lookxy/src/ui/mod.rs` (`pub mod ribbon;`).

**Interfaces:** Produces `Act`, `Focus`, `Hit`, `Dir`, `Ribbon` with
`Ribbon::home()`, `render_tabs(focus)`, `render_body(focus)`, `render_hint`,
`hit`, `nav`, `enter_body`, `button_count`, `active_tab`, `set_active`,
`set_toggles`.

- [ ] **Step 1 — failing tests** (port docxy's ribbon tests, adapted): assert
  `Ribbon::home()` has 6 tabs, Home tab exposes `Act::Compose`, `button_count`
  > 0, and `hit(x, 0)` returns the right `Tab`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Port the `Seg`/`btn`/`Group`/`Placed`/`Focus`/
  `Hit`/`Dir`/`Ribbon` shapes from `docxy/src/ribbon.rs` verbatim, replacing the
  command set with lookxy's `Act`:
  `Compose, Reply, ReplyAll, Forward, Delete, Flag, MarkRead, MarkUnread, Move,
  Categorize, Find, NewEvent, EditEvent, DeleteEvent, RsvpAccept, RsvpDecline,
  RsvpTentative, SendReceive, ExpandAll, CollapseAll, Threaded, CategoryFilter,
  Help, AutoReplies, Settings, Exit, Todo(&'static str)`.
  Tabs: `["File","Home","Send/Receive","Folder","View","Help"]`; File has no
  groups (opens backstage). Build Home's groups with the mail buttons (Task B4
  adds the calendar variant). Keep `render_tabs`/`render_body`/`render_hint`/
  `hit`/`nav` logic identical to docxy.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: port the docxy ribbon (tabs, Acts, render/hit/nav)`.

---

## Task B2: Ribbon in the layout + F9/arrow/Esc navigation

**Files:** `lookxy/src/app.rs` (`ribbon: Ribbon`, `ribbon_open: bool`,
`ribbon_focus: Focus`, `ribbon_h: u16`), `lookxy/src/ui/mod.rs` (draw the tab
strip + body, record `ribbon_h`; route F9/arrows/Esc when `ribbon_focus != None`).

- [ ] **Step 1 — failing test** (`ui/mod.rs`): F9 sets `ribbon_focus` to a tab
  and `ribbon_open = true`; `←/→` move the focused tab; `Esc` returns
  `ribbon_focus = None`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Add the fields (default collapsed/`Focus::None`).
  In `draw`, always render the tab strip at the top (1 row); when
  `ribbon_open`, render the body rows beneath and set `ribbon_h` accordingly;
  everything else lays out below `ribbon_h`. In `handle_key`, before the pane
  match: `F9` → focus tabs + open; when `ribbon_focus != None`, route
  `←/→/↑/↓` to `ribbon.nav` (updating `active` when moving tabs), `Enter` →
  `run_ribbon_act` (Task B3), `Esc` → `Focus::None` + collapse.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: ribbon in the layout + F9/arrow/Esc navigation`.

---

## Task B3: Mouse on the ribbon + Act dispatch

**Files:** `lookxy/src/app.rs` (`ribbon_click`, `run_ribbon_act`), `on_mouse`
ribbon branch; `lookxy/src/ui/mod.rs` if needed.

- [ ] **Step 1 — failing tests** (`app.rs`): clicking a tab header switches the
  active tab + opens; `run_ribbon_act(Act::Compose)` opens the composer;
  `run_ribbon_act(Act::Help)` sets `help = true`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** `on_mouse`: when the click y is within `ribbon_h`,
  call `ribbon_click(x, y)` which uses `ribbon.hit(...)` → `Hit::Tab(i)` sets
  active + open; `Hit::Button(act)` → `run_ribbon_act(act)`. `run_ribbon_act`
  matches every `Act` to its `App` method (see spec); `Act::Todo(n)` sets a
  status. After a button runs, hand focus back to the panes (and collapse if
  needd, matching docxy's post-click behavior).
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: ribbon mouse clicks + Act dispatch`.

---

## Task B4: Context-aware Home + toggle state

**Files:** `lookxy/src/ui/ribbon.rs` (calendar Home group + `set_toggles`),
`lookxy/src/app.rs` (rebuild ribbon Home on mode change; feed toggles).

- [ ] **Step 1 — failing test:** in Calendar mode the Home tab exposes
  `Act::NewEvent` (not `Act::Compose`); `Threaded` shows active when
  `app.threaded`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Give Home two group sets (mail / calendar); a
  `Ribbon::set_home_context(calendar: bool)` swaps them. Call it whenever
  `set_mode` runs. Feed `set_toggles(vec![Threaded if app.threaded, …])` before
  drawing so toggle buttons invert.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: context-aware Home tab + toggle-button state`.

---

## Task B5: File backstage (port docxy/src/backstage.rs)

**Files:** Create `lookxy/src/ui/backstage.rs`; `lookxy/src/app.rs`
(`backstage: Option<Backstage>`, open/close, key + mouse), `ui/mod.rs` (draw +
route).

**Interfaces:** Produces `Backstage` state; entries Automatic Replies / Settings
/ Exit.

- [ ] **Step 1 — failing tests** (`app.rs`): opening the backstage then
  selecting Automatic Replies opens the OOF form; toggling the Threaded setting
  flips `app.threaded`; selecting Exit sets `app.quit`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Port `backstage.rs`'s left-menu + right-content
  layout. Entries: Automatic Replies (`open_oof_form` + close backstage),
  Settings (rows: Threaded on/off → `toggle_threaded`; Reminder notifications
  on/off → flip `reminders_notify` + persist), Exit (`quit = true`). Open from
  the File tab / `run_ribbon_act(Act::AutoReplies|Settings|Exit)` or a File-tab
  click; route its own keys (↑/↓ select, Enter activate, Esc close) and mouse in
  `on_mouse` (early-return branch like docxy's `bs_mouse`). Draw it full-frame
  when open (like compose/OOF), ahead of the panes.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: File backstage (automatic replies, settings, exit)`.

---

## Task B6: Folder ops + Send/Receive wiring + help/status polish

**Files:** `lookxy/src/app.rs` (`expand_all_folders`/`collapse_all_folders`,
manual-sync trigger), `lookxy/src/ui/help.rs` (add F9 ribbon line).

- [ ] **Step 1 — failing tests:** `expand_all_folders` expands every folder with
  children; `run_ribbon_act(Act::SendReceive)` sends the refresh `SyncCommand`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** `expand_all_folders`/`collapse_all_folders`: set
  `is_expanded` on every folder (store + cached rows) and rebuild the visible
  tree. `SendReceive`: send whatever `SyncCommand` triggers a mail delta +
  `RefreshCalendar` (check the engine's command set). Add a "F9 ribbon · mouse:
  click to select" line to the help overlay + a status hint.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: Expand/Collapse All, Send/Receive, help/status for ribbon+mouse`.

---

## Self-Review

- **Coverage:** mouse capture+click+wheel (A1–A2); ribbon model/render (B1),
  layout+keys (B2), mouse+dispatch (B3), context Home+toggles (B4), backstage
  (B5), folder ops/send-receive/help (B6). Every spec section maps to a task.
- **Type consistency:** `Act`/`Focus`/`Hit`/`Dir`/`Ribbon` names match docxy;
  rect field names shared across A1/A2/B2/B3.
- **Ordering risk:** A1 adds fields set by `draw` — every draw path (Mail +
  Calendar) must set them or they stay zero (safe no-op). B2 changes the
  top-of-frame layout (ribbon_h) — the rail/pane rects recorded in A1 shift down
  by `ribbon_h`; recompute them from the post-ribbon area.
- After each task: `test --workspace`, `clippy --all-targets -D warnings`, `fmt`.
  Final whole-branch review (direct), then push to PR #21.
