//! The shared Project control surface: maps [`ctlcore`] verbs onto the **live** project,
//! so an external agent (e.g. Claude Code in a sibling agwinterm pane) can read
//! and edit the open schedule without touching the file on disk.
//!
//! Every mutating verb snapshots the project first (through [`Editor`]), so an
//! agent's edits land on the *same* undo stack as keyboard edits, reschedule
//! the plan (CPM), and repaint the Gantt live; reads serialize the in-memory
//! project + schedule, so they always reflect unsaved changes.
//!
//! Tasks are addressed by **UID** (stable across reordering); `task.list`
//! reports each task's uid alongside its scheduled dates.
//!
//! ## Verbs
//!
//! | Verb | Args | Result |
//! |---|---|---|
//! | `proj.path` | — | `{path, modified, name, tasks, start, finish}` |
//! | `task.list` | — | `{count, tasks:[{uid, name, level, manual, duration, start, finish, critical, …}]}` |
//! | `task.get` | `{uid}` | one task |
//! | `task.set` | `{uid, name?, duration?, level?, manual?}` | the updated task (`manual`: `true` Manually / `false` Auto Scheduled) |
//! | `task.add` | `{after?, name?, duration?}` | the new task |
//! | `task.del` | `{uid}` | `{deleted, removed:[uid…]}` (a summary takes its subtree) |
//! | `link.add` | `{uid, pred, type?, lag?}` | the updated task (`lag` as the Predecessors cell spells it: `4h`, `2ed`, `50%`) |
//! | `link.del` | `{uid, pred}` | the updated task |
//! | `find` | `{query}` | `{count, tasks:[…]}` |
//!
//! `path_info` provides the common `proj.path` fields. File verbs (`proj.save`,
//! `proj.reload`, `proj.open`) belong to the host and are not dispatched here.

use ctlcore::json::Json;
use projcore::datetime::DateTime;
use projcore::editor::{Editor, TaskPatch, parse_duration, parse_lag};
use projcore::model::{LagFormat, LinkType, Predecessor, Task};

/// Successful calls to these verbs signal agent editing activity.
pub const MUTATING: &[&str] = &["task.set", "task.add", "task.del", "link.add", "link.del"];

/// Recognize editor verbs before a host resolves its target document.
pub fn is_editor_verb(verb: &str) -> bool {
    MUTATING.contains(&verb) || matches!(verb, "task.list" | "task.get" | "find")
}

