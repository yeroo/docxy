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
  not move them, except `Refresh` and `SignIn`, whose full pass reschedules
  both. The normal pass runs when its
  deadline passed or `needs_full_pass()` (no enumeration yet, or no folders:
  a quarantine or an offline start recovers at the next fast deadline). When
  the last pass ran to its end and its folder list still failed non-fatally
  (a 403, a 5xx: no back-off), `hold_full` keeps the fast deadlines from
  repeating it before `next_full`, unless a quarantine (`reconverge_pending`)
  came since; otherwise `on_fast_tick` drains the outbox and refreshes the
  `SetFastFolders` folders that already have a delta link (never a backfill,
  #1871), picked after the drain. Nothing runs inside `next_retry`; an
  aborted normal pass is due again at its deadline. `after_full_pass` (both
  deadlines, `hold_full`) follows every full pass: a normal one, and
  through `after_pass_if_fast` the startup pass, `Refresh` and a sign-in, so
  a throttled startup or sign-in pass is retried at its deadline too. A
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
  failing would stop the calendar and the fast cycle for good). The outbox
  has its own back-off for it (`op_retry`/`op_backoff`, 5 s doubling to
  300 s, cleared when an op goes through or the queue is empty): until then
  every drain, a command's included, sends nothing and is `Deferred`, so the
  op is not resent at every fast tick and no engine-wide Offline is set.
  Without `fast_tick`, `recv_timeout` also wakes at a future `op_retry`
  (signed in only), so the tick there resends it as a back-off deadline
  did; `Refresh` clears `op_retry` (not `op_backoff`) to resend at once.
- After a drain that sent a draft, `sync_after_send` refreshes Drafts, then
  Sent Items, each only when it has a delta link. Drafts first: without
  `ImmutableId` Graph gives the sent message a new id in Sent Items, and the
  Drafts delta's `@removed` of the draft's id removes the row `SendDraft`
  re-filed under Sent, so Sent never shows it twice. Graph moves the draft
  and files the Sent copy asynchronously, so either may only come with a
  later delta: a Drafts delta that still reports a sent draft (an upsert,
  which re-files the row under Drafts) is undone by re-filing the ids this
  drain sent under Sent Items again, until the `@removed` deletes the row.

## Folders and counts on every tick (#1886)

- Every sync pass lists the folder tree (`sync_pass(full)` always calls
  `sync_folders`). `full` (startup, `Refresh`, sign-in, a pass that
  `needs_full_pass`) only adds people and master categories, and a
  `FoldersUpdated` after every successful list. A normal tick emits
  `FoldersUpdated` only when a folder row was added, changed
  (`Store::upsert_folder` returns whether it did) or removed. Fast ticks
  never list. Names, parents, new folders and counts follow the server
  within one tick. A count changed by an op this pass drains shows a tick
  later: the list runs before the drain.
- Why not `GET /me/mailFolders/delta`: it would need its own delta link,
  410 reset and quarantine handling, and it is not clear that it reports
  a count change. `list_folders` stays cheap instead. It skips a folder's
  `childFolders` call when the listing says `childFolderCount: 0` (asked
  when the property is absent). A new child bumps its parent's count in the
  same listing, so the child is still found in one tick. Every list
  (top level and `childFolders`) follows `@odata.nextLink`.
- `list_folders` returns a `FolderListing { folders, complete }`. An error
  on a top-level page propagates (nothing is upserted). `Unauthorized` and
  `Throttled` on a `childFolders` page propagate too. Any other
  `childFolders` error (a 404 for a folder deleted between the two calls,
  a 5xx) keeps what was listed and sets `complete = false`, as does a
  listed folder without an id (dropped, never upserted as `""`).
- The walk is many requests, not a snapshot. A folder moved during it (into
  a parent already read, e.g. a soft delete into Deleted Items, or past an
  offset `nextLink`) is in no page, yet the listing says complete. So after
  a complete, non-empty listing, each stored folder missing from it (but
  never the drafts sentinel) is first asked for
  (`GraphClient::folder_exists`: `GET /me/mailFolders/{id}?$select=id`,
  `Engine::folders_to_keep`). Only a 404 lets it go. One that exists, or
  whose GET fails non-fatally, is kept for the next tick to list. A fatal
  failure (auth, throttle, transport) keeps every unconfirmed folder and
  stops the pass. A child of a deleted parent gets its own 404. This costs
  one GET per missing folder, only when one is missing.
- Then `Store::retain_folders` removes every folder not kept, in one
  IMMEDIATE transaction. Messages go
  by `ON DELETE CASCADE` (bodies, attachments, FTS rows), and delta links
  go with the row. Removal runs before the pass's deltas, so a deleted
  folder never gets a delta (no 404 `Error`). An incomplete or empty
  listing removes nothing and waits for the next tick. `sort_order`,
  `is_expanded` and `delta_link` are never written by `upsert_folder`, so
  they survive a rename or move. Deleting a folder in Outlook or OWA moves
  it under Deleted Items (same id, a re-parent). Only a hard delete
  removes it.
