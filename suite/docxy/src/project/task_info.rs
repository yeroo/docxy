//! Task > Properties > Information...: Project's Task Information dialog
//! (#1368), with General, Predecessors, Resources, Advanced and Notes pages.
//!
//! Each field reads and writes in the grid cell's own grammar
//! ([`cell_edit_text`], [`apply_cell`]), so a value means the same here and in
//! the table. OK writes only the fields the person changed, all inside one
//! [`projcore::editor::Editor::batch`]: one undo step, and a field that is
//! refused leaves the plan exactly as it was with the dialog open.
use super::*;
use crate::dialog::catalog;
use crate::dialog::{Control, ControlKind, Dialog, DialogOwner, Reaction, Value};
use crate::page_setup::{choice, chosen, ok_cancel, text_of};
use projcore::DateTime;
use projcore::editor::{
    PRIORITY_DEFAULT, PRIORITY_MAX, day_end, day_start, format_date_field, parse_cell_date,
};
use projcore::{ConstraintType, TaskType};

/// The page the dialog opens on: Information... opens General, Notes...
/// opens Notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InfoPage {
    General,
    Notes,
}

impl InfoPage {
    fn tab(self) -> usize {
        match self {
            Self::General => 0,
            Self::Notes => TABS.len() - 1,
        }
    }
}

const TABS: [&str; 5] = ["General", "Predecessors", "Resources", "Advanced", "Notes"];

/// Constraint types in MSPDI code order, which is `ConstraintType::code`.
const CONSTRAINTS: [(ConstraintType, &str); 8] = [
    (ConstraintType::AsSoonAsPossible, "As Soon As Possible"),
    (ConstraintType::AsLateAsPossible, "As Late As Possible"),
    (ConstraintType::MustStartOn, "Must Start On"),
    (ConstraintType::MustFinishOn, "Must Finish On"),
    (ConstraintType::StartNoEarlierThan, "Start No Earlier Than"),
    (ConstraintType::StartNoLaterThan, "Start No Later Than"),
    (
        ConstraintType::FinishNoEarlierThan,
        "Finish No Earlier Than",
    ),
    (ConstraintType::FinishNoLaterThan, "Finish No Later Than"),
];

const TASK_TYPES: [(TaskType, &str); 3] = [
    (TaskType::FixedUnits, "Fixed Units"),
    (TaskType::FixedDuration, "Fixed Duration"),
    (TaskType::FixedWork, "Fixed Work"),
];

fn needs_date(constraint: ConstraintType) -> bool {
    !matches!(
        constraint,
        ConstraintType::AsSoonAsPossible | ConstraintType::AsLateAsPossible
    )
}

fn field(name: &'static str, label: &str, kind: ControlKind, text: String, page: usize) -> Control {
    let mut c = Control::new(name, label, kind, Value::Text(text));
    c.page = Some(page);
    c
}

