#!/usr/bin/env bash
# revmux-review.sh — ralphex external-review hook that runs revmux.
#
# ralphex calls this via pkg/executor/custom.go as `exec.Command(script, promptFile)`
# — a direct exec with NO shell — so on Windows it must be reached through
# revmux-review.cmd, which bridges into Git bash. See that file.
#
# Wiring (.ralphex/config):
#   external_review_tool = custom
#   custom_review_script = scripts/revmux-review.cmd
#
# Contract: $1 is the rendered prompt file (ralphex has already substituted
# {{GOAL}}, {{DIFF_INSTRUCTION}}, {{PLAN_FILE}} and friends into it). Whatever we
# print on stdout becomes the findings ralphex feeds to its evaluation phase.

set -euo pipefail

command -v revmux >/dev/null 2>&1 || { echo "error: revmux not found on PATH" >&2; exit 1; }

prompt_file="${1:-}"
if [[ -z "$prompt_file" || ! -f "$prompt_file" ]]; then
    echo "error: prompt file not provided or not found: ${prompt_file:-<none>}" >&2
    exit 1
fi

repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$repo_root"

# One revmux task per branch, one round per invocation. ralphex's external
# review is a LOOP — it calls this again after each fix round — so the run name
# has to be unique or `revmux new` would collide with the previous round.
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo detached)
task="ralphex-${branch//\//-}"
run="$(date +%Y%m%d-%H%M%S)"

paths=$(revmux new --task "$task" --run "$run")
pluck() { printf '%s' "$paths" | sed -n "s/.*\"$1\": \"\\(.*\\)\".*/\\1/p" | sed 's/\\\\/\//g'; }
goal_md=$(pluck goal)
scope_md=$(pluck scope)
profile_md=$(pluck profile)
round_dir=$(pluck round_dir)

# The ralphex prompt already states the goal and the review focus, and carries
# the diff command for this iteration plus any previous-round context. Hand it
# over whole rather than paraphrasing it — paraphrase is where review context
# silently goes missing between rounds.
cp "$prompt_file" "$goal_md"

# The default branch ralphex diffs against, as it wrote it into the prompt.
# ralphex renders the diff command for THIS iteration into the prompt — it is
# narrower than "branch vs main" once the run is under way, and reviewing the
# whole branch would drag in every commit that predates the plan. Take its
# command verbatim rather than reconstructing one.
diff_cmd=$(grep -m1 -oE '^git diff .*$' "$prompt_file" || true)
diff_cmd="${diff_cmd:-git diff origin/main...HEAD}"

{
    echo "# Item under review: the working tree of $(basename "$repo_root"), branch \`$branch\`"
    echo
    echo "Review exactly the changes this command prints, and nothing else — the"
    echo "branch also carries earlier work that is not under review here."
    echo "Read the full diff before judging any hunk:"
    echo
    echo '```'
    echo "$diff_cmd"
    echo '```'
    echo
    echo "## Files changed"
    echo
    echo '```'
    eval "${diff_cmd/git diff/git diff --stat}" 2>/dev/null || echo "(diff unavailable)"
    echo '```'
    echo
    echo "## Plan being executed"
    echo
    echo "The branch is being built task-by-task from a ralphex plan under"
    echo "\`docs/plans/\`. Read it: it records deliberate scope decisions, and a"
    echo "finding that argues against a documented decision is not a finding."
} > "$scope_md"

{
    cat <<'EOF'
# Project conventions

## Two cargo workspaces — this is the trap

This repository holds TWO separate cargo workspaces. A green build in one says
nothing about the other, and this has already broken CI once: growing a shared
struct compiled fine under `suite/` and broke `xlsxy`, `gridwasm` and the TUI
`docxy`, which build literals of that type.

```bash
# the suite (GPUI desktop app) — its own workspace, NOT part of the root one
cargo build --manifest-path suite/Cargo.toml
cargo test  --manifest-path suite/Cargo.toml

# the root workspace: gridcore, docxcore, xlsxy, gridwasm, lookxy, TUI docxy
cargo build --all-targets
cargo test  -p gridcore
cargo clippy -p gridcore --all-targets -- -D warnings
cargo fmt --check
```

Any change touching `gridcore` types must be checked against BOTH.

## Testing

- Pure free functions in `suite/docxy/src/main.rs` are tested in the
  `#[cfg(test)]` module at the bottom of that file. gpui `#[test]` works for
  pure logic; constructing views or elements blows up the render macro, so
  helpers are deliberately written as pure free functions to stay testable.
- Model behaviour is tested in `gridcore/src/*.rs` next to the code.
- There is no browser e2e harness. On-screen behaviour is verified manually
  against an installer build, not in CI.

## What is worth reporting

- Real defects: wrong behaviour, dropped data, panics, silent fallbacks that
  hide a user's mistake.
- OOXML correctness — invalid XML or refs Excel rejects are severe, because the
  symptom is Excel reporting the workbook as needing repair and dropping
  content.
- Missing tests for a code path the change introduced.

## What is not

- Style preferences, naming, comment density — the codebase has settled
  conventions and matching them beats improving them.
- Anything the plan file explicitly lists as out of scope or deferred.
EOF
} > "$profile_md"

# REVMUX_REVIEW_DRYRUN=1 scaffolds the round and stops, so the wiring can be
# checked without paying for a panel of agents.
if [[ -n "${REVMUX_REVIEW_DRYRUN:-}" ]]; then
    echo "dry run: round scaffolded at $round_dir" >&2
    for f in "$goal_md" "$scope_md" "$profile_md"; do
        echo "  $(wc -c <"$f" | tr -d ' ') bytes  $f" >&2
    done
    exit 0
fi

# --no-tui because ralphex captures stdout; --markdown so report.md is written
# in a form the evaluation phase can read directly.
revmux --task "$task" --run "$run" --no-tui --markdown --workdir "$repo_root" >&2

report="$round_dir/report.md"
if [[ -f "$report" ]]; then
    cat "$report"
else
    echo "error: revmux produced no report at $report" >&2
    exit 1
fi
