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
use crate::page_setup::{choice, ok_cancel};
use projcore::editor::{PRIORITY_DEFAULT, PRIORITY_MAX, format_date_field};
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
            task.constraint_date
                .map_or_else(String::new, |d| format_date_field(Some(d))),
            3,
        ),
        field(
            "deadline",
            "Dea&dline:",
            ControlKind::Date,
            task.deadline
                .map_or_else(String::new, |d| format_date_field(Some(d))),
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

fn text_of(d: &Dialog, name: &str) -> String {
    d.controls
        .iter()
        .find(|c| c.name == name)
        .map_or_else(String::new, Control::text)
}

fn choice_of(d: &Dialog, name: &str) -> Option<usize> {
    match d.value(name) {
        Some(Value::Choice(i)) => *i,
        _ => None,
    }
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
    let before = ed.project().clone();
    let changed = |name: &str| d.changed(name);
    ed.batch(|ed| {
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
        // task: the person's explicit choice here wins.
        if changed("ctype") || changed("cdate") {
            let kind = choice_of(d, "ctype")
                .and_then(|k| CONSTRAINTS.get(k))
                .map(|(c, _)| *c)
                .ok_or("Pick a constraint type")?;
            let abbrev = kind.abbrev();
            let date = text_of(d, "cdate");
            let date = date.trim();
            let typed = if !needs_date(kind) {
                abbrev.to_string()
            } else if date.is_empty() {
                return Err(format!("{abbrev} needs a constraint date"));
            } else {
                format!(
                    "{abbrev} {}",
                    projcore::editor::parse_cell_date(date).map(|_| date)?
                )
            };
            ed.set_constraint(uid, &typed)?;
        }
        if changed("deadline") {
            let deadline = text_of(d, "deadline");
            let deadline = deadline.trim();
            let deadline = if deadline.is_empty() {
                None
            } else {
                Some(projcore::editor::parse_cell_date(deadline)?)
            };
            ed.set_deadline(uid, deadline)?;
        }
        if changed("ttype") {
            let kind = choice_of(d, "ttype")
                .and_then(|k| TASK_TYPES.get(k))
                .map(|(t, _)| *t)
                .ok_or("Pick a task type")?;
            ed.set_task_type(uid, kind)?;
        }
        if changed("effort") {
            ed.set_effort_driven(uid, d.value("effort") == Some(&Value::Bool(true)))?;
        }
        if changed("notes") {
            ed.set_notes(uid, &projcore::text::notes_from_line(&text_of(d, "notes")))?;
        }
        Ok(())
    })?;
    Ok(ed.project() != &before)
}

#[cfg(test)]
mod tests;
