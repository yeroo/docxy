# legacy-format corpus

The 17 workbooks of `corpus/xlsx`, saved by **Microsoft Excel 16** as
Excel 97-2003 Workbook (`.xls`), Excel Binary Workbook (`.xlsb`) and
OpenDocument Spreadsheet (`.ods`): 51 files. They are the oracle for
gridcore's legacy readers (`gridcore::legacy`, #603). Importing
`<stem>.<ext>` must give the workbook `corpus/xlsx/<stem>.xlsx` holds.

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
the three formats. The output depends on the Excel build, so regenerate only
when the `.xlsx` corpus changes, and rerun the test.
