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
    LevelAll,
    ClearLeveling,
    ExportGantt,
    ScrollLeft,
    ScrollRight,
    GoToStart,
    Find,
    Timeline,
    // Keyboard/QAT/backstage only; excluded from the ribbon inventory.
    Level,
    Save,
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
        Self::LevelAll,
        Self::ClearLeveling,
        Self::ExportGantt,
        Self::ScrollLeft,
        Self::ScrollRight,
        Self::GoToStart,
        Self::Find,
        Self::Timeline,
    ];
}

/// The Project document ribbon. Tabs, groups and command names follow Microsoft
/// Project so written instructions ("Project tab > Schedule > Set Baseline") can
/// be followed as they are; docxy-only extras sit in the nearest group.
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
                    "Schedule",
                    90,
                    vec![
                        rs::column(vec![
                            cmd(
                                "pr-indent",
                                "indent-increase",
                                "Indent Task",
                                Indent,
                                "Alt+Shift+Right",
                                "I",
                            ),
                            cmd(
                                "pr-outdent",
                                "indent-decrease",
                                "Outdent Task",
                                Outdent,
                                "Alt+Shift+Left",
                                "O",
                            ),
                        ]),
                        Control::Large(cmd(
                            "pr-link",
                            "copy",
                            "Link the Selected Tasks",
                            AddLink,
                            "Alt, T, P",
                            "P",
                        )),
                    ],
                ),
                rs::group(
                    "Insert",
                    80,
                    vec![
                        Control::Large(cmd(
                            "pr-add",
                            "table-insert-row",
                            "Task",
                            AddTask,
                            "Insert",
                            "N",
                        )),
                        rs::column(vec![cmd(
                            "pr-milestone",
                            "symbol",
                            "Milestone",
                            Milestone,
                            "Alt, T, M",
                            "M",
                        )]),
                    ],
                ),
                rs::group(
                    "Properties",
                    70,
                    vec![
                        Control::Large(cmd(
                            "pr-constraint",
                            "print-layout",
                            "Information",
                            Constraint,
                            "Alt, T, C",
                            "C",
                        )),
                        rs::column(vec![
                            cmd("pr-rename", "font-name", "Rename", Rename, "Alt, T, R", "R"),
                            cmd(
                                "pr-duration",
                                "rule",
                                "Duration",
                                Duration,
                                "Alt, T, D",
                                "D",
                            ),
                        ]),
                    ],
                ),
                rs::group(
                    "Editing",
                    60,
                    vec![rs::column(vec![
                        cmd("pr-find", "find", "Find", Find, "Ctrl+F / F3 next", "F"),
                        cmd(
                            "pr-delete",
                            "table-delete-row",
                            "Delete Task",
                            DeleteTask,
                            "Delete",
                            "X",
                        ),
                    ])],
                ),
            ],
        ),
        rs::tab(
            "Resource",
            "U",
            vec![
                rs::group(
                    "Assignments",
                    90,
                    vec![
                        Control::Large(cmd(
                            "pr-assign",
                            "comment",
                            "Assign Resources",
                            Assign,
                            "Alt, U, A",
                            "A",
                        )),
                        rs::column(vec![cmd(
                            "pr-clear",
                            "table-delete-row",
                            "Clear Resources",
                            ClearResources,
                            "Alt, U, R",
                            "R",
                        )]),
                    ],
                ),
                rs::group(
                    "Level",
                    80,
                    vec![
                        Control::Large(cmd(
                            "pr-level-all",
                            "align-left",
                            "Level All",
                            LevelAll,
                            "Alt, U, L  (Ctrl+Shift+L toggles)",
                            "L",
                        )),
                        rs::column(vec![cmd(
                            "pr-level-clear",
                            "table-delete-row",
                            "Clear Leveling",
                            ClearLeveling,
                            "Alt, U, C",
                            "C",
                        )]),
                    ],
                ),
            ],
        ),
        rs::tab(
            "Report",
            "R",
            vec![rs::group(
                "Export",
                90,
                vec![Control::Large(cmd(
                    "pr-export",
                    "save",
                    "Export Gantt",
                    ExportGantt,
                    "Ctrl+E",
                    "E",
                ))],
            )],
        ),
        rs::tab(
            "Project",
            "P",
            vec![rs::group(
                "Schedule",
                90,
                vec![
                    Control::Large(cmd(
                        "pr-recalc",
                        "redo",
                        "Calculate Project",
                        Recalc,
                        "Alt, P, E",
                        "E",
                    )),
                    Control::Large(cmd(
                        "pr-baseline",
                        "save",
                        "Set Baseline",
                        Baseline,
                        "Alt, P, B",
                        "B",
                    )),
                ],
            )],
        ),
        rs::tab(
            "View",
            "W",
            vec![
                rs::group(
                    "Split View",
                    70,
                    vec![rs::column(vec![cmd(
                        "pr-timeline",
                        "rule",
                        "Timeline",
                        Timeline,
                        "Alt, W, T",
                        "T",
                    )])],
                ),
                rs::group(
                    "Zoom",
                    90,
                    vec![rs::column(vec![
                        cmd(
                            "pr-left",
                            "indent-decrease",
                            "Scroll Left",
                            ScrollLeft,
                            "Alt+Left",
                            "L",
                        ),
                        cmd(
                            "pr-right",
                            "indent-increase",
                            "Scroll Right",
                            ScrollRight,
                            "Alt+Right",
                            "R",
                        ),
                        cmd(
                            "pr-start",
                            "indent-decrease",
                            "Go to Start",
                            GoToStart,
                            "Alt, W, G",
                            "G",
                        ),
                    ])],
                ),
            ],
        ),
    ])
}

