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
//! | `task.list` | `{fields?}` | `{count, tasks:[{uid, id, outline_number, name, level, manual, duration, start, finish, critical, …}]}` |
//! | `task.get` | `{uid, fields?}` | one task |
//! | `task.fields` | — | `{count, fields:[name…]}`: the field names `fields` can read |
//! | `task.set` | `{uid, name?, duration?, level?, manual?}` | the updated task (`manual`: `true` Manually / `false` Auto Scheduled) |
//! | `task.add` | `{after?, name?, duration?}` | the new task (without `duration`, 1 day, estimated when the plan's `NewTasksEstimated` is; inserted `after` a task that a finish-to-start link joins to the next, it is linked into that chain when the plan's `Autolink` is on, as it is by default) |
//! | `task.del` | `{uid}` | `{deleted, removed:[uid…]}` (a summary takes its subtree) |
//! | `link.add` | `{uid, pred, type?, lag?}` | the updated task (`lag` uses Predecessors cell spellings such as `4h`, `2ed`, `50%`; other duration spellings such as `1 month` are read as working-days lags) |
//! | `link.del` | `{uid, pred}` | the updated task |
//! | `find` | `{query, fields?}` | `{count, tasks:[…]}` |
//! | `assign.list` | `{uid?, resource?, fields?}` | `{count, assignments:[…]}`: every assignment, or those of task `uid` and/or of `resource` (a resource uid, or a name matched ignoring ASCII case) |
//! | `assign.get` | `{uid, fields?}` | one assignment (by its own uid) |
//! | `assign.fields` | — | `{count, fields:[name…]}`: the assignment field names `fields` can read |
//! | `assign.add` | `{task, resource, units?, work?, fields?}` | the new assignment (`resource` a uid, or a name; a new name is staged as a work resource, as the Resource Names cell does) |
//! | `assign.set` | `{uid, units?, work?, rate_table?, delay?, fields?}` | the updated assignment, after its task is rescheduled |
//! | `assign.del` | `{uid}` | `{deleted, task}` |
//!
//! `fields` is a list of Project field names (`["% Complete", "Baseline1
//! Finish"]`, matched ignoring ASCII case and surrounding space). The verbs
//! that reply with a task (`task.set`, `task.add`, `link.add`, `link.del`)
//! take it too, and check it before they edit anything. Each task
//! then carries `fields: {"<name as asked>": {text, value}}`: `text` as the
//! sheet shows it (`1 day`, `4 hrs`, `$1,400.00`, `NA`; the Entry columns in
//! the grid's own spellings, `2d`), `value` underneath: dates
//! `YYYY-MM-DD HH:MM`, durations, work and slack signed minutes, money a
//! number, percents integers, flags booleans, enums and text strings, and
//! `null` for an absent stored value or a date that shows `NA`. An unknown
//! name fails the whole call. See `projcore::editor::fields` for each
//! field's default and unit.
//!
//! An assignment reads as `{uid, task, resource, resource_name, units,
//! work_hours, regular_work_hours, overtime_work_hours, cost, rate_table,
//! baseline_work_hours, baseline_cost, actual_work_hours,
//! remaining_work_hours, actual_cost, remaining_cost, percent_work_complete,
//! start, finish, delay_hours, contour}`: units a fraction (1.0 = 100%; a
//! material's quantity), work in hours, money in currency units, `null` for an
//! absent stored value, `rate_table` a letter (`A` when unset), `contour`
//! Project's name (`Flat`), `start`/`finish` its own dates. The assign verbs
//! take `fields` too, with Project's assignment field names (see
//! `projcore::editor::fields::assignment`; units read as a number). Arguments:
//! `units` a number or a percent string (`"50%"`); `work` and `delay` a
//! number of hours or a duration (`"40h"`, `"5d"`); `rate_table` `"A"`..`"E"`.
//! The task is rescheduled by its type, as in Project: a units edit on a
//! Fixed Units task keeps the work and moves the duration; on Fixed Duration
//! the work follows the units, and units given together with work are
//! recomputed from the work. Every argument is checked before the edit, so
//! a rejected call leaves the editor untouched.
//!
//! `path_info` provides the common `proj.path` fields. File verbs (`proj.save`,
//! `proj.reload`, `proj.open`) belong to the host and are not dispatched here.

use ctlcore::json::Json;
use projcore::datetime::DateTime;
use projcore::editor::{
    AssignmentField, AssignmentPatch, DURATION_HINT, Editor, Field, FieldReader, FieldValue,
    ResourceRef, TaskPatch, assignment_dates, assignment_field_names, duration_format_code,
    field_names, parse_duration, parse_lag, parse_task_duration_unit, rate_table_letter,
    read_assignment_field, work_contour_name,
};
use projcore::model::{Assignment, LagFormat, LagUnit, LinkType, Predecessor, Rate, Task};

/// Successful calls to these verbs signal agent editing activity.
pub const MUTATING: &[&str] = &[
    "task.set",
    "task.add",
    "task.del",
    "link.add",
    "link.del",
    "assign.add",
    "assign.set",
    "assign.del",
];

/// Recognize editor verbs before a host resolves its target document.
pub fn is_editor_verb(verb: &str) -> bool {
    MUTATING.contains(&verb)
        || matches!(
            verb,
            "task.list"
                | "task.get"
                | "task.fields"
                | "find"
                | "assign.list"
                | "assign.get"
                | "assign.fields"
        )
}

