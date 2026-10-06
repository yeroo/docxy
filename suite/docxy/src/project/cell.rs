//! Entry-table edit state and transitions, shared by keyboard, mouse and host actions.
use super::*;
use projcore::LagUnit;
use projcore::editor::{
    DURATION_HINT, duration_suffix, format_duration_exact, parse_cell_date,
    parse_task_duration_unit, parse_task_predecessors,
};

pub(crate) const COL_ID: usize = 0;
pub(crate) const COL_MODE: usize = 1;
pub(crate) const COL_NAME: usize = 2;
pub(crate) const COL_DURATION: usize = 3;
pub(crate) const COL_START: usize = 4;
pub(crate) const COL_FINISH: usize = 5;
pub(crate) const COL_PREDECESSORS: usize = 6;
pub(crate) const COL_RESOURCES: usize = 7;
pub(crate) const COLUMN_COUNT: usize = 8;

/// The entry table's columns, in Project's Gantt Chart order.
pub(crate) const COLUMNS: [&str; COLUMN_COUNT] = [
    "ID",
    "Task Mode",
    "Name",
    "Duration",
    "Start",
    "Finish",
    "Predecessors",
    "Resource Names",
];

#[derive(Clone, Debug)]
pub(crate) struct CellEdit {
    /// The edited task; `None` on the entry row, where committing appends one.
    pub uid: Option<i32>,
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

/// A range of cells: the shown task rows (uids, top to bottom) and the
/// inclusive column span between the anchor and the cursor. Computed from
/// [`ProjectView::selection`], never stored, so it cannot go stale: an
/// anchor whose task was deleted, hidden or undone away degrades to no
/// selection.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Selection {
    pub uids: Vec<i32>,
    pub cols: std::ops::RangeInclusive<usize>,
}

impl Selection {
    /// The drawing and the clipboard agree on what is in the range.
    pub fn contains(&self, uid: i32, col: usize) -> bool {
        self.uids.contains(&uid) && self.cols.contains(&col)
    }

    /// How many cells the range covers; the harness `state` reports it.
    pub fn count(&self) -> usize {
        self.uids.len() * self.cols.clone().count()
    }
}

impl ProjectView {
    /// The bounding box of the anchor and the cell cursor, or `None` when
    /// there is no range: no anchor, the cursor on the entry row, the anchor
    /// task gone or hidden under a collapsed summary, or the anchor on the
    /// cursor. Both rows are positions among the shown rows, so rows a
    /// collapsed summary hides never land in the range.
    pub fn selection(&self) -> Option<Selection> {
        let (anchor_uid, anchor_col) = self.anchor?;
        let cursor_uid = self.selected_uid()?;
        if anchor_uid == cursor_uid && anchor_col == self.col {
            return None;
        }
        let rows = self.ed.visible_rows();
        let pos = |uid: i32| {
            rows.iter()
                .position(|&i| self.ed.project().tasks[i].uid == uid)
        };
        let (a, c) = (pos(anchor_uid)?, pos(cursor_uid)?);
        let (top, bottom) = if a <= c { (a, c) } else { (c, a) };
        Some(Selection {
            uids: rows[top..=bottom]
                .iter()
                .map(|&i| self.ed.project().tasks[i].uid)
                .collect(),
            cols: anchor_col.min(self.col)..=anchor_col.max(self.col),
        })
    }

