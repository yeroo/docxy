#!/usr/bin/env bash
# Bridge: make revmux serve as ralphex's external review tool.
#
# Adapted from the proven winterm-browser hook (tools/ralphex-revmux.sh). Keep
# the two in step — differences here should be docxy-specific, not accidental.
#
# ralphex calls this with exec.Command(script, promptFile) — no shell, one
# argument, stdout and stderr merged and streamed line by line. It watches the
# stream for <<<RALPHEX:CODEX_REVIEW_DONE>>>, which we MUST emit exactly once
# when finished; without it ralphex waits out its idle timeout on a review that
# already completed.
#
# The prompt file ralphex hands us is its rendered custom_review.txt: it already
# carries the goal, the git diff command for THIS iteration, the plan path and
# the progress log. That is very nearly a revmux scope, so it is passed through
# verbatim rather than reconstructed — paraphrase is where review context
# silently goes missing between rounds.
#
# Wire it up in .ralphex/config:
#     external_review_tool = custom
#     custom_review_script = scripts\revmux-review.cmd
#
# Invoked on Windows through the .cmd sibling, because exec.Command cannot run a
# .sh directly there.

# NOT `set -e`: revmux exits non-zero when it HAS findings, the way a linter
# does, and every guard below is meant to warn and continue rather than take the
# whole review phase down.
set -uo pipefail

PROMPT_FILE="${1:-}"
DONE_SIGNAL='<<<RALPHEX:CODEX_REVIEW_DONE>>>'

# Emitted on every exit path, including the failures below.
finish() { printf '%s\n' "$DONE_SIGNAL"; }
trap finish EXIT

if [ -z "$PROMPT_FILE" ] || [ ! -f "$PROMPT_FILE" ]; then
  echo "revmux-review: no prompt file passed (got '${PROMPT_FILE}')" >&2
  exit 0
fi

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || REPO_ROOT="$PWD"
cd "$REPO_ROOT" || exit 0

command -v revmux >/dev/null 2>&1 || { echo "revmux-review: revmux not on PATH" >&2; exit 0; }

# comprehensive is the diff-shaped roster: bugs+impl, arch+quality and
# docs+tests on claude, plus an adversarial codex peer.
PROFILE="${RALPHEX_REVMUX_PROFILE:-comprehensive}"
MIN_CONFIDENCE="${RALPHEX_REVMUX_MIN_CONFIDENCE:-60}"
# revmux's default hard timeout is 20m per agent attempt. The one panel this
# repo has run took ~15m with agents at 3.7M tokens on a single-file diff, so
# the default is close enough to bite on a wider one — an agent killed mid-read
# reports nothing, and a review that silently covered less is worse than one
# that took longer.
HARD_TIMEOUT="${RALPHEX_REVMUX_HARD_TIMEOUT:-40m}"

# One revmux task per PLAN, one run per review iteration. Keeping the task
# stable across iterations is the point: revmux carries earlier rounds into
# every later prompt, so iteration 2 knows what iteration 1 already reported.
# Keying on the branch instead would merge unrelated plans into one history.
PLAN_NAME="$(grep -m1 -oE '[^ /\\]+\.md' "$PROMPT_FILE" 2>/dev/null | head -1 | sed 's/\.md$//')"
[ -z "$PLAN_NAME" ] && PLAN_NAME="review"
TASK="ralphex-${PLAN_NAME}"
RUN="$(date +%Y%m%d-%H%M%S)"

PATHS_JSON="$(revmux new --task "$TASK" --run "$RUN" 2>/dev/null)" || {
  echo "revmux-review: revmux new failed" >&2; exit 0; }

# Take the input paths out of revmux's own payload rather than joining them by
# hand.
pluck() {
  printf '%s' "$PATHS_JSON" \
    | sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\(.*\)\".*/\1/p" \
    | head -1 | sed 's/\\\\/\//g'
}
SCOPE="$(pluck scope)"
# revmux allocates a profile file beside the scope; left unwritten, every
# reviewer runs with the project's conventions empty.
PROFILE_MD="$(pluck profile)"
[ -z "$SCOPE" ] && { echo "revmux-review: could not read scope path from revmux new" >&2; exit 0; }

