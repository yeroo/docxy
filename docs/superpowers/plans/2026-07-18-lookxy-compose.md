# lookxy v2 — Compose / reply / forward Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let lookxy write mail — a rich-text compose view (bold/italic/underline/lists/links), reply/reply-all/forward with the original quoted, draft persistence, and send — all on lookxy v1's optimistic-store + outbox rails.

**Architecture:** Extract docxy's editable model + edit engine into a new shared `editcore` crate; lookxy's composer edits an `editcore::RichText` buffer, serialized to email HTML in `mailcore::compose_html`. Reply/forward use Graph's `createReply`/`createForward` (return a pre-quoted draft) loaded into the editor via lookxy's existing `htmlrender`. Drafts and sends ride the existing SQLite store + `SyncCommand`/outbox model.

**Tech Stack:** Rust (edition 2024), ratatui/crossterm, ureq+rustls, rusqlite, the existing `mailcore`/`docxcore` crates. Hand-rolled HTML serialize (no new deps).

## Global Constraints

- Edition 2024; MSRV 1.88 (`edition.workspace = true`, `rust-version.workspace = true`). `[lints] workspace = true` in every crate.
- `cargo clippy --all-targets -D warnings` and `cargo fmt --all --check` must stay green workspace-wide (CI gates both). Inline `#[cfg(test)]` module tests.
- **Build/test on this machine** only via the wrapper `bash <scratchpad>/lcargo.sh <args>` with the Bash tool's `dangerouslyDisableSandbox: true` (the `.cargo/bin` shims are broken). Plain `cargo` fails.
- No new third-party deps. `editcore` is pure `std`, no TUI. `compose_html` is pure `std` in `mailcore`.
- **docxy's existing test suite MUST stay green** after the `editcore` refactor — this is a non-negotiable acceptance criterion of Tasks 1–3.
- Secrets (tokens) never logged. Sends/drafts go through `SyncCommand` + the outbox — the UI never blocks on the network; no direct outbox `enqueue_op` from the TUI (the engine enqueues on receiving the command).
- New workspace member: `editcore` (added to root `Cargo.toml` members). `lookxy` gains `editcore` + `docxcore` deps as needed; commit `Cargo.lock`.
- Graph base `https://graph.microsoft.com/v1.0`; all Graph calls go through the engine's existing `with_auth` refresh/throttle wrapper.

---

### Task 1: `editcore` crate — RichText model + cursor/selection

**Files:**
- Create: `editcore/Cargo.toml`, `editcore/src/lib.rs`, `editcore/src/model.rs`, `editcore/src/cursor.rs`
- Modify: `Cargo.toml` (root — add `editcore` to members)

**Interfaces:**
- Produces:
  - `enum Block { Paragraph(Vec<Run>), ListItem { ordered: bool, level: u8, runs: Vec<Run> } }`
  - `struct Run { pub text: String, pub bold: bool, pub italic: bool, pub underline: bool, pub link: Option<String> }`
  - `struct RichText { pub blocks: Vec<Block> }` with `fn new() -> RichText` (one empty paragraph), `fn plain(&self) -> String` (flatten to text), `fn is_empty(&self) -> bool`.
  - `struct Pos { pub block: usize, pub run: usize, pub offset: usize }` and `struct Selection { pub anchor: Pos, pub caret: Pos }` with `fn is_collapsed(&self) -> bool`, `fn ordered(&self) -> (Pos, Pos)`.
  - `fn Run::plain(text: &str) -> Run`.

