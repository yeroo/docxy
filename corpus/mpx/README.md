# projcore MPX corpus (#309)

MPX is the comma-separated text interchange format Microsoft Project 4.0 to
2010 saved and later versions still open. Each `NN-*.mpx` here is the MPX of
`corpus/mspdi/NN-*.xml`, its **twin**, written by
[MPXJ](https://www.mpxj.org/) 16.8.0 (Apache-2.0), the tool the issue's
reproduction used. Project no longer saves MPX, so these files are an oracle
of MPXJ's writer, not of Project's. Do not edit them by hand: regenerate them.

| Path | Date order (record 12) |
|---|---|
| `*.mpx` | month/day/year, as US Project writes |
| `dmy/*.mpx` | day/month/year |
| `ymd/*.mpx` | year/month/day |

`projcore/tests/mpx_corpus.rs` reads every file with `projcore::mpx`,
schedules it, and asserts it has its twin's tasks in its twin's order, with
the same names, outline levels and durations, scheduled to the same start
and finish. Where the MPX file does not carry what the twin does, the test
leaves that field out of the pair and says why (`EXCLUDED`):

- task calendars (12, 16, 23, 25, 28): an MPX task row has no calendar field;
- manual scheduling (19, 27): MPX has no Manual field;
- constraint times (29): MPX dates are whole days;
- Honor Constraints off (31): MPX does not carry the option, so 30 and 31
  read to the same plan;
- 28's durations: MPXJ writes them in days to two decimals.

## Variants

`variants/` holds MPX shapes the MSPDI corpus does not produce, each MPXJ's
MPX of one corpus file with one change, written by
`corpus/tools/gen_mpx_variants.py`; `projcore/src/mpx/tests.rs` reads them:

- `month-units.mpx`: 06-lag with B lasting 1 month and lagged 1 elapsed
  month (MPXJ writes `1mon`, `1FS+1emon`);
- `weekday-dates.mpx`: 06-lag in date format 9 (`Mon 03/02/26`);
- `multiline-note.mpx`: 02-link-fs with a two-line note (MPXJ writes the
  line break as U+007F);
- `month-names-{full,full-time,short,weekday}.mpx` (#1805): 09-constraint-snet
  in the date formats that spell the month (3 MDY `March 05 2026`, 2 MDY with
  the time, 5 DMY `05 Mar '26`, 10 YMD `Thu '26 Mar 05`);
- `locale-<l>.mpx` (#1805): one plan (20-task-fields with an SS+1d and an
  FF+1ed link, a Start No Earlier Than constraint and an assignment of a
  resource accruing at End) in each locale MPXJ's writer knows but Chinese
  (en de fr it pt sv es ru), with the locale's words, units, link types,
  date format, separators and code page (Russian is Windows-1251). The
  script reads each back with MPXJ in its locale and fails unless it gives
  the English file's tasks.

## Regenerating

```sh
pip install mpxj==16.8.0 JPype1    # needs a Java runtime (JAVA_HOME)
python corpus/tools/gen_mpx_corpus.py        # MDY, into corpus/mpx/
python corpus/tools/gen_mpx_corpus.py DMY    # into corpus/mpx/dmy/
python corpus/tools/gen_mpx_corpus.py YMD    # into corpus/mpx/ymd/
python corpus/tools/gen_mpx_variants.py      # into corpus/mpx/variants/
```

Each run writes the generation day into record 30's current date; the reader
does not use it, so a regeneration changes nothing else.
