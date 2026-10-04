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

use crate::{RibbonTab, SheetAct};

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
    /// clicking one is a no-op, so the harness lists them disabled.
    pub fn enabled(&self) -> bool {
        !matches!(self.act, SheetAct::Todo)
    }
}

/// A group's content.
pub(crate) enum Body {
    /// Buttons and button columns side by side.
    Strip { gap: Gap, items: &'static [Item] },
    /// Rows of small buttons, stacked.
    Rows(&'static [&'static [SheetCmd]]),
}

/// One slot of a `Body::Strip`.
pub(crate) enum Item {
    One(SheetCmd),
    Col { gap: Gap, cmds: &'static [SheetCmd] },
}

pub(crate) struct Group {
    pub title: &'static str,
    /// Draws the dialog-launcher glyph beside the title (inert).
    pub launcher: bool,
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
    /// The group's commands in drawn order.
    pub fn commands(&self) -> Vec<&'static SheetCmd> {
        match &self.body {
            Body::Strip { items, .. } => items
                .iter()
                .flat_map(|item| match item {
                    Item::One(cmd) => std::slice::from_ref(cmd),
                    Item::Col { cmds, .. } => cmds,
                })
                .collect(),
            Body::Rows(rows) => rows.iter().flat_map(|r| r.iter()).collect(),
        }
    }
}

