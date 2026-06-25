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
    /// Open the Paste Special dialog (choose how clipboard content is pasted).
    PasteSpecial,
    /// Insert a horizontal line (paragraph bottom border) at the caret.
    HorizontalLine,
    /// Open the Insert Field dialog (date, page, author, …).
    InsertField,
    Bold,
    Italic,
    Underline,
    Strike,
    /// Subscript / superscript (toggle vertical alignment).
    Subscript,
    Superscript,
    /// Grow / shrink the font size of the selection.
    GrowFont,
    ShrinkFont,
    /// Cycle the case of the selection (Shift+F3).
    ChangeCase,
    /// Reset character formatting (Ctrl+Space).
    ClearFormatting,
    /// Pickers: font typeface, size, font colour, highlight colour.
    FontName,
    FontSize,
    FontColor,
    Highlight,
    /// Paragraph group: lists, indent, sort, borders.
    Bullets,
    Numbering,
    IncreaseIndent,
    DecreaseIndent,
    /// First-line indent / hanging indent quick-apply (0.5").
    FirstLineIndent,
    HangingIndent,
    /// Open the Paragraph dialog (precise left/first-line/hanging indent).
    ParagraphDialog,
    Sort,
    ParaBorders,
    AlignLeft,
    AlignCenter,
    AlignRight,
    Justify,
    ShowHide,
    Find,
    Replace,
    SelectAll,
    /// Toggle the comments review side panel.
    ToggleComments,
    /// Jump to the previous / next review comment.
    PrevComment,
    NextComment,
    /// Add a comment on the selection / delete the selected comment.
    NewComment,
    DeleteComment,
    // View tab
    /// Reflowed reading view (page layout off).
    ReadMode,
    /// Print layout (pages with margins/headers).
    PrintLayout,
    /// Switch the document page between dark and light.
    DarkMode,
    /// Toggle the column ruler.
    ToggleRuler,
    /// Toggle the navigation (outline) pane.
    ToggleNav,
    /// Auto-hide the ribbon: collapse it to the tab strip after each use.
    AutoHideRibbon,
    /// Edit the body / the header / the footer (a 3-way surface switch).
    EditDocument,
    EditHeader,
    EditFooter,
    /// Markdown view switch (only on the View tab for `.md` files): show the
    /// document rendered, or edit the raw Markdown source.
    MdRendered,
    MdSource,
    /// Apply a named paragraph style (the `&str` is the `w:styleId`).
    ApplyStyle(&'static str),
    /// Open the Apply-Styles dialog (every style the document defines).
    StylesDialog,
    /// Not yet implemented; the `&str` is the feature name for the hint.
    Todo(&'static str),
}

/// The named styles offered on the Styles ribbon, as `(label, styleId)`. Kept in
/// one place so the buttons and the active-style highlight can't drift apart.
pub const STYLE_BUTTONS: &[(&str, &str)] = &[
    ("Normal", "Normal"),
    ("No Spacing", "NoSpacing"),
    ("Title", "Title"),
    ("Subtitle", "Subtitle"),
    ("Heading 1", "Heading1"),
    ("Heading 2", "Heading2"),
    ("Heading 3", "Heading3"),
];

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
    /// Groups per tab (aligned with `tabs`); the File tab has none — it opens
    /// the backstage instead of an in-ribbon body.
    tab_groups: Vec<Vec<Group>>,
    placed: Vec<Placed>,
    /// Column ranges of each tab header on the (collapsed or expanded) top row.
    tab_cols: Vec<(u16, u16)>, // (start, end_exclusive)
    width: u16,
    /// Toggle buttons that are currently "on" — drawn with inverted fg/bg.
    active_toggles: Vec<Act>,
    /// Whether the document page is light (drives the View ▸ Dark Mode icon).
    light_page: bool,
    /// Whether a Markdown file is open (adds the View ▸ Markdown group).
    markdown: bool,
}

const ROW0: usize = 1; // y of first button row inside the expanded ribbon
const ROW1: usize = 2; // y of second button row

impl Ribbon {
    pub fn home() -> Ribbon {
        let mut r = Ribbon {
            // File opens the backstage; the rest are ribbons.
            tabs: vec!["File", "Home", "Styles", "Insert", "Review", "View"],
            active: 1,
            tab_groups: vec![
                Vec::new(),
                home_groups(),
                styles_groups(),
                insert_groups(),
                review_groups(),
                view_groups(false),
            ],
            placed: Vec::new(),
            tab_cols: Vec::new(),
            width: 0,
            active_toggles: Vec::new(),
            light_page: false,
            markdown: false,
        };
        r.layout();
        r
    }

