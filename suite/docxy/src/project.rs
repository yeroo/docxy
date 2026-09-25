//! Project tab: file/session policy and the Gantt view over the shared editor.
use super::*;
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use projcore::editor::{Editor as ProjectEditor, untitled_project};
use projcore::{LinkType, Project, Task, mspdi, yppx};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
mod gantt;
pub(super) use gantt::*;
mod commands;
pub(super) use commands::*;
mod cell;
pub(super) use cell::*;
mod timeline;
pub(super) use timeline::*;

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
    pub cell: Option<CellEdit>,
    pub exported: Option<String>,
    /// The whole pane's width, which the Timeline spans.
    pub width: f32,
    /// Whether the Timeline pane is shown: window view state, never saved.
    pub timeline: bool,
    /// The table pane width the user dragged the split bar to; `None` is the
    /// default. Window view state, never saved.
    pub split: Option<f32>,
}

impl ProjectView {
    fn new(project: Project, dirty: bool) -> Self {
        let ed = ProjectEditor::restored(project, dirty);
        let scale = gantt_scale(&ed);
        Self {
            ed,
            scroll: UniformListScrollHandle::new(),
            table_x: PaneOffset::default(),
            gantt_x: PaneOffset::default(),
            table_w: 590.,
            gantt_w: 590. - GANTT_INSET - SCROLLBAR_W,
            scale,
            prompt: None,
            col: 1,
            cell: None,
            exported: None,
            width: 590. + SCROLLBAR_W,
            timeline: true,
            split: None,
        }
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
        self.table_w = split_table_width(self.width, self.split);
        self.gantt_w = (self.width - self.table_w - GANTT_INSET - SCROLLBAR_W).max(0.);
        self.refresh_schedule_layout();
    }

    /// Moves the split bar. It writes `table_w` itself, so the next frame's
    /// `layout` sees no resize and leaves the table where it was scrolled.
    pub fn set_split(&mut self, table_w: f32) {
        self.split = Some(split_table_width(self.width, Some(table_w)));
        self.apply_widths();
    }

    pub fn reset_split(&mut self) {
        self.split = None;
        self.apply_widths();
    }

    pub fn refresh_schedule_layout(&mut self) {
        self.scale = gantt_scale(&self.ed);
        self.clamp_offsets();
    }

    /// The drawn scale: the plan's, widened to the viewport so a short plan's chart still has
    /// days to its right edge. Scrolling clamps to the plan's own scale, not this one.
    pub fn chart_scale(&self) -> GanttScale {
        GanttScale {
            days: self.scale.days.max((self.gantt_w / DAY_W).ceil() as i64),
            ..self.scale
        }
    }

    fn clamp_offsets(&mut self) {
        self.table_x.clamp(TABLE_W, self.table_w);
        self.gantt_x.clamp(self.scale.width(), self.gantt_w);
    }

    /// Navigation and horizontal scrolling only; command completion owns row reveal.
    pub fn key(&mut self, key: &str, shift: bool) -> bool {
        if matches!(key, "left" | "right" | "tab") {
            let left = key == "left" || (key == "tab" && shift);
            self.col = if left {
                self.col.saturating_sub(1)
            } else {
                (self.col + 1).min(6)
            };
            self.reveal_col();
            return true;
        }
        let index = match key {
            "up" => self.ed.sel().saturating_sub(1),
            "down" => self.ed.sel().saturating_add(1),
            "home" => 0,
            "end" => self.ed.project().tasks.len().saturating_sub(1),
            _ => return false,
        };
        self.ed.select(index);
        true
    }

