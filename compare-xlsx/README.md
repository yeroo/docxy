# xlsxy ⇄ Excel compare launcher

A small WPF app that sits at the **left edge** of the screen and lets you browse
the [classified xlsx corpus](../corpus) by **category, feature tag, or folder**.
Pick a workbook and it opens that file in **xlsxy (terminal)** and **Microsoft
Excel** side by side, so you can compare calculated values and rendering at a
glance. It's the spreadsheet twin of [`../compare`](../compare) (docxy ⇄ Word).

## Behaviour

- Launches xlsxy (a fresh terminal window) and Excel, tiled to the right of the
  launcher: `[launcher strip] [xlsxy] [Excel]`.
- Once a workbook is open the launcher **collapses to a thin strip** on the left;
  move the mouse over it and it **expands over xlsxy** so you can pick another
  file, then collapses again.
- Switching to a different workbook **closes the previous Excel + terminal** and
  reopens the new file in both.
- Closing the launcher closes Excel and the terminal too.
- **Search** matches on file name, folder, tag, **or worksheet-function name** —
  so typing `XLOOKUP` or `YEARFRAC` jumps straight to the workbooks that use it.
- The tree tooltip lists every function a workbook calls, so you can see at a
  glance which features each file exercises.

## Requirements

- .NET 9 SDK (`dotnet`)
- Rust toolchain (`cargo`) — the build rebuilds xlsxy automatically
- Microsoft Excel (automated via COM)
- `corpus/classification-xlsx.json` generated —
  `python corpus/tools/classify_xlsx.py`

## Run

```powershell
cd compare-xlsx
dotnet run -c Release
```

Building the launcher first runs `cargo build --release -p xlsxy` (a pre-build
MSBuild step), so `dotnet run` always launches the current xlsxy — the launcher
uses `target/release/xlsxy.exe`. cargo is incremental, so it's a no-op when
xlsxy is unchanged.

## Corpus

`classify_xlsx.py` classifies the workbooks in `corpus/xlsx/` in place. To bring
in an external tree of tricky/real workbooks and classify them too:

```powershell
python corpus/tools/classify_xlsx.py --src "C:\path\to\xlsx-files"
```

That copies them under `corpus/xlsx-ext/` (preserving structure) and folds them
into the same manifest.

## Notes

- Excel opens workbooks **read-only** so comparisons don't accidentally edit the
  corpus copies, and `DisplayAlerts` is off to suppress modal prompts.
- The Excel window is located by its `XLMAIN` window class + a title match on the
  file name; if you already have many Excel windows open it may grab the wrong
  one. Close stray Excel windows for best results.
- xlsxy doesn't yet reload on external save, so this is an open-and-look tool.
