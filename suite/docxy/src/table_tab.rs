//! Word's two contextual table tabs (#647, #648): **Table Design** (style
//! options, the Table Styles gallery, Shading, Borders) and the table
//! **Layout** tab (Select, View Gridlines, Rows & Columns, Merge, Cell Size,
//! Alignment, Data). Both show while the caret is in a table, of the body or
//! of the header or footer being edited, and act on the innermost table
//! through docxcore's table commands: each is one undo step, and a refusal
//! says why on the status line and changes nothing.
//!
//! Also here: the Insert > Table drop-down (the columns × rows hover grid and
//! its items), the one menu a table command set shares with the Insert tab.
use super::*;
use docxcore::editor::{AutoFitKind, BorderCmd};
use docxcore::table_props::{TblLook, VAlign};
use docxcore::table_styles::BUILTIN;

/// The table tabs' names as the harness and `ribbon-read` know them. The tab
/// strip draws the Layout one as plain "Layout", as Word does.
pub(crate) const DESIGN_TAB: &str = "Table Design";
pub(crate) const LAYOUT_TAB: &str = "Table Layout";

/// The Insert > Table hover grid: 10 columns by 8 rows, as in Word.
pub(crate) const GRID_COLS: usize = 10;
pub(crate) const GRID_ROWS: usize = 8;

/// A table tab command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TableAct {
    // ---- Layout ----
    Select(SelectWhat),
    ViewGridlines,
    InsertAbove,
    InsertBelow,
    InsertLeft,
    InsertRight,
    /// Open Delete Cells...
    DeleteCells,
    DeleteColumns,
    DeleteRows,
    DeleteTable,
    MergeCells,
    /// Open Split Cells...
    SplitCells,
    SplitTable,
    AutoFit(AutoFitKind),
    DistributeRows,
    DistributeColumns,
    Align(VAlign, Align),
    TextDirection,
    /// Open Sort...
    Sort,
    /// Open Convert to Text...
    ConvertToText,
    // ---- Design ----
    Look(LookFlag),
    /// Apply the built-in style `BUILTIN[i]`.
    Style(usize),
    /// Fill the selected cells; `None` is No Color.
    Shading(Option<u32>),
    Borders(BorderCmd),
    // ---- Insert > Table ----
    /// Open Insert Table...
    InsertTable,
    /// Open Convert Text to Table...
    TextToTable,
    /// An item Word has with nothing behind it yet: drawn disabled.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectWhat {
    Cell,
    Column,
    Row,
    Table,
}

/// A Table Style Options check box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LookFlag {
    HeaderRow,
    TotalRow,
    BandedRows,
    FirstColumn,
    LastColumn,
    BandedColumns,
}

impl LookFlag {
    fn get(self, l: &TblLook) -> bool {
        match self {
            Self::HeaderRow => l.first_row,
            Self::TotalRow => l.last_row,
            Self::BandedRows => l.banded_rows,
            Self::FirstColumn => l.first_col,
            Self::LastColumn => l.last_col,
            Self::BandedColumns => l.banded_cols,
        }
    }
    fn toggle(self, l: &mut TblLook) {
        let f = match self {
            Self::HeaderRow => &mut l.first_row,
            Self::TotalRow => &mut l.last_row,
            Self::BandedRows => &mut l.banded_rows,
            Self::FirstColumn => &mut l.first_col,
            Self::LastColumn => &mut l.last_col,
            Self::BandedColumns => &mut l.banded_cols,
        };
        *f = !*f;
    }
}

/// The Shading palette: Word's theme row, then its standard colours.
pub(crate) const SHADES: [(&str, &str, u32); 12] = [
    ("shade-white", "White", 0xFFFFFF),
    ("shade-gray", "Gray", 0xD9D9D9),
    ("shade-blue-light", "Blue, Lighter 80%", 0xD9E2F3),
    ("shade-orange-light", "Orange, Lighter 80%", 0xFBE4D5),
    ("shade-green-light", "Green, Lighter 80%", 0xE2EFD9),
    ("shade-gold-light", "Gold, Lighter 80%", 0xFFF2CC),
    ("shade-red", "Red", 0xFF0000),
    ("shade-orange", "Orange", 0xFFC000),
    ("shade-yellow", "Yellow", 0xFFFF00),
    ("shade-green", "Green", 0x00B050),
    ("shade-blue", "Blue", 0x0070C0),
    ("shade-purple", "Purple", 0x7030A0),
];

fn t(act: TableAct) -> Act {
    Act::Table(act)
}

