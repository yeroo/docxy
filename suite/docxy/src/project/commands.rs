//! Project commands and prompt policy: pure DocTab functions first, window host glue last.
use super::*;
use crate::dialog::{ButtonRole, Dialog, DialogOwner, DialogStack};
use projcore::editor::{AssignOutcome, FindOutcome, constraint_hint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectAct {
    AddTask,
    /// Task › Insert › Blank Row: an empty row above the selected one.
    InsertBlankRow,
    Milestone,
    Indent,
    Outdent,
    AddLink,
    UnlinkTasks,
    Inactivate,
    ManuallySchedule,
    AutoSchedule,
    MoveTask,
    Constraint,
    /// Task › Properties › Notes... and a task row's context menu › Notes...:
    /// the selected task's notes in one prompt (`\n` types a newline; empty
    /// removes), like Project's Notes command.
    Notes,
    Baseline,
    ClearBaseline,
    Recalc,
    Assign,
    LevelAll,
    ClearLeveling,
    /// View › Data › Outline: Show Subtasks / Hide Subtasks.
    ShowSubtasks,
    HideSubtasks,
    Find,
    ScrollToTask,
    Timeline,
    /// Gantt Chart Format › Bar Styles › Critical Tasks.
    CriticalTasks,
    /// Gantt Chart Format › Bar Styles › Baseline.
    BaselineBars,
    // Keyboard/QAT/backstage/status bar only; excluded from the ribbon inventory.
    Level,
    /// Ctrl+E and File › Export.
    ExportGantt,
    /// Alt+Left / Alt+Right: pan the timescale.
    ScrollLeft,
    ScrollRight,
    /// Alt+Home: the timescale back to the project start.
    GoToStart,
    /// Alt+End: the timescale to the project finish.
    GoToFinish,
    /// The status bar's `New Tasks: …` item: the plan's mode for new tasks.
    NewTasksMode,
    Save,
    Undo,
    Redo,
    FindNext,
    /// Delete key: clear the active cell (the task itself on the ID column).
    ClearCell,
    /// Ctrl+Delete: clear the active cell, or reset it to its default; never
    /// deletes the task.
    ResetCell,
    /// A task row's context menu › Delete Task: the selected task, whatever
    /// column the cursor is on; a summary asks first, as Delete on its ID does.
    DeleteTask,
    /// Ctrl+K and a task row's context menu › Hyperlink...: the selected
    /// task's hyperlink (display text, address, in-file location) in one
    /// prompt. Keyboard and row-menu only; no ribbon command.
    Hyperlink,
    /// Ctrl+C / Ctrl+X / Ctrl+V: the cursor cell (or range) on the table, the
    /// whole buffer in an open cell edit (#561); also the Task tab's
    /// Clipboard group. See `clip.rs`.
    Copy,
    Cut,
    Paste,
    /// F11: a new, empty project in its own tab, as Backstage › New › Project.
    NewProject,
    /// Shift+F11 / View › Window: move the active tab into a new window
    /// (#587); a one-tab window opens the new window on a blank document.
    NewWindow,
    /// View › Window: resize every window to an equal strip of this window's
    /// display (gpui cannot move windows at the pinned rev, so positions
    /// stay).
    ArrangeAll,
}

impl ProjectAct {
    #[cfg(test)]
    pub const RIBBON: &[Self] = &[
        Self::AddTask,
        Self::InsertBlankRow,
        Self::Milestone,
        Self::Indent,
        Self::Outdent,
        Self::AddLink,
        Self::UnlinkTasks,
        Self::Inactivate,
        Self::ManuallySchedule,
        Self::AutoSchedule,
        Self::MoveTask,
        Self::Constraint,
        Self::Notes,
        Self::Baseline,
        Self::ClearBaseline,
        Self::Recalc,
        Self::Assign,
        Self::LevelAll,
        Self::ClearLeveling,
        Self::ShowSubtasks,
        Self::HideSubtasks,
        Self::Find,
        Self::ScrollToTask,
        Self::Timeline,
        Self::CriticalTasks,
        Self::BaselineBars,
        Self::Paste,
        Self::Cut,
        Self::Copy,
        Self::NewWindow,
        Self::ArrangeAll,
    ];
}

/// The Project document ribbon. Tabs, groups, command labels and screentips
/// follow Microsoft Project 2024, so written instructions can be followed as
/// they are, whether they name a command by its label ("Task › Schedule ›
/// Indent") or by its screentip ("Indent Task"). Only Project's own commands
/// are here; docxy's extras keep their keyboard and cell routes.
pub(crate) fn project_ribbon() -> rs::Ribbon<Act> {
    use ProjectAct::*;
    // `cmd` for a command whose screentip is its label, `cmds` when Project's
    // screentip differs from the label.
    let cmd = |id, icon, label, act, shortcut, key| {
        cmdt(id, icon, label, Act::Project(act), shortcut).key(key)
    };
    let cmds = |id, icon, label, tip, act, shortcut, key| {
        rs::cmd(id, icon, label, Act::Project(act))
            .tip(tip, "", shortcut)
            .key(key)
    };
    rs::Ribbon::new(vec![
        rs::tab(
            "Task",
            "T",
            vec![
                // Clipboard leads the Task tab, as in Project: Paste large,
                // Cut and Copy beside it (#561).
                rs::group(
                    "Clipboard",
                    10,
                    vec![
                        Control::Large(cmd("pr-paste", "paste", "Paste", Paste, "Ctrl+V", "W")),
                        rs::column(vec![
                            cmd("pr-cut", "cut", "Cut", Cut, "Ctrl+X", "X"),
                            cmd("pr-copy", "copy", "Copy", Copy, "Ctrl+C", "Y"),
                        ]),
                    ],
                ),
                rs::group(
                    "Schedule",
                    125,
                    vec![
                        rs::column(vec![
                            cmds(
                                "pr-indent",
                                "indent-increase",
                                "Indent",
                                "Indent Task",
                                Indent,
                                "Alt+Shift+Right",
                                "I",
                            ),
                            cmds(
                                "pr-outdent",
                                "indent-decrease",
                                "Outdent",
                                "Outdent Task",
                                Outdent,
                                "Alt+Shift+Left",
                                "O",
                            ),
                            cmd(
                                "pr-unlink",
                                "cut",
                                "Unlink Tasks",
                                UnlinkTasks,
                                "Alt, T, U  (Ctrl+Shift+F2)",
                                "U",
                            ),
                        ]),
                        Control::Large(cmds(
                            "pr-link",
                            "copy",
                            "Link Tasks",
                            "Link the Selected Tasks",
                            AddLink,
                            "Alt, T, P  (Ctrl+F2)",
                            "P",
                        )),
                        rs::column(vec![cmd(
                            "pr-inactivate",
                            "strikethrough",
                            "Inactivate",
                            Inactivate,
                            "Alt, T, E",
                            "E",
                        )]),
                    ],
                ),
                rs::group(
                    "Tasks",
                    190,
                    vec![
                        // Project's order: the two modes, then Move.
                        rs::column(vec![
                            cmd(
                                "pr-manual",
                                "lock",
                                "Manually Schedule",
                                ManuallySchedule,
                                "Alt, T, H",
                                "H",
                            ),
                            cmd(
                                "pr-auto",
                                "redo",
                                "Auto Schedule",
                                AutoSchedule,
                                "Alt, T, A",
                                "A",
                            ),
                        ]),
                        Control::Large(cmds(
                            "pr-move",
                            "indent-increase",
                            "Move",
                            "Move Task",
                            MoveTask,
                            "Alt, T, V",
                            "V",
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
                            "Alt, T, N",
                            "N",
                        )),
                        rs::column(vec![
                            cmds(
                                "pr-milestone",
                                "symbol",
                                "Milestone",
                                "Insert Milestone",
                                Milestone,
                                "Alt, T, M",
                                "M",
                            ),
                            cmds(
                                "pr-blank-row",
                                "table-insert-row",
                                "Blank Row",
                                "Insert Blank Row",
                                InsertBlankRow,
                                "Insert",
                                "B",
                            ),
                        ]),
                    ],
                ),
                rs::group(
                    "Properties",
                    70,
                    vec![
                        Control::Large(cmds(
                            "pr-constraint",
                            "print-layout",
                            "Information...",
                            "View Task Information",
                            Constraint,
                            "Alt, T, C",
                            "C",
                        )),
                        // Project 2024's keytips take "N" for Insert › Task;
                        // "T" is free on the Task tab.
                        rs::column(vec![cmds(
                            "pr-notes",
                            "comment",
                            "Notes...",
                            "Notes",
                            Notes,
                            "Alt, T, T",
                            "T",
                        )]),
                    ],
                ),
                rs::group(
                    "Editing",
                    60,
                    vec![rs::column(vec![
                        cmd("pr-find", "find", "Find...", Find, "Ctrl+F / F3 next", "F"),
                        cmd(
                            "pr-scroll-to-task",
                            "align-left",
                            "Scroll to Task",
                            ScrollToTask,
                            "Alt, T, S",
                            "S",
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
                    vec![Control::Large(cmd(
                        "pr-assign",
                        "comment",
                        "Assign Resources...",
                        Assign,
                        "Alt, U, A",
                        "A",
                    ))],
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
        // Project's Report groups (View Reports, Export › Visual Reports) are
        // not implemented; the tab stays so the tab set is Project's (#72).
        rs::tab("Report", "R", Vec::new()),
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
                    // A split button: the upper half sets the baseline at
                    // once, the arrow opens Set Baseline... / Clear
                    // Baseline... (#397). Project 2024 draws a menu button
                    // here, with no one-press half; the one press is kept
                    // so Alt, P, B still sets the baseline.
                    Control::Split {
                        primary: cmd(
                            "pr-baseline",
                            "save",
                            "Set Baseline",
                            Baseline,
                            "Alt, P, B",
                            "B",
                        ),
                        menu: vec![
                            cmds(
                                "pr-baseline-set",
                                "save",
                                "Set Baseline...",
                                "Set Baseline",
                                Baseline,
                                "Alt, P, B",
                                "B",
                            ),
                            cmds(
                                "pr-baseline-clear",
                                "table-delete-row",
                                "Clear Baseline...",
                                "Clear Baseline",
                                ClearBaseline,
                                "Alt, P, L",
                                "L",
                            ),
                        ],
                    },
                ],
            )],
        ),
        rs::tab(
            "View",
            "W",
            vec![
                rs::group(
                    "Data",
                    90,
                    vec![rs::column(vec![
                        cmd(
                            "pr-show-subtasks",
                            "table-insert-row",
                            "Show Subtasks",
                            ShowSubtasks,
                            "Alt+Shift+=",
                            "S",
                        ),
                        cmd(
                            "pr-hide-subtasks",
                            "table-delete-row",
                            "Hide Subtasks",
                            HideSubtasks,
                            "Alt+Shift+-",
                            "H",
                        ),
                    ])],
                ),
                rs::group(
                    "Split View",
                    70,
                    vec![rs::column(vec![cmds(
                        "pr-timeline",
                        "rule",
                        "Timeline",
                        "Timeline View",
                        Timeline,
                        "Alt, W, T",
                        "T",
                    )])],
                ),
                rs::group(
                    "Window",
                    60,
                    vec![rs::column(vec![
                        cmd(
                            "pr-new-window",
                            "new",
                            "New Window",
                            NewWindow,
                            "Shift+F11",
                            "N",
                        ),
                        cmd(
                            "pr-arrange-all",
                            "columns",
                            "Arrange All",
                            ArrangeAll,
                            "Alt, W, A",
                            "A",
                        ),
                    ])],
                ),
            ],
        ),
    ])
}

/// Project's contextual Gantt Chart Format tab, shown after View while a
/// Project document's Gantt pane is showing (see `RibbonTab::GanttFormat`).
pub(crate) fn gantt_format_tab() -> rs::Tab<Act> {
    use ProjectAct::*;
    let cmd = |id, icon, label, act, shortcut, key| {
        cmdt(id, icon, label, Act::Project(act), shortcut).key(key)
    };
    rs::tab(
        "Gantt Chart Format",
        "O",
        vec![rs::group(
            "Bar Styles",
            90,
            vec![rs::column(vec![
                cmd(
                    "pr-fmt-critical",
                    "highlight",
                    "Critical Tasks",
                    CriticalTasks,
                    "Alt, O, C",
                    "C",
                ),
                cmd(
                    "pr-fmt-baseline",
                    "rule",
                    "Baseline",
                    BaselineBars,
                    "Alt, O, B",
                    "B",
                ),
            ])],
        )],
    )
}

/// Whether a command act drops the entry-table range selection: every act
/// clears it except the ones that keep or consume it — Copy keeps the
/// highlight, Cut and Paste clear it when they run, Link and Unlink act on
/// it. The rule lives here, one place, so keys, the ribbon, the QAT and
/// Backstage cannot drift apart.
pub(crate) fn project_act_clears_range(act: ProjectAct) -> bool {
    use ProjectAct::*;
    !matches!(act, Copy | Cut | Paste | AddLink | UnlinkTasks)
}

/// Whether an in-cell cut may empty the buffer: the read-back clipboard
/// `now` holds the buffer's text `wrote`, compared CRLF-insensitively. A
/// write can fail without a word — another process holds the OS clipboard —
/// and it then reads as `Nothing` or someone else's item, never as our text;
/// the buffer must not be lost to a copy that never landed (#561 r1/r2).
/// Cutting an empty buffer is harmless and the host skips the check.
pub(crate) fn cell_cut_write_took(now: &ClipRead, wrote: &str) -> bool {
    match now {
        ClipRead::Text(held) => held.replace("\r\n", "\n") == wrote.replace("\r\n", "\n"),
        _ => false,
    }
}

/// A Project command's checked state on the ribbon.
pub(crate) fn project_act_active(v: &ProjectView, act: ProjectAct) -> bool {
    match act {
        ProjectAct::LevelAll => v.ed.leveled(),
        ProjectAct::Timeline => v.timeline,
        ProjectAct::CriticalTasks => v.show_critical,
        ProjectAct::BaselineBars => v.show_baseline,
        ProjectAct::Inactivate => selected_task_inactive(v),
        // The selected task's mode, as Project highlights it; none on the
        // entry row or a blank row.
        ProjectAct::ManuallySchedule | ProjectAct::AutoSchedule => v
            .selected_uid()
            .and_then(|uid| v.ed.project().task(uid))
            .is_some_and(|t| !t.is_null && t.manual == (act == ProjectAct::ManuallySchedule)),
        _ => false,
    }
}

/// A task row's context menu, in the order Project's Gantt Chart table draws
/// it (#397). An item with no command here is drawn disabled, so the order
/// stays Project's. The rest follow the row the cursor is on: `task` is a
/// real task (not a blank row), `row` any task row, blank ones included
/// (not the entry row, which has no task to delete).
pub(crate) fn project_row_menu(v: &ProjectView) -> Vec<crate::menu::MenuItem> {
    use crate::menu::{
        Entry,
        MenuItem::{Item, Separator},
    };
    use ProjectAct::*;
    let uid = v.selected_uid();
    let row = uid.is_some();
    let task = uid
        .and_then(|uid| v.ed.project().task(uid))
        .is_some_and(|t| !t.is_null);
    let item = |id, label, icon, act: ProjectAct, enabled| {
        Item(Entry::new(id, label, icon, Act::Project(act), enabled))
    };
    let toggle = |id, label, icon, act: ProjectAct| {
        Item(
            Entry::new(id, label, icon, Act::Project(act), task)
                .checked(task && project_act_active(v, act)),
        )
    };
    let none = |id, label| Item(Entry::unavailable(id, label));
    vec![
        item("rm-cut", "Cut", "cut", Cut, true),
        item("rm-copy", "Copy", "copy", Copy, true),
        item("rm-paste", "Paste", "paste", Paste, true),
        none("rm-paste-special", "Paste Special..."),
        Separator,
        item(
            "rm-scroll",
            "Scroll to Task",
            "align-left",
            ScrollToTask,
            task,
        ),
        Separator,
        item(
            "rm-insert",
            "Insert Task",
            "table-insert-row",
            InsertBlankRow,
            true,
        ),
        item(
            "rm-delete",
            "Delete Task",
            "table-delete-row",
            DeleteTask,
            row,
        ),
        toggle(
            "rm-inactivate",
            "Inactivate Task",
            "strikethrough",
            Inactivate,
        ),
        Separator,
        toggle("rm-manual", "Manually Schedule", "lock", ManuallySchedule),
        toggle("rm-auto", "Auto Schedule", "redo", AutoSchedule),
        Separator,
        item("rm-assign", "Assign Resources...", "comment", Assign, task),
        Separator,
        none("rm-text-styles", "Text Styles..."),
        none("rm-font", "Font..."),
        Separator,
        none("rm-fill-down", "Fill Down"),
        none("rm-clear", "Clear Contents"),
        Separator,
        item(
            "rm-info",
            "Information...",
            "print-layout",
            Constraint,
            task,
        ),
        item("rm-notes", "Notes...", "comment", Notes, task),
        none("rm-timeline", "Add to Timeline"),
        Separator,
        // No link icon in the set yet; Project's chain is drawn by the
        // column-chooser follow-up. The item still runs its command.
        item("rm-hyperlink", "Hyperlink...", "", Hyperlink, task),
    ]
}

fn selected_task_inactive(v: &ProjectView) -> bool {
    v.selected_uid()
        .and_then(|uid| v.ed.project().tasks.iter().position(|t| t.uid == uid))
        .is_some_and(|i| inactive_row(v.ed.project(), i))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Predecessor,
    Constraint,
    Move,
    Assign,
    Find,
    Hyperlink,
    Notes,
}
impl PromptKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Predecessor => "predecessor",
            Self::Constraint => "constraint",
            Self::Move => "move",
            Self::Assign => "assign",
            Self::Find => "find",
            Self::Hyperlink => "hyperlink",
            Self::Notes => "notes",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Predecessor => "Predecessor ID",
            Self::Constraint => "Constraint (TYPE [date])",
            Self::Move => "Move task by (1d / 1w / 4w; -1d back)",
            Self::Assign => "Assign resource (empty to clear)",
            Self::Find => "Find",
            Self::Hyperlink => "Hyperlink (ADDRESS[#LOCATION] [| TEXT]; empty to remove)",
            Self::Notes => "Notes (\\n = new line; empty to remove)",
        }
    }
}

/// The prompt bar's label.
pub(crate) fn prompt_label(p: &ProjectPrompt) -> SharedString {
    p.kind.label().into()
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
            PromptKind::Constraint => task.map(constraint_hint).unwrap_or_default(),
            PromptKind::Hyperlink => task.map(hyperlink_hint).unwrap_or_default(),
            PromptKind::Notes => task
                .map(|t| projcore::text::notes_to_line(t.notes.as_deref().unwrap_or("")))
                .unwrap_or_default(),
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
        // From the entry row, the search starts at the first task.
        let from_top = self.on_entry_row();
        let status = find_status(&mut self.ed, query, from_top);
        if matches!(status, Some((FindOutcome::Found(_), _))) {
            self.entry = false;
        }
        status.map(|(_, status)| status)
    }
}