/// A Project command's checked state on the ribbon.
pub(crate) fn project_act_active(v: &ProjectView, act: ProjectAct) -> bool {
    match act {
        ProjectAct::LevelAll => v.ed.leveled(),
        ProjectAct::Timeline => v.timeline,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Rename,
    Duration,
    Predecessor,
    Constraint,
    Assign,
    Find,
    /// Yes/no: delete the prompt's summary and its subtasks.
    ConfirmDelete,
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
            Self::ConfirmDelete => "delete",
        }
    }
    /// A confirmation has no input box, so typing must not fill its buffer.
    fn takes_text(self) -> bool {
        self != Self::ConfirmDelete
    }
    fn label(self) -> &'static str {
        match self {
            Self::Rename => "Rename",
            Self::Duration => "Duration (3d / 4h / 2w)",
            Self::Predecessor => "Predecessor ID",
            Self::Constraint => "Constraint (TYPE [date])",
            Self::Assign => "Assign resource (empty to clear)",
            Self::Find => "Find",
            Self::ConfirmDelete => "Delete",
        }
    }
}

/// The prompt bar's label. A delete confirmation names the task and how many
/// subtasks go with it, read from the editor so it cannot go stale.
pub(crate) fn prompt_label(p: &ProjectPrompt, ed: &ProjectEditor) -> SharedString {
    if p.kind != PromptKind::ConfirmDelete {
        return p.kind.label().into();
    }
    let Some(uid) = p.uid else {
        return p.kind.label().into();
    };
    let name = ed.project().task(uid).map_or("", |t| &t.name);
    let n = ed.subtree_len(uid).unwrap_or(0);
    let noun = if n == 1 { "subtask" } else { "subtasks" };
    format!("Delete '{name}' and its {n} {noun}? Enter = delete, Esc = cancel").into()
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectPrompt {
    pub kind: PromptKind,
    pub buf: String,
    pub uid: Option<i32>,
}

impl ProjectView {
    /// Task prompts do not open on the entry row, which has no task; Find does.
    pub fn open_prompt(&mut self, kind: PromptKind) {
        if kind != PromptKind::Find && self.on_entry_row() {
            return;
        }
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
                .then(|| self.selected_uid())
                .flatten(),
        });
    }
    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
    }
    pub fn select_row(&mut self, row: usize) {
        self.cancel_prompt();
        self.entry = false;
        self.ed.select(row);
    }

    /// Find the next match; an empty query repeats the last search. From the
    /// entry row the search starts at the first task, and a hit leaves it.
    pub fn find(&mut self, query: &str) -> Option<String> {
        if self.on_entry_row() {
            // Tasks added after the cursor went there (Redo, the control
            // pipe) can leave the selection short of the last task.
            self.ed.select(usize::MAX);
        }
        let status = find_status(&mut self.ed, query);
        if matches!(status, Some((FindOutcome::Found(_), _))) {
            self.entry = false;
        }
        status.map(|(_, status)| status)
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
                if prompt.kind.takes_text() {
                    prompt.buf.pop();
                }
                v.prompt = Some(prompt);
            }
            "tab" => v.prompt = Some(prompt),
            _ => {
                if let Some(text) = text.filter(|_| prompt.kind.takes_text()) {
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
        let before = (v.cursor_row(), v.selected_uid());
        v.key(key, m.shift);
        let changed = before != (v.cursor_row(), v.selected_uid());
        complete_project(tab, changed);
    }
    None
}