- [ ] **Step 1: Scaffold the crate + add to workspace.** Write `editcore/Cargo.toml` (mirror `opccore`'s: workspace-inherited fields, `[lints] workspace = true`, no deps), add `"editcore"` to root `members`, `editcore/src/lib.rs` with `pub mod model; pub mod cursor;`.

- [ ] **Step 2: Write failing tests** (`model.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_richtext_has_one_empty_paragraph() {
        let rt = RichText::new();
        assert_eq!(rt.blocks.len(), 1);
        assert!(rt.is_empty());
        assert_eq!(rt.plain(), "");
    }
    #[test]
    fn plain_flattens_runs_and_paragraphs() {
        let rt = RichText { blocks: vec![
            Block::Paragraph(vec![Run::plain("Hello "), Run{ text:"world".into(), bold:true, ..Run::plain("")}]),
            Block::Paragraph(vec![Run::plain("Next")]),
        ]};
        assert_eq!(rt.plain(), "Hello world\nNext");
    }
}
```

- [ ] **Step 3: Run, verify fail.** `bash <wrapper> test -p editcore model::`. Expected: FAIL.
- [ ] **Step 4: Implement `model.rs`** — the structs above; `plain()` joins runs then blocks with `\n`; `is_empty()` true when the only block is an empty paragraph. `cursor.rs` — `Pos`/`Selection` with `ordered()` returning (min,max) by (block,run,offset) tuple compare.
- [ ] **Step 5: Run, verify pass** + `bash <wrapper> clippy -p editcore --all-targets -- -D warnings` + `fmt`.
- [ ] **Step 6: Commit** `editcore/ Cargo.toml`: `editcore: RichText model + cursor/selection`.

---

### Task 2: `editcore` edit operations + undo/redo

**Files:**
- Create: `editcore/src/ops.rs`, `editcore/src/history.rs`
- Modify: `editcore/src/lib.rs` (add `pub mod ops; pub mod history;`)

**Interfaces:**
- Consumes: Task 1 types.
- Produces:
  - `struct Editor { pub text: RichText, pub sel: Selection, history: History }` with `fn new() -> Editor`, `fn from(text: RichText) -> Editor`.
  - Ops (each records an undoable step and updates `sel`): `insert_text(&mut self, s: &str)`, `delete_backward(&mut self)`, `delete_selection(&mut self)`, `split_paragraph(&mut self)`, `toggle_bold`/`toggle_italic`/`toggle_underline(&mut self)` (over the selection, or the pending style at a collapsed caret), `make_link(&mut self, url: &str)`, `list_toggle(&mut self, ordered: bool)`, `indent(&mut self)`, `outdent(&mut self)`.
  - `undo(&mut self) -> bool`, `redo(&mut self) -> bool`.
  - Cursor movement: `move_left`/`right`/`up`/`down`/`home`/`end(&mut self, extend: bool)`.

- [ ] **Step 1: Write failing tests** covering the load-bearing behaviors:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn insert_then_undo_redo() {
        let mut e = Editor::new();
        e.insert_text("hello");
        assert_eq!(e.text.plain(), "hello");
        e.undo();
        assert_eq!(e.text.plain(), "");
        e.redo();
        assert_eq!(e.text.plain(), "hello");
    }
    #[test]
    fn split_paragraph_creates_two_blocks() {
        let mut e = Editor::new();
        e.insert_text("ab");
        e.sel = collapsed(0,0,1); // between a and b — helper builds a Selection
        e.split_paragraph();
        assert_eq!(e.text.blocks.len(), 2);
        assert_eq!(e.text.plain(), "a\nb");
    }
    #[test]
    fn toggle_bold_over_selection_sets_runs_bold() {
        let mut e = Editor::from(RichText{blocks:vec![Block::Paragraph(vec![Run::plain("abcd")])]});
        e.sel = range(0,0,1, 0,0,3); // select "bc"
        e.toggle_bold();
        // "b","c" now bold; "a","d" not — assert via run inspection
        let runs = match &e.text.blocks[0] { Block::Paragraph(r) => r, _ => panic!() };
        let bolded: String = runs.iter().filter(|r| r.bold).map(|r| r.text.clone()).collect();
        assert_eq!(bolded, "bc");
    }
    #[test]
    fn delete_backward_across_paragraph_merges() {
        let mut e = Editor::from(RichText{blocks:vec![
            Block::Paragraph(vec![Run::plain("a")]), Block::Paragraph(vec![Run::plain("b")])]});
        e.sel = collapsed(1,0,0); // start of 2nd paragraph
        e.delete_backward();
        assert_eq!(e.text.plain(), "ab");
        assert_eq!(e.text.blocks.len(), 1);
    }
    #[test]
    fn empty_editor_delete_backward_is_noop() {
        let mut e = Editor::new();
        e.delete_backward(); // must not panic
        assert!(e.text.is_empty());
    }
}
```

(Provide the `collapsed`/`range`/`row` test helpers in the test module — build `Selection`s directly.)

- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p editcore ops::`. Expected: FAIL.
- [ ] **Step 3: Implement `ops.rs` + `history.rs`.** Runs are split/merged as needed; `toggle_*` splits boundary runs so only the selection changes; deletes clamp and merge blocks; every op pushes an inverse-capable snapshot (simplest correct approach: `History` stores `(RichText, Selection)` snapshots before each op — small buffers, email-sized). Cursor moves clamp to valid `Pos`. **No op may panic on an empty buffer or boundary position** — the tests pin this.
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `editcore: edit ops (insert/delete/split/style/list) + undo/redo`.

---

### Task 3: Refactor docxy's editor onto `editcore`

**Files:**
- Modify: `docxy/src/editor/*` (build on `editcore` where the paragraph/run/list subset overlaps), `docxy/Cargo.toml` (add `editcore` dep)
- Reference: `docxy/src/editor/ops.rs`, `cursor.rs`, `history.rs`

**Interfaces:**
- Consumes: `editcore` (Task 1–2).
- Produces: docxy's editing behavior unchanged; its OOXML-specific editing (tables, styles, page view) stays in docxy, but paragraph/run text editing + cursor + undo route through `editcore` where they map cleanly.

**Rationale/risk:** This is the "reuse, don't restructure behavior" task. Do the MINIMAL adaptation that makes docxy consume `editcore` for the overlapping subset. If a clean mapping is not achievable without changing docxy's behavior, STOP and report DONE_WITH_CONCERNS describing the mismatch — do not force it.

- [ ] **Step 1: Map the overlap.** Read `docxy/src/editor/` and identify which ops are the paragraph/run/undo subset editcore now covers. Write a short note in the report.
- [ ] **Step 2: Adapt.** Route the overlapping ops through `editcore` (adapter converting docxy's `Block`/run representation to `editcore::RichText` for those ops, or have docxy's paragraph edits delegate). Keep docxy's public editor API stable.
- [ ] **Step 3: Run docxy's FULL suite.** `bash <wrapper> test -p docxy` — **must be green** (0 failures). `bash <wrapper> clippy -p docxy --all-targets -- -D warnings` + fmt.
- [ ] **Step 4: If any docxy test regresses**, revert the risky part and report the specific incompatibility (DONE_WITH_CONCERNS) rather than weakening docxy's tests.
- [ ] **Step 5: Commit** `docxy: build the paragraph editor on the shared editcore crate`.

---

### Task 4: `mailcore::compose_html` — RichText ↔ email HTML

**Files:**
- Create: `mailcore/src/compose_html.rs`
- Modify: `mailcore/src/lib.rs` (`pub mod compose_html;`), `mailcore/Cargo.toml` (add `editcore` dep)

**Interfaces:**
- Consumes: `editcore::{RichText, Block, Run}`, `crate::htmlrender` (reverse parse).
- Produces:
  - `fn to_html(rt: &RichText) -> String` — email-safe HTML: `<p>` per paragraph, `<b>/<i>/<u>` nested for emphasis, `<a href="…">` for links, `<ul>/<ol>` grouping consecutive `ListItem`s by `ordered`, `<li>` per item; entity-escape text.
  - `fn to_text(rt: &RichText) -> String` — plain-text alternative (`rt.plain()`).
  - `fn from_html(html: &str) -> RichText` — parse an existing message/draft body HTML into an editable `RichText` by walking `htmlrender`'s tokenizer output (map bold/italic/underline/link/list/paragraph; unknown → text). Used to load reply/forward quoted drafts into the editor.

- [ ] **Step 1: Write failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use editcore::{RichText, Block, Run};
    #[test]
    fn to_html_emphasis_and_paragraphs() {
        let rt = RichText{blocks:vec![
            Block::Paragraph(vec![Run::plain("Hi "), Run{text:"there".into(),bold:true,..Run::plain("")}]),
            Block::Paragraph(vec![Run::plain("a < b & c")]),
        ]};
        let h = to_html(&rt);
        assert!(h.contains("<p>Hi <b>there</b></p>"));
        assert!(h.contains("a &lt; b &amp; c"));
    }
    #[test]
    fn to_html_lists_group_consecutive_items() {
        let rt = RichText{blocks:vec![
            Block::ListItem{ordered:false,level:0,runs:vec![Run::plain("one")]},
            Block::ListItem{ordered:false,level:0,runs:vec![Run::plain("two")]},
        ]};
        let h = to_html(&rt);
        assert!(h.contains("<ul><li>one</li><li>two</li></ul>"));
    }
    #[test]
    fn from_html_roundtrips_basic() {
        let rt = from_html("<p>Hello <b>bold</b></p>");
        assert_eq!(rt.plain(), "Hello bold");
        // the bold run survived:
        let runs = match &rt.blocks[0] { Block::Paragraph(r)=>r, _=>panic!() };
        assert!(runs.iter().any(|r| r.bold && r.text.contains("bold")));
    }
    #[test]
    fn from_html_does_not_panic_on_gnarly_input() {
        let _ = from_html("<p><b>unclosed <i> &amp; <a href=x>link</p><<>>");
    }
}
```

- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore compose_html::`. Expected: FAIL.
- [ ] **Step 3: Implement.** `to_html` walks blocks, opens/closes emphasis tags per run (coalesce adjacent same-style), groups consecutive `ListItem`s of the same `ordered` into one `<ul>`/`<ol>`, escapes with the same helper style as `htmlrender`. `from_html` adapts `htmlrender`'s existing tokenizer (Task-14 v1 code) to build `RichText` instead of styled lines — reuse the tokenizer, map its style stack to `Run` flags. Must not panic on malformed input (mirror htmlrender's leniency).
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: compose_html — RichText<->email HTML`.

---

### Task 5: Graph client — reply / forward / draft / send

**Files:**
- Modify: `mailcore/src/graph/client.rs`

**Interfaces:**
- Consumes: existing `GraphClient`, `GraphError`, `with_auth` pattern, `crate::json`.
- Produces (all `Result<_, GraphError>`, through the request funnel that sets auth + maps errors):
  - `fn create_reply(&self, id: &str, all: bool) -> Result<Message, GraphError>` — POST `/me/messages/{id}/createReply` (or `createReplyAll`); returns the created draft (parsed via `Message::from_json`).
  - `fn create_forward(&self, id: &str) -> Result<Message, GraphError>` — POST `.../createForward`.
  - `fn create_draft(&self, body_html: &str, subject: &str, to: &[Recipient], cc: &[Recipient]) -> Result<Message, GraphError>` — POST `/me/messages` with `isDraft` implied; returns the draft (with its Graph id).
  - `fn update_draft(&self, id: &str, body_html: &str, subject: &str, to: &[Recipient], cc: &[Recipient]) -> Result<(), GraphError>` — PATCH `/me/messages/{id}`.
  - `fn send_draft(&self, id: &str) -> Result<(), GraphError>` — POST `/me/messages/{id}/send`.
  - Path ids percent-encoded via the existing `encode_path_segment`.

- [ ] **Step 1: Write failing tests** against the fake server (mirror v1 client tests): `create_reply` parses the returned draft; `create_draft` sends a JSON body containing `"subject"`, `"body":{"contentType":"HTML"…}`, `"toRecipients"`; `send_draft` issues POST to `.../send` and maps 202/200 to `Ok`; a 401 maps to `Unauthorized`.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore graph::client::`. Expected: FAIL.
- [ ] **Step 3: Implement** using `Value::Object(...).to_string()` for bodies (no hand-formatted JSON), the existing error mapping, and `encode_path_segment` for ids.
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: Graph reply/forward/draft/send client methods`.

---

### Task 6: Store — draft + local-id support

**Files:**
- Modify: `mailcore/src/store/mod.rs`, `mailcore/src/store/schema.rs`, `mailcore/src/graph/model.rs` (add `is_draft` to `Message`)

**Interfaces:**
- Produces:
  - `messages.is_draft INTEGER` column (default 0); `Message.is_draft: bool` parsed from Graph `isDraft`.
  - `fn create_local_draft(&self, subject: &str, to: &str, cc: &str, body_html: &str) -> Result<String, StoreError>` — inserts a message with a `local:<uuid>` id into the Drafts folder, returns the id. (uuid: a small hand-rolled random hex via the existing OS-random helper used by pkce; NOT a new dep.)
  - `fn update_draft_fields(&self, id: &str, subject: &str, to: &str, cc: &str, body_html: &str)`.
  - `fn reconcile_id(&self, local_id: &str, graph_id: &str) -> Result<(), StoreError>` — rewrites the message + body rows from the `local:` id to the Graph id (used after `create_draft` returns).
  - `fn draft(&self, id: &str) -> Result<Option<(MessageRow, Body)>, StoreError>` — load a draft for editing.

- [ ] **Step 1: Write failing tests** (temp/in-memory DB): create a local draft → it appears in the Drafts folder query with a `local:` id and `is_draft=1`; update fields → body/subject change; `reconcile_id` moves it to the Graph id (old id gone, new id present with the same body).
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore store::`. Expected: FAIL.
- [ ] **Step 3: Implement.** Schema `IF NOT EXISTS` add for `is_draft` (guarded `ALTER TABLE`/recreate — since schema is created fresh via `CREATE TABLE IF NOT EXISTS`, add the column to the create SQL AND a defensive `ALTER TABLE messages ADD COLUMN is_draft ...` wrapped in a `let _ =` for existing DBs). Reconcile in a transaction (update messages + bodies + fts). The Drafts folder id: resolve by `well_known_name='drafts'`; if not yet synced, store under a sentinel and let the folder resolve on next sync.
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: store draft rows + local-id reconciliation`.

---

### Task 7: Sync — SaveDraft / SendDraft outbox ops + engine

**Files:**
- Modify: `mailcore/src/store/mod.rs` (OutboxOp variants), `mailcore/src/sync/outbox.rs` (apply), `mailcore/src/sync/engine.rs` (commands/events + handling)

**Interfaces:**
- Produces:
  - `OutboxOp::SaveDraft{ id }`, `OutboxOp::SendDraft{ id }` (JSON (de)serialize like existing ops).
  - `apply_op` handles them: SaveDraft → if `id` starts `local:`, `create_draft` then `reconcile_id`; else `update_draft`. SendDraft → ensure the draft exists on Graph (SaveDraft first if local), then `send_draft(graph_id)`.
  - `SyncCommand::{ SaveDraft{id}, SendDraft{id}, ComposeReply{ id, all }, ComposeForward{ id } }`; `SyncEvent::{ DraftReady{ id }, Sent{ id } }`.
  - Engine: `ComposeReply`/`ComposeForward` call `create_reply`/`create_forward`, store the returned draft, emit `DraftReady{id}` (the UI opens the editor on it). `SaveDraft`/`SendDraft` write optimistically (Send moves the message to Sent locally, marks sent) + enqueue the op; drain uses the same quarantine/retry policy as triage; failures → `SyncEvent::Error`.

- [ ] **Step 1: Write failing tests:** outbox round-trip for the two new ops; an engine integration test (fake server + temp DB): send `ComposeReply` → a draft is stored + `DraftReady` emitted; `SendDraft` on a local draft → draft created on Graph (id reconciled) then `.../send` called, `Sent` emitted, message in Sent locally. Use `recv_timeout`.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore sync::`. Expected: FAIL.
- [ ] **Step 3: Implement** mirroring the v1 mutation/outbox pattern; `apply_op` reuses the Task-5 client methods; local→graph reconciliation happens inside SaveDraft apply. Never double-enqueue.
- [ ] **Step 4: Run, verify pass** (full `bash <wrapper> test -p mailcore`) + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: SaveDraft/SendDraft outbox ops + reply/forward engine commands`.

---

### Task 8: lookxy compose TUI — fields + body editor + action bar

**Files:**
- Create: `lookxy/src/ui/compose.rs`
- Modify: `lookxy/src/app.rs` (compose state), `lookxy/src/ui/mod.rs` (route to compose view), `lookxy/Cargo.toml` (add `editcore`, `mailcore::compose_html` already available)

**Interfaces:**
- Produces:
  - `struct Compose { pub to: String, pub cc: String, pub subject: String, pub editor: editcore::Editor, pub focus: ComposeField, pub draft_id: String }` with `enum ComposeField { To, Cc, Subject, Body }`.
  - `fn draw_compose(f: &mut Frame, app: &App)` — full-screen: To/Cc/Subject single-line fields, the body editor (map `editcore` runs → ratatui `Span` styles; render caret/selection), and an action-bar footer (Send Ctrl-Enter · Save Esc · Discard Ctrl-D · Ctrl-B/I/U · list).
  - Key handling: Tab cycles fields; in Body, keys drive `editor` ops (printable→insert, Backspace→delete_backward, Enter→split_paragraph, arrows→move, Ctrl-B/I/U→toggle_*, Ctrl-L→list_toggle); Ctrl-Enter→send, Esc→save draft + close, Ctrl-D→discard.

- [ ] **Step 1: Write failing test** (`TestBackend`): build an `App` in compose mode with a seeded draft, `draw_compose` renders To/Subject and the body text without panic; typing a char in Body appends to `editor.text.plain()`; Ctrl-B toggles bold on a selection. Also: `App::for_test` with an empty compose renders (no panic) and Tab cycles focus To→Cc→Subject→Body.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p lookxy ui::compose`. Expected: FAIL.
- [ ] **Step 3: Implement** the view + key handling. Body render maps runs to `Span` with `Modifier::{BOLD,ITALIC,UNDERLINED}`; lists indented; a block cursor at the caret. Empty-buffer and boundary key presses must not panic (editcore guarantees the ops; the UI must guard indexing).
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `lookxy: compose view (fields + editcore body editor + action bar)`.

