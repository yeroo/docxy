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
| `gantt` | export a scheduled project as a Markdown/Mermaid Gantt chart |
| `yppx` | the native `.yppx` OPC package (ZIP + `[Content_Types].xml` + `project.xml`) |

The model is **pure input** — the scheduler never mutates it; it returns a
separate `Schedule`. MSPDI's own computed `Start`/`Finish` are captured as
`stored_*` and used as an **oracle** for the scheduler.

## The scheduling model

- **Tasks** have a duration in *working minutes*, an outline level (summary
  tasks own the deeper rows below them), and may be milestones (zero duration).
- **Dependencies** are the four link types with lag/lead: Finish-to-Start,
  Start-to-Start, Finish-to-Finish, Start-to-Finish. Lag is stored in MSPDI as
  *tenths of a minute* — one of several unit traps the reader normalizes.
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
  plus the forward-affecting constraints (MSO/SNET/FNET/MFO).
- **Backward pass** → late start/finish from the project finish, plus the
  backward-affecting constraints (MFO/FNLT/SNLT/MSO).
- **Total & free slack**, the **critical** flag (slack ≤ 0), and **summary
  rollup** (a summary's dates derive from its descendants).

Leaf tasks are ordered by a Kahn topological sort of the dependency graph;
cycles fall back to input order.

### Verification

There's no free high-fidelity oracle for scheduling (Project isn't scriptable in
CI), so the corpus is **self-oracling**: `corpus/mspdi/` holds a dozen tiny
one-feature MSPDI files, each embedding the `Start`/`Finish` that Project itself
would compute (hand-verified against a standard 8h/day Mon–Fri calendar).
`projcore/tests/corpus.rs` reads each file, runs the scheduler, and asserts the
computed dates match — and also runs each file through **MSPDI → `.yppx` →
back** to prove the writer and OPC container are lossless. Regenerate with
`python3 corpus/tools/gen_mspdi_corpus.py`.

## Resource leveling

`schedule::level(proj)` runs CPM, then delays tasks so no work resource is
booked beyond its capacity. It processes tasks in topological order; each task
starts no earlier than (a) its CPM early start and (b) the earliest time all its
resources have free capacity for its whole duration (a sweep-based peak-load
check that supports fractional units). A predecessor's leveling delay propagates
to its successors, preserving every link's gap. v1 is single-calendar,
delay-only, and treats a task's occupation as its wall-clock span; multi-calendar
leveling and task splitting are future work. `yppxy` toggles the overlay with
`L` (View ▸ Level).

## Formats

- **MSPDI** (`.xml`) — Project's documented interchange format. `projcore` reads
  and writes it; this is how schedules move to and from real Project.
- **`.yppx`** — the native package: an OPC ZIP (`[Content_Types].xml` +
  `project.xml`) built on `opccore`, the project analog of `.docx`/`.xlsx`. The
  `project.xml` part is MSPDI-compatible, so `.yppx` stays interoperable — unzip,
  rename, and Project opens it — while giving us a container to grow.
- **`.mpp`** — the legacy binary. It's an OLE2 **Compound File** (MS-CFB), which
  `mppread` reads exactly. Its metadata streams are OLE **property sets**
  (MS-OLEPS), also decoded exactly (title/author/company/dates), so
  `yppxy legacy.mpp` opens with the right name. The task/resource **var-data
  blocks inside are undocumented** and version-specific (MPP8/9/12/14) — decoding
  them is the one genuinely reverse-engineering-only layer.

## Roadmap

Done: MSPDI read/write · CPM (links · lag · constraints · slack · critical ·
summaries) · resource leveling · baselines · Markdown/Mermaid Gantt export ·
native `.yppx` · `mppread` CFB + metadata · the full `yppxy` app (ribbon,
backstage, live Gantt, editing, undo/redo, find, vim mode, themes).

Next, roughly in order:

1. **`.mpp` task decoder** — the undocumented var-data blocks. Blocked on a real
   `.mpp` corpus; the workflow (drop files in `corpus/mpp/`, map streams with the
   `mppread` `streams` example, reverse each block against an MSPDI oracle export)
   is documented in `corpus/mpp/README.md`.
2. **Richer leveling** — priority-ordered (not just topological), multi-calendar,
   optional task splitting, and a "resource-critical" flag.
3. **Assignment editing depth** — units/work per assignment, effort-driven
   durations, over-allocation highlighting in the UI.
4. **Views** — filtering, grouping, and a resource-usage view.
