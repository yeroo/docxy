# MCP New-File Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `docxy_new` and `xlsxy_new` MCP tools — create a blank .docx/.xlsx at a path and open it — to the terminal MCP servers and the bundled VS Code MCP server, with no ctl protocol change.

**Architecture:** `new` is composed in the MCP layer: resolve the target instance FIRST (bad/ambiguous target creates nothing), create the blank file at an absolutized path (refusing to overwrite), then reuse the existing `doc.open`/`wb.open` verb. Shared engine `ctlcore::client::new_file`; terminal binaries build blank bytes from their own document models; the bundled server ships committed template assets guarded by Rust load tests.

**Tech Stack:** Rust (ctlcore, docxy, xlsxy, docxcore, gridcore — std-only as today), plain Node ESM (`offxy-vscode/mcp/server.mjs`, dependency-free).

**Spec:** `docs/superpowers/specs/2026-07-18-mcp-new-file-design.md`
**Branch:** `claude/mcp-new-file` (based on `claude/offxy-agent-access`, PR #22).

## Global Constraints

- No version bumps anywhere (workspace 0.4.0, extension 0.3.0); release is Boris's call.
- No ctl protocol change: the tab/terminal ctl surfaces are untouched — no new verbs, no Rust changes to docxwasm/gridwasm/ctlserver.ts/extension.ts.
- Existing terminal behavior must not change: all existing docxy/xlsxy tests pass unmodified.
- MCP tool parity is exact between the terminal binaries and `server.mjs`: same tool names, character-identical descriptions, same inputSchema property names/types/required arrays, same tool ORDER (insert `*_new` immediately after `*_list` on every surface).
- Error-message parity: `server.mjs`'s new-path error strings mirror the Rust ones byte-for-byte.
- `server.mjs` stays dependency-free (Node built-ins only).
- Reply shape (spec-fixed): success = `{"path":<abs>,"opened":true,"instance":<name>}` or `{"path":<abs>,"opened":false}`; errors: `missing path`, `already exists: <abs>`, `bad path: <io err>`, `create failed: <io err>`, `created <abs> but open failed: <err>`, `no running <app> matches target "<t>"`, and `resolve_target`'s existing ambiguity wording verbatim.
- **Windows agent shell quirks:** every cargo/npm command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Packaging via `npx --yes @vscode/vsce@latest package --no-dependencies`.

---

### Task 1: `ctlcore::client::{resolve_target_for_new, new_file}` — the shared engine

**Files:**
- Modify: `ctlcore/src/client.rs`

**Interfaces:**
- Consumes: existing `discover_live`, `Client::call`, `Json` (variants `Null/Bool/Num/Str/Arr/Obj`), the fixture pattern in client.rs's existing tests (`crate::serve` + a reply thread).
- Produces (Tasks 2–3 depend on these exact signatures):
  ```rust
  pub fn resolve_target_for_new(dir: &Path, app: &str, target: Option<&str>) -> Result<Option<Client>, String>;
  pub fn new_file(dir: &Path, app: &str, open_verb: &str, blank: &[u8], args: &Json) -> Result<Json, String>;
  ```

- [ ] **Step 1: Write the failing tests** in client.rs's `#[cfg(test)] mod tests` (reuse the `serve` + reply-thread style of `discovers_and_calls_a_live_server`):

```rust
#[test]
fn new_file_without_instance_creates_and_reports_unopened() {
    let dir = std::env::temp_dir().join(format!("ctlcore_new_none_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("fresh.docx");
    let args = Json::obj(vec![("path", Json::Str(out.display().to_string()))]);
    let r = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"BLANK");
    assert_eq!(r.get("opened").and_then(Json::as_bool), Some(false));
    assert!(r.get_str("path").unwrap().contains("fresh.docx"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_file_refuses_overwrite_and_bad_target_creates_nothing() {
    let dir = std::env::temp_dir().join(format!("ctlcore_new_guard_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let existing = dir.join("existing.docx");
    std::fs::write(&existing, b"OLD").unwrap();
    let args = Json::obj(vec![("path", Json::Str(existing.display().to_string()))]);
    let err = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap_err();
    assert!(err.starts_with("already exists: "), "{err}");
    assert_eq!(std::fs::read(&existing).unwrap(), b"OLD");

    // A target that matches nothing errors and writes nothing.
    let fresh = dir.join("never.docx");
    let args = Json::obj(vec![
        ("path", Json::Str(fresh.display().to_string())),
        ("target", Json::Str("nope".into())),
    ]);
    let err = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap_err();
    assert_eq!(err, "no running docxy matches target \"nope\"");
    assert!(!fresh.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_file_with_live_instance_creates_then_opens() {
    let dir = std::env::temp_dir().join(format!("ctlcore_new_live_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (server, rx) = serve(&dir, "docxy-new-test").unwrap();
    let (tx, opened_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for req in rx {
            if req.verb == "doc.open" {
                tx.send(req.args.get_str("path").unwrap_or("").to_string()).ok();
                req.reply_ok(Json::obj(vec![("path", Json::Str("x".into()))]));
            } else {
                req.reply_err("nope");
            }
        }
    });
    let out = dir.join("opened.docx");
    let args = Json::obj(vec![("path", Json::Str(out.display().to_string()))]);
    let r = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap();
    assert_eq!(r.get("opened").and_then(Json::as_bool), Some(true));
    assert_eq!(r.get_str("instance"), Some("docxy-new-test"));
    // The instance received the SAME absolute path that was created.
    let sent = opened_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(sent, r.get_str("path").unwrap());
    assert!(out.exists());
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
```

(If `serve`'s request type names its fields differently than `req.verb`/`req.args`, mirror whatever `discovers_and_calls_a_live_server` uses.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ctlcore new_file`
Expected: FAIL — `new_file` / `resolve_target_for_new` not found.

- [ ] **Step 3: Implement** in client.rs, below `resolve_target`:

```rust
/// Like [`resolve_target`], but for tools that can proceed without any
/// instance: zero live instances with no `target` is `Ok(None)` instead of an
/// error. A `target` that matches nothing, or an ambiguous candidate set, is
/// still an error — a bad target must not be silently ignored.
pub fn resolve_target_for_new(
    dir: &Path,
    app: &str,
    target: Option<&str>,
) -> Result<Option<Client>, String> {
    let prefix = format!("{app}-");
    let mut live: Vec<_> = discover_live(dir)
        .into_iter()
        .filter(|i| i.instance.starts_with(&prefix))
        .collect();
    if let Some(target) = target {
        live.retain(|i| i.instance.contains(target));
        if live.is_empty() {
            return Err(format!("no running {app} matches target \"{target}\""));
        }
    }
    match live.len() {
        0 => Ok(None),
        1 => Ok(Some(live.remove(0).client())),
        _ => {
            let names: Vec<&str> = live.iter().map(|i| i.instance.as_str()).collect();
            Err(format!(
                "several {app} instances are running ({}); pass \"target\" with a distinguishing substring (e.g. the pane id)",
                names.join(", ")
            ))
        }
    }
}

/// The shared engine of the `docxy_new`/`xlsxy_new` MCP tools: create a new
/// file from `blank` bytes at `args.path` (absolutized against this process's
/// cwd — the creating process and the target instance have different cwds, so
/// the absolute path is used both for creation and in the open request), then
/// open it in the resolved `app` instance via `open_verb`. Resolution runs
/// FIRST so a bad or ambiguous target creates nothing; with no target and no
/// live instance the file is still created and `opened` is false. Refuses to
/// overwrite an existing file.
pub fn new_file(
    dir: &Path,
    app: &str,
    open_verb: &str,
    blank: &[u8],
    args: &Json,
) -> Result<Json, String> {
    let path = args.get_str("path").ok_or("missing path")?;
    let abs = std::path::absolute(Path::new(path)).map_err(|e| format!("bad path: {e}"))?;
    let client = resolve_target_for_new(dir, app, args.get_str("target"))?;
    if abs.exists() {
        return Err(format!("already exists: {}", abs.display()));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create failed: {e}"))?;
    }
    std::fs::write(&abs, blank).map_err(|e| format!("create failed: {e}"))?;
    let abs_str = abs.display().to_string();
    match client {
        Some(client) => {
            client
                .call(open_verb, Json::obj(vec![("path", Json::Str(abs_str.clone()))]))
                .map_err(|e| format!("created {abs_str} but open failed: {e}"))?;
            let name = client.instance().instance.clone();
            Ok(Json::obj(vec![
                ("path", Json::Str(abs_str)),
                ("opened", Json::Bool(true)),
                ("instance", Json::Str(name)),
            ]))
        }
        None => Ok(Json::obj(vec![
            ("path", Json::Str(abs_str)),
            ("opened", Json::Bool(false)),
        ])),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ctlcore`
Expected: PASS (all ctlcore tests, including the three new ones).

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo fmt --all --check && cargo clippy -p ctlcore --all-targets -- -D warnings
git add ctlcore && git commit -m "ctlcore: new_file — create-and-open engine for the *_new MCP tools"
```

---

### Task 2: `docxy_new` in the terminal MCP server

**Files:**
- Modify: `docxy/src/mcp.rs`

**Interfaces:**
- Consumes: Task 1's `client::new_file`; `docxcore::package::{new_package, save_package, load_package}`; `docxcore::model::{Block, Document, Inline, ParProps, Paragraph, Run, RunProps}` (the `doc_with` fixture in `docxy/src/control.rs:294` shows the exact construction).
- Produces: tool `docxy_new` (schema below — Task 4 generates the template with it; Task 5 mirrors it verbatim); `pub(crate) fn blank_docx_bytes() -> Vec<u8>`.

- [ ] **Step 1: Write the failing tests** in mcp.rs's test mod:

```rust
#[test]
fn blank_docx_bytes_load_back_as_one_empty_paragraph() {
    let pkg = docxcore::package::load_package(&blank_docx_bytes()).expect("blank loads");
    assert_eq!(pkg.document.body.len(), 1);
    assert_eq!(pkg.document.plain_text(), "\n");
}

#[test]
fn tool_defs_include_docxy_new_with_required_path() {
    let defs = tool_defs();
    let tools = defs.as_array().unwrap();
    // Ordered right after docxy_list (parity with the bundled server).
    let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
    let list_pos = names.iter().position(|n| *n == "docxy_list").unwrap();
    assert_eq!(names[list_pos + 1], "docxy_new");
    let new_tool = tools.iter().find(|t| t.get_str("name") == Some("docxy_new")).unwrap();
    let req = new_tool.get("inputSchema").unwrap().get("required").unwrap();
    assert_eq!(req.to_string(), "[\"path\"]");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p docxy mcp`
Expected: FAIL — `blank_docx_bytes` not found / `docxy_new` missing.

- [ ] **Step 3: Implement.** In `do_tool`, after the `docxy_list` early-return and BEFORE the verb match:

```rust
    if name == "docxy_new" {
        return Ok(client::new_file(&dir, "docxy", "doc.open", &blank_docx_bytes(), args)?.to_string());
    }
```

Add the helper (bottom of the file, above the tests):

```rust
/// A minimal valid .docx: one empty paragraph in a fresh OPC package. Also the
/// source of the committed template the bundled VS Code MCP server ships.
pub(crate) fn blank_docx_bytes() -> Vec<u8> {
    use docxcore::model::{Block, Document, Inline, ParProps, Paragraph, Run, RunProps};
    let doc = Document {
        body: vec![Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: String::new(),
                props: RunProps::default(),
            })],
        })],
    };
    docxcore::package::save_package(&docxcore::package::new_package(doc))
}
```

In `tool_defs()`, insert immediately after the `docxy_list` tool:

```rust
        tool(
            "docxy_new",
            "Create a new blank .docx at a path and open it in the running docxy (in a VS Code \
             window, a new tab). With no docxy running the file is still created. Refuses to \
             overwrite an existing file.",
            vec![
                (
                    "path",
                    prop("string", "File path for the new document (created; must not exist)."),
                ),
                target(),
            ],
            &["path"],
        ),
