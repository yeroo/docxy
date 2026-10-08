//! The spreadsheet ribbon as data (#699).
//!
//! The document and Project ribbons come from a declarative definition that
//! both the renderer and the UI test harness read. The sheet ribbon used to be
//! drawn by hand, button by button, so the harness had nothing to read and
//! refused sheet tabs. This table is now the only place a sheet button exists:
//! `Docxy::sheet_ribbon_body` draws exactly what is listed here, in this order,
//! and `ribbon-read` / `ribbon-click` report and resolve the same entries. A
//! button cannot be drawn without being listed, or listed without being drawn.
//!
//! Layout is part of the data only as far as the drawn ribbon needs it: every
//! group is either a strip of buttons and button columns (`Body::Strip`) or a
//! stack of small-button rows (`Body::Rows`), and each keeps the spacing it was
//! drawn with before it became a table.
//!
//! The ribbon body fits three small rows. A column or a rows stack is built only
//! through [`col`] and [`rows`], `const fn`s that assert `len <= 3`: a table
//! with a fourth row does not compile (#1018).

use crate::help_tab::HelpAct;
use crate::sheet_menus::SheetMenu;
use crate::sheet_page_setup::{AreaOp, BreakOp, MarginPreset, PageAct, SetupTab};
use crate::{RibbonTab, SheetAct, SumFn};

/// A flex gap, in the unit the hand-drawn ribbon used for it (`gap_1` is a
/// rem quarter, `gap(px(1.))` is a pixel), so the table draws the same pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Gap {
    Px(f32),
    Rem(f32),
}

/// How a command is drawn.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Shape {
    /// Icon (optional) over word-wrapped text.
    Large(Option<&'static str>),
    /// Small icon (optional) beside text.
    Row(Option<&'static str>),
    /// Icon only; draws the command's on-state.
    Icon(&'static str),
    /// A text glyph only.
    Glyph(&'static str),
    /// An inert combo box showing a fixed value.
    Combo { value: &'static str, wide: bool },
    /// The Number group's format combo, showing the selection's format.
    NumFmt,
    /// A Large button that opens a drop-down (Sort & Filter); its items are
    /// the group's [`Dropdown`].
    Menu(Option<&'static str>),
    /// A check box beside its text, checked from the sheet's state
    /// (`Docxy::sheet_act_checked`): Page Layout › Sheet Options (#1019).
    Check,
    /// A Large split button: the icon and text run the command, the arrow
    /// under them opens `menu` (Home › Paste and its gallery, #707).
    Split {
        icon: Option<&'static str>,
        menu: crate::sheet_menus::SheetMenu,
    },
}

/// One sheet ribbon command.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SheetCmd {
    /// Unique across the whole sheet ribbon; what `ribbon-click` takes first.
    pub id: &'static str,
    /// The command's name (Excel's, for icon and glyph buttons).
    pub label: &'static str,
    /// The text drawn on a Large or Row button, when it is not `label`.
    pub text: Option<&'static str>,
    /// The label (and drawn text) while the command's state is on:
    /// Unfreeze Panes, Unprotect Sheet.
    pub alt: Option<&'static str>,
    pub shape: Shape,
    pub act: SheetAct,
}

impl SheetCmd {
    /// The label as it reads now; `toggled` is the command's state.
    pub fn label(&self, toggled: bool) -> &'static str {
        match self.alt {
            Some(alt) if toggled => alt,
            _ => self.label,
        }
    }

    /// The text drawn on the button as it reads now.
    pub fn text(&self, toggled: bool) -> &'static str {
        match self.alt {
            Some(alt) if toggled => alt,
            _ => self.text.unwrap_or(self.label),
        }
    }

    /// Whether the command does anything yet. `Todo` buttons are drawn, but
    /// clicking one is a no-op, so the harness lists them disabled. A
    /// `Shape::Menu` button is enabled although its act is `Todo`: pressing it
    /// opens its menu, and the items carry the acts.
    pub fn enabled(&self) -> bool {
        match self.act {
            SheetAct::Todo => matches!(self.shape, Shape::Menu(_)),
            SheetAct::Help(act) => crate::help_tab::help_enabled(act),
            _ => true,
        }
    }
}

/// A group's content.
pub(crate) enum Body {
    /// Buttons and button columns side by side.
    Strip { gap: Gap, items: &'static [Item] },
    /// Rows of small buttons, stacked (built by [`rows`]).
    Rows(RowStack),
}

/// The most small-button rows a column or a rows stack holds: what the 100 px
/// ribbon body fits.
pub(crate) const MAX_COL_ROWS: usize = ribbonspec::MAX_COLUMN_ROWS;

/// One slot of a `Body::Strip`.
pub(crate) enum Item {
    One(SheetCmd),
    /// A column of small buttons (built by [`col`]).
    Col(Column),
    /// A Large drop-down button and its menu (built by [`menu`]).
    Menu(Dropdown),
}

/// A button column of at most [`MAX_COL_ROWS`] rows. The fields are readable;
/// the only way to make one is [`col`].
pub(crate) struct Column {
    pub gap: Gap,
    pub cmds: &'static [SheetCmd],
    _built_by_col: (),
}

/// A stack of at most [`MAX_COL_ROWS`] rows of small buttons; made by [`rows`].
pub(crate) struct RowStack {
    pub rows: &'static [&'static [SheetCmd]],
    _built_by_rows: (),
}

/// A Large drop-down button and the items of its menu; made by [`menu`].
pub(crate) struct Dropdown {
    pub button: SheetCmd,
    pub items: &'static [SheetCmd],
    _built_by_menu: (),
}

/// A button column. A fourth row fails the build where the table is a `const`.
pub(crate) const fn col(gap: Gap, cmds: &'static [SheetCmd]) -> Item {
    assert!(
        cmds.len() <= MAX_COL_ROWS,
        "a ribbon column holds at most 3 rows"
    );
    Item::Col(Column {
        gap,
        cmds,
        _built_by_col: (),
    })
}

/// A stack of small-button rows; as [`col`], at most three.
pub(crate) const fn rows(rows: &'static [&'static [SheetCmd]]) -> Body {
    assert!(
        rows.len() <= MAX_COL_ROWS,
        "a ribbon rows stack holds at most 3 rows"
    );
    Body::Rows(RowStack {
        rows,
        _built_by_rows: (),
    })
}

/// A Large drop-down: a button named `label` opens a menu of `items`. Only
/// here is a `Shape::Menu` command made.
pub(crate) const fn menu(
    id: &'static str,
    label: &'static str,
    icon: Option<&'static str>,
    items: &'static [SheetCmd],
) -> Item {
    Item::Menu(Dropdown {
        button: cmd(id, label, Shape::Menu(icon), SheetAct::Todo),
        items,
        _built_by_menu: (),
    })
}

pub(crate) struct Group {
    pub title: &'static str,
    /// The order groups shrink as the window narrows (#1020): the lowest
    /// goes icon-only, then collapses, first. As the document ribbon's
    /// `ribbonspec::Group::priority`, the leftmost groups are kept longest.
    pub priority: u8,
    /// Draws the dialog-launcher glyph beside the title.
    pub launcher: bool,
    /// What the launcher runs; `None` draws it inert.
    pub launch: Option<SheetAct>,
    pub body: Body,
}

pub(crate) struct Tab {
    pub tab: RibbonTab,
    pub titles: Titles,
    pub groups: &'static [Group],
}

/// How a tab draws its group titles. Home's title row makes room for the
/// dialog launchers; the other tabs never had one and center plain text. The
/// two lay out a pixel apart, so the difference is kept rather than unified.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Titles {
    WithLaunchers,
    Plain,
}

impl Group {
    /// The group's commands in drawn order; a drop-down's items follow its
    /// button.
    pub fn commands(&self) -> Vec<&'static SheetCmd> {
        match &self.body {
            Body::Strip { items, .. } => {
                let mut out = Vec::new();
                for item in *items {
                    match item {
                        Item::One(cmd) => out.push(cmd),
                        Item::Col(c) => out.extend(c.cmds),
                        Item::Menu(m) => {
                            out.push(&m.button);
                            out.extend(m.items);
                        }
                    }
                }
                out
            }
            Body::Rows(r) => r.rows.iter().flat_map(|r| r.iter()).collect(),
        }
    }

    /// The drop-down whose button is `id`.
    pub fn dropdown(&self, id: &str) -> Option<&'static Dropdown> {
        let Body::Strip { items, .. } = &self.body else {
            return None;
        };
        items.iter().find_map(|item| match item {
            Item::Menu(m) if m.button.id == id => Some(m),
            _ => None,
        })
    }

    /// The drop-down that lists `item`.
    pub fn menu_owner(&self, item: &SheetCmd) -> Option<&'static Dropdown> {
        let Body::Strip { items, .. } = &self.body else {
            return None;
        };
        items.iter().find_map(|i| match i {
            Item::Menu(m) if m.items.iter().any(|c| c.id == item.id) => Some(m),
            _ => None,
        })
    }
}

