# lookxy Spatial Navigation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development
> or superpowers:executing-plans. Steps use checkbox (`- [ ]`). Executed inline
> this session (subagent cap reached).

**Goal:** Arrow-driven spatial navigation with a level-0 Mail/Calendar rail,
auto-preview on list navigation, and prev/next-message paging in the reader;
remove `g`/`Tab`/`Shift+Tab`.

**Architecture:** All in `lookxy`. A new `Pane::Rail` + `ui/rail.rs`; `handle_key`
routes bare arrows (and `h`/`j`/`k`/`l`) through four intent fns
`nav_left/right/up/down`. `Mode` is now rail-driven.

**Tech Stack:** ratatui/crossterm. No new deps, no `mailcore` changes.

## Global Constraints

- MSRV 1.88; `lookxy` only. Full workspace green, `clippy --all-targets
  -D warnings` clean, `fmt` clean. Build via `bash "$LCARGO" …`. Each task committed.
- Spec: `docs/superpowers/specs/2026-07-22-lookxy-spatial-navigation-design.md`.
- Preview (auto-open on list move) must NOT mark read; **activate** marks read.

---

## Task 1: Rail pane — variant, layout, rendering, mode switch

**Files:** Create `lookxy/src/ui/rail.rs`; modify `lookxy/src/app.rs`
(`Pane::Rail`), `lookxy/src/ui/mod.rs` (module, layout, draw, top-of-`handle_key`
rail routing).

**Interfaces:** Produces `Pane::Rail`; `rail::draw(f, app, area)`;
`App`-level `mode` switched when `focus == Pane::Rail` and Up/Down pressed.

- [ ] **Step 1 — failing test** (`ui/mod.rs` tests):

```rust
#[test]
fn rail_up_down_switches_mail_and_calendar() {
    let mut app = App::for_test_with_seeded_store();
    app.focus = Pane::Rail;
    assert_eq!(app.mode, Mode::Mail);
    handle_key(&mut app, KeyEvent::from(KeyCode::Down));
    assert_eq!(app.mode, Mode::Calendar);
    handle_key(&mut app, KeyEvent::from(KeyCode::Up));
    assert_eq!(app.mode, Mode::Mail);
}
```

- [ ] **Step 2 — run, expect FAIL** (`Pane::Rail` missing).
- [ ] **Step 3 — implement.**
  - `app.rs`: add `Rail` to `enum Pane`.
  - `ui/rail.rs`: `draw(f, &App, Rect)` — a bordered 5-wide column, two rows
    `✉`/`📅`, highlight the row matching `app.mode` (reverse style); `focused`
    border when `app.focus == Pane::Rail`.
  - `ui/mod.rs` `draw`: prepend a `Constraint::Length(5)` rail column in BOTH
    the Mail three-pane layout and the Calendar layout; call `rail::draw`.
  - `ui/mod.rs` `handle_key`: right after the modal routers and before the
    `mode == Calendar` branch, add: `if app.focus == Pane::Rail { … return; }`
    handling `Up/k → mode=Mail`, `Down/j → mode=Calendar`, `Right/l/Enter →`
    enter section (`focus = Pane::Folders`), and nothing else. (Left is a no-op.)
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: Mail/Calendar rail (Pane::Rail) + layout + mode switch`.

---

## Task 2: nav_left / nav_right across Mail panes; remove g/Tab/Shift-Tab

**Files:** modify `lookxy/src/ui/mod.rs` (arrow routing, `nav_left`,
`nav_right`, delete `cycle_focus`/`cycle_focus_back`/`focus_back` + `g` arm),
`lookxy/src/app.rs` (activate marks read).

**Interfaces:** Produces `nav_left(app)`, `nav_right(app)`; `Enter`/`Right`
activate marks the opened message read.

- [ ] **Step 1 — failing tests** (`ui/mod.rs` tests) — replace the two A1 tests:

```rust
#[test]
fn right_from_folders_enters_list_then_activates_to_reading() {
    let mut app = App::for_test_with_seeded_store();
    app.focus = Pane::Folders;
    handle_key(&mut app, KeyEvent::from(KeyCode::Right)); // leaf folder → List
    assert_eq!(app.focus, Pane::List);
    handle_key(&mut app, KeyEvent::from(KeyCode::Right)); // message → activate
    assert_eq!(app.focus, Pane::Reading);
    assert!(app.selected_msg.is_some());
}

#[test]
fn left_walks_back_out_to_the_rail() {
    let mut app = App::for_test_with_seeded_store();
    app.focus = Pane::Reading;
    handle_key(&mut app, KeyEvent::from(KeyCode::Left));
    assert_eq!(app.focus, Pane::List);
    handle_key(&mut app, KeyEvent::from(KeyCode::Left));
    assert_eq!(app.focus, Pane::Folders);
    handle_key(&mut app, KeyEvent::from(KeyCode::Left)); // top-level folder → Rail
    assert_eq!(app.focus, Pane::Rail);
}

