//! A Word/Excel-style tabbed ribbon, the same interaction model as docxy and
//! xlsxy, retargeted to project scheduling. Tab headers show on one line; F9
//! (or a click) engages it, Down enters the buttons, arrows move, Enter applies,
//! Esc leaves. The File tab is bodyless — it opens the backstage.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// A ribbon command. `Todo` entries render dimmed and report "not implemented
/// yet" until wired up.
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

impl Act {
    fn enabled(self) -> bool {
        !matches!(self, Act::Todo(_))
    }
}

struct Button {
    glyph: &'static str,
    width: usize,
    act: Act,
    hint: &'static str,
}

enum Seg {
    Btn(Button),
    Gap(&'static str),
}

fn btn(glyph: &'static str, width: usize, act: Act, hint: &'static str) -> Seg {
    Seg::Btn(Button { glyph, width, act, hint })
}

struct Group {
    title: &'static str,
    width: usize,
    rows: [Vec<Seg>; 2],
}

#[derive(Clone, Copy)]
struct Placed {
    row: u8,
    x: u16,
    w: u16,
    act: Act,
    hint: &'static str,
}

/// Keyboard focus within the ribbon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    None,
    Tab(usize),
    Button(usize),
}

/// Result of a mouse click on the ribbon.
pub enum Hit {
    Tab(usize),
    Button(Act),
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

pub struct Ribbon {
    tabs: Vec<&'static str>,
    active: usize,
    tab_groups: Vec<Vec<Group>>,
    placed: Vec<Placed>,
    tab_cols: Vec<(u16, u16)>,
    width: u16,
    active_toggles: Vec<Act>,
}

const ROW0: usize = 1;
const ROW1: usize = 2;

impl Ribbon {
    pub fn new() -> Ribbon {
        let mut r = Ribbon {
            tabs: vec!["File", "Task", "Schedule", "View"],
            active: 1,
            tab_groups: vec![Vec::new(), task_groups(), schedule_groups(), view_groups()],
            placed: Vec::new(),
            tab_cols: Vec::new(),
            width: 0,
            active_toggles: Vec::new(),
        };
        r.layout();
        r
    }

    /// Whether tab `i` is the bodyless File tab (opens the backstage).
    pub fn tab_is_file(&self, i: usize) -> bool {
        self.tabs.get(i) == Some(&"File")
    }

    pub fn set_toggles(&mut self, acts: Vec<Act>) {
        self.active_toggles = acts;
    }

    fn groups(&self) -> &[Group] {
        &self.tab_groups[self.active]
    }

    pub fn active_tab(&self) -> usize {
        self.active
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn tab_label(&self, i: usize) -> Option<&'static str> {
        self.tabs.get(i).copied()
    }

    pub fn set_active(&mut self, i: usize) {
        if i < self.tabs.len() && !self.tab_groups[i].is_empty() {
            self.active = i;
            self.layout();
        }
    }

    // ---- layout ----

    fn layout(&mut self) {
        self.placed.clear();
        let mut gx = 1u16;
        let active = self.active;
        for g in &self.tab_groups[active] {
            for (ri, row) in g.rows.iter().enumerate() {
                let mut x = gx + 1;
                for seg in row {
                    match seg {
                        Seg::Gap(s) => x += s.chars().count() as u16,
                        Seg::Btn(b) => {
                            self.placed.push(Placed {
                                row: ri as u8,
                                x,
                                w: b.width as u16,
                                act: b.act,
                                hint: b.hint,
                            });
                            x += b.width as u16;
                        }
                    }
                }
            }
            gx += g.width as u16 + 3;
        }
        self.width = gx;
        self.tab_cols.clear();
        let mut tx = 2u16;
        for t in &self.tabs {
            let w = t.chars().count() as u16;
            self.tab_cols.push((tx, tx + w));
            tx += w + 3;
        }
    }

    // ---- mouse ----

