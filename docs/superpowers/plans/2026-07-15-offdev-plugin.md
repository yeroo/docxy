# offdev Claude Code Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A repo-local Claude Code plugin `offdev` with three slash commands (`/offdev-docxy`, `/offdev-xlsxy`, `/offdev-yppxy`) that build the corresponding TUI and open it in a new agwinterm session on a corpus file.

**Architecture:** A local plugin marketplace at `tools/claude-plugin/` (marketplace `docxy-tools`) containing one plugin `offdev`; each command is a self-contained prompt-driven markdown file — no scripts, no code. Spec: `docs/superpowers/specs/2026-07-15-offdev-plugin-design.md`.

**Tech Stack:** Claude Code plugin manifests (JSON), markdown command prompts, `agwintermctl`, cargo.

## Global Constraints

- Plugin name **`offdev`**; marketplace name **`docxy-tools`**; command names exactly `offdev-docxy`, `offdev-xlsxy`, `offdev-yppxy` (the `offixy` name is reserved for a future plugin — do not use it anywhere).
- Commands are **prompt-driven markdown only** — no helper scripts in the plugin.
- No machine-specific absolute paths in committed files, with two allowed env-relative fallbacks spelled out in the prompts: `%LOCALAPPDATA%\Programs\agwinterm\agwintermctl.exe` and the rustup shim workaround via `%USERPROFILE%\.cargo\bin\rustup.exe`.
- Repo root is always resolved at runtime via `git rev-parse --show-toplevel`; corpus defaults: docxy → `corpus/files`, xlsxy → `corpus/xlsx`, yppxy → `corpus/mpp/snapshots`.
- A command must **never create an agwinterm session unless the binary and the file both exist**.
- Testing a command file = literally following its steps (they are instructions for Claude); after each end-to-end test, close the created session with `agwintermctl session close <id>`.

---

### Task 1: Plugin scaffold + `/offdev-xlsxy`

**Files:**
- Create: `tools/claude-plugin/.claude-plugin/marketplace.json`
- Create: `tools/claude-plugin/offdev/.claude-plugin/plugin.json`
- Create: `tools/claude-plugin/offdev/commands/offdev-xlsxy.md`

**Interfaces:**
- Produces: the marketplace `docxy-tools` and plugin `offdev` that Tasks 2–3 add command files into; the command-file template (structure, frontmatter, step layout) that Tasks 2–3 mirror.

- [ ] **Step 1: Create the marketplace manifest**

Write `tools/claude-plugin/.claude-plugin/marketplace.json`:

```json
{
  "name": "docxy-tools",
  "owner": {
    "name": "Boris Kudriashov"
  },
  "plugins": [
    {
      "name": "offdev",
      "source": "./offdev",
      "description": "docxy development commands: launch docxy/xlsxy/yppxy in agwinterm test sessions"
    }
  ]
}
```

- [ ] **Step 2: Create the plugin manifest**

Write `tools/claude-plugin/offdev/.claude-plugin/plugin.json`:

```json
{
  "name": "offdev",
  "version": "0.1.0",
  "description": "Development commands for the docxy repo: build a TUI (docxy/xlsxy/yppxy) and open it in a new agwinterm session on a corpus file",
  "author": {
    "name": "Boris Kudriashov"
  }
}
```

- [ ] **Step 3: Create the xlsxy command**

Write `tools/claude-plugin/offdev/commands/offdev-xlsxy.md`:

````markdown
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
   `agwintermctl session new --name xlsxy --cwd "<repo root>" --command "<repo root>\target\release\xlsxy.exe <resolved file>"`
   It prints the new session id on success. If the control pipe is unreachable, agwinterm isn't running — tell the user to start agwinterm and stop.
   - Caveat: `agwintermctl` has no `--help` — probing `session new --help` **creates a session**. Don't probe; if you create one by accident, close it with `agwintermctl session close <id>`.

