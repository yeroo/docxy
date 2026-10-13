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
use crate::table_draw::{
    self, Drag, DrawMode, ERASER_STATUS, Gesture, PEN_STATUS, ParaPlace, Tool,
};
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
    /// Draw > Draw Table (and Insert > Table > Draw Table): the pen (#763).
    DrawTable,
    /// Draw > Eraser: erasing a line merges the cells on either side (#763).
    Eraser,
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
                "Draw",
                20,
                vec![
                    Control::Large(
                        cmdt(
                            "draw-table",
                            "table-draw",
                            "Draw Table",
                            t(T::DrawTable),
                            "",
                        )
                        .key("N"),
                    ),
                    Control::Large(
                        cmdt("eraser", "table-eraser", "Eraser", t(T::Eraser), "").key("E"),
                    ),
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
                STYLES_GROUP,
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
        MenuItem::Item(Entry::new(
            "draw-table",
            "Draw Table",
            "table-draw",
            t(TableAct::DrawTable),
            true,
        )),
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
        T::InsertTable | T::DrawTable => true,
        T::TextToTable => ed.has_selection() && ed.cell_range().is_none(),
        T::MergeCells => ed.cell_range().is_some(),
        _ => ed.in_table(),
    }
}

/// Whether a table command shows checked. `draw` is the pen on in the
/// active tab, if any.
pub(crate) fn table_checked(
    ed: Option<&Editor>,
    gridlines: bool,
    draw: Option<Tool>,
    act: TableAct,
) -> bool {
    use TableAct as T;
    match act {
        T::ViewGridlines => return gridlines,
        T::DrawTable => return draw == Some(Tool::Pen),
        T::Eraser => return draw == Some(Tool::Eraser),
        _ => {}
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
        | T::DrawTable
        | T::Eraser
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
            T::DrawTable => return self.draw_toggle(Tool::Pen, window, cx),
            T::Eraser => return self.draw_toggle(Tool::Eraser, window, cx),
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
            match at {
                Some((cols, rows)) => self.set_doc_preview(PreviewKind::Grid { cols, rows }),
                None => self.drop_doc_preview(PreviewKind::is_grid),
            }
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
        // Closing the menu forgets the hover and drops its preview.
        self.close_menu();
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
    /// The table pen on in the active tab (#763), if any.
    pub(crate) fn draw_tool(&self) -> Option<Tool> {
        self.table_draw
            .filter(|m| m.tab == self.active)
            .map(|m| m.tool)
    }

    /// A click on Draw Table or Eraser: that pen on, or off when it was on.
    /// A pen replaces the highlighter (#623).
    fn draw_toggle(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.table_draw.take();
        self.cancel_pointer_modes();
        self.table_draw = mode;
        self.draw_drag = None;
        // The pen makes no selection, so no mini toolbar comes up over the
        // page while it is on.
        self.mini_bar = None;
        let on = DrawMode::click(&mut self.table_draw, tool, self.active);
        self.set_status(if on { tool.status() } else { DRAW_OFF });
        self.refocus(window, cx);
    }

    /// End the table pens, whatever ended them, and take their status text
    /// off the tab they were on. True when a pen was on for the active tab.
    pub(crate) fn cancel_draw_mode(&mut self) -> bool {
        self.draw_drag = None;
        let Some(m) = self.table_draw.take() else {
            return false;
        };
        if let Some(t) = self.tabs.get_mut(m.tab) {
            if t.status == PEN_STATUS || t.status == ERASER_STATUS {
                t.status = DRAW_OFF.into();
            }
        }
        m.tab == self.active
    }

    /// Whether a window point is on the document's pages as the frame on
    /// screen laid them out: Print Layout's sheets, or Draft's whole view.
    fn on_page(&self, (x, y): (f32, f32)) -> bool {
        self.page_rects().iter().any(|r| r.contains(x, y))
    }

    /// The pages' rects on screen (clipped to the view): Print Layout's
    /// sheets, or Draft's view, which is all page.
    pub(crate) fn page_rects(&self) -> Vec<ScreenRect> {
        let probe = self.ruler_probe.borrow();
        let Some(view) = probe.viewport else {
            return Vec::new();
        };
        if probe.draft {
            return vec![view];
        }
        probe
            .pages
            .iter()
            .flatten()
            .filter(|r| r.intersects(&view))
            .map(|r| {
                let (x, y) = (r.x.max(view.x), r.y.max(view.y));
                ScreenRect {
                    x,
                    y,
                    w: (r.x + r.w).min(view.x + view.w) - x,
                    h: (r.y + r.h).min(view.y + view.h) - y,
                }
            })
            .collect()
    }

    /// A left press with a pen on: `Ok` when it lands on the page and starts
    /// a gesture, which the press then belongs to alone. Under anything drawn
    /// over the page (a menu, the ribbon flyout, the tab popup, a dialog, the
    /// File view), off the page, or with a header or footer open, outside
    /// its area, the press is left to the rest of the window, and the reason
    /// says which.
    pub(crate) fn draw_press(&mut self, at: (f32, f32)) -> Result<(), &'static str> {
        if self.draw_tool().is_none() {
            return Err(table_draw::NO_PEN);
        }
        let overlays = table_draw::Overlays {
            menu: self.menu.is_some(),
            ribbon_flyout: self.ribbon_flyout.is_some(),
            tab_popup: self.tab_more_open,
            dialog: self
                .tabs
                .get(self.active)
                .is_none_or(|t| t.dialogs.is_open()),
            backstage: self.backstage,
        };
        if overlays.cover_page() {
            return Err(table_draw::COVERED);
        }
        if !self.on_page(at) {
            return Err(table_draw::OFF_PAGE);
        }
        // With a header or footer open, only its area takes the pen.
        let story = self
            .draw_geom
            .borrow()
            .on_screen()
            .story_at(self.hf_active(), at);
        let hf = story.ok_or(table_draw::OUTSIDE_HF)?;
        self.mini_bar = None;
        self.draw_drag = Some(Drag {
            from: at,
            to: at,
            hf,
        });
        Ok(())
    }

    /// The pointer moved during a pen gesture: the preview follows it.
    pub(crate) fn draw_move(&mut self, at: (f32, f32)) -> bool {
        let Some(d) = self.draw_drag.as_mut() else {
            return false;
        };
        d.to = at;
        true
    }

    /// The release that ends a pen gesture: read it and run it.
    pub(crate) fn draw_release(
        &mut self,
        at: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut d) = self.draw_drag.take() else {
            return false;
        };
        d.to = at;
        let _ = self.draw_gesture(d, window, cx);
        true
    }

    /// Read the gesture `d` on the frame on screen and run it: the edit on
    /// the edited story, its status text or its refusal on the status line.
    /// A gesture leaves the mode on, as in Word; a refusal in Protected View
    /// or a final document ends it.
    pub(crate) fn draw_gesture(
        &mut self,
        d: Drag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let tool = self.draw_tool().ok_or(table_draw::NO_PEN)?;
        let (gesture, para) = {
            let geom = self.draw_geom.borrow();
            let frame = geom.on_screen();
            let g = table_draw::classify(frame, d.hf, tool, d.from, d.to, self.zoom);
            let pages: Vec<(f32, f32)> = self
                .ruler_probe
                .borrow()
                .pages
                .iter()
                .flatten()
                .map(|r| (r.y, r.y + r.h))
                .collect();
            (g, frame.para_for(d.hf, d.from, &pages))
        };
        if matches!(gesture, Gesture::Nothing) {
            cx.notify();
            return Ok(String::new());
        }
        if self.protected_refused(cx) {
            self.cancel_draw_mode();
            let why = self.tabs.get(self.active).map(|t| t.status.to_string());
            self.refocus(window, cx);
            return Err(why.unwrap_or_default());
        }
        let result = match self.edit_target() {
            Some(ed) => draw_on(ed, gesture, para),
            None => Err("no document is open".into()),
        };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            match &result {
                Ok(status) => {
                    tab.set_dirty();
                    tab.status = status.clone().into();
                }
                Err(e) => tab.status = e.clone().into(),
            }
        }
        self.refocus(window, cx);
        result
    }
}

