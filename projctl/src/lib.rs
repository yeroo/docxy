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
//! | `task.set` | `{uid, name?, duration?, level?, manual?, start?, finish?, percent_complete?, actual_start?, actual_finish?, actual_duration?, remaining_duration?, priority?, deadline?, constraint?, task_type?, effort_driven?, notes?, milestone?, calendar?, ignore_resource_calendar?, wbs?, earned_value_method?, custom?}` | the updated task (`manual`: `true` Manually / `false` Auto Scheduled). The Task Information fields (#1550): `start` and `finish` (#1570) are set as typed in the Start and Finish cells: `YYYY-MM-DD` (the task calendar's first working time for a start, its last for a finish) or `YYYY-MM-DDTHH:MM`; an auto task gains a Start or Finish No Earlier Than constraint, a manual one is pinned (keeping its duration for a start, the duration following a finish), after `manual`, `duration`, `milestone` and the calendar are applied, and before the actuals and a given `constraint`; only these see the call's calendar, as a bare `actual_*`, `deadline` or `constraint.date` day resolves on the task's calendar before a `calendar` in the same call; both on an auto task, or either on an auto summary, are refused, and `null` is not accepted. `percent_complete` 0–100, the Update Tasks fields (#1568) `actual_start` / `actual_finish` (a date as for `deadline`, an actual start taking the day's first working time and a finish its last, or `null` to clear) and `actual_duration` / `remaining_duration` (durations: setting one adjusts % Complete, the remaining duration also the task's duration; an actual finish completes the task; a `percent_complete` given with an actual finish date, `actual_duration` or `remaining_duration` must equal the % they make or is refused, and with only `actual_start` or a cleared finish it applies after them, a started task getting that share as its actual duration; a `duration` given with `remaining_duration` must equal the actual plus the remaining), `priority` 0–1000, `deadline` a date (`YYYY-MM-DD`, the day's last working time, or `YYYY-MM-DDTHH:MM`) or `null`, `constraint` `{type, date?}` with the type `ASAP`/`ALAP`/`MSO`/`MFO`/`SNET`/`SNLT`/`FNET`/`FNLT`, `task_type` `fixed-units`/`fixed-duration`/`fixed-work`, `effort_driven` and `milestone` booleans, `notes` text (`\n` line breaks), `calendar` a base calendar's name or uid or `null`, `ignore_resource_calendar` a boolean, `wbs` text, `earned_value_method` `percent-complete`/`physical-percent-complete`. `custom` (#1554) is an object of Project custom field names to values as typed in the cell (`{"Text1": "Civil", "Flag3": true, "Date1": "2026-03-09", "Duration1": "3d", "Cost1": "$1,400", "Number1": null}`; `null` or `""` clears): the field needs a definition in the plan, a formula field is refused, a lookup-table field takes one of its values. Every argument is checked and all apply in one undo step, or none does |
//! | `task.add` | `{after?, name?, duration?}` | the new task (without `duration`, 1 day, estimated when the plan's `NewTasksEstimated` is; inserted `after` a task that a finish-to-start link joins to the next, it is linked into that chain when the plan's `Autolink` is on, as it is by default) |
//! | `task.del` | `{uid}` | `{deleted, removed:[uid…]}` (a summary takes its subtree) |
//! | `link.add` | `{uid, pred, type?, lag?}` | the updated task (`lag` uses Predecessors cell spellings such as `4h`, `2ed`, `50%`; other duration spellings such as `1 month` are read as working-days lags) |
//! | `link.del` | `{uid, pred}` | the updated task |
//! | `find` | `{query, fields?}` | `{count, tasks:[…]}` |
//! | `assign.list` | `{uid?, resource?, fields?}` | `{count, assignments:[…]}`: every assignment, or those of task `uid` and/or of `resource` (a resource uid, or a name matched ignoring ASCII case) |
//! | `assign.get` | `{uid, fields?}` | one assignment (by its own uid) |
//! | `assign.fields` | — | `{count, fields:[name…]}`: the assignment field names `fields` can read |
//! | `assign.add` | `{task, resource, units?, work?, fields?}` | the new assignment (`resource` a uid, or a name; a new name is staged as a work resource; a new name that is a number or contains a comma is refused: pass a uid as a number) |
//! | `assign.set` | `{uid, units?, work?, overtime_work?, contour?, rate_table?, delay?, actual_work? \| percent_work_complete?, fields?}` | the updated assignment, after its task is rescheduled |
//! | `assign.del` | `{uid}` | `{deleted, task}` |
//! | `resource.list` | `{}` | `{count, resources: [{uid, id, name, type, initials, group, code, email, material_label, max_units, standard_rate, overtime_rate, cost_per_use, accrue_at, base_calendar: {uid, name}, notes, assignments}]}` (#1588): `type` `work`/`material`/`cost`, `max_units` a fraction, the rates currency per hour (per unit for a material) and the cost per use in currency units, `accrue_at` `Start`/`End`/`Prorated`, `assignments` how many the resource has |
//! | `resource.get` | `{uid}` | one resource |
//! | `resource.add` | `{name, type?, initials?, group?, code?, email?, material_label?, max_units?, standard_rate?, overtime_rate?, cost_per_use?, accrue_at?, base_calendar?, notes?}` | the new resource (#1588): a work resource at 100%, costs prorated, unless given otherwise; the name is unique ignoring ASCII case, not a number, without a comma |
//! | `resource.set` | `{uid, name?, type?, …the same fields}` | the updated resource (#1588): `null` or `""` clears an optional text or rate; `type` changes only while the resource has no assignments; a rate edit keeps cost rate table A's current row, and a `max_units` edit the availability period in effect, in step |
//! | `resource.del` | `{uid}` | `{deleted, assignments: [uid…]}` (#1588): its assignments go with it, each as the Resource Names cell removes a name, and its own derived calendar when nothing else uses it; later resources move up one ID |
//! | `proj.get` | `{}` | the plan's settings (#1587): `{title, start, finish, status_date, current_date, calendar: {uid, name}, calendars: [{uid, name}], new_tasks}` (dates `YYYY-MM-DD HH:MM`, the status and current dates `null` when unset, `calendars` the base calendars, `new_tasks` `auto` or `manual`: how tasks added to the plan are scheduled, #1585) |
//! | `level.all` | `{}` | `{leveled: true, overallocated}` (#1589): show the plan leveled, as Resource > Level > Level All's display overlay; `overallocated` counts the work resources still booked past their capacity. Not an undo step: the overlay is a display setting |
//! | `level.clear` | `{}` | `{leveled: false, overallocated}` (#1589): show the plan as scheduled again. With `{uids: [uid…]}` or `{all: true}` (#1594) it is Clear Leveling as a plan edit instead: the tasks' stored `LevelingDelay` and pre-leveled dates are removed, one undo step, replying `{cleared, overallocated}` |
//! | `level.overallocations` | `{}` | `{count, resources: [{uid, name, max_units, peak_units, first}]}` (#1589): the work resources booked past their capacity on the plan as shown, each with its peak (1.0 = 100%) and the first overallocated instant, earliest first (Next Overallocation) |
//! | `level.selection` | `{uids: [uid…], options?}` | `{changed, delayed, overallocated, tasks: [{uid, leveling_delay_min, start}]}` (#1594): Level Selection as a plan edit. The leveler runs over the plan with only these tasks (a summary stands for its subtasks) free to move, every other task taking no delay of its own and booked at the dates the plan gives it now (an automatic one still follows its links once the delays are written, so the pass repeats while a round still changes the overallocation (a smaller overbooked extent, or conflicts not seen before) and drops a round that leaves the same conflicts; a conflict that remains is reported as overallocated, and a later pass resolves it only with the follower in its scope or with `clear_first` off), and writes each moved task's own `LevelingDelay` (elapsed minutes from the start its leveled predecessors drive it to, stored in tenths of a minute as Project does) and pre-leveled dates, one undo step. `changed` counts the tasks whose stored values changed (0 records no step), `delayed` those left with a delay, `tasks` every targeted leaf with its delay and start after the pass. The plan's Leveling Options apply |
//! | `level.resource` | `{resource, options?}` | the same reply (#1594): Level Resource, over the tasks the resource (a uid or a name) is assigned to. `options` (both verbs) is a `level.options` object applied to this pass only; the plan's options stay as they are. A task that only follows a delayed predecessor records no delay of its own, as Project does |
//! | `level.options` | `{order?, within_slack?, adjust_assignments?, split?, clear_first?}` | `{order, within_slack, adjust_assignments, split, clear_first}` (#1594): Project's Leveling Options as plan options, read with `{}`, set as one undo step (an unchanged setting records none). `order` is `ID`, `Standard` (the default: dependencies, then the plan's row order) or `Priority` (the highest priority first; priority 1000 is never leveled); `within_slack` never delays a task past its total slack, nor moves the project finish or turns a task's total slack negative (a task whose delay would do either, through a summary's percentage lag, is held, overallocated); `clear_first` (default true) drops the targeted tasks' stored delays before a pass, else it builds on them; `adjust_assignments` and `split` are kept with the plan, the leveler delaying whole tasks |
//! | `cal.list` | `{}` | `{count, calendars: [{uid, name, base: {uid, name} \| null, default, resources: [uid…], tasks: [uid…]}]}` (#1605), base calendars first |
//! | `cal.get` | `{calendar}` | the calendar (`calendar` a uid or a name) with its resolved `week` (`[{day, working, times: [["08:00","12:00"], …], stated}]`), `exceptions: [{index, name, from, to, working, times, recurring}]` and `work_weeks: [{index, name, from, to, week}]` |
//! | `cal.day` | `{calendar, date}` | `{date, working, times, based_on: {kind: exception \| work_week \| default, calendar, name}}`: Change Working Time's "Working times for" and "Based on" |
//! | `cal.add` | `{name, base?, copy?}` | the new calendar: derived from `base`, else a base calendar with Project's standard week, or a copy of the base calendar `copy` |
//! | `cal.set` | `{calendar, name?, base?, week?}` | the calendar after the change: `week` `{"Mon": {"working": true, "times": [["08:00","12:00"],["13:00","17:00"]]}, "Sat": {"working": false}}` for the days given (`null` for a derived calendar's day inherits the base again) |
//! | `cal.exception.set` | `{calendar, index?, name?, from, to, working?, times?}` | the calendar; no `index` adds a date-range exception (non-working unless `times`), an `index` replaces that one's dates and day, keeping its name unless `name` is given (`""` clears it; a repeating exception, `recurring` in `cal.get`, is read only) |
//! | `cal.exception.del` | `{calendar, index}` | the calendar (a repeating exception is read only) |
//! | `cal.workweek.set` | `{calendar, index?, name?, from, to, week}` | the calendar; `week` as `cal.set`'s, the days the work week states (`{}` for none; required, as a replace drops the days not given); the name is kept unless given |
//! | `cal.workweek.del` | `{calendar, index}` | the calendar |
//! | `cal.del` | `{calendar}` | `{deleted}`: refused for the project calendar, one a task or resource uses, or one another derives from |
//! | `proj.set` | `{title?, start?, status_date?, current_date?, calendar?, new_tasks?}` | the same reply after the change (#1587): dates `YYYY-MM-DD` (the project calendar's day start; a bare status date is the day's end, as Project sets it) or `YYYY-MM-DDTHH:MM`, `null` clearing a status or current date, `calendar` a base calendar's name or uid, `new_tasks` `auto` or `manual` (Task > Tasks > Mode, #1585; existing tasks keep their own mode). All apply in one undo step, or none does |
//!
//! `fields` is a list of Project field names (`["% Complete", "Baseline1
//! Finish"]`, matched ignoring ASCII case and surrounding space). The verbs
//! that reply with a task (`task.set`, `task.add`, `link.add`, `link.del`)
//! take it too, and check it before they edit anything. Each task
//! then carries `fields: {"<name as asked>": {text, value}}`: `text` as the
//! sheet shows it (`1 day`, `4 hrs`, `$1,400.00` in the plan's currency,
//! `Mon 3/2/26`, `NA`),
//! `value` underneath: dates
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
//! `units` a number or a percent string (`"50%"`); `work`, `overtime_work`,
//! `actual_work` and `delay` a number of hours or a duration (`"40h"`,
//! `"5d"`); `rate_table` `"A"`..`"E"`; `contour` Project's name (`"Back
//! Loaded"`, ASCII case ignored) or its code 0 (Flat) to 7 (Turtle), never
//! Contoured; `percent_work_complete` a whole number 0 to 100 or `"50%"`,
//! instead of `actual_work`.
//! The task is rescheduled by its type, as in Project: a units edit on a
//! Fixed Units task keeps the work and moves the duration; on Fixed Duration
//! the work follows the units, and units given together with work are
//! recomputed from the work. Overtime (a work resource's, at most the work;
//! a work edit clears it, so given together it applies after the work) is
//! priced at the overtime rate but not scheduled. A contour keeps the work,
//! units and duration and the assignment spans its task. Actual work (at
//! most the work, applied last) sets the actual start, at the whole work
//! the actual finish too, prices the actual cost by the resource's accrual,
//! and derives the remaining work and cost and % work complete; task
//! progress (its actual dates, % complete) is not derived from it. A
//! contour or an actual start ends a delayed assignment's extension of its
//! task past its duration. Every argument is checked before the edit, so
//! a rejected call leaves the editor untouched.
//!
//! `path_info` provides the common `proj.path` fields. File verbs (`proj.save`,
//! `proj.reload`, `proj.open`) belong to the host and are not dispatched here.

use ctlcore::json::Json;
use projcore::datetime::DateTime;
use projcore::editor::{
    AssignmentField, AssignmentPatch, DURATION_HINT, DayPatch, DaySource, EV_PERCENT_COMPLETE,
    EV_PHYSICAL_PERCENT_COMPLETE, Editor, Field, FieldReader, FieldValue, ResourcePatch,
    ResourceRef, TaskPatch, WEEKDAYS, assignment_dates, assignment_field_names, clock, day_end,
    day_start, duration_format_code, field_names, kind_name, parse_cell_date, parse_clock,
    parse_duration, parse_lag, parse_task_duration_unit, rate_table_letter, read_assignment_field,
    resource_base_calendar, weekday_index, work_contour_name,
};
use projcore::model::{
    AccrueAt, Assignment, Calendar, ConstraintType, DayWorking, LagFormat, LagUnit, LinkType,
    Predecessor, Rate, Resource, ResourceType, Task, TaskType, WorkingTime,
};
use projcore::schedule::overbookings;

/// Successful calls to these verbs signal agent editing activity.
pub const MUTATING: &[&str] = &[
    "proj.set",
    "task.set",
    "task.add",
    "task.del",
    "link.add",
    "link.del",
    "assign.add",
    "assign.set",
    "assign.del",
    "resource.add",
    "resource.set",
    "resource.del",
    "level.all",
    "level.clear",
    "level.selection",
    "level.resource",
    "level.options",
    "cal.add",
    "cal.set",
    "cal.exception.set",
    "cal.exception.del",
    "cal.workweek.set",
    "cal.workweek.del",
    "cal.del",
];

/// The settings `level.options` may set; anything else in its arguments
/// (`tab`, say) routes the call and changes nothing.
const LEVEL_OPTION_KEYS: [&str; 5] = [
    "order",
    "within_slack",
    "adjust_assignments",
    "split",
    "clear_first",
];

/// Whether a call changes the plan (#1594 r4, r6): a [`MUTATING`] verb,
/// except `level.options` naming no setting, which only reads the options.
pub fn mutates(verb: &str, args: &Json) -> bool {
    MUTATING.contains(&verb)
        && (verb != "level.options"
            || LEVEL_OPTION_KEYS
                .iter()
                .any(|k| args.get(k).is_some_and(|v| !matches!(v, Json::Null))))
}

/// Recognize editor verbs before a host resolves its target document.
pub fn is_editor_verb(verb: &str) -> bool {
    MUTATING.contains(&verb)
        || matches!(
            verb,
            "proj.get"
                | "task.list"
                | "task.get"
                | "task.fields"
                | "find"
                | "assign.list"
                | "assign.get"
                | "assign.fields"
                | "resource.list"
                | "resource.get"
                | "level.overallocations"
                | "cal.list"
                | "cal.get"
                | "cal.day"
        )
}

/// Apply a UI plugin command's ops (`projcore::uiplugin::Op`: an editor
/// verb and its args) as one `Editor::batch`: one undo step, and a refused
/// op (or one that is not an editor verb) rolls every op back and is the
/// error. The hosts call it for `yppx_ui_command`'s reply.
pub fn apply_ops(ed: &mut Editor, ops: &[projcore::uiplugin::Op]) -> Result<(), String> {
    if ops.is_empty() {
        return Ok(());
    }
    ed.batch(|ed| apply_ops_in(ed, ops))
}

/// [`apply_ops`] inside a batch the caller already holds (a dialog's OK
/// that applies its own fields in the same undo step).
pub fn apply_ops_in(ed: &mut Editor, ops: &[projcore::uiplugin::Op]) -> Result<(), String> {
    for (n, op) in ops.iter().enumerate() {
        let args = Json::parse(&op.args.to_string())
            .map_err(|e| format!("op {} ({}): bad args: {e}", n + 1, op.verb))?;
        match dispatch_editor(ed, &op.verb, &args) {
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("{}: {e}", op.verb)),
            None => return Err(format!("op {}: '{}' is not an editor verb", n + 1, op.verb)),
        }
    }
    Ok(())
}