5. **Verify and report.** Run `agwintermctl session text --target <session id>` and confirm the xlsxy grid rendered (ribbon row, formula bar, cell grid). Report the session id, the file opened, and a one-line description of what's on screen.
````

- [ ] **Step 4: Register the marketplace and install the plugin**

Run:
```bash
claude plugin marketplace add "$(git rev-parse --show-toplevel)/tools/claude-plugin"
claude plugin install offdev@docxy-tools
```
Expected: both commands succeed; `claude plugin list` (or the `/plugin` UI) shows `offdev` with the `offdev-xlsxy` command. If `claude` isn't on PATH in the test shell, tell the user to run the two commands themselves and how.

- [ ] **Step 5: End-to-end test `/offdev-xlsxy` with no argument**

Follow the steps in `tools/claude-plugin/offdev/commands/offdev-xlsxy.md` literally, as if invoked with an empty argument.
Expected: build passes, a session named `xlsxy` appears, `session text` shows the grid with a corpus workbook loaded.
Then close it: `agwintermctl session close <id>` → prints `closed`.

- [ ] **Step 6: End-to-end test with a bare filename argument**

Follow the command steps with argument `calc-dates.xlsx`.
Expected: resolves to `corpus/xlsx/calc-dates.xlsx`, session opens on it. Close the session afterwards. Also test a nonexistent name (e.g. `nope.xlsx`): expected — reports the missing file and does **not** create a session.

- [ ] **Step 7: Commit**

```bash
git add tools/claude-plugin
git commit -m "feat: offdev Claude Code plugin — /offdev-xlsxy agwinterm test session"
```

---

### Task 2: `/offdev-yppxy`

**Files:**
- Create: `tools/claude-plugin/offdev/commands/offdev-yppxy.md`

**Interfaces:**
- Consumes: the plugin scaffold from Task 1 (`tools/claude-plugin/offdev/`); mirrors the Task 1 command structure.
- Produces: nothing later tasks rely on.

- [ ] **Step 1: Create the yppxy command**

Write `tools/claude-plugin/offdev/commands/offdev-yppxy.md`:

````markdown
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
   `agwintermctl session new --name yppxy --cwd "<repo root>" --command "<repo root>\target\release\yppxy.exe <resolved file>"`
   It prints the new session id on success. If the control pipe is unreachable, agwinterm isn't running — tell the user to start agwinterm and stop.
   - Caveat: `agwintermctl` has no `--help` — probing `session new --help` **creates a session**. Don't probe; if you create one by accident, close it with `agwintermctl session close <id>`.

5. **Verify and report.** Run `agwintermctl session text --target <session id>` and confirm yppxy rendered (task table / schedule view; for a `.mpp` snapshot expect at minimum the WBS task names). Report the session id, the file opened, and a one-line description of what's on screen.
````

- [ ] **Step 2: Reload and end-to-end test with no argument**

Reload plugins (`/reload-plugins` in the Claude Code session, or reinstall via `claude plugin`), then follow the command's steps literally with an empty argument.
Expected: build passes, a session named `yppxy` appears showing the chosen snapshot's task list. Close it with `agwintermctl session close <id>`.

- [ ] **Step 3: End-to-end test with a bare filename argument**

Follow the command steps with argument `28-link-lag.xml`.
Expected: resolves to `corpus/mpp/snapshots/28-link-lag.xml`, session opens showing the MSPDI-loaded schedule. Close the session afterwards.

- [ ] **Step 4: Commit**

```bash
git add tools/claude-plugin/offdev/commands/offdev-yppxy.md
git commit -m "feat: /offdev-yppxy agwinterm test session command"
```

---

### Task 3: `/offdev-docxy`

**Files:**
- Create: `tools/claude-plugin/offdev/commands/offdev-docxy.md`

**Interfaces:**
- Consumes: the plugin scaffold from Task 1 (`tools/claude-plugin/offdev/`); mirrors the Task 1 command structure.
- Produces: nothing later tasks rely on.