impl Docxy {
    /// With a table pen on (#763): a crosshair over the pages, and while a
    /// gesture is drawn, its dashed preview: the rectangle of a new table, or
    /// the line along the drag's main axis inside a table.
    pub(crate) fn draw_overlay(&self) -> Option<AnyElement> {
        let tool = self.draw_tool()?;
        let pages: Vec<Bounds<Pixels>> = self
            .page_rects()
            .iter()
            .map(|r| Bounds::new(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))))
            .collect();
        let preview = self.draw_drag.filter(|_| tool == Tool::Pen).map(|d| {
            let in_table = self
                .draw_geom
                .borrow()
                .on_screen()
                .tables
                .iter()
                .any(|t| t.hf == d.hf && t.holds(d.from, 0.0));
            table_draw::preview(d, in_table)
        });
        let ink = hsla_u(BRAND);
        Some(
            canvas(
                move |_, window, _| {
                    pages
                        .iter()
                        .map(|b| window.insert_hitbox(*b, HitboxBehavior::Normal))
                        .collect::<Vec<_>>()
                },
                move |_, hitboxes, window, _| {
                    for h in &hitboxes {
                        window.set_cursor_style(CursorStyle::Crosshair, h);
                    }
                    let Some(((x0, y0), (x1, y1))) = preview else {
                        return;
                    };
                    let b = Bounds::new(
                        point(px(x0.min(x1)), px(y0.min(y1))),
                        size(px((x1 - x0).abs().max(1.0)), px((y1 - y0).abs().max(1.0))),
                    );
                    window.paint_quad(gpui::outline(b, ink, gpui::BorderStyle::Dashed));
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }
}

/// A rectangle drawn on a page with no paragraph of its own to place it by.
const NO_PARAGRAPH: &str = "there is no paragraph on this page to draw the table by";

/// The status line after a pen is put away.
const DRAW_OFF: &str = "Drawing off";

/// Run a pen gesture on `ed`, one undo step; a new table goes after the
/// top-level paragraph the rectangle was drawn from (or before the first
/// paragraph of its page, drawn above it), never inside one, so it never
/// splits a paragraph or joins a table (`para`, from `Frame::para_for`).
fn draw_on(ed: &mut Editor, gesture: Gesture, para: Option<ParaPlace>) -> Result<String, String> {
    let mut result = Ok(String::new());
    match gesture {
        Gesture::Nothing => {}
        Gesture::Refused(why) => result = Err(why.to_string()),
        Gesture::NewTable { w_tw, h_tw } => ed.one_step("Draw Table", |e| {
            let drawn = match para {
                Some(ParaPlace::After(b)) => e.draw_table_after(&[b], w_tw, h_tw),
                Some(ParaPlace::Before(b)) => e.draw_table_before(&[b], w_tw, h_tw),
                None => Err(NO_PARAGRAPH.into()),
            };
            result = drawn.map(|()| "Table drawn".into());
        }),
        Gesture::Line { table, line } => ed.one_step("Draw Table", |e| {
            result = e
                .draw_table_line(&table, line)
                .map(|()| "Line drawn".into());
        }),
        Gesture::Erase { table, edge } => ed.one_step("Eraser", |e| {
            result = e
                .erase_table_line(&table, edge)
                .map(|()| "Line erased".into());
        }),
    }
    result
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
    /// In a flyout narrower than the gallery (`max_w`), the tiles wrap onto
    /// rows (#1020).
    pub(crate) fn table_style_gallery(
        &self,
        gal: &rs::Gallery<Act>,
        max_w: Option<f32>,
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
                    .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                        let Act::Table(TableAct::Style(i)) = act else {
                            return;
                        };
                        if *over {
                            this.table_style_hover(Some(i), cx);
                        } else if this
                            .doc_preview
                            .as_ref()
                            .is_some_and(|p| p.kind == PreviewKind::Style(i))
                        {
                            // Leaving this tile; another one may already have
                            // taken the preview.
                            this.table_style_hover(None, cx);
                        }
                    }))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        let geom = crate::ribbon_fit::gallery_geom(gal);
        let (_, well_w) = crate::ribbon_fit::gallery_row(geom, gal.items.len(), max_w);
        h_flex()
            .flex_none()
            .flex_wrap()
            .w(px(well_w))
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

