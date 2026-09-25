//! The yppxy control surface: maps [`ctlcore`] verbs onto the **live** project,
//! so an external agent (e.g. Claude Code in a sibling agwinterm pane) can read
//! and edit the open schedule without touching the file on disk.
//!
//! Every mutating verb snapshots the project first (through [`projcore::editor::Editor`]), so an
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
//! | `task.del` | `{uid}` | `{deleted, removed:[uid…]}` (a summary takes its subtree) |
//! | `link.add` | `{uid, pred, type?, lag?}` | the updated task |
//! | `link.del` | `{uid, pred}` | the updated task |
//! | `find` | `{query}` | `{count, tasks:[…]}` |
//! | `proj.save` | `{path?}` (.yppx/.xml only; extensionless adds .yppx) | `{path, …}` with the actual saved path |
//! | `proj.reload` | — | `{path, …}` |
//! | `proj.open` | `{path}` | `{path, …}` |

use crate::App;
use ctlcore::json::Json;

/// Route one control verb against the live project, returning the JSON result
/// or an error message.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "proj.path" => Ok(path_info(app)),
        "proj.save" => {
            let p = args
                .get_str("path")
                .map(str::to_string)
                .or_else(|| app.path.clone())
                .ok_or("project has no file path yet — pass {\"path\": …}")?;
            app.save_to_path(&p)
                .map_err(|e| format!("save failed: {e}"))?;
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
        other => projctl::dispatch_editor(&mut app.ed, other, args)
            .unwrap_or_else(|| Err(format!("unknown verb '{other}'"))),
    };
    if out.is_ok()
        && (projctl::MUTATING.contains(&verb) || matches!(verb, "proj.open" | "proj.reload"))
    {
        app.drop_delete_confirm();
    }
    if out.is_ok() {
        // An agent edit flashes this pane's status dot, so a watcher sees the
        // plan being worked on.
        if projctl::MUTATING.contains(&verb) {
            ctlcore::signal_activity();
        }
    }
    out
}

fn path_info(app: &App) -> Json {
    projctl::path_info(app.path.as_deref(), &app.ed)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_project;
    fn app() -> App {
        App::new(new_project(), Some("ctl-test.xml".to_string()), false)
    }
    fn add(app: &mut App, name: &str, dur: &str) -> i64 {
        projctl::dispatch_editor(
            &mut app.ed,
            "task.add",
            &Json::obj(vec![
                ("name", Json::Str(name.into())),
                ("duration", Json::Str(dur.into())),
            ]),
        )
        .unwrap()
        .unwrap()
        .get("uid")
        .unwrap()
        .as_i64()
        .unwrap()
    }
    #[test]
    fn save_rebinds_only_after_success_and_reports_resolved_target() {
        let dir = std::env::temp_dir().join(format!("yppxy-control-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = app();
        a.ed.rename(1, "Unsaved change").unwrap();
        let before = dispatch(&mut a, "proj.path", &Json::Null).unwrap();
        for name in ["plan.mpp", "missing/plan.xml"] {
            let path = dir.join(name);
            assert!(
                dispatch(
                    &mut a,
                    "proj.save",
                    &Json::obj(vec![(
                        "path",
                        Json::Str(path.to_string_lossy().into_owned())
                    )])
                )
                .is_err()
            );
            assert_eq!(dispatch(&mut a, "proj.path", &Json::Null).unwrap(), before);
            assert!(!path.exists());
        }
        let path = dir.join("plan");
        let result = dispatch(
            &mut a,
            "proj.save",
            &Json::obj(vec![(
                "path",
                Json::Str(path.to_string_lossy().into_owned()),
            )]),
        )
        .unwrap();
        let actual = path.with_extension("yppx");
        assert_eq!(result.get_str("path"), actual.to_str());
        assert_eq!(a.path.as_deref(), actual.to_str());
        assert!(!a.ed.dirty());
        assert_eq!(
            crate::load(actual.to_str().unwrap()).unwrap().tasks[0].name,
            "Unsaved change"
        );
        assert!(!path.exists());
        // Subsequent pathless saves use the resolved, valid binding.
        a.ed.rename(1, "Next change").unwrap();
        dispatch(&mut a, "proj.save", &Json::Null).unwrap();
        assert_eq!(
            crate::load(actual.to_str().unwrap()).unwrap().tasks[0].name,
            "Next change"
        );
        std::fs::remove_file(actual).unwrap();
        std::fs::remove_dir(dir).unwrap();
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
    fn agent_edits_and_reloads_drop_a_pending_summary_delete() {
        let uid = |u: i32| Json::obj(vec![("uid", Json::Num(u as f64))]);
        for (verb, args) in [
            (
                "task.set",
                Json::obj(vec![("uid", Json::Num(2.0)), ("level", Json::Num(1.0))]),
            ),
            ("task.add", Json::obj(vec![("after", Json::Num(1.0))])),
            ("task.del", uid(2)),
            ("proj.reload", Json::Null),
            (
                "proj.open",
                Json::obj(vec![("path", Json::Str("missing.xml".into()))]),
            ),
        ] {
            let mut a = app();
            add(&mut a, "Child", "1d");
            a.ed.indent(2, 1).unwrap();
            a.ed.select(0);
            a.delete_task();
            assert!(a.confirm.is_some(), "{verb}");
            // Reads leave the question open.
            dispatch(&mut a, "task.list", &Json::Null).unwrap();
            assert!(a.confirm.is_some(), "{verb}");
            dispatch(&mut a, verb, &args).unwrap();
            assert!(a.confirm.is_none(), "{verb} must drop the stale question");
        }

        // An Exit question is not about the plan, so it stays.
        let mut a = app();
        a.request_exit();
        dispatch(&mut a, "task.del", &uid(1)).unwrap();
        assert!(a.confirm.is_some());
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
