//! Project tab: file/session policy and a read-only view over the shared editor.
use super::*;
use projcore::editor::{Editor as ProjectEditor, untitled_project};
use projcore::{LinkType, Project, Task, mspdi, yppx};
use std::path::Path;

pub(super) struct ProjectView {
    pub ed: ProjectEditor,
    pub scroll: UniformListScrollHandle,
}

impl ProjectView {
    fn new(project: Project, dirty: bool) -> Self {
        Self {
            ed: ProjectEditor::restored(project, dirty),
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Shared by the key handler and tests; empty history must preserve restored dirtiness.
    pub fn key(&mut self, key: &str, ctrl: bool) -> bool {
        if ctrl {
            match key {
                "z" => {
                    self.ed.undo();
                }
                "y" => {
                    self.ed.redo();
                }
                _ => return false,
            }
        } else {
            let index = match key {
                "up" => self.ed.sel().saturating_sub(1),
                "down" => self.ed.sel().saturating_add(1),
                "home" => 0,
                "end" => self.ed.project().tasks.len().saturating_sub(1),
                _ => return false,
            };
            self.ed.select(index);
        }
        if !self.ed.project().tasks.is_empty() {
            self.scroll
                .scroll_to_item(self.ed.sel(), ScrollStrategy::Nearest);
        }
        true
    }
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
    if path.extension().is_none() {
        Ok(path.with_extension("yppx"))
    } else if ext_is(path, "yppx") || ext_is(path, "xml") {
        Ok(path.into())
    } else {
        Err("Project schedules can only be saved as .yppx or .xml (MSPDI)".into())
    }
}

fn write_project(ed: &ProjectEditor, path: &Path) -> Result<(PathBuf, usize), String> {
    let path = save_target(path)?;
    let bytes = if ext_is(&path, "yppx") {
        yppx::write_yppx(ed.project())
    } else {
        mspdi::write_mspdi(ed.project()).into_bytes()
    };
    std::fs::write(&path, &bytes).map_err(|e| format!("save failed: {e}"))?;
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

fn apply_save(tab: &mut DocTab, target: &Path) -> Result<usize, String> {
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
    } else if t.dirty {
        "no sidecar recorded"
    } else {
        return match path {
            Some(path) => project_tab_from_path(&path),
            None => new_project_tab(),
        };
    };
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

fn project_row(ed: &ProjectEditor, task: &Task) -> [String; 7] {
    let project = ed.project();
    let predecessors = task
        .predecessors
        .iter()
        .map(|p| {
            let id = project
                .task(p.uid)
                .map(|t| t.id.to_string())
                .unwrap_or_else(|| format!("?{}", p.uid));
            let kind = match p.link {
                LinkType::FinishStart if p.lag_min == 0 => "",
                LinkType::FinishStart => "FS",
                LinkType::StartStart => "SS",
                LinkType::FinishFinish => "FF",
                LinkType::StartFinish => "SF",
            };
            let lag = if p.lag_min == 0 {
                String::new()
            } else {
                format!(
                    "{}{}",
                    if p.lag_min > 0 { "+" } else { "" },
                    days(project, p.lag_min)
                )
            };
            format!("{id}{kind}{lag}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let resources = project
        .assignments
        .iter()
        .filter(|a| a.task_uid == task.uid)
        .filter_map(|a| project.resources.iter().find(|r| r.uid == a.resource_uid))
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    [
        task.id.to_string(),
        task.name.clone(),
        if task.is_milestone() {
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

pub(super) fn project_el(
    view: &ProjectView,
    index: usize,
    pal: Pal,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    let count = view.ed.project().tasks.len();
    let table = v_flex()
        .w(px(WIDTHS.iter().sum()))
        .flex_shrink_0()
        .h_full()
        .min_h_0()
        .text_color(pal.fg)
        .text_size(px(12.))
        .child(
            div()
                .bg(pal.panel)
                .font_weight(FontWeight::BOLD)
                .child(row_cells(
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
                )),
        )
        .when(count == 0, |d| {
            d.child(div().p_4().text_color(pal.dim).child("No tasks"))
        })
        .child(
            uniform_list(
                ("project-rows", index),
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    let Some(Surface::Project(v)) = this.tabs.get(index).map(|t| &t.surface) else {
                        return vec![];
                    };
                    range
                        .filter_map(|i| {
                            v.ed.project().tasks.get(i).map(|task| {
                                let indent =
                                    task.outline_level.saturating_sub(1).min(20) as f32 * 12.;
                                div()
                                    .id(("project-row", i))
                                    .h(px(ROW_H))
                                    .cursor_pointer()
                                    .when(task.summary, |d| d.font_weight(FontWeight::BOLD))
                                    .when(i == v.ed.sel(), |d| d.bg(pal.sel))
                                    .child(row_cells(project_row(&v.ed, task), indent))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(Surface::Project(v)) =
                                            this.tabs.get_mut(index).map(|t| &mut t.surface)
                                        {
                                            v.ed.select(i);
                                        }
                                        this.refocus(window, cx);
                                    }))
                            })
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&view.scroll)
            .flex_1()
            .min_h_0(),
        );
    div()
        .id(("project-table", index))
        .flex()
        .flex_1()
        .h_full()
        .min_w_0()
        .min_h_0()
        .overflow_x_scroll()
        .child(table)
}

#[cfg(test)]
mod tests;
