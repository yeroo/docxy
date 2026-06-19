//! The Home ribbon: a Word-style tabbed toolbar rendered in the terminal.
//!
//! The ribbon is collapsed to its tab headers by default; clicking a header or
//! pressing F9 expands it. Buttons are mouse-clickable and keyboard-navigable
//! (F9 focuses the tabs, Down enters the buttons, arrows move, Up returns to the
//! tabs). Icons use styled letters, a couple of Braille bar-glyphs for alignment,
//! and plain symbols/labels elsewhere — every glyph is single-width so the layout
//! is exact. The focused/hovered button's description is shown in a black-on-
//! yellow hint bar at the bottom edge.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// A ribbon command. Most map to an existing editor op; `Todo` ones are drawn
/// dimmed and only report "not implemented yet" until wired up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Cut,
    Copy,
    Paste,
    Bold,
    Italic,
    Underline,
    Strike,
    AlignLeft,
    AlignCenter,
    AlignRight,
    Justify,
    ShowHide,
    Find,
    Replace,
    SelectAll,
    /// Not yet implemented; the `&str` is the feature name for the hint.
    Todo(&'static str),
}

impl Act {
    fn enabled(self) -> bool {
        !matches!(self, Act::Todo(_))
    }
}

/// One button: its drawn glyph(s), the action it triggers, and the hint text.
struct Button {
    glyph: &'static str,
    width: usize,
    act: Act,
    hint: &'static str,
}

