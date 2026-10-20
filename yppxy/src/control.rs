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
//! | `app-info` | — | the build: `{version, commit, short_commit, branch, commit_date, dirty, last_pr, issue, ahead, built_at, profile, target, host, kind, manual, summary, …}` |
//! | `task.list` | `{fields?}` | `{count, tasks:[{uid, id, outline_number, name, level, manual, duration, start, finish, critical, …}]}` |
//! | `task.get` | `{uid, fields?}` | one task |
//! | `task.fields` | — | `{count, fields:[name…]}`: the field names `fields` can read |
//! | `task.set` | `{uid, name?, duration?, level?, manual?, fields?, the Task Information fields of #1550: percent_complete?, actual_start?, actual_finish?, actual_duration?, remaining_duration?, priority?, deadline?, constraint?, task_type?, effort_driven?, notes?, milestone?, calendar?, ignore_resource_calendar?, wbs?, earned_value_method?, and custom? (#1554: `{"Text1": "Civil", ...}`)}` | the updated task (`manual`: `true` Manually / `false` Auto Scheduled); see `projctl` for each field's form |
//! | `task.add` | `{after?, name?, duration?, fields?}` | the new task (without `duration`, 1 day, estimated unless the plan's `NewTasksEstimated` is off; inserted `after` a task that a finish-to-start link joins to the next, it is linked into that chain, A->B becoming A->N->B, while the plan's `Autolink` is on, the default) |
//! | `task.del` | `{uid}` | `{deleted, removed:[uid…]}` (a summary takes its subtree) |
//! | `link.add` | `{uid, pred, type?, lag?, fields?}` | the updated task |
//! | `link.del` | `{uid, pred, fields?}` | the updated task |
//! | `find` | `{query, fields?}` | `{count, tasks:[…]}` |
//! | `assign.list` | `{uid?, resource?, fields?}` | `{count, assignments:[…]}` (of task `uid` and/or `resource`, a uid or name) |
//! | `assign.get` | `{uid, fields?}` | one assignment |
//! | `assign.fields` | — | `{count, fields:[name…]}`: the assignment field names |
//! | `assign.add` | `{task, resource, units?, work?, fields?}` | the new assignment |
//! | `assign.set` | `{uid, units?, work?, overtime_work?, contour?, rate_table?, delay?, actual_work? \| percent_work_complete?, fields?}` | the updated assignment |
//! | `assign.del` | `{uid}` | `{deleted, task}` |
//! | `level.all` / `level.clear` | `{}` | `{leveled, overallocated}`: show the plan leveled or as scheduled (the display overlay, not an undo step, #1589); see `projctl` |
//! | `level.overallocations` | `{}` | `{count, resources:[{uid, name, max_units, peak_units, first}]}`: the work resources booked past their capacity as shown (#1589) |
//! | `cal.list` / `cal.get` / `cal.day` | `{}` / `{calendar}` / `{calendar, date}` | the plan's calendars, one with its week, exceptions and work weeks, a day's working times and their source (#1605); see `projctl` |
//! | `cal.add` / `cal.set` / `cal.del` | `{name, base?, copy?}` / `{calendar, name?, base?, week?}` / `{calendar}` | a calendar added, changed or deleted, one undo step (#1605) |
//! | `cal.exception.set` / `cal.exception.del` / `cal.workweek.set` / `cal.workweek.del` | `{calendar, index?, name?, from, to, working?, times?}` / `{calendar, index}` / `{calendar, index?, name?, from, to, week}` / `{calendar, index}` | an exception or work week added, changed or removed, one undo step (#1605) |
//! | `resource.list` | `{}` | `{count, resources:[{uid, id, name, type, initials, group, code, email, material_label, max_units, standard_rate, overtime_rate, cost_per_use, accrue_at, base_calendar, notes, assignments}]}` (#1588); see `projctl` |
//! | `resource.get` | `{uid}` | one resource |
//! | `resource.add` | `{name, type?, …the resource fields}` | the new resource, one undo step (#1588) |
//! | `resource.set` | `{uid, …the resource fields}` | the updated resource, one undo step (#1588) |
//! | `resource.del` | `{uid}` | `{deleted, assignments:[uid…]}`: the resource with its assignments, one undo step (#1588) |
//! | `proj.get` | `{}` | the plan's title, start, finish, status and current dates, calendar (#1587) and `new_tasks` mode (#1585); see `projctl` |
//! | `proj.set` | `{title?, start?, status_date?, current_date?, calendar?, new_tasks?}` | the same after the change, one undo step (#1587, #1585); see `projctl` |
//! | `proj.view` | — | the grid's order (#1533): `{sort, sort_descending, filter, filters:[{column, values}], count, rows:[uid…]}`, the rows in the order the table draws them |
//! | `proj.sort` | `{column?, descending?}` (no `column` clears) | the same as `proj.view`; view state, not an edit |
//! | `proj.filter` | `{column, values?}` or `{clear:true}` | the same as `proj.view`; `values` are the cell texts (one may contain `|`), `(Blanks)`, or on a date column `YYYY`, `YYYY-MM`, `YYYY-MM-DD`; no `values` clears that column |
//! | `proj.save` | `{path?}` (.yppx/.xml, or a type a format plugin writes; extensionless adds .yppx) | `{path, …}` with the actual saved path |
//! | `proj.reload` | — | `{path, …}` |
//! | `proj.open` | `{path}` | `{path, …}` |
//! | `field-read` | `{input?}` | a text prompt (`yppxy-prompt`, `yppxy-save-as`): `{input, value, focused, caret, selection, at, kind, keys, clicks, frames, dirty}`; with no `input`, the counters and the ids (`ctlcore::field`, #1204) |
//!
//! `fields` reads Project fields by name as `{text, value}`; it is checked
//! before an edit, so a bad list changes nothing. See `projctl`, which also
//! describes an assignment's JSON.