/// Project verbs independent of the host's file handling. `None` means the
/// verb belongs to the host (or is unknown).
pub fn dispatch_editor(ed: &mut Editor, verb: &str, args: &Json) -> Option<Result<Json, String>> {
    Some(match verb {
        "proj.get" => Ok(proj_get(ed)),
        "proj.set" => proj_set(ed, args),
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
        "resource.list" => Ok(resource_list(ed)),
        "resource.get" => resource_get(ed, args),
        "resource.add" => resource_add(ed, args),
        "resource.set" => resource_set(ed, args),
        "resource.del" => resource_del(ed, args),
        "level.all" => Ok(level_set(ed, true)),
        "level.clear" => level_clear(ed, args),
        "level.overallocations" => Ok(level_overallocations(ed)),
        "level.selection" => level_edit(ed, args, false),
        "level.resource" => level_edit(ed, args, true),
        "level.options" => level_options(ed, args),
        "cal.list" => Ok(cal_list(ed)),
        "cal.get" => cal_get(ed, args),
        "cal.day" => cal_day(ed, args),
        "cal.add" => cal_add(ed, args),
        "cal.set" => cal_set(ed, args),
        "cal.exception.set" => cal_exception_set(ed, args),
        "cal.exception.del" => cal_exception_del(ed, args),
        "cal.workweek.set" => cal_workweek_set(ed, args),
        "cal.workweek.del" => cal_workweek_del(ed, args),
        "cal.del" => cal_del(ed, args),
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

/// The grid's sort and AutoFilter as both hosts report them (#1533):
/// `filter` (`All Tasks`, or `AutoFilter` while a column is filtered),
/// `filters` (`[{column, values}]`, in the order they were filtered), `sort`
/// (`ID`, or the sorted column) and `sort_descending`.
pub fn view_fields(ed: &Editor) -> Vec<(&'static str, Json)> {
    let filters = ed
        .filtered_fields()
        .into_iter()
        .filter_map(|field| {
            let values = ed.filter(field)?;
            Some(Json::obj(vec![
                ("column", Json::Str(field.name())),
                (
                    "values",
                    Json::Arr(values.iter().cloned().map(Json::Str).collect()),
                ),
            ]))
        })
        .collect();
    vec![
        (
            "filter",
            Json::Str(if ed.is_filtered() {
                "AutoFilter".into()
            } else {
                "All Tasks".into()
            }),
        ),
        ("filters", Json::Arr(filters)),
        (
            "sort",
            Json::Str(ed.sort().map_or_else(|| "ID".into(), |s| s.field.name())),
        ),
        (
            "sort_descending",
            Json::Bool(ed.sort().is_some_and(|s| s.descending)),
        ),
    ]
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
        ("new_tasks", new_tasks_json(ed)),
        ("leveled", Json::Bool(ed.leveled())),
    ])
}

// ---------------------------------------------------------------------------
// Leveling (#1589): the display overlay, as Resource > Level switches it
// ---------------------------------------------------------------------------

/// The work resources booked past their capacity on the plan as shown.
fn shown_overbookings(ed: &Editor) -> Vec<(i32, projcore::schedule::Overbooking)> {
    let mut over: Vec<_> = overbookings(ed.project(), |uid| {
        Some((ed.disp_start(uid)?, ed.disp_finish(uid)?))
    })
    .into_iter()
    .collect();
    over.sort_by_key(|(uid, o)| (o.first, *uid));
    over
}

/// `level.all` / `level.clear`: show the plan leveled or as scheduled.
fn level_set(ed: &mut Editor, on: bool) -> Json {
    ed.set_leveled(on);
    Json::obj(vec![
        ("leveled", Json::Bool(ed.leveled())),
        (
            "overallocated",
            Json::Num(shown_overbookings(ed).len() as f64),
        ),
    ])
}

/// `level.clear`: with `uids` or `all`, Clear Leveling as a plan edit
/// (#1594); else the overlay off (#1589).
fn level_clear(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let all = match args.get("all") {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(_) => return Err("'all' must be true or false".into()),
    };
    let uids = match args.get("uids") {
        None | Some(Json::Null) => None,
        Some(Json::Arr(items)) => Some(
            items
                .iter()
                .map(|u| {
                    u.as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(|| "'uids' must be task uids".to_string())
                })
                .collect::<Result<Vec<i32>, String>>()?,
        ),
        Some(_) => return Err("'uids' must be a list of task uids".into()),
    };
    if !all && uids.is_none() {
        return Ok(level_set(ed, false));
    }
    let cleared = ed.clear_leveling(uids.as_deref().filter(|_| !all))?;
    // The committed plan's conflicts (#1594 r6), not the overlay's: the
    // overlay re-levels what the edit just uncovered.
    Ok(Json::obj(vec![
        ("cleared", Json::Num(cleared as f64)),
        (
            "overallocated",
            Json::Num(ed.committed_overallocated() as f64),
        ),
    ]))
}

/// `level.selection` / `level.resource` (#1594): Level Selection or Level
/// Resource as a plan edit, one undo step.
fn level_edit(ed: &mut Editor, args: &Json, by_resource: bool) -> Result<Json, String> {
    use projcore::leveling::LevelTarget;
    let target = if by_resource {
        let r = resource_arg(args)?.ok_or("'resource' is required: a uid or a name")?;
        let uid = match r {
            ResourceRef::Uid(uid) => uid,
            ResourceRef::Name(name) => ed
                .project()
                .resources
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name.trim()))
                .map(|r| r.uid)
                .ok_or_else(|| format!("no resource named '{name}'"))?,
        };
        LevelTarget::Resource(uid)
    } else {
        let uids = match args.get("uids") {
            Some(Json::Arr(items)) => items
                .iter()
                .map(|u| {
                    u.as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(|| "'uids' must be task uids".to_string())
                })
                .collect::<Result<Vec<i32>, String>>()?,
            _ => return Err("'uids' is required: the task uids to level".into()),
        };
        LevelTarget::Selection(uids)
    };
    // Options for this pass only (#1594 r1), over the plan's.
    let options = match args.get("options") {
        None | Some(Json::Null) => None,
        Some(o @ Json::Obj(_)) => {
            let o = projcore::json::Json::parse(&o.to_string())
                .map_err(|e| format!("'options': {e}"))?;
            Some(ed.level_options().updated(&o, "options")?)
        }
        Some(_) => return Err("'options' must be an object".into()),
    };
    let out = ed.level_tasks_with(&target, options.as_ref())?;
    // The leaves the pass targeted, in row order, after the pass.
    let tasks: Vec<Json> = out
        .targeted
        .iter()
        .filter_map(|&uid| {
            let t = ed.project().task(uid)?;
            if t.summary || t.is_null {
                return None;
            }
            Some(Json::obj(vec![
                ("uid", Json::Num(f64::from(uid))),
                (
                    "leveling_delay_min",
                    Json::Num(t.leveling_delay.unwrap_or(0) as f64 / 10.),
                ),
                (
                    "start",
                    // The committed schedule's start (#1594 r4), not the
                    // display overlay's.
                    ed.schedule()
                        .get(uid)
                        .map_or(Json::Null, |r| Json::Str(dt_str(r.early_start))),
                ),
            ]))
        })
        .collect();
    Ok(Json::obj(vec![
        ("changed", Json::Num(out.changed as f64)),
        ("delayed", Json::Num(out.delayed as f64)),
        ("overallocated", Json::Num(out.overallocated as f64)),
        ("tasks", Json::Arr(tasks)),
    ]))
}

/// `level.options` (#1594): read, or set as one undo step, the plan's
/// Leveling Options.
fn level_options(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    // The options speak projcore's JSON; this crate's is ctlcore's.
    let args = projcore::json::Json::parse(&args.to_string())
        .map_err(|e| format!("level.options: {e}"))?;
    let options = ed.level_options().updated(&args, "level.options")?;
    ed.set_level_options(&options)?;
    Json::parse(&options.to_json().to_string()).map_err(|e| format!("level.options: {e}"))
}

/// `level.overallocations`: each overallocated work resource with its peak
/// and the first overallocated instant, earliest first.
fn level_overallocations(ed: &Editor) -> Json {
    let proj = ed.project();
    let resources: Vec<Json> = shown_overbookings(ed)
        .into_iter()
        .filter_map(|(uid, o)| {
            let r = proj.resources.iter().find(|r| r.uid == uid)?;
            Some(Json::obj(vec![
                ("uid", Json::Num(f64::from(uid))),
                ("name", Json::Str(r.name.clone())),
                (
                    "max_units",
                    Json::Num(if r.max_units > 0.0 { r.max_units } else { 1.0 }),
                ),
                ("peak_units", Json::Num(o.peak)),
                ("first", Json::Str(dt_str(o.first))),
            ]))
        })
        .collect();
    Json::obj(vec![
        ("count", Json::Num(resources.len() as f64)),
        ("resources", Json::Arr(resources)),
    ])
}

/// The plan's mode for new tasks (#1585): `auto` or `manual`.
fn new_tasks_json(ed: &Editor) -> Json {
    Json::Str(
        if ed.project().new_tasks_are_manual {
            "manual"
        } else {
            "auto"
        }
        .into(),
    )
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
        // The Milestone field, not Task::is_milestone: a summary whose stored
        // duration is 0 is no milestone, as the field and the grid read it.
        (
            "milestone",
            Json::Bool(reader.read(t, Field::Milestone).value == FieldValue::Bool(true)),
        ),
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
    // A stored leveling delay (#1594), in elapsed minutes, when the task
    // has one.
    if let Some(d) = t.leveling_delay.filter(|d| *d != 0) {
        fields.push(("leveling_delay_min", Json::Num(d as f64 / 10.)));
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

/// The Task Information fields `task.set` takes beside the patch (#1550),
/// parsed before anything is applied.
#[derive(Default)]
struct TaskInfoArgs {
    /// Start and Finish as typed in the cell (#1570): resolved by the
    /// editor's setters against the task as the call leaves it, applied after
    /// `manual`, `duration`, `milestone` and the calendar, before the actuals
    /// and `constraint`. Bare days of the other date fields resolve on the
    /// task's calendar before a `calendar` in the same call.
    start: Option<Typed>,
    finish: Option<Typed>,
    percent_complete: Option<i32>,
    /// The Update Tasks fields (#1568); `Some(None)` clears a date.
    actual_start: Option<Option<DateTime>>,
    actual_finish: Option<Option<DateTime>>,
    actual_duration: Option<i64>,
    remaining_duration: Option<i64>,
    priority: Option<i32>,
    /// `Some(None)` clears the deadline.
    deadline: Option<Option<DateTime>>,
    constraint: Option<(ConstraintType, Option<DateTime>)>,
    task_type: Option<TaskType>,
    effort_driven: Option<bool>,
    notes: Option<String>,
    milestone: Option<bool>,
    /// Project's Marked flag (#1667).
    marked: Option<bool>,
    /// `Some(None)` is the project's calendar.
    calendar: Option<Option<i32>>,
    ignore_resource_calendar: Option<bool>,
    wbs: Option<String>,
    earned_value_method: Option<i32>,
    /// Custom fields by Project's name, as typed (#1554).
    custom: Vec<(Field, String)>,
}

/// `YYYY-MM-DDTHH:MM` as typed. `parse_mspdi` alone rolls 2026-02-30 over to
/// March 2, so the date half must be a real calendar date.
fn instant(text: &str) -> Option<DateTime> {
    parse_cell_date(text.split_once('T')?.0).ok()?;
    DateTime::parse_mspdi(text)
}

/// A typed Start or Finish: a bare day, which the cell's setter resolves to
/// the task calendar's first or last working time, or an exact instant.
#[derive(Clone, Copy)]
enum Typed {
    Day(DateTime),
    At(DateTime),
}

/// The refusals of the Entry table's cells and the Task Information dialog.
const SUMMARY_DATES_READ_ONLY: &str = "Summary dates and duration are read-only";
const START_OR_FINISH: &str =
    "Change Start or Finish, not both, on an auto-scheduled task; change Duration for the span";

impl TaskInfoArgs {
    fn any(&self) -> bool {
        self.start.is_some()
            || self.finish.is_some()
            || self.percent_complete.is_some()
            || self.actual_start.is_some()
            || self.actual_finish.is_some()
            || self.actual_duration.is_some()
            || self.remaining_duration.is_some()
            || self.priority.is_some()
            || self.deadline.is_some()
            || self.constraint.is_some()
            || self.task_type.is_some()
            || self.effort_driven.is_some()
            || self.notes.is_some()
            || self.milestone.is_some()
            || self.marked.is_some()
            || self.calendar.is_some()
            || self.ignore_resource_calendar.is_some()
            || self.wbs.is_some()
            || self.earned_value_method.is_some()
            || !self.custom.is_empty()
    }

    /// Read the fields from `args`, for task `uid` of `ed`'s plan (dates
    /// are resolved in the task's calendar).
    fn parse(ed: &Editor, uid: i32, args: &Json) -> Result<TaskInfoArgs, String> {
        let task = ed
            .project()
            .task(uid)
            .ok_or_else(|| format!("no task with uid {uid}"))?;
        let proj = ed.project();
        let whole = |key: &str| -> Result<Option<i32>, String> {
            args.get(key)
                .map(|v| {
                    v.as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(|| format!("'{key}' must be a whole number"))
                })
                .transpose()
        };
        let flag = |key: &str| -> Result<Option<bool>, String> {
            args.get(key)
                .map(|v| {
                    v.as_bool()
                        .ok_or_else(|| format!("'{key}' must be true or false"))
                })
                .transpose()
        };
        let text = |key: &str| -> Result<Option<String>, String> {
            args.get(key)
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("'{key}' must be text"))
                })
                .transpose()
        };
        // A date: `YYYY-MM-DD` is the day's first working time, or its last
        // when `ends_day`; `YYYY-MM-DDTHH:MM` is taken as typed.
        let stamp = |key: &str, s: &str, ends_day: bool| -> Result<DateTime, String> {
            let bad = || format!("'{key}' must be a date, YYYY-MM-DD or YYYY-MM-DDTHH:MM");
            if s.contains('T') {
                return instant(s).ok_or_else(bad);
            }
            let day = parse_cell_date(s).map_err(|_| bad())?;
            Ok(if ends_day {
                day_end(proj, task, day)
            } else {
                day_start(proj, task, day)
            })
        };
        // A date or null; an actual start is the day's first working time, an
        // actual finish and a deadline its last.
        let date_or_null =
            |key: &str, ends_day: bool| -> Result<Option<Option<DateTime>>, String> {
                match args.get(key) {
                    None => Ok(None),
                    Some(Json::Null) => Ok(Some(None)),
                    Some(Json::Str(s)) if s.trim().is_empty() => Ok(Some(None)),
                    Some(Json::Str(s)) => Ok(Some(Some(stamp(key, s.trim(), ends_day)?))),
                    Some(_) => Err(format!("'{key}' must be a date or null")),
                }
            };
        // Start and Finish are a date, never null: the cell cannot clear one.
        let typed = |key: &str| -> Result<Option<Typed>, String> {
            let Some(v) = args.get(key) else {
                return Ok(None);
            };
            let bad = || format!("'{key}' must be a date, YYYY-MM-DD or YYYY-MM-DDTHH:MM");
            let s = v.as_str().ok_or_else(bad)?.trim();
            if s.contains('T') {
                return instant(s).map(|d| Some(Typed::At(d))).ok_or_else(bad);
            }
            parse_cell_date(s)
                .map(|d| Some(Typed::Day(d)))
                .map_err(|_| bad())
        };
        let start = typed("start")?;
        let finish = typed("finish")?;
        let deadline = date_or_null("deadline", true)?;
        let actual_start = date_or_null("actual_start", false)?;
        let actual_finish = date_or_null("actual_finish", true)?;
        let duration_arg = |key: &str| -> Result<Option<i64>, String> {
            args.get(key)
                .map(|v| {
                    let text = v
                        .as_str()
                        .ok_or_else(|| format!("'{key}' must be a duration"))?;
                    parse_duration(text, proj)
                        .ok_or_else(|| format!("Couldn't read {key} '{text}' ({DURATION_HINT})"))
                })
                .transpose()
        };
        let constraint = match args.get("constraint") {
            None => None,
            Some(c) => {
                let kind = c.get_str("type").ok_or("'constraint' needs a 'type'")?;
                let kind = [
                    ConstraintType::AsSoonAsPossible,
                    ConstraintType::AsLateAsPossible,
                    ConstraintType::MustStartOn,
                    ConstraintType::MustFinishOn,
                    ConstraintType::StartNoEarlierThan,
                    ConstraintType::StartNoLaterThan,
                    ConstraintType::FinishNoEarlierThan,
                    ConstraintType::FinishNoLaterThan,
                ]
                .into_iter()
                .find(|k| k.abbrev().eq_ignore_ascii_case(kind.trim()))
                .ok_or_else(|| {
                    format!(
                        "'constraint.type' is one of ASAP, ALAP, MSO, MFO, SNET, SNLT, FNET, FNLT, not '{kind}'"
                    )
                })?;
                let ends_day = matches!(
                    kind,
                    ConstraintType::FinishNoLaterThan | ConstraintType::MustFinishOn
                );
                let date = match c.get("date") {
                    None | Some(Json::Null) => None,
                    Some(Json::Str(s)) if s.trim().is_empty() => None,
                    Some(Json::Str(s)) => Some(stamp("constraint.date", s.trim(), ends_day)?),
                    Some(_) => return Err("'constraint.date' must be a date or null".into()),
                };
                Some((kind, date))
            }
        };
        let task_type = match args.get_str("task_type") {
            None => None,
            Some(t) => Some(match t.trim().to_ascii_lowercase().as_str() {
                "fixed-units" | "fixed units" => TaskType::FixedUnits,
                "fixed-duration" | "fixed duration" => TaskType::FixedDuration,
                "fixed-work" | "fixed work" => TaskType::FixedWork,
                other => {
                    return Err(format!(
                        "'task_type' is fixed-units, fixed-duration or fixed-work, not '{other}'"
                    ));
                }
            }),
        };
        let calendar = match args.get("calendar") {
            None => None,
            Some(Json::Null) => Some(None),
            Some(Json::Str(s)) if s.trim().is_empty() || s.trim().eq_ignore_ascii_case("none") => {
                Some(None)
            }
            Some(Json::Str(name)) => {
                let cal = proj
                    .calendars
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
                    .ok_or_else(|| format!("no calendar named '{name}'"))?;
                Some(Some(cal.uid))
            }
            Some(Json::Num(n)) => Some(Some(
                i32::try_from(*n as i64).map_err(|_| "'calendar' must be a calendar uid")?,
            )),
            Some(_) => return Err("'calendar' must be a calendar name, uid or null".into()),
        };
        let custom = match args.get("custom") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Obj(pairs)) => pairs
                .iter()
                .map(|(name, value)| {
                    let field = Field::parse(name)?;
                    if !matches!(field, Field::Custom(..)) {
                        return Err(format!("'custom' takes custom fields (Text1, Number2, Flag3, Date1, Start1, Finish1, Duration1, Cost1, ...), not '{name}'"));
                    }
                    let text = match value {
                        Json::Str(s) => s.clone(),
                        Json::Null => String::new(),
                        Json::Bool(b) => b.to_string(),
                        Json::Num(n) => n.to_string(),
                        other => {
                            return Err(format!("'custom.{name}' must be text, a number, a boolean or null, not {other}"));
                        }
                    };
                    Ok((field, text))
                })
                .collect::<Result<Vec<_>, String>>()?,
            Some(_) => return Err("'custom' must be an object of field name to value".into()),
        };
        let earned_value_method = match args.get_str("earned_value_method") {
            None => None,
            Some(m) => Some(match m.trim().to_ascii_lowercase().as_str() {
                "percent-complete" | "% complete" | "0" => EV_PERCENT_COMPLETE,
                "physical-percent-complete" | "physical % complete" | "1" => {
                    EV_PHYSICAL_PERCENT_COMPLETE
                }
                other => {
                    return Err(format!(
                        "'earned_value_method' is percent-complete or physical-percent-complete, not '{other}'"
                    ));
                }
            }),
        };
        Ok(TaskInfoArgs {
            start,
            finish,
            percent_complete: whole("percent_complete")?,
            actual_start,
            actual_finish,
            actual_duration: duration_arg("actual_duration")?,
            remaining_duration: duration_arg("remaining_duration")?,
            priority: whole("priority")?,
            deadline,
            constraint,
            task_type,
            effort_driven: flag("effort_driven")?,
            notes: text("notes")?,
            milestone: flag("milestone")?,
            marked: flag("marked")?,
            calendar,
            ignore_resource_calendar: flag("ignore_resource_calendar")?,
            wbs: text("wbs")?,
            earned_value_method,
            custom,
        })
    }

    fn apply_calendar(&self, ed: &mut Editor, uid: i32) -> Result<(), String> {
        if let Some(cal) = self.calendar {
            ed.set_task_calendar(uid, cal)?;
        }
        if let Some(on) = self.ignore_resource_calendar {
            ed.set_ignore_resource_calendar(uid, on)?;
        }
        Ok(())
    }

    /// Start and Finish, as the cell sets them.
    fn apply_dates(&self, ed: &mut Editor, uid: i32) -> Result<(), String> {
        if self.start.is_some() || self.finish.is_some() {
            let task = ed
                .project()
                .task(uid)
                .ok_or_else(|| format!("no task with uid {uid}"))?;
            if task.summary && !task.manual {
                return Err(SUMMARY_DATES_READ_ONLY.into());
            }
            // Judged as the setters do: typing into a blank row of a plan
            // whose new tasks are manual makes it manual.
            let manual = task.manual || (task.is_null && ed.project().new_tasks_are_manual);
            if self.start.is_some() && self.finish.is_some() && !manual {
                return Err(START_OR_FINISH.into());
            }
        }
        match self.start {
            Some(Typed::Day(day)) => ed.set_start(uid, day)?,
            Some(Typed::At(at)) => ed.set_start_at(uid, at)?,
            None => {}
        }
        match self.finish {
            Some(Typed::Day(day)) => ed.set_finish(uid, day)?,
            Some(Typed::At(at)) => ed.set_finish_at(uid, at)?,
            None => {}
        }
        Ok(())
    }

    /// Apply the fields, in the dialog's order: type and effort driven
    /// first (a resource added later rescales by them), the constraint after
    /// the dates.
    fn apply(&self, ed: &mut Editor, uid: i32) -> Result<(), String> {
        if let Some(kind) = self.task_type {
            ed.set_task_type(uid, kind)?;
        }
        if let Some(on) = self.effort_driven {
            ed.set_effort_driven(uid, on)?;
        }
        if let Some(on) = self.milestone {
            ed.set_milestone(uid, on)?;
        }
        // The calendar first, so a bare day resolves on the one the call ends
        // with; a blank row has no properties until a date makes it a task.
        let blank = ed.project().task(uid).is_some_and(|t| t.is_null);
        if !blank {
            self.apply_calendar(ed, uid)?;
        }
        self.apply_dates(ed, uid)?;
        if blank {
            self.apply_calendar(ed, uid)?;
        }
        // A finish, an actual duration or a remaining duration derive
        // % Complete; a start alone, or clearing the finish, leave it to
        // `percent_complete`.
        let derives = matches!(self.actual_finish, Some(Some(_)))
            || self.actual_duration.is_some()
            || self.remaining_duration.is_some();
        // Moving both dates later: the start is checked against the stored
        // finish, so the finish goes first.
        let stored_finish = ed.project().task(uid).and_then(|t| t.actual_finish);
        let finish_first = match (self.actual_start, self.actual_finish) {
            (Some(Some(start)), Some(_)) => stored_finish.is_some_and(|f| start > f),
            _ => false,
        };
        if finish_first && let Some(finish) = self.actual_finish {
            ed.set_actual_finish(uid, finish)?;
        }
        if let Some(start) = self.actual_start {
            ed.set_actual_start(uid, start)?;
        }
        if !finish_first && let Some(finish) = self.actual_finish {
            ed.set_actual_finish(uid, finish)?;
        }
        if let Some(minutes) = self.actual_duration {
            ed.set_actual_duration(uid, minutes)?;
        }
        if let Some(minutes) = self.remaining_duration {
            ed.set_remaining_duration(uid, minutes)?;
        }
        if let Some(p) = self.percent_complete {
            if derives {
                // The actuals say how far the task is; a different
                // percent_complete is a contradiction, never an override.
                let now = ed.project().task(uid).and_then(|t| t.percent_complete);
                if i32::from(now.unwrap_or(0)) != p {
                    return Err(format!(
                        "percent_complete {p} contradicts the actuals, which make it {}",
                        now.unwrap_or(0)
                    ));
                }
            } else {
                // A started task keeps its actual and remaining duration at
                // the percent's share of the duration; see the method.
                ed.set_percent_complete_synced(uid, p)?;
            }
        }
        if let Some(p) = self.priority {
            ed.set_priority(uid, p)?;
        }
        if let Some((kind, date)) = self.constraint {
            ed.set_constraint_typed(uid, kind, date)?;
        }
        if let Some(d) = self.deadline {
            ed.set_deadline(uid, d)?;
        }
        if let Some(wbs) = &self.wbs {
            ed.set_wbs(uid, wbs)?;
        }
        if let Some(m) = self.earned_value_method {
            ed.set_earned_value_method(uid, m)?;
        }
        if let Some(notes) = &self.notes {
            ed.set_notes(uid, &projcore::text::notes_from_line(notes))?;
        }
        if let Some(on) = self.marked {
            ed.set_marked(uid, on)?;
        }
        for (field, text) in &self.custom {
            ed.set_custom_field(uid, *field, text)?;
        }
        Ok(())
    }
}

