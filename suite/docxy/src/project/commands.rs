//! Project commands and prompt policy: pure DocTab functions first, window host glue last.
use super::*;
use projcore::editor::{AssignOutcome, FindOutcome, constraint_hint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectAct {
    AddTask,
    DeleteTask,
    Milestone,
    Indent,
    Outdent,
    Rename,
    Duration,
    AddLink,
    Constraint,
    Baseline,
    Recalc,
    Assign,
    ClearResources,
    ExportGantt,
    ScrollLeft,
    ScrollRight,
    GoToStart,
    Theme,
    Level,
    Save,
    SaveAs,
    Find,
    // Keyboard/QAT only; excluded from the ribbon inventory.
    Undo,
    Redo,
    FindNext,
}

impl ProjectAct {
    #[cfg(test)]
    pub const RIBBON: &[Self] = &[
        Self::AddTask,
        Self::DeleteTask,
        Self::Milestone,
        Self::Indent,
        Self::Outdent,
        Self::Rename,
        Self::Duration,
        Self::AddLink,
        Self::Constraint,
        Self::Baseline,
        Self::Recalc,
        Self::Assign,
        Self::ClearResources,
        Self::ExportGantt,
        Self::ScrollLeft,
        Self::ScrollRight,
        Self::GoToStart,
        Self::Theme,
        Self::Level,
        Self::Save,
        Self::SaveAs,
        Self::Find,
    ];
}

pub(crate) fn project_ribbon() -> rs::Ribbon<Act> {
    use ProjectAct::*;
    let cmd = |id, icon, label, act, shortcut, key| {
        cmdt(id, icon, label, Act::Project(act), shortcut).key(key)
    };
    rs::Ribbon::new(vec![
        rs::tab(
            "Task",
            "T",
            vec![
                rs::group(
                    "Tasks",
                    90,
                    vec![
                        Control::Large(cmd(
                            "pr-add",
                            "table-insert-row",
                            "Add Task",
                            AddTask,
                            "Insert",
                            "N",
                        )),
                        rs::column(vec![
                            cmd(
                                "pr-delete",
                                "table-delete-row",
                                "Delete",
                                DeleteTask,
                                "Delete",
                                "X",
                            ),
                            cmd(
                                "pr-milestone",
                                "symbol",
                                "Milestone",
                                Milestone,
                                "Alt, T, M",
                                "M",
                            ),
                        ]),
                    ],
                ),
                rs::group(
                    "Outline",
                    70,
                    vec![rs::column(vec![
                        cmd(
                            "pr-indent",
                            "indent-increase",
                            "Indent",
                            Indent,
                            "Alt+Shift+Right",
                            "I",
                        ),
                        cmd(
                            "pr-outdent",
                            "indent-decrease",
                            "Outdent",
                            Outdent,
                            "Alt+Shift+Left",
                            "O",
                        ),
                    ])],
                ),
                rs::group(
                    "Edit",
                    80,
                    vec![rs::column(vec![
                        cmd("pr-rename", "font-name", "Rename", Rename, "Alt, T, R", "R"),
                        cmd(
                            "pr-duration",
                            "rule",
                            "Duration",
                            Duration,
                            "Alt, T, D",
                            "D",
                        ),
                        cmd("pr-find", "find", "Find", Find, "Ctrl+F / F3 next", "F"),
                    ])],
                ),
                rs::group(
                    "File",
                    20,
                    vec![rs::column(vec![
                        cmd("pr-save", "save", "Save", Save, "Ctrl+S", "S"),
                        cmd("pr-saveas", "save", "Save As", SaveAs, "Alt, T, A", "A"),
                    ])],
                ),
            ],
        ),
        rs::tab(
            "Schedule",
            "S",
            vec![
                rs::group(
                    "Dependencies",
                    90,
                    vec![
                        Control::Large(cmd("pr-link", "copy", "Link", AddLink, "Alt, S, P", "P")),
                        rs::column(vec![
                            cmd(
                                "pr-constraint",
                                "print-layout",
                                "Constraint",
                                Constraint,
                                "Alt, S, C",
                                "C",
                            ),
                            cmd("pr-recalc", "redo", "Recalculate", Recalc, "Alt, S, E", "E"),
                        ]),
                    ],
                ),
                rs::group(
                    "Resources",
                    80,
                    vec![
                        rs::column(vec![
                            cmd("pr-assign", "comment", "Assign", Assign, "Alt, S, A", "A"),
                            cmd(
                                "pr-clear",
                                "table-delete-row",
                                "Clear resources",
                                ClearResources,
                                "Alt, S, R",
                                "R",
                            ),
                        ]),
                        Control::Toggle(cmd(
                            "pr-level",
                            "align-left",
                            "Level resources",
                            Level,
                            "Ctrl+Shift+L",
                            "L",
                        )),
                    ],
                ),
                rs::group(
                    "Baseline",
                    70,
                    vec![Control::Large(cmd(
                        "pr-baseline",
                        "save",
                        "Set baseline",
                        Baseline,
                        "Alt, S, B",
                        "B",
                    ))],
                ),
            ],
        ),
        rs::tab(
            "View",
            "W",
            vec![
                rs::group(
                    "Gantt",
                    90,
                    vec![
                        Control::Large(cmd(
                            "pr-export",
                            "save",
                            "Export Gantt",
                            ExportGantt,
                            "Ctrl+E",
                            "E",
                        )),
                        rs::column(vec![
                            cmd(
                                "pr-left",
                                "indent-decrease",
                                "Scroll left",
                                ScrollLeft,
                                "Alt+Left",
                                "L",
                            ),
                            cmd(
                                "pr-right",
                                "indent-increase",
                                "Scroll right",
                                ScrollRight,
                                "Alt+Right",
                                "R",
                            ),
                            cmd(
                                "pr-start",
                                "indent-decrease",
                                "Go to start",
                                GoToStart,
                                "Alt, W, G",
                                "G",
                            ),
                        ]),
                    ],
                ),
                rs::group(
                    "Window",
                    10,
                    vec![Control::Large(cmd(
                        "pr-theme",
                        "case",
                        "Theme",
                        Theme,
                        "Alt, W, T",
                        "T",
                    ))],
                ),
            ],
        ),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Rename,
    Duration,
    Predecessor,
    Constraint,
    Assign,
    Find,
}
impl PromptKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::Duration => "duration",
            Self::Predecessor => "predecessor",
            Self::Constraint => "constraint",
            Self::Assign => "assign",
            Self::Find => "find",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Rename => "Rename",
            Self::Duration => "Duration (3d / 4h / 2w)",
            Self::Predecessor => "Predecessor ID",
            Self::Constraint => "Constraint (TYPE [date])",
            Self::Assign => "Assign resource (empty to clear)",
            Self::Find => "Find",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectPrompt {
    pub kind: PromptKind,
    pub buf: String,
    pub uid: Option<i32>,
}

impl ProjectView {
    pub fn open_prompt(&mut self, kind: PromptKind) {
        let task = self.ed.project().tasks.get(self.ed.sel());
        let buf = match kind {
            PromptKind::Rename => task.map(|t| t.name.clone()).unwrap_or_default(),
            PromptKind::Constraint => task.map(constraint_hint).unwrap_or_default(),
            _ => String::new(),
        };
        self.prompt = Some(ProjectPrompt {
            kind,
            buf,
            uid: (kind != PromptKind::Find)
                .then(|| self.ed.selected_uid())
                .flatten(),
        });
    }
    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
    }
    pub fn select_row(&mut self, row: usize) {
        self.cancel_prompt();
        self.ed.select(row);
    }
}

pub(crate) fn key_act(key: &str, m: Modifiers) -> Option<ProjectAct> {
    use ProjectAct::*;
    if m.platform {
        return None;
    }
    if m.alt {
        if m.control {
            return None;
        }
        return match (key, m.shift) {
            ("right", true) => Some(Indent),
            ("left", true) => Some(Outdent),
            ("right", false) => Some(ScrollRight),
            ("left", false) => Some(ScrollLeft),
            _ => None,
        };
    }
    if m.control {
        return match key {
            "l" if m.shift => Some(Level),
            "f" => Some(Find),
            "z" => Some(Undo),
            "y" => Some(Redo),
            "s" => Some(Save),
            "e" => Some(ExportGantt),
            _ => None,
        };
    }
    match key {
        "insert" => Some(AddTask),
        "delete" => Some(DeleteTask),
        "f3" => Some(FindNext),
        _ => None,
    }
}

/// Handles prompts/navigation and returns mapped acts for host dispatch, using one modifier gate.
pub(crate) fn project_input(
    tab: &mut DocTab,
    key: &str,
    text: Option<&str>,
    m: Modifiers,
) -> Option<ProjectAct> {
    let Surface::Project(v) = &mut tab.surface else {
        return None;
    };
    if v.cell.is_some() {
        if m.control && !m.alt && !m.platform && key == "s" {
            return commit_project_cell(tab).then_some(ProjectAct::Save);
        }
        project_cell_input(tab, key, text, m);
        return None;
    }
    if let Some(mut prompt) = v.prompt.take() {
        if m.control || m.alt || m.platform {
            v.prompt = Some(prompt);
            return None;
        }
        match key {
            "escape" => {}
            "enter" => commit_prompt(tab, prompt),
            "backspace" => {
                prompt.buf.pop();
                v.prompt = Some(prompt);
            }
            "tab" => v.prompt = Some(prompt),
            _ => {
                if let Some(text) = text {
                    prompt.buf.extend(text.chars().filter(|c| !c.is_control()));
                }
                v.prompt = Some(prompt);
            }
        }
        return None;
    }
    if let Some(act) = key_act(key, m) {
        return Some(act);
    }
    if !m.control && !m.alt && !m.platform {
        let typed = text
            .map(|s| s.chars().filter(|c| !c.is_control()).collect::<String>())
            .filter(|s| !s.is_empty());
        if matches!(key, "enter" | "f2") || typed.is_some() {
            if let Err(e) = v.open_cell(typed.as_deref()) {
                tab.status = e.into();
            }
            return None;
        }
        let before = (v.ed.sel(), v.ed.selected_uid());
        v.key(key, m.shift);
        let changed = before != (v.ed.sel(), v.ed.selected_uid());
        complete_project(tab, changed);
    }
    None
}

fn find_status(ed: &mut ProjectEditor, query: &str) -> Option<String> {
    match ed.find(query) {
        FindOutcome::Inactive => None,
        FindOutcome::Found(_) => Some(format!("Found '{}'  (F3 next)", ed.find_query())),
        FindOutcome::NotFound => Some(format!("No task matching '{}'", ed.find_query())),
    }
}

fn assign_status(ed: &mut ProjectEditor, uid: i32, text: &str) -> Result<Option<String>, String> {
    let name = text.trim();
    Ok(match ed.assign_resource(uid, name)? {
        AssignOutcome::Assigned => Some(format!("Assigned {name}")),
        AssignOutcome::AlreadyAssigned => Some(format!("{name} is already assigned")),
        AssignOutcome::Cleared => Some("Cleared the task's resources".into()),
        AssignOutcome::NothingToClear => None,
    })
}

fn commit_edit(v: &mut ProjectView, p: ProjectPrompt) -> Result<Option<String>, String> {
    if p.kind == PromptKind::Find {
        return Ok(find_status(&mut v.ed, &p.buf));
    }
    let uid = p.uid.ok_or("No task selected")?;
    match p.kind {
        PromptKind::Rename => v.ed.rename(uid, &p.buf)?,
        PromptKind::Duration => v.ed.set_duration(uid, &p.buf)?,
        PromptKind::Predecessor => {
            let id = p
                .buf
                .trim()
                .parse::<i32>()
                .map_err(|_| "Predecessor must be a task ID (number)")?;
            let pred =
                v.ed.project()
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.uid)
                    .filter(|p| *p != uid)
                    .ok_or_else(|| format!("No other task with ID {id}"))?;
            if v.ed
                .project()
                .task(uid)
                .is_some_and(|t| t.predecessors.iter().any(|p| p.uid == pred))
            {
                return Err(format!("Already depends on {id}"));
            }
            v.ed.add_predecessor(uid, pred, LinkType::FinishStart, 0)?;
        }
        PromptKind::Constraint => {
            v.ed.set_constraint(uid, &p.buf)?;
            return Ok(Some(format!(
                "Constraint set: {}",
                p.buf
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_uppercase()
            )));
        }
        PromptKind::Assign => return assign_status(&mut v.ed, uid, &p.buf),
        PromptKind::Find => unreachable!(),
    }
    Ok(None)
}

pub(crate) fn commit_prompt(tab: &mut DocTab, prompt: ProjectPrompt) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    v.cancel_prompt();
    match commit_edit(v, prompt) {
        Ok(Some(status)) | Err(status) => tab.status = status.into(),
        Ok(None) => {}
    }
    complete_project(tab, true);
}