/// The Table Design group that holds the Table Styles gallery.
const STYLES_GROUP: &str = "Table Styles";

/// What a live preview (#764) shows: the table the Insert > Table grid's
/// hovered cell would insert, or the caret's table in a hovered Table
/// Styles tile's style (`BUILTIN[i]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PreviewKind {
    Grid { cols: usize, rows: usize },
    Style(usize),
}

impl PreviewKind {
    pub(crate) fn is_grid(self) -> bool {
        matches!(self, PreviewKind::Grid { .. })
    }
}

/// A live preview (#764): the document as a hovered command would leave
/// it, drawn in place of the real one and never saved. It holds the
/// document it was built from (the tab, the edit generation, the caret and
/// selection), and is drawn only while that is still what the tab shows.
pub(crate) struct DocPreview {
    pub(crate) tab: usize,
    /// How many documents the window had, as `RibbonFlyout` keeps it: a
    /// close shifts the indexes, and another document can take `tab`'s.
    pub(crate) docs: usize,
    pub(crate) generation: u64,
    pub(crate) caret: docxcore::editor::Caret,
    pub(crate) anchor: Option<docxcore::editor::Caret>,
    pub(crate) kind: PreviewKind,
    pub(crate) editor: Editor,
}

impl DocPreview {
    /// Whether this preview still shows `ed`, tab `active` of `docs`'s
    /// editor, at edit generation `generation`: built from that tab, with no
    /// edit since, the
    /// caret and selection where they were, and the hovered control
    /// `showing`. A caret moved by a key while the pointer rests on a tile
    /// would otherwise draw the old table restyled.
    pub(crate) fn current(
        &self,
        (active, docs): (usize, usize),
        ed: &Editor,
        generation: u64,
        showing: bool,
    ) -> bool {
        showing
            && self.tab == active
            && self.docs == docs
            && self.generation == generation
            && self.caret == ed.caret
            && self.anchor == ed.anchor
    }
}