/// The dialog for task `uid`, opened on `page`. A blank row has no task to
/// describe.
pub(crate) fn task_info_dialog(
    ed: &ProjectEditor,
    uid: i32,
    page: InfoPage,
) -> Result<Dialog, String> {
    let task = ed.project().task(uid).ok_or("No task selected")?;
    if task.is_null {
        return Err("A blank row has no task information".into());
    }
    let text = |col| cell_edit_text(ed, task, col);
    let read_only = |col| summary_read_only(task, col);
    let mut d = Dialog::message(
        catalog::TASK_INFORMATION,
        "Task Information",
        String::new(),
        &[],
        DialogOwner::TaskInfo { uid },
    );
    d.text = None;
    d.tabs = TABS.iter().map(|t| t.to_string()).collect();
    d.tab = page.tab();
    let proj = ed.project();
    let constraint = CONSTRAINTS
        .iter()
        .position(|(c, _)| *c == task.constraint)
        .unwrap_or(0);
    let task_type = TASK_TYPES
        .iter()
        .position(|(t, _)| *t == task.task_type.unwrap_or(proj.default_task_type()))
        .unwrap_or(0);
    let effort = task.effort_driven.unwrap_or(proj.new_tasks_effort_driven());
    d.controls = vec![
        field("name", "&Name:", ControlKind::Text, text(COL_NAME), 0),
        field(
            "duration",
            "&Duration:",
            ControlKind::Duration,
            text(COL_DURATION),
            0,
        ),
        field(
            "percent",
            "&Percent complete:",
            ControlKind::Number,
            task.percent_complete.unwrap_or(0).to_string(),
            0,
        ),
        field(
            "priority",
            "P&riority:",
            ControlKind::Number,
            task.priority.unwrap_or(PRIORITY_DEFAULT).to_string(),
            0,
        ),
        field("start", "&Start:", ControlKind::Date, text(COL_START), 0),
        field("finish", "&Finish:", ControlKind::Date, text(COL_FINISH), 0),
        field(
            "predecessors",
            "&Predecessors:",
            ControlKind::Text,
            text(COL_PREDECESSORS),
            1,
        ),
        field(
            "resources",
            "&Resource names:",
            ControlKind::Text,
            text(COL_RESOURCES),
            2,
        ),
        choice(
            "ctype",
            "Constraint &type:",
            ControlKind::Dropdown,
            &CONSTRAINTS.map(|(_, name)| name),
            Some(constraint),
            Some(3),
        ),
        field(
            "cdate",
            "Constraint &date:",
            ControlKind::Date,
            task.constraint_date.map_or_else(String::new, |d| {
                stamp_text(proj, task, d, constraint_ends_day(task.constraint))
            }),
            3,
        ),
        field(
            "deadline",
            "Dea&dline:",
            ControlKind::Date,
            task.deadline
                .map_or_else(String::new, |d| stamp_text(proj, task, d, true)),
            3,
        ),
        choice(
            "ttype",
            "Task t&ype:",
            ControlKind::Dropdown,
            &TASK_TYPES.map(|(_, name)| name),
            Some(task_type),
            Some(3),
        ),
        {
            let mut c = Control::new(
                "effort",
                "&Effort driven",
                ControlKind::Checkbox,
                Value::Bool(effort),
            );
            c.page = Some(3);
            c
        },
        field(
            "notes",
            "&Notes (\\n = new line):",
            ControlKind::Text,
            projcore::text::notes_to_line(task.notes.as_deref().unwrap_or("")),
            4,
        ),
    ];
    let enabled = |d: &mut Dialog, name: &str, on: bool| {
        if let Some(c) = d.controls.iter_mut().find(|c| c.name == name) {
            c.enabled = on;
        }
    };
    // An auto summary's duration and dates roll up from its subtasks, and so
    // does its % complete.
    for (name, col) in [
        ("duration", COL_DURATION),
        ("start", COL_START),
        ("finish", COL_FINISH),
    ] {
        enabled(&mut d, name, !read_only(col));
    }
    enabled(&mut d, "percent", !task.summary);
    enabled(&mut d, "cdate", needs_date(task.constraint));
    d.buttons = ok_cancel();
    d.react = Some(Reaction(react));
    d.mark_opened();
    Ok(d)
}

/// ASAP and ALAP take no date: the constraint date field follows the type.
fn react(d: &mut Dialog, i: usize, _before: &Value) {
    if d.controls[i].name != "ctype" {
        return;
    }
    let on = match &d.controls[i].value {
        Value::Choice(Some(k)) => CONSTRAINTS.get(*k).is_some_and(|(c, _)| needs_date(*c)),
        _ => false,
    };
    if let Some(c) = d.controls.iter_mut().find(|c| c.name == "cdate") {
        c.enabled = on;
    }
}

/// Whether a constraint's date-only form means the end of that day: the
/// finish-bound types that fall on a day's last working time.
fn constraint_ends_day(constraint: ConstraintType) -> bool {
    matches!(
        constraint,
        ConstraintType::FinishNoLaterThan | ConstraintType::MustFinishOn
    )
}

/// A stored instant as the field shows it: the bare date when that is the
/// day's start (or end, `ends_day`) a bare date means, else with its time, so
/// retyping what is shown gives the same instant.
fn stamp_text(proj: &projcore::Project, task: &Task, at: DateTime, ends_day: bool) -> String {
    let day = at.start_of_day();
    let bare = if ends_day {
        day_end(proj, task, day)
    } else {
        day_start(proj, task, day)
    };
    let date = format_date_field(Some(at));
    if at == bare {
        date
    } else {
        let p = at.parts();
        format!("{date}T{:02}:{:02}", p.hour, p.minute)
    }
}

