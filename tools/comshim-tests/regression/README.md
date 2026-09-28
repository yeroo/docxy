# COM shim regression kit

Compares how the COM shims (`wordcomshim`, `xlcomshim`) behave for real COM clients
on two builds — typically `main` and a change — case by case. Use it for anything CI
cannot see: CI does not register COM servers or run clients, so a change can pass CI
and still break every automation client.

It is what caught the `windows` 0.62 upgrade (#532) making every shim object agile:
out-of-process calls moved off the server's STA thread, `SaveAs` failed with E_FAIL
and the server never exited after `Quit`, while CI was green.

**Run it for:** any `windows` / `windows-core` / `windows-implement` bump, any change to
`comshimcore`'s class factory, server loop or `#[implement]` objects, and any change to
how the shims are built or registered.

## What it runs

For each shim × activation path (out-of-proc `LocalServer32` exe, in-proc
`InprocServer32` dll) × client (the existing smoke clients in `tools/wordshim-tests` and
`tools/comshim-tests`: VBScript, pywin32, PowerShell 5.1 and 7, C#, R), it registers only
that path, runs the client headless and records the exit code, output, shim log,
produced files (hashes of each OOXML part, not the zip), and for out-of-proc how long
the server takes to exit after the last client. It also runs a two-client test (a
second client attaching while the first holds the object) and a pywin32
`GetTypeInfo` / makepy probe. Clients whose runtime is missing are reported as
unavailable, not installed.

## How

Needs PowerShell 7 (`pwsh`), on Windows.

```powershell
# 1. baseline on main
git checkout main
cargo build --release -p wordcomshim -p xlcomshim
pwsh -NoProfile -File tools\comshim-tests\regression\stage-bins.ps1 -Dest tools\comshim-tests\regression\bin-main
pwsh -NoProfile -File tools\comshim-tests\regression\run.ps1 -BinDir tools\comshim-tests\regression\bin-main -IncludeOop

# 2. the change
git checkout <branch>
cargo build --release -p wordcomshim -p xlcomshim
pwsh -NoProfile -File tools\comshim-tests\regression\stage-bins.ps1 -Dest tools\comshim-tests\regression\bin-change
pwsh -NoProfile -File tools\comshim-tests\regression\run.ps1 -BinDir tools\comshim-tests\regression\bin-change -IncludeOop

# 3. compare
pwsh -NoProfile -File tools\comshim-tests\regression\compare.ps1 `
  tools\comshim-tests\regression\run-bin-main tools\comshim-tests\regression\run-bin-change
```

`-Only <regex>` runs a subset (e.g. `-Only 'word-oop'`). Each run writes
`results.md` / `results.json` into its output folder. The shim-process *count* in a
comparison varies between runs of the same build; the case outcomes, produced files
and server exit time are what must match.

## Safety

- **Registry.** The run snapshots every HKCU key the register scripts can touch
  (ProgIDs, the shim CLSIDs, typelibs, interfaces) before it starts and restores
  exactly that state in a `finally`, then writes `reg-diff.txt`, which must be empty.
  A machine that already has the shims installed (e.g. from `dist\office-shims`) keeps
  its installation.
- **Office.** It refuses to start if `WINWORD.EXE` or `EXCEL.EXE` is running and aborts
  if either appears. It never opens real Word or Excel.
- **Screen.** A window guard aborts the run if any window appears. The shims are
  GUI-subsystem executables (#530), so an out-of-proc COM launch opens none.
- **Processes.** It kills only clients it started and shim exes running from `-BinDir`.
- It backs up and restores your real `%TEMP%` shim logs.

## Files

- `run.ps1` — the matrix run. `compare.ps1` — case-by-case comparison of two runs.
- `stage-bins.ps1` — copies a release build out of `target\` with a `BUILDINFO.txt`.
- `lib\` — helpers: `holder.py` (the first client in the two-client test),
  `typeinfo_probe.py` (pywin32 GetTypeInfo / makepy), `winprobe.ps1` (the window
  guard), `ensure_debug.ps1`, and `threadwatch.ps1` (a diagnostic: the server's threads
  and their wait states, for a server that will not exit).