/// A segment of a button row: either a focusable button or fixed filler text.
enum Seg {
    Btn(Button),
    Gap(&'static str),
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
/// ribbon's own top-left), plus its action — the single source of truth shared by
/// rendering, mouse hit-testing, and keyboard navigation.
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

pub struct Ribbon {
    tabs: Vec<&'static str>,
    active: usize,
    groups: Vec<Group>,
    placed: Vec<Placed>,
    /// Column ranges of each tab header on the (collapsed or expanded) top row.
    tab_cols: Vec<(u16, u16)>, // (start, end_exclusive)
    width: u16,
}

const ROW0: usize = 1; // y of first button row inside the expanded ribbon
const ROW1: usize = 2; // y of second button row

impl Ribbon {
    pub fn home() -> Ribbon {
        use Act::*;
        let groups = vec![
            Group {
                title: "Clipboard",
                width: 9,
                rows: [
                    vec![
                        btn(
                            "Paste",
                            5,
                            Paste,
                            "Paste (Ctrl+V) — insert clipboard contents",
                        ),
                        Seg::Gap(" "),
                        btn("✂", 1, Cut, "Cut (Ctrl+X)"),
                        Seg::Gap(" "),
                        btn("⧉", 1, Copy, "Copy (Ctrl+C)"),
                    ],
                    vec![
                        Seg::Gap(" "),
                        btn(
                            "▾",
                            1,
                            Todo("Paste options"),
                            "Paste options (paste special)",
                        ),
                        Seg::Gap("   "),
                        btn(
                            "▥",
                            1,
                            Todo("Format Painter"),
                            "Format Painter — copy formatting",
                        ),
                    ],
                ],
            },
            Group {
                title: "Font",
                width: 27,
                rows: [
                    vec![
                        btn("[Calibri ▾]", 11, Todo("Font"), "Font — choose a typeface"),
                        btn("[11▾]", 5, Todo("Font size"), "Font size"),
                        Seg::Gap(" "),
                        btn("A↑", 2, Todo("Grow font"), "Grow font (Ctrl+])"),
                        Seg::Gap(" "),
                        btn("A↓", 2, Todo("Shrink font"), "Shrink font (Ctrl+[)"),
                        Seg::Gap(" "),
                        btn("Aa", 2, Todo("Change case"), "Change case (Shift+F3)"),
                        Seg::Gap(" "),
                        btn(
                            "⌧",
                            1,
                            Todo("Clear formatting"),
                            "Clear formatting (Ctrl+Space)",
                        ),
                    ],
                    vec![
                        btn("B", 1, Bold, "Bold (Ctrl+B) — make the selection bold"),
                        Seg::Gap("  "),
                        btn("I", 1, Italic, "Italic (Ctrl+I)"),
                        Seg::Gap("  "),
                        btn("U", 1, Underline, "Underline (Ctrl+U)"),
                        Seg::Gap("  "),
                        btn("S", 1, Strike, "Strikethrough"),
                        Seg::Gap("  "),
                        btn("x₂", 2, Todo("Subscript"), "Subscript (Ctrl+=)"),
                        Seg::Gap(" "),
                        btn("x²", 2, Todo("Superscript"), "Superscript (Ctrl+Shift+=)"),
                        Seg::Gap("  "),
                        btn("ab▾", 3, Todo("Highlight"), "Text highlight colour"),
                        Seg::Gap("  "),
                        btn("A▾", 2, Todo("Font colour"), "Font colour"),
                    ],
                ],
            },
            Group {
                title: "Paragraph",
                width: 27,
                rows: [
                    vec![
                        btn("•──", 3, Todo("Bullets"), "Bullets — bulleted list"),
                        Seg::Gap(" "),
                        btn("1──", 3, Todo("Numbering"), "Numbering — numbered list"),
                        Seg::Gap(" "),
                        btn("•◦─", 3, Todo("Multilevel list"), "Multilevel list"),
                        Seg::Gap(" "),
                        btn(
                            "ind-",
                            4,
                            Todo("Decrease indent"),
                            "Decrease indent (Ctrl+Shift+M)",
                        ),
                        Seg::Gap(" "),
                        btn(
                            "ind+",
                            4,
                            Todo("Increase indent"),
                            "Increase indent (Ctrl+M)",
                        ),
                        Seg::Gap(" "),
                        btn("A↓Z", 3, Todo("Sort"), "Sort"),
                        Seg::Gap(" "),
                        btn(
                            "¶",
                            1,
                            ShowHide,
                            "Show/hide formatting marks (Ctrl+Shift+8)",
                        ),
                    ],
                    vec![
                        btn("⡿⠍", 2, AlignLeft, "Align left (Ctrl+L)"),
                        Seg::Gap(" "),
                        btn("⢽⡯", 2, AlignCenter, "Align center (Ctrl+E)"),
                        Seg::Gap(" "),
                        btn("⠩⢿", 2, AlignRight, "Align right (Ctrl+R)"),
                        Seg::Gap(" "),
                        btn("⣿⣿", 2, Justify, "Justify (Ctrl+J)"),
                        Seg::Gap("  "),
                        btn("↕≡", 2, Todo("Line spacing"), "Line and paragraph spacing"),
                        Seg::Gap("  "),
                        btn("▩▾", 2, Todo("Shading"), "Shading"),
                        Seg::Gap("  "),
                        btn("⊞▾", 2, Todo("Borders"), "Borders"),
                    ],
                ],
            },
            Group {
                title: "Editing",
                width: 7,
                rows: [
                    vec![
                        Seg::Gap("  "),
                        btn("⌕", 1, Find, "Find (Ctrl+F)"),
                        Seg::Gap(" "),
                        btn("⇄", 1, Replace, "Replace (Ctrl+H)"),
                    ],
                    vec![
                        Seg::Gap("   "),
                        btn("▭", 1, SelectAll, "Select all (Ctrl+A)"),
                    ],
                ],
            },
        ];
        let mut r = Ribbon {
            // Only Home exists for now; other tabs aren't drawn until implemented.
            tabs: vec!["Home"],
            active: 0,
            groups,
            placed: Vec::new(),
            tab_cols: Vec::new(),
            width: 0,
        };
        r.layout();
        r
    }

    /// Compute placed-button rects and total width from the group definitions.
    fn layout(&mut self) {
        self.placed.clear();
        // Each group cell is " " + content(width) + " " between │ borders.
        let mut gx = 1u16; // after the left "│"
        for g in &self.groups {
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
            gx += g.width as u16 + 3; // cell pad(1)+content+pad(1) + next "│"
        }
        self.width = gx; // includes the final "│"
        // Tab header columns on the top tab strip: "  File   Home   ...".
        self.tab_cols.clear();
        let mut tx = 2u16;
        for t in &self.tabs {
            let w = t.chars().count() as u16;
            self.tab_cols.push((tx, tx + w));
            tx += w + 3; // three spaces between tabs
        }
    }

    pub fn active_tab(&self) -> usize {
        self.active
    }
    pub fn width(&self) -> u16 {
        self.width
    }
    #[cfg(test)]
    pub fn button_count(&self) -> usize {
        self.placed.len()
    }
    #[cfg(test)]
    pub fn focus_hint(&self, f: Focus) -> Option<&'static str> {
        match f {
            Focus::Button(i) => self.placed.get(i).map(|p| p.hint),
            _ => None,
        }
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
    /// `expanded` selects whether button rows are present.
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
            // Button rows are at ribbon-y ROW0/ROW1 → tab strip is y0, box top y1,
            // so button rows render at y2/y3.
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
                            Focus::Tab(self.active) // top row → back to tabs
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

    /// Render the collapsed ribbon: just the tab strip (one line).
    pub fn render_tabs(&self, focus: Focus) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        let focused_tab = if let Focus::Tab(t) = focus {
            Some(t)
        } else {
            None
        };
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if i == self.active {
                // active tab: inverted (white bg / black fg)
                Style::default().fg(Color::Black).bg(Color::White)
            } else if Some(i) == focused_tab {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        Line::from(spans)
    }

    /// Render the expanded ribbon body (box + buttons + group titles). Does not
    /// include the tab strip (line 0) or the hint bar.
    pub fn render_body(&self, focus: Focus) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let widths: Vec<usize> = self.groups.iter().map(|g| g.width).collect();
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
        let mut out = vec![bar("┌", "┬", "┐")];
        for ri in 0..2 {
            let mut spans = vec![Span::styled("│", dim)];
            for g in &self.groups {
                spans.push(Span::raw(" "));
                self.row_spans(&g.rows[ri], ri as u8, focused, &mut spans);
                // Pad the row to the group width so the right border lines up with
                // the ┬/┼ in the borders above and below.
                let pad = g.width.saturating_sub(row_w(&g.rows[ri]));
                spans.push(Span::raw(" ".repeat(pad + 1)));
                spans.push(Span::styled("│", dim));
            }
            out.push(Line::from(spans));
        }
        out.push(bar("├", "┼", "┤"));
        // group titles
        let mut spans = vec![Span::styled("│", dim)];
        for g in &self.groups {
            let pad = g.width.saturating_sub(g.title.chars().count());
            let l = pad / 2;
            spans.push(Span::raw(format!(
                " {}{}{} ",
                " ".repeat(l),
                g.title,
                " ".repeat(pad - l)
            )));
            spans.push(Span::styled("│", dim));
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
                    let style = if is_focus {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
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
                    format!(" {h} — not implemented yet")
                }
            }
            _ => {
                " F9 ribbon · ←→ tabs · ↓ enter · arrows move · Enter apply · Esc leave".to_string()
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_and_dimmed_buttons_are_classified() {
        let r = Ribbon::home();
        assert!(Act::Bold.enabled());
        assert!(Act::AlignLeft.enabled());
        assert!(!Act::Todo("Bullets").enabled());
        assert!(r.button_count() > 20);
    }

    #[test]
    fn clicking_the_home_tab_is_detected() {
        let r = Ribbon::home();
        let (a, b) = r.tab_cols[0];
        assert!(matches!(r.hit((a + b) / 2, 0, false), Hit::Tab(0)));
    }

    #[test]
    fn down_from_tabs_enters_first_button_then_up_returns() {
        let r = Ribbon::home();
        let f = r.nav(Focus::Tab(0), Dir::Down);
        assert!(matches!(f, Focus::Button(_)));
        // the first button is on the top row, so Up goes back to the tabs
        assert!(matches!(r.nav(f, Dir::Up), Focus::Tab(_)));
    }

    #[test]
    fn left_right_moves_along_a_row() {
        let r = Ribbon::home();
        let start = r.enter_body();
        let right = r.nav(start, Dir::Right);
        assert_ne!(format!("{start:?}"), format!("{right:?}"));
        // moving right then left returns to (or near) the start row
        if let (Focus::Button(_), Focus::Button(_)) = (start, right) {
        } else {
            panic!("expected button focus");
        }
    }

    #[test]
    fn body_border_columns_line_up_on_every_row() {
        let r = Ribbon::home();
        let lines = r.render_body(Focus::None);
        let width = |l: &Line| -> usize { l.spans.iter().map(|s| s.content.chars().count()).sum() };
        let bar_cols = |l: &Line| -> Vec<usize> {
            let mut cols = Vec::new();
            let mut c = 0usize;
            for sp in &l.spans {
                for ch in sp.content.chars() {
                    if "┌┐└┘├┤┬┴┼│".contains(ch) {
                        cols.push(c);
                    }
                    c += 1;
                }
            }
            cols
        };
        let top = bar_cols(&lines[0]);
        for (i, l) in lines.iter().enumerate() {
            assert_eq!(width(l), width(&lines[0]), "line {i} has a different width");
            assert_eq!(bar_cols(l), top, "border columns drift on line {i}");
        }
    }

    #[test]
    fn paste_split_dropdown_is_keyboard_reachable() {
        let r = Ribbon::home();
        // Entering the body lands on Paste (first button, top-left).
        let paste = r.enter_body();
        assert!(matches!(r.focus_act(paste), Some((Act::Paste, _))));
        // Down from Paste reaches its dropdown caret directly below it.
        let down = r.nav(paste, Dir::Down);
        let hint = r.focus_hint(down).unwrap_or("");
        assert!(
            hint.contains("Paste options"),
            "Down from Paste should reach the paste dropdown, got: {hint}"
        );
    }

    #[test]
    fn focused_button_exposes_hint() {
        let r = Ribbon::home();
        // find Bold's placed index
        let i = (0..r.button_count())
            .find(|&i| matches!(r.focus_act(Focus::Button(i)), Some((Act::Bold, _))))
            .unwrap();
        assert!(r.focus_hint(Focus::Button(i)).unwrap().contains("Bold"));
    }
}