/// Project verbs independent of the host's file handling. `None` means the
/// verb belongs to the host (or is unknown).
pub fn dispatch_editor(ed: &mut Editor, verb: &str, args: &Json) -> Option<Result<Json, String>> {
    Some(match verb {
        "task.list" => Ok(task_list(ed)),
        "task.get" => task_get(ed, args),
        "task.set" => task_set(ed, args),
        "task.add" => task_add(ed, args),
        "task.del" => task_del(ed, args),
        "link.add" => link_add(ed, args),
        "link.del" => link_del(ed, args),
        "find" => find(ed, args),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Read-only verbs
// ---------------------------------------------------------------------------

fn dt_str(dt: DateTime) -> String {
    let p = dt.parts();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        p.year, p.month, p.day, p.hour, p.minute
    )
}

pub fn path_info(path: Option<&str>, ed: &Editor) -> Json {
    Json::obj(vec![
        (
            "path",
            match path {
                Some(p) => Json::Str(p.to_string()),
                None => Json::Null,
            },
        ),
        ("modified", Json::Bool(ed.dirty())),
        ("name", Json::Str(ed.project().name.clone())),
        ("tasks", Json::Num(ed.project().tasks.len() as f64)),
        ("start", Json::Str(dt_str(ed.schedule().project_start))),
        ("finish", Json::Str(dt_str(ed.schedule().project_finish))),
    ])
}

fn link_name(l: LinkType) -> &'static str {
    match l {
        LinkType::FinishStart => "FS",
        LinkType::StartStart => "SS",
        LinkType::FinishFinish => "FF",
        LinkType::StartFinish => "SF",
    }
}

fn parse_link_name(s: &str) -> Option<LinkType> {
    match s.to_ascii_uppercase().as_str() {
        "FS" => Some(LinkType::FinishStart),
        "SS" => Some(LinkType::StartStart),
        "FF" => Some(LinkType::FinishFinish),
        "SF" => Some(LinkType::StartFinish),
        _ => None,
    }
}

/// One task as JSON, including its scheduled (or leveled) dates.
fn task_json(ed: &Editor, t: &Task) -> Json {
    let preds = t
        .predecessors
        .iter()
        .map(|p| {
            Json::obj(vec![
                ("uid", Json::Num(p.uid as f64)),
                ("type", Json::Str(link_name(p.link).to_string())),
                // Minutes of working or elapsed time, or a percentage of
                // the predecessor's duration, as `lag_format` (MSPDI's
                // LagFormat code) says.
                ("lag", Json::Num(p.lag as f64)),
                ("lag_format", Json::Num(p.lag_format.code() as f64)),
            ])
        })
        .collect();
    let mut fields = vec![
        ("uid", Json::Num(t.uid as f64)),
        ("name", Json::Str(t.name.clone())),
        ("level", Json::Num(t.outline_level as f64)),
        ("summary", Json::Bool(t.summary)),
        ("milestone", Json::Bool(t.is_milestone())),
        ("manual", Json::Bool(t.manual)),
        (
            "duration_days",
            Json::Num(ed.project().minutes_to_days(t.duration_min)),
        ),
        ("predecessors", Json::Arr(preds)),
    ];
    if let Some(s) = ed.disp_start(t.uid) {
        fields.push(("start", Json::Str(dt_str(s))));
    }
    if let Some(f) = ed.disp_finish(t.uid) {
        fields.push(("finish", Json::Str(dt_str(f))));
    }
    // A summary's rolled-up span. A manual summary keeps its own start and
    // finish, and warns when its subtasks finish after it.
    if let Some((start, finish)) = ed.disp_rollup(t.uid) {
        fields.push(("rollup_start", Json::Str(dt_str(start))));
        fields.push(("rollup_finish", Json::Str(dt_str(finish))));
    }
    if t.summary && t.manual {
        fields.push(("warning", Json::Bool(ed.summary_warning(t.uid))));
    }
    if let Some(r) = ed.schedule().get(t.uid) {
        fields.push(("critical", Json::Bool(r.critical)));
        fields.push((
            "slack_days",
            Json::Num(ed.project().minutes_to_days(r.total_slack_min)),
        ));
    }
    Json::obj(fields)
}

fn task_list(ed: &Editor) -> Json {
    let tasks = ed
        .project()
        .tasks
        .iter()
        .map(|t| task_json(ed, t))
        .collect();
    Json::obj(vec![
        ("count", Json::Num(ed.project().tasks.len() as f64)),
        ("tasks", Json::Arr(tasks)),
    ])
}

fn find(ed: &Editor, args: &Json) -> Result<Json, String> {
    let query = args.get_str("query").ok_or("find needs a 'query'")?;
    let needle = query.to_lowercase();
    let tasks: Vec<Json> = ed
        .project()
        .tasks
        .iter()
        .filter(|t| t.name.to_lowercase().contains(&needle))
        .map(|t| task_json(ed, t))
        .collect();
    Ok(Json::obj(vec![
        ("query", Json::Str(query.to_string())),
        ("count", Json::Num(tasks.len() as f64)),
        ("tasks", Json::Arr(tasks)),
    ]))
}

// ---------------------------------------------------------------------------
// Mutating verbs (undoable snapshots, rescheduled)
// ---------------------------------------------------------------------------

fn uid_arg(args: &Json, key: &str) -> Result<i32, String> {
    args.get(key)
        .and_then(Json::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| format!("needs a numeric '{key}' (a task UID)"))
}

fn task_index(ed: &Editor, uid: i32) -> Result<usize, String> {
    ed.project()
        .tasks
        .iter()
        .position(|t| t.uid == uid)
        .ok_or_else(|| format!("no task with uid {uid}"))
}

fn task_get(ed: &Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let i = task_index(ed, uid)?;
    Ok(task_json(ed, &ed.project().tasks[i]))
}

fn task_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let duration_min = args
        .get_str("duration")
        .map(|d| {
            parse_duration(d, ed.project())
                .ok_or_else(|| format!("Couldn't read duration '{d}' (try 3d, 4h, 2w)"))
        })
        .transpose()?;
    let level = args
        .get("level")
        .map(|l| {
            l.as_i64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("'level' must be 1..=20")
        })
        .transpose()?;
    let manual = args
        .get("manual")
        .map(|m| m.as_bool().ok_or("'manual' must be true or false"))
        .transpose()?;
    ed.update_task(
        uid,
        TaskPatch {
            name: args.get_str("name").map(str::to_string),
            duration_min,
            level,
            manual,
        },
    )?;
    task_get(ed, args)
}

