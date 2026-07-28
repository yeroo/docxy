//! docxy — the doc-centric desktop suite (docs / sheets / mail in tabs), on GPUI.
//!
//! A thin GPUI view over `docxcore::editor::Editor` — the lossless engine the
//! terminal docxy uses. Custom title bar hosting the document tabs + window
//! controls, an Office-style ribbon (File backstage + Home/Styles/Insert/Review/
//! View tabs of titled command groups), and a rich editable document surface.
//! Theming (Auto/Light/Dark) follows gpui-component's theme; session hot-exit
//! persists open tabs/files + the theme choice to `<config>/docxy/session.json`.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use docxcore::comments::Comment;
use docxcore::editor::{Caret, Clip, Editor};
use docxcore::package::Package;
use docxcore::model::{Align, Block, BorderKind, Document, Inline, ParBorders, Paragraph, RunProps, Table, VertAlign};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, Sizable, Theme, ThemeMode, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    tooltip::Tooltip,
    v_flex,
};
use ribbonspec::{self as rs, Control};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// ---- assets: bundle our MIT Fluent icons + gpui-component's own assets -------

#[derive(RustEmbed)]
#[folder = "assets/icons"]
#[prefix = "icons/"]
struct Icons;

/// Serves our embedded Fluent icon SVGs (`icons/*.svg`), falling back to
/// gpui-component's bundled assets for everything else.
struct DocxyAssets;
impl AssetSource for DocxyAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(f) = Icons::get(path) {
            return Ok(Some(f.data));
        }
        gpui_component_assets::Assets.load(path)
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

fn icon_svg(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).text_color(color).flex_none()
}

/// A small Quick-Access-Toolbar icon button (Undo/Redo in the title bar).
fn qat_btn(id: &'static str, icon: &'static str, tip: &'static str, pal: Pal, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|d| d.bg(pal.hover))
        .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
        .child(icon_svg(icon, 14., pal.fg))
        .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
        .on_click(on_click)
}

// ---- session model ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Kind {
    Docx,
    Xlsx,
    Look,
}

impl Kind {
    fn glyph(self) -> &'static str {
        match self {
            Kind::Docx => "\u{1F4C4}",
            Kind::Xlsx => "\u{1F4CA}",
            Kind::Look => "\u{2709}",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
enum ThemePref {
    #[default]
    Auto,
    Light,
    Dark,
}

impl ThemePref {
    fn label(self) -> &'static str {
        match self {
            ThemePref::Auto => "\u{25D1} Auto",
            ThemePref::Light => "\u{2600} Light",
            ThemePref::Dark => "\u{263D} Dark",
        }
    }
    fn next(self) -> Self {
        match self {
            ThemePref::Auto => ThemePref::Light,
            ThemePref::Light => ThemePref::Dark,
            ThemePref::Dark => ThemePref::Auto,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PersistTab {
    kind: Kind,
    title: String,
    path: Option<String>,
    #[serde(default)]
    dirty: bool,
    /// Hot-exit sidecar `.docx` holding this tab's current (possibly unsaved)
    /// content. Restored in preference to `path` so edits survive a restart.
    #[serde(default)]
    hot: Option<String>,
    #[serde(default)]
    markdown: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Session {
    tabs: Vec<PersistTab>,
    active: usize,
    #[serde(default)]
    theme: ThemePref,
    #[serde(default)]
    ask_on_close: bool,
}

fn session_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("docxy").join("session.json")
}

// ---- ribbon tabs -----------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum RibbonTab {
    Home,
    Insert,
    Review,
    View,
    /// Contextual Table Tools tab — only reachable while the caret is in a table.
    Table,
}

// ---- runtime ---------------------------------------------------------------

enum Surface {
    Doc(Editor),
    Sheet(SheetView),
    Placeholder,
}

/// A spreadsheet tab's live state: the loaded workbook package, which sheet is
/// active, the selected cell, and the grid's scroll handle.
struct SheetView {
    pkg: gridcore::xlsx::SheetPackage,
    active: usize,
    /// Selected cell — the range's active corner (typing lands here), 0-based
    /// (row, col).
    sel: (u32, u32),
    /// The other corner of the selection range; equals `sel` when a single cell
    /// is selected. `range()` normalizes the two into (r0,c0,r1,c1).
    anchor: (u32, u32),
    /// When `Some`, the selected cell is being edited and this is the in-progress
    /// input buffer (a leading `=` marks a formula).
    editing: Option<String>,
    /// Caret position within `editing`, as a char index (0..=len). Only
    /// meaningful while `editing` is `Some`.
    edit_caret: usize,
    /// The recalc engine, indexed over the workbook's formulas, so an edit
    /// re-evaluates only the affected cells.
    engine: gridcore::engine::Engine,
    /// Undo / redo stacks of workbook snapshots (with the selection at the time).
    undo: Vec<SheetSnapshot>,
    redo: Vec<SheetSnapshot>,
    /// An in-progress column-resize drag (the header border being dragged).
    col_drag: Option<ColDrag>,
    /// Live PivotTable definitions authored in the UI, one per output sheet, so
    /// the field panel can re-place fields and recompute.
    pivot_views: Vec<PivotDef>,
    /// Charts authored in the UI, rendered as floating cards over their sheet.
    charts: Vec<ChartView>,
    /// Vertical scroll + item state of the virtualized row list. A gpui `list`
    /// (not `uniform_list`) so rows can have individual heights (wrap text /
    /// explicit row height). Its item count is re-synced to the visible-row
    /// count each render; visible rows are re-measured every layout, so height
    /// changes take effect without an explicit reset.
    vlist: ListState,
    /// Leftmost visible column (horizontal scroll offset). Columns virtualize by
    /// offset — rendered `col0..=cend` — so columns past the viewport are
    /// reachable (raw gpui can't wrap the virtualized row list in an h-scroller).
    col0: u32,
    /// The selection the horizontal scroll last followed. reconcile only re-centres
    /// on the selection when it differs from this, so manual scrolling (arrows /
    /// wheel / thumb) can move the view away without being snapped back.
    follow_sel: (u32, u32),
}

/// A UI-authored chart: which sheet it floats over, its cell anchor (for save),
/// and its (cached) data.
#[derive(Clone)]
struct ChartView {
    sheet: usize,
    from: (u32, u32),
    to: (u32, u32),
    data: gridcore::sheet::ChartData,
}

/// A UI-authored PivotTable: its source and, per source field, the role the user
/// assigned (0 none, 1 rows, 2 columns, 3 values). Recomputed into `out_sheet`.
#[derive(Clone)]
struct PivotDef {
    src_sheet: usize,
    src_range: (u32, u32, u32, u32),
    out_sheet: usize,
    names: Vec<String>,
    role: Vec<u8>,
    /// Per field, the aggregation index into [`PIVOT_AGGS`] (only meaningful for
    /// Values fields; 0 = Sum). Parallel to `names`/`role`.
    agg: Vec<u8>,
}

/// Aggregations a pivot value field can cycle through, with short badge labels.
/// The index is what `PivotDef.agg` stores (0 = Sum, the default).
const PIVOT_AGGS: [(gridcore::frame::Agg, &str); 6] = [
    (gridcore::frame::Agg::Sum, "Sum"),
    (gridcore::frame::Agg::Count, "Count"),
    (gridcore::frame::Agg::Average, "Avg"),
    (gridcore::frame::Agg::Max, "Max"),
    (gridcore::frame::Agg::Min, "Min"),
    (gridcore::frame::Agg::Product, "Product"),
];

/// A point-in-time snapshot of a spreadsheet for undo/redo.
struct SheetSnapshot {
    wb: gridcore::sheet::Workbook,
    active: usize,
    sel: (u32, u32),
    anchor: (u32, u32),
}

/// An in-progress column-resize drag: which column, and the mouse-x + width it
/// started at (character units).
#[derive(Clone, Copy)]
struct ColDrag {
    col: u32,
    start_x: f32,
    start_w: f64,
}

/// An in-progress auto-fill drag from the selection's fill handle: the source
/// range and the cell the handle has reached.
#[derive(Clone, Copy)]
struct FillDrag {
    src: (u32, u32, u32, u32),
    to: (u32, u32),
}

/// The dominant-axis fill box for `src` dragged to `to`: extend rows (down) or
/// columns (right), whichever the handle was pulled furthest along. Returns the
/// full box (source + filled cells), 0-based inclusive.
fn fill_box(src: (u32, u32, u32, u32), to: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sr0, sc0, sr1, sc1) = src;
    let (tr, tc) = to;
    let dr = tr.saturating_sub(sr1);
    let dc = tc.saturating_sub(sc1);
    if dr >= dc {
        (sr0, sc0, sr1.max(tr), sc1)
    } else {
        (sr0, sc0, sr1, sc1.max(tc))
    }
}

/// The grid clipboard: a rectangular block of cells copied from a sheet.
#[derive(Clone)]
struct GridClip {
    cells: Vec<Vec<gridcore::sheet::Cell>>,
}

/// A spreadsheet ribbon command (the sheet counterpart to the document `Act`).
/// Mirrors Excel's Home tab; `Todo` is an inert placeholder for commands whose
/// engine support isn't wired yet (they render but do nothing), like Word's
/// dialog-launcher stubs.
#[derive(Clone, Copy)]
enum SheetAct {
    Cut,
    Copy,
    Paste,
    Bold,
    Italic,
    AlignL,
    AlignC,
    AlignR,
    WrapText,
    RowHeight,
    GrowFont,
    ShrinkFont,
    Percent,
    Currency,
    Comma,
    InsertPivot,
    InsertChart(&'static str),
    FillColor,
    FontColor,
    ToggleBorder,
    FreezePanes,
    NewComment,
    DeleteComment,
    PrevComment,
    NextComment,
    InsertRow,
    DeleteRow,
    InsertCol,
    DeleteCol,
    SortAsc,
    SortDesc,
    CustomSort,
    AutoSum,
    FormatCells,
    Merge,
    CondFormat,
    DataValidation,
    Filter,
    RemoveDuplicates,
    TextToColumns,
    FormatAsTable,
    ProtectSheet,
    Subtotal,
    Outline,
    Todo,
}

/// Parse a conditional-format comparison into an Excel cellIs operator plus one
/// or two operands: ">500", "<=100", "=42" (default greaterThan), or a between
/// range "100..500".
fn parse_cf_input(s: &str) -> Option<(&'static str, String, Option<String>)> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once("..") {
        let (a, b) = (a.trim(), b.trim());
        if !a.is_empty() && !b.is_empty() {
            return Some(("between", a.to_string(), Some(b.to_string())));
        }
    }
    let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
        ("greaterThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<=") {
        ("lessThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<>") {
        ("notEqual", r)
    } else if let Some(r) = s.strip_prefix('>') {
        ("greaterThan", r)
    } else if let Some(r) = s.strip_prefix('<') {
        ("lessThan", r)
    } else if let Some(r) = s.strip_prefix('=') {
        ("equal", r)
    } else {
        ("greaterThan", s)
    };
    let rest = rest.trim();
    if rest.is_empty() { None } else { Some((op, rest.to_string(), None)) }
}

/// Excel's "Light Red Fill with Dark Red Text" conditional-format preset.
fn cf_preset_dxf() -> gridcore::sheet::Dxf {
    gridcore::sheet::Dxf { fill: Some((0xFF, 0xC7, 0xCE)), color: Some((0x9C, 0x00, 0x06)), bold: None, italic: None }
}

/// Which colour a sheet swatch picker is choosing.
#[derive(Clone, Copy, PartialEq)]
enum SheetPick {
    Fill,
    Font,
}

/// A whole-row / whole-column structural edit at the selection.
#[derive(Clone, Copy)]
enum StructOp {
    InsertRow,
    DeleteRow,
    InsertCol,
    DeleteCol,
}

impl SheetView {
    fn sheet(&self) -> &gridcore::sheet::Sheet {
        &self.pkg.workbook.sheets[self.active.min(self.pkg.workbook.sheets.len().saturating_sub(1))]
    }
    /// The text to seed the editor with when re-editing a cell: `=formula` for a
    /// formula, the raw literal otherwise (unformatted, so it round-trips).
    fn edit_string(&self, row: u32, col: u32) -> String {
        use gridcore::sheet::CellValue;
        let sh = self.sheet();
        match sh.cell(row, col) {
            Some(c) if c.formula.is_some() => format!("={}", c.formula.as_deref().unwrap_or("")),
            Some(c) => match &c.value {
                CellValue::Number(n) => n.to_string(),
                CellValue::Text(s) => s.clone(),
                CellValue::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
                CellValue::Error(e) => e.clone(),
                CellValue::Empty => String::new(),
            },
            None => String::new(),
        }
    }

    // ---- in-cell edit caret (char-indexed into `editing`) ----
    /// Number of chars in the edit buffer.
    fn edit_len(&self) -> usize {
        self.editing.as_deref().map(|s| s.chars().count()).unwrap_or(0)
    }
    /// Put the caret at the end of the current buffer (called when editing starts).
    fn edit_caret_to_end(&mut self) {
        self.edit_caret = self.edit_len();
    }
    /// Insert text at the caret, advancing it.
    fn edit_insert(&mut self, s: &str) {
        if let Some(buf) = self.editing.as_mut() {
            buf_insert(buf, &mut self.edit_caret, s);
        }
    }
    /// Delete the char before the caret (Backspace).
    fn edit_backspace(&mut self) {
        if let Some(buf) = self.editing.as_mut() {
            buf_backspace(buf, &mut self.edit_caret);
        }
    }
    /// Delete the char at the caret (Delete).
    fn edit_delete(&mut self) {
        let caret = self.edit_caret;
        if let Some(buf) = self.editing.as_mut() {
            buf_delete(buf, caret);
        }
    }
    /// Move the caret by `delta` chars, clamped to the buffer.
    fn edit_move(&mut self, delta: i32) {
        let n = self.edit_len() as i32;
        self.edit_caret = (self.edit_caret as i32 + delta).clamp(0, n) as usize;
    }

    /// The display text for a cell (number-formatted via its style).
    fn cell_text(&self, row: u32, col: u32) -> String {
        let sh = self.sheet();
        match sh.cell(row, col) {
            Some(c) if !c.is_blank() => {
                let xf = self.pkg.workbook.styles.xf(c.style);
                gridcore::sheet::format_with(&xf, &c.value, self.pkg.workbook.date1904)
            }
            _ => String::new(),
        }
    }
    /// Restore this view from an undo/redo snapshot, rebuilding the recalc engine.
    fn restore(&mut self, snap: SheetSnapshot) {
        self.pkg.workbook = snap.wb;
        self.engine = gridcore::engine::Engine::new(&self.pkg.workbook);
        self.active = snap.active.min(self.pkg.workbook.sheets.len().saturating_sub(1));
        self.sel = snap.sel;
        self.anchor = snap.anchor;
        self.editing = None;
    }
    /// The selection rectangle as (r0, c0, r1, c1), top-left to bottom-right.
    fn range(&self) -> (u32, u32, u32, u32) {
        let (ar, ac) = self.sel;
        let (br, bc) = self.anchor;
        (ar.min(br), ac.min(bc), ar.max(br), ac.max(bc))
    }
    /// Whether more than one cell is selected.
    fn has_range(&self) -> bool {
        self.sel != self.anchor
    }
    /// The index of `row` within the virtualized list (non-hidden scrollable
    /// rows, past the frozen ones) — for `ListState::scroll_to_reveal_item`.
    fn row_list_index(&self, row: u32) -> usize {
        let sh = self.sheet();
        let vis_before = (0..row).filter(|&r| !sh.row_hidden(r)).count();
        let fr = (sh.freeze.0 as usize).min(30);
        vis_before.saturating_sub(fr)
    }
    /// The used extent (max row, max col) over the active sheet's cells + merges.
    fn extent(&self) -> (u32, u32) {
        let sh = self.sheet();
        let mut r = 0u32;
        let mut c = 0u32;
        for &(row, col) in sh.cells.keys() {
            r = r.max(row);
            c = c.max(col);
        }
        for &(r0, c0, r1, c1) in &sh.merges {
            r = r.max(r0).max(r1);
            c = c.max(c0).max(c1);
        }
        (r, c)
    }
}

struct DocTab {
    kind: Kind,
    title: SharedString,
    path: Option<PathBuf>,
    surface: Surface,
    dirty: bool,
    status: SharedString,
    /// Review comments anchored in this document (markers live in the body; the
    /// text/author is stored here and written to comments.xml on save).
    comments: Vec<Comment>,
    /// The original package this doc was loaded from, kept so a save re-serializes
    /// only document.xml back into it and preserves every other part (footnotes,
    /// headers/footers, images, themes, …). `None` for a new empty document.
    pkg: Option<Package>,
    /// Footnotes / endnotes parsed from the package (display-only side panel).
    notes: Vec<docxcore::notes::Note>,
    /// This tab is Markdown-backed (opened from a `.md`); Save writes Markdown.
    markdown: bool,
    /// When `Some`, the header or footer is being edited (Word's header/footer
    /// edit mode): keystrokes/clicks route to this editor instead of the body,
    /// and its blocks are serialized back into the package part on exit/save.
    hf_edit: Option<HfEdit>,
}

/// Live header/footer edit session: an editor over the parsed header/footer
/// blocks, plus the package part they came from so edits can be written back.
struct HfEdit {
    editor: Editor,
    part_name: String,
    is_header: bool,
    /// Which reference type is being edited: `"default"`, `"first"`, or `"even"`.
    variant: &'static str,
}

struct Docxy {
    tabs: Vec<DocTab>,
    active: usize,
    focus: FocusHandle,
    focused: bool,
    ribbon_tab: RibbonTab,
    ribbon_min: bool,
    backstage: bool,
    bs_new: bool,
    clip: Option<Clip>,
    theme_pref: ThemePref,
    /// When set, closing the window with unsaved tabs shows a confirm dialog.
    /// Off by default: work is hot-persisted and restored regardless, so closing
    /// is normally silent.
    ask_on_close: bool,
    applied: Option<ThemeMode>,
    // Find & replace bar (Ctrl+F). Self-managed text fields (no gpui-component
    // InputState entity) — keystrokes route here while `find_open`.
    find_open: bool,
    find_query: String,
    replace_text: String,
    find_field: FindField,
    find_case: bool,
    // Font-colour / highlight swatch picker (None = closed).
    picker: Option<PickKind>,
    // Scroll handle for the document body, so the caret can be kept in view.
    doc_scroll: ScrollHandle,
    // New-comment entry bar (Review ▸ New comment); routes keys while open.
    comment_open: bool,
    comment_text: String,
    // Show formatting marks (¶, tab arrows) — View ▸ Show/Hide.
    show_marks: bool,
    // Comments review side panel (View / Review ▸ Comments).
    show_comments: bool,
    // Navigation (heading outline) side panel — View ▸ Navigation.
    show_nav: bool,
    // Footnotes/endnotes side panel — Review ▸ Notes.
    show_notes: bool,
    // Print Layout: render the document on a page sheet with margins (View).
    page_view: bool,
    // Horizontal ruler with margin/indent/tab markers (View ▸ Ruler).
    show_ruler: bool,
    // In-progress drag of a ruler marker.
    ruler_drag: Option<RulerDrag>,
    // The tab-stop type placed when clicking the ruler (cycled via the corner box).
    ruler_tab: docxcore::model::TabAlign,
    // The ruler content area's left-margin screen x, written by the ruler canvas
    // each paint and read by click handlers to map a click to a tab position.
    ruler_x0: std::rc::Rc<std::cell::Cell<f32>>,
    // A text drag-selection is in progress (mouse down in the doc, not yet up).
    selecting: bool,
    // A spreadsheet drag-select is in progress (left button held over cells).
    // The first dragged-over cell plants the anchor; later ones extend the range.
    sheet_dragging: bool,
    // An in-progress auto-fill drag from the selection's fill handle.
    sheet_fill: Option<FillDrag>,
    // KeyTips (Alt access keys): Off, tab letters, or the active tab's commands.
    keytips: KeyTip,
    // Right-click context menu position (window coords), if open.
    context_menu: Option<Point<Pixels>>,
    // Floating mini formatting toolbar shown after a drag-selection (window coords).
    mini_bar: Option<Point<Pixels>>,
    // Document zoom factor (1.0 = 100%), controlled from the status bar.
    zoom: f32,
    // While a ruler marker is being dragged, the screen x of a vertical guide
    // line drawn down the page (Word's drag guide). None when not dragging.
    ruler_guide: Option<f32>,
    // The spreadsheet clipboard: a rectangular block of cells from the last grid
    // copy/cut, pasted at the selection on Ctrl+V.
    grid_clip: Option<GridClip>,
    // Open sheet colour-swatch picker (fill or font), None = closed.
    sheet_pick: Option<SheetPick>,
    // Inline sheet-tab rename in progress: (tab index, edit buffer). None = idle.
    sheet_rename: Option<(usize, String)>,
    // Grid width (px) captured each render, so the horizontal scroll track can map
    // a click x back to a column fraction.
    sheet_grid_w: f32,
    // In-progress cell-comment entry for the selected cell (the input buffer);
    // None = the comment bar is closed.
    sheet_comment_edit: Option<String>,
    // Whether the data-validation list dropdown is open on the selected cell.
    sheet_dv_open: bool,
    // Whether the Number-group format picker strip is open.
    sheet_numfmt_open: bool,
    // Whether the consolidated Format Cells panel is open.
    sheet_fmt_open: bool,
    // In-progress conditional-formatting rule entry (the value buffer, e.g. ">500").
    sheet_cf_edit: Option<String>,
    // In-progress data-validation list entry (comma-separated allowed values).
    sheet_dv_edit: Option<String>,
    // In-progress AutoFilter criteria entry for the selected column.
    sheet_filter_edit: Option<String>,
    // In-progress Text-to-Columns delimiter entry.
    sheet_ttc_edit: Option<String>,
    // In-progress multi-level sort spec entry ("B asc, C desc").
    sheet_sort_edit: Option<String>,
    // In-progress row-height entry (points, or "auto").
    sheet_rowh_edit: Option<String>,
}

/// Parse a delimiter word/char: "tab" -> \t, "space" -> ' ', else the first
/// character (default comma).
fn parse_delim(s: &str) -> char {
    match s.trim().to_lowercase().as_str() {
        "" | "comma" => ',',
        "tab" => '\t',
        "space" => ' ',
        "semicolon" => ';',
        "pipe" => '|',
        other => other.chars().next().unwrap_or(','),
    }
}

/// Common number formats offered by the Number-group dropdown: (label, code).
/// The `General` code is empty (clears xf.code back to the default).
const NUM_FORMATS: [(&str, &str); 9] = [
    ("General", ""),
    ("Number", "#,##0.00"),
    ("Currency", "$#,##0.00"),
    ("Accounting", "_($* #,##0.00_);_($* (#,##0.00);_($* \"-\"??_)"),
    ("Percentage", "0.00%"),
    ("Scientific", "0.00E+00"),
    ("Short Date", "yyyy-mm-dd"),
    ("Time", "h:mm:ss"),
    ("Text", "@"),
];

// gpui reserves Tab / Shift-Tab for focus traversal and never delivers them to
// on_key_down, so a tab must be inserted through a bound action instead.
actions!(docxy, [InsertTabAction, OutdentAction]);

#[derive(Clone, Copy, PartialEq)]
enum KeyTip {
    Off,
    Tabs,
    Commands,
}

#[derive(Clone, Copy, PartialEq)]
enum RulerHandle {
    FirstLine,
    Left,
    Right,
    MarginLeft,
    MarginRight,
}

#[derive(Clone, Copy)]
struct RulerDrag {
    handle: RulerHandle,
    start_x: f32,
    start_indent: i32,
    start_first: i32,
    start_right: i32,
    start_ml: i32,
    start_mr: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum FindField {
    Query,
    Replace,
}

#[derive(Clone, Copy, PartialEq)]
enum PickKind {
    Color,
    Highlight,
    FontName,
    FontSize,
    Field,
    Table,
    Symbol,
    LineSpacing,
    Equation,
}

/// Table sizes offered by the Insert ▸ Table picker: (label, rows, cols).
const TABLE_PRESETS: &[(&str, usize, usize)] = &[("2×2", 2, 2), ("3×2", 3, 2), ("3×3", 3, 3), ("4×3", 4, 3), ("5×3", 5, 3), ("5×5", 5, 5)];

/// The characters offered by the Insert ▸ Symbol picker — Word's common set:
/// typographic punctuation, currency, arrows, and maths.
const SYMBOLS: &[&str] = &[
    "\u{2014}", "\u{2013}", "\u{2011}", "\u{2026}", "\u{2022}", "\u{00B7}", "\u{00A9}", "\u{00AE}",
    "\u{2122}", "\u{00B0}", "\u{00B1}", "\u{00D7}", "\u{00F7}", "\u{2260}", "\u{2248}", "\u{2264}",
    "\u{2265}", "\u{221E}", "\u{00A7}", "\u{00B6}", "\u{20AC}", "\u{00A3}", "\u{00A5}", "\u{00A2}",
    // Typographic quotes: guillemets, low/high quotes, angle quotes.
    "\u{00AB}", "\u{00BB}", "\u{201E}", "\u{201C}", "\u{201D}", "\u{201A}", "\u{2018}", "\u{2019}",
    "\u{2039}", "\u{203A}",
    "\u{2190}", "\u{2192}", "\u{2191}", "\u{2193}", "\u{03B1}", "\u{03B2}", "\u{03C0}", "\u{03BC}",
    "\u{03A9}", "\u{2211}", "\u{221A}", "\u{2212}", "\u{2605}",
];

/// Common equation templates offered by Insert ▸ Equation: (label, LaTeX). The
/// engine turns the LaTeX into real Word OMML on insert.
const EQUATIONS: &[(&str, &str)] = &[
    ("x\u{00B2}", "x^2"),
    ("a\u{207F}", "a^{n}"),
    ("a/b", "\\frac{a}{b}"),
    ("\u{221A}x", "\\sqrt{x}"),
    ("\u{03A3}", "\\sum_{i=1}^{n} i"),
    ("\u{222B}", "\\int_{a}^{b} f(x)\\,dx"),
    ("lim", "\\lim_{x \\to \\infty} f(x)"),
    ("\u{03B1}\u{03B2}\u{03B3}", "\\alpha\\beta\\gamma"),
    ("a\u{00B2}+b\u{00B2}=c\u{00B2}", "a^2 + b^2 = c^2"),
    ("Quadratic", "x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}"),
];

/// The fields offered by the Insert ▸ Field picker: (label, instruction, fallback).
const FIELDS: &[(&str, &str, &str)] = &[
    ("Date", "DATE \\@ \"M/d/yyyy\"", ""),
    ("Time", "TIME \\@ \"h:mm AM/PM\"", ""),
    ("Page", "PAGE", "1"),
    ("Pages", "NUMPAGES", "1"),
    ("Author", "AUTHOR", "docxy"),
    ("File name", "FILENAME", ""),
];

/// Everything a rendered word/atom needs to turn a click into a caret move: the
/// view handle to update and the paragraph's block path. Cheap to copy (borrows).
#[derive(Clone, Copy)]
struct Click<'a> {
    ent: &'a Entity<Docxy>,
    path: &'a [usize],
}

/// Shared state for the recursive document renderer, so caret/selection/clicks
/// resolve correctly at any nesting depth (top-level blocks and table cells).
#[derive(Clone, Copy)]
struct RenderCtx<'a> {
    caret_path: &'a [usize],
    caret_off: usize,
    spans: &'a [(Vec<usize>, usize, usize)],
    ent: &'a Entity<Docxy>,
    pal: Pal,
    marks: bool,
    zoom: f32,
    /// Whether this surface currently has the caret. When false (e.g. the body
    /// while a header/footer is being edited) no caret is drawn and clicks are
    /// inert, so the inactive surface reads as dimmed and untouchable.
    active: bool,
    /// Text measurer for tab-stop positioning.
    meas: &'a Measurer,
    /// Content width (px) when rendering header/footer paragraphs, enabling the
    /// implicit centre/right tab stops. `None` for body paragraphs.
    hf_width: Option<f32>,
}

/// Colours the document renderer needs, pulled from the active theme.
#[derive(Clone, Copy)]
struct Pal {
    fg: Hsla,
    dim: Hsla,
    border: Hsla,
    panel: Hsla,
    hover: Hsla,
    sel: Hsla,
}

const BRAND: u32 = 0x2AA79B; // teal wordmark/accent (reads on light + dark)
const LINK: u32 = 0x2f6fdb;
const FILE_FG: u32 = 0xffffff;

fn empty_doc() -> Document {
    docxcore::markdown::from_markdown("# Untitled\n\n")
}

/// A loaded document with everything a tab needs to hold and re-save it losslessly.
struct Loaded {
    doc: Document,
    comments: Vec<Comment>,
    notes: Vec<docxcore::notes::Note>,
    pkg: Option<Package>,
    markdown: bool,
    status: SharedString,
}

impl Loaded {
    fn empty(status: impl Into<SharedString>) -> Self {
        Loaded { doc: empty_doc(), comments: vec![], notes: vec![], pkg: None, markdown: false, status: status.into() }
    }
    fn into_tab(self, kind: Kind, title: SharedString, path: Option<PathBuf>, dirty: bool) -> DocTab {
        DocTab { kind, title, path, surface: Surface::Doc(Editor::new(self.doc)), dirty, status: self.status, comments: self.comments, pkg: self.pkg, notes: self.notes, markdown: self.markdown, hf_edit: None }
    }
}

fn is_markdown_path(path: &std::path::Path) -> bool {
    let l = path.to_string_lossy().to_lowercase();
    l.ends_with(".md") || l.ends_with(".markdown") || l.ends_with(".mdown")
}

/// Load a `.docx` from bytes, keeping the whole package so save stays lossless.
fn load_bytes(bytes: &[u8]) -> Loaded {
    match docxcore::package::load_package(bytes) {
        Ok(pkg) => Loaded {
            doc: pkg.document.clone(),
            comments: docxcore::comments::parse_comments(&pkg),
            notes: docxcore::notes::parse_notes(&pkg),
            pkg: Some(pkg),
            markdown: false,
            status: "loaded".into(),
        },
        Err(e) => Loaded::empty(format!("load error: {e:?}")),
    }
}

fn doc_from_path(path: &PathBuf) -> Loaded {
    match std::fs::read(path) {
        Ok(bytes) if is_markdown_path(path) => Loaded {
            doc: docxcore::markdown::from_markdown(&String::from_utf8_lossy(&bytes)),
            comments: vec![],
            notes: vec![],
            pkg: None,
            markdown: true,
            status: "loaded (markdown)".into(),
        },
        Ok(bytes) => load_bytes(&bytes),
        Err(e) => Loaded::empty(format!("read error: {e}")),
    }
}

fn sample_doc() -> Loaded {
    load_bytes(include_bytes!("../../../assets/sample.docx"))
}

/// Load a `.xlsx` into a spreadsheet surface (or a placeholder + error status).
/// Serialize a live spreadsheet view to `.xlsx` bytes, folding in any
/// UI-authored charts via a throwaway package clone (so the live package isn't
/// mutated / charts aren't re-added on each call). Shared by Save and hot-exit.
fn sheet_bytes(v: &SheetView) -> Vec<u8> {
    if v.charts.is_empty() {
        gridcore::xlsx::save_xlsx(&v.pkg)
    } else {
        let mut pkg = v.pkg.clone();
        for cv in &v.charts {
            pkg.add_chart(cv.sheet, cv.from, cv.to, &cv.data);
        }
        gridcore::xlsx::save_xlsx(&pkg)
    }
}

/// Build a tab by loading `path` from disk — an .xlsx spreadsheet or a
/// Word/Markdown document, dispatched on the extension. Shared by the Open
/// dialog and command-line file arguments.
fn tab_from_path(path: &PathBuf) -> DocTab {
    let title: SharedString = file_name(path).into();
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xlsx")) {
        let (surface, status) = sheet_from_path(path);
        DocTab { kind: Kind::Xlsx, title, path: Some(path.clone()), surface, dirty: false, status, comments: vec![], pkg: None, notes: vec![], markdown: false, hf_edit: None }
    } else {
        doc_from_path(path).into_tab(Kind::Docx, title, Some(path.clone()), false)
    }
}

/// Char index → byte offset in `s` (clamped to the string length).
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

// Pure text-buffer + caret edits (char-indexed). Kept as free functions so they
// are unit-testable without constructing a gpui-heavy SheetView.
fn buf_insert(text: &mut String, caret: &mut usize, s: &str) {
    let at = char_to_byte(text, *caret);
    text.insert_str(at, s);
    *caret += s.chars().count();
}
fn buf_backspace(text: &mut String, caret: &mut usize) {
    if *caret == 0 {
        return;
    }
    let start = char_to_byte(text, *caret - 1);
    let end = char_to_byte(text, *caret);
    text.replace_range(start..end, "");
    *caret -= 1;
}
fn buf_delete(text: &mut String, caret: usize) {
    if caret >= text.chars().count() {
        return;
    }
    let start = char_to_byte(text, caret);
    let end = char_to_byte(text, caret + 1);
    text.replace_range(start..end, "");
}

/// Render an in-progress edit buffer with a blinking-style caret bar at `caret`
/// (a char index): the text before the caret, the caret, then the text after.
/// Shared by the in-cell editor and the formula bar.
fn edit_caret_row(text: &str, caret: usize, color: Hsla, caret_color: Hsla) -> AnyElement {
    let chars: Vec<char> = text.chars().collect();
    let c = caret.min(chars.len());
    let before: String = chars[..c].iter().collect();
    let after: String = chars[c..].iter().collect();
    h_flex()
        .items_center()
        .child(div().text_size(px(12.)).text_color(color).child(SharedString::from(before)))
        .child(div().w(px(1.5)).h(px(13.)).bg(caret_color).flex_none())
        .child(div().text_size(px(12.)).text_color(color).child(SharedString::from(after)))
        .into_any_element()
}

/// One click-to-caret text segment for the formula bar: a StyledText whose
/// TextLayout maps the click position to a char index (`base_off` is the char
/// offset of this segment within the whole buffer). Used for the before/after
/// halves around the caret so clicking places the caret under the pointer.
fn fx_segment(s: String, base_off: usize, ent: Entity<Docxy>) -> AnyElement {
    let styled = StyledText::new(SharedString::from(s.clone()));
    let layout = styled.layout().clone();
    div()
        .child(styled)
        .text_size(px(12.))
        .text_color(hsla_u(0x1a1a1a))
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
            let byte = layout.index_for_position(ev.position).unwrap_or_else(|e| e).min(s.len());
            let idx = base_off + s[..byte].chars().count();
            ent.update(cx, |this, cx| {
                if let Some(v) = this.active_sheet_mut() {
                    v.edit_caret = idx;
                }
                cx.notify();
            });
        })
        .into_any_element()
}

/// The formula bar's editing content: the text split before/after the caret
/// (each a click-to-caret segment) with the caret bar between them.
fn fx_edit_row(text: &str, caret: usize, ent: &Entity<Docxy>) -> AnyElement {
    let chars: Vec<char> = text.chars().collect();
    let cc = caret.min(chars.len());
    let before: String = chars[..cc].iter().collect();
    let after: String = chars[cc..].iter().collect();
    h_flex()
        .flex_1()
        .h_full()
        .items_center()
        .child(fx_segment(before, 0, ent.clone()))
        .child(div().w(px(1.5)).h(px(13.)).bg(hsla_u(BRAND)).flex_none())
        .child(fx_segment(after, cc, ent.clone()))
        .into_any_element()
}

/// A fresh, empty single-sheet workbook surface — the "New spreadsheet" path
/// (a real editable grid, not a placeholder).
fn new_sheet_surface() -> Surface {
    let pkg = gridcore::xlsx::new_xlsx();
    let engine = gridcore::engine::Engine::new(&pkg.workbook);
    Surface::Sheet(SheetView {
        pkg,
        active: 0,
        sel: (0, 0),
        anchor: (0, 0),
        editing: None,
        edit_caret: 0,
        engine,
        undo: vec![],
        redo: vec![],
        col_drag: None,
        pivot_views: vec![],
        charts: vec![],
        vlist: ListState::new(0, ListAlignment::Top, px(400.)),
        col0: 0,
        follow_sel: (0, 0),
    })
}

fn sheet_from_path(path: &PathBuf) -> (Surface, SharedString) {
    match std::fs::read(path) {
        Ok(bytes) => match gridcore::xlsx::load_xlsx(&bytes) {
            Ok(pkg) => {
                let n = pkg.workbook.sheets.len();
                let engine = gridcore::engine::Engine::new(&pkg.workbook);
                let view = SheetView { pkg, active: 0, sel: (0, 0), anchor: (0, 0), editing: None, edit_caret: 0, engine, undo: vec![], redo: vec![], col_drag: None, pivot_views: vec![], charts: vec![], vlist: ListState::new(0, ListAlignment::Top, px(400.)), col0: 0, follow_sel: (0, 0) };
                (Surface::Sheet(view), format!("loaded — {n} sheet{}", if n == 1 { "" } else { "s" }).into())
            }
            Err(e) => (Surface::Placeholder, format!("xlsx load error: {e:?}").into()),
        },
        Err(e) => (Surface::Placeholder, format!("read error: {e}").into()),
    }
}

fn build_surface(kind: Kind, path: Option<&PathBuf>) -> (Surface, Vec<Comment>, Vec<docxcore::notes::Note>, Option<Package>, SharedString) {
    match kind {
        Kind::Docx => match path {
            Some(p) => {
                let l = doc_from_path(p);
                (Surface::Doc(Editor::new(l.doc)), l.comments, l.notes, l.pkg, l.status)
            }
            None => (Surface::Doc(Editor::new(empty_doc())), vec![], vec![], None, "untitled".into()),
        },
        Kind::Xlsx => match path {
            Some(p) => {
                let (surface, status) = sheet_from_path(p);
                (surface, vec![], vec![], None, status)
            }
            None => (new_sheet_surface(), vec![], vec![], None, "new spreadsheet".into()),
        },
        _ => (Surface::Placeholder, vec![], vec![], None, "".into()),
    }
}

fn file_name(path: &PathBuf) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled.docx".into())
}

/// Directory holding the hot-exit sidecars — one `.docx` per open Doc tab, kept in
/// sync on each persist so unsaved edits survive a restart.
fn hot_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("docxy").join("hot")
}

/// Serialize a document to `.docx` bytes, adding a numbering part when it uses
/// lists (so markers survive the round-trip and open correctly in Word).
fn doc_to_docx(doc: &Document, comments: &[Comment], base: Option<&Package>) -> Vec<u8> {
    use std::collections::HashSet;
    // With the original package in hand, re-serialize just document.xml back into
    // it — every other part (footnotes, headers/footers, images, themes, …) is
    // preserved. Otherwise build a minimal package (new/empty documents).
    let mut pkg = match base {
        Some(p) => {
            let mut p = p.clone();
            p.document = doc.clone();
            p
        }
        None => {
            let has_list = doc.body.iter().any(|b| matches!(b, Block::Paragraph(p) if p.props.num_id.is_some()));
            if has_list { docxcore::package::new_markdown_package(doc.clone()) } else { docxcore::package::new_package(doc.clone()) }
        }
    };
    // Reconcile comments.xml with the tab's comment list: the base already holds the
    // comments it was loaded with, so only remove the deleted ones and add the new.
    let existing: Vec<i32> = base.map(|p| docxcore::comments::parse_comments(p).iter().filter_map(|c| c.id.parse().ok()).collect()).unwrap_or_default();
    let current: HashSet<i32> = comments.iter().filter_map(|c| c.id.parse().ok()).collect();
    for id in &existing {
        if !current.contains(id) {
            pkg.remove_comment(*id);
        }
    }
    let existing_set: HashSet<i32> = existing.iter().copied().collect();
    for c in comments {
        if let Ok(id) = c.id.parse::<i32>() {
            if !existing_set.contains(&id) {
                pkg.add_comment(id, &c.author, &c.initials, &c.date, &c.text);
            }
        }
    }
    docxcore::package::save_package(&pkg)
}

impl Docxy {
    fn new(cx: &mut Context<Self>) -> Self {
        let session: Session = std::fs::read(session_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        let mut tabs = Vec::new();
        for t in &session.tabs {
            let path = t.path.as_ref().map(PathBuf::from);
            // Prefer the hot-exit sidecar (current, possibly unsaved content); fall
            // back to the real file on disk, then to an empty doc.
            let hot = t.hot.as_ref().map(PathBuf::from).filter(|p| p.exists());
            let mut tab = match (t.kind, &hot) {
                (Kind::Docx, Some(hp)) => {
                    let mut l = doc_from_path(hp);
                    l.status = if t.dirty { "unsaved — restored".into() } else { "loaded".into() };
                    l.into_tab(t.kind, t.title.clone().into(), path, t.dirty)
                }
                // Spreadsheet with unsaved content: load the hot .xlsx sidecar but
                // keep the original on-disk `path` (so Save still targets the real
                // file; a never-saved sheet keeps path=None → Save prompts Save As).
                (Kind::Xlsx, Some(hp)) => {
                    let (surface, _) = sheet_from_path(hp);
                    let status = if t.dirty { "unsaved — restored" } else { "loaded" };
                    DocTab { kind: Kind::Xlsx, title: t.title.clone().into(), path, surface, dirty: t.dirty, status: status.into(), comments: vec![], pkg: None, notes: vec![], markdown: false, hf_edit: None }
                }
                _ => {
                    let (surface, comments, notes, pkg, status) = build_surface(t.kind, path.as_ref());
                    let markdown = path.as_deref().map(is_markdown_path).unwrap_or(false);
                    DocTab { kind: t.kind, title: t.title.clone().into(), path, surface, dirty: t.dirty, status, comments, pkg, notes, markdown, hf_edit: None }
                }
            };
            // The hot sidecar is always .docx; restore the Markdown flag from session.
            tab.markdown = t.markdown || tab.markdown;
            tabs.push(tab);
        }
        if tabs.is_empty() {
            tabs.push(sample_doc().into_tab(Kind::Docx, "sample.docx".into(), None, false));
        }
        let active = session.active.min(tabs.len().saturating_sub(1));
        let this = Self::build(tabs, active, session.theme, session.ask_on_close, cx);
        this.persist();
        this
    }

    /// Assemble the app state from ready tabs, with everything else at defaults.
    /// The disk-free half of `new`, so tests can seed a known document without
    /// touching the user's session.
    fn build(tabs: Vec<DocTab>, active: usize, theme_pref: ThemePref, ask_on_close: bool, cx: &mut Context<Self>) -> Self {
        Self {
            tabs,
            active,
            focus: cx.focus_handle(),
            focused: false,
            ribbon_tab: RibbonTab::Home,
            ribbon_min: false,
            backstage: false,
            bs_new: false,
            clip: None,
            theme_pref,
            ask_on_close,
            applied: None,
            find_open: false,
            find_query: String::new(),
            replace_text: String::new(),
            find_field: FindField::Query,
            find_case: false,
            picker: None,
            doc_scroll: ScrollHandle::new(),
            comment_open: false,
            comment_text: String::new(),
            show_marks: false,
            show_comments: false,
            show_nav: false,
            show_notes: false,
            page_view: false,
            show_ruler: false,
            ruler_drag: None,
            ruler_tab: docxcore::model::TabAlign::Left,
            ruler_x0: std::rc::Rc::new(std::cell::Cell::new(0.0)),
            selecting: false,
            sheet_dragging: false,
            sheet_fill: None,
            keytips: KeyTip::Off,
            context_menu: None,
            mini_bar: None,
            zoom: 1.0,
            ruler_guide: None,
            grid_clip: None,
            sheet_pick: None,
            sheet_rename: None,
            sheet_grid_w: 1000.0,
            sheet_comment_edit: None,
            sheet_dv_open: false,
            sheet_numfmt_open: false,
            sheet_fmt_open: false,
            sheet_cf_edit: None,
            sheet_dv_edit: None,
            sheet_filter_edit: None,
            sheet_ttc_edit: None,
            sheet_sort_edit: None,
            sheet_rowh_edit: None,
        }
    }


    fn persist(&self) {
        let hd = hot_dir();
        let _ = std::fs::create_dir_all(&hd);
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Write the tab's live content to a sidecar so unsaved edits are
                // held across a restart (closing never loses work). Docs → .docx,
                // spreadsheets → .xlsx; both are restored in preference to `path`.
                let hot = match &t.surface {
                    Surface::Doc(ed) => {
                        let p = hd.join(format!("tab-{i}.docx"));
                        std::fs::write(&p, doc_to_docx(&ed.doc, &t.comments, t.pkg.as_ref())).ok().map(|_| p.display().to_string())
                    }
                    Surface::Sheet(v) => {
                        let p = hd.join(format!("tab-{i}.xlsx"));
                        std::fs::write(&p, sheet_bytes(v)).ok().map(|_| p.display().to_string())
                    }
                    Surface::Placeholder => None,
                };
                PersistTab {
                    kind: t.kind,
                    title: t.title.to_string(),
                    path: t.path.as_ref().map(|p| p.display().to_string()),
                    dirty: t.dirty,
                    hot,
                    markdown: t.markdown,
                }
            })
            .collect();
        let session = Session { tabs, active: self.active, theme: self.theme_pref, ask_on_close: self.ask_on_close };
        if let Ok(json) = serde_json::to_string_pretty(&session) {
            let p = session_path();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json);
        }
    }

    fn refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_pref = self.theme_pref.next();
        self.applied = None; // force re-apply on next render
        self.persist();
        self.refocus(window, cx);
    }

    fn add_tab(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let (title, surface): (SharedString, Surface) = match kind {
            Kind::Docx => ("Untitled.docx".into(), Surface::Doc(Editor::new(empty_doc()))),
            Kind::Xlsx => ("Untitled.xlsx".into(), new_sheet_surface()),
            Kind::Look => ("Inbox".into(), Surface::Placeholder),
        };
        self.tabs.push(DocTab { kind, title, path: None, surface, dirty: false, status: "new".into(), comments: vec![], pkg: None, notes: vec![], markdown: false, hf_edit: None });
        self.active = self.tabs.len() - 1;
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    /// Move the spreadsheet selection to a cell (from a grid click), collapsing
    /// the range and committing any in-progress edit first.
    fn select_cell(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        if self.active_sheet().is_some_and(|v| v.editing.is_some()) {
            self.sheet_commit(0, 0, cx); // commit in place before moving away
        }
        if let Some(v) = self.active_sheet_mut() {
            v.sel = (row, col);
            v.anchor = (row, col);
            v.editing = None;
        }
        cx.notify();
    }

    /// Begin an auto-fill drag from the selection's fill handle (the small
    /// square at the range's bottom-right). Captures the source range.
    fn sheet_fill_start(&mut self, cx: &mut Context<Self>) {
        if self.sheet_fill.is_some() {
            return;
        }
        if let Some(v) = self.active_sheet() {
            let src = v.range();
            self.sheet_fill = Some(FillDrag { src, to: (src.2, src.3) });
        }
        cx.notify();
    }

    /// Update the auto-fill target as the handle is dragged; the selection
    /// highlight extends to preview the fill box (dominant axis).
    fn sheet_fill_over(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        let Some(mut f) = self.sheet_fill else { return };
        if f.to == (row, col) {
            return;
        }
        f.to = (row, col);
        self.sheet_fill = Some(f);
        let (r0, c0, r1, c1) = fill_box(f.src, (row, col));
        if let Some(v) = self.active_sheet_mut() {
            v.anchor = (r0, c0);
            v.sel = (r1, c1);
        }
        cx.notify();
    }

    /// Finish an auto-fill drag: fill the source pattern into the dragged region
    /// (numeric series or copy), leaving the filled box selected.
    fn sheet_fill_end(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.sheet_fill.take() else { return };
        if f.to == (f.src.2, f.src.3) {
            return; // never dragged off the source
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            gridcore::edit::autofill(&mut v.pkg.workbook, s, f.src, f.to);
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Left-drag over a cell: the first cell of the drag plants the anchor (and
    /// commits any in-progress edit); subsequent cells extend the selection.
    /// Driven by cell `on_mouse_move` while the left button is held (the
    /// virtualized list swallows child `on_mouse_down`, so the drag start is
    /// inferred from the first move rather than a press).
    fn sheet_drag_over(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        if !self.sheet_dragging {
            self.sheet_dragging = true;
            self.select_cell(row, col, cx); // anchor + sel at the drag origin
        } else {
            // Only re-render when the target cell actually changed.
            if self.active_sheet().is_some_and(|v| v.sel != (row, col)) {
                self.extend_to(row, col, cx);
            }
        }
    }

    /// Extend the selection to a cell (Shift+click), keeping the anchor.
    fn extend_to(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        if self.active_sheet().is_some_and(|v| v.editing.is_some()) {
            self.sheet_commit(0, 0, cx);
        }
        if let Some(v) = self.active_sheet_mut() {
            v.sel = (row, col);
            v.editing = None;
        }
        cx.notify();
    }

    /// Switch the active spreadsheet to another sheet (from a sheet-tab click).
    fn select_sheet(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(Surface::Sheet(v)) = self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            v.active = idx.min(v.pkg.workbook.sheets.len().saturating_sub(1));
            v.sel = (0, 0);
            v.anchor = (0, 0);
            v.editing = None;
            cx.notify();
        }
    }

    /// Adjust `col0` (horizontal scroll) so the selected column stays visible in
    /// `avail_w` px of grid width. Called each render before drawing the grid, so
    /// arrow-key navigation past the right edge scrolls columns into view.
    fn reconcile_sheet_hscroll(&mut self, avail_w: f32) {
        if let Some(v) = self.active_sheet_mut() {
            let (_, frz_c) = v.sheet().freeze;
            let fc = frz_c.min(64);
            // The scroll offset never enters the frozen region.
            if v.col0 < fc {
                v.col0 = fc;
            }
            // Only re-centre on the selection when it has actually moved; otherwise
            // leave col0 alone so manual scrolling (arrows/wheel/thumb) sticks.
            if v.sel == v.follow_sel {
                return;
            }
            v.follow_sel = v.sel;
            let sc = v.sel.1;
            if sc < fc {
                return; // a frozen column is always visible
            }
            // Available width for the scrollable region excludes the pinned columns.
            let frozen_w: f32 = (0..fc).map(|c| col_px(v.sheet().col_width(c))).sum();
            let avail = (avail_w - frozen_w).max(80.0);
            let col0 = v.col0;
            v.col0 = scroll_col0_for_sel(|c| col_px(v.sheet().col_width(c)), col0, fc, sc, avail);
        }
    }

    /// Scroll horizontally by `delta` columns (Shift+wheel), clamped to the extent.
    fn sheet_hscroll(&mut self, delta: i32, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            let (_, mc) = v.extent();
            let nv = (v.col0 as i32 + delta).clamp(0, mc as i32);
            if nv as u32 != v.col0 {
                v.col0 = nv as u32;
                cx.notify();
            }
        }
    }

    /// Add a new blank sheet (unique "SheetN" name) and switch to it.
    fn sheet_add(&mut self, cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            // Pick the lowest "SheetN" not already taken.
            let mut n = v.pkg.workbook.sheets.len() + 1;
            let taken = |v: &SheetView, name: &str| v.pkg.workbook.sheets.iter().any(|s| s.name.eq_ignore_ascii_case(name));
            while taken(v, &format!("Sheet{n}")) {
                n += 1;
            }
            let idx = v.pkg.add_sheet(&format!("Sheet{n}"));
            v.active = idx;
            v.sel = (0, 0);
            v.anchor = (0, 0);
            v.editing = None;
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Delete sheet `idx` (guarded: never the last sheet). Fixes up the active
    /// index and any pivot/chart views that referenced shifted sheet indices.
    fn sheet_delete(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            if v.pkg.workbook.sheets.len() <= 1 || !v.pkg.remove_sheet(idx) {
                return;
            }
            // Drop views on the removed sheet; shift indices above it down one.
            v.pivot_views.retain(|d| d.out_sheet != idx);
            for d in &mut v.pivot_views {
                if d.out_sheet > idx {
                    d.out_sheet -= 1;
                }
                if d.src_sheet > idx {
                    d.src_sheet -= 1;
                }
            }
            v.charts.retain(|c| c.sheet != idx);
            for c in &mut v.charts {
                if c.sheet > idx {
                    c.sheet -= 1;
                }
            }
            if v.active >= v.pkg.workbook.sheets.len() {
                v.active = v.pkg.workbook.sheets.len() - 1;
            }
            v.sel = (0, 0);
            v.anchor = (0, 0);
            v.editing = None;
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Begin an inline rename of tab `idx`, seeding the buffer with its name.
    fn sheet_begin_rename(&mut self, idx: usize, cx: &mut Context<Self>) {
        let name = self
            .active_sheet()
            .and_then(|v| v.pkg.workbook.sheets.get(idx).map(|s| s.name.clone()));
        if let Some(name) = name {
            self.sheet_rename = Some((idx, name));
            cx.notify();
        }
    }

    /// Route a keystroke into the active inline rename (char/backspace/enter/esc).
    fn sheet_rename_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some((idx, mut buf)) = self.sheet_rename.clone() else { return };
        match key {
            "escape" => self.sheet_rename = None,
            "enter" => {
                if let Some(v) = self.active_sheet_mut() {
                    v.pkg.rename_sheet(idx, &buf);
                }
                self.sheet_rename = None;
                self.mark_sheet_dirty();
            }
            "backspace" => {
                buf.pop();
                self.sheet_rename = Some((idx, buf));
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_rename = Some((idx, buf));
            }
        }
        cx.notify();
    }

    // ---- spreadsheet cell editing -----------------------------------------

    fn active_sheet(&self) -> Option<&SheetView> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Sheet(v)) => Some(v),
            _ => None,
        }
    }
    fn active_sheet_mut(&mut self) -> Option<&mut SheetView> {
        match self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            Some(Surface::Sheet(v)) => Some(v),
            _ => None,
        }
    }
    /// Whether the active sheet is protected (cells read-only until unprotected).
    fn sheet_protected(&self) -> bool {
        self.active_sheet().is_some_and(|v| v.pkg.workbook.sheets[v.active].is_protected())
    }

    /// Toggle protection on the active sheet (undoable; Excel's default flag set).
    fn sheet_toggle_protection(&mut self, cx: &mut Context<Self>) {
        let now = !self.sheet_protected();
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            v.pkg.workbook.sheets[s].set_protected(now);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    fn active_is_sheet(&self) -> bool {
        matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Sheet(_)))
    }
    fn mark_sheet_dirty(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.dirty = true;
        }
    }

    /// Begin editing the selected cell. `initial` seeds the buffer (a freshly
    /// typed character); `None` re-edits the existing content (F2).
    fn sheet_begin_edit(&mut self, initial: Option<String>, cx: &mut Context<Self>) {
        if self.sheet_protected() {
            return;
        }
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            v.editing = Some(initial.unwrap_or_else(|| v.edit_string(r, c)));
            v.edit_caret_to_end();
        }
        cx.notify();
    }

    /// Push a workbook snapshot onto the undo stack (clearing redo). Called before
    /// each mutating grid operation.
    fn sheet_snapshot(&mut self) {
        if let Some(v) = self.active_sheet_mut() {
            v.undo.push(SheetSnapshot { wb: v.pkg.workbook.clone(), active: v.active, sel: v.sel, anchor: v.anchor });
            if v.undo.len() > 100 {
                v.undo.remove(0);
            }
            v.redo.clear();
        }
    }

    fn sheet_undo(&mut self, cx: &mut Context<Self>) {
        let mut done = false;
        if let Some(v) = self.active_sheet_mut() {
            if let Some(snap) = v.undo.pop() {
                v.redo.push(SheetSnapshot { wb: v.pkg.workbook.clone(), active: v.active, sel: v.sel, anchor: v.anchor });
                v.restore(snap);
                done = true;
            }
        }
        if done {
            self.mark_sheet_dirty();
        }
        cx.notify();
    }

    fn sheet_redo(&mut self, cx: &mut Context<Self>) {
        let mut done = false;
        if let Some(v) = self.active_sheet_mut() {
            if let Some(snap) = v.redo.pop() {
                v.undo.push(SheetSnapshot { wb: v.pkg.workbook.clone(), active: v.active, sel: v.sel, anchor: v.anchor });
                v.restore(snap);
                done = true;
            }
        }
        if done {
            self.mark_sheet_dirty();
        }
        cx.notify();
    }

    /// Commit the in-progress edit (if any) into the workbook, recalc, and move
    /// the selection by (dr, dc).
    fn sheet_commit(&mut self, dr: i32, dc: i32, cx: &mut Context<Self>) {
        let has_edit = self.active_sheet().is_some_and(|v| v.editing.is_some());
        if has_edit {
            self.sheet_snapshot();
        }
        if let Some(v) = self.active_sheet_mut() {
            if let Some(buf) = v.editing.take() {
                let (r, c) = v.sel;
                let s = v.active;
                let style = v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0);
                let cell = parse_cell_input(&buf, style);
                v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
            }
        }
        if has_edit {
            self.mark_sheet_dirty();
        }
        self.sheet_move(dr, dc, cx);
    }

    /// Move the selection by (dr, dc), clamped at the top-left origin, collapsing
    /// the range and discarding any in-progress edit.
    fn sheet_move(&mut self, dr: i32, dc: i32, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            v.sel = ((r as i32 + dr).max(0) as u32, (c as i32 + dc).max(0) as u32);
            v.anchor = v.sel;
            v.editing = None;
        }
        cx.notify();
    }

    /// Extend the selection by (dr, dc), keeping the anchor (Shift+arrow).
    fn sheet_extend(&mut self, dr: i32, dc: i32, editing: bool, cx: &mut Context<Self>) {
        if editing {
            self.sheet_commit(0, 0, cx);
        }
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            v.sel = ((r as i32 + dr).max(0) as u32, (c as i32 + dc).max(0) as u32);
            v.editing = None;
        }
        cx.notify();
    }

    /// Arrow-key navigation: commit an open edit first, then move.
    fn sheet_nav(&mut self, dr: i32, dc: i32, editing: bool, cx: &mut Context<Self>) {
        if editing {
            self.sheet_commit(0, 0, cx);
        }
        self.sheet_move(dr, dc, cx);
    }

    /// Clear the whole selected range's content (Delete / Backspace), keeping
    /// each cell's style.
    fn sheet_clear(&mut self, cx: &mut Context<Self>) {
        if self.sheet_protected() {
            return;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r0, c0, r1, c1) = v.range();
            let s = v.active;
            for r in r0..=r1 {
                for c in c0..=c1 {
                    let style = v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0);
                    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), gridcore::sheet::Cell { style, ..Default::default() });
                }
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Copy (or cut) the selected range into the grid clipboard and, as TSV, the
    /// system clipboard.
    fn sheet_copy(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(v) = self.active_sheet() else { return };
        let (r0, c0, r1, c1) = v.range();
        let mut cells = Vec::new();
        let mut tsv = String::new();
        for r in r0..=r1 {
            let mut row = Vec::new();
            for c in c0..=c1 {
                if c > c0 {
                    tsv.push('\t');
                }
                tsv.push_str(&v.cell_text(r, c));
                row.push(v.sheet().cell(r, c).cloned().unwrap_or_default());
            }
            cells.push(row);
            tsv.push('\n');
        }
        self.grid_clip = Some(GridClip { cells });
        cx.write_to_clipboard(ClipboardItem::new_string(tsv));
        if cut {
            self.sheet_clear(cx); // snapshots, clears the range, marks dirty
        } else {
            cx.notify();
        }
    }

    /// Paste at the selection: the grid clipboard when present (full-fidelity
    /// cells), else the system clipboard parsed as TSV.
    fn sheet_paste(&mut self, cx: &mut Context<Self>) {
        if self.sheet_protected() {
            return;
        }
        let block: Vec<Vec<gridcore::sheet::Cell>> = if let Some(clip) = &self.grid_clip {
            clip.cells.clone()
        } else if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            text.replace("\r\n", "\n")
                .trim_end_matches('\n')
                .split('\n')
                .map(|line| line.split('\t').map(|f| parse_cell_input(f, 0)).collect())
                .collect()
        } else {
            return;
        };
        if block.is_empty() {
            return;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (br, bc) = v.sel;
            let s = v.active;
            for (dr, row) in block.iter().enumerate() {
                for (dc, cell) in row.iter().enumerate() {
                    v.engine.set_cell(&mut v.pkg.workbook, (s, br + dr as u32, bc + dc as u32), cell.clone());
                }
            }
            let h = block.len() as u32;
            let w = block.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
            if h > 0 && w > 0 {
                v.anchor = (br + h - 1, bc + w - 1);
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    // ---- column resize -----------------------------------------------------

    fn col_resize_start(&mut self, col: u32, x: f32, _cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let w = v.sheet().col_width(col);
            v.col_drag = Some(ColDrag { col, start_x: x, start_w: w });
        }
    }
    fn col_resize_move(&mut self, x: f32, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(v) = self.active_sheet_mut() {
            if let Some(d) = v.col_drag {
                // px → character units, inverting col_px: units = (px - 6) / 7.
                let new_px = (col_px(d.start_w) + (x - d.start_x)).max(20.0);
                let new_units = (((new_px - 6.0) / 7.0) as f64).max(0.5);
                let s = v.active;
                v.pkg.workbook.sheets[s].set_col_width(d.col, new_units);
                changed = true;
            }
        }
        if changed {
            self.mark_sheet_dirty();
            cx.notify();
        }
    }
    fn col_resize_end(&mut self, cx: &mut Context<Self>) {
        let mut was = false;
        if let Some(v) = self.active_sheet_mut() {
            was = v.col_drag.take().is_some();
        }
        if was {
            cx.notify();
        }
    }

    // ---- cell formatting (ribbon) -----------------------------------------

    /// Apply a formatting change to every cell in the selection: mutate a copy of
    /// each cell's `Xf`, intern it (dedup), and re-point the cell's style. Values
    /// and formulas are untouched, so no recalc is needed.
    fn sheet_format(&mut self, apply: impl Fn(&mut gridcore::sheet::Xf), cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r0, c0, r1, c1) = v.range();
            let s = v.active;
            for r in r0..=r1 {
                for c in c0..=c1 {
                    let cur = v.sheet().cell(r, c).cloned();
                    let mut xf = v.pkg.workbook.styles.xf(cur.as_ref().map(|cl| cl.style).unwrap_or(0));
                    apply(&mut xf);
                    let idx = v.pkg.workbook.styles.intern(xf);
                    let mut cell = cur.unwrap_or_default();
                    cell.style = idx;
                    v.pkg.workbook.sheets[s].set_cell(r, c, cell);
                }
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// The active cell's current `Xf` (for reading a toggle's current state).
    fn active_xf(&self) -> gridcore::sheet::Xf {
        self.active_sheet()
            .map(|v| {
                let (r, c) = v.sel;
                v.pkg.workbook.styles.xf(v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0))
            })
            .unwrap_or_default()
    }

    // ---- cell comments -----------------------------------------------------

    /// The comment author to stamp on new comments (the OS user, else "docxy").
    fn comment_author() -> String {
        std::env::var("USERNAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "docxy".to_string())
    }

    /// The comment text on the active sheet's selected cell, if any.
    fn selected_comment(&self) -> Option<String> {
        let v = self.active_sheet()?;
        let (r, c) = v.sel;
        v.pkg
            .comments()
            .into_iter()
            .find(|cm| cm.sheet == v.active && cm.row == r && cm.col == c)
            .map(|cm| cm.text)
    }

    /// Open the comment entry bar for the selected cell, seeded with its existing
    /// comment text (so New Comment doubles as Edit).
    fn sheet_new_comment(&mut self, cx: &mut Context<Self>) {
        if self.active_sheet().is_some() {
            self.sheet_comment_edit = Some(self.selected_comment().unwrap_or_default());
            cx.notify();
        }
    }

    /// Route a keystroke into the conditional-format entry bar; Enter applies the
    /// rule (Light-Red preset) to the selection, Esc cancels.
    fn sheet_cf_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_cf_edit.clone() else { return };
        match key {
            "escape" => self.sheet_cf_edit = None,
            "enter" => {
                if buf.trim().eq_ignore_ascii_case("clear") {
                    self.sheet_snapshot();
                    if let Some(v) = self.active_sheet_mut() {
                        let s = v.active;
                        v.pkg.clear_conditional_formats(s);
                        v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
                    }
                    self.mark_sheet_dirty();
                } else if let Some((op, val, val2)) = parse_cf_input(&buf) {
                    self.sheet_snapshot();
                    if let Some(v) = self.active_sheet_mut() {
                        let s = v.active;
                        let (r0, c0, r1, c1) = v.range();
                        v.pkg.add_conditional_format(s, (r0, c0, r1, c1), op, &val, val2.as_deref(), cf_preset_dxf());
                        v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
                    }
                    self.mark_sheet_dirty();
                }
                self.sheet_cf_edit = None;
            }
            "backspace" => {
                buf.pop();
                self.sheet_cf_edit = Some(buf);
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_cf_edit = Some(buf);
            }
        }
        cx.notify();
    }

    /// Apply an AutoFilter criteria to the selected column: hide the rows of the
    /// contiguous region whose value fails it (header kept). "clear" unhides.
    fn sheet_apply_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        use gridcore::sheet::CellValue;
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let sc = v.sel.1;
            let cur_r = v.sel.0;
            let (max_r, max_c) = v.extent();
            // Contiguous region around the cursor.
            let sh = &v.pkg.workbook.sheets[s];
            let used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
            if !used(cur_r) {
                return;
            }
            let mut top = cur_r;
            while top > 0 && used(top - 1) {
                top -= 1;
            }
            let mut bottom = cur_r;
            while bottom < max_r && used(bottom + 1) {
                bottom += 1;
            }
            let header = matches!(sh.cell(top, sc).map(|c| &c.value), Some(CellValue::Text(_)));
            let start = if header { top + 1 } else { top };
            if text.trim().eq_ignore_ascii_case("clear") {
                for r in top..=bottom {
                    v.pkg.workbook.sheets[s].set_row_hidden(r, false);
                }
            } else if let Some((op, operand)) = gridcore::filter::parse(text) {
                let keep: Vec<bool> = (start..=bottom)
                    .map(|r| {
                        let val = v.pkg.workbook.sheets[s].cell(r, sc).map(|c| c.value.clone());
                        gridcore::filter::matches(val.as_ref(), &op, &operand)
                    })
                    .collect();
                for (i, r) in (start..=bottom).enumerate() {
                    v.pkg.workbook.sheets[s].set_row_hidden(r, !keep[i]);
                }
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Route a keystroke into the Text-to-Columns delimiter bar. Enter splits the
    /// selected column's rows by the delimiter into the columns to the right.
    fn sheet_ttc_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_ttc_edit.clone() else { return };
        match key {
            "escape" => self.sheet_ttc_edit = None,
            "enter" => {
                let delim = parse_delim(&buf);
                self.sheet_snapshot();
                if let Some(v) = self.active_sheet_mut() {
                    let s = v.active;
                    let (r0, c0, r1, _) = v.range();
                    gridcore::edit::text_to_columns(&mut v.pkg.workbook, s, c0, r0, r1, delim);
                    v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
                }
                self.mark_sheet_dirty();
                self.sheet_ttc_edit = None;
            }
            "backspace" => {
                buf.pop();
                self.sheet_ttc_edit = Some(buf);
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_ttc_edit = Some(buf);
            }
        }
        cx.notify();
    }

    /// Route a keystroke into the AutoFilter criteria bar (Enter applies, Esc cancels).
    fn sheet_filter_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_filter_edit.clone() else { return };
        match key {
            "escape" => {
                self.sheet_filter_edit = None;
                cx.notify();
            }
            "enter" => {
                self.sheet_filter_edit = None;
                self.sheet_apply_filter(&buf, cx);
            }
            "backspace" => {
                buf.pop();
                self.sheet_filter_edit = Some(buf);
                cx.notify();
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_filter_edit = Some(buf);
                cx.notify();
            }
        }
    }

    /// Route a keystroke into the multi-level sort entry bar; Enter runs the sort.
    fn sheet_sort_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_sort_edit.clone() else { return };
        match key {
            "escape" => {
                self.sheet_sort_edit = None;
                cx.notify();
            }
            "enter" => {
                self.sheet_sort_edit = None;
                self.sheet_commit_sort(&buf, cx);
            }
            "backspace" => {
                buf.pop();
                self.sheet_sort_edit = Some(buf);
                cx.notify();
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_sort_edit = Some(buf);
                cx.notify();
            }
        }
    }

    /// Route a keystroke into the row-height entry bar; Enter applies it.
    fn sheet_rowh_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_rowh_edit.clone() else { return };
        match key {
            "escape" => {
                self.sheet_rowh_edit = None;
                cx.notify();
            }
            "enter" => {
                self.sheet_rowh_edit = None;
                let t = buf.trim();
                let pts = if t.is_empty() || t.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    match t.parse::<f64>() {
                        Ok(h) if h > 0.0 => Some(h),
                        _ => {
                            cx.notify();
                            return;
                        }
                    }
                };
                self.sheet_set_row_height(pts, cx);
            }
            "backspace" => {
                buf.pop();
                self.sheet_rowh_edit = Some(buf);
                cx.notify();
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_rowh_edit = Some(buf);
                cx.notify();
            }
        }
    }

    /// Route a keystroke into the data-validation entry bar; Enter creates a list
    /// validation from the comma-separated values over the selection.
    fn sheet_dv_edit_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_dv_edit.clone() else { return };
        match key {
            "escape" => self.sheet_dv_edit = None,
            "enter" => {
                let items: Vec<&str> = buf.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
                if !items.is_empty() {
                    let f1 = format!("\"{}\"", items.join(","));
                    self.sheet_snapshot();
                    if let Some(v) = self.active_sheet_mut() {
                        let s = v.active;
                        let (r0, c0, r1, c1) = v.range();
                        v.pkg.add_data_validation(s, (r0, c0, r1, c1), "list", "", &f1, None);
                    }
                    self.mark_sheet_dirty();
                }
                self.sheet_dv_edit = None;
            }
            "backspace" => {
                buf.pop();
                self.sheet_dv_edit = Some(buf);
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_dv_edit = Some(buf);
            }
        }
        cx.notify();
    }

    /// Route a keystroke into the open comment bar (char / backspace / enter / esc).
    fn sheet_comment_key(&mut self, ev: &KeyDownEvent, key: &str, cx: &mut Context<Self>) {
        let Some(mut buf) = self.sheet_comment_edit.clone() else { return };
        match key {
            "escape" => {
                self.sheet_comment_edit = None;
                cx.notify();
            }
            "enter" => self.sheet_commit_comment(cx),
            "backspace" => {
                buf.pop();
                self.sheet_comment_edit = Some(buf);
                cx.notify();
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        buf.push_str(c);
                    }
                }
                self.sheet_comment_edit = Some(buf);
                cx.notify();
            }
        }
    }

    /// Commit the comment bar's buffer onto the selected cell (empty = delete).
    fn sheet_commit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.sheet_comment_edit.take() else { return };
        let author = Self::comment_author();
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            let s = v.active;
            let t = text.trim();
            if t.is_empty() {
                v.pkg.remove_comment(s, r, c);
            } else {
                v.pkg.set_comment(s, r, c, &author, t);
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    fn sheet_delete_comment(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            let s = v.active;
            v.pkg.remove_comment(s, r, c);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Move the selection to the next / previous commented cell (row-major).
    fn sheet_comment_nav(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            let mut cells: Vec<(u32, u32)> = v
                .pkg
                .comments()
                .into_iter()
                .filter(|cm| cm.sheet == v.active)
                .map(|cm| (cm.row, cm.col))
                .collect();
            if cells.is_empty() {
                return;
            }
            cells.sort_unstable();
            cells.dedup();
            let cur = v.sel;
            let next = if forward {
                cells.iter().find(|&&x| x > cur).copied().unwrap_or(cells[0])
            } else {
                cells.iter().rev().find(|&&x| x < cur).copied().unwrap_or(*cells.last().unwrap())
            };
            v.sel = next;
            v.anchor = next;
            // Bring the target row into view (columns follow via reconcile).
            v.vlist.scroll_to_reveal_item(v.row_list_index(next.0));
        }
        cx.notify();
    }

    /// Merge & Center: merge the selected range into one cell (centring the
    /// anchor's contents), or unmerge if the anchor is already a merge origin.
    fn sheet_merge_toggle(&mut self, cx: &mut Context<Self>) {
        use gridcore::sheet::Align;
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r0, c0, r1, c1) = v.range();
            let s = v.active;
            let wb = &mut v.pkg.workbook;
            if let Some(i) = wb.sheets[s].merges.iter().position(|&(mr1, mc1, _, _)| mr1 == r0 && mc1 == c0) {
                wb.sheets[s].merges.remove(i);
            } else if r1 > r0 || c1 > c0 {
                wb.sheets[s].merges.push((r0, c0, r1, c1));
                // Centre the anchor cell (interning a Center-aligned xf).
                let style = wb.sheets[s].cell(r0, c0).map(|x| x.style).unwrap_or(0);
                let mut xf = wb.styles.xf(style);
                xf.align = Align::Center;
                let idx = wb.styles.intern(xf);
                wb.sheets[s].cells.entry((r0, c0)).or_default().style = idx;
            }
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// AutoSum (Σ): insert `=SUM(range)` in the selected cell, summing the run of
    /// numeric cells directly above it (else to its left) — Excel's behaviour.
    fn sheet_autosum(&mut self, cx: &mut Context<Self>) {
        use gridcore::sheet::{cell_name, Cell, CellValue};
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let (r, c) = v.sel;
            let sh = &v.pkg.workbook.sheets[s];
            let is_num = |rr: u32, cc: u32| matches!(sh.cell(rr, cc).map(|x| &x.value), Some(CellValue::Number(_)));
            let range = if r > 0 && is_num(r - 1, c) {
                let mut top = r - 1;
                while top > 0 && is_num(top - 1, c) {
                    top -= 1;
                }
                Some(format!("{}:{}", cell_name(top, c), cell_name(r - 1, c)))
            } else if c > 0 && is_num(r, c - 1) {
                let mut left = c - 1;
                while left > 0 && is_num(r, left - 1) {
                    left -= 1;
                }
                Some(format!("{}:{}", cell_name(r, left), cell_name(r, c - 1)))
            } else {
                None
            };
            let Some(range) = range else { return };
            let style = sh.cell(r, c).map(|x| x.style).unwrap_or(0);
            let cell = Cell { style, ..Cell::formula(&format!("SUM({range})")) };
            v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Remove duplicate rows in the contiguous region around the selection
    /// (header-aware), keeping the first occurrence.
    fn sheet_remove_duplicates(&mut self, cx: &mut Context<Self>) {
        use gridcore::sheet::CellValue;
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let sc = v.sel.1;
            let cur_r = v.sel.0;
            let (max_r, max_c) = v.extent();
            let sh = &v.pkg.workbook.sheets[s];
            let used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
            if !used(cur_r) {
                return;
            }
            let mut top = cur_r;
            while top > 0 && used(top - 1) {
                top -= 1;
            }
            let mut bottom = cur_r;
            while bottom < max_r && used(bottom + 1) {
                bottom += 1;
            }
            let header = matches!(sh.cell(top, sc).map(|c| &c.value), Some(CellValue::Text(_)));
            gridcore::edit::dedupe_rows(&mut v.pkg.workbook, s, top, bottom, header);
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Subtotal: at each change in the selected column's value, insert a
    /// SUBTOTAL(9,…) row over the numeric columns plus a grand total; detail
    /// rows are grouped (outline level 1) for collapse. The region should be
    /// sorted by that column first.
    fn sheet_subtotal(&mut self, cx: &mut Context<Self>) {
        use gridcore::sheet::CellValue;
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let sc = v.sel.1;
            let cur_r = v.sel.0;
            let (max_r, max_c) = v.extent();
            let sh = &v.pkg.workbook.sheets[s];
            let used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
            if !used(cur_r) {
                return;
            }
            let mut top = cur_r;
            while top > 0 && used(top - 1) {
                top -= 1;
            }
            let mut bottom = cur_r;
            while bottom < max_r && used(bottom + 1) {
                bottom += 1;
            }
            let header = matches!(sh.cell(top, sc).map(|c| &c.value), Some(CellValue::Text(_)));
            gridcore::edit::subtotal(&mut v.pkg.workbook, s, top, bottom, sc, &[], header);
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Collapse (hide) or expand all grouped detail rows (outline level ≥ 1),
    /// leaving the subtotal rows visible.
    fn sheet_toggle_outline(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let sh = &v.pkg.workbook.sheets[s];
            let outlined: Vec<u32> = sh.row_attrs.keys().copied().filter(|&r| sh.row_outline(r) >= 1).collect();
            if outlined.is_empty() {
                return;
            }
            let any_visible = outlined.iter().any(|&r| !sh.row_hidden(r));
            for &r in &outlined {
                v.pkg.workbook.sheets[s].set_row_hidden(r, any_visible);
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Format as Table: wrap the active multi-cell selection (or the contiguous
    /// region grown around the cursor) in an Excel Table — banded, filterable,
    /// and styled by Excel on open. The first row becomes headers when it is all
    /// text.
    fn sheet_format_as_table(&mut self, cx: &mut Context<Self>) {
        use gridcore::sheet::CellValue;
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let (max_r, max_c) = v.extent();
            let region = if v.has_range() {
                Some(v.range())
            } else {
                let sh = &v.pkg.workbook.sheets[s];
                let (cur_r, cur_c) = v.sel;
                let row_used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
                if !row_used(cur_r) {
                    None
                } else {
                    let mut top = cur_r;
                    while top > 0 && row_used(top - 1) {
                        top -= 1;
                    }
                    let mut bottom = cur_r;
                    while bottom < max_r && row_used(bottom + 1) {
                        bottom += 1;
                    }
                    let col_used = |c: u32| (top..=bottom).any(|r| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
                    let mut left = cur_c;
                    while left > 0 && col_used(left - 1) {
                        left -= 1;
                    }
                    let mut right = cur_c;
                    while right < max_c && col_used(right + 1) {
                        right += 1;
                    }
                    Some((top, left, bottom, right))
                }
            };
            if let Some((r1, c1, r2, c2)) = region {
                let sh = &v.pkg.workbook.sheets[s];
                let has_header = (c1..=c2).all(|c| matches!(sh.cell(r1, c).map(|cl| &cl.value), Some(CellValue::Text(_))));
                v.pkg.add_table(s, (r1, c1, r2, c2), has_header, "TableStyleMedium2");
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// The contiguous region around the selection to sort, as `(start, bottom)`
    /// data-row bounds (header excluded). A header is inferred when the top row
    /// carries a text label over numeric data in *any* column. `None` when
    /// there's nothing to sort.
    fn sheet_sort_bounds(&self) -> Option<(u32, u32)> {
        use gridcore::sheet::CellValue;
        let v = self.active_sheet()?;
        let s = v.active;
        let (max_r, max_c) = v.extent();
        let sh = &v.pkg.workbook.sheets[s];
        let row_used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
        let sr = v.sel.0;
        if !row_used(sr) {
            return None;
        }
        let mut top = sr;
        while top > 0 && row_used(top - 1) {
            top -= 1;
        }
        let mut bottom = sr;
        while bottom < max_r && row_used(bottom + 1) {
            bottom += 1;
        }
        let header = (0..=max_c).any(|c| {
            matches!(sh.cell(top, c).map(|cl| &cl.value), Some(CellValue::Text(_)))
                && (top + 1..=bottom).any(|r| matches!(sh.cell(r, c).map(|cl| &cl.value), Some(CellValue::Number(_))))
        });
        let start = if header { top + 1 } else { top };
        (bottom > start).then_some((start, bottom))
    }

    /// Sort the current region by the selected column (header-aware). Rows move
    /// as whole units (all columns + styles); blanks sort last. Formula refs are
    /// not re-based, so this targets value tables (the common case).
    fn sheet_sort(&mut self, ascending: bool, cx: &mut Context<Self>) {
        let Some((start, bottom)) = self.sheet_sort_bounds() else {
            return;
        };
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let sc = v.sel.1;
            gridcore::edit::sort_rows(&mut v.pkg.workbook, s, start, bottom, &[(sc, ascending)]);
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Multi-level sort of the current region from a typed spec like
    /// "B asc, C desc" (column letters, optional asc/desc, default ascending).
    fn sheet_commit_sort(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(keys) = gridcore::edit::parse_sort_spec(text) else {
            return;
        };
        let Some((start, bottom)) = self.sheet_sort_bounds() else {
            return;
        };
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            gridcore::edit::sort_rows(&mut v.pkg.workbook, s, start, bottom, &keys);
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Insert/delete a whole row or column at the selection (Home ▸ Cells), then
    /// rebuild the recalc engine so shifted formulas re-evaluate.
    fn sheet_structural(&mut self, op: StructOp, cx: &mut Context<Self>) {
        use gridcore::edit;
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let (r, c) = v.sel;
            let wb = &mut v.pkg.workbook;
            match op {
                StructOp::InsertRow => edit::insert_rows(wb, s, r, 1),
                StructOp::DeleteRow => edit::delete_rows(wb, s, r, 1),
                StructOp::InsertCol => edit::insert_cols(wb, s, c, 1),
                StructOp::DeleteCol => edit::delete_cols(wb, s, c, 1),
            }
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Follow the hyperlink on cell (r,c) of the active sheet, if any: jump for an
    /// in-workbook `#Sheet!A1` target, else open the URL externally.
    fn sheet_follow_hyperlink(&mut self, r: u32, c: u32, cx: &mut Context<Self>) {
        let link = self.active_sheet().and_then(|v| v.sheet().hyperlinks.get(&(r, c)).cloned());
        let Some(link) = link else { return };
        if let Some(loc) = link.strip_prefix('#') {
            let (sheet_name, cellref) = match loc.rsplit_once('!') {
                Some((s, cr)) => (Some(s.trim_matches('\'').to_string()), cr.to_string()),
                None => (None, loc.to_string()),
            };
            if let Some(v) = self.active_sheet_mut() {
                if let Some(sn) = sheet_name {
                    if let Some(idx) = v.pkg.workbook.sheets.iter().position(|s| s.name == sn) {
                        v.active = idx;
                    }
                }
                if let Some((rr, cc)) = gridcore::sheet::parse_cell_name(&cellref.replace('$', "")) {
                    v.sel = (rr, cc);
                    v.anchor = (rr, cc);
                    v.vlist.scroll_to_reveal_item(v.row_list_index(rr));
                }
            }
            cx.notify();
        } else {
            cx.open_url(&link);
        }
    }

    // ---- data-validation list dropdown -------------------------------------

    /// If the selected cell has a `list` data validation, the allowed values —
    /// an inline `"a,b,c"` list or the contents of a referenced range.
    fn dv_list_values(&self) -> Option<Vec<String>> {
        use gridcore::sheet::CellValue;
        let v = self.active_sheet()?;
        let (r, c) = v.sel;
        let sh = v.sheet();
        let dv = sh.validations.iter().find(|d| d.kind == "list" && d.covers(r, c))?;
        let f = dv.formula1.trim();
        // Inline list: "Yes,No,Maybe".
        if f.len() >= 2 && f.starts_with('"') && f.ends_with('"') {
            return Some(
                f[1..f.len() - 1]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        }
        // Range reference, optionally sheet-qualified.
        let (sheet_idx, rref) = match f.split_once('!') {
            Some((sname, rest)) => {
                let sname = sname.trim_matches('\'');
                (v.pkg.workbook.sheets.iter().position(|s| s.name == sname)?, rest)
            }
            None => (v.active, f),
        };
        let (r1, c1, r2, c2) = gridcore::sheet::parse_range_name(&rref.replace('$', ""))?;
        let src = v.pkg.workbook.sheets.get(sheet_idx)?;
        let mut out = Vec::new();
        for rr in r1..=r2 {
            for cc in c1..=c2 {
                let t = match src.cell(rr, cc).map(|cl| &cl.value) {
                    Some(CellValue::Text(s)) => s.clone(),
                    Some(CellValue::Number(n)) => n.to_string(),
                    Some(CellValue::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                    _ => String::new(),
                };
                if !t.is_empty() {
                    out.push(t);
                }
            }
        }
        Some(out)
    }

    fn sheet_dv_toggle(&mut self, cx: &mut Context<Self>) {
        self.sheet_dv_open = !self.sheet_dv_open;
        cx.notify();
    }

    /// Set the selected cell to `value` (a picked validation option).
    fn sheet_dv_pick(&mut self, value: String, cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            let s = v.active;
            let style = v.sheet().cell(r, c).map(|x| x.style).unwrap_or(0);
            v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), parse_cell_input(&value, style));
        }
        self.sheet_dv_open = false;
        self.mark_sheet_dirty();
        cx.notify();
    }

    fn sheet_toggle_bold(&mut self, cx: &mut Context<Self>) {
        let on = !self.active_xf().bold;
        self.sheet_format(move |xf| xf.bold = on, cx);
    }
    fn sheet_toggle_italic(&mut self, cx: &mut Context<Self>) {
        let on = !self.active_xf().italic;
        self.sheet_format(move |xf| xf.italic = on, cx);
    }
    fn sheet_align(&mut self, a: gridcore::sheet::Align, cx: &mut Context<Self>) {
        self.sheet_format(move |xf| xf.align = a, cx);
    }
    /// Toggle Wrap Text on the selection. Wrapped cells render across multiple
    /// lines and grow their row (the grid uses a variable-height gpui `list`).
    fn sheet_toggle_wrap(&mut self, cx: &mut Context<Self>) {
        let on = !self.active_xf().wrap;
        self.sheet_format(move |xf| xf.wrap = on, cx);
    }
    /// Set an explicit height (points) on every selected row, or clear it back
    /// to auto-fit when `pts` is `None`.
    fn sheet_set_row_height(&mut self, pts: Option<f64>, cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let (r0, _, r1, _) = v.range();
            for r in r0..=r1 {
                v.pkg.workbook.sheets[s].set_row_height(r, pts);
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }
    /// Grow / shrink the font of the selection by one point (default base 11).
    fn sheet_font_step(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.sheet_format(move |xf| {
            let cur = xf.font_size.unwrap_or(11.0);
            xf.font_size = Some((cur + delta).clamp(1.0, 409.0));
        }, cx);
    }
    /// Apply a number format code to the selection (Excel's %, currency, comma).
    fn sheet_numfmt(&mut self, code: &'static str, cx: &mut Context<Self>) {
        self.sheet_format(move |xf| xf.code = Some(code.to_string()), cx);
    }

    /// Apply a number format from the Number dropdown ("" = General/clear) + close.
    fn sheet_apply_numfmt(&mut self, code: &str, cx: &mut Context<Self>) {
        let code = code.to_string();
        self.sheet_format(move |xf| xf.code = if code.is_empty() { None } else { Some(code.clone()) }, cx);
        self.sheet_numfmt_open = false;
    }

    /// Friendly name for the selected cell's current number format.
    fn active_numfmt_name(&self) -> &'static str {
        let code = self.active_xf().code;
        match code {
            None => "General",
            Some(c) => NUM_FORMATS.iter().find(|(_, fc)| *fc == c.as_str()).map(|(n, _)| *n).unwrap_or("Custom"),
        }
    }
    /// Set fill or font colour on the selection from a swatch (None = clear), and
    /// close the picker.
    fn sheet_apply_color(&mut self, pick: SheetPick, rgb: Option<(u8, u8, u8)>, cx: &mut Context<Self>) {
        self.sheet_format(move |xf| match pick {
            SheetPick::Fill => xf.fill = rgb,
            SheetPick::Font => xf.color = rgb,
        }, cx);
        self.sheet_pick = None;
        cx.notify();
    }
    /// Toggle a thin box border on the selected cells.
    fn sheet_toggle_border(&mut self, cx: &mut Context<Self>) {
        let on = !self.active_xf().border;
        self.sheet_format(move |xf| xf.border = on, cx);
    }
    /// Freeze panes at the selected cell (toggles off if already frozen). Rows
    /// above and columns left of the selection stay pinned while scrolling.
    fn sheet_freeze(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            let s = v.active;
            let cur = v.sheet().freeze;
            let next = if cur != (0, 0) { (0, 0) } else { (r, c) };
            if let Some(sheet) = v.pkg.workbook.sheets.get_mut(s) {
                sheet.freeze = next;
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    // ---- find & replace (sheet) -------------------------------------------

    /// Route a keystroke to the open find bar (query / replace fields).
    fn sheet_find_key(&mut self, ev: &KeyDownEvent, shift: bool, key: &str, cx: &mut Context<Self>) {
        match key {
            "escape" => self.find_open = false,
            "enter" => return self.sheet_find_next(shift, cx),
            "backspace" => match self.find_field {
                FindField::Query => {
                    self.find_query.pop();
                }
                FindField::Replace => {
                    self.replace_text.pop();
                }
            },
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        match self.find_field {
                            FindField::Query => self.find_query.push_str(c),
                            FindField::Replace => self.replace_text.push_str(c),
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    /// Select the next (or previous) cell whose display text contains the query,
    /// wrapping around, scanning row-major from the current selection.
    fn sheet_find_next(&mut self, back: bool, cx: &mut Context<Self>) {
        let q = self.find_query.to_lowercase();
        if q.is_empty() {
            return;
        }
        if let Some(v) = self.active_sheet_mut() {
            let (mr, mc) = v.extent();
            let ncols = mc as i64 + 1;
            let total = (mr as i64 + 1) * ncols;
            let (sr, sc) = v.sel;
            let start = sr as i64 * ncols + sc as i64;
            for step in 1..=total {
                let idx = if back { (start - step).rem_euclid(total) } else { (start + step).rem_euclid(total) };
                let r = (idx / ncols) as u32;
                let c = (idx % ncols) as u32;
                let t = v.cell_text(r, c).to_lowercase();
                if !t.is_empty() && t.contains(&q) {
                    v.sel = (r, c);
                    v.anchor = (r, c);
                    v.vlist.scroll_to_reveal_item(v.row_list_index(r));
                    break;
                }
            }
        }
        cx.notify();
    }

    /// Replace the query in the current cell (if it matches), then move to the next.
    fn sheet_replace(&mut self, cx: &mut Context<Self>) {
        let q = self.find_query.clone();
        let rep = self.replace_text.clone();
        if q.is_empty() {
            return;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            let s = v.active;
            let text = v.cell_text(r, c);
            if text.to_lowercase().contains(&q.to_lowercase()) {
                let new = ci_replace(&text, &q, &rep);
                let style = v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0);
                v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), parse_cell_input(&new, style));
            }
        }
        self.mark_sheet_dirty();
        self.sheet_find_next(false, cx);
    }

    /// Replace the query in every matching cell of the sheet.
    fn sheet_replace_all(&mut self, cx: &mut Context<Self>) {
        let q = self.find_query.clone();
        let rep = self.replace_text.clone();
        if q.is_empty() {
            return;
        }
        self.sheet_snapshot();
        let mut n = 0u32;
        if let Some(v) = self.active_sheet_mut() {
            let (mr, mc) = v.extent();
            let s = v.active;
            let ql = q.to_lowercase();
            for r in 0..=mr {
                for c in 0..=mc {
                    let text = v.cell_text(r, c);
                    if !text.is_empty() && text.to_lowercase().contains(&ql) {
                        let new = ci_replace(&text, &q, &rep);
                        let style = v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0);
                        v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), parse_cell_input(&new, style));
                        n += 1;
                    }
                }
            }
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.status = format!("replaced {n}").into();
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Insert a PivotTable for the selected range (or the used range): a fresh
    /// output sheet plus a live `PivotDef` (first text column → Rows, each numeric
    /// column → Sum Values) that the field panel can then re-place.
    fn sheet_insert_pivot(&mut self, cx: &mut Context<Self>) {
        use gridcore::frame::Frame;
        use gridcore::sheet::CellValue;
        self.sheet_snapshot();
        let mut def: Option<PivotDef> = None;
        if let Some(v) = self.active_sheet() {
            let s = v.active;
            let (r0, c0, r1, c1) = if v.has_range() {
                v.range()
            } else {
                let (mr, mc) = v.extent();
                (0, 0, mr, mc)
            };
            let sh = v.sheet();
            let frame = Frame::from_range(&v.pkg.workbook, s, (r0, c0, r1, c1));
            let mut role = vec![0u8; frame.names.len()];
            let mut have_row = false;
            for (i, c) in (c0..=c1).enumerate() {
                let (mut nums, mut txts) = (0u32, 0u32);
                for r in (r0 + 1)..=r1 {
                    match sh.cell(r, c).map(|cl| &cl.value) {
                        Some(CellValue::Number(_)) => nums += 1,
                        Some(CellValue::Text(_)) => txts += 1,
                        _ => {}
                    }
                }
                if nums > 0 && nums >= txts {
                    role[i] = 3; // Values (Sum)
                } else if !have_row {
                    role[i] = 1; // Rows
                    have_row = true;
                }
            }
            if !have_row && !role.is_empty() {
                role[0] = 1;
            }
            let agg = vec![0u8; frame.names.len()];
            def = Some(PivotDef { src_sheet: s, src_range: (r0, c0, r1, c1), out_sheet: 0, names: frame.names.clone(), role, agg });
        }
        if let Some(mut d) = def {
            if let Some(v) = self.active_sheet_mut() {
                let n = v.pkg.workbook.sheets.iter().filter(|s| s.name.starts_with("Pivot")).count();
                let name = if n == 0 { "Pivot".to_string() } else { format!("Pivot{}", n + 1) };
                // add_sheet wires the OPC part + workbook entry so the sheet saves.
                d.out_sheet = v.pkg.add_sheet(&name);
                v.active = d.out_sheet;
                v.pivot_views.push(d);
                v.sel = (0, 0);
                v.anchor = (0, 0);
                v.editing = None;
            }
            let idx = self.active_sheet().map(|v| v.pivot_views.len() - 1).unwrap_or(0);
            self.recompute_pivot(idx);
            self.mark_sheet_dirty();
        }
        cx.notify();
    }

    /// (Re)compute pivot `idx` from its current field roles and write the result
    /// onto its output sheet.
    fn recompute_pivot(&mut self, idx: usize) {
        use gridcore::frame::{pivot, pivot_table_strings, Frame, Measure, PivotSpec};
        use gridcore::sheet::Cell;
        let built = self.active_sheet().and_then(|v| {
            let d = v.pivot_views.get(idx)?;
            let frame = Frame::from_range(&v.pkg.workbook, d.src_sheet, d.src_range);
            let pick = |want: u8| d.role.iter().enumerate().filter(move |(_, r)| **r == want).map(|(i, _)| i);
            let rows: Vec<usize> = pick(1).collect();
            let cols: Vec<usize> = pick(2).collect();
            let measures: Vec<Measure> = pick(3)
                .map(|i| {
                    let (agg, lbl) = PIVOT_AGGS[d.agg.get(i).copied().unwrap_or(0) as usize % PIVOT_AGGS.len()];
                    Measure { col: i, agg, name: format!("{lbl} of {}", frame.names[i]), calc: None }
                })
                .collect();
            let spec = PivotSpec { rows, cols, measures, grand_rows: true, grand_cols: true, ..Default::default() };
            let out = pivot(&frame, &spec);
            Some((pivot_table_strings(&out), out.header_rows, out.label_cols, d.out_sheet))
        });
        if let Some((strings, header_rows, label_cols, out_sheet)) = built {
            if let Some(v) = self.active_sheet_mut() {
                if let Some(sheet) = v.pkg.workbook.sheets.get_mut(out_sheet) {
                    sheet.cells.clear();
                    for (ri, row) in strings.iter().enumerate() {
                        for (ci, sv) in row.iter().enumerate() {
                            if sv.is_empty() {
                                continue;
                            }
                            let body = ri >= header_rows && ci >= label_cols;
                            let cell = match (body, sv.parse::<f64>()) {
                                (true, Ok(nf)) => Cell::number(nf),
                                _ => Cell::text(sv),
                            };
                            sheet.set_cell(ri as u32, ci as u32, cell);
                        }
                    }
                }
                v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
            }
        }
    }

    /// The pivot definition whose output sheet is currently active, if any.
    fn active_pivot(&self) -> Option<usize> {
        let v = self.active_sheet()?;
        v.pivot_views.iter().position(|d| d.out_sheet == v.active)
    }

    /// Cycle a source field's role in pivot `idx`: none → Rows → Columns → Values,
    /// then recompute.
    fn pivot_cycle_field(&mut self, idx: usize, field: usize, cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            if let Some(d) = v.pivot_views.get_mut(idx) {
                if let Some(r) = d.role.get_mut(field) {
                    *r = (*r + 1) % 4;
                }
            }
        }
        self.recompute_pivot(idx);
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Cycle a value field's aggregation (Sum → Count → Avg → Max → Min →
    /// Product) in pivot `idx`, then recompute.
    fn pivot_cycle_agg(&mut self, idx: usize, field: usize, cx: &mut Context<Self>) {
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            if let Some(d) = v.pivot_views.get_mut(idx) {
                if let Some(a) = d.agg.get_mut(field) {
                    *a = (*a + 1) % PIVOT_AGGS.len() as u8;
                }
            }
        }
        self.recompute_pivot(idx);
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Build a clustered column chart from the selected range (categories = first
    /// text column, one series per numeric column) and float it over the sheet.
    fn sheet_insert_chart(&mut self, kind: &str, cx: &mut Context<Self>) {
        use gridcore::sheet::{CellValue, ChartData, ChartSeries};
        if let Some(v) = self.active_sheet_mut() {
            let (r0, c0, r1, c1) = if v.has_range() {
                v.range()
            } else {
                let (mr, mc) = v.extent();
                (0, 0, mr, mc)
            };
            let sh = v.sheet();
            let mut cat_col: Option<u32> = None;
            let mut num_cols: Vec<u32> = Vec::new();
            for c in c0..=c1 {
                let (mut nums, mut txts) = (0u32, 0u32);
                for r in (r0 + 1)..=r1 {
                    match sh.cell(r, c).map(|cl| &cl.value) {
                        Some(CellValue::Number(_)) => nums += 1,
                        Some(CellValue::Text(_)) => txts += 1,
                        _ => {}
                    }
                }
                if nums > 0 && nums >= txts {
                    num_cols.push(c);
                } else if cat_col.is_none() {
                    cat_col = Some(c);
                }
            }
            let cat_col = cat_col.unwrap_or(c0);
            let data_rows: Vec<u32> = (r0 + 1..=r1).collect();
            let categories: Vec<String> = data_rows.iter().map(|&r| v.cell_text(r, cat_col)).collect();
            let title = v.cell_text(r0, cat_col);
            let series: Vec<ChartSeries> = num_cols
                .iter()
                .map(|&c| {
                    let name = v.cell_text(r0, c);
                    let values = data_rows
                        .iter()
                        .map(|&r| match v.sheet().cell(r, c).map(|cl| &cl.value) {
                            Some(CellValue::Number(n)) => *n,
                            _ => 0.0,
                        })
                        .collect();
                    ChartSeries { name, values }
                })
                .collect();
            if !series.is_empty() {
                let data = ChartData { title: if title.is_empty() { "Chart".into() } else { title }, kind: kind.to_string(), categories, series };
                let s = v.active;
                // Anchor the saved chart just right of the selected range.
                let from = (r0, c1 + 2);
                let to = (r0 + 16, c1 + 10);
                v.charts.push(ChartView { sheet: s, from, to, data });
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// The "PivotTable Fields" side panel: each source field with its current
    /// role badge; clicking cycles the role and recomputes.
    fn pivot_panel(&self, idx: usize, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let Some(d) = self.active_sheet().and_then(|v| v.pivot_views.get(idx)) else {
            return div().into_any_element();
        };
        let ent = cx.entity();
        let mut fields = v_flex().gap(px(2.));
        for (i, name) in d.names.iter().enumerate() {
            let role_val = d.role.get(i).copied().unwrap_or(0);
            let (badge, col) = match role_val {
                1 => ("Rows", hsla_u(BRAND)),
                2 => ("Columns", hsla_u(0x2f6fdb)),
                3 => ("\u{03A3} Values", hsla_u(0xc0705a)),
                _ => ("", pal.dim),
            };
            let ent_role = ent.clone();
            // Name + role badge: clicking cycles the field's role. Kept flex_1 so
            // the whole row width remains the role target, with the aggregation
            // chip (Values only) as a separate sibling click target beside it.
            let name_area = div()
                .id(ElementId::Name(format!("pivfield-{i}").into()))
                .flex().flex_1().items_center().justify_between().gap_2()
                .px_2().py(px(3.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|dd| dd.bg(pal.hover))
                .child(div().text_size(px(12.)).text_color(pal.fg).overflow_hidden().child(SharedString::from(name.clone())))
                .when(!badge.is_empty(), |dd| {
                    dd.child(div().px_1p5().py(px(1.)).rounded(px(3.)).text_size(px(10.)).text_color(hsla_u(0xffffff)).bg(col).child(badge))
                })
                .on_click(move |_ev, _w, cx| {
                    ent_role.update(cx, |this, cx| this.pivot_cycle_field(idx, i, cx));
                });
            let mut row = h_flex().items_center().gap_1().child(name_area);
            if role_val == 3 {
                let agg_lbl = PIVOT_AGGS[d.agg.get(i).copied().unwrap_or(0) as usize % PIVOT_AGGS.len()].1;
                let ent_agg = ent.clone();
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("pivagg-{i}").into()))
                        .flex_none()
                        .px_1p5().py(px(1.))
                        .rounded(px(3.))
                        .text_size(px(10.))
                        .text_color(pal.fg)
                        .bg(pal.hover)
                        .border_1().border_color(pal.border)
                        .cursor_pointer()
                        .hover(|dd| dd.bg(pal.panel))
                        .child(SharedString::from(agg_lbl))
                        .on_click(move |_ev, _w, cx| {
                            ent_agg.update(cx, |this, cx| this.pivot_cycle_agg(idx, i, cx));
                        }),
                );
            }
            fields = fields.child(row);
        }
        v_flex()
            .w(px(232.))
            .h_full()
            .flex_none()
            .bg(pal.panel)
            .border_l_1()
            .border_color(pal.border)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).child("PivotTable Fields"))
            .child(div().px_3().pb_1().text_size(px(10.)).text_color(pal.dim).child("Field: Rows \u{2192} Columns \u{2192} \u{03A3} Values \u{2192} off. On a Values field, click the chip to change Sum/Count/Avg/Max/Min."))
            .child(div().id("pivot-fields").flex_1().min_h(px(0.)).overflow_y_scroll().px_2().child(fields))
            .into_any_element()
    }

    /// Dispatch a spreadsheet ribbon command.
    fn run_sheet_act(&mut self, act: SheetAct, window: &mut Window, cx: &mut Context<Self>) {
        use gridcore::sheet::Align;
        match act {
            SheetAct::Cut => self.sheet_copy(true, cx),
            SheetAct::Copy => self.sheet_copy(false, cx),
            SheetAct::Paste => self.sheet_paste(cx),
            SheetAct::Bold => self.sheet_toggle_bold(cx),
            SheetAct::Italic => self.sheet_toggle_italic(cx),
            SheetAct::AlignL => self.sheet_align(Align::Left, cx),
            SheetAct::AlignC => self.sheet_align(Align::Center, cx),
            SheetAct::AlignR => self.sheet_align(Align::Right, cx),
            SheetAct::GrowFont => self.sheet_font_step(1.0, cx),
            SheetAct::ShrinkFont => self.sheet_font_step(-1.0, cx),
            SheetAct::Percent => self.sheet_numfmt("0.00%", cx),
            SheetAct::Currency => self.sheet_numfmt("$#,##0.00", cx),
            SheetAct::Comma => self.sheet_numfmt("#,##0.00", cx),
            SheetAct::InsertPivot => self.sheet_insert_pivot(cx),
            SheetAct::InsertChart(kind) => self.sheet_insert_chart(kind, cx),
            SheetAct::FillColor => {
                self.sheet_pick = Some(SheetPick::Fill);
                cx.notify();
            }
            SheetAct::FontColor => {
                self.sheet_pick = Some(SheetPick::Font);
                cx.notify();
            }
            SheetAct::ToggleBorder => self.sheet_toggle_border(cx),
            SheetAct::FreezePanes => self.sheet_freeze(cx),
            SheetAct::NewComment => self.sheet_new_comment(cx),
            SheetAct::DeleteComment => self.sheet_delete_comment(cx),
            SheetAct::PrevComment => self.sheet_comment_nav(false, cx),
            SheetAct::NextComment => self.sheet_comment_nav(true, cx),
            SheetAct::InsertRow => self.sheet_structural(StructOp::InsertRow, cx),
            SheetAct::DeleteRow => self.sheet_structural(StructOp::DeleteRow, cx),
            SheetAct::InsertCol => self.sheet_structural(StructOp::InsertCol, cx),
            SheetAct::DeleteCol => self.sheet_structural(StructOp::DeleteCol, cx),
            SheetAct::SortAsc => self.sheet_sort(true, cx),
            SheetAct::SortDesc => self.sheet_sort(false, cx),
            SheetAct::CustomSort => {
                self.sheet_sort_edit = Some(String::new());
                cx.notify();
            }
            SheetAct::AutoSum => self.sheet_autosum(cx),
            SheetAct::FormatCells => {
                self.sheet_fmt_open = true;
                cx.notify();
            }
            SheetAct::Merge => self.sheet_merge_toggle(cx),
            SheetAct::WrapText => self.sheet_toggle_wrap(cx),
            SheetAct::RowHeight => {
                self.sheet_rowh_edit = Some(String::new());
                cx.notify();
            }
            SheetAct::CondFormat => {
                self.sheet_cf_edit = Some(String::new());
                cx.notify();
            }
            SheetAct::DataValidation => {
                self.sheet_dv_edit = Some(String::new());
                cx.notify();
            }
            SheetAct::Filter => {
                self.sheet_filter_edit = Some(String::new());
                cx.notify();
            }
            SheetAct::RemoveDuplicates => self.sheet_remove_duplicates(cx),
            SheetAct::FormatAsTable => self.sheet_format_as_table(cx),
            SheetAct::ProtectSheet => self.sheet_toggle_protection(cx),
            SheetAct::Subtotal => self.sheet_subtotal(cx),
            SheetAct::Outline => self.sheet_toggle_outline(cx),
            SheetAct::TextToColumns => {
                self.sheet_ttc_edit = Some(String::new());
                cx.notify();
            }
            SheetAct::Todo => {}
        }
        self.refocus(window, cx);
    }

    /// Route a keystroke to the spreadsheet grid (called from `on_key` when the
    /// active surface is a sheet).
    fn sheet_key(&mut self, ev: &KeyDownEvent, ctrl: bool, shift: bool, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        // An inline sheet-tab rename swallows all typing until Enter/Esc.
        if self.sheet_rename.is_some() {
            return self.sheet_rename_key(ev, key, cx);
        }
        // The comment entry bar swallows typing until Enter (commit) / Esc.
        if self.sheet_comment_edit.is_some() {
            return self.sheet_comment_key(ev, key, cx);
        }
        // The conditional-format entry bar likewise swallows typing.
        if self.sheet_cf_edit.is_some() {
            return self.sheet_cf_key(ev, key, cx);
        }
        // The data-validation entry bar swallows typing too.
        if self.sheet_dv_edit.is_some() {
            return self.sheet_dv_edit_key(ev, key, cx);
        }
        // The AutoFilter criteria bar swallows typing too.
        if self.sheet_filter_edit.is_some() {
            return self.sheet_filter_key(ev, key, cx);
        }
        // The Text-to-Columns delimiter bar swallows typing too.
        if self.sheet_ttc_edit.is_some() {
            return self.sheet_ttc_key(ev, key, cx);
        }
        // The multi-level sort spec bar swallows typing too.
        if self.sheet_sort_edit.is_some() {
            return self.sheet_sort_key(ev, key, cx);
        }
        // The row-height entry bar swallows typing too.
        if self.sheet_rowh_edit.is_some() {
            return self.sheet_rowh_key(ev, key, cx);
        }
        // While the find bar is open, keystrokes edit it (Ctrl+S/F still work).
        if self.find_open && !(ctrl && matches!(key, "s")) {
            if ctrl && key == "f" {
                self.find_open = false;
                cx.notify();
                return;
            }
            return self.sheet_find_key(ev, shift, key, cx);
        }
        let editing = self.active_sheet().is_some_and(|v| v.editing.is_some());
        if ctrl {
            match key {
                "s" => self.save_active(window, cx),
                "f" => {
                    self.find_open = true;
                    self.find_field = FindField::Query;
                    cx.notify();
                }
                "c" => self.sheet_copy(false, cx),
                "x" => self.sheet_copy(true, cx),
                "v" => self.sheet_paste(cx),
                "z" => self.sheet_undo(cx),
                "y" => self.sheet_redo(cx),
                "b" => self.sheet_toggle_bold(cx),
                "i" => self.sheet_toggle_italic(cx),
                // Insert a PivotTable for the selection (also on the Insert ribbon).
                "p" if shift => self.sheet_insert_pivot(cx),
                // Insert a chart of the selection (also on the Insert ribbon).
                "k" if shift => self.sheet_insert_chart("column", cx),
                "a" => {
                    // Select the whole used range.
                    if let Some(v) = self.active_sheet_mut() {
                        let (mr, mc) = v.extent();
                        v.sel = (0, 0);
                        v.anchor = (mr, mc);
                        v.editing = None;
                    }
                    cx.notify();
                }
                "f1" => {
                    self.ribbon_min = !self.ribbon_min;
                    cx.notify();
                }
                _ => {}
            }
            return;
        }
        match key {
            "escape" => {
                self.sheet_pick = None;
                if let Some(v) = self.active_sheet_mut() {
                    v.editing = None;
                }
                cx.notify();
            }
            "enter" => self.sheet_commit(if shift { -1 } else { 1 }, 0, cx),
            "f2" => self.sheet_begin_edit(None, cx),
            "backspace" => {
                if editing {
                    if let Some(v) = self.active_sheet_mut() {
                        v.edit_backspace();
                    }
                    cx.notify();
                } else {
                    self.sheet_clear(cx);
                }
            }
            "delete" => {
                if editing {
                    if let Some(v) = self.active_sheet_mut() {
                        v.edit_delete();
                    }
                    cx.notify();
                } else {
                    self.sheet_clear(cx);
                }
            }
            // While editing, Left/Right/Home/End move the caret WITHIN the cell
            // (Excel's edit mode) instead of switching cells.
            "left" if editing => {
                if let Some(v) = self.active_sheet_mut() {
                    v.edit_move(-1);
                }
                cx.notify();
            }
            "right" if editing => {
                if let Some(v) = self.active_sheet_mut() {
                    v.edit_move(1);
                }
                cx.notify();
            }
            "home" if editing => {
                if let Some(v) = self.active_sheet_mut() {
                    v.edit_caret = 0;
                }
                cx.notify();
            }
            "end" if editing => {
                if let Some(v) = self.active_sheet_mut() {
                    v.edit_caret_to_end();
                }
                cx.notify();
            }
            // Not editing: arrows move / extend the selection. Up/Down while
            // editing still commit and move (single-line cells).
            "left" if shift => self.sheet_extend(0, -1, editing, cx),
            "right" if shift => self.sheet_extend(0, 1, editing, cx),
            "up" if shift => self.sheet_extend(-1, 0, editing, cx),
            "down" if shift => self.sheet_extend(1, 0, editing, cx),
            "left" => self.sheet_nav(0, -1, editing, cx),
            "right" => self.sheet_nav(0, 1, editing, cx),
            "up" => self.sheet_nav(-1, 0, editing, cx),
            "down" => self.sheet_nav(1, 0, editing, cx),
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        let protected = self.sheet_protected();
                        if let Some(v) = self.active_sheet_mut() {
                            if v.editing.is_some() {
                                v.edit_insert(c);
                            } else if !protected {
                                // Start a fresh edit with the typed char.
                                v.editing = Some(String::new());
                                v.edit_caret = 0;
                                v.edit_insert(c);
                            }
                        }
                        cx.notify();
                    }
                }
            }
        }
    }

    fn select_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.persist();
            self.refocus(window, cx);
        }
    }

    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if i < self.active {
            self.active -= 1;
        }
        self.persist();
        self.refocus(window, cx);
    }

    fn active_editor(&mut self) -> Option<&mut Editor> {
        match self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            Some(Surface::Doc(ed)) => Some(ed),
            _ => None,
        }
    }

    /// The editor keystrokes/clicks currently drive: the header/footer editor when
    /// in HF edit mode, otherwise the document body. All editing routes through
    /// this so the same machinery serves both surfaces.
    fn edit_target(&mut self) -> Option<&mut Editor> {
        let tab = self.tabs.get_mut(self.active)?;
        if let Some(hf) = tab.hf_edit.as_mut() {
            return Some(&mut hf.editor);
        }
        match &mut tab.surface {
            Surface::Doc(ed) => Some(ed),
            _ => None,
        }
    }

    /// Whether the active tab is currently in header/footer edit mode.
    fn hf_active(&self) -> bool {
        self.tabs.get(self.active).is_some_and(|t| t.hf_edit.is_some())
    }

    /// Enter header (or footer) edit mode: resolve the existing part or create a
    /// fresh one, parse its blocks into an editor, and switch to Print Layout so
    /// the margin area is visible. No-op for markdown/package-less tabs.
    fn enter_hf(&mut self, is_header: bool, variant: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf(); // commit any header/footer already open
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        if !matches!(tab.surface, Surface::Doc(_)) {
            return;
        }
        let Some(pkg) = tab.pkg.as_mut() else {
            tab.status = "Headers/footers need a .docx (not a Markdown document)".into();
            return self.refocus(window, cx);
        };
        let part_name = match hf_part_name_typed(pkg, is_header, variant) {
            Some(n) => n,
            None => match pkg.create_hf(is_header, variant) {
                Some(n) => {
                    tab.dirty = true;
                    n
                }
                None => {
                    tab.status = "Could not create the header/footer part".into();
                    return self.refocus(window, cx);
                }
            },
        };
        let blocks = parse_hf_part(pkg, &part_name);
        let doc = docxcore::model::Document { body: blocks };
        tab.hf_edit = Some(HfEdit { editor: Editor::new(doc), part_name, is_header, variant });
        self.page_view = true;
        if let Some(t) = self.tabs.get_mut(idx) {
            let region = if is_header { "header" } else { "footer" };
            let vlabel = match variant { "first" => "first-page ", "even" => "even-page ", _ => "" };
            t.status = format!("Editing {vlabel}{region} — press Esc to return to the document").into();
        }
        self.refocus(window, cx);
    }

    /// Serialize the open header/footer editor back into its package part (called
    /// on exit and before every save) so edits persist. Leaves the session open.
    fn flush_hf(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(hf) = tab.hf_edit.as_ref() else { return };
        let inner = docxcore::serialize::blocks_to_xml(&hf.editor.doc.body);
        let tag = if hf.is_header { "w:hdr" } else { "w:ftr" };
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <{tag} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\">{inner}</{tag}>"
        );
        let part_name = hf.part_name.clone();
        if let Some(pkg) = tab.pkg.as_mut() {
            pkg.set_part(&part_name, xml.into_bytes());
        }
        tab.dirty = true;
    }

    /// Leave header/footer edit mode, committing edits to the package part.
    fn exit_hf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.hf_edit = None;
            tab.status = "Closed header/footer".into();
        }
        self.refocus(window, cx);
    }

    /// Toggle "Different First Page" (`<w:titlePg/>`); off while editing the
    /// first-page variant drops the edit session back to the default variant.
    fn toggle_title_pg(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf();
        let idx = self.active;
        let mut on = false;
        if let Some(tab) = self.tabs.get_mut(idx) {
            if let Some(pkg) = tab.pkg.as_mut() {
                on = !pkg.has_title_pg();
                pkg.set_title_pg(on);
                tab.dirty = true;
            }
        }
        if !on {
            if let Some((is_h, "first")) = self.tabs.get(idx).and_then(|t| t.hf_edit.as_ref()).map(|h| (h.is_header, h.variant)) {
                return self.enter_hf(is_h, "default", window, cx);
            }
        }
        self.refocus(window, cx);
    }

    /// Toggle "Different Odd & Even Pages" (`<w:evenAndOddHeaders/>`); off while
    /// editing the even variant drops back to the default variant.
    fn toggle_even_odd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf();
        let idx = self.active;
        let mut on = false;
        if let Some(tab) = self.tabs.get_mut(idx) {
            if let Some(pkg) = tab.pkg.as_mut() {
                on = !pkg.has_even_odd();
                pkg.set_even_odd(on);
                tab.dirty = true;
            }
        }
        if !on {
            if let Some((is_h, "even")) = self.tabs.get(idx).and_then(|t| t.hf_edit.as_ref()).map(|h| (h.is_header, h.variant)) {
                return self.enter_hf(is_h, "default", window, cx);
            }
        }
        self.refocus(window, cx);
    }

    /// The contextual Header & Footer toolbar, shown while editing one. Lets the
    /// user switch region (header/footer) and variant (default/first/even) and
    /// toggle the "Different First Page" / "Different Odd & Even" section options.
    fn hf_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.tabs.get(self.active);
        let hf = tab.and_then(|t| t.hf_edit.as_ref());
        let (is_header, variant) = hf.map(|h| (h.is_header, h.variant)).unwrap_or((true, "default"));
        let title_pg = tab.and_then(|t| t.pkg.as_ref()).is_some_and(|p| p.has_title_pg());
        let even_odd = tab.and_then(|t| t.pkg.as_ref()).is_some_and(|p| p.has_even_odd());
        let pill = move |id: &'static str, label: SharedString, active: bool| {
            div()
                .id(id)
                .px_2()
                .h(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .text_size(px(12.))
                .text_color(if active { hsla_u(BRAND) } else { pal.fg })
                .border_1()
                .border_color(if active { hsla_u(BRAND) } else { pal.border })
                .cursor_pointer()
                .hover(|d| d.bg(pal.hover))
                .child(label)
        };
        let check = move |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .hover(|d| d.text_color(hsla_u(BRAND)))
                .child(
                    div()
                        .size(px(13.))
                        .rounded(px(2.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(if on { hsla_u(BRAND) } else { pal.border })
                        .bg(if on { hsla_u(BRAND) } else { Hsla { a: 0., ..pal.fg } })
                        .when(on, |d| d.child(div().text_size(px(9.)).text_color(rgb(0xffffff)).child("\u{2713}"))),
                )
                .child(label)
        };
        let sep = || div().w(px(1.)).h(px(16.)).bg(pal.border);
        h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(96.)).child("Header & Footer"))
            .child(pill("hf-hdr", "Header".into(), is_header).on_click(cx.listener(move |t, _, w, c| t.enter_hf(true, variant, w, c))))
            .child(pill("hf-ftr", "Footer".into(), !is_header).on_click(cx.listener(move |t, _, w, c| t.enter_hf(false, variant, w, c))))
            .child(sep())
            .child(pill("hf-def", "Default".into(), variant == "default").on_click(cx.listener(move |t, _, w, c| t.enter_hf(is_header, "default", w, c))))
            .when(title_pg, |d| d.child(pill("hf-first", "First page".into(), variant == "first").on_click(cx.listener(move |t, _, w, c| t.enter_hf(is_header, "first", w, c)))))
            .when(even_odd, |d| d.child(pill("hf-even", "Even".into(), variant == "even").on_click(cx.listener(move |t, _, w, c| t.enter_hf(is_header, "even", w, c)))))
            .child(sep())
            .child(check("hf-tp", "Different First Page", title_pg).on_click(cx.listener(|t, _, w, c| t.toggle_title_pg(w, c))))
            .child(check("hf-eo", "Different Odd & Even", even_odd).on_click(cx.listener(|t, _, w, c| t.toggle_even_odd(w, c))))
            .child(div().flex_1())
            .child(pill("hf-close", "Close".into(), false).on_click(cx.listener(|t, _, w, c| t.exit_hf(w, c))))
            .into_any_element()
    }

    fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A spreadsheet tab: commit any open cell edit, then write .xlsx.
        if self.active_is_sheet() {
            if self.active_sheet().is_some_and(|v| v.editing.is_some()) {
                self.sheet_commit(0, 0, cx);
            }
            return self.save_sheet(window, cx);
        }
        self.flush_hf(); // commit any open header/footer edits into the package first
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Surface::Doc(editor) = &tab.surface else { return };
        // Markdown-backed tabs save as Markdown; everything else as lossless .docx.
        let bytes = if tab.markdown {
            docxcore::markdown::to_markdown(&editor.doc).into_bytes()
        } else {
            doc_to_docx(&editor.doc, &tab.comments, tab.pkg.as_ref())
        };
        let path = tab
            .path
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(tab.title.to_string()));
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                tab.title = file_name(&path).into();
                tab.path = Some(path.clone());
                tab.dirty = false;
                tab.status = format!("saved {} bytes → {}", bytes.len(), path.display()).into();
            }
            Err(e) => tab.status = format!("save failed: {e}").into(),
        }
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    /// Serialize the active spreadsheet back to `.xlsx` (lossless — save_xlsx
    /// re-writes into the loaded package), preserving styles and formulas.
    fn save_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Serialize first (borrows the sheet), then decide the path so a native
        // Save-As dialog for a new workbook doesn't clash with the borrow.
        let (bytes, existing_path, title) = {
            let Some(tab) = self.tabs.get(self.active) else { return };
            let Surface::Sheet(v) = &tab.surface else { return };
            (sheet_bytes(v), tab.path.clone(), tab.title.to_string())
        };
        // A never-saved workbook asks where to go (Excel-style), instead of
        // silently dumping into the working directory.
        let path = match existing_path {
            Some(p) => p,
            None => match rfd::FileDialog::new().add_filter("Excel workbook", &["xlsx"]).set_file_name(title).save_file() {
                Some(p) => p,
                None => {
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        tab.status = "save cancelled".into();
                    }
                    return self.refocus(window, cx);
                }
            },
        };
        let written = std::fs::write(&path, &bytes);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            match written {
                Ok(()) => {
                    tab.title = file_name(&path).into();
                    tab.path = Some(path.clone());
                    tab.dirty = false;
                    tab.status = format!("saved {} bytes → {}", bytes.len(), path.display()).into();
                }
                Err(e) => tab.status = format!("save failed: {e}").into(),
            }
        }
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.tabs.get(self.active).map(|t| t.title.to_string()).unwrap_or_else(|| "Untitled.docx".into());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Word document", &["docx"])
            .add_filter("Markdown", &["md", "markdown"])
            .set_file_name(start)
            .save_file()
        {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                // Choosing a .md name switches the tab to Markdown, and vice-versa.
                tab.markdown = is_markdown_path(&path);
                tab.path = Some(path);
            }
            self.save_active(window, cx);
        } else {
            self.refocus(window, cx);
        }
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("All supported", &["docx", "md", "markdown", "xlsx"])
            .add_filter("Word or Markdown", &["docx", "md", "markdown"])
            .add_filter("Excel workbook", &["xlsx"])
            .pick_file()
        {
            self.tabs.push(tab_from_path(&path));
            self.active = self.tabs.len() - 1;
        }
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    /// Open files passed on the command line (e.g. double-clicking a .docx/.xlsx
    /// in Explorer) on top of the restored session. A file already open is
    /// focused rather than duplicated; if that tab has unsaved changes, ask
    /// before reloading it from disk.
    fn open_args(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let mut changed = false;
        for path in paths {
            let key = canon(&path);
            match self.tabs.iter().position(|t| t.path.as_deref().map(canon) == Some(key.clone())) {
                Some(i) => {
                    if self.tabs[i].dirty {
                        let reload = matches!(
                            rfd::MessageDialog::new()
                                .set_title("docxy")
                                .set_description(format!(
                                    "\"{}\" is already open with unsaved changes.\n\nReload it from disk? Your unsaved changes will be lost.\nChoose No to keep your current version.",
                                    file_name(&path)
                                ))
                                .set_buttons(rfd::MessageButtons::YesNo)
                                .show(),
                            rfd::MessageDialogResult::Yes
                        );
                        if reload {
                            self.tabs[i] = tab_from_path(&path);
                        }
                    }
                    self.active = i;
                }
                None => {
                    self.tabs.push(tab_from_path(&path));
                    self.active = self.tabs.len() - 1;
                }
            }
            changed = true;
        }
        if changed {
            self.backstage = false;
            self.persist();
            cx.notify();
        }
    }

    /// Scroll the document so the caret's top-level block is in view (keyboard
    /// navigation/typing in a long document shouldn't let the caret drift off).
    fn scroll_to_caret(&self) {
        if let Some(t) = self.tabs.get(self.active) {
            if let Surface::Doc(ed) = &t.surface {
                if let Some(&b) = ed.caret.path.first() {
                    self.doc_scroll.scroll_to_item(b);
                }
            }
        }
    }

    /// Place the caret at an explicit paragraph path + char offset (used by
    /// click-to-caret). With `extend` (Shift-click) it keeps/starts an anchor so
    /// the click extends the selection; otherwise it collapses any selection.
    fn set_caret(&mut self, path: Vec<usize>, offset: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = self.edit_target() {
            if extend {
                ed.extend_selection(true); // anchor at the current caret if none, else keep it
            } else {
                ed.clear_selection();
            }
            ed.caret = Caret::at(path, offset);
            ed.clamp();
        }
        self.focus.focus(window, cx);
        self.focused = true;
        cx.notify();
    }

    /// Start a mouse drag-selection at a click. Without Shift it plants a fresh
    /// anchor at the click; with Shift it extends the existing selection.
    fn begin_select(&mut self, path: Vec<usize>, offset: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.mini_bar = None;
        self.context_menu = None;
        if let Some(ed) = self.edit_target() {
            if extend {
                ed.extend_selection(true); // anchor at the current caret if none
            } else {
                ed.clear_selection();
            }
            ed.caret = Caret::at(path, offset);
            if !extend {
                ed.extend_selection(true); // plant the anchor here so a drag extends from it
            }
            ed.clamp();
        }
        self.selecting = true;
        self.focus.focus(window, cx);
        self.focused = true;
        cx.notify();
    }

    /// Extend the drag-selection to the character under the cursor (anchor stays).
    fn extend_select(&mut self, path: Vec<usize>, offset: usize, cx: &mut Context<Self>) {
        if let Some(ed) = self.edit_target() {
            ed.caret = Caret::at(path, offset);
            ed.clamp();
        }
        cx.notify();
    }

    fn with_editor(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            // Route to the header/footer editor while it's open, else the body.
            if let Some(hf) = tab.hf_edit.as_mut() {
                f(&mut hf.editor);
                tab.dirty = true;
            } else if let Surface::Doc(ed) = &mut tab.surface {
                f(ed);
                tab.dirty = true;
            }
        }
        self.refocus(window, cx);
    }

    fn do_copy(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut dirty = false;
        if let Some(ed) = self.edit_target() {
            let c = if cut { dirty = true; ed.cut() } else { ed.copy() };
            if c.is_some() {
                self.clip = c;
            }
        }
        if dirty {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
            }
        }
        self.refocus(window, cx);
    }

    fn do_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(clip) = self.clip.clone() {
            self.with_editor(window, cx, |e| e.paste(&clip));
        } else {
            self.refocus(window, cx);
        }
    }

    // ---- find & replace ----------------------------------------------------

    /// Open the find bar (focused on the query field) or close it.
    fn toggle_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = !self.find_open;
        if self.find_open {
            self.find_field = FindField::Query;
            // Seed from the current selection, if any, for a quick "find selected".
            if let Some(ed) = self.active_editor() {
                let sel = ed.selection_text();
                if !sel.is_empty() && !sel.contains('\n') {
                    self.find_query = sel;
                }
            }
            self.find_step(false, false, cx); // highlight the first match
            self.refocus(window, cx);
        } else {
            self.refocus(window, cx);
        }
    }

    /// Number of matches for the current query in the active document.
    fn match_count(&self) -> usize {
        if self.find_query.is_empty() {
            return 0;
        }
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => ed.find_all(&self.find_query, self.find_case).len(),
            _ => 0,
        }
    }

    /// Move to the next/previous match and select it (so it highlights).
    /// `from_start` restarts the search from the document top (used as-you-type).
    fn find_step(&mut self, reverse: bool, from_start: bool, cx: &mut Context<Self>) {
        let q = self.find_query.clone();
        let cs = self.find_case;
        if let Some(ed) = self.active_editor() {
            if from_start {
                ed.move_doc_start();
                ed.clear_selection();
            }
            if let Some(m) = ed.find_next(&q, cs, reverse) {
                ed.select_match(&m);
            }
        }
        cx.notify();
    }

    /// Replace the current match (if one is selected) and advance to the next.
    fn replace_one(&mut self, cx: &mut Context<Self>) {
        let with = self.replace_text.clone();
        let q = self.find_query.clone();
        let cs = self.find_case;
        let mut changed = false;
        if let Some(ed) = self.active_editor() {
            if ed.has_selection() {
                ed.replace_current_with(&with);
                changed = true;
            }
            if let Some(m) = ed.find_next(&q, cs, false) {
                ed.select_match(&m);
            }
        }
        if changed {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
            }
        }
        cx.notify();
    }

    /// Replace every match; report the count in the status line.
    fn replace_all_now(&mut self, cx: &mut Context<Self>) {
        let with = self.replace_text.clone();
        let q = self.find_query.clone();
        let cs = self.find_case;
        let mut n = 0;
        if let Some(ed) = self.active_editor() {
            n = ed.replace_all(&q, &with, cs);
        }
        if n > 0 {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
                t.status = format!("replaced {n}").into();
            }
        }
        cx.notify();
    }

    // ---- font colour / highlight pickers -----------------------------------

    fn toggle_picker(&mut self, kind: PickKind, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = if self.picker == Some(kind) { None } else { Some(kind) };
        self.refocus(window, cx);
    }

    fn apply_color(&mut self, hex: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_color(hex));
    }

    fn apply_highlight(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_highlight(name));
    }

    fn apply_font(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_font(&name));
    }

    fn apply_size(&mut self, pts: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_font_size(pts * 2));
    }

    /// Field evaluation context: the current clock, author and file name.
    fn field_context(&self) -> docxcore::field::FieldContext {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| docxcore::field::civil_from_unix(d.as_secs() as i64));
        let filename = self.tabs.get(self.active).map(|t| t.title.to_string()).unwrap_or_default();
        let mut props = docxcore::field::DocProps::default();
        props.author = "docxy".to_string();
        docxcore::field::FieldContext { now, props, filename }
    }

    /// Insert a field (`<w:fldSimple>`) with its computed value at the caret.
    fn insert_field(&mut self, instr: &'static str, fallback: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let ctx = self.field_context();
        let val = docxcore::field::eval_field_ctx(instr, &ctx).unwrap_or_else(|| fallback.to_string());
        let raw = format!("<w:fldSimple w:instr=\"{}\"><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:fldSimple>", xml_escape(instr), xml_escape(&val));
        self.with_editor(window, cx, |e| e.paste(&Clip { paras: vec![vec![Inline::Field { raw, text: val }]] }));
    }

    fn insert_page_break(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_editor(window, cx, |e| e.paste(&Clip { paras: vec![vec![Inline::Break(docxcore::model::BreakKind::Page)]] }));
    }

    /// Insert one symbol/special character at the caret (Insert ▸ Symbol).
    fn insert_symbol(&mut self, s: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let s = s.to_string();
        self.with_editor(window, cx, move |e| e.insert_str(&s));
    }

    /// Insert an inline math equation from a LaTeX template (Insert ▸ Equation).
    fn insert_equation(&mut self, latex: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, move |e| e.insert_equation(latex, false));
    }

    /// Cycle the section's newspaper columns 1 → 2 → 3 → 1 (Layout ▸ Columns).
    /// docxy renders a single column, but the layout round-trips and Word lays it
    /// out in columns.
    fn cycle_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Some(pkg) = tab.pkg.as_mut() {
                let next = match pkg.columns() {
                    1 => 2,
                    2 => 3,
                    _ => 1,
                };
                pkg.set_columns(next);
                tab.dirty = true;
                tab.status = format!("Columns: {next}").into();
            } else {
                tab.status = "Columns need a .docx (not Markdown)".into();
            }
        }
        self.refocus(window, cx);
    }

    /// Toggle automatic hyphenation for the document (Layout ▸ Hyphenation).
    fn toggle_hyphenation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Some(pkg) = tab.pkg.as_mut() {
                let on = !pkg.has_auto_hyphenation();
                pkg.set_auto_hyphenation(on);
                tab.dirty = true;
                tab.status = if on { "Automatic hyphenation: on".into() } else { "Automatic hyphenation: off".into() };
            } else {
                tab.status = "Hyphenation needs a .docx (not Markdown)".into();
            }
        }
        self.refocus(window, cx);
    }

    /// Apply an auto-rule line spacing to the selected paragraphs (Line Spacing menu).
    fn apply_line_spacing(&mut self, line: i32, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, move |e| e.set_line_spacing(line, "auto"));
    }
    /// Toggle 12pt of space before / after the selected paragraphs.
    fn toggle_space_before(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| {
            let on = e.caret_space_before().unwrap_or(0) > 0;
            e.set_space_before(if on { None } else { Some(240) });
        });
    }
    fn toggle_space_after(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| {
            let on = e.caret_space_after().unwrap_or(0) > 0;
            e.set_space_after(if on { None } else { Some(240) });
        });
    }

    /// If the caret is inside a table, the (block index, row, cell).
    fn caret_table(&self) -> Option<(usize, usize, usize)> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => {
                let p = &ed.caret.path;
                (p.len() >= 3 && matches!(ed.doc.body.get(p[0]), Some(Block::Table(_)))).then(|| (p[0], p[1], p[2]))
            }
            _ => None,
        }
    }

    /// An empty table cell (one blank paragraph).
    fn empty_cell() -> docxcore::model::Cell {
        docxcore::model::Cell { grid_span: 1, v_merge: docxcore::model::VMerge::None, blocks: vec![Block::Paragraph(Paragraph::default())], raw_tcpr: None }
    }

    /// Run a Table Tools operation relative to the caret's cell.
    fn table_op(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        use Act::*;
        let Some((tb, row, col)) = self.caret_table() else { return self.refocus(window, cx) };
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                if let Some(Block::Table(table)) = ed.doc.body.get_mut(tb) {
                    let ncols = table.grid.len().max(table.rows.first().map_or(0, |r| r.cells.len()));
                    match act {
                        RowAbove | RowBelow => {
                            let at = if matches!(act, RowAbove) { row } else { row + 1 };
                            let new = docxcore::model::Row { cells: (0..ncols).map(|_| Self::empty_cell()).collect(), raw_props: vec![] };
                            table.rows.insert(at.min(table.rows.len()), new);
                            ed.caret = Caret::at(vec![tb, at.min(table.rows.len() - 1), col.min(ncols - 1), 0], 0);
                        }
                        ColLeft | ColRight => {
                            let at = if matches!(act, ColLeft) { col } else { col + 1 };
                            for r in &mut table.rows {
                                r.cells.insert(at.min(r.cells.len()), Self::empty_cell());
                            }
                            let w = table.grid.first().copied().unwrap_or(2340);
                            table.grid.insert(at.min(table.grid.len()), w);
                            ed.caret = Caret::at(vec![tb, row, at, 0], 0);
                        }
                        DelRow => {
                            if table.rows.len() > 1 {
                                table.rows.remove(row);
                                let nr = table.rows.len();
                                ed.caret = Caret::at(vec![tb, row.min(nr - 1), col.min(ncols - 1), 0], 0);
                            }
                        }
                        DelCol => {
                            if ncols > 1 {
                                for r in &mut table.rows {
                                    if col < r.cells.len() {
                                        r.cells.remove(col);
                                    }
                                }
                                if col < table.grid.len() {
                                    table.grid.remove(col);
                                }
                                ed.caret = Caret::at(vec![tb, row, col.min(ncols - 2), 0], 0);
                            }
                        }
                        DelTable => {
                            ed.doc.body.remove(tb);
                            let at = tb.min(ed.doc.body.len().saturating_sub(1));
                            ed.caret = Caret::at(vec![at], 0);
                        }
                        _ => {}
                    }
                    ed.clear_selection();
                    ed.clamp();
                }
            }
            t.dirty = true;
        }
        self.refocus(window, cx);
    }

    /// Insert an empty `rows`×`cols` bordered table after the caret's block, and
    /// move the caret into its first cell.
    fn insert_table(&mut self, rows: usize, cols: usize, window: &mut Window, cx: &mut Context<Self>) {
        use docxcore::model::{Cell, Row, Table, VMerge};
        self.picker = None;
        const TBLPR: &str = "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>\
<w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
</w:tblBorders></w:tblPr>";
        let col_w = (9360 / cols.max(1)) as u32;
        let mk_cell = || Cell { grid_span: 1, v_merge: VMerge::None, blocks: vec![Block::Paragraph(Paragraph::default())], raw_tcpr: None };
        let mk_row = || Row { cells: (0..cols).map(|_| mk_cell()).collect(), raw_props: vec![] };
        let table = Table { grid: vec![col_w; cols], rows: (0..rows).map(|_| mk_row()).collect(), raw_tblpr: Some(TBLPR.to_string()) };
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                let at = ed.caret.path.first().copied().unwrap_or(0).min(ed.doc.body.len().saturating_sub(1));
                let pos = (at + 1).min(ed.doc.body.len());
                ed.doc.body.insert(pos, Block::Table(table));
                ed.clear_selection();
                ed.caret = Caret::at(vec![pos, 0, 0, 0], 0);
                ed.clamp();
            }
            t.dirty = true;
        }
        self.refocus(window, cx);
    }

    /// The swatch strip shown under the ribbon while a picker is open.
    fn picker_bar(&self, kind: PickKind, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let swatch = |bg: Hsla, ring: bool| {
            div().size(px(20.)).rounded(px(3.)).border_1().border_color(if ring { pal.fg } else { pal.border }).bg(bg).cursor_pointer().hover(|d| d.border_color(hsla_u(BRAND)))
        };
        let mut row = h_flex().w_full().items_center().flex_wrap().gap_1p5().px_3().py_1().bg(pal.panel).border_b_1().border_color(pal.border);
        row = row.child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(78.)).child(match kind {
            PickKind::Color => "Font colour",
            PickKind::Highlight => "Highlight",
            PickKind::FontName => "Font",
            PickKind::FontSize => "Size",
            PickKind::Field => "Field",
            PickKind::Table => "Table",
            PickKind::Symbol => "Symbol",
            PickKind::LineSpacing => "Line spacing",
            PickKind::Equation => "Equation",
        }));
        let chip = |id_key: usize, label: SharedString, tag: &'static str| {
            div().id((tag, id_key)).flex().items_center().px_2().h(px(22.)).rounded(px(3.)).text_size(px(12.)).text_color(pal.fg).border_1().border_color(pal.border).cursor_pointer().hover(|d| d.bg(pal.hover).border_color(hsla_u(BRAND))).child(label)
        };
        match kind {
            PickKind::Color => {
                // Automatic (clear) chip.
                row = row.child(
                    div()
                        .id("col-auto")
                        .px_2()
                        .h(px(20.))
                        .rounded(px(3.))
                        .text_size(px(11.))
                        .text_color(pal.fg)
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .hover(|d| d.bg(pal.hover))
                        .child("Automatic")
                        .on_click(cx.listener(|this, _, window, cx| this.apply_color(None, window, cx))),
                );
                for &c in COLOR_SWATCHES {
                    let hex = format!("{c:06X}");
                    row = row.child(swatch(hsla_u(c), c == 0xFFFFFF).id(("col", c as usize)).on_click(cx.listener(move |this, _, window, cx| this.apply_color(Some(hex.clone()), window, cx))));
                }
            }
            PickKind::Highlight => {
                row = row.child(
                    div()
                        .id("hl-none")
                        .px_2()
                        .h(px(20.))
                        .rounded(px(3.))
                        .text_size(px(11.))
                        .text_color(pal.fg)
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .hover(|d| d.bg(pal.hover))
                        .child("None")
                        .on_click(cx.listener(|this, _, window, cx| this.apply_highlight(None, window, cx))),
                );
                for (i, &name) in HIGHLIGHT_SWATCHES.iter().enumerate() {
                    let (c, _) = highlight_rgb(name);
                    row = row.child(swatch(hsla_u(c), false).id(("hl", i)).on_click(cx.listener(move |this, _, window, cx| this.apply_highlight(Some(name.to_string()), window, cx))));
                }
            }
            PickKind::FontName => {
                for (i, &name) in FONT_NAMES.iter().enumerate() {
                    // Preview each name in its own font family.
                    row = row.child(chip(i, name.into(), "fn").font_family(name).on_click(cx.listener(move |this, _, window, cx| this.apply_font(name.to_string(), window, cx))));
                }
            }
            PickKind::FontSize => {
                for (i, &pts) in FONT_SIZES.iter().enumerate() {
                    row = row.child(chip(i, pts.to_string().into(), "fs").on_click(cx.listener(move |this, _, window, cx| this.apply_size(pts, window, cx))));
                }
            }
            PickKind::Field => {
                for (i, &(label, instr, fallback)) in FIELDS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "fld").on_click(cx.listener(move |this, _, window, cx| this.insert_field(instr, fallback, window, cx))));
                }
            }
            PickKind::Table => {
                for (i, &(label, r, c)) in TABLE_PRESETS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "tbl").on_click(cx.listener(move |this, _, window, cx| this.insert_table(r, c, window, cx))));
                }
            }
            PickKind::Symbol => {
                for (i, &s) in SYMBOLS.iter().enumerate() {
                    // A slightly larger, fixed-width chip so each glyph reads clearly.
                    row = row.child(
                        div()
                            .id(("sym", i))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .rounded(px(3.))
                            .text_size(px(15.))
                            .text_color(pal.fg)
                            .border_1()
                            .border_color(pal.border)
                            .cursor_pointer()
                            .hover(|d| d.bg(pal.hover).border_color(hsla_u(BRAND)))
                            .child(SharedString::from(s))
                            .on_click(cx.listener(move |this, _, window, cx| this.insert_symbol(s, window, cx))),
                    );
                }
            }
            PickKind::LineSpacing => {
                // Word's Line Spacing menu: the multiples with the current one lit,
                // then Add/Remove space before/after the paragraph.
                let (cur, has_before, has_after) = match self.tabs.get(self.active).map(|t| &t.surface) {
                    Some(Surface::Doc(ed)) => (ed.caret_line_multiple(), ed.caret_space_before().unwrap_or(0) > 0, ed.caret_space_after().unwrap_or(0) > 0),
                    _ => (None, false, false),
                };
                for (i, &(label, mult, line)) in LINE_SPACINGS.iter().enumerate() {
                    let active = cur.is_some_and(|c| (c - mult).abs() < 0.03);
                    row = row.child(
                        div()
                            .id(("ls", i))
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_w(px(34.))
                            .h(px(22.))
                            .px_2()
                            .rounded(px(3.))
                            .text_size(px(12.))
                            .text_color(if active { hsla_u(BRAND) } else { pal.fg })
                            .border_1()
                            .border_color(if active { hsla_u(BRAND) } else { pal.border })
                            .cursor_pointer()
                            .hover(|d| d.bg(pal.hover).border_color(hsla_u(BRAND)))
                            .child(label)
                            .on_click(cx.listener(move |this, _, window, cx| this.apply_line_spacing(line, window, cx))),
                    );
                }
                row = row.child(div().w(px(1.)).h(px(16.)).bg(pal.border));
                let before = if has_before { "Remove space before" } else { "Add space before" };
                let after = if has_after { "Remove space after" } else { "Add space after" };
                row = row.child(chip(100, before.into(), "lsb").on_click(cx.listener(|this, _, window, cx| this.toggle_space_before(window, cx))));
                row = row.child(chip(101, after.into(), "lsa").on_click(cx.listener(|this, _, window, cx| this.toggle_space_after(window, cx))));
            }
            PickKind::Equation => {
                for (i, &(label, latex)) in EQUATIONS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "eq").on_click(cx.listener(move |this, _, window, cx| this.insert_equation(latex, window, cx))));
                }
            }
        }
        row.into_any_element()
    }

    // ---- comments ----------------------------------------------------------

    /// The next numeric comment id for the active tab (max existing + 1).
    fn next_comment_id(&self) -> i32 {
        self.tabs
            .get(self.active)
            .map(|t| t.comments.iter().filter_map(|c| c.id.parse::<i32>().ok()).max().map(|m| m + 1).unwrap_or(1))
            .unwrap_or(1)
    }

    /// Begin a new comment on the current selection (opens the comment entry bar).
    fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_sel = matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Doc(ed)) if ed.has_selection());
        if !has_sel {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.status = "Select text first, then add a comment".into();
            }
            return self.refocus(window, cx);
        }
        self.comment_open = true;
        self.comment_text.clear();
        self.picker = None;
        self.find_open = false;
        self.refocus(window, cx);
    }

    /// Commit the pending comment: wrap the selection in markers, store the text.
    fn commit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.comment_text.trim().to_string();
        self.comment_open = false;
        if text.is_empty() {
            return self.refocus(window, cx);
        }
        let id = self.next_comment_id();
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                let quoted = ed.selection_text();
                if !ed.add_comment(&id.to_string()) {
                    t.status = "No selection to comment on".into();
                    return self.refocus(window, cx);
                }
                let author = "docxy".to_string();
                t.comments.push(Comment { id: id.to_string(), author, initials: "D".into(), date: String::new(), text, quoted });
                t.dirty = true;
                t.status = format!("Comment {id} added").into();
            }
        }
        self.refocus(window, cx);
    }

    /// Route a keystroke to the comment entry bar while it is open.
    fn comment_key(&mut self, ev: &KeyDownEvent, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            "escape" => {
                self.comment_open = false;
                self.comment_text.clear();
                return self.refocus(window, cx);
            }
            "enter" => return self.commit_comment(window, cx),
            "backspace" => {
                self.comment_text.pop();
                return cx.notify();
            }
            _ => {}
        }
        if let Some(c) = ev.keystroke.key_char.as_deref() {
            if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                self.comment_text.push_str(c);
                cx.notify();
            }
        }
    }

    /// Begin dragging a ruler marker.
    fn ruler_drag_start(&mut self, handle: RulerHandle, x: f32, cx: &mut Context<Self>) {
        let (indent, first, right) = match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => {
                let (i, f) = ed.caret_para_indent();
                (i, f, ed.caret_para_right_indent())
            }
            _ => (0, 0, 0),
        };
        let geom = self.tabs.get(self.active).and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        self.ruler_drag = Some(RulerDrag { handle, start_x: x, start_indent: indent, start_first: first, start_right: right, start_ml: geom.ml, start_mr: geom.mr });
        self.ruler_guide = Some(x); // guide starts under the pointer
        cx.notify();
    }

    /// Update the dragged ruler marker (x is the live pointer position; only the
    /// delta from the drag start is used). Markers snap to the 1/8" ruler grid
    /// (Word's sticky ruler), and a vertical guide line is tracked down the page.
    fn ruler_drag_move(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.ruler_drag else { return };
        let delta = ((x - d.start_x) * 15.0).round() as i32; // px → twips
        let x0 = self.ruler_x0.get(); // left-margin screen x
        let geom = self.tabs.get(self.active).and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        let content_w = (geom.w - geom.ml - geom.mr).max(0);
        let origin = x0 - geom.ml as f32 / 15.0; // page's left-edge screen x
        let tw = |t: i32| t as f32 / 15.0;
        let guide = match d.handle {
            RulerHandle::FirstLine => {
                // Snap the marker (indent + first) to the grid, keep indent fixed.
                let marker = snap_twips(d.start_indent + d.start_first + delta);
                let first = marker - d.start_indent;
                self.with_editor(window, cx, |e| e.set_first_line(first));
                x0 + tw(d.start_indent + first)
            }
            RulerHandle::Left => {
                let ind = snap_twips((d.start_indent + delta).max(0));
                let first = d.start_first;
                self.with_editor(window, cx, |e| e.set_indent(ind, first));
                x0 + tw(ind)
            }
            // Dragging the right marker left (negative delta) increases the indent.
            RulerHandle::Right => {
                let ri = snap_twips((d.start_right - delta).max(0));
                self.with_editor(window, cx, |e| e.set_right_indent(ri));
                x0 + tw(content_w - ri)
            }
            RulerHandle::MarginLeft | RulerHandle::MarginRight => {
                let (ml, mr) = match d.handle {
                    RulerHandle::MarginLeft => (snap_twips((d.start_ml + delta).max(0)), geom.mr),
                    _ => (geom.ml, snap_twips((d.start_mr - delta).max(0))),
                };
                if let Some(t) = self.tabs.get_mut(self.active) {
                    if let Some(pkg) = t.pkg.as_mut() {
                        pkg.set_page_margins(geom.mt, mr, geom.mb, ml);
                        t.dirty = true;
                    }
                }
                match d.handle {
                    RulerHandle::MarginLeft => origin + tw(ml),
                    _ => origin + tw(geom.w - mr),
                }
            }
        };
        self.ruler_guide = Some(guide);
        cx.notify();
    }

    /// Add a tab stop at the clicked ruler position (mapped from the stored
    /// left-margin origin), or remove one if the click lands on an existing stop.
    fn ruler_click_tab(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let pos = ((x - self.ruler_x0.get()) * 15.0).round() as i32;
        if pos < 0 {
            return;
        }
        let align = self.ruler_tab;
        self.with_editor(window, cx, |e| {
            if !e.remove_tab_stop_near(pos, 90) {
                e.add_tab_stop(pos, align);
            }
        });
    }

    /// Cycle the tab-stop type set by clicking the ruler (Left → Center → Right).
    fn cycle_ruler_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use docxcore::model::TabAlign::*;
        self.ruler_tab = match self.ruler_tab {
            Left => Center,
            Center => Right,
            Right => Left,
        };
        self.refocus(window, cx);
    }

    fn ruler_drag_end(&mut self, cx: &mut Context<Self>) {
        if self.ruler_drag.take().is_some() {
            self.ruler_guide = None;
            cx.notify();
        }
    }

    /// A Word-style horizontal ruler: the page/margins, tick marks, tab stops and
    /// the current paragraph's first-line / left (other-rows) indent markers.
    fn ruler(&self, cx: &mut Context<Self>) -> AnyElement {
        use docxcore::model::{TabAlign, TabStop};
        let tab = self.tabs.get(self.active);
        let geom = tab.and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        let (indent, first_line, indent_right, tabs): (i32, i32, i32, Vec<TabStop>) = match tab.map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => {
                let (i, f) = ed.caret_para_indent();
                (i, f, ed.caret_para_right_indent(), ed.caret_para_props().tabs.clone())
            }
            _ => (0, 0, 0, vec![]),
        };
        let d = 15.0_f32; // twips → px at ~96dpi
        let pw = geom.w as f32 / d;
        let ml = geom.ml as f32 / d;
        let mr = geom.mr as f32 / d;
        let ind = indent as f32 / d;
        let fl = first_line as f32 / d;
        let rind = indent_right as f32 / d;
        let content_r = pw - mr;
        let right_marker = content_r - rind;
        let h = 22.0_f32;
        let x0_cell = self.ruler_x0.clone();

        let paint = canvas(
            move |_b, _w, _a| {},
            move |b: Bounds<Pixels>, _s, window: &mut Window, _a: &mut App| {
                // Record the left-margin screen x so click handlers can map a click
                // to a tab position (twips from the left margin).
                x0_cell.set(f32::from(b.origin.x) + ml);
                let x = |v: f32| b.origin.x + px(v);
                let top = b.origin.y;
                let base = hsla_u(0xb8b8b8);
                let white = hsla_u(0xffffff);
                let tick = hsla_u(0x707070);
                let brand = hsla_u(BRAND);
                let dim = hsla_u(0x555555);
                window.paint_quad(fill(b, base));
                window.paint_quad(fill(Bounds::from_corners(point(x(ml), top + px(3.)), point(x(content_r), top + px(h - 3.))), white));
                // Tick marks every 1/8", taller each inch, from the left margin.
                let step = 96.0 / 8.0;
                let mut i = 0;
                let mut xx = ml;
                while xx <= content_r + 0.5 {
                    let major = i % 8 == 0;
                    let th = if major { h * 0.34 } else if i % 4 == 0 { h * 0.24 } else { h * 0.15 };
                    window.paint_quad(fill(Bounds::from_corners(point(x(xx), top + px((h - th) * 0.5)), point(x(xx + 1.0), top + px((h + th) * 0.5))), tick));
                    xx += step;
                    i += 1;
                }
                // Default tab stops (every 0.5") as tiny ticks along the baseline.
                let mut tx = ml + 48.0;
                while tx <= content_r {
                    window.paint_quad(fill(Bounds::from_corners(point(x(tx), top + px(h - 4.)), point(x(tx + 1.0), top + px(h - 2.))), hsla_u(0x999999)));
                    tx += 48.0;
                }
                // Custom tab stops (from the paragraph) as L / ⊥ / ⌐ markers.
                for t in &tabs {
                    let sx = x(ml + t.pos as f32 / d);
                    let yb = top + px(h - 5.);
                    // vertical stem
                    window.paint_quad(fill(Bounds::from_corners(point(sx, top + px(h - 11.)), point(sx + px(1.5), yb)), dim));
                    // foot direction encodes alignment
                    let (fx0, fx1) = match t.align {
                        TabAlign::Left => (0.0, 5.0),
                        TabAlign::Right => (-5.0, 0.0),
                        TabAlign::Center => (-3.0, 3.0),
                    };
                    window.paint_quad(fill(Bounds::from_corners(point(sx + px(fx0), yb - px(1.5)), point(sx + px(fx1), yb)), dim));
                }
                let z = point(0.0_f32, 0.0);
                // First-line indent — downward triangle at the top.
                let flx = ml + ind + fl;
                let mut t1 = Path::new(point(x(flx - 5.0), top + px(1.)));
                t1.push_triangle((point(x(flx - 5.0), top + px(1.)), point(x(flx + 5.0), top + px(1.)), point(x(flx), top + px(8.))), (z, z, z));
                window.paint_path(t1, brand);
                // Left / other-rows indent — upward triangle + a square below it.
                let lx = ml + ind;
                let by = top + px(h - 1.);
                let mut t2 = Path::new(point(x(lx - 5.0), by - px(4.)));
                t2.push_triangle((point(x(lx - 5.0), by - px(4.)), point(x(lx + 5.0), by - px(4.)), point(x(lx), by - px(11.))), (z, z, z));
                window.paint_path(t2, brand);
                window.paint_quad(fill(Bounds::from_corners(point(x(lx - 4.0), by - px(4.)), point(x(lx + 4.0), by)), brand));
                // Right indent — upward triangle, positioned in from the right
                // margin by the paragraph's right indent.
                let rx = right_marker;
                let mut t3 = Path::new(point(x(rx - 5.0), by));
                t3.push_triangle((point(x(rx - 5.0), by), point(x(rx + 5.0), by), point(x(rx), top + px(h - 8.))), (z, z, z));
                window.paint_path(t3, brand);
            },
        );

        // Measurement numbers (1, 2, 3 …) at each inch from the left margin.
        let mut numbers = div().absolute().size_full();
        let mut inch = 1;
        loop {
            let xx = ml + inch as f32 * 96.0;
            if xx > content_r - 6.0 {
                break;
            }
            numbers = numbers.child(div().absolute().left(px(xx - 3.0)).top(px(4.0)).text_size(px(8.)).text_color(hsla_u(0x555555)).child(SharedString::from(inch.to_string())));
            inch += 1;
        }

        // Draggable indent handles, split into a TOP band (first-line marker) and
        // a BOTTOM band (left / right markers) so they never overlap where they
        // share an x (e.g. a paragraph with no indent) and each stays grabbable.
        let handle = |id: &'static str, cx_px: f32, top_px: f32, h_px: f32, which: RulerHandle, cxx: &mut Context<Self>| {
            div()
                .id(id)
                .absolute()
                .left(px(cx_px - 6.0))
                .top(px(top_px))
                .w(px(12.))
                .h(px(h_px))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cxx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.ruler_drag_start(which, f32::from(ev.position.x), cx);
                }))
        };

        // Margin grab strips sit in the grey zone just OUTSIDE the white content
        // area (Word's margin boundary), clear of the indent markers.
        let margin_handle = |id: &'static str, left_px: f32, which: RulerHandle, cxx: &mut Context<Self>| {
            div()
                .id(id)
                .absolute()
                .left(px(left_px))
                .top(px(0.))
                .w(px(8.))
                .h(px(h))
                .cursor_col_resize()
                .on_mouse_down(MouseButton::Left, cxx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.ruler_drag_start(which, f32::from(ev.position.x), cx);
                }))
        };

        let container = div()
            .relative()
            .w(px(pw))
            .h(px(h))
            .child(paint.size_full())
            .child(numbers)
            // Click the content area to add/remove a tab stop of the current type.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                this.ruler_click_tab(f32::from(ev.position.x), window, cx);
            }))
            .child(margin_handle("rh-mleft", ml - 8.0, RulerHandle::MarginLeft, cx))
            .child(margin_handle("rh-mright", content_r, RulerHandle::MarginRight, cx))
            .child(handle("rh-first", ml + ind + fl, 0.0, h * 0.5, RulerHandle::FirstLine, cx))
            .child(handle("rh-left", ml + ind, h * 0.5, h * 0.5, RulerHandle::Left, cx))
            .child(handle("rh-right", right_marker, h * 0.5, h * 0.5, RulerHandle::Right, cx));

        // The tab-type selector box at the far left (click to cycle L/Centre/Right).
        let tab_glyph = match self.ruler_tab {
            docxcore::model::TabAlign::Left => "L",
            docxcore::model::TabAlign::Center => "\u{22A5}",
            docxcore::model::TabAlign::Right => "\u{2510}",
        };
        // Same width + gap as the vertical ruler so the horizontal ruler content
        // lines up with the page sheet (this box sits in the corner, Word-style).
        let selector = div()
            .id("ruler-tabtype")
            .w(px(18.))
            .h(px(18.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(2.))
            .bg(hsla_u(0xffffff))
            .border_1()
            .border_color(hsla_u(0xb0b0b0))
            .text_size(px(11.))
            .text_color(hsla_u(0x333333))
            .cursor_pointer()
            .child(tab_glyph)
            .tooltip(|w, cx| Tooltip::new("Tab stop type — click to cycle").build(w, cx))
            .on_click(cx.listener(|this, _, window, cx| this.cycle_ruler_tab(window, cx)));

        // Drag move/end are handled at the window root (so a drag survives the
        // pointer leaving this thin strip); the ruler itself only starts drags.
        h_flex()
            .w_full()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .bg(hsla_u(0xdedede))
            .py_0p5()
            .child(selector)
            .child(container)
            .into_any_element()
    }

    /// The vertical ruler on the left of the page (Print Layout): top/bottom
    /// margins shaded, content white, tick marks every inch from the top margin.
    fn vruler(&self) -> AnyElement {
        let geom = self.tabs.get(self.active).and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        let d = 15.0_f32;
        let mt = geom.mt as f32 / d;
        let mb = geom.mb as f32 / d;
        let paint = canvas(
            move |_b, _w, _a| {},
            move |b: Bounds<Pixels>, _s, window: &mut Window, _a: &mut App| {
                let y = |v: f32| b.origin.y + px(v);
                let left = b.origin.x;
                let w = f32::from(b.size.width);
                let hh = f32::from(b.size.height);
                let base = hsla_u(0xb8b8b8);
                let white = hsla_u(0xffffff);
                let tick = hsla_u(0x707070);
                // ground + white content strip between top and bottom margins.
                window.paint_quad(fill(b, base));
                window.paint_quad(fill(Bounds::from_corners(point(left + px(3.), y(mt)), point(left + px(w - 3.), y((hh - mb).max(mt)))), white));
                // ticks every 1/8", taller each inch, measured from the top margin.
                let step = 96.0 / 8.0;
                let mut i = 0;
                let mut yy = mt;
                while yy <= hh - mb + 0.5 {
                    let major = i % 8 == 0;
                    let tw = if major { w * 0.42 } else if i % 4 == 0 { w * 0.30 } else { w * 0.18 };
                    let x1 = left + px((w - tw) * 0.5);
                    let x2 = left + px((w + tw) * 0.5);
                    window.paint_quad(fill(Bounds::from_corners(point(x1, y(yy)), point(x2, y(yy + 1.0))), tick));
                    yy += step;
                    i += 1;
                }
            },
        );
        // Measurement numbers down the ruler at each inch from the top margin.
        let mut numbers = div().absolute().size_full();
        let mut inch = 1;
        loop {
            let yy = mt + inch as f32 * 96.0;
            // stop numbering a little before the bottom margin using a generous page.
            if yy > (geom.h as f32 / d) - mb - 6.0 {
                break;
            }
            numbers = numbers.child(div().absolute().top(px(yy - 5.0)).left(px(4.0)).text_size(px(8.)).text_color(hsla_u(0x555555)).child(SharedString::from(inch.to_string())));
            inch += 1;
        }
        div().relative().w(px(18.)).flex_none().child(paint.size_full()).child(numbers).into_any_element()
    }

    /// The comment entry bar, shown under the ribbon while `comment_open`.
    fn comment_bar(&self, pal: Pal, _cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(icon_svg("comment-add", 14., hsla_u(BRAND)))
            .child(div().text_size(px(11.)).text_color(pal.dim).child("New comment"))
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .px_2()
                    .h(px(24.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .bg(pal.panel)
                    .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(self.comment_text.clone())))
                    .child(caret_bar()),
            )
            .child(div().text_size(px(11.)).text_color(pal.dim).child("Enter to add · Esc to cancel"))
            .into_any_element()
    }

    /// Delete a comment: strip its markers from the body and drop it from the list.
    fn delete_comment(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            if let Surface::Doc(ed) = &mut t.surface {
                ed.remove_comment_markers(&id);
            }
            t.comments.retain(|c| c.id != id);
            t.dirty = true;
        }
        self.refocus(window, cx);
    }

    /// Select the text a comment is anchored to (find its quoted run).
    fn goto_comment(&mut self, quoted: String, window: &mut Window, cx: &mut Context<Self>) {
        if !quoted.is_empty() {
            if let Some(ed) = self.active_editor() {
                if let Some(m) = ed.find_next(&quoted, true, false) {
                    ed.select_match(&m);
                }
            }
        }
        self.scroll_to_caret();
        self.refocus(window, cx);
    }

    /// The comments review side panel (toggled from Review/View ▸ Comments pane).
    fn comments_panel(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let comments = self.tabs.get(self.active).map(|t| t.comments.clone()).unwrap_or_default();
        let mut list = v_flex().id("cmt-list").flex_1().overflow_y_scroll().gap_2().p_2();
        if comments.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(pal.dim).p_2().child("No comments. Select text, then Review \u{203A} New comment."));
        }
        for c in &comments {
            let id = c.id.clone();
            let quoted = c.quoted.clone();
            list = list.child(
                v_flex()
                    .id(("cmt", id.parse::<usize>().unwrap_or(0)))
                    .gap_1()
                    .p_2()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .cursor_pointer()
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(hsla_u(BRAND)).child(SharedString::from(c.author.clone())))
                            .child(
                                div()
                                    .id(("cmtx", id.parse::<usize>().unwrap_or(0)))
                                    .px_1()
                                    .rounded_sm()
                                    .text_color(pal.dim)
                                    .hover(|d| d.bg(pal.hover))
                                    .child("\u{00d7}")
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.delete_comment(id.clone(), window, cx);
                                        }
                                    })),
                            ),
                    )
                    .when(!c.quoted.is_empty(), |d| d.child(div().text_size(px(11.)).italic().text_color(pal.dim).child(SharedString::from(format!("\u{201C}{}\u{201D}", c.quoted)))))
                    .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(c.text.clone())))
                    .on_click(cx.listener(move |this, _, window, cx| this.goto_comment(quoted.clone(), window, cx))),
            );
        }
        v_flex()
            .w(px(280.))
            .h_full()
            .border_l_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).border_b_1().border_color(pal.border).child(SharedString::from(format!("Comments ({})", comments.len()))))
            .child(list)
            .into_any_element()
    }

    /// Navigate the caret to the start of a top-level block and scroll to it.
    fn goto_block(&mut self, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.set_caret(vec![block], 0, false, window, cx);
        self.scroll_to_caret();
    }

    /// The navigation (heading outline) side panel — click a heading to jump.
    fn nav_panel(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let headings: Vec<(usize, u8, String)> = match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => ed
                .doc
                .body
                .iter()
                .enumerate()
                .filter_map(|(i, b)| match b {
                    Block::Paragraph(p) => p.props.heading_level.map(|lvl| (i, lvl as u8, p.plain_text())),
                    _ => None,
                })
                .filter(|(_, _, t)| !t.trim().is_empty())
                .collect(),
            _ => vec![],
        };
        let mut list = v_flex().id("nav-list").flex_1().overflow_y_scroll().gap_0p5().p_2();
        if headings.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(pal.dim).p_2().child("No headings."));
        }
        for (i, lvl, text) in &headings {
            let block = *i;
            let indent = (lvl.saturating_sub(1)) as f32 * 12.0;
            list = list.child(
                div()
                    .id(("nav", block))
                    .pl(px(8.0 + indent))
                    .pr_2()
                    .py_1()
                    .rounded(px(3.))
                    .text_size(px(if *lvl <= 1 { 13. } else { 12. }))
                    .text_color(if *lvl <= 1 { pal.fg } else { pal.dim })
                    .when(*lvl <= 1, |d| d.font_weight(FontWeight::MEDIUM))
                    .cursor_pointer()
                    .hover(|d| d.bg(pal.hover))
                    .child(SharedString::from(text.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.goto_block(block, window, cx))),
            );
        }
        v_flex()
            .w(px(240.))
            .h_full()
            .border_r_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).border_b_1().border_color(pal.border).child("Navigation"))
            .child(list)
            .into_any_element()
    }

    /// The footnotes/endnotes side panel (display-only).
    fn notes_panel(&self, pal: Pal, _cx: &mut Context<Self>) -> AnyElement {
        let notes = self.tabs.get(self.active).map(|t| t.notes.clone()).unwrap_or_default();
        let mut list = v_flex().id("notes-list").flex_1().overflow_y_scroll().gap_2().p_2();
        if notes.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(pal.dim).p_2().child("No footnotes or endnotes."));
        }
        for n in &notes {
            let tag = if n.endnote { "endnote" } else { "footnote" };
            list = list.child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(hsla_u(BRAND)).child(SharedString::from(format!("{tag} {}", n.id))))
                    .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(n.text.clone()))),
            );
        }
        v_flex()
            .w(px(280.))
            .h_full()
            .border_l_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).border_b_1().border_color(pal.border).child(SharedString::from(format!("Notes ({})", notes.len()))))
            .child(list)
            .into_any_element()
    }

    /// Route a keystroke to the find bar while it is open.
    fn find_key(&mut self, ev: &KeyDownEvent, shift: bool, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            "escape" => return self.toggle_find(window, cx),
            "enter" => {
                if self.find_field == FindField::Replace {
                    self.replace_one(cx);
                } else {
                    self.find_step(shift, false, cx);
                }
                return;
            }
            // Tab is swallowed by gpui's focus traversal, so also accept Up/Down to
            // move between the Find and Replace fields (Tab still works if delivered).
            "tab" | "down" | "up" => {
                self.find_field = match (self.find_field, key) {
                    (FindField::Query, "up") => FindField::Query,
                    (FindField::Replace, "down") => FindField::Replace,
                    (FindField::Query, _) => FindField::Replace,
                    (FindField::Replace, _) => FindField::Query,
                };
                cx.notify();
                return;
            }
            "backspace" => {
                let f = self.find_field;
                let field = if f == FindField::Query { &mut self.find_query } else { &mut self.replace_text };
                field.pop();
                if f == FindField::Query {
                    self.find_step(false, true, cx);
                } else {
                    cx.notify();
                }
                return;
            }
            _ => {}
        }
        if let Some(c) = ev.keystroke.key_char.as_deref() {
            if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                let f = self.find_field;
                if f == FindField::Query {
                    self.find_query.push_str(c);
                    self.find_step(false, true, cx);
                } else {
                    self.replace_text.push_str(c);
                    cx.notify();
                }
            }
        }
    }

    /// The find & replace bar, shown under the ribbon while `find_open`.
    fn find_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let count = self.match_count();
        let count_txt = if self.find_query.is_empty() {
            String::new()
        } else if count == 0 {
            "no matches".to_string()
        } else {
            format!("{count} match{}", if count == 1 { "" } else { "es" })
        };

        let field = |label: &'static str, text: &str, active: bool| {
            h_flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .h(px(24.))
                .min_w(px(150.))
                .rounded(px(4.))
                .border_1()
                .border_color(if active { hsla_u(BRAND) } else { pal.border })
                .bg(pal.panel)
                .child(div().text_size(px(10.)).text_color(pal.dim).child(label))
                .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(text.to_string())))
                .when(active, |d| d.child(caret_bar()))
        };

        let icon_btn = |id: &'static str, glyph: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(14.))
                .text_color(pal.fg)
                .when(on, |d| d.bg(pal.hover))
                .hover(|d| d.bg(pal.hover))
                .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
                .child(glyph)
        };
        let text_btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .px_2()
                .h(px(24.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .hover(|d| d.bg(pal.hover))
                .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
                .child(label)
        };

        h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(field("Find", &self.find_query, self.find_field == FindField::Query).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.find_field = FindField::Query;
                cx.notify();
            })))
            .child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(68.)).child(SharedString::from(count_txt)))
            .child(icon_btn("f-prev", "\u{2191}", false).on_click(cx.listener(|this, _, _, cx| this.find_step(true, false, cx))))
            .child(icon_btn("f-next", "\u{2193}", false).on_click(cx.listener(|this, _, _, cx| this.find_step(false, false, cx))))
            .child(div().w(px(1.)).h(px(18.)).bg(pal.border))
            .child(field("Replace", &self.replace_text, self.find_field == FindField::Replace).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.find_field = FindField::Replace;
                cx.notify();
            })))
            .child(text_btn("f-rep", "Replace").on_click(cx.listener(|this, _, _, cx| this.replace_one(cx))))
            .child(text_btn("f-all", "All").on_click(cx.listener(|this, _, _, cx| this.replace_all_now(cx))))
            .child(div().flex_1())
            .child(icon_btn("f-case", "Aa", self.find_case).on_click(cx.listener(|this, _, _, cx| {
                this.find_case = !this.find_case;
                this.find_step(false, true, cx);
            })))
            .child(icon_btn("f-close", "\u{00d7}", false).on_click(cx.listener(|this, _, window, cx| this.toggle_find(window, cx))))
            .into_any_element()
    }

    /// Insert a tab at the caret (bound to the Tab key via an action, since gpui
    /// swallows Tab for focus traversal before on_key_down sees it).
    fn tab_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // While the find bar is open, Tab switches between the query and replace
        // fields.
        if self.find_open {
            self.find_field = match self.find_field {
                FindField::Query => FindField::Replace,
                FindField::Replace => FindField::Query,
            };
            cx.notify();
            return;
        }
        if self.keytips != KeyTip::Off || self.comment_open || self.backstage {
            return;
        }
        self.mini_bar = None;
        self.context_menu = None;
        // On a sheet, Tab commits the edit and advances one cell to the right.
        if self.active_is_sheet() {
            return self.sheet_commit(0, 1, cx);
        }
        self.with_editor(window, cx, |e| e.insert_tab());
        self.scroll_to_caret();
    }

    /// Shift+Tab decreases the paragraph indent (Word's outdent).
    fn shift_tab_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.keytips != KeyTip::Off || self.find_open || self.comment_open || self.backstage {
            return;
        }
        self.mini_bar = None;
        self.context_menu = None;
        // On a sheet, Shift+Tab commits and moves one cell to the left.
        if self.active_is_sheet() {
            return self.sheet_commit(0, -1, cx);
        }
        self.with_editor(window, cx, |e| e.change_indent(-720));
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        let ctrl = m.control || m.platform;
        let shift = m.shift;
        let key = ev.keystroke.key.clone();
        // KeyTips (Alt / F10 access keys): toggle the overlay; while it's showing,
        // letters pick a tab / run a command instead of typing.
        if (key == "alt" || key == "f10") && !ctrl {
            self.keytips = if self.keytips == KeyTip::Off { KeyTip::Tabs } else { KeyTip::Off };
            cx.notify();
            return;
        }
        if self.keytips != KeyTip::Off {
            if key == "escape" {
                self.keytips = if self.keytips == KeyTip::Commands { KeyTip::Tabs } else { KeyTip::Off };
                cx.notify();
                return;
            }
            if let Some(c) = ev.keystroke.key_char.as_deref().filter(|c| c.chars().count() == 1 && c.chars().next().is_some_and(|ch| ch.is_ascii_alphanumeric())) {
                return self.keytip_input(c, window, cx);
            }
            return; // swallow other keys while KeyTips are up
        }
        // Any key dismisses the floating mini toolbar / context menu.
        self.mini_bar = None;
        self.context_menu = None;
        // Spreadsheet surface: the grid has its own key handling (navigation,
        // cell editing, recalc) — nothing routes to a text editor.
        if self.active_is_sheet() {
            return self.sheet_key(ev, ctrl, shift, key.as_str(), window, cx);
        }
        // In header/footer edit mode, Esc returns to the document body.
        if key == "escape" && self.hf_active() {
            return self.exit_hf(window, cx);
        }
        // Ctrl+F toggles the find bar; while it's open, all keys go to it.
        if ctrl && key == "f" {
            return self.toggle_find(window, cx);
        }
        if self.comment_open {
            return self.comment_key(ev, key.as_str(), window, cx);
        }
        if self.find_open {
            return self.find_key(ev, shift, key.as_str(), window, cx);
        }
        if ctrl {
            match key.as_str() {
                "s" => return self.save_active(window, cx),
                "c" => return self.do_copy(false, window, cx),
                "x" => return self.do_copy(true, window, cx),
                "v" => return self.do_paste(window, cx),
                "f1" => {
                    self.ribbon_min = !self.ribbon_min;
                    cx.notify();
                    return;
                }
                // Zoom (Ctrl+= / Ctrl+- / Ctrl+0), the browser-standard gesture.
                "=" | "+" => {
                    self.zoom = (self.zoom + 0.1).min(3.0);
                    cx.notify();
                    return;
                }
                "-" => {
                    self.zoom = (self.zoom - 0.1).max(0.5);
                    cx.notify();
                    return;
                }
                "0" => {
                    self.zoom = 1.0;
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        let Some(ed) = self.edit_target() else { return };
        if key == "escape" {
            ed.clear_selection();
            cx.notify();
            return;
        }
        // Navigation keys extend the selection when Shift is held and collapse it
        // otherwise; every other key leaves the anchor alone (typing, backspace and
        // delete handle any active selection themselves in the engine).
        if matches!(key.as_str(), "left" | "right" | "home" | "end" | "up" | "down") {
            ed.extend_selection(shift);
        }
        let changed = if ctrl {
            match key.as_str() {
                "b" => yes(|| ed.toggle_bold()),
                "i" => yes(|| ed.toggle_italic()),
                "u" => yes(|| ed.toggle_underline()),
                "z" => {
                    ed.undo();
                    true
                }
                "y" => {
                    ed.redo();
                    true
                }
                "a" => no(|| ed.select_all()),
                // Indent / outdent (Ctrl+M, Ctrl+Shift+M).
                "m" if shift => yes(|| ed.change_indent(-720)),
                "m" => yes(|| ed.change_indent(720)),
                // Non-breaking space (Ctrl+Shift+Space), a typesetting staple.
                "space" if shift => yes(|| ed.insert_str("\u{00A0}")),
                // Word- and document-wise motion (Ctrl+←/→, Ctrl+Home/End).
                "left" => no(|| ed.move_word_left()),
                "right" => no(|| ed.move_word_right()),
                "home" => no(|| ed.move_doc_start()),
                "end" => no(|| ed.move_doc_end()),
                _ => false,
            }
        } else {
            match key.as_str() {
                "backspace" => yes(|| ed.backspace()),
                "delete" => yes(|| ed.delete_forward()),
                "enter" => yes(|| ed.insert_newline()),
                "tab" => yes(|| ed.insert_tab()),
                "left" => no(|| ed.move_left()),
                "right" => no(|| ed.move_right()),
                "home" => no(|| ed.move_home()),
                "end" => no(|| ed.move_end()),
                "up" => no(|| move_vert(ed, false)),
                "down" => no(|| move_vert(ed, true)),
                _ => match ev.keystroke.key_char.as_deref() {
                    Some(c) if !c.is_empty() && !c.chars().next().unwrap().is_control() => {
                        ed.insert_str(c);
                        true
                    }
                    _ => false,
                },
            }
        };
        if changed {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
            }
        }
        self.scroll_to_caret();
        cx.notify();
    }
}

/// Parse a cell's edit buffer into a `Cell`, Excel-style: `=…` is a formula, a
/// bare number is numeric, TRUE/FALSE is boolean, anything else is text. The
/// existing style index is carried over so formatting survives the edit.
/// Case-insensitive replace of every `needle` in `hay` with `rep` (ASCII-fold;
/// byte offsets from the lowercased copy line up for the ASCII case).
fn ci_replace(hay: &str, needle: &str, rep: &str) -> String {
    if needle.is_empty() {
        return hay.to_string();
    }
    let (hl, nl) = (hay.to_lowercase(), needle.to_lowercase());
    if hl.len() != hay.len() {
        // Non-ASCII fold changed lengths — fall back to a plain contains check.
        return if hl.contains(&nl) { hay.replace(needle, rep) } else { hay.to_string() };
    }
    let mut out = String::new();
    let mut i = 0;
    while let Some(p) = hl[i..].find(&nl) {
        out.push_str(&hay[i..i + p]);
        out.push_str(rep);
        i += p + needle.len();
    }
    out.push_str(&hay[i..]);
    out
}

fn parse_cell_input(raw: &str, style: u32) -> gridcore::sheet::Cell {
    use gridcore::sheet::{Cell, CellValue};
    let t = raw.trim();
    let mut cell = if t.is_empty() {
        Cell::default()
    } else if let Some(f) = t.strip_prefix('=') {
        Cell::formula(f)
    } else if let Ok(n) = t.parse::<f64>() {
        Cell::number(n)
    } else if t.eq_ignore_ascii_case("true") {
        Cell { value: CellValue::Bool(true), ..Cell::default() }
    } else if t.eq_ignore_ascii_case("false") {
        Cell { value: CellValue::Bool(false), ..Cell::default() }
    } else {
        Cell::text(raw)
    };
    cell.style = style;
    cell
}

fn yes(mut f: impl FnMut()) -> bool {
    f();
    true
}
fn no(mut f: impl FnMut()) -> bool {
    f();
    false
}

// ---- the ribbon, defined once via ribbonspec (shared model) ----------------

#[derive(Clone, Copy)]
enum Act {
    Bold, Italic, Underline, Strike, Grow, Shrink,
    AlignL, AlignC, AlignR, AlignJ,
    Cut, Copy, Paste,
    Normal, H1, H2, H3, HRule, SelectAll, Case,
    Bullets, Numbers, IndentInc, IndentDec, ClearFmt, Find, FontColor, Highlight, FontName, FontSize, Super, Sub, NewComment,
    Sort, LineSpacing, ParaBorders, Title, Subtitle, ShowHide, ToggleComments, ToggleNav, DarkMode, AutoHideRibbon,
    InsertField, PageBreak, ToggleNotes, InsertTable, InsertSymbol, EditHeader, EditFooter, PageNumber, NoSpacing, Columns, Hyphenation, InsertEquation,
    RowAbove, RowBelow, ColLeft, ColRight, DelRow, DelCol, DelTable, PrintLayout, ToggleRuler,
    // Dialog-box launchers (open advanced dialogs — placeholder until we have a
    // dialog system).
    LaunchFont, LaunchParagraph,
}

/// Word's line-spacing menu presets, as (label, multiple, `w:line` twips).
const LINE_SPACINGS: &[(&str, f32, i32)] = &[
    ("1.0", 1.0, 240),
    ("1.15", 1.15, 276),
    ("1.5", 1.5, 360),
    ("2.0", 2.0, 480),
    ("2.5", 2.5, 600),
    ("3.0", 3.0, 720),
];

// The two numbering ids new_markdown_package defines: 1 = bullets, 2 = decimal.
// Applying a list sets a paragraph's num_id to one of these, and save writes a
// numbering part that defines them (so the list renders in Word too).
const NUM_BULLET: i32 = 1;
const NUM_DECIMAL: i32 = 2;

/// A command with a Fluent icon id + ScreenTip (title = label, + shortcut).
fn cmdt(id: &'static str, icon: &'static str, label: &'static str, act: Act, shortcut: &'static str) -> rs::Cmd<Act> {
    rs::cmd(id, icon, label, act).tip(label, "", shortcut)
}

fn docxy_ribbon() -> rs::Ribbon<Act> {
    use Act::*;
    rs::Ribbon::new(vec![
        rs::tab("Home", "H", vec![
            // Clipboard: a large Paste button + a small Cut/Copy column (Word).
            rs::group("Clipboard", 10, vec![
                Control::Large(cmdt("paste", "paste", "Paste", Paste, "Ctrl+V").key("V")),
                rs::column(vec![
                    cmdt("cut", "cut", "Cut", Cut, "Ctrl+X").key("X"),
                    cmdt("copy", "copy", "Copy", Copy, "Ctrl+C").key("C"),
                ]),
            ]),
            // Font: two rows — combos + size controls on top, character toggles below.
            rs::group("Font", 40, vec![rs::rows(vec![
                vec![
                    rs::combo(cmdt("fontname", "font-name", "Font", FontName, ""), true),
                    rs::combo(cmdt("fontsize", "font-size", "Font size", FontSize, ""), false),
                    rs::btn(cmdt("grow", "font-increase", "Grow font", Grow, "").key("G")),
                    rs::btn(cmdt("shrink", "font-decrease", "Shrink font", Shrink, "").key("K")),
                    rs::btn(cmdt("case", "case", "Change case", Case, "").key("7")),
                    rs::btn(cmdt("clearfmt", "clear-format", "Clear formatting", ClearFmt, "").key("E")),
                ],
                vec![
                    rs::btn(cmdt("b", "bold", "Bold", Bold, "Ctrl+B").key("1")),
                    rs::btn(cmdt("i", "italic", "Italic", Italic, "Ctrl+I").key("2")),
                    rs::btn(cmdt("u", "underline", "Underline", Underline, "Ctrl+U").key("3")),
                    rs::btn(cmdt("s", "strikethrough", "Strikethrough", Strike, "").key("4")),
                    rs::btn(cmdt("sub", "subscript", "Subscript", Sub, "").key("5")),
                    rs::btn(cmdt("sup", "superscript", "Superscript", Super, "").key("6")),
                    rs::btn(cmdt("color", "text-color", "Font colour", FontColor, "").key("8")),
                    rs::btn(cmdt("hl", "highlight", "Text highlight", Highlight, "").key("9")),
                ],
            ])])
            .launcher(LaunchFont),
            // Paragraph: two rows — lists/indent/sort/marks on top, alignment below.
            rs::group("Paragraph", 30, vec![rs::rows(vec![
                vec![
                    rs::btn(cmdt("bullets", "list-bullet", "Bullets", Bullets, "").key("U")),
                    rs::btn(cmdt("numbers", "list-numbered", "Numbering", Numbers, "").key("N")),
                    rs::btn(cmdt("inddec", "indent-decrease", "Decrease indent", IndentDec, "Ctrl+Shift+M").key("O")),
                    rs::btn(cmdt("indinc", "indent-increase", "Increase indent", IndentInc, "Ctrl+M").key("P")),
                    rs::btn(cmdt("linespacing", "line-spacing", "Line and Paragraph Spacing", LineSpacing, "").key("Y")),
                    rs::btn(cmdt("sort", "sort", "Sort", Sort, "").key("S")),
                    rs::btn(cmdt("showhide", "paragraph", "Formatting marks", ShowHide, "").key("H")),
                ],
                vec![
                    rs::btn(cmdt("al", "align-left", "Align left", AlignL, "").key("L")),
                    rs::btn(cmdt("ac", "align-center", "Center", AlignC, "").key("A")),
                    rs::btn(cmdt("ar", "align-right", "Align right", AlignR, "").key("R")),
                    rs::btn(cmdt("aj", "align-justify", "Justify", AlignJ, "").key("J")),
                    rs::btn(cmdt("borders", "border-bottom", "Bottom border", ParaBorders, "").key("B")),
                ],
            ])])
            .launcher(LaunchParagraph),
            // Styles: a gallery of style thumbnails (Word keeps this on Home).
            rs::group("Styles", 35, vec![Control::Gallery(rs::Gallery {
                id: "styles",
                tip: rs::ScreenTip::default(),
                // Word's Quick Styles order: Normal, No Spacing, headings, then Title/Subtitle.
                items: vec![
                    rs::GalleryItem { label: "Normal", preview: "normal", act: Normal },
                    rs::GalleryItem { label: "No Spacing", preview: "normal", act: NoSpacing },
                    rs::GalleryItem { label: "Heading 1", preview: "h1", act: H1 },
                    rs::GalleryItem { label: "Heading 2", preview: "h2", act: H2 },
                    rs::GalleryItem { label: "Heading 3", preview: "h3", act: H3 },
                    rs::GalleryItem { label: "Title", preview: "title", act: Title },
                    rs::GalleryItem { label: "Subtitle", preview: "subtitle", act: Subtitle },
                ],
            })]),
            // Editing: a labelled column (Word: Find / Replace / Select).
            rs::group("Editing", 20, vec![rs::column(vec![
                cmdt("find", "find", "Find & Replace", Find, "Ctrl+F").key("F"),
                cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A").key("D"),
            ])]),
        ]),
        // Insert: headline commands as large buttons (Word's Insert tab style).
        rs::tab("Insert", "N", vec![
            rs::group("Pages", 40, vec![Control::Large(cmdt("pagebreak", "rule", "Page Break", PageBreak, "").key("B"))]),
            rs::group("Tables", 35, vec![Control::Large(cmdt("table", "table", "Table", InsertTable, "").key("T"))]),
            rs::group("Header & Footer", 34, vec![
                Control::Large(cmdt("header", "header", "Edit Header", EditHeader, "").key("H")),
                Control::Large(cmdt("footer", "footer", "Edit Footer", EditFooter, "").key("O")),
                Control::Large(cmdt("pagenum", "page-number", "Page Number", PageNumber, "").key("G")),
            ]),
            rs::group("Text", 30, vec![Control::Large(cmdt("field", "case", "Field", InsertField, "").key("Q"))]),
            rs::group("Symbols", 20, vec![
                Control::Large(cmdt("equation", "equation", "Equation", InsertEquation, "").key("E")),
                Control::Large(cmdt("symbol", "symbol", "Symbol", InsertSymbol, "").key("S")),
                Control::Large(cmdt("hr", "rule", "Rule", HRule, "").key("L")),
            ]),
            rs::group("Layout", 22, vec![
                Control::Large(cmdt("columns", "columns", "Columns", Columns, "").key("C")),
                Control::Large(cmdt("hyphen", "hyphenation", "Hyphenation", Hyphenation, "").key("Z")),
            ]),
        ]),
        // Review: a large New Comment + a small pane-toggle column, then Editing.
        rs::tab("Review", "R", vec![
            rs::group("Comments", 40, vec![
                Control::Large(cmdt("newcomment", "comment-add", "New Comment", NewComment, "").key("C")),
                rs::column(vec![
                    cmdt("togglecomments", "comment", "Comments pane", ToggleComments, "").key("P"),
                    cmdt("togglenotes", "comment", "Notes pane", ToggleNotes, "").key("O"),
                ]),
            ]),
            rs::group("Editing", 30, vec![rs::column(vec![
                cmdt("find", "find", "Find & Replace", Find, "Ctrl+F").key("F"),
                cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A").key("D"),
                cmdt("case", "case", "Change case", Case, "").key("7"),
            ])]),
        ]),
        // View: a large Print Layout toggle, then Show and Appearance columns.
        rs::tab("View", "W", vec![
            rs::group("Views", 40, vec![Control::Large(cmdt("printlayout", "print-layout", "Print Layout", PrintLayout, "").key("P"))]),
            rs::group("Show", 30, vec![rs::column(vec![
                cmdt("ruler", "rule", "Ruler", ToggleRuler, "").key("R"),
                cmdt("showhide", "paragraph", "Formatting marks", ShowHide, "").key("M"),
                cmdt("nav", "select-all", "Navigation", ToggleNav, "").key("N"),
                cmdt("viewcomments", "comment", "Comments pane", ToggleComments, "").key("C"),
            ])]),
            rs::group("Appearance", 20, vec![rs::column(vec![
                cmdt("darkmode", "case", "Theme", DarkMode, "").key("T"),
                cmdt("autohide", "rule", "Collapse ribbon", AutoHideRibbon, "Ctrl+F1").key("A"),
            ])]),
        ]),
    ])
}

fn ribbon_tab_index(t: RibbonTab) -> usize {
    match t {
        RibbonTab::Home => 0,
        RibbonTab::Insert => 1,
        RibbonTab::Review => 2,
        RibbonTab::View => 3,
        RibbonTab::Table => 0, // handled specially (see ribbon_body / table_tab)
    }
}

/// Find the command in a control whose KeyTip matches `key` (case-insensitive).
fn control_keytip(c: &Control<Act>, key: &str) -> Option<Act> {
    let m = |cmd: &rs::Cmd<Act>| (!cmd.key_tip.is_empty() && cmd.key_tip.eq_ignore_ascii_case(key)).then_some(cmd.act);
    match c {
        Control::Large(cmd) | Control::Toggle(cmd) => m(cmd),
        Control::Column(cmds) => cmds.iter().find_map(m),
        Control::Rows(rows) => rows.iter().flatten().find_map(|cell| match cell {
            rs::Cell::Btn(cmd) => m(cmd),
            rs::Cell::Combo { cmd, .. } => m(cmd),
        }),
        _ => None,
    }
}

/// Find a command in a tab by its KeyTip letter.
fn tab_keytip_cmd(tab: &rs::Tab<Act>, key: &str) -> Option<Act> {
    tab.groups.iter().flat_map(|g| g.items.iter()).find_map(|c| control_keytip(c, key))
}

/// A small KeyTip access-key badge, centred at the bottom of its host element.
fn keytip_badge(text: &str) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_end()
        .justify_center()
        .child(div().px(px(3.)).rounded(px(2.)).bg(hsla_u(0xf2d24b)).text_size(px(9.)).text_color(hsla_u(0x1a1a1a)).child(SharedString::from(text.to_string())))
        .into_any_element()
}

/// The contextual Table Tools tab, shown only while the caret is in a table.
fn table_tab() -> rs::Tab<Act> {
    use Act::*;
    rs::tab("Table", "T", vec![
        rs::group("Rows & Columns", 40, vec![rs::rows(vec![
            vec![
                rs::btn(cmdt("rowabove", "table-insert-row", "Insert row above", RowAbove, "")),
                rs::btn(cmdt("colleft", "table-insert-column", "Insert column left", ColLeft, "")),
                rs::btn(cmdt("delrow", "table-delete-row", "Delete row", DelRow, "")),
            ],
            vec![
                rs::btn(cmdt("rowbelow", "table-insert-row", "Insert row below", RowBelow, "")),
                rs::btn(cmdt("colright", "table-insert-column", "Insert column right", ColRight, "")),
                rs::btn(cmdt("delcol", "table-delete-column", "Delete column", DelCol, "")),
            ],
        ])]),
        rs::group("Table", 20, vec![rs::column(vec![
            cmdt("deltable", "table-dismiss", "Delete table", DelTable, ""),
        ])]),
    ])
}

/// Rough natural width (px) of a group, for responsive collapse decisions.
fn group_est(g: &rs::Group<Act>, icon_only: bool) -> f32 {
    let mut w: f32 = 22.0;
    for c in &g.items {
        w += match c {
            Control::Toggle(_) => 26.0,
            Control::Large(_) => 58.0,
            Control::Column(_) => {
                if icon_only {
                    34.0
                } else {
                    104.0
                }
            }
            // A two-row grid: its width is that of the widest row.
            Control::Rows(rows) => rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| match cell {
                            rs::Cell::Combo { wide, .. } => {
                                if *wide {
                                    106.0
                                } else {
                                    48.0
                                }
                            }
                            rs::Cell::Btn(_) => 25.0,
                        })
                        .sum::<f32>()
                })
                .fold(0.0_f32, f32::max),
            Control::Gallery(gal) => gal.items.len() as f32 * 80.0,
            Control::Separator => 10.0,
            _ => 30.0,
        };
    }
    w.max(44.0)
}

fn move_vert(ed: &mut Editor, down: bool) {
    if ed.caret.path.len() != 1 {
        return;
    }
    let i = ed.caret.path[0];
    let col = ed.caret.offset;
    let n = ed.doc.body.len();
    let candidates: Vec<usize> = if down { (i + 1..n).collect() } else { (0..i).rev().collect() };
    for j in candidates {
        if let Block::Paragraph(p) = &ed.doc.body[j] {
            let len = p.plain_text().chars().count();
            ed.caret = Caret::at(vec![j], col.min(len));
            ed.clear_selection();
            return;
        }
    }
}

// ---- doc rendering (theme-aware) -------------------------------------------

fn hsla_u(c: u32) -> Hsla {
    rgb(c).into()
}

fn hex_rgb(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Map a Word highlight name (`w:highlight`) to an approximate RGB, and whether
/// its text should read dark (true) or light (false).
fn highlight_rgb(name: &str) -> (u32, bool) {
    match name {
        "yellow" => (0xFFFF00, true),
        "green" => (0x00FF00, true),
        "cyan" => (0x00FFFF, true),
        "magenta" => (0xFF00FF, true),
        "blue" => (0x0000FF, false),
        "red" => (0xFF0000, false),
        "darkYellow" => (0x808000, false),
        "darkGreen" => (0x008000, false),
        "darkCyan" => (0x008080, false),
        "darkMagenta" => (0x800080, false),
        "darkBlue" => (0x000080, false),
        "darkRed" => (0x800000, false),
        "lightGray" => (0xC0C0C0, true),
        "darkGray" => (0x808080, false),
        "black" => (0x000000, false),
        "white" => (0xFFFFFF, true),
        _ => (0xFFF29A, true),
    }
}

/// Office-style font-colour swatches (hex, no `#`). `None` = automatic (clear).
const COLOR_SWATCHES: &[u32] = &[
    0x000000, 0x404040, 0x808080, 0xBFBFBF, 0xFFFFFF, 0xC00000, 0xFF0000, 0xFFC000, 0xFFFF00, 0x92D050, 0x00B050, 0x00B0F0, 0x0070C0, 0x002060, 0x7030A0,
];
/// Highlight swatches (Word highlight names). `None` = no highlight (clear).
const HIGHLIGHT_SWATCHES: &[&str] = &["yellow", "green", "cyan", "magenta", "blue", "red", "darkYellow", "darkGreen", "darkCyan", "darkRed", "darkBlue", "lightGray"];
/// Font families offered in the Font-name picker.
const FONT_NAMES: &[&str] = &["Calibri", "Cambria", "Arial", "Times New Roman", "Georgia", "Verdana", "Tahoma", "Segoe UI", "Courier New", "Consolas", "Comic Sans MS"];
/// Point sizes offered in the Font-size picker.
const FONT_SIZES: &[u32] = &[8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48, 72];

/// Snap a twips measurement to the nearest 1/8" ruler gridline (180 twips), so
/// dragging a ruler marker sticks to the visible ticks like Word's ruler.
fn snap_twips(v: i32) -> i32 {
    const GRID: i32 = 180; // 1/8 inch
    ((v as f32 / GRID as f32).round() as i32) * GRID
}

fn caret_bar() -> AnyElement {
    // Negative side margins cancel the 2px width so the caret takes no layout
    // space — it sits between glyphs without nudging them apart to make room.
    div().w(px(2.)).h(px(19.)).ml(px(-1.)).mr(px(-1.)).bg(rgb(BRAND)).into_any_element()
}

/// Synchronous text-width measurement (via the window's text system + the ambient
/// base font), so a tab can advance content to an absolute tab-stop column
/// instead of a fixed gap.
struct Measurer {
    ts: std::sync::Arc<WindowTextSystem>,
    base: Font,
}

impl Measurer {
    fn new(window: &Window) -> Self {
        Measurer { ts: window.text_system().clone(), base: window.text_style().font() }
    }
    /// Rendered pixel width of `text` at `size` px in the base font (bold/italic
    /// applied), matching how `emit_words` shapes it.
    fn width(&self, text: &str, size: f32, bold: bool, italic: bool) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let mut font = self.base.clone();
        if bold {
            font.weight = FontWeight::BOLD;
        }
        if italic {
            font.style = FontStyle::Italic;
        }
        let run = TextRun { len: text.len(), font, color: hsla_u(0), ..Default::default() };
        f32::from(self.ts.shape_line(SharedString::from(text.to_string()), px(size), &[run], None).width())
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_words(out: &mut Vec<AnyElement>, text: &str, props: &RunProps, base: f32, is_link: bool, selected: bool, click: Option<Click>, seg_start: usize, pal: Pal) {
    let mut off = seg_start;
    for word in text.split_inclusive(' ') {
        if word.is_empty() {
            continue;
        }
        let word_off = off;
        off += word.chars().count();
        let mut size = props.size_half_pts.map(|h| h as f32 / 2.0 * 1.333).unwrap_or(base);
        // Superscript / subscript render ~0.7× and are nudged up/down.
        let vshift = match props.vert_align {
            VertAlign::Superscript => {
                size *= 0.72;
                Some(-(base * 0.35))
            }
            VertAlign::Subscript => {
                size *= 0.72;
                Some(base * 0.18)
            }
            VertAlign::Baseline => None,
        };
        let color: Hsla = if is_link {
            hsla_u(LINK)
        } else {
            props.color.as_deref().and_then(hex_rgb).map(hsla_u).unwrap_or(pal.fg)
        };
        // Render each word as a StyledText so its TextLayout gives pixel-exact
        // hit-testing (index_for_position), for a true click-anywhere caret.
        let word_str = word.to_string();
        let styled = StyledText::new(word_str.clone());
        let layout = styled.layout().clone();
        out.push(
            div()
                .child(styled)
                .text_size(px(size))
                .text_color(color)
                .when(props.bold, |d| d.font_weight(FontWeight::BOLD))
                .when(props.italic, |d| d.italic())
                .when(props.underline || is_link, |d| d.underline())
                .when(props.strike, |d| d.line_through())
                .when_some(vshift, |d, dy| d.relative().top(px(dy)))
                // Selection wins over any run highlight so the selected range reads
                // as one contiguous band.
                .when_some(props.highlight.as_deref().filter(|_| !selected), |d, name| {
                    let (c, dark) = highlight_rgb(name);
                    d.bg(rgb(c)).text_color(if dark { rgb(0x1a1a1a) } else { rgb(0xf5f5f5) })
                })
                .when(selected, |d| d.bg(pal.sel))
                // Click / drag-to-caret: place the caret at the exact character under
                // the pointer (byte index from the layout → char count within the
                // word), and while the button is held extend the selection.
                .when_some(click, |d, c| {
                    let ent = c.ent.clone();
                    let path = c.path.to_vec();
                    let off_at = {
                        let layout = layout.clone();
                        let word_str = word_str.clone();
                        move |pos| {
                            let byte = layout.index_for_position(pos).unwrap_or_else(|e| e).min(word_str.len());
                            word_off + word_str[..byte].chars().count()
                        }
                    };
                    d.cursor_text()
                        .on_mouse_down(MouseButton::Left, {
                            let ent = ent.clone();
                            let path = path.clone();
                            let off_at = off_at.clone();
                            move |ev, window, cx| {
                                cx.stop_propagation();
                                let extend = ev.modifiers.shift;
                                let off = off_at(ev.position);
                                ent.update(cx, |this, cx| this.begin_select(path.clone(), off, extend, window, cx));
                            }
                        })
                        .on_mouse_move(move |ev, _window, cx| {
                            let off = off_at(ev.position);
                            let path = path.clone();
                            ent.update(cx, move |this, cx| {
                                if this.selecting {
                                    this.extend_select(path, off, cx);
                                }
                            });
                        })
                })
                .into_any_element(),
        );
    }
}

/// Emit one run's text, splicing in the caret bar and shading any part that falls
/// inside the selection range `sel` (both are absolute char offsets within the
/// paragraph). `idx` is advanced by the run's char length.
#[allow(clippy::too_many_arguments)]
fn emit_run(
    out: &mut Vec<AnyElement>,
    text: &str,
    props: &RunProps,
    base: f32,
    is_link: bool,
    idx: &mut usize,
    caret: &mut Option<usize>,
    sel: Option<(usize, usize)>,
    click: Option<Click>,
    pal: Pal,
) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let start = *idx;
    let end = start + len;
    // Cut the run at the caret and at each selection boundary that lands inside it,
    // so each resulting segment is uniformly selected-or-not.
    let mut cuts: Vec<usize> = vec![0, len];
    let add = |abs: usize, cuts: &mut Vec<usize>| {
        if abs > start && abs < end {
            cuts.push(abs - start);
        }
    };
    if let Some(c) = *caret {
        add(c, &mut cuts);
    }
    if let Some((s, e)) = sel {
        add(s, &mut cuts);
        add(e, &mut cuts);
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if *caret == Some(start + a) {
            out.push(caret_bar());
            *caret = None;
        }
        let seg: String = chars[a..b].iter().collect();
        let selected = sel.map_or(false, |(s, e)| s < e && start + a >= s && start + b <= e);
        emit_words(out, &seg, props, base, is_link, selected, click, start + a, pal);
    }
    if *caret == Some(end) {
        out.push(caret_bar());
        *caret = None;
    }
    *idx = end;
}

/// A tab is ONE char in the engine; render it as a fixed-width spacer that
/// advances to its tab stop (`width` px, computed by the caller). Keeps `idx` in
/// sync and participates in caret/selection like any other char.
#[allow(clippy::too_many_arguments)]
fn emit_tab(out: &mut Vec<AnyElement>, idx: &mut usize, caret: &mut Option<usize>, sel: Option<(usize, usize)>, click: Option<Click>, marks: bool, base: f32, width: f32, pal: Pal) {
    let pos = *idx;
    if *caret == Some(pos) {
        out.push(caret_bar());
        *caret = None;
    }
    let selected = sel.map_or(false, |(s, e)| s < e && s <= pos && pos < e);
    let w = width.max(3.0);
    out.push(
        div()
            .flex_none()
            .w(px(w))
            .h(px(base))
            .overflow_hidden()
            // With formatting marks on, a tab arrow sits at the start of the gap.
            .when(marks, |d| d.flex().items_center().text_size(px(base * 0.9)).text_color(pal.dim).child("\u{2192}"))
            .when(selected, |d| d.bg(pal.sel))
            .when_some(click, |d, c| {
                let ent = c.ent.clone();
                let path = c.path.to_vec();
                d.cursor_text().on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                    cx.stop_propagation();
                    let extend = ev.modifiers.shift;
                    ent.update(cx, |this, cx| this.set_caret(path.clone(), pos, extend, window, cx));
                })
            })
            .into_any_element(),
    );
    *idx += 1;
}

/// A line break is ONE char in the engine; render it as a wrap and keep `idx` in
/// sync (so caret/selection offsets past it stay correct).
fn emit_break(out: &mut Vec<AnyElement>, idx: &mut usize, caret: &mut Option<usize>) {
    if *caret == Some(*idx) {
        out.push(caret_bar());
        *caret = None;
    }
    out.push(div().w_full().h(px(0.)).into_any_element());
    *idx += 1;
}

/// Compute the list marker text for each top-level block (`None` = not a list
/// item). Bullets use •/◦ by level; decimal lists get real ordinals that restart
/// per level and break whenever a non-list block interrupts the run.
/// Extract the header (or footer) block content from a package: resolve the
/// section's header/footerReference rId → part → parse. Empty if none.
// OOXML namespaces used when re-serializing an edited header/footer part.
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Resolve the package part name (e.g. `word/header1.xml`) backing a specific
/// header/footer reference type (`"default"`, `"first"`, `"even"`).
fn hf_part_name_typed(pkg: &Package, is_header: bool, wtype: &str) -> Option<String> {
    let sect = pkg.sect_pr();
    let kind = if is_header { "headerReference" } else { "footerReference" };
    let rid = docxcore::load::header_footer_ref_rid(sect, kind, wtype)?;
    let rels_bytes = pkg.part("word/_rels/document.xml.rels")?;
    let rels = docxcore::load::parse_rels_xml(&String::from_utf8_lossy(rels_bytes));
    let target = rels.target(&rid)?;
    Some(format!("word/{}", target.trim_start_matches('/')))
}

/// Blocks of a specific header/footer variant, or empty if that ref is absent.
fn header_footer_blocks_typed(pkg: &Package, is_header: bool, wtype: &str) -> Vec<Block> {
    match hf_part_name_typed(pkg, is_header, wtype) {
        Some(name) => parse_hf_part(pkg, &name),
        None => vec![],
    }
}

/// Parse the blocks of a specific header/footer part.
fn parse_hf_part(pkg: &Package, part_name: &str) -> Vec<Block> {
    let Some(rels_bytes) = pkg.part("word/_rels/document.xml.rels") else { return vec![] };
    let rels = docxcore::load::parse_rels_xml(&String::from_utf8_lossy(rels_bytes));
    let Some(xml) = pkg.part(part_name) else { return vec![] };
    docxcore::load::parse_header_footer(&String::from_utf8_lossy(xml), &rels)
}


/// Render header/footer blocks read-only (no caret, no click) for the page margins.
fn hf_els(blocks: &[Block], pal: Pal, meas: &Measurer, hf_width: f32) -> Vec<AnyElement> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(paragraph_el(p, None, None, None, None, false, 1.0, pal, Some(meas), Some(hf_width))),
            _ => None,
        })
        .collect()
}

/// Rough rendered height (px) of a block, for paginating Print Layout. Text is
/// estimated from a character-per-line calc; close enough to place page breaks.
fn block_height_est(b: &Block, content_w: f32) -> f32 {
    match b {
        Block::Paragraph(p) => {
            let base = match p.props.heading_level {
                Some(1) => 26.0,
                Some(2) => 22.0,
                Some(3) => 19.0,
                Some(4) => 17.0,
                Some(_) => 15.0,
                None => 14.5,
            };
            let lh = base * 1.4;
            let chars = p.plain_text().chars().count().max(1) as f32;
            let cpl = (content_w / (base * 0.5)).max(1.0);
            let breaks = p.content.iter().filter(|i| matches!(i, Inline::Break(_))).count() as f32;
            let lines = (chars / cpl).ceil().max(1.0) + breaks;
            lines * lh + if p.props.heading_level.is_some() { base } else { 4.0 }
        }
        Block::Table(t) => t.rows.len() as f32 * 30.0 + 8.0,
        Block::Raw(_) => 0.0,
    }
}

/// Does this block force a page break (a `w:br` of type page)?
fn has_page_break(b: &Block) -> bool {
    matches!(b, Block::Paragraph(p) if p.content.iter().any(|i| matches!(i, Inline::Break(docxcore::model::BreakKind::Page))))
}

/// Group top-level block indices into pages by accumulated estimated height and
/// hard page breaks. Returns `[start, end)` block ranges, one per page.
fn paginate(blocks: &[Block], content_h: f32, content_w: f32) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut start = 0usize;
    let mut acc = 0.0_f32;
    for (i, b) in blocks.iter().enumerate() {
        let bh = block_height_est(b, content_w);
        if acc + bh > content_h && i > start {
            pages.push((start, i));
            start = i;
            acc = 0.0;
        }
        acc += bh;
        if has_page_break(b) {
            pages.push((start, i + 1));
            start = i + 1;
            acc = 0.0;
        }
    }
    if start < blocks.len() {
        pages.push((start, blocks.len()));
    }
    if pages.is_empty() {
        pages.push((0, blocks.len()));
    }
    pages
}

/// Flow blocks into `ncols` columns per page (newspaper columns). Each column
/// holds `content_h` worth of content estimated at the per-column width; a page
/// is `ncols` such columns. Returns one `Vec<(start,end)>` (the columns) per page.
fn paginate_cols(blocks: &[Block], content_h: f32, col_w: f32, ncols: usize) -> Vec<Vec<(usize, usize)>> {
    let mut pages: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut page: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    let mut acc = 0.0_f32;
    let flush_col = |page: &mut Vec<(usize, usize)>, pages: &mut Vec<Vec<(usize, usize)>>, start: usize, i: usize| {
        page.push((start, i));
        if page.len() >= ncols {
            pages.push(std::mem::take(page));
        }
    };
    for (i, b) in blocks.iter().enumerate() {
        let bh = block_height_est(b, col_w);
        if acc + bh > content_h && i > start {
            flush_col(&mut page, &mut pages, start, i);
            start = i;
            acc = 0.0;
        }
        acc += bh;
        if has_page_break(b) {
            flush_col(&mut page, &mut pages, start, i + 1);
            // A hard break ends the current column *and* the page.
            if !page.is_empty() {
                pages.push(std::mem::take(&mut page));
            }
            start = i + 1;
            acc = 0.0;
        }
    }
    if start < blocks.len() {
        flush_col(&mut page, &mut pages, start, blocks.len());
    }
    if !page.is_empty() {
        pages.push(page);
    }
    if pages.is_empty() {
        pages.push(vec![(0, blocks.len())]);
    }
    pages
}

fn list_markers(body: &[Block]) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(body.len());
    let mut counts: Vec<u32> = Vec::new();
    for b in body {
        let marker = match b {
            Block::Paragraph(p) if p.props.num_id.is_some() => {
                let ilvl = p.props.ilvl as usize;
                if p.props.num_id == Some(NUM_DECIMAL) {
                    if counts.len() <= ilvl {
                        counts.resize(ilvl + 1, 0);
                    }
                    counts.truncate(ilvl + 1); // returning to a shallower level restarts deeper ones
                    counts[ilvl] += 1;
                    Some(format!("{}. ", counts[ilvl]))
                } else {
                    counts.clear();
                    Some(if ilvl % 2 == 1 { "\u{25E6} ".to_string() } else { "\u{2022} ".to_string() })
                }
            }
            _ => {
                counts.clear();
                None
            }
        };
        out.push(marker);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn paragraph_el(p: &Paragraph, mut caret: Option<usize>, sel: Option<(usize, usize)>, marker: Option<&str>, click: Option<Click>, marks: bool, zoom: f32, pal: Pal, meas: Option<&Measurer>, hf_width: Option<f32>) -> AnyElement {
    use docxcore::model::TabAlign;
    let base = zoom
        * match p.props.heading_level {
            Some(1) => 26.0,
            Some(2) => 22.0,
            Some(3) => 19.0,
            Some(4) => 17.0,
            Some(_) => 15.0,
            None => 14.5,
        };
    let is_heading = p.props.heading_level.is_some();
    let mut spans: Vec<AnyElement> = Vec::new();
    let mut idx = 0usize;

    // Tab-stop geometry. `x` tracks the running pixel width from the row's left
    // edge; a tab advances it to the next stop measured from the text margin. The
    // row's left edge sits `pad_l` px in from the margin (the paragraph indent).
    let pad_l = zoom * ((p.props.indent.max(0) as f32) / 15.0 + p.props.ilvl.max(0) as f32 * 20.0);
    let interval = zoom * 720.0 / 15.0; // Word's default tab stop: every 1/2"
    let mut customs: Vec<(f32, TabAlign)> = p.props.tabs.iter().map(|t| (zoom * t.pos as f32 / 15.0, t.align)).collect();
    // Header/footer paragraphs with no explicit tabs get Word's implicit centre +
    // right stops, so a centred title / right-aligned page number lands correctly.
    if customs.is_empty() {
        if let Some(w) = hf_width {
            customs.push((w * 0.5, TabAlign::Center));
            // A few px inside the edge so a right-aligned segment ending exactly at
            // the content width doesn't trip the row's flex-wrap onto a new line.
            customs.push(((w - 4.0).max(0.0), TabAlign::Right));
        }
    }
    let max_custom = customs.iter().map(|(c, _)| *c).fold(f32::NEG_INFINITY, f32::max);
    let mut x = 0.0_f32;
    // Only paragraphs that actually contain a tab need per-run width measurement.
    let has_tab = p.content.iter().any(|i| matches!(i, Inline::Tab(_)));
    let m = meas.filter(|_| has_tab);
    // Width of a run's text at its effective size (matching emit_words' sizing).
    let run_w = |r: &docxcore::model::Run| -> f32 {
        match m {
            Some(m) => {
                let sz = r.props.size_half_pts.map(|h| h as f32 / 2.0 * 1.333).unwrap_or(base);
                m.width(&r.text, sz, r.props.bold, r.props.italic)
            }
            None => 0.0,
        }
    };
    // The rendered width of a single inline (for tracking x and looking ahead to
    // size centre/right tabs). Inlines with no measurable text contribute 0.
    let inline_w = |it: &Inline| -> f32 {
        match it {
            Inline::Run(r) => run_w(r),
            Inline::Hyperlink(h) => h.runs.iter().map(run_w).sum(),
            Inline::Field { text, .. } => m.map(|m| m.width(if text.is_empty() { "[field]" } else { text }, base, false, false)).unwrap_or(0.0),
            Inline::FootnoteRef { id, .. } => m.map(|m| m.width(&id.to_string(), base * 0.72, false, false)).unwrap_or(0.0),
            _ => 0.0,
        }
    };
    // Total width of content from index `from` up to the next tab / break / end —
    // the segment a centre/right tab must position.
    let seg_width = |from: usize| -> f32 {
        p.content[from..].iter().take_while(|it| !matches!(it, Inline::Tab(_) | Inline::Break(_))).map(&inline_w).sum()
    };
    // The next tab stop strictly past `xm` (twips-px from the margin) and its
    // alignment: the nearest custom stop, else the default 1/2" grid (defaults
    // are suppressed up to the last custom stop, as Word does).
    let next_stop = |xm: f32| -> (f32, TabAlign) {
        let mut pos = f32::INFINITY;
        let mut align = TabAlign::Left;
        for &(c, a) in &customs {
            if c > xm + 0.5 && c < pos {
                pos = c;
                align = a;
            }
        }
        let lo = xm.max(if max_custom.is_finite() { max_custom } else { 0.0 });
        let mut d = ((lo / interval).floor() + 1.0) * interval;
        while d <= xm + 0.5 {
            d += interval;
        }
        if d < pos {
            pos = d;
            align = TabAlign::Left;
        }
        (pos, align)
    };

    if let Some(m) = marker {
        // The marker isn't document content — render it plain (non-clickable) so it
        // never maps clicks to bogus offsets.
        spans.push(div().text_size(px(base)).text_color(pal.dim).child(SharedString::from(m.to_string())).into_any_element());
        if let Some(ms) = meas.filter(|_| has_tab) {
            x += ms.width(m, base, false, false);
        }
    }
    for i in 0..p.content.len() {
        let inline = &p.content[i];
        match inline {
            Inline::Run(r) => {
                emit_run(&mut spans, &r.text, &r.props, base, false, &mut idx, &mut caret, sel, click, pal);
                x += run_w(r);
            }
            Inline::Hyperlink(h) => {
                for r in &h.runs {
                    emit_run(&mut spans, &r.text, &r.props, base, true, &mut idx, &mut caret, sel, click, pal);
                    x += run_w(r);
                }
            }
            Inline::Tab(_) => {
                // Advance to the next stop; centre/right stops position the segment
                // that follows (up to the next tab) so it centres on / ends at it.
                let xm = pad_l + x;
                let (stop, align) = next_stop(xm);
                let w = match align {
                    TabAlign::Left => stop - xm,
                    TabAlign::Right => stop - seg_width(i + 1) - xm,
                    TabAlign::Center => stop - seg_width(i + 1) / 2.0 - xm,
                }
                .max(3.0);
                emit_tab(&mut spans, &mut idx, &mut caret, sel, click, marks, base, w, pal);
                x += w;
            }
            Inline::Break(_) => {
                emit_break(&mut spans, &mut idx, &mut caret);
                x = 0.0; // a hard break restarts the line
            }
            Inline::Raw(xml) => {
                // Comment reference → a small badge; other raw XML (range markers,
                // bookmarks, …) is invisible content and renders nothing.
                if xml.contains("commentReference") {
                    spans.push(
                        div()
                            .px(px(3.))
                            .rounded(px(3.))
                            .bg(hsla_u(0xF2C744))
                            .text_size(px(9.))
                            .text_color(rgb(0x1a1a1a))
                            .relative()
                            .top(px(-(base * 0.35)))
                            .child("\u{1F4AC}")
                            .into_any_element(),
                    );
                }
            }
            Inline::Field { text, .. } => {
                // Show the field's cached value with a subtle shade so it reads as a
                // field, not plain text.
                let shown = if text.is_empty() { "[field]".to_string() } else { text.clone() };
                spans.push(div().px(px(2.)).rounded_sm().bg(pal.panel).text_size(px(base)).text_color(pal.fg).child(SharedString::from(shown)).into_any_element());
                x += inline_w(inline);
            }
            Inline::FootnoteRef { id, .. } => {
                // A superscript note number in the brand colour.
                spans.push(div().text_size(px(base * 0.72)).text_color(hsla_u(BRAND)).relative().top(px(-(base * 0.35))).child(SharedString::from(id.to_string())).into_any_element());
                x += inline_w(inline);
            }
            Inline::Equation { text, latex, .. } => {
                // Math renders as its Unicode form (falling back to the LaTeX),
                // in a faint math tint — docxy can't typeset, but the equation
                // is visible and editable-as-text rather than a "[equation]" stub.
                let shown = if !text.is_empty() { text.clone() } else { latex.clone().unwrap_or_default() };
                spans.push(div().px(px(3.)).rounded_sm().bg(Hsla { a: 0.14, ..hsla_u(BRAND) }).text_size(px(base)).text_color(pal.fg).italic().child(SharedString::from(shown)).into_any_element());
            }
            Inline::SmartArt { text, .. } => {
                // docxy can't draw the diagram graphics, but the node labels are
                // extracted from the diagram data — show them as a captioned box
                // (matching the terminal editor) rather than a "[diagram]" stub.
                let mut box_el = v_flex()
                    .my(px(2.))
                    .px(px(8.))
                    .py(px(4.))
                    .gap(px(1.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .child(div().text_size(px(9.)).text_color(hsla_u(BRAND)).child("\u{25C6} SmartArt"));
                if text.is_empty() {
                    box_el = box_el.child(div().text_size(px(base * 0.9)).text_color(pal.dim).child("(no text)"));
                } else {
                    for node in text {
                        box_el = box_el.child(div().text_size(px(base * 0.9)).text_color(pal.fg).child(SharedString::from(format!("\u{2022} {node}"))));
                    }
                }
                spans.push(box_el.into_any_element());
            }
            other => {
                let tag = match other {
                    Inline::Chart { .. } => "[chart]",
                    Inline::TextBox { .. } => "[textbox]",
                    _ => "[image]",
                };
                spans.push(div().px_1().rounded_sm().bg(pal.panel).text_size(px(12.)).text_color(pal.dim).child(tag).into_any_element());
            }
        }
    }
    if caret.is_some() {
        spans.push(caret_bar());
    }
    // A pilcrow at the paragraph end when formatting marks are shown.
    if marks {
        spans.push(div().text_size(px(base)).text_color(pal.dim).child("\u{00B6}").into_any_element());
    }
    let has_border = p.props.borders.bottom.is_some();
    // Line spacing (auto-rule multiple; exact/atLeast fall back to single here).
    let line_mult = p.props.spacing.line_multiple().unwrap_or(1.0).clamp(0.5, 4.0);
    let line_h = base * 1.35 * line_mult;
    let mut row = h_flex().w_full().flex_wrap().min_h(px(line_h.max(base + 6.))).line_height(px(line_h));
    row = match p.props.align {
        Align::Center => row.justify_center(),
        Align::Right => row.justify_end(),
        _ => row,
    };
    // Leading indent: explicit paragraph indent (twips → px at ~96dpi) plus a step
    // per list nesting level.
    let pad = zoom * ((p.props.indent.max(0) as f32) / 15.0 + p.props.ilvl.max(0) as f32 * 20.0);
    let pad_r = zoom * (p.props.indent_right.max(0) as f32) / 15.0;
    // The whole paragraph area is a click fallback (empty space past the text, the
    // indent gutter) that drops the caret at the paragraph end. Word clicks fire
    // first and stop propagation, so this only runs on a "past the text" click.
    let para_end = idx;
    // Space before / after the paragraph (`w:spacing` before/after, twips → px).
    let sp_before = zoom * (p.props.spacing.before.unwrap_or(0).max(0) as f32) / 15.0;
    let sp_after = zoom * (p.props.spacing.after.unwrap_or(0).max(0) as f32) / 15.0;
    v_flex()
        .w_full()
        .py_0p5()
        .when(sp_before > 0.5, |d| d.pt(px(sp_before)))
        .when(sp_after > 0.5, |d| d.pb(px(sp_after)))
        .pl(px(pad))
        .pr(px(pad_r))
        .when(is_heading, |d| d.mt_2())
        .when(has_border, |d| d.border_b_1().border_color(pal.fg).pb_1())
        .when_some(click, |d, c| {
            let ent = c.ent.clone();
            let path = c.path.to_vec();
            d.cursor_text().on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                let extend = ev.modifiers.shift;
                ent.update(cx, |this, cx| this.set_caret(path.clone(), para_end, extend, window, cx));
            })
        })
        .child(row.children(spans))
        .into_any_element()
}

fn table_el(t: &Table, path: &[usize], ctx: RenderCtx) -> AnyElement {
    let mut rows = Vec::new();
    for (ri, row) in t.rows.iter().enumerate() {
        let mut cells = Vec::new();
        for (ci, cell) in row.cells.iter().enumerate() {
            let inner: Vec<AnyElement> = cell
                .blocks
                .iter()
                .enumerate()
                .map(|(k, b)| {
                    let mut cp = path.to_vec();
                    cp.extend_from_slice(&[ri, ci, k]);
                    block_el(b, cp, None, ctx)
                })
                .collect();
            cells.push(v_flex().flex_1().px_2().py_1().border_1().border_color(ctx.pal.border).children(inner).into_any_element());
        }
        rows.push(h_flex().w_full().children(cells).into_any_element());
    }
    v_flex().w_full().my_2().children(rows).into_any_element()
}

/// Render one block at absolute `path`, wiring caret/selection/click from `ctx`.
fn block_el(b: &Block, path: Vec<usize>, marker: Option<&str>, ctx: RenderCtx) -> AnyElement {
    match b {
        Block::Paragraph(p) => {
            let caret = (ctx.active && ctx.caret_path == path.as_slice()).then_some(ctx.caret_off);
            let sel = ctx.active.then(|| ctx.spans.iter().find(|(pp, _, _)| pp.as_slice() == path.as_slice()).map(|(_, s, e)| (*s, *e))).flatten();
            let click = ctx.active.then_some(Click { ent: ctx.ent, path: &path });
            paragraph_el(p, caret, sel, marker, click, ctx.marks, ctx.zoom, ctx.pal, Some(ctx.meas), ctx.hf_width)
        }
        Block::Table(t) => table_el(t, &path, ctx),
        Block::Raw(_) => div().h(px(0.)).into_any_element(),
    }
}

// ---- chrome: ribbon + backstage --------------------------------------------

impl Docxy {
    fn ribbon_tabs(&self, fg: Hsla, dim: Hsla, panel: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let names = ["File", "Home", "Insert", "Review", "View"];
        let mut strip = h_flex().w_full().items_end().gap_1().px_2().pt_1().bg(panel);
        for (i, name) in names.iter().enumerate() {
            let is_file = i == 0;
            let this_tab = match i {
                1 => Some(RibbonTab::Home),
                2 => Some(RibbonTab::Insert),
                3 => Some(RibbonTab::Review),
                4 => Some(RibbonTab::View),
                _ => None,
            };
            let active = !self.backstage && this_tab == Some(self.ribbon_tab);
            let tab_key = ["F", "H", "N", "R", "W"][i];
            let show_kt = self.keytips == KeyTip::Tabs;
            strip = strip.child(
                div()
                    .id(("rtab", i))
                    .relative()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .rounded_t_sm()
                    .text_size(px(12.))
                    .when(!is_file, |d| d.hover(|d| d.bg(Hsla { a: 0.10, ..fg })))
                    .when(is_file, |d| d.bg(rgb(BRAND)).text_color(rgb(FILE_FG)).font_weight(FontWeight::BOLD).rounded_t_sm())
                    .when(active, |d| d.text_color(rgb(BRAND)).border_b_2().border_color(rgb(BRAND)))
                    .when(!active && !is_file, |d| d.text_color(fg))
                    .child(*name)
                    .when(show_kt, |d| d.child(keytip_badge(tab_key)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if is_file {
                            this.backstage = true;
                            this.bs_new = false;
                            cx.notify();
                        } else if let Some(t) = this_tab {
                            this.ribbon_tab = t;
                            this.refocus(window, cx);
                        }
                    })),
            );
        }
        // Contextual Table Tools tab — only while the caret is in a table. It reads
        // with a coloured accent (Word shows contextual tabs tinted).
        if self.caret_table().is_some() {
            let active = !self.backstage && self.ribbon_tab == RibbonTab::Table;
            let accent = hsla_u(0xC0_5B_2E); // a warm contextual accent
            strip = strip.child(
                div()
                    .id(("rtab", 99usize))
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .rounded_t_sm()
                    .text_size(px(12.))
                    .text_color(accent)
                    .hover(|d| d.bg(Hsla { a: 0.10, ..accent }))
                    .when(active, |d| d.border_b_2().border_color(accent).font_weight(FontWeight::BOLD))
                    .child("Table")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.ribbon_tab = RibbonTab::Table;
                        this.refocus(window, cx);
                    })),
            );
        }
        // right side: the ribbon collapse/expand chevron
        strip = strip.child(div().flex_1());
        strip = strip.child(
            div()
                .id("ribbon-min")
                .px_2()
                .py_1()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(fg)
                .child(if self.ribbon_min { "\u{2304}" } else { "\u{2303}" })
                .tooltip(|w, cx| Tooltip::new("Collapse the ribbon  \u{00b7}  Ctrl+F1").build(w, cx))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.ribbon_min = !this.ribbon_min;
                    this.refocus(window, cx);
                })),
        );
        let _ = dim;
        strip.into_any_element()
    }

    /// Dispatch a ribbon command to the engine / app, then refocus the document.
    fn launch_msg(&mut self, msg: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.status = SharedString::from(msg.to_string());
        }
        self.refocus(window, cx);
    }

    /// The right-click context menu (clipboard + quick formatting), anchored at the
    /// click position, over a full-window backdrop that dismisses it.
    fn context_menu_el(&self, at: Point<Pixels>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let item = |cx: &mut Context<Self>, id: &'static str, label: &'static str, icon: &'static str, act: Act| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .hover(|d| d.bg(pal.hover))
                .when(!icon.is_empty(), |d| d.child(icon_svg(icon, 14., pal.fg)))
                .when(icon.is_empty(), |d| d.child(div().w(px(14.))))
                .child(SharedString::from(label))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.context_menu = None;
                    this.dispatch(act, window, cx);
                }))
        };
        let sep = || div().h(px(1.)).mx_2().my_0p5().bg(pal.border);
        let menu = v_flex()
            .absolute()
            .left(at.x)
            .top(at.y)
            .w(px(200.))
            .py_1()
            .rounded_md()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .shadow_lg()
            .child(item(cx, "cm-cut", "Cut", "cut", Act::Cut))
            .child(item(cx, "cm-copy", "Copy", "copy", Act::Copy))
            .child(item(cx, "cm-paste", "Paste", "paste", Act::Paste))
            .child(sep())
            .child(item(cx, "cm-bold", "Bold", "bold", Act::Bold))
            .child(item(cx, "cm-italic", "Italic", "italic", Act::Italic))
            .child(item(cx, "cm-underline", "Underline", "underline", Act::Underline))
            .child(sep())
            .child(item(cx, "cm-comment", "New Comment", "comment-add", Act::NewComment));
        // Full-window backdrop to catch outside clicks / right-clicks.
        div()
            .id("cm-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _w, cx| {
                this.context_menu = None;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _w, cx| {
                this.context_menu = None;
                cx.notify();
            }))
            .child(menu)
            .into_any_element()
    }

    /// The floating mini formatting toolbar, anchored just above the selection.
    fn mini_bar_el(&self, at: Point<Pixels>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let btn = |cx: &mut Context<Self>, id: &'static str, icon: &'static str, act: Act| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(3.))
                .cursor_pointer()
                .when(self.act_active(act), |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }))
                .hover(|d| d.bg(pal.hover))
                .child(icon_svg(icon, 15., pal.fg))
                .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
        };
        h_flex()
            .absolute()
            .left(at.x)
            .top((at.y - px(36.)).max(px(2.)))
            .items_center()
            .gap(px(1.))
            .p_0p5()
            .rounded_md()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(btn(cx, "mb-b", "bold", Act::Bold))
            .child(btn(cx, "mb-i", "italic", Act::Italic))
            .child(btn(cx, "mb-u", "underline", Act::Underline))
            .child(btn(cx, "mb-s", "strikethrough", Act::Strike))
            .child(div().w(px(1.)).h(px(18.)).mx_0p5().bg(pal.border))
            .child(btn(cx, "mb-grow", "font-increase", Act::Grow))
            .child(btn(cx, "mb-shrink", "font-decrease", Act::Shrink))
            .child(btn(cx, "mb-color", "text-color", Act::FontColor))
            .child(btn(cx, "mb-hl", "highlight", Act::Highlight))
            .into_any_element()
    }

    /// Handle a letter pressed while KeyTips are showing: pick a tab (Tabs level)
    /// or run a command (Commands level).
    fn keytip_input(&mut self, c: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.keytips {
            KeyTip::Tabs => {
                if c.eq_ignore_ascii_case("F") {
                    self.keytips = KeyTip::Off;
                    self.backstage = true;
                    self.bs_new = false;
                    return cx.notify();
                }
                let ribbon = docxy_ribbon();
                if let Some(i) = ribbon.tabs.iter().position(|t| t.key_tip.eq_ignore_ascii_case(c)) {
                    self.ribbon_tab = match i {
                        0 => RibbonTab::Home,
                        1 => RibbonTab::Insert,
                        2 => RibbonTab::Review,
                        _ => RibbonTab::View,
                    };
                    self.keytips = KeyTip::Commands;
                } else if c.eq_ignore_ascii_case("T") && self.caret_table().is_some() {
                    self.ribbon_tab = RibbonTab::Table;
                    self.keytips = KeyTip::Commands;
                } else {
                    self.keytips = KeyTip::Off;
                }
                cx.notify();
            }
            KeyTip::Commands => {
                let ribbon = docxy_ribbon();
                let table = table_tab();
                let tab = if self.ribbon_tab == RibbonTab::Table { &table } else { &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)] };
                let act = tab_keytip_cmd(tab, c);
                self.keytips = KeyTip::Off;
                if let Some(a) = act {
                    self.dispatch(a, window, cx);
                } else {
                    cx.notify();
                }
            }
            KeyTip::Off => {}
        }
    }

    fn dispatch(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        use Act::*;
        match act {
            Cut => self.do_copy(true, window, cx),
            Copy => self.do_copy(false, window, cx),
            Paste => self.do_paste(window, cx),
            LaunchFont => self.launch_msg("Font — advanced dialog coming soon", window, cx),
            LaunchParagraph => self.launch_msg("Paragraph — advanced dialog coming soon", window, cx),
            Find => self.toggle_find(window, cx),
            FontColor => self.toggle_picker(PickKind::Color, window, cx),
            Highlight => self.toggle_picker(PickKind::Highlight, window, cx),
            FontName => self.toggle_picker(PickKind::FontName, window, cx),
            FontSize => self.toggle_picker(PickKind::FontSize, window, cx),
            NewComment => self.start_comment(window, cx),
            ShowHide => {
                self.show_marks = !self.show_marks;
                self.refocus(window, cx);
            }
            ToggleComments => {
                self.show_comments = !self.show_comments;
                self.refocus(window, cx);
            }
            ToggleNav => {
                self.show_nav = !self.show_nav;
                self.refocus(window, cx);
            }
            DarkMode => self.cycle_theme(window, cx),
            AutoHideRibbon => {
                self.ribbon_min = !self.ribbon_min;
                self.refocus(window, cx);
            }
            InsertField => self.toggle_picker(PickKind::Field, window, cx),
            InsertTable => self.toggle_picker(PickKind::Table, window, cx),
            InsertSymbol => self.toggle_picker(PickKind::Symbol, window, cx),
            InsertEquation => self.toggle_picker(PickKind::Equation, window, cx),
            LineSpacing => self.toggle_picker(PickKind::LineSpacing, window, cx),
            EditHeader => self.enter_hf(true, "default", window, cx),
            EditFooter => self.enter_hf(false, "default", window, cx),
            PageNumber => self.insert_field("PAGE", "1", window, cx),
            Columns => self.cycle_columns(window, cx),
            Hyphenation => self.toggle_hyphenation(window, cx),
            RowAbove | RowBelow | ColLeft | ColRight | DelRow | DelCol | DelTable => self.table_op(act, window, cx),
            PrintLayout => {
                self.page_view = !self.page_view;
                self.refocus(window, cx);
            }
            ToggleRuler => {
                self.show_ruler = !self.show_ruler;
                self.refocus(window, cx);
            }
            PageBreak => self.insert_page_break(window, cx),
            ToggleNotes => {
                self.show_notes = !self.show_notes;
                self.refocus(window, cx);
            }
            _ => self.with_editor(window, cx, |e| match act {
                Bold => e.toggle_bold(),
                Italic => e.toggle_italic(),
                Underline => e.toggle_underline(),
                Strike => e.toggle_strike(),
                Super => e.toggle_vert_align(VertAlign::Superscript),
                Sub => e.toggle_vert_align(VertAlign::Subscript),
                Grow => e.resize_font(2),
                Shrink => e.resize_font(-2),
                AlignL => e.set_align(Align::Left),
                AlignC => e.set_align(Align::Center),
                AlignR => e.set_align(Align::Right),
                AlignJ => e.set_align(Align::Justify),
                Normal => e.set_para_style(None),
                // No Spacing: Word's body style with single spacing and no space
                // before/after. Modelled as Normal + explicit zeroed spacing.
                NoSpacing => {
                    e.set_para_style(None);
                    e.set_space_before(Some(0));
                    e.set_space_after(Some(0));
                    e.set_line_spacing(240, "auto");
                }
                H1 => e.set_para_style(Some("Heading1")),
                H2 => e.set_para_style(Some("Heading2")),
                H3 => e.set_para_style(Some("Heading3")),
                HRule => e.insert_hrule(),
                SelectAll => e.select_all(),
                Case => e.cycle_case(),
                // Toggle: if every selected paragraph is already in this list, drop
                // it; otherwise apply it.
                Bullets => e.set_list((!e.all_in_list(NUM_BULLET)).then_some(NUM_BULLET)),
                Numbers => e.set_list((!e.all_in_list(NUM_DECIMAL)).then_some(NUM_DECIMAL)),
                IndentInc => e.change_indent(720),
                IndentDec => e.change_indent(-720),
                Sort => e.sort_paragraphs(),
                ParaBorders => {
                    let has = e.caret_para_props().borders.bottom.is_some();
                    let b = if has { ParBorders::default() } else { ParBorders { top: None, bottom: Some(BorderKind::Single) } };
                    e.set_para_border(b);
                }
                Title => e.set_para_style(Some("Title")),
                Subtitle => e.set_para_style(Some("Subtitle")),
                ClearFmt => e.clear_run_formatting(),
                Cut | Copy | Paste | LaunchFont | LaunchParagraph | Find | FontColor | Highlight | FontName | FontSize | NewComment | ShowHide | ToggleComments | ToggleNav | DarkMode | AutoHideRibbon | InsertField | PageBreak | ToggleNotes | InsertTable | InsertSymbol | InsertEquation | LineSpacing | EditHeader | EditFooter | PageNumber | Columns | Hyphenation | RowAbove | RowBelow | ColLeft | ColRight | DelRow | DelCol | DelTable | PrintLayout | ToggleRuler => {}
            }),
        }
    }

    /// The ribbon body for the active tab, rendered from the shared ribbonspec
    /// model with Fluent icons — and RESPONSIVE: as `width` drops, control labels
    /// are dropped first, then the lowest-`priority` groups collapse into an
    /// overflow indicator (Office-style scaling driven by ribbonspec::priority).
    fn ribbon_body(&self, width: f32, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ribbon = docxy_ribbon();
        let ctx_tab = table_tab();
        let tab = if self.ribbon_tab == RibbonTab::Table { &ctx_tab } else { &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)] };
        let avail = (width - 28.0).max(120.0);

        // 1) drop control labels if the full layout overflows.
        let icon_only = tab.groups.iter().map(|g| group_est(g, false)).sum::<f32>() > avail;
        // 2) collapse lowest-priority groups until what remains fits.
        let mut shown: Vec<usize> = (0..tab.groups.len()).collect();
        loop {
            let total: f32 = shown.iter().map(|&i| group_est(&tab.groups[i], icon_only)).sum();
            if total <= avail || shown.len() <= 1 {
                break;
            }
            let victim = *shown.iter().min_by_key(|&&i| tab.groups[i].priority).unwrap();
            shown.retain(|&i| i != victim);
        }
        let hidden = tab.groups.len() - shown.len();

        let mut groups: Vec<AnyElement> =
            shown.iter().map(|&i| self.render_group(&tab.groups[i], icon_only, pal, cx)).collect();
        if hidden > 0 {
            groups.push(
                v_flex()
                    .items_center()
                    .justify_center()
                    .h_full()
                    .px_2()
                    .gap_1()
                    .text_color(pal.dim)
                    .child(div().text_size(px(18.)).child("\u{22EF}"))
                    .child(div().text_size(px(9.)).child(SharedString::from(format!("{hidden} more"))))
                    .into_any_element(),
            );
        }
        h_flex().w_full().h(px(98.)).items_stretch().px_1().bg(pal.panel).border_b_1().border_color(pal.border).children(groups).into_any_element()
    }

    /// One spreadsheet-ribbon button: a glyph/label that runs a `SheetAct`.
    /// A small icon-only Home button (the two-row Font/Alignment buttons).
    fn sheet_ib(&self, icon: &'static str, act: SheetAct, on: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(ElementId::Name(format!("sib-{icon}").into()))
            .flex().items_center().justify_center()
            .size(px(22.))
            .rounded(px(3.))
            .cursor_pointer()
            .when(on, |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }))
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(icon, 15., pal.fg))
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(act, window, cx)))
            .into_any_element()
    }

    /// A small glyph/text Home button (number formats, wrap, merge, …).
    fn sheet_gb(&self, glyph: &'static str, act: SheetAct, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(ElementId::Name(format!("sgb-{glyph}").into()))
            .flex().items_center().justify_center()
            .min_w(px(22.)).h(px(22.)).px_1()
            .rounded(px(3.))
            .cursor_pointer()
            .text_size(px(12.)).text_color(pal.fg)
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(glyph)
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(act, window, cx)))
            .into_any_element()
    }

    /// A small icon+label row (Clipboard Cut/Copy, Editing AutoSum/Fill/Clear).
    fn sheet_rb(&self, icon: Option<&'static str>, label: &'static str, act: SheetAct, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(ElementId::Name(format!("srb-{label}").into()))
            .flex().items_center().gap_1p5().px_1().h(px(20.))
            .rounded(px(3.))
            .cursor_pointer()
            .hover(|d| d.bg(pal.hover))
            .when_some(icon, |d, ic| d.child(icon_svg(ic, 14., pal.fg)))
            .child(div().text_size(px(11.)).text_color(pal.fg).child(label))
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(act, window, cx)))
            .into_any_element()
    }

    /// A large icon-over-label Home button (Paste, Styles, Cells, Editing). The
    /// label wraps at a word boundary (never mid-word) and the button sizes to it.
    fn sheet_lb(&self, icon: Option<&'static str>, label: &'static str, act: SheetAct, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let mut lines = v_flex().items_center();
        for ln in label_lines(label) {
            lines = lines.child(div().text_size(px(10.)).text_color(pal.fg).child(SharedString::from(ln)));
        }
        div()
            .id(ElementId::Name(format!("slb-{label}").into()))
            .flex().flex_col().items_center().justify_center().gap_0p5()
            .min_w(px(40.)).h_full().px_1p5()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .when_some(icon, |d, ic| d.child(icon_svg(ic, 22., pal.fg)))
            .child(lines)
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(act, window, cx)))
            .into_any_element()
    }

    /// A combo-box display (font name/size, number format) — inert for now.
    fn sheet_combo(&self, value: &'static str, wide: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(ElementId::Name(format!("scombo-{value}").into()))
            .flex().items_center().justify_between().gap_1()
            .w(px(if wide { 108. } else { 50. })).h(px(22.)).px_1p5()
            .rounded(px(3.))
            .border_1().border_color(pal.border).bg(pal.panel)
            .cursor_pointer()
            .hover(|d| d.border_color(hsla_u(BRAND)))
            .child(div().text_size(px(11.)).text_color(pal.fg).overflow_hidden().child(value))
            .child(div().text_size(px(8.)).text_color(pal.dim).child("\u{25BE}"))
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(SheetAct::Todo, window, cx)))
            .into_any_element()
    }

    /// The Number-group format combo: shows the selection's current format name
    /// and toggles the format-picker strip.
    fn sheet_numfmt_combo(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let name = self.active_numfmt_name();
        div()
            .id("numfmt-combo")
            .flex().items_center().justify_between().gap_1()
            .w(px(108.)).h(px(22.)).px_1p5()
            .rounded(px(3.))
            .border_1().border_color(pal.border).bg(pal.panel)
            .cursor_pointer()
            .hover(|d| d.border_color(hsla_u(BRAND)))
            .child(div().text_size(px(11.)).text_color(pal.fg).overflow_hidden().child(name))
            .child(div().text_size(px(8.)).text_color(pal.dim).child("\u{25BE}"))
            .on_click(cx.listener(|this, _, _w, cx| {
                this.sheet_numfmt_open = !this.sheet_numfmt_open;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The format-picker strip shown under the ribbon while the Number dropdown is
    /// open: each option applies its code to the selection and shows a live sample.
    fn sheet_numfmt_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::{format_with, CellValue, Xf};
        let d1904 = self.active_sheet().is_some_and(|v| v.pkg.workbook.date1904);
        let mut row = h_flex().w_full().items_center().flex_wrap().gap_1p5().px_3().py_1().bg(pal.panel).border_b_1().border_color(pal.border);
        row = row.child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(70.)).child("Number format"));
        for (label, code) in NUM_FORMATS {
            // Live sample of 1234.5 in this format (dates/text show a fixed sample).
            let sample = if code.is_empty() {
                "1234.5".to_string()
            } else if code == "@" {
                "abc".to_string()
            } else if code.contains('y') || code.contains('h') {
                format_with(&Xf { code: Some(code.to_string()), ..Xf::default() }, &CellValue::Number(45658.5), d1904)
            } else {
                format_with(&Xf { code: Some(code.to_string()), ..Xf::default() }, &CellValue::Number(1234.5), d1904)
            };
            row = row.child(
                div()
                    .id(ElementId::Name(format!("nf-{label}").into()))
                    .flex().flex_col().px_2().py_1().rounded(px(3.)).cursor_pointer()
                    .border_1().border_color(pal.border).bg(hsla_u(0xffffff))
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(pal.fg).child(label))
                    .child(div().text_size(px(10.)).text_color(pal.dim).child(SharedString::from(sample)))
                    .on_click(cx.listener(move |this, _, _w, cx| this.sheet_apply_numfmt(code, cx))),
            );
        }
        row.into_any_element()
    }

    /// The consolidated "Format Cells" modal — one place for Number, Font, Fill,
    /// Alignment and Border, applied live to the selection via the existing xf
    /// helpers. A backdrop dismisses it.
    fn sheet_format_panel(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::Align;
        let xf = self.active_xf();
        let cur_fmt = self.active_numfmt_name();
        let heading = |t: &str| div().text_size(px(10.)).font_weight(FontWeight::BOLD).text_color(pal.dim).child(t.to_string());

        // Number formats.
        let mut number = h_flex().flex_wrap().gap_1();
        for (label, code) in NUM_FORMATS {
            let active = label == cur_fmt;
            number = number.child(
                div()
                    .id(ElementId::Name(format!("fmt-nf-{label}").into()))
                    .px_2().py(px(3.)).rounded_sm().cursor_pointer().text_size(px(11.))
                    .bg(if active { hsla_u(BRAND) } else { pal.panel })
                    .text_color(if active { hsla_u(0xffffff) } else { pal.fg })
                    .border_1().border_color(pal.border)
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _w, cx| this.sheet_apply_numfmt(code, cx))),
            );
        }

        // Toggle button (Bold/Italic/Align/Border).
        let tbtn = |id: &str, label: &str, active: bool| {
            div()
                .id(ElementId::Name(format!("fmt-{id}").into()))
                .px_2p5().py(px(3.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(if active { hsla_u(BRAND) } else { pal.panel })
                .text_color(if active { hsla_u(0xffffff) } else { pal.fg })
                .border_1().border_color(pal.border)
                .hover(|d| d.border_color(hsla_u(BRAND)))
                .child(label.to_string())
        };
        let font_row = h_flex().gap_1p5()
            .child(tbtn("bold", "B", xf.bold).on_click(cx.listener(|this, _, _w, cx| this.sheet_toggle_bold(cx))))
            .child(tbtn("italic", "I", xf.italic).on_click(cx.listener(|this, _, _w, cx| this.sheet_toggle_italic(cx))));
        let align_row = h_flex().gap_1p5()
            .child(tbtn("al", "Left", xf.align == Align::Left).on_click(cx.listener(|this, _, _w, cx| this.sheet_align(Align::Left, cx))))
            .child(tbtn("ac", "Center", xf.align == Align::Center).on_click(cx.listener(|this, _, _w, cx| this.sheet_align(Align::Center, cx))))
            .child(tbtn("ar", "Right", xf.align == Align::Right).on_click(cx.listener(|this, _, _w, cx| this.sheet_align(Align::Right, cx))));
        let border_row = h_flex()
            .child(tbtn("border", "Box border", xf.border).on_click(cx.listener(|this, _, _w, cx| this.sheet_toggle_border(cx))));

        // Colour swatch row for a given picker (with a leading "None").
        let swatches = |pick: SheetPick, cur: Option<(u8, u8, u8)>, cx: &mut Context<Self>| {
            let mut row = h_flex().flex_wrap().gap_1();
            let none_sel = cur.is_none();
            row = row.child(
                div().id(ElementId::Name(format!("fmt-c-none-{}", pick == SheetPick::Fill).into()))
                    .px_1p5().py(px(1.)).rounded_sm().cursor_pointer().text_size(px(10.))
                    .border_1().border_color(if none_sel { hsla_u(BRAND) } else { pal.border }).text_color(pal.fg)
                    .child("None")
                    .on_click(cx.listener(move |this, _, _w, cx| this.sheet_apply_color(pick, None, cx))),
            );
            for &c in COLOR_SWATCHES {
                let rgb = (((c >> 16) & 0xff) as u8, ((c >> 8) & 0xff) as u8, (c & 0xff) as u8);
                let sel = cur == Some(rgb);
                row = row.child(
                    div().id(ElementId::Name(format!("fmt-c-{}-{c:06x}", pick == SheetPick::Fill).into()))
                        .size(px(18.)).rounded_sm().cursor_pointer()
                        .bg(hsla_u(c))
                        .border_1().border_color(if sel { hsla_u(BRAND) } else { hsla_u(0x9a9a9a) })
                        .on_click(cx.listener(move |this, _, _w, cx| this.sheet_apply_color(pick, Some(rgb), cx))),
                );
            }
            row
        };
        let font_colors = swatches(SheetPick::Font, xf.color, cx);
        let fill_colors = swatches(SheetPick::Fill, xf.fill, cx);

        let card = v_flex()
            .w(px(420.)).gap_3().p_4()
            .bg(pal.panel).border_1().border_color(pal.border).rounded(px(8.))
            .shadow_lg()
            .child(h_flex().items_center().justify_between()
                .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(pal.fg).child("Format Cells"))
                .child(div().id("fmt-close").px_2().rounded_sm().cursor_pointer().text_size(px(15.)).text_color(pal.dim)
                    .hover(|d| d.text_color(pal.fg)).child("\u{00d7}")
                    .on_click(cx.listener(|this, _, _w, cx| { this.sheet_fmt_open = false; cx.notify(); }))))
            .child(v_flex().gap_1().child(heading("NUMBER")).child(number))
            .child(v_flex().gap_1().child(heading("FONT")).child(h_flex().gap_3().items_center().child(font_row).child(font_colors)))
            .child(v_flex().gap_1().child(heading("FILL")).child(fill_colors))
            .child(v_flex().gap_1().child(heading("ALIGNMENT")).child(align_row))
            .child(v_flex().gap_1().child(heading("BORDER")).child(border_row))
            .child(h_flex().justify_end()
                .child(div().id("fmt-done").px_3().py(px(4.)).rounded_sm().cursor_pointer().text_size(px(12.))
                    .bg(hsla_u(BRAND)).text_color(hsla_u(0xffffff)).child("Done")
                    .on_click(cx.listener(|this, _, _w, cx| { this.sheet_fmt_open = false; cx.notify(); }))));

        // Backdrop (click to dismiss) + centred card.
        div()
            .absolute().inset_0()
            .flex().items_center().justify_center()
            .bg(Hsla { h: 0., s: 0., l: 0., a: 0.35 })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _w, cx| { this.sheet_fmt_open = false; cx.notify(); }))
            .child(
                // Stop the card's own clicks from dismissing.
                div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(card),
            )
            .into_any_element()
    }

    /// The swatch strip shown under the ribbon while a sheet colour picker is open;
    /// a swatch sets the fill or font colour of the selection.
    fn sheet_picker_bar(&self, pick: SheetPick, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let mut row = h_flex().w_full().items_center().flex_wrap().gap_1p5().px_3().py_1().bg(pal.panel).border_b_1().border_color(pal.border);
        row = row.child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(70.)).child(match pick {
            SheetPick::Fill => "Fill colour",
            SheetPick::Font => "Font colour",
        }));
        row = row.child(
            div()
                .id("sc-none")
                .px_2().h(px(20.)).rounded(px(3.))
                .text_size(px(11.)).text_color(pal.fg)
                .border_1().border_color(pal.border)
                .cursor_pointer().hover(|d| d.bg(pal.hover))
                .child(if pick == SheetPick::Fill { "No fill" } else { "Automatic" })
                .on_click(cx.listener(move |this, _, _, cx| this.sheet_apply_color(pick, None, cx))),
        );
        for &c in COLOR_SWATCHES {
            let rgb = (((c >> 16) & 0xff) as u8, ((c >> 8) & 0xff) as u8, (c & 0xff) as u8);
            row = row.child(
                div()
                    .id(("sc", c as usize))
                    .size(px(20.)).rounded(px(3.))
                    .border_1().border_color(if c == 0xFFFFFF { pal.fg } else { pal.border })
                    .bg(hsla_u(c))
                    .cursor_pointer().hover(|d| d.border_color(hsla_u(BRAND)))
                    .on_click(cx.listener(move |this, _, _, cx| this.sheet_apply_color(pick, Some(rgb), cx))),
            );
        }
        row.into_any_element()
    }

    /// The sheet Find & Replace bar (Ctrl+F): query + prev/next, replace + all.
    /// The conditional-format entry bar: type a comparison like ">500" to apply
    /// the Light-Red highlight rule to the selection.
    fn sheet_cf_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::cell_name;
        let range = self.active_sheet().map(|v| {
            let (r0, c0, r1, c1) = v.range();
            format!("{}:{}", cell_name(r0, c0), cell_name(r1, c1))
        }).unwrap_or_default();
        let ent = cx.entity();
        let (ent_ok, ent_cancel) = (ent.clone(), ent.clone());
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child(format!("Highlight {range} where value")))
            .child(
                div().w(px(160.)).h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() { ">500".to_string() } else { buf.to_string() })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(div().text_size(px(11.)).text_color(pal.dim).child("(>, <, =, <>; 100..500 between; 'clear')"))
            .child(div().id("cf-apply").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(hsla_u(BRAND)).text_color(hsla_u(0xffffff)).border_1().border_color(pal.border).child("Apply")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_ok.update(cx, |this, cx| {
                        // Reuse the key path's commit by simulating Enter.
                        this.sheet_cf_commit(cx);
                    });
                }))
            .child(div().id("cf-cancel").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(pal.panel).text_color(pal.fg).border_1().border_color(pal.border).child("Cancel")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| { this.sheet_cf_edit = None; cx.notify(); });
                }))
            .into_any_element()
    }

    /// The Text-to-Columns delimiter bar.
    fn sheet_ttc_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent_cancel = cx.entity();
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child("Split selected column by delimiter:"))
            .child(
                div().w(px(150.)).h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() { "comma (or tab, space, ;)".to_string() } else { buf.to_string() })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(div().id("ttc-cancel").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(pal.panel).text_color(pal.fg).border_1().border_color(pal.border).child("Cancel")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| { this.sheet_ttc_edit = None; cx.notify(); });
                }))
            .into_any_element()
    }

    /// The AutoFilter criteria bar: type a comparison on the current column.
    fn sheet_filter_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::col_name;
        let col = self.active_sheet().map(|v| col_name(v.sel.1)).unwrap_or_default();
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child(format!("Filter column {col} where value")))
            .child(
                div().w(px(180.)).h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() { "=Laptop  (or >500, clear)".to_string() } else { buf.to_string() })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(div().id("filter-cancel").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(pal.panel).text_color(pal.fg).border_1().border_color(pal.border).child("Cancel")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| { this.sheet_filter_edit = None; cx.notify(); });
                }))
            .into_any_element()
    }

    /// The multi-level sort bar: type a spec like "B asc, C desc".
    fn sheet_sort_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child("Sort the region by"))
            .child(
                div().w(px(220.)).h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() { "B asc, C desc".to_string() } else { buf.to_string() })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(div().id("sort-cancel").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(pal.panel).text_color(pal.fg).border_1().border_color(pal.border).child("Cancel")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| { this.sheet_sort_edit = None; cx.notify(); });
                }))
            .into_any_element()
    }

    /// The row-height entry bar: type a height in points (or "auto").
    fn sheet_rowh_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child("Row height (points, or 'auto')"))
            .child(
                div().w(px(120.)).h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() { "30".to_string() } else { buf.to_string() })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(div().id("rowh-cancel").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(pal.panel).text_color(pal.fg).border_1().border_color(pal.border).child("Cancel")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| { this.sheet_rowh_edit = None; cx.notify(); });
                }))
            .into_any_element()
    }

    /// The data-validation entry bar: type comma-separated allowed values to make
    /// the selection a dropdown list.
    fn sheet_dv_edit_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::cell_name;
        let range = self.active_sheet().map(|v| {
            let (r0, c0, r1, c1) = v.range();
            format!("{}:{}", cell_name(r0, c0), cell_name(r1, c1))
        }).unwrap_or_default();
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child(format!("Dropdown list for {range} (comma-separated):")))
            .child(
                div().flex_1().h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() { "Yes, No, Maybe".to_string() } else { buf.to_string() })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(div().id("dv-cancel").px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(pal.panel).text_color(pal.fg).border_1().border_color(pal.border).child("Cancel")
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| { this.sheet_dv_edit = None; cx.notify(); });
                }))
            .into_any_element()
    }

    /// Apply the current CF buffer (used by the Apply button; Enter uses sheet_cf_key).
    fn sheet_cf_commit(&mut self, cx: &mut Context<Self>) {
        let buf = self.sheet_cf_edit.clone().unwrap_or_default();
        let buf = if buf.trim().is_empty() { ">500".to_string() } else { buf };
        if buf.trim().eq_ignore_ascii_case("clear") {
            self.sheet_snapshot();
            if let Some(v) = self.active_sheet_mut() {
                let s = v.active;
                v.pkg.clear_conditional_formats(s);
                v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
            }
            self.mark_sheet_dirty();
        } else if let Some((op, val, val2)) = parse_cf_input(&buf) {
            self.sheet_snapshot();
            if let Some(v) = self.active_sheet_mut() {
                let s = v.active;
                let (r0, c0, r1, c1) = v.range();
                v.pkg.add_conditional_format(s, (r0, c0, r1, c1), op, &val, val2.as_deref(), cf_preset_dxf());
                v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
            }
            self.mark_sheet_dirty();
        }
        self.sheet_cf_edit = None;
        cx.notify();
    }

    /// The cell-comment entry bar: a labelled text field (self-managed, keys via
    /// sheet_comment_key) with the target cell, Save/Cancel. Enter commits.
    fn sheet_comment_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::cell_name;
        let cell = self.active_sheet().map(|v| cell_name(v.sel.0, v.sel.1)).unwrap_or_default();
        let ent = cx.entity();
        let (ent_save, ent_cancel) = (ent.clone(), ent.clone());
        let btn = |label: &str, primary: bool| {
            div()
                .px_2().py(px(2.)).rounded_sm().cursor_pointer().text_size(px(12.))
                .bg(if primary { hsla_u(BRAND) } else { pal.panel })
                .text_color(if primary { hsla_u(0xffffff) } else { pal.fg })
                .border_1().border_color(pal.border)
                .child(label.to_string())
        };
        h_flex()
            .w_full().h(px(30.)).items_center().gap_2().px_2()
            .bg(pal.panel).border_b_1().border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child(format!("Comment on {cell}:")))
            .child(
                div().flex_1().h(px(22.)).px_2().flex().items_center().rounded_sm()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(BRAND))
                    .text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(buf.to_string())))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(btn("Save", true).on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                ent_save.update(cx, |this, cx| this.sheet_commit_comment(cx));
            }))
            .child(btn("Cancel", false).on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                ent_cancel.update(cx, |this, cx| { this.sheet_comment_edit = None; cx.notify(); });
            }))
            .into_any_element()
    }

    fn sheet_find_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let qf = self.find_field == FindField::Query;
        let rf = self.find_field == FindField::Replace;
        let field = |id: &'static str, val: &str, focused: bool, ph: &'static str| {
            let empty = val.is_empty();
            div()
                .id(id)
                .flex().items_center().min_w(px(150.)).h(px(24.)).px_2()
                .rounded(px(3.))
                .border_1().border_color(if focused { hsla_u(BRAND) } else { pal.border })
                .bg(hsla_u(0xffffff))
                .cursor_text()
                .text_size(px(12.)).text_color(if empty { hsla_u(0x999999) } else { hsla_u(0x1a1a1a) })
                .child(SharedString::from(if empty { ph.to_string() } else { val.to_string() }))
                .when(focused, |d| d.child(div().w(px(1.)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))))
        };
        let btn = |id: &'static str, label: SharedString| {
            div().id(id).px_2().h(px(24.)).flex().items_center().justify_center().min_w(px(24.)).rounded(px(3.)).cursor_pointer().text_size(px(12.)).text_color(pal.fg).border_1().border_color(pal.border).hover(|d| d.bg(pal.hover)).child(label)
        };
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(46.)).child("Find"))
            .child(field("sf-q", &self.find_query, qf, "Find in sheet").on_click(cx.listener(|this, _, _, cx| {
                this.find_field = FindField::Query;
                cx.notify();
            })))
            .child(btn("sf-prev", "\u{25C0}".into()).on_click(cx.listener(|this, _, _, cx| this.sheet_find_next(true, cx))))
            .child(btn("sf-next", "\u{25B6}".into()).on_click(cx.listener(|this, _, _, cx| this.sheet_find_next(false, cx))))
            .child(div().text_size(px(11.)).text_color(pal.dim).child("Replace"))
            .child(field("sf-r", &self.replace_text, rf, "Replace with").on_click(cx.listener(|this, _, _, cx| {
                this.find_field = FindField::Replace;
                cx.notify();
            })))
            .child(btn("sf-rep", "Replace".into()).on_click(cx.listener(|this, _, _, cx| this.sheet_replace(cx))))
            .child(btn("sf-all", "All".into()).on_click(cx.listener(|this, _, _, cx| this.sheet_replace_all(cx))))
            .child(div().flex_1())
            .child(btn("sf-close", "\u{2715}".into()).on_click(cx.listener(|this, _, _, cx| {
                this.find_open = false;
                cx.notify();
            })))
            .into_any_element()
    }

    /// The spreadsheet ribbon body — the Home tab, or the Insert tab (Tables).
    fn sheet_ribbon_body(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        match self.ribbon_tab {
            RibbonTab::Insert => self.sheet_insert_ribbon(pal, cx),
            RibbonTab::Review => self.sheet_review_ribbon(pal, cx),
            RibbonTab::View => self.sheet_view_ribbon(pal, cx),
            _ => self.sheet_home_ribbon(pal, cx),
        }
    }

    /// The Insert tab: a Tables group (PivotTable, Table) like Excel.
    fn sheet_insert_ribbon(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let group = |title: &str, body: AnyElement| -> AnyElement {
            v_flex()
                .h(px(94.))
                .px_1p5()
                .py(px(3.))
                .justify_between()
                .border_r_1()
                .border_color(pal.border)
                .child(div().flex_1().flex().items_center().child(body))
                .child(div().w_full().text_size(px(10.)).text_color(pal.dim).text_center().child(title.to_string()))
                .into_any_element()
        };
        h_flex()
            .id("sheet-ribbon")
            .w_full()
            .h(px(100.))
            .items_stretch()
            .px_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .overflow_x_scroll()
            .child(group("Tables", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(Some("table"), "PivotTable", SheetAct::InsertPivot, pal, cx))
                .child(self.sheet_lb(Some("table"), "Table", SheetAct::FormatAsTable, pal, cx))
                .child(self.sheet_lb(None, "Data Validation", SheetAct::DataValidation, pal, cx))
                .child(self.sheet_lb(None, "Text to Columns", SheetAct::TextToColumns, pal, cx))
                .into_any_element()))
            .child(group("Outline", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(None, "Subtotal", SheetAct::Subtotal, pal, cx))
                .child(self.sheet_lb(None, "Group / Ungroup", SheetAct::Outline, pal, cx))
                .into_any_element()))
            .child(group("Charts", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(None, "Column", SheetAct::InsertChart("column"), pal, cx))
                .child(self.sheet_lb(None, "Bar", SheetAct::InsertChart("bar"), pal, cx))
                .child(self.sheet_lb(None, "Line", SheetAct::InsertChart("line"), pal, cx))
                .child(self.sheet_lb(None, "Pie", SheetAct::InsertChart("pie"), pal, cx))
                .into_any_element()))
            .into_any_element()
    }

    /// The Review tab, laid out like Excel: Proofing, Comments, Protect. These
    /// aren't modeled for sheets yet (no cell-comment model), so the buttons are
    /// inert placeholders — the point is a distinct, Excel-faithful tab identity
    /// (it used to fall through to the Home ribbon).
    fn sheet_review_ribbon(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let group = |title: &str, body: AnyElement| -> AnyElement {
            v_flex()
                .h(px(94.))
                .px_1p5()
                .py(px(3.))
                .justify_between()
                .border_r_1()
                .border_color(pal.border)
                .child(div().flex_1().flex().items_center().child(body))
                .child(div().w_full().text_size(px(10.)).text_color(pal.dim).text_center().child(title.to_string()))
                .into_any_element()
        };
        h_flex()
            .id("sheet-ribbon")
            .w_full()
            .h(px(100.))
            .items_stretch()
            .px_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .overflow_x_scroll()
            .child(group("Proofing", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(None, "Spelling", SheetAct::Todo, pal, cx))
                .into_any_element()))
            .child(group("Comments", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(None, "New Comment", SheetAct::NewComment, pal, cx))
                .child(self.sheet_lb(None, "Delete", SheetAct::DeleteComment, pal, cx))
                .child(self.sheet_lb(None, "Previous", SheetAct::PrevComment, pal, cx))
                .child(self.sheet_lb(None, "Next", SheetAct::NextComment, pal, cx))
                .into_any_element()))
            .child(group("Protect", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(Some("lock"), if self.sheet_protected() { "Unprotect Sheet" } else { "Protect Sheet" }, SheetAct::ProtectSheet, pal, cx))
                .child(self.sheet_lb(None, "Protect Workbook", SheetAct::Todo, pal, cx))
                .into_any_element()))
            .into_any_element()
    }

    /// The View tab: a Window group with Freeze Panes, like Excel.
    fn sheet_view_ribbon(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let frozen = self.active_sheet().is_some_and(|v| v.sheet().freeze != (0, 0));
        let group = |title: &str, body: AnyElement| -> AnyElement {
            v_flex()
                .h(px(94.))
                .px_1p5()
                .py(px(3.))
                .justify_between()
                .border_r_1()
                .border_color(pal.border)
                .child(div().flex_1().flex().items_center().child(body))
                .child(div().w_full().text_size(px(10.)).text_color(pal.dim).text_center().child(title.to_string()))
                .into_any_element()
        };
        h_flex()
            .id("sheet-ribbon")
            .w_full()
            .h(px(100.))
            .items_stretch()
            .px_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .overflow_x_scroll()
            .child(group("Window", h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(None, if frozen { "Unfreeze Panes" } else { "Freeze Panes" }, SheetAct::FreezePanes, pal, cx))
                .into_any_element()))
            .into_any_element()
    }

    /// The spreadsheet Home tab, laid out like Excel: Clipboard, Font, Alignment,
    /// Number, Styles, Cells, Editing.
    fn sheet_home_ribbon(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let xf = self.active_xf();
        // A group frame: content on top, a centered label (+ optional dialog
        // launcher) at the bottom, and a right divider — exactly like the doc ribbon.
        let group = |title: &str, launcher: bool, body: AnyElement| -> AnyElement {
            v_flex()
                .h(px(94.))
                .px_1p5()
                .py(px(3.))
                .justify_between()
                .border_r_1()
                .border_color(pal.border)
                .child(div().flex_1().flex().items_center().child(body))
                .child(
                    h_flex().w_full().items_center().justify_center().gap_1()
                        .child(div().text_size(px(10.)).text_color(pal.dim).child(title.to_string()))
                        .when(launcher, |d| d.child(div().text_size(px(9.)).text_color(pal.dim).child("\u{2921}"))),
                )
                .into_any_element()
        };
        let row = |kids: Vec<AnyElement>| h_flex().items_center().gap(px(2.)).children(kids).into_any_element();
        let col = |kids: Vec<AnyElement>| v_flex().gap(px(1.)).children(kids).into_any_element();

        h_flex()
            .id("sheet-ribbon")
            .w_full()
            .h(px(100.))
            .items_stretch()
            .px_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .overflow_x_scroll()
            // Clipboard: big Paste + a Cut/Copy/Format-Painter column.
            .child(group("Clipboard", true, h_flex().h_full().items_center().gap_1()
                .child(self.sheet_lb(Some("paste"), "Paste", SheetAct::Paste, pal, cx))
                .child(col(vec![
                    self.sheet_rb(Some("cut"), "Cut", SheetAct::Cut, pal, cx),
                    self.sheet_rb(Some("copy"), "Copy", SheetAct::Copy, pal, cx),
                    self.sheet_rb(None, "Format Painter", SheetAct::Todo, pal, cx),
                ]))
                .into_any_element()))
            // Font: name/size combos + grow/shrink; then B/I/U, borders, fill, colour.
            .child(group("Font", true, col(vec![
                row(vec![
                    self.sheet_combo("Calibri", true, pal, cx),
                    self.sheet_combo("11", false, pal, cx),
                    self.sheet_ib("font-increase", SheetAct::GrowFont, false, pal, cx),
                    self.sheet_ib("font-decrease", SheetAct::ShrinkFont, false, pal, cx),
                ]),
                row(vec![
                    self.sheet_ib("bold", SheetAct::Bold, xf.bold, pal, cx),
                    self.sheet_ib("italic", SheetAct::Italic, xf.italic, pal, cx),
                    self.sheet_ib("underline", SheetAct::Todo, false, pal, cx),
                    self.sheet_ib("border-bottom", SheetAct::ToggleBorder, xf.border, pal, cx),
                    self.sheet_ib("highlight", SheetAct::FillColor, false, pal, cx),
                    self.sheet_ib("text-color", SheetAct::FontColor, false, pal, cx),
                ]),
            ])))
            // Alignment: top/mid/bottom + wrap; then left/center/right, indent, merge.
            .child(group("Alignment", true, col(vec![
                row(vec![
                    self.sheet_gb("\u{2580}", SheetAct::Todo, pal, cx),
                    self.sheet_gb("\u{25AC}", SheetAct::Todo, pal, cx),
                    self.sheet_gb("\u{2584}", SheetAct::Todo, pal, cx),
                    self.sheet_rb(None, "Wrap Text", SheetAct::WrapText, pal, cx),
                ]),
                row(vec![
                    self.sheet_ib("align-left", SheetAct::AlignL, matches!(xf.align, gridcore::sheet::Align::Left), pal, cx),
                    self.sheet_ib("align-center", SheetAct::AlignC, matches!(xf.align, gridcore::sheet::Align::Center), pal, cx),
                    self.sheet_ib("align-right", SheetAct::AlignR, matches!(xf.align, gridcore::sheet::Align::Right), pal, cx),
                    self.sheet_ib("indent-decrease", SheetAct::Todo, false, pal, cx),
                    self.sheet_rb(None, "Row Height", SheetAct::RowHeight, pal, cx),
                    self.sheet_rb(None, "Merge", SheetAct::Merge, pal, cx),
                ]),
            ])))
            // Number: format combo; then currency/percent/comma + decimals.
            .child(group("Number", true, col(vec![
                row(vec![self.sheet_numfmt_combo(pal, cx)]),
                row(vec![
                    self.sheet_gb("$", SheetAct::Currency, pal, cx),
                    self.sheet_gb("%", SheetAct::Percent, pal, cx),
                    self.sheet_gb(",", SheetAct::Comma, pal, cx),
                    self.sheet_gb("\u{2192}.0", SheetAct::Todo, pal, cx),
                    self.sheet_gb(".00\u{2190}", SheetAct::Todo, pal, cx),
                ]),
            ])))
            // Styles: Conditional Formatting, Format as Table, Cell Styles.
            .child(group("Styles", false, h_flex().h_full().items_center().gap_0p5()
                .child(self.sheet_lb(None, "Conditional Formatting", SheetAct::CondFormat, pal, cx))
                .child(self.sheet_lb(Some("table"), "Format as Table", SheetAct::FormatAsTable, pal, cx))
                .child(self.sheet_lb(None, "Cell Styles", SheetAct::Todo, pal, cx))
                .into_any_element()))
            // Cells: Insert, Delete, Format.
            .child(group("Cells", false, h_flex().h_full().items_center().gap_2()
                .child(v_flex().gap_0p5()
                    .child(self.sheet_rb(None, "Insert Row", SheetAct::InsertRow, pal, cx))
                    .child(self.sheet_rb(None, "Insert Col", SheetAct::InsertCol, pal, cx)))
                .child(v_flex().gap_0p5()
                    .child(self.sheet_rb(None, "Delete Row", SheetAct::DeleteRow, pal, cx))
                    .child(self.sheet_rb(None, "Delete Col", SheetAct::DeleteCol, pal, cx)))
                .child(self.sheet_lb(None, "Format", SheetAct::FormatCells, pal, cx))
                .into_any_element()))
            // Editing: AutoSum/Fill/Clear column + Sort & Filter, Find & Select.
            .child(group("Editing", false, h_flex().h_full().items_center().gap_1()
                .child(col(vec![
                    self.sheet_rb(None, "\u{03A3} AutoSum", SheetAct::AutoSum, pal, cx),
                    self.sheet_rb(None, "Fill", SheetAct::Todo, pal, cx),
                    self.sheet_rb(Some("clear-format"), "Clear", SheetAct::Todo, pal, cx),
                ]))
                .child(col(vec![
                    self.sheet_rb(Some("sort"), "Sort A \u{2192} Z", SheetAct::SortAsc, pal, cx),
                    self.sheet_rb(Some("sort"), "Sort Z \u{2192} A", SheetAct::SortDesc, pal, cx),
                    self.sheet_rb(Some("sort"), "Custom Sort\u{2026}", SheetAct::CustomSort, pal, cx),
                    self.sheet_rb(None, "Filter", SheetAct::Filter, pal, cx),
                    self.sheet_rb(None, "Remove Dup", SheetAct::RemoveDuplicates, pal, cx),
                ]))
                .child(self.sheet_lb(Some("find"), "Find & Select", SheetAct::Todo, pal, cx))
                .into_any_element()))
            .into_any_element()
    }

    fn render_group(&self, g: &rs::Group<Act>, icon_only: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let controls: Vec<AnyElement> = g.items.iter().map(|c| self.render_control(c, icon_only, pal, cx)).collect();
        // group title row + optional dialog-box launcher (⤢)
        let title_row = h_flex()
            .items_center()
            .gap_1()
            .child(div().text_size(px(9.)).text_color(pal.dim).child(g.title))
            .when_some(g.launcher, |d, act| {
                d.child(
                    div()
                        .id(SharedString::from(format!("launch-{}", g.title)))
                        .text_size(px(10.))
                        .text_color(pal.dim)
                        .cursor_pointer()
                        .hover(|d| d.text_color(pal.fg))
                        .child("\u{2922}")
                        .tooltip(|w, cx| Tooltip::new("More options").build(w, cx))
                        .on_click(cx.listener(move |this, _, w, cx| this.dispatch(act, w, cx))),
                )
            });
        v_flex()
            .items_center()
            .justify_between()
            .h_full()
            .px_2()
            .py_0p5()
            .gap_0p5()
            .border_r_1()
            .border_color(pal.border)
            .child(h_flex().flex_1().items_center().gap_1().children(controls))
            .child(title_row)
            .into_any_element()
    }

    fn render_control(&self, c: &Control<Act>, icon_only: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        match c {
            Control::Toggle(cmd) => self.icon_btn(cmd, false, pal, cx),
            Control::Large(cmd) => self.large_btn(cmd, pal, cx),
            Control::Column(cmds) => {
                // Office caps a button column at 3 rows; extra buttons wrap into
                // the next column so nothing overflows the ribbon body height.
                let cols: Vec<AnyElement> = cmds
                    .chunks(3)
                    .map(|chunk| {
                        v_flex()
                            .gap(px(1.))
                            .children(chunk.iter().map(|cm| self.icon_btn(cm, !icon_only, pal, cx)))
                            .into_any_element()
                    })
                    .collect();
                h_flex().items_start().gap_1().children(cols).into_any_element()
            }
            // The Office two-row layout: each inner Vec is one left-to-right row.
            Control::Rows(rows) => {
                let rendered: Vec<AnyElement> = rows
                    .iter()
                    .map(|row| {
                        h_flex()
                            .items_center()
                            .gap(px(1.))
                            .children(row.iter().map(|cell| self.render_cell(cell, pal, cx)))
                            .into_any_element()
                    })
                    .collect();
                v_flex().items_start().gap(px(2.)).children(rendered).into_any_element()
            }
            Control::Gallery(gal) => self.style_gallery(gal, pal, cx),
            Control::Separator => div().w(px(1.)).h(px(44.)).bg(pal.border).mx_1().into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// The Styles gallery: a row of thumbnail boxes, each showing its name in that
    /// style's own weight/size (Word's Style gallery).
    fn style_gallery(&self, gal: &rs::Gallery<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let cur = self.tabs.get(self.active).and_then(|t| if let Surface::Doc(ed) = &t.surface { ed.caret_para_style() } else { None });
        let boxes: Vec<AnyElement> = gal
            .items
            .iter()
            .map(|it| {
                let act = it.act;
                // Map the preview hint to a thumbnail appearance.
                let (size, weight) = match it.preview {
                    "title" => (16.0, FontWeight::BOLD),
                    "subtitle" => (12.0, FontWeight::NORMAL),
                    "h1" => (14.0, FontWeight::BOLD),
                    "h2" => (13.0, FontWeight::BOLD),
                    "h3" => (12.0, FontWeight::SEMIBOLD),
                    _ => (11.0, FontWeight::NORMAL),
                };
                let style_id = match it.preview {
                    "title" => Some("Title"),
                    "subtitle" => Some("Subtitle"),
                    "h1" => Some("Heading1"),
                    "h2" => Some("Heading2"),
                    "h3" => Some("Heading3"),
                    _ => None,
                };
                let selected = cur.as_deref() == style_id;
                div()
                    .id(it.label)
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(76.))
                    .h(px(40.))
                    .px_1()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if selected { hsla_u(BRAND) } else { pal.border })
                    .bg(pal.panel)
                    .cursor_pointer()
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(div().text_size(px(size)).font_weight(weight).text_color(pal.fg).overflow_hidden().child(SharedString::from(it.label)))
                    .tooltip({
                        let label = it.label;
                        move |w, cx| Tooltip::new(label).build(w, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
                    .into_any_element()
            })
            .collect();
        h_flex().items_center().gap_1().children(boxes).into_any_element()
    }

    fn render_cell(&self, cell: &rs::Cell<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        match cell {
            rs::Cell::Btn(cmd) => self.icon_btn(cmd, false, pal, cx),
            rs::Cell::Combo { cmd, wide } => self.combo_box(cmd, *wide, pal, cx),
        }
    }

    /// The current font family / size at the caret (for the Font combos).
    fn caret_run_props(&self) -> Option<RunProps> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => Some(ed.caret_props()),
            _ => None,
        }
    }

    /// A Font-group combo box showing the current value with a dropdown chevron.
    fn combo_box(&self, cmd: &rs::Cmd<Act>, wide: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let props = self.caret_run_props();
        let value: SharedString = if cmd.id == "fontname" {
            props.and_then(|p| p.font).unwrap_or_else(|| "Calibri".into()).into()
        } else {
            props
                .and_then(|p| p.size_half_pts)
                .map(|h| {
                    let s = h as f32 / 2.0;
                    if s.fract() == 0.0 { format!("{}", s as u32) } else { format!("{s}") }
                })
                .unwrap_or_else(|| "11".into())
                .into()
        };
        let act = cmd.act;
        div()
            .id(cmd.id)
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .w(px(if wide { 104. } else { 46. }))
            .h(px(22.))
            .px_1p5()
            .rounded(px(3.))
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .cursor_pointer()
            .hover(|d| d.border_color(hsla_u(BRAND)))
            .child(div().text_size(px(11.)).text_color(pal.fg).overflow_hidden().child(value))
            .child(div().text_size(px(8.)).text_color(pal.dim).child("\u{25BE}"))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    /// A large icon-over-label ribbon button (e.g. Paste, Table).
    fn large_btn(&self, cmd: &rs::Cmd<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let act = cmd.act;
        let on = self.act_active(act);
        let tip: SharedString = cmd.label.into();
        let keytip = (self.keytips == KeyTip::Commands && !cmd.key_tip.is_empty()).then_some(cmd.key_tip);
        // Wrap a multi-word label at a word boundary rather than breaking mid-word.
        let mut label = v_flex().items_center();
        for ln in label_lines(cmd.label) {
            label = label.child(div().text_size(px(11.)).text_color(pal.fg).child(SharedString::from(ln)));
        }
        div()
            .id(cmd.id)
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_0p5()
            .px_2()
            .h_full()
            .rounded(px(4.))
            .cursor_pointer()
            .when(on, |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }))
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(cmd.icon.0, 26., pal.fg))
            .child(label)
            .when_some(keytip, |d, k| d.child(keytip_badge(k)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    /// Whether a toggle command is currently "on" for the caret's formatting, so
    /// the ribbon button can show a pressed state (Word highlights e.g. Bold when
    /// the caret sits in bold text).
    fn act_active(&self, act: Act) -> bool {
        use Act::*;
        let doc = match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => Some(ed),
            _ => None,
        };
        let rp = doc.map(|ed| ed.caret_props());
        let pp = doc.map(|ed| ed.caret_para_props());
        match act {
            Bold => rp.map_or(false, |p| p.bold),
            Italic => rp.map_or(false, |p| p.italic),
            Underline => rp.map_or(false, |p| p.underline),
            Strike => rp.map_or(false, |p| p.strike),
            Super => rp.map_or(false, |p| p.vert_align == VertAlign::Superscript),
            Sub => rp.map_or(false, |p| p.vert_align == VertAlign::Subscript),
            AlignL => pp.map_or(false, |p| p.align == Align::Left),
            AlignC => pp.map_or(false, |p| p.align == Align::Center),
            AlignR => pp.map_or(false, |p| p.align == Align::Right),
            AlignJ => pp.map_or(false, |p| p.align == Align::Justify),
            Bullets => doc.map_or(false, |ed| ed.all_in_list(NUM_BULLET)),
            Numbers => doc.map_or(false, |ed| ed.all_in_list(NUM_DECIMAL)),
            ParaBorders => pp.map_or(false, |p| p.borders.bottom.is_some()),
            ShowHide => self.show_marks,
            ToggleComments => self.show_comments,
            ToggleNav => self.show_nav,
            ToggleNotes => self.show_notes,
            PrintLayout => self.page_view,
            ToggleRuler => self.show_ruler,
            _ => false,
        }
    }

    fn icon_btn(&self, cmd: &rs::Cmd<Act>, show_label: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let act = cmd.act;
        let tip = cmd.tip;
        let on = self.act_active(act);
        let tip_text: SharedString = if tip.shortcut.is_empty() {
            tip.title.into()
        } else {
            format!("{}  \u{00b7}  {}", tip.title, tip.shortcut).into()
        };
        let keytip = (self.keytips == KeyTip::Commands && !cmd.key_tip.is_empty()).then_some(cmd.key_tip);
        div()
            .id(cmd.id)
            .relative()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .h(px(22.))
            .rounded(px(4.))
            .cursor_pointer()
            // Pressed/checked state: a soft brand wash + brand border, like Word.
            .when(on, |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }).border_1().border_color(hsla_u(BRAND)))
            .when(!on, |d| d.border_1().border_color(gpui::transparent_black()))
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(cmd.icon.0, 16., pal.fg))
            .when(show_label, |d| d.child(div().text_size(px(12.)).text_color(pal.fg).child(SharedString::from(cmd.label))))
            .when_some(keytip, |d, k| d.child(keytip_badge(k)))
            .tooltip(move |window, cx| Tooltip::new(tip_text.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    fn backstage_view(&self, bg: Hsla, fg: Hsla, dim: Hsla, sidebar: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let rail_item = |cx: &mut Context<Self>, id: &'static str, label: &'static str, f: fn(&mut Docxy, &mut Window, &mut Context<Docxy>)| {
            div()
                .id(id)
                .w_full()
                .px_4()
                .py_2()
                .cursor_pointer()
                .rounded_sm()
                .text_color(fg)
                .hover(|d| d.bg(rgb(BRAND)).text_color(rgb(FILE_FG)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
        };

        let rail = v_flex()
            .w(px(220.))
            .h_full()
            .py_3()
            .gap_1()
            .bg(sidebar)
            .child(
                div()
                    .id("bs-back")
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .text_color(rgb(BRAND))
                    .child("\u{2190} Back")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.backstage = false;
                        this.bs_new = false;
                        this.refocus(window, cx);
                    })),
            )
            .child(
                div()
                    .id("bs-new")
                    .w_full()
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .rounded_sm()
                    .text_color(fg)
                    .hover(|d| d.bg(rgb(BRAND)).text_color(rgb(FILE_FG)))
                    .child("New")
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.bs_new = true;
                        cx.notify();
                    })),
            )
            .child(rail_item(cx, "bs-open", "Open\u{2026}", |t, w, cx| t.open_file(w, cx)))
            .child(rail_item(cx, "bs-save", "Save", |t, w, cx| t.save_active(w, cx)))
            .child(rail_item(cx, "bs-saveas", "Save As\u{2026}", |t, w, cx| t.save_as(w, cx)))
            .child(rail_item(cx, "bs-close", "Close", |t, w, cx| {
                let a = t.active;
                t.backstage = false;
                t.close_tab(a, w, cx);
            }));

        let pane = if self.bs_new {
            let card = |cx: &mut Context<Self>, id: &'static str, glyph: &'static str, name: &'static str, sub: &'static str, kind: Kind| {
                v_flex()
                    .id(id)
                    .w(px(150.))
                    .h(px(160.))
                    .p_3()
                    .gap_2()
                    .rounded_md()
                    .border_1()
                    .border_color(dim)
                    .cursor_pointer()
                    .hover(|d| d.border_color(rgb(BRAND)))
                    .child(div().text_size(px(40.)).child(glyph))
                    .child(div().text_color(fg).font_weight(FontWeight::BOLD).child(name))
                    .child(div().text_size(px(11.)).text_color(dim).child(sub))
                    .on_click(cx.listener(move |this, _, window, cx| this.add_tab(kind, window, cx)))
            };
            v_flex()
                .flex_1()
                .h_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).text_color(fg).child("New"))
                .child(
                    h_flex()
                        .gap_4()
                        .child(card(cx, "new-doc-card", Kind::Docx.glyph(), "Document", "Blank .docx", Kind::Docx))
                        .child(card(cx, "new-xls-card", Kind::Xlsx.glyph(), "Spreadsheet", "Blank .xlsx", Kind::Xlsx))
                        .child(card(cx, "new-mail-card", Kind::Look.glyph(), "Mail", "New message", Kind::Look)),
                )
                .into_any_element()
        } else {
            let active = self.tabs.get(self.active);
            let (cur_title, cur_path) = active
                .map(|t| (t.title.to_string(), t.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not saved yet".into())))
                .unwrap_or_else(|| ("—".into(), "".into()));
            let recents: Vec<AnyElement> = self
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    div()
                        .id(("recent", i))
                        .px_3()
                        .py_1p5()
                        .cursor_pointer()
                        .rounded_sm()
                        .text_color(fg)
                        .hover(|d| d.bg(sidebar))
                        .child(format!("{} {}", t.kind.glyph(), t.title))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.backstage = false;
                            this.select_tab(i, window, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            v_flex()
                .flex_1()
                .h_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).text_color(fg).child(cur_title))
                .child(div().text_size(px(12.)).text_color(dim).child(cur_path))
                .child(div().text_size(px(13.)).text_color(rgb(BRAND)).mt_4().child("Open"))
                .child(v_flex().gap_0p5().children(recents))
                .child(div().text_size(px(13.)).text_color(rgb(BRAND)).mt_4().child("Settings"))
                .child(
                    div()
                        .id("bs-ask-toggle")
                        .flex()
                        .items_center()
                        .gap_2()
                        .py_1()
                        .cursor_pointer()
                        .rounded_sm()
                        .hover(|d| d.bg(sidebar))
                        .child(
                            div()
                                .size(px(16.))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(if self.ask_on_close { hsla_u(BRAND) } else { dim })
                                .bg(if self.ask_on_close { hsla_u(BRAND) } else { Hsla { a: 0., ..fg } })
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(self.ask_on_close, |d| d.child(div().text_size(px(11.)).text_color(rgb(FILE_FG)).child("\u{2713}"))),
                        )
                        .child(div().text_color(fg).child("Ask before closing with unsaved changes"))
                        .on_click(cx.listener(|this, _, _w, cx| {
                            this.ask_on_close = !this.ask_on_close;
                            this.persist();
                            cx.notify();
                        })),
                )
                .child(div().text_size(px(11.)).text_color(dim).child("Off: closing is silent — your work is always kept and reopened next launch."))
                .into_any_element()
        };

        h_flex().size_full().bg(bg).child(rail).child(pane).into_any_element()
    }
}

impl Render for Docxy {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply the theme choice (Auto follows the OS appearance).
        let desired = match self.theme_pref {
            ThemePref::Auto => ThemeMode::from(window.appearance()),
            ThemePref::Light => ThemeMode::Light,
            ThemePref::Dark => ThemeMode::Dark,
        };
        if self.applied != Some(desired) {
            Theme::change(desired, Some(window), cx);
            self.applied = Some(desired);
        }
        if !self.focused {
            self.focus.focus(window, cx);
            self.focused = true;
        }

        // Spreadsheet horizontal scroll: reconcile the column offset so the
        // selected column stays visible, and remember the grid width for sheet_el.
        let sheet_grid_w = if self.active_is_sheet() {
            let panel = if self.active_pivot().is_some() { 232.0 } else { 0.0 };
            let w = f32::from(window.viewport_size().width) - panel;
            self.reconcile_sheet_hscroll((w - SHEET_GUT).max(120.0));
            self.sheet_grid_w = w;
            w
        } else {
            0.0
        };
        // List data-validation options for the selected cell (dropdown), if any.
        let sheet_dv = self.active_is_sheet().then(|| self.dv_list_values()).flatten();
        if sheet_dv.is_none() {
            self.sheet_dv_open = false;
        }

        let t = cx.theme();
        let bg = t.background;
        let fg = t.foreground;
        let dim = t.muted_foreground;
        let border = t.border;
        let panel = t.secondary;
        let sidebar = t.sidebar;
        let tab_active = t.tab_active;
        // A theme-adaptive hover tint: a low-alpha wash of the foreground, so it's
        // clearly visible as a highlight on both light and dark grounds.
        let hover = Hsla { a: 0.12, ..fg };
        let pal = Pal { fg, dim, border, panel, hover, sel: t.selection };

        // --- title bar: wordmark + document tab chips + theme toggle ---
        let theme_pref = self.theme_pref;
        let chips: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tb)| {
                let active = i == self.active && !self.backstage;
                let mark = if tb.dirty { " \u{2022}" } else { "" };
                h_flex()
                    .id(("chip", i))
                    .items_center()
                    .gap_1()
                    .px_2()
                    .h(px(24.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .when(active, |d| d.bg(tab_active).text_color(fg))
                    .when(!active, |d| d.text_color(dim))
                    .child(SharedString::from(format!("{} {}{}", tb.kind.glyph(), tb.title, mark)))
                    .child(
                        div()
                            .id(("chipx", i))
                            .px_1()
                            .rounded_sm()
                            .hover(|d| d.bg(border))
                            .child("\u{00d7}")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(i, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.select_tab(i, window, cx)))
                    .into_any_element()
            })
            .collect();

        // NOTE: the "docxy" brand label and the flex_1 spacer are plain,
        // non-interactive divs, so TitleBar's own drag region shows through them
        // — that idle space is natively draggable and double-click maximizes.
        // Only the *interactive* clusters (chips, theme button) swallow the
        // mouse-down so a drag on them doesn't start a window move.
        let title_bar = TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pl_2()
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(BRAND)).child("docxy"))
                // Quick Access Toolbar: Undo / Redo (Word keeps these here, not on
                // the ribbon).
                .child(
                    h_flex()
                        .items_center()
                        .gap_0p5()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(qat_btn("qat-undo", "undo", "Undo (Ctrl+Z)", pal, cx.listener(|this, _, window, cx| this.with_editor(window, cx, |e| { e.undo(); }))))
                        .child(qat_btn("qat-redo", "redo", "Redo (Ctrl+Y)", pal, cx.listener(|this, _, window, cx| this.with_editor(window, cx, |e| { e.redo(); })))),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(chips),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(Button::new("theme").ghost().xsmall().label(theme_pref.label()).on_click(cx.listener(|this, _, window, cx| this.cycle_theme(window, cx)))),
                ),
        );

        if self.backstage {
            let backstage = self.backstage_view(bg, fg, dim, sidebar, cx);
            return v_flex().size_full().bg(bg).track_focus(&self.focus).child(title_bar).child(backstage).into_any_element();
        }

        let is_doc = matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Doc(_)));
        // The contextual Table tab is only valid while the caret is in a table.
        if self.ribbon_tab == RibbonTab::Table && self.caret_table().is_none() {
            self.ribbon_tab = RibbonTab::Home;
        }
        let vw = f32::from(window.viewport_size().width);
        let ribbon_tabs = self.ribbon_tabs(fg, dim, panel, cx);
        let ribbon_body = (!self.ribbon_min && (is_doc || self.active_is_sheet())).then(|| {
            if is_doc {
                self.ribbon_body(vw, pal, cx)
            } else {
                self.sheet_ribbon_body(pal, cx)
            }
        });
        let find_bar = (is_doc && self.find_open).then(|| self.find_bar(pal, cx));
        let picker_bar = (is_doc).then_some(self.picker).flatten().map(|k| self.picker_bar(k, pal, cx));
        let sheet_pick_bar = self.active_is_sheet().then_some(self.sheet_pick).flatten().map(|p| self.sheet_picker_bar(p, pal, cx));
        let sheet_numfmt_bar = (self.active_is_sheet() && self.sheet_numfmt_open).then(|| self.sheet_numfmt_bar(pal, cx));
        let sheet_fmt_panel = (self.active_is_sheet() && self.sheet_fmt_open).then(|| self.sheet_format_panel(pal, cx));
        let sheet_find = (self.active_is_sheet() && self.find_open).then(|| self.sheet_find_bar(pal, cx));
        let sheet_comment = self.sheet_comment_edit.clone().map(|buf| self.sheet_comment_bar(&buf, pal, cx));
        let sheet_cf = self.sheet_cf_edit.clone().map(|buf| self.sheet_cf_bar(&buf, pal, cx));
        let sheet_dv_bar = self.sheet_dv_edit.clone().map(|buf| self.sheet_dv_edit_bar(&buf, pal, cx));
        let sheet_filter = self.sheet_filter_edit.clone().map(|buf| self.sheet_filter_bar(&buf, pal, cx));
        let sheet_ttc = self.sheet_ttc_edit.clone().map(|buf| self.sheet_ttc_bar(&buf, pal, cx));
        let sheet_sort = self.sheet_sort_edit.clone().map(|buf| self.sheet_sort_bar(&buf, pal, cx));
        let sheet_rowh = self.sheet_rowh_edit.clone().map(|buf| self.sheet_rowh_bar(&buf, pal, cx));
        let comment_bar = (is_doc && self.comment_open).then(|| self.comment_bar(pal, cx));
        let hf_bar = self.hf_active().then(|| self.hf_bar(pal, cx));
        let ruler = (is_doc && self.show_ruler).then(|| self.ruler(cx));

        // Text measurer for tab-stop positioning (shared across the document).
        let measurer = Measurer::new(window);
        let content: AnyElement = match self.tabs.get(self.active) {
            Some(tab) => match &tab.surface {
                Surface::Doc(editor) => {
                    let spans = editor.selection_spans();
                    let markers = list_markers(&editor.doc.body);
                    let ent = cx.entity();
                    // In Print Layout the sheet is always a light page (dark ink on
                    // white) regardless of the app theme, like Word's document surface.
                    let doc_pal = if self.page_view {
                        Pal { fg: hsla_u(0x202020), dim: hsla_u(0x808080), border: hsla_u(0xcccccc), panel: hsla_u(0xf0f0f0), hover: Hsla { a: 0.08, ..hsla_u(0x000000) }, sel: pal.sel }
                    } else {
                        pal
                    };
                    // While a header/footer is being edited the body is inactive
                    // (no caret, clicks inert) so it visually recedes.
                    let hf = tab.hf_edit.as_ref();
                    let ctx = RenderCtx { caret_path: &editor.caret.path, caret_off: editor.caret.offset, spans: &spans, ent: &ent, pal: doc_pal, marks: self.show_marks, zoom: self.zoom, active: hf.is_none(), meas: &measurer, hf_width: None };
                    let body = &editor.doc.body;
                    if self.page_view {
                        // Print Layout: split the body into discrete white page sheets
                        // (section margins), stacked on a grey canvas.
                        let geom = tab.pkg.as_ref().map(|p| p.page_geom()).unwrap_or_default();
                        let zoom = self.zoom;
                        let tw = move |t: i32| px(zoom * (t.max(0) as f32) / 15.0); // twips → px @ ~96dpi, zoomed
                        let canvas = if self.applied == Some(ThemeMode::Dark) { hsla_u(0x2b2b2b) } else { hsla_u(0x9a9a9a) };
                        let content_h = (geom.h - geom.mt - geom.mb).max(1) as f32 / 15.0;
                        let content_w = (geom.w - geom.ml - geom.mr).max(1) as f32 / 15.0;
                        // Newspaper columns: flow the body into N columns per page.
                        let ncols = geom.cols.max(1) as usize;
                        let colgap = geom.col_space.max(0) as f32 / 15.0;
                        let col_w = if ncols > 1 { ((content_w - colgap * (ncols as f32 - 1.0)) / ncols as f32).max(1.0) } else { content_w };
                        let pages: Vec<Vec<(usize, usize)>> = if ncols > 1 {
                            paginate_cols(body, content_h, col_w, ncols)
                        } else {
                            paginate(body, content_h, content_w).into_iter().map(|r| vec![r]).collect()
                        };
                        let show_ruler = self.show_ruler;
                        // Per-page header/footer. A section can carry distinct
                        // first-page (w:titlePg) and even-page (evenAndOddHeaders)
                        // variants; every other page uses the "default" one.
                        let pkg = tab.pkg.as_ref();
                        let title_pg = pkg.is_some_and(|p| p.has_title_pg());
                        let even_odd = pkg.is_some_and(|p| p.has_even_odd());
                        let refp = |kind: &str, wt: &str| pkg.is_some_and(|p| docxcore::load::header_footer_ref_rid(p.sect_pr(), kind, wt).is_some());
                        let (h_first_ref, h_even_ref) = (refp("headerReference", "first"), refp("headerReference", "even"));
                        let (f_first_ref, f_even_ref) = (refp("footerReference", "first"), refp("footerReference", "even"));
                        let parse = |is_h: bool, wt: &str| pkg.map(|p| header_footer_blocks_typed(p, is_h, wt)).unwrap_or_default();
                        let (hdef, hfirst, heven) = (parse(true, "default"), parse(true, "first"), parse(true, "even"));
                        let (fdef, ffirst, feven) = (parse(false, "default"), parse(false, "first"), parse(false, "even"));
                        let variant_for = |page1: usize, is_h: bool| -> &'static str {
                            let (fr, ev) = if is_h { (h_first_ref, h_even_ref) } else { (f_first_ref, f_even_ref) };
                            if page1 == 1 && title_pg && fr {
                                "first"
                            } else if page1 % 2 == 0 && even_odd && ev {
                                "even"
                            } else {
                                "default"
                            }
                        };
                        let pick = |is_h: bool, wt: &str| -> &[Block] {
                            match (is_h, wt) {
                                (true, "first") => &hfirst,
                                (true, "even") => &heven,
                                (true, _) => &hdef,
                                (false, "first") => &ffirst,
                                (false, "even") => &feven,
                                (false, _) => &fdef,
                            }
                        };
                        // Header/footer text-area width, for the implicit centre/right tab stops.
                        let hf_w = self.zoom * (geom.w - geom.ml - geom.mr).max(0) as f32 / 15.0;
                        let hf_spans = hf.map(|h| h.editor.selection_spans()).unwrap_or_default();
                        let hf_ctx = hf.map(|h| RenderCtx { caret_path: &h.editor.caret.path, caret_off: h.editor.caret.offset, spans: &hf_spans, ent: &ent, pal: doc_pal, marks: false, zoom: self.zoom, active: true, meas: &measurer, hf_width: Some(hf_w) });
                        // The first page whose region+variant matches the one being
                        // edited is the editable page (fallback page 0, so the surface
                        // is always visible even for a not-yet-shown variant).
                        let edit_page = hf.map(|h| (0..pages.len()).find(|&i| variant_for(i + 1, h.is_header) == h.variant).unwrap_or(0));
                        let has_hf = [&hdef, &hfirst, &heven, &fdef, &ffirst, &feven].iter().any(|v| !v.is_empty()) || hf.is_some();
                        // One region's margin content for a given page: the live editor
                        // blocks (editable on the edit page), else the read-only variant.
                        let region_children = |pi: usize, is_h: bool| -> Vec<AnyElement> {
                            let dv = variant_for(pi + 1, is_h);
                            if let (Some(h), Some(ep)) = (hf, edit_page) {
                                if h.is_header == is_h {
                                    if pi == ep {
                                        return h.editor.doc.body.iter().enumerate().map(|(i, b)| block_el(b, vec![i], None, hf_ctx.unwrap())).collect();
                                    }
                                    let blocks: &[Block] = if dv == h.variant { &h.editor.doc.body } else { pick(is_h, dv) };
                                    return hf_els(blocks, doc_pal, &measurer, hf_w);
                                }
                            }
                            hf_els(pick(is_h, dv), doc_pal, &measurer, hf_w)
                        };
                        // The middle content of a page: a single flow, or an N-column
                        // row (each column its own block range) when in columns mode.
                        let build_mid = |cols: &[(usize, usize)]| -> AnyElement {
                            if cols.len() <= 1 {
                                let (s, e) = cols.first().copied().unwrap_or((0, 0));
                                let blocks: Vec<AnyElement> = (s..e).map(|i| block_el(&body[i], vec![i], markers[i].as_deref(), ctx)).collect();
                                return v_flex().w_full().gap_1().children(blocks).into_any_element();
                            }
                            let column_els: Vec<AnyElement> = cols
                                .iter()
                                .map(|&(s, e)| {
                                    let blocks: Vec<AnyElement> = (s..e).map(|i| block_el(&body[i], vec![i], markers[i].as_deref(), ctx)).collect();
                                    v_flex().flex_1().min_w(px(0.)).gap_1().children(blocks).into_any_element()
                                })
                                .collect();
                            h_flex().w_full().items_start().gap(tw(geom.col_space)).children(column_els).into_any_element()
                        };
                        let sheets: Vec<AnyElement> = pages
                            .iter()
                            .enumerate()
                            .map(|(pi, cols_ranges)| {
                                let hdr_children = region_children(pi, true);
                                let ftr_children = region_children(pi, false);
                                let edit_hdr_here = hf.is_some_and(|h| h.is_header) && Some(pi) == edit_page;
                                let edit_ftr_here = hf.is_some_and(|h| !h.is_header) && Some(pi) == edit_page;
                                let page_base = v_flex()
                                    .w(tw(geom.w))
                                    .min_h(tw(geom.h))
                                    .bg(hsla_u(0xffffff))
                                    .text_color(doc_pal.fg)
                                    .border_1()
                                    .border_color(hsla_u(0xd0d0d0));
                                // Tint the region actively being edited on this page.
                                let tint = Hsla { a: 0.5, ..hsla_u(0xeef4ff) };
                                let hdr_bg = if edit_hdr_here { tint } else { hsla_u(0xffffff) };
                                let ftr_bg = if edit_ftr_here { tint } else { hsla_u(0xffffff) };
                                let page = if has_hf {
                                    // Header in the top margin, content in the middle, footer
                                    // in the bottom margin. The body area exits header/footer
                                    // editing on click (Word's "click the document to leave").
                                    let mut mid = v_flex().flex_1().pl(tw(geom.ml)).pr(tw(geom.mr)).child(build_mid(cols_ranges));
                                    if hf.is_some() {
                                        let ent2 = ent.clone();
                                        mid = mid.cursor_pointer().on_mouse_down(MouseButton::Left, move |_ev, window, cx| {
                                            ent2.update(cx, |this, cx| this.exit_hf(window, cx));
                                        });
                                    }
                                    page_base
                                        .child(div().min_h(tw(geom.mt)).pt(tw(geom.mt / 2)).pl(tw(geom.ml)).pr(tw(geom.mr)).bg(hdr_bg).children(hdr_children))
                                        .child(mid)
                                        .child(div().min_h(tw(geom.mb)).pl(tw(geom.ml)).pr(tw(geom.mr)).bg(ftr_bg).children(ftr_children))
                                } else {
                                    page_base.pt(tw(geom.mt)).pr(tw(geom.mr)).pb(tw(geom.mb)).pl(tw(geom.ml)).child(build_mid(cols_ranges))
                                };
                                h_flex()
                                    .items_stretch()
                                    .gap(px(3.))
                                    .when(show_ruler, |d| d.child(self.vruler()))
                                    .child(page)
                                    .into_any_element()
                            })
                            .collect();
                        v_flex()
                            .id("doc-scroll")
                            .track_scroll(&self.doc_scroll)
                            .flex_1()
                            .h_full()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .bg(canvas)
                            .items_center()
                            .gap(px(18.))
                            .py(px(24.))
                            .children(sheets)
                            .into_any_element()
                    } else {
                        let blocks: Vec<AnyElement> = body.iter().enumerate().map(|(i, b)| block_el(b, vec![i], markers[i].as_deref(), ctx)).collect();
                        v_flex().id("doc-scroll").track_scroll(&self.doc_scroll).flex_1().h_full().min_h(px(0.)).overflow_y_scroll().bg(bg).text_color(fg).px(px(48.)).py(px(28.)).gap_1().children(blocks).into_any_element()
                    }
                }
                Surface::Sheet(v) => sheet_el(v, &cx.entity(), self.sheet_rename.clone(), self.sheet_comment_edit.is_some(), sheet_dv.clone(), self.sheet_dv_open, sheet_grid_w, cx).into_any_element(),
                Surface::Placeholder => placeholder(tab.kind, bg, dim).into_any_element(),
            },
            None => v_flex().flex_1().bg(bg).items_center().justify_center().text_color(dim).child("No documents — File \u{203A} New").into_any_element(),
        };

        // Word-style counts for the status bar's left cluster: total pages
        // (estimated by the same paginator Print Layout uses) and word count.
        let doc_stats: Option<(usize, usize)> = is_doc
            .then(|| self.tabs.get(self.active))
            .flatten()
            .and_then(|tab| match &tab.surface {
                Surface::Doc(ed) => {
                    let words = ed.doc.plain_text().split_whitespace().count();
                    let geom = tab.pkg.as_ref().map(|p| p.page_geom()).unwrap_or_default();
                    let ch = (geom.h - geom.mt - geom.mb).max(1) as f32 / 15.0;
                    let cw = (geom.w - geom.ml - geom.mr).max(1) as f32 / 15.0;
                    let pages = paginate(&ed.doc.body, ch, cw).len().max(1);
                    Some((words, pages))
                }
                _ => None,
            });
        let stats_text = doc_stats.map(|(w, p)| {
            SharedString::from(format!(
                "{} page{} \u{00b7} {} word{}",
                p,
                if p == 1 { "" } else { "s" },
                w,
                if w == 1 { "" } else { "s" }
            ))
        });

        let zoom_btn = |cx: &mut Context<Self>, id: &'static str, glyph: &'static str, delta: f32| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded_sm()
                .cursor_pointer()
                .text_color(fg)
                .hover(|d| d.bg(pal.hover))
                .child(glyph)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let z = if delta == 0.0 { 1.0 } else { this.zoom + delta };
                    this.zoom = z.clamp(0.5, 3.0);
                    this.refocus(window, cx);
                }))
        };
        let status = h_flex()
            .w_full()
            .px_4()
            .py_1()
            .gap_2()
            .bg(panel)
            .text_size(px(11.))
            .text_color(dim)
            .child(self.tabs.get(self.active).map(|t| t.status.clone()).unwrap_or_default())
            .when_some(stats_text, |d, s| d.child(div().text_color(dim).child("·")).child(div().text_color(dim).child(s)))
            .child(div().flex_1())
            .child(if self.active_is_sheet() { "type or F2 to edit · Enter/Tab to move · =formula · Ctrl+S save" } else { "type · Ctrl+B/I/U · Ctrl+F find · Ctrl+C/X/V · Ctrl+Z/Y · Ctrl+S" })
            // Zoom controls (Word's bottom-right zoom).
            .child(zoom_btn(cx, "zoom-out", "\u{2212}", -0.1))
            .child(div().id("zoom-pct").min_w(px(34.)).flex().justify_center().cursor_pointer().hover(|d| d.text_color(fg)).child(SharedString::from(format!("{}%", (self.zoom * 100.0).round() as i32))).on_click(cx.listener(|this, _, window, cx| { this.zoom = 1.0; this.refocus(window, cx); })))
            .child(zoom_btn(cx, "zoom-in", "+", 0.1));

        // The body is the document, flanked by the navigation and comments panes.
        let nav_panel = (is_doc && self.show_nav).then(|| self.nav_panel(pal, cx));
        let comments_panel = (is_doc && self.show_comments).then(|| self.comments_panel(pal, cx));
        let notes_panel = (is_doc && self.show_notes).then(|| self.notes_panel(pal, cx));
        // The PivotTable Fields panel, shown when a pivot output sheet is active.
        let pivot_panel = self.active_pivot().map(|i| self.pivot_panel(i, pal, cx));
        let body = h_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            // Right-click anywhere in the document body opens the context menu.
            .on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                this.context_menu = Some(ev.position);
                cx.notify();
            }))
            .when_some(nav_panel, |d, n| d.child(n))
            .child(content)
            .when_some(comments_panel, |d, p| d.child(p))
            .when_some(notes_panel, |d, p| d.child(p))
            .when_some(pivot_panel, |d, p| d.child(p));
        let context_menu = self.context_menu.map(|at| self.context_menu_el(at, pal, cx));
        let mini_bar = (is_doc && self.context_menu.is_none()).then_some(self.mini_bar).flatten().map(|at| self.mini_bar_el(at, pal, cx));

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &InsertTabAction, window, cx| this.tab_key(window, cx)))
            .on_action(cx.listener(|this, _: &OutdentAction, window, cx| this.shift_tab_key(window, cx)))
            // Ruler drags are tracked at the window level so they keep working when
            // the pointer leaves the thin ruler strip (gpui move events are
            // hitbox-scoped, so a ruler-only handler would stop the moment the
            // cursor moved off it).
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                if this.ruler_drag.is_some() {
                    this.ruler_drag_move(f32::from(ev.position.x), window, cx);
                }
            }))
            // End a text drag-selection or a ruler drag wherever the button is
            // released; a non-empty text selection pops the mini formatting toolbar.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                if this.ruler_drag.is_some() {
                    this.ruler_drag_end(cx);
                }
                if this.selecting {
                    this.selecting = false;
                    let has_sel = matches!(this.tabs.get(this.active).map(|t| &t.surface), Some(Surface::Doc(ed)) if ed.has_selection());
                    this.mini_bar = has_sel.then_some(ev.position);
                    cx.notify();
                }
            }))
            .bg(bg)
            .child(title_bar)
            .child(ribbon_tabs)
            .when_some(ribbon_body, |d, r| d.child(r))
            .when_some(find_bar, |d, f| d.child(f))
            .when_some(picker_bar, |d, p| d.child(p))
            .when_some(sheet_pick_bar, |d, p| d.child(p))
            .when_some(sheet_numfmt_bar, |d, b| d.child(b))
            .when_some(sheet_find, |d, f| d.child(f))
            .when_some(sheet_comment, |d, c| d.child(c))
            .when_some(sheet_cf, |d, c| d.child(c))
            .when_some(sheet_dv_bar, |d, c| d.child(c))
            .when_some(sheet_filter, |d, c| d.child(c))
            .when_some(sheet_ttc, |d, c| d.child(c))
            .when_some(sheet_sort, |d, c| d.child(c))
            .when_some(sheet_rowh, |d, c| d.child(c))
            .when_some(comment_bar, |d, c| d.child(c))
            .when_some(hf_bar, |d, b| d.child(b))
            .when_some(ruler, |d, r| d.child(r))
            .child(body)
            .child(status)
            .when_some(mini_bar, |d, m| d.child(m))
            .when_some(context_menu, |d, m| d.child(m))
            .when_some(sheet_fmt_panel, |d, p| d.child(p))
            // A vertical guide line down the page while a ruler marker is dragged.
            .when_some(self.ruler_guide, |d, gx| {
                d.child(div().absolute().top_0().bottom_0().left(px(gx)).w(px(1.)).bg(Hsla { a: 0.6, ..hsla_u(BRAND) }))
            })
            .into_any_element()
    }
}

/// Excel column width (character units) → pixels, clamped to a sane range.
fn col_px(units: f64) -> f32 {
    ((units * 7.0 + 6.0) as f32).clamp(28.0, 320.0)
}

// ---- grid geometry (pure; unit-tested in `grid_geom_tests`) --------------
// These mirror the exact math the row/header renderers and the h-scroll
// reconciler use, extracted so they can be regression-tested without a gpui
// window (the gpui TestAppContext render path is infeasible here). The
// renderers below CALL these — they are the single source of truth, so a test
// failure means the on-screen grid geometry changed.

/// The last column that renders (inclusive) in the scrollable window: starting
/// at `col0`, keep adding columns until their pixel widths exceed `avail` (the
/// overflowing column is still included, so it clips at the edge like Excel),
/// bounded by `maxcol`. Always returns at least `col0`.
fn last_visible_col(col_w_px: impl Fn(u32) -> f32, col0: u32, avail: f32, maxcol: u32) -> u32 {
    let mut cend = col0;
    let mut wsum = 0.0f32;
    loop {
        wsum += col_w_px(cend);
        if (wsum > avail && cend > col0) || cend >= maxcol {
            break;
        }
        cend += 1;
    }
    cend
}

/// The new leftmost-visible column so the selected column `sc` stays on screen:
/// if it's left of the window, snap to it; if right, shrink the window from the
/// left until `[col0..=sc]` fits `avail`. `fc` = frozen column count (never
/// scrolled past). Caller handles `sc < fc` (a frozen column is always visible).
fn scroll_col0_for_sel(col_w_px: impl Fn(u32) -> f32, col0: u32, fc: u32, sc: u32, avail: f32) -> u32 {
    if sc < col0 {
        return sc.max(fc);
    }
    let mut start = col0;
    let mut sum: f32 = (start..=sc).map(&col_w_px).sum();
    while sum > avail && start < sc {
        sum -= col_w_px(start);
        start += 1;
    }
    start.max(fc)
}

/// A row's pixel height: an explicit `<row ht>` (points) scaled at the app's
/// 15pt≈`base`px, else `base`. Wrapped cells grow the row beyond this at layout
/// time; this is the min-height floor.
fn row_height_px(explicit_pt: Option<f64>, base: f32) -> f32 {
    explicit_pt.map(|ht| (ht as f32) * (base / 15.0)).unwrap_or(base)
}

/// Split a ribbon button label into at most two lines at a word boundary, so a
/// large button wraps like Word/Excel ("Conditional" / "Formatting") instead of
/// breaking mid-word. Short or single-word labels stay on one line.
fn label_lines(label: &str) -> Vec<String> {
    if label.len() <= 9 || !label.contains(' ') {
        return vec![label.to_string()];
    }
    let mid = label.len() as isize / 2;
    let split = label
        .char_indices()
        .filter(|(_, c)| *c == ' ')
        .min_by_key(|(i, _)| (*i as isize - mid).abs())
        .map(|(i, _)| i);
    match split {
        Some(i) => vec![label[..i].to_string(), label[i + 1..].to_string()],
        None => vec![label.to_string()],
    }
}

const SHEET_ROW_H: f32 = 21.0;
const SHEET_GUT: f32 = 46.0;

/// The frozen column-letter header row (with drag-to-resize handles). Rendered
/// once above the virtualized rows so it stays put while they scroll vertically.
fn sheet_col_header(view: &SheetView, ent: &Entity<Docxy>, fc: u32, col0: u32, cend: u32) -> AnyElement {
    use gridcore::sheet::col_name;
    let sh = view.sheet();
    let gridline = hsla_u(0xd9d9d9);
    let freeze_line = hsla_u(0x8a8a8a);
    let head_bg = hsla_u(0xf1f1f1);
    let head_fg = hsla_u(0x5a5a5a);
    let brand = hsla_u(BRAND);
    let (_, c0, _, c1) = view.range();
    let mut header = h_flex().child(div().w(px(SHEET_GUT)).flex_shrink_0().h(px(SHEET_ROW_H)).bg(head_bg).border_r_1().border_b_1().border_color(gridline));
    // Frozen columns 0..fc pinned, then the scrollable window col0..=cend.
    for c in (0..fc).chain(col0..=cend) {
        let hl = c >= c0 && c <= c1;
        let on_freeze = fc > 0 && c + 1 == fc;
        let ent_h = ent.clone();
        let handle = div()
            .absolute()
            .top_0()
            .right_0()
            .w(px(5.))
            .h(px(SHEET_ROW_H))
            .cursor_col_resize()
            .on_mouse_down(MouseButton::Left, move |ev, _w, cx| {
                let x = f32::from(ev.position.x);
                ent_h.update(cx, |this, cx| this.col_resize_start(c, x, cx));
            });
        header = header.child(
            div().relative().w(px(col_px(sh.col_width(c)))).flex_shrink_0().h(px(SHEET_ROW_H)).flex().items_center().justify_center()
                .bg(if hl { brand } else { head_bg })
                .border_r_1().border_b_1().border_color(if on_freeze { freeze_line } else { gridline })
                .text_size(px(11.)).text_color(if hl { hsla_u(0xffffff) } else { head_fg })
                .child(SharedString::from(col_name(c)))
                .child(handle),
        );
    }
    header.into_any_element()
}

/// One data row: the row-number gutter cell plus the visible cells (frozen
/// columns `0..fc` pinned, then the scrollable window `col0..=cend`).
fn sheet_row(view: &SheetView, ent: &Entity<Docxy>, r: u32, fc: u32, col0: u32, cend: u32, comment_cells: &std::collections::HashSet<(u32, u32)>) -> AnyElement {
    use gridcore::sheet::{Align, CellValue};
    let sh = view.sheet();
    let styles = &view.pkg.workbook.styles;
    let d1904 = view.pkg.workbook.date1904;
    // The active sheet index for conditional-formatting lookups.
    let sidx = view.active.min(view.pkg.workbook.sheets.len().saturating_sub(1));
    let has_cf = !sh.cond_formats.is_empty();
    let (sr, sc) = view.sel;
    let (r0, c0, r1, c1) = view.range();
    let editing = view.editing.clone();
    let gridline = hsla_u(0xd9d9d9);
    let freeze_line = hsla_u(0x8a8a8a);
    let head_bg = hsla_u(0xf1f1f1);
    let head_fg = hsla_u(0x5a5a5a);
    let brand = hsla_u(BRAND);
    let range_tint = Hsla { a: 0.14, ..brand };
    let hl_row = r >= r0 && r <= r1;
    // Variable row height: an explicit <row ht> sets a floor (points → px at the
    // app's 15pt≈21px scale); wrapped cells grow the row past it via their
    // natural (min-content) height. items_stretch makes every cell fill it.
    let min_row_h = row_height_px(sh.row_height(r), SHEET_ROW_H);
    let mut row = h_flex().items_stretch().min_h(px(min_row_h)).child(
        div().w(px(SHEET_GUT)).flex_shrink_0().flex().items_center().justify_center()
            .bg(if hl_row { brand } else { head_bg })
            .border_r_1().border_b_1().border_color(gridline)
            .text_size(px(11.)).text_color(if hl_row { hsla_u(0xffffff) } else { head_fg })
            .child(SharedString::from((r + 1).to_string())),
    );
    // Merged regions: the top-left cell spans its columns' combined width; cells
    // it covers in the same row are skipped; cells under a vertical merge render
    // blank (content lives only in the top-left).
    let mut skip_to: i64 = -1;
    for c in (0..fc).chain(col0..=cend) {
        if (c as i64) <= skip_to {
            continue;
        }
        let merge = sh.merges.iter().find(|&&(mr1, mc1, mr2, mc2)| r >= mr1 && r <= mr2 && c >= mc1 && c <= mc2).copied();
        let (cell_w, blank_covered) = match merge {
            Some((mr1, mc1, _mr2, mc2)) if r == mr1 && c == mc1 => {
                skip_to = mc2 as i64; // widen; skip the rest of the span in this row
                ((mc1..=mc2).map(|cc| col_px(sh.col_width(cc))).sum::<f32>(), false)
            }
            Some((mr1, _, _, _)) if r == mr1 => continue, // covered in the top row
            Some(_) => (col_px(sh.col_width(c)), true), // under a vertical merge → blank
            None => (col_px(sh.col_width(c)), false),
        };
        let selected = (r, c) == (sr, sc);
        let in_range = r >= r0 && r <= r1 && c >= c0 && c <= c1;
        let cell_editing = selected && editing.is_some();
        let on_freeze = fc > 0 && c + 1 == fc;
        let (text, xf, is_num) = match sh.cell(r, c) {
            _ if blank_covered => (String::new(), None, false),
            Some(cl) if !cl.is_blank() => {
                let xf = styles.xf(cl.style);
                (gridcore::sheet::format_with(&xf, &cl.value, d1904), Some(xf), matches!(cl.value, CellValue::Number(_)))
            }
            _ => (String::new(), None, false),
        };
        let halign = match xf.as_ref().map(|x| x.align).unwrap_or(Align::General) {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
            Align::General => if is_num { 2 } else { 0 },
        };
        let mut fill = xf.as_ref().and_then(|x| x.fill);
        let mut color_rgb = xf.as_ref().and_then(|x| x.color);
        let mut bold = xf.as_ref().is_some_and(|x| x.bold);
        let mut italic = xf.as_ref().is_some_and(|x| x.italic);
        // Conditional formatting overlays the base style where a rule matches.
        if has_cf {
            if let Some(dxf) = gridcore::cf::cell_dxf(&view.pkg.workbook, sidx, r, c) {
                if dxf.fill.is_some() {
                    fill = dxf.fill;
                }
                if dxf.color.is_some() {
                    color_rgb = dxf.color;
                }
                if let Some(b) = dxf.bold {
                    bold = b;
                }
                if let Some(i) = dxf.italic {
                    italic = i;
                }
            }
        }
        let color = color_rgb.map(|(r, g, b)| rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32)).unwrap_or(rgb(0x1a1a1a));
        let bg = if let Some((r, g, b)) = fill { rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into() } else { hsla_u(0xffffff) };
        let cell_border = xf.as_ref().is_some_and(|x| x.border);
        let wrap = xf.as_ref().is_some_and(|x| x.wrap);
        let mut cell = div()
            .id(ElementId::Name(format!("cell-{r}-{c}").into()))
            .w(px(cell_w))
            .flex_shrink_0()
            .px(px(4.))
            .py(px(2.))
            .flex()
            // Wrapped cells top-align and let text flow onto multiple lines
            // (growing the row); plain cells stay single-line and clip.
            .map(|d| if wrap { d.items_start() } else { d.items_center().overflow_hidden() })
            .bg(if cell_editing { hsla_u(0xffffff) } else { bg })
            .border_r_1()
            .border_b_1()
            .border_color(if on_freeze { freeze_line } else { gridline })
            // A thin box border (xf border) darkens all four sides.
            .when(cell_border, |d| d.border_1().border_color(hsla_u(0x7a7a7a)))
            .when(in_range && !selected, |d| d.bg(range_tint))
            .when(selected, |d| d.border_2().border_color(brand));
        if cell_editing {
            cell = cell
                .justify_start()
                .child(edit_caret_row(&editing.clone().unwrap_or_default(), view.edit_caret, hsla_u(0x1a1a1a), brand));
        } else {
            cell = match halign {
                1 => cell.justify_center(),
                2 => cell.justify_end(),
                _ => cell.justify_start(),
            };
            if !text.is_empty() {
                // Hyperlinked cells render as underlined blue and follow on click.
                let is_link = sh.hyperlinks.contains_key(&(r, c));
                cell = cell.child(
                    div()
                        .text_size(px(12.))
                        .text_color(if is_link { rgb(0x0563c1) } else { color })
                        .when(is_link, |d| d.underline())
                        .when(bold, |d| d.font_weight(FontWeight::BOLD))
                        .when(italic, |d| d.italic())
                        // Wrap onto multiple lines (bounded to the cell width so
                        // it actually breaks), or clip to one line.
                        .map(|d| if wrap { d.whitespace_normal().w_full() } else { d.whitespace_nowrap() })
                        .child(SharedString::from(text)),
                );
            }
        }
        let has_link = sh.hyperlinks.contains_key(&(r, c));
        let ent2 = ent.clone();
        cell = cell
            .on_click(move |ev, window, cx| {
                let shift = ev.modifiers().shift;
                let dbl = ev.click_count() >= 2;
                ent2.update(cx, |this, cx| {
                    if shift {
                        this.extend_to(r, c, cx)
                    } else {
                        this.select_cell(r, c, cx);
                        if dbl {
                            // Double-click enters inline edit mode (Excel-style).
                            this.sheet_begin_edit(None, cx);
                        } else if has_link {
                            this.sheet_follow_hyperlink(r, c, cx);
                        }
                    }
                    // Keep keyboard focus on the grid after a click inside the
                    // virtualized list (which would otherwise capture it).
                    this.focus.focus(window, cx);
                });
            });
        // Drag-select: while the left button is held, extend the selection to
        // whatever cell the pointer is over (on_mouse_move is hitbox-scoped, so
        // one fires per cell crossed).
        let ent_drag = ent.clone();
        cell = cell.on_mouse_move(move |ev, _window, cx| {
            if ev.pressed_button == Some(MouseButton::Left) {
                ent_drag.update(cx, |this, cx| {
                    if this.sheet_fill.is_some() {
                        this.sheet_fill_over(r, c, cx);
                    } else {
                        this.sheet_drag_over(r, c, cx);
                    }
                });
            }
        });
        // Auto-fill handle: the small square at the selection's bottom-right
        // corner. Dragging it fills the source pattern into the dragged region.
        if !cell_editing && r == r1 && c == c1 {
            let ent_fill = ent.clone();
            cell = cell.relative().child(
                // A generous transparent grab zone in the bottom-right corner
                // (easy to grab / not clipped by the cell), with the small
                // visible square at its corner.
                div()
                    .absolute()
                    .bottom(px(0.))
                    .right(px(0.))
                    .w(px(12.))
                    .h(px(12.))
                    .flex()
                    .items_end()
                    .justify_end()
                    .cursor(CursorStyle::Crosshair)
                    .on_mouse_move(move |ev, _w, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            ent_fill.update(cx, |this, cx| this.sheet_fill_start(cx));
                        }
                    })
                    .child(div().w(px(8.)).h(px(8.)).bg(brand).border_1().border_color(hsla_u(0xffffff))),
            );
        }
        // Red corner marker for a commented cell (Excel's note indicator).
        if comment_cells.contains(&(r, c)) {
            cell = cell.relative().child(
                div().absolute().top_0().right_0().w(px(0.)).h(px(0.))
                    .border_t(px(5.)).border_r(px(5.))
                    .border_color(hsla_u(0xd0322b)),
            );
        }
        row = row.child(cell);
    }
    row.into_any_element()
}

/// A floating chart card: a clustered column chart drawn with div bars, plus a
/// title and legend. Handles bar/column data (the common case) for any kind.
fn chart_card(data: &gridcore::sheet::ChartData) -> AnyElement {
    const PALETTE: [u32; 6] = [0x2AA79B, 0x2F6FDB, 0xC0705A, 0xD8A44A, 0x7A5EA8, 0x5A9E5A];
    let maxv = data.series.iter().flat_map(|s| s.values.iter().copied()).fold(0.0f64, f64::max).max(1.0);
    let ncat = data.categories.len().max(data.series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    let plot_h = 148.0f32;
    let cat_label = |ci: usize| {
        let label = data.categories.get(ci).cloned().unwrap_or_default();
        div().text_size(px(8.)).text_color(hsla_u(0x666666)).max_w(px(52.)).overflow_hidden().child(SharedString::from(label))
    };

    // The plot area is drawn differently per chart kind. Column/Bar/Line share a
    // per-series legend; Pie's slices are per-category, so it builds its own.
    let kind = data.kind.as_str();
    let mut pie_legend: Option<AnyElement> = None;
    let plot: AnyElement = match kind {
        "bar" => {
            // Horizontal bars: one row per category, width proportional to value.
            let mut col = v_flex().flex_1().gap(px(3.)).px_2().py_2().justify_center();
            for ci in 0..ncat {
                let mut row = h_flex().items_center().gap(px(4.)).h(px(16.));
                row = row.child(div().w(px(46.)).text_size(px(8.)).text_color(hsla_u(0x666666)).overflow_hidden().child(SharedString::from(data.categories.get(ci).cloned().unwrap_or_default())));
                let mut bars = v_flex().flex_1().gap(px(1.));
                for (si, s) in data.series.iter().enumerate() {
                    let val = s.values.get(ci).copied().unwrap_or(0.0);
                    let frac = (val.max(0.0) / maxv) as f32;
                    bars = bars.child(div().h(px(6.)).w(relative(frac.clamp(0.02, 1.0))).rounded_r(px(1.)).bg(rgb(PALETTE[si % PALETTE.len()])));
                }
                row = row.child(bars);
                col = col.child(row);
            }
            col.h(px(plot_h + 20.)).into_any_element()
        }
        "line" => {
            // Point/line preview: each series' value plotted as a dot at its height.
            let mut plot = h_flex().h(px(plot_h + 20.)).items_end().gap(px(6.)).px_2().pt_2();
            for ci in 0..ncat {
                let mut stack = div().relative().w(px(14.)).h(px(plot_h));
                for (si, s) in data.series.iter().enumerate() {
                    let val = s.values.get(ci).copied().unwrap_or(0.0);
                    let h = ((val.max(0.0) / maxv) as f32 * plot_h).clamp(1.0, plot_h);
                    stack = stack.child(div().absolute().bottom(px(h - 3.5)).left(px(3.5)).size(px(7.)).rounded(px(4.)).bg(rgb(PALETTE[si % PALETTE.len()])));
                }
                plot = plot.child(v_flex().items_center().justify_end().gap(px(2.)).h(px(plot_h + 18.)).child(stack).child(cat_label(ci)));
            }
            plot.into_any_element()
        }
        "pie" => {
            // Pie preview as a 100%-stacked proportion bar; slices = categories,
            // proportions from the first series. Legend is per-category.
            let vals: Vec<f64> = (0..ncat).map(|ci| data.series.first().and_then(|s| s.values.get(ci)).copied().unwrap_or(0.0).max(0.0)).collect();
            let total = vals.iter().sum::<f64>().max(1.0);
            let mut bar = h_flex().w_full().h(px(30.)).rounded(px(4.)).overflow_hidden();
            let mut leg = h_flex().gap_3().px_2().pb_1().flex_wrap();
            for ci in 0..ncat {
                let frac = (vals[ci] / total) as f32;
                bar = bar.child(div().h_full().w(relative(frac.max(0.0))).bg(rgb(PALETTE[ci % PALETTE.len()])));
                leg = leg.child(h_flex().items_center().gap_1()
                    .child(div().size(px(9.)).rounded(px(2.)).bg(rgb(PALETTE[ci % PALETTE.len()])))
                    .child(div().text_size(px(9.)).text_color(hsla_u(0x333333)).child(SharedString::from(data.categories.get(ci).cloned().unwrap_or_default()))));
            }
            pie_legend = Some(leg.into_any_element());
            v_flex().flex_1().justify_center().gap(px(6.)).px_3().py_2().h(px(plot_h + 20.)).child(bar).into_any_element()
        }
        _ => {
            // Column (default): vertical clustered bars.
            let mut plot = h_flex().h(px(plot_h + 20.)).items_end().gap(px(6.)).px_2().pt_2();
            for ci in 0..ncat {
                let mut cluster = h_flex().items_end().gap(px(1.));
                for (si, s) in data.series.iter().enumerate() {
                    let val = s.values.get(ci).copied().unwrap_or(0.0);
                    let h = ((val.max(0.0) / maxv) as f32 * plot_h).clamp(1.0, plot_h);
                    cluster = cluster.child(div().w(px(11.)).h(px(h)).rounded_t(px(1.)).bg(rgb(PALETTE[si % PALETTE.len()])));
                }
                plot = plot.child(v_flex().items_center().justify_end().gap(px(2.)).h(px(plot_h + 18.)).child(cluster).child(cat_label(ci)));
            }
            plot.into_any_element()
        }
    };

    let legend: AnyElement = pie_legend.unwrap_or_else(|| {
        let mut legend = h_flex().gap_3().px_2().pb_1().flex_wrap();
        for (si, s) in data.series.iter().enumerate() {
            legend = legend.child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(9.)).rounded(px(2.)).bg(rgb(PALETTE[si % PALETTE.len()])))
                    .child(div().text_size(px(9.)).text_color(hsla_u(0x333333)).child(SharedString::from(s.name.clone()))),
            );
        }
        legend.into_any_element()
    });
    v_flex()
        .w(px(360.))
        .bg(hsla_u(0xffffff))
        .border_1()
        .border_color(hsla_u(0xcccccc))
        .rounded(px(4.))
        .child(div().w_full().text_center().py_1().text_size(px(12.)).font_weight(FontWeight::BOLD).text_color(hsla_u(0x222222)).child(SharedString::from(data.title.clone())))
        .child(plot)
        .child(legend)
        .into_any_element()
}

/// Render a spreadsheet tab: a formula/reference bar; a horizontally-scrolling
/// grid whose column header (and any frozen rows) stay pinned while the rows
/// virtualize vertically via `uniform_list`; and the sheet tabs.
fn sheet_el(view: &SheetView, ent: &Entity<Docxy>, rename: Option<(usize, String)>, comment_editing: bool, dv_values: Option<Vec<String>>, dv_open: bool, grid_w: f32, cx: &mut Context<Docxy>) -> AnyElement {
    use gridcore::sheet::cell_name;
    let sh = view.sheet();
    let (sr, sc) = view.sel;
    let (max_r, max_c) = view.extent();
    // Frozen columns (freeze panes, cols axis): pinned at the left, always shown.
    let (frz_r, frz_c) = sh.freeze;
    let fc = frz_c.min(64);
    let frozen_w: f32 = (0..fc).map(|c| col_px(sh.col_width(c))).sum();
    // Cells carrying a comment (red corner marker); parsed once per render.
    let comment_cells: std::rc::Rc<std::collections::HashSet<(u32, u32)>> = std::rc::Rc::new(
        view.pkg.comments().into_iter().filter(|c| c.sheet == view.active).map(|c| (c.row, c.col)).collect(),
    );
    // Horizontal column window: frozen cols 0..fc are always drawn; the scrollable
    // window fills the REMAINING width from the scroll offset col0 (kept >= fc).
    // Column virtualization by offset — the counterpart to the row uniform_list.
    let col0 = view.col0.max(fc).min(255);
    let avail = (grid_w - SHEET_GUT - frozen_w).max(80.0);
    let cend = last_visible_col(|c| col_px(sh.col_width(c)), col0, avail, 255);
    // Total rows to virtualize over: the used range plus generous headroom.
    let total_rows = ((max_r + 100).max(500)) as usize;
    let gridline = hsla_u(0xd9d9d9);
    let (r0, c0, r1, c1) = view.range();

    let editing = view.editing.clone();
    // ---- formula / reference bar ----
    let sel_ref = if view.has_range() {
        format!("{}:{}", cell_name(r0, c0), cell_name(r1, c1))
    } else {
        cell_name(sr, sc)
    };
    let sel_content = if let Some(buf) = &editing {
        buf.clone()
    } else {
        match sh.cell(sr, sc) {
            Some(c) if c.formula.is_some() => format!("={}", c.formula.as_deref().unwrap_or_default()),
            _ => view.cell_text(sr, sc),
        }
    };
    let bar = h_flex()
        .w_full()
        .h(px(26.))
        .items_center()
        .gap_2()
        .px_2()
        .bg(hsla_u(0xfafafa))
        .border_b_1()
        .border_color(gridline)
        .child(div().min_w(px(64.)).px_2().py(px(2.)).rounded_sm().bg(hsla_u(0xffffff)).border_1().border_color(gridline).text_size(px(12.)).text_color(hsla_u(0x333333)).child(SharedString::from(sel_ref)))
        .child(div().text_size(px(13.)).text_color(hsla_u(0x888888)).child("fx"))
        .child(if let Some(buf) = &editing {
            // Editing: the live buffer with the caret; clicking places the caret
            // under the pointer (typing/arrows/backspace land in this buffer via
            // the grid's keyboard focus).
            fx_edit_row(buf, view.edit_caret, ent)
        } else {
            // Not editing: clicking the bar starts editing the selected cell.
            let ent_fx = ent.clone();
            div()
                .id("fx-edit")
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .cursor_text()
                .text_size(px(12.))
                .text_color(hsla_u(0x1a1a1a))
                .child(SharedString::from(sel_content))
                .on_click(move |_ev, window, cx| {
                    ent_fx.update(cx, |this, cx| {
                        this.sheet_begin_edit(None, cx);
                        this.focus.focus(window, cx);
                    });
                })
                .into_any_element()
        });

    // ---- frozen column header + frozen rows + vertically-virtualized rows ----
    // The rows go straight into a `uniform_list` (no horizontal-scroll wrapper —
    // wrapping it in `overflow_x` steals the wheel and breaks vertical scrolling).
    // Columns are rendered to fill the viewport; a horizontal scroller can't be
    // layered on without losing virtualization on raw gpui.
    let header = sheet_col_header(view, ent, fc, col0, cend);
    let cc_frozen = comment_cells.clone();
    let cc_list = comment_cells.clone();
    // Visible rows (filter/hide skips `hidden="1"` rows) — the list virtualizes
    // over these, so a filtered-out row collapses instead of showing blank.
    let visible: std::rc::Rc<Vec<u32>> = std::rc::Rc::new((0..total_rows as u32).filter(|r| !sh.row_hidden(*r)).collect());
    // Frozen top rows (Excel freeze panes, rows axis): pinned below the header,
    // outside the virtualized list, so they stay put while the rest scrolls.
    let fr = (frz_r as usize).min(visible.len()).min(30);
    let mut frozen = v_flex().flex_none();
    for i in 0..fr {
        frozen = frozen.child(sheet_row(view, ent, visible[i], fc, col0, cend, &cc_frozen));
    }
    let ent_list = ent.clone();
    let vis_list = visible.clone();
    // Keep the list's item count in sync with the scrollable-row count. Only a
    // count change forces a reset (which drops scroll); height changes to
    // visible rows are re-measured automatically each layout.
    let scrollable = visible.len().saturating_sub(fr);
    if view.vlist.item_count() != scrollable {
        view.vlist.reset(scrollable);
    }
    let list = list(view.vlist.clone(), move |ix, _w, app| {
        let this = ent_list.read(app);
        let Some(v) = this.active_sheet() else { return div().into_any_element() };
        let row = vis_list.get(fr + ix).copied().unwrap_or(0);
        sheet_row(v, &ent_list, row, fc, col0, cend, &cc_list)
    })
    .with_sizing_behavior(ListSizingBehavior::Auto)
    .flex_1()
    .min_h(px(0.));
    let grid_area = v_flex()
        .flex_1()
        .min_h(px(0.))
        // Shift+wheel scrolls columns; plain wheel is left entirely alone so the
        // row uniform_list keeps its own vertical scrolling. When we DO act on a
        // shift-wheel we consume the event, so the list doesn't also scroll.
        .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, cx| {
            if !ev.modifiers.shift {
                return;
            }
            let dy = match ev.delta {
                ScrollDelta::Lines(p) => p.y,
                ScrollDelta::Pixels(p) => f32::from(p.y) / SHEET_ROW_H,
            };
            let step = if dy < 0.0 { 1 } else if dy > 0.0 { -1 } else { 0 };
            if step != 0 {
                this.sheet_hscroll(step, cx);
                cx.stop_propagation();
            }
        }))
        .child(header)
        .child(frozen)
        .child(list);

    // ---- sheet tabs (bottom) ----
    let nsheets = view.pkg.workbook.sheets.len();
    let mut tabs = h_flex().w_full().h(px(26.)).items_center().gap(px(1.)).px_2().bg(hsla_u(0xf1f1f1)).border_t_1().border_color(gridline);
    for (i, s) in view.pkg.workbook.sheets.iter().enumerate() {
        let active = i == view.active;
        let renaming = rename.as_ref().is_some_and(|(ri, _)| *ri == i);
        let mut tab = div()
            .id(ElementId::Name(format!("sheet-tab-{i}").into()))
            .px_3().h(px(20.)).flex().items_center().gap_1().rounded_t(px(4.)).cursor_pointer().text_size(px(12.))
            .bg(if active { hsla_u(0xffffff) } else { hsla_u(0xe4e4e4) })
            .text_color(if active { hsla_u(0x1a1a1a) } else { hsla_u(0x666666) });
        if renaming {
            // Inline editor: show the live buffer + a caret bar; typing is routed
            // through sheet_rename_key (keyboard focus stays on the grid root).
            let buf = rename.as_ref().map(|(_, b)| b.clone()).unwrap_or_default();
            tab = tab
                .bg(hsla_u(0xffffff))
                .border_1().border_color(hsla_u(BRAND))
                .child(div().min_w(px(8.)).child(SharedString::from(buf)))
                .child(div().w(px(1.)).h(px(12.)).bg(hsla_u(BRAND)));
        } else {
            let ent_sel = ent.clone();
            tab = tab
                .child(div().child(SharedString::from(s.name.clone())))
                .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                    // Double-click renames; single-click selects.
                    let dbl = ev.click_count >= 2;
                    ent_sel.update(cx, |this, cx| {
                        if dbl {
                            // Focus the grid root so rename keystrokes route to sheet_key.
                            this.focus.focus(window, cx);
                            this.sheet_begin_rename(i, cx);
                        } else {
                            this.select_sheet(i, cx);
                        }
                    });
                });
            // A small × on the active tab (never on the last remaining sheet).
            if active && nsheets > 1 {
                let ent_del = ent.clone();
                tab = tab.child(
                    div()
                        .id(ElementId::Name(format!("sheet-del-{i}").into()))
                        .px(px(2.)).rounded(px(2.)).text_size(px(11.)).text_color(hsla_u(0x999999))
                        .hover(|d| d.text_color(hsla_u(0xc0392b)).bg(hsla_u(0xececec)))
                        .child("\u{00d7}")
                        .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                            cx.stop_propagation();
                            ent_del.update(cx, |this, cx| this.sheet_delete(i, cx));
                        }),
                );
            }
        }
        tabs = tabs.child(tab);
    }
    // ＋ new sheet.
    let ent_add = ent.clone();
    tabs = tabs.child(
        div()
            .id("sheet-add")
            .px_2().h(px(20.)).flex().items_center().rounded_t(px(4.)).cursor_pointer().text_size(px(15.))
            .text_color(hsla_u(0x666666))
            .hover(|d| d.bg(hsla_u(0xe4e4e4)).text_color(hsla_u(0x1a1a1a)))
            .child("+")
            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                ent_add.update(cx, |this, cx| this.sheet_add(cx));
            }),
    );
    // Excel-style: a short horizontal scrollbar shares the tab row, pushed to the
    // right by a flexible spacer.
    tabs = tabs
        .child(div().flex_1().min_w(px(8.)))
        .child(sheet_hbar(view, ent, max_c, col0, cend));

    // Chart cards, positioned from their CELL ANCHOR so they scroll with the grid
    // (Excel behaviour) instead of being pinned to the viewport corner.
    // x: gutter + frozen columns + the anchor column's distance from col0.
    // y: formula bar + column header + frozen rows + (anchor row - scrolled rows).
    // Approximate the pixel scroll offset from the list's logical position
    // (exact when rows are uniform height; a tall row scrolled above the anchor
    // shifts it slightly — acceptable for chart cards).
    let top = view.vlist.logical_scroll_top();
    let scrolled_px = -(top.item_ix as f32 * SHEET_ROW_H + f32::from(top.offset_in_item));
    let col_x = |ac: u32| -> Option<f32> {
        if ac < fc {
            // Anchored inside the frozen region: always visible at its fixed x.
            return Some(SHEET_GUT + (0..ac).map(|c| col_px(sh.col_width(c))).sum::<f32>());
        }
        if ac < col0 {
            return None; // scrolled off to the left
        }
        Some(SHEET_GUT + frozen_w + (col0..ac).map(|c| col_px(sh.col_width(c))).sum::<f32>())
    };
    // y is relative to the CHART LAYER's origin (top of the frozen-row band, i.e.
    // just under the column header) — the layer clips, so a chart scrolled up
    // slides under the header instead of drawing over it.
    let row_y = |ar: u32| -> f32 {
        if (ar as usize) < fr {
            return ar as f32 * SHEET_ROW_H; // pinned frozen row
        }
        fr as f32 * SHEET_ROW_H + (ar - fr as u32) as f32 * SHEET_ROW_H + scrolled_px
    };
    let loaded = sh.drawings.iter().filter_map(|d| match &d.kind {
        gridcore::sheet::DrawingKind::Chart(cd) => Some((d.from, cd)),
        _ => None,
    });
    let cards: Vec<AnyElement> = view
        .charts
        .iter()
        .filter(|c| c.sheet == view.active)
        .map(|cv| (cv.from, &cv.data))
        .chain(loaded)
        .filter_map(|(from, data)| {
            // `from` is (row, col) for UI charts and gridcore drawings alike.
            let (ar, ac) = from;
            let x = col_x(ac)?;
            let y = row_y(ar);
            Some(div().absolute().left(px(x)).top(px(y)).child(chart_card(data)).into_any_element())
        })
        .collect();
    // A yellow note box for the selected commented cell (hidden while its entry
    // bar is open), anchored just off the cell's top-right like Excel.
    let note: Option<AnyElement> = if comment_editing {
        None
    } else {
        let (sr, sc) = view.sel;
        if !comment_cells.contains(&(sr, sc)) {
            None
        } else {
            view.pkg
                .comments()
                .into_iter()
                .find(|c| c.sheet == view.active && c.row == sr && c.col == sc)
                .and_then(|c| {
                    let x = col_x(sc)? + col_px(sh.col_width(sc)) + 6.0;
                    let y = row_y(sr);
                    Some(
                        v_flex()
                            .absolute().left(px(x)).top(px(y))
                            .w(px(200.)).px_2().py_1p5().gap_1()
                            .bg(hsla_u(0xffffe1)).border_1().border_color(hsla_u(0xc9b458)).rounded_sm()
                            .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(hsla_u(0x333333)).child(SharedString::from(c.author.clone())))
                            .child(div().text_size(px(11.)).text_color(hsla_u(0x1a1a1a)).child(SharedString::from(c.text.clone())))
                            .into_any_element(),
                    )
                })
        }
    };
    // Data-validation list dropdown: an arrow on the selected cell + (when open)
    // a value popup, both cell-anchored in the same layer.
    let mut dv_overlay: Vec<AnyElement> = Vec::new();
    if let Some(vals) = &dv_values {
        let (sr, sc) = view.sel;
        if let Some(cx0) = col_x(sc) {
            let cw = col_px(sh.col_width(sc));
            let y = row_y(sr);
            let ent_arrow = ent.clone();
            dv_overlay.push(
                div()
                    .id("dv-arrow")
                    .absolute().left(px(cx0 + cw - 17.0)).top(px(y + 1.0))
                    .w(px(16.)).h(px(SHEET_ROW_H - 2.0))
                    .flex().items_center().justify_center().cursor_pointer()
                    .bg(hsla_u(0xf1f1f1)).border_1().border_color(hsla_u(0x9a9a9a)).rounded_sm()
                    .text_size(px(8.)).text_color(hsla_u(0x333333))
                    .child("\u{25bc}")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_arrow.update(cx, |this, cx| this.sheet_dv_toggle(cx));
                    })
                    .into_any_element(),
            );
            if dv_open {
                let mut list = v_flex()
                    .id("dv-list")
                    .absolute().left(px(cx0)).top(px(y + SHEET_ROW_H))
                    .min_w(px(cw.max(90.0))).max_h(px(220.)).overflow_y_scroll()
                    .bg(hsla_u(0xffffff)).border_1().border_color(hsla_u(0x9a9a9a)).rounded_sm();
                for val in vals {
                    let ent_pick = ent.clone();
                    let v2 = val.clone();
                    list = list.child(
                        div()
                            .id(ElementId::Name(format!("dv-{val}").into()))
                            .px_2().py(px(2.)).cursor_pointer().text_size(px(12.)).text_color(hsla_u(0x1a1a1a))
                            .hover(|d| d.bg(hsla_u(0xe8f0fe)))
                            .child(SharedString::from(val.clone()))
                            .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                                ent_pick.update(cx, |this, cx| this.sheet_dv_pick(v2.clone(), cx));
                            }),
                    );
                }
                dv_overlay.push(list.into_any_element());
            }
        }
    }
    // The clipping layer: spans the rows viewport (under the column header, above
    // the sheet-tab row). Non-interactive, so cell clicks pass through.
    let chart_layer = div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(26. + SHEET_ROW_H))
        .bottom(px(26.))
        .overflow_hidden()
        .children(cards)
        .children(note)
        .children(dv_overlay);

    let ent_move = ent.clone();
    let ent_up = ent.clone();
    v_flex()
        .flex_1()
        .h_full()
        .min_h(px(0.))
        // Allow the grid to shrink below its content width so a side panel (the
        // PivotTable Fields list) can sit beside it instead of overflowing.
        .min_w(px(0.))
        .relative()
        .overflow_hidden()
        .bg(hsla_u(0xffffff))
        // Column-resize drag: track the pointer and release anywhere in the grid.
        .on_mouse_move(move |ev, _w, cx| {
            let x = f32::from(ev.position.x);
            ent_move.update(cx, |this, cx| this.col_resize_move(x, cx));
        })
        .on_mouse_up(MouseButton::Left, move |_ev, _w, cx| {
            ent_up.update(cx, |this, cx| {
                this.col_resize_end(cx);
                this.sheet_dragging = false; // end any drag-select
                this.sheet_fill_end(cx); // commit an auto-fill drag, if any
            });
        })
        .child(bar)
        .child(grid_area)
        // Charts sit above the cells but CLIPPED to the rows viewport, so they
        // scroll under the column header rather than floating over the chrome.
        .child(chart_layer)
        // Excel places the vertical scrollbar down the right edge of the grid; it
        // overlays the rows area (below the formula bar, above the sheet tabs).
        .child(
            div()
                .absolute()
                .top(px(26. + SHEET_ROW_H))
                .right_0()
                .bottom(px(26.))
                .w(px(12.))
                .child(gpui_component::scroll::Scrollbar::vertical(&view.vlist)),
        )
        // (The horizontal scrollbar now lives inside the sheet-tab row, Excel-style.)
        .child(tabs)
        .into_any_element()
}

/// The horizontal scroll strip under the grid: an Excel-style track whose thumb
/// A short Excel-style horizontal scrollbar for the sheet-tab row: end arrows
/// that step one column plus a proportional thumb (columns virtualize by offset,
/// so the thumb reflects col0 within the used-column extent).
fn sheet_hbar(_view: &SheetView, ent: &Entity<Docxy>, max_c: u32, col0: u32, cend: u32) -> AnyElement {
    let total = (max_c + 1).max(cend + 1).max(1);
    let shown = (cend + 1).saturating_sub(col0).max(1);
    let frac = (shown as f32 / total as f32).clamp(0.08, 1.0);
    let pos = if total > shown { col0 as f32 / (total - shown) as f32 } else { 0.0 };
    let arrow = |glyph: &'static str, id: &'static str, ent: Entity<Docxy>, delta: i32| {
        div()
            .id(id)
            .w(px(15.)).h(px(15.)).flex().items_center().justify_center().cursor_pointer()
            .rounded_sm().bg(hsla_u(0xe4e4e4)).text_size(px(8.)).text_color(hsla_u(0x444444))
            .hover(|d| d.bg(hsla_u(0xd0d0d0)))
            .child(glyph)
            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                ent.update(cx, |this, cx| this.sheet_hscroll(delta, cx));
            })
    };
    h_flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .mr(px(4.))
        .child(arrow("\u{25C0}", "hbar-left", ent.clone(), -1))
        .child(
            // Fixed-width track (shorter, like Excel) with a proportional thumb.
            div()
                .relative()
                .w(px(160.)).h(px(9.))
                .rounded(px(3.)).bg(hsla_u(0xe0e0e0)).border_1().border_color(hsla_u(0xcfcfcf))
                .child(
                    div()
                        .absolute().top(px(0.))
                        .h(px(7.))
                        .left(relative((pos * (1.0 - frac)).clamp(0.0, 1.0 - frac)))
                        .w(relative(frac))
                        .rounded(px(3.))
                        .bg(hsla_u(0xa8a8a8)),
                ),
        )
        .child(arrow("\u{25B6}", "hbar-right", ent.clone(), 1))
        .into_any_element()
}

fn placeholder(kind: Kind, bg: Hsla, dim: Hsla) -> impl IntoElement {
    let (name, blurb) = match kind {
        Kind::Xlsx => ("xlsxy", "the spreadsheet grid (gridcore) lands here next"),
        Kind::Look => ("lookxy", "mail list + reading pane (mailcore) lands here next"),
        Kind::Docx => ("docxy", ""),
    };
    v_flex()
        .flex_1()
        .bg(bg)
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_color(rgb(BRAND)).font_weight(FontWeight::BOLD).text_size(px(20.)).child(name))
        .child(div().text_color(dim).child(blurb))
}

fn main() {
    // Files passed on the command line (e.g. double-clicking a document in
    // Explorer) — opened on top of the restored hot-exit session.
    let cli_files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).filter(|p| p.is_file()).collect();
    gpui_platform::application().with_assets(DocxyAssets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        // Tab / Shift-Tab are reserved by gpui's focus system; bind them to
        // actions so the document can insert a tab / outdent instead.
        cx.bind_keys([
            KeyBinding::new("tab", InsertTabAction, None),
            KeyBinding::new("shift-tab", OutdentAction, None),
        ]);
        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitleBar::title_bar_options()),
            window_min_size: Some(size(px(460.), px(420.))),
            kind: WindowKind::Normal,
            ..Default::default()
        };
        let startup_files = cli_files.clone();
        cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| Docxy::new(cx));
            // Open any command-line files on top of the restored session.
            if !startup_files.is_empty() {
                view.update(cx, move |this, cx| this.open_args(startup_files, cx));
            }
            // Hot-exit: capture the latest (possibly unsaved) content when the
            // window is closed, so a restart restores exactly what was open. By
            // default closing is silent; with "ask before closing" on, confirm
            // when there are unsaved tabs.
            let on_close = view.clone();
            window.on_window_should_close(cx, move |_window, cx| {
                on_close.update(cx, |this, _| {
                    this.persist();
                    if this.ask_on_close && this.tabs.iter().any(|t| t.dirty) {
                        matches!(
                            rfd::MessageDialog::new()
                                .set_title("docxy")
                                .set_description("You have unsaved changes.\n\nClose anyway? Your work is kept and reopened next launch.")
                                .set_buttons(rfd::MessageButtons::YesNo)
                                .show(),
                            rfd::MessageDialogResult::Yes
                        )
                    } else {
                        true
                    }
                })
            });
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open docxy window");
    });
}

#[cfg(test)]
mod grid_geom_tests {
    use super::{col_px, last_visible_col, row_height_px, scroll_col0_for_sel};

    // A uniform-width sheet: every column is `w` px.
    fn uniform(w: f32) -> impl Fn(u32) -> f32 {
        move |_c| w
    }

    #[test]
    fn col_px_scales_and_clamps() {
        assert_eq!(col_px(10.0), 76.0); // 10*7+6
        assert_eq!(col_px(0.5), 28.0); // clamped up to the 28 floor
        assert_eq!(col_px(1000.0), 320.0); // clamped to the 320 ceiling
    }

    #[test]
    fn last_visible_col_fills_and_overshoots_by_one() {
        // 100px cols, 350px available: cols 0,1,2 = 300 fit, col 3 overshoots to
        // 400 > 350 and is the last (included, clips at edge).
        assert_eq!(last_visible_col(uniform(100.0), 0, 350.0, 255), 3);
        // Exactly fits three: still stops one past when the 4th overflows.
        assert_eq!(last_visible_col(uniform(100.0), 0, 300.0, 255), 3);
        // The anchor column always renders (window never collapses below col0),
        // even with no room.
        assert!(last_visible_col(uniform(100.0), 5, 0.0, 255) >= 5);
        // Respects the max-column bound.
        assert_eq!(last_visible_col(uniform(10.0), 250, 100.0, 255), 255);
    }

    #[test]
    fn scroll_col0_keeps_selection_visible() {
        let w = uniform(100.0); // 4 cols fit in 400px
        // Selection left of the window snaps col0 to it (but not past frozen).
        assert_eq!(scroll_col0_for_sel(&w, 5, 1, 3, 400.0), 3);
        assert_eq!(scroll_col0_for_sel(&w, 5, 4, 2, 400.0), 4); // clamps to fc
        // Selection already inside the window: col0 unchanged.
        assert_eq!(scroll_col0_for_sel(&w, 2, 0, 4, 400.0), 2); // cols 2..=4 = 300 ≤ 400
        // Selection past the right edge: window shrinks from the left so sc fits.
        // col0=0, sc=6 → need [start..=6] ≤ 400px (4 cols) → start=3.
        assert_eq!(scroll_col0_for_sel(&w, 0, 0, 6, 400.0), 3);
    }

    #[test]
    fn scroll_col0_then_last_visible_makes_selection_visible() {
        // The end-to-end invariant that guards arrow-key navigation: after
        // reconcile, the selected column lies within [col0, cend].
        let w = uniform(90.0);
        for sc in 0..40u32 {
            let col0 = scroll_col0_for_sel(&w, 0, 0, sc, 500.0);
            let cend = last_visible_col(&w, col0, 500.0, 255);
            assert!(col0 <= sc && sc <= cend, "sc={sc} not in [{col0},{cend}]");
        }
    }

    #[test]
    fn row_height_px_maps_points_and_defaults() {
        assert_eq!(row_height_px(None, 21.0), 21.0); // default
        assert_eq!(row_height_px(Some(15.0), 21.0), 21.0); // 15pt == the base
        assert_eq!(row_height_px(Some(30.0), 21.0), 42.0); // double height
    }
}

#[cfg(test)]
mod edit_caret_tests {
    use super::{buf_backspace, buf_delete, buf_insert, char_to_byte};

    #[test]
    fn insert_backspace_delete_at_caret() {
        let mut t = String::new();
        let mut c = 0usize;
        for s in ["h", "e", "llo"] {
            buf_insert(&mut t, &mut c, s);
        }
        assert_eq!(t, "hello");
        assert_eq!(c, 5);

        // Insert in the MIDDLE (the whole point — not just append).
        c = 3; // "hel|lo"
        buf_insert(&mut t, &mut c, "X"); // "helX|lo"
        assert_eq!(t, "helXlo");
        assert_eq!(c, 4);

        // Backspace removes the char BEFORE the caret.
        buf_backspace(&mut t, &mut c); // "hel|lo"
        assert_eq!(t, "hello");
        assert_eq!(c, 3);

        // Delete removes the char AT the caret.
        buf_delete(&mut t, c); // "hel|o"
        assert_eq!(t, "helo");

        // Backspace at position 0 is a no-op.
        let mut c0 = 0usize;
        buf_backspace(&mut t, &mut c0);
        assert_eq!(t, "helo");
        assert_eq!(c0, 0);
    }

    #[test]
    fn char_to_byte_is_utf8_aware() {
        // "café" — é is 2 bytes; char index 4 maps past the 'é'.
        let s = "café";
        assert_eq!(char_to_byte(s, 0), 0);
        assert_eq!(char_to_byte(s, 3), 3); // start of é
        assert_eq!(char_to_byte(s, 4), 5); // end of string (é took 2 bytes)
        assert_eq!(char_to_byte(s, 99), s.len()); // clamps
        // Inserting after a multibyte char lands on a char boundary (no panic).
        let mut t = s.to_string();
        let mut c = 4usize;
        buf_insert(&mut t, &mut c, "!");
        assert_eq!(t, "café!");
    }
}