impl Tab {
    /// The drop-down that lists `item`, in whichever group holds it.
    pub fn menu_owner(&self, item: &SheetCmd) -> Option<&'static Dropdown> {
        self.groups.iter().find_map(|g| g.menu_owner(item))
    }

    /// The tab's commands in drawn order.
    pub fn commands(&self) -> Vec<&'static SheetCmd> {
        self.groups.iter().flat_map(Group::commands).collect()
    }
}

const fn cmd(id: &'static str, label: &'static str, shape: Shape, act: SheetAct) -> SheetCmd {
    SheetCmd {
        id,
        label,
        text: None,
        alt: None,
        shape,
        act,
    }
}

/// An item of a drop-down menu, named as its menu shows it.
const fn menu_item(id: &'static str, label: &'static str, act: SheetAct) -> SheetCmd {
    cmd(id, label, Shape::Row(None), act)
}

/// A menu item whose menu text differs from its name.
const fn menu_item_as(
    id: &'static str,
    label: &'static str,
    text: &'static str,
    act: SheetAct,
) -> SheetCmd {
    SheetCmd {
        text: Some(text),
        ..menu_item(id, label, act)
    }
}

const fn large(
    id: &'static str,
    label: &'static str,
    icon: Option<&'static str>,
    act: SheetAct,
) -> SheetCmd {
    cmd(id, label, Shape::Large(icon), act)
}

const fn row(
    id: &'static str,
    label: &'static str,
    icon: Option<&'static str>,
    act: SheetAct,
) -> SheetCmd {
    cmd(id, label, Shape::Row(icon), act)
}

/// A Row button whose drawn text differs from its name.
const fn row_as(
    id: &'static str,
    label: &'static str,
    text: &'static str,
    icon: Option<&'static str>,
    act: SheetAct,
) -> SheetCmd {
    SheetCmd {
        text: Some(text),
        ..row(id, label, icon, act)
    }
}

const fn icon(
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    act: SheetAct,
) -> SheetCmd {
    cmd(id, label, Shape::Icon(icon), act)
}

const fn glyph(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    act: SheetAct,
) -> SheetCmd {
    cmd(id, label, Shape::Glyph(glyph), act)
}

const fn combo(id: &'static str, label: &'static str, value: &'static str, wide: bool) -> SheetCmd {
    cmd(id, label, Shape::Combo { value, wide }, SheetAct::Todo)
}

/// A check box beside its name.
const fn check(id: &'static str, label: &'static str, act: SheetAct) -> SheetCmd {
    cmd(id, label, Shape::Check, act)
}

/// A Large button that reads `alt` while its state is on.
const fn toggle(
    id: &'static str,
    label: &'static str,
    alt: &'static str,
    icon: Option<&'static str>,
    act: SheetAct,
) -> SheetCmd {
    SheetCmd {
        alt: Some(alt),
        ..large(id, label, icon, act)
    }
}

/// `gap_1`, `gap_0p5`, `gap_2` and the pixel gaps the hand-drawn ribbon used.
const GAP_1: Gap = Gap::Rem(0.25);
const GAP_0P5: Gap = Gap::Rem(0.125);
const GAP_2: Gap = Gap::Rem(0.5);
const COL: Gap = Gap::Px(1.);

