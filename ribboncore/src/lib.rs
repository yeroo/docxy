//! The shared ribbon widget for the office TUIs (docxy/xlsxy/yppxy/lookxy).
//!
//! One generic [`Ribbon<A>`] over an app's command type `A` holds the tab/
//! button data and provides the layout, mouse hit-testing, keyboard navigation,
//! and rendering — all identical across apps, so a change here lands in every
//! app at once. Each app supplies its own `Act`, tabs/groups, accent colour and
//! command dispatch; the look is uniform (a closed box drawn entirely in the
//! app accent, the selected tab inverted, no hint bar).
//!
//! The ribbon is collapsed to its 1-row tab strip by default; expanded it is a
//! 6-line bordered body (top border, two button rows, mid divider, group
//! titles, bottom border) — [`EXPANDED_H`] rows including the tab strip.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// A ribbon button.
pub struct Button<A> {
    pub glyph: &'static str,
    pub width: usize,
    pub act: A,
    pub hint: &'static str,
}

/// A segment of a button row: a focusable button or fixed filler text.
pub enum Seg<A> {
    Btn(Button<A>),
    Gap(&'static str),
}

/// A focusable button: `btn(glyph, width, act, hint)`.
pub fn btn<A>(glyph: &'static str, width: usize, act: A, hint: &'static str) -> Seg<A> {
    Seg::Btn(Button {
        glyph,
        width,
        act,
        hint,
    })
}

/// Fixed filler between buttons.
pub fn gap<A>(s: &'static str) -> Seg<A> {
    Seg::Gap(s)
}

/// A group of buttons (two rows) with a centred title, in the tab body.
pub struct Group<A> {
    pub title: &'static str,
    pub width: usize,
    pub rows: [Vec<Seg<A>>; 2],
}

/// Where a placed button sits in the expanded ribbon (cells from the ribbon's
/// own top-left), plus its action — the single source of truth shared by
/// rendering, mouse hit-testing, and keyboard navigation.
struct Placed<A> {
    row: u8, // 0 = first button row, 1 = second
    x: u16,
    w: u16,
    act: A,
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
pub enum Hit<A> {
    Tab(usize),
    Button(A),
    Outside,
}

/// A navigation direction (a key press mapped to a move).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// Rows inside the expanded ribbon body (0 = first button row).
const ROW0: usize = 1;
const ROW1: usize = 2;
/// Total rows a fully-expanded ribbon occupies: the tab strip + the 6-line
/// bordered body (top border, two button rows, mid divider, titles, bottom
/// border).
pub const EXPANDED_H: u16 = 7;

pub struct Ribbon<A> {
    tabs: Vec<&'static str>,
    active: usize,
    /// Groups per tab (aligned with `tabs`); a tab with no groups (e.g. File)
    /// has no in-ribbon body — the app opens a backstage instead.
    tab_groups: Vec<Vec<Group<A>>>,
    placed: Vec<Placed<A>>,
    tab_cols: Vec<(u16, u16)>, // (start, end_exclusive) of each tab header
    active_toggles: Vec<A>,
    /// Per-button glyph overrides (e.g. docxy's Dark-Mode sun/moon): a button
    /// whose act matches draws the override glyph instead of `Button::glyph`.
    glyph_overrides: Vec<(A, &'static str)>,
    /// The app accent — the whole ribbon draws in this colour.
    accent: Color,
}

impl<A: Copy + PartialEq> Ribbon<A> {
    /// Build a ribbon with `tabs` and their `tab_groups` (aligned), the initial
    /// active tab, and the app accent colour.
    pub fn new(
        tabs: Vec<&'static str>,
        tab_groups: Vec<Vec<Group<A>>>,
        active: usize,
        accent: Color,
    ) -> Ribbon<A> {
        let mut r = Ribbon {
            tabs,
            active,
            tab_groups,
            placed: Vec::new(),
            tab_cols: Vec::new(),
            active_toggles: Vec::new(),
            glyph_overrides: Vec::new(),
            accent,
        };
        r.layout();
        r
    }

    /// Replace tab `i`'s groups (a contextual swap, e.g. Home mail/calendar or
    /// the View ▸ Markdown group); re-lays out when `i` is the active tab.
    pub fn set_tab_groups(&mut self, i: usize, groups: Vec<Group<A>>) {
        if let Some(slot) = self.tab_groups.get_mut(i) {
            *slot = groups;
            if self.active == i {
                self.layout();
            }
        }
    }