- [ ] **Step 1: Create the docxy command**

Write `tools/claude-plugin/offdev/commands/offdev-docxy.md`:

````markdown
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
   `agwintermctl session new --name docxy --cwd "<repo root>" --command "<repo root>\target\release\docxy.exe <resolved file>"`
   It prints the new session id on success. If the control pipe is unreachable, agwinterm isn't running — tell the user to start agwinterm and stop.
   - Caveat: `agwintermctl` has no `--help` — probing `session new --help` **creates a session**. Don't probe; if you create one by accident, close it with `agwintermctl session close <id>`.

5. **Verify and report.** Run `agwintermctl session text --target <session id>` and confirm docxy rendered the document (page/paragraph text visible). Report the session id, the file opened, and a one-line description of what's on screen.
````

- [ ] **Step 2: Reload and end-to-end test the unfetched-corpus path**

`corpus/files` is currently absent in this checkout. Follow the command's steps literally with an empty argument.
Expected: the command stops at step 2 and surfaces the `corpus/README.md` clone+copy instructions — **no build, no session**.

- [ ] **Step 3: End-to-end test with a real document**

Fetch the corpus (run the `corpus/README.md` snippet: clone `yeroo/docxy-corpus` to a temp dir, copy `files/` to `corpus/files`), then follow the command steps with an empty argument.
Expected: build passes, a session named `docxy` appears rendering the chosen document. Close it with `agwintermctl session close <id>`.

- [ ] **Step 4: Commit**

```bash
git add tools/claude-plugin/offdev/commands/offdev-docxy.md
git commit -m "feat: /offdev-docxy agwinterm test session command"
```

---

### Task 4: Document the plugin

**Files:**
- Create: `tools/claude-plugin/README.md`

**Interfaces:**
- Consumes: everything from Tasks 1–3 (names, install commands, command list).
- Produces: nothing later tasks rely on.

- [ ] **Step 1: Write the README**

Write `tools/claude-plugin/README.md`:

```markdown
# docxy-tools — local Claude Code plugin marketplace

A repo-local plugin marketplace for docxy development helpers.

## Plugins

### offdev

Development commands that build a docxy TUI and open it in a new
[agwinterm](https://github.com/yeroo/agwinterm) session on a corpus file
(agwinterm must be running; `agwintermctl` is found on PATH or in the
default install location):

| Command | Opens | Default corpus dir |
|---|---|---|
| `/offdev-docxy [filename]` | docxy (Word TUI) | `corpus/files` |
| `/offdev-xlsxy [filename]` | xlsxy (Excel TUI) | `corpus/xlsx` |
| `/offdev-yppxy [filename]` | yppxy (Project TUI) | `corpus/mpp/snapshots` |

A bare filename resolves inside the default corpus dir; a path is used
as-is; with no argument the command picks a representative corpus file.
Each command builds the crate (`cargo build --release -p <crate>`) before
launching, and never creates a session unless the binary and file exist.

## Install (once per machine)

    claude plugin marketplace add <docxy checkout>/tools/claude-plugin
    claude plugin install offdev@docxy-tools

The name `offixy` is reserved for a future general-purpose plugin.
```

- [ ] **Step 2: Commit**

```bash
git add tools/claude-plugin/README.md
git commit -m "docs: README for the docxy-tools plugin marketplace"
```

---

## Self-Review Notes

- Spec coverage: layout (Task 1), all three commands with corpus defaults and no-arg/missing-corpus behavior (Tasks 1–3), install flow (Task 1 step 4), error handling embedded in each command file, testing = the end-to-end steps. The spec's "fetch script" wording for docxy is refined here: only the mpp corpus has fetch scripts; the docx corpus uses the documented clone+copy snippet from `corpus/README.md`.
- No placeholders; complete file contents inline in every create step.
- Names consistent: `offdev`, `docxy-tools`, `offdev-<tool>.md`, session names `docxy`/`xlsxy`/`yppxy`.