fn task_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = uid_arg(args, "uid")?;
    let asked = fields_arg(args)?;
    let info = TaskInfoArgs::parse(ed, uid, args)?;
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
    let patch = TaskPatch {
        name: args.get_str("name").map(str::to_string),
        duration_min: duration.map(|d| d.0),
        level,
        manual,
        estimated: duration.map(|d| d.1),
        duration_format: duration.and_then(|d| duration_format_code(d.2)),
    };
    let has_patch = patch.name.is_some()
        || patch.duration_min.is_some()
        || patch.level.is_some()
        || patch.manual.is_some();
    if !has_patch && !info.any() {
        return Err(
            "task.set needs at least one of 'name', 'duration', 'level', 'manual', 'start', 'finish' or a Task Information field"
                .into(),
        );
    }
    // One undo step for the patch and every field; a refusal undoes all.
    ed.batch(|ed| {
        if has_patch {
            ed.update_task(uid, patch)?;
        }
        info.apply(ed, uid)?;
        // A remaining duration sets the duration; a different one typed
        // beside it is a contradiction.
        if let (Some(typed), Some(_)) = (duration, info.remaining_duration) {
            let now = ed.project().task(uid).map_or(typed.0, |t| t.duration_min);
            if now != typed.0 {
                return Err(format!(
                    "'duration' contradicts 'remaining_duration', which makes the duration {now} minutes"
                ));
            }
        }
        Ok(())
    })?;
    task_reply(ed, uid, asked.as_deref())
}

/// `proj.get`: the plan's settings (#1587).
fn proj_get(ed: &Editor) -> Json {
    let proj = ed.project();
    let option_date = |name: &str| {
        proj.option(name)
            .and_then(DateTime::parse_mspdi)
            .map_or(Json::Null, |d| Json::Str(dt_str(d)))
    };
    let calendar = |c: &projcore::model::Calendar| {
        Json::obj(vec![
            ("uid", Json::Num(f64::from(c.uid))),
            ("name", Json::Str(c.name.clone())),
        ])
    };
    Json::obj(vec![
        ("title", Json::Str(proj.title.clone())),
        ("start", Json::Str(dt_str(ed.schedule().project_start))),
        ("finish", Json::Str(dt_str(ed.schedule().project_finish))),
        ("status_date", option_date("StatusDate")),
        ("current_date", option_date("CurrentDate")),
        (
            "calendar",
            proj.calendar(proj.default_calendar_uid)
                .map_or(Json::Null, calendar),
        ),
        (
            "calendars",
            Json::Arr(
                proj.calendars
                    .iter()
                    .filter(|c| c.base_calendar_uid.is_none())
                    .map(calendar)
                    .collect(),
            ),
        ),
        ("new_tasks", new_tasks_json(ed)),
    ])
}

/// A `proj.set` date: `YYYY-MM-DD` at the project calendar's day start (its
/// day end for the status date, as Project sets one typed bare), or
/// `YYYY-MM-DDTHH:MM`.
fn plan_date(name: &str, text: &str, ed: &Editor, ends_day: bool) -> Result<DateTime, String> {
    let text = text.trim();
    if text.contains('T') {
        return DateTime::parse_mspdi(text)
            .ok_or_else(|| format!("'{name}' is YYYY-MM-DD or YYYY-MM-DDTHH:MM, not '{text}'"));
    }
    let day = parse_cell_date(text)
        .map_err(|_| format!("'{name}' is YYYY-MM-DD or YYYY-MM-DDTHH:MM, not '{text}'"))?;
    let probe = Task::default();
    Ok(if ends_day {
        day_end(ed.project(), &probe, day)
    } else {
        day_start(ed.project(), &probe, day)
    })
}

/// `proj.set`: the plan's settings as one undo step (#1587).
fn proj_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let title = args.get_str("title").map(str::to_string);
    let start = match args.get("start") {
        None | Some(Json::Null) => None,
        Some(Json::Str(s)) => Some(plan_date("start", s, ed, false)?),
        Some(_) => return Err("'start' must be a date".into()),
    };
    let dated = |key: &str| -> Result<Option<Option<DateTime>>, String> {
        match args.get(key) {
            None => Ok(None),
            Some(Json::Null) => Ok(Some(None)),
            Some(Json::Str(s)) if s.trim().is_empty() => Ok(Some(None)),
            Some(Json::Str(s)) => Ok(Some(Some(plan_date(key, s, ed, key == "status_date")?))),
            Some(_) => Err(format!("'{key}' must be a date or null")),
        }
    };
    let status_date = dated("status_date")?;
    let current_date = dated("current_date")?;
    let calendar = match args.get("calendar") {
        None | Some(Json::Null) => None,
        Some(Json::Str(name)) => {
            let name = name.trim();
            let found = ed
                .project()
                .calendars
                .iter()
                .find(|c| c.base_calendar_uid.is_none() && c.name.eq_ignore_ascii_case(name))
                .map(|c| c.uid)
                .or_else(|| name.parse::<i32>().ok());
            Some(found.ok_or_else(|| format!("no base calendar named '{name}'"))?)
        }
        Some(Json::Num(n)) => Some(*n as i32),
        Some(_) => return Err("'calendar' must be a base calendar's name or uid".into()),
    };
    // Task > Tasks > Mode (#1585). Project's manual variants (On Project
    // Start Date, On Project Finish Date, Without Dates) are not in the
    // model, which places a new manual task at the project start.
    let new_tasks = match args.get("new_tasks") {
        None | Some(Json::Null) => None,
        Some(Json::Str(mode)) => match mode.trim().to_ascii_lowercase().as_str() {
            "auto" | "auto scheduled" => Some(false),
            "manual" | "manually scheduled" => Some(true),
            _ => return Err("'new_tasks' is \"auto\" or \"manual\"".into()),
        },
        Some(_) => return Err("'new_tasks' is \"auto\" or \"manual\"".into()),
    };
    if title.is_none()
        && start.is_none()
        && status_date.is_none()
        && current_date.is_none()
        && calendar.is_none()
        && new_tasks.is_none()
    {
        return Err(
            "proj.set needs at least one of 'title', 'start', 'status_date', 'current_date', 'calendar' or 'new_tasks'"
                .into(),
        );
    }
    ed.batch(|ed| {
        if let Some(manual) = new_tasks {
            ed.set_new_tasks_manual(manual);
        }
        if let Some(t) = &title {
            ed.set_title(t)?;
        }
        if let Some(s) = start {
            ed.set_project_start(s)?;
        }
        if let Some(d) = status_date {
            ed.set_status_date(d)?;
        }
        if let Some(d) = current_date {
            ed.set_current_date(d)?;
        }
        if let Some(c) = calendar {
            ed.set_default_calendar(c)?;
        }
        Ok(())
    })?;
    Ok(proj_get(ed))
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
    fraction_arg(args, "units")
}

