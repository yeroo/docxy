# Offxy Agent Access (VS Code ctl bridge + bundled MCP) — Design

**Date:** 2026-07-17
**Status:** Approved (design review with Boris, this session). **Unparked:**
PR #20 (xlsxy/yppxy agent bridges) has merged; it kept verb logic per-app in
the TUIs (no docxcore/gridcore-level extraction), so Layer 1's extraction has
no collision, and Layer 4's xlsxy verb/tool set is now pinned (see Layer 4).

## Summary

Give AI assistants — Claude Code and VS Code Copilot agent mode — full
read+write access to Word/Excel documents open in the offxy VS Code extension,
by teaching offxy to speak the existing **ctlcore** agent-control protocol
(from `docs/agent-control.md`, merged in PR #19). VS Code tabs advertise
themselves exactly like terminal docxy panes, so the same MCP thin clients see
both. A small bundled Node MCP server makes the tools available to Copilot with
zero user setup.

## Context (what exists on main today)

- **ctlcore**: app-neutral control-surface crate — loopback TCP, newline JSON,
  token auth, discovery files. Server side: `serve(dir, instance) -> (Server,
  Receiver<Request>)` with channel dispatch and `reply_ok`/`reply_err`. Client
  side: `discover_live(dir)` + `Client::call(verb, args)`.
- **docxy** binds `doc.*` verbs (path/outline/read/find/replace-range/insert/
  append/save/reload/open, block-index addressed) to its live TUI editor in
  `docxy/src/control.rs`; edits ride the native undo stack.
- **`docxy --mcp`**: a thin-client MCP stdio server (newline JSON-RPC 2.0;
  initialize/ping/tools/list/tools/call) exposing 8 verbs as `docxy_*` tools.
  It discovers instances named `docxy-*` in `%APPDATA%\docxy\ctl` and routes
  `target` by substring match.
- Full architecture map: `.superpowers/sdd/agent-control-map.md`.
- The concurrent session is adding the same for xlsxy and yppxy (terminal).

## Decisions made during review

- **Access level:** read + write.
- **Mechanism:** the one ctl protocol + MCP; no VS Code Language-Model tools.
- **Identity:** *same namespaces* — VS Code docx tabs advertise as
  `docxy-vscode-<basename>-<n>` in **docxy's** ctl dir and implement the
  *identical* verb set, so existing `docxy --mcp` sessions see them with no
  reconfiguration. Excel tabs will do the same under xlsxy's namespace once
  its verb set lands.
- **Copilot:** the extension bundles a Node MCP stdio server (TS mirror of
  `mcp.rs`'s thin client) and registers it via VS Code's MCP API.

## Layer 1 — Shared doc-verb core (Rust)

Extract the document-verb logic from `docxy/src/control.rs` into an
editor-generic, std-only module in **docxcore** (working title
`docxcore::agent`): outline, block read (index/kind/text/heading), find,
replace-range, insert, append — implemented over `Editor` + styles, with the
host supplying only (a) how an edit is applied so it lands on that host's undo
stack, and (b) save/reload/path metadata.

- `docxy/src/control.rs` becomes a thin binding (TUI undo + repaint + file IO).
- `docxwasm` gains a ctl entry point:
  `docx_ctl(handle, ptr, len) -> resultPtr` taking one request JSON
  (`{"verb": "...", "args": {...}}`, no token — transport auth is the host's
  job) and returning the verb result JSON (or `{"error": ...}`). Mutating
  verbs route through the session's existing edit paths so each lands as one
  entry on the wasm undo stack (VS Code lockstep preserved).
- `doc.save` / `doc.reload` / `doc.open` / `doc.path` are **host verbs**, not
  wasm verbs: the VS Code bridge answers them at the extension-host level
  (save via VS Code's save pipeline so the dirty state clears; reload via a
  fresh `open` message; `doc.open` = `vscode.openWith`; `doc.path` from the
  document URI + dirty flag + block count from the wasm).

**Coordination constraint:** if the concurrent session's xlsxy work extracts a
shared verb-core shape first (in ctlcore or elsewhere), adopt their shape
instead of introducing a second one. This layer starts only after their merge.

## Layer 2 — ctl servers in the extension host (TypeScript)

New `offxy-vscode/src/ctlserver.ts`:

- Per open custom-editor document, a loopback TCP server (Node `net`) speaking
  ctlcore's wire protocol: one JSON object per line, `token` checked on every
  request, one reply line per request. Port 0 (OS-assigned); token from
  `crypto.randomBytes`.
- Discovery file per instance, ctlcore-compatible shape
  (`{"instance","port","token","pid"}`), written to **docxy's ctl dir**
  (`%APPDATA%\docxy\ctl` / `$XDG_CONFIG_HOME/docxy/ctl`) with instance name
  `docxy-vscode-<sanitized basename>-<seq>`. Names must stay
  substring-targetable and unambiguous alongside terminal panes. Deleted on
  tab close and on extension deactivate; tolerate ctlcore's stale-sweep
  semantics (a dead file is swept by the next terminal docxy start — the
  bridge must also survive its file being swept while alive: recreate on a
  watcher tick or on next advertise refresh).
- Request servicing: TCP line → verb router. Wasm verbs round-trip
  host → `panel.webview.postMessage({type:'ctl', requestId, payload})` →
  webview calls `docx_ctl` → `{type:'ctlResult'}` → TCP reply. Host verbs
  (save/reload/open/path) answered directly (see Layer 1). Mutating verbs
  additionally fire the provider's `edit` event exactly once (dirty dot +
  VS Code undo lockstep), reusing the existing webview `edit` message flow.
- Concurrency: one in-flight request per document (queue), mirroring the
  terminal's single-threaded servicing; per-request timeout with an `ok:false`
  reply if the webview is hidden AND `retainContextWhenHidden` fails us
  (webviews are retained, so normally serviceable while hidden).
- The grid editor gets the same server shell with the verb set left
  unimplemented until Layer 4 (`ok:false, error:"not yet implemented"` for
  unknown verbs is ctl-conformant).

## Layer 3 — Bundled MCP server (offxy-mcp, TypeScript/Node)

New `offxy-vscode/mcp/server.mjs` (bundled in the vsix, no dependencies):

- MCP stdio server mirroring `docxy/src/mcp.rs`: newline-delimited JSON-RPC
  2.0; `initialize`, `ping`, `tools/list`, `tools/call`; same 8 `docxy_*`
  tools with the same input schemas and `target` substring semantics, plus
  `docxy_list`/`docxy_status`. Implementation is a thin ctl client:
  enumerate discovery files, filter live, fresh TCP connection per call.
- Discovers **all** `docxy-*` instances — terminal panes and VS Code tabs
  alike. When xlsxy's tool set lands, the same server grows the `xlsxy_*`
  tools over xlsxy's ctl dir (kept schema-identical to `xlsxy --mcp`).
- **Copilot registration:** the extension contributes an MCP server
  definition provider (`vscode.lm.registerMcpServerDefinitionProvider` +
  the matching `contributes` entry) that launches
  `node <extensionPath>/mcp/server.mjs`. Copilot agent mode then lists the
  tools with no user setup.
- **Claude Code:** documented one-liner
  `claude mcp add offxy -- node <extensionPath>/mcp/server.mjs` (README +
  changelog); terminal users can keep `docxy --mcp` — both clients see the
  same instances, tools are interchangeable.

## Layer 4 — Excel (and later Project) parity

PR #20's xlsxy surface, adopted verbatim:

- Verbs: `wb.path`, `sheet.list`, `sheet.read {sheet?, range?}`,
  `cell.get {ref}`, `cell.set {ref, text}` (leading `=` = formula, validated
  + recalculated), `range.clear {range}`, `find {query}`, `wb.recalc`,
  `wb.save`, `wb.reload`, `wb.open {path}`. A1-style refs; `sheet` by index
  or name.
- `gridwasm` gains `grid_ctl(handle, ptr, len)` implementing the wasm-side
  verbs directly over its `Session` (the sheet verbs are thin over the
  engine — `cell.set` is the existing `set` path — so no gridcore extraction
  is warranted; parity with `xlsxy/src/control.rs` semantics is asserted by
  tests instead). `wb.save`/`wb.reload`/`wb.open`/`wb.path` are host verbs,
  like their docx counterparts.
- Excel tabs advertise as `xlsxy-vscode-<basename>-<n>` in xlsxy's ctl dir
  (`%APPDATA%\xlsxy\ctl`); the bundled MCP server mirrors the ten `xlsxy_*`
  tools schema-identically to `xlsxy --mcp` (built on the same framing
  `ctlcore::mcp` established).
- yppxy has no VS Code editor — nothing to bridge; its terminal ctl+MCP
  shipped in PR #20.

## Security posture

Same as the terminal surface: loopback-only TCP, per-instance random token,
discovery files in the user's config dir. The webview never sees the token
(host strips transport auth before forwarding). No new network exposure
beyond what PR #19 established.

## Testing

- **Rust:** verb-core native tests moved/extended with the extraction —
  identical behavior TUI vs wasm (same fixtures, same expected JSON);
  `docx_ctl` round-trip tests in docxwasm (outline/read/replace-range/undo
  lockstep: one ctl edit = one undo entry).
- **Node integration:** a test script that starts the real ctl server code
  against the real wasm (webview mocked as a direct function call), connects
  over TCP with the ctlcore wire format, and drives read → edit → undo →
  save-path verbs; plus an MCP-server test speaking JSON-RPC over stdio
  (initialize → tools/list → tools/call round-trip against a live instance).
- **Manual e2e:** Claude Code (`docxy --mcp` and the bundled server) editing
  a document open in a VS Code tab, watching it change live; Copilot agent
  mode calling the tools; two docxy instances (terminal + tab) disambiguated
  by `target`.

## Out of scope

- Language-Model tools API integration (rejected: MCP covers both assistants).
- yppxy VS Code editor.
- Remote/web VS Code (the ctl surface is loopback + local filesystem).
- Any change to the terminal apps' verb sets or MCP servers beyond the
  shared-core extraction.
