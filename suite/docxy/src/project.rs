//! Project tab: file/session policy and the Gantt view over the shared editor.
use super::*;
use crate::project_formats::{self, Source};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use projcore::editor::{
    ENTRY_FIELDS, Editor as ProjectEditor, FieldReader, format_date_field as date, task_mode_name,
    untitled_project,
};
use projcore::{LinkType, Project, Task};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
mod gantt;
pub(super) use gantt::*;
mod commands;
pub(super) use commands::*;
mod cell;
mod task_info;
pub(super) use cell::*;
pub(super) use task_info::*;
mod clip;
pub(super) use clip::*;
mod timeline;
mod zoom;
mod zoom_dialogs;
pub(super) use timeline::*;
pub(super) use zoom::{WheelZoom, available as timescale_available};
pub(super) use zoom_dialogs::{apply_timescale, apply_zoom};
mod rows;
pub(super) use rows::*;
mod columns;
mod selection;
pub(super) use columns::{ARROW_W, EDGE_W, WidthDrag, committed_widths, project_width_release_all};
mod autofilter;
pub(super) use autofilter::{AUTO_FILTER_STATUS, MENU_ROW_H, autofilter_click, menu_json};

/// Whole rows a PageUp/PageDown steps by: the rows that fit in the
/// `project-body` probe, less one, so the cursor's landing row stays on
/// screen. Always at least 1.
fn page_rows_for(body_h: f32) -> usize {
    ((body_h / ROW_H).floor() as usize).saturating_sub(1).max(1)
}

pub(super) struct ProjectView {
    pub ed: ProjectEditor,
    pub scroll: UniformListScrollHandle,
    pub table_x: PaneOffset,
    pub gantt_x: PaneOffset,
    pub table_w: f32,
    pub gantt_w: f32,
    pub scale: GanttScale,
    pub prompt: Option<ProjectPrompt>,
    pub col: usize,
    /// PageUp/PageDown step the cursor by this many rows. `project_key`
    /// refreshes it from the `project-body` probe height on each keydown;
    /// until the first layout it stays at the default of 10.
    pub page_rows: usize,
    pub cell: Option<CellEdit>,
    /// The cell cursor is on the entry row below the last task, where typing
    /// appends a task, as in Project; see [`Self::on_entry_row`]. Entering it
    /// selects the last task, but later edits (Redo, an agent's `task.add`, a
    /// reload) can move the editor's selection, so on the entry row read
    /// [`Self::cursor_row`] and [`Self::selected_uid`], never `ed.sel()`.
    pub entry: bool,
    pub exported: Option<String>,
    /// The whole pane's width, which the Timeline spans.
    pub width: f32,
    /// Whether the Timeline pane is shown: window view state, never saved.
    pub timeline: bool,
    /// The press on the Timeline's view box while its button is held: the
    /// pointer's x and the chart offset then. A drag of the box is measured
    /// from it; `None` when no press is held, and a drag without one does nothing.
    pub timeline_press: Option<(f32, f32)>,
    /// The press on one of the view box's edges while its button is held;
    /// see [`timeline::EdgePress`]. A drag of it sets the chart's zoom.
    pub timeline_edge: Option<timeline::EdgePress>,
    /// The Timeline's height the user dragged its grip to; `None` is
    /// [`timeline::TIMELINE_H`]. Window view state, never saved (like `split`).
    pub timeline_h: Option<f32>,
    /// The press on the Timeline's grip while its button is held: the
    /// pointer's y and the pane's height then.
    pub grip_press: Option<(f32, f32)>,
    /// Smooth Ctrl+wheel deltas not yet worth a zoom step (`wheel_zoom`).
    pub wheel_acc: f32,
    /// The window's height, which bounds the Timeline's (half of it at most).
    pub height: f32,
    /// Gantt Chart Format › Bar Styles › Critical Tasks: critical bars drawn in
    /// the critical colour. Window view state, never saved.
    pub show_critical: bool,
    /// Gantt Chart Format › Bar Styles › Baseline: baseline bars drawn under
    /// the current bars. Window view state, never saved.
    pub show_baseline: bool,
    /// The table pane width the user dragged the split bar to; `None` is the
    /// default. Window view state, never saved.
    pub split: Option<f32>,
    /// The Gantt's zoom in pixels per day; `None` is [`DEFAULT_DAY_W`] (the
    /// Days level). Window view state, never saved or undone.
    pub zoom_day_w: Option<f32>,
    /// A resource-levelling pass that has been asked for but not yet run:
    /// its token and the leveled state it sets. The status bar says `Busy`
    /// while it is here; see [`request_level_pass`].
    pub busy: Option<(u64, bool)>,
    /// The token of the pending pass whose frame callback render has already
    /// registered, so it registers once. A callback whose token no longer
    /// matches `busy` does nothing.
    pub scheduled: Option<u64>,
    /// The selection's anchor (a task uid and column): the corner that stays
    /// put while Shift+arrows, a drag or Shift+click move the cursor. The
    /// selection is the bounding box of the anchor and the cursor, computed by
    /// [`ProjectView::selection`]; `None` is no range.
    pub anchor: Option<(i32, usize)>,
    /// A press on a task's ID cell: the range stays whole rows (the anchor's
    /// and the cursor's columns are pinned) while the drag moves over rows.
    pub row_drag: bool,
    /// The range was made by a press on a task's ID cell (Project's row
    /// header) or by Shift+Space (#1534), and nothing has reshaped it since: Delete over it deletes
    /// the cursor's task rather than clearing cells (#1260). Only meaningful
    /// while `anchor` is set.
    pub row_header: bool,
    /// A left press on a cell that has not seen its release yet; `drag_over`
    /// extends the range only while this is set. The window-level mouse-up
    /// clears it, like the sheet's `grid_release`.
    pub dragging: bool,
    /// A drag crossed onto another cell (or a Shift/ID press made a range):
    /// the cell/row/body `on_click` that gpui delivers on release must not
    /// clear what the gesture selected, so `cell_click` swallows that click.
    pub drag_made_range: bool,
    /// The file the plan was opened from, while a format plugin writes its
    /// type: each save of that type passes it back ([`Source`]).
    pub source: Option<Source>,
    /// What the UI plugins last answered for this view (#1532): control
    /// state by plan revision and selection, overlay shapes by revision and
    /// scale (`plugin_ui`).
    pub plugin_cache: std::cell::RefCell<crate::plugin_ui::PluginCache>,
    /// A heading edge held down (#1533): the column follows the pointer and
    /// one undo step records the width on release.
    pub width_drag: Option<WidthDrag>,
}

impl ProjectView {
    fn new(project: Project, dirty: bool) -> Self {
        let ed = ProjectEditor::restored(project, dirty);
        let scale = gantt_scale(&ed);
        // A plan without tasks has only the entry row.
        let entry = ed.project().tasks.is_empty();
        Self {
            ed,
            scroll: UniformListScrollHandle::new(),
            table_x: PaneOffset::default(),
            gantt_x: PaneOffset::default(),
            table_w: 590.,
            gantt_w: 590. - GANTT_INSET - SCROLLBAR_W,
            scale,
            prompt: None,
            col: COL_NAME,
            page_rows: 10,
            cell: None,
            entry,
            exported: None,
            width: 590. + SCROLLBAR_W,
            timeline: true,
            timeline_press: None,
            timeline_edge: None,
            timeline_h: None,
            grip_press: None,
            wheel_acc: 0.,
            height: 700.,
            show_critical: true,
            show_baseline: true,
            split: None,
            zoom_day_w: None,
            busy: None,
            scheduled: None,
            anchor: None,
            row_drag: false,
            row_header: false,
            dragging: false,
            drag_made_range: false,
            plugin_cache: crate::plugin_ui::new_cache(),
            width_drag: None,
            source: None,
        }
    }

    /// `bar` as the Gantt draws it under the Gantt Chart Format toggles.
    pub fn styled_bar(&self, bar: GanttBar) -> GanttBar {
        styled_bar(bar, self.show_critical, self.show_baseline)
    }

