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

## Desktop suite: Project tabs

The desktop `suite` serves Project control automatically on normal startup,
using the same `ctlcore` protocol and the shared `projctl` editor verbs as
`yppxy`. Startup continues if the listener cannot be created. Word and Sheet
control verbs are not exposed by this server.

Discovery is `<config root>/suite/ctl/suite-<id>.json`, with `<id>` taken from
`AGWINTERM_SESSION_ID` or the process ID. The config root is `DOCXY_CONFIG_DIR`
when set and non-empty, otherwise the OS config directory (normally
`%APPDATA%` on Windows). The override moves both control discovery and session
state. In `--harness` mode only the isolated harness server runs; it also
accepts the Project verbs. A harness instance ignores `AGWINTERM_SESSION_ID`
and is always `suite-<pid>` (#697). Normal control does not expose harness operations
such as `open`, `key`, `type`, or `quit`.

Every verb except `proj.open` and `proj.new` accepts optional `tab`: an absolute zero-based
index among **all** tabs, or a case-insensitive substring of a Project tab's
title/path. Omit it to use the active tab. Ambiguous strings, empty strings,
invalid indices, non-Project targets, and failed-load placeholders are errors.
Explicit targets do not activate that tab. Task/link arguments use stable
**UIDs**, as reported by `task.list`, not the table's displayed IDs.

Supported verbs are `proj.path`, `task.list`, `task.get`, `task.fields`, `task.set`,
`task.add`, `task.del`, `link.add`, `link.del`, `find`, `assign.list`,
`assign.get`, `assign.fields`, `assign.add`, `assign.set`, `assign.del`,
`proj.save`, `proj.reload`, `proj.open` and `proj.new`, with the yppxy
argument/result shapes. `proj.path` additionally
reports `tab`, `imported`, `cell` (active column name), `cell_row` (zero-based row),
and `cell_edit` (pending cell buffer, or `null` when closed). Reads and rejected
edits leave selection, prompts, pending cell edits, history and scroll unchanged.
Successful edits use the live editor's undo stack, reschedule, cancel the target's
prompt, discard its uncommitted cell edit, and repaint. Successful `proj.reload`
also discards that tab's pending cell edit. `task.del` on a summary deletes its
whole subtree as one undo step, without the confirmation the Project tab shows,
and lists every removed UID in `removed`. `task.set` takes `manual: true|false`
to switch a task between Manually and Auto Scheduled: a task that becomes manual
is pinned at its shown start and finish, one that becomes auto is placed by its
links and constraints again. It may be combined with `name`, `duration` and
`level` in one undo step; every task in `task.list`/`task.get` reports `manual`.

Every task in `task.list`, `task.get` and `find` also reports its row `id` and
its `outline_number` (`1.2`; `0` for the project summary row, `null` for a
blank row). To read any other field, pass `fields`, a list of Project's own
field names; `task.fields` returns `{count, fields}`, every name this build can
read, so a test can tell "not supported" from "empty":

```json
{"uid": 2, "fields": ["% Complete", "Actual Start", "Baseline1 Finish", "Total Slack"]}
```

Each task then carries `fields: {"<name as asked>": {"text": …, "value": …}}`.
Names match ignoring ASCII case and surrounding space. An unknown name, or a
`fields` that is not a list of strings, fails the whole call
(`unknown task field 'Bogus'`). The verbs that reply with a task (`task.set`,
`task.add`, `link.add`, `link.del`) take `fields` too and check it before they
edit anything, so a bad list leaves the plan and its undo history unchanged. The fields are the Entry columns (ID, Task
Mode, Name, Duration, Start, Finish, Predecessors, Resource Names), % Complete,
% Work Complete, Physical % Complete, Actual Start/Finish/Duration/Work/Cost,
Remaining Duration/Work/Cost, Work, Cost, Fixed Cost, Fixed Cost Accrual,
Baseline and Baseline1–Baseline10 Start/Finish/Duration/Work/Cost, Start,
Finish, Duration, Work and Cost Variance, Total, Free, Start and Finish Slack,
Early and Late Start/Finish, Critical, Constraint Type and Date, Deadline,
Active, Outline Number, Outline Level, WBS, Leveling Delay, Type, Effort Driven,
Priority, Notes, Milestone, Summary, Estimated, Status and Unique ID. Status is
measured at the plan's StatusDate, else its CurrentDate, and is empty when the
plan has neither. Earned-value and custom fields are not readable yet.

- `text` is what the sheet shows. The Entry columns use the grid's own text,
  which the harness `cell` verb also returns (`2d`, `2026-03-02`); the other
  fields use Project's spellings: `0 days`, `1 day`, `-1 day`, `1.25 days`,
  `2 wks`, `2 edays`, `4 hrs` (work is always hours), `$1,400.00`,
  `($40,000.00)`, `50%`, `Yes`/`No`. Dates show `YYYY-MM-DD`, or `NA` when
  unset, so Duration `2d` can sit next to Actual Duration `2 days`.
- `value` is what lies underneath: dates `YYYY-MM-DD HH:MM`; durations, work
  and slack signed minutes; money a number of currency units; percents
  integers; flags booleans; enums (Task Mode, Constraint Type, Type, Fixed
  Cost Accrual, Status) their display names; text strings. It is `null` only
  for a date that shows `NA`, for a stored quantity the plan does not have
  (unset % Complete reads `"0%"` and `null`; a stored 0 reads `"0%"` and `0`),
  and for Status when the plan has no StatusDate or CurrentDate (or the task
  no start), which then reads `""` and `null`. Fields
  with a Project default read the default: Active Yes, Priority 500, Type and
  Effort Driven the plan's new-task defaults, Fixed Cost Accrual Prorated.
- Variances follow the live schedule, not the values a file stores. They
  measure the scheduled (unleveled) dates, which a save writes and Set
  Baseline records; while leveling is on, the grid's Start and Finish can
  differ from them. Start and Finish Variance are working minutes from the
  Baseline date to the scheduled one (0 without a baseline), and Duration, Work and Cost Variance are the
  current value less the Baseline one (an absent Baseline value counts as 0).
  Slack comes from the schedule, and total slack can be negative.

Assignments are addressed by their own UID. `assign.list {uid?, resource?}`
lists every assignment, or those of task `uid` and/or of `resource` (a uid, or
a name matched ignoring ASCII case; an unknown one is an error, `no resource
named 'Nobody'`). `assign.get {uid}` reads one. Each reports `uid, task,
resource, resource_name, units, work_hours, regular_work_hours,
overtime_work_hours, cost, rate_table, baseline_work_hours, baseline_cost,
actual_work_hours, remaining_work_hours, actual_cost, remaining_cost,
percent_work_complete, start, finish, delay_hours, contour`: units a fraction
(1.0 = 100%; a material's quantity), work in hours, money in currency units,
`null` for a value the plan does not store, `rate_table` a letter (`A` when
unset), `start`/`finish` the assignment's own dates.

```json
{"verb": "assign.add", "args": {"task": 3, "resource": "Ann", "units": "50%"}}
{"verb": "assign.set", "args": {"uid": 7, "units": 1, "rate_table": "B"}}
{"verb": "assign.del", "args": {"uid": 7}}
```

`assign.add {task, resource, units?, work?}` takes a resource uid or name (a new
name is staged as a work resource; a new name that is a number or contains a
comma is refused, so pass a uid as a number) and refuses a resource already on
the task. `assign.set {uid, units?, work?, rate_table?,
delay?}` needs at least one of them; units are a number or `"50%"`, work and
delay a number of hours or a duration (`"40h"`, `"5d"`), the rate table `"A"` to
`"E"`. The task is rescheduled by its type, as in Project: on a Fixed Units task
a units edit keeps the work and moves the duration, on Fixed Duration the work
follows the units (and units given together with work are recomputed from the
work); a work edit clears the assignment's overtime. A bare number of work or
delay, even as a string (`"8"`), is hours. `assign.del
{uid}` replies `{deleted, task}`; an effort-driven task keeps its work across
the assignments left. Each edit is one undo step, a value the assignment
already has records none, and every argument is checked first, so a rejected
call leaves the plan untouched. The assign verbs take `fields` with Project's
assignment field names (`assign.fields` lists them: Unique ID, Task ID, Task
Name, Resource Name, Units, Work, Regular, Overtime, Actual, Remaining and
Baseline Work, Cost, Actual, Remaining and Baseline Cost, % Work Complete,
Start, Finish, Delay, Cost Rate Table, Work Contour, Peak, Budget Work and
Budget Cost); `{"Work": {"text": "40 hrs", "value": 2400}, "Units": {"text":
"100%", "value": 1}}`. Peak and the budget fields read what the plan stores.

File handling differs from the TUI:

- `proj.save {"path"?: "..."}` never opens a dialog. Without a path it saves
  `.yppx`/`.xml` in place; imported `.mpp` and untitled projects require a path.
  Explicit paths accept `.yppx`/`.xml`, add `.yppx` when extensionless, and
  reject `.mpp`. Saving commits a valid pending cell edit first; invalid cell
  input blocks the write. A subsequent I/O failure preserves the file binding
  and retains the committed edit in memory, including its dirty flag and history.
  Saves and exports normally write and sync a temporary sibling before replacing
  the destination. A failed temporary write leaves the previous file intact. On
  Unix, if the original owner/group cannot be restored on the temp, the synced
  bytes are written back into the original file to preserve ownership; an I/O
  failure during that fallback can leave partial output. Exports refuse
  destinations that identify the source file, including symlinks and hard links.
- `proj.reload` discards dirty content only after a successful load from its
  current path. Failure preserves the entire tab. Untitled tabs cannot reload.
- `proj.open {"path":"..."}` does not accept `tab`. It validates a Project
  file before changing anything, focuses a loaded same-path tab without
  replacing unsaved content, recovers a same-path placeholder when loading now
  succeeds, or appends a new tab. Activation uses the normal tab lifecycle.
- `proj.new {}` appends a blank Project tab and activates it through the same
  handler as the Backstage › New › Project card, so the plan is the app's
  blank one: `Untitled.yppx`, named `Project1`, no tasks, no path. It replies
  with `proj.path` for the new tab. It takes neither `tab` nor `name`; save the
  plan with `proj.save {"path":"..."}` to give it a file. It is refused while a
  dialog is open on the active tab.

For example, the repository's CLI sends raw control requests (PowerShell;
use your explicit config root in place of `$env:APPDATA` when overridden):

```powershell
target/debug/uiharness.exe --ctl "$env:APPDATA/suite/ctl" call proj.path '{}'
target/debug/uiharness.exe --ctl "$env:APPDATA/suite/ctl" call task.list '{"tab":"schedule.xml"}'
target/debug/uiharness.exe --ctl "$env:APPDATA/suite/ctl" call task.set '{"tab":"schedule.xml","uid":2,"duration":"3d"}'
```

Add `--instance suite-<id>` when several suite processes are running. The
equivalent Rust client resolves with
`ctlcore::client::resolve_target(&ctl_dir, "suite", Some("<id>"))`, then calls
`client.call("task.list", Json::obj(vec![]))`.

MCP clients can reach these Project tabs through `yppxy --mcp` using the existing
`yppxy_*` tools. `yppxy_list` combines live `yppxy-*` instances from yppxy's
control directory and live `suite-*` instances from the suite's control directory;
rows include `app` (`yppxy` or `suite`). With one live instance it is selected
automatically. With several, pass `target`, an instance/pane-id substring; ambiguity
is checked across both applications. An absent discovery directory is ignored.
Set `DOCXY_CONFIG_DIR` in the MCP process too if the suite uses that override.
The suite directory uses the OS config directory (or `.` if unavailable) otherwise.

Every instance-addressed `yppxy_*` tool accepts optional `tab` with the suite
selector rules above. For example, `yppxy_tasks` with
`{"target":"suite-","tab":"schedule.xml"}` reads that live Project tab, and
`yppxy_set` with `{"target":"suite-","tab":2,"uid":7,"duration":"3d"}` edits
UID 7 in tab 2. Passing `tab` to a standalone yppxy instance is an error.
The bridge does not add Word/Sheet tools or a suite skill.

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
| `doc.find` | `{query, case_sensitive?}` | `{query, count, matches:[{path, start, end, block?, text?}]}` — `start`/`end` are editor offsets (see notes) |
| `doc.replace-range` | `{start, end?, text, markdown?}` | `{replaced, total}` |
| `doc.insert` | `{at, text, markdown?}` | `{total}` |
| `doc.append` | `{text, markdown?}` | `{total}` |
| `doc.save` | — | `{path, …}` |
| `doc.reload` | — | `{path, …}` (re-reads the file, dropping unsaved edits) |
| `doc.open` | `{path}` | `{path, …}` |
| `doc.compare` | `{original, revised}` | `{path, insertions, deletions, skipped:[{kind, index?, revision?}]}` — Review ▸ Compare: opens a new, unsaved `Compare Result N.docx` (beside the revised file) whose tracked changes turn the original into the revised `.docx`; neither source is written. Refuses while the open document has unsaved changes. `skipped` kinds: `table`, `object`, `formatting` (original property markup whose namespace prefix the revised document binds differently; reported once), `note-ref`, `unsupported-revision`, `paragraph-mark` |
| `doc.export` | `{format:"markdown"\|"text"}` | `{format, text}` — the **live buffer** |
| `doc.export-pdf` | `{path}` | `{path}` (absolutized; refuses to overwrite — same `already exists:`/`bad path:`/`create failed:` error family as creating a new file) |
| `doc.comments` | — | `{comments:[{id,author,initials,date,text,anchor}]}` |
| `doc.notes` | — | `{notes:[{id,kind:"footnote"\|"endnote",text}]}` |
| `doc.header` / `doc.footer` | — | `{blocks:[{index,kind,text}]}` (empty list if the document has none) |
| `doc.metadata` | — | present-if-set keys: `{title?,author?,subject?,keywords?,comments?,last_saved_by?,revision?,created?,modified?}` |
| `doc.stats` | — | `{words, chars, paragraphs, blocks}` |
| `doc.replace-all` | `{query, text, case_sensitive?}` | `{replaced}` |
| `doc.undo` / `doc.redo` | — | `{done}` (`false` = nothing to undo/redo) |
| `doc.format` | `{start, end?, patch}` | `{formatted}` — block count; ONE undo checkpoint over the whole range |
| `doc.set-style` | `{start, end?, style?, align?}` | `{styled}` — block count; ONE undo checkpoint |
| `doc.revisions` | — | `{count,revisions:[…]}` in document order, including stable target, kind, metadata, support state, nesting, and editor-safe locations |
| `doc.revision-current` | — | `{count,revision}` for the navigation selection or change at the caret; `revision:null` when none |
| `doc.revision-next` / `doc.revision-previous` | — | `{count,revision}` after selecting the wrapping next/previous change |
| `doc.revision-accept` / `doc.revision-reject` | `{revision}` | structured applied/stale/unsupported/malformed outcome |
| `doc.revisions-accept-all` / `doc.revisions-reject-all` | — | `{total,applied,outcomes:[…]}` from one undoable transaction |

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
- **`doc.find` and `doc.replace-all` see only the text the editor can edit.**
  A paragraph's editable text is its runs, tabs and breaks, including those
  inside hyperlinks, plus one offset (U+FFFC) for each field that shows a
  result: a field is edited as one unit, as in Word. Text it only shows is not
  searched or replaced: tracked changes (`w:ins`/`w:del`), field results,
  footnote/endnote references, equations, SmartArt, chart titles and inline
  text boxes, including any of these inside a hyperlink. A link's bookmarks and
  proofing marks (`w:proofErr`) don't hide its text, and an edit keeps them in
  place. A match's `start`/`end` count only editable text (a field counting
  one), while `text` is the paragraph's full
  plain text (the form `doc.replace-range` round-trips), which includes the
  rest. So `text[start..end]` is the match only when the paragraph holds
  nothing but editable text; otherwise don't splice `text` at those offsets.
  A text box's own paragraphs are still searched and replaced, as matches
  under their own `path` (with no `block`/`text`). The editor's own Find bar
  (a person's, not these verbs) also shows the text it only draws, as
  read-only matches it can go to but never replaces.
- `doc.replace-all` and `doc.undo`/`doc.redo` no-op cleanly: a `query` that
  matches nothing, or an undo/redo on an empty stack, reports `replaced:0`/
  `done:false` and does **not** mark the document modified or flash the
  agent-status dot — nothing actually changed.

### DOCX protection and watermark behavior

`doc.path` reports a human-readable `protection` value when a document declares
one: `read-only` (including password-backed write protection), `comments only`,
`formatting locked`, `form fields only`,
`tracked changes only`, `restricted editing`, or the advisory
`read-only (recommended)`. The optional `watermark` value contains the first
applied text watermark, or `picture (preview unavailable)` /
`unsupported (preview unavailable)` when no text preview exists. These fields
are status hints; mutation policy uses parsed OOXML metadata rather than these
labels.

The terminal TUI and control dispatcher use the same five mutation classes.
MCP edit tools are thin mappings to those control verbs, so they inherit the
same checks and do not maintain a second policy table:

| Protection state | Content | Structure | Formatting | Comments | Package metadata |
|---|---:|---:|---:|---:|---:|
| absent, disabled, or recommendation-only write protection | allow | allow | allow | allow | allow |
| password-backed write protection | deny | deny | deny | deny | deny |
| enforced read-only | deny | deny | deny | deny | deny |
| enforced comments-only | deny | deny | deny | allow | deny |
| enforced formatting-only | allow | allow | deny | allow | allow |
| enforced forms-only | deny | deny | deny | deny | deny |
| enforced tracked-changes-only | deny | deny | deny | deny | deny |
| enforced unknown mode | deny | deny | deny | deny | deny |

The current mutating control/MCP operations cover Structure
(`doc.replace-range`, `doc.insert`, `doc.append`), Content (`doc.replace-all`,
`doc.undo`, `doc.redo`, `doc.revision-accept`, `doc.revision-reject`,
`doc.revisions-accept-all`, `doc.revisions-reject-all`), and Formatting
(`doc.format`, `doc.set-style`). There
is no comment-writing control verb yet, so comments-only protection denies all
current automation edits even though comment mutations in the TUI are allowed.
Markdown control/MCP inserts that carry styles, numbering, or direct run
formatting additionally require Formatting authorization.
Read, navigation, inspection, export, same-format save, open/reload, compare, and
new-file operations remain available. Cross-format Save As in the TUI is package
metadata and is protected; same-format save only persists already-authorized
changes and is not a new mutation. When nothing was edited, the original main
document XML is preserved verbatim rather than regenerated.

Denied control and MCP requests return
`protection_denied:<stable_code>: <explanation>`, where `<stable_code>` is one
of `read_only`, `comments_only`, `formatting_locked`, `forms_unsupported`,
`tracked_changes_unsupported`, or `unsupported_mode`. The check runs before
argument parsing and before any mutation, so a rejected request does not alter
the document, package parts, caret, undo/redo stacks, dirty state, or save
state. Invalid arguments and unknown verbs retain their existing non-protection
errors.

Forms-only, tracked-changes-only, and unknown enforced modes deliberately fail
closed. docxy cannot yet make conforming form-field-only edits or automatically
record ordinary edits as tracked changes; use Word for those edits until those
editing models exist. Recommendation-only `w:writeProtection` is different: the
TUI shows a warning and `doc.path` reports `read-only (recommended)`, but all
edits remain allowed. A password/hash-backed declaration is enforced read-only
until docxy can verify the password.

In page view, applied text watermarks are rendered as muted page overlays using
the correct inherited default/first/even header. They are not part of document
text, selection, copy/export, caret/hit testing, or saved OOXML. Picture and
unsupported watermarks receive a page-associated `preview unavailable`
fallback. The status fields above remain available to control/MCP clients; the
overlay itself is visual TUI state and is not returned as document content.
The exhaustive route mapping is maintained in
[`docx-mutation-inventory.md`](docx-mutation-inventory.md).

### Tracked-change review

`doc.revisions` is the discovery call for review automation. Its `revision`
values are document-local stable ids encoded as strings; pass one unchanged to
`doc.revision-accept` or `doc.revision-reject`. Each entry reports `ordinal`,
`depth`, `kind`, `supported`, `current`, `start`, and `end`, with `parent`,
`scope`, `unsupported_kind`, `id`, `author`, and `date` when applicable.
Navigation wraps and updates the live editor's review selection; it does not
edit the document or create an undo entry.

Supported kinds are insertions, deletions, and property changes in run,
paragraph, table, row, cell, and section scopes. Accepting an insertion keeps
its content; rejecting it removes the content. Accepting a deletion removes its
content; rejecting it restores ordinary content. Accepting a property change
keeps current properties, while rejecting restores the prior snapshot. A
`paragraph mark insertion`/`paragraph mark deletion` is a tracked change of a
paragraph mark: accepting a mark deletion or rejecting a mark insertion joins the
paragraph with the next paragraph in its container (taking that paragraph's
properties), so top-level block indices shift — refresh them from `doc.outline`
or `doc.read` afterwards. Nested bulk changes are transformed innermost-first and reported in their original
document order. See [`docx-revision-inventory.md`](docx-revision-inventory.md)
for the complete fidelity and exclusion contract.

A successful single action is one undo step. An all-action applies every
supported initial target as one undo step and returns a per-target outcome;
unsupported or malformed records remain untouched. Applied results have
`status:"applied"`, `revision`, `action`, and `kind`. No-op results use
`status:"error"` and a structured `error.code`: `stale_revision`,
`unsupported_revision`, or `malformed_revision`. Refresh `doc.revisions` after
an action instead of reusing a removed target.

Review actions use the central Content mutation policy. In particular,
enforced tracked-changes-only protection does not grant permission to make
ordinary untracked edits or to accept/reject imported changes; those requests
fail with `protection_denied:tracked_changes_unsupported`. Reads and review
navigation remain available.

### Markdown-formatted writes

`doc.insert`, `doc.replace-range`, and `doc.append` all take an optional
`markdown` boolean (default `false`). `markdown:false` (or the arg omitted)
is byte-identical to today's plain-text behavior — `text` becomes one
paragraph per `\n`-separated line. `markdown:true` parses `text` as Markdown
and splices the resulting **blocks** (headings, styled runs, lists, tables,
…) into the body at the same position the plain-text form would target, into
the document's **existing** content — not a fresh package. Replies are
unchanged (`{total}` / `{replaced, total}`); undo-step parity with the
plain-text form is preserved (`insert`/`append` = one undo step;
`replace-range` = two steps when the replaced range is non-empty, one when
it's empty — same as plain text on the same range). An empty/whitespace-only
`text` that parses to zero blocks errors `"empty markdown"` and touches
nothing (no splice, no undo entry, no dirty flag). Undoing a markdown write
reverts the spliced *content*, but any style/numbering definitions it ensured
(`Heading1`, a list's numbering part, …) remain in the package — deliberate,
since ensures aren't checkpointed onto the undo stack.

Every construct below was verified spliced into an **existing** document
(not just a freshly generated one), including its round-trip through
`doc.export {format:"markdown"}`:

| Construct | Result |
|---|---|
| Headings `#`..`######` | Works — styles auto-ensured. |
| Bold / italic / strike | Works. |
| Inline code | Works structurally (round-trips); the `Code` character style is **not** auto-ensured (see below) — inline code never renders monospace in the TUI or PDF today regardless, a pre-existing, unrelated gap. |
| Links | Works. |
| Nested bullet lists | Works — all 9 indent levels are ensured (not just the top one), so a nested item gets a real marker, not a stray numeral. |
| Nested ordered lists | Works — all 9 indent levels are ensured (not just the top one), so a nested item gets a real marker, not a stray numeral. |
| Tables | Works. |
| Blockquote | Works — styles auto-ensured. |
| Horizontal rule | Works. |
| Fenced code (generic) | Works — styles auto-ensured. |
| Fenced code with a language tag | Same as generic fenced code; the language tag itself isn't preserved (pre-existing `from_markdown` limitation, unrelated to this feature). |
| `$inline math$` | Works. |
| `$$display math$$` | Works. |
| ` ```mermaid ` fences | Works. |

14 rows covering all 15 spec-listed constructs (`Bold / italic / strike`
bundles three) land correctly — nothing degrades silently. "Styles
auto-ensured" means: when a markdown write references a paragraph style the
target `.docx` doesn't already define (`Heading1`–`Heading6`, `Quote`,
`SourceCode`), the write injects that style's definition into `styles.xml`
first (strictly additive — an existing definition with the same id is left
byte-untouched), so headings/blockquotes/fenced code render correctly in
Word even when spliced into a package that never had those styles, not just
one built fresh from Markdown. The one deliberate exception is `Code`: it's
a run-level *character* style (`w:rStyle`), not a paragraph style, so it
falls outside this auto-ensure mechanism — inline code's `<w:rStyle
w:val="Code"/>` reference is written on save regardless, but the style
definition itself is only ensured for the six paragraph styles above.

### Formatting and styles

`doc.format {start, end?, patch}` applies direct run-level formatting to
every run in the block range `[start, end]` (`end` default `start`). Both
endpoints must be **paragraphs** (the same `require_para` rule other range
verbs use); a table block mid-range is skipped (untouched) but still counted
toward `formatted`. `patch` is an object with at least one of these eight
keys:

| Key | Type | Notes |
|---|---|---|
| `bold` | boolean | **set-to-value**, not toggle — `bold:true` on an already-bold run is a no-op on that run |
| `italic` | boolean | set-to-value |
| `underline` | boolean | set-to-value |
| `strike` | boolean | set-to-value |
| `color` | string | `"#RRGGBB"` |
| `highlight` | string | one of `yellow`, `green`, `cyan`, `magenta`, `red`, `blue`, `lightGray`, `darkYellow`, or `"none"` (clears the highlight) |
| `font` | string | any font-name string, unvalidated |
| `size` | number | points, fractional allowed (e.g. `10.5`) |

Errors mirror `cell.format`'s family: an empty patch → `"patch needs at
least one key"`; an unknown key → `"unknown patch key '<key>'"`; a malformed
value → a key-specific message (`"bad color '<v>' (want \"#RRGGBB\")"`,
`"bad highlight '<v>' (want one of yellow, green, cyan, magenta, red, blue,
lightGray, darkYellow, or none)"`, `"bad size '<v>' (want a positive number
of points)"`). `{formatted:N}` is
the number of blocks in `[start, end]` (tables included in the count even
though they're skipped structurally); a patch that changes nothing (e.g.
reapplying an already-set value) still checkpoints, matching `cell.format`'s
own always-snapshot behavior. **ONE undo checkpoint per call**, regardless
of how many keys the patch carries or how many blocks the range spans.

`doc.set-style {start, end?, style?, align?}` requires at least one of
`style`/`align` (`"set-style needs 'style' or 'align'"` otherwise, both
omitted). `style` accepts the Wave-2 markdown paragraph-style set —
`Heading1`–`Heading6`, `Quote`, `SourceCode` — plus `Normal`, which clears
the paragraph back to the default style. An unknown id errors naming it and
listing the full accepted set. Applying any of the seven non-`Normal` styles
runs the same `ensure_styles` mechanism Wave-2's markdown writes use
(strictly additive), so the paragraph actually renders styled in Word even
in a package that never defined that style before — `Normal` skips this,
since it only clears a reference rather than requiring one. `align` accepts
`left`, `center`, `right`, `justify` (`"bad align '<v>' (want
left/center/right/justify)"` otherwise). **ONE undo checkpoint per call**,
whether `style`, `align`, or both are given together.

On a `.md` (markdown-editor) tab, `doc.format`'s `underline`/`size` keys and
`doc.set-style`'s `align` still apply to the live buffer and report success —
but Markdown has no syntax for underline, alignment, or font size, so the next
save (`docx_to_md`) silently drops them; an agent formatting a markdown tab
this way won't see the change survive a save/reload.

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
`docxy_replace_all`, `docxy_undo`, `docxy_redo`, `docxy_format`,
`docxy_set_style`, `docxy_revisions`, `docxy_revision_current`,
`docxy_revision_next`, `docxy_revision_previous`, `docxy_revision_accept`,
`docxy_revision_reject`, `docxy_revisions_accept_all`,
`docxy_revisions_reject_all`, and `docxy_compare` (32 total). Each edit
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
surface, nothing more (except xlsxy's `wb.properties`/`wb.set-properties` and the
page-layout and printing verbs (`page.*`, `print-area.*`, `print-titles.set`,
`page-break.*`, `print.pages`, `wb.export-pdf`), which only a terminal xlsxy
answers so far; a tab answers `unknown verb`): a couple of internal-only verbs the extension host
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
- **Agent `sheet.remove` undo restores the sheet's content, comments,
  sheet-scoped defined names, and any pivot table registrations that lived
  on it — but re-appends it at the END of the tab's sheet order**, not back
  at its original index. Sheets below the removed one don't shift back, so a
  workbook with sheets `[A, B, C]` where an agent removes `B` and the user
  then presses Ctrl+Z ends up `[A, C, B]`, not the original `[A, B, C]`.
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

**Wave-2 additions on tabs** — markdown-formatted writes and the two new
xlsxy formatting verbs behave exactly like their terminal counterparts, with
one undo-mechanics distinction worth knowing:

- **Markdown writes and `cell.format` are true undo-stack entries** — a
  markdown `doc.insert`/`doc.append`/`doc.replace-range` and a `cell.format`
  both land on the same wasm undo-stack group their plain-text/`range.set`
  counterparts do, so a single <kbd>Ctrl+Z</kbd> undoes the whole write (one
  step for insert/append, matching the plain-text step count for
  replace-range; one step for `cell.format` regardless of how many cells the
  range covered).
- **`col.width` undoes via an inverse, like `comment.add`/`comment.remove`**
  — it is not on the wasm undo stack at all (matching the TUI's own `F7`/`F8`
  width keys), so the tab drives it the same host-orchestrated way Wave-1's
  comment verbs work: the wasm reply carries the prior width as a
  self-describing inverse `col.width` call, and the tab's "Agent: col.width"
  edit event's own undo/redo applies that inverse (and the inverse's own
  reply carries a fresh inverse back to the width just replaced, so redo
  keeps working indefinitely) — rather than an on-stack undo replay.

**Wave-3 additions on tabs** — both new docxy verbs and `pivot.create`
behave exactly like their terminal counterparts, with the same undo-bucket
split as the rest of the surface:

- **`doc.format` and `doc.set-style` are each a true wasm undo-stack
  entry**, like `cell.format` before them — a single <kbd>Ctrl+Z</kbd> undoes
  the whole call (every block in the range, every patch key, together),
  regardless of how many blocks or keys it touched.
- **`pivot.create` undoes via the same inverse mechanism as
  `sheet.import-csv`/`sheet.remove`, not the wasm undo stack.** Its declared
  inverse is `sheet.remove` on the sheet it just created, so a single
  <kbd>Ctrl+Z</kbd> removes the new sheet AND the pivot registration
  together — both-or-neither, never a dangling pivot entry or an orphaned
  empty sheet. Redoing that removal (<kbd>Ctrl+Shift+Z</kbd>/<kbd>Ctrl+Y</kbd>)
  restores BOTH the sheet and the pivot, via the same restore path
  `sheet.remove`/`sheet.restore-removed` already use — the restored pivot's
  output is immediately correct and keeps refreshing on subsequent
  `wb.recalc` calls, same as any other pivot.

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

## JetBrains tabs

Documents open in the [offxy-jetbrains](../offxy-jetbrains) plugin advertise
on this same surface: instance `docxy-jetbrains-<basename>-<pid>-<n>` in
docxy's ctl dir (pid keeps two IDE windows on a same-basename file distinct,
as with VS Code tabs), identical wire protocol and token semantics. Any
`docxy --mcp` session lists IDE tabs next to terminal panes and VS Code tabs;
disambiguate with `target` as usual. The `doc.*` verb surface is served by
the same `docx_ctl` engine the VS Code tabs use; host verbs
(`doc.path`/`doc.save`/`doc.reload`/`doc.open`) are answered by the IDE.
Differences from a terminal pane:

- **`doc.open` opens a new tab**, not an in-place swap — same caveat as
  VS Code tabs above: re-resolve `target` to reach the new file's instance.
- **`doc.reload` drops unsaved edits** (terminal semantics) and, unlike a
  VS Code tab, correctly clears the IDE's dirty indicator.
- **Mutating verbs land as one IDE undo step each** (a snapshot-backed
  entry) — an agent edit is one Ctrl+Z away, interleaved cleanly with the
  user's own typing.
- **`doc.undo`/`doc.redo` are rejected** ("undo is IDE-owned…"): the tab's
  undo stack belongs to the platform; driving the engine's internal stack
  from outside would desync them. Agents undo by asking the user, or by
  making the inverse edit.
- **`doc.export-pdf` is not yet implemented** on JetBrains tabs (the
  host-side exclusive-create write is a follow-up); `doc.export`
  (markdown/text of the live buffer) works.
- `doc.blocks` is internal (composes `doc.path`) and answers
  `unknown verb 'doc.blocks'` externally, same as every other surface.

**Excel tabs** (`xlsxy-jetbrains-<basename>-<pid>-<n>` in xlsxy's ctl dir)
serve the full xlsxy verb surface through `grid_ctl` (except
`wb.properties`/`wb.set-properties` and the page-layout and printing verbs,
terminal xlsxy only for now: a tab answers `unknown verb`), with the same host-verb
split (`wb.path`/`wb.save`/`wb.reload`/`wb.open`; `wb.open` opens a new tab;
`wb.info` internal). Every mutating agent verb lands as **one IDE undo step**
driving the engine's own undo stack — the same mechanism the grid UI uses,
detected generically via the engine's `edits` counter. Divergence from
VS Code tabs: an agent `sheet.remove` is not restorable from the IDE's undo
(no single-slot stash wiring yet); `sheet.restore-removed` still works over
the wire.

## The other editors

**xlsxy** (spreadsheet; A1-style refs/ranges, `sheet` selects by index or
name and defaults to the active sheet):

| Verb | Args | Result |
|---|---|---|
| `wb.path` | — | `{path, modified, read_only, sheets, active, active_name, circular}` — `read_only` is true while the workbook is bound to the file `xlsxy --read-only` opened (terminal xlsxy; see `wb.save`);  `circular` lists the cells on circular references (active sheet first, bare `E1`; other sheets as `Sheet2!A1`), empty when there are none. It lists them whether or not the workbook enables iterative calculation (the TUI's warning and footer note appear only when it does not, as in Excel). Without iterative calculation those cells are 0, as in Excel |
| `sheet.list` | — | `{active, sheets:[{index, name, rows, cols}]}` |
| `sheet.read` | `{sheet?, range?}` | `{sheet, name, rows, cols, cells:[…], truncated}` |
| `cell.get` | `{ref, sheet?}` | `{ref, row, col, value, formula?, text, format?}` — `format` is present only if the cell has non-default styling (see below) |
| `cell.set` | `{ref, text, sheet?}` | `{ref, value, text, …}` — typed the way the grid types it: leading `=` is a formula, validated + recalculated; numbers, currency, percents, fractions, dates and times are recognised (a General cell takes the matching number format); a Text-formatted cell keeps the text as typed; a leading `'` stores the rest as text with `quotePrefix`; more than 32,767 characters is refused and the cell is left as it was; an edit that would change part of a legacy CSE array (not its anchor) is refused with `cell.set: You can't change part of an array.` and nothing changes |
| `range.clear` | `{range, sheet?}` | `{cleared}`; a clear that would change part of a legacy CSE array (not its anchor) is refused with `range.clear: You can't change part of an array.` and nothing changes |
| `cell.format` | `{range, patch, sheet?}` | `{formatted}` — cell count; ONE undo group over every cell in `range` |
| `col.width` | `{col, width, sheet?}` | `{col, width}` — `col` accepts a letter or a 0-based index; the reply always echoes the **numeric** index |
| `find` | `{query, sheet?}` | `{query, count, matches:[…]}` |
| `wb.recalc` | — | `{recalculated:true}` |
| `wb.properties` | — | **Terminal xlsxy only for now** (a VS Code or JetBrains tab answers `unknown verb`). `{title, tags, categories, subject, comments, company, manager, hyperlinkBase, author, lastModifiedBy, created, modified, custom:[{name, type, value}]}` — the document properties File › Info shows; an absent one is `null`; a custom `type` is `text`/`number`/`bool`/`date`/`other` (a variant type xlsxy does not model, its raw XML as `value`) |
| `wb.set-properties` | `{title?, tags?, categories?, subject?, comments?, company?, manager?, hyperlinkBase?, custom?: {name: value\|null}}` | Terminal xlsxy only for now. `wb.properties` + `{changed}`. Only the given keys change; `null` or `""` removes one. A custom value is a string (text), a number, a bool (yes/no) or `{"date":"YYYY-MM-DD[THH:MM:SSZ]"}`; a name that exists (in any case) changes in place, keeping its position. An unknown key is an error and nothing changes, as is a change that would go into a `docProps/core.xml` or `app.xml` xlsxy can't read (e.g. UTF-16): `document properties can't be edited: <part> is unreadable`. Marks the workbook modified when something changed; not on the undo stack (Excel's Info edits aren't either). Author, Last Modified By, Created and Modified are read-only: every `wb.save` stamps Modified (UTC) and Last Modified By (the OS user name, as for comments), and Author and Created are set on a save only when the file has none (its first save, typically) |
| `wb.save` | — | `{path, …}`; a failed write answers `ok:false` with `save failed: …` (the status-bar text) and the workbook stays modified. When the workbook has Excel's *Always create backup* (`<workbookPr backupFile="1">`), the file being replaced is first kept as `Backup of <stem>.xlk` in the same folder (replacing an older backup); a backup that cannot be written fails the save with `save failed: …` naming the backup, and the workbook file untouched. Under `xlsxy --read-only` (`-r`), a save while bound to that file answers `ok:false` with `"<name>" is read-only. Save a copy under a new name.` and writes nothing; nothing else xlsxy writes (a Save As, a text type's supporting files, a backup, File › Export) may land on that file either |
| `wb.reload` | — | `{path, …}` (re-reads the file, dropping unsaved edits; a file opened with `--read-only` stays refused). A workbook last saved as CSV UTF-8, CSV (Comma delimited), Text (Tab delimited) or Unicode Text is re-imported from that file (no dialog) and stays bound to it and its type. One last saved as Formatted Text (`.prn`) or Web Page is refused ("… cannot be read back; reload is not available for this file") and nothing changes |
| `wb.open` | `{path}` | `{path, …}`; a `.csv`/`.tsv` opens as Excel opens it, and a `.txt`/`.prn` is imported with the Text Import Wizard's defaults (tab-delimited, General columns) without showing the wizard — use `sheet.import-text` for other options. Under `xlsxy --read-only`, opening that file again (or any other) keeps it refused to `wb.save`. An opened `.csv`/`.tsv`/`.txt`/`.prn` is **rebound to `<name>.xlsx`**, or to `<name>1.xlsx`, `<name>2.xlsx`, … when that exists: `wb.save` writes that workbook, never the text file, and never replaces an existing workbook (the first save moves on to the next free name if the bound one has appeared since the open). A load that fails is the verb's error |
| `comment.list` | — | `{comments:[{sheet,ref,author,text}]}` (threads flattened in reply order) |
| `wb.export-csv` | `{sheet?}` | `{sheet, csv}` — display-formatted, Excel's CSV text (CR LF records, LF inside a quoted field; a file adds the UTF-8 BOM), the **live buffer** |
| `sheet.pivot` | `{range,rows:[col],cols?:[col],values:[{col,agg}],sheet?}` | `{table:[[string]]}` — **ad-hoc and read-only**, no workbook mutation |
| `pivot.create` | `{range,rows:[col],cols?:[col],values:[{col,agg}],name?,sheet?}` | `{sheet,name}` — builds a REAL, persistent workbook pivot on a NEW sheet |
| `formula.eval` | `{formula,ref?,sheet?}` | `{value,text}` — side-effect-free preview, writes nowhere |
| `sheet.stats` | `{range,sheet?}` | `{sum,count,countNums,average,min,max}` |
| `chart.list` | — | `{charts:[{kind,title?,categories,series:[{name?,values}]}]}` |
| `pivot.list` | — | `{pivots:[{sheet,rows,cols,values}]}` (persistent pivots, summarized) |
| `comment.add` | `{ref,text,author?,sheet?}` | `{sheet,ref}` |
| `comment.remove` | `{ref,sheet?}` | `{removed:bool}` |
| `range.set` | `{start,rows:[[string]],sheet?}` | `{set:N}` — each string typed like `cell.set`; **atomic**: every formula and length validated first, any invalid (a bad formula, or an entry over 32,767 characters) → an error naming the cell and nothing applied; a group that would change part of a legacy CSE array (not its anchor) is refused with `range.set: You can't change part of an array.` and nothing applied; one undo group |
| `sheet.import-csv` | `{text,name?}` | `{sheet,name,rows,cols}` — always a **new** sheet, never overwrites ; fields convert as opening a `.csv` does (a `sep=` first line, typed-entry dates/percentages/formulas, 15 digits, File › Options › Data) |
| `sheet.import-text` | `{text\|path,options?,name?}` | `{sheet,name,rows,cols}` — the Text Import Wizard without the dialog, into a **new** sheet. `options`: `kind` (`delimited`/`fixed`), `delimiters` (`tab`, `semicolon`, `comma`, `space` or a character), `consecutive`, `qualifier` (`"`, `'`, `none`), `breaks`, `start_row`, `origin` (`auto`, `utf-8`, `utf-16le`, `windows-1252`), `columns` (`general`, `text`, `date:dmy`…, `skip`), `decimal`, `thousands`, `trailing_minus` |
| `app.options` | `{convert_leading_zeros?,convert_long_numbers?,convert_e_notation?,convert_dates?,edit_fixed_decimal?,edit_fixed_decimal_places?,edit_move_after_enter?,edit_move_direction?,edit_in_cell?,edit_autocomplete?}` | every option — File › Options › Data › Automatic Data Conversion (four booleans, followed by the next `.csv`/text open and `sheet.import-csv`/`sheet.import-text`) and Advanced › Editing (booleans, places a whole number -300..=300, direction `down`/`right`/`up`/`left`); every given key is checked before any is set; saved with the app's preferences on exit. The fixed decimal point shifts only numbers typed into the grid, never `cell.set`/`range.set` |
| `range.text-to-columns` | `{range,options?,dest?,replace?,sheet?}` | `{rows}` — Data › Text to Columns on **one** column with the same `options`; refuses with "Do you want to replace the contents of the destination cells?" unless `replace:true`; one undo step |
| `wb.consolidate` | `{refs,dest?,fn?,top?,left?,links?}` | Terminal xlsxy only for now. `{sheet,range}` — Data › Consolidate: `refs` are source references such as `East!A1:C4` or `'My sheet'!$A$1:$D$9` (a bare range is on the destination's sheet; other workbooks and defined names are not supported); `dest` is the output's top-left cell, `Sheet!B2` or `B2` (default: the cursor on the active sheet); `fn` is a dialog name or file token, case-insensitive (`Sum` by default, `Count`, `Average`, `Max`, `Min`, `Product`, `Count Numbers`/`countNums`, `StdDev`, `StdDevp`, `Var`, `Varp`); `top`/`left` match rows and columns by the labels in each source's top row / left column (case-insensitive, whole label, any order); `links` writes `=Sheet!$B$2` detail formulas in hidden outline rows under a summary formula, and is refused for a source on the destination sheet. A refusal (no or bad references, a source overlapping the output) changes nothing. The settings are kept on the destination sheet (`<dataConsolidate>`). One undo step |
| `wb.replace-all` | `{query,text}` | `{replaced}` — spans **all sheets**, one undo group; the match runs on each cell's own input text (never on the `'` a quote prefix adds, which is kept on the result), and the replaced text is re-read with the entry rules of `cell.set`, except that a percent cell's number constant is not divided again; a result over the cell limit leaves that cell as it was |
| `sheet.add` | `{name?}` | `{sheet,name}` — deduplicates a taken name, never errors |
| `sheet.remove` | `{sheet}` | `{removed:true}` (errors on the last sheet; `sheet` is required, no active-sheet default) |
| `sheet.rename` | `{sheet,name}` | `{name}` — rewrites formula/defined-name references |
| `table.list` | — | Terminal xlsxy only for now. `{tables:[{name,sheet,sheet_name,ref,columns,header_rows,totals_rows}]}` |
| `table.rename` | `{name,new}` | Terminal xlsxy only for now. The renamed table, as `table.list` shows it. Excel's name rules (letter, `_` or `\` first; letters, digits, `_`, `.`; not a cell reference; unique among tables and defined names). Every formula naming it, defined names, rules, PivotTable sources and the data model follow. One undo step |
| `table.resize` | `{name,ref}` | Terminal xlsxy only for now. The table. The header row stays, the new range overlaps the old one, keeps a data row ("A table needs at least one data row") and covers no other table, PivotTable or array formula; a table with a Total Row keeps its bottom row ("Turn off the Total Row first"). A new column is named from its header cell (or `ColumnN`), made unique ignoring case (`Qty` beside `Qty` becomes `Qty2`), and that name is written into the header cell. One undo step |
| `table.convert` | `{name}` | Terminal xlsxy only for now. `{converted}` — Convert to Range: structured references become cell references (`$B$2:$B$9`, `$B5` for `[@Col]`), the table part leaves the file at the next save. Refused while a PivotTable, a SUMX-style formula or the data model uses the table. One undo step |
| `row.insert` / `row.delete` | `{at,count?,sheet?}` | `{inserted\|deleted:N}` |
| `col.insert` / `col.delete` | `{at,count?,sheet?}` | `{inserted\|deleted:N}` |
| `page.setup` | `{sheet?\|sheets?, …fields}` | Terminal xlsxy only for now. The sheet's page layout. Settable fields: `margins:{left,right,top,bottom,header,footer}` (inches), `paperSize` (1 Letter, 9 A4, …), `orientation` (`portrait`/`landscape`/`default`), `scale`, `fitToPage`, `fitToWidth`, `fitToHeight` (0 = Automatic; absent in the file means 1), `firstPageNumber` (`null` = Auto), `pageOrder`, `blackAndWhite`, `draft`, `cellComments`, `errors`, `gridLines`, `headings`, `horizontalCentered`, `verticalCentered`, `differentOddEven`, `differentFirst`, `scaleWithDoc`, `alignWithMargins`. Reply-only (set them with their own verbs): `headers:{oddHeader…firstFooter}` (stored codes; `page.header`), `printArea` (`print-area.*`), `printTitles:{rows,cols}` (`print-titles.set`), `rowBreaks`, `colBreaks` (`page-break.*`). Any settable field given sets it and the reply adds `changed`; a reply-only key is an unknown field; the save rewrites only the attributes that changed. A fit count turns `fitToPage` on and `scale` turns it off, unless `fitToPage` is given. Scale outside 10–400, a fit count over 32767, a negative margin or an unknown field is an error and nothing changes. With `sheets`, the fields go to the first sheet and its page setup is then copied to the others, as Page Setup on grouped sheets does: print areas and titles stay each sheet's own, header pictures (`&G`) are not copied. One undo step |
| `page.header` | `{sheet?, kind?:odd\|even\|first, part?:header\|footer, left?, center?, right?}` | Terminal xlsxy only for now. `{stored, left, center, right, changed}`. The three sections in the header editor's form: `&[Page]`, `&[Pages]`, `&[Date]`, `&[Time]`, `&[Path]`, `&[File]`, `&[Tab]` are stored as Excel's `&P`, `&N`, `&D`, `&T`, `&Z`, `&F`, `&A`; `&&` stays a literal ampersand; formatting codes (`&"Arial,Bold"`, `&12`) pass through. With none of `left`/`center`/`right` it only reads; when setting, an absent section is empty and all three empty removes the header. A section over 255 characters is refused, and so is `&L`, `&C` or `&R` inside a section (it would start another; type a literal ampersand as `&&`). `&[Picture]` is accepted only where the section already shows a picture (header pictures can't be inserted yet). One undo step |
| `print-area.set` / `print-area.add` | `{range, sheet?}` | Terminal xlsxy only for now. `{printArea, changed}` — `range` is `A1:C10`, several ranges `A1:C10,E1:F5`, whole columns `A:C` or rows `1:5`; written as `Sheet1!$A$1:$C$10,…`. `add` appends to the print area (or sets one). One undo step |
| `print-area.clear` | `{sheet?}` | Terminal xlsxy only for now. `{printArea:null, changed}`; the save removes the `_xlnm.Print_Area` name. One undo step when there was one |
| `print-titles.set` | `{rows?, cols?, sheet?}` | Terminal xlsxy only for now. `{printTitles:{rows,cols}, changed}` — rows to repeat at top (`"1:2"`) and columns at left (`"A:A"`), written columns first; an absent key keeps that part, `null` or `""` clears it. They repeat on every page that doesn't already show them, except that titles that would fill a page by themselves, at the print scale, don't repeat (our rule; the spec is silent). One undo step |
| `page-break.insert` / `page-break.remove` | `{cell, sheet?}` | Terminal xlsxy only for now. `{rowBreaks, colBreaks, changed}` — the manual breaks' ids (a row break with id 13 starts its page at row 14). Insert at a cell adds a break above it (unless it is in row 1) and left of it (unless it is in column A); remove takes the manual breaks bordering it. One undo step when something changed |
| `page-break.reset` | `{sheet?}` | Terminal xlsxy only for now. `{rowBreaks, colBreaks, changed}` — Reset All Page Breaks: every manual break goes, automatic ones stay |
| `print.pages` | `{what?, sheet?\|sheets?, range?, ignorePrintAreas?, from?, to?}` | Terminal xlsxy only for now. `{total, pages:[{sheet, name, range, number, titleRows, titleCols, scale}]}` — the pages printing lays out. `what`: `active` (default; `sheets`, `sheet` or the active sheet, each starting new pages), `workbook` (every visible sheet) or `selection` (`range` of `sheet`, comma-separated ranges each on pages of their own). A print area's ranges each start new pages; hidden rows and columns don't print, nor do hidden sheets with `workbook` (a hidden sheet named by `sheet`/`sheets`, or active, prints); title rows/columns repeat on pages that don't show them, except that titles that would fill a page by themselves, at the print scale, don't repeat; numbering continues across sheets and honours a first page number; `from`/`to` pick pages by position (1-based), `total` counts them all. A selected range prints only as far as its printed cells, and one with none prints nothing. A job over 100,000 pages is an error, `This would print more than 100000 pages; set a print area or select less.`, as for `wb.export-pdf` |
| `wb.export-pdf` | `{path, …print.pages args}` | Terminal xlsxy only for now. `{path, pages}` — the job as PDF (the active sheet by default), the **live buffer**; `&D`/`&T` are the local date and time. Refuses to overwrite (`already exists: …`). Nothing to print errors with `We didn't find anything to print.` and writes no file, and so does a job over 100,000 pages (`print.pages`' error). Text outside Windows-1252 prints as `?` (standard PDF fonts). Doesn't mark the workbook modified |

Notes:

- **`wb.export-csv` reads the live buffer** — same live-buffer guarantee as
  `doc.export` above: it reflects unsaved edits, not the saved file.
- **`sheet.pivot` is read-only and ad-hoc.** It computes a grid straight from
  a snapshot of `range` and never writes a persistent pivot table into the
  workbook — `pivot.list` (also read-only) lists *existing* persistent
  pivots, a separate thing. `pivot.create` (below) is the mutating
  counterpart that actually creates one.
- **`wb.recalc` also refreshes every persistent pivot table**, not just
  formulas — a source-cell edit followed by `wb.recalc` recomputes any
  pivot's output sheet along with the rest of the recalc. Cost scales with
  the number of pivots in the workbook, not just the number of dirty cells.
- **`wb.replace-all` spans every sheet** in the workbook, unlike a find/replace
  scoped to one sheet — the whole multi-sheet edit lands as a single undo
  group.
- `sheet.remove`/`sheet.rename` require `sheet` explicitly (not defaulted to
  the active sheet) — a destructive or renaming op shouldn't silently land on
  "whichever sheet happens to be showing".

### Cell formatting

`cell.format`'s `patch` is an object with at least one of these six optional
keys — an empty or all-unknown-key patch is an error (below), and setting a
key applies it to every cell in `range`; keys left out of the patch leave
that aspect of each cell's existing style untouched:

| Key | Type | Notes |
|---|---|---|
| `numFmt` | string | a number-format code, as `numfmt::parse_format` accepts |
| `bold` | boolean | |
| `italic` | boolean | |
| `fontColor` | string | `"#RRGGBB"` |
| `fillColor` | string | `"#RRGGBB"` |
| `align` | string | `"left"` \| `"center"` \| `"right"` |

Errors: an empty patch → `"patch needs at least one key"`; an unknown key →
`"unknown patch key '<key>'"` naming the offending key; a malformed value for
a known key → a key-specific message (e.g. `"bad numFmt code '<code>'"`,
`"bad color '<value>' (want \"#RRGGBB\")"`). A rejected patch applies
nothing. `col.width`'s `width` is a fractional **number** (Excel
column-width units, e.g. `20.5`), not an integer; a non-positive width
errors `"col.width: 'width' must be positive"`.

`col.width`'s undo behavior differs from `cell.format`'s: `cell.format` lands
on the same true undo-stack group `range.set` uses (one undo step, all
formatted cells restored together). `col.width` is **not** on the undo
stack at all (matching the TUI's own `F7`/`F8` width keys) — the wasm/tab
surfaces instead carry the prior width as a self-describing inverse (see
"VS Code tabs" below), the same pattern Wave-1 used for `comment.add`/
`comment.remove`.

### Format read-back (`cell.get` only)

`cell.get`'s reply gains an additive, present-if-set `format` object echoing
whichever of the six `patch` keys above differ from the cell's style
defaults — an unstyled cell (or one explicitly reset back to the default for
every key it touched) has **no** `format` key at all, not an empty object.
This read-back is deliberately scoped to `cell.get` **only**: `sheet.read`,
`find`, and `cell.set`'s own reply never carry a `format` key, even for a
heavily styled cell, to keep bulk reads and the busiest mutating verb lean.

The "differs from default" rule has one subtlety: `numFmt` compares by
**classification**, not by raw stored code string, specifically so a real
loaded `.xlsx`'s implicit `numFmtId="0"` ("General") — present on every
unstyled cell in any file Excel actually wrote — never echoes as
`numFmt:"General"`. The other five fields have no equivalent implicit
default-but-present value, so they compare directly against the workbook's
default style. One consequence: explicitly patching `numFmt:"General"` as a
deliberate reset also echoes nothing afterward, matching how the other five
fields already behave when reset to their default.

### Persistent pivots

`pivot.create` takes the same arg shape as `sheet.pivot` above (first row of
`range` is the header, `rows`/`cols` name grouping columns, `values` is
`[{col,agg}]` using the same 11 aggregation strings and the same
unknown-header error family — `"pivot.create: unknown column '<col>'"`),
plus an optional `name`: the **destination sheet's** name (default: a
generated `PivotN`, unique among existing sheet names; an explicit name that
collides with any existing sheet errors `"pivot.create: sheet name '<name>'
is already taken"`). No value fields at all errors `"pivot.create needs at
least one value field"`.

Unlike `sheet.pivot`, this builds a REAL, persistent workbook pivot table
via the TUI's own pivot-creation machinery — not an ad-hoc computed grid —
and lands its output on a **new** sheet, exactly mirroring where the TUI
would place it. Reply: `{sheet, name}` — `sheet` is the new destination
sheet's index, `name` is its name. The created pivot immediately shows up in
`pivot.list`, its output is refreshed by `wb.recalc` like any other pivot,
and — the one engine question this feature was probed against before
shipping — **it survives `wb.save` → reload**: a saved and reopened
workbook's pivot definition and refresh both keep working (proven by a
create → save → reload → refresh round-trip test before the verb shipped;
had the write path proved incomplete, `pivot.create` would have shipped as
an honest error instead of a silently session-only pivot).

Undo is a **history-clear + host-orchestrated inverse** — the same bucket
`sheet.import-csv`/`sheet.remove` use — not a true undo-stack entry. The
inverse is `sheet.remove` on the newly created destination sheet, which
removes the pivot registration along with the sheet: `sheet.remove`'s own
cascade drops a pivot's parts/registration whenever its destination sheet is
removed, so no separate `pivot.remove` verb exists or is needed — removing
the pivot's sheet is both-or-neither by construction. See "VS Code tabs"
below for how this plays out through the tab's inverse-based undo.

MCP: `claude mcp add xlsxy -- xlsxy --mcp` → `xlsxy_list`, `xlsxy_new`,
`xlsxy_status`, `xlsxy_sheets`, `xlsxy_read`, `xlsxy_get`, `xlsxy_set`,
`xlsxy_clear`, `xlsxy_find`, `xlsxy_recalc`, `xlsxy_save`, `xlsxy_comments`,
`xlsxy_comment_add`, `xlsxy_comment_remove`, `xlsxy_range_set`,
`xlsxy_export_csv`, `xlsxy_import_csv`, `xlsxy_pivot`, `xlsxy_replace_all`,
`xlsxy_sheet_add`, `xlsxy_sheet_remove`, `xlsxy_sheet_rename`,
`xlsxy_row_insert`, `xlsxy_row_delete`, `xlsxy_col_insert`,
`xlsxy_col_delete`, `xlsxy_eval`, `xlsxy_stats`, `xlsxy_charts`,
`xlsxy_pivots`, `xlsxy_format`, `xlsxy_col_width`, `xlsxy_pivot_create`,
`xlsxy_properties`, `xlsxy_set_properties`, `xlsxy_page_setup`,
`xlsxy_page_header`, `xlsxy_print_area_set`, `xlsxy_print_area_add`,
`xlsxy_print_area_clear`, `xlsxy_print_titles`, `xlsxy_page_break_insert`,
`xlsxy_page_break_remove`, `xlsxy_page_break_reset`, `xlsxy_print_pages`,
`xlsxy_export_pdf` (46 total; docxy's 31 + xlsxy's 46 = **77 tools** total
across both apps).
Skill: `xlsxy install skill`.

**yppxy** (project schedule; tasks addressed by UID, durations like `3d`/`4h`):
`proj.path`, `task.list` (scheduled dates, critical path, slack, links),
`task.get/set/add/del`, `task.fields` (`task.list`/`task.get`/`find` take
`fields: [...]` to read any listed field by Project's name), `link.add {uid, pred, type?, lag?}` / `link.del`,
`find {query}`, `assign.list/get/add/set/del` and `assign.fields` (a task's
resource assignments, by assignment UID), `proj.save {path?}`, `proj.reload`,
`proj.open {path}`. Edits
reschedule the plan (CPM) live. MCP: `claude mcp add yppxy -- yppxy --mcp` →
`yppxy_list`, `yppxy_status`, `yppxy_tasks`, `yppxy_get`, `yppxy_set`,
`yppxy_fields`, `yppxy_add`, `yppxy_del`, `yppxy_link`, `yppxy_unlink`,
`yppxy_find`, `yppxy_save` (the task tools take `fields`). Skill: `yppxy install skill`.

Everything else — discovery, the wire protocol, tokens, `target`
disambiguation, the status-dot flash on agent edits — works identically across
the three.