    /// Hit-test a click. `y` is the row within the ribbon area (0 = tab strip).
    pub fn hit(&self, x: u16, y: u16, expanded: bool) -> Hit {
        if y == 0 {
            for (i, &(a, b)) in self.tab_cols.iter().enumerate() {
                if x >= a && x < b {
                    return Hit::Tab(i);
                }
            }
            return Hit::Outside;
        }
        if expanded {
            let brow = match y as usize {
                n if n == ROW0 + 1 => Some(0u8),
                n if n == ROW1 + 1 => Some(1u8),
                _ => None,
            };
            if let Some(rr) = brow {
                for p in &self.placed {
                    if p.row == rr && x >= p.x && x < p.x + p.w {
                        return Hit::Button(p.act);
                    }
                }
            }
        }
        Hit::Outside
    }

    // ---- keyboard nav ----

    pub fn enter_body(&self) -> Focus {
        self.placed
            .iter()
            .position(|p| p.row == 0)
            .map(Focus::Button)
            .unwrap_or(Focus::Tab(self.active))
    }

    pub fn focus_act(&self, f: Focus) -> Option<(Act, &'static str)> {
        match f {
            Focus::Button(i) => self.placed.get(i).map(|p| (p.act, p.hint)),
            _ => None,
        }
    }

    pub fn nav(&self, f: Focus, dir: Dir) -> Focus {
        match f {
            Focus::Tab(t) => match dir {
                Dir::Left => Focus::Tab(t.saturating_sub(1)),
                Dir::Right => Focus::Tab((t + 1).min(self.tabs.len() - 1)),
                Dir::Down => self.enter_body(),
                Dir::Up => Focus::Tab(t),
            },
            Focus::Button(i) => {
                let Some(cur) = self.placed.get(i) else {
                    return Focus::Tab(self.active);
                };
                match dir {
                    Dir::Left | Dir::Right => self
                        .nearest_in_row(cur.row, cur.x, dir == Dir::Right, i)
                        .map(Focus::Button)
                        .unwrap_or(Focus::Button(i)),
                    Dir::Down => self
                        .nearest_in_row_byx(1, cur.x)
                        .map(Focus::Button)
                        .unwrap_or(Focus::Button(i)),
                    Dir::Up => {
                        if cur.row == 0 {
                            Focus::Tab(self.active)
                        } else {
                            self.nearest_in_row_byx(0, cur.x)
                                .map(Focus::Button)
                                .unwrap_or(Focus::Button(i))
                        }
                    }
                }
            }
            Focus::None => Focus::Tab(self.active),
        }
    }

    fn nearest_in_row(&self, row: u8, x: u16, right: bool, skip: usize) -> Option<usize> {
        self.placed
            .iter()
            .enumerate()
            .filter(|(j, p)| *j != skip && p.row == row && if right { p.x > x } else { p.x < x })
            .min_by_key(|(_, p)| p.x.abs_diff(x))
            .map(|(j, _)| j)
    }

    fn nearest_in_row_byx(&self, row: u8, x: u16) -> Option<usize> {
        self.placed
            .iter()
            .enumerate()
            .filter(|(_, p)| p.row == row)
            .min_by_key(|(_, p)| p.x.abs_diff(x))
            .map(|(j, _)| j)
    }

    // ---- rendering ----

    pub fn render_tabs(&self, focus: Focus) -> Line<'static> {
        let engaged = focus != Focus::None;
        let focused_tab = if let Focus::Tab(t) = focus { Some(t) } else { None };
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if !engaged {
                Style::default().add_modifier(Modifier::DIM)
            } else if i == self.active {
                Style::default().fg(Color::Black).bg(Color::White)
            } else if Some(i) == focused_tab {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled("· F9 ribbon".to_string(), Style::default().add_modifier(Modifier::DIM)));
        Line::from(spans)
    }