    /// Runs every frame, so it reveals the selected column only when the table
    /// pane resizes: a table-scrollbar drag that hides that column must stay
    /// where it was put.
    pub fn layout(&mut self, width: f32) {
        let old = self.table_w;
        self.width = width;
        self.apply_widths();
        if self.table_w != old {
            self.reveal_col();
        }
    }

    fn apply_widths(&mut self) {
        self.table_w =
            split_table_width_for(self.width, self.split, self.table_total(), self.widths()[0]);
        self.gantt_w = (self.width - self.table_w - GANTT_INSET - SCROLLBAR_W).max(0.);
        self.refresh_schedule_layout();
    }

    /// Moves the split bar. It writes `table_w` itself, so the next frame's
    /// `layout` sees no resize and leaves the table where it was scrolled.
    pub fn set_split(&mut self, table_w: f32) {
        self.split = Some(split_table_width_for(
            self.width,
            Some(table_w),
            self.table_total(),
            self.widths()[0],
        ));
        self.apply_widths();
    }

    pub fn reset_split(&mut self) {
        self.split = None;
        self.apply_widths();
    }

    pub fn refresh_schedule_layout(&mut self) {
        self.scale = GanttScale {
            day_w: self.zoom_day_w.unwrap_or(DEFAULT_DAY_W),
            ..gantt_scale(&self.ed)
        };
        self.clamp_offsets();
    }

    /// The drawn scale: the plan's, widened to the viewport so a short plan's chart still has
    /// days to its right edge. Scrolling clamps to the plan's own scale, not this one.
    pub fn chart_scale(&self) -> GanttScale {
        GanttScale {
            days: self
                .scale
                .days
                .max((self.gantt_w / self.scale.day_w).ceil() as i64),
            ..self.scale
        }
    }

    fn clamp_offsets(&mut self) {
        self.table_x.clamp(self.table_total(), self.table_w);
        self.gantt_x.clamp(self.scale.width(), self.gantt_w);
    }

    /// Whether the cell cursor is on the entry row. A plan without tasks has
    /// no other row.
    pub fn on_entry_row(&self) -> bool {
        self.entry || self.ed.project().tasks.is_empty()
    }

    /// Latch the entry row once the plan has no tasks, so tasks that appear
    /// later without the user moving (an agent's `task.add`, Redo, a reload)
    /// go above the cursor rather than under it. Called wherever an edit may
    /// have emptied the plan; a new view latches it too.
    pub fn latch_entry_row(&mut self) {
        if self.ed.project().tasks.is_empty() {
            self.entry = true;
        }
    }

    /// The row the cell cursor is on: a task's index, or the task count for
    /// the entry row.
    pub fn cursor_row(&self) -> usize {
        if self.on_entry_row() {
            self.ed.project().tasks.len()
        } else {
            self.ed.sel()
        }
    }

    /// The cursor's row as shown, where collapsed summaries and the AutoFilter
    /// hide rows: its position among the displayed rows (0 for a selection
    /// none shows, which an edit's `sel_into_display` prevents), or their
    /// count on the entry row.
    pub fn display_row(&self) -> usize {
        let rows = self.ed.display_rows();
        if self.on_entry_row() {
            rows.len()
        } else {
            rows.iter().position(|&i| i == self.ed.sel()).unwrap_or(0)
        }
    }

    /// The task the cursor is on; none on the entry row, so commands that act
    /// on the selected task do nothing there.
    pub fn selected_uid(&self) -> Option<i32> {
        if self.on_entry_row() {
            None
        } else {
            self.ed.selected_uid()
        }
    }

    /// Put the cursor on the entry row. The selection moves to the last
    /// visible task, leaving collapsed summaries collapsed.
    pub fn enter_entry_row(&mut self) {
        self.cancel_prompt();
        self.entry = true;
        self.ed.select_last_visible();
    }

    /// Navigation and horizontal scrolling only; command completion owns row reveal.
    pub fn key(&mut self, key: &str, shift: bool) -> bool {
        if matches!(key, "left" | "right" | "tab") {
            let left = key == "left" || (key == "tab" && shift);
            self.col = if left {
                self.col.saturating_sub(1)
            } else {
                (self.col + 1).min(COLUMN_COUNT - 1)
            };
            self.reveal_col();
            return true;
        }
        // Home and End stay on the row and go to its first and last field, as in Project.
        if matches!(key, "home" | "end") {
            self.col = if key == "home" { 0 } else { COLUMN_COUNT - 1 };
            self.reveal_col();
            return true;
        }
        // PageUp/PageDown step a screen of rows, as in Project; rows hidden
        // under collapsed summaries are skipped, and past the last task the
        // cursor lands on the entry row.
        if matches!(key, "pageup" | "pagedown") {
            let rows = self.ed.display_rows();
            if rows.is_empty() {
                return true;
            }
            let at = self.display_row();
            let target = if key == "pagedown" {
                (at + self.page_rows).min(rows.len())
            } else {
                at.saturating_sub(self.page_rows)
            };
            if target == rows.len() {
                self.enter_entry_row();
            } else {
                self.entry = false;
                self.ed.select(rows[target]);
            }
            return true;
        }
        // Rows under collapsed summaries are skipped.
        let last = self.ed.display_rows().last().copied();
        let index = match key {
            // From the entry row, Up goes to the last task.
            "up" if self.on_entry_row() => match last {
                Some(last) => last,
                None => return true,
            },
            "up" => self.ed.visible_step(self.ed.sel(), -1),
            // Down from the last task goes to the entry row, as in Project.
            "down"
                if self.on_entry_row()
                    || last.is_none_or(|last| self.ed.sel() == last)
                    || !self.ed.display_rows().contains(&self.ed.sel()) =>
            {
                self.enter_entry_row();
                return true;
            }
            "down" => self.ed.visible_step(self.ed.sel(), 1),
            _ => return false,
        };
        self.entry = false;
        self.ed.select(index);
        true
    }

    /// Project's Ctrl navigation: Ctrl+Left/Right are Home/End, Ctrl+Up/Down go
    /// to the first/last visible task, and Ctrl+Home/End do both. An empty plan
    /// has only the entry row, so only the column moves.
    pub fn ctrl_key(&mut self, key: &str) -> bool {
        let (col, first) = match key {
            "left" => return self.key("home", false),
            "right" => return self.key("end", false),
            "up" => (None, true),
            "down" => (None, false),
            "home" => (Some(0), true),
            "end" => (Some(COLUMN_COUNT - 1), false),
            _ => return false,
        };
        let rows = self.ed.display_rows();
        let row = if first { rows.first() } else { rows.last() };
        if let Some(&row) = row {
            self.entry = false;
            self.ed.select(row);
        }
        if let Some(col) = col {
            self.col = col;
            self.reveal_col();
        }
        true
    }

    /// The table and chart scrollbars' handles. Each strip is drawn at the width
    /// its handle clamps against: gpui-component takes the track length from the
    /// strip, so the two must be the same number.
    pub fn pane_scrolls(&self) -> (PaneScroll, PaneScroll) {
        (
            PaneScroll {
                offset: self.table_x.clone(),
                content: self.table_total(),
                viewport: self.table_w,
            },
            PaneScroll {
                offset: self.gantt_x.clone(),
                content: self.scale.width(),
                viewport: self.gantt_w,
            },
        )
    }

    pub fn pan_gantt(&mut self, right: bool) {
        self.gantt_x.set(
            self.gantt_x.get()
                + if right {
                    self.scale.day_w
                } else {
                    -self.scale.day_w
                },
        );
        self.clamp_offsets();
    }