#[test]
fn g_tab_and_backtab_no_longer_navigate() {
    let mut app = App::for_test_with_seeded_store();
    app.focus = Pane::Folders;
    let before = (app.mode, app.focus);
    handle_key(&mut app, KeyEvent::from(KeyCode::Char('g')));
    handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
    handle_key(&mut app, KeyEvent::from(KeyCode::BackTab));
    assert_eq!((app.mode, app.focus), before);
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.**
  - Delete `cycle_focus`, `cycle_focus_back`, `focus_back`, their `Tab`/`BackTab`/
    `Left|h|Esc` arms, and the `Char('g') => toggle_mode` arm.
  - Add arrow routing in the Mail-mode match: `Left|Char('h') => nav_left(app)`,
    `Right|Char('l') => nav_right(app)`. Keep the existing Folders-pane
    `expand_selected`/`collapse_or_parent`/`toggle` — but fold expand/collapse
    INTO `nav_left`/`nav_right` (below) so there's one place per direction.
  - `nav_right(app)`:
    - `Folders`: if selected folder has children & collapsed → `expand_selected`
      then move selection to the first child (the row right after it); else →
      `focus = List` and preview first message (preview added in Task 3; here
      just set focus + `msg_index = 0`).
    - `List`: on a collapsed thread header → expand + select first child (reuse
      `activate_thread_row` header path WITHOUT focusing Reading — or a new
      `expand_thread_row`); on a message → `activate(app)` (focus Reading).
    - `Rail`/`Reading`: no-op (Rail handled earlier; Reading has nothing right).
  - `nav_left(app)`:
    - `Reading` → `focus = List`.
    - `List` → `focus = Folders`.
    - `Folders`: expanded → `collapse_or_parent`; else has-parent →
      `collapse_or_parent` (its parent-jump branch); else top-level →
      `focus = Rail`.
    - `Rail`: no-op.
  - `Esc` (Mail, non-capturing): behave as `nav_left` when focus is
    Reading/List (keep the reminder/category Esc handlers ahead of it).
  - `app.rs` `activate`/`activate_thread_row`: when opening a message, also
    `mark_read(true)` for that id (thread header → its latest id).
- [ ] **Step 4 — run, expect PASS** (update/replace the old A1 tests).
- [ ] **Step 5 — commit:** `lookxy: spatial nav_left/nav_right; drop g/Tab/Shift-Tab`.

---

## Task 3: List auto-preview on Up/Down (and on entry)

**Files:** modify `lookxy/src/ui/mod.rs` (`nav_down`/`nav_up` or `move_selection`
List arm), `lookxy/src/app.rs` (a `preview_selected_message` helper).

**Interfaces:** Produces `App::preview_selected_message()` — opens the body of
the row under the cursor without marking read.

- [ ] **Step 1 — failing test** (`app.rs` tests):

```rust
#[test]
fn arrowing_the_list_previews_without_marking_read() {
    let mut app = App::for_test_with_seeded_store(); // one unread msg "m1"
    app.focus = Pane::List;
    app.preview_selected_message();
    assert_eq!(app.selected_msg.as_deref(), Some("m1"));
    assert_eq!(app.focus, Pane::List);        // still in the list
    let unread = app.messages.iter().find(|m| m.id == "m1").map(|m| !m.is_read);
    assert_eq!(unread, Some(true));           // preview did NOT mark read
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.**
  - `preview_selected_message`: resolve the id under the cursor (threaded: the
    `Row::Message` id, or a collapsed header's latest; flat: `messages[msg_index]`)
    and call `open_message(&id)` — no `mark_read`, no focus change.
  - In the List arm of the up/down handler, after moving the selection, call
    `preview_selected_message`. Also call it when `nav_right`/`Enter` first enters
    the List from Folders (so the first message previews on arrival).
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: auto-preview the message under the cursor (no mark-read)`.

---

## Task 4: Reading active — PgUp/PgDn prev/next message

**Files:** modify `lookxy/src/ui/mod.rs` (PgUp/PgDn routing when
`focus == Reading`), `lookxy/src/app.rs` (`open_sibling_message(delta)`).

**Interfaces:** Produces `App::open_sibling_message(delta: isize) -> bool` —
moves the list selection by one selectable message, opens + marks it read,
keeps focus in Reading; returns false at the ends.

- [ ] **Step 1 — failing test** (`app.rs` tests): seed two messages in the folder.

```rust
#[test]
fn pgdn_in_the_reader_opens_and_reads_the_next_message() {
    let mut app = App::for_test_with_two_messages(); // "m1","m2" both unread
    app.focus = Pane::Reading;
    app.open_message("m1");
    let moved = app.open_sibling_message(1);
    assert!(moved);
    assert_eq!(app.selected_msg.as_deref(), Some("m2"));
    assert_eq!(app.focus, Pane::Reading);            // stays active
    assert!(app.messages.iter().find(|m| m.id=="m2").unwrap().is_read); // marked read
    assert!(!app.open_sibling_message(1));           // no-op past the end
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** `open_sibling_message`: compute the next/prev
  selectable message index (flat: clamp `msg_index ± 1`; threaded: next/prev
  `Row::Message` in `visible_rows`), update the cursor, `open_message` + `mark_read(true)`,
  keep focus. Route `PgUp → open_sibling_message(-1)`, `PgDn → open_sibling_message(1)`
  in the match, guarded to `focus == Pane::Reading` (ahead of the existing
  reading-scroll page arms, which move to `Home/End` only). Add a
  `for_test_with_two_messages` helper if none fits.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: PgUp/PgDn steps prev/next message in the reader`.

---

## Task 5: Calendar via the rail

**Files:** modify `lookxy/src/ui/calendar.rs` (Left/Esc → Rail, Right/Enter →
edit; drop `Esc/g → toggle_mode`), `lookxy/src/ui/mod.rs` if routing needs it.

- [ ] **Step 1 — failing test** (`ui/calendar.rs` or `ui/mod.rs` tests):

```rust
#[test]
fn left_from_the_agenda_returns_to_the_rail() {
    let mut app = App::for_test_with_seeded_store();
    app.mode = Mode::Calendar;
    app.focus = Pane::Folders; // "in the agenda" (non-rail)
    crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Left));
    assert_eq!(app.focus, Pane::Rail);
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** In `calendar::handle_key`: replace the
  `Esc | Char('g') => toggle_mode` arm with `Left | Esc → app.focus = Pane::Rail`
  (only when no overlay/prompt is capturing — those route earlier). Add
  `Right | Enter → app.open_edit_event()`. Keep `j`/`k`/↑/↓, `c`/`e`/`x`,
  `a`/`d`/`t`, `O`. Ensure the top-of-`handle_key` Rail routing (Task 1) still
  runs before `calendar::handle_key` so `focus == Rail` keys switch section.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: reach Calendar via the rail; agenda Left returns to it`.

---

## Task 6: Help overlay + status bar to the new keymap

**Files:** modify `lookxy/src/ui/help.rs` (groups), `lookxy/src/ui/status_bar.rs`
(hint text) if it names `Tab`/`g`.

- [ ] **Step 1 — failing test** (`ui/help.rs` tests): assert the overlay text now
  contains a "Rail" group header and no longer advertises `Tab`.

```rust
#[test]
fn help_lists_the_rail_and_arrow_model() {
    let mut app = App::for_test_with_seeded_store();
    app.open_help();
    let text = render_help_to_string(&app); // existing draw-to-buffer pattern
    assert!(text.contains("Rail"));
    assert!(!text.contains("Tab "));
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Rewrite `HELP` groups: **Rail** (↑/↓ Mail/Calendar,
  → enter), **Folders** (↑/↓ move, → expand/enter, ← collapse/parent/rail),
  **Message list** (↑/↓ move+preview, → open thread/activate, ← folders, Enter
  activate + triage keys m/u/f/d/v/a/l/L/t/c/r/R/F/A/D/T/O), **Reading** (↑/↓
  scroll, PgUp/PgDn prev/next msg, ←/Esc back), **Calendar**, **Event form**,
  **Compose**. Update `status_bar` if it mentions removed keys.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: help overlay + status bar for the arrow model`.

---

## Self-Review

- **Coverage:** rail (T1), left/right + removals (T2), auto-preview (T3),
  reader paging (T4), calendar-via-rail (T5), help/status (T6). Spec's every
  section maps to a task.
- **Type consistency:** `nav_left`/`nav_right`/`preview_selected_message`/
  `open_sibling_message` used consistently; `Pane::Rail` added in T1 and matched
  everywhere `Pane` is matched (compiler backstop will flag non-exhaustive
  matches — fix each).
- **Ordering risk:** T1 adds `Pane::Rail` → every exhaustive `match app.focus`
  (move_selection, rendering, etc.) must gain a `Rail` arm in T1, or it won't
  compile. Handle in T1.
- After each task: `test --workspace`, `clippy --all-targets -D warnings`, `fmt
  --all -- --check`. Final whole-branch review (direct), then push to PR #21.
