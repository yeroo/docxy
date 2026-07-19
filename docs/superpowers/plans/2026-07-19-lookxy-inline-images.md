# lookxy Inline Image Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render `<img>` images embedded in an HTML email (`cid:` inline attachments and `data:` URIs) as real pixels in the reading pane, in their in-flow position, with a bordered-box fallback where graphics aren't available; remote `http(s)` images are blocked.

**Architecture:** Bottom-up in five layers — (1) mailcore carries `content_id` on attachment metadata and a new in-memory inline-byte fetch command/event; (2) `htmlrender` emits an image *marker* line per `<img>` instead of dropping it; (3) the reader gains vertical scroll + a deterministic row layout that reserves a fixed band per image and draws a fallback box; (4) lookxy resolves each `cid` to an attachment and fetches its bytes into memory; (5) a paint pass overlays decoded, scaled images over the reserved bands using the `ratatui-image` stack already proven in `docxy`/`xlsxy`, cropping at the scroll viewport.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), ratatui 0.29, `ratatui-image` 8, `image` 0.25. Hand-rolled `mailcore::json` and `htmlrender` (no serde/scraper).

## Global Constraints

- **Build/test ONLY through the wrapper** (bare `cargo` fails with os error 448). Every command is `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **MSRV 1.88, edition 2024.** clippy `-D warnings` clean on ubuntu/macos/windows. Run `bash "$LCARGO" fmt` before every commit.
- **Sources handled:** `cid:` (inline attachment) and `data:image/…;base64,…` render; **remote `http(s)` images are NEVER fetched** (tracking-pixel protection) — they, unresolved `cid`s, and malformed `data:` show a fallback box.
- **New deps** (Task 6 only): `ratatui-image = { version = "8", default-features = false, features = ["crossterm"] }` and `image = { version = "0.25", default-features = false, features = ["png","jpeg","gif","bmp","tiff"] }` — the exact lines `docxy`/`xlsxy` use. No other new deps.
- **Reuse the workspace's proven graphics API — do not invent it.** The `ratatui-image` v8 calls are used verbatim in `docxy/src/main.rs`: picker init at `docxy/src/main.rs:6376` (`Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)))`); encode at `docxy/src/main.rs:2961-2974` (`picker.new_protocol(cropped, size, Resize::Fit(None)).ok()` → `Option<Protocol>`); render at `docxy/src/main.rs:3056` (`f.render_widget(Image::new(&st.proto), rect)`); the crop-on-scroll `draw_images` shape at `docxy/src/main.rs:3017-3065`. Mirror those call shapes.
- **Never panic or block the UI thread on image work:** `data:` base64 decode is bounded (in `htmlrender`); `cid` bytes arrive via the async command/event path; decode/scale on the draw thread is guarded and falls back to a box on any error.
- **Additive struct changes:** `AttachmentMeta` and `StyledLine` each gain a field. Every `AttachmentMeta { … }` / `StyledLine { … }` literal in the workspace (tests included) must be updated to compile — treat "workspace compiles" as part of the task's done bar.

## Reserved constants

- `IMAGE_BOX_ROWS: usize = 10` — the fixed reader-side band height reserved per inline image (defined in `lookxy/src/ui/reading.rs`, Task 4). htmlrender stays display-agnostic (emits one marker line; the reader expands it into the band).

---

### Task 1: mailcore — `content_id` on attachment metadata + store column + migration

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`AttachmentMeta` + `from_json`)
- Modify: `mailcore/src/store/schema.rs` (attachments table)
- Modify: `mailcore/src/store/mod.rs` (idempotent migration, `put_attachments`, `attachments`)
- Modify: every `AttachmentMeta { … }` literal (tests in `lookxy/src/app.rs`, `lookxy/src/ui/attachments.rs`, `mailcore` tests) to add `content_id`

**Interfaces:**
- Produces: `AttachmentMeta.content_id: Option<String>`; store persists/reads it; `AttachmentMeta::from_json` parses Graph `contentId`.

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/graph/model.rs` tests:
```rust
    #[test]
    fn attachment_meta_parses_content_id() {
        let v = crate::json::parse(
            r#"{"id":"a1","name":"logo.png","contentType":"image/png","size":10,"isInline":true,"contentId":"logo123"}"#
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.content_id.as_deref(), Some("logo123"));
        assert!(a.is_inline);
    }
    #[test]
    fn attachment_meta_content_id_absent_is_none() {
        let v = crate::json::parse(
            r#"{"id":"a1","name":"x.txt","contentType":"text/plain","size":1,"isInline":false}"#
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.content_id, None);
    }
```
Add to `mailcore/src/store/mod.rs` tests (adapt to the file's existing test-store helper — find how other store tests open an in-memory `Store`, e.g. `Store::open_in_memory()` or similar, and match it):
```rust
    #[test]
    fn attachments_round_trip_content_id() {
        let store = test_store();
        store.upsert_message("inbox", &sample_message("m1")).unwrap(); // reuse existing test helpers
        store.put_attachments("m1", &[AttachmentMeta {
            id: "a1".into(), name: "logo.png".into(), content_type: "image/png".into(),
            size: 10, is_inline: true, content_id: Some("logo123".into()),
        }]).unwrap();
        let got = store.attachments("m1").unwrap();
        assert_eq!(got[0].content_id.as_deref(), Some("logo123"));
    }
```
If `mailcore/src/store/mod.rs` tests lack a `test_store`/`sample_message` helper, use whatever the existing attachment/body tests in that file use to construct a store and a message (read the test module first and mirror it exactly).

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore content_id`
Expected: FAIL — `AttachmentMeta` has no field `content_id`.

- [ ] **Step 3: Implement**

In `mailcore/src/graph/model.rs`, add the field and parse it:
```rust
pub struct AttachmentMeta {
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub is_inline: bool,
    /// The `Content-ID` of an inline attachment (Graph `contentId`), used to
    /// resolve `<img src="cid:…">` in the body to this attachment. `None` for
    /// ordinary (non-inline) attachments.
    pub content_id: Option<String>,
}
```
In `from_json`, after `is_inline`:
```rust
            content_id: {
                let cid = str_field(v, "contentId");
                if cid.is_empty() { None } else { Some(cid) }
            },
```

In `mailcore/src/store/schema.rs`, add the column to the `attachments` CREATE TABLE (line 64-72) so fresh DBs have it:
```sql
CREATE TABLE IF NOT EXISTS attachments (
    id           TEXT NOT NULL,
    message_id   TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    name         TEXT NOT NULL DEFAULT '',
    content_type TEXT NOT NULL DEFAULT '',
    size         INTEGER NOT NULL DEFAULT 0,
    is_inline    INTEGER NOT NULL DEFAULT 0,
    content_id   TEXT,
    PRIMARY KEY (message_id, id)
);
```

In `mailcore/src/store/mod.rs`, where the schema is applied at open (find where `schema::…` / the `CREATE TABLE` SQL is executed — the `Store::open`/`new` path), run an **idempotent migration** for pre-existing DBs right after the schema is created:
```rust
        // Additive migration for DBs created before `attachments.content_id`
        // existed. SQLite has no `ADD COLUMN IF NOT EXISTS`, so attempt the
        // ALTER and swallow the "duplicate column name" error it raises when
        // the column is already present (fresh DBs get it from schema.rs).
        let _ = conn.execute("ALTER TABLE attachments ADD COLUMN content_id TEXT", []);
```
(`execute` returning `Err` for a duplicate column is discarded by `let _ =`; a fresh DB where the column already exists from `schema.rs` also lands here harmlessly.)

Update `put_attachments` INSERT (add `content_id` column + `?7` param binding `a.content_id`) and `attachments` SELECT (`SELECT id, name, content_type, size, is_inline, content_id …`, and read `content_id: row.get(5)?`).

Update every `AttachmentMeta { … }` literal across the workspace to add `content_id: None` (or a value in the new tests). Find them: `bash "$LCARGO" build -p lookxy 2>&1 | grep "missing field"` after the model change will list them; they are in `lookxy/src/app.rs` (several test fixtures), `lookxy/src/ui/attachments.rs` tests, and any mailcore tests.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore content_id` then `bash "$LCARGO" build` (whole workspace compiles with the new field).
Expected: PASS; workspace builds.

- [ ] **Step 5: Migration-idempotency test + fmt + clippy + commit**

Add a test that opening a store twice over the same path (or applying the migration twice) doesn't error and preserves rows — mirror how existing store tests exercise open. If the test store is in-memory only, instead assert the raw ALTER twice is harmless:
```rust
    #[test]
    fn content_id_migration_is_idempotent() {
        let store = test_store();
        // The column exists from schema.rs; re-applying the ALTER must not error the caller.
        // (Store::open already ran it once; run the same statement again directly.)
        let _ = store.raw_conn_for_test().execute("ALTER TABLE attachments ADD COLUMN content_id TEXT", []);
        // A second real use still works:
        store.upsert_message("inbox", &sample_message("m2")).unwrap();
        store.put_attachments("m2", &[/* one AttachmentMeta with content_id: None */]).unwrap();
        assert!(store.attachments("m2").is_ok());
    }
```
If there's no test accessor for the raw connection, drop this direct-ALTER assertion and instead rely on the round-trip test plus a comment; do NOT add a public raw-connection accessor just for the test. Then:
```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "mailcore: content_id on AttachmentMeta + store column + idempotent migration"
```

---

### Task 2: mailcore — in-memory inline-image fetch command/event

**Files:**
- Modify: `mailcore/src/sync/engine.rs` (`SyncCommand`, `SyncEvent`, dispatch arm, handler)

**Interfaces:**
- Consumes: `GraphClient::get_attachment_bytes(message_id, attachment_id) -> Result<Vec<u8>, GraphError>` (exists).
- Produces: `SyncCommand::FetchInlineImage { message_id: String, attachment_id: String, content_id: String }`; `SyncEvent::InlineImageReady { message_id: String, content_id: String, bytes: Vec<u8> }`.

- [ ] **Step 1: Write the failing test**

In `mailcore/src/sync/engine.rs` tests, mirror the existing `save_attachment_writes_bytes_and_emits_saved_path` test (it sets up a `testserver` route returning `contentBytes:"aGVsbG8="` for `GET /me/messages/M1/attachments/A1` and drives the engine). Write:
```rust
    #[test]
    fn fetch_inline_image_emits_bytes_with_content_id() {
        // Same route fixture as save_attachment's test: GET the attachment returns base64 "hello".
        let (engine, cmd_tx, evt_rx) = /* build engine over a testserver with the A1 route — copy save_attachment test's setup */;
        cmd_tx.send(SyncCommand::FetchInlineImage {
            message_id: "M1".into(), attachment_id: "A1".into(), content_id: "logo".into(),
        }).unwrap();
        // drive the engine one step the same way the sibling test does, then:
        let evt = /* recv until InlineImageReady */;
        match evt {
            SyncEvent::InlineImageReady { message_id, content_id, bytes } => {
                assert_eq!(message_id, "M1");
                assert_eq!(content_id, "logo");
                assert_eq!(bytes, b"hello");
            }
            other => panic!("expected InlineImageReady, got {other:?}"),
        }
    }
```
Read `save_attachment_writes_bytes_and_emits_saved_path` first and copy its engine/testserver construction verbatim (it already has the exact fixture and drive pattern).

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore fetch_inline_image`
Expected: FAIL — no `FetchInlineImage` variant.

- [ ] **Step 3: Implement**

Add to `SyncCommand` (near `SaveAttachment`):
```rust
    /// Fetch an inline image's bytes (`GraphClient::get_attachment_bytes`)
    /// into memory for rendering in the reading pane — distinct from
    /// `SaveAttachment`, which writes to disk. `content_id` is echoed back on
    /// [`SyncEvent::InlineImageReady`] so the UI can key the bytes by the
    /// `cid:` the body references.
    FetchInlineImage {
        message_id: String,
        attachment_id: String,
        content_id: String,
    },
```
Add to `SyncEvent` (near `AttachmentSaved`):
```rust
    /// An inline image's bytes were fetched (from
    /// [`SyncCommand::FetchInlineImage`]); the UI caches them by `content_id`.
    InlineImageReady {
        message_id: String,
        content_id: String,
        bytes: Vec<u8>,
    },
```
Add the dispatch arm (next to `SyncCommand::SaveAttachment => …`):
```rust
            SyncCommand::FetchInlineImage { message_id, attachment_id, content_id } =>
                self.fetch_inline_image(&message_id, &attachment_id, &content_id),
```
Add the handler (model it on `fetch_attachments`/`save_attachment`):
```rust
    fn fetch_inline_image(&mut self, message_id: &str, attachment_id: &str, content_id: &str) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_attachment_bytes(message_id, attachment_id)) {
            Ok(bytes) => self.emit(SyncEvent::InlineImageReady {
                message_id: message_id.to_string(),
                content_id: content_id.to_string(),
                bytes,
            }),
            Err(e) => self.react(e),
        }
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore fetch_inline_image` then `bash "$LCARGO" build`
Expected: PASS; builds. (The `lookxy` side will handle the new event in Task 5 — until then `App::on_sync_event` may need a `SyncEvent::InlineImageReady { .. } => {}` no-op arm if the match is exhaustive; add it if `build` complains, it's replaced in Task 5.)

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "mailcore: FetchInlineImage command + InlineImageReady event (in-memory inline bytes)"
```