```

(If `Paragraph`/`Run` field names differ from the `doc_with` fixture, follow the fixture — it compiles today.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p docxy`
Expected: PASS — all docxy tests, existing ones unmodified.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo fmt --all --check && cargo clippy -p docxy --all-targets -- -D warnings
git add docxy && git commit -m "docxy: docxy_new MCP tool — create a blank document and open it"
```

---

### Task 3: `xlsxy_new` in the terminal MCP server

**Files:**
- Modify: `xlsxy/src/mcp.rs`

**Interfaces:**
- Consumes: Task 1's `client::new_file`; `gridcore::xlsx::{new_xlsx, save_xlsx, load_xlsx}`.
- Produces: tool `xlsxy_new` (Task 4 generates the template with it; Task 5 mirrors it verbatim); `pub(crate) fn blank_xlsx_bytes() -> Vec<u8>`.

- [ ] **Step 1: Write the failing tests** in xlsxy/src/mcp.rs's test mod (follow its existing test style):

```rust
#[test]
fn blank_xlsx_bytes_load_back_with_one_sheet() {
    let pkg = gridcore::xlsx::load_xlsx(&blank_xlsx_bytes()).expect("blank loads");
    // new_xlsx() ships exactly one sheet named Sheet1; assert via whatever
    // accessor SheetPackage exposes (sheet count / name).
    assert_eq!(pkg.sheets.len(), 1);
}

