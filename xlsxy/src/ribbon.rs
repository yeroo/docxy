//! xlsxy's ribbon: its command set (`Act`), tab/button data, green accent, and
//! dispatch — all rendered/navigated by the shared [`ribboncore`] crate. The
//! wrapper `Ribbon` derefs to `ribboncore::Ribbon<Act>` so every call site uses
//! the core API directly.

use ratatui::style::Color;
use ribboncore::{Ribbon as CoreRibbon, Seg};
use unicode_width::UnicodeWidthStr;

pub use ribboncore::{Dir, EXPANDED_H, Focus, Hit};

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

type Group = ribboncore::Group<Act>;

/// xlsxy's ribbon accent — the whole ribbon draws green (lookxy cyan, docxy
/// light blue, yppxy yellow).
const ACCENT: Color = Color::Green;

/// A focusable button; width is the glyph's display width (some glyphs are two
/// columns wide, so this is computed rather than hand-counted).
fn btn(glyph: &'static str, act: Act, hint: &'static str) -> Seg<Act> {
    ribboncore::btn(glyph, glyph.width(), act, hint)
}

/// xlsxy's ribbon — a thin wrapper over the shared core.
pub struct Ribbon(CoreRibbon<Act>);

impl Ribbon {
    pub fn new() -> Ribbon {
        let tabs = vec!["File", "Home", "Insert", "Review", "View"];
        let tab_groups = vec![
            Vec::new(), // File → backstage
            home_groups(),
            insert_groups(),
            review_groups(),
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
        for groups in [
            home_groups(),
            insert_groups(),
            review_groups(),
            view_groups(),
        ] {
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
