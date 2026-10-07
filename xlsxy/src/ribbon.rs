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
    /// Home › Paste Special (Ctrl+Alt+V, #669).
    PasteSpecial,
    Undo,
    Redo,
    Find,
    Replace,
    GoTo,
    ClearContents,
    FillDown,
    FillRight,
    /// Home › Fill › Up and Left (#668).
    FillUp,
    FillLeft,
    InsertRow,
    InsertCol,
    DeleteRow,
    DeleteCol,
    SortAsc,
    SortDesc,
    CustomSort,
    AutoSum,
    InsertChart(&'static str),
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
    WrapText,
    RowHeight,
    NumberFormat,
    FontColor,
    FillColor,
    MergeCenter,
    CondFormat,
    DataValidation,
    CircleInvalid,
    ClearCircles,
    /// Data › Sort & Filter.
    Filter,
    ClearFilter,
    ReapplyFilter,
    AdvancedFilter,
    FilterByCell,
    RemoveDuplicates,
    TextToColumns,
    FormatAsTable,
    /// Table Design commands on the table under the cursor.
    TableName,
    ResizeTable,
    ConvertToRange,
    /// Data ▸ Data Tools ▸ Consolidate.
    Consolidate,
    /// Data ▸ Outline.
    Subtotal,
    /// Data ▸ Form…: Excel's data form over the list at the cursor.
    DataForm,
    GroupOutline,
    UngroupOutline,
    ShowDetail,
    HideDetail,
    AutoOutline,
    ClearOutline,
    OutlineSettings,
    /// Review ▸ Comments.
    NewComment,
    NewNote,
    DeleteComment,
    PrevComment,
    NextComment,
    ToggleComments,
    ProtectSheet,
    /// View toggles.
    FormulaView,
    FreezePanes,
    ShowHidden,
    ShowObjects,
    ThemeToggle,
    AutoHideRibbon,
    /// Help tab (#1021): Help says the documentation is coming; Contact
    /// Support and Feedback open the GitHub new-issue page with this build;
    /// About shows File › Info, where every build field is.
    Help,
    ContactSupport,
    Feedback,
    About,
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
        let tabs = vec!["File", "Home", "Insert", "Data", "Review", "View", "Help"];
        let tab_groups = vec![
            Vec::new(), // File → backstage
            home_groups(),
            insert_groups(),
            data_groups(),
            review_groups(),
            view_groups(),
            help_groups(),
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
                vec![
                    btn("Paste", Paste, "Paste (Ctrl+V)"),
                    Seg::Gap(" "),
                    btn("Spec…", PasteSpecial, "Paste Special (Ctrl+Alt+V)"),
                ],
                vec![
                    btn("✂ Cut", Cut, "Cut (Ctrl+X)"),
                    Seg::Gap(" "),
                    btn("⧉", Copy, "Copy (Ctrl+C)"),
                ],
            ],
        },
        Group {
            title: "Cells",
            width: 28,
            rows: [
                vec![
                    btn("+Row", InsertRow, "Insert rows above the selection"),
                    Seg::Gap(" "),
                    btn("+Col", InsertCol, "Insert columns left of the selection"),
                    Seg::Gap(" "),
                    btn("Fill↓", FillDown, "Fill down (Ctrl+D)"),
                    Seg::Gap(" "),
                    btn("Row Ht", RowHeight, "Set the row height in points"),
                ],
                vec![
                    btn("−Row", DeleteRow, "Delete the selected rows"),
                    Seg::Gap(" "),
                    btn("−Col", DeleteCol, "Delete the selected columns"),
                    Seg::Gap(" "),
                    btn("Fill→", FillRight, "Fill right (Ctrl+R)"),
                    Seg::Gap(" "),
                    btn("Fill↑", FillUp, "Fill up"),
                    Seg::Gap(" "),
                    btn("Fill←", FillLeft, "Fill left"),
                ],
            ],
        },
        Group {
            title: "Font",
            width: 28,
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
                    Seg::Gap(" "),
                    btn(
                        "Merge",
                        MergeCenter,
                        "Merge & Center the selection (toggle)",
                    ),
                    Seg::Gap(" "),
                    btn("Wrap", WrapText, "Wrap text within the cell (toggle)"),
                ],
            ],
        },
        Group {
            title: "Number",
            width: 8,
            rows: [
                vec![btn("Format ▾", NumberFormat, "Number format…")],
                vec![btn(
                    "Cond Fmt",
                    CondFormat,
                    "Conditional formatting (highlight cells)",
                )],
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
            title: "Data",
            width: 20,
            rows: [
                vec![
                    btn("↑ Sort", SortAsc, "Sort A->Z by the current column"),
                    Seg::Gap(" "),
                    btn("↓ Sort", SortDesc, "Sort Z->A by the current column"),
                ],
                vec![
                    btn("Σ Sum", AutoSum, "Sum the numbers above/left"),
                    Seg::Gap(" "),
                    btn("⇅ Sort…", CustomSort, "Multi-level sort (B asc, C desc)"),
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
                vec![btn(
                    "▦ Table",
                    FormatAsTable,
                    "Format the region as an Excel Table",
                )],
                vec![btn("PivotTable", Todo("PivotTable"), "Insert a PivotTable")],
            ],
        },
        Group {
            title: "Table",
            width: 24,
            rows: [
                vec![
                    btn("✎ Name", TableName, "Rename the table under the cursor"),
                    btn("⤡ Resize", ResizeTable, "Resize the table under the cursor"),
                ],
                vec![btn(
                    "Convert to Range",
                    ConvertToRange,
                    "Turn the table under the cursor into plain cells",
                )],
            ],
        },
        Group {
            title: "Charts",
            width: 18,
            rows: [
                vec![btn(
                    "▊ Column",
                    InsertChart("column"),
                    "Insert a column chart from the selection",
                )],
                vec![btn(
                    "▬ Bar",
                    InsertChart("bar"),
                    "Insert a bar chart from the selection",
                )],
            ],
        },
    ]
}

