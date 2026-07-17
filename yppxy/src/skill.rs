//! The bundled agent skill (agwinterm-style self-onboarding): a `SKILL.md` that
//! teaches an agent to drive a live yppxy via the MCP tools or the control pipe.
//! `yppxy install skill` writes it via [`ctlcore::install_skill`].

/// The skill document installed for agents to self-onboard.
pub const SKILL_MD: &str = r#"---
name: yppxy
description: Use when reading or editing a Microsoft Project schedule (.mpp/.xml/.yppx) that is open in a yppxy editor (often a sibling agwinterm pane) — list tasks with scheduled dates and critical path, add/edit/delete tasks, set durations, link dependencies, and save, via the yppxy_* MCP tools or yppxy's control pipe. Edits land on yppxy's own undo stack, reschedule the plan (CPM), and repaint the Gantt live, so this beats editing the file on disk (no clobbering unsaved work, no stale reads).
---

# yppxy

yppxy is a terminal MS Project editor with a real CPM scheduler and a control
surface. You can read and edit the **live** open schedule — including unsaved
changes — instead of touching the file on disk. Your edits go through yppxy's
own edit path: they are undoable, recompute the schedule, and repaint the Gantt
pane instantly.

## When to use
The user has a schedule open in yppxy (often in a split pane next to you) and
asks you to read or change it: "what's on the critical path?", "add a testing
phase after Build", "make Design 5 days and link it before Review".

## Preferred: MCP tools
If the `yppxy` MCP server is configured (`claude mcp add yppxy -- yppxy --mcp`),
use its tools:
- `yppxy_list` — which yppxy editors are running
- `yppxy_status` — path, modified flag, task count, project start/finish
- `yppxy_tasks` — every task: uid, name, outline level, duration, scheduled
  start/finish, critical flag, slack, predecessors
- `yppxy_get` `{uid}` — one task
- `yppxy_set` `{uid, name?, duration?, level?}` — edit a task (duration like
  "3d", "4h", "2w"; level = outline depth 1..20)
- `yppxy_add` `{after?, name?, duration?}` — insert a task after uid `after`
  (or append); returns the new task with its uid
- `yppxy_del` `{uid}` — delete a task (dangling links are dropped)
- `yppxy_link` `{uid, pred, type?, lag?}` — make task `uid` depend on `pred`
  (type FS/SS/FF/SF, default FS; lag like "1d")
- `yppxy_unlink` `{uid, pred}`
- `yppxy_find` `{query}` — tasks by name substring
- `yppxy_save` `{path?}`

Tasks are addressed by **uid** (stable). When several yppxy editors are open,
pass `target` (a substring of the pane id; `yppxy_list` shows them). Typical
flow: `yppxy_tasks` to see the plan, then `yppxy_set`/`yppxy_add`/`yppxy_link`,
then `yppxy_save`.

## Fallback: the control pipe
Without MCP, talk to yppxy directly over loopback TCP + newline-delimited JSON.
Find the discovery file in `%APPDATA%\yppxy\ctl\` (Windows) or
`$XDG_CONFIG_HOME/yppxy/ctl/` (Unix): `<instance>.json` = `{port, token, pid}`,
where instance is `yppxy-<AGWINTERM_SESSION_ID>` — the pane id shown by
`agwintermctl tree`. Then one request per line:

```
→ {"token":"…","verb":"task.set","args":{"uid":3,"duration":"5d"}}
← {"ok":true,"result":{ … }}
```

Verbs: `proj.path`, `task.list`, `task.get`, `task.set`, `task.add`, `task.del`,
`link.add`, `link.del`, `find`, `proj.save`, `proj.reload`, `proj.open`.

## Two panes in one agwinterm session
`agwintermctl split on`, then launch `yppxy <file>` in the new pane (or press
Ctrl+D and run yppxy there). Now you sit beside the schedule and edit it live.

## Notes
- Read `yppxy_tasks` before editing; scheduled dates come from the CPM engine,
  so changing a duration or link moves every dependent task.
- Each edit is one undo step — the user can undo it in yppxy (Ctrl+Z).
- While you edit, the yppxy pane's status dot flashes active, so the user sees
  the plan being worked on.
"#;

/// Install `SKILL.md` for self-discovery (see [`ctlcore::install_skill`]).
pub fn install() -> std::io::Result<String> {
    ctlcore::install_skill("yppxy", SKILL_MD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_frontmatter_and_verbs() {
        assert!(SKILL_MD.starts_with("---\nname: yppxy\n"));
        assert!(SKILL_MD.contains("description:"));
        for needle in [
            "yppxy_set",
            "task.set",
            "AGWINTERM_SESSION_ID",
            "agwintermctl split on",
        ] {
            assert!(SKILL_MD.contains(needle), "skill missing {needle}");
        }
    }
}