#[test]
fn tool_defs_include_xlsxy_new_with_required_path() {
    let defs = tool_defs();
    let tools = defs.as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t.get_str("name")).collect();
    let list_pos = names.iter().position(|n| *n == "xlsxy_list").unwrap();
    assert_eq!(names[list_pos + 1], "xlsxy_new");
    let new_tool = tools.iter().find(|t| t.get_str("name") == Some("xlsxy_new")).unwrap();
    let req = new_tool.get("inputSchema").unwrap().get("required").unwrap();
    assert_eq!(req.to_string(), "[\"path\"]");
}
```

(If `SheetPackage` doesn't expose `.sheets` publicly, assert equivalently — e.g. `load_xlsx(...).is_ok()` plus whatever sheet-count accessor exists; check `gridcore/src/xlsx.rs:64`.)

- [ ] **Step 2: Run to verify FAIL** — `cargo test -p xlsxy mcp`.

- [ ] **Step 3: Implement.** In `do_tool`, after the `xlsxy_list` early-return:

```rust
    if name == "xlsxy_new" {
        return Ok(client::new_file(&dir, "xlsxy", "wb.open", &blank_xlsx_bytes(), args)?.to_string());
    }
```

Helper:

```rust
/// A minimal valid .xlsx: one empty sheet ("Sheet1") in a fresh OPC package.
/// Also the source of the committed template the bundled VS Code MCP server ships.
pub(crate) fn blank_xlsx_bytes() -> Vec<u8> {
    gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx())
}
```

In `tool_defs()`, insert immediately after the `xlsxy_list` tool:

```rust
        tool(
            "xlsxy_new",
            "Create a new blank .xlsx at a path and open it in the running xlsxy (in a VS Code \
             window, a new tab). With no xlsxy running the file is still created. Refuses to \
             overwrite an existing file.",
            vec![
                (
                    "path",
                    prop("string", "File path for the new workbook (created; must not exist)."),
                ),
                target(),
            ],
            &["path"],
        ),
