# lookxy MCP / control surface (design)

Give lookxy the same **agent control surface** docxy/xlsxy/yppxy have: an
external agent (e.g. Claude Code in a sibling agwinterm pane) can read and
triage the **live** open mailbox, and `lookxy --mcp` exposes those operations
as native MCP tools. Built on the shared, dependency-free `ctlcore` crate
(vendored onto this branch), mirroring the established pattern
(`docs/agent-control.md`).

## 1. Goals / non-goals

**Goals**
- Embed a `ctlcore` control server in a running lookxy: on startup write a
  discovery file (`%APPDATA%\lookxy\ctl\<instance>.json`), listen on loopback
  TCP with a token, and serve **mail verbs** against the live `App`
  (its `Store` + `SyncHandle` + selection).
- Reads reflect the live local store (offline-first source of truth, incl.
  optimistically-applied changes). Triage verbs go through the **same** App
  paths as the UI keys — optimistic store write + `SyncCommand` to the engine
  — so an agent's action lands in the outbox and **repaints the panes live**,
  exactly as a keypress would.
- `lookxy --mcp` runs an MCP stdio server (thin client of a running lookxy,
  discovered via the ctl dir) exposing the verbs as `lookxy_*` tools.
- `lookxy install skill` writes a `SKILL.md` for agent self-onboarding.

**Non-goals (match lookxy v1 scope)**
- No compose/reply/forward, no calendar (read + triage only, like the TUI).
- The MCP process opens no mailbox of its own; it only drives a running one.
- No new mailcore/engine changes — the control layer sits over existing
  `App`/`Store`/`SyncHandle` methods.

## 2. Crate layout (mirrors docxy)

- `ctlcore` — vendored (done, commit b461ce1). Provides `serve`, `Request`
  (`arg`/`reply_ok`/`reply_err`), `config_ctl_dir`, `instance_name`,
  `install_skill`, and the `client` / `json` / `mcp` submodules.
- `lookxy/src/control.rs` — `control_dir()`, `instance_name()`, and
  `dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String>`
  routing the mail verbs. New.
- `lookxy/src/mcp.rs` — the MCP stdio bridge (`run()`, `do_tool`,
  `tool_defs()`), a thin `ctlcore::client` of a running lookxy. New.
- `lookxy/src/skill.rs` — `SKILL_MD` + `install()`. New.
- `lookxy/src/main.rs` — wiring (below). `lookxy/Cargo.toml` — add `ctlcore`.

## 3. Verbs (control protocol)

Addressing is by **message id** (Graph id string) and **folder id**. All reads
serialize the live store. `mail.list`/`mail.read` report the fields an agent
needs to then act.

| Verb | Args | Result |
|---|---|---|
| `mail.status` | — | `{account, sync_state, folders, unread_total, pending_ops, selected_folder?, selected_message?}` |
| `mail.folders` | — | `{folders:[{id, name, unread, total, well_known?}]}` |
| `mail.list` | `{folder?, limit?, offset?}` | `{folder, total, messages:[{id, from_name, from_addr, subject, received, is_read, is_flagged, has_attachments, preview}]}` (default folder = current selection or Inbox; default limit 50) |
| `mail.read` | `{id}` | `{id, folder, subject, from, to, cc, received, is_read, is_flagged, has_attachments, body_text, body_pending?}` — body is the rendered plain text; if not cached, requests a fetch and sets `body_pending:true` |
| `mail.search` | `{query, limit?}` | `{query, count, messages:[…as mail.list…]}` (local FTS5) |
| `mail.mark` | `{id, read}` | `{id, is_read}` — optimistic + `SyncCommand::MarkRead` |
| `mail.flag` | `{id, flagged}` | `{id, is_flagged}` — optimistic + `SetFlag` |
| `mail.move` | `{id, dest}` | `{id, folder}` — optimistic + `Move` |
| `mail.delete` | `{id}` | `{id, deleted:true}` — optimistic + `Delete` |
| `mail.attachments` | `{id}` | `{id, attachments:[{id, name, content_type, size}]}` — requests metadata fetch if absent |
| `mail.save-attachment` | `{id, attachment, dest?}` | `{queued:true, dest}` — issues `SaveAttachment` (dest defaults to Downloads) |
| `mail.select` | `{folder?, id?}` | `{selected_folder?, selected_message?}` — moves the TUI selection so the pane reflects it |
| `mail.refresh` | — | `{refreshing:true}` — sends `SyncCommand::Refresh` |

