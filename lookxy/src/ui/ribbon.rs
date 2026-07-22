//! The ribbon: an Outlook-style tabbed toolbar rendered in the terminal,
//! ported from `docxy/src/ribbon.rs` (same `Ribbon`/`Act`/`Seg`/`Group`/
//! `Placed`/`Focus`/`Hit`/`Dir` shapes and the same render/hit/nav model),
//! adapted to lookxy's command set.
//!
//! Collapsed to its tab strip by default; clicking a header or pressing F9
//! expands it. Buttons are mouse-clickable and keyboard-navigable (F9 focuses
//! the tabs, Down enters the buttons, arrows move, Up returns to the tabs).
//! Every glyph is single-width so the layout is exact. The focused/hovered
//! button's description shows in a black-on-yellow hint bar at the bottom edge.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// A ribbon command. Each maps to an existing `App` method (see
/// `App::run_ribbon_act`); `Todo` ones are drawn dimmed and only report "not
/// implemented yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    // Home (Mail)
    Compose,
    Reply,
    ReplyAll,
    Forward,
    Delete,
    Flag,
    MarkRead,
    MarkUnread,
    Move,
    Categorize,
    Find,
    // Home (Calendar)
    NewEvent,
    EditEvent,
    DeleteEvent,
    RsvpAccept,
    RsvpDecline,
    RsvpTentative,
    // Send / Receive
    SendReceive,
    // Folder
    ExpandAll,
    CollapseAll,
    // View
    Threaded,
    CategoryFilter,
    // Help
    Help,
    /// Not yet implemented; the `&str` is the feature name for the hint.
    Todo(&'static str),
}

impl Act {
    fn enabled(self) -> bool {
        !matches!(self, Act::Todo(_))
    }
}

/// A segment of a button row: a focusable button or fixed filler text.
enum Seg {
    Btn(Button),
    Gap(&'static str),
}

struct Button {
    glyph: &'static str,
    width: usize,
    act: Act,
    hint: &'static str,
}

fn btn(glyph: &'static str, width: usize, act: Act, hint: &'static str) -> Seg {
    Seg::Btn(Button {
        glyph,
        width,
        act,
        hint,
    })
}

struct Group {
    title: &'static str,
    width: usize,
    rows: [Vec<Seg>; 2],
}

/// Where a placed button sits in the expanded ribbon (cells, 0-based from the
/// ribbon's own top-left), plus its action — the single source of truth shared
/// by rendering, mouse hit-testing, and keyboard navigation.
#[derive(Clone, Copy)]
struct Placed {
    row: u8, // 0 = first button row, 1 = second
    x: u16,
    w: u16,
    act: Act,
    hint: &'static str,
}

/// Keyboard focus within the ribbon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Not in the ribbon.
    None,
    /// On the tab headers (index of the focused tab).
    Tab(usize),
    /// On a button (index into the placed-button list).
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
    /// Groups per tab (aligned with `tabs`); File has none — it opens the
    /// backstage instead of an in-ribbon body.
    tab_groups: Vec<Vec<Group>>,
    placed: Vec<Placed>,
    tab_cols: Vec<(u16, u16)>, // (start, end_exclusive) of each tab header
    active_toggles: Vec<Act>,
}

/// The app's ribbon accent — lookxy is cyan (docxy light blue, xlsxy green,
/// yppxy yellow — one colour per app in the suite). The whole ribbon (tab
/// names, box, group titles, button glyphs) draws in this colour.
const ACCENT: Color = Color::Cyan;

/// Rows inside the expanded ribbon body (0 = first button row).
const ROW0: usize = 1;
const ROW1: usize = 2;
/// Total rows a fully-expanded ribbon occupies: tab strip + 5 body lines + hint.
pub const EXPANDED_H: u16 = 7;

impl Ribbon {
    pub fn new() -> Ribbon {
        let mut r = Ribbon {
            tabs: vec!["File", "Home", "Send/Receive", "Folder", "View", "Help"],
            active: 1,
            tab_groups: vec![
                Vec::new(),
                home_groups(false),
                send_receive_groups(),
                folder_groups(),
                view_groups(),
                help_groups(),
            ],
            placed: Vec::new(),
            tab_cols: Vec::new(),
            active_toggles: Vec::new(),
        };
        r.layout();
        r
    }

    /// Swap the Home tab between the mail and calendar button sets; relays out
    /// when Home is the active tab.
    pub fn set_home_context(&mut self, calendar: bool) {
        for (idx, tab) in self.tabs.iter().enumerate() {
            if *tab == "Home" {
                self.tab_groups[idx] = home_groups(calendar);
                if self.active == idx {
                    self.layout();
                }
                break;
            }
        }
    }

