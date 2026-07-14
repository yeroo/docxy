# MS Project .mpp corpus generator — design

**Date:** 2026-07-13
**Status:** approved (design review with user)
**Consumers:** `mppread` / `projcore` / `yppxy` (the MS-Project third of the
doc → docx, xls → xlsx, mpp → yppx trilogy)

## Problem

`mppread` reverse-engineers the undocumented binary `.mpp` format against MSPDI
XML oracle exports (see `corpus/mpp/README.md`). Today the corpus is
bring-your-own: third-party `.mpp` files that can't be committed (licensing),
don't come with paired oracles, and don't systematically cover features. The
known decode gaps — link **lag**, the **newest MPP** generation, MPP8 — all
need files with *known contents*. The cloud development session has no real
`.mpp` files at all.

We have MS Project installed locally (Office16 / `WINPROJ.EXE`, COM ProgID
`MSProject.Application` registered) — the one tool that can mint real `.mpp`
files whose contents we control and therefore own and may publish.

## Goal

A **VSTO AddIn for MS Project** that builds **one project cumulatively, one
feature per step**, saving a snapshot pair after every step:

- `NN-slug.mpp` — real binary .mpp in the running Project's current format
  (the newest MPP generation — itself a known decode gap), and
- `NN-slug.xml` — the same state as MSPDI XML, the documented oracle.

Adjacent snapshots differ by **exactly one feature**, so each feature can be
localized by diffing snapshot N against N+1 (per CFB stream, using `mppread`'s
existing storage-tree tooling), with the paired XML stating what the change
means. The full run is the "broad MS Project tour" (~46 steps).

## Non-goals

