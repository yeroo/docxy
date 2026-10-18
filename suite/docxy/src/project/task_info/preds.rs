//! The Predecessors page's grid (#1536): ID, Task Name, Type and Lag, one row
//! per link and a blank row for the next. Picking a task by name fills its ID
//! and typing an ID fills its name; OK reads the rows back into links with
//! the cell's own grammar for the ID and the lag, and keeps a link spelled as
//! it was shown exactly as it is stored.
use super::*;
use projcore::editor::{format_lag, format_link_id, parse_lag, parse_task_predecessors};
use projcore::{LagFormat, LinkType, Predecessor};

const COLUMNS: [&str; 4] = ["ID", "Task Name", "Type", "Lag"];

const ID: usize = 0;
const NAME: usize = 1;
const TYPE: usize = 2;
const LAG: usize = 3;

pub(super) const TYPES: [(LinkType, &str); 4] = [
    (LinkType::FinishStart, "Finish-to-Start (FS)"),
    (LinkType::StartStart, "Start-to-Start (SS)"),
    (LinkType::FinishFinish, "Finish-to-Finish (FF)"),
    (LinkType::StartFinish, "Start-to-Finish (SF)"),
];

/// A link's lag as the grid shows it: the cell's spelling without the `+`.
fn lag_text(p: &Predecessor, proj: &projcore::Project) -> String {
    format_lag(p, proj).trim_start_matches('+').to_string()
}

/// The grid for `task`'s links.
pub(super) fn grid(proj: &projcore::Project, task: &Task) -> Control {
    let mut rows: Vec<Vec<String>> = task
        .predecessors
        .iter()
        .map(|p| {
            let name = match proj.task(p.uid) {
                Some(t) if p.cross_project != Some(true) => t.name.clone(),
                _ => String::new(),
            };
            let link = TYPES
                .iter()
                .find(|(l, _)| *l == p.link)
                .map_or("", |(_, name)| name);
            vec![
                format_link_id(p, proj),
                name,
                link.to_string(),
                lag_text(p, proj),
            ]
        })
        .collect();
    rows.push(vec![String::new(); COLUMNS.len()]);
    let others: Vec<&Task> = proj
        .tasks
        .iter()
        .filter(|t| !t.is_null && t.uid != task.uid && !t.is_project_summary())
        .collect();
    let mut c = Control::new(
        "predecessors",
        "&Predecessors:",
        ControlKind::Grid,
        Value::Rows(rows),
    );
    c.page = Some(1);
    c.columns = COLUMNS.map(String::from).to_vec();
    c.column_kinds = vec![
        ControlKind::Text,
        ControlKind::Combo,
        ControlKind::Dropdown,
        ControlKind::Text,
    ];
    // The ID column carries the IDs of the Task Name list, item for item.
    c.column_items = vec![
        others.iter().map(|t| t.id.to_string()).collect(),
        others.iter().map(|t| t.name.clone()).collect(),
        TYPES.iter().map(|(_, name)| name.to_string()).collect(),
        Vec::new(),
    ];
    c.editable_grid = true;
    c
}

/// A cell of the grid committed: an ID fills its Task Name, a Task Name its
/// ID, and a row that has a task gets the usual Finish-to-Start.
pub(super) fn follow(d: &mut Dialog, i: usize, before: &Value) {
    let (Value::Rows(old), Value::Rows(new)) = (before, d.controls[i].value.clone()) else {
        return;
    };
    // A pick acts on the selected row's Task Name even when it shows that
    // name already (of two tasks with one name, the other was picked).
    let picked = d.controls[i].picked;
    let Some((r, c)) = picked
        .and(d.controls[i].row.filter(|&r| r < new.len()))
        .map(|r| (r, NAME))
        .or_else(|| {
            (0..new.len()).find_map(|r| {
                let (was, now) = (old.get(r)?, &new[r]);
                (0..COLUMNS.len())
                    .find(|&c| was.get(c) != now.get(c))
                    .map(|c| (r, c))
            })
        })
    else {
        return;
    };
    let ids = d.controls[i].column_items[ID].clone();
    let names = d.controls[i].column_items[NAME].clone();
    let Value::Rows(rows) = &mut d.controls[i].value else {
        return;
    };
    let row = &mut rows[r];
    match c {
        NAME => {
            let want = row[NAME].trim().to_lowercase();
            // A pick carries its item: of two tasks with one name, the one
            // picked.
            let hit = picked.or_else(|| names.iter().position(|n| n.to_lowercase() == want));
            row[ID] = hit.map_or_else(String::new, |k| ids[k].clone());
            if let Some(k) = hit {
                row[NAME] = names[k].clone();
            }
        }
        ID => {
            let want = row[ID].trim();
            if let Some(k) = ids.iter().position(|id| id == want) {
                row[NAME] = names[k].clone();
            } else if want.is_empty() {
                row[NAME].clear();
            }
        }
        _ => return,
    }
    if !row[ID].trim().is_empty() && row[TYPE].trim().is_empty() {
        row[TYPE] = TYPES[0].1.to_string();
    }
}