fn border_id(b: BorderCmd) -> &'static str {
    match b {
        BorderCmd::Bottom => "border-bottom",
        BorderCmd::Top => "border-top",
        BorderCmd::Left => "border-left",
        BorderCmd::Right => "border-right",
        BorderCmd::NoBorder => "border-none",
        BorderCmd::All => "border-all",
        BorderCmd::Outside => "border-outside",
        BorderCmd::Inside => "border-inside",
        BorderCmd::InsideH => "border-inside-h",
        BorderCmd::InsideV => "border-inside-v",
        BorderCmd::DiagDown => "border-diag-down",
        BorderCmd::DiagUp => "border-diag-up",
    }
}

/// The nine alignment buttons: (id, label, vertical, horizontal).
const ALIGNS: [(&str, &str, VAlign, Align); 9] = [
    ("align-top-left", "Align Top Left", VAlign::Top, Align::Left),
    (
        "align-top-center",
        "Align Top Center",
        VAlign::Top,
        Align::Center,
    ),
    (
        "align-top-right",
        "Align Top Right",
        VAlign::Top,
        Align::Right,
    ),
    (
        "align-center-left",
        "Align Center Left",
        VAlign::Center,
        Align::Left,
    ),
    (
        "align-center",
        "Align Center",
        VAlign::Center,
        Align::Center,
    ),
    (
        "align-center-right",
        "Align Center Right",
        VAlign::Center,
        Align::Right,
    ),
    (
        "align-bottom-left",
        "Align Bottom Left",
        VAlign::Bottom,
        Align::Left,
    ),
    (
        "align-bottom-center",
        "Align Bottom Center",
        VAlign::Bottom,
        Align::Center,
    ),
    (
        "align-bottom-right",
        "Align Bottom Right",
        VAlign::Bottom,
        Align::Right,
    ),
];

fn align_icon(h: Align) -> &'static str {
    match h {
        Align::Left => "align-left",
        Align::Center => "align-center",
        Align::Right => "align-right",
        Align::Justify => "align-justify",
    }
}