---

### Task 3: mailcore htmlrender — `<img>` markers + image-ref extractor

**Files:**
- Modify: `mailcore/src/htmlrender.rs` (`StyledLine`, new `ImageRef`/`ImageSource`, `render_html` `<img>` handling, new `image_refs`)
- Modify: `mailcore/src/graph/client.rs` (make `base64_decode` reachable) and any `StyledLine { … }` literal (in `htmlrender`/`compose_html`) to add `image: None`

**Interfaces:**
- Consumes: `crate::graph::client::base64_decode` (make `pub(crate)`).
- Produces:
  - `StyledLine.image: Option<ImageRef>` (default `None`).
  - `pub enum ImageSource { Cid(String), Data { mime: String, bytes: Vec<u8> }, Remote(String), Unsupported }`
  - `pub struct ImageRef { pub src: ImageSource, pub alt: String }`
  - `pub fn image_refs(html: &str) -> Vec<ImageRef>` — every `<img>`'s ref, in document order, width-independent (for the reader's fetch-triggering).

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/htmlrender.rs` tests:
```rust
    #[test]
    fn img_cid_becomes_an_image_marker_line() {
        let lines = render_html(r#"<p>before</p><img src="cid:logo123" alt="Logo"><p>after</p>"#, 80);
        let marker = lines.iter().find(|l| l.image.is_some()).expect("an image marker line");
        match &marker.image.as_ref().unwrap().src {
            ImageSource::Cid(c) => assert_eq!(c, "logo123"),
            other => panic!("expected Cid, got {other:?}"),
        }
        assert_eq!(marker.image.as_ref().unwrap().alt, "Logo");
        // surrounding text still renders
        let joined: String = lines.iter().flat_map(|l| l.spans.iter()).map(|s| s.text.clone()).collect();
        assert!(joined.contains("before") && joined.contains("after"));
    }
    #[test]
    fn img_data_uri_decodes_bytes() {
        // "R0lGOD" ... use a tiny valid base64: "aGk=" decodes to "hi"
        let lines = render_html(r#"<img src="data:image/png;base64,aGk=">"#, 80);
        let m = lines.iter().find_map(|l| l.image.as_ref()).unwrap();
        match &m.src {
            ImageSource::Data { mime, bytes } => { assert_eq!(mime, "image/png"); assert_eq!(bytes, b"hi"); }
            other => panic!("expected Data, got {other:?}"),
        }
    }
    #[test]
    fn img_remote_is_marked_remote_and_not_fetched() {
        let lines = render_html(r#"<img src="https://tracker.example/x.png">"#, 80);
        let m = lines.iter().find_map(|l| l.image.as_ref()).unwrap();
        assert!(matches!(m.src, ImageSource::Remote(_)));
    }
    #[test]
    fn img_malformed_data_is_unsupported() {
        let lines = render_html(r#"<img src="data:whoops">"#, 80);
        let m = lines.iter().find_map(|l| l.image.as_ref()).unwrap();
        assert!(matches!(m.src, ImageSource::Unsupported));
    }
    #[test]
    fn image_refs_extracts_all_in_order() {
        let refs = image_refs(r#"<img src="cid:a"><p>x</p><img src="https://y">"#);
        assert_eq!(refs.len(), 2);
        assert!(matches!(refs[0].src, ImageSource::Cid(ref c) if c == "a"));
        assert!(matches!(refs[1].src, ImageSource::Remote(_)));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore img_ image_refs`
Expected: FAIL — `image` field / `ImageSource` / `image_refs` don't exist.

- [ ] **Step 3: Implement**

Make the base64 decoder reachable — in `mailcore/src/graph/client.rs:856` change `fn base64_decode` to `pub(crate) fn base64_decode`.

In `mailcore/src/htmlrender.rs`, add the types (near `StyledLine`):
```rust
/// The source of an `<img>` in a rendered body.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageSource {
    /// `src="cid:X"` — an inline attachment, resolved by `Content-ID`.
    Cid(String),
    /// `src="data:<mime>;base64,<b64>"` — bytes already decoded here.
    Data { mime: String, bytes: Vec<u8> },
    /// `src="http(s)://…"` — a remote image, deliberately NOT fetched
    /// (tracking-pixel protection); the consumer shows a box.
    Remote(String),
    /// Any other/malformed `src` — the consumer shows a box.
    Unsupported,
}

/// One `<img>` from a body: its source plus the `alt` text (for the fallback
/// box caption).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub src: ImageSource,
    pub alt: String,
}

/// Classifies an `<img src>` value into an [`ImageSource`]. `data:` URIs are
/// base64-decoded here (bounded work); `cid:` keeps the bare id; `http(s)`
/// is marked remote and never fetched.
fn classify_img_src(src: &str) -> ImageSource {
    let s = src.trim();
    if let Some(cid) = s.strip_prefix("cid:").or_else(|| s.strip_prefix("CID:")) {
        return ImageSource::Cid(cid.to_string());
    }
    if let Some(rest) = s.strip_prefix("data:") {
        // rest = "<mime>;base64,<b64>"
        if let Some((meta, b64)) = rest.split_once(',') {
            let is_b64 = meta.rsplit(';').any(|p| p.eq_ignore_ascii_case("base64"));
            let mime = meta.split(';').next().unwrap_or("").to_string();
            if is_b64 && !mime.is_empty() {
                if let Some(bytes) = crate::graph::client::base64_decode(b64.trim()) {
                    return ImageSource::Data { mime, bytes };
                }
            }
        }
        return ImageSource::Unsupported;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return ImageSource::Remote(s.to_string());
    }
    ImageSource::Unsupported
}
```
Add `pub image: Option<ImageRef>` to `StyledLine` (keep `#[derive(… Default)]` — `Option` defaults to `None`, so `StyledLine::default()` still works). Any `StyledLine { spans, indent }` literal that does NOT use `..Default::default()` (there are internal ones in `render_html`/`render_text`/the footnote appendix) must add `image: None`. Prefer switching those to `..Default::default()` where clean.

In `render_html`, handle `<img>` in the `TagOpen` match (add a case before `_ => {}`), flushing the current text block first so the image sits between paragraphs:
```rust
                    "img" => {
                        flush(&mut lines, &mut words, indent, width);
                        let src = attrs.iter().find(|(k, _)| k == "src")
                            .map(|(_, v)| v.as_str()).unwrap_or("");
                        let alt = attrs.iter().find(|(k, _)| k == "alt")
                            .map(|(_, v)| v.clone()).unwrap_or_default();
                        lines.push(StyledLine {
                            spans: Vec::new(),
                            indent,
                            image: Some(ImageRef { src: classify_img_src(src), alt }),
                        });
                    }
```
Add the extractor (walks the same tokenizer, no wrapping/state):
```rust
/// Every `<img>` in `html` as an [`ImageRef`], in document order — used by the
/// reader to trigger inline-image fetches without caring about wrap width.
pub fn image_refs(html: &str) -> Vec<ImageRef> {
    let mut tok = Tokenizer::new(html);
    let mut out = Vec::new();
    loop {
        match tok.next() {
            Token::Eof => break,
            Token::TagOpen { name, attrs, .. } if name == "img" => {
                let src = attrs.iter().find(|(k, _)| k == "src").map(|(_, v)| v.as_str()).unwrap_or("");
                let alt = attrs.iter().find(|(k, _)| k == "alt").map(|(_, v)| v.clone()).unwrap_or_default();
                out.push(ImageRef { src: classify_img_src(src), alt });
            }
            _ => {}
        }
    }
    out
}
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore img_ image_refs` then `bash "$LCARGO" test -p mailcore` (existing htmlrender tests still pass — the `image` field is additive; `StyledLine`-equality tests now compare `image: None` on both sides).
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "mailcore htmlrender: <img> markers (cid/data/remote) + image_refs extractor"
```

---

### Task 4: lookxy reader — scroll + deterministic layout + fallback image boxes

**Files:**
- Modify: `lookxy/src/app.rs` (add `reading_scroll`/`reading_viewport`/`reading_content_rows`, reset on open; scroll methods)
- Modify: `lookxy/src/ui/reading.rs` (fixed header + scrolling body with explicit rows; reserve `IMAGE_BOX_ROWS` per image and draw a fallback box; `draw` takes `&mut App`)
- Modify: `lookxy/src/ui/mod.rs` (`ui::draw` takes `&mut App`, passes `&*app` to the immutable panes and `app` to `reading::draw`; route scroll keys to the reading pane)
- Modify: `lookxy/src/main.rs` (the `terminal.draw(|f| ui::draw(f, app))` call already holds `app: &mut App`, so no change beyond the type flowing through)

**Note — do the `&mut App` draw-signature flip HERE (not later).** `reading::draw` must record the live viewport height and rendered content-row count each frame so scroll can clamp, which needs `&mut App`. So `ui::draw`/`reading::draw` become `&mut App` in this task; Task 6 (image painting) then needs no further signature change. The immutable panes keep `&App` — `ui::draw` reborrows `&*app` for them.

**Interfaces:**
- Consumes: `htmlrender::render_html` (now returns lines that may carry `image: Some(..)`), `ImageRef`/`ImageSource`.
- Produces: `App.reading_scroll: usize`; `App::reading_scroll_by(delta: isize)`, `App::reading_scroll_page(delta: isize)`, `App::reading_scroll_home()/reading_scroll_end()`; `reading::IMAGE_BOX_ROWS`.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/app.rs` tests:
```rust
    #[test]
    fn reading_scroll_clamps_and_resets_on_open() {
        let mut app = App::for_test_with_seeded_store();
        app.reading_scroll = 0;
        app.reading_viewport = 5;        // 5 visible body rows
        app.reading_content_rows = 20;   // 20 total rows
        app.reading_scroll_by(100);         // way past the end
        assert_eq!(app.reading_scroll, 15); // clamped to content(20) - viewport(5)
        app.reading_scroll_by(-100);
        assert_eq!(app.reading_scroll, 0);
        // opening a message resets scroll
        app.reading_scroll = 7;
        app.open_message("m1");
        assert_eq!(app.reading_scroll, 0);
    }
```
(Use whatever seeded message id `for_test_with_seeded_store` provides; if `open_message("m1")` needs a real row, seed it the way other app tests do. `reading_viewport`/`reading_content_rows` are plain `pub usize` fields set directly here — match the crate's existing test style, which sets `pub` fields directly.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy reading_scroll`
Expected: FAIL — no `reading_scroll` field / scroll methods.

- [ ] **Step 3: Implement scroll state + keys**

In `App`: add `pub reading_scroll: usize`, plus (for clamping) `pub reading_viewport: usize` and `pub reading_content_rows: usize` (plain fields, updated each draw by `reading::draw` via its `&mut App`). Initialize all to `0` in the constructor(s). In `open_message` (app.rs:623) set `self.reading_scroll = 0;` after recording `selected_msg`.

Add methods:
```rust
    fn reading_max_scroll(&self) -> usize {
        self.reading_content_rows.saturating_sub(self.reading_viewport)
    }
    pub fn reading_scroll_by(&mut self, delta: isize) {
        let max = self.reading_max_scroll() as isize;
        self.reading_scroll = (self.reading_scroll as isize + delta).clamp(0, max) as usize;
    }
    pub fn reading_scroll_page(&mut self, dir: isize) {
        let page = self.reading_viewport.max(1) as isize;
        self.reading_scroll_by(dir * page);
    }
    pub fn reading_scroll_home(&mut self) { self.reading_scroll = 0; }
    pub fn reading_scroll_end(&mut self) { self.reading_scroll = self.reading_max_scroll(); }
```
(If tests set the fields directly, `set_reading_viewport`/`set_reading_content_rows` aren't needed — use `pub` fields. Keep whichever the test in Step 1 used; make them consistent.)

Route keys in `lookxy/src/ui/mod.rs`. In the mail-mode key handler (around line 182-193), the `Up/k` and `Down/j` currently call `move_selection`, which is a no-op for `Pane::Reading` (`ui/mod.rs:283`). Change so that when `app.focus == Pane::Reading`, vertical keys scroll the reader; otherwise keep moving the selection. Concretely, before the existing `KeyCode::Up | Char('k') => move_selection(app, -1)` arms, add a guard, or make `move_selection`'s `Pane::Reading` arm scroll. Cleanest: in the mail-mode branch, add reading-focused handling:
```rust
        KeyCode::Char('k') | KeyCode::Up if app.focus == Pane::Reading => app.reading_scroll_by(-1),
        KeyCode::Char('j') | KeyCode::Down if app.focus == Pane::Reading => app.reading_scroll_by(1),
        KeyCode::PageUp if app.focus == Pane::Reading => app.reading_scroll_page(-1),
        KeyCode::PageDown if app.focus == Pane::Reading => app.reading_scroll_page(1),
        KeyCode::Home if app.focus == Pane::Reading => app.reading_scroll_home(),
        KeyCode::End if app.focus == Pane::Reading => app.reading_scroll_end(),
```
placed BEFORE the generic `KeyCode::Up | Char('k') => move_selection(...)` arms so the reading-focused guard wins. (Match arms with `if` guards must precede the unguarded ones for the same key.)

- [ ] **Step 4: Deterministic layout + fallback boxes in `reading::draw`**

Rewrite `reading::draw` (`lookxy/src/ui/reading.rs`) to: (a) split the pane into a fixed header sub-area and a scrolling body sub-area; (b) build an explicit `Vec` of body rows where a `StyledLine` with `image: Some` expands into `IMAGE_BOX_ROWS` reserved rows; (c) record `reading_content_rows`/`reading_viewport` for clamping; (d) render only the visible window from `reading_scroll`; (e) draw a bordered fallback box over each image band that is visible.

Add the constant and an owned layout model at the top of `reading.rs`. This mirrors docxy's actual model — a flat OWNED line list (each image band = `IMAGE_BOX_ROWS` blank lines) plus a separate list of image boxes carrying each band's absolute top row. Owning everything means the layout borrows nothing from `app`, so `app.reading_*` can be assigned right after; and an absolute-row image box can be cropped even when its top row is scrolled above the viewport:
```rust
/// Rows reserved in the reader for one inline image band.
pub const IMAGE_BOX_ROWS: usize = 10;

/// One inline image's placement: the absolute body-row of its band's first row,
/// plus the ref (owned — the layout borrows nothing from `app`).
struct ImgBox {
    row: usize,
    img: ImageRef,
}
```
`render_html` returns an OWNED `Vec<StyledLine>` (it borrows nothing from `app`), so a `body_layout(lines) -> (Vec<Line<'static>>, Vec<ImgBox>)` can consume it: for a text line push `to_ratatui_line`; for `image: Some(r)` record `ImgBox { row: out_lines.len(), img: r.clone() }` then push `IMAGE_BOX_ROWS` blank `Line::from("")`s.

Flip the draw signatures to `&mut App` in this task (see the task's Note). In `lookxy/src/ui/mod.rs`, change `pub fn draw(f: &mut Frame, app: &App)` → `pub fn draw(f: &mut Frame, app: &mut App)`; pass `&*app` (immutable reborrow) to `folders::draw`, `message_list::draw`, `status_bar`, `calendar`, and the popups, and pass `app` (mutable) only to `reading::draw`. The `main.rs:200` call `terminal.draw(|f| ui::draw(f, app))` already holds `app: &mut App`, so it type-checks unchanged.

Layout/paint in `draw`:
```rust
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Reading;
    let block = Block::default().title("Reading Pane").borders(Borders::ALL).border_style(border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(m) = selected_message(app) else {
        f.render_widget(Paragraph::new("(no message selected — press Enter on a message)"), inner);
        return;
    };

    // Fixed header (From/Subject/Received + blank), then the scrolling body.
    let header = header_lines(m);
    let header_h = (header.len() as u16 + 1).min(inner.height);
    let header_area = Rect { height: header_h, ..inner };
    let body_area = Rect { y: inner.y + header_h, height: inner.height.saturating_sub(header_h), ..inner };
    f.render_widget(Paragraph::new(header), header_area);

    // Build the owned layout (render_body_lines returns Vec<StyledLine>, owned).
    let styled = render_body(app, body_area.width as usize); // Vec<StyledLine> (see body_lines' current match on html vs text)
    let (lines, images) = body_layout(styled);
    let vh = body_area.height as usize;
    app.reading_content_rows = lines.len();
    app.reading_viewport = vh;
    let scroll = app.reading_scroll.min(lines.len().saturating_sub(vh));

    // Text: render the visible window as one Paragraph, no re-wrap (lines already
    // fit width; blank lines hold the image bands' space). trim:false keeps columns.
    let visible: Vec<Line<'static>> = lines.iter().skip(scroll).take(vh).cloned().collect();
    f.render_widget(Paragraph::new(visible), body_area);

    // Images: crop each band to the visible window (docxy's draw_images math,
    // main.rs:3017-3065). In THIS task every box is the fallback; Task 6 paints
    // pixels first and only falls back here.
    for ib in &images {
        let wtop = scroll.saturating_sub(ib.row);
        let wbot = (scroll + vh).saturating_sub(ib.row).min(IMAGE_BOX_ROWS);
        if wbot <= wtop { continue; }
        let y = body_area.y + (ib.row + wtop - scroll) as u16;
        let rect = Rect { x: body_area.x, y, width: body_area.width, height: (wbot - wtop) as u16 };
        draw_image_fallback_rect(f, rect, &ib.img);
    }
}

/// The opened message's body as `StyledLine`s (HTML or plain), mirroring the
/// current `body_lines` match on `app.body`/`body_loading` — but returning the
/// neutral `Vec<StyledLine>` (so image markers survive) instead of ratatui lines.
fn render_body(app: &App, width: usize) -> Vec<StyledLine> {
    match (&app.body, app.body_loading) {
        (_, true) => vec![StyledLine { spans: vec![StyledSpan { text: "loading…".into(), ..Default::default() }], ..Default::default() }],
        (Some(b), false) if b.content_type.eq_ignore_ascii_case("html") => htmlrender::render_html(&b.content, width),
        (Some(b), false) => htmlrender::render_text(&b.content, width),
        (None, false) => vec![StyledLine { spans: vec![StyledSpan { text: "(no body)".into(), ..Default::default() }], ..Default::default() }],
    }
}
```
`draw_image_fallback_rect` renders the bordered box captioned with `img.alt` (or `[image]`) — reused unchanged by Task 6 as the fallback:
```rust
fn draw_image_fallback_rect(f: &mut Frame, rect: Rect, img: &ImageRef) {
    let label = if img.alt.is_empty() { "[image]".to_string() } else { format!("[image: {}]", img.alt) };
    f.render_widget(
        Paragraph::new(label).block(Block::default().borders(Borders::ALL).border_style(Style::new().fg(Color::DarkGray))),
        rect,
    );
}
```
(The old `body_lines`/`to_ratatui_line` stay — `body_layout` calls `to_ratatui_line` per text line. Delete the now-unused `Wrap`/`body_lines` single-Paragraph path.)

- [ ] **Step 5: Run to verify pass + a render smoke test**

Add a reading render test (mirror `attachments.rs`'s `TestBackend` pattern) that opens a message whose HTML body has `<img src="cid:x" alt="Logo">` and asserts the drawn buffer contains `[image: Logo]`. Run:
`bash "$LCARGO" test -p lookxy reading_scroll` then `bash "$LCARGO" test -p lookxy`
Expected: PASS.

- [ ] **Step 6: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add -A
git commit -m "lookxy reader: vertical scroll + deterministic layout + inline-image fallback boxes"
```

---

### Task 5: lookxy — resolve `cid`s and fetch inline bytes into memory

**Files:**
- Modify: `lookxy/src/app.rs` (`inline_images` cache, `requested_inline` set, `request_inline_images`, `on_sync_event` handling for `InlineImageReady` + re-trigger on `AttachmentsUpdated`, reset on open)

**Interfaces:**
- Consumes: `htmlrender::image_refs`, `ImageSource::Cid`, `Store::attachments` (now with `content_id`), `SyncCommand::FetchInlineImage`, `SyncEvent::InlineImageReady`/`AttachmentsUpdated`, `SyncCommand::FetchAttachments`.
- Produces: `App.inline_images: HashMap<String, Vec<u8>>` (content_id → bytes) and, for `data:` images, bytes are carried in the `ImageRef` itself (no fetch). Painting (Task 6) reads `inline_images`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn opening_a_message_with_cid_images_requests_their_bytes() {
        let mut app = App::for_test_with_seeded_store();
        // Seed a message with an HTML body referencing cid:logo, and its attachment metadata.
        seed_html_message(&mut app, "mimg",
            r#"<p>hi</p><img src="cid:logo"><p>bye</p>"#);
        app.store.put_attachments("mimg", &[AttachmentMeta {
            id: "att1".into(), name: "logo.png".into(), content_type: "image/png".into(),
            size: 3, is_inline: true, content_id: Some("logo".into()),
        }]).unwrap();
        app.open_message("mimg");                 // loads body + should request inline images
        // A FetchInlineImage for att1/logo was enqueued:
        let cmd = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(cmd, Ok(SyncCommand::FetchInlineImage { attachment_id, content_id, .. })
            if attachment_id == "att1" && content_id == "logo"));
        // Delivering the bytes caches them by content_id:
        app.on_sync_event(SyncEvent::InlineImageReady {
            message_id: "mimg".into(), content_id: "logo".into(), bytes: vec![1,2,3],
        });
        assert_eq!(app.inline_images.get("logo").map(|b| b.as_slice()), Some(&[1,2,3][..]));
    }
```
Write `seed_html_message` (or reuse an existing helper) to insert a message row + an HTML body into the store and add it to `app.messages`, mirroring how `body_loading`/reader tests seed bodies (there is already a body-seeding test path — find it: search app.rs tests for `get_body`/`put_body`/a seeded HTML body and copy it). `FetchBody` may fire too; the test only asserts the `FetchInlineImage` is present (drain/inspect accordingly — if the queue also has `FetchBody`, receive until `FetchInlineImage`).

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy cid_images`
Expected: FAIL — no `inline_images` / `request_inline_images`.

- [ ] **Step 3: Implement**

Add to `App`: `pub inline_images: std::collections::HashMap<String, Vec<u8>>` and `requested_inline: std::collections::HashSet<String>` (content_ids already asked for, to avoid re-firing). Initialize empty. In `open_message`, after `reload_body()`, clear both (`self.inline_images.clear(); self.requested_inline.clear();`) BEFORE requesting, then call `self.request_inline_images();`. Also call `request_inline_images` from `reload_body`'s success path if the body is HTML (so a late `BodyReady` triggers it).

```rust
    /// For the opened HTML message, resolve each `cid:` image to its
    /// attachment and fetch its bytes into `inline_images` (once). Needs the
    /// message's attachment metadata; if that isn't loaded yet, kicks off
    /// `FetchAttachments` and returns — `on_sync_event`'s `AttachmentsUpdated`
    /// arm calls this again once it lands. `data:` images carry their own
    /// bytes and need no fetch; remote/unsupported are skipped.
    pub fn request_inline_images(&mut self) {
        let Some(id) = self.selected_msg.clone() else { return; };
        let Some(body) = &self.body else { return; };
        if !body.content_type.eq_ignore_ascii_case("html") { return; }
        let refs = mailcore::htmlrender::image_refs(&body.content);
        let cids: Vec<String> = refs.iter().filter_map(|r| match &r.src {
            mailcore::htmlrender::ImageSource::Cid(c) => Some(c.clone()),
            _ => None,
        }).collect();
        if cids.is_empty() { return; }
        let metas = self.store.attachments(&id).unwrap_or_default();
        if metas.is_empty() {
            // No metadata yet — fetch it; AttachmentsUpdated re-enters here.
            let _ = self.sync.cmd_tx.send(SyncCommand::FetchAttachments { message_id: id });
            return;
        }
        for cid in cids {
            if self.requested_inline.contains(&cid) { continue; }
            if let Some(att) = metas.iter().find(|a| a.content_id.as_deref() == Some(cid.as_str())) {
                self.requested_inline.insert(cid.clone());
                let _ = self.sync.cmd_tx.send(SyncCommand::FetchInlineImage {
                    message_id: id.clone(), attachment_id: att.id.clone(), content_id: cid,
                });
            }
        }
    }
```
In `on_sync_event`, replace the Task-2 placeholder arm and add the attachments re-trigger:
```rust
            SyncEvent::InlineImageReady { message_id, content_id, bytes }
                if self.selected_msg.as_deref() == Some(message_id.as_str()) =>
            {
                self.inline_images.insert(content_id, bytes);
            }
            SyncEvent::InlineImageReady { .. } => {} // for a message no longer open — drop
```
And in the existing `AttachmentsUpdated` arm (find it — it currently calls `reload_attachments`), after `reload_attachments`, also call `self.request_inline_images();` (now that metadata is present, cids can resolve). Guard on the open message id as that arm already does.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy cid_images` then `bash "$LCARGO" test -p lookxy`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add -A
git commit -m "lookxy: resolve cid images and fetch inline bytes into memory"
```

---

### Task 6: lookxy — paint decoded images over the reserved bands

**Files:**
- Modify: `lookxy/Cargo.toml` (add `ratatui-image`, `image`)
- Modify: `lookxy/src/main.rs` (build `Picker` at startup; `ui::draw` takes `&mut App`)
- Modify: `lookxy/src/ui/mod.rs` (`draw(f, app: &mut App)`; pass `&*app` to immutable panes, `app` to `reading::draw`)
- Modify: `lookxy/src/app.rs` (`picker: Option<Picker>`, `image_protocols` cache, reset on open)
- Modify: `lookxy/src/ui/reading.rs` (`draw(f, app: &mut App, area)`; paint pixels, else fallback box)

**Interfaces:**
- Consumes: `App.inline_images` (Task 5), `ImageSource::{Cid,Data}`, the `ratatui-image` API exactly as `docxy/src/main.rs` uses it (see Global Constraints for the four call sites).
- Produces: pixel rendering with crop-on-scroll; box fallback when `picker` is `None`, bytes are missing, or decode fails.

- [ ] **Step 1: Add deps + capability detection (compile-only checkpoint)**

Add to `lookxy/Cargo.toml` `[dependencies]` the two lines from Global Constraints. In `lookxy/src/main.rs`, add `use ratatui_image::picker::Picker;`. Where the `App` is constructed for the TUI (the `run`/startup path, near where docxy sets `app.picker` at `docxy/src/main.rs:6376`), set:
```rust
    app.picker = Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16))));
```
Add `pub picker: Option<ratatui_image::picker::Picker>` to `App` (init `None` in constructors — the picker is only set on the real TUI path, so tests keep `None` and always hit the fallback box). Run `bash "$LCARGO" build -p lookxy` to confirm the deps resolve and the workspace compiles.

- [ ] **Step 2: Protocol cache**

The `&mut App` draw signature was already flipped in Task 4 — no signature change here. Add to `App`: `image_protocols: std::collections::HashMap<String, ratatui_image::protocol::Protocol>` (key = `content_id`/data-hash + box dims), initialized empty in the constructors; clear it in `open_message` alongside `inline_images`.

Run `bash "$LCARGO" build -p lookxy`.

- [ ] **Step 3: Write the failing test (fallback path, picker absent)**

In `reading.rs` tests, assert that with `app.picker == None` (the test default), an inline `cid:` image whose bytes ARE cached still renders the fallback box (no panic, box shown) — pixels require a real terminal, so tests only exercise the box path:
```rust
    #[test]
    fn cid_image_without_graphics_capability_draws_the_box() {
        let mut app = App::for_test_with_seeded_store();
        seed_html_message(&mut app, "mimg", r#"<img src="cid:logo" alt="Logo">"#);
        app.inline_images.insert("logo".into(), vec![0,1,2]); // bytes present but no Picker
        app.open_message("mimg");
        app.inline_images.insert("logo".into(), vec![0,1,2]); // re-add (open cleared it)
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("[image: Logo]"));
    }
```

- [ ] **Step 4: Run to verify it fails (or drives the change)**

Run: `bash "$LCARGO" test -p lookxy cid_image_without_graphics`
Expected: initially FAIL to compile against the new `&mut` draw / or pass trivially if Task 4's box already shows — if it already passes, keep it as a guard and proceed (the real work is the paint branch below).

- [ ] **Step 5: Implement the paint pass**

In `reading::draw` (already `&mut App` from Task 4), change the image-box loop so each `ImgBox` tries pixels first and falls back to the Task-4 box. The crop math (`wtop`/`wbot`/`rect`) is unchanged from Task 4; only the body of the loop changes. Mirror `docxy/src/main.rs:2961-2974` (encode) and `:3017-3065` (crop-on-scroll draw):
```rust
    for ib in &images {
        let wtop = scroll.saturating_sub(ib.row);
        let wbot = (scroll + vh).saturating_sub(ib.row).min(IMAGE_BOX_ROWS);
        if wbot <= wtop { continue; }
        let y = body_area.y + (ib.row + wtop - scroll) as u16;
        let rect = Rect { x: body_area.x, y, width: body_area.width, height: (wbot - wtop) as u16 };
        // Resolve bytes + a stable cache key from the source (immutable reads first).
        let resolved: Option<(String, &[u8])> = match &ib.img.src {
            ImageSource::Cid(c) => app.inline_images.get(c).map(|b| (format!("cid:{c}"), b.as_slice())),
            ImageSource::Data { bytes, .. } => Some((format!("data:{}", bytes.len()), bytes.as_slice())),
            _ => None, // Remote / Unsupported → box
        };
        let painted = match (&app.picker, resolved) {
            (Some(picker), Some((key, bytes))) =>
                paint_inline_image(f, picker, &mut app.image_protocols, &key, bytes, rect),
            _ => false,
        };
        if !painted { draw_image_fallback_rect(f, rect, &ib.img); }
    }
```
`paint_inline_image(f: &mut Frame, picker: &Picker, cache: &mut HashMap<String, Protocol>, key: &str, bytes: &[u8], rect: Rect) -> bool` — a FREE function (split borrows: it takes `&Picker` and `&mut cache` separately, so it doesn't overlap the `&app.picker` / `&mut app.image_protocols` borrows; resolve `resolved` above before calling, as shown):
- Cache key = `format!("{key}#{}x{}", rect.width, rect.height)`. If absent from `cache`: `let img = image::load_from_memory(bytes).ok()?;` (on `None`/decode failure return `false`); then build the protocol exactly as docxy's `encode` closure (`docxy/src/main.rs:2961-2974`): `picker.new_protocol(img, rect, Resize::Fit(None)).ok()` → `Option<Protocol>` (for email, encode the whole scaled image to `rect` — no mid-image crop). On `None` return `false`; else `cache.insert(cache_key.clone(), proto)`.
- `f.render_widget(Image::new(cache.get(&cache_key).unwrap()), rect)` (docxy `main.rs:3056`). Return `true`.

Note: pixels for a partially-scrolled band paint into the visible `rect` only; the `new_protocol` re-scales to that rect (v8's `Resize::Fit`), which is acceptable — docxy's finer per-window crop is an enhancement not required for email. `draw_image_fallback_rect` is the Task-4 helper, reused unchanged.

- [ ] **Step 6: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy` (the box-path test passes; pixel emission is manual). Then whole workspace: `bash "$LCARGO" test`.
Expected: PASS.

- [ ] **Step 7: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "lookxy reader: paint decoded cid/data images over reserved bands (ratatui-image), box fallback"
```

---

## Notes for the implementer

- **Copy the graphics API, don't invent it.** Every `ratatui-image` v8 call has a working example in `docxy/src/main.rs` (cited in Global Constraints). If a call doesn't type-check, diff against docxy's usage rather than guessing — the crate version is identical.
- **Tests never render real pixels.** `app.picker` is `None` in every test, so tests exercise the fallback-box path and the data-plumbing (fetch/cache/resolve). Actual pixel output is verified manually in a graphics-capable terminal, exactly as `docxy`/`xlsxy` do.
- **Additive fields ripple.** After Tasks 1 and 3, run a full `bash "$LCARGO" build` and fix every "missing field `content_id`/`image`" — the `..Default::default()` spread avoids most, but literal struct constructions in tests need the field.
- **Remote images are never fetched** — `classify_img_src` marks them `Remote` and `request_inline_images` skips everything but `Cid`; there is no HTTP image path anywhere. Keep it that way.
- **Borrow discipline in the paint pass:** resolve the image bytes and cache key from `&app` first, then call the paint helper with a split `&Picker` + `&mut HashMap` so the immutable body-row iteration and the mutable protocol cache don't overlap.