/// `key` as a fraction: a number, or a percent string (`"50%"`).
fn fraction_arg(args: &Json, key: &str) -> Result<Option<f64>, String> {
    let Some(u) = args.get(key) else {
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
        .ok_or_else(|| format!("couldn't read '{key}' (try 0.5 or \"50%\")"))
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

/// `contour` as a work contour code: Project's name, ASCII case ignored,
/// or the code itself. Contoured reads as 8, which the editor refuses with
/// its reason.
fn contour_arg(args: &Json) -> Result<Option<u8>, String> {
    let Some(c) = args.get("contour") else {
        return Ok(None);
    };
    let code = match c.as_str().map(str::trim) {
        Some(name) => name.parse::<u8>().ok().or_else(|| {
            (0..=8).find(|&code| work_contour_name(Some(code)).eq_ignore_ascii_case(name))
        }),
        None => c.as_i64().and_then(|n| u8::try_from(n).ok()),
    };
    code.map(Some).ok_or_else(|| {
        "couldn't read 'contour' (a name such as \"Back Loaded\", or 0 to 7)".to_string()
    })
}

/// `key` as a whole percent, 0 to 100: a number or a percent string
/// (`"50%"`).
fn percent_arg(args: &Json, key: &str) -> Result<Option<u8>, String> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    let n = match v.as_str() {
        Some(text) => {
            let text = text.trim();
            let text = text.strip_suffix('%').map_or(text, str::trim_end);
            text.parse::<f64>().ok()
        }
        None => v.as_f64(),
    };
    let n = n
        .filter(|n| n.is_finite() && n.fract() == 0.0)
        .ok_or_else(|| format!("couldn't read '{key}' (a whole percent such as 50 or \"50%\")"))?;
    if (0.0..=100.0).contains(&n) {
        Ok(Some(n as u8))
    } else {
        Err(format!("'{key}' must be 0 to 100, not {n}"))
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
        overtime_work_min: hours_arg(ed, args, "overtime_work")?,
        work_contour: contour_arg(args)?,
        actual_work_min: hours_arg(ed, args, "actual_work")?,
        percent_work_complete: percent_arg(args, "percent_work_complete")?,
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

// ---------------------------------------------------------------------------
// Calendars (#1605)
// ---------------------------------------------------------------------------

/// `{uid, name}` of a calendar.
fn cal_ref(c: &Calendar) -> Json {
    Json::obj(vec![
        ("uid", Json::Num(f64::from(c.uid))),
        ("name", Json::Str(c.name.clone())),
    ])
}

/// Working times as `[["08:00","12:00"], …]`.
fn times_json(times: &[WorkingTime]) -> Json {
    Json::Arr(
        times
            .iter()
            .map(|t| Json::Arr(vec![Json::Str(clock(t.from)), Json::Str(clock(t.to))]))
            .collect(),
    )
}

/// A week's seven days: `{day, working, times, stated}`; `stated` whether
/// the calendar states the day itself (a derived calendar inherits the
/// rest), `resolved` what the day works.
fn week_json(stated: &[Option<DayWorking>; 7], resolved: &[DayWorking; 7]) -> Json {
    Json::Arr(
        (0..7)
            .map(|d| {
                Json::obj(vec![
                    ("day", Json::Str(WEEKDAYS[d][..3].into())),
                    ("working", Json::Bool(resolved[d].working())),
                    ("times", times_json(&resolved[d].times)),
                    ("stated", Json::Bool(stated[d].is_some())),
                ])
            })
            .collect(),
    )
}

fn date_json(d: Option<DateTime>) -> Json {
    d.map_or(Json::Null, |d| {
        let p = d.parts();
        Json::Str(format!("{:04}-{:02}-{:02}", p.year, p.month, p.day))
    })
}

/// One calendar's listing: `cal.list`'s row.
fn calendar_row(ed: &Editor, c: &Calendar) -> Vec<(&'static str, Json)> {
    let proj = ed.project();
    vec![
        ("uid", Json::Num(f64::from(c.uid))),
        ("name", Json::Str(c.name.clone())),
        (
            "base",
            c.base_calendar_uid
                .and_then(|b| proj.calendar(b))
                .map_or(Json::Null, cal_ref),
        ),
        ("default", Json::Bool(proj.default_calendar_uid == c.uid)),
        (
            "resources",
            Json::Arr(
                proj.resources
                    .iter()
                    .filter(|r| r.calendar_uid == Some(c.uid))
                    .map(|r| Json::Num(f64::from(r.uid)))
                    .collect(),
            ),
        ),
        (
            "tasks",
            Json::Arr(
                proj.tasks
                    .iter()
                    .filter(|t| t.calendar_uid == Some(c.uid))
                    .map(|t| Json::Num(f64::from(t.uid)))
                    .collect(),
            ),
        ),
    ]
}

/// `cal.get`'s calendar: the row, the resolved week, the exceptions and
/// the work weeks.
fn calendar_json(ed: &Editor, c: &Calendar) -> Json {
    let proj = ed.project();
    let mut out = calendar_row(ed, c);
    let resolved = proj.resolved_week(c);
    out.push(("week", week_json(&c.week, &resolved)));
    out.push((
        "exceptions",
        Json::Arr(
            c.exceptions
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    Json::obj(vec![
                        ("index", Json::Num(i as f64)),
                        ("name", e.name.clone().map_or(Json::Null, Json::Str)),
                        ("from", date_json(e.from)),
                        ("to", date_json(e.to)),
                        ("working", Json::Bool(e.day.working())),
                        ("times", times_json(&e.day.times)),
                        ("recurring", Json::Bool(e.scheduled().is_none())),
                    ])
                })
                .collect(),
        ),
    ));
    out.push((
        "work_weeks",
        Json::Arr(
            c.work_weeks
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    // A work week's unstated day is shown from the calendar's
                    // default week chain (`stated: false`); what a date in the
                    // week works, a base's covering work week included, is
                    // `cal.day`'s answer.
                    let mut shown = resolved.clone();
                    for (d, day) in w.week.iter().enumerate() {
                        if let Some(day) = day {
                            shown[d] = day.clone();
                        }
                    }
                    Json::obj(vec![
                        ("index", Json::Num(i as f64)),
                        ("name", w.name.clone().map_or(Json::Null, Json::Str)),
                        ("from", date_json(w.from)),
                        ("to", date_json(w.to)),
                        ("week", week_json(&w.week, &shown)),
                    ])
                })
                .collect(),
        ),
    ));
    Json::obj(out)
}

/// The calendar `args` names: `calendar` as a uid or a name, or `uid`.
fn calendar_arg(ed: &Editor, args: &Json) -> Result<i32, String> {
    match args.get("calendar").or_else(|| args.get("uid")) {
        Some(Json::Num(_)) => {
            let uid = calendar_uid(args.get("calendar").or_else(|| args.get("uid")), "calendar")?;
            ed.project()
                .calendar(uid)
                .map(|_| uid)
                .ok_or_else(|| format!("no calendar with uid {uid}"))
        }
        Some(Json::Str(s)) => ed.calendar_by(s),
        _ => Err("needs a 'calendar' (a calendar uid or name)".into()),
    }
}

/// A calendar reference given as a number: a whole number in i32 range
/// (2.9 is not calendar 2).
fn calendar_uid(value: Option<&Json>, key: &str) -> Result<i32, String> {
    value
        .and_then(Json::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| format!("'{key}' must be a whole-number calendar uid or a name"))
}

/// An optional calendar reference under `key`, a uid or a name.
fn calendar_opt(ed: &Editor, args: &Json, key: &str) -> Result<Option<i32>, String> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Num(_)) => {
            let uid = calendar_uid(args.get(key), key)?;
            ed.project()
                .calendar(uid)
                .map(|_| Some(uid))
                .ok_or_else(|| format!("no calendar with uid {uid}"))
        }
        Some(Json::Str(s)) => ed.calendar_by(s).map(Some),
        Some(_) => Err(format!("'{key}' must be a calendar uid or name")),
    }
}

/// A day's working times from `[["08:00","12:00"], …]`.
fn times_arg(value: &Json) -> Result<Vec<WorkingTime>, String> {
    let bad = || "'times' is a list of [\"HH:MM\",\"HH:MM\"] pairs".to_string();
    let mut out = Vec::new();
    for pair in value.as_array().ok_or_else(bad)? {
        let pair = pair.as_array().ok_or_else(bad)?;
        let [from, to] = pair else {
            return Err(bad());
        };
        let from = from.as_str().and_then(parse_clock).ok_or_else(bad)?;
        let to = to.as_str().and_then(parse_clock).ok_or_else(bad)?;
        out.push(WorkingTime { from, to });
    }
    Ok(out)
}

/// A day's pattern from `{working?, times?}`: `times` given is the day,
/// `working: false` (or no times) a non-working day, `null` inherits.
/// Anything else is refused (a bare `true` or a times list is not a day).
fn day_arg(value: &Json) -> Result<DayPatch, String> {
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    if !matches!(value, Json::Obj(_)) {
        return Err("a day is {working?, times?} or null".into());
    }
    let working = match value.get("working") {
        None | Some(Json::Null) => None,
        Some(Json::Bool(b)) => Some(*b),
        Some(_) => return Err("'working' is true or false".into()),
    };
    let times = match value.get("times") {
        Some(t) if !matches!(t, Json::Null) => times_arg(t)?,
        _ => Vec::new(),
    };
    if working == Some(false) {
        return Ok(Some(DayWorking { times: Vec::new() }));
    }
    if working == Some(true) && times.is_empty() {
        return Ok(Some(DayWorking {
            times: Calendar::standard_week()[1].times.clone(),
        }));
    }
    Ok(Some(DayWorking { times }))
}

/// A week's days from `{"Mon": {...}, "Sat": null, …}`.
fn week_arg(args: &Json, key: &str) -> Result<Vec<(usize, DayPatch)>, String> {
    let Some(week) = args.get(key) else {
        return Ok(Vec::new());
    };
    let Json::Obj(days) = week else {
        return Err(format!("'{key}' is an object of weekdays"));
    };
    let mut out = Vec::new();
    for (day, pattern) in days {
        let d = weekday_index(day).ok_or_else(|| format!("'{day}' is not a weekday"))?;
        out.push((d, day_arg(pattern).map_err(|e| format!("'{day}': {e}"))?));
    }
    Ok(out)
}

/// `from` and `to` as dates.
fn range_args(args: &Json) -> Result<(DateTime, DateTime), String> {
    let date = |key: &str| {
        let text = args
            .get_str(key)
            .ok_or_else(|| format!("needs '{key}' (YYYY-MM-DD)"))?;
        parse_cell_date(text).map_err(|_| format!("'{key}' is not a date (YYYY-MM-DD)"))
    };
    Ok((date("from")?, date("to")?))
}

fn index_arg(args: &Json) -> Result<Option<usize>, String> {
    match args.get("index") {
        None | Some(Json::Null) => Ok(None),
        Some(v) => v
            .as_usize()
            .map(Some)
            .ok_or_else(|| "'index' must be a whole number".to_string()),
    }
}

fn cal_reply(ed: &Editor, uid: i32) -> Result<Json, String> {
    ed.project()
        .calendar(uid)
        .map(|c| calendar_json(ed, c))
        .ok_or_else(|| format!("no calendar with uid {uid}"))
}

fn cal_list(ed: &Editor) -> Json {
    let proj = ed.project();
    let mut cals: Vec<&Calendar> = proj.calendars.iter().collect();
    cals.sort_by_key(|c| c.base_calendar_uid.is_some());
    let rows: Vec<Json> = cals
        .iter()
        .map(|c| Json::obj(calendar_row(ed, c)))
        .collect();
    Json::obj(vec![
        ("count", Json::Num(rows.len() as f64)),
        ("calendars", Json::Arr(rows)),
    ])
}

fn cal_get(ed: &Editor, args: &Json) -> Result<Json, String> {
    cal_reply(ed, calendar_arg(ed, args)?)
}

fn cal_day(ed: &Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    let text = args.get_str("date").ok_or("needs a 'date' (YYYY-MM-DD)")?;
    let date = parse_cell_date(text).map_err(|_| format!("'{text}' is not a date (YYYY-MM-DD)"))?;
    let (times, source) = ed.calendar_day(uid, date)?;
    let (kind, calendar, name) = match source {
        DaySource::Exception { calendar, name } => ("exception", calendar, name),
        DaySource::WorkWeek { calendar, name } => ("work_week", calendar, name),
        DaySource::Default { calendar } => ("default", calendar, None),
    };
    Ok(Json::obj(vec![
        ("date", date_json(Some(date))),
        ("working", Json::Bool(!times.is_empty())),
        ("times", times_json(&times)),
        (
            "based_on",
            Json::obj(vec![
                ("kind", Json::Str(kind.into())),
                ("calendar", Json::Str(calendar)),
                ("name", name.map_or(Json::Null, Json::Str)),
            ]),
        ),
    ]))
}

fn cal_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let name = args
        .get_str("name")
        .ok_or("cal.add needs a 'name'")?
        .to_string();
    let base = calendar_opt(ed, args, "base")?;
    let copy = calendar_opt(ed, args, "copy")?;
    if base.is_some() && copy.is_some() {
        return Err(
            "'base' (a derived calendar) and 'copy' (a copied base calendar) exclude each other"
                .into(),
        );
    }
    let uid = ed.add_calendar(&name, base, copy)?;
    cal_reply(ed, uid)
}

fn cal_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    let name = args.get_str("name").map(str::to_string);
    let base = calendar_opt(ed, args, "base")?;
    let week = week_arg(args, "week")?;
    if name.is_none() && base.is_none() && week.is_empty() {
        return Err("cal.set needs at least one of 'name', 'base' or 'week'".into());
    }
    ed.set_calendar(uid, name.as_deref(), base, &week)?;
    cal_reply(ed, uid)
}

fn cal_exception_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    let index = index_arg(args)?;
    let (from, to) = range_args(args)?;
    // The exception's day is the arguments' own `working` and `times`;
    // neither given is a non-working day.
    let day = day_arg(args)?.unwrap_or_default();
    ed.set_calendar_exception(uid, index, args.get_str("name"), from, to, day)?;
    cal_reply(ed, uid)
}

fn cal_exception_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    let index = index_arg(args)?.ok_or("needs an 'index'")?;
    ed.delete_calendar_exception(uid, index)?;
    cal_reply(ed, uid)
}

fn cal_workweek_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    let index = index_arg(args)?;
    let (from, to) = range_args(args)?;
    // The week is the work week: without it a replace would drop every
    // day the work week states. `{}` states none on purpose.
    if matches!(args.get("week"), None | Some(Json::Null)) {
        return Err("needs a 'week' (the days the work week states; {} for none)".into());
    }
    let mut week: [DayPatch; 7] = Default::default();
    for (d, pattern) in week_arg(args, "week")? {
        week[d] = pattern;
    }
    ed.set_calendar_work_week(uid, index, args.get_str("name"), from, to, week)?;
    cal_reply(ed, uid)
}

fn cal_workweek_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    let index = index_arg(args)?.ok_or("needs an 'index'")?;
    ed.delete_calendar_work_week(uid, index)?;
    cal_reply(ed, uid)
}

fn cal_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = calendar_arg(ed, args)?;
    ed.delete_calendar(uid)?;
    Ok(Json::obj(vec![("deleted", Json::Num(f64::from(uid)))]))
}

// ---------------------------------------------------------------------------
// Resources (#1588)
// ---------------------------------------------------------------------------

/// One resource as JSON.
fn resource_json(ed: &Editor, r: &Resource) -> Json {
    let proj = ed.project();
    let text = |v: &Option<String>| v.clone().map_or(Json::Null, Json::Str);
    let rate = |v: &Option<Rate>| {
        v.as_ref()
            .and_then(Rate::to_f64)
            .map_or(Json::Null, Json::Num)
    };
    let base = resource_base_calendar(proj, r).and_then(|uid| proj.calendar(uid));
    Json::obj(vec![
        ("uid", Json::Num(f64::from(r.uid))),
        ("id", Json::Num(f64::from(r.id))),
        ("name", Json::Str(r.name.clone())),
        ("type", Json::Str(kind_name(r.kind).into())),
        ("initials", text(&r.initials)),
        ("group", text(&r.group)),
        ("code", text(&r.code)),
        ("email", text(&r.email_address)),
        ("material_label", text(&r.material_label)),
        (
            "max_units",
            if r.kind == ResourceType::Work {
                Json::Num(r.max_units)
            } else {
                Json::Null
            },
        ),
        ("standard_rate", rate(&r.standard_rate)),
        ("overtime_rate", rate(&r.overtime_rate)),
        ("cost_per_use", money(r.cost_per_use.as_ref())),
        (
            "accrue_at",
            Json::Str(accrue_name(r.accrue_at.unwrap_or(AccrueAt::Prorated)).into()),
        ),
        (
            "base_calendar",
            base.map_or(Json::Null, |c| {
                Json::obj(vec![
                    ("uid", Json::Num(f64::from(c.uid))),
                    ("name", Json::Str(c.name.clone())),
                ])
            }),
        ),
        ("notes", text(&r.notes)),
        (
            "assignments",
            Json::Num(
                proj.assignments
                    .iter()
                    .filter(|a| a.resource_uid == r.uid)
                    .count() as f64,
            ),
        ),
    ])
}

fn accrue_name(accrue: AccrueAt) -> &'static str {
    match accrue {
        AccrueAt::Start => "Start",
        AccrueAt::End => "End",
        AccrueAt::Prorated | AccrueAt::Invalid => "Prorated",
    }
}

fn resource_uid(args: &Json) -> Result<i32, String> {
    args.get("uid")
        .and_then(Json::as_i64)
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| "needs a numeric 'uid' (a resource UID)".to_string())
}

fn resource_reply(ed: &Editor, uid: i32) -> Result<Json, String> {
    let r = ed
        .project()
        .resources
        .iter()
        .find(|r| r.uid == uid)
        .ok_or_else(|| format!("no resource with uid {uid}"))?;
    Ok(resource_json(ed, r))
}

fn resource_list(ed: &Editor) -> Json {
    let resources: Vec<Json> = ed
        .project()
        .resources
        .iter()
        .map(|r| resource_json(ed, r))
        .collect();
    Json::obj(vec![
        ("count", Json::Num(resources.len() as f64)),
        ("resources", Json::Arr(resources)),
    ])
}

fn resource_get(ed: &Editor, args: &Json) -> Result<Json, String> {
    resource_reply(ed, resource_uid(args)?)
}

/// `type` as a resource type: `work`, `material` or `cost`, ASCII case
/// ignored.
fn resource_type_arg(args: &Json) -> Result<Option<ResourceType>, String> {
    let Some(t) = args.get("type") else {
        return Ok(None);
    };
    match t.as_str().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("work") => Ok(Some(ResourceType::Work)),
        Some("material") => Ok(Some(ResourceType::Material)),
        Some("cost") => Ok(Some(ResourceType::Cost)),
        _ => Err("'type' is \"work\", \"material\" or \"cost\"".into()),
    }
}

/// An optional text: a string, or `null` to clear it.
fn text_arg(args: &Json, key: &str) -> Result<Option<Option<String>>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Json::Null) => Ok(Some(None)),
        Some(Json::Str(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(format!("'{key}' must be text or null")),
    }
}

/// A rate in currency units: a number or a numeric string, or `null` to
/// clear it; stored `scale` times larger (100 for a cost kept in hundredths).
fn rate_arg(args: &Json, key: &str, scale: f64) -> Result<Option<Option<Rate>>, String> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    if matches!(v, Json::Null) {
        return Ok(Some(None));
    }
    let n = match v.as_str() {
        Some(text) => text.trim().trim_start_matches('$').parse::<f64>().ok(),
        None => v.as_f64(),
    };
    let n = n
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| format!("couldn't read '{key}' (a number of currency units, at least 0)"))?;
    let rate = if scale == 1.0 {
        Rate::from_f64(n)
    } else {
        Rate::from_f64((n * scale).round() + 0.0)
    };
    Ok(Some(rate))
}

