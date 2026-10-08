//! yppxy's ribbon: its command set (`Act`), tab/button data, yellow accent, and
//! dispatch — all rendered/navigated by the shared [`ribboncore`] crate. The
//! wrapper `Ribbon` derefs to `ribboncore::Ribbon<Act>`.

use projcore::report::ReportKind;
use ratatui::style::Color;
use ribboncore::{Ribbon as CoreRibbon, Seg};
use unicode_width::UnicodeWidthStr;

pub use ribboncore::{Dir, Focus, Hit};

/// A ribbon command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    // Task
    Indent,
    Outdent,
    AddLink,
    ManuallySchedule,
    AutoSchedule,
    AddTask,
    InsertBlankRow,
    Milestone,
    Constraint,
    Notes,
    Find,
    // Resource
    Assign,
    LevelAll,
    ClearLeveling,
    // Report (#1123)
    /// A View Reports item: the report, written beside the plan.
    Report(ReportKind),
    // Project
    CalculateProject,
    Baseline,
    // View
    ShowSubtasks,
    HideSubtasks,
    // Help (#1021)
    /// Help: the documentation is coming.
    Help,
    /// Contact Support / Feedback: the GitHub new-issue page with this build.
    ContactSupport,
    Feedback,
    /// About yppxy: File › Info, with every build field.
    About,
    /// Show Training / What's New: nothing to show yet; the name is said.
    Todo(&'static str),
}

type Group = ribboncore::Group<Act>;

/// yppxy's ribbon accent — the whole ribbon draws yellow (lookxy cyan, docxy
/// light blue, xlsxy green).
const ACCENT: Color = Color::Yellow;

/// A focusable button; width is the glyph's display width.
fn btn(glyph: &'static str, act: Act, hint: &'static str) -> Seg<Act> {
    ribboncore::btn(glyph, glyph.width(), act, hint)
}

/// yppxy's ribbon — a thin wrapper over the shared core.
pub struct Ribbon(CoreRibbon<Act>);

impl Ribbon {
    pub fn new() -> Ribbon {
        let (tabs, tab_groups) = tabs().into_iter().unzip();
        Ribbon(CoreRibbon::new(tabs, tab_groups, 1, ACCENT))
    }

    /// Whether tab `i` is the bodyless File tab (opens the backstage).
    pub fn tab_is_file(&self, i: usize) -> bool {
        self.0.tab_label(i) == Some("File")
    }
}

