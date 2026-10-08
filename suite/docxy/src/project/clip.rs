//! Ctrl+C / Ctrl+X / Ctrl+V in the entry table (#369, range copy/cut in #560).
//! The table has one cell cursor; with a range selection (Shift+arrows, a
//! drag, Shift+click, a click on an ID cell) Copy and Cut take every cell in
//! it, as tab-separated text with one line per shown row. A single cell
//! copies, cuts and pastes as before. Paste writes tab-separated text over
//! the table from the cursor cell (from the range's top-left when one is
//! selected), as Project does with cells selected: filled rows are
//! overwritten, blank rows and the entry row become tasks, no row is
//! inserted, and the whole paste is one undo step. The host moves the text
//! to and from the system clipboard.
//!
//! Whole rows (#1100): a copy of a range covering every column, as a click
//! on an ID cell selects (#560), also records the rows as tasks
//! ([`ProjectRowsClip`]). While the system clipboard still holds the text
//! that copy wrote, Paste inserts those tasks above the cursor row instead
//! of overwriting cells, as Project does, as one undo step.
use super::*;

/// The clipboard's text: the range's TSV when one is selected, else the
/// cursor cell. The range is one line per shown row, one field per selected
/// column, each field exactly what a single-cell copy of that cell gives
/// ([`cell_edit_text`], a blank row's shown text); a tab or line break
/// inside a field becomes a space so the TSV stays valid. Every line ends
/// with `\n`, as a spreadsheet's copy does, so a last row of empty fields
/// survives the paste's TSV parse.
pub(crate) fn project_copy_text(v: &ProjectView) -> String {
    if let Some(sel) = v.selection() {
        let mut text = sel
            .uids
            .iter()
            .map(|&uid| {
                let task = v.ed.project().task(uid).expect("selection rows exist");
                sel.cols
                    .clone()
                    .map(|col| {
                        let text = if task.is_null {
                            project_row(&v.ed, task)[col].clone()
                        } else {
                            cell_edit_text(&v.ed, task, col)
                        };
                        text.replace(['\t', '\r', '\n'], " ")
                    })
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        return text;
    }
    let Some(task) = v.selected_uid().and_then(|uid| v.ed.project().task(uid)) else {
        return String::new();
    };
    if task.is_null {
        project_row(&v.ed, task)[v.col].clone()
    } else {
        cell_edit_text(&v.ed, task, v.col)
    }
}

/// The resource UID of an assignment to nobody (MS Project's placeholder).
const UNASSIGNED_RESOURCE: i32 = -65535;

/// Tasks copied as whole rows: the rows' tasks (a collapsed summary's hidden
/// subtree with it), their assignments, and the names of the resources those
/// name, since another plan's resource UIDs mean other resources. They are
/// snapshots taken at copy time, so an edit to the source plan does not touch
/// them. `text` is what the copy wrote to the system clipboard: the clip is
/// live only while the clipboard still holds it (see [`Self::live`]).
#[derive(Clone, Debug)]
pub(crate) struct ProjectRowsClip {
    text: String,
    tasks: Vec<Task>,
    assignments: Vec<projcore::Assignment>,
    resources: Vec<(i32, String)>,
    /// The task calendars the tasks name, with their names, for the same
    /// reason as `resources`.
    calendars: Vec<(i32, String)>,
}

impl ProjectRowsClip {
    /// Whether the clipboard's `text` is still the copy this clip recorded
    /// (a clipboard round trip may turn `\n` into `\r\n`).
    pub fn live(&self, text: &str) -> bool {
        text.replace("\r\n", "\n") == self.text
    }
}

/// The whole-rows clip of the selected range, when it spans every column.
/// Blank rows are skipped. `text` is the clipboard text the copy writes.
pub(crate) fn project_rows_clip(v: &ProjectView, text: &str) -> Option<ProjectRowsClip> {
    let sel = v.selection()?;
    if *sel.cols.start() != COL_ID || *sel.cols.end() != COLUMN_COUNT - 1 {
        return None;
    }
    let proj = v.ed.project();
    let mut picked: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for &uid in &sel.uids {
        let Some(i) = proj.tasks.iter().position(|t| t.uid == uid) else {
            continue;
        };
        picked.insert(uid);
        if v.ed.is_collapsed(uid) {
            // The rows a collapsed summary hides are part of the row.
            let level = proj.tasks[i].outline_level;
            picked.extend(
                proj.tasks[i + 1..]
                    .iter()
                    .filter(|t| !t.is_null)
                    .take_while(|t| t.outline_level > level)
                    .map(|t| t.uid),
            );
        }
    }
    let tasks: Vec<Task> = proj
        .tasks
        .iter()
        .filter(|t| !t.is_null && picked.contains(&t.uid))
        .cloned()
        .collect();
    if tasks.is_empty() {
        return None;
    }
    let assignments: Vec<projcore::Assignment> = proj
        .assignments
        .iter()
        .filter(|a| tasks.iter().any(|t| t.uid == a.task_uid))
        .cloned()
        .collect();
    let resources = proj
        .resources
        .iter()
        .filter(|r| assignments.iter().any(|a| a.resource_uid == r.uid))
        .map(|r| (r.uid, r.name.clone()))
        .collect();
    let calendars = proj
        .calendars
        .iter()
        .filter(|c| tasks.iter().any(|t| t.calendar_uid == Some(c.uid)))
        .map(|c| (c.uid, c.name.clone()))
        .collect();
    Some(ProjectRowsClip {
        text: text.replace("\r\n", "\n"),
        tasks,
        assignments,
        resources,
        calendars,
    })
}

/// Paste whole rows as new tasks above the cursor row (above the range's top
/// row when one is selected; appended from the entry row), as one undo step.
/// An assignment follows its resource into another plan by name; one whose
/// resource that plan lacks is dropped, and the status says how many. The
/// cursor goes to the first new task. A paste that cannot apply changes
/// nothing and says why.
pub(crate) fn paste_project_rows(tab: &mut DocTab, clip: &ProjectRowsClip) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    let before = if v.on_entry_row() {
        None
    } else if let Some(sel) = v.selection() {
        sel.uids.first().copied()
    } else {
        v.selected_uid()
    };
    let resources = v.ed.project().resources.clone();
    let mut dropped = 0;
    let mut assignments = Vec::new();
    for a in &clip.assignments {
        // The unassigned placeholder is no resource of any plan: it goes
        // as it is. A resource the copy has no record of is dropped.
        let Some((_, name)) = clip
            .resources
            .iter()
            .find(|(uid, _)| *uid == a.resource_uid)
        else {
            if a.resource_uid == UNASSIGNED_RESOURCE {
                assignments.push(a.clone());
            } else {
                dropped += 1;
            }
            continue;
        };
        // The same resource in the same plan, else one of that name (an
        // empty name says nothing, so it never matches across resources).
        let named = |r: &&projcore::Resource| r.name.eq_ignore_ascii_case(name);
        let target = resources
            .iter()
            .find(|r| r.uid == a.resource_uid && named(r))
            .or_else(|| resources.iter().find(|r| !name.is_empty() && named(r)));
        match target {
            Some(r) => assignments.push(projcore::Assignment {
                resource_uid: r.uid,
                ..a.clone()
            }),
            None => dropped += 1,
        }
    }
    // A task calendar is the plan's own: the one of the same name here (the
    // same UID alone could be another calendar), else the plan's default.
    let calendars = &v.ed.project().calendars;
    let mut tasks = clip.tasks.clone();
    for t in &mut tasks {
        t.calendar_uid = t.calendar_uid.and_then(|uid| {
            let (_, name) = clip.calendars.iter().find(|(c, _)| *c == uid)?;
            let same = |c: &&projcore::Calendar| c.name.eq_ignore_ascii_case(name);
            calendars
                .iter()
                .find(|c| c.uid == uid && same(c))
                .or_else(|| calendars.iter().find(same))
                .map(|c| c.uid)
        });
    }
    match v.ed.insert_tasks(before, &tasks, &assignments) {
        Ok(new) => {
            let n = new.len();
            let mut status = format!("Pasted {n} row{}", if n == 1 { "" } else { "s" });
            if dropped > 0 {
                status += &format!(
                    "; {dropped} resource assignment{} dropped",
                    if dropped == 1 { "" } else { "s" }
                );
            }
            tab.status = status.into();
            v.anchor = None;
            if let Some(index) = new
                .first()
                .and_then(|&uid| v.ed.project().tasks.iter().position(|t| t.uid == uid))
            {
                // As typing into the entry row does: the cursor goes to the task.
                v.entry = false;
                v.ed.select(index);
            }
            complete_project(tab, true);
        }
        Err(e) => tab.status = e.into(),
    }
}

/// Clear the copied cells after the host copied them. A range: whichever of
/// Name, Predecessors and Resource Names the range covers clear for every
/// task in it, as one undo step (other columns keep their value, Cut never
/// deletes a task, a blank row is skipped, and a field that cannot clear
/// cancels the whole cut, naming the cell). When the range covers none of
/// them, only the copy happens — the status says so. Otherwise the cursor
/// cell: Delete's clear, as one undo step, on Name, Predecessors and
/// Resource Names; other columns keep their value, and Cut never deletes the
/// task, even on its ID.
pub(crate) fn project_cut(tab: &mut DocTab) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    if let Some(sel) = v.selection() {
        let cut_cols: Vec<usize> = sel
            .cols
            .clone()
            .filter(|c| matches!(*c, COL_NAME | COL_PREDECESSORS | COL_RESOURCES))
            .collect();
        if cut_cols.is_empty() {
            tab.status = "Selection copied; its columns can't be cut".into();
        } else {
            let result = v.ed.batch(|ed| {
                let mut status = None;
                for &uid in &sel.uids {
                    let Some(task) = ed.project().task(uid) else {
                        continue;
                    };
                    if task.is_null {
                        continue;
                    }
                    for col in &cut_cols {
                        let applied = apply_cell(ed, uid, *col, "")
                            .map_err(|e| cell_error(ed, Some(uid), *col, e))?;
                        status = applied.or(status);
                    }
                }
                Ok(status)
            });
            match result {
                Ok(Some(status)) => tab.status = status.into(),
                Ok(None) => {}
                Err(status) => tab.status = status.into(),
            }
        }
        v.anchor = None;
        complete_project(tab, false);
        return;
    }
    let Some(task) = v.selected_uid().and_then(|uid| v.ed.project().task(uid)) else {
        return;
    };
    let (uid, blank) = (task.uid, task.is_null);
    let result = match v.col {
        COL_NAME | COL_PREDECESSORS | COL_RESOURCES if blank => Ok(None),
        COL_NAME | COL_PREDECESSORS | COL_RESOURCES => apply_cell(&mut v.ed, uid, v.col, ""),
        col => Err(format!("{} can't be cut", COLUMNS[col])),
    };
    match result {
        Ok(Some(status)) | Err(status) => tab.status = status.into(),
        Ok(None) => {}
    }
    complete_project(tab, false);
}

