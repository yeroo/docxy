//! yppxy's ribbon: its command set (`Act`), tab/button data, yellow accent, and
//! dispatch — all rendered/navigated by the shared [`ribboncore`] crate. The
//! wrapper `Ribbon` derefs to `ribboncore::Ribbon<Act>`.

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
    Milestone,
    Constraint,
    Rename,
    Duration,
    Find,
    DeleteTask,
    // Resource
    Assign,
    ClearResources,
    LevelAll,
    ClearLeveling,
    // Report
    ExportGantt,
    // Project
    CalculateProject,
    Baseline,
    // View
    ScrollLeft,
    ScrollRight,
    GoToStart,
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
// Microsoft Project's tabs, groups and command names, matching the suite's
// `project_ribbon()` (suite/docxy/src/project/commands.rs), so written Project
// instructions ("Project › Schedule › Set Baseline") work in both front ends.
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
    ]
}

fn task_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Schedule",
            width: 29,
            rows: [
                vec![
                    btn("→ Indent Task", Indent, "Indent — make a subtask (Tab)"),
                    Seg::Gap("  "),
                    btn("← Outdent Task", Outdent, "Outdent (Shift+Tab)"),
                ],
                vec![btn(
                    "🔗 Link the Selected Tasks",
                    AddLink,
                    "Add a predecessor by task ID (p)",
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
                    "Pin the task at its current dates (m toggles)",
                )],
                vec![btn(
                    "⟳ Auto Schedule",
                    AutoSchedule,
                    "Let the scheduler place the task by its links (m toggles)",
                )],
            ],
        },
        Group {
            title: "Insert",
            width: 11,
            rows: [
                vec![btn("＋ Task", AddTask, "Add a task below (n)")],
                vec![btn(
                    "◆ Milestone",
                    Milestone,
                    "Toggle milestone (0-day) on the task",
                )],
            ],
        },
        Group {
            title: "Properties",
            width: 20,
            rows: [
                vec![btn(
                    "ⓘ Information",
                    Constraint,
                    "Set a date constraint — SNET/MSO/… (c)",
                )],
                vec![
                    btn("✎ Rename", Rename, "Rename the task (Enter)"),
                    Seg::Gap("  "),
                    btn("⏱ Duration", Duration, "Set duration — 3d / 4h / 2w (d)"),
                ],
            ],
        },
        Group {
            title: "Editing",
            width: 13,
            rows: [
                vec![btn(
                    "⌕ Find",
                    Find,
                    "Find a task by name (Ctrl+F / F3 next)",
                )],
                vec![btn("✗ Delete Task", DeleteTask, "Delete the task (x)")],
            ],
        },
    ]
}

fn resource_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Assignments",
            width: 19,
            rows: [
                vec![btn(
                    "👤 Assign Resources",
                    Assign,
                    "Assign a resource to the task (a)",
                )],
                vec![btn(
                    "✗ Clear Resources",
                    ClearResources,
                    "Remove the task's resources",
                )],
            ],
        },
        Group {
            title: "Level",
            width: 16,
            rows: [
                vec![btn(
                    "⚖ Level All",
                    LevelAll,
                    "Delay bars to fit resource capacity (L toggles)",
                )],
                vec![btn(
                    "✗ Clear Leveling",
                    ClearLeveling,
                    "Turn resource leveling off (L toggles)",
                )],
            ],
        },
    ]
}

fn report_groups() -> Vec<Group> {
    use Act::*;
    vec![Group {
        title: "Export",
        width: 14,
        rows: [
            vec![btn(
                "⭳ Export Gantt",
                ExportGantt,
                "Export a Markdown/Mermaid Gantt (Ctrl+E)",
            )],
            Vec::new(),
        ],
    }]
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
                "Recompute the schedule (automatic on every edit)",
            )],
            vec![btn(
                "⚑ Set Baseline",
                Baseline,
                "Snapshot the current plan as the baseline (b)",
            )],
        ],
    }]
}

fn view_groups() -> Vec<Group> {
    use Act::*;
    vec![Group {
        title: "Zoom",
        width: 29,
        rows: [
            vec![
                btn("◀ Scroll Left", ScrollLeft, "Scroll the timeline left (h)"),
                Seg::Gap("  "),
                btn(
                    "▶ Scroll Right",
                    ScrollRight,
                    "Scroll the timeline right (l)",
                ),
            ],
            vec![btn(
                "⇤ Go to Start",
                GoToStart,
                "Scroll to the earliest task or project start",
            )],
        ],
    }]
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
    /// this only deliberately.
    const BODY_BUDGET: usize = 109;

    #[test]
    fn tabs_are_microsoft_projects() {
        let r = Ribbon::new();
        let labels: Vec<_> = (0..).map_while(|i| r.tab_label(i)).collect();
        assert_eq!(
            labels,
            ["File", "Task", "Resource", "Report", "Project", "View"]
        );
    }

    #[test]
    fn project_instruction_paths_exist() {
        use Act::*;
        // (tab, group, command name, act) — the suite's `project_ribbon()`.
        let paths = [
            ("Task", "Schedule", "Indent Task", Indent),
            ("Task", "Schedule", "Outdent Task", Outdent),
            ("Task", "Schedule", "Link the Selected Tasks", AddLink),
            ("Task", "Tasks", "Manually Schedule", ManuallySchedule),
            ("Task", "Tasks", "Auto Schedule", AutoSchedule),
            ("Task", "Insert", "Task", AddTask),
            ("Task", "Insert", "Milestone", Milestone),
            ("Task", "Properties", "Information", Constraint),
            ("Task", "Properties", "Rename", Rename),
            ("Task", "Properties", "Duration", Duration),
            ("Task", "Editing", "Find", Find),
            ("Task", "Editing", "Delete Task", DeleteTask),
            ("Resource", "Assignments", "Assign Resources", Assign),
            ("Resource", "Assignments", "Clear Resources", ClearResources),
            ("Resource", "Level", "Level All", LevelAll),
            ("Resource", "Level", "Clear Leveling", ClearLeveling),
            ("Report", "Export", "Export Gantt", ExportGantt),
            ("Project", "Schedule", "Calculate Project", CalculateProject),
            ("Project", "Schedule", "Set Baseline", Baseline),
            ("View", "Zoom", "Scroll Left", ScrollLeft),
            ("View", "Zoom", "Scroll Right", ScrollRight),
            ("View", "Zoom", "Go to Start", GoToStart),
        ];
        let tabs = tabs();
        for (tab, group, name, act) in paths {
            let groups = &tabs.iter().find(|(t, _)| *t == tab).unwrap().1;
            let g = groups
                .iter()
                .find(|g| g.title == group)
                .unwrap_or_else(|| panic!("no group {tab} › {group}"));
            let found = g.rows.iter().flatten().any(|s| match s {
                Seg::Btn(b) => b.glyph.ends_with(&format!(" {name}")) && b.act == act,
                Seg::Gap(_) => false,
            });
            assert!(found, "no {tab} › {group} › {name} → {act:?}");
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
    }

    #[test]
    fn every_group_is_wide_enough_for_its_content() {
        for (_, groups) in tabs() {
            for g in &groups {
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