/// `accrue_at` as an accrual: `Start`, `End` or `Prorated`, ASCII case
/// ignored.
fn accrue_arg(args: &Json) -> Result<Option<AccrueAt>, String> {
    let Some(a) = args.get("accrue_at") else {
        return Ok(None);
    };
    match a.as_str().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("start") => Ok(Some(AccrueAt::Start)),
        Some("end") => Ok(Some(AccrueAt::End)),
        Some("prorated") => Ok(Some(AccrueAt::Prorated)),
        _ => Err("'accrue_at' is \"Start\", \"End\" or \"Prorated\"".into()),
    }
}

/// `base_calendar` as a base calendar's uid, from its name or uid.
fn base_calendar_arg(ed: &Editor, args: &Json) -> Result<Option<i32>, String> {
    match args.get("base_calendar") {
        None => Ok(None),
        Some(Json::Str(name)) => {
            let name = name.trim();
            ed.project()
                .calendars
                .iter()
                .find(|c| c.base_calendar_uid.is_none() && c.name.eq_ignore_ascii_case(name))
                .map(|c| Some(c.uid))
                .or_else(|| name.parse::<i32>().ok().map(Some))
                .ok_or_else(|| format!("no base calendar named '{name}'"))
        }
        Some(Json::Num(n)) => Ok(Some(*n as i32)),
        Some(_) => Err("'base_calendar' must be a base calendar's name or uid".into()),
    }
}

/// The resource fields `resource.add` and `resource.set` share.
fn resource_patch(ed: &Editor, args: &Json) -> Result<ResourcePatch, String> {
    Ok(ResourcePatch {
        name: args.get_str("name").map(str::to_string),
        kind: resource_type_arg(args)?,
        initials: text_arg(args, "initials")?,
        group: text_arg(args, "group")?,
        code: text_arg(args, "code")?,
        email: text_arg(args, "email")?,
        material_label: text_arg(args, "material_label")?,
        max_units: fraction_arg(args, "max_units")?,
        standard_rate: rate_arg(args, "standard_rate", 1.0)?,
        overtime_rate: rate_arg(args, "overtime_rate", 1.0)?,
        cost_per_use: rate_arg(args, "cost_per_use", 100.0)?,
        accrue_at: accrue_arg(args)?,
        base_calendar: base_calendar_arg(ed, args)?,
        notes: text_arg(args, "notes")?,
    })
}

fn resource_add(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let name = args
        .get_str("name")
        .ok_or("resource.add needs a 'name'")?
        .to_string();
    let patch = resource_patch(ed, args)?;
    let kind = patch.kind.unwrap_or(ResourceType::Work);
    let uid = ed.add_resource(&name, kind, &patch)?;
    resource_reply(ed, uid)
}

fn resource_set(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = resource_uid(args)?;
    let patch = resource_patch(ed, args)?;
    if patch.is_empty() {
        return Err("resource.set needs at least one field to change".into());
    }
    ed.set_resource(uid, &patch)?;
    resource_reply(ed, uid)
}

