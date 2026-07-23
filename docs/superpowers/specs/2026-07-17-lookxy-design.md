# lookxy — Outlook TUI for Exchange Online (design)

A terminal mail client in the spirit of docxy/xlsxy/yppxy: read and triage
corporate Microsoft 365 mail from the terminal. Verified target: the EPAM
tenant is fully on Exchange Online (MX → `*.mail.protection.outlook.com`,
autodiscover → `autodiscover.outlook.com`, managed Entra tenant
`b41b72d0-4e9f-4c26-8a69-f949f367c91d`), so the backend is the **Microsoft
Graph REST API**. EWS is retired for Exchange Online in October 2026 and
IMAP is typically disabled in corporate tenants; neither is targeted.

## 1. Goals / non-goals

**Goals (v1)**

- Sign in once via OAuth2 authorization-code + PKCE flow (system browser +
  loopback redirect); stay signed in via cached refresh tokens
  (DPAPI-encrypted on Windows).
- Sync mail to a local SQLite store with Graph delta queries; the UI reads
  only from the local store and is fully usable offline.
- Outlook-like three-pane TUI: folder tree | message list | reading pane.
- Read messages: HTML rendered to styled terminal text; attachments listed,
  saved to disk, opened.
- Triage: mark read/unread, flag/unflag, delete, move to folder — applied
  optimistically to the local store and pushed to Graph via an outbox queue
  with retry.
- Local full-text search (SQLite FTS5) over subject/sender/body.

**Non-goals (v1)**

