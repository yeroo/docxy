# How docxy is worked on

One way of working for every machine and every Claude session that changes
docxy: the queues on the Mac, Linux and Windows, the work laptop, and ad-hoc
sessions. If a rule here and a habit disagree, this file wins. Change it by PR.

## The unit of work

- Everything starts as a **GitHub issue** with a `priority:P0`–`priority:P3`
  label. Work nobody filed gets an issue first.
- One issue → one branch `issue-<n>-<slug>` → one PR → merged by
  `merge-check` (auto-merge), never by a direct push to `main`.
- An issue is done when its PR is merged with CI green, including the UI
  sweeps. Leftovers become `follow-up` issues, not unchecked boxes.

## Tooling: agworkbench everywhere

Issues are worked through [agworkbench](https://github.com/yeroo/agworkbench):
a planner and an implementer in one checkout. They agree a plan in writing
before any code, review with revmux and open the PR. They merge when
`merge-check` is ok.

- **Queues.** Each machine runs a named queue of P0/P1 issues:

  ```
  github-workbench -Queue <issues> -Repo yeroo/docxy -QueueName <machine>
                   -Parallel 1 -Implementer claude -AutoMerge -Autonomous
  ```

  Without `-Autonomous`, a merged member never closes its sessions. Set
  `ceilingExtra: 0` in `~/.agworkbench.json` for strictly one live issue
  (agworkbench#126).
- **Ad-hoc sessions** follow the same contract by hand: claim, branch,
  review with revmux, PR, `merge-check`.

## Claiming an issue

Two machines must never work on the same issue. Before starting one:

1. **Check:**
   - Skip the issue if any of these is true: it has the `in-progress` label
     or a **Grabbed** comment, an open or merged PR mentions it, or a remote
     `issue-<n>-*` branch exists.
   - Queues apply the same check to every pending member before it launches.
2. **Claim:** add the `in-progress` label and comment
   `Grabbed by <machine> (<session or queue>)`.
3. **Release:** remove `in-progress` when the PR merges, or when the work is
   abandoned. When abandoning, comment why.

An issue the user hands to a session directly is that session's to claim,
right away.

## Models and permissions

- **Claude only**, through a Claude subscription or Amazon Bedrock. No OpenAI
  models anywhere: no Codex implementer, no Codex or GPT reviewers. Every
  revmux profile is Claude-only.
- **Auto mode in every agent session**, via `permissions.defaultMode: "auto"`
  in `~/.claude/settings.json`. Never pass `--permission-mode` through
  `claudeArgs`: the Claude implementer pane refuses it and does not start.
- **The planner's last step must not be refused.** Allow
  `wb.py loop-state` in the Claude settings. A refused
  `loop-state done --pr <n> --sha <sha>` leaves a merged issue "active", and
  the queue stops.

## Review and CI

- **revmux rounds:** up to five per issue. A sixth needs the user's go-ahead.
  A round with no Major lets `merge-check` pass at the cap.
- **CI:** all checks green. A UI sweep that fails twice on the same case is a
  real failure, not a flake. Fix it from the run's uploaded transcript
  artifact; don't rerun it again.
- **Missed mail:** if CI has finished and the planner is still idle, its relay
  mail was lost. Tell it the result.

## Talking between machines

**The Mac session `docxy-6c` coordinates.** The user reaches the machines
remotely only through that session, so it is the one place to escalate to.
A session on any other machine that is stuck asks `docxy-6c`, through the
channel below, and keeps working on whatever it can meanwhile. Write the
question in a comment that names `docxy-6c`, in the issue it concerns. That
issue can be public or in a private repo. The coordinator picks up every
comment that names it within 15 minutes. Stuck means a
decision it can't make from the issue and the repo, a conflict with another
machine, broken tooling or credentials, or anything that needs the user. The
coordinator answers what it can and takes the rest to the user.

- **Through GitHub only.** On the public repo the channel is the issue itself:
  claim comments, plus questions to whoever grabbed it.
- **Private work stays private.** Work in a private repo (such as
  format-plugin repos) keeps its coordination thread in that repo. Never copy
  private details into public issues or PRs.
- **Mark who wrote it.** Sessions often post as the same GitHub user, so start
  every cross-machine comment with `**[<machine>]**`.
- **Wording.** In issue and PR comments, never use the words "hold", "wait",
  "wip" or "do not merge".

## Per-machine notes

- **Disk.** A checkout with `suite/target` reaches 7–9 GB. Put a
  `.cargo/config.toml` above the checkout root with `debug = "line-tables-only"`
  and `incremental = false`, and set `minFreeGB` to what the disk can spare.
- **macOS.** agterm starts commands with launchd's minimal `PATH`, so
  wrap launched commands to restore the user's `PATH`. Grant agterm
  Accessibility and Screen Recording for UI testing.
- **Bedrock.** Set `CLAUDE_CODE_USE_BEDROCK=1`, `AWS_REGION` and the AWS
  profile or SSO in the environment the terminal starts its panes with, so
  agent panes, revmux children and the relay all inherit them. An expired
  SSO login stops every agent: log in again, then resume the panes.