/// What the frame drew in place of the document (#764), for the harness's
/// `state.preview`: the preview's kind, its tables (nested ones too), the
/// caret table's style, and the pages (Print Layout only).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnPreview {
    pub(crate) kind: PreviewKind,
    pub(crate) tables: usize,
    pub(crate) style: Option<String>,
    pub(crate) pages: Option<usize>,
}

impl DrawnPreview {
    /// What drawing `p` shows; the Print Layout path adds its pages.
    pub(crate) fn of(p: &DocPreview) -> Self {
        DrawnPreview {
            kind: p.kind,
            tables: count_tables(&p.editor.doc.body),
            style: p.editor.table_style(),
            pages: None,
        }
    }
}

/// The tables in `blocks`, nested ones included.
pub(crate) fn count_tables(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Table(t) => {
                1 + t
                    .rows
                    .iter()
                    .flat_map(|r| &r.cells)
                    .map(|c| count_tables(&c.blocks))
                    .sum::<usize>()
            }
            _ => 0,
        })
        .sum()
}

/// `target` as `kind`'s command would leave it, on a copy: the real editor
/// is only read. `None` when the command would be refused there, so a
/// refused preview draws nothing different.
pub(crate) fn build_preview(target: &Editor, kind: PreviewKind) -> Option<Editor> {
    let mut ed = target.preview_copy();
    match kind {
        PreviewKind::Grid { cols, rows } => ed
            .insert_table(rows, cols, docxcore::table::AutoFit::Default)
            .ok()?,
        PreviewKind::Style(i) => ed.set_table_style(BUILTIN.get(i)?.id).ok()?,
    }
    Some(ed)
}

