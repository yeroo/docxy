//! The entry table's column widths (#1533): the plan's own
//! (`projcore::tableview`, saved in the `.yppx`) over the defaults, a heading
//! edge's drag, and Best Fit. The maths is here and unit-tested; the heading
//! draws the edge over the arrow, so the edge wins a press where they meet.
use super::*;
use projcore::tableview::{MAX_WIDTH, MIN_WIDTH};

/// The grab zone at a heading's right edge, in px.
pub(crate) const EDGE_W: f32 = 6.;
/// The AutoFilter arrow button, left of the edge, in px.
pub(crate) const ARROW_W: f32 = 16.;
/// Best Fit adds the cell's padding (`px_2` both sides) and a little air.
const FIT_PAD: f32 = 20.;

/// A width as the table keeps it: whole px within the limits.
pub(crate) fn clamp_width(width: f32) -> f32 {
    width.round().clamp(MIN_WIDTH, MAX_WIDTH)
}

/// Best Fit: the widest of the heading (with its arrow) and the cells, with
/// padding, clamped.
pub(crate) fn best_fit_width(heading: f32, cells: impl IntoIterator<Item = f32>) -> f32 {
    let widest = cells.into_iter().fold(0., f32::max);
    clamp_width(widest.max(heading + ARROW_W + EDGE_W) + FIT_PAD)
}

/// A heading edge's drag in flight: the pointer's x and the column's width
/// when it was pressed. The width shows while it moves; one undo step records
/// it on release.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WidthDrag {
    pub col: usize,
    pub start_x: f32,
    pub start_w: f32,
    /// The pointer moved: a press and release in place is no resize.
    pub moved: bool,
    pub width: f32,
}

/// The columns' committed widths: the plan's own over the defaults.
pub(crate) fn committed_widths(ed: &ProjectEditor) -> [f32; COLUMN_COUNT] {
    std::array::from_fn(|col| {
        ed.project()
            .table_view
            .width(COLUMNS[col])
            .unwrap_or(DEFAULT_WIDTHS[col])
    })
}

impl ProjectView {
    /// The columns' widths: the live drag's, the plan's own, else the default.
    pub fn widths(&self) -> [f32; COLUMN_COUNT] {
        let mut widths = committed_widths(&self.ed);
        if let Some(d) = self.width_drag {
            widths[d.col] = d.width;
        }
        widths
    }

    /// The columns' widths in all: the entry table's content width.
    pub fn table_total(&self) -> f32 {
        self.widths().iter().sum()
    }

    /// Press on column `col`'s edge at pointer x `x`.
    pub fn begin_width_drag(&mut self, col: usize, x: f32) {
        let start_w = self.widths()[col];
        self.width_drag = Some(WidthDrag {
            col,
            start_x: x,
            start_w,
            moved: false,
            width: start_w,
        });
    }

    /// The pointer moved to `x` with the edge held: the column follows.
    /// Returns whether anything changed.
    pub fn move_width_drag(&mut self, x: f32) -> bool {
        let Some(d) = self.width_drag else {
            return false;
        };
        let width = clamp_width(d.start_w + (x - d.start_x));
        if width == d.width {
            return false;
        }
        self.width_drag = Some(WidthDrag {
            moved: true,
            width,
            ..d
        });
        true
    }

    /// The button came up: record the dragged width as one undo step.
    /// Returns whether the plan changed.
    pub fn end_width_drag(&mut self) -> bool {
        let Some(d) = self.width_drag.take() else {
            return false;
        };
        d.moved && self.set_column_width(d.col, d.width)
    }

    /// Set column `col`'s width as one undo step; whether the plan changed.
    pub fn set_column_width(&mut self, col: usize, width: f32) -> bool {
        let width = clamp_width(width);
        let before = self.ed.revision();
        self.ed
            .edit_table_view(|v| v.set_width(COLUMNS[col], width));
        self.apply_widths();
        self.ed.revision() != before
    }
}

/// The Best Fit width of column `col`: its heading and the cells of the rows
/// shown, as they draw (summaries bold, the Name column indented).
pub(crate) fn best_fit(v: &ProjectView, col: usize, measure: &Measurer) -> f32 {
    let heading = measure.width(COLUMNS[col], 12., true, false);
    let tasks = &v.ed.project().tasks;
    let cells = v.ed.display_rows().into_iter().map(|i| {
        let task = &tasks[i];
        let text = &project_row(&v.ed, task)[col];
        let indent = if col == COL_NAME {
            // The outline indent, and a summary's toggle glyph.
            task.outline_level.saturating_sub(1).min(20) as f32 * 12.
                + if task.summary { 12. } else { 0. }
        } else {
            0.
        };
        measure.width(text, 12., task.summary, false) + indent
    });
    best_fit_width(heading, cells)
}

impl Docxy {
    /// Double-click on a heading's right edge: Best Fit, one undo step.
    pub(crate) fn best_fit_column(&mut self, col: usize, window: &Window, cx: &mut Context<Self>) {
        let measure = Measurer::new(window);
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Surface::Project(v) = &mut tab.surface else {
            return;
        };
        let width = best_fit(v, col, &measure);
        if v.set_column_width(col, width) {
            complete_project(tab, false);
            tab.status = format!("{} column set to Best Fit", COLUMNS[col]).into();
        }
        cx.notify();
    }
}

/// Release every Project tab's heading-edge drag, committing the width.
pub(crate) fn project_width_release_all(tabs: &mut [DocTab]) {
    for tab in tabs {
        let Surface::Project(v) = &mut tab.surface else {
            continue;
        };
        if v.end_width_drag() {
            complete_project(tab, false);
        }
    }
}

#[cfg(test)]
mod tests;