    /// Task › Editing › Scroll to Task: pan the chart so the selected task's
    /// bar starts a day in from its left edge. The scale is refreshed first:
    /// an edit since the last layout may have moved its origin or end.
    pub fn scroll_to_task(&mut self) {
        self.refresh_schedule_layout();
        let bar = self
            .selected_uid()
            .and_then(|uid| self.ed.project().task(uid))
            .and_then(|task| gantt_bar(&self.ed, task, self.scale));
        if let Some(bar) = bar {
            self.gantt_x
                .set((bar.start - 1).max(0) as f32 * self.scale.day_w);
            self.clamp_offsets();
        }
    }

    /// Alt+End: pan the chart so the project's displayed finish day is the
    /// last fully visible day column, right-aligned at the chart's right
    /// edge. The scale is refreshed first, as in `scroll_to_task`.
    pub fn scroll_to_finish(&mut self) {
        self.refresh_schedule_layout();
        let finish = self.ed.disp_project_finish().day_number();
        self.gantt_x.set(self.scale.x(finish + 1) - self.gantt_w);
        self.clamp_offsets();
    }

    /// The pane at `x`, relative to the split area's left edge. The boundary is
    /// the middle of the split bar, where `SplitDrag` puts it.
    pub fn pane_at(&self, x: f32) -> ProjectPane {
        if x < self.table_w + GANTT_INSET / 2. {
            ProjectPane::Table
        } else {
            ProjectPane::Chart
        }
    }

    /// Scroll `pane` right by `dx` pixels (left when negative), clamped.
    /// Returns whether its offset moved.
    pub fn wheel_x(&mut self, pane: ProjectPane, dx: f32) -> bool {
        let offset = match pane {
            ProjectPane::Table => self.table_x.clone(),
            ProjectPane::Chart => self.gantt_x.clone(),
        };
        let old = offset.get();
        offset.set(old + dx);
        self.clamp_offsets();
        offset.get() != old
    }

    /// A horizontal wheel delta over the split area: scroll the pane under the
    /// pointer at `x` (relative to the area's left edge). `delta_x` is gpui's,
    /// which is negative to scroll right, as Shift+wheel down is on Windows.
    pub fn wheel(&mut self, x: f32, delta_x: f32) -> bool {
        delta_x != 0. && self.wheel_x(self.pane_at(x), -delta_x)
    }
}

/// The two horizontally scrolled panes of the Project split area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectPane {
    Table,
    Chart,
}

/// A pane's horizontal offset, shared with its scrollbar: gpui-component calls
/// `ScrollbarHandle::set_offset` from a mouse handler that has no `Docxy` to write to.
#[derive(Clone, Default, Debug)]
pub(super) struct PaneOffset(Rc<Cell<f32>>);

impl PaneOffset {
    pub fn get(&self) -> f32 {
        self.0.get()
    }

    pub fn set(&self, x: f32) {
        self.0.set(x);
    }

    /// Keep the offset within `[0, content - viewport]`.
    fn clamp(&self, content: f32, viewport: f32) {
        self.set(self.get().clamp(0., (content - viewport).max(0.)));
    }
}

/// The scrollbar's view of one horizontal pane, rebuilt each render from the
/// same widths the pane and its strip are drawn at.
#[derive(Clone)]
pub(super) struct PaneScroll {
    offset: PaneOffset,
    content: f32,
    viewport: f32,
}

impl ScrollbarHandle for PaneScroll {
    fn offset(&self) -> Point<Pixels> {
        point(px(-self.offset.get()), px(0.))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.offset.set(-f32::from(offset.x));
        self.offset.clamp(self.content, self.viewport);
    }

    fn content_size(&self) -> Size<Pixels> {
        size(px(self.content), px(SCROLLBAR_W))
    }
}

/// Avoid an Editor snapshot when an outline limit makes this a no-op.
pub(super) fn indent_project(tab: &mut DocTab, delta: i32) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    let Some(uid) = v.selected_uid() else {
        return;
    };
    let level = v.ed.project().task(uid).unwrap().outline_level;
    if (i64::from(level) + i64::from(delta)).clamp(1, 20) == i64::from(level) {
        return;
    }
    if let Err(e) = v.ed.indent(uid, delta) {
        tab.status = e.into();
        return;
    }
    tab.dirty = v.ed.dirty();
    crate::bump_edit_generation();
}

impl Docxy {
    /// Project regions resolve their probes through the same `lookup`
    /// `region_bounds_with` was given (`get` for rect/shot, `on_screen` for
    /// pointer verbs).
    pub(super) fn project_region_bounds(
        &self,
        region: harness::Region,
        lookup: fn(&Probes, &str) -> Option<Bounds<Pixels>>,
    ) -> Result<Bounds<Pixels>, String> {
        let Some(Surface::Project(v)) = self.tabs.get(self.active).map(|t| &t.surface) else {
            return Err("the active tab is not a loaded Project".into());
        };
        let probes = self.probes.borrow();
        project_region(v, &probes, region, lookup)
    }
}

fn project_region(
    v: &ProjectView,
    probes: &Probes,
    region: harness::Region,
    lookup: fn(&Probes, &str) -> Option<Bounds<Pixels>>,
) -> Result<Bounds<Pixels>, String> {
    // The strips are probed under their region names; they need no Gantt viewport.
    if let harness::Region::ProjectTimeline
    | harness::Region::ProjectTimelineBoxLeft
    | harness::Region::ProjectTimelineBoxRight
    | harness::Region::ProjectTimelineGrip = region
    {
        if !v.timeline {
            return Err("the Timeline is hidden (View > Split View > Timeline)".into());
        }
        return lookup(probes, &harness::region_name(region))
            .ok_or_else(|| "the Timeline has not been laid out".into());
    }
    if let harness::Region::ProjectHbarTable
    | harness::Region::ProjectHbarChart
    | harness::Region::ProjectVbar = region
    {
        let name = harness::region_name(region);
        return lookup(probes, &name)
            .ok_or_else(|| format!("the {name} scrollbar has not been laid out"));
    }
    if let harness::Region::ProjectSplit = region {
        return lookup(probes, &harness::region_name(region))
            .ok_or_else(|| "the split bar has not been laid out".into());
    }
    let body = lookup(probes, "project-body").ok_or("the Project body has not been laid out")?;
    if let harness::Region::ProjectHeading(col)
    | harness::Region::ProjectHeadingArrow(col)
    | harness::Region::ProjectHeadingEdge(col) = region
    {
        let part = match region {
            harness::Region::ProjectHeadingArrow(_) => "project-heading-arrow",
            harness::Region::ProjectHeadingEdge(_) => "project-heading-edge",
            _ => "project-heading",
        };
        let heading = lookup(probes, &format!("{part}:{col}"))
            .ok_or("the entry table's headings have not been laid out")?;
        // The heading row sits above the body: clip to the table pane's
        // columns only.
        let table = Bounds {
            origin: point(body.origin.x, heading.origin.y),
            size: size(px(v.table_w), heading.size.height),
        };
        return intersect(heading, table).ok_or_else(|| {
            format!(
                "the {} heading is outside the table viewport",
                COLUMNS[col.min(COLUMN_COUNT - 1)]
            )
        });
    }
    let viewport =
        gantt_viewport(body, v.table_w, v.gantt_w).ok_or("the Gantt viewport is empty")?;
    match region {
        harness::Region::Cells(r0, c0, r1, c1) => {
            if r0 != r1 || c0 != c1 || c0 as usize >= COLUMN_COUNT {
                return Err("Project regions address one entry-table cell".into());
            }
            // Rows are drawn rows; the entry row sits just below the last one.
            let row = match shown_task(&v.ed, r0 as usize) {
                Some(ShownRow::Task(i)) => v.ed.project().tasks[i].id.to_string(),
                Some(ShownRow::Entry) => "entry".into(),
                None => return Err("No task at this row".into()),
            };
            let cell = lookup(probes, &format!("project-cell:{row}:{c0}"))
                .ok_or("Project cell is not rendered")?;
            // The absolute probe fills the padding box; include the cell's 1px border.
            let cell = Bounds {
                origin: cell.origin - point(px(1.), px(1.)),
                size: cell.size + size(px(2.), px(2.)),
            };
            let table = Bounds {
                origin: body.origin,
                size: size(px(v.table_w), body.size.height),
            };
            intersect(cell, table).ok_or("Project cell is outside the table viewport".into())
        }
        harness::Region::Gantt => Ok(viewport),
        harness::Region::Bar(id) => {
            let task =
                v.ed.project()
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .ok_or_else(|| format!("no task with ID {id}"))?;
            if gantt_bar(&v.ed, task, v.scale).is_none() {
                return Err(format!("task {id} has no schedule result"));
            }
            let bar = lookup(probes, &format!("bar:{id}"))
                .ok_or_else(|| format!("task {id} row is not rendered (scrolled out of view)"))?;
            intersect(bar, viewport)
                .ok_or_else(|| format!("task {id} bar is outside the Gantt viewport"))
        }
        _ => Err("not a Project region".into()),
    }
}

