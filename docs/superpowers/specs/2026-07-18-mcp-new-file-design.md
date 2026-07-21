# MCP new-file tools — design

**Goal:** Agents (Claude Code, VS Code Copilot) can create a new blank .docx or .xlsx
at a path and start editing it, via new MCP tools `docxy_new` and `xlsxy_new`.

**Builds on:** the agent-access work (`docs/superpowers/specs/2026-07-17-offxy-agent-access-design.md`,
PR #22): terminal `docxy --mcp` / `xlsxy --mcp` servers, the bundled VS Code MCP
server (`offxy-vscode/mcp/server.mjs`), and the ctl protocol in `docs/agent-control.md`.

## Decision summary

- Both formats: `docxy_new` (.docx) and `xlsxy_new` (.xlsx).
- All MCP surfaces: the terminal Rust MCP servers AND the bundled VS Code server
  gain the same two tools — tool-list parity between them stays exact.
- **No ctl protocol change.** `new` is composed in the MCP layer: create the blank
  file on disk, then reuse the existing `doc.open` / `wb.open` verb to open it.
  The tab and terminal ctl surfaces are untouched, so the "a VS Code tab is
  indistinguishable from a terminal instance" guarantee cannot regress.
- No-instance case: the tool still creates the file and reports that it is on
  disk but not opened. (This is the reason `new` lives in the MCP layer — a ctl
  verb would need a live instance.)

## Tool semantics (identical on every surface)

- Args: `{path, target?}`. `path` required; `target` optional with the same
  substring-resolution and ambiguity-error semantics as every other tool
  (`resolve_target` in `ctlcore/src/client.rs`).
- The MCP server **absolutizes `path` against its own cwd before doing anything**,
  creates parent directories, and uses the absolute path both for file creation
  and in the `doc.open`/`wb.open` request. (The creating process and the target
  instance have different cwds; sending a relative path would open a different
  file than was created.)
- Refuses to overwrite: if the file exists, error `already exists: <path>`.
  No overwrite option — agents must not destroy documents via `new`.
- Blank content: one empty paragraph (.docx) / one empty sheet named `Sheet1`
  with no cells (.xlsx). Files must be loadable by docxy/xlsxy/the extension.
- Open step: resolution runs FIRST, before anything is written, with the same
  semantics as the existing tools. Outcomes: exactly one instance resolves →
  create the file, then send `doc.open`/`wb.open` with the absolute path
  (terminal: in-place swap; VS Code tab: opens a NEW tab, per the documented
  deviation); zero live instances (and no `target` given) → create the file,
  skip the open step; multiple candidates, or a `target` that matches nothing →
  the standard resolution/ambiguity error, nothing written.
- Reply (MCP text content, JSON): `{"path": <abs path>, "opened": true|false}`
  plus `"instance": <name>` when opened. Creation errors and resolution errors
  use the same error style as the existing tools.

## Components

### 1. Terminal Rust MCP servers

- `docxy/src/mcp.rs` + `xlsxy/src/mcp.rs` (+ shared scaffolding in
  `ctlcore/src/mcp.rs` where it fits): add the `*_new` tool definition and
  handler.
- Blank-file creation uses the app's own document model and save machinery
  (docxy: blank `Document` → save-as; xlsxy: blank workbook → save). No
  embedded assets in the binaries.

### 2. Bundled server (`offxy-vscode/mcp/server.mjs`)

- Ships two committed template assets: `offxy-vscode/mcp/templates/blank.docx`
  and `blank.xlsx` (a few KB each, produced by the terminal apps, committed as
  binaries, included in the vsix). `server.mjs` stays dependency-free: create =
  `fs.copyFileSync` of the template (after the exists check), then the open step
  over the existing TCP client code.
- A Rust test asserts docxy/gridcore can load the committed templates, so the
  assets cannot silently rot.

### 3. Docs

- `docs/agent-control.md`: add the two tools to the MCP tool tables; one line in
  the "VS Code tabs" section noting `new` on a tab opens a new tab (same as
  `open`) and that with no tab alive the file is created but nothing opens.
- `offxy-vscode/README.md` tools list + CHANGELOG entry.

## Error handling

| Case | Behavior |
|------|----------|
| file exists | error, nothing written |
| `target` matches nothing, or resolution is ambiguous (with or without `target`) | standard resolution error, nothing written |
| path unwritable / parent mkdir fails | error, nothing opened |
| no `target`, no live instance | success: `{"path":…, "opened":false}` |
| open step fails after creation (instance died) | error mentioning the file WAS created at `<path>` |

## Testing

- Rust: unit tests for blank-file creation (file loads back into the model) and
  the tool handler paths (exists error, no-instance reply, target-first
  resolution order).
- Bundled server: extend the Task-6-style harness — `tools/list` parity
  cross-check against the real terminal binaries now includes the new tools;
  `tools/call` cases: create in a temp dir (no instance → `opened:false`),
  create with a live instance (`opened:true`, tab/instance actually serves the
  file), exists error, bad target creates nothing.
- Template assets: the Rust load test above.

## Out of scope

- yppxy (no MCP surface in the agent-access design).
- Overwrite/truncate semantics, templates beyond blank, and creating from
  content in one call (`new` + existing edit verbs compose for that).
- Version bumps: none; release remains Boris's call.