- The drafts sentinel (`local:drafts-pending`) is never listed by Graph.
  It is removed only once it holds no message, by a statement that checks
  that itself. `create_local_draft` resolves its folder (`drafts_folder_id_on`)
  inside its own IMMEDIATE transaction, so `retain_folders` runs wholly
  before it or after it. It never runs between the sentinel's insert and
  the draft's, which would fail the draft's foreign key. The engine's own
  `drafts_folder_id` caller is on the sync thread, so it cannot race the
  removal.
- A non-fatal list failure (a 403, or a 5xx other than 503, on
  `/me/mailFolders`) now surfaces an `Error` on every normal tick through
  `react`. The pass carries on with the stored folders. 429 and 503 are
  throttles: the pass stops and backs off, as auth and transport failures
  abort it.

## A send that fails goes back to Drafts (#1874)

- `SyncCommand::SendDraft` queues the op first (a send that cannot be
  queued is a `SendFailed` and touches nothing), then makes its optimistic
  write (`mark_sent`, and the move into Sent Items once that has synced),
  emits `SyncEvent::Sending { id }` and drains. A send left queued by an
  earlier run gets no `Sending`.
- The drain applies a `SendDraft` through `outbox::apply_send` with the
  op's `seq`. `outbox.send_posted` (migrated in `Store::init`) is set right
  before the `/send` POST, so a lost answer or a crash counts. Only a
  refusal Graph answered (a 4xx, a 404, a 401) clears it, as nothing went
  out. Any 5xx, a 429/503 throttle (a gateway 503 can follow a send the
  back end accepted) or a dropped connection leaves it set: Graph may
  still have delivered, and the retry costs one GET. On the open that adds
  the column, every `sendDraft` op an older engine left queued is marked
  posted, as it may follow a lost answer; a reopen leaves the flags alone.
  A posted op whose draft is still `local:` was never on Graph: the flag is
  cleared and it is sent with no check.
