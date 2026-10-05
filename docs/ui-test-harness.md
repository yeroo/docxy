# The scripted UI test harness

Every visual change in the desktop suite used to be verified by a person
installing a build and looking at it. That is why two regressions — a
drag-to-select that ran auto-fill, and dashes on an ordinary selection —
reached an installer before anyone noticed. Neither was reachable from a unit
test: both live in event handling and rendering, and gpui `#[test]` blows up the
moment a view is constructed.

The harness drives the app by verbs, screenshots it, and asserts on the pixels,
in an instance that **cannot touch the real one**.

```text
test drag-to-select does not fill
  open ../fixtures/basic.xlsx
  click A1
  snapshot A1:C5
  drag A1 -> C5
  assert range is A1:C5
  assert no fill preview
  assert cells unchanged
  shot cell:A1:C5
```

Two halves, in two workspaces:

| Half | Where | What it does |
|---|---|---|
| the app side | `suite/docxy/src/harness.rs` | an opt-in [`ctlcore`](../ctlcore) server: the verbs, the state reply, and the region geometry |
| the driver side | `uiharness/` (root workspace) | the script format, the launcher, `PrintWindow` capture, the pixel probes, the runner |

## Running the committed cases

```bash
cargo build --release --manifest-path suite/Cargo.toml   # the harness drives a built suite
cargo run -p uiharness -- run uiharness/cases/sheet-selection.uit
```