/// `body_h` is the last frame's `project-body` height; `None` before the first layout.
pub(super) fn project_state(
    v: &ProjectView,
    body_h: Option<f32>,
) -> Vec<(String, ctlcore::json::Json)> {
    use ctlcore::json::Json;
    let scale = GanttScale {
        day_w: v.scale.day_w,
        ..gantt_scale(&v.ed)
    };
    let count = v.ed.project().tasks.len();
    let shown = v.ed.display_rows().len();
    let scroll_y = -f32::from(v.scroll.0.borrow().base_handle.offset().y);
    let mut entries = vec![
        ("selected_task".into(), Json::Num(v.cursor_row() as f64)),
        (
            "selected_row".into(),
            Json::Num((v.display_row() + 1) as f64),
        ),
        ("tasks".into(), Json::Num(count as f64)),
        ("widths".into(), widths_json(&v.widths())),
        ("gantt_day_w".into(), Json::Num(f64::from(v.day_w()))),
        ("gantt_zoom".into(), Json::Str(zoom::label(v.day_w()))),
        ("filtered".into(), Json::Bool(v.ed.is_filtered())),
        (
            "filler_rows".into(),
            Json::Num(body_h.map_or(0, |h| filler_rows(h, scroll_y, shown)) as f64),
        ),
        (
            "prompt".into(),
            Json::Str(
                v.prompt
                    .as_ref()
                    .map(|p| format!("{}:{}", p.kind.name(), p.buf))
                    .unwrap_or_else(|| "none".into()),
            ),
        ),
        (
            "selected_name".into(),
            Json::Str(
                v.selected_uid()
                    .and_then(|uid| v.ed.project().task(uid))
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
            ),
        ),
        (
            "exported".into(),
            Json::Str(v.exported.clone().unwrap_or_else(|| "none".into())),
        ),
        ("table_w".into(), Json::Num(f64::from(v.table_w))),
        // Whether the pane shows every column, no table scrolling needed (#1534).
        (
            "table_fits".into(),
            Json::Bool(v.table_total() <= v.table_w),
        ),
        ("gantt_w".into(), Json::Num(f64::from(v.gantt_w))),
    ];
    // What the Gantt draws, after the Gantt Chart Format toggles.
    entries.extend(v.ed.project().tasks.iter().flat_map(|t| {
        let bar = gantt_bar(&v.ed, t, scale).map(|b| v.styled_bar(b));
        [
            (
                format!("bar_{}", t.id),
                Json::Str(bar.map_or_else(|| "none".into(), GanttBar::state)),
            ),
            (
                format!("baseline_{}", t.id),
                Json::Str(
                    bar.and_then(|b| b.baseline)
                        .map_or_else(|| "none".into(), |(s, e)| format!("{s}-{e}")),
                ),
            ),
        ]
    }));
    entries.extend(project_cell_state(v));
    entries.extend(selection::selection_state(v));
    entries.extend(timeline_state(v));
    entries.push(("undo_depth".into(), Json::Num(v.ed.undo_depth() as f64)));
    entries.push(("redo_depth".into(), Json::Num(v.ed.redo_depth() as f64)));
    entries
}

/// Whether `path` opens as a Project tab: a type some project format reads
/// (`.yppx`, `.xml`, `.mpp`, a format plugin's).
pub(super) fn is_project_path(path: &Path) -> bool {
    project_formats::global().is_project_path(path)
}

/// Whether the tab's file is one its Save never writes: a project of a type
/// nothing writes (`.mpp` unless a format plugin writes it), or a document
/// imported from a Word 97-2003 file (#634).
pub(super) fn is_imported(tab: &DocTab) -> bool {
    match tab.kind {
        Kind::Project => tab
            .path
            .as_deref()
            .is_some_and(|p| project_formats::global().is_import(p)),
        Kind::Docx => tab.import.binary_source,
        _ => false,
    }
}

/// `status`, with the import note when `path` is of a type nothing writes.
fn imported_status(status: String, path: Option<&Path>) -> SharedString {
    match path.and_then(|p| project_formats::global().import_note(p)) {
        Some(note) => format!("{status}, {note}; Save As .yppx or MSPDI to keep edits").into(),
        None => status.into(),
    }
}

fn project_tab(
    title: SharedString,
    path: Option<PathBuf>,
    surface: Surface,
    dirty: bool,
    status: SharedString,
) -> DocTab {
    DocTab {
        kind: Kind::Project,
        title,
        path,
        surface,
        dirty,
        status,
        comments: vec![],
        tracked_comment_ids: Default::default(),
        comments_removed_all: false,
        used_comment_ids: Default::default(),
        pictures: Default::default(),
        pkg: None,
        notes: vec![],
        markdown: false,
        hf_edit: None,
        bundle_html: None,
        load_failed: false,
        dialogs: crate::dialog::DialogStack::default(),
        access: crate::open_mode::Access::default(),
        last_hot: Default::default(),
        converted_docx: None,
        pending_conversion: false,
        mail: Default::default(),
        field_codes: false,
        import: Default::default(),
    }
}

/// The plan in the file at `path`, by its type's reader.
pub(super) fn project_from_path(path: &Path) -> Result<Project, String> {
    project_formats::global()
        .read(path)
        .map(|(project, _)| project)
}

/// A view of the plan in the file at `path`, keeping the file as the
/// [`Source`] of its saves when a format plugin writes its type.
fn view_from_path(path: &Path) -> Result<ProjectView, String> {
    let formats = project_formats::global();
    let (project, bytes) = formats.read(path)?;
    let mut view = ProjectView::new(project, false);
    view.source = formats.source(path, bytes);
    Ok(view)
}

pub(super) fn project_tab_from_path(path: &Path) -> DocTab {
    let (surface, status) = match view_from_path(path) {
        Ok(v) => {
            let tasks = v.ed.project().tasks.len();
            let status = imported_status(format!("loaded — {tasks} tasks"), Some(path));
            (Surface::Project(v), status)
        }
        Err(e) => (
            Surface::Placeholder,
            format!("project load error: {e}").into(),
        ),
    };
    project_tab(
        file_name(path).into(),
        Some(path.into()),
        surface,
        false,
        status,
    )
}

/// A blank plan (#588): the tab is titled `title` — the run-minted
/// `Project<n>` — and the plan's name is that title, so the tab, the
/// `proj.new` reply and the saved `<Name>` can never disagree. A restored
/// plan with a legacy title (`Untitled.yppx`) is not a minted name, so its
/// plan keeps `untitled_project()`'s default `Project1`.
pub(super) fn new_project_tab(title: &str) -> DocTab {
    let mut project = untitled_project();
    if crate::doc_name::project_number(title).is_some() {
        project.name = title.into();
    }
    project_tab(
        title.into(),
        None,
        Surface::Project(ProjectView::new(project, false)),
        false,
        "new project".into(),
    )
}

/// Where a save to `path` goes: an extensionless name becomes `.yppx`, a
/// type no format writes is refused.
fn save_target(path: &Path) -> Result<PathBuf, String> {
    project_formats::global().registry.save_target(path)
}

