# lookxy Non-File Attachment Handling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `itemAttachment` (nested email/event) download as `.eml`/`.ics` and `referenceAttachment` (cloud link) open in the browser from the attachments popup, instead of failing with "no contentBytes".

**Architecture:** `AttachmentMeta` gains a `kind` (from `@odata.type`) and `source_url`; a new Graph `/$value` raw fetch + a `SaveItemAttachment` command let the engine download a nested item and pick its extension by sniffing the bytes (`BEGIN:VCALENDAR` → `.ics`, else `.eml`); the app's save keypath branches by kind — file unchanged, item → `SaveItemAttachment` with an extension-less base path (open-intent matched by stem so the engine can choose the extension), reference → open the link directly.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), ratatui 0.29, hand-rolled `mailcore::json` (no serde), `ureq` HTTP.

## Global Constraints

- **Build/test ONLY through the wrapper** (bare `cargo` fails with os error 448). Every command is `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **MSRV 1.88, edition 2024.** clippy `-D warnings` clean on ubuntu/macos/windows. Run `bash "$LCARGO" fmt` before every commit. No new dependencies.
- **Locked product decisions:** itemAttachment downloads (Enter saves, `o` saves+opens) as `.ics` when the `/$value` bytes start with `BEGIN:VCALENDAR` (after optional leading whitespace/BOM), else `.eml`. referenceAttachment: BOTH Enter and `o` open `source_url` with the OS handler; a "Opened link: {name}" notice; no download. fileAttachment behavior is UNCHANGED.
- **Additive struct change:** `AttachmentMeta` gains `kind` + `source_url`; every `AttachmentMeta { … }` literal in the workspace must be updated to compile (add `kind: mailcore::graph::model::AttachmentKind::File, source_url: None`). "Workspace compiles" is part of Task 1's done bar.
- **Security posture unchanged:** filenames still go through `sanitize_filename` into the Downloads dir; the OS-open still uses the `rundll32`/`open`/`xdg-open` seam (never `cmd /c start`).

---

### Task 1: mailcore — `kind` + `source_url` on attachment metadata + store columns

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`AttachmentMeta`, new `AttachmentKind`, `from_json`)
- Modify: `mailcore/src/store/schema.rs` (attachments table)
- Modify: `mailcore/src/store/mod.rs` (idempotent migration, `put_attachments`, `attachments`)
- Modify: every `AttachmentMeta { … }` literal in the workspace

**Interfaces:**
- Produces: `pub enum AttachmentKind { File, Item, Reference }` with `fn as_db_str(&self) -> &'static str` and `fn from_db_str(s: &str) -> AttachmentKind`; `AttachmentMeta.kind: AttachmentKind`; `AttachmentMeta.source_url: Option<String>`.

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/graph/model.rs` tests:
```rust
    #[test]
    fn attachment_meta_parses_item_kind() {
        let v = crate::json::parse(
            r#"{"@odata.type":"#microsoft.graph.itemAttachment","id":"a1","name":"Fwd: hi","contentType":"","size":0,"isInline":false}"#
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.kind, AttachmentKind::Item);
        assert_eq!(a.source_url, None);
    }
    #[test]
    fn attachment_meta_parses_reference_kind_with_source_url() {
        let v = crate::json::parse(
            r#"{"@odata.type":"#microsoft.graph.referenceAttachment","id":"a2","name":"Doc","contentType":"","size":0,"isInline":false,"sourceUrl":"https://contoso.sharepoint.com/x"}"#
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.kind, AttachmentKind::Reference);
        assert_eq!(a.source_url.as_deref(), Some("https://contoso.sharepoint.com/x"));
    }
    #[test]
    fn attachment_meta_file_kind_is_default() {
        let v = crate::json::parse(
            r#"{"@odata.type":"#microsoft.graph.fileAttachment","id":"a3","name":"x.pdf","contentType":"application/pdf","size":5,"isInline":false}"#
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.kind, AttachmentKind::File);
        // an absent @odata.type also defaults to File:
        let v2 = crate::json::parse(r#"{"id":"a4","name":"y","contentType":"","size":0,"isInline":false}"#).unwrap();
        assert_eq!(AttachmentMeta::from_json(&v2).unwrap().kind, AttachmentKind::File);
    }
```
Add to `mailcore/src/store/mod.rs` tests (mirror the existing `attachments_round_trip_content_id` test's store/message harness):
```rust
    #[test]
    fn attachments_round_trip_kind_and_source_url() {
        let store = /* same test-store construction the content_id test uses */;
        /* seed a message "m1" the same way that test does */;
        store.put_attachments("m1", &[AttachmentMeta {
            id: "a1".into(), name: "Doc".into(), content_type: "".into(), size: 0,
            is_inline: false, content_id: None,
            kind: AttachmentKind::Reference, source_url: Some("https://x/y".into()),
        }]).unwrap();
        let got = store.attachments("m1").unwrap();
        assert_eq!(got[0].kind, AttachmentKind::Reference);
        assert_eq!(got[0].source_url.as_deref(), Some("https://x/y"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore attachment_meta_parses kind_and_source_url`
Expected: FAIL — no `AttachmentKind` / `kind` / `source_url`.

- [ ] **Step 3: Implement the model**

In `mailcore/src/graph/model.rs`, add the enum and fields:
```rust
/// Which Graph attachment kind this is (`@odata.type`). Determines what the
/// UI does on save: `File` downloads its `contentBytes`; `Item` downloads its
/// `/$value` MIME (`.eml`/`.ics`); `Reference` opens `source_url`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    File,
    Item,
    Reference,
}

impl AttachmentKind {
    /// Short token stored in the `attachments.kind` column.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            AttachmentKind::File => "file",
            AttachmentKind::Item => "item",
            AttachmentKind::Reference => "reference",
        }
    }
    /// Inverse of `as_db_str`; anything unrecognized reads back as `File`.
    pub fn from_db_str(s: &str) -> AttachmentKind {
        match s {
            "item" => AttachmentKind::Item,
            "reference" => AttachmentKind::Reference,
            _ => AttachmentKind::File,
        }
    }
}
```
Add to `AttachmentMeta` (after `content_id`):
```rust
    pub kind: AttachmentKind,
    /// The cloud link of a `referenceAttachment` (Graph `sourceUrl`); `None`
    /// for other kinds.
    pub source_url: Option<String>,
```
In `from_json`, after `content_id`:
```rust
            kind: match v.get("@odata.type").and_then(Value::as_str) {
                Some("#microsoft.graph.itemAttachment") => AttachmentKind::Item,
                Some("#microsoft.graph.referenceAttachment") => AttachmentKind::Reference,
                _ => AttachmentKind::File,
            },
            source_url: {
                let u = str_field(v, "sourceUrl");
                if u.is_empty() { None } else { Some(u) }
            },
```

- [ ] **Step 4: Implement the store**

In `mailcore/src/store/schema.rs`, add the columns to the `attachments` CREATE TABLE (after `content_id TEXT`):
```sql
    kind         TEXT NOT NULL DEFAULT 'file',
    source_url   TEXT,
```
In `mailcore/src/store/mod.rs`, in `Store::init` next to the `content_id` migration, add two more idempotent migrations:
```rust
        let _ = conn.execute("ALTER TABLE attachments ADD COLUMN kind TEXT NOT NULL DEFAULT 'file'", []);
        let _ = conn.execute("ALTER TABLE attachments ADD COLUMN source_url TEXT", []);
```
Update `put_attachments` INSERT (add `kind, source_url` columns + `?8, ?9` params binding `a.kind.as_db_str()` and `a.source_url`) and `attachments` SELECT (`SELECT id, name, content_type, size, is_inline, content_id, kind, source_url …`, reading `kind: AttachmentKind::from_db_str(&row.get::<_, String>(6)?)` and `source_url: row.get(7)?`). Import `AttachmentKind` alongside `AttachmentMeta`.

- [ ] **Step 5: Fix every `AttachmentMeta` literal**

Run `bash "$LCARGO" build 2>&1 | grep "missing field"` to list them (lookxy `app.rs`/`ui/attachments.rs`/`control.rs`, mailcore tests). Add `kind: AttachmentKind::File, source_url: None` to each (or a real value in the new tests). Import `AttachmentKind` where needed.

- [ ] **Step 6: Run to verify pass + fmt/clippy/commit**

Run: `bash "$LCARGO" test -p mailcore attachment_meta_parses kind_and_source_url` then `bash "$LCARGO" build` (whole workspace).
Expected: PASS; builds.
```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "mailcore: AttachmentKind + source_url on attachment metadata + store columns"
```

---

### Task 2: mailcore — raw `/$value` attachment fetch

**Files:**
- Modify: `mailcore/src/graph/client.rs` (`get_attachment_raw_value`)

**Interfaces:**
- Produces: `pub fn get_attachment_raw_value(&self, message_id: &str, attachment_id: &str) -> Result<Vec<u8>, GraphError>`.

- [ ] **Step 1: Write the failing test**

Mirror `get_attachment_bytes_decodes_base64`'s testserver setup (in `mailcore/src/graph/client.rs` tests), but the route body is RAW bytes and the path ends in `/$value`:
```rust
    #[test]
    fn get_attachment_raw_value_returns_body_bytes() {
        let server = /* testserver with this route (copy the sibling test's builder) */ Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1/attachments/A1/$value".into(),
            status: 200, headers: vec![],
            body: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into(),
        };
        let c = /* client over that server, same as the sibling test */;
        let bytes = c.get_attachment_raw_value("M1", "A1").unwrap();
        assert_eq!(bytes, b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n");
    }
```
Read `get_attachment_bytes_decodes_base64` first and copy its exact server/client construction (the `$value` suffix is the only path difference).

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore get_attachment_raw_value`
Expected: FAIL — method doesn't exist.

- [ ] **Step 3: Implement**

Add to `mailcore/src/graph/client.rs` (next to `get_attachment_bytes`):
```rust
    /// GET `/me/messages/{id}/attachments/{aid}/$value` — the attachment's
    /// raw body bytes (an `itemAttachment`'s MIME), NOT JSON. Used to download
    /// a nested item as `.eml`/`.ics`; `get_attachment_bytes` handles a
    /// `fileAttachment`'s base64 `contentBytes` instead.
    pub fn get_attachment_raw_value(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GraphError> {
        use std::io::Read;
        let message_id = encode_path_segment(message_id);
        let attachment_id = encode_path_segment(attachment_id);
        let path = format!("/me/messages/{message_id}/attachments/{attachment_id}/$value");
        let resp = self.send(Method::Get, &path, None, &[])?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| GraphError::Transport(e.to_string()))?;
        Ok(buf)
    }
```

- [ ] **Step 4: Run to verify pass + fmt/clippy/commit**

Run: `bash "$LCARGO" test -p mailcore get_attachment_raw_value`
Expected: PASS.
```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "mailcore: get_attachment_raw_value — raw /\$value bytes for item attachments"
```

---

### Task 3: mailcore — `SaveItemAttachment` command + engine handler (sniff + write)

**Files:**
- Modify: `mailcore/src/sync/engine.rs` (`SyncCommand`, dispatch arm, `save_item_attachment` handler)

**Interfaces:**
- Consumes: `GraphClient::get_attachment_raw_value` (Task 2), the existing `SyncEvent::AttachmentSaved { path }`.
- Produces: `SyncCommand::SaveItemAttachment { message_id: String, attachment_id: String, dest_base: PathBuf }` — `dest_base` is the Downloads path WITHOUT an extension; the engine appends `.ics`/`.eml`.

- [ ] **Step 1: Write the failing test**

Mirror `save_attachment_writes_bytes_and_emits_saved_path` (same file). Two cases in one test (or two tests): a `/$value` route returning `BEGIN:VCALENDAR…` → the emitted `AttachmentSaved.path` ends in `.ics` and the file exists; a route returning RFC822 text (`"Received: ...\r\n..."`) → ends in `.eml`. Use a `tempfile`-style dest or the same temp-dir approach the sibling test uses (copy it). Sketch:
```rust
    #[test]
    fn save_item_attachment_picks_ics_or_eml_by_content() {
        // ICS case
        let dir = /* temp dir like the sibling test */;
        let (engine, cmd_tx, evt_rx) = /* engine over a testserver whose /$value route returns BEGIN:VCALENDAR */;
        cmd_tx.send(SyncCommand::SaveItemAttachment {
            message_id: "M1".into(), attachment_id: "A1".into(),
            dest_base: dir.path().join("invite"),
        }).unwrap();
        /* drive engine; recv AttachmentSaved */;
        let path = /* the AttachmentSaved path */;
        assert!(path.extension().unwrap() == "ics");
        assert!(path.exists());
        // (repeat with a route returning "Received: x\r\n\r\nbody" → extension "eml")
    }
```
Copy the sibling test's engine/testserver/tempdir construction verbatim; only the route body and the command differ.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore save_item_attachment`
Expected: FAIL — no `SaveItemAttachment` variant.

- [ ] **Step 3: Implement**

Add to `SyncCommand` (near `SaveAttachment`):
```rust
    /// Download a nested `itemAttachment` (`GraphClient::get_attachment_raw_value`)
    /// and write it to `{dest_base}.ics` or `{dest_base}.eml` — the extension
    /// is chosen by sniffing the bytes (`BEGIN:VCALENDAR` → calendar item).
    /// `dest_base` is the Downloads path WITHOUT an extension (the UI can't
    /// know it until the bytes are sniffed). Emits [`SyncEvent::AttachmentSaved`]
    /// with the extended path, the same event `SaveAttachment` uses.
    SaveItemAttachment {
        message_id: String,
        attachment_id: String,
        dest_base: PathBuf,
    },
```
Add the dispatch arm (next to `SaveAttachment`):
```rust
            SyncCommand::SaveItemAttachment { message_id, attachment_id, dest_base } =>
                self.save_item_attachment(&message_id, &attachment_id, dest_base),
```
Add the handler (model it on `save_attachment`):
```rust
    fn save_item_attachment(&mut self, message_id: &str, attachment_id: &str, dest_base: PathBuf) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_attachment_raw_value(message_id, attachment_id)) {
            Ok(bytes) => {
                let ext = if looks_like_icalendar(&bytes) { "ics" } else { "eml" };
                // Append the extension (do NOT use with_extension — dest_base may
                // legitimately contain dots we must keep).
                let mut os = dest_base.into_os_string();
                os.push(".");
                os.push(ext);
                let dest = std::path::PathBuf::from(os);
                if let Some(parent) = dest.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        self.emit(SyncEvent::Error(format!("failed to create {}: {e}", parent.display())));
                        return;
                    }
                }
                match std::fs::write(&dest, &bytes) {
                    Ok(()) => self.emit(SyncEvent::AttachmentSaved { path: dest }),
                    Err(e) => self.emit(SyncEvent::Error(format!(
                        "failed to save attachment to {}: {e}", dest.display()
                    ))),
                }
            }
            Err(e) => self.react(e),
        }
    }
```
Add the sniff helper (module scope in `engine.rs`):
```rust
/// Whether `bytes` begin (after optional BOM/leading whitespace) with an
/// iCalendar header, so a nested item should be saved as `.ics` rather than
/// `.eml`.
fn looks_like_icalendar(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(64)];
    let s = String::from_utf8_lossy(head);
    let s = s.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    s.to_ascii_uppercase().starts_with("BEGIN:VCALENDAR")
}
```

- [ ] **Step 4: Run to verify pass + fmt/clippy/commit**

Run: `bash "$LCARGO" test -p mailcore save_item_attachment` then `bash "$LCARGO" build`.
Expected: PASS; builds. (If lookxy's `on_sync_event` match is exhaustive over `SyncCommand`, no change needed — commands are UI→engine; only new `SyncEvent`s need lookxy arms, and this task adds none.)
```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "mailcore: SaveItemAttachment — download nested item as .ics/.eml by content sniff"
```

---

### Task 4: lookxy — save branching by kind + open-intent-by-stem + popup labels

**Files:**
- Modify: `lookxy/src/app.rs` (`send_save_attachment_command`, `finish_attachment_save`)
- Modify: `lookxy/src/ui/attachments.rs` (`line` per kind)

**Interfaces:**
- Consumes: `AttachmentMeta.kind`/`source_url` (Task 1), `SyncCommand::SaveItemAttachment` (Task 3), the existing `open_with_os_handler`, `downloads_dir`, `sanitize_filename`, `pending_saves`, `attachment_notice`.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/app.rs` tests (mirror the existing attachment-popup tests' seeding — they call `store.put_attachments` then `open_attachments_popup`):
```rust
    #[test]
    fn saving_an_item_attachment_sends_save_item_command_with_extensionless_base() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_attachment(&mut app, AttachmentMeta {
            id: "a1".into(), name: "Invite.ics".into(), content_type: "".into(), size: 0,
            is_inline: false, content_id: None,
            kind: AttachmentKind::Item, source_url: None,
        }); // helper: put_attachments for the highlighted message + open_attachments_popup
        app.save_attachment(); // Enter
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SaveItemAttachment { dest_base, .. }) => {
                // extension stripped so the engine can choose .ics/.eml
                assert_eq!(dest_base.extension(), None);
                assert!(dest_base.file_name().unwrap().to_string_lossy().starts_with("Invite"));
            }
            other => panic!("expected SaveItemAttachment, got {other:?}"),
        }
    }

    #[test]
    fn opening_a_reference_attachment_opens_the_link_and_sends_no_command() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_attachment(&mut app, AttachmentMeta {
            id: "a2".into(), name: "Doc".into(), content_type: "".into(), size: 0,
            is_inline: false, content_id: None,
            kind: AttachmentKind::Reference, source_url: Some("https://x/y".into()),
        });
        let before = app.open_invocations.get();
        app.save_attachment(); // Enter → opens the link
        assert_eq!(app.open_invocations.get(), before + 1); // OS handler invoked
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // no command
        assert!(app.attachments.is_none()); // popup closed
    }

    #[test]
    fn finish_attachment_save_opens_item_file_by_stem() {
        let mut app = App::for_test_with_seeded_store();
        let base = downloads_dir().join("Invite");
        app.pending_saves_insert_for_test(base.clone(), true); // open_after = true
        let before = app.open_invocations.get();
        app.finish_attachment_save(base.with_extension("ics")); // engine chose .ics
        assert_eq!(app.open_invocations.get(), before + 1); // opened via stem match
    }
