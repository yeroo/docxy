//! Ctrl+C / Ctrl+X / Ctrl+V in the entry table (#369). The table has one cell
//! cursor, so Copy and Cut take that cell; Paste writes tab-separated text over
//! the table from it, as Project does with cells selected: filled rows are
//! overwritten, blank rows and the entry row become tasks, no row is inserted,
//! and the whole paste is one undo step. The host moves the text to and from
//! the system clipboard.
use super::*;

/// The cursor cell as the clipboard gets it: its edit text (see
/// [`cell_edit_text`]), so it pastes back as the same value. Empty on the
/// entry row, and in a blank row's cells but its ID.
pub(crate) fn project_copy_text(v: &ProjectView) -> String {
    let Some(task) = v.selected_uid().and_then(|uid| v.ed.project().task(uid)) else {
        return String::new();
    };
    if task.is_null {
        project_row(&v.ed, task)[v.col].clone()
    } else {
        cell_edit_text(&v.ed, task, v.col)
    }
}

/// Clear the cursor cell after the host copied it: Delete's clear, as one
/// undo step, on Name, Predecessors and Resource Names. Other columns keep
/// their value, and Cut never deletes the task, even on its ID.
pub(crate) fn project_cut(tab: &mut DocTab) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
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

/// A paste error, naming the cell: `Task 2 Duration: …`, or `New row …` on
/// a line past the last task.
fn cell_error(ed: &ProjectEditor, uid: Option<i32>, col: usize, e: String) -> String {
    match uid.and_then(|uid| ed.project().task(uid)) {
        Some(task) => format!("Task {} {}: {e}", task.id, COLUMNS[col]),
        None => format!("New row {}: {e}", COLUMNS[col]),
    }
}

#[cfg(test)]
mod tests;