/// The table Layout tab.
pub(crate) fn table_layout_tab() -> rs::Tab<Act> {
    use TableAct as T;
    let align_rows = ALIGNS
        .chunks(3)
        .map(|row| {
            row.iter()
                .map(|&(id, label, v, h)| {
                    rs::btn(cmdt(id, align_icon(h), label, t(T::Align(v, h)), ""))
                })
                .collect()
        })
        .collect();
    rs::tab(
        LAYOUT_TAB,
        "J",
        vec![
            rs::group(
                "Table",
                30,
                vec![
                    Control::Dropdown {
                        cmd: cmdt(
                            "select",
                            "table-select",
                            "Select",
                            t(T::Select(SelectWhat::Cell)),
                            "",
                        )
                        .key("K"),
                        items: vec![
                            rs::cmd(
                                "select-cell",
                                "table-select",
                                "Select Cell",
                                t(T::Select(SelectWhat::Cell)),
                            ),
                            rs::cmd(
                                "select-column",
                                "table-select",
                                "Select Column",
                                t(T::Select(SelectWhat::Column)),
                            ),
                            rs::cmd(
                                "select-row",
                                "table-select",
                                "Select Row",
                                t(T::Select(SelectWhat::Row)),
                            ),
                            rs::cmd(
                                "select-table",
                                "table-select",
                                "Select Table",
                                t(T::Select(SelectWhat::Table)),
                            ),
                        ],
                    },
                    rs::column(vec![
                        cmdt(
                            "gridlines",
                            "table-gridlines",
                            "View Gridlines",
                            t(T::ViewGridlines),
                            "",
                        )
                        .key("TG"),
                    ]),
                ],
            ),
            rs::group(
                "Rows & Columns",
                40,
                vec![
                    Control::Dropdown {
                        cmd: cmdt("delete", "table-dismiss", "Delete", t(T::DeleteRows), "")
                            .key("D"),
                        items: vec![
                            rs::cmd(
                                "delete-cells",
                                "table-dismiss",
                                "Delete Cells...",
                                t(T::DeleteCells),
                            ),
                            rs::cmd(
                                "delete-columns",
                                "table-delete-column",
                                "Delete Columns",
                                t(T::DeleteColumns),
                            ),
                            rs::cmd(
                                "delete-rows",
                                "table-delete-row",
                                "Delete Rows",
                                t(T::DeleteRows),
                            ),
                            rs::cmd(
                                "delete-table",
                                "table-dismiss",
                                "Delete Table",
                                t(T::DeleteTable),
                            ),
                        ],
                    },
                    rs::column(vec![
                        cmdt(
                            "insert-above",
                            "table-insert-row",
                            "Insert Above",
                            t(T::InsertAbove),
                            "",
                        )
                        .key("A"),
                        cmdt(
                            "insert-below",
                            "table-insert-row",
                            "Insert Below",
                            t(T::InsertBelow),
                            "",
                        )
                        .key("BE"),
                    ]),
                    rs::column(vec![
                        cmdt(
                            "insert-left",
                            "table-insert-column",
                            "Insert Left",
                            t(T::InsertLeft),
                            "",
                        )
                        .key("L"),
                        cmdt(
                            "insert-right",
                            "table-insert-column",
                            "Insert Right",
                            t(T::InsertRight),
                            "",
                        )
                        .key("R"),
                    ]),
                ],
            ),
            rs::group(
                "Merge",
                35,
                vec![rs::column(vec![
                    cmdt(
                        "merge-cells",
                        "table-merge",
                        "Merge Cells",
                        t(T::MergeCells),
                        "",
                    )
                    .key("M"),
                    cmdt(
                        "split-cells",
                        "table-split",
                        "Split Cells...",
                        t(T::SplitCells),
                        "",
                    )
                    .key("P"),
                    cmdt(
                        "split-table",
                        "table-split",
                        "Split Table",
                        t(T::SplitTable),
                        "",
                    )
                    .key("Q"),
                ])],
            ),
            rs::group(
                "Cell Size",
                25,
                vec![
                    Control::Dropdown {
                        cmd: cmdt(
                            "autofit",
                            "table-autofit",
                            "AutoFit",
                            t(T::AutoFit(AutoFitKind::Contents)),
                            "",
                        )
                        .key("F"),
                        items: vec![
                            rs::cmd(
                                "autofit-contents",
                                "table-autofit",
                                "AutoFit Contents",
                                t(T::AutoFit(AutoFitKind::Contents)),
                            ),
                            rs::cmd(
                                "autofit-window",
                                "table-autofit",
                                "AutoFit Window",
                                t(T::AutoFit(AutoFitKind::Window)),
                            ),
                            rs::cmd(
                                "autofit-fixed",
                                "table-autofit",
                                "Fixed Column Width",
                                t(T::AutoFit(AutoFitKind::Fixed)),
                            ),
                        ],
                    },
                    rs::column(vec![
                        cmdt(
                            "distribute-rows",
                            "table-distribute",
                            "Distribute Rows",
                            t(T::DistributeRows),
                            "",
                        )
                        .key("WR"),
                        cmdt(
                            "distribute-columns",
                            "table-distribute",
                            "Distribute Columns",
                            t(T::DistributeColumns),
                            "",
                        )
                        .key("WC"),
                    ]),
                ],
            ),
            rs::group(
                "Alignment",
                30,
                vec![
                    rs::rows(align_rows),
                    Control::Large(
                        cmdt(
                            "text-direction",
                            "text-direction",
                            "Text Direction",
                            t(T::TextDirection),
                            "",
                        )
                        .key("X"),
                    ),
                ],
            ),
            rs::group(
                "Data",
                20,
                vec![rs::column(vec![
                    cmdt("table-sort", "sort", "Sort...", t(T::Sort), "").key("SO"),
                    cmdt(
                        "convert-to-text",
                        "table-dismiss",
                        "Convert to Text...",
                        t(T::ConvertToText),
                        "",
                    )
                    .key("V"),
                ])],
            ),
        ],
    )
}