impl Tab {
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
                launcher: true,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large("paste", "Paste", Some("paste"), SheetAct::Paste)),
                        Item::Col {
                            gap: COL,
                            cmds: &[
                                row("cut", "Cut", Some("cut"), SheetAct::Cut),
                                row("copy", "Copy", Some("copy"), SheetAct::Copy),
                                row("format-painter", "Format Painter", None, SheetAct::Todo),
                            ],
                        },
                    ],
                },
            },
            Group {
                title: "Font",
                launcher: true,
                body: Body::Rows(&[
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
                launcher: true,
                body: Body::Rows(&[
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
                launcher: true,
                body: Body::Rows(&[
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
                launcher: false,
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
                launcher: false,
                body: Body::Strip {
                    gap: GAP_2,
                    items: &[
                        Item::Col {
                            gap: GAP_0P5,
                            cmds: &[
                                row("insert-row", "Insert Row", None, SheetAct::InsertRow),
                                row("insert-col", "Insert Col", None, SheetAct::InsertCol),
                            ],
                        },
                        Item::Col {
                            gap: GAP_0P5,
                            cmds: &[
                                row("delete-row", "Delete Row", None, SheetAct::DeleteRow),
                                row("delete-col", "Delete Col", None, SheetAct::DeleteCol),
                            ],
                        },
                        Item::One(large("format", "Format", None, SheetAct::FormatCells)),
                    ],
                },
            },
            Group {
                title: "Editing",
                launcher: false,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::Col {
                            gap: COL,
                            cmds: &[
                                row_as(
                                    "autosum",
                                    "AutoSum",
                                    "\u{03A3} AutoSum",
                                    None,
                                    SheetAct::AutoSum,
                                ),
                                row("fill", "Fill", None, SheetAct::Todo),
                                row("clear", "Clear", Some("clear-format"), SheetAct::Todo),
                            ],
                        },
                        Item::Col {
                            gap: COL,
                            cmds: &[
                                row_as(
                                    "sort-a-z",
                                    "Sort A to Z",
                                    "Sort A \u{2192} Z",
                                    Some("sort"),
                                    SheetAct::SortAsc,
                                ),
                                row_as(
                                    "sort-z-a",
                                    "Sort Z to A",
                                    "Sort Z \u{2192} A",
                                    Some("sort"),
                                    SheetAct::SortDesc,
                                ),
                                row_as(
                                    "custom-sort",
                                    "Custom Sort",
                                    "Custom Sort\u{2026}",
                                    Some("sort"),
                                    SheetAct::CustomSort,
                                ),
                                row("filter", "Filter", None, SheetAct::Filter),
                                row_as(
                                    "remove-duplicates",
                                    "Remove Duplicates",
                                    "Remove Dup",
                                    None,
                                    SheetAct::RemoveDuplicates,
                                ),
                            ],
                        },
                        Item::One(large(
                            "find-select",
                            "Find & Select",
                            Some("find"),
                            SheetAct::Todo,
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
                launcher: false,
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
                launcher: false,
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
    // Excel's Data tab (#693): Data Validation and Text to Columns moved
    // here from Insert, where they never were in Excel.
    Tab {
        tab: RibbonTab::Data,
        titles: Titles::Plain,
        groups: &[
            Group {
                title: "Data Tools",
                launcher: false,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large(
                            "text-to-columns",
                            "Text to Columns",
                            None,
                            SheetAct::TextToColumns,
                        )),
                        Item::One(large(
                            "data-validation",
                            "Data Validation",
                            None,
                            SheetAct::DataValidation,
                        )),
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
                launcher: false,
                body: Body::Strip {
                    gap: GAP_1,
                    items: &[
                        Item::One(large("group", "Group", None, SheetAct::Group)),
                        Item::One(large("ungroup", "Ungroup", None, SheetAct::Ungroup)),
                        Item::One(large("subtotal", "Subtotal", None, SheetAct::Subtotal)),
                        Item::Col {
                            gap: COL,
                            cmds: &[
                                row("show-detail", "Show Detail", None, SheetAct::ShowDetail),
                                row("hide-detail", "Hide Detail", None, SheetAct::HideDetail),
                            ],
                        },
                        Item::Col {
                            gap: COL,
                            cmds: &[
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
                        },
                    ],
                },
            },
            // The outline's level buttons, as Excel draws them over the row
            // numbers: 1 shows only the top level, 8 everything.
            Group {
                title: "Show Level",
                launcher: false,
                body: Body::Rows(&[
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
                launcher: false,
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
                launcher: false,
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
                launcher: false,
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
            launcher: false,
            body: Body::Strip {
                gap: GAP_1,
                items: &[Item::One(toggle(
                    "freeze-panes",
                    "Freeze Panes",
                    "Unfreeze Panes",
                    None,
                    SheetAct::FreezePanes,
                ))],
            },
        }],
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

/// Resolve `query` on `tab` by id, else by label as it reads now, else by
/// drawn text, as the document ribbon resolves by id, label, then screentip.
/// `toggled` gives a command's state for its current label.
pub(crate) fn resolve<'a>(
    commands: &[&'a SheetCmd],
    tab_name: &str,
    query: &str,
    toggled: impl Fn(SheetAct) -> bool,
) -> Result<&'a SheetCmd, String> {
    let tiers: [&dyn Fn(&SheetCmd) -> bool; 3] = [
        &|c| c.id == query,
        &|c| c.label(toggled(c.act)) == query,
        &|c| c.text(toggled(c.act)) == query,
    ];
    let matches: Vec<&'a SheetCmd> = tiers
        .iter()
        .map(|hit| {
            commands
                .iter()
                .copied()
                .filter(|c| hit(c))
                .collect::<Vec<_>>()
        })
        .find(|m| !m.is_empty())
        .unwrap_or_default();
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
                for c in tab.commands() {
                    assert!(
                        seen.insert(c.label(toggled)),
                        "label {} repeats on a tab",
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
                "fill",
                "clear",
                "find-select",
                "spelling",
                "protect-workbook",
            ]
        );
    }

    #[test]
    fn resolution_takes_an_id_then_a_label_then_the_drawn_text() {
        let home = tab_def(RibbonTab::Home).commands();
        let off = |_| false;
        assert_eq!(resolve(&home, "Home", "bold", off).unwrap().id, "bold");
        assert_eq!(resolve(&home, "Home", "Bold", off).unwrap().id, "bold");
        assert_eq!(
            resolve(&home, "Home", "Sort A to Z", off).unwrap().id,
            "sort-a-z"
        );
        assert_eq!(
            resolve(&home, "Home", "\u{03A3} AutoSum", off).unwrap().id,
            "autosum"
        );
        assert_eq!(
            resolve(&home, "Home", "Spelling", off).unwrap_err(),
            "command 'Spelling' is not on tab 'Home'"
        );
        assert_eq!(
            resolve(&home, "Home", "Format Painter", off).unwrap_err(),
            "'Format Painter' is not implemented"
        );
    }

    #[test]
    fn a_toggle_resolves_by_the_label_it_shows_now() {
        let view = tab_def(RibbonTab::View).commands();
        let on = |act| matches!(act, SheetAct::FreezePanes);
        let off = |_| false;
        assert!(resolve(&view, "View", "Unfreeze Panes", on).is_ok());
        assert!(resolve(&view, "View", "Unfreeze Panes", off).is_err());
        assert!(resolve(&view, "View", "Freeze Panes", off).is_ok());
        assert!(resolve(&view, "View", "freeze-panes", on).is_ok());
    }

    #[test]
    fn an_ambiguous_label_names_the_ids_it_could_mean() {
        static A: SheetCmd = cmd("a", "Same", Shape::Glyph("a"), SheetAct::Bold);
        static B: SheetCmd = cmd("b", "Same", Shape::Glyph("b"), SheetAct::Italic);
        assert_eq!(
            resolve(&[&A, &B], "Home", "Same", |_| false).unwrap_err(),
            "command 'Same' is ambiguous on tab 'Home': a, b"
        );
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
}