    /// Set which toggle buttons are "on" (drawn inverted).
    pub fn set_toggles(&mut self, acts: Vec<A>) {
        self.active_toggles = acts;
    }

    /// Override the glyph a button draws (matched by `act`); pass repeatedly to
    /// update. Powers docxy's Dark-Mode sun/moon.
    pub fn set_glyph_override(&mut self, act: A, glyph: &'static str) {
        self.glyph_overrides.retain(|(a, _)| *a != act);
        self.glyph_overrides.push((act, glyph));
    }

    fn glyph_for(&self, b: &Button<A>) -> &'static str {
        self.glyph_overrides
            .iter()
            .find(|(a, _)| *a == b.act)
            .map(|(_, g)| *g)
            .unwrap_or(b.glyph)
    }

    fn groups(&self) -> &[Group<A>] {
        &self.tab_groups[self.active]
    }

    /// Switch to tab `i` if it has a body, re-laying it out. Tabs without a body
    /// are ignored so the current ribbon stays put.
    pub fn set_active(&mut self, i: usize) {
        if i < self.tabs.len() && !self.tab_groups[i].is_empty() {
            self.active = i;
            self.layout();
        }
    }

    /// Whether tab `i` has an in-ribbon body (no body = a backstage tab).
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
        let _content_width = gx;
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
    pub fn tab_label(&self, i: usize) -> Option<&'static str> {
        self.tabs.get(i).copied()
    }
    pub fn width(&self) -> u16 {
        // Right edge of the last tab column (informational).
        self.tab_cols.last().map(|&(_, b)| b).unwrap_or(0)
    }
    pub fn button_count(&self) -> usize {
        self.placed.len()
    }
    pub fn has_act(&self, act: A) -> bool {
        self.placed.iter().any(|p| p.act == act)
    }
    /// Whether `act` is currently drawn as an active toggle (see `set_toggles`).
    pub fn toggle_on(&self, act: A) -> bool {
        self.active_toggles.contains(&act)
    }
    pub fn focus_hint(&self, f: Focus) -> Option<&'static str> {
        match f {
            Focus::Button(i) => self.placed.get(i).map(|p| p.hint),
            _ => None,
        }
    }

    /// The action a focused button would trigger.
    pub fn focus_act(&self, f: Focus) -> Option<(A, &'static str)> {
        match f {
            Focus::Button(i) => self.placed.get(i).map(|p| (p.act, p.hint)),
            _ => None,
        }
    }

    // ---- mouse ----

