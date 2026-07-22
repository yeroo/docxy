//! A Word/Excel-style tabbed ribbon rendered in the terminal — the same
//! interaction model as docxy's ribbon, tuned for the spreadsheet. It is
//! collapsed to its tab headers by default; F9 (or a click on a header)
//! engages it, Down enters the buttons, arrows move, Enter applies, Esc
//! leaves. Buttons are mouse-clickable. The focused button's hint shows in a
//! yellow bar at the ribbon's bottom edge.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// A ribbon command. `Todo` entries are drawn dimmed and only report
/// "not implemented yet" until wired up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Cut,
    Copy,
    Paste,
    Undo,
    Redo,
    Find,
    Replace,
    GoTo,
    ClearContents,
    FillDown,
    FillRight,
    InsertRow,
    InsertCol,
    DeleteRow,
    DeleteCol,
    AddSheet,
    RenameSheet,
    Save,
    SaveAs,
    /// Cell formatting.
    Bold,
    Italic,
    AlignLeft,
    AlignCenter,
    AlignRight,
    NumberFormat,
    FontColor,
    FillColor,
    /// Review ▸ Comments.
    NewComment,
    NewNote,
    DeleteComment,
    PrevComment,
    NextComment,
    ToggleComments,
    /// View toggles.
    FormulaView,
    FreezePanes,
    ShowHidden,
    ShowObjects,
    ThemeToggle,
    AutoHideRibbon,
    Todo(&'static str),
}

impl Act {
    fn enabled(self) -> bool {
        !matches!(self, Act::Todo(_))
    }
}

struct Button {
    glyph: &'static str,
    /// Display columns the glyph occupies — computed, since some glyphs
    /// (💾 ＋ …) are two columns wide and hand-counted widths drift.
    width: usize,
    act: Act,
    hint: &'static str,
}