    /// Set which toggle buttons are "on" (drawn inverted).
    pub fn set_toggles(&mut self, acts: Vec<Act>) {
        self.active_toggles = acts;
    }

    fn groups(&self) -> &[Group] {
        &self.tab_groups[self.active]
    }

    /// Switch to tab `i` if it has a body, re-laying it out. Tabs without a body
    /// (File) are ignored so the current ribbon stays put.
    pub fn set_active(&mut self, i: usize) {
        if i < self.tabs.len() && !self.tab_groups[i].is_empty() {
            self.active = i;
            self.layout();
        }
    }

    /// Whether tab `i` has an in-ribbon body (File does not).
    pub fn tab_has_body(&self, i: usize) -> bool {
        self.tab_groups.get(i).is_some_and(|g| !g.is_empty())
    }

    fn layout(&mut self) {
        self.placed.clear();
        let mut gx = 1u16; // after the left "│"
        for g in &self.tab_groups[self.active] {
            for (ri, row) in g.rows.iter().enumerate() {
                let mut x = gx + 1; // group cell has a leading pad space
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
            gx += g.width as u16 + 3; // pad(1)+content+pad(1) + next "│"
        }
        let _content_width = gx; // ribbon body width; not needed past layout
        self.tab_cols.clear();
        let mut tx = 2u16;
        for t in &self.tabs {
            let w = t.chars().count() as u16;
            self.tab_cols.push((tx, tx + w));
            tx += w + 3;
        }
    }

    pub fn active_tab(&self) -> usize {
        self.active
    }
    #[cfg(test)]
    pub fn tab_label(&self, i: usize) -> Option<&'static str> {
        self.tabs.get(i).copied()
    }
    #[cfg(test)]
    pub fn button_count(&self) -> usize {
        self.placed.len()
    }
    #[cfg(test)]
    pub fn has_act(&self, act: Act) -> bool {
        self.placed.iter().any(|p| p.act == act)
    }
    #[cfg(test)]
    pub fn toggle_on(&self, act: Act) -> bool {
        self.active_toggles.contains(&act)
    }

    /// The action a focused button would trigger.
    pub fn focus_act(&self, f: Focus) -> Option<(Act, &'static str)> {
        match f {
            Focus::Button(i) => self.placed.get(i).map(|p| (p.act, p.hint)),
            _ => None,
        }
    }

    // ---- mouse ----

    /// Hit-test a click. `y` is the row within the ribbon area (0 = tab strip).
    /// `expanded` selects whether the button rows are present.
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

    // ---- keyboard navigation ----

    /// First button when entering the body from the tabs (Down).
    pub fn enter_body(&self) -> Focus {
        self.placed
            .iter()
            .position(|p| p.row == 0)
            .map(Focus::Button)
            .unwrap_or(Focus::Tab(self.active))
    }