/// Every sheet ribbon tab. Home comes first: it is what a tab without an entry
/// of its own draws.
pub(crate) const SHEET_RIBBON: &[Tab] = &[
    Tab {
        tab: RibbonTab::Home,
        titles: Titles::WithLaunchers,
        groups: &[
            Group {
                title: "Clipboard",
                priority: 90,
                launcher: true,
                // The Office Clipboard pane (#669).
                launch: Some(SheetAct::OfficeClipboard),
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(cmd(
                            "paste",
                            "Paste",
                            Shape::Split {
                                icon: Some("paste"),
                                menu: SheetMenu::Paste,
                            },
                            SheetAct::Paste,
                        )),
                        col(
                            COL,
                            &[
                                row("cut", "Cut", Some("cut"), SheetAct::Cut),
                                row("copy", "Copy", Some("copy"), SheetAct::Copy),
                                row("format-painter", "Format Painter", None, SheetAct::Todo),
                            ],
                        ),
                    ],
                },
            },
            Group {
                title: "Font",
                priority: 80,
                launcher: true,
                launch: None,
                body: rows(&[
                    &[
                        combo("font-name", "Font", "Calibri", true),
                        combo("font-size", "Font Size", "11", false),
                        icon(
                            "grow-font",
                            "Increase Font Size",
                            "font-increase",
                            SheetAct::GrowFont,
                        ),
                        icon(
                            "shrink-font",
                            "Decrease Font Size",
                            "font-decrease",
                            SheetAct::ShrinkFont,
                        ),
                    ],
                    &[
                        icon("bold", "Bold", "bold", SheetAct::Bold),
                        icon("italic", "Italic", "italic", SheetAct::Italic),
                        icon("underline", "Underline", "underline", SheetAct::Todo),
                        icon(
                            "borders",
                            "Borders",
                            "border-bottom",
                            SheetAct::ToggleBorder,
                        ),
                        icon("fill-color", "Fill Color", "highlight", SheetAct::FillColor),
                        icon(
                            "font-color",
                            "Font Color",
                            "text-color",
                            SheetAct::FontColor,
                        ),
                    ],
                ]),
            },
            Group {
                title: "Alignment",
                priority: 70,
                launcher: true,
                launch: None,
                body: rows(&[
                    &[
                        glyph("top-align", "Top Align", "\u{2580}", SheetAct::Todo),
                        glyph("middle-align", "Middle Align", "\u{25AC}", SheetAct::Todo),
                        glyph("bottom-align", "Bottom Align", "\u{2584}", SheetAct::Todo),
                        row("wrap-text", "Wrap Text", None, SheetAct::WrapText),
                    ],
                    &[
                        icon("align-left", "Align Left", "align-left", SheetAct::AlignL),
                        icon("center", "Center", "align-center", SheetAct::AlignC),
                        icon(
                            "align-right",
                            "Align Right",
                            "align-right",
                            SheetAct::AlignR,
                        ),
                        icon(
                            "decrease-indent",
                            "Decrease Indent",
                            "indent-decrease",
                            SheetAct::Todo,
                        ),
                        row("row-height", "Row Height", None, SheetAct::RowHeight),
                        row("merge", "Merge", None, SheetAct::Merge),
                    ],
                ]),
            },
            Group {
                title: "Number",
                priority: 60,
                launcher: true,
                launch: None,
                body: rows(&[
                    &[cmd(
                        "number-format",
                        "Number Format",
                        Shape::NumFmt,
                        SheetAct::NumberFormatMenu,
                    )],
                    &[
                        glyph(
                            "currency",
                            "Accounting Number Format",
                            "$",
                            SheetAct::Currency,
                        ),
                        glyph("percent", "Percent Style", "%", SheetAct::Percent),
                        glyph("comma", "Comma Style", ",", SheetAct::Comma),
                        glyph(
                            "increase-decimal",
                            "Increase Decimal",
                            "\u{2192}.0",
                            SheetAct::Todo,
                        ),
                        glyph(
                            "decrease-decimal",
                            "Decrease Decimal",
                            ".00\u{2190}",
                            SheetAct::Todo,
                        ),
                    ],
                ]),
            },
            Group {
                title: "Styles",
                priority: 50,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_0P5,
                    items: &[
                        Item::One(large(
                            "conditional-formatting",
                            "Conditional Formatting",
                            None,
                            SheetAct::CondFormat,
                        )),
                        Item::One(large(
                            "format-as-table",
                            "Format as Table",
                            Some("table"),
                            SheetAct::FormatAsTable,
                        )),
                        Item::One(large("cell-styles", "Cell Styles", None, SheetAct::Todo)),
                    ],
                },
            },
            Group {
                title: "Cells",
                priority: 40,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_2,
                    items: &[
                        col(
                            GAP_0P5,
                            &[
                                row("insert-row", "Insert Row", None, SheetAct::InsertRow),
                                row("insert-col", "Insert Col", None, SheetAct::InsertCol),
                            ],
                        ),
                        col(
                            GAP_0P5,
                            &[
                                row("delete-row", "Delete Row", None, SheetAct::DeleteRow),
                                row("delete-col", "Delete Col", None, SheetAct::DeleteCol),
                            ],
                        ),
                        Item::One(large("format", "Format", None, SheetAct::FormatCells)),
                    ],
                },
            },
            Group {
                title: "Editing",
                priority: 30,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        col(
                            COL,
                            &[
                                row_as(
                                    "autosum",
                                    "AutoSum",
                                    "\u{03A3} AutoSum",
                                    None,
                                    SheetAct::AutoSum,
                                ),
                                row("fill", "Fill", None, SheetAct::Menu(SheetMenu::Fill)),
                                row(
                                    "clear",
                                    "Clear",
                                    Some("clear-format"),
                                    SheetAct::Menu(SheetMenu::Clear),
                                ),
                            ],
                        ),
                        menu(
                            "sort-filter",
                            "Sort & Filter",
                            Some("sort"),
                            &[
                                menu_item("sort-a-z", "Sort A to Z", SheetAct::SortAsc),
                                menu_item("sort-z-a", "Sort Z to A", SheetAct::SortDesc),
                                menu_item_as(
                                    "custom-sort",
                                    "Custom Sort",
                                    "Custom Sort...",
                                    SheetAct::CustomSort,
                                ),
                                menu_item("filter", "Filter", SheetAct::Filter),
                                menu_item("home-clear-filter", "Clear", SheetAct::ClearFilter),
                                menu_item(
                                    "home-reapply-filter",
                                    "Reapply",
                                    SheetAct::ReapplyFilter,
                                ),
                            ],
                        ),
                        Item::One(large(
                            "find-select",
                            "Find & Select",
                            Some("find"),
                            SheetAct::Menu(SheetMenu::FindSelect),
                        )),
                    ],
                },
            },
        ],
    },
    Tab {
        tab: RibbonTab::Insert,
        titles: Titles::Plain,
        groups: &[
            Group {
                title: "Tables",
                priority: 90,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "pivot-table",
                            "PivotTable",
                            Some("table"),
                            SheetAct::InsertPivot,
                        )),
                        Item::One(large(
                            "table",
                            "Table",
                            Some("table"),
                            SheetAct::FormatAsTable,
                        )),
                    ],
                },
            },
            Group {
                title: "Charts",
                priority: 80,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "column-chart",
                            "Column",
                            None,
                            SheetAct::InsertChart("column"),
                        )),
                        Item::One(large(
                            "bar-chart",
                            "Bar",
                            None,
                            SheetAct::InsertChart("bar"),
                        )),
                        Item::One(large(
                            "line-chart",
                            "Line",
                            None,
                            SheetAct::InsertChart("line"),
                        )),
                        Item::One(large(
                            "pie-chart",
                            "Pie",
                            None,
                            SheetAct::InsertChart("pie"),
                        )),
                    ],
                },
            },
        ],
    },
    // Excel's Page Layout tab (#1019): page setup, print area, breaks,
    // scaling and the print options over `gridcore::print`. Themes,
    // Background, Right-to-Left, the view check boxes (the sheet keeps no
    // view state for them) and Arrange wait for their engines.
    Tab {
        tab: RibbonTab::PageLayout,
        // Page Setup, Scale to Fit and Sheet Options launch Page Setup.
        titles: Titles::WithLaunchers,
        groups: &[
            Group {
                title: "Themes",
                priority: 90,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large("themes", "Themes", None, SheetAct::Todo)),
                        col(
                            COL,
                            &[
                                row("theme-colors", "Colors", None, SheetAct::Todo),
                                row("theme-fonts", "Fonts", None, SheetAct::Todo),
                                row("theme-effects", "Effects", None, SheetAct::Todo),
                            ],
                        ),
                    ],
                },
            },
            Group {
                title: "Page Setup",
                priority: 80,
                launcher: true,
                launch: Some(SheetAct::Page(PageAct::Dialog(SetupTab::Page))),
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        menu(
                            "margins",
                            "Margins",
                            None,
                            &[
                                menu_item(
                                    "margins-normal",
                                    "Normal",
                                    SheetAct::Page(PageAct::Margins(MarginPreset::Normal)),
                                ),
                                menu_item(
                                    "margins-wide",
                                    "Wide",
                                    SheetAct::Page(PageAct::Margins(MarginPreset::Wide)),
                                ),
                                menu_item(
                                    "margins-narrow",
                                    "Narrow",
                                    SheetAct::Page(PageAct::Margins(MarginPreset::Narrow)),
                                ),
                                menu_item(
                                    "custom-margins",
                                    "Custom Margins...",
                                    SheetAct::Page(PageAct::Dialog(SetupTab::Margins)),
                                ),
                            ],
                        ),
                        menu(
                            "orientation",
                            "Orientation",
                            None,
                            &[
                                menu_item(
                                    "portrait",
                                    "Portrait",
                                    SheetAct::Page(PageAct::Landscape(false)),
                                ),
                                menu_item(
                                    "landscape",
                                    "Landscape",
                                    SheetAct::Page(PageAct::Landscape(true)),
                                ),
                            ],
                        ),
                        menu(
                            "size",
                            "Size",
                            None,
                            &[
                                menu_item(
                                    "size-letter",
                                    "Letter",
                                    SheetAct::Page(PageAct::Paper(1)),
                                ),
                                menu_item(
                                    "size-tabloid",
                                    "Tabloid",
                                    SheetAct::Page(PageAct::Paper(3)),
                                ),
                                menu_item("size-legal", "Legal", SheetAct::Page(PageAct::Paper(5))),
                                menu_item(
                                    "size-executive",
                                    "Executive",
                                    SheetAct::Page(PageAct::Paper(7)),
                                ),
                                menu_item("size-a3", "A3", SheetAct::Page(PageAct::Paper(8))),
                                menu_item("size-a4", "A4", SheetAct::Page(PageAct::Paper(9))),
                                menu_item("size-a5", "A5", SheetAct::Page(PageAct::Paper(11))),
                                menu_item(
                                    "size-b4",
                                    "B4 (JIS)",
                                    SheetAct::Page(PageAct::Paper(12)),
                                ),
                                menu_item(
                                    "size-b5",
                                    "B5 (JIS)",
                                    SheetAct::Page(PageAct::Paper(13)),
                                ),
                                menu_item(
                                    "more-paper-sizes",
                                    "More Paper Sizes...",
                                    SheetAct::Page(PageAct::Dialog(SetupTab::Page)),
                                ),
                            ],
                        ),
                        menu(
                            "print-area",
                            "Print Area",
                            None,
                            &[
                                menu_item(
                                    "set-print-area",
                                    "Set Print Area",
                                    SheetAct::Page(PageAct::Area(AreaOp::Set)),
                                ),
                                menu_item(
                                    "clear-print-area",
                                    "Clear Print Area",
                                    SheetAct::Page(PageAct::Area(AreaOp::Clear)),
                                ),
                                menu_item(
                                    "add-to-print-area",
                                    "Add to Print Area",
                                    SheetAct::Page(PageAct::Area(AreaOp::Add)),
                                ),
                            ],
                        ),
                        menu(
                            "breaks",
                            "Breaks",
                            None,
                            &[
                                menu_item(
                                    "insert-page-break",
                                    "Insert Page Break",
                                    SheetAct::Page(PageAct::Break(BreakOp::Insert)),
                                ),
                                menu_item(
                                    "remove-page-break",
                                    "Remove Page Break",
                                    SheetAct::Page(PageAct::Break(BreakOp::Remove)),
                                ),
                                menu_item(
                                    "reset-page-breaks",
                                    "Reset All Page Breaks",
                                    SheetAct::Page(PageAct::Break(BreakOp::Reset)),
                                ),
                            ],
                        ),
                        Item::One(large("background", "Background...", None, SheetAct::Todo)),
                        Item::One(large(
                            "print-titles",
                            "Print Titles",
                            None,
                            SheetAct::Page(PageAct::Dialog(SetupTab::Sheet)),
                        )),
                    ],
                },
            },
            Group {
                title: "Scale to Fit",
                priority: 70,
                launcher: true,
                launch: Some(SheetAct::Page(PageAct::Dialog(SetupTab::Page))),
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[col(
                        COL,
                        &[
                            row(
                                "fit-width",
                                "Width:",
                                None,
                                SheetAct::Menu(SheetMenu::FitWidth),
                            ),
                            row(
                                "fit-height",
                                "Height:",
                                None,
                                SheetAct::Menu(SheetMenu::FitHeight),
                            ),
                            row(
                                "fit-scale",
                                "Scale:",
                                None,
                                SheetAct::Menu(SheetMenu::Scale),
                            ),
                        ],
                    )],
                },
            },
            // Excel heads each pair with Gridlines and Headings; a check box
            // here reads as its screentip does.
            Group {
                title: "Sheet Options",
                priority: 60,
                launcher: true,
                launch: Some(SheetAct::Page(PageAct::Dialog(SetupTab::Sheet))),
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        col(
                            COL,
                            &[row(
                                "right-to-left",
                                "Right-to-Left Document",
                                None,
                                SheetAct::Todo,
                            )],
                        ),
                        col(
                            COL,
                            &[
                                check("view-gridlines", "View Gridlines", SheetAct::Todo),
                                check(
                                    "print-gridlines",
                                    "Print Gridlines",
                                    SheetAct::Page(PageAct::PrintGridlines),
                                ),
                            ],
                        ),
                        col(
                            COL,
                            &[
                                check("view-headings", "View Headings", SheetAct::Todo),
                                check(
                                    "print-headings",
                                    "Print Headings",
                                    SheetAct::Page(PageAct::PrintHeadings),
                                ),
                            ],
                        ),
                    ],
                },
            },
            Group {
                title: "Arrange",
                priority: 50,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "bring-forward",
                            "Bring Forward",
                            None,
                            SheetAct::Todo,
                        )),
                        Item::One(large(
                            "send-backward",
                            "Send Backward",
                            None,
                            SheetAct::Todo,
                        )),
                        Item::One(large(
                            "selection-pane",
                            "Selection Pane...",
                            None,
                            SheetAct::Todo,
                        )),
                        col(
                            COL,
                            &[
                                row("arrange-align", "Align", None, SheetAct::Todo),
                                row("arrange-group", "Group", None, SheetAct::Todo),
                                row("arrange-rotate", "Rotate", None, SheetAct::Todo),
                            ],
                        ),
                    ],
                },
            },
        ],
    },
    // Excel's Formulas tab (#1019). AutoSum and its menu run; Insert
    // Function, the function galleries, names (#719), auditing, Show
    // Formulas (#676) and calculation (#674) wait for their engines.
    Tab {
        tab: RibbonTab::Formulas,
        titles: Titles::Plain,
        groups: &[
            Group {
                title: "Function Library",
                priority: 90,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "insert-function",
                            "Insert Function...",
                            None,
                            SheetAct::Todo,
                        )),
                        Item::One(cmd(
                            "formulas-autosum",
                            "AutoSum",
                            Shape::Split {
                                icon: None,
                                menu: SheetMenu::AutoSum,
                            },
                            SheetAct::AutoSumFn(SumFn::Sum),
                        )),
                        Item::One(large(
                            "recently-used",
                            "Recently Used",
                            None,
                            SheetAct::Todo,
                        )),
                        Item::One(large("financial", "Financial", None, SheetAct::Todo)),
                        Item::One(large("logical", "Logical", None, SheetAct::Todo)),
                        Item::One(large("text-functions", "Text", None, SheetAct::Todo)),
                        Item::One(large("date-time", "Date & Time", None, SheetAct::Todo)),
                        col(
                            COL,
                            &[
                                row(
                                    "lookup-reference",
                                    "Lookup & Reference",
                                    None,
                                    SheetAct::Todo,
                                ),
                                row("math-trig", "Math & Trig", None, SheetAct::Todo),
                                row("more-functions", "More Functions", None, SheetAct::Todo),
                            ],
                        ),
                    ],
                },
            },
            Group {
                title: "Defined Names",
                priority: 80,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large("name-manager", "Name Manager", None, SheetAct::Todo)),
                        col(
                            COL,
                            &[
                                row("define-name", "Define Name", None, SheetAct::Todo),
                                row("use-in-formula", "Use in Formula", None, SheetAct::Todo),
                                row(
                                    "create-from-selection",
                                    "Create from Selection...",
                                    None,
                                    SheetAct::Todo,
                                ),
                            ],
                        ),
                    ],
                },
            },
            Group {
                title: "Formula Auditing",
                priority: 70,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        col(
                            COL,
                            &[
                                row("trace-precedents", "Trace Precedents", None, SheetAct::Todo),
                                row("trace-dependents", "Trace Dependents", None, SheetAct::Todo),
                                row("remove-arrows", "Remove Arrows", None, SheetAct::Todo),
                            ],
                        ),
                        col(
                            COL,
                            &[
                                row("show-formulas", "Show Formulas", None, SheetAct::Todo),
                                row("error-checking", "Error Checking", None, SheetAct::Todo),
                                row("evaluate-formula", "Evaluate Formula", None, SheetAct::Todo),
                            ],
                        ),
                        Item::One(large("watch-window", "Watch Window", None, SheetAct::Todo)),
                    ],
                },
            },
            Group {
                title: "Calculation",
                priority: 60,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "calculation-options",
                            "Calculation Options",
                            None,
                            SheetAct::Todo,
                        )),
                        col(
                            COL,
                            &[
                                row("calculate-now", "Calculate Now", None, SheetAct::Todo),
                                row("calculate-sheet", "Calculate Sheet", None, SheetAct::Todo),
                            ],
                        ),
                    ],
                },
            },
        ],
    },
    // Excel's Data tab (#693, #696): Sort & Filter, Data Tools, Outline. Data
    // Validation and Text to Columns moved here from Insert, where they never
    // were in Excel.
    Tab {
        tab: RibbonTab::Data,
        titles: Titles::Plain,
        groups: &[
            // Excel's names; ids prefixed where Home has the same command.
            Group {
                title: "Sort & Filter",
                priority: 90,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        col(
                            COL,
                            &[
                                row_as(
                                    "data-sort-a-z",
                                    "Sort A to Z",
                                    "Sort A \u{2192} Z",
                                    Some("sort"),
                                    SheetAct::SortAsc,
                                ),
                                row_as(
                                    "data-sort-z-a",
                                    "Sort Z to A",
                                    "Sort Z \u{2192} A",
                                    Some("sort"),
                                    SheetAct::SortDesc,
                                ),
                            ],
                        ),
                        Item::One(large(
                            "data-sort",
                            "Sort",
                            Some("sort"),
                            SheetAct::CustomSort,
                        )),
                        Item::One(large("data-filter", "Filter", None, SheetAct::Filter)),
                        col(
                            COL,
                            &[
                                row("clear-filter", "Clear", None, SheetAct::ClearFilter),
                                row("reapply-filter", "Reapply", None, SheetAct::ReapplyFilter),
                                row(
                                    "advanced-filter",
                                    "Advanced",
                                    None,
                                    SheetAct::AdvancedFilter,
                                ),
                            ],
                        ),
                    ],
                },
            },
            Group {
                title: "Data Tools",
                priority: 80,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "text-to-columns",
                            "Text to Columns",
                            None,
                            SheetAct::TextToColumns,
                        )),
                        // Flash Fill (#666).
                        col(
                            COL,
                            &[
                                row("flash-fill", "Flash Fill", None, SheetAct::FlashFill),
                                row(
                                    "data-remove-duplicates",
                                    "Remove Duplicates",
                                    None,
                                    SheetAct::RemoveDuplicates,
                                ),
                            ],
                        ),
                        Item::One(large(
                            "data-validation",
                            "Data Validation",
                            None,
                            SheetAct::DataValidation,
                        )),
                        // Excel keeps these under Data Validation's split
                        // button; this ribbon has none, so they follow it (#689).
                        col(
                            COL,
                            &[
                                row(
                                    "circle-invalid",
                                    "Circle Invalid Data",
                                    None,
                                    SheetAct::CircleInvalid,
                                ),
                                row(
                                    "clear-validation-circles",
                                    "Clear Validation Circles",
                                    None,
                                    SheetAct::ClearValidationCircles,
                                ),
                            ],
                        ),
                        Item::One(large(
                            "consolidate",
                            "Consolidate",
                            None,
                            SheetAct::Consolidate,
                        )),
                    ],
                },
            },
            Group {
                title: "Outline",
                priority: 70,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large("group", "Group", None, SheetAct::Group)),
                        Item::One(large("ungroup", "Ungroup", None, SheetAct::Ungroup)),
                        Item::One(large("subtotal", "Subtotal", None, SheetAct::Subtotal)),
                        col(
                            COL,
                            &[
                                row("show-detail", "Show Detail", None, SheetAct::ShowDetail),
                                row("hide-detail", "Hide Detail", None, SheetAct::HideDetail),
                            ],
                        ),
                        col(
                            COL,
                            &[
                                row("auto-outline", "Auto Outline", None, SheetAct::AutoOutline),
                                row(
                                    "clear-outline",
                                    "Clear Outline",
                                    None,
                                    SheetAct::ClearOutline,
                                ),
                                row(
                                    "outline-settings",
                                    "Settings...",
                                    None,
                                    SheetAct::OutlineSettings,
                                ),
                            ],
                        ),
                    ],
                },
            },
            // The outline's level buttons, as Excel draws them over the row
            // numbers: 1 shows only the top level, 8 everything.
            Group {
                title: "Show Level",
                priority: 60,
                launcher: false,
                launch: None,
                body: rows(&[
                    &[
                        glyph("level-1", "Show Level 1", "1", SheetAct::ShowLevel(1)),
                        glyph("level-2", "Show Level 2", "2", SheetAct::ShowLevel(2)),
                        glyph("level-3", "Show Level 3", "3", SheetAct::ShowLevel(3)),
                        glyph("level-4", "Show Level 4", "4", SheetAct::ShowLevel(4)),
                    ],
                    &[
                        glyph("level-5", "Show Level 5", "5", SheetAct::ShowLevel(5)),
                        glyph("level-6", "Show Level 6", "6", SheetAct::ShowLevel(6)),
                        glyph("level-7", "Show Level 7", "7", SheetAct::ShowLevel(7)),
                        glyph("level-8", "Show Level 8", "8", SheetAct::ShowLevel(8)),
                    ],
                ]),
            },
        ],
    },
    Tab {
        tab: RibbonTab::Review,
        titles: Titles::Plain,
        groups: &[
            Group {
                title: "Proofing",
                priority: 90,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[Item::One(large(
                        "spelling",
                        "Spelling",
                        None,
                        SheetAct::Todo,
                    ))],
                },
            },
            Group {
                title: "Comments",
                priority: 80,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "new-comment",
                            "New Comment",
                            None,
                            SheetAct::NewComment,
                        )),
                        Item::One(large(
                            "delete-comment",
                            "Delete",
                            None,
                            SheetAct::DeleteComment,
                        )),
                        Item::One(large(
                            "previous-comment",
                            "Previous",
                            None,
                            SheetAct::PrevComment,
                        )),
                        Item::One(large("next-comment", "Next", None, SheetAct::NextComment)),
                    ],
                },
            },
            Group {
                title: "Protect",
                priority: 70,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(toggle(
                            "protect-sheet",
                            "Protect Sheet",
                            "Unprotect Sheet",
                            Some("lock"),
                            SheetAct::ProtectSheet,
                        )),
                        Item::One(large(
                            "protect-workbook",
                            "Protect Workbook",
                            None,
                            SheetAct::Todo,
                        )),
                    ],
                },
            },
        ],
    },
    Tab {
        tab: RibbonTab::View,
        titles: Titles::Plain,
        groups: &[Group {
            title: "Window",
            priority: 90,
            launcher: false,
            launch: None,
            body: Body::Strip {
                gap: GAP_1,
                items: &[
                    Item::One(toggle(
                        "freeze-panes",
                        "Freeze Panes",
                        "Unfreeze Panes",
                        None,
                        SheetAct::FreezePanes,
                    )),
                    Item::One(large(
                        "newwindow",
                        "New Window",
                        Some("new"),
                        SheetAct::NewWindow,
                    )),
                    Item::One(large(
                        "arrangeall",
                        "Arrange All",
                        Some("columns"),
                        SheetAct::ArrangeAll,
                    )),
                ],
            },
        }],
    },
    // Help ends the row, as in Excel (#1021): the document ribbon's Help tab.
    Tab {
        tab: RibbonTab::Help,
        titles: Titles::Plain,
        groups: &[
            Group {
                title: "Help",
                priority: 90,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large("help", "Help", None, SheetAct::Help(HelpAct::Help))),
                        col(
                            COL,
                            &[
                                row(
                                    "contact-support",
                                    "Contact Support",
                                    None,
                                    SheetAct::Help(HelpAct::ContactSupport),
                                ),
                                row(
                                    "feedback",
                                    "Feedback",
                                    None,
                                    SheetAct::Help(HelpAct::Feedback),
                                ),
                                row(
                                    "show-training",
                                    "Show Training",
                                    None,
                                    SheetAct::Help(HelpAct::ShowTraining),
                                ),
                            ],
                        ),
                        col(
                            COL,
                            &[row(
                                "whats-new",
                                "What's New",
                                None,
                                SheetAct::Help(HelpAct::WhatsNew),
                            )],
                        ),
                    ],
                },
            },
            Group {
                title: "About",
                priority: 80,
                launcher: false,
                launch: None,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[Item::One(large(
                        "about-docxy",
                        "About docxy suite",
                        None,
                        SheetAct::Help(HelpAct::About),
                    ))],
                },
            },
        ],
    },
];