/// Excel's Data tab: Sort & Filter, the data tools that were on Insert, and
/// the outline.
fn data_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Sort & Filter",
            width: 34,
            rows: [
                vec![
                    btn("↑ A→Z", SortAsc, "Sort A to Z by the current column"),
                    Seg::Gap(" "),
                    btn("↓ Z→A", SortDesc, "Sort Z to A by the current column"),
                    Seg::Gap(" "),
                    btn(
                        "⇅ Sort…",
                        CustomSort,
                        "Sort by levels, custom lists, colour or icon",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "By Cell…",
                        FilterByCell,
                        "Filter by the selected cell's value, colour, font colour or icon",
                    ),
                ],
                vec![
                    btn(
                        "Filter ▾",
                        Filter,
                        "Filter buttons on/off (Ctrl+Shift+L); Alt+Down on a header opens one",
                    ),
                    Seg::Gap(" "),
                    btn("Clear", ClearFilter, "Clear the filter: show every row"),
                    Seg::Gap(" "),
                    btn("Reapply", ReapplyFilter, "Apply the filter again"),
                    Seg::Gap(" "),
                    btn(
                        "Advanced…",
                        AdvancedFilter,
                        "Filter with a criteria range, in place or to a copy",
                    ),
                ],
            ],
        },
        Group {
            title: "Data Tools",
            width: 30,
            rows: [
                vec![
                    btn(
                        "Validation…",
                        DataValidation,
                        "Data Validation: rule, input message and error alert",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "Circle",
                        CircleInvalid,
                        "Circle Invalid Data: circle cells whose value breaks its rule",
                    ),
                    Seg::Gap(" "),
                    btn("Clear ○", ClearCircles, "Clear Validation Circles"),
                ],
                vec![
                    btn(
                        "Remove Dup",
                        RemoveDuplicates,
                        "Remove duplicate rows in the region",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "Split",
                        TextToColumns,
                        "Text to Columns: split by a delimiter",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "Consolidate…",
                        Consolidate,
                        "Combine ranges from several sheets into one, by position or label",
                    ),
                ],
            ],
        },
        Group {
            title: "Outline",
            width: 26,
            rows: [
                vec![
                    btn(
                        "⊞ Group",
                        GroupOutline,
                        "Group rows or columns (Alt+Shift+Right)",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "⊟ Ungroup",
                        UngroupOutline,
                        "Ungroup rows or columns (Alt+Shift+Left)",
                    ),
                ],
                vec![
                    btn("+ Show", ShowDetail, "Show Detail: expand the group here"),
                    Seg::Gap(" "),
                    btn("- Hide", HideDetail, "Hide Detail: collapse the group here"),
                ],
            ],
        },
        Group {
            title: "Subtotal",
            width: 12,
            rows: [
                vec![btn(
                    "Σ Subtotal…",
                    Subtotal,
                    "Insert subtotals (or Remove All) at each change in a column",
                )],
                vec![btn(
                    "≣ Form…",
                    DataForm,
                    "Data Form: view, edit, add, delete and find records",
                )],
            ],
        },
        Group {
            title: "Auto",
            width: 26,
            rows: [
                vec![
                    btn(
                        "Auto Outline",
                        AutoOutline,
                        "Build the outline from summary formulas",
                    ),
                    Seg::Gap(" "),
                    btn("Clear", ClearOutline, "Clear Outline: remove every group"),
                ],
                vec![btn(
                    "Settings…",
                    OutlineSettings,
                    "Summary rows below detail, summary columns to the right",
                )],
            ],
        },
    ]
}

