# Project scheduling — design & roadmap

This document covers the project-scheduling side of the workspace: the
`projcore` engine, the `yppxy` terminal app, and the `mppread` legacy reader —
the `mpp → yppx` third of the `doc → docx`, `xls → xlsx`, **`mpp → yppx`**
trilogy. For the spreadsheet side see [SPREADSHEET.md](SPREADSHEET.md); for the
apps and keys see the [README](README.md).

## The idea

Microsoft Project is a scheduling engine wrapped in a UI. A `.mpp` file is a
list of **tasks** linked by **dependencies**, interpreted against working-time
**calendars**, optionally staffed by **resources** — and the app computes when
everything happens (the **Critical Path Method**). We rebuild that as a small,
dependency-free Rust engine plus a terminal app, exactly as `gridcore`/`xlsxy`
rebuilt Excel.

The interop insight that makes this tractable: Project's **MSPDI** XML
(`File ▸ Save As ▸ XML`) is a *documented* open format. So `projcore` never has
to decode the undocumented binary `.mpp` to exchange schedules with Project — it
reads/writes MSPDI, and keeps its own native package, `.yppx`.

## Crates

- **`projcore`** — the engine. `std`-only, on top of `opccore` (shared ZIP/XML
  plumbing). No third-party dependencies.
- **`yppxy`** — the TUI (ratatui): task outline + live terminal Gantt, the same
  ribbon/backstage UX as docxy/xlsxy.
- **`mppread`** — `std`-only reader for the OLE2 Compound File container of
  legacy binary `.mpp`/`.doc`/`.xls` files.

## `projcore` layers

Built bottom-up, each a pure module:

| Module | Responsibility |
|--------|----------------|
| `datetime` | a civil wall-clock instant (minutes since 1970), proleptic-Gregorian conversion, MSPDI ISO parse/format |
| `model` | the pure domain: `Task`, `Predecessor`, `Resource`, `Assignment`, `Calendar`, `Project`; `LinkType`/`ConstraintType` with MSPDI's integer codes pinned once |
| `mspdi` | read **and** write MS Project's MSPDI XML — the interop bridge |
| `schedule` | the CPM engine + resource leveling |
| `assign` | assignment dates and costs from the schedule and rate tables, and the refresh of stored totals an edit made stale |
| `editor` | shared editing, selection, dirty tracking, undo/redo, and live scheduling |
| `gantt` | export a scheduled project as a Markdown/Mermaid Gantt chart, with a task table (dates, Deadline with a missed-deadline `⚠`, duration, slack) |
| `yppx` | the native `.yppx` OPC package (ZIP + `[Content_Types].xml` + `project.xml`) |

The model is **pure input** — the scheduler never mutates it; it returns a
separate `Schedule`. MSPDI's own computed `Start`/`Finish` are captured as
`stored_*` and used as an **oracle** for the scheduler. The editor rewrites them
for a manual task whose dates it edits, so a save's `Start`/`Finish` agree with
its `ManualStart`/`ManualDuration` (Project does not reschedule manual tasks on
open). A manual summary saves its own dates there too, as Project does; a
summary switched back to auto saves its rolled-up span. Likewise the editor
refreshes the stored assignment dates, costs and remaining work, and the
resource, task and summary totals, that an edit made stale
(`assign::refresh`, run from `Editor::reschedule` during an edit); values no
edit touched are saved exactly as read. Project-level options the model does not hold (`ScheduleFromStart`,
currency, file identity, ...) are kept verbatim in
`Project::options` and written back on save; docxy does not act on them yet, so
it still schedules forward even when `ScheduleFromStart` is 0. Six options are
typed when their text parses: `NewTasksEffortDriven`, `NewTasksEstimated`,
`DefaultTaskType`, `Autolink`, `CriticalSlackLimit` and `MultipleCriticalPaths`.
A save writes them in canonical form (`true` as `1`), keeps unparseable text
verbatim, and leaves an option the file did not state absent. docxy acts on
them: new tasks take the stated task type, effort-driven default and estimated
default duration, a task added between two others autolinks, and the critical
flag follows the slack limit and multiple critical paths (Project's defaults
apply when absent).