/// Project verbs independent of the host's file handling. `None` means the
/// verb belongs to the host (or is unknown).
pub fn dispatch_editor(ed: &mut Editor, verb: &str, args: &Json) -> Option<Result<Json, String>> {
    Some(match verb {
        "task.list" => task_list(ed, args),
        "task.get" => task_get(ed, args),
        "task.fields" => Ok(task_fields()),
        "task.set" => task_set(ed, args),
        "task.add" => task_add(ed, args),
        "task.del" => task_del(ed, args),
        "link.add" => link_add(ed, args),
        "link.del" => link_del(ed, args),
        "find" => find(ed, args),
        "assign.list" => assign_list(ed, args),
        "assign.get" => assign_get(ed, args),
        "assign.fields" => Ok(assign_fields()),
        "assign.add" => assign_add(ed, args),
        "assign.set" => assign_set(ed, args),
        "assign.del" => assign_del(ed, args),
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

/// The fields a read asks for, each with the name as the caller wrote it;
/// `None` when it asks for none. Every name resolves before any output.
fn fields_arg(args: &Json) -> Result<Option<Vec<(String, Field)>>, String> {
    fields_of(args, Field::parse)
}

/// `fields` resolved by `parse`, each with the name as the caller wrote it.
fn fields_of<F>(
    args: &Json,
    parse: impl Fn(&str) -> Result<F, String>,
) -> Result<Option<Vec<(String, F)>>, String> {
    let Some(fields) = args.get("fields") else {
        return Ok(None);
    };
    let names = fields
        .as_array()
        .ok_or("'fields' must be a list of field names")?;
    names
        .iter()
        .map(|name| {
            let name = name
                .as_str()
                .ok_or("'fields' must be a list of field names")?;
            Ok((name.to_string(), parse(name)?))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Some)
}

fn field_value_json(value: FieldValue) -> Json {
    match value {
        FieldValue::Null => Json::Null,
        FieldValue::Bool(b) => Json::Bool(b),
        FieldValue::Int(n) | FieldValue::Minutes(n) => Json::Num(n as f64),
        FieldValue::Money(m) | FieldValue::Number(m) => Json::Num(m),
        FieldValue::Date(d) => Json::Str(dt_str(d)),
        FieldValue::Text(s) => Json::Str(s),
    }
}

fn task_fields() -> Json {
    names_json(field_names())
}

fn names_json(names: Vec<String>) -> Json {
    Json::obj(vec![
        ("count", Json::Num(names.len() as f64)),
        (
            "fields",
            Json::Arr(names.into_iter().map(Json::Str).collect()),
        ),
    ])
}

/// One task as JSON, including its scheduled (or leveled) dates, its row ID
/// and outline number, and the `fields` asked for.
fn task_json(
    ed: &Editor,
    reader: &FieldReader,
    t: &Task,
    asked: Option<&[(String, Field)]>,
) -> Json {
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
        ("id", Json::Num(t.id as f64)),
        (
            "outline_number",
            reader
                .outline_number(t.uid)
                .map_or(Json::Null, |n| Json::Str(n.to_string())),
        ),
        ("name", Json::Str(t.name.clone())),
        ("level", Json::Num(t.outline_level as f64)),
        ("summary", Json::Bool(t.summary)),
        ("milestone", Json::Bool(t.is_milestone())),
        ("manual", Json::Bool(t.manual)),
        (
            "duration_days",
            Json::Num(ed.project().minutes_to_days(t.duration_min)),
        ),
        // As the duration shows it (`1d?`); a summary rolls it up.
        (
            "estimated",
            Json::Bool(!projcore::editor::duration_suffix(ed.project(), t.uid).is_empty()),
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
    // finish, and warns when its subtasks finish after it or it finishes
    // after its parent manual summary.
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
    if let Some(asked) = asked {
        let values = asked
            .iter()
            .map(|(name, field)| {
                let read = reader.read(t, *field);
                (
                    name.clone(),
                    Json::obj(vec![
                        ("text", Json::Str(read.text)),
                        ("value", field_value_json(read.value)),
                    ]),
                )
            })
            .collect();
        fields.push(("fields", Json::Obj(values)));
    }
    Json::obj(fields)
}

fn task_list(ed: &Editor, args: &Json) -> Result<Json, String> {
    let asked = fields_arg(args)?;
    let reader = FieldReader::new(ed);
    let tasks = ed
        .project()
        .tasks
        .iter()
        .map(|t| task_json(ed, &reader, t, asked.as_deref()))
        .collect();
    Ok(Json::obj(vec![
        ("count", Json::Num(ed.project().tasks.len() as f64)),
        ("tasks", Json::Arr(tasks)),
    ]))
}

fn find(ed: &Editor, args: &Json) -> Result<Json, String> {
    let query = args.get_str("query").ok_or("find needs a 'query'")?;
    let asked = fields_arg(args)?;
    let reader = FieldReader::new(ed);
    let needle = query.to_lowercase();
    let tasks: Vec<Json> = ed
        .project()
        .tasks
        .iter()
        .filter(|t| t.name.to_lowercase().contains(&needle))
        .map(|t| task_json(ed, &reader, t, asked.as_deref()))
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
    let asked = fields_arg(args)?;
    task_reply(ed, uid, asked.as_deref())
}

/// Task `uid` as a read or an edit replies with it, with the fields asked
/// for. An edit resolves `fields` before it changes anything, so a bad list
/// is rejected with the editor untouched.
fn task_reply(ed: &Editor, uid: i32, asked: Option<&[(String, Field)]>) -> Result<Json, String> {
    let i = task_index(ed, uid)?;
    Ok(task_json(
        ed,
        &FieldReader::new(ed),
        &ed.project().tasks[i],
        asked,
    ))
}

fn task_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let asked = fields_arg(args)?;
    let duration = args
        .get_str("duration")
        .map(|d| {
            parse_task_duration_unit(d, ed.project())
                .ok_or_else(|| format!("Couldn't read duration '{d}' ({DURATION_HINT})"))
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
            duration_min: duration.map(|d| d.0),
            level,
            manual,
            estimated: duration.map(|d| d.1),
            duration_format: duration.and_then(|d| duration_format_code(d.2)),
        },
    )?;
    task_reply(ed, uid, asked.as_deref())
}

fn task_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let after = args
        .get("after")
        .map(|_| uid_arg(args, "after"))
        .transpose()?;
    let asked = fields_arg(args)?;
    let (duration_min, estimated, unit) = match args.get_str("duration") {
        Some(d) => parse_task_duration_unit(d, ed.project())
            .ok_or_else(|| format!("Couldn't read duration '{d}' ({DURATION_HINT})"))?,
        // The default duration is estimated when the plan's new tasks are.
        None => (480, ed.project().new_tasks_estimated(), LagUnit::Day),
    };
    let name = args.get_str("name").unwrap_or("New task");
    // One undo step: the task, then the unit its duration was typed in.
    let at = ed.batch(|ed| {
        let at = ed.add_task(after, name, duration_min, estimated)?;
        let uid = ed.project().tasks[at].uid;
        ed.set_duration_typed(uid, duration_min, estimated, unit)?;
        Ok(at)
    })?;
    task_reply(ed, ed.project().tasks[at].uid, asked.as_deref())
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
    let asked = fields_arg(args)?;
    let link = match args.get_str("type") {
        Some(t) => parse_link_name(t).ok_or("'type' must be FS, SS, FF, or SF")?,
        None => LinkType::FinishStart,
    };
    // The Predecessors cell's lag grammar; anything else parse_duration reads
    // (a bare number, unit words, months) is a working-days lag.
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
            ..Predecessor::fs(pred)
        },
    )?;
    task_reply(ed, uid, asked.as_deref())
}

fn link_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let asked = fields_arg(args)?;
    ed.remove_predecessor(uid, uid_arg(args, "pred")?)?;
    task_reply(ed, uid, asked.as_deref())
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

fn assign_fields() -> Json {
    names_json(assignment_field_names())
}

fn hours(min: i64) -> Json {
    Json::Num(min as f64 / 60.0)
}

fn opt_hours(min: Option<i64>) -> Json {
    min.map_or(Json::Null, hours)
}

/// Money MSPDI stores in hundredths, as currency units.
fn money(rate: Option<&Rate>) -> Json {
    rate.and_then(Rate::to_f64)
        .map_or(Json::Null, |h| Json::Num(h / 100.0))
}

/// One assignment as JSON, with the `fields` asked for.
fn assignment_json(
    ed: &Editor,
    a: &Assignment,
    asked: Option<&[(String, AssignmentField)]>,
) -> Json {
    let proj = ed.project();
    let resource = proj.resources.iter().find(|r| r.uid == a.resource_uid);
    let baseline = a.baseline(0);
    let dates = assignment_dates(ed, a);
    let date = |d: Option<DateTime>| d.map_or(Json::Null, |d| Json::Str(dt_str(d)));
    let mut out = vec![
        ("uid", Json::Num(a.uid as f64)),
        ("task", Json::Num(a.task_uid as f64)),
        ("resource", Json::Num(a.resource_uid as f64)),
        (
            "resource_name",
            resource.map_or(Json::Null, |r| Json::Str(r.name.clone())),
        ),
        ("units", Json::Num(a.units)),
        ("work_hours", hours(a.work_min)),
        ("regular_work_hours", opt_hours(a.regular_work_min)),
        ("overtime_work_hours", opt_hours(a.overtime_work_min)),
        ("cost", money(a.cost.as_ref())),
        (
            "rate_table",
            Json::Str(rate_table_letter(a.cost_rate_table).to_string()),
        ),
        (
            "baseline_work_hours",
            opt_hours(baseline.and_then(|b| b.work_min)),
        ),
        (
            "baseline_cost",
            money(baseline.and_then(|b| b.cost.as_ref())),
        ),
        ("actual_work_hours", opt_hours(a.actual_work_min)),
        ("remaining_work_hours", opt_hours(a.remaining_work_min)),
        ("actual_cost", money(a.actual_cost.as_ref())),
        ("remaining_cost", money(a.remaining_cost.as_ref())),
        (
            "percent_work_complete",
            a.percent_work_complete
                .map_or(Json::Null, |p| Json::Num(f64::from(p))),
        ),
        ("start", date(dates.map(|(s, _)| s))),
        ("finish", date(dates.map(|(_, f)| f))),
        ("delay_hours", hours(a.delay_min())),
        (
            "contour",
            Json::Str(work_contour_name(a.work_contour).into()),
        ),
    ];
    if let Some(asked) = asked {
        let values = asked
            .iter()
            .map(|(name, field)| {
                let read = read_assignment_field(ed, a, *field);
                (
                    name.clone(),
                    Json::obj(vec![
                        ("text", Json::Str(read.text)),
                        ("value", field_value_json(read.value)),
                    ]),
                )
            })
            .collect();
        out.push(("fields", Json::Obj(values)));
    }
    Json::obj(out)
}

fn assignment_uid(args: &Json) -> Result<i32, String> {
    args.get("uid")
        .and_then(Json::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| "needs a numeric 'uid' (an assignment UID)".to_string())
}

/// Assignment `uid` as a read or an edit replies with it.
fn assignment_reply(
    ed: &Editor,
    uid: i32,
    asked: Option<&[(String, AssignmentField)]>,
) -> Result<Json, String> {
    let a = ed
        .project()
        .assignments
        .iter()
        .find(|a| a.uid == uid)
        .ok_or_else(|| format!("no assignment with uid {uid}"))?;
    Ok(assignment_json(ed, a, asked))
}

/// `resource` as a uid (a number) or a name (a string).
fn resource_arg(args: &Json) -> Result<Option<ResourceRef<'_>>, String> {
    let Some(r) = args.get("resource") else {
        return Ok(None);
    };
    if let Some(name) = r.as_str() {
        return Ok(Some(ResourceRef::Name(name)));
    }
    r.as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .map(|uid| Some(ResourceRef::Uid(uid)))
        .ok_or_else(|| "'resource' must be a resource uid or name".to_string())
}

