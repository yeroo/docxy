# Driving docxy from an agent (the control surface)

docxy exposes a small **control surface** so an external agent — e.g. Claude Code
running in a sibling [agwinterm](https://github.com/yeroo/agwinterm) pane — can
read and edit the *live* open document. Edits go through docxy's own editor, so
they land on the **undo stack** and repaint the view instantly; reads reflect
**unsaved** changes, because they serialize the in-memory buffer, never the file
on disk.

The transport is loopback TCP speaking **newline-delimited JSON**, implemented by
the dependency-free [`ctlcore`](../ctlcore) crate (shared, so xlsxy/yppxy can
adopt it later).

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

On startup docxy writes a discovery file to
`%APPDATA%\docxy\ctl\<instance>.json` (Windows) or
`$XDG_CONFIG_HOME/docxy/ctl/<instance>.json` (Unix), where the instance is:

- `docxy-<AGWINTERM_SESSION_ID>` inside an agwinterm pane — and
  `AGWINTERM_SESSION_ID` **is the pane id** shown in `agwintermctl tree`, so an
  agent that knows docxy's pane id knows its discovery file exactly; or
- `docxy-<pid>` otherwise.

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