/// Write the plan to `path` by its type's writer, handing `source` (the
/// file it was opened from) to a save of that file's type.
fn write_project(
    ed: &ProjectEditor,
    path: &Path,
    source: Option<&Source>,
) -> Result<(PathBuf, usize), String> {
    let path = save_target(path)?;
    let source = source.and_then(|s| s.for_target(&path));
    let bytes = project_formats::global()
        .registry
        .write(ed.project(), &path, source)?;
    opccore::fsio::write_atomic(&path, &bytes).map_err(|e| format!("save failed: {e}"))?;
    Ok((path, bytes.len()))
}

#[derive(Debug, PartialEq)]
pub(super) enum SaveDecision {
    InPlace(PathBuf),
    Dialog { suggested: String },
    RefuseHarness(String),
    Unsaveable,
}

pub(super) fn save_decision(tab: &DocTab, harness: bool, explicit_save_as: bool) -> SaveDecision {
    if !matches!(tab.surface, Surface::Project(_)) {
        return SaveDecision::Unsaveable;
    }
    if !explicit_save_as && !is_imported(tab) {
        if let Some(path) = &tab.path {
            return SaveDecision::InPlace(path.clone());
        }
    }
    if harness {
        return SaveDecision::RefuseHarness(
            "This project needs Save As, and a harness instance cannot open the Save As dialog; use the harness save-as verb"
                .into(),
        );
    }
    // The type Save As proposes: the plan's own when something writes it,
    // else a format plugin's declared default, else .yppx.
    SaveDecision::Dialog {
        suggested: project_formats::global()
            .suggested_name(tab.path.as_deref(), tab.title.as_ref()),
    }
}

pub(super) fn apply_save(tab: &mut DocTab, target: &Path) -> Result<usize, String> {
    // Levelling changes only the displayed dates, not what is written, but a
    // pass asked for runs first so the save's message is the last word and
    // the tab it reports on is settled.
    flush_level_pass(tab);
    if !commit_project_cell(tab) {
        return Err(tab.status.to_string());
    }
    let result = match &tab.surface {
        Surface::Project(v) => write_project(&v.ed, target, v.source.as_ref()),
        _ => Err("This project could not be loaded and cannot be saved".into()),
    };
    match result {
        Ok((path, n)) => {
            if let Surface::Project(v) = &mut tab.surface {
                v.ed.commit_save();
            }
            tab.dirty = false;
            tab.title = file_name(&path).into();
            tab.status = format!("saved {n} bytes → {}", path.display()).into();
            tab.path = Some(path);
            Ok(n)
        }
        Err(e) => {
            tab.status = e.clone().into();
            Err(e)
        }
    }
}

/// Cancellation is a status change only, shared by the native dialog and tests.
pub(super) fn finish_project_save(tab: &mut DocTab, target: Option<&Path>) {
    match target {
        Some(path) => {
            let _ = apply_save(tab, path);
        }
        None => tab.status = "save cancelled".into(),
    }
}

pub(super) fn restore_project_tab(t: &PersistTab) -> DocTab {
    let path = t.path.as_deref().map(PathBuf::from);
    let recovery = if let Some(hot) = &t.hot {
        let hp = Path::new(hot);
        match project_from_path(hp) {
            Ok(mut p) => {
                for name in &t.unreadable {
                    if !p
                        .package
                        .unreadable
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(name))
                    {
                        p.package.unreadable.push(name.clone());
                    }
                }
                let status = imported_status(
                    if t.dirty {
                        "unsaved — restored"
                    } else {
                        "loaded"
                    }
                    .into(),
                    path.as_deref(),
                );
                let mut view = ProjectView::new(p, t.dirty);
                // The sidecar holds the plan; a plugin's save still gets the
                // file it was opened from as its source.
                view.source = path
                    .as_deref()
                    .and_then(|p| project_formats::global().source_at(p));
                return project_tab(
                    t.title.clone().into(),
                    path,
                    Surface::Project(view),
                    t.dirty,
                    status,
                );
            }
            Err(_) => {
                if hp.exists() {
                    "sidecar unreadable"
                } else {
                    "sidecar missing"
                }
            }
        }
    } else {
        "no sidecar recorded"
    };
    if !t.dirty {
        return match path {
            Some(path) => project_tab_from_path(&path),
            // Clean and never saved: keep the stored title verbatim (#588).
            None => new_project_tab(&t.title),
        };
    }
    let (surface, status) = match path.as_deref() {
        Some(orig) => match view_from_path(orig) {
            Ok(v) => (
                Surface::Project(v),
                imported_status(
                    format!(
                        "{recovery} — reloaded {}; unsaved edits lost",
                        file_name(orig)
                    ),
                    Some(orig),
                ),
            ),
            Err(e) => (
                Surface::Placeholder,
                format!(
                    "{recovery} — {} could not be read: {e}; unsaved edits lost",
                    file_name(orig)
                )
                .into(),
            ),
        },
        None => (
            Surface::Placeholder,
            format!("{recovery} — unsaved edits lost; no file to reload").into(),
        ),
    };
    project_tab(t.title.clone().into(), path, surface, false, status)
}

/// The status bar's `New Tasks: …` item, with the glyph Microsoft Project
/// draws beside it: a pushpin when new tasks are manually scheduled, a gear
/// when they are auto-scheduled.
pub(crate) fn new_tasks_label(manual: bool) -> String {
    format!(
        "{} New Tasks: {}",
        new_tasks_glyph(manual),
        task_mode_name(manual)
    )
}

fn new_tasks_glyph(manual: bool) -> &'static str {
    if manual { "📌" } else { "⚙" }
}

/// The status bar's items as `status-read` reports them, left to right, each
/// with a stable id: the application state (not on a placeholder), on a
/// Project the new-tasks mode, the last message, and on a document its
/// page/word count. Render draws the same texts from the same functions, so
/// the two cannot drift.
pub(crate) fn status_items(tab: &DocTab, app_dialog: bool) -> Vec<(&'static str, String)> {
    let mut items = Vec::new();
    if let Some(state) = tab_app_state(tab, app_dialog) {
        items.push(("state", state.label().to_string()));
    }
    if let Surface::Project(v) = &tab.surface {
        items.push((
            "new-tasks",
            new_tasks_label(v.ed.project().new_tasks_are_manual),
        ));
    }
    if let Surface::Project(v) = &tab.surface
        && v.ed.is_filtered()
    {
        items.push(("filter", AUTO_FILTER_STATUS.to_string()));
    }
    items.push(("message", tab.status.to_string()));
    // Manual calculation with values out of date (#674).
    if let Surface::Sheet(v) = &tab.surface {
        if let Some(word) = crate::sheet_calc::status_word(v) {
            items.push(("calculate", word.to_string()));
        }
    }
    if let Some(stats) = crate::doc_stats_text(tab) {
        items.push(("stats", stats));
    }
    items
}

/// A row's Entry cells, read through the task field registry so a cell and
/// `task.get`'s field of the same name cannot disagree.
pub(crate) fn project_row(ed: &ProjectEditor, task: &Task) -> [String; COLUMN_COUNT] {
    let reader = FieldReader::new(ed);
    ENTRY_FIELDS.map(|field| reader.read(task, field).text)
}

pub(crate) const ROW_H: f32 = 28.;
/// The entry table's default column widths; a plan's own (`tableview.xml`,
/// dragged on a heading's right edge) replace them per column.
pub(crate) const DEFAULT_WIDTHS: [f32; COLUMN_COUNT] = [46., 132., 140., 90., 94., 94., 118., 133.];
/// The default widths' sum.
#[cfg(test)]
pub(crate) const TABLE_W: f32 = sum_widths();
/// Includes the divider, leaving space between clipped table text and the chart.
const GANTT_INSET: f32 = 6.;
/// gpui-component's private scrollbar `WIDTH`: it paints the bar in the last 16px
/// of its strip's cross axis, so a thinner strip clips the bar.
const SCROLLBAR_W: f32 = 16.;

