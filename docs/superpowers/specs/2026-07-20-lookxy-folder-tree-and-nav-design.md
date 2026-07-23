# lookxy — Collapsible Folder Tree + Pane Navigation & Help

**Date:** 2026-07-20
**Status:** design approved (tree = collapsible; remaining defaults below)

## Goal

Two cohesive folder-pane / navigation improvements, driven by live-mailbox use:

1. **Pane navigation & help** — you can currently only cycle panes *forward*
   (`Tab`: Folders → List → Reading → Folders). After `Enter` opens a message,
   the only way back to the list is `Tab` twice. Add reverse navigation and an
   F1/`?` help overlay so the shortcuts are discoverable.
2. **Collapsible folder tree** — the folder pane renders every folder as one
   flat alphabetical list, collapsing the real hierarchy (Inbox › EPAM › ADPT…).
   mailcore already fetches the full tree with each folder's `parent_id`; render
   it as a collapsible tree matching Outlook.

Built as **two features**, navigation/help first (it's blocking), then the tree.

## Global constraints

- Rust 2024, MSRV 1.88; crates `mailcore` (headless) + `lookxy` (ratatui/crossterm).
- Optimistic-local + outbox unchanged. No new dependencies.
- Full workspace green, `clippy --all-targets -D warnings` clean, `fmt` clean.
- TDD, task-by-task, each committed.

---

## Feature A — Pane navigation & help overlay

### A1. Reverse pane navigation

Focus is `Pane::{Folders, List, Reading}`. Add:

- **`Shift+Tab` (`KeyCode::BackTab`)** — reverse-cycle: Reading → List →
  Folders → Reading. Mirror of `Tab`. Global (Mail mode), highest-precedence
  among the pane keys.
- **`←` / `h`** — step focus toward the left pane: Reading → List, List →
  Folders. In the **Folders** pane `←`/`h` instead drives tree collapse (see
  B3), so this arm is pane-specific.
- **`Esc`** — when focus is Reading or List and nothing else claims Esc (no
  reminder queued, no category filter, no capturing text field), step focus back
  one pane (Reading → List → Folders). Checked *after* the existing Esc handlers
  (reminder dismiss, category-filter clear) so it never steals from them.
- `Tab`, `→`/`l`, and `Enter` keep their current forward behavior.

`→`/`l` in the List/Reading panes continue to do nothing new (kept simple);
forward movement stays on `Tab`/`Enter`.

### A2. Help overlay

- **`F1`** and **`?`** open a modal help overlay (`App::help: bool`, or an
  `Option` if state is needed). Read-only. **Esc**, **F1**, **`?`**, or **`q`**
  close it. While open it swallows all other keys (routed first in
  `ui::handle_key`, like other modals) and counts as capturing for the global
  `q`-quit guard so `q` closes it rather than quitting the app.
- `?` must not open help while a text field is capturing (search query, compose,
  RSVP comment, event-form/OOF fields) — there `?` is a literal character. Gate
  the `?` opener on `!is_capturing_text()`; `F1` is unambiguous and always opens.
- Content: a static, grouped cheat-sheet rendered in a centered bordered panel,
  scrollable if it exceeds the height. Groups and keys (authoritative list drawn
  from the current bindings):
  - **Global:** `Tab`/`Shift+Tab` cycle panes · `←`/`h`/`Esc` back · `g` toggle
    Mail/Calendar · `/` search · `F1`/`?` help · `q` quit
  - **Folders:** `j`/`k` move · `→`/`l` expand · `←`/`h` collapse · `Space`
    toggle · `Enter` open folder
  - **Message list:** `j`/`k` move · `Enter` open · `m`/`u` read/unread · `f`
    flag · `d`/`Del` delete · `v` move · `a` attachments · `l`/`L` categorize/
    filter · `t` threaded · `c` compose · `r`/`R` reply/all · `F` forward ·
    `A`/`D`/`T` RSVP · `O` out-of-office
  - **Reading:** `j`/`k` scroll · `PgUp`/`PgDn` · `Home`/`End` · `Esc`/`←` back
  - **Calendar:** `j`/`k` move · `c` new · `e` edit · `x` delete · `a`/`d`/`t`
    RSVP · `O` out-of-office · `g`/`Esc` back to Mail
  - **Event form:** `Tab` next field · `Space` all-day · `Ctrl-B` free/busy ·
    `Ctrl-Enter` save · `Esc` cancel
  - **Compose:** `Ctrl-Enter` send · `Esc` cancel

The help text is a hand-maintained constant in the help module — accepted
duplication of the keymap (a TUI cheat sheet is worth the double-entry).

---

## Feature B — Collapsible folder tree

### B1. Data model / persistence

- Add `is_expanded INTEGER NOT NULL DEFAULT 0` to the `folders` table via an
  idempotent migration (`let _ = conn.execute("ALTER TABLE folders ADD COLUMN
  is_expanded ...", [])`, swallowing the duplicate-column error).
- `upsert_folder` **must not** reference `is_expanded` — so a re-sync's
  `ON CONFLICT DO UPDATE` leaves a user's expand/collapse choices intact.
- `FolderRow` gains `is_expanded: bool`; `folders()` selects it.
- `Store::set_folder_expanded(id: &str, expanded: bool)` writes the single
  column.
- **First-run default:** Inbox expanded. Track a one-time init via a config
  flag `folder_tree_initialized` (mirrors the `threaded`/`reminders_notify`
  persistence in `lookxy/src/config.rs`): on the first tree load where the flag
  is false, call `set_folder_expanded(inbox_id, true)`, then persist the flag
  true. Everything else starts collapsed.

### B2. Tree building (lookxy)

Convert the flat `Vec<FolderRow>` (already ordered: well-known rank, then
`display_name`) into a display list:

- A folder is **top-level** if its `parent_id` is `None` or not the id of any
  folder in the set (Graph's `msgfolderroot` is not one of our rows).
- Group children by `parent_id`; **sibling order = the store order** (well-known
  rank at the top level, `display_name` within a parent — the existing `folders()`
  ORDER BY already yields this per level once grouped).
- Depth-first flatten: each folder immediately followed by its children, but a
  child is **visible only if every ancestor is expanded**.
- Produce `Vec<VisibleFolder { row: FolderRow, depth: usize, has_children: bool,
  expanded: bool }>` — the visible rows in render order.

`App` holds the full `folders: Vec<FolderRow>` (unchanged) plus a derived
`visible_folders: Vec<VisibleFolder>` rebuilt by `reload_folders` and after any
expand/collapse toggle. `folder_index` indexes **`visible_folders`**.
`selected_folder` stays an id; after a rebuild, clamp `folder_index` and keep it
pointing at `selected_folder` when that row is still visible (if a collapse hid
it, move selection to the nearest visible ancestor).

### B3. Folder-pane keys

Focus == Folders:

- `j`/`k` / `↑`/`↓` — move among **visible** rows (`move_selection` uses
  `visible_folders`).
- `→` / `l` — if the row has children and is collapsed, **expand**; else no-op.
- `←` / `h` — if the row has children and is expanded, **collapse**; else move
  selection to the row's **parent** (Outlook behavior). If already top-level,
  no-op.
- `Space` — toggle expand/collapse on a row with children (no-op on a leaf).
- `Enter` — unchanged: select the folder and move focus to List (loads its
  messages).

Every expand/collapse calls `set_folder_expanded` and rebuilds `visible_folders`.

### B4. Rendering (`lookxy/src/ui/folders.rs`)

Each visible row: `"{indent}{chevron} {name}{count}"`

- `indent` = `"  "` × `depth`.
- `chevron` = `▾` expanded, `▸` collapsed, `" "` (space) for a leaf — so names
  align whether or not a folder has children.
- `name` + `count` as today (`" (N)"` when `unread_count > 0`).

Highlight/selection styling unchanged; `ListState` selects `folder_index`.

---

## Testing

**mailcore**
- Migration idempotent; `is_expanded` round-trips; `upsert_folder` re-sync
  preserves an expanded folder's flag; `set_folder_expanded` toggles.

**lookxy**
- Tree build: parents precede indented children; correct `depth`/`has_children`;
  collapsed parent hides its subtree; top-level detection when `parent_id`
  points outside the set.
- Selection: collapsing the parent of the selected folder moves selection to the
  parent and keeps `folder_index` in range.
- Keys: `→`/`l` expands, `←`/`h` collapses then jumps to parent, `Space`
  toggles, `j`/`k` skip hidden rows.
- Nav: `Shift+Tab` reverse-cycles; `←`/`h`/`Esc` step focus back from Reading
  and List; Esc still dismisses a reminder / clears a category filter first.
- Help: `F1` opens; `?` opens only when not capturing text; Esc/`q` close; while
  open `q` does not quit; a render test asserts a couple of group headers show.
- First-run: Inbox expanded once, flag persisted, honored (not re-expanded)
  after the user collapses it.

## Out of scope

- Drag/reorder, create/rename/delete folders, multi-select.
- Remembering expand state *per account* (single account today).
- Horizontal scroll for very deep indentation (deep trees just indent; the pane
  is wide enough in practice).
