# lookxy outbound attachments + signatures — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Builds on:** lookxy v1 (mailcore store + sync + Graph client + outbox), v2 (compose via editcore, drafts + send), v3 (thread view), v4 (contacts/autocomplete/Bcc).

## Goal

Let the user attach local files to a compose draft (new / reply / forward),
upload them to the draft on Microsoft Graph when the message is sent, and
auto-append a configurable signature to new messages.

## Product decisions (locked)

1. **Any-size attachments.** Files ≤3MB upload inline as a Graph
   `fileAttachment`; larger files use a Graph upload session (chunked `PUT`).
2. **Upload at send time only.** Attachments are stored locally as file
   references and uploaded to the Graph draft during `SendDraft` (not
   `SaveDraft`) — the local table is the single source of truth, cleared after
   a successful send, so there is no duplicate-upload bookkeeping.
3. **File-picker popup.** File selection is a filesystem-browser popup, built
   by generalizing the existing list-popup UI (the move-picker / attachments
   popup pattern) rather than a new widget.
4. **Signature on new messages only.** A configurable `signature` string is
   appended to a brand-new compose's body; replies/forwards are left untouched.

## Architecture

Attachments are **references to local files** (path + name + size), not bytes:
the store keeps the reference, the bytes are read from disk lazily at send
time. The compose view accumulates references (via the file picker) into a new
`outbound_attachments` table keyed by draft id. On `SendDraft`, the outbox —
after ensuring the draft exists on Graph — reads each reference's bytes and
uploads them (inline or chunked by size), then sends, then clears the
references. Signatures are a config string applied at compose-open. Clean
responsibility split:

- **mailcore/store** owns the `outbound_attachments` table + CRUD, and
  re-points its rows on `reconcile_id`.
- **mailcore/graph** owns attachment upload (inline + upload session) and a
  standard-base64 encoder.
- **mailcore/sync/outbox** uploads pending attachments in the `SendDraft` path.
- **lookxy** owns the file-picker popup, the compose attachment list/keys, and
  the signature application.
- **config** owns the `signature` string.

## Components

### 1. Store — `outbound_attachments`

New table, one row per (draft, file):

| column     | meaning                                        |
|------------|------------------------------------------------|
| `draft_id` | the draft this file is attached to             |
| `path`     | absolute local filesystem path to the file     |
| `name`     | display/attachment file name (basename)        |
| `size`     | file size in bytes (for display + threshold)   |

Primary key `(draft_id, path)` (attaching the same path twice is a no-op).
`OutboundAttachment { path: String, name: String, size: i64 }`.

Methods on `Store`:
- `add_outbound_attachment(draft_id, path, name, size)` — insert-or-ignore.
- `outbound_attachments(draft_id) -> Vec<OutboundAttachment>` — ordered by name.
- `remove_outbound_attachment(draft_id, path)`.
- `clear_outbound_attachments(draft_id)` — after a successful send, or on discard.

`reconcile_id(local_id, graph_id)` (which already re-points the draft row/body
when a `local:` draft first gets a Graph id) must ALSO
`UPDATE outbound_attachments SET draft_id = graph_id WHERE draft_id = local_id`,
so attachments added before the first save aren't orphaned.