    pub fn render_body(&self, focus: Focus) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let widths: Vec<usize> = self.groups().iter().map(|g| g.width).collect();
        let bar = |l: &str, m: &str, r: &str| -> Line<'static> {
            let mut s = String::from(l);
            for (i, w) in widths.iter().enumerate() {
                if i > 0 {
                    s.push_str(m);
                }
                s.push_str(&"─".repeat(w + 2));
            }
            s.push_str(r);
            Line::styled(s, dim)
        };
        let focused = if let Focus::Button(i) = focus { self.placed.get(i).copied() } else { None };
        let row_w = |row: &[Seg]| -> usize {
            row.iter()
                .map(|s| match s {
                    Seg::Gap(g) => g.chars().count(),
                    Seg::Btn(b) => b.width,
                })
                .sum()
        };
        let mut out = vec![bar("┌", "┬", "┐")];
        for ri in 0..2 {
            let mut spans = vec![Span::styled("│", dim)];
            for g in self.groups() {
                spans.push(Span::raw(" "));
                self.row_spans(&g.rows[ri], ri as u8, focused, &mut spans);
                let pad = g.width.saturating_sub(row_w(&g.rows[ri]));
                spans.push(Span::raw(" ".repeat(pad + 1)));
                spans.push(Span::styled("│", dim));
            }
            out.push(Line::from(spans));
        }
        out.push(bar("├", "┼", "┤"));
        let mut spans = vec![Span::styled("│", dim)];
        for g in self.groups() {
            let pad = g.width.saturating_sub(g.title.chars().count());
            let l = pad / 2;
            spans.push(Span::raw(format!(" {}{}{} ", " ".repeat(l), g.title, " ".repeat(pad - l))));
            spans.push(Span::styled("│", dim));
        }
        out.push(Line::from(spans));
        out
    }

    fn row_spans(&self, row: &[Seg], rr: u8, focused: Option<Placed>, out: &mut Vec<Span<'static>>) {
        for seg in row {
            match seg {
                Seg::Gap(s) => out.push(Span::raw(s.to_string())),
                Seg::Btn(b) => {
                    let is_focus = focused
                        .map(|p| p.row == rr && p.act == b.act && p.hint == b.hint)
                        .unwrap_or(false);
                    let is_on = self.active_toggles.contains(&b.act);
                    let style = if is_focus {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else if is_on {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if b.act.enabled() {
                        Style::default()
                    } else {
                        Style::default().add_modifier(Modifier::DIM)
                    };
                    out.push(Span::styled(b.glyph.to_string(), style));
                }
            }
        }
    }

    pub fn render_hint(&self, focus: Focus, total_width: u16) -> Line<'static> {
        let style = Style::default().fg(Color::Black).bg(Color::Yellow);
        let text = match focus {
            Focus::Button(i) => {
                let p = self.placed.get(i);
                let enabled = p.map(|p| p.act.enabled()).unwrap_or(true);
                let h = p.map(|p| p.hint).unwrap_or("");
                if enabled {
                    format!(" {h}")
                } else {
                    format!(" {h} — not implemented yet")
                }
            }
            _ => " ←→ tabs · ↓ enter · arrows move · Enter apply · Esc leave".to_string(),
        };
        let w = total_width as usize;
        let padded = if text.chars().count() >= w {
            text.chars().take(w).collect()
        } else {
            format!("{text}{}", " ".repeat(w - text.chars().count()))
        };
        Line::styled(padded, style)
    }
}