/// The Table Design tab.
pub(crate) fn table_design_tab() -> rs::Tab<Act> {
    use TableAct as T;
    let look = |id, label, f| cmdt(id, "table", label, t(T::Look(f)), "");
    let mut shading = vec![rs::cmd(
        "shade-none",
        "table-shading",
        "No Color",
        t(T::Shading(None)),
    )];
    shading.extend(
        SHADES
            .iter()
            .map(|&(id, label, rgb)| rs::cmd(id, "table-shading", label, t(T::Shading(Some(rgb))))),
    );
    rs::tab(
        DESIGN_TAB,
        "T",
        vec![
            rs::group(
                "Table Style Options",
                30,
                vec![
                    rs::column(vec![
                        look("header-row", "Header Row", LookFlag::HeaderRow),
                        look("total-row", "Total Row", LookFlag::TotalRow),
                        look("banded-rows", "Banded Rows", LookFlag::BandedRows),
                    ]),
                    rs::column(vec![
                        look("first-column", "First Column", LookFlag::FirstColumn),
                        look("last-column", "Last Column", LookFlag::LastColumn),
                        look("banded-columns", "Banded Columns", LookFlag::BandedColumns),
                    ]),
                ],
            ),
            rs::group(
                "Table Styles",
                40,
                vec![
                    Control::Gallery(rs::Gallery {
                        id: "tablestyles",
                        tip: rs::ScreenTip {
                            title: "Table Styles",
                            body: "Give the table a style",
                            shortcut: "",
                        },
                        items: BUILTIN
                            .iter()
                            .enumerate()
                            .map(|(i, s)| rs::GalleryItem {
                                label: s.name,
                                preview: s.id,
                                act: t(T::Style(i)),
                            })
                            .collect(),
                    }),
                    Control::Dropdown {
                        cmd: cmdt(
                            "shading",
                            "table-shading",
                            "Shading",
                            t(T::Shading(None)),
                            "",
                        )
                        .key("H"),
                        items: shading,
                    },
                ],
            ),
            rs::group(
                "Borders",
                35,
                vec![Control::Dropdown {
                    cmd: cmdt(
                        "borders",
                        "border-bottom",
                        "Borders",
                        t(T::Borders(BorderCmd::Bottom)),
                        "",
                    )
                    .key("B"),
                    items: BorderCmd::ALL
                        .iter()
                        .map(|&b| {
                            rs::cmd(border_id(b), "border-bottom", b.label(), t(T::Borders(b)))
                        })
                        .collect(),
                }],
            ),
        ],
    )
}

/// The Insert > Table drop-down's items: the hover grid, then Word's items.
pub(crate) fn insert_table_menu(has_selection: bool) -> Vec<menu::MenuItem> {
    use menu::{Entry, MenuItem};
    vec![
        MenuItem::TableGrid {
            cols: GRID_COLS,
            rows: GRID_ROWS,
        },
        MenuItem::Separator,
        MenuItem::Item(Entry::new(
            "insert-table-dialog",
            "Insert Table...",
            "table",
            t(TableAct::InsertTable),
            true,
        )),
        MenuItem::Item(Entry::unavailable("draw-table", "Draw Table")),
        MenuItem::Item(Entry::new(
            "convert-text-to-table",
            "Convert Text to Table...",
            "table",
            t(TableAct::TextToTable),
            has_selection,
        )),
        MenuItem::Item(Entry::unavailable("excel-spreadsheet", "Excel Spreadsheet")),
        MenuItem::Item(Entry::unavailable("quick-tables", "Quick Tables")),
    ]
}

/// The hover grid's header: Word's "Insert Table", or "<cols>x<rows> Table"
/// while the pointer is over the grid.
pub(crate) fn grid_header(hover: Option<(usize, usize)>) -> String {
    match hover {
        Some((c, r)) => format!("{c}x{r} Table"),
        None => "Insert Table".into(),
    }
}

/// Whether a table command can run in `ed`'s state (`None`: no document).
pub(crate) fn table_enabled(ed: Option<&Editor>, act: TableAct) -> bool {
    use TableAct as T;
    let Some(ed) = ed else {
        return false;
    };
    match act {
        T::Unavailable => false,
        T::InsertTable => true,
        T::TextToTable => ed.has_selection() && ed.cell_range().is_none(),
        T::MergeCells => ed.cell_range().is_some(),
        _ => ed.in_table(),
    }
}

/// Whether a table command shows checked.
pub(crate) fn table_checked(ed: Option<&Editor>, gridlines: bool, act: TableAct) -> bool {
    use TableAct as T;
    if act == T::ViewGridlines {
        return gridlines;
    }
    let Some(ed) = ed.filter(|e| e.in_table()) else {
        return false;
    };
    match act {
        T::Look(f) => ed.table_look().is_some_and(|l| f.get(&l)),
        T::Style(i) => BUILTIN.get(i).map(|s| s.id) == ed.table_style().as_deref(),
        T::Borders(b) => ed.border_state(b),
        T::Align(v, h) => ed.cell_alignment() == Some((v, h)),
        T::Shading(Some(rgb)) => ed.cell_shading() == Some(format!("{rgb:06X}")),
        _ => false,
    }
}

