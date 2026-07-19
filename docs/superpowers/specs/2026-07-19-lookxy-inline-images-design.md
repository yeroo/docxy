# lookxy inline image rendering — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Sub-project:** B of the "receive attachments" split (A = non-file attachment
handling, its own later spec). This spec covers **inline image rendering in the
reading pane only**.
**Builds on:** the reading pane (`lookxy/src/ui/reading.rs`,
`mailcore::htmlrender`), the attachment fetch path
(`GraphClient::get_attachment_bytes`, `SyncCommand`/`SyncEvent`), and the
terminal-image stack already proven in `docxy`/`xlsxy` (`ratatui-image` v8 +
`image` v0.25, `Picker` capability detection, the `ImgState`/`draw_images`
crop-on-scroll pattern).

## Goal

Render `<img>` images embedded in an HTML email as real pixels in the reading
pane, in their in-flow position in the body text, with a graceful bordered-box
fallback when the terminal has no graphics capability or the image can't be
decoded. Sources handled: **`cid:` inline attachments** and **`data:` URIs**.
Remote `http(s)` images are **blocked** (tracking-pixel protection) and shown as
a box.

## Background — why this is a sizable feature

Two load-bearing gaps in the reader today:

1. **No scroll.** `reading::draw` renders the body into a `Paragraph` with
   `Wrap { trim: false }` that merely clips to the pane; there is no scroll
   state anywhere. Inline images typically sit below the fold (signatures,
   footers), so without scroll they are unreachable.
2. **Non-deterministic layout.** `Paragraph`'s internal re-wrap means the screen
   row of any given body line isn't known to the caller, and painting an image
   at the correct row needs deterministic layout. `docxy` avoids `Paragraph`
   for exactly this reason and lays out rows itself.

Also, `htmlrender::render_html` currently **drops `<img>`** (it falls through
the transparent-tag path and emits nothing).

So this sub-project must add reader scroll and a deterministic row layout as its
foundation, then the image subsystem on top. Reader scroll is a standalone
improvement (long emails become readable) and is confirmed in scope.

## Product decisions (locked)

- **Sources:** `cid:` (inline attachment) and `data:image/...;base64,...` render;
  everything else (remote `http(s)`, unresolved `cid`, malformed `data:`) shows a
  fallback box with alt text. **Remote images are never fetched.**
- **Reader scroll:** vertical only, keys `j`/`k` (line), PgUp/PgDn (page),
  Home/End (top/bottom), active when the reading pane is focused, clamped to
  content height.
- **Fallback box:** a bordered box sized to the reserved band, captioned with the
  image's alt text (or a generic label) and, when known, its pixel dimensions —
  the same "something is here" affordance `docxy`'s `draw_fallback_box` gives.
