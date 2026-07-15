---
description: Build yppxy and open it in a new agwinterm session on a corpus .mpp or MSPDI .xml
argument-hint: [filename]
---

Launch the **yppxy** TUI (terminal MS Project clone) in a new agwinterm session for interactive testing.

Filename argument (may be empty): `$ARGUMENTS`

Follow these steps exactly; do not create a session unless the binary and the file both exist.

1. **Resolve the repo root.** Run `git rev-parse --show-toplevel`. The repo must be docxy (it contains the `yppxy/` crate). If it isn't, stop and tell the user this command only works inside a docxy checkout.

2. **Resolve the project file.** The default corpus directory is `<repo>/corpus/mpp/snapshots` (the generated corpus: `NN-slug.mpp`, `NN-slug-mpp12.mpp`, and MSPDI `NN-slug.xml` — yppxy opens both `.mpp` and `.xml`).
   - Argument is an absolute path or contains a path separator → use it as-is.
   - Argument is a bare filename → resolve to `<repo>/corpus/mpp/snapshots/<name>`.
   - No argument → list the directory and pick a representative late-numbered snapshot (feature-rich; e.g. `46-progress.mpp`); state which file you chose.
   - If the directory is missing or empty, the corpus hasn't been fetched: point the user at `corpus/tools/fetch-mpp-corpus.ps1` (or `.sh`) — it needs authenticated GitHub access to the private `yeroo/mpp-corpus` repo (`gh auth login` + `gh auth setup-git`) — and stop.
   - If the resolved file does not exist, report it (show a directory listing) and stop.

3. **Build.** Run `cargo build --release -p yppxy` from the repo root.
   - Known machine quirk: the `%USERPROFILE%\.cargo\bin` shims are zero-byte rustup symlinks that some agent shells cannot spawn ("No application is associated with the specified file" or os error 448). If that happens, run instead:
     `"%USERPROFILE%\.cargo\bin\rustup.exe" run stable cargo build --release -p yppxy`
     with the environment variable `RUSTC` set to `%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\rustc.exe`, and disable the shell sandbox if spawning still fails.
   - If the build fails, diagnose and report the actual compiler error; do not launch.

4. **Launch.** Find `agwintermctl` on PATH; if absent, use `%LOCALAPPDATA%\Programs\agwinterm\agwintermctl.exe`. Then run:
   `agwintermctl session new --name yppxy --cwd "<repo root>" --command "\"<repo root>\target\release\yppxy.exe\" \"<resolved file>\""`
   The `--command` string is split on whitespace with double-quote grouping (there is no shell), so wrap both the exe path and the resolved file path in embedded double quotes — this matters because paths may contain spaces (user-supplied paths especially).
   It prints the new session id on success. If the control pipe is unreachable, agwinterm isn't running — tell the user to start agwinterm and stop.
   - Caveat: `agwintermctl` has no `--help` — probing `session new --help` **creates a session**. Don't probe; if you create one by accident, close it with `agwintermctl session close <id>`.

5. **Verify and report.** Run `agwintermctl session text --target <session id>` and confirm the task table / schedule view rendered with task rows. Garbled text or `????` task names on a `.mpp` snapshot indicate a known mppread decoder gap (exactly what this corpus exists to expose) — report it as a decode-gap observation, not a launch failure. If the text dump comes back empty or truncated, wait a few seconds and re-run it before concluding anything; if it stays empty but the session and process exist, report that the screen dump was unavailable rather than declaring the launch failed. Report the session id, the file opened, and a one-line description of what's on screen.
