# lookxy v2 — Compose / reply / forward (rich HTML + drafts) — design

Give lookxy the ability to **write** mail, not just read and triage it: a
rich-text compose view (bold/italic/underline/lists), reply / reply-all /
forward with the original quoted, draft persistence, and send — all riding
lookxy v1's existing rails (auth-code+PKCE, the SQLite store, the
optimistic-store + outbox sync model, the ratatui TUI).

## 1. Goals / non-goals

**Goals**
- Compose a new message with a **rich-text body** (paragraphs; bold, italic,
  underline; bulleted/numbered lists; links), To/Cc recipients, and subject.
- **Reply / reply-all / forward** an open message, with the original thread
  quoted and recipients pre-filled (via Graph's reply/forward draft endpoints).
- **Drafts**: save a half-written message to the Drafts folder and resume it
  later; editing a reply/forward is editing its draft.
- **Send** through the same optimistic-store + outbox path as triage — the UI
  never blocks on the network; the send retries in the background.
- Reuse docxy's editing engine via a shared **`editcore`** crate rather than
  writing a second editor.

**Non-goals (this feature)**
- Attachments *on outgoing* mail (compose-time attach) — a follow-up; v2 send
  is body + recipients + subject.
- Tables, images, or full Word-style formatting in the body — the editor is a
  focused rich-*text* editor (paragraphs, inline emphasis, lists, links), not a
  document editor.
- Signatures, templates, scheduling/send-later, encryption/S-MIME.
- Contact/directory autocomplete for recipients (v2 accepts typed addresses;
  autocomplete is a later addition).

## 2. New shared crate: `editcore`

docxcore already holds the *editable model* (paragraphs of styled runs); the
*edit engine* (cursor, selection, edit operations, undo/redo) lives in the
**docxy binary** and is not currently a library. This feature extracts the
reusable editing core so both docxy and lookxy share one implementation.

`editcore` (pure `std`, no TUI) provides:
- `RichText` — an editable buffer: `Vec<Block>` where `Block` is a `Paragraph`
  of styled `Run`s (`text`, `bold`, `italic`, `underline`, `link: Option<…>`) or
  a `ListItem { ordered, level, runs }`. (A deliberately smaller model than
  docxcore's full OOXML AST — the email-body subset.)
- `Cursor` / `Selection` — position (block, run, char offset) and range.
- Edit ops: `insert_text`, `delete`, `split_paragraph`, `merge`,
  `toggle_bold`/`italic`/`underline` over the selection, `make_link`,
  `list_toggle`/`indent`/`outdent`.
- `History` — undo/redo command stack.
- No rendering and no I/O — it is a headless, unit-testable editing core.

docxy is refactored to build its editor on `editcore` (its OOXML model maps
onto `RichText` for the paragraph/run/list subset it already edits); anything
docxy-specific (tables, styles, page geometry) stays in docxy. This refactor is
scoped to *reuse*, not to change docxy's behavior — docxy's tests must stay
green. The extraction is the largest single piece of this feature and is
sequenced first in the plan, behind its own review gate.

## 3. Body HTML serialization

- `mailcore` gains `compose_html.rs`: `RichText → email HTML` — `<p>` per
  paragraph, `<br>` for soft breaks, `<b>/<i>/<u>` for emphasis, `<ul>/<ol>`
  + `<li>` for lists, `<a href>` for links; entity-escaped, inline-styled only
  (no external CSS), email-client-safe. Also emits a **plain-text alternative**
  (the body flattened) so Graph sends `contentType: html` with a text fallback
  where applicable.
- The **reverse** (loading a reply/forward draft's quoted HTML into the editor)
  reuses lookxy's existing `htmlrender.rs` tokenizer to parse the draft body's
  HTML into `RichText` (the same tag set the reading pane already handles);
  unknown/rich constructs degrade to text, matching the reading-pane philosophy.

## 4. Reply / forward / new via Graph

- **New**: build a `RichText`, serialize to HTML, create a draft or send.
- **Reply / reply-all / forward**: call Graph `POST /me/messages/{id}/createReply`
  / `createReplyAll` / `createForward` — each returns a **draft message** with
  the quoted original body and recipients pre-populated. lookxy stores that draft
  locally, parses its HTML body into `RichText`, and opens the editor on it. The
  user edits above the quote and sends.
- Graph client (`graph/client.rs`) gains: `create_reply(id, kind)`,
  `create_forward(id)`, `create_draft(payload)`, `update_draft(id, payload)`,
  `send_draft(id)`, `send_mail(payload)` (direct send without a persisted draft,
  used only if draft creation is skipped). All go through the existing
  `with_auth` refresh/throttle wrapper.

## 5. Drafts + send (rides the outbox)

A draft is a message with `isDraft=true` in the Drafts folder — it syncs and
displays like any other message (the Drafts folder already appears in the
folder pane). Compose flow:

1. On entering compose (new/reply/forward), a **local draft row** is created
   (and, for reply/forward, the Graph draft from §4); edits update the local
   `bodies`/`messages` rows immediately.
2. **Save draft** (Esc / explicit): enqueue `SyncCommand::SaveDraft{id}` — the
   engine `create_draft`/`update_draft`s to Graph and reconciles the id. The
   local draft persists across restarts (it's in the store).
3. **Send**: apply optimistically (move the local message to Sent, mark sent),
   enqueue `SyncCommand::SendDraft{id}` (or `SendMail{payload}` for a
   never-persisted draft) → the engine ensures the draft exists on Graph, then
   `send_draft`s it, with the same **quarantine-after-N-attempts** and
   reconverge policy as triage ops. A failed send surfaces via `SyncEvent::Error`
   (the status-bar error surface from v1).

New `OutboxOp` variants: `SaveDraft{id}`, `SendDraft{id}`. New `SyncCommand` /
`SyncEvent`: `SaveDraft`/`SendDraft`/`ComposeReply`/`ComposeForward` down;
`DraftReady{id}` / `Sent{id}` up.

## 6. Store changes

- `messages` already models drafts (a draft is a message). Add columns the
  composer needs if absent: `is_draft`, `to_recipients`/`cc_recipients` are
  already stored. A `drafts`-specific concern: the **local-only** in-progress
  draft (before it has a Graph id) needs a stable local id — use a
  `local:<uuid>` id, reconciled to the Graph id on `SaveDraft`.
- Bodies for drafts are stored in `bodies` like any message body (HTML +
  the editor round-trips through it).

## 7. TUI

- **Compose view** — a full-screen surface (over the three panes): header rows
  for **To**, **Cc**, **Subject** (single-line text fields with basic editing),
  then the **rich body editor** (an `editcore` buffer rendered to ratatui —
  emphasis via `Style`, lists indented, a visible cursor/selection), and a
  bottom action bar: **Send** (`Ctrl-Enter`), **Save draft** (`Esc`), **Discard**
  (`Ctrl-D`), plus formatting keys (`Ctrl-B/I/U`, list toggle).
- **Entry points** from the message list / reading pane: `c` new, `r` reply,
  `R` reply-all, `f` forward. Reply/forward open the composer on the Graph draft.
- **Drafts folder**: selecting a draft opens it in the composer to resume.
- Field focus cycles To→Cc→Subject→Body with Tab; recipient fields accept
  comma/`;`-separated addresses.

## 8. Error handling

- Network down while composing: editing is fully local; Save draft persists
  locally and syncs when back (offline-first). Send queues in the outbox.
- Graph send failure: quarantine + `SyncEvent::Error` to the status bar; the
  draft is NOT lost (stays in Drafts).
- Malformed recipient: validated on send (basic `x@y` shape); the composer flags
  it inline rather than sending.
- No panic paths in the editor on empty buffer / boundary cursor moves
  (editcore is unit-tested for these).

## 9. Testing

- `editcore`: headless unit tests for every edit op, cursor/selection math,
  undo/redo, and boundary cases (empty buffer, delete-across-blocks, list
  in/out) — the bulk of the correctness surface, no TUI needed.
- `compose_html`: `RichText → HTML` round-trips (and HTML→RichText via
  htmlrender for reply-quote loading); emphasis, lists, links, escaping.
- `graph::client`: the new reply/forward/draft/send methods against the
  in-process fake server (recorded Graph responses).
- `store` / `sync`: draft persistence, local→Graph id reconciliation, the
  SaveDraft/SendDraft outbox ops (drain, retry, quarantine) on temp DBs +
  fake server.
- `lookxy` TUI: compose-view render + key handling (field focus, formatting
  toggles, send/save/discard) via `TestBackend`.
- docxy: its existing suite must stay green after the `editcore` extraction.
- CI needs no network or account.

## 10. Build order (for the plan)

1. **`editcore`** — extract the editable model + edit ops + undo from docxy into
   the new crate; refactor docxy onto it; docxy tests green. (Largest; own gate.)
2. `mailcore::compose_html` — RichText↔HTML.
3. `graph::client` reply/forward/draft/send methods (+ fake-server tests).
4. `store` draft/local-id support; `sync` SaveDraft/SendDraft outbox ops + engine.
5. `lookxy` compose TUI (fields + editcore body editor + action bar).
6. Entry points (c/r/R/f, Drafts resume) + wiring; docs.
