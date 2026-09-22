//! The yppxy control surface: maps [`ctlcore`] verbs onto the **live** project,
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
//! | `task.list` | — | `{count, tasks:[{uid, name, level, duration, start, finish, critical, …}]}` |
//! | `task.get` | `{uid}` | one task |
//! | `task.set` | `{uid, name?, duration?, level?}` | the updated task |
//! | `task.add` | `{after?, name?, duration?}` | the new task |
//! | `task.del` | `{uid}` | `{deleted}` |
//! | `link.add` | `{uid, pred, type?, lag?}` | the updated task |
//! | `link.del` | `{uid, pred}` | the updated task |
//! | `find` | `{query}` | `{count, tasks:[…]}` |
//! | `proj.save` | `{path?}` | `{path, …}` |
//! | `proj.reload` | — | `{path, …}` |
//! | `proj.open` | `{path}` | `{path, …}` |

use crate::App;
use ctlcore::json::Json;
use projcore::datetime::DateTime;
use projcore::editor::{Editor, TaskPatch, parse_duration};
use projcore::model::{LinkType, Task};

/// Route one control verb against the live project, returning the JSON result
/// or an error message.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "proj.path" => Ok(path_info(app)),
        "proj.save" => {
            if let Some(p) = args.get_str("path") {
                app.path = Some(p.to_string());
            }
            let Some(p) = app.path.clone() else {
                return Err("project has no file path yet — pass {\"path\": …}".into());
            };
            crate::save_to(app.ed.project(), &p).map_err(|e| format!("save failed: {e}"))?;
            app.ed.mark_saved();
            app.status = format!("Saved {p}");
            Ok(path_info(app))
        }
        "proj.reload" => {
            let Some(p) = app.path.clone() else {
                return Err("project has no file path to reload".into());
            };
            app.open_file(&p);
            Ok(path_info(app))
        }
        "proj.open" => {
            let p = args
                .get_str("path")
                .ok_or("proj.open needs a 'path' string")?
                .to_string();
            app.open_file(&p);
            Ok(path_info(app))
        }
        other => dispatch_editor(&mut app.ed, other, args)
            .unwrap_or_else(|| Err(format!("unknown verb '{other}'"))),
    };
    if out.is_ok() {
        // An agent edit flashes this pane's status dot, so a watcher sees the
        // plan being worked on.
        if matches!(
            verb,
            "task.set" | "task.add" | "task.del" | "link.add" | "link.del"
        ) {
            ctlcore::signal_activity();
        }
    }
    out
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

