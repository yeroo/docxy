# Running ralphex with revmux as its reviewer

ralphex executes a plan task by task. revmux reviews a diff with a panel of
agents that then verify each other's findings. Wiring the second in as the
first's external review phase gives you an autonomous loop: implement, review,
fix, re-review, until it converges.

This document is what that cost to get working, so the next repo does not pay
it again. It is portable; the repo-specific parts are the profile names in the
hook and the round at which the roster narrows.

**Status**: proven. A 14-round loop on `docxy` converged 8 → … → 3 → 1 findings
over 4h14m, fixing two Major defects that three earlier reviews had missed. That
run is also what motivated the convergence caps in [Bounding the
cost](#bounding-the-cost): every Major it found arrived in the first two rounds,
and the other twelve were the loop reviewing its own documentation fixes.
Before this, the same wiring had failed at the review phase in two repositories
for four different reasons, all listed below.

## Setup

Three files. Copy them, change the profile text, done.

### 1. `.ralphex/config`

```
external_review_tool = custom
custom_review_script = scripts\revmux-review.cmd
max_external_iterations = 3
review_patience = 2
```

The first two are the wiring. The last two are the convergence caps — see
[Bounding the cost](#bounding-the-cost) for why they are pinned here rather
than left to the global default.

**The path must use backslashes.** ralphex runs it via
`exec.Command(script, promptFile)` with no shell; a `.cmd` reached that way
goes through `cmd.exe`, which reads a leading `/` as a switch prefix. A
forward-slash path fails with `'scripts' is not recognized as an internal or
external command` and takes the whole review phase down.

Keep this file minimal. Local config *shadows* the global one, so a key copied
in that you never meant to pin will silently override `~/.config/ralphex/`.

⚠️ **Check that these keys are actually uncommented.** `ralphex --init`
writes the full template with everything commented out, which looks like a
configured project but is not: the hook sits there and is never invoked, and
the external review phase quietly runs codex instead. That was the state of one
repo here for weeks. `grep -vE '^\s*#|^\s*$' .ralphex/config` should print
exactly the four lines above — the two wiring keys and the two caps.

### 2. `scripts/revmux-review.cmd`

The Windows bridge. `exec.Command` cannot run a `.sh` on Windows — it fails
with `%1 is not a valid Win32 application`.

```bat
@echo off
setlocal
set "BASH=%ProgramFiles%\Git\bin\bash.exe"
if not exist "%BASH%" set "BASH=%ProgramFiles%\Git\usr\bin\bash.exe"
if not exist "%BASH%" (
  echo revmux-review: git bash not found 1>&2
  echo ^<^<^<RALPHEX:CODEX_REVIEW_DONE^>^>^>
  exit /b 0
)
"%BASH%" "%~dp0revmux-review.sh" %1
exit /b 0
```

**`Git\bin\bash.exe`, not `Git\usr\bin\bash.exe`.** The first is the MSYS
wrapper and sets up `PATH`; the second is the raw binary and does not. Reach for
the wrong one and the script dies on `date: command not found` while `git` and
`revmux` resolve fine from the system `PATH` — a baffling failure until you
know.

### Line endings — three files that fail in different directions

Get this wrong and the setup breaks on a fresh clone rather than on your
machine, which makes it hard to spot. Put all three rules in `.gitattributes`:

```
*.sh              text eol=lf     # bash: a CR gives "$'\r': command not found"
.ralphex/config   text eol=lf     # a CR joins the value: a path ending in \r
*.cmd             text eol=crlf   # cmd.exe mis-parses a multi-line if-block in LF
*.bat             text eol=crlf
```

A repo with no `.gitattributes` at all is the dangerous case: everything falls
through to the user's `core.autocrlf`, so on Windows the `.sh` and the config
are the ones that get mangled. Adding a rule for `*.cmd` alone makes it worse,
not better — it fixes the file that was already fine and leaves the other two
to chance. `git check-attr eol -- <file>` tells you what will actually happen.

### 3. `scripts/revmux-review.sh`

The logic. See this repo's copy for the full file; the parts that matter:

```bash
set -uo pipefail                      # NOT -e — see trap 4

DONE_SIGNAL='<<<RALPHEX:CODEX_REVIEW_DONE>>>'
finish() { printf '%s\n' "$DONE_SIGNAL"; }
trap finish EXIT                      # every exit path — see trap 5

# One revmux task per PLAN, one run per iteration.
PLAN_NAME="$(grep -m1 -oE '[^ /\\]+\.md' "$PROMPT_FILE" | head -1 | sed 's/\.md$//')"
TASK="ralphex-${PLAN_NAME}"

# Which round this is — the rounds already on disk, +1. Works only because
# the task is keyed on the PLAN and so stays stable across iterations.
ROUNDS_DONE=$(find ".revmux/tasks/$TASK" -mindepth 1 -maxdepth 1 -type d | wc -l)
ROUND=$((ROUNDS_DONE + 1))

# The round number LEADS the run name, and not just for sorting: a bare
# timestamp is second-resolution, so two rounds landing in the same second
# would silently share a directory — one round reusing the other's inputs,
# corrupting the history revmux carries forward.
RUN="$(printf '%02d' "$ROUND")-$(date +%Y%m%d-%H%M%S)"

# The roster narrows by round — the convergence control. See below.
if   [ -n "${RALPHEX_REVMUX_PROFILE:-}" ]; then PROFILE="$RALPHEX_REVMUX_PROFILE"
elif [ "$ROUND" -le 2 ]; then                   PROFILE="comprehensive"
else                                            PROFILE="final"
fi

PATHS_JSON="$(revmux new --task "$TASK" --run "$RUN")"
# write BOTH the scope and the profile revmux allocates

revmux --task "$TASK" --run "$RUN" \
       --profile "$PROFILE" --min-confidence 60 \
       --hard-timeout 40m --markdown --no-tui \
       --workdir "$REPO_ROOT" 2>&1 || true
exit 0                                # always — see trap 4
```

## The five traps

Each of these cost real debugging time. Two of them killed runs in a second
repository as well, independently.

1. **`.sh` cannot be the hook on Windows.** `exec.Command(script, promptFile)`
   is a direct exec with no shell. Bridge through a `.cmd`.
2. **Forward slashes in the config path.** `'scripts' is not recognized`.
   Backslashes only.
3. **`Git\usr\bin\bash.exe` has no `PATH`.** `date: command not found`, then
   `sed`, `cp`, `wc`, `basename`. Use `Git\bin\bash.exe`.
4. **`set -e` plus revmux's exit code.** revmux exits non-zero when it *has*
   findings, the way a linter does. Under `set -e` the script dies one line
   before handing the report over, and ralphex sees a crashed hook — a review
   that worked perfectly reports as a failure. Never propagate revmux's status;
   always `exit 0`. A review tool that takes the whole run down when it has
   something to say is worse than no review tool.
5. **The missing `DONE` signal.** ralphex watches the stream for
   `<<<RALPHEX:CODEX_REVIEW_DONE>>>` and waits out its idle timeout without it.
   Emit it from a `trap ... EXIT` so failure paths emit it too. This one hides
   behind trap 4: while the hook is crashing, ralphex bails on the error and
   never reaches the wait — fix the crash without adding the signal and the
   next run hangs instead.

## Design choices that matter

- **Key the revmux task on the PLAN, not the branch.** revmux carries earlier
  rounds into every later prompt, so round 2 knows what round 1 reported and
  what was done about it. A branch usually carries several plans; keying on it
  merges their histories.
- **Write the profile file, not just the scope.** `revmux new` allocates both.
  Left empty, the panel reports the same non-findings every round — "add a test
  for the renderer", "no e2e coverage" — against a codebase that deliberately
  has neither. Put the conventions there: what is testable, what counts as a
  finding, what does not.
- **Pass ralphex's prompt through verbatim** as the scope. It already carries
  the goal, the diff command for this iteration, and the plan path. Paraphrase
  is where review context silently goes missing between rounds.
- **`--hard-timeout 40m`.** The 20m default is close enough to bite: a single
  round here ran ~15m with agents at 3.7M tokens. An agent killed mid-read
  reports nothing, and a review that silently covered less is worse than a slow
  one.

## Operating it

**Test the wiring without paying for a panel.** Give the hook a
`RALPHEX_REVMUX_DRY_RUN=1` branch that creates the round, writes the scope, and
stops:

```bash
RALPHEX_REVMUX_DRY_RUN=1 cmd //c "scripts\revmux-review.cmd" "$PROMPT_FILE"
```

`cmd //c` from Git bash reproduces ralphex's exec closely enough. Expect exit 0
and the `DONE` signal.

**The three escape hatches**, all read from the environment:

| Variable | Effect |
|---|---|
| `RALPHEX_REVMUX_PROFILE` | pins one profile for **every** round — which turns the round ladder off, and with it the thing that ends the Minor-docs tail. Set it to debug a roster, not to run a loop. |
| `RALPHEX_REVMUX_MIN_CONFIDENCE` | the panel's reporting threshold (default 60) |
| `RALPHEX_REVMUX_HARD_TIMEOUT` | the per-round wall clock (default 40m) |

**Run it in a split pane**, not a new session — easier to watch beside the work.

**Monitor the progress log, not the terminal.** The pane's scrollback is lost
exactly when you need it. Two things a monitor must get right:

- **Gate on timestamps.** Progress logs accumulate across runs, so a failure
  from hours ago reads as live. This produced a false alarm that nearly had a
  fixed problem "fixed" again.
- **Exit if the process dies**, not only on success. A watch that greps for the
  success marker stays silent through a crash, and silence looks identical to
  still-running.

**Commit before RESTARTING a run** — not before every round. The diff
instruction changes between iterations:

| Iteration | Diff |
|---|---|
| the first of a run | `git diff <base-ref>...HEAD` — committed work only |
| every later one | plain `git diff` — the uncommitted working tree |

So a running loop reviews its own uncommitted fixes and needs no help. But a
**restart** begins again at `base...HEAD` and is blind to the working tree —
which is exactly how a rate limit costs you work: ralphex finishes and verifies
a fix, dies before committing it, and the restart's panel re-reports defects
that are already fixed. When a run dies mid-round, verify and commit what it
left behind before restarting.

**On a rate limit**, pass `--wait 2h`. ralphex detects the limit, sleeps and
resumes by itself. Check for orphaned work first — it may have finished and
verified a fix without reaching the commit.

**To resume after the task phase is done**, use `--external-only`. ⚠️ It does
**not** move the plan to `docs/plans/completed/`, so a plan left in
`docs/plans/` is not evidence the work is unfinished.

**Do not commit tooling onto the plan's base while it runs.** Hook edits landed
between `--base-ref` and HEAD get reviewed as if they were plan work; a panel
duly reported "two script files in the branch diff are not documentation" and a
round was spent on it.

## Knowing when to stop

The loop does not always converge on its own, and the failure is subtle: **each
round's own documentation fix seeds the next round's findings.** A fix lands,
its comment update introduces a small inconsistency, the next panel finds it.
That can oscillate indefinitely.

- **Watch severity, not count.** Counts bounce around — 8 → 2 → 7 → 4 → 8 is a
  healthy run. Major → all-Minor-docs is convergence. A Major every round means
  each fix is introducing the next defect, which is a different problem.
- **The clearest stop signal is a round whose findings are all about the
  previous round's own fix.** The pie run ended 7 → 2 → 0 → 3, and all three of
  the last round's findings were about a sentence the round before had
  rewritten. At that point the loop is reviewing itself, not the work.
- **Verify the premise in Task 1 and let the plan say STOP.** The one run that
  never raised a single Major was the one whose plan opened by proving its own
  central claim — against the schema *and* a corpus of real files — before any
  behaviour changed, with instructions to halt and rewrite the plan if the
  claim failed. That task also turned up a detail nobody had considered
  (`<c:idx>` need not be contiguous), which the later tasks were then written
  to handle rather than trip over.
- **Adjacent pre-existing issues are a stopping signal.** When the panel starts
  reaching into problems the plan does not cover, file them as their own plan.
  Do not let them be smuggled into the branch.
- **Watch for scope creep in fix rounds.** A new model field appeared here to
  fix a finding about a fix about a finding — three levels deep — and generated
  two Majors of its own, including a regression that could write a `<c:f>`
  naming a sheet the workbook no longer had. A review round is the wrong place
  to add a field.
### Bounding the cost

Left alone the external loop runs `max(3, max_iterations/5)` = **10** rounds,
each a full panel. That is the token sink, and the shape above is why: across
four plans here, every Major finding arrived in round 1 or 2, and every round
after that was Minor documentation seeding the next round's Minor documentation.

Three layers, cheapest first:

1. **The roster narrows by round** (`scripts/revmux-review.sh`). Rounds 1–2 get
   `comprehensive` — four agents, where the real findings come from. Round 3 on
   gets `final`: two agents, and nothing below Major reported. This is what
   actually ends the tail, and it ends it *by construction* — no Majors means no
   findings, and ralphex stops. It is also the layer that costs least when it is
   wrong, because a real Major is still reported.
2. **`review_patience = 2`** stops the loop after two rounds with no commits,
   which is what a standing disagreement between the panel and ralphex looks
   like. It is `0` (disabled) by default; this repo pins it.
3. **`max_external_iterations = 3`** is the hard ceiling, for when the first two
   are wrong about something.

The layers bound different things, which is easy to misread. The hard ceiling
bounds **one run**, so within a single run the roster narrows for at most its
last permitted round. The round counter is derived from the directories under
`.revmux/tasks/$TASK`, which persist — so the ladder also carries across
**re-runs of the same plan**: resume a plan that has had three rounds and the
next one is already `final`.

## Writing plans for this loop

- **Cite symbol names, not `:NNNN` anchors.** A plan is a contract handed to an
  implementing agent, and line numbers drift under the very commit that writes
  them. Two rounds here were spent on citations that were stale the moment they
  were written — including once when they were "refreshed" from HEAD rather
  than the working tree that added lines above every one of them.
- **Record deliberate scope decisions.** The panel respects them: tell the
  reviewers that a finding arguing against a documented decision is not a
  finding, and they will honour it.
- **Name the primary failure mode.** Saying so in the plan is what made the
  panel look for it — and find it.
- **State the build commands, including the traps.** This repo has two cargo
  workspaces, and a green build in one says nothing about the other; putting
  that in the plan and the profile stopped it recurring.

## What the loop is actually good at

Not typos. In this run the panel found, with all four agents confirming
independently:

- A rule that flipped a chart's orientation on reload for a supported edit.
- A fix that moved a cross-sheet defect from one slot onto another — the same
  bug, relocated, with the loader regressing identically so a save-and-reload
  agreed on the wrong answer rather than correcting it.
- A test asserting `x == x`, which passed for any implementation.
- A guarantee stated in a doc comment that the code had stopped honouring.

Several of these were in changes made *by hand* between runs, and would not
have been reviewed at all otherwise.
