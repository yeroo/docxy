# projcore MSPDI seed corpus

Tiny, single-feature MS Project XML (MSPDI) files used to validate the
`projcore` CPM scheduler, resource and baseline round trips. Each file isolates one feature so a
failing assertion points at a single code path.

## Embedded expectations

There is no free high-fidelity oracle for project scheduling — MS Project is the
reference implementation and isn't scriptable in CI. So each file embeds every
task's `Start`/`Finish`, `TotalSlack` (tenths of a minute, as MSPDI stores it)
and `Critical` for a standard calendar.
`projcore/tests/corpus.rs` reads each file, runs the CPM scheduler, and asserts
the computed dates, slack and critical flags equal the embedded ones. It also
checks that every project has a critical leaf and that its last-finishing
leaves have nonpositive slack.

All 18 files were verified against Microsoft Project 2021 in
[issue #74](https://github.com/yeroo/docxy/issues/74) by
`corpus/tools/verify_mspdi_project.py`. The script has Project schedule a copy
of each file with every task's `Start`, `Finish`, `TotalSlack` and `Critical`
removed, so Project cannot read the oracle back. Before comparing, it checks
that Project imported each task's duration, links, lags, constraint and
calendar as the file states them. Every file carries
`<ProjectExternallyEdited>0</ProjectExternallyEdited>`: without it Project
rederives durations from Start/Finish and imports tasks that start at the
project start with zero duration. The #74 run found no oracle to correct, and
it reproduces the owner's earlier manual runs below.

The owner checked the SF shapes in files 05 and 14 against Microsoft Project
2021 in [issue #53](https://github.com/yeroo/docxy/issues/53). File 05's B finish
was corrected from March 3 at 17:00 to March 4 at 08:00, the instant A starts.
File 14 records Project scheduling the SF successor before the project start.
File 16 records the 24-hour calendar dates verified against Project 2021 in
[issue #58](https://github.com/yeroo/docxy/issues/58): a three-day duration
(24 working hours) finishes exactly one day after its start.
File 17 records the FNLT conflict verified against Project 2021 in
[issue #60](https://github.com/yeroo/docxy/issues/60): the constraint takes
precedence over the FS link, with -5 days total slack on both tasks.
File 18 records the FS milestone dates verified against Project 2021 in
[issue #59](https://github.com/yeroo/docxy/issues/59): zero-duration successors
keep the predecessor's finish instant. Only zero-lag links were verified;
for nonzero lag the scheduler uses the finish side of the successor calendar's
working-time boundary, which remains unverified against Project.
File 19 pins manually scheduled tasks
([issue #77](https://github.com/yeroo/docxy/issues/77)) and is the one fixture
**not** verified against Project 2021: its Start/Finish are what a manual task
keeps by definition, but its `TotalSlack`/`Critical` are hand-derived from our
scheduler, including the violated link (Review, pinned two days before Design
finishes, gets -2 days). `verify_mspdi_project.py` keeps the tasks this file
marks `<Manual>1</Manual>` manual, so a Project run can check it.
File 20 records a missed task Deadline
([issue #100](https://github.com/yeroo/docxy/issues/100)): B's deadline is
five days before its finish, so A and B both carry -5 days total slack and no
date moves. It was hand-written, and its oracle is taken from the issue's
Project 2021 capture of `15-deadline-missed` in the private spec corpus. It was
**not** checked by `verify_mspdi_project.py`, which does not check the Deadline
on import.

- **Anchor:** Monday 2026-03-02 08:00.
- **Calendar:** Standard, 8h/day, Mon–Fri (08:00–12:00, 13:00–17:00); weekends
  off. File 12 adds a second calendar with Saturday working; file 16 adds the
  built-in 24 Hours calendar, working midnight to midnight every day.

## Files

| File | Feature | What it pins |
|------|---------|--------------|
| `01-single-task` | basic | duration → finish, ISO-8601 units |
| `02-link-fs` | finish-to-start | the common dependency |
| `03-link-ss` | start-to-start | SS link math |
| `04-link-ff` | finish-to-finish | FF link math |
| `05-link-sf` | start-to-finish | the rare SF link |
| `06-lag` | +2d lag | LinkLag in tenths-of-a-minute |
| `07-lead` | −1d lead | negative lag / overlap |
| `08-milestone` | milestone + SNET | zero duration, start=finish |
| `09-constraint-snet` | Start-No-Earlier-Than | hard forward constraint |
| `10-summary` | outline rollup | summary derives from children |
| `11-resource-assignment` | resource + assignment | units × work parsing |
| `12-calendar-6day` | custom calendar | Saturday working changes the finish |
| `13-resource-fields` | resource round trip | Work identity/rates, Cost kind, Material label in MSPDI and `.yppx` (RES-CASE-005/006) |
| `14-link-sf-before-start` | start-to-finish before anchor | linked task starts before project start; its predecessor is critical |
| `15-baseline-slots` | baseline round trip | slots 0/1/2 retain distinct dates and recorded durations, including missing Duration, in MSPDI and `.yppx` |
| `16-24-hour-calendar` | full-day calendar | midnight-to-midnight shifts schedule continuously and survive MSPDI and `.yppx` round trips |
| `17-constraint-fnlt-conflict` | FNLT versus FS link | default constraint precedence and -5 days total slack on both tasks |
| `18-milestone-after-fs` | FS milestones | predecessor finish instants retained, including a chain with two milestones |
| `19-manual-tasks` | manually scheduled tasks | pinned before and after an FS link, an auto successor and a summary follow the pinned dates; task mode, manual fields and `NewTasksAreManual` survive MSPDI and `.yppx` |
| `20-deadline-missed` | missed Deadline | a deadline bounds late finish only: -5 days total slack on the task and its FS driver, dates unchanged |

See `manifest.json` for machine-readable tags.

## Regenerating

```
python3 corpus/tools/gen_mspdi_corpus.py
```

Pure stdlib Python; no external tools. Edit the generator (not the files) to add
cases, then re-run and confirm `cargo test -p projcore` stays green. A new or
changed oracle must come from Project: on Windows with Microsoft Project and
pywin32, and with Project closed, run

```
python corpus/tools/verify_mspdi_project.py [file.xml ...]
```

It exits nonzero on any input-fidelity failure or schedule mismatch, and it
never writes into `corpus/mspdi/`. If Project is already running it refuses to
start (exit 2): Project is a single-instance COM server, so the script would
otherwise attach to your session and close its projects without saving.