    /// Move focus. Returns the new focus (may step back onto the tabs).
    pub fn nav(&self, f: Focus, dir: Dir) -> Focus {
        match f {
            Focus::None => Focus::Tab(self.active),
            Focus::Tab(t) => match dir {
                Dir::Left => Focus::Tab(t.saturating_sub(1)),
                Dir::Right => Focus::Tab((t + 1).min(self.tabs.len() - 1)),
                Dir::Down => self.enter_body(),
                Dir::Up => Focus::Tab(t),
            },
            Focus::Button(i) => {
                let Some(cur) = self.placed.get(i).copied() else {
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

    /// Render the collapsed ribbon: the tab strip (one line). The active tab is
    /// app-accent tab names; the active tab is inverted (black on accent), the
    /// keyboard cursor (when the ribbon is engaged) is reversed.
    pub fn render_tabs(&self, focus: Focus) -> Line<'static> {
        let focused_tab = if let Focus::Tab(t) = focus {
            Some(t)
        } else {
            None
        };
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if i == self.active {
                // Selected tab: inverted — black on the app accent.
                Style::default().fg(Color::Black).bg(ACCENT)
            } else if Some(i) == focused_tab {
                Style::default().add_modifier(Modifier::REVERSED) // keyboard cursor
            } else {
                Style::default().fg(ACCENT) // other tab names: app colour
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        Line::from(spans)
    }

    /// Render the tab strip with `active_tab` drawn selected (highlighted) and
    /// the rest dimmed — used by the File backstage so File looks selected while
    /// the other tabs stay visible and clickable (their columns match `hit`).
    pub fn render_tabs_as(&self, active_tab: usize) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if i == active_tab {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                Style::default().fg(ACCENT)
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        Line::from(spans)
    }

    /// Render the expanded ribbon body (box + buttons + group titles). Does not
    /// include the tab strip (line 0) or the hint bar.
    pub fn render_body(&self, focus: Focus) -> Vec<Line<'static>> {
        // The whole ribbon body draws in the app accent (box, titles, glyphs).
        let accent = Style::default().fg(ACCENT);
        let widths: Vec<usize> = self.groups().iter().map(|g| g.width).collect();
        let bar = |l: &str, m: &str, r: &str| -> Line<'static> {
            let mut s = String::from(l);
            for (i, w) in widths.iter().enumerate() {
                if i > 0 {
                    s.push_str(m);
                }
                s.push_str(&"\u{2500}".repeat(w + 2));
            }
            s.push_str(r);
            Line::styled(s, accent)
        };
        let focused = if let Focus::Button(i) = focus {
            self.placed.get(i).copied()
        } else {
            None
        };
        let row_w = |row: &[Seg]| -> usize {
            row.iter()
                .map(|s| match s {
                    Seg::Gap(g) => g.chars().count(),
                    Seg::Btn(b) => b.width,
                })
                .sum()
        };
        let mut out = vec![bar("\u{250c}", "\u{252c}", "\u{2510}")];
        for ri in 0..2 {
            let mut spans = vec![Span::styled("\u{2502}", accent)];
            for g in self.groups() {
                spans.push(Span::raw(" "));
                self.row_spans(&g.rows[ri], ri as u8, focused, &mut spans);
                let pad = g.width.saturating_sub(row_w(&g.rows[ri]));
                spans.push(Span::raw(" ".repeat(pad + 1)));
                spans.push(Span::styled("\u{2502}", accent));
            }
            out.push(Line::from(spans));
        }
        out.push(bar("\u{251c}", "\u{253c}", "\u{2524}"));
        let mut spans = vec![Span::styled("\u{2502}", accent)];
        for g in self.groups() {
            let pad = g.width.saturating_sub(g.title.chars().count());
            let l = pad / 2;
            spans.push(Span::styled(
                format!(" {}{}{} ", " ".repeat(l), g.title, " ".repeat(pad - l)),
                accent,
            ));
            spans.push(Span::styled("\u{2502}", accent));
        }
        out.push(Line::from(spans));
        out
    }

    fn row_spans(
        &self,
        row: &[Seg],
        rr: u8,
        focused: Option<Placed>,
        out: &mut Vec<Span<'static>>,
    ) {
        for seg in row {
            match seg {
                Seg::Gap(s) => out.push(Span::raw(s.to_string())),
                Seg::Btn(b) => {
                    let is_focus = focused
                        .map(|p| p.row == rr && p.act == b.act && p.hint == b.hint)
                        .unwrap_or(false);
                    let is_on = self.active_toggles.contains(&b.act);
                    let style = if is_focus {
                        Style::default().fg(Color::Black).bg(ACCENT)
                    } else if is_on {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if b.act.enabled() {
                        Style::default().fg(ACCENT)
                    } else {
                        Style::default().add_modifier(Modifier::DIM)
                    };
                    out.push(Span::styled(b.glyph.to_string(), style));
                }
            }
        }
    }

    /// The yellow hint bar shown at the ribbon's bottom edge.
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
                    format!(" {h} \u{2014} not implemented yet")
                }
            }
            _ => " F9 ribbon \u{b7} \u{2190}\u{2192} tabs \u{b7} \u{2193} enter \u{b7} arrows move \u{b7} Enter apply \u{b7} Esc leave".to_string(),
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
        Self::new()
    }
}