```
Add tiny test-support as needed: a `seed_one_attachment(app, meta)` helper mirroring the existing popup tests (put the meta for the highlighted message, then `open_attachments_popup`), and — only if the crate has no existing accessor — a `#[cfg(test)] fn pending_saves_insert_for_test(&mut self, p: PathBuf, open: bool)` that inserts into `pending_saves`. If an existing test already reaches `pending_saves`, use that instead of adding an accessor.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy item_attachment reference_attachment by_stem`
Expected: FAIL — item routes to `SaveAttachment`, reference errors/does-nothing, stem lookup missing.

- [ ] **Step 3: Implement the save branch**

Rewrite `send_save_attachment_command` (`lookxy/src/app.rs:1924`) to branch on kind:
```rust
    fn send_save_attachment_command(&mut self, open_after: bool) {
        let Some(popup) = &self.attachments else { return; };
        let Some(att) = popup.items.get(popup.index) else { return; };
        let message_id = popup.message_id.clone();
        let attachment_id = att.id.clone();
        match att.kind {
            mailcore::graph::model::AttachmentKind::File => {
                let dest = downloads_dir().join(sanitize_filename(&att.name));
                self.pending_saves.insert(dest.clone(), open_after);
                let _ = self.sync.cmd_tx.send(SyncCommand::SaveAttachment { message_id, attachment_id, dest });
            }
            mailcore::graph::model::AttachmentKind::Item => {
                // Extension is chosen by the engine (content sniff); register the
                // open-intent by the extension-less base, matched by stem in
                // `finish_attachment_save`.
                let base_name = strip_item_ext(&sanitize_filename(&att.name)).to_string();
                let dest_base = downloads_dir().join(base_name);
                self.pending_saves.insert(dest_base.clone(), open_after);
                let _ = self.sync.cmd_tx.send(SyncCommand::SaveItemAttachment { message_id, attachment_id, dest_base });
            }
            mailcore::graph::model::AttachmentKind::Reference => {
                match att.source_url.clone() {
                    Some(url) => {
                        self.open_with_os_handler(std::path::Path::new(&url));
                        self.attachment_notice = Some(format!("Opened link: {}", att.name));
                        self.attachments = None;
                    }
                    None => {
                        self.attachment_notice = Some("No link for this attachment".to_string());
                    }
                }
            }
        }
    }
```
Add the extension-strip helper (module scope in `app.rs`, near `sanitize_filename`):
```rust
/// Strips a trailing `.eml`/`.ics` (case-insensitive) from an item
/// attachment's sanitized name, so appending the sniffed extension can't
/// double it (`Invite.ics` → base `Invite` → `Invite.ics`, not `Invite.ics.ics`).
/// Other names (incl. ones with internal dots) are returned unchanged.
fn strip_item_ext(name: &str) -> &str {
    for ext in [".eml", ".ics"] {
        if name.len() > ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}
```