fn resource_del(ed: &mut Editor, args: &Json) -> Result<Json, String> {
    let uid = resource_uid(args)?;
    let assignments = ed.delete_resource(uid)?;
    Ok(Json::obj(vec![
        ("deleted", Json::Num(f64::from(uid))),
        (
            "assignments",
            Json::Arr(
                assignments
                    .into_iter()
                    .map(|a| Json::Num(f64::from(a)))
                    .collect(),
            ),
        ),
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
        assert_eq!(field(&r, "Duration"), ("1 day", &Json::Num(480.0)));
        // Keyed by the name as asked; unset values are null, shown as Project shows them.
        assert_eq!(field(&r, " % complete"), ("0%", &Json::Null));
        assert_eq!(field(&r, "Actual Start"), ("NA", &Json::Null));
        assert_eq!(field(&r, "Baseline10 Cost"), ("$0.00", &Json::Null));

        let list = call(&mut ed, "task.list", vec![("fields", asked)]).unwrap();
        let tasks = list.get("tasks").unwrap().as_array().unwrap();
        let outline: Vec<_> = tasks.iter().map(|t| t.get_str("outline_number")).collect();
        assert_eq!(outline, [Some("1"), Some("1.1"), Some("1.2")]);
        assert!(tasks.iter().all(|t| t.get("fields").is_some()));
        assert_eq!(field(&tasks[0], "Duration").0, "2 days");

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
            ("Tue 3/3/26", &Json::Str("2026-03-03 08:00".into()))
        );
    }

    #[test]
    fn milestone_key_agrees_with_the_milestone_field() {
        let mut p = new_project();
        // A summary whose stored duration is 0: the Milestone field reads No.
        p.tasks.push(Task {
            uid: 2,
            id: 2,
            name: "Phase".into(),
            outline_level: 1,
            duration_min: 0,
            summary: true,
            ..Task::default()
        });
        p.tasks.push(Task {
            uid: 3,
            id: 3,
            name: "Work".into(),
            outline_level: 2,
            duration_min: 480,
            ..Task::default()
        });
        // A zero-length leaf: the field reads Yes.
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "Flag".into(),
            outline_level: 1,
            duration_min: 0,
            ..Task::default()
        });
        let mut ed = Editor::new(p);
        for (uid, milestone) in [(2, false), (4, true)] {
            let r = call(
                &mut ed,
                "task.get",
                vec![
                    ("uid", Json::Num(uid as f64)),
                    ("fields", names(&["Milestone"])),
                ],
            )
            .unwrap();
            let shown = if milestone { "Yes" } else { "No" };
            assert_eq!(
                r.get("milestone"),
                Some(&Json::Bool(milestone)),
                "uid {uid}"
            );
            assert_eq!(field(&r, "Milestone"), (shown, &Json::Bool(milestone)));
        }
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
            for bad in [names(&["Name", "Bogus2"]), Json::Str("x".into())] {
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
            args.push(("fields", names(&["Name", "Bogus2", "Bogus"])));
            assert_eq!(
                call(&mut ed, verb, args),
                Err("unknown task field 'Bogus2'".into()),
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
        assert_eq!(field(&r, "Duration"), ("3 days", &Json::Num(1440.0)));
        assert_eq!(
            field(&r, "Start"),
            ("Mon 3/2/26", &Json::Str("2026-03-02 08:00".into()))
        );
        assert_eq!(
            field(&r, "Finish"),
            ("Wed 3/4/26", &Json::Str("2026-03-04 17:00".into()))
        );
        assert_eq!(field(&r, "% Complete"), ("25%", &Json::Num(25.0)));
        assert_eq!(field(&r, "Cost"), ("$1,400.00", &Json::Num(1400.0)));
        assert_eq!(
            field(&r, "Notes"),
            ("Kickoff Monday", &Json::Str("Kickoff Monday".into()))
        );
    }

    #[test]
    fn task_get_money_text_follows_the_plans_currency() {
        // #579: the text is in the plan's currency; the value stays units.
        let xml = "<Project><CurrencySymbol>€</CurrencySymbol>            <CurrencyDigits>2</CurrencyDigits>            <CurrencySymbolPosition>3</CurrencySymbolPosition><Tasks>            <Task><UID>1</UID><ID>1</ID><Name>A</Name><OutlineLevel>1</OutlineLevel>            <Duration>PT8H0M0S</Duration><Cost>140000</Cost>            <FixedCost>-4000000</FixedCost></Task></Tasks></Project>";
        let mut ed = Editor::new(projcore::mspdi::read_mspdi(xml).unwrap());
        let r = call(
            &mut ed,
            "task.get",
            vec![
                ("uid", Json::Num(1.0)),
                ("fields", names(&["Cost", "Fixed Cost"])),
            ],
        )
        .unwrap();
        assert_eq!(field(&r, "Cost"), ("1,400.00 €", &Json::Num(1400.0)));
        assert_eq!(
            field(&r, "Fixed Cost"),
            ("(40,000.00 €)", &Json::Num(-40000.0))
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
    /// The Update Tasks fields (#1568), one undo step with the rest.
    #[test]
    fn task_set_takes_the_update_tasks_fields() {
        let mut a = app();
        let uid = add(&mut a, "Pour", "4d");
        let set = |a: &mut Editor, extra: Vec<(&str, Json)>| {
            let mut args = vec![("uid", Json::Num(uid as f64))];
            args.extend(extra);
            dispatch_editor(a, "task.set", &Json::obj(args)).unwrap()
        };
        let task = |a: &Editor| a.project().task(uid as i32).unwrap().clone();
        let depth = a.undo_depth();
        set(
            &mut a,
            vec![
                ("actual_start", Json::Str("2026-03-02".into())),
                ("actual_duration", Json::Str("2d".into())),
                ("priority", Json::Num(700.0)),
                ("percent_complete", Json::Num(50.0)),
            ],
        )
        .unwrap();
        assert_eq!(a.undo_depth(), depth + 1);
        let t = task(&a);
        assert_eq!(
            t.actual_start.map(|d| (d.parts().day, d.parts().hour)),
            Some((2, 8))
        );
        assert_eq!(
            (
                t.actual_duration_min,
                t.remaining_duration_min,
                t.percent_complete,
                t.priority
            ),
            (Some(960), Some(960), Some(50), Some(700))
        );
        // A conflicting percent_complete is refused, not overridden, and
        // takes the other fields with it.
        let before = a.project().clone();
        let err = set(
            &mut a,
            vec![
                ("priority", Json::Num(100.0)),
                ("remaining_duration", Json::Str("1d".into())),
                ("percent_complete", Json::Num(50.0)),
            ],
        )
        .unwrap_err();
        assert!(err.contains("contradicts"), "{err}");
        assert_eq!(*a.project(), before);
        // A bad remaining duration leaves the priority alone.
        for bad in [Json::Str("soon".into()), Json::Num(3.0)] {
            assert!(
                set(
                    &mut a,
                    vec![("priority", Json::Num(100.0)), ("remaining_duration", bad)]
                )
                .is_err()
            );
            assert_eq!(*a.project(), before);
        }
        set(&mut a, vec![("remaining_duration", Json::Str("1d".into()))]).unwrap();
        let t = task(&a);
        assert_eq!((t.duration_min, t.percent_complete), (1440, Some(67)));
        set(
            &mut a,
            vec![("actual_finish", Json::Str("2026-03-04".into()))],
        )
        .unwrap();
        let t = task(&a);
        assert_eq!(
            t.actual_finish.map(|d| (d.parts().day, d.parts().hour)),
            Some((4, 17))
        );
        assert_eq!(t.percent_complete, Some(100));
        // null clears.
        set(&mut a, vec![("actual_start", Json::Null)]).unwrap();
        let t = task(&a);
        assert_eq!(
            (t.actual_start, t.actual_finish, t.percent_complete),
            (None, None, Some(0))
        );
        set(&mut a, vec![("actual_start", Json::Str("later".into()))]).unwrap_err();
        let fresh = add(&mut a, "Wall", "2d");
        dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(fresh as f64)),
                ("actual_start", Json::Str("2026-03-02".into())),
                ("percent_complete", Json::Num(100.0)),
            ]),
        )
        .unwrap()
        .unwrap();
        let w = a.project().task(fresh as i32).unwrap();
        assert_eq!(
            (
                w.actual_duration_min,
                w.remaining_duration_min,
                w.percent_complete
            ),
            (Some(w.duration_min), Some(0), Some(100))
        );

        // A start alone leaves % Complete to percent_complete.
        set(
            &mut a,
            vec![
                ("actual_start", Json::Str("2026-03-02".into())),
                ("percent_complete", Json::Num(50.0)),
            ],
        )
        .unwrap();
        let t = task(&a);
        assert_eq!(
            (t.percent_complete, t.actual_duration_min),
            (Some(50), Some(t.duration_min / 2)),
            "the recorded actual follows the percent"
        );
        let err = set(
            &mut a,
            vec![
                ("actual_start", Json::Str("2026-03-03".into())),
                ("percent_complete", Json::Num(0.0)),
            ],
        )
        .unwrap_err();
        assert!(err.contains("more than 0%"), "{err}");
        // Both dates moved later in one call.
        set(
            &mut a,
            vec![("actual_finish", Json::Str("2026-03-04".into()))],
        )
        .unwrap();
        set(
            &mut a,
            vec![
                ("actual_start", Json::Str("2026-03-09".into())),
                ("actual_finish", Json::Str("2026-03-10".into())),
            ],
        )
        .unwrap();
        let t = task(&a);
        assert_eq!(
            (
                t.actual_start.map(|d| d.parts().day),
                t.actual_finish.map(|d| d.parts().day)
            ),
            (Some(9), Some(10))
        );
        // A finished task cleared to 50 %: its actual is 50 % of the duration.
        set(
            &mut a,
            vec![("actual_finish", Json::Str("2026-03-10".into()))],
        )
        .unwrap();
        set(
            &mut a,
            vec![
                ("actual_finish", Json::Null),
                ("percent_complete", Json::Num(50.0)),
            ],
        )
        .unwrap();
        let t = task(&a);
        assert_eq!(
            (
                t.percent_complete,
                t.actual_duration_min,
                t.remaining_duration_min
            ),
            (
                Some(50),
                Some(t.duration_min / 2),
                Some(t.duration_min - t.duration_min / 2)
            )
        );
        // 100 % on a started task completes its actual duration, with no finish.
        set(&mut a, vec![("percent_complete", Json::Num(100.0))]).unwrap();
        let t = task(&a);
        assert_eq!(
            (
                t.actual_duration_min,
                t.remaining_duration_min,
                t.actual_finish
            ),
            (Some(t.duration_min), Some(0), None)
        );
        // The same 50 % again, with the duration made longer first: the
        // actual follows the new duration.
        set(
            &mut a,
            vec![
                ("duration", Json::Str("8d".into())),
                ("percent_complete", Json::Num(50.0)),
            ],
        )
        .unwrap();
        let t = task(&a);
        assert_eq!((t.duration_min, t.actual_duration_min), (3840, Some(1920)));
        // A duration beside a remaining duration must agree with it.
        set(&mut a, vec![("actual_finish", Json::Null)]).unwrap();
        let before = a.project().clone();
        let err = set(
            &mut a,
            vec![
                ("duration", Json::Str("9d".into())),
                ("remaining_duration", Json::Str("1d".into())),
            ],
        )
        .unwrap_err();
        assert!(err.contains("contradicts"), "{err}");
        assert_eq!(*a.project(), before);
    }

    #[test]
    fn task_set_takes_start_and_finish_as_typed_in_the_cell() {
        let call = |a: &mut Editor, uid: i64, extra: Vec<(&str, Json)>| {
            let mut args = vec![("uid", Json::Num(uid as f64))];
            args.extend(extra);
            dispatch_editor(a, "task.set", &Json::obj(args)).unwrap()
        };
        let text = |s: &str| Json::Str(s.into());
        let day = |s: &str| parse_cell_date(s).unwrap();
        let mut a = app();
        let uid = add(&mut a, "Pour", "4d");
        let id = uid as i32;
        // A twin edited through the cell's own setters is the reference.
        let mut twin = Editor::new(a.project().clone());
        let depth = a.undo_depth();
        call(&mut a, uid, vec![("start", text("2026-03-09"))]).unwrap();
        twin.set_start(id, day("2026-03-09")).unwrap();
        assert_eq!(a.undo_depth(), depth + 1);
        let t = a.project().task(id).unwrap();
        assert_eq!(t.constraint, ConstraintType::StartNoEarlierThan);
        assert_eq!((t.constraint, t.constraint_date), {
            let r = twin.project().task(id).unwrap();
            (r.constraint, r.constraint_date)
        });
        assert_eq!(t.constraint_date.map(|d| d.parts().hour), Some(8));
        call(&mut a, uid, vec![("finish", text("2026-03-20"))]).unwrap();
        twin.set_finish(id, day("2026-03-20")).unwrap();
        let (t, r) = (
            a.project().task(id).unwrap(),
            twin.project().task(id).unwrap(),
        );
        assert_eq!(t.constraint, ConstraintType::FinishNoEarlierThan);
        assert_eq!(
            (t.constraint, t.constraint_date),
            (r.constraint, r.constraint_date)
        );
        assert_eq!(t.constraint_date.map(|d| d.parts().hour), Some(17));
        // A non-working day is refused as the cell refuses it.
        let before = a.project().clone();
        let err = call(&mut a, uid, vec![("finish", text("2026-03-14"))]).unwrap_err();
        assert!(err.contains("non-working"), "{err}");
        assert_eq!(*a.project(), before);
        // An exact instant is taken as typed.
        call(&mut a, uid, vec![("start", text("2026-03-10T10:00"))]).unwrap();
        let t = a.project().task(id).unwrap();
        assert_eq!(
            (
                t.constraint,
                t.constraint_date.map(|d| (d.parts().day, d.parts().hour))
            ),
            (ConstraintType::StartNoEarlierThan, Some((10, 10)))
        );
        call(&mut a, uid, vec![("finish", text("2026-03-18T12:00"))]).unwrap();
        let t = a.project().task(id).unwrap();
        assert_eq!(
            (
                t.constraint,
                t.constraint_date.map(|d| (d.parts().day, d.parts().hour))
            ),
            (ConstraintType::FinishNoEarlierThan, Some((18, 12)))
        );
        // Both on an auto task: refused, and the name beside them is not kept.
        let before = a.project().clone();
        let depth = a.undo_depth();
        let err = call(
            &mut a,
            uid,
            vec![
                ("name", text("Renamed")),
                ("start", text("2026-03-09")),
                ("finish", text("2026-03-13")),
            ],
        )
        .unwrap_err();
        assert!(err.contains("not both"), "{err}");
        assert_eq!(*a.project(), before);
        assert_eq!(a.undo_depth(), depth);
        // Bad text, null and "" name the key and change nothing.
        for (key, bad) in [
            ("start", text("soon")),
            ("start", Json::Null),
            ("finish", text("")),
            ("finish", Json::Num(5.0)),
            ("start", text("2026-02-30")),
            ("start", text("2026-03-10Tnoon")),
            ("start", text("2026-02-30T10:00")),
            ("finish", text("2026-13-01T10:00")),
        ] {
            let err = call(&mut a, uid, vec![("name", text("X")), (key, bad)]).unwrap_err();
            assert!(err.contains(&format!("'{key}' must be a date")), "{err}");
            assert_eq!(*a.project(), before);
        }
        // Manual: the start is pinned keeping the duration, in one undo step
        // with the switch and the duration.
        let manual = add(&mut a, "Wall", "2d");
        let mid = manual as i32;
        let depth = a.undo_depth();
        call(
            &mut a,
            manual,
            vec![
                ("manual", Json::Bool(true)),
                ("duration", text("3d")),
                ("start", text("2026-03-16")),
            ],
        )
        .unwrap();
        assert_eq!(a.undo_depth(), depth + 1);
        let t = a.project().task(mid).unwrap();
        assert!(t.manual);
        assert_eq!(
            (t.manual_start.map(|d| d.parts().day), t.duration_min),
            (Some(16), 3 * 480)
        );
        // Both apply on a manual task, start first, as the dialog does.
        call(
            &mut a,
            manual,
            vec![
                ("start", text("2026-03-17")),
                ("finish", text("2026-03-19")),
            ],
        )
        .unwrap();
        let t = a.project().task(mid).unwrap();
        assert_eq!(
            (
                t.manual_start.map(|d| d.parts().day),
                t.manual_finish.map(|d| d.parts().day),
                t.duration_min
            ),
            (Some(17), Some(19), 3 * 480)
        );
        // A finish before the start is refused and rolls everything back.
        let before = a.project().clone();
        let err = call(
            &mut a,
            manual,
            vec![
                ("name", text("Nope")),
                ("start", text("2026-03-24")),
                ("finish", text("2026-03-20")),
            ],
        )
        .unwrap_err();
        assert!(err.contains("before"), "{err}");
        assert_eq!(*a.project(), before);
        // An instant finish on a manual task sets the span up to it.
        call(&mut a, manual, vec![("finish", text("2026-03-18T12:00"))]).unwrap();
        let t = a.project().task(mid).unwrap();
        assert_eq!(
            t.manual_finish.map(|d| (d.parts().day, d.parts().hour)),
            Some((18, 12))
        );
        // An explicit constraint beside a start wins, applied after it.
        call(
            &mut a,
            uid,
            vec![
                ("start", text("2026-03-09")),
                (
                    "constraint",
                    Json::obj(vec![("type", text("ASAP")), ("date", Json::Null)]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            a.project().task(id).unwrap().constraint,
            ConstraintType::AsSoonAsPossible
        );
    }

    #[test]
    fn task_set_start_and_finish_judge_a_blank_row_as_the_setters_do() {
        let blank = |manual: bool| {
            let mut p = new_project();
            p.new_tasks_are_manual = manual;
            p.tasks.push(Task {
                uid: 2,
                id: 2,
                is_null: true,
                ..Task::default()
            });
            Editor::new(p)
        };
        let args = Json::obj(vec![
            ("uid", Json::Num(2.0)),
            ("start", Json::Str("2026-03-09".into())),
            ("finish", Json::Str("2026-03-11".into())),
        ]);
        // New tasks are manual: typing into the blank row makes it manual,
        // so both dates apply, as two cell edits would.
        let mut a = blank(true);
        dispatch_editor(&mut a, "task.set", &args).unwrap().unwrap();
        let t = a.project().task(2).unwrap();
        assert!(t.manual);
        assert_eq!(
            (
                t.manual_start.map(|d| d.parts().day),
                t.manual_finish.map(|d| d.parts().day)
            ),
            (Some(9), Some(11))
        );
        // New tasks are auto: refused, the row stays blank.
        let mut a = blank(false);
        let err = dispatch_editor(&mut a, "task.set", &args)
            .unwrap()
            .unwrap_err();
        assert!(err.contains("not both"), "{err}");
        assert!(a.project().task(2).unwrap().is_null);
    }

    #[test]
    fn task_set_start_resolves_a_day_on_the_calendar_the_call_ends_with() {
        use projcore::model::WorkingTime;
        let mut p = new_project();
        let mut late = p.calendars[0].clone();
        late.uid = 99;
        late.name = "Late".into();
        late.base_calendar_uid = None;
        late.week = std::array::from_fn(|_| {
            Some(DayWorking {
                times: vec![WorkingTime { from: 600, to: 840 }],
            })
        });
        p.calendars.push(late);
        let mut a = Editor::new(p);
        let args = Json::obj(vec![
            ("uid", Json::Num(1.0)),
            ("start", Json::Str("2026-03-09".into())),
            ("calendar", Json::Num(99.0)),
        ]);
        dispatch_editor(&mut a, "task.set", &args).unwrap().unwrap();
        let t = a.project().task(1).unwrap();
        assert_eq!(t.calendar_uid, Some(99));
        assert_eq!(t.constraint, ConstraintType::StartNoEarlierThan);
        assert_eq!(
            t.constraint_date.map(|d| (d.parts().day, d.parts().hour)),
            Some((9, 10))
        );
    }

    #[test]
    fn task_set_start_with_a_calendar_works_on_a_blank_row() {
        let mut p = new_project();
        p.tasks.push(Task {
            uid: 2,
            id: 2,
            is_null: true,
            ..Task::default()
        });
        let cal = p.calendars[0].uid;
        let mut a = Editor::new(p);
        let args = Json::obj(vec![
            ("uid", Json::Num(2.0)),
            ("start", Json::Str("2026-03-09".into())),
            ("calendar", Json::Num(f64::from(cal))),
        ]);
        dispatch_editor(&mut a, "task.set", &args).unwrap().unwrap();
        let t = a.project().task(2).unwrap();
        assert!(!t.is_null);
        assert_eq!(t.calendar_uid, Some(cal));
        assert_eq!(t.constraint, ConstraintType::StartNoEarlierThan);
    }

    #[test]
    fn task_set_refuses_an_impossible_date_in_every_instant() {
        let mut a = app();
        let before = a.project().clone();
        for args in [
            vec![("deadline", Json::Str("2026-02-30T10:00".into()))],
            vec![("actual_start", Json::Str("2026-02-30T10:00".into()))],
            vec![(
                "constraint",
                Json::obj(vec![
                    ("type", Json::Str("SNET".into())),
                    ("date", Json::Str("2026-02-30T10:00".into())),
                ]),
            )],
        ] {
            let mut all = vec![("uid", Json::Num(1.0))];
            all.extend(args);
            let err = dispatch_editor(&mut a, "task.set", &Json::obj(all))
                .unwrap()
                .unwrap_err();
            assert!(err.contains("must be a date"), "{err}");
            assert_eq!(*a.project(), before);
        }
    }

    #[test]
    fn task_set_start_refuses_auto_summaries() {
        let mut p = new_project();
        p.tasks[0].name = "Phase".into();
        for uid in [2, 3] {
            p.tasks.push(Task {
                uid,
                id: uid,
                name: format!("Sub {uid}"),
                outline_level: 2,
                duration_min: 480,
                ..Task::default()
            });
        }
        let mut a = Editor::new(p);
        let summary = a.project().tasks.iter().find(|t| t.summary).unwrap().uid;
        for key in ["start", "finish"] {
            let before = a.project().clone();
            let args = Json::obj(vec![
                ("uid", Json::Num(summary as f64)),
                ("name", Json::Str("X".into())),
                (key, Json::Str("2026-03-09".into())),
            ]);
            let err = dispatch_editor(&mut a, "task.set", &args)
                .unwrap()
                .unwrap_err();
            assert!(err.contains("read-only"), "{err}");
            assert_eq!(*a.project(), before);
        }
    }

    /// Marked (#1667): `task.set {marked}` is one undo step, the same value
    /// again none, and the `Marked` field reads it back.
    #[test]
    fn task_set_marks_a_task_as_one_step() {
        let mut a = app();
        let uid = add(&mut a, "Design", "3d");
        let set = |a: &mut Editor, on: bool| {
            dispatch_editor(
                a,
                "task.set",
                &Json::obj(vec![
                    ("uid", Json::Num(uid as f64)),
                    ("marked", Json::Bool(on)),
                    ("fields", Json::Arr(vec![Json::Str("Marked".into())])),
                ]),
            )
            .unwrap()
        };
        let depth = a.undo_depth();
        let r = set(&mut a, true).unwrap();
        assert_eq!(a.undo_depth(), depth + 1);
        assert_eq!(a.project().task(uid as i32).unwrap().marked, Some(true));
        let marked = r.get("fields").and_then(|f| f.get("Marked")).unwrap();
        assert_eq!(marked.get_str("text"), Some("Yes"));
        set(&mut a, true).unwrap();
        assert_eq!(a.undo_depth(), depth + 1, "unchanged is no step");
        assert!(set(&mut a, false).is_ok());
        assert_eq!(a.project().task(uid as i32).unwrap().marked, Some(false));
        let bad = dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("marked", Json::Str("yes".into())),
            ]),
        )
        .unwrap();
        assert!(bad.is_err());
    }

    /// The Task Information fields (#1550): every argument applies in the
    /// one undo step, and a refused one undoes them all.
    #[test]
    fn task_set_takes_the_task_information_fields_as_one_step() {
        let mut a = app();
        let uid = add(&mut a, "Design", "3d");
        let get =
            |a: &Editor| task_get(a, &Json::obj(vec![("uid", Json::Num(uid as f64))])).unwrap();
        let depth = a.undo_depth();
        let calendar = a.project().calendars[0].name.clone();
        let r = dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("percent_complete", Json::Num(40.0)),
                ("priority", Json::Num(800.0)),
                ("deadline", Json::Str("2026-03-27".into())),
                (
                    "constraint",
                    Json::obj(vec![
                        ("type", Json::Str("snet".into())),
                        ("date", Json::Str("2026-03-10".into())),
                    ]),
                ),
                ("task_type", Json::Str("fixed-work".into())),
                ("effort_driven", Json::Bool(true)),
                ("notes", Json::Str("check\nit".into())),
                ("calendar", Json::Str(calendar.clone())),
                ("ignore_resource_calendar", Json::Bool(true)),
                ("wbs", Json::Str("1.2".into())),
                (
                    "earned_value_method",
                    Json::Str("physical-percent-complete".into()),
                ),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.get_str("name"), Some("Design"));
        assert_eq!(a.undo_depth(), depth + 1, "every field is one step");
        let t = a.project().task(uid as i32).unwrap().clone();
        assert_eq!(t.percent_complete, Some(40));
        assert_eq!(t.priority, Some(800));
        assert_eq!(
            t.deadline.map(|d| d.parts().day),
            Some(27),
            "the deadline is the day's last working time"
        );
        assert_eq!(t.deadline.map(|d| d.parts().hour), Some(17));
        assert_eq!(t.constraint, ConstraintType::StartNoEarlierThan);
        assert_eq!(
            t.constraint_date.map(|d| (d.parts().day, d.parts().hour)),
            Some((10, 8))
        );
        assert_eq!(t.task_type, Some(TaskType::FixedWork));
        assert_eq!(t.effort_driven, Some(true));
        assert_eq!(
            t.notes.as_deref(),
            Some(
                "check
it"
            )
        );
        assert_eq!(t.calendar_uid, Some(a.project().calendars[0].uid));
        assert_eq!(t.ignore_resource_calendar, Some(true));
        assert_eq!(t.wbs.as_deref(), Some("1.2"));
        assert_eq!(t.earned_value_method, Some(EV_PHYSICAL_PERCENT_COMPLETE));
        assert_eq!(get(&a).get_str("name"), Some("Design"));

        // A milestone, a cleared deadline and calendar, a dateless constraint.
        dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("milestone", Json::Bool(true)),
                ("deadline", Json::Null),
                ("ignore_resource_calendar", Json::Bool(false)),
                ("calendar", Json::Null),
                (
                    "constraint",
                    Json::obj(vec![("type", Json::Str("ASAP".into()))]),
                ),
            ]),
        )
        .unwrap()
        .unwrap();
        let t = a.project().task(uid as i32).unwrap().clone();
        assert!(t.is_milestone() && t.deadline.is_none() && t.calendar_uid.is_none());
        assert_eq!(t.constraint, ConstraintType::AsSoonAsPossible);

        // A refused field undoes the whole call.
        let before = a.project().clone();
        let depth = a.undo_depth();
        let err = dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![
                ("uid", Json::Num(uid as f64)),
                ("priority", Json::Num(5.0)),
                ("percent_complete", Json::Num(500.0)),
            ]),
        )
        .unwrap()
        .unwrap_err();
        assert!(err.contains("0 to 100"), "{err}");
        assert_eq!(*a.project(), before);
        assert_eq!(a.undo_depth(), depth);
        // Bad spellings are refused before anything applies.
        for (key, value) in [
            ("task_type", Json::Str("fixed-cost".into())),
            ("earned_value_method", Json::Str("guess".into())),
            ("deadline", Json::Str("soon".into())),
            ("calendar", Json::Str("Night".into())),
            (
                "constraint",
                Json::obj(vec![("type", Json::Str("WHEN".into()))]),
            ),
            (
                "constraint",
                Json::obj(vec![("type", Json::Str("MSO".into()))]),
            ),
            ("priority", Json::Str("high".into())),
        ] {
            let err = dispatch_editor(
                &mut a,
                "task.set",
                &Json::obj(vec![("uid", Json::Num(uid as f64)), (key, value)]),
            )
            .unwrap()
            .unwrap_err();
            assert!(!err.is_empty(), "{key}");
            assert_eq!(*a.project(), before, "{key}");
        }
        // No argument at all is refused by name.
        let err = dispatch_editor(
            &mut a,
            "task.set",
            &Json::obj(vec![("uid", Json::Num(uid as f64))]),
        )
        .unwrap()
        .unwrap_err();
        assert!(err.contains("Task Information field"), "{err}");
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
            r#"{"path":"ctl-test.xml","modified":false,"name":"Project1","tasks":1,"start":"2026-01-05 08:00","finish":"2026-01-05 17:00","new_tasks":"auto","leveled":false}"#
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

    /// #1594 r6: a reply's `start` is the committed one after a second
    /// round. X, P, H (after P, not selected) and M, one day each on one
    /// resource from Monday; Level Selection P and M: H follows P onto M
    /// once the first round's delays are written, and the second round
    /// moves M to Friday.
    #[test]
    fn a_second_round_reply_reports_the_committed_start() {
        use projcore::model::{Assignment, Predecessor, Resource, Task};
        let mut p = projcore::editor::untitled_project();
        p.resources.push(Resource {
            uid: 1,
            id: 1,
            name: "Ann".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        for (uid, name) in [(1, "X"), (2, "P"), (3, "H"), (4, "M")] {
            let mut t = Task {
                uid,
                id: uid,
                name: name.into(),
                outline_level: 1,
                duration_min: 480,
                ..Task::default()
            };
            if uid == 3 {
                t.predecessors.push(Predecessor::fs(2));
            }
            p.tasks.push(t);
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 1,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let mut ed = Editor::new(p);
        let anchor = ed.schedule().get(1).unwrap().early_start;
        let out = run(&mut ed, "level.selection", r#"{"uids":[2,4]}"#).unwrap();
        assert_eq!(num(&out, "changed"), 2.0);
        assert_eq!(num(&out, "overallocated"), 0.0);
        let tasks = out.get("tasks").and_then(Json::as_array).unwrap();
        let m = tasks.iter().find(|t| num(t, "uid") == 4.0).unwrap();
        assert_eq!(num(m, "leveling_delay_min"), 4.0 * 1440.0);
        assert_eq!(
            m.get_str("start"),
            Some(dt_str(DateTime::from_minutes(anchor.minutes() + 4 * 1440)).as_str()),
            "Friday, the committed start"
        );
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
    fn calendar_verbs_read_and_edit_calendars() {
        for verb in ["cal.list", "cal.get", "cal.day"] {
            assert!(is_editor_verb(verb) && !MUTATING.contains(&verb), "{verb}");
        }
        for verb in [
            "cal.add",
            "cal.set",
            "cal.exception.set",
            "cal.exception.del",
            "cal.workweek.set",
            "cal.workweek.del",
            "cal.del",
        ] {
            assert!(is_editor_verb(verb) && MUTATING.contains(&verb), "{verb}");
        }
        let mut ed = Editor::new(new_project());
        let list = run(&mut ed, "cal.list", "{}").unwrap();
        assert_eq!(num(&list, "count"), 1.0);
        let std = &list.get("calendars").and_then(Json::as_array).unwrap()[0];
        assert_eq!(std.get_str("name"), Some("Standard"));
        assert_eq!(std.get("default"), Some(&Json::Bool(true)));
        let got = run(&mut ed, "cal.get", r#"{"calendar":"Standard"}"#).unwrap();
        let week = got.get("week").and_then(Json::as_array).unwrap();
        assert_eq!(week.len(), 7);
        assert_eq!(week[0].get_str("day"), Some("Sun"));
        assert_eq!(week[0].get("working"), Some(&Json::Bool(false)));
        assert_eq!(week[1].get("working"), Some(&Json::Bool(true)));
        assert_eq!(
            week[1].get("times").unwrap().to_string(),
            r#"[["08:00","12:00"],["13:00","17:00"]]"#
        );
        // A derived calendar with Fridays off, a holiday, a work week.
        let night = run(
            &mut ed,
            "cal.add",
            r#"{"name":"Night Shift","base":"Standard"}"#,
        )
        .unwrap();
        let night_uid = num(&night, "uid") as i32;
        assert_eq!(night.get("base").unwrap().get_str("name"), Some("Standard"));
        assert_eq!(
            night.get("week").and_then(Json::as_array).unwrap()[1].get("stated"),
            Some(&Json::Bool(false))
        );
        let set = run(
            &mut ed,
            "cal.set",
            &format!(r#"{{"calendar":{night_uid},"week":{{"Fri":{{"working":false}},"Sat":{{"times":[["09:00","13:00"]]}}}}}}"#),
        )
        .unwrap();
        let week = set.get("week").and_then(Json::as_array).unwrap();
        assert_eq!(week[5].get("working"), Some(&Json::Bool(false)));
        assert_eq!(week[5].get("stated"), Some(&Json::Bool(true)));
        assert_eq!(
            week[6].get("times").unwrap().to_string(),
            r#"[["09:00","13:00"]]"#
        );
        let holiday = run(
            &mut ed,
            "cal.exception.set",
            r#"{"calendar":"Standard","name":"Founders' Day","from":"2026-03-03","to":"2026-03-03"}"#,
        )
        .unwrap();
        let exc = &holiday.get("exceptions").and_then(Json::as_array).unwrap()[0];
        assert_eq!(exc.get_str("name"), Some("Founders' Day"));
        assert_eq!(exc.get("working"), Some(&Json::Bool(false)));
        assert_eq!(exc.get_str("from"), Some("2026-03-03"));
        // The derived calendar inherits it; cal.day says so.
        let day = run(
            &mut ed,
            "cal.day",
            &format!(r#"{{"calendar":{night_uid},"date":"2026-03-03"}}"#),
        )
        .unwrap();
        assert_eq!(day.get("working"), Some(&Json::Bool(false)));
        let based = day.get("based_on").unwrap();
        assert_eq!(based.get_str("kind"), Some("exception"));
        assert_eq!(based.get_str("calendar"), Some("Standard"));
        assert_eq!(based.get_str("name"), Some("Founders' Day"));
        let day = run(
            &mut ed,
            "cal.day",
            r#"{"calendar":"Night Shift","date":"2026-03-06"}"#,
        )
        .unwrap();
        assert_eq!(day.get("working"), Some(&Json::Bool(false)));
        assert_eq!(
            day.get("based_on").unwrap().get_str("kind"),
            Some("default")
        );
        assert_eq!(
            day.get("based_on").unwrap().get_str("calendar"),
            Some("Night Shift")
        );
        let ww = run(
            &mut ed,
            "cal.workweek.set",
            r#"{"calendar":"Standard","name":"Crunch","from":"2026-03-09","to":"2026-03-15","week":{"Sat":{"times":[["08:00","12:00"]]}}}"#,
        )
        .unwrap();
        assert_eq!(
            ww.get("work_weeks").and_then(Json::as_array).unwrap().len(),
            1
        );
        let day = run(
            &mut ed,
            "cal.day",
            r#"{"calendar":"Standard","date":"2026-03-14"}"#,
        )
        .unwrap();
        assert_eq!(day.get("working"), Some(&Json::Bool(true)));
        assert_eq!(
            day.get("based_on").unwrap().get_str("kind"),
            Some("work_week")
        );
        // Each edit was one step; deletes by index; refusals change nothing.
        assert_eq!(ed.undo_depth(), 4);
        run(
            &mut ed,
            "cal.workweek.del",
            r#"{"calendar":"Standard","index":0}"#,
        )
        .unwrap();
        run(
            &mut ed,
            "cal.exception.del",
            r#"{"calendar":"Standard","index":0}"#,
        )
        .unwrap();
        assert_eq!(ed.undo_depth(), 6);
        let before = ed.project().clone();
        for (verb, args) in [
            ("cal.set", r#"{"calendar":"Standard"}"#),
            (
                "cal.set",
                r#"{"calendar":"Standard","week":{"Mon":{"times":[["12:00","08:00"]]}}}"#,
            ),
            (
                "cal.set",
                r#"{"calendar":"Standard","week":{"Someday":{"working":false}}}"#,
            ),
            // A day is an object: a bare flag, a times list or a text
            // working is refused, never read as a non-working day.
            ("cal.set", r#"{"calendar":"Standard","week":{"Mon":true}}"#),
            (
                "cal.set",
                r#"{"calendar":"Standard","week":{"Mon":[["08:00","12:00"]]}}"#,
            ),
            (
                "cal.set",
                r#"{"calendar":"Standard","week":{"Mon":{"working":"yes"}}}"#,
            ),
            (
                "cal.workweek.set",
                r#"{"calendar":"Standard","from":"2026-03-09","to":"2026-03-13","week":{"Mon":"08:00-17:00"}}"#,
            ),
            // A work week needs its week; a calendar uid is a whole number.
            (
                "cal.workweek.set",
                r#"{"calendar":"Standard","from":"2026-03-09","to":"2026-03-13"}"#,
            ),
            ("cal.get", r#"{"calendar":1.5}"#),
            ("cal.del", r#"{"calendar":2.9}"#),
            ("cal.add", r#"{"name":"Half","base":1.5}"#),
            ("cal.set", r#"{"calendar":"Standard","base":"Night Shift"}"#),
            ("cal.add", r#"{"name":"standard"}"#),
            (
                "cal.exception.set",
                r#"{"calendar":"Standard","from":"2026-03-09","to":"2026-03-08"}"#,
            ),
            ("cal.exception.del", r#"{"calendar":"Standard","index":3}"#),
            ("cal.del", r#"{"calendar":"Standard"}"#),
            ("cal.get", r#"{"calendar":"Moon"}"#),
        ] {
            assert!(run(&mut ed, verb, args).is_err(), "{verb} {args}");
        }
        assert_eq!(*ed.project(), before);
        let del = run(&mut ed, "cal.del", r#"{"calendar":"Night Shift"}"#).unwrap();
        assert_eq!(num(&del, "deleted"), f64::from(night_uid));
        assert_eq!(num(&run(&mut ed, "cal.list", "{}").unwrap(), "count"), 1.0);
    }

    #[test]
    fn resource_verbs_read_add_edit_and_delete_resources() {
        for verb in ["resource.list", "resource.get"] {
            assert!(is_editor_verb(verb), "{verb}");
            assert!(!MUTATING.contains(&verb), "{verb}");
        }
        for verb in ["resource.add", "resource.set", "resource.del"] {
            assert!(is_editor_verb(verb), "{verb}");
            assert!(MUTATING.contains(&verb), "{verb}");
        }
        let mut ed = staffed(projcore::model::TaskType::FixedUnits, false);
        let list = run(&mut ed, "resource.list", "{}").unwrap();
        assert_eq!(num(&list, "count"), 2.0);
        let ann = &list.get("resources").unwrap().as_array().unwrap()[0];
        assert_eq!(ann.get_str("name"), Some("Ann"));
        assert_eq!(ann.get_str("type"), Some("work"));
        assert_eq!(num(ann, "max_units"), 1.0);
        assert_eq!(num(ann, "standard_rate"), 50.0);
        assert_eq!(ann.get("cost_per_use"), Some(&Json::Null));
        assert_eq!(ann.get_str("accrue_at"), Some("Prorated"));
        assert_eq!(
            ann.get("base_calendar").unwrap().get_str("name"),
            Some("Standard")
        );
        assert_eq!(num(ann, "assignments"), 0.0);
        // Added with its fields, in currency units.
        let added = run(
            &mut ed,
            "resource.add",
            r#"{"name":"Steel","type":"Material","material_label":"t","standard_rate":"120","cost_per_use":25.5,"accrue_at":"end","notes":"Grade S355"}"#,
        )
        .unwrap();
        assert_eq!(num(&added, "uid"), 3.0);
        assert_eq!(added.get_str("type"), Some("material"));
        assert_eq!(added.get("max_units"), Some(&Json::Null));
        assert_eq!(num(&added, "standard_rate"), 120.0);
        assert_eq!(num(&added, "cost_per_use"), 25.5);
        assert_eq!(added.get_str("accrue_at"), Some("End"));
        assert_eq!(added.get_str("material_label"), Some("t"));
        assert_eq!(ed.undo_depth(), 1);
        // Edited: rate and units, a cleared note; the reply is the resource.
        let set = run(
            &mut ed,
            "resource.set",
            r#"{"uid":1,"standard_rate":60,"max_units":"50%","group":"Crew","initials":"A","email":"ann@example.com"}"#,
        )
        .unwrap();
        assert_eq!(num(&set, "standard_rate"), 60.0);
        assert_eq!(num(&set, "max_units"), 0.5);
        assert_eq!(set.get_str("group"), Some("Crew"));
        assert_eq!(set.get_str("email"), Some("ann@example.com"));
        let cleared = run(&mut ed, "resource.set", r#"{"uid":1,"group":null}"#).unwrap();
        assert_eq!(cleared.get("group"), Some(&Json::Null));
        assert_eq!(ed.undo_depth(), 3);
        let got = run(&mut ed, "resource.get", r#"{"uid":1}"#).unwrap();
        assert_eq!(got, cleared);
        // Refusals change nothing.
        let before = ed.project().clone();
        for args in [
            r#"{"uid":1}"#,
            r#"{"uid":99,"name":"X"}"#,
            r#"{"uid":1,"name":"Steel"}"#,
            r#"{"uid":1,"type":"robot"}"#,
            r#"{"uid":1,"standard_rate":"lots"}"#,
            r#"{"uid":1,"standard_rate":-5}"#,
            r#"{"uid":1,"accrue_at":"sometimes"}"#,
            r#"{"uid":1,"base_calendar":"Moon"}"#,
            r#"{"uid":3,"max_units":1}"#,
        ] {
            assert!(run(&mut ed, "resource.set", args).is_err(), "{args}");
        }
        assert!(run(&mut ed, "resource.add", r#"{"type":"work"}"#).is_err());
        assert!(run(&mut ed, "resource.add", r#"{"name":"ann"}"#).is_err());
        assert_eq!(*ed.project(), before);
        assert_eq!(ed.undo_depth(), 3);
        // Deleting takes the assignments along, as one step.
        let a = run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        let a_uid = num(&a, "uid") as i32;
        assert!(run(&mut ed, "resource.set", r#"{"uid":1,"type":"cost"}"#).is_err());
        let del = run(&mut ed, "resource.del", r#"{"uid":1}"#).unwrap();
        assert_eq!(num(&del, "deleted"), 1.0);
        assert_eq!(
            del.get("assignments").unwrap().as_array().unwrap(),
            [Json::Num(f64::from(a_uid))]
        );
        assert_eq!(ed.undo_depth(), 5);
        let list = run(&mut ed, "resource.list", "{}").unwrap();
        assert_eq!(num(&list, "count"), 2.0);
        assert_eq!(
            num(&list.get("resources").unwrap().as_array().unwrap()[0], "id"),
            1.0
        );
        assert!(ed.project().assignments.is_empty());
        assert!(run(&mut ed, "resource.get", r#"{"uid":1}"#).is_err());
        assert!(ed.undo());
        assert_eq!(ed.project().assignments.len(), 1);
    }

    #[test]
    fn level_verbs_switch_the_overlay_and_list_the_overallocations() {
        for verb in ["level.all", "level.clear"] {
            assert!(is_editor_verb(verb) && MUTATING.contains(&verb), "{verb}");
        }
        assert!(
            is_editor_verb("level.overallocations") && !MUTATING.contains(&"level.overallocations")
        );
        // Ann on two parallel tasks is overallocated from their shared start.
        let mut ed = staffed(projcore::model::TaskType::FixedUnits, false);
        run(&mut ed, "task.add", r#"{"name":"Paint","duration":"2d"}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":2,"resource":1}"#).unwrap();
        let depth = ed.undo_depth();
        let over = run(&mut ed, "level.overallocations", "{}").unwrap();
        assert_eq!(num(&over, "count"), 1.0);
        let ann = &over.get("resources").and_then(Json::as_array).unwrap()[0];
        assert_eq!(ann.get_str("name"), Some("Ann"));
        assert_eq!(num(ann, "peak_units"), 2.0);
        assert_eq!(num(ann, "max_units"), 1.0);
        assert_eq!(ann.get_str("first"), Some("2026-01-05 08:00"));
        assert_eq!(
            path_info(None, &ed).get("leveled"),
            Some(&Json::Bool(false))
        );
        // Level All delays Paint behind Build; the overlay is no undo step.
        let on = run(&mut ed, "level.all", "{}").unwrap();
        assert_eq!(on.get("leveled"), Some(&Json::Bool(true)));
        assert_eq!(num(&on, "overallocated"), 0.0);
        assert!(ed.leveled());
        assert_eq!(ed.undo_depth(), depth);
        assert_eq!(
            num(
                &run(&mut ed, "level.overallocations", "{}").unwrap(),
                "count"
            ),
            0.0
        );
        assert!(ed.disp_start(2).unwrap() > ed.disp_finish(1).unwrap());
        assert_eq!(path_info(None, &ed).get("leveled"), Some(&Json::Bool(true)));
        let off = run(&mut ed, "level.clear", "{}").unwrap();
        assert_eq!(off.get("leveled"), Some(&Json::Bool(false)));
        assert_eq!(num(&off, "overallocated"), 1.0);
        assert!(!ed.leveled());
        // A plugin op's batch keeps the switch, although the plan is unchanged.
        let ops = [projcore::uiplugin::Op {
            verb: "level.all".into(),
            args: projcore::json::Json::Obj(Vec::new()),
        }];
        apply_ops(&mut ed, &ops).unwrap();
        assert!(ed.leveled());
        assert_eq!(ed.undo_depth(), depth);
        assert!(ed.leveled());
    }

    /// #1594: Level Selection, Level Resource, Leveling Options and Clear
    /// Leveling as plan edits: each one undo step, the delays stored and
    /// scheduled, the overlay untouched.
    #[test]
    fn level_edits_write_the_delays_as_undo_steps() {
        for verb in ["level.selection", "level.resource", "level.options"] {
            assert!(is_editor_verb(verb) && MUTATING.contains(&verb), "{verb}");
        }
        // A bare `level.options` reads; one naming a setting edits (#1594 r4).
        assert!(!mutates("level.options", &Json::obj(vec![])));
        assert!(mutates(
            "level.options",
            &Json::obj(vec![("within_slack", Json::Bool(true))])
        ));
        assert!(mutates("level.selection", &Json::obj(vec![])));
        // Routing fields are not settings (#1594 r6).
        assert!(!mutates(
            "level.options",
            &Json::obj(vec![("tab", Json::Num(0.0))])
        ));
        assert!(!mutates(
            "level.options",
            &Json::obj(vec![("order", Json::Null)])
        ));
        let mut ed = staffed(projcore::model::TaskType::FixedUnits, false);
        run(&mut ed, "task.add", r#"{"name":"Paint","duration":"2d"}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        run(&mut ed, "assign.add", r#"{"task":2,"resource":1}"#).unwrap();
        let depth = ed.undo_depth();
        let build_finish = ed.schedule().get(1).unwrap().early_finish;
        // Options: read, then set (one step); a bad order is named.
        let opts = run(&mut ed, "level.options", "{}").unwrap();
        assert_eq!(opts.get_str("order"), Some("Standard"));
        assert_eq!(opts.get("clear_first"), Some(&Json::Bool(true)));
        assert_eq!(ed.undo_depth(), depth);
        let set = run(
            &mut ed,
            "level.options",
            r#"{"order":"priority","within_slack":true}"#,
        )
        .unwrap();
        assert_eq!(set.get_str("order"), Some("Priority"));
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_eq!(ed.project().option("LevelingOrder"), Some("Priority"));
        let err = run(&mut ed, "level.options", r#"{"order":"slack"}"#).unwrap_err();
        assert!(err.contains("not ID, Standard or Priority"), "{err}");
        run(
            &mut ed,
            "level.options",
            r#"{"order":"standard","within_slack":false}"#,
        )
        .unwrap();
        assert_eq!(ed.undo_depth(), depth + 2);
        // Level Selection on Paint: it follows Build, with its delay stored.
        let out = run(&mut ed, "level.selection", r#"{"uids":[2]}"#).unwrap();
        assert_eq!(num(&out, "changed"), 1.0);
        assert_eq!(num(&out, "delayed"), 1.0);
        assert_eq!(num(&out, "overallocated"), 0.0);
        let tasks = out.get("tasks").and_then(Json::as_array).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(num(&tasks[0], "uid"), 2.0);
        assert!(num(&tasks[0], "leveling_delay_min") > 0.0);
        assert_eq!(ed.undo_depth(), depth + 3);
        assert!(ed.schedule().get(2).unwrap().early_start >= build_finish);
        assert!(!ed.leveled(), "the overlay is not switched");
        let paint = run(&mut ed, "task.get", r#"{"uid":2}"#).unwrap();
        assert!(num(&paint, "leveling_delay_min") > 0.0);
        // Clear Leveling as an edit: the delay goes, the dates come back, and
        // the reply counts the committed plan's conflicts, not the overlay's
        // (#1594 r6), which is on here and would re-level them away.
        run(&mut ed, "level.all", "{}").unwrap();
        let cleared = run(&mut ed, "level.clear", r#"{"uids":[2]}"#).unwrap();
        assert_eq!(num(&cleared, "cleared"), 1.0);
        assert_eq!(
            num(&cleared, "overallocated"),
            1.0,
            "committed, not the overlay"
        );
        assert_eq!(ed.undo_depth(), depth + 4);
        assert!(ed.schedule().get(2).unwrap().early_start < build_finish);
        run(&mut ed, "level.clear", "{}").unwrap();
        assert!(!ed.leveled());
        // Level Resource by name, then clear all; refusals name the problem.
        let out = run(&mut ed, "level.resource", r#"{"resource":"ann"}"#).unwrap();
        assert_eq!(num(&out, "delayed"), 1.0);
        assert_eq!(out.get("tasks").and_then(Json::as_array).unwrap().len(), 2);
        let err = run(&mut ed, "level.resource", r#"{"resource":"Bob"}"#).unwrap_err();
        assert!(err.contains("no assignments"), "{err}");
        let err = run(&mut ed, "level.selection", "{}").unwrap_err();
        assert!(err.contains("'uids' is required"), "{err}");
        assert_eq!(
            run(&mut ed, "level.clear", r#"{"all":1}"#).unwrap_err(),
            "'all' must be true or false"
        );
        let cleared = run(&mut ed, "level.clear", r#"{"all":true}"#).unwrap();
        assert_eq!(num(&cleared, "cleared"), 1.0);
        // A plain level.clear still switches the overlay off, no step.
        let depth = ed.undo_depth();
        run(&mut ed, "level.all", "{}").unwrap();
        let off = run(&mut ed, "level.clear", "{}").unwrap();
        assert_eq!(off.get("leveled"), Some(&Json::Bool(false)));
        assert_eq!(ed.undo_depth(), depth);
        // Options for one pass only (#1594 r1): within slack, Paint (no
        // slack beside Build) cannot move and stays overallocated; the
        // plan's options are untouched, and a bad `options` is refused.
        let once = run(
            &mut ed,
            "level.selection",
            r#"{"uids":[2],"options":{"within_slack":true}}"#,
        )
        .unwrap();
        assert_eq!(num(&once, "changed"), 0.0);
        assert_eq!(num(&once, "overallocated"), 1.0);
        assert_eq!(
            run(&mut ed, "level.options", "{}")
                .unwrap()
                .get("within_slack"),
            Some(&Json::Bool(false))
        );
        let err = run(&mut ed, "level.selection", r#"{"uids":[2],"options":3}"#).unwrap_err();
        assert!(err.contains("'options' must be an object"), "{err}");
        let err = run(
            &mut ed,
            "level.selection",
            r#"{"uids":[2],"options":{"order":"slack"}}"#,
        )
        .unwrap_err();
        assert!(err.contains("not ID, Standard or Priority"), "{err}");
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
    fn contour_and_percent_arguments_read_names_codes_and_percents() {
        let contour = |json: &str| contour_arg(&Json::parse(json).unwrap());
        for (json, code) in [
            (r#"{"contour":"Back Loaded"}"#, 1),
            (r#"{"contour":" turtle "}"#, 7),
            (r#"{"contour":"FLAT"}"#, 0),
            (r#"{"contour":"Contoured"}"#, 8),
            (r#"{"contour":6}"#, 6),
            (r#"{"contour":"3"}"#, 3),
        ] {
            assert_eq!(contour(json), Ok(Some(code)), "{json}");
        }
        for json in [
            r#"{"contour":"Wavy"}"#,
            r#"{"contour":1.5}"#,
            r#"{"contour":-1}"#,
            r#"{"contour":true}"#,
        ] {
            assert!(contour(json).is_err(), "{json}");
        }
        assert_eq!(contour("{}"), Ok(None));
        let pct = |json: &str| percent_arg(&Json::parse(json).unwrap(), "p");
        for (json, p) in [
            (r#"{"p":50}"#, 50),
            (r#"{"p":"50%"}"#, 50),
            (r#"{"p":" 100 % "}"#, 100),
            (r#"{"p":0}"#, 0),
        ] {
            assert_eq!(pct(json), Ok(Some(p)), "{json}");
        }
        assert_eq!(
            pct(r#"{"p":101}"#),
            Err("'p' must be 0 to 100, not 101".into())
        );
        for json in [r#"{"p":50.5}"#, r#"{"p":"half"}"#, r#"{"p":-1}"#] {
            assert!(pct(json).is_err(), "{json}");
        }
    }

    #[test]
    fn assign_set_takes_overtime_a_contour_and_actuals() {
        use projcore::model::TaskType;
        let mut p = staffed(TaskType::FixedUnits, false).project().clone();
        p.resources[0].overtime_rate = Rate::parse("75");
        let mut ed = Editor::new(p);
        let ann = run(&mut ed, "assign.add", r#"{"task":1,"resource":1}"#).unwrap();
        let uid = num(&ann, "uid");
        let mut set = |args: &str| {
            let args = format!(r#"{{"uid":{uid},{args}}}"#);
            run(&mut ed, "assign.set", &args).unwrap()
        };
        // 32h at $50 and 8h at $75.
        let ann = set(r#""overtime_work":8"#);
        assert_eq!(
            (
                num(&ann, "overtime_work_hours"),
                num(&ann, "regular_work_hours"),
                num(&ann, "cost")
            ),
            (8.0, 32.0, 2200.0)
        );
        let ann = set(r#""contour":"back loaded""#);
        assert_eq!(ann.get_str("contour"), Some("Back Loaded"));
        let ann = set(r#""contour":6"#);
        assert_eq!(ann.get_str("contour"), Some("Bell"));
        // 16 of the 40 hours, all regular, at $50.
        let ann = set(r#""actual_work":"2d""#);
        assert_eq!(
            (
                num(&ann, "actual_work_hours"),
                num(&ann, "remaining_work_hours"),
                num(&ann, "percent_work_complete"),
                num(&ann, "actual_cost"),
                num(&ann, "remaining_cost")
            ),
            (16.0, 24.0, 40.0, 800.0, 1400.0)
        );
        let ann = set(r#""percent_work_complete":"100%""#);
        assert_eq!(
            (
                num(&ann, "actual_work_hours"),
                num(&ann, "percent_work_complete"),
                num(&ann, "actual_cost"),
                num(&ann, "remaining_cost")
            ),
            (40.0, 100.0, 2200.0, 0.0)
        );
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
    fn task_add_then_assign_fills_task_work_and_cost() {
        use projcore::model::TaskType;
        let mut ed = staffed(TaskType::FixedUnits, false);
        let get = r#"{"uid":2,"fields":["Work","Cost"]}"#;
        let added = run(&mut ed, "task.add", r#"{"name":"New","duration":"5d"}"#).unwrap();
        assert_eq!(num(&added, "uid"), 2.0);
        let task = run(&mut ed, "task.get", get).unwrap();
        assert_eq!(field(&task, "Work").1, &Json::Null);
        assert_eq!(field(&task, "Cost").1, &Json::Null);

        let ann = run(&mut ed, "assign.add", r#"{"task":2,"resource":"Ann"}"#).unwrap();
        let task = run(&mut ed, "task.get", get).unwrap();
        assert_eq!(field(&task, "Work"), ("40 hrs", &Json::Num(2400.0)));
        assert_eq!(field(&task, "Cost"), ("$2,000.00", &Json::Num(2000.0)));

        // Filled totals move by delta from then on, down to zero.
        let del = format!(r#"{{"uid":{}}}"#, num(&ann, "uid"));
        run(&mut ed, "assign.del", &del).unwrap();
        let task = run(&mut ed, "task.get", get).unwrap();
        assert_eq!(field(&task, "Work"), ("0 hrs", &Json::Num(0.0)));
        assert_eq!(field(&task, "Cost"), ("$0.00", &Json::Num(0.0)));
        // Task 1, untouched, keeps its stored zero totals.
        let one = run(&mut ed, "task.get", r#"{"uid":1,"fields":["Work","Cost"]}"#).unwrap();
        assert_eq!(field(&one, "Work"), ("0 hrs", &Json::Num(0.0)));
        assert_eq!(field(&one, "Cost"), ("$0.00", &Json::Num(0.0)));
    }

    #[test]
    fn task_set_writes_custom_fields_by_name() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <StartDate>2026-03-02T08:00:00</StartDate>
  <ExtendedAttributes>
    <ExtendedAttribute><FieldID>188743731</FieldID><FieldName>Text1</FieldName><Alias>Trade</Alias>
      <ValueList>
        <Value><ID>1</ID><Value>Civil</Value><FieldGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101</FieldGUID></Value>
        <Value><ID>2</ID><Value>M&amp;E</Value><FieldGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E102</FieldGUID></Value>
      </ValueList></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743767</FieldID><FieldName>Number1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743752</FieldID><FieldName>Flag1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743737</FieldID><FieldName>Date1</FieldName></ExtendedAttribute>
  </ExtendedAttributes>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>Build</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT16H0M0S</Duration>
      <ExtendedAttribute><FieldID>188743731</FieldID><Value>Civil</Value></ExtendedAttribute></Task>
  </Tasks>
</Project>"#;
        let mut ed = Editor::new(projcore::mspdi::read_mspdi(xml).unwrap());
        let t = run(
            &mut ed,
            "task.set",
            r#"{"uid":1,"wbs":"1.4","custom":{"Text1":"m&e","number1":12.5,"Flag1":true,"Date1":"2026-03-09"},"fields":["Text1","Number1","Flag1","Date1","WBS"]}"#,
        )
        .unwrap();
        assert_eq!(field(&t, "Text1"), ("M&E", &Json::Str("M&E".into())));
        assert_eq!(field(&t, "Number1"), ("12.5", &Json::Num(12.5)));
        assert_eq!(field(&t, "Flag1"), ("Yes", &Json::Bool(true)));
        assert_eq!(field(&t, "Date1").0, "Mon 3/9/26");
        assert_eq!(field(&t, "WBS").0, "1.4");
        // One undo step for the WBS and the four fields.
        assert_eq!(ed.undo_depth(), 1);
        // A cleared field, and a refusal that undoes the whole call.
        let t = run(
            &mut ed,
            "task.set",
            r#"{"uid":1,"custom":{"Number1":null},"fields":["Number1"]}"#,
        )
        .unwrap();
        assert_eq!(field(&t, "Number1"), ("0", &Json::Null));
        let before = ed.project().clone();
        let err = run(
            &mut ed,
            "task.set",
            r#"{"uid":1,"wbs":"9","custom":{"Text1":"Steel"}}"#,
        )
        .unwrap_err();
        assert!(err.contains("Civil, M&E"), "{err}");
        assert_eq!(*ed.project(), before);
        for args in [
            r#"{"uid":1,"custom":{"Name":"x"}}"#,
            r#"{"uid":1,"custom":{"Text9":"x"}}"#,
            r#"{"uid":1,"custom":{"Flag1":"maybe"}}"#,
            r#"{"uid":1,"custom":{"Number1":[1]}}"#,
            r#"{"uid":1,"custom":5}"#,
        ] {
            assert!(run(&mut ed, "task.set", args).is_err(), "{args}");
        }
        assert_eq!(*ed.project(), before);
    }

    #[test]
    fn proj_get_and_set_read_and_write_the_plans_settings() {
        let mut ed = Editor::new(new_project());
        let got = run(&mut ed, "proj.get", "{}").unwrap();
        assert_eq!(got.get("status_date"), Some(&Json::Null));
        assert!(got.get_str("start").is_some());
        let calendars = got.get("calendars").and_then(Json::as_array).unwrap();
        assert!(!calendars.is_empty());
        let standard = calendars[0].get_str("name").unwrap().to_string();
        let set = run(
            &mut ed,
            "proj.set",
            &format!(
                r#"{{"title":"Build","start":"2026-03-09","status_date":"2026-03-16T17:00","current_date":"2026-03-10","calendar":"{standard}"}}"#
            ),
        )
        .unwrap();
        assert_eq!(set.get_str("title"), Some("Build"));
        assert_eq!(set.get_str("start"), Some("2026-03-09 08:00"));
        assert_eq!(set.get_str("status_date"), Some("2026-03-16 17:00"));
        assert_eq!(set.get_str("current_date"), Some("2026-03-10 08:00"));
        assert_eq!(
            set.get("calendar").unwrap().get_str("name"),
            Some(standard.as_str())
        );
        // One undo step for all of them.
        assert_eq!(ed.undo_depth(), 1);
        let cleared = run(&mut ed, "proj.set", r#"{"status_date":null}"#).unwrap();
        assert_eq!(cleared.get("status_date"), Some(&Json::Null));
        assert_eq!(ed.undo_depth(), 2);
        // A bare status date is the day's end, a bare current date its start.
        let bare = run(
            &mut ed,
            "proj.set",
            r#"{"status_date":"2026-03-20","current_date":"2026-03-20"}"#,
        )
        .unwrap();
        assert_eq!(bare.get_str("status_date"), Some("2026-03-20 17:00"));
        assert_eq!(bare.get_str("current_date"), Some("2026-03-20 08:00"));
        assert_eq!(ed.undo_depth(), 3);
        // The new-task mode (#1585): proj.get, proj.path and new tasks follow.
        assert_eq!(bare.get_str("new_tasks"), Some("auto"));
        let manual = run(&mut ed, "proj.set", r#"{"new_tasks":"Manual"}"#).unwrap();
        assert_eq!(manual.get_str("new_tasks"), Some("manual"));
        assert!(ed.project().new_tasks_are_manual);
        assert_eq!(ed.undo_depth(), 4);
        assert_eq!(path_info(None, &ed).get_str("new_tasks"), Some("manual"));
        let added = run(&mut ed, "task.add", r#"{"name":"By hand"}"#).unwrap();
        assert_eq!(added.get("manual"), Some(&Json::Bool(true)));
        // The mode the plan has records no step.
        run(&mut ed, "proj.set", r#"{"new_tasks":"manual"}"#).unwrap();
        assert_eq!(ed.undo_depth(), 5);
        // Refusals undo nothing.
        let before = ed.project().clone();
        for args in [
            r#"{}"#,
            r#"{"start":"soon"}"#,
            r#"{"calendar":"Moon"}"#,
            r#"{"title":"x","calendar":"Moon"}"#,
            r#"{"new_tasks":"sometimes"}"#,
            r#"{"new_tasks":true}"#,
        ] {
            assert!(run(&mut ed, "proj.set", args).is_err(), "{args}");
        }
        assert_eq!(*ed.project(), before);
        assert_eq!(ed.undo_depth(), 5);
    }

    #[test]
    fn an_assignment_reads_its_custom_fields() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <StartDate>2026-03-02T08:00:00</StartDate>
  <ExtendedAttributes>
    <ExtendedAttribute><FieldID>188743731</FieldID><FieldName>Text1</FieldName>
      <SecondaryPID>255868938</SecondaryPID></ExtendedAttribute>
    <ExtendedAttribute><FieldID>255869002</FieldID><FieldName>Number1</FieldName></ExtendedAttribute>
  </ExtendedAttributes>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>Build</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT16H0M0S</Duration></Task>
  </Tasks>
  <Resources>
    <Resource><UID>1</UID><ID>1</ID><Name>Ann</Name><Type>1</Type></Resource>
  </Resources>
  <Assignments>
    <Assignment><UID>7</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID><Units>1</Units>
      <Work>PT16H0M0S</Work>
      <ExtendedAttribute><FieldID>255868938</FieldID><Value>Civil</Value></ExtendedAttribute>
      <ExtendedAttribute><FieldID>255869002</FieldID><Value>12.5</Value></ExtendedAttribute>
    </Assignment>
  </Assignments>
</Project>"#;
        let mut ed = Editor::new(projcore::mspdi::read_mspdi(xml).unwrap());
        let a = run(
            &mut ed,
            "assign.get",
            r#"{"uid":7,"fields":["Text1","number1","Flag1"]}"#,
        )
        .unwrap();
        assert_eq!(field(&a, "Text1"), ("Civil", &Json::Str("Civil".into())));
        assert_eq!(field(&a, "number1"), ("12.5", &Json::Num(12.5)));
        assert_eq!(field(&a, "Flag1"), ("No", &Json::Bool(false)));
        let err = run(&mut ed, "assign.get", r#"{"uid":7,"fields":["Text31"]}"#).unwrap_err();
        assert_eq!(err, "unknown assignment field 'Text31'");
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
        // Peak and the budget derive (#1179): the units, and 0 without a budget.
        let fields = all.get("fields").unwrap();
        for (name, value) in [("Peak", 1.0), ("Budget Work", 0.0), ("Budget Cost", 0.0)] {
            assert_eq!(
                fields.get(name).unwrap().get("value"),
                Some(&Json::Num(value)),
                "{name}"
            );
        }
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
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"overtime_work":"lots"}}"#),
            ),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"overtime_work":-1}}"#),
            ),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"overtime_work":1000}}"#),
            ),
            ("assign.set", format!(r#"{{"uid":{uid},"contour":"Wavy"}}"#)),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"contour":"Contoured"}}"#),
            ),
            ("assign.set", format!(r#"{{"uid":{uid},"contour":1.5}}"#)),
            ("assign.set", format!(r#"{{"uid":{uid},"actual_work":-1}}"#)),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"actual_work":1000}}"#),
            ),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"percent_work_complete":50.5}}"#),
            ),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"percent_work_complete":"half"}}"#),
            ),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"percent_work_complete":101}}"#),
            ),
            (
                "assign.set",
                format!(r#"{{"uid":{uid},"actual_work":1,"percent_work_complete":10}}"#),
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