/// Whether an Alt chord is one of Project's own keys (Alt+Left/Right,
/// Alt+Home/End, Alt+Shift+arrows and ±). Those reach the Project surface even
/// while KeyTips are up: pressing Alt starts KeyTips, and none of these keys
/// is a KeyTip letter, so the overlay would otherwise swallow them.
pub(crate) fn project_alt_key(key: &str, m: Modifiers) -> bool {
    m.alt && !m.control && !m.platform && key_act(key, m).is_some()
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
            // Project's Alt+Shift+Minus / Alt+Shift+Plus; the key may arrive
            // shifted or not, depending on the platform and layout.
            ("-", true) | ("_", _) => Some(HideSubtasks),
            ("=", true) | ("+", _) => Some(ShowSubtasks),
            ("right", false) => Some(ScrollRight),
            ("left", false) => Some(ScrollLeft),
            // Project: move the timescale to the project start.
            ("home", false) => Some(GoToStart),
            // Project: move the timescale to the project finish.
            ("end", false) => Some(GoToFinish),
            _ => None,
        };
    }
    if m.control {
        return match key {
            "l" if m.shift => Some(Level),
            // Project's Link / Unlink the selected tasks.
            "f2" if m.shift => Some(UnlinkTasks),
            "f2" => Some(AddLink),
            "f" => Some(Find),
            // Project's Insert Hyperlink on the selected task.
            "k" => Some(Hyperlink),
            "z" => Some(Undo),
            "y" => Some(Redo),
            "s" => Some(Save),
            "e" => Some(ExportGantt),
            "c" if !m.shift => Some(Copy),
            "x" if !m.shift => Some(Cut),
            "v" if !m.shift => Some(Paste),
            "delete" if !m.shift => Some(ResetCell),
            _ => None,
        };
    }
    match key {
        "insert" => Some(InsertBlankRow),
        "delete" => Some(ClearCell),
        "f3" => Some(FindNext),
        // Shift+F11 is Project's New Window (#587); plain F11 a new project.
        "f11" if m.shift => Some(NewWindow),
        "f11" => Some(NewProject),
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
    // A levelling pass asked for first runs first, in the order the user
    // gave them; see [`flush_level_pass`].
    flush_level_pass(tab);
    let Surface::Project(v) = &mut tab.surface else {
        return None;
    };
    if v.cell.is_some() {
        if m.control && !m.alt && !m.platform && key == "s" {
            return commit_project_cell(tab).then_some(ProjectAct::Save);
        }
        // F11's new project and Shift+F11's New Window (#587) commit the
        // open cell first, as every command act does (#561).
        if let Some(act @ (ProjectAct::NewProject | ProjectAct::NewWindow)) = key_act(key, m) {
            return commit_project_cell(tab).then_some(act);
        }
        // #561: the host edits the open cell's buffer in place; the edit
        // stays open, so the range anchor survives like it does for Copy.
        if let Some(act @ (ProjectAct::Copy | ProjectAct::Cut | ProjectAct::Paste)) =
            key_act(key, m)
        {
            return Some(act);
        }
        v.anchor = None;
        project_cell_input(tab, key, text, m);
        return None;
    }
    if let Some(mut prompt) = v.prompt.take() {
        if m.control || m.alt || m.platform {
            v.prompt = Some(prompt);
            return None;
        }
        v.anchor = None;
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
    if m.control && !m.alt && !m.platform {
        // Shift is ignored, as it is for the plain arrows.
        v.anchor = None;
        navigate(tab, |v| v.ctrl_key(key));
        return None;
    }
    if !m.control && !m.alt && !m.platform {
        let typed = text
            .map(|s| s.chars().filter(|c| !c.is_control()).collect::<String>())
            .filter(|s| !s.is_empty());
        if matches!(key, "enter" | "f2") || typed.is_some() {
            v.anchor = None;
            if let Err(e) = v.open_cell(typed.as_deref()) {
                tab.status = e.into();
            }
            return None;
        }
        if m.shift && matches!(key, "up" | "down" | "left" | "right") {
            navigate(tab, |v| v.extend_selection(key));
        } else {
            // A plain arrow, Tab, Home/End, PageUp/PageDown or Escape moves
            // the cursor; the range a Shift+arrow made does not survive.
            v.anchor = None;
            navigate(tab, |v| v.key(key, m.shift));
        }
    }
    None
}

/// Run a cursor move and, when it handled the key, complete it, revealing the
/// row only when it changed.
fn navigate(tab: &mut DocTab, step: impl FnOnce(&mut ProjectView) -> bool) {
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    let before = (v.cursor_row(), v.selected_uid());
    if step(v) {
        let changed = before != (v.cursor_row(), v.selected_uid());
        complete_project(tab, changed);
    }
}

fn find_status(
    ed: &mut ProjectEditor,
    query: &str,
    from_top: bool,
) -> Option<(FindOutcome, String)> {
    let outcome = if from_top {
        ed.find_from_top(query)
    } else {
        ed.find(query)
    };
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

/// The hyperlink prompt's initial text: `address#location | text`, parts
/// omitted when the task does not store them — except the text's ` | `
/// separator, which stays, so a text-only link prefills as ` | text` and
/// parses back text-only instead of becoming the address.
fn hyperlink_hint(task: &projcore::model::Task) -> String {
    let mut buf = String::new();
    if let Some(address) = &task.hyperlink_address {
        buf.push_str(address);
    }
    if let Some(location) = &task.hyperlink_sub_address {
        buf.push('#');
        buf.push_str(location);
    }
    if let Some(text) = &task.hyperlink {
        buf.push_str(" | ");
        buf.push_str(text);
    }
    buf
}

/// Parse the prompt's `ADDRESS[#LOCATION] [| TEXT]`: the first ` | ` splits
/// the display text, the first `#` splits the address from the location
/// (Project's own address#subaddress convention, so an address cannot hold
/// `#`), ends trimmed, an empty part absent. `| TEXT` typed without the
/// leading space is text-only, like ` | TEXT`.
fn parse_hyperlink(buf: &str) -> (Option<String>, Option<String>, Option<String>) {
    let (link, text) = match buf.trim_start().strip_prefix('|') {
        Some(text) => ("", Some(text)),
        None => match buf.split_once(" | ") {
            Some((link, text)) => (link, Some(text)),
            None => (buf, None),
        },
    };
    let (address, location) = match link.split_once('#') {
        Some((address, location)) => (Some(address), Some(location)),
        None => (Some(link), None),
    };
    let clean = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    (clean(address), clean(location), clean(text))
}

fn commit_edit(v: &mut ProjectView, p: ProjectPrompt) -> Result<Option<String>, String> {
    if p.kind == PromptKind::Find {
        return Ok(v.find(&p.buf));
    }
    let uid = p.uid.ok_or("No task selected")?;
    match p.kind {
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
        PromptKind::Move => {
            let start = v.ed.move_task(uid, &p.buf)?;
            return Ok(Some(format!("Moved to {}", date(Some(start)))));
        }
        PromptKind::Assign => return assign_status(&mut v.ed, uid, &p.buf),
        PromptKind::Hyperlink => {
            let task = v.ed.project().task(uid).ok_or("No task selected")?;
            // Enter on the prefill the prompt opened with changes nothing: a
            // stored address can hold `#` or ` | `, which the grammar would
            // re-split, and an address-only link would grow a display text.
            // An empty buffer never counts: "empty to remove" must still
            // remove a link whose stored parts are empty strings.
            if !p.buf.is_empty() && p.buf == hyperlink_hint(task) {
                return Ok(None);
            }
            let had_link = task.hyperlink.is_some()
                || task.hyperlink_address.is_some()
                || task.hyperlink_sub_address.is_some();
            let (address, location, text) = parse_hyperlink(&p.buf);
            // Text omitted displays the address, else the location, as
            // Project's Text to display defaults.
            let text = text
                .or_else(|| address.clone())
                .or_else(|| location.clone());
            v.ed.set_hyperlink(
                uid,
                text.as_deref().unwrap_or(""),
                address.as_deref().unwrap_or(""),
                location.as_deref().unwrap_or(""),
            )?;
            return Ok(match text {
                Some(text) => Some(format!("Hyperlink set: {text}")),
                None => had_link.then(|| "Hyperlink removed".into()),
            });
        }
        PromptKind::Notes => {
            let notes = projcore::text::notes_from_line(&p.buf);
            // Decoding the prefill gives back the stored notes exactly
            // (notes_from_line ∘ notes_to_line is the identity), so the
            // changed flag the editor reports on the normalised comparison
            // is exact. An empty buffer still removes a note that exists,
            // and stored empty notes count as none, so "empty to remove" is
            // not swallowed.
            if !v.ed.set_notes(uid, &notes)? {
                return Ok(None);
            }
            return Ok(Some(
                if notes.is_empty() {
                    "Notes removed"
                } else {
                    "Notes set"
                }
                .into(),
            ));
        }
        PromptKind::Find => unreachable!(),
    }
    Ok(None)
}

pub(crate) fn commit_prompt(tab: &mut DocTab, prompt: ProjectPrompt) {
    // A levelling pass asked for first runs first, in the order the user
    // gave them; see [`flush_level_pass`].
    flush_level_pass(tab);
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
        crate::bump_edit_generation();
        v.latch_entry_row();
        // The list always holds the entry row, so there is a row to reveal.
        if reveal {
            v.scroll
                .scroll_to_item(v.display_row(), ScrollStrategy::Nearest);
        }
    }
}

/// Delete the selected task. A summary takes its subtasks with it, so it is
/// not deleted here: the message box that asks first is returned, for the
/// caller to open on the tab.
fn delete_selected_task(v: &mut ProjectView) -> Result<Option<Dialog>, String> {
    let Some(uid) = v.selected_uid() else {
        return Ok(None);
    };
    let n = v.ed.subtree_len(uid)?;
    if n > 0 {
        return Ok(Some(delete_summary_dialog(&v.ed, uid, n)));
    }
    v.ed.delete_task(uid)?;
    Ok(None)
}

/// The yes/no message box a summary's delete asks: Yes deletes the summary
/// and its `n` subtasks, No (and Escape) leaves the plan alone.
pub(crate) fn delete_summary_dialog(ed: &ProjectEditor, uid: i32, n: usize) -> Dialog {
    let name = ed.project().task(uid).map_or("", |t| &t.name);
    let noun = if n == 1 { "subtask" } else { "subtasks" };
    Dialog::message(
        "delete-summary",
        "Delete Task",
        format!("Delete '{name}' and its {n} {noun}?"),
        &[("Yes", ButtonRole::Accept), ("No", ButtonRole::Cancel)],
        DialogOwner::DeleteSummary { uid },
    )
}

/// The leveled state a levelling command asks for, given the state it acts
/// on: Level toggles it, Level All and Clear Leveling set it.
fn level_target(act: ProjectAct, leveled: bool) -> bool {
    match act {
        ProjectAct::LevelAll => true,
        ProjectAct::ClearLeveling => false,
        _ => !leveled,
    }
}

/// Run the levelling pass: bring the plan to `want` and name the result for
/// the status bar. Idempotent, so a pass that runs after other edits still
/// leaves the plan exactly as asked.
fn level_to(v: &mut ProjectView, want: bool) -> &'static str {
    if v.ed.leveled() != want {
        v.ed.toggle_level();
    }
    if v.ed.leveled() {
        "Resource leveling ON — bars delayed to fit resource capacity"
    } else {
        "Resource leveling OFF"
    }
}

