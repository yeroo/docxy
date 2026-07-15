# offdev Claude Code plugin — design

**Date:** 2026-07-15
**Status:** approved (design review with user)
**Consumers:** developers of the docxy TUIs (`docxy`, `xlsxy`, `yppxy`) testing
them interactively in [agwinterm](https://github.com/yeroo/agwinterm)

## Problem

Testing a TUI from a Claude Code conversation means manually: building the
crate (with this machine's cargo-shim workaround), picking a corpus file,
finding `agwintermctl`, composing a `session new` invocation, and peeking at
the result. That's a repeatable recipe — it should be one slash command.

## Goal

A Claude Code plugin named **`offdev`** (development tooling; the `offixy`
name is reserved for a future general-purpose plugin) providing three
commands:

- `/offdev-docxy [filename]`
- `/offdev-xlsxy [filename]`
- `/offdev-yppxy [filename]`

Each builds the corresponding TUI and opens it in a **new agwinterm session**
on a corpus file, then confirms the screen rendered.

## Non-goals

- No general-purpose agwinterm control commands (that's the future plugin).
- No headless/CI use — agwinterm must be running interactively.
- No publishing to a public marketplace; this is a repo-local dev tool.

## Layout (inside the docxy repo)

```
tools/claude-plugin/
  .claude-plugin/marketplace.json     # local marketplace "docxy-tools", lists ./offdev
  offdev/
    .claude-plugin/plugin.json        # name "offdev", description, version
    commands/
      offdev-docxy.md
      offdev-xlsxy.md
      offdev-yppxy.md
```

Installed once per machine:

```
claude plugin marketplace add <docxy checkout>\tools\claude-plugin
claude plugin install offdev@docxy-tools
```

Commands run in whatever session invokes them, so all paths are resolved from
the current repo (`git rev-parse --show-toplevel`) — nothing machine-specific
is hard-coded.

## Command behavior

Each command is a **prompt-driven** markdown file (frontmatter: `description`,
`argument-hint: [filename]`) instructing Claude to:

1. **Resolve the file.** `$ARGUMENTS` is an optional filename. A bare name
   resolves against the tool's default corpus directory; an absolute path or
   a path containing separators is used as-is.

   | Command | Crate/exe | Default corpus dir |
   |---|---|---|
   | `/offdev-docxy` | `docxy` | `corpus/files` |
   | `/offdev-xlsxy` | `xlsxy` | `corpus/xlsx` |
   | `/offdev-yppxy` | `yppxy` | `corpus/mpp/snapshots` |

   **No argument** → list the corpus dir and pick a representative file,
   stating which one was chosen. **Empty/unfetched dir** → say so and offer
   the matching fetch script (`corpus/tools/fetch-*.{sh,ps1}`) rather than
   failing silently.

2. **Build.** `cargo build --release -p <crate>` (always — incremental builds
   are fast and guarantee the current code is what's tested). If the cargo
   shim won't spawn (rustup 1.29 zero-byte symlink shims fail from agent
   shells), fall back to
   `<cargo home>\rustup.exe run stable cargo build --release -p <crate>`
   with `RUSTC` pointed at the real toolchain `rustc.exe` — the workaround is
   spelled out in the command text so fresh sessions don't depend on memory.

3. **Launch.** `agwintermctl session new --name <tool> --cwd <repo root>
   --command "<repo>\target\release\<tool>.exe <file>"`. `agwintermctl` is
   looked up on PATH, falling back to
   `%LOCALAPPDATA%\Programs\agwinterm\agwintermctl.exe`. If the control pipe
   is unreachable, tell the user to start agwinterm.

4. **Verify.** `agwintermctl session text --target <session id>` to peek at
   the new session's screen; report the session id/name and a one-line
   description of what's showing.

## Error handling

Prompt-level by design: build failures are diagnosed (not dumped raw), a
missing corpus offers the fetch path, and **no session is created unless the
binary and the file both exist**. Known `agwintermctl` sharp edge, documented
in each command: it has no `--help` — probing `session new --help` *creates a
session*.

## Testing

Run each command once end-to-end: `/offdev-xlsxy` on a tracked oracle
workbook, `/offdev-yppxy` on a generated snapshot, `/offdev-docxy` with an
explicit file (or after fetching `corpus/files`). Success = a live agwinterm
session appears with the expected content on screen.

## Decisions log

- Plugin lives **inside the docxy repo** with a local marketplace (user
  choice) — versioned with the repo it serves.
- No-arg behavior: **list corpus dir, pick a sensible file** (user choice).
- Build: **always build first**, release profile (user choice).
- Corpus defaults as tabled above (user choice).
- Mechanics: **prompt-driven commands**, no helper script (user choice).
- Prefix **`offdev-`**, plugin name **`offdev`** (user decision 2026-07-15):
  this is the *development* plugin; `offixy` is reserved for a planned
  general-purpose plugin.
