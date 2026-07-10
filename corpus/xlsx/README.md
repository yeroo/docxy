# xlsx conformance corpus

Workbooks whose formula cells carry **cached values computed by an engine
other than gridcore**, used as an oracle: `gridcore` recalculates every
formula and diffs against the embedded values. Run the scoreboard on any
workbook with:

```sh
xlsxy file.xlsx --verify
```

CI enforces a clean scoreboard over this directory via
`gridcore/tests/conformance.rs`.

## Files

- **`oracle-basic.xlsx`** — 42 formulas over math/aggregation, criteria
  (SUMIF/COUNTIF with wildcards), text, lookup (VLOOKUP/MATCH/INDEX),
  logic, whole-column refs, and financial (PMT/NPV/STDEV). Cell layout:
  data in `A`/`B`, formulas in `D`. Cached values computed with the Python
  [`formulas`](https://pypi.org/project/formulas/) engine, then one
  correction applied where that engine itself deviates from Excel
  (`TRIM` must collapse internal space runs — D34).

## Adding corpus files

The best oracle is **real Excel**: open a workbook in Excel, let it
calculate, save — the cached values in the file are the ground truth.
Files produced by LibreOffice or other engines work too but may carry
their own deviations; note any corrections here, as done for D34 above.
Keep workbooks small and focused (one area of the function surface each),
and prefer values that don't depend on locale, timezone, or the current
date (`--verify` excludes volatile formulas automatically).