/// Sync dirty state; reveal only when the caller requested selection visibility.
pub(crate) fn complete_project(tab: &mut DocTab, reveal: bool) {
    if let Surface::Project(v) = &mut tab.surface {
        tab.dirty = v.ed.dirty();
        if reveal && !v.ed.project().tasks.is_empty() {
            v.scroll.scroll_to_item(v.ed.sel(), ScrollStrategy::Nearest);
        }
    }
}

pub(crate) fn apply_project_act(tab: &mut DocTab, act: ProjectAct) {
    if !commit_project_cell(tab) {
        return;
    }
    use ProjectAct::*;
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    v.cancel_prompt();
    let mut status = None;
    let result: Result<(), String> = (|| {
        match act {
            AddTask => {
                let at = v.ed.add_task(v.ed.selected_uid(), "New task", 480)?;
                v.ed.select(at);
            }
            DeleteTask => {
                if let Some(uid) = v.ed.selected_uid() {
                    v.ed.delete_task(uid)?;
                }
            }
            Milestone => {
                if let Some(uid) = v.ed.selected_uid() {
                    v.ed.toggle_milestone(uid)?;
                }
            }
            Indent | Outdent => {} // shared no-op-at-limit policy below
            Rename => v.open_prompt(PromptKind::Rename),
            Duration => v.open_prompt(PromptKind::Duration),
            AddLink => v.open_prompt(PromptKind::Predecessor),
            Constraint => v.open_prompt(PromptKind::Constraint),
            Assign => v.open_prompt(PromptKind::Assign),
            Find => v.open_prompt(PromptKind::Find),
            ClearResources => {
                if let Some(uid) = v.ed.selected_uid() {
                    status = assign_status(&mut v.ed, uid, "")?;
                }
            }
            Baseline => {
                if !v.ed.project().tasks.is_empty() {
                    v.ed.set_baseline();
                    status =
                        Some("Baseline set — baseline bars now show under the current bars".into());
                }
            }
            Level => {
                v.ed.toggle_level();
                status = Some(
                    if v.ed.leveled() {
                        "Resource leveling ON — bars delayed to fit resource capacity"
                    } else {
                        "Resource leveling OFF"
                    }
                    .into(),
                );
            }
            Recalc => status = Some("Rescheduled (automatic on every edit)".into()),
            Undo => {
                status = Some(
                    if v.ed.undo() {
                        "Undo"
                    } else {
                        "Nothing to undo"
                    }
                    .into(),
                )
            }
            Redo => {
                status = Some(
                    if v.ed.redo() {
                        "Redo"
                    } else {
                        "Nothing to redo"
                    }
                    .into(),
                )
            }
            FindNext => status = find_status(&mut v.ed, ""),
            ScrollLeft => {
                v.pan_gantt(false);
            }
            ScrollRight => {
                v.pan_gantt(true);
            }
            GoToStart => v.gantt_x = 0.,
            Save | SaveAs | ExportGantt | Theme => {} // window-dependent host actions
        }
        Ok(())
    })();
    if let Err(e) = result {
        status = Some(e);
    }
    if let Some(status) = status {
        tab.status = status.into();
    }
    if matches!(act, Indent | Outdent) {
        indent_project(tab, if act == Indent { 1 } else { -1 });
    }
    complete_project(
        tab,
        matches!(act, AddTask | DeleteTask | FindNext | Undo | Redo),
    );
}