enum Seg {
    Btn(Button),
    Gap(&'static str),
}

fn btn(glyph: &'static str, act: Act, hint: &'static str) -> Seg {
    Seg::Btn(Button {
        glyph,
        width: glyph.width(),
        act,
        hint,
    })
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
            // "File" has no body — it opens the backstage instead.
            tabs: vec!["File", "Home", "Insert", "Review", "View"],
            active: 1,
            tab_groups: vec![
                Vec::new(),
                home_groups(),
                insert_groups(),
                review_groups(),
                view_groups(),
            ],
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
                        Seg::Gap(s) => x += s.width() as u16,
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
        let focused_tab = if let Focus::Tab(t) = focus {
            Some(t)
        } else {
            None
        };
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if !engaged {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::DIM) // idle: app accent (xlsxy)
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
        spans.push(Span::styled(
            "· F9 ribbon".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
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
        let focused = if let Focus::Button(i) = focus {
            self.placed.get(i).copied()
        } else {
            None
        };
        let row_w = |row: &[Seg]| -> usize {
            row.iter()
                .map(|s| match s {
                    Seg::Gap(g) => g.width(),
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
            let pad = g.width.saturating_sub(g.title.width());
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
        out.push(bar("└", "┴", "┘"));
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

// ---- tab definitions ----

fn home_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Clipboard",
            width: 13,
            rows: [
                vec![btn("Paste", Paste, "Paste (Ctrl+V)")],
                vec![
                    btn("✂ Cut", Cut, "Cut (Ctrl+X)"),
                    Seg::Gap(" "),
                    btn("⧉", Copy, "Copy (Ctrl+C)"),
                ],
            ],
        },
        Group {
            title: "Cells",
            width: 26,
            rows: [
                vec![
                    btn("+Row", InsertRow, "Insert rows above the selection"),
                    Seg::Gap(" "),
                    btn("+Col", InsertCol, "Insert columns left of the selection"),
                    Seg::Gap(" "),
                    btn("Fill↓", FillDown, "Fill down (Ctrl+D)"),
                ],
                vec![
                    btn("−Row", DeleteRow, "Delete the selected rows"),
                    Seg::Gap(" "),
                    btn("−Col", DeleteCol, "Delete the selected columns"),
                    Seg::Gap(" "),
                    btn("Fill→", FillRight, "Fill right (Ctrl+R)"),
                ],
            ],
        },
        Group {
            title: "Font",
            width: 17,
            rows: [
                vec![
                    btn("B", Bold, "Bold (Ctrl+B)"),
                    Seg::Gap("  "),
                    btn("I", Italic, "Italic (Ctrl+I)"),
                    Seg::Gap("  "),
                    btn("A▾", FontColor, "Font color"),
                    Seg::Gap(" "),
                    btn("▧▾", FillColor, "Fill color"),
                ],
                vec![
                    btn("Left", AlignLeft, "Align left"),
                    Seg::Gap(" "),
                    btn("Center", AlignCenter, "Align center"),
                    Seg::Gap(" "),
                    btn("Right", AlignRight, "Align right"),
                ],
            ],
        },
        Group {
            title: "Number",
            width: 8,
            rows: [
                vec![btn("Format ▾", NumberFormat, "Number format…")],
                vec![],
            ],
        },
        Group {
            title: "Editing",
            width: 22,
            rows: [
                vec![
                    btn("⌕ Find", Find, "Find (Ctrl+F)"),
                    Seg::Gap(" "),
                    btn("⇄ Replace", Replace, "Replace (Ctrl+H)"),
                    Seg::Gap(" "),
                    btn("→", GoTo, "Go To (Ctrl+G)"),
                ],
                vec![
                    btn("↶ Undo", Undo, "Undo (Ctrl+Z)"),
                    Seg::Gap(" "),
                    btn("↷ Redo", Redo, "Redo (Ctrl+Y)"),
                    Seg::Gap(" "),
                    btn("⌫ Clear", ClearContents, "Clear (Del)"),
                ],
            ],
        },
        Group {
            title: "File",
            width: 14,
            rows: [
                vec![btn("💾 Save", Save, "Save (Ctrl+S)")],
                vec![btn("Save As…", SaveAs, "Save As (F12)")],
            ],
        },
    ]
}

fn insert_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Sheets",
            width: 20,
            rows: [
                vec![btn("＋ New Sheet", AddSheet, "Add a sheet (Ctrl+T)")],
                vec![btn("✎ Rename", RenameSheet, "Rename the sheet (Shift+F2)")],
            ],
        },
        Group {
            title: "Tables",
            width: 12,
            rows: [
                vec![btn("PivotTable", Todo("PivotTable"), "Insert a PivotTable")],
                vec![btn("Function", Todo("Function"), "Insert a function")],
            ],
        },
    ]
}

fn review_groups() -> Vec<Group> {
    use Act::*;
    vec![Group {
        title: "Comments",
        width: 33,
        rows: [
            vec![
                btn(
                    "✎ Comment",
                    NewComment,
                    "New threaded comment / reply on the current cell",
                ),
                Seg::Gap("  "),
                btn(
                    "✗ Delete",
                    DeleteComment,
                    "Delete the current cell's comment",
                ),
            ],
            vec![
                btn("☰ Note", NewNote, "New legacy note on the current cell"),
                Seg::Gap(" "),
                btn("‹ Prev", PrevComment, "Previous comment"),
                Seg::Gap(" "),
                btn("Next ›", NextComment, "Next comment"),
                Seg::Gap(" "),
                btn("▤", ToggleComments, "Show/hide the comments panel"),
            ],
        ],
    }]
}

fn view_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Show",
            width: 22,
            rows: [
                vec![
                    btn(
                        "ƒ Formulas",
                        FormulaView,
                        "Show formulas instead of values (Ctrl+`)",
                    ),
                    btn(
                        "⤢ Hidden",
                        ShowHidden,
                        "Reveal rows/columns hidden by a filter or manual hide",
                    ),
                ],
                vec![
                    btn(
                        "❄ Freeze",
                        FreezePanes,
                        "Freeze panes at the cursor (toggle)",
                    ),
                    btn(
                        "🖼 Objects",
                        ShowObjects,
                        "Show/hide floating pictures and charts",
                    ),
                ],
            ],
        },
        Group {
            title: "Window",
            width: 14,
            rows: [
                vec![btn("◐ Theme", ThemeToggle, "Toggle light / dark theme")],
                vec![btn(
                    "⬒ Auto-hide",
                    AutoHideRibbon,
                    "Auto-hide the ribbon after each use",
                )],
            ],
        },
        Group {
            title: "Panel",
            width: 12,
            rows: [
                vec![btn(
                    "▤ Comments",
                    ToggleComments,
                    "Show/hide the comments panel",
                )],
                vec![],
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
            r.set_active(tab);
            for g in r.groups() {
                let widest = g
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|seg| match seg {
                                Seg::Gap(s) => s.width(),
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
    fn review_tab_exposes_comment_actions() {
        let mut r = Ribbon::new();
        let review = (0..r.tabs.len())
            .find(|&i| r.tab_label(i) == Some("Review"))
            .unwrap();
        r.set_active(review);
        let acts: Vec<Act> = r.placed.iter().map(|p| p.act).collect();
        for a in [
            Act::NewComment,
            Act::DeleteComment,
            Act::PrevComment,
            Act::NextComment,
            Act::ToggleComments,
        ] {
            assert!(acts.contains(&a), "Review tab missing {a:?}");
        }
    }

    #[test]
    fn down_from_tabs_enters_a_button() {
        let r = Ribbon::new();
        assert!(matches!(r.nav(Focus::Tab(0), Dir::Down), Focus::Button(_)));
    }

    /// Every body line must span the same number of terminal COLUMNS (display
    /// width, not chars) on every tab — otherwise the side borders zigzag
    /// wherever a two-column glyph (💾 ＋ …) appears.
    #[test]
    fn body_rows_share_one_width() {
        let mut r = Ribbon::new();
        for tab in 0..r.tabs.len() {
            if r.tab_groups[tab].is_empty() {
                continue;
            }
            r.set_active(tab);
            let lines = r.render_body(Focus::None);
            let w = |l: &Line| l.spans.iter().map(|s| s.width()).sum::<usize>();
            let w0 = w(&lines[0]);
            for (i, l) in lines.iter().enumerate() {
                assert_eq!(w(l), w0, "tab {tab} line {i} width");
            }
        }
    }

    /// The body is a closed box: top ┌…┐, bottom └…┘.
    #[test]
    fn body_box_is_closed() {
        let r = Ribbon::new();
        let lines = r.render_body(Focus::None);
        assert_eq!(lines.len(), 6, "bar + 2 rows + separator + titles + bar");
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let top = text(&lines[0]);
        let bottom = text(lines.last().unwrap());
        assert!(top.starts_with('┌') && top.ends_with('┐'), "top: {top}");
        assert!(
            bottom.starts_with('└') && bottom.ends_with('┘'),
            "bottom: {bottom}"
        );
    }

    /// A button's placed hit-box width equals its glyph's display width, so
    /// clicks land on what the eye sees.
    #[test]
    fn placed_widths_are_display_widths() {
        let mut r = Ribbon::new();
        for tab in 0..r.tabs.len() {
            if r.tab_groups[tab].is_empty() {
                continue;
            }
            r.set_active(tab);
            for g in r.groups() {
                for row in &g.rows {
                    for seg in row {
                        if let Seg::Btn(b) = seg {
                            assert_eq!(b.width, b.glyph.width(), "button {:?}", b.glyph);
                        }
                    }
                }
            }
        }
    }
}
