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
- `yppxy_tasks` — every task: uid, name, outline level, manual, duration, scheduled
  start/finish, critical flag, slack, predecessors. Summaries also have
  `rollup_start`/`rollup_finish` (their subtasks' span); a manually scheduled
  summary keeps its own start/finish, which can differ, and has `warning`
  (true when its subtasks finish after it, or it finishes after its parent
  manual summary). Setting a manual summary's
  duration sets its own span.
- `yppxy_get` `{uid, fields?}` — one task. Every task carries its row `id` and
  `outline_number`; `fields` (also taken by `yppxy_tasks`, `yppxy_find` and the
  editing tools) reads any Project field by name, e.g. `["% Complete", "Actual
  Start", "Baseline Finish", "Finish Variance", "Total Slack", "Cost"]`, as
  `fields: {name: {text, value}}`: `text` as the sheet shows it (`2 days`,
  `$1,400.00`, `NA`), `value` underneath (dates `YYYY-MM-DD HH:MM`; durations,
  work and slack in minutes; money a number; `null` when unset). An unknown
  name fails the call, before any edit
- `yppxy_fields` — every field name `fields` can read
- `yppxy_set` `{uid, name?, duration?, level?, manual?}` — edit a task (duration like
  "3d", "4h", "2w"; level = outline depth 1..20; manual = true pins the task at
  its current dates as Manually Scheduled, false makes it Auto Scheduled)
- `yppxy_add` `{after?, name?, duration?}` — insert a task after uid `after`
  (or append); returns the new task with its uid. Without `duration` it is
  1 day, estimated unless the plan's NewTasksEstimated is off. When a
  finish-to-start link joins `after` to the next task, the new task is linked
  into that chain (A->B becomes A->N->B) while the plan's Autolink is on (the
  default)
- `yppxy_del` `{uid}` — delete a task; a summary takes its subtasks with it
  (the reply's `removed` lists every UID; dangling links are dropped)
- `yppxy_link` `{uid, pred, type?, lag?}` — make task `uid` depend on `pred`
  (type FS/SS/FF/SF, default FS; lag like "1d")
- `yppxy_unlink` `{uid, pred}`
- `yppxy_find` `{query}` — tasks by name substring
- `yppxy_save` `{path?}`
- `yppxy_assign_list` `{uid?, resource?, fields?}` — resource assignments:
  every one, or those of task `uid` and/or `resource` (a resource uid, or a
  name ignoring ASCII case). Each has its own `uid`, `task`, `resource`,
  `resource_name`, `units` (a fraction, 1.0 = 100%), `work_hours`, `cost`,
  `rate_table`, actual, remaining and baseline values, `start`/`finish`
- `yppxy_assign_get` `{uid, fields?}` — one assignment by its own uid
- `yppxy_assign_fields` — every assignment field name their `fields` can read.
  A field's `value` gives work and delay in minutes, unlike `work_hours`,
  `delay_hours` and the `work`/`delay` arguments, which are hours
- `yppxy_assign_add` `{task, resource, units?, work?, fields?}` — assign a
  resource (uid, or name: a new name becomes a work resource; one that is a
  number or contains a comma is refused) to task `task`
- `yppxy_assign_set` `{uid, units?, work?, overtime_work?, contour?,
  rate_table?, delay?, actual_work? | percent_work_complete?, fields?}` —
  units as `0.5` or `"50%"`, work, overtime, actual work and delay as hours or
  `"40h"`/`"5d"`, the contour by name (`"Back Loaded"`) or code 0..7, the rate
  table `"A"`..`"E"`, the percent 0..100 or `"50%"`; the task reschedules by
  its type. Overtime is priced, not scheduled; actual work derives remaining
  work, cost and % work complete but not the task's own progress
- `yppxy_assign_del` `{uid}` — the reply is `{deleted, task}`

Assignments are addressed by their own uid, from `yppxy_assign_list` (whose
`uid` filter is a task's). Tasks are addressed by **uid** (stable). When
several yppxy editors are open, pass `target` (a substring of the pane id;
`yppxy_list` shows them). Typical flow: `yppxy_tasks` to see the plan, then
`yppxy_set`/`yppxy_add`/`yppxy_link`, then `yppxy_save`.

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

Verbs: `proj.path`, `task.list`, `task.get`, `task.fields`, `task.set`, `task.add`,
`task.del`, `link.add`, `link.del`, `find`, `assign.list`, `assign.get`,
`assign.fields`, `assign.add`, `assign.set`, `assign.del`, `proj.save`,
`proj.reload`, `proj.open`.

Assignments are addressed by their own UID, as `assign.list {uid?, resource?}`
reports it. `assign.add {task, resource, units?, work?}` takes a resource uid or
name (a new name becomes a work resource; one that is a number or contains a
comma is refused, so pass a uid as a number); `assign.set {uid, units?, work?,
overtime_work?, contour?, rate_table?, delay?, actual_work? |
percent_work_complete?}` takes units as `0.5` or `"50%"`, work, overtime,
actual work and delay as hours or `"40h"`/`"5d"`, the contour as a name or code
0..7, the rate table as `"A"`..`"E"`, the percent as 0..100 or `"50%"`, and
reschedules the task by its type. Each assignment reports `units`, `work_hours`, `cost`, `rate_table`,
overtime, actual, remaining and baseline values, and its own `start`/`finish`;
`fields` reads Project's assignment fields (`assign.fields` lists them).

## Two panes in one agwinterm session
`agwintermctl split on`, then launch `yppxy <file>` in the new pane (or press
Ctrl+D and run yppxy there). Now you sit beside the schedule and edit it live.

## Notes
- Read `yppxy_tasks` before editing; scheduled dates come from the CPM engine,
  so changing a duration or link moves every dependent task.
- Each edit is one undo step — the user can undo it in yppxy (Ctrl+Z) until
  the next save: as in Project, a successful save clears undo and redo. Save
  only when asked, and say first that the edits can no longer be undone.
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
            "rollup_finish",
            "warning",
            "yppxy_fields",
            "task.fields",
            "outline_number",
            "assign.set",
            "assign.fields",
            "yppxy_assign_set",
        ] {
            assert!(SKILL_MD.contains(needle), "skill missing {needle}");
        }
    }
}