    /// The table and chart scrollbars' handles. Each strip is drawn at the width
    /// its handle clamps against: gpui-component takes the track length from the
    /// strip, so the two must be the same number.
    pub fn pane_scrolls(&self) -> (PaneScroll, PaneScroll) {
        (
            PaneScroll {
                offset: self.table_x.clone(),
                content: TABLE_W,
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
        self.gantt_x
            .set(self.gantt_x.get() + if right { DAY_W } else { -DAY_W });
        self.clamp_offsets();
    }
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
    let Some(uid) = v.ed.selected_uid() else {
        return;
    };
    let level = v.ed.project().task(uid).unwrap().outline_level;
    if (i64::from(level) + i64::from(delta)).clamp(1, 20) == i64::from(level) {
        return;
    }
    if let Err(e) = v.ed.indent(uid, delta) {
        tab.status = e.into();
    }
    tab.dirty = v.ed.dirty();
}

impl Docxy {
    pub(super) fn project_region_bounds(
        &self,
        region: harness::Region,
    ) -> Result<Bounds<Pixels>, String> {
        let Some(Surface::Project(v)) = self.tabs.get(self.active).map(|t| &t.surface) else {
            return Err("the active tab is not a loaded Project".into());
        };
        let probes = self.probes.borrow();
        project_region(v, &probes, region)
    }
}

fn project_region(
    v: &ProjectView,
    probes: &Probes,
    region: harness::Region,
) -> Result<Bounds<Pixels>, String> {
    // The strips are probed under their region names; they need no Gantt viewport.
    if let harness::Region::ProjectTimeline = region {
        if !v.timeline {
            return Err("the Timeline is hidden (View > Split View > Timeline)".into());
        }
        return probes
            .get(&harness::region_name(region))
            .ok_or_else(|| "the Timeline has not been laid out".into());
    }
    if let harness::Region::ProjectHbarTable
    | harness::Region::ProjectHbarChart
    | harness::Region::ProjectVbar = region
    {
        let name = harness::region_name(region);
        return probes
            .get(&name)
            .ok_or_else(|| format!("the {name} scrollbar has not been laid out"));
    }
    if let harness::Region::ProjectSplit = region {
        return probes
            .get(&harness::region_name(region))
            .ok_or_else(|| "the split bar has not been laid out".into());
    }
    let body = probes
        .get("project-body")
        .ok_or("the Project body has not been laid out")?;
    let viewport =
        gantt_viewport(body, v.table_w, v.gantt_w).ok_or("the Gantt viewport is empty")?;
    match region {
        harness::Region::Cells(r0, c0, r1, c1) => {
            if r0 != r1 || c0 != c1 || c0 >= 7 {
                return Err("Project regions address one entry-table cell".into());
            }
            let task =
                v.ed.project()
                    .tasks
                    .get(r0 as usize)
                    .ok_or("No task at this row")?;
            let cell = probes
                .get(&format!("project-cell:{}:{c0}", task.id))
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
            let bar = probes
                .get(&format!("bar:{id}"))
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
    let scale = gantt_scale(&v.ed);
    let count = v.ed.project().tasks.len();
    let scroll_y = -f32::from(v.scroll.0.borrow().base_handle.offset().y);
    let mut entries = vec![
        ("selected_task".into(), Json::Num(v.ed.sel() as f64)),
        ("tasks".into(), Json::Num(count as f64)),
        (
            "filler_rows".into(),
            Json::Num(body_h.map_or(0, |h| filler_rows(h, scroll_y, count)) as f64),
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
                v.ed.project()
                    .tasks
                    .get(v.ed.sel())
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
            ),
        ),
        (
            "exported".into(),
            Json::Str(v.exported.clone().unwrap_or_else(|| "none".into())),
        ),
        ("table_w".into(), Json::Num(f64::from(v.table_w))),
        ("gantt_w".into(), Json::Num(f64::from(v.gantt_w))),
    ];
    entries.extend(v.ed.project().tasks.iter().map(|t| {
        (
            format!("bar_{}", t.id),
            Json::Str(
                gantt_bar(&v.ed, t, scale)
                    .map(GanttBar::state)
                    .unwrap_or_else(|| "none".into()),
            ),
        )
    }));
    entries.extend(project_cell_state(v));
    entries.extend(timeline_state(v));
    entries.push(("undo_depth".into(), Json::Num(v.ed.undo_depth() as f64)));
    entries.push(("redo_depth".into(), Json::Num(v.ed.redo_depth() as f64)));
    entries
}

fn ext_is(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

pub(super) fn is_project_path(path: &Path) -> bool {
    ["yppx", "xml", "mpp"].iter().any(|e| ext_is(path, e))
}

pub(super) fn is_imported(tab: &DocTab) -> bool {
    tab.kind == Kind::Project && tab.path.as_deref().is_some_and(|p| ext_is(p, "mpp"))
}

fn imported_status(status: String, path: Option<&Path>) -> SharedString {
    if path.is_some_and(|p| ext_is(p, "mpp")) {
        format!("{status}, imported from .mpp; Save As .yppx or MSPDI to keep edits").into()
    } else {
        status.into()
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
        pkg: None,
        notes: vec![],
        markdown: false,
        hf_edit: None,
        bundle_html: None,
        load_failed: false,
    }
}

pub(super) fn project_from_path(path: &Path) -> Result<Project, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read error: {e}"))?;
    if ext_is(path, "yppx") {
        yppx::read_yppx(&bytes)
    } else if ext_is(path, "mpp") {
        mppread::project::project_from_mpp(&bytes)
    } else if ext_is(path, "xml") {
        mspdi::read_mspdi(std::str::from_utf8(&bytes).map_err(|_| "not UTF-8")?)
    } else {
        Err("expected .yppx, .xml or .mpp".into())
    }
}

pub(super) fn project_tab_from_path(path: &Path) -> DocTab {
    let (surface, status) = match project_from_path(path) {
        Ok(p) => {
            let status = imported_status(format!("loaded — {} tasks", p.tasks.len()), Some(path));
            (Surface::Project(ProjectView::new(p, false)), status)
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

pub(super) fn new_project_tab() -> DocTab {
    project_tab(
        "Untitled.yppx".into(),
        None,
        Surface::Project(ProjectView::new(untitled_project(), false)),
        false,
        "new project".into(),
    )
}

fn save_target(path: &Path) -> Result<PathBuf, String> {
    yppx::save_target(path)
}

fn write_project(ed: &ProjectEditor, path: &Path) -> Result<(PathBuf, usize), String> {
    let path = save_target(path)?;
    let bytes = if ext_is(&path, "yppx") {
        yppx::write_yppx(ed.project())
    } else {
        mspdi::write_mspdi(ed.project()).into_bytes()
    };
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
            "This project needs Save As, and a harness instance cannot open the Save As dialog"
                .into(),
        );
    }
    let name = tab
        .path
        .as_deref()
        .unwrap_or_else(|| Path::new(tab.title.as_ref()));
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    SaveDecision::Dialog {
        suggested: format!("{stem}.yppx"),
    }
}

pub(super) fn apply_save(tab: &mut DocTab, target: &Path) -> Result<usize, String> {
    if !commit_project_cell(tab) {
        return Err(tab.status.to_string());
    }
    let result = match &tab.surface {
        Surface::Project(v) => write_project(&v.ed, target),
        _ => Err("This project could not be loaded and cannot be saved".into()),
    };
    match result {
        Ok((path, n)) => {
            if let Surface::Project(v) = &mut tab.surface {
                v.ed.mark_saved();
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
            Ok(p) => {
                let status = imported_status(
                    if t.dirty {
                        "unsaved — restored"
                    } else {
                        "loaded"
                    }
                    .into(),
                    path.as_deref(),
                );
                return project_tab(
                    t.title.clone().into(),
                    path,
                    Surface::Project(ProjectView::new(p, t.dirty)),
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
            None => new_project_tab(),
        };
    }
    let (surface, status) = match path.as_deref() {
        Some(orig) => match project_from_path(orig) {
            Ok(p) => (
                Surface::Project(ProjectView::new(p, false)),
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

fn days(project: &Project, min: i64) -> String {
    let d = project.minutes_to_days(min);
    if (d.round() - d).abs() < 1e-9 {
        format!("{d:.0}d")
    } else {
        format!("{d:.1}d")
    }
}

fn date(dt: Option<projcore::DateTime>) -> String {
    dt.map(|d| {
        let p = d.parts();
        format!("{:04}-{:02}-{:02}", p.year, p.month, p.day)
    })
    .unwrap_or_else(|| "—".into())
}

pub(crate) fn project_row(ed: &ProjectEditor, task: &Task) -> [String; 7] {
    // A blank row (#80) is not a task: Project shows only its ID.
    if task.is_null {
        return [
            task.id.to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ];
    }
    let project = ed.project();
    let predecessors = projcore::editor::format_predecessors(task, project);
    let resources = projcore::editor::format_resource_names(project, task.uid);
    [
        task.id.to_string(),
        task.name.clone(),
        // Summaries first: their stored duration is stale (and may be 0, which
        // `is_milestone` would misread), so derive it from the shown dates.
        if task.summary {
            ed.disp_duration_min(task.uid)
                .map_or_else(|| "?".into(), |min| days(project, min))
        } else if task.is_milestone() {
            "—".into()
        } else {
            days(project, task.duration_min)
        },
        date(ed.disp_start(task.uid)),
        date(ed.disp_finish(task.uid)),
        predecessors,
        resources,
    ]
}

const ROW_H: f32 = 28.;
const WIDTHS: [f32; 7] = [48., 240., 80., 100., 100., 150., 190.];
const TABLE_W: f32 = sum_widths();
/// Includes the divider, leaving space between clipped table text and the chart.
const GANTT_INSET: f32 = 6.;
/// gpui-component's private scrollbar `WIDTH`: it paints the bar in the last 16px
/// of its strip's cross axis, so a thinner strip clips the bar.
const SCROLLBAR_W: f32 = 16.;

const fn sum_widths() -> f32 {
    let mut total = 0.;
    let mut i = 0;
    while i < WIDTHS.len() {
        total += WIDTHS[i];
        i += 1;
    }
    total
}

fn row_cells(values: [String; 7], indent: f32) -> impl IntoElement {
    h_flex()
        .h(px(ROW_H))
        .items_center()
        .children(values.into_iter().enumerate().map(|(i, value)| {
            div()
                .w(px(WIDTHS[i]))
                .flex_none()
                .px_2()
                .overflow_hidden()
                .whitespace_nowrap()
                .when(i == 1, |d| d.pl(px(8. + indent)))
                .child(value)
        }))
}

fn editable_row_cells(
    v: &ProjectView,
    task: &Task,
    row: usize,
    index: usize,
    indent: f32,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    window: &Window,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    h_flex().h(px(ROW_H)).items_center().children(
        project_row(&v.ed, task)
            .into_iter()
            .enumerate()
            .map(|(col, value)| {
                let edit = v
                    .cell
                    .as_ref()
                    .filter(|c| c.uid == task.uid && c.col == col);
                let content = if let Some(edit) = edit {
                    let measure = Measurer::new(window);
                    let offset = edit.scroll_x(WIDTHS[col] - 24., |s| {
                        measure.width(s, 12., task.summary, false)
                    });
                    h_flex()
                        .relative()
                        .left(px(-offset))
                        .flex_none()
                        .items_center()
                        .child(edit.buf[..edit.caret].to_owned())
                        .child(div().flex_none().w(px(1.5)).h(px(16.)).bg(hsla_u(BRAND)))
                        .child(edit.buf[edit.caret..].to_owned())
                        .into_any_element()
                } else {
                    div().child(value).into_any_element()
                };
                div()
                    .id(("project-cell", row * 7 + col))
                    .relative()
                    .flex()
                    .items_center()
                    .w(px(WIDTHS[col]))
                    .h(px(ROW_H))
                    .flex_none()
                    .px_2()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .border_1()
                    .border_color(if row == v.ed.sel() && col == v.col {
                        hsla_u(BRAND)
                    } else {
                        hsla(0., 0., 0., 0.)
                    })
                    .when(col == 1 && edit.is_none(), |d| d.pl(px(8. + indent)))
                    .child(probe(probes, format!("project-cell:{}:{col}", task.id)))
                    .child(content)
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(tab) = this.tabs.get_mut(index) {
                            project_cell_click(tab, row, Some(col), ev.click_count() >= 2);
                        }
                        this.refocus(window, cx);
                    }))
            }),
    )
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

fn chart_pane(width: f32, offset: f32, pal: Pal, content: impl IntoElement) -> impl IntoElement {
    h_flex()
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

pub(super) fn project_el(
    view: &ProjectView,
    index: usize,
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    let count = view.ed.project().tasks.len();
    let (table_w, gantt_w, table_x, gantt_x, scale) = (
        view.table_w,
        view.gantt_w,
        view.table_x.get(),
        view.gantt_x.get(),
        view.chart_scale(),
    );
    let (table_bar, chart_bar) = view.pane_scrolls();
    let vbar = harness::region_name(harness::Region::ProjectVbar);
    let row_probes = probes.clone();
    v_flex()
        .flex_1()
        .h_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .text_color(pal.fg)
        .text_size(px(12.))
        .when(view.timeline, |d| d.child(timeline_el(view, pal, probes)))
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
                .child(
                    h_flex()
                        .h(px(ROW_H))
                        .flex_none()
                        .bg(pal.panel)
                        .font_weight(FontWeight::BOLD)
                        .child(pane(
                            table_w,
                            table_x,
                            row_cells(
                                [
                                    "ID",
                                    "Name",
                                    "Duration",
                                    "Start",
                                    "Finish",
                                    "Predecessors",
                                    "Resource Names",
                                ]
                                .map(str::to_string),
                                0.,
                            ),
                        ))
                        .child(chart_pane(
                            gantt_w,
                            gantt_x,
                            pal,
                            gantt_header(scale, gantt_x, gantt_w, pal),
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
                        // Rows stop propagation, so only the ruled empty rows land here.
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(tab) = this.tabs.get_mut(index) {
                                project_blank_click(tab);
                            }
                            this.refocus(window, cx);
                        }))
                        .child(
                            uniform_list(
                                ("project-rows", index),
                                count,
                                cx.processor(
                                    move |this, range: std::ops::Range<usize>, window, cx| {
                                        let Some(Surface::Project(v)) =
                                            this.tabs.get(index).map(|t| &t.surface)
                                        else {
                                            return vec![];
                                        };
                                        range
                                            .filter_map(|i| {
                                                v.ed.project().tasks.get(i).map(|task| {
                                                    let indent = task
                                                        .outline_level
                                                        .saturating_sub(1)
                                                        .min(20)
                                                        as f32
                                                        * 12.;
                                                    h_flex()
                                                        .id(("project-row", i))
                                                        .h(px(ROW_H))
                                                        .w_full()
                                                        .cursor_pointer()
                                                        .when(task.summary, |d| {
                                                            d.font_weight(FontWeight::BOLD)
                                                        })
                                                        .when(i == v.ed.sel(), |d| d.bg(pal.sel))
                                                        .child(pane(
                                                            table_w,
                                                            table_x,
                                                            editable_row_cells(
                                                                v,
                                                                task,
                                                                i,
                                                                index,
                                                                indent,
                                                                &row_probes,
                                                                window,
                                                                cx,
                                                            ),
                                                        ))
                                                        .child(chart_pane(
                                                            gantt_w,
                                                            gantt_x,
                                                            pal,
                                                            gantt_strip(
                                                                gantt_bar(&v.ed, task, scale),
                                                                task.id,
                                                                scale,
                                                                pal,
                                                                &row_probes,
                                                            ),
                                                        ))
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                cx.stop_propagation();
                                                                if let Some(tab) =
                                                                    this.tabs.get_mut(index)
                                                                {
                                                                    project_cell_click(
                                                                        tab, i, None, false,
                                                                    );
                                                                }
                                                                this.refocus(window, cx);
                                                            },
                                                        ))
                                                })
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(&view.scroll)
                            .flex_1()
                            .h_full()
                            .min_h_0(),
                        )
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
                        )
                        .when(count == 0, |d| {
                            // Over the first ruled row, so the grid runs under it.
                            d.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .h(px(ROW_H))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .text_color(pal.dim)
                                    .child("No tasks"),
                            )
                        }),
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

#[cfg(test)]
mod tests;