/// Run a table command that edits the document (every one but the dialogs
/// and View Gridlines) on `ed`. The status line text on success; the refusal
/// otherwise.
pub(crate) fn table_apply(ed: &mut Editor, act: TableAct) -> Result<String, String> {
    use TableAct as T;
    let done = |label: &str| Ok(label.to_string());
    match act {
        T::Select(SelectWhat::Cell) => ed.select_cell().and(done("Cell selected")),
        T::Select(SelectWhat::Column) => ed.select_column().and(done("Column selected")),
        T::Select(SelectWhat::Row) => ed.select_row().and(done("Row selected")),
        T::Select(SelectWhat::Table) => ed.select_table().and(done("Table selected")),
        T::InsertAbove => ed.insert_rows(true).and(done("Inserted above")),
        T::InsertBelow => ed.insert_rows(false).and(done("Inserted below")),
        T::InsertLeft => ed.insert_columns(true).and(done("Inserted left")),
        T::InsertRight => ed.insert_columns(false).and(done("Inserted right")),
        T::DeleteColumns => ed.delete_columns().and(done("Columns deleted")),
        T::DeleteRows => ed.delete_rows().and(done("Rows deleted")),
        T::DeleteTable => ed.delete_table().and(done("Table deleted")),
        T::MergeCells => ed.merge_cells().and(done("Cells merged")),
        T::SplitTable => ed.split_table().and(done("Table split")),
        T::AutoFit(k) => ed.autofit(k).and(done(match k {
            AutoFitKind::Contents => "AutoFit Contents",
            AutoFitKind::Window => "AutoFit Window",
            AutoFitKind::Fixed => "Fixed Column Width",
        })),
        T::DistributeRows => ed.distribute_rows().and(done("Rows distributed")),
        T::DistributeColumns => ed.distribute_columns().and(done("Columns distributed")),
        T::Align(v, h) => ed.set_cell_alignment(v, h).and(done("Cell alignment")),
        T::TextDirection => ed.cycle_text_direction().and(Ok(format!(
            "Text direction: {}",
            ed.cell_text_direction().as_deref().unwrap_or("horizontal")
        ))),
        T::Look(f) => {
            let mut look = ed.table_look().ok_or("the caret is not in a table")?;
            f.toggle(&mut look);
            ed.set_table_look(look).and(done("Table style options"))
        }
        T::Style(i) => {
            let s = BUILTIN.get(i).ok_or("no such table style")?;
            ed.set_table_style(s.id)
                .and(Ok(format!("Table style: {}", s.name)))
        }
        T::Shading(fill) => {
            let hex = fill.map(|c| format!("{c:06X}"));
            ed.set_cell_shading(hex.as_deref()).and(done("Shading"))
        }
        T::Borders(b) => ed.apply_borders(b).and(Ok(b.label().to_string())),
        T::ViewGridlines
        | T::DeleteCells
        | T::SplitCells
        | T::Sort
        | T::ConvertToText
        | T::InsertTable
        | T::TextToTable
        | T::Unavailable => Ok(String::new()),
    }
}

/// Whether `act` is a table command that changes the document when it
/// succeeds (Select only moves the selection).
fn edits(act: TableAct) -> bool {
    !matches!(act, TableAct::Select(_))
}

impl Docxy {
    /// The editor table commands act on (the open header or footer, else the
    /// body), read-only.
    pub(crate) fn edit_target_ref(&self) -> Option<&Editor> {
        let tab = self.tabs.get(self.active)?;
        if let Some(hf) = tab.hf_edit.as_ref() {
            return Some(&hf.editor);
        }
        match &tab.surface {
            Surface::Doc(ed) => Some(ed),
            _ => None,
        }
    }

    /// Whether the caret of the edited story is in a table: the contextual
    /// table tabs show.
    pub(crate) fn caret_in_table(&self) -> bool {
        self.edit_target_ref().is_some_and(Editor::in_table)
    }

    /// Dispatch a table command.
    pub(crate) fn table_act(&mut self, act: TableAct, window: &mut Window, cx: &mut Context<Self>) {
        use TableAct as T;
        let dialog = match act {
            T::ViewGridlines => {
                self.view_gridlines = !self.view_gridlines;
                return self.refocus(window, cx);
            }
            T::Unavailable => return self.refocus(window, cx),
            T::DeleteCells => Some(crate::table_dialogs::delete_cells_dialog as DialogBuilder),
            T::SplitCells => Some(crate::table_dialogs::split_cells_dialog as DialogBuilder),
            T::Sort => Some(crate::table_dialogs::sort_dialog as DialogBuilder),
            T::ConvertToText => Some(crate::table_dialogs::table_to_text_dialog as DialogBuilder),
            T::InsertTable => Some(crate::table_dialogs::insert_table_dialog as DialogBuilder),
            T::TextToTable => Some(crate::table_dialogs::text_to_table_dialog as DialogBuilder),
            _ => None,
        };
        if let Some(build) = dialog {
            self.open_table_dialog(build);
            return self.refocus(window, cx);
        }
        let result = self.edit_target().map(|ed| table_apply(ed, act));
        if let Some(tab) = self.tabs.get_mut(self.active) {
            match result {
                Some(Ok(status)) => {
                    if edits(act) {
                        tab.set_dirty();
                    }
                    tab.status = status.into();
                }
                Some(Err(e)) => tab.status = e.into(),
                None => {}
            }
        }
        // A table deleted or converted away takes its tabs with it.
        if !self.caret_in_table()
            && matches!(
                self.ribbon_tab,
                RibbonTab::TableDesign | RibbonTab::TableLayout
            )
        {
            self.ribbon_tab = RibbonTab::Home;
        }
        self.scroll_to_caret();
        self.refocus(window, cx);
    }