{
  echo "# Review scope (handed over by ralphex)"
  echo
  echo "This round was opened automatically by ralphex's external review phase for"
  echo "the task it just implemented. Everything below the rule is ralphex's own"
  echo "review prompt, verbatim — it carries the goal, the exact diff command for"
  echo "this iteration, and the paths to the plan and the progress log."
  echo
  echo "Review the diff it names. The plan file states what the task was supposed"
  echo "to do; a change that works but does not match the plan is a finding worth"
  echo "reporting, and one that argues against a decision the plan records as"
  echo "deliberate is not."
  echo
  echo "## This repository holds TWO cargo workspaces"
  echo
  echo "A green build in one says nothing about the other. Growing a shared"
  echo "gridcore struct has already compiled clean under \`suite/\` while breaking"
  echo "\`xlsxy\`, \`gridwasm\` and the TUI \`docxy\`, which build literals of it."
  echo
  echo '```bash'
  echo "cargo build --manifest-path suite/Cargo.toml   # the GPUI desktop suite"
  echo "cargo test  --manifest-path suite/Cargo.toml"
  echo "cargo build --all-targets                      # gridcore, xlsxy, gridwasm, lookxy, TUI"
  echo "cargo test  -p gridcore"
  echo "cargo clippy -p gridcore --all-targets -- -D warnings"
  echo '```'
  echo
  echo "Invalid XML or refs Excel rejects are severe: the symptom is Excel"
  echo "reporting the workbook as needing repair and dropping content. Style,"
  echo "naming and comment density are not findings — the codebase has settled"
  echo "conventions and matching them beats improving them."
  echo
  echo '---'
  echo
  cat "$PROMPT_FILE"
} > "$SCOPE" 2>/dev/null || { echo "revmux-review: could not write scope" >&2; exit 0; }

# The conventions every reviewer is held to, in revmux's own profile slot. Kept
# apart from the scope because it is about the REPOSITORY rather than this diff:
# what counts as a finding here, and what the test harness can and cannot do.
# Left empty, the panel reports "add a test for the panel renderer" and "no e2e
# coverage" every round against a codebase that deliberately has neither.
if [ -n "$PROFILE_MD" ]; then
  cat > "$PROFILE_MD" <<'CONVENTIONS' || \
    echo "revmux-review: could not write profile (continuing)" >&2
# Project conventions

## Testing

- Pure free functions in `suite/docxy/src/main.rs` are tested in the
  `#[cfg(test)]` module at the bottom of that file. gpui `#[test]` works for
  pure logic; constructing views or elements blows up the render macro, so
  helpers are deliberately written as pure free functions to stay testable.
  "Add a test for the renderer or the view" is not a finding here — it cannot
  be done.
- Model behaviour is tested in `gridcore/src/*.rs` next to the code, and the
  load -> save -> load round-trips are the tests that matter for charts.
- There is no browser or UI e2e harness. On-screen behaviour is verified
  manually against an installer build, not in CI, so "no e2e coverage" is not
  a finding either.

## What is worth reporting

- Real defects: wrong behaviour, dropped data, panics, silent fallbacks that
  hide a user's mistake.
- OOXML correctness. Invalid XML or refs Excel rejects are severe, because the
  symptom is Excel reporting the workbook as needing repair and dropping
  content.
- A change that works but does not match the plan it was built from, and a
  plan or doc left describing a world the code no longer has.
- Missing tests for a code path the change introduced, where that path is
  testable per the section above.

## What is not

- Style preferences, naming, comment density. The codebase has settled
  conventions and matching them beats improving them.
- Anything the plan file explicitly lists as out of scope or deferred, or
  argues against as a recorded decision.
CONVENTIONS
fi

echo "revmux-review: running revmux (profile=$PROFILE, task=$TASK, run=$RUN)" >&2

# RALPHEX_REVMUX_DRY_RUN=1 proves the wiring without paying for a panel: the
# argument arrived, the round was created, the scope was written.
if [ "${RALPHEX_REVMUX_DRY_RUN:-0}" = "1" ]; then
  echo "revmux-review: DRY RUN — round created, scope written, revmux not invoked" >&2
  echo "scope: $SCOPE" >&2
  echo "profile: ${PROFILE_MD:-<none allocated>}" >&2
  echo "NO ISSUES FOUND"
  exit 0
fi

# revmux exits non-zero when it HAS findings — a normal review outcome, not a
# failure — so the status is deliberately not propagated. ralphex reads the
# findings off stdout, so revmux's output is merged there rather than split.
revmux --task "$TASK" --run "$RUN" \
       --profile "$PROFILE" \
       --min-confidence "$MIN_CONFIDENCE" \
       --hard-timeout "$HARD_TIMEOUT" \
       --markdown --no-tui \
       --workdir "$REPO_ROOT" 2>&1 || true

exit 0