- No MPP8/MPP9 generation (needs Project 98/2003 installs; out of scope).
- No headless/CI generation (Office automation requires an interactive
  desktop; generation runs on the developer's machine).
- No decoding work itself — this project only *produces* corpus material.

## Deliverables & repo layout

Everything lives in a **new private corpus repository** (working name
`mpp-corpus`), separate from docxy:

```
mpp-corpus/
  addin/            # C# VSTO solution (the generator)
  snapshots/        # generated NN-slug.mpp + NN-slug.xml pairs
  manifest.json     # machine-readable step descriptions + expected values
  README.md         # what this is, how to regenerate, licensing note
```

Distribution: GitHub releases ship a zip of `snapshots/` + `manifest.json`.
In docxy, `corpus/mpp/README.md` gains a pointer and a small fetch script that
downloads the latest release into the (still gitignored) `corpus/mpp/`.
Because the repo is **private**, the fetch script authenticates via `gh
release download` (or a `GITHUB_TOKEN`), and any session that needs the corpus
— including the cloud session — must hold a token with access to the repo;
anonymous `curl` won't work.

## The AddIn

- VSTO Project Add-in, **C# / .NET Framework 4.8**, built with **VS Community
  2022** + the *Office/SharePoint development* workload (workload not yet
  installed; first implementation step — needs admin, e.g.
  `setup.exe modify --installPath "C:\Program Files\Microsoft Visual
  Studio\2022\Community" --add Microsoft.VisualStudio.Workload.Office
  --includeRecommended --passive`).
- UI: one ribbon tab **Corpus** with a **Generate corpus** button (folder
  picker, then run) and a progress/abort dialog.
- All generation logic lives in a plain `CorpusBuilder` class the ribbon
  merely invokes, so the same code could later be driven without the ribbon.

## The feature script

An ordered list of steps in code — `(slug, description, Action<Project>)` —
each mutating the same live project. The pinned step list:

**Shell**
1. `empty` — new project; pinned start date (Mon 2025-01-06), Standard
   calendar, pinned author
2. `properties` — title, subject, author, company, comments

**Tasks**
3. `first-task` — one task, 3d
4. `more-tasks` — four more tasks, durations 1d / 2d / 5d / 10d
5. `milestone` — zero-duration milestone
6. `unicode-name` — long task name (~200 chars, Latin + Cyrillic + CJK +
   emoji) — stresses the Var2Data string decode
7. `task-notes` — notes on a task
8. `outline-2` — summary task with two children (indent)
9. `outline-deep` — a depth-4 subtree
10. `manual-task` — manually scheduled task
11. `inactive-task` — inactive task
12. `recurring-task` — weekly recurrence × 4
13. `split-task` — one task split into two segments
14. `estimated-duration` — duration flagged estimated
15. `elapsed-duration` — 3 elapsed days
16. `deadline` — deadline on a task
17. `priority` — non-default priority (900)
18. `constraint-snet` — Start No Earlier Than
19. `constraint-fnlt` — Finish No Later Than
20. `constraint-mso` — Must Start On
21. `task-calendar` — built-in *24 Hours* calendar assigned to a task
22. `hyperlink` — hyperlink on a task
23. `custom-fields` — Text1 / Number1 / Flag1 / Date1 on tasks

**Links**
24. `link-fs` — Finish-to-Start
25. `link-ss` — Start-to-Start
26. `link-ff` — Finish-to-Finish
27. `link-sf` — Start-to-Finish
28. `link-lag` — FS + 2d lag *(top decode gap)*
29. `link-lead` — FS − 1d lead
30. `multi-pred` — one task with two predecessors

**Calendars**
31. `calendar-hours` — edit Standard working times
32. `calendar-6day` — new base calendar with working Saturdays
33. `calendar-holiday` — exception (holiday) in a calendar

**Resources**
34. `resource-work` — work resource "Alice"
35. `resource-material` — material resource with label
36. `resource-cost` — cost resource
37. `resource-rates` — max units, standard/overtime rates, cost per use
38. `resource-calendar` — vacation day on a resource calendar

**Assignments**
39. `assign-single` — one resource at 100%
40. `assign-multi` — two resources at 50% each
41. `task-types` — fixed-work and fixed-duration tasks, effort-driven off
42. `work-contour` — non-flat contour (back-loaded)
43. `assignment-delay` — delayed assignment

**Tracking**
44. `baseline` — save baseline
45. `baseline1` — save Baseline1 (multiple baselines)
46. `progress` — % complete and actual start on several tasks

Exact object-model calls per step are implementation detail (e.g. how a task
split is applied); the *observable state* per step is what the manifest pins.

## Snapshot mechanics & determinism

- After each step: `FileSaveAs(path, pjMPP)` then `FileSaveAs(path, pjXML)`;
  continue building from the in-memory project. (`FileSaveAs` renames the
  active document — the builder tracks/restores this.)
- Deterministic content: fixed start date, fixed author, no volatile text, so
  regeneration produces *logically* identical files.
- Caveat (accepted): Project stamps save times/GUIDs, so byte-identical
  regeneration is impossible. Diffing is done **per CFB stream** — noise stays
  confined to metadata streams; `mppread`'s `inspect` maps the stream tree.
- Old-format probe: at startup, probe once whether the installed Project still
  offers an older `.mpp` Save As format (e.g. 2007). If yes, emit a third file
  per snapshot; if not, skip silently and record the fact in the manifest.

## Manifest

`manifest.json` records the generator version, MS Project version/build, the
formats emitted, and per step: index, slug, description, what changed, and the
key expected values (task names, dates, durations, link types, lag values,
resource/assignment facts). Machine-readable so docxy can grow a scoreboard
test: `mppread` decodes snapshot N → assert against manifest → skip when the
corpus is absent (same pattern as `mppread/tests/real_mpp.rs`).

## Error handling

A cumulative corpus is worthless past a silently-failed step, so any step
failure **stops generation** with an error naming the step; snapshots already
written stay on disk. `DisplayAlerts`/`ScreenUpdating` are disabled where the
API allows so no Project dialog can hang a run; the abort button closes the
project without saving.

## Verification

After a full generation run, before publishing a release:

1. `cargo run -p mppread --example streams/inspect/tasknames` against several
   snapshots — the container, names, dates decode (or fail informatively —
   this generation of MPP is a known gap the corpus exists to close).
2. Spot-check decoded names/dates/links against the paired `.xml` oracle.
3. Open a snapshot pair in `yppxy` (`yppxy NN-slug.xml`) to confirm the MSPDI
   side round-trips.

## Decisions log

- Distribution: **separate private corpus repo + release zips** (user choice;
  keeps docxy lean; private means fetching requires an authenticated `gh` /
  token rather than anonymous `curl`).
- Scope: **broad MS Project tour** (~46 steps), not just decoder-driven
  (user choice).
- Vehicle: **real VSTO AddIn** with ribbon button (user choice over external
  COM script); VSTO workload to be installed into VS Community 2022 (user
  choice over a plain-COM add-in).
- **Amendment (2026-07-13, user decision):** the AddIn ribbon was skipped.
  The headless `MppCorpus.Runner` (built as the AddIn's testable core)
  generates the complete corpus on its own — including the unplanned MPP12
  old-format emission — so the ribbon added no generation capability. The
  Runner is the regeneration tool of record.
- **Amendment (2026-07-14, user decision):** distribution follows the
  docxy-corpus pattern learned from docxy main — the corpus is plain git
  content in the private repo, fetched into docxy's git-ignored
  `corpus/mpp/` by clone-based scripts (`corpus/tools/fetch-mpp-corpus.*`).
  No release zips are published; the earlier release-zip decision is
  superseded.