#[cfg(test)]
const fn sum_widths() -> f32 {
    let mut total = 0.;
    let mut i = 0;
    while i < DEFAULT_WIDTHS.len() {
        total += DEFAULT_WIDTHS[i];
        i += 1;
    }
    total
}

/// The entry table's headings, in the rows' column widths and padding. A
/// right press on one selects its column and opens the column's menu
/// (#1208).
fn heading_cells(
    v: &ProjectView,
    pal: Pal,
    index: usize,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    // The plan's Column Titles text style (#1532), when it sets one.
    let style = crate::plugin_ui::category_style(v, "Column Titles");
    let row = h_flex().h(px(ROW_H)).items_center();
    let row = match &style {
        Some(style) => crate::plugin_ui::styled(row, style),
        None => row,
    };
    let widths = v.widths();
    let look = v.look();
    row.children(COLUMNS.iter().enumerate().map(|(col, name)| {
        let filtered = v.ed.filter(ENTRY_FIELDS[col]).is_some();
        div()
            .id(("project-heading", col))
            .relative()
            .h_full()
            .flex()
            .items_center()
            .w(px(widths[col]))
            .flex_none()
            .pl_2()
            .pr(px(ARROW_W + EDGE_W))
            .overflow_hidden()
            .whitespace_nowrap()
            .when(col == COL_NAME, |d| d.pl(px(8.)))
            .when(look.heading_hot(col), |d| {
                d.bg(selection::header_tint_color())
            })
            .child(probe(probes, format!("project-heading:{col}")))
            .child(*name)
            // A left press selects the column (#1534); the arrow and the
            // edge, drawn after, stop their own presses.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if this.active == index
                        && let Some(tab) = this.tabs.get_mut(index)
                    {
                        project_column_select(tab, col);
                    }
                    this.refocus(window, cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if this.active == index {
                        let _ = this.open_column_menu(col, ev.position, window, cx);
                    }
                }),
            )
            // The AutoFilter arrow (a funnel once the column filters): its
            // press never selects or drags anything.
            .child(
                div()
                    .id(("project-heading-arrow", col))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(EDGE_W))
                    .w(px(ARROW_W))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(9.))
                    .text_color(if filtered {
                        hsla_u(crate::accent::brand())
                    } else {
                        pal.dim
                    })
                    .child(probe(probes, format!("project-heading-arrow:{col}")))
                    .child(if filtered { "\u{29e9}" } else { "\u{25bc}" })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if this.active == index {
                            let _ = this.open_project_filter(col, window, cx);
                        }
                    })),
            )
            // The right edge: drag to resize, double-click for Best Fit.
            .child(
                div()
                    .id(("project-heading-edge", col))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(EDGE_W))
                    .cursor_col_resize()
                    .child(probe(probes, format!("project-heading-edge:{col}")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                            cx.stop_propagation();
                            if this.active == index
                                && let Some(Surface::Project(v)) =
                                    this.tabs.get_mut(index).map(|t| &mut t.surface)
                            {
                                v.begin_width_drag(col, f32::from(ev.position.x));
                                cx.notify();
                            }
                        }),
                    )
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if ev.click_count() >= 2 && this.active == index {
                            this.best_fit_column(col, window, cx);
                        }
                    })),
            )
    }))
}

/// A row's cells: a task's, or the empty entry row's when `task` is `None`.
fn editable_row_cells(
    v: &ProjectView,
    pal: Pal,
    task: Option<&Task>,
    row: usize,
    index: usize,
    indent: f32,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    window: &Window,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    let inactive = inactive_row(v.ed.project(), row);
    let values = task.map(|t| project_row(&v.ed, t)).unwrap_or_default();
    let (uid, summary) = (task.map(|t| t.uid), task.is_some_and(|t| t.summary));
    // The plan's text styles (#1532): the row's categories, then each
    // cell's own, when the formatting sets any.
    let critical = task
        .and_then(|t| v.ed.schedule().get(t.uid))
        .is_some_and(|r| r.critical);
    let row_style = task.and_then(|t| crate::plugin_ui::row_style(v, t, critical, inactive));
    let probe_row = task.map_or_else(|| "entry".to_string(), |t| t.id.to_string());
    let cursor_row = v.cursor_row();
    let widths = v.widths();
    // Once per row: which cells the selection fills and which ID cell is
    // tinted (#1534).
    let look = v.look();
    let selected_bg = |u: Option<i32>, col: usize| {
        if look.fill(u, col) {
            selection::cell_fill_color()
        } else if col == COL_ID && u.is_some_and(|u| look.id_hot(u)) {
            selection::header_tint_color()
        } else {
            hsla(0., 0., 0., 0.)
        }
    };
    let cells = h_flex()
        .h(px(ROW_H))
        .items_center()
        .when(inactive, |d| d.text_color(pal.dim).line_through());
    let cells = match &row_style {
        Some(style) => crate::plugin_ui::styled(cells, style),
        None => cells,
    };
    cells.children(values.into_iter().enumerate().map(|(col, value)| {
        let cell_style = crate::plugin_ui::cell_style(v, row_style.as_ref(), uid, COLUMNS[col]);
        let edit = v.cell.as_ref().filter(|c| c.uid == uid && c.col == col);
        let content = if let Some(edit) = edit {
            let measure = Measurer::new(window);
            let offset =
                edit.scroll_x(widths[col] - 24., |s| measure.width(s, 12., summary, false));
            line_edit_view(edit, offset, 16.).into_any_element()
        } else if let Some(uid) = uid.filter(|_| summary && col == COL_NAME) {
            // A summary's outline glyph: a click shows or hides its
            // subtasks without editing the cell or moving the cursor.
            // The press is swallowed too, so the cell's own press
            // listener cannot move the cursor before the click toggles.
            let glyph = if v.ed.is_collapsed(uid) { "▸" } else { "▾" };
            h_flex()
                .child(
                    div()
                        .id(("project-toggle", row))
                        .relative()
                        .flex_none()
                        .w(px(12.))
                        .cursor_pointer()
                        .child(probe(probes, format!("project-toggle:{probe_row}")))
                        .child(glyph)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if let Some(tab) = this.tabs.get_mut(index) {
                                toggle_project_collapse(tab, uid);
                            }
                            this.refocus(window, cx);
                        })),
                )
                .child(value)
                .into_any_element()
        } else {
            div().child(value).into_any_element()
        };
        let cell = div()
            .id(("project-cell", row * COLUMN_COUNT + col))
            .relative()
            .flex()
            .items_center()
            .w(px(widths[col]))
            .h(px(ROW_H))
            .flex_none()
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(selected_bg(uid, col));
        let cell = match &cell_style {
            Some(style) => crate::plugin_ui::styled(cell, style),
            None => cell,
        };
        cell.border_1()
            .border_color(if row == cursor_row && col == v.col {
                hsla_u(BRAND)
            } else {
                hsla(0., 0., 0., 0.)
            })
            .when(col == COL_NAME && edit.is_none(), |d| d.pl(px(8. + indent)))
            .child(probe(probes, format!("project-cell:{probe_row}:{col}")))
            .child(content)
            // The press starts (and the move extends) a drag selection;
            // the release is the window-level mouse-up, like the sheet.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    if this.active == index
                        && let Some(tab) = this.tabs.get_mut(index)
                    {
                        project_cell_press(tab, row, col, ev.modifiers.shift);
                    }
                    this.refocus(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _window, cx| {
                // A held left button is a drag's move; plain hover never
                // extends a range, however the press state stands.
                if ev.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if this.active == index
                    && let Some(tab) = this.tabs.get_mut(index)
                    && project_cell_drag_over(tab, row, col)
                {
                    cx.notify();
                }
            }))
            // Right-click selects the cell, then opens the row's menu.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if this.active == index {
                        let _ = this.open_row_menu(row, Some(col), ev.position, window, cx);
                    }
                }),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                cx.stop_propagation();
                if let Some(tab) = this.tabs.get_mut(index) {
                    project_row_click(tab, row, Some(col), ev.click_count() >= 2);
                }
                this.refocus(window, cx);
            }))
    }))
}