/// `units` as a fraction: a number, or a percent string (`"50%"`).
fn units_arg(args: &Json) -> Result<Option<f64>, String> {
    let Some(u) = args.get("units") else {
        return Ok(None);
    };
    let units = match u.as_str() {
        Some(text) => {
            let text = text.trim();
            match text.strip_suffix('%') {
                Some(n) => n.trim_end().parse::<f64>().ok().map(|n| n / 100.0),
                None => text.parse::<f64>().ok(),
            }
        }
        None => u.as_f64(),
    };
    units
        .filter(|u| u.is_finite())
        .map(Some)
        .ok_or_else(|| "couldn't read 'units' (try 0.5 or \"50%\")".to_string())
}

/// `key` in minutes: a number of hours, or a duration (`"40h"`, `"5d"`).
fn hours_arg(ed: &Editor, args: &Json, key: &str) -> Result<Option<i64>, String> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    // A bare number, even as a string, is hours; a duration needs its unit
    // (`parse_duration` would read a bare number as days).
    let hours = match v.as_str() {
        Some(text) => text.trim().parse::<f64>().ok(),
        None => v.as_f64(),
    };
    let min = match (hours, v.as_str()) {
        (Some(h), _) => Some(h)
            .filter(|h| h.is_finite())
            .map(|h| (h * 60.0).round() as i64),
        (None, Some(text)) => parse_duration(text, ed.project()),
        (None, None) => None,
    };
    min.map(Some)
        .ok_or_else(|| format!("couldn't read '{key}' (hours, or a duration such as 40h or 5d)"))
}

/// `rate_table` as a table number, from its letter `A`..`E`.
fn rate_table_arg(args: &Json) -> Result<Option<u8>, String> {
    let Some(t) = args.get("rate_table") else {
        return Ok(None);
    };
    let letter = t.as_str().map(str::trim).unwrap_or_default();
    match letter.to_ascii_uppercase().as_bytes() {
        [l @ b'A'..=b'E'] => Ok(Some(l - b'A')),
        _ => Err("'rate_table' must be a letter A to E".into()),
    }
}

fn assign_list(ed: &Editor, args: &Json) -> Result<Json, String> {
    let asked = fields_of(args, AssignmentField::parse)?;
    let task = args.get("uid").map(|_| uid_arg(args, "uid")).transpose()?;
    if let Some(uid) = task {
        task_index(ed, uid)?;
    }
    let proj = ed.project();
    let resource = match resource_arg(args)? {
        None => None,
        Some(ResourceRef::Uid(uid)) => {
            if !proj.resources.iter().any(|r| r.uid == uid) {
                return Err(format!("no resource with uid {uid}"));
            }
            Some(uid)
        }
        Some(ResourceRef::Name(name)) => Some(
            proj.resources
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name.trim()))
                .ok_or_else(|| format!("no resource named '{name}'"))?
                .uid,
        ),
    };
    let assignments: Vec<Json> = proj
        .assignments
        .iter()
        .filter(|a| task.is_none_or(|uid| a.task_uid == uid))
        .filter(|a| resource.is_none_or(|uid| a.resource_uid == uid))
        .map(|a| assignment_json(ed, a, asked.as_deref()))
        .collect();
    Ok(Json::obj(vec![
        ("count", Json::Num(assignments.len() as f64)),
        ("assignments", Json::Arr(assignments)),
    ]))
}

