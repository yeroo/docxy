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
| `doc.path` | — | `{path, format, modified, blocks, protection?, watermark?}` |
| `doc.outline` | — | `{headings:[{index, level, text}]}` |
| `doc.read` | `{start?, end?}` or `{range?:"a..b"}` (default: whole doc) | `{total, start, end, text, blocks:[{index, kind, text, heading?}]}` |
| `doc.find` | `{query, case_sensitive?}` | `{query, count, matches:[{path, start, end, block?, text?}]}` |
| `doc.replace-range` | `{start, end?, text}` | `{replaced, total}` |
| `doc.insert` | `{at, text}` | `{total}` |
| `doc.append` | `{text}` | `{total}` |
| `doc.save` | — | `{path, …}` |
| `doc.reload` | — | `{path, …}` (re-reads the file, dropping unsaved edits) |
| `doc.open` | `{path}` | `{path, …}` |
| `doc.export` | `{format:"markdown"\|"text"}` | `{format, text}` — the **live buffer** |
| `doc.export-pdf` | `{path}` | `{path}` (absolutized; refuses to overwrite — same `already exists:`/`bad path:`/`create failed:` error family as creating a new file) |
| `doc.comments` | — | `{comments:[{id,author,initials,date,text,anchor}]}` |
| `doc.notes` | — | `{notes:[{id,kind:"footnote"\|"endnote",text}]}` |
| `doc.header` / `doc.footer` | — | `{blocks:[{index,kind,text}]}` (empty list if the document has none) |
| `doc.metadata` | — | present-if-set keys: `{title?,author?,subject?,keywords?,comments?,last_saved_by?,revision?,created?,modified?}` |
| `doc.stats` | — | `{words, chars, paragraphs, blocks}` |
| `doc.replace-all` | `{query, text, case_sensitive?}` | `{replaced}` |
| `doc.undo` / `doc.redo` | — | `{done}` (`false` = nothing to undo/redo) |

Notes:

- In `text`, `\n` separates paragraphs, so `doc.insert`/`doc.append`/
  `doc.replace-range` can add several paragraphs at once.
- Edit verbs require **paragraph** endpoints (not tables/raw); mid-range blocks
  of any kind are replaced.
- A `doc.replace-range` is a delete-then-insert — the same two undo steps as a
  paste over a selection in the UI.
- **`doc.export` reads the live buffer.** Unlike opening the saved `.docx` in
  another tool, `doc.export`'s Markdown/text reflects **unsaved** edits —
  it serializes `editor.doc`, never the file on disk. This is the same
  live-buffer guarantee every read verb already has (`doc.read`, `doc.outline`,
  …); it's called out here because "export" more easily reads as "export the
  saved file" than "read" does. `xlsxy`'s `wb.export-csv` (below) is the same
  differentiator: both let an agent capture the document/workbook exactly as
  it currently stands, mid-edit, without a `doc.save`/`wb.save` first.
- **`doc.header`/`doc.footer` read the *default* section variant only.**
  A document can have distinct first-page and even-page headers/footers;
  those aren't surfaced by these verbs — only `app.headers.default`/
  `app.footers.default`.
- `doc.replace-all` and `doc.undo`/`doc.redo` no-op cleanly: a `query` that
  matches nothing, or an undo/redo on an empty stack, reports `replaced:0`/
  `done:false` and does **not** mark the document modified or flash the
  agent-status dot — nothing actually changed.

## MCP (native tools in Claude Code)