- [ ] **Step 4: Implement the stem lookup**

Update `finish_attachment_save` (`lookxy/src/app.rs:1950`) so an item save (whose emitted path carries an engine-chosen extension) finds its open-intent registered under the extension-less base:
```rust
    pub fn finish_attachment_save(&mut self, path: PathBuf) {
        // File saves registered the exact path; item saves registered the
        // extension-less base (the engine appended .ics/.eml), so fall back to
        // the stem. Remove whichever matched.
        let open_after = self
            .pending_saves
            .remove(&path)
            .or_else(|| self.pending_saves.remove(&path.with_extension("")))
            .unwrap_or(false);
        if open_after {
            self.open_with_os_handler(&path);
        }
        self.attachment_notice = Some(format!("Saved: {}", path.display()));
        if self.pending_saves.is_empty() {
            self.attachments = None;
        }
    }
```

- [ ] **Step 5: Popup labels per kind**

In `lookxy/src/ui/attachments.rs`, replace `line` (currently `{name}  ({content_type}, {kb} KB)`) with a per-kind label:
```rust
fn line(a: &AttachmentMeta) -> String {
    match a.kind {
        AttachmentKind::Reference => format!("🔗 {}  (link)", a.name),
        AttachmentKind::Item => format!("✉ {}  (item)", a.name),
        AttachmentKind::File => {
            let kb = a.size as f64 / 1024.0;
            format!("{}  ({}, {kb:.1} KB)", a.name, a.content_type)
        }
    }
}
```
Import `AttachmentKind` in `attachments.rs`. Update the `line`/render tests already in that file (the seeded attachment is a `fileAttachment`, so the existing assertions on `budget.xlsx`/`2.0 KB` still hold — just add `kind: AttachmentKind::File, source_url: None` to the `seed_attachment` literal). Add one assertion that a `Reference` attachment renders `🔗` and `(link)`.