Every verb requests a repaint; mutating verbs also `ctlcore::signal_activity()`
so the pane's agwinterm status dot flashes. Unknown verb → error reply.

## 4. MCP tools

`lookxy --mcp` exposes (via `ctlcore::mcp::McpServer`): `lookxy_list` (running
instances — ctlcore convention), `lookxy_status`, `lookxy_folders`,
`lookxy_messages` (→`mail.list`), `lookxy_read`, `lookxy_search`, `lookxy_mark`,
`lookxy_flag`, `lookxy_move`, `lookxy_delete`, `lookxy_attachments`,
`lookxy_save_attachment`, `lookxy_select`, `lookxy_refresh`. Each maps to a verb
(`do_tool`), resolving `target` (a substring of the instance id) when multiple
lookxy editors run. `claude mcp add lookxy -- lookxy --mcp`.

## 5. main.rs wiring

- `mod control; mod mcp; mod skill;`.
- Early returns before the TUI starts: `--mcp` → `mcp::run()`;
  `lookxy install skill` → `skill::install()`. (Same shape as docxy.)
- In the run loop: `ctlcore::serve(control_dir, instance)` → `(server, ctl_rx)`
  kept alive for the session (Drop removes the discovery file). If the loopback
  bind fails, lookxy runs exactly as before, just without a control channel.
  lookxy's loop already polls crossterm (200 ms) and drains `SyncEvent`s each
  tick; it additionally **`try_recv`s `ctl_rx` each tick**, and for each
  `Request` calls `control::dispatch(&mut app, verb, args)` then
  `req.reply_ok(json)` / `req.reply_err(msg)`, and marks the frame dirty. No
  forwarder thread is needed — the polling loop drains the channel directly.
- Help text gains `--mcp` and `install skill` lines.

## 6. Testing

- `control.rs`: `dispatch` against an `App::for_test_with_seeded_store` (folder
  + messages) — every read verb returns the expected JSON shape; every triage
  verb updates the store and returns the right result; unknown verb errors.
  (No real socket needed — call `dispatch` directly, as docxy's tests do.)
- `mcp.rs`: `tool_defs()` includes the expected tools with object input
  schemas; unknown tool errors; `lookxy_list` shape stable with nothing
  running.
- `skill.rs`: `SKILL_MD` has frontmatter + the verb/tool names;
  `install_skill_to(tmp_home, …)` writes `~/.claude/skills/lookxy/SKILL.md`.
- All existing tests stay green; workspace clippy + fmt clean. CI needs no
  network or account (the control server binds loopback only when the TUI runs;
  tests exercise `dispatch`/`tool_defs` directly).

## 7. Build order (plan)

1. `lookxy/Cargo.toml` add `ctlcore`; `control.rs` with `control_dir`/
   `instance_name` + read verbs (`status`, `folders`, `list`, `read`, `search`)
   + tests.
2. `control.rs` triage verbs (`mark`, `flag`, `move`, `delete`, `select`,
   `refresh`, `attachments`, `save-attachment`) + tests.
3. `mcp.rs` bridge + `tool_defs` + tests.
4. `skill.rs` (`SKILL_MD` + `install`) + tests.
5. `main.rs` wiring (`--mcp`, `install skill`, ctl server in the loop) + help
   text; smoke-build.
6. Docs: `LOOKXY.md` MCP/agent section + a lookxy entry note.
