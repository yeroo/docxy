# lookxy — Spatial Arrow Navigation Model

**Date:** 2026-07-22
**Status:** design (Phase 2 of the UI/UX pass; Phase 1 shipped)

## Goal

Replace the pane-cycling + Enter-to-open model with a **spatial, arrow-driven**
one. Left/Right move between hierarchy levels, Up/Down move within a level, and
the message under the cursor **opens automatically** (preview) as you arrow
through the list. A new **level-0 rail** (✉ Mail / 📅 Calendar) becomes the only
way to switch sections. `g`, `Tab`, and `Shift+Tab` are **removed**; `Enter`
stays as an activate accelerator.

## Global constraints

- Rust 2024, MSRV 1.88; `lookxy` (TUI) only — no `mailcore` changes expected.
- Full workspace green, `clippy --all-targets -D warnings` clean, `fmt` clean.
- TDD, task-by-task, each committed. No new dependencies.
- Links (Ctrl+Up/Down, status-bar URL, open-with-warning) are **Phase 3**, out
  of scope here.

## The level model

`Pane` gains a `Rail` variant: `Pane::{Rail, Folders, List, Reading}`. `Mode`
(`Mail`/`Calendar`) still exists but is now driven only by the rail. Left = out
a level, Right = in a level, Up/Down = move within.