- The next attempt of a posted op first checks delivery, before any PATCH
  or upload. The local row gone (a Drafts delta's `@removed`), or
  `GraphClient::is_draft` (`GET /me/messages/{id}?$select=isDraft`)
  answering 404 or `false`, means delivered: `Sent`, and the id joins the
  drain's `sent` list for `sync_after_send`. The attachment rows are
  cleared when Graph answered; when the row is gone they stay behind,
  keyed by a Graph id nothing resolves to any more (harmless). `true` clears the flag and
  sends again. A failure of the check is never a failed send: a retryable
  one keeps the op queued, any other keeps the attempts and quarantine
  policy, and the row stays under Sent Items.
- In `drain_outbox`, a 5xx still defers the op, and 401, 429/503 and
  transport still stop the drain. Any other failure of a `SendDraft` whose
  `/send` never went out (a 4xx, a 404, a `Parse`: an attachment file that
  cannot be read, a store error) is final on the first attempt.
  `Engine::fail_send` drops the op, and `Store::return_to_drafts` makes the
  row a draft again in the Drafts folder (or the local sentinel), in one
  `draft_row_sql!` statement. It emits `SendFailed { id, error }` (`id` as
  the command carried it, `error` from `outbox::send_error_text`: Graph's
  `error.message`, else `HTTP {status}` or the failure's own text), then
  `MessagesUpdated` for Drafts and Sent Items, and the drain goes on. There
  is no `Error` and no delta reconverge. A 404 here (the Graph draft is
  gone, the send never posted) is a failed send, and the Drafts delta may
  then remove the reverted row. After a posted send, a 404 means delivered
  (above).
- Once `/send` succeeded nothing may read as a failure: the clear of the
  attachment rows after it ignores a store error.
- `outbound_attachments.uploaded` (migrated in `Store::init`) is set on a
  row right after Graph confirms its upload, and `apply_send` skips a row
  already set. An upload Graph confirmed is never repeated, by a resend
  after a failed send or a retry after a 5xx. An upload whose answer was
  lost (a 5xx or a dropped connection after Graph stored it), or whose
  marker could not be written, is uploaded again. The rows stay until the
  send succeeds, so compose still lists them. lookxy keeps an uploaded row:
  Ctrl+R refuses to remove one (a notice says it is already on the server
  draft), and Discard removes only the rows not uploaded
  (`Store::discard_pending_outbound_attachments`), since removing the row
  would not take the file off the Graph draft.
- lookxy shows a `SendFailed` as `Send failed: <error>` in `App::send_failure`,
  which no sync event clears: the engine settles to `State(Idle)` right after
  it, and that clears `error_notice`. The next key press clears it.

## The signed-in user's profile, photo and Sign out (#1920)

- `GraphClient::me()` is `GET /me?$select=displayName,mail,userPrincipalName`
  (only `User.Read`), giving a `model::Me`. `mail` is `""` when Graph sends
  `null` or leaves it out (personal accounts, some tenants), and
  `Me::address()` then falls back to `user_principal_name`.
- `GraphClient::my_photo()` is `GET /me/photo/$value`, then on a 404
  `/me/photos/48x48/$value`. Both 404 is `Ok(None)` (no photo: the client
  draws initials), not an error. Any other error, on either request, comes
  back as it is, with no fallback. Each request sends `Accept: image/*`, which
  replaces `send`'s JSON `Accept` (ureq's `set` replaces a header of the same
  name). The content type is the `Content-Type` header without its parameters,
  or `application/octet-stream`. A photo over `PHOTO_MAX` (4 MiB) is a `Parse`
  error: refused by `Content-Length` before reading, and `read_capped` reads at
  most `PHOTO_MAX + 1` bytes whatever the header says. The fake server always
  sends a true `Content-Length`, so `read_capped`'s own tests cover the second
  check.
- The engine's `SyncCommand::FetchProfile` / `FetchPhoto` are direct calls like
  `FetchAutomaticReplies`. They emit `ProfileFetched { me }` /
  `PhotoFetched { photo }`, or `Error("not signed in")` with no request, and
  Graph failures go through `with_auth` and `react`.
- `SyncCommand::SignOut` deletes the token file (`tokencache::clear`, which
  also removes the `.tmp` sibling `save` writes, through the shared
  `tmp_path`), clears the throttle/offline back-off (not the outbox's
  `op_retry`), and goes through `enter_signin`: it always emits
  `SignInRequired`, but `State(SignInRequired)` only when the state changes
  (`set_state`), so a repeated SignOut emits just `SignInRequired`, and
  clients key on that. A file it cannot delete is reported as an
  `Error`, but the engine still signs out in memory. The store and the outbox
  are kept: queued ops drain after the next `SignIn`, whose `prompt=select_account`
  lets another account sign in. That account would then get the old
  account's store and queued ops; this is a known gap, recorded as a
  follow-up.
- lookxy ignores `ProfileFetched` / `PhotoFetched` (it never asks for them).

## Granted scopes and a refused sign-in's error_description (#1875)

- `auth::TokenSet::scopes` (`Option<Vec<String>>`, `None` = unknown) is the
  token response's `scope`, split on whitespace and spelled as Microsoft
  sent it (Graph scopes usually as `https://graph.microsoft.com/Mail.ReadWrite`).
  An absent or blank `scope` is `None`. `tokencache` saves it as a `scopes`
  array, leaving the key out for `None`; a cache without it (from before
  #1875) or with anything but an array of strings loads as `None`, never as
  an error. Tests writing a cache by hand go through `protect`: on Windows
  `load` DPAPI-decrypts, so a plain JSON file fails there.
- `Engine::try_refresh` keeps the previous token's `scopes` when the refresh
  response names none (RFC 6749 §5.1: an omitted `scope` is the one
  requested); `auth::refresh` itself does not, as it has no previous token.
- `auth::missing_scopes(requested, granted)`: the scopes of `requested`
  (spelled as there, each once) the grant lacks, compared ASCII-lowercased
  without the `https://graph.microsoft.com/` prefix (`normalise_scope`).
  `offline_access`, `openid`, `profile`, `email` and any `.default` scope
  are never missing.
- `SyncEvent::Scopes { granted, missing }` (`missing` against
  `AuthConfig::scope`) comes from `announce_scopes`, called after a startup
  load, every `try_refresh` (startup near expiry and `with_auth`'s 401
  retry) and a sign-in redeem. It is sent only for a known grant that
  differs from `announced_scopes`, a sorted, de-duplicated set of
  normalised scopes (meta scopes kept), so a startup that refreshes sends
  one, and an hourly refresh with the same grant, reordered or re-cased,
  sends none. `enter_signin` clears `announced_scopes`, so every new session
  (after a sign-out or a failed refresh) sends it again. lookxy ignores it.
- `handle_redirect` reports a provider error as
  `authorization denied: <error>: <error_description>`, the description
  percent-decoded and on one line (`one_line`: every run of control
  characters and whitespace becomes one space, trimmed; Entra's has `\r\n`
  before `Trace ID:`). Only when the redirect's `state` matches: anything
  can send the browser to the loopback port during sign-in, and free text
  from it would be a phishing line in our UI. With a wrong or missing
  `state`, or a blank description, it is today's `authorization denied:
  <error>`; an error still wins over a state mismatch. The loopback page
  stays the generic error page.