fn review_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
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
        },
        Group {
            title: "Protect",
            width: 15,
            rows: [
                vec![btn(
                    "🔒 Protect",
                    ProtectSheet,
                    "Protect/unprotect the sheet (make cells read-only)",
                )],
                vec![],
            ],
        },
    ]
}

/// The Help tab every editor ends with (#1021): Show Training and What's New
/// have nothing to show yet, so they are drawn dimmed.
fn help_groups() -> Vec<Group> {
    use Act::*;
    vec![
        Group {
            title: "Help",
            width: 27,
            rows: [
                vec![
                    btn("Help", Help, "Help — the documentation is coming"),
                    Seg::Gap(" "),
                    btn(
                        "Feedback",
                        Feedback,
                        "Feedback — a GitHub issue with this build filled in",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "Show Training",
                        Todo("Show Training"),
                        "Show Training — coming later",
                    ),
                ],
                vec![
                    btn(
                        "Contact Support",
                        ContactSupport,
                        "Contact Support — a GitHub issue with this build filled in",
                    ),
                    Seg::Gap(" "),
                    btn(
                        "What's New",
                        Todo("What's New"),
                        "What's New — coming later",
                    ),
                ],
            ],
        },
        Group {
            title: "About",
            width: 11,
            rows: [
                vec![btn(
                    "About xlsxy",
                    About,
                    "About xlsxy — this build's details",
                )],
                vec![],
            ],
        },
    ]
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
            data_groups(),
            review_groups(),
            view_groups(),
            help_groups(),
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
    fn help_is_the_last_tab() {
        let mut r = Ribbon::new();
        assert_eq!(r.tab_label(6), Some("Help"));
        assert_eq!(r.tab_label(7), None);
        r.set_active(6);
        for act in [Act::Help, Act::ContactSupport, Act::Feedback, Act::About] {
            assert!(r.has_act(act), "{act:?}");
        }
        assert!(r.has_act(Act::Todo("Show Training")));
        assert!(r.has_act(Act::Todo("What's New")));
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