impl Default for Ribbon {
    fn default() -> Self {
        Self::new()
    }
}
impl std::ops::Deref for Ribbon {
    type Target = CoreRibbon<Act>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Ribbon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// ---- tab definitions --------------------------------------------------------
//
// Microsoft Project's tabs, groups and command labels, matching the suite's
// `project_ribbon()` (suite/docxy/src/project/commands.rs), so written Project
// instructions ("Project › Schedule › Set Baseline") work in both front ends.
// Each hint starts with Project's screentip ("Indent Task — …"). Only
// Project's commands are here: yppxy's own (rename, duration, delete, export,
// scrolling) stay on their keys (Enter, d, x, Ctrl+E, h/l, Alt+Home).
// Save / Save As live in the File backstage (and Ctrl+S); Theme is the
// tab-strip button (and `T`), like the suite's title-bar button.

/// Every tab with its groups, in ribbon order.
fn tabs() -> Vec<(&'static str, Vec<Group>)> {
    vec![
        ("File", Vec::new()), // File → backstage
        ("Task", task_groups()),
        ("Resource", resource_groups()),
        ("Report", report_groups()),
        ("Project", project_groups()),
        ("View", view_groups()),
        // Help ends the row, as in Project (#1021).
        ("Help", help_groups()),
    ]
}

fn task_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Schedule",
            width: 19,
            rows: [
                vec![
                    btn("→ Indent", Indent, "Indent Task — make a subtask (Tab)"),
                    Seg::Gap("  "),
                    btn(
                        "← Outdent",
                        Outdent,
                        "Outdent Task — move it up a level (Shift+Tab)",
                    ),
                ],
                vec![btn(
                    "🔗 Link Tasks",
                    AddLink,
                    "Link the Selected Tasks — add a predecessor by task ID (p)",
                )],
            ],
        },
        Group {
            title: "Tasks",
            width: 20,
            rows: [
                vec![btn(
                    "📌 Manually Schedule",
                    ManuallySchedule,
                    "Manually Schedule — pin the task at its current dates (m toggles)",
                )],
                vec![btn(
                    "⟳ Auto Schedule",
                    AutoSchedule,
                    "Auto Schedule — let the scheduler place the task by its links (m toggles)",
                )],
            ],
        },
        Group {
            title: "Insert",
            width: 20,
            rows: [
                vec![
                    btn("＋ Task", AddTask, "Task — add a task below (n)"),
                    Seg::Gap("  "),
                    btn(
                        "▭ Blank Row",
                        InsertBlankRow,
                        "Insert Blank Row — a blank row above (N)",
                    ),
                ],
                vec![btn(
                    "◆ Milestone",
                    Milestone,
                    "Insert Milestone — toggle milestone (0-day) on the task",
                )],
            ],
        },
        Group {
            title: "Properties",
            width: 16,
            rows: [
                vec![btn(
                    "ⓘ Information...",
                    Constraint,
                    "View Task Information — set a date constraint, SNET/MSO/… (c)",
                )],
                vec![btn(
                    "📝 Notes...",
                    Notes,
                    "Notes — the task's notes, one line; \\n is a new line, empty removes",
                )],
            ],
        },
        Group {
            title: "Editing",
            width: 9,
            rows: [
                vec![btn(
                    "⌕ Find...",
                    Find,
                    "Find... — a task by name (Ctrl+F / F3 next)",
                )],
                Vec::new(),
            ],
        },
    ]
}

fn resource_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Assignments",
            width: 22,
            rows: [
                vec![btn(
                    "👤 Assign Resources...",
                    Assign,
                    "Assign Resources... — assign a resource to the task; empty clears (a)",
                )],
                Vec::new(),
            ],
        },
        Group {
            title: "Level",
            width: 16,
            rows: [
                vec![btn(
                    "⚖ Level All",
                    LevelAll,
                    "Level All — delay bars to fit resource capacity (L toggles)",
                )],
                vec![btn(
                    "✗ Clear Leveling",
                    ClearLeveling,
                    "Clear Leveling — turn resource leveling off (L toggles)",
                )],
            ],
        },
    ]
}

/// Report › View Reports (#1123). Project draws each of Dashboards,
/// Resources, Costs and In Progress as a menu; a terminal ribbon has no
/// menus, so each is a group of its reports' buttons, and an instruction
/// reads "Report › Costs › Task Cost Overview" as in Project. Only the
/// reports projcore backs are here (see `projcore::report`).
fn report_groups() -> Vec<Group> {
    use ReportKind::*;
    let report = |glyph, kind: ReportKind, what| btn(glyph, Act::Report(kind), what);
    vec![
        Group {
            title: "Dashboards",
            width: 18,
            rows: [
                vec![report(
                    "▦ Project Overview",
                    ProjectOverview,
                    "Project Overview — dates, progress, milestones due and late tasks, as Markdown beside the plan",
                )],
                Vec::new(),
            ],
        },
        Group {
            title: "Resources",
            width: 25,
            rows: [
                vec![report(
                    "◉ Resource Overview",
                    ResourceOverview,
                    "Resource Overview — each resource's work, as Markdown beside the plan",
                )],
                vec![report(
                    "▲ Overallocated Resources",
                    OverallocatedResources,
                    "Overallocated Resources — resources booked past their units, as Markdown beside the plan",
                )],
            ],
        },
        Group {
            title: "Costs",
            width: 24,
            rows: [
                vec![report(
                    "$ Task Cost Overview",
                    TaskCostOverview,
                    "Task Cost Overview — each task's costs and the total, as Markdown beside the plan",
                )],
                vec![report(
                    "$ Resource Cost Overview",
                    ResourceCostOverview,
                    "Resource Cost Overview — each resource's costs and the total, as Markdown beside the plan",
                )],
            ],
        },
        Group {
            title: "In Progress",
            width: 30,
            rows: [
                vec![
                    report(
                        "★ Critical Tasks",
                        CriticalTasks,
                        "Critical Tasks — critical tasks not yet complete, as Markdown beside the plan",
                    ),
                    Seg::Gap("  "),
                    report(
                        "◷ Late Tasks",
                        LateTasks,
                        "Late Tasks — tasks late at the status date, as Markdown beside the plan",
                    ),
                ],
                vec![report(
                    "◆ Milestone Report",
                    MilestoneReport,
                    "Milestone Report — milestones with their status, as Markdown beside the plan",
                )],
            ],
        },
    ]
}