/// Whether a click on the Project body, pressed at `down_y` and released at
/// `up_y`, stayed in the empty area below the entry row: both ends at or
/// below `entry_bottom`, the bottom of the entry row's cell probe (its padding
/// box, so the cell's 1px bottom border still counts as the row). `None`:
/// the entry row is not drawn, which in the scrolled list means it is below
/// the viewport, so no empty area shows.
fn below_entry_row(down_y: Pixels, up_y: Pixels, entry_bottom: Option<Pixels>) -> bool {
    entry_bottom.is_some_and(|bottom| {
        let edge = bottom + px(1.);
        down_y >= edge && up_y >= edge
    })
}

fn inactive_row(proj: &projcore::Project, index: usize) -> bool {
    proj.tasks
        .get(index)
        .is_some_and(|t| !t.is_null && !proj.effectively_active(index))
}

fn pane(width: f32, offset: f32, content: impl IntoElement) -> impl IntoElement {
    div()
        .relative()
        .w(px(width))
        .h(px(ROW_H))
        .flex_none()
        .overflow_hidden()
        .child(div().absolute().left(px(-offset)).top_0().child(content))
}

fn chart_pane(
    width: f32,
    offset: f32,
    pal: Pal,
    hot: bool,
    content: impl IntoElement,
) -> impl IntoElement {
    h_flex()
        .when(hot, |d| d.bg(pal.sel))
        .w(px(width + GANTT_INSET))
        .h(px(ROW_H))
        .flex_none()
        .child(
            div()
                .w(px(GANTT_INSET))
                .h_full()
                .flex_none()
                .border_l(px(1.))
                .border_color(pal.border),
        )
        .child(pane(width, offset, content))
}

/// The task index list row `r` draws, given the visible rows: the `r`th
/// visible task, or past them the entry row (index `task_count`).
pub(crate) fn list_task(rows: &[usize], task_count: usize, r: usize) -> usize {
    rows.get(r).copied().unwrap_or(task_count)
}

pub(super) fn project_el(
    view: &ProjectView,
    index: usize,
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    // Collapsed summaries hide their subtasks' rows.
    let shown = view.ed.display_rows().len();
    let (table_w, gantt_w, table_x, gantt_x, scale) = (
        view.table_w,
        view.gantt_w,
        view.table_x.get(),
        view.gantt_x.get(),
        view.chart_scale(),
    );
    let (table_bar, chart_bar) = view.pane_scrolls();
    // The rows' scroll as drawn, which a Ctrl+wheel zoom puts back.
    let held_scroll = view.scroll.0.borrow().base_handle.offset();
    let vbar = harness::region_name(harness::Region::ProjectVbar);
    let row_probes = probes.clone();
    // The bar palette the plan's formatting names (#1532), once per render.
    let palette = std::rc::Rc::new(crate::plugin_ui::palette(view));
    v_flex()
        .flex_1()
        .h_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .text_color(pal.fg)
        .text_size(px(12.))
        .when(view.timeline, |d| {
            d.child(timeline_el(view, index, pal, probes, cx))
        })
        // The split bar spans header, body and scrollbar strip, under the Timeline.
        .child(
            v_flex()
                .id(("project-split-area", index))
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .on_drag_move::<SplitDrag>(cx.listener(
                    move |this, e: &DragMoveEvent<SplitDrag>, window, cx| {
                        cx.set_active_drag_cursor_style(CursorStyle::ResizeLeftRight, window);
                        if let Some(Surface::Project(v)) =
                            this.tabs.get_mut(index).map(|t| &mut t.surface)
                        {
                            v.set_split(
                                f32::from(e.event.position.x - e.bounds.left()) - GANTT_INSET / 2.,
                            );
                            cx.notify();
                        }
                    },
                ))
                // Horizontal wheel (tilt, trackpad, Shift+wheel on every platform)
                // scrolls the pane under the pointer; the rows keep the vertical wheel.
                .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, window, cx| {
                    let delta = ev.delta.pixel_delta(window.line_height());
                    // Ctrl+wheel over the chart zooms the timescale around the
                    // pointer (#1535). The rows' own handler has already moved
                    // them by the same wheel turn; put them back.
                    // Only a whole line is a notch; fractional lines (hi-res
                    // wheels, touchpads) are summed like pixels.
                    let notch = zoom::is_notch(&ev.delta);
                    if ev.modifiers.control
                        && (!delta.y.is_zero() || ev.touch_phase != TouchPhase::Moved)
                    {
                        let Some(body) = this.probes.borrow().current("project-body") else {
                            return;
                        };
                        cx.stop_propagation();
                        if let Some(tab) = this.tabs.get_mut(index)
                            && let Surface::Project(v) = &mut tab.surface
                        {
                            let zoom = v.wheel_zoom(
                                f32::from(ev.position.x - body.left()),
                                f32::from(delta.y),
                                notch,
                                ev.touch_phase,
                            );
                            if zoom != WheelZoom::NotChart {
                                v.scroll.0.borrow().base_handle.set_offset(held_scroll);
                                if let WheelZoom::Stepped(status) = zoom {
                                    tab.status = status.into();
                                }
                                cx.notify();
                            }
                        }
                        return;
                    }
                    if delta.x.is_zero() {
                        return;
                    }
                    // `project-body` spans the split area, so its left is the area's.
                    let Some(body) = this.probes.borrow().current("project-body") else {
                        return;
                    };
                    cx.stop_propagation();
                    if let Some(Surface::Project(v)) =
                        this.tabs.get_mut(index).map(|t| &mut t.surface)
                        && v.wheel(f32::from(ev.position.x - body.left()), f32::from(delta.x))
                    {
                        cx.notify();
                    }
                }))
                .child(
                    h_flex()
                        .h(px(ROW_H))
                        .flex_none()
                        .bg(pal.panel)
                        .font_weight(FontWeight::BOLD)
                        .child(pane(table_w, table_x, heading_cells(view, pal, index, probes, cx)))
                        .child(chart_pane(
                            gantt_w,
                            gantt_x,
                            pal,
                            false,
                            gantt_header(
                                scale,
                                view.ed.project().project_shading_calendar(),
                                gantt_x,
                                gantt_w,
                                pal,
                            ),
                        ))
                        // Above the vertical scrollbar.
                        .child(div().w(px(SCROLLBAR_W)).h_full().flex_none()),
                )
                .child(
                    div()
                        .id(("project-body", index))
                        .relative()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(body_grid(view, pal))
                        .child(probe(probes, "project-body"))
                        // Meant for the ruled empty rows below the entry row. Rows stop
                        // propagation of their own clicks, but a press on one row and a
                        // release on another is no row's click and still the body's
                        // (#1121), so the gesture itself has to have stayed below.
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                            let ClickEvent::Mouse(click) = ev else {
                                return;
                            };
                            // The frame on screen only: an entry row that just scrolled away
                            // must not answer from the frame before it.
                            let entry = this.probes.borrow().on_screen("project-cell:entry:0");
                            if !below_entry_row(
                                click.down.position.y,
                                click.up.position.y,
                                entry.map(|b| b.bottom()),
                            ) {
                                return;
                            }
                            if let Some(tab) = this.tabs.get_mut(index) {
                                project_below_click(tab);
                            }
                            this.refocus(window, cx);
                        }))
                        .child(
                            uniform_list(
                                ("project-rows", index),
                                // The visible tasks, then the entry row.
                                shown + 1,
                                cx.processor(
                                    move |this, range: std::ops::Range<usize>, window, cx| {
                                        let Some(Surface::Project(v)) =
                                            this.tabs.get(index).map(|t| &t.surface)
                                        else {
                                            return vec![];
                                        };
                                        let cursor_row = v.cursor_row();
                                        let tasks = &v.ed.project().tasks;
                                        let rows = v.ed.display_rows();
                                        // List rows are shown rows; `i` is the task index,
                                        // and past the last shown task the one entry row
                                        // (`None`, index `tasks.len()`).
                                        range
                                            .filter(|&r| r <= rows.len())
                                            .map(|r| {
                                                let i = list_task(&rows, tasks.len(), r);
                                                let task = tasks.get(i);
                                                let indent = task
                                                    .map_or(0, |t| t.outline_level)
                                                    .saturating_sub(1)
                                                    .min(20)
                                                    as f32
                                                    * 12.;
                                                h_flex()
                                                    .id(("project-row", i))
                                                    .h(px(ROW_H))
                                                    .w_full()
                                                    .cursor_pointer()
                                                    .when(task.is_some_and(|t| t.summary), |d| {
                                                        d.font_weight(FontWeight::BOLD)
                                                    })
                                                    .when(i == cursor_row && task.is_none(), |d| d.bg(pal.sel))
                                                    // The row's menu opens on its table,
                                                    // past the cells too; the chart's bars
                                                    // open their own (#1207).
                                                    .child(
                                                        div()
                                                            .flex_none()
                                                            .on_mouse_down(
                                                                MouseButton::Right,
                                                                cx.listener(
                                                                    move |this,
                                                                          ev: &MouseDownEvent,
                                                                          window,
                                                                          cx| {
                                                                        cx.stop_propagation();
                                                                        if this.active == index {
                                                                            let _ = this.open_row_menu(
                                                                                i,
                                                                                None,
                                                                                ev.position,
                                                                                window,
                                                                                cx,
                                                                            );
                                                                        }
                                                                    },
                                                                ),
                                                            )
                                                            .child(pane(
                                                                table_w,
                                                                table_x,
                                                                editable_row_cells(
                                                                    v,
                                                                    pal,
                                                                    task,
                                                                    i,
                                                                    index,
                                                                    indent,
                                                                    &row_probes,
                                                                    window,
                                                                    cx,
                                                                ),
                                                            )),
                                                    )
                                                    .child(chart_pane(
                                                        gantt_w,
                                                        gantt_x,
                                                        pal,
                                                        // The table's selection is cell-wise
                                                        // (#1534); the chart keeps the row.
                                                        task.is_some() && i == cursor_row,
                                                        // The entry row has no bar, so no `bar:` probe.
                                                        gantt_strip(
                                                            task.and_then(|t| {
                                                                gantt_bar(&v.ed, t, scale)
                                                            })
                                                            .map(|b| v.styled_bar(b)),
                                                            task.map_or(0, |t| t.id),
                                                            scale,
                                                            pal,
                                                            palette.as_ref().as_ref(),
                                                            &row_probes,
                                                            // A right-press on the bar
                                                            // selects its row and opens
                                                            // the bar's menu.
                                                            cx.listener(
                                                                move |this,
                                                                      ev: &MouseDownEvent,
                                                                      window,
                                                                      cx| {
                                                                    cx.stop_propagation();
                                                                    if this.active == index {
                                                                        let _ = this.open_bar_menu(
                                                                            i,
                                                                            ev.position,
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    }
                                                                },
                                                            ),
                                                            // A right-press beside the bar:
                                                            // the chart's menu (#1532).
                                                            cx.listener(
                                                                move |this,
                                                                      ev: &MouseDownEvent,
                                                                      _window,
                                                                      cx| {
                                                                    cx.stop_propagation();
                                                                    if this.active == index {
                                                                        this.open_chart_menu(
                                                                            ev.position,
                                                                            cx,
                                                                        );
                                                                    }
                                                                },
                                                            ),
                                                        ),
                                                    ))
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            cx.stop_propagation();
                                                            if let Some(tab) =
                                                                this.tabs.get_mut(index)
                                                            {
                                                                project_row_click(
                                                                    tab, i, None, false,
                                                                );
                                                            }
                                                            this.refocus(window, cx);
                                                        },
                                                    ))
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(&view.scroll)
                            .flex_1()
                            .h_full()
                            .min_h_0()
                            // By default gpui turns an x-only delta into a vertical
                            // scroll; the split area's wheel handler owns x.
                            .map(|mut list| {
                                list.style().restrict_scroll_to_axis = Some(true);
                                list
                            }),
                        )
                        // The UI plugins' overlay, after the rows (#1532).
                        .children(gantt_overlay(view, pal))
                        // Project's one vertical bar, shared by table and chart, over the
                        // SCROLLBAR_W that `layout` keeps out of the chart.
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(SCROLLBAR_W))
                                .child(probe(probes, vbar.clone()))
                                .child(
                                    Scrollbar::vertical(&view.scroll)
                                        .id(SharedString::from(vbar))
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        ),
                )
                // Independent horizontal bars under the table and the chart, as in Project.
                .child(
                    h_flex()
                        .h(px(SCROLLBAR_W))
                        .flex_none()
                        .bg(pal.panel)
                        .child(hbar_strip(
                            table_bar,
                            harness::Region::ProjectHbarTable,
                            probes,
                        ))
                        .child(div().w(px(GANTT_INSET)).h_full().flex_none())
                        .child(hbar_strip(
                            chart_bar,
                            harness::Region::ProjectHbarChart,
                            probes,
                        ))
                        .child(div().w(px(SCROLLBAR_W)).h_full().flex_none()),
                )
                .child(split_bar(table_w, index, probes, cx)),
        )
}