/// Tab-separated text as lines of fields. One trailing line break, which
/// spreadsheets end a copy with, closes the last line rather than adding an
/// empty one. There is always a line: an empty copied cell (`""`, or Excel's
/// `"\r\n"`) is one empty field, which pastes as an emptied cell.
pub(crate) fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let text = text.replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    text.split('\n')
        .map(|line| line.split('\t').map(str::to_owned).collect())
        .collect()
}

/// Paste `text` from the cursor cell: line i goes to the i-th visible row at
/// or below the cursor, field j to the column j to the right of it (fields
/// past the last column are dropped, ID fields ignored). Lines past the last
/// task append tasks. An empty field clears Name, Predecessors or Resource
/// Names of a task and is ignored elsewhere, so an empty line makes no task.
/// A field that cannot apply cancels the whole paste and says which cell.
pub(crate) fn paste_project_text(tab: &mut DocTab, text: &str) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    if let Some(sel) = v.selection() {
        // A range is selected: the paste writes from its top-left cell, as
        // Project does with a block of cells selected, and the range clears.
        if let Some(&first) = sel.uids.first()
            && let Some(index) = v.ed.project().tasks.iter().position(|t| t.uid == first)
        {
            v.entry = false;
            v.ed.select(index);
        }
        v.col = *sel.cols.start();
        v.reveal_col();
        v.anchor = None;
    }
    let lines = parse_tsv(text);
    let first_col = v.col;
    let entry = v.on_entry_row();
    // By UID, fixed before anything changes: typing into a blank row can
    // change the outline and so which rows are shown.
    let targets: Vec<i32> = if entry {
        Vec::new()
    } else {
        let rows = v.ed.visible_rows();
        rows[v.display_row().min(rows.len())..]
            .iter()
            .map(|&i| v.ed.project().tasks[i].uid)
            .collect()
    };
    let result = v.ed.batch(|ed| {
        let mut status = None;
        let mut first_new = None;
        for (i, line) in lines.iter().enumerate() {
            let fields = line
                .iter()
                .enumerate()
                .map(|(j, field)| (first_col + j, field.as_str()))
                .filter(|&(col, _)| col < COLUMN_COUNT && col != COL_ID);
            match targets.get(i) {
                Some(&uid) => {
                    let blank = ed.project().task(uid).is_some_and(|t| t.is_null);
                    for (col, field) in fields {
                        let clears = matches!(col, COL_NAME | COL_PREDECESSORS | COL_RESOURCES);
                        if field.is_empty() && (blank || !clears) {
                            continue;
                        }
                        let applied = apply_cell(ed, uid, col, field)
                            .map_err(|e| cell_error(ed, Some(uid), col, e))?;
                        status = applied.or(status);
                    }
                }
                None => {
                    let mut fields: Vec<_> = fields.filter(|(_, f)| !f.is_empty()).collect();
                    // The task is made by its name when the line has one, as
                    // typing a name into the entry row makes it.
                    if let Some(at) = fields.iter().position(|&(col, _)| col == COL_NAME) {
                        let name = fields.remove(at);
                        fields.insert(0, name);
                    }
                    let mut made = None;
                    for (col, field) in fields {
                        let applied = match made {
                            Some(uid) => apply_cell(ed, uid, col, field),
                            // `append_row` takes one setter; the line's
                            // other fields apply to the task it made.
                            None => ed
                                .append_row(|ed, uid| apply_cell(ed, uid, col, field))
                                .map(|row| {
                                    row.and_then(|(row, applied)| {
                                        made = Some(ed.project().tasks[row].uid);
                                        first_new.get_or_insert(row);
                                        applied
                                    })
                                }),
                        }
                        .map_err(|e| cell_error(ed, None, col, e))?;
                        status = applied.or(status);
                    }
                }
            }
        }
        Ok((status, first_new))
    });
    match result {
        Ok((status, first_new)) => {
            tab.status = status.unwrap_or_else(|| "Ready".into()).into();
            if let (true, Some(row)) = (entry, first_new) {
                // As typing into the entry row does: the cursor goes to the task.
                v.entry = false;
                v.ed.select(row);
            }
            complete_project(tab, true);
        }
        Err(e) => tab.status = e.into(),
    }
}

/// A paste or range-cut error, naming the cell: `Task 2 Duration: …`, or
/// `New row …` on a line past the last task.
pub(crate) fn cell_error(ed: &ProjectEditor, uid: Option<i32>, col: usize, e: String) -> String {
    match uid.and_then(|uid| ed.project().task(uid)) {
        Some(task) => format!("Task {} {}: {e}", task.id, COLUMNS[col]),
        None => format!("New row {}: {e}", COLUMNS[col]),
    }
}

#[cfg(test)]
mod tests;