/// A typed date or `date`T`hh:mm`: a bare date is the day's start (or end,
/// `ends_day`) in the task's calendar, a time is taken as typed.
fn parse_stamp(
    ed: &ProjectEditor,
    uid: i32,
    text: &str,
    ends_day: bool,
) -> Result<DateTime, String> {
    let text = text.trim();
    let bad = || "Expected a valid date: YYYY-MM-DD or YYYY-MM-DDThh:mm".to_string();
    if let Some((day, time)) = text.split_once('T') {
        let day = parse_cell_date(day)?;
        let (h, m) = time.split_once(':').ok_or_else(bad)?;
        let (h, m) = (h.parse::<i64>(), m.parse::<i64>());
        return match (h, m) {
            (Ok(h), Ok(m)) if (0..24).contains(&h) && (0..60).contains(&m) => {
                Ok(day.add_minutes(h * 60 + m))
            }
            _ => Err(bad()),
        };
    }
    let day = parse_cell_date(text)?;
    let proj = ed.project();
    let task = proj.task(uid).ok_or("The edited task no longer exists")?;
    Ok(if ends_day {
        day_end(proj, task, day)
    } else {
        day_start(proj, task, day)
    })
}

/// A whole number within `lo..=hi`, or the refusal naming the field.
fn whole(d: &Dialog, name: &str, what: &str, lo: i32, hi: i32) -> Result<i32, String> {
    text_of(d, name)
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|n| (lo..=hi).contains(n))
        .ok_or_else(|| format!("{what} is a whole number from {lo} to {hi}"))
}

/// Write what the person changed in `d` to task `uid`, as one undo step.
/// `Ok(true)` when the plan changed. A refused field changes nothing.
pub(crate) fn apply_task_info(
    ed: &mut ProjectEditor,
    uid: i32,
    d: &Dialog,
) -> Result<bool, String> {
    let stored = ed.project().task(uid).and_then(|t| t.constraint_date);
    let changed = |name: &str| d.changed(name);
    ed.batch(|ed| {
        // Task type and Effort driven first: a resource added below rescales
        // work or duration by them.
        if changed("ttype") {
            let kind = chosen(d, "ttype")
                .and_then(|k| TASK_TYPES.get(k))
                .map(|(t, _)| *t)
                .ok_or("Pick a task type")?;
            ed.set_task_type(uid, kind)?;
        }
        if changed("effort") {
            ed.set_effort_driven(uid, d.value("effort") == Some(&Value::Bool(true)))?;
        }
        if changed("start")
            && changed("finish")
            && !ed.project().task(uid).is_some_and(|t| t.manual)
        {
            // Each would replace the other's constraint on an auto task.
            return Err(
                "Change Start or Finish, not both, on an auto-scheduled task; change Duration for the span"
                    .into(),
            );
        }
        for (name, col) in [
            ("name", COL_NAME),
            ("duration", COL_DURATION),
            ("predecessors", COL_PREDECESSORS),
            ("resources", COL_RESOURCES),
            ("start", COL_START),
            ("finish", COL_FINISH),
        ] {
            if changed(name) {
                apply_cell(ed, uid, col, &text_of(d, name))?;
            }
        }
        if changed("percent") {
            ed.set_percent_complete(uid, whole(d, "percent", "% Complete", 0, 100)?)?;
        }
        if changed("priority") {
            ed.set_priority(uid, whole(d, "priority", "Priority", 0, PRIORITY_MAX)?)?;
        }
        // After Start and Finish, which add their own constraint to an auto
        // task: the person's explicit choice here wins. A change of type
        // alone keeps the stored instant.
        if changed("ctype") || changed("cdate") {
            let kind = chosen(d, "ctype")
                .and_then(|k| CONSTRAINTS.get(k))
                .map(|(c, _)| *c)
                .ok_or("Pick a constraint type")?;
            let date = if !needs_date(kind) {
                None
            } else if changed("cdate") {
                let typed = text_of(d, "cdate");
                if typed.trim().is_empty() {
                    return Err(format!("{} needs a constraint date", kind.abbrev()));
                }
                Some(parse_stamp(ed, uid, &typed, constraint_ends_day(kind))?)
            } else {
                Some(stored.ok_or_else(|| format!("{} needs a constraint date", kind.abbrev()))?)
            };
            ed.set_constraint_typed(uid, kind, date)?;
        }
        if changed("deadline") {
            let typed = text_of(d, "deadline");
            let deadline = if typed.trim().is_empty() {
                None
            } else {
                Some(parse_stamp(ed, uid, &typed, true)?)
            };
            ed.set_deadline(uid, deadline)?;
        }
        if changed("notes") {
            ed.set_notes(uid, &projcore::text::notes_from_line(&text_of(d, "notes")))?;
        }
        Ok(())
    })?;
    // The tab's dirty flag and edit generation follow the editor
    // (`complete_project`), as for a summary delete.
    Ok(false)
}

#[cfg(test)]
mod tests;