- [ ] **Step 6: Run to verify pass + fmt/clippy/commit**

Run: `bash "$LCARGO" test -p lookxy item_attachment reference_attachment by_stem` then `bash "$LCARGO" test -p lookxy` then whole workspace `bash "$LCARGO" test`.
Expected: PASS; all green (file-attachment save path byte-identical, existing tests unaffected).
```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy --all-targets -- -D warnings
git add -A
git commit -m "lookxy: attachment save branches by kind (item -> .eml/.ics, reference -> open link) + popup labels"
```

---

## Notes for the implementer

- **fileAttachment is untouched.** The `File` arm of `send_save_attachment_command` is byte-identical to today's body; existing file-save tests must stay green.
- **The extension round-trips by construction.** The app registers open-intent under the extension-less base; the engine appends exactly one extension via `OsString::push` (NOT `with_extension`, which would clobber internal dots); `finish_attachment_save` recovers the base with `path.with_extension("")` (strips exactly the one appended extension). `strip_item_ext` only removes a trailing `.eml`/`.ics` so the base never already ends in one.
- **Reference opens a URL through the `&Path` OS-open seam** — `open_with_os_handler` already documents that it safely passes a path OR URL to `rundll32`/`open`/`xdg-open` (no `cmd /c start`), so `Path::new(&url)` is correct and keeps the injection-safe behavior.
- **Security posture unchanged:** Downloads-dir confinement + `sanitize_filename` still gate every written file; reference URLs are opened, never downloaded.