| Focus | Up / Down | Right (→) | Left (←) | Enter |
|---|---|---|---|---|
| **Rail** | switch Mail⇄Calendar (section updates live) | enter the section (Folders, or Agenda in Calendar) | — | enter the section |
| **Folders** | move folder selection (loads that folder's list) | collapsed+children → expand & select first child; else → enter **List** | expanded → collapse; else parent; else top-level → **Rail** | enter **List** |
| **List** | move selection — **message auto-previews**, cursor stays | folded thread → expand & select first child; on a message → **activate** (mark read, focus Reading) | → **Folders** | activate |
| **Reading** (active) | **scroll body** | — | → **List** (Esc too) | — |

- **Reading active:** `PgUp`/`PgDn` = previous/next message (moves the list
  selection and opens it, staying in Reading); `Home`/`End` = top/bottom of the
  body; `Ctrl+Up`/`Ctrl+Down` reserved for Phase 3 link-jump.
- `j`/`k` remain aliases for Down/Up everywhere they work today (harmless vim
  accelerators — only `g`/`Tab`/`Shift+Tab` are being removed).

## Rail (level 0)

- A new always-visible leftmost column, width **5**. Two rows: `✉` (Mail) and
  `📅` (Calendar), each on its own line, the active section highlighted
  (reverse/blue). A terminal that can't render the emoji still shows the
  highlight and the section is unambiguous from the panes to its right; if this
  proves unreadable in testing, fall back to `M`/`C` letters.
- `Pane::Rail` Up/Down flip `mode` **immediately** (so the panes to the right
  switch section live — consistent with list auto-preview). Right/Enter move
  focus into the section (`Folders` for Mail, `Reading`-equivalent agenda focus
  for Calendar — see below). There is nothing to the left of the rail.
- Layout becomes: `Rail(5) | Folders | List | Reading` in Mail mode, and
  `Rail(5) | Agenda` in Calendar mode (event form / overlays unchanged).
- On startup focus begins at `Folders` (unchanged first impression), not the
  rail.

## Folders (level 1)

Extend the existing folder-tree keys:

- **Right / `l`:** if the selected folder has children and is collapsed, expand
  it and move the selection to its **first child** (mirrors the thread-strip
  rule); otherwise (leaf, or already expanded) move focus to **List** and
  auto-preview its first message.
- **Left / `h`:** expanded → collapse; else has-parent → move to parent; else
  (top-level) → focus **Rail**.
- Up/Down unchanged (move folder selection, load that folder's message list).
- **Enter:** enter **List** (same as Right into the list).

## List (level 2) — auto-preview

- Moving the selection (Up/Down, and on entry from Folders) **auto-previews**
  the message under the cursor: load its body into the reading pane via the
  existing `open_message` (which does **not** mark read), leaving focus on the
  List. Preview never marks a message read — so unread bold survives arrowing.
  - Threaded mode: previewing a `Row::Message` shows that message; previewing a
    collapsed `Row::Header` shows the thread's **latest** message.
- **Right / `l`:** on a collapsed thread `Row::Header`, expand it and select its
  first child (stay in List); on a message row, **activate** — mark it read and
  move focus to **Reading**.
- **Left / `h`:** → **Folders**.
- **Enter:** activate the current row (existing `activate`/`activate_thread_row`
  path), which now also marks the opened message read.
- Marking read on activate: activate calls `mark_read(true)` for the opened
  message id (threaded: the specific message, or the thread's latest for a
  header) in addition to opening it.

## Reading (level 3) — active message

- Focus `Reading`. Up/Down scroll the body (existing `reading_scroll_by`);
  `Home`/`End` jump to top/bottom.
- **PgUp / PgDn:** previous / next message. Moves the List selection by one
  selectable message (flat: prev/next `messages` row; threaded: prev/next
  `Row::Message` in `visible_rows`, skipping headers), opens it, marks it read,
  and keeps focus in Reading. No-op at the ends.
- **Left / Esc:** back to **List** (focus List, message stays open behind).

## Calendar via the rail

- `g` is removed; Calendar is reached only by selecting 📅 on the rail. Agenda
  navigation (`j`/`k`/↑/↓, `c`/`e`/`x`, `a`/`d`/`t`, `O`) is unchanged.
- **Left / Esc** from the agenda focuses the **Rail** (replacing the old
  `g`/`Esc`→Mail). **Right / Enter** on an event opens the edit form (same as
  `e`) — a natural "descend into the event".
- The event form, RSVP prompt, free/busy, and OOF overlays keep first crack at
  keys exactly as today.

## Removals & key cleanups

- Delete `cycle_focus` (Tab) and `cycle_focus_back` (Shift+Tab) and their arms;
  remove the `KeyCode::Char('g') => toggle_mode` arm and the calendar
  `Esc/g → toggle_mode`. `toggle_mode` itself stays (the rail calls it) but is
  no longer bound to `g`.
- The A1 tests `shift_tab_reverse_cycles_panes` and
  `left_and_esc_step_focus_back_from_reading_and_list` are replaced by the new
  navigation tests below.
- `focus_back` is superseded by the richer `nav_left`.

## Handler shape

Introduce four intent functions in `ui/mod.rs`, dispatched from `handle_key`
for the bare arrows (and `h`/`j`/`k`/`l` aliases):

- `nav_up(app)` / `nav_down(app)` — within-level move (rail mode-switch;
  folder move; list move + auto-preview; reading scroll).
- `nav_right(app)` — descend/expand/activate per the table.
- `nav_left(app)` — ascend/collapse per the table.

`PgUp`/`PgDn` route to `reading_page_or_sibling` (scroll page vs prev/next
message by focus). Keep `Enter` → `activate` (extended to mark read).

## Help overlay & status bar

- Rewrite `ui/help.rs`'s groups to the new model (Rail / Folders / List /
  Reading, with Left/Right/Up/Down and the PgUp/PgDn + auto-preview notes).
- Update any status-bar hint text that referenced `Tab`/`g`.

## Testing

- **Rail:** Up/Down flips `mode`; Right from rail enters Folders (Mail) / agenda
  (Calendar); Left from top-level folder returns to Rail.
- **Folders:** Right on a collapsed parent expands + selects first child; Right
  on a leaf enters List and previews; Left collapses / goes to parent / to rail.
- **List auto-preview:** moving selection sets `selected_msg` (body loaded) with
  focus still `List` and the message **still unread**; Right/Enter on a message
  marks it read and focuses Reading.
- **Reading:** Up/Down scroll; PgDn selects and opens the next message (marks
  read) staying in Reading; PgUp at the top message is a no-op; Left/Esc → List.
- **Threaded:** Right on a collapsed header expands + selects first child;
  activating a message marks that message read.
- **Removals:** `g`, `Tab`, `BackTab` no longer change focus/mode.
- **Calendar:** rail selects it; Left from agenda → rail.
- **Help:** overlay lists the new Rail/arrow bindings.

## Out of scope (Phase 3)

- Ctrl+Up/Down link jump, status-bar full-URL display, Enter-opens-with-warning,
  inline blue link rendering. Designed separately once this lands.
