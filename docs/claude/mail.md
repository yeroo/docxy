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
  whatever is stored). Since #1978 the engine also fetches bodies on its
  own (below), through the same store path and the same rule
  (`store::body_satisfies`).
- Not done: FTS indexes an html body's markup (the triggers copy `content`).

## Sync one folder, the fast cycle, Sent Items after a send (#1885)

- `SyncCommand::SyncFolder { id }` (Shift+F9, "Update Folder") drains the
  outbox, runs that folder's delta and re-reads its counts
  (`GraphClient::get_folder_counts`: `GET /me/mailFolders/{id}?$select=…`,
  stored by `Store::set_folder_counts`, which leaves name, parent and
  well-known name alone), with `FoldersUpdated` only when the counts
  changed (`Engine::refresh_folder`). An id not in the store is an `Error`
  with no Graph call; signed out or inside `next_retry` it does nothing. A
  folder that never synced runs its phase 1 (#1871, below): the head, or
  for a small folder its full delta with the cutoff, which completes it; a
  big folder's older mail follows in idle time.
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
  response names none; `auth::refresh` itself does not, as it has no
  previous token. Why: Entra always names the granted scopes, so such an
  answer tells nothing new, and the last grant the server did name is kept.
  RFC 6749 §5.1 reads an omitted `scope` as everything requested
  (`cfg.scope`); that is not taken, as claiming scopes the server never
  named would hide a missing one until a 403, which #1875 removes.
- `auth::missing_scopes(requested, granted)`: the scopes of `requested`
  (spelled as there, each once) the grant lacks, compared ASCII-lowercased
  without the `https://graph.microsoft.com/` prefix (`normalise_scope`).
  `offline_access`, `openid`, `profile`, `email` and any `.default` scope
  are never missing.
- `SyncEvent::Scopes { granted, missing }` (`missing` against
  `AuthConfig::scope`) comes from `announce_scopes`, called after a startup
  load, every `try_refresh` (startup near expiry, and the 401 retries in
  `with_auth` and the outbox's `apply_op_with_retry`) and a sign-in redeem. It is sent only for a known grant that
  differs from `announced_scopes`, a sorted, de-duplicated set of
  normalised scopes (meta scopes kept), so a startup that refreshes sends
  one, and a later refresh (after a 401, about hourly) with the same grant,
  reordered or re-cased, sends none. `enter_signin` clears `announced_scopes`, so every new session
  (after a sign-out or a failed refresh) sends it again. lookxy ignores it.
- `handle_redirect` reports a provider error as
  `authorization denied: <error>: <error_description>`, both
  percent-decoded and on one line (`one_line`: every run of control
  characters and whitespace becomes one space, trimmed; Entra's description
  has `\r\n` before `Trace ID:`); a blank description is left out. Only
  when the redirect's `state` matches: anything can send the browser to the
  loopback port during sign-in, and free text from it would be a phishing
  line in our UI. With a wrong or missing `state`, no free text is shown:
  an `error` that is a bare OAuth code (`is_oauth_error_code`: 1-64 ASCII
  `a-z`/`_`, e.g. `consent_required`) gives today's `authorization denied:
  <error>`, anything else the fixed `authorization denied (redirect state
  did not match)`. An error still wins over a state mismatch. The loopback
  page stays the generic error page.

## Bodiless POSTs and one-line HTTP error text (#1954)

- `GraphClient::send` sends a POST or PATCH with no body as an empty one
  (`send_bytes(&[])`, which ureq frames as `Content-Length: 0`). ureq's
  `call()` writes no `Content-Length` at all, and Exchange Online front ends
  answer such a POST (`createReply`, `createReplyAll`, `createForward`,
  `send`) with an IIS `411 Length Required` page, now and then. GET and
  DELETE still go through `call()`, with no `Content-Length`.
- `GraphError::Http`'s `Display` never shows the body: `HTTP {status}:
  {message}` when the body is Graph JSON with a non-empty `error.message`
  (`client::graph_error_message`, which the outbox's `send_error_text`
  shares), else `HTTP {status} {reason}`, the reason phrase from
  `reason_phrase`'s fixed table (no HTML `<title>` scraping), else
  `HTTP {status}`. It is one line (`one_line`: whitespace runs become one
  space, control characters are dropped) of at most `HTTP_ERROR_TEXT_MAX`
  (200) characters, ending in `…` when cut. The engine's `react` emits it as
  `SyncEvent::Error`, which is what a host's status line shows. The body
  stays in the error (`Debug`, matches). `send_error_text` keeps `HTTP
  {status}` without a message, and caps the message the same way.

## Inline outbound attachments (#1949)

- An outbound draft attachment may be inline with a content id (an html
  body's `<img src="cid:pic1">`), from a file or from bytes (a pasted
  picture). `outbound_attachments` has three nullable columns (in
  `SCHEMA_SQL` and as idempotent ALTERs in `Store::init`): `content_id`
  (set = inline), `content_type` (NULL = `content_type_for(name)` at send
  time, every plain file row) and `data` (set = the bytes are in the row).
  A row from before reads as a plain file row.
- `add_outbound_attachment` is unchanged (lookxy's calls). An inline file:
  `add_inline_outbound_attachment_file`, keyed by its path and
  upserted: it makes a pending plain row of the same path inline in place
  (rowid kept); an uploaded plain row stays plain (Graph has it
  non-inline), and an inline row is left alone (the file is read at send
  time), so the same file under a second content id is ignored, as is a
  plain `add_outbound_attachment` over an inline row. Bytes:
  `add_outbound_attachment_bytes`, always inline, keyed `cid:<content_id>`
  (a reserved key namespace, never a file path; the key is returned for
  `remove_*`/`mark_*`), `size` = `bytes.len()`, content type required. A
  re-add of a content id replaces a pending row's name, type and bytes in
  place (`ON CONFLICT DO UPDATE`, not `INSERT OR REPLACE`, so the rowid and
  attach order hold); an uploaded row is left alone (Graph has its bytes).
- `outbound_attachments()` does not load `data` (compose lists on every
  redraw): `from_bytes` says it is there, `outbound_attachment_bytes` reads
  it for the send. `reconcile_id`, `discard_pending_*`, `clear_*`, `remove_*`
  and `mark_*` move or delete whole rows, so inline and bytes rows follow
  the #1884 and #1874 rules unchanged.
- `GraphClient::add_attachment_with(.., content_id)`: with `Some`, both the
  small `fileAttachment` POST and the upload session's `AttachmentItem` get
  `isInline: true` and `contentId` after the other keys. `add_attachment`
  and `add_file_attachment` keep their signatures and pass `None`: a plain
  attachment's request body is unchanged (no `isInline: false`).
  `apply_send` reads a bytes row's data (none left is a `Parse` error, like a
  missing file) and uploads with the row's content id and type.
- Not done: a lookxy UI for inline pictures, turning an html body's `data:`
  images into `cid:` attachments, checking that body references and content
  ids agree.

## Whole mailbox, newest first (#1871)

- `spawn`'s `backfill_days` keeps its type: `<= 0` (`engine::WHOLE_MAILBOX`)
  is the whole mailbox, a positive value the last that many days (window
  mode, what lookxy runs with 180). `cutoff_iso` already read it so.
- **Phase 1, in the sync pass.** A folder with no `delta_link` gets a *head*
  delta: `/messages/delta?$filter=receivedDateTime ge <since>&$orderby=
  receivedDateTime desc&$select=…` (`DeltaCursor::Head`), `since` being 14
  days ago (`HEAD_WINDOW_DAYS`), or the window when it is shorter. The
  server filters it, so no cutoff. Its deltaLink becomes the folder's
  (`Store::set_head_link`, scope `head`), so ticks, fast ticks and
  `SyncFolder` keep it fresh: new mail arrives during phase 2.
  `Store::folders` order makes it Inbox first. A folder whose listed
  `total_count` is at most 50 (`SMALL_FOLDER`, one page) skips the head:
  its full delta runs inline and completes it (an empty folder costs one
  request, not two).
- **Phase 2, one page per idle step.** `DeltaCursor::Full`
  (`$orderby=receivedDateTime desc`, no filter), from the folder's
  persisted `backfill_link`, else a fresh first call; one folder to its end
  before the next. A page carries the server's current state, and nothing
  else re-sends an old row's (the head's link sees only recent mail, the
  walk's deltaLink only later changes), so it merges (`merge_message`),
  but never over a local edit. That rule holds for every delta that starts
  fresh and so carries every message it covers, not only what changed (a
  head, a small folder's inline full delta, a backfill page:
  `merge_delta_items` with `protect`): it skips every id a queued outbox op
  names (`Store::pending_message_ids`, `Engine::protected_ids`: an
  optimistic write waiting after a 5xx, and a deleted message is not put
  back), and merges every other row with
  `Store::merge_message_unless_edited`, which leaves a draft with an
  unsaved edit (below) alone in the same statement as the write, so a
  compose save on lookxy's connection during the fetch is never lost;
  everything else is merged, drafts included. A round on a stored
  link merges everything, as Graph reports a change there. In window mode items
  older than the cutoff are skipped, unless a reconverge is pending.
  `Store::set_backfill_progress` stores the `nextLink`, the count and the
  walk's scope (`backfill_scope`: `window` once a page ran with the
  window's cutoff, else `all`), then `MessagesUpdated` and `FolderProgress`
  go out. A walk recorded `window` that resumes in whole mode starts over
  (`reset_backfill`). When the deltaLink comes, one last round on the
  folder's own link (the head's, or a link from before #1871) runs first,
  so recent mail changed during the walk is up to date. A link whose sync
  state expired (`is_expired_sync_state`) is skipped, and the walk's
  deltaLink heals the folder by replacing it; any other round that fails
  does not complete (a fatal failure stops through `react`, any other
  parks the folder), and the next step fetches the last page again. Then
  the deltaLink replaces the folder's link in one statement that marks it
  complete (`Store::complete_backfill`, scope `window` when any page of the
  walk ran with the cutoff, else `all`), and `repair_headerless` runs. A
  restart resumes from the stored link.
- **Unsaved draft edits** (`messages.local_edit`, migrated in
  `Store::init`). Compose saves a draft's fields into the store with no
  outbox op until `SaveDraft`, whose PATCH reads the row, so a fresh delta
  must not overwrite them, but must still merge a clean draft (one edited
  on another device, or one a quarantined op left wrong). The column is a
  generation: `update_draft_fields` bumps it. `ensure_draft_on_graph` reads
  it (`Store::local_edit_generation`) before it reads the draft, and after
  a successful create (plus `reconcile_id`) or update clears it
  (`Store::clear_local_edit`) only if it is still that generation, in one
  statement. An edit compose makes meanwhile stays unsaved, with no race
  against lookxy's connection; a failed push or a store error on the clear
  leaves the draft protected until its next push. A fresh delta reads the
  marker in the statement that writes (`merge_message_unless_edited`:
  `ON CONFLICT … DO UPDATE … WHERE local_edit = 0`), never from a list read
  before the fetch. Rows from before read as clean.
- **When a page runs.** `main_loop` waits zero while `next_backfill()` has a
  folder: signed in, outside `next_retry`, a folder list done
  (`backfill_done`), and the first folder in order with a delta link whose
  history is not complete (`history_complete`) and that is not parked. A
  queued command runs first (it waits for at most the page in flight), then
  a due timed wake (`Engine::due`), and only then one page. Without a fast
  tick, `due` is the tick (`next_tick`), a passed `next_retry` or a passed
  `op_retry` while signed in, so a throttled pass and a 5xx-deferred op are
  retried on time; with one, either deadline outside a back-off.
  `next_tick` moves with ticks and commands, never with pages. After a timed
  wake a passed `next_retry`/`op_retry` is dropped (`on_timer`), so one a
  tick left behind cannot run a pass at every zero wait.
- **Complete per mode** (`history_complete(scope, whole)`, asked only for a
  folder with a delta link: phase 1 first): a link from before #1871
  (scope NULL) and `window` are complete in window mode only, and whole mode
  backfills such a folder, its old link serving as the head meanwhile;
  `head` never; `all` always. Known limit: a folder completed as `window`
  stays complete when a caller later raises `backfill_days`, so the older
  mail never comes (as before #1871).
- **Failures of a page.** Auth, throttle (429/503 with `Retry-After`) and
  transport go through `react` (sign-in, `next_retry`, Offline), and the
  same link is asked for again after. An expired sync state
  (`client::is_expired_sync_state`: a 410, or a 400 whose `error.code` is
  `SyncStateNotFound`, `SyncStateInvalid` or `resyncRequired`) starts the
  folder over from a fresh full delta (`Store::reset_backfill`, count 0),
  once a pass (`Engine::expired`). A second expiry, any other non-fatal
  failure (another 4xx, a 404, a parse error, a page with neither link) or
  a store error is surfaced as an `Error` and parks the folder
  (`Engine::parked`) until the next sync pass: one request per park.
- **State.** A backfill step sets no `SyncState` (a 1,000-page backfill
  would send 2,000 `State` events); a failed page keeps what `react` set,
  and the next tick recovers it. It never calls `clear_backoff`.
- **Progress.** `SyncEvent::FolderProgress { folder_id, synced, total,
  complete }` after every page and once at completion. `synced` counts the
  upserts the walk passed within the window, merged or kept as a local
  edit (not `@removed`, not items the window skipped); `total` is the
  listed `total_count` in whole mode, `None` in window mode. `synced` may exceed `total` (the full delta re-sends the
  head's mail, mail arrives meanwhile, delta can repeat an item): a UI
  clamps. `Store::folder_history`/`folder_histories` hold the same
  persisted state (`folders.backfill_link`, `backfill_synced`,
  `history_scope`, `backfill_scope`, migrated in `Store::init`; `upsert_folder` never writes
  them) for a UI's first draw. lookxy ignores the event.
- **Reconverge.** `clear_delta_links` also clears the backfill link, count
  and scope, so every folder runs both phases again. `reconverge_pending`
  turns off phase 2's window cutoff (its pages merge as always, to
  correct rows the dropped op left wrong, drafts included, still skipping
  `protected_ids`). It is cleared only when a pass ran
  every folder's delta and no folder's backfill remains, else by the step
  that completes the last folder, or by the one that parks the last walk
  left: parked walks do not count (`backfill_remains`), so a page that
  fails (it is parked) cannot hold the reconverge, and the cutoff, off for
  good. It is persisted (`meta.reconverge_pending`,
  set by `clear_delta_links` in the same transaction, loaded by `run`,
  cleared by `Engine::end_reconverge`), so an engine restarted in the
  middle still runs the walks without the cutoff.
- One request at a time (the engine is one thread). Bodies stay lazy
  (`FetchBody`) beyond `prefetch_body_days` (#1978, below); headers and
  `bodyPreview` come for everything (`MESSAGE_SELECT`). Pages stay at 50
  (`odata.maxpagesize=50`).
- Tests: `FakeServer::start_paced` gives a route a delay (a slow page, so a
  test can act while a backfill runs), and `RecordedRequest::at` is the
  arrival time. Most older engine fixtures list folders of 0 or 1 message,
  so they take the small-folder path: one full delta at startup, no phase 2.
- Not done: a 410 on a folder's `delta_link` itself (not a backfill page)
  still surfaces an `Error` every tick; rows a full enumeration did not
  return are not swept (a delete during the backfill comes through the full
  deltaLink's later rounds).

## New mail with its body, prefetch, FetchBody first (#1978)

- `EngineOptions::prefetch_body_days` (`u32`, default `PREFETCH_BODY_DAYS`
  = 14; `Default` is a manual impl now) is how many days of the newest mail
  get bodies without a `FetchBody`. `0` turns every body fetch the engine
  makes on its own off: then bodies come only from `FetchBody`, as before.
  lookxy keeps `spawn`, so it runs the default.
- **Which messages.** `Store::body_candidates(format, since, limit, skip)`:
  received at `since` or later (header-less rows are out), not a draft
  (`is_draft`, the Drafts folder, the local sentinel: `get_body_for`'s
  rule), whose stored body does not satisfy `format` under
  `store::body_satisfies` (the rule `get_body_for` uses, written once),
  newest first. `Store::needs_body(id, format, since)` is the same rule for
  one row (a missing row: no). `idx_messages_received` keeps the
  cross-folder query cheap on a whole-mailbox store.
- **New mail.** Only a round on a stored delta link (`sync_folder_delta`
  with `DeltaCursor::Link`, and the backfill's last round on the folder's
  own link) collects the ids it adds: `merge_delta_items` with no
  `protect` notes an upsert that was not stored before
  (`Store::message_exists`) into `DeltaRun::new_ids`. After the repair,
  `fetch_new_bodies` fetches at most 50 of them (`NEW_BODY_LIMIT`), in the
  pass, fast tick or `SyncFolder`. A fresh delta (a head, a small folder's
  inline full delta, a backfill page) carries history, not new mail: the
  prefetch takes its rows, so a first sync is no longer than before.
- **Prefetch.** One idle step of the main loop fetches up to 10 bodies
  (`PREFETCH_BATCH`, `next_prefetch`/`prefetch_step`), signed in, outside a
  back-off, after a folder list succeeded, before any backfill page (the
  window bounds it). A command a checkpoint kept, or a tick come due, ends
  the batch, so they wait for one body at most, as for one backfill page; a
  kept command also ends a round's new-mail bodies (the prefetch takes the
  rest). One `GET /me/messages/{id}?$select=body` each, the
  call `FetchBody` makes (`fetch_and_store_body` is shared: `fetched_as`
  recorded, `BodyReady` sent, so `BodyReady` may name a message nobody
  opened).
- **Failures** (`background_body`). Auth, throttle or transport goes through
  `react` and stops the caller: a 429/503 sets `next_retry`, which the
  prefetch waits out. A 404, another 4xx or a parse error is not reported,
  as the user asked for nothing, and puts the id in `Engine::body_skip` for
  this run. A 5xx (other than a throttle's 503: `classify_status` makes
  500/502/504 `Http`) or a store failure parks these fetches until the next
  pass (`body_parked`), the id not skipped, so a failing service or store
  costs one request a pass, not a loop, and the body comes once it works;
  while parked, new-mail rounds fetch no body either. A response over its
  cap is a `Parse` error, not `Transport` (`parse_body_capped`: 32 MiB for
  a body, `BODY_MAX`; 10 MiB for any other JSON, ureq's old
  `into_string` limit), so a body too big to hold is skipped, never an
  offline loop; a `FetchBody` of one gets an `Error`. A row deleted since (its body fails the
  foreign key) is not a store failure. Note that `Store::open`'s
  connections keep rusqlite's default 5 s busy timeout, so a write lock
  held for a moment by the UI's own connection only delays the write.
- **FetchBody first.** The engine owns `cmd_rx`. `Engine::checkpoint` runs
  between two requests of long work: before each folder's delta in a pass
  and a fast tick, between delta pages (`run_delta`, which then returns
  `stopped` with no deltaLink: nothing records the walk complete), before
  each repair `get_message`, before each body fetch, before the drain,
  before people/categories, and before the tick's calendar. It serves a
  `FetchBody` at once and keeps every other command in `deferred`, in
  order, which the main loop runs before reading the channel again. It says
  stop only on a `Shutdown` (or a dropped handle) or when the `FetchBody`
  it served got an auth, throttle or transport failure, never because of a
  back-off set before it: `Refresh` runs a pass during one. A `Shutdown`
  stops the work at the next checkpoint; the commands that came before it
  still run. So a click costs one round trip plus the request in flight;
  any other command waits for the idle step in flight (one prefetch body,
  one backfill page, or a backfill's last step: its last round, repair and
  new-mail bodies).
  A served `FetchBody` overtakes the commands queued before it, which do
  not depend on a body read.
- Tests (`engine.rs` `tests::backfill::bodies`): the engine tests from
  before #1978 run `EngineOptions::without_prefetch` (`spawn_with_bases`,
  `spawn_in`, `spawn_fast`, `html_options`, the body tests): their fixtures
  are dated, some inside any recent window, so with prefetch on what they
  assert would change with the day they run. The #1978 tests opt in and
  date their mail relative to now. They read the store only after the
  engine stopped.
- Not done: no checkpoint inside the outbox drain (a send with big
  attachments still holds a click), the folder-tree listing or the
  calendar's pages; no `$batch`; pictures and attachments are not
  prefetched; no lookxy setting for the window.

## Calendar sync keeps reminder settings (#1937)

- `Store::upsert_event` and `Store::replace_events_in_window` (the writer
  `Engine::refresh_calendar` uses, in one transaction) both write an
  `events` row through one private `upsert_event_on(conn, e)`, so every
  column `NewEvent` carries, `reminder_minutes` and `is_reminder_on`
  included, is written by both. Before #1937 the window writer had its own
  copy of the SQL without the two reminder columns, and every synced event
  read back as 0 minutes, reminder off. A new `events` column goes into
  `upsert_event_on` once. `put_event_attendees` opens its own transaction,
  so `replace_events_in_window` still inlines the attendee SQL.
- Tests: `store` `calendar_tests::replace_events_in_window_writes_reminder_fields`
  (insert, and a later refresh that changes them),
  `calendar_tests::replace_events_in_window_conflict_path_updates_reminder_fields`
  (a stored row outside the window takes the `ON CONFLICT` path), and
  `engine.rs` `tests::refresh_calendar_keeps_reminder_settings` (a fake
  `calendarView` with `isReminderOn` / `reminderMinutesBeforeStart`).
- Not done: creating or updating an event sends no reminder fields to Graph
  (`event_body_json`), and a `local:` event is stored with 0/false until the
  next refresh.