/// The drag payload for the split bar; it draws nothing, since the panes follow the pointer.
struct SplitDrag;

impl Render for SplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Over the `GANTT_INSET` gutter. It blocks presses from the row and body under
/// it, but lets the wheel through so the rows still scroll there.
fn split_bar(
    table_w: f32,
    index: usize,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    div()
        .id(("project-split", index))
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(table_w))
        .w(px(GANTT_INSET))
        .block_mouse_except_scroll()
        .cursor(CursorStyle::ResizeLeftRight)
        .child(probe(
            probes,
            harness::region_name(harness::Region::ProjectSplit),
        ))
        .on_drag(SplitDrag, |_, _, _, cx| cx.new(|_| SplitDrag))
        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
            if ev.click_count() >= 2
                && let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface)
            {
                v.reset_split();
            }
            this.refocus(window, cx);
        }))
}

fn hbar_strip(
    bar: PaneScroll,
    region: harness::Region,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
) -> impl IntoElement {
    // One name for the probe, the Scrollbar's state id and the harness region.
    let id = harness::region_name(region);
    div()
        .relative()
        .w(px(bar.viewport))
        .h_full()
        .flex_none()
        .child(probe(probes, id.clone()))
        .child(
            Scrollbar::horizontal(&bar)
                .id(SharedString::from(id))
                .scrollbar_show(ScrollbarShow::Always),
        )
}

/// A [`LineEdit`] drawn as text with the caret bar and the tinted selection,
/// shifted left by `offset` (its [`LineEdit::scroll_x`]); shared by the cell
/// editor and the prompt bar.
pub(crate) fn line_edit_view(edit: &LineEdit, offset: f32, caret_h: f32) -> Div {
    let sel = edit.selection().unwrap_or(edit.caret..edit.caret);
    let caret_bar = || {
        div()
            .flex_none()
            .w(px(1.5))
            .h(px(caret_h))
            .bg(hsla_u(BRAND))
    };
    h_flex()
        .relative()
        .left(px(-offset))
        .flex_none()
        .items_center()
        .child(edit.buf[..sel.start].to_owned())
        .when(edit.caret == sel.start, |d| d.child(caret_bar()))
        .when(sel.start < sel.end, |d| {
            let c = hsla_u(BRAND);
            d.child(
                div()
                    .flex_none()
                    .bg(hsla(c.h, c.s, c.l, 0.18))
                    .child(edit.buf[sel.clone()].to_owned()),
            )
        })
        .when(edit.caret > sel.start, |d| d.child(caret_bar()))
        .child(edit.buf[sel.end..].to_owned())
}

#[cfg(test)]
mod tests;