/// What the status bar's leftmost item says, as in Project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppState {
    Ready,
    Edit,
    Busy,
}

impl AppState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Edit => "Edit",
            Self::Busy => "Busy",
        }
    }
}

/// `Busy` while a levelling pass is pending, `Edit` while a cell editor or a
/// prompt has the keyboard, otherwise `Ready`.
pub(crate) fn project_app_state(v: &ProjectView) -> AppState {
    if v.busy.is_some() {
        AppState::Busy
    } else if v.cell.is_some() || v.prompt.is_some() {
        AppState::Edit
    } else {
        AppState::Ready
    }
}

/// A Project's status-bar state: [`project_app_state`], and `Edit` while a
/// dialog is open over it.
pub(crate) fn project_dialog_state(v: &ProjectView, dialogs: &DialogStack) -> AppState {
    match project_app_state(v) {
        AppState::Ready if dialogs.is_open() => AppState::Edit,
        state => state,
    }
}

/// [`project_dialog_state`] for any tab: `None` off a Project.
pub(crate) fn tab_app_state(tab: &DocTab) -> Option<AppState> {
    let Surface::Project(v) = &tab.surface else {
        return None;
    };
    Some(project_dialog_state(v, &tab.dialogs))
}

/// A fresh token for a levelling pass, unique for the process, so a frame
/// callback can tell its own pass from a later one and find it on whichever
/// tab it is now.
fn next_pass_token() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Ask for a levelling pass instead of running it, so the frame that says
/// `Busy` is drawn first; render schedules it and [`finish_level_pass`] or
/// [`flush_level_pass`] runs it. A pass still pending runs first, so the
/// target is resolved against the plan it left. Returns the pass's token.
pub(crate) fn request_level_pass(tab: &mut DocTab, act: ProjectAct) -> Option<u64> {
    flush_level_pass(tab);
    if !commit_project_cell(tab) {
        return None;
    }
    let Surface::Project(v) = &mut tab.surface else {
        return None;
    };
    v.cancel_prompt();
    let token = next_pass_token();
    v.busy = Some((token, level_target(act, v.ed.leveled())));
    v.scheduled = None;
    Some(token)
}

