---
description: Build docxy and open it in a new agwinterm session on a corpus .docx
argument-hint: [filename]
---

Launch the **docxy** TUI (terminal Word clone) in a new agwinterm session for interactive testing.

Filename argument (may be empty): `$ARGUMENTS`

Follow these steps exactly; do not create a session unless the binary and the file both exist.

1. **Resolve the repo root.** Run `git rev-parse --show-toplevel`. The repo must be docxy (it contains the `docxy/` crate). If it isn't, stop and tell the user this command only works inside a docxy checkout.

2. **Resolve the document.** The default corpus directory is `<repo>/corpus/files` (third-party `.docx`, git-ignored, fetched from the `yeroo/docxy-corpus` repo).
   - Argument is an absolute path or contains a path separator → use it as-is.
   - Argument is a bare filename → resolve to `<repo>/corpus/files/<name>`.
   - No argument → list `corpus/files` (it may have subdirectories — search recursively for `.docx`) and pick a representative document; state which file you chose.
   - If the directory is missing or empty, the corpus hasn't been fetched: show the user the clone+copy snippet from `corpus/README.md` ("What lives in the separate corpus repos" — `git clone https://github.com/yeroo/docxy-corpus` then copy `files/` into `corpus/files`) and stop.
   - If the resolved file does not exist, report it (show a directory listing) and stop.

3. **Build.** Run `cargo build --release -p docxy` from the repo root.
   - Known machine quirk: the `%USERPROFILE%\.cargo\bin` shims are zero-byte rustup symlinks that some agent shells cannot spawn ("No application is associated with the specified file" or os error 448). If that happens, run instead:
     `"%USERPROFILE%\.cargo\bin\rustup.exe" run stable cargo build --release -p docxy`
     with the environment variable `RUSTC` set to `%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\rustc.exe`, and disable the shell sandbox if spawning still fails.
   - If the build fails, diagnose and report the actual compiler error; do not launch.

4. **Launch.** Find `agwintermctl` on PATH; if absent, use `%LOCALAPPDATA%\Programs\agwinterm\agwintermctl.exe`. Then run:
   `agwintermctl session new --name docxy --cwd "<repo root>" --command "\"<repo root>\target\release\docxy.exe\" \"<resolved file>\""`
   The `--command` string is split on whitespace with double-quote grouping (there is no shell), so wrap both the exe path and the resolved file path in embedded double quotes — this matters because corpus filenames commonly contain spaces (e.g. `1 Comment.docx`).
   It prints the new session id on success. If the control pipe is unreachable, agwinterm isn't running — tell the user to start agwinterm and stop.
   - Caveat: `agwintermctl` has no `--help` — probing `session new --help` **creates a session**. Don't probe; if you create one by accident, close it with `agwintermctl session close <id>`.

5. **Verify and report.** Run `agwintermctl session text --target <session id>` and confirm docxy rendered the document (page/paragraph text visible). If the text dump comes back empty or truncated, wait a few seconds and re-run it before concluding anything; if it stays empty but the session and process exist, report that the screen dump was unavailable rather than declaring the launch failed. Report the session id, the file opened, and a one-line description of what's on screen.