That one command starts a sandboxed instance, runs every case in the file
against it, files a PNG per `shot`, and stops it again. It exits non-zero if any
case failed. Windows needs a desktop session for `PrintWindow`. Linux can run
on a private virtual display, including on CI; see [Capture on Linux](#capture-on-linux).

Output is a line per step, passing or not, because a transcript that mentions
only the failure leaves you guessing whether the setup even worked:

```text
case: drag-to-select does not fill — FAILED
  ok    open ../fixtures/basic.xlsx
  ok    drag A1 -> C5
  FAIL  assert no fill preview
        expected: no fill armed and nothing previewed
        observed: filling=true, fill_preview=A1:C5
```

Useful flags on `run`:

| Flag | Effect |
|---|---|
| `--suite EXE` | drive this binary. Otherwise `$UIHARNESS_SUITE` if it is set (an error if it does not name a file), otherwise the first build found — release before debug, under `suite/target/` then `target/`, relative to the repository this crate was built from and then the working directory. Release wins over debug whatever their dates, so every run prints the binary it drove as its first line — that is what makes a stale release build answering for a fresh debug one visible |
| `--run DIR` | where evidence is filed (default `./uiharness-runs`, which is git-ignored). The path a capture lands on is `<case>/<line>-<region>.png`, so two runs sharing this directory *at the same time* would file over each other's pictures — give them different `--run` directories if you run them in parallel. The instances themselves never share anything: each gets its own sandbox |
| `--sandbox DIR` | the throwaway config root (default `<run>/sandbox-<pid>-<timestamp>`). Every default is unique and retained for diagnosis; the harness performs no recursive cleanup. A directory you name yourself is also yours to manage and is kept |
| `--keep` | leave the instance up after the script ends, to poke at the window a case failed on |
| `--desktop NAME` | run the suite on a separate Win32 desktop, never shown — see below |

### On a separate desktop

gpui shows the suite's window and takes the foreground when it starts, which
on a long pass can steal your keyboard mid-sentence. `--desktop NAME` starts
the suite on a Win32 desktop of its own — `WinSta0\NAME`, created if it is not
there, reused if it is — that is never switched in: nothing appears on your
screen and nothing can take the focus. Off Windows the flag is an error.

Captures still work. Window enumeration is per-desktop, so the harness
attaches its own thread to `NAME` before it touches the instance: the run's
`shot`/`window`/`assert` and any later
`uiharness --config <sandbox> --desktop NAME …` see the window exactly as if
it were on yours. `--keep` leaves the instance reachable only this way — a
plain `uiharness --config <sandbox> window` from your desktop reports "no
visible top-level window", which is the truth: the window is not on your
desktop.

The launch retargets the suite's stdout and stderr at
`<sandbox>/suite-output.log` (`STARTF_USESTDHANDLES`) and creates no console
for it (`CREATE_NO_WINDOW`), so its output goes to the log rather than your
terminal; when the instance exits before connecting, `run` quotes that log's
tail, so a refusal from the isolation gate stays visible.

The desktop buys no more than that. The clipboard belongs to the window
station, not the desktop, so a clipboard case on the separate desktop still
reads and writes yours.

### Evidence

Every capture lands under the run directory in a folder named for the test that
took it, so a failing case can be looked at afterwards:

```text
uiharness-runs/drag-to-select-does-not-fill/007-cell-a1-c5.png
```

Names are slugged (`cell:A1:C5` → `cell-a1-c5`) — a colon in a Windows path is a
stream separator, and a capture named straight from a region would silently
write somewhere else. The number is the step's line in the script: two steps may
look at the same region, and without it the later capture would overwrite the
one the failure above it points at.

### Crash log

The suite (harness or not) appends every panic, on any thread, to
`<config root>/docxy/crash.log`: the sandbox for a harness instance, otherwise
`%APPDATA%\docxy\crash.log`. The same file gets the start-up failures `main`
otherwise only prints: the harness control server failing to start and
"Project control unavailable". The release build has no console, so this file
is often the only place a panic shows up. An entry has a UTC timestamp, pid,
version, thread, message, `file:line:col` and a backtrace. Past 256 KiB the log
moves to `crash.log.1`. A stack overflow, a native or GPU access violation, an
abort and `taskkill` never reach it, so for those the exit code is the only
evidence. On Windows and Linux a release build carries line tables
(`debug = "line-tables-only"` in `suite/Cargo.toml`), so its backtrace names
each frame's file and line. On Windows they resolve through `suite.pdb` next
to the exe, which the installer ships; without it every frame reads
`<unknown>`. The macOS `.app` ships no dSYM, so its backtraces have no line
numbers.

## Driving one by hand

Working a case out interactively beats guessing at it. Start an instance
yourself, then attach:

```bash
DOCXY_CONFIG_DIR=/tmp/sandbox suite.exe --harness
uiharness --config /tmp/sandbox ping
uiharness --config /tmp/sandbox call drag '{"from":"A1","to":"C5"}'
uiharness --config /tmp/sandbox rect cell:A1:C5
uiharness --config /tmp/sandbox shot grid --test scratch
uiharness --config /tmp/sandbox assert border A1:C5 solid teal
```

`--config` is the sandbox the instance was started with; the control directory
is derived from it exactly as the app derives it, so the two cannot end up
looking in different places. It defaults to `DOCXY_CONFIG_DIR` in your own
environment. `--ctl DIR` names the control directory outright.

`DOCXY_HARNESS=1` is equivalent to `--harness`, for a launcher that cannot add
an argument.

A harness instance takes nothing from the agwinterm pane it was started from
(#697). The runner removes every `AGWINTERM*` variable from the child's
environment, and the suite ignores them anyway in harness mode, for launchers
that pass the whole environment through: a harness instance is always named
`suite-<pid>`, never `suite-<pane id>`, and never runs `agwintermctl` to signal
activity. A normal suite in a pane keeps both.

## Writing a case

A script (`*.uit`) is plain text. `#` starts a comment, with two exceptions:
inside `type` and `call`, whose payloads are taken verbatim, because `#` is a character a
spreadsheet test has every reason to type; and inside a **border** assertion
(`assert border …` / `assert no border …`), where a `#` followed by exactly six
hex digits is a colour (see [Colours](#colours)) rather than a comment — so
there, a comment after the colour needs a space or a non-hex word first. The
exception is that narrow on purpose: a border assertion is the only step that
names a colour, and plenty of ordinary words are six hex digits (`decade`,
`beefed`, `deface`), so `assert range is A1:C5 #decade later` comments cleanly.
A `test <name>` line starts a case and everything under it belongs to that case.
Two cases may not share a name — not in one file and not across the files of one
run; their captures would land on top of each other, and the run refuses before
it launches anything. "Share a name" is judged after the name is folded to its
evidence directory, so `smoke case` and `Smoke-Case` count as the same name.

`parse_script` is a **pure function over the text**, and the runner parses every
file before it launches anything — so a typo costs milliseconds rather than a
cold start, and every accepted and rejected form is a unit test.

### Steps

| Step | Drives |
|---|---|
| `open <path>` | the file, resolved **against the script's own directory** — never the working directory. A file the app could not read is an ERROR, not a silent green step: the loaders substitute an empty document and record the reason in `status`, so the step reads the status back and stops the case there |
| `open copy:<path>` | copy that fixture to `<sandbox>/<case-slug>/<original filename>` and open the copy; refuses an existing destination. Use this for saves and exports so their outputs stay in the run sandbox and the original fixture stays untouched |
| `open copy:"<path>" as <name>` | copy the same fixture under a distinct plain filename (no separators or `..`), so a case can open multiple tabs without the suite focusing an existing path; quotes keep a source path containing ` as ` unambiguous |
| `call <verb> <json-object>` | send a raw control request, including Project verbs such as `call task.set {"uid":2,"duration":"3d"}`. JSON is parsed before launch and retained verbatim, including quoted `#`; trailing comments are not allowed. Non-object/invalid JSON is a script error; a refused request is an `ERROR` with the server message |
| `call-error <verb> <json-object> => <message>` | require the app to refuse the request with a message containing the given text; a successful request or different refusal fails the step |
| `click <cell> [shift] [double]` | the cell's click handler (press, click, release) |
| `drag <from> -> <to>` | press, one move per cell crossed, release. `to` and a bare space read the same |
| `type <text>` | one key event per character. The text is taken verbatim between its ends; the whitespace on either side of it is trimmed, so `type   =SUM(` types `=SUM(` |
| `key <k> [k…]` | those keys, in order: `escape`, `enter`, `tab`, `up`, `alt`, `f2`, `ctrl+c`, `shift+down`, … (`tab` and `shift+tab` go through the app's bound *action*, which is where gpui sends them — see below) |
| `select chart <n>` | the press on a chart card, counting from 0 |
| `focus <field>` | the click on a reference field |
| `snapshot <range>` | remembers those cells, for a later `assert cells unchanged` |
| `shot <region>` | files a PNG of the region |
| `assert …` | see below |

### Tab is the one key that is not a key press

gpui reserves Tab and Shift-Tab for focus traversal: it matches key *bindings*
before it delivers a key-down event, so those two never reach the app's
`on_key`. The app binds them as actions instead (`InsertTabAction` /
`OutdentAction`), which is where its Tab behaviour lives — on a sheet, commit
the open edit and move one cell right or left.

The harness routes them the same way, so `key tab` and a literal tab inside
`type` both reach the real handler. This is worth stating because the obvious
implementation — push every keystroke into `on_key` — makes `key tab` a silent
no-op on a sheet, and a case asserting the selection *did not* move would then
pass against a perfectly correct app. `ACTION_KEYS` in `harness.rs` lists what
gets re-routed, and a test fails if it drifts from what `cx.bind_keys`
registers.

Reference fields, for `focus`: `chart-range`, `chart-title`, `categories`,
`series-name:N`, `series-values:N`, `cond-format`, `validation`.
Data › Text to Columns is a dialog (`text-to-columns`), driven with the
`dialog-*` verbs like the others.

**`open` always loads the file from disk.** The cases in a script share one
instance — a process per case would multiply a two-second launch by however
many cases there are — so `open` is the only setup a case has, and it has to
mean the same thing on case 5 as it did on case 1. In a harness instance it
therefore replaces the tab whether or not it has unsaved edits, silently. Not
everything a case leaves behind sets the dirty flag — an uncommitted in-cell
edit, the selection, the scroll position — so reloading only the dirty ones
would carry the rest into the next case, and a case would pass or fail on the
order it ran in. (A normal instance still asks before discarding unsaved work,
and still keeps it if you say no; `open`'s `reopen: "ask"` asks the same
question, as an app dialog — see [Open modes](#open-modes-and-protected-view).)

⚠️ **No modal dialog may sit on a path a verb can reach.** `rfd` runs its own
message loop on the app thread, which stops the control pump dead — the window
keeps answering Windows messages so it *looks* alive, while every verb after it
times out with nothing on stderr to say why. The Save As
dialog a `key ctrl+s` on a never-saved or read-only workbook would raise, and the
unsaved-changes prompt on close are each gated on `harness.is_none()` (the
reopen question is an app dialog now, see [Dialogs](#dialogs)); anything
new in that family needs the same guard, and should refuse in words instead
(`tab.status`), which a case can read. Save As itself is driven with the
[`save-as`](#save-as-the-clipboard-and-the-fill-handle) verb, which hands the
save functions the path the dialog would have answered with. The app's own dialogs are not in that
family: they are app state that never runs a loop of their own (see
[Dialogs](#dialogs)).

### The fixture

Cases run against `uiharness/fixtures/basic.xlsx`: one sheet, a `Region/Q1/Q2`
header and four rows in `A1:C5`, and one bar chart anchored below and right of
`A1:D5` so it never sits under a drag. It is committed — a UI test must not
depend on a build step running first — but generated from literals in
`uiharness/tests/fixture.rs`. Change `build()` there and regenerate, or the
drift check fails:

```bash
UIHARNESS_REGEN_FIXTURE=1 cargo test -p uiharness --test fixture
```

Nothing in the harness reads a user document, which is the point of the fixture
living under the harness's own directory.

`basic.docx` is a copy of the repository's blank Word template
(`offxy-vscode/mcp/templates/blank.docx`) for the cross-kind ribbon smoke case.
`doc-table.docx` has a paragraph before and after a two-cell table; the
`doc-state.uit` case uses it to check that the table tabs show only in a table.
`rulers.docx` has contrasting page margins, first-line and hanging indents,
and four pages of text. `doc-rulers.uit` checks the painted ruler at two zooms.

Every verb goes in through **the same entry point the pointer or keyboard
would**. A verb that reached past a handler into the state it maintains could
pass while the handler under test was broken — the one way this harness could be
worse than nothing.
`selection-set` is a setup exception: it places the caret through the editor API
because the UI cannot place it by document offset, so it does not test clicking
or dragging a selection. `window-size` and `window-zoom` are setup exceptions
that call GPUI window APIs. `title-tab` calls the same handler methods as the
title-bar arrows and dropdown items, and `tab-select` calls `select_tab`, the
tab chip's click handler. `pointer-click`/`pointer-drag` are the hit-testing
exception (#545): real `PlatformInput` events dispatched through gpui, for
overlap order a handler call cannot see. `proj.new` calls `add_tab(Kind::Project)`, the
Backstage › New › Project card's handler; F11 also commits the active plan's
pending cell edit first, and `proj.new` does not.

⚠️ **"The same entry point" usually means the handler, not the hitbox.** A verb calls
the method a handler calls; it does not synthesize a pointer at a coordinate and
let gpui hit-test the element tree. So the harness sees what a handler *does*,
and cannot see **which elements carry handlers at all**. `drag A1 -> C5` runs
`grid_press_cell` → `grid_drag_over` → `grid_release`; it never touches the
deferred fill-handle hitbox that sits over the cells, so a case's `assert cells
unchanged` would stay green if someone put a second handler back on that handle.

The exception is `pointer-click`/`pointer-drag` (#545): they queue real
`PlatformInput` mouse events on the control reply, and the pump dispatches them
through `Window::dispatch_event` once the entity borrow ends — so hit testing
runs against the last rendered frame exactly as an OS click would. They exist
precisely for what handler-calling verbs cannot express: hit order between
overlapping elements, like the more-tabs list (deferred, priority 1) over the
sheet's fill handle (deferred, priority 0).

That is exactly how the auto-fill-on-sweep regression arrived, so it is not a
hypothetical gap. It is closed **in the app, not in a case**:
`sheet_fill_start` refuses while a grid gesture is in flight
(`grid_gesture_in_flight`), which makes any mid-sweep handler on that element
inert whatever fires it. The guard's truth table is a unit test in the suite
(`a_press_that_landed_on_the_grid_cannot_arm_a_fill`) — a guard the harness
cannot reach still has to be something a test can, or nothing notices when it
stops guarding. The rule to take from this: when a regression is *an element
wired wrongly* rather than *a handler behaving wrongly*, a case cannot be the
whole guard — put the invariant where the state changes, test it there, and let
the case cover the behaviour around it.

### Assertions

| Assertion | Reads |
|---|---|
| `border A1:C5 solid teal` | the pixels |
| `border A1 top dashed` | the pixels, one named edge (`top`, `right`, `bottom`, `left`) |
| `no border H20 teal` | the pixels: no such line on any edge |
| `<key> is [not] <value>` | one key of the app's state reply |
| `reply.<path> is [not] <value>` | a field from the last successful driving verb's reply; `open` clears it |
| `cell B2 is [not] <text>` | what that cell shows |
| `no fill preview` | `filling` and `fill_preview` together |
| `cells unchanged` | every cell of the last `snapshot` |

State keys, as the app reports them after every driving verb:

| Key | |
|---|---|
| `tab`, `title`, `dirty`, `status`, `sheet_tab` | the active tab |
| `caption`, `read_only`, `protected`, `repaired`, `final` | `final` is a document Word marked as final (#617), locked like Protected View until Edit Anyway and captioned `[Read-Only]`; the active tab's caption as the strip draws it (`book.xlsx [Read-Only]`, or `Report.doc [Compatibility Mode]` for a document imported from Word 97-2003 until Convert, #634) and its open mode; see [Open modes](#open-modes-and-protected-view) |
| `app_state` | a Project tab's status-bar state, `Ready`, `Edit` (a cell editor, prompt or dialog is open) or `Busy` (a levelling pass is pending); `null` on other tabs |
| `dialog` | the active tab's top dialog's id, or `none`, on every surface; `dialog-click`'s reply carries the `dialog-read` object under this key instead |
| `tabs`, `ask_on_close` | open tab count and whether window close asks about unsaved changes |
| `autorecover_minutes` | minutes between AutoRecover writes while a tab is unsaved; `0` is off |
| `keep_drafts` | whether Don't Save keeps a workbook's last AutoRecover copy as a draft |
| `user_name`, `user_initials` | Settings' User name and Initials for new comments (#620); empty falls back to the OS account name and initials derived from it |
| `sheet_editing` | Settings' Sheet editing options (#672), by their `session.json` keys: `edit_fixed_decimal`, `edit_fixed_decimal_places`, `edit_move_after_enter`, `edit_move_direction` (`down`/`right`/`up`/`left`), `edit_in_cell`, `edit_autocomplete`, `edit_fill_handle` |
| `fx_expanded` | whether Ctrl+Shift+U has expanded the sheet formula bar |
| `menu` | the open menu's `{target}`, or null; `menu-read` has its items |
| `sheet`, `sel`, `anchor`, `range` | the sheet and its selection |
| `editing`, `edit` | whether a cell edit is open, and its text |
| `comment_edit` | the sheet comment editor's text (`null` when closed) |
| `chart_sel`, `panel_chart`, `charts` | chart selection and the panel |
| `field`, `field_text` | the focused reference field, and its buffer |
| `filling`, `fill_preview`, `dragging` | the auto-fill and the sweep |
| `picking`, `range_preview`, `sel_hidden` | point mode |
| `selected_task`, `tasks`, `bar_<id>`, `baseline_<id>` | Project: cell cursor's row (zero-based; equals `tasks` on the entry row below the last task), task count, and each task's drawn bar and baseline bar by displayed ID |
| `selected_row` | Project: the cell cursor's drawn row, **one-based** like `rows[].row` (a collapsed summary's hidden subtasks are not counted); the `rows` reply's `count + 1` on the entry row |
| `prompt`, `selected_name`, `exported` | Project: `none` or `<kind>:<buffer>` for the open prompt, selected task name (empty on the entry row), and `none` or the filename of the last successful Gantt export |
| `cell`, `cell_row`, `cell_edit` | Project: active column name, zero-based row index, and open cell editor buffer (`null` when closed) |
| `undo_depth`, `redo_depth` | Project: number of available undo and redo steps |
| `table_w`, `gantt_w` | Project: entry-table pane and Gantt chart widths in px. Split-bar drags move them; `table_w + 6 + gantt_w + 16` is the window width |
| `timeline`, `timeline_start`, `timeline_finish` | Project: `shown`/`hidden`, and the Timeline's Start/Finish labels (`Mon 3/2/26`; the displayed span, leveled while leveling is on) |
| `timeline_view_start`, `timeline_view_finish` | Project: the first and last day the Gantt chart shows, which the Timeline's view box highlights, clamped to the Timeline's span (`Mon 3/2/26`) |
| `nonworking` | Project: Timeline's inclusive, merged shaded day runs as `[[first, last], ...]` in Project date form (`Wed 3/4/26`). Empty when each day is narrower than 2 px or the project calendar has no weekly working time. Reported even while the Timeline is hidden |
| `filler_rows` | Project: ruled empty rows visible below the last task, from the last drawn frame's `project-body` height (0 before the first layout). Unlike the other keys it trails a driving verb by a frame, so settle with a `shot` before asserting it |
| `ribbon_tab` | current kind-aware ribbon tab name (`Task`, `Resource`, `View`, `Home`, etc.; the contextual table tabs are `Table Design` and `Table Layout`, though the strip draws the second as plain "Layout", as Word does) |

Dotted keys traverse objects, and numeric components index arrays: `assert
sel.start is 1`, `assert reply.tabs.0.name is File`. A later driving verb
replaces the saved reply, and `open` clears it.

### Document tabs

`state()` and `call doc {}` report `text`, `textboxes`, `sel`, `anchor`,
`caret`, `cross_story`, `para`, `run`, `view`, `hf_edit`, `ruler`, and `mail` from the active body
editor. `doc` returns only those document fields. `text` is the main story;
each paragraph contributes a final `\n`, including a table cell paragraph.
Tab is `\t`, and line/page/column breaks are `\u000B`/`\u000C`/`\u000E`.
A field that shows a result contributes one U+FFFC (it is edited as one unit);
its cached result text, like revision text, has no editor caret and
contributes no offset.
Text boxes appear in `textboxes` as separate stories.

`table` is the caret's innermost table, or `null` outside one (#705):
`rows`, `columns` (grid columns), the caret's `row` and `cell`, the table's
`style` id, its style options as the `w:tblLook` hex mask (`look`, `04A0` by
default), the caret cell's `shading` (`RRGGBB`, `null` for none) and
`text_direction`, the cell-range selection as `range` `{top, bottom, left,
right}` (grid columns; `null` when the selection is not across cells), and
`cells`: per row, per cell, the cell's paragraphs as text, with a tab shown as
`⇥` so a script can name it (`assert table.cells.0.0.1 is b⇥c`). Every field
describes that one table: a selection from an outer table's cell into a table
nested in another cell reports the nested table, with `range` `null` (the
range belongs to the outer table, which the table commands act on). `view`
carries `gridlines`, the Table Layout tab's View Gridlines.

`mail` is the tab's mail merge (#628): `doc_type` (`Letters`, `E-mail
Messages`, `Envelopes`, `Labels`, `Directory`, or `null` for a Normal Word
Document), the attached list's `rows` and `columns`, the previewed `record`
(1-based), the `preview` and `highlight` toggles, `pending` (the data source
the document names but has not read), and `text`: the body as it shows, a
merge field as its value or placeholder, paragraphs joined by `¶` (`assert
mail.text is DearJane¶DearJohn`).

Insert > Table's hover grid is a menu item (`menu-read` reports it as
`{"table_grid": {"columns": 10, "rows": 8}}`) and is driven with `table-grid`,
through the handlers the drawn cells call: `{"cols": c, "rows": r}` moves the
pointer over that cell, `{}` moves it off the grid, and `{"cols": c, "rows": r,
"click": true}` inserts a c-column, r-row table and closes the menu. The reply is
`{header, state}`, where `header` reads `Insert Table` or `<c>x<r> Table`. It is
refused unless the Insert > Table menu is open (`ribbon-click
{"tab":"Insert","command":"table"}` opens it). `word-tables.uit` drives it.

`ruler` reports the last painted ruler geometry in logical pixels rounded to
0.1: `first_offset`, `left_offset`, and `right_offset` are marker distances from
the text column edges; `column_inset` is the Draft column's inset from the
viewport; `text_inset`, `vtop_inset`, and `vbottom_inset` are the Print text box's
distances from the page edges. `tracked_page` is the zero-based page index and
`frame` is the frame counter. The object is `null` when the ruler is hidden or
has not painted. Draft reports `text_inset`, both vertical insets, and
`tracked_page` as `null`; Print reports `column_inset` as `null`. `tabs` lists
the tab stops the ruler draws for the caret paragraph of the surface being
typed into (the open header or footer, else the body), as `{pos, align}` in
twips; a paragraph with none of its own shows its style's (#641). Geometry
trails a driving verb by a frame, so settle with `shot window` before asserting
it, as with `filler_rows`.

Offsets count Unicode scalar values, as the editor does. Word counts UTF-16
code units, so offsets after an astral character need conversion for external
comparison. Table row and cell boundaries add no extra marks in this model.
The final paragraph mark is present in `text`, but the offset after it is not
addressable.

`anchor` and `caret` are `{story, offset}`. A story is `main` or
`textbox:<host path>`. On a document tab, `sel` is `{story,start,end}` with
ordered numeric ends when both endpoints are in one story. If a selection
crosses stories, `sel` is `null`, `cross_story` is true, and the two endpoints
still report their stories and offsets. On a sheet, `sel` remains its A1 cell
reference string. `hf_edit` indicates that keys currently reach a header or
footer editor; `selection-set` refuses while it is open.

| Call | Effect |
|---|---|
| `selection-set {"start":5,"end":1}` | set main-story anchor and caret through `Editor`; backward selections keep the larger anchor; an empty range leaves a collapsed caret, the state a click leaves; both offsets are validated before either changes |
| `ribbon-read {}` | list File, ribbon tabs and the contextual tabs — Header & Footer while a header or footer is being edited, Table while the caret is in a table, Gantt Chart Format while a Project's Gantt shows — with groups, commands, galleries and Quick Access Toolbar. The Header from Top and Footer from Bottom boxes carry the `value` they show (`0.5"`). Each `qat` item carries `enabled` and `menu` (a split button with a drop-down). On a document tab `qat-undo` has `menu: true`, and `qat-redo` reads `Redo` (tip `Redo (Ctrl+Y)`) while there is something to redo, else `Repeat` (#618): tip `Repeat (Ctrl+Y)` when Ctrl+Y / F4 would repeat the last action, or `Can't Repeat` with `enabled: false` |
| `ribbon-click {"tab":"Home","command":"Bold"}` | resolve a command on a valid tab, contextual tabs included, by id, else by unique label, else by unique screentip title, and invoke the same action handler as its button |
| `ribbon-layout {}` (or `{"tab":"Data"}`) | where each group of the shown ribbon tab drew its content, from the last frame (#1018): per group `title`, `bounds`, `content_bounds` (the union of its button columns and row stacks, `null` for a group of lone large buttons), `clipped_v` (content taller than the group body: it runs past the title row or out of the group) and `clipped_h`; `hidden: true` for a group the responsive ribbon dropped; plus `any_clipped_v` and `any_clipped_h`. Works on document, Project and sheet tabs. With `tab` it shows that tab first; any call whose last frame is not the shown tab's (a tab was just switched, by `tab` or by another verb) answers `settled: false` with no groups, because the groups are drawn a frame later: take a `shot window`, then ask again. A contextual tab that is not active (Table outside a table), Protected View and a final document (no ribbon body is drawn) are refused; `tab` is refused under a dialog, the plain read is not. Like `title-bar` it reads probes, so settle with a `shot` after any verb that changes the ribbon. A debug build also prints a warning once per clipped group |
| `status-read {}` | read the tab's status line as an ordered `items` array of `{id, text}`: on a Project tab `state` (Ready/Edit/Busy), `new-tasks` (`New Tasks: …`) and `message`; on other tabs only `message` (a document's word-count stats are not reported) |
| `backstage {"action":"open"}` | enter File; `read` reports its open state and rail items (`Info` only while the active tab is a document); `close` returns to the tab |
| `convert {}` | File > Info > Convert on the active tab (#634), the same handler as the page's button: an imported Word 97-2003 document leaves Compatibility Mode (`compatibilityMode` 15 on the next save), its caption drops ` [Compatibility Mode]` and it is dirty. Answers `{status, caption, dirty}`; refused, with the tab's status line saying why, on a tab that is not in Compatibility Mode or not a document |
| `inspect {}` | File > Info > Inspect Document on the active document tab (refused for any other tab): `{comments:{found,count}, revisions:{found,count}, hidden:{found,count,unremovable}, properties:{found}}`; hidden's `count` includes the `unremovable` runs inside tracked moves, fields, a group shape's other text boxes or other preserved XML (`w:customXml`, unmodeled blocks), which Remove All leaves in place. With `"remove":"comments"` (or `revisions`, `hidden`, `properties`) it runs that category's Remove All, the same handler as the page's button, and adds the `status` line it left; revisions are accepted. A category not found changes nothing but the tab's status line, which says `<Category>: nothing to remove`; the Info page shows the last Remove All's line under its rows |
| `theme-set {"theme":"dark"}` | set the window theme as the title bar's theme button does (`light`, `dark` or `auto`); replies with the preference and the mode it resolved to |
| `title-bar {}` | read the measured title content, active chip, tab strip, theme button and drag space; reports tab count, active/first/visible indices, layout mode, `overflow`, `controls_clear`, `active_visible`, `active_dirty_visible` (the active tab is dirty and its bullet lies inside the chip), `theme_visible`, `drag_w`, `drag_ok`, and logical-pixel right edges. `caption_left` comes from a separate probe of Root's inner box minus the pinned caption-control width (102 px on Windows/Linux, zero on macOS) |
| `title-tab {"action":"prev"}` | use the previous/next overflow arrow's tab-selection handler; `more` toggles the dropdown only while its button is shown (overflow or more-only), and `pick` with an `index` selects a tab after `more` has opened the list |
| `tab-list {}` | read every open tab in strip order: `{active, tabs:[{index, title, kind, path, dirty, imported, caption, read_only, protected, final, repaired}]}`. `kind` is `docx`, `xlsx`, `project` or `mail`; `path` is `null` for a tab never saved; `imported` is true for a Project read from `.mpp` and for a document whose file is a Word 97-2003 binary it was imported from (#634; a `.doc`, or one renamed), until a save rebinds the tab to the file it wrote |
| `tab-select {"tab":"schedule"}` | make a tab active as clicking its chip does, and reply with the state. `tab` is an index or a case-insensitive title/path substring over **all** tabs, the rule the `proj.*` verbs use; a miss (`no tab matches 'x'`), an ambiguous match (`several tabs match 'x' (2, 3)`) and an index past the end (`no tab at index 9`) are refused. The Backstage stays as it was, as it does for a chip click |
| `pointer-click {"region":"tab-chip:1"}` | dispatch a real hover-press-release at the region's centre through gpui's own hit testing (or `{"at":"fill-handle"}`: the active selection's handle point); replies `{x, y, item}` where `item` is the more-tabs list index under the point, or -1 off the list. Refuses under a dialog; a press reaches an open menu's own item or backdrop, so it does not pre-close menus |
| `pointer-drag {"from":"tab-chip:0","to":"tab-chip:2","offset":[6,0]}` | dispatch a real press, 8 pressed moves and a release from the `from` region's centre to the `to` region's centre — plus the optional logical-pixel `offset` on the target. The drag arms once a pressed move lands more than 2px from the press, so a from→to distance (including `offset`) of about 2.25px or less acts as a click; longer drags (chip reorder) happen exactly as by pointer. Replies `{from:[x,y], to:[x,y]}` |
| `proj.new {}` | make a blank Project and activate it, as Backstage › New › Project does; replies with `proj.path` for it (`tab`, `path: null`, `name: Project1`, 0 `tasks`, `imported`, the cell state). It takes no `tab` and no `name`: the plan is the app's, so name it by saving it (`proj.save {"path":…}`). The Project control server accepts it too |
| `window-size {"w":600,"h":700}` | resize the harness window in logical pixels; accepts width 300..4096 and height 200..4096 |
| `window-zoom {}` | call GPUI's zoom action; on Windows it maximizes, while the native caption Max button uses the OS control area. Use a fresh harness window for restored geometry on Windows |

`ribbon-read` and `ribbon-click` work on document, Project and sheet tabs. Each
command's `enabled` is the predicate its button draws with (every document and
Project ribbon command is enabled today except the document Layout tab's
placeholders, `LayoutAct::Unavailable`: Line Numbering Options..., Manual and
Hyphenation Options..., and the Design tab's Page Borders on a Markdown tab,
which keeps no section properties; sheet placeholders, drawn but doing nothing
yet, report `enabled: false`, see below), and a command inside a split button's or a
drop-down's menu carries `menu`, its button's id (Set Baseline's `Set Baseline...` and `Clear
Baseline...` read `menu: "pr-baseline"`). Each command carries a `label` and a
screentip `tip.title`; on the Project ribbon they are Microsoft Project 2024's,
and they differ for icon-only commands (Indent is `Indent Task`, Link Tasks is
`Link the Selected Tasks`), so `ribbon-click` finds a command by either name.
The Project ribbon holds only Project's commands, and its Report tab has no
groups yet (`groups: []`). Extend Selection mode, native prompts,
and backstage pages are not represented by these verbs.

On a sheet tab the reply lists File, Home, Insert, Data, Review and View from
`sheet_ribbon::SHEET_RIBBON`, the table the sheet ribbon is drawn from, so a
button cannot be drawn without being listed. Ids are kebab-case and unique
across the sheet ribbon (`bold`, `sort-a-z`, `freeze-panes`); icon and glyph
buttons carry Excel's names (`Top Align`, `Increase Decimal`, `Accounting
Number Format`), and `tip.title` is the label (sheet buttons have no
screentips or KeyTips). `checked` is the pressed state Bold, Italic, Borders
and the three aligns draw for the selection. A toggle reads its current label
(`Unfreeze Panes`, `Unprotect Sheet`) and resolves by it. Placeholder buttons
that do nothing yet (Format Painter, Underline, Cell Styles, Spelling, …) are
`enabled: false`, and `ribbon-click` refuses them (`'Spelling' is not
implemented`) instead of replying green over a no-op. `ribbon-click` resolves
by id, else label, else the drawn text (`Σ AutoSum`), selects the tab and runs
the button's own `run_sheet_act` (a drop-down button opens its menu; a menu item
is clicked through the menu, see below). Buttons that open a bar (Filter, Custom
Sort, Data Validation, …) leave it open for `type` and `key enter`, as a click
does. Home > Editing's Sort & Filter is a drop-down (#1018): `menu-open
{"target":{"ribbon":["Home","Editing","Sort & Filter"]}}` opens it (any other
sheet button is refused: `'Paste' is not a drop-down on the sheet ribbon`) and
`menu-click` picks an item. Its items are listed after the button in
`ribbon-read` and `ribbon-click` takes their ids (`sort-a-z`, `custom-sort`,
`filter`, `home-clear-filter`, `home-reapply-filter`) or labels, opening the menu
and clicking the item through `menu_activate`. A name that is both a ribbon
button and a menu item (`Clear`) resolves to the button. `sheet-ribbon.uit`
and `ribbon-fit.uit` cover these.

Levelling (Level, Level All, Clear Leveling, Ctrl+Shift+L) is asked for, not
run, so the Project status bar can draw `Busy`; render schedules the pass for
the frame after. Every harness verb first runs a pass still pending, so the
verb that asked for it replies `app_state: "Busy"` and every later verb, and
every plain `assert <key>`, sees `Ready` and the levelled plan. Assert Busy
with `assert reply.app_state is Busy` on that verb's own reply.

Close a dirty tab with `call close-tab {"answer":"save"}` (`discard` and
`cancel` are the other answers). Without an answer a dirty close opens the
in-app close prompt (#629), as Ctrl+W, File > Close and the tab's X do:
`dialog is save-on-close`, driven with `dialog-read`, `dialog-set` and
`dialog-click`. A document's prompt is Word's `Save your changes to this
file?` with `file-name`, `extension` (a label) and `location`, and the buttons
Save, Don't Save, Cancel and More options... (refused under the harness, like
every native dialog); its Save writes `<location>/<file-name><extension>`, or
the tab's own file in place when that is what they name and the tab saves in
place (`extension` is then the file's own, whatever it is), and refuses an
existing other file. A tab whose Save is Save As (opened read-only, repaired
or converted) proposes `<name> (copy)` beside its file and refuses its own
file's name. Under the harness the only location offered after the tab's own folder
is the sandbox. A workbook or a Project asks `Save changes to <title> before
closing?`. A tab with another dialog open is not closed: it comes to the
front with `Close the open dialog first`.
An optional `index` targets an inactive tab; it defaults to the active tab.
`call backstage-close {}` calls the Backstage Close handler without supplying
an answer. `call close-window {}` is the window's close button (#630): with
`ask-on-close` on and work unsaved, it brings the first unsaved tab to the front
with the close prompt (`quit` in its owner) and replies; each Save or Don't
Save goes on to the next unsaved tab in tab order (a clean tab is never asked),
Cancel keeps the window, and once the last is answered the process ends after
that reply, as after `quit`. Don't Save there keeps a file-backed tab in the
session as its file alone (it reopens clean) and drops a never-saved one.
Otherwise `close-window` exits at once (hot exit). The window's own close in a
harness instance never asks: `quit` must end the run it waits on. `call ask-on-close {"on":true}` uses the same setting handler as
Settings; closing a dirty single tab always asks regardless of this window setting.

`call autorecover {"minutes":N}` sets the Settings AutoRecover interval (`0`
turns it off). `call autorecover-now {}` runs one AutoRecover tick at once, as
the timer would when the interval is up, and replies `{"wrote":bool}`: whether
anything was unsaved and so written to the hot-exit sidecars and
`session.json`. It flushes an open header/footer but leaves sheet and Project
cell editors open. A run leaves `<config>/docxy/running` behind until a clean
exit (window close or `quit`) removes it; a relaunch that finds it labels its
dirty restored tabs `recovered — AutoRecover copy …`. The ignored desktop test
`uiharness/tests/autorecover.rs` kills an instance after a tick and checks the
relaunch.

`call keep-drafts {"on":bool}` sets Settings' "Keep the last AutoRecovered
version if I close without saving" (#613, on by default). With it and
AutoRecover on, a workbook closed with Don't Save after a hot-exit write while
it was unsaved (a tick, or any other persist such as switching tabs) leaves a
copy of that write in `<config>/docxy/drafts/`. An open cell edit is not in it:
commit the cell (Enter) before the tick. If the draft could not be kept, the
`close-tab` reply carries the reason as `draft_error` (outside the harness it is
the status line, or a warning when no tab is left). `call drafts {}` replies
`{"drafts":[{name,path,age_secs}]}`, newest first, and deletes drafts older
than four days unless one is open in a tab. `call open-draft {"index":N}` opens
the Nth of a fresh listing read-only, as a click on its row in the backstage's
Recover Unsaved Workbooks does. The ignored desktop test in
`uiharness/tests/autorecover.rs` walks the issue's scenario.

`call user-name {}` opens Settings' User name... dialog (#620) on the active
tab, as the backstage row does: `dialog-set` its `user-name` and `initials`
fields and `dialog-click` OK to store and persist them. New Word comments are
stamped with that name and those initials (else the OS account name, else
`docxy`, with initials derived from the name) and the UTC time.

`project-tabs.uit` drives several plans at once: a blank one from `proj.new`
that takes tasks without a fixture, two opened plans switched between by title
with `tab-select`, and `tab-list` read after each step. Assert one tab's entry
with a dotted path, e.g. `assert reply.tabs.2.dirty is true`. What a relaunch
restores is checked by the ignored desktop test
`uiharness/tests/tab_restart.rs`: it quits with two plans and a blank one, one
dirty and another active, relaunches the same sandbox and requires the same
`tab-list` reply back. A relaunch is not a script step.

Project `bar_<id>` values are `<kind> <start>-<end>` in inclusive day offsets
from the Gantt chart's scale origin, or `none` when the task has no schedule result.
Kinds are `critical`, `on-track`, `summary`, and `milestone`. The kind is the one
drawn after the Gantt Chart Format toggles: with Bar Styles › Critical Tasks off,
a critical task reads `on-track`. `baseline_<id>` is the drawn baseline bar,
`<start>-<end>` on the same scale, or `none` when the task has no baseline or
Bar Styles › Baseline is off. These state keys cover every task, including those
outside the visible chart.

Project `timeline` is `shown` or `hidden` (View > Split View > Timeline).
`timeline_start` and `timeline_finish` are the Timeline's end labels in Project's
date form, for example `Mon 3/2/26`. They span every displayed bar: a task shown
before the project start moves the start earlier, and the finish is the leveled
one while leveling is on.
`timeline_view_start` and `timeline_view_finish` are the first and last day
the chart shows (a day partly in view counts), clamped to that span; they are
what the Timeline's view box covers. Dragging the box scrolls the chart.
`pointer-drag` drives drags by region endpoints, not the box's own
coordinates, so cases move the chart with keys (Task › Editing › Scroll to
Task, and Alt+Home for the project start) instead.

Project cells use Enter/F2 or a double-click to edit the current value; typing
any printable character replaces it. Left/Right move between columns, and
Home/End (or Ctrl+Left/Right) go to the row's first and last column. Up/Down
move between rows; Ctrl+Up/Down go to the first and last task, and
Ctrl+Home/End to the first task's first column and the last task's last column.
Down from the last task, or a click below it, goes to the entry row, where
typing appends a task (`click C<n>` addresses it, where `n` is one past the last drawn row). Tab/Shift+Tab move
between columns.
Insert inserts a blank row above the current row (above the entry row when the
cursor is on it) and keeps the column. Delete clears the active Name,
Predecessors, or Resource Names cell; on Task Mode, Duration, Start, or Finish
it reports `<column> can't be cleared`. On the ID column Delete deletes the task.
A summary asks first in the `delete-summary` message box (see
[Dialogs](#dialogs)): Yes or Enter deletes it with its subtasks as one undo
step, No or Escape cancels, and nothing else reaches the plan while it is open.
The state's `dialog` reads `delete-summary`, and `prompt` stays `none`. Ctrl+Delete clears or resets the cell as one undo step: Name,
Predecessors and Resource Names clear as with Delete, Duration becomes 1 day
(`1d?` unless the plan's `NewTasksEstimated` is off; an auto summary's is
refused), and Task Mode becomes the plan's mode for new tasks. On ID, Start and
Finish it reports `<column> can't be cleared` and never deletes the task; on a
blank row or the entry row it does nothing. Alt+Shift+Right/Left
indent/outdent, Alt+Right/Left pan the Gantt,
Alt+Home moves it back to the project start, and Ctrl+Shift+L toggles leveling.
Ctrl+F2 opens the Predecessor prompt (Task › Schedule › Link Tasks, screentip
Link the Selected Tasks) and Ctrl+Shift+F2 removes the selected task's links
(Unlink Tasks) as one undo step. Former bare-letter commands
(`n x d p c a b L`) now type into cells. Predecessor, constraint, resource and
baseline commands are on the ribbon under Project's names; renaming and
durations are cell edits, and there is no ribbon Delete Task, Clear Resources,
Export Gantt, Scroll Left/Right or Go to Start (Project has none): use Delete on
the ID or Resource Names cell, Ctrl+E, Alt+Left/Right and Alt+Home.
Predecessors in cells use **displayed IDs**. Ctrl+F
opens Find; F3 repeats and reveals the selected row. Ctrl+Z/Y undo/redo, Ctrl+S
saves, and Ctrl+E exports Markdown. Use `open copy:` before save/export. Project
ribbon KeyTips are File/Task/Resource/Report/Project/View = F/T/U/R/P/W after
Alt or F10.

An open cell editor owns input before prompts and KeyTips. Enter commits and
moves down, Up/Down commit and move one row up or down (keeping the column),
Tab/Shift+Tab commit and move right/left, and Escape cancels.
Left/Right/Home/End move the caret; Backspace/Delete remove characters.
Invalid input retains the buffer and selection. ID and summary Duration/Start/Finish
cells are read-only. Ctrl+S commits before saving; other Control, Alt, and
platform-modified events do not edit an open cell buffer. Clicking another cell
or Gantt row and running ribbon commands commit first and stop on invalid input.
Switching tabs preserves pending cell edits. On window close, valid pending
Project edits are committed before hot-exit persistence; invalid buffers are
discarded on exit while their last committed project state is still saved.

An open Project prompt owns input before KeyTips: Enter commits, Escape cancels,
Backspace removes one character, and printable text appends. Tab/Shift+Tab do
nothing while it is open. Control, Alt, and platform-modified prompt events are
ignored (including modified Enter/Escape/Backspace); no AltGr input is supported.
Outside editors and prompts, the Ctrl and Alt chords listed above are handled;
other Control/Alt/platform chords are ignored. Changing tasks, documents, or commands
cancels the prompt. The `project-cells`, `project-invalid`, `project-ribbon`, and
`ribbon-kinds` cases exercise these routes, including Word/Sheet strip switching.

`assert cell is Duration` checks the Project column state, while
`assert cell D2 is 3d` reads the value displayed at that entry-table cell.
Project A1 references use **drawn** row positions and columns A through H (ID,
Task Mode, Name, Duration, Start, Finish, Predecessors, Resource Names), rather
than task IDs. A collapsed summary's subtasks are not drawn, so they take no row:
`C5` is the fifth row on screen, the row just below the last drawn task is the
entry row, and anything past that is refused. `cell`, `click` and `rect cell:…`
agree on this. With nothing collapsed a drawn row is a task's position in the plan.

`call cell` and `call click-cell` also take `{"uid":3,"column":"Name"}` in place
of `cell`: a task by UID, and a column by zero-based index or header name (any
case). `cell` reads a task a collapsed summary hides (its reply's `cell` and
`row` are then `null`); `click-cell` refuses one, since nothing is drawn to
click. Giving both `cell` and `uid` is an error. The Project `cell` reply adds
the task's `id` and `uid`, and `entry: true` on the entry row, which reads empty.

`call rows {}` lists the rows the entry table draws, top to bottom, without the
entry row; an optional `tab` picks a Project tab as the control verbs do, without
switching to it. The reply is `{view, table, filter, group, sort, count, total,
rows}`: `view`, `table`, `filter`, `group` and `sort` name what is applied
(`Gantt Chart`, `Entry`, `All Tasks`, `No Group`, `ID`; the tab has no others
yet), `count` is the rows listed and `total` every task, blank rows included, so
`count < total` means a collapse hides some. Each row is `{row, kind, id, uid,
name, level, summary, collapsed, blank, cells}`: `row` is one-based; `kind` is
`blank`, `external` (an external task), `summary` or `task`, first match wins;
`id` and `uid` are the task's own, never renumbered by a collapse; a blank row
has an empty `name` and a `null` `level`; `collapsed` is true for a summary whose
subtasks are hidden; and `cells` are the Entry table's texts, column A first.
The `project-rows` case collapses with Alt+Shift+- and expands with Alt+Shift+=.

`nothing` is how a script writes "this key is null" (`assert chart_sel is
nothing`); `null` and `none` read the same, and `empty` matches an empty string.
Comparison is case-insensitive, and `is not` negates.

### Save As, the clipboard and the fill handle

Three things a person does with a native dialog, the OS clipboard or a
pointer on a few pixels, each driven through the app's own handlers (#699).

| Call | Effect |
|---|---|
| `mail-attach {"path":"list.csv"}` | Mailings › Select Recipients › Use an Existing List… without the native dialog (#628): the path goes to the same attach the dialog's answer feeds, on the active Word document. A relative path resolves against the tab's folder. The reply is `{rows, columns, status}`; a file that is not a `.csv`/`.txt` list, cannot be read or has no header row is refused |
| `save-as {"path":"out.md"}` | Save As the active tab to `path` without the native dialog: the path goes to the same save function the dialog's answer feeds (`save_doc_to`, `save_sheet_as`, `save_project_to`), and the tab is rebound (title, path, clean, a document's Markdown flag) exactly as after a dialog Save As. Optional `format` and `overwrite` |
| `clipboard {"action":"read"}` | the clipboard's text and what the active tab's paste would use |
| `clipboard {"action":"write","text":"a\tb\n"}` | put text on the clipboard, as another app's copy would |
| `fill-drag {"from":"B4:B5","to":"B8"}` | press the fill handle, cross each cell to `to`, release. Optional `from` |

**`save-as`.** `path` is required; a relative one resolves against the folder
of the tab's own file, where the dialog would open, so after `open copy:` it
lands in the case's sandbox folder. A tab that was never saved needs an
absolute path. The format follows the extension by the app's own rules:
documents save as `.docx`, `.md` (`.markdown`) or an editable-HTML bundle
(`.html`/`.htm`, only in a build that can make one or from a tab that is one);
workbooks as `.xlsx`, `.xlsm`, `.xltx` or `.xltm`, each written as that file
type (a macro-free one without the workbook's macros); Projects as `.yppx` or
MSPDI `.xml`. A path with no extension takes `format`'s (`docx`, `md`, `html`
→ `.docx.html`, `xlsx`, `xlsm`, `xltx`, `xltm`, `yppx`, `xml`) or the kind's
first (`.docx`, `.xlsx`, `.yppx`). The reply is
`{path, format, title, dirty, status}`. Refused in words, with nothing written
and the tab unchanged:

- a missing or empty `path`; a relative `path` on a never-saved tab;
- a format the tab kind cannot save (`Documents can be saved as .docx, .md or
  .html`, `Workbooks can only be saved as .xlsx, .xlsm, .xltx or .xltm`,
  `Project schedules can only be saved as .yppx or .xml (MSPDI)`), including
  any other document extension, which the save would otherwise write as a
  Word package under that name;
- a `format` that does not match the extension given;
- an existing file, unless `"overwrite": true` (the dialog would ask);
- a write that fails: the reply is the tab's status (`save failed: …`), and
  the tab stays bound where it was.

`key ctrl+s` on a never-saved tab and the Backstage's Save As… (a
pointer-only control no verb reaches) still refuse in a harness instance, now
ending with `use the harness save-as verb`. `save-as.uit` covers each kind and
refusal.

**`clipboard`.** A harness instance never touches the OS clipboard: the app's
clipboard reads and writes (document, sheet and Project copy and paste) go
through a private clipboard that starts empty, so a run neither reads nor
overwrites what the person at the machine copied. `read` replies `{text, app}`:
`text` is that clipboard's text (`null` when empty) and `app` is what the
active tab's paste would take besides it: `{kind: "doc", text}` (a document's
copied runs, formatting intact) or `{kind: "grid", text, rows, cols}` (the
sheet's copied cells), each kept while the clipboard still holds the text that
copy wrote, or holds no item at all; or `{kind: "none"}`, when a document,
sheet or Project paste takes `text` (#755). A copy with no text in it (an
image, an empty copy) is newer than either clip too: `text` is `null`, `app`
is `none`, and a paste does nothing. The exception is a document copy of only
an image, whose own text is empty: its clip stays ours.
`write` needs `text` (an empty one is such a copy) and replies the same. Copy, cut and paste are the app's own keys and
buttons (`key ctrl+c`, `ribbon-click {"tab":"Home","command":"Copy"}`), not
actions of this verb, which refuses them and `paste-special` (`paste special is
not implemented in this app`). `clipboard.uit` covers these.

**`fill-drag`.** The `drag` verb presses the grid, so it only sweeps a
selection; `fill-drag` does what a pointer on the fill handle does:
`sheet_fill_start` (the handle's press), `grid_drag_over` for each cell of the
straight path from the selection's bottom-right corner to `to`, and
`grid_release`, which commits the fill as a series (`gridcore::edit::autofill`).
`from`, a cell or range, is selected first with `click-cell`'s own
press/click/release (then the same with Shift for the far corner). The reply is
the state plus `filled`, the filled box (`B4:B8`), or `null` when released on
the source. Refused:

- while the handle is not drawn: `the fill handle is not shown: a cell is being
  edited`, `… a reference is being pointed at`, `… a chart is selected`,
  `… the fill handle is turned off in Settings` (#672)
  (`fill_handle_hidden`, the grid's own condition), or `… File (backstage) is
  open`; or while `the fill handle is covered: the more-tabs list is open`. An
  edit, a pointing reference, the Settings switch and a cover are refused
  before `from` is clicked,
  so a refused verb changes nothing (a click would land in the edit or the
  reference); a selected chart is refused only if the handle is still hidden
  after `from`'s click, which takes the selection back as the pointer's would.
  A handle hidden under a chart card is decided by layout and is not
  modelled;
- when the press does not arm a fill: `the fill did not arm: Protected View —
  select Enable Editing to edit` (#610), `the fill did not arm: the sheet is
  protected` (a protected sheet refuses a fill from the pointer too), or
  `… another gesture is in flight`;
- an `option`: `AutoFill Options are not implemented in this app`.

`sheet-fill.uit` covers these.

### Open modes and Protected View

Excel's ways of opening a workbook (#610) go through the `open` verb, the
same `open_path` the backstage's Open Read-Only…, Open as Copy… and Open and
Repair… call after their file pick:

| Call | Effect |
|---|---|
| `open {"path":"book.xlsx","mode":"read-only"}` | `mode` is `normal` (the default), `read-only`, `copy`, `repair` or `recover-text`; a workbook ignores `recover-text`, a document takes only `recover-text` (Recover Text from Any File, #633), and a Project ignores every mode |
| `open {"path":"book.xlsx","reopen":"ask"}` | a person's open of a file already open: a dirty tab asks first (the `reopen` dialog), a clean one reloads only in another mode |
| `enable-editing {}` | the PROTECTED VIEW message bar's Enable Editing button; refused off a protected tab |
| `edit-anyway {}` | the MARKED AS FINAL message bar's Edit Anyway button (#617); refused off a document marked as final, and in Protected View (whose bar shows first) |

A relative `path` resolves against the active tab's folder, as `save-as`'s
does, so after `open copy:` a case names its own copy; with no saved tab
active it is refused (`… never been saved, so a relative 'path' has no
folder …`), never resolved against the working directory. An open that
only focuses a tab or asks about it loads nothing, so the tab's status is
not judged as a load. `open` still reloads a
file that is already open without asking unless `reopen` is `"ask"` (see
`open` above). The modes, as the state's `read_only`, `repaired` and
`protected` report them:

- **read-only**: Save goes to Save As. A harness instance refuses `key
  ctrl+s` in words (`… opened read-only or repaired, so Save needs Save As …`);
  `save-as` over the tab's own file is refused (`"book.xlsx" is read-only.
  Save a copy under a new name.`), and to any other path it writes, rebinds
  the tab and clears `read_only`.
- **copy**: writes `Copy (1)book.xlsx` beside the file (the first free
  `Copy (k)`) and opens that as an ordinary tab. The copy carries the
  source's `Zone.Identifier` stream, so a copy of a downloaded file stays
  downloaded. A copy that cannot be written, or a copy of a downloaded file
  that cannot be marked as one (or whose own mark cannot be read), opens
  nothing (the unmarked copy is removed) and is the error. A template opens
  as a new, untitled workbook in every mode, so Copy writes no file for it.
- **repair**: a lenient load (`gridcore::xlsx::load_xlsx_repair`); the
  status still starts with `loaded` and names what was emptied or dropped.
  Save goes to Save As, which may pick the file itself.
- **Protected View** is the file's, not a mode: a workbook whose
  `Zone.Identifier` stream says zone 3 or 4 opens protected in every mode,
  Copy included, and so does that copy when it is opened again later. A
  document (`.docx`, RTF, Web Page, PDF, Markdown, #633) opens protected the
  same way. Edits and saves are refused with `Protected View — select
  Enable Editing to edit`: on a document only moving, selecting, copying,
  Find and view commands pass, and `mail-attach` is refused too. A script
  cannot write that stream, so `uiharness/tests/protected_view.rs`
  (desktop-only, `--ignored`) covers it.
- **recover-text** (#633, documents only): Word's Recover Text from Any File.
  A Word package gives its recovered text, anything else its printable text
  runs; the status says `recovered text (N paragraphs)` (a successful open).
  Like a document converted from RTF, a Web Page or a PDF (`loaded
  (converted from …)`) or a damaged `.docx` opened normally (`recovered text
  from a damaged file (N paragraphs)`), the tab never writes its file: Save
  needs Save As (a harness instance refuses `key ctrl+s` in words, `… was
  converted from another format, so Save needs Save As …`), and `save-as`
  over the file is refused (`"name.rtf" was converted; save it as a Word
  document under a new name.`).
- **Conversions run in a child process** (#633): an RTF, Web Page, PDF,
  damaged `.docx` or Recover Text open spawns the suite itself as
  `--convert-import <what> <in> <out.docx>` (at most 20 s; on Windows in a
  Job object limited to 2 GiB), so a conversion that dies or hangs is a
  `load error: the file could not be converted` / `… took too long`, not a
  dead instance; a child that cannot start says `cannot start the
  conversion: <reason>`. `DOCXY_CONVERT_IN_PROCESS=1` converts in process
  instead; either way the tab is built from the same converted `.docx`
  (it has that package). A converted tab restored from the session with no
  readable sidecar (missing or damaged) shows `not converted yet …` until
  its tab is first in front (it converts when selected, or before the next
  harness verb), and a
  Protected View rollback restores what the tab was converted to, never
  converting again.
  `uiharness/tests/open_converted.rs` (desktop-only, `--ignored`) opens the
  Word fixtures through it.
- **Trusted documents** (#882): Enable Editing records the file in
  `<DOCXY_CONFIG_DIR>/docxy/trusted.json` by its canonical path, length and
  modified time as it was opened. That file opens again without Protected
  View, in every mode; a different file downloaded to the same path does not
  match and is protected again. A copy of a trusted file opens editable, but
  the copy is not trusted itself. A store that cannot be written leaves
  editing enabled and the status says `editing enabled (not remembered: …)`.
  `call trusted-clear {}` (and the backstage Settings' Trusted Documents
  Clear, #895) empties the store and replies `{"cleared":N}`; a store that
  cannot be written is the error and nothing is cleared.

`open-modes.uit` covers the rest.

### Dialogs

A dialog is app state on its document tab, **never a native modal loop**, so the
control pump cannot block on one (#393). Dialogs stack: a button can open a
child over its parent, and only the top one takes input. The window draws the
top dialog over a backdrop that covers everything, title bar and ribbon
included. Each has a stable id, which the state's `dialog` key reports:

| Id | Dialog |
|---|---|
| `delete-summary` | Project: delete a summary task and its subtasks (`project-dialog.uit`) |
| `page-setup`, `columns` | Word's Page Setup and Columns (#649) |
| `more-colors`, `fill-effects`, `watermark`, `page-borders`, `page-border-options` | the Design tab (#651): Page Color's More Colors and Fill Effects, Custom Watermark (Word's Printed Watermark), and Borders and Shading's Page Border tab with its Options... child, whose OK writes its margins back into `page-borders` instead of the document |
| `hf-distance`, `page-number-format` | the Header & Footer tab's distance box (#641) and Page Number Format (#650) |
| `insert-table`, `delete-cells`, `split-cells`, `sort`, `convert-to-text`, `convert-text-to-table` | the table dialogs (#646, #647) |
| `text-to-columns`, `text-to-columns-replace` | Excel's Convert Text to Columns Wizard and its replace question (#692) |
| `subtotal`, `outline-settings`, `group`, `ungroup` | Data › Outline (#693): Subtotal (OK, Remove All, Cancel), the outline Settings, and Group's (Ungroup's) Rows/Columns question |
| `consolidate` | Data › Data Tools › Consolidate (#694): `function`, `reference`, `refs` (All references; choosing an entry is what Delete removes), `top`, `left`, `links`; Add and Delete edit the list and keep the dialog open, OK writes at the cell it opened on as one undo step and a refusal keeps it open, Close cancels |
| `reopen` | "… is already open … Do you want to reopen …?" before an open discards a workbook's unsaved changes (#610) |
| `mail-envelopes`, `mail-envelope-options`, `mail-labels`, `mail-label-options`, `mail-replace` | Mailings: Envelopes and Labels (Create), Envelope and Label Options (Start Mail Merge), and the confirm before those replace the document (#628) |
| `mail-recipients`, `mail-address-block`, `mail-greeting-line`, `mail-match-fields`, `mail-find`, `mail-check-errors`, `mail-merge-new`, `mail-attach`, `mail-report` | Mailings: Edit Recipient List, Address Block, Greeting Line, Match Fields, Find Recipient, Check for Errors, Merge to New Document, "Opening this document will run the following SQL command" and a report (#628) |

`reopen`'s **Yes** is not an undo step and applies nothing to the tab: it
loads the tab again from its file, in the open mode that was asked for
(read-only, repaired or normal), and the unsaved changes are gone. **No**
closes it and keeps the tab as it was. `open-modes.uit` drives it.

There is no `dialog-open`. A dialog opens through the verb a person would use
(`key`, `ribbon-click`, `click-cell {double}`), so a case covers the real entry
point. Four verbs read and drive it:

| Verb | Args | Reply |
|---|---|---|
| `dialog-read` | `{}` | the top dialog, or `{open: false}` |
| `dialog-set` | `{control, value}`; a grid takes `{control, row, column, value}`, `{control, insert_row: n}` or `{control, delete_row: n}`; a check list `{control, value: true\|false\|[labels]}`, `{control, item, checked}` or `{control, index, checked}` | the dialog after the edit |
| `dialog-tab` | `{tab}` | the dialog on that tab |
| `dialog-click` | `{button}` | `state` after the button's handler, with `dialog` set to the dialog now on top (the child it opened, the parent, or `{open: false}`) |

`dialog-read` replies `{open, id, depth, title, text, tabs, tab, controls,
buttons}`. `id` is stable (`delete-summary`), `depth` counts from 1, `text` is
a message box's message (null otherwise), and `tab` is the current tab's label
(null for a dialog without tabs). Each button is `{label, enabled, default}`.
Each control is `{name, label, kind, value, text, enabled, visible}`:

- `kind` is one of `text`, `number`, `date`, `duration`, `checkbox`, `radio`,
  `dropdown`, `list`, `grid`, `checklist` and `label`.
- `value` is typed: a checkbox's is a bool, a number's a number, an item
  control's the selected item's label (or null), and a grid's its rows.
  `text` is what the control shows.
- `radio`, `dropdown` and `list` add `items` (in order) and `selected` (an
  index, or null). A `grid` adds `columns` and `rows`.
- A `checklist` (the AutoFilter drop-down's values, #690) has a bool per item
  as its `value`, and adds `items` and `depths`: the items form a tree by
  depth (`(Select All)` at 0 over the values; a year › month › day date
  tree under it), and checking an item checks the items under it, while an
  item with items under it is checked exactly when they all are. `value:
  true`/`false` checks or clears all, `value: [labels]` checks exactly those,
  and `{item, checked}` one item (or `{index, checked}` for a label the tree
  repeats, a day under two months). The overlay draws it virtualised (up to
  10,000 values) and a click toggles an item.

`controls` lists **only the current tab's controls**, because that is what a
person sees. To read a staged value on another tab, `dialog-tab` to it first. A
hidden control on the current tab is listed with `visible: false`.

The rules for addressing:

- A control is found by its visible label, case-insensitively and without its
  `&` accelerator mark or trailing colon (`"&Name:"` answers to `name`), then by
  its `name`. A label two controls share is refused, naming the candidates.
- Buttons are found by label in the same way. `OK`, `Cancel`, `Yes` and `No`
  are ordinary labels.
- A tab is found by its label.
- An unknown control, button or tab is refused, and the refusal lists what
  exists.

`dialog-set` goes through the control's input handler, the one the overlay's
editable widgets call too (#649). A click on a text, number, date or duration
field focuses it; a click toggles a checkbox, picks a radio item, or steps a
dropdown to its next item; the tab strip switches tabs. Lists and grids are
still drawn read-only, so only `dialog-set` edits them. (The drawn buttons,
Enter/Escape and `dialog-click` share one press handler.) `dialog-set`
refuses a disabled or hidden control, a control on another tab, a label, and a
value of the wrong shape: a checkbox takes a bool, a number a finite number,
and an item control one of its items. A number field also takes the start of
a number while it is typed (empty, a sign, a trailing point); the owner
parses it on OK. Dates and durations are staged as text: the owner checks them
on OK, and an OK the owner refuses leaves the dialog open with its staged
values. A disabled button refuses `dialog-click`.

Staged values stay in the dialog until an accept button (OK, Yes) hands them to
its owner, which applies them as one undo step. Cancel and No drop the dialog,
and its staged values with it. Apply hands them over and stays open.

While a dialog is open on the active tab:

- **Keys go to the dialog first**, before the tab list, KeyTips and every
  Project key path. Enter presses the default button and Escape the cancel
  button. Tab and Shift+Tab move the focus through the editable widgets on
  the current tab. The focused widget takes the rest: typed characters and
  Backspace edit a field (a character a number field refuses changes nothing
  and says why in `status`), Space toggles a checkbox, and Up and Down step a
  radio group or dropdown. Ctrl and Alt chords are swallowed and edit nothing,
  so `key` and `type` drive the widgets the way a person does.
- **Pointer verbs are refused** with `a dialog is open: <title>`: `click-cell`,
  `drag`, `fill-drag`, `save-as`, `ribbon-click`, `select-chart`, `focus-field`, `title-tab`,
  `tab-select`, `proj.new`, `backstage {open}`, `backstage-close`, `close-tab`,
  `close-window`, `selection-set`, `enable-editing` and `edit-anyway`.
- The state reads `dialog: <id>` (`none` on every surface when nothing is open),
  and a Project's `app_state` reads `Edit`.
- A control-pipe edit, reload or save of that Project dismisses its dialogs
  unapplied, as it cancels a prompt. The exception is a close prompt
  (`save-on-close`, #629/#630): while one is open on any tab, `proj.open`,
  `proj.save`, `proj.reload` and every edit are refused with
  `a dialog is open: <title>`, so the question cannot vanish under itself.

### Menus

A menu is app state, like a dialog, drawn from the same model the harness reads
(#397). One opens through the opener its pointer gesture uses, and an item is
clicked through the item's own click handler, the one the drawn item calls.
Menus open today:

- **the task-row menu** on a Project: right-click a row's table cells (not its
  Gantt bar). It selects the row as a left click does, committing an open cell
  edit, and lists Project's Gantt Chart row menu in Project's order;
- **Set Baseline's split menu**: the lower half of Project › Schedule › Set
  Baseline (`Set Baseline...`, `Clear Baseline...`);
- **the sheet ribbon's Sort & Filter drop-down** (#1018): Home › Editing. Six
  items (Sort A to Z, Sort Z to A, Custom Sort..., Filter, Clear, Reapply),
  each running the Data tab's act of the same name. `menu-open
  {"target":{"ribbon":["Home","Editing","Sort & Filter"]}}` or `ribbon-click`
  on the button opens it; `ribbon-click` on an item opens it and clicks it;
- **the document menu** (Cut, Copy, Paste, Bold, Italic, Underline, New
  Comment): right-click a document body. It never opens on a sheet or a
  Project, and `menu-open {"target":"document"}` refuses on both;
- **the cell menu** on a sheet (#690, #691): right-click a cell. A cell outside
  the selection is selected first (no link followed; while a formula or a range
  field is pointing, nothing moves). It lists Cut, Copy, Paste, the Filter and
  Sort submenus and New Comment, each a sheet command as the ribbon runs it;
- **the Mailings tab's drop-downs** on a document (#628): Start Mail Merge,
  Select Recipients, Insert Merge Field (the attached list's columns), Rules,
  Finish & Merge, and the Preview Results record box (the attached rows).
  `menu-open {"ribbon": ["Mailings", "Write & Insert Fields", "Insert Merge
  Field"]}` opens one. Commands Word has that are follow-ups are drawn
  disabled, and their tip says so;
- **the Layout tab's drop-downs** on a document (#649): Layout › Page Setup's
  Margins, Orientation, Size, Columns, Breaks, Line Numbers and Hyphenation. A
  press anywhere on the button opens its menu, and so does its KeyTip (Alt, P,
  O opens Orientation; Size is `SZ` and Line Numbers `LN`, which wait for their
  second letter). `menu-open {"ribbon": ["Layout", "Page Setup", "Margins"]}`
  opens one. The items follow Word, with its separators and the Breaks menu's
  `Page Breaks` / `Section Breaks` headings; the current choice is `checked`
  (the caret section's margins, orientation, size, columns and line numbers,
  and the document's hyphenation). Line Numbering Options..., Manual and
  Hyphenation Options... read `enabled: false`. Custom Margins... and More
  Paper Sizes... open the `page-setup` dialog, and More Columns... the
  `columns` one;
- **the Design tab's Page Color and Watermark** (#651): drop-downs whose press,
  or KeyTip (Alt, G, P, C and Alt, G, P, W), opens the menu;
  `menu-open {"ribbon": ["Design", "Page Background", "Page Color"]}` opens
  one. Page Color has `Theme Colors` and `Standard Colors` headings over ten
  colours each, then No Color, More Colors... (the `more-colors` dialog) and
  Fill Effects... (`fill-effects`); the document's solid page colour, or No
  Color, is `checked`. Watermark has Word's gallery under `Confidential`,
  `Disclaimers` and `Urgent` headings (each text diagonal, 1, and horizontal,
  2), then Custom Watermark... (`watermark`) and Remove Watermark; the
  watermark the document shows is `checked`. Page Borders (Alt, G, P, B) is a
  button that opens `page-borders`.

| Verb | Args | Reply |
|---|---|---|
| `menu-open` | `{target}`: `"document"`, `"cell"` (a sheet's cell menu over the selection: Cut, Copy, Paste, the Filter and Sort submenus, New Comment; #690, #691), `{"cell": "D7"}` (a right-click on that cell: outside the selection it selects it first, then the cell menu), `{"row": <task uid>}` (`{"row": null}` is the entry row below the last task), `{"ribbon": [tab, group, command]}` or `{"qat": "qat-undo"}` (the Quick Access Toolbar Undo arrow on a document tab; #619) | the menu, as `menu-read` |
| `menu-read` | `{}` | `{open: true, target, items}`, or `{open: false}` |
| `menu-click` | `{label}` among the top-level items, `{path: [labels]}` through submenus, or `{index}`: the top-level item at that 0-based index, separators and headings not counted, for labels that repeat | `state` after the item's handler; the menu closes first |
| `menu-close` | `{}` | `state`, as Esc leaves it |

Each item is `{id, label, enabled, checked, key_tip, submenu}` (`submenu` null
or the submenu's items), a separator `{separator: true}` and a section heading
`{heading}`. An item with no command behind it yet is drawn greyed and read
`enabled: false`, so the order stays Project's: on the row menu those are
Paste Special..., Text Styles..., Font..., Fill Down, Clear Contents, Notes...,
Add to Timeline and Hyperlink.... The rest follow the row: Scroll to Task,
Inactivate Task, Manually/Auto Schedule, Assign Resources... and
Information... need a real task, Delete Task any task row (a blank one too), and
Cut, Copy, Paste and Insert Task any row, the entry row included. `checked`
is the ribbon's pressed state (Inactivate Task, the task's mode). Delete Task
deletes the selected task whatever column the cursor is on; a summary asks
first, in the `delete-summary` dialog.

- **the Header, Footer and Page Number drop-downs** (#641, #650), on Insert ›
  Header & Footer and on the contextual Header & Footer tab. Header and Footer
  list the `Built-in` designs (Blank, Blank (Three Columns)), then Edit and
  Remove. Page Number lists Top of Page, Bottom of Page, Page Margins
  (`enabled: false`) and Current Position, each available one opening a
  submenu of designs (`menu-click {"path": ["Bottom of Page", "Plain Number
  2"]}`; the pointer opens a submenu in the menu's place), then Format Page
  Numbers... (the `page-number-format` dialog) and Remove Page Numbers. The contextual tab's
  Header from Top and Footer from Bottom boxes open a menu of distances, the
  current one `checked`, and Custom... (the `hf-distance` dialog).

- **the Undo drop-down** (#619), on the Quick Access Toolbar of a document
  tab: the undo steps' names, newest first (`Bold`, `Typing "two"`, `Enter`,
  `Typing "one"`), at most 100. Typing is named by its text (shortened to 30
  characters with `…`), a command by its name; Backspace and Delete of single
  characters read `Delete`; deleting a selection, joining paragraphs and any
  other edit with no name of its own read `Edit`. The
  item at index `k` undoes `k + 1` steps, back to and including it; Redo
  then brings them back one at a time. With nothing to undo it lists one
  disabled `Can't Undo`. Two steps can share a name, so pick those with
  `menu-click {"index": k}`. Project and sheet tabs have no list.

A press on a split button's arrow or a drop-down button while its own menu is
open shuts the menu, as in Office; the harness's `menu-open` always opens.

`state` has `menu`: null, or `{target}`. The menu opens at the target's drawn
position when the last frame drew it, else in the middle of the window; either
way it keeps itself inside the window.

Refusals change nothing, and each names what it refuses: `menu-open` under a
dialog, a `row` off a Project tab, an unknown uid, a task hidden under a
collapsed summary (there is no row to right-click), a ribbon command that has
no menu, `"document"` on a Project, `"cell"` off a sheet, and the targets
without a menu yet (`bar`, `column`, the ribbon's own right-click); `menu-click` with no
menu open, on a disabled item, an unknown or ambiguous label, a heading, or an
item that opens a submenu; `menu-click` and `menu-close` under a dialog;
`menu-open` and `menu-click` while File (the backstage) or the more-tabs list
is open, since the window draws no menu then;
`menu-close` with no menu open. `menu-click` also refuses when the menu no
longer fits its target: a row menu whose task is no longer the selected one,
or the document menu on a Project.

A menu belongs to the moment it opened in. What moves on from it closes it:
another tab, the backstage, a command run from anywhere, a control-pipe verb
that edits, reloads, saves or focuses the plan, and every harness verb that
stands for a press outside the menu (`click-cell`, `drag`, `fill-drag`,
`save-as`, `select-chart`,
`focus-field`, `ribbon-click`, `title-tab`, `close-tab`, `selection-set`,
`open`, `backstage` open and close (not `read`), `backstage-close`,
`inspect` with `remove`,
`theme-set`, `ask-on-close`, `autorecover`, `keep-drafts`, `user-name`,
`trusted-clear`,
`open-draft`,
`enable-editing`, `edit-anyway`, `close-window` and the
`dialog-*` drivers), which closes it first and then goes on, as the press
would. Reads leave it open.

While a menu is open it takes every key: Esc closes it, and so, until menus
take arrows and Enter, does any other key, Tab included. None reaches the
document or cell under it. A press outside the menu closes it too.
`project-menus.uit` drives all three menus.

### Headers and footers

Header/footer editing (#640, #641) acts on a section: the one under the body
caret when it starts, the one a double-clicked page belongs to, or the one
Previous and Next move to. A section with no header of its own edits the one
it inherits ("Same as Previous"); Link to Previous gives it its own copy.
While a header or footer is open, the contextual **Header & Footer** tab shows
(KeyTip `J`) and is selected; leaving selects Insert.

| Verb | Args | Reply |
|---|---|---|
| `hf-state` | `{}` | `{editing}`, and while editing `kind` (`header`/`footer`), `section` (1-based), `variant` (`default`/`first`/`even`), `label` (Word's tab label: `Header -Section 2-`, `First Page Footer`, `Odd Page Header`, …), `same_as_previous`, `part`, `text` (its paragraphs, one per line), `show_document_text` and `page_numbers` (placed page numbers in it); always `different_first_page`, `different_odd_even`, `header_from_top` and `footer_from_bottom` (inches) for the target section |
| `page-double-click` | `{page, area}`: 0-based print-layout page, `header`, `footer` or `body` | `hf-state` after the page's own double-click handler: a header or footer area edits that page's section and variant, the body leaves header/footer editing (a single click in the body no longer does) |

`page-double-click` refuses under a dialog and for a page the document does not
have. `word-header-footer.uit` drives these over a three-section document.

### Regions

`window`, `grid`, `chart-panel`, `cell:B3`, `cell:A1:C5`, `chart:0`, `gantt`,
`bar:<id>` (for example `bar:3`), `project-hbar-table`, `project-hbar-chart`,
`project-vbar`, `project-timeline`, `project-split`, `gallery`, `title-tabs`,
`tab-prev`, `tab-next`, `tab-more`, `tab-more-item:<index>` (the last exists while
the more-tabs list is open), `tab-chip:<index>` — a visible title-bar chip by
absolute tab index; absent while the chip is scrolled out of the strip. `gantt`
is the visible Project Gantt chart body,
excluding its header, divider and vertical scrollbar; `bar:<id>` addresses a task by
displayed ID. The `project-hbar-*` and `project-vbar` regions are the Project tab's three
scrollbar strips: under the table, under the chart, and down the right edge of the rows.
`project-timeline` is the Timeline pane above the Gantt view, and is an error while
View > Split View > Timeline has it hidden. `project-split` is the draggable bar
between the entry table and the chart; its drags show in the `table_w` and `gantt_w`
state entries. `gallery` is the Home ribbon's Styles gallery well on a
document tab, and is an error while another ribbon tab, the Backstage or a
collapsed ribbon hides it. `filter-button:<column>` (for example
`filter-button:B`) is the AutoFilter button on that column's header cell
(#690), an error while the sheet has no filter there or the button is
scrolled out of view; `shot` it before a `pointer-click` so the click lands
on the frame that drew it. Inside a
border assertion the `cell:` may be dropped — `border A1:C5 solid` — because an
assertion about a selection should read like the selection.

**The app answers where a region is; the harness takes the picture.** Only the
layout knows where the grid starts, where `A1` is at this scroll position, or
how big a chart card became, so the app answers the `rect` verb in physical
desktop pixels and the harness crops its capture to that. A harness that
computed "the grid starts 120px down" would break on a ribbon change and would
have to be corrected for DPI.

Geometry and pixels are read from a **settled** frame. A verb only marks the
view dirty, so when its reply goes out the frame that shows what it did has not
been laid out yet; the driver reads the frame counter, sends its verbs, then
waits for `rect` to report a higher one before capturing.

**A partly visible region is refused, except for Project bars and cells.** Both
sides of the crop enforce this, because a half-visible edge is the one failure
mode a pixel assertion cannot notice by itself: the picture is a perfectly good
picture, the probe reads a perfectly good line, and the verdict is about the
wrong pixels.

- The app refuses `rect` for a range whose row or column is cut by the edge of
  the grid — `column C is only partly in the grid's view; scroll it fully into
  view first`. Its border is not drawn where a probe would look for it, so
  there is no honest answer to give.
- The harness clamps a crop that runs off the *window* (there are still pixels
  worth filing as evidence) but records which edges it moved, and a border
  assertion then refuses those edges rather than reading the window's frame as
  if it were the region's. That refusal is an **ERROR**, not a FAIL: nothing was
  learned about the app, so the case stops there instead of carrying on and
  reporting the same geometry as half a dozen more failures. The same goes for a
  region too small to read an edge of at all.

Project `bar:<id>` regions are one exception: long bars commonly extend beyond
the chart, so the app returns their visible part, clipped to the Gantt chart
viewport in both axes. An entirely outside bar or a task row that is not rendered
is refused. A screenshot of a clipped bar records that visible part; its crop
edge may be the pane edge, not the bar's actual edge. Use a fully visible bar for
a border assertion. Window-edge clipping still follows the harness rule above.

Project `cell:B3` regions likewise return the visible part of one entry-table
cell, clipped to the table pane in both axes. Entirely hidden or unrendered cells
are refused, as are Project cell ranges. Use a fully visible cell for a border
assertion: a clipped crop edge can be the table pane edge instead of the cell border.

### Colours

`teal`/`brand` (`#2AA79B` — the selection ring and the pointed range's dashes),
`blue` (`#4472C4`), `purple`, `green`, `white`, `black`, or any `#rrggbb`.
`amber` (`#D9642C`) is the Project critical-path bar and mirrors `GANTT_CRIT`.

**Name the colour when you mean "no selection here."** An expectation with no
colour asks "is there *any* line here", measured against the region's own
background — and an ordinary cell has the sheet's gridlines along two of its
sides, so `no border H20` fails on a perfectly ordinary cell with `solid (100%
of 61px) #d9d9d9`. That is the truth about the picture, but not what the test
meant. `no border H20 teal` is. The nameless form still earns its keep for a
region that should be blank — a fill preview that must not have been drawn, say.

### Two things learned by making the cases fail

Each of the committed cases was verified by reverting the fix it guards and
watching it go red. Two of them turned out to bite through something other than
what they looked like they bit through:

1. **`assert cells unchanged` is what catches the auto-fill regression**, not
   the pixels. Under a sweep that fills, `assert no fill preview` and `assert
   dragging is false` both *pass* — by the time the case asks, the fill has
   committed and cleared `fill_preview` again. An armed fill shows in
   `fill_preview`; a fill that *ran* shows only in the cells. Drop the
   `snapshot`/`cells unchanged` pair and the case goes green on the very bug it
   was written for.
2. **A state assertion can be the only thing holding a pixel case up.** With a
   formula pick wrongly counted as point mode, `picking` flips but `border
   A1:B4 solid #2f6fdb` still passes — a dashed border needs a multi-cell
   selection or a range-field preview, and a half-typed `=SUM(` in E2 has
   neither, so no dashes are ever drawn to be seen.

Assert the state *and* the picture. Either alone has a way to be quietly right.

## The isolation guarantee

**A harness instance cannot write to the installed app's config.** This is the
requirement the harness is built around, not a nicety: the suite hot-persists
open documents regardless of whether they were saved, so a test instance sharing
that directory would overwrite documents the user has open.

`--harness` is honoured **only** when `DOCXY_CONFIG_DIR` names a directory that
is not the real config root. `harness::gate` refuses otherwise, and it refuses
in the process that would do the damage — before a listener exists, so the
symptom is the driver's connect timing out and the cause is on the child's exit.
The launcher sets the variable and then trusts that refusal; a second check on
the driver side would be an opinion that can drift from the first.

### The trap that makes isolation look complete when it is not

There are two different path-resolution mechanisms in play:

| Path | Resolved by | Honours `APPDATA`? |
|---|---|---|
| `ctlcore::config_ctl_dir` | reads `APPDATA` directly | yes |
| `dirs::config_dir()` | the Windows known-folder API | **no** |

So setting `APPDATA` for the child isolates the control socket while
`session.json` and the hot sidecars still land in the real profile. That is why
the sandbox is a dedicated variable the app reads itself, and why the discovery
file goes under `<sandbox>/suite/ctl` — derived from the sandbox root — rather
than through `ctlcore::config_ctl_dir`, which would do its own `APPDATA` lookup
and could put the socket in a different sandbox from the session state.

### How it was measured

`%APPDATA%\docxy` was hashed per file and its mtimes recorded — nine files,
including four `hot/tab-N.{docx,xlsx}` sidecars of the user's own open
documents. The full case file was then run. Afterwards the real directory was
**byte-identical and mtime-identical**, and the same writes the app would have
made had landed in the sandbox instead:

```text
target/uiharness-runs/sandbox-<pid>-<timestamp>/docxy/session.json
target/uiharness-runs/sandbox-<pid>-<timestamp>/docxy/hot/tab-0.docx
target/uiharness-runs/sandbox-<pid>-<timestamp>/docxy/hot/tab-1.xlsx
```

That pair is the whole proof: the run *did* persist, and it persisted somewhere
else. A run that simply never wrote would show the same untouched profile and
would prove nothing.

The no-flag build was checked against the same directory: started with no
`--harness` and `DOCXY_CONFIG_DIR` removed from its environment, it used the
real config root as it always has, started **no listener** (`%APPDATA%\suite\ctl`
was never created) and wrote **no discovery file**. Without the flag none of this
runs; the harness is opt-in per process, never a mode a user can end up in by
accident.

The committed cases are also guarded by a unit test that every `open` in them
resolves to something under the harness's own tree — a case that reached into
the user's documents would pass on the machine it was written on and nowhere
else.

## gpui has no window readback — do not go looking for a screenshot API

This costs an afternoon to rediscover, so: **gpui cannot hand you the pixels of
a shipping window.** The three things that sound like they can, and why they
cannot:

| Looks right | Actually |
|---|---|
| `App::screen_capture_sources` | the screen-*sharing* source list — whole displays, not a window |
| `to_image_data` | renders SVG |
| `Window::render_to_image` | behind `cfg(any(test, feature = "test-support"))`, and re-renders the scene to an offscreen texture rather than reading what the compositor put on screen |

The last one is the route macOS now takes, deliberately and only in a build
made for it — see [Capture on macOS](#capture-on-macos). A shipping build
still cannot do it.

On **Windows** the app reports geometry and the harness takes the picture, with Win32
`PrintWindow` (`PW_RENDERFULLCONTENT`) against the test window's HWND, found
from its process id. `PrintWindow` asks the window to draw itself into a device
context, which gets that window alone even when another is in front of it — a
test therefore does not have to own the desktop.

Its weakness is the mirror image: a window drawn by the GPU (gpui uses DirectX)
sometimes comes back blank, because the flag only reaches DWM-redirected
content. When that happens the capture falls back to copying the same rectangle
off the screen, which needs the window visible and unobstructed but always shows
exactly what is there. Which route a capture took is on the `Capture` it
returns, and the CLI prints it (`via PrintWindow` / `via Screen`) — worth reading
when an assertion fails in a way that makes no sense.

There is no image crate in the dependency list either: `uiharness` writes real
PNGs through its own DEFLATE (`deflate.rs`, `png.rs`, round-tripped in tests
against `opccore`'s inflater). Its only dependencies are `ctlcore` and `opccore`
from this repo, `windows` for Win32 capture, `x11rb` for Linux capture, and `gridcore` as a
dev-dependency solely to generate and check the fixture workbook — the binary
itself has no spreadsheet engine in it and wants none, since it reads pixels and
JSON.

### Capture on Linux

The normal Linux build supports screenshots and pixel assertions through X11
`GetImage`. The harness finds the largest mapped client with its suite PID in
the window manager's `_NET_CLIENT_LIST`, reads the client pixels, and translates
the client origin to physical desktop coordinates for cropping. It reports
`via X11 GetImage`. No `harness-capture` build is needed.

Run on an isolated Xvfb display with Openbox so the suite cannot steal your
focus and other apps cannot cover its pixels:

```sh
sudo apt install xvfb openbox x11-utils
cargo build --release --manifest-path suite/Cargo.toml
scripts/ui-linux.py -- cargo run -p uiharness -- run uiharness/cases/sheet-selection.uit
# Exercise capture itself (PID, colors, movement, resize, repaint and occlusion):
scripts/ui-linux.py -- cargo test -p uiharness --test linux_capture -- --ignored
```

The wrapper waits for both the X server and window manager, removes
`WAYLAND_DISPLAY` from the child environment, and stops its command group,
Openbox and Xvfb on exit. The real desktop and config are untouched; the
harness still creates its own config sandbox. The wrapper prints its server
log directory. `--screen`, `--xvfb`, `--wm` and `--logs` override its defaults.
Tools are found on `PATH`, then in `~/.local/bin` for per-user installations.
The virtual display lasts only as long as the wrapped command, so `--keep`
does not keep a suite alive after the wrapper exits. To attach interactively,
wrap a shell and run the harness and attach commands inside that shell.

X11 does not guarantee obscured pixels. The backend refuses an off-screen
client or any overlapping, mapped root sibling above its window-manager frame;
it also refuses blank images and geometry that changes during capture. This
conservatively rejects transparent overlays too. Existing frame waits and
region stability checks still run around captures. TrueColor 16/24/32-bit
pixels are decoded with the server's byte order, row padding and RGB masks;
the returned RGBA alpha is opaque. Native Wayland capture remains unsupported.

#### Sweeping every script

Run each committed script in a fresh suite instance: several assume the
initial tab count or default view settings. Passing all files to one `run`
shares an instance and carries those settings into the next file. The sweep
runner does that for every script, each on a private display of its own:

```sh
cargo build --release --manifest-path suite/Cargo.toml
cargo build --release -p uiharness
python3 scripts/ui-linux-sweep.py                                # every script
python3 scripts/ui-linux-sweep.py uiharness/cases/doc-state.uit  # just these
```

It runs `scripts/ui-linux.py -- uiharness run <script>` per script with
absolute `--suite`, script and `--run` paths, prints a line per script as it
finishes and a table at the end, and exits non-zero unless every script is
`PASS` or `XFAIL`. `--suite` and `--uiharness` default to the release builds;
it builds nothing itself. `--timeout` (default 600 s) stops a hung script.
Everything lands under `--run` (default `uiharness-runs/sweep-<timestamp>`,
which must be new or empty):

```text
<run>/summary.txt, summary.json   the table, and the same as data
<run>/<script>/transcript.txt     uiharness and suite output
<run>/<script>/x11/               the Xvfb and Openbox logs
<run>/<script>/harness/           the harness's captures and sandbox
```

A script is `ERROR`, never satisfied by an expected failure, when it timed
out or its report is incomplete or inconsistent: no case lines, no summary
line, cases that differ from the script's `test` lines, or an exit code that
disagrees with them.

#### Expected failures

`uiharness/cases/expected-failures.txt` lists the cases known to fail on the
sweep, one per line, each with the issue that tracks it:

```text
doc-rulers.uit | <exact test name> | #808
```

A listed case that fails is `XFAIL`; an unlisted failure is `FAIL`, and a
listed case that passes is `XPASS` — both fail the sweep, so the entry is
removed in the change that fixes its issue. An entry covers a case whose
expectations failed (`FAIL` steps); a step that could not run at all
(`ERROR`, such as a refused verb or a failed capture) is still a new failure.
An entry ending in `| error` is the reverse: the case must fail with an
`ERROR` step (`FAIL` steps may appear too), and one that fails on `FAIL`
steps only is a new failure, so a partial fix shows up and the entry goes
back to the default kind. Every entry must name an existing script
and `test` line, checked before anything starts, even when only some scripts
are run.

CI's `ui sweep (linux)` job runs the sweep on `ubuntu-latest` after release
builds, and uploads the run directory as the `ui-sweep-linux` artifact when
it fails.

### Capture on macOS

Nothing outside the app can photograph a macOS harness window: it is never on
screen, and reading another process's pixels needs Screen Recording
permission, which a test must not ask for. So on macOS **the app takes the
picture itself**. The `capture` verb renders the last drawn frame to an
offscreen texture with `Window::render_to_image`, writes the raw RGBA to
`<sandbox>/suite/capture/last.rgba`, and replies:

```json
{"path": "...", "width": 1180, "height": 800, "scale": 1, "frame": 8,
 "content_origin": {"x": 370, "y": 140}}
```

`uiharness` reads that file and from then on it is an ordinary capture:
`content_origin` is in the same physical desktop pixels `rect` answers in,
computed by the same function, so the existing crop (a region's rect minus the
capture's origin) lands on the right pixels unchanged, and every border probe
runs as it does on Windows. The pixels travel beside the control channel, not
through it — a full window is megabytes, no size for a JSON reply. The CLI
reports these captures as `via offscreen render`.

It needs a build made for it:

```bash
cargo build --release --manifest-path suite/Cargo.toml \
  --features harness-capture --target-dir suite/target/capture
cargo run --release -p uiharness -- run uiharness/cases/sheet-selection.uit \
  --suite suite/target/capture/release/suite
```

The separate `--target-dir` is not decoration: the feature changes how gpui
itself is compiled, so sharing a target directory with the normal build would
rebuild gpui every time you switched between them. A default build answers
`capture` with an error naming the feature, rather than a blank picture.

⚠️ **Three things to know before trusting a macOS pixel case:**

- **It tests a different build from the one users run.** `harness-capture`
  turns on gpui's test-support, which also makes gpui draw every dirty window as
  it flushes effects, and turns on leak detection. The scene is the same, but
  the scheduling of draws is not. Never ship this feature.
- **It is the scene gpui drew, not what a compositor showed.** Anything the
  system draws outside gpui is absent — the native window buttons, most
  visibly. `window` is therefore content only, where Windows' `PrintWindow`
  includes the frame.
- **It is refused off macOS.** The feature is a compile error on any other
  target, because gpui's draw-without-present would let a Windows run
  photograph a frame that never reached the screen.

Enabling it has a cost. Measured on an M1 against the pinned gpui: the binary
grows from 21,702,768 to 22,137,088 bytes (+2%), the dependency graph gains
twelve crates (`proptest` and its helpers), and a clean build into the separate
target directory took 252 s with the git dependencies already fetched.

### Why not golden images

Considered and rejected. Reference PNGs diffed wholesale catch every unintended
change, but they are brittle across GPU, DPI and font differences, and the
practical failure mode is that a diff gets blanket-approved rather than
investigated. Targeted probes state what is being asserted, so a failure names a
property — "top dashed (30 dashes of ~3.3px, gaps ~3.0px, 52% of 191px)" —
rather than a pixel count. The full PNG is saved regardless, so nothing stops a
human looking at the whole picture.

## Tests of the harness itself

```bash
cargo test -p uiharness
cargo clippy -p uiharness --all-targets -- -D warnings
cargo test --manifest-path suite/Cargo.toml     # the app side: the gate, region and key parsing, the config root
```

The decisions live in pure free functions — parsing a script, resolving a region
name, deciding whether a sampled row of pixels is dashed or solid, comparing an
expectation to an observation — and those are what the unit tests cover. The
harness end to end is exercised by running it.

## Where a harness window goes, and why it draws at all

A harness instance is opened with gpui's `focus` flag off and, on macOS, its
`show` flag off too. What that buys differs by platform, and the difference is
not cosmetic:

| | Takes focus? | On the user's screen? |
|---|---|---|
| **macOS** | no | **no** — the window is never shown |
| **Windows** | ⚠️ **yes, still** | yes |

⚠️ **`focus` is a macOS-only lever at the gpui revision `suite/Cargo.lock`
pins.** Only that platform's window layer reads it, as `orderFront` instead of
`makeKeyAndOrderFront`; the Windows and Linux layers read `show` and ignore
`focus` entirely. A Windows harness window therefore still activates, and
keeping a run off the user's desktop there is still the launcher's problem.
The flag is set on every platform anyway — it costs nothing and starts working
the moment the pin moves — but do not read it as a promise off macOS.

(Upstream gpui has since grown a `SW_SHOWNOACTIVATE` path and an
`inactive_frame_interval` option. Neither is in the pinned build. **Check the
vendored source under `~/.cargo/git/checkouts/`, not GitHub**, before relying
on any gpui behaviour — this document has been wrong that way twice.)

On Windows the window stays **shown**, and that is load-bearing rather than an
oversight: that layer reports an unshown window as `Hidden`, and hidden is
exactly the state that stops frames there. Withholding it would break the one
harness that already works.

### The frame the app draws for itself

⚠️ **gpui only draws a dirty window when its platform frame source ticks**, and
on macOS that source is a display link which starts only while the window's
occlusion state says it is visible. A harness window is unfocused and unshown,
so the link never runs: `cx.notify()` leaves the view dirty forever, the app
answers every verb correctly, and the frame counter never moves. The symptom is
that `rect` times out — `the app drew no new frame within 5s (still frame 2,
waiting for 4); is it hung?` — against an app that is not hung at all.

(gpui does have a path that draws every dirty window as it flushes effects, but
it is compiled in only under its own test cfg, so a shipping build never takes
it.)

So on macOS the `frame` verb **draws the frame itself** rather than waiting for
a source that is never going to tick: it marks the view dirty and then drives
the render pass directly before replying. `Done::ok_drawn` carries that request
out to the pump and is the only reply that does; an ordinary verb still just
marks the view dirty. Since a driver polls `frame` while it waits for the view
to settle, the frames a case needs arrive exactly when it asks for them.

Nothing is **presented**. A draw is all the probes and the frame counter need,
and presentation is the part that would require the window to be on screen —
which is the thing this is avoiding.

⚠️ **That forced draw is gated to macOS, and the gate is not tidiness.**
Elsewhere frames already flow for a shown window, so it would buy nothing while
costing something real: a draw advances the frame counter *without presenting*,
so `settle` could be satisfied by a frame that was never put on screen and
`PrintWindow` would photograph the one before it. A capture that quietly reads
stale pixels is exactly the failure a pixel assertion cannot notice by itself.

## Not covered

- **More than one window.** The suite has one window, so there is no
  `window-list` or `window-new`, and no New Window or Arrange All to drive.
  `tab-list` covers the tabs of that window.
- **The drawn window title and a recent-files list.** The title bar draws the
  tab strip, not a `Project1 - <app>` title, and Backstage's Open lists the
  open tabs, not a recent-files list, so neither is there to report.

- **Native Wayland capture.** Linux pixel tests use X11 on a private virtual
  display. They do not exercise the Wayland backend or desktop portal dialogs.
- **Mail editing and advanced document UI.** Document text, selection, ribbon,
  status and File rail are covered, and dialogs have the `dialog-*` verbs.
  Menus, pane contents and pointer gestures in document text still need
  harness drivers.