    /// Open a table dialog over the active document, or say why not.
    fn open_table_dialog(&mut self, build: DialogBuilder) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let ed = match (tab.hf_edit.as_ref(), &tab.surface) {
            (Some(hf), _) => &hf.editor,
            (None, Surface::Doc(ed)) => ed,
            _ => return,
        };
        match build(ed) {
            Ok(d) => tab.dialogs.push(d),
            Err(e) => tab.status = e.into(),
        }
    }

    /// The pointer over the Insert > Table grid's cell `(cols, rows)`
    /// (`None`: off the grid). The header follows it.
    pub(crate) fn table_grid_hover(
        &mut self,
        at: Option<(usize, usize)>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.table_grid_open()?;
        if let Some((c, r)) = at {
            if !(1..=GRID_COLS).contains(&c) || !(1..=GRID_ROWS).contains(&r) {
                return Err(format!(
                    "the grid is {GRID_COLS} columns by {GRID_ROWS} rows"
                ));
            }
        }
        if self.table_grid_hover != at {
            self.table_grid_hover = at;
            cx.notify();
        }
        Ok(())
    }

    /// A click on the Insert > Table grid's cell `(cols, rows)`: insert that
    /// table (columns first, as Word labels it) and close the menu.
    pub(crate) fn table_grid_click(
        &mut self,
        cols: usize,
        rows: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.table_grid_hover(Some((cols, rows)), cx)?;
        self.close_menu();
        self.table_grid_hover = None;
        self.insert_table(rows, cols, docxcore::table::AutoFit::Default, window, cx);
        Ok(())
    }

    fn table_grid_open(&self) -> Result<(), String> {
        let open = self.menu.as_ref().is_some_and(|m| {
            m.items
                .iter()
                .any(|i| matches!(i, menu::MenuItem::TableGrid { .. }))
        });
        if open {
            Ok(())
        } else {
            Err("the Insert > Table menu is not open".into())
        }
    }
}