/// Run `tab`'s pending levelling pass if its token is `token`. A callback for
/// a pass that was flushed or replaced finds nothing and does nothing.
pub(crate) fn finish_level_pass(tab: &mut DocTab, token: u64) -> bool {
    match &tab.surface {
        Surface::Project(v) if v.busy.is_some_and(|(t, _)| t == token) => flush_level_pass(tab),
        _ => false,
    }
}

/// Run `tab`'s pending levelling pass now, if it has one. Called before any
/// input reaches the plan (keys, clicks, prompts, acts) and wherever it is
/// read out or saved, so nothing sees a pass half-asked-for and edits apply
/// in the order the user gave them.
pub(crate) fn flush_level_pass(tab: &mut DocTab) -> bool {
    let Surface::Project(v) = &mut tab.surface else {
        return false;
    };
    v.scheduled = None;
    let Some((_, want)) = v.busy.take() else {
        return false;
    };
    tab.status = level_to(v, want).into();
    complete_project(tab, false);
    true
}

/// [`flush_level_pass`] on every tab; whether any pass ran.
pub(crate) fn flush_level_passes(tabs: &mut [DocTab]) -> bool {
    tabs.iter_mut()
        .fold(false, |ran, tab| flush_level_pass(tab) | ran)
}

pub(crate) fn apply_project_act(tab: &mut DocTab, act: ProjectAct) {
    use ProjectAct::*;
    // The host asks for a pass and runs it a frame later; run at once, it is
    // the same two steps.
    if matches!(act, Level | LevelAll | ClearLeveling) {
        request_level_pass(tab, act);
        flush_level_pass(tab);
        return;
    }
    if !commit_project_cell(tab) {
        return;
    }
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    v.cancel_prompt();
    // A command acts on the cursor: the range does not survive it. (Copy,
    // Cut and Paste are handled by the host before they reach here; Cut and
    // Paste clear the range themselves, and Link/Unlink below are exempt in
    // [`project_act_clears_range`], which the host-side dispatch also uses.)
    if project_act_clears_range(act) {
        v.anchor = None;
    }
    let before = (v.cursor_row(), v.display_row());
    let mut status = None;
    let mut reveal_clear = false;
    let mut dialog = None;
    let result: Result<(), String> = (|| {
        match act {
            AddTask => {
                // On the entry row there is no task to insert after: append.
                // The default duration is estimated when the plan's new
                // tasks are.
                let estimated = v.ed.project().new_tasks_estimated();
                let min = v.ed.project().days_to_minutes(1.0);
                let at =
                    v.ed.add_task(v.selected_uid(), "New task", min, estimated)?;
                v.select_row(at);
            }
            InsertBlankRow => {
                // Above the selected row; on the entry row, just above it.
                let at = v.ed.insert_blank_row(v.selected_uid())?;
                v.select_row(at);
            }
            ClearCell => {
                if let Some(uid) = v.selected_uid() {
                    match v.col {
                        COL_ID => {
                            dialog = delete_selected_task(v)?;
                            reveal_clear = true;
                        }
                        COL_NAME | COL_PREDECESSORS | COL_RESOURCES => {
                            if !v.ed.project().task(uid).ok_or("No task selected")?.is_null {
                                apply_cell(&mut v.ed, uid, v.col, "")?;
                            }
                        }
                        _ => status = Some(format!("{} can't be cleared", COLUMNS[v.col])),
                    }
                }
            }
            DeleteTask => {
                dialog = delete_selected_task(v)?;
                reveal_clear = true;
            }
            ResetCell => {
                if let Some(uid) = v.selected_uid() {
                    status = reset_cell(&mut v.ed, uid, v.col)?;
                }
            }
            Milestone => {
                if let Some(uid) = v.selected_uid() {
                    v.ed.toggle_milestone(uid)?;
                }
            }
            ManuallySchedule | AutoSchedule => {
                // Nothing on the entry row: there is no task to switch.
                if let Some(uid) = v.selected_uid() {
                    v.ed.set_manual(uid, act == ManuallySchedule)?;
                }
            }
            NewTasksMode => {
                let manual = !v.ed.project().new_tasks_are_manual;
                v.ed.set_new_tasks_manual(manual);
                status = Some(format!("New tasks: {}", task_mode_name(manual)));
            }
            Indent | Outdent => {} // shared no-op-at-limit policy below
            AddLink => {
                // Over a range: link the shown tasks finish-to-start in row
                // order (blank rows are not linked, a pair already linked is
                // left alone) as one undo step; a link that would cycle (or
                // cross a summary's outline) cancels the whole batch, naming
                // the pair. Otherwise the predecessor prompt opens, as for a
                // single task. The selection survives both.
                if let Some(sel) = v.selection().filter(|s| s.uids.len() >= 2) {
                    let linked = v.ed.batch(|ed| {
                        let tasks: Vec<i32> = sel
                            .uids
                            .iter()
                            .copied()
                            .filter(|&uid| ed.project().task(uid).is_some_and(|t| !t.is_null))
                            .collect();
                        let mut n = 0;
                        for w in tasks.windows(2) {
                            let (a, b) = (w[0], w[1]);
                            let already = ed
                                .project()
                                .task(b)
                                .is_some_and(|t| t.predecessors.iter().any(|p| p.uid == a));
                            if !already {
                                ed.add_link(b, projcore::Predecessor::fs(a))
                                    .map_err(|e| cell_error(ed, Some(b), COL_PREDECESSORS, e))?;
                                n += 1;
                            }
                        }
                        Ok(n)
                    });
                    status = Some(match linked {
                        Ok(0) => "No links to add".into(),
                        Ok(1) => "Added 1 link".into(),
                        Ok(n) => format!("Added {n} links"),
                        Err(e) => e,
                    });
                } else {
                    v.open_prompt(PromptKind::Predecessor);
                }
            }
            UnlinkTasks => {
                // Over a range: every selected task loses its links in one
                // undo step; the single-task wording and depth are unchanged.
                if let Some(sel) = v.selection().filter(|s| s.uids.len() >= 2) {
                    let removed = v.ed.batch(|ed| {
                        let mut n = 0;
                        for uid in &sel.uids {
                            n += ed.unlink_task(*uid)?;
                        }
                        Ok(n)
                    });
                    status = Some(match removed {
                        Ok(0) => "No links to remove".into(),
                        Ok(1) => "Removed 1 link".into(),
                        Ok(n) => format!("Removed {n} links"),
                        Err(e) => e,
                    });
                } else if let Some(uid) = v.selected_uid() {
                    status = Some(match v.ed.unlink_task(uid)? {
                        0 => "No links to remove".into(),
                        1 => "Removed 1 link".into(),
                        n => format!("Removed {n} links"),
                    });
                }
            }
            Inactivate => {
                if let Some(uid) = v.selected_uid() {
                    let activate = selected_task_inactive(v);
                    let count = v.ed.set_active(uid, activate)?;
                    status = Some(match (activate, count) {
                        (true, 1) => "Task activated".into(),
                        (false, 1) => "Task inactivated".into(),
                        (true, n) => format!("Activated {n} tasks"),
                        (false, n) => format!("Inactivated {n} tasks"),
                    });
                }
            }
            MoveTask => v.open_prompt(PromptKind::Move),
            Constraint => v.open_prompt(PromptKind::Constraint),
            Notes => v.open_prompt(PromptKind::Notes),
            Assign => v.open_prompt(PromptKind::Assign),
            Hyperlink => v.open_prompt(PromptKind::Hyperlink),
            Find => v.open_prompt(PromptKind::Find),
            Baseline => {
                if !v.ed.project().tasks.is_empty() {
                    v.ed.set_baseline();
                    status = Some(
                        if v.show_baseline {
                            "Baseline set — baseline bars now show under the current bars"
                        } else {
                            "Baseline set — baseline bars are hidden (Gantt Chart Format › Baseline)"
                        }
                        .into(),
                    );
                }
            }
            ClearBaseline => {
                status = Some(
                    if v.ed.clear_baseline()? {
                        "Baseline cleared"
                    } else {
                        "No baseline to clear"
                    }
                    .into(),
                );
            }
            // Handled above, through the pass.
            Level | LevelAll | ClearLeveling => {}
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
            GoToFinish => v.scroll_to_finish(),
            ShowSubtasks => {
                if let Some(uid) = v.selected_uid() {
                    v.ed.set_collapsed(uid, false)?;
                }
            }
            // On a subtask, its summary collapses and takes the cursor.
            HideSubtasks => {
                if let Some(uid) = v.selected_uid() {
                    v.ed.hide_subtasks(uid)?;
                }
            }
            ScrollToTask => v.scroll_to_task(),
            Timeline => v.timeline = !v.timeline,
            CriticalTasks => v.show_critical = !v.show_critical,
            BaselineBars => v.show_baseline = !v.show_baseline,
            // Window-dependent host actions: the file dialog, the clipboard,
            // a new tab.
            Save | ExportGantt | Copy | Cut | Paste | NewProject => {}
            // App-level View › Window commands (#587): the host dispatches
            // them on the app before a tab ever sees them.
            NewWindow | ArrangeAll => {}
        }
        Ok(())
    })();
    if let Err(e) = result {
        status = Some(e);
    }
    if let Some(status) = status {
        tab.status = status.into();
    }
    if let Some(dialog) = dialog {
        tab.dialogs.push(dialog);
    }
    if matches!(act, Indent | Outdent) {
        indent_project(tab, if act == Indent { 1 } else { -1 });
    }
    // The cursor's row as shown can move without a new selection: Hide
    // Subtasks takes a cursor inside to the summary, and indenting a task
    // under a collapsed summary shows the summary's subtasks above it.
    let moved = match &tab.surface {
        Surface::Project(v) => (v.cursor_row(), v.display_row()) != before,
        _ => false,
    };
    complete_project(
        tab,
        matches!(act, AddTask | InsertBlankRow | FindNext | Undo | Redo) || reveal_clear || moved,
    );
}