fn path_info(app: &App) -> Json {
    Json::obj(vec![
        (
            "path",
            match &app.path {
                Some(p) => Json::Str(p.clone()),
                None => Json::Null,
            },
        ),
        ("modified", Json::Bool(app.ed.dirty())),
        ("name", Json::Str(app.ed.project().name.clone())),
        ("tasks", Json::Num(app.ed.project().tasks.len() as f64)),
        ("start", Json::Str(dt_str(app.ed.schedule().project_start))),
        (
            "finish",
            Json::Str(dt_str(app.ed.schedule().project_finish)),
        ),
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
                ("lag_min", Json::Num(p.lag_min as f64)),
            ])
        })
        .collect();
    let mut fields = vec![
        ("uid", Json::Num(t.uid as f64)),
        ("name", Json::Str(t.name.clone())),
        ("level", Json::Num(t.outline_level as f64)),
        ("summary", Json::Bool(t.summary)),
        ("milestone", Json::Bool(t.is_milestone())),
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
    ed.update_task(
        uid,
        TaskPatch {
            name: args.get_str("name").map(str::to_string),
            duration_min,
            level,
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
    ed.delete_task(uid)?;
    Ok(Json::obj(vec![("deleted", Json::Num(uid as f64))]))
}

fn link_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let pred = uid_arg(args, "pred")?;
    let link = match args.get_str("type") {
        Some(t) => parse_link_name(t).ok_or("'type' must be FS, SS, FF, or SF")?,
        None => LinkType::FinishStart,
    };
    let lag_min = match args.get_str("lag") {
        Some(l) => parse_duration(l, ed.project())
            .ok_or_else(|| format!("couldn't read lag '{l}' (try 1d, 4h)"))?,
        None => 0,
    };
    ed.add_predecessor(uid, pred, link, lag_min)?;
    task_get(ed, args)
}

fn link_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    ed.remove_predecessor(uid_arg(args, "uid")?, uid_arg(args, "pred")?)?;
    task_get(ed, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_project;

    fn app() -> App {
        App::new(new_project(), Some("ctl-test.xml".to_string()), false)
    }

    fn add(app: &mut App, name: &str, dur: &str) -> i64 {
        let r = task_add(
            &mut app.ed,
            &Json::obj(vec![
                ("name", Json::Str(name.into())),
                ("duration", Json::Str(dur.into())),
            ]),
        )
        .unwrap();
        r.get("uid").unwrap().as_i64().unwrap()
    }

    #[test]
    fn path_reports_project_shape() {
        let a = app();
        let r = path_info(&a);
        assert_eq!(r.get_str("path"), Some("ctl-test.xml"));
        assert_eq!(r.get("modified").unwrap().as_bool(), Some(false));
        assert!(r.get("start").is_some());
    }

    #[test]
    fn add_set_and_get_a_task() {
        let mut a = app();
        let uid = add(&mut a, "Design", "3d");
        assert!(a.ed.dirty());
        let g = task_get(&a.ed, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap();
        assert_eq!(g.get_str("name"), Some("Design"));
        assert_eq!(g.get("duration_days").unwrap().as_f64(), Some(3.0));
        assert!(g.get("start").is_some());
        assert!(g.get("finish").is_some());

        let r = task_set(
            &mut a.ed,
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
        let before = task_get(&a.ed, &Json::obj(vec![("uid", Json::Num(t2 as f64))]))
            .unwrap()
            .get_str("start")
            .unwrap()
            .to_string();
        link_add(
            &mut a.ed,
            &Json::obj(vec![
                ("uid", Json::Num(t2 as f64)),
                ("pred", Json::Num(t1 as f64)),
            ]),
        )
        .unwrap();
        let after = task_get(&a.ed, &Json::obj(vec![("uid", Json::Num(t2 as f64))])).unwrap();
        let preds = after.get("predecessors").unwrap().as_array().unwrap();
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].get_str("type"), Some("FS"));
        // The dependent task now starts after its 2-day predecessor.
        assert_ne!(after.get_str("start").unwrap(), before);

        // And the link can be removed again.
        let r = link_del(
            &mut a.ed,
            &Json::obj(vec![
                ("uid", Json::Num(t2 as f64)),
                ("pred", Json::Num(t1 as f64)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("predecessors").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn agent_edits_share_the_undo_stack() {
        let mut a = app();
        let n0 = a.ed.project().tasks.len();
        add(&mut a, "Extra", "1d");
        assert_eq!(a.ed.project().tasks.len(), n0 + 1);
        a.undo();
        assert_eq!(a.ed.project().tasks.len(), n0);
        a.redo();
        assert_eq!(a.ed.project().tasks.len(), n0 + 1);
    }

    #[test]
    fn delete_drops_dangling_links() {
        let mut a = app();
        let t1 = add(&mut a, "A", "1d");
        let t2 = add(&mut a, "B", "1d");
        link_add(
            &mut a.ed,
            &Json::obj(vec![
                ("uid", Json::Num(t2 as f64)),
                ("pred", Json::Num(t1 as f64)),
            ]),
        )
        .unwrap();
        task_del(&mut a.ed, &Json::obj(vec![("uid", Json::Num(t1 as f64))])).unwrap();
        let g = task_get(&a.ed, &Json::obj(vec![("uid", Json::Num(t2 as f64))])).unwrap();
        assert_eq!(g.get("predecessors").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn find_matches_by_name() {
        let mut a = app();
        add(&mut a, "Write spec", "1d");
        add(&mut a, "Review spec", "1d");
        add(&mut a, "Ship", "1d");
        let r = find(&a.ed, &Json::obj(vec![("query", Json::Str("spec".into()))])).unwrap();
        assert_eq!(r.get_usize("count"), Some(2));
    }

    #[test]
    fn bad_args_change_nothing() {
        let mut a = app();
        let uid = add(&mut a, "T", "1d");
        let dirty_before = a.ed.dirty();
        let undo_before = a.ed.undo_depth();
        assert!(
            task_set(
                &mut a.ed,
                &Json::obj(vec![
                    ("uid", Json::Num(uid as f64)),
                    ("duration", Json::Str("banana".into())),
                ]),
            )
            .is_err()
        );
        assert!(task_get(&a.ed, &Json::obj(vec![("uid", Json::Num(999.0))])).is_err());
        assert_eq!(a.ed.dirty(), dirty_before);
        assert_eq!(
            a.ed.undo_depth(),
            undo_before,
            "failed edits push no snapshot"
        );
    }

    #[test]
    fn dispatch_routes_and_reports_unknown() {
        let mut a = app();
        assert!(dispatch(&mut a, "proj.path", &Json::Null).is_ok());
        assert!(dispatch(&mut a, "task.list", &Json::Null).is_ok());
        let err = dispatch(&mut a, "proj.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }

    #[test]
    fn append_without_after_inherits_last_level_and_keeps_selection() {
        let mut a = app();
        a.ed.indent(1, 2).unwrap();
        let r = dispatch(&mut a, "task.add", &Json::Null).unwrap();
        assert_eq!(r.get_usize("level"), Some(3));
        assert_eq!(a.ed.sel(), 0);
        assert_eq!(a.ed.undo_depth(), 2);
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
        assert_eq!(pred.lag_min, 240);
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
}