    /// Show or hide the contextual View ▸ Markdown group (Rendered / Source). A
    /// no-op when already in the requested state; relays out if View is active.
    pub fn set_markdown(&mut self, on: bool) {
        if self.markdown == on {
            return;
        }
        self.markdown = on;
        if let Some(idx) = self.tabs.iter().position(|t| *t == "View") {
            self.tab_groups[idx] = view_groups(on);
            if self.active == idx {
                self.layout();
            }
        }
    }

    /// Set which toggle buttons are "on" (drawn inverted).
    pub fn set_toggles(&mut self, acts: Vec<Act>) {
        self.active_toggles = acts;
    }

    /// Set whether the page is light (flips the Dark Mode sun/moon icon).
    pub fn set_light_page(&mut self, on: bool) {
        self.light_page = on;
    }

    /// The groups of the currently active tab (empty for tabs without a body).
    fn groups(&self) -> &[Group] {
        &self.tab_groups[self.active]
    }

    /// Switch the active ribbon to tab `i` if it has a body, re-laying it out.
    /// Tabs without a body (File) are ignored, so the current ribbon stays put.
    pub fn set_active(&mut self, i: usize) {
        if i < self.tabs.len() && !self.tab_groups[i].is_empty() {
            self.active = i;
            self.layout();
        }
    }
}

/// The Home tab's groups (Clipboard / Font / Paragraph / Editing).
fn home_groups() -> Vec<Group> {
    use Act::*;
    vec![
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
                        PasteSpecial,
                        "Paste Special (Ctrl+Alt+V) — choose paste format",
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
                    btn("[Calibri ▾]", 11, FontName, "Font — choose a typeface"),
                    btn("[11▾]", 5, FontSize, "Font size"),
                    Seg::Gap(" "),
                    btn("A↑", 2, GrowFont, "Grow font (Ctrl+])"),
                    Seg::Gap(" "),
                    btn("A↓", 2, ShrinkFont, "Shrink font (Ctrl+[)"),
                    Seg::Gap(" "),
                    btn("Aa", 2, ChangeCase, "Change case (Shift+F3)"),
                    Seg::Gap(" "),
                    btn("⌧", 1, ClearFormatting, "Clear formatting (Ctrl+Space)"),
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
                    btn("x₂", 2, Subscript, "Subscript (Ctrl+=)"),
                    Seg::Gap(" "),
                    btn("x²", 2, Superscript, "Superscript (Ctrl+Shift+=)"),
                    Seg::Gap("  "),
                    btn("ab▾", 3, Highlight, "Text highlight colour"),
                    Seg::Gap("  "),
                    btn("A▾", 2, FontColor, "Font colour"),
                ],
            ],
        },
        Group {
            title: "Paragraph",
            width: 27,
            rows: [
                vec![
                    btn("•──", 3, Bullets, "Bullets — bulleted list"),
                    Seg::Gap(" "),
                    btn("1──", 3, Numbering, "Numbering — numbered list"),
                    Seg::Gap(" "),
                    btn("•◦─", 3, Todo("Multilevel list"), "Multilevel list"),
                    Seg::Gap(" "),
                    btn("ind-", 4, DecreaseIndent, "Decrease indent (Ctrl+Shift+M)"),
                    Seg::Gap(" "),
                    btn("ind+", 4, IncreaseIndent, "Increase indent (Ctrl+M)"),
                    Seg::Gap(" "),
                    btn("A↓Z", 3, Sort, "Sort selected paragraphs A→Z"),
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
                    Seg::Gap(" "),
                    btn("⊞▾", 2, ParaBorders, "Bottom border (toggle)"),
                    Seg::Gap(" "),
                    btn("¶→", 2, FirstLineIndent, "First-line indent (0.5\")"),
                    Seg::Gap(" "),
                    btn("¶↤", 2, HangingIndent, "Hanging indent (0.5\")"),
                    Seg::Gap(" "),
                    btn("¶…", 2, ParagraphDialog, "Paragraph — indent & spacing…"),
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
    ]
}

/// The Styles tab: a gallery of common paragraph styles plus a "More…" launcher
/// for the full Apply-Styles dialog. Button labels/ids come from [`STYLE_BUTTONS`].
fn styles_groups() -> Vec<Group> {
    use Act::*;
    let b = |label: &'static str, id: &'static str| {
        btn(
            label,
            label.chars().count(),
            ApplyStyle(id),
            "Apply this paragraph style",
        )
    };
    vec![Group {
        title: "Styles",
        width: 35,
        rows: [
            vec![
                b("Normal", "Normal"),
                Seg::Gap(" "),
                b("No Spacing", "NoSpacing"),
                Seg::Gap(" "),
                b("Title", "Title"),
                Seg::Gap(" "),
                b("Subtitle", "Subtitle"),
            ],
            vec![
                b("Heading 1", "Heading1"),
                Seg::Gap(" "),
                b("Heading 2", "Heading2"),
                Seg::Gap(" "),
                b("Heading 3", "Heading3"),
                Seg::Gap(" "),
                btn("More…", 5, StylesDialog, "Apply Styles — pick any style…"),
            ],
        ],
    }]
}

/// The Review tab's groups (Comments / Tracking).
/// The Insert tab's groups (Pages / Symbols).
fn insert_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Pages",
            width: 10,
            rows: [
                vec![btn(
                    "Page Break",
                    10,
                    Todo("Page Break"),
                    "Insert a page break",
                )],
                vec![btn(
                    "Blank Page",
                    10,
                    Todo("Blank Page"),
                    "Insert a blank page",
                )],
            ],
        },
        Group {
            title: "Text",
            width: 13,
            rows: [
                vec![btn(
                    "⎀ Field",
                    7,
                    InsertField,
                    "Insert a field — date, page, author, …",
                )],
                vec![btn("⧉ Quick Parts", 13, Todo("Quick Parts"), "Quick parts")],
            ],
        },
        Group {
            title: "Symbols",
            width: 13,
            rows: [
                vec![btn(
                    "─ Horiz. Line",
                    13,
                    HorizontalLine,
                    "Insert a horizontal line (or type --- then Enter)",
                )],
                vec![btn("Ω Symbol", 8, Todo("Symbol"), "Insert a symbol")],
            ],
        },
    ]
}

