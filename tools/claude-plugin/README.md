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