#[derive(Debug, PartialEq)]
pub(crate) enum ExportDecision {
    InPlace(PathBuf),
    Dialog { suggested: String },
    RefuseHarness,
    Unsaveable,
}
pub(crate) fn export_decision(tab: &DocTab, harness: bool) -> ExportDecision {
    if !matches!(tab.surface, Surface::Project(_)) {
        return ExportDecision::Unsaveable;
    }
    if let Some(p) = &tab.path {
        return ExportDecision::InPlace(p.with_extension("md"));
    }
    if harness {
        ExportDecision::RefuseHarness
    } else {
        ExportDecision::Dialog {
            suggested: "schedule.md".into(),
        }
    }
}
pub(crate) fn apply_export(tab: &mut DocTab, target: &Path) -> Result<usize, String> {
    let result: Result<usize, String> = (|| {
        let Surface::Project(v) = &tab.surface else {
            return Err("This project could not be loaded and cannot be exported".into());
        };
        if tab.path.as_deref() == Some(target) {
            return Err("Export cannot overwrite the source project".into());
        }
        let bytes = projcore::gantt::to_markdown(v.ed.project(), v.ed.schedule());
        std::fs::write(target, &bytes).map_err(|e| format!("Export failed: {e}"))?;
        Ok(bytes.len())
    })();
    match &result {
        Ok(_) => {
            if let Surface::Project(v) = &mut tab.surface {
                v.exported = Some(file_name(target));
            }
            tab.status = format!("Exported Gantt to {}", target.display()).into();
        }
        Err(e) => tab.status = SharedString::from(e.clone()),
    }
    result
}
pub(crate) fn finish_project_export(tab: &mut DocTab, target: Option<&Path>) {
    if let Some(p) = target {
        let _ = apply_export(tab, p);
    } else {
        tab.status = "export cancelled".into();
    }
}