fn project_groups() -> Vec<Group> {
    use Act::*;
    vec![Group {
        title: "Schedule",
        width: 19,
        rows: [
            vec![btn(
                "⟳ Calculate Project",
                CalculateProject,
                "Calculate Project — recompute the schedule (automatic on every edit)",
            )],
            vec![btn(
                "⚑ Set Baseline",
                Baseline,
                "Set Baseline — snapshot the current plan as the baseline (b)",
            )],
        ],
    }]
}

fn view_groups() -> Vec<Group> {
    use Act::*;
    vec![Group {
        title: "Data",
        width: 16,
        rows: [
            vec![btn(
                "▾ Show Subtasks",
                ShowSubtasks,
                "Show Subtasks — the selected summary's subtasks (+)",
            )],
            vec![btn(
                "▸ Hide Subtasks",
                HideSubtasks,
                "Hide Subtasks — the selected summary's subtasks (-)",
            )],
        ],
    }]
}

/// The Help tab every editor ends with (#1021). Show Training and What's New
/// have nothing to show yet and say so.
fn help_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Help",
            width: 35,
            rows: [
                vec![
                    btn("? Help", Help, "Help — the documentation is coming"),
                    Seg::Gap("  "),
                    btn(
                        "✉ Feedback",
                        Feedback,
                        "Feedback — a GitHub issue with this build filled in",
                    ),
                    Seg::Gap("  "),
                    btn(
                        "▷ Show Training",
                        Todo("Show Training"),
                        "Show Training — coming later",
                    ),
                ],
                vec![
                    btn(
                        "☎ Contact Support",
                        ContactSupport,
                        "Contact Support — a GitHub issue with this build filled in",
                    ),
                    Seg::Gap("  "),
                    btn(
                        "✦ What's New",
                        Todo("What's New"),
                        "What's New — coming later",
                    ),
                ],
            ],
        },
        Group {
            title: "About",
            width: 13,
            rows: [
                vec![btn(
                    "ⓘ About yppxy",
                    About,
                    "About yppxy — this build's details",
                )],
                vec![],
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ribboncore::Seg;

    fn content_w(row: &[Seg<Act>]) -> usize {
        row.iter()
            .map(|s| match s {
                Seg::Gap(g) => g.width(),
                Seg::Btn(b) => b.width,
            })
            .sum()
    }

    /// The widest tab body: the Task tab (1 + Σ(width + 3)). Project's
    /// Task › Tasks › Manually/Auto Schedule path (#121) took it past the
    /// old 89-column budget; no layout keeps Project's names and groups in
    /// 89. A narrower terminal clips the body on the right (the body is a
    /// non-wrapping Paragraph); the clipped buttons keep their keys. Raise
    /// this only deliberately: #158's Task › Insert › Blank Row took it from
    /// 109 to 118.
    const BODY_BUDGET: usize = 118;

    #[test]
    fn tabs_are_microsoft_projects() {
        let r = Ribbon::new();
        let labels: Vec<_> = (0..).map_while(|i| r.tab_label(i)).collect();
        assert_eq!(
            labels,
            [
                "File", "Task", "Resource", "Report", "Project", "View", "Help"
            ]
        );
    }

    #[test]
    fn project_instruction_paths_exist() {
        use Act::*;
        // (tab, group, Project's label, Project's screentip, act) — the
        // suite's `project_ribbon()`.
        let paths = [
            ("Task", "Schedule", "Indent", "Indent Task", Indent),
            ("Task", "Schedule", "Outdent", "Outdent Task", Outdent),
            (
                "Task",
                "Schedule",
                "Link Tasks",
                "Link the Selected Tasks",
                AddLink,
            ),
            (
                "Task",
                "Tasks",
                "Manually Schedule",
                "Manually Schedule",
                ManuallySchedule,
            ),
            (
                "Task",
                "Tasks",
                "Auto Schedule",
                "Auto Schedule",
                AutoSchedule,
            ),
            ("Task", "Insert", "Task", "Task", AddTask),
            (
                "Task",
                "Insert",
                "Blank Row",
                "Insert Blank Row",
                InsertBlankRow,
            ),
            ("Task", "Insert", "Milestone", "Insert Milestone", Milestone),
            (
                "Task",
                "Properties",
                "Information...",
                "View Task Information",
                Constraint,
            ),
            ("Task", "Properties", "Notes...", "Notes", Notes),
            ("Task", "Editing", "Find...", "Find...", Find),
            (
                "Resource",
                "Assignments",
                "Assign Resources...",
                "Assign Resources...",
                Assign,
            ),
            ("Resource", "Level", "Level All", "Level All", LevelAll),
            (
                "Resource",
                "Level",
                "Clear Leveling",
                "Clear Leveling",
                ClearLeveling,
            ),
            (
                "Project",
                "Schedule",
                "Calculate Project",
                "Calculate Project",
                CalculateProject,
            ),
            (
                "Project",
                "Schedule",
                "Set Baseline",
                "Set Baseline",
                Baseline,
            ),
            (
                "View",
                "Data",
                "Show Subtasks",
                "Show Subtasks",
                ShowSubtasks,
            ),
            (
                "View",
                "Data",
                "Hide Subtasks",
                "Hide Subtasks",
                HideSubtasks,
            ),
            // The Help tab every editor ends with (#1021).
            ("Help", "Help", "Help", "Help", Help),
            ("Help", "Help", "Feedback", "Feedback", Feedback),
            (
                "Help",
                "Help",
                "Show Training",
                "Show Training",
                Todo("Show Training"),
            ),
            (
                "Help",
                "Help",
                "Contact Support",
                "Contact Support",
                ContactSupport,
            ),
            (
                "Help",
                "Help",
                "What's New",
                "What's New",
                Todo("What's New"),
            ),
            ("Help", "About", "About yppxy", "About yppxy", About),
            // Report › View Reports' menus as groups (#1123).
            (
                "Report",
                "Dashboards",
                "Project Overview",
                "Project Overview",
                Report(ReportKind::ProjectOverview),
            ),
            (
                "Report",
                "Resources",
                "Resource Overview",
                "Resource Overview",
                Report(ReportKind::ResourceOverview),
            ),
            (
                "Report",
                "Resources",
                "Overallocated Resources",
                "Overallocated Resources",
                Report(ReportKind::OverallocatedResources),
            ),
            (
                "Report",
                "Costs",
                "Task Cost Overview",
                "Task Cost Overview",
                Report(ReportKind::TaskCostOverview),
            ),
            (
                "Report",
                "Costs",
                "Resource Cost Overview",
                "Resource Cost Overview",
                Report(ReportKind::ResourceCostOverview),
            ),
            (
                "Report",
                "In Progress",
                "Critical Tasks",
                "Critical Tasks",
                Report(ReportKind::CriticalTasks),
            ),
            (
                "Report",
                "In Progress",
                "Late Tasks",
                "Late Tasks",
                Report(ReportKind::LateTasks),
            ),
            (
                "Report",
                "In Progress",
                "Milestone Report",
                "Milestone Report",
                Report(ReportKind::MilestoneReport),
            ),
        ];
        let tabs = tabs();
        for (tab, group, name, tip, act) in paths {
            let groups = &tabs.iter().find(|(t, _)| *t == tab).unwrap().1;
            let g = groups
                .iter()
                .find(|g| g.title == group)
                .unwrap_or_else(|| panic!("no group {tab} › {group}"));
            let found = g.rows.iter().flatten().find_map(|s| match s {
                Seg::Btn(b) if b.glyph.ends_with(&format!(" {name}")) && b.act == act => Some(b),
                _ => None,
            });
            let b = found.unwrap_or_else(|| panic!("no {tab} › {group} › {name} → {act:?}"));
            assert!(
                b.hint == tip || b.hint.starts_with(&format!("{tip} — ")),
                "{tab} › {group} › {name}: hint {:?} does not start with {tip:?}",
                b.hint
            );
        }
        // Nothing else: exactly these groups per tab, and no stray buttons.
        for (tab, groups) in &tabs {
            let want: Vec<_> =
                paths
                    .iter()
                    .filter(|p| p.0 == *tab)
                    .map(|p| p.1)
                    .fold(Vec::new(), |mut v, g| {
                        if !v.contains(&g) {
                            v.push(g);
                        }
                        v
                    });
            let got: Vec<_> = groups.iter().map(|g| g.title).collect();
            assert_eq!(got, want, "groups on {tab}");
            let buttons = groups
                .iter()
                .flat_map(|g| g.rows.iter().flatten())
                .filter(|s| matches!(s, Seg::Btn(_)))
                .count();
            let rows = paths.iter().filter(|p| p.0 == *tab).count();
            assert_eq!(buttons, rows, "buttons on {tab}");
        }
        // Every View Report is on the Report tab, once (#1123).
        let reports: Vec<_> = tabs
            .iter()
            .filter(|(t, _)| *t == "Report")
            .flat_map(|(_, g)| g.iter().flat_map(|g| g.rows.iter().flatten()))
            .filter_map(|s| match s {
                Seg::Btn(b) => Some(b.act),
                _ => None,
            })
            .collect();
        assert_eq!(reports, ReportKind::ALL.map(Act::Report));
    }

    #[test]
    fn every_group_is_wide_enough_for_its_content() {
        for (_, groups) in tabs() {
            for g in &groups {
                assert!(
                    g.width >= g.title.chars().count(),
                    "group {:?} width {} < its title",
                    g.title,
                    g.width
                );
                for row in &g.rows {
                    assert!(
                        g.width >= content_w(row),
                        "group {:?} width {} < content {}",
                        g.title,
                        g.width,
                        content_w(row)
                    );
                }
            }
        }
    }

    #[test]
    fn every_tab_fits_the_width_budget() {
        for (tab, groups) in tabs() {
            let body = 1 + groups.iter().map(|g| g.width + 3).sum::<usize>();
            assert!(body <= BODY_BUDGET, "{tab} body {body} > {BODY_BUDGET}");
        }
    }

    #[test]
    fn constructs_hits_and_navigates() {
        let r = Ribbon::new();
        assert!(r.tab_is_file(0));
        assert!(!r.tab_is_file(1));
        assert!(r.button_count() > 0);
        assert!(matches!(r.hit(2, 0, false), Hit::Tab(0)));
        let f = r.nav(Focus::Tab(1), Dir::Down);
        assert!(matches!(f, Focus::Button(_)));
        assert!(matches!(r.nav(f, Dir::Up), Focus::Tab(1)));
    }
}