/// The tab definition drawn for `tab`: its own entry, or Home's.
pub(crate) fn tab_def(tab: RibbonTab) -> &'static Tab {
    SHEET_RIBBON
        .iter()
        .find(|t| t.tab == tab)
        .unwrap_or(&SHEET_RIBBON[0])
}

/// Whether `act`'s button draws pressed for the selection's format `xf`.
/// Whether `act` is a toggle drawn pressed while it is on ([`act_on`]): a
/// press flips it and changes nothing else, so a ribbon flyout stays open
/// over it (#1020). Fill Color and Font Color open a picker instead.
pub(crate) fn act_toggles(act: SheetAct) -> bool {
    matches!(
        act,
        SheetAct::Bold
            | SheetAct::Italic
            | SheetAct::ToggleBorder
            | SheetAct::AlignL
            | SheetAct::AlignC
            | SheetAct::AlignR
    )
}

pub(crate) fn act_on(act: SheetAct, xf: &gridcore::sheet::Xf) -> bool {
    use gridcore::sheet::Align;
    match act {
        SheetAct::Bold => xf.bold,
        SheetAct::Italic => xf.italic,
        SheetAct::ToggleBorder => xf.border,
        SheetAct::AlignL => matches!(xf.align, Align::Left),
        SheetAct::AlignC => matches!(xf.align, Align::Center),
        SheetAct::AlignR => matches!(xf.align, Align::Right),
        _ => false,
    }
}