`docxy --mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
stdio server that exposes the verbs as native tools — no shell glue, and Claude
Code's own permission prompts apply. It is a thin client of a running docxy
(discovered via the ctl directory above); it opens no document itself, except
via `docxy_new`, which creates the file on disk before handing off to an
instance to open it.

```bash
claude mcp add docxy -- docxy --mcp
```

Tools: `docxy_list`, `docxy_new`, `docxy_status`, `docxy_outline`, `docxy_read`,
`docxy_find`, `docxy_replace_range`, `docxy_insert`, `docxy_append`,
`docxy_save`, `docxy_export`, `docxy_export_pdf`, `docxy_comments`,
`docxy_notes`, `docxy_header`, `docxy_footer`, `docxy_metadata`, `docxy_stats`,
`docxy_replace_all`, `docxy_undo`, `docxy_redo` (21 total). Each edit
tool maps to the matching verb — except `docxy_new`, which composes a file
create with a `doc.open` — and results come back as JSON text. When several
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

## VS Code tabs

The [`offxy` VS Code extension](../offxy-vscode) gives every open `.docx`/
`.xlsx` tab its own ctlcore-compatible control server
(`offxy-vscode/src/ctlserver.ts`) — discoverable and drivable exactly like a
terminal docxy/xlsxy pane: same discovery directory, same wire protocol, same
verb tables above (`doc.*` for Word tabs, the xlsxy verbs below for Excel
tabs). A tab's instance id is `<app>-vscode-<basename>-<pid>-<n>` (e.g.
`docxy-vscode-report_docx-4821-1`, where `4821` is the extension host's process
id), so it lists alongside terminal instances (`docxy-<pid>` /
`docxy-<AGWINTERM_SESSION_ID>`) in the same discovery dir and in
`docxy_list`/`xlsxy_list`. The pid keeps ids distinct across two VS Code
windows that open a same-basename file, which would otherwise mint the same
`<basename>-<n>` in both and clobber each other's discovery file. A tab exposes
**exactly** the terminal verb
surface, nothing more: a couple of internal-only verbs the extension host
uses to compose its own `doc.path`/`wb.path` replies (`doc.blocks`, `wb.info`)
are deliberately not in the tab's exposed verb set, and are rejected as
`"unknown verb"` — same as a terminal instance, which has no arm for them at
all.

Two behaviors differ from a terminal instance — worth knowing before
scripting against a tab:

- **`doc.open`/`wb.open` opens a new tab, not an in-place swap.** VS Code's
  per-tab document model has no equivalent of the terminal apps' single
  mutable "current document"; calling `doc.open`/`wb.open` on a tab's ctl
  instance opens the target file in its *own new tab* — a wholly separate ctl
  instance — instead of swapping the current instance's content the way the
  terminal apps do. An agent that opens a file via one instance and keeps
  issuing verbs to that *same* instance is still operating on the **old**
  file; it needs to re-resolve `target` (e.g. via `docxy_list`/`xlsxy_list`)
  to reach the instance for the file it just opened. A tab's
  `doc.open`/`wb.open` reply also carries just `{path}` (the path opened),
  whereas a terminal instance returns its full `doc.path`/`wb.path` info for
  the now-current document — a tab has no single "current document" to report.
- **`doc.reload` doesn't clear VS Code's dirty flag.** It re-reads the file
  from disk and repaints the tab with the fresh content (dropping unsaved
  edits, per its documented behavior) — but unlike VS Code's own "Revert
  File" command, there's no public API for a custom editor to clear the dirty
  indicator outside the edit-event path, which would wrongly put "reload" on
  the undo stack. So immediately after a `doc.reload`, the tab's title may
  still show the dirty dot even though its content now matches disk.

**Wave-1 additions on tabs** — every new verb above (docxy and xlsxy) is
reachable on a tab exactly like the original surface, with these mechanics
worth knowing:

- **`doc.undo`/`doc.redo` land as their own labeled entries on VS Code's undo
  stack**, not as a replay of whatever was already there. The wasm undo/redo
  runs immediately, and the tab fires a *new* edit event — labeled "Agent:
  undo"/"Agent: redo" — whose own undo/redo drives the **inverse** wasm op
  (agent `doc.undo` → the event's `undo()` sends a wasm redo, `redo()` sends a
  wasm undo), keeping VS Code's stack and the wasm stack in lockstep with no
  private API. A `{done:false}` no-op (nothing to undo/redo) fires no event.
  Every other mutating verb's edit event is labeled "Agent: `<verb>`" (e.g.
  "Agent: range.set", "Agent: comment.add").
- **Agent `sheet.remove` undo restores the sheet's content, comments, and
  sheet-scoped defined names — but re-appends it at the END of the tab's
  sheet order**, not back at its original index. Sheets below the removed one
  don't shift back, so a workbook with sheets `[A, B, C]` where an agent
  removes `B` and the user then presses Ctrl+Z ends up `[A, C, B]`, not the
  original `[A, B, C]`.
- **Agent sheet-removals are single-level-undoable.** The restore is backed by
  a single-slot stash (only the most recently removed sheet is recoverable);
  a *second* consecutive `sheet.remove` followed by two undos succeeds on the
  first (restoring the second removal) but the second undo shows a warning
  (`"Offxy: couldn't undo … — nothing left to reverse"` or "… nothing to
  restore") instead of silently failing or reviving the first removed sheet.
- **An agent `sheet.remove`/`sheet.import-csv` invalidates earlier grid
  edits' undo entries.** These verbs clear the workbook's own undo history
  (mirroring the terminal apps, whose package-parts churn can't be represented
  as a stack entry), so any grid edits made *before* one of them can no longer
  be reversed on the wasm stack. VS Code still holds their edit-event entries,
  so pressing Ctrl+Z past that point reports success but changes nothing.
  (Per-edit epoch tracking that would surface this as a real warning is a
  disclosed fast-follow.)
- **Comment author defaults to `"agent"` on tabs**, not the OS username — the
  terminal apps stamp new threaded comments with the OS user
  (`$USER`/`%USERNAME%`, falling back to `"xlsxy"`); a tab's `comment.add`
  with no `author` arg stamps `"agent"` instead, since there's no terminal
  session to read a username from.
- **`doc.export-pdf` on a tab is written by the extension host, not the
  wasm.** `docxcore`'s PDF exporter is std-only and can't run inside the wasm
  sandbox, so the webview renders the PDF bytes and hands them to the
  extension host, which does the exclusive-create write to disk (same
  refuses-to-overwrite / `already exists:` semantics as the terminal, which
  writes directly). The reply shape is identical either way: `{path}`. Pass an
  **absolute** `path`: a relative one absolutizes against the serving process's
  cwd, which differs between a terminal instance and the extension host.

`docxy_new`/`xlsxy_new` on a tab instance opens the created document as a
**new** tab (same as `doc.open`/`wb.open`, above); with no tab alive, the
file is still created on disk but nothing opens (`"opened":false`). The
reply's `instance` names the tab that *handled the open* and still serves its
**old** document — not the fresh tab the new file landed in (found via
`docxy_list`/`xlsxy_list`) — so don't reuse it as `target` for follow-up verbs
on the new file.

See the [extension's README](../offxy-vscode/README.md#ai-assistants) for how
to point an AI assistant at these tabs (Copilot: automatic; Claude Code: a
one-liner).

## The other editors

**xlsxy** (spreadsheet; A1-style refs/ranges, `sheet` selects by index or
name and defaults to the active sheet):

| Verb | Args | Result |
|---|---|---|
| `wb.path` | — | `{path, modified, sheets, active, active_name}` |
| `sheet.list` | — | `{active, sheets:[{index, name, rows, cols}]}` |
| `sheet.read` | `{sheet?, range?}` | `{sheet, name, rows, cols, cells:[…], truncated}` |
| `cell.get` | `{ref, sheet?}` | `{ref, row, col, value, formula?, text}` |
| `cell.set` | `{ref, text, sheet?}` | `{ref, value, text, …}` — leading `=` is a formula, validated + recalculated |
| `range.clear` | `{range, sheet?}` | `{cleared}` |
| `find` | `{query, sheet?}` | `{query, count, matches:[…]}` |
| `wb.recalc` | — | `{recalculated:true}` |
| `wb.save` | — | `{path, …}` |
| `wb.reload` | — | `{path, …}` (re-reads the file, dropping unsaved edits) |
| `wb.open` | `{path}` | `{path, …}` |
| `comment.list` | — | `{comments:[{sheet,ref,author,text}]}` (threads flattened in reply order) |
| `wb.export-csv` | `{sheet?}` | `{sheet, csv}` — display-formatted RFC-4180, the **live buffer** |
| `sheet.pivot` | `{range,rows:[col],cols?:[col],values:[{col,agg}],sheet?}` | `{table:[[string]]}` — **ad-hoc and read-only**, no workbook mutation |
| `formula.eval` | `{formula,ref?,sheet?}` | `{value,text}` — side-effect-free preview, writes nowhere |
| `sheet.stats` | `{range,sheet?}` | `{sum,count,countNums,average,min,max}` |
| `chart.list` | — | `{charts:[{kind,title?,categories,series:[{name?,values}]}]}` |
| `pivot.list` | — | `{pivots:[{sheet,rows,cols,values}]}` (persistent pivots, summarized) |
| `comment.add` | `{ref,text,author?,sheet?}` | `{sheet,ref}` |
| `comment.remove` | `{ref,sheet?}` | `{removed:bool}` |
| `range.set` | `{start,rows:[[string]],sheet?}` | `{set:N}` — **atomic**: every formula validated first, any invalid → error and nothing applied; one undo group |
| `sheet.import-csv` | `{text,name?}` | `{sheet,name,rows,cols}` — always a **new** sheet, never overwrites |
| `wb.replace-all` | `{query,text}` | `{replaced}` — spans **all sheets**, one undo group |
| `sheet.add` | `{name?}` | `{sheet,name}` — deduplicates a taken name, never errors |
| `sheet.remove` | `{sheet}` | `{removed:true}` (errors on the last sheet; `sheet` is required, no active-sheet default) |
| `sheet.rename` | `{sheet,name}` | `{name}` — rewrites formula/defined-name references |
| `row.insert` / `row.delete` | `{at,count?,sheet?}` | `{inserted\|deleted:N}` |
| `col.insert` / `col.delete` | `{at,count?,sheet?}` | `{inserted\|deleted:N}` |

Notes:

- **`wb.export-csv` reads the live buffer** — same live-buffer guarantee as
  `doc.export` above: it reflects unsaved edits, not the saved file.
- **`sheet.pivot` is read-only and ad-hoc.** It computes a grid straight from
  a snapshot of `range` and never writes a persistent pivot table into the
  workbook — `pivot.list` (also read-only) lists *existing* persistent
  pivots, a separate thing.
- **`wb.replace-all` spans every sheet** in the workbook, unlike a find/replace
  scoped to one sheet — the whole multi-sheet edit lands as a single undo
  group.
- `sheet.remove`/`sheet.rename` require `sheet` explicitly (not defaulted to
  the active sheet) — a destructive or renaming op shouldn't silently land on
  "whichever sheet happens to be showing".

MCP: `claude mcp add xlsxy -- xlsxy --mcp` → `xlsxy_list`, `xlsxy_new`,
`xlsxy_status`, `xlsxy_sheets`, `xlsxy_read`, `xlsxy_get`, `xlsxy_set`,
`xlsxy_clear`, `xlsxy_find`, `xlsxy_recalc`, `xlsxy_save`, `xlsxy_comments`,
`xlsxy_comment_add`, `xlsxy_comment_remove`, `xlsxy_range_set`,
`xlsxy_export_csv`, `xlsxy_import_csv`, `xlsxy_pivot`, `xlsxy_replace_all`,
`xlsxy_sheet_add`, `xlsxy_sheet_remove`, `xlsxy_sheet_rename`,
`xlsxy_row_insert`, `xlsxy_row_delete`, `xlsxy_col_insert`,
`xlsxy_col_delete`, `xlsxy_eval`, `xlsxy_stats`, `xlsxy_charts`,
`xlsxy_pivots` (30 total). Skill: `xlsxy install skill`.

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
