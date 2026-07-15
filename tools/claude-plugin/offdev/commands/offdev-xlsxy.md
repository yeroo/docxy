---
description: Build xlsxy and open it in a new agwinterm session on a corpus workbook
argument-hint: [filename]
---

Launch the **xlsxy** TUI (terminal Excel clone) in a new agwinterm session for interactive testing.

Filename argument (may be empty): `$ARGUMENTS`

Follow these steps exactly; do not create a session unless the binary and the file both exist.

1. **Resolve the repo root.** Run `git rev-parse --show-toplevel`. The repo must be docxy (it contains the `xlsxy/` crate). If it isn't, stop and tell the user this command only works inside a docxy checkout.

2. **Resolve the workbook.** The default corpus directory is `<repo>/corpus/xlsx`.
   - Argument is an absolute path or contains a path separator → use it as-is.
   - Argument is a bare filename → resolve to `<repo>/corpus/xlsx/<name>`.
   - No argument → list `corpus/xlsx` and pick a representative workbook (prefer a formula-rich one such as `calc-refs.xlsx`); state which file you chose.
   - If the resolved file does not exist, report it (show the directory listing) and stop.
   - `corpus/xlsx` is tracked in git, so it is always present; if it is somehow empty, say so and stop.

3. **Build.** Run `cargo build --release -p xlsxy` from the repo root.
   - Known machine quirk: the `%USERPROFILE%\.cargo\bin` shims are zero-byte rustup symlinks that some agent shells cannot spawn ("No application is associated with the specified file" or os error 448). If that happens, run instead:
     `"%USERPROFILE%\.cargo\bin\rustup.exe" run stable cargo build --release -p xlsxy`
     with the environment variable `RUSTC` set to `%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\rustc.exe`, and disable the shell sandbox if spawning still fails.
   - If the build fails, diagnose and report the actual compiler error; do not launch.

4. **Launch.** Find `agwintermctl` on PATH; if absent, use `%LOCALAPPDATA%\Programs\agwinterm\agwintermctl.exe`. Then run:
   `agwintermctl session new --name xlsxy --cwd "<repo root>" --command "\"<repo root>\target\release\xlsxy.exe\" \"<resolved file>\""`
   The `--command` string is split on whitespace with double-quote grouping (there is no shell), so wrap both the exe path and the resolved file path in embedded double quotes — this matters because corpus filenames commonly contain spaces (e.g. `1 Comment.docx`).
   It prints the new session id on success. If the control pipe is unreachable, agwinterm isn't running — tell the user to start agwinterm and stop.
   - Caveat: `agwintermctl` has no `--help` — probing `session new --help` **creates a session**. Don't probe; if you create one by accident, close it with `agwintermctl session close <id>`.

5. **Verify and report.** Run `agwintermctl session text --target <session id>` and confirm the xlsxy grid rendered (ribbon row, formula bar, cell grid). If the text dump comes back empty or truncated, wait a few seconds and re-run it before concluding anything; if it stays empty but the session and process exist, report that the screen dump was unavailable rather than declaring the launch failed. Report the session id, the file opened, and a one-line description of what's on screen.
