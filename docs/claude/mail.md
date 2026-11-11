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
  `SetCategories` resolve their id through `current_id` too since #1896
  (in the command arm and again at apply time), so a reconciled `local:`
  id reaches the Graph id; one never reconciled still reaches Graph as is.
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
  (`Store::put_fetched_body_with_headers`), not the content type that came back. NULL is
  a body not fetched by `FetchBody`: `put_body` (a `createReply` draft),
  `create_local_draft`, `update_draft_fields` (which also makes the body
  html, as compose writes html), or a row from before the column.
  `Store::get_body_for(id, format)` is the body when it satisfies the format:
  a recorded format must equal it; NULL satisfies both when the body is
  html or a draft's, else counts as asked for text (the old engine always
  did). A draft is `is_draft` or filed in the Drafts folder (synced or the
  local sentinel). Before #2096 a complete delta item reset `is_draft`
  (`isDraft` was not selected); a row from then may still read 0 in
  Drafts, so the folder still counts. So a draft's local body (a plain-text
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
  `tmp_path`, and since #1877 the secret store's item), clears the throttle/offline back-off (not the outbox's
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

## Attachment upload progress and resume (#1941)

- `SyncEvent::AttachmentProgress { draft_id, path, name, sent, total }`
  comes from the outbox drain while a `SendDraft` uploads: once for a small
  (≤ `INLINE_MAX`, 3 MiB) attachment after its POST (`sent == total`), and
  after each chunk of an upload session Graph accepted (`sent` = the
  chunk's end). `draft_id` is the op's id, as `Sent { id }`; `path` is the
  row key, so two attachments of one name differ. An uploaded (skipped) row
  reports nothing. All of them follow the command's `Sending` (a retry from
  a later drain emits no new one) and precede `Sent`/`SendFailed`. The
  engine sends them from a clone of `evt_tx` inside `apply_op_with_retry`'s
  closure (it must not borrow `self` across `try_refresh`). lookxy ignores
  the event: its composer closes on Send.
- `GraphClient::upload_attachment` (`pub(crate)`, as is `UploadObserver`)
  reports to the observer: `session(Some(url))` when a session opens,
  `session(None)` when it is no good any more, `progress(sent, total)`.
  `add_attachment_with` is it with no resume and the `()` observer.
