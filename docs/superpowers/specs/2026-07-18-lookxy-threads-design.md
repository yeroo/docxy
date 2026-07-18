# lookxy conversation/thread view — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-18.
**Builds on:** lookxy v1 (mailcore store + sync engine, ratatui TUI), v2 (compose, calendar).

## Goal

Group the folder message-list into collapsible **conversations**, so a user sees
one row per thread (with a message count and participants) instead of one row per
message. Grouping is by the `conversation_id` mailcore already stores on every
`MessageRow`.

## Product decisions (locked)

1. **Cross-folder conversations.** A conversation shown in a folder view contains
   *all* of its messages across every folder (Inbox + Sent + Archive + …),
   Gmail-style — not just the ones in the selected folder.
2. **Toggle key, default ON.** The folder view is threaded by default; a key (`t`)
   flips to the classic flat list. The choice is persisted in config.
3. **Whole-conversation triage, with confirm on destructive actions.** On a
   collapsed thread header, `m`/`!` (mark-read/flag) apply to the whole
   conversation immediately; `d`/`v` (delete/move) apply to the whole conversation
   but only after a confirmation modal that surfaces how many messages — and
   whether any are in Sent — will be affected.

## Feasibility note

The sync engine (`mailcore::sync::engine::sync_pass`) already re-enumerates every
folder and delta-syncs each one. So messages from Sent/Archive are **already in the
local store**, each with its `folder_id` and `conversation_id`. Cross-folder
conversations are therefore a **store-query + UI** problem: no sync expansion, no
new Graph backfill.

## Architecture

Grouping strategy: **eager in-memory grouping**. One store query loads all messages
of the conversations visible in the folder; a pure Rust function groups them into
`Thread`s held in memory. Rationale: whole-conversation triage needs every message
id anyway, real threads are small (handfuls), and holding them in memory makes
navigation and actions trivial. The alternative — lazy SQL aggregation (a header
row via `GROUP BY`, children fetched on expand) — scales to very large mailboxes
but doubles the query paths; it is the documented fallback if performance ever
demands it, not built now.

Responsibilities split cleanly:

- **mailcore/store** decides *which* conversations appear and returns their rows.
- **mailcore/thread** (new, pure) turns rows into `Thread`s — no DB, fully unit-testable.
- **lookxy/app** owns threaded-vs-flat mode, expansion state, the visible-row
  projection, the cursor, and triage dispatch.
- **lookxy/ui/message_list** renders the threaded view; the flat/search path is unchanged.

## Components

### 1. Store — `conversations_in_folder`

New method on `Store` (in `mailcore/src/store/mod.rs`):

```rust
pub fn conversations_in_folder(
    &self,
    folder_id: &str,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<MessageRow>>
```

Semantics:

- The **grouping key** of a message is `conversation_id` when non-empty, else
  `msg:<id>` — so a message with a blank `conversation_id` (drafts, or a message
  Graph gave no conversation for) becomes its own singleton thread rather than
  being lumped with every other blank-conversation message.
- A conversation is **eligible** for this folder view if it has ≥1 message whose
  `folder_id = ?folder_id`.
