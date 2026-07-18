//! The yppxy control surface: maps [`ctlcore`] verbs onto the **live** project,
//! so an external agent (e.g. Claude Code in a sibling agwinterm pane) can read
//! and edit the open schedule without touching the file on disk.
//!
//! Every mutating verb snapshots the project first ([`App::snapshot`]), so an
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

use crate::{App, parse_duration};
use ctlcore::json::Json;
use projcore::datetime::DateTime;
use projcore::model::{LinkType, Predecessor, Task};

/// Route one control verb against the live project, returning the JSON result
/// or an error message.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "proj.path" => Ok(path_info(app)),
        "task.list" => Ok(task_list(app)),
        "task.get" => task_get(app, args),
        "task.set" => task_set(app, args),
        "task.add" => task_add(app, args),
        "task.del" => task_del(app, args),
        "link.add" => link_add(app, args),
        "link.del" => link_del(app, args),
        "find" => find(app, args),
        "proj.save" => {
            if let Some(p) = args.get_str("path") {
                app.path = Some(p.to_string());
            }
            let Some(p) = app.path.clone() else {
                return Err("project has no file path yet — pass {\"path\": …}".into());
            };
            crate::save_to(&app.proj, &p).map_err(|e| format!("save failed: {e}"))?;
            app.dirty = false;
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
        other => Err(format!("unknown verb '{other}'")),
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
        ("modified", Json::Bool(app.dirty)),
        ("name", Json::Str(app.proj.name.clone())),
        ("tasks", Json::Num(app.proj.tasks.len() as f64)),
        ("start", Json::Str(dt_str(app.sched.project_start))),
        ("finish", Json::Str(dt_str(app.sched.project_finish))),
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
fn task_json(app: &App, t: &Task) -> Json {
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
            Json::Num(app.proj.minutes_to_days(t.duration_min)),
        ),
        ("predecessors", Json::Arr(preds)),
    ];
    if let Some(s) = app.disp_start(t.uid) {
        fields.push(("start", Json::Str(dt_str(s))));
    }
    if let Some(f) = app.disp_finish(t.uid) {
        fields.push(("finish", Json::Str(dt_str(f))));
    }
    if let Some(r) = app.sched.get(t.uid) {
        fields.push(("critical", Json::Bool(r.critical)));
        fields.push((
            "slack_days",
            Json::Num(app.proj.minutes_to_days(r.total_slack_min)),
        ));
    }
    Json::obj(fields)
}

fn task_list(app: &App) -> Json {
    let tasks = app.proj.tasks.iter().map(|t| task_json(app, t)).collect();
    Json::obj(vec![
        ("count", Json::Num(app.proj.tasks.len() as f64)),
        ("tasks", Json::Arr(tasks)),
    ])
}

