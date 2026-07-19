# lookxy non-file attachment handling — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Sub-project:** A of the "receive attachments" split (B = inline image
rendering, already shipped). This spec covers **itemAttachment and
referenceAttachment handling in the attachments popup only**.
**Builds on:** the attachments popup (`lookxy/src/ui/attachments.rs`,
`App::open_attachments_popup`/`save_attachment`/`save_and_open_attachment`/
`finish_attachment_save`/`open_with_os_handler`), the Graph client
(`list_attachments`/`get_attachment_bytes`), the store attachments table +
idempotent-migration pattern (from the `content_id` work), and the
`SaveAttachment`→`AttachmentSaved` outbox path.

## Goal

Make the two non-file attachment kinds do the right thing in the `a` popup
instead of failing with "response has no contentBytes field":

- **`itemAttachment`** (a nested email/event forwarded as an attachment) →
  **download as `.eml`/`.ics`** (via Graph `/$value`), decided by sniffing the
  bytes.
- **`referenceAttachment`** (a OneDrive/SharePoint cloud link) → **open the
  cloud link in the browser** (no local download).

`fileAttachment` behavior (the current save/save+open) is unchanged.

## Background — why it's broken today

`get_attachment_bytes` reads the attachment's `contentBytes` (base64), which
ONLY `fileAttachment` has. `list_attachments` returns all three kinds, so an
`itemAttachment` or `referenceAttachment` appears in the popup, but pressing
Enter/`o` fetches `contentBytes` → Graph returns none → `GraphError::Parse` →
save fails. Neither kind is distinguishable in the model today
(`AttachmentMeta` doesn't carry `@odata.type`).

## Product decisions (locked)

- **itemAttachment → download.** Enter saves to Downloads; `o` saves + opens.
  Extension: **sniff the `/$value` bytes** — starts with `BEGIN:VCALENDAR` →
  `.ics`, else `.eml`.
- **referenceAttachment → open link.** BOTH Enter and `o` open `source_url`
  with the OS handler (browser). There is no local file, so "save" maps to
  "open the link". A status notice reports "Opened link: {name}".
- **No `.msg`, no copy-link, no in-reader preview of the nested item.**

## Architecture

### 1. Model + store (`mailcore`)

`AttachmentMeta` gains:
- `kind: AttachmentKind` — `enum AttachmentKind { File, Item, Reference }`,
  derived in `from_json` from `@odata.type`:
  `#microsoft.graph.itemAttachment` → `Item`;
  `#microsoft.graph.referenceAttachment` → `Reference`;
  anything else (incl. `#microsoft.graph.fileAttachment` and absent) → `File`.
- `source_url: Option<String>` — from `sourceUrl` (present on
  `referenceAttachment`; `None` otherwise).

Store: two new columns on the `attachments` table (`kind TEXT`,
`source_url TEXT`), added by the same **idempotent `ALTER TABLE … ADD COLUMN`**
migration used for `content_id` (swallow the duplicate-column error).
`put_attachments`/`attachments` persist/read them. `kind` is stored as a short
string (`"file"`/`"item"`/`"reference"`) and mapped back on read.

### 2. Graph — raw `/$value` fetch (`mailcore/src/graph/client.rs`)

```rust
pub fn get_attachment_raw_value(&self, message_id: &str, attachment_id: &str) -> Result<Vec<u8>, GraphError>
```
`GET /me/messages/{id}/attachments/{aid}/$value` — returns the raw response
body bytes (an `itemAttachment`'s MIME), NOT JSON-parsed. Distinct from
`get_attachment_bytes` (which base64-decodes a `fileAttachment`'s
`contentBytes`). Needs a raw-body read on the client's response (the JSON path
`parse_body` is bypassed).

### 3. Sync — item-save command/event (`mailcore/src/sync/engine.rs`)

`SyncCommand::SaveItemAttachment { message_id, attachment_id, dest_base: PathBuf }`
where `dest_base` is the Downloads path WITHOUT an extension (the app can't know
the extension until the bytes are sniffed). The engine:
1. `get_attachment_raw_value(message_id, attachment_id)`.
2. Sniff: bytes start with `BEGIN:VCALENDAR` (after optional leading
   whitespace/BOM) → `.ics`, else `.eml`.
3. Write to `{dest_base}.{ext}`, emit `SyncEvent::AttachmentSaved { path }`
   (the SAME event `SaveAttachment` uses — so `finish_attachment_save` handles
   both).

`referenceAttachment` needs NO command — opening the link is entirely
client-side (`open_with_os_handler(source_url)`).

### 4. App — save branching (`lookxy/src/app.rs`)

`send_save_attachment_command(open_after)` branches on the highlighted
attachment's `kind`:
- **File** → unchanged: `dest = Downloads/sanitize(name)`, register open-intent
  by `dest`, send `SaveAttachment`.
- **Item** → `dest_base = Downloads/sanitize(name_without_ext)`; register
  open-intent by `dest_base`; send `SaveItemAttachment { …, dest_base }`.
- **Reference** → open `source_url` immediately via `open_with_os_handler`; set
  a "Opened link: {name}" notice; close the popup. (No command, no
  open-intent.) If `source_url` is `None`, notice "No link for this
  attachment" and leave the popup open.

**Open-intent by stem.** Because the engine appends the extension, the final
`AttachmentSaved { path }` won't equal the registered `dest_base` for item
saves. `finish_attachment_save` looks up open-intent by BOTH the exact path
(file saves) AND `path.with_extension("")` (item saves) — removing whichever
matched. This preserves the existing concurrent-save disambiguation (each save
still has a unique base) while letting the engine decide the extension.

### 5. Popup labels (`lookxy/src/ui/attachments.rs`)

`line(a)` renders per kind so the user sees the action before pressing a key:
- File → `{name}  ({content_type}, {size} KB)` (unchanged).
- Reference → `🔗 {name}  (link)`.
- Item → `✉ {name}  (item)`.

The popup title stays `Attachments (Enter: save, o: save+open)` — the per-kind
verbs (save vs open) are conveyed by the row label + the notice.

## Data flow

```
Item:   popup Enter/o on an Item row
  → dest_base = Downloads/sanitize(name-sans-ext); pending_saves[dest_base]=open_after
  → SaveItemAttachment{msg, aid, dest_base}
  → engine: get_attachment_raw_value → sniff → write {dest_base}.eml|.ics
  → AttachmentSaved{path}
  → finish_attachment_save: open-intent found via path.with_extension(""), opens if `o`

Reference: popup Enter/o on a Reference row
  → open_with_os_handler(source_url) → notice "Opened link: {name}" → close popup

File:   unchanged (SaveAttachment → contentBytes → Downloads)
```

## Error handling & edge cases

- **`/$value` fetch fails** → `SyncEvent::Error` (same surfacing as a failed
  file save); popup behavior unchanged.
- **Reference with no `source_url`** → "No link for this attachment" notice,
  popup stays open (guarded; shouldn't occur for a real referenceAttachment).
- **Sniff on empty/short bytes** → defaults to `.eml` (the common case).
- **Filename safety** → `sanitize_filename` + Downloads-dir confinement,
  unchanged. `name_without_ext` = strip a trailing `.eml`/`.ics`/other final
  extension from the sanitized name before appending the sniffed one (so a
  nested item named "Invite.ics" doesn't become "Invite.ics.ics").
- **Unknown `@odata.type`** → treated as `File` (best-effort; a genuinely
  unknown kind then hits the existing contentBytes path and, if it has none,
  the existing error notice — no worse than today).

## Testing

**mailcore (unit):**
- `AttachmentMeta::from_json`: `itemAttachment` → `kind: Item`;
  `referenceAttachment` → `kind: Reference` + `source_url: Some(..)`;
  `fileAttachment`/absent → `kind: File`, `source_url: None`.
- Store round-trips `kind`/`source_url`; the migration is idempotent (runs
  twice without error).
- Engine `SaveItemAttachment`: a `/$value` route returning `BEGIN:VCALENDAR…`
  writes `{base}.ics`; one returning RFC822 text writes `{base}.eml`; both emit
  `AttachmentSaved` with the extended path (mirror the existing
  `save_attachment` engine test's harness).

**lookxy (unit):**
- Save branch routing: an `Item` row sends `SaveItemAttachment` (not
  `SaveAttachment`) with an extension-less `dest_base`; a `Reference` row calls
  the OS handler (asserted via the `open_invocations` test seam) and sends NO
  command; a `File` row still sends `SaveAttachment`.
- `finish_attachment_save` opens the item file when the event path is
  `{registered_base}.ics`/`.eml` and open-intent was `o` (stem lookup).
- Popup `line` renders the `🔗`/`✉` prefixes for Reference/Item.

## Scope boundaries (YAGNI)

- **Only `.eml`/`.ics`** for item attachments — no `.msg`, no in-app rendering
  of the nested item.
- **Reference = open only** — no copy-link, no download of the linked file.
- **No change** to `fileAttachment` or inline-image handling.
- **No recursion** into a nested item's own attachments.