```

- [ ] **Step 4: Run to verify PASS** — `cargo test -p xlsxy`.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo fmt --all --check && cargo clippy -p xlsxy --all-targets -- -D warnings
git add xlsxy && git commit -m "xlsxy: xlsxy_new MCP tool — create a blank workbook and open it"
```

---

### Task 4: Committed template assets + drift guards

**Files:**
- Create: `offxy-vscode/mcp/templates/blank.docx`, `offxy-vscode/mcp/templates/blank.xlsx` (binary, committed)
- Modify: `docxy/src/mcp.rs` (one test), `xlsxy/src/mcp.rs` (one test)

**Interfaces:**
- Consumes: Tasks 2–3's tools and `blank_*_bytes()` helpers; the built release binaries.
- Produces: the template files Task 5's `server.mjs` copies.

- [ ] **Step 1: Generate the templates via the real tools** — this doubles as the terminal tools' end-to-end test. Point `APPDATA` at an empty temp dir so NO real instance is discovered (otherwise the tool would open the template in a live editor):

```bash
cargo build --release -p docxy -p xlsxy
mkdir -p offxy-vscode/mcp/templates
TMPA=$(mktemp -d)
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"docxy_new","arguments":{"path":"offxy-vscode/mcp/templates/blank.docx"}}}' \
  | APPDATA="$TMPA" XDG_CONFIG_HOME="$TMPA" target/release/docxy --mcp
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"xlsxy_new","arguments":{"path":"offxy-vscode/mcp/templates/blank.xlsx"}}}' \
  | APPDATA="$TMPA" XDG_CONFIG_HOME="$TMPA" target/release/xlsxy --mcp
```

