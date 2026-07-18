# lookxy Outbound Attachments + Signatures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Attach local files to a compose draft, upload them to the Graph draft at send time (inline ≤3MB / chunked upload session >3MB), and auto-append a configurable signature to new messages.

**Architecture:** Attachments are local file *references* stored in a new `outbound_attachments` table keyed by draft id; on `SendDraft` the outbox reads each file's bytes and uploads them to the Graph draft, then sends, then clears them. A file-picker popup selects files; a config `signature` string is appended to new composes.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), rusqlite (bundled SQLite), ureq+rustls Graph client, ratatui 0.29, hand-rolled `mailcore::json` (no serde). No new dependencies.

## Global Constraints

- **Build/test ONLY through the wrapper** (bare `cargo` fails with os error 448). Every command is `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **No new dependencies.** Hand-rolled base64/JSON; `ureq` (already a dep) for HTTP.
- **MSRV 1.88, edition 2024.** Let-chains available; extern blocks are `unsafe extern`.
- **CI runs `cargo clippy --all-targets -- -D warnings` on ubuntu/macos/windows.** No warnings; no `#[cfg(windows)]`-only bindings left unused on Unix. Run `bash "$LCARGO" fmt` before every commit.
- **Preserve existing behavior.** Existing mail (flat/threaded), compose (To/Cc/Bcc/autocomplete/Subject/Body), draft save/send, and incoming-attachment save/open must stay green; extend only where a task says so.
- **Upload at send only.** Attachments upload during `SendDraft`, never `SaveDraft`. The local `outbound_attachments` table is the source of truth; cleared only after a successful send.
- **3MB inline threshold** (`INLINE_MAX = 3 * 1024 * 1024`); chunk size a multiple of 320 KiB (`CHUNK_SIZE = 12 * 320 * 1024` = 3,932,160 bytes).
- **`fileAttachment.contentBytes` uses STANDARD base64** (`+`/`/`, `=` padding) — not the url-safe encoder in `pkce`.

---

### Task 1: Standard base64 encoder

**Files:**
- Modify: `mailcore/src/graph/client.rs` (add `base64_encode` next to the existing `base64_decode`; add tests)