- **Animated GIF:** first frame only. **No zoom / open-in-viewer** from the
  reader (the attachment popup's `o` covers saving/opening).

## Architecture — five components (bottom-up)

### 1. mailcore — cid metadata + in-memory inline-byte fetch

- **`AttachmentMeta`** (`mailcore/src/graph/model.rs`) gains
  `content_id: Option<String>`, parsed from Graph's `contentId`
  (`str_field`-style, `None` when absent/empty). `from_json` change only.
- **Store** (`mailcore/src/store`): a `content_id TEXT` column on the
  `attachments` table. Because existing local cache DBs already have the table,
  add it with an **idempotent migration** at open: attempt
  `ALTER TABLE attachments ADD COLUMN content_id TEXT` and swallow the
  "duplicate column name" error (SQLite has no `ADD COLUMN IF NOT EXISTS`).
  `put_attachments`/`attachments` read & write the new column.
- **New fetch path:** `SyncCommand::FetchInlineImage { message_id,
  attachment_id }` → `GraphClient::get_attachment_bytes` (already decodes a
  `fileAttachment`'s `contentBytes`) → `SyncEvent::InlineImageReady {
  message_id, content_id, bytes }`. This delivers bytes **into memory** for
  rendering — distinct from the existing `SaveAttachment` → disk path. Inline
  images are `fileAttachment`s with `isInline: true` and a `contentId`, so the
  existing bytes endpoint suffices; no new Graph endpoint.

### 2. mailcore htmlrender — `<img>` markers

`render_html` emits, for an `<img>`, a dedicated marker rather than nothing.
Additive change to the neutral representation:

- `StyledLine` gains `image: Option<ImageRef>` (default `None`); a marker is
  **one** `StyledLine` with empty `spans` and `image: Some(..)`. Consistent with
  htmlrender's existing philosophy (it emits `indent` as a count and leaves the
  space-painting to the consumer, refusing to hardcode a column figure), the
  **reserved band height is a reader-side concern, not htmlrender's**: htmlrender
  emits one marker line per image; the reader expands it into a fixed band of
  `IMAGE_BOX_ROWS` rows (a lookxy constant, clamped — real pixel size isn't known
  until decode). htmlrender stays display-agnostic.
- `ImageRef { src: ImageSource, alt: String }`, where
  `ImageSource::{ Cid(String), Data { mime, bytes: Vec<u8> }, Remote(String), Unsupported }`.
  `src="cid:X"` → `Cid("X")`; `src="data:image/png;base64,.."` → decode base64
  once here → `Data`; `src="http(s)://.."` → `Remote` (never fetched);
  anything else → `Unsupported`. `alt` from the `alt` attribute.
- Text rendering is untouched: a consumer that ignores `image` still sees a
  blank band. `render_text` is unchanged (plain-text bodies have no images).

### 3. lookxy reader — scroll + deterministic layout

`reading::draw` stops using `Paragraph`'s auto-wrap for the body and lays out an
explicit row list (htmlrender already wraps to the inner width, so one
`StyledLine` = one row; a marker line reserves its band's rows):

- `App` gains `reading_scroll: usize` (top row offset) and the viewport height
  captured each draw for clamping. Keys (reading pane focused): `j`/`k`,
  PgUp/PgDn, Home/End; clamp to `content_rows.saturating_sub(viewport)`.
  Opening a different message resets scroll to 0.
- Header lines stay; the body area scrolls. Text rows are drawn manually (or via
  a `Paragraph` fed the already-sliced visible rows with **no** re-wrap) so the
  row index of every marker band is known and stable.

### 4. lookxy image subsystem (reuse docxy/xlsxy stack)

- Add deps `ratatui-image = { version = "8", default-features = false,
  features = ["crossterm"] }` and `image = { version = "0.25",
  default-features = false, features = ["png","jpeg","gif","bmp","tiff"] }` to
  `lookxy/Cargo.toml`.
- **Capability detection once at startup** (`main.rs`): `Picker::from_query_stdio()`
  with `Picker::from_fontsize((8,16))` half-block fallback — copied from
  `docxy`/`xlsxy`. Stored on `App` as `Option<Picker>` (None only if even the
  fallback can't be built; then everything is a box).
- **Per-message image cache** keyed by the resolved source id (`cid` or a
  data-URI hash), value = decoded+scaled `Protocol` (the `ImgState` pattern:
  scale to the box once, re-encode per visible window while scrolling). Cleared
  when the opened message changes.
- The draw entry point changes from `ui::draw(f, &App)` to `&mut App` so the
  cache can be updated during paint (matches docxy's `&mut self` draw). `main.rs`
  already owns `app` mutably at `terminal.draw`.

### 5. Painting

A `draw_images`-style overlay pass (after the body text is drawn): for each
marker band, compute its on-screen rect from `row − reading_scroll`, clip to the
body viewport (crop at top/bottom edges like docxy), and:

- `Cid(x)`: if bytes are cached → decode/scale/paint; else fire
  `FetchInlineImage` (once) and paint a "loading" box this frame.
- `Data{..}`: decode immediately (bytes already in hand) → paint.
- `Remote(_)` / `Unsupported` / unresolved `Cid` / decode failure / no `Picker`:
  paint the bordered fallback box with alt text (+ dimensions when known).

## Data flow

```
open message (html body with <img src="cid:logo">)
  → render_html reserves a band + records ImageRef{ Cid("logo"), alt }
  → reader lays out rows; the cid isn't cached yet
  → SyncCommand::FetchInlineImage{ message_id, attachment_id for contentId "logo" }
  → engine get_attachment_bytes → SyncEvent::InlineImageReady{ content_id:"logo", bytes }
  → App caches bytes; next draw decodes/scales into the band, painting real pixels,
    moving/cropping with reading_scroll
data: URI  → decoded in render_html → painted on first draw (no fetch)
remote/http → box, never fetched
```

Resolving a `cid` to an `attachment_id`: the message's attachment metadata
(already fetched on demand for the popup, now carrying `content_id`) maps
`content_id == "logo"` → that attachment's `id` for the `FetchInlineImage`
command. If the metadata isn't loaded yet, the reader triggers the existing
`FetchAttachments` first (same as the popup), then resolves.

## Error handling & edge cases

- **No graphics capability** → every image is a fallback box.
- **Decode failure / truncated bytes** → box captioned "couldn't render".
- **Unresolved `cid`** (no attachment with that `contentId`) → box with alt text.
- **Remote `http(s)`** → box; never fetched (tracking-pixel protection).
- **Malformed `data:`** (bad base64 / unknown mime) → box.
- **Oversized image** → clamped to a max box (cols × rows) before scaling.
- **Never panic, never block the UI thread:** base64 decode of a `data:` URI is
  bounded and done in `htmlrender`; `cid` bytes arrive via the async
  command/event path; image decode/scale happens on the draw thread but is
  guarded and falls back to a box on any error (same risk profile as docxy).
- **Scroll clamp:** empty/short body → scroll pinned at 0; `reading_scroll` never
  exceeds `content_rows − viewport`.

## Testing

**mailcore (unit, no ratatui):**
- `AttachmentMeta::from_json` parses `contentId` → `content_id`; absent → `None`.
- Store round-trips `content_id` (put → read); the idempotent migration runs
  twice without error and preserves rows.
- `render_html`: `<img src="cid:x" alt="a">` → a marker line with
  `ImageRef{ Cid("x"), alt:"a" }` and the reserved height; `data:image/png;base64,..`
  → `Data{ mime, bytes }` with correctly decoded bytes; `http(s)://..` → `Remote`
  (and NO bytes fetched); malformed `data:` → `Unsupported`/box. Text around the
  image still renders (marker is additive).

**lookxy (unit):**
- Reading-pane scroll: `j`/`k`/PgDn/Home/End move `reading_scroll` and clamp to
  content height; opening a new message resets it to 0.
- cid→attachment_id resolution picks the attachment whose `content_id` matches;
  a missing metadata set triggers `FetchAttachments` first.
- The fallback-box path is chosen when `Picker` is absent or bytes fail to
  decode (capability treated as absent in tests).

**Manual/interactive (not unit-tested, like docxy):** actual pixel emission in a
kitty/iTerm2/Sixel terminal, and crop-on-scroll smoothness.

## Scope boundaries (YAGNI)

- **`cid:` and `data:` only** — no remote `http(s)` fetch (privacy).
- **No image zoom / open-in-viewer** from the reader (attachment popup `o` covers
  save+open).
- **First GIF frame only** — no animation.
- **Vertical scroll only** — no horizontal scroll in the reader.
- **No HTML layout beyond what `htmlrender` already does** — images flow at their
  marker position in the existing line stream; no float/table-cell image
  positioning, no width/height honoring beyond a clamped default box.
- **Non-file attachment handling (item/reference)** is sub-project A — out of
  scope here.