/// A click on a summary's outline glyph: show or hide its subtasks. An open
/// cell edit commits first, as on any click away, and a failed commit keeps
/// the outline as it is. The cursor stays unless the rows it was on hide.
pub(crate) fn toggle_project_collapse(tab: &mut DocTab, uid: i32) {
    // A levelling pass asked for first runs first, in the order the user
    // gave them; see [`flush_level_pass`].
    flush_level_pass(tab);
    if !commit_project_cell(tab) {
        return;
    }
    let Surface::Project(v) = &mut tab.surface else {
        return;
    };
    // The glyph's click is a plain click: the range does not survive it, so
    // collapsing cannot park an old anchor that expanding would revive.
    v.anchor = None;
    let before = v.cursor_row();
    if let Err(e) = v.ed.toggle_collapsed(uid) {
        tab.status = e.into();
    }
    let moved = v.cursor_row() != before;
    complete_project(tab, moved);
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
    /// Whether the active tab shows a Project Gantt pane, the context of the
    /// Gantt Chart Format tab. A Project that failed to load has no Gantt.
    pub(crate) fn project_gantt_showing(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.kind == Kind::Project && matches!(t.surface, Surface::Project(_)))
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
        // A pass already asked for runs first, so this act, and a second
        // levelling command above all, sees the plan it left.
        self.flush_project_passes(cx);
        // With a cell editor open, Copy/Cut/Paste edit its buffer instead of
        // the table (#561); committing first would break the edit.
        if matches!(act, ProjectAct::Copy | ProjectAct::Cut | ProjectAct::Paste)
            && self
                .tabs
                .get(self.active)
                .is_some_and(|t| matches!(&t.surface, Surface::Project(v) if v.cell.is_some()))
        {
            self.project_cell_clipboard(act, cx);
            return self.refocus(window, cx);
        }
        if !self.commit_active_project_cell() {
            self.refocus(window, cx);
            return;
        }
        self.project_prompt_cancel();
        // A command acts on the cursor, so the range does not survive it —
        // here, one place, however the act reached the app (keys, the
        // ribbon, the QAT, the row menus). Backstage Save and Save As go
        // straight to save_project, which applies the same rule itself.
        if project_act_clears_range(act)
            && let Some(Surface::Project(v)) =
                self.tabs.get_mut(self.active).map(|t| &mut t.surface)
        {
            v.anchor = None;
        }
        match act {
            ProjectAct::Save => return self.save_project(false, window, cx),
            ProjectAct::NewProject => return self.add_tab(Kind::Project, window, cx),
            // View › Window (#587): app-level commands, handled on the app,
            // not the plan; their refusals reach the status line.
            ProjectAct::NewWindow => match self.new_window(None, window, cx) {
                Ok(_) => self.set_status("Opened a new window"),
                Err(e) => self.set_status(e),
            },
            ProjectAct::ArrangeAll => {
                let n = self.arrange_all(window, cx);
                self.set_status(format!("Arranged {n} windows"));
            }
            // Deferred so the status bar can say Busy: render schedules it.
            ProjectAct::Level | ProjectAct::LevelAll | ProjectAct::ClearLeveling => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    request_level_pass(tab, act);
                }
            }
            ProjectAct::Copy | ProjectAct::Cut | ProjectAct::Paste => {
                self.project_clipboard(act, cx)
            }
            ProjectAct::ExportGantt => {
                let Some(tab) = self.tabs.get(self.active) else {
                    return;
                };
                match export_decision(tab, self.harness) {
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
    /// Run every tab's pending levelling pass now, for a reader or writer of
    /// the plan that must not see it half-asked-for.
    pub(crate) fn flush_project_passes(&mut self, cx: &mut Context<Self>) {
        if flush_level_passes(&mut self.tabs) {
            cx.notify();
        }
    }
    /// Called from render: give each pending levelling pass a callback for
    /// the next frame. The frame being built draws `Busy`; gpui runs the
    /// callback before drawing the next one, which then draws the result.
    /// Scheduling from the command itself would run the pass before the
    /// `Busy` frame was ever drawn.
    pub(crate) fn schedule_project_passes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            let Surface::Project(v) = &mut tab.surface else {
                continue;
            };
            let Some((token, _)) = v.busy else {
                continue;
            };
            if v.scheduled == Some(token) {
                continue;
            }
            v.scheduled = Some(token);
            cx.on_next_frame(window, move |this, _, cx| {
                // Found by token, not index: a tab may close or move first.
                if this.tabs.iter_mut().any(|t| finish_level_pass(t, token)) {
                    cx.notify();
                }
            });
            window.request_animation_frame();
        }
    }
    /// Copy/Cut write the cursor cell to the system clipboard; Paste reads it.
    fn project_clipboard(&mut self, act: ProjectAct, cx: &mut Context<Self>) {
        if act == ProjectAct::Paste {
            let now = self.clipboard_read(cx);
            if let (Some(tab), Some(text)) = (self.tabs.get_mut(self.active), now.text()) {
                paste_project_text(tab, text);
            }
            return;
        }
        let Some(Surface::Project(v)) = self.tabs.get(self.active).map(|t| &t.surface) else {
            return;
        };
        let text = project_copy_text(v);
        self.clipboard_write(text, cx);
        // A sheet pastes its own clipboard first; this copy is newer.
        self.grid_clip = None;
        if act == ProjectAct::Cut {
            project_cut(&mut self.tabs[self.active]);
        }
    }
    /// Copy/Cut/Paste with a cell editor open take the buffer, not the table
    /// (#561). Copy and Cut put the whole buffer on the system clipboard (the
    /// editor has no selection); Cut empties the buffer only when the read-
    /// back clipboard holds the buffer's text — a silently failed write (a
    /// busy OS clipboard) otherwise keeps the buffer and the status says so.
    /// Paste inserts the clipboard's text at the caret. The edit stays open;
    /// the project, its undo stack and its dirty flag are untouched.
    fn project_cell_clipboard(&mut self, act: ProjectAct, cx: &mut Context<Self>) {
        if act == ProjectAct::Paste {
            let now = self.clipboard_read(cx);
            if let (Some(tab), Some(text)) = (self.tabs.get_mut(self.active), now.text())
                && let Surface::Project(v) = &mut tab.surface
                && let Some(cell) = &mut v.cell
            {
                cell.paste(text);
            }
            return;
        }
        let text = self.tabs.get(self.active).and_then(|t| match &t.surface {
            Surface::Project(v) => v.cell.as_ref().map(|c| c.buf.clone()),
            _ => None,
        });
        let Some(text) = text else {
            return;
        };
        self.clipboard_write(text.clone(), cx);
        if act == ProjectAct::Copy {
            // A sheet pastes its own clipboard first; this copy is newer.
            self.grid_clip = None;
            return;
        }
        // Cut: an empty buffer has nothing to lose; otherwise empty it only
        // when the clipboard holds what the write put there.
        if !text.is_empty() && !cell_cut_write_took(&self.clipboard_read(cx), &text) {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.status = "Cut failed: the clipboard is busy".into();
            }
            return;
        }
        // A sheet pastes its own clipboard first; this copy is newer.
        self.grid_clip = None;
        if let Some(Surface::Project(v)) = self.tabs.get_mut(self.active).map(|t| &mut t.surface)
            && let Some(cell) = &mut v.cell
        {
            cell.cut();
        }
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
        // Paging steps by the rows the body shows; until the first layout the
        // probe is absent and the view keeps its default.
        let body_h = self
            .probes
            .borrow()
            .current("project-body")
            .map(|b| f32::from(b.size.height));
        if let (Some(body_h), Surface::Project(v)) = (body_h, &mut tab.surface) {
            v.page_rows = page_rows_for(body_h);
        }
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
                        .child(prompt_label(prompt)),
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