fn task_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let after = args
        .get("after")
        .map(|_| uid_arg(args, "after"))
        .transpose()?;
    let duration_min = match args.get_str("duration") {
        Some(d) => parse_duration(d, ed.project())
            .ok_or_else(|| format!("Couldn't read duration '{d}' (try 3d, 4h, 2w)"))?,
        None => 480,
    };
    let at = ed.add_task(
        after,
        args.get_str("name").unwrap_or("New task"),
        duration_min,
    )?;
    Ok(task_json(ed, &ed.project().tasks[at]))
}

fn task_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let removed = ed.delete_task(uid)?;
    Ok(Json::obj(vec![
        ("deleted", Json::Num(uid as f64)),
        (
            "removed",
            Json::Arr(removed.into_iter().map(|u| Json::Num(u as f64)).collect()),
        ),
    ]))
}

fn link_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let pred = uid_arg(args, "pred")?;
    let link = match args.get_str("type") {
        Some(t) => parse_link_name(t).ok_or("'type' must be FS, SS, FF, or SF")?,
        None => LinkType::FinishStart,
    };
    // The Predecessors cell's lag grammar; a bare number stays working days.
    let (lag, lag_format) = match args.get_str("lag") {
        Some(l) => parse_lag(l, ed.project())
            .or_else(|| parse_duration(l, ed.project()).map(|min| (min, LagFormat::DAYS)))
            .ok_or_else(|| format!("couldn't read lag '{l}' (try 1d, 4h, 2ed, 50%)"))?,
        None => (0, LagFormat::DAYS),
    };
    ed.add_link(
        uid,
        Predecessor {
            uid: pred,
            link,
            lag,
            lag_format,
        },
    )?;
    task_get(ed, args)
}