/// The Home tab's groups — mail actions, or (when `calendar`) event actions.
fn home_groups(calendar: bool) -> Vec<Group> {
    use Act::*;
    if calendar {
        return vec![
            Group {
                title: "New",
                width: 3,
                rows: [vec![btn("New", 3, NewEvent, "New event (c)")], vec![]],
            },
            Group {
                title: "Event",
                width: 8,
                rows: [
                    vec![
                        btn("Edit", 4, EditEvent, "Edit event (e)"),
                        Seg::Gap(" "),
                        btn("Del", 3, DeleteEvent, "Delete event (x)"),
                    ],
                    vec![],
                ],
            },
            Group {
                title: "RSVP",
                width: 7,
                rows: [
                    vec![
                        btn("Acc", 3, RsvpAccept, "Accept (a)"),
                        Seg::Gap(" "),
                        btn("Dec", 3, RsvpDecline, "Decline (d)"),
                    ],
                    vec![btn("Tent", 4, RsvpTentative, "Tentative (t)")],
                ],
            },
        ];
    }
    vec![
        Group {
            title: "New",
            width: 3,
            rows: [vec![btn("New", 3, Compose, "New message (c)")], vec![]],
        },
        Group {
            title: "Respond",
            width: 9,
            rows: [
                vec![
                    btn("Reply", 5, Reply, "Reply (r)"),
                    Seg::Gap(" "),
                    btn("All", 3, ReplyAll, "Reply all (R)"),
                ],
                vec![btn("Fwd", 3, Forward, "Forward (F)")],
            ],
        },
        Group {
            title: "Manage",
            width: 13,
            rows: [
                vec![
                    btn("Del", 3, Delete, "Delete (d)"),
                    Seg::Gap(" "),
                    btn("Flag", 4, Flag, "Flag (f)"),
                    Seg::Gap(" "),
                    btn("Read", 4, MarkRead, "Mark read (m)"),
                ],
                vec![
                    btn("Unread", 6, MarkUnread, "Mark unread (u)"),
                    Seg::Gap(" "),
                    btn("Move", 4, Move, "Move to folder (v)"),
                ],
            ],
        },
        Group {
            title: "Tools",
            width: 5,
            rows: [
                vec![btn("Label", 5, Categorize, "Categorize (l)")],
                vec![btn("Find", 4, Find, "Search (/)")],
            ],
        },
    ]
}

fn send_receive_groups() -> Vec<Group> {
    vec![Group {
        title: "Sync",
        width: 14,
        rows: [
            vec![btn(
                "Send & Receive",
                14,
                Act::SendReceive,
                "Sync mail and calendar now",
            )],
            vec![],
        ],
    }]
}

fn folder_groups() -> Vec<Group> {
    vec![Group {
        title: "Tree",
        width: 10,
        rows: [
            vec![btn("Expand All", 10, Act::ExpandAll, "Expand every folder")],
            vec![btn(
                "Collapse",
                8,
                Act::CollapseAll,
                "Collapse every folder",
            )],
        ],
    }]
}

fn view_groups() -> Vec<Group> {
    vec![Group {
        title: "Layout",
        width: 8,
        rows: [
            vec![btn(
                "Threaded",
                8,
                Act::Threaded,
                "Toggle threaded/flat view (t)",
            )],
            vec![btn(
                "Filter",
                6,
                Act::CategoryFilter,
                "Filter by category (L)",
            )],
        ],
    }]
}

fn help_groups() -> Vec<Group> {
    vec![Group {
        title: "Help",
        width: 9,
        rows: [
            vec![btn("Shortcuts", 9, Act::Help, "Keyboard shortcuts (F1)")],
            vec![btn("About", 5, Act::Todo("About"), "About lookxy")],
        ],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content_w(row: &[Seg]) -> usize {
        row.iter()
            .map(|s| match s {
                Seg::Gap(g) => g.chars().count(),
                Seg::Btn(b) => b.width,
            })
            .sum()
    }

    #[test]
    fn every_group_is_wide_enough_for_its_content() {
        for calendar in [false, true] {
            let tabs = [
                home_groups(calendar),
                send_receive_groups(),
                folder_groups(),
                view_groups(),
                help_groups(),
            ];
            for groups in tabs {
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
    }

    #[test]
    fn home_has_six_tabs_and_mail_actions() {
        let r = Ribbon::new();
        assert_eq!(r.tabs.len(), 6);
        assert_eq!(r.tab_label(0), Some("File"));
        assert!(r.has_act(Act::Compose));
        assert!(r.button_count() > 0);
        assert!(!r.tab_has_body(0)); // File opens the backstage
    }

    #[test]
    fn idle_tab_names_use_the_app_accent() {
        let r = Ribbon::new();
        let line = r.render_tabs(Focus::None);
        // The first non-padding span is a tab name, coloured with the accent.
        let first = line
            .spans
            .iter()
            .find(|s| s.content.trim() == "File")
            .unwrap();
        assert_eq!(first.style.fg, Some(ACCENT));
    }

    #[test]
    fn clicking_a_tab_header_hit_tests_to_that_tab() {
        let r = Ribbon::new();
        // The very first tab header column ("File") starts at x=2.
        assert!(matches!(r.hit(2, 0, false), Hit::Tab(0)));
    }

    #[test]
    fn nav_enters_buttons_and_returns_to_tabs() {
        let r = Ribbon::new();
        let f = r.nav(Focus::Tab(1), Dir::Down); // into the body
        assert!(matches!(f, Focus::Button(_)));
        let back = r.nav(f, Dir::Up); // top row → back to tabs
        assert!(matches!(back, Focus::Tab(1)));
    }
}