fn review_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Comments",
            width: 25,
            rows: [
                vec![
                    btn("✎ New", 5, NewComment, "New comment on the selection"),
                    Seg::Gap("  "),
                    btn("✗ Delete", 8, DeleteComment, "Delete the selected comment"),
                ],
                vec![
                    btn("‹ Prev", 6, PrevComment, "Previous comment"),
                    Seg::Gap(" "),
                    btn("Next ›", 6, NextComment, "Next comment"),
                    Seg::Gap("  "),
                    // a plain toggle — inverts when the panel is on
                    btn(
                        "▤ Comments",
                        10,
                        ToggleComments,
                        "Show/hide the comments panel",
                    ),
                ],
            ],
        },
        Group {
            title: "Tracking",
            width: 8,
            rows: [
                vec![btn(
                    "Track ▾",
                    7,
                    Todo("Track Changes"),
                    "Track Changes (Ctrl+Shift+E)",
                )],
                vec![btn(
                    "Markup ▾",
                    8,
                    Todo("Display for Review"),
                    "Display for review",
                )],
            ],
        },
    ]
}

/// The View tab's groups (Views / Page / Show / Ribbon / Edit, plus a contextual
/// Markdown group when a `.md` file is open).
fn view_groups(markdown: bool) -> Vec<Group> {
    use Act::*;
    let mut groups = vec![
        Group {
            title: "Views",
            width: 5,
            rows: [
                vec![btn(
                    "Read",
                    4,
                    ReadMode,
                    "Read Mode — reflowed reading view",
                )],
                vec![btn(
                    "Print",
                    5,
                    PrintLayout,
                    "Print Layout — pages with margins & headers",
                )],
            ],
        },
        Group {
            title: "Page",
            width: 6,
            rows: [
                vec![btn(
                    "☀ Mode",
                    6,
                    DarkMode,
                    "Dark Mode — switch the page between dark and light",
                )],
                vec![],
            ],
        },
        Group {
            title: "Show",
            width: 8,
            rows: [
                vec![btn("Ruler", 5, ToggleRuler, "Show the column ruler")],
                vec![btn(
                    "Nav Pane",
                    8,
                    ToggleNav,
                    "Navigation pane — jump to a heading",
                )],
            ],
        },
        Group {
            title: "Ribbon",
            width: 9,
            rows: [
                vec![btn(
                    "Auto Hide",
                    9,
                    AutoHideRibbon,
                    "Auto-hide the ribbon — collapse to tabs after each use",
                )],
                vec![],
            ],
        },
        Group {
            title: "Edit",
            width: 16,
            rows: [
                vec![
                    btn("Document", 8, EditDocument, "Edit the document body"),
                    Seg::Gap("  "),
                    btn("Header", 6, EditHeader, "Edit the page header (F6)"),
                ],
                vec![btn("Footer", 6, EditFooter, "Edit the page footer (F7)")],
            ],
        },
    ];
    if markdown {
        groups.push(Group {
            title: "Markdown",
            width: 8,
            rows: [
                vec![btn(
                    "Rendered",
                    8,
                    MdRendered,
                    "Show the document rendered (formatted)",
                )],
                vec![btn(
                    "Source",
                    6,
                    MdSource,
                    "Edit the raw Markdown source text",
                )],
            ],
        });
    }
    groups
}

