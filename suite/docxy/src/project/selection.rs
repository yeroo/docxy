//! What the Entry table draws as selected (#1534), as Project does: the
//! selected cells are filled themselves (a click on Duration fills Duration,
//! not the row), the ID cell of every selected row and the heading of every
//! selected column carry a lighter tint, and a row selection (an ID press,
//! Shift+Space) fills every column of its rows. One pure [`Look`] per frame
//! decides all of it, so the drawing, the harness `state` and the unit tests
//! read the same answer.
use super::*;
use std::ops::RangeInclusive;

/// The selected rows (task uids) and columns, and whether the cursor sits on
/// the entry row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Look {
    /// The range's rows top to bottom, else the cursor's task; empty on the
    /// entry row.
    pub uids: Vec<i32>,
    /// The range's columns, else the cursor's.
    pub cols: RangeInclusive<usize>,
    /// The cursor is on the entry row (it has no uid).
    pub entry: bool,
    /// The cursor's column.
    pub cursor_col: usize,
}

impl Look {
    /// Whether the cell is filled with the selection colour. The entry row
    /// (`uid` `None`) fills only its cursor cell.
    pub fn fill(&self, uid: Option<i32>, col: usize) -> bool {
        match uid {
            Some(uid) => self.uids.contains(&uid) && self.cols.contains(&col),
            None => self.entry && col == self.cursor_col,
        }
    }

    /// Whether the task's ID cell carries the header tint: its row holds a
    /// selected cell.
    pub fn id_hot(&self, uid: i32) -> bool {
        self.uids.contains(&uid)
    }

    /// Whether the column's heading carries the header tint.
    pub fn heading_hot(&self, col: usize) -> bool {
        self.cols.contains(&col)
    }
}

impl ProjectView {
    /// The look of the selection now.
    pub fn look(&self) -> Look {
        match self.selection() {
            Some(s) => Look {
                uids: s.uids,
                cols: s.cols,
                entry: false,
                cursor_col: self.col,
            },
            None => Look {
                uids: self.selected_uid().into_iter().collect(),
                cols: self.col..=self.col,
                entry: self.on_entry_row(),
                cursor_col: self.col,
            },
        }
    }

    /// Ctrl+A: every shown task by every column, the cursor on the first cell
    /// and the anchor on the last. With no task the cursor goes to the entry
    /// row.
    pub fn select_all_cells(&mut self) {
        self.select_span(COLUMN_COUNT - 1, 0);
    }

    /// Ctrl+Space (and a heading's left press): column `col` over every shown
    /// task, the cursor on its first and the anchor on its last. One shown
    /// task is no range, the cursor cell being the selection.
    pub fn select_column(&mut self, col: usize) {
        let col = col.min(COLUMN_COUNT - 1);
        self.select_span(col, col);
    }

    /// Every shown task from the first (the cursor, in `cursor_col`) to the
    /// last (the anchor, in `anchor_col`); the entry row when none is shown.
    fn select_span(&mut self, anchor_col: usize, cursor_col: usize) {
        self.row_header = false;
        self.row_drag = false;
        let rows = self.ed.display_rows();
        match (rows.first().copied(), rows.last().copied()) {
            (Some(first), Some(last)) => {
                let last_uid = self.ed.project().tasks[last].uid;
                self.select_row(first);
                self.anchor = Some((last_uid, anchor_col));
            }
            _ => {
                self.anchor = None;
                self.enter_entry_row();
            }
        }
        self.col = cursor_col;
        self.reveal_col();
    }

    /// Shift+Space: the cursor's row over every column, as an ID press selects
    /// it (so Delete deletes the row). Nothing on the entry row.
    pub fn select_cursor_row(&mut self) -> bool {
        let Some(uid) = self.selected_uid() else {
            return false;
        };
        self.anchor = Some((uid, COLUMN_COUNT - 1));
        self.row_drag = false;
        self.row_header = true;
        self.col = 0;
        self.reveal_col();
        true
    }
}

/// The selection colours (#1534), both washes of the project accent so they
/// read on the light and the dark theme: the selected cells, and the ID
/// cells and headings of the selected rows and columns.
pub(crate) fn cell_fill_color() -> Hsla {
    crate::accent::wash(0.30)
}
pub(crate) fn header_tint_color() -> Hsla {
    crate::accent::wash(0.16)
}

/// The harness `state` fields: `selected_columns` (names), `selected_rows`
/// (1-based shown rows, whose ID cells carry the tint), and what is drawn: `filled_cells` (`"row:Column"`,
/// `entry` for the entry row), `hot_headings` (columns).
pub(crate) fn selection_state(v: &ProjectView) -> Vec<(String, ctlcore::json::Json)> {
    use ctlcore::json::Json;
    let look = v.look();
    let rows = v.ed.display_rows();
    let tasks = &v.ed.project().tasks;
    let shown: Vec<(usize, i32)> = rows
        .iter()
        .enumerate()
        .map(|(at, &i)| (at + 1, tasks[i].uid))
        .collect();
    let selected_rows: Vec<usize> = shown
        .iter()
        .filter(|(_, uid)| look.id_hot(*uid))
        .map(|(at, _)| *at)
        .collect();
    let columns: Vec<&str> = look.cols.clone().map(|c| COLUMNS[c]).collect();
    let mut filled = Vec::new();
    for &(at, uid) in &shown {
        for col in look.cols.clone() {
            if look.fill(Some(uid), col) {
                filled.push(format!("{at}:{}", COLUMNS[col]));
            }
        }
    }
    if look.fill(None, look.cursor_col) {
        filled.push(format!("entry:{}", COLUMNS[look.cursor_col]));
    }
    let strs = |items: Vec<String>| Json::Arr(items.into_iter().map(Json::Str).collect());
    vec![
        (
            "selected_columns".into(),
            strs(columns.iter().map(|c| c.to_string()).collect()),
        ),
        (
            "selected_rows".into(),
            Json::Arr(selected_rows.iter().map(|&r| Json::Num(r as f64)).collect()),
        ),
        ("filled_cells".into(), strs(filled)),
        (
            "hot_headings".into(),
            strs(columns.iter().map(|c| c.to_string()).collect()),
        ),
        (
            "cursor_border".into(),
            Json::Str(if look.entry {
                format!("entry:{}", COLUMNS[v.col])
            } else {
                format!("{}:{}", v.display_row() + 1, COLUMNS[v.col])
            }),
        ),
    ]
}

#[cfg(test)]
mod tests;