/// The commands matching `query` in the first tier that has any: by id, else
/// by label as it reads now, else by drawn text.
fn tier_matches<'a>(
    commands: &[&'a SheetCmd],
    query: &str,
    toggled: &impl Fn(SheetAct) -> bool,
) -> Vec<&'a SheetCmd> {
    let tiers: [&dyn Fn(&SheetCmd) -> bool; 3] = [
        &|c| c.id == query,
        &|c| c.label(toggled(c.act)) == query,
        &|c| c.text(toggled(c.act)) == query,
    ];
    tiers
        .iter()
        .map(|hit| {
            commands
                .iter()
                .copied()
                .filter(|c| hit(c))
                .collect::<Vec<_>>()
        })
        .find(|m| !m.is_empty())
        .unwrap_or_default()
}

/// One match, or why there is not one.
fn single<'a>(
    matches: Vec<&'a SheetCmd>,
    tab_name: &str,
    query: &str,
    toggled: &impl Fn(SheetAct) -> bool,
) -> Result<&'a SheetCmd, String> {
    match matches.as_slice() {
        [only] if !only.enabled() => Err(format!(
            "'{}' is not implemented",
            only.label(toggled(only.act))
        )),
        [only] => Ok(only),
        [] => Err(format!("command '{query}' is not on tab '{tab_name}'")),
        many => Err(format!(
            "command '{query}' is ambiguous on tab '{tab_name}': {}",
            many.iter().map(|c| c.id).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Resolve `query` on a tab of the table by id, else by label as it reads now,
/// else by drawn text, as the document ribbon resolves by id, label, then
/// screentip. `toggled` gives a command's state for its current label. A drop-down's items are looked at only
/// when no button of the tab matches: Home's AutoSum column has a Clear and
/// so does Sort & Filter, and the button wins.
pub(crate) fn resolve_on(
    tab: &'static Tab,
    tab_name: &str,
    query: &str,
    toggled: impl Fn(SheetAct) -> bool,
) -> Result<&'static SheetCmd, String> {
    let all = tab.commands();
    let buttons: Vec<&'static SheetCmd> = all
        .iter()
        .copied()
        .filter(|c| !tab.menu_owner(c).is_some())
        .collect();
    let mut matches = tier_matches(&buttons, query, &toggled);
    if matches.is_empty() {
        matches = tier_matches(&all, query, &toggled);
    }
    single(matches, tab_name, query, &toggled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, ribbon_tab_set};
    use std::collections::HashSet;

    fn all() -> Vec<&'static SheetCmd> {
        SHEET_RIBBON.iter().flat_map(Tab::commands).collect()
    }

    #[test]
    fn every_sheet_ribbon_tab_the_strip_offers_has_its_own_entry() {
        for (tab, name, _) in ribbon_tab_set(Kind::Xlsx) {
            let Some(tab) = tab else { continue }; // File is the backstage
            assert!(
                SHEET_RIBBON.iter().any(|t| t.tab == *tab),
                "sheet tab {name} has no table entry, so it would draw Home"
            );
        }
        assert!(SHEET_RIBBON[0].tab == RibbonTab::Home);
        assert!(tab_def(RibbonTab::TableLayout).tab == RibbonTab::Home);
    }

    #[test]
    fn command_ids_are_unique_across_the_whole_sheet_ribbon() {
        let mut seen = HashSet::new();
        for c in all() {
            assert!(seen.insert(c.id), "duplicate sheet ribbon id {}", c.id);
            assert!(!c.id.is_empty() && !c.label.is_empty());
        }
    }

    #[test]
    fn labels_are_unique_within_each_tab_in_both_states() {
        for tab in SHEET_RIBBON {
            for toggled in [false, true] {
                let mut seen = HashSet::new();
                // A menu item may repeat a ribbon button's name (Home's Clear);
                // `resolve_on` prefers the button.
                for c in tab
                    .commands()
                    .into_iter()
                    .filter(|c| !tab.menu_owner(c).is_some())
                {
                    assert!(
                        seen.insert(c.label(toggled)),
                        "label {} repeats on a tab",
                        c.label(toggled)
                    );
                }
                // Menu items need distinct names among themselves too: when no
                // ribbon button matches, `ribbon-click` (`resolve_on`) looks
                // over every item of the tab, and two alike would be ambiguous.
                let mut items = HashSet::new();
                for c in tab
                    .commands()
                    .into_iter()
                    .filter(|c| tab.menu_owner(c).is_some())
                {
                    assert!(
                        items.insert(c.label(toggled)),
                        "menu item label {} repeats on a tab",
                        c.label(toggled)
                    );
                }
            }
        }
    }

    #[test]
    fn the_placeholders_are_exactly_the_disabled_commands() {
        let disabled: Vec<&str> = all()
            .into_iter()
            .filter(|c| !c.enabled())
            .map(|c| c.id)
            .collect();
        assert_eq!(
            disabled,
            [
                "format-painter",
                "font-name",
                "font-size",
                "underline",
                "top-align",
                "middle-align",
                "bottom-align",
                "decrease-indent",
                "increase-decimal",
                "decrease-decimal",
                "cell-styles",
                // Page Layout (#1019): no theme, background, view state or
                // drawing-object engine yet.
                "themes",
                "theme-colors",
                "theme-fonts",
                "theme-effects",
                "background",
                "right-to-left",
                "view-gridlines",
                "view-headings",
                "bring-forward",
                "send-backward",
                "selection-pane",
                "arrange-align",
                "arrange-group",
                "arrange-rotate",
                // Formulas (#1019): Insert Function and the galleries, names
                // (#719), auditing, Show Formulas (#676), calculation (#674).
                "insert-function",
                "recently-used",
                "financial",
                "logical",
                "text-functions",
                "date-time",
                "lookup-reference",
                "math-trig",
                "more-functions",
                "name-manager",
                "define-name",
                "use-in-formula",
                "create-from-selection",
                "trace-precedents",
                "trace-dependents",
                "remove-arrows",
                "show-formulas",
                "error-checking",
                "evaluate-formula",
                "watch-window",
                "calculation-options",
                "calculate-now",
                "calculate-sheet",
                "spelling",
                "protect-workbook",
                // Help (#1021): nothing to show yet.
                "show-training",
                "whats-new",
            ]
        );
    }

    #[test]
    fn resolution_takes_an_id_then_a_label_then_the_drawn_text() {
        let home = tab_def(RibbonTab::Home);
        let off = |_| false;
        assert_eq!(resolve_on(home, "Home", "bold", off).unwrap().id, "bold");
        assert_eq!(resolve_on(home, "Home", "Bold", off).unwrap().id, "bold");
        assert_eq!(
            resolve_on(home, "Home", "Sort A to Z", off).unwrap().id,
            "sort-a-z"
        );
        assert_eq!(
            resolve_on(home, "Home", "\u{03A3} AutoSum", off)
                .unwrap()
                .id,
            "autosum"
        );
        assert_eq!(
            resolve_on(home, "Home", "Spelling", off).unwrap_err(),
            "command 'Spelling' is not on tab 'Home'"
        );
        assert_eq!(
            resolve_on(home, "Home", "Format Painter", off).unwrap_err(),
            "'Format Painter' is not implemented"
        );
    }

    /// Excel's tabs on a new workbook, in Excel's order, with Excel's
    /// KeyTips (APP-021, APP-CASE-014; #1019, Help from #1021).
    #[test]
    fn workbook_tabs_are_excels_in_excels_order() {
        let tabs: Vec<(&str, &str)> = ribbon_tab_set(Kind::Xlsx)
            .iter()
            .map(|&(_, name, key)| (name, key))
            .collect();
        let excel = [
            ("File", "F"),
            ("Home", "H"),
            ("Insert", "N"),
            ("Page Layout", "P"),
            ("Formulas", "M"),
            ("Data", "A"),
            ("Review", "R"),
            ("View", "W"),
            ("Help", "Y"),
        ];
        assert_eq!(tabs, excel);
        // The strip and its KeyTips come from `ribbon_for`, which agrees.
        let strip: Vec<(&str, &str)> = crate::ribbon_for(Kind::Xlsx)
            .tabs
            .iter()
            .map(|t| (t.name, t.key_tip))
            .collect();
        assert_eq!(strip, excel[1..]);
        for (tab, name, _) in ribbon_tab_set(Kind::Xlsx).iter().skip(1) {
            assert_eq!(crate::ribbon_tab_name(tab.unwrap()), *name);
        }
        // A document and a project keep their own.
        let names = |k| {
            ribbon_tab_set(k)
                .iter()
                .map(|&(_, n, _)| n)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Kind::Docx),
            [
                "File", "Home", "Insert", "Design", "Layout", "Mailings", "Review", "View", "Help"
            ]
        );
        assert!(!names(Kind::Project).contains(&"Page Layout"));
    }

    /// Group titles and the command labels of each group, in drawn order.
    fn groups_of(tab: RibbonTab) -> Vec<(&'static str, Vec<&'static str>)> {
        tab_def(tab)
            .groups
            .iter()
            .map(|g| {
                (
                    g.title,
                    g.commands().iter().map(|c| c.label(false)).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn the_page_layout_tab_has_excels_groups_in_excels_order() {
        assert!(tab_def(RibbonTab::PageLayout).tab == RibbonTab::PageLayout);
        assert_eq!(
            groups_of(RibbonTab::PageLayout),
            [
                ("Themes", vec!["Themes", "Colors", "Fonts", "Effects"]),
                (
                    "Page Setup",
                    vec![
                        "Margins",
                        "Normal",
                        "Wide",
                        "Narrow",
                        "Custom Margins...",
                        "Orientation",
                        "Portrait",
                        "Landscape",
                        "Size",
                        "Letter",
                        "Tabloid",
                        "Legal",
                        "Executive",
                        "A3",
                        "A4",
                        "A5",
                        "B4 (JIS)",
                        "B5 (JIS)",
                        "More Paper Sizes...",
                        "Print Area",
                        "Set Print Area",
                        "Clear Print Area",
                        "Add to Print Area",
                        "Breaks",
                        "Insert Page Break",
                        "Remove Page Break",
                        "Reset All Page Breaks",
                        "Background...",
                        "Print Titles",
                    ]
                ),
                ("Scale to Fit", vec!["Width:", "Height:", "Scale:"]),
                (
                    "Sheet Options",
                    vec![
                        "Right-to-Left Document",
                        "View Gridlines",
                        "Print Gridlines",
                        "View Headings",
                        "Print Headings",
                    ]
                ),
                (
                    "Arrange",
                    vec![
                        "Bring Forward",
                        "Send Backward",
                        "Selection Pane...",
                        "Align",
                        "Group",
                        "Rotate",
                    ]
                ),
            ]
        );
        // Page Setup, Scale to Fit and Sheet Options open Page Setup.
        let launched: Vec<_> = tab_def(RibbonTab::PageLayout)
            .groups
            .iter()
            .map(|g| (g.title, g.launcher, g.launch))
            .collect();
        let dialog = |at| Some(SheetAct::Page(PageAct::Dialog(at)));
        assert_eq!(
            launched,
            [
                ("Themes", false, None),
                ("Page Setup", true, dialog(SetupTab::Page)),
                ("Scale to Fit", true, dialog(SetupTab::Page)),
                ("Sheet Options", true, dialog(SetupTab::Sheet)),
                ("Arrange", false, None),
            ]
        );
    }

    #[test]
    fn the_formulas_tab_has_excels_groups_in_excels_order() {
        assert!(tab_def(RibbonTab::Formulas).tab == RibbonTab::Formulas);
        assert_eq!(
            groups_of(RibbonTab::Formulas),
            [
                (
                    "Function Library",
                    vec![
                        "Insert Function...",
                        "AutoSum",
                        "Recently Used",
                        "Financial",
                        "Logical",
                        "Text",
                        "Date & Time",
                        "Lookup & Reference",
                        "Math & Trig",
                        "More Functions",
                    ]
                ),
                (
                    "Defined Names",
                    vec![
                        "Name Manager",
                        "Define Name",
                        "Use in Formula",
                        "Create from Selection...",
                    ]
                ),
                (
                    "Formula Auditing",
                    vec![
                        "Trace Precedents",
                        "Trace Dependents",
                        "Remove Arrows",
                        "Show Formulas",
                        "Error Checking",
                        "Evaluate Formula",
                        "Watch Window",
                    ]
                ),
                (
                    "Calculation",
                    vec!["Calculation Options", "Calculate Now", "Calculate Sheet"]
                ),
            ]
        );
    }

    /// The commands whose engine exists run (#1019); the rest are drawn and
    /// refused as not implemented.
    #[test]
    fn wired_commands_are_enabled() {
        let pl = tab_def(RibbonTab::PageLayout);
        let off = |_| false;
        let act = |tab, name, q| resolve_on(tab, name, q, off).map(|c| c.act);
        let page = |p| Ok(SheetAct::Page(p));
        for (q, want) in [
            ("Set Print Area", page(PageAct::Area(AreaOp::Set))),
            ("Clear Print Area", page(PageAct::Area(AreaOp::Clear))),
            ("Add to Print Area", page(PageAct::Area(AreaOp::Add))),
            ("Insert Page Break", page(PageAct::Break(BreakOp::Insert))),
            ("Remove Page Break", page(PageAct::Break(BreakOp::Remove))),
            (
                "Reset All Page Breaks",
                page(PageAct::Break(BreakOp::Reset)),
            ),
            ("Normal", page(PageAct::Margins(MarginPreset::Normal))),
            ("Wide", page(PageAct::Margins(MarginPreset::Wide))),
            ("Narrow", page(PageAct::Margins(MarginPreset::Narrow))),
            (
                "Custom Margins...",
                page(PageAct::Dialog(SetupTab::Margins)),
            ),
            ("Portrait", page(PageAct::Landscape(false))),
            ("Landscape", page(PageAct::Landscape(true))),
            ("Letter", page(PageAct::Paper(1))),
            ("A4", page(PageAct::Paper(9))),
            ("Legal", page(PageAct::Paper(5))),
            ("A3", page(PageAct::Paper(8))),
            ("More Paper Sizes...", page(PageAct::Dialog(SetupTab::Page))),
            ("Print Titles", page(PageAct::Dialog(SetupTab::Sheet))),
            ("Width:", Ok(SheetAct::Menu(SheetMenu::FitWidth))),
            ("Height:", Ok(SheetAct::Menu(SheetMenu::FitHeight))),
            ("Scale:", Ok(SheetAct::Menu(SheetMenu::Scale))),
            ("Print Gridlines", page(PageAct::PrintGridlines)),
            ("Print Headings", page(PageAct::PrintHeadings)),
        ] {
            assert_eq!(act(pl, "Page Layout", q), want, "{q}");
        }
        // The Size menu's papers are the dialog's.
        let sizes: Vec<SheetAct> = pl
            .commands()
            .iter()
            .filter(|c| c.id.starts_with("size-"))
            .map(|c| c.act)
            .collect();
        let papers: Vec<SheetAct> = crate::sheet_page_setup::PAPERS
            .iter()
            .map(|&(code, _)| SheetAct::Page(PageAct::Paper(code)))
            .collect();
        assert_eq!(sizes, papers);
        let f = tab_def(RibbonTab::Formulas);
        let autosum = resolve_on(f, "Formulas", "AutoSum", off).unwrap();
        assert_eq!(autosum.act, SheetAct::AutoSumFn(SumFn::Sum));
        assert!(matches!(
            autosum.shape,
            Shape::Split {
                menu: SheetMenu::AutoSum,
                ..
            }
        ));
        for q in [
            "Trace Precedents",
            "Name Manager",
            "Show Formulas",
            "Calculate Now",
        ] {
            assert_eq!(
                resolve_on(f, "Formulas", q, off).unwrap_err(),
                format!("'{q}' is not implemented")
            );
        }
        for q in ["Themes", "View Gridlines", "Background...", "Bring Forward"] {
            assert!(resolve_on(pl, "Page Layout", q, off).is_err(), "{q}");
        }
    }

    #[test]
    fn the_data_tab_has_excels_groups_in_excels_order() {
        let data = tab_def(RibbonTab::Data);
        assert!(data.tab == RibbonTab::Data);
        let titles: Vec<&str> = data.groups.iter().map(|g| g.title).collect();
        assert_eq!(
            titles,
            ["Sort & Filter", "Data Tools", "Outline", "Show Level"]
        );
        let labels = |i: usize| -> Vec<&str> {
            data.groups[i]
                .commands()
                .iter()
                .map(|c| c.label(false))
                .collect()
        };
        assert_eq!(
            labels(0),
            [
                "Sort A to Z",
                "Sort Z to A",
                "Sort",
                "Filter",
                "Clear",
                "Reapply",
                "Advanced"
            ]
        );
        assert_eq!(
            labels(1),
            [
                "Text to Columns",
                "Flash Fill",
                "Remove Duplicates",
                "Data Validation",
                "Circle Invalid Data",
                "Clear Validation Circles",
                "Consolidate"
            ]
        );
        assert_eq!(
            labels(2)[..5],
            ["Group", "Ungroup", "Subtotal", "Show Detail", "Hide Detail"]
        );
    }

    #[test]
    fn data_tab_resolves_excel_names_to_the_existing_commands() {
        let data = tab_def(RibbonTab::Data);
        let off = |_| false;
        let act = |q| resolve_on(data, "Data", q, off).map(|c| c.act);
        assert_eq!(act("Sort A to Z"), Ok(SheetAct::SortAsc));
        assert_eq!(act("Sort Z to A"), Ok(SheetAct::SortDesc));
        assert_eq!(act("Sort"), Ok(SheetAct::CustomSort));
        assert_eq!(act("Filter"), Ok(SheetAct::Filter));
        assert_eq!(act("Clear"), Ok(SheetAct::ClearFilter));
        assert_eq!(act("Reapply"), Ok(SheetAct::ReapplyFilter));
        assert_eq!(act("Advanced"), Ok(SheetAct::AdvancedFilter));
        assert_eq!(act("Remove Duplicates"), Ok(SheetAct::RemoveDuplicates));
        assert_eq!(act("Text to Columns"), Ok(SheetAct::TextToColumns));
        assert_eq!(act("Data Validation"), Ok(SheetAct::DataValidation));
        assert_eq!(act("Consolidate"), Ok(SheetAct::Consolidate));
        assert_eq!(act("Flash Fill"), Ok(SheetAct::FlashFill));
        assert_eq!(act("Circle Invalid Data"), Ok(SheetAct::CircleInvalid));
        assert_eq!(
            act("Clear Validation Circles"),
            Ok(SheetAct::ClearValidationCircles)
        );
    }

    #[test]
    fn a_toggle_resolves_by_the_label_it_shows_now() {
        let view = tab_def(RibbonTab::View);
        let on = |act| matches!(act, SheetAct::FreezePanes);
        let off = |_| false;
        assert!(resolve_on(view, "View", "Unfreeze Panes", on).is_ok());
        assert!(resolve_on(view, "View", "Unfreeze Panes", off).is_err());
        assert!(resolve_on(view, "View", "Freeze Panes", off).is_ok());
        assert!(resolve_on(view, "View", "freeze-panes", on).is_ok());
    }

    #[test]
    fn an_ambiguous_label_names_the_ids_it_could_mean() {
        static A: SheetCmd = cmd("a", "Same", Shape::Glyph("a"), SheetAct::Bold);
        static B: SheetCmd = cmd("b", "Same", Shape::Glyph("b"), SheetAct::Italic);
        let off = |_: SheetAct| false;
        assert_eq!(
            single(tier_matches(&[&A, &B], "Same", &off), "Home", "Same", &off).unwrap_err(),
            "command 'Same' is ambiguous on tab 'Home': a, b"
        );
    }

    /// The most small-button rows a column or a rows stack holds: three fit the
    /// ribbon body (#1018). Names the tab, the group and the count.
    #[test]
    fn no_ribbon_column_has_more_than_three_rows() {
        // ribbonspec's limit, not the table's own: the table's const is part of what is checked.
        const LIMIT: usize = ribbonspec::MAX_COLUMN_ROWS;
        let mut bad: Vec<String> = Vec::new();
        for tab in SHEET_RIBBON {
            let name = crate::ribbon_tab_name(tab.tab);
            for g in tab.groups {
                if let Body::Rows(r) = &g.body {
                    if r.rows.len() > LIMIT {
                        bad.push(format!(
                            "sheet tab '{name}', group '{}': {} rows (max {LIMIT})",
                            g.title,
                            r.rows.len()
                        ));
                    }
                }
                if let Body::Strip { items, .. } = &g.body {
                    for item in *items {
                        if let Item::Col(c) = item {
                            if c.cmds.len() > LIMIT {
                                bad.push(format!(
                                    "sheet tab '{name}', group '{}': {} rows (max {LIMIT})",
                                    g.title,
                                    c.cmds.len()
                                ));
                            }
                        }
                    }
                }
            }
        }
        // The document and Project ribbons, and the tabs that appear in context.
        for kind in [Kind::Docx, Kind::Project] {
            bad.extend(crate::ribbon_for(kind).row_violations());
        }
        for tab in [
            crate::table_tab::table_design_tab(),
            crate::table_tab::table_layout_tab(),
            crate::hf_tab::hf_tab(),
            crate::gantt_format_tab(),
        ] {
            bad.extend(tab.row_violations());
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn home_editing_matches_excel() {
        let home = tab_def(RibbonTab::Home);
        let g = home.groups.iter().find(|g| g.title == "Editing").unwrap();
        let Body::Strip { items, .. } = &g.body else {
            panic!("Editing is a strip");
        };
        let [Item::Col(col_), Item::Menu(sort), Item::One(find)] = items else {
            panic!("Editing is a column, a drop-down and a button");
        };
        let ids = |c: &[SheetCmd]| c.iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(ids(col_.cmds), ["AutoSum", "Fill", "Clear"]);
        assert!(matches!(sort.button.shape, Shape::Menu(_)));
        assert_eq!(sort.button.label, "Sort & Filter");
        assert_eq!(
            sort.items.iter().map(|c| c.text(false)).collect::<Vec<_>>(),
            [
                "Sort A to Z",
                "Sort Z to A",
                "Custom Sort...",
                "Filter",
                "Clear",
                "Reapply"
            ]
        );
        assert!(matches!(find.shape, Shape::Large(_)));
        assert_eq!(find.label, "Find & Select");
        assert!(
            home.commands()
                .iter()
                .all(|c| c.act != SheetAct::RemoveDuplicates),
            "Remove Duplicates lives on Data > Data Tools"
        );
        let data = tab_def(RibbonTab::Data).commands();
        assert!(data.iter().any(|c| c.id == "data-remove-duplicates"));
    }

    #[test]
    fn only_the_sort_filter_items_belong_to_a_menu() {
        let home = tab_def(RibbonTab::Home);
        let editing = home.groups.iter().find(|g| g.title == "Editing").unwrap();
        let owner = |id: &str| {
            let c = editing.commands().into_iter().find(|c| c.id == id).unwrap();
            editing.menu_owner(c).map(|m| m.button.id)
        };
        for id in [
            "sort-a-z",
            "sort-z-a",
            "custom-sort",
            "filter",
            "home-clear-filter",
            "home-reapply-filter",
        ] {
            assert_eq!(owner(id), Some("sort-filter"), "{id}");
        }
        for id in ["autosum", "fill", "clear", "sort-filter", "find-select"] {
            assert_eq!(owner(id), None, "{id}");
        }
    }

    #[test]
    fn the_sort_filter_menu_keeps_the_ribbon_click_ids_and_runs_the_data_tabs_acts() {
        let home = tab_def(RibbonTab::Home);
        let off = |_| false;
        let act = |q| resolve_on(home, "Home", q, off).map(|c| c.act);
        assert_eq!(act("sort-a-z"), Ok(SheetAct::SortAsc));
        assert_eq!(act("Sort Z to A"), Ok(SheetAct::SortDesc));
        assert_eq!(act("custom-sort"), Ok(SheetAct::CustomSort));
        assert_eq!(act("filter"), Ok(SheetAct::Filter));
        assert_eq!(act("home-reapply-filter"), Ok(SheetAct::ReapplyFilter));
        assert_eq!(act("home-clear-filter"), Ok(SheetAct::ClearFilter));
        // The menu's Clear shares its name with the AutoSum column's: the
        // ribbon button wins, and that one opens the Clear menu (#707).
        for q in ["Clear", "clear"] {
            let c = resolve_on(home, "Home", q, off).unwrap();
            assert_eq!(c.id, "clear");
            assert!(matches!(c.act, SheetAct::Menu(_)));
        }
        // The drop-down button itself resolves, enabled: it opens its menu.
        let button = resolve_on(home, "Home", "Sort & Filter", off).unwrap();
        assert!(button.enabled() && matches!(button.shape, Shape::Menu(_)));
    }

    #[test]
    fn pressed_state_follows_the_selection_format() {
        let bold = gridcore::sheet::Xf {
            bold: true,
            ..Default::default()
        };
        assert!(act_on(SheetAct::Bold, &bold));
        assert!(!act_on(SheetAct::Italic, &bold));
        assert!(!act_on(SheetAct::Bold, &Default::default()));
        assert!(!act_on(SheetAct::Paste, &bold));
    }

    /// #1020 r1: a command that can draw pressed is a toggle (a flyout stays
    /// open over it); Fill Color and Font Color open a picker, and are not.
    #[test]
    fn the_toggles_are_the_commands_that_draw_pressed() {
        use gridcore::sheet::{Align, Xf};
        let on = [
            Xf {
                bold: true,
                italic: true,
                border: true,
                align: Align::Left,
                ..Default::default()
            },
            Xf {
                align: Align::Center,
                ..Default::default()
            },
            Xf {
                align: Align::Right,
                ..Default::default()
            },
        ];
        for t in SHEET_RIBBON {
            for c in t.commands() {
                if on.iter().any(|xf| act_on(c.act, xf)) {
                    assert!(act_toggles(c.act), "{} draws pressed", c.id);
                }
            }
        }
        assert!(!act_toggles(SheetAct::FillColor));
        assert!(!act_toggles(SheetAct::FontColor));
    }
}