impl Default for Ribbon {
    fn default() -> Self {
        Ribbon::new()
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
                    btn("＋ Task", 6, AddTask, "Add a task below (n)"),
                    Seg::Gap("  "),
                    btn("✗ Delete", 8, DeleteTask, "Delete the task (x)"),
                ],
                vec![btn("◆ Milestone", 11, Milestone, "Toggle milestone (0-day) on the task")],
            ],
        },
        Group {
            title: "Outline",
            width: 20,
            rows: [
                vec![btn("→ Indent", 8, Indent, "Indent — make a subtask (Tab)")],
                vec![btn("← Outdent", 9, Outdent, "Outdent (Shift+Tab)")],
            ],
        },
        Group {
            title: "Edit",
            width: 18,
            rows: [
                vec![btn("✎ Rename", 8, Rename, "Rename the task (Enter)")],
                vec![btn("⏱ Duration", 10, Duration, "Set duration — 3d / 4h / 2w (d)")],
            ],
        },
        Group {
            title: "File",
            width: 14,
            rows: [
                vec![btn("💾 Save", 6, Save, "Save (Ctrl+S)")],
                vec![btn("Save As…", 8, SaveAs, "Save As")],
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
                vec![btn("🔗 Link", 6, AddLink, "Add a predecessor by task ID (p)")],
                vec![btn("⛓ Constraint", 12, Constraint, "Set a date constraint — SNET/MSO/… (c)")],
            ],
        },
        Group {
            title: "Resources",
            width: 16,
            rows: [
                vec![btn("👤 Assign", 8, Assign, "Assign a resource to the task (a)")],
                vec![btn("✗ Clear", 7, ClearResources, "Remove the task's resources")],
            ],
        },
        Group {
            title: "Baseline",
            width: 14,
            rows: [
                vec![btn("⚑ Baseline", 10, Baseline, "Snapshot the current plan as the baseline (b)")],
                vec![btn("⟳ Recalc", 8, Todo("Reschedule"), "Recompute (automatic on every edit)")],
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
                    btn("◀", 1, ScrollLeft, "Scroll the timeline left (h)"),
                    Seg::Gap(" "),
                    btn("▶", 1, ScrollRight, "Scroll the timeline right (l)"),
                    Seg::Gap("  "),
                    btn("⇤ Start", 7, GoToStart, "Scroll to the project start"),
                ],
                vec![btn("⭳ Export Markdown", 17, ExportGantt, "Export a Markdown/Mermaid Gantt (Ctrl+E)")],
            ],
        },
        Group {
            title: "Window",
            width: 16,
            rows: [
                vec![btn("◐ Theme", 7, ThemeToggle, "Toggle light / dark theme")],
                vec![btn("⚖ Level", 7, Level, "Toggle resource leveling (L)")],
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_fit_their_declared_width() {
        let mut r = Ribbon::new();
        for tab in 0..r.tabs.len() {
            if r.tab_groups[tab].is_empty() {
                continue;
            }
            r.set_active(tab);
            for g in r.groups() {
                let widest = g
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|seg| match seg {
                                Seg::Gap(s) => s.chars().count(),
                                Seg::Btn(b) => b.width,
                            })
                            .sum::<usize>()
                    })
                    .max()
                    .unwrap_or(0);
                assert!(g.width >= widest, "group {:?} width too small", g.title);
            }
        }
    }

    #[test]
    fn task_tab_exposes_core_actions() {
        let mut r = Ribbon::new();
        let task = (0..r.tabs.len()).find(|&i| r.tab_label(i) == Some("Task")).unwrap();
        r.set_active(task);
        let acts: Vec<Act> = r.placed.iter().map(|p| p.act).collect();
        for a in [Act::AddTask, Act::DeleteTask, Act::Indent, Act::Duration, Act::Save] {
            assert!(acts.contains(&a), "Task tab missing {a:?}");
        }
    }

    #[test]
    fn file_tab_is_bodyless() {
        let r = Ribbon::new();
        let file = (0..r.tabs.len()).find(|&i| r.tab_label(i) == Some("File")).unwrap();
        assert!(r.tab_is_file(file));
        assert!(r.tab_groups[file].is_empty());
    }

    #[test]
    fn down_from_tabs_enters_a_button() {
        let r = Ribbon::new();
        assert!(matches!(r.nav(Focus::Tab(1), Dir::Down), Focus::Button(_)));
    }

    #[test]
    fn body_rows_share_one_width() {
        let r = Ribbon::new();
        let lines = r.render_body(Focus::None);
        let w = |l: &Line| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
        let w0 = w(&lines[0]);
        for l in &lines {
            assert_eq!(w(l), w0);
        }
    }
}
