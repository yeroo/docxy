# Driving the editors from an agent (the control surface)

All three TUIs — **docxy** (Word), **xlsxy** (Excel), and **yppxy** (Project) —
expose a **control surface** so an external agent — e.g. Claude Code running in
a sibling [agwinterm](https://github.com/yeroo/agwinterm) pane — can read and
edit the *live* open document. Edits go through each editor's own edit path, so
they land on the **undo stack** (and, for xlsxy/yppxy, recalculate/reschedule)
and repaint the view instantly; reads reflect **unsaved** changes, because they
serialize the in-memory state, never the file on disk.

The transport is loopback TCP speaking **newline-delimited JSON**, implemented
by the dependency-free [`ctlcore`](../ctlcore) crate, which the three editors
share (server, discovery, MCP scaffolding, skill installer, status signal).
This page documents docxy's verbs in full; xlsxy and yppxy follow the same
pattern with their own verbs (see "The other editors" below and each editor's
`SKILL.md` via `<app> install skill`).

## Two panes in one agwinterm session

A session holds up to two panes via a split. From the Claude pane:

```bash
agwintermctl split on                 # split the current pane
agwintermctl tree --json              # read back the new pane id
agwintermctl session type --target <paneB> 'docxy mydoc.docx\n'
```

Or manually: focus the pane, press **Ctrl+D**, and launch `docxy <file>` in the
new pane. (`agwintermctl session new` makes a *separate* session, not a split.)

## Discovery

On startup each editor writes a discovery file to
`%APPDATA%\<app>\ctl\<instance>.json` (Windows) or
`$XDG_CONFIG_HOME/<app>/ctl/<instance>.json` (Unix) — `<app>` being `docxy`,
`xlsxy`, or `yppxy` — where the instance is:

- `<app>-<AGWINTERM_SESSION_ID>` inside an agwinterm pane — and
  `AGWINTERM_SESSION_ID` **is the pane id** shown in `agwintermctl tree`, so an
  agent that knows the editor's pane id knows its discovery file exactly; or
- `<app>-<pid>` otherwise.

The file is `{"instance","port","token","pid"}`. Connect to `127.0.0.1:<port>`
and present `token` on every request. Stale files (editor gone) are swept the
next time any docxy starts; a client should also treat "connection refused" as
"not running" and move on.

## Protocol

One JSON object per line; one reply line per request:

```text
→ {"token":"…","verb":"doc.read","args":{"start":1,"end":3},"id":7}
← {"ok":true,"result":{ … },"id":7}
← {"ok":false,"error":"block 9 out of bounds","id":7}
```

`id` is optional and echoed back. Addressing is by **top-level block index**
(position in the document body); `doc.read`/`doc.outline` report each block's
`kind` so you know which indices are `paragraph`s — the ones the edit verbs take.

## Verbs

| Verb | Args | Result |
|---|---|---|
| `doc.path` | — | `{path, format, modified, blocks}` |
| `doc.outline` | — | `{headings:[{index, level, text}]}` |
| `doc.read` | `{start?, end?}` or `{range?:"a..b"}` (default: whole doc) | `{total, start, end, text, blocks:[{index, kind, text, heading?}]}` |
| `doc.find` | `{query, case_sensitive?}` | `{query, count, matches:[{path, start, end, block?, text?}]}` |
| `doc.replace-range` | `{start, end?, text}` | `{replaced, total}` |
| `doc.insert` | `{at, text}` | `{total}` |
| `doc.append` | `{text}` | `{total}` |
| `doc.save` | — | `{path, …}` |
| `doc.reload` | — | `{path, …}` (re-reads the file, dropping unsaved edits) |
| `doc.open` | `{path}` | `{path, …}` |

Notes:

- In `text`, `\n` separates paragraphs, so `doc.insert`/`doc.append`/
  `doc.replace-range` can add several paragraphs at once.
- Edit verbs require **paragraph** endpoints (not tables/raw); mid-range blocks
  of any kind are replaced.
- A `doc.replace-range` is a delete-then-insert — the same two undo steps as a
  paste over a selection in the UI.

## MCP (native tools in Claude Code)

`docxy --mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
stdio server that exposes the verbs as native tools — no shell glue, and Claude
Code's own permission prompts apply. It is a thin client of a running docxy
(discovered via the ctl directory above); it opens no document itself.

```bash
claude mcp add docxy -- docxy --mcp
```

Tools: `docxy_list`, `docxy_status`, `docxy_outline`, `docxy_read`, `docxy_find`,
`docxy_replace_range`, `docxy_insert`, `docxy_append`, `docxy_save`. Each edit
tool maps to the matching verb; results come back as JSON text. When several
docxy editors are open, pass `target` (a substring of the instance/pane id) to
pick one — `docxy_list` shows what's running. So the whole flow is: split the
pane, open a document in docxy, and ask Claude to "tighten the second paragraph
of my open document" — it calls `docxy_read` then `docxy_replace_range`, and you
watch the pane change live.

## Example (shell)

```bash
d=$APPDATA/docxy/ctl/docxy-$AGWINTERM_SESSION_ID.json     # docxy's pane, if it's your sibling
port=$(jq -r .port "$d"); tok=$(jq -r .token "$d")
send() { printf '{"token":"%s","verb":"%s","args":%s}\n' "$tok" "$1" "$2" | nc 127.0.0.1 "$port"; }

send doc.outline '{}'
send doc.read '{"start":1,"end":2}'
send doc.replace-range '{"start":1,"text":"A tighter second paragraph."}'
send doc.save '{}'
```

## The other editors

**xlsxy** (spreadsheet; A1-style refs/ranges, `sheet` by index or name):
`wb.path`, `sheet.list`, `sheet.read {sheet?, range?}`, `cell.get {ref}`,
`cell.set {ref, text}` (leading `=` = formula, validated + recalculated),
`range.clear {range}`, `find {query}`, `wb.recalc`, `wb.save`, `wb.reload`,
`wb.open {path}`. MCP: `claude mcp add xlsxy -- xlsxy --mcp` → `xlsxy_list`,
`xlsxy_status`, `xlsxy_sheets`, `xlsxy_read`, `xlsxy_get`, `xlsxy_set`,
`xlsxy_clear`, `xlsxy_find`, `xlsxy_recalc`, `xlsxy_save`. Skill:
`xlsxy install skill`.

**yppxy** (project schedule; tasks addressed by UID, durations like `3d`/`4h`):
`proj.path`, `task.list` (scheduled dates, critical path, slack, links),
`task.get/set/add/del`, `link.add {uid, pred, type?, lag?}` / `link.del`,
`find {query}`, `proj.save {path?}`, `proj.reload`, `proj.open {path}`. Edits
reschedule the plan (CPM) live. MCP: `claude mcp add yppxy -- yppxy --mcp` →
`yppxy_list`, `yppxy_status`, `yppxy_tasks`, `yppxy_get`, `yppxy_set`,
`yppxy_add`, `yppxy_del`, `yppxy_link`, `yppxy_unlink`, `yppxy_find`,
`yppxy_save`. Skill: `yppxy install skill`.

Everything else — discovery, the wire protocol, tokens, `target`
disambiguation, the status-dot flash on agent edits — works identically across
the three.