**Interfaces:**
- Produces: `pub(crate) fn base64_encode(bytes: &[u8]) -> String` — RFC 4648 §4 standard base64 with `=` padding.

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/graph/client.rs` tests:

```rust
    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        // round-trips with the existing decoder
        let raw: &[u8] = &[0, 1, 2, 250, 251, 252, 253, 255];
        assert_eq!(base64_decode(&base64_encode(raw)).unwrap(), raw);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore base64_encode`
Expected: FAIL — `cannot find function base64_encode`.

- [ ] **Step 3: Implement**

Add near `base64_decode` in `client.rs`:

```rust
/// Standard base64 (RFC 4648 §4: `+`/`/` alphabet, `=` padding) — the encoding
/// Graph expects for `fileAttachment.contentBytes`. The `pkce` module's
/// base64*url* encoder can't be reused (different alphabet, no padding).
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[((n >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore base64_encode`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/graph/client.rs
git commit -m "mailcore: standard base64 encoder for attachment contentBytes"
```

---

### Task 2: `outbound_attachments` store table + CRUD + reconcile re-point

**Files:**
- Modify: `mailcore/src/store/schema.rs` (add the table to `SCHEMA_SQL`)
- Modify: `mailcore/src/store/mod.rs` (`OutboundAttachment`, CRUD methods, `reconcile_id` re-point; add tests)

**Interfaces:**
- Produces:
  - `pub struct OutboundAttachment { pub path: String, pub name: String, pub size: i64 }`
  - `pub fn add_outbound_attachment(&self, draft_id, path, name, size) -> Result<(), StoreError>`
  - `pub fn outbound_attachments(&self, draft_id) -> Result<Vec<OutboundAttachment>, StoreError>`
  - `pub fn remove_outbound_attachment(&self, draft_id, path) -> Result<(), StoreError>`
  - `pub fn clear_outbound_attachments(&self, draft_id) -> Result<(), StoreError>`
  - `reconcile_id` re-points `outbound_attachments.draft_id`.

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/store/mod.rs` tests:

```rust
    #[test]
    fn outbound_attachment_crud_and_dedup() {
        let s = Store::open_in_memory().unwrap();
        s.add_outbound_attachment("local:d1", "/tmp/a.pdf", "a.pdf", 10).unwrap();
        s.add_outbound_attachment("local:d1", "/tmp/b.txt", "b.txt", 20).unwrap();
        s.add_outbound_attachment("local:d1", "/tmp/a.pdf", "a.pdf", 10).unwrap(); // dup path → no-op
        let got = s.outbound_attachments("local:d1").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a.pdf"); // ordered by name
        s.remove_outbound_attachment("local:d1", "/tmp/a.pdf").unwrap();
        assert_eq!(s.outbound_attachments("local:d1").unwrap().len(), 1);
        s.clear_outbound_attachments("local:d1").unwrap();
        assert!(s.outbound_attachments("local:d1").unwrap().is_empty());
    }

    #[test]
    fn reconcile_id_repoints_outbound_attachments() {
        let s = Store::open_in_memory().unwrap();
        // a local draft (message + body) plus a pending attachment on it
        let id = s.create_local_draft("Sub", "", "", "body").unwrap();
        s.add_outbound_attachment(&id, "/tmp/a.pdf", "a.pdf", 10).unwrap();
        s.reconcile_id(&id, "GRAPH-1").unwrap();
        assert!(s.outbound_attachments(&id).unwrap().is_empty());          // old id emptied
        assert_eq!(s.outbound_attachments("GRAPH-1").unwrap().len(), 1);    // moved to graph id
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore outbound_attachment reconcile_id_repoints`
Expected: FAIL — no such methods / table.

- [ ] **Step 3: Implement**

In `schema.rs`, add to `SCHEMA_SQL`:

```sql
CREATE TABLE IF NOT EXISTS outbound_attachments (
    draft_id  TEXT NOT NULL,
    path      TEXT NOT NULL,
    name      TEXT NOT NULL,
    size      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (draft_id, path)
);
```

In `mod.rs`, add the struct (near `AttachmentMeta`/`MessageRow`):

```rust
/// A file the user has attached to an outbound draft — a reference (path +
/// name + size), not bytes; the bytes are read from disk at send time.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundAttachment {
    pub path: String,
    pub name: String,
    pub size: i64,
}
```

Add the methods to `impl Store`:

```rust
    /// Records a file attached to draft `draft_id`. Idempotent per (draft, path).
    pub fn add_outbound_attachment(&self, draft_id: &str, path: &str, name: &str, size: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO outbound_attachments (draft_id, path, name, size) VALUES (?1, ?2, ?3, ?4)",
            params![draft_id, path, name, size],
        )?;
        Ok(())
    }

    /// The files attached to `draft_id`, ordered by name.
    pub fn outbound_attachments(&self, draft_id: &str) -> Result<Vec<OutboundAttachment>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT path, name, size FROM outbound_attachments WHERE draft_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map(params![draft_id], |r| {
                Ok(OutboundAttachment { path: r.get(0)?, name: r.get(1)?, size: r.get(2)? })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Removes one attached file from `draft_id` (by path). No-op if absent.
    pub fn remove_outbound_attachment(&self, draft_id: &str, path: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM outbound_attachments WHERE draft_id = ?1 AND path = ?2",
            params![draft_id, path],
        )?;
        Ok(())
    }

    /// Removes every attached file from `draft_id` (after a send, or on discard).
    pub fn clear_outbound_attachments(&self, draft_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM outbound_attachments WHERE draft_id = ?1",
            params![draft_id],
        )?;
        Ok(())
    }
```

In `reconcile_id` (mod.rs:797), add this `tx.execute` alongside the existing `messages`/`bodies` updates, before `tx.commit()`:

```rust
        tx.execute(
            "UPDATE outbound_attachments SET draft_id = ?2 WHERE draft_id = ?1",
            params![local_id, graph_id],
        )?;
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore outbound_attachment reconcile_id_repoints` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/store/schema.rs mailcore/src/store/mod.rs
git commit -m "mailcore: outbound_attachments table + CRUD + reconcile re-point"
```

---

### Task 3: Graph inline attachment (`add_file_attachment`)

**Files:**
- Modify: `mailcore/src/graph/client.rs` (add `add_file_attachment`; add a test)

**Interfaces:**
- Consumes: `base64_encode` (Task 1), the existing `send`/`encode_path_segment`/`Value`.
- Produces: `pub fn add_file_attachment(&self, message_id, name, content_type, bytes: &[u8]) -> Result<(), GraphError>`.

- [ ] **Step 1: Write the failing test**

Add to `client.rs` tests (mirror the `create_draft_posts_body_and_parses_returned_draft` FakeServer/captured-body style):

```rust
    #[test]
    fn add_file_attachment_posts_a_file_attachment_with_base64_bytes() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(), path_prefix: "/me/messages/M1/attachments".into(),
            status: 201, headers: vec![], body: r#"{"id":"att1"}"#.into(),
        }]);
        let c = GraphClient::new(srv.base_url(), "T".into()); // adapt to the real constructor
        c.add_file_attachment("M1", "hello.txt", "text/plain", b"hello").unwrap();
        let body = srv.requests()[0].body.clone(); // adapt to how the existing tests read the captured body
        let sent = mailcore::json::parse(&body).unwrap();
        use mailcore::json::Value;
        assert_eq!(sent.get("@odata.type").and_then(Value::as_str), Some("#microsoft.graph.fileAttachment"));
        assert_eq!(sent.get("name").and_then(Value::as_str), Some("hello.txt"));
        assert_eq!(sent.get("contentType").and_then(Value::as_str), Some("text/plain"));
        assert_eq!(sent.get("contentBytes").and_then(Value::as_str), Some("aGVsbG8=")); // base64("hello")
    }
```

(Adapt `GraphClient::new`, `Route`, and the captured-body access to the exact shapes the neighbouring tests use.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore add_file_attachment`
Expected: FAIL — `no method named add_file_attachment`.

- [ ] **Step 3: Implement**

Add to `impl GraphClient`:

```rust
    /// POST `/me/messages/{id}/attachments` with an inline
    /// `#microsoft.graph.fileAttachment` (base64 `contentBytes`). For files
    /// ≤ 3MB; larger files go through `upload_large_attachment` (see
    /// `add_attachment`).
    pub fn add_file_attachment(&self, message_id: &str, name: &str, content_type: &str, bytes: &[u8]) -> Result<(), GraphError> {
        let id = encode_path_segment(message_id);
        let path = format!("/me/messages/{id}/attachments");
        let body = Value::Object(vec![
            ("@odata.type".to_string(), Value::Str("#microsoft.graph.fileAttachment".to_string())),
            ("name".to_string(), Value::Str(name.to_string())),
            ("contentType".to_string(), Value::Str(content_type.to_string())),
            ("contentBytes".to_string(), Value::Str(base64_encode(bytes))),
        ])
        .to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore add_file_attachment`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/graph/client.rs
git commit -m "mailcore: GraphClient::add_file_attachment (inline fileAttachment)"
```

---

### Task 4: Graph chunked upload session + `add_attachment` dispatcher

**Files:**
- Modify: `mailcore/src/graph/client.rs` (`add_attachment`, `upload_large_attachment`, `put_bytes`; consts; add tests)

**Interfaces:**
- Consumes: `add_file_attachment` (Task 3), `send`, `parse_body`, `classify_status`, `encode_path_segment`, `ureq`.
- Produces: `pub fn add_attachment(&self, message_id, name, content_type, bytes) -> Result<(), GraphError>` — the size-routing dispatcher.

- [ ] **Step 1: Write the failing tests**

Add to `client.rs` tests. The chunked path posts `createUploadSession`, reads `uploadUrl`, then PUTs chunks to it. Point the returned `uploadUrl` back at the FakeServer so the PUTs are captured:

```rust
    #[test]
    fn add_attachment_routes_small_inline_and_large_to_upload_session() {
        // small: routes to inline POST /attachments
        let small = FakeServer::start(vec![Route {
            method: "POST".into(), path_prefix: "/me/messages/M1/attachments".into(),
            status: 201, headers: vec![], body: r#"{"id":"a"}"#.into(),
        }]);
        let c = GraphClient::new(small.base_url(), "T".into());
        c.add_attachment("M1", "s.txt", "text/plain", b"tiny").unwrap();
        assert!(small.requests().iter().any(|r| r.path.contains("/attachments") && !r.path.contains("createUploadSession")));

        // large: createUploadSession → chunked PUT(s) to the returned uploadUrl
        let big = vec![7u8; 3 * 1024 * 1024 + 100]; // just over INLINE_MAX → one upload session, ≥1 chunk
        let session_body = format!(r#"{{"uploadUrl":"{}/upload/xyz"}}"#, /* the FakeServer base */ "BASE");
        // Build routes: POST createUploadSession → returns uploadUrl at this server; PUT /upload/xyz → 201.
        // (Construct `session_body` with the actual base_url; the FakeServer must accept PUT and record it.)
        // Assert: exactly one createUploadSession POST, and the PUT(s) cover the whole payload with
        // correct Content-Range headers summing to big.len().
    }
```

Note: this test depends on the FakeServer's capabilities. READ `mailcore/src/testserver.rs` first: confirm it (a) matches `PUT`, (b) records request bodies and headers. Write the test to what it actually supports — at minimum assert the `createUploadSession` POST happened and that a `PUT` to the `uploadUrl` path happened carrying the payload bytes; if headers are captured, also assert the `Content-Range` sequence covers `0..big.len()`. If the FakeServer cannot express the two-hop (session URL → PUT), split into: a unit test of the chunk-range arithmetic (a small pure helper `chunk_ranges(total, chunk) -> Vec<(usize, usize)>` you extract and test directly) plus the inline-vs-session routing assertion above. Keep assertions meaningful; do not weaken to a tautology.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore add_attachment`
Expected: FAIL — `no method named add_attachment`.

- [ ] **Step 3: Implement**

Add consts (module scope in `client.rs`):

```rust
/// Graph's inline-attachment ceiling: files this size or smaller go inline as a
/// fileAttachment; larger ones use an upload session.
const INLINE_MAX: usize = 3 * 1024 * 1024;
/// Upload-session chunk size — a multiple of 320 KiB per Graph's requirement.
const CHUNK_SIZE: usize = 12 * 320 * 1024; // 3,932,160 bytes
```

Add to `impl GraphClient`:

```rust
    /// Attaches `bytes` to the draft `message_id`, routing by size: inline
    /// `fileAttachment` for ≤ `INLINE_MAX`, an upload session (chunked PUT) for
    /// larger files.
    pub fn add_attachment(&self, message_id: &str, name: &str, content_type: &str, bytes: &[u8]) -> Result<(), GraphError> {
        if bytes.len() <= INLINE_MAX {
            self.add_file_attachment(message_id, name, content_type, bytes)
        } else {
            self.upload_large_attachment(message_id, name, content_type, bytes)
        }
    }

    /// POST `.../attachments/createUploadSession`, then PUT the bytes to the
    /// returned pre-authenticated `uploadUrl` in `CHUNK_SIZE` chunks, each with
    /// a `Content-Range` header, until the last chunk completes.
    fn upload_large_attachment(&self, message_id: &str, name: &str, content_type: &str, bytes: &[u8]) -> Result<(), GraphError> {
        let id = encode_path_segment(message_id);
        let path = format!("/me/messages/{id}/attachments/createUploadSession");
        let body = Value::Object(vec![(
            "AttachmentItem".to_string(),
            Value::Object(vec![
                ("attachmentType".to_string(), Value::Str("file".to_string())),
                ("name".to_string(), Value::Str(name.to_string())),
                ("size".to_string(), Value::Num(bytes.len() as f64)),
                ("contentType".to_string(), Value::Str(content_type.to_string())),
            ]),
        )])
        .to_string();
        let resp = self.send(Method::Post, &path, Some(body), &[])?;
        let v = parse_body(resp)?;
        let upload_url = v
            .get("uploadUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| GraphError::Parse("createUploadSession response has no uploadUrl".to_string()))?
            .to_string();

        let total = bytes.len();
        let mut start = 0;
        while start < total {
            let end = (start + CHUNK_SIZE).min(total);
            let range = format!("bytes {}-{}/{}", start, end - 1, total);
            self.put_bytes(&upload_url, &range, &bytes[start..end])?;
            start = end;
        }
        Ok(())
    }

    /// Raw `PUT` of one `chunk` to a Graph upload-session `upload_url` with a
    /// `Content-Range` header and NO bearer (the session URL is already
    /// authorized). A 2xx (202 for an accepted intermediate chunk, 200/201 for
    /// the final one) is Ok; anything else maps to a `GraphError`.
    fn put_bytes(&self, upload_url: &str, content_range: &str, chunk: &[u8]) -> Result<(), GraphError> {
        match ureq::put(upload_url).set("Content-Range", content_range).send_bytes(chunk) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, resp)) => Err(classify_status(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(GraphError::Transport(t.to_string())),
        }
    }
```

If the test uses a `chunk_ranges` helper, extract the `while` arithmetic into:
```rust
fn chunk_ranges(total: usize, chunk: usize) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + chunk).min(total);
        v.push((start, end));
        start = end;
    }
    v
}
```
and have `upload_large_attachment` iterate `chunk_ranges(total, CHUNK_SIZE)`.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore add_attachment` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/graph/client.rs
git commit -m "mailcore: chunked upload session + add_attachment size dispatcher"
```

---

### Task 5: Outbox uploads attachments on `SendDraft`

**Files:**
- Modify: `mailcore/src/sync/outbox.rs` (`SendDraft` arm; add `content_type_for`; add a test)

**Interfaces:**
- Consumes: `Store::outbound_attachments`/`clear_outbound_attachments` (Task 2), `GraphClient::add_attachment` (Task 4).

- [ ] **Step 1: Write the failing test**

Add to `outbox.rs` tests (mirror `apply_op_save_draft_creates_and_reconciles_a_local_draft`). Seed a local draft + a pending attachment pointing at a real temp file, serve the draft-create/attachment/send routes, run `apply_op(SendDraft)`, assert the attachment POST happened and the pending row was cleared:

```rust
    #[test]
    fn send_draft_uploads_pending_attachments_then_clears_them() {
        // temp file to attach
        let dir = std::env::temp_dir().join(format!("lookxy-att-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, b"hello").unwrap();

        // ... seed store: create_local_draft, add_outbound_attachment(draft_id, file, "note.txt", 5) ...
        // ... FakeServer routes: create_draft (POST /me/messages) → returns a Graph id,
        //     POST /me/messages/{gid}/attachments (the inline upload) → 201,
        //     POST /me/messages/{gid}/send → 202, as the existing send-draft test wires them ...
        // apply_op(&client, &store, &OutboxOp::SendDraft { id: draft_id }).unwrap();

        // the attachment POST was made, and the pending rows are cleared after send:
        assert!(srv.requests().iter().any(|r| r.path.contains("/attachments")));
        assert!(store.outbound_attachments("GRAPH-ID").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_draft_errors_and_keeps_attachments_when_a_file_is_missing() {
        // ... seed a local draft + a pending attachment whose path does NOT exist ...
        // let r = apply_op(&client, &store, &OutboxOp::SendDraft { id: draft_id });
        // assert!(r.is_err());
        // the pending attachment is NOT cleared (so a retry can re-upload once the file is back)
        // assert!(!store.outbound_attachments(<current draft id>).unwrap().is_empty());
    }
```

Adapt the store seeding, `FakeServer`/`Route`, and the graph-id the create-draft route returns to the existing `send_draft`-of-a-local-draft test in this module; keep the assertions.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore send_draft_uploads send_draft_errors_and_keeps`
Expected: FAIL — attachments never uploaded / not cleared (no wiring yet).

- [ ] **Step 3: Implement**

Replace the `SendDraft` arm of `apply_op` (outbox.rs:35-38) with:

```rust
        OutboxOp::SendDraft { id } => {
            let graph_id = ensure_draft_on_graph(client, store, id)?;
            // Upload each pending attachment to the (now-on-Graph) draft before
            // sending. A file-read or upload error returns here, so the drain's
            // retry/quarantine policy applies and the attachments are NOT
            // cleared — the send hasn't happened, so a retry is clean.
            for att in store
                .outbound_attachments(&graph_id)
                .map_err(|e| GraphError::Parse(e.to_string()))?
            {
                let bytes = std::fs::read(&att.path)
                    .map_err(|e| GraphError::Parse(format!("cannot read attachment {}: {e}", att.path)))?;
                client.add_attachment(&graph_id, &att.name, &content_type_for(&att.name), &bytes)?;
            }
            client.send_draft(&graph_id)?;
            store
                .clear_outbound_attachments(&graph_id)
                .map_err(|e| GraphError::Parse(e.to_string()))?;
            Ok(())
        }
```

Add the content-type helper (module scope in `outbox.rs`):

```rust
/// A best-effort MIME type from a file name's extension, defaulting to
/// `application/octet-stream`. Small built-in map — no new dependency.
fn content_type_for(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_string()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore send_draft` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green (existing send-draft tests still pass — a draft with no pending attachments just skips the loop).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/sync/outbox.rs
git commit -m "mailcore: upload pending attachments in the SendDraft outbox path"
```

---

### Task 6: Signature — config + new-message append

**Files:**
- Modify: `lookxy/src/config.rs` (`signature` field + overlays)
- Modify: `lookxy/src/main.rs` (seed `app.signature`)
- Modify: `lookxy/src/app.rs` (`App.signature` field; `compose_new` appends the signature; add `signature_body_html`; tests)

**Interfaces:**
- Consumes: `Store::create_local_draft` (existing, arg order `subject, to, cc, body_html`).
- Produces: `Config.signature: String`; `App.signature: String`; `fn signature_body_html(&str) -> String`.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/config.rs` tests:

```rust
    #[test]
    fn signature_defaults_empty_and_file_overlay_sets_it() {
        assert_eq!(Config::default().signature, "");
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!("lookxy-config-{}-sig", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"signature":"--\nBoris"}"#).unwrap();
        assert_eq!(Config::load_from(Some(&path)).signature, "--\nBoris");
        let _ = std::fs::remove_dir_all(&dir);
    }
```

Add to `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn signature_body_html_wraps_lines_and_is_empty_when_blank() {
        assert_eq!(signature_body_html(""), "");
        assert_eq!(signature_body_html("   "), "");
        let html = signature_body_html("Boris\nEPAM");
        assert!(html.contains("<p>--</p>"));
        assert!(html.contains("<p>Boris</p>"));
        assert!(html.contains("<p>EPAM</p>"));
    }

    #[test]
    fn compose_new_seeds_the_signature_into_the_draft_body() {
        let mut app = App::for_test_with_seeded_store();
        app.signature = "Boris".into();
        app.compose_new();
        let editor_text = app.compose.as_ref().unwrap().editor.text();
        assert!(editor_text.contains("Boris")); // signature landed in the composer body
    }
```

(Adapt `editor.text()` to the actual `editcore::Editor` accessor the existing compose tests use to read body text.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy signature`
Expected: FAIL — `no field signature` / `cannot find function signature_body_html`.

- [ ] **Step 3: Implement**

In `config.rs`: add `pub signature: String` to `Config`; default `String::new()`; in `overlay_json` add
```rust
        if let Some(s) = value.get("signature").and_then(|v| v.as_str()) {
            self.signature = s.to_string();
        }
```
and in `overlay_env` add
```rust
        if let Ok(v) = std::env::var("LOOKXY_SIGNATURE") {
            self.signature = v;
        }
```

In `main.rs`, after `App::new(...)` (near where `app.threaded`/`app.config_path` are set), add:
```rust
    app.signature = config.signature.clone();
```

In `app.rs`: add `pub signature: String` to `App` (init `signature: String::new()` in `App::new`). Rewrite `compose_new`:
```rust
    pub fn compose_new(&mut self) {
        let body = signature_body_html(&self.signature);
        if let Ok(id) = self.store.create_local_draft("", "", "", &body) {
            self.open_draft(&id);
        }
    }
```
Add the helper (module scope in `app.rs`):
```rust
/// Builds the initial body HTML for a new message: empty when the signature is
/// blank; otherwise an empty first paragraph (where the cursor lands), a `--`
/// separator, then one paragraph per signature line (HTML-escaped). Only new
/// messages get this — reply/forward bodies come from Graph untouched.
fn signature_body_html(sig: &str) -> String {
    if sig.trim().is_empty() {
        return String::new();
    }
    let mut html = String::from("<p></p><p>--</p>");
    for line in sig.lines() {
        html.push_str(&format!("<p>{}</p>", mailcore::compose_html::escape_html(line)));
    }
    html
}
```
(If `mailcore::compose_html::escape_html` is not `pub`, either make it `pub` in a one-line change or inline a minimal escape of `&`/`<`/`>`; confirm which and keep it DRY.)

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy signature` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/config.rs lookxy/src/main.rs lookxy/src/app.rs
git commit -m "lookxy: config signature appended to new-message body"
```

---

### Task 7: File-picker popup

**Files:**
- Create: `lookxy/src/ui/filepicker.rs` (the `FilePicker` state + `draw`)
- Modify: `lookxy/src/ui/mod.rs` (`mod filepicker;`, key routing, draw call), `lookxy/src/app.rs` (`App.file_picker` field)

**Interfaces:**
- Produces:
  - `pub struct FileEntry { pub name: String, pub path: PathBuf, pub is_dir: bool, pub size: u64 }`
  - `pub struct FilePicker { pub dir: PathBuf, pub entries: Vec<FileEntry>, pub index: usize }`
  - `FilePicker::open(dir) -> FilePicker`, `move_selection(&mut self, delta: isize)`, `enter(&mut self) -> Option<PathBuf>` (Some(file path) when a file is chosen; None when it navigated into a directory)
  - `App.file_picker: Option<FilePicker>`

- [ ] **Step 1: Write the failing tests**

Create `lookxy/src/ui/filepicker.rs` with a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_parent_then_dirs_then_files_and_navigates() {
        let base = std::env::temp_dir().join(format!("lookxy-fp-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("z.txt"), b"z").unwrap();
        std::fs::write(base.join("a.txt"), b"a").unwrap();

        let mut fp = FilePicker::open(base.clone());
        // ".." first, then the directory "sub", then files a.txt, z.txt (dirs before files, each sorted)
        assert_eq!(fp.entries[0].name, "..");
        let names: Vec<&str> = fp.entries.iter().map(|e| e.name.as_str()).collect();
        let sub_i = names.iter().position(|n| *n == "sub").unwrap();
        let a_i = names.iter().position(|n| *n == "a.txt").unwrap();
        assert!(sub_i < a_i, "directories sort before files");

        // navigating into "sub" (a directory) returns None and changes dir
        fp.index = sub_i;
        assert_eq!(fp.enter(), None);
        assert_eq!(fp.dir, sub);

        // selecting a file returns its path
        let mut fp2 = FilePicker::open(base.clone());
        let ai = fp2.entries.iter().position(|e| e.name == "a.txt").unwrap();
        fp2.index = ai;
        assert_eq!(fp2.enter(), Some(base.join("a.txt")));

        // move is clamped
        let mut fp3 = FilePicker::open(base.clone());
        fp3.move_selection(-1);
        assert_eq!(fp3.index, 0);

        let _ = std::fs::remove_dir_all(&base);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy filepicker`
Expected: FAIL — module/types don't exist.

- [ ] **Step 3: Implement**

Write `lookxy/src/ui/filepicker.rs`:

```rust
//! A filesystem-browser popup for choosing a file to attach: navigate
//! directories (`..` goes up, Enter on a directory descends), Enter on a file
//! selects it. Modeled on the other list popups (`message_list::draw_move_picker`).

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::ui::centered_rect;

/// One row in the file picker: a directory (including the synthetic `..`) or a file.
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

/// The open file picker: the directory being browsed, its entries, and the cursor.
pub struct FilePicker {
    pub dir: PathBuf,
    pub entries: Vec<FileEntry>,
    pub index: usize,
}

impl FilePicker {
    /// Opens the picker on `dir`, listing its entries.
    pub fn open(dir: PathBuf) -> FilePicker {
        let entries = read_entries(&dir);
        FilePicker { dir, entries, index: 0 }
    }

    /// Moves the cursor, clamped to the entry list.
    pub fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = (self.entries.len() - 1) as isize;
        self.index = (self.index as isize + delta).clamp(0, max) as usize;
    }

    /// Enter on the selected entry: descends into a directory (re-lists, returns
    /// `None`) or selects a file (returns its path). `None` on an empty list.
    pub fn enter(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.index)?;
        if entry.is_dir {
            let dir = entry.path.clone();
            self.dir = dir.clone();
            self.entries = read_entries(&dir);
            self.index = 0;
            None
        } else {
            Some(entry.path.clone())
        }
    }
}

/// Lists `dir`: a synthetic `..` (its parent) first when there is one, then
/// subdirectories (sorted by name), then files (sorted by name). Unreadable
/// entries and the directory itself failing to read are skipped defensively.
fn read_entries(dir: &Path) -> Vec<FileEntry> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => dirs.push(FileEntry { name, path, is_dir: true, size: 0 }),
                Ok(ft) if ft.is_file() => {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push(FileEntry { name, path, is_dir: false, size });
                }
                _ => {}
            }
        }
    }
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = Vec::new();
    if let Some(parent) = dir.parent() {
        out.push(FileEntry { name: "..".to_string(), path: parent.to_path_buf(), is_dir: true, size: 0 });
    }
    out.extend(dirs);
    out.extend(files);
    out
}

/// Renders the file picker as a centered overlay when `app.file_picker` is set.
pub fn draw(f: &mut Frame, app: &crate::app::App) {
    let Some(fp) = &app.file_picker else {
        return;
    };
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);
    let items: Vec<ListItem> = fp
        .entries
        .iter()
        .map(|e| {
            let line = if e.is_dir {
                format!("{}/", e.name)
            } else {
                format!("{}  {} B", e.name, e.size)
            };
            ListItem::new(line)
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!("Attach a file — {}", fp.dir.display()))
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));
    let mut state = ListState::default();
    if !fp.entries.is_empty() {
        state.select(Some(fp.index.min(fp.entries.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}
```

In `lookxy/src/ui/mod.rs`: add `pub mod filepicker;` near the other `mod` lines; confirm `centered_rect` is reachable as `crate::ui::centered_rect` (it's used by the other popups — make it `pub(crate)` if needed). In `handle_key`, add — BEFORE the `if app.compose.is_some()` branch (so the picker, drawn over the composer, gets keys first):
```rust
    if app.file_picker.is_some() {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => { if let Some(fp) = app.file_picker.as_mut() { fp.move_selection(-1); } }
            KeyCode::Down | KeyCode::Char('j') => { if let Some(fp) = app.file_picker.as_mut() { fp.move_selection(1); } }
            KeyCode::Enter => app.file_picker_enter(),
            KeyCode::Esc => app.file_picker = None,
            _ => {}
        }
        return;
    }
```
In `ui::draw`, after the compose draw (and other popups), add `filepicker::draw(f, app);`.

In `lookxy/src/app.rs`: add `pub file_picker: Option<crate::ui::filepicker::FilePicker>` to `App` (init `file_picker: None`). Add a stub `pub fn file_picker_enter(&mut self) {}` for now — Task 8 fills it in (this keeps Task 7 self-contained and compiling; a no-op enter is harmless until wired).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy filepicker` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/filepicker.rs lookxy/src/ui/mod.rs lookxy/src/app.rs
git commit -m "lookxy: file-picker popup (browse + select a file)"
```

---

### Task 8: Compose attachments — attach/remove + list + wiring

**Files:**
- Modify: `lookxy/src/ui/compose.rs` (`Compose.attachments`, draw row, Ctrl+O/Ctrl+R handling)
- Modify: `lookxy/src/app.rs` (`open_draft` loads attachments; `file_picker_enter`/`attach_file`; discard clears)

**Interfaces:**
- Consumes: `Store::{outbound_attachments,add_outbound_attachment,remove_outbound_attachment,clear_outbound_attachments}` (Task 2), `FilePicker` (Task 7), `mailcore::store::OutboundAttachment`.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn attaching_a_file_records_it_on_the_draft_and_in_compose() {
        let mut app = App::for_test_with_seeded_store();
        app.compose_new(); // opens a fresh local draft
        let draft_id = app.compose.as_ref().unwrap().draft_id.clone();
        // a real temp file to attach
        let dir = std::env::temp_dir().join(format!("lookxy-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("r.pdf");
        std::fs::write(&file, b"pdfbytes").unwrap();

        app.attach_file(&file);

        // stored on the draft AND reflected in the composer's list
        assert_eq!(app.store.outbound_attachments(&draft_id).unwrap().len(), 1);
        assert_eq!(app.compose.as_ref().unwrap().attachments.len(), 1);
        assert_eq!(app.compose.as_ref().unwrap().attachments[0].name, "r.pdf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_draft_loads_existing_attachments() {
        let mut app = App::for_test_with_seeded_store();
        app.compose_new();
        let draft_id = app.compose.as_ref().unwrap().draft_id.clone();
        app.store.add_outbound_attachment(&draft_id, "/tmp/x.txt", "x.txt", 3).unwrap();
        app.open_draft(&draft_id); // reopen
        assert_eq!(app.compose.as_ref().unwrap().attachments.len(), 1);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy attaching_a_file open_draft_loads_existing`
Expected: FAIL — `no method named attach_file` / `no field attachments`.

- [ ] **Step 3: Implement**

In `compose.rs`: add `pub attachments: Vec<mailcore::store::OutboundAttachment>` to `Compose`; init `attachments: Vec::new()` in `Compose::new`. In `draw`, add an "Attachments:" row (between the header fields and the body) listing `name (size)` joined by `, `, truncated to width — a no-op visual when the list is empty.

`Ctrl+O`/`Ctrl+R` touch `app.file_picker`/`app.store`, so handle them in `compose::handle_key` at the **`app` level** — in the `ctrl` branch, before delegating to the compose-only key handler (`app.compose.as_mut()` / `handle_compose_key`). `app.file_picker`, `app.store`, and `app.compose` are disjoint fields, so borrowing them together compiles:
- `Ctrl+O` → open the picker: `app.file_picker = Some(crate::ui::filepicker::FilePicker::open(crate::app::downloads_dir()));` then `return`.
- `Ctrl+R` → remove the last attachment:
  ```rust
  if let Some(compose) = app.compose.as_mut() {
      if let Some(att) = compose.attachments.pop() {
          let _ = app.store.remove_outbound_attachment(&compose.draft_id, &att.path);
      }
  }
  return;
  ```
  (The plan/implementer must confirm `Ctrl+O` and `Ctrl+R` are free in the compose Ctrl handling — the existing bound ones are `Ctrl+B/I/U/L` (Body), `Ctrl+Enter` (send), `Ctrl+D` (discard).)

In `app.rs`:
- `open_draft`: before building `Compose`, `let attachments = self.store.outbound_attachments(&row.id).unwrap_or_default();` and set `attachments` in the `Compose { … }` literal.
- Add:
  ```rust
  /// Enter in the file picker: on a file, attach it and close the picker; on a
  /// directory, the picker navigated (stays open).
  pub fn file_picker_enter(&mut self) {
      let Some(picker) = self.file_picker.as_mut() else { return; };
      if let Some(path) = picker.enter() {
          self.file_picker = None;
          self.attach_file(&path);
      }
  }

  /// Records `path` as an attachment on the open draft (store + the composer's
  /// in-memory list). A no-op if no composer is open.
  pub fn attach_file(&mut self, path: &std::path::Path) {
      let Some(compose) = self.compose.as_mut() else { return; };
      let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("attachment").to_string();
      let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
      let path_str = path.to_string_lossy().to_string();
      let draft_id = compose.draft_id.clone();
      let _ = self.store.add_outbound_attachment(&draft_id, &path_str, &name, size);
      if let Some(compose) = self.compose.as_mut() {
          compose.attachments = self.store.outbound_attachments(&draft_id).unwrap_or_default();
      }
  }
  ```
  (Replace the Task 7 stub `file_picker_enter`.)
- In `apply_compose_action`'s Discard arm, clear the pending attachments: `let _ = self.store.clear_outbound_attachments(&draft_id);` (use whatever local holds the compose's `draft_id` in that arm).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy attaching_a_file open_draft_loads_existing` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green (existing compose tests unaffected — `attachments` defaults empty, the row renders nothing when empty).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/compose.rs lookxy/src/app.rs
git commit -m "lookxy: attach/remove files in compose + wire the file picker"
```

---

## Notes for the implementer

- **Match existing signatures/helpers.** Tasks 3–5 say "adapt to the existing X" (FakeServer/Route/captured-body access, `GraphClient::new`, the send-draft test's routes, `editor.text()`). Read the neighbouring code and match it exactly; keep the assertions.
- **Existing behavior stays green.** Attachments/signature are additive: an empty signature seeds no body, an empty attachment list renders nothing and skips the upload loop, and the new Ctrl chords must not shadow `Ctrl+B/I/U/L`/`Ctrl+Enter`/`Ctrl+D`.
- **Upload-at-send is load-bearing.** Never upload on `SaveDraft`. On any send-path failure the attachments must remain (not cleared) so a retry re-uploads to a fresh draft/session cleanly.
- **Borrows in `app.rs`:** `self.compose.as_mut()` and `self.store` are disjoint fields, so a compose borrow plus a store call compiles — but read the compose field(s) you need first / clone the `draft_id` before the store call, as `attach_file` shows.
