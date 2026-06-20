# docxy ⇄ Word compare launcher

A small WPF app that sits at the **left edge** of the screen and lets you browse
the [classified corpus](../corpus) by **category, tag, or folder**. Pick a file
and it opens that document in **docxy (terminal)** and **Microsoft Word** side by
side, so you can compare renderings at a glance.

## Behaviour

- Launches docxy (a fresh console window) and Word, tiled to the right of the
  launcher: `[launcher strip] [docxy] [Word]`.
- Once a document is open the launcher **collapses to a thin strip** on the left;
  move the mouse over it and it **expands over docxy** so you can pick another
  file, then collapses again.
- Switching to a different document **closes the previous Word + terminal** and
  reopens the new file in both.
- Closing the launcher closes Word and the terminal too.

## Requirements

- .NET 9 SDK (`dotnet`)
- Microsoft Word (automated via COM)
- `docxy.exe` built in the repo — `cargo build --release` (the launcher prefers
  `target/release/docxy.exe`, falling back to `target/debug/docxy.exe`)
- `corpus/classification.json` generated — `python corpus/tools/classify.py`

## Run

```powershell
cd compare
dotnet run -c Release
```

## Notes

- Word opens documents **read-only** so comparisons don't accidentally edit the
  corpus copies.
- The Word window is located by its `OpusApp` window class + a title match on the
  file name; if you already have many Word windows open it may grab the wrong
  one. Close stray Word windows for best results.
- docxy doesn't yet reload on external save, so this is an open-and-look tool.