    /// Hit-test a click. `y` is the row within the ribbon area (0 = tab strip).
    /// `expanded` selects whether the button rows are present.
    pub fn hit(&self, x: u16, y: u16, expanded: bool) -> Hit<A> {
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
                let Some(cur) = self.placed.get(i) else {
                    return Focus::Tab(self.active);
                };
                let (crow, cx) = (cur.row, cur.x);
                match dir {
                    Dir::Left | Dir::Right => self
                        .nearest_in_row(crow, cx, dir == Dir::Right, i)
                        .map(Focus::Button)
                        .unwrap_or(Focus::Button(i)),
                    Dir::Down => self
                        .nearest_in_row_byx(1, cx)
                        .map(Focus::Button)
                        .unwrap_or(Focus::Button(i)),
                    Dir::Up => {
                        if crow == 0 {
                            Focus::Tab(self.active)
                        } else {
                            self.nearest_in_row_byx(0, cx)
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

    /// The collapsed ribbon: the tab strip (one line). App-accent tab names; the
    /// active tab inverted (black on accent); the keyboard cursor reversed.
    pub fn render_tabs(&self, focus: Focus) -> Line<'static> {
        let focused_tab = if let Focus::Tab(t) = focus {
            Some(t)
        } else {
            None
        };
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if i == self.active {
                Style::default().fg(Color::Black).bg(self.accent)
            } else if Some(i) == focused_tab {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(self.accent)
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        Line::from(spans)
    }

    /// The tab strip with `active_tab` drawn as selected and the rest in the
    /// accent — used by the File backstage so File looks selected while the
    /// other tabs stay visible and clickable (their columns match [`hit`]).
    pub fn render_tabs_as(&self, active_tab: usize) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if i == active_tab {
                Style::default().fg(Color::Black).bg(self.accent)
            } else {
                Style::default().fg(self.accent)
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        Line::from(spans)
    }

    /// The expanded ribbon body (a closed box + buttons + group titles, all in
    /// the accent). Does not include the tab strip (line 0).
    pub fn render_body(&self, focus: Focus) -> Vec<Line<'static>> {
        let accent = Style::default().fg(self.accent);
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
            self.placed.get(i).map(|p| (p.row, p.act, p.hint))
        } else {
            None
        };
        let row_w = |row: &[Seg<A>]| -> usize {
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
        out.push(bar("\u{2514}", "\u{2534}", "\u{2518}")); // bottom border — close the box
        out
    }

    fn row_spans(
        &self,
        row: &[Seg<A>],
        rr: u8,
        focused: Option<(u8, A, &'static str)>,
        out: &mut Vec<Span<'static>>,
    ) {
        for seg in row {
            match seg {
                Seg::Gap(s) => out.push(Span::raw(s.to_string())),
                Seg::Btn(b) => {
                    let is_focus = focused
                        .map(|(frow, fact, fhint)| frow == rr && fact == b.act && fhint == b.hint)
                        .unwrap_or(false);
                    let is_on = self.active_toggles.contains(&b.act);
                    let style = if is_focus {
                        Style::default().fg(Color::Black).bg(self.accent)
                    } else if is_on {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default().fg(self.accent)
                    };
                    out.push(Span::styled(self.glyph_for(b).to_string(), style));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Act {
        A,
        B,
        C,
    }

    fn sample() -> Ribbon<Act> {
        let tabs = vec!["File", "Home", "View"];
        let groups = vec![
            Vec::new(), // File: no body
            vec![Group {
                title: "G1",
                width: 7,
                rows: [
                    vec![
                        btn("Aaa", 3, Act::A, "a"),
                        gap(" "),
                        btn("Bbb", 3, Act::B, "b"),
                    ],
                    vec![btn("Ccc", 3, Act::C, "c")],
                ],
            }],
            vec![Group {
                title: "G2",
                width: 3,
                rows: [vec![btn("Ddd", 3, Act::A, "d")], vec![]],
            }],
        ];
        Ribbon::new(tabs, groups, 1, Color::Cyan)
    }

    #[test]
    fn layout_and_queries() {
        let r = sample();
        assert_eq!(r.active_tab(), 1);
        assert_eq!(r.tab_label(0), Some("File"));
        assert!(!r.tab_has_body(0)); // File
        assert!(r.tab_has_body(1));
        assert!(r.has_act(Act::A));
        assert_eq!(r.button_count(), 3);
    }

    #[test]
    fn hit_finds_tabs_and_buttons() {
        let r = sample();
        assert!(matches!(r.hit(2, 0, false), Hit::Tab(0))); // "File" at x=2
        // First button row is at y = ROW0 + 1 = 2; the first button starts at x=2.
        assert!(matches!(r.hit(2, 2, true), Hit::Button(Act::A)));
        assert!(matches!(r.hit(200, 5, true), Hit::Outside));
    }

    #[test]
    fn nav_enters_and_returns() {
        let r = sample();
        let f = r.nav(Focus::Tab(1), Dir::Down);
        assert!(matches!(f, Focus::Button(_)));
        assert!(matches!(r.nav(f, Dir::Up), Focus::Tab(1)));
        assert!(matches!(r.nav(Focus::Tab(1), Dir::Right), Focus::Tab(2)));
    }

    #[test]
    fn set_active_ignores_bodyless_tabs() {
        let mut r = sample();
        r.set_active(0); // File has no body
        assert_eq!(r.active_tab(), 1);
        r.set_active(2);
        assert_eq!(r.active_tab(), 2);
    }

    #[test]
    fn glyph_override_applies() {
        let mut r = sample();
        r.set_glyph_override(Act::A, "ZZZ");
        let body = r.render_body(Focus::None);
        let text: String = body
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.clone())
            .collect();
        assert!(text.contains("ZZZ"));
    }

    #[test]
    fn active_tab_renders_inverted_in_accent() {
        let r = sample();
        let line = r.render_tabs(Focus::None);
        let home = line.spans.iter().find(|s| s.content == "Home").unwrap();
        assert_eq!(home.style.bg, Some(Color::Cyan)); // active tab: bg = accent
        let file = line.spans.iter().find(|s| s.content == "File").unwrap();
        assert_eq!(file.style.fg, Some(Color::Cyan)); // other tabs: fg = accent
    }
}