- Compose / reply / forward.
- Calendar, meeting invites, rules, categories editing.
- Multiple accounts; non-Graph backends (IMAP, EWS, on-prem Exchange).
- Pixel-faithful HTML rendering; the reading pane is a reduced, readable
  view (same philosophy as docxy's document rendering).

## 2. Existential risk — VALIDATED 2026-07-17

Everything depended on EPAM's conditional-access policies issuing a delegated
mail token. This was validated by throwaway spikes before any code:

- **Device-code flow is BLOCKED** by EPAM Conditional Access. The Microsoft
  Office first-party client is not installed in the tenant at all
  (`AADSTS700016`); other public clients (Graph CLI, Azure CLI) issue a
  device code, but completing the browser login returns *"sign-in was
  successful but … an authentication flow that is restricted by your admin"*
  — the classic **"Block device code flow"** CA control.
- **Authorization-code + PKCE (interactive system browser + loopback
  redirect) PASSES.** Using the **Microsoft Graph CLI** public client
  (`14d82eec-204b-4c2f-b7e8-296a70dab67e`) with a `http://localhost:<port>`
  redirect, the flow returned an access token carrying `Mail.ReadWrite` plus
  a refresh token, and `GET /me/messages?$top=5` listed the real inbox. The
  Azure CLI client (`04b07795-…`) fails this flow with `AADSTS65002`
  (not preauthorized for Graph), so the Graph CLI client is the one to use.

**Consequence for the design:** lookxy authenticates via **auth-code + PKCE
with a loopback redirect**, not device-code. See §4.

**Security caveat surfaced by the spike:** the Graph CLI client comes with a
very broad pre-consented scope set (Directory.ReadWrite.All,
Group.ReadWrite.All, and more). lookxy only ever *calls* mail endpoints, but
the cached token nominally carries those rights. A dedicated EPAM app
registration scoped to just `Mail.ReadWrite` (delegated) is the cleaner
long-term option and is noted as a v2/hardening item; using the Graph CLI
client is acceptable for the personal-use v1 since the token only ever grants
the user's own existing privileges.

## 3. Crate layout

Follows the house pattern (core engine crate + TUI crate):

- **`mailcore`** — headless engine, no TUI dependencies:
  - `json` — hand-rolled JSON parser/serializer (same spirit as docxcore's
    XML; keeps serde out of the tree).
  - `auth` — OAuth2 authorization-code + PKCE flow (opens the system browser,
    runs a transient `http://localhost:<port>` loopback listener to catch the
    redirect), token refresh, token cache (DPAPI-encrypted file on Windows via
    `windows-sys`; mode-0600 plain file on Unix).
  - `graph` — thin Graph REST client over `ureq` (blocking, `rustls` TLS):
    typed wrappers for the endpoints we use, paging, `Prefer:
    outlook.body-content-type` handling, throttling/429 back-off.
  - `store` — SQLite via `rusqlite` (bundled) with FTS5: folders, messages,
    bodies, attachment metadata, outbox, sync state.
  - `sync` — the sync engine: initial windowed backfill + delta loop +
    outbox push. Runs on a caller-provided thread; communicates via
    channels.
- **`lookxy`** — ratatui/crossterm TUI binary, reusing docxy's UI
  conventions (ribbon, mouse support, status line, key help).

New workspace dependencies: `ureq`, `rustls` (via ureq), `rusqlite`
(bundled SQLite, FTS5 feature), `windows-sys` (DPAPI). This is a deliberate,
discussed break from the pure-std core ethos: TLS and a battle-tested store
are not things to hand-roll; everything above them (JSON, OAuth, Graph
protocol, sync) stays from-scratch.

## 4. Auth

- **Authorization-code + PKCE** flow against
  `https://login.microsoftonline.com/organizations/oauth2/v2.0/{authorize,token}`
  with scopes `Mail.ReadWrite offline_access` (delegated). Device-code flow is
  NOT used — EPAM Conditional Access blocks it (§2).
- Interactive step: generate a PKCE `code_verifier`/`code_challenge` (S256),
  bind a transient loopback listener on `http://localhost:<ephemeral-port>`,
  open the system browser at the `/authorize` URL
  (`response_type=code`, `redirect_uri=http://localhost:<port>`,
  `code_challenge`, `state`), capture the redirect's `code`, and POST it to
  `/token` with the `code_verifier` to obtain the token set. `state` is
  verified; the listener serves a small "you can close this tab" page and
  shuts down.
- Client ID: **`14d82eec-204b-4c2f-b7e8-296a70dab67e`** (Microsoft Graph CLI
  public client — validated in §2). Overridable via `LOOKXY_CLIENT_ID` so
  users in other tenants can substitute their own app registration.
- Token cache: JSON blob `{refresh_token, access_token, expiry, account}`
  encrypted with `CryptProtectData` (per-user DPAPI) at
  `%LOCALAPPDATA%\lookxy\token.bin`. Access tokens refreshed proactively by
  the sync thread; an invalid/expired refresh token surfaces in the UI as a
  "sign in again" banner that re-runs the browser flow, never a crash.

## 5. Storage (SQLite)

DB at `%LOCALAPPDATA%\lookxy\<account>\mail.db` (account = UPN, sanitized).
Schema v1:

- `folders(id TEXT PK, parent_id, display_name, total_count, unread_count,
  delta_link, well_known_name, sort_order)`
- `messages(id TEXT PK, folder_id, conversation_id, subject, from_name,
  from_addr, to_recipients, cc_recipients, received_at, sent_at, is_read,
  is_flagged, has_attachments, importance, preview)`
- `bodies(message_id TEXT PK, content_type, content)` — fetched eagerly for
  the backfill window, on demand otherwise.
- `attachments(id TEXT, message_id, name, content_type, size, is_inline)` —
  metadata only; bytes downloaded on save/open, not stored.
- `outbox(seq INTEGER PK AUTOINCREMENT, op TEXT, message_id, payload TEXT,
  attempts, last_error)` — pending local mutations.
- `messages_fts` — FTS5 (subject, from, body text) kept in step by the
  store layer.
- `meta(key, value)` — schema version, backfill window, account info.

All writes go through the store layer in transactions; the DB is the single
source of truth for the UI.

## 6. Sync engine

One background thread owned by mailcore, talking to the UI over two
channels (`SyncEvent` up, `SyncCommand` down) — matching the repo's
synchronous, worker-thread style (no async runtime).

- **Initial backfill:** enumerate folders, then per folder pull messages
  newer than the configured window (default 6 months) via
  `/mailFolders/{id}/messages/delta`, storing the final `deltaLink`.
  Bodies for the window fetched eagerly (batched); older mail is invisible
  in v1 unless the window is widened in config.
- **Steady state:** every N seconds (default 60, plus a manual refresh key)
  replay each folder's `deltaLink` to pick up new/changed/deleted messages.
- **Outbox push:** after each delta pass (and immediately on user action),
  drain `outbox` in order: `PATCH` for read/flag, `POST …/move`, `DELETE`.
  On failure: exponential back-off with jitter, honor `Retry-After` on 429,
  keep the op queued; after repeated hard failures (4xx other than 429)
  surface the error in the UI and drop the op with the local change
  reverted.
- **Conflicts:** server wins on incoming delta except where a newer local
  outbox op targets the same message (op replays after).

## 7. TUI

Three-pane layout, docxy conventions (ribbon on top, status line at
bottom, mouse + keyboard):

- **Folder pane** (left, collapsible): tree with unread counts; well-known
  folders (Inbox, Sent, Drafts, Deleted, Junk, Archive) pinned first.
- **Message list** (middle): sender, subject, time, flags; unread bold;
  sorted newest-first; infinite scroll within the synced window.
- **Reading pane** (right or full-screen toggle): headers block, then the
  body rendered by a small hand-rolled HTML→styled-text pass built on the
  existing XML tokenizer (headings, bold/italic/underline, links as
  footnote-style refs, blockquote indentation for reply chains, `<table>`
  as docxy-style grid where feasible). Fallback for pathological HTML:
  Graph's server-side text conversion (`Prefer:
  outlook.body-content-type="text"`), fetched on demand.
- **Actions:** `m`/`u` read-unread, `f` flag, `d`/`Del` delete, `v` move
  (folder picker), `/` search (FTS5, results as a virtual list), `Enter`
  open, `a` attachments (list → save/open), `r` manual refresh.
- **Status line:** sync state (idle / syncing / offline / N ops pending /
  sign-in required), account, folder counts.

## 8. Error handling

- Network down: sync thread flips to offline state, retries with back-off;
  UI stays fully functional on the local store.
- Graph throttling (429/503): honor `Retry-After`; never tight-loop.
- Auth expiry: "sign in again" banner that re-runs the browser (auth-code)
  flow; store untouched.
- DB corruption: detected on open (integrity check); offer rebuild
  (delete + full resync) rather than limping.
- Any sync error is a status-line state + log line, never a panic; the
  sync thread is unwind-safe and restartable.

## 9. Testing

- `mailcore::json` — table-driven parser/serializer tests incl. malformed
  input.
- `mailcore::graph` + `auth` — tests run against an in-process fake HTTP
  server (std `TcpListener` speaking canned HTTP/1.1) with recorded Graph
  response fixtures; no network, no real account, TLS off in tests (plain
  `http://127.0.0.1` allowed only under `cfg(test)`).
- `mailcore::store` — temp-file DBs; schema migration, FTS, outbox
  ordering.
- `mailcore::sync` — fake server + temp DB end-to-end: backfill, delta
  add/change/delete, outbox retry, 429 back-off.
- `lookxy` UI — same approach as docxy's existing TUI tests (widget-level
  render assertions).
- CI needs no secrets and no network.

## 10. Security notes

- Tokens: DPAPI-encrypted at rest; never logged.
- Mail store: plaintext SQLite under the user profile (same trust model as
  Outlook's own OST); directory created with default per-user ACLs. At-rest
  encryption is out of scope for v1 and noted as a possible v2 item.
- TLS: rustls with system roots (`rustls-native-certs`) so corporate
  TLS-inspection root CAs keep working.

## 11. Build order (for the implementation plan)

1. Spike: auth-code + PKCE token + list inbox (throwaway, proves §2). DONE.
2. `mailcore::json`.
3. `mailcore::auth` (+ token cache).
4. `mailcore::graph` client + fake-server test rig.
5. `mailcore::store` (schema + FTS + outbox).
6. `mailcore::sync` (backfill → delta → outbox).
7. `lookxy` TUI shell (panes, navigation) over a synced store.
8. Reading pane HTML rendering; attachments.
9. Triage actions wired to outbox; search UI.
10. Polish: status line, sign-in banner, config file, docs.
