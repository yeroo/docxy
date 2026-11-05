# Mail

Notes moved from `CLAUDE.md` (#1903). Read them before changing `mailcore` and Mail drafts and bodies. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Mail drafts keep their `local:` id (#1884, #1881)

- `Store::reconcile_id` keeps a draft's `local:` id on its row as
  `messages.local_id` (migrated in `Store::init` like the other columns, its
  unique index created after the ALTER), so a caller still holding it (a
  compose window left open across `SaveDraft`, a queued `SendDraft` retried
  after a 503) keeps reaching the draft now stored under its Graph id. The
  alias is on the row, so it goes with the message on every delete path
  (`delete_message`, a delta `@removed`, a folder cascade); a delta upsert
  keeps it.
- Every draft call resolves inside its one statement (atomic against the
  sync thread's `reconcile_id` on its own connection): `draft` (the row's
  `id` is the current id), `update_draft_fields`, `mark_sent`,
  `move_message`, `delete_message` (`draft_row_sql!`), and the
  `outbound_attachments` calls (`current_id_sql!`). Only a `local:` id is
  looked up; `Store::current_id` is the same rule on its own.
- `ensure_draft_on_graph` decides create vs update by the row's current id,
  so a second `SaveDraft`/`SendDraft` with a reconciled `local:` id is a
  `PATCH`, never a second create. Their payloads and `Sent { id }` keep the id
  the caller used. A local delete that also sends `SyncCommand::Delete` goes
  through `Store::delete_message_resolved` (one `DELETE … RETURNING id`) and
  sends the id it returns, else the id it had: the engine's `Delete` arm, and
  lookxy's list deletes (`delete_and_send`) and `mail.delete`, as the list can
  still hold a `local:` id the sync thread reconciled. `OutboxOp::Delete` of
  an id that still resolves to `local:` is treated as never having reached
  Graph and calls nothing; a delete that races the engine's create leaves the
  Graph draft for a later sync. `Move`, `MarkRead`, `SetFlag` and
  `SetCategories` with a `local:` id still reach Graph with it.
- `FakeServer::start_limited` gives a route a use limit; a used-up route is
  skipped for the next match (a send that fails once, then succeeds).

## Mail body format (#1901)

- `engine::EngineOptions { body_format }` (`graph::model::BodyFormat`,
  default `Text`, re-exported from `sync::engine`) is chosen at spawn:
  `spawn` runs the default, `spawn_with_options` takes one. `Text` asks
  Graph for plain text (`Prefer: outlook.body-content-type="text"`, lookxy
  unchanged); `Html` sends no `Prefer`, so a body comes back native (html,
  or text for a plain-text message).
- `bodies.fetched_as` records the format `FetchBody` *asked for*
  (`Store::put_fetched_body`), not the content type that came back. NULL is
  a body not fetched by `FetchBody`: `put_body` (a `createReply` draft),
  `create_local_draft`, `update_draft_fields` (which also makes the body
  html, as compose writes html), or a row from before the column.
  `Store::get_body_for(id, format)` is the body when it satisfies the format:
  a recorded format must equal it; NULL satisfies both when the body is
  html or a draft's, else counts as asked for text (the old engine always
  did). A draft is `is_draft` or filed in the Drafts folder (synced or the
  local sentinel): a complete delta item resets `is_draft`, as `isDraft` is
  not in `MESSAGE_SELECT`. So a draft's local body (a plain-text
  `createReply`, or html an older `update_draft_fields` left under
  `content_type = 'text'`) is never refetched over unsynced edits, a store
  filled as text upgrades one message per `FetchBody`, and a native text
  answer to `Html` is not refetched. `FetchBody` itself always goes to
  Graph: the caller gates on `get_body_for` (`get_body` still shows
  whatever is stored).
- Not done: FTS indexes an html body's markup (the triggers copy `content`).

## Sync one folder, the fast cycle, Sent Items after a send (#1885)

- `SyncCommand::SyncFolder { id }` (Shift+F9, "Update Folder") drains the
  outbox, runs that folder's delta and re-reads its counts
  (`GraphClient::get_folder_counts`: `GET /me/mailFolders/{id}?$select=…`,
  stored by `Store::set_folder_counts`, which leaves name, parent and
  well-known name alone), with `FoldersUpdated` only when the counts
  changed (`Engine::refresh_folder`). An id not in the store is an `Error`
  with no Graph call; signed out or inside `next_retry` it does nothing. A
  folder that never synced runs its initial delta (with the cutoff): the user
  asked for it.
- The fast cycle is opt-in: `EngineOptions::fast_tick` (`None` or zero: the
  old single tick, whose wait restarts on every command). With it the engine
  keeps two deadlines, `next_full` (every `tick`) and `next_fast`; commands do
  not move them, `Refresh` resets both. The normal pass runs when its
  deadline passed or `needs_full_pass()` (no enumeration yet, or no folders:
  a quarantine or an offline start recovers at the next fast deadline). When
  the last pass ran to its end and its folder list still failed non-fatally
  (a 403, a 5xx: no back-off), `hold_full` keeps the fast deadlines from
  repeating it before `next_full`, unless a quarantine (`reconverge_pending`)
  came since; otherwise `on_fast_tick` drains the outbox and refreshes the
  `SetFastFolders` folders that already have a delta link (never a backfill,
  #1871), picked after the drain. Nothing runs inside `next_retry`; an
  aborted normal pass is due again at its deadline (`after_full_pass`). A
  fast tick sets no `Syncing` state; a complete one settles, and one cut
  short (like a `SyncFolder` cut short) keeps the state its failure set, so
  Offline is not settled away. `SetFastFolders` without `fast_tick` is kept
  and does nothing.
- Without `fast_tick`, a throttled `SyncFolder` makes the deadline run a full
  pass (`recv_timeout` waits for `next_retry`, then `on_tick`), as any
  throttled command does.
- `drain_outbox` returns a `Drain`: `Aborted` when cut short (auth,
  throttle, transport, or the post-send sync stopping), and every sync
  (`sync_pass`, a fast tick, `SyncFolder`) then stops and keeps the back-off
  (`sync_pass` used to `clear_backoff` right after a failed drain). Its
  throttle arm settles the state, as `react` does. A 5xx op is never
  quarantined, so it is `Deferred`, not `Aborted`: the op keeps its place,
  the drain stops there, and the sync carries on (else one op Graph keeps
  failing would stop the calendar and the fast cycle for good). A command's
  drain (`enqueue_and_drain`) still backs off on `Deferred` (`go_offline`),
  as before; the pass at that deadline carries on past the op and clears it.
- After a drain that sent a draft, `sync_after_send` refreshes Drafts, then
  Sent Items, each only when it has a delta link. Drafts first: without
  `ImmutableId` Graph gives the sent message a new id in Sent Items, and the
  Drafts delta's `@removed` of the draft's id removes the row `SendDraft`
  re-filed under Sent, so Sent never shows it twice. Graph files the Sent
  copy asynchronously, so it may only come with a later delta.