- Of the eligible conversations, take the `limit` most-recent (by the
  conversation's newest `received_at`), applying `offset`.
- Return **all** messages (across all folders) belonging to those conversations.
- Order: `(conversation latest received_at DESC, message received_at ASC)` — active
  threads on top, and each thread's messages oldest→newest for top-to-bottom reading.

Returns the same `MessageRow` shape (which already includes `conversation_id` and
`folder_id`) that `messages_in_folder` returns, so the row renderer and triage code
reuse the existing type.

### 2. Grouping model — `mailcore/src/thread.rs` (new)

Pure, DB-free, unit-testable:

```rust
pub struct Thread {
    pub key: String,                 // grouping key (conversation_id or "msg:<id>")
    pub messages: Vec<MessageRow>,   // oldest→newest, as returned by the store
    pub latest_received: String,     // max received_at across messages
    pub unread_count: usize,         // messages with is_read == false
    pub any_flagged: bool,
    pub any_attachments: bool,
    pub subject: String,             // latest non-empty subject (fallback: "")
    pub participants: Vec<String>,   // unique from_name, first-seen order
}

pub fn build_threads(rows: &[MessageRow]) -> Vec<Thread>;
```

`build_threads` groups consecutive-or-not rows by grouping key (the store already
orders them so a thread's rows are contiguous, but the function must not rely on
that — it groups by key regardless), preserving the store's ordering of threads and
of messages within a thread, and derives the aggregate fields.

### 3. App state — `lookxy/src/app.rs`

New fields:

```rust
pub threaded: bool,                 // from config; default true
pub threads: Vec<ThreadView>,       // built when threaded == true
pub visible_rows: Vec<Row>,         // flattened projection for render + navigation
pub row_index: usize,               // cursor into visible_rows (threaded mode)
// msg_index continues to drive the flat list; row_index drives threaded mode
```

```rust
pub struct ThreadView {
    pub thread: mailcore::thread::Thread,
    pub expanded: bool,
}

pub enum Row {
    Header(usize),          // index into threads
    Message(usize, usize),  // (thread index, message index within the thread)
}
```

Behavior:

- A thread with **exactly one message** renders as a plain message row (no chevron,
  no header/child distinction); its `Row` is a single `Message(t, 0)` and actions
  target that message directly. Keeps the common single-message case clean.
- `visible_rows` is rebuilt whenever the folder changes, the `t` toggle flips,
  a thread is expanded/collapsed, or triage changes the set — and the rebuild
  tries to keep the cursor on the same conversation (falling back to a clamped
  index) so the selection does not jump.
- When `threaded == false`, the folder view uses the existing flat
  `messages`/`msg_index` path verbatim.

The threaded rows come from `store.conversations_in_folder(...)` →
`thread::build_threads(...)`; the flat rows continue to come from
`store.messages_in_folder(...)`. `reload_messages` gains a threaded branch that
populates `threads`/`visible_rows` instead of the flat list.

### 4. Rendering — `lookxy/src/ui/message_list.rs`

- **Flat/search path unchanged.** `draw_list` (shared with `ui::search`) keeps
  rendering a `&[MessageRow]`.
- **Threaded path** (new function) renders `visible_rows`:
  - **Header row:** `▾`/`▸` chevron, `(N)` count, participant names, the thread
    subject, and the latest short time; aggregate markers — bold when
    `unread_count > 0`, `!` when `any_flagged`, `@` when `any_attachments`.
  - **Child message row** (only under an expanded header): indented, showing
    sender — subject/preview and its own time, bold if that message is unread.
  - Selection highlight uses the same style as the flat list, applied to whichever
    `visible_rows` entry the cursor is on.

### 5. Navigation & interaction

- **Up/Down** move the cursor over `visible_rows` (clamped, matching the flat
  list's non-wrapping behavior).
- **On a header:** Enter or `→`/`l` expands it and opens the thread's latest
  message in the reading pane; Enter or `←`/`h` on an expanded header collapses it.
- **On a message row:** Enter opens that message in the reading pane (as today).
- **Triage:**
  - On a **collapsed header** (multi-message thread): `m`/`!` apply to *every*
    message in the conversation immediately (optimistic store write + one
    `SyncCommand` enqueued per message, the same path a single keypress uses).
    `d`/`v` first open a **confirm modal** — e.g. `Delete 5 messages (incl. 2 in
    Sent)?` / `Move 5 messages (incl. 2 in Sent) to …?` — and only enqueue the
    per-message ops on confirmation.
  - On an **expanded message row** (or a singleton row): every key acts on that one
    message exactly as it does in the flat list today.

### 6. Config + toggle

- `lookxy/src/config.rs`: add a persisted `threaded: bool`, default `true`.
- A toggle key flips `app.threaded`, rebuilds the view, and writes the config back.
  `t` is the intended binding; the plan must confirm it is not already bound in
  `on_key_char` and pick another otherwise-unused key if it is.

## Scope boundaries (YAGNI)

- **Search results stay flat.** Threading is folder-view only; the search view
  keeps using the shared flat `draw_list`.
- **Control/MCP surface stays message-oriented.** `mail.list` and the triage verbs
  remain per-message; a `mail.threads` verb is a possible later addition, not part
  of this work.
- **Pagination is by conversation** with a generous page size; finer conversation
  paging (and the lazy-aggregation strategy) is a later refinement.
- **No new sync work.** Cross-folder data is already synced.

## Error handling & edge cases

- **Blank `conversation_id`** → singleton thread keyed `msg:<id>` (never merged with
  other blanks).
- **Count-1 conversation** → plain message row, no expand affordance.
- **Empty folder / empty thread list** → same empty-state behavior as the flat list;
  the cursor stays unset (no panic), matching the existing bounds-safe rendering.
- **Cursor stability** across rebuilds (toggle, triage, expand) → re-anchor to the
  same conversation key when present, else clamp into range.
- **Whole-thread action partial failure** → each per-message op flows through the
  existing outbox/quarantine/retry machinery independently; one message failing to
  send does not block the others, exactly as today.

## Testing

**mailcore (unit):**

- `conversations_in_folder`: on a seeded store with the same `conversation_id`
  split across Inbox + Sent, the Inbox view returns the Sent messages too; folder
  filter excludes conversations with no message in the folder; ordering is
  (latest-conversation-first, oldest-message-first); blank-`conversation_id`
  messages come back as singletons; `limit`/`offset` page by conversation.
- `build_threads`: counts, `unread_count`, `any_flagged`, `any_attachments`,
  `participants` (unique, ordered), `subject` (latest non-empty), and the
  singleton case.

**lookxy (unit + render):**

- `visible_rows` projection: collapsed vs expanded, singleton renders as a bare
  message row.
- Navigation over mixed header/message rows (clamped, no panic on empty).
- Whole-thread `m`/`!` enqueue one `SyncCommand` per message and update the store
  optimistically; `d`/`v` are gated behind the confirm modal and enqueue per-message
  ops only after confirmation.
- `t` toggles threaded/flat and persists to config.
- Threaded-list render snapshot (`TestBackend`): header shows chevron, count,
  participants, subject; expanded shows indented children.
