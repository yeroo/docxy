# Offxy Agent Access Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Claude Code and VS Code Copilot read/edit documents open in the offxy extension through the existing ctlcore agent-control protocol, with a bundled MCP server for zero-setup Copilot access.

**Architecture:** Extract the doc-verb logic from `docxy/src/control.rs` into an editor-generic `docxcore::agent` module; give `docxwasm` a `docx_ctl` export and `gridwasm` a `grid_ctl` export implementing the same verbs the terminal apps expose. The extension hosts a ctlcore-compatible TCP server per open tab (discovery files under the per-app ctl dirs, `<app>-vscode-*` instance names) and bundles a Node MCP stdio server mirroring the `docxy_*`/`xlsxy_*` tools, registered with VS Code's MCP API.

**Tech Stack:** Rust (docxcore/docxwasm/gridwasm), TypeScript + Node `net` (extension), plain Node ESM (MCP server), @vscode/vsce.

**Spec:** `docs/superpowers/specs/2026-07-17-offxy-agent-access-design.md`
**Protocol reference:** `docs/agent-control.md`; architecture map `.superpowers/sdd/agent-control-map.md`.

## Global Constraints

- Version stays **0.4.0** (workspace) / **0.3.0** (extension package.json) — no bumps; release is Boris's call.
- Terminal apps' behavior must not change: `docxy`/`xlsxy`/`yppxy` keep passing their existing tests; `docxy/src/control.rs` may only be *rebased onto the extracted module* (same verbs, same replies).
- New Rust code: `docxcore::agent` is std-only; docxwasm/gridwasm keep their single-dependency rule.
- Wire compatibility is sacred: the TS ctl server speaks EXACTLY ctlcore's protocol (one JSON object per line; `{"token","verb","args","id?"}` → `{"ok":true,"result":…,"id?"}` / `{"ok":false,"error":…}`); discovery files are `{"instance","port","token","pid"}`; the bundled MCP server's tools are schema-identical to `docxy --mcp` / `xlsxy --mcp` (compare against `docxy/src/mcp.rs`, `xlsxy/src/mcp.rs`, `ctlcore/src/mcp.rs`).
- All Rust gates: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, relevant `cargo test -p …`.
- **Windows agent shell quirks:** every cargo/npm command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Packaging via `npx --yes @vscode/vsce@latest package --no-dependencies`.

---

### Task 1: Extract `docxcore::agent` and rebase docxy's control.rs onto it

**Files:**
- Create: `docxcore/src/agent.rs`
- Modify: `docxcore/src/lib.rs` (add `pub mod agent;`)
- Modify: `docxy/src/control.rs`

**Interfaces:**
- Consumes: `docxy/src/control.rs` (read it FULLY first — the verb bodies move; its tests at the bottom define expected behavior), `docxcore::editor::{Editor, Caret, Clip}`, `docxcore::model::Block`, `ctlcore::Json`.
- Produces (Task 2 depends on these exact signatures):
  ```rust
  // docxcore/src/agent.rs — document-verb core shared by the TUI and docxwasm.
  // Wait: docxcore cannot depend on ctlcore's Json (dependency direction).
  // Use a callback-free design: verbs return/accept plain Rust types; hosts
  // do their own JSON. Exact API:
  pub struct BlockInfo { pub index: usize, pub kind: &'static str, pub text: String, pub heading: Option<u8> }
  pub struct Heading { pub index: usize, pub level: u8, pub text: String }
  pub fn outline(doc: &Document) -> Vec<Heading>;
  pub fn read(doc: &Document, start: usize, end: usize) -> Result<Vec<BlockInfo>, String>; // validates bounds
  pub fn find(doc: &Document, query: &str, case_sensitive: bool) -> Vec<(usize, usize, usize, String)>; // (block, start, end, text)
  pub fn block_kind(b: &Block) -> &'static str;
  pub fn require_para(body: &[Block], i: usize) -> Result<(), String>;
  pub fn bounds(start: usize, end: usize, n: usize) -> Result<(), String>;
  // Edit verbs over the bare Editor; the caller does its own finish_edit.
  pub fn replace_range(ed: &mut Editor, start: usize, end: usize, text: &str) -> Result<usize, String>; // -> replaced count
  pub fn insert(ed: &mut Editor, at: usize, text: &str) -> Result<(), String>;
  pub fn append(ed: &mut Editor, text: &str);
  ```
  The exact text-extraction/`heading` logic comes from the current `control.rs` bodies — move it, don't reinvent. If a signature needs a small adjustment to match how `control.rs` actually computes something (e.g. block text via an existing docxcore helper), adjust THIS plan's signature to the code, keep the shape.

