# projcore MSPDI seed corpus

Tiny, single-feature MS Project XML (MSPDI) files used to validate the
`projcore` CPM scheduler. Each file isolates exactly one scheduling feature so a
failing assertion points at a single code path.

## Self-oracling

There is no free high-fidelity oracle for project scheduling — MS Project is the
reference implementation and isn't scriptable in CI. So each file embeds the
`Start`/`Finish` that **Project itself** would compute, hand-verified against a
standard calendar. `projcore/tests/corpus.rs` reads each file, runs the CPM
scheduler, and asserts the computed dates equal the embedded ones. The corpus
therefore validates the scheduler against Project's semantics without Project.

- **Anchor:** Monday 2026-03-02 08:00.
- **Calendar:** Standard, 8h/day, Mon–Fri (08:00–12:00, 13:00–17:00); weekends
  off. File 12 adds a second calendar with Saturday working.

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

See `manifest.json` for machine-readable tags.

## Regenerating

```
python3 corpus/tools/gen_mspdi_corpus.py
```

Pure stdlib Python; no external tools. Edit the generator (not the files) to add
cases, then re-run and confirm `cargo test -p projcore` stays green.