/// The links the grid's rows say, for task `uid`. A row with no task is skipped; a
/// row names its task by ID (the cell's grammar: a local ID, a cross-project
/// reference) or, without one, by Task Name. A link whose task, type and lag
/// are what is stored keeps the stored link, lag format and all, when its lag
/// is spelled as the grid showed it or parses to the stored format; a lag
/// re-spelled in another unit or with `?` is an edit.
pub(super) fn parse(
    ed: &ProjectEditor,
    uid: i32,
    rows: &[Vec<String>],
) -> Result<Vec<Predecessor>, String> {
    let proj = ed.project();
    let task = proj.task(uid).ok_or("The edited task no longer exists")?;
    let mut out = Vec::new();
    for row in rows {
        let cell = |c: usize| row.get(c).map_or("", |s| s.trim());
        // A row names its task by ID or Task Name; with neither (a Type or
        // Lag alone, or a cleared ID) it is no link.
        let target = if !cell(ID).is_empty() {
            match parse_task_predecessors(cell(ID), task, proj)?.as_slice() {
                [one] => one.clone(),
                _ => return Err("A link's ID is one task's ID".into()),
            }
        } else if !cell(NAME).is_empty() {
            let want = cell(NAME).to_lowercase();
            let hit = proj
                .tasks
                .iter()
                .find(|t| !t.is_null && t.uid != uid && t.name.to_lowercase() == want)
                .ok_or_else(|| format!("No task named '{}'", cell(NAME)))?;
            Predecessor::fs(hit.uid)
        } else {
            continue;
        };
        let link = if cell(TYPE).is_empty() {
            LinkType::FinishStart
        } else {
            let want = cell(TYPE).to_lowercase();
            TYPES
                .iter()
                .find(|(_, name)| name.to_lowercase() == want || code_of(name) == want)
                .map(|(l, _)| *l)
                .ok_or_else(|| format!("'{}' is not a link type", cell(TYPE)))?
        };
        let (lag, lag_format) = if cell(LAG).is_empty() {
            (0, LagFormat::DAYS)
        } else {
            parse_lag(cell(LAG).trim_start_matches('+'), proj).ok_or("Invalid predecessor lag")?
        };
        let parsed = Predecessor {
            uid: target.uid,
            link,
            lag,
            lag_format,
            cross_project: target.cross_project,
            cross_project_name: target.cross_project_name.clone(),
        };
        // The stored link stays when the row says it again: the same task,
        // type and lag, in the same format or spelled as the grid opened.
        // A lag re-spelled in another unit or with `?` is an edit.
        out.push(
            match task.predecessors.iter().find(|p| {
                (p.uid, p.link, p.lag) == (parsed.uid, link, lag)
                    && (p.lag_format == lag_format
                        || cell(LAG).trim_start_matches('+') == lag_text(p, proj))
            }) {
                Some(stored) => stored.clone(),
                None => parsed,
            },
        );
    }
    Ok(out)
}

/// `ss` for `Start-to-Start (SS)`, lower case.
fn code_of(name: &str) -> String {
    name.rsplit('(')
        .next()
        .unwrap_or("")
        .trim_end_matches(')')
        .to_lowercase()
}
