//! docxy's ribbon: its command set (`Act`), tab/button data, light-blue accent,
//! the Styles list, and the Markdown/Dark-Mode context — all rendered and
//! navigated by the shared [`ribboncore`] crate. The wrapper `Ribbon` derefs to
//! `ribboncore::Ribbon<Act>`; only docxy's own bits (markdown-context swap,
//! Dark-Mode sun/moon glyph) live here.

use ratatui::style::Color;
use ribboncore::{Ribbon as CoreRibbon, Seg};

pub use ribboncore::{Dir, EXPANDED_H, Focus, Hit};

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
    /// Toggle the footnotes/endnotes side panel.
    ToggleNotes,
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

type Group = ribboncore::Group<Act>;

/// docxy's ribbon accent — the whole ribbon draws light blue (lookxy cyan,
/// xlsxy green, yppxy yellow).
const ACCENT: Color = Color::LightBlue;
const HOME_TAB: usize = 1;
const VIEW_TAB: usize = 5;

/// A focusable button (docxy hand-counts widths — some glyphs render one column
/// where unicode-width reports two).
fn btn(glyph: &'static str, width: usize, act: Act, hint: &'static str) -> Seg<Act> {
    ribboncore::btn(glyph, width, act, hint)
}

/// docxy's ribbon — a thin wrapper over the shared core, tracking the Markdown
/// context so it can swap the Home/View groups.
pub struct Ribbon {
    inner: CoreRibbon<Act>,
    markdown: bool,
}

impl Ribbon {
    pub fn home() -> Ribbon {
        let tabs = vec!["File", "Home", "Styles", "Insert", "Review", "View"];
        let tab_groups = vec![
            Vec::new(), // File → backstage
            home_groups(false),
            styles_groups(),
            insert_groups(),
            review_groups(),
            view_groups(false),
        ];
        Ribbon {
            inner: CoreRibbon::new(tabs, tab_groups, HOME_TAB, ACCENT),
            markdown: false,
        }
    }

    /// Show or hide the contextual View ▸ Markdown group (and drop the
    /// Markdown-incapable Home formatting controls). A no-op when unchanged.
    pub fn set_markdown(&mut self, on: bool) {
        if self.markdown == on {
            return;
        }
        self.markdown = on;
        self.inner.set_tab_groups(HOME_TAB, home_groups(on));
        self.inner.set_tab_groups(VIEW_TAB, view_groups(on));
    }

    /// Flip the Dark-Mode button's glyph: a sun on a dark page, a moon on a
    /// light one.
    pub fn set_light_page(&mut self, on: bool) {
        self.inner
            .set_glyph_override(Act::DarkMode, if on { "☾ Mode" } else { "☀ Mode" });
    }
}

impl std::ops::Deref for Ribbon {
    type Target = CoreRibbon<Act>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl std::ops::DerefMut for Ribbon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

fn home_groups(markdown: bool) -> Vec<Group> {
    use Act::*;
    let mut groups = vec![
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
    ];
    // Markdown can't express most character/paragraph formatting, so the Home tab
    // keeps only the controls that map to Markdown: bold/italic/strike and lists.
    if markdown {
        groups[1] = Group {
            title: "Font",
            width: 9,
            rows: [
                vec![
                    btn("B", 1, Bold, "Bold (Ctrl+B) — **text**"),
                    Seg::Gap("  "),
                    btn("I", 1, Italic, "Italic (Ctrl+I) — *text*"),
                    Seg::Gap("  "),
                    btn("S", 1, Strike, "Strikethrough — ~~text~~"),
                ],
                vec![],
            ],
        };
        groups[2] = Group {
            title: "Paragraph",
            width: 11,
            rows: [
                vec![
                    btn("•──", 3, Bullets, "Bullets — bulleted list"),
                    Seg::Gap(" "),
                    btn("1──", 3, Numbering, "Numbering — numbered list"),
                ],
                vec![btn(
                    "¶",
                    1,
                    ShowHide,
                    "Show/hide formatting marks (Ctrl+Shift+8)",
                )],
            ],
        };
    }
    groups
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
            width: 35,
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
                    Seg::Gap("  "),
                    btn(
                        "▤ Notes",
                        7,
                        ToggleNotes,
                        "Show/hide the footnotes/endnotes panel",
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
        // Markdown has no fixed pages, so drop the Views (Read / Print Layout)
        // group and offer the Rendered / Source switch instead.
        groups.remove(0);
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
#[cfg(test)]
mod tests {
    use super::*;
    use ribboncore::Seg;

    fn content_w(row: &[Seg<Act>]) -> usize {
        row.iter()
            .map(|s| match s {
                Seg::Gap(g) => g.chars().count(),
                Seg::Btn(b) => b.width,
            })
            .sum()
    }

    #[test]
    fn every_group_is_wide_enough_for_its_content() {
        for md in [false, true] {
            let tabs = [
                home_groups(md),
                styles_groups(),
                insert_groups(),
                review_groups(),
                view_groups(md),
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
    fn constructs_hits_and_navigates() {
        let r = Ribbon::home();
        assert_eq!(r.tab_label(0), Some("File"));
        assert!(!r.tab_has_body(0)); // File → backstage
        assert!(r.button_count() > 0);
        assert!(matches!(r.hit(2, 0, false), Hit::Tab(0)));
        let f = r.nav(Focus::Tab(1), Dir::Down);
        assert!(matches!(f, Focus::Button(_)));
        assert!(matches!(r.nav(f, Dir::Up), Focus::Tab(1)));
    }

    #[test]
    fn dark_mode_glyph_follows_the_page() {
        let mut r = Ribbon::home();
        r.set_active(VIEW_TAB);
        let body = |r: &Ribbon| -> String {
            r.render_body(Focus::None)
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.clone())
                .collect()
        };
        r.set_light_page(false);
        assert!(body(&r).contains('☀')); // dark page → sun
        r.set_light_page(true);
        assert!(body(&r).contains('☾')); // light page → moon
    }

    #[test]
    fn markdown_context_swaps_the_view_group() {
        let mut r = Ribbon::home();
        r.set_active(VIEW_TAB);
        r.set_markdown(true);
        // The Markdown source/rendered buttons appear.
        assert!(r.has_act(Act::MdSource) || r.has_act(Act::MdRendered));
    }
}