impl Docxy {
    /// The Insert > Table hover grid, drawn in its menu: the header, then the
    /// cells, those up to the pointer's highlighted.
    pub(crate) fn table_grid_el(
        &self,
        cols: usize,
        rows: usize,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = self.table_grid_hover;
        let lit_bg = Hsla {
            a: 0.25,
            ..hsla_u(BRAND)
        };
        let grid = v_flex()
            .id("table-grid")
            .gap(px(2.))
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                if !*over {
                    let _ = this.table_grid_hover(None, cx);
                }
            }))
            .children((1..=rows).map(|r| {
                h_flex().gap(px(2.)).children((1..=cols).map(|c| {
                    let lit = hover.is_some_and(|(hc, hr)| c <= hc && r <= hr);
                    div()
                        .id(("table-grid-cell", r * 100 + c))
                        .size(px(15.))
                        .border_1()
                        .border_color(if lit { hsla_u(BRAND) } else { pal.border })
                        .bg(if lit { lit_bg } else { hsla_u(0xFFFFFF) })
                        .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                            if *over {
                                let _ = this.table_grid_hover(Some((c, r)), cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Err(e) = this.table_grid_click(c, r, window, cx) {
                                    this.set_status(e);
                                }
                            }),
                        )
                }))
            }));
        v_flex()
            .px_3()
            .py_1()
            .gap_1()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(pal.fg)
                    .child(SharedString::from(grid_header(hover))),
            )
            .child(grid)
            .into_any_element()
    }

    /// The Table Styles gallery: a tile per built-in style, each a small
    /// table drawn in that style (header row, banding and borders, resolved
    /// as the document view resolves them), the table's own style outlined.
    pub(crate) fn table_style_gallery(
        &self,
        gal: &rs::Gallery<Act>,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use docxcore::table::{AutoFit, edit_table_props, new_table};
        let tiles: Vec<AnyElement> = gal
            .items
            .iter()
            .map(|it| {
                let act = it.act;
                let selected = self.act_active(act);
                let mut sample = new_table(4, 4, 4 * 240, AutoFit::Default);
                // A new table already has Word's default style options.
                edit_table_props(&mut sample, |p| {
                    p.set(&format!("<w:tblStyle w:val=\"{}\"/>", it.preview))
                });
                let style = docxcore::table_styles::lookup_style(None, it.preview);
                let boxes = crate::table_view::layout(&sample, style.as_ref());
                let ink = hsla_u(0x404040);
                let side =
                    |l: Option<crate::table_view::Line>| l.map(|l| l.color.map_or(ink, hsla_u));
                let rows = boxes.iter().map(|rb| {
                    h_flex().children(rb.cells.iter().map(|cb| {
                        let mut d = div()
                            .relative()
                            .w(px(12.))
                            .h(px(8.))
                            .bg(cb.fill.map_or(hsla_u(0xFFFFFF), hsla_u));
                        for (k, l) in [cb.top, cb.bottom, cb.left, cb.right]
                            .into_iter()
                            .enumerate()
                        {
                            if let Some(c) = side(l) {
                                let line = div().absolute().bg(c);
                                d = d.child(match k {
                                    0 => line.top_0().left_0().right_0().h(px(1.)),
                                    1 => line.bottom_0().left_0().right_0().h(px(1.)),
                                    2 => line.top_0().bottom_0().left_0().w(px(1.)),
                                    _ => line.top_0().bottom_0().right_0().w(px(1.)),
                                });
                            }
                        }
                        d
                    }))
                });
                div()
                    .id(it.preview)
                    .flex_none()
                    .p(px(3.))
                    .rounded(px(3.))
                    .border_2()
                    .border_color(if selected {
                        hsla_u(BRAND)
                    } else {
                        gpui::transparent_black()
                    })
                    .cursor_pointer()
                    .hover(|d| d.border_color(pal.border))
                    .child(v_flex().children(rows))
                    .tooltip({
                        let label = it.label;
                        move |w, cx| Tooltip::new(label).build(w, cx)
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        h_flex()
            .flex_none()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(3.))
            .border_1()
            .border_color(pal.border)
            .bg(hsla_u(0xFAFAFA))
            .children(tiles)
            .into_any_element()
    }
}

/// Builds a table dialog from the edited story's editor.
pub(crate) type DialogBuilder = fn(&Editor) -> Result<crate::dialog::Dialog, String>;

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::editor::Caret;
    use docxcore::model::Document;
    use docxcore::table::{AutoFit, new_table};

    fn ed_in_table() -> Editor {
        let mut ed = Editor::new(Document {
            body: vec![
                Block::Table(new_table(2, 2, 9000, AutoFit::Default)),
                Block::Paragraph(Paragraph::default()),
            ],
        });
        ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
        ed
    }

    fn commands(tab: &rs::Tab<Act>) -> Vec<(&'static str, Act)> {
        let mut out = Vec::new();
        for g in &tab.groups {
            for c in &g.items {
                match c {
                    Control::Large(c) | Control::Toggle(c) => out.push((c.id, c.act)),
                    Control::Column(cs) => out.extend(cs.iter().map(|c| (c.id, c.act))),
                    Control::Dropdown { cmd, items }
                    | Control::Split {
                        primary: cmd,
                        menu: items,
                    } => {
                        out.push((cmd.id, cmd.act));
                        out.extend(items.iter().map(|c| (c.id, c.act)));
                    }
                    Control::Rows(rows) => {
                        for cell in rows.iter().flatten() {
                            if let rs::Cell::Btn(c) = cell {
                                out.push((c.id, c.act));
                            }
                        }
                    }
                    Control::Gallery(g) => out.extend(g.items.iter().map(|i| (i.preview, i.act))),
                    Control::Separator => {}
                }
            }
        }
        out
    }

    #[test]
    fn the_layout_tab_has_words_groups_and_commands() {
        let tab = table_layout_tab();
        assert_eq!(tab.name, LAYOUT_TAB);
        let groups: Vec<_> = tab.groups.iter().map(|g| g.title).collect();
        assert_eq!(
            groups,
            [
                "Table",
                "Rows & Columns",
                "Merge",
                "Cell Size",
                "Alignment",
                "Data"
            ]
        );
        let ids: Vec<_> = commands(&tab).into_iter().map(|(id, _)| id).collect();
        for id in [
            "select-cell",
            "select-column",
            "select-row",
            "select-table",
            "gridlines",
            "delete-cells",
            "insert-above",
            "insert-right",
            "merge-cells",
            "split-cells",
            "split-table",
            "autofit-window",
            "distribute-columns",
            "align-bottom-right",
            "text-direction",
            "table-sort",
            "convert-to-text",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
    }

    #[test]
    fn the_design_tab_has_options_gallery_shading_and_borders() {
        let tab = table_design_tab();
        assert_eq!(tab.name, DESIGN_TAB);
        let cmds = commands(&tab);
        let ids: Vec<_> = cmds.iter().map(|(id, _)| *id).collect();
        for id in [
            "header-row",
            "banded-columns",
            "GridTable4-Accent1",
            "TableGrid",
            "shade-none",
            "border-none",
            "border-diag-up",
            "border-inside-v",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
        assert_eq!(
            cmds.iter()
                .filter(|(_, a)| matches!(a, Act::Table(TableAct::Borders(_))))
                .count(),
            13,
            "the Borders button and its twelve items"
        );
    }

    #[test]
    fn enabled_follows_the_selection() {
        let mut ed = ed_in_table();
        assert!(table_enabled(Some(&ed), TableAct::InsertAbove));
        assert!(!table_enabled(Some(&ed), TableAct::MergeCells));
        assert!(!table_enabled(Some(&ed), TableAct::TextToTable));
        assert!(!table_enabled(Some(&ed), TableAct::Unavailable));
        ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![0, 1, 1, 0], 0);
        assert!(table_enabled(Some(&ed), TableAct::MergeCells));
        // Outside a table: Convert Text to Table needs a selection.
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![Inline::Run(docxcore::model::Run {
                    text: "a\tb".into(),
                    props: RunProps::default(),
                })],
                ..Paragraph::default()
            })],
        });
        assert!(!table_enabled(Some(&ed), TableAct::InsertAbove));
        assert!(!table_enabled(Some(&ed), TableAct::TextToTable));
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(0, 3);
        assert!(table_enabled(Some(&ed), TableAct::TextToTable));
        assert!(!table_enabled(None, TableAct::InsertTable));
    }

    #[test]
    fn apply_runs_the_docxcore_command_and_checks_follow() {
        let mut ed = ed_in_table();
        let s = table_apply(&mut ed, TableAct::Style(7)).unwrap();
        assert_eq!(s, "Table style: Grid Table 4 Accent 1");
        assert!(table_checked(Some(&ed), true, TableAct::Style(7)));
        assert!(!table_checked(Some(&ed), true, TableAct::Style(0)));
        assert!(table_checked(
            Some(&ed),
            true,
            TableAct::Look(LookFlag::HeaderRow)
        ));
        table_apply(&mut ed, TableAct::Look(LookFlag::HeaderRow)).unwrap();
        assert!(!table_checked(
            Some(&ed),
            true,
            TableAct::Look(LookFlag::HeaderRow)
        ));
        table_apply(&mut ed, TableAct::Align(VAlign::Bottom, Align::Right)).unwrap();
        assert!(table_checked(
            Some(&ed),
            true,
            TableAct::Align(VAlign::Bottom, Align::Right)
        ));
        table_apply(&mut ed, TableAct::Shading(Some(0xFF0000))).unwrap();
        assert!(table_checked(
            Some(&ed),
            true,
            TableAct::Shading(Some(0xFF0000))
        ));
        assert!(table_checked(
            Some(&ed),
            false,
            TableAct::Borders(BorderCmd::All)
        ));
        table_apply(&mut ed, TableAct::Borders(BorderCmd::All)).unwrap();
        assert!(!table_checked(
            Some(&ed),
            false,
            TableAct::Borders(BorderCmd::All)
        ));
        assert!(table_checked(Some(&ed), true, TableAct::ViewGridlines));
        assert!(!table_checked(Some(&ed), false, TableAct::ViewGridlines));
        table_apply(&mut ed, TableAct::InsertBelow).unwrap();
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 3);
        assert!(table_apply(&mut ed, TableAct::MergeCells).is_err());
    }

    #[test]
    fn the_insert_table_menu_and_grid_header() {
        let items = insert_table_menu(false);
        assert!(matches!(
            items[0],
            menu::MenuItem::TableGrid { cols: 10, rows: 8 }
        ));
        let labels: Vec<(String, bool)> = items
            .iter()
            .filter_map(|i| match i {
                menu::MenuItem::Item(e) => Some((e.label.clone(), e.enabled)),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("Insert Table...".to_string(), true),
                ("Draw Table".to_string(), false),
                ("Convert Text to Table...".to_string(), false),
                ("Excel Spreadsheet".to_string(), false),
                ("Quick Tables".to_string(), false),
            ]
        );
        assert_eq!(grid_header(None), "Insert Table");
        assert_eq!(grid_header(Some((4, 3))), "4x3 Table");
    }
}