---

### Task 9: Entry points, drafts resume, send/save wiring, docs

**Files:**
- Modify: `lookxy/src/ui/mod.rs`, `lookxy/src/app.rs`, `lookxy/src/main.rs`, `LOOKXY.md`

**Interfaces:**
- Produces: keys from the message list/reading pane — `c` new (empty compose; `create_local_draft`), `r`/`R` reply/reply-all (send `SyncCommand::ComposeReply{id,all}`, open editor on `DraftReady`), `f` forward (`ComposeForward`). Selecting a message in the **Drafts** folder opens it in the composer (`store.draft(id)` → `compose_html::from_html` → `editcore::Editor`). On Send: serialize `editor.text` via `compose_html::to_html`, update the draft (`update_draft_fields`), send `SyncCommand::SendDraft{id}`; close the composer optimistically. On Save (Esc): `update_draft_fields` + `SyncCommand::SaveDraft{id}`.

- [ ] **Step 1: Write failing test:** in the message list, `on_key_char('c')` enters compose mode with a fresh local draft; opening a Drafts-folder message enters compose loaded with its body; pressing send sends `SyncCommand::SendDraft` (inspect via the test command channel) and exits compose.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p lookxy`. Expected: FAIL.
- [ ] **Step 3: Implement** the entry points + send/save wiring; on `SyncEvent::DraftReady{id}` open the composer loaded from the store.
- [ ] **Step 4: Docs.** Add a "Writing mail" section to `LOOKXY.md` (compose/reply/forward/drafts keys, rich-text formatting keys, that sends ride the outbox and appear in Sent). 
- [ ] **Step 5: Full workspace green.** `bash <wrapper> test --workspace`, `clippy --workspace --all-targets -- -D warnings`, `fmt --all --check` — all clean (incl. docxy).
- [ ] **Step 6: Commit** `lookxy: compose entry points, drafts resume, send/save wiring, docs`.

---

## Self-Review Notes

- **Spec coverage:** §2 editcore → Tasks 1–3; §3 compose_html → Task 4; §4 reply/forward/draft/send Graph → Task 5; §5 drafts+outbox → Tasks 6–7; §6 store → Task 6; §7 TUI → Tasks 8–9; §8 error handling → Tasks 7 (quarantine/Error), 8/9 (validation, offline); §9 testing → every task TDD + docxy suite gate (Task 3); §10 build order matches.
- **docxy risk isolated:** Task 3 is the only task touching docxy and its acceptance criterion is "docxy suite green or report the mismatch" — it cannot silently weaken docxy.
- **Type consistency:** `RichText`/`Block`/`Run`/`Editor`/`Selection`/`Pos` defined in Tasks 1–2 and reused unchanged; `OutboxOp::{SaveDraft,SendDraft}` and `SyncCommand::{SaveDraft,SendDraft,ComposeReply,ComposeForward}` / `SyncEvent::{DraftReady,Sent}` consistent across Tasks 7–9.
- **Deferred (spec non-goals):** compose-time attachments, signatures, recipient autocomplete — not in any task, by design.
