//! yppxy's ribbon: its command set (`Act`), tab/button data, yellow accent, and
//! dispatch — all rendered/navigated by the shared [`ribboncore`] crate. The
//! wrapper `Ribbon` derefs to `ribboncore::Ribbon<Act>`.

use ratatui::style::Color;
use ribboncore::{Ribbon as CoreRibbon, Seg};
use unicode_width::UnicodeWidthStr;

pub use ribboncore::{Dir, Focus, Hit};

/// A ribbon command. `Todo` entries only report "not implemented yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    // Task
    AddTask,
    DeleteTask,
    Milestone,
    Indent,
    Outdent,
    Rename,
    Duration,
    // Schedule
    AddLink,
    Constraint,
    Baseline,
    Assign,
    ClearResources,
    // View
    ExportGantt,
    ScrollLeft,
    ScrollRight,
    GoToStart,
    ThemeToggle,
    Level,
    // File group (on the Task tab too)
    Save,
    SaveAs,
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
        let tabs = vec!["File", "Task", "Schedule", "View"];
        let tab_groups = vec![
            Vec::new(), // File → backstage
            task_groups(),
            schedule_groups(),
            view_groups(),
        ];
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

fn task_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Tasks",
            width: 24,
            rows: [
                vec![
                    btn("＋ Task", AddTask, "Add a task below (n)"),
                    Seg::Gap("  "),
                    btn("✗ Delete", DeleteTask, "Delete the task (x)"),
                ],
                vec![btn(
                    "◆ Milestone",
                    Milestone,
                    "Toggle milestone (0-day) on the task",
                )],
            ],
        },
        Group {
            title: "Outline",
            width: 20,
            rows: [
                vec![btn("→ Indent", Indent, "Indent — make a subtask (Tab)")],
                vec![btn("← Outdent", Outdent, "Outdent (Shift+Tab)")],
            ],
        },
        Group {
            title: "Edit",
            width: 18,
            rows: [
                vec![btn("✎ Rename", Rename, "Rename the task (Enter)")],
                vec![btn(
                    "⏱ Duration",
                    Duration,
                    "Set duration — 3d / 4h / 2w (d)",
                )],
            ],
        },
        Group {
            title: "File",
            width: 14,
            rows: [
                vec![btn("💾 Save", Save, "Save (Ctrl+S)")],
                vec![btn("Save As…", SaveAs, "Save As")],
            ],
        },
    ]
}

fn schedule_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Dependencies",
            width: 22,
            rows: [
                vec![btn("🔗 Link", AddLink, "Add a predecessor by task ID (p)")],
                vec![btn(
                    "⛓ Constraint",
                    Constraint,
                    "Set a date constraint — SNET/MSO/… (c)",
                )],
            ],
        },
        Group {
            title: "Resources",
            width: 16,
            rows: [
                vec![btn(
                    "👤 Assign",
                    Assign,
                    "Assign a resource to the task (a)",
                )],
                vec![btn(
                    "✗ Clear",
                    ClearResources,
                    "Remove the task's resources",
                )],
            ],
        },
        Group {
            title: "Baseline",
            width: 14,
            rows: [
                vec![btn(
                    "⚑ Baseline",
                    Baseline,
                    "Snapshot the current plan as the baseline (b)",
                )],
                vec![btn(
                    "⟳ Recalc",
                    Todo("Reschedule"),
                    "Recompute (automatic on every edit)",
                )],
            ],
        },
    ]
}

fn view_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Gantt",
            width: 26,
            rows: [
                vec![
                    btn("◀", ScrollLeft, "Scroll the timeline left (h)"),
                    Seg::Gap(" "),
                    btn("▶", ScrollRight, "Scroll the timeline right (l)"),
                    Seg::Gap("  "),
                    btn("⇤ Start", GoToStart, "Scroll to the project start"),
                ],
                vec![btn(
                    "⭳ Export Markdown",
                    ExportGantt,
                    "Export a Markdown/Mermaid Gantt (Ctrl+E)",
                )],
            ],
        },
        Group {
            title: "Window",
            width: 16,
            rows: [
                vec![btn("◐ Theme", ThemeToggle, "Toggle light / dark theme")],
                vec![btn("⚖ Level", Level, "Toggle resource leveling (L)")],
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

    #[test]
    fn every_group_is_wide_enough_for_its_content() {
        for groups in [task_groups(), schedule_groups(), view_groups()] {
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