- [ ] **Step 1: Read `docxy/src/control.rs` and its tests fully.** The tests (`doc_with`/`app_with` fixtures near the bottom) are the behavior contract.
- [ ] **Step 2: Create `docxcore/src/agent.rs`** by moving the verb bodies: `outline`, the block-reading core of `read`, `find`'s match loop, `block_kind`, `require_para`, `bounds`, and the editor manipulations inside `replace_range`/`insert`/`append` (the `Caret::top`/`move_end`/`paste(Clip::from_text)` sequences, WITHOUT `finish_edit` — that stays host-side). Arg parsing (`range_args`, `parse_range_str`, Json handling) STAYS in `docxy/src/control.rs` — docxcore must not know about Json.
- [ ] **Step 3: Rebase `docxy/src/control.rs`** to call the moved functions; keep `dispatch`, all Json marshalling, `finish_edit`, save/reload/open, and the tests exactly where they are. The tests must pass UNCHANGED (they call `dispatch`, which is unchanged behavior).
- [ ] **Step 4: Add native tests in `agent.rs`** (docxcore has no ctlcore dep, so construct `Document`/`Editor` directly — copy the fixture style from control.rs's `doc_with`): `outline_reports_heading_levels`, `replace_range_is_single_paste` (assert body text after replace + that editor selection is consumed), `insert_at_end_equals_append`, `find_locates_across_blocks`.
- [ ] **Step 5: Verify**
  ```bash
  cargo test -p docxcore -p docxy 2>&1 | grep -E 'test result|FAILED'; echo "exit=$?"
  cargo fmt --all && cargo fmt --all --check && cargo clippy -p docxcore -p docxy --all-targets -- -D warnings
  ```
  Expected: all green (docxy's control tests unchanged and passing).
- [ ] **Step 6: Commit** — `git add docxcore docxy && git commit -m "docxcore: extract the agent doc-verb core from docxy's control surface"`

---

### Task 2: `docx_ctl` in docxwasm

**Files:**
- Modify: `docxwasm/src/bridge.rs`, `docxwasm/src/lib.rs`

**Interfaces:**
- Consumes: Task 1's `docxcore::agent` API; docxwasm's `Session` (read `bridge.rs` first: it owns an `Editor` and an undo mechanism driven by its `dispatch`/cmd strings — find how a `paste` cmd lands on the undo stack and route ctl edits the same way).
- Produces (Task 4's ctlserver depends on this): wasm export `docx_ctl(handle, ptr, len) -> resultPtr`. Request: `{"verb":"doc.outline"|"doc.read"|"doc.find"|"doc.replace-range"|"doc.insert"|"doc.append"|"doc.blocks","args":{…}}` (JSON bytes; no token). Response: the verb result object with `"ok":true`, or `{"ok":false,"error":"…"}`. Verb args/results EXACTLY as `docs/agent-control.md` documents them (same keys, same shapes — the VS Code tab must be indistinguishable from a terminal docxy). `doc.blocks` is a tiny extra: `{}` → `{"total":N,"modified":bool}` — the host's `doc.path` composes it with URI info.
- `Session::ctl(&mut self, request_json: &str) -> String` is the native-testable core; `lib.rs`'s export is thin marshalling like `docx_cmd`.

- [ ] **Step 1: Write failing tests in `docxwasm/src/bridge.rs`** (follow its existing test style; build docs via its existing fixture helper):
  ```rust
  #[test]
  fn ctl_outline_and_read_match_contract() {
      let mut s = Session::open(&sample_docx("Hello world")).expect("open");
      let out = s.ctl(r#"{"verb":"doc.read","args":{}}"#);
      assert!(out.contains("\"ok\":true") && out.contains("Hello world") && out.contains("\"kind\":\"paragraph\""), "{out}");
  }
  #[test]
  fn ctl_replace_range_edits_and_is_one_undo() {
      let mut s = Session::open(&sample_docx("first")).expect("open");
      s.ctl(r#"{"verb":"doc.append","args":{"text":"second"}}"#);
      let out = s.ctl(r#"{"verb":"doc.replace-range","args":{"start":1,"text":"better"}}"#);
      assert!(out.contains("\"replaced\":1"), "{out}");
      // one undo removes the replace, restoring "second"
      s.dispatch("undo");
      let v = s.view_json(None);
      assert!(v.contains("second") && !v.contains("better"), "{v}");
  }
  #[test]
  fn ctl_rejects_unknown_verbs_and_bad_args() {
      let mut s = Session::open(&sample_docx("x")).expect("open");
      assert!(s.ctl(r#"{"verb":"doc.nope","args":{}}"#).contains("\"ok\":false"));
      assert!(s.ctl(r#"{"verb":"doc.replace-range","args":{"start":99,"text":"y"}}"#).contains("out of bounds"));
  }
  ```
  (Adapt `sample_docx` to whatever fixture exists; if `view_json` shows blocks differently, assert equivalently.)
- [ ] **Step 2: RED** — `cargo test -p docxwasm` fails (no `ctl`).
- [ ] **Step 3: Implement.** `Session::ctl`: hand-rolled JSON parse of `verb`+`args` (docxwasm has `json.rs` for WRITING; for parsing, mirror how `dispatch` avoids JSON — but ctl requests are JSON. Check `ctlcore`'s parser (`ctlcore/src/json.rs`) — docxwasm cannot depend on ctlcore, so either (a) copy the minimal parse functions needed into `docxwasm/src/json.rs` with a comment crediting ctlcore, or (b) accept a tab-delimited ctl encoding instead — NO: wire fidelity matters only host-side; the HOST (TS) can parse the JSON and forward a TAB-DELIMITED form. DECISION: keep wasm simple — `docx_ctl` takes the same JSON the wire carries, and docxwasm copies ctlcore's small recursive-descent parser into `json.rs` (it's std-only, ~200 lines, same license/authors). This keeps every layer speaking one format.) Then: match verb → `docxcore::agent` calls → build result JSON with the existing writer; mutating verbs must go through the same code path the interactive `paste` cmd uses so undo records one entry — find that path in `dispatch` and reuse it (do NOT bypass into raw Editor calls if that skips undo bookkeeping; if Session's undo IS the Editor's internal one, direct agent::replace_range is fine — verify by the undo test).
- [ ] **Step 4: `lib.rs` export** `docx_ctl` mirroring `docx_cmd`'s marshalling. GREEN: all docxwasm tests pass.
- [ ] **Step 5: fmt/clippy; wasm32 build** (`cargo build -p docxwasm --target wasm32-unknown-unknown --release`).
- [ ] **Step 6: Commit** — `"docxwasm: docx_ctl — the agent verb surface for webview-hosted documents"`

---

### Task 3: `grid_ctl` in gridwasm

**Files:**
- Modify: `gridwasm/src/bridge.rs`, `gridwasm/src/lib.rs`, `gridwasm/src/json.rs` (parser, same approach as Task 2)

**Interfaces:**
- Consumes: `xlsxy/src/control.rs` (read FULLY — it is the semantic contract), `gridcore::sheet::{parse_cell_name, parse_range_name, cell_name}`, gridwasm `Session` (set/clear/engine/undo already exist).
- Produces: wasm export `grid_ctl(handle, ptr, len) -> resultPtr`, verbs `sheet.list`, `sheet.read {sheet?, range?}`, `cell.get {ref}`, `cell.set {ref, text}`, `range.clear {range}`, `find {query}`, `wb.recalc`, `wb.blocks`-equivalent `wb.info {}` → `{"sheets":N,"active":i,"modified":bool}` (host composes `wb.path`). Args/results schema-identical to xlsxy's control.rs replies (copy its result-object construction: same keys, same value shapes — e.g. whatever `sheet.read` returns there, return here).

- [ ] **Step 1: Write failing tests** (gridwasm test style, `sample_xlsx()` fixture exists):
  ```rust
  #[test]
  fn ctl_sheet_read_and_cell_get() {
      let mut s = Session::open(&sample_xlsx()).expect("open");
      let out = s.ctl(r#"{"verb":"sheet.read","args":{}}"#);
      assert!(out.contains("\"ok\":true") && out.contains("Apple"), "{out}");
      let out = s.ctl(r#"{"verb":"cell.get","args":{"ref":"B4"}}"#);
      assert!(out.contains("SUM(B1:B3)"), "{out}");
  }
  #[test]
  fn ctl_cell_set_recalculates_and_undoes() {
      let mut s = Session::open(&sample_xlsx()).expect("open");
      s.ctl(r#"{"verb":"cell.set","args":{"ref":"B2","text":"10"}}"#);
      let v = s.view_json(None);
      assert!(v.contains("12.5"), "recalc: {v}");
      s.dispatch("undo");
      let v = s.view_json(None);
      assert!(v.contains("3.75"), "one undo restores: {v}");
  }
  #[test]
  fn ctl_invalid_formula_and_bad_ref_error() {
      let mut s = Session::open(&sample_xlsx()).expect("open");
      assert!(s.ctl(r#"{"verb":"cell.set","args":{"ref":"B2","text":"=SUM("}}"#).contains("\"ok\":false"));
      assert!(s.ctl(r#"{"verb":"cell.get","args":{"ref":"NOPE99X"}}"#).contains("\"ok\":false"));
  }
  ```
- [ ] **Step 2: RED.** **Step 3: Implement** — `Session::ctl` reusing the existing `set`/`clear` dispatch paths (undo groups + recalc for free); `sheet.read` renders values via the same `format_with` path `view_json` uses (match xlsxy control.rs's read output shape exactly); `find` over cells like xlsxy's. **Step 4: export + GREEN. Step 5: fmt/clippy + wasm32 build. Step 6: Commit** `"gridwasm: grid_ctl — the agent verb surface for webview-hosted workbooks"`.

---

### Task 4: `ctlserver.ts` — the extension-host ctl server

**Files:**
- Create: `offxy-vscode/src/ctlserver.ts`
- Test: `offxy-vscode/src/ctlserver.test.mjs` (plain-node test runner via `node --test` against the COMPILED bundle is overkill — instead make ctlserver.ts's core logic exportable and test it in Task 5's integration harness; this task's own verification is the unit-shaped harness below)

**Interfaces:**
- Consumes: Node `net`, `crypto`, `fs`, `os`; the wire/discovery contracts from Global Constraints.
- Produces (Task 5 wires it):
  ```ts
  export interface CtlHost {
    // forward a wasm verb into the webview; resolves with the raw response JSON string
    callWasm(requestJson: string): Promise<string>;
    // host verbs
    pathInfo(): Promise<object>;   // doc.path / wb.path composition
    save(): Promise<object>;
    reload(): Promise<object>;
    open(path: string): Promise<object>;
    // fired after any mutating verb so the provider can raise the VS Code edit event
    onMutated(verbLabel: string): void;
  }
  export class CtlServer {
    constructor(app: 'docxy' | 'xlsxy', instanceSuffix: string, host: CtlHost, wasmVerbs: Set<string>, mutatingVerbs: Set<string>);
    start(): Promise<void>;   // listen on 127.0.0.1:0, write discovery file
    dispose(): void;          // close server, delete discovery file, clear refresh timer
    readonly instance: string; // `${app}-vscode-${instanceSuffix}`
  }
  ```
- Behavior: ctl dir = `%APPDATA%\<app>\ctl` (Windows) / `$XDG_CONFIG_HOME/<app>/ctl` (fallback `~/.config`); mkdir -p. Token: `crypto.randomBytes(24).toString('hex')` (match ctlcore's token shape — check `ctlcore/src/lib.rs` and imitate length/charset). Discovery JSON: `{"instance","port","token","pid":process.pid}`. Per connection: line-split buffer; each line → JSON.parse (parse failure → `{"ok":false,"error":"bad json"}`); token check FIRST (mismatch → `{"ok":false,"error":"bad token"}`); echo `id` when present. Verb routing: `doc.save`→host.save() etc. (`doc.path` composes `host.pathInfo()` + a `doc.blocks`/`wb.info` wasm call); wasm verbs → strip token/id, forward `{"verb","args"}` via `host.callWasm`, wrap reply. Requests serialize through a single promise queue per server. Mutating verbs (docx: replace-range/insert/append; xlsx: cell.set/range.clear) call `host.onMutated` AFTER a successful reply. A 30-second timer re-writes the discovery file if missing (survives terminal apps' stale sweeps).

- [ ] **Step 1: Implement `ctlserver.ts` per the above** (complete file; ~200 lines).
- [ ] **Step 2: Unit-shaped harness** (scratchpad, not committed): a Node script that imports the compiled server class (esbuild-bundle `ctlserver.ts` standalone to scratchpad for the test: `npx esbuild src/ctlserver.ts --bundle --format=esm --platform=node --outfile=<scratch>/ctlserver.mjs`), starts it with a FAKE CtlHost (canned responses), then over a real TCP socket: (a) bad token rejected; (b) `doc.read` forwarded and reply wrapped with echoed id; (c) mutating verb triggers onMutated once; (d) discovery file exists with the right shape, and after deleting it the refresh timer restores it (use a shortened timer injected via ctor default param); (e) dispose removes the file and closes the port. Run: `node <scratch>/ctl_harness.mjs` — expected `ALL OK`, exit 0.
- [ ] **Step 3: typecheck** (`npm run typecheck`) — ctlserver.ts compiles under the extension tsconfig.
- [ ] **Step 4: Commit** — `"offxy: ctlcore-compatible control server for extension-hosted documents"`

---

### Task 5: Wire ctl servers into the provider + webview `ctl` messages

**Files:**
- Modify: `offxy-vscode/src/extension.ts`, `offxy-vscode/media/webview.js`, `offxy-vscode/media/grid.js`

**Interfaces:**
- Consumes: Task 4's `CtlServer`/`CtlHost`; Tasks 2–3's `docx_ctl`/`grid_ctl` exports; the provider's `EditorSpec`/`BinaryDocument`/message plumbing.
- Produces: every open docx/xlsx tab is a live ctl instance.

- [ ] **Step 1: webview side.** Both webviews handle a new host message `{type:'ctl', requestId, payload}` → call `ex.docx_ctl`/`ex.grid_ctl` with the payload bytes (same marshalling as their cmd path) → post `{type:'ctlResult', requestId, payload:<response string>}`. After a ctl call whose response is ok AND whose verb was mutating, the webview ALSO repaints: docx → `render()`; grid → `requestView()` (the wasm state changed under the UI). Simplest correct trigger: the host tells the webview whether to repaint — extend the message to `{type:'ctl', requestId, payload, repaint:boolean}` computed from the mutating set.
- [ ] **Step 2: host side.** In `OffxyEditorProvider.resolveCustomEditor`: construct a `CtlServer` per document (app from the spec entry: docxEditor→'docxy', gridEditor→'xlsxy'; suffix = sanitized basename + a per-session sequence). `CtlHost` impl: `callWasm` posts the `ctl` message and resolves on `ctlResult` (requestId map, same pattern as `requestBytes`); `pathInfo` from `document.uri` + the wasm info verb; `save()` → `vscode.commands.executeCommand('workbench.action.files.save')`? NO — save the CUSTOM document: `document.save()` isn't public; use `vscode.workspace.save(document.uri)` (VS Code ≥1.68) and fall back to `saveCustomDocument` via the provider (call our own `saveAs(document, document.uri)` then fire… careful: saving through the provider directly does NOT clear VS Code's dirty state; `vscode.workspace.save(uri)` does the full pipeline. Use `vscode.workspace.save`.) `reload()` → `vscode.commands.executeCommand('workbench.action.files.revert', ...)`? Use the provider's `revertCustomDocument` equivalent: `vscode.workspace` has no revert API for custom docs — implement as: re-read bytes from disk, post fresh `open` to the webview, and fire NO edit event (matches revert semantics; VS Code's dirty flag stays — document this quirk in the code comment and return `{"ok":true,…}` with the path). `open(path)` → `vscode.commands.executeCommand('vscode.openWith', Uri.file(path), spec.viewType)`. `onMutated(label)` → fire the provider's `_onDidChange` edit event with that label (undo/redo route to the webview `do` messages exactly like interactive edits — the wasm undo stack already has the ctl edit as one entry).
- [ ] **Step 3: lifecycle.** `start()` after the webview's first `ready`; `dispose()` on panel dispose and extension deactivate (add a `context.subscriptions` disposable).
- [ ] **Step 4: Integration test (scratchpad harness, the real thing).** Node script: instantiate the REAL wasm (media/docxwasm.wasm), the REAL compiled CtlServer, and a CtlHost whose `callWasm` calls the wasm directly (no VS Code): then over TCP run the full ctl conversation from `docs/agent-control.md`'s example — `doc.read`, `doc.replace-range`, verify the reply shapes byte-match the documented contract (keys present, ok flags). Repeat with gridwasm + `cell.set`/`sheet.read`. Expected `ALL OK` exit 0.
- [ ] **Step 5: typecheck + build + package + install** (`npm run typecheck && npm run build`, vsce package, `code --install-extension … --force`). Manual e2e deferred to Boris: open a docx tab, then from a terminal `docxy --mcp` session ask Claude Code to edit it (`docxy_list` should show `docxy-vscode-…`).
- [ ] **Step 6: Commit** — `"offxy: open tabs advertise on the agent control surface"`

---

### Task 6: Bundled MCP server (`mcp/server.mjs`)

**Files:**
- Create: `offxy-vscode/mcp/server.mjs` (dependency-free Node ESM)
- Modify: `offxy-vscode/.vscodeignore` (ensure `mcp/**` ships in the vsix)

**Interfaces:**
- Consumes: the ctl dirs/discovery/wire contracts; tool schemas from `docxy/src/mcp.rs` + `xlsxy/src/mcp.rs` + the shared scaffolding in `ctlcore/src/mcp.rs` (read all three; the TS server must emit the same `tools/list` JSON: same names, same descriptions' intent, same inputSchema property names/types/required arrays).
- Produces: an MCP stdio server exposing `docxy_list/status/outline/read/find/replace_range/insert/append/save` and `xlsxy_list/status/sheets/read/get/set/clear/find/recalc/save`, discovering instances across BOTH ctl dirs (docxy + xlsxy), routing by the same `target` substring semantics (`resolve_target` behavior in `ctlcore/src/client.rs:149` — ambiguity error lists candidates).

- [ ] **Step 1: Implement.** Line-buffered stdin; newline-delimited JSON-RPC 2.0; handle `initialize` (respond with the same serverInfo/capabilities shape ctlcore's `McpServer::handle` produces — read it), `ping`, `tools/list`, `tools/call`; notifications (no id) get no reply. `tools/call` → resolve target instance (per-app dir by tool prefix) → fresh TCP connection, one line out (`{"token",…,"verb","args"}`), one line back → wrap as MCP text content. Errors → JSON-RPC error or `isError:true` tool result, matching ctlcore's choices.
- [ ] **Step 2: Test harness** (scratchpad): spawn `node mcp/server.mjs` with piped stdio, drive `initialize` → `tools/list` (assert all 19 tool names + that `docxy_replace_range`'s schema has `start` required) → with a live ctl instance from Task 5's harness running, `tools/call` `docxy_read` end-to-end. Expected `ALL OK` exit 0.
- [ ] **Step 3: Cross-check schemas** — run `docxy --mcp` (built binary: `cargo build --release -p docxy` if needed) with the same harness's `tools/list` call; diff the tool JSON against ours (names + inputSchema keys must match; descriptions may differ in wording). Paste the diff summary in the task report.
- [ ] **Step 4: Commit** — `"offxy: bundled MCP stdio server (docxy_* + xlsxy_* tools)"`

---

### Task 7: Register the MCP server with VS Code + Claude Code docs

**Files:**
- Modify: `offxy-vscode/package.json`, `offxy-vscode/src/extension.ts`, `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`, `docs/agent-control.md`

**Interfaces:**
- Consumes: VS Code MCP API — `contributes.mcpServerDefinitionProviders` (package.json) + `vscode.lm.registerMcpServerDefinitionProvider(id, provider)` returning `McpStdioServerDefinition[]` with `command: process.execPath` (the Electron node) or `'node'`, `args: [<abs path to mcp/server.mjs>]`. VERIFY the exact API names against the installed VS Code's `vscode.d.ts` (`node_modules/@types/vscode`) — if the proposal/type names differ, follow the d.ts; if the API is unavailable in `engines.vscode` 1.84, bump `engines.vscode` to the minimum that has it stable and note it in the changelog.
- Produces: Copilot agent mode lists the tools automatically; README documents `claude mcp add offxy -- node <extension>/mcp/server.mjs` (with the versioned-extension-path caveat and the `docxy --mcp` alternative).

- [ ] **Step 1: package.json contribution + activation event** (whatever the d.ts requires, likely `onMcpServerDefinition:<id>`); **Step 2: register in `activate`**; **Step 3: docs** — README "AI assistants" section (Copilot: automatic; Claude Code: the one-liner; what tools exist; live-tab semantics), CHANGELOG entry, and a short "VS Code tabs" subsection in `docs/agent-control.md` (tabs advertise as `<app>-vscode-*`; same verbs; `doc.reload` quirk).
- [ ] **Step 4: typecheck/build/package/install; verify in `code --list-extensions` and that the vsix contains `mcp/server.mjs` (unzip -l).**
- [ ] **Step 5: Commit** — `"offxy: register the bundled MCP server with VS Code; document assistant access"`

---

### Task 8: Full verification + ledger

- [ ] **Step 1:** `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -p docxcore -p docxy -p docxwasm -p gridwasm -p xlsxy` — all green, exit codes reported.
- [ ] **Step 2:** Both wasm32 builds; `npm run typecheck && npm run build`; vsce package; install. Re-run the Task 5 and Task 6 harnesses against the FINAL artifacts.
- [ ] **Step 3:** Manual e2e checklist for Boris (in the report): open docx + xlsx tabs → `docxy_list`/`xlsxy_list` from Claude Code show the `-vscode-` instances → a `docxy_replace_range` edit appears live in the tab with the dirty dot on and Ctrl+Z undoing it → Copilot agent mode lists/uses the tools.
- [ ] **Step 4: Commit** any stragglers — `"offxy: agent-access verification pass"`

## Self-Review Notes

- Spec coverage: Layer 1 → Tasks 1–2; Layer 4 wasm → Task 3; Layer 2 → Tasks 4–5; Layer 3 → Tasks 6–7; testing/docs → per-task + Task 8. `doc.reload` host-quirk documented (Task 5/7). yppxy: out of scope per spec.
- Type consistency: `CtlServer`/`CtlHost` names used in Tasks 4 and 5 match; `docx_ctl`/`grid_ctl`/`Session::ctl` consistent across Tasks 2, 3, 5.
- Known judgment calls encoded: JSON parser copied into the wasm crates (dependency rules), tab repaint driven by a host-computed `repaint` flag, `vscode.workspace.save` for the save verb, revert-quirk accepted and documented.
