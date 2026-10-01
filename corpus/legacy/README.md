# legacy-format corpus

The 17 workbooks of `corpus/xlsx`, saved by **Microsoft Excel 16** as
Excel 97-2003 Workbook (`.xls`), Excel Binary Workbook (`.xlsb`) and
OpenDocument Spreadsheet (`.ods`): 51 files. They are the oracle for
gridcore's legacy readers (`gridcore::legacy`, #603). Importing
`<stem>.<ext>` must give the workbook `corpus/xlsx/<stem>.xlsx` holds.

`extra/` holds workbooks Excel built itself, so their `.xlsx` sources are
Excel's too: `chart-embedded` has a sheet with formulas and an embedded
column chart (in the `.xls`, a chart substream nested in the worksheet's).

`addin/` holds `addin-udf`, which Excel also built itself. It is saved as
`.xlsx` and `.xlsb` only, because Excel's SaveAs `.xls` hangs on a dialog for
it. It calls `EUROCONVERT` from the add-in that ships with Office
(`Library\EUROTOOL.XLAM`). Excel stores that call as a library external link:
`[1]!EUROCONVERT(…)` with `xl/externalLinks/externalLink1.xml` in the
`.xlsx`, and a ptgNameX into a BrtSupBookSrc book in the `.xlsb`. The main
test doesn't cover it (that test needs all three formats). Instead, two tests
in `gridcore/tests/legacy.rs` check that the `.xlsb` import and its save as
`.xlsx` keep the formula and the link the way Excel's `.xlsx` has them (#888).

## The test over this corpus (runs in CI)

`gridcore/tests/legacy.rs` opens each file with `open_workbook` and checks it
against the `.xlsx`:

- (a) the same sheets, in order;
- (b) the same values, both ways (no cell missing, none extra);
- (c) a formula wherever the source has one;
- (d) recalculating the import reproduces the source's cached values;
- (e) the formulas parse to the same expression;
- (f) the same number-format codes;
- (g) the same date system and defined names;
- (h) (b), (f) and (g) still hold after `save_xlsx` and a reload.

Each exception is a commented entry in the test's `ALLOW` list, which the
test prints. There are eight, for three causes:

- **calc-3d, all three formats (cached values):** Excel read the source's
  unquoted `SUM(Q1:Q3!A1:A1)` with `Q1` as a cell, so the values it cached
  are `#VALUE!`. gridcore recalculates the stored formula correctly.
- **calc-refs.ods (sheet name, and the one formula naming it):** Excel's
  `.ods` writer renamed the sheet "Calc Zone" to `Calc_Zone` in the file.
- **shape-salestable, all three formats (the 12 formulas with structured
  references):** they arrive as the ranges they cover. Excel wrote them that
  way to the `.xls` (no tables) and the `.ods`; the `.xlsb` keeps its table,
  but the import doesn't model tables, so the reader resolves them the same
  way.

So seven entries come from what Excel wrote, and one (the `.xlsb`
structured references) from the reader not importing tables.

## Regenerating

On Windows with Excel installed, from the repo root:

```powershell
powershell -File scripts/make-legacy-fixtures.ps1
```

It opens each `corpus/xlsx/*.xlsx` in a fresh Excel process and saves it in
the three formats. Then it rebuilds `extra/`: Excel creates each of those
workbooks from scratch and saves its `.xlsx` source as well as the three
formats. Last it rebuilds `addin/`. `-Sections corpus,extra,addin` picks
which of the three parts to rebuild. The output depends on the Excel build,
so regenerate only when the `.xlsx` corpus (or the script) changes, and rerun
the tests.