fn find_status(ed: &mut ProjectEditor, query: &str) -> Option<(FindOutcome, String)> {
    let outcome = ed.find(query);
    let status = match outcome {
        FindOutcome::Inactive => return None,
        FindOutcome::Found(_) => format!("Found '{}'  (F3 next)", ed.find_query()),
        FindOutcome::NotFound => format!("No task matching '{}'", ed.find_query()),
    };
    Some((outcome, status))
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
        return Ok(v.find(&p.buf));
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
        PromptKind::ConfirmDelete => {
            v.ed.delete_task(uid)?;
        }
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
        v.latch_entry_row();
        // The list always holds the entry row, so there is a row to reveal.
        if reveal {
            v.scroll
                .scroll_to_item(v.cursor_row(), ScrollStrategy::Nearest);
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
                // On the entry row there is no task to insert after: append.
                let at = v.ed.add_task(v.selected_uid(), "New task", 480)?;
                v.select_row(at);
            }
            DeleteTask => {
                if let Some(uid) = v.selected_uid() {
                    // A summary takes its subtasks with it, so ask first.
                    if v.ed.subtree_len(uid)? > 0 {
                        v.open_prompt(PromptKind::ConfirmDelete);
                    } else {
                        v.ed.delete_task(uid)?;
                    }
                }
            }
            Milestone => {
                if let Some(uid) = v.selected_uid() {
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
                if let Some(uid) = v.selected_uid() {
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
            Level | LevelAll | ClearLeveling => {
                let want = match act {
                    LevelAll => true,
                    ClearLeveling => false,
                    _ => !v.ed.leveled(),
                };
                if v.ed.leveled() != want {
                    v.ed.toggle_level();
                }
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
            FindNext => status = v.find(""),
            ScrollLeft => {
                v.pan_gantt(false);
            }
            ScrollRight => {
                v.pan_gantt(true);
            }
            GoToStart => v.gantt_x.set(0.),
            Timeline => v.timeline = !v.timeline,
            Save | ExportGantt => {} // window-dependent host actions
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
        let bytes = projcore::gantt::to_markdown(v.ed.project(), v.ed.schedule());
        opccore::fsio::export_atomic(tab.path.as_deref(), target, bytes.as_bytes())
            .map_err(|e| format!("Export failed: {e}"))?;
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
    /// The prompt bar's confirm button: the same commit as Enter.
    pub(crate) fn project_prompt_confirm(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if let Surface::Project(v) = &mut tab.surface {
            if let Some(prompt) = v.prompt.take() {
                commit_prompt(tab, prompt);
            }
        }
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
        let confirm = !prompt.kind.takes_text();
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
                        .text_color(if confirm { pal.fg } else { pal.dim })
                        .child(prompt_label(prompt, &v.ed)),
                )
                // A yes/no question gets a button, not an input box.
                .when(confirm, |bar| {
                    bar.child(
                        div()
                            .id("project-prompt-confirm")
                            .px_2()
                            .cursor_pointer()
                            .text_color(hsla_u(BRAND))
                            .child(prompt.kind.label())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.project_prompt_confirm();
                                this.refocus(window, cx);
                            })),
                    )
                })
                .when(!confirm, |bar| {
                    bar.child(
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
                })
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