pub(crate) fn project_info_lines(ed: &ProjectEditor) -> Vec<String> {
    let mut lines = vec![
        ed.project().name.clone(),
        format!(
            "Start: {}   Finish: {}",
            date(Some(ed.schedule().project_start)),
            date(Some(ed.schedule().project_finish))
        ),
        format!(
            "{} tasks · {} critical",
            ed.project().tasks.len(),
            ed.schedule().results().filter(|r| r.critical).count()
        ),
    ];
    lines.extend(ed.project().tasks.iter().take(16).map(|t| {
        format!(
            "{}• {}",
            "  ".repeat(t.outline_level.saturating_sub(1).min(20) as usize),
            t.name
        )
    }));
    lines
}

impl Docxy {
    pub(crate) fn ribbon_kind(&self) -> Kind {
        self.tabs
            .get(self.active)
            .map(|t| t.kind)
            .unwrap_or(Kind::Docx)
    }
    pub(crate) fn project_prompt_open(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| matches!(&t.surface, Surface::Project(v) if v.prompt.is_some()))
    }
    pub(crate) fn project_edit_open(&self) -> bool {
        self.project_prompt_open()
            || self
                .tabs
                .get(self.active)
                .is_some_and(|t| matches!(&t.surface, Surface::Project(v) if v.cell.is_some()))
    }
    pub(crate) fn commit_active_project_cell(&mut self) -> bool {
        self.tabs
            .get_mut(self.active)
            .is_none_or(commit_project_cell)
    }
    pub(crate) fn project_tab_key(
        &mut self,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            project_input(
                tab,
                "tab",
                None,
                Modifiers {
                    shift,
                    ..Modifiers::default()
                },
            );
        }
        self.refocus(window, cx);
    }
    pub(crate) fn project_prompt_cancel(&mut self) {
        if let Some(Surface::Project(v)) = self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            v.cancel_prompt();
        }
    }
    pub(crate) fn project_act(
        &mut self,
        act: ProjectAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.commit_active_project_cell() {
            self.refocus(window, cx);
            return;
        }
        self.project_prompt_cancel();
        match act {
            ProjectAct::Save => return self.save_project(false, window, cx),
            ProjectAct::SaveAs => return self.save_project(true, window, cx),
            ProjectAct::Theme => return self.cycle_theme(window, cx),
            ProjectAct::ExportGantt => {
                let Some(tab) = self.tabs.get(self.active) else {
                    return;
                };
                match export_decision(tab, self.harness.is_some()) {
                    ExportDecision::InPlace(p) => finish_project_export(&mut self.tabs[self.active], Some(&p)),
                    ExportDecision::Dialog { suggested } => {
                        let target = rfd::FileDialog::new().add_filter("Markdown", &["md"]).set_file_name(suggested).save_file();
                        finish_project_export(&mut self.tabs[self.active], target.as_deref());
                    },
                    ExportDecision::RefuseHarness => self.tabs[self.active].status = "This project needs an export filename, and a harness instance cannot open the export dialog".into(),
                    ExportDecision::Unsaveable => self.tabs[self.active].status = "This project could not be loaded and cannot be exported".into(),
                }
                self.backstage = false;
            }
            _ => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    apply_project_act(tab, act);
                }
            }
        }
        self.refocus(window, cx);
    }
    pub(crate) fn project_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.backstage {
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if let Some(act) = project_input(
            tab,
            ev.keystroke.key.as_str(),
            ev.keystroke.key_char.as_deref(),
            ev.keystroke.modifiers,
        ) {
            return self.project_act(act, window, cx);
        }
        self.refocus(window, cx);
    }
    pub(crate) fn project_prompt_bar(
        &self,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Surface::Project(v) = &self.tabs.get(self.active)?.surface else {
            return None;
        };
        let prompt = v.prompt.as_ref()?;
        Some(
            h_flex()
                .w_full()
                .h(px(32.))
                .flex_none()
                .items_center()
                .gap_2()
                .px_2()
                .bg(pal.panel)
                .border_b_1()
                .border_color(pal.border)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(pal.dim)
                        .child(prompt.kind.label()),
                )
                .child(
                    h_flex()
                        .min_w(px(180.))
                        .max_w(px(500.))
                        .h(px(24.))
                        .px_2()
                        .items_center()
                        .overflow_hidden()
                        .border_1()
                        .border_color(hsla_u(BRAND))
                        .text_color(pal.fg)
                        .child(prompt.buf.clone())
                        .child(div().w(px(1.5)).h(px(14.)).bg(hsla_u(BRAND))),
                )
                .child(
                    div()
                        .id("project-prompt-cancel")
                        .px_2()
                        .cursor_pointer()
                        .text_color(pal.fg)
                        .child("Cancel")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.project_prompt_cancel();
                            this.refocus(window, cx);
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests;