/// Whether tab `tab` takes a live preview: a document's body, not a header
/// or footer being edited (drawn by its own path), not Protected View or a
/// final document, not a Display for Review view that shows text other
/// than the document's (`view_only`, where the command is refused), and no
/// table pen on (`pen`: the pens read the geometry the frame drew).
pub(crate) fn preview_allowed(tab: &DocTab, view_only: bool, pen: bool) -> bool {
    matches!(tab.surface, Surface::Doc(_))
        && tab.hf_edit.is_none()
        && !tab.access.locked()
        && !view_only
        && !pen
}

impl Docxy {
    /// Show `kind`'s live preview in the active document, or none where
    /// its command would be refused. Built once here, on a hover change,
    /// never per frame.
    fn set_doc_preview(&mut self, kind: PreviewKind) {
        self.doc_preview = self.doc_preview_of(kind);
    }

    fn doc_preview_of(&self, kind: PreviewKind) -> Option<DocPreview> {
        let tab = self.tabs.get(self.active)?;
        if !preview_allowed(tab, self.view_only_active(), self.draw_tool().is_some()) {
            return None;
        }
        let Surface::Doc(ed) = &tab.surface else {
            return None;
        };
        Some(DocPreview {
            tab: self.active,
            docs: self.tabs.len(),
            generation: edit_generation(),
            caret: ed.caret.clone(),
            anchor: ed.anchor.clone(),
            kind,
            editor: build_preview(ed, kind)?,
        })
    }

    /// Drop the live preview when `kind` holds for its kind.
    pub(crate) fn drop_doc_preview(&mut self, kind: impl Fn(PreviewKind) -> bool) {
        if self.doc_preview.as_ref().is_some_and(|p| kind(p.kind)) {
            self.doc_preview = None;
        }
    }

    /// The live preview to draw in place of the active document this
    /// frame, if any: still current ([`DocPreview::current`]), still
    /// allowed, and its control still on screen (the grid's menu open on
    /// that cell; the Table Styles gallery showing). Correctness rests on
    /// this check, not on every path that hides the control clearing it.
    pub(crate) fn shown_preview(&self, window: &Window) -> Option<&DocPreview> {
        let p = self.doc_preview.as_ref()?;
        let tab = self.tabs.get(self.active)?;
        let Surface::Doc(ed) = &tab.surface else {
            return None;
        };
        if !preview_allowed(tab, self.view_only_active(), self.draw_tool().is_some()) {
            return None;
        }
        let showing = match p.kind {
            PreviewKind::Grid { cols, rows } => {
                self.table_grid_open().is_ok() && self.table_grid_hover == Some((cols, rows))
            }
            PreviewKind::Style(_) => self.table_styles_showing(window),
        };
        p.current(
            (self.active, self.tabs.len()),
            ed,
            edit_generation(),
            showing,
        )
        .then_some(p)
    }

    /// Whether the Table Styles gallery is on screen: the Table Design tab
    /// shows (the caret in a body table, the ribbon body drawn), and its
    /// group is drawn in place, or collapsed (#1020) with its flyout open.
    /// It reads the ribbon fit the frame draws from (`ribbon_fit_now`).
    pub(crate) fn table_styles_showing(&self, window: &Window) -> bool {
        if self.backstage || self.ribbon_body_hidden() || !self.ribbon_is_model() {
            return false;
        }
        let tab = valid_ribbon_tab(
            self.ribbon_kind(),
            self.ribbon_tab,
            self.caret_in_table(),
            self.project_gantt_showing(),
            self.hf_active(),
        );
        if tab != RibbonTab::TableDesign {
            return false;
        }
        let now = self.ribbon_fit_now(window);
        let Some(i) = now.titles.iter().position(|t| *t == STYLES_GROUP) else {
            return false;
        };
        if now.fit.in_flyout(i) || now.fit.in_overflow(i) {
            self.ribbon_flyout.is_some_and(|f| {
                f.target == FlyoutTarget::Group(STYLES_GROUP) && self.flyout_still_fits(f, window)
            })
        } else {
            true
        }
    }

