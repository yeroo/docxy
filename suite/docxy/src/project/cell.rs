//! Entry-table edit state and transitions, shared by keyboard, mouse and host actions.
use super::*;
use projcore::editor::{
    format_duration_exact, parse_cell_date, parse_duration, parse_predecessors,
};

pub(super) const COLUMNS: [&str; 7] = [
    "ID",
    "Name",
    "Duration",
    "Start",
    "Finish",
    "Predecessors",
    "Resource Names",
];

#[derive(Clone, Debug)]
pub(crate) struct CellEdit {
    pub uid: i32,
    pub col: usize,
    pub initial: String,
    pub buf: String,
    /// UTF-8 byte boundary.
    pub caret: usize,
    pub last_error: Option<String>,
}

impl CellEdit {
    pub fn key(&mut self, key: &str, text: Option<&str>) {
        let prev = || {
            self.buf[..self.caret]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        let next = || {
            self.caret
                + self.buf[self.caret..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(0)
        };
        match key {
            "left" => self.caret = prev(),
            "right" => self.caret = next(),
            "home" => self.caret = 0,
            "end" => self.caret = self.buf.len(),
            "backspace" => {
                let p = prev();
                self.buf.replace_range(p..self.caret, "");
                self.caret = p;
            }
            "delete" => {
                let n = next();
                self.buf.replace_range(self.caret..n, "");
            }
            _ => {
                if let Some(text) = text {
                    let text: String = text.chars().filter(|c| !c.is_control()).collect();
                    self.buf.insert_str(self.caret, &text);
                    self.caret += text.len();
                }
            }
        }
    }

    /// Scroll the buffer by its measured width, leaving room for the caret.
    pub fn scroll_x(&self, available: f32, measure: impl FnOnce(&str) -> f32) -> f32 {
        (measure(&self.buf[..self.caret]) - available.max(0.)).max(0.)
    }
}

impl ProjectView {
    pub fn reveal_col(&mut self) {
        let left: f32 = WIDTHS[..self.col].iter().sum();
        let right = left + WIDTHS[self.col];
        let x = self.table_x.get();
        if left < x {
            self.table_x.set(left);
        } else if right > x + self.table_w {
            self.table_x.set((right - self.table_w).min(left));
        }
        self.clamp_offsets();
    }

    pub fn open_cell(&mut self, typed: Option<&str>) -> Result<(), String> {
        if self.prompt.is_some() || self.cell.is_some() {
            return Ok(());
        }
        let task = self
            .ed
            .project()
            .tasks
            .get(self.ed.sel())
            .ok_or("No task selected")?;
        if self.col == 0 || (task.summary && (2..=4).contains(&self.col)) {
            return Err(format!(
                "{} is read-only{}",
                COLUMNS[self.col],
                if task.summary {
                    " for summary tasks"
                } else {
                    ""
                }
            ));
        }
        let initial = if self.col == 2 {
            if task.duration_min == 0 {
                "0".into()
            } else {
                format_duration_exact(task.duration_min, self.ed.project())
            }
        } else {
            project_row(&self.ed, task)[self.col].clone()
        };
        let buf = typed.map(str::to_owned).unwrap_or_else(|| initial.clone());
        self.cell = Some(CellEdit {
            last_error: None,
            uid: task.uid,
            col: self.col,
            initial,
            caret: buf.len(),
            buf,
        });
        self.reveal_col();
        Ok(())
    }

    fn commit_cell_value(&mut self) -> Result<Option<String>, String> {
        let Some(cell) = &self.cell else {
            return Ok(None);
        };
        if cell.buf == cell.initial {
            return Ok(None);
        }
        let uid = cell.uid;
        let task = self
            .ed
            .project()
            .task(uid)
            .ok_or("The edited task no longer exists")?;
        if task.summary && (2..=4).contains(&cell.col) {
            return Err("Summary dates and duration are read-only".into());
        }
        match cell.col {
            1 => self.ed.rename(uid, &cell.buf)?,
            2 => {
                let min = parse_duration(&cell.buf, self.ed.project())
                    .ok_or("Invalid duration (try 3d, 4h, 2w)")?;
                self.ed.set_duration_min(uid, min)?;
            }
            3 | 4 => {
                let day = parse_cell_date(&cell.buf)?;
                // A manual task takes the typed date as its own start or
                // finish; an auto task gets an SNET/FNET constraint.
                let (manual, previous) = (task.manual, (task.constraint, task.constraint_date));
                if cell.col == 3 {
                    self.ed.set_start(uid, day)?;
                } else {
                    self.ed.set_finish(uid, day)?;
                }
                let task = self
                    .ed
                    .project()
                    .task(uid)
                    .ok_or("The edited task no longer exists")?;
                let current = (task.constraint, task.constraint_date);
                if manual || current == previous {
                    return Ok(None);
                }
                return Ok(Some(format!(
                    "Constraint set: {} (was {})",
                    current.0.abbrev(),
                    previous.0.abbrev()
                )));
            }
            5 => {
                let predecessors = parse_predecessors(&cell.buf, self.ed.project())?;
                self.ed.set_predecessors(uid, predecessors)?;
            }
            6 => self.ed.set_resources(
                uid,
                &cell.buf.split(',').map(str::to_owned).collect::<Vec<_>>(),
            )?,
            _ => return Err("ID is read-only".into()),
        }
        Ok(None)
    }
}

/// Close-time persistence saves every valid Project buffer, including inactive tabs.
/// Invalid buffers leave the last committed model available for hot-exit recovery.
pub(crate) fn commit_project_cells_for_exit(tabs: &mut [DocTab]) {
    for tab in tabs {
        let _ = commit_project_cell(tab);
    }
}

/// Commit before changing focus or dispatching commands. Failure preserves edit and selection.
pub(crate) fn commit_project_cell(tab: &mut DocTab) -> bool {
    let Surface::Project(v) = &mut tab.surface else {
        return true;
    };
    if v.cell.is_none() {
        return true;
    }
    match v.commit_cell_value() {
        Ok(status) => {
            let last_error = v.cell.take().and_then(|cell| cell.last_error);
            if let Some(status) = status {
                tab.status = status.into();
            } else if last_error.as_deref() == Some(tab.status.as_ref()) {
                tab.status = "Ready".into();
            }
            complete_project(tab, false);
            true
        }
        Err(status) => {
            if let Some(cell) = &mut v.cell {
                cell.last_error = Some(status.clone());
            }
            tab.status = status.into();
            false
        }
    }
}

pub(crate) fn project_cell_click(tab: &mut DocTab, row: usize, col: Option<usize>, double: bool) {
    if !commit_project_cell(tab) {
        return;
    }
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    v.select_row(row);
    if let Some(col) = col {
        v.col = col.min(6);
        v.reveal_col();
    }
    if double && col.is_some() {
        if let Err(e) = v.open_cell(None) {
            tab.status = e.into();
        }
    }
    complete_project(tab, true);
}

pub(crate) fn project_cell_input(tab: &mut DocTab, key: &str, text: Option<&str>, m: Modifiers) {
    if m.alt || m.platform || m.control {
        // Save commits the buffer before host dispatch; other chords cannot alter it.
        return;
    }
    if key == "enter" || key == "tab" {
        if commit_project_cell(tab) {
            if let Surface::Project(v) = &mut tab.surface {
                v.key(
                    if key == "enter" {
                        "down"
                    } else if m.shift {
                        "left"
                    } else {
                        "right"
                    },
                    false,
                );
            }
            complete_project(tab, key == "enter");
        }
    } else if let Surface::Project(v) = &mut tab.surface {
        if key == "escape" {
            v.cell = None;
        } else if let Some(cell) = &mut v.cell {
            cell.key(key, text);
        }
    }
}

pub(crate) fn project_cell_state(v: &ProjectView) -> Vec<(String, ctlcore::json::Json)> {
    use ctlcore::json::Json;
    vec![
        ("cell".into(), Json::Str(COLUMNS[v.col].into())),
        ("cell_row".into(), Json::Num(v.ed.sel() as f64)),
        (
            "cell_edit".into(),
            v.cell
                .as_ref()
                .map(|c| Json::Str(c.buf.clone()))
                .unwrap_or(Json::Null),
        ),
    ]
}

#[cfg(test)]
mod tests;