Migration adds the table via the store's `SCHEMA_SQL` (`CREATE TABLE IF NOT
EXISTS`, reached on every `open`).

### 2. Graph client — attachment upload

New, on `GraphClient` (the client only *downloads* attachments today):

- `add_attachment(message_id, name, content_type, bytes: &[u8]) -> Result<(), GraphError>`
  — the dispatcher: if `bytes.len() <= INLINE_MAX` (3 MB = 3 * 1024 * 1024)
  calls `add_file_attachment`, else `upload_large_attachment`.
- `add_file_attachment(...)` — `POST /me/messages/{id}/attachments` with body
  `{"@odata.type":"#microsoft.graph.fileAttachment","name":name,"contentType":content_type,"contentBytes":base64(bytes)}`.
- `upload_large_attachment(...)` — `POST /me/messages/{id}/attachments/createUploadSession`
  with `{"AttachmentItem":{"attachmentType":"file","name":name,"size":len,"contentType":content_type}}`,
  read `uploadUrl` from the response, then `PUT` the bytes to that URL in
  chunks whose size is a multiple of 320 KiB (Graph's requirement — e.g.
  3,932,160 bytes = 12 × 320 KiB), each with a
  `Content-Range: bytes {start}-{end}/{total}` header, until the last chunk
  returns 201/200. The upload URL is a full absolute URL
  (pre-authenticated by Graph) — `PUT` it directly, no bearer header needed.

Content type: derived from the file extension via a small built-in map with an
`application/octet-stream` fallback (no new dependency).

Needs a **standard base64 encoder** (RFC 4648 §4, with `+`/`/` and `=` padding)
alongside the existing decoder — the `pkce` module's base64url encoder uses a
different alphabet and no padding, so it can't be reused for `contentBytes`.

### 3. Outbox — upload on `SendDraft`

`apply_op`'s `SendDraft` arm currently does `ensure_draft_on_graph` → `send_draft`.
Insert an upload step between them: after `ensure_draft_on_graph` returns the
Graph id, for each `store.outbound_attachments(graph_id)` read the file's bytes
(`std::fs::read`) and `client.add_attachment(graph_id, name, content_type, &bytes)`;
then `send_draft`; then `store.clear_outbound_attachments(graph_id)`. `SaveDraft`
is unchanged (no upload). A file-read or upload error returns `Err`, so the
existing drain retry/quarantine policy (and the UI error notice) applies; the
attachments are NOT cleared on failure, so a retry re-uploads cleanly (the send
hasn't happened, so no partial message escapes).

### 4. Compose UI — attach, file picker, list

- **Attach key:** `Ctrl+O` in the compose view opens the file picker (a free
  chord — the plan must confirm it isn't already bound in compose).
- **File picker popup** (`lookxy`): a filesystem browser built by generalizing
  the existing list-popup pattern. State: the current directory and its
  entries (subdirectories first, then files, each with a size for files).
  Keys: ↑/↓ move, Enter on a directory navigates into it (`..` goes up), Enter
  on a file attaches it and closes the popup, Esc cancels. Starts at the user's
  home directory. Hidden/inaccessible entries are skipped defensively.
- **Compose attachment list:** `Compose` gains
  `attachments: Vec<OutboundAttachment>`, loaded from
  `store.outbound_attachments(draft_id)` when the composer opens and refreshed
  on attach/remove. Drawn as a one-line "Attachments:" row (names + sizes,
  truncated) between the header fields and the body. Removal is LIFO: `Ctrl+R`
  in compose pops the most-recently-added attachment off both the list and the
  store, repeatable — no per-item selection UI in v1 (the plan confirms `Ctrl+R`
  is a free chord).
- Attaching: file picker returns a path → `store.add_outbound_attachment(draft_id, path, basename, size)` → refresh `compose.attachments`.

### 5. Signature

- `Config` gains `signature: String` (default `""`), loaded from
  `config.json` (and a `LOOKXY_SIGNATURE` env override, for parity with the
  other config fields).
- `compose_new` (the `c` new-message path that creates a blank local draft and
  opens it) appends the signature to the editcore body — a blank line, a `--`
  separator line, then the signature text — only when the signature is
  non-empty. Reply/forward paths do not touch it.

## Data flow

```
Attach:  Ctrl+O → file picker → pick file P
         → store.add_outbound_attachment(draft_id, P, basename(P), size(P))
         → compose.attachments = store.outbound_attachments(draft_id)

Send:    apply_compose_action(Send) → update_draft_fields → SyncCommand::SendDraft
         → outbox SendDraft:
             graph_id = ensure_draft_on_graph(id)      // create/update draft, reconcile id
             for a in outbound_attachments(graph_id):
                 bytes = fs::read(a.path)
                 client.add_attachment(graph_id, a.name, content_type(a.name), &bytes)  // inline or chunked
             send_draft(graph_id)
             clear_outbound_attachments(graph_id)
```

## Error handling & edge cases

- **File deleted/unreadable at send** → `fs::read` errors → the `SendDraft` op
  fails and is retried/quarantined by the existing outbox machinery; the send
  has not happened and attachments are not cleared, so a fixed file lets a
  retry succeed. Surfaced via the existing `error_notice`.
- **Upload-session chunk failure** → the op errors and retries from scratch
  (a fresh upload session) on the next drain — no partial send, since
  `send_draft` runs only after all attachments upload.
- **Reconcile before first save** → `reconcile_id` re-points
  `outbound_attachments.draft_id`, so files attached to a `local:` draft
  survive the first `create_draft`.
- **Discard** → clearing the local draft also clears its
  `outbound_attachments` (no orphaned rows).
- **Empty signature** → `compose_new` appends nothing.
- **Same file attached twice** → PK `(draft_id, path)` makes the second a no-op.
- **Graph's ~150MB message ceiling** → surfaced as Graph's error, not
  pre-checked locally.

## Testing

**mailcore (unit):**
- `outbound_attachments` CRUD (add/list/remove/clear); `reconcile_id` re-points
  attachment rows from `local:` to the Graph id.
- Standard base64 encode against known vectors.
- `add_file_attachment`: the POSTed body is a `fileAttachment` with the right
  name/contentType and base64 `contentBytes` (captured-body assertion).
- `upload_large_attachment`: `createUploadSession` then the expected sequence
  of chunked `PUT`s with correct `Content-Range` headers (FakeServer captures
  the requests); the 3MB threshold routes inline vs session correctly.
- outbox `SendDraft`: uploads each pending attachment then sends then clears;
  on a file-read/upload failure the op errors and does NOT clear (retry-safe).

**lookxy (unit + render):**
- File picker: directory listing (dirs before files), navigation into a dir and
  up via `..`, Enter-on-file returns the path, Esc cancels.
- Compose: `Ctrl+O` opens the picker; attaching adds to the store + the
  in-memory list; remove drops it; the attachments row renders name + size.
- Signature: `compose_new` with a non-empty config signature appends the `--`
  block to the body; with an empty signature appends nothing; reply/forward do
  not get a signature.

## Scope boundaries (YAGNI)

- **No re-attaching an incoming message's attachments** on forward — the user
  attaches local files only.
- **No inline/embedded images** in the body.
- **No in-app signature editor** — `config.json` (or `LOOKXY_SIGNATURE`) only,
  plain text appended to the HTML body as text.
- **Signature on new messages only** — not reply/forward.
- **No attachment preview** in the picker beyond name + size.