    /// The pointer over Table Styles tile `BUILTIN[i]` (`None`: off the
    /// gallery): the caret's table is previewed in that style (#764).
    pub(crate) fn table_style_hover(&mut self, at: Option<usize>, cx: &mut Context<Self>) {
        let now = self.doc_preview.as_ref().map(|p| p.kind);
        match at {
            Some(i) if now != Some(PreviewKind::Style(i)) => {
                self.set_doc_preview(PreviewKind::Style(i));
                cx.notify();
            }
            None if matches!(now, Some(PreviewKind::Style(_))) => {
                self.doc_preview = None;
                cx.notify();
            }
            _ => {}
        }
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
                "Draw",
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
            "draw-table",
            "eraser",
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
        assert!(table_checked(Some(&ed), true, None, TableAct::Style(7)));
        assert!(!table_checked(Some(&ed), true, None, TableAct::Style(0)));
        assert!(table_checked(
            Some(&ed),
            true,
            None,
            TableAct::Look(LookFlag::HeaderRow)
        ));
        table_apply(&mut ed, TableAct::Look(LookFlag::HeaderRow)).unwrap();
        assert!(!table_checked(
            Some(&ed),
            true,
            None,
            TableAct::Look(LookFlag::HeaderRow)
        ));
        table_apply(&mut ed, TableAct::Align(VAlign::Bottom, Align::Right)).unwrap();
        assert!(table_checked(
            Some(&ed),
            true,
            None,
            TableAct::Align(VAlign::Bottom, Align::Right)
        ));
        table_apply(&mut ed, TableAct::Shading(Some(0xFF0000))).unwrap();
        assert!(table_checked(
            Some(&ed),
            true,
            None,
            TableAct::Shading(Some(0xFF0000))
        ));
        assert!(table_checked(
            Some(&ed),
            false,
            None,
            TableAct::Borders(BorderCmd::All)
        ));
        table_apply(&mut ed, TableAct::Borders(BorderCmd::All)).unwrap();
        assert!(!table_checked(
            Some(&ed),
            false,
            None,
            TableAct::Borders(BorderCmd::All)
        ));
        assert!(table_checked(
            Some(&ed),
            true,
            None,
            TableAct::ViewGridlines
        ));
        assert!(!table_checked(
            Some(&ed),
            false,
            None,
            TableAct::ViewGridlines
        ));
        // The pens show checked while their mode is on, in a table or not.
        assert!(table_checked(
            None,
            false,
            Some(Tool::Pen),
            TableAct::DrawTable
        ));
        assert!(!table_checked(
            None,
            false,
            Some(Tool::Pen),
            TableAct::Eraser
        ));
        assert!(table_checked(
            None,
            false,
            Some(Tool::Eraser),
            TableAct::Eraser
        ));
        assert!(!table_checked(Some(&ed), false, None, TableAct::DrawTable));
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
                ("Draw Table".to_string(), true),
                ("Convert Text to Table...".to_string(), false),
                ("Excel Spreadsheet".to_string(), false),
                ("Quick Tables".to_string(), false),
            ]
        );
        assert_eq!(grid_header(None), "Insert Table");
        assert_eq!(grid_header(Some((4, 3))), "4x3 Table");
    }

    /// A grid preview (#764) is the table the click would insert, built on
    /// a copy: the real editor keeps its document and its history.
    #[test]
    fn a_grid_preview_inserts_on_a_copy_764() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        ed.insert_str("x");
        let before = ed.doc.clone();
        let p = build_preview(&ed, PreviewKind::Grid { cols: 3, rows: 2 }).unwrap();
        assert_eq!(count_tables(&p.doc.body), count_tables(&ed.doc.body) + 1);
        let t = p.table_at_caret().unwrap();
        let t = p.table(&t.table).unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 3);
        assert_eq!(ed.doc, before);
        assert!(ed.undo(), "the typing is still the newest step");
        assert!(!ed.undo());
        assert!(build_preview(&ed, PreviewKind::Grid { cols: 0, rows: 2 }).is_none());
        // No undo serial is issued: Repeat (F4) and the typing run read it.
        let serial = docxcore::editor::undo_serial_counter();
        build_preview(&ed, PreviewKind::Grid { cols: 3, rows: 2 }).unwrap();
        build_preview(&ed_in_table(), PreviewKind::Style(7)).unwrap();
        assert_eq!(docxcore::editor::undo_serial_counter(), serial);
        assert!(build_preview(&ed, PreviewKind::Grid { cols: 2, rows: 0 }).is_none());
    }

    #[test]
    fn a_style_preview_restyles_the_caret_table_on_a_copy_764() {
        let ed = ed_in_table();
        let before = ed.doc.clone();
        let p = build_preview(&ed, PreviewKind::Style(7)).unwrap();
        assert_eq!(p.table_style().as_deref(), Some(BUILTIN[7].id));
        assert_eq!(ed.doc, before);
        assert_ne!(ed.table_style().as_deref(), Some(BUILTIN[7].id));
        // Outside a table the command is refused, and so is the preview.
        let plain = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        assert!(build_preview(&plain, PreviewKind::Style(7)).is_none());
        assert!(build_preview(&ed, PreviewKind::Style(BUILTIN.len())).is_none());
    }

    /// A preview is dropped once the document it was built on changes: an
    /// edit (the generation), a caret or selection move, another tab, a
    /// closed tab, or its control gone from the screen (#764).
    #[test]
    fn a_preview_is_current_only_over_the_document_it_was_built_on_764() {
        let ed = ed_in_table();
        let p = DocPreview {
            tab: 1,
            docs: 2,
            generation: 5,
            caret: ed.caret.clone(),
            anchor: ed.anchor.clone(),
            kind: PreviewKind::Style(7),
            editor: build_preview(&ed, PreviewKind::Style(7)).unwrap(),
        };
        assert!(p.current((1, 2), &ed, 5, true));
        assert!(!p.current((1, 2), &ed, 6, true), "an edit since");
        assert!(!p.current((0, 2), &ed, 5, true), "another tab");
        assert!(!p.current((1, 1), &ed, 5, true), "a tab closed");
        assert!(!p.current((1, 2), &ed, 5, false), "not showing");
        let mut moved = ed.preview_copy();
        moved.caret = Caret::at(vec![0, 1, 1, 0], 0);
        assert!(!p.current((1, 2), &moved, 5, true), "the caret moved");
        let mut selected = ed.preview_copy();
        selected.anchor = Some(Caret::at(vec![0, 1, 1, 0], 0));
        assert!(!p.current((1, 2), &selected, 5, true), "a selection made");
    }

    #[test]
    fn a_preview_is_not_built_where_the_command_is_refused_764() {
        let mut t = blank_docx_tab("Preview");
        assert!(preview_allowed(&t, false, false));
        assert!(!preview_allowed(&t, true, false), "No Markup / Original");
        assert!(!preview_allowed(&t, false, true), "a table pen on");
        t.access.protected = true;
        assert!(!preview_allowed(&t, false, false), "Protected View");
        t.access.protected = false;
        t.access.marked_final = true;
        assert!(!preview_allowed(&t, false, false), "marked as final");
        t.access.marked_final = false;
        t.hf_edit = Some(HfEdit {
            editor: Editor::new(empty_doc()),
            part_name: "word/header1.xml".into(),
            is_header: true,
            section: 0,
            variant: docxcore::package::HeaderVariant::Default,
            show_text: true,
        });
        assert!(!preview_allowed(&t, false, false), "a header being edited");
    }

    #[test]
    fn count_tables_counts_nested_tables_764() {
        let mut outer = new_table(1, 1, 9000, AutoFit::Default);
        outer.rows[0].cells[0]
            .blocks
            .push(Block::Table(new_table(1, 1, 4000, AutoFit::Default)));
        let body = vec![Block::Table(outer), Block::Paragraph(Paragraph::default())];
        assert_eq!(count_tables(&body), 2);
    }
}