fn link_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    ed.remove_predecessor(uid_arg(args, "uid")?, uid_arg(args, "pred")?)?;
    task_get(ed, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_project() -> projcore::Project {
        let mut p = projcore::editor::untitled_project();
        p.tasks.push(Task {
            uid: 1,
            id: 1,
            name: "New task".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        p
    }
    fn app() -> Editor {
        Editor::new(new_project())
    }
    fn add(ed: &mut Editor, name: &str, dur: &str) -> i64 {
        task_add(
            ed,
            &Json::obj(vec![
                ("name", Json::Str(name.into())),
                ("duration", Json::Str(dur.into())),
            ]),
        )
        .unwrap()
        .get("uid")
        .unwrap()
        .as_i64()
        .unwrap()
    }
    #[test]
    fn summaries_report_their_rollup_and_manual_ones_a_warning() {
        let mut p = new_project();
        p.tasks[0].name = "Phase".into();
        for uid in [2, 3] {
            p.tasks.push(Task {
                uid,
                id: uid,
                name: format!("Sub {uid}"),
                outline_level: 2,
                duration_min: 480 * i64::from(uid),
                ..Task::default()
            });
        }
        let mut ed = Editor::new(p);
        let get = |ed: &Editor, uid: i32| {
            task_get(ed, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap()
        };
        let auto = get(&ed, 1);
        assert_eq!(auto.get_str("rollup_start"), auto.get_str("start"));
        assert_eq!(auto.get_str("rollup_finish"), auto.get_str("finish"));
        assert!(auto.get("warning").is_none());
        let leaf = get(&ed, 2);
        assert!(leaf.get("rollup_start").is_none());
        assert!(leaf.get("warning").is_none());

        ed.set_manual(1, true).unwrap();
        ed.set_duration(1, "1d").unwrap();
        let manual = get(&ed, 1);
        assert_eq!(manual.get_str("rollup_start"), manual.get_str("start"));
        assert_ne!(manual.get_str("rollup_finish"), manual.get_str("finish"));
        assert_eq!(manual.get("warning"), Some(&Json::Bool(true)));
        ed.set_duration(1, "3d").unwrap();
        assert_eq!(get(&ed, 1).get("warning"), Some(&Json::Bool(false)));
    }

    #[test]
    fn add_set_and_get_a_task() {
        let mut a = app();
        let uid = add(&mut a, "Design", "3d");
        assert!(a.dirty());
        let g = task_get(&a, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap();
        assert_eq!(g.get_str("name"), Some("Design"));
        assert_eq!(g.get("duration_days").unwrap().as_f64(), Some(3.0));
        assert!(g.get("start").is_some());
        assert!(g.get("finish").is_some());

        let r = task_set(
            &mut a,
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("name", Json::Str("Design v2".into())),
                ("duration", Json::Str("5d".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_str("name"), Some("Design v2"));
        assert_eq!(r.get("duration_days").unwrap().as_f64(), Some(5.0));
    }

    #[test]
    fn links_reschedule_the_successor() {
        let mut a = app();
        let t1 = add(&mut a, "Build", "2d");
        let t2 = add(&mut a, "Test", "1d");
        let before = task_get(&a, &Json::obj(vec![("uid", Json::Num(t2 as f64))]))
            .unwrap()
            .get_str("start")
            .unwrap()
            .to_string();
        link_add(
            &mut a,
            &Json::obj(vec![
                ("uid", Json::Num(t2 as f64)),
                ("pred", Json::Num(t1 as f64)),
            ]),
        )
        .unwrap();
        let after = task_get(&a, &Json::obj(vec![("uid", Json::Num(t2 as f64))])).unwrap();
        let preds = after.get("predecessors").unwrap().as_array().unwrap();
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].get_str("type"), Some("FS"));
        // The dependent task now starts after its 2-day predecessor.
        assert_ne!(after.get_str("start").unwrap(), before);

        // And the link can be removed again.
        let r = link_del(
            &mut a,
            &Json::obj(vec![
                ("uid", Json::Num(t2 as f64)),
                ("pred", Json::Num(t1 as f64)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("predecessors").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn link_add_reads_percent_elapsed_and_bare_lags() {
        // #104: link.add spells lags as the Predecessors cell does, keeps the
        // format, and reports both the lag and its format.
        for (text, lag, format) in [
            ("50%", 50, 19),
            ("-25%", -25, 19),
            ("+2ed", 2880, 8),
            ("1ew?", 10080, 42),
            ("4h", 240, 5),
            ("2", 960, 7),
        ] {
            let mut a = app();
            let t1 = add(&mut a, "Build", "4d");
            let t2 = add(&mut a, "Test", "1d");
            let r = link_add(
                &mut a,
                &Json::obj(vec![
                    ("uid", Json::Num(t2 as f64)),
                    ("pred", Json::Num(t1 as f64)),
                    ("lag", Json::Str(text.into())),
                ]),
            )
            .unwrap();
            let preds = r.get("predecessors").unwrap().as_array().unwrap();
            assert_eq!(preds[0].get("lag").unwrap().as_i64(), Some(lag), "{text}");
            assert_eq!(
                preds[0].get("lag_format").unwrap().as_i64(),
                Some(format),
                "{text}"
            );
            assert_eq!(preds[0].get("lag_min"), None, "{text}");
        }
        let mut a = app();
        let t1 = add(&mut a, "Build", "4d");
        let t2 = add(&mut a, "Test", "1d");
        for bad in ["50e%", "1mo", "2x"] {
            let err = link_add(
                &mut a,
                &Json::obj(vec![
                    ("uid", Json::Num(t2 as f64)),
                    ("pred", Json::Num(t1 as f64)),
                    ("lag", Json::Str(bad.into())),
                ]),
            )
            .unwrap_err();
            assert!(err.contains("couldn't read lag"), "{bad}: {err}");
        }
    }

    #[test]
    fn delete_drops_dangling_links() {
        let mut a = app();
        let t1 = add(&mut a, "A", "1d");
        let t2 = add(&mut a, "B", "1d");
        link_add(
            &mut a,
            &Json::obj(vec![
                ("uid", Json::Num(t2 as f64)),
                ("pred", Json::Num(t1 as f64)),
            ]),
        )
        .unwrap();
        task_del(&mut a, &Json::obj(vec![("uid", Json::Num(t1 as f64))])).unwrap();
        let g = task_get(&a, &Json::obj(vec![("uid", Json::Num(t2 as f64))])).unwrap();
        assert_eq!(g.get("predecessors").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn deleting_a_summary_removes_its_subtree_without_asking() {
        let mut a = app();
        let phase = add(&mut a, "Phase", "1d");
        let p1 = add(&mut a, "P1", "1d");
        let p2 = add(&mut a, "P2", "1d");
        for uid in [p1, p2] {
            a.indent(uid as i32, 1).unwrap();
        }
        let r = task_del(&mut a, &Json::obj(vec![("uid", Json::Num(phase as f64))])).unwrap();
        assert_eq!(r.get("deleted"), Some(&Json::Num(phase as f64)));
        let removed: Vec<f64> = r
            .get("removed")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|j| match j {
                Json::Num(n) => *n,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(removed, [phase, p1, p2].map(|u| u as f64));
        assert_eq!(a.project().tasks.len(), 1, "only the original task is left");
    }

    #[test]
    fn find_matches_by_name() {
        let mut a = app();
        add(&mut a, "Write spec", "1d");
        add(&mut a, "Review spec", "1d");
        add(&mut a, "Ship", "1d");
        let r = find(&a, &Json::obj(vec![("query", Json::Str("spec".into()))])).unwrap();
        assert_eq!(r.get_usize("count"), Some(2));
    }

    #[test]
    fn bad_args_change_nothing() {
        let mut a = app();
        let uid = add(&mut a, "T", "1d");
        let dirty_before = a.dirty();
        let undo_before = a.undo_depth();
        assert!(
            task_set(
                &mut a,
                &Json::obj(vec![
                    ("uid", Json::Num(uid as f64)),
                    ("duration", Json::Str("banana".into())),
                ]),
            )
            .is_err()
        );
        assert!(task_get(&a, &Json::obj(vec![("uid", Json::Num(999.0))])).is_err());
        assert_eq!(a.dirty(), dirty_before);
        assert_eq!(a.undo_depth(), undo_before, "failed edits push no snapshot");
    }

    #[test]
    fn task_add_rejects_negative_duration_without_changing_editor_state() {
        let mut ed = app();
        ed.rename(1, "temporary").unwrap();
        ed.undo();
        ed.mark_saved();
        let before = ed.project().clone();
        let selected = ed.sel();
        let args = Json::parse(r#"{"name":"Invalid","duration":"-3d"}"#).unwrap();
        assert_eq!(
            dispatch_editor(&mut ed, "task.add", &args)
                .unwrap()
                .unwrap_err(),
            "Duration must not be negative"
        );
        assert_eq!(ed.project(), &before);
        assert_eq!(
            (ed.undo_depth(), ed.redo_depth(), ed.dirty(), ed.sel()),
            (0, 1, false, selected)
        );
    }

    #[test]
    fn append_without_after_inherits_last_level_and_keeps_selection() {
        let mut a = app();
        a.indent(1, 2).unwrap();
        let r = dispatch_editor(&mut a, "task.add", &Json::Null)
            .unwrap()
            .unwrap();
        assert_eq!(r.get_usize("level"), Some(3));
        assert_eq!(a.sel(), 0);
        assert_eq!(a.undo_depth(), 2);
    }

    #[test]
    fn add_after_a_summary_makes_its_first_child() {
        let mut a = app();
        let child = add(&mut a, "Child", "1d") as i32;
        a.indent(child, 1).unwrap();
        let args = Json::parse(r#"{"after":1,"name":"First"}"#).unwrap();
        let r = dispatch_editor(&mut a, "task.add", &args).unwrap().unwrap();
        assert_eq!(r.get_usize("level"), Some(2));
        let summary = task_get(&a, &Json::obj(vec![("uid", Json::Num(1.0))])).unwrap();
        assert_eq!(summary.get("summary"), Some(&Json::Bool(true)));
        let levels: Vec<_> = a.project().tasks.iter().map(|t| t.outline_level).collect();
        assert_eq!(levels, [1, 2, 2], "Child stays under the summary");
    }

    #[test]
    fn project_verbs_dispatch_on_a_bare_editor() {
        let mut ed = Editor::new(new_project());
        let r = dispatch_editor(
            &mut ed,
            "task.add",
            &Json::obj(vec![
                ("name", Json::Str("Second".into())),
                ("duration", Json::Str("2d".into())),
            ]),
        )
        .unwrap()
        .unwrap();
        let uid = r.get("uid").unwrap().clone();
        let args = Json::obj(vec![("uid", uid.clone())]);
        let r = dispatch_editor(
            &mut ed,
            "task.set",
            &Json::obj(vec![
                ("uid", uid.clone()),
                ("name", Json::Str("Changed".into())),
                ("duration", Json::Str("3d".into())),
                ("level", Json::Num(1.0)),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.get_str("name"), Some("Changed"));
        assert_eq!(r.get("duration_days").unwrap().as_f64(), Some(3.0));
        assert_eq!(ed.undo_depth(), 2, "one snapshot for a three-field patch");
        ed.undo();
        assert_eq!(ed.project().tasks[1].name, "Second");
        assert_eq!(ed.project().tasks[1].duration_min, 960);
        ed.redo();
        let link = Json::obj(vec![
            ("uid", uid.clone()),
            ("pred", Json::Num(1.0)),
            ("type", Json::Str("SS".into())),
            ("lag", Json::Str("4h".into())),
        ]);
        dispatch_editor(&mut ed, "link.add", &link)
            .unwrap()
            .unwrap();
        let pred = ed.project().tasks[1].predecessors[0];
        assert_eq!(pred.link, LinkType::StartStart);
        assert_eq!((pred.lag, pred.lag_format.code()), (240, 5));
        dispatch_editor(&mut ed, "link.del", &link)
            .unwrap()
            .unwrap();
        assert!(ed.project().tasks[1].predecessors.is_empty());
        let r = dispatch_editor(&mut ed, "task.get", &args)
            .unwrap()
            .unwrap();
        assert_eq!(r.get_str("name"), Some("Changed"));
        let r = dispatch_editor(&mut ed, "task.list", &Json::Null)
            .unwrap()
            .unwrap();
        assert_eq!(r.get_usize("count"), Some(2));
        dispatch_editor(&mut ed, "task.del", &args)
            .unwrap()
            .unwrap();
        assert_eq!(ed.project().tasks.len(), 1);
        assert!(dispatch_editor(&mut ed, "proj.save", &Json::Null).is_none());
        assert!(dispatch_editor(&mut ed, "unknown", &Json::Null).is_none());
    }

    #[test]
    fn control_find_does_not_change_selection_query_or_history() {
        let mut ed = Editor::new(new_project());
        ed.add_task(None, "Another task", 480).unwrap();
        ed.find("another");
        ed.rename(1, "Rename").unwrap();
        ed.undo();
        let before = ed.project().clone();
        let r = dispatch_editor(
            &mut ed,
            "find",
            &Json::obj(vec![("query", Json::Str("task".into()))]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.get_usize("count"), Some(2));
        assert_eq!(r.get_str("query"), Some("task"));
        assert_eq!(ed.sel(), 1);
        assert_eq!(ed.find_query(), "another");
        assert_eq!((ed.undo_depth(), ed.redo_depth()), (1, 1));
        assert!(ed.dirty());
        assert_eq!(ed.project(), &before);
    }

    #[test]
    fn invalid_control_arguments_preserve_redo_and_project() {
        let mut ed = Editor::new(new_project());
        ed.rename(1, "Change").unwrap();
        ed.undo();
        ed.mark_saved();
        let before = ed.project().clone();
        for args in [
            Json::obj(vec![("uid", Json::Num(1.0))]),
            Json::obj(vec![
                ("uid", Json::Num(1.0)),
                ("name", Json::Str("bad".into())),
                ("level", Json::Num(21.0)),
            ]),
            Json::obj(vec![
                ("uid", Json::Num(1.0)),
                ("name", Json::Str("bad".into())),
                ("duration", Json::Str("bad".into())),
            ]),
        ] {
            assert!(
                dispatch_editor(&mut ed, "task.set", &args)
                    .unwrap()
                    .is_err()
            );
            assert_eq!(ed.project(), &before);
            assert_eq!((ed.undo_depth(), ed.redo_depth()), (0, 1));
            assert!(!ed.dirty());
        }
    }
    #[test]
    fn task_set_switches_the_mode_in_one_undo_step() {
        let mut a = app();
        let uid = add(&mut a, "Design", "3d");
        let get =
            |a: &Editor| task_get(a, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap();
        assert_eq!(get(&a).get("manual"), Some(&Json::Bool(false)));
        let start = get(&a).get_str("start").unwrap().to_string();
        let depth = a.undo_depth();
        let r = dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("name", Json::Str("Pinned".into())),
                ("manual", Json::Bool(true)),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.get("manual"), Some(&Json::Bool(true)));
        assert_eq!(r.get_str("name"), Some("Pinned"));
        assert_eq!(r.get_str("start"), Some(start.as_str()));
        assert_eq!(a.undo_depth(), depth + 1, "rename and switch are one step");
        let task = a.project().task(uid as i32).unwrap();
        assert!(task.manual && task.manual_start.is_some());
        let r = dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("manual", Json::Bool(false)),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.get("manual"), Some(&Json::Bool(false)));
        assert_eq!(a.project().task(uid as i32).unwrap().manual_start, None);
        assert!(a.undo());
        assert!(a.project().task(uid as i32).unwrap().manual);
    }

    #[test]
    fn task_set_rejects_a_mode_that_is_not_a_bool() {
        let mut ed = app();
        ed.mark_saved();
        let before = ed.project().clone();
        for manual in [Json::Str("true".into()), Json::Num(1.0), Json::Null] {
            let args = Json::obj(vec![
                ("uid", Json::Num(1.0)),
                ("name", Json::Str("must not rename".into())),
                ("manual", manual),
            ]);
            let err = dispatch_editor(&mut ed, "task.set", &args)
                .unwrap()
                .unwrap_err();
            assert_eq!(err, "'manual' must be true or false");
            assert_eq!(ed.project(), &before);
            assert_eq!(ed.undo_depth(), 0);
            assert!(!ed.dirty());
        }
    }

    #[test]
    fn path_info_preserves_the_tui_wire_representation() {
        let ed = app();
        assert_eq!(
            path_info(Some("ctl-test.xml"), &ed).to_string(),
            r#"{"path":"ctl-test.xml","modified":false,"name":"Untitled","tasks":1,"start":"2026-01-05 08:00","finish":"2026-01-05 17:00"}"#
        );
        assert_eq!(path_info(None, &ed).get("path"), Some(&Json::Null));
    }
}