`projcore::editor::Editor` owns the editable project, its 100-entry undo history,
selection, dirty flag, computed schedule and optional leveling overlay. Validated
edits snapshot once and reschedule; rejected edits preserve the whole session.
`yppxy` supplies the keys, status messages and file I/O, and its project control
verbs use the same Editor through `dispatch_editor`. Opening or creating a project
clears history while retaining the find query and leveling preference.

## The scheduling model

- **Tasks** have a duration in *working minutes*, an outline level (summary
  tasks own the deeper rows below them), and may be milestones (zero duration).
  A task is auto-scheduled or **manually scheduled** (MSPDI `Manual`, with
  `ManualStart`/`ManualFinish`/`ManualDuration`); tasks added to a plan follow
  its `NewTasksAreManual` default. Switching a task to manual (Task ▸ Tasks ▸
  Manually Schedule, the Task Mode column, yppxy's `m`, or `task.set
  {manual: true}`) pins it at the start and finish it is shown at, the leveled
  ones while leveling is on, and stamps its saved Start/Finish; switching it
  to auto clears the pin and the scheduler places it by its links and
  constraints again. Either is one undo step. A summary switched to manual
  keeps the dates it shows instead of rolling up (see **Manual summaries**
  below). The status bar's `New Tasks: …` (yppxy's `M`) switches the plan's
  default.
- **Dependencies** are the four link types with lag/lead: Finish-to-Start,
  Start-to-Start, Finish-to-Finish, Start-to-Finish. A lag is working time,
  **elapsed** calendar time (`+2ed`, counted from the predecessor's instant,
  after which the link acts as a zero-lag one), or a **percentage** of the
  predecessor's duration (`+50%`). Its MSPDI `LagFormat` selects the unit of
  `LinkLag`: *tenths of a minute* of working or elapsed time, or the percentage
  itself for formats 19/51 — one of several unit traps the reader normalizes.
  The format is kept and written back; unsupported formats fail the read.
- **Inactive tasks** use MSPDI `Active`. Task ▸ Schedule ▸ Inactivate toggles a
  task, cascading to its subtasks in one undo step. Inactive tasks keep their
  own dates and can follow active or inactive predecessors, but they do not
  drive active successors, active summary rollups, the active project's bounds,
  or its critical path. When no active leaf can be scheduled, dormant dates
  bound the project. A summary whose subtasks are all inactive keeps their
  rollup.
- **Constraints** pin dates: ASAP/ALAP and the six hard ones
  (SNET/SNLT/FNET/FNLT/MSO/MFO).
- **Calendars** define working time per weekday (e.g. Mon–Fri 08:00–12:00,
  13:00–17:00); weekends and off-days are skipped.
- **Resources & assignments** staff tasks (units × work).
- **Baselines** snapshot the saved plan for planned-vs-current variance.

## The CPM engine

The core trick is **working-minute index space**. Wall-clock scheduling is
awkward — 5pm Friday + 1 working hour is 9am Monday. So each calendar maps to a
monotonic **timeline**: a function from an instant to "working minutes elapsed
since the project anchor" (`to_index`) and its inverse (`abs_start`/
`abs_finish`). In index space, `finish = start + duration`,
`successor = predecessor + lag`, and slack are all integer arithmetic; we only
convert back to a wall-clock `DateTime` at the end.

A subtle but essential detail: **start and finish use different boundary
conventions**. An index that lands exactly on an end-of-day boundary maps to the
*next morning* as a start, but to *this evening* as a finish. That is what makes
"a 2-day task from Monday finishes Tuesday 17:00" and "its successor starts
Wednesday 08:00" both come out right.

- **Forward pass** → early start/finish, honoring links + lag, ASAP by default,
  plus SNET/FNET date bounds. With `HonorConstraints` enabled (the default,
  persisted as MSPDI `HonorConstraints`), MSO/MFO/FNLT/SNLT override conflicting
  links; with it off, links can delay tasks past those constraint dates.
- **Backward pass** → late start/finish from the project finish, plus the
  backward-affecting constraints (MFO/FNLT/SNLT/MSO).
- **Total & free slack**: link conflicts produce negative total slack in both
  precedence modes. When a constraint moves a task earlier than its links allow,
  total slack uses the link-driven start. Dates are limited by the timeline
  horizon; pre-start constraints do not pull unlinked tasks before the project
  start. Also computed: the **critical** flag (total slack at or below the plan's
  `CriticalSlackLimit`, 0 days by default; with `MultipleCriticalPaths`, a leaf
  without successors is measured to its own early finish rather than the
  project finish, so each such chain is a critical path) and **summary
  rollup** (a summary's dates derive from its descendants, unless it is a
  manual summary).
- **Manual tasks** stay at their pinned dates: the manual start (else the stored
  start) and the manual finish, else start + duration on the task calendar.
  Links and constraints never move them; auto successors schedule from the
  pinned dates. A violated link shows as negative total slack (at least the gap
  between the pinned and the link-driven start); an unlinked manual task gets no
  slack penalty, even before the project start. A manual task without any start
  schedules like an auto task.
- **Manual summaries** (#124) keep their own dates instead of rolling up, as
  measured in Project over COM: the manual start (else the stored start) and
  the manual finish, else start + `ManualDuration` on the summary calendar.
  Their subtasks' span stays available as the **rollup**
  (`Schedule::rolled_up`, `Editor::disp_rollup`). Project's **warning**
  (`schedule::manual_warning`, `Editor::summary_warning`) flags a manual
  summary whose subtasks finish after its manual finish, and a manual task
  finishing after its direct parent when that parent is a manual summary (an
  auto summary in between breaks this); starting early never warns. Hosts
  show it on manual summaries only (projctl's `warning`, the docxy and yppxy
  summary bars); a manual leaf's row does not show it yet. The manual start
  **floors** every ASAP auto subtask, from the nearest manual-summary ancestor
  (auto summaries in between pass it on); a later link still wins, and a
  subtask with any other constraint, even SNET, ignores it. The floor never
  enters the backward pass or the slack's link-driven start. The manual finish
  counts toward the project finish, so every task's slack is measured to it.
  A manual summary's late window spans its subtasks' late dates, but never
  starts before its own start nor finishes before its own finish; its slack is
  the smaller of its start and finish slack, so it is never negative, and it is
  critical only when that slack is at or below the plan's `CriticalSlackLimit`
  (0 days by default; critical subtasks do not make it so). Every ancestor,
  manual or auto, rolls up through its own span and sees it as fixed: the
  nested summary's late window is its own span, and an auto summary over one
  measures its slack from its late window. A manual summary with no start
  (TBD) rolls up as an auto summary. FS/SS links into an auto summary drive
  leaves below auto summary levels; a nested manual summary stops inheritance.
  Links out of an auto summary use its rolled start or finish, while links out
  of a manual summary use its own dates. A link into a manual summary changes
  predecessor slack but does not move that summary or its children. Auto
  summary start floors and backward date bounds apply to inherited leaves.
  Summary FF/SF predecessors and some summary constraint types remain unsupported;
  the provisional late-bound rules await verification in Microsoft Project.

Leaf tasks are ordered by a Kahn topological sort of the dependency graph;
cycles fall back to input order.

### Verification

There's no free high-fidelity oracle for scheduling (Project isn't scriptable in
CI), so the corpus is **self-oracling**: `corpus/mspdi/` holds twenty-seven tiny
one-feature MSPDI files, each embedding every task's `Start`/`Finish`,
`TotalSlack` and `Critical`. In files 01–18 these are the values Microsoft
Project 2024 computes: `corpus/tools/verify_mspdi_project.py` checked every
one against Project over COM (#74), with the oracle elements removed from the
copy Project schedules. Files 19 (manual tasks, #77) and 20 (stored task
fields and a blank row, #80) are hand-derived from our scheduler and not yet
verified in Project. File 27 (manual summaries, #124) was checked against
the local Project by `verify_mspdi_project.py`, as generated and as
`write_mspdi` writes it; the rollup and the warning have no MSPDI field, so
unit tests check them against Project's COM values. Blank rows (`IsNull`)
carry no oracle.
`projcore/tests/corpus.rs` reads each file, runs the scheduler, and asserts the
computed dates, slack and critical flags match — and also runs each file
through **MSPDI → `.yppx` → back** to prove the writer and OPC container are
lossless. Regenerate with `python3 corpus/tools/gen_mspdi_corpus.py`.

## Resource leveling

`schedule::level(proj)` runs CPM, then delays tasks so no work resource is
booked beyond its capacity. A resource's capacity is its Max. Units, or, when it
has availability periods, each period's units over its dates and none outside
them (as Project 2024 does). It processes tasks in topological order; each task
starts no earlier than (a) its CPM early start and (b) the earliest time all its
resources have capacity for it: every stretch between booking and capacity
changes must hold the load plus the task's units (fractional units allowed),
and the search jumps to the next booking end or capacity change. Each
assignment books its resource from its `Delay` into the task to the task's
finish; a stored `LevelingDelay`, Project's last leveling, is not added. A
resource that is free and available somewhere takes a task above its capacity
there, and one with no capacity anywhere later is left overallocated: the task
goes where every other resource fits, or keeps its earliest start when it has
no other. An active predecessor's leveling delay propagates to active
successors, preserving their link gaps. Inactive tasks book no capacity and
keep their CPM dates, even when an active predecessor is delayed by leveling.
v1 is single-calendar and delay-only,
and treats a task's occupation as its wall-clock span; multi-calendar leveling
and task splitting are future work. Leveling never moves a manual
task: its bookings are placed first, and auto tasks level around them.
`yppxy` toggles the overlay with `L` (View ▸ Level).

## Formats

- **MSPDI** (`.xml`) — Project's documented interchange format. `projcore` reads
  and writes it; this is how schedules move to and from real Project.
- **`.yppx`** — the native package: an OPC ZIP (`[Content_Types].xml` +
  `project.xml`) built on `opccore`, the project analog of `.docx`/`.xlsx`. The
  `project.xml` part is MSPDI-compatible, so `.yppx` stays interoperable — unzip,
  rename, and Project opens it — while giving us a container to grow.
- **`.mpp`** — the legacy binary. It's an OLE2 **Compound File** (MS-CFB), which
  `mppread` reads exactly — including the **storage tree**, so nested blocks are
  addressable by path (`read_path("TBkndTask/FixedData")`). Its metadata streams
  are OLE **property sets** (MS-OLEPS). For recognized MPP9 and current Project
  task layouts, `mppread` uses counted `FixedMeta` and `VarMeta` records to
  locate rows and names, then reads dates, outline levels and validated task
  fields at known offsets. Current Project blank rows keep their ID and UID;
  GUID, type, flags, priority, deadline, Notes and leveling options survive an
  import and MSPDI save. Explicit WBS codes and generated codes under the
  default numeric mask also survive; generated codes under a custom mask
  remain absent. Current-layout over-allocation is derived from each task's
  direct assignments and their resources' availability. A valid empty
  assignment table gives false; a missing or unrecognized assignment table,
  or a needed resource table, leaves the field absent. The value is kept after edits.
  Recurring tasks are validated against a UI-authored Project sample.
  Stored task IDs determine display order; stable UIDs connect predecessor
  links from `TBkndCons`. An unrecognized layout or malformed row causes an
  import error. Legacy MPP9 automatic leaves, including childless inserted
  subprojects, are pinned to decoded starts. Current-layout tasks keep their
  recorded constraints; an automatic leaf receives a Must-Start-On pin only
  when the scheduler cannot otherwise reproduce its stored start. Automatic
  leaves retain decoded durations, and outline summaries roll up from their
  children. Nonzero link lag still lacks a real-file oracle.

## Desktop suite entry table

The suite's Project tab edits Task Mode, Name, Duration, Start, Finish,
Predecessors, and Resource Names directly in the selected cell. Task Mode takes
`Manually Scheduled` or `Auto Scheduled`, or any start of them (`m`, `auto`). Enter/F2 or a double-click opens
the existing value; typing replaces it. Enter commits and moves down, Tab and
Shift+Tab commit and move between columns, and Escape cancels. Invalid input
stays open for correction. ID and an auto summary's dates/duration are
read-only. A manual summary's Start, Finish and Duration set its own span as
they do for a manual task, without touching its subtasks; its Gantt row adds
the subtasks' rolled-up span as a thin bar above its own, the part past its
finish in a warning colour (yppxy marks those days `╍`), else its own finish
day (an overrun within that day, or a finish past its manual parent).

Durations show Project's `?` for an estimated task (`1d?`, `2.5d?`), in the
suite, yppxy and projctl's task JSON (`estimated`). Typing a duration with a
trailing `?` marks it estimated; typing it without `?` commits the estimate,
even at the same duration (`1d` over `1d?`), as one undo step. A task never
marked estimated stays unmarked. A summary shows `?` when any task below it is
estimated; that rollup is shown, not saved, and a summary takes no `?` of its
own.

A task that finishes after its Deadline gets Project's missed-deadline
indicator (#170), judged by `Task::misses_deadline`: Finish later than the
Deadline instant, as stored. yppxy's grid shows a `⚠` at the end of the task's
row (after Slack) when its displayed, possibly leveled, finish misses it, and
the header then names it for the selected task (`⚠ B: finishes after its
deadline 2026-03-06`), the text Project gives in the indicator's tooltip. The
Markdown Gantt export's table has a Deadline column: the task's deadline,
followed by `⚠` when the row's own Finish is after it. The suite's grid has no
indicator yet.

As in Project, the blank row below the last task is the entry row: clicking any
empty row below the tasks, or Down from the last task, puts the cell cursor
there, and typing into it then committing appends a task (`1 day?`, or
`1 day` when the plan's `NewTasksEstimated` is off, unless a duration is
typed), as one undo step. A new plan starts there, so typing a name creates its
first task. Commands that act on the selected task (Delete on ID,
milestone, indent/outdent, clear resources, the task prompts) do nothing on the
entry row; Insert inserts a blank row just above it, and Find searches from the
first task.

Task › Insert › Blank Row (keytip Alt, T, B) and the Insert key insert an empty
row above the selected row, or just above the entry row, as one undo step, and
select it, keeping the column, as Project's Insert Task › Blank Row does. The
row is outside the outline and the schedule until something is typed into it;
then it becomes a task at the level of the task above where it sits (a summary's
first child). yppxy has the same command on its ribbon and on `N`.

Arrow keys move the cell cursor. Home and End (or Ctrl+Left/Right) go to the
first and last field of the row, as in Project; Ctrl+Up/Down go to the first
and last task, keeping the column, and Ctrl+Home/End to the first task's first
field and the last task's last field. Insert inserts a blank row above the
current row, keeping the column. Delete clears the active Name, Predecessors, or
Resource Names cell; on the ID column, it deletes the task (a summary asks
first: Enter deletes it with its subtasks, Esc cancels). A task added with Task › Insert
› Task (keytip Alt, T, N; here, in yppxy and through projctl's `task.add`) is 1
day, estimated unless the plan's `NewTasksEstimated` is off, and with the plan's
`Autolink` on (the default) it is linked into the finish-to-start chain it
splits: A→B becomes A→N→B, N→B keeping the lag. Alt+Shift+Right/Left
indent/outdent; Alt+Right/Left pan the Gantt and Alt+Home moves it back to
the project start, as in Project;
Ctrl+Shift+L toggles leveling. Ctrl+F2 links the selected task (it opens
the Predecessor prompt, as Task › Schedule › Link Tasks does) and
Ctrl+Shift+F2 removes its links, as Project does. Ctrl+F, F3, Ctrl+Z/Y/S/E
retain find, repeat find,
undo/redo, save, and export. The former bare-letter commands are available on
the ribbon; letters now start cell edits.

The ribbon (suite and yppxy) holds only Microsoft Project's commands, each with
Project 2024's label and screentip, which differ for icon-only commands:
Task › Schedule › Indent / Outdent / Link Tasks show the screentips Indent
Task, Outdent Task and Link the Selected Tasks; Information... is View Task
Information; Move, Milestone and Blank Row are Move Task, Insert Milestone and
Insert Blank Row; View › Timeline is Timeline View. The harness's
`ribbon-click` finds a command by either name. docxy's extras have no ribbon
button: rename and set durations in the cells (yppxy: Enter/F2 and `d`),
delete a task with Delete on its ID (yppxy `x`), clear resources with Delete
on Resource Names (yppxy: Assign with an empty name), export with Ctrl+E or
File › Export, and scroll the Gantt with Alt+Left/Right and Alt+Home (yppxy
h/l and Alt+Home). The Report tab stays, with no groups yet.

Ctrl+C / Ctrl+X / Ctrl+V copy, cut and paste cells through the system clipboard
as tab-separated text (#369). Copy takes the cursor cell's edit text (a
duration as `2d`, so it pastes back exactly); Cut clears it as Delete does on
Name, Predecessors and Resource Names, and elsewhere only copies (it never
deletes a task). Paste overwrites from the cursor cell as Project does with
cells selected: line i goes to the i-th shown row below, field j to the j-th
column right, ID fields are ignored, and lines past the last task append
tasks; no row is inserted. The whole paste is one undo step, and a field that
cannot apply cancels it, naming the cell.

A summary's subtasks can be hidden and shown again, as in Project: View › Data
› Show Subtasks / Hide Subtasks (Alt+Shift+Plus / Alt+Shift+Minus; yppxy `+` or
`=` and `-`), or a click on the `▾`/`▸` beside the summary's name. Hide Subtasks
on a subtask collapses its summary. Hidden rows leave the grid and the Gantt,
and the arrow keys, Ctrl+Up/Down and Ctrl+Home/End (Home/End in yppxy) and
clicks move over the rows shown. Collapsing is
view state kept by `projcore::Editor`: it is not an edit, not undone, not saved
(MSPDI has no element for it) and not restored with the session. The selected
row is never hidden: selecting or finding a hidden task, or moving it under a
collapsed summary, shows it, while a cursor left inside a collapsed subtree by
a delete or an undo moves up to the summary. A task typed below a collapsed
last summary becomes its sibling.

Dates use `YYYY-MM-DD`. On an auto task, Start sets SNET and Finish sets FNET at
the chosen working day's calendar finish (non-working Finish dates are rejected).
On a manual task, Start moves the task to that day's first working time (08:00
on a non-working day) keeping its duration, and Finish sets its finish at the
day's last working time (17:00 on a non-working day) and its duration to the
working time in between; neither adds a constraint. Predecessors use
displayed task IDs, e.g. `2, 3SS+2h, 4FF-7m`. Resource names are comma-separated,
and unknown names create resources. A new work assignment starts at the
resource's Max. Units capped at 100%; `Name[NN%]` sets explicit units instead (NN > 0; over-allocation such as `Bob[150%]` is
allowed). The cell shows `Name[NN%]` for a work assignment that is not at 100%,
and deleting the bracket resets it to 100%. A material assignment always shows
its quantity and the resource's material label, as Project does:
`Cement[5 tons]` (`Cement[5]` without a label). `Name[<qty>]` or
`Name[<qty> <label>]` sets the quantity (qty > 0; the label, if typed, must be
the resource's own, case-insensitively), its work is the quantity in hours, and
deleting the bracket resets the quantity to 1. A material takes no `NN%`, and a
work or cost resource no quantity; rate-based material (`5 tons/day`) is not
supported. In the Resource Names cell, a token
that names, or equals the shown `Name[...]` text of, one of the task's
assignments keeps that assignment, with exact spellings before case-insensitive
ones (a token that fits two resources equally well is ambiguous and rejected);
otherwise an existing resource whose name matches the whole token wins;
otherwise a name ending in `[...]` must hold valid units for its resource's
kind. Retained assignments whose text shows the same units keep their units/work.

Duration, work and units follow the task's Type, as in Project (a task
without one is Fixed Units, and not effort-driven):
- A duration edit gives each work assignment work = duration x units on a
  Fixed Units or Fixed Duration task; a Fixed Work task keeps its work and its
  units change (from an assignment's delay to the task finish).
- A units edit on a Fixed Units or Fixed Work task keeps the assignment's work
  and the task takes the duration its longest assignment needs (the others
  keep their work); on a Fixed Duration task the work follows the units.
- A new assignment gets work = duration x units. On an effort-driven task (a
  Fixed Work task always is) that had work, adding or removing work resources
  keeps the total work and splits it by units: the duration changes, or on a
  Fixed Duration task the units do. Adding resources and changing units in
  one edit splits by the new units.
- Material and cost assignments take no part. Summaries, milestones, tasks
  with a contoured assignment and, for effort-driven, a delayed one keep work
  = duration x units. A manual task keeps its start and its finish follows.
An unchanged edit preserves history and existing constraints. Cycles retain the
engine's existing best-effort scheduling behavior. Names containing commas cannot
be entered individually through the resource-list syntax, and a token whose new
name would contain a comma is rejected rather than created. A comma inside a
`[...]` pair that ends a token (only whitespace, then a comma or the end of the
text, follows its `]`) does not split, so a material label such as
`Cement[5 bags, 50 lb]` stays in its token. Brackets pair like parentheses; a
stray `[` or `]`, or a pair closed mid-name (`Crew [A, Bob] Jr`), protects
nothing. Brackets that pair across names into a token ending in `]` (a
resource named `Crew [A` followed by `Bob]`) make one token, which is rejected
unless it names a resource.

## Roadmap

Done: MSPDI read/write · CPM (links · lag · constraints · slack · critical ·
summaries) · resource leveling · baselines · Markdown/Mermaid Gantt export ·
native `.yppx` · `mppread` CFB + metadata · the full `yppxy` app (ribbon,
backstage, live Gantt, editing, undo/redo, find, vim mode, themes).

Next, roughly in order:

1. **Broader `.mpp` support** — recognized MPP9 and current Project task
   layouts import validated names, dates, outline levels, predecessor links and
   current-layout task fields.
   Other layouts and nonzero link lag need further oracle-backed work. The
   corpus workflow is documented in `corpus/mpp/README.md`.
2. **Richer leveling** — priority-ordered (not just topological), multi-calendar,
   optional task splitting, and a "resource-critical" flag.
3. **Assignment editing depth** — editing work (a Work column) and a task's
   Type and Effort Driven, over-allocation highlighting in the UI. Units per
   assignment, task types and effort-driven durations are done.
4. **Views** — filtering, grouping, and a resource-usage view.