fn assign_get(ed: &Editor, args: &Json) -> Result<Json, String> {
    let uid = assignment_uid(args)?;
    let asked = fields_of(args, AssignmentField::parse)?;
    assignment_reply(ed, uid, asked.as_deref())
}

fn assign_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let task = uid_arg(args, "task")?;
    let resource =
        resource_arg(args)?.ok_or("assign.add needs a 'resource' (a resource uid or name)")?;
    let asked = fields_of(args, AssignmentField::parse)?;
    let units = units_arg(args)?;
    let work = hours_arg(ed, args, "work")?;
    let uid = ed.add_assignment(task, resource, units, work)?;
    assignment_reply(ed, uid, asked.as_deref())
}

fn assign_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = assignment_uid(args)?;
    let asked = fields_of(args, AssignmentField::parse)?;
    let patch = AssignmentPatch {
        units: units_arg(args)?,
        work_min: hours_arg(ed, args, "work")?,
        rate_table: rate_table_arg(args)?,
        delay_min: hours_arg(ed, args, "delay")?,
    };
    ed.set_assignment(uid, patch)?;
    assignment_reply(ed, uid, asked.as_deref())
}

fn assign_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = assignment_uid(args)?;
    let task = ed.delete_assignment(uid)?;
    Ok(Json::obj(vec![
        ("deleted", Json::Num(uid as f64)),
        ("task", Json::Num(task as f64)),
    ]))
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

    #[test]
    fn link_add_rejects_summary_subtask_links_from_the_control_fixture() {
        let project =
            projcore::mspdi::read_mspdi(include_str!("../../corpus/mspdi/10-summary.xml")).unwrap();
        let mut ed = Editor::new(project);
        for (uid, pred) in [(2, 1), (1, 3)] {
            let before = ed.project().clone();
            let depth = ed.undo_depth();
            let args = Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("pred", Json::Num(pred as f64)),
            ]);
            let err = dispatch_editor(&mut ed, "link.add", &args)
                .unwrap()
                .unwrap_err();
            assert!(err.contains("summary and its subtask"), "{err}");
            assert_eq!(ed.project(), &before);
            assert_eq!(ed.undo_depth(), depth);
        }
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
    fn durations_take_and_report_an_estimate() {
        let mut a = app();
        let uid = add(&mut a, "Guess", "3d?");
        let get =
            |a: &Editor| task_get(a, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap();
        assert_eq!(get(&a).get("estimated"), Some(&Json::Bool(true)));
        assert_eq!(get(&a).get("duration_days").unwrap().as_f64(), Some(3.0));
        // The same duration without `?` commits it.
        let r = task_set(
            &mut a,
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("duration", Json::Str("3d".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("estimated"), Some(&Json::Bool(false)));
        let r = task_set(
            &mut a,
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("duration", Json::Str("4d?".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("estimated"), Some(&Json::Bool(true)));
        assert_eq!(r.get("duration_days").unwrap().as_f64(), Some(4.0));
    }

    #[test]
    fn task_verbs_accept_word_units_and_estimates() {
        let mut a = app();
        let uid = add(&mut a, "Two days", "2d");
        let set = |a: &mut Editor, duration: &str| {
            task_set(
                a,
                &Json::obj(vec![
                    ("uid", Json::Num(uid as f64)),
                    ("duration", Json::Str(duration.into())),
                ]),
            )
            .unwrap()
        };
        let r = set(&mut a, "2 week");
        assert_eq!(a.project().task(uid as i32).unwrap().duration_min, 4800);
        assert_eq!(r.get("estimated"), Some(&Json::Bool(false)));
        let r = set(&mut a, "2d?");
        assert_eq!(a.project().task(uid as i32).unwrap().duration_min, 960);
        assert_eq!(r.get("estimated"), Some(&Json::Bool(true)));
        let r = set(&mut a, "1 mon?");
        assert_eq!(a.project().task(uid as i32).unwrap().duration_min, 9600);
        assert_eq!(r.get("estimated"), Some(&Json::Bool(true)));
        let added = task_add(
            &mut a,
            &Json::obj(vec![
                ("name", Json::Str("Hours".into())),
                ("duration", Json::Str("4 hour".into())),
            ]),
        )
        .unwrap();
        assert_eq!(added.get("duration_days").unwrap().as_f64(), Some(0.5));
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
            ("1mo", 9600, 7),
            ("1 month", 9600, 7),
            ("2 weeks", 4800, 7),
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
        for bad in ["50e%", "2x"] {
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
    fn a_duration_s_unit_is_kept_and_saved() {
        let mut ed = app();
        let depth = ed.undo_depth();
        let set = Json::parse(r#"{"uid":1,"duration":"1.5w"}"#).unwrap();
        dispatch_editor(&mut ed, "task.set", &set).unwrap().unwrap();
        let add = Json::parse(r#"{"name":"Hours","duration":"4h?"}"#).unwrap();
        let added = dispatch_editor(&mut ed, "task.add", &add).unwrap().unwrap();
        let uid = added.get_usize("uid").unwrap() as i32;
        // The task and its unit are one step.
        assert_eq!(ed.undo_depth(), depth + 2);
        let task = ed.project().task(uid).unwrap();
        assert_eq!(
            (task.duration_min, task.estimated, task.duration_format),
            (240, Some(true), Some(5))
        );
        let saved =
            projcore::mspdi::read_mspdi(&projcore::mspdi::write_mspdi(ed.project())).unwrap();
        assert_eq!(saved.task(1).unwrap().duration_format, Some(9));
        assert_eq!(saved.task(1).unwrap().duration_min, 3600);
        assert_eq!(saved.task(uid).unwrap().duration_format, Some(5));
        ed.undo();
        assert!(ed.project().task(uid).is_none());
        assert_eq!(ed.project().task(1).unwrap().duration_format, Some(9));
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
    fn add_without_a_duration_follows_the_plans_new_tasks_estimated() {
        for (stated, estimated) in [(None, true), (Some(true), true), (Some(false), false)] {
            let mut p = new_project();
            p.new_tasks_estimated = stated;
            let mut ed = Editor::new(p);
            let r = dispatch_editor(&mut ed, "task.add", &Json::Null)
                .unwrap()
                .unwrap();
            assert_eq!(
                r.get("estimated"),
                Some(&Json::Bool(estimated)),
                "{stated:?}"
            );
            assert_eq!(r.get("duration_days").unwrap().as_f64(), Some(1.0));
        }
        // A typed duration keeps its own `?` rule.
        let mut a = app();
        let uid = add(&mut a, "Sure", "2d");
        let r = task_get(&a, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap();
        assert_eq!(r.get("estimated"), Some(&Json::Bool(false)));
    }

    #[test]
    fn add_after_a_task_links_it_into_the_chain_it_splits() {
        let mut a = app();
        let b = add(&mut a, "B", "1d");
        let link = |a: &mut Editor, uid: i64, pred: i64| {
            link_add(
                a,
                &Json::obj(vec![
                    ("uid", Json::Num(uid as f64)),
                    ("pred", Json::Num(pred as f64)),
                ]),
            )
        };
        link(&mut a, b, 1).unwrap();
        let args = Json::parse(r#"{"after":1,"name":"N"}"#).unwrap();
        let r = dispatch_editor(&mut a, "task.add", &args).unwrap().unwrap();
        let n = r.get("uid").unwrap().as_i64().unwrap();
        let preds = |r: &Json| -> Vec<i64> {
            r.get("predecessors")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.get("uid").unwrap().as_i64().unwrap())
                .collect()
        };
        assert_eq!(preds(&r), [1]);
        let b_task = task_get(&a, &Json::obj(vec![("uid", Json::Num(b as f64))])).unwrap();
        assert_eq!(preds(&b_task), [n]);
        // An agent linking N to A itself gets an error, not a duplicate.
        let depth = a.undo_depth();
        let err = link(&mut a, n, 1).unwrap_err();
        assert!(err.contains("Already depends on"), "{err}");
        assert_eq!(a.undo_depth(), depth);
        assert_eq!(a.project().task(n as i32).unwrap().predecessors.len(), 1);
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
        let pred = &ed.project().tasks[1].predecessors[0];
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
        ed.add_task(None, "Another task", 480, false).unwrap();
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

    fn call(ed: &mut Editor, verb: &str, args: Vec<(&str, Json)>) -> Result<Json, String> {
        dispatch_editor(ed, verb, &Json::obj(args)).unwrap()
    }

    fn names(names: &[&str]) -> Json {
        Json::Arr(names.iter().map(|n| Json::Str(n.to_string())).collect())
    }

    /// `(text, value)` of one requested field in a task's JSON.
    fn field<'a>(task: &'a Json, name: &str) -> (&'a str, &'a Json) {
        let f = task.get("fields").unwrap().get(name).unwrap();
        (f.get_str("text").unwrap(), f.get("value").unwrap())
    }

    #[test]
    fn task_reads_carry_row_id_outline_number_and_asked_fields() {
        let project =
            projcore::mspdi::read_mspdi(include_str!("../../corpus/mspdi/10-summary.xml")).unwrap();
        let mut ed = Editor::new(project);
        let r = call(&mut ed, "task.get", vec![("uid", Json::Num(3.0))]).unwrap();
        assert_eq!(r.get("id"), Some(&Json::Num(3.0)));
        assert_eq!(r.get_str("outline_number"), Some("1.2"));
        assert!(r.get("fields").is_none());

        let asked = names(&["Duration", " % complete", "Actual Start", "Baseline10 Cost"]);
        let r = call(
            &mut ed,
            "task.get",
            vec![("uid", Json::Num(3.0)), ("fields", asked.clone())],
        )
        .unwrap();
        assert_eq!(field(&r, "Duration"), ("1d", &Json::Num(480.0)));
        // Keyed by the name as asked; unset values are null, shown as Project shows them.
        assert_eq!(field(&r, " % complete"), ("0%", &Json::Null));
        assert_eq!(field(&r, "Actual Start"), ("NA", &Json::Null));
        assert_eq!(field(&r, "Baseline10 Cost"), ("$0.00", &Json::Null));

        let list = call(&mut ed, "task.list", vec![("fields", asked)]).unwrap();
        let tasks = list.get("tasks").unwrap().as_array().unwrap();
        let outline: Vec<_> = tasks.iter().map(|t| t.get_str("outline_number")).collect();
        assert_eq!(outline, [Some("1"), Some("1.1"), Some("1.2")]);
        assert!(tasks.iter().all(|t| t.get("fields").is_some()));
        assert_eq!(field(&tasks[0], "Duration").0, "2d");

        let found = call(
            &mut ed,
            "find",
            vec![
                ("query", Json::Str("B".into())),
                ("fields", names(&["Start"])),
            ],
        )
        .unwrap();
        let hit = &found.get("tasks").unwrap().as_array().unwrap()[0];
        assert_eq!(
            field(hit, "Start"),
            ("2026-03-03", &Json::Str("2026-03-03 08:00".into()))
        );
    }

    #[test]
    fn task_fields_lists_every_readable_name() {
        let mut ed = app();
        assert!(is_editor_verb("task.fields"));
        assert!(!MUTATING.contains(&"task.fields"));
        let r = call(&mut ed, "task.fields", vec![]).unwrap();
        let listed = r.get("fields").unwrap().as_array().unwrap();
        assert_eq!(r.get("count"), Some(&Json::Num(listed.len() as f64)));
        for name in ["% Complete", "Baseline10 Cost", "Total Slack", "Unique ID"] {
            assert!(listed.iter().any(|n| n.as_str() == Some(name)), "{name}");
        }
        // Every listed name reads.
        let r = call(
            &mut ed,
            "task.get",
            vec![
                ("uid", Json::Num(1.0)),
                ("fields", Json::Arr(listed.to_vec())),
            ],
        )
        .unwrap();
        let read = match r.get("fields") {
            Some(Json::Obj(pairs)) => pairs.len(),
            other => panic!("{other:?}"),
        };
        assert_eq!(read, listed.len());
    }

    #[test]
    fn a_bad_field_list_rejects_an_edit_before_it_changes_anything() {
        let mut ed = app();
        let second = add(&mut ed, "Second", "1d");
        let third = add(&mut ed, "Third", "1d");
        ed.add_predecessor(second as i32, 1, LinkType::FinishStart, 0)
            .unwrap();
        ed.rename(1, "Change").unwrap();
        ed.undo();
        ed.mark_saved();
        let before = ed.project().clone();
        let edits = [
            (
                "task.set",
                vec![("uid", Json::Num(1.0)), ("name", Json::Str("X".into()))],
            ),
            ("task.add", vec![("name", Json::Str("Y".into()))]),
            (
                "link.add",
                vec![("uid", Json::Num(third as f64)), ("pred", Json::Num(1.0))],
            ),
            (
                "link.del",
                vec![("uid", Json::Num(second as f64)), ("pred", Json::Num(1.0))],
            ),
        ];
        for (verb, extra) in edits {
            for bad in [names(&["Name", "Status"]), Json::Str("x".into())] {
                let mut args = extra.clone();
                args.push(("fields", bad));
                assert!(call(&mut ed, verb, args).is_err(), "{verb}");
                assert_eq!(ed.project(), &before, "{verb}");
                assert_eq!((ed.undo_depth(), ed.redo_depth()), (3, 1), "{verb}");
                assert!(!ed.dirty(), "{verb}");
            }
        }
        // A good list comes back on the edit's reply.
        let r = call(
            &mut ed,
            "task.add",
            vec![
                ("name", Json::Str("Z".into())),
                ("fields", names(&["Name", "Outline Number"])),
            ],
        )
        .unwrap();
        assert_eq!(field(&r, "Name"), ("Z", &Json::Str("Z".into())));
        let r = call(
            &mut ed,
            "link.add",
            vec![
                ("uid", Json::Num(third as f64)),
                ("pred", Json::Num(1.0)),
                ("fields", names(&["Predecessors"])),
            ],
        )
        .unwrap();
        assert_eq!(field(&r, "Predecessors").0, "1");
    }

    #[test]
    fn a_bad_field_list_fails_the_whole_read() {
        let mut ed = app();
        for (verb, extra) in [
            ("task.get", vec![("uid", Json::Num(1.0))]),
            ("task.list", vec![]),
            ("find", vec![("query", Json::Str("task".into()))]),
        ] {
            let mut args = extra.clone();
            args.push(("fields", names(&["Name", "Status", "Bogus"])));
            assert_eq!(
                call(&mut ed, verb, args),
                Err("unknown task field 'Status'".into()),
                "{verb}"
            );
            for bad in [Json::Str("Name".into()), Json::Arr(vec![Json::Num(1.0)])] {
                let mut args = extra.clone();
                args.push(("fields", bad));
                assert_eq!(
                    call(&mut ed, verb, args),
                    Err("'fields' must be a list of field names".into()),
                    "{verb}"
                );
            }
        }
    }

    #[test]
    fn finish_variance_follows_a_duration_edit_after_set_baseline() {
        let mut ed = app();
        let first = add(&mut ed, "First", "2d");
        let second = add(&mut ed, "Second", "1d");
        link_add(
            &mut ed,
            &Json::obj(vec![
                ("uid", Json::Num(second as f64)),
                ("pred", Json::Num(first as f64)),
            ]),
        )
        .unwrap();
        ed.set_baseline();
        let read = |ed: &mut Editor| {
            call(
                ed,
                "task.get",
                vec![
                    ("uid", Json::Num(second as f64)),
                    ("fields", names(&["Baseline Finish", "Finish Variance"])),
                ],
            )
            .unwrap()
        };
        let before = read(&mut ed);
        assert_eq!(
            field(&before, "Finish Variance"),
            ("0 days", &Json::Num(0.0))
        );
        call(
            &mut ed,
            "task.set",
            vec![
                ("uid", Json::Num(first as f64)),
                ("duration", Json::Str("4d".into())),
            ],
        )
        .unwrap();
        let after = read(&mut ed);
        assert_eq!(
            field(&after, "Baseline Finish"),
            field(&before, "Baseline Finish")
        );
        assert_eq!(
            field(&after, "Finish Variance"),
            ("2 days", &Json::Num(960.0))
        );
    }

    #[test]
    fn slack_reads_in_minutes_including_negative_total_slack() {
        let mut ed = app();
        let uid = add(&mut ed, "Late", "3d") as i32;
        let mut p = ed.project().clone();
        p.tasks.iter_mut().find(|t| t.uid == uid).unwrap().deadline =
            Some(DateTime::from_ymd_hm(2026, 1, 5, 17, 0));
        let mut ed = Editor::new(p);
        let r = call(
            &mut ed,
            "task.get",
            vec![
                ("uid", Json::Num(uid as f64)),
                ("fields", names(&["Total Slack", "Free Slack"])),
            ],
        )
        .unwrap();
        assert_eq!(field(&r, "Total Slack"), ("-2 days", &Json::Num(-960.0)));
        assert_eq!(field(&r, "Free Slack"), ("0 days", &Json::Num(0.0)));
    }

    #[test]
    fn the_project_summary_row_reads_over_the_control_surface() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Name>plan</Name><CalendarUID>1</CalendarUID>
  <StartDate>2026-03-02T08:00:00</StartDate>
  <ProjectExternallyEdited>0</ProjectExternallyEdited>
  <Tasks>
    <Task><UID>0</UID><ID>0</ID><Name>Plan</Name><OutlineLevel>0</OutlineLevel><Summary>1</Summary>
      <Duration>PT24H0M0S</Duration><Cost>140000</Cost><PercentComplete>25</PercentComplete>
      <Notes>Kickoff Monday</Notes></Task>
    <Task><UID>1</UID><ID>1</ID><Name>A</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT16H0M0S</Duration></Task>
    <Task><UID>2</UID><ID>2</ID><Name>B</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT8H0M0S</Duration>
      <PredecessorLink><PredecessorUID>1</PredecessorUID><Type>1</Type></PredecessorLink></Task>
  </Tasks>
</Project>"#;
        let mut ed = Editor::new(projcore::mspdi::read_mspdi(xml).unwrap());
        let asked = names(&["Duration", "Start", "Finish", "% Complete", "Cost", "Notes"]);
        let r = call(
            &mut ed,
            "task.get",
            vec![("uid", Json::Num(0.0)), ("fields", asked)],
        )
        .unwrap();
        assert_eq!(r.get_str("outline_number"), Some("0"));
        assert_eq!(field(&r, "Duration"), ("3d", &Json::Num(1440.0)));
        assert_eq!(
            field(&r, "Start"),
            ("2026-03-02", &Json::Str("2026-03-02 08:00".into()))
        );
        assert_eq!(
            field(&r, "Finish"),
            ("2026-03-04", &Json::Str("2026-03-04 17:00".into()))
        );
        assert_eq!(field(&r, "% Complete"), ("25%", &Json::Num(25.0)));
        assert_eq!(field(&r, "Cost"), ("$1,400.00", &Json::Num(1400.0)));
        assert_eq!(
            field(&r, "Notes"),
            ("Kickoff Monday", &Json::Str("Kickoff Monday".into()))
        );
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

    // ---- assignments (#395) ----

    fn run(ed: &mut Editor, verb: &str, args: &str) -> Result<Json, String> {
        dispatch_editor(ed, verb, &Json::parse(args).unwrap()).unwrap()
    }

    fn num(j: &Json, key: &str) -> f64 {
        j.get(key).and_then(Json::as_f64).unwrap()
    }

    /// Task 1, "Build", 5 days, typed `kind`, with stored (zero) totals; Ann
    /// (1) at $50/h and $80/h on rate table B, Bob (2) at $30/h.
    fn staffed(kind: projcore::model::TaskType, effort_driven: bool) -> Editor {
        use projcore::model::{RateEntry, Resource};
        let mut p = new_project();
        let t = &mut p.tasks[0];
        t.name = "Build".into();
        t.duration_min = 5 * 480;
        t.task_type = Some(kind);
        t.effort_driven = Some(effort_driven);
        t.work_min = Some(0);
        t.cost = Rate::parse("0");
        let resource = |uid, name: &str, rate| Resource {
            uid,
            id: uid,
            name: name.into(),
            max_units: 1.0,
            standard_rate: Rate::parse(rate),
            ..Resource::default()
        };
        let mut ann = resource(1, "Ann", "50");
        ann.rates.push(RateEntry {
            rate_table: Some(1),
            standard_rate: Rate::parse("80"),
            ..RateEntry::default()
        });
        p.resources = vec![ann, resource(2, "Bob", "30")];
        Editor::new(p)
    }

    #[test]
    fn assign_verbs_are_editor_verbs_and_edits_mutate() {
        for verb in ["assign.list", "assign.get", "assign.fields"] {
            assert!(is_editor_verb(verb), "{verb}");
            assert!(!MUTATING.contains(&verb), "{verb}");
        }
        for verb in ["assign.add", "assign.set", "assign.del"] {
            assert!(is_editor_verb(verb), "{verb}");
            assert!(MUTATING.contains(&verb), "{verb}");
        }
    }

    /// The issue's acceptance script: two resources at different units on
    /// one task, a units edit, a deletion and a rate table switch.
    #[test]
    fn a_script_assigns_changes_deletes_and_reprices_assignments() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedDuration, false);
        let ann = run(
            &mut ed,
            "assign.add",
            r#"{"task":1,"resource":"Ann","units":1}"#,
        )
        .unwrap();
        let bob = run(
            &mut ed,
            "assign.add",
            r#"{"task":1,"resource":2,"units":"50%"}"#,
        )
        .unwrap();
        let (ann_uid, bob_uid) = (num(&ann, "uid"), num(&bob, "uid"));
        let list = run(&mut ed, "assign.list", r#"{"uid":1}"#).unwrap();
        assert_eq!(num(&list, "count"), 2.0);
        let rows = list.get("assignments").unwrap().as_array().unwrap();
        let read = |a: &Json| {
            (
                a.get_str("resource_name").unwrap().to_string(),
                num(a, "units"),
                num(a, "work_hours"),
                num(a, "cost"),
            )
        };
        assert_eq!(read(&rows[0]), ("Ann".into(), 1.0, 40.0, 2000.0));
        assert_eq!(read(&rows[1]), ("Bob".into(), 0.5, 20.0, 600.0));

        // A units edit on a Fixed Duration task recomputes the work.
        let set = format!(r#"{{"uid":{bob_uid},"units":1}}"#);
        let bob = run(&mut ed, "assign.set", &set).unwrap();
        assert_eq!((num(&bob, "work_hours"), num(&bob, "cost")), (40.0, 1200.0));
        let task = run(
            &mut ed,
            "task.get",
            r#"{"uid":1,"fields":["Duration","Work","Cost"]}"#,
        )
        .unwrap();
        assert_eq!(num(&task, "duration_days"), 5.0);
        assert_eq!(field(&task, "Work"), ("80 hrs", &Json::Num(4800.0)));
        assert_eq!(field(&task, "Cost"), ("$3,200.00", &Json::Num(3200.0)));

        // Deleting Bob leaves Ann exactly as she was.
        let get_ann = format!(r#"{{"uid":{ann_uid}}}"#);
        let before = run(&mut ed, "assign.get", &get_ann).unwrap();
        let del = run(&mut ed, "assign.del", &format!(r#"{{"uid":{bob_uid}}}"#)).unwrap();
        assert_eq!((num(&del, "deleted"), num(&del, "task")), (bob_uid, 1.0));
        assert_eq!(run(&mut ed, "assign.get", &get_ann).unwrap(), before);
        assert_eq!(
            num(&run(&mut ed, "assign.list", "{}").unwrap(), "count"),
            1.0
        );

        // Rate table B prices Ann at $80/h.
        let set = format!(r#"{{"uid":{ann_uid},"rate_table":"b"}}"#);
        let ann = run(&mut ed, "assign.set", &set).unwrap();
        assert_eq!(ann.get_str("rate_table"), Some("B"));
        assert_eq!(num(&ann, "cost"), 3200.0);
    }

    #[test]
    fn a_units_edit_on_fixed_units_keeps_work_and_moves_the_duration() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedUnits, false);
        let ann = run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        let set = format!(r#"{{"uid":{},"units":0.5}}"#, num(&ann, "uid"));
        run(&mut ed, "assign.set", &set).unwrap();
        let get = format!(r#"{{"uid":{}}}"#, num(&ann, "uid"));
        let ann = run(&mut ed, "assign.get", &get).unwrap();
        assert_eq!((num(&ann, "units"), num(&ann, "work_hours")), (0.5, 40.0));
        let task = run(&mut ed, "task.get", r#"{"uid":1,"fields":["Work"]}"#).unwrap();
        assert_eq!(num(&task, "duration_days"), 10.0);
        assert_eq!(field(&task, "Work"), ("40 hrs", &Json::Num(2400.0)));
    }

    #[test]
    fn an_assignment_reads_its_own_numbers_and_asked_fields() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedUnits, false);
        let ann = run(
            &mut ed,
            "assign.add",
            r#"{"task":1,"resource":"ann","fields":["Work","Cost","Units"]}"#,
        )
        .unwrap();
        assert_eq!(field(&ann, "Work"), ("40 hrs", &Json::Num(2400.0)));
        assert_eq!(field(&ann, "Cost"), ("$2,000.00", &Json::Num(2000.0)));
        assert_eq!(field(&ann, "Units"), ("100%", &Json::Num(1.0)));
        let keys = [
            "uid",
            "task",
            "resource",
            "resource_name",
            "units",
            "work_hours",
            "regular_work_hours",
            "overtime_work_hours",
            "cost",
            "rate_table",
            "baseline_work_hours",
            "baseline_cost",
            "actual_work_hours",
            "remaining_work_hours",
            "actual_cost",
            "remaining_cost",
            "percent_work_complete",
            "start",
            "finish",
            "delay_hours",
            "contour",
            "fields",
        ];
        let Json::Obj(pairs) = &ann else { panic!() };
        assert_eq!(
            pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            keys
        );
        assert_eq!(ann.get("overtime_work_hours"), Some(&Json::Null));
        assert_eq!(ann.get("baseline_cost"), Some(&Json::Null));
        assert_eq!(ann.get_str("rate_table"), Some("A"));
        assert_eq!(ann.get_str("contour"), Some("Flat"));
        assert_eq!(ann.get_str("start"), Some("2026-01-05 08:00"));
        assert_eq!(ann.get_str("finish"), Some("2026-01-09 17:00"));
        assert_eq!(num(&ann, "remaining_work_hours"), 40.0);

        // Work and delay take hours or a duration.
        let set = format!(r#"{{"uid":{},"work":"4d","delay":8}}"#, num(&ann, "uid"));
        let ann = run(&mut ed, "assign.set", &set).unwrap();
        assert_eq!(
            (num(&ann, "work_hours"), num(&ann, "delay_hours")),
            (32.0, 8.0)
        );

        let fields = run(&mut ed, "assign.fields", "{}").unwrap();
        let listed = fields.get("fields").unwrap().as_array().unwrap();
        assert_eq!(num(&fields, "count"), listed.len() as f64);
        for name in ["Work", "Cost", "Units", "Cost Rate Table", "Budget Cost"] {
            assert!(listed.iter().any(|n| n.as_str() == Some(name)), "{name}");
        }
        // Every listed name reads.
        let get = Json::obj(vec![
            ("uid", Json::Num(num(&ann, "uid"))),
            ("fields", Json::Arr(listed.to_vec())),
        ]);
        let all = dispatch_editor(&mut ed, "assign.get", &get)
            .unwrap()
            .unwrap();
        let Json::Obj(read) = all.get("fields").unwrap() else {
            panic!()
        };
        assert_eq!(read.len(), listed.len());
    }

    #[test]
    fn a_bare_number_of_work_or_delay_is_hours_even_as_a_string() {
        use projcore::model::TaskType;
        for (work, delay, hours) in [
            ("8", "8", 8.0),
            ("8.5", "2", 8.5),
            ("\"8\"", "\"8\"", 8.0),
            ("\" 12 \"", "\"4\"", 12.0),
            ("\"1d\"", "\"1d\"", 8.0),
            ("\"6h\"", "\"6h\"", 6.0),
        ] {
            let mut ed = staffed(TaskType::FixedUnits, false);
            let ann = run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
            let uid = num(&ann, "uid");
            let set = format!(r#"{{"uid":{uid},"work":{work},"delay":{delay}}}"#);
            let ann = run(&mut ed, "assign.set", &set).unwrap();
            assert_eq!(num(&ann, "work_hours"), hours, "{work}");
            let expect = match delay {
                "8" | "\"8\"" | "\"1d\"" => 8.0,
                "2" => 2.0,
                "\"4\"" => 4.0,
                _ => 6.0,
            };
            assert_eq!(num(&ann, "delay_hours"), expect, "{delay}");
        }
        // Far beyond the scheduling horizon.
        let mut ed = staffed(TaskType::FixedUnits, false);
        let ann = run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        let uid = num(&ann, "uid");
        let before = (ed.project().clone(), ed.undo_depth());
        for key in ["work", "delay"] {
            let set = format!(r#"{{"uid":{uid},"{key}":1e17}}"#);
            let err = run(&mut ed, "assign.set", &set).unwrap_err();
            assert_eq!(err, format!("{key} is beyond the scheduling range"));
            assert_eq!((ed.project().clone(), ed.undo_depth()), before);
        }
    }

    #[test]
    fn a_resource_uid_spelled_as_a_string_is_refused_not_staged() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedUnits, false);
        let before = (ed.project().clone(), ed.undo_depth(), ed.dirty());
        let err = run(&mut ed, "assign.add", r#"{"task":1,"resource":"2"}"#).unwrap_err();
        assert_eq!(
            err,
            "no resource named '2'; pass a resource uid as a number"
        );
        assert_eq!((ed.project().clone(), ed.undo_depth(), ed.dirty()), before);
        let bob = run(&mut ed, "assign.add", r#"{"task":1,"resource":2}"#).unwrap();
        assert_eq!(bob.get_str("resource_name"), Some("Bob"));
    }

    #[test]
    fn assignments_filter_by_task_and_resource() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedUnits, false);
        run(&mut ed, "task.add", r#"{"name":"Test"}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":2,"resource":1}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":2,"resource":2}"#).unwrap();
        let count = |ed: &mut Editor, args| num(&run(ed, "assign.list", args).unwrap(), "count");
        assert_eq!(count(&mut ed, "{}"), 3.0);
        assert_eq!(count(&mut ed, r#"{"uid":2}"#), 2.0);
        assert_eq!(count(&mut ed, r#"{"resource":"ANN"}"#), 2.0);
        assert_eq!(count(&mut ed, r#"{"resource":2}"#), 1.0);
        assert_eq!(count(&mut ed, r#"{"uid":1,"resource":2}"#), 0.0);
        for (args, err) in [
            (r#"{"resource":"Nobody"}"#, "no resource named 'Nobody'"),
            (r#"{"resource":9}"#, "no resource with uid 9"),
            (r#"{"uid":9}"#, "no task with uid 9"),
            (r#"{"fields":["Nope"]}"#, "unknown assignment field 'Nope'"),
        ] {
            assert_eq!(run(&mut ed, "assign.list", args).unwrap_err(), err);
        }
    }

    #[test]
    fn assign_edits_are_one_undo_step_and_rejections_touch_nothing() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedUnits, false);
        let before = ed.project().clone();
        run(
            &mut ed,
            "assign.add",
            r#"{"task":1,"resource":"Carol","work":10}"#,
        )
        .unwrap();
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.project().resources.iter().any(|r| r.name == "Carol"));
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        assert!(ed.redo());
        let uid = ed.project().assignments[0].uid;
        for (verb, args) in [
            ("assign.add", r#"{"task":1,"resource":9}"#.to_string()),
            ("assign.add", r#"{"task":1}"#.to_string()),
            (
                "assign.add",
                r#"{"task":1,"resource":1,"units":0}"#.to_string(),
            ),
            (
                "assign.add",
                r#"{"task":1,"resource":1,"units":"lots"}"#.to_string(),
            ),
            (
                "assign.add",
                r#"{"task":1,"resource":1,"fields":["Nope"]}"#.to_string(),
            ),
            ("assign.set", r#"{"uid":99,"units":1}"#.to_string()),
            ("assign.set", format!(r#"{{"uid":{uid}}}"#)),
            ("assign.set", format!(r#"{{"uid":{uid},"rate_table":"F"}}"#)),
            ("assign.set", format!(r#"{{"uid":{uid},"rate_table":1}}"#)),
            ("assign.set", format!(r#"{{"uid":{uid},"work":"soon"}}"#)),
            ("assign.set", format!(r#"{{"uid":{uid},"work":-1}}"#)),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"units":2,"fields":["Nope"]}}"#),
            ),
            ("assign.del", r#"{"uid":99}"#.to_string()),
            ("assign.get", r#"{"uid":99}"#.to_string()),
        ] {
            let state = (
                ed.project().clone(),
                ed.undo_depth(),
                ed.redo_depth(),
                ed.dirty(),
            );
            assert!(run(&mut ed, verb, &args).is_err(), "{verb} {args}");
            let after = (
                ed.project().clone(),
                ed.undo_depth(),
                ed.redo_depth(),
                ed.dirty(),
            );
            assert_eq!(after, state, "{verb} {args}");
        }
        // A value it already has records nothing.
        let depth = ed.undo_depth();
        run(
            &mut ed,
            "assign.set",
            &format!(r#"{{"uid":{uid},"rate_table":"A"}}"#),
        )
        .unwrap();
        assert_eq!(ed.undo_depth(), depth);
        run(&mut ed, "assign.del", &format!(r#"{{"uid":{uid}}}"#)).unwrap();
        assert_eq!(ed.undo_depth(), depth + 1);
        assert!(ed.project().assignments.is_empty());
    }
}