use crate::App;
use ctlcore::json::Json;

/// `app-info`: the build this binary is (see the `buildinfo` crate), as
/// JSON, with `plugins` (the libraries loaded and skipped, #1532).
fn app_info() -> Result<Json, String> {
    let mut info = Json::parse(&buildinfo::get(env!("CARGO_PKG_VERSION")).json())?;
    let formats = crate::formats::global();
    if let Json::Obj(fields) = &mut info {
        fields.push((
            "plugins".into(),
            crate::formats::plugins_json(&formats.plugins, &formats.ui, &formats.skipped),
        ));
    }
    Ok(info)
}

/// yppxy's text prompts, as `field-read {"input":id}` names them (#1204):
/// the prompt every command shares (its vim `:` line and Ctrl+S Save As
/// included) and Backstage Save As's file name. The suite's input registry
/// lists the same ids.
pub(crate) const INPUTS: &[&str] = &["yppxy-prompt", "yppxy-save-as"];

/// Route one control verb against the live project, returning the JSON result
/// or an error message.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "proj.path" => Ok(path_info(app)),
        "app-info" => app_info(),
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
        "proj.view" => Ok(view_json(app)),
        "proj.sort" => {
            let text = match args.get_str("column") {
                Some(column) if args.get("descending").and_then(Json::as_bool) == Some(true) => {
                    format!("{column} desc")
                }
                Some(column) => column.to_string(),
                None => String::new(),
            };
            app.apply_sort(&text)?;
            Ok(view_json(app))
        }
        "proj.filter" => {
            if args.get("clear").and_then(Json::as_bool) == Some(true) {
                app.apply_filter("")?;
            } else {
                let field = crate::entry_field(
                    args.get_str("column")
                        .ok_or("proj.filter needs a 'column' (or {\"clear\": true})")?,
                )?;
                let values: Vec<String> = match args.get("values") {
                    None => Vec::new(),
                    Some(v) => v
                        .as_array()
                        .filter(|a| !a.is_empty())
                        .ok_or("'values' must be a non-empty array of texts (omit it to clear)")?
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_string)
                                .ok_or("'values' must be an array of texts")
                        })
                        .collect::<Result<_, _>>()?,
                };
                app.filter_values(field, &values)?;
            }
            Ok(view_json(app))
        }
        "field-read" => ctlcore::field::reply(args, INPUTS, app.counts, app.ed.dirty(), |id| {
            app.input_field(id)
        }),
        other => projctl::dispatch_editor(&mut app.ed, other, args)
            .unwrap_or_else(|| Err(format!("unknown verb '{other}'"))),
    };
    if out.is_ok()
        && (projctl::MUTATING.contains(&verb) || matches!(verb, "proj.open" | "proj.reload"))
    {
        app.refresh_confirm();
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

/// The grid's sort, filters and the rows as drawn (#1533).
fn view_json(app: &App) -> Json {
    let ed = &app.ed;
    let tasks = &ed.project().tasks;
    let rows: Vec<Json> = ed
        .display_rows()
        .into_iter()
        .map(|i| Json::Num(f64::from(tasks[i].uid)))
        .collect();
    let mut fields = projctl::view_fields(ed);
    fields.extend([
        ("count", Json::Num(rows.len() as f64)),
        ("rows", Json::Arr(rows)),
    ]);
    Json::obj(fields)
}

fn path_info(app: &App) -> Json {
    projctl::path_info(app.path.as_deref(), &app.ed)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Act, PromptKind, new_project, on_key, view_note};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    fn app() -> App {
        App::new(new_project(), Some("ctl-test.xml".to_string()), false)
    }
    #[test]
    fn app_info_reports_the_build() {
        let mut app = app();
        let info = dispatch(&mut app, "app-info", &Json::Null).unwrap();
        for k in [
            "version",
            "commit",
            "short_commit",
            "branch",
            "commit_date",
            "dirty",
            "last_pr",
            "issue",
            "ahead",
            "built_at",
            "profile",
            "target",
            "host",
            "kind",
            "manual",
            "summary",
            "plugins",
        ] {
            assert!(info.get(k).is_some(), "app-info lacks {k}: {info:?}");
        }
        // No plugin loads in a unit test: both lists are empty.
        let plugins = info.get("plugins").unwrap();
        assert_eq!(
            plugins
                .get("loaded")
                .and_then(Json::as_array)
                .map(<[Json]>::len),
            Some(0)
        );
        assert_eq!(
            plugins
                .get("skipped")
                .and_then(Json::as_array)
                .map(<[Json]>::len),
            Some(0)
        );
        assert_eq!(info.get_str("version"), Some(env!("CARGO_PKG_VERSION")));
        let kind = info.get_str("kind").unwrap();
        assert!(["release", "ci", "local"].contains(&kind), "{kind}");
        // manual == (kind is local || dirty)
        let dirty = info.get("dirty").and_then(Json::as_bool).unwrap();
        assert_eq!(
            info.get("manual").and_then(Json::as_bool),
            Some(kind == "local" || dirty)
        );
        let commit = info.get_str("commit").unwrap();
        assert!(
            commit == "unknown"
                || (commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()))
        );
        assert!(
            info.get_str("summary")
                .unwrap()
                .contains(env!("CARGO_PKG_VERSION"))
        );
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

    /// A save clears undo and redo, as in Project; a failed one keeps both
    /// (#863).
    #[test]
    fn save_clears_undo_and_redo_only_on_success() {
        let dir =
            std::env::temp_dir().join(format!("yppxy-control-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let save = |a: &mut App, name: &str| {
            let path = dir.join(name).to_string_lossy().into_owned();
            dispatch(a, "proj.save", &Json::obj(vec![("path", Json::Str(path))]))
        };
        let mut a = app();
        a.ed.rename(1, "A").unwrap();
        a.ed.rename(1, "B").unwrap();
        a.undo();
        assert_eq!((a.ed.undo_depth(), a.ed.redo_depth()), (1, 1));
        assert!(save(&mut a, "plan.mpp").is_err());
        assert_eq!((a.ed.undo_depth(), a.ed.redo_depth()), (1, 1));
        save(&mut a, "saved.xml").unwrap();
        assert_eq!((a.ed.undo_depth(), a.ed.redo_depth()), (0, 0));
        a.undo();
        a.redo();
        assert_eq!(a.ed.project().tasks[0].name, "A");
        std::fs::remove_dir_all(dir).unwrap();
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

        // An Exit question stays, its warning follows the now-dirty plan,
        // and the user's No survives the update.
        let mut a = app();
        a.request_exit();
        assert!(!a.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        a.confirm_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!a.confirm.as_ref().unwrap().yes_selected());
        dispatch(&mut a, "task.del", &uid(1)).unwrap();
        let exit = a.confirm.as_ref().expect("the Exit question stays open");
        assert_eq!(exit.action(), &crate::ConfirmAction::Exit);
        assert!(exit.prompt().contains("Unsaved"));
        assert!(!exit.yes_selected(), "No must stay selected");
        // A second edit leaves the (unchanged) question alone.
        dispatch(&mut a, "task.add", &Json::Null).unwrap();
        assert!(!a.confirm.as_ref().unwrap().yes_selected());
        a.confirm_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!a.quit, "Enter on No must not quit");
        assert!(a.confirm.is_none());
    }

    #[test]
    fn dispatch_routes_and_reports_unknown() {
        let mut a = app();
        assert!(dispatch(&mut a, "proj.path", &Json::Null).is_ok());
        assert!(dispatch(&mut a, "task.list", &Json::Null).is_ok());
        let err = dispatch(&mut a, "proj.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }

    /// `field-read` for each prompt, closed and then open and drawn, typed
    /// into with the keys a person presses (#1204).
    #[test]
    fn field_read_reports_each_prompt_closed_and_open() {
        let read = |app: &mut App, id: &str| {
            dispatch(
                app,
                "field-read",
                &Json::obj(vec![("input", Json::Str(id.into()))]),
            )
            .unwrap()
        };
        let draw = |app: &mut App| {
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
            term.draw(|f| crate::draw(f, app)).unwrap();
        };
        let mut app = app();
        draw(&mut app);
        for id in INPUTS {
            let r = read(&mut app, id);
            assert_eq!(r.get("value"), Some(&Json::Null), "{id}: {r}");
            assert_eq!(r.get("at"), Some(&Json::Null), "{id}: {r}");
        }
        assert!(
            dispatch(
                &mut app,
                "field-read",
                &Json::obj(vec![("input", Json::Str("x".into()))])
            )
            .unwrap_err()
            .contains("yppxy-save-as")
        );
        // F2 renames the selected task, prefilled with its name.
        crate::on_key(&mut app, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        crate::on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        draw(&mut app);
        let r = read(&mut app, "yppxy-prompt");
        assert_eq!(r.get_str("value"), Some("New task!"), "{r}");
        assert_eq!(r.get_usize("caret"), Some(9));
        assert_eq!(r.get_str("kind"), Some("Rename"));
        assert_eq!(r.get("focused").and_then(Json::as_bool), Some(true));
        let at = r.get("at").unwrap();
        assert_eq!(
            at.get_usize("row"),
            Some(21),
            "the prompt box's middle row: {r}"
        );
        assert_eq!(at.get_usize("col"), Some(30 + 1 + " Rename: ".len()), "{r}");
        let all = dispatch(&mut app, "field-read", &Json::obj(vec![])).unwrap();
        assert_eq!(all.get_usize("frames"), Some(2), "{all}");
    }

    /// Plan, Build, Test, Ship finish on 1/12, 3/2, 3/17 and 4/6 (the suite's
    /// `project-filter.xml`).
    fn dated() -> App {
        let task = |uid: u32, name: &str, days: u32, date: &str| {
            format!(
                "<Task><UID>{uid}</UID><ID>{uid}</ID><Name>{name}</Name><OutlineLevel>1</OutlineLevel>\
                 <Duration>PT{}H0M0S</Duration><ConstraintType>4</ConstraintType>\
                 <ConstraintDate>{date}T08:00:00</ConstraintDate></Task>",
                days * 8
            )
        };
        let xml = format!(
            "<?xml version=\"1.0\"?><Project xmlns=\"http://schemas.microsoft.com/project\">\
             <Name>d</Name><StartDate>2026-01-05T08:00:00</StartDate>\
             <MinutesPerDay>480</MinutesPerDay><MinutesPerWeek>2400</MinutesPerWeek><Tasks>{}{}{}{}</Tasks></Project>",
            task(1, "Plan", 1, "2026-01-12"),
            task(2, "Build", 1, "2026-03-02"),
            task(3, "Test", 2, "2026-03-16"),
            task(4, "Ship", 1, "2026-04-06"),
        );
        App::new(
            projcore::mspdi::read_mspdi(&xml).unwrap(),
            Some("dated.xml".to_string()),
            false,
        )
    }

    fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    fn uids(view: &Json) -> Vec<i64> {
        view.get("rows")
            .and_then(Json::as_array)
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect()
    }

    #[test]
    fn sort_reorders_the_rows_and_clears() {
        let mut a = dated();
        let v = dispatch(&mut a, "proj.view", &Json::Null).unwrap();
        assert_eq!(uids(&v), [1, 2, 3, 4]);
        assert_eq!(v.get_str("sort"), Some("ID"));
        let v = dispatch(
            &mut a,
            "proj.sort",
            &obj(vec![
                ("column", Json::Str("Finish".into())),
                ("descending", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert_eq!(uids(&v), [4, 3, 2, 1]);
        assert_eq!(v.get_str("sort"), Some("Finish"));
        assert_eq!(v.get("sort_descending").and_then(Json::as_bool), Some(true));
        let v = dispatch(
            &mut a,
            "proj.sort",
            &obj(vec![("column", Json::Str("name".into()))]),
        )
        .unwrap();
        assert_eq!(uids(&v), [2, 1, 4, 3], "Build, Plan, Ship, Test");
        let v = dispatch(&mut a, "proj.sort", &Json::Null).unwrap();
        assert_eq!(uids(&v), [1, 2, 3, 4]);
        // Sorting is view state: the plan is not dirty.
        assert!(!a.ed.dirty());
    }

    #[test]
    fn filtering_finish_to_a_month_and_anding_a_second_column() {
        let mut a = dated();
        let filter =
            |a: &mut App, pairs: Vec<(&str, Json)>| dispatch(a, "proj.filter", &obj(pairs));
        let v = filter(
            &mut a,
            vec![
                ("column", Json::Str("Finish".into())),
                ("values", Json::Arr(vec![Json::Str("2026-03".into())])),
            ],
        )
        .unwrap();
        assert_eq!(uids(&v), [2, 3]);
        assert_eq!(v.get_str("filter"), Some("AutoFilter"));
        let v = filter(
            &mut a,
            vec![
                ("column", Json::Str("Name".into())),
                ("values", Json::Arr(vec![Json::Str("test".into())])),
            ],
        )
        .unwrap();
        assert_eq!(uids(&v), [3]);
        assert_eq!(
            v.get("filters").and_then(Json::as_array).map(<[Json]>::len),
            Some(2)
        );
        // Sorted and filtered compose.
        dispatch(
            &mut a,
            "proj.sort",
            &obj(vec![("column", Json::Str("Name".into()))]),
        )
        .unwrap();
        // A column alone clears its filter.
        let v = filter(&mut a, vec![("column", Json::Str("Name".into()))]).unwrap();
        assert_eq!(uids(&v), [2, 3]);
        let v = filter(&mut a, vec![("clear", Json::Bool(true))]).unwrap();
        assert_eq!(uids(&v), [2, 1, 4, 3]);
        assert_eq!(v.get_str("filter"), Some("All Tasks"));
        // Every value is no filter.
        let v = filter(
            &mut a,
            vec![
                ("column", Json::Str("Finish".into())),
                ("values", Json::Arr(vec![Json::Str("2026".into())])),
            ],
        )
        .unwrap();
        assert_eq!(v.get_str("filter"), Some("All Tasks"));
    }

    #[test]
    fn a_bad_column_or_value_is_refused_and_changes_nothing() {
        let mut a = dated();
        let err = dispatch(
            &mut a,
            "proj.sort",
            &obj(vec![("column", Json::Str("Nope".into()))]),
        )
        .unwrap_err();
        assert!(
            err.starts_with("No column 'Nope' (ID, Task Mode, Name"),
            "{err}"
        );
        let err = dispatch(
            &mut a,
            "proj.filter",
            &obj(vec![
                ("column", Json::Str("Finish".into())),
                ("values", Json::Arr(vec![Json::Str("2026-05".into())])),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "No value '2026-05' in Finish");
        assert!(!a.ed.is_filtered());
        assert!(dispatch(&mut a, "proj.filter", &Json::Null).is_err());
    }

    #[test]
    fn the_prompts_and_vim_commands_do_the_same() {
        let mut a = dated();
        a.apply_act(Act::Sort);
        let p = a.prompt.as_mut().unwrap();
        assert!(matches!(p.kind, PromptKind::Sort));
        p.buf = "Finish desc".into();
        on_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.status, "Sorted by Finish desc");
        assert_eq!(view_note(&a.ed), " · ↓ Finish");
        a.apply_act(Act::Filter);
        let p = a.prompt.as_mut().unwrap();
        assert!(matches!(p.kind, PromptKind::Filter));
        p.buf = "Finish: 2026-03".into();
        on_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.status, "AutoFilter Applied");
        assert_eq!(view_note(&a.ed), " · ↓ Finish · ⧩ AutoFilter Applied");
        let rows: Vec<_> = a.ed.display_rows();
        assert_eq!(rows, [2, 1], "Test then Build, the March rows latest first");
        // The sort refuses position-dependent edits until it is cleared.
        a.ed.select(2);
        a.indent(1);
        assert_eq!(a.status, "Clear the sort first");
        a.vim_run("sort");
        assert_eq!(a.status, "Sort cleared");
        a.vim_run("filter");
        assert_eq!(a.status, "Filter cleared");
        assert_eq!(a.ed.display_rows(), [0, 1, 2, 3]);
        a.vim_run("sort Task desc");
        assert_eq!(
            a.ed.sort().map(|s| (s.field.name(), s.descending)),
            Some(("Name".into(), true))
        );
    }

    #[test]
    fn filter_values_may_hold_a_bar_and_an_empty_text_is_a_blank_cell() {
        let mut a = dated();
        a.ed.rename(2, "Build | Test").unwrap();
        let filter = |a: &mut App, column: &str, values: &[&str]| {
            dispatch(
                a,
                "proj.filter",
                &obj(vec![
                    ("column", Json::Str(column.into())),
                    (
                        "values",
                        Json::Arr(values.iter().map(|v| Json::Str((*v).into())).collect()),
                    ),
                ]),
            )
        };
        let v = filter(&mut a, "Name", &["Build | Test"]).unwrap();
        assert_eq!(uids(&v), [2]);
        // No task has an empty Name: asking for it is refused, not a clear.
        let err = filter(&mut a, "Name", &[""]).unwrap_err();
        assert_eq!(err, "No value '' in Name");
        assert_eq!(
            uids(&dispatch(&mut a, "proj.view", &Json::Null).unwrap()),
            [2]
        );
        // Every task has no resource: (Blanks) is every value, so no filter.
        dispatch(
            &mut a,
            "proj.filter",
            &obj(vec![("clear", Json::Bool(true))]),
        )
        .unwrap();
        let v = filter(&mut a, "Resource Names", &["(Blanks)"]).unwrap();
        assert_eq!(v.get_str("filter"), Some("All Tasks"));
    }

    #[test]
    fn an_empty_values_array_is_refused_not_a_clear() {
        let mut a = dated();
        dispatch(
            &mut a,
            "proj.filter",
            &obj(vec![
                ("column", Json::Str("Name".into())),
                ("values", Json::Arr(vec![Json::Str("Plan".into())])),
            ]),
        )
        .unwrap();
        let err = dispatch(
            &mut a,
            "proj.filter",
            &obj(vec![
                ("column", Json::Str("Name".into())),
                ("values", Json::Arr(vec![])),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("non-empty array"), "{err}");
        assert!(a.ed.is_filtered(), "the filter stands");
    }

    #[test]
    fn selection_based_edits_act_only_on_a_drawn_row() {
        let mut a = dated();
        dispatch(
            &mut a,
            "proj.filter",
            &obj(vec![
                ("column", Json::Str("Name".into())),
                ("values", Json::Arr(vec![Json::Str("Plan".into())])),
            ]),
        )
        .unwrap();
        // Add Task makes a row the filter hides: the selection stays drawn.
        a.ed.select(0);
        a.add_task();
        assert!(a.ed.display_rows().contains(&a.ed.sel()));
        assert_eq!(a.ed.project().tasks[a.ed.sel()].name, "Plan");
        // Rename the only drawn task so nothing is drawn: no edit has a row.
        a.ed.rename(1, "Gone").unwrap();
        assert!(a.ed.display_rows().is_empty());
        assert_eq!(a.ed.shown_selected_uid(), None);
        let before = a.ed.project().tasks.len();
        a.delete_task();
        a.indent(1);
        assert_eq!(a.ed.project().tasks.len(), before, "nothing was deleted");
    }
}