Expected: each second reply's text contains `"opened":false` and the absolute template path; both files exist and are a few KB.

- [ ] **Step 2: Add the drift-guard tests.** In docxy/src/mcp.rs tests:

```rust
#[test]
fn committed_blank_template_matches_blank_docx_bytes() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../offxy-vscode/mcp/templates/blank.docx");
    let bytes = std::fs::read(&p).expect("template committed");
    assert_eq!(bytes, blank_docx_bytes(), "regenerate the template (see plan Task 4)");
}
```

And the xlsxy twin in xlsxy/src/mcp.rs (`blank.xlsx` vs `blank_xlsx_bytes()`). If either equality fails because the zip writer embeds a timestamp (nondeterminism), downgrade THAT test to load-and-shape assertions (`load_package`/`load_xlsx` + block/sheet count) and note it in the task report.

- [ ] **Step 3: Run** — `cargo test -p docxy -p xlsxy`
Expected: PASS.

- [ ] **Step 4: Verify the vsix will ship the templates.** `offxy-vscode/.vscodeignore` must NOT exclude `mcp/**` (it doesn't today — confirm no pattern matches `mcp/templates/`).

- [ ] **Step 5: Commit**

```bash
git add offxy-vscode/mcp/templates docxy/src/mcp.rs xlsxy/src/mcp.rs
git commit -m "offxy: committed blank-document templates for the bundled MCP server"
```

---

### Task 5: `docxy_new`/`xlsxy_new` in the bundled server (`server.mjs`)

**Files:**
- Modify: `offxy-vscode/mcp/server.mjs`

**Interfaces:**
- Consumes: Task 4's templates; server.mjs's existing `discoverLive`, `ctlDir`, `callInstance`, `prop`, `tool`, `doTool`; the Rust wording from Tasks 1–3 (byte-for-byte parity).
- Produces: tools `docxy_new`/`xlsxy_new`, schema- and wording-identical to the terminal binaries.

- [ ] **Step 1: Tool definitions.** In `docxyToolDefs()`, insert immediately after the `docxy_list` tool:

```js
    tool(
      'docxy_new',
      'Create a new blank .docx at a path and open it in the running docxy (in a VS Code ' +
        'window, a new tab). With no docxy running the file is still created. Refuses to ' +
        'overwrite an existing file.',
      Object.fromEntries([
        ['path', prop('string', 'File path for the new document (created; must not exist).')],
        target(),
      ]),
      ['path'],
    ),
```

And the xlsxy twin (`.xlsx` / `xlsxy` wording, `'File path for the new workbook (created; must not exist).'`) immediately after `xlsxy_list` in `xlsxyToolDefs()`. The description strings must match the Rust `tool_defs()` character-for-character (mind the multi-line string joins — the Rust `\` continuation collapses to a single space).

- [ ] **Step 2: The new-path engine.** Below `resolveTarget`, add mirrors of Task 1's Rust (same doc comments style as the file's other mirrors; identical error wording):

```js
/** Like `resolveTarget`, but for tools that can proceed without any instance:
 *  zero live instances with no `target` is `undefined` instead of an error. A
 *  `target` that matches nothing, or an ambiguous candidate set, is still an
 *  error — mirrors `ctlcore::client::resolve_target_for_new`. */
async function resolveTargetForNew(app, target) {
  const prefix = `${app}-`;
  let live = (await discoverLive(ctlDir(app))).filter((i) => i.instance.startsWith(prefix));
  if (typeof target === 'string') {
    live = live.filter((i) => i.instance.includes(target));
    if (live.length === 0) {
      throw new Error(`no running ${app} matches target "${target}"`);
    }
  }
  if (live.length === 0) return undefined;
  if (live.length === 1) return live[0];
  const names = live.map((i) => i.instance).join(', ');
  throw new Error(
    `several ${app} instances are running (${names}); pass "target" with a distinguishing substring (e.g. the pane id)`,
  );
}

const TEMPLATES = {
  docxy: path.join(__dirname, 'templates', 'blank.docx'),
  xlsxy: path.join(__dirname, 'templates', 'blank.xlsx'),
};

/** `docxy_new`/`xlsxy_new`: copy the shipped blank template to an absolutized
 *  path, then open it via the existing open verb — mirrors
 *  `ctlcore::client::new_file` (resolution first: a bad or ambiguous target
 *  creates nothing; no live instance still creates, with `opened:false`). */
async function doNew(app, args) {
  if (typeof args?.path !== 'string') throw new Error('missing path');
  const abs = path.resolve(args.path);
  const target = typeof args?.target === 'string' ? args.target : undefined;
  const inst = await resolveTargetForNew(app, target);
  if (fs.existsSync(abs)) throw new Error(`already exists: ${abs}`);
  try {
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    // COPYFILE_EXCL: create-exclusive, so a file appearing between the exists
    // check and the copy errors instead of being truncated — mirrors the
    // create_new(true) open in ctlcore::client::new_file.
    fs.copyFileSync(TEMPLATES[app], abs, fs.constants.COPYFILE_EXCL);
  } catch (e) {
    if (e?.code === 'EEXIST') throw new Error(`already exists: ${abs}`);
    throw new Error(`create failed: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (inst === undefined) return JSON.stringify({ path: abs, opened: false });
  try {
    await callInstance(inst, app === 'docxy' ? 'doc.open' : 'wb.open', { path: abs });
  } catch (e) {
    throw new Error(`created ${abs} but open failed: ${e instanceof Error ? e.message : String(e)}`);
  }
  return JSON.stringify({ path: abs, opened: true, instance: inst.instance });
}
```

In `doTool`, after the two `_list` early-returns:

```js
  if (name === 'docxy_new') return doNew('docxy', args);
  if (name === 'xlsxy_new') return doNew('xlsxy', args);
```

(Do NOT add `docxy_new`/`xlsxy_new` to `DOCXY_VERBS`/`XLSXY_VERBS` — they are not forwarded verbs.)

- [ ] **Step 3: Harness (scratchpad, not committed).** Extend/adapt the existing MCP harness (see the end of `.superpowers/sdd/task-6-report.md` for its location and how it spawns `node mcp/server.mjs` and a fake ctl instance; if missing, rebuild from that description). Isolate discovery by setting `APPDATA`/`XDG_CONFIG_HOME` to a temp dir in the child env. Cases:
  1. `tools/list` now returns 21 tools; `docxy_new` sits right after `docxy_list`, requires `path`.
  2. `docxy_new` with no instance → reply text parses to `{path, opened:false}`, file exists, byte-equal to `templates/blank.docx`.
  3. Same path again → `isError:true`, text `already exists: <abs>`; file unchanged.
  4. With the fake docxy ctl instance running: `docxy_new` (fresh path) → fake receives `doc.open` with the SAME absolute path; reply `{path, opened:true, instance}`.
  5. `docxy_new` with `target:"nope"` → error `no running docxy matches target "nope"`, and the file was NOT created.
  6. `xlsxy_new` no-instance smoke case (`opened:false`, byte-equal to `blank.xlsx`).

Run: `node <scratch>/mcp_harness.mjs` — expected `ALL OK`, exit 0.

- [ ] **Step 4: Schema/wording parity cross-check.** Drive `tools/list` against `target/release/docxy --mcp` and `target/release/xlsxy --mcp` (built in Task 4) and against `node mcp/server.mjs`; diff each tool's `name`, `description`, `inputSchema` — must be identical for all 21 tools. Paste the diff summary (expected: no differences) in the task report.

- [ ] **Step 5: Commit**

```bash
git add offxy-vscode/mcp/server.mjs
git commit -m "offxy: docxy_new / xlsxy_new in the bundled MCP server"
```

---

### Task 6: Docs + full verification

**Files:**
- Modify: `docs/agent-control.md`, `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`

- [ ] **Step 1: Docs.**
  - `docs/agent-control.md`: add `docxy_new`/`xlsxy_new` to the MCP tool enumerations (wherever `docxy_list`/`xlsxy_list` are listed, keeping the after-`_list` order); in the "VS Code tabs" section add one line: `docxy_new`/`xlsxy_new` on a tab instance opens the new document as a NEW tab (same as `doc.open`); with no tab alive the file is created on disk but nothing opens (`"opened":false`).
  - `offxy-vscode/README.md`: add the two tools to the AI-assistants tools list (same position).
  - `offxy-vscode/CHANGELOG.md`: entry under the unreleased/current section: bundled MCP server gains `docxy_new`/`xlsxy_new` (create a blank document/workbook and open it; ships blank templates under `mcp/templates/`).

- [ ] **Step 2: Full gates.**

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test -p ctlcore -p docxcore -p docxy -p gridcore -p xlsxy -p docxwasm -p gridwasm
cd offxy-vscode && npm run typecheck && npm run build
npx --yes @vscode/vsce@latest package --no-dependencies
```

Expected: all exit 0; the vsce file listing shows `mcp/server.mjs` AND `mcp/templates/blank.docx` + `blank.xlsx`. Install: `code --install-extension <vsix> --force` (full path `$LOCALAPPDATA/Programs/Microsoft VS Code/bin/code` if needed).

- [ ] **Step 3: Re-run the Task 5 harness** against the final artifacts — `ALL OK`, exit 0.

- [ ] **Step 4: Manual e2e note for Boris** (in the task report, not a doc): from Claude Code with the bundled server registered — `docxy_new {path:"<workspace>/draft.docx"}` with a docx tab open should create the file and open a second tab; with no tab open it should create the file and report `opened:false`; in a terminal docxy pane, `docxy_new` should swap the pane to the new document in place.

- [ ] **Step 5: Commit**

```bash
git add docs/agent-control.md offxy-vscode/README.md offxy-vscode/CHANGELOG.md
git commit -m "offxy: document the docxy_new / xlsxy_new tools"
```

## Self-Review Notes

- Spec coverage: semantics (absolutize/resolve-first/no-overwrite/parent-dirs) → Task 1; terminal tools → Tasks 2–3; templates + drift guard → Task 4; bundled server + parity → Task 5; docs + error table + verification → Task 6. Reply shape and every error string pinned in Global Constraints.
- Type consistency: `new_file(dir, app, open_verb, blank, args)` identical in Tasks 1–3; `blank_docx_bytes`/`blank_xlsx_bytes` names consistent across Tasks 2–4; tool ordering rule (after `_list`) enforced by tests in Tasks 2, 3 and the harness in Task 5.
- Known judgment calls encoded: template generation runs the real tools with `APPDATA` pointed at an empty dir (doubles as terminal e2e); byte-equality drift guard with a sanctioned fallback if the zip writer is nondeterministic; `resolve_target_for_new` gets its own clearer no-match wording (new tool, no back-compat concern) while reusing the ambiguity wording verbatim.
