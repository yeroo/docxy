# projcore MSPDI seed corpus

Tiny, single-feature MS Project XML (MSPDI) files used to validate the
`projcore` CPM scheduler, resource and baseline round trips. Each file isolates one feature so a
failing assertion points at a single code path.

## Embedded expectations

There is no free high-fidelity oracle for project scheduling — MS Project is the
reference implementation and isn't scriptable in CI. So each file embeds the
hand-derived `Start`/`Finish` expectations for a standard calendar.
`projcore/tests/corpus.rs` reads each file, runs the CPM scheduler, and asserts
the computed dates equal the embedded ones. It also checks that every project
has a critical leaf and that its last-finishing leaves have nonpositive slack.

The owner checked the SF shapes in files 05 and 14 against Microsoft Project
2021 in [issue #53](https://github.com/yeroo/docxy/issues/53). File 05's B finish
was corrected from March 3 at 17:00 to March 4 at 08:00, the instant A starts.
File 14 records Project scheduling the SF successor before the project start.
File 16 records the 24-hour calendar dates verified against Project 2021 in
[issue #58](https://github.com/yeroo/docxy/issues/58): a three-day duration
(24 working hours) finishes exactly one day after its start.
The other fixtures remain hand-derived expectations, not independently verified
Project outputs.

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

See `manifest.json` for machine-readable tags.

## Regenerating

```
python3 corpus/tools/gen_mspdi_corpus.py
```

Pure stdlib Python; no external tools. Edit the generator (not the files) to add
cases, then re-run and confirm `cargo test -p projcore` stays green.