fn find(app: &App, args: &Json) -> Result<Json, String> {
    let query = args.get_str("query").ok_or("find needs a 'query'")?;
    let needle = query.to_lowercase();
    let tasks: Vec<Json> = app
        .proj
        .tasks
        .iter()
        .filter(|t| t.name.to_lowercase().contains(&needle))
        .map(|t| task_json(app, t))
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

fn task_index(app: &App, uid: i32) -> Result<usize, String> {
    app.proj
        .tasks
        .iter()
        .position(|t| t.uid == uid)
        .ok_or_else(|| format!("no task with uid {uid}"))
}

fn task_get(app: &App, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let i = task_index(app, uid)?;
    Ok(task_json(app, &app.proj.tasks[i]))
}

fn task_set(app: &mut App, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let i = task_index(app, uid)?;
    // Validate everything before the snapshot so a bad arg changes nothing.
    let duration = match args.get_str("duration") {
        Some(d) => Some(
            parse_duration(d, &app.proj)
                .ok_or_else(|| format!("couldn't read duration '{d}' (try 3d, 4h, 2w)"))?,
        ),
        None => None,
    };
    let level = match args.get("level") {
        Some(l) => Some(
            l.as_i64()
                .filter(|n| (1..=20).contains(n))
                .ok_or("'level' must be 1..=20")? as u32,
        ),
        None => None,
    };
    let name = args.get_str("name").map(str::to_string);
    if name.is_none() && duration.is_none() && level.is_none() {
        return Err("task.set needs at least one of 'name', 'duration', 'level'".into());
    }
    app.snapshot();
    {
        let t = &mut app.proj.tasks[i];
        if let Some(n) = name {
            t.name = n;
        }
        if let Some(min) = duration {
            t.duration_min = min;
            t.milestone = min == 0;
        }
        if let Some(lv) = level {
            t.outline_level = lv;
        }
    }
    app.mark_dirty();
    app.reschedule();
    Ok(task_json(app, &app.proj.tasks[i]))
}

fn task_add(app: &mut App, args: &Json) -> Result<Json, String> {
    // Insert after the task with uid `after`, or append at the end.
    let at = match args.get("after") {
        Some(_) => task_index(app, uid_arg(args, "after")?)? + 1,
        None => app.proj.tasks.len(),
    };
    let duration_min = match args.get_str("duration") {
        Some(d) => parse_duration(d, &app.proj)
            .ok_or_else(|| format!("couldn't read duration '{d}' (try 3d, 4h, 2w)"))?,
        None => 480,
    };
    let name = args.get_str("name").unwrap_or("New task").to_string();
    let level = at
        .checked_sub(1)
        .and_then(|p| app.proj.tasks.get(p))
        .map(|t| t.outline_level)
        .unwrap_or(1);
    app.snapshot();
    let uid = app.proj.tasks.iter().map(|t| t.uid).max().unwrap_or(0) + 1;
    app.proj.tasks.insert(
        at,
        Task {
            uid,
            id: uid,
            name,
            outline_level: level,
            duration_min,
            milestone: duration_min == 0,
            ..Task::default()
        },
    );
    app.mark_dirty();
    app.reschedule();
    Ok(task_json(app, &app.proj.tasks[at]))
}

fn task_del(app: &mut App, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let i = task_index(app, uid)?;
    app.snapshot();
    app.proj.tasks.remove(i);
    // Drop dangling predecessor links to the removed task.
    for t in &mut app.proj.tasks {
        t.predecessors.retain(|p| p.uid != uid);
    }
    app.sel = app.sel.min(app.proj.tasks.len().saturating_sub(1));
    app.mark_dirty();
    app.reschedule();
    Ok(Json::obj(vec![("deleted", Json::Num(uid as f64))]))
}

fn link_add(app: &mut App, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let pred = uid_arg(args, "pred")?;
    let i = task_index(app, uid)?;
    task_index(app, pred)?; // the predecessor must exist
    if uid == pred {
        return Err("a task cannot depend on itself".into());
    }
    if app.proj.tasks[i].predecessors.iter().any(|p| p.uid == pred) {
        return Err(format!("task {uid} already depends on {pred}"));
    }
    let link = match args.get_str("type") {
        Some(t) => parse_link_name(t).ok_or("'type' must be FS, SS, FF, or SF")?,
        None => LinkType::FinishStart,
    };
    let lag_min = match args.get_str("lag") {
        Some(l) => parse_duration(l, &app.proj)
            .ok_or_else(|| format!("couldn't read lag '{l}' (try 1d, 4h)"))?,
        None => 0,
    };
    app.snapshot();
    app.proj.tasks[i].predecessors.push(Predecessor {
        uid: pred,
        link,
        lag_min,
    });
    app.mark_dirty();
    app.reschedule();
    Ok(task_json(app, &app.proj.tasks[i]))
}

fn link_del(app: &mut App, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let pred = uid_arg(args, "pred")?;
    let i = task_index(app, uid)?;
    if !app.proj.tasks[i].predecessors.iter().any(|p| p.uid == pred) {
        return Err(format!("task {uid} has no predecessor {pred}"));
    }
    app.snapshot();
    app.proj.tasks[i].predecessors.retain(|p| p.uid != pred);
    app.mark_dirty();
    app.reschedule();
    Ok(task_json(app, &app.proj.tasks[i]))
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
            app,
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
        assert!(a.dirty);
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
    fn agent_edits_share_the_undo_stack() {
        let mut a = app();
        let n0 = a.proj.tasks.len();
        add(&mut a, "Extra", "1d");
        assert_eq!(a.proj.tasks.len(), n0 + 1);
        a.undo();
        assert_eq!(a.proj.tasks.len(), n0);
        a.redo();
        assert_eq!(a.proj.tasks.len(), n0 + 1);
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
        let dirty_before = a.dirty;
        let undo_before = a.undo.len();
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
        assert_eq!(a.dirty, dirty_before);
        assert_eq!(a.undo.len(), undo_before, "failed edits push no snapshot");
    }

    #[test]
    fn dispatch_routes_and_reports_unknown() {
        let mut a = app();
        assert!(dispatch(&mut a, "proj.path", &Json::Null).is_ok());
        assert!(dispatch(&mut a, "task.list", &Json::Null).is_ok());
        let err = dispatch(&mut a, "proj.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }
}