impl Ribbon {
    /// Compute placed-button rects and total width from the group definitions.
    fn layout(&mut self) {
        self.placed.clear();
        // Each group cell is " " + content(width) + " " between │ borders.
        let mut gx = 1u16; // after the left "│"
        let active = self.active;
        for g in &self.tab_groups[active] {
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
    pub fn tab_label(&self, i: usize) -> Option<&'static str> {
        self.tabs.get(i).copied()
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

    /// Render the collapsed ribbon: just the tab strip (one line). The active tab
    /// is only highlighted (inverted) while the ribbon is engaged; when it's idle
    /// the tabs are drawn plain.
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
                Style::default() // idle: plain, nothing inverted
            } else if i == self.active {
                Style::default().fg(Color::Black).bg(Color::White) // active tab
            } else if Some(i) == focused_tab {
                Style::default().add_modifier(Modifier::REVERSED) // keyboard cursor
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(t.to_string(), style));
            spans.push(Span::raw("   "));
        }
        Line::from(spans)
    }

    /// Render the tab strip with `active_tab` drawn as the selected (highlighted)
    /// tab and the rest dimmed. Used by the File backstage so the File header
    /// looks selected while the other tabs stay visible and clickable (their
    /// columns match [`Ribbon::hit`]).
    pub fn render_tabs_as(&self, active_tab: usize) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        for (i, t) in self.tabs.iter().enumerate() {
            let style = if i == active_tab {
                Style::default().fg(Color::Black).bg(Color::White)
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
        for g in self.groups() {
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
                    let is_on = self.active_toggles.contains(&b.act);
                    let style = if is_focus {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else if is_on {
                        // a toggle that's on: invert fg/bg so it's obviously active
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if b.act.enabled() {
                        Style::default()
                    } else {
                        Style::default().add_modifier(Modifier::DIM)
                    };
                    // Dark Mode shows the sun on a dark page, the moon on a light one.
                    let glyph = if b.act == Act::DarkMode {
                        if self.light_page {
                            "☾ Mode"
                        } else {
                            "☀ Mode"
                        }
                    } else {
                        b.glyph
                    };
                    out.push(Span::styled(glyph.to_string(), style));
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
    fn every_group_is_wide_enough_for_its_content() {
        // A group whose declared `width` is narrower than its widest row overflows
        // the next "│" border (this once broke the View ▸ Edit group's right edge).
        let mut r = Ribbon::home();
        r.set_markdown(true); // also exercise the contextual View ▸ Markdown group
        for tab in 0..r.tabs.len() {
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
                assert!(
                    g.width >= widest,
                    "group {:?} width {} < content {}",
                    g.title,
                    g.width,
                    widest
                );
            }
        }
    }

    #[test]
    fn live_and_dimmed_buttons_are_classified() {
        let r = Ribbon::home();
        assert!(Act::Bold.enabled());
        assert!(Act::AlignLeft.enabled());
        assert!(!Act::Todo("Bullets").enabled());
        assert!(r.button_count() > 20);
    }

    #[test]
    fn an_on_toggle_button_is_drawn_inverted() {
        let mut r = Ribbon::home();
        r.set_active(5); // View tab has Read/Print toggles
        assert_eq!(r.tab_label(5), Some("View"));
        // nothing inverted yet
        let plain = r.render_body(Focus::None);
        let any_rev = |ls: &[Line]| {
            ls.iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        };
        assert!(!any_rev(&plain));
        // mark Read Mode on → its button inverts
        r.set_toggles(vec![Act::ReadMode]);
        assert!(any_rev(&r.render_body(Focus::None)));
    }

    #[test]
    fn dark_mode_icon_flips_with_the_page() {
        let mut r = Ribbon::home();
        r.set_active(5);
        let body = |r: &Ribbon| {
            r.render_body(Focus::None)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.clone())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        r.set_light_page(false);
        assert!(body(&r).contains('☀')); // dark page → sun
        r.set_light_page(true);
        assert!(body(&r).contains('☾')); // light page → moon
    }

    #[test]
    fn clicking_the_home_tab_is_detected() {
        let r = Ribbon::home();
        // tab 0 = File, tab 1 = Home
        assert_eq!(r.tab_label(0), Some("File"));
        let (a, b) = r.tab_cols[1];
        assert!(matches!(r.hit((a + b) / 2, 0, false), Hit::Tab(1)));
    }

    #[test]
    fn down_from_tabs_enters_first_button_then_up_returns() {
        let r = Ribbon::home();
        let f = r.nav(Focus::Tab(1), Dir::Down);
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
        assert!(matches!(r.focus_act(down), Some((Act::PasteSpecial, _))));
        let hint = r.focus_hint(down).unwrap_or("");
        assert!(
            hint.contains("Paste Special"),
            "Down from Paste should reach the Paste Special dropdown, got: {hint}"
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