    /// Shift+arrow: extend (or shrink) the range from the anchor to the
    /// cursor. The first extension anchors where the cursor is; later ones
    /// move only the cursor, so the anchor never drifts. Up/Down move over
    /// the shown rows and never step onto the entry row; on it, Shift+arrows
    /// do nothing. A move that lands the cursor back on the anchor leaves no
    /// range, which [`Self::selection`] then reports.
    pub fn extend_selection(&mut self, key: &str) -> bool {
        if !matches!(key, "up" | "down" | "left" | "right") {
            return false;
        }
        if self.on_entry_row() {
            return true;
        }
        let Some(uid) = self.selected_uid() else {
            return true;
        };
        let rows = self.ed.visible_rows();
        // Anchor where the cursor is: on the first extension, or when the
        // stored anchor's task is gone or hidden (a delete, an undo, a
        // collapse) — an anchor that cannot resolve selects nothing.
        let anchor_gone = self
            .anchor
            .is_some_and(|(uid, _)| !rows.iter().any(|&i| self.ed.project().tasks[i].uid == uid));
        if self.anchor.is_none() || anchor_gone {
            self.anchor = Some((uid, self.col));
        }
        match key {
            "left" | "right" => self.key(key, false),
            _ => {
                let at = self.display_row();
                let next = if key == "up" {
                    at.checked_sub(1)
                } else if at + 1 < rows.len() {
                    Some(at + 1)
                } else {
                    None
                };
                if let Some(at) = next {
                    self.entry = false;
                    self.ed.select(rows[at]);
                }
                true
            }
        }
    }

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
        let (uid, initial) = if self.on_entry_row() {
            if self.col == COL_ID {
                return Err("ID is read-only".into());
            }
            // The entry row is empty until committing it appends a task.
            (None, String::new())
        } else {
            let task = self
                .ed
                .project()
                .tasks
                .get(self.ed.sel())
                .ok_or("No task selected")?;
            if self.col == COL_ID || summary_read_only(task, self.col) {
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
            (Some(task.uid), cell_edit_text(&self.ed, task, self.col))
        };
        let buf = typed.map(str::to_owned).unwrap_or_else(|| initial.clone());
        self.cell = Some(CellEdit {
            last_error: None,
            uid,
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
        let (col, buf) = (cell.col, cell.buf.clone());
        let Some(uid) = cell.uid else {
            // The entry row: one undo step appends the task and applies the value.
            let Some((row, status)) = self
                .ed
                .append_row(|ed, uid| apply_cell(ed, uid, col, &buf))?
            else {
                return Ok(None);
            };
            self.entry = false;
            self.ed.select(row);
            return Ok(status);
        };
        apply_cell(&mut self.ed, uid, col, &buf)
    }
}

/// The text a cell edit opens with, which [`apply_cell`] reads back as the
/// same value: a duration exactly (`2d`, not the rounded `2 days`), in the
/// task's own unit where that is exact (`1.5w`, `0.5d`) so re-entering it
/// keeps the unit. Copy writes it too, so a copied cell pastes as it was.
pub(crate) fn cell_edit_text(ed: &ProjectEditor, task: &Task, col: usize) -> String {
    if col != COL_DURATION {
        return project_row(ed, task)[col].clone();
    }
    // A manual summary's duration is the span it shows.
    let min = if task.summary {
        ed.disp_duration_min(task.uid).unwrap_or(0)
    } else {
        task.duration_min
    };
    // A zero in days reads as a bare `0`; in another unit it keeps it (`0w`).
    if min == 0 && (task.summary || task.duration_unit().is_none_or(|u| u == LagUnit::Day)) {
        "0".into()
    } else if task.summary {
        // A summary's `?` is its subtasks'; it takes no estimate. Its
        // duration shows in days, whatever its format.
        format_duration_exact(min, ed.project(), None)
    } else {
        // An estimated duration reopens as it shows, `1d?`.
        format_duration_exact(min, ed.project(), task.duration_unit())
            + duration_suffix(ed.project(), task.uid)
    }
}

/// An auto summary's dates and duration roll up from its subtasks; a manual
/// summary's are its own, typed like a manual task's.
const SUMMARY_READ_ONLY: std::ops::RangeInclusive<usize> = COL_DURATION..=COL_FINISH;

fn summary_read_only(task: &Task, col: usize) -> bool {
    task.summary && !task.manual && SUMMARY_READ_ONLY.contains(&col)
}

/// A typed Task Mode: Project's names, or any start of them (`m`, `auto`),
/// ignoring case. `true` is Manually Scheduled.
pub(crate) fn parse_task_mode(text: &str) -> Result<bool, String> {
    let typed = text.trim().to_lowercase();
    let names = [("manually scheduled", true), ("auto scheduled", false)];
    names
        .into_iter()
        .find(|(name, _)| !typed.is_empty() && name.starts_with(typed.as_str()))
        .map(|(_, manual)| manual)
        .ok_or_else(|| "Task Mode is Manually Scheduled or Auto Scheduled (type m or a)".into())
}

/// Apply a typed cell value to task `uid`; a status line on success.
pub(crate) fn apply_cell(
    ed: &mut ProjectEditor,
    uid: i32,
    col: usize,
    buf: &str,
) -> Result<Option<String>, String> {
    let task = ed
        .project()
        .task(uid)
        .ok_or("The edited task no longer exists")?;
    if summary_read_only(task, col) {
        return Err("Summary dates and duration are read-only".into());
    }
    match col {
        COL_MODE => ed.set_manual(uid, parse_task_mode(buf)?)?,
        COL_NAME => ed.rename(uid, buf)?,
        COL_DURATION => {
            let (min, estimated, unit) = parse_task_duration_unit(buf, ed.project())
                .ok_or_else(|| format!("Invalid duration ({DURATION_HINT})"))?;
            ed.set_duration_typed(uid, min, estimated, unit)?;
        }
        COL_START | COL_FINISH => {
            let day = parse_cell_date(buf)?;
            // A manual task takes the typed date as its own start or
            // finish; an auto task gets an SNET/FNET constraint. Whether
            // it is manual is read after the edit: typing into a blank
            // row can make it a manual task.
            let previous = (task.constraint, task.constraint_date);
            if col == COL_START {
                ed.set_start(uid, day)?;
            } else {
                ed.set_finish(uid, day)?;
            }
            let task = ed
                .project()
                .task(uid)
                .ok_or("The edited task no longer exists")?;
            let current = (task.constraint, task.constraint_date);
            if task.manual || current == previous {
                return Ok(None);
            }
            return Ok(Some(format!(
                "Constraint set: {} (was {})",
                current.0.abbrev(),
                previous.0.abbrev()
            )));
        }
        COL_PREDECESSORS => {
            let task = ed
                .project()
                .task(uid)
                .ok_or("The edited task no longer exists")?;
            let predecessors = parse_task_predecessors(buf, task, ed.project())?;
            ed.set_predecessors(uid, predecessors)?;
        }
        COL_RESOURCES => ed.set_resources(uid, &projcore::editor::split_resource_names(buf))?,
        _ => return Err("ID is read-only".into()),
    }
    Ok(None)
}
/// Ctrl+Delete on task `uid`'s `col`: clear the field, or reset it to what a
/// new task gets where it cannot be empty (a milestone's Duration becomes a
/// day). Never deletes the task, and a blank row stays blank. A status line
/// when the field has nothing to reset to.
pub(crate) fn reset_cell(
    ed: &mut ProjectEditor,
    uid: i32,
    col: usize,
) -> Result<Option<String>, String> {
    let task = ed.project().task(uid).ok_or("No task selected")?;
    // Before every column: a mode or duration would make the row a task.
    if task.is_null {
        return Ok(None);
    }
    match col {
        COL_NAME | COL_PREDECESSORS | COL_RESOURCES => apply_cell(ed, uid, col, ""),
        COL_DURATION => {
            if summary_read_only(task, col) {
                return Err("Summary dates and duration are read-only".into());
            }
            let proj = ed.project();
            let (min, estimated) = (proj.days_to_minutes(1.0), proj.new_tasks_estimated());
            // A new task's duration, in days.
            ed.reset_duration(uid, min, estimated)?;
            Ok(None)
        }
        COL_MODE => {
            let manual = ed.project().new_tasks_are_manual;
            ed.set_manual(uid, manual)?;
            Ok(None)
        }
        _ => Ok(Some(format!("{} can't be cleared", COLUMNS[col]))),
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

/// Where a click in the entry table landed.
enum ClickTarget {
    Task(usize),
    /// The entry row as drawn, just below the last task.
    EntryRow,
    /// The ruled space below the entry row.
    Below,
}

pub(crate) fn project_cell_click(tab: &mut DocTab, row: usize, col: Option<usize>, double: bool) {
    cell_click(tab, ClickTarget::Task(row), col, double);
}

/// A click on the entry row below the last task, in column `col` (`None`:
/// outside the table, the column stays). The cursor goes to the entry row,
/// where typing appends a task, unless the click commits an open entry-row
/// edit that appends one: the clicked row is then that task, so the cursor
/// lands on it, as in Project (not on the new entry row below, where typing
/// would make a second task). A failed commit keeps the edit and its status,
/// and the cursor stays.
pub(crate) fn project_entry_click(tab: &mut DocTab, col: Option<usize>, double: bool) {
    cell_click(tab, ClickTarget::EntryRow, col, double);
}

/// A click on the ruled rows below the entry row: commit like any click-away,
/// then go to the entry row, keeping the column.
pub(crate) fn project_below_click(tab: &mut DocTab) {
    cell_click(tab, ClickTarget::Below, None, false);
}

/// The left press on a cell (`row` a task index, or the entry row's), as the
/// cell's `on_mouse_down(Left)` delivers it. It commits an open cell edit
/// exactly as a click does and gives up on a failed commit, then places the
/// cursor. A plain press clears the range; a Shift press extends from the
/// cursor; a press on a task's ID cell selects the whole row (and pins the
/// range to whole rows while it drags). A press that made a range swallows
/// the release-click through `drag_made_range`, so the gesture survives the
/// `on_click` gpui delivers on release.
pub(crate) fn project_cell_press(tab: &mut DocTab, row: usize, col: usize, shift: bool) {
    // A levelling pass asked for first runs first, as on any click; see
    // [`flush_level_pass`].
    flush_level_pass(tab);
    if !commit_project_cell(tab) {
        return;
    }
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    v.dragging = true;
    let col = col.min(COLUMN_COUNT - 1);
    let uid = v.ed.project().tasks.get(row).map(|t| t.uid);
    match (shift, uid) {
        (true, Some(_)) => {
            if let Some(cursor) = v.selected_uid() {
                v.anchor = Some((cursor, v.col));
            }
            v.select_row(row);
            v.col = col;
            v.drag_made_range = v.anchor.is_some();
        }
        (true, None) => {
            // Extending onto the entry row selects nothing; the old anchor
            // does not survive either.
            v.anchor = None;
            v.enter_entry_row();
            v.col = col;
            v.drag_made_range = false;
        }
        (false, Some(uid)) if col == COL_ID => {
            v.anchor = Some((uid, COLUMN_COUNT - 1));
            v.row_drag = true;
            v.select_row(row);
            v.col = COL_ID;
            v.drag_made_range = true;
        }
        (false, uid) => {
            // The press cell becomes the anchor: a drag grows the range from
            // it, and a press-release that goes nowhere leaves the anchor on
            // the cursor, which is no range at all.
            v.anchor = uid.map(|uid| (uid, col));
            v.row_drag = false;
            if uid.is_some() {
                v.select_row(row);
            } else {
                v.enter_entry_row();
            }
            v.col = col;
            v.drag_made_range = false;
        }
    }
    v.reveal_col();
    complete_project(tab, true);
}

/// The pointer entering cell (`row`, `col`) with the left button held: the
/// press's drag. The range grows to the rectangle between the press and this
/// cell; rows are task indices as for [`project_cell_press`]. The entry row
/// and past it end the gesture's reach (the range stops on the last task), as
/// do rows hidden under a collapsed summary, which have no cell to enter.
/// Whether the cursor actually moved is returned so the caller repaints only
/// then.
pub(crate) fn project_cell_drag_over(tab: &mut DocTab, row: usize, col: usize) -> bool {
    let Surface::Project(v) = &mut tab.surface else {
        return false;
    };
    if !v.dragging {
        return false;
    }
    let Some(uid) = v.ed.project().tasks.get(row).map(|t| t.uid) else {
        return false;
    };
    let col = col.min(COLUMN_COUNT - 1);
    let moved = v.selected_uid() != Some(uid) || (!v.row_drag && v.col != col);
    if !moved {
        return false;
    }
    // From here the release-click must not undo the gesture.
    v.drag_made_range = true;
    v.select_row(row);
    if !v.row_drag {
        v.col = col;
        v.reveal_col();
    }
    complete_project(tab, true);
    true
}

/// The left release anywhere: the suite-root mouse-up's capture phase calls
/// this, so no descendant's click handler can stop it and leave the drag
/// armed. `may_click` says the release landed where gpui delivers a row or
/// body release-click (over the entry-table body): that click still needs
/// `drag_made_range` to know it must be swallowed. A release anywhere else
/// leaves no click behind, so the flag is spent here — left set, it would
/// swallow a later, genuine click.
pub(crate) fn project_cell_release(tab: &mut DocTab, may_click: bool) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    v.dragging = false;
    v.row_drag = false;
    if !may_click {
        v.drag_made_range = false;
    }
}

fn cell_click(tab: &mut DocTab, target: ClickTarget, col: Option<usize>, double: bool) {
    // A drag (or a Shift/ID press) made a range: the release-click gpui
    // delivers on the gesture's element must not clear it. The flag is
    // taken, so a later genuine click is never swallowed; a double-click
    // still runs (an ID cell's "ID is read-only", for one).
    let swallowed = {
        let Surface::Project(v) = &mut tab.surface else {
            return;
        };
        std::mem::take(&mut v.drag_made_range)
    };
    if swallowed && !double {
        return;
    }
    // A click reaches here only with no press before it — the row's chart
    // half, the ruled rows below the table, a right-click's menu: it places
    // the cursor like any plain click, so the range does not survive it.
    if let Surface::Project(v) = &mut tab.surface {
        v.anchor = None;
    }
    // A levelling pass asked for first runs first, in the order the user
    // gave them; see [`flush_level_pass`].
    flush_level_pass(tab);
    let Surface::Project(v) = &tab.surface else {
        return;
    };
    let count = v.ed.project().tasks.len();
    let entry_edit = v.cell.as_ref().is_some_and(|c| c.uid.is_none());
    if !commit_project_cell(tab) {
        return;
    }
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    let appended = entry_edit && v.ed.project().tasks.len() > count;
    match target {
        ClickTarget::Task(row) => v.select_row(row),
        // Committing the entry row's edit made the clicked row a task.
        ClickTarget::EntryRow if appended => v.select_row(count),
        ClickTarget::EntryRow | ClickTarget::Below => v.enter_entry_row(),
    }
    if let Some(col) = col {
        v.col = col.min(COLUMN_COUNT - 1);
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
    // Up, Down, and Shift+Enter commit and move a row, as Enter does, in Project.
    if matches!(key, "enter" | "tab" | "up" | "down") {
        if commit_project_cell(tab) {
            if let Surface::Project(v) = &mut tab.surface {
                v.key(
                    match key {
                        "enter" if m.shift => "up",
                        "enter" => "down",
                        "up" | "down" => key,
                        _ if m.shift => "left",
                        _ => "right",
                    },
                    false,
                );
            }
            complete_project(tab, key != "tab");
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
        ("cell_row".into(), Json::Num(v.cursor_row() as f64)),
        (
            "cell_edit".into(),
            v.cell
                .as_ref()
                .map(|c| Json::Str(c.buf.clone()))
                .unwrap_or(Json::Null),
        ),
        (
            "selection".into(),
            v.selection()
                .map_or(Json::Null, |s| Json::Num(s.count() as f64)),
        ),
    ]
}

#[cfg(test)]
mod tests;