- `outbound_attachments.upload_url` and `upload_digest` (nullable, in
  `SCHEMA_SQL` and as idempotent ALTERs) hold the open session and the
  fingerprint of what it uploads: `outbox::upload_digest`, `"{len}:{FNV-1a
  64 hex}"` over the name, the effective content type (the row's or
  `content_type_for(name)`), the content id and the bytes, with marker
  bytes between fields. `apply_send` passes the url as `resume_url` only
  while the digests match, so a file edited between attempts (a file row
  is read at send time, and `add_outbound_attachment` is `INSERT OR
  IGNORE`, so no re-add could clear it) goes up whole in a new session,
  never spliced. `set_outbound_attachment_upload_session` writes or clears
  both; `mark_outbound_attachment_uploaded` clears them. A store error on
  either only loses the resume. The url is pre-authenticated (write access
  to that one session, short-lived): it sits in SQLite with the bodies,
  unlike the OAuth tokens (#1877).
- A resume first GETs the url (no bearer) and goes on from the lowest
  start in `nextExpectedRanges`, reported as `progress(start, total)`. Any
  failed GET (any status, transport, a body with no usable range, a start
  not below the size) opens a new session from byte 0. A 4xx other than
  429 on a resumed PUT forgets the session and opens a new one in the same
  attempt; on a new session's PUT it forgets the session and fails the
  send (the drain's 4xx rule). A 5xx, a 429/503 or a transport failure
  keeps the session for the next attempt. `sent` can go down after a
  fallback, between attempts or within one.
- `put_bytes` maps a 401 and a 404 from the session url to `Http`, not
  `Unauthorized`/`NotFound`: they are the session's, so the engine neither
  refreshes the token nor asks for a sign-in. For a not-yet-posted send a
  404 ends in `fail_send` as before (only its text changes: `HTTP 404`). A
  401 changes more: it used to be `Unauthorized`, so the engine refreshed
  the token, retried, then called `enter_signin` with the op kept queued;
  now `fail_send` returns the draft to Drafts with `SendFailed`.
- Not done: a session whose last chunk Graph accepted but whose answer was
  lost is gone at the next GET, so the file is uploaded again (the #1874
  rule for any lost upload answer). No UI shows the progress yet.

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
  rest). One `GET /me/messages/{id}?$select=body,internetMessageHeaders`
  (`BODY_SELECT`, #1933) each, the
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
  stops the work at the next checkpoint; of the commands that came before
  it only the writes are persisted, unsent (#2127, below). So a click costs one round trip plus the request in flight;
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
- Not done: no checkpoint inside the outbox drain (it only takes the waiting
  commands before each op, #2127; a send with big
  attachments still holds a click), the folder-tree listing or the
  calendar's pages; no `$batch`; pictures and attachments are not
  prefetched; no lookxy setting for the window.

## Exchange headers with a body (#1933)

- The body GET (`GraphClient::get_body_with_headers`) is
  `/me/messages/{id}?$select=body,internetMessageHeaders` (`BODY_SELECT`): the
  headers come in the same request, never in a second one. `get_body` is that
  call with the headers dropped (a fresh draft's, `store_composed_draft`).
- Only `graph::model::STORED_HEADERS` are kept: `X-MS-Exchange-Organization-AuthAs`
  (`Internal` inside the tenant, `Anonymous` from outside) and
  `X-MS-Exchange-Organization-MessageDirectionality` (`Originating` /
  `Incoming`), never the whole block. Names match ignoring ASCII case, the
  first of a name wins, values are trimmed (an empty one stays `Some("")`).
  Parsing is lenient: no field, a null one or a malformed entry gives no
  header, never an error.
- #2015 adds `msip_labels`, the Microsoft Purview sensitivity label
  (`MSIP_Label_<guid>_Name=Confidential; ...`), so a client can show it as
  Outlook does under the recipients. It is stored like the two stamps (case
  ignored, trimmed, otherwise verbatim); mailcore never parses it, and label
  names or parents from the Information Protection API are the caller's.
  Mail stored before #2015 reads `None` until its body is fetched again.
  Since #2088 every synced row also carries whether it has an enabled label
  and its name, read from the same metadata as an extended property (see "A
  message's protection on its row (#2088)"); this header is unchanged.
- `SyncEngine::fetch_and_store_body` (FetchBody and the engine's own fetches)
  stores them with `Store::put_fetched_body_with_headers`: body and headers in
  one transaction, the message's earlier headers replaced. `put_fetched_body`
  leaves headers alone; it is `#[cfg(test)]`, tests use it.
- They live in `message_headers` (`message_id`, lowercased `name`, `value`),
  a new table, so an older database gets it empty at `open` and its mail
  reads `None`; old mail is never refetched for them. Rows go with their
  message (FK cascade) and follow `reconcile_id`.
- `Store::message_header(id, name)` reads one; `None`: no such header came,
  it is not a kept one, or the body was stored before #1933. Deciding on it is
  the caller's.
- Tests: `client.rs` `get_body_selects_internet_message_headers`,
  `get_body_without_usable_headers_yields_none`; `store`
  `body_format_tests::message_header_*`, `refetch_replaces_message_headers`,
  `a_failed_header_write_keeps_the_earlier_body_and_headers`,
  `a_missing_message_stores_no_headers`, `message_headers_go_with_a_deleted_message`,
  `reconcile_id_moves_message_headers`; `engine.rs`
  `fetch_body_stores_exchange_auth_as_header`. #2015: `model.rs`
  `msip_labels_is_a_stored_header`, `client.rs`
  `get_body_keeps_the_msip_labels_header`, `store`
  `body_format_tests::message_header_keeps_msip_labels`, `engine.rs`
  `fetch_body_stores_the_msip_labels_header`.

## Calendar sync keeps reminder settings (#1937)

- `Store::upsert_event`, `Store::replace_events_in_window` and
  `Store::apply_calendar_sync` (the writer `Engine::refresh_calendar` uses
  since #1984, in one transaction) all write an
  `events` row through one private `upsert_event_on(conn, e)`, so every
  column `NewEvent` carries, `reminder_minutes` and `is_reminder_on`
  included, is written by all three. Before #1937 the window writer had its own
  copy of the SQL without the two reminder columns, and every synced event
  read back as 0 minutes, reminder off. A new `events` column goes into
  `upsert_event_on` once. Attendees go the same way: `put_event_attendees`
  (in its own transaction), `replace_events_in_window` and
  `apply_calendar_sync` (in theirs) all run
  `put_event_attendees_on(conn, id, a)`.
- Tests: `store` `calendar_tests::replace_events_in_window_writes_reminder_fields`
  (insert, and a later refresh that changes them),
  `calendar_tests::replace_events_in_window_conflict_path_updates_reminder_fields`
  (a stored row outside the window takes the `ON CONFLICT` path), and
  `engine.rs` `tests::refresh_calendar_keeps_reminder_settings` (a fake
  `calendarView` with `isReminderOn` / `reminderMinutesBeforeStart`).
- The write side (a create or edit sets the reminder, and a create stores
  Graph's) is #1994's: see "Calendar event create and update fields".

## Calendar events carry what a client shows (#1983)

- `graph::model::Event::from_json` reads `showAs`, `isOnlineMeeting`,
  `onlineMeetingProvider`, a `join_url`, `categories`, `sensitivity`,
  `importance`, `type` (`event_type`), a series' `recurrence`
  (`EventRecurrence`), `isCancelled`, `isOrganizer`, `hasAttachments`,
  `responseRequested`, `allowNewTimeProposals`, `originalStartTimeZone` /
  `originalEndTimeZone`, `calendar.id` (`calendar_id`), and each attendee's
  `status.time` (`response_time`). Enums stay Graph's raw strings.
- `join_url` is the first of `onlineMeeting.joinUrl`, `onlineMeetingUrl`, a
  meeting link in `location.displayName`, then one in `body.content`
  (`meeting_link_in`, the one helper the store uses too): the location goes
  first because it is a field someone set, while a body can quote an old
  invite. A link starts at `https://` (any case) and ends at a quote, `<`,
  `>`, whitespace or an HTML entity other than `&amp;` (`is_entity_end`: `&#`
  then anything, or `&`, a letter, up to 31 letters or digits and `;`).
  lookxy's `escape_html` writes `&lt;`, `&gt;` and `&quot;`; other HTML
  bodies also use `&nbsp;` and numeric entities (`&#160;`, `&#34;`). A raw
  `&` not shaped like an entity (`?pwd=x&y=1` in a plain-text location)
  stays in the link. `&amp;` is decoded and trailing `.,;:)]!` trimmed. After a rejected link the scan resumes at
  that link's end, so a meeting link nested in it is not taken and each byte
  is scanned once: a body is sender-controlled, and resuming just past the
  `https://` made a long run of them quadratic. Only the host a browser would open decides
  (`is_meeting_url`): the authority ends at `/`, `?`, `#` or `\` (a browser
  reads `\` as `/`), userinfo (`@`) is refused, a `:port` must be digits, and
  the host may hold only ASCII letters, digits, `-` and `.`. Hosts:
  `teams.microsoft.com` (`/l/meetup-join/`, `/meet/`), `teams.live.com`
  (`/meet/`), `zoom.us` / `zoomgov.com` and their subdomains (`/j/`, `/w/`,
  `/my/`), `meet.google.com`, `meet.goto.com`, `chime.aws`, and `webex.com`
  and its subdomains (any non-empty path), `global.gotomeeting.com`
  (`/join/`). Rejected: a meeting host in another link's query or path,
  lookalikes (`notzoom.us`), userinfo after a port
  (`teams.microsoft.com:443@evil.example`) and a backslash
  (`evil.example\.zoom.us`).
- `responseRequested` and `allowNewTimeProposals` are `true` when absent, as in
  Graph, and every default agrees: the hand-written `Default` of `Event`,
  `NewEvent` and `EventRow`, and the columns' `DEFAULT 1`. Graph's "no date"
  (`0001-01-01...`, a `noEnd` range's `endDate`, a never-responded attendee's
  `status.time`) reads as `""` (`graph_date_or_empty`).
- Two recurrence columns: `events.recurrence` is the recurrence a local
  create was given, which `event_for_send` re-sends on every create (and on a
  pre-#1994 queued update read as `LEGACY_UPDATE`; an edit since #1994 never
  sends it): the create-side `Recurrence` bare, or since #2034 a lossless
  `EventRecurrence` wrapped as `{"graphRecurrence": ...}`;
  `events.graph_recurrence` is Graph's `EventRecurrence` (read side), written
  by `upsert_event_on`. Never write Graph's recurrence into `recurrence`: the
  lossy create-side type (three kinds, `numbered` becomes `noEnd`,
  `firstDayOfWeek` forced to `sunday`) would rewrite a synced series on the
  first edit.
- Every new column is in `upsert_event_on` (insert and `ON CONFLICT`), as
  #1937 asks. `Store::init` adds them to an old database with `ALTER TABLE`
  after `SCHEMA_SQL` runs, so `SCHEMA_SQL` must not index any of them (an old
  database would fail the `CREATE INDEX` before the column exists).
- `create_local_event` stores what the host set (#1994), else the fields
  Graph sets on create: `busy`,
  `normal` sensitivity and importance, organizer, both request flags on,
  `singleInstance`, or `seriesMaster` with its recurrence (the lossless one
  when set, #2034) in `graph_recurrence`, and the `join_url` `meeting_link_in` finds in the
  location or body. `update_event_fields` derives `join_url` again from the
  edited location and body, except on an online meeting (`is_online_meeting`),
  whose link is Graph's `onlineMeeting.joinUrl` and is kept. A series then
  reads as `calendarView`'s occurrences on the next refresh.
- Tests: `model.rs` `event_reads_calendar_client_fields`,
  `event_defaults_when_fields_absent`, `event_join_url_*`,
  `event_recurrence_reads_every_pattern_and_range`,
  `attendee_response_time_and_never`; `store` `calendar_tests`
  (`replace_events_in_window_writes_calendar_client_fields`,
  `upsert_event_conflict_path_updates_calendar_client_fields`,
  `create_local_event_reads_as_graph_will_create_it`,
  `update_event_fields_follows_the_join_link_unless_online_meeting`,
  `events_without_1983_columns_migrate`); `engine.rs`
  `refresh_calendar_stores_calendar_client_fields`.
- Not done: plain `calendarView` returns occurrences, never series masters; a
  synced master is stored when a calendar delta round returns or fetches it
  (#2064), with its `recurrence`; the grid queries leave it out once a row of
  its series is stored.
  `calendar_id` is the calendar a pass fetched the event from since #1984
  (`calendar.id` in the JSON loses to it). Safe Links-wrapped links
  (`*.safelinks.protection.outlook.com/?url=...`) are not unwrapped.

## Calendar event create and update fields (#1994)

- `graph::client::EventInput` carries every field a calendar client sets:
  `showAs`, `sensitivity`, `importance`, `categories`, `isReminderOn` /
  `reminderMinutesBeforeStart`, `isOnlineMeeting` / `onlineMeetingProvider`,
  `responseRequested`, `allowNewTimeProposals`, and typed attendees
  (`AttendeeInput`, `""` sends `required`). `event_body_json` sends only the
  groups in `input.fields` (`EventFieldSet`) and never an empty enum string
  (Graph refuses `""`). `onlineMeetingProvider` is sent only when the host
  sets one: with none Graph uses the calendar's default
  (`teamsForBusiness` on Microsoft 365), and no provider is guessed.
- `EventFieldSet` groups and their JSON names, which a queued
  `OutboxOp::UpdateEvent` stores in `fields` (never rename one; an unknown
  name is dropped, a row without `fields` reads as `LEGACY_UPDATE`, what an
  update sent before: subject, times, location, attendees, body,
  recurrence):

  | name | Graph properties |
  |---|---|
  | `subject` | `subject` |
  | `times` | `start`, `end`, `isAllDay` (one group: Graph validates them together) |
  | `location` | `location` |
  | `attendees` | `attendees` |
  | `body` | `body` |
  | `recurrence` | `recurrence`, only when `Some` |
  | `showAs`, `sensitivity`, `importance`, `categories` | the same name |
  | `reminder` | `isReminderOn`, `reminderMinutesBeforeStart` |
  | `onlineMeeting` | `isOnlineMeeting`, `onlineMeetingProvider` when set |
  | `responseRequested`, `allowNewTimeProposals` | the same name |

- An edit: `Store::update_event_fields` diffs the host's
  `LocalEventFields` against the stored row, writes it, and returns the
  changed groups; the host sends `SyncCommand::UpdateEvent { id, fields }`
  only when the set is non-empty, and `apply_op` sends nothing for an empty
  one. The op reads the stored values at send time, so two queued edits each
  send the latest value of their own groups. Each `Option` field of
  `LocalEventFields` is `None` for "keep as stored" (and Graph's default on
  a create), and `Some("")` for the four enum strings is `None`, so a host
  whose form shows only some fields (lookxy) keeps what Outlook set.
  Attendees are rewritten only when the ordered (name, address, type) list
  changed; an attendee whose address matches a stored one
  (case-insensitive) keeps its `response`, `response_time`, and its type when
  the input's is `""`. Before, every edit reset every RSVP to `none`.
- lookxy's form shows a body's plain text: a save whose text equals the
  stored HTML's `from_html(..).plain()` keeps the stored HTML, so an
  untouched body is neither rewritten nor counted as changed.
- A create sends every group, read at send time (so an edit of the `local:`
  event before the drain rides along), except the reminder when the host set
  none: `create_local_event` marks the row `events.reminder_unset` (internal,
  not in `EventRow`, not written by `upsert_event_on`) and Graph applies the
  mailbox default. `Store::reconcile_created_event` re-points the id and, in
  the same transaction, writes Graph's returned reminder, a non-empty
  `join_url` and provider, and clears `reminder_unset`, so a Graph-id row
  never keeps it. An edit that sets a reminder clears it too.
- `GraphClient::update_event` returns Graph's updated event (`Ok(None)` for
  a 204 or a body without an `id`). When the op's set holds `onlineMeeting`,
  `apply_op` writes back the join link and provider
  (`Store::set_event_online_meeting`); its error is ignored, as the PATCH
  already happened and the next refresh corrects the row.
- Graph ignores `isOnlineMeeting: false` once a meeting is online. The store
  writes `false` but keeps the stored `join_url` (its rule keys on the
  stored flag before the edit), as Graph keeps the meeting; the next refresh
  sets the flag back.
- Tests: `client.rs` `event_body_sends_calendar_client_fields`,
  `event_body_sends_only_the_given_fields`, `event_body_skips_empty_enum_strings`,
  `event_field_set_names_round_trip`, `update_event_patches_only_changed_fields`,
  `update_event_without_an_event_in_the_answer_is_ok_none`; `store`
  `calendar_tests::update_event_fields_*`, `create_local_event_stores_given_fields`,
  `create_local_event_without_reminder_marks_it_unset`,
  `reconcile_created_event_*`, `events_without_1983_columns_migrate`
  (`reminder_unset` too), `outbox_tests::update_event_op_round_trips_fields`,
  `legacy_update_event_op_reads_as_full_set`; `outbox.rs`
  `update_event_sends_only_changed_fields`, `update_event_with_empty_set_sends_nothing`,
  `legacy_update_event_sends_the_old_full_set`,
  `update_event_online_meeting_writes_back_join_url`,
  `create_event_without_reminder_omits_it_and_stores_graphs`,
  `create_event_with_reminder_sends_it`; lookxy
  `saving_an_untouched_synced_event_sends_no_update` (timed and all-day),
  `editing_only_the_title_sends_only_the_subject`.
- Not done: lookxy's form has no controls for the new fields (the host API
  has them). A series is edited with `UpdateEventSeries` (#2022). Reading the calendar's
  `defaultOnlineMeetingProvider` / `allowedOnlineMeetingProviders`.

## Every calendar and calendar queries (#1984)

Items 3 and 4 of #1984; the wider window with `calendarView` delta is #2013, a
range on demand (`FetchCalendarRange`) #2014 (the next section).

- `GraphClient::list_calendars` (`/me/calendars?$select=id,name,color,hexColor,
  isDefaultCalendar,canEdit,owner&$top=100`, paged, all or `Err`) and
  `calendar_view_in(id, from, to)` (`/me/calendars/{encoded id}/calendarView`,
  sharing `calendar_view`'s UTC `Prefer` and paging). `calendar_view_in` sets
  each event's `calendar_id` to the id it was asked for: the URL decides.
- `calendars` table, written whole by `Store::replace_calendars` (Graph's
  order in `position`; returns whether anything changed, so
  `SyncEvent::CalendarsUpdated` goes out only then). `Store::calendars()`
  gives `CalendarRow { calendar, syncs }`.
- Which calendars sync: `Store::synced_calendar_ids`, the one rule. A host's
  choice (`SyncCommand::SetCalendars { ids: Some(..) }`, in `meta`
  `calendar_selection` as a JSON array, so it survives a restart) intersected
  with the stored list (a chosen id Graph does not list syncs once it does);
  with no choice (`None`), the user's own: owner address equal to the default
  calendar's, ASCII case and whitespace ignored (`is_contact`'s fold), the
  default always, and every calendar when there is no default or it has no
  owner address.
- A pass (`Engine::refresh_calendar`, tick and `RefreshCalendar`): list, then
  each synced calendar's round over `calendar_window(now)` (a
  `calendarView/delta` round since #2013, next section but one), then one
  transaction (`Store::apply_calendar_sync`): (1) rows with `calendar_id = ''`
  (`local:` too) take the default calendar's id, when one is stored; (2) each
  fetched calendar's non-`local:` rows overlapping the window are replaced
  (an initial round; a delta round applies only what it names);
  (3) non-`local:` rows of any date whose non-empty `calendar_id` is not
  synced are deleted. Step 1 matters: before #1984 sync read only the
  default calendar and never deleted rows that slid out of the window, so a
  `''` row is the default calendar's history, which a date navigator shows.
  A failed list changes nothing. A 401, throttle or transport error on any
  view writes no events for the pass (the calendar list, stored and
  announced before the views, stays); a 404/HTTP/parse error on one calendar
  emits `Error` and keeps that calendar's rows.
- `SetCalendars` (`Store::set_calendar_selection`) deletes at once only the
  events of **stored** calendars that no longer sync. Unknown ids and `''`
  rows wait for the next pass, which runs only after a successful list, so a
  host restoring its choice at startup (empty `calendars`) deletes nothing.
  It emits `CalendarUpdated`, `CalendarsUpdated` when the synced set changed,
  and refreshes when signed in and not inside a throttle/back-off wait.
- `create_local_event` stores the default calendar's id (`CreateEvent` POSTs
  to `/me/events`), `''` before the list is known (step 1 adopts it), unless
  the host names another calendar (#2048, "Create in a chosen calendar").
- Queries: `Store::events_in_range(from, to, calendar_ids)` (the
  `events_in_window` overlap rule and order; an empty slice is no events;
  `events_in_window` still returns every calendar, and lookxy keeps it), and
  `Store::days_with_events(year, month, utc_offset_minutes, calendar_ids)`:
  a timed event marks each local day its `[start, end)` overlaps (an end at
  local midnight does not mark the next day, zero length marks its start
  day); an all-day event marks its UTC dates without the offset. One fixed
  offset, not a zone: in a DST month an event within the shifted hour of
  midnight can mark the neighbouring day.
- Test fixtures: the fake server takes the first route whose prefix
  matches, so the list route is `/me/calendars?` (it cannot answer
  `/me/calendars/{id}/calendarView`); `engine.rs` `calendar_routes` builds
  both. A test that only silences the tick answers `/me/calendars?` with an
  empty list (no view requests; `is_calendar` counts list requests).
- Known gaps: deselecting a calendar while an edit or RSVP on one of its
  events is queued deletes the row, so `apply_op`'s `event_input_for` fails
  until the op is quarantined. A chosen calendar of another owner needs
  `Calendars.Read.Shared`, which the default scope lacks, so its view errors
  on every pass (it is not synced unless chosen). A create goes to the
  calendar the host names since #2048; editing and deleting still address
  `/me/events/{id}`. Each pass costs one
  list plus one view per synced calendar.
- Tests: `model.rs` `calendar_reads_graph_fields`; `client.rs`
  `list_calendars_follows_next_link`, `list_calendars_fails_whole_when_a_page_fails`,
  `calendar_view_in_encodes_the_id_and_pages`; `store` `calendar_tests`
  (`replace_calendars_upserts_deletes_and_keeps_order`,
  `own_calendars_follow_the_default_calendars_owner`,
  `calendar_selection_intersects_the_list_and_survives_a_restart`,
  `set_calendar_selection_deletes_only_stored_deselected_calendars_events`,
  `set_calendars_before_the_first_list_deletes_nothing`, `apply_calendar_sync_*`,
  `events_in_range_filters_by_calendar`, `days_with_events_marks_local_days`,
  `create_local_event_takes_the_default_calendar`); `engine.rs`
  `refresh_calendar_syncs_two_calendars_into_separate_ids`,
  `set_calendars_drops_a_calendars_events_and_stops_syncing_it`,
  `refresh_calendar_skips_calendars_of_other_owners_by_default`,
  `refresh_calendar_keeps_a_calendars_events_when_its_view_fails`,
  `refresh_calendar_writes_nothing_when_a_view_is_throttled`,
  `refresh_calendar_adopts_pre_1984_rows_into_the_default_calendar`.

## Calendar ranges on demand (#2014)

Item 1 of #1984: a host whose calendar shows dates outside the kept window
(`calendar_window`, 30 days back and 180 forward since #2013) asks for them.

- `SyncCommand::FetchCalendarRange { from_utc, to_utc }` (the store's
  `YYYY-MM-DDTHH:MM:SSZ`; anything else, or `from >= to`, is an `Error`
  with no request). For each synced calendar (`synced_calendar_ids`), only
  the parts the store does not hold fresh (`Store::calendar_range_gaps`) go
  to `calendar_view_in`, **one request per gap** (a range around the
  window has two, neither re-downloading it), and each gap is written as it
  lands (`Store::apply_calendar_range`: the pass's step 1 adoption, then
  that calendar's synced rows overlapping the gap replaced, then the gap
  recorded). Then `CalendarUpdated` when anything was written, and
  `SyncEvent::CalendarRangeFetched { from, to }` (the command's bounds). A
  range wholly fresh is served from the store: no request, only
  `CalendarRangeFetched`. With no stored calendar list it lists first.
- Failures: signed out (with a gap to fetch) is an `Error`; inside a
  throttle or back-off wait the command does nothing more, and a 401,
  throttle or transport error mid-range ends it (what landed stays,
  recorded): in both no `CalendarRangeFetched`, the host asks again. A
  404/HTTP/parse error on a calendar's gap is an `Error`: that gap and the
  calendar's later gaps stay unfetched and unrecorded (its earlier gaps stay
  written), and the other calendars carry on, so `CalendarRangeFetched`
  still follows and means "every calendar whose view did not fail"; asking
  again fetches what is missing. However the command ends (a store error
  too), `CalendarUpdated` goes out first when a gap was written
  (`Engine::fetch_range_gaps`).
- Coverage is the `calendar_ranges` table (`calendar_id`, `from_utc`,
  `to_utc`, `fetched_at` in unix seconds), kept on disk, so a restart does
  not re-fetch. Per calendar the intervals are disjoint: recording one cuts
  the older ones it overlaps back to their outside pieces, which keep
  their own `fetched_at`. A row is fresh while `fetched_at > fresh_since`
  (`RangeStamp`, now minus `EngineOptions::calendar_range_fresh_for`,
  default `CALENDAR_RANGE_FRESH_FOR`, an hour). Strict, so a zero
  freshness re-fetches within the same second.
- Every calendar pass records its window (`apply_calendar_sync` takes a
  `RangeStamp`), so a range inside the window costs nothing. The window
  moves (each UTC day since #2013, each pass before) and would leave one
  sliver row per move: recording
  first deletes the calendar's rows at or before the cutoff, which can
  never count again.
- Forgetting: `delete_calendar_events_on` (deselecting, `SetCalendars`)
  deletes the calendar's ranges with its events, and the pass's step 3
  deletes the ranges of every calendar not synced, events or not (an empty
  calendar Graph stops listing). Selected again, its ranges are fetched
  again.
- Not done: a range dropped in a back-off or offline is not retried (the
  host sends it again; `events_in_range` serves what is stored meanwhile).
  No cap on a range's length, no checkpoint between a range's pages.
  lookxy never sends the command.
- Tests: `store` `calendar_tests`
  `calendar_range_gaps_finds_the_parts_not_fetched_fresh`,
  `calendar_range_gaps_freshness_is_strict`,
  `recording_a_range_splits_older_intervals`,
  `recording_prunes_rows_that_can_no_longer_be_fresh`,
  `apply_calendar_sync_records_the_window_of_each_fetched_calendar`,
  `apply_calendar_range_replaces_only_that_calendar_in_the_range`,
  `deselecting_a_calendar_forgets_its_ranges`,
  `apply_calendar_sync_forgets_ranges_of_a_dropped_empty_calendar`;
  `engine.rs`
  `fetch_calendar_range_outside_the_window_is_fetched_once_then_served_from_the_store`,
  `fetch_calendar_range_inside_the_refreshed_window_makes_no_request`,
  `fetch_calendar_range_fetches_only_the_part_outside_the_window`,
  `fetch_calendar_range_around_the_window_makes_one_request_per_gap`,
  `fetch_calendar_range_refetches_a_stale_range`,
  `fetch_calendar_range_rejects_bad_bounds`,
  `fetch_calendar_range_throttled_emits_no_range_fetched`,
  `fetch_calendar_range_keeps_going_past_a_failed_calendar`. The engine
  tests count calendar requests only (`calendar_requests`: `/me/calendars`
  and, since #2013, the `/cal-delta/` link rounds): the mail sync makes
  its own requests in the meantime.

## Calendar delta over a kept window (#2013)

Item 2 of #1984, with item 3's "delta per calendar": each pass keeps a
wider window fresh without downloading it again.

- The window: `EngineOptions::calendar_window` (`CalendarWindow {
  past_days, future_days }`, default 30 and 180). `calendar_window(base,
  window)` anchors at the start of `base`'s UTC day: `[day - past_days,
  day + future_days + 1)`, the whole of the last day. Anchored, so the
  bounds, and a deltaLink over them, hold all day; the first pass of a new
  day (or after the option changed) runs an initial round per calendar. An
  unanchored `now - 30 d` would never match a stored link.
- `GraphClient::calendar_delta(CalendarDeltaCursor)`: one page. `Start {
  calendar_id, from_utc, to_utc }` GETs `/me/calendars/{encoded id}/
  calendarView/delta?startDateTime=&endDateTime=` (no `$top` or `$select`:
  Graph's calendarView delta refuses both) and sets each upsert's
  `calendar_id` (the URL decides); `Link(url)` sends a nextLink or
  deltaLink as is (`full_url`), and the engine sets the calendar. Every
  request, a link's too, sends `Prefer: outlook.timezone="UTC",
  odata.maxpagesize=50`: a deltaLink does not keep the time zone.
  `EventDeltaPage` holds `EventDeltaItem::Upsert(Event)` or `Removed(id)`
  (an `@removed` item; one without an id is dropped). No `value` array, or
  neither a nextLink nor a deltaLink (a round that could never end), is a
  `Parse` error.
- State: the `calendar_deltas` table (`calendar_id` primary key,
  `from_utc`, `to_utc`, `delta_link`), `CREATE TABLE IF NOT EXISTS`, so an
  older database starts with initial rounds. `Store::calendar_delta(id)`.
  A pass uses the link only while its `(from, to)` is the pass's window.
  The old single `meta` `calendar_delta_link` (never read) is gone; an
  old row of it stays, unread.
- A round (`Engine::calendar_delta_round`) follows nextLinks to the
  deltaLink with every page in memory, so a round that fails part way
  writes nothing (replaying a link is harmless). In a delta round the
  latest item about an event wins: a removal drops the round's earlier
  upserts of that id (not of its occurrences: see the series rule below),
  and an upsert cancels an earlier removal, since the store removes
  first. An initial round's
  removals are ignored: it replaces the window.
- Writing: `apply_calendar_sync` takes `SyncedCalendar { fetched, round,
  delta_link }` (`FetchedCalendar` itself is unchanged, and
  `apply_calendar_range` still takes it). `CalendarRound::Window` is the
  #1984 replace. `CalendarRound::Delta { removed }` upserts `fetched`'s
  events under the calendar's id with their attendees, and each removed id
  deletes that calendar's non-`local:` row with that id and every row
  whose `series_master_id` it is (a removed series takes its
  occurrences; Graph's docs do not say whether it lists a master or each
  occurrence, so both are handled; an id with no row is a no-op). A
  removal deletes only rows overlapping the window: Graph's calendarView
  delta also lists as `@removed` events outside the range that were
  added, changed or deleted, which Graph may still have (an event moved
  to day 200, a `FetchCalendarRange` row, history); an event that moved
  out still has its old in-window times locally, so it goes. A removed
  id the round also upserts occurrences of is a changed series master
  (one starting before the window is "outside the range" when edited),
  not a removed series: only the row with that id goes, and the engine
  keeps the round's occurrences of it. Both
  record the window as fetched (#2014's freshness) and store the window
  and deltaLink, in the pass's one transaction. Except after a series
  cascade (a delta round's removed master took occurrence rows other than
  its own id's): Graph may list a master that only changed (its series end
  moved past the window, say) with no in-window occurrence in the round,
  so the calendar's delta state is deleted and the window not recorded
  again; the next pass's initial round restores what Graph still has, or
  confirms it is gone. Until then the earlier window record (an hour
  fresh) still serves a `FetchCalendarRange` inside it from the store.
- Errors: an expired link (`is_expired_sync_state`, as mail's #1871: a
  410, or a 400 `syncStateNotFound`/`syncStateInvalid`/`resyncRequired`)
  runs that calendar's initial round at once, in the same pass, once, with
  no `Error`. A 401, throttle or transport error goes through `react`: the
  pass writes nothing and every link stays, so the next pass after the
  wait runs the same link. A 404, other HTTP or parse error is an `Error`,
  and that calendar keeps its rows and link: a link that keeps failing
  that way is retried until the window moves, at most a day.
- Forgetting: `delete_calendar_events_on` (deselect, `SetCalendars`)
  deletes the calendar's state with its events, or the calendar selected
  again would replay its link onto no rows; step 3 deletes the state of
  every calendar not synced.
- Event-op quarantine: a delta never names an event Graph did not change,
  so a quarantined `RespondEvent`, `UpdateEvent` or `DeleteEvent` would
  leave its optimistic local write (an RSVP, edited fields, a deleted row)
  until the next day. Before #2013 every tick's window replace repaired
  it. `drain_outbox` now calls `Store::clear_calendar_deltas` and then
  `refresh_calendar` for those three: on disk, so if that refresh fails the
  next pass still runs initial rounds. `CreateEvent`'s `local:` row is
  never replaced either way.
- Not done: no checkpoint between a round's pages (as before); series
  masters come with the rounds, or are fetched for their occurrences, and
  are stored, off the grid once a row of their series is (#2064);
  `FetchCalendarRange` stays plain
  `calendarView` (a range overlapping the window, when its record is
  stale, re-fetches that part, server truth either way). Two passes
  straddling 00:00 UTC make the second an initial round, so a test that
  asserts a link round could see one around midnight; no clock seam.
- Tests: `model.rs` `event_delta_page_reads_upserts_removed_and_links`,
  `event_delta_page_without_value_is_none`; `client.rs`
  `calendar_delta_start_encodes_id_window_and_prefer_headers`,
  `calendar_delta_link_cursor_uses_url_as_is`,
  `calendar_delta_page_without_any_link_is_a_parse_error`; `store`
  `calendar_tests` `apply_calendar_sync_delta_upserts_and_removes_only_that_calendar`,
  `apply_calendar_sync_delta_removed_series_master_removes_its_occurrences`,
  `apply_calendar_sync_delta_keeps_local_rows`,
  `apply_calendar_sync_delta_removes_only_rows_in_the_window`,
  `apply_calendar_sync_delta_keeps_a_series_the_round_names_occurrences_of`,
  `apply_calendar_sync_delta_cascade_drops_the_delta_state_and_records_no_range`,
  `apply_calendar_sync_delta_plain_removal_stores_the_link`,
  `apply_calendar_sync_stores_and_forgets_delta_state`,
  `clear_calendar_deltas_clears_every_calendar`; `engine.rs`
  `calendar_window_defaults_to_30_days_back_and_180_days_forward`,
  `refresh_calendar_first_pass_is_an_initial_delta_over_the_window`,
  `refresh_calendar_uses_the_custom_window_option`,
  `calendar_delta_adds_updates_and_removes_occurrences`,
  `calendar_delta_removed_series_master_removes_its_occurrences`,
  `calendar_delta_removed_outside_the_window_keeps_the_row`,
  `calendar_delta_removed_master_beside_its_occurrences_keeps_them`,
  `calendar_delta_410_restarts_with_an_initial_round`,
  `calendar_delta_throttled_keeps_the_link_and_writes_nothing`,
  `calendar_delta_link_of_another_window_is_not_used`,
  `respond_event_quarantine_reconverges_local_status_from_server_truth`,
  `delete_event_quarantine_restores_the_event`. Fixtures:
  `calendar_routes` serves each calendar's initial delta round (its body
  with `delta_link_of(id)`, a relative `/cal-delta/{id}?token=1`), that
  link with no change, and the plain `calendarView?` ranges use; every
  view prefix ends in `?`, so none answers another. #2014's "outside the
  window" events are now 200 days out.

## Contacts folder as records, is_contact and contact writes (#1934, #2063)

- Records: `/me/contacts` (the default Contacts folder) is stored one row per
  contact in `contact_records`, keyed by Graph `id` (or a `local:` id until its
  create reaches Graph), with `changeKey` and the fields Outlook's contact
  form edits (`graph::model::ContactFields`): given, middle, surname, display
  name, file-as, `emailAddresses` (name and address each, in
  `contact_emails`), business and home phones, mobile, company, job title,
  department, business and home `physicalAddress` (Graph's `countryOrRegion`),
  birthday, personal notes, categories. Phone and category lists are
  `\u{1f}`-joined (`encode_categories`), as `messages.categories`. The type is
  `FolderContact { id, change_key, fields }`, not `Contact`:
  `store::Contact` is the autocomplete row (lookxy uses it).
- `birthday` is `YYYY-MM-DD` (`""` for none): the date part of Graph's
  DateTimeOffset when it starts with one. A write sends noon UTC of the day
  (`T12:00:00Z`), so no client's zone shows the day before; `""` sends `null`.
- Reads: `Store::folder_contacts()` (ordered by `file_as`, or `display_name`
  where it is empty, `COLLATE NOCASE`, then id) and `Store::folder_contact(id)`
  (a Graph id, or the `local:` id a contact was created under, followed
  through `contact_aliases`).
- `Store::is_contact(address)` answers whether an address is in the user's
  own Contacts folder, for a "Trust email from my contacts" setting. It
  reads `contact_emails.address_key` only (and the legacy table below). It
  never reads `contacts`, which holds `/me/people` (anyone the user
  corresponded with) and mined senders and recipients for autocomplete: a
  trust decision on those would trust anyone who ever mailed the user. The
  match is exact (`=`, never `LIKE`), ignoring ASCII case and surrounding
  whitespace (`folder_contact_key`: `to_ascii_lowercase`, both when stored and
  when asked). Not Unicode `to_lowercase`, which `contacts` keeps for
  autocomplete: it folds U+212A KELVIN SIGN onto `k`, so `\u{212A}ate@x.com`
  would match `kate@x.com`, a false positive on trust. The cost is on the
  safe side: a stored `Ärger@x.com` does not match `ärger@x.com`. A blank
  address is `false`. A blank address or one without an `@` (a directory
  contact's X500 DN) is stored for display with `address_key = ''`, so it
  never matches. A contact created or deleted locally counts at once.
- Legacy: #1934's `folder_contacts` (one row per address) stays in
  `SCHEMA_SQL`. Nothing writes it; `is_contact` still reads it and
  `replace_contacts` empties it, so a database from before the records keeps
  its trust answers until their first fetch, never a window of `false`.
  Before that fetch `folder_contacts()` lists nothing, so no contact only the
  legacy table knows can be edited or deleted.
- Tables of their own, not a `source` value in `contacts`: there any two
  sources merge into `both`. `upsert_contact`, `refresh_local_contacts` and
  `search_contacts` never touch them. All `CREATE TABLE IF NOT EXISTS`, so an
  older database gets them empty at `open`. `contact_emails` follows a
  re-keyed row by `ON UPDATE CASCADE` (`foreign_keys` is on).
- `GraphClient`: `contacts()` GETs
  `/me/contacts?$top=200&$select=id,changeKey,…` (`CONTACT_SELECT`, every
  property `ContactFieldSet` names; Graph's default page is 10) and follows
  `@odata.nextLink`; an item without an `id` is skipped, any page's error is
  an `Err`, never a partial list. `contact(id)` GETs one with the same
  `$select`. `create_contact(&ContactFields)` POSTs the non-empty properties
  (`ContactFieldSet::non_empty`) and parses Graph's answer.
  `update_contact(id, &fields, set)` PATCHes only `set`'s properties (an empty
  set sends nothing). `delete_contact(id)`. Path ids are percent-encoded
  (`encode_path_segment`). Errors are the usual `GraphError`: a 404 is
  `NotFound`, a 403 `Http { status: 403, .. }`. The body
  (`contact_body_json`): strings as given (`""` clears), lists whole, an
  address as its full five-key object, `emailAddresses` as `[{name, address}]`
  without blank addresses.
- `ContactFieldSet`: one bit per Graph property; its JSON names are the Graph
  property names, which a queued `updateContact` stores, so they never change
  (an unknown name is dropped on read; a row without `fields` is corrupt).
  `ContactFieldSet::changed(a, b)` is what `Store::update_contact_fields`
  returns.
- Full pass: `sync_people`, then `sync_folder_contacts` (`contacts()` +
  `Store::replace_contacts`), then `refresh_master_categories`. A successful
  fetch replaces the records in one transaction and sends
  `SyncEvent::ContactsUpdated`, changed or not: every row Graph no longer
  lists goes, every listed one is written, **except** a `local:` row is never
  removed, a contact a queued `UpdateContact` names (resolved through
  `contact_aliases`) keeps its stored row, and one a queued `DeleteContact`
  names is not written back. The outbox is read inside the same transaction.
  Any error (403 without `Contacts.Read`, Conditional Access, offline, parse)
  is swallowed: no `Error`, no event, the pass carries on, and the stored set
  is kept. A revoked scope therefore leaves the last set trusted until a
  fetch succeeds. Full passes only (startup, `Refresh`, sign-in, a pass
  `needs_full_pass` asks for): in a long-running session a removal made
  elsewhere can take days to count; the `is_contact` doc says so.
- Writes, through the outbox. Unlike event writes, the store write queues
  the op **in the same transaction** as the local row (#2063 r1): a command
  waiting in the channel, or deferred by a full pass's `checkpoint`, would
  otherwise leave the row unprotected while `replace_contacts` or a refused
  write's re-read runs, and the later op would send the server's own values.
  The commands only drain (`drain_queued`) and emit `ContactsUpdated`; they
  queue nothing. A host that never sends one still has the op sent by the
  next drain (any tick). `revert_folder_contact`, `discard_contact_row` and
  `replace_contacts` queue nothing. Blank phone and category entries are
  dropped before a contact is stored or compared (`tidy_contact_fields`):
  `encode_categories` stores `[""]` as `""`, which reads back as `[]`.
  - `Store::create_local_contact(&fields)` → `local:` id, `CreateContact`
    queued; `SyncCommand::CreateContact { id }` pushes it. The op POSTs the row as stored at
    send time; `Store::reconcile_created_contact` re-keys it to Graph's id,
    sets `change_key`, fills `display_name`/`file_as` from Graph only where
    the stored ones are empty (Graph computes them), overwrites nothing else
    (an edit made while the POST was in flight is kept; its queued
    `UpdateContact` sends it), and records `contact_aliases(local → id)`,
    also when the row was deleted before Graph answered (so the queued
    `DeleteContact` reaches the Graph contact).
    `SyncEvent::ContactCreated { local_id, id }` follows. A row gone before
    the drain, or an id that already resolves (a retry after the re-key),
    sends nothing.
  - `Store::update_contact_fields(id, &fields)` → the changed
    `ContactFieldSet`, `UpdateContact` queued under the id the contact is
    stored as; `SyncCommand::UpdateContact { id, fields }` pushes it. The
    PATCH sends those properties with their values at send time. An empty set
    writes and queues nothing, and its command does nothing. An id still `local:` after resolving, or a row gone, sends
    nothing.
  - `Store::delete_contact(id)` → `DeleteContact` queued when a row went;
    `SyncCommand::DeleteContact { id }` pushes it. A 404 is success. A `local:` id never re-keyed sends nothing.
- Refused writes: 401, throttle, transport and 5xx are unchanged (stop or
  defer; a 5xx keeps the op queued, and the pending-op rule above keeps a full
  pass from undoing it). Any other failure is final at the first refusal, not
  the event ops' five attempts and quarantine: a 403 or 400 never heals by
  retrying, and the quarantine's mail reconverge has nothing to do with
  contacts. The engine (`refuse_contact_write`) reads the row as the user left
  it, drops the op, and reverts to the server: a refused create deletes its
  `local:` row (`discard_contact_row`); a refused update or delete owes a
  re-read (`Store::owe_contact_reread`, the `contact_reread_owed` table, so a
  restart keeps it): `GET /me/contacts/{id}` and `revert_folder_contact`
  store Graph's version, a 404 deletes the row. The re-read waits while
  another queued contact op names the same contact, as that op reads the row
  at send time and would otherwise PATCH the server's own value, losing the
  user's later edit silently; it runs when the last such op is applied or
  refused, and at the end of every drain, an empty one included, so the
  startup drain settles one a restart interrupted (`settle_contact_rereads`).
  Every later edit is a queued op from the moment it is written, and
  `revert_folder_contact` checks the queue in its own transaction: an edit
  written during the GET makes it write nothing and keep the id owed. A
  final refusal of the GET (a 4xx other than 404, such as a 403 without read
  scope, or an answer that did not parse) gives the re-read up
  (`forget_contact_reread`) and the row is left until the next full pass
  replaces it. A transient one (a 5xx, a throttle, offline, a 401 a refresh
  did not cure) keeps it owed and pauses re-reads for the rest of that drain
  (`contact_rereads_paused`); a later drain retries it. Best-effort: no
  back-off, offline state or sign-in is started from there.
  `owed_contact_rereads` is a plain read (no write lock), as it runs on every
  drain; the revert checks the queue again in its own write transaction. Then `SyncEvent::ContactWriteFailed { id, write, status, error,
  attempted }`, one `SyncEvent::Error("{create|update|delete} contact {id}:
  {error}")` (as the reminder ops report, so a host showing only `Error` lines
  sees it), and `ContactsUpdated`. `id` is the id stored at the failure (the
  Graph id once re-keyed); `status` is the HTTP status (`NotFound` →
  `Some(404)`), `None` for a parse or store failure; `attempted` is the row as
  the user left it (`None` for a delete).
- A refused create is not proof Graph lacks the contact: a POST that worked
  but whose answer did not parse, or whose re-key failed in the store, lands
  there with `status: None`. Deleting the `local:` row is still right: the
  next full pass brings Graph's copy back.
- No `If-Match`: the stored `changeKey` is not used for optimistic
  concurrency, the last writer wins.
- Scope: `AuthConfig::default()` asks for neither. Reading needs
  `Contacts.Read`; writing needs `Contacts.ReadWrite`, which a host adds to
  its `AuthConfig::scope` (#1875's `Scopes { missing }` tells it when the
  grant lacks it). With only `Contacts.Read` every write is refused 403:
  `ContactWriteFailed { status: Some(403), attempted: Some(..) }`, the edit
  kept in `attempted`, the store back to the server's record. lookxy ignores
  `ContactsUpdated`, `ContactCreated` and `ContactWriteFailed`.
- Tests: `client.rs` `contacts_follows_next_links_and_keeps_every_contact`,
  `contacts_parses_every_record_field`, `contact_gets_one_record_and_404_is_not_found`,
  `create_contact_posts_the_nonempty_fields_and_parses_the_answer`,
  `update_contact_patches_only_the_given_fields`, `delete_contact_404_is_not_found`,
  `contact_403_on_write_is_http_403`, `contact_field_set_names_round_trip`,
  `contact_field_set_changed_and_non_empty`, `contacts_propagates_a_403`,
  `contacts_later_page_error_is_an_error`, `contacts_page_without_value_is_an_error`;
  `store` `is_contact_matches_only_folder_contacts`,
  `is_contact_folds_ascii_case_only`,
  `replace_contacts_drops_absent_and_never_matches_blank`,
  `is_contact_on_a_database_from_before_the_table`,
  `is_contact_reads_legacy_folder_contacts_until_the_first_replace`,
  `replace_contacts_stores_records_and_drops_absent`,
  `folder_contacts_orders_by_file_as_then_display_name`,
  `folder_contact_by_id_and_by_local_alias`,
  `update_contact_fields_returns_only_changed`,
  `reconcile_created_contact_rekeys_and_keeps_local_edits`,
  `reconcile_created_contact_of_a_deleted_row_still_aliases`,
  `replace_contacts_keeps_rows_with_pending_ops`, `contact_ops_round_trip_json`,
  `contact_writes_queue_their_op_in_the_same_transaction`,
  `blank_phone_and_category_entries_are_not_stored_or_compared`,
  `revert_folder_contact_waits_while_an_op_names_it`,
  `owed_contact_reread_survives_reopening_the_store`;
  `outbox.rs` `create_contact_reconciles_the_local_id`,
  `create_contact_retry_after_reconcile_sends_nothing`,
  `create_contact_of_a_deleted_row_sends_nothing`,
  `update_contact_of_a_local_id_sends_nothing`,
  `update_contact_after_create_patches_the_graph_id`,
  `delete_contact_404_is_success`; `engine.rs`
  `created_contact_round_trips_through_a_full_pass`,
  `contact_edit_patches_only_the_changed_fields`, `contact_delete_sends_delete`,
  `contact_delete_404_is_success`, `contact_write_403_reports_and_reverts`,
  `contact_delete_403_restores_the_row`,
  `refused_contact_write_with_failed_reread_leaves_the_row`,
  `refused_contact_edit_keeps_a_later_queued_edit`,
  `is_contact_follows_created_and_deleted_contacts`,
  `full_pass_keeps_a_contact_edit_waiting_behind_a_5xx`,
  `full_pass_keeps_an_edit_whose_command_is_still_in_the_channel` and
  `full_pass_keeps_a_delete_whose_command_is_still_in_the_channel` (the store
  written while a slow `/me/contacts`, `start_paced`, is in flight),
  `reread_after_refusal_waits_for_an_edit_written_but_not_commanded`,
  `owed_contact_reread_survives_a_transient_get_failure`,
  `owed_contact_reread_survives_a_restart`,
  `full_pass_syncs_the_contacts_folder_apart_from_people_and_mined`,
  `contacts_403_keeps_the_stored_set_and_does_not_fail_the_pass`,
  `contact_removed_from_the_folder_stops_counting_after_the_next_full_pass`,
  `only_a_full_pass_fetches_the_contacts_folder`, and the request order in
  `normal_tick_without_fast_option_is_unchanged` and
  `fast_tick_syncs_only_the_fast_folders`. A refused write's engine test waits
  for the refusal's `ContactsUpdated` and then the command's own
  (`wait_refused`), a create for `ContactCreated` and then the command's
  (`wait_created`): a wait for a single `ContactsUpdated` would catch the
  leftover one.
- Not done: the contact photo (#2065). A periodic refresh of the set (a
  normal tick, or `/me/contacts/delta`). Contacts in other contact folders
  (`/me/contactFolders/{id}/contacts`), contact groups. A directory (GAL)
  contact's X500 DN address is not matched: resolving it needs `/users`.
  Feeding the folder into autocomplete. Clearing the set on sign-out (the
  store is kept across sign-out, #1920). Dedup of a create retried after a
  lost POST answer (as events: Graph has nothing to ask). `If-Match`.

## Cancel and forward a meeting (#1985)

The first slice of #1985 (items 1 and 2). The rest is split: series and occurrences
(#2022), respond options and proposals (#2023), meeting message types (#2024), reminder
dismiss and snooze on the server (#2025).

- `GraphClient::cancel_event(id, comment)` POSTs `/me/events/{id}/cancel` with
  `{"comment":…}`, and `forward_event(id, to, comment)` POSTs `.../forward` with
  `{"toRecipients":[…],"comment":…}` (the drafts' `emailAddress` shape). `comment` is `""`
  for `None`, as in `respond_event`. Graph answers both with `202` and no body.
- Both are outbox ops (`cancelEvent`, `forwardEvent`), so they wait out being offline. A
  `ForwardEvent` stores its recipients as `[{"name":…,"address":…}]`; `from_json` keeps
  an empty list (only a hand-edited row holds one) and refuses a recipient without an
  address.
- `SyncCommand::CancelEvent`: the **engine** does the local delete, unlike
  `DeleteEvent`, whose host deletes the row first. It reads the row (`Store::event`) and
  refuses with an `Error` an event that is not stored or whose `is_organizer` is false
  (Graph lets only the organizer cancel; an attendee declines or deletes). Then it queues
  the op **before** it deletes the row, as `SendDraft` does: a failed queue after the
  delete would hide a meeting every attendee keeps, with no op whose quarantine could
  bring it back. A host must not delete the row itself first.
- A cancel answered 404 is dropped by the drain at once, with no retry and no quarantine
  `Error`, and resets the calendars (`reset_calendars`: delta state cleared, then a
  refresh of initial rounds). A 404 is a retry after a lost 202 (Graph deleted the
  organizer's copy on the first attempt; a lost 202 is a `Transport` error, which bumps no
  attempt, so the attempt count cannot tell) or a cancel Graph could never do: a stale id,
  or an event `/me/events` does not hold, such as a shared calendar's. Only Graph knows
  which, so the reset asks it: the row stays gone when Graph deleted the event, and comes
  back when Graph still has it. `apply_op` returns the 404 as is. A `local:` cancel sends nothing (nobody was invited), as
  `DeleteEvent`. A quarantined `CancelEvent` is an event op: the calendar reset brings
  the row back, as for `DeleteEvent`.
- The engine does not refuse a cancel of an event without attendees: the stored attendee
  list is not authoritative. Whether to offer Cancel (attendees) or Delete is the host's.
- `SyncCommand::ForwardEvent` changes nothing locally, so no `CalendarUpdated`, and it is
  not an event op for the quarantine. An empty `to`, or a `local:` id (an event id has
  no later alias to follow, unlike a draft's `current_id`), is an `Error` and nothing is
  queued.
- Known gaps, recorded as follow-ups: a forward retried after a lost 202 goes out twice
  (Graph has nothing to ask whether it went out). A pending cancel can be undone locally
  by an *initial* calendar round (a window move, `FetchCalendarRange`) while it is still
  queued offline, as a pending `DeleteEvent` can. A `local:` cancel queued behind its
  `CreateEvent` has `DeleteEvent`'s race. No lookxy or suite UI yet.
- Tests: `client.rs` `cancel_event_posts_comment`,
  `cancel_event_without_comment_sends_empty`, `cancel_event_id_is_percent_encoded`,
  `cancel_event_refused_is_an_error`, `forward_event_posts_recipients_and_comment`,
  `forward_event_without_comment_sends_empty`; `store`
  `outbox_op_cancel_and_forward_round_trip`, `event_by_id_returns_the_row`; `outbox.rs`
  `cancel_event_local_id_makes_no_request`, `cancel_event_posts_cancel`,
  `cancel_event_404_and_refusals_are_errors`, `forward_event_posts_forward`; `engine.rs`
  `cancel_event_deletes_and_posts_cancel`, `cancel_event_refuses_an_attendee_or_unknown_id`,
  `cancel_event_of_a_local_event_deletes_it_and_sends_nothing`,
  `cancel_event_quarantine_restores_the_event`,
  `cancel_event_404_brings_back_an_event_graph_still_has`,
  `cancel_event_404_after_a_lost_202_leaves_the_event_gone`, `forward_event_posts_forward`,
  `forward_event_refuses_local_id_and_no_recipients`.

## Recurring series and occurrences (#2022)

Item 3 of #1985: list a series' occurrences, open "the series", and edit or delete
this occurrence or the whole series, as Outlook does.

- `GraphClient::get_event(id)` GETs `/me/events/{encoded id}` with the UTC `Prefer`:
  a master with its `recurrence` (`EventRecurrence`) and `event_type ==
  "seriesMaster"`, which `calendarView` never lists. An answer without an `id` is a
  `Parse` error. `event_instances(master, from, to)` GETs `/me/events/{encoded
  id}/instances?startDateTime=&endDateTime=&$top=50`, paged and with the `Prefer`
  of `calendar_view` (`calendar_view_paged`).
- Which id: **this occurrence** is the occurrence's own id, through the existing
  `UpdateEvent` (its PATCH never carries `recurrence`: `update_event_fields` never
  reports the group, and a synced occurrence's row has none) and `DeleteEvent`.
  **The series** is the master's id: `UpdateEventSeries`, and for a delete
  `Store::delete_event_series(master)` (the row with that id, a local series' own,
  and every row whose `series_master_id` it is, in any calendar, with attendees),
  then `DeleteEvent { id: master }`.
- A series edit sends Graph's own shape: `EventInput::series_recurrence`
  (`Option<EventRecurrence>`), under the `recurrence` group, wins over the
  create-side `recurrence` and is written by `EventRecurrence::to_send_json`. That
  sends the pattern's `type` and `interval`, and `month`, `dayOfMonth`, `daysOfWeek`,
  `firstDayOfWeek` and `index` only when non-zero or non-empty, as given (nothing
  defaulted). It sends the range's `type` and `startDate`, plus `endDate` only for an
  `endDate` range, `numberOfOccurrences` only for a `numbered` range, and
  `recurrenceTimeZone` only when non-empty. `to_json` (the `graph_recurrence` column
  text) is no write body: Graph refuses its `""` dates.
- Engine: `FetchEventSeries { id }` → `EventSeriesFetched { id, master }`;
  `FetchEventInstances { id, from_utc, to_utc }` → `EventInstancesFetched { id,
  master_id, from, to, instances }`, with bounds checked as `FetchCalendarRange`
  checks them (else an `Error`, no request); `UpdateEventSeries { id, input }`
  queues `OutboxOp::UpdateEventSeries`. All three resolve `id` with
  `series_master_of`: a stored occurrence's `series_master_id`, else the id itself.
  A `local:` master is an `Error` with nothing sent or queued (a local series is
  edited through its row). The two fetches are direct calls (signed out is
  `Error("not signed in")`, a 401 or throttle goes through `react`) and store
  nothing. A kept window's occurrences come from the calendar sync, and other dates
  from `FetchCalendarRange`. A fetched event whose non-empty `event_type` is not
  `seriesMaster` is an `Error`. Graph names no calendar in these answers, so the
  master and each instance take the calendar of a stored row of the series
  (`Store::series_calendar_id`), or `""` when none is stored.
- `OutboxOp::UpdateEventSeries { id, body }` (`updateEventSeries`) stores the PATCH
  body whole (`event_body_json` at command time), because the master was not
  stored then (#2064 stores it, off the grid). `apply_op` sends it with `update_event_body`. An empty `input.fields`
  queues nothing. There is no optimistic local write, so the op is not an event op
  for the quarantine reset. Instead the calendars owe a reset once it went
  through (`calendar_reset_owed`, in memory; a refused `RespondEvent` owes it too, #2023): a pattern change mints new occurrence
  ids, and how calendarView delta reports a series change is the uncertain part
  of #2013, so initial rounds take each window from Graph. The reset is decided
  where it is used: every `refresh_calendar`, on any path (the drain's own
  quarantine and cancel-404 `reset_calendars` included), first takes the waiting
  commands (`checkpoint`, only while the reset is owed; a shutdown stops the
  refresh), then checks (`calendar_reset_ready`). Only when no event op with a
  local write (`writes_event_locally`: `RespondEvent`, `UpdateEvent`,
  `DeleteEvent`, `CancelEvent`) is queued, and no command that queues one
  (`command_writes_event_locally`) waits in `deferred`, does it clear every delta
  state and run initial rounds; otherwise it runs from the kept links and the
  reset stays owed. So the reset's initial rounds never run while such an op is
  queued or its command has reached the engine: they would replace its row
  (a host writes the row before it sends `UpdateEvent` or `DeleteEvent`), and a
  queued `UpdateEvent`, which reads its row at send time, would send Graph's old
  values. A host that writes its row after the reset's refresh began has the
  general race of any refresh, and a delta round from a kept link can still upsert
  a changed occurrence over a queued edit: both are the follow-up. The refresh
  does not run in the drain's loop after a series edit, so a 401, throttle or
  offline error it meets reaches the path as usual (a quarantine's or cancel-404's
  reset in the loop does not stop the drain on one: pre-existing). `on_tick`
  refreshes after its pass anyway; every other path that drains (a command's
  `drain_queued`, a fast tick, `SyncFolder`, the startup, sign-in and `Refresh`
  passes) calls `refresh_if_calendar_reset_owed` after its drain, which refreshes
  when the reset may run, signed in, outside any wait and not shutting down, and
  tells the caller to stop when that refresh signed out or started a wait;
  `drain_queued` then leaves the state that failure set. A drain that quarantines
  or cancels around a series edit can refresh twice. A restart before the reset
  ran drops it, and the delta rounds carry on.
- Masters are stored since #2064 (the delta returns them, and a missing one is
  fetched), off the grid: `events_in_window` / `events_in_range` leave a master out
  once a row of its series is stored, or it would show on the series' first day, and
  a `FetchCalendarRange`, whose plain `calendarView` never returns one, keeps it.
  See "Occurrences take their series' fields (#2064)".
- A series edit with `TIMES` names the master's zone and wall clocks
  (`EventInput::start_time_zone` / `start_local` and the end's, #2039, which closes
  #2030), so the series stays in its zone; with none named it is sent in UTC, and
  Graph moves the series into UTC, where its local times drift across DST. See
  "Event times in a named time zone (#2039)".
- Known gaps (follow-ups): cancelling a whole series (`CancelEvent` wants a stored row) and "this
  and following" (an end-date edit plus a new series) are not done. No lookxy or
  suite UI: lookxy ignores both new events.
- Tests: `model.rs` `event_recurrence_send_json_drops_what_graph_refuses`; `client.rs`
  `event_instances_gets_the_range_with_utc_and_pages`,
  `get_event_returns_the_master_with_its_recurrence`,
  `get_event_without_id_is_a_parse_error`,
  `update_occurrence_patches_its_id_without_recurrence` (a pin),
  `update_series_patches_the_master_with_its_full_recurrence`; `store`
  `delete_event_series_removes_master_and_occurrences`,
  `outbox_op_update_event_series_round_trips`; `outbox.rs`
  `update_event_series_patches_master_with_stored_body`,
  `update_event_series_local_id_makes_no_request`,
  `delete_occurrence_and_series_delete_their_ids` (a pin); `engine.rs`
  `fetch_event_series_resolves_an_occurrence_to_its_master`,
  `fetch_event_series_of_a_single_event_is_an_error`,
  `fetch_event_series_refuses_local_and_signed_out`,
  `fetch_event_instances_emits_the_range`, `fetch_event_instances_rejects_bad_bounds`,
  `update_event_series_queues_and_patches_the_master`,
  `update_event_series_given_an_occurrence_patches_the_master`,
  `update_event_series_queued_offline_resets_on_the_later_drain`,
  `update_event_series_reset_throttled_stops_the_pass`,
  `update_event_series_reset_waits_for_a_queued_event_edit`,
  `update_event_series_reset_skips_an_event_edit_queued_after_it`,
  `update_event_series_reset_waits_for_an_event_command_not_yet_queued` (a slow
  series PATCH, `spawn_calendar_engine_paced`),
  `update_event_series_reset_offline_keeps_the_state`,
  `update_event_series_reset_does_not_refresh_after_a_shutdown`,
  `update_event_series_sent_by_a_fast_tick_resets_the_calendars`,
  `update_event_series_with_no_fields_queues_nothing`. A 503 is a throttle
  (`Throttled`), not a deferring 5xx: a test of the deferred path uses a 500.

## Creating a series with any recurrence (#2034)

- `LocalEventFields::series_recurrence` (`Option<EventRecurrence>`) is Graph's
  whole recurrence on create: every pattern type, `noEnd`, `endDate` and
  `numbered` ranges, `firstDayOfWeek`, `index`, `month`, `recurrenceTimeZone`,
  as given. It wins over the create-side `recurrence` (kept: its callers
  compile and send as before) when both are set; the losing one is not
  stored. Both are create-only: `update_event_fields` ignores them.
- `create_local_event` stores it in `events.recurrence` as
  `{"graphRecurrence": <EventRecurrence::to_json>}` (`SEND_RECURRENCE_KEY`,
  `encode_send_recurrence`), and in `graph_recurrence` as the row's
  recurrence, with `event_type` `seriesMaster`. The wrapper keeps the shapes
  apart: `Recurrence::from_json` would read a bare lossless daily, weekly or
  absoluteMonthly text and drop its `index`, `month` and `numbered` range.
  Not a new column (a migration, and two columns that can disagree), and not
  `graph_recurrence`, which sync writes and must never be re-sent.
- `decode_send_recurrence` reads the column for `event_for_send`: a wrapper is
  `EventSendData::series_recurrence`, a bare object the create-side
  `recurrence`, and anything else (corrupt text, a wrapper without a `pattern`
  object, an unknown create-side type) neither, without an error.
  `event_input_for` passes it as `EventInput::series_recurrence`, which
  `event_body_json` sends with `to_send_json` under the `recurrence` group.
- Not done: a pending `local:` series' recurrence cannot be changed before its
  `CreateEvent` runs (the row edit never writes recurrence, and
  `UpdateEventSeries` sends nothing for a `local:` id).
- Tests: `store` `calendar_tests` `create_local_event_stores_a_lossless_recurrence`,
  `create_local_event_lossless_recurrence_wins`,
  `update_event_fields_never_changes_a_lossless_recurrence`,
  `event_for_send_reads_neither_shape_as_no_recurrence`,
  `create_local_event_round_trips_recurrence`; `outbox.rs`
  `create_event_sends_every_lossless_recurrence_unchanged` (6 pattern types × 3
  ranges), `create_event_sends_a_lossless_recurrence_as_given` (a JSON literal),
  `create_event_with_create_side_recurrence_sends_as_before`,
  `create_event_without_recurrence_sends_none`.

## Event times in a named time zone (#2039)

A host names the time zone of an event's start and of its end, so Graph keeps the
event, and expands a series, in the organizer's zone: a weekly 09:00 stays 09:00 across
DST and on the right weekday. Also #2030: zone-preserving series time edits.

- `EventInput`, `LocalEventFields` and `EventSendData` carry `start_time_zone`,
  `start_local`, `end_time_zone`, `end_local` (`String`, `""` by default). The zone is a
  Windows time zone id (`W. Europe Standard Time`, the safe choice) or one of the IANA
  names Graph lists for `dateTimeTimeZone.timeZone` (`Europe/Berlin`); Graph warns that a
  create or update may not take every zone it reads. The wall clock is
  `YYYY-MM-DDTHH:MM:SS` in that zone, given by the caller beside the UTC instant the store
  keeps (`start_utc` / `end_utc`; the agenda, windows and delta stay UTC): mailcore has no
  time zone database. Each end is independent (a flight). An all-day end's wall clock is
  `T00:00:00` of its local date.
- `graph::client::event_zone_error(zone, local, utc)` is the one rule: `None` for `""`;
  a named end needs a real wall clock in that shape, a store-shaped `utc`, and
  `local − utc` in Graph's range of zones, UTC−12 to UTC+14 (a wrong day or a swapped
  field, not a wrong DST offset). `create_local_event` and `update_event_fields` refuse
  what it refuses (`StoreError::Sql`, nothing written), and so does `UpdateEventSeries`
  (a `SyncEvent::Error` before the master is resolved, nothing queued). A host may call
  it first.
- `event_body_json`: each end it accepts is sent as `{"dateTime": <local>, "timeZone":
  <zone>}`, else the UTC wall clock with `timeZone: "UTC"` as before (lenient: a raw
  `EventInput` with a refused pair sends UTC, never an error). When the start is sent in
  its zone, the recurrence (either shape, with or without `TIMES` in the set) sends that
  zone as `range.recurrenceTimeZone`, over a `series_recurrence`'s own, since Graph reads
  `startDate` / `endDate` in it and `startDate` must match `start`; the create-side
  `Recurrence` gains the key in `event_body_json` (`with_range_time_zone`). A start sent
  in UTC sends the recurrence as before. With no zone named, the body is byte-for-byte
  what it was.
- Stored in `events.send_start_tz`, `send_start_local`, `send_end_tz`,
  `send_end_local` (internal, `''` by default, added by `Store::init`'s `ALTER TABLE`
  list, so an old row sends UTC). Not `original_start_tz`: that is Graph's report for a
  synced row, with no wall clock beside it. `upsert_event_on` does not write them, so a
  delta upsert keeps them, and nor does `reconcile_created_event`. A window or range
  replace (`replace_events_in_window`, `replace_calendar_range_on`) deletes synced rows
  and re-inserts them with `''`: harmless, as their times are Graph's again with them,
  and `local:` rows (whose `CreateEvent` reads the pair) are never deleted there.
  `event_for_send` reads them
  and `event_input_for` copies them, so a queued `CreateEvent` sends them, and a queued
  `UpdateEvent` when its set has `TIMES`.
- `update_event_fields`: `TIMES` is marked as before, only when `start_utc`, `end_utc`
  or `is_all_day` changed, never by a zone or wall clock alone: a form always has a
  zone and a synced row stores `""`, so a title edit would otherwise PATCH a meeting's
  times. Per end, a named pair is always written (a pending `CreateEvent` reads it); a
  `""` zone keeps the stored pair when `TIMES` is not marked and clears it when it is.
  So a `TIMES` send never carries a stale wall clock (another client moved a reconciled
  row): moving the times again names a new pair or clears it. Changing only the zone at
  the same instant is not sent by `UpdateEvent`; `UpdateEventSeries` sends what the host
  sets.
- Not done: detecting the machine's zone (the caller names it), lookxy or suite UI,
  reading events back in a zone other than UTC (`Prefer: outlook.timezone="UTC"` stays),
  writing the zone into the stored `graph_recurrence` (the next sync brings Graph's).
- Tests: `client.rs` `event_body_without_zones_is_unchanged` (pinned before the change),
  `event_body_sends_each_end_in_its_named_zone`,
  `event_body_series_recurrence_zone_is_the_start_zone`,
  `event_body_malformed_zone_pair_sends_utc`, `event_zone_error_checks_each_named_end`;
  `store` `calendar_tests` `create_local_event_stores_named_zones`,
  `create_local_event_refuses_a_malformed_wall_clock`,
  `update_event_fields_keeps_zones_when_times_unchanged`,
  `update_event_fields_zone_alone_does_not_mark_times`,
  `update_event_fields_clears_zones_when_times_move_without_one`,
  `update_event_fields_refuses_a_malformed_wall_clock`,
  `events_without_1983_columns_migrate` (the zones too); `outbox.rs`
  `queued_create_event_sends_named_zones`,
  `queued_update_without_times_sends_no_start_or_end`; `engine.rs`
  `update_event_series_sends_named_zones`,
  `update_event_series_refuses_a_malformed_wall_clock`.

## Create in a chosen calendar (#2048)

A host showing several calendars (#1984) files a new event in the one the
person selected.

- `LocalEventFields::calendar_id` (create-only): `None` or `Some("")` is the
  default calendar (its id, `''` before the list is known, as before), and
  `Some(id)` must be a stored calendar whose `can_edit` is set.
  `create_local_event` refuses any other (`StoreError::Sql`, with no
  `events` or `event_attendees` row written, before the row's upsert), and
  never files it in another calendar. With no list stored, every named id is
  refused. `update_event_fields` ignores the field: an edit never moves an
  event between calendars, and the field is never in the changed set.
- Where the POST goes is one rule, `Store::event_create_calendar(id)`: `None`
  (`/me/events`) when the row's `calendar_id` is `''` or the stored default's
  id, so every existing caller sends what it sent before, else `Some(id)`
  (`GraphClient::create_event_in`, `POST /me/calendars/{encode_path_segment(id)}/events`,
  the same body as `create_event`, and the returned event's `calendar_id` is
  the id: the URL decides, as in `calendar_view_in`). The default is read at
  drain time, so a `''` row a pass adopted since still goes to `/me/events`.
- Sync needed no change. `reconcile_created_event` keeps the row's
  `calendar_id`, and a pass replaces or deletes only rows of the calendar it
  fetched (`local:` rows never). So the default calendar's round neither
  drops nor claims the event, and its own calendar's round adopts it under
  the Graph id. Startup runs no calendar pass (only the tick does), so in the
  engine test the refresh after the create is an initial round per calendar
  and the next one a delta round.
- An editable calendar that does not sync (`synced_calendar_ids`) can be
  chosen. The event shows while it is `local:`, and the first pass after the
  reconcile deletes it (step 3), as its calendar is not shown.
- Known gaps: `can_edit` is not checked again at drain time. A calendar that
  turned read-only or vanished meanwhile gets a refused POST, then the usual
  retry and quarantine. Creating in another owner's shared calendar needs
  `Calendars.ReadWrite.Shared`, which the default scope lacks, so that POST is
  refused and quarantined and its `local:` row stays. Updates and deletes
  still address `/me/events/{id}`, which reaches only the user's own
  mailbox's calendars, not a shared calendar's event. No lookxy control to
  choose the calendar (lookxy passes `None`).
- Tests: `client.rs` `create_event_in_posts_to_the_calendars_events`;
  `store` `calendar_tests` `create_local_event_takes_a_chosen_calendar`,
  `create_local_event_refuses_an_unknown_or_read_only_calendar`,
  `event_create_calendar_is_none_for_blank_and_default`,
  `update_event_fields_ignores_calendar_id`,
  `create_local_event_takes_the_default_calendar` (unchanged); `outbox.rs`
  `create_event_in_a_chosen_calendar_posts_to_its_events` (and its edit
  PATCHes `/me/events/EV1`),
  `create_event_in_the_default_calendar_posts_to_me_events`,
  `create_event_encodes_the_calendar_id`; `engine.rs`
  `created_event_in_a_second_calendar_stays_in_it_through_both_refreshes`.

## Respond options and proposals (#2023)

Item 4 of #1985: every response can skip telling the organizer, carry a comment, and
propose a new time on tentative as well as decline. The organizer sees each attendee's
proposal.

- `SyncCommand::RespondEvent`, `SyncCommand::RespondMeeting` and `OutboxOp::RespondEvent`
  carry `send_response` ("Don't send a response" when false). The op's JSON key is
  `sendResponse`; a row queued before #2023 has none and reads as `true`, the only
  behaviour then. The local status (`set_event_response`) changes either way. With
  `sendResponse: false` Graph ignores the comment: documented, not refused.
- `graph::client::rsvp_proposal(kind, send_response, start, end)` checks a proposal and
  returns the window **normalised** to canonical UTC (`model::to_utc`), which is what the
  engine queues and sends. `Some("")` counts as absent. It refuses a proposal with an
  accept (Graph's `accept` takes none), with `send_response` false (Graph requires
  `sendResponse: true` to propose), with half a window, or with an end not after its
  start. `RsvpKind::from_status` maps the `response_status` words to a kind (it replaced
  `outbox.rs`'s private `rsvp_kind`).
- The engine runs the checks first, before it writes, queues or calls anything:
  `rsvp_proposal`, an unknown `kind` (`RespondEvent`), and a proposal on a stored event
  whose `allow_new_time_proposals` is false (a missing row skips this; Graph has the last
  word). A refusal is a `SyncEvent::Error`. A refused `RespondEvent` also owes the
  calendars a reset (`calendar_reset_owed`, then `refresh_if_calendar_reset_owed`),
  because a host such as lookxy writes the status itself before it sends the command
  (`lookxy/src/app.rs` `dispatch_rsvp`), on a row no delta round would name again
  (#2013). It is #2022's gated reset, not `reset_calendars`: its initial rounds wait
  until no event op with a local write is queued and no such command waits, so a
  deferred `UpdateEvent` (which reads its row at send time) still sends its edit.
  Signed out, it runs after the sign-in. In memory only, as #2022's: a restart before
  it ran drops it, and the host-written status stays until Graph changes the event.
  Graph's status comes back for rows in the kept window; a range-fetched row outside
  it (#2014) is corrected by its next range fetch once stale.
  A host that writes first should call `rsvp_proposal` before it writes. `RespondMeeting` checks `rsvp_proposal` before the
  invite's `meeting_event_id` GET and the stored row once the event id is known; it
  writes nothing locally, so it does not reset.
- `respond_event` sends `proposedNewTime`'s `dateTime`s without the `Z`, next to
  `timeZone: "UTC"`, as `event_body_json` and `get_schedule` do, and drops a proposal on
  accept as a second guard.
- The organizer's view: `Attendee`, `NewAttendee` and `AttendeeRow` carry
  `proposed_start_utc`/`proposed_end_utc` (canonical UTC, both `""` for none), from
  `attendees[].proposedNewTime`. Both are set only when `start.dateTime` and
  `end.dateTime` are both strings: `datetime_field_to_utc` on a missing side would read
  `0000-00-00T00:00:00Z`, and half a slot is no proposal. They are two `event_attendees`
  columns (the `ALTER TABLE` list beside `response_time`); `typed_attendees` keeps a
  matching attendee's proposal across a local attendee edit, as it keeps the response.
  The calendarView requests already ask for UTC and select every field, so no request
  changed.
- lookxy maps an invite's response with `RsvpKind::from_status`: an unknown one is an
  error notice and sends nothing, never an accept (`lookxy` test
  `an_unknown_invite_response_errors_and_sends_nothing`).
- Not yet: lookxy has no "Don't send a response" toggle, still offers a proposal on an
  event that disallows it (the engine refuses it and owes the reset), and does not show
  attendees' proposals. The organizer accepting a proposal is an `update_event` of the
  times.
- Every `Store` transaction is IMMEDIATE (`Store::write_tx`; all of them write). The
  engine and a host (lookxy, the engine tests) each hold a connection to one WAL file,
  and a DEFERRED transaction that read first and then writes after the other connection
  committed gets `SQLITE_BUSY_SNAPSHOT` ("database is locked") at once: rusqlite's 5 s
  busy timeout never waits that out. IMMEDIATE takes the write lock at `BEGIN`, which
  the timeout does wait for. This was the flaky `database is locked` in the engine tests
  on macOS and Windows CI (`store` test `writes_wait_out_another_connections_write`).
- Tests: `client.rs` `respond_event_tentative_with_proposed_time_sends_proposed_new_time`,
  `respond_event_accept_drops_a_proposed_time`, `rsvp_kind_from_status`,
  `rsvp_proposal_normalises_and_refuses`; `model.rs` `attendee_proposed_new_time`;
  `store` `op_json_round_trips_exactly`,
  `respond_event_queued_before_send_response_reads_as_sending`,
  `replace_events_in_window_writes_calendar_client_fields`,
  `update_event_fields_keeps_attendee_proposals`, `events_without_1983_columns_migrate`;
  `outbox.rs` `apply_op_respond_event_without_sending_a_response`; `engine.rs`
  `respond_event_without_sending_a_response`,
  `respond_event_sends_a_normalised_proposal_on_tentative`,
  `respond_event_refusals_write_queue_and_send_nothing`,
  `a_refused_respond_event_restores_graphs_status`,
  `a_refused_respond_event_resets_only_once_no_event_edit_waits`,
  `a_respond_event_refused_signed_out_resets_after_sign_in`,
  `respond_meeting_without_sending_a_response`, `respond_meeting_refusals_send_nothing`,
  `calendar_sync_stores_an_attendees_proposal`.

## Meeting messages (#2024)

Item 5 of #1985: a meeting message in mail (a request, a cancellation, a response) gives
its `meetingMessageType` and the event it refers to, and a cancellation can be removed
from the calendar without sending anything.

- `GraphClient::meeting_message(message_id)` GETs `/me/messages/{id}?$expand=
  microsoft.graph.eventMessage/event($select=id,isOrganizer)` and returns
  `model::MeetingMessage { message_type, event_id, is_organizer }`. The `eventMessage`
  cast covers `eventMessageRequest` and `eventMessageResponse`, which derive from it.
  There is no top-level `$select`, as in `meeting_event_id`: `meetingMessageType` is a
  derived-type property, which a plain GET returns. `RespondMeeting` keeps
  `meeting_event_id`'s narrower `eventMessageRequest` cast on purpose.
- `MeetingMessageType::from_graph` reads `meetingRequest`, `meetingCancelled`,
  `meetingAccepted`, `meetingDeclined` and `meetingTenativelyAccepted` (Graph v1.0's own
  spelling), plus the corrected `meetingTentativelyAccepted`. Graph's `none`, an unknown
  value and an absent one are `None`, never an error. `MeetingMessage::from_json` never
  fails either: a plain message is all `None` and `is_organizer: false`, and so is an
  `event` without `isOrganizer` (for the organizer flag) or with an empty id (for the id).
- On demand here; each row stores its type since #2081 (read from the message class, not
  this property: see "A message's meeting type on its row (#2081)"). The type is a
  derived-type property, and the delta `$select` (`MESSAGE_SELECT`) holds base `message`
  fields only; a type-cast select there would put every sync round at risk. The reader
  asks for the open message's banner with `SyncCommand::FetchMeetingMessage { message_id
  }`, which emits `SyncEvent::MeetingMessageFetched { message_id, meeting }` (and, since
  #2081, writes the type to the stored row). The stored `Message::is_meeting_request`
  (from `@odata.type`) still marks invites in the list.
- `SyncCommand::RemoveFromCalendar { message_id }` resolves the message when the remove
  runs. Its refusals are a `SyncEvent::Error` with nothing queued or sent: a message whose
  type is not `Cancelled` (removing a live meeting is a decline, or `DeleteEvent`), and an
  event the user organizes (the organizer's `DELETE` sends every attendee a cancellation).
  A cancellation with no `event` (removed already, here or elsewhere) sends nothing and
  emits `RemovedFromCalendar { message_id, event_id: None }`: the user's goal holds.
  Otherwise it queues `OutboxOp::DeleteEvent` **before** the local delete, as
  `CancelEvent` does, then runs `Store::delete_event_series(event_id)` (the event, or a
  cancelled series' master row and every stored occurrence whose `series_master_id` it
  is), emits `CalendarUpdated` and `RemovedFromCalendar { event_id: Some(..) }`, and
  drains. Graph gets `DELETE /me/events/{id}`, which for an attendee tells nobody; there
  is no `/decline` or `/cancel`. A failed local delete is an `Error`, and the rest still
  runs: the op is queued, and the next calendar round removes the row. A `DeleteEvent` of
  the same event already queued (a second remove while the first waits offline or in a
  back-off) is not queued again: Graph still holds the event, so both would resolve it,
  and the second DELETE would 404 after the first went out, which `DeleteEvent`'s drain
  retries into a quarantine. A queue that cannot be read is queued to. The host must not
  delete the row first. The command is in `command_writes_event_locally`, next to
  `CancelEvent`. Both commands start with the signed-in guard (`Error("not signed in")`,
  nothing sent), and a failed GET goes through `react`.
- Not done: lookxy shows no list icon for the stored type (#2081 stores it). The cancellation message
  stays in mail after a remove (Outlook moves it to Deleted Items; the host may delete
  it). A `DELETE` answered 404 (the event removed between the GET and the drain) keeps
  `DeleteEvent`'s drain policy. lookxy has no meeting banner: its `on_sync_event` takes
  both new events as no-ops.
- Tests: `model.rs` `meeting_message_type_maps_graph_values_and_is_lenient`,
  `meeting_message_reads_type_and_event`; `client.rs`
  `meeting_message_reads_cancellation_and_event`, `meeting_message_reads_a_response`,
  `meeting_message_of_a_plain_message_is_none`, `meeting_message_id_is_percent_encoded`,
  `meeting_message_missing_is_not_found`; `engine.rs`
  `fetch_meeting_message_emits_type_and_event`,
  `remove_from_calendar_deletes_the_event_without_sending`,
  `remove_from_calendar_removes_a_cancelled_series`,
  `remove_from_calendar_twice_while_queued_queues_one_delete`,
  `remove_from_calendar_refuses_a_message_that_is_not_a_cancellation`,
  `remove_from_calendar_refuses_the_organizer`,
  `remove_from_calendar_of_a_removed_meeting_sends_nothing`,
  `meeting_message_commands_signed_out_send_nothing`.

## Dismiss and snooze reminders (#2025)

Item 6 of #1985: a reminder dismissed or snoozed here is dismissed or snoozed on Graph,
so the user's other devices follow.

- `GraphClient::dismiss_reminder(id)` POSTs `/me/events/{id}/dismissReminder` with no
  body (`send` frames it as `Content-Length: 0`, #1954), and `snooze_reminder(id,
  new_reminder_utc)` POSTs `.../snoozeReminder` with
  `{"newReminderTime":{"dateTime":"2026-11-08T10:15:00","timeZone":"UTC"}}`
  (`graph_utc_wall_clock`). Graph answers `200` with no body; the id is percent-encoded.
- `SyncCommand::DismissReminder { id }` and `SnoozeReminder { id, new_reminder_utc }`
  queue `OutboxOp::DismissReminder` / `SnoozeReminder` (kinds `dismissReminder`,
  `snoozeReminder`; keys `id`, `newReminderUtc`) and drain, so an action taken offline or
  signed out goes out later. `id` is the event the host showed the reminder for: for a
  series, the occurrence id `calendarView` lists. Nothing tests whether Graph takes a
  series master's id here.
- Refusals are a `SyncEvent::Error` with nothing queued: a `local:` id (not on Graph,
  and a queued op's id is never retargeted when `CreateEvent` reconciles it), and a
  snooze time that, after `to_utc(t, "UTC")`, `iso8601_to_unix_secs` cannot read.
  `to_utc` never fails: text that is not a time comes back malformed (padded, not
  parsed), hence the check that the result reads as a time. The time
  is UTC: `strip_offset` drops a `+02:00` without converting it, as for RSVP proposals.
  A snooze queued offline goes out even if its time has passed by then.
- Dedupe: an op equal to the **last** reminder op queued for the same id is not queued
  again (a second Dismiss while the first waits, the same snooze time twice). Only the
  last counts: with `[Dismiss, Snooze]` queued, a new Dismiss is queued, or the snooze
  would land last and bring the reminder back.
- No local write and no `CalendarUpdated`: the store keeps only `is_reminder_on` /
  `reminder_minutes`, and what Exchange does to them comes back with the next calendar
  round. Neither op is in `writes_event_locally`, so nothing resets the calendars.
- Drain: a 404 drops the op silently (the event, and its reminder, are gone). Any other
  refusal but a 5xx (a 400, a 403, a parse error) drops it at once with one
  `Error("dismiss reminder {id}: {err}")` (`snooze reminder` for a snooze): no retry, no
  quarantine, as a quarantine's mail reconverge re-enumerates the whole mailbox for an
  op that wrote nothing. A 5xx keeps the outbox back-off; 401, throttle (429, 503) and
  transport keep their central handling.
- Not done: host UI (lookxy's Esc on the reminder banner only pops it; there is no
  snooze, and the suite has no Mail reminders), and `GET /me/reminderView`, without
  which a snooze made on another device does not move this device's reminder.
- Tests: `client.rs` `dismiss_reminder_posts_no_body`, `snooze_reminder_posts_new_time`,
  `reminder_ids_are_percent_encoded`, `dismiss_reminder_refused_is_an_error`; `store`
  `outbox_op_dismiss_and_snooze_round_trip`; `outbox.rs`
  `dismiss_reminder_posts_dismiss_reminder`, `snooze_reminder_posts_snooze_reminder`,
  `reminder_ops_local_id_make_no_request`; `engine.rs` `dismiss_reminder_posts_to_graph`,
  `snooze_reminder_posts_a_normalised_time`,
  `reminder_commands_refuse_local_id_and_a_bad_time`,
  `reminder_op_404_is_dropped_without_error`, `reminder_op_400_is_dropped_with_one_error`,
  `dismiss_reminder_twice_while_queued_queues_one`,
  `dismiss_after_a_queued_snooze_is_queued_again`.

## OAuth tokens in the platform secret store (#1877)

- `tokencache` keeps the `TokenSet` in the platform's secret store when one is reachable: the
  Secret Service (`org.freedesktop.secrets` over the D-Bus session bus, through `zbus`'s pure-Rust
  blocking client, a Linux-only dependency) on Linux, a generic password item in the default
  Keychain (`security-framework`, already in the macOS tree) on macOS. Windows has no store: its
  DPAPI file is unchanged. The file at `path` then holds only a reference,
  `{"secret_store":"secret-service","key":"<abs path>","account":"me@x"}`, written like any cache
  file (atomic, created 0600). `load` tells it apart by the `secret_store` key and reads the item
  under the file's `key`; a reference without `key` is `InvalidData`.
- The key is the cache file's absolute path (`std::path::absolute`), not the account: one file has
  one item, and signing in as someone else replaces it. Linux attributes: `xdg:schema =
  mailcore.TokenCache`, `application = mailcore`, `cache = <key>`, label `Mail sign-in for
  <account>`, created in the `default` collection with `replace`, then any match outside that
  collection deleted: only unlocked ones and never through a prompt (best effort: a failure
  there does not fail the save, and a locked twin does not raise a prompt on every save;
  sign-out removes it). Locked items are unlocked before they are read or deleted:
  gnome-keyring refuses `Item.Delete` on a locked item (`IsLocked`). `delete` removes the
  unlocked matches first, then unlocks and removes the locked ones, trying everything and
  returning the first error, so a locked twin's refused prompt cannot keep the live token. macOS: service `mailcore`, account `<key>`, no label.
- The core is store-generic (`save_in` / `load_in` / `clear_in` over a `SecretStore`), which the
  tests drive with a fake; `save` / `load` / `clear` keep their signatures and pass
  `platform_store()`, and `save_reporting` / `load_reporting` also say where the token went
  (`Storage::SecretStore`, `EncryptedFile`, `PlainFile`). `platform_store()` is `None` on Windows,
  under `cfg(test)` and with `MAILCORE_TOKEN_STORE=file`. `cfg(test)` only covers mailcore's own
  unit tests: a host's tests that write a token through mailcore set `MAILCORE_TOKEN_STORE=file`,
  as CI's `test` and `coverage` jobs do (a macOS Keychain prompt would hang a leg).
- `SecretStoreError` (not `StoreError`, which is `mailcore::store`'s) is `Unavailable` (no bus,
  no Secret Service, no default collection, no keychain: cheap, not remembered), `Refused` (the
  unlock prompt dismissed or unanswered within `secret_service::PROMPT_TIMEOUT`, 60 s, after
  which it is dismissed; or a D-Bus method call unanswered within `METHOD_TIMEOUT`, 25 s, set on
  the connection, as zbus waits forever by default) or `Failed`. The prompt wait races an
  `async_io::Timer` on the calling thread (`futures-lite`'s `block_on`; both crates were already
  in zbus's tree), so nothing outlives the call. The platform store is one process-wide
  `Latched` store: after the first `Refused`, every `get` and `set` is `Unavailable` without
  asking, so an hourly refresh does not raise the prompt again. `delete` bypasses the latch:
  sign-out is the user's request, and a token stored before the refusal would otherwise stay in
  the store with no file pointing at it. The trait, the `_in` core, `Latched` and
  `platform_store` are `pub(crate)`.
- Save: a store that takes the JSON gets a reference written; any store error falls back to the
  token file (0600 plaintext off Windows), never a save error. A reference that cannot be written
  after the store took the token is the write's error, and the old file stays. A reference that
  named another key (a moved directory, another path to the cache) has that item deleted after
  the new reference is written, errors ignored, unless the file at that key still refers to it:
  a copied cache directory's original still uses its item. Checked after the write, so the same
  file reached under another name now names the new key and the old item goes.
- Load: no file is `Ok(None)` with no store call. A reference whose item is gone is `Ok(None)`; no
  store or an unavailable/refused one is `io::ErrorKind::NotConnected`, `secret store unavailable:
  <reason>`, and the reference stays (a locked keyring may open next time); a store failure is its
  own error. A token file is migrated when a store takes it: stored, then replaced by the
  reference (atomic rename; the old blocks are not zero-filled), the `.tmp` removed. If the store
  does not take it, or the reference cannot be written, the file stays and reports `PlainFile`.
- Clear (sign-out): removes the file and the `.tmp`, then with a store deletes the item under the
  computed key and under the reference's key when it differs, whatever the file held: a fallback
  token file may sit over a reference whose item is still stored. The first error is returned
  after everything was tried: an unreachable store is reported even over a token file, as an item
  may be there (`NotConnected`). The engine's sign-out message (`sign_out_error`) is the old one
  when the file is still there; with the file gone, "could not reach the secret store to delete a
  sign-in saved there" for `NotConnected` (no claim that an item exists), and "… from the secret
  store …; it stays there" for a store that failed.
- `tokencache::account(path)` reads the account from the file (reference or token file) with no
  store call. lookxy's `reload_account` uses it: it runs on the UI thread after every sync pass,
  where a keyring prompt must not block.
- `SyncEvent::TokenStoredUnencrypted`: sent at most once per engine run (`warned_unencrypted`,
  kept over sign-out), the first time the startup load, a refresh's save or the sign-in's save
  ends in `PlainFile`; never on Windows. At startup it comes right after the load, before the
  near-expiry refresh. lookxy ignores it. A startup load that fails with `NotConnected` emits
  `Error("secret store unavailable: …")` before `SignInRequired`; any other load error stays silent.
- macOS ties a Keychain item's access to the code identity that created it: an unsigned rebuild
  asks once whether it may read the item. That prompt blocks the engine thread, not the UI.
- `zbus` 5 pulls `uuid`, whose 1.27+ needs rustc 1.89: `Cargo.lock` pins `uuid` 1.26.1 for MSRV
  1.88. A `cargo update` that lifts it breaks the `msrv` job.
- Tests: the fake-store tests in `tokencache::tests` (save, load, migrate, clear whatever the file
  holds, an unreachable store on clear, fallback per error, a gone item, an unreachable store, a
  reference without key, re-save under another account, a save over a reference to another key,
  a copied cache and a cache under another name,
  `account()`, the latch, sign-out past the latch, a reference that cannot be written: skipped
  where a read-only directory is still writable, as root); `secret_service::tests::
  errors_are_classified`; engine tests `a_plaintext_token_warns_once_per_run`,
  `a_sign_in_saved_to_the_file_warns`, `a_reference_without_its_store_says_why_sign_in_is_needed`,
  `sign_out_errors_say_what_was_left`. The real store:
  `tokencache::secret_service::tests::real_roundtrip`, which runs only with
  `MAILCORE_SECRET_SERVICE_TEST=1`, in CI's `test (ubuntu-latest)` step "Token cache in a real
  Secret Service": `gnome-keyring dbus-daemon`, then under `dbus-run-session` with a throwaway
  `XDG_DATA_HOME` and `XDG_RUNTIME_DIR` (the latter also keeps `--unlock` off a desktop's running
  daemon) and `DISPLAY` / `WAYLAND_DISPLAY` unset (run on a desktop, the locked-keyring step
  would otherwise put a real unlock prompt on the screen), `echo -n ci-pass | gnome-keyring-daemon --unlock --components=secrets`. It also
  locks the collection (`Service.Lock`) and signs out: headless, gnome-keyring dismisses the
  unlock prompt at once (no prompter), so the test checks the sign-out asked to unlock
  (`NotConnected`, "dismissed", where a bare `Delete` fails `IsLocked`), unlocks with the
  password through `org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface.
  UnlockWithMasterPassword`, and signs out again: the item is gone. An answered prompt is not
  tested. The Keychain backend has no test; the macOS leg builds and lints it.

## A moved message follows its new id (#1896)

- Graph mints a new id on every move. After a successful `OutboxOp::Move`,
  `drain_outbox` drops the op and then calls `Store::follow_move(old, new)`
  with the id `move_message` answered (`outbox::Applied::Moved`). That
  re-keys the row, its body, `message_headers`, `attachments` (which
  `reconcile_id` never renames), `outbound_attachments`, and FTS through
  the triggers. The engine then emits `SyncEvent::MessageMoved { old_id,
  new_id }` once per move Graph made. Nothing happens for the same id or an
  empty one. A message no longer stored (deleted locally while its move
  was in flight) has no row to re-key, but its alias is recorded and the
  event emitted all the same: a `Delete` by the old id, queued behind the
  move or sent after it, must reach the new id, not 404 into a quarantine
  whose reconverge would bring the message back. A store failure emits an `Error`, and the op
  still counts as done: Graph already moved the message, so a retry would
  POST to a dead id. `apply_op` itself never writes the move to the store.
- The old ids live in `moved_ids (old_id PK, message_id)`, a separate
  table and not `messages.local_id`: one row can have many of them, and
  `draft_row_sql!` must not match them. It has no foreign key on purpose:
  an alias outlives its row (for that `Delete`), so its rows are never
  pruned, one per move. The chain is kept flat: A -> B -> C stores
  A -> C and B -> C. `follow_move` renames the row, or, when a row `new`
  already exists (a delta stored the moved copy first), moves the old
  row's children (`UPDATE OR IGNORE`: what the new row has stays) and
  `local_id` (off the old row first, as it is UNIQUE) onto it and
  deletes the old row. Then it repoints the chain, drops an alias named
  `new` (an alias never names its own row) and records `old -> new`.
- Only `Store::current_id` follows moves: the `local:` lookup, then
  `moved_ids`, so a reconciled draft that was then moved resolves too.
  `draft_row_sql!`, `current_id_sql!` and the store writes built on them
  (`delete_message`, `delete_message_resolved`, `set_read`, `set_flag`,
  `set_categories`, `move_message`) match the id only. The delta
  `@removed` path uses `delete_message`, so the source folder's
  `@removed` of the old id deletes nothing. Do not make those writes
  follow aliases.
- The engine's `MarkRead`, `SetFlag`, `SetCategories`, `Move` and `Delete`
  arms resolve with `current_id` first, and `apply_op` resolves the
  queued op's id again at apply time. A queued `Move(old, B)` followed by
  `Move(old, A)` therefore posts the second move to the id the first one
  returned. `pending_message_ids` resolves too, so an op queued under the
  old id still protects the re-keyed row from a fresh delta.
- `merge_delta_items` skips an upsert of an alias id
  (`Store::is_moved_alias`, a PK probe) before `protect` and the
  new-mail bookkeeping. Graph never reuses an id, so such an upsert is
  the pre-move copy Graph has not removed yet. The destination's upsert
  of the new id merges into the re-keyed row. No other upsert site needs
  this: the header repair fetches by the stored id, and `store_draft`
  writes a fresh draft.
- lookxy writes its optimistic changes on its own connection with the id
  it holds (`control.rs` `set_read`/`set_flag`, `app.rs` `move_message`,
  `delete_and_send`). With an old id those writes change zero rows. The
  command it sends next is then resolved by the engine arm, which writes
  the row: correct, only late. On `MessageMoved`, lookxy (`follow_moved`)
  re-keys `selected_msg` and the move picker and attachments popup it has
  open, and then re-reads the list.
- `OutboxOp::Delete` stays `DELETE /me/messages/{id}`: Graph soft-deletes
  the message to Recoverable Items, not to Deleted Items. A client that
  wants "delete moves to Deleted Items" sends `Move` to that folder.
- Not followed: `FetchBody`, `FetchAttachments`, `SaveAttachment`,
  `SaveItemAttachment`, reply/forward and the meeting commands take the id
  as given, since the host re-keys on `MessageMoved`. Re-keyed
  `attachments` rows keep the pre-move attachment ids until the next
  `FetchAttachments`. `moved_ids` rows are never pruned.
- Tests. Store (`moved_id_tests`): `follow_move_rekeys_the_row_body_headers_and_attachments`,
  `follow_move_does_nothing_for_the_same_or_an_empty_id`,
  `follow_move_of_a_message_no_longer_stored_still_records_the_alias`,
  `follow_move_chain_resolves_every_old_id_to_the_newest`,
  `follow_move_onto_an_existing_row_keeps_one_row`,
  `follow_move_onto_an_existing_row_keeps_the_drafts_own_state`,
  `follow_move_onto_an_existing_row_keeps_that_rows_own_body`,
  `follow_move_back_to_an_earlier_id_drops_that_alias`,
  `follow_move_of_a_reconciled_draft_keeps_its_local_id`,
  `delete_message_by_a_moved_alias_deletes_nothing`,
  `moved_aliases_outlive_a_deleted_message`, `pending_message_ids_resolves_moved_ids`.
  Outbox: `apply_op_move_returns_the_new_id`,
  `apply_op_mark_read_of_a_moved_id_patches_the_new_id`,
  `apply_op_move_of_a_moved_id_posts_to_the_new_id`. Engine
  (`tests::moved`, with ids that are not prefixes of one another, since
  `FakeServer` matches by prefix): `move_back_by_the_old_id_lands_in_the_source`,
  `mark_read_by_the_old_id_after_a_move_reaches_the_new_id`,
  `queued_moves_by_the_old_id_drain_in_order`,
  `a_delta_after_a_move_keeps_one_row_under_the_newest_id`,
  `set_flag_and_delete_by_the_old_id_reach_the_new_id`,
  `a_delete_queued_behind_a_move_reaches_the_new_id`,
  `a_delete_sent_after_the_move_drained_reaches_the_new_id`, and
  `move_command_optimistically_refiles_locally`. lookxy:
  `message_moved_rekeys_the_open_message_and_the_list`.

## Server search and opening a hit (#2006)

- `GraphClient::search_messages(folder_id, query, top)`: `GET
  /me/messages?$search=…&$select=MESSAGE_SELECT,parentFolderId&$top=…&$expand=MESSAGE_EXPAND`,
  or `/me/mailFolders/{id}/messages?…` with a folder (that folder only, not
  its subfolders). `query` is KQL as the client wrote it: `search_value`
  puts it in double quotes, escapes its `\` and `"` with a backslash so it
  cannot end the clause, and percent-encodes the whole value with
  `encode_path_segment`. Graph allows neither `$orderby` nor `$filter` with
  a message `$search` as query options, so the hits come in Graph's order;
  neither is sent (the expand's own `$filter` sits inside its parentheses),
  nor a `Prefer: odata.maxpagesize`. Since #2081 the first request carries
  the expand through `get_with_expand`, for each hit's reminder and meeting
  type: a 400 sends it once more without it and marks the hits
  `reminder_unasked`, so bad KQL now costs two requests (see "A message's
  meeting type on its row (#2081)"). `top` is clamped to `1..=SEARCH_TOP_MAX`
  (1000, Graph's cap; `0` asks for one). `@odata.nextLink` is sent as is
  (through `full_url`, like a delta link) until `top` hits are in (the rest
  truncated), a page has no link, or a page brings no hit (a looping link;
  an item without an id is no hit).
- `SearchHit { folder_id, message, fields }` (`graph::model`) is what a
  delta upsert carries plus `parentFolderId` (empty when Graph sent none).
  `SearchHit::from_json` drops an item without an id: `Message::from_json`
  never returns `None`, so it checks the id itself.
  `GraphClient::get_message_hit(id)` is `get_message` with
  `parentFolderId` selected, and a body without an id is a `Parse` error.
  `get_message` keeps its URL for the header repair.
- `SyncCommand::SearchMessages { query, folder_id, top }` answers with
  `SyncEvent::MessageSearchResults { query, folder_id, hits }`, echoing
  the query and folder so a client can drop a stale answer. No matches is
  an empty `hits`. The hits are not stored. A failure is
  `SyncEvent::MessageSearchFailed { query, folder_id, reason }` (one line),
  never `SyncEvent::Error`. Signed out ("not signed in") and a blank query
  ("empty query") send no request. Auth, throttle and transport failures
  also go through `react` (sign-in, `next_retry`, Offline), which emits no
  `Error` for them. `Engine::react_quietly` is `react` for those classes and
  nothing for the rest. Like `FetchBody`, neither command waits out a
  back-off: a user's search or click is tried.
- `SyncCommand::FetchMessage { id }` opens a hit the store lacks, in this
  order:
  - Refused with no request: signed out; an id a move left behind
    (`is_moved_alias`, "moved; open its current id"); and an id a queued op
    owns (`protected_ids`) whose row is gone ("deleted locally": a queued
    `Delete` is never undone).
  - Then `get_message_hit`. A folder that is empty or not in
    `Store::folders()` writes nothing.
  - The header is stored with `merge_message_unless_edited` in the hit's
    folder, as a fresh delta row is, under the id asked (as
    `repair_headerless` does).
  - A protected id whose row exists keeps its row (no merge: the queued
    op's optimistic state and folder stay). So does a draft holding an
    unsaved edit: `merge_message_unless_edited` returns `Ok(false)`, and
    only the row's own folder gets `MessagesUpdated`.
  - The row's old folder is read first (`Store::message_folder`). When the
    merge re-filed it, `MessagesUpdated` goes out for the old folder, then
    for the row's folder.
  - Then the body. A row stored before whose body satisfies the engine's
    format (`get_body_for`, which a draft's local body always does, #1901)
    keeps it: `BodyReady` with no request. A fetch would put Graph's older
    body over a compose edit, and a queued `SaveDraft` would then push
    that. An edited row with no such body is refused ("edited locally").
    Any other row is fetched via `fetch_and_store_body` (`BodyReady`, the
    #1933 headers). A body failure after the header still answers
    `MessageFetchFailed { id, reason }`, and the header stays stored.
- A later delta or backfill of the folder upserts the same row (same PK), and
  a stored-link round does not count it as new mail (`message_exists`), so
  its body is not fetched again. Messages are never pruned by the window, so
  a hit older than the cutoff stays.
- Both commands are served at `Engine::checkpoint`, like `FetchBody`, so a
  search during a pass waits only for the request in flight. A served
  command's auth, throttle or transport failure stops the work.
- A client dedupes server hits against its local ones (`Store::search`).
  Pass each hit's id through `Store::current_id` first: a pre-move alias
  would otherwise be listed beside its local row and could not be opened.
- lookxy never sends the two commands. Its `on_sync_event` ignores the
  three events.

## Message rows by id (#2053)

- `Store::message_row(id)` is a stored message's `MessageRow`, the row
  `messages_in_folder` gives for it, whether or not a body is stored
  (`Store::draft` joins `bodies`, so it is only for a row with one). `None`
  when no row has the id.
- `Store::message_rows(ids)` is the batch a client uses to check up to a
  few hundred server-search hits against the store: the rows of the ids it
  has, in no particular order, each once; unknown ids are skipped and an
  empty slice runs nothing.
- Both take exact current ids, like `message_exists` and `message_folder`:
  no `local:` or moved-id resolution. Pass a hit's id through
  `Store::current_id` first.
- The batch is one statement whose one parameter is the ids as a JSON
  array (`WHERE id IN (SELECT value FROM json_each(?1))`, as
  `retain_folders` passes a list), not `IN (?, ?, ...)` chunked under
  SQLite's variable limit: one statement is one read a concurrent writer
  cannot shift, with no limit and no chunking. SQLite plans it as one
  primary-key lookup per id; `message_rows_reads_messages_by_its_primary_key`
  asserts that plan (`SEARCH messages ... (id=?)`, no `SCAN messages`). The
  store never runs `ANALYZE`, whose stats could change it.
- `MESSAGE_ROW_COLUMNS` is `map_message_row`'s column list, used by these
  two, `messages_in_folder` and, since #2081, `search`, `draft` and
  `due_message_reminders`; only `conversations_in_folder` still spells it
  out (twice: its `keyed` CTE and its outer select).

## A flagged message's reminder (#2054)

Outlook's "Follow Up › Add Reminder" on a message: read, stored, snoozed and dismissed, as
#2025 does for events.

- Where it lives (checked on learn.microsoft.com's Outlook MAPI pages, which cite
  MS-OXPROPS, 2026-11-09): Graph v1.0 `message` has no reminder property, and
  `followupFlag` only `flagStatus` and its dates. Outlook keeps it in PSETID_Common
  (`{00062008-0000-0000-C000-000000000046}`) named properties, which Graph exposes as
  `singleValueExtendedProperties`: `PidLidReminderSet` (LID `0x8503`, Boolean, on),
  `PidLidReminderTime` (`0x8502`, SystemTime, what it was set for) and
  `PidLidReminderSignalTime` (`0x8560`, SystemTime, when it fires next, moved by a
  snooze). Their Graph ids are `model::REMINDER_SET_PROP`, `REMINDER_TIME_PROP` and
  `REMINDER_SIGNAL_TIME_PROP`, one source for the parse and the PATCH.
- Read: `client::MESSAGE_EXPAND` (`$expand=singleValueExtendedProperties($filter=id eq
  … or …)`, quotes and braces percent-encoded) goes on a delta's first call (`Head`,
  `Full`), `get_message` and `get_message_hit`, and since #2081 (which adds the message
  class to it) on a search's first request too.
  Graph keeps a first call's options in every link it mints, so a `Link` is sent as is.
  Nothing showed delta accepts this exact expand, so a 400 on it (`get_with_expand`)
  sends the call once more without it, and the folder still syncs. A 400 with another
  cause costs that one extra request.
- A refused expand reads no reminders and keeps the stored ones: what the fallback
  returns is `MessageFields::reminder_unasked` (`writes_reminder` is false unless the
  item carries the key; `flag_due_at` still follows the flag). A delta first call that
  fell back sets `DeltaPage::expand_refused`, and every link Graph mints from it lacks
  the expand too, so the refusal belongs to the walk, and the mark follows the first
  call of the link stored as `delta_link`:
  - a head or small-folder walk (`run_delta`) keeps its first page's refusal in memory
    for its nextLink pages (`DeltaRun::expand_refused`), and stores it, 1 or 0, in
    the same statement as its link (`folders.reminder_expand_refused`: a head's
    `set_head_link(.., refused)`, a small folder's `complete_backfill(.., Some(refused))`).
    Written apart, a reader between the two (a test on Windows, every run) or a crash
    saw the new link with the old mark (#2054 r2, fixed in #1941);
  - a backfill walk spans idle steps, so its first page's refusal is kept next to
    `backfill_link` (`folders.backfill_expand_refused`) for its later pages, and
    `complete_backfill(.., None)` moves it to the folder's mark with its link; `reset_backfill`
    drops it;
  - a round on the stored link merges as that link's mark says.
  An accepted first page never clears the mark while the refused walk's link is still
  `delta_link`. `clear_delta_links` clears both (the migration adds them at 0); the next
  first call then asks again, with no retry loop of its own.
- Parse (`model::parse_message_reminder`), lenient and never an error: property ids
  match ASCII-case-insensitively (Graph may lower-case the GUID); on is only `"true"`
  (any case) or JSON `true`; a time is read only when it ends in `Z` or has nothing
  after its seconds (a fraction allowed): `to_utc` drops a `+02:00` without applying it.
  `is_reminder_on` needs a readable time too (one with none cannot fire). Outlook's
  "never" signal, `4501-01-01T00:00:00Z`, parses and is never due. `flag_due_at` is
  `flag.dueDateTime` in UTC, `""` in a named zone (no zone database here).
- Store: `messages.is_reminder_on`, `reminder_at`, `reminder_signal_at`,
  `flag_due_at`, on `Message` and `MessageRow` (there is no `MessageSummary`). A merge
  writes the reminder columns when `MessageFields::writes_reminder` (the key carried, or
  a complete item: Graph leaves the key out when the filter matches nothing) and
  `flag_due_at` with the flag. **Reminder on ⇒ flagged**: the merge ANDs it with the
  row's resulting flag, and `set_flag(false)` turns it off (times kept), so an unflag or
  a completed flag stays off even if Exchange keeps `PidLidReminderSet` and a later
  delta says so. Since #2089 a meeting message's own `PidLidReminderSet` is kept apart,
  in `meeting_reminder_on`, which never fires; `is_reminder_on`, this rule and
  `due_message_reminders` are unchanged (see "A meeting message's own reminder on its
  row (#2089)").
- Migration: `migrate_message_reminders` probes `pragma_table_info` in one IMMEDIATE
  transaction; a store without the columns gets all four and every folder's delta and
  backfill state cleared (what `clear_delta_links` clears, without
  `reconverge_pending`), as a pre-#2054 link never carries the expand. A crash cannot
  leave the columns without the reset; a second connection waits and finds them. The
  cost is one re-enumeration of the mailbox: fresh deltas report no row as new mail
  (`new_ids` is filled only on a stored link's round), and a stored body is not fetched
  again. `idx_messages_reminder` (partial, `WHERE is_reminder_on = 1`) comes after.
- `Store::due_message_reminders(now_utc)`: flagged rows, any folder, reminder on, whose
  `COALESCE(NULLIF(reminder_signal_at, ''), reminder_at)` is not empty and not after
  `now_utc`, soonest first then by id (`due_message_reminders_sql`, which the plan test
  shares). `now_utc` is read by `reminder_time` too; one it cannot read is a
  `StoreError::Invalid`, not an empty list. A client polls it.
- Commands: `SyncCommand::DismissMessageReminder { id }` and `SnoozeMessageReminder {
  id, new_reminder_utc }` resolve `id` (`current_id`), refuse a `local:` id and a snooze
  time `model::reminder_time` cannot read (as Graph's are: an offset is refused, not
  dropped) (`Error("dismiss message reminder {id}: the
  message is not on the server yet")`, `"snooze message reminder {id}: not a UTC time:
  …"`), write the store (`Store::dismiss_message_reminder`, `snooze_message_reminder`)
  and queue `OutboxOp::DismissMessageReminder` / `SnoozeMessageReminder` (kinds
  `dismissMessageReminder`, `snoozeMessageReminder`; keys `id`, `newReminderUtc`). They
  are message ops: in `pending_message_ids`, and resolved again when they drain, so they
  follow a move. The PATCH is `/me/messages/{id}` with
  `{"singleValueExtendedProperties":[{"id":"Boolean {…} Id 0x8503","value":"false"}]}`, or
  the `0x8560` SystemTime with the snooze time. No new `SyncEvent`, as for `SetFlag`.
- Dedupe is #2025's, keyed by `ReminderTarget`: an event's id, or a message as it is
  stored now, so one queued before a move matches one sent after it, and an event op
  never matches a message op of the same id.
- Drain is #2025's (404 silent, any other non-5xx refusal dropped with one `Error`, no
  quarantine), with one difference, as these ops wrote the store: after such a refusal
  other than a 404 `reread_message` GETs the message and merges it
  (`merge_message_unless_edited`) into its row's folder, as no delta re-reports a
  message Graph never changed. A row another queued op still names (`protected_ids`: a
  `SetFlag` behind the refused op, say) is not re-read, so that op's optimistic write
  stands until it drains. A failed re-read is ignored.
- `testserver` is a `#[doc(hidden)] pub mod`, so a crate built on mailcore can drive the
  whole path in its tests: `FakeServer::start` with routes for the delta and the
  `PATCH /me/messages/`, then `GraphClient::new(&srv.base_url, …)`, or the engine through
  `sync::engine::spawn_with_bases_and_options` (`#[doc(hidden)] pub`, the test seam;
  hosts use `spawn` / `spawn_with_options`, which hard-code Graph) with `srv.base_url`
  as both bases, then `srv.requests()` for the recorded PATCH.
- Not done: setting a new reminder, task (To Do) reminders, host UI, and
  `GET /me/reminderView`.
- Tests: `model.rs` `message_reminder_parses_the_three_properties`,
  `message_without_reminder_properties_has_none`, `message_reminder_false_is_off`,
  `message_reminder_with_garbage_times_is_off`,
  `message_reminder_with_an_offset_time_is_unreadable`,
  `flag_due_date_time_is_normalised_utc`, `flag_due_in_a_named_zone_is_empty`,
  `message_fields_reminder_written_only_when_carried_or_complete`; `client.rs`
  `message_expand_names_every_property` (#2096's name), the delta / `get_message_hit`
  URL tests, `get_message_expands_the_reminder`, `delta_link_is_sent_as_it_is`,
  `delta_head_retries_without_expand_on_400`,
  `delta_full_retries_without_expand_on_400_only`,
  `get_message_retries_without_expand_on_400`,
  `get_message_hit_retries_without_expand_on_400`, `get_message_reads_the_reminder`,
  `dismiss_message_reminder_patches_reminder_set_false`,
  `snooze_message_reminder_patches_signal_time`, `message_reminder_refused_is_an_error`;
  `store` `message_reminder_tests` (round trip through every reader, partial and
  complete merges, a merge read without the reminder, the flag invariant, `set_flag`,
  the due query, its index and a bad `now`, the refused-expand marks and their move with
  the backfill's link, the migration, the
  op round trip, `pending_message_ids`) and
  `upsert_writes_every_column`; `outbox.rs`
  `dismiss_message_reminder_patches_the_resolved_id`,
  `snooze_message_reminder_patches_the_resolved_id`,
  `message_reminder_op_of_a_local_id_makes_no_request`; `engine.rs`
  `tests::message_reminders` (delta store and clear, a refused expand keeping reminders
  through a link round, the migrated store's re-enumeration, dismiss, snooze, refusals
  with an offset time, 404, 400 with the re-read, no re-read of a row a queued op owns,
  dedupe, unflag), `tests::backfill` `an_accepted_backfill_link_unmarks_a_refused_head`,
  `a_refused_backfill_keeps_reminders_and_leaves_an_accepted_head_unmarked` and `tests::moved` `message_reminder_ops_queued_behind_a_move_reach_the_new_id`,
  `message_reminder_commands_by_the_old_id_write_and_reach_the_new_id`,
  `a_dismiss_by_the_new_id_dedupes_against_one_queued_by_the_old_id`.

## Occurrences take their series' fields (#2064)

Graph's `calendarView/delta` (#2013) returns each recurring occurrence with only its
`id`, `type`, `start`, `end` and `seriesMasterId`; the series' fields live on the
`seriesMaster`, which the delta returns too. Stored as returned, every occurrence
was a blank meeting, and the grid queries drew the masters as meetings on each
series' first day.

- **The fill, at write time.** `apply_calendar_sync` runs `fill_occurrences_on` for
  each fetched calendar after its removals and upserts (`Window` and `Delta`, cascade
  or not), in the pass's transaction. A non-`local:` `occurrence` of that calendar
  whose master is stored there as a `seriesMaster` is filled when the round upserted
  it, or upserted its master and the master changed, or it is blank (`BLANK_OCCURRENCE`: `subject`, `organizer_name`, `organizer_addr`,
  `location` and `response_status` all `''`; every real event has an organizer).
  The blank rule repairs a store from before #2064 on its next pass, with no
  download. An occurrence that neither the round nor its master touched keeps its
  own values: an optimistic local write (a `RespondEvent`'s status, an `UpdateEvent`
  of this occurrence) waits for its op. A changed master is server truth and
  overwrites, as a delta upsert does. "Changed" (`changed_masters_on`, read before
  the round's writes; r3): not stored as a master of that calendar, or stored with
  another `last_modified` (Graph's `lastModifiedDateTime`), or either one empty.
  Every initial round lists or refetches the masters of its occurrences, so without
  this an unchanged master would refill every occurrence of its series, of any date,
  daily, overwriting a pending write on one outside the window (a range fetch's, or
  history); the round's own occurrences are still filled, as touched ones.
  Exceptions are full items and never filled.
- What is copied: `SERIES_FIELDS` (subject, location, organizer, response status,
  body preview, `is_all_day`, reminder, show-as, online meeting and join URL,
  categories, sensitivity, importance, `is_cancelled`, `is_organizer`,
  `response_requested`, `allow_new_time_proposals`, `has_attachments`, the original
  zones), and the master's attendees replace the occurrence's. The fill runs after
  `upsert_fetched_events_on`, which writes each minimal occurrence's own empty
  attendee list. `is_all_day` matters: a minimal occurrence of an all-day series
  reads `false`, and `days_with_events` would shift it by the offset. Not copied:
  id, times, `series_master_id`, `event_type`, `calendar_id`, `web_link`,
  `last_modified`, both recurrences, the body and the internal columns.
- **The body is a reference.** `event_body_on` gives an `occurrence` with no body of
  its own its stored master's (same calendar). Copying the master's body would have
  duplicated the Teams HTML of each master into every occurrence. `Store::event_body`
  uses it, and so
  does `update_event_fields` for its `old` body, so it compares an occurrence edit's
  body against what the host showed: a subject-only edit does not mark `BODY`, and
  the master's HTML is not sent. `event_for_send` reads the row's own body (r4): a
  `BODY` PATCH sends exactly what the row holds, so clearing an occurrence's shown
  body sends it cleared. Known transient: until the next sync stores that occurrence
  as an `exception` (which has no fallback), `event_body` still shows the master's
  body; a marker would need a new column. An edit that leaves the body alone (no
  `BODY`) writes the row's own body, not the shown one (r5): a subject-only edit of
  a filled occurrence keeps `''`, so it still follows its master, and an edit during
  the transient does not write the master's body back for the queued clear to send.
  Its join link is still read from the shown body (the master's link, already
  copied).
- **A missing master is fetched** (`Engine::fetch_missing_masters`, after a round
  and before the write). It fetches the masters of the round's occurrences and of
  the calendar's stored blank ones (`Store::blank_occurrence_masters`) that are not
  stored as a `seriesMaster` of that calendar (`Store::has_series_master`; in an
  initial round, stored or not: see below), plus
  every id the round lists as removed that is stored as one. A master starting
  before the window that changed comes as `@removed` (#2013); the removal does not
  delete it, since it does not overlap the window, so without the refetch its
  occurrences would keep the old fields. The round's own masters are not fetched,
  and each id is fetched once, in id order, with `get_event`. A `seriesMaster`
  answer joins the round's events (under the round's calendar) with its attendees;
  any other type is ignored. A 404 is no `Error`: the id goes into the engine's
  `masters_not_found` (in memory, cleared by `enter_signin` and a restart), so a
  needed master that `/me/events` cannot reach is asked once a session, not every
  pass. A removed stored master that 404s goes through `CalendarRound::Delta`'s
  `gone_masters`, which deletes only that calendar's `seriesMaster` row of that id,
  of any date, before the upserts. A removed stored master skips the
  `masters_not_found` check (r1): if its pass stops before the write, it is still
  stored, and the replayed link lists it again, so the next pass's 404 deletes it.
  A 401, throttle or transport error ends the pass as a failed round does (nothing
  written, every link kept: `MasterFetch::StopPass`). An HTTP or parse error is
  reported. For a needed master the round is written without it (its occurrences
  stay blank and are named again next pass). For a removed stored master the
  calendar is left out of the pass (`MasterFetch::SkipCalendar`), as a failed
  round is: writing would advance the link past the `@removed` and keep the stale
  master for good (r1). Only once per id a session (`masters_skipped`, cleared with
  `masters_not_found`; a refetch that succeeds or 404s forgets it): a refetch that
  fails again (a 403 on a delegate's private item, a repeated 500) writes the round
  with the stale master, or one master would stop its calendar syncing until the
  window moves at 00:00 UTC; the next initial round fetches it again (r2).
- **An initial round refetches its stored masters** (r2). In a
  `CalendarRound::Window` round a needed master that is not among the round's own is
  fetched even when it is stored: Graph's initial round may not list a master that
  starts before the window, the round re-sends minimal occurrences, and a stale
  stored master (a series renamed before a reset, `UpdateEventSeries`' among them)
  would refill them with its old fields, while the new baseline means no later
  delta lists the change. One GET per series per initial round (daily, plus
  resets). An HTTP or parse failure there writes the round with the stored copy
  (no `@removed` to replay); a 404 joins `masters_not_found`, and the stored row
  stays, since the round still names its occurrences. Delta rounds fetch only
  masters that are not stored, or removed. A refetched master that has not changed
  (the same `lastModifiedDateTime`) fills only the round's own occurrences (r3,
  above). #2013's cascade is
  unchanged: a master listed as removed with none of its occurrences in the round
  still takes its in-window occurrences and the delta state, and the next initial
  round brings them back, filled from the refetched master.
- **Masters stay stored, off the grid.** `GRID_ROWS`, shared by `events_in_window`
  and `events_in_range` (and so `days_with_events`), leaves out a `seriesMaster`
  once a row of its series (an occurrence or exception, of any date, cancelled or
  not) is stored, and a cancelled `occurrence` or `exception`. A master with none
  stays: a new series is one `seriesMaster` row until its occurrences sync, as
  `local:` (#2034) and after its `CreateEvent` reconciles to its Graph id alike,
  since no refresh follows the drain (r1; a "synced master" rule hid it until the
  next pass). `idx_events_series` (`series_master_id`, an original column) serves
  the `EXISTS`. After #2013's cascade a refetched master is alone until the next
  initial round, so it is drawn on its first day meanwhile, usually before the
  window. A cancelled `singleInstance` stays too, for Remove from Calendar (#2024). The asymmetry is deliberate: an occurrence takes its master's
  `is_cancelled`, so a whole series the organizer cancelled leaves an attendee's
  grid, as the issue asked. `Store::event`, `series_calendar_id`,
  `delete_event_series` and the fill still see masters. A `FetchCalendarRange`
  (`apply_calendar_range`, plain `calendarView`, which never returns a master) keeps
  the stored masters in its range (`replace_calendar_range_on`'s `keep_masters`);
  a `Window` round replaces them, since the delta returns them. `FetchCalendarRange`
  does not fill: its occurrences come full.
- Not done (follow-ups): masters that no row refers to any more are not pruned:
  with no row of its series stored, such a master is drawn on its own first day
  (usually before the window) until something removes it (a window replace drops
  in-window ones, a 404'd removed master goes, a deselected calendar goes). A shared calendar of another owner: `/me/events/{id}` may 404
  there, so its occurrences stay blank (asked once a session).
- Tests: `store` `calendar_tests` `apply_calendar_sync_fills_occurrences_from_their_master`,
  `apply_calendar_sync_fill_leaves_exceptions_alone`,
  `apply_calendar_sync_fill_keeps_an_untouched_occurrences_local_write`,
  `apply_calendar_sync_delta_master_change_refreshes_its_occurrences`,
  `apply_calendar_sync_repairs_blank_occurrences_from_a_stored_master`,
  `apply_calendar_sync_fill_needs_the_master_in_the_same_calendar`,
  `events_in_range_leaves_out_synced_series_masters_but_not_local_ones`,
  `events_in_window_leaves_out_series_masters`,
  `events_in_range_leaves_out_cancelled_series_members`,
  `a_filled_all_day_occurrence_marks_its_utc_date`,
  `event_body_of_an_occurrence_falls_back_to_its_master`,
  `blank_occurrence_masters_lists_distinct_ids`,
  `apply_calendar_range_keeps_a_stored_series_master`,
  `update_event_fields_on_a_filled_occurrence_does_not_mark_the_masters_body`,
  `apply_calendar_sync_gone_masters_deletes_only_that_calendars_master`,
  `a_series_master_leaves_the_grid_once_its_series_has_a_row`,
  `a_created_series_stays_on_the_grid_through_its_reconcile`,
  `apply_calendar_sync_unchanged_master_fills_only_the_rounds_occurrences`,
  `update_event_fields_clearing_an_occurrences_body_sends_it_empty`,
  `update_event_fields_leaving_an_occurrences_body_keeps_its_own`; `outbox.rs`
  `update_event_clearing_an_occurrences_shown_body_sends_it_empty`; `engine.rs`
  `calendar_delta_occurrences_take_their_masters_fields`,
  `calendar_delta_repairs_blank_occurrences_in_one_pass`,
  `calendar_delta_fetches_a_missing_master`,
  `calendar_delta_missing_master_404_writes_the_rest_once_per_session`,
  `calendar_delta_removed_master_beside_occurrences_is_refetched`,
  `calendar_delta_master_only_removed_is_refetched_and_restored_next_pass`,
  `calendar_delta_removed_master_404_deletes_the_stored_master`,
  `calendar_delta_removed_master_http_error_keeps_the_link`,
  `calendar_delta_removed_master_404_in_a_stopped_pass_is_retried`,
  `calendar_delta_removed_master_failing_twice_writes_the_second_pass`,
  `calendar_initial_round_refetches_a_stored_master_it_does_not_list`,
  `calendar_initial_round_unchanged_master_keeps_a_far_occurrences_write`,
  `calendar_delta_master_fetch_throttled_writes_nothing`. Tests that read a series
  master's fields through `events_in_window` read it with `Store::event` now.

## An event's own response time and created time (#2068)

- `Event`/`NewEvent`/`EventRow` `response_time` is Graph's `responseStatus.time`, when
  the signed-in user answered the event (a client's "Accepted on <time>"); `created_utc`
  is `createdDateTime`, the meeting's "Sent" date. Both are canonical UTC
  `YYYY-MM-DDTHH:MM:SSZ` (`graph::model::graph_utc_or_empty`: `graph_date_or_empty`
  turns Graph's year-1 "never" into `""` first, as that time is otherwise readable, then
  `reminder_time` drops any fraction and reads `""` for a numeric offset or garbage) and
  `""` when absent. `AttendeeRow::response_time` stays the attendees' `status.time`
  (#1983).
- Stored in `events.response_time` / `events.created_utc` (`TEXT NOT NULL DEFAULT ''`,
  added to an older database by `Store::init`'s #1983 column loop), written by
  `upsert_event_on` on insert and `ON CONFLICT`, read by `EVENT_ROW_COLUMNS`.
- Status and time are written, and corrected, together. `Store::set_event_response`
  takes the time (`at`), so the store stays clock-free: the engine's `RespondEvent`
  passes now, as Graph stamps it, and a host such as lookxy, which writes its RSVP before
  it sends `RespondEvent`, passes its own now (`ui::calendar::now_utc`). A refused
  `RespondEvent` writes nothing in the engine; a host's own write keeps both until the
  reset round (#2013) puts Graph's back, as for the status. A quarantined op is
  corrected the same way.
- Occurrences take both from their master (`SERIES_FIELDS`, #2064): Graph's occurrence
  shares the series' answer and creation time (an occurrence answered on its own is an
  exception, never filled). The fill gate holds for them as for the status: an
  untouched occurrence keeps its own pending RSVP time.
- `Store::reconcile_created_event` writes both from the event Graph returned to
  `CreateEvent`, so a meeting the user just sent has its "Sent" date at once; a
  `local:` row has `''`.
- No `$select` change: no calendar event read sends `$select` (`calendar_view*`,
  calendarView delta, which refuses it, `event_instances`, `get_event`), so Graph's
  default representation carries `createdDateTime` and `responseStatus`.
- Tests: `model.rs` `event_own_response_time_and_created_utc`,
  `event_response_time_never_and_absent_are_empty`; `store` `calendar_tests`
  `event_response_time_and_created_utc_round_trip`, `set_event_response_flips_status`,
  `reconcile_created_event_repoints_event_and_attendees`, the #2064 fill tests
  (`assert_filled_from`, `apply_calendar_sync_fill_keeps_an_untouched_occurrences_local_write`,
  `apply_calendar_sync_delta_master_change_refreshes_its_occurrences`), and
  `events_without_1983_columns_migrate` (an old row reads `''`); `engine.rs`
  `calendar_delta_stores_own_response_time_and_created_utc`,
  `calendar_delta_occurrences_take_their_masters_fields`,
  `respond_event_optimistically_flips_status_and_drains_to_accept`; lookxy
  `ui::calendar` `accept_key_opens_a_comment_prompt_and_enter_submits_it_with_the_comment`,
  `tentative_key_sets_the_tentative_status`.

## A draft's unpushed body is kept against a fetch (#2082)

- `Store::put_fetched_body_with_headers` (and the `cfg(test)` `put_fetched_body`) write
  nothing and return `Ok(false)` when a body is already stored for the id and either
  the row has an unsaved compose edit (`local_edit > 0`) or a queued `SaveDraft` /
  `SendDraft` names it by the id it is stored under now (`current_id_on`: an op queued
  under the `local:` id since reconciled, or before a move, #1896). Neither the body
  nor the headers change. Such an op PATCHes `Store::draft`'s body at drain time
  (`ensure_draft_on_graph`), so a fetched body written over the user's would be sent as
  the draft's: a `SendDraft` deferred by a 5xx, then a `FetchBody`, sent Graph's
  quote-only reply.
- Other ops (`MarkRead`, `Delete`, ...) push no body and keep nothing; a row with no
  stored body takes the fetch, as it holds no user text. The check
  (`fetched_body_is_kept_on`) and the write share one `write_tx`. It reads the outbox
  only after the cheaper tests (a stored body, then `local_edit`), and skips an outbox
  payload it cannot decode, so one bad row does not fail every fetch.
- `put_body` is not guarded: it is the local writer (`createReply`'s body in
  `store_composed_draft`, an embedder's own write), and refusing it would lose the newer
  text.
- The engine's `fetch_and_store_body` takes a kept body as an answer: `BodyReady`, and
  `Ok(true)`, so `FetchBody` is answered, `FetchMessage` does not fail with "could not
  store the body", and the background fetcher does not park. `get_body_for` (#1901)
  still decides whether an embedder asks; this is the store-side backstop.
- `fetch_and_store_body` returns `BodyStored` (`Written`, `Kept`, `Failed`). A kept body
  leaves the row unchanged, so it still reads as needing a body (`needs_body`,
  `body_candidates`: a row `mark_sent` filed outside Drafts, with a text body under
  `BodyFormat::Html`); `background_body` puts a `Kept` id in `body_skip`, or the
  prefetch, whose wait is zero while it has candidates, would fetch it on every loop turn
  for as long as the op waits (r1).
- Embedders' tests: the platform secret store is skipped only under mailcore's own
  `cfg(test)`, so a client's tests that reach the token store set
  `MAILCORE_TOKEN_STORE=file` (`tokencache` and `testserver` module docs).
- Tests: `store` `kept_body_tests` (a local edit, a queued `SaveDraft` / `SendDraft`, an op
  under the reconciled `local:` id, `MarkRead` and `Delete` keep nothing, no body yet,
  `put_body` still writes, an undecodable outbox row); `engine.rs`
  `a_deferred_send_keeps_the_users_body_over_a_fetch` (the 500 PATCH, then `FetchBody`
  before the back-off, then `Refresh`: the PATCH before `/send` carries the user's body).
  `engine.rs` `a_kept_draft_body_is_prefetched_once_not_in_a_loop` (one body GET for the
  protected row in a run).

## DraftReady names its request (#2085)

- `SyncEvent::DraftReady { id, original, kind }`: `id` is the stored draft, `original`
  the message id exactly as the `ComposeReply` / `ComposeForward` command carried it, and
  `kind` (`DraftKind::Reply`, `ReplyAll`, `Forward`) the answer asked for. A host that
  stopped waiting for a request (a timeout, the form closed) compares them with the
  request it waits for now and drops a late draft of an earlier one: a failed request
  emits no `DraftReady`, and no failure event names the request (a transport error emits
  only `State(Offline)`, when the engine was not offline already; a throttle at most a
  `State` change, and the request is not retried; others an `Error` or the sign-in
  events), so "late" and "failed" cannot be told apart otherwise, and two
  answers in one conversation share `conversation_id`.
- `original` is never rewritten. Graph is addressed by `current_id` (an id from before a
  move, #1896), as every other message command is, but `compose_reply` /
  `compose_forward` pass the command's own id to `store_composed_draft` for the event.
- lookxy opens every draft it is handed (`{ id, .. }`).
- Tests: `engine.rs` `compose_draft_ready_names_its_original_and_kind` (M1 reply, M2
  reply all, M3 forward, three events in order); `moved::`
  `compose_reply_by_the_old_id_after_a_move_names_the_old_id` (`createReply` POSTs to
  `NEW1`, the event names `OLD1`).

## A message's meeting type on its row (#2081)

A host's message list tells a meeting request, a cancellation and an attendee's accepted,
tentative or declined response from ordinary mail without a request per row: each
message's `MeetingMessageType` (#2024's enum) is stored on its row.

- Source: MAPI `PidTagMessageClass` (tag `0x001A`, PT_UNICODE; every message has one),
  as the `singleValueExtendedProperties` entry `model::MESSAGE_CLASS_PROP` (`String
  0x001A`). It is one more `or id eq` term in #2054's `client::MESSAGE_EXPAND`, so it rides
  every request that already sends the expand (a delta's first call, `get_message`,
  `get_message_hit`), Graph keeps it in the links it mints, and a refused expand falls
  back as #2054's does. No new request, no new failure mode on the sync path. Rejected:
  a delta `$select` of the cast `microsoft.graph.eventMessage/meetingMessageType` (no doc
  shows delta accepting a type-cast select, and a refused `$select` has no fallback), and
  `@odata.type` (a cancellation is a plain `eventMessage`, as a `none` one is; a response's
  kind is in another derived property).
- Classes (`MeetingMessageType::from_message_class`, MS-OXOCAL):
  `IPM.Schedule.Meeting.Request` → `Request`, `.Canceled` → `Cancelled`, `.Resp.Pos` →
  `Accepted`, `.Resp.Tent` → `TentativelyAccepted`, `.Resp.Neg` → `Declined`; ASCII case
  ignored (MAPI does), and a subclass (`.` and a non-empty suffix) keeps its base's type.
  Anything else (`IPM.Note`, `IPM.Schedule.Meeting.Notification.Forward`,
  `…Meeting.RequestX`) is `None`.
- Parse (`model::parse_meeting_message_type`, lenient, never an error): a top-level
  `meetingMessageType` string wins (a plain GET of an `eventMessage` has it; `none` and
  unknown values are `None`), else the class entry with a string value. The id matches
  through `is_message_class_prop`: `String ` in any case and a hex tag equal to `0x1A`,
  so Graph's short echo (`String 0x1a`) matches and `Integer 0x001A` does not.
- Written only when carried: `MessageFields::meeting_type` is the parse's carried flag,
  and `writes_meeting_type` is that alone. Every message has a class, so a request that
  asked for it always gets it back, and a carried `IPM.Note` writes `None`. An item
  without it (partial, read without the expand, or an expand Graph dropped without a 400)
  says nothing, and the stored type, perhaps one `FetchMeetingMessage` wrote, is kept.
  Unlike the reminder, whose key Graph leaves out when nothing matches, there is no
  "complete item" term: it would blank every row on a silently dropped expand.
- Store: `messages.meeting_message_type` (TEXT, NULL for none) holds
  `MeetingMessageType::as_graph` (Graph's names, `meetingTenativelyAccepted` included),
  read back by `from_graph`, so an unknown stored value reads `None`. `MessageRow` carries
  it from every row read; `search` and `draft` now select `MESSAGE_ROW_COLUMNS` too, and
  `draft`'s body columns follow at `BODY_AFTER_ROW_COLUMNS`. Only
  `conversations_in_folder` still spells the list out, twice (its `keyed` CTE and outer
  select).
- Migration: `migrate_meeting_message_type`, modelled on `migrate_message_reminders` (one
  IMMEDIATE transaction that probes `pragma_table_info`), adds the column (old rows read
  `None`) and resets every folder's delta and backfill state and both expand-refused
  marks, without a reconverge: a link minted before #2081 carries the old expand, so new
  mail through it would never get a type. The cost is one re-enumeration, as #2054's.
  A pre-#2054 store runs both on one open.
- `FetchMeetingMessage`: an answer with a type writes it to the stored row
  (`Store::set_meeting_message_type`, through `current_id`, `UPDATE … WHERE … IS NOT ?2
  RETURNING folder_id`) and, when that changed it, emits `MessagesUpdated { folder_id }`
  before `MeetingMessageFetched`. An answer without one writes nothing; a store failure
  is an `Error`, and the event still comes. `RemoveFromCalendar` does not write it.
- Search hits (`search_messages`, #2006) ask for the expand on their first request
  through `get_with_expand`, so a hit's `Message` carries the type; a refusal marks the
  hits `reminder_unasked`, and a 400 from bad KQL now costs a second request that gets
  the same 400. Graph forbids `$filter` with `$search` as a query option; the expand's own
  `$filter` is inside its parentheses, and the fallback covers a Graph that refuses it.
- Tests: `model.rs` `meeting_message_type_from_message_class`,
  `meeting_message_type_round_trips_its_graph_name`,
  `message_class_prop_id_matches_its_forms_only`,
  `message_reads_meeting_type_from_class_and_top_level`,
  `writes_meeting_type_only_when_carried`; `client.rs`
  `message_expand_names_every_property` (#2096's name),
  `delta_page_reads_each_message_class_as_its_meeting_type`,
  `search_messages_reads_the_meeting_type_of_each_hit`,
  `search_messages_retries_without_expand_on_400`; `store` `meeting_message_type_tests`
  (every read path, `draft`'s body, the merge rule, the migration, a pre-#2054 store
  running every migration in order (`a_store_from_before_2054_runs_every_migration_in_order`,
  renamed in #2088, which adds a third:
  `migrate_meeting_message_type` resets the marks `migrate_message_reminders` adds, so
  the order in `Store::init` matters), `set_meeting_message_type` through a move); `engine.rs` `meeting_message_types::` (a delta page of each type and a
  plain message, a later round without the class keeping it, `FetchMeetingMessage`
  writing an old row and its events, an answer without a type keeping the row).

## A message's protection on its row (#2088)

A host's message list shows classic Outlook's lock, a label or a sensitivity cue on a
protected message without opening it: each `MessageRow` (every read: `message_row`,
`message_rows`, `messages_in_folder`, `conversations_in_folder`, `search`, `draft`,
`due_message_reminders`) and each `Message` carries a `model::MessageProtection`.

- Sources (checked on learn.microsoft.com, 2026-11-10). Graph v1.0 `message` has no
  sensitivity or label property, so all three are `singleValueExtendedProperties`:
  - `PidTagSensitivity`, tag `0x0036`, PT_LONG: 0 normal, 1 personal, 2 private,
    3 confidential (MS-OXCMSG 2.2.1.13). `model::SENSITIVITY_PROP` (`Integer 0x0036`),
    read into `Sensitivity` by `from_mapi` (anything else is `Normal`).
  - The `msip_labels` header as a PS_INTERNET_HEADERS named property
    (`MSIP_LABELS_PROP`, `String {00020386-0000-0000-C000-000000000046} Name
    msip_labels`), the header #2015 stores with a body.
  - `PidNameContentClass` (`CONTENT_CLASS_PROP`, PS_INTERNET_HEADERS, name
    `Content-Class`): MS-OXORMMS 2.2.2.1 says a rights-managed message's is
    `rpmsg.message`. That is `is_rights_protected` (trimmed, ASCII case ignored).
  - The message-delta page (v1.0, updated 2026-06-19) says delta supports `$select`,
    `$top` and `$expand` for messages, which #2054 could not find; the 400 fallback stays.
- Request: three more `or id eq` terms in `client::MESSAGE_EXPAND`, so they ride every
  request that already sends it (a delta's first call, `get_message`, `get_message_hit`,
  a search's first request) and every link Graph mints from them. No new request. One
  expand: a 400 on any term (the PS_INTERNET_HEADERS named ids in a `$filter` are the
  least proven) sends the call again without all of them, so that walk also reads no
  reminder or class (`reminder_unasked`; nothing is blanked).
- The label (`model::parse_msip_labels`, lenient): `;`-separated `key=value` pairs, each
  key `MSIP_Label_<guid>_<field>` split at its last `_` (a GUID has none); one pair of
  surrounding `"` is stripped from a value. The label is the first GUID, in order of
  appearance, whose `Enabled` is `true` (keys, field, value and GUID compared ignoring
  ASCII case); its name is that GUID's `Name`. A message can carry a sender tenant's
  label beside ours, and the tenant is not known here, so the first enabled one wins.
  `has_label` with `label_name == ""` means labelled, but the metadata names none (newer
  clients may leave `_Name` out). No enabled GUID, or a non-string value, is no label.
- Written only when the class came: `parse_message_protection`'s carried flag (and
  `MessageFields::protection`, `writes_protection`) is "the array holds a
  `MESSAGE_CLASS_PROP` entry". The three properties are absent on most mail, so their
  absence alone cannot tell "none" from "not asked"; every message has a class (#2081)
  and it rides the same expand, so an item carrying it was read with the expand, and an
  absent property then means none (a carried plain message clears a stored protection).
  An item without the class (partial, read without the expand, a silently dropped
  expand, a plain GET with only a top-level `meetingMessageType`) is not carried, and the
  stored values are kept. #2054's "complete item" rule was rejected for the reason #2081
  rejected it: it would blank every row on a silently dropped expand. A composed draft
  (`upsert_message`, `MessageFields::all()`) writes the default; the next delta corrects
  it.
- Store: `messages.sensitivity` (INTEGER, `Sensitivity::as_mapi`; an odd stored value
  reads `Normal`), `has_sensitivity_label`, `sensitivity_label_name` (TEXT, `''`) and
  `is_rights_protected`, all NOT NULL with defaults. They were the last four of
  `MESSAGE_ROW_COLUMNS` (`BODY_AFTER_ROW_COLUMNS` 28; #2089 adds one after them) and of
  `conversations_in_folder`'s two spelled lists. `merge_message`'s one statement writes all four on
  `writes_protection`.
- Migration: `migrate_message_protection`, after `migrate_meeting_message_type` in
  `Store::init` (and after `migrate_message_reminders`, whose marks it resets), probes
  for `sensitivity` in one IMMEDIATE transaction, adds the four columns (old rows read
  unprotected) and resets every folder's delta and backfill state and both
  expand-refused marks, as #2081's does and for the same reason: a link minted before
  #2088 carries the old expand (class, no protection terms), and new mail through it
  would be written unprotected. The cost is one more re-enumeration; a pre-#2081 store
  runs both resets on one open.
- Known limit: a store that already has the columns but holds a link minted under the
  older expand (an older binary syncing a store a newer one migrated, or two hosts on
  one `mail.db` built from different commits) gets carried classes without the
  protection terms, so its new and changed rows read unprotected until the next full
  resync. No per-link expand version is kept.
- Not done: any host UI (a lock icon, a label banner), setting a sensitivity or a label
  on send, filling the row from the #2015 header on a body fetch, label names or colours
  from the Information Protection API (or whether a label encrypts), S/MIME
  (`IPM.Note.SMIME*`).
- Tests: `model.rs` `sensitivity_from_mapi_reads_the_four_values_and_defaults_the_rest`,
  `msip_labels_reads_the_enabled_label_and_its_name`,
  `message_protection_reads_the_three_properties`,
  `message_protection_is_carried_only_with_the_class`,
  `protection_terms_leave_the_reminder_rule_alone`,
  `sensitivity_prop_id_matches_its_forms_only`; `client.rs`
  `message_expand_names_every_property` (#2096's name; renamed from
  `…_reminder_and_class_properties`), `delta_page_reads_each_message_protection`;
  `store` `message_protection_tests` (every read path, `draft`'s body after the shifted
  columns, an odd stored sensitivity, the merge rule, the migration and its idempotence),
  `a_store_from_before_2054_runs_every_migration_in_order`, `upsert_writes_every_column`;
  `engine.rs` `message_protection::` (a delta of a label, Sensitivity confidential, an
  IRM message and a plain message each over a stored protection, and a later round
  without the class keeping it).

## A meeting message's own reminder on its row (#2089)

A host's message list shows classic Outlook's reminder bell beside a meeting message's
icon without a request per row: each `MessageRow` (every read: `message_row`,
`message_rows`, `messages_in_folder`, `conversations_in_folder`, `search`, `draft`,
`due_message_reminders`) and each `Message` carries `meeting_reminder_on`.

- Source: `PidLidReminderSet` (`model::REMINDER_SET_PROP`), which #2054's
  `client::MESSAGE_EXPAND` already asks for on every message, beside the class
  (`MESSAGE_CLASS_PROP`, #2081). No new term and no new request: a delta's first call,
  `get_message`, `get_message_hit` and a search's first request bring both.
- Not `is_reminder_on`: that one is the flag's reminder (#2054), never true on an
  unflagged row, and it drives `due_message_reminders`. A meeting request's own set bit
  is kept in its own field and never fires a reminder; an unflagged meeting request with
  the bit is not due. A flagged meeting request can have both on.
- Parse (`model::parse_meeting_reminder`, `(carried, on)`, lenient): carried is "the
  array holds a `MESSAGE_CLASS_PROP` entry", whatever its value, the protection's rule
  (#2088) and for the same reason: the bit is absent on most mail, the class rides the
  same expand, so an item carrying it was read with the expand. On is carried, a meeting
  message and the bit, read by the private `reminder_set` that
  `parse_message_reminder` shares (`"true"` any case, or JSON `true`). No reminder time
  is needed: this one never fires. A meeting message is a string class `is_meeting_class`
  accepts (`IPM.Schedule.Meeting`, or it plus `.` and at least one character, ASCII case
  ignored, not trimmed: `from_message_class`'s subclass rule), or an `@odata.type`
  starting `#microsoft.graph.eventMessage`, which makes the item a meeting message
  whenever the class does not: a class entry with no string value, or a string
  `is_meeting_class` rejects (`IPM.Note`, `IPM.Schedule.MeetingX`). The class half covers
  the forward notification
  (`IPM.Schedule.Meeting.Notification.Forward`), which has no `MeetingMessageType`, so the
  rule does not key on `meeting_message_type`; `@odata.type` alone is absent on a partial
  item. A carried message that is neither writes false.
- Store: `messages.meeting_reminder_on` (INTEGER NOT NULL DEFAULT 0), the last of
  `MESSAGE_ROW_COLUMNS` (`BODY_AFTER_ROW_COLUMNS` 29) and of `conversations_in_folder`'s
  two spelled lists. `merge_message` writes it on `MessageFields::writes_meeting_reminder`
  (`MessageFields::meeting_reminder`, the carried flag), with no flag term. A partial
  item, a refused expand (`reminder_unasked`) or a silently dropped one keeps the stored
  value. A composed draft (`upsert_message`, `MessageFields::all()`) writes false.
  `set_flag` and `snooze_message_reminder` leave it alone.
- Dismiss: `Store::dismiss_message_reminder` turns it off too, as the command's PATCH
  sets the same `PidLidReminderSet` false, so the row matches the next delta. A refused
  PATCH's `reread_message` (`get_message`, with the expand) restores it. Known limit: if
  that re-read is itself refused the expand (a 400, then a plain GET), it carries no
  class, and the row reads off until a delta reports the message again.
- Migration: `migrate_meeting_reminder`, last in `Store::init`, probes for the column in
  one IMMEDIATE transaction and adds it (old rows read false). It does **not** reset any
  delta or backfill state, unlike #2054's, #2081's and #2088's: the expand did not
  change, so every stored link already brings the bit and the class, and a store older
  than #2088 is reset by that migration on the same open. Rows stored before read false
  until a delta reports them again; a whole-mailbox re-enumeration for a bell on old mail
  was judged not worth it.
- Not done: any host UI (lookxy's list shows no bell), backfilling rows stored before the
  migration, dismissing or snoozing a meeting message's reminder as a command of its own,
  or any reminder that fires for a meeting message (a calendar event's reminder is
  #2025's).
- Tests: `model.rs` `meeting_reminder_reads_the_set_bit_of_a_meeting_message`,
  `meeting_reminder_takes_the_type_when_the_class_has_no_string`,
  `meeting_reminder_is_false_on_a_plain_message`,
  `meeting_reminder_is_carried_only_with_the_class`,
  `meeting_reminder_is_carried_exactly_when_the_protection_is` (the two carried rules
  agree), `meeting_reminder_leaves_the_flag_reminder_alone`; `store`
  `meeting_reminder_tests` (every read path, `draft`'s body after the shifted columns,
  the merge rule, no flag term and never due, dismiss, the migration without a reset and
  its idempotence), `a_store_from_before_2054_runs_every_migration_in_order`,
  `upsert_writes_every_column`; `engine.rs` `meeting_reminders::` (a delta page of a
  meeting request with and without the bit, a plain message with the bit and a time, a
  flagged meeting request still due; a later round without the class keeping it and one
  with it clearing it; dismiss, and a refused dismiss re-read on through `get_message`).

## Which message a reply draft answers (#2096)

A host marks a message that has an unsent reply draft, as classic Outlook does with a red
`[Draft]` before the sender: a draft whose `conversation_id` matches the message's and
whose In-Reply-To matches the message's Internet message id. The store keeps both ids on
every row, keeps Graph's `isDraft`, links a reply composed here at once, and offers the
links in one cheap read.

- Sources: `internetMessageId` and `isDraft`, both added to `client::MESSAGE_SELECT`, and
  `PidTagInReplyToId` (tag `0x1042`, PT_UNICODE) as `model::IN_REPLY_TO_PROP`
  (`String 0x1042`), one more `or id eq` term in `client::MESSAGE_EXPAND`, so it rides
  every request that already sends the expand (a delta's first call, `get_message`,
  `get_message_hit`, a search's first request), and a refused expand falls back as
  #2054's does. Rejected: a `$select` of `internetMessageHeaders` (delta does not return
  it, and it is heavy).
- Parse (lenient, never an error): `Message::internet_message_id` is `str_field`
  (`""` when absent or not a string); `model::parse_in_reply_to` is `(carried, value)`,
  carried when the `singleValueExtendedProperties` array holds an entry
  `is_in_reply_to_prop` accepts (`String ` in any case, a hex tag equal to `0x1042`;
  `Integer 0x1042` does not) with a string value. The value is stored as Graph sends
  it, not trimmed, because a `<…>` id is compared verbatim with `internetMessageId`.
- Merge: `MessageFields::internet_message_id` (the key as a string) and `in_reply_to`
  (the parse's carried flag) gate the write, with no "complete item" term (#2081's
  reason: a silently dropped expand would blank every row). **And** the SQL writes only
  a non-empty value (`CASE WHEN ?w AND excluded.x <> '' …`), so a stored id is never
  cleared through a merge: not by a partial item, not by a complete one read without the
  expand, not by a carried `""`, not by a `Message` built in code (`upsert_message`,
  `MessageFields::all()`, such as a `createReply` response or one of lookxy's callers).
  A message's ids never legitimately go from set to empty. An insert writes what the
  item has.
- `isDraft`: `MessageFields::writes_is_draft` keeps its rule (`is_draft || !is_partial()`),
  so a complete delta item now writes Graph's value, and a draft made in Outlook or on the
  web stays a draft. A code-built complete item without the key still writes its parsed
  `is_draft`.
- Composed drafts (`Engine::store_composed_draft`): the row is `is_draft` whatever the
  POST response says (a `createReply`/`createReplyAll`/`createForward` result is a draft
  by definition). For `DraftKind::Reply` and `ReplyAll`, `in_reply_to` is the original's
  stored `internet_message_id`, through `current_id` (#1896), so the link exists with no
  further request. A missing row, an empty id or a store error leaves it `""`, and the
  next delta fills it from Graph. A forward gets none, as Exchange sets no In-Reply-To on
  one.
- Store: `messages.internet_message_id` and `messages.in_reply_to` (TEXT NOT NULL DEFAULT
  `''`), the last two of `MESSAGE_ROW_COLUMNS` (`BODY_AFTER_ROW_COLUMNS` 31) and of
  `conversations_in_folder`'s two spelled lists. `MessageRow` carries both from every row
  read (`message_row`, `message_rows`, `messages_in_folder`, `conversations_in_folder`,
  `search`, `draft`, `due_message_reminders`).
- `Store::reply_draft_links() -> Vec<(conversation_id, in_reply_to)>`: every row with
  `is_draft = 1 AND in_reply_to <> ''`, in any folder but the well-known Deleted Items
  (`LEFT JOIN folders`, `COALESCE(well_known_name, '') <> 'deleteditems'`), each pair
  once, sorted. A discarded draft moves to Deleted Items with Graph's `isDraft` still
  true, so without that the original would stay marked after the reply was thrown away.
  A sent draft (`mark_sent`) drops out and comes back with `return_to_drafts`; a forward
  and a received reply are never in it. It reads the partial index
  `idx_messages_reply_drafts` (`ON messages(conversation_id, in_reply_to, folder_id)
  WHERE is_draft = 1 AND in_reply_to <> ''`; `folder_id` makes it covering for the
  join). The messages side of the query's WHERE is spelled as the index's, or SQLite
  will not use it. The index is created in `Store::init` after the migration, never in `schema.rs`:
  that batch runs before the migrations, when an old table has no `in_reply_to`.
- Migration: `migrate_reply_links`, last in `Store::init`, probes for
  `internet_message_id` in one IMMEDIATE transaction, adds both columns (old rows read
  `""`) and resets every folder's delta and backfill state and both expand-refused marks,
  as #2088's does. A link minted before #2096 carries the old `$select` and `$expand`, so
  new mail through it would never get either id, and `isDraft` would stay unread. The
  cost is one more re-enumeration. A pre-#2054 store runs every migration on one open.
  In the same transaction, a one-off body fix: a draft body an engine from before #1901
  wrote as compose html under `content_type = 'text'` becomes `html`, so lookxy's text
  branch does not show (and save) its tags. It touches only a local write (`fetched_as IS
  NULL`) of a draft row (`is_draft`, or filed in Drafts or the local sentinel, as
  `get_body_for` decides) whose content starts as `compose_html::to_html` output does
  (`<p>`, `<ul><li>`, `<ol><li>`). A genuine text body (a plain-text `createReply`), a
  fetched body and a non-draft body are left alone. Every pre-#1901 store is pre-#2096,
  so each gets the fix once. Then, in the same transaction, every body still `text`
  with no recorded format gets `fetched_as = 'text'`, in any folder. Before #1901
  `FetchBody` stored every body with none. A draft made elsewhere reads `is_draft = 0`
  until the re-enumeration, wherever it is filed (Drafts, a Drafts subfolder, a user
  folder, Deleted Items), so a draft-row filter would miss it. Without the fix, a
  reader's text rendering of such a draft would read as written here
  (`draft_body_is_local`), open in compose, and save flattened text over the html
  draft. It leaves out a row holding an unpushed edit (`local_edit > 0`), a `local:`
  draft, a reconciled one (`local_id`) and a row in the local Drafts sentinel, since
  all four are compose's own. For a non-draft nothing changes: `None` and `Some("text")`
  both satisfy only a text ask (`body_satisfies`). The html relabel above keeps its
  draft-row condition. The accepted cost: a plain-text `createReply` body stored before
  #2096 opens read-only. Such a draft body satisfies a text ask as before but no longer
  an html one, so an html embedder fetches it once, and that fetch still keeps an edited
  or queued draft's body (#2082).
- Readers of `is_draft` that change behaviour:
  - lookxy: a draft made in Outlook or on the web opens read-only in the reader, as it
    did before #2096 (when it read `is_draft = 0`); editing it in lookxy is not
    supported (a follow-up). `open_draft` opens the composer only for a draft whose body
    was written here (`Store::draft_body_is_local`: a body row with `fetched_as IS
    NULL`, which only `update_draft_fields`, `create_local_draft` and a composed
    reply/forward's `put_body` write; a text body fetched before #1901, in any folder,
    is marked fetched by `migrate_reply_links`, the Migration bullet above). A draft
    with no body, or with one `FetchBody`
    stored (Graph's rendering, text under lookxy's `BodyFormat::Text`), opens in the
    reader (`open_message`), which fetches a missing body. So Graph's rendering is
    never edited and saved back over the server's draft. `Store::draft`, which the
    outbox also reads, is unchanged.
  - lookxy's list: Enter, and a click on a message row (flat, or a thread's message
    row; `on_left_click` activates a draft row at once, since the click-to-preview path
    skips drafts), both go through `open_draft`. A thread header whose latest message
    is a draft is never previewed, so it never becomes `selected_msg`. Its first click
    only selects it, and a second click on the same header toggles it, as for any
    header. "The same header" is `App::last_list_click`, a `ListClick` of the folder,
    `cursor_target`'s id (the header's latest message) and the header flag, never the
    row position. A sync reload, a key move or a toggle that puts another header at the
    clicked row therefore does not count, and any click elsewhere forgets it. Cursor
    preview and PgUp/PgDn skip drafts, as before.
  - lookxy's composer honours a local body's type (`draft_rich_text`): an `html` body is
    parsed as HTML, and any other (a `createReply` of a plain-text message) becomes one
    paragraph per line, so line breaks and `<alice@x>` survive. Esc on a draft unchanged
    since `open_draft` opened it (`App::compose_baseline`, a `ComposeSnapshot` of the
    fields, the body's HTML and the attachments) saves nothing: no `update_draft_fields`,
    no `SaveDraft`. A `local:` draft still saves, since its Save is what creates it on
    Graph, and Send always sends.
  - Known limit: `Store::headerless_message_ids` leaves out `is_draft = 1` rows as
    made here. A new row first seen as a partial delta item carrying `isDraft: true`
    and no `receivedDateTime` is never completed by #1899's repair (before this issue a
    partial item could not carry `isDraft`). Not fixed here, because a fix changes
    #1899's contract.
  - The `mark_sent` race (SendDraft's optimistic `is_draft = 0`, then a Drafts delta
    reporting the draft as 1) adds no new failure mode: the folder already flips back in
    that race, and the outbox drains before a folder's delta.
- Not done: editing a draft made elsewhere in lookxy (it opens read-only; a follow-up:
  fetch it as html for compose and keep that body from the reader's text), any host UI
  (lookxy's list shows no `[Draft]`), matching on
  `conversation_id` alone or on a draft's `References`, and backfilling `in_reply_to`
  from a body fetch's headers (#2015) for rows stored before.
- Tests: `model.rs` `in_reply_to_prop_id_matches_its_forms_only`,
  `message_reads_internet_message_id_and_in_reply_to`,
  `in_reply_to_is_carried_only_with_a_string_entry`,
  `fields_carry_internet_message_id_only_with_the_key`,
  `a_complete_item_writes_its_is_draft`; `client.rs`
  `message_expand_names_every_property`,
  `message_select_names_internet_message_id_and_is_draft`,
  `delta_page_reads_internet_message_id_in_reply_to_and_is_draft`; `store`
  `reply_link_tests` (every read path, `draft`'s body after the shifted columns, the
  merge rule, `reply_draft_links`, Deleted Items left out, its plan, the migration through `Store::open` with
  the index on the migrated table, and its idempotence),
  `a_store_from_before_2054_runs_every_migration_in_order`, `upsert_writes_every_column`;
  `engine.rs` `reply_links::` (a delta of a synced reply draft, an `isDraft: false` item
  over a stored draft and the original; a later round without the property keeping the
  link; ComposeReply and ComposeReplyAll answering M1 with only the POST and the body,
  ComposeForward answering nothing, and a reply to a moved original through
  `current_id`); lookxy
  `a_synced_draft_opens_read_only_in_the_reader`,
  `one_click_opens_a_synced_draft_in_the_reader`,
  `a_draft_with_a_local_text_body_opens_in_compose_as_text`,
  `a_draft_s_html_body_still_opens_as_html`,
  `esc_on_an_unchanged_draft_saves_nothing_and_after_an_edit_saves`,
  `esc_on_an_unchanged_new_message_still_saves_it`,
  `two_clicks_on_a_header_whose_latest_is_a_draft_toggle_it`,
  `a_click_on_another_header_at_the_same_row_only_selects_it`; store
  `draft_body_is_local_only_for_a_body_written_here`, and the migration test's two body
  fixes;
  and
  `a_plain_text_reply_draft_is_not_a_refetch_candidate`, which now expects `is_draft`.

## Contact search folds in Rust (#1890)

`Store::search_contacts(query, limit)` (lookxy's attendee autocomplete) used to compare the
query with SQLite's `lower(name) LIKE …`. This bundled SQLite has no ICU, so its `lower()` and
`LIKE` fold ASCII only: `Борис`, `Élise` and `Ωmega` were never found by their lowercase
spelling, nor even typed as stored, because the query was lowercased in Rust first.

- `contact_match(name, address, folded_query)` is a Rust scalar SQL function (rusqlite's
  `functions` feature, which adds no crate), `SQLITE_UTF8 | SQLITE_DETERMINISTIC`.
  `register_contact_match` registers it at the end of `Store::init`, so every connection has
  it: the engine's, a host's, the tests'. A NULL argument reads as `''` and never fails the
  query. The rank lives in `contact_match_rank`.
- The fold, `fold_contact_text`, applies to the query and to both columns: Turkish `İ` → `i`,
  then Unicode `to_lowercase`, then `ς` → `σ`. `to_lowercase` writes `İ` as `i` + U+0307, so
  `ilk` would never find `İlker`. It also applies Final_Sigma, so `ΟΔΥΣ` lowercases to `οδυς`,
  which is never found in `οδυσσέας` without the last step. Unicode case folding does the same
  with `ς`.
- Rank `0` is a word-prefix match. The query starts the folded name, or starts right after a
  separator, or does the same in the address' local part (before its first `@`). A separator
  is a character that is neither `char::is_alphanumeric` nor `continues_a_word`: a space, `-`,
  `'`, `’`, `,`, `(`. `continues_a_word` holds the common blocks of combining marks (Latin,
  Cyrillic, Hebrew points, Arabic harakat, Indic viramas and nuktas, Thai marks), ZWNJ/ZWJ and
  variation selectors, which are not alphanumeric but do not end a word. std has no general
  category lookup, so these are ranges. Punctuation inside those blocks (Hebrew maqaf, the
  Indic dandas) still separates. So `pek` in `İpek`, `lise` in an NFD `E\u{301}lise`, and the
  letters after a virama or a ZWNJ are mid-word. The domain never counts, or `li` would rank
  every `@linkedin.com` row first. A name that is itself an address (an `@` and no whitespace)
  takes the address rule. Every char boundary is tried, so a mid-word occurrence does not hide
  a later word-start one. Rank `1` is any other literal occurrence; NULL is no match. After the
  rank, the order is relevance (nulls last), frequency desc, `last_seen` desc, then name.
  `Wei Li` therefore ranks with whole-name prefixes for `li`.
- The match is a plain `str::contains`, so `_`, `%` and `\` are literal and the old `LIKE`
  escaping is gone.
- An empty query matches every contact at rank 0, so the result is in the tie order. A
  negative `limit` means no limit (`LIMIT -1`). `Store::all_contacts()` returns every row in
  that same order, for a caller that does its own matching.
- Rejected: stored `name_folded`/`address_folded` columns. They need a migration with a Rust
  backfill, and every contact writer would have to keep them in step. The word-prefix rank
  needs Rust anyway, and a `%q%` search is a full scan either way.
- Known limits: no accent folding (`elise` does not find `Élise`), no spelling equivalences
  (`STRASSE` does not find `Straße`, since `to_lowercase` keeps `ß`), and no Unicode
  normalisation (an NFD name does not match an NFC query, and normalising needs a crate).
  `folder_contacts()`'s `COLLATE NOCASE` order and `is_contact`'s deliberately ASCII key are
  unchanged.
- Tests: store `search_contacts_folds_any_script`,
  `search_contacts_ranks_word_prefix_with_name_prefix`,
  `search_contacts_empty_query_and_negative_limit`,
  `all_contacts_lists_every_row_in_rank_order`,
  `search_contacts_finds_a_turkish_dotted_capital_by_any_case`,
  `contact_match_rank_word_starts_and_fold`,
  `contact_match_reads_a_null_argument_as_empty`; unchanged
  `search_contacts_ranks_prefix_first_then_frequency` and
  `search_contacts_treats_underscore_literally`.

## Folder writes: create, rename, move (#1910)

- `SyncCommand::{CreateFolder { parent, name }, RenameFolder { id, name }, MoveFolder { id,
  dest }}`. Each command's store write is one IMMEDIATE transaction that writes the folder row
  and queues the op (as the contact writes, #2063 r1), then `FoldersUpdated` and a drain.
  Graph: `POST /me/mailFolders` (`parent: None`) or `POST /me/mailFolders/{parent}/childFolders`
  with `{"displayName"}`; `PATCH /me/mailFolders/{id}` with `{"displayName"}`;
  `POST /me/mailFolders/{id}/move` with `{"destinationId"}`, `"msgfolderroot"` for `dest: None`
  (`GraphClient::{create,rename,move}_folder`; an answer without an id is `Parse`).
- Deleting a folder: see "Delete a folder (#2107)" below (split from #1910 after review).
- Store (`Store::{create_local_folder, rename_folder, move_folder}`, crate-private, as are their
  result types: hosts use the commands; `current_folder_id` is public). Each returns
  `FolderWriteResult::{Written(id), Unchanged, Refused(FolderRefusal)}`. A rename writes only
  `display_name`, a move only `parent_id`, so `sort_order`, `is_expanded` and the delta link stay.
  The same name or the same parent is `Unchanged`: nothing is queued and no event is sent. A
  folder made at the top (and a move to the top) stores the root id the listing gives top folders
  (`root_folder_id_on`: the parent most stored folders share that is not itself stored; NULL
  before any listing); Graph's move answer then stores the real one.
- Refused by the store, with nothing written or queued: an unknown id, parent or dest; the drafts
  sentinel (`local:drafts-pending`) as any of them, since Graph never had it; and a move into the
  folder itself or a folder under it (the stored `parent_id` would hold a cycle). The engine never
  refuses a folder for being well known; Graph does. Reported as `FolderWriteFailed { status:
  None }` plus `Error`; its `id` is the folder named, or a create's parent.
- A created folder lives under a `local:` id (`local_draft_id`) until its create reaches Graph.
  `Store::reconcile_created_folder` then re-keys it (`rekey_folder_on`): the row,
  `messages.folder_id` and the children's `parent_id`. Foreign keys are deferred to the commit
  (`PRAGMA defer_foreign_keys`, as `messages.folder_id` has no `ON UPDATE CASCADE`), so the row
  keeps every column, including ones added later. A stale row already under the new id hands its
  messages and children to the kept row before it goes. `folder_aliases (old_id → id)` records
  it, also when the row is gone. `Store::current_folder_id` resolves an old id in one lookup: a
  re-key re-points the aliases that named the old id. Every folder op, the message `Move`
  command and op (`dest`), `SyncFolder`, `SetFastFolders` and a scoped `SearchMessages` resolve
  through it. `SyncEvent::FolderCreated { local_id, id }` and `FoldersUpdated` follow a create,
  and the engine's fast folders are mapped through the alias.
- `CreateFolder` sends the name it was created with. A rename or move made before the create
  reached Graph is its own queued op, sent after it under the Graph id. A queued op on a folder
  still `local:` (its create was refused and discarded), or on a row that is gone, sends nothing.
  A create under a parent, or a move into a dest, that is still `local:` is refused (`Parse`). A
  message `Move` into a folder still `local:` sends nothing; it is only a guard, as a refused
  create drops every queued message move into its folder.
- A move whose answer has a new id re-keys the folder like a create, and also clears
  `delta_link` and the backfill columns, since the links name the old id; phase 1 runs again.
  The answer's parent is stored only while the stored parent is still the one the op wrote (a
  later queued move keeps its own). `Applied::FoldersChanged` then sends `FoldersUpdated`.
- Refusals: 401, a throttle (429 and 503), transport and other 5xx keep the op queued and in
  order, as for every op. Anything else (4xx, a 404, `Parse`) is final at once
  (`refuse_folder_write`): the op is dropped, the store reverted, and `FolderWriteFailed { id,
  write, status, error }`, `Error("{create|rename|move} folder {id}: {error}")` and
  `FoldersUpdated` are sent. There are no five attempts and no quarantine. The status comes from
  `refused_write_status`, shared with the contact writes.
- A refused create removes its `local:` folder and the `local:` folders created under it,
  found by their queued creates' parents, wherever they sit now (`discard_local_folder`,
  `refused_creates_on`, r10). Their creates are dropped, and each is reported:
  `FolderWriteFailed { write: Create, status: None, error: "its parent folder could not be
  created" }` plus its `Error`, after the refused folder's own, before the one `FoldersUpdated`.
  A folder only moved under the refused one stays, with everything under it: a Graph folder, or
  a `local:` one created elsewhere, whose create still goes to Graph where it was created. Its
  queued move into the refused folder is refused next (`Parse`: the destination never reached
  Graph) and reverts it to its `prev_parent`; until then it shows at the top. Mail Graph does not have in
  them (`unsent_mail_in_on`: a `local:` id, `local_edit > 0`, or a queued `SaveDraft`/`SendDraft`)
  is first filed back in Drafts (or the drafts sentinel), its own save or send still queued, with
  `MessagesUpdated` for that folder. Every queued message move into them is dropped, and when
  other mail was moved there, so are the queued message ops naming it
  (`drop_local_folder_mail_on`): a fresh delta skips every id a queued op names, and Graph still
  has that mail where it was. The mail then reconverges as after a quarantine
  (`start_mail_reconverge`, every delta link cleared), so it comes back where Graph has it. A
  read, flag or category change on such a message is lost with it. These drops happen mid-drain,
  after the drain read its ops, so the drain checks that each op is still queued
  (`Store::has_op`) before it sends it.
- A refused rename or move puts `prev_name` / `prev_parent` back only while the stored value is
  still the op's own (`revert_folder_name`, `revert_folder_parent`). Best effort: `prev` values
  are not chained (op1 A→B refused while op2 B→C waits leaves op2's `prev` B, a name or parent
  Graph never had), and the next listing, once no op protects the row, rewrites it.
- While an op is queued, a sync pass does not undo it (`sync_folders` reads
  `Store::queued_folder_writes` first): a folder a queued rename names keeps its stored name, and
  one a queued move names its stored parent; counts and the well-known name still update. A
  stored `local:` folder is kept without a `folder_exists` GET. No Graph read is sent for a
  `local:` folder: the pass's delta loop skips it (and so, now, the drafts sentinel, which used
  to get a delta Graph could only refuse), `refresh_folder` (`SyncFolder`, fast ticks) sends
  nothing for it, and a search scoped to it answers `MessageSearchResults` with no hits and sends
  no request. The backfill needs a delta link, which such a folder never has.
- A deferred op in the tests is answered 500, not 503: a 503 is a throttle, retried at once.
- lookxy ignores `FolderCreated` and `FolderWriteFailed`: it has no folder management.
- Tests: `client.rs` `create_folder_posts_top_level_and_child`,
  `create_folder_answer_without_an_id_is_a_parse_error`, `rename_folder_patches_display_name`,
  `move_folder_posts_destination_and_root`; store `folder_write_tests`
  (`folder_ops_round_trip_json`, `folder_writes_queue_their_op_in_the_same_transaction`,
  `create_at_the_top_stores_the_listed_root_as_parent`,
  `folder_writes_refuse_unknown_folders_and_the_drafts_sentinel`,
  `move_into_itself_or_a_folder_inside_it_is_refused`,
  `rename_and_move_keep_sort_order_and_expanded`,
  `discarding_a_local_folder_drops_the_ops_of_mail_moved_into_it`,
  `a_refused_create_files_unsent_mail_back_in_drafts`,
  `a_refused_create_discards_what_was_created_under_it_not_what_was_moved_there`,
  `reconcile_created_folder_rekeys_messages_children_and_aliases`,
  `reconcile_keeps_the_mail_of_a_stale_row_under_the_graph_id`,
  `reconcile_of_a_row_gone_still_aliases`, `current_folder_id_follows_chained_aliases`,
  `a_move_answered_with_a_new_id_rekeys_and_clears_the_links`,
  `a_move_answer_gives_the_parent_unless_a_later_move_wrote_one`,
  `revert_folder_name_keeps_a_later_rename`, `revert_folder_parent_keeps_a_later_move`,
  `discard_local_folder_leaves_folders_graph_has`); `outbox.rs`
  `create_folder_reconciles_the_local_id`,
  `create_folder_retry_after_reconcile_or_of_a_deleted_row_sends_nothing`,
  `create_folder_under_a_parent_that_never_reached_graph_is_refused`,
  `rename_and_move_after_a_create_reach_the_graph_id`,
  `move_folder_stores_the_answer_and_refuses_a_local_dest`,
  `message_move_into_a_created_folder_sends_its_graph_id`; `engine.rs` `folder_writes`
  (`create_folder_round_trip_at_the_top_and_under_a_parent`,
  `rename_folder_patches_and_keeps_local_state`,
  `move_folder_to_the_top_and_under_a_folder_stores_graphs_answer`,
  `rename_move_and_message_move_queued_behind_a_deferred_create_follow_it`,
  `refused_folder_writes_revert_and_report`,
  `refused_create_of_a_folder_holding_moved_mail_reconverges` (its dropped flag covers the
  `has_op` skip), `a_refused_create_files_a_draft_moved_into_it_back_in_drafts`,
  `a_sync_pass_keeps_folder_writes_still_queued`,
  `folder_commands_the_store_refuses_report_and_queue_nothing`,
  `a_folder_only_moved_under_a_refused_create_keeps_its_own_create`,
  `a_folder_created_under_a_refused_create_is_reported`,
  `fast_folders_follow_a_created_folder`).
- Not done: a lookxy folder-management UI; dedup of a create retried
  after a lost POST answer (a retry may 409 on the name Graph already has; the refusal discards
  the local row and the next listing brings Graph's folder); `GET /me/mailFolders/delta`; search
  folders; folders in other mailboxes; returning the `local:` id synchronously (a host finds the
  new row on `FoldersUpdated`; `FolderCreated` names both ids).

## Delete a folder (#2107)

- `SyncCommand::DeleteFolder { id }`. The rule: a folder delete never takes, on Graph, an item
  the store shows elsewhere. #1910's review found a new hole after each guard that trusted the
  store's state at send time, so this does not trust it: it records what the user saw once, and
  checks Graph's real contents against it right before the DELETE.
- Store (`Store::delete_folder`, one IMMEDIATE transaction): `id` and its subtree go (rows,
  mail, delta links; the mail by cascade). Refused, nothing written: an unknown folder, the
  drafts sentinel as `id` or in the subtree, and unsent mail in it (`unsent_mail_in_on`:
  `FolderRefusal::UnsentMail(n)`, "it holds n unsent message(s)"); refusing loses nothing,
  keeping would mean moving drafts first. The `local:` folders in it, plus those created under
  them and moved out since (`refused_creates_on`, reported like #1910 r10 with "its parent
  folder was deleted"), never reached Graph: their creates and other ops go
  (`drop_folder_ops_on`), Graph mail moved into them is dropped as a refused create drops it
  (`drop_local_folder_mail_on`, then a mail reconverge), and every queued `MoveFolder` into one
  goes, wherever the moved folder is now; the folder's next queued move inherits its
  `prev_parent`, and with none a folder outside the subtree is put back under it. Queued
  `MarkFolderRead`s of the subtree go too. The Graph roots are `id` (when Graph has it), each
  Graph folder moved locally under a removed `local:` one, and each Graph folder of the subtree
  with a queued `MoveFolder` (r1: once its row is gone the move would send nothing and Graph
  would keep it where it was, so its moves are dropped and it is deleted where it is): one
  `OutboxOp::DeleteFolder { id, batch }` each, sharing a `batch`. A root Graph has inside
  another one is done when its walk answers 404.
- The allow-list (`folder_delete_allow`, per batch): every Graph folder and every message
  (with its `internet_message_id`) the subtree showed, written before the rows go.
- Tombstones (`forgotten_ids`): triggers on `messages` and `folders`
  (`schema::FORGOTTEN_TRIGGERS_SQL`, made in `Store::init` after the migrations) record every
  Graph id a row was deleted under (any path: delta `@removed`, a cascade, a discard, a host's
  delete on its own connection) or re-keyed away from, with the message's internet message
  id; a row back under the id clears it. `local:` ids are not recorded. The batch's own items
  are cleared again by the delete. Pruned to the newest `FORGOTTEN_CAP` (100 000) by rowid at
  each delete and on open: an item pushed out by 100 000 later removals before its folder's
  delete is the residual risk, and the internet-message-id check still catches it while any
  row or tombstone holds that id.
- The drain (`run_delete_folder`, never `apply_op`, which answers it `Parse`): a
  `DeleteFolder` waits until no older op is queued (skipped with no attempt and no back-off;
  the next drain looks again), so a move queued before it lands or is given up first. A root
  still `local:` sends nothing. Then `GraphClient::folder_subtree_items` walks Graph's
  subtree (messages `$select=id,internetMessageId&$top=1000`, `childFolders` as
  `list_folders` sees them, no call for a leaf; `None` when the folder answers 404, which is
  done; a descendant's 404 leaves it out; any other failure an error, an item without an id
  `Parse`), and `Store::delete_allow_check` judges each item: allowed when the batch has it
  (through `moved_ids` / `folder_aliases`) and no row holds it now, or when the store never
  held it (no row, alias, tombstone or queued op naming it: mail never synced, or come since).
  Graph mints a new message id on a move (no `ImmutableId`), so a move whose answer was lost
  leaves an id the store never saw: per non-empty internet message id, the walked items are
  refused when they outnumber the batch's allowed ones and a row or a tombstone holds that id
  (a copy a rule filed beside its original passes; a copy not synced yet beside a stored one
  is a false refusal, on the safe side). Anything refused: no DELETE, `FolderWriteFailed {
  status: None, error: "it holds n item(s) the store shows elsewhere or removed" }` (a
  tombstone can also be the user's own message delete whose op was quarantined). Else
  `DELETE /me/mailFolders/{id}`; 2xx or 404 is done.
- Failures (`engine_op_call_failed`, shared with `MarkFolderRead`): auth, throttle and
  transport stop the drain, and a throttle holds the outbox for its `Retry-After`
  (`hold_outbox`, r1); another 5xx defers it (`defer_op`: attempts bumped, never
  quarantined); a 4xx or `Parse` refuses it (`refuse_folder_delete`): the op
  goes (the batch's allow-list with its last op), `FolderWriteFailed { write: Delete }` +
  `Error` + `FoldersUpdated`, and a listing after the drain (`relist_after_drain`) brings the
  folders back where Graph has them; their mail follows with their next delta (no link). A
  throttle mid-walk restarts the walk at the next drain. A well-known folder is walked before
  Graph refuses its DELETE: accepted.
- Each allow-listed folder records its `root` (the nearest root at or above it in the stored
  tree). While that root's op is queued (`QueuedFolderWrites::deleted`,
  `Store::folder_being_deleted`; r2: not while only another root of the batch is, so a refused
  root's folders come back at once), a listing skips the folder and every folder Graph lists
  under one (a folder kept by a
  queued move elsewhere stays where it is); a scoped search answers no hits with no request.
  The fast folders lose the removed ids at the command; fast ticks and `SyncFolder` already
  read only stored folders. A refused move whose `prev_parent` a delete removed (or that is
  tombstoned) is not reverted (`revert_folder_parent`): the folder stays where the user saw it.
- Graph semantics: the docs say only that `DELETE` deletes the folder (204);
  `POST .../permanentDelete` is the hard delete. The folder leaves the tree; should Graph place
  it under Deleted Items instead, the next listing shows it there. A UI that wants "delete to
  Deleted Items" sends `MoveFolder` there. The engine does not refuse well-known folders: Graph
  does.
- Known shapes: a message whose move into the deleted tree is refused stays on Graph where it
  was and comes back with its folder's next delta once the batch ends (a folder with a queued
  move into it is its own root instead, r1). The empty shell of a
  folder whose `MoveFolder` answer was lost and that was then moved back is not detected (the
  mail inside is, by its internet message ids). Another client changing Graph between the
  walk and the DELETE is out of scope. Hidden folders are never listed, so never held.
- Tests: store `folder_delete_tests` (`delete_folder_op_round_trips_json`,
  `delete_folder_drops_the_subtree_and_records_its_batch`, `delete_folder_refuses_unsent_mail`,
  `delete_folder_refuses_the_drafts_sentinel_and_unknown_folders`,
  `deleting_a_local_folder_queues_nothing_and_drops_the_mail_moved_into_it`,
  `a_graph_folder_moved_under_a_deleted_local_folder_is_its_own_root`,
  `a_move_into_a_deleted_local_folder_is_dropped_and_its_prev_parent_inherited`,
  `delete_folder_drops_a_queued_mark_folder_read_of_the_subtree`,
  `the_walk_allows_what_the_store_never_held`, `the_walk_refuses_an_item_a_row_shows_elsewhere`,
  `the_walk_refuses_what_the_store_held_and_no_longer_shows`,
  `the_walk_counts_internet_message_ids`, `a_revert_into_a_deleted_parent_leaves_the_folder`,
  `a_batch_ends_with_its_last_op`, `forgotten_ids_record_every_removal_path`); `client.rs`
  `delete_folder_sends_delete_and_reports_a_404`,
  `folder_subtree_items_pages_and_skips_a_child_gone`,
  `folder_subtree_items_root_gone_errors_and_ids`; `engine.rs` `folder_writes::delete_folder`
  (`delete_folder_walks_graph_then_deletes`, `a_folder_already_gone_is_done`,
  `a_refused_delete_reports_and_lists_the_folders_back`,
  `a_5xx_or_a_throttle_keeps_the_delete_queued`,
  `a_folder_moved_into_the_deleted_one_goes_with_it`,
  `a_refused_move_out_keeps_the_delete_from_taking_the_folder`,
  `a_graph_folder_moved_under_a_deleted_local_folder_is_deleted_by_its_own_op`,
  `nothing_is_read_for_a_folder_being_deleted`,
  `a_queued_mark_folder_read_of_the_subtree_is_dropped`, `the_delete_waits_for_older_ops`,
  `a_folder_whose_move_in_is_still_queued_is_deleted_where_it_is`,
  `a_refused_root_is_listed_back_while_another_root_waits`), store
  `a_graph_folder_with_a_queued_move_into_the_subtree_is_its_own_root`.
- `SyncEvent::FolderWriteFailed` for a delete: a 404 is done, not a failure; `status: None`
  is the store's refusal (unsent mail too) or the walk's; a refusal is followed by a relist,
  not a revert; the `Error` verb is `delete`; `id` is the root as queued, resolved.

## Mark a folder read (#1913)

- `SyncCommand::MarkFolderRead { folder }` marks the whole folder read, on Graph too, not only
  the mail the store holds. `EmptyFolder` (#2110, below) shares its `$batch` loop (`bulk_batches`).
- Store (`Store::mark_folder_read(folder, now_utc)`, crate-private): one IMMEDIATE transaction
  marks every stored message of the folder read, sets its `unread_count` to 0 and queues
  `OutboxOp::MarkFolderRead { folder, before }` (kind `markFolderRead`). It is all or nothing,
  and the test proves it on 5 000 rows with an aborting trigger. `before` is the cutoff: the later
  of the command's UTC time and the newest stored `received_at` in the folder. A fractional
  second rounds up to the next whole one (`ceil_to_whole_second`, carrying across a minute or
  a day), so Graph's `receivedDateTime le` still takes a message at `…07.512Z` (r1). Mail
  Graph receives after the command, or while the engine is offline, is not the command's, and
  a client clock behind Graph's still covers the mail the user saw. Every
  command queues its own op, with no dedup: `MarkFolderRead`, then `MarkRead` to unread, then
  `MarkFolderRead` again must end read. An unknown folder or the drafts sentinel is refused
  (`FolderWriteFailed { write: MarkRead, status: None }` plus `Error`), and nothing is written.
  On `Written`, `MessagesUpdated { folder_id }` and `FoldersUpdated` follow, then a drain.
- `messages_fts_au` fires only on an UPDATE of `id`, `subject`, `from_name` or `from_addr`
  (`schema::MESSAGES_FTS_AU_SQL`, installed and replaced in older databases by
  `migrate_fts_update_trigger`). Its `DELETE` scans the whole FTS table (`message_id` is
  `UNINDEXED`), so firing it on every read-state UPDATE took about 32 s for 5 000 rows. It now
  takes about 20 ms. `messages_fts_ai` still scans once per INSERT (not changed here).
- Graph (`GraphClient::batch`, `BatchRequest`, `BatchResponse`, `BATCH_MAX` = 20):
  `POST /$batch`. Sub-request ids are their 1-based index. An answer is matched back by id and
  returned in request order. A missing, duplicate or unknown id is `Parse`, and so is more than
  20 requests (nothing is sent). A sub-response's `Retry-After` is read case-insensitively. A
  sub-response 401 makes the whole call `Unauthorized`. `GraphClient::mark_read_request(id)`
  is `PATCH /me/messages/{id}` `{"isRead":true}` with `Prefer: return=minimal`, which Graph
  answers with 204. `unread_message_ids(folder, before)`:
  `GET /me/mailFolders/{id}/messages?$filter=isRead eq false and receivedDateTime le {before}
  &$select=id&$top=1000`, following `@odata.nextLink`, ids deduplicated.
- The engine runs the op itself (`run_mark_folder_read`). `apply_op` answers it `Parse` and
  sends nothing, and `folder_op()` leaves it out. The first step pages every id into `bulk:
  Option<BulkRun>` before the first PATCH, because paging an `isRead eq false` listing while
  PATCHes clear it shifts its skip-token pages and skips ids. The run then sends 20 ids a batch.
  A batch's ids leave the run only once Graph has answered.
  - A 2xx or a 404 (the message is gone) is done.
  - A 429 or 503 puts its ids back at the front and stops the drain for the batch's longest
    `Retry-After` (default 30 s). `schedule_retry` holds the passes, and `hold_outbox` sets the
    drain's own gate (`op_retry`, a later one kept), because `next_retry` alone would let a
    command's drain (`drain_queued`) send the throttled ids again at once (r1). A whole-call
    `Throttled` holds the outbox the same way, and `due` wakes the loop when the hold ends.
  - Another 5xx puts its ids back under the outbox back-off (`op_retry`; the op's attempts are
    bumped, and it is never quarantined).
  - Any other status is recorded and reported once the rest are done: `"{n} of {total} messages
    could not be marked read: {first error}"`.

  `checkpoint()` runs between batches, and a stop there keeps the run. A whole-call failure of
  the listing or of a batch takes the drain's usual arms: auth, throttle and transport stop it,
  and a 5xx defers it. Anything else (a 404, another 4xx, `Parse`) is final
  (`refuse_mark_read`). A run whose op is gone is cleared at the next drain. A restart loses
  the run, and the op pages again, which is safe, since mail already read is not listed.
- A refusal drops the op, with no attempts and no quarantine. It sends `FolderWriteFailed { id,
  write: MarkRead, status, error }`, then `Error("mark all read in folder {id}: {error}")`,
  and clears that folder's links (`Store::restart_folder_delta`, the statement a moved folder's
  re-key uses), so the next passes read its read states again. In whole-mailbox mode, rows older
  than the head window are corrected when the folder's backfill runs again.
- A folder still `local:` when the op drains (its create never reached Graph) sends nothing, and
  the op is dropped.
- While the op is queued (`QueuedFolderWrites::marked_read`, folder → latest `before`), a pass
  keeps the folder's stored `unread_count`. This covers the listing (`sync_folders`) and one
  folder's count read (`refresh_folder`, `kept_unread_count`). `merge_delta_items` (heads,
  stored-link rounds, backfill pages) stores some items read (`read_guard`, `ReadGuard`, read
  once per page):
  - before the run has paged, an item received at or before the cutoff;
  - once it has paged, only an id the run still holds in `todo` (r1). Mail that came into the
    folder after the listing, for example moved in with an older `receivedDateTime`, is never
    PATCHed, so it keeps Graph's state.

  An id another queued op names (`protected_ids`) is never forced, on stored-link rounds too,
  so a `MarkRead` to unread queued after the op keeps its own state (r1). Newer mail keeps
  Graph's state, so the count can read 0 beside a newer unread row until the op drains.
  `FetchMessage`, search hits and header repair are not guarded: each mark the op makes comes
  back as a delta change.
- Known limit: a long throttled run holds every op queued behind it, sends too, until it ends.
  Graph throttles per mailbox, so a send would be throttled meanwhile anyway. The drain order is
  what keeps a later per-message `MarkRead` or `Move` correct, so don't reorder.
- lookxy has no "Mark all as read" yet.
- Tests: `client.rs` `batch_posts_numbered_sub_requests_and_matches_answers_by_id`,
  `batch_reads_retry_after_from_a_sub_response`, `batch_sub_response_401_is_unauthorized`,
  `batch_answer_missing_an_id_is_a_parse_error`, `batch_of_more_than_20_is_refused`,
  `batch_status_is_classified`, `unread_message_ids_follows_next_link_and_dedups`; store
  `mark_folder_read_tests` (`mark_folder_read_op_round_trips_json`,
  `mark_folder_read_marks_zeroes_and_queues_in_one_transaction`,
  `mark_folder_read_is_all_or_nothing_on_5000_rows`,
  `mark_folder_read_refuses_unknown_and_the_drafts_sentinel`,
  `a_second_mark_folder_read_after_a_mark_unread_is_queued_again`,
  `mark_folder_read_cutoff_is_the_later_of_now_and_the_newest_row`,
  `ceil_to_whole_second_rounds_a_fraction_up_with_carries`,
  `mark_folder_read_follows_a_folder_alias`, `a_read_state_update_leaves_the_fts_row_alone`,
  `migrate_fts_update_trigger_replaces_an_old_trigger_once`,
  `restart_folder_delta_clears_only_that_folder`); `outbox.rs`
  `apply_op_mark_folder_read_sends_nothing`; `engine.rs` `folder_writes::mark_folder_read`
  (`mark_folder_read_pages_and_batches_every_server_unread_id_once`,
  `a_429_inside_a_batch_is_retried_after_retry_after`,
  `a_sync_pass_keeps_a_queued_mark_folder_read`,
  `before_the_run_pages_mail_up_to_the_cutoff_is_stored_read`,
  `mark_folder_read_leaves_mail_received_after_the_command_unread`,
  `mail_the_paged_run_does_not_hold_keeps_graphs_state`,
  `a_mark_unread_queued_after_the_op_keeps_its_state`,
  `a_throttled_batch_holds_a_commands_drain_until_retry_after`,
  `mark_folder_read_refusals_report_and_restart_the_folder_delta`,
  `mark_folder_read_survives_a_restart`,
  `mark_folder_read_of_an_unknown_folder_reports_and_queues_nothing`).

## Empty a folder (#2110)

- `SyncCommand::EmptyFolder { folder, subfolders }` empties a folder on the server too. It is
  #1913's sibling (the `$batch` loop) and #2107's (the allow-list check), and follows their
  rules. The rule is #2107's: a delete never takes, on Graph, a message the store shows
  elsewhere.
- Store (`Store::empty_folder(folder, now_utc)`, one IMMEDIATE transaction, all or nothing; the
  test proves it on 5 000 rows with an aborting trigger). An unknown folder or the drafts
  sentinel is refused (`FolderWriteFailed { write: Empty, status: None }` + `Error`), and nothing
  is written. Unsent mail (`unsent_mail_in_on`: a `local:` id, `local_edit > 0`, a queued
  `SaveDraft`/`SendDraft`) stays. Refusing the command for it, as a folder delete does, would make
  "Empty Drafts" useless while one draft is open. Every other row of the folder goes (bodies,
  attachments and headers by cascade), recorded first in the allow-list (`folder_delete_allow`,
  kind `message`, batch `empty:…`) with its internet message id. Its tombstones are cleared as
  `delete_folder` clears its own. The counts become those of the kept rows. The folder's delta and
  backfill links are cleared (`clear_folder_links_on`): no stored link resumes over the removed
  mail, and no backfill picks the folder (`incomplete_histories` needs a delta link). Then
  `OutboxOp::EmptyFolder { folder, before, batch }` (kind `emptyFolder`) is queued. `before` is
  `mark_folder_read`'s cutoff (`folder_cutoff_on`, shared). Every command queues its own op.
- FTS: `messages_fts_ad` and `bodies_fts_ad` scan the whole FTS table per deleted row
  (`message_id` is `UNINDEXED`). They are now `schema::FTS_DELETE_TRIGGERS_SQL`, run by `init`
  right after `SCHEMA_SQL`, with the text unchanged. `empty_folder` drops both inside its
  transaction, deletes the folder's FTS rows in one statement, deletes the rows, and runs that same
  text again before the commit. DDL is transactional, so a rollback restores them; the test
  compares `sqlite_master` byte for byte after a commit and after a rollback. Measured in a
  release build, deleting 5 000 rows:

  | FTS rows of other folders | per-row triggers | `empty_folder` |
  |---|---|---|
  | 0 | 5.4 s | 0.10 s |
  | 20 000 | 43.5 s | 0.04 s |

  With 1 000 rows the per-row triggers took 0.17 s and `empty_folder` 0.01 s. The schema change
  makes lookxy's connection re-prepare its statements once, which is harmless.
- The command emits `MessagesUpdated { folder }`, then `FoldersUpdated` and a drain. With
  `subfolders`, each folder stored right under it (read before the write) is deleted from the
  store too (`Engine::delete_folder_write`, the store half of `delete_folder`), before the one
  drain (r1): the children leave the UI at once, and their `DeleteFolder`s, queued after the
  `EmptyFolder` and waiting for it, survive a restart. Each child is refused or not on its own,
  and is reported as #2107 reports it.
- The drain (`run_empty_folder`, never `apply_op`, which answers it `Parse`; `folder_op()` leaves
  it out). Like a `DeleteFolder`, it waits until no older op is queued (`has_older_op`, no
  attempt, no back-off). The drain passes a non-final 4xx, so without this a move into the folder
  refused once would land after the empty and bring the mail back. A folder still `local:` sends
  nothing: the op and its allow-list go. The first step pages every message Graph holds there
  received at or before `before` (`GraphClient::folder_messages_before`:
  `$filter=receivedDateTime le {before}&$select=id,internetMessageId&$top=1000`, following
  `@odata.nextLink`, deduplicated, an item without an id `Parse`), all before the first DELETE,
  since deleting while paging shifts the pages. It then checks them with
  `Store::delete_allow_check(batch, &[], items)`.
  - The refused ids are sorted by `Store::empty_refused` (r1). One stored in another folder, or
    not stored (a tombstone, an internet message id held elsewhere), is kept and reported: the
    store shows it elsewhere or removed it.
  - One stored in this very folder that is unsent (`unsent_mail_in_on`) is the command's own
    choice: left alone and not reported.
  - One stored in this folder that the batch allows (mail the user emptied that a fetch of a
    stale search hit or a list row by id stored there again) is deleted with the rest.
  - One stored in this folder that the batch does not allow came in after the command (a later
    move): left alone, not reported.
  - The rest go 20 a `$batch` (`GraphClient::delete_message_request`: `DELETE /me/messages/{id}`,
    the soft delete `OutboxOp::Delete` sends), through `bulk_batches`, the loop
    `run_mark_folder_read` now shares (`BulkRun` has a `kind`, `kept` and `rows_deleted`).
    A 2xx or 404 is done, and the stored row of that id goes too, wherever it is, should one
    have come back meanwhile (a search hit, a fetch); `MessagesUpdated` then follows at the end.
    429/503: back at the front, `hold_outbox` for the longest `Retry-After`. Another 5xx: back,
    `defer_op`. Anything else is recorded.
- The end (`finish_empty_folder`): `Store::finish_folder_empty(seq, batch, folder)` drops the op
  and the batch's allow rows and clears the folder's links in one transaction, so no crash leaves
  the op gone with an old link in place. The next pass reads the folder afresh: mail that came
  after the listing, and what the run kept or could not delete. One report if anything failed or
  was kept: `FolderWriteFailed { write: Empty, status }` + `Error("empty folder {id}: …")`, the
  error joining `"{n} of {total} messages could not be deleted: {first error}"` and `"{k} of
  {total} messages were kept: the store shows them elsewhere or removed"` with `"; "`. `status` is
  the first refused id's, `None` when only kept mail is reported. `total` counts what the run took
  on: listed, less what it left alone (the unsent mail, and mail moved in since).
- Whole-call failures go through `engine_op_call_failed`: auth, throttle and transport stop the
  drain, and a 5xx defers. Anything else (a 404, another 4xx, `Parse`) refuses it
  (`refuse_empty_folder`): `finish_folder_empty`, `bulk` cleared, and the report. The cleared
  links bring back the mail Graph still has. A restart loses the run, and the op pages again,
  which is safe: deleted mail is no longer listed, and the allow-list is in the store.
- While the op is queued (`QueuedFolderWrites::emptied`, `Store::folder_being_emptied`) nothing is
  read for the folder. `sync_folder_delta` returns at once, so a pass, a fast tick and the
  post-send sync skip it; `refresh_folder` (`SyncFolder`, fast ticks) sends no delta and no count
  read. A listing keeps its stored `total_count` and `unread_count`, and a search scoped to it
  answers no hits with no request. Guarding delta items one by one was rejected: any page the guard
  misjudged would bring a deleted item back. Not guarded: an unscoped search hit stored, a
  `FetchMessage`, a list row by id (#2053). The run deletes the row of each id Graph answered for,
  and the end's fresh delta re-reads the folder.
- `Store::delete_folder` (#2107) of the folder or a folder above it drops a queued `EmptyFolder`
  and its allow-list (`drop_folder_ops_on`), and `drain_outbox` then drops a run whose op is
  gone. The delete's walk still allows the mail the empty removed because the store never held
  it any more: there is no row, and the empty cleared its own tombstones, so no tombstone either
  (the internet-message-id rule then finds no row or tombstone). Don't "fix" that clearing.
- Known shapes: mail moved into the folder on Graph by a rule or another client after the
  command, with an old `receivedDateTime`, is deleted (never held, before the cutoff), as #1913
  marks it read. The cutoff takes this client's clock (r1-m2): with it Δ ahead of Graph's, mail
  delivered to the folder up to Δ after the command passes the filter and, never held, is
  deleted unseen. A server time source (the `Date` header) would close it; none is read today. While the op is queued no new mail shows in the folder (often Inbox, a fast
  folder); a long throttled run makes that visible. A long run holds every op queued behind it,
  as #1913's does. Another client changing the folder between the listing and the deletes is out
  of scope, as are "empty to Deleted Items" (a move), `permanentDelete`, emptying subfolders'
  mail without deleting them, and a lookxy UI (lookxy ignores `FolderWrite::Empty`).
- Tests: store `empty_folder_tests` (`empty_folder_op_round_trips_json`,
  `empty_folder_removes_the_folders_mail_zeroes_and_queues_in_one_transaction`,
  `empty_folder_is_all_or_nothing_on_5000_rows` (with 10 000 FTS rows of another folder: over 30 s
  in a debug build with the per-row triggers), `empty_folder_keeps_unsent_mail_and_counts_it`,
  `empty_folder_fts_rows_go_and_the_fts_triggers_come_back`,
  `empty_folder_refuses_unknown_and_the_drafts_sentinel`, `empty_folder_allow_list_and_tombstones`,
  `empty_folder_cutoff_is_the_later_of_now_and_the_newest_row`,
  `delete_folder_drops_a_queued_empty_folder_and_its_allow_list`,
  `empty_refused_sorts_what_the_check_refused`); `client.rs`
  `folder_messages_before_filters_pages_and_dedups`, `delete_message_request_is_a_delete`;
  `outbox.rs` `apply_op_empty_folder_sends_nothing`; `engine.rs` `folder_writes::empty_folder`
  (`empty_folder_pages_and_deletes_every_server_id_once`,
  `a_429_inside_a_batch_is_retried_after_retry_after`,
  `a_sync_pass_reads_nothing_for_a_folder_being_emptied`,
  `empty_folder_keeps_mail_the_store_shows_elsewhere`,
  `empty_folder_waits_for_an_older_move_into_it_and_deletes_it_under_its_new_id`,
  `empty_folder_keeps_unsent_drafts_and_reports_nothing`,
  `empty_folder_refusals_report_and_restart_the_folder_delta`, `empty_folder_survives_a_restart`,
  `empty_folder_with_subfolders_deletes_the_children` (the children gone before the run's
  listing answers), `an_emptied_message_stored_again_in_the_folder_is_deleted`,
  `mail_moved_into_the_folder_while_the_empty_waits_is_left_alone`,
  `a_delete_of_the_folder_drops_its_queued_empty`,
  `empty_folder_of_an_unknown_folder_reports_and_queues_nothing`).

## Copy a folder (#2118)

- `SyncCommand::CopyFolder { id, dest }` copies a folder, with its subfolders and their mail,
  under `dest` (`None`: the top, `msgfolderroot`), as Outlook's Copy Folder does. Graph:
  `POST /me/mailFolders/{id}/copy` with `{"destinationId"}` (`GraphClient::copy_folder`, the
  body shared with `move_folder` as `destination_body`; an answer without an id is `Parse`).
  It is built from #1910's create (a `local:` row re-keyed on the answer) and move pieces.
- Store (`Store::copy_folder(source, dest)`, one IMMEDIATE transaction): a new `local:` row
  under `dest` (or the listed root, as `create_local_folder` stores a top folder), with the
  source's name and counts, no mail, no subfolders, no `well_known_name`; then
  `OutboxOp::CopyFolder { id, source, dest }` (kind `copyFolder`). The store never copies mail
  rows: Graph copies them under new ids. Refused, nothing written (`FolderWriteFailed { write:
  Copy, status: None }` + `Error`): an unknown source or dest, the drafts sentinel as either.
  Nothing else: a dest that is the source or under it makes no cycle (the copy is a new row),
  and Graph decides; a copy beside the original is a real request too (never `Unchanged`).
- The `local:` copy is a local creation wherever the store special-cases `CreateFolder`:
  `refused_creates_on` counts it as created under its `dest`, so a copy queued into a refused
  or deleted `local:` folder goes with it; `discard_local_folder` and `drop_folder_ops_on` drop
  its op; `queued_op_ids_on` names its id, source and dest. Such a discarded copy is reported as
  a copy of its source (`LocalFolderDiscard::discarded_copies`, `FolderDelete::discarded_copies`,
  `(copy id, source)`): "its destination folder could not be created" after a refused create
  or copy, "its destination folder was deleted" after a delete. A copy whose *source* never
  reaches Graph (its create refused, a `local:` source deleted) is not discarded up front: its
  op is refused when sent (`Parse`, "source folder never reached Graph"), the path a move into
  a refused folder takes.
- The op (`sync/outbox.rs` `copy_folder`): the copy row gone, or its id already re-keyed (a
  retry), sends nothing. `source` and `dest` are resolved at send time; either still `local:`
  is `Parse`. The source row is not required: a `DeleteFolder` of the source queued after the
  copy removes the row at once, and Graph keeps the folder until that delete runs, after the
  copy. Then `reconcile_created_folder` re-keys the copy (its parent and counts come with the
  listing) and the drain gets `Applied::FolderCreated`, as for a create:
  `SyncEvent::FolderCreated { local_id, id }` and `FoldersUpdated`, the same meaning for a host
  ("the folder you saw as `local_id` is `id` now"). No `FolderCopied` event.
- A copy is a snapshot of Graph, so `drain_outbox` sends it only once no older op is queued
  (`has_older_op`, as a `DeleteFolder` or `EmptyFolder`). Without it, a copy queued after an
  `EmptyFolder` waiting behind a move refused once (a 4xx the drain passes) would copy the mail
  the user had emptied. A copy that waits is also a barrier (r1): the drain stops there
  (`break`, no deferral, so the caller's sync carries on and the post-drain steps run), and
  nothing queued after it is sent before it. Skipping it instead (as a waiting `DeleteFolder`
  is skipped) let newer ops run: a rename or move of the copy's `local:` id sent nothing and
  was dropped, a folder created or a message moved into it was refused or dropped, and a
  delete or move of the source's mail reached Graph before the copy. The oldest op is never
  held, a 4xx'd one is quarantined after `MAX_OP_ATTEMPTS`, and a bulk run ends, so the queue
  still moves.
- After the drain: a copy that reached Graph sets `relist_after_drain`, and after that one
  listing (which brings the copy's subfolders) `sync_copied_folders` runs `refresh_folder`
  (delta, then counts) for the copy and every stored folder under it, each id resolved then
  (a later op may have re-keyed it), a row gone skipped. A stop there is `Drain::Aborted`; the
  copy has no delta link, so the next pass syncs it anyway.
- A Graph refusal (any 4xx, `Parse`; final at once, as #1910's folder writes) drops the op and
  removes the copy with `discard_local_folder` (what was created or copied under it, drafts
  filed back in Drafts, a mail reconverge for mail moved into it), reported as
  `FolderWriteFailed { id: <source>, write: Copy, status }` + `Error("copy folder {source}: …")`
  + `FoldersUpdated`. The report names the source, the folder the command named.
- Known shapes, not fixed: a `DeleteFolder` or `EmptyFolder` of a copy whose mail has not synced
  yet protects that mail (#2107's allow-list never saw it; the copies share the source's
  internet message ids); a copy retried after a lost answer may 409 or make a second copy on
  Graph (the next listing shows Graph's truth, as for a create); the copy shows the source's
  name and counts until the listing after it. Out of scope: copying into another mailbox, search
  folders, a lookxy Copy Folder UI (lookxy ignores `FolderWrite::Copy`).
- Tests: `client.rs` `copy_folder_posts_destination_and_root`,
  `copy_folder_answer_without_an_id_is_a_parse_error`; store `folder_write_tests`
  (`copy_folder_op_round_trips_json`,
  `copy_folder_adds_a_local_row_with_the_source_counts_and_queues_its_op` (a dest that is the
  source or under it is not refused), `copy_folder_refuses_unknown_folders_and_the_drafts_sentinel`,
  `a_refused_create_discards_a_copy_queued_into_it`, `deleting_a_local_copy_drops_its_op`);
  `outbox.rs` `copy_folder_reconciles_the_local_id`,
  `copy_folder_retry_after_reconcile_or_of_a_deleted_row_sends_nothing`,
  `copy_folder_refuses_a_local_source_or_dest`,
  `copy_folder_of_a_source_being_deleted_is_still_sent`;
  `engine.rs` `folder_writes::copy_folder`
  (`copy_folder_round_trip_syncs_the_copy_and_its_subfolder`,
  `a_copy_into_or_of_a_created_folder_follows_its_create`,
  `a_refused_copy_removes_the_local_copy_and_reports` (409 with a folder and a copy under it,
  then 400), `a_copy_waits_for_an_older_empty_of_its_source`,
  `ops_queued_after_a_waiting_copy_wait_for_it` (r1: a delete of the source's mail, a rename
  of the copy and a folder created under it, each sent after the copy, under its Graph id)), and
  `folder_commands_the_store_refuses_report_and_queue_nothing` (copy refusals).

## Message and folder sizes (#2123)

A host can draw classic Outlook's Folder Size dialog. The Local Data tab comes from the store
(`Store::folder_sizes`) and the Server Data tab from Graph (`SyncCommand::FetchFolderSizes`).

- Source: `PidTagMessageSize` (tag `0x0E08`, PT_LONG) as `model::MESSAGE_SIZE_PROP`
  (`Integer 0x0E08`), one more `or id eq` term in `client::MESSAGE_EXPAND`, so a delta, `get_message`,
  `get_message_hit` and a search hit bring it with no extra request. `model::parse_message_size`
  is lenient: a string (as Graph sends extended values) or a JSON integer, non-negative; no
  array, no entry or anything else is `None`. The id matches `is_message_size_prop`
  (`is_tag_prop(id, "Integer", 0x0E08)`: `Integer 0xe08` matches, the folder's `Long 0x0E08`
  does not).
- Store: `messages.size INTEGER` (NULL = unknown) and `messages.size_asked INTEGER NOT NULL
  DEFAULT 0`; `MessageRow::size: Option<i64>` on every read (`MESSAGE_ROW_COLUMNS` gains `size`
  last, `BODY_AFTER_ROW_COLUMNS` is 32, and `conversations_in_folder` spells it twice).
- Merge: `size = COALESCE(excluded.size, messages.size)`. A carried size writes. Anything else
  keeps the stored one: a partial item, a refused or dropped expand, a link minted before #2123
  or a code-built `Message`. A size never goes from known to unknown, so no `MessageFields`
  flag is needed. A local draft (`create_local_draft`) reads `None` until Graph's copy brings
  one. `reconcile_id` and `follow_move` re-key the row and keep it. The FTS update trigger reads
  other columns (#1913), so no size write touches `messages_fts`.
- Migration `migrate_message_size` is the last one in `Store::init`: one IMMEDIATE transaction
  that probes `pragma_table_info('messages')` for `size` and adds both columns. Unlike
  #2081/#2088/#2096, it resets **no** delta or backfill state. The size fill covers both the
  rows stored before #2123 and the rows a link minted before it brings later.
- The size fill (idle, `Engine::next_size_fill` / `size_fill_step`, `EngineOptions::size_fill`,
  on by default; `EngineOptions::without_prefetch` turns it off for the older engine tests):
  - Candidates (`unsized_where!`, spelled the same in the partial index
    `idx_messages_unsized ON messages(folder_id)` and in the queries that read it): `size IS
    NULL AND size_asked = 0`, a non-empty `received_at`, and an id not `local:`. A local draft
    and a header-less row would stay unsized for ever, so they are not in the index, which
    holds nothing once every row is sized. The engine probes it on every idle turn
    (`Store::size_fill_folders`: Graph folders in `Store::folders` order, no `local:` folder and
    not the drafts sentinel; a test checks that the plan reads only the index). The index is
    created in `Store::init` after the migration, never in `schema.rs`.
  - `Store::size_fill_candidates(folder)` also leaves out `pending_message_ids` (passed as a
    `json_each` array): a row moved here locally sits under its old id until the drain follows
    the move (#1896), and Graph has no such id in this folder. It returns the ids and the
    oldest `received_at`. A folder in the probe with no candidate is parked for the pass (no
    request). Before **every** page, not only the first, a folder gone from the store, being
    deleted (#2107) or being emptied (#2110) drops the fill and parks the folder quietly: no
    request and no `Error`.
  - Order in `main_loop`: the prefetch, then the backfill, then the size fill. A ready fill
    makes the wait zero, and its step runs only when `!self.due()`, so a queued command and a
    due wake go first.
  - One page per idle step: `GraphClient::message_sizes_target(folder, oldest)` is
    `GET /me/mailFolders/{id}/messages?$filter=receivedDateTime ge {oldest}&$select=id&$top=1000
    &$expand=` its size alone. `message_sizes_page` then follows each `@odata.nextLink`. Graph
    may return fewer items than `$top`, and the loop never assumes a full page.
  - Each page goes to `Store::set_message_sizes` in one transaction (`UPDATE … SET size = ?
    WHERE id = ? AND size IS NULL`): the size only, never over a known one, an unstored id
    skipped. A page that wrote a row emits `MessagesUpdated`.
  - The last page calls `Store::mark_size_asked(folder, candidates)`. It sets `size_asked = 1`
    on the candidates that are still unsized, still in the folder and still not pending (a
    local move during the fill stays unmarked). These are messages gone or moved on the server,
    or with no property. No row is asked twice. Rows that arrived during the fill get their own
    fill.
  - The fill's state (folder, candidates, next page) is in memory only. A restart starts the
    folder over: rows already filled are no longer candidates. Sign-out and sign-in drop it
    and its parked folders (`drop_size_fill`), and a pass clears the parked set.
  - Failures: 401, 429, a whole-call 503 (`classify_status` makes it `Throttled`) and transport
    go through `react`, and the page is asked again after. Another 5xx parks the folder until
    the next pass, and the fill starts over then. Any other 4xx (a 404 folder, a refused
    `$filter` or `$expand`) or a parse error gives up: the candidates are marked asked, with one
    `Error`, and the folder is parked. If the store cannot mark them, the folder is parked as
    well (with the store's `Error`), so the refused request is not sent again before the next
    pass. There is no `SyncState` and no `FolderProgress`.
  - Accepted costs, against one re-enumeration of every folder:
    - Through a link minted before #2123, each new mail in a folder costs one small idle listing
      for as long as that link lives, because links are not reset here.
    - An old-dated message moved into a folder from another device lists that folder from its
      date (a 2019 message in a 20 000-message folder: about 20 pages).
    - Rejected alternatives: a per-id `$batch` (Graph's throttling counts each sub-request) and a
      per-folder "done" mark (a pre-#2123 link's new mail would then stay unsized for ever).
- `Store::folder_sizes() -> Vec<FolderSize { folder_id, messages, sized_bytes, unsized_count }>`
  has one entry per `folders` row (an empty folder reads zeros), sorted by `folder_id`. It is
  one grouped read (`LEFT JOIN` over an aggregate) and touches no body. `unsized_count` counts
  every row with `size IS NULL`, local drafts included, so a host can say a figure is
  incomplete. (The field is not named `unsized`, which is a reserved word in Rust.) A total with
  subfolders is the host's sum over `parent_id`.
- `SyncCommand::FetchFolderSizes { folder_ids }` answers `SyncEvent::FolderSizesFetched { sizes:
  Vec<(String, Option<i64>)> }` (one entry per id asked, in order, as asked) or
  `FolderSizesFailed { folder_ids, error }`, never a `SyncEvent::Error`. Nothing is stored.
  - It is served in `checkpoint` ahead of a running pass, and in the main loop, like
    `SearchMessages` (#2006), and is tried during a back-off.
  - Each id maps through `current_folder_id`. A `local:` folder, or one being deleted or
    emptied, answers `None` with no request. An empty list answers `[]` with no request. Signed
    out, it answers `FolderSizesFailed { error: "not signed in" }`.
  - `GraphClient::folder_size_request(id)` is `GET /me/mailFolders/{id}?$select=id&$expand=`
    `PidTagMessageSizeExtended` (`model::FOLDER_SIZE_PROP`, `Long 0x0E08`, PT_I8: the folder's
    items without its subfolders). The requests go `engine::FOLDER_SIZE_BATCH` = 4 a `$batch`:
    Exchange allows 4 concurrent requests per app per mailbox and evaluates every sub-request
    against that, so 20 GETs to one mailbox come back with `MailboxConcurrency` 429s.
  - Answers: a 2xx reads `model::parse_folder_size` (string or number, non-negative, else
    `None`), and a 404 is `None`. A sub-response 429 or 503 fails the command and backs off
    (`react_quietly(Throttled { Retry-After or 30 })`, as the bulk loop reads a sub-response
    and `classify_status` a whole call). Any other sub-status fails it alone, and a whole-call
    error fails it (auth, throttle and transport also through `react_quietly`). There is no
    retry loop and no sleep on the engine thread.

## Shutdown persists queued writes (#2127)

A host that sends many per-message writes (`MarkRead`, `Delete`, `Move`) and then `Shutdown`
must not lose the ones the engine has not reached. Each such command is `enqueue_and_drain`:
one Graph round trip per op, serially. Before #2127 the `Shutdown` was honoured only after every
one of them had been sent, so a host that waits a bounded time at quit lost the tail. The store
already showed the host's side (row deleted, row read), so nothing looked wrong until a later
quarantine re-listed the mailbox and brought the lost writes back.

- `Engine::take_waiting` moves every command on the channel into `deferred`, in order, serving
  none. A `Shutdown` or a dropped handle sets `shutting_down` and ends the take (commands sent
  after the `Shutdown` stay unread).
- `drain_outbox` returns `Drain::Aborted` at its top while `shutting_down`, and calls
  `take_waiting` before each op it is about to send (after the `has_op` skip, before the
  `DeleteFolder`/`EmptyFolder`/`MarkFolderRead`/`apply_op` dispatch), returning `Aborted` once
  it sees the shutdown. So an idle engine that gets N writes and a `Shutdown` in a burst sends
  at most the one request already in flight when the burst raced the engine's read of the
  first command.
- `Engine::checkpoint` is built on `take_waiting`: it reads the channel up to a waiting
  `Shutdown` first, then serves the kept commands of a served kind (`served_at_checkpoint`:
  `FetchBody`, `SearchMessages`, `FetchMessage`, `FetchFolderSizes`) out of `deferred`, one at
  a time, taking again after each. A read sent before a `Shutdown` is therefore never served,
  in the bulk runs' checkpoints inside a drain too. Outside a shutdown the order is unchanged:
  a served kind in `deferred` is one the drain took early, which the old `checkpoint` would have
  read from the channel and served ahead of the deferred writes the same way, and `main_loop`
  still runs `deferred` before the channel. Without this, a read a drain took (`Delete,
  Refresh, FetchBody` in a burst) would wait for the whole `Refresh` pass (#1978's regression
  guard: `a_read_taken_by_a_drain_is_served_at_the_next_pass_checkpoint`).
- `main_loop`, once `shutting_down`, pops `deferred` and runs a command only when
  `persists_at_shutdown` (an exhaustive `match`, no `_` arm, so a new command forces a
  decision). Its store write and `enqueue_op` happen as in `handle_command`; its drain is cut at
  the top, and `refresh_if_calendar_reset_owed` returns `false` when shutting down. Every other
  kept command is dropped. Then the thread returns, and the next start drains the outbox.
  - Persisted: the message writes (`MarkRead`, `SetFlag`, `SetCategories`, `Move`, `Delete`,
    `SaveDraft`, `SendDraft`, both message reminders), the event writes (`RespondEvent`,
    `CreateEvent`, `UpdateEvent`, `DeleteEvent`, `CancelEvent`, `ForwardEvent`,
    `UpdateEventSeries`, both event reminders), the contact writes (their ops were queued by the
    host's store write; the arm only drains), the folder writes (`CreateFolder` to
    `EmptyFolder`), and also `SetFastFolders` (in memory), `SetCalendars` (the selection is
    stored; `set_calendars` skips its `refresh_calendar` when shutting down, and the next pass
    refreshes) and `SignOut` (the token cache is cleared; later writes still queue, and with no
    token nothing drains).
  - Dropped: every read and pass (`Refresh`, `SyncFolder`, `SignIn`, the `Fetch*` commands,
    `SearchMessages`, `SaveAttachment`, `RefreshCalendar`, ...), and the writes that reach
    Graph outside the outbox, which have nothing durable to persist: `RespondMeeting` and
    `RemoveFromCalendar` (they read the meeting first), `SetAutomaticReplies` (a direct PATCH),
    and `ComposeReply`/`ComposeForward` (`createReply`).
- A drain cut by the shutdown after an op went through returns before its post-drain syncs
  (`sync_after_send`, `relist_after_drain`, `sync_copied_folders`). A copy applied just before it
  gets its contents synced by the next start's pass, as after a bulk run's checkpoint stop.
- Not done: the drain is no faster (no `$batch` for per-message ops), and hosts still do not
  wait for the engine thread at quit (`lookxy` sends `Shutdown` and returns; a bounded wait
  for `evt_rx` to disconnect is the host's change).
- Tests (`engine.rs` `tests::shutdown_persists`): 20 `Delete`s against a `DELETE` paced 500 ms,
  then a `Shutdown` or a dropped handle: the engine returns within 3 s with at most one sent,
  and a second engine on the store sends the rest. Commands sent during a paced startup pass
  are persisted with no write, body, search or calendar request. And the #1978 guard above.
