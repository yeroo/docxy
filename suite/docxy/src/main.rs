//! docxy — the doc-centric desktop suite (docs / sheets / mail in tabs), on GPUI.
//!
//! A thin GPUI view over `docxcore::editor::Editor` — the lossless engine the
//! terminal docxy uses. Custom title bar hosting the document tabs + window
//! controls, an Office-style ribbon (File backstage + Home/Styles/Insert/Review/
//! View tabs of titled command groups), and a rich editable document surface.
//! Theming (Auto/Light/Dark) follows gpui-component's theme; session hot-exit
//! persists open tabs/files + the theme choice to `<config>/docxy/session.json`.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod harness;

use std::path::PathBuf;

use docxcore::comments::Comment;
use docxcore::editor::{Caret, Clip, Editor};
use docxcore::model::{
    Align, Block, BorderKind, Document, Inline, ParBorders, Paragraph, RunProps, Table, VertAlign,
};
use docxcore::package::Package;
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
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .text_color(color)
        .flex_none()
}

/// A small Quick-Access-Toolbar icon button (Undo/Redo in the title bar).
fn qat_btn(
    id: &'static str,
    icon: &'static str,
    tip: &'static str,
    pal: Pal,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
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

/// Environment variable that redirects every file the app persists — the
/// session, the hot sidecars, and (later) the harness discovery file — to a
/// directory of the caller's choosing. Set by the UI test harness so a test
/// instance cannot touch the installed app's state.
const CONFIG_DIR_ENV: &str = "DOCXY_CONFIG_DIR";

/// Root under which everything this app persists lives. Normally the OS config
/// directory; a `DOCXY_CONFIG_DIR` override redirects all of it at once.
///
/// ⚠️ The override has to be its own variable rather than `APPDATA`. Measured on
/// Windows 11 with `APPDATA` pointed elsewhere: `ctlcore::config_ctl_dir` reads
/// `APPDATA` and follows the override, while `dirs::config_dir()` asks the
/// known-folder API and keeps returning the real `…\AppData\Roaming`. So an
/// `APPDATA` override moves the control socket but leaves `session.json` and
/// the hot sidecars in the user's own profile — isolation that looks complete
/// and is not. One explicit variable that BOTH path mechanisms go through is
/// the only thing that actually isolates.
fn config_root() -> PathBuf {
    config_root_from(std::env::var_os(CONFIG_DIR_ENV), dirs::config_dir())
}

/// The decision behind [`config_root`], separated from the environment so it
/// can be tested. An override that is set but empty counts as unset — an
/// exported-but-blank variable is a shell accident, not a request to write into
/// the current directory. A relative override is honoured verbatim (resolved
/// against the working directory), because that is what typing one means.
fn config_root_from(over: Option<std::ffi::OsString>, os_config: Option<PathBuf>) -> PathBuf {
    match over {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => os_config.unwrap_or_else(|| PathBuf::from(".")),
    }
}

fn session_path() -> PathBuf {
    config_root().join("docxy").join("session.json")
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
    /// A column a range field asked to be brought into view, scrolled to on the
    /// next render. Only the render pass knows how wide the grid is, so
    /// `reveal_range` can't work out a rightwards scroll itself.
    reveal_col: Option<u32>,
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
    /// UI-authored charts live outside the workbook, so undoing a chart move,
    /// delete or re-point needs them snapshotted alongside it.
    charts: Vec<ChartView>,
    /// So do the pivot definitions — and they carry sheet indices that a sheet
    /// delete rewrites, so an undo that restored only the workbook would leave
    /// a pivot writing its table over whatever sheet took that index.
    pivots: Vec<PivotDef>,
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

/// An in-progress chart drag: which chart on the active sheet, where the press
/// landed (window coords) and how far the pointer has travelled since. `edge`
/// is which side the grip owns — `(0, 0)` is the card itself, i.e. a move;
/// anything else is a resize from that corner or edge.
#[derive(Clone, Copy)]
struct ChartDrag {
    idx: usize,
    edge: (i8, i8),
    origin: (f32, f32),
    delta: (f32, f32),
}

/// Chart state the overlay renderer needs, which lives on `Docxy` rather than
/// on the sheet view.
#[derive(Clone, Copy, Default)]
struct ChartUi {
    sel: Option<usize>,
    /// (index, dx, dy, edge) of the card the pointer is currently dragging.
    drag: Option<(usize, f32, f32, (i8, i8))>,
}

/// Which field has the keyboard. A range target puts the grid in point mode and
/// outlines the cells it names; a plain-text one behaves like any text box.
/// Variants arrive with their consumers (series refs, the other range bars).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RefTarget {
    ChartRange,
    ChartTitle,
    /// The cell naming series `i` — a ref, but plain text is accepted too.
    SeriesName(usize),
    /// The cells series `i` plots.
    SeriesValues(usize),
    /// The cells holding the category labels.
    Categories,
    /// The cells a conditional-formatting rule applies to.
    CondFormat,
    /// The cells a data-validation list applies to.
    Validation,
    /// The rows a sort runs over.
    Sort,
    /// The cells Text-to-Columns splits.
    TextToColumns,
}

impl RefTarget {
    /// Does this field hold a cell range? A range target puts the grid in point
    /// mode while it has the keyboard.
    fn is_range(self) -> bool {
        !matches!(self, RefTarget::ChartTitle)
    }

    /// Does this field belong to one of the sheet entry bars? Those bars own
    /// the keyboard while they are open, so their range field has to be asked
    /// first — otherwise what you type lands in the bar's own buffer.
    fn is_bar(self) -> bool {
        matches!(
            self,
            RefTarget::CondFormat
                | RefTarget::Validation
                | RefTarget::Sort
                | RefTarget::TextToColumns
        )
    }
}

/// The range field a sheet action opens, if that action has one.
fn bar_target(act: SheetAct) -> Option<RefTarget> {
    match act {
        SheetAct::CondFormat => Some(RefTarget::CondFormat),
        SheetAct::DataValidation => Some(RefTarget::Validation),
        SheetAct::CustomSort => Some(RefTarget::Sort),
        SheetAct::TextToColumns => Some(RefTarget::TextToColumns),
        _ => None,
    }
}

/// Whether a ribbon command reads or writes the cell selection.
///
/// The same rule `chart_hand_back` states for keys, asked of the ribbon: a
/// command that acts on the selected cells has to be given a selection that is
/// on screen. Ribbon ▸ Copy and Ctrl+C are the same `sheet_copy`, so they take
/// the selection the same way, and Delete Row is the case that matters — with a
/// chart selected `sel_hidden` leaves no ring, no wash and no header mark, so
/// the rows would go from cells nothing on screen named.
///
/// Almost everything here targets cells, including the ones that only look
/// sheet-wide: Freeze Panes freezes AT the selected cell, the comment steps
/// move the selection, and the colour pickers paint it. The exceptions are the
/// three that never touch it — protection and outlining are properties of the
/// whole sheet, and `Todo` does nothing at all.
fn act_targets_cells(act: SheetAct) -> bool {
    !matches!(
        act,
        SheetAct::ProtectSheet | SheetAct::Outline | SheetAct::Todo
    )
}

/// A live text field: which target it edits, its buffer, the caret and the
/// selection anchor (equal to the caret when nothing is selected).
#[derive(Clone)]
struct RangeEdit {
    target: RefTarget,
    buf: String,
    caret: usize,
    anchor: usize,
    /// A drag is in progress, so pointer moves extend the selection.
    dragging: bool,
}

impl RangeEdit {
    /// The selected char range, ordered; `None` when the caret is collapsed.
    fn selection(&self) -> Option<(usize, usize)> {
        let n = self.buf.chars().count();
        let (a, c) = (self.anchor.min(n), self.caret.min(n));
        (a != c).then(|| (a.min(c), a.max(c)))
    }
    /// Drop the selected text, leaving the caret where it was.
    fn delete_selection(&mut self) -> bool {
        let Some((s0, s1)) = self.selection() else {
            return false;
        };
        let (b0, b1) = (char_to_byte(&self.buf, s0), char_to_byte(&self.buf, s1));
        self.buf.replace_range(b0..b1, "");
        self.caret = s0;
        self.anchor = s0;
        true
    }
    fn set_caret(&mut self, idx: usize, extend: bool) {
        self.caret = idx.min(self.buf.chars().count());
        if !extend {
            self.anchor = self.caret;
        }
    }
}

/// Where the idx-th chart of the active sheet lives: authored this session, or
/// loaded from the file as a drawing.
#[derive(Clone, Copy)]
enum ChartRef {
    Ui(usize),
    Drawing(usize),
}

/// The colours offered per series in the Chart panel (the renderer's own
/// palette, so the swatches match what an unstyled chart already draws).
const CHART_COLORS: [u32; 8] = [
    0x2AA79B, 0x2F6FDB, 0xC0705A, 0xD8A44A, 0x7A5EA8, 0x5A9E5A, 0xD06C9E, 0x707880,
];

/// Width of a right-hand side panel (PivotTable Fields, Chart). The grid
/// subtracts each visible one when it works out how many columns fit.
const SIDE_PANEL_W: f32 = 232.0;

/// A chart card never shrinks below this, however far its grip is dragged.
const MIN_CHART_W: f32 = 150.0;
const MIN_CHART_H: f32 = 110.0;

/// A chart card's pixel size: the extent of the cells its anchor spans, which
/// is how Excel sizes a `twoCellAnchor` object. Bounded so a wild `to` (or a
/// run of hidden cells) can't blow up the walk.
fn chart_span_px(sh: &gridcore::sheet::Sheet, from: (u32, u32), to: (u32, u32)) -> (f32, f32) {
    let c_end = to.1.max(from.1 + 1).min(from.1 + 256);
    let r_end = to.0.max(from.0 + 1).min(from.0 + 1024);
    let w: f32 = (from.1..c_end).map(|c| col_px(sh.col_width(c))).sum();
    let h: f32 = (from.0..r_end)
        .map(|r| row_height_px(sh.row_height(r), SHEET_ROW_H) + 1.0)
        .sum();
    (w.max(MIN_CHART_W), h.max(MIN_CHART_H))
}

/// How one axis of a resize drag splits into (edge offset, size change) for a
/// grip on `edge` (-1 near side, +1 far side, 0 not on this axis) dragged `d`
/// pixels. Shared by the live preview and the commit so they can't disagree.
fn resize_axis(edge: i8, d: f32, size: f32, min: f32) -> (f32, f32) {
    match edge {
        -1 => {
            let n = (size - d).max(min);
            (size - n, n - size)
        }
        1 => {
            let n = (size + d).max(min);
            (0.0, n - size)
        }
        _ => (0.0, 0.0),
    }
}

/// The column an anchor lands on after being dragged `dx` px sideways: walk
/// cell by cell from `ac`, stopping at whichever boundary the drag ended
/// nearest. Bounded, so a run of zero-width (hidden) columns can't spin.
fn shift_col(sh: &gridcore::sheet::Sheet, ac: u32, dx: f32) -> u32 {
    let mut c = ac as i64;
    let mut acc = 0.0f32;
    for _ in 0..512 {
        if dx >= 0.0 {
            let w = col_px(sh.col_width(c as u32));
            if acc + w > dx {
                if dx - acc > w / 2.0 {
                    c += 1;
                }
                break;
            }
            acc += w;
            c += 1;
        } else {
            if c == 0 {
                break;
            }
            let w = col_px(sh.col_width((c - 1) as u32));
            if acc - w < dx {
                if acc - dx > w / 2.0 {
                    c -= 1;
                }
                break;
            }
            acc -= w;
            c -= 1;
        }
    }
    c.max(0) as u32
}

/// `shift_col`'s counterpart down the rows (each row is its height plus the
/// 1px gridline under it).
fn shift_row(sh: &gridcore::sheet::Sheet, ar: u32, dy: f32) -> u32 {
    let h = |r: u32| row_height_px(sh.row_height(r), SHEET_ROW_H) + 1.0;
    let mut rr = ar as i64;
    let mut acc = 0.0f32;
    for _ in 0..2048 {
        if dy >= 0.0 {
            let rh = h(rr as u32);
            if acc + rh > dy {
                if dy - acc > rh / 2.0 {
                    rr += 1;
                }
                break;
            }
            acc += rh;
            rr += 1;
        } else {
            if rr == 0 {
                break;
            }
            let rh = h((rr - 1) as u32);
            if acc - rh < dy {
                if acc - dy > rh / 2.0 {
                    rr -= 1;
                }
                break;
            }
            acc -= rh;
            rr -= 1;
        }
    }
    rr.max(0) as u32
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
    if rest.is_empty() {
        None
    } else {
        Some((op, rest.to_string(), None))
    }
}

/// Excel's "Light Red Fill with Dark Red Text" conditional-format preset.
fn cf_preset_dxf() -> gridcore::sheet::Dxf {
    gridcore::sheet::Dxf {
        fill: Some((0xFF, 0xC7, 0xCE)),
        color: Some((0x9C, 0x00, 0x06)),
        bold: None,
        italic: None,
    }
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
        &self.pkg.workbook.sheets[self
            .active
            .min(self.pkg.workbook.sheets.len().saturating_sub(1))]
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
                CellValue::Bool(b) => {
                    if *b {
                        "TRUE".into()
                    } else {
                        "FALSE".into()
                    }
                }
                CellValue::Error(e) => e.clone(),
                CellValue::Empty => String::new(),
            },
            None => String::new(),
        }
    }

    // ---- in-cell edit caret (char-indexed into `editing`) ----
    /// Number of chars in the edit buffer.
    fn edit_len(&self) -> usize {
        self.editing
            .as_deref()
            .map(|s| s.chars().count())
            .unwrap_or(0)
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
    /// Capture this view's undoable state — the workbook plus everything the UI
    /// keeps beside it that a mutation can move.
    fn snapshot(&self) -> SheetSnapshot {
        SheetSnapshot {
            wb: self.pkg.workbook.clone(),
            charts: self.charts.clone(),
            pivots: self.pivot_views.clone(),
            active: self.active,
            sel: self.sel,
            anchor: self.anchor,
        }
    }

    /// Restore this view from an undo/redo snapshot, rebuilding the recalc engine.
    fn restore(&mut self, snap: SheetSnapshot) {
        self.pkg.workbook = snap.wb;
        self.charts = snap.charts;
        self.pivot_views = snap.pivots;
        self.engine = gridcore::engine::Engine::new(&self.pkg.workbook);
        self.active = snap
            .active
            .min(self.pkg.workbook.sheets.len().saturating_sub(1));
        self.sel = snap.sel;
        self.anchor = snap.anchor;
        self.editing = None;
    }
    /// The selection rectangle as (r0, c0, r1, c1), top-left to bottom-right.
    fn range(&self) -> (u32, u32, u32, u32) {
        sel_range(self.sel, self.anchor)
    }
    /// Whether more than one cell is selected.
    fn has_range(&self) -> bool {
        self.sel != self.anchor
    }
    /// The index of `row` within the virtualized list (non-hidden scrollable
    /// rows, past the frozen ones) — for `ListState::scroll_to_reveal_item`.
    fn row_list_index(&self, row: u32) -> usize {
        let sh = self.sheet();
        row_index_of(|r| sh.row_hidden(r), (sh.freeze.0 as usize).min(30), row)
    }
    /// The sheet row a list index refers to — the inverse of `row_list_index`,
    /// which the list needs because hidden rows collapse out of it.
    fn row_at_list_index(&self, ix: usize) -> Option<u32> {
        let sh = self.sheet();
        row_at_index(|r| sh.row_hidden(r), (sh.freeze.0 as usize).min(30), ix)
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
    // The selected chart on the active sheet, as an index into that sheet's
    // chart list (UI-authored charts first, then the ones loaded from the file).
    chart_sel: Option<usize>,
    // The chart the Chart panel SHOWS, which outlives `chart_sel`: deselecting
    // a chart drops its handles and its source outlines but leaves the panel
    // open on it, so a range edit survives the click on the grid that was
    // pointing it. Moved only by `chart_panel_after`; see `PanelEvent`.
    panel_chart: Option<usize>,
    // An in-progress chart move: which chart, and how far the pointer has
    // travelled since the press. The anchor only moves on release.
    chart_drag: Option<ChartDrag>,
    // The Chart panel field being typed into: which one, its buffer, and the
    // caret's char offset in it.
    range_edit: Option<RangeEdit>,
    // What the last commit said about a field, shown under it: which field, did
    // it work, and the text.
    ref_msg: Option<(RefTarget, bool, String)>,
    // The cell a left press landed on, so a drag-select starts there.
    drag_anchor: Option<(u32, u32)>,
    // A reference being pointed at while a formula is being typed: the buffer
    // and caret as they were when the press landed, plus the anchor cell. Each
    // move re-splices from those, so a drag rewrites one reference rather than
    // appending one per cell crossed.
    formula_pick: Option<(String, usize, (u32, u32))>,
    // A range being picked off the grid while a range field has the keyboard
    // (Excel's point mode): the anchor cell, and whether the pointer has moved
    // since the press — a press that never moves is a plain click, not a pick.
    range_pick: Option<((u32, u32), bool)>,
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
    // Which entry bar's range field is on screen, if any. Its seed depends on
    // the bar (a sort's is the region it would find, not the selection).
    bar_field: Option<RefTarget>,
    // A range the user PINNED into that field, by typing it or pointing at it.
    // While this is None the bar keeps following the selection, which is what
    // these bars did before they grew a range field.
    bar_range: Option<String>,
    // The UI test harness's control server and its request pump, parked here so
    // they live as long as the window. `None` on every normal launch — the
    // harness is opt-in per process (`--harness`) and starts nothing otherwise.
    harness: Option<harness::Harness>,
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
    (
        "Accounting",
        "_($* #,##0.00_);_($* (#,##0.00);_($* \"-\"??_)",
    ),
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
const TABLE_PRESETS: &[(&str, usize, usize)] = &[
    ("2×2", 2, 2),
    ("3×2", 3, 2),
    ("3×3", 3, 3),
    ("4×3", 4, 3),
    ("5×3", 5, 3),
    ("5×5", 5, 5),
];

/// The characters offered by the Insert ▸ Symbol picker — Word's common set:
/// typographic punctuation, currency, arrows, and maths.
const SYMBOLS: &[&str] = &[
    "\u{2014}", "\u{2013}", "\u{2011}", "\u{2026}", "\u{2022}", "\u{00B7}", "\u{00A9}", "\u{00AE}",
    "\u{2122}", "\u{00B0}", "\u{00B1}", "\u{00D7}", "\u{00F7}", "\u{2260}", "\u{2248}", "\u{2264}",
    "\u{2265}", "\u{221E}", "\u{00A7}", "\u{00B6}", "\u{20AC}", "\u{00A3}", "\u{00A5}", "\u{00A2}",
    // Typographic quotes: guillemets, low/high quotes, angle quotes.
    "\u{00AB}", "\u{00BB}", "\u{201E}", "\u{201C}", "\u{201D}", "\u{201A}", "\u{2018}", "\u{2019}",
    "\u{2039}", "\u{203A}", "\u{2190}", "\u{2192}", "\u{2191}", "\u{2193}", "\u{03B1}", "\u{03B2}",
    "\u{03C0}", "\u{03BC}", "\u{03A9}", "\u{2211}", "\u{221A}", "\u{2212}", "\u{2605}",
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

impl Pal {
    /// Read the palette off the active theme. The render pass builds one and
    /// passes it down; a field deep in the tree can ask for its own instead of
    /// having it threaded through every signature.
    fn of(cx: &App) -> Pal {
        let t = cx.theme();
        let fg = t.foreground;
        Pal {
            fg,
            dim: t.muted_foreground,
            border: t.border,
            panel: t.secondary,
            // A theme-adaptive hover tint: a low-alpha wash of the foreground,
            // clearly visible on both light and dark grounds.
            hover: Hsla { a: 0.12, ..fg },
            sel: t.selection,
        }
    }
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
        Loaded {
            doc: empty_doc(),
            comments: vec![],
            notes: vec![],
            pkg: None,
            markdown: false,
            status: status.into(),
        }
    }
    fn into_tab(
        self,
        kind: Kind,
        title: SharedString,
        path: Option<PathBuf>,
        dirty: bool,
    ) -> DocTab {
        DocTab {
            kind,
            title,
            path,
            surface: Surface::Doc(Editor::new(self.doc)),
            dirty,
            status: self.status,
            comments: self.comments,
            pkg: self.pkg,
            notes: self.notes,
            markdown: self.markdown,
            hf_edit: None,
        }
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
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
    {
        let (surface, status) = sheet_from_path(path);
        DocTab {
            kind: Kind::Xlsx,
            title,
            path: Some(path.clone()),
            surface,
            dirty: false,
            status,
            comments: vec![],
            pkg: None,
            notes: vec![],
            markdown: false,
            hf_edit: None,
        }
    } else {
        doc_from_path(path).into_tab(Kind::Docx, title, Some(path.clone()), false)
    }
}

/// Char index → byte offset in `s` (clamped to the string length).
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
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

/// The runs an in-progress edit buffer is drawn in: the text split at the caret
/// AND at the boundaries of every reference the formula mentions, each run
/// carrying the index of the reference it belongs to (`None` for ordinary
/// text). Splitting at the caret too keeps the caret bar exactly between two
/// runs, and every run reports the char offset a click needs to place the
/// caret. The reference index is the one `formula_refs` hands the grid, so the
/// text and the outlines are coloured from the same numbering.
///
/// A buffer that isn't a formula gets no colouring — `A1` typed as text is text.
fn edit_runs(buf: &str, caret_chars: usize) -> Vec<(usize, String, Option<usize>)> {
    let caret = char_to_byte(buf, caret_chars);
    let toks = if buf.starts_with('=') {
        formula_ref_tokens(buf)
    } else {
        Vec::new()
    };
    let mut cuts = vec![0usize, caret, buf.len()];
    cuts.extend(toks.iter().flat_map(|(s, _)| [s.start, s.end]));
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let color = toks.iter().position(|(s, _)| s.start <= a && b <= s.end);
            (buf[..a].chars().count(), buf[a..b].to_string(), color)
        })
        .collect()
}

/// Render an in-progress edit buffer with a blinking-style caret bar at `caret`
/// (a char index), each reference the formula mentions in its own colour.
/// Shared by the in-cell editor and the formula bar.
fn edit_caret_row(text: &str, caret: usize, color: Hsla, caret_color: Hsla) -> AnyElement {
    let cc = caret.min(text.chars().count());
    let bar = || div().w(px(1.5)).h(px(13.)).bg(caret_color).flex_none();
    let mut row = h_flex().items_center();
    let mut placed = false;
    for (off, s, ci) in edit_runs(text, cc) {
        if off == cc && !placed {
            row = row.child(bar());
            placed = true;
        }
        let c = ci.map(|i| hsla_u(ref_color(i))).unwrap_or(color);
        row = row.child(
            div()
                .text_size(px(12.))
                .text_color(c)
                .child(SharedString::from(s)),
        );
    }
    if !placed {
        row = row.child(bar());
    }
    row.into_any_element()
}

/// One click-to-caret text segment for the formula bar: a StyledText whose
/// TextLayout maps the click position to a char index (`base_off` is the char
/// offset of this segment within the whole buffer). One per run of
/// `edit_runs`, so clicking any of them places the caret under the pointer.
fn fx_segment(s: String, base_off: usize, color: Option<u32>, ent: Entity<Docxy>) -> AnyElement {
    let styled = StyledText::new(SharedString::from(s.clone()));
    let layout = styled.layout().clone();
    div()
        .child(styled)
        .text_size(px(12.))
        .text_color(hsla_u(color.unwrap_or(0x1a1a1a)))
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
            let byte = layout
                .index_for_position(ev.position)
                .unwrap_or_else(|e| e)
                .min(s.len());
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

/// The formula bar's editing content: one click-to-caret segment per run of
/// `edit_runs` — so each reference is drawn in its grid colour — with the caret
/// bar sitting between the runs it splits.
fn fx_edit_row(text: &str, caret: usize, ent: &Entity<Docxy>) -> AnyElement {
    let cc = caret.min(text.chars().count());
    let bar = || div().w(px(1.5)).h(px(13.)).bg(hsla_u(BRAND)).flex_none();
    let mut row = h_flex().flex_1().h_full().items_center();
    let mut placed = false;
    for (off, s, ci) in edit_runs(text, cc) {
        if off == cc && !placed {
            row = row.child(bar());
            placed = true;
        }
        row = row.child(fx_segment(s, off, ci.map(ref_color), ent.clone()));
    }
    if !placed {
        row = row.child(bar());
    }
    row.into_any_element()
}

/// The sheet row at list index `ix`, skipping hidden rows and the `frozen`
/// ones that render outside the list. `None` past the last row of the sheet —
/// which is what bounds the scan: `0..u32::MAX` would take a minute to answer
/// `None` for an index nothing can satisfy.
fn row_at_index(hidden: impl Fn(u32) -> bool, frozen: usize, ix: usize) -> Option<u32> {
    (0..gridcore::sheet::MAX_ROWS)
        .filter(|&r| !hidden(r))
        .nth(frozen + ix)
}

/// The list index of `row` — the inverse of `row_at_index`, which the list
/// needs because hidden rows collapse out of it.
fn row_index_of(hidden: impl Fn(u32) -> bool, frozen: usize, row: u32) -> usize {
    (0..row)
        .filter(|&r| !hidden(r))
        .count()
        .saturating_sub(frozen)
}

/// The column at `x` pixels from the grid's left edge: the gutter first, then
/// the frozen columns, then the scrolled window from `col0`. `None` when `x`
/// lands in the gutter, which is a row header rather than a cell.
fn col_at_x(
    col_w_px: impl Fn(u32) -> f32,
    x: f32,
    fc: u32,
    col0: u32,
    max_col: u32,
) -> Option<u32> {
    if x < SHEET_GUT {
        return None;
    }
    let mut at = SHEET_GUT;
    for c in 0..fc {
        let w = col_w_px(c);
        if x < at + w {
            return Some(c);
        }
        at += w;
    }
    for c in col0..=max_col {
        let w = col_w_px(c);
        if x < at + w {
            return Some(c);
        }
        at += w;
    }
    None
}

/// What committing a series-NAME field should do with the text in it.
#[derive(Debug, PartialEq, Eq)]
enum NameCommit {
    /// The text is still the name the field was seeded with — leave the series
    /// exactly as it is.
    Unchanged,
    /// A reference: read the name out of those cells — on the sheet it names,
    /// if it named one — and keep the link.
    Ref(RefText),
    /// Anything else is the name itself.
    Literal,
}

/// "Does this text name cells?" can only be asked of text the user actually
/// changed, so the question is put against `shown` — whatever the field was
/// displaying (`series_name_shown`: the reference when the name came from one,
/// the literal name when it didn't). Otherwise focusing the field of a series
/// called `Q1` (or `H1`, or `FY1` — quarter and half-year headers are the common
/// case) and pressing Enter would read it as cell Q1: the name becomes that
/// cell's contents, usually empty, and the series ends up bound to an unrelated
/// cell.
fn series_name_commit(text: &str, shown: &str) -> NameCommit {
    if text.trim() == shown.trim() {
        return NameCommit::Unchanged;
    }
    match parse_ref_text(text) {
        Some(r) => NameCommit::Ref(r),
        None => NameCommit::Literal,
    }
}

/// The name a typed literal gives a series, without the `=` the field seeds
/// itself with.
///
/// A ref-backed series' NAME field shows `=Budget!$B$1:$B$1`, so editing that
/// into a label in place — rather than retyping over the whole selection —
/// leaves the `=` in front of it. Kept, it would be the series' name:
/// `chart_space_xml` writes it straight into `<c:tx><c:v>`, and the file would
/// carry a name Excel reads as a broken formula. Every other path off this
/// field already drops that `=` (`parse_ref_text`, `not_a_range_msg`); this one
/// is the last that didn't.
fn literal_series_name(text: &str) -> String {
    let t = text.trim();
    t.strip_prefix('=').unwrap_or(t).trim().to_string()
}

/// A reference as a field holds it: the sheet it names, if it named one, and
/// the cell box. `None` means the sheet in front of you — a bare `A1:D5`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefText {
    sheet: Option<String>,
    range: (u32, u32, u32, u32),
}

/// What a range field's text refers to, or `None` if it isn't a range.
///
/// Input is deliberately more permissive than what `ref_a1` writes back, as
/// Excel's is: the leading `=` is optional, `$` anchors are optional and
/// ignored, the `Sheet!` qualifier is optional, quoting is optional when the
/// name doesn't need it, and either corner may come first (`D5:A1` names the
/// same box as `A1:D5`).
///
/// A qualifier is KEPT, not dropped: whoever asked for `Budget!A1:D5` gets
/// Budget's cells or a message saying there's no such sheet — never this
/// sheet's cells of the same name. The split is on the LAST `!` because a
/// quoted sheet name may contain one and the cells never can.
fn parse_ref_text(text: &str) -> Option<RefText> {
    let t = text.trim();
    let t = t.strip_prefix('=').unwrap_or(t).trim();
    let (name, cells) = match t.rsplit_once('!') {
        // A `!` with nothing in front of it names no sheet, and Excel refuses
        // `!A1:D5` and `''!A1:D5` rather than reading them as this one. Taking
        // them would act on the sheet in front of you for a reference that
        // pointedly didn't name it.
        Some((p, _)) if unquote_sheet_name(p).is_none() => return None,
        Some((p, r)) => (unquote_sheet_name(p), r),
        None => (None, t),
    };
    Some(RefText {
        sheet: name,
        range: gridcore::sheet::parse_range_name(cells)?,
    })
}

/// The sheet name a qualifier carries, undoing `quote_sheet_name`: a
/// `'...'`-wrapped name loses its quotes and its doubled `''` become one `'`.
/// An empty qualifier names no sheet at all — which `parse_ref_text` reads as a
/// typo and refuses, rather than as "the one in front of you".
fn unquote_sheet_name(prefix: &str) -> Option<String> {
    let p = prefix.trim();
    let name = match p.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Some(inner) => inner.replace("''", "'"),
        None => p.to_string(),
    };
    (!name.is_empty()).then_some(name)
}

/// Which sheet a reference names, as an index into `names`, or what to tell the
/// user. `None` — a bare `A1:D5` — is the sheet in front of you, `active`.
///
/// Matching is case-insensitive for ASCII names, because Excel's is:
/// `budget!A1` finds the `Budget` sheet. The fold is `eq_ignore_ascii_case`, so
/// a name outside ASCII matches only at its own case — `бюджет!A1` does NOT
/// find `Бюджет`. The same fold decides `preview_range` (which delegates here),
/// the rename uniqueness check, and `bar_range_text` — the wash that spells a
/// reference back out — so folding HERE alone would let resolution and the wash
/// disagree about the same reference. The limit is stated rather than fixed in
/// one place for that reason. (It is not universal: `sheet_follow_hyperlink`
/// and `dv_list_values` still match a sheet name byte for byte, a separate and
/// older inconsistency this reference syntax didn't reach.)
///
/// A name matching nothing is REFUSED rather than falling back to `active`:
/// silently redirecting a qualifier someone typed is the bug this reference
/// syntax exists to remove.
///
/// Excel forbids two sheets whose names differ only in case, but a hand-built
/// file can carry them; the first one wins, as it does in each of the lookups
/// above.
fn sheet_index_of(names: &[String], sheet: Option<&str>, active: usize) -> Result<usize, String> {
    let Some(want) = sheet else {
        return Ok(active);
    };
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(want))
        .ok_or_else(|| format!("there's no sheet called \"{want}\""))
}

/// The complaint a range field gives text that isn't a range, held up against
/// the shape that field wants. Every field words it the same way, so no failure
/// reads as a different KIND of failure depending on which field met it — and
/// every one quotes what was typed without the `=` the field seeds itself with,
/// since that `=` is not part of what the user got wrong.
fn not_a_range_msg(text: &str, example: &str) -> String {
    let t = text.trim();
    format!(
        "\"{}\" isn't a range like {example}",
        t.strip_prefix('=').unwrap_or(t).trim()
    )
}

/// A resolved reference: which sheet of the workbook, and which of its cells.
type RefCells = (usize, (u32, u32, u32, u32));

/// The most cells a chart reads from one field. It plots a point per cell AND
/// renders an element per point, so an unbounded range — `A1:A1048576` parses
/// perfectly well — would allocate a million strings and ask the renderer for a
/// million divs, every frame, rather than draw anything anyone wanted.
const MAX_CHART_CELLS: u64 = 4096;

/// Which sheet a chart field reads and which of its cells, or what to tell the
/// user. `example` is the shape that field wants, for the "isn't a range"
/// message; `names` are the workbook's sheets and `active` the one on screen,
/// which is what an unqualified reference means.
///
/// The cell cap is weighed BEFORE the sheet is looked up, because a range too
/// big to plot is too big on every sheet — reporting the missing sheet first
/// would only send the user back to fix the same field twice.
fn chart_ref_of(
    text: &str,
    example: &str,
    names: &[String],
    active: usize,
) -> Result<RefCells, String> {
    let Some(r) = parse_ref_text(text) else {
        return Err(not_a_range_msg(text, example));
    };
    let (r1, c1, r2, c2) = r.range;
    let cells = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
    if cells > MAX_CHART_CELLS {
        return Err(format!(
            "that range is {cells} cells; a chart plots at most {MAX_CHART_CELLS}"
        ));
    }
    Ok((sheet_index_of(names, r.sheet.as_deref(), active)?, r.range))
}

/// Drop series `i`, unless it is the last one — a chart with no series has
/// nothing to draw, and Excel won't let you get there either. Reports whether
/// it removed one.
fn series_remove(list: &mut Vec<gridcore::sheet::ChartSeries>, i: usize) -> bool {
    if list.len() <= 1 || i >= list.len() {
        return false;
    }
    list.remove(i);
    true
}

/// The sheet a resolved reference reads, and the `ChartSource` a slot pointed
/// there writes back — stamped with THAT sheet's name rather than the name of
/// whichever sheet happened to be on screen, which is what makes a cross-sheet
/// reference survive a save: `chart_space_xml` spells the `<c:f>` from this.
///
/// `None` when `si` names no sheet, so a stale index reports rather than
/// panics. Every chart slot resolves its index through `sheet_index_of` first,
/// so that is a backstop, not an expected answer.
fn ref_source(
    sheets: &[gridcore::sheet::Sheet],
    si: usize,
    range: (u32, u32, u32, u32),
) -> Option<(&gridcore::sheet::Sheet, gridcore::sheet::ChartSource)> {
    let sh = sheets.get(si)?;
    Some((
        sh,
        gridcore::sheet::ChartSource {
            sheet: sh.name.clone(),
            range,
            cat_col: range.1,
        },
    ))
}

/// Rebuild a chart's box from the references its slots hold right now.
///
/// `ChartSource::union` merges rectangles and keeps the receiver's sheet name,
/// so unioning across sheets would leave the box naming one sheet and covering
/// the other's cells — and `chart_space_xml` derives a ref-less series' and the
/// categories' `<c:f>` from that box. Replacing it is no better: the box would
/// then describe the one slot just re-pointed rather than the chart, and the
/// DATA RANGE field showing it would offer to replot the whole chart from a
/// single foreign column. So a reference naming another sheet than the box
/// leaves the box alone, exactly as the loader resolves the same clash
/// (`parse_chart` in `gridcore/src/drawing.rs`).
///
/// The fold starts EMPTY, not from the box already there. Growing the existing
/// box instead strands it: once every slot has moved to another sheet, nothing
/// matches it any more, so it would go on naming a sheet no slot reads. The
/// panel seeds DATA RANGE from that box, and Enter there replots the whole
/// chart from the cells the user moved away from, with no undo snapshot.
/// Rebuilding converges the moment the references do, which is what actually
/// makes a chart read the same before and after a save — the loader unions from
/// scratch too, so anything less diverges from it.
///
/// The NUMBERS go in first — every series' values, and a scatter's or bubble's
/// points beside them — and only then the categories and the series' NAME
/// cells. Those two are folded in at all
/// because the loader folds them in (`<c:cat>` carries mode 2 and `<c:tx>` mode
/// 1): leaving them out would drop the label column and the header row from the
/// box, shrinking the DATA RANGE the panel shows from `A1:D5` to `B2:D5` the
/// first time a series was re-pointed. They are folded AFTER because a
/// categories ref is one LINE of labels and a name is one CELL, and each may
/// legally name another sheet than the numbers (`target_takes_foreign_sheet` is
/// `true` for both, and `categories_apply`/`series_apply_name` resolve one) —
/// first, it would seed the box, and every local ref after it would be skipped
/// for the sheet mismatch, collapsing a chart plotting `A1:D5` onto that single
/// foreign line. `parse_chart` holds its `<c:cat>` and `<c:tx>` refs back to
/// the end for the same reason, so the two still agree slot for slot.
///
/// A chart with no parsable reference anywhere keeps the box it had — there is
/// nothing to rebuild it from, and dropping it would blank DATA RANGE.
fn rebuild_source(data: &mut gridcore::sheet::ChartData) {
    use gridcore::sheet::ChartSource;
    // The loader's own fold, called rather than copied: the two have to settle
    // a cross-sheet ref identically or a chart reads one way before a save and
    // another after it. The ORDER below is this function's; the decision it
    // makes per ref is `parse_chart`'s.
    let fold = gridcore::drawing::fold_source;
    let mut built = None;
    // The NUMBERS decide which sheet the box names: every series' values, in
    // series order. That is what the chart IS — the labels and the headers only
    // annotate it.
    //
    // `point_refs` belongs here too, folded with its own series: a scatter or a
    // bubble plots from `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` and so carries no
    // `values_ref` at all, and rebuilding such a chart's box from its label
    // cells alone would collapse the DATA RANGE the panel shows onto one header
    // cell. `parse_chart` folds the two in the same document order.
    for s in &data.series {
        if let Some(src) = s.values_ref.clone() {
            fold(&mut built, src);
        }
        for src in &s.point_refs {
            fold(&mut built, src.clone());
        }
    }
    // Then the LABEL cells, then the HEADER cells. All four slots belong in the
    // box — leaving the categories out drops the label column and leaving the
    // names out drops the header row, shrinking the DATA RANGE the panel shows
    // from `A1:D5` to `B2:D5` the first time a series is re-pointed — but
    // neither may DECIDE the sheet. Categories are one LINE of labels and a
    // name is ONE cell, and each may legally sit on another sheet than the
    // numbers (`target_takes_foreign_sheet` is `true` for both). Folded first
    // one would seed the box, and every local ref after it would then be
    // skipped for the sheet mismatch, collapsing a chart plotting `A1:D5` onto
    // that one foreign line. Folded after, they stretch the box the numbers
    // already decided, or seed it only when nothing else did — categories
    // before names, so a chart whose series are all literal still takes its
    // sheet from its labels rather than from a header cell.
    if let Some(src) = data.categories_ref.clone() {
        fold(&mut built, src);
    }
    for s in &data.series {
        if let Some(src) = s.name_ref.as_deref().and_then(ChartSource::parse_f_ref) {
            fold(&mut built, src);
        }
    }
    if built.is_some() {
        data.source = built;
    }
}

/// Why a picked range can't be a series' values, or `None` if it can.
///
/// A series plots ONE line of cells. Excel splits a two-dimensional pick into a
/// series per line and reads such a ref in that orientation's major order,
/// while `range_numbers` flattens row-major — so accepting a rectangle here
/// would write a `<c:f>` whose own cache is in the wrong order.
///
/// WHICH line is the chart's own reading of its range, not a constant: a
/// column-oriented chart's series is a column (`B2:B5`), a row-oriented one's
/// is a row (`B2:D2`). Checking for a column either way would refuse every
/// range a row chart's series can legally hold, and the message has to name the
/// shape THIS chart wants — sending the user to `B2:B5` on a row chart is worse
/// than not checking, since the range it asks for would be refused again.
fn series_values_shape_err(by_row: bool, range: (u32, u32, u32, u32)) -> Option<&'static str> {
    if by_row {
        (range.0 != range.2).then_some("a series plots one row — point at cells like B2:D2")
    } else {
        (range.1 != range.3).then_some("a series plots one column — point at cells like B2:B5")
    }
}

/// How many of a chart's series the plot actually DRAWS.
///
/// Every kind draws all of them but the pie, which draws the first. That is a
/// PLOTTING rule, not a format one: `CT_PieChart` takes its `ser` from
/// `EG_PieChartShared`, which declares it `maxOccurs="unbounded"` (ECMA-376
/// Part 1, `dml-chart.xsd`), so a pie may HOLD several and the writer keeps
/// every one of them — see `chart_space_xml`'s pie arm. Excel plots the first.
///
/// Said once, here, so the card and the panel answer it the same way. The card
/// used to draw whatever series it was handed and the panel to list them all,
/// which was only ever right because the writer had already thrown the extras
/// away; now that it doesn't, "draw them all" would put slices on screen that
/// Excel will not.
fn chart_plotted_series(kind: &str, series: usize) -> usize {
    if kind == "pie" { series.min(1) } else { series }
}

/// Whether the series at `si` of a `series`-long chart is one of the drawn ones.
fn series_is_plotted(kind: &str, si: usize, series: usize) -> bool {
    si < chart_plotted_series(kind, series)
}

/// What the Chart panel says beside its type buttons when the chart holds
/// series it will not draw, or `None` when everything it holds is plotted.
///
/// The loss this note replaces was invisible precisely because nothing said
/// anything: the panel listed every series, each pointed at cells and
/// colourable, while the save kept one. The data survives now, so the sentence
/// to write is what actually becomes of the rest — kept, not drawn — rather
/// than a refusal.
fn chart_unplotted_note(kind: &str, series: usize) -> Option<String> {
    let extra = series.saturating_sub(chart_plotted_series(kind, series));
    (extra > 0).then(|| {
        // The two-series pie is the COMMON case (a "+ Series" push, a
        // two-column Insert ▸ Pie), so its sentence has to read as
        // English rather than as a count: "the other one is", not "the other 1
        // is". Past one, the numeral is what the reader wants.
        let rest = if extra == 1 {
            "one is".to_string()
        } else {
            format!("{extra} are")
        };
        format!(
            "A {kind} plots the first series only \u{2014} the other {rest} kept in the \
             file but not drawn."
        )
    })
}

/// Why a picked range can't be the category labels, or `None` if it can.
///
/// `<c:cat>` holds ONE line of labels, and `range_labels` flattens whatever it
/// is given row-major, so a genuine RECTANGLE is refused for
/// [`series_values_shape_err`]'s first reason: the cache docxy writes beside
/// the ref and the labels Excel derives from the ref itself would be in
/// different orders, and the two disagree the moment Excel refreshes.
///
/// WHICH line is the chart's own reading of its range, exactly as it is for the
/// values. Category labels name the POINTS of a series, and a series' points
/// run down rows on a column chart and along columns on a row one, so the
/// labels run the same way: down a column (`A2:A5`) or along a row (`B1:D1`).
/// That is the shape both derivations write (`chart_from_columns` takes a label
/// COLUMN, `chart_from_rows` a label ROW) and the shape `chart_field_examples`
/// has always offered here, so this is the guard catching up with its own hint.
///
/// It is also what makes the shape of `<c:cat>` evidence `infer_by_row` can
/// trust. There is no orientation element in SpreadsheetML, so a chart whose
/// series are all single cells is read back through its categories: labels down
/// a column are the column reading's, along a row the row reading's. Accepting
/// either line on either orientation put those two rules in contradiction — a
/// user could commit a row of labels onto a column chart, and the file would
/// come back row-oriented, with the VALUES fields refusing the very refs the
/// series hold and the next DATA RANGE commit folding N series into one. A
/// SINGLE CELL is one row and one column at once, so it fits both readings and
/// goes through either way round, which is what a row chart over a range two
/// columns wide needs. See `suite/docs/chart-orientation.md`.
fn categories_shape_err(by_row: bool, range: (u32, u32, u32, u32)) -> Option<&'static str> {
    if by_row {
        (range.0 != range.2).then_some("category labels are one row — point at cells like B1:D1")
    } else {
        (range.1 != range.3).then_some("category labels are one column — point at cells like A2:A5")
    }
}

/// Where the Chart panel's three range fields point their `e.g.` at, for a
/// chart read this way round.
///
/// A row-oriented chart is the transpose of a column one, so every example has
/// to be too: its series is a ROW (`B2:D2`), the cell naming that series is the
/// one to its LEFT (`A2`), and its category labels lie along the row ABOVE
/// (`B1:D1`). A fixed column-shaped hint is not merely unhelpful on a row chart
/// — `series_values_shape_err` REFUSES the very range the values hint offers,
/// which is the failure that function's own comment calls worse than not
/// checking at all.
///
/// Cells are the 0-based offsets `ref_example` stamps a sheet name onto.
fn chart_field_examples(by_row: bool) -> ChartFieldExamples {
    if by_row {
        ChartFieldExamples {
            name: (1, 0, 1, 0),
            values: (1, 1, 1, 3),
            categories: (0, 1, 0, 3),
        }
    } else {
        ChartFieldExamples {
            name: (0, 1, 0, 1),
            values: (1, 1, 4, 1),
            categories: (1, 0, 4, 0),
        }
    }
}

/// The cell boxes [`chart_field_examples`] hands back, one per range field.
struct ChartFieldExamples {
    /// The single cell naming a series.
    name: (u32, u32, u32, u32),
    /// The line of cells one series plots.
    values: (u32, u32, u32, u32),
    /// The line of cells labelling the axis.
    categories: (u32, u32, u32, u32),
}

/// What a chart's DATA RANGE box has to include for the chart to read it, said
/// the way round THIS chart reads. The header row names the series of a column
/// chart; the label column names the series of a row one.
fn chart_range_help(by_row: bool) -> &'static str {
    if by_row {
        "Include the label column: it names the series. Enter to replot."
    } else {
        "Include the header row: it names the series. Enter to replot."
    }
}

/// Give a chart the type the panel's buttons picked, in place.
///
/// Picking a type is the explicit "author this one afresh" the panel's note
/// asks for, so `complex` is cleared with it: a stacked or combo plot area held
/// the part back, and the user has now said what to replace it with.
///
/// `point_refs` go too, when the new kind is one the WRITER authors. They are a
/// scatter's and a bubble's `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>`, and
/// `chart_space_xml`'s bar/column/line/pie arms emit none of those elements:
/// such a part is regenerated from `values_ref`, `categories_ref` and the box
/// alone. Carried across the conversion they would leave `rebuild_source`
/// folding cells the converted chart no longer plots, stretching the box back
/// over the obsolete X column the next time any field commits.
///
/// (Not the writer's `<c:cat>` fallback, which is a question about the BOX:
/// that reads `data.source`'s `cat_col`, never these, so clearing them is not
/// what keeps an obsolete category ref out of the file. Re-deriving the chart
/// is — see [`chart_reauthored`].)
///
/// The series that reach here still HOLDING points are the ones a re-point gave
/// a `values_ref` beside them; `ChartSeries::point_refs` records that the two
/// slots coexist. A series carrying nothing but points never gets this far,
/// whichever OTHER series the same chart has: `chart_would_lose_points` asks per
/// series, so `chart_set_kind` authors that whole chart afresh from its box
/// instead of converting it into one the writer would save with an empty
/// `<c:val>` where those points were. So the BOX is left as it stands — it is
/// what DATA RANGE offers — and what the next `rebuild_source` folds is the
/// `values_ref` beside the cleared points, not a lone name cell.
///
/// Cleared here rather than in `series_set_values`, because a scatter that
/// STAYS a scatter still needs them to match what the next `parse_chart` reads
/// back out of its round-tripped part.
fn chart_take_kind(data: &mut gridcore::sheet::ChartData, kind: &str) {
    data.kind = kind.to_string();
    data.complex = false;
    if gridcore::xlsx::chart_kind_is_writable(kind) {
        for s in &mut data.series {
            s.point_refs.clear();
            // The same slot, for the points the LOADER could not hold. A series
            // reaching here still marked was re-pointed by hand (a series with
            // nothing but unheld points goes through `chart_reauthored`
            // instead), so it plots from `values_ref` now and the mark is as
            // obsolete as the refs beside it.
            s.points_unheld = false;
            s.points_ref_unheld = false;
        }
    }
}

/// Whether relabelling this chart a writable kind would hand `chart_space_xml`
/// a series holding points it cannot write — the question that decides whether
/// picking such a type may simply RELABEL the chart or has to author it afresh.
///
/// The writer takes a series' `<c:val>` from one of three places: its own
/// `values_ref`, the chart's box plus the series' `col`, or — for a snapshot
/// chart — the cached `values` written back as a `<c:numLit>`. A scatter's and
/// a bubble's points come from none of the three. `parse_chart` keeps their
/// `<c:xVal>`/`<c:yVal>` REFS (`ChartSeries::point_refs`) but caches no numbers
/// from them, and never sets `col`, which is exactly why
/// `chart_kind_is_writable` refuses those kinds. Relabel such a series `column`
/// and every one of the three lookups comes back empty: the save writes
/// `<c:val><c:numLit><c:ptCount val="0"/></c:numLit></c:val>` over the part,
/// and its 50 points are gone.
///
/// Asked per SERIES, because `chart_space_xml` writes each one independently.
/// A scatter whose first series a re-point gave a `values_ref` and whose second
/// still carries nothing but points is not half safe: relabel it and the file
/// keeps the half the user touched and loses the half nobody did, silently and
/// with no `complex` to hold the part back. One such series is enough to send
/// the whole chart through [`chart_reauthored`], which reads every numeric
/// column of the box and so brings both back carrying refs the writer can emit.
///
/// "It holds points" is what makes it a LOSS rather than an empty series the
/// user built themselves: "+ Series" pushes one with no refs and no numbers
/// (`series_add` — `values` is empty whenever the chart has no categories yet),
/// and that one has nothing to destroy, while re-deriving the chart over it
/// would throw away the hand edits on every OTHER series. So the question is
/// not "can the writer emit this series" but "does it hold points the writer
/// cannot emit". A chart with no series at all is already the empty chart, so
/// it relabels too — there is no plot to destroy.
///
/// Which is why it takes TWO slots to ask, not just `point_refs`. That vec is
/// filled only when the loader could parse a `<c:f>` out of the point elements;
/// a scatter whose points are literal (`<c:xVal><c:numLit>`) or name a whole
/// column has just as much to lose and would arrive here indistinguishable from
/// the freshly-added empty series above. `parse_chart` marks that case
/// `ChartSeries::points_unheld` for exactly this question. Such a chart usually
/// has no box worth re-deriving either, and that is the point: it gets
/// `chart_reauthored`'s refusal — `CHART_NO_BOX`, or the shape its range fails
/// to plot — where before it got a silent `<c:ptCount val="0"/>`.
fn chart_would_lose_points(data: &gridcore::sheet::ChartData) -> bool {
    data.series.iter().any(series_loses_points)
}

/// [`chart_would_lose_points`] asked of ONE series, so the two questions that
/// need it — "is this chart's plot at risk" and "which half of it is off the
/// box" ([`chart_points_off_box`]) — cannot drift apart.
fn series_loses_points(s: &gridcore::sheet::ChartSeries) -> bool {
    (!s.point_refs.is_empty() || s.points_unheld)
        && s.values_ref.is_none()
        && s.col.is_none()
        && s.values.is_empty()
}

/// Whether a series the re-author is about to rescue plots cells the box `src`
/// provably does NOT cover.
///
/// Two shapes answer yes, and both are the same defect: the series names cells
/// and `rebuild_source` folded none of them, so the box describes part of the
/// plot and re-deriving from it would drop the rest.
///
/// - **A ref the loader could not hold** (`ChartSeries::points_ref_unheld`).
///   The marks are per point ELEMENT, so one series can carry both: an
///   `<c:xVal>` naming `Sheet1!$A:$A` (which `parse_f_ref` refuses) beside a
///   `<c:yVal>` the loader held gives `point_refs.len() == 1` AND the mark.
/// - **A held ref naming ANOTHER SHEET than the box.** Excel is happy for a
///   scatter's X to sit on `Data` and its Y on `Other`, and
///   [`gridcore::drawing::fold_source`] SKIPS the second rather than unioning
///   across sheets (which would leave the box naming one sheet and covering the
///   other's cells). Nothing else marks that skip — `complex` is not set for it,
///   because a point element carries no `mode` — so the sheets have to be
///   compared here. `chart_from_range` reads ONE sheet, so a re-derivation would
///   plot the half on the box's sheet and silently lose the half that is not.
///
/// Either way the box comes out over one coordinate alone — and
/// [`chart_reauthored`] would widen THAT and plot it, converting the scatter to
/// a chart of one coordinate with the other silently outside the range it
/// re-read. The status line's "the series below are the range's" is true and
/// still says nothing about the half that was never in the range.
///
/// LITERAL points (`<c:numLit>`, marked `points_unheld` but not
/// `points_ref_unheld`) are a different shape and deliberately not caught, even
/// when they sit beside a held ref on the same series: they are in no cells at
/// all, so there is no box that could have covered them and re-deriving from
/// the one the chart has is the best that exists. Asking `points_unheld` here
/// instead would refuse an ordinary bubble whose `<c:bubbleSize>` is a literal
/// beside held X/Y refs — a chart whose box covers every cell its plot names.
///
/// Asked of the series [`chart_would_lose_points`] answers for, not of every
/// series: one whose `values_ref` a re-point already filled is saved by the
/// writer as it stands, its ref is in the fold, and the unheld element beside
/// it is what `chart_take_kind` clears on any relabel anyway.
fn chart_points_off_box(
    data: &gridcore::sheet::ChartData,
    src: &gridcore::sheet::ChartSource,
) -> bool {
    data.series
        .iter()
        .filter(|s| series_loses_points(s))
        .any(|s| {
            s.points_ref_unheld
                // Case-insensitively, the way a sheet name resolves everywhere
                // else and the way `fold_source` itself decided to skip the ref.
                || s.point_refs
                    .iter()
                    .any(|p| !p.sheet.eq_ignore_ascii_case(&src.sheet))
        })
}

/// Whether the chart's box was folded out of POINT refs, and so need not lead
/// with a header line at all.
///
/// The provenance question, asked of the slots that answer it: `<c:xVal>`,
/// `<c:yVal>` and `<c:bubbleSize>` are the only things `parse_chart` folds into
/// a box that `chart_from_range` would go on to read as a header — everything
/// else in the fold is either a `<c:val>` (which starts a line BELOW the header
/// row, because that is where the derivation put it) or a label slot that
/// stretches the box up over one. A chart with no point refs anywhere therefore
/// has the box the user can see in DATA RANGE, shaped the way
/// [`gridcore::sheet::chart_from_range`] shapes one.
///
/// Deliberately NOT [`chart_would_lose_points`], which is the narrower question
/// "would a relabel throw points away" and goes false the moment a series is
/// re-pointed. `series_set_values` fills `values_ref` and leaves `point_refs`
/// alone — `rebuild_source` still folds them, so the box still sits on the
/// points — and gating the widening on the narrow question would hand the
/// re-pointed scatter's unwidened box to `chart_from_range` to eat a line of.
/// Asked of the whole CHART rather than per series, because it is one box.
fn chart_box_from_points(data: &gridcore::sheet::ChartData) -> bool {
    data.series
        .iter()
        .any(|s| !s.point_refs.is_empty() || s.points_unheld)
}

/// The chart's box, widened by one line if its leading one is PLOTTED rather
/// than a header — or `None` when there is no line to widen into.
///
/// [`chart_from_range`](gridcore::sheet::chart_from_range) reads every box the
/// same way: with `by_row` false the box's first ROW names the series and the
/// data starts one row below it (`chart_from_columns` plots `r0 + 1..=r1`),
/// transposed for `by_row`. Every box docxy itself derives has that shape,
/// because that derivation is where it came from.
///
/// An imported SCATTER's does not. `parse_chart` folds a scatter's box out of
/// the `<c:xVal>`/`<c:yVal>` refs — its NUMBERS — and only then lets `<c:cat>`
/// and `<c:tx>` stretch it. A series whose `<c:tx>` is a literal (`<c:v>Speed`,
/// no `<c:f>`, which is how Excel writes a typed series name) leaves nothing to
/// stretch it upward, so the box arrives sitting exactly on the points: `A2:B4`
/// for points in rows 2-4, where the equivalent authored chart's box is `A1:B4`.
/// Handed to `chart_from_range` unchanged, row 2 is eaten as the header row and
/// a three-point scatter comes back a TWO-point column chart named after the
/// numbers it just consumed — silently, and then saved over the original part.
///
/// So the leading line is measured against the cells the chart says it PLOTS:
/// every series' `point_refs` and `values_ref`, on the box's own sheet (a
/// foreign ref was never folded into the box, so its rows say nothing about it).
/// If ANY of them reaches the box's first line, that line holds plotted numbers
/// and is not a header — `any`, not `all`, because one series starting there is
/// enough for the header reading to eat its first point.
///
/// Widening rather than refusing keeps this the SAME question DATA RANGE and
/// Insert answer: the chart wanted is the one the user would have got by
/// selecting those cells, and they would have selected the header line too. The
/// line above may be blank, and that is fine — the series come back unnamed,
/// which is what the scatter's literal `<c:tx>` names amount to here anyway.
/// What cannot be fixed is a box already against the sheet's edge, and
/// [`chart_reauthored`] says so rather than quietly eating a row of the plot.
///
/// `by_row` is the orientation the box is ABOUT to be read as, which is not
/// always the chart's own: [`chart_reauthored`] keeps `data.by_row`, and Switch
/// Row/Column asks for the flipped one, since which line is the header is a
/// question about the READING and a box that leads with a header row need not
/// lead with a label column.
fn chart_box_with_header(
    data: &gridcore::sheet::ChartData,
    src: &gridcore::sheet::ChartSource,
    by_row: bool,
) -> Option<(u32, u32, u32, u32)> {
    let (r0, c0, r1, c1) = src.range;
    let leads = data
        .series
        .iter()
        .flat_map(|s| s.point_refs.iter().chain(s.values_ref.iter()))
        .filter(|p| p.sheet.eq_ignore_ascii_case(&src.sheet))
        .any(|p| {
            if by_row {
                p.range.1 == c0
            } else {
                p.range.0 == r0
            }
        });
    if !leads {
        return Some(src.range);
    }
    if by_row {
        c0.checked_sub(1).map(|c| (r0, c, r1, c1))
    } else {
        r0.checked_sub(1).map(|r| (r, c0, r1, c1))
    }
}

/// How to NAME the box's leading line to a user whose chart has no room to
/// widen into: where its points start, the line that isn't there, and the fix.
/// Shared so the two doors that widen a box — [`chart_reauthored`] and Switch
/// Row/Column — refuse in the same words, each for the orientation IT is about
/// to read the box as.
fn chart_header_edge(by_row: bool) -> (&'static str, &'static str, &'static str) {
    if by_row {
        ("column A", "label column beside", "a column before")
    } else {
        ("row 1", "header row above", "a row above")
    }
}

/// [`MAX_CHART_CELLS`] asked of a box, in the sentence
/// [`Docxy::chart_range_sheet`] has always printed.
///
/// Asked in two places for one reason: the box that reaches
/// [`gridcore::sheet::chart_from_range`] is not always the box that was
/// counted. [`chart_box_with_header`] adds a LINE to it, and a range that only
/// just fitted would otherwise walk past the cap on the strength of a header
/// row nobody counted.
fn chart_cells_within_cap(range: (u32, u32, u32, u32)) -> Result<(), String> {
    let (r0, c0, r1, c1) = range;
    let cells = u64::from(r1 - r0 + 1) * u64::from(c1 - c0 + 1);
    if cells > MAX_CHART_CELLS {
        return Err(format!(
            "Its range is {cells} cells; a chart plots at most {MAX_CHART_CELLS}."
        ));
    }
    Ok(())
}

/// Author a chart of `kind` afresh from the box it is already showing, or WHY
/// its box can't be read that way.
///
/// This is what "picking a type is the explicit *author this one afresh*" means
/// for a chart the writer cannot otherwise save — a scatter or bubble, any of
/// whose series carry refs but no numbers ([`chart_would_lose_points`]). Going
/// through `chart_from_range` is the same call DATA RANGE and the Insert button
/// make, so a converted chart is indistinguishable from one authored that way
/// round in the first place, and it comes out with real `values_ref`s instead
/// of series the save would empty.
///
/// It is also what keeps the box honest. `chart_take_kind` leaves `data.source`
/// alone, and for a scatter stripped of its `point_refs` the only foldable slot
/// left would be the `<c:tx>` name cell — so the next `rebuild_source`, which
/// `series_apply_name`, `series_delete`, `series_reorder` and
/// `categories_apply` all call, would collapse a box over `A1:B4` onto `B1`,
/// and Enter on that DATA RANGE would be refused for having no column of
/// numbers under a header row. Re-deriving here means there is never such a
/// chart: every series has a `values_ref` the rebuild can fold.
///
/// Two things about the box are NOT taken on trust, because `chart_from_range`
/// assumes both of a box IT derived and an imported scatter's satisfies neither
/// by construction:
/// - Its leading line is a HEADER. See [`chart_box_with_header`], which widens
///   the box by one line when the plotted refs reach its edge, and whose refusal
///   is the first this function can return.
/// - Its ORIENTATION. `data.by_row` decides whether the box reads as a series
///   per numeric column or per numeric row, and `infer_by_row` has no `<c:val>`
///   to read it off for these kinds — so it reads their `point_refs` too, and a
///   scatter laid out along ROWS comes back one series per row instead of as N
///   one-point series. Switch Row/Column remains the remedy for a chart whose
///   shape says nothing either way.
///
/// What rides along and what doesn't, for `chart_switch_row_column`'s reasons:
/// - **The title** is kept — it may have been typed.
/// - **`part`** is kept, and it must be: the writer only overwrites a chart
///   whose part it knows (`cd.part`), so dropping it would leave the ORIGINAL
///   scatter part on disk and the conversion invisible in the saved file.
/// - **`complex`** is NOT kept: picking a type is the user saying what to
///   replace the plot area with, which is the same thing `chart_take_kind`
///   clears it for. `chart_from_range` says `false`.
/// - **Series colours** do not: the series are different data now — a scatter's
///   one X/Y pair becomes a series per numeric column — so matching by position
///   would paint the wrong one.
fn chart_reauthored(
    data: &gridcore::sheet::ChartData,
    kind: &str,
    sheet: &gridcore::sheet::Sheet,
) -> Result<gridcore::sheet::ChartData, String> {
    let src = data
        .source
        .as_ref()
        .ok_or_else(|| CHART_NO_BOX.to_string())?;
    // A series naming point cells the fold took none of — a ref the loader could
    // not hold, or one on a sheet other than the box's — is the shape where the
    // box is KNOWN to be short of the plot: `rebuild_source` folded the rest and
    // nothing at all of that half. Re-deriving from it would plot one coordinate
    // and drop the one that is off the box — the same silent loss this whole
    // door exists to prevent, arriving by the door itself. See
    // [`chart_points_off_box`].
    if chart_points_off_box(data, src) {
        return Err(CHART_POINTS_OFF_BOX.to_string());
    }
    // The box a scatter arrives with sits on its POINTS, with no header line for
    // `chart_from_range` to name the series from; see [`chart_box_with_header`].
    let range = chart_box_with_header(data, src, data.by_row).ok_or_else(|| {
        let (start, edge, insert) = chart_header_edge(data.by_row);
        format!(
            "Its points start at {start}, so there is no {edge} them for a {kind} chart to name its series from \u{2014} insert {insert}, or point DATA RANGE at the cells it should read."
        )
    })?;
    // The cap `chart_range_sheet` applied was the box's; this is a line wider,
    // and a box that only just fitted must not walk past it because the header
    // line was added after the counting.
    chart_cells_within_cap(range)?;
    let mut out = gridcore::sheet::chart_from_range(sheet, &sheet.name, range, kind, data.by_row)
        .ok_or_else(|| {
        // Said the way round THIS chart reads, as every other range
        // refusal is: telling a row chart's user to go and find a column
        // sends them after the wrong shape.
        let (line, edge) = if data.by_row {
            ("row", "beside a label column")
        } else {
            ("column", "under a header row")
        };
        format!(
            "Its range has no {line} of numbers {edge} to plot as a {kind} chart \
                 \u{2014} point DATA RANGE at the cells it should read first."
        )
    })?;
    // Nothing is asked about the series COUNT any more. A box two numeric
    // columns wide re-derives as two series whatever the kind, pie included:
    // `chart_space_xml` writes every one of them, so the second is kept in the
    // file rather than dropped on save, and the panel says which of them the
    // plot draws (`chart_plotted_series`, `chart_unplotted_note`).
    out.title = data.title.clone();
    out.part = data.part.clone();
    Ok(out)
}

/// A box KNOWN to be short of its chart's plot cannot be re-derived from either,
/// and for the same reason at both doors: `chart_from_range` reads the box, so
/// whatever the fold skipped comes back missing. See [`chart_points_off_box`].
const CHART_POINTS_OFF_BOX: &str = "Part of its plot is outside its data range, which covers only the rest \u{2014} point DATA RANGE at the cells the whole plot lives in first.";

/// A chart whose references this model can't hold has no box to re-derive from,
/// which is the same answer for every door that asks — Switch Row/Column and
/// picking a type alike.
const CHART_NO_BOX: &str =
    "No data range to re-read \u{2014} this chart came with references the model can't hold.";

/// Re-point series `i` at `src`, plotting `values`. Reports how many points it
/// took, or `None` when there is no series `i`.
///
/// `col` is the column a series occupies, and only a column-oriented one
/// occupies one: a row series spans every column of its ref. It feeds the
/// writer's fallback ref (`src.f_ref(s.col?, s.col?, true)`) and `claimed_col`,
/// both column-shaped questions, so a row series' left-hand column index there
/// would make them quietly wrong rather than inapplicable — the same reason
/// `chart_from_rows` leaves it `None`.
///
/// The chart's box is rebuilt because it is the union of what the chart reads,
/// so the DATA RANGE the panel shows covers this series too — otherwise it only
/// caught up after a save and reload, when `parse_chart` re-unions the refs.
/// Rebuilt from every slot rather than grown, so it follows the references off
/// a sheet instead of being stranded on one none of them read any more.
fn series_set_values(
    data: &mut gridcore::sheet::ChartData,
    i: usize,
    values: Vec<f64>,
    src: gridcore::sheet::ChartSource,
) -> Option<usize> {
    let by_row = data.by_row;
    let s = data.series.get_mut(i)?;
    let n = values.len();
    s.values = values;
    s.col = (!by_row).then_some(src.range.1);
    s.values_ref = Some(src);
    rebuild_source(data);
    Some(n)
}

/// Read a chart's range the other way round: what was a series becomes a
/// category and back again, which is Excel's `Switch Row/Column`.
///
/// This is a RE-DERIVATION, not a rearrangement of the series already there.
/// Flipping goes back through `chart_from_range` — the same call the DATA RANGE
/// field and the Insert button make — so exactly one piece of code decides what
/// a range means, and a flipped chart is indistinguishable from one authored
/// that way round in the first place.
///
/// `sheet` must be the sheet `data.source` NAMES, not whichever is on screen: a
/// chart floating over one sheet can plot another's numbers, and re-deriving
/// from the wrong one would quietly replot it against the cells underneath it.
///
/// The refusals, which are what greys the button out and what the note under it
/// prints: there is no `source` box to re-derive from (an imported chart whose
/// refs the model can't hold), the box's leading line the OTHER way round is
/// plotted and there is nowhere to widen into, or the range doesn't read the
/// other way round at all — a single column of numbers has no row of them.
///
/// The middle one is [`chart_box_with_header`]'s, asked for the orientation the
/// flip is ABOUT to read the box as rather than the one it has: a scatter whose
/// box `parse_chart` folded out of its points sits ON them, so read the other
/// way round its first column is data and `chart_from_rows` would consume it as
/// the series names — a three-point scatter back as three one-point series
/// named after the X values it just ate, and every one of them carrying a
/// `values_ref` the writer will happily save over the original part.
///
/// Only a chart [`chart_box_from_points`] accepts is widened — the set whose
/// box came out of point refs, asked of the refs rather than of what a relabel
/// would cost, so a scatter half re-pointed by hand is still one. Every other
/// box is the one the user can see in DATA RANGE, derived by `chart_from_range`
/// from a line the reading already treats as a header, and widening THAT would
/// silently pull in a column they never selected and move the field under their
/// hands: a `Year | Sales` box is all numbers, so its first series starts in the
/// box's own leading column and the plotted-line test alone would fire on it.
/// Flipping twice must return the chart the range describes.
///
/// What the widening does NOT promise is a way back to the imported scatter. The
/// flip re-derives, so the chart that comes out is one `chart_from_range` made
/// from the widened box and has left the points-only class for good; flipping IT
/// twice returns it, but the box it now carries leads with the line the FIRST
/// flip needed and not with one the other reading can use. That is the same
/// answer any hand edit gets here — the range is the source of truth again — and
/// undo, not a second flip, is what puts the scatter back.
///
/// What rides along and what doesn't:
/// - **Title, part and `complex`** are kept, for the reasons `chart_apply_range`
///   keeps them: the title may have been typed, and taking `chart_from_range`'s
///   `complex: false` would let the writer regenerate a stacked or combo plot
///   area as a plain clustered one.
/// - **Series colours do not.** The flipped series are different data — after
///   the Overview's example the series are Laptop/Monitor/Keyboard where they
///   were Qty/Unit price/Total — so matching colours by position would paint
///   "Laptop" with the colour the user chose for "Qty". There is usually not
///   even the same NUMBER of them.
///
/// For the same reason, hand edits to the plot (a series removed, one re-pointed
/// at another column) do not survive a flip: the range is the source of truth
/// again. Flipping twice therefore returns the chart the range describes, which
/// IS the original chart for one that was derived from its range and never
/// hand-edited.
fn chart_switch_row_column(
    data: &gridcore::sheet::ChartData,
    sheet: &gridcore::sheet::Sheet,
) -> Result<gridcore::sheet::ChartData, String> {
    let src = data
        .source
        .as_ref()
        .ok_or_else(|| CHART_NO_BOX.to_string())?;
    // The flip re-derives through `chart_from_range` exactly as the re-author
    // door does, so a box the fold left short of the plot loses the same half
    // here — silently, and on screen rather than only on disk. Refused in the
    // same words, which also greys the button out with the reason under it.
    if chart_points_off_box(data, src) {
        return Err(CHART_POINTS_OFF_BOX.to_string());
    }
    let to_row = !data.by_row;
    let range = if chart_box_from_points(data) {
        let range = chart_box_with_header(data, src, to_row).ok_or_else(|| {
            let (start, edge, insert) = chart_header_edge(to_row);
            format!(
                "Read the other way round its points start at {start}, so there is no {edge} them to name the series from \u{2014} insert {insert}, or point DATA RANGE at the cells it should read."
            )
        })?;
        // A line wider than the box `chart_range_sheet` counted; see
        // `chart_cells_within_cap`.
        chart_cells_within_cap(range)?;
        range
    } else {
        src.range
    };
    let mut out = gridcore::sheet::chart_from_range(sheet, &sheet.name, range, &data.kind, to_row)
        .ok_or_else(|| {
            format!(
                "Its range has no {} of numbers to read the other way round.",
                if data.by_row { "column" } else { "row" },
            )
        })?;
    out.title = data.title.clone();
    out.part = data.part.clone();
    out.complex = data.complex;
    Ok(out)
}

/// Move series `i` by `delta` places, clamped to the ends. Returns where it
/// landed, or `None` if it couldn't move. Colours ride along, since they live
/// on the series rather than on its position.
///
/// The ones it moves past close up behind it, as a list reorder does — a swap
/// would be the same thing for the ±1 the arrows send, but would silently
/// scramble the order for anything wider.
fn series_move(list: &mut [gridcore::sheet::ChartSeries], i: usize, delta: i32) -> Option<usize> {
    if i >= list.len() {
        return None;
    }
    let to = (i as i32 + delta).clamp(0, list.len() as i32 - 1) as usize;
    if to == i {
        return None;
    }
    if to > i {
        list[i..=to].rotate_left(1);
    } else {
        list[to..=i].rotate_right(1);
    }
    Some(to)
}

/// Whether byte `at` sits inside a `"…"` string literal or a `'…'` quoted sheet
/// name — the two places in a formula where text may look like a reference and
/// isn't. (A doubled quote escapes itself, and toggling twice lands on the same
/// answer, so no special case is needed for it.)
fn quoted_at(buf: &str, at: usize) -> bool {
    let (mut dq, mut sq) = (false, false);
    for (i, c) in buf.char_indices() {
        if i >= at {
            break;
        }
        match c {
            '"' if !sq => dq = !dq,
            '\'' if !dq => sq = !sq,
            _ => {}
        }
    }
    dq || sq
}

/// The byte range of the cell reference the caret sits in or immediately after,
/// so pointing at the grid REPLACES the reference you are standing on rather
/// than appending a second one. `None` when the caret isn't on a reference —
/// after an operator, a comma or an open bracket — where a pick inserts instead.
///
/// A token followed by `(` is a function name, not a reference: `LOG10(` reads
/// as a cell otherwise, exactly as it does in Excel.
fn ref_token_at(buf: &str, caret_chars: usize) -> Option<std::ops::Range<usize>> {
    let caret = char_to_byte(buf, caret_chars);
    let is_ref_char = |c: char| c.is_ascii_alphanumeric() || c == '$' || c == ':';
    let start = buf[..caret]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ref_char(*c))
        .map(|(i, _)| i)
        .last()?;
    let end = buf[caret..]
        .char_indices()
        .take_while(|(_, c)| is_ref_char(*c))
        .map(|(i, c)| caret + i + c.len_utf8())
        .last()
        .unwrap_or(caret);
    if buf[end..].starts_with('(') {
        return None; // a function name
    }
    if buf[end..].starts_with('[') {
        return None; // a structured reference's table name — `T1[Amount]`
    }
    if quoted_at(buf, start) {
        // Inside `"…"` or `'…'`: a string literal's contents, or a quoted sheet
        // name. `formula_ref_tokens` steps over both, and repointing either one
        // rewrites text that never named a cell — `='Q1'!A1` would become
        // `='D7'!A1`, naming a sheet that doesn't exist.
        return None;
    }
    if buf[..start].ends_with('!') {
        // `Sheet2!A1` — the cell half of another sheet's reference. Replacing it
        // would silently repoint that reference at THIS sheet's picked cell, so
        // a pick inserts instead. (`formula_ref_tokens` skips these for the same
        // reason: we can only outline the sheet we are looking at.)
        return None;
    }
    if buf[end..].starts_with('!') {
        // The SHEET half, unquoted. A cell-shaped sheet name — `Q1`, `H1`, `FY1`
        // — parses as a reference, and `translate_formula` writes exactly that
        // form (`sheet_prefix` quotes only names with non-identifier
        // characters). Replacing it would turn `=Q1!B2` into `=D7!B2`: a
        // reference to a sheet that doesn't exist.
        return None;
    }
    let token = &buf[start..end];
    // `D2:` is half a range — typed, not finished. It counts as the reference
    // under the caret, so a pick completes it instead of appending and leaving
    // `=SUM(D2:D2:D5`.
    let typed = token.strip_suffix(':').unwrap_or(token);
    gridcore::sheet::parse_range_name(typed).map(|_| start..end)
}

/// Write `text` into a formula buffer at the caret: over the reference the
/// caret is on, or inserted where it stands. Returns the new buffer and where
/// the caret lands (after what was written).
fn replace_ref(buf: &str, caret_chars: usize, text: &str) -> (String, usize) {
    let span = ref_token_at(buf, caret_chars).unwrap_or_else(|| {
        let at = char_to_byte(buf, caret_chars);
        at..at
    });
    let mut out = String::with_capacity(buf.len() + text.len());
    out.push_str(&buf[..span.start]);
    out.push_str(text);
    out.push_str(&buf[span.end..]);
    let caret = out[..span.start + text.len()].chars().count();
    (out, caret)
}

/// One reference inside a formula: the byte span it occupies in the text, and
/// the cells `(r1, c1, r2, c2)` it names.
type RefToken = (std::ops::Range<usize>, (u32, u32, u32, u32));

/// What one of the small per-series buttons (move up/down, remove) does when
/// it is clicked.
type SeriesAction = Box<dyn Fn(&mut Docxy, &mut Context<Docxy>)>;

/// Every cell reference in a formula, in the order it is written: where it sits
/// in the text and which cells it names. One scan feeds both the outlines on the
/// grid and the colouring of the text, so the two can't disagree.
///
/// Skips what only looks like a reference: function names (`LOG10(`), the cell
/// part of another sheet's ref (`Sheet2!A1`, which this grid can't outline), and
/// anything inside a string literal.
fn formula_ref_tokens(buf: &str) -> Vec<RefToken> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let c = buf[i..].chars().next().unwrap_or(' ');
        if c == '"' {
            // Step over a string literal whole; "A1" in there is text.
            i += 1;
            while i < buf.len() && !buf[i..].starts_with('"') {
                i += buf[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            }
            i += 1;
            continue;
        }
        if c == '\'' {
            // A quoted SHEET name, and never a cell: `='Q1'!A1` names a cell on
            // the sheet Q1, not the cell Q1 here. Stepping over it also keeps
            // the `!` in front of the cell part, which is what tells the scan
            // below that the cell belongs to another sheet.
            i += 1;
            while i < buf.len() && !buf[i..].starts_with('\'') {
                i += buf[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            }
            i += 1;
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == '$') {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let mut end = i;
        while end < buf.len() {
            let ch = buf[end..].chars().next().unwrap_or(' ');
            if ch.is_ascii_alphanumeric() || ch == '$' || ch == ':' {
                end += ch.len_utf8();
            } else {
                break;
            }
        }
        let is_call = buf[end..].starts_with('(');
        let qualified = buf[..start].ends_with('!');
        // The sheet half of an unquoted qualified ref (`=Q1!B2` — a sheet named
        // `Q1`). Cell-shaped, so `parse_range_name` takes it and we'd outline
        // cell Q1 of the sheet in view while the real ref went uncoloured.
        let is_sheet_name = buf[end..].starts_with('!');
        // A structured reference's table name — `T1[Amount]`. Short ones are
        // cell-shaped; longer ones are only rejected by `parse_col`'s XFD bound,
        // which is an accident rather than a check.
        let is_table = buf[end..].starts_with('[');
        if !is_call
            && !qualified
            && !is_sheet_name
            && !is_table
            && let Some(r) = gridcore::sheet::parse_range_name(&buf[start..end])
        {
            out.push((start..end, r));
        }
        i = end.max(start + 1);
    }
    out
}

/// The colour a formula's `i`-th reference is drawn in, on the grid and in the
/// text alike.
fn ref_color(i: usize) -> u32 {
    const REF_COLORS: [u32; 6] = [0x2F6FDB, 0xC0705A, 0x7A5EA8, 0x2AA79B, 0xD8A44A, 0xD06C9E];
    REF_COLORS[i % REF_COLORS.len()]
}

/// How many cells a range covers — the size the overlap rule ranks by.
fn range_cells(range: (u32, u32, u32, u32)) -> u64 {
    let (r0, c0, r1, c1) = range;
    (r1 as u64 - r0 as u64 + 1) * (c1 as u64 - c0 as u64 + 1)
}

/// Whether `range` covers cell `(r, c)`.
fn range_covers(range: (u32, u32, u32, u32), r: u32, c: u32) -> bool {
    let (r0, c0, r1, c1) = range;
    r >= r0 && r <= r1 && c >= c0 && c <= c1
}

/// Which of a list of ranges owns cell `(r, c)` when they overlap: the SMALLEST
/// one covering it, earliest index on a tie. A cell claimed twice belongs to
/// the tighter claim.
///
/// Two lists ask that question — a formula's references and a selected chart's
/// source areas — and they have to answer it the same way. They do NOT share
/// this function, though: `chart_areas_at` needs every covering area in paint
/// order rather than the single winner, because an outline's loser is a
/// rectangle whose side ran through the cell (see its own doc). What is shared
/// is the arithmetic underneath — `range_cells` and `range_covers` — so the
/// two cannot drift on what "smaller" and "covers" mean; the ORDER each wants
/// is stated once in each place, and `chart_areas_at`'s reversed sort is
/// deliberately this `min_by_key` read backwards.
fn smallest_ref_at(
    ranges: impl Iterator<Item = (u32, u32, u32, u32)>,
    r: u32,
    c: u32,
) -> Option<usize> {
    ranges
        .enumerate()
        .filter(|&(_, rg)| range_covers(rg, r, c))
        .min_by_key(|&(i, rg)| (range_cells(rg), i))
        .map(|(i, _)| i)
}

/// Which of a formula's references owns cell `(r, c)` for colouring: the
/// SMALLEST one covering it, earliest index on a tie.
///
/// References nest — `=SUM(B2:B5)/B3` covers B3 twice. The text colours B3 by
/// the token it sits in (the inner one), so the grid has to agree or the second
/// reference would have no cell drawn in its colour at all.
fn ref_index_at(refs: &[(u32, u32, u32, u32)], r: u32, c: u32) -> Option<usize> {
    smallest_ref_at(refs.iter().copied(), r, c)
}

/// Which slot of a chart a source area fills — what those cells MEAN to the
/// chart, rather than which reference they happen to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChartSlot {
    /// A series' numbers: its `<c:val>`, or a scatter's/bubble's point refs.
    Values,
    /// The category labels: `<c:cat>`.
    Categories,
    /// A series' name cell: its `<c:tx>`, usually the column header.
    Name,
}

/// The colours a selected chart's source areas are outlined in — **Excel's own
/// mapping**, deliberately, and NOT the `ref_color` palette above.
///
/// The two answer different questions. `ref_color` says "the Nth reference of
/// the formula you are typing": its colours mean an ORDER, and cycle once they
/// run out. These three say what the cells ARE to the chart, and anyone
/// arriving from Excel already knows them by sight — blue values, purple
/// categories, green series names. Matching Excel beats matching docxy for
/// exactly that reason, so please don't "unify" these with the palette.
const CHART_VALUES_COLOR: u32 = 0x4472c4; // blue
const CHART_CATEGORIES_COLOR: u32 = 0x7030a0; // purple
const CHART_NAME_COLOR: u32 = 0x00b050; // green

/// The colour a source area is outlined in, by what it feeds the chart.
fn chart_slot_color(slot: ChartSlot) -> u32 {
    match slot {
        ChartSlot::Values => CHART_VALUES_COLOR,
        ChartSlot::Categories => CHART_CATEGORIES_COLOR,
        ChartSlot::Name => CHART_NAME_COLOR,
    }
}

/// One area of the sheet a selected chart reads, and what it reads it as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ChartSourceArea {
    range: (u32, u32, u32, u32),
    slot: ChartSlot,
}

/// The areas the chart reads on the sheet called `sheet`, one per reference it
/// holds — for the grid to outline while the chart is selected.
///
/// SLOTS, not the box. `ChartData::source` is the union the panel's DATA RANGE
/// shows, and outlining it would draw one rectangle around everything and say
/// nothing about what any part of it does. What the user is asking when they
/// select a chart is *which cells are the numbers, which are the labels* — so
/// this walks `values_ref`, `point_refs`, `categories_ref` and `name_ref`
/// instead, exactly the four slots the panel edits.
///
/// The ORDER is `rebuild_source`'s: every series' numbers first, then the
/// categories, then the name cells. It matters because it is the tie-break for
/// two areas of equal size (`chart_areas_at`), and the model's own fold order is
/// the one already justified.
///
/// Only THIS sheet's cells. A ref naming another sheet gets nothing at all,
/// exactly as `preview_range` refuses the pointed-range wash for one — the
/// cells it names are real, but they are not the cells in front of you, and
/// drawing this sheet's cells of the same address would be a lie. A ref naming
/// NO sheet is the chart's own, which is this one (a chart is only selectable
/// on the sheet it floats over).
/// Whether a selected chart's source outlines belong on the grid right now.
///
/// Two conditions, and the second is the one that is easy to miss: picking a
/// range is the ONE time a chart stays selected while the grid is being used
/// for something else. The panel's field holds the keyboard, the chart keeps
/// its handles so the panel stays live, and without this the three source
/// colours sit under the dashed preview being dragged across them — three
/// answers to "which cells matter" at the moment exactly one of them does.
fn chart_outlines_shown(chart_selected: bool, picking_a_range: bool) -> bool {
    chart_selected && !picking_a_range
}

fn chart_source_areas(cd: &gridcore::sheet::ChartData, sheet: &str) -> Vec<ChartSourceArea> {
    use gridcore::sheet::ChartSource;
    let mut out: Vec<ChartSourceArea> = Vec::new();
    let mut push = |src: &ChartSource, slot: ChartSlot| {
        if !(src.sheet.is_empty() || src.sheet.eq_ignore_ascii_case(sheet)) {
            return; // another sheet's cells; nothing to draw here
        }
        let area = ChartSourceArea {
            range: src.range,
            slot,
        };
        // Two series pointed at one cell, or a re-point that left a duplicate,
        // would otherwise draw the same box twice for no difference on screen.
        if !out.contains(&area) {
            out.push(area);
        }
    };
    for s in &cd.series {
        if let Some(src) = &s.values_ref {
            push(src, ChartSlot::Values);
        }
        // A scatter's and a bubble's numbers live here instead of in
        // `values_ref`, so leaving them out would outline nothing at all for
        // the one chart kind whose plot IS its refs.
        for src in &s.point_refs {
            push(src, ChartSlot::Values);
        }
    }
    if let Some(src) = &cd.categories_ref {
        push(src, ChartSlot::Categories);
    }
    for s in &cd.series {
        if let Some(src) = s.name_ref.as_deref().and_then(ChartSource::parse_f_ref) {
            push(&src, ChartSlot::Name);
        }
    }
    out
}

/// EVERY source area covering cell `(r, c)`, largest first.
///
/// The slots nest by construction — a series' NAME cell is the header of the
/// column its VALUES read, and a row chart's categories sit inside its box — so
/// a cell is regularly claimed twice. The formula's references answer that with
/// `smallest_ref_at`: one cell, one colour, tightest claim wins. That is right
/// for a FILL and wrong for an OUTLINE, because the loser is a rectangle whose
/// side ran through this cell, and dropping it leaves that rectangle open. A
/// `values_ref` of `B1:B5` headed by a `name_ref` of `B1` — what the SERIES
/// VALUES field writes when you include the header — would draw with no top
/// edge at all.
///
/// So the renderer draws all of them and the order carries the rule instead:
/// largest first means the tightest claim paints LAST and wins any edge two
/// areas share, which is the same cell going to the same slot as before.
fn chart_areas_at(areas: &[ChartSourceArea], r: u32, c: u32) -> Vec<usize> {
    // `range_covers` and `range_cells` are `smallest_ref_at`'s own, so "covers"
    // and "smaller" cannot come to mean two different things here; only the
    // order differs, and deliberately.
    let mut hit: Vec<usize> = (0..areas.len())
        .filter(|&i| range_covers(areas[i].range, r, c))
        .collect();
    // Reversed on both keys: the last painted is the smallest, and the earliest
    // index among equals — `smallest_ref_at`'s winner, expressed as paint order.
    hit.sort_by_key(|&i| {
        (
            std::cmp::Reverse(range_cells(areas[i].range)),
            std::cmp::Reverse(i),
        )
    });
    hit
}

/// What a press — or a grid navigation key — is aimed at, for the "one
/// selection at a time" rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SelectTarget {
    /// A press on a grid cell.
    Cell,
    /// A press on the idx-th chart card of the active sheet — its body or one
    /// of its resize grips alike, because a resize is still a press on the
    /// chart it grips, and Excel selects a chart on mouse-DOWN.
    Chart(usize),
    /// Anything that reads or writes the cell selection without being a press
    /// on a grid object: the navigation keys and Tab, `F2` and any printable
    /// character, the `Ctrl` commands that act on cells, Find Next / Replace,
    /// every ribbon command `act_targets_cells` accepts, and a click on the
    /// formula bar. It moves or acts on the cell selection, so it is aimed at
    /// the cells even though nothing on the GRID was clicked — which is what
    /// separates it from `Cell` and `Chart` above.
    NavKey,
}

/// What is selected once that press or key has been handled.
///
/// Two things could claim to be selected — a chart card and a cell — and the
/// whole point of this rule is that only ever one does, so that "what will the
/// keyboard act on" has a visible answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SelectionAfter {
    /// The chart selected afterwards; `None` means the grid has it.
    chart: Option<usize>,
    /// The cell selection moves to what was pressed. False while pointing (the
    /// click writes a reference into a field and the selection stays put) and
    /// false for a press on a chart.
    cell_moves: bool,
    /// Drop the panel's focused field, its message and any pick in flight. They
    /// are all keyed by series position within the SELECTED chart, so they mean
    /// something else — or nothing — the moment that chart changes.
    drop_field: bool,
}

/// Whether the grid draws its cell selection at all: the ring, the range wash,
/// the headers' highlight and the auto-fill handle.
///
/// It is deliberately NOT a field of `SelectionAfter`, because it is not a
/// fourth decision — a chart being selected IS the cell selection being hidden,
/// whether the chart was just pressed or has been selected all along. The
/// render pass asks this every frame with no press in sight, and the answer has
/// to be the same one a press produced. The cells keep their selection; they
/// merely stop showing it until the chart is dismissed.
fn cell_selection_shown(chart: Option<usize>) -> bool {
    chart.is_none()
}

/// What just happened to the Chart panel, as the panel itself sees it.
///
/// The panel used to be gated straight on `chart_sel`, so a click anywhere on
/// the grid closed it — including the click that was meant to point one of its
/// own range fields at a cell. Under the sticky rule the panel has its own
/// state, and these are the only four things that move it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PanelEvent {
    /// A chart card was pressed: the panel shows that chart, swapping off
    /// whichever one it was showing.
    Select(usize),
    /// The chart lost the selection to the grid (a click on a cell, a
    /// navigation key). The panel is STICKY: it keeps showing that chart, so a
    /// range edit survives a click on the cells it is being pointed at.
    Deselect,
    /// The panel's `\u{00d7}`, or Escape. The two deliberate ways out, and the
    /// only ones that a user performs on purpose.
    Dismiss,
    /// The chart list underneath changed — a delete, a sheet switch, a tab
    /// switch, an undo. Every chart-keyed index now means something else, so
    /// the panel cannot keep showing "chart 2" and must close outright.
    Invalidate,
}

/// Which chart the Chart panel shows after `ev`, given the one it shows now.
/// `None` is the panel closed; there is no separate open flag, because "open"
/// and "has a chart to show" are the same question and two fields could
/// disagree about it.
fn chart_panel_after(shown: Option<usize>, ev: PanelEvent) -> Option<usize> {
    match ev {
        PanelEvent::Select(i) => Some(i),
        // The whole point: deselection does NOT close it.
        PanelEvent::Deselect => shown,
        PanelEvent::Dismiss | PanelEvent::Invalidate => None,
    }
}

/// The chart the panel actually renders, filtered against the sheet in front of
/// you: an index past the end of that sheet's chart list names nothing, so the
/// panel closes rather than rendering an empty shell that still eats
/// `SIDE_PANEL_W` of grid.
///
/// `Invalidate` is supposed to have caught every way the list can change, and
/// this is the second line of defence for when it hasn't — the checkbox asks
/// that the panel "must not display a chart that no longer exists", and a rule
/// that holds by construction beats one that holds if every mutation site
/// remembered to fire an event.
fn chart_panel_shown(shown: Option<usize>, n_charts: usize) -> Option<usize> {
    shown.filter(|&i| i < n_charts)
}

/// Does the live range field survive a press on chart `pressed`?
///
/// `press_selection` says a press on a chart that is not the selected one drops
/// the field, because the field belongs to the chart being left. Under the
/// sticky-panel rule that question has to be asked of the PANEL's chart instead:
/// click a cell and the chart deselects while its panel stays open, so pressing
/// that same card is `chart_sel == None` and would drop a field that never
/// changed owner.
///
/// The panel is not the only thing that can hold a live field, though.
/// `run_sheet_act` hands the chart back WITHOUT shutting the panel, so a bar
/// field — Data Validation, Sort, Text to Columns — can be pointing at the grid
/// with a chart panel open behind it. That field belongs to the sheet. Letting
/// it keep the keyboard while the card takes the selection is exactly the
/// two-things-selected split this plan set out to remove, so only the panel's
/// own field is kept.
fn keeps_panel_field(panel_chart: Option<usize>, pressed: usize, field: Option<RefTarget>) -> bool {
    panel_chart == Some(pressed) && !field.is_some_and(RefTarget::is_bar)
}

/// Which of the two selections a press or navigation key leaves selected.
///
/// - A press on a **cell** takes the selection back from any chart: its handles
///   and its source outlines go, and the cell ring returns.
/// - A press on a **chart** takes it the other way, and the same chart again is
///   a no-op rather than a re-selection — otherwise every press on a selected
///   card would drop the panel field you were about to type in.
/// - While **pointing** (a range field or a half-typed formula has the
///   keyboard) a press on a cell writes a reference and changes nothing about
///   what is selected. That is the whole reason the chart panel's range fields
///   work: the chart being edited must survive the clicks that edit it.
///   Pressing another CHART still swaps, pointing or not — the field belongs
///   to the chart being left.
fn press_selection(target: SelectTarget, chart: Option<usize>, pointing: bool) -> SelectionAfter {
    match target {
        SelectTarget::Chart(idx) if chart == Some(idx) => SelectionAfter {
            chart,
            cell_moves: false,
            drop_field: false,
        },
        SelectTarget::Chart(idx) => SelectionAfter {
            chart: Some(idx),
            cell_moves: false,
            drop_field: true,
        },
        _ if pointing => SelectionAfter {
            chart,
            cell_moves: false,
            drop_field: false,
        },
        SelectTarget::Cell | SelectTarget::NavKey => SelectionAfter {
            chart: None,
            cell_moves: true,
            // The grid has the keyboard now, so a panel field cannot keep it.
            drop_field: true,
        },
    }
}

/// A range as BARE A1 text — `B2:B5`, no `=`, no anchors, no sheet.
///
/// This is the form for a reference written into a CELL, where naming the sheet
/// in front of you is noise Excel doesn't write either: the formula-bar pick
/// (`range_text`) and the readout while that drag is in progress. Every range
/// FIELD shows `ref_a1` instead — the qualified, anchored form Excel's own
/// dialogs show — so a reference can be copied between the two apps. When in
/// doubt it's `ref_a1`: this one names no sheet, so a field holding it would
/// lose a qualifier the moment it was re-shown.
fn range_a1((r1, c1, r2, c2): (u32, u32, u32, u32)) -> String {
    use gridcore::sheet::cell_name;
    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
}

/// A reference as a range field shows it, the way Excel writes one:
/// `=Budget!$A$1:$D$5`, or `=$A$1:$D$5` when it names no sheet.
///
/// The quoting rules for the sheet name are the writer's, reached through
/// `ChartSource::to_ref` so a name needing quotes (`'My Sheet'`, `'Bob''s
/// Data'`) is spelled here exactly as it is spelled in the saved `<c:f>`. The
/// only difference between the two is the leading `=`, which the field shows
/// and the XML doesn't. `parse_ref_text` reads back everything this writes.
fn ref_a1(sheet: Option<&str>, range: (u32, u32, u32, u32)) -> String {
    let src = gridcore::sheet::ChartSource {
        sheet: sheet.unwrap_or_default().to_string(),
        range,
        cat_col: range.1,
    };
    format!("={}", src.to_ref())
}

/// A source the model holds, as the field showing it reads it. An empty sheet
/// name is a source naming none — `ref_a1`'s `None` — which is what a chart
/// authored before its refs carried a sheet still has.
fn source_ref_text(src: &gridcore::sheet::ChartSource) -> String {
    ref_a1(
        Some(src.sheet.as_str()).filter(|n| !n.is_empty()),
        src.range,
    )
}

/// What a series' NAME field shows: the reference the name came from, in the
/// same qualified form every other range field uses, or the literal name when
/// it came from no reference. Excel's Series name box reads the same way — it
/// holds `=Budget!$B$1` and shows the resolved `Q1` beside it, not in it.
///
/// `series_apply_name` measures "did you change it?" against THIS, not against
/// the bare name: that question is only meaningful against the text the field
/// actually displayed.
fn series_name_shown(name: &str, name_ref: Option<&str>) -> String {
    match name_ref.and_then(gridcore::sheet::ChartSource::parse_f_ref) {
        Some(src) => source_ref_text(&src),
        None => name.to_string(),
    }
}

/// The rectangle a selection covers, whichever corner it was dragged from.
fn sel_range(sel: (u32, u32), anchor: (u32, u32)) -> (u32, u32, u32, u32) {
    let ((ar, ac), (br, bc)) = (sel, anchor);
    (ar.min(br), ac.min(bc), ar.max(br), ac.max(bc))
}

/// What a bar's range field makes of what was typed: the cells as `ref_a1`
/// spells them — qualified with `sheet` and anchored — or the complaint to show
/// under the field. What the bar echoes is what the bar would accept again.
///
/// A `Sheet!` prefix is only accepted when it names `sheet`. A chart resolves
/// one (it can plot a sheet it doesn't float over), but a rule, a split or a
/// sort acts on the ACTIVE sheet: taking `Sheet2!` there would apply it to this
/// sheet's cells of the same name, and the message would agree.
fn bar_range_text(text: &str, sheet: &str) -> Result<String, String> {
    let t = text.trim();
    // The field SEEDS itself with `=Sheet1!$A$1:$D$5`, so the `=` comes off
    // before the qualifier is read — otherwise the bar refuses its own untouched
    // text, complaining about a sheet called `=Sheet1`.
    let t = t.strip_prefix('=').unwrap_or(t).trim();
    if let Some((prefix, _)) = t.rsplit_once('!') {
        if let Some(named) = unquote_sheet_name(prefix) {
            if !named.eq_ignore_ascii_case(sheet) {
                return Err(format!(
                    "\"{named}\" is another sheet; this acts on {sheet}"
                ));
            }
        }
    }
    match parse_ref_text(t) {
        // Back in the field's own form, qualified with the sheet just checked:
        // what the bar echoes is what the bar would accept again.
        Some(r) => Ok(ref_a1(Some(sheet), r.range)),
        None => Err(not_a_range_msg(t, &ref_a1(Some(sheet), (0, 0, 4, 3)))),
    }
}

/// Does a range field for `target` read cells on a sheet other than the one in
/// front of you, when its reference names one?
///
/// The split is about what the field FEEDS, not about the field:
///
/// - The chart targets resolve. A chart floats over one sheet and plots
///   another's numbers all the time — that is the whole point of a `<c:f>`
///   carrying a sheet name.
/// - `Validation` resolves. A dropdown belongs wherever the cells being
///   validated are, and those are commonly on a different sheet from the one
///   the rule is built on (a lookup sheet holding the list, an entry sheet
///   holding the boxes).
/// - `CondFormat`, `Sort` and `TextToColumns` refuse. Each acts on the rows in
///   front of you: a rule paints these cells, a sort reorders these rows, a
///   split rewrites these columns. A qualifier naming elsewhere is a mistake,
///   and the existing message says so rather than acting on the same-named
///   cells here.
/// - `ChartTitle` isn't a range at all, so nothing resolves; it answers `false`
///   only because the question doesn't apply to it.
fn target_takes_foreign_sheet(target: RefTarget) -> bool {
    match target {
        RefTarget::ChartRange
        | RefTarget::SeriesName(_)
        | RefTarget::SeriesValues(_)
        | RefTarget::Categories
        | RefTarget::Validation => true,
        RefTarget::CondFormat
        | RefTarget::Sort
        | RefTarget::TextToColumns
        | RefTarget::ChartTitle => false,
    }
}

/// Which sheet an entry bar will act on and the text its field keeps, or the
/// complaint to show under it. `names` are the workbook's sheets and `active`
/// the one on screen, which is what an unqualified reference means.
///
/// A bar that refuses a foreign sheet (see `target_takes_foreign_sheet`) goes
/// on answering through `bar_range_text`, message and all. One that resolves
/// looks the name up and answers with the sheet ACTUALLY found, spelled the way
/// the workbook spells it — so `budget!a1:a9` comes back `=Budget!$A$1:$A$9`
/// and the field stops disagreeing with the tab it names.
fn bar_ref_text(
    text: &str,
    target: RefTarget,
    names: &[String],
    active: usize,
) -> Result<(usize, String), String> {
    let here = names.get(active).map_or("", String::as_str);
    if !target_takes_foreign_sheet(target) {
        return bar_range_text(text, here).map(|a1| (active, a1));
    }
    let Some(r) = parse_ref_text(text) else {
        // The same complaint the refusing bars give, so the two never read as
        // different kinds of failure.
        return Err(not_a_range_msg(text, &ref_a1(Some(here), (0, 0, 4, 3))));
    };
    let i = sheet_index_of(names, r.sheet.as_deref(), active)?;
    Ok((i, ref_a1(names.get(i).map(String::as_str), r.range)))
}

/// The rows a sort runs over: a field naming more than one row sorts exactly
/// those, anything else falls back to the region found around the cursor.
fn sort_rows_from(
    field: Option<(u32, u32, u32, u32)>,
    region: Option<(u32, u32)>,
) -> Option<(u32, u32)> {
    match field {
        Some((r0, _, r1, _)) if r1 > r0 => Some((r0, r1)),
        _ => region,
    }
}

/// The cells a range field's text points at ON THE SHEET IN FRONT OF YOU, or
/// `None` when it points somewhere else. `names` are the workbook's sheets and
/// `active` the one on screen.
///
/// A reference naming another sheet gets no wash: washing this sheet's A1:D5
/// for a ref that means Budget's A1:D5 would draw the very lie — same-named
/// cells standing in for the ones actually read — that keeping the qualifier
/// exists to remove. The field still holds the ref and the grid is still in
/// point mode, so a drag can re-point it at cells you can see.
///
/// The qualifier is RESOLVED (`sheet_index_of`) rather than matched against the
/// active sheet's name, so the wash answers for the sheet a commit will
/// actually act on. The two differ only on a workbook Excel forbids and this
/// code tolerates — two sheets whose names differ in case, where the lookup
/// takes the first — and there a name test would outline the active sheet's
/// cells for a ref that reads the other one's.
fn preview_range(text: &str, names: &[String], active: usize) -> Option<(u32, u32, u32, u32)> {
    let r = parse_ref_text(text)?;
    (sheet_index_of(names, r.sheet.as_deref(), active).ok()? == active).then_some(r.range)
}

/// The A1 text for a range dragged from `anchor` to `to`, in either direction.
/// A drag IS a selection, so it normalises and formats through the same two
/// functions the selection does rather than repeating them.
fn range_text(anchor: (u32, u32), to: (u32, u32)) -> String {
    range_a1(sel_range(to, anchor))
}

/// The text a drag writes into a range FIELD: the same rectangle `range_text`
/// reports, qualified with the sheet it was picked from — the form the field
/// keeps once the drag ends. The two have to agree, or the reference would
/// appear to change the instant the mouse came up.
///
/// `range_text` stays bare because the OTHER thing a drag writes into is a
/// cell's formula, where naming this very sheet is noise Excel doesn't write.
fn ref_pick_text(sheet: &str, anchor: (u32, u32), to: (u32, u32)) -> String {
    ref_a1(Some(sheet), sel_range(to, anchor))
}

/// One run of a Chart-panel field's text (`base_off` is its char offset in the
/// buffer). Pressing puts the caret under the pointer — extending the selection
/// on Shift, taking the whole field on a double click — and dragging over any
/// run extends it, so the three runs together behave like one selectable line.
fn ref_field_segment(
    s: String,
    base_off: usize,
    target: RefTarget,
    selected: bool,
    ent: &Entity<Docxy>,
) -> AnyElement {
    let styled = StyledText::new(SharedString::from(s.clone()));
    let layout = styled.layout().clone();
    let (ent_dn, ent_mv) = (ent.clone(), ent.clone());
    let (s_dn, l_dn) = (s.clone(), layout.clone());
    let at = move |pos, text: &str, layout: &gpui::TextLayout| {
        let byte = layout
            .index_for_position(pos)
            .unwrap_or_else(|e| e)
            .min(text.len());
        base_off + text[..byte].chars().count()
    };
    let at_mv = at;
    div()
        .child(styled)
        .when(selected, |d| {
            d.bg(Hsla {
                a: 0.30,
                ..hsla_u(BRAND)
            })
        })
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
            cx.stop_propagation();
            let idx = at(ev.position, &s_dn, &l_dn);
            let dbl = ev.click_count >= 2;
            ent_dn.update(cx, |this, cx| {
                if let Some(f) = &mut this.range_edit {
                    if f.target == target {
                        if dbl {
                            // Double click takes the whole field, so retyping a
                            // title doesn't mean backspacing over it.
                            f.anchor = 0;
                            f.caret = f.buf.chars().count();
                        } else {
                            f.set_caret(idx, ev.modifiers.shift);
                            f.dragging = true;
                        }
                    }
                }
                cx.notify();
            });
        })
        .on_mouse_move(move |ev, _window, cx| {
            if ev.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let idx = at_mv(ev.position, &s, &layout);
            ent_mv.update(cx, |this, cx| {
                if let Some(f) = &mut this.range_edit {
                    if f.target == target && f.dragging && f.caret != idx {
                        f.set_caret(idx, true);
                        cx.notify();
                    }
                }
            });
        })
        .into_any_element()
}

/// A Chart-panel field's content while it has the keyboard: the buffer cut into
/// before / selected / after, with the caret bar at whichever end it sits.
fn ref_field_row(f: &RangeEdit, ent: &Entity<Docxy>) -> AnyElement {
    let chars: Vec<char> = f.buf.chars().collect();
    let n = chars.len();
    let caret = f.caret.min(n);
    let (s0, s1) = f.selection().unwrap_or((caret, caret));
    let take = |a: usize, b: usize| -> String { chars[a.min(n)..b.min(n)].iter().collect() };
    let bar = || div().w(px(1.5)).h(px(13.)).bg(hsla_u(BRAND)).flex_none();
    h_flex()
        .flex_1()
        .items_center()
        .overflow_hidden()
        .child(ref_field_segment(take(0, s0), 0, f.target, false, ent))
        .when(caret == s0, |d| d.child(bar()))
        .child(ref_field_segment(take(s0, s1), s0, f.target, true, ent))
        .when(caret == s1 && s1 != s0, |d| d.child(bar()))
        .child(ref_field_segment(take(s1, n), s1, f.target, false, ent))
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
        reveal_col: None,
    })
}

fn sheet_from_path(path: &PathBuf) -> (Surface, SharedString) {
    match std::fs::read(path) {
        Ok(bytes) => match gridcore::xlsx::load_xlsx(&bytes) {
            Ok(pkg) => {
                let n = pkg.workbook.sheets.len();
                let engine = gridcore::engine::Engine::new(&pkg.workbook);
                let view = SheetView {
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
                    reveal_col: None,
                };
                (
                    Surface::Sheet(view),
                    format!("loaded — {n} sheet{}", if n == 1 { "" } else { "s" }).into(),
                )
            }
            Err(e) => (
                Surface::Placeholder,
                format!("xlsx load error: {e:?}").into(),
            ),
        },
        Err(e) => (Surface::Placeholder, format!("read error: {e}").into()),
    }
}

fn build_surface(
    kind: Kind,
    path: Option<&PathBuf>,
) -> (
    Surface,
    Vec<Comment>,
    Vec<docxcore::notes::Note>,
    Option<Package>,
    SharedString,
) {
    match kind {
        Kind::Docx => match path {
            Some(p) => {
                let l = doc_from_path(p);
                (
                    Surface::Doc(Editor::new(l.doc)),
                    l.comments,
                    l.notes,
                    l.pkg,
                    l.status,
                )
            }
            None => (
                Surface::Doc(Editor::new(empty_doc())),
                vec![],
                vec![],
                None,
                "untitled".into(),
            ),
        },
        Kind::Xlsx => match path {
            Some(p) => {
                let (surface, status) = sheet_from_path(p);
                (surface, vec![], vec![], None, status)
            }
            None => (
                new_sheet_surface(),
                vec![],
                vec![],
                None,
                "new spreadsheet".into(),
            ),
        },
        _ => (Surface::Placeholder, vec![], vec![], None, "".into()),
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled.docx".into())
}

/// Directory holding the hot-exit sidecars — one `.docx` per open Doc tab, kept in
/// sync on each persist so unsaved edits survive a restart.
fn hot_dir() -> PathBuf {
    config_root().join("docxy").join("hot")
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
            let has_list = doc
                .body
                .iter()
                .any(|b| matches!(b, Block::Paragraph(p) if p.props.num_id.is_some()));
            if has_list {
                docxcore::package::new_markdown_package(doc.clone())
            } else {
                docxcore::package::new_package(doc.clone())
            }
        }
    };
    // Reconcile comments.xml with the tab's comment list: the base already holds the
    // comments it was loaded with, so only remove the deleted ones and add the new.
    let existing: Vec<i32> = base
        .map(|p| {
            docxcore::comments::parse_comments(p)
                .iter()
                .filter_map(|c| c.id.parse().ok())
                .collect()
        })
        .unwrap_or_default();
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
                    l.status = if t.dirty {
                        "unsaved — restored".into()
                    } else {
                        "loaded".into()
                    };
                    l.into_tab(t.kind, t.title.clone().into(), path, t.dirty)
                }
                // Spreadsheet with unsaved content: load the hot .xlsx sidecar but
                // keep the original on-disk `path` (so Save still targets the real
                // file; a never-saved sheet keeps path=None → Save prompts Save As).
                (Kind::Xlsx, Some(hp)) => {
                    let (surface, _) = sheet_from_path(hp);
                    let status = if t.dirty {
                        "unsaved — restored"
                    } else {
                        "loaded"
                    };
                    DocTab {
                        kind: Kind::Xlsx,
                        title: t.title.clone().into(),
                        path,
                        surface,
                        dirty: t.dirty,
                        status: status.into(),
                        comments: vec![],
                        pkg: None,
                        notes: vec![],
                        markdown: false,
                        hf_edit: None,
                    }
                }
                _ => {
                    let (surface, comments, notes, pkg, status) =
                        build_surface(t.kind, path.as_ref());
                    let markdown = path.as_deref().map(is_markdown_path).unwrap_or(false);
                    DocTab {
                        kind: t.kind,
                        title: t.title.clone().into(),
                        path,
                        surface,
                        dirty: t.dirty,
                        status,
                        comments,
                        pkg,
                        notes,
                        markdown,
                        hf_edit: None,
                    }
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
    fn build(
        tabs: Vec<DocTab>,
        active: usize,
        theme_pref: ThemePref,
        ask_on_close: bool,
        cx: &mut Context<Self>,
    ) -> Self {
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
            chart_sel: None,
            panel_chart: None,
            chart_drag: None,
            range_edit: None,
            ref_msg: None,
            formula_pick: None,
            drag_anchor: None,
            range_pick: None,
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
            bar_field: None,
            bar_range: None,
            harness: None,
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
                        std::fs::write(&p, doc_to_docx(&ed.doc, &t.comments, t.pkg.as_ref()))
                            .ok()
                            .map(|_| p.display().to_string())
                    }
                    Surface::Sheet(v) => {
                        let p = hd.join(format!("tab-{i}.xlsx"));
                        std::fs::write(&p, sheet_bytes(v))
                            .ok()
                            .map(|_| p.display().to_string())
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
        let session = Session {
            tabs,
            active: self.active,
            theme: self.theme_pref,
            ask_on_close: self.ask_on_close,
        };
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
            Kind::Docx => (
                "Untitled.docx".into(),
                Surface::Doc(Editor::new(empty_doc())),
            ),
            Kind::Xlsx => ("Untitled.xlsx".into(), new_sheet_surface()),
            Kind::Look => ("Inbox".into(), Surface::Placeholder),
        };
        self.tabs.push(DocTab {
            kind,
            title,
            path: None,
            surface,
            dirty: false,
            status: "new".into(),
            comments: vec![],
            pkg: None,
            notes: vec![],
            markdown: false,
            hf_edit: None,
        });
        self.active = self.tabs.len() - 1;
        self.backstage = false;
        self.bs_new = false;
        self.drop_grid_state();
        self.persist();
        self.refocus(window, cx);
    }

    /// Move the spreadsheet selection to a cell (from a grid click), collapsing
    /// the range and committing any in-progress edit first.
    fn select_cell(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        // One selection at a time, decided in one place: while a range field or
        // a half-typed formula is POINTING, this click writes a reference and
        // nothing is selected or deselected — which is exactly why a chart
        // panel's range fields can be pointed at the grid at all. Otherwise the
        // grid takes the selection back from whatever chart was holding it.
        let after = press_selection(
            SelectTarget::Cell,
            self.chart_sel,
            self.formula_pick_active() || self.range_field_active(),
        );
        if !after.cell_moves {
            // Typing a formula: a click writes the cell in rather than
            // committing the edit and moving away.
            if self.formula_pick_active() {
                return self.formula_pick_to(row, col, true, cx);
            }
            // Mid-drag: the pick already has this cell, and its release applies.
            if matches!(self.range_pick, Some((_, true))) {
                return;
            }
            // A plain click writes the clicked cell into the field and keeps it
            // focused — Enter commits. Ending point mode here instead would make
            // the interaction depend on whether the pointer happened to twitch
            // between press and release: a one-pixel move inside the same cell
            // goes through `sheet_drag_over` and picks, a perfectly still click
            // would not. Escape is the way out of the field.
            return self.range_pick_to(row, col, true, cx);
        }
        if self.active_sheet().is_some_and(|v| v.editing.is_some()) {
            self.sheet_commit(0, 0, cx); // commit in place before moving away
        }
        // The chart drops its handles and its source outlines; the cell ring
        // comes back out. The PANEL stays open on it — that is the sticky rule,
        // and the reason this click cannot interrupt a range edit.
        self.chart_sel = after.chart;
        self.chart_panel_event(PanelEvent::Deselect);
        if after.drop_field {
            self.range_edit = None;
            self.ref_msg = None;
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
            self.sheet_fill = Some(FillDrag {
                src,
                to: (src.2, src.3),
            });
        }
        cx.notify();
    }

    /// Update the auto-fill target as the handle is dragged. Nothing is
    /// committed here — neither the cells nor the selection move until the
    /// button comes up; the drag only outlines the box it would fill.
    fn sheet_fill_over(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        let Some(mut f) = self.sheet_fill else { return };
        if f.to == (row, col) {
            return;
        }
        f.to = (row, col);
        self.sheet_fill = Some(f);
        cx.notify();
    }

    /// Chart selection + in-progress drag, for the overlay renderer.
    fn chart_ui(&self) -> ChartUi {
        ChartUi {
            sel: self.chart_sel,
            drag: self
                .chart_drag
                .map(|d| (d.idx, d.delta.0, d.delta.1, d.edge)),
        }
    }

    /// Press on a chart card: select it (Excel selects on mouse-down, not on
    /// click) and arm a drag from where the pointer went down. `edge` is
    /// `(0, 0)` for the card itself (a move) or the side a resize grip owns —
    /// a grip's press comes through here too, so a resize selects the chart it
    /// grips rather than the cells underneath.
    ///
    /// Selecting the chart takes the selection AWAY from the grid: the cell
    /// ring, the range wash and the headers' highlight are not drawn while a
    /// chart owns it (`GridOverlay::sel_hidden`), so the two never compete over
    /// which one the keyboard will act on.
    fn chart_press(&mut self, idx: usize, edge: (i8, i8), at: (f32, f32), cx: &mut Context<Self>) {
        // The panel's focused field is keyed by series position within the
        // SELECTED chart, so it means something else on another one: it would
        // point the new chart's series at the old one's buffer, or stay focused
        // (and swallow the keyboard) on a series the new chart doesn't have.
        // Pointing is passed through rather than assumed: a press on a chart
        // selects it whether or not a field has the keyboard, and `press_selection`
        // is where that is written down once.
        let after = press_selection(
            SelectTarget::Chart(idx),
            self.chart_sel,
            self.formula_pick_active() || self.range_field_active(),
        );
        // Asked of the PANEL's chart, not of `chart_sel`. Under the sticky rule
        // those two diverge: click a cell and the chart is deselected while its
        // panel stays open, so pressing that same card again is `chart_sel ==
        // None` and misses `press_selection`'s same-chart no-op. The field being
        // typed in belongs to the chart the panel SHOWS, and re-selecting that
        // chart is not a change of chart — throwing the edit away there is the
        // mid-edit loss the sticky panel was added to stop. `keeps_panel_field`
        // states the whole rule, including the bar fields that are not the
        // panel's to keep.
        let keep = keeps_panel_field(
            self.panel_chart,
            idx,
            self.range_edit.as_ref().map(|f| f.target),
        );
        if after.drop_field && !keep {
            self.range_edit = None;
            self.ref_msg = None;
            self.range_pick = None;
        }
        // A half-typed cell would otherwise keep its caret and its white box
        // while the card draws its handles — two things selected, and the
        // keyboard going to the one the chart is hiding. `select_cell` commits
        // in place for the same reason; a press on a card is as much a press
        // away from the cell as a press on another cell is.
        if self.active_sheet().is_some_and(|v| v.editing.is_some()) {
            self.sheet_commit(0, 0, cx);
        }
        self.chart_sel = after.chart;
        // The panel swaps to it, opening if it was shut.
        self.chart_panel_event(PanelEvent::Select(idx));
        self.chart_drag = Some(ChartDrag {
            idx,
            edge,
            origin: at,
            delta: (0.0, 0.0),
        });
        cx.notify();
    }

    /// Put a message on the active tab's status line.
    fn set_status(&mut self, msg: impl Into<SharedString>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.status = msg.into();
        }
    }

    /// The left button came up: end every grid drag that could be in flight and
    /// commit what it did. Every step takes its state, so this is idempotent and
    /// can be called from wherever the release lands — the grid, the panel that
    /// swallows the event, or the window root when the pointer left both. Left
    /// only on the grid, a release over the ribbon would leave (say) `sheet_fill`
    /// armed, and the NEXT drag anywhere would commit an auto-fill nobody asked
    /// for.
    fn grid_release(&mut self, cx: &mut Context<Self>) {
        self.col_resize_end(cx);
        self.sheet_dragging = false; // end any drag-select
        self.drag_anchor = None;
        self.sheet_fill_end(cx); // commit an auto-fill drag, if any
        self.chart_drag_end(cx); // commit a chart move, if any
        self.range_pick_end(cx); // replot a range picked off the grid
        self.formula_pick = None; // the reference stays; the drag is over
        // A text drag inside a range field can be released anywhere too. Only
        // the Chart panel had its own mouse-up, so a drag in one of the entry
        // bars' fields left this armed and the next hover over that text kept
        // extending the selection with no button down.
        if let Some(f) = &mut self.range_edit {
            f.dragging = false;
        }
    }

    /// Drop everything keyed to the chart list: the selection itself, a drag in
    /// flight, the panel's focused field and its message. All of them are bare
    /// indices into ONE sheet's charts, so they mean something else the moment
    /// that list changes underneath them (a sheet switch, a tab switch, an undo).
    fn chart_drop_selection(&mut self) {
        self.chart_sel = None;
        // The panel is sticky against DESELECTION, not against the list moving
        // under it: an index into a list that just changed names a different
        // chart, so the panel closes here rather than silently swapping to
        // whichever one slid into the gap.
        self.chart_panel_event(PanelEvent::Invalidate);
        self.chart_drag = None;
        self.range_edit = None;
        self.ref_msg = None;
        self.range_pick = None;
    }

    /// Move the Chart panel's state. The decision is `chart_panel_after`; this
    /// is only the field it is written to, so every caller expresses what
    /// happened rather than what the panel should now be.
    fn chart_panel_event(&mut self, ev: PanelEvent) {
        self.panel_chart = chart_panel_after(self.panel_chart, ev);
    }

    /// Hand the cell selection back to the grid, the way a click on a cell
    /// does. Every key the GRID acts on goes through here first.
    ///
    /// While a chart is selected the cell selection is hidden (`sel_hidden`),
    /// so anything that reads or writes it — moving it, typing into it, pasting
    /// over it, selecting all of it — would be a change with nothing on screen
    /// to show for it. That is the invisible-motion confusion the "one
    /// selection at a time" rule exists to end, and it is not particular to the
    /// arrow keys. The chart's own keys (Escape, Delete) are the exceptions and
    /// never call this; so are the ones aimed at the document or the window
    /// rather than the selection (Ctrl+S, Ctrl+F, Ctrl+F1).
    fn chart_hand_back(&mut self, cx: &mut Context<Self>) {
        if self.chart_sel.is_none() {
            return;
        }
        // `pointing` is false here, unlike at the press sites. Point mode is a
        // property of a POINTER gesture — a click on a cell writes a reference
        // into the focused field instead of moving the selection — and no key
        // that reaches this function is one. A Ctrl+X arriving while a range
        // field has focus is still a cut, and if the chart kept the selection
        // it would be a cut of cells `sel_hidden` is not drawing: the exact
        // invisible write this function exists to prevent.
        let after = press_selection(SelectTarget::NavKey, self.chart_sel, false);
        self.chart_sel = after.chart;
        self.chart_panel_event(PanelEvent::Deselect);
        if after.drop_field {
            // The grid has the keyboard now, so a panel field cannot keep it —
            // the whole of `drop_field`, not just the message. Leaving
            // `range_edit` standing would hand the ring back to the cells and
            // then feed every following keystroke to the field anyway.
            self.range_edit = None;
            self.range_pick = None;
            self.ref_msg = None;
        }
        // The handles and the source outlines have just gone and the ring has
        // just come back. Several callers return without repainting — a paste
        // into a protected sheet, a copy with nothing to copy — so the repaint
        // is owed here rather than to whatever runs next.
        cx.notify();
    }

    /// How many charts the active sheet has, in `chart_locate`'s order. The
    /// panel's index is checked against this, so it can never render a chart
    /// that has gone.
    fn chart_count(&self) -> usize {
        let Some(v) = self.active_sheet() else {
            return 0;
        };
        let sidx = v.active;
        let ui = v.charts.iter().filter(|c| c.sheet == sidx).count();
        // `sheet()` rather than `sheets[sidx]`: the render pass asks this every
        // frame now (via `panel_chart_shown`), and `SheetView` treats an
        // out-of-range `active` as possible everywhere else it indexes.
        let dw = v
            .sheet()
            .drawings
            .iter()
            .filter(|dw| matches!(dw.kind, gridcore::sheet::DrawingKind::Chart(_)))
            .count();
        ui + dw
    }

    /// The chart the Chart panel renders, or `None` when the panel is shut.
    /// This is the ONE answer both the render gate and the grid-width
    /// reservation ask, so the panel can never be drawn in a slot the grid also
    /// laid itself out over.
    fn panel_chart_shown(&self) -> Option<usize> {
        chart_panel_shown(self.panel_chart, self.chart_count())
    }

    /// Everything the UI holds that points INTO one grid: the chart selection
    /// and panel field, the open entry bar, a fill drag, a formula's pick. The
    /// moment the grid under them changes — another sheet, another tab, a sheet
    /// added or deleted — they name something else, so every such transition
    /// goes through here.
    fn drop_grid_state(&mut self) {
        self.chart_drop_selection();
        self.bar_close();
        self.sheet_fill = None;
        self.formula_pick = None;
    }

    /// Where the idx-th chart of the active sheet lives. The overlay lays the
    /// cards out in this order too: authored charts first, then the drawings.
    fn chart_locate(&self, idx: usize) -> Option<ChartRef> {
        let v = self.active_sheet()?;
        let sidx = v.active;
        let ui: Vec<usize> = v
            .charts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.sheet == sidx)
            .map(|(i, _)| i)
            .collect();
        if let Some(&pos) = ui.get(idx) {
            return Some(ChartRef::Ui(pos));
        }
        v.pkg.workbook.sheets[sidx]
            .drawings
            .iter()
            .enumerate()
            .filter(|(_, dw)| matches!(dw.kind, gridcore::sheet::DrawingKind::Chart(_)))
            .map(|(i, _)| i)
            .nth(idx - ui.len())
            .map(ChartRef::Drawing)
    }

    /// The idx-th chart's data, in `chart_locate`'s order.
    fn chart_data_at(&self, idx: usize) -> Option<gridcore::sheet::ChartData> {
        let v = self.active_sheet()?;
        match self.chart_locate(idx)? {
            ChartRef::Ui(i) => Some(v.charts[i].data.clone()),
            ChartRef::Drawing(i) => match &v.pkg.workbook.sheets[v.active].drawings[i].kind {
                gridcore::sheet::DrawingKind::Chart(cd) => Some(cd.clone()),
                _ => None,
            },
        }
    }

    /// The data the Chart panel is editing — the chart it SHOWS, not the one
    /// selected. Under the sticky rule those differ: a click on a cell drops
    /// the selection and leaves the panel open, and every field in it must go
    /// on reading and writing the chart whose name is at the top of it.
    ///
    /// The grid's source outlines ask `chart_sel` instead (`chart_refs`), which
    /// is the whole distinction: what is SELECTED is drawn on the cells, what
    /// is SHOWN is edited in the panel.
    fn chart_data(&self) -> Option<gridcore::sheet::ChartData> {
        self.chart_data_at(self.panel_chart_shown()?)
    }

    /// Write the panel's chart data back, marking it edited so a save
    /// regenerates its chart part. The panel's chart for the same reason
    /// `chart_data` reads it: a commit from a field has to land on the chart
    /// that field belongs to, selected or not.
    fn chart_set_data(&mut self, mut data: gridcore::sheet::ChartData, cx: &mut Context<Self>) {
        let Some(sel) = self.panel_chart_shown() else {
            return;
        };
        let Some(loc) = self.chart_locate(sel) else {
            return;
        };
        data.edited = true;
        // Committing a field without having changed anything (Enter on the
        // seeded title, say) must not mark the chart edited: an edited chart is
        // REGENERATED from this model on save, losing every bit of its part we
        // don't model — gradients, data labels, trendlines.
        if self.chart_data().is_some_and(|mut cur| {
            cur.edited = true;
            cur == data
        }) {
            return;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let sidx = v.active;
            match loc {
                ChartRef::Ui(i) => v.charts[i].data = data,
                ChartRef::Drawing(i) => {
                    v.pkg.workbook.sheets[sidx].drawings[i].kind =
                        gridcore::sheet::DrawingKind::Chart(data)
                }
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Re-point the panel's chart — the one it SHOWS, `chart_data`'s, which is
    /// not always the selected one — at another range, replotting its
    /// categories and series from those cells while keeping its type, title and
    /// colours.
    /// The reference names the sheet it reads — a chart floating over one sheet
    /// can plot another sheet's numbers — and an unqualified one means the
    /// sheet in front of you.
    fn chart_apply_range(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(old) = self.chart_data() else { return };
        let example = self.ref_example((0, 0, 4, 3));
        let (si, range) = match self.chart_ref(text, &example) {
            Ok(r) => r,
            Err(m) => {
                self.ref_msg = Some((RefTarget::ChartRange, false, m));
                cx.notify();
                return;
            }
        };
        // `chart_from_range` is handed the RESOLVED sheet and stamps its name on
        // every ref it builds, so a chart re-pointed at another sheet saves as a
        // `<c:f>` naming that sheet.
        let Some(mut data) = self.active_sheet().and_then(|v| {
            let sh = v.pkg.workbook.sheets.get(si)?;
            // Re-pointing keeps the chart reading the way it already does.
            gridcore::sheet::chart_from_range(sh, &sh.name, range, &old.kind, old.by_row)
        }) else {
            let (r1, c1, r2, c2) = range;
            // Named the way round THIS chart reads: a row chart wants a row of
            // numbers beside a label column, and telling its user to go and
            // find a column would send them after the wrong shape.
            let (line, edge) = if old.by_row {
                ("row", "beside a label column")
            } else {
                ("column", "under a header row")
            };
            self.ref_msg = Some((
                RefTarget::ChartRange,
                false,
                format!(
                    "{}:{} has no {line} of numbers {edge}",
                    gridcore::sheet::cell_name(r1, c1),
                    gridcore::sheet::cell_name(r2, c2)
                ),
            ));
            cx.notify();
            return;
        };
        // Nothing is asked about the series COUNT. Re-pointing a pie at a box
        // several numeric columns wide used to be refused right here, because
        // `chart_space_xml` then wrote the first series and dropped the rest on
        // save; it writes every one of them now, so the widened plot is kept
        // whole and the panel says which of it the pie draws
        // (`chart_plotted_series`, `chart_unplotted_note`).
        // The new plot keeps the look of the old one.
        data.title = old.title.clone();
        data.part = old.part.clone();
        // Re-pointing is not "author this one afresh" — only picking a type is
        // (see `chart_set_kind`). `chart_from_range` always says `complex:
        // false`, and taking that would let the writer regenerate a stacked or
        // combo plot area as a plain clustered one.
        data.complex = old.complex;
        for (i, s) in data.series.iter_mut().enumerate() {
            s.color = old.series.get(i).and_then(|o| o.color);
        }
        let msg = format!(
            "Plotting {} series over {} categories",
            data.series.len(),
            data.categories.len()
        );
        // What it plots is the RESOLVED reference, not the text: uppercasing what
        // was typed would shout a sheet name the workbook hasn't got ("Chart
        // plots ='MY SHEET'!$A$1:$D$5" for a sheet called `My Sheet`), and a
        // qualifier resolves case-insensitively, so only the canonical spelling
        // is true. The `=` comes off because this reads as a sentence.
        let said = ref_a1(self.sheet_names().get(si).map(String::as_str), range);
        self.set_status(format!("Chart plots {}", said.trim_start_matches('=')));
        self.ref_msg = Some((RefTarget::ChartRange, true, msg));
        self.chart_set_data(data, cx);
    }

    /// The sheet a chart's own box NAMES, resolved — the sheet to re-derive
    /// that chart from — or WHY it can't be re-derived at all.
    ///
    /// The box's sheet name is what gets resolved, not the sheet on screen: a
    /// chart floating over one sheet can plot another's numbers, and re-deriving
    /// from the wrong one would quietly replot it against the cells underneath
    /// it. An EMPTY name is an unqualified box, which means the sheet on screen,
    /// so it is passed as `None` rather than looked up and missed.
    ///
    /// The cell cap is `chart_ref_of`'s, for `chart_ref_of`'s reason and then
    /// some: this range did not come from a field, it came from the FILE, so it
    /// is the one chart range nothing has ever bounded — a `<c:f>` may legally
    /// name `$A$1:$A$1048576`. Re-deriving that would walk a million cells and
    /// build a series per row, and `chart_switched` asks it on every frame the
    /// panel draws, not once per click.
    ///
    /// The three sentences are the ones the Switch Row/Column note has always
    /// printed, kept word for word: each names the CHART's problem rather than
    /// that of whichever door asked, so `chart_set_kind` prints them too.
    ///
    /// The one-line wrapper `range-selector.md` asks for: the decision it
    /// delegates to (`sheet_index_of`) is the pure, tested part.
    fn chart_range_sheet(
        &self,
        data: &gridcore::sheet::ChartData,
    ) -> Result<&gridcore::sheet::Sheet, String> {
        let Some(src) = data.source.as_ref() else {
            return Err(CHART_NO_BOX.to_string());
        };
        chart_cells_within_cap(src.range)?;
        let Some(v) = self.active_sheet() else {
            return Err("No workbook open.".to_string());
        };
        let named = (!src.sheet.is_empty()).then_some(src.sheet.as_str());
        sheet_index_of(&self.sheet_names(), named, v.active)
            .ok()
            .and_then(|si| v.pkg.workbook.sheets.get(si))
            .ok_or_else(|| {
                format!(
                    "Its range names a sheet this workbook hasn't got ({}).",
                    src.sheet
                )
            })
    }

    /// The panel's chart (`chart_data`'s, not `chart_sel`'s) re-derived from its
    /// own range the other way round, or WHY it can't be — which is both what greys the Switch Row/Column button
    /// out and the note printed under it, so the button is enabled exactly when
    /// clicking it would do something and the reason given is the real one. The
    /// two used to be worked out separately, and the note then blamed the range
    /// for a sheet name the workbook had lost.
    ///
    /// The sheet resolution and the cell cap are [`Self::chart_range_sheet`]'s,
    /// shared with the other door that re-derives a chart from its own box.
    fn chart_switched(&self) -> Result<gridcore::sheet::ChartData, String> {
        let Some(data) = self.chart_data() else {
            return Err("No chart selected.".to_string());
        };
        let sh = self.chart_range_sheet(&data)?;
        // The flip keeps `kind`, and a pie read the other way round is usually
        // one one-point series per category — which used to be refused here,
        // because the writer then dropped all but the first on save. It writes
        // every one of them now, so the flip is allowed and the panel says
        // which of the series the plot draws (`chart_unplotted_note`).
        chart_switch_row_column(&data, sh)
    }

    /// Excel's `Switch Row/Column`: read the chart's range the other way round,
    /// so each row becomes a series instead of each column.
    ///
    /// `data` is the flip the panel ALREADY derived to decide whether to enable
    /// the button, handed back rather than worked out a second time — the
    /// button and the click then cannot disagree about what clicking does.
    ///
    /// The undo snapshot is `chart_set_data`'s — it takes one before writing the
    /// chart back, and a flip always differs from what is there (the
    /// orientation, if nothing else), so its "committed nothing" early return
    /// can't swallow this.
    fn chart_switch_orientation(
        &mut self,
        data: gridcore::sheet::ChartData,
        cx: &mut Context<Self>,
    ) {
        self.set_status(format!(
            "Chart reads each {} as a series \u{2014} {} series over {} categories",
            if data.by_row { "row" } else { "column" },
            data.series.len(),
            data.categories.len()
        ));
        // A flip doesn't just reorder the series, it replaces them — three
        // columns become two rows, and card 3 now means something else
        // entirely. Both of these are keyed by bare series index, so an open
        // field would commit its buffer onto whichever series inherited the
        // number, the same hazard `series_delete` clears them for.
        self.range_edit = None;
        self.ref_msg = None;
        self.chart_set_data(data, cx);
    }

    /// Switch the panel's chart (`chart_data`'s) between column / bar / line /
    /// pie.
    fn chart_set_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(data) = self.chart_data() else {
            return;
        };
        if data.kind == kind && !data.complex {
            return;
        }
        // Picking **Pie** on a chart that already has several series used to be
        // refused right here, this being the widest door to a multi-series pie:
        // one click on a chart whose series the user has already pointed and
        // coloured. It is allowed now — `chart_space_xml` keeps every series a
        // pie holds, so the click loses nothing, and picking the old kind back
        // returns the chart intact.
        //
        // Picking a WRITABLE type hands the part to `chart_space_xml` on the
        // next save, and it writes each series from the numbers the model holds
        // — a `values_ref`, a `col` inside the box, or a cached snapshot. A
        // scatter's or bubble's series has none of the three
        // (`chart_would_lose_points`), so relabelling one `column` would
        // overwrite its part with a series of no points: the plot the user was
        // looking at, gone in one click and not recoverable by undoing the type.
        // `chart_kind_is_writable`'s own comment calls that outcome
        // irreversible; this is the door that would reach it.
        //
        // So such a chart is AUTHORED AFRESH from the box it is showing, which
        // is what the panel's note already promises picking a type does, and
        // what `chart_take_kind` leaving the box alone assumes the user goes on
        // to do by hand. Done here, at the click, the conversion cannot be saved
        // half-finished — and no chart is ever left with its points cleared and
        // nothing but a name cell for `rebuild_source` to fold.
        // The kind the chart HAD, for the status line below: `chart_take_kind`
        // is about to overwrite it, and the sentence names the plot the user is
        // losing. Not always "scatter" — `chart_would_lose_points` reads
        // `<c:bubbleSize>` too, so a `<c:bubbleChart>` reaches the same branch.
        let was = data.kind.clone();
        // Whether the click RE-READ the chart rather than relabelling it, so the
        // status line can say so afterwards. Which branch a click takes turns on
        // per-series state the panel doesn't draw, and the two do very different
        // things to the cards below — the note above the type buttons warns that
        // a re-read replaces them, but only the click knows it happened.
        let mut reauthored = false;
        let mut data =
            if gridcore::xlsx::chart_kind_is_writable(kind) && chart_would_lose_points(&data) {
                // Bound the borrow of `self` before `set_status` needs it back.
                let out = self
                    .chart_range_sheet(&data)
                    .and_then(|sh| chart_reauthored(&data, kind, sh));
                match out {
                    Ok(d) => {
                        // Re-deriving REPLACES the series rather than relabelling
                        // them: a scatter's one X/Y pair comes back as a series per
                        // numeric column of the box, and three series over a
                        // one-column box come back as one. Card `i` is different
                        // data under the same number, and both of these are keyed
                        // by bare series index — the hazard
                        // `chart_switch_orientation` and `series_delete` clear them
                        // for, reached here by the panel's own "type below, then
                        // pick a type above": an open field would commit its buffer
                        // onto whichever series inherited the number, and when the
                        // count SHRINKS it would go on taking keystrokes while no
                        // longer drawn.
                        self.range_edit = None;
                        self.ref_msg = None;
                        self.range_pick = None;
                        reauthored = true;
                        d
                    }
                    Err(m) => {
                        self.set_status(m);
                        cx.notify();
                        return;
                    }
                }
            } else {
                data
            };
        // Still called on a re-derived chart, so exactly one place decides what
        // picking a type does to `kind`, `complex` and `point_refs`. It is a
        // no-op there — `chart_from_range` already said all three.
        chart_take_kind(&mut data, kind);
        let n = data.series.len();
        self.chart_set_data(data, cx);
        if reauthored {
            self.set_status(format!(
                "Re-read this chart from its data range as {n} series \u{2014} the series below are the range's, not the ones the {was} chart carried."
            ));
        }
    }

    /// Colour one series of the panel's chart (`chart_data`'s).
    fn chart_set_color(&mut self, series: usize, rgb: u32, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        let Some(s) = data.series.get_mut(series) else {
            return;
        };
        // Clicking the colour a series already has clears it back to the
        // palette default.
        s.color = if s.color == Some(rgb) {
            None
        } else {
            Some(rgb)
        };
        self.chart_set_data(data, cx);
    }

    /// Apply a field's text to whatever it edits. Every field commits through
    /// here, so a new target means one arm rather than a new key handler.
    fn ref_commit(&mut self, target: RefTarget, text: &str, cx: &mut Context<Self>) {
        match target {
            RefTarget::ChartRange => self.chart_apply_range(text, cx),
            RefTarget::ChartTitle => {
                if let Some(mut data) = self.chart_data() {
                    data.title = text.trim().to_string();
                    self.chart_set_data(data, cx);
                }
            }
            RefTarget::SeriesValues(i) => self.series_apply_values(i, text, cx),
            RefTarget::SeriesName(i) => self.series_apply_name(i, text, cx),
            RefTarget::Categories => self.categories_apply(text, cx),
            RefTarget::CondFormat
            | RefTarget::Validation
            | RefTarget::Sort
            | RefTarget::TextToColumns => self.bar_range_apply(target, text, cx),
        }
    }

    /// Re-point the open entry bar. The bar itself does nothing until its own
    /// Enter — this only says which cells it will act on.
    fn bar_range_apply(&mut self, target: RefTarget, text: &str, cx: &mut Context<Self>) {
        let Some(active) = self.active_sheet().map(|v| v.active) else {
            return;
        };
        match bar_ref_text(text, target, &self.sheet_names(), active) {
            Ok((_, a1)) => {
                // The message reads as a sentence, so it drops the field's `=`
                // and keeps the qualifier: "Applies to Sheet1!$B$2:$D$5". The
                // qualifier is the whole point when a bar resolved one — it is
                // what says the rule is landing on the other sheet.
                let said = a1.trim_start_matches('=').to_string();
                self.ref_msg = Some((target, true, format!("Applies to {said}")));
                self.bar_range = Some(a1);
            }
            Err(m) => self.ref_msg = Some((target, false, m)),
        }
        cx.notify();
    }

    /// The sheet the open bar acts on: the one its pinned range names, else the
    /// one on screen. Only the bars that resolve a foreign qualifier can differ
    /// from `active` — the others refuse one, so their pinned range always
    /// names this sheet.
    ///
    /// Derived from `bar_range` rather than stored beside it, so the two cannot
    /// drift into naming one sheet and acting on another.
    fn bar_sheet_index(&self) -> Option<usize> {
        let active = self.active_sheet()?.active;
        match self.bar_range.as_deref().and_then(parse_ref_text) {
            Some(r) => sheet_index_of(&self.sheet_names(), r.sheet.as_deref(), active).ok(),
            None => Some(active),
        }
    }

    /// The cells the open bar acts on: the range pinned into its field, else
    /// whatever the field is showing — which follows the selection, exactly as
    /// these bars did before they had a field at all.
    fn bar_cells(&self) -> Option<(u32, u32, u32, u32)> {
        self.bar_range
            .as_deref()
            .and_then(parse_ref_text)
            .map(|r| r.range)
            .or_else(|| self.bar_seed())
    }

    /// What an untouched range field shows: the live selection, or for a sort
    /// the region it would find (header already dropped).
    fn bar_seed(&self) -> Option<(u32, u32, u32, u32)> {
        if self.bar_field == Some(RefTarget::Sort) {
            if let Some((top, bottom)) = self.sheet_sort_bounds() {
                if let Some(max_c) = self.active_sheet().map(|v| v.extent().1) {
                    return Some((top, 0, bottom, max_c));
                }
            }
        }
        self.active_sheet().map(|v| v.range())
    }

    /// Open a bar's range field. It starts unpinned, so until the user types a
    /// range or points at one the bar still acts on the selection.
    fn bar_open(&mut self, target: RefTarget) {
        // The four bars share ONE `bar_field`/`bar_range`, and `sheet_key`
        // routes to whichever is open first. Leaving a second one on screen
        // therefore aims the first at cells pinned for the other — Text to
        // Columns splitting the Sort bar's whole region, say. Only one at a
        // time, which is also what the keyboard already assumed.
        self.bar_close();
        // `bar_close` only drops a field belonging to a bar. A Chart panel field
        // left focused would keep `range_field_active` true, so the very first
        // drag meant for this bar would be committed through `ref_commit` to the
        // CHART — replotting it — while the bar's own range stayed unpinned and
        // its rule landed on the untouched selection instead.
        self.range_edit = None;
        self.range_pick = None;
        self.bar_field = Some(target);
        self.ref_msg = None;
    }

    /// Commit whatever is typed in a focused bar range field, for the commit
    /// paths that don't come through the field's own Enter (the Apply button).
    /// `false` means the text isn't a range: the field keeps its old cells, so
    /// applying anyway would act on cells the user isn't looking at.
    fn bar_flush(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((target, buf)) = self
            .range_edit
            .as_ref()
            .filter(|f| f.target.is_bar())
            .map(|f| (f.target, f.buf.clone()))
        else {
            return true;
        };
        self.range_edit = None;
        self.ref_commit(target, &buf, cx);
        !matches!(&self.ref_msg, Some((t, false, _)) if *t == target)
    }

    /// A bar closed: drop the bar itself along with its range field. Closing the
    /// field alone would leave a bar on screen whose seeding (`bar_seed` keys
    /// off `bar_field`) had gone with it — a Sort bar still showing, now acting
    /// on the selection rather than the region it found.
    fn bar_close(&mut self) {
        self.sheet_cf_edit = None;
        self.sheet_dv_edit = None;
        self.sheet_ttc_edit = None;
        self.sheet_sort_edit = None;
        self.bar_field = None;
        self.bar_range = None;
        if matches!(&self.range_edit, Some(f) if f.target.is_bar()) {
            self.range_edit = None;
        }
        if matches!(&self.ref_msg, Some((t, _, _)) if t.is_bar()) {
            self.ref_msg = None;
        }
    }

    /// Every bar that swallows typing. `sheet_key` asks them BEFORE it asks a
    /// range field that isn't one of theirs, so a field outside them taking
    /// focus has to close them: the Chart panel renders off its own state and
    /// not the bars', so its fields stay clickable while a bar is open — more
    /// so now that it is sticky — and clicking one would
    /// draw a focused border and a caret while every keystroke went to the bar
    /// — and a drag on the grid rewrote (and committed) the CHART's range.
    fn typing_bars_close(&mut self) {
        self.bar_close();
        self.sheet_comment_edit = None;
        self.sheet_filter_edit = None;
        self.sheet_rowh_edit = None;
        self.find_open = false;
    }

    /// Re-point one series at another range, re-reading just its numbers. The
    /// other series and the categories are left exactly as they were.
    fn series_apply_values(&mut self, i: usize, text: &str, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        // The example is the shape THIS chart wants, so the "that isn't a range"
        // message doesn't send a row chart's user off to point at a column.
        let example = self.ref_example(chart_field_examples(data.by_row).values);
        let (si, range) = match self.chart_ref(text, &example) {
            Ok(r) => r,
            Err(m) => {
                self.ref_msg = Some((RefTarget::SeriesValues(i), false, m));
                cx.notify();
                return;
            }
        };
        if let Some(m) = series_values_shape_err(data.by_row, range) {
            self.ref_msg = Some((RefTarget::SeriesValues(i), false, m.into()));
            cx.notify();
            return;
        }
        let Some(v) = self.active_sheet() else { return };
        // The RESOLVED sheet, not the one on screen: its name is what rides on
        // the `ChartSource` written back, and so what makes a cross-sheet
        // reference survive a save.
        let Some((sh, src)) = ref_source(&v.pkg.workbook.sheets, si, range) else {
            return;
        };
        let values = gridcore::sheet::range_numbers(sh, range);
        let Some(n) = series_set_values(&mut data, i, values, src) else {
            return;
        };
        self.ref_msg = Some((RefTarget::SeriesValues(i), true, format!("{n} points")));
        self.chart_set_data(data, cx);
    }

    /// Name a series: from a cell if the text is a reference, else literally.
    fn series_apply_name(&mut self, i: usize, text: &str, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        let shown = data
            .series
            .get(i)
            .map(|s| series_name_shown(&s.name, s.name_ref.as_deref()))
            .unwrap_or_default();
        let (name, name_ref) = match series_name_commit(text, &shown) {
            NameCommit::Unchanged => return,
            NameCommit::Ref(r) => {
                let si = match self.ref_sheet_index(r.sheet.as_deref()) {
                    Ok(si) => si,
                    Err(m) => {
                        self.ref_msg = Some((RefTarget::SeriesName(i), false, m));
                        cx.notify();
                        return;
                    }
                };
                // A name is ONE cell, so the reference is narrowed to its
                // top-left corner BEFORE any cells are read: `chart_space_xml`
                // caches a single point beside the ref, so a wider pick would
                // write a `<c:f>` its own cache contradicts — and this is the
                // one chart field that doesn't go through `chart_ref_of`, so
                // reading the range whole would let `=A1:XFD1048576` build a
                // string per cell of the sheet for a label taken from one.
                let range = r.range;
                let cell = (range.0, range.1, range.0, range.1);
                let Some(v) = self.active_sheet() else { return };
                let Some((sh, src)) = ref_source(&v.pkg.workbook.sheets, si, cell) else {
                    return;
                };
                let label = gridcore::sheet::range_labels(sh, cell)
                    .first()
                    .cloned()
                    .unwrap_or_default();
                (label, Some(src.to_ref()))
            }
            // Not a reference — Excel takes a typed name as the name.
            NameCommit::Literal => (literal_series_name(text), None),
        };
        let Some(s) = data.series.get_mut(i) else {
            return;
        };
        s.name = name;
        s.name_ref = name_ref;
        // A name cell is part of the chart's box (`rebuild_source`), so moving
        // one moves the box — and `parse_chart` would rebuild it from the refs
        // on the next open whether or not this did. Without this the panel
        // shows one DATA RANGE before a save and another after it.
        rebuild_source(&mut data);
        self.chart_set_data(data, cx);
    }

    /// Add an empty series and put the keyboard in its values field, so the
    /// next thing you do is say what it plots.
    fn series_add(&mut self, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        let n = data.series.len();
        // A pie used to refuse a second series here, because `chart_space_xml`
        // wrote only the first and the rest were dropped on save without a
        // word. It writes every one of them now, so "+ Series" adds one to a
        // pie like any other kind — the card says `NOT PLOTTED` and the note
        // under CHART TYPE says the file keeps it (`chart_unplotted_note`).
        data.series.push(gridcore::sheet::ChartSeries {
            name: format!("Series {}", n + 1),
            values: vec![0.0; data.categories.len()],
            ..Default::default()
        });
        self.chart_set_data(data, cx);
        self.range_edit = Some(RangeEdit {
            target: RefTarget::SeriesValues(n),
            buf: String::new(),
            caret: 0,
            anchor: 0,
            dragging: false,
        });
        self.ref_msg = Some((
            RefTarget::SeriesValues(n),
            true,
            "Point at the cells this series plots".into(),
        ));
        cx.notify();
    }

    /// Remove a series, unless it is the only one.
    fn series_delete(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        if !series_remove(&mut data.series, i) {
            self.ref_msg = Some((
                RefTarget::SeriesValues(i),
                false,
                "A chart needs at least one series".into(),
            ));
            cx.notify();
            return;
        }
        self.range_edit = None;
        // `ref_msg` is keyed by series index, and every index past `i` just
        // shifted — the old message would surface under a different series.
        self.ref_msg = None;
        // Rebuilt for the reason that always holds: the panel must show the box
        // `parse_chart` will rebuild on the next open, or it reads one way
        // before a save and another after it.
        //
        // It also shrinks the box off the deleted column — but only when that
        // column is at an EDGE of it. The box is a rectangle, so deleting the
        // middle series of `A1:D5` (labels in A, series in B, C, D) leaves the
        // survivors' refs spanning B..D again and the box unchanged: DATA RANGE
        // still offers `A1:D5`, and Enter on that untouched field hands
        // `chart_from_range` all three numeric columns, which re-derives the
        // deleted series. That is what the loader does on reload too, so the
        // panel is honest either way; undoing the delete through a field nobody
        // typed in is the rectangle's doing, not this call's.
        rebuild_source(&mut data);
        self.chart_set_data(data, cx);
    }

    /// Reorder a series, which is also the order it is drawn and listed in.
    fn series_reorder(&mut self, i: usize, delta: i32, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        if series_move(&mut data.series, i, delta).is_none() {
            return;
        }
        self.range_edit = None;
        self.ref_msg = None; // same index-keying as `series_delete`
        // Reordering changes no reference, but it changes the ORDER they fold
        // in — and the first values ref decides which sheet the box names.
        // Moving a series that reads `Budget` ahead of ones that read `Data`
        // therefore moves the box, and `parse_chart` rebuilds it in the new
        // document order on the next open whether or not this does. Pointing a
        // series at another sheet is a supported commit
        // (`target_takes_foreign_sheet(SeriesValues)`), so such a chart is
        // reachable.
        rebuild_source(&mut data);
        self.chart_set_data(data, cx);
    }

    /// Re-point the category labels.
    fn categories_apply(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(mut data) = self.chart_data() else {
            return;
        };
        // The labels of a row chart run ALONG a row, so a column example here
        // would send its user at the one shape the chart doesn't read.
        let example = self.ref_example(chart_field_examples(data.by_row).categories);
        let (si, range) = match self.chart_ref(text, &example) {
            Ok(r) => r,
            Err(m) => {
                self.ref_msg = Some((RefTarget::Categories, false, m));
                cx.notify();
                return;
            }
        };
        if let Some(m) = categories_shape_err(data.by_row, range) {
            self.ref_msg = Some((RefTarget::Categories, false, m.into()));
            cx.notify();
            return;
        }
        let Some(v) = self.active_sheet() else { return };
        // The resolved sheet: its labels, and its name on the ref written back.
        let Some((sh, src)) = ref_source(&v.pkg.workbook.sheets, si, range) else {
            return;
        };
        data.categories = gridcore::sheet::range_labels(sh, range);
        data.categories_ref = Some(src);
        // The chart's box covers everything it reads, labels included — the same
        // reason `series_apply_values` rebuilds, and the DATA RANGE the panel
        // shows is derived from it.
        rebuild_source(&mut data);
        self.ref_msg = Some((
            RefTarget::Categories,
            true,
            format!("{} labels", data.categories.len()),
        ));
        self.chart_set_data(data, cx);
    }

    /// A press landed on the grid: remember which cell, so the drag that may
    /// follow extends from there rather than from wherever the pointer first
    /// crossed a boundary.
    fn grid_press(&mut self, pos: Point<Pixels>, _cx: &mut Context<Self>) {
        let Some(cell) = self.cell_at(pos) else {
            return;
        };
        if self.formula_pick_active() {
            // Anchor the reference on the pressed cell, with the buffer as it
            // stands, so the drag rewrites from there.
            if let Some(v) = self.active_sheet() {
                self.formula_pick =
                    Some((v.editing.clone().unwrap_or_default(), v.edit_caret, cell));
            }
        } else if self.range_field_active() {
            self.range_pick = Some((cell, false));
        } else {
            self.drag_anchor = Some(cell);
        }
    }

    /// The cell under a window position, or `None` when the pointer isn't over
    /// one (the gutter, the header, past the last column). Rows come from the
    /// list's own measured bounds rather than a uniform row height, so this is
    /// exact on content-tall rows too; columns come from the same widths the
    /// renderer uses.
    ///
    /// This exists because the virtualized list never delivers `on_mouse_down`
    /// to a cell, so a press can only be located by hit-testing it.
    fn cell_at(&self, pos: Point<Pixels>) -> Option<(u32, u32)> {
        let v = self.active_sheet()?;
        let sh = v.sheet();
        // Frozen rows render outside the list, so the list's bounds can't locate
        // a press in that band; leave those sheets on the old behaviour.
        if sh.freeze.0 > 0 {
            return None;
        }
        let list_bounds = v.vlist.viewport_bounds();
        let (x, y) = (f32::from(pos.x - list_bounds.left()), pos.y);
        if y < list_bounds.top() || y > list_bounds.bottom() {
            return None;
        }
        let fc = sh.freeze.1.min(64);
        let col = col_at_x(
            |c| col_px(sh.col_width(c)),
            x,
            fc,
            v.col0.max(fc).min(MAX_VISIBLE_COL),
            MAX_VISIBLE_COL,
        )?;
        // Walk the rendered rows from the scroll position until one contains y.
        let top = v.vlist.logical_scroll_top().item_ix;
        for ix in top..top.saturating_add(200) {
            let Some(b) = v.vlist.bounds_for_item(ix) else {
                break;
            };
            if y >= b.top() && y < b.bottom() {
                // A merged region renders as ONE cell at its top-left, and every
                // other path reports that cell. Pressing in the middle of the
                // merge has to agree, or a drag started inside one covers a
                // different range than the same drag started outside it.
                return v.row_at_list_index(ix).map(|r| match sh.merge_at(r, col) {
                    Some((mr, mc, _, _)) => (mr, mc),
                    None => (r, col),
                });
            }
            if b.top() > pos.y {
                break;
            }
        }
        None
    }

    /// The ranges the formula being typed mentions, for the grid to outline.
    fn formula_refs(&self) -> std::rc::Rc<Vec<(u32, u32, u32, u32)>> {
        let refs = match self.active_sheet().and_then(|v| v.editing.as_deref()) {
            Some(buf) if buf.starts_with('=') => formula_ref_tokens(buf)
                .into_iter()
                .map(|(_, r)| r)
                .collect(),
            _ => Vec::new(),
        };
        std::rc::Rc::new(refs)
    }

    /// Is a formula being typed? Then a click or drag on the grid writes its
    /// cells into the formula instead of moving the selection.
    fn formula_pick_active(&self) -> bool {
        self.active_sheet()
            .and_then(|v| v.editing.as_ref())
            .is_some_and(|b| b.starts_with('='))
    }

    /// Point at `(row, col)` while typing a formula. `start` plants the anchor;
    /// otherwise the reference grows from wherever the anchor already is.
    fn formula_pick_to(&mut self, row: u32, col: u32, start: bool, cx: &mut Context<Self>) {
        let (buf, caret, anchor) = match self.formula_pick.clone() {
            // The press already planted the anchor; a click on the same cell
            // must not move it, or the reference would follow the pointer.
            Some(p) if !start || p.2 == (row, col) => p,
            _ => {
                let Some(v) = self.active_sheet() else { return };
                let base = (
                    v.editing.clone().unwrap_or_default(),
                    v.edit_caret,
                    (row, col),
                );
                self.formula_pick = Some(base.clone());
                base
            }
        };
        // A one-cell pick reads as A1, not A1:A1, which is what Excel writes.
        let text = if anchor == (row, col) {
            gridcore::sheet::cell_name(row, col)
        } else {
            range_text(anchor, (row, col))
        };
        let (next, next_caret) = replace_ref(&buf, caret, &text);
        if let Some(v) = self.active_sheet_mut() {
            v.editing = Some(next);
            v.edit_caret = next_caret;
        }
        cx.notify();
    }

    /// Is a range field holding the keyboard? Then the grid is in point mode:
    /// clicking and dragging over cells writes the range into that field rather
    /// than moving the cell selection.
    fn range_field_active(&self) -> bool {
        matches!(&self.range_edit, Some(f) if f.target.is_range())
    }

    /// Point at `(row, col)`: `start` plants the anchor (a press), otherwise the
    /// pick extends from wherever the anchor already is (a drag or Shift-click).
    fn range_pick_to(&mut self, row: u32, col: u32, start: bool, cx: &mut Context<Self>) {
        let anchor = match self.range_pick {
            Some((a, _)) if !start => {
                self.range_pick = Some((a, true));
                a
            }
            _ => {
                self.range_pick = Some(((row, col), !start));
                (row, col)
            }
        };
        let sheet = self
            .active_sheet()
            .map(|v| v.sheet().name.clone())
            .unwrap_or_default();
        let text = ref_pick_text(&sheet, anchor, (row, col));
        if let Some(f) = &mut self.range_edit {
            f.caret = text.chars().count();
            f.anchor = f.caret;
            f.buf = text;
        }
        cx.notify();
    }

    /// The pointer came up: a picked range replots straight away, the way
    /// dragging a new source range does in Excel.
    fn range_pick_end(&mut self, cx: &mut Context<Self>) {
        // Only a real drag sets the range; a plain click means "done pointing".
        if !matches!(self.range_pick.take(), Some((_, true))) {
            return;
        }
        // Commit to whatever field is being pointed — not always the chart's own
        // range, now that a series' values and the labels are pointable too.
        let Some((target, buf)) = self.range_edit.as_ref().map(|f| (f.target, f.buf.clone()))
        else {
            return;
        };
        self.ref_commit(target, &buf, cx);
    }

    /// Bring a referenced range into view, so pointing a chart at cells that are
    /// scrolled away still shows you what you picked.
    fn reveal_range(&mut self, before: Option<(u32, u32, u32, u32)>) {
        let after = self.range_preview();
        if after == before {
            return;
        }
        let Some((r0, c0, r1, c1)) = after else {
            return;
        };
        if let Some(v) = self.active_sheet_mut() {
            // Reveal the far end first, then the near one: a range that fits
            // ends up wholly in view, and one that doesn't shows its start.
            let (near, far) = (v.row_list_index(r0), v.row_list_index(r1));
            v.vlist.scroll_to_reveal_item(far);
            v.vlist.scroll_to_reveal_item(near);
            // A range to the LEFT can be scrolled to here; one to the right
            // needs the grid width, which only the render pass knows.
            v.col0 = v.col0.min(c0);
            v.reveal_col = Some(c1);
        }
    }

    /// Typing in one of the Chart panel's text fields. Arrows, Home/End and
    /// Delete move and edit around the caret; holding Shift extends the
    /// selection, and anything typed over one replaces it.
    fn range_edit_key(
        &mut self,
        ev: &KeyDownEvent,
        ctrl: bool,
        shift: bool,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        let was = self.range_preview();
        let Some(mut f) = self.range_edit.clone() else {
            return;
        };
        let len = f.buf.chars().count();
        if ctrl {
            if key == "a" {
                f.anchor = 0;
                f.caret = len;
                self.range_edit = Some(f);
                cx.notify();
            }
            return;
        }
        match key {
            "escape" => {
                // Escape is the way out of point mode, so it has to take the
                // last commit's message with it — otherwise an abandoned edit
                // leaves its complaint standing under a field nobody is in.
                self.range_edit = None;
                self.range_pick = None;
                if matches!(&self.ref_msg, Some((t, _, _)) if *t == f.target) {
                    self.ref_msg = None;
                }
            }
            "enter" => {
                // The repaint is ours, not the commit's: several arms return
                // early when nothing changed, and the field has just lost its
                // focus ring and caret either way.
                self.range_edit = None;
                self.ref_commit(f.target, &f.buf, cx);
                cx.notify();
                return;
            }
            // A plain arrow past a selection lands on the end it points at.
            "left" => {
                let to = match f.selection() {
                    Some((s0, _)) if !shift => s0,
                    _ => f.caret.saturating_sub(1),
                };
                f.set_caret(to, shift);
                self.range_edit = Some(f);
            }
            "right" => {
                let to = match f.selection() {
                    Some((_, s1)) if !shift => s1,
                    _ => (f.caret + 1).min(len),
                };
                f.set_caret(to, shift);
                self.range_edit = Some(f);
            }
            "home" => {
                f.set_caret(0, shift);
                self.range_edit = Some(f);
            }
            "end" => {
                f.set_caret(len, shift);
                self.range_edit = Some(f);
            }
            "backspace" => {
                if !f.delete_selection() {
                    buf_backspace(&mut f.buf, &mut f.caret);
                    f.anchor = f.caret;
                }
                self.range_edit = Some(f);
            }
            "delete" => {
                if !f.delete_selection() {
                    buf_delete(&mut f.buf, f.caret);
                }
                self.range_edit = Some(f);
            }
            _ => {
                if let Some(c) = ev.keystroke.key_char.as_deref() {
                    if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                        f.delete_selection(); // typing replaces what's selected
                        buf_insert(&mut f.buf, &mut f.caret, c);
                        f.anchor = f.caret;
                    }
                }
                self.range_edit = Some(f);
            }
        }
        self.reveal_range(was);
        cx.notify();
    }

    /// Apply `f` to the anchor of the idx-th chart on the active sheet, whether
    /// it is one of this session's or one loaded from the file. Takes the undo
    /// snapshot itself, and only when the anchor really moves: a drag of a few
    /// pixels resolves back to the cells it started on, and pushing a snapshot
    /// for that would spend an undo step, clear the redo stack and mark a
    /// pristine file dirty. Returns whether anything changed.
    fn chart_edit_anchor(
        &mut self,
        idx: usize,
        f: impl Fn(&gridcore::sheet::Sheet, (u32, u32), (u32, u32)) -> ((u32, u32), (u32, u32)),
    ) -> bool {
        let Some(loc) = self.chart_locate(idx) else {
            return false;
        };
        let Some(v) = self.active_sheet() else {
            return false;
        };
        let sidx = v.active;
        let (from, to) = match loc {
            ChartRef::Ui(i) => (v.charts[i].from, v.charts[i].to),
            ChartRef::Drawing(i) => {
                let dw = &v.pkg.workbook.sheets[sidx].drawings[i];
                (dw.from, dw.to)
            }
        };
        let (nf, nt) = f(&v.pkg.workbook.sheets[sidx], from, to);
        if (nf, nt) == (from, to) {
            return false;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            match loc {
                ChartRef::Ui(i) => {
                    v.charts[i].from = nf;
                    v.charts[i].to = nt;
                }
                ChartRef::Drawing(i) => {
                    let dw = &mut v.pkg.workbook.sheets[sidx].drawings[i];
                    dw.from = nf;
                    dw.to = nt;
                }
            }
        }
        true
    }

    /// Track a chart move. The card follows the pointer, but its anchor cell
    /// isn't touched until the button comes up.
    fn chart_drag_move(&mut self, at: (f32, f32), cx: &mut Context<Self>) {
        let Some(mut d) = self.chart_drag else { return };
        let delta = (at.0 - d.origin.0, at.1 - d.origin.1);
        if delta == d.delta {
            return;
        }
        d.delta = delta;
        self.chart_drag = Some(d);
        cx.notify();
    }

    /// Finish a chart drag: re-anchor the card to the cells it landed on. A move
    /// carries the whole anchor; a resize moves only the dragged edges.
    fn chart_drag_end(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.chart_drag.take() else {
            return;
        };
        if d.delta == (0.0, 0.0) {
            return; // a plain click — selection only
        }
        // `chart_edit_anchor` takes the snapshot, and only if the cells change.
        let changed = if d.edge == (0, 0) {
            self.chart_edit_anchor(d.idx, |sh, from, to| {
                // The far corner rides along, so the card keeps its cell span.
                let nf = (
                    shift_row(sh, from.0, d.delta.1),
                    shift_col(sh, from.1, d.delta.0),
                );
                let (dr, dc) = (nf.0 as i64 - from.0 as i64, nf.1 as i64 - from.1 as i64);
                (
                    nf,
                    (
                        (to.0 as i64 + dr).max(0) as u32,
                        (to.1 as i64 + dc).max(0) as u32,
                    ),
                )
            })
        } else {
            self.chart_edit_anchor(d.idx, |sh, from, to| {
                let (w, h) = chart_span_px(sh, from, to);
                let (x_off, w_delta) = resize_axis(d.edge.0, d.delta.0, w, MIN_CHART_W);
                let (y_off, h_delta) = resize_axis(d.edge.1, d.delta.1, h, MIN_CHART_H);
                let nf = (shift_row(sh, from.0, y_off), shift_col(sh, from.1, x_off));
                let nt = (
                    shift_row(sh, to.0, y_off + h_delta),
                    shift_col(sh, to.1, x_off + w_delta),
                );
                // Keep at least one cell of span in each axis.
                (nf, (nt.0.max(nf.0 + 1), nt.1.max(nf.1 + 1)))
            })
        };
        if changed {
            self.mark_sheet_dirty();
        }
        cx.notify();
    }

    /// Remove the selected chart (Delete on a selected object, Excel-style).
    fn chart_delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.chart_sel else {
            return;
        };
        // The list is about to shift under every chart-keyed index, the panel's
        // focused field included — taking `chart_sel` alone would leave a
        // `SeriesValues(i)` field pointing into whichever chart slid into the
        // gap.
        self.chart_drop_selection();
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let sidx = v.active;
            let ui_n = v.charts.iter().filter(|c| c.sheet == sidx).count();
            if idx < ui_n {
                if let Some(pos) = v
                    .charts
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.sheet == sidx)
                    .map(|(i, _)| i)
                    .nth(idx)
                {
                    v.charts.remove(pos);
                }
            } else if let Some(i) = v.pkg.workbook.sheets[sidx]
                .drawings
                .iter()
                .enumerate()
                .filter(|(_, dw)| matches!(dw.kind, gridcore::sheet::DrawingKind::Chart(_)))
                .map(|(i, _)| i)
                .nth(idx - ui_n)
            {
                // The drawing part round-trips verbatim, so record the anchor a
                // save has to strike from it as well.
                let gone = v.pkg.workbook.sheets[sidx].drawings.remove(i);
                v.pkg.workbook.sheets[sidx]
                    .drawings_removed
                    .push(gone.anchor_ix);
            }
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// The cells a focused range field refers to, live as it is typed — the
    /// grid outlines them so you can see what you are pointing the chart at.
    /// Only ever cells on the sheet in front of you; see `preview_range`.
    fn range_preview(&self) -> Option<(u32, u32, u32, u32)> {
        let f = self.range_edit.as_ref()?;
        // Every field that names cells outlines them — a series' values and the
        // category labels most of all, since that is where you most need to see
        // what you picked. The title isn't a range, so its text never parses as
        // one by accident.
        if !f.target.is_range() {
            return None;
        }
        let active = self.active_sheet()?.active;
        preview_range(&f.buf, &self.sheet_names(), active)
    }

    /// The cells the selected chart reads, for the grid to outline in the
    /// colour of the slot each one feeds. Nothing at all when no chart is
    /// selected, so deselecting a chart drops the outlines with its handles.
    ///
    /// The active sheet's name is what decides whose cells these are: a chart
    /// may legally read another sheet, and `chart_source_areas` draws nothing
    /// for such a ref rather than pointing at this sheet's cells of the same
    /// address.
    ///
    /// **Nothing while a range field is pointing either.** Picking a range is
    /// the one time the chart stays selected while the grid is being used for
    /// something else, so the three source colours would sit under the dashed
    /// preview being dragged over them — three answers to "which cells matter"
    /// at the moment only one of them does. The outlines come back when the
    /// field gives the keyboard up.
    fn chart_refs(&self) -> std::rc::Rc<Vec<ChartSourceArea>> {
        // `chart_sel`, deliberately, NOT the panel's chart: these outlines are
        // the selection made visible on the cells, so they go with the handles
        // the moment the chart is deselected — even though the panel stays open
        // on it.
        let areas = match (
            self.active_sheet(),
            self.chart_sel.and_then(|i| self.chart_data_at(i)),
        ) {
            (Some(v), Some(cd)) if chart_outlines_shown(true, self.range_field_active()) => {
                chart_source_areas(&cd, &v.sheet().name)
            }
            _ => Vec::new(),
        };
        std::rc::Rc::new(areas)
    }

    /// Everything drawn over the grid that isn't the cells themselves: the fill
    /// and range previews, point mode, the formula's coloured references and a
    /// selected chart's source areas.
    /// One bundle, so the render pass doesn't thread four more arguments through
    /// `sheet_el`. `handle_hidden` is filled in there, where the chart cards'
    /// boxes are known.
    fn grid_overlay(&self) -> GridOverlay {
        GridOverlay {
            fill_preview: self.sheet_fill_preview(),
            range_preview: self.range_preview(),
            // Point mode asks only whether a range field has focus — NOT
            // whether it currently washes anything. A field holding another
            // sheet's reference draws no wash (`preview_range`) and must still
            // let you drag a new range out of the sheet you can see.
            picking: self.range_field_active(),
            formula_refs: self.formula_refs(),
            chart_refs: self.chart_refs(),
            // In point mode a drag off the selection's corner means "sweep a
            // range", not "auto-fill". The handle's own guard only covers an
            // in-cell edit, so without this a drag that starts on those few
            // pixels writes cells instead of picking them.
            handle_hidden: self.range_field_active() || self.formula_pick_active(),
            // The border's range and the cap on its dashes both need the
            // visible-row list and the column window, which only `sheet_el`
            // has.
            border_rg: None,
            range_dashed: true,
            // One selection at a time: a selected chart owns it, and the grid
            // shows nothing of its own until that chart is dismissed.
            sel_hidden: !cell_selection_shown(self.chart_sel),
        }
    }

    /// The box the in-progress fill drag would cover, for the preview outline.
    fn sheet_fill_preview(&self) -> Option<(u32, u32, u32, u32)> {
        let f = self.sheet_fill?;
        if f.to == (f.src.2, f.src.3) {
            return None; // still on the source — nothing to outline
        }
        Some(fill_box(f.src, f.to))
    }

    /// Finish an auto-fill drag: fill the source pattern into the dragged region
    /// (numeric series or copy), leaving the filled box selected.
    fn sheet_fill_end(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.sheet_fill.take() else {
            return;
        };
        let (br0, bc0, br1, bc1) = fill_box(f.src, f.to);
        // Dragged back onto the source, or up/left off it — either way the box
        // is the source and `autofill` would write nothing. Bail before the
        // snapshot, or an idle flick of the handle costs an undo step and marks
        // a clean workbook dirty.
        if (br0, bc0, br1, bc1) == f.src {
            return;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            // Only now does anything move: the cells fill and the selection grows
            // to cover them (during the drag it was just an outline).
            v.anchor = (br0, bc0);
            v.sel = (br1, bc1);
            gridcore::edit::autofill(&mut v.pkg.workbook, s, f.src, f.to);
            // Filled formulas were re-based, so their copied results are stale.
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
            v.engine.recalc_all(&mut v.pkg.workbook);
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
        // A gesture that has already claimed the pointer wins, whatever mode the
        // grid is in. A chart's resize grips straddle the card's edge, so a
        // resize drag is over ORDINARY CELLS from its first move; testing point
        // mode first let that drag rewrite (and, at `grid_release`, commit) the
        // focused range field with whatever cells the pointer swept.
        if self.chart_drag.is_some() {
            return; // the pointer is carrying a chart, not sweeping cells
        }
        if self.sheet_fill.is_some() {
            return; // an auto-fill drag owns the pointer too
        }
        if self.active_sheet().is_some_and(|v| v.col_drag.is_some()) {
            // A column-resize drag is armed from the HEADER, but the cells' own
            // `on_mouse_move` keeps firing as the pointer drifts down into the
            // rows — and each one would sweep the focused range field, with
            // `grid_release` committing whatever it swept.
            return;
        }
        if self.formula_pick_active() {
            return self.formula_pick_to(row, col, false, cx);
        }
        if self.range_field_active() {
            return self.range_pick_to(row, col, false, cx);
        }
        if !self.sheet_dragging {
            self.sheet_dragging = true;
            // The press located the origin; fall back to this cell when it
            // couldn't (frozen rows, or a press outside the grid).
            let (ar, ac) = self.drag_anchor.unwrap_or((row, col));
            self.select_cell(ar, ac, cx);
            if (ar, ac) != (row, col) {
                self.extend_to(row, col, cx);
            }
        } else {
            // Only re-render when the target cell actually changed.
            if self.active_sheet().is_some_and(|v| v.sel != (row, col)) {
                self.extend_to(row, col, cx);
            }
        }
    }

    /// Extend the selection to a cell (Shift+click), keeping the anchor.
    fn extend_to(&mut self, row: u32, col: u32, cx: &mut Context<Self>) {
        // A shift-click, or a sweep across cells, is a press on the CELLS: it
        // takes the selection back from a chart exactly as a plain click does,
        // or the range would grow behind a selection nothing is drawing.
        let after = press_selection(
            SelectTarget::Cell,
            self.chart_sel,
            self.formula_pick_active() || self.range_field_active(),
        );
        if !after.cell_moves {
            if self.formula_pick_active() {
                return self.formula_pick_to(row, col, false, cx);
            }
            return self.range_pick_to(row, col, false, cx);
        }
        if self.active_sheet().is_some_and(|v| v.editing.is_some()) {
            self.sheet_commit(0, 0, cx);
        }
        self.chart_sel = after.chart;
        self.chart_panel_event(PanelEvent::Deselect);
        if after.drop_field {
            self.range_edit = None;
            self.ref_msg = None;
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
        } else {
            return;
        }
        // Everything below points into the sheet we just left.
        self.drop_grid_state();
        cx.notify();
    }

    /// Adjust `col0` (horizontal scroll) so the selected column stays visible in
    /// `avail_w` px of grid width. Called each render before drawing the grid, so
    /// arrow-key navigation past the right edge scrolls columns into view.
    fn reconcile_sheet_hscroll(&mut self, avail_w: f32) {
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        let (_, frz_c) = v.sheet().freeze;
        let fc = frz_c.min(64);
        // The scroll offset never enters the frozen region.
        if v.col0 < fc {
            v.col0 = fc;
        }
        // Available width for the scrollable region excludes the pinned columns.
        let frozen_w: f32 = (0..fc).map(|c| col_px(v.sheet().col_width(c))).sum();
        let avail = (avail_w - frozen_w).max(80.0);
        // A range field asked for a column: scroll just far enough right to
        // show it, then leave the selection-following below alone.
        if let Some(rc) = v.reveal_col.take() {
            if rc >= fc {
                let col0 = v.col0;
                v.col0 =
                    scroll_col0_for_sel(|c| col_px(v.sheet().col_width(c)), col0, fc, rc, avail);
            }
        }
        // Only re-centre on the selection when it has actually moved; otherwise
        // leave col0 alone so manual scrolling (arrows/wheel/thumb) sticks.
        'follow: {
            if v.sel == v.follow_sel {
                break 'follow;
            }
            v.follow_sel = v.sel;
            let sc = v.sel.1;
            if sc < fc {
                break 'follow; // a frozen column is always visible
            }
            let col0 = v.col0;
            v.col0 = scroll_col0_for_sel(|c| col_px(v.sheet().col_width(c)), col0, fc, sc, avail);
        }
        // Both the renderer and the hit-test stop at `MAX_VISIBLE_COL`, so a
        // `col0` past it is a grid that draws column IV and answers no clicks
        // beyond it — and, because the wheel and the thumb move `col0` by one at
        // a time from wherever it is, appears frozen. `reveal_col` is unbounded
        // by design (a rule over a whole column is a normal thing to want), so
        // the clamp belongs here rather than on the range field.
        v.col0 = v.col0.min(MAX_VISIBLE_COL);
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
            let taken = |v: &SheetView, name: &str| {
                v.pkg
                    .workbook
                    .sheets
                    .iter()
                    .any(|s| s.name.eq_ignore_ascii_case(name))
            };
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
        // We just switched sheets, same as `select_sheet`.
        self.drop_grid_state();
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Delete sheet `idx` (guarded: never the last sheet). Fixes up the active
    /// index and any pivot/chart views that referenced shifted sheet indices.
    fn sheet_delete(&mut self, idx: usize, cx: &mut Context<Self>) {
        // Clicking × on a lone sheet does nothing — and must not spend an undo
        // step doing it, which would also throw away the redo stack.
        if self
            .active_sheet()
            .is_none_or(|v| v.pkg.workbook.sheets.len() <= 1 || idx >= v.pkg.workbook.sheets.len())
        {
            return;
        }
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            if !v.pkg.remove_sheet(idx) {
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
            // Deleting a sheet BELOW the active one shifts it down by one;
            // clamping alone would silently leave the view on its neighbour.
            if idx < v.active {
                v.active -= 1;
            }
            if v.active >= v.pkg.workbook.sheets.len() {
                v.active = v.pkg.workbook.sheets.len() - 1;
            }
            v.sel = (0, 0);
            v.anchor = (0, 0);
            v.editing = None;
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        // The chart list was just re-indexed and the view may have moved.
        self.drop_grid_state();
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
        let Some((idx, mut buf)) = self.sheet_rename.clone() else {
            return;
        };
        match key {
            "escape" => self.sheet_rename = None,
            "enter" => {
                if let Some(v) = self.active_sheet_mut() {
                    let old = v.pkg.workbook.sheets.get(idx).map(|s| s.name.clone());
                    // `rename_sheet` follows the refs inside the workbook (and
                    // declines a name already taken); a chart this session
                    // authored isn't in there yet, and would save pointing at a
                    // sheet name that no longer exists.
                    if let (true, Some(old)) = (v.pkg.rename_sheet(idx, &buf), old) {
                        let new = buf.trim().to_string();
                        for c in &mut v.charts {
                            gridcore::edit::rename_sheet_in_chart(&mut c.data, &old, &new);
                        }
                    }
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
    /// The workbook sheet a range field's reference names, as an index into the
    /// open workbook's sheets; `None` — an unqualified reference — is the sheet
    /// on screen.
    ///
    /// The `Err` is worded for the user because that is where it goes: straight
    /// into `ref_msg`, under the field that named a sheet the workbook hasn't
    /// got.
    fn ref_sheet_index(&self, sheet: Option<&str>) -> Result<usize, String> {
        let Some(v) = self.active_sheet() else {
            return Err("there's no workbook open".to_string());
        };
        sheet_index_of(&self.sheet_names(), sheet, v.active)
    }
    /// The open workbook's sheet names, in order, for the by-name lookups a
    /// reference's qualifier needs. Empty when no sheet is on screen.
    fn sheet_names(&self) -> Vec<String> {
        self.active_sheet()
            .map(|v| {
                v.pkg
                    .workbook
                    .sheets
                    .iter()
                    .map(|s| s.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// The `=Sheet1!$A$1:$D$5` a range field holds up as the shape it wants —
    /// spelled with the sheet IN FRONT OF YOU rather than a name the workbook
    /// may not have. A hint (or a complaint) naming `Sheet1` in a workbook whose
    /// sheets are `Budget` and `Ledger` sends whoever follows it straight into
    /// "there's no sheet called \"Sheet1\"".
    fn ref_example(&self, range: (u32, u32, u32, u32)) -> String {
        let names = self.sheet_names();
        let active = self.active_sheet().map_or(0, |v| v.active);
        ref_a1(names.get(active).map(String::as_str), range)
    }

    /// What a chart field's text commits to: the sheet index to read and the
    /// cells of it. The `Err` is the message for `ref_msg`, whether the text
    /// isn't a range, asks for more cells than a chart plots, or names a sheet
    /// the workbook hasn't got.
    fn chart_ref(&self, text: &str, example: &str) -> Result<RefCells, String> {
        let Some(v) = self.active_sheet() else {
            return Err("there's no workbook open".to_string());
        };
        chart_ref_of(text, example, &self.sheet_names(), v.active)
    }
    fn active_sheet_mut(&mut self) -> Option<&mut SheetView> {
        match self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            Some(Surface::Sheet(v)) => Some(v),
            _ => None,
        }
    }
    /// Whether the active sheet is protected (cells read-only until unprotected).
    fn sheet_protected(&self) -> bool {
        self.active_sheet()
            .is_some_and(|v| v.pkg.workbook.sheets[v.active].is_protected())
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
        matches!(
            self.tabs.get(self.active).map(|t| &t.surface),
            Some(Surface::Sheet(_))
        )
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
        // A cell editor and a range field cannot both hold the keyboard. The
        // key router asks `range_edit` BEFORE the cell editor, so leaving a
        // field standing here would draw the caret and the white box over a
        // cell while every keystroke — and the Enter that commits — went to the
        // field instead. Reachable since the Chart panel became sticky: select
        // a chart, click a cell (the chart hands the selection back but the
        // panel stays), click a panel range field, then click the fx bar —
        // `chart_hand_back` returns early there, with nothing selected to hand
        // back, so the drop is owed here.
        //
        // The bars themselves go with it. `sheet_key` asks every one of them
        // before the cell editor too, and clearing `range_edit` alone would
        // hand the keyboard STRAIGHT to the bar the field sat in: `to_bar` is
        // gated on `bar_field`, which is read off `range_edit`. Ribbon ▸ Data
        // Validation, click its range field, click the fx bar — Enter would
        // add a validation rule instead of committing the cell.
        self.typing_bars_close();
        self.range_edit = None;
        self.range_pick = None;
        self.ref_msg = None;
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
            let snap = v.snapshot();
            v.undo.push(snap);
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
                let now = v.snapshot();
                v.redo.push(now);
                v.restore(snap);
                done = true;
            }
        }
        if done {
            // The chart list just changed under it, so an index into it means
            // something else now.
            self.chart_drop_selection();
            self.mark_sheet_dirty();
        }
        cx.notify();
    }

    fn sheet_redo(&mut self, cx: &mut Context<Self>) {
        let mut done = false;
        if let Some(v) = self.active_sheet_mut() {
            if let Some(snap) = v.redo.pop() {
                let now = v.snapshot();
                v.undo.push(now);
                v.restore(snap);
                done = true;
            }
        }
        if done {
            self.chart_drop_selection();
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
                    v.engine.set_cell(
                        &mut v.pkg.workbook,
                        (s, r, c),
                        gridcore::sheet::Cell {
                            style,
                            ..Default::default()
                        },
                    );
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
                    v.engine.set_cell(
                        &mut v.pkg.workbook,
                        (s, br + dr as u32, bc + dc as u32),
                        cell.clone(),
                    );
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
            v.col_drag = Some(ColDrag {
                col,
                start_x: x,
                start_w: w,
            });
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
        // This writes to `v.range()`, so the selection it acts on has to be the
        // one the grid is DRAWING. `run_sheet_act` hands the selection back for
        // the ribbon commands, but not every formatting writer arrives that way
        // — the Number dropdown flips its own flag and its strip calls
        // `sheet_apply_numfmt` directly, and the colour swatches call
        // `sheet_apply_color` directly. Asking here covers the lot: it returns
        // immediately when no chart is selected, so the common path is free.
        self.chart_hand_back(cx);
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r0, c0, r1, c1) = v.range();
            let s = v.active;
            for r in r0..=r1 {
                for c in c0..=c1 {
                    let cur = v.sheet().cell(r, c).cloned();
                    let mut xf = v
                        .pkg
                        .workbook
                        .styles
                        .xf(cur.as_ref().map(|cl| cl.style).unwrap_or(0));
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
                v.pkg
                    .workbook
                    .styles
                    .xf(v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0))
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
        let Some(mut buf) = self.sheet_cf_edit.clone() else {
            return;
        };
        match key {
            "escape" => {
                self.sheet_cf_edit = None;
                self.bar_close();
            }
            "enter" => {
                let cells = self.bar_cells();
                if buf.trim().eq_ignore_ascii_case("clear") {
                    self.sheet_snapshot();
                    if let Some(v) = self.active_sheet_mut() {
                        let s = v.active;
                        v.pkg.clear_conditional_formats(s);
                        v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
                    }
                    self.mark_sheet_dirty();
                } else if let Some(((op, val, val2), cells)) = parse_cf_input(&buf).zip(cells) {
                    self.sheet_snapshot();
                    if let Some(v) = self.active_sheet_mut() {
                        let s = v.active;
                        v.pkg.add_conditional_format(
                            s,
                            cells,
                            op,
                            &val,
                            val2.as_deref(),
                            cf_preset_dxf(),
                        );
                        v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
                    }
                    self.mark_sheet_dirty();
                }
                self.sheet_cf_edit = None;
                self.bar_close();
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
                        let val = v.pkg.workbook.sheets[s]
                            .cell(r, sc)
                            .map(|c| c.value.clone());
                        gridcore::filter::matches(val.as_ref(), op, &operand)
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
        let Some(mut buf) = self.sheet_ttc_edit.clone() else {
            return;
        };
        match key {
            "escape" => {
                self.sheet_ttc_edit = None;
                self.bar_close();
            }
            "enter" => {
                let delim = parse_delim(&buf);
                if let Some((r0, c0, r1, _)) = self.bar_cells() {
                    self.sheet_snapshot();
                    if let Some(v) = self.active_sheet_mut() {
                        let s = v.active;
                        gridcore::edit::text_to_columns(&mut v.pkg.workbook, s, c0, r0, r1, delim);
                        v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
                    }
                    self.mark_sheet_dirty();
                }
                self.sheet_ttc_edit = None;
                self.bar_close();
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
        let Some(mut buf) = self.sheet_filter_edit.clone() else {
            return;
        };
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
        let Some(mut buf) = self.sheet_sort_edit.clone() else {
            return;
        };
        match key {
            "escape" => {
                self.sheet_sort_edit = None;
                self.bar_close();
                cx.notify();
            }
            "enter" => {
                self.sheet_sort_edit = None;
                self.sheet_commit_sort(&buf, cx);
                self.bar_close();
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
        let Some(mut buf) = self.sheet_rowh_edit.clone() else {
            return;
        };
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
        let Some(mut buf) = self.sheet_dv_edit.clone() else {
            return;
        };
        match key {
            "escape" => {
                self.sheet_dv_edit = None;
                self.bar_close();
            }
            "enter" => {
                let items: Vec<String> = buf
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                // The rule lands on the sheet the field NAMES, which for a
                // validation may not be the one on screen: the list is built
                // where it is read, and the boxes reading it commonly sit on
                // another sheet from the one it was typed on.
                let target = self.bar_cells().zip(self.bar_sheet_index());
                match target {
                    _ if items.is_empty() => {}
                    Some((cells, s)) => {
                        let f1 = format!("\"{}\"", items.join(","));
                        self.sheet_snapshot();
                        if let Some(v) = self.active_sheet_mut() {
                            v.pkg.add_data_validation(s, cells, "list", "", &f1, None);
                        }
                        self.mark_sheet_dirty();
                    }
                    // The pinned range no longer resolves — its sheet was
                    // renamed or removed while this bar sat open. The bar closes
                    // either way, and `bar_close` takes its `ref_msg` with it, so
                    // the status line is what's left to say the rule never
                    // landed rather than let the close look like it applied.
                    None => self.set_status("That range names a sheet this workbook hasn't got"),
                }
                self.sheet_dv_edit = None;
                self.bar_close();
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
        let Some(mut buf) = self.sheet_comment_edit.clone() else {
            return;
        };
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
        let Some(text) = self.sheet_comment_edit.take() else {
            return;
        };
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
                cells
                    .iter()
                    .find(|&&x| x > cur)
                    .copied()
                    .unwrap_or(cells[0])
            } else {
                cells
                    .iter()
                    .rev()
                    .find(|&&x| x < cur)
                    .copied()
                    .unwrap_or(*cells.last().unwrap())
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
            if let Some(i) = wb.sheets[s]
                .merges
                .iter()
                .position(|&(mr1, mc1, _, _)| mr1 == r0 && mc1 == c0)
            {
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
        use gridcore::sheet::{Cell, CellValue, cell_name};
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            let (r, c) = v.sel;
            let sh = &v.pkg.workbook.sheets[s];
            let is_num = |rr: u32, cc: u32| {
                matches!(
                    sh.cell(rr, cc).map(|x| &x.value),
                    Some(CellValue::Number(_))
                )
            };
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
            let cell = Cell {
                style,
                ..Cell::formula(&format!("SUM({range})"))
            };
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
            let outlined: Vec<u32> = sh
                .row_attrs
                .keys()
                .copied()
                .filter(|&r| sh.row_outline(r) >= 1)
                .collect();
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
                let row_used =
                    |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
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
                    let col_used = |c: u32| {
                        (top..=bottom).any(|r| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()))
                    };
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
                let has_header = (c1..=c2).all(|c| {
                    matches!(sh.cell(r1, c).map(|cl| &cl.value), Some(CellValue::Text(_)))
                });
                v.pkg
                    .add_table(s, (r1, c1, r2, c2), has_header, "TableStyleMedium2");
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
            matches!(
                sh.cell(top, c).map(|cl| &cl.value),
                Some(CellValue::Text(_))
            ) && (top + 1..=bottom).any(|r| {
                matches!(
                    sh.cell(r, c).map(|cl| &cl.value),
                    Some(CellValue::Number(_))
                )
            })
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
        // Only a PINNED range overrides the region the sort would find on its
        // own; the field showing that region is not the user choosing it.
        let field = self
            .bar_range
            .as_deref()
            .and_then(parse_ref_text)
            .map(|r| r.range);
        let Some((start, bottom)) = sort_rows_from(field, self.sheet_sort_bounds()) else {
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
            let shift = match op {
                StructOp::InsertRow => {
                    edit::insert_rows(wb, s, r, 1);
                    (true, r, 1i64)
                }
                StructOp::DeleteRow => {
                    edit::delete_rows(wb, s, r, 1);
                    (true, r, -1)
                }
                StructOp::InsertCol => {
                    edit::insert_cols(wb, s, c, 1);
                    (false, c, 1)
                }
                StructOp::DeleteCol => {
                    edit::delete_cols(wb, s, c, 1);
                    (false, c, -1)
                }
            };
            // Charts the UI authored live outside the workbook until they're
            // saved, so `structural_edit` never sees them. Their refs are
            // written to the file all the same, and Excel re-reads them.
            let name = v.pkg.workbook.sheets[s].name.clone();
            let shift = gridcore::formula::EditShift {
                rows: shift.0,
                at: shift.1,
                delta: shift.2,
            };
            // Every authored chart, not just the ones ON the edited sheet: a
            // chart elsewhere can name these cells outright (`Data!$B$2:$B$10`)
            // and has to follow them. `home` is what keeps the UNqualified refs
            // — which mean the chart's own sheet — out of it.
            for ch in v.charts.iter_mut() {
                edit::shift_chart_refs(&mut ch.data, &name, ch.sheet == s, &shift);
            }
            v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
        }
        self.mark_sheet_dirty();
        cx.notify();
    }

    /// Follow the hyperlink on cell (r,c) of the active sheet, if any: jump for an
    /// in-workbook `#Sheet!A1` target, else open the URL externally.
    fn sheet_follow_hyperlink(&mut self, r: u32, c: u32, cx: &mut Context<Self>) {
        let link = self
            .active_sheet()
            .and_then(|v| v.sheet().hyperlinks.get(&(r, c)).cloned());
        let Some(link) = link else { return };
        if let Some(loc) = link.strip_prefix('#') {
            let (sheet_name, cellref) = match loc.rsplit_once('!') {
                Some((s, cr)) => (Some(s.trim_matches('\'').to_string()), cr.to_string()),
                None => (None, loc.to_string()),
            };
            // A link that lands on another sheet is a sheet switch like any
            // other, so everything keyed to the grid we are leaving has to go
            // with it. The Chart panel especially: it is STICKY now, so unlike
            // before it is still open when the jump happens, and "chart 2" on
            // the sheet we land on is a different chart the user never picked.
            let mut switched = false;
            if let (Some(sn), Some(v)) = (sheet_name, self.active_sheet_mut()) {
                if let Some(idx) = v.pkg.workbook.sheets.iter().position(|s| s.name == sn) {
                    switched = idx != v.active;
                    v.active = idx;
                }
            }
            if switched {
                self.drop_grid_state();
            }
            if let Some(v) = self.active_sheet_mut() {
                if let Some((rr, cc)) = gridcore::sheet::parse_cell_name(&cellref.replace('$', ""))
                {
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
        let dv = sh
            .validations
            .iter()
            .find(|d| d.kind == "list" && d.covers(r, c))?;
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
                (
                    v.pkg.workbook.sheets.iter().position(|s| s.name == sname)?,
                    rest,
                )
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
            v.engine.set_cell(
                &mut v.pkg.workbook,
                (s, r, c),
                parse_cell_input(&value, style),
            );
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
        self.sheet_format(
            move |xf| {
                let cur = xf.font_size.unwrap_or(11.0);
                xf.font_size = Some((cur + delta).clamp(1.0, 409.0));
            },
            cx,
        );
    }
    /// Apply a number format code to the selection (Excel's %, currency, comma).
    fn sheet_numfmt(&mut self, code: &'static str, cx: &mut Context<Self>) {
        self.sheet_format(move |xf| xf.code = Some(code.to_string()), cx);
    }

    /// Apply a number format from the Number dropdown ("" = General/clear) + close.
    fn sheet_apply_numfmt(&mut self, code: &str, cx: &mut Context<Self>) {
        let code = code.to_string();
        self.sheet_format(
            move |xf| {
                xf.code = if code.is_empty() {
                    None
                } else {
                    Some(code.clone())
                }
            },
            cx,
        );
        self.sheet_numfmt_open = false;
    }

    /// Friendly name for the selected cell's current number format.
    fn active_numfmt_name(&self) -> &'static str {
        let code = self.active_xf().code;
        match code {
            None => "General",
            Some(c) => NUM_FORMATS
                .iter()
                .find(|(_, fc)| *fc == c.as_str())
                .map(|(n, _)| *n)
                .unwrap_or("Custom"),
        }
    }
    /// Set fill or font colour on the selection from a swatch (None = clear), and
    /// close the picker.
    fn sheet_apply_color(
        &mut self,
        pick: SheetPick,
        rgb: Option<(u8, u8, u8)>,
        cx: &mut Context<Self>,
    ) {
        self.sheet_format(
            move |xf| match pick {
                SheetPick::Fill => xf.fill = rgb,
                SheetPick::Font => xf.color = rgb,
            },
            cx,
        );
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
    fn sheet_find_key(
        &mut self,
        ev: &KeyDownEvent,
        shift: bool,
        key: &str,
        cx: &mut Context<Self>,
    ) {
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
        // A find MOVES the selection, so it is a press on the cells like any
        // arrow key: take the selection back first. `Ctrl+F` itself is left off
        // the hand-back list — opening a bar is aimed at the window — but what
        // the bar then does is not, and the guard cannot live at the keystroke
        // anyway: `sheet_key` returns into `sheet_find_key` while `find_open`
        // is set, and the bar's own buttons never reach `sheet_key` at all.
        // Without this the ring lands on a cell `sel_hidden` is not drawing.
        self.chart_hand_back(cx);
        if let Some(v) = self.active_sheet_mut() {
            let (mr, mc) = v.extent();
            let ncols = mc as i64 + 1;
            let total = (mr as i64 + 1) * ncols;
            let (sr, sc) = v.sel;
            let start = sr as i64 * ncols + sc as i64;
            for step in 1..=total {
                let idx = if back {
                    (start - step).rem_euclid(total)
                } else {
                    (start + step).rem_euclid(total)
                };
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
        // Replace WRITES the selected cell, so it hands the selection back for
        // the same reason `Ctrl+X` does: a write to a cell the chart is hiding
        // is one you cannot see happening. Before `sheet_snapshot`, so the undo
        // step is taken with the selection already back on the cells.
        self.chart_hand_back(cx);
        self.sheet_snapshot();
        if let Some(v) = self.active_sheet_mut() {
            let (r, c) = v.sel;
            let s = v.active;
            let text = v.cell_text(r, c);
            if text.to_lowercase().contains(&q.to_lowercase()) {
                let new = ci_replace(&text, &q, &rep);
                let style = v.sheet().cell(r, c).map(|cl| cl.style).unwrap_or(0);
                v.engine.set_cell(
                    &mut v.pkg.workbook,
                    (s, r, c),
                    parse_cell_input(&new, style),
                );
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
                        v.engine.set_cell(
                            &mut v.pkg.workbook,
                            (s, r, c),
                            parse_cell_input(&new, style),
                        );
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
            def = Some(PivotDef {
                src_sheet: s,
                src_range: (r0, c0, r1, c1),
                out_sheet: 0,
                names: frame.names.clone(),
                role,
                agg,
            });
        }
        if let Some(mut d) = def {
            if let Some(v) = self.active_sheet_mut() {
                let n = v
                    .pkg
                    .workbook
                    .sheets
                    .iter()
                    .filter(|s| s.name.starts_with("Pivot"))
                    .count();
                let name = if n == 0 {
                    "Pivot".to_string()
                } else {
                    format!("Pivot{}", n + 1)
                };
                // add_sheet wires the OPC part + workbook entry so the sheet saves.
                d.out_sheet = v.pkg.add_sheet(&name);
                v.active = d.out_sheet;
                v.pivot_views.push(d);
                v.sel = (0, 0);
                v.anchor = (0, 0);
                v.editing = None;
            }
            // A brand-new sheet, so the chart selection and any open field are
            // pointing at the one we came from.
            self.drop_grid_state();
            let idx = self
                .active_sheet()
                .map(|v| v.pivot_views.len() - 1)
                .unwrap_or(0);
            self.recompute_pivot(idx);
            self.mark_sheet_dirty();
        }
        cx.notify();
    }

    /// (Re)compute pivot `idx` from its current field roles and write the result
    /// onto its output sheet.
    fn recompute_pivot(&mut self, idx: usize) {
        use gridcore::frame::{Frame, Measure, PivotSpec, pivot, pivot_table_strings};
        use gridcore::sheet::Cell;
        let built = self.active_sheet().and_then(|v| {
            let d = v.pivot_views.get(idx)?;
            let frame = Frame::from_range(&v.pkg.workbook, d.src_sheet, d.src_range);
            let pick = |want: u8| {
                d.role
                    .iter()
                    .enumerate()
                    .filter(move |(_, r)| **r == want)
                    .map(|(i, _)| i)
            };
            let rows: Vec<usize> = pick(1).collect();
            let cols: Vec<usize> = pick(2).collect();
            let measures: Vec<Measure> = pick(3)
                .map(|i| {
                    let (agg, lbl) =
                        PIVOT_AGGS[d.agg.get(i).copied().unwrap_or(0) as usize % PIVOT_AGGS.len()];
                    Measure {
                        col: i,
                        agg,
                        name: format!("{lbl} of {}", frame.names[i]),
                        calc: None,
                    }
                })
                .collect();
            let spec = PivotSpec {
                rows,
                cols,
                measures,
                grand_rows: true,
                grand_cols: true,
                ..Default::default()
            };
            let out = pivot(&frame, &spec);
            Some((
                pivot_table_strings(&out),
                out.header_rows,
                out.label_cols,
                d.out_sheet,
            ))
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
        // Plot first, so nothing below runs (and no undo entry is pushed) for a
        // range that holds no numbers.
        let Some((range, data)) = self.active_sheet().and_then(|v| {
            let range = if v.has_range() {
                v.range()
            } else {
                // Nothing selected: plot the used cells — but a big sheet has
                // far more of those than a card can draw, and this path doesn't
                // go through `chart_ref_of`'s cap. Columns are capped first,
                // at a count a legend can still name; without that a sheet 4096
                // columns wide leaves room for zero rows and the button does
                // nothing at all.
                const MAX_CHART_COLS: u32 = 64;
                let (mr, mc) = v.extent();
                let mc = mc.min(MAX_CHART_COLS - 1);
                let cols = u64::from(mc) + 1;
                let rows = (MAX_CHART_CELLS / cols).saturating_sub(1).max(1) as u32;
                (0, 0, mr.min(rows), mc)
            };
            let sh = v.sheet();
            // A freshly inserted chart reads columns, as Excel's does.
            let data = gridcore::sheet::chart_from_range(sh, &sh.name, range, kind, false)?;
            Some((range, data))
        }) else {
            return;
        };
        // Insert ▸ Pie over several numeric columns reads as a series each, and
        // that is inserted as-is: the writer keeps every one of them, and the
        // panel says the pie draws the first (`chart_unplotted_note`).
        //
        // UI-authored charts live in the snapshot alongside the workbook, so
        // without this Ctrl+Z would undo the edit BEFORE the insert and leave
        // the chart standing.
        self.sheet_snapshot();
        // A UI-authored chart goes in FRONT of the file's drawings in
        // `chart_locate`'s order, so pushing one shifts every drawing-backed
        // index by one. A selection or a focused panel field left over from
        // before would then name — and, on the next pick, repoint — a different
        // chart than the one on screen.
        self.chart_drop_selection();
        if let Some(v) = self.active_sheet_mut() {
            let s = v.active;
            // Anchor the saved chart just right of the selected range. The span
            // is the card's size now, so pick one that reads well and let the
            // user drag it from there.
            let (r0, _, _, c1) = range;
            v.charts.push(ChartView {
                sheet: s,
                from: (r0, c1 + 2),
                to: (r0 + 10, c1 + 8),
                data,
            });
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
                .flex()
                .flex_1()
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py(px(3.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|dd| dd.bg(pal.hover))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(pal.fg)
                        .overflow_hidden()
                        .child(SharedString::from(name.clone())),
                )
                .when(!badge.is_empty(), |dd| {
                    dd.child(
                        div()
                            .px_1p5()
                            .py(px(1.))
                            .rounded(px(3.))
                            .text_size(px(10.))
                            .text_color(hsla_u(0xffffff))
                            .bg(col)
                            .child(badge),
                    )
                })
                .on_click(move |_ev, _w, cx| {
                    ent_role.update(cx, |this, cx| this.pivot_cycle_field(idx, i, cx));
                });
            let mut row = h_flex().items_center().gap_1().child(name_area);
            if role_val == 3 {
                let agg_lbl =
                    PIVOT_AGGS[d.agg.get(i).copied().unwrap_or(0) as usize % PIVOT_AGGS.len()].1;
                let ent_agg = ent.clone();
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("pivagg-{i}").into()))
                        .flex_none()
                        .px_1p5()
                        .py(px(1.))
                        .rounded(px(3.))
                        .text_size(px(10.))
                        .text_color(pal.fg)
                        .bg(pal.hover)
                        .border_1()
                        .border_color(pal.border)
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
            .w(px(SIDE_PANEL_W))
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

    /// One text field, whatever it edits. While it holds the keyboard it shows
    /// the live buffer split around a caret, each half a click-to-caret run
    /// (the formula bar's trick); otherwise the committed value, or a hint when
    /// that is empty. Under it sits whatever the last commit said about this
    /// field, or `help` when there is nothing to report. `id` is a `String` so
    /// the per-series fields can build theirs from the series index.
    fn ref_field(
        &self,
        id: impl Into<String>,
        target: RefTarget,
        value: String,
        hint: String,
        help: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id: String = id.into();
        let pal = Pal::of(cx);
        let ent = cx.entity();
        let editing = match &self.range_edit {
            Some(f) if f.target == target => Some(f.clone()),
            _ => None,
        };
        let shown = match &editing {
            Some(_) => String::new(),
            None if value.is_empty() => hint,
            None => value.clone(),
        };
        let idle_text = StyledText::new(SharedString::from(shown));
        let seed = value.clone();
        let msg = match &self.ref_msg {
            Some((t, ok, m)) if *t == target => Some((*ok, m.clone())),
            _ => None,
        };
        let boxed = div()
            .id(ElementId::Name(id.into()))
            .h(px(24.))
            .px_2()
            .flex()
            .items_center()
            .gap(px(1.))
            .rounded(px(4.))
            .bg(hsla_u(0xffffff))
            .border_1()
            .border_color(if editing.is_some() {
                hsla_u(BRAND)
            } else {
                pal.border
            })
            .cursor_text()
            .text_size(px(12.))
            .text_color(if editing.is_some() || !value.is_empty() {
                hsla_u(0x1a1a1a)
            } else {
                hsla_u(0x999999)
            })
            .map(|d| match &editing {
                Some(f) => d.child(ref_field_row(f, &ent)),
                None => d.child(div().overflow_hidden().child(idle_text)),
            })
            // The click that FOCUSES a field selects all of it, so typing a new
            // value replaces the old one instead of appending to it (the
            // caret-at-click behaviour turned every retyped range into
            // "A1:B5A1:D5"). Clicks once focused place the caret normally.
            .when(editing.is_none(), |d| {
                let ent_c = ent.clone();
                d.on_mouse_down(MouseButton::Left, move |_ev, _w, cx2| {
                    cx2.stop_propagation();
                    let seed = seed.clone();
                    ent_c.update(cx2, |this, cx2| {
                        // A bar's own field is routed to first and must leave
                        // its bar standing; anything else has to take the
                        // keyboard away from whatever bar is open.
                        if !target.is_bar() {
                            this.typing_bars_close();
                        }
                        let n = seed.chars().count();
                        this.range_edit = Some(RangeEdit {
                            target,
                            buf: seed,
                            caret: n,
                            anchor: 0,
                            dragging: false,
                        });
                        cx2.notify();
                    });
                })
            });
        v_flex()
            .child(boxed)
            .when(msg.is_some() || !help.is_empty(), |d| {
                d.child(match msg {
                    Some((ok, m)) => div()
                        .pt(px(2.))
                        .text_size(px(10.))
                        .text_color(if ok { hsla_u(BRAND) } else { hsla_u(0xd0322b) })
                        .child(SharedString::from(m)),
                    None => div()
                        .pt(px(2.))
                        .text_size(px(10.))
                        .text_color(pal.dim)
                        .child(help),
                })
            })
            .into_any_element()
    }

    /// The "Chart" side panel, shown while `panel_chart_shown()` is `Some` —
    /// from the press on a card until Escape, the close box or an `Invalidate`,
    /// which under the sticky rule outlives the chart's SELECTION: what it
    /// plots, how it is drawn, its title, and a colour per series. Every
    /// control acts immediately on the chart the panel SHOWS (`chart_data` /
    /// `chart_set_data`), not on whatever is selected.
    fn chart_panel(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let Some(data) = self.chart_data() else {
            return div().into_any_element();
        };
        let ent = cx.entity();
        let heading = |t: &'static str| {
            div()
                .px_1()
                .pt_2()
                .pb(px(2.))
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .text_color(pal.dim)
                .child(t)
        };

        // Chart type: the four kinds we draw, current one filled in.
        let mut types = h_flex().gap(px(4.)).flex_wrap();
        for (kind, label) in [
            ("column", "Column"),
            ("bar", "Bar"),
            ("line", "Line"),
            ("pie", "Pie"),
        ] {
            let on = data.kind == kind;
            let ent_k = ent.clone();
            types = types.child(
                div()
                    .id(ElementId::Name(format!("charttype-{kind}").into()))
                    .px_2()
                    .py(px(3.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .border_1()
                    .border_color(if on { hsla_u(BRAND) } else { pal.border })
                    .bg(if on { hsla_u(BRAND) } else { pal.hover })
                    .text_color(if on { hsla_u(0xffffff) } else { pal.fg })
                    .hover(|d| if on { d } else { d.bg(pal.panel) })
                    .child(label)
                    .on_click(move |_ev, _w, cx2| {
                        ent_k.update(cx2, |this, cx2| this.chart_set_kind(kind, cx2));
                    }),
            );
        }

        // Excel's Switch Row/Column, which lives in its Select Data Source
        // dialog — here it sits under the type buttons, since both answer "what
        // does this range mean".
        //
        // It is enabled exactly when clicking it would do something: the flip
        // derived here IS what the click commits, so the button can never look
        // live and then do nothing, and it is derived once rather than once to
        // light the button and again to act on it. When it can't be derived,
        // `chart_switched` says why and that becomes the note under the greyed
        // button, rather than leaving a dead button to be clicked at.
        let switched = self.chart_switched();
        let can_switch = switched.is_ok();
        let switch_note = match &switched {
            Ok(_) => format!(
                "Each {} is a series; each {} a category.",
                if data.by_row { "row" } else { "column" },
                if data.by_row { "column" } else { "row" },
            ),
            Err(why) => why.clone(),
        };
        let flip = switched.ok();
        // What picking a type would actually DO to this chart, for the
        // not-writable note below — `None` when a click just relabels it,
        // `Some(Ok)` when it re-reads the box, `Some(Err)` when it can only
        // refuse.
        //
        // Asked by TRYING it, the way `switched` above asks its own question,
        // rather than by a proxy. `data.source.is_some()` was one, and not a
        // sound one: `chart_reauthored` refuses on four further counts that all
        // leave the box in place — a plot half the box doesn't cover, no line
        // to widen a points-leading box into, a widened box past the cell cap,
        // and a box with no line of numbers under a header — plus
        // `chart_range_sheet`'s missing sheet. Each of those had the note
        // promising a re-read that the click then declined, which is the shape
        // the `CHART_NO_BOX` case was already fixed for.
        //
        // `"column"` stands for all four buttons: they are all writable kinds,
        // and no refusal left in `chart_set_kind` turns on WHICH one is picked.
        // (One did — a pie-only series count — until the writer stopped losing
        // a pie's extra series and the refusal stopped being a fix.)
        //
        // Cheap enough for a render path for `chart_switched`'s reason: the
        // same `chart_range_sheet` cap bounds both, and the flip above already
        // re-derives a whole chart every frame.
        let reread = chart_would_lose_points(&data).then(|| {
            self.chart_range_sheet(&data)
                .and_then(|sh| chart_reauthored(&data, "column", sh))
                .map(|_| ())
        });
        let ent_sw = ent.clone();
        let switch = v_flex()
            .gap(px(2.))
            .pt(px(4.))
            .child(
                div()
                    .id("chart-switch-rowcol")
                    .px_2()
                    .py(px(3.))
                    .rounded(px(4.))
                    .text_size(px(11.))
                    .border_1()
                    .border_color(pal.border)
                    .text_color(if can_switch { pal.fg } else { pal.dim })
                    .when(can_switch, |d| {
                        d.cursor_pointer().hover(|d| d.bg(pal.hover)).on_click(
                            // Cloned per click rather than moved: an `on_click`
                            // handler is a `Fn`, and this one outlives the
                            // frame that derived the flip.
                            move |_ev, _w, cx2| {
                                let Some(flip) = flip.clone() else { return };
                                ent_sw.update(cx2, |this, cx2| {
                                    this.chart_switch_orientation(flip, cx2)
                                });
                            },
                        )
                    })
                    .tooltip(move |w, cx2| {
                        gpui_component::tooltip::Tooltip::new(
                            "Switch Row/Column \u{2014} replot the range with its rows and \
                             columns swapped",
                        )
                        .build(w, cx2)
                    })
                    .child("Switch Row/Column"),
            )
            .child(
                div()
                    .px_1()
                    .text_size(px(10.))
                    .text_color(pal.dim)
                    .child(switch_note),
            );

        // One card per series: what names it, what it plots, and its colour.
        // Everything here is a field, so a series can be pointed at the grid.
        // The two hints are the same on every card and cost a walk of the
        // workbook's sheet names each, so they are spelled once for the lot.
        // They follow the chart's orientation for the reason
        // `chart_field_examples` gives: the values field REFUSES a column on a
        // row chart, so a fixed column hint would offer a range of its own that
        // it then rejects.
        let ex = chart_field_examples(data.by_row);
        let name_hint = format!("e.g. {} or a name", self.ref_example(ex.name));
        let vals_hint = format!("e.g. {}", self.ref_example(ex.values));
        let mut series_list = v_flex().gap(px(6.));
        for (si, sr) in data.series.iter().enumerate() {
            let vals = sr
                .values_ref
                .as_ref()
                .map(source_ref_text)
                .unwrap_or_default();
            let mut swatches = h_flex().gap(px(3.));
            for swatch in CHART_COLORS {
                let on = sr.color == Some(swatch);
                let ent_c = ent.clone();
                swatches = swatches.child(
                    div()
                        .id(ElementId::Name(
                            format!("chartcol-{si}-{swatch:06x}").into(),
                        ))
                        .w(px(18.))
                        .h(px(14.))
                        .rounded(px(3.))
                        .bg(rgb(swatch))
                        .border_2()
                        .border_color(if on {
                            hsla_u(0x1a1a1a)
                        } else {
                            hsla_u(0xffffff)
                        })
                        .cursor_pointer()
                        .on_click(move |_ev, _w, cx2| {
                            ent_c.update(cx2, |this, cx2| this.chart_set_color(si, swatch, cx2));
                        }),
                );
            }
            // Reorder / remove, one small button each.
            let btn =
                |id: String, glyph: &'static str, tip: &'static str, on: bool, f: SeriesAction| {
                    let ent_b = ent.clone();
                    div()
                        .id(ElementId::Name(id.into()))
                        .w(px(16.))
                        .h(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .text_size(px(10.))
                        .text_color(if on { pal.fg } else { pal.dim })
                        .when(on, |d| d.cursor_pointer().hover(|d| d.bg(pal.panel)))
                        .tooltip(move |w, cx2| {
                            gpui_component::tooltip::Tooltip::new(tip).build(w, cx2)
                        })
                        .child(glyph)
                        .when(on, |d| {
                            d.on_click(move |_ev, _w, cx2| {
                                ent_b.update(cx2, |this, cx2| f(this, cx2));
                            })
                        })
                };
            let n_series = data.series.len();
            series_list = series_list.child(
                v_flex()
                    .gap(px(3.))
                    .p(px(6.))
                    .rounded(px(4.))
                    .bg(pal.hover)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            // "NAME", and beside it whether this series is one
                            // the chart draws. A card that says nothing is a
                            // card that implies "plotted", which on the extra
                            // series of a pie is the very impression that made
                            // the old save's data loss invisible.
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(
                                        div().text_size(px(10.)).text_color(pal.dim).child("NAME"),
                                    )
                                    .when(!series_is_plotted(&data.kind, si, n_series), |d| {
                                        d.child(
                                            div()
                                                .px(px(3.))
                                                .rounded(px(3.))
                                                .border_1()
                                                .border_color(pal.border)
                                                .text_size(px(9.))
                                                .text_color(pal.dim)
                                                .child("NOT PLOTTED"),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap(px(2.))
                                    .child(btn(
                                        format!("series-up-{si}"),
                                        "\u{25b2}",
                                        "Move up",
                                        si > 0,
                                        Box::new(move |t, cx2| t.series_reorder(si, -1, cx2)),
                                    ))
                                    .child(btn(
                                        format!("series-dn-{si}"),
                                        "\u{25bc}",
                                        "Move down",
                                        si + 1 < n_series,
                                        Box::new(move |t, cx2| t.series_reorder(si, 1, cx2)),
                                    ))
                                    .child(btn(
                                        format!("series-rm-{si}"),
                                        "\u{00d7}",
                                        "Remove series",
                                        n_series > 1,
                                        Box::new(move |t, cx2| t.series_delete(si, cx2)),
                                    )),
                            ),
                    )
                    .child(self.ref_field(
                        format!("series-name-{si}"),
                        RefTarget::SeriesName(si),
                        series_name_shown(&sr.name, sr.name_ref.as_deref()),
                        name_hint.clone(),
                        "",
                        cx,
                    ))
                    .child(
                        div()
                            .pt(px(2.))
                            .text_size(px(10.))
                            .text_color(pal.dim)
                            .child("VALUES"),
                    )
                    .child(self.ref_field(
                        format!("series-vals-{si}"),
                        RefTarget::SeriesValues(si),
                        vals,
                        vals_hint.clone(),
                        "",
                        cx,
                    ))
                    .child(swatches),
            );
        }

        let range_shown = data
            .source
            .as_ref()
            .map(source_ref_text)
            .unwrap_or_default();
        v_flex()
            .id("chart-panel")
            .w(px(SIDE_PANEL_W))
            .h_full()
            .flex_none()
            .bg(pal.panel)
            .border_l_1()
            .border_color(pal.border)
            // The panel is opaque to the mouse: a press that lands on it (or on
            // the gaps between its controls) must not reach the grid behind and
            // move the cell selection out from under the chart being edited.
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, {
                let ent_up = ent.clone();
                move |_ev, _w, cx| {
                    cx.stop_propagation();
                    ent_up.update(cx, |this, cx| {
                        if let Some(f) = &mut this.range_edit {
                            f.dragging = false;
                        }
                        // The panel eats this event, so the root never sees it:
                        // end a grid drag that was released over the panel here.
                        this.grid_release(cx);
                    });
                }
            })
            .on_mouse_move(|_ev, _w, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(pal.fg)
                            .child("Chart"),
                    )
                    .child({
                        let ent_x = ent.clone();
                        div()
                            .id("chart-panel-close")
                            .w(px(18.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_size(px(13.))
                            .text_color(pal.dim)
                            .hover(|d| d.bg(pal.hover).text_color(pal.fg))
                            .tooltip(|w, cx2| {
                                gpui_component::tooltip::Tooltip::new(
                                    "Close \u{2014} deselects the chart",
                                )
                                .build(w, cx2)
                            })
                            .child("\u{00d7}")
                            .on_click(move |_ev, _w, cx2| {
                                ent_x.update(cx2, |this, cx2| {
                                    this.chart_sel = None;
                                    // One of the two deliberate ways out: the
                                    // panel closes here rather than sticking.
                                    this.chart_panel_event(PanelEvent::Dismiss);
                                    this.range_edit = None;
                                    this.ref_msg = None;
                                    this.range_pick = None;
                                    cx2.notify();
                                });
                            })
                    }),
            )
            .child(
                v_flex()
                    .id("chart-panel-body")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px_3()
                    .pb_2()
                    .child(heading("DATA RANGE"))
                    .child(self.ref_field(
                        "chart-range",
                        RefTarget::ChartRange,
                        range_shown,
                        format!("e.g. {}", self.ref_example((0, 0, 4, 3))),
                        chart_range_help(data.by_row),
                        cx,
                    ))
                    .child(heading("TYPE"))
                    .child(types)
                    // What the picked type DRAWS, when that is less than what
                    // the chart holds. A pie keeps every series it is given —
                    // the save no longer drops them — and plots the first, so
                    // the panel says which of the cards below reach the screen.
                    // Only shown when there is something to say; a single-series
                    // pie, and every other kind, draw the lot.
                    .when_some(
                        chart_unplotted_note(&data.kind, data.series.len()),
                        |d, note| {
                            d.child(
                                div()
                                    .px_1()
                                    .pt(px(2.))
                                    .text_size(px(10.))
                                    .text_color(pal.dim)
                                    .child(note),
                            )
                        },
                    )
                    // The four buttons above are the only kinds we can WRITE,
                    // and only as a single clustered/standard plot group. A
                    // scatter/area/doughnut/radar chart — or a stacked or combo
                    // one — round-trips as its original part rather than being
                    // flattened on save, so edits here show on screen but don't
                    // reach the file until a type is picked. Say so.
                    //
                    // And say what picking one DOES to those edits, because for
                    // one class of chart it does not save them. A scatter or
                    // bubble whose series still hold nothing but points goes
                    // through `chart_reauthored`, which re-reads the chart from
                    // its DATA RANGE and REPLACES the series below — names,
                    // colours, re-points, additions and all — keeping only the
                    // title. (Relabelling it instead is what
                    // `chart_would_lose_points` exists to prevent: the save would
                    // write `<c:ptCount val="0"/>` over the plot.) Which of the
                    // three a click takes is invisible from here — re-point
                    // EVERY series of a scatter and it relabels, re-point only
                    // some and the whole chart is re-derived, and a box the
                    // re-derivation can't read refuses it outright — so the note
                    // asks `reread` and says which, rather than promising the
                    // edits below are what gets saved.
                    .when(!gridcore::xlsx::chart_is_writable(&data), |d| {
                        d.child(
                            div()
                                .px_1()
                                .pb(px(2.))
                                .text_size(px(10.))
                                .text_color(pal.dim)
                                .child(format!(
                                    "This {}chart is kept as Excel wrote it. Edits below show \
                                     here but {}",
                                    // Name the kind only when the KIND is what
                                    // holds it back; "This stacked chart" would
                                    // read as a kind we don't have a word for.
                                    if data.complex || data.kind.is_empty() {
                                        String::new()
                                    } else {
                                        format!("{} ", data.kind)
                                    },
                                    // The answer `chart_set_kind` will give,
                                    // not a proxy for it — see `reread`, which
                                    // is the very call the click makes.
                                    match &reread {
                                        // A relabel: the type button makes the
                                        // chart writable and the next save
                                        // writes the cards below as they stand.
                                        // True of every chart the writer won't
                                        // save for a reason other than its
                                        // points.
                                        None => "are not saved until you pick a type above.",
                                        // A re-read: the cards below are
                                        // replaced wholesale by the box's, so
                                        // the note says so rather than
                                        // promising they get saved.
                                        Some(Ok(())) =>
                                            "are not saved until you pick a type above, which \
                                             re-reads it from its data range and replaces the \
                                             series below.",
                                        // A refusal. The click's own status
                                        // line names WHICH of the five it is;
                                        // the note names the remedy they share,
                                        // because sending the user to a button
                                        // that can only refuse is what the
                                        // `data.source.is_some()` proxy did.
                                        Some(Err(_)) =>
                                            "are not saved, and picking a type above can't save \
                                             them either until DATA RANGE names cells this chart \
                                             can be re-read from.",
                                    }
                                )),
                        )
                    })
                    // BELOW the note deliberately. Switch Row/Column is an edit
                    // like any other: `chart_switch_row_column` carries
                    // `complex` and `part` across the flip, so on a stacked bar
                    // or an area/scatter/doughnut chart the plot visibly
                    // transposes on screen and the original part still
                    // round-trips on save. That is exactly what the note's first
                    // half says ("Edits below show here but are not saved until
                    // you pick a type above"), and above the note it would be the
                    // one control the wording excluded. The note's second half —
                    // that picking a type RE-READS a points-only chart from its
                    // range — is the one thing a flip is exempt from, and not
                    // because the re-read honours it: `chart_switch_row_column`
                    // re-derives THERE AND THEN, so every series comes back
                    // carrying a `values_ref` and the chart has left the
                    // points-only class `chart_would_lose_points` asks about.
                    // The later click merely relabels it, which is also why the
                    // second half stops being printed once the flip has landed.
                    .child(switch)
                    .child(heading("TITLE"))
                    .child(self.ref_field(
                        "chart-title",
                        RefTarget::ChartTitle,
                        data.title.clone(),
                        "Chart title".to_string(),
                        "",
                        cx,
                    ))
                    .child(heading("SERIES"))
                    .child(series_list)
                    .child({
                        let ent_add = ent.clone();
                        div()
                            .id("series-add")
                            .mt(px(4.))
                            .px_2()
                            .py(px(3.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .border_1()
                            .border_color(pal.border)
                            .text_color(pal.fg)
                            .hover(|d| d.bg(pal.hover))
                            .child("+ Add series")
                            .on_click(move |_ev, _w, cx2| {
                                ent_add.update(cx2, |this, cx2| this.series_add(cx2));
                            })
                    })
                    .child(heading("CATEGORY LABELS"))
                    .child(
                        self.ref_field(
                            "chart-cats",
                            RefTarget::Categories,
                            data.categories_ref
                                .as_ref()
                                .map(source_ref_text)
                                .unwrap_or_default(),
                            format!("e.g. {}", self.ref_example(ex.categories)),
                            "The cells labelling each point along the axis.",
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    /// Dispatch a spreadsheet ribbon command.
    fn run_sheet_act(&mut self, act: SheetAct, window: &mut Window, cx: &mut Context<Self>) {
        use gridcore::sheet::Align;
        // Before anything reads the selection — including the bar seeding below
        // — the grid takes it back, so a command that acts on cells acts on
        // cells the user can see. `chart_hand_back` returns at once when no
        // chart is selected, so this costs nothing in the ordinary case.
        if act_targets_cells(act) {
            self.chart_hand_back(cx);
        }
        // An action that opens a bar with a range field seeds that field from
        // the selection first, so the bar starts on the cells it always used.
        if let Some(target) = bar_target(act) {
            self.bar_open(target);
        }
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
    fn sheet_key(
        &mut self,
        ev: &KeyDownEvent,
        ctrl: bool,
        shift: bool,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An inline sheet-tab rename swallows all typing until Enter/Esc.
        if self.sheet_rename.is_some() {
            return self.sheet_rename_key(ev, key, cx);
        }
        // A bar's range field is asked before the bar it sits in, or the bar's
        // own buffer would eat what is typed into the field. Ctrl chords stay
        // with the sheet — except Ctrl+A, which the focused field reads as
        // "select this text", not "select the used range".
        let bar_field = matches!(&self.range_edit, Some(f) if f.target.is_bar());
        if bar_field && (!ctrl || key == "a") {
            return self.range_edit_key(ev, ctrl, shift, key, cx);
        }
        // A chord the field just declined belongs to the SHEET. Letting it reach
        // the bar handlers below instead would only get it dropped — the bar's
        // own buffer takes plain typing, not chords — so `Ctrl+C`/`V`/`Z`/`S`
        // would do nothing at all while a bar's range field had focus.
        let to_bar = |open: bool| open && !bar_field;
        // The comment entry bar swallows typing until Enter (commit) / Esc.
        if self.sheet_comment_edit.is_some() {
            return self.sheet_comment_key(ev, key, cx);
        }
        // The conditional-format entry bar likewise swallows typing.
        if to_bar(self.sheet_cf_edit.is_some()) {
            return self.sheet_cf_key(ev, key, cx);
        }
        // The data-validation entry bar swallows typing too.
        if to_bar(self.sheet_dv_edit.is_some()) {
            return self.sheet_dv_edit_key(ev, key, cx);
        }
        // The AutoFilter criteria bar swallows typing too.
        if self.sheet_filter_edit.is_some() {
            return self.sheet_filter_key(ev, key, cx);
        }
        // The Text-to-Columns delimiter bar swallows typing too.
        if to_bar(self.sheet_ttc_edit.is_some()) {
            return self.sheet_ttc_key(ev, key, cx);
        }
        // The multi-level sort spec bar swallows typing too.
        if to_bar(self.sheet_sort_edit.is_some()) {
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
        // Same as the bar fields above: a focused Chart-panel field owns Ctrl+A.
        // Without this it falls through to the sheet's select-all below, and the
        // field's own handler is dead code.
        if ctrl && key == "a" && self.range_edit.is_some() {
            return self.range_edit_key(ev, ctrl, shift, key, cx);
        }
        if ctrl {
            // The Ctrl keys that act on the CELLS take the selection back
            // first, for the same reason the arrows do — see `chart_hand_back`.
            // Undo and redo are absent because they drop the chart selection
            // themselves (the chart list moves under them); the rest of the
            // block acts on the document or the window, not on the selection.
            if matches!(key, "c" | "x" | "v" | "a" | "b" | "i")
                || (shift && matches!(key, "p" | "k"))
            {
                self.chart_hand_back(cx);
            }
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
        // A Chart-panel text field swallows typing until Enter/Esc.
        if self.range_edit.is_some() {
            return self.range_edit_key(ev, ctrl, shift, key, cx);
        }
        // A selected chart takes the object keys (Escape drops it, Delete removes
        // it) before they reach the grid.
        if self.chart_sel.is_some() && !editing {
            match key {
                "escape" => {
                    self.chart_sel = None;
                    self.chart_panel_event(PanelEvent::Dismiss);
                    cx.notify();
                    return;
                }
                "delete" | "backspace" => return self.chart_delete_selected(cx),
                // The keys the grid acts on below take the selection BACK
                // first, the same way a click on a cell does: the navigation
                // keys move it, F2 opens an edit in it. Without this they would
                // move or edit a cell selection the chart is hiding — something
                // you cannot see happening, which is exactly the "who has the
                // keyboard" confusion this rule exists to end. The key then
                // falls through to the grid, which acts on the selection it has
                // just been given.
                "left" | "right" | "up" | "down" | "enter" | "f2" => self.chart_hand_back(cx),
                // A printable character starts a fresh edit in the selected
                // cell, so it is as much a press on the cells as F2 is. Every
                // other key — a lone modifier, a function key the grid ignores
                // — leaves the chart selected, because it changes nothing about
                // the selection either way. Tab is NOT in that set: gpui
                // swallows it for focus traversal, so it arrives as an action
                // (`tab_key` / `shift_tab_key`) and never reaches here, and it
                // states the hand-back for itself.
                _ if ev
                    .keystroke
                    .key_char
                    .as_deref()
                    .and_then(|c| c.chars().next())
                    .is_some_and(|ch| !ch.is_control()) =>
                {
                    self.chart_hand_back(cx)
                }
                _ => {}
            }
        }
        match key {
            "escape" => {
                self.sheet_pick = None;
                self.chart_sel = None;
                // Escape reaches here when the chart is already deselected and
                // only the sticky panel is left, and it is the keyboard's way
                // of shutting that panel — the same dismissal as its close box.
                self.chart_panel_event(PanelEvent::Dismiss);
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
            // Same reason as `select_sheet`: these all index the document we
            // were just on.
            self.drop_grid_state();
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
        // Whatever tab we land on, the state below belonged to another one.
        self.drop_grid_state();
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
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.hf_edit.is_some())
    }

    /// Enter header (or footer) edit mode: resolve the existing part or create a
    /// fresh one, parse its blocks into an editor, and switch to Print Layout so
    /// the margin area is visible. No-op for markdown/package-less tabs.
    fn enter_hf(
        &mut self,
        is_header: bool,
        variant: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_hf(); // commit any header/footer already open
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
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
        tab.hf_edit = Some(HfEdit {
            editor: Editor::new(doc),
            part_name,
            is_header,
            variant,
        });
        self.page_view = true;
        if let Some(t) = self.tabs.get_mut(idx) {
            let region = if is_header { "header" } else { "footer" };
            let vlabel = match variant {
                "first" => "first-page ",
                "even" => "even-page ",
                _ => "",
            };
            t.status =
                format!("Editing {vlabel}{region} — press Esc to return to the document").into();
        }
        self.refocus(window, cx);
    }

    /// Serialize the open header/footer editor back into its package part (called
    /// on exit and before every save) so edits persist. Leaves the session open.
    fn flush_hf(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Some(hf) = tab.hf_edit.as_ref() else {
            return;
        };
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
            if let Some((is_h, "first")) = self
                .tabs
                .get(idx)
                .and_then(|t| t.hf_edit.as_ref())
                .map(|h| (h.is_header, h.variant))
            {
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
            if let Some((is_h, "even")) = self
                .tabs
                .get(idx)
                .and_then(|t| t.hf_edit.as_ref())
                .map(|h| (h.is_header, h.variant))
            {
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
        let (is_header, variant) = hf
            .map(|h| (h.is_header, h.variant))
            .unwrap_or((true, "default"));
        let title_pg = tab
            .and_then(|t| t.pkg.as_ref())
            .is_some_and(|p| p.has_title_pg());
        let even_odd = tab
            .and_then(|t| t.pkg.as_ref())
            .is_some_and(|p| p.has_even_odd());
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
                        .bg(if on {
                            hsla_u(BRAND)
                        } else {
                            Hsla { a: 0., ..pal.fg }
                        })
                        .when(on, |d| {
                            d.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(rgb(0xffffff))
                                    .child("\u{2713}"),
                            )
                        }),
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
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .min_w(px(96.))
                    .child("Header & Footer"),
            )
            .child(
                pill("hf-hdr", "Header".into(), is_header)
                    .on_click(cx.listener(move |t, _, w, c| t.enter_hf(true, variant, w, c))),
            )
            .child(
                pill("hf-ftr", "Footer".into(), !is_header)
                    .on_click(cx.listener(move |t, _, w, c| t.enter_hf(false, variant, w, c))),
            )
            .child(sep())
            .child(
                pill("hf-def", "Default".into(), variant == "default").on_click(
                    cx.listener(move |t, _, w, c| t.enter_hf(is_header, "default", w, c)),
                ),
            )
            .when(title_pg, |d| {
                d.child(
                    pill("hf-first", "First page".into(), variant == "first").on_click(
                        cx.listener(move |t, _, w, c| t.enter_hf(is_header, "first", w, c)),
                    ),
                )
            })
            .when(even_odd, |d| {
                d.child(
                    pill("hf-even", "Even".into(), variant == "even").on_click(
                        cx.listener(move |t, _, w, c| t.enter_hf(is_header, "even", w, c)),
                    ),
                )
            })
            .child(sep())
            .child(
                check("hf-tp", "Different First Page", title_pg)
                    .on_click(cx.listener(|t, _, w, c| t.toggle_title_pg(w, c))),
            )
            .child(
                check("hf-eo", "Different Odd & Even", even_odd)
                    .on_click(cx.listener(|t, _, w, c| t.toggle_even_odd(w, c))),
            )
            .child(div().flex_1())
            .child(
                pill("hf-close", "Close".into(), false)
                    .on_click(cx.listener(|t, _, w, c| t.exit_hf(w, c))),
            )
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
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Surface::Doc(editor) = &tab.surface else {
            return;
        };
        // Markdown-backed tabs save as Markdown; everything else as lossless .docx.
        let bytes = if tab.markdown {
            docxcore::markdown::to_markdown(&editor.doc).into_bytes()
        } else {
            doc_to_docx(&editor.doc, &tab.comments, tab.pkg.as_ref())
        };
        let path = tab.path.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .join(tab.title.to_string())
        });
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
            let Some(tab) = self.tabs.get(self.active) else {
                return;
            };
            let Surface::Sheet(v) = &tab.surface else {
                return;
            };
            (sheet_bytes(v), tab.path.clone(), tab.title.to_string())
        };
        // A never-saved workbook asks where to go (Excel-style), instead of
        // silently dumping into the working directory.
        let path = match existing_path {
            Some(p) => p,
            None => match rfd::FileDialog::new()
                .add_filter("Excel workbook", &["xlsx"])
                .set_file_name(title)
                .save_file()
            {
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
        let start = self
            .tabs
            .get(self.active)
            .map(|t| t.title.to_string())
            .unwrap_or_else(|| "Untitled.docx".into());
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
            self.drop_grid_state();
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
        let canon =
            |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let mut changed = false;
        for path in paths {
            let key = canon(&path);
            match self
                .tabs
                .iter()
                .position(|t| t.path.as_deref().map(canon) == Some(key.clone()))
            {
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
            // The tab under the panel/bar state just changed (or was replaced).
            self.drop_grid_state();
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
    fn set_caret(
        &mut self,
        path: Vec<usize>,
        offset: usize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    fn begin_select(
        &mut self,
        path: Vec<usize>,
        offset: usize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn with_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Editor),
    ) {
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
            let c = if cut {
                dirty = true;
                ed.cut()
            } else {
                ed.copy()
            };
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
        self.picker = if self.picker == Some(kind) {
            None
        } else {
            Some(kind)
        };
        self.refocus(window, cx);
    }

    fn apply_color(&mut self, hex: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_color(hex));
    }

    fn apply_highlight(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let filename = self
            .tabs
            .get(self.active)
            .map(|t| t.title.to_string())
            .unwrap_or_default();
        let mut props = docxcore::field::DocProps::default();
        props.author = "docxy".to_string();
        docxcore::field::FieldContext {
            now,
            props,
            filename,
        }
    }

    /// Insert a field (`<w:fldSimple>`) with its computed value at the caret.
    fn insert_field(
        &mut self,
        instr: &'static str,
        fallback: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker = None;
        let ctx = self.field_context();
        let val =
            docxcore::field::eval_field_ctx(instr, &ctx).unwrap_or_else(|| fallback.to_string());
        let raw = format!(
            "<w:fldSimple w:instr=\"{}\"><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:fldSimple>",
            xml_escape(instr),
            xml_escape(&val)
        );
        self.with_editor(window, cx, |e| {
            e.paste(&Clip {
                paras: vec![vec![Inline::Field { raw, text: val }]],
            })
        });
    }

    fn insert_page_break(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_editor(window, cx, |e| {
            e.paste(&Clip {
                paras: vec![vec![Inline::Break(docxcore::model::BreakKind::Page)]],
            })
        });
    }

    /// Insert one symbol/special character at the caret (Insert ▸ Symbol).
    fn insert_symbol(&mut self, s: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let s = s.to_string();
        self.with_editor(window, cx, move |e| e.insert_str(&s));
    }

    /// Insert an inline math equation from a LaTeX template (Insert ▸ Equation).
    fn insert_equation(
        &mut self,
        latex: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                tab.status = if on {
                    "Automatic hyphenation: on".into()
                } else {
                    "Automatic hyphenation: off".into()
                };
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
                (p.len() >= 3 && matches!(ed.doc.body.get(p[0]), Some(Block::Table(_))))
                    .then(|| (p[0], p[1], p[2]))
            }
            _ => None,
        }
    }

    /// An empty table cell (one blank paragraph).
    fn empty_cell() -> docxcore::model::Cell {
        docxcore::model::Cell {
            grid_span: 1,
            v_merge: docxcore::model::VMerge::None,
            blocks: vec![Block::Paragraph(Paragraph::default())],
            raw_tcpr: None,
        }
    }

    /// Run a Table Tools operation relative to the caret's cell.
    fn table_op(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        use Act::*;
        let Some((tb, row, col)) = self.caret_table() else {
            return self.refocus(window, cx);
        };
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                if let Some(Block::Table(table)) = ed.doc.body.get_mut(tb) {
                    let ncols = table
                        .grid
                        .len()
                        .max(table.rows.first().map_or(0, |r| r.cells.len()));
                    match act {
                        RowAbove | RowBelow => {
                            let at = if matches!(act, RowAbove) {
                                row
                            } else {
                                row + 1
                            };
                            let new = docxcore::model::Row {
                                cells: (0..ncols).map(|_| Self::empty_cell()).collect(),
                                raw_props: vec![],
                            };
                            table.rows.insert(at.min(table.rows.len()), new);
                            ed.caret = Caret::at(
                                vec![tb, at.min(table.rows.len() - 1), col.min(ncols - 1), 0],
                                0,
                            );
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
                                ed.caret =
                                    Caret::at(vec![tb, row.min(nr - 1), col.min(ncols - 1), 0], 0);
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
    fn insert_table(
        &mut self,
        rows: usize,
        cols: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let mk_cell = || Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![Block::Paragraph(Paragraph::default())],
            raw_tcpr: None,
        };
        let mk_row = || Row {
            cells: (0..cols).map(|_| mk_cell()).collect(),
            raw_props: vec![],
        };
        let table = Table {
            grid: vec![col_w; cols],
            rows: (0..rows).map(|_| mk_row()).collect(),
            raw_tblpr: Some(TBLPR.to_string()),
        };
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                let at = ed
                    .caret
                    .path
                    .first()
                    .copied()
                    .unwrap_or(0)
                    .min(ed.doc.body.len().saturating_sub(1));
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
            div()
                .size(px(20.))
                .rounded(px(3.))
                .border_1()
                .border_color(if ring { pal.fg } else { pal.border })
                .bg(bg)
                .cursor_pointer()
                .hover(|d| d.border_color(hsla_u(BRAND)))
        };
        let mut row = h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_1p5()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border);
        row = row.child(
            div()
                .text_size(px(11.))
                .text_color(pal.dim)
                .min_w(px(78.))
                .child(match kind {
                    PickKind::Color => "Font colour",
                    PickKind::Highlight => "Highlight",
                    PickKind::FontName => "Font",
                    PickKind::FontSize => "Size",
                    PickKind::Field => "Field",
                    PickKind::Table => "Table",
                    PickKind::Symbol => "Symbol",
                    PickKind::LineSpacing => "Line spacing",
                    PickKind::Equation => "Equation",
                }),
        );
        let chip = |id_key: usize, label: SharedString, tag: &'static str| {
            div()
                .id((tag, id_key))
                .flex()
                .items_center()
                .px_2()
                .h(px(22.))
                .rounded(px(3.))
                .text_size(px(12.))
                .text_color(pal.fg)
                .border_1()
                .border_color(pal.border)
                .cursor_pointer()
                .hover(|d| d.bg(pal.hover).border_color(hsla_u(BRAND)))
                .child(label)
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
                        .on_click(
                            cx.listener(|this, _, window, cx| this.apply_color(None, window, cx)),
                        ),
                );
                for &c in COLOR_SWATCHES {
                    let hex = format!("{c:06X}");
                    row = row.child(
                        swatch(hsla_u(c), c == 0xFFFFFF)
                            .id(("col", c as usize))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.apply_color(Some(hex.clone()), window, cx)
                            })),
                    );
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
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.apply_highlight(None, window, cx)
                        })),
                );
                for (i, &name) in HIGHLIGHT_SWATCHES.iter().enumerate() {
                    let (c, _) = highlight_rgb(name);
                    row = row.child(swatch(hsla_u(c), false).id(("hl", i)).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.apply_highlight(Some(name.to_string()), window, cx)
                        },
                    )));
                }
            }
            PickKind::FontName => {
                for (i, &name) in FONT_NAMES.iter().enumerate() {
                    // Preview each name in its own font family.
                    row = row.child(chip(i, name.into(), "fn").font_family(name).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.apply_font(name.to_string(), window, cx)
                        }),
                    ));
                }
            }
            PickKind::FontSize => {
                for (i, &pts) in FONT_SIZES.iter().enumerate() {
                    row = row.child(chip(i, pts.to_string().into(), "fs").on_click(
                        cx.listener(move |this, _, window, cx| this.apply_size(pts, window, cx)),
                    ));
                }
            }
            PickKind::Field => {
                for (i, &(label, instr, fallback)) in FIELDS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "fld").on_click(cx.listener(
                        move |this, _, window, cx| this.insert_field(instr, fallback, window, cx),
                    )));
                }
            }
            PickKind::Table => {
                for (i, &(label, r, c)) in TABLE_PRESETS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "tbl").on_click(
                        cx.listener(move |this, _, window, cx| this.insert_table(r, c, window, cx)),
                    ));
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.insert_symbol(s, window, cx)
                            })),
                    );
                }
            }
            PickKind::LineSpacing => {
                // Word's Line Spacing menu: the multiples with the current one lit,
                // then Add/Remove space before/after the paragraph.
                let (cur, has_before, has_after) =
                    match self.tabs.get(self.active).map(|t| &t.surface) {
                        Some(Surface::Doc(ed)) => (
                            ed.caret_line_multiple(),
                            ed.caret_space_before().unwrap_or(0) > 0,
                            ed.caret_space_after().unwrap_or(0) > 0,
                        ),
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.apply_line_spacing(line, window, cx)
                            })),
                    );
                }
                row = row.child(div().w(px(1.)).h(px(16.)).bg(pal.border));
                let before = if has_before {
                    "Remove space before"
                } else {
                    "Add space before"
                };
                let after = if has_after {
                    "Remove space after"
                } else {
                    "Add space after"
                };
                row = row.child(chip(100, before.into(), "lsb").on_click(
                    cx.listener(|this, _, window, cx| this.toggle_space_before(window, cx)),
                ));
                row = row.child(chip(101, after.into(), "lsa").on_click(
                    cx.listener(|this, _, window, cx| this.toggle_space_after(window, cx)),
                ));
            }
            PickKind::Equation => {
                for (i, &(label, latex)) in EQUATIONS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "eq").on_click(cx.listener(
                        move |this, _, window, cx| this.insert_equation(latex, window, cx),
                    )));
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
            .map(|t| {
                t.comments
                    .iter()
                    .filter_map(|c| c.id.parse::<i32>().ok())
                    .max()
                    .map(|m| m + 1)
                    .unwrap_or(1)
            })
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
                t.comments.push(Comment {
                    id: id.to_string(),
                    author,
                    initials: "D".into(),
                    date: String::new(),
                    text,
                    quoted,
                });
                t.dirty = true;
                t.status = format!("Comment {id} added").into();
            }
        }
        self.refocus(window, cx);
    }

    /// Route a keystroke to the comment entry bar while it is open.
    fn comment_key(
        &mut self,
        ev: &KeyDownEvent,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let geom = self
            .tabs
            .get(self.active)
            .and_then(|t| t.pkg.as_ref())
            .map(|p| p.page_geom())
            .unwrap_or_default();
        self.ruler_drag = Some(RulerDrag {
            handle,
            start_x: x,
            start_indent: indent,
            start_first: first,
            start_right: right,
            start_ml: geom.ml,
            start_mr: geom.mr,
        });
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
        let geom = self
            .tabs
            .get(self.active)
            .and_then(|t| t.pkg.as_ref())
            .map(|p| p.page_geom())
            .unwrap_or_default();
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
        let geom = tab
            .and_then(|t| t.pkg.as_ref())
            .map(|p| p.page_geom())
            .unwrap_or_default();
        let (indent, first_line, indent_right, tabs): (i32, i32, i32, Vec<TabStop>) =
            match tab.map(|t| &t.surface) {
                Some(Surface::Doc(ed)) => {
                    let (i, f) = ed.caret_para_indent();
                    (
                        i,
                        f,
                        ed.caret_para_right_indent(),
                        ed.caret_para_props().tabs.clone(),
                    )
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
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(x(ml), top + px(3.)),
                        point(x(content_r), top + px(h - 3.)),
                    ),
                    white,
                ));
                // Tick marks every 1/8", taller each inch, from the left margin.
                let step = 96.0 / 8.0;
                let mut i = 0;
                let mut xx = ml;
                while xx <= content_r + 0.5 {
                    let major = i % 8 == 0;
                    let th = if major {
                        h * 0.34
                    } else if i % 4 == 0 {
                        h * 0.24
                    } else {
                        h * 0.15
                    };
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(x(xx), top + px((h - th) * 0.5)),
                            point(x(xx + 1.0), top + px((h + th) * 0.5)),
                        ),
                        tick,
                    ));
                    xx += step;
                    i += 1;
                }
                // Default tab stops (every 0.5") as tiny ticks along the baseline.
                let mut tx = ml + 48.0;
                while tx <= content_r {
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(x(tx), top + px(h - 4.)),
                            point(x(tx + 1.0), top + px(h - 2.)),
                        ),
                        hsla_u(0x999999),
                    ));
                    tx += 48.0;
                }
                // Custom tab stops (from the paragraph) as L / ⊥ / ⌐ markers.
                for t in &tabs {
                    let sx = x(ml + t.pos as f32 / d);
                    let yb = top + px(h - 5.);
                    // vertical stem
                    window.paint_quad(fill(
                        Bounds::from_corners(point(sx, top + px(h - 11.)), point(sx + px(1.5), yb)),
                        dim,
                    ));
                    // foot direction encodes alignment
                    let (fx0, fx1) = match t.align {
                        TabAlign::Left => (0.0, 5.0),
                        TabAlign::Right => (-5.0, 0.0),
                        TabAlign::Center => (-3.0, 3.0),
                    };
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(sx + px(fx0), yb - px(1.5)),
                            point(sx + px(fx1), yb),
                        ),
                        dim,
                    ));
                }
                let z = point(0.0_f32, 0.0);
                // First-line indent — downward triangle at the top.
                let flx = ml + ind + fl;
                let mut t1 = Path::new(point(x(flx - 5.0), top + px(1.)));
                t1.push_triangle(
                    (
                        point(x(flx - 5.0), top + px(1.)),
                        point(x(flx + 5.0), top + px(1.)),
                        point(x(flx), top + px(8.)),
                    ),
                    (z, z, z),
                );
                window.paint_path(t1, brand);
                // Left / other-rows indent — upward triangle + a square below it.
                let lx = ml + ind;
                let by = top + px(h - 1.);
                let mut t2 = Path::new(point(x(lx - 5.0), by - px(4.)));
                t2.push_triangle(
                    (
                        point(x(lx - 5.0), by - px(4.)),
                        point(x(lx + 5.0), by - px(4.)),
                        point(x(lx), by - px(11.)),
                    ),
                    (z, z, z),
                );
                window.paint_path(t2, brand);
                window.paint_quad(fill(
                    Bounds::from_corners(point(x(lx - 4.0), by - px(4.)), point(x(lx + 4.0), by)),
                    brand,
                ));
                // Right indent — upward triangle, positioned in from the right
                // margin by the paragraph's right indent.
                let rx = right_marker;
                let mut t3 = Path::new(point(x(rx - 5.0), by));
                t3.push_triangle(
                    (
                        point(x(rx - 5.0), by),
                        point(x(rx + 5.0), by),
                        point(x(rx), top + px(h - 8.)),
                    ),
                    (z, z, z),
                );
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
            numbers = numbers.child(
                div()
                    .absolute()
                    .left(px(xx - 3.0))
                    .top(px(4.0))
                    .text_size(px(8.))
                    .text_color(hsla_u(0x555555))
                    .child(SharedString::from(inch.to_string())),
            );
            inch += 1;
        }

        // Draggable indent handles, split into a TOP band (first-line marker) and
        // a BOTTOM band (left / right markers) so they never overlap where they
        // share an x (e.g. a paragraph with no indent) and each stays grabbable.
        let handle = |id: &'static str,
                      cx_px: f32,
                      top_px: f32,
                      h_px: f32,
                      which: RulerHandle,
                      cxx: &mut Context<Self>| {
            div()
                .id(id)
                .absolute()
                .left(px(cx_px - 6.0))
                .top(px(top_px))
                .w(px(12.))
                .h(px(h_px))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cxx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.ruler_drag_start(which, f32::from(ev.position.x), cx);
                    }),
                )
        };

        // Margin grab strips sit in the grey zone just OUTSIDE the white content
        // area (Word's margin boundary), clear of the indent markers.
        let margin_handle =
            |id: &'static str, left_px: f32, which: RulerHandle, cxx: &mut Context<Self>| {
                div()
                    .id(id)
                    .absolute()
                    .left(px(left_px))
                    .top(px(0.))
                    .w(px(8.))
                    .h(px(h))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cxx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.ruler_drag_start(which, f32::from(ev.position.x), cx);
                        }),
                    )
            };

        let container = div()
            .relative()
            .w(px(pw))
            .h(px(h))
            .child(paint.size_full())
            .child(numbers)
            // Click the content area to add/remove a tab stop of the current type.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.ruler_click_tab(f32::from(ev.position.x), window, cx);
                }),
            )
            .child(margin_handle(
                "rh-mleft",
                ml - 8.0,
                RulerHandle::MarginLeft,
                cx,
            ))
            .child(margin_handle(
                "rh-mright",
                content_r,
                RulerHandle::MarginRight,
                cx,
            ))
            .child(handle(
                "rh-first",
                ml + ind + fl,
                0.0,
                h * 0.5,
                RulerHandle::FirstLine,
                cx,
            ))
            .child(handle(
                "rh-left",
                ml + ind,
                h * 0.5,
                h * 0.5,
                RulerHandle::Left,
                cx,
            ))
            .child(handle(
                "rh-right",
                right_marker,
                h * 0.5,
                h * 0.5,
                RulerHandle::Right,
                cx,
            ));

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
        let geom = self
            .tabs
            .get(self.active)
            .and_then(|t| t.pkg.as_ref())
            .map(|p| p.page_geom())
            .unwrap_or_default();
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
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(left + px(3.), y(mt)),
                        point(left + px(w - 3.), y((hh - mb).max(mt))),
                    ),
                    white,
                ));
                // ticks every 1/8", taller each inch, measured from the top margin.
                let step = 96.0 / 8.0;
                let mut i = 0;
                let mut yy = mt;
                while yy <= hh - mb + 0.5 {
                    let major = i % 8 == 0;
                    let tw = if major {
                        w * 0.42
                    } else if i % 4 == 0 {
                        w * 0.30
                    } else {
                        w * 0.18
                    };
                    let x1 = left + px((w - tw) * 0.5);
                    let x2 = left + px((w + tw) * 0.5);
                    window.paint_quad(fill(
                        Bounds::from_corners(point(x1, y(yy)), point(x2, y(yy + 1.0))),
                        tick,
                    ));
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
            numbers = numbers.child(
                div()
                    .absolute()
                    .top(px(yy - 5.0))
                    .left(px(4.0))
                    .text_size(px(8.))
                    .text_color(hsla_u(0x555555))
                    .child(SharedString::from(inch.to_string())),
            );
            inch += 1;
        }
        div()
            .relative()
            .w(px(18.))
            .flex_none()
            .child(paint.size_full())
            .child(numbers)
            .into_any_element()
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
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .child("New comment"),
            )
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
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(pal.fg)
                            .child(SharedString::from(self.comment_text.clone())),
                    )
                    .child(caret_bar()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .child("Enter to add · Esc to cancel"),
            )
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
        let comments = self
            .tabs
            .get(self.active)
            .map(|t| t.comments.clone())
            .unwrap_or_default();
        let mut list = v_flex()
            .id("cmt-list")
            .flex_1()
            .overflow_y_scroll()
            .gap_2()
            .p_2();
        if comments.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .p_2()
                    .child("No comments. Select text, then Review \u{203A} New comment."),
            );
        }
        for c in &comments {
            let id = c.id.clone();
            let quoted = c.quoted.clone();
            list =
                list.child(
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
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(hsla_u(BRAND))
                                        .child(SharedString::from(c.author.clone())),
                                )
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
                        .when(!c.quoted.is_empty(), |d| {
                            d.child(
                                div().text_size(px(11.)).italic().text_color(pal.dim).child(
                                    SharedString::from(format!("\u{201C}{}\u{201D}", c.quoted)),
                                ),
                            )
                        })
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(pal.fg)
                                .child(SharedString::from(c.text.clone())),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.goto_comment(quoted.clone(), window, cx)
                        })),
                );
        }
        v_flex()
            .w(px(280.))
            .h_full()
            .border_l_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(pal.fg)
                    .border_b_1()
                    .border_color(pal.border)
                    .child(SharedString::from(format!("Comments ({})", comments.len()))),
            )
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
        let headings: Vec<(usize, u8, String)> =
            match self.tabs.get(self.active).map(|t| &t.surface) {
                Some(Surface::Doc(ed)) => ed
                    .doc
                    .body
                    .iter()
                    .enumerate()
                    .filter_map(|(i, b)| match b {
                        Block::Paragraph(p) => {
                            p.props.heading_level.map(|lvl| (i, lvl, p.plain_text()))
                        }
                        _ => None,
                    })
                    .filter(|(_, _, t)| !t.trim().is_empty())
                    .collect(),
                _ => vec![],
            };
        let mut list = v_flex()
            .id("nav-list")
            .flex_1()
            .overflow_y_scroll()
            .gap_0p5()
            .p_2();
        if headings.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .p_2()
                    .child("No headings."),
            );
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
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.goto_block(block, window, cx)),
                    ),
            );
        }
        v_flex()
            .w(px(240.))
            .h_full()
            .border_r_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(pal.fg)
                    .border_b_1()
                    .border_color(pal.border)
                    .child("Navigation"),
            )
            .child(list)
            .into_any_element()
    }

    /// The footnotes/endnotes side panel (display-only).
    fn notes_panel(&self, pal: Pal, _cx: &mut Context<Self>) -> AnyElement {
        let notes = self
            .tabs
            .get(self.active)
            .map(|t| t.notes.clone())
            .unwrap_or_default();
        let mut list = v_flex()
            .id("notes-list")
            .flex_1()
            .overflow_y_scroll()
            .gap_2()
            .p_2();
        if notes.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .p_2()
                    .child("No footnotes or endnotes."),
            );
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
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(hsla_u(BRAND))
                            .child(SharedString::from(format!("{tag} {}", n.id))),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(pal.fg)
                            .child(SharedString::from(n.text.clone())),
                    ),
            );
        }
        v_flex()
            .w(px(280.))
            .h_full()
            .border_l_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(pal.fg)
                    .border_b_1()
                    .border_color(pal.border)
                    .child(SharedString::from(format!("Notes ({})", notes.len()))),
            )
            .child(list)
            .into_any_element()
    }

    /// Route a keystroke to the find bar while it is open.
    fn find_key(
        &mut self,
        ev: &KeyDownEvent,
        shift: bool,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                let field = if f == FindField::Query {
                    &mut self.find_query
                } else {
                    &mut self.replace_text
                };
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
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(pal.fg)
                        .child(SharedString::from(text.to_string())),
                )
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
            .child(
                field(
                    "Find",
                    &self.find_query,
                    self.find_field == FindField::Query,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.find_field = FindField::Query;
                        cx.notify();
                    }),
                ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .min_w(px(68.))
                    .child(SharedString::from(count_txt)),
            )
            .child(
                icon_btn("f-prev", "\u{2191}", false)
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(true, false, cx))),
            )
            .child(
                icon_btn("f-next", "\u{2193}", false)
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(false, false, cx))),
            )
            .child(div().w(px(1.)).h(px(18.)).bg(pal.border))
            .child(
                field(
                    "Replace",
                    &self.replace_text,
                    self.find_field == FindField::Replace,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.find_field = FindField::Replace;
                        cx.notify();
                    }),
                ),
            )
            .child(
                text_btn("f-rep", "Replace")
                    .on_click(cx.listener(|this, _, _, cx| this.replace_one(cx))),
            )
            .child(
                text_btn("f-all", "All")
                    .on_click(cx.listener(|this, _, _, cx| this.replace_all_now(cx))),
            )
            .child(div().flex_1())
            .child(
                icon_btn("f-case", "Aa", self.find_case).on_click(cx.listener(|this, _, _, cx| {
                    this.find_case = !this.find_case;
                    this.find_step(false, true, cx);
                })),
            )
            .child(
                icon_btn("f-close", "\u{00d7}", false)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_find(window, cx))),
            )
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
            // A Chart-panel range field swallows Tab the same way it swallows
            // every other key, and for the same reason: it has the keyboard.
            // `sheet_key` states that first (`range_edit.is_some()` routes to
            // `range_edit_key` BEFORE the hand-back arm), and Tab is
            // action-bound, so it has to be stated again here — otherwise Tab
            // would be the one navigation key that hands the chart back out
            // from under a live field, discarding the half-typed reference and
            // its error message with `drop_field`.
            if self.range_edit.is_some() {
                // The dismissals above are the only state this arm changes, and
                // nothing further repaints — say so, or a menu cleared here
                // stays on screen until the next unrelated frame.
                cx.notify();
                return;
            }
            // Tab is action-bound, so it never reaches `sheet_key`'s hand-back
            // arm — the rule has to be stated again here. It moves the cell
            // selection exactly as the arrows do, and a selection the chart is
            // hiding is one you cannot watch move.
            self.chart_hand_back(cx);
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
            // Same as Tab above: the field has the keyboard first, then
            // action-bound, moves the selection, hands back.
            if self.range_edit.is_some() {
                // Same as Tab: the dismissals above need a frame.
                cx.notify();
                return;
            }
            self.chart_hand_back(cx);
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
            self.keytips = if self.keytips == KeyTip::Off {
                KeyTip::Tabs
            } else {
                KeyTip::Off
            };
            cx.notify();
            return;
        }
        if self.keytips != KeyTip::Off {
            if key == "escape" {
                self.keytips = if self.keytips == KeyTip::Commands {
                    KeyTip::Tabs
                } else {
                    KeyTip::Off
                };
                cx.notify();
                return;
            }
            if let Some(c) = ev.keystroke.key_char.as_deref().filter(|c| {
                c.chars().count() == 1
                    && c.chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_alphanumeric())
            }) {
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
        if matches!(
            key.as_str(),
            "left" | "right" | "home" | "end" | "up" | "down"
        ) {
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
        return if hl.contains(&nl) {
            hay.replace(needle, rep)
        } else {
            hay.to_string()
        };
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
        Cell {
            value: CellValue::Bool(true),
            ..Cell::default()
        }
    } else if t.eq_ignore_ascii_case("false") {
        Cell {
            value: CellValue::Bool(false),
            ..Cell::default()
        }
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
    Bold,
    Italic,
    Underline,
    Strike,
    Grow,
    Shrink,
    AlignL,
    AlignC,
    AlignR,
    AlignJ,
    Cut,
    Copy,
    Paste,
    Normal,
    H1,
    H2,
    H3,
    HRule,
    SelectAll,
    Case,
    Bullets,
    Numbers,
    IndentInc,
    IndentDec,
    ClearFmt,
    Find,
    FontColor,
    Highlight,
    FontName,
    FontSize,
    Super,
    Sub,
    NewComment,
    Sort,
    LineSpacing,
    ParaBorders,
    Title,
    Subtitle,
    ShowHide,
    ToggleComments,
    ToggleNav,
    DarkMode,
    AutoHideRibbon,
    InsertField,
    PageBreak,
    ToggleNotes,
    InsertTable,
    InsertSymbol,
    EditHeader,
    EditFooter,
    PageNumber,
    NoSpacing,
    Columns,
    Hyphenation,
    InsertEquation,
    RowAbove,
    RowBelow,
    ColLeft,
    ColRight,
    DelRow,
    DelCol,
    DelTable,
    PrintLayout,
    ToggleRuler,
    // Dialog-box launchers (open advanced dialogs — placeholder until we have a
    // dialog system).
    LaunchFont,
    LaunchParagraph,
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
fn cmdt(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    act: Act,
    shortcut: &'static str,
) -> rs::Cmd<Act> {
    rs::cmd(id, icon, label, act).tip(label, "", shortcut)
}

fn docxy_ribbon() -> rs::Ribbon<Act> {
    use Act::*;
    rs::Ribbon::new(vec![
        rs::tab(
            "Home",
            "H",
            vec![
                // Clipboard: a large Paste button + a small Cut/Copy column (Word).
                rs::group(
                    "Clipboard",
                    10,
                    vec![
                        Control::Large(cmdt("paste", "paste", "Paste", Paste, "Ctrl+V").key("V")),
                        rs::column(vec![
                            cmdt("cut", "cut", "Cut", Cut, "Ctrl+X").key("X"),
                            cmdt("copy", "copy", "Copy", Copy, "Ctrl+C").key("C"),
                        ]),
                    ],
                ),
                // Font: two rows — combos + size controls on top, character toggles below.
                rs::group(
                    "Font",
                    40,
                    vec![rs::rows(vec![
                        vec![
                            rs::combo(cmdt("fontname", "font-name", "Font", FontName, ""), true),
                            rs::combo(
                                cmdt("fontsize", "font-size", "Font size", FontSize, ""),
                                false,
                            ),
                            rs::btn(cmdt("grow", "font-increase", "Grow font", Grow, "").key("G")),
                            rs::btn(
                                cmdt("shrink", "font-decrease", "Shrink font", Shrink, "").key("K"),
                            ),
                            rs::btn(cmdt("case", "case", "Change case", Case, "").key("7")),
                            rs::btn(
                                cmdt("clearfmt", "clear-format", "Clear formatting", ClearFmt, "")
                                    .key("E"),
                            ),
                        ],
                        vec![
                            rs::btn(cmdt("b", "bold", "Bold", Bold, "Ctrl+B").key("1")),
                            rs::btn(cmdt("i", "italic", "Italic", Italic, "Ctrl+I").key("2")),
                            rs::btn(
                                cmdt("u", "underline", "Underline", Underline, "Ctrl+U").key("3"),
                            ),
                            rs::btn(
                                cmdt("s", "strikethrough", "Strikethrough", Strike, "").key("4"),
                            ),
                            rs::btn(cmdt("sub", "subscript", "Subscript", Sub, "").key("5")),
                            rs::btn(cmdt("sup", "superscript", "Superscript", Super, "").key("6")),
                            rs::btn(
                                cmdt("color", "text-color", "Font colour", FontColor, "").key("8"),
                            ),
                            rs::btn(
                                cmdt("hl", "highlight", "Text highlight", Highlight, "").key("9"),
                            ),
                        ],
                    ])],
                )
                .launcher(LaunchFont),
                // Paragraph: two rows — lists/indent/sort/marks on top, alignment below.
                rs::group(
                    "Paragraph",
                    30,
                    vec![rs::rows(vec![
                        vec![
                            rs::btn(
                                cmdt("bullets", "list-bullet", "Bullets", Bullets, "").key("U"),
                            ),
                            rs::btn(
                                cmdt("numbers", "list-numbered", "Numbering", Numbers, "").key("N"),
                            ),
                            rs::btn(
                                cmdt(
                                    "inddec",
                                    "indent-decrease",
                                    "Decrease indent",
                                    IndentDec,
                                    "Ctrl+Shift+M",
                                )
                                .key("O"),
                            ),
                            rs::btn(
                                cmdt(
                                    "indinc",
                                    "indent-increase",
                                    "Increase indent",
                                    IndentInc,
                                    "Ctrl+M",
                                )
                                .key("P"),
                            ),
                            rs::btn(
                                cmdt(
                                    "linespacing",
                                    "line-spacing",
                                    "Line and Paragraph Spacing",
                                    LineSpacing,
                                    "",
                                )
                                .key("Y"),
                            ),
                            rs::btn(cmdt("sort", "sort", "Sort", Sort, "").key("S")),
                            rs::btn(
                                cmdt("showhide", "paragraph", "Formatting marks", ShowHide, "")
                                    .key("H"),
                            ),
                        ],
                        vec![
                            rs::btn(cmdt("al", "align-left", "Align left", AlignL, "").key("L")),
                            rs::btn(cmdt("ac", "align-center", "Center", AlignC, "").key("A")),
                            rs::btn(cmdt("ar", "align-right", "Align right", AlignR, "").key("R")),
                            rs::btn(cmdt("aj", "align-justify", "Justify", AlignJ, "").key("J")),
                            rs::btn(
                                cmdt("borders", "border-bottom", "Bottom border", ParaBorders, "")
                                    .key("B"),
                            ),
                        ],
                    ])],
                )
                .launcher(LaunchParagraph),
                // Styles: a gallery of style thumbnails (Word keeps this on Home).
                rs::group(
                    "Styles",
                    35,
                    vec![Control::Gallery(rs::Gallery {
                        id: "styles",
                        tip: rs::ScreenTip::default(),
                        // Word's Quick Styles order: Normal, No Spacing, headings, then Title/Subtitle.
                        items: vec![
                            rs::GalleryItem {
                                label: "Normal",
                                preview: "normal",
                                act: Normal,
                            },
                            rs::GalleryItem {
                                label: "No Spacing",
                                preview: "normal",
                                act: NoSpacing,
                            },
                            rs::GalleryItem {
                                label: "Heading 1",
                                preview: "h1",
                                act: H1,
                            },
                            rs::GalleryItem {
                                label: "Heading 2",
                                preview: "h2",
                                act: H2,
                            },
                            rs::GalleryItem {
                                label: "Heading 3",
                                preview: "h3",
                                act: H3,
                            },
                            rs::GalleryItem {
                                label: "Title",
                                preview: "title",
                                act: Title,
                            },
                            rs::GalleryItem {
                                label: "Subtitle",
                                preview: "subtitle",
                                act: Subtitle,
                            },
                        ],
                    })],
                ),
                // Editing: a labelled column (Word: Find / Replace / Select).
                rs::group(
                    "Editing",
                    20,
                    vec![rs::column(vec![
                        cmdt("find", "find", "Find & Replace", Find, "Ctrl+F").key("F"),
                        cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A").key("D"),
                    ])],
                ),
            ],
        ),
        // Insert: headline commands as large buttons (Word's Insert tab style).
        rs::tab(
            "Insert",
            "N",
            vec![
                rs::group(
                    "Pages",
                    40,
                    vec![Control::Large(
                        cmdt("pagebreak", "rule", "Page Break", PageBreak, "").key("B"),
                    )],
                ),
                rs::group(
                    "Tables",
                    35,
                    vec![Control::Large(
                        cmdt("table", "table", "Table", InsertTable, "").key("T"),
                    )],
                ),
                rs::group(
                    "Header & Footer",
                    34,
                    vec![
                        Control::Large(
                            cmdt("header", "header", "Edit Header", EditHeader, "").key("H"),
                        ),
                        Control::Large(
                            cmdt("footer", "footer", "Edit Footer", EditFooter, "").key("O"),
                        ),
                        Control::Large(
                            cmdt("pagenum", "page-number", "Page Number", PageNumber, "").key("G"),
                        ),
                    ],
                ),
                rs::group(
                    "Text",
                    30,
                    vec![Control::Large(
                        cmdt("field", "case", "Field", InsertField, "").key("Q"),
                    )],
                ),
                rs::group(
                    "Symbols",
                    20,
                    vec![
                        Control::Large(
                            cmdt("equation", "equation", "Equation", InsertEquation, "").key("E"),
                        ),
                        Control::Large(
                            cmdt("symbol", "symbol", "Symbol", InsertSymbol, "").key("S"),
                        ),
                        Control::Large(cmdt("hr", "rule", "Rule", HRule, "").key("L")),
                    ],
                ),
                rs::group(
                    "Layout",
                    22,
                    vec![
                        Control::Large(cmdt("columns", "columns", "Columns", Columns, "").key("C")),
                        Control::Large(
                            cmdt("hyphen", "hyphenation", "Hyphenation", Hyphenation, "").key("Z"),
                        ),
                    ],
                ),
            ],
        ),
        // Review: a large New Comment + a small pane-toggle column, then Editing.
        rs::tab(
            "Review",
            "R",
            vec![
                rs::group(
                    "Comments",
                    40,
                    vec![
                        Control::Large(
                            cmdt("newcomment", "comment-add", "New Comment", NewComment, "")
                                .key("C"),
                        ),
                        rs::column(vec![
                            cmdt(
                                "togglecomments",
                                "comment",
                                "Comments pane",
                                ToggleComments,
                                "",
                            )
                            .key("P"),
                            cmdt("togglenotes", "comment", "Notes pane", ToggleNotes, "").key("O"),
                        ]),
                    ],
                ),
                rs::group(
                    "Editing",
                    30,
                    vec![rs::column(vec![
                        cmdt("find", "find", "Find & Replace", Find, "Ctrl+F").key("F"),
                        cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A").key("D"),
                        cmdt("case", "case", "Change case", Case, "").key("7"),
                    ])],
                ),
            ],
        ),
        // View: a large Print Layout toggle, then Show and Appearance columns.
        rs::tab(
            "View",
            "W",
            vec![
                rs::group(
                    "Views",
                    40,
                    vec![Control::Large(
                        cmdt(
                            "printlayout",
                            "print-layout",
                            "Print Layout",
                            PrintLayout,
                            "",
                        )
                        .key("P"),
                    )],
                ),
                rs::group(
                    "Show",
                    30,
                    vec![rs::column(vec![
                        cmdt("ruler", "rule", "Ruler", ToggleRuler, "").key("R"),
                        cmdt("showhide", "paragraph", "Formatting marks", ShowHide, "").key("M"),
                        cmdt("nav", "select-all", "Navigation", ToggleNav, "").key("N"),
                        cmdt(
                            "viewcomments",
                            "comment",
                            "Comments pane",
                            ToggleComments,
                            "",
                        )
                        .key("C"),
                    ])],
                ),
                rs::group(
                    "Appearance",
                    20,
                    vec![rs::column(vec![
                        cmdt("darkmode", "case", "Theme", DarkMode, "").key("T"),
                        cmdt(
                            "autohide",
                            "rule",
                            "Collapse ribbon",
                            AutoHideRibbon,
                            "Ctrl+F1",
                        )
                        .key("A"),
                    ])],
                ),
            ],
        ),
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
    let m = |cmd: &rs::Cmd<Act>| {
        (!cmd.key_tip.is_empty() && cmd.key_tip.eq_ignore_ascii_case(key)).then_some(cmd.act)
    };
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
    tab.groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find_map(|c| control_keytip(c, key))
}

/// A small KeyTip access-key badge, centred at the bottom of its host element.
fn keytip_badge(text: &str) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_end()
        .justify_center()
        .child(
            div()
                .px(px(3.))
                .rounded(px(2.))
                .bg(hsla_u(0xf2d24b))
                .text_size(px(9.))
                .text_color(hsla_u(0x1a1a1a))
                .child(SharedString::from(text.to_string())),
        )
        .into_any_element()
}

/// The contextual Table Tools tab, shown only while the caret is in a table.
fn table_tab() -> rs::Tab<Act> {
    use Act::*;
    rs::tab(
        "Table",
        "T",
        vec![
            rs::group(
                "Rows & Columns",
                40,
                vec![rs::rows(vec![
                    vec![
                        rs::btn(cmdt(
                            "rowabove",
                            "table-insert-row",
                            "Insert row above",
                            RowAbove,
                            "",
                        )),
                        rs::btn(cmdt(
                            "colleft",
                            "table-insert-column",
                            "Insert column left",
                            ColLeft,
                            "",
                        )),
                        rs::btn(cmdt("delrow", "table-delete-row", "Delete row", DelRow, "")),
                    ],
                    vec![
                        rs::btn(cmdt(
                            "rowbelow",
                            "table-insert-row",
                            "Insert row below",
                            RowBelow,
                            "",
                        )),
                        rs::btn(cmdt(
                            "colright",
                            "table-insert-column",
                            "Insert column right",
                            ColRight,
                            "",
                        )),
                        rs::btn(cmdt(
                            "delcol",
                            "table-delete-column",
                            "Delete column",
                            DelCol,
                            "",
                        )),
                    ],
                ])],
            ),
            rs::group(
                "Table",
                20,
                vec![rs::column(vec![cmdt(
                    "deltable",
                    "table-dismiss",
                    "Delete table",
                    DelTable,
                    "",
                )])],
            ),
        ],
    )
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
    let candidates: Vec<usize> = if down {
        (i + 1..n).collect()
    } else {
        (0..i).rev().collect()
    };
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
    (s.len() == 6)
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    0x000000, 0x404040, 0x808080, 0xBFBFBF, 0xFFFFFF, 0xC00000, 0xFF0000, 0xFFC000, 0xFFFF00,
    0x92D050, 0x00B050, 0x00B0F0, 0x0070C0, 0x002060, 0x7030A0,
];
/// Highlight swatches (Word highlight names). `None` = no highlight (clear).
const HIGHLIGHT_SWATCHES: &[&str] = &[
    "yellow",
    "green",
    "cyan",
    "magenta",
    "blue",
    "red",
    "darkYellow",
    "darkGreen",
    "darkCyan",
    "darkRed",
    "darkBlue",
    "lightGray",
];
/// Font families offered in the Font-name picker.
const FONT_NAMES: &[&str] = &[
    "Calibri",
    "Cambria",
    "Arial",
    "Times New Roman",
    "Georgia",
    "Verdana",
    "Tahoma",
    "Segoe UI",
    "Courier New",
    "Consolas",
    "Comic Sans MS",
];
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
    div()
        .w(px(2.))
        .h(px(19.))
        .ml(px(-1.))
        .mr(px(-1.))
        .bg(rgb(BRAND))
        .into_any_element()
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
        Measurer {
            ts: window.text_system().clone(),
            base: window.text_style().font(),
        }
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
        let run = TextRun {
            len: text.len(),
            font,
            color: hsla_u(0),
            ..Default::default()
        };
        f32::from(
            self.ts
                .shape_line(SharedString::from(text.to_string()), px(size), &[run], None)
                .width(),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_words(
    out: &mut Vec<AnyElement>,
    text: &str,
    props: &RunProps,
    base: f32,
    is_link: bool,
    selected: bool,
    click: Option<Click>,
    seg_start: usize,
    pal: Pal,
) {
    let mut off = seg_start;
    for word in text.split_inclusive(' ') {
        if word.is_empty() {
            continue;
        }
        let word_off = off;
        off += word.chars().count();
        let mut size = props
            .size_half_pts
            .map(|h| h as f32 / 2.0 * 1.333)
            .unwrap_or(base);
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
            props
                .color
                .as_deref()
                .and_then(hex_rgb)
                .map(hsla_u)
                .unwrap_or(pal.fg)
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
                .when_some(
                    props.highlight.as_deref().filter(|_| !selected),
                    |d, name| {
                        let (c, dark) = highlight_rgb(name);
                        d.bg(rgb(c))
                            .text_color(if dark { rgb(0x1a1a1a) } else { rgb(0xf5f5f5) })
                    },
                )
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
                            let byte = layout
                                .index_for_position(pos)
                                .unwrap_or_else(|e| e)
                                .min(word_str.len());
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
                                ent.update(cx, |this, cx| {
                                    this.begin_select(path.clone(), off, extend, window, cx)
                                });
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
        let selected = sel.is_some_and(|(s, e)| s < e && start + a >= s && start + b <= e);
        emit_words(
            out,
            &seg,
            props,
            base,
            is_link,
            selected,
            click,
            start + a,
            pal,
        );
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
fn emit_tab(
    out: &mut Vec<AnyElement>,
    idx: &mut usize,
    caret: &mut Option<usize>,
    sel: Option<(usize, usize)>,
    click: Option<Click>,
    marks: bool,
    base: f32,
    width: f32,
    pal: Pal,
) {
    let pos = *idx;
    if *caret == Some(pos) {
        out.push(caret_bar());
        *caret = None;
    }
    let selected = sel.is_some_and(|(s, e)| s < e && s <= pos && pos < e);
    let w = width.max(3.0);
    out.push(
        div()
            .flex_none()
            .w(px(w))
            .h(px(base))
            .overflow_hidden()
            // With formatting marks on, a tab arrow sits at the start of the gap.
            .when(marks, |d| {
                d.flex()
                    .items_center()
                    .text_size(px(base * 0.9))
                    .text_color(pal.dim)
                    .child("\u{2192}")
            })
            .when(selected, |d| d.bg(pal.sel))
            .when_some(click, |d, c| {
                let ent = c.ent.clone();
                let path = c.path.to_vec();
                d.cursor_text()
                    .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                        cx.stop_propagation();
                        let extend = ev.modifiers.shift;
                        ent.update(cx, |this, cx| {
                            this.set_caret(path.clone(), pos, extend, window, cx)
                        });
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
    let kind = if is_header {
        "headerReference"
    } else {
        "footerReference"
    };
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
    let Some(rels_bytes) = pkg.part("word/_rels/document.xml.rels") else {
        return vec![];
    };
    let rels = docxcore::load::parse_rels_xml(&String::from_utf8_lossy(rels_bytes));
    let Some(xml) = pkg.part(part_name) else {
        return vec![];
    };
    docxcore::load::parse_header_footer(&String::from_utf8_lossy(xml), &rels)
}

/// Render header/footer blocks read-only (no caret, no click) for the page margins.
fn hf_els(blocks: &[Block], pal: Pal, meas: &Measurer, hf_width: f32) -> Vec<AnyElement> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(paragraph_el(
                p,
                None,
                None,
                None,
                None,
                false,
                1.0,
                pal,
                Some(meas),
                Some(hf_width),
            )),
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
            let breaks = p
                .content
                .iter()
                .filter(|i| matches!(i, Inline::Break(_)))
                .count() as f32;
            let lines = (chars / cpl).ceil().max(1.0) + breaks;
            lines * lh
                + if p.props.heading_level.is_some() {
                    base
                } else {
                    4.0
                }
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
fn paginate_cols(
    blocks: &[Block],
    content_h: f32,
    col_w: f32,
    ncols: usize,
) -> Vec<Vec<(usize, usize)>> {
    let mut pages: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut page: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    let mut acc = 0.0_f32;
    let flush_col = |page: &mut Vec<(usize, usize)>,
                     pages: &mut Vec<Vec<(usize, usize)>>,
                     start: usize,
                     i: usize| {
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
                    Some(if ilvl % 2 == 1 {
                        "\u{25E6} ".to_string()
                    } else {
                        "\u{2022} ".to_string()
                    })
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
fn paragraph_el(
    p: &Paragraph,
    mut caret: Option<usize>,
    sel: Option<(usize, usize)>,
    marker: Option<&str>,
    click: Option<Click>,
    marks: bool,
    zoom: f32,
    pal: Pal,
    meas: Option<&Measurer>,
    hf_width: Option<f32>,
) -> AnyElement {
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
    let mut customs: Vec<(f32, TabAlign)> = p
        .props
        .tabs
        .iter()
        .map(|t| (zoom * t.pos as f32 / 15.0, t.align))
        .collect();
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
    let max_custom = customs
        .iter()
        .map(|(c, _)| *c)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut x = 0.0_f32;
    // Only paragraphs that actually contain a tab need per-run width measurement.
    let has_tab = p.content.iter().any(|i| matches!(i, Inline::Tab(_)));
    let m = meas.filter(|_| has_tab);
    // Width of a run's text at its effective size (matching emit_words' sizing).
    let run_w = |r: &docxcore::model::Run| -> f32 {
        match m {
            Some(m) => {
                let sz = r
                    .props
                    .size_half_pts
                    .map(|h| h as f32 / 2.0 * 1.333)
                    .unwrap_or(base);
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
            Inline::Field { text, .. } => m
                .map(|m| {
                    m.width(
                        if text.is_empty() { "[field]" } else { text },
                        base,
                        false,
                        false,
                    )
                })
                .unwrap_or(0.0),
            Inline::FootnoteRef { id, .. } => m
                .map(|m| m.width(&id.to_string(), base * 0.72, false, false))
                .unwrap_or(0.0),
            _ => 0.0,
        }
    };
    // Total width of content from index `from` up to the next tab / break / end —
    // the segment a centre/right tab must position.
    let seg_width = |from: usize| -> f32 {
        p.content[from..]
            .iter()
            .take_while(|it| !matches!(it, Inline::Tab(_) | Inline::Break(_)))
            .map(&inline_w)
            .sum()
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
        let lo = xm.max(if max_custom.is_finite() {
            max_custom
        } else {
            0.0
        });
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
        spans.push(
            div()
                .text_size(px(base))
                .text_color(pal.dim)
                .child(SharedString::from(m.to_string()))
                .into_any_element(),
        );
        if let Some(ms) = meas.filter(|_| has_tab) {
            x += ms.width(m, base, false, false);
        }
    }
    for i in 0..p.content.len() {
        let inline = &p.content[i];
        match inline {
            Inline::Run(r) => {
                emit_run(
                    &mut spans, &r.text, &r.props, base, false, &mut idx, &mut caret, sel, click,
                    pal,
                );
                x += run_w(r);
            }
            Inline::Hyperlink(h) => {
                for r in &h.runs {
                    emit_run(
                        &mut spans, &r.text, &r.props, base, true, &mut idx, &mut caret, sel,
                        click, pal,
                    );
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
                emit_tab(
                    &mut spans, &mut idx, &mut caret, sel, click, marks, base, w, pal,
                );
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
                let shown = if text.is_empty() {
                    "[field]".to_string()
                } else {
                    text.clone()
                };
                spans.push(
                    div()
                        .px(px(2.))
                        .rounded_sm()
                        .bg(pal.panel)
                        .text_size(px(base))
                        .text_color(pal.fg)
                        .child(SharedString::from(shown))
                        .into_any_element(),
                );
                x += inline_w(inline);
            }
            Inline::FootnoteRef { id, .. } => {
                // A superscript note number in the brand colour.
                spans.push(
                    div()
                        .text_size(px(base * 0.72))
                        .text_color(hsla_u(BRAND))
                        .relative()
                        .top(px(-(base * 0.35)))
                        .child(SharedString::from(id.to_string()))
                        .into_any_element(),
                );
                x += inline_w(inline);
            }
            Inline::Equation { text, latex, .. } => {
                // Math renders as its Unicode form (falling back to the LaTeX),
                // in a faint math tint — docxy can't typeset, but the equation
                // is visible and editable-as-text rather than a "[equation]" stub.
                let shown = if !text.is_empty() {
                    text.clone()
                } else {
                    latex.clone().unwrap_or_default()
                };
                spans.push(
                    div()
                        .px(px(3.))
                        .rounded_sm()
                        .bg(Hsla {
                            a: 0.14,
                            ..hsla_u(BRAND)
                        })
                        .text_size(px(base))
                        .text_color(pal.fg)
                        .italic()
                        .child(SharedString::from(shown))
                        .into_any_element(),
                );
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
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hsla_u(BRAND))
                            .child("\u{25C6} SmartArt"),
                    );
                if text.is_empty() {
                    box_el = box_el.child(
                        div()
                            .text_size(px(base * 0.9))
                            .text_color(pal.dim)
                            .child("(no text)"),
                    );
                } else {
                    for node in text {
                        box_el = box_el.child(
                            div()
                                .text_size(px(base * 0.9))
                                .text_color(pal.fg)
                                .child(SharedString::from(format!("\u{2022} {node}"))),
                        );
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
                spans.push(
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(pal.panel)
                        .text_size(px(12.))
                        .text_color(pal.dim)
                        .child(tag)
                        .into_any_element(),
                );
            }
        }
    }
    if caret.is_some() {
        spans.push(caret_bar());
    }
    // A pilcrow at the paragraph end when formatting marks are shown.
    if marks {
        spans.push(
            div()
                .text_size(px(base))
                .text_color(pal.dim)
                .child("\u{00B6}")
                .into_any_element(),
        );
    }
    let has_border = p.props.borders.bottom.is_some();
    // Line spacing (auto-rule multiple; exact/atLeast fall back to single here).
    let line_mult = p
        .props
        .spacing
        .line_multiple()
        .unwrap_or(1.0)
        .clamp(0.5, 4.0);
    let line_h = base * 1.35 * line_mult;
    let mut row = h_flex()
        .w_full()
        .flex_wrap()
        .min_h(px(line_h.max(base + 6.)))
        .line_height(px(line_h));
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
            d.cursor_text()
                .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                    let extend = ev.modifiers.shift;
                    ent.update(cx, |this, cx| {
                        this.set_caret(path.clone(), para_end, extend, window, cx)
                    });
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
            cells.push(
                v_flex()
                    .flex_1()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(ctx.pal.border)
                    .children(inner)
                    .into_any_element(),
            );
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
            let sel = ctx
                .active
                .then(|| {
                    ctx.spans
                        .iter()
                        .find(|(pp, _, _)| pp.as_slice() == path.as_slice())
                        .map(|(_, s, e)| (*s, *e))
                })
                .flatten();
            let click = ctx.active.then_some(Click {
                ent: ctx.ent,
                path: &path,
            });
            paragraph_el(
                p,
                caret,
                sel,
                marker,
                click,
                ctx.marks,
                ctx.zoom,
                ctx.pal,
                Some(ctx.meas),
                ctx.hf_width,
            )
        }
        Block::Table(t) => table_el(t, &path, ctx),
        Block::Raw(_) => div().h(px(0.)).into_any_element(),
    }
}

// ---- chrome: ribbon + backstage --------------------------------------------

impl Docxy {
    fn ribbon_tabs(&self, fg: Hsla, dim: Hsla, panel: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let names = ["File", "Home", "Insert", "Review", "View"];
        let mut strip = h_flex()
            .w_full()
            .items_end()
            .gap_1()
            .px_2()
            .pt_1()
            .bg(panel);
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
                    .when(is_file, |d| {
                        d.bg(rgb(BRAND))
                            .text_color(rgb(FILE_FG))
                            .font_weight(FontWeight::BOLD)
                            .rounded_t_sm()
                    })
                    .when(active, |d| {
                        d.text_color(rgb(BRAND))
                            .border_b_2()
                            .border_color(rgb(BRAND))
                    })
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
                    .when(active, |d| {
                        d.border_b_2()
                            .border_color(accent)
                            .font_weight(FontWeight::BOLD)
                    })
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
                .child(if self.ribbon_min {
                    "\u{2304}"
                } else {
                    "\u{2303}"
                })
                .tooltip(|w, cx| {
                    Tooltip::new("Collapse the ribbon  \u{00b7}  Ctrl+F1").build(w, cx)
                })
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
        let item = |cx: &mut Context<Self>,
                    id: &'static str,
                    label: &'static str,
                    icon: &'static str,
                    act: Act| {
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
            .child(item(
                cx,
                "cm-underline",
                "Underline",
                "underline",
                Act::Underline,
            ))
            .child(sep())
            .child(item(
                cx,
                "cm-comment",
                "New Comment",
                "comment-add",
                Act::NewComment,
            ));
        // Full-window backdrop to catch outside clicks / right-clicks.
        div()
            .id("cm-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _w, cx| {
                    this.context_menu = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _w, cx| {
                    this.context_menu = None;
                    cx.notify();
                }),
            )
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
                .when(self.act_active(act), |d| {
                    d.bg(Hsla {
                        a: 0.20,
                        ..hsla_u(BRAND)
                    })
                })
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
                if let Some(i) = ribbon
                    .tabs
                    .iter()
                    .position(|t| t.key_tip.eq_ignore_ascii_case(c))
                {
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
                let tab = if self.ribbon_tab == RibbonTab::Table {
                    &table
                } else {
                    &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)]
                };
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
            LaunchParagraph => {
                self.launch_msg("Paragraph — advanced dialog coming soon", window, cx)
            }
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
            RowAbove | RowBelow | ColLeft | ColRight | DelRow | DelCol | DelTable => {
                self.table_op(act, window, cx)
            }
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
                    let b = if has {
                        ParBorders::default()
                    } else {
                        ParBorders {
                            top: None,
                            bottom: Some(BorderKind::Single),
                        }
                    };
                    e.set_para_border(b);
                }
                Title => e.set_para_style(Some("Title")),
                Subtitle => e.set_para_style(Some("Subtitle")),
                ClearFmt => e.clear_run_formatting(),
                Cut | Copy | Paste | LaunchFont | LaunchParagraph | Find | FontColor
                | Highlight | FontName | FontSize | NewComment | ShowHide | ToggleComments
                | ToggleNav | DarkMode | AutoHideRibbon | InsertField | PageBreak | ToggleNotes
                | InsertTable | InsertSymbol | InsertEquation | LineSpacing | EditHeader
                | EditFooter | PageNumber | Columns | Hyphenation | RowAbove | RowBelow
                | ColLeft | ColRight | DelRow | DelCol | DelTable | PrintLayout | ToggleRuler => {}
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
        let tab = if self.ribbon_tab == RibbonTab::Table {
            &ctx_tab
        } else {
            &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)]
        };
        let avail = (width - 28.0).max(120.0);

        // 1) drop control labels if the full layout overflows.
        let icon_only = tab.groups.iter().map(|g| group_est(g, false)).sum::<f32>() > avail;
        // 2) collapse lowest-priority groups until what remains fits.
        let mut shown: Vec<usize> = (0..tab.groups.len()).collect();
        loop {
            let total: f32 = shown
                .iter()
                .map(|&i| group_est(&tab.groups[i], icon_only))
                .sum();
            if total <= avail || shown.len() <= 1 {
                break;
            }
            let victim = *shown
                .iter()
                .min_by_key(|&&i| tab.groups[i].priority)
                .unwrap();
            shown.retain(|&i| i != victim);
        }
        let hidden = tab.groups.len() - shown.len();

        let mut groups: Vec<AnyElement> = shown
            .iter()
            .map(|&i| self.render_group(&tab.groups[i], icon_only, pal, cx))
            .collect();
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
                    .child(
                        div()
                            .text_size(px(9.))
                            .child(SharedString::from(format!("{hidden} more"))),
                    )
                    .into_any_element(),
            );
        }
        h_flex()
            .w_full()
            .h(px(98.))
            .items_stretch()
            .px_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .children(groups)
            .into_any_element()
    }

    /// One spreadsheet-ribbon button: a glyph/label that runs a `SheetAct`.
    /// A small icon-only Home button (the two-row Font/Alignment buttons).
    fn sheet_ib(
        &self,
        icon: &'static str,
        act: SheetAct,
        on: bool,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(format!("sib-{icon}").into()))
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.))
            .rounded(px(3.))
            .cursor_pointer()
            .when(on, |d| {
                d.bg(Hsla {
                    a: 0.20,
                    ..hsla_u(BRAND)
                })
            })
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(icon, 15., pal.fg))
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(act, window, cx)))
            .into_any_element()
    }

    /// A small glyph/text Home button (number formats, wrap, merge, …).
    fn sheet_gb(
        &self,
        glyph: &'static str,
        act: SheetAct,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(format!("sgb-{glyph}").into()))
            .flex()
            .items_center()
            .justify_center()
            .min_w(px(22.))
            .h(px(22.))
            .px_1()
            .rounded(px(3.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(pal.fg)
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(glyph)
            .on_click(cx.listener(move |this, _, window, cx| this.run_sheet_act(act, window, cx)))
            .into_any_element()
    }

    /// A small icon+label row (Clipboard Cut/Copy, Editing AutoSum/Fill/Clear).
    fn sheet_rb(
        &self,
        icon: Option<&'static str>,
        label: &'static str,
        act: SheetAct,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(format!("srb-{label}").into()))
            .flex()
            .items_center()
            .gap_1p5()
            .px_1()
            .h(px(20.))
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
    fn sheet_lb(
        &self,
        icon: Option<&'static str>,
        label: &'static str,
        act: SheetAct,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut lines = v_flex().items_center();
        for ln in label_lines(label) {
            lines = lines.child(
                div()
                    .text_size(px(10.))
                    .text_color(pal.fg)
                    .child(SharedString::from(ln)),
            );
        }
        div()
            .id(ElementId::Name(format!("slb-{label}").into()))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_0p5()
            .min_w(px(40.))
            .h_full()
            .px_1p5()
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
    fn sheet_combo(
        &self,
        value: &'static str,
        wide: bool,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(format!("scombo-{value}").into()))
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .w(px(if wide { 108. } else { 50. }))
            .h(px(22.))
            .px_1p5()
            .rounded(px(3.))
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .cursor_pointer()
            .hover(|d| d.border_color(hsla_u(BRAND)))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.fg)
                    .overflow_hidden()
                    .child(value),
            )
            .child(
                div()
                    .text_size(px(8.))
                    .text_color(pal.dim)
                    .child("\u{25BE}"),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.run_sheet_act(SheetAct::Todo, window, cx)
            }))
            .into_any_element()
    }

    /// The Number-group format combo: shows the selection's current format name
    /// and toggles the format-picker strip.
    fn sheet_numfmt_combo(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let name = self.active_numfmt_name();
        div()
            .id("numfmt-combo")
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .w(px(108.))
            .h(px(22.))
            .px_1p5()
            .rounded(px(3.))
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .cursor_pointer()
            .hover(|d| d.border_color(hsla_u(BRAND)))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.fg)
                    .overflow_hidden()
                    .child(name),
            )
            .child(
                div()
                    .text_size(px(8.))
                    .text_color(pal.dim)
                    .child("\u{25BE}"),
            )
            .on_click(cx.listener(|this, _, _w, cx| {
                this.sheet_numfmt_open = !this.sheet_numfmt_open;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The format-picker strip shown under the ribbon while the Number dropdown is
    /// open: each option applies its code to the selection and shows a live sample.
    fn sheet_numfmt_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::{CellValue, Xf, format_with};
        let d1904 = self.active_sheet().is_some_and(|v| v.pkg.workbook.date1904);
        let mut row = h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_1p5()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border);
        row = row.child(
            div()
                .text_size(px(11.))
                .text_color(pal.dim)
                .min_w(px(70.))
                .child("Number format"),
        );
        for (label, code) in NUM_FORMATS {
            // Live sample of 1234.5 in this format (dates/text show a fixed sample).
            let sample = if code.is_empty() {
                "1234.5".to_string()
            } else if code == "@" {
                "abc".to_string()
            } else if code.contains('y') || code.contains('h') {
                format_with(
                    &Xf {
                        code: Some(code.to_string()),
                        ..Xf::default()
                    },
                    &CellValue::Number(45658.5),
                    d1904,
                )
            } else {
                format_with(
                    &Xf {
                        code: Some(code.to_string()),
                        ..Xf::default()
                    },
                    &CellValue::Number(1234.5),
                    d1904,
                )
            };
            row = row.child(
                div()
                    .id(ElementId::Name(format!("nf-{label}").into()))
                    .flex()
                    .flex_col()
                    .px_2()
                    .py_1()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(pal.border)
                    .bg(hsla_u(0xffffff))
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(pal.fg)
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(pal.dim)
                            .child(SharedString::from(sample)),
                    )
                    .on_click(
                        cx.listener(move |this, _, _w, cx| this.sheet_apply_numfmt(code, cx)),
                    ),
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
        let heading = |t: &str| {
            div()
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .text_color(pal.dim)
                .child(t.to_string())
        };

        // Number formats.
        let mut number = h_flex().flex_wrap().gap_1();
        for (label, code) in NUM_FORMATS {
            let active = label == cur_fmt;
            number = number.child(
                div()
                    .id(ElementId::Name(format!("fmt-nf-{label}").into()))
                    .px_2()
                    .py(px(3.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .bg(if active { hsla_u(BRAND) } else { pal.panel })
                    .text_color(if active { hsla_u(0xffffff) } else { pal.fg })
                    .border_1()
                    .border_color(pal.border)
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(label)
                    .on_click(
                        cx.listener(move |this, _, _w, cx| this.sheet_apply_numfmt(code, cx)),
                    ),
            );
        }

        // Toggle button (Bold/Italic/Align/Border).
        let tbtn = |id: &str, label: &str, active: bool| {
            div()
                .id(ElementId::Name(format!("fmt-{id}").into()))
                .px_2p5()
                .py(px(3.))
                .rounded_sm()
                .cursor_pointer()
                .text_size(px(12.))
                .bg(if active { hsla_u(BRAND) } else { pal.panel })
                .text_color(if active { hsla_u(0xffffff) } else { pal.fg })
                .border_1()
                .border_color(pal.border)
                .hover(|d| d.border_color(hsla_u(BRAND)))
                .child(label.to_string())
        };
        let font_row = h_flex()
            .gap_1p5()
            .child(
                tbtn("bold", "B", xf.bold)
                    .on_click(cx.listener(|this, _, _w, cx| this.sheet_toggle_bold(cx))),
            )
            .child(
                tbtn("italic", "I", xf.italic)
                    .on_click(cx.listener(|this, _, _w, cx| this.sheet_toggle_italic(cx))),
            );
        let align_row = h_flex()
            .gap_1p5()
            .child(
                tbtn("al", "Left", xf.align == Align::Left)
                    .on_click(cx.listener(|this, _, _w, cx| this.sheet_align(Align::Left, cx))),
            )
            .child(
                tbtn("ac", "Center", xf.align == Align::Center)
                    .on_click(cx.listener(|this, _, _w, cx| this.sheet_align(Align::Center, cx))),
            )
            .child(
                tbtn("ar", "Right", xf.align == Align::Right)
                    .on_click(cx.listener(|this, _, _w, cx| this.sheet_align(Align::Right, cx))),
            );
        let border_row = h_flex().child(
            tbtn("border", "Box border", xf.border)
                .on_click(cx.listener(|this, _, _w, cx| this.sheet_toggle_border(cx))),
        );

        // Colour swatch row for a given picker (with a leading "None").
        let swatches = |pick: SheetPick, cur: Option<(u8, u8, u8)>, cx: &mut Context<Self>| {
            let mut row = h_flex().flex_wrap().gap_1();
            let none_sel = cur.is_none();
            row = row.child(
                div()
                    .id(ElementId::Name(
                        format!("fmt-c-none-{}", pick == SheetPick::Fill).into(),
                    ))
                    .px_1p5()
                    .py(px(1.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(10.))
                    .border_1()
                    .border_color(if none_sel { hsla_u(BRAND) } else { pal.border })
                    .text_color(pal.fg)
                    .child("None")
                    .on_click(
                        cx.listener(move |this, _, _w, cx| this.sheet_apply_color(pick, None, cx)),
                    ),
            );
            for &c in COLOR_SWATCHES {
                let rgb = (
                    ((c >> 16) & 0xff) as u8,
                    ((c >> 8) & 0xff) as u8,
                    (c & 0xff) as u8,
                );
                let sel = cur == Some(rgb);
                row = row.child(
                    div()
                        .id(ElementId::Name(
                            format!("fmt-c-{}-{c:06x}", pick == SheetPick::Fill).into(),
                        ))
                        .size(px(18.))
                        .rounded_sm()
                        .cursor_pointer()
                        .bg(hsla_u(c))
                        .border_1()
                        .border_color(if sel { hsla_u(BRAND) } else { hsla_u(0x9a9a9a) })
                        .on_click(cx.listener(move |this, _, _w, cx| {
                            this.sheet_apply_color(pick, Some(rgb), cx)
                        })),
                );
            }
            row
        };
        let font_colors = swatches(SheetPick::Font, xf.color, cx);
        let fill_colors = swatches(SheetPick::Fill, xf.fill, cx);

        let card = v_flex()
            .w(px(420.))
            .gap_3()
            .p_4()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .rounded(px(8.))
            .shadow_lg()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(pal.fg)
                            .child("Format Cells"),
                    )
                    .child(
                        div()
                            .id("fmt-close")
                            .px_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_size(px(15.))
                            .text_color(pal.dim)
                            .hover(|d| d.text_color(pal.fg))
                            .child("\u{00d7}")
                            .on_click(cx.listener(|this, _, _w, cx| {
                                this.sheet_fmt_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(v_flex().gap_1().child(heading("NUMBER")).child(number))
            .child(
                v_flex().gap_1().child(heading("FONT")).child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(font_row)
                        .child(font_colors),
                ),
            )
            .child(v_flex().gap_1().child(heading("FILL")).child(fill_colors))
            .child(
                v_flex()
                    .gap_1()
                    .child(heading("ALIGNMENT"))
                    .child(align_row),
            )
            .child(v_flex().gap_1().child(heading("BORDER")).child(border_row))
            .child(
                h_flex().justify_end().child(
                    div()
                        .id("fmt-done")
                        .px_3()
                        .py(px(4.))
                        .rounded_sm()
                        .cursor_pointer()
                        .text_size(px(12.))
                        .bg(hsla_u(BRAND))
                        .text_color(hsla_u(0xffffff))
                        .child("Done")
                        .on_click(cx.listener(|this, _, _w, cx| {
                            this.sheet_fmt_open = false;
                            cx.notify();
                        })),
                ),
            );

        // Backdrop (click to dismiss) + centred card.
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(Hsla {
                h: 0.,
                s: 0.,
                l: 0.,
                a: 0.35,
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _w, cx| {
                    this.sheet_fmt_open = false;
                    cx.notify();
                }),
            )
            .child(
                // Stop the card's own clicks from dismissing.
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(card),
            )
            .into_any_element()
    }

    /// The swatch strip shown under the ribbon while a sheet colour picker is open;
    /// a swatch sets the fill or font colour of the selection.
    fn sheet_picker_bar(&self, pick: SheetPick, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let mut row = h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_1p5()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border);
        row = row.child(
            div()
                .text_size(px(11.))
                .text_color(pal.dim)
                .min_w(px(70.))
                .child(match pick {
                    SheetPick::Fill => "Fill colour",
                    SheetPick::Font => "Font colour",
                }),
        );
        row = row.child(
            div()
                .id("sc-none")
                .px_2()
                .h(px(20.))
                .rounded(px(3.))
                .text_size(px(11.))
                .text_color(pal.fg)
                .border_1()
                .border_color(pal.border)
                .cursor_pointer()
                .hover(|d| d.bg(pal.hover))
                .child(if pick == SheetPick::Fill {
                    "No fill"
                } else {
                    "Automatic"
                })
                .on_click(
                    cx.listener(move |this, _, _, cx| this.sheet_apply_color(pick, None, cx)),
                ),
        );
        for &c in COLOR_SWATCHES {
            let rgb = (
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            );
            row = row.child(
                div()
                    .id(("sc", c as usize))
                    .size(px(20.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if c == 0xFFFFFF { pal.fg } else { pal.border })
                    .bg(hsla_u(c))
                    .cursor_pointer()
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sheet_apply_color(pick, Some(rgb), cx)
                    })),
            );
        }
        row.into_any_element()
    }

    /// The sheet Find & Replace bar (Ctrl+F): query + prev/next, replace + all.
    /// The conditional-format entry bar: type a comparison like ">500" to apply
    /// the Light-Red highlight rule to the selection.
    fn sheet_cf_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent = cx.entity();
        let (ent_ok, ent_cancel) = (ent.clone(), ent.clone());
        h_flex()
            .w_full()
            .min_h(px(30.))
            .py(px(3.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child("Highlight"),
            )
            .child(div().w(px(160.)).child(self.bar_range_field(
                "cf-range",
                RefTarget::CondFormat,
                cx,
            )))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child("where value"),
            )
            .child(
                div()
                    .w(px(160.))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() {
                        ">500".to_string()
                    } else {
                        buf.to_string()
                    })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .child("(>, <, =, <>; 100..500 between; 'clear')"),
            )
            .child(
                div()
                    .id("cf-apply")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(hsla_u(BRAND))
                    .text_color(hsla_u(0xffffff))
                    .border_1()
                    .border_color(pal.border)
                    .child("Apply")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_ok.update(cx, |this, cx| {
                            // Reuse the key path's commit by simulating Enter.
                            this.sheet_cf_commit(cx);
                        });
                    }),
            )
            .child(
                div()
                    .id("cf-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(pal.panel)
                    .text_color(pal.fg)
                    .border_1()
                    .border_color(pal.border)
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.sheet_cf_edit = None;
                            this.bar_close();
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// The range field an entry bar carries: the cells it will act on, pointable
    /// at the grid like any other reference.
    fn bar_range_field(
        &self,
        id: &'static str,
        target: RefTarget,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Unpinned, it shows the selection live, so what the field says and what
        // Apply will do can't drift apart.
        let sheet = self
            .active_sheet()
            .map(|v| v.sheet().name.clone())
            .unwrap_or_default();
        let value = self
            .bar_range
            .clone()
            .or_else(|| self.bar_seed().map(|r| ref_a1(Some(&sheet), r)))
            .unwrap_or_default();
        let hint = self.ref_example((0, 0, 4, 3));
        self.ref_field(id, target, value, hint, "", cx)
    }

    /// The Text-to-Columns delimiter bar.
    fn sheet_ttc_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent_cancel = cx.entity();
        h_flex()
            .w_full()
            .min_h(px(30.))
            .py(px(3.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child("Split"))
            .child(div().w(px(160.)).child(self.bar_range_field(
                "ttc-range",
                RefTarget::TextToColumns,
                cx,
            )))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child("by delimiter:"),
            )
            .child(
                div()
                    .w(px(150.))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() {
                        "comma (or tab, space, ;)".to_string()
                    } else {
                        buf.to_string()
                    })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                div()
                    .id("ttc-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(pal.panel)
                    .text_color(pal.fg)
                    .border_1()
                    .border_color(pal.border)
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.sheet_ttc_edit = None;
                            this.bar_close();
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// The AutoFilter criteria bar: type a comparison on the current column.
    fn sheet_filter_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::col_name;
        let col = self
            .active_sheet()
            .map(|v| col_name(v.sel.1))
            .unwrap_or_default();
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full()
            .h(px(30.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child(format!("Filter column {col} where value")),
            )
            .child(
                div()
                    .w(px(180.))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() {
                        "=Laptop  (or >500, clear)".to_string()
                    } else {
                        buf.to_string()
                    })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                div()
                    .id("filter-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(pal.panel)
                    .text_color(pal.fg)
                    .border_1()
                    .border_color(pal.border)
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.sheet_filter_edit = None;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// The multi-level sort bar: type a spec like "B asc, C desc".
    fn sheet_sort_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full()
            .min_h(px(30.))
            .py(px(3.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(div().text_size(px(12.)).text_color(pal.dim).child("Sort"))
            .child(
                div()
                    .w(px(160.))
                    .child(self.bar_range_field("sort-range", RefTarget::Sort, cx)),
            )
            .child(div().text_size(px(12.)).text_color(pal.dim).child("by"))
            .child(
                div()
                    .w(px(220.))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() {
                        "B asc, C desc".to_string()
                    } else {
                        buf.to_string()
                    })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                div()
                    .id("sort-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(pal.panel)
                    .text_color(pal.fg)
                    .border_1()
                    .border_color(pal.border)
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.sheet_sort_edit = None;
                            this.bar_close();
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// The row-height entry bar: type a height in points (or "auto").
    fn sheet_rowh_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full()
            .h(px(30.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child("Row height (points, or 'auto')"),
            )
            .child(
                div()
                    .w(px(120.))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() {
                        "30".to_string()
                    } else {
                        buf.to_string()
                    })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                div()
                    .id("rowh-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(pal.panel)
                    .text_color(pal.fg)
                    .border_1()
                    .border_color(pal.border)
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.sheet_rowh_edit = None;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// The data-validation entry bar: type comma-separated allowed values to make
    /// the selection a dropdown list.
    fn sheet_dv_edit_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ent = cx.entity();
        let ent_cancel = ent.clone();
        h_flex()
            .w_full()
            .min_h(px(30.))
            .py(px(3.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child("Dropdown list for"),
            )
            .child(div().w(px(160.)).child(self.bar_range_field(
                "dv-range",
                RefTarget::Validation,
                cx,
            )))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child("(comma-separated):"),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(if buf.is_empty() {
                        "Yes, No, Maybe".to_string()
                    } else {
                        buf.to_string()
                    })))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                div()
                    .id("dv-cancel")
                    .px_2()
                    .py(px(2.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .bg(pal.panel)
                    .text_color(pal.fg)
                    .border_1()
                    .border_color(pal.border)
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.sheet_dv_edit = None;
                            this.bar_close();
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    /// Apply the current CF buffer (used by the Apply button; Enter uses sheet_cf_key).
    fn sheet_cf_commit(&mut self, cx: &mut Context<Self>) {
        let buf = self.sheet_cf_edit.clone().unwrap_or_default();
        let buf = if buf.trim().is_empty() {
            ">500".to_string()
        } else {
            buf
        };
        // The Apply button doesn't go through the field's Enter, so a range
        // typed but not yet committed would be silently ignored. And when that
        // text isn't a range at all, the bar stays open showing why rather than
        // quietly applying the rule to the old cells.
        if !self.bar_flush(cx) {
            cx.notify();
            return;
        }
        let cells = self.bar_cells();
        if buf.trim().eq_ignore_ascii_case("clear") {
            self.sheet_snapshot();
            if let Some(v) = self.active_sheet_mut() {
                let s = v.active;
                v.pkg.clear_conditional_formats(s);
                v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
            }
            self.mark_sheet_dirty();
        } else if let Some(((op, val, val2), cells)) = parse_cf_input(&buf).zip(cells) {
            self.sheet_snapshot();
            if let Some(v) = self.active_sheet_mut() {
                let s = v.active;
                v.pkg
                    .add_conditional_format(s, cells, op, &val, val2.as_deref(), cf_preset_dxf());
                v.engine = gridcore::engine::Engine::new(&v.pkg.workbook);
            }
            self.mark_sheet_dirty();
        }
        self.sheet_cf_edit = None;
        self.bar_close();
        cx.notify();
    }

    /// The cell-comment entry bar: a labelled text field (self-managed, keys via
    /// sheet_comment_key) with the target cell, Save/Cancel. Enter commits.
    fn sheet_comment_bar(&self, buf: &str, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        use gridcore::sheet::cell_name;
        let cell = self
            .active_sheet()
            .map(|v| cell_name(v.sel.0, v.sel.1))
            .unwrap_or_default();
        let ent = cx.entity();
        let (ent_save, ent_cancel) = (ent.clone(), ent.clone());
        let btn = |label: &str, primary: bool| {
            div()
                .px_2()
                .py(px(2.))
                .rounded_sm()
                .cursor_pointer()
                .text_size(px(12.))
                .bg(if primary { hsla_u(BRAND) } else { pal.panel })
                .text_color(if primary { hsla_u(0xffffff) } else { pal.fg })
                .border_1()
                .border_color(pal.border)
                .child(label.to_string())
        };
        h_flex()
            .w_full()
            .h(px(30.))
            .items_center()
            .gap_2()
            .px_2()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(pal.dim)
                    .child(format!("Comment on {cell}:")),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .text_size(px(12.))
                    .text_color(hsla_u(0x1a1a1a))
                    .child(div().child(SharedString::from(buf.to_string())))
                    .child(div().w(px(1.5)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND))),
            )
            .child(
                btn("Save", true).on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_save.update(cx, |this, cx| this.sheet_commit_comment(cx));
                }),
            )
            .child(
                btn("Cancel", false).on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    ent_cancel.update(cx, |this, cx| {
                        this.sheet_comment_edit = None;
                        cx.notify();
                    });
                }),
            )
            .into_any_element()
    }

    fn sheet_find_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let qf = self.find_field == FindField::Query;
        let rf = self.find_field == FindField::Replace;
        let field = |id: &'static str, val: &str, focused: bool, ph: &'static str| {
            let empty = val.is_empty();
            div()
                .id(id)
                .flex()
                .items_center()
                .min_w(px(150.))
                .h(px(24.))
                .px_2()
                .rounded(px(3.))
                .border_1()
                .border_color(if focused { hsla_u(BRAND) } else { pal.border })
                .bg(hsla_u(0xffffff))
                .cursor_text()
                .text_size(px(12.))
                .text_color(if empty {
                    hsla_u(0x999999)
                } else {
                    hsla_u(0x1a1a1a)
                })
                .child(SharedString::from(if empty {
                    ph.to_string()
                } else {
                    val.to_string()
                }))
                .when(focused, |d| {
                    d.child(div().w(px(1.)).h(px(13.)).ml(px(1.)).bg(hsla_u(BRAND)))
                })
        };
        let btn = |id: &'static str, label: SharedString| {
            div()
                .id(id)
                .px_2()
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .min_w(px(24.))
                .rounded(px(3.))
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .border_1()
                .border_color(pal.border)
                .hover(|d| d.bg(pal.hover))
                .child(label)
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
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .min_w(px(46.))
                    .child("Find"),
            )
            .child(
                field("sf-q", &self.find_query, qf, "Find in sheet").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.find_field = FindField::Query;
                        cx.notify();
                    },
                )),
            )
            .child(
                btn("sf-prev", "\u{25C0}".into())
                    .on_click(cx.listener(|this, _, _, cx| this.sheet_find_next(true, cx))),
            )
            .child(
                btn("sf-next", "\u{25B6}".into())
                    .on_click(cx.listener(|this, _, _, cx| this.sheet_find_next(false, cx))),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .child("Replace"),
            )
            .child(
                field("sf-r", &self.replace_text, rf, "Replace with").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.find_field = FindField::Replace;
                        cx.notify();
                    },
                )),
            )
            .child(
                btn("sf-rep", "Replace".into())
                    .on_click(cx.listener(|this, _, _, cx| this.sheet_replace(cx))),
            )
            .child(
                btn("sf-all", "All".into())
                    .on_click(cx.listener(|this, _, _, cx| this.sheet_replace_all(cx))),
            )
            .child(div().flex_1())
            .child(
                btn("sf-close", "\u{2715}".into()).on_click(cx.listener(|this, _, _, cx| {
                    this.find_open = false;
                    cx.notify();
                })),
            )
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
                .child(
                    div()
                        .w_full()
                        .text_size(px(10.))
                        .text_color(pal.dim)
                        .text_center()
                        .child(title.to_string()),
                )
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
            .child(group(
                "Tables",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(
                        Some("table"),
                        "PivotTable",
                        SheetAct::InsertPivot,
                        pal,
                        cx,
                    ))
                    .child(self.sheet_lb(Some("table"), "Table", SheetAct::FormatAsTable, pal, cx))
                    .child(self.sheet_lb(
                        None,
                        "Data Validation",
                        SheetAct::DataValidation,
                        pal,
                        cx,
                    ))
                    .child(self.sheet_lb(None, "Text to Columns", SheetAct::TextToColumns, pal, cx))
                    .into_any_element(),
            ))
            .child(group(
                "Outline",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(None, "Subtotal", SheetAct::Subtotal, pal, cx))
                    .child(self.sheet_lb(None, "Group / Ungroup", SheetAct::Outline, pal, cx))
                    .into_any_element(),
            ))
            .child(group(
                "Charts",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(None, "Column", SheetAct::InsertChart("column"), pal, cx))
                    .child(self.sheet_lb(None, "Bar", SheetAct::InsertChart("bar"), pal, cx))
                    .child(self.sheet_lb(None, "Line", SheetAct::InsertChart("line"), pal, cx))
                    .child(self.sheet_lb(None, "Pie", SheetAct::InsertChart("pie"), pal, cx))
                    .into_any_element(),
            ))
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
                .child(
                    div()
                        .w_full()
                        .text_size(px(10.))
                        .text_color(pal.dim)
                        .text_center()
                        .child(title.to_string()),
                )
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
            .child(group(
                "Proofing",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(None, "Spelling", SheetAct::Todo, pal, cx))
                    .into_any_element(),
            ))
            .child(group(
                "Comments",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(None, "New Comment", SheetAct::NewComment, pal, cx))
                    .child(self.sheet_lb(None, "Delete", SheetAct::DeleteComment, pal, cx))
                    .child(self.sheet_lb(None, "Previous", SheetAct::PrevComment, pal, cx))
                    .child(self.sheet_lb(None, "Next", SheetAct::NextComment, pal, cx))
                    .into_any_element(),
            ))
            .child(group(
                "Protect",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(
                        Some("lock"),
                        if self.sheet_protected() {
                            "Unprotect Sheet"
                        } else {
                            "Protect Sheet"
                        },
                        SheetAct::ProtectSheet,
                        pal,
                        cx,
                    ))
                    .child(self.sheet_lb(None, "Protect Workbook", SheetAct::Todo, pal, cx))
                    .into_any_element(),
            ))
            .into_any_element()
    }

    /// The View tab: a Window group with Freeze Panes, like Excel.
    fn sheet_view_ribbon(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let frozen = self
            .active_sheet()
            .is_some_and(|v| v.sheet().freeze != (0, 0));
        let group = |title: &str, body: AnyElement| -> AnyElement {
            v_flex()
                .h(px(94.))
                .px_1p5()
                .py(px(3.))
                .justify_between()
                .border_r_1()
                .border_color(pal.border)
                .child(div().flex_1().flex().items_center().child(body))
                .child(
                    div()
                        .w_full()
                        .text_size(px(10.))
                        .text_color(pal.dim)
                        .text_center()
                        .child(title.to_string()),
                )
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
            .child(group(
                "Window",
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(
                        None,
                        if frozen {
                            "Unfreeze Panes"
                        } else {
                            "Freeze Panes"
                        },
                        SheetAct::FreezePanes,
                        pal,
                        cx,
                    ))
                    .into_any_element(),
            ))
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
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(pal.dim)
                                .child(title.to_string()),
                        )
                        .when(launcher, |d| {
                            d.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(pal.dim)
                                    .child("\u{2921}"),
                            )
                        }),
                )
                .into_any_element()
        };
        let row = |kids: Vec<AnyElement>| {
            h_flex()
                .items_center()
                .gap(px(2.))
                .children(kids)
                .into_any_element()
        };
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
            .child(group(
                "Clipboard",
                true,
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(self.sheet_lb(Some("paste"), "Paste", SheetAct::Paste, pal, cx))
                    .child(col(vec![
                        self.sheet_rb(Some("cut"), "Cut", SheetAct::Cut, pal, cx),
                        self.sheet_rb(Some("copy"), "Copy", SheetAct::Copy, pal, cx),
                        self.sheet_rb(None, "Format Painter", SheetAct::Todo, pal, cx),
                    ]))
                    .into_any_element(),
            ))
            // Font: name/size combos + grow/shrink; then B/I/U, borders, fill, colour.
            .child(group(
                "Font",
                true,
                col(vec![
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
                ]),
            ))
            // Alignment: top/mid/bottom + wrap; then left/center/right, indent, merge.
            .child(group(
                "Alignment",
                true,
                col(vec![
                    row(vec![
                        self.sheet_gb("\u{2580}", SheetAct::Todo, pal, cx),
                        self.sheet_gb("\u{25AC}", SheetAct::Todo, pal, cx),
                        self.sheet_gb("\u{2584}", SheetAct::Todo, pal, cx),
                        self.sheet_rb(None, "Wrap Text", SheetAct::WrapText, pal, cx),
                    ]),
                    row(vec![
                        self.sheet_ib(
                            "align-left",
                            SheetAct::AlignL,
                            matches!(xf.align, gridcore::sheet::Align::Left),
                            pal,
                            cx,
                        ),
                        self.sheet_ib(
                            "align-center",
                            SheetAct::AlignC,
                            matches!(xf.align, gridcore::sheet::Align::Center),
                            pal,
                            cx,
                        ),
                        self.sheet_ib(
                            "align-right",
                            SheetAct::AlignR,
                            matches!(xf.align, gridcore::sheet::Align::Right),
                            pal,
                            cx,
                        ),
                        self.sheet_ib("indent-decrease", SheetAct::Todo, false, pal, cx),
                        self.sheet_rb(None, "Row Height", SheetAct::RowHeight, pal, cx),
                        self.sheet_rb(None, "Merge", SheetAct::Merge, pal, cx),
                    ]),
                ]),
            ))
            // Number: format combo; then currency/percent/comma + decimals.
            .child(group(
                "Number",
                true,
                col(vec![
                    row(vec![self.sheet_numfmt_combo(pal, cx)]),
                    row(vec![
                        self.sheet_gb("$", SheetAct::Currency, pal, cx),
                        self.sheet_gb("%", SheetAct::Percent, pal, cx),
                        self.sheet_gb(",", SheetAct::Comma, pal, cx),
                        self.sheet_gb("\u{2192}.0", SheetAct::Todo, pal, cx),
                        self.sheet_gb(".00\u{2190}", SheetAct::Todo, pal, cx),
                    ]),
                ]),
            ))
            // Styles: Conditional Formatting, Format as Table, Cell Styles.
            .child(group(
                "Styles",
                false,
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_0p5()
                    .child(self.sheet_lb(
                        None,
                        "Conditional Formatting",
                        SheetAct::CondFormat,
                        pal,
                        cx,
                    ))
                    .child(self.sheet_lb(
                        Some("table"),
                        "Format as Table",
                        SheetAct::FormatAsTable,
                        pal,
                        cx,
                    ))
                    .child(self.sheet_lb(None, "Cell Styles", SheetAct::Todo, pal, cx))
                    .into_any_element(),
            ))
            // Cells: Insert, Delete, Format.
            .child(group(
                "Cells",
                false,
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(self.sheet_rb(None, "Insert Row", SheetAct::InsertRow, pal, cx))
                            .child(self.sheet_rb(None, "Insert Col", SheetAct::InsertCol, pal, cx)),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(self.sheet_rb(None, "Delete Row", SheetAct::DeleteRow, pal, cx))
                            .child(self.sheet_rb(None, "Delete Col", SheetAct::DeleteCol, pal, cx)),
                    )
                    .child(self.sheet_lb(None, "Format", SheetAct::FormatCells, pal, cx))
                    .into_any_element(),
            ))
            // Editing: AutoSum/Fill/Clear column + Sort & Filter, Find & Select.
            .child(group(
                "Editing",
                false,
                h_flex()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .child(col(vec![
                        self.sheet_rb(None, "\u{03A3} AutoSum", SheetAct::AutoSum, pal, cx),
                        self.sheet_rb(None, "Fill", SheetAct::Todo, pal, cx),
                        self.sheet_rb(Some("clear-format"), "Clear", SheetAct::Todo, pal, cx),
                    ]))
                    .child(col(vec![
                        self.sheet_rb(
                            Some("sort"),
                            "Sort A \u{2192} Z",
                            SheetAct::SortAsc,
                            pal,
                            cx,
                        ),
                        self.sheet_rb(
                            Some("sort"),
                            "Sort Z \u{2192} A",
                            SheetAct::SortDesc,
                            pal,
                            cx,
                        ),
                        self.sheet_rb(
                            Some("sort"),
                            "Custom Sort\u{2026}",
                            SheetAct::CustomSort,
                            pal,
                            cx,
                        ),
                        self.sheet_rb(None, "Filter", SheetAct::Filter, pal, cx),
                        self.sheet_rb(None, "Remove Dup", SheetAct::RemoveDuplicates, pal, cx),
                    ]))
                    .child(self.sheet_lb(Some("find"), "Find & Select", SheetAct::Todo, pal, cx))
                    .into_any_element(),
            ))
            .into_any_element()
    }

    fn render_group(
        &self,
        g: &rs::Group<Act>,
        icon_only: bool,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let controls: Vec<AnyElement> = g
            .items
            .iter()
            .map(|c| self.render_control(c, icon_only, pal, cx))
            .collect();
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

    fn render_control(
        &self,
        c: &Control<Act>,
        icon_only: bool,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                            .children(
                                chunk
                                    .iter()
                                    .map(|cm| self.icon_btn(cm, !icon_only, pal, cx)),
                            )
                            .into_any_element()
                    })
                    .collect();
                h_flex()
                    .items_start()
                    .gap_1()
                    .children(cols)
                    .into_any_element()
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
                v_flex()
                    .items_start()
                    .gap(px(2.))
                    .children(rendered)
                    .into_any_element()
            }
            Control::Gallery(gal) => self.style_gallery(gal, pal, cx),
            Control::Separator => div()
                .w(px(1.))
                .h(px(44.))
                .bg(pal.border)
                .mx_1()
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// The Styles gallery: a row of thumbnail boxes, each showing its name in that
    /// style's own weight/size (Word's Style gallery).
    fn style_gallery(
        &self,
        gal: &rs::Gallery<Act>,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cur = self.tabs.get(self.active).and_then(|t| {
            if let Surface::Doc(ed) = &t.surface {
                ed.caret_para_style()
            } else {
                None
            }
        });
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
                    .child(
                        div()
                            .text_size(px(size))
                            .font_weight(weight)
                            .text_color(pal.fg)
                            .overflow_hidden()
                            .child(SharedString::from(it.label)),
                    )
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
            .items_center()
            .gap_1()
            .children(boxes)
            .into_any_element()
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
    fn combo_box(
        &self,
        cmd: &rs::Cmd<Act>,
        wide: bool,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let props = self.caret_run_props();
        let value: SharedString = if cmd.id == "fontname" {
            props
                .and_then(|p| p.font)
                .unwrap_or_else(|| "Calibri".into())
                .into()
        } else {
            props
                .and_then(|p| p.size_half_pts)
                .map(|h| {
                    let s = h as f32 / 2.0;
                    if s.fract() == 0.0 {
                        format!("{}", s as u32)
                    } else {
                        format!("{s}")
                    }
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
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.fg)
                    .overflow_hidden()
                    .child(value),
            )
            .child(
                div()
                    .text_size(px(8.))
                    .text_color(pal.dim)
                    .child("\u{25BE}"),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    /// A large icon-over-label ribbon button (e.g. Paste, Table).
    fn large_btn(&self, cmd: &rs::Cmd<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let act = cmd.act;
        let on = self.act_active(act);
        let tip: SharedString = cmd.label.into();
        let keytip =
            (self.keytips == KeyTip::Commands && !cmd.key_tip.is_empty()).then_some(cmd.key_tip);
        // Wrap a multi-word label at a word boundary rather than breaking mid-word.
        let mut label = v_flex().items_center();
        for ln in label_lines(cmd.label) {
            label = label.child(
                div()
                    .text_size(px(11.))
                    .text_color(pal.fg)
                    .child(SharedString::from(ln)),
            );
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
            .when(on, |d| {
                d.bg(Hsla {
                    a: 0.20,
                    ..hsla_u(BRAND)
                })
            })
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
            Bold => rp.is_some_and(|p| p.bold),
            Italic => rp.is_some_and(|p| p.italic),
            Underline => rp.is_some_and(|p| p.underline),
            Strike => rp.is_some_and(|p| p.strike),
            Super => rp.is_some_and(|p| p.vert_align == VertAlign::Superscript),
            Sub => rp.is_some_and(|p| p.vert_align == VertAlign::Subscript),
            AlignL => pp.is_some_and(|p| p.align == Align::Left),
            AlignC => pp.is_some_and(|p| p.align == Align::Center),
            AlignR => pp.is_some_and(|p| p.align == Align::Right),
            AlignJ => pp.is_some_and(|p| p.align == Align::Justify),
            Bullets => doc.is_some_and(|ed| ed.all_in_list(NUM_BULLET)),
            Numbers => doc.is_some_and(|ed| ed.all_in_list(NUM_DECIMAL)),
            ParaBorders => pp.is_some_and(|p| p.borders.bottom.is_some()),
            ShowHide => self.show_marks,
            ToggleComments => self.show_comments,
            ToggleNav => self.show_nav,
            ToggleNotes => self.show_notes,
            PrintLayout => self.page_view,
            ToggleRuler => self.show_ruler,
            _ => false,
        }
    }

    fn icon_btn(
        &self,
        cmd: &rs::Cmd<Act>,
        show_label: bool,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let act = cmd.act;
        let tip = cmd.tip;
        let on = self.act_active(act);
        let tip_text: SharedString = if tip.shortcut.is_empty() {
            tip.title.into()
        } else {
            format!("{}  \u{00b7}  {}", tip.title, tip.shortcut).into()
        };
        let keytip =
            (self.keytips == KeyTip::Commands && !cmd.key_tip.is_empty()).then_some(cmd.key_tip);
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
            .when(on, |d| {
                d.bg(Hsla {
                    a: 0.20,
                    ..hsla_u(BRAND)
                })
                .border_1()
                .border_color(hsla_u(BRAND))
            })
            .when(!on, |d| {
                d.border_1().border_color(gpui::transparent_black())
            })
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(cmd.icon.0, 16., pal.fg))
            .when(show_label, |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(pal.fg)
                        .child(SharedString::from(cmd.label)),
                )
            })
            .when_some(keytip, |d, k| d.child(keytip_badge(k)))
            .tooltip(move |window, cx| Tooltip::new(tip_text.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    fn backstage_view(
        &self,
        bg: Hsla,
        fg: Hsla,
        dim: Hsla,
        sidebar: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rail_item =
            |cx: &mut Context<Self>,
             id: &'static str,
             label: &'static str,
             f: fn(&mut Docxy, &mut Window, &mut Context<Docxy>)| {
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
            .child(rail_item(cx, "bs-open", "Open\u{2026}", |t, w, cx| {
                t.open_file(w, cx)
            }))
            .child(rail_item(cx, "bs-save", "Save", |t, w, cx| {
                t.save_active(w, cx)
            }))
            .child(rail_item(cx, "bs-saveas", "Save As\u{2026}", |t, w, cx| {
                t.save_as(w, cx)
            }))
            .child(rail_item(cx, "bs-close", "Close", |t, w, cx| {
                let a = t.active;
                t.backstage = false;
                t.close_tab(a, w, cx);
            }));

        let pane = if self.bs_new {
            let card = |cx: &mut Context<Self>,
                        id: &'static str,
                        glyph: &'static str,
                        name: &'static str,
                        sub: &'static str,
                        kind: Kind| {
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
                    .child(
                        div()
                            .text_color(fg)
                            .font_weight(FontWeight::BOLD)
                            .child(name),
                    )
                    .child(div().text_size(px(11.)).text_color(dim).child(sub))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.add_tab(kind, window, cx)),
                    )
            };
            v_flex()
                .flex_1()
                .h_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(fg)
                        .child("New"),
                )
                .child(
                    h_flex()
                        .gap_4()
                        .child(card(
                            cx,
                            "new-doc-card",
                            Kind::Docx.glyph(),
                            "Document",
                            "Blank .docx",
                            Kind::Docx,
                        ))
                        .child(card(
                            cx,
                            "new-xls-card",
                            Kind::Xlsx.glyph(),
                            "Spreadsheet",
                            "Blank .xlsx",
                            Kind::Xlsx,
                        ))
                        .child(card(
                            cx,
                            "new-mail-card",
                            Kind::Look.glyph(),
                            "Mail",
                            "New message",
                            Kind::Look,
                        )),
                )
                .into_any_element()
        } else {
            let active = self.tabs.get(self.active);
            let (cur_title, cur_path) = active
                .map(|t| {
                    (
                        t.title.to_string(),
                        t.path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "not saved yet".into()),
                    )
                })
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
                .child(
                    div()
                        .text_size(px(22.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(fg)
                        .child(cur_title),
                )
                .child(div().text_size(px(12.)).text_color(dim).child(cur_path))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(BRAND))
                        .mt_4()
                        .child("Open"),
                )
                .child(v_flex().gap_0p5().children(recents))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(BRAND))
                        .mt_4()
                        .child("Settings"),
                )
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
                                .border_color(if self.ask_on_close {
                                    hsla_u(BRAND)
                                } else {
                                    dim
                                })
                                .bg(if self.ask_on_close {
                                    hsla_u(BRAND)
                                } else {
                                    Hsla { a: 0., ..fg }
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(self.ask_on_close, |d| {
                                    d.child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(FILE_FG))
                                            .child("\u{2713}"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_color(fg)
                                .child("Ask before closing with unsaved changes"),
                        )
                        .on_click(cx.listener(|this, _, _w, cx| {
                            this.ask_on_close = !this.ask_on_close;
                            this.persist();
                            cx.notify();
                        })),
                )
                .child(div().text_size(px(11.)).text_color(dim).child(
                    "Off: closing is silent — your work is always kept and reopened next launch.",
                ))
                .into_any_element()
        };

        h_flex()
            .size_full()
            .bg(bg)
            .child(rail)
            .child(pane)
            .into_any_element()
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
            // Every side panel takes its width out of the grid's; miss one and
            // the grid lays itself out underneath it.
            let mut panel = 0.0;
            if self.active_pivot().is_some() {
                panel += SIDE_PANEL_W;
            }
            if self.panel_chart_shown().is_some() {
                panel += SIDE_PANEL_W;
            }
            let w = f32::from(window.viewport_size().width) - panel;
            self.reconcile_sheet_hscroll((w - SHEET_GUT).max(120.0));
            self.sheet_grid_w = w;
            w
        } else {
            0.0
        };
        // List data-validation options for the selected cell (dropdown), if any.
        let sheet_dv = self
            .active_is_sheet()
            .then(|| self.dv_list_values())
            .flatten();
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
        let pal = Pal::of(cx);

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
                    .child(SharedString::from(format!(
                        "{} {}{}",
                        tb.kind.glyph(),
                        tb.title,
                        mark
                    )))
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
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(i, window, cx)),
                    )
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
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(BRAND))
                        .child("docxy"),
                )
                // Quick Access Toolbar: Undo / Redo (Word keeps these here, not on
                // the ribbon).
                .child(
                    h_flex()
                        .items_center()
                        .gap_0p5()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(qat_btn(
                            "qat-undo",
                            "undo",
                            "Undo (Ctrl+Z)",
                            pal,
                            cx.listener(|this, _, window, cx| {
                                this.with_editor(window, cx, |e| {
                                    e.undo();
                                })
                            }),
                        ))
                        .child(qat_btn(
                            "qat-redo",
                            "redo",
                            "Redo (Ctrl+Y)",
                            pal,
                            cx.listener(|this, _, window, cx| {
                                this.with_editor(window, cx, |e| {
                                    e.redo();
                                })
                            }),
                        )),
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
                        .child(
                            Button::new("theme")
                                .ghost()
                                .xsmall()
                                .label(theme_pref.label())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.cycle_theme(window, cx)),
                                ),
                        ),
                ),
        );

        if self.backstage {
            let backstage = self.backstage_view(bg, fg, dim, sidebar, cx);
            return v_flex()
                .size_full()
                .bg(bg)
                .track_focus(&self.focus)
                .child(title_bar)
                .child(backstage)
                .into_any_element();
        }

        let is_doc = matches!(
            self.tabs.get(self.active).map(|t| &t.surface),
            Some(Surface::Doc(_))
        );
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
        let picker_bar = (is_doc)
            .then_some(self.picker)
            .flatten()
            .map(|k| self.picker_bar(k, pal, cx));
        let sheet_pick_bar = self
            .active_is_sheet()
            .then_some(self.sheet_pick)
            .flatten()
            .map(|p| self.sheet_picker_bar(p, pal, cx));
        let sheet_numfmt_bar = (self.active_is_sheet() && self.sheet_numfmt_open)
            .then(|| self.sheet_numfmt_bar(pal, cx));
        let sheet_fmt_panel = (self.active_is_sheet() && self.sheet_fmt_open)
            .then(|| self.sheet_format_panel(pal, cx));
        let sheet_find =
            (self.active_is_sheet() && self.find_open).then(|| self.sheet_find_bar(pal, cx));
        let sheet_comment = self
            .sheet_comment_edit
            .clone()
            .map(|buf| self.sheet_comment_bar(&buf, pal, cx));
        let sheet_cf = self
            .sheet_cf_edit
            .clone()
            .map(|buf| self.sheet_cf_bar(&buf, pal, cx));
        let sheet_dv_bar = self
            .sheet_dv_edit
            .clone()
            .map(|buf| self.sheet_dv_edit_bar(&buf, pal, cx));
        let sheet_filter = self
            .sheet_filter_edit
            .clone()
            .map(|buf| self.sheet_filter_bar(&buf, pal, cx));
        let sheet_ttc = self
            .sheet_ttc_edit
            .clone()
            .map(|buf| self.sheet_ttc_bar(&buf, pal, cx));
        let sheet_sort = self
            .sheet_sort_edit
            .clone()
            .map(|buf| self.sheet_sort_bar(&buf, pal, cx));
        let sheet_rowh = self
            .sheet_rowh_edit
            .clone()
            .map(|buf| self.sheet_rowh_bar(&buf, pal, cx));
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
                        Pal {
                            fg: hsla_u(0x202020),
                            dim: hsla_u(0x808080),
                            border: hsla_u(0xcccccc),
                            panel: hsla_u(0xf0f0f0),
                            hover: Hsla {
                                a: 0.08,
                                ..hsla_u(0x000000)
                            },
                            sel: pal.sel,
                        }
                    } else {
                        pal
                    };
                    // While a header/footer is being edited the body is inactive
                    // (no caret, clicks inert) so it visually recedes.
                    let hf = tab.hf_edit.as_ref();
                    let ctx = RenderCtx {
                        caret_path: &editor.caret.path,
                        caret_off: editor.caret.offset,
                        spans: &spans,
                        ent: &ent,
                        pal: doc_pal,
                        marks: self.show_marks,
                        zoom: self.zoom,
                        active: hf.is_none(),
                        meas: &measurer,
                        hf_width: None,
                    };
                    let body = &editor.doc.body;
                    if self.page_view {
                        // Print Layout: split the body into discrete white page sheets
                        // (section margins), stacked on a grey canvas.
                        let geom = tab.pkg.as_ref().map(|p| p.page_geom()).unwrap_or_default();
                        let zoom = self.zoom;
                        let tw = move |t: i32| px(zoom * (t.max(0) as f32) / 15.0); // twips → px @ ~96dpi, zoomed
                        let canvas = if self.applied == Some(ThemeMode::Dark) {
                            hsla_u(0x2b2b2b)
                        } else {
                            hsla_u(0x9a9a9a)
                        };
                        let content_h = (geom.h - geom.mt - geom.mb).max(1) as f32 / 15.0;
                        let content_w = (geom.w - geom.ml - geom.mr).max(1) as f32 / 15.0;
                        // Newspaper columns: flow the body into N columns per page.
                        let ncols = geom.cols.max(1) as usize;
                        let colgap = geom.col_space.max(0) as f32 / 15.0;
                        let col_w = if ncols > 1 {
                            ((content_w - colgap * (ncols as f32 - 1.0)) / ncols as f32).max(1.0)
                        } else {
                            content_w
                        };
                        let pages: Vec<Vec<(usize, usize)>> = if ncols > 1 {
                            paginate_cols(body, content_h, col_w, ncols)
                        } else {
                            paginate(body, content_h, content_w)
                                .into_iter()
                                .map(|r| vec![r])
                                .collect()
                        };
                        let show_ruler = self.show_ruler;
                        // Per-page header/footer. A section can carry distinct
                        // first-page (w:titlePg) and even-page (evenAndOddHeaders)
                        // variants; every other page uses the "default" one.
                        let pkg = tab.pkg.as_ref();
                        let title_pg = pkg.is_some_and(|p| p.has_title_pg());
                        let even_odd = pkg.is_some_and(|p| p.has_even_odd());
                        let refp = |kind: &str, wt: &str| {
                            pkg.is_some_and(|p| {
                                docxcore::load::header_footer_ref_rid(p.sect_pr(), kind, wt)
                                    .is_some()
                            })
                        };
                        let (h_first_ref, h_even_ref) = (
                            refp("headerReference", "first"),
                            refp("headerReference", "even"),
                        );
                        let (f_first_ref, f_even_ref) = (
                            refp("footerReference", "first"),
                            refp("footerReference", "even"),
                        );
                        let parse = |is_h: bool, wt: &str| {
                            pkg.map(|p| header_footer_blocks_typed(p, is_h, wt))
                                .unwrap_or_default()
                        };
                        let (hdef, hfirst, heven) = (
                            parse(true, "default"),
                            parse(true, "first"),
                            parse(true, "even"),
                        );
                        let (fdef, ffirst, feven) = (
                            parse(false, "default"),
                            parse(false, "first"),
                            parse(false, "even"),
                        );
                        let variant_for = |page1: usize, is_h: bool| -> &'static str {
                            let (fr, ev) = if is_h {
                                (h_first_ref, h_even_ref)
                            } else {
                                (f_first_ref, f_even_ref)
                            };
                            if page1 == 1 && title_pg && fr {
                                "first"
                            } else if page1.is_multiple_of(2) && even_odd && ev {
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
                        let hf_ctx = hf.map(|h| RenderCtx {
                            caret_path: &h.editor.caret.path,
                            caret_off: h.editor.caret.offset,
                            spans: &hf_spans,
                            ent: &ent,
                            pal: doc_pal,
                            marks: false,
                            zoom: self.zoom,
                            active: true,
                            meas: &measurer,
                            hf_width: Some(hf_w),
                        });
                        // The first page whose region+variant matches the one being
                        // edited is the editable page (fallback page 0, so the surface
                        // is always visible even for a not-yet-shown variant).
                        let edit_page = hf.map(|h| {
                            (0..pages.len())
                                .find(|&i| variant_for(i + 1, h.is_header) == h.variant)
                                .unwrap_or(0)
                        });
                        let has_hf = [&hdef, &hfirst, &heven, &fdef, &ffirst, &feven]
                            .iter()
                            .any(|v| !v.is_empty())
                            || hf.is_some();
                        // One region's margin content for a given page: the live editor
                        // blocks (editable on the edit page), else the read-only variant.
                        let region_children = |pi: usize, is_h: bool| -> Vec<AnyElement> {
                            let dv = variant_for(pi + 1, is_h);
                            if let (Some(h), Some(ep)) = (hf, edit_page) {
                                if h.is_header == is_h {
                                    if pi == ep {
                                        return h
                                            .editor
                                            .doc
                                            .body
                                            .iter()
                                            .enumerate()
                                            .map(|(i, b)| {
                                                block_el(b, vec![i], None, hf_ctx.unwrap())
                                            })
                                            .collect();
                                    }
                                    let blocks: &[Block] = if dv == h.variant {
                                        &h.editor.doc.body
                                    } else {
                                        pick(is_h, dv)
                                    };
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
                                let blocks: Vec<AnyElement> = (s..e)
                                    .map(|i| {
                                        block_el(&body[i], vec![i], markers[i].as_deref(), ctx)
                                    })
                                    .collect();
                                return v_flex()
                                    .w_full()
                                    .gap_1()
                                    .children(blocks)
                                    .into_any_element();
                            }
                            let column_els: Vec<AnyElement> = cols
                                .iter()
                                .map(|&(s, e)| {
                                    let blocks: Vec<AnyElement> = (s..e)
                                        .map(|i| {
                                            block_el(&body[i], vec![i], markers[i].as_deref(), ctx)
                                        })
                                        .collect();
                                    v_flex()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .gap_1()
                                        .children(blocks)
                                        .into_any_element()
                                })
                                .collect();
                            h_flex()
                                .w_full()
                                .items_start()
                                .gap(tw(geom.col_space))
                                .children(column_els)
                                .into_any_element()
                        };
                        let sheets: Vec<AnyElement> = pages
                            .iter()
                            .enumerate()
                            .map(|(pi, cols_ranges)| {
                                let hdr_children = region_children(pi, true);
                                let ftr_children = region_children(pi, false);
                                let edit_hdr_here =
                                    hf.is_some_and(|h| h.is_header) && Some(pi) == edit_page;
                                let edit_ftr_here =
                                    hf.is_some_and(|h| !h.is_header) && Some(pi) == edit_page;
                                let page_base = v_flex()
                                    .w(tw(geom.w))
                                    .min_h(tw(geom.h))
                                    .bg(hsla_u(0xffffff))
                                    .text_color(doc_pal.fg)
                                    .border_1()
                                    .border_color(hsla_u(0xd0d0d0));
                                // Tint the region actively being edited on this page.
                                let tint = Hsla {
                                    a: 0.5,
                                    ..hsla_u(0xeef4ff)
                                };
                                let hdr_bg = if edit_hdr_here {
                                    tint
                                } else {
                                    hsla_u(0xffffff)
                                };
                                let ftr_bg = if edit_ftr_here {
                                    tint
                                } else {
                                    hsla_u(0xffffff)
                                };
                                let page = if has_hf {
                                    // Header in the top margin, content in the middle, footer
                                    // in the bottom margin. The body area exits header/footer
                                    // editing on click (Word's "click the document to leave").
                                    let mut mid = v_flex()
                                        .flex_1()
                                        .pl(tw(geom.ml))
                                        .pr(tw(geom.mr))
                                        .child(build_mid(cols_ranges));
                                    if hf.is_some() {
                                        let ent2 = ent.clone();
                                        mid = mid.cursor_pointer().on_mouse_down(
                                            MouseButton::Left,
                                            move |_ev, window, cx| {
                                                ent2.update(cx, |this, cx| {
                                                    this.exit_hf(window, cx)
                                                });
                                            },
                                        );
                                    }
                                    page_base
                                        .child(
                                            div()
                                                .min_h(tw(geom.mt))
                                                .pt(tw(geom.mt / 2))
                                                .pl(tw(geom.ml))
                                                .pr(tw(geom.mr))
                                                .bg(hdr_bg)
                                                .children(hdr_children),
                                        )
                                        .child(mid)
                                        .child(
                                            div()
                                                .min_h(tw(geom.mb))
                                                .pl(tw(geom.ml))
                                                .pr(tw(geom.mr))
                                                .bg(ftr_bg)
                                                .children(ftr_children),
                                        )
                                } else {
                                    page_base
                                        .pt(tw(geom.mt))
                                        .pr(tw(geom.mr))
                                        .pb(tw(geom.mb))
                                        .pl(tw(geom.ml))
                                        .child(build_mid(cols_ranges))
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
                        let blocks: Vec<AnyElement> = body
                            .iter()
                            .enumerate()
                            .map(|(i, b)| block_el(b, vec![i], markers[i].as_deref(), ctx))
                            .collect();
                        v_flex()
                            .id("doc-scroll")
                            .track_scroll(&self.doc_scroll)
                            .flex_1()
                            .h_full()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .bg(bg)
                            .text_color(fg)
                            .px(px(48.))
                            .py(px(28.))
                            .gap_1()
                            .children(blocks)
                            .into_any_element()
                    }
                }
                Surface::Sheet(v) => sheet_el(
                    v,
                    &cx.entity(),
                    self.sheet_rename.clone(),
                    self.sheet_comment_edit.is_some(),
                    sheet_dv.clone(),
                    self.sheet_dv_open,
                    sheet_grid_w,
                    self.grid_overlay(),
                    self.chart_ui(),
                    cx,
                )
                .into_any_element(),
                Surface::Placeholder => placeholder(tab.kind, bg, dim).into_any_element(),
            },
            None => v_flex()
                .flex_1()
                .bg(bg)
                .items_center()
                .justify_center()
                .text_color(dim)
                .child("No documents — File \u{203A} New")
                .into_any_element(),
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

        let zoom_btn =
            |cx: &mut Context<Self>, id: &'static str, glyph: &'static str, delta: f32| {
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
            .child(
                self.tabs
                    .get(self.active)
                    .map(|t| t.status.clone())
                    .unwrap_or_default(),
            )
            .when_some(stats_text, |d, s| {
                d.child(div().text_color(dim).child("·"))
                    .child(div().text_color(dim).child(s))
            })
            .child(div().flex_1())
            .child(if self.active_is_sheet() {
                "type or F2 to edit · Enter/Tab to move · =formula · Ctrl+S save"
            } else {
                "type · Ctrl+B/I/U · Ctrl+F find · Ctrl+C/X/V · Ctrl+Z/Y · Ctrl+S"
            })
            // Zoom controls (Word's bottom-right zoom).
            .child(zoom_btn(cx, "zoom-out", "\u{2212}", -0.1))
            .child(
                div()
                    .id("zoom-pct")
                    .min_w(px(34.))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|d| d.text_color(fg))
                    .child(SharedString::from(format!(
                        "{}%",
                        (self.zoom * 100.0).round() as i32
                    )))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.zoom = 1.0;
                        this.refocus(window, cx);
                    })),
            )
            .child(zoom_btn(cx, "zoom-in", "+", 0.1));

        // The body is the document, flanked by the navigation and comments panes.
        let nav_panel = (is_doc && self.show_nav).then(|| self.nav_panel(pal, cx));
        let comments_panel = (is_doc && self.show_comments).then(|| self.comments_panel(pal, cx));
        let notes_panel = (is_doc && self.show_notes).then(|| self.notes_panel(pal, cx));
        // The PivotTable Fields panel, shown when a pivot output sheet is active.
        let pivot_panel = self.active_pivot().map(|i| self.pivot_panel(i, pal, cx));
        // The Chart panel takes the same slot. It is gated on the chart it
        // SHOWS, not the one selected: deselecting leaves it open (see
        // `PanelEvent`), so a click on the grid can no longer close it mid-edit.
        let chart_panel =
            (!is_doc && self.panel_chart_shown().is_some()).then(|| self.chart_panel(pal, cx));
        let body = h_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            // Right-click anywhere in the document body opens the context menu.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                    this.context_menu = Some(ev.position);
                    cx.notify();
                }),
            )
            .when_some(nav_panel, |d, n| d.child(n))
            .child(content)
            .when_some(comments_panel, |d, p| d.child(p))
            .when_some(notes_panel, |d, p| d.child(p))
            .when_some(pivot_panel, |d, p| d.child(p))
            .when_some(chart_panel, |d, p| d.child(p));
        let context_menu = self
            .context_menu
            .map(|at| self.context_menu_el(at, pal, cx));
        let mini_bar = (is_doc && self.context_menu.is_none())
            .then_some(self.mini_bar)
            .flatten()
            .map(|at| self.mini_bar_el(at, pal, cx));

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
                // A grid drag released off the grid (over the ribbon, the sheet
                // tabs, outside the window) ends here; idempotent, so the grid's
                // own handler having run first costs nothing.
                this.grid_release(cx);
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
fn scroll_col0_for_sel(
    col_w_px: impl Fn(u32) -> f32,
    col0: u32,
    fc: u32,
    sc: u32,
    avail: f32,
) -> u32 {
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

// ---- the pointed range's dashed border (pure; unit-tested below) ---------
//
// ⚠️ Correction to this feature's discovery notes: gpui CAN draw a dashed
// border natively, so a dash is NOT a per-dash element. Citations, all at the
// gpui rev this workspace pins (zed 8276687, `suite/Cargo.lock`):
//
//   * `Styled::border_dashed()` — `crates/gpui/src/styled.rs:500`, sets
//     `style.border_style = Some(BorderStyle::Dashed)`.
//   * `BorderStyle::{Solid, Dashed}` — `crates/gpui/src/scene.rs:597`.
//   * The dashes are drawn by the quad shader, on every backend we ship:
//     `crates/gpui_windows/src/shaders.hlsl:664`,
//     `crates/gpui_wgpu/src/shaders.wgsl:693`, and
//     `crates/gpui_macos/src/shaders.metal` (83 `dash` hits).
//   * `PathBuilder::dash_array()` — `crates/gpui/src/path_builder.rs:108` —
//     also exists, for stroked paths. We don't need it: a quad border is
//     cheaper and lays out with the cell.
//
// What is still ours to compute is WHICH edges of the range a given cell owns,
// because the grid renders cell by cell — which is also the approach that
// avoids reconstructing row positions from a scroll offset (they drift; row
// heights are content-driven).

/// The pointed range's border width in px. The dash pitch follows from it:
/// gpui's shader lays a dash of `2 × width` and a gap of `1 × width`, so the
/// pitch is `3 × width` and 2px gives Excel's ~4px dash on a 100% display.
const RANGE_BORDER_W: f32 = 2.0;

/// gpui's dash pattern, from the shader cited above: dash `2 × border width`,
/// gap `1 × border width`. Kept as constants because every number below —
/// pitch, the solid-fallback threshold, the dash count — is derived from them,
/// and if gpui ever changes the pattern these are the two values to edit.
const DASH_LEN_PER_W: f32 = 2.0;
const DASH_GAP_PER_W: f32 = 1.0;

/// Past this many *visible* boundary cells the border is drawn solid instead of
/// dashed. Because the border is rendered per cell, the count is bounded by the
/// viewport rather than by the range: selecting whole columns costs the same as
/// selecting the screen. At the narrowest column (28px) and shortest row (21px)
/// a 1920×1200 grid shows ~69 × ~55 cells, so the perimeter of even a
/// select-all tops out near 250. The cap is a backstop against a future
/// viewport nobody has, not an operating limit.
const RANGE_BORDER_CELL_CAP: usize = 512;

/// Which sides of the pointed range's border a single cell draws. A cell can
/// own more than one (a corner owns two; a one-cell range owns all four).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct EdgeMask {
    top: bool,
    right: bool,
    bottom: bool,
    left: bool,
}

impl EdgeMask {
    /// Nothing to draw — the cell is inside the range, or outside it.
    fn is_empty(self) -> bool {
        !(self.top || self.right || self.bottom || self.left)
    }
}

/// The sides of `range`'s border that cell `(r, c)` owns. Cells off the range,
/// and cells strictly inside it, own nothing. This is the per-cell question the
/// row renderer asks; `range_border_plan` answers it for a whole viewport.
fn range_edges_at(range: (u32, u32, u32, u32), r: u32, c: u32) -> EdgeMask {
    let (r0, c0, r1, c1) = range;
    if r < r0 || r > r1 || c < c0 || c > c1 {
        return EdgeMask::default();
    }
    EdgeMask {
        top: r == r0,
        right: c == c1,
        bottom: r == r1,
        left: c == c0,
    }
}

/// `range` with its row span pulled onto rows the grid actually DRAWS:
/// `visible` is the sorted list of unhidden rows the renderer walks, so a range
/// whose first or last row is hidden or filtered out has no rendered cell to
/// own that edge and `range_edges_at` would leave the rectangle open on that
/// side. Snapping `r0` up to the first visible row at or after it, and `r1`
/// down to the last visible row at or before it, hands the edge to the cell
/// that is actually against the gap — which is where the eye puts it anyway,
/// since a hidden row collapses to zero height and its neighbours sit flush.
///
/// A range with no visible row at all comes back unchanged: nothing of it is
/// drawn either way, and leaving the value alone keeps this a total function,
/// so a caller mapping over a list (a formula's references, a chart's source
/// areas) keeps its INDICES — which are what pick the colour.
///
/// `rendered` is the number of rows the grid virtualizes over, and it is what
/// tells a HIDDEN row apart from one that is merely past the end of the drawing.
/// `visible` is that span with the hidden rows taken out, so "not in `visible`"
/// alone would also catch every row past it — and a `=SUM(A1:A800)` on a
/// 500-row grid would have its bottom edge pulled back and drawn CLOSED across
/// the last row, saying the reference ends there. A range that runs off the
/// bottom keeps its `r1`: the edge belongs to a row nothing draws, so the
/// rectangle stays open, which is the truthful picture.
fn snap_range_rows(
    range: (u32, u32, u32, u32),
    visible: &[u32],
    rendered: u32,
) -> (u32, u32, u32, u32) {
    let (r0, c0, r1, c1) = range;
    let first = visible.partition_point(|&v| v < r0);
    let past_last = visible.partition_point(|&v| v <= r1);
    if first >= past_last {
        return range;
    }
    let last = if r1 >= rendered {
        r1
    } else {
        visible[past_last - 1]
    };
    (visible[first], c0, last, c1)
}

/// How the pointed range's border is drawn over one viewport: the boundary
/// cells that are actually on screen with the edges each owns, and whether
/// those edges are dashed or (past the cap) solid.
///
/// Test-only, and deliberately so. Nothing draws from a plan — `sheet_row` asks
/// `range_edges_at` per cell, which needs no list, and the cap asks
/// `range_border_cell_count`, which needs no allocation. What the plan is FOR
/// is being the slow, obvious definition those two fast answers are checked
/// against: build every boundary cell, then assert the shortcuts agree.
#[cfg(test)]
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct RangeBorderPlan {
    /// Boundary cells in row-major order, each with the edges it draws.
    cells: Vec<(u32, u32, EdgeMask)>,
    /// False past `RANGE_BORDER_CELL_CAP` — draw the same edges solid.
    dashed: bool,
}

/// The border for `range` clipped to the visible window `view` (both inclusive
/// `(r0, c0, r1, c1)` boxes). Only the perimeter is walked, never the area, so
/// a selection of a million rows costs the rows you can see. An edge that falls
/// outside the window is simply not drawn — Excel clips the same way.
#[cfg(test)]
fn range_border_plan(range: (u32, u32, u32, u32), view: (u32, u32, u32, u32)) -> RangeBorderPlan {
    let (r0, c0, r1, c1) = range;
    let (vr0, vc0, vr1, vc1) = view;
    let (cr0, cr1) = (r0.max(vr0), r1.min(vr1));
    let (cc0, cc1) = (c0.max(vc0), c1.min(vc1));
    if cr0 > cr1 || cc0 > cc1 {
        // Scrolled entirely out of view: nothing to draw, and so no reason to
        // fall back to solid. `dashed: false` here would say "too expensive to
        // dash" about a border that costs nothing — an answer that is only
        // harmless while the window this was clipped against is exactly the one
        // the rows render.
        return RangeBorderPlan {
            cells: Vec::new(),
            dashed: true,
        };
    }
    // A BTreeMap both merges the corners (where two edge runs meet on one cell)
    // and hands the cells back in row-major order, which is the order the rows
    // render in and the order the tests read in.
    let mut cells: std::collections::BTreeMap<(u32, u32), EdgeMask> = Default::default();
    if r0 >= vr0 && r0 <= vr1 {
        for c in cc0..=cc1 {
            cells.entry((r0, c)).or_default().top = true;
        }
    }
    if r1 >= vr0 && r1 <= vr1 {
        for c in cc0..=cc1 {
            cells.entry((r1, c)).or_default().bottom = true;
        }
    }
    if c0 >= vc0 && c0 <= vc1 {
        for r in cr0..=cr1 {
            cells.entry((r, c0)).or_default().left = true;
        }
    }
    if c1 >= vc0 && c1 <= vc1 {
        for r in cr0..=cr1 {
            cells.entry((r, c1)).or_default().right = true;
        }
    }
    let cells: Vec<_> = cells.into_iter().map(|((r, c), e)| (r, c, e)).collect();
    let dashed = cells.len() <= RANGE_BORDER_CELL_CAP;
    RangeBorderPlan { cells, dashed }
}

/// The most rows the grid can have on screen at once, for the cap decision
/// only. `sheet_el` is handed the grid's WIDTH but not its height, so the row
/// side of the viewport has to be bounded by a number rather than measured.
///
/// 128 rows at the 21px row-height floor is a 2688px-tall grid — taller than
/// any display in landscape, so it over-counts every real viewport, which is
/// the safe direction: the cap can only fire sooner, never later. It is not
/// set higher because over-counting is not free — two full columns of 256
/// would clear `RANGE_BORDER_CELL_CAP` on their own and drop a perfectly
/// ordinary tall selection to a solid border.
const GRID_MAX_VISIBLE_ROWS: u32 = 128;

/// Which range wears a border. A focused range field wins: while one has the
/// keyboard the border's whole job is to show what it points at. Otherwise it
/// outlines the selection, but only when that spans more than one cell — a
/// single cell already wears the active ring, and drawing both would be the
/// doubled-up indicator this plan is trying to remove.
///
/// Whether that border is DASHED is a separate question, decided at the call
/// site: only a pointed range dashes. A range swept with the mouse gets the
/// same box, solid.
///
/// `sel` is an Option because the selection can be there and not shown: while a
/// chart owns the selection the grid draws none of its own (`sel_hidden`), and
/// a range it does not outline is a range it must not border either. A POINTED
/// range still wins in that state — that is a chart's own field pointing.
/// Whether the range's border is drawn dashed rather than solid.
///
/// Dashes mean "a field is POINTING at these cells" — they are not decoration
/// for a wide selection. Excel reserves its marching ants the same way, for a
/// copy or a dialog's range picker, and a range swept with the mouse wears a
/// solid border there. Dashing an ordinary selection made a plain drag look
/// like a formula was reading it.
///
/// `within_cap` is the separate cost question (`range_border_dashed`): a border
/// spanning more boundary cells than the cap falls back to solid even while
/// pointing, because the renderer draws it cell by cell.
fn border_is_dashed(pointing: bool, within_cap: bool) -> bool {
    pointing && within_cap
}

fn border_range(
    preview: Option<(u32, u32, u32, u32)>,
    sel: Option<(u32, u32, u32, u32)>,
) -> Option<(u32, u32, u32, u32)> {
    if preview.is_some() {
        return preview;
    }
    let (r0, c0, r1, c1) = sel?;
    ((r0, c0) != (r1, c1)).then_some((r0, c0, r1, c1))
}

/// The selection as the grid is currently WILLING to draw it: `None` while a
/// chart owns the selection, so every indicator keyed to it — the ring, the
/// wash, the header highlight, the border — goes dark together rather than one
/// call site at a time remembering to check.
fn shown_sel(ov: &GridOverlay, sel: (u32, u32, u32, u32)) -> Option<(u32, u32, u32, u32)> {
    (!ov.sel_hidden).then_some(sel)
}

/// Whether `range`'s border is drawn dashed, or falls back to solid because it
/// would cost more than `RANGE_BORDER_CELL_CAP` boundary cells.
///
/// The renderer draws the border cell by cell, so the cost is bounded by the
/// viewport, not by the range: `cols` is the visible column window and
/// `max_rows` the row bound above. The window is anchored on the range's own
/// first row, which over-counts whenever the range starts above the fold — the
/// same safe direction as `GRID_MAX_VISIBLE_ROWS`.
fn range_border_dashed(range: (u32, u32, u32, u32), cols: (u32, u32), max_rows: u32) -> bool {
    let (r0, _, _, _) = range;
    let vr1 = r0.saturating_add(max_rows.saturating_sub(1));
    range_border_cell_count(range, (r0, cols.0, vr1, cols.1)) <= RANGE_BORDER_CELL_CAP as u64
}

/// How many visible boundary cells `range` has inside `view` — the same number
/// `range_border_plan` produces, without producing them.
///
/// The plan builds a map and a vector, and the render path reads one boolean
/// off them and drops both, every frame, for a count the perimeter gives in
/// closed form: `h` boundary ROWS across the clipped width, `v` boundary
/// COLUMNS down the clipped height, less the `h × v` corner cells that are in
/// both. `h` and `v` are 0, 1 or 2 — one edge each for a single-row or
/// single-column range, and 0 for an edge that is scrolled out.
///
/// `u64` throughout because a full-column selection is a million rows before
/// clipping; the clipped result is small, but nothing here relies on that.
fn range_border_cell_count(range: (u32, u32, u32, u32), view: (u32, u32, u32, u32)) -> u64 {
    let (r0, c0, r1, c1) = range;
    let (vr0, vc0, vr1, vc1) = view;
    let (cr0, cr1) = (r0.max(vr0), r1.min(vr1));
    let (cc0, cc1) = (c0.max(vc0), c1.min(vc1));
    if cr0 > cr1 || cc0 > cc1 {
        return 0; // scrolled entirely out of view
    }
    let (w, h) = (cc1 as u64 - cc0 as u64 + 1, cr1 as u64 - cr0 as u64 + 1);
    let on_screen = |a: u32, b: u32, lo: u32, hi: u32| {
        u64::from(a >= lo && a <= hi) + u64::from(b != a && b >= lo && b <= hi)
    };
    let (rows, cols) = (on_screen(r0, r1, vr0, vr1), on_screen(c0, c1, vc0, vc1));
    rows * w + cols * h - rows * cols
}

/// Where gpui's shader will put the dashes along one straight edge, so we can
/// answer "how many, and does this edge dash at all" without a window. It is a
/// model of the shader, not a substitute for it — nothing here is drawn.
#[derive(Clone, PartialEq, Debug)]
struct DashFit {
    /// Dashes along the edge; the first starts at 0 and the last ends flush
    /// with the far end, which is why a straight edge reads as deliberate.
    count: u32,
    /// Each dash's length in px (`2 × border width`).
    dash_px: f32,
    /// Distance between dash starts in px — stretched from the nominal
    /// `3 × width` so the dashes divide the edge evenly.
    pitch_px: f32,
    /// Each dash's start offset from the edge's near end, in px.
    offsets: Vec<f32>,
}

/// Fit dashes to an edge of `edge_px` at `border_w`, mirroring the shader's own
/// arithmetic (`shaders.hlsl:811`): lay a `2W` dash and a `1W` gap, reserve a
/// dash's length at the far end so the edge both starts and ends with one, then
/// stretch the gap to divide what's left evenly.
///
/// `None` means the edge is too short to dash — at or under `4 × border width`
/// the shader gives up and paints it solid, so a 5px sliver of a scrolled-off
/// column reads as a solid tick rather than a lone half-dash.
// Deliberately uncalled: Task 2 took gpui's native `border_dashed()` rather
// than laying the dashes itself, so nothing draws from this. It stays because
// it is the executable half of the note above `RANGE_BORDER_W`: the written
// form of what we read out of the shader, worked through to the numbers it
// produces at our column widths, so "what will a pointed range look like"
// has an answer that can be run.
//
// It is NOT a regression guard, and must not be read as one. Every input is
// ours — `DASH_LEN_PER_W`, `DASH_GAP_PER_W`, `RANGE_BORDER_W` — so a gpui bump
// that changes the shader's pattern leaves these tests green while the dashes
// on screen change. Nothing here can see that happen. After a `cargo update`
// that moves gpui, the only check is re-reading `shaders.hlsl` by hand and
// editing the two `*_PER_W` constants if it has moved.
#[allow(dead_code)]
fn dash_fit(edge_px: f32, border_w: f32) -> Option<DashFit> {
    if edge_px <= 0.0 || border_w <= 0.0 {
        return None;
    }
    let period_per_w = DASH_LEN_PER_W + DASH_GAP_PER_W;
    let px_per_t = border_w * period_per_w; // one dash period, in px
    let dash_t = DASH_LEN_PER_W / period_per_w; // a dash, in dash-space
    // The shader's `max_t`: the edge in dash-space, less the dash it reserves
    // for the far end.
    let max_t = edge_px / px_per_t - dash_t;
    if max_t <= dash_t {
        return None; // `dash_gap > 0.0` fails in the shader → drawn solid
    }
    let gaps = max_t.floor().max(1.0);
    let pitch_px = (max_t / gaps) * px_per_t;
    let count = gaps as u32 + 1;
    Some(DashFit {
        count,
        dash_px: dash_t * px_per_t,
        pitch_px,
        offsets: (0..count).map(|k| k as f32 * pitch_px).collect(),
    })
}

/// A row's pixel height: an explicit `<row ht>` (points) scaled at the app's
/// 15pt≈`base`px, else `base`. Wrapped cells grow the row beyond this at layout
/// time; this is the min-height floor.
fn row_height_px(explicit_pt: Option<f64>, base: f32) -> f32 {
    explicit_pt
        .map(|ht| (ht as f32) * (base / 15.0))
        .unwrap_or(base)
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

/// The last column the grid draws and hit-tests. Every clamp on `col0` has to
/// agree with it: a scroll offset past this renders a window nothing can reach.
const MAX_VISIBLE_COL: u32 = 255;
const SHEET_ROW_H: f32 = 21.0;
const SHEET_GUT: f32 = 46.0;

/// The frozen column-letter header row (with drag-to-resize handles). Rendered
/// once above the virtualized rows so it stays put while they scroll vertically.
fn sheet_col_header(
    view: &SheetView,
    ent: &Entity<Docxy>,
    fc: u32,
    col0: u32,
    cend: u32,
    sel_hidden: bool,
) -> AnyElement {
    use gridcore::sheet::col_name;
    let sh = view.sheet();
    let gridline = hsla_u(0xd9d9d9);
    let freeze_line = hsla_u(0x8a8a8a);
    let head_bg = hsla_u(0xf1f1f1);
    let head_fg = hsla_u(0x5a5a5a);
    let brand = hsla_u(BRAND);
    let (_, c0, _, c1) = view.range();
    let mut header = h_flex().child(
        div()
            .w(px(SHEET_GUT))
            .flex_shrink_0()
            .h(px(SHEET_ROW_H))
            .bg(head_bg)
            .border_r_1()
            .border_b_1()
            .border_color(gridline),
    );
    // Frozen columns 0..fc pinned, then the scrollable window col0..=cend.
    for c in (0..fc).chain(col0..=cend) {
        // Dark while a chart owns the selection, for the same reason the ring is.
        let hl = !sel_hidden && c >= c0 && c <= c1;
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
            div()
                .relative()
                .w(px(col_px(sh.col_width(c))))
                .flex_shrink_0()
                .h(px(SHEET_ROW_H))
                .flex()
                .items_center()
                .justify_center()
                .bg(if hl { brand } else { head_bg })
                .border_r_1()
                .border_b_1()
                .border_color(if on_freeze { freeze_line } else { gridline })
                .text_size(px(11.))
                .text_color(if hl { hsla_u(0xffffff) } else { head_fg })
                .child(SharedString::from(col_name(c)))
                .child(handle),
        );
    }
    header.into_any_element()
}

/// Grid state that lives on `Docxy` rather than the sheet view, threaded into
/// the row renderer (which only sees the view).
#[derive(Clone, Default)]
struct GridOverlay {
    /// Box an in-progress fill drag would cover — outlined, not yet applied.
    fill_preview: Option<(u32, u32, u32, u32)>,
    /// Cells a focused range field points at, outlined with the dashed brand
    /// border (`border_range`). No wash of its own — that went with the border.
    /// An INPUT to `border_rg` below, which `sheet_el` resolves; the row
    /// renderer reads that rather than this.
    range_preview: Option<(u32, u32, u32, u32)>,
    /// A range field has the keyboard: the active cell drops its ring so it
    /// can't be mistaken for the range being picked, and wears a wash instead.
    picking: bool,
    /// The ranges the formula being typed mentions, in writing order — each
    /// outlined in its own colour so you can see what it reads.
    formula_refs: std::rc::Rc<Vec<(u32, u32, u32, u32)>>,
    /// The cells the SELECTED chart reads, one entry per slot with its role,
    /// each outlined in Excel's own colour for that role. Empty when no chart
    /// is selected — which is the whole of the "is this drawn?" question, so
    /// there is no separate flag.
    chart_refs: std::rc::Rc<Vec<ChartSourceArea>>,
    /// The selection's corner is under a chart card, which owns those pixels.
    handle_hidden: bool,
    /// What the dashed brand border outlines this frame: the pointed range,
    /// else a selection spanning more than one cell (`border_range`), with its
    /// rows snapped onto the ones the grid draws (`snap_range_rows`).
    ///
    /// Resolved in `sheet_el` and not by the row renderer, because it needs the
    /// visible-row list, and because the cap below has to be costed against the
    /// same range the rows will draw — one answer, asked once.
    border_rg: Option<(u32, u32, u32, u32)>,
    /// The pointed range's border dashes. False past `RANGE_BORDER_CELL_CAP`
    /// visible boundary cells, where the same edges are drawn solid instead.
    /// Only `sheet_el` knows the column window, so it fills this in.
    range_dashed: bool,
    /// A chart is selected, and one selection at a time means the grid's own is
    /// not shown: no ring, no range wash, no header highlight, no fill handle,
    /// no data-validation arrow, no comment note.
    /// The cells KEEP their selection -- it simply stops being drawn until the
    /// chart is dismissed (a click on the grid, Escape, or the panel's close
    /// button), at which point it reappears exactly where it was.
    ///
    /// A pointed range is deliberately still drawn: pointing at cells is what a
    /// selected chart's range fields do, and the border is what shows where.
    sel_hidden: bool,
}

/// One data row: the row-number gutter cell plus the visible cells (frozen
/// columns `0..fc` pinned, then the scrollable window `col0..=cend`).
fn sheet_row(
    view: &SheetView,
    ent: &Entity<Docxy>,
    r: u32,
    fc: u32,
    col0: u32,
    cend: u32,
    comment_cells: &std::collections::HashSet<(u32, u32)>,
    ov: GridOverlay,
) -> AnyElement {
    use gridcore::sheet::{Align, CellValue};
    let sh = view.sheet();
    let styles = &view.pkg.workbook.styles;
    let d1904 = view.pkg.workbook.date1904;
    // The active sheet index for conditional-formatting lookups.
    let sidx = view
        .active
        .min(view.pkg.workbook.sheets.len().saturating_sub(1));
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
    // The row-number gutter highlights the selection's rows — unless a chart
    // owns the selection, in which case the grid shows none of it.
    let hl_row = !ov.sel_hidden && r >= r0 && r <= r1;
    // What the dashed border outlines this frame — the cells a focused range
    // field points at, else a selection spanning more than one cell — resolved
    // once for the whole grid in `sheet_el`, where the visible rows are known.
    let border_rg = ov.border_rg;
    // Variable row height: an explicit <row ht> sets a floor (points → px at the
    // app's 15pt≈21px scale); wrapped cells grow the row past it via their
    // natural (min-content) height. items_stretch makes every cell fill it.
    let min_row_h = row_height_px(sh.row_height(r), SHEET_ROW_H);
    let mut row = h_flex().items_stretch().min_h(px(min_row_h)).child(
        div()
            .w(px(SHEET_GUT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(if hl_row { brand } else { head_bg })
            .border_r_1()
            .border_b_1()
            .border_color(gridline)
            .text_size(px(11.))
            .text_color(if hl_row { hsla_u(0xffffff) } else { head_fg })
            .child(SharedString::from((r + 1).to_string())),
    );
    // Merged regions: the top-left cell spans its columns' combined width; cells
    // it covers in the same row are skipped; cells under a vertical merge render
    // blank (content lives only in the top-left).
    // The row is drawn in two segments — the frozen columns, then the scrolled
    // window from `col0` — and `seg_start` marks the first column of each. A
    // merge whose origin sits LEFT of a segment's start would otherwise have
    // every one of its covered columns `continue`d away while the column header
    // still drew them, shifting the rest of the row left by the merge's width;
    // and `skip_to` set in the frozen band would eat the scrolled window's first
    // columns. The clipped remainder is drawn at the segment start instead.
    let mut skip_to: i64 = -1;
    let cols = (0..fc)
        .map(|c| (c, c == 0))
        .chain((col0..=cend).map(|c| (c, c == col0)));
    for (c, seg_start) in cols {
        if seg_start {
            skip_to = -1;
        } else if (c as i64) <= skip_to {
            continue;
        }
        let merge = sh
            .merges
            .iter()
            .find(|&&(mr1, mc1, mr2, mc2)| r >= mr1 && r <= mr2 && c >= mc1 && c <= mc2)
            .copied();
        let (cell_w, blank_covered) = match merge {
            Some((mr1, mc1, _mr2, mc2)) if r == mr1 && (c == mc1 || seg_start) => {
                skip_to = mc2 as i64; // widen; skip the rest of the span in this row
                (
                    (c.max(mc1)..=mc2)
                        .map(|cc| col_px(sh.col_width(cc)))
                        .sum::<f32>(),
                    false,
                )
            }
            Some((mr1, _, _, _)) if r == mr1 => continue, // covered in the top row
            Some(_) => (col_px(sh.col_width(c)), true),   // under a vertical merge → blank
            None => (col_px(sh.col_width(c)), false),
        };
        let selected = (r, c) == (sr, sc);
        // While a range is being picked the active cell keeps its place with a
        // wash instead of the ring, so only the picked range reads as an outline.
        let ring = selected && !ov.picking && !ov.sel_hidden;
        let in_range = !ov.sel_hidden && r >= r0 && r <= r1 && c >= c0 && c <= c1;
        // Cells the fill drag would reach: shaded while the button is down, so
        // the drag reads as a preview and nothing has actually moved yet.
        let in_preview = !in_range
            && ov
                .fill_preview
                .is_some_and(|(pr0, pc0, pr1, pc1)| r >= pr0 && r <= pr1 && c >= pc0 && c <= pc1);
        // Cells the formula being typed reads: the innermost reference covering
        // this cell gives it its colour, which is the one the TEXT draws too.
        let formula_ref = ref_index_at(&ov.formula_refs, r, c).map(|i| (i, ov.formula_refs[i]));
        // Cells the SELECTED chart reads: every area covering this one, largest
        // first, so each box keeps the sides that run through here and the
        // tightest slot still paints last — a series' name cell reads as a name
        // without opening up the values box it heads.
        let chart_areas: Vec<ChartSourceArea> = chart_areas_at(&ov.chart_refs, r, c)
            .into_iter()
            .map(|i| ov.chart_refs[i])
            .collect();
        let cell_editing = selected && editing.is_some();
        let on_freeze = fc > 0 && c + 1 == fc;
        let (text, xf, is_num) = match sh.cell(r, c) {
            _ if blank_covered => (String::new(), None, false),
            Some(cl) if !cl.is_blank() => {
                let xf = styles.xf(cl.style);
                (
                    gridcore::sheet::format_with(&xf, &cl.value, d1904),
                    Some(xf),
                    matches!(cl.value, CellValue::Number(_)),
                )
            }
            _ => (String::new(), None, false),
        };
        let halign = match xf.as_ref().map(|x| x.align).unwrap_or(Align::General) {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
            Align::General => {
                if is_num {
                    2
                } else {
                    0
                }
            }
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
        let color = color_rgb
            .map(|(r, g, b)| rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
            .unwrap_or(rgb(0x1a1a1a));
        let bg = if let Some((r, g, b)) = fill {
            rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into()
        } else {
            hsla_u(0xffffff)
        };
        let cell_border = xf.as_ref().is_some_and(|x| x.border);
        let wrap = xf.as_ref().is_some_and(|x| x.wrap);
        let mut cell = div()
            .id(ElementId::Name(format!("cell-{r}-{c}").into()))
            .w(px(cell_w))
            .flex_shrink_0()
            // The selected cell trades 1px of padding per side for its thicker
            // ring, so its box — and therefore the row's height and the text's
            // position — is byte-identical to an unselected cell's. Without
            // this, selecting a cell grows its row by 3px and shoves the grid.
            .map(|d| {
                if ring {
                    d.pt(px(0.)).pb(px(1.)).pl(px(2.)).pr(px(3.))
                } else {
                    d.px(px(4.)).py(px(2.))
                }
            })
            .flex()
            // Wrapped cells top-align and let text flow onto multiple lines
            // (growing the row); plain cells stay single-line and clip.
            .map(|d| {
                if wrap {
                    d.items_start()
                } else {
                    d.items_center().overflow_hidden()
                }
            })
            .bg(if cell_editing { hsla_u(0xffffff) } else { bg })
            .border_r_1()
            .border_b_1()
            .border_color(if on_freeze { freeze_line } else { gridline })
            // A thin box border (xf border) darkens all four sides.
            .when(cell_border, |d| d.border_1().border_color(hsla_u(0x7a7a7a)))
            .when(in_range && !selected, |d| d.bg(range_tint))
            .when(in_preview, |d| {
                d.bg(Hsla {
                    h: 0.,
                    s: 0.,
                    l: 0.45,
                    a: 0.16,
                })
            })
            .when_some(formula_ref, |d, (i, _)| {
                d.bg(Hsla {
                    a: 0.14,
                    ..hsla_u(ref_color(i))
                })
            })
            .when(selected && ov.picking && !ov.sel_hidden, |d| {
                d.bg(Hsla { a: 0.38, ..brand })
            })
            .when(ring, |d| d.border_2().border_color(brand));
        if cell_editing {
            cell = cell.justify_start().child(edit_caret_row(
                &editing.clone().unwrap_or_default(),
                view.edit_caret,
                hsla_u(0x1a1a1a),
                brand,
            ));
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
                        .map(|d| {
                            if wrap {
                                d.whitespace_normal().w_full()
                            } else {
                                d.whitespace_nowrap()
                            }
                        })
                        .child(SharedString::from(text)),
                );
            }
        }
        let has_link = sh.hyperlinks.contains_key(&(r, c));
        let ent2 = ent.clone();
        cell = cell.on_click(move |ev, window, cx| {
            let shift = ev.modifiers().shift;
            let dbl = ev.click_count() >= 2;
            ent2.update(cx, |this, cx| {
                if shift {
                    this.extend_to(r, c, cx)
                } else {
                    // While a formula is being typed — or a range field has the
                    // keyboard — a click POINTS at this cell: the selection
                    // never moves, so neither opening an editor on it (which
                    // would replace the half-typed formula with this cell's
                    // contents, or edit a cell nobody selected) nor following
                    // its link is what was asked for.
                    let pointing = this.formula_pick_active() || this.range_field_active();
                    this.select_cell(r, c, cx);
                    if pointing {
                        // the reference is written; nothing else to do
                    } else if dbl {
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
        // A formula's references, each in its own colour — drawn per edge cell
        // for the same reason as the range outline below.
        if let Some((i, rg)) = formula_ref {
            let e = range_edges_at(rg, r, c);
            if !e.is_empty() {
                cell = cell.relative().child(deferred(
                    div()
                        .absolute()
                        .left(px(-1.))
                        .top(px(-1.))
                        .right(px(-1.))
                        .bottom(px(-1.))
                        .border_color(hsla_u(ref_color(i)))
                        .when(e.top, |d| d.border_t(px(2.)))
                        .when(e.bottom, |d| d.border_b(px(2.)))
                        .when(e.left, |d| d.border_l(px(2.)))
                        .when(e.right, |d| d.border_r(px(2.))),
                ));
            }
        }
        // The selected chart's source areas, each outlined in the colour of the
        // slot it feeds — blue values, purple categories, green series names,
        // which is Excel's own mapping and deliberately not `ref_color`'s.
        // Drawn per edge cell, `deferred`, exactly like the references above.
        for a in &chart_areas {
            let e = range_edges_at(a.range, r, c);
            if !e.is_empty() {
                cell = cell.relative().child(deferred(
                    div()
                        .absolute()
                        .left(px(-1.))
                        .top(px(-1.))
                        .right(px(-1.))
                        .bottom(px(-1.))
                        .border_color(hsla_u(chart_slot_color(a.slot)))
                        .when(e.top, |d| d.border_t(px(2.)))
                        .when(e.bottom, |d| d.border_b(px(2.)))
                        .when(e.left, |d| d.border_l(px(2.)))
                        .when(e.right, |d| d.border_r(px(2.))),
                ));
            }
        }
        // The pointed range's border: dashed, in the brand teal, at Excel's
        // border width. Each edge cell draws only the sides it owns
        // (`range_edges_at`), positioned by the cell's own layout — the overlay
        // alternative reconstructs row positions from the scroll offset and
        // drifts on content-tall rows. `deferred` keeps the cell's overflow clip
        // from eating the line. The dashes themselves come from gpui's quad
        // shader via `border_dashed`, so they cost exactly what a solid border
        // costs; past `RANGE_BORDER_CELL_CAP` boundary cells `range_dashed` is
        // false and the same edges are drawn solid.
        if let Some(e) = border_rg.map(|rg| range_edges_at(rg, r, c)) {
            if !e.is_empty() {
                cell = cell.relative().child(deferred(
                    div()
                        .absolute()
                        .left(px(-1.))
                        .top(px(-1.))
                        .right(px(-1.))
                        .bottom(px(-1.))
                        .border_color(brand)
                        .when(ov.range_dashed, |d| d.border_dashed())
                        .when(e.top, |d| d.border_t(px(RANGE_BORDER_W)))
                        .when(e.bottom, |d| d.border_b(px(RANGE_BORDER_W)))
                        .when(e.left, |d| d.border_l(px(RANGE_BORDER_W)))
                        .when(e.right, |d| d.border_r(px(RANGE_BORDER_W))),
                ));
            }
        }
        // Auto-fill handle: a small square centred ON the selection's bottom-right
        // corner point. It lives inside that cell, so layout places it exactly (no
        // scroll/row-height math to drift); it is absolutely positioned, so it adds
        // nothing to the cell's size; and it is `deferred`, so it paints after the
        // neighbouring cells that would otherwise clip its outer half.
        if editing.is_none() && !ov.handle_hidden && !ov.sel_hidden && r == r1 && c == c1 {
            let ent_fill_dn = ent.clone();
            // Insets are measured from the PADDING box, so back out the cell's
            // border to reach the corner point, then half the box to centre on it.
            let edge = if ring { 2.0 } else { 1.0 } + 6.0;
            cell = cell.relative().child(deferred(
                div()
                    .id("fill-handle")
                    .absolute()
                    .right(px(-edge))
                    .bottom(px(-edge))
                    .w(px(12.))
                    .h(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor(CursorStyle::Crosshair)
                    // ONLY the press arms the fill. The deferred hitbox sits above
                    // the list, so unlike a cell it does see mouse-down, and that
                    // is the whole signal needed.
                    //
                    // There used to be an `on_mouse_move` fallback here for "a drag
                    // that leaves the grip before the press fires", and it turned
                    // every drag-to-select into a fill. The handle is pinned to the
                    // selection's bottom-right corner, and while you sweep a range
                    // that corner IS your pointer — so the handle was re-rendered
                    // under the moving cursor, its move handler fired with the
                    // button down, and selecting cells wrote to them instead.
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx2| {
                        cx2.stop_propagation();
                        ent_fill_dn.update(cx2, |this, cx2| this.sheet_fill_start(cx2));
                    })
                    .child(
                        // A 6px square in a 1px white surround, so it reads against
                        // the selection ring and the cell behind it alike.
                        div()
                            .w(px(8.))
                            .h(px(8.))
                            .bg(hsla_u(0xffffff))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .w(px(6.))
                                    .h(px(6.))
                                    .bg(hsla_u(0x147A6F))
                                    .hover(|d| d.bg(hsla_u(BRAND))),
                            ),
                    ),
            ));
        }
        // Red corner marker for a commented cell (Excel's note indicator).
        if comment_cells.contains(&(r, c)) {
            cell = cell.relative().child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .w(px(0.))
                    .h(px(0.))
                    .border_t(px(5.))
                    .border_r(px(5.))
                    .border_color(hsla_u(0xd0322b)),
            );
        }
        row = row.child(cell);
    }
    row.into_any_element()
}

/// A floating chart card: a clustered column chart drawn with div bars, plus a
/// title and legend. Handles bar/column data (the common case) for any kind.
/// The eight resize grips of a selected chart: one per corner and edge, each
/// centred on the selection frame and carrying the sides it drags.
fn chart_grips(idx: usize, w: f32, h: f32, ent: &Entity<Docxy>) -> Vec<AnyElement> {
    const EDGES: [(i8, i8); 8] = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    EDGES
        .iter()
        .map(|&(ex, ey)| {
            let along = |e: i8, extent: f32| match e {
                -1 => -3.0,
                1 => extent + 3.0,
                _ => extent / 2.0,
            } - 4.0;
            let cursor = match (ex, ey) {
                (0, _) => CursorStyle::ResizeUpDown,
                (_, 0) => CursorStyle::ResizeLeftRight,
                (a, b) if a == b => CursorStyle::ResizeUpLeftDownRight,
                _ => CursorStyle::ResizeUpRightDownLeft,
            };
            let ent_dn = ent.clone();
            let ent_mv = ent.clone();
            div()
                .id(ElementId::Name(format!("grip-{idx}-{ex}-{ey}").into()))
                .absolute()
                .left(px(along(ex, w)))
                .top(px(along(ey, h)))
                .w(px(8.))
                .h(px(8.))
                .bg(hsla_u(0xffffff))
                .border_1()
                .border_color(hsla_u(BRAND))
                .rounded(px(1.))
                .cursor(cursor)
                .on_mouse_down(MouseButton::Left, move |ev, _w, cx2| {
                    // Beat the card's own press, which would start a move.
                    cx2.stop_propagation();
                    let at = (f32::from(ev.position.x), f32::from(ev.position.y));
                    ent_dn.update(cx2, |this, cx2| this.chart_press(idx, (ex, ey), at, cx2));
                })
                .on_mouse_move(move |ev, _w, cx2| {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        let at = (f32::from(ev.position.x), f32::from(ev.position.y));
                        ent_mv.update(cx2, |this, cx2| this.chart_drag_move(at, cx2));
                    }
                })
                .into_any_element()
        })
        .collect()
}

/// A floating chart card drawn at `w` × `h` (its anchor's cell extent).
fn chart_card(data: &gridcore::sheet::ChartData, w: f32, h: f32) -> AnyElement {
    const PALETTE: [u32; 6] = [0x2AA79B, 0x2F6FDB, 0xC0705A, 0xD8A44A, 0x7A5EA8, 0x5A9E5A];
    // A series' own colour wins over its slot in the palette.
    let ser_color = |si: usize| {
        data.series
            .get(si)
            .and_then(|s| s.color)
            .unwrap_or(PALETTE[si % PALETTE.len()])
    };
    // One element per point per series, every frame. A chart the UI authored is
    // capped at MAX_CHART_CELLS when it is pointed, but one read from a file can
    // cache as many points as Excel cared to write, and a card a few hundred
    // pixels wide can't show them anyway. BOTH axes need the cap: `parse_chart`
    // pushes one `ChartSeries` per `<c:ser>` with no bound of its own, so
    // capping only the points still leaves points × series elements per frame.
    const MAX_CARD_POINTS: usize = 512;
    const MAX_CARD_SERIES: usize = 32;
    // The plot area is drawn differently per chart kind. Column/Bar/Line share a
    // per-series legend; Pie's slices are per-category, so it builds its own.
    let kind = data.kind.as_str();
    // How many series are DRAWN — all of them, except on a pie, where it is the
    // first (`chart_plotted_series`). Asked here rather than assumed from what
    // the file will hold: the writer keeps every series a pie carries, so
    // "whatever is in `data.series`" is no longer the same question. Everything
    // measured off the series is measured off the drawn ones only — an axis
    // stretched by a series that isn't plotted, or a slice per category of a
    // longer one that isn't either, would both be scaled to invisible data.
    let nser = chart_plotted_series(kind, data.series.len()).min(MAX_CARD_SERIES);
    // `nser` is bounded by `data.series.len()` on both terms, so this can't
    // slice past the end.
    let plotted = &data.series[..nser];
    let maxv = plotted
        .iter()
        .flat_map(|s| s.values.iter().copied())
        .fold(0.0f64, f64::max)
        .max(1.0);
    let ncat = data
        .categories
        .len()
        .max(plotted.iter().map(|s| s.values.len()).max().unwrap_or(0))
        .min(MAX_CARD_POINTS);
    // The title strip and the legend take fixed bites out of the card; the plot
    // area gets the rest, and the bars scale to it.
    let area_h = (h - 46.0).max(40.0);
    let plot_h = (area_h - 20.0).max(20.0);
    let cat_label = |ci: usize| {
        let label = data.categories.get(ci).cloned().unwrap_or_default();
        div()
            .text_size(px(8.))
            .text_color(hsla_u(0x666666))
            .max_w(px(52.))
            .overflow_hidden()
            .child(SharedString::from(label))
    };

    let mut pie_legend: Option<AnyElement> = None;
    let plot: AnyElement = match kind {
        "bar" => {
            // Horizontal bars: one row per category, width proportional to value.
            let mut col = v_flex().flex_1().gap(px(3.)).px_2().py_2().justify_center();
            for ci in 0..ncat {
                let mut row = h_flex().items_center().gap(px(4.)).h(px(16.));
                row = row.child(
                    div()
                        .w(px(46.))
                        .text_size(px(8.))
                        .text_color(hsla_u(0x666666))
                        .overflow_hidden()
                        .child(SharedString::from(
                            data.categories.get(ci).cloned().unwrap_or_default(),
                        )),
                );
                let mut bars = v_flex().flex_1().gap(px(1.));
                for (si, s) in plotted.iter().enumerate() {
                    let val = s.values.get(ci).copied().unwrap_or(0.0);
                    let frac = (val.max(0.0) / maxv) as f32;
                    bars = bars.child(
                        div()
                            .h(px(6.))
                            .w(relative(frac.clamp(0.02, 1.0)))
                            .rounded_r(px(1.))
                            .bg(rgb(ser_color(si))),
                    );
                }
                row = row.child(bars);
                col = col.child(row);
            }
            col.h(px(area_h)).into_any_element()
        }
        "line" => {
            // Point/line preview: each series' value plotted as a dot at its height.
            let mut plot = h_flex().h(px(area_h)).items_end().gap(px(6.)).px_2().pt_2();
            for ci in 0..ncat {
                let mut stack = div().relative().w(px(14.)).h(px(plot_h));
                for (si, s) in plotted.iter().enumerate() {
                    let val = s.values.get(ci).copied().unwrap_or(0.0);
                    let h = ((val.max(0.0) / maxv) as f32 * plot_h).clamp(1.0, plot_h);
                    stack = stack.child(
                        div()
                            .absolute()
                            .bottom(px(h - 3.5))
                            .left(px(3.5))
                            .size(px(7.))
                            .rounded(px(4.))
                            .bg(rgb(ser_color(si))),
                    );
                }
                plot = plot.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_end()
                        .gap(px(2.))
                        .h(px(area_h - 2.))
                        .child(stack)
                        .child(cat_label(ci)),
                );
            }
            plot.into_any_element()
        }
        "pie" => {
            // Pie preview as a 100%-stacked proportion bar; slices = categories,
            // proportions from the one series a pie plots — `nser` is that one,
            // and any others the chart holds are kept in the file undrawn.
            let vals: Vec<f64> = (0..ncat)
                .map(|ci| {
                    plotted
                        .first()
                        .and_then(|s| s.values.get(ci))
                        .copied()
                        .unwrap_or(0.0)
                        .max(0.0)
                })
                .collect();
            let total = vals.iter().sum::<f64>().max(1.0);
            let mut bar = h_flex()
                .w_full()
                .h(px(30.))
                .rounded(px(4.))
                .overflow_hidden();
            let mut leg = h_flex().gap_3().px_2().pb_1().flex_wrap();
            for ci in 0..ncat {
                let frac = (vals[ci] / total) as f32;
                bar = bar.child(
                    div()
                        .h_full()
                        .w(relative(frac.max(0.0)))
                        .bg(rgb(PALETTE[ci % PALETTE.len()])),
                );
                leg = leg.child(
                    h_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size(px(9.))
                                .rounded(px(2.))
                                .bg(rgb(PALETTE[ci % PALETTE.len()])),
                        )
                        .child(div().text_size(px(9.)).text_color(hsla_u(0x333333)).child(
                            SharedString::from(
                                data.categories.get(ci).cloned().unwrap_or_default(),
                            ),
                        )),
                );
            }
            pie_legend = Some(leg.into_any_element());
            v_flex()
                .flex_1()
                .justify_center()
                .gap(px(6.))
                .px_3()
                .py_2()
                .h(px(area_h))
                .child(bar)
                .into_any_element()
        }
        _ => {
            // Column (default): vertical clustered bars.
            let mut plot = h_flex().h(px(area_h)).items_end().gap(px(6.)).px_2().pt_2();
            for ci in 0..ncat {
                let mut cluster = h_flex().items_end().gap(px(1.));
                for (si, s) in plotted.iter().enumerate() {
                    let val = s.values.get(ci).copied().unwrap_or(0.0);
                    let h = ((val.max(0.0) / maxv) as f32 * plot_h).clamp(1.0, plot_h);
                    cluster = cluster.child(
                        div()
                            .w(px(11.))
                            .h(px(h))
                            .rounded_t(px(1.))
                            .bg(rgb(ser_color(si))),
                    );
                }
                plot = plot.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_end()
                        .gap(px(2.))
                        .h(px(area_h - 2.))
                        .child(cluster)
                        .child(cat_label(ci)),
                );
            }
            plot.into_any_element()
        }
    };

    let legend: AnyElement = pie_legend.unwrap_or_else(|| {
        let mut legend = h_flex().gap_3().px_2().pb_1().flex_wrap();
        for (si, s) in plotted.iter().enumerate() {
            legend = legend.child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(9.)).rounded(px(2.)).bg(rgb(ser_color(si))))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hsla_u(0x333333))
                            .child(SharedString::from(s.name.clone())),
                    ),
            );
        }
        legend.into_any_element()
    });
    v_flex()
        .w(px(w))
        .h(px(h))
        .overflow_hidden()
        .bg(hsla_u(0xffffff))
        .border_1()
        .border_color(hsla_u(0xcccccc))
        .rounded(px(4.))
        .child(
            div()
                .w_full()
                .text_center()
                .py_1()
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(hsla_u(0x222222))
                .child(SharedString::from(data.title.clone())),
        )
        .child(plot)
        .child(legend)
        .into_any_element()
}

/// Render a spreadsheet tab: a formula/reference bar; a horizontally-scrolling
/// grid whose column header (and any frozen rows) stay pinned while the rows
/// virtualize vertically via `uniform_list`; and the sheet tabs.
fn sheet_el(
    view: &SheetView,
    ent: &Entity<Docxy>,
    rename: Option<(usize, String)>,
    comment_editing: bool,
    dv_values: Option<Vec<String>>,
    dv_open: bool,
    grid_w: f32,
    ov: GridOverlay,
    chart_ui: ChartUi,
    cx: &mut Context<Docxy>,
) -> AnyElement {
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
        view.pkg
            .comments()
            .into_iter()
            .filter(|c| c.sheet == view.active)
            .map(|c| (c.row, c.col))
            .collect(),
    );
    // Horizontal column window: frozen cols 0..fc are always drawn; the scrollable
    // window fills the REMAINING width from the scroll offset col0 (kept >= fc).
    // Column virtualization by offset — the counterpart to the row uniform_list.
    let col0 = view.col0.max(fc).min(MAX_VISIBLE_COL);
    let avail = (grid_w - SHEET_GUT - frozen_w).max(80.0);
    let cend = last_visible_col(|c| col_px(sh.col_width(c)), col0, avail, MAX_VISIBLE_COL);
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
            Some(c) if c.formula.is_some() => {
                format!("={}", c.formula.as_deref().unwrap_or_default())
            }
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
        .child(
            div()
                .min_w(px(64.))
                .px_2()
                .py(px(2.))
                .rounded_sm()
                .bg(hsla_u(0xffffff))
                .border_1()
                .border_color(gridline)
                .text_size(px(12.))
                .text_color(hsla_u(0x333333))
                .child(SharedString::from(sel_ref)),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(hsla_u(0x888888))
                .child("fx"),
        )
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
                        // Clicking the bar opens an editor on the selected
                        // cell, so it is a press on the cells and takes the
                        // selection like one. Without this the caret and the
                        // white edit box would be drawn while the chart still
                        // drew its frame and grips — two things selected — and
                        // over a cell `sel_hidden` was leaving unmarked.
                        this.chart_hand_back(cx);
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
    let header = sheet_col_header(view, ent, fc, col0, cend, ov.sel_hidden);
    let cc_frozen = comment_cells.clone();
    let cc_list = comment_cells.clone();
    // A chart card floating over the selection's corner owns those pixels, so
    // the fill handle (which paints above every cell) stands down there. A card
    // is drawn at its anchor, `chart_span_px` wide and tall — the same size the
    // overlay uses, so a resized card hides the handle over exactly the cells it
    // actually covers.
    let corner_under_chart = {
        let (br, bc) = (r1, c1);
        let boxes = view
            .charts
            .iter()
            .filter(|c| c.sheet == view.active)
            .map(|cv| (cv.from, cv.to))
            .chain(
                sh.drawings
                    .iter()
                    .filter(|d| matches!(d.kind, gridcore::sheet::DrawingKind::Chart(_)))
                    .map(|d| (d.from, d.to)),
            );
        boxes.into_iter().any(|(from, to)| {
            let (ar, ac) = from;
            if br < ar || bc < ac {
                return false;
            }
            let (cw, ch) = chart_span_px(sh, from, to);
            // Hidden rows/columns measure zero, so both walks are bounded by a
            // cell count as well as by the card's extent.
            let mut w = 0.0f32;
            let mut cc = ac;
            while w < cw && cc < ac + 256 {
                w += col_px(sh.col_width(cc));
                cc += 1;
            }
            let mut h = 0.0f32;
            let mut rr = ar;
            while h < ch && rr < ar + 1024 {
                h += row_height_px(sh.row_height(rr), SHEET_ROW_H) + 1.0;
                rr += 1;
            }
            br < rr && bc < cc
        })
    };
    // Visible rows (filter/hide skips `hidden="1"` rows) — the list virtualizes
    // over these, so a filtered-out row collapses instead of showing blank.
    // Built before the overlay is finished because every outline is snapped
    // onto it below.
    let visible: std::rc::Rc<Vec<u32>> = std::rc::Rc::new(
        (0..total_rows as u32)
            .filter(|r| !sh.row_hidden(*r))
            .collect(),
    );
    // The cards' boxes are only known here, so the fill handle's visibility is
    // the one overlay field the render pass can't fill in.
    //
    // Neither is the dash cap: the border is drawn cell by cell, so its cost is
    // the visible boundary cells, and the column window (`fc` frozen columns
    // then `col0..=cend`) is only settled here. The frozen band and the scrolled
    // window are counted as one span `0..=cend`, which over-counts the columns
    // scrolled between them — the same safe direction as everything else here.
    //
    // And every outlined range is snapped onto the rows above first: an edge
    // belongs to a row that is drawn, or it is not drawn at all and the
    // rectangle opens up (`snap_range_rows`). That is the border, the formula's
    // references and the chart's source areas alike — the three lists
    // `range_edges_at` answers for.
    //
    // The border snaps BEFORE `border_range` decides, not after: the
    // single-cell guard there ("a lone cell already wears the ring") has to see
    // the range that will actually be DRAWN, or a two-row selection with one of
    // its rows hidden collapses to one cell and wears both the ring and a full
    // dashed box — the doubled indicator the guard exists to remove.
    let preview_rg = ov
        .range_preview
        .map(|p| snap_range_rows(p, &visible, total_rows as u32));
    let border_rg = border_range(
        preview_rg,
        shown_sel(&ov, (r0, c0, r1, c1)).map(|s| snap_range_rows(s, &visible, total_rows as u32)),
    );
    // Dashes mean "a field is POINTING at these cells", never merely "these
    // cells are selected". Excel reserves its marching ants the same way — for
    // a copy, or a dialog's range picker — and a range you swept with the mouse
    // wears a solid border there. Dashing an ordinary selection made the two
    // indistinguishable, so a plain drag looked like a formula was reading it.
    let range_dashed = border_is_dashed(
        preview_rg.is_some(),
        border_rg.is_none_or(|rg| {
            let cols = (if fc > 0 { 0 } else { col0 }, cend);
            range_border_dashed(rg, cols, GRID_MAX_VISIBLE_ROWS)
        }),
    );
    // Re-wrapped only when there is something to snap: the common frame has no
    // formula open and no chart selected, and pays nothing.
    let formula_refs = if ov.formula_refs.is_empty() {
        ov.formula_refs.clone()
    } else {
        std::rc::Rc::new(
            ov.formula_refs
                .iter()
                .map(|&rg| snap_range_rows(rg, &visible, total_rows as u32))
                .collect(),
        )
    };
    let chart_refs = if ov.chart_refs.is_empty() {
        ov.chart_refs.clone()
    } else {
        std::rc::Rc::new(
            ov.chart_refs
                .iter()
                .map(|a| ChartSourceArea {
                    range: snap_range_rows(a.range, &visible, total_rows as u32),
                    ..*a
                })
                .collect(),
        )
    };
    let ov = GridOverlay {
        handle_hidden: ov.handle_hidden || corner_under_chart,
        border_rg,
        range_dashed,
        formula_refs,
        chart_refs,
        ..ov
    };
    // Read off before the row-list closure moves `ov`; the DV overlay below is
    // built after that move but is gated on the same flag.
    let sel_hidden = ov.sel_hidden;
    // Frozen top rows (Excel freeze panes, rows axis): pinned below the header,
    // outside the virtualized list, so they stay put while the rest scrolls.
    let fr = (frz_r as usize).min(visible.len()).min(30);
    let mut frozen = v_flex().flex_none();
    for i in 0..fr {
        frozen = frozen.child(sheet_row(
            view,
            ent,
            visible[i],
            fc,
            col0,
            cend,
            &cc_frozen,
            ov.clone(),
        ));
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
        let Some(v) = this.active_sheet() else {
            return div().into_any_element();
        };
        let row = vis_list.get(fr + ix).copied().unwrap_or(0);
        sheet_row(v, &ent_list, row, fc, col0, cend, &cc_list, ov.clone())
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
            let step = if dy < 0.0 {
                1
            } else if dy > 0.0 {
                -1
            } else {
                0
            };
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
    let mut tabs = h_flex()
        .w_full()
        .h(px(26.))
        .items_center()
        .gap(px(1.))
        .px_2()
        .bg(hsla_u(0xf1f1f1))
        .border_t_1()
        .border_color(gridline);
    for (i, s) in view.pkg.workbook.sheets.iter().enumerate() {
        let active = i == view.active;
        let renaming = rename.as_ref().is_some_and(|(ri, _)| *ri == i);
        let mut tab = div()
            .id(ElementId::Name(format!("sheet-tab-{i}").into()))
            .px_3()
            .h(px(20.))
            .flex()
            .items_center()
            .gap_1()
            .rounded_t(px(4.))
            .cursor_pointer()
            .text_size(px(12.))
            .bg(if active {
                hsla_u(0xffffff)
            } else {
                hsla_u(0xe4e4e4)
            })
            .text_color(if active {
                hsla_u(0x1a1a1a)
            } else {
                hsla_u(0x666666)
            });
        if renaming {
            // Inline editor: show the live buffer + a caret bar; typing is routed
            // through sheet_rename_key (keyboard focus stays on the grid root).
            let buf = rename.as_ref().map(|(_, b)| b.clone()).unwrap_or_default();
            tab = tab
                .bg(hsla_u(0xffffff))
                .border_1()
                .border_color(hsla_u(BRAND))
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
                        .px(px(2.))
                        .rounded(px(2.))
                        .text_size(px(11.))
                        .text_color(hsla_u(0x999999))
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
            .px_2()
            .h(px(20.))
            .flex()
            .items_center()
            .rounded_t(px(4.))
            .cursor_pointer()
            .text_size(px(15.))
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
    // The RENDERED row height: SHEET_ROW_H is the cell's min-height, but each row
    // also carries a 1px bottom gridline, so a row occupies SHEET_ROW_H + 1 px.
    // The overlay's y math must use this or the anchor drifts ~1px per row.
    let row_h = SHEET_ROW_H + 1.0;
    let top = view.vlist.logical_scroll_top();
    let scrolled_px = -(top.item_ix as f32 * row_h + f32::from(top.offset_in_item));
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
        // `scrolled_px` counts LIST items, and the list virtualizes over
        // `visible` — hidden rows collapse out of it. Treating the raw sheet row
        // as that index drifts one row height per hidden row above the anchor,
        // so after an AutoFilter every card, note and dropdown slides down the
        // sheet (far enough to leave the clipped chart layer entirely).
        // A hidden anchor row lands on its insertion point, which is where the
        // row it names would sit.
        let vi = match visible.binary_search(&ar) {
            Ok(i) | Err(i) => i,
        };
        if vi < fr {
            return vi as f32 * row_h; // pinned frozen row
        }
        vi as f32 * row_h + scrolled_px
    };
    let loaded = sh.drawings.iter().filter_map(|d| match &d.kind {
        gridcore::sheet::DrawingKind::Chart(cd) => Some((d.from, d.to, cd)),
        _ => None,
    });
    let cards: Vec<AnyElement> = view
        .charts
        .iter()
        .filter(|c| c.sheet == view.active)
        .map(|cv| (cv.from, cv.to, &cv.data))
        .chain(loaded)
        .enumerate()
        .filter_map(|(i, (from, to, data))| {
            // `from` is (row, col) for UI charts and gridcore drawings alike.
            let (ar, ac) = from;
            let x = col_x(ac)?;
            let y = row_y(ar);
            // A card being dragged follows the pointer; its anchor only moves
            // when the button comes up.
            // The card's size is the extent of the cells it spans; a resize drag
            // previews by moving the dragged edges only.
            let (mut cw, mut ch) = chart_span_px(sh, from, to);
            let (mut cx0, mut cy0) = (x, y);
            if let Some((di, dx, dy, edge)) = chart_ui.drag {
                if di == i {
                    if edge == (0, 0) {
                        cx0 += dx;
                        cy0 += dy;
                    } else {
                        let (x_off, w_delta) = resize_axis(edge.0, dx, cw, MIN_CHART_W);
                        let (y_off, h_delta) = resize_axis(edge.1, dy, ch, MIN_CHART_H);
                        cx0 += x_off;
                        cy0 += y_off;
                        cw += w_delta;
                        ch += h_delta;
                    }
                }
            }
            let selected = chart_ui.sel == Some(i);
            let ent_c = ent.clone();
            let ent_m = ent.clone();
            Some(
                div()
                    .id(ElementId::Name(format!("chart-{i}").into()))
                    .absolute()
                    .left(px(cx0))
                    .top(px(cy0))
                    .cursor(if selected {
                        CursorStyle::OpenHand
                    } else {
                        CursorStyle::Arrow
                    })
                    // Excel selects an object on press, and the same press begins
                    // the move; the cell underneath must not also react.
                    .on_mouse_down(MouseButton::Left, move |ev, _w, cx2| {
                        cx2.stop_propagation();
                        let at = (f32::from(ev.position.x), f32::from(ev.position.y));
                        ent_c.update(cx2, |this, cx2| this.chart_press(i, (0, 0), at, cx2));
                    })
                    // The card follows the pointer, so it sits under it for most
                    // of the drag — and a hovered hitbox is the only one that gets
                    // move events. Without this the drag stalls the moment the
                    // card catches up; the grid's own handler covers the rest.
                    .on_mouse_move(move |ev, _w, cx2| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            let at = (f32::from(ev.position.x), f32::from(ev.position.y));
                            ent_m.update(cx2, |this, cx2| this.chart_drag_move(at, cx2));
                        }
                    })
                    .child(chart_card(data, cw, ch))
                    // The selection frame sits just outside the card, like Excel's,
                    // with a grip on each corner and edge.
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(-3.))
                                .top(px(-3.))
                                .right(px(-3.))
                                .bottom(px(-3.))
                                .border_1()
                                .border_color(hsla_u(BRAND))
                                .rounded(px(4.)),
                        )
                        .children(chart_grips(i, cw, ch, ent))
                    })
                    .into_any_element(),
            )
        })
        .collect();
    // A yellow note box for the selected commented cell (hidden while its entry
    // bar is open), anchored just off the cell's top-right like Excel.
    //
    // It goes with the selection for the same reason the DV arrow below does:
    // it is anchored on the selected cell and names it, so while a chart owns
    // the selection (`sel_hidden`) it would hang over a cell the grid is
    // drawing no ring, no wash and no header mark for — and paint over the
    // chart layer while it did.
    let note: Option<AnyElement> = if comment_editing || sel_hidden {
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
                            .absolute()
                            .left(px(x))
                            .top(px(y))
                            .w(px(200.))
                            .px_2()
                            .py_1p5()
                            .gap_1()
                            .bg(hsla_u(0xffffe1))
                            .border_1()
                            .border_color(hsla_u(0xc9b458))
                            .rounded_sm()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(hsla_u(0x333333))
                                    .child(SharedString::from(c.author.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(hsla_u(0x1a1a1a))
                                    .child(SharedString::from(c.text.clone())),
                            )
                            .into_any_element(),
                    )
                })
        }
    };
    // Data-validation list dropdown: an arrow on the selected cell + (when open)
    // a value popup, both cell-anchored in the same layer.
    // Both go with the selection: while a chart owns it (`sel_hidden`) the
    // arrow would float over a cell nothing on screen names, and picking a
    // value from it would write that unmarked cell — the same invisible write
    // the fill handle drops its own affordance to avoid.
    let mut dv_overlay: Vec<AnyElement> = Vec::new();
    if let Some(vals) = dv_values.as_ref().filter(|_| !sel_hidden) {
        let (sr, sc) = view.sel;
        if let Some(cx0) = col_x(sc) {
            let cw = col_px(sh.col_width(sc));
            let y = row_y(sr);
            let ent_arrow = ent.clone();
            dv_overlay.push(
                div()
                    .id("dv-arrow")
                    .absolute()
                    .left(px(cx0 + cw - 17.0))
                    .top(px(y + 1.0))
                    .w(px(16.))
                    .h(px(SHEET_ROW_H - 2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(hsla_u(0xf1f1f1))
                    .border_1()
                    .border_color(hsla_u(0x9a9a9a))
                    .rounded_sm()
                    .text_size(px(8.))
                    .text_color(hsla_u(0x333333))
                    .child("\u{25bc}")
                    .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                        ent_arrow.update(cx, |this, cx| this.sheet_dv_toggle(cx));
                    })
                    .into_any_element(),
            );
            if dv_open {
                let mut list = v_flex()
                    .id("dv-list")
                    .absolute()
                    .left(px(cx0))
                    .top(px(y + SHEET_ROW_H))
                    .min_w(px(cw.max(90.0)))
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .bg(hsla_u(0xffffff))
                    .border_1()
                    .border_color(hsla_u(0x9a9a9a))
                    .rounded_sm();
                for val in vals {
                    let ent_pick = ent.clone();
                    let v2 = val.clone();
                    list = list.child(
                        div()
                            .id(ElementId::Name(format!("dv-{val}").into()))
                            .px_2()
                            .py(px(2.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(hsla_u(0x1a1a1a))
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
    // (The auto-fill handle is drawn inside its own cell — see sheet_row — where
    // its position is exact; it uses `deferred` to escape occlusion.)

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
        // A press anywhere in the grid plants the drag anchor. The virtualized
        // list never gives a cell its own mouse-down, so without this the anchor
        // could only be the first cell the pointer MOVED into — which is a cell
        // late whenever the press is near an edge.
        .on_mouse_down(MouseButton::Left, {
            let ent_dn = ent.clone();
            move |ev, _w, cx| {
                ent_dn.update(cx, |this, cx| this.grid_press(ev.position, cx));
            }
        })
        // Column-resize drag: track the pointer and release anywhere in the grid.
        .on_mouse_move(move |ev, _w, cx| {
            let at = (f32::from(ev.position.x), f32::from(ev.position.y));
            ent_move.update(cx, |this, cx| {
                this.col_resize_move(at.0, cx);
                // A chart move is tracked here rather than on the card, so the
                // pointer can outrun it without dropping the drag.
                if ev.pressed_button == Some(MouseButton::Left) {
                    this.chart_drag_move(at, cx);
                }
            });
        })
        .on_mouse_up(MouseButton::Left, move |_ev, _w, cx| {
            ent_up.update(cx, |this, cx| this.grid_release(cx));
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
fn sheet_hbar(
    _view: &SheetView,
    ent: &Entity<Docxy>,
    max_c: u32,
    col0: u32,
    cend: u32,
) -> AnyElement {
    let total = (max_c + 1).max(cend + 1).max(1);
    let shown = (cend + 1).saturating_sub(col0).max(1);
    let frac = (shown as f32 / total as f32).clamp(0.08, 1.0);
    let pos = if total > shown {
        col0 as f32 / (total - shown) as f32
    } else {
        0.0
    };
    let arrow = |glyph: &'static str, id: &'static str, ent: Entity<Docxy>, delta: i32| {
        div()
            .id(id)
            .w(px(15.))
            .h(px(15.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .rounded_sm()
            .bg(hsla_u(0xe4e4e4))
            .text_size(px(8.))
            .text_color(hsla_u(0x444444))
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
                .w(px(160.))
                .h(px(9.))
                .rounded(px(3.))
                .bg(hsla_u(0xe0e0e0))
                .border_1()
                .border_color(hsla_u(0xcfcfcf))
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
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
        Kind::Look => (
            "lookxy",
            "mail list + reading pane (mailcore) lands here next",
        ),
        Kind::Docx => ("docxy", ""),
    };
    v_flex()
        .flex_1()
        .bg(bg)
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_color(rgb(BRAND))
                .font_weight(FontWeight::BOLD)
                .text_size(px(20.))
                .child(name),
        )
        .child(div().text_color(dim).child(blurb))
}

fn main() {
    // The command line: files to open (e.g. double-clicking a document in
    // Explorer, opened on top of the restored hot-exit session), plus the
    // opt-in `--harness` flag.
    let cli = harness::parse_args(std::env::args_os().skip(1));
    for flag in &cli.unknown_flags {
        eprintln!("docxy: ignoring unknown option {flag}");
    }
    let cli_files: Vec<PathBuf> = cli.files.into_iter().filter(|p| p.is_file()).collect();

    // The harness control surface: started only when asked for, and only into
    // an isolated config root. `gate` refuses to run against the installed
    // app's own config, so a mistyped invocation cannot drive — and overwrite —
    // the user's live instance. Both refusals are fatal rather than a silent
    // downgrade: a harness that came up without its socket would leave its
    // driver waiting on a discovery file that is never written.
    let want_harness = cli.harness || harness::env_flag(std::env::var_os(harness::HARNESS_ENV));
    let ctl = if want_harness {
        let root = match harness::gate(
            std::env::var_os(CONFIG_DIR_ENV).as_deref(),
            dirs::config_dir().as_deref(),
        ) {
            Ok(root) => root,
            Err(e) => {
                eprintln!("docxy: {e}");
                std::process::exit(2);
            }
        };
        match harness::start(&root) {
            Ok(pair) => Some(pair),
            Err(e) => {
                eprintln!("docxy: the harness could not start its control server: {e}");
                std::process::exit(2);
            }
        }
    } else {
        None
    };
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
            let view = cx.new(Docxy::new);
            // In harness mode, start draining control requests as soon as the
            // view exists, so a driver can connect the moment the window is up.
            if let Some((server, rx)) = ctl {
                harness::attach(&view, server, rx, window, cx);
            }
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
    use super::{
        CHART_CATEGORIES_COLOR, CHART_NAME_COLOR, CHART_VALUES_COLOR, ChartSlot, ChartSourceArea,
        EdgeMask, GRID_MAX_VISIBLE_ROWS, GridOverlay, PanelEvent, RANGE_BORDER_CELL_CAP,
        RANGE_BORDER_W, RangeBorderPlan, RefText, SHEET_ROW_H, SelectTarget, SelectionAfter,
        border_range, cell_selection_shown, char_to_byte, chart_areas_at, chart_panel_after,
        chart_panel_shown, chart_ref_of, chart_slot_color, chart_source_areas, col_at_x, col_px,
        dash_fit, edit_runs, fill_box, formula_ref_tokens, last_visible_col, parse_ref_text,
        press_selection, preview_range, range_a1, range_border_cell_count, range_border_dashed,
        range_border_plan, range_edges_at, range_text, ref_a1, ref_color, ref_index_at,
        ref_pick_text, ref_token_at, replace_ref, resize_axis, row_height_px, scroll_col0_for_sel,
        series_move, series_name_shown, series_remove, sheet_index_of, shift_col, shift_row,
        shown_sel, snap_range_rows, source_ref_text,
    };

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    /// A qualifier names a sheet by name, whatever case it was typed in; no
    /// qualifier means the sheet in front of you, whichever that is.
    #[test]
    fn sheet_index_of_finds_the_sheet_a_ref_names() {
        let wb = names(&["Sheet1", "Budget", "My Sheet"]);

        // No qualifier: the active sheet, not sheet 0.
        assert_eq!(sheet_index_of(&wb, None, 2), Ok(2));
        assert_eq!(sheet_index_of(&wb, None, 0), Ok(0));

        // Named, from a different active sheet each time.
        assert_eq!(sheet_index_of(&wb, Some("Sheet1"), 1), Ok(0));
        assert_eq!(sheet_index_of(&wb, Some("Budget"), 0), Ok(1));

        // Excel matches sheet names case-insensitively.
        assert_eq!(sheet_index_of(&wb, Some("budget"), 0), Ok(1));
        assert_eq!(sheet_index_of(&wb, Some("BUDGET"), 0), Ok(1));

        // A name with spaces is a name like any other — the quotes came off in
        // `parse_ref_text`, so what arrives here is the bare name.
        assert_eq!(sheet_index_of(&wb, Some("My Sheet"), 0), Ok(2));
        assert_eq!(sheet_index_of(&wb, Some("my sheet"), 0), Ok(2));
    }

    /// A sheet that isn't there is refused BY NAME, never quietly swapped for
    /// the one on screen — that silent redirect is the bug this syntax removes.
    #[test]
    fn sheet_index_of_refuses_a_sheet_that_isnt_there() {
        let wb = names(&["Sheet1", "Budget"]);

        assert_eq!(
            sheet_index_of(&wb, Some("Forecast"), 0),
            Err("there's no sheet called \"Forecast\"".to_string())
        );
        // Not the active sheet's index by another route.
        assert!(sheet_index_of(&wb, Some("Forecast"), 1).is_err());
        // A near miss is still a miss.
        assert!(sheet_index_of(&wb, Some("Budgets"), 0).is_err());
        // An empty workbook has nothing to name.
        assert!(sheet_index_of(&[], Some("Sheet1"), 0).is_err());
    }

    /// Excel forbids two sheets whose names differ only in case, but a
    /// hand-built file can carry them. The first wins; nothing panics.
    #[test]
    fn sheet_index_of_takes_the_first_of_two_names_differing_only_in_case() {
        let wb = names(&["budget", "Budget"]);
        assert_eq!(sheet_index_of(&wb, Some("Budget"), 1), Ok(0));
        assert_eq!(sheet_index_of(&wb, Some("budget"), 1), Ok(0));
        assert_eq!(sheet_index_of(&wb, Some("BUDGET"), 1), Ok(0));
    }

    /// The two halves compose: what `parse_ref_text` pulls out of a field is
    /// exactly what the lookup takes.
    #[test]
    fn a_parsed_ref_resolves_to_the_sheet_it_named() {
        let wb = names(&["Sheet1", "Budget", "Bob's Data"]);
        let resolve = |text: &str, active: usize| {
            let r = parse_ref_text(text).expect("parses");
            sheet_index_of(&wb, r.sheet.as_deref(), active).map(|i| (i, r.range))
        };

        assert_eq!(resolve("=Budget!$A$1:$D$5", 0), Ok((1, (0, 0, 4, 3))));
        assert_eq!(resolve("A1:D5", 2), Ok((2, (0, 0, 4, 3))));
        assert_eq!(resolve("'Bob''s Data'!A1", 0), Ok((2, (0, 0, 0, 0))));
        assert_eq!(
            resolve("=Forecast!A1:D5", 0),
            Err("there's no sheet called \"Forecast\"".to_string())
        );
    }

    // A uniform-width sheet: every column is `w` px.
    fn uniform(w: f32) -> impl Fn(u32) -> f32 {
        move |_c| w
    }

    /// The grid and the formula text must colour a cell by the SAME reference.
    /// `=SUM(B2:B5)/B3` covers B3 twice; the text draws it as the second token,
    /// so a grid that took the first covering ref would leave that reference
    /// with no cell of its own colour anywhere.
    #[test]
    fn overlapping_refs_colour_by_the_innermost() {
        // Taken from the real scan, not written out: hardcoding it here would
        // let the two sides drift apart while both tests stayed green.
        let refs: Vec<_> = formula_ref_tokens("=SUM(B2:B5)/B3")
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        // B2:B5 (rows 1..4, col 1) then B3 (row 2, col 1).
        assert_eq!(refs, vec![(1, 1, 4, 1), (2, 1, 2, 1)]);
        assert_eq!(ref_index_at(&refs, 2, 1), Some(1)); // B3 → the inner ref
        assert_eq!(ref_index_at(&refs, 1, 1), Some(0)); // B2 → only the outer
        assert_eq!(ref_index_at(&refs, 4, 1), Some(0)); // B5 → only the outer
        assert_eq!(ref_index_at(&refs, 0, 1), None); // B1 → neither
        assert_eq!(ref_index_at(&refs, 2, 2), None); // C3 → neither
        assert_eq!(ref_index_at(&[], 0, 0), None);
        // Two identical refs: the earlier index wins, so the colour is stable.
        assert_eq!(ref_index_at(&[(0, 0, 0, 0), (0, 0, 0, 0)], 0, 0), Some(0));
    }

    /// The text side of the same formula, so the two assertions sit together:
    /// `edit_runs` gives B3 the second reference's colour.
    #[test]
    fn edit_runs_colours_a_nested_ref_by_its_own_token() {
        let runs = edit_runs("=SUM(B2:B5)/B3", 0);
        let b3 = runs.iter().find(|(_, s, _)| s == "B3").expect("B3 run");
        assert_eq!(b3.2, Some(1));
        let outer = runs.iter().find(|(_, s, _)| s == "B2:B5").expect("B2:B5");
        assert_eq!(outer.2, Some(0));
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
    fn col_at_x_locates_the_pressed_column() {
        // 100px columns, no frozen ones, scrolled so column 5 is leftmost.
        let w = |_c: u32| 100.0f32;
        let g = super::SHEET_GUT;
        // The gutter is a row header, not a cell.
        assert_eq!(col_at_x(w, 0.0, 0, 5, 255), None);
        assert_eq!(col_at_x(w, g - 1.0, 0, 5, 255), None);
        // First pixel past the gutter is the leftmost scrolled column.
        assert_eq!(col_at_x(w, g, 0, 5, 255), Some(5));
        assert_eq!(col_at_x(w, g + 99.0, 0, 5, 255), Some(5));
        // A boundary belongs to the column it opens — the off-by-one that made
        // a press near an edge anchor a cell late.
        assert_eq!(col_at_x(w, g + 100.0, 0, 5, 255), Some(6));
        assert_eq!(col_at_x(w, g + 250.0, 0, 5, 255), Some(7));

        // Frozen columns come first and are always at the left, whatever col0 is.
        assert_eq!(col_at_x(w, g, 2, 9, 255), Some(0));
        assert_eq!(col_at_x(w, g + 150.0, 2, 9, 255), Some(1));
        assert_eq!(
            col_at_x(w, g + 200.0, 2, 9, 255),
            Some(9),
            "past the frozen band comes col0"
        );
        // Past the last column there is no cell.
        assert_eq!(col_at_x(w, 100_000.0, 0, 0, 255), None);
    }

    #[test]
    fn a_series_name_is_only_read_as_a_reference_when_it_was_changed() {
        use super::{NameCommit, series_name_commit};
        // The field is seeded with the name the series reports, so committing
        // it untouched must change nothing — even when that name happens to
        // read as a cell. `Q1`..`Q4` and `H1`/`H2` are the common headers.
        assert_eq!(series_name_commit("Q1", "Q1"), NameCommit::Unchanged);
        assert_eq!(series_name_commit("H1", "H1"), NameCommit::Unchanged);
        assert_eq!(series_name_commit(" Qty ", "Qty"), NameCommit::Unchanged);
        // A name that came from a cell is shown resolved; re-committing it must
        // not sever the link either.
        assert_eq!(
            series_name_commit("Revenue", "Revenue"),
            NameCommit::Unchanged
        );
        // Actually typing a reference points the name at those cells.
        assert_eq!(
            series_name_commit("B1", "Qty"),
            NameCommit::Ref(super::RefText {
                sheet: None,
                range: (0, 1, 0, 1)
            })
        );
        assert_eq!(
            series_name_commit("$D$7", "Qty"),
            NameCommit::Ref(super::RefText {
                sheet: None,
                range: (6, 3, 6, 3)
            })
        );
        // A qualifier survives the commit: this is the name of a cell on
        // ANOTHER sheet, and `series_apply_name` resolves it there.
        assert_eq!(
            series_name_commit("=Budget!$B$1", "Qty"),
            NameCommit::Ref(super::RefText {
                sheet: Some("Budget".into()),
                range: (0, 1, 0, 1)
            })
        );
        // Anything else is the name itself.
        assert_eq!(series_name_commit("Revenue", "Qty"), NameCommit::Literal);
        assert_eq!(series_name_commit("", "Qty"), NameCommit::Literal);
    }

    #[test]
    fn a_typed_series_name_drops_the_fields_seeded_equals() {
        use super::literal_series_name;
        // The field seeds itself with `=Budget!$B$1:$B$1`, so a label edited in
        // over the reference keeps that `=` unless it's taken off here — and the
        // `=` would go into the file as part of the series' name.
        assert_eq!(literal_series_name("=Total"), "Total");
        assert_eq!(literal_series_name(" = My Label "), "My Label");
        // A plain label is untouched, and one that only says `=` names nothing.
        assert_eq!(literal_series_name("Revenue"), "Revenue");
        assert_eq!(literal_series_name("="), "");
        // Only the leading `=` goes; one inside the label is part of it.
        assert_eq!(literal_series_name("Q1=Q2"), "Q1=Q2");
    }

    #[test]
    fn a_charts_box_is_rebuilt_from_the_references_its_slots_hold() {
        use gridcore::sheet::{ChartData, ChartSeries, ChartSource};
        let src = |sheet: &str, range| ChartSource {
            sheet: sheet.into(),
            range,
            cat_col: 0,
        };
        // A two-series chart over `Data!A1:C5`: a header cell naming each
        // series, a label column, and a numeric column each.
        let chart = |ser: Vec<ChartSeries>, cats: Option<ChartSource>, box_| ChartData {
            series: ser,
            categories_ref: cats,
            source: box_,
            ..ChartData::default()
        };
        let ser = |name_ref: Option<&str>, vals: Option<ChartSource>| ChartSeries {
            name_ref: name_ref.map(str::to_string),
            values_ref: vals,
            ..ChartSeries::default()
        };

        // Every slot on one sheet: the box is their union, and it reaches row 1
        // because the name cells are folded in — the loader folds `<c:tx>` too,
        // and leaving it out would shrink DATA RANGE to `A2:C5`.
        let mut d = chart(
            vec![
                ser(Some("Data!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Data!$C$1"), Some(src("Data", (1, 2, 4, 2)))),
            ],
            Some(src("Data", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 2))),
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 4, 2)))
        );

        // The finding this test exists for: once EVERY slot has moved to
        // another sheet, the box follows. Growing the old box instead left it
        // naming "Data" — a sheet nothing read any more — and the panel seeded
        // DATA RANGE from it, so Enter there replotted the chart from the cells
        // the user had just moved away from.
        let mut d = chart(
            vec![
                ser(Some("Budget!$B$1"), Some(src("Budget", (1, 1, 4, 1)))),
                ser(Some("Budget!$C$1"), Some(src("Budget", (1, 2, 4, 2)))),
            ],
            Some(src("Budget", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 2))),
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Budget", (0, 0, 4, 2))),
            "the box follows the references off the sheet once none of them read it"
        );

        // Still mixed: `union` keeps the RECEIVER's name, so stretching would
        // leave the box saying "Data" over cells on "Budget" — and
        // `chart_space_xml` derives refs from that box. The first slot in
        // writing order wins and the foreign one is skipped, as it is in the
        // loader when it meets the same clash.
        let mut d = chart(
            vec![
                ser(Some("Data!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Budget!$C$1"), Some(src("Budget", (9, 9, 9, 9)))),
            ],
            Some(src("Data", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 1))),
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 4, 1)))
        );

        // A single foreign series NAME must not take the box off the cells the
        // chart plots. Folded first it would seed the box with `Budget!$B$1`
        // and every local slot after it would be skipped for the sheet
        // mismatch, so a chart over `Data!A1:C5` would read as one cell of
        // Budget — DATA RANGE would show it, and Enter there would refuse the
        // 1x1 box. Names go in LAST, so the plotted cells decide the sheet.
        let mut d = chart(
            vec![
                ser(Some("Budget!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Data!$C$1"), Some(src("Data", (1, 2, 4, 2)))),
            ],
            Some(src("Data", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 2))),
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 4, 2))),
            "the foreign name is skipped, not made the box"
        );

        // The same, one slot over: `categories_apply` resolves a foreign sheet
        // too (`target_takes_foreign_sheet(Categories)` is `true`), so pointing
        // CATEGORY LABELS at `Budget!$A$2:$A$5` must not take the box off the
        // numbers either. Folded before the values it would seed the box and
        // every local slot after it would be skipped, so DATA RANGE would read
        // `=Budget!$A$2:$A$5` for a chart plotting `Data!B2:C5` — and Switch
        // Row/Column re-derives the whole chart from it.
        let mut d = chart(
            vec![
                ser(Some("Data!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Data!$C$1"), Some(src("Data", (1, 2, 4, 2)))),
            ],
            Some(src("Budget", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 2))),
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 1, 4, 2))),
            "the foreign categories ref is skipped, not made the box"
        );

        // With nothing else to go on, though, a name still seeds the box —
        // otherwise a chart whose slots are all name cells would have none.
        let mut d = chart(vec![ser(Some("Budget!$B$1"), None)], None, None);
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Budget", (0, 1, 0, 1)))
        );

        // A chart whose series are all literal still has labels to cover.
        let mut d = chart(vec![ser(None, None)], Some(src("Data", (1, 0, 4, 0))), None);
        super::rebuild_source(&mut d);
        assert_eq!(d.source.map(|s| s.range), Some((1, 0, 4, 0)));

        // And the labels, not the header, are what it takes its SHEET from —
        // the categories-before-names half of the order, which the cases above
        // cannot show because each of them has numbers to seed the box. Fold
        // the name first and `Budget!$B$1` seeds it, the local categories ref
        // is skipped for the sheet mismatch, and DATA RANGE reads a 1x1 box on
        // a sheet the chart takes no label from.
        let mut d = chart(
            vec![ser(Some("Budget!$B$1"), None)],
            Some(src("Data", (1, 0, 4, 0))),
            None,
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (1, 0, 4, 0))),
            "the labels decide the sheet when there are no numbers, not the header"
        );

        // A SCATTER plots from `<c:xVal>`/`<c:yVal>`, so `values_ref` is empty
        // however live it is. Its points are carried in `point_refs` and folded
        // with the numbers — otherwise the lone name cell would be the only
        // slot here, and committing a series name would collapse the box of a
        // chart plotting `A2:B3` onto `B1`.
        let mut d = chart(
            vec![ChartSeries {
                name_ref: Some("Data!$B$1".into()),
                point_refs: vec![src("Data", (1, 0, 2, 0)), src("Data", (1, 1, 2, 1))],
                ..ChartSeries::default()
            }],
            None,
            Some(src("Data", (0, 0, 2, 1))),
        );
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 2, 1))),
            "a scatter's points are numbers and hold its box open"
        );

        // Deleting a series takes its slots with it, so the box SHRINKS —
        // `series_delete` rebuilds for this. Left alone the box would go on
        // covering column C, DATA RANGE would keep offering `A1:C5`, and Enter
        // on that untouched field would re-derive the deleted series.
        let mut d = chart(
            vec![
                ser(Some("Data!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Data!$C$1"), Some(src("Data", (1, 2, 4, 2)))),
            ],
            Some(src("Data", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 2))),
        );
        assert!(super::series_remove(&mut d.series, 1));
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 4, 1))),
            "the deleted series' column is out of the box"
        );

        // The LIMIT of that, pinned so it is recorded rather than implied away:
        // the box is a rectangle, so it can only shrink off a column at its
        // ENDS. Delete the MIDDLE series of `A1:D5` and the survivors' refs
        // still span B..D — DATA RANGE goes on offering `A1:D5`, and Enter on
        // that untouched field re-derives the deleted series. `parse_chart`
        // computes the same unshrunk box on the next open, so the panel is
        // honest either way; `series_delete` rebuilds for that agreement, not
        // for a shrink it cannot always deliver.
        let mut d = chart(
            vec![
                ser(Some("Data!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Data!$C$1"), Some(src("Data", (1, 2, 4, 2)))),
                ser(Some("Data!$D$1"), Some(src("Data", (1, 3, 4, 3)))),
            ],
            Some(src("Data", (1, 0, 4, 0))),
            Some(src("Data", (0, 0, 4, 3))),
        );
        assert!(super::series_remove(&mut d.series, 1));
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 4, 3))),
            "a middle delete leaves the box exactly as wide"
        );

        // Reordering changes no reference, but it changes which one seeds the
        // box — the first values ref decides the sheet, and pointing a series
        // at another sheet is a supported commit. `series_reorder` rebuilds so
        // the panel agrees with the document order `parse_chart` will read back.
        let mut d = chart(
            vec![
                ser(Some("Data!$B$1"), Some(src("Data", (1, 1, 4, 1)))),
                ser(Some("Budget!$C$1"), Some(src("Budget", (1, 2, 4, 2)))),
            ],
            None,
            Some(src("Data", (0, 0, 4, 2))),
        );
        assert!(super::series_move(&mut d.series, 1, -1).is_some());
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Budget", (0, 2, 4, 2))),
            "the series now drawn first decides the sheet"
        );

        // And so does one with no `<c:ser>` at all, whose categories were first
        // set through `categories_apply`.
        let mut d = chart(Vec::new(), Some(src("Data", (1, 0, 4, 0))), None);
        super::rebuild_source(&mut d);
        assert_eq!(d.source.map(|s| s.range), Some((1, 0, 4, 0)));

        // Nothing parsable anywhere: the box it had stands, since there is
        // nothing to rebuild it from and blanking it would empty DATA RANGE.
        let mut d = chart(vec![ser(None, None)], None, Some(src("Data", (0, 0, 4, 2))));
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 0, 4, 2)))
        );
    }

    /// Converting a scatter to a kind the writer authors drops the scatter-only
    /// refs, so the box the panel rebuilds afterwards describes what the
    /// converted chart actually plots.
    #[test]
    fn picking_a_writable_type_clears_a_scatters_point_refs() {
        use gridcore::sheet::{ChartData, ChartSeries, ChartSource};
        let src = |range| ChartSource {
            sheet: "Data".into(),
            range,
            cat_col: 0,
        };
        // A scatter named from `B1`, X in `A2:A3`, Y in `B2:B3`.
        let scatter = || ChartData {
            kind: "scatter".into(),
            source: Some(src((0, 0, 2, 1))),
            series: vec![ChartSeries {
                name_ref: Some("Data!$B$1".into()),
                point_refs: vec![src((1, 0, 2, 0)), src((1, 1, 2, 1))],
                ..ChartSeries::default()
            }],
            ..ChartData::default()
        };

        // The shape that actually reaches `chart_take_kind` with points still
        // on it: a scatter whose series was RE-POINTED through SERIES VALUES,
        // which the field offers for every kind, so a `values_ref` now sits
        // beside the `<c:xVal>`/`<c:yVal>` refs (`ChartSeries::point_refs`).
        // One with no values at all never gets here — `chart_set_kind` sends
        // that one through `chart_reauthored` instead.
        let mut d = scatter();
        d.series[0].values_ref = Some(src((1, 1, 2, 1)));
        d.series[0].col = Some(1);
        super::chart_take_kind(&mut d, "column");
        assert_eq!(d.kind, "column");
        assert!(!d.complex);
        assert!(
            d.series[0].point_refs.is_empty(),
            "a column chart has no `<c:xVal>` for them to describe"
        );
        // The box is left alone by the conversion itself: it is what DATA RANGE
        // offers, and the panel may be about to re-derive from it.
        assert_eq!(d.source.as_ref().map(|s| s.range), Some((0, 0, 2, 1)));

        // And the next rebuild — a series name, a delete, a reorder — folds the
        // `values_ref` that is still there, so the box describes what the
        // converted chart plots instead of being stretched back over the
        // obsolete X column. That it does not collapse onto the lone `B1` name
        // cell is what the `values_ref` guarantees.
        super::rebuild_source(&mut d);
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 1, 2, 1))),
        );

        // Re-pointing after the conversion agrees with it.
        let n = super::series_set_values(&mut d, 0, vec![1.0, 2.0], src((1, 1, 2, 1)));
        assert_eq!(n, Some(2));
        assert_eq!(
            d.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Data", (0, 1, 2, 1))),
        );

        // A scatter that STAYS a scatter keeps them: its part round-trips
        // verbatim, so the next `parse_chart` reads those very refs back, and
        // dropping them here would collapse the box the panel shows.
        let mut d = scatter();
        super::chart_take_kind(&mut d, "scatter");
        assert_eq!(d.series[0].point_refs.len(), 2);

        // The unconverted scatter is the one holding points the writer cannot
        // emit, which is what routes it away from a bare relabel.
        assert!(super::chart_would_lose_points(&scatter()));
        let mut live = scatter();
        live.series[0].values_ref = Some(src((1, 1, 2, 1)));
        assert!(!super::chart_would_lose_points(&live));
        // A snapshot series holds no refs at all and still plots: the writer
        // writes its cached numbers back as a `<c:numLit>`.
        let mut snap = scatter();
        snap.series[0].values = vec![1.0, 2.0];
        assert!(!super::chart_would_lose_points(&snap));
        // Asked per SERIES: re-pointing ONE series of a two-series scatter does
        // not make the other one safe to relabel. `chart_space_xml` writes each
        // series from its own slots, so a relabel here would keep the half the
        // user touched and write `<c:ptCount val="0"/>` over the half nobody
        // did — a partial loss no `complex` holds the part back for.
        let mut mixed = scatter();
        mixed.series.push(mixed.series[0].clone());
        mixed.series[0].values_ref = Some(src((1, 1, 2, 1)));
        mixed.series[0].col = Some(1);
        assert!(super::chart_would_lose_points(&mixed));
        // But an empty series the USER built is not a loss: "+ Series" pushes
        // one with no refs and no numbers (`values` is empty until the chart has
        // categories), and re-deriving the chart over it would throw away the
        // hand edits on every other series instead.
        let mut added = live.clone();
        added.series.push(ChartSeries {
            name: "Series 2".into(),
            ..ChartSeries::default()
        });
        assert!(!super::chart_would_lose_points(&added));
        // Points the LOADER could not hold look exactly like that empty series
        // from here — no refs, no `col`, no numbers — so they are marked, and
        // the mark is the other half of the question. A `<c:xVal><c:numLit>`
        // scatter, or one naming a whole column, has just as much to destroy.
        let mut unheld = scatter();
        unheld.series[0].point_refs.clear();
        assert!(!super::chart_would_lose_points(&unheld));
        unheld.series[0].points_unheld = true;
        assert!(super::chart_would_lose_points(&unheld));
        // And it is cleared with the refs once the chart is converted, so the
        // relabel a later click makes is not re-derived all over again.
        let mut took = unheld.clone();
        took.series[0].points_ref_unheld = true;
        took.series[0].values_ref = Some(src((1, 1, 2, 1)));
        super::chart_take_kind(&mut took, "column");
        assert!(!took.series[0].points_unheld);
        assert!(!took.series[0].points_ref_unheld);
        // No series is not a plot to destroy.
        assert!(!super::chart_would_lose_points(&ChartData::default()));
    }

    /// Picking a writable type on a chart the writer would save EMPTY authors it
    /// afresh from its own box, rather than relabelling it and letting the next
    /// save overwrite the part with series of nothing.
    #[test]
    fn picking_a_writable_type_authors_a_valueless_scatter_afresh() {
        use gridcore::sheet::{Cell, ChartData, ChartSeries, ChartSource, Sheet};
        // `Data`: X down column A, Y down column B under a header.
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        sh.set_cell(0, 1, Cell::text("Speed"));
        for (i, (x, y)) in [(1.0, 10.0), (2.0, 20.0), (3.0, 30.0)].iter().enumerate() {
            let r = i as u32 + 1;
            sh.set_cell(r, 0, Cell::number(*x));
            sh.set_cell(r, 1, Cell::number(*y));
        }
        let src = |range| ChartSource {
            sheet: "Data".into(),
            range,
            cat_col: 0,
        };
        // As `parse_chart` hands a scatter over: refs, no numbers, no `col`.
        let scatter = ChartData {
            kind: "scatter".into(),
            title: "Trial 1".into(),
            source: Some(src((0, 0, 3, 1))),
            part: Some("xl/charts/chart1.xml".into()),
            series: vec![ChartSeries {
                name: "Speed".into(),
                name_ref: Some("Data!$B$1".into()),
                point_refs: vec![src((1, 0, 3, 0)), src((1, 1, 3, 1))],
                ..ChartSeries::default()
            }],
            ..ChartData::default()
        };

        let out = super::chart_reauthored(&scatter, "column", &sh).expect("re-derived");
        assert_eq!(out.kind, "column");
        assert!(!out.complex);
        // Every column of the box is numbers, so both become series — the same
        // reading Excel gives a scatter converted to a column chart, and the
        // point of re-deriving: each one now carries a `values_ref` the writer
        // can emit, where the relabelled chart had none.
        assert_eq!(out.series.len(), 2);
        assert!(out.series.iter().all(|s| s.values_ref.is_some()));
        assert!(out.series.iter().all(|s| s.point_refs.is_empty()));
        assert!(!super::chart_would_lose_points(&out));
        // The typed title rides along; so does the part, and it must — the
        // writer only overwrites a chart part it knows, so dropping it would
        // leave the original scatter on disk and the conversion invisible.
        assert_eq!(out.title, "Trial 1");
        assert_eq!(out.part.as_deref(), Some("xl/charts/chart1.xml"));

        // The box is now the plot's own union rather than an inherited one, so
        // the rebuild the next panel commit runs leaves it alone.
        let mut after = out.clone();
        super::rebuild_source(&mut after);
        assert_eq!(after.source, out.source);

        // A pie re-derives the same way and is no longer refused for its count:
        // two numeric columns are two series, both kept, the first drawn.
        let pie = super::chart_reauthored(&scatter, "pie", &sh).expect("two series");
        assert_eq!(pie.kind, "pie");
        assert_eq!(pie.series.len(), 2);
        assert_eq!(super::chart_plotted_series(&pie.kind, pie.series.len()), 1);

        // And a box with no numbers under its header is refused with the shape
        // this chart reads, not with a silent empty chart.
        let mut blank = scatter.clone();
        blank.source = Some(src((0, 0, 0, 1)));
        let err = super::chart_reauthored(&blank, "column", &sh).expect_err("header row only");
        assert!(
            err.contains("no column of numbers under a header row"),
            "{err}"
        );

        // Nothing to re-derive from at all: the message every door prints for a
        // chart whose references the model can't hold.
        let mut boxless = scatter.clone();
        boxless.source = None;
        assert_eq!(
            super::chart_reauthored(&boxless, "column", &sh).unwrap_err(),
            super::CHART_NO_BOX
        );
    }

    /// The box an imported scatter actually loads with sits ON its points —
    /// `parse_chart` folds it out of the `<c:xVal>`/`<c:yVal>` refs, and a
    /// literal `<c:tx>` name leaves nothing to stretch it up over a header row.
    /// `chart_from_range` reads every box the other way, so re-deriving that one
    /// unchanged would eat the first data row as headings.
    #[test]
    fn re_authoring_a_scatter_whose_box_sits_on_its_points_keeps_every_point() {
        use gridcore::sheet::{Cell, ChartData, ChartSeries, ChartSource, Sheet};
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        sh.set_cell(0, 1, Cell::text("Speed"));
        for (i, (x, y)) in [(1.0, 10.0), (2.0, 20.0), (3.0, 30.0)].iter().enumerate() {
            let r = i as u32 + 1;
            sh.set_cell(r, 0, Cell::number(*x));
            sh.set_cell(r, 1, Cell::number(*y));
        }
        // The same trial laid out SIDEWAYS, well clear of the block above so the
        // column cases read the cells they always did: names down column A, the
        // five X's along row 7 and the five Y's along row 8.
        sh.set_cell(6, 0, Cell::text("Run A"));
        sh.set_cell(7, 0, Cell::text("Run B"));
        for c in 1..=5u32 {
            sh.set_cell(6, c, Cell::number(f64::from(c)));
            sh.set_cell(7, c, Cell::number(f64::from(c) * 10.0));
        }
        let src = |range| ChartSource {
            sheet: "Data".into(),
            range,
            cat_col: 0,
        };
        // No `name_ref`: the series name came as a literal `<c:v>Speed`, which
        // is what Excel writes for a typed one. So the box is A2:B4, the points
        // and nothing else.
        let scatter = ChartData {
            kind: "scatter".into(),
            title: "Trial 1".into(),
            source: Some(src((1, 0, 3, 1))),
            part: Some("xl/charts/chart1.xml".into()),
            series: vec![ChartSeries {
                name: "Speed".into(),
                point_refs: vec![src((1, 0, 3, 0)), src((1, 1, 3, 1))],
                ..ChartSeries::default()
            }],
            ..ChartData::default()
        };

        let out = super::chart_reauthored(&scatter, "column", &sh).expect("re-derived");
        // Widened up over row 1 — which does hold the header — so both columns
        // come back whole. Read as-is it would have been two series of TWO
        // points named `1` and `10`, the numbers it ate.
        assert_eq!(out.source.as_ref().unwrap().range, (0, 0, 3, 1));
        assert_eq!(out.series.len(), 2);
        assert!(out.series.iter().all(|s| s.values.len() == 3));
        assert_eq!(out.series[1].name, "Speed");
        assert_eq!(out.series[0].values, vec![1.0, 2.0, 3.0]);

        // A blank line above is still the right answer — the series come back
        // unnamed, which is all a literal `<c:tx>` amounts to here anyway — but
        // no line at all cannot be fixed, and is refused rather than eating a
        // row of the plot.
        let mut at_edge = scatter.clone();
        at_edge.source = Some(src((0, 0, 2, 1)));
        at_edge.series[0].point_refs = vec![src((0, 0, 2, 0)), src((0, 1, 2, 1))];
        let err = super::chart_reauthored(&at_edge, "column", &sh).expect_err("no room above");
        assert!(err.contains("Its points start at row 1"), "{err}");

        // Sideways, for a row-laid chart: the leading COLUMN is the label one.
        // This is the shape `infer_by_row` now answers `true` for off a scatter's
        // `point_refs` — points along `$B$7:$F$7`/`$B$8:$F$8` — and the whole
        // point of carrying `data.by_row` through this door, so it is driven end
        // to end rather than through the widening helper alone.
        let mut rows = scatter.clone();
        rows.by_row = true;
        rows.source = Some(src((6, 1, 7, 5)));
        rows.series[0].point_refs = vec![src((6, 1, 6, 5)), src((7, 1, 7, 5))];
        let range = super::chart_box_with_header(&rows, rows.source.as_ref().unwrap(), rows.by_row);
        assert_eq!(range, Some((6, 0, 7, 5)));
        let out = super::chart_reauthored(&rows, "column", &sh).expect("re-derived");
        // Widened LEFT over column A, not up over row 6: the orientation decides
        // which line is the header, and a box at row 0 would otherwise be refused
        // for "Its points start at row 1" on a chart whose problem is column A.
        assert_eq!(out.source.as_ref().unwrap().range, (6, 0, 7, 5));
        assert!(out.by_row);
        // One series per numeric ROW, five points each. Read the column way this
        // is the 8e defect exactly: five one-point series, saved over the part.
        assert_eq!(out.series.len(), 2);
        assert!(out.series.iter().all(|s| s.values.len() == 5));
        assert_eq!(out.series[0].name, "Run A");
        assert_eq!(out.series[1].name, "Run B");
        assert_eq!(out.series[1].values, vec![10.0, 20.0, 30.0, 40.0, 50.0]);

        // A box that DOES lead with a header — one a `<c:tx>` ref stretched up
        // over, or any box docxy derived — is left exactly as it stands.
        let mut headed = scatter.clone();
        headed.source = Some(src((0, 0, 3, 1)));
        assert_eq!(
            super::chart_box_with_header(&headed, headed.source.as_ref().unwrap(), headed.by_row),
            Some((0, 0, 3, 1))
        );

        // The added line is counted. `chart_range_sheet` applied the cap to the
        // box the chart arrived with, and this one is exactly at it: widened it
        // is a line over, so a box that only just fitted must not walk past the
        // cap on the strength of a header row nobody counted. Refused before
        // any cell is read, which is why the fixture needs none.
        let mut brim = scatter.clone();
        let rows = super::MAX_CHART_CELLS as u32 / 2;
        brim.source = Some(src((1, 0, rows, 1)));
        brim.series[0].point_refs = vec![src((1, 0, rows, 0)), src((1, 1, rows, 1))];
        let err = super::chart_reauthored(&brim, "column", &sh).expect_err("over the cap");
        assert!(
            err.contains(&format!(
                "Its range is {} cells",
                super::MAX_CHART_CELLS + 2
            )),
            "{err}"
        );

        // The flip door counts its own widening too, and widens sideways: a
        // column each row deep, so the box grows by a whole COLUMN.
        let mut wide = brim.clone();
        wide.source = Some(src((0, 1, rows - 1, 2)));
        wide.series[0].point_refs = vec![src((0, 1, rows - 1, 1)), src((0, 2, rows - 1, 2))];
        let err = super::chart_switch_row_column(&wide, &sh).expect_err("over the cap");
        assert!(err.contains("a chart plots at most"), "{err}");
    }

    /// The two ways a scatter's box can be short of its plot, both refused, and
    /// the two neighbouring shapes that are not.
    ///
    /// Short: `<c:xVal>` naming a whole column `parse_f_ref` refuses beside a
    /// `<c:yVal>` the loader held, and a held `<c:xVal>` on ANOTHER SHEET than
    /// the box (`fold_source` skips it rather than unioning across sheets).
    /// Either way `rebuild_source` folds one half only, so re-deriving would come
    /// back plotting one coordinate with the other outside the range it just
    /// re-read. Refused, with DATA RANGE named, which re-derives over whatever
    /// the user points at.
    #[test]
    fn re_authoring_refuses_a_scatter_whose_box_covers_only_the_held_half() {
        use gridcore::sheet::{Cell, ChartData, ChartSeries, ChartSource, Sheet};
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        sh.set_cell(0, 1, Cell::text("Speed"));
        for (i, (x, y)) in [(1.0, 10.0), (2.0, 20.0), (3.0, 30.0)].iter().enumerate() {
            let r = i as u32 + 1;
            sh.set_cell(r, 0, Cell::number(*x));
            sh.set_cell(r, 1, Cell::number(*y));
        }
        let src = |range| ChartSource {
            sheet: "Data".into(),
            range,
            cat_col: 0,
        };
        let partial = ChartData {
            kind: "scatter".into(),
            // The Y column alone, which is all there was to fold.
            source: Some(src((1, 1, 3, 1))),
            part: Some("xl/charts/chart1.xml".into()),
            series: vec![ChartSeries {
                name: "Speed".into(),
                point_refs: vec![src((1, 1, 3, 1))],
                points_unheld: true,
                points_ref_unheld: true,
                ..ChartSeries::default()
            }],
            ..ChartData::default()
        };
        let box_of = |d: &ChartData| d.source.clone().unwrap();
        assert!(super::chart_would_lose_points(&partial));
        assert!(super::chart_points_off_box(&partial, &box_of(&partial)));
        let err = super::chart_reauthored(&partial, "column", &sh).expect_err("box is short");
        assert!(err.contains("DATA RANGE"), "{err}");
        assert!(err.contains("covers only the rest"), "{err}");

        // A HELD ref on another sheet is the same defect by the other route:
        // both halves parsed, so nothing is marked unheld at all, but
        // `fold_source` skipped the foreign one and `chart_from_range` reads the
        // box's sheet only — so a re-derivation would lose the X column outright.
        let mut foreign = partial.clone();
        foreign.series[0].points_unheld = false;
        foreign.series[0].points_ref_unheld = false;
        foreign.series[0].point_refs.push(ChartSource {
            sheet: "Other".into(),
            range: (1, 0, 3, 0),
            cat_col: 0,
        });
        assert!(super::chart_would_lose_points(&foreign));
        assert!(super::chart_points_off_box(&foreign, &box_of(&foreign)));
        let err = super::chart_reauthored(&foreign, "column", &sh).expect_err("box is short");
        assert!(err.contains("covers only the rest"), "{err}");
        // Case is not what makes a sheet foreign, here or anywhere else a name
        // resolves.
        let mut same = foreign.clone();
        same.series[0].point_refs[1].sheet = "dATA".into();
        assert!(!super::chart_points_off_box(&same, &box_of(&same)));

        // The wider mark WITHOUT the narrower is the literal-points shape, and a
        // different one: those points live in no cells at all, so no box could
        // have covered them and the chart's own is the best that exists. Its box
        // is whatever the LABEL slots folded — a `<c:tx>` ref stretches up over
        // the header row, so it already leads with one — and it goes through.
        let mut literal = partial.clone();
        literal.source = Some(src((0, 0, 3, 1)));
        literal.series[0].point_refs.clear();
        literal.series[0].points_ref_unheld = false;
        assert!(!super::chart_points_off_box(&literal, &box_of(&literal)));
        let out = super::chart_reauthored(&literal, "column", &sh).expect("re-derived");
        assert_eq!(out.source.as_ref().unwrap().range, (0, 0, 3, 1));
        assert_eq!(out.series.len(), 2);

        // And a literal point element BESIDE a held ref is that same shape, not
        // the short-box one: the bubble whose `<c:bubbleSize>` is a `<c:numLit>`
        // has a box covering every cell its X and Y name. Asking `points_unheld`
        // here would refuse it for a reference it hasn't got.
        let mut lit_beside = literal.clone();
        lit_beside.series[0].point_refs = vec![src((1, 0, 3, 1))];
        assert!(super::chart_would_lose_points(&lit_beside));
        assert!(!super::chart_points_off_box(
            &lit_beside,
            &box_of(&lit_beside)
        ));
        super::chart_reauthored(&lit_beside, "column", &sh).expect("re-derived");

        // Switch Row/Column re-derives through the same `chart_from_range`, so
        // it loses the same half and is refused in the same words — which is
        // what greys the button out with the reason under it. The literal case
        // still flips, for the reason it still converts.
        for short in [&partial, &foreign] {
            let err = super::chart_switch_row_column(short, &sh).expect_err("box is short");
            assert!(err.contains("covers only the rest"), "{err}");
        }
        // Whatever the literal case's flip answers, it is never THIS refusal:
        // its box covers every cell its plot names.
        if let Err(e) = super::chart_switch_row_column(&lit_beside, &sh) {
            assert!(!e.contains("covers only the rest"), "{e}");
        }

        // A re-point puts the series beyond the question: the writer emits its
        // `values_ref` as it stands, the fold has that ref, and the unheld
        // element beside it is what any relabel clears anyway.
        let mut repointed = partial.clone();
        repointed.series[0].values_ref = Some(src((1, 1, 3, 1)));
        assert!(!super::chart_would_lose_points(&repointed));
        assert!(!super::chart_points_off_box(
            &repointed,
            &box_of(&repointed)
        ));
    }

    /// The Overview's worked example, as a sheet: one row per item, a header
    /// row of column headings. Charted by column it plots Qty/Unit price/Total
    /// against Laptop/Monitor/Keyboard; switched, it is the transpose.
    fn overview_sheet() -> gridcore::sheet::Sheet {
        use gridcore::sheet::{Cell, Sheet};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        let rows: [(&str, [f64; 3]); 3] = [
            ("Laptop", [2.0, 1199.0, 2398.0]),
            ("Monitor", [4.0, 249.5, 998.0]),
            ("Keyboard", [6.0, 39.99, 239.94]),
        ];
        for (c, h) in ["Item", "Qty", "Unit price", "Total"].iter().enumerate() {
            sh.set_cell(0, c as u32, Cell::text(h));
        }
        for (r, (name, nums)) in rows.iter().enumerate() {
            let r = r as u32 + 1;
            sh.set_cell(r, 0, Cell::text(name));
            for (c, n) in nums.iter().enumerate() {
                sh.set_cell(r, c as u32 + 1, Cell::number(*n));
            }
        }
        sh
    }

    #[test]
    fn switch_row_column_replots_the_range_the_other_way_round() {
        let sh = overview_sheet();
        let col = gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", false)
            .expect("column chart");
        // What docxy plots today: a series per numeric column, categories from
        // the label column.
        assert!(!col.by_row);
        let names: Vec<&str> = col.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Qty", "Unit price", "Total"]);
        assert_eq!(col.categories, vec!["Laptop", "Monitor", "Keyboard"]);

        // Switched, it is the chart the Overview's Excel screenshot shows.
        let row = super::chart_switch_row_column(&col, &sh).expect("switched");
        assert!(row.by_row);
        let names: Vec<&str> = row.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Laptop", "Monitor", "Keyboard"]);
        assert_eq!(row.categories, vec!["Qty", "Unit price", "Total"]);
        assert_eq!(row.series[0].values, vec![2.0, 1199.0, 2398.0]);
        assert_eq!(row.series[2].values, vec![6.0, 39.99, 239.94]);
        // The refs move with the plot, so a save writes the switched chart
        // rather than the one it was switched from.
        assert_eq!(
            row.series[0].values_ref.as_ref().map(|s| s.to_ref()),
            Some("Budget!$B$2:$D$2".to_string())
        );
        assert_eq!(row.series[0].name_ref.as_deref(), Some("Budget!$A$2"));
        assert_eq!(
            row.categories_ref.as_ref().map(|s| s.to_ref()),
            Some("Budget!$B$1:$D$1".to_string())
        );
        // The box it re-reads is the same box, so switching again is possible
        // and the DATA RANGE field doesn't move.
        assert_eq!(row.source.as_ref().map(|s| s.range), Some((0, 0, 3, 3)));
        // The look of the chart is kept: the type, and the plot area a stacked
        // or combo part held back.
        assert_eq!(row.kind, "column");
        assert_eq!(row.title, col.title);
    }

    #[test]
    fn switching_twice_returns_the_original_chart() {
        let sh = overview_sheet();
        let col = gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", false)
            .expect("column chart");
        let there = super::chart_switch_row_column(&col, &sh).expect("switched");
        let back = super::chart_switch_row_column(&there, &sh).expect("switched back");
        // Whole-value equality, not a field-by-field walk: a flip is a
        // re-derivation from the range, so a chart that came from that range
        // must come back byte for byte.
        assert_eq!(back, col);
    }

    #[test]
    fn switching_keeps_the_title_and_the_part_it_cannot_regenerate() {
        let sh = overview_sheet();
        let mut col =
            gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", false)
                .expect("column chart");
        col.title = "Q3 spend".into();
        col.part = Some("<c:chartSpace/>".into());
        col.complex = true;
        // Colours are the one thing that does NOT ride along: the switched
        // series are different data, so matching by position would paint
        // "Laptop" with the colour chosen for "Qty".
        col.series[0].color = Some(0xff0000);
        let row = super::chart_switch_row_column(&col, &sh).expect("switched");
        assert_eq!(row.title, "Q3 spend");
        assert_eq!(row.part.as_deref(), Some("<c:chartSpace/>"));
        assert!(row.complex, "a stacked/combo part must stay held back");
        assert!(row.series.iter().all(|s| s.color.is_none()));
    }

    #[test]
    fn a_chart_with_nothing_to_re_read_cannot_be_switched() {
        let sh = overview_sheet();
        let col = gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", false)
            .expect("column chart");
        // An imported chart whose refs the model couldn't hold has no box to
        // re-derive from; the panel greys the button out on this answer.
        let mut orphan = col.clone();
        orphan.source = None;
        assert!(super::chart_switch_row_column(&orphan, &sh).is_err());

        // A range that doesn't read the other way round refuses too. One
        // column wide, there is nothing to read ACROSS — a row of it is a
        // single cell — so the column chart is the only reading there is.
        let boxed = |range| {
            Some(gridcore::sheet::ChartSource {
                sheet: "Budget".into(),
                range,
                cat_col: 0,
            })
        };
        let mut narrow = col.clone();
        narrow.source = boxed((0, 0, 3, 0));
        assert!(super::chart_switch_row_column(&narrow, &sh).is_err());

        // And the transpose of that: a row-oriented chart one row deep has no
        // column to read down.
        let mut flat =
            gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", true)
                .expect("row chart");
        flat.source = boxed((1, 0, 1, 3));
        assert!(super::chart_switch_row_column(&flat, &sh).is_err());
    }

    /// The flip re-derives from the chart's own box, which for an imported
    /// scatter sits ON its points — the shape `chart_box_with_header` exists
    /// for, asked here for the orientation the flip is ABOUT to read the box as
    /// rather than the one it has.
    #[test]
    fn switching_a_scatter_whose_box_sits_on_its_points_does_not_eat_a_line() {
        use gridcore::sheet::{Cell, ChartData, ChartSeries, ChartSource, Sheet};
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        // A label column, then a header row over two columns of points.
        for (r, label) in [(1, "First"), (2, "Second"), (3, "Third")] {
            sh.set_cell(r, 0, Cell::text(label));
        }
        sh.set_cell(0, 1, Cell::text("X"));
        sh.set_cell(0, 2, Cell::text("Speed"));
        for (i, (x, y)) in [(1.0, 10.0), (2.0, 20.0), (3.0, 30.0)].iter().enumerate() {
            let r = i as u32 + 1;
            sh.set_cell(r, 1, Cell::number(*x));
            sh.set_cell(r, 2, Cell::number(*y));
        }
        let src = |range| ChartSource {
            sheet: "Data".into(),
            range,
            cat_col: 0,
        };
        // Points in B2:C4, and a literal `<c:tx>` name that left nothing to
        // stretch the box over them: it is B2:C4 too.
        let scatter = ChartData {
            kind: "scatter".into(),
            source: Some(src((1, 1, 3, 2))),
            series: vec![ChartSeries {
                name: "Speed".into(),
                point_refs: vec![src((1, 1, 3, 1)), src((1, 2, 3, 2))],
                ..ChartSeries::default()
            }],
            ..ChartData::default()
        };
        let row = super::chart_switch_row_column(&scatter, &sh).expect("switched");
        // Widened LEFT, not up: the flip reads the box by row, so the line it
        // needs is the label column. Read as-is, column B would have been eaten
        // as the series names and the plot would be one point short.
        assert_eq!(row.source.as_ref().unwrap().range, (1, 0, 3, 2));
        assert!(row.by_row);
        let names: Vec<&str> = row.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["First", "Second", "Third"]);
        assert_eq!(row.series[0].values, vec![1.0, 10.0]);

        // With no line to widen into it is refused, in the same words the
        // re-author door uses — not flipped onto a mangled plot whose every
        // series carries a `values_ref` the next save would write out.
        let mut at_edge = scatter.clone();
        at_edge.source = Some(src((1, 0, 3, 1)));
        at_edge.series[0].point_refs = vec![src((1, 0, 3, 0)), src((1, 1, 3, 1))];
        let err = super::chart_switch_row_column(&at_edge, &sh).expect_err("no room beside");
        assert!(err.contains("its points start at column A"), "{err}");

        // Re-pointing a series does not close the door behind it. The pick
        // fills `values_ref`, so the chart no longer has points to LOSE, but
        // `point_refs` stay on the series and `rebuild_source` still folds them
        // — the box sits on the points exactly as before, and asking the
        // narrower "would a relabel cost points" question here would hand that
        // box to `chart_from_rows` to eat column B as the series names.
        let mut repointed = scatter.clone();
        super::series_set_values(&mut repointed, 0, vec![10.0, 20.0, 30.0], src((1, 2, 3, 2)))
            .expect("re-pointed");
        assert!(!super::chart_would_lose_points(&repointed));
        assert_eq!(repointed.source.as_ref().unwrap().range, (1, 1, 3, 2));
        let row = super::chart_switch_row_column(&repointed, &sh).expect("switched");
        assert_eq!(row.source.as_ref().unwrap().range, (1, 0, 3, 2));
        let names: Vec<&str> = row.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["First", "Second", "Third"]);

        // A chart that plots through `<c:val>` is NOT widened: its box is the
        // one the user set in DATA RANGE, and pulling a column in beside it
        // would move the field under their hands and break the round trip.
        //
        // The box is B1:C4 — two numeric columns under a header row, so
        // `chart_from_columns` falls back to the FIRST as its category column
        // and the leading series' `values_ref` starts in the box's own leading
        // column. The plotted-line test therefore says "widen" here; only the
        // provenance gate keeps column A out, which is what this case pins.
        let col = gridcore::sheet::chart_from_range(&sh, "Data", (0, 1, 3, 2), "column", false)
            .expect("column chart");
        assert_eq!(
            super::chart_box_with_header(&col, col.source.as_ref().unwrap(), true),
            Some((0, 0, 3, 2)),
        );
        let flipped = super::chart_switch_row_column(&col, &sh).expect("switched");
        assert_eq!(flipped.source.as_ref().unwrap().range, (0, 1, 3, 2));
    }

    /// What the CARD draws, per kind and count. This is the rule the renderer
    /// used to get for free from the writer having thrown the extra series
    /// away; now that a pie keeps every series it holds, the card has to know
    /// that it still plots one of them.
    #[test]
    fn a_pie_draws_one_series_however_many_it_holds() {
        use super::chart_plotted_series as drawn;
        assert_eq!(drawn("pie", 0), 0);
        assert_eq!(drawn("pie", 1), 1);
        assert_eq!(drawn("pie", 2), 1);
        assert_eq!(drawn("pie", 7), 1);
        // Every other kind plots the lot, including the empty and the one-series
        // cases the pie shares with them.
        for k in ["column", "bar", "line", ""] {
            for n in [0, 1, 2, 7] {
                assert_eq!(drawn(k, n), n, "{k} with {n}");
            }
        }
        // And the invariant `chart_card` actually leans on: whatever the kind,
        // the drawn count never exceeds the held one, so `&data.series[..nser]`
        // cannot slice past the end. Said as an assertion rather than left in
        // the comment beside that slice, since it is the comment that would go
        // stale first if a future kind plotted something derived instead.
        for k in ["pie", "column", "bar", "line", "scatter", "doughnut", ""] {
            for n in [0, 1, 2, 3, 7, 64] {
                assert!(drawn(k, n) <= n, "{k} with {n} draws {}", drawn(k, n));
            }
        }

        // Per series, which is the question the panel's card asks: on a pie only
        // the leading card is drawn, on anything else all of them are.
        use super::series_is_plotted as shown;
        assert!(shown("pie", 0, 3));
        assert!(!shown("pie", 1, 3));
        assert!(!shown("pie", 2, 3));
        assert!(shown("pie", 0, 1));
        for si in 0..3 {
            assert!(shown("column", si, 3));
        }
    }

    /// What the PANEL says beside the type buttons, given a kind and a count.
    ///
    /// The note exists because the data now survives: the extra series of a pie
    /// are still in the file and still editable below, so the sentence says
    /// "kept but not drawn" rather than refusing anything.
    #[test]
    fn the_panel_says_a_pie_plots_the_first_series_only() {
        use super::chart_unplotted_note as note;
        // Nothing to say when everything the chart holds is drawn.
        assert_eq!(note("pie", 0), None);
        assert_eq!(note("pie", 1), None);
        for k in ["column", "bar", "line", ""] {
            for n in [0, 1, 2, 7] {
                assert_eq!(note(k, n), None, "{k} with {n}");
            }
        }
        // Two series: one extra, said in the singular — and as a WORD, since
        // this is the case the user meets most and "the other 1 is" reads as a
        // tally rather than a sentence.
        let two = note("pie", 2).expect("a two-series pie has something to say");
        assert!(two.contains("A pie plots the first series only"), "{two}");
        assert!(two.contains("the other one is kept in the file"), "{two}");
        assert!(!two.contains("the other 1 "), "{two}");
        // Three: two extra, in the plural.
        let three = note("pie", 3).expect("a three-series pie too");
        assert!(
            three.contains("the other 2 are kept in the file"),
            "{three}"
        );
        assert!(three.contains("not drawn"), "{three}");
    }

    /// The multi-series pie each door now hands over, and what the panel says
    /// about it.
    ///
    /// There were six refusals: `series_add`'s inline `n > 0`, and
    /// `chart_reauthored`, `chart_apply_range`, `chart_switched`,
    /// `chart_set_kind` and `sheet_insert_chart`, which all asked one shared
    /// `chart_kind_series_err`.
    /// Every one of them existed because `chart_space_xml` wrote a pie's FIRST
    /// series and dropped the rest, so a second could be pointed at cells and
    /// coloured and then lost on save without a word. The writer keeps them all
    /// now (`a_pie_writes_every_series_it_holds`, gridcore), so the refusals
    /// guard nothing — what is left to say is which of the series the plot
    /// draws, and that is `chart_plotted_series` and `chart_unplotted_note`.
    ///
    /// **What this test can and cannot reach.** Five of the six sites sit in
    /// `Docxy` methods that a unit test cannot call, for want of a constructed
    /// view whose Chart panel holds a chart (`panel_chart`, not `chart_sel` —
    /// the two came apart when the panel went sticky): `chart_apply_range`, `chart_set_kind`,
    /// `sheet_insert_chart` and `series_add` take `&mut self` and a
    /// `Context<Self>`, and `chart_switched` is `&self` (the render path calls
    /// it each frame to decide whether the Switch Row/Column button greys out)
    /// but needs that same view. What this test calls is the pure half each one
    /// delegates to — `chart_switch_row_column` under `chart_switched`,
    /// `chart_from_range` under `sheet_insert_chart`'s derivation, and
    /// `series_add`'s push written out, while `chart_set_kind`'s arm models its
    /// RELABEL path, where `chart_take_kind` keeps the series it finds and only
    /// rewrites the kind (its re-deriving branch runs through
    /// `chart_reauthored`, main.rs:3859, and is not what this arm walks) — and
    /// then asks `chart_plotted_series` / `chart_unplotted_note` what the panel
    /// makes of the result. So it pins the SHAPE each door produces and how it is
    /// described; it does not pin that the doors are unguarded. Re-adding a
    /// refusal inside one of those five `Docxy` bodies would leave this green.
    /// Four arms are written below; the fifth `Docxy` site, `chart_apply_range`,
    /// has no arm of its own, but its whole re-derivation is the same
    /// `chart_from_range` call these arms make (main.rs:3657), so the shape it
    /// hands over is the one pinned here. The sixth site, `chart_reauthored`, is
    /// the one that is a free function rather than a `Docxy` method, so it is
    /// the only one a unit test can call as ITSELF — and one does:
    /// `picking_a_writable_type_authors_a_valueless_scatter_afresh` walks it
    /// (main.rs:18392) and expects the two-series pie where it used to expect
    /// this very refusal. That one site is pinned against a re-added guard; the
    /// five `Docxy` ones are not.
    #[test]
    fn the_multi_series_pie_each_door_hands_over_is_described_not_refused() {
        use super::{chart_plotted_series as drawn, chart_unplotted_note as note};

        // The switch is the route that reaches it in one click: a pie over a
        // label column plus one numeric column reads, the other way round, as
        // one one-point series per row.
        let mut sh = gridcore::sheet::Sheet {
            name: "Budget".into(),
            ..Default::default()
        };
        for (addr, cell) in [
            ("A1", gridcore::sheet::Cell::text("Item")),
            ("B1", gridcore::sheet::Cell::text("Qty")),
            ("A2", gridcore::sheet::Cell::text("Laptop")),
            ("B2", gridcore::sheet::Cell::number(2.0)),
            ("A3", gridcore::sheet::Cell::text("Monitor")),
            ("B3", gridcore::sheet::Cell::number(4.0)),
            ("A4", gridcore::sheet::Cell::text("Keyboard")),
            ("B4", gridcore::sheet::Cell::number(6.0)),
        ] {
            let (r, c) = gridcore::sheet::parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        let pie = gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 1), "pie", false)
            .expect("pie");
        assert_eq!(pie.series.len(), 1);
        let flipped = super::chart_switch_row_column(&pie, &sh).expect("flip");
        assert_eq!(flipped.kind, "pie");
        assert_eq!(flipped.series.len(), 3);
        // All three are kept; one is drawn, and the panel says so.
        assert_eq!(drawn(&flipped.kind, flipped.series.len()), 1);
        assert!(
            note(&flipped.kind, flipped.series.len())
                .is_some_and(|m| m.contains("kept in the file"))
        );

        // The doors that do NOT re-derive. `chart_set_kind` keeps the series it
        // finds and rewrites the kind, so a column chart over a range with two
        // numeric columns becomes a pie holding both — the widest route, since
        // it is one click on the word "Pie", and now a lossless one.
        for (addr, cell) in [
            ("C1", gridcore::sheet::Cell::text("Price")),
            ("C2", gridcore::sheet::Cell::number(900.0)),
            ("C3", gridcore::sheet::Cell::number(150.0)),
            ("C4", gridcore::sheet::Cell::number(40.0)),
        ] {
            let (r, c) = gridcore::sheet::parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        let cols = gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 2), "column", false)
            .expect("cols");
        assert_eq!(cols.series.len(), 2);
        // What `chart_set_kind("pie", …)` commits: the SAME series, under a
        // kind that draws the first and keeps the second. Nothing is dropped,
        // so picking "column" straight back returns the chart it started as.
        assert_eq!(drawn("pie", cols.series.len()), 1);
        assert_eq!(drawn("column", cols.series.len()), 2);
        assert!(note("pie", cols.series.len()).is_some());
        assert_eq!(note("column", cols.series.len()), None);

        // And what `sheet_insert_chart("pie", …)` derives over the same range:
        // a series per numeric column, inserted as-is.
        let inserted = gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 2), "pie", false)
            .expect("pie");
        assert_eq!(inserted.series.len(), 2);
        assert_eq!(drawn(&inserted.kind, inserted.series.len()), 1);
        assert!(note(&inserted.kind, inserted.series.len()).is_some());

        // `series_add`'s push, which used to be refused at `n > 0`: a pie with
        // one series gets a second, and the panel marks it not plotted rather
        // than the button turning it away. (That it then SURVIVES the save is
        // `a_series_added_to_a_pie_survives_a_save`, gridcore — this side of
        // the wall cannot reach the writer.)
        let mut grown = pie.clone();
        grown.series.push(gridcore::sheet::ChartSeries {
            name: "Series 2".into(),
            values: vec![0.0; grown.categories.len()],
            ..Default::default()
        });
        assert_eq!(grown.series.len(), 2);
        assert_eq!(drawn(&grown.kind, grown.series.len()), 1);
        assert!(!super::series_is_plotted(
            &grown.kind,
            1,
            grown.series.len()
        ));
        assert!(super::series_is_plotted(&grown.kind, 0, grown.series.len()));
    }

    #[test]
    fn the_shape_a_series_may_be_re_pointed_at_follows_the_charts_orientation() {
        use super::series_values_shape_err as err;
        // A column chart: one column is the only accepted shape. `B2:B5` and a
        // single cell go through; anything wider is refused, and the message
        // names the shape it wants.
        assert_eq!(err(false, (1, 1, 4, 1)), None);
        assert_eq!(err(false, (1, 1, 1, 1)), None);
        assert_eq!(
            err(false, (1, 1, 1, 3)),
            Some("a series plots one column — point at cells like B2:B5")
        );
        assert!(err(false, (1, 1, 4, 3)).is_some());

        // A row chart is the transpose: `B2:D2` goes through, and the column
        // that a column chart wants is now the refused shape. The regression
        // this task fixes — before it, EVERY row was refused, so a
        // row-oriented series could not be re-pointed at all.
        assert_eq!(err(true, (1, 1, 1, 3)), None);
        assert_eq!(err(true, (1, 1, 1, 1)), None);
        assert_eq!(
            err(true, (1, 1, 4, 1)),
            Some("a series plots one row — point at cells like B2:D2")
        );
        assert!(err(true, (1, 1, 4, 3)).is_some());

        // The message must never send the user at the shape this chart would
        // refuse next time round.
        assert!(err(true, (1, 1, 4, 1)).unwrap().contains("B2:D2"));
        assert!(err(false, (1, 1, 1, 3)).unwrap().contains("B2:B5"));
    }

    /// Categories are one line too, and the line follows the chart the way the
    /// values' does: labels name a series' POINTS, which run down rows on a
    /// column chart and along columns on a row one. Taking either line on
    /// either orientation would let a user commit a shape `infer_by_row` reads
    /// back as the OTHER orientation, so the file would return flipped.
    #[test]
    fn the_category_labels_field_takes_the_line_this_chart_reads() {
        use super::categories_shape_err as err;
        // The line each reading's own derivation writes goes through.
        assert_eq!(err(false, (1, 0, 4, 0)), None, "a column chart's labels");
        assert_eq!(err(true, (0, 1, 0, 3)), None, "a row chart's labels");
        // One cell is one row and one column at once, so it fits both — which
        // is what a row chart over a two-column range has.
        for by_row in [false, true] {
            assert_eq!(err(by_row, (1, 0, 1, 0)), None, "one cell");
        }
        // The other reading's line is refused, and the message names the shape
        // THIS chart wants rather than the one that was picked.
        assert_eq!(
            err(false, (0, 1, 0, 3)),
            Some("category labels are one column — point at cells like A2:A5")
        );
        assert_eq!(
            err(true, (1, 0, 4, 0)),
            Some("category labels are one row — point at cells like B1:D1")
        );
        // A rectangle differs BOTH ways round, so the same check refuses it on
        // either orientation — the order mismatch that first motivated it.
        assert!(err(false, (0, 1, 2, 3)).is_some());
        assert!(err(true, (0, 1, 2, 3)).is_some());
        // The message must never send the user at the shape this chart would
        // refuse next time round.
        assert!(err(false, (0, 1, 0, 3)).unwrap().contains("A2:A5"));
        assert!(err(true, (1, 0, 4, 0)).unwrap().contains("B1:D1"));
    }

    /// The hints have to follow the orientation for the same reason the
    /// messages do — and the values hint most of all, since it is the one the
    /// guard above can REFUSE. An empty VALUES field on a row chart offering
    /// `B2:B5` would be telling the user to type the one range it rejects.
    #[test]
    fn the_example_cells_each_chart_field_offers_follow_the_charts_orientation() {
        use super::{chart_field_examples, series_values_shape_err as err};

        let col = chart_field_examples(false);
        let row = chart_field_examples(true);

        // Whatever the values hint offers, the guard must accept.
        assert_eq!(err(false, col.values), None, "column hint is refused");
        assert_eq!(err(true, row.values), None, "row hint is refused");
        // And each is the shape the OTHER orientation refuses, so they are
        // genuinely transposed rather than accidentally both accepted.
        assert!(err(true, col.values).is_some());
        assert!(err(false, row.values).is_some());

        // A series' name is one cell: the header above it by column, the label
        // to its left by row.
        assert_eq!(col.name, (0, 1, 0, 1));
        assert_eq!(row.name, (1, 0, 1, 0));
        // Categories run down a column one way round and along a row the other.
        assert!(col.categories.0 != col.categories.2);
        assert_eq!(col.categories.1, col.categories.3, "a column of labels");
        assert_eq!(row.categories.0, row.categories.2, "a row of labels");
        assert!(row.categories.1 != row.categories.3);
    }

    /// The DATA RANGE help names what the box must include, and that differs by
    /// orientation: the header row names a column chart's series, the label
    /// column a row chart's.
    #[test]
    fn the_data_range_help_names_the_line_that_holds_this_charts_series_names() {
        use super::chart_range_help;
        assert!(chart_range_help(false).contains("header row"));
        assert!(chart_range_help(true).contains("label column"));
        assert_ne!(chart_range_help(false), chart_range_help(true));
    }

    #[test]
    fn re_pointing_a_row_series_moves_its_ref_and_grows_the_charts_box() {
        use gridcore::sheet::ChartSource;
        let sh = overview_sheet();
        let mut row =
            gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", true)
                .expect("row chart");
        assert_eq!(row.source.as_ref().map(|s| s.range), Some((0, 0, 3, 3)));

        // Re-point "Laptop" at the Keyboard row instead, the way the panel's
        // VALUES field does once the guard has let the range through.
        let range = (3, 1, 3, 3);
        let src = ChartSource {
            sheet: "Budget".into(),
            range,
            cat_col: range.1,
        };
        let values = gridcore::sheet::range_numbers(&sh, range);
        let n = super::series_set_values(&mut row, 0, values, src).expect("series 0");
        assert_eq!(n, 3);
        assert_eq!(row.series[0].values, vec![6.0, 39.99, 239.94]);
        assert_eq!(
            row.series[0].values_ref.as_ref().map(|s| s.to_ref()),
            Some("Budget!$B$4:$D$4".to_string())
        );
        // `col` names a column, and a row series occupies every column of its
        // ref. Storing the left-hand one would arm the writer's fallback ref
        // and `claimed_col` with an answer that reads the chart the wrong way
        // round.
        assert_eq!(row.series[0].col, None);
        // `rebuild_source` unions rectangles, so it needs no orientation of its
        // own — the box still covers everything the chart reads, header row and
        // label column included.
        assert_eq!(row.source.as_ref().map(|s| s.range), Some((0, 0, 3, 3)));
        assert_eq!(
            row.source.as_ref().map(|s| s.sheet.as_str()),
            Some("Budget")
        );

        // Point it off the box, and the box grows to cover the new cells —
        // otherwise DATA RANGE would go on naming a rectangle the chart no
        // longer reads all of.
        let range = (5, 1, 5, 4);
        let src = ChartSource {
            sheet: "Budget".into(),
            range,
            cat_col: range.1,
        };
        super::series_set_values(&mut row, 0, gridcore::sheet::range_numbers(&sh, range), src)
            .expect("series 0");
        assert_eq!(row.source.as_ref().map(|s| s.range), Some((0, 0, 5, 4)));
        assert_eq!(row.series[0].col, None);
    }

    #[test]
    fn re_pointing_a_column_series_still_records_the_column_it_took() {
        use gridcore::sheet::ChartSource;
        let sh = overview_sheet();
        let mut col =
            gridcore::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 2), "column", false)
                .expect("column chart");
        let range = (1, 3, 3, 3);
        let src = ChartSource {
            sheet: "Budget".into(),
            range,
            cat_col: range.1,
        };
        let n =
            super::series_set_values(&mut col, 0, gridcore::sheet::range_numbers(&sh, range), src)
                .expect("series 0");
        assert_eq!(n, 3);
        // Unchanged behaviour for a column chart: `col` is the column it plots,
        // which the writer's fallback ref and `claimed_col` both read.
        assert_eq!(col.series[0].col, Some(3));
        assert_eq!(col.series[0].values, vec![2398.0, 998.0, 239.94]);
        assert_eq!(col.source.as_ref().map(|s| s.range), Some((0, 0, 3, 3)));

        // No series `i`: nothing to re-point, and nothing said about points.
        assert_eq!(
            super::series_set_values(
                &mut col,
                99,
                vec![1.0],
                ChartSource {
                    sheet: "Budget".into(),
                    range: (0, 0, 0, 0),
                    cat_col: 0,
                },
            ),
            None
        );
    }

    #[test]
    fn switching_re_reads_the_sheet_the_box_names_not_the_one_on_screen() {
        // `chart_switch_row_column` is handed the sheet, so the wrong one is a
        // possible mistake — pin that it plots whatever it is given and stamps
        // THAT sheet's name on the refs it writes.
        let mut other = overview_sheet();
        other.name = "Ledger".into();
        let col =
            gridcore::sheet::chart_from_range(&other, "Ledger", (0, 0, 3, 3), "column", false)
                .expect("column chart");
        let row = super::chart_switch_row_column(&col, &other).expect("switched");
        assert_eq!(
            row.series[0].values_ref.as_ref().map(|s| s.to_ref()),
            Some("Ledger!$B$2:$D$2".to_string())
        );
        assert_eq!(
            row.source.as_ref().map(|s| s.sheet.clone()),
            Some("Ledger".to_string())
        );
    }

    #[test]
    fn a_pointed_slot_reads_the_sheet_its_reference_named() {
        use gridcore::sheet::{Cell, Sheet};
        let sheet = |name: &str, n: f64| {
            let mut sh = Sheet {
                name: name.to_string(),
                ..Default::default()
            };
            sh.set_cell(0, 0, Cell::number(n));
            sh
        };
        let sheets = vec![sheet("Sheet1", 1.0), sheet("Budget", 42.0)];
        // The sheet in front of you is 0; the reference named Budget, so 1 is
        // what `sheet_index_of` resolved and 1 is what gets read — Budget's
        // numbers, not Sheet1's cells of the same name.
        let (sh, src) = super::ref_source(&sheets, 1, (0, 0, 0, 0)).unwrap();
        assert_eq!(gridcore::sheet::range_numbers(sh, (0, 0, 0, 0)), vec![42.0]);
        // And Budget's NAME on the source written back, which is what carries
        // the reference across a save.
        assert_eq!(src.sheet, "Budget");
        assert_eq!(src.to_ref(), "Budget!$A$1:$A$1");
        assert_eq!(super::source_ref_text(&src), "=Budget!$A$1:$A$1");
        // An unqualified reference resolves to the active sheet, and reads it.
        let (sh, src) = super::ref_source(&sheets, 0, (0, 0, 0, 0)).unwrap();
        assert_eq!(gridcore::sheet::range_numbers(sh, (0, 0, 0, 0)), vec![1.0]);
        assert_eq!(src.sheet, "Sheet1");
        // An index naming no sheet answers `None` rather than panicking.
        assert!(super::ref_source(&sheets, 2, (0, 0, 0, 0)).is_none());
        assert!(super::ref_source(&[], 0, (0, 0, 0, 0)).is_none());
    }

    #[test]
    fn parse_ref_text_accepts_what_a_range_field_is_typed() {
        let bare = |r| {
            Some(RefText {
                sheet: None,
                range: r,
            })
        };
        let on = |n: &str, r| {
            Some(RefText {
                sheet: Some(n.into()),
                range: r,
            })
        };
        // The plain forms, in any case, with or without surrounding space.
        assert_eq!(parse_ref_text("A1:D5"), bare((0, 0, 4, 3)));
        assert_eq!(parse_ref_text("  a1:d5 "), bare((0, 0, 4, 3)));
        // A single cell is a one-cell range.
        assert_eq!(parse_ref_text("C3"), bare((2, 2, 2, 2)));
        // $ anchors and a leading = are accepted and ignored.
        assert_eq!(parse_ref_text("$A$1:$D$5"), bare((0, 0, 4, 3)));
        assert_eq!(parse_ref_text("=$A$1:$D$5"), bare((0, 0, 4, 3)));
        // A Sheet! qualifier is KEPT — the sheet it names is what gets read.
        assert_eq!(parse_ref_text("Budget!A1:D5"), on("Budget", (0, 0, 4, 3)));
        assert_eq!(
            parse_ref_text("=Budget!$A$1:$D$5"),
            on("Budget", (0, 0, 4, 3))
        );
        // A quoted name loses its quotes; a doubled '' is one apostrophe.
        assert_eq!(
            parse_ref_text("'My Sheet'!A1:D5"),
            on("My Sheet", (0, 0, 4, 3))
        );
        assert_eq!(
            parse_ref_text("'Bob''s Data'!A1"),
            on("Bob's Data", (0, 0, 0, 0))
        );
        // Corners in the other order still name the same box.
        assert_eq!(parse_ref_text("D5:A1"), bare((0, 0, 4, 3)));
    }

    #[test]
    fn parse_ref_text_refuses_what_isnt_a_range() {
        // Nothing else is a range — notably the concatenation a field used to
        // produce when typing over an existing value appended instead.
        assert_eq!(parse_ref_text("A1:B5A1:D5"), None);
        assert_eq!(parse_ref_text(""), None);
        assert_eq!(parse_ref_text("total"), None);
        assert_eq!(parse_ref_text("A0"), None);
        // A qualifier with no cells after it names nothing to read.
        assert_eq!(parse_ref_text("Budget!"), None);
        assert_eq!(parse_ref_text("=Budget!"), None);
        assert_eq!(parse_ref_text("'My Sheet'!total"), None);
        // And cells with an EMPTY qualifier in front of them are refused rather
        // than read as this sheet's — Excel refuses both spellings too, and
        // taking them would land a reference on the sheet in front of you that
        // pointedly named none.
        assert_eq!(parse_ref_text("!A1:D5"), None);
        assert_eq!(parse_ref_text("=!A1:D5"), None);
        assert_eq!(parse_ref_text("''!A1:D5"), None);
    }

    #[test]
    fn unquote_sheet_name_undoes_the_writers_quoting() {
        assert_eq!(super::unquote_sheet_name("Budget"), Some("Budget".into()));
        assert_eq!(super::unquote_sheet_name(" Budget "), Some("Budget".into()));
        assert_eq!(
            super::unquote_sheet_name("'My Sheet'"),
            Some("My Sheet".into())
        );
        assert_eq!(
            super::unquote_sheet_name("'Bob''s Data'"),
            Some("Bob's Data".into())
        );
        // An empty qualifier names no sheet — which is why `parse_ref_text`
        // refuses `!A1` outright instead of reading it as this sheet's.
        assert_eq!(super::unquote_sheet_name(""), None);
        assert_eq!(super::unquote_sheet_name("''"), None);
    }

    #[test]
    fn range_targets_know_whether_they_hold_a_range() {
        use super::RefTarget;
        // A range target puts the grid in point mode; a text one must not, or
        // clicking a cell while renaming a chart would rewrite the title.
        assert!(RefTarget::ChartRange.is_range());
        assert!(!RefTarget::ChartTitle.is_range());
        // A series' cells, its name cell and the labels can all be pointed at.
        assert!(RefTarget::SeriesValues(0).is_range());
        assert!(RefTarget::SeriesName(2).is_range());
        assert!(RefTarget::Categories.is_range());
        // Targets are per series, so two series never share a field.
        assert_ne!(RefTarget::SeriesValues(0), RefTarget::SeriesValues(1));
    }

    #[test]
    fn ref_token_at_finds_the_reference_under_the_caret() {
        // Caret inside, at either edge of, or just after a reference finds it.
        let buf = "=B2*C2";
        assert_eq!(ref_token_at(buf, 2), Some(1..3), "inside B2");
        assert_eq!(ref_token_at(buf, 3), Some(1..3), "just after B2");
        assert_eq!(ref_token_at(buf, 6), Some(4..6), "end of the buffer");
        // A range counts as one token, colon and all.
        assert_eq!(
            ref_token_at("=SUM(D2:D5)", 9),
            Some(5..10),
            "D2:D5 spans bytes 5..10"
        );
        // Right after an operator or an open bracket there is nothing to
        // replace — a pick inserts there instead.
        assert_eq!(ref_token_at("=SUM(", 5), None);
        assert_eq!(ref_token_at("=B2*", 4), None);
        assert_eq!(ref_token_at("=B2,", 4), None);
        // A function name is not a reference, even when it reads like a cell:
        // LOG10 parses as column LOG row 10 unless the bracket is noticed.
        assert_eq!(ref_token_at("=LOG10(A1)", 6), None);
        assert_eq!(ref_token_at("=SUM(A1)", 4), None);
        // Nor is anything that simply doesn't name cells.
        assert_eq!(ref_token_at("=total", 6), None);
        assert_eq!(ref_token_at("", 0), None);
    }

    #[test]
    fn ref_token_at_will_not_repoint_another_sheets_reference() {
        // `Sheet2!A1` — replacing the cell half would silently swing that
        // reference onto THIS sheet's picked cell. A pick inserts instead.
        assert_eq!(ref_token_at("=Sheet2!A1", 10), None);
        assert_eq!(ref_token_at("=SUM(Sheet2!A1:A5)", 17), None);
        // The local reference alongside one is still replaceable.
        assert_eq!(ref_token_at("=Sheet2!A1+B2", 13), Some(11..13));
        // The SHEET half of a cell-shaped, unquoted name is not a reference
        // either: a pick there would turn `=Q1!B2` into `=D7!B2`, naming a sheet
        // that doesn't exist.
        assert_eq!(ref_token_at("=Q1!B2", 3), None, "caret just after Q1");
        assert_eq!(ref_token_at("=Q1!B2", 2), None, "caret inside Q1");
        assert_eq!(ref_token_at("=Q1!B2", 6), None, "the cell half, as above");
        assert_eq!(ref_token_at("=Q1!B2+C3", 9), Some(7..9), "the local one");
        // The same name QUOTED. `formula_ref_tokens` steps over `'…'`, so this
        // has to as well or a pick with the caret inside the quotes rewrites
        // `='Q1'!A1` to `='D7'!A1` — a sheet that doesn't exist.
        assert_eq!(ref_token_at("='Q1'!A1", 4), None, "caret after the name");
        assert_eq!(ref_token_at("='Q1'!A1", 3), None, "caret inside it");
        assert_eq!(ref_token_at("='Q1'!A1", 8), None, "the cell half");
        assert_eq!(ref_token_at("='Bob''s data'!A1", 5), None);
        assert_eq!(
            ref_token_at("='Q1'!A1+C3", 11),
            Some(9..11),
            "the local one"
        );
        // A string literal's contents are text, not cells — `formula_ref_tokens`
        // skips them too, and a pick inside one would edit the string.
        assert_eq!(ref_token_at("=\"A1 here\"", 4), None);
        assert_eq!(
            ref_token_at("=\"A1\"&C3", 8),
            Some(6..8),
            "outside it again"
        );
        // A structured reference's table name is not a cell.
        assert_eq!(ref_token_at("=SUM(T1[Amount])", 7), None);
    }

    #[test]
    fn the_two_reference_scanners_agree_on_the_same_buffer() {
        // `formula_ref_tokens` colours the text and the grid; `ref_token_at`
        // decides what a pick replaces. They are separate scans of the same
        // four rules (function name, `Sheet!`-qualified, quoted name, string),
        // so drift between them shows up as a formula edited where nothing was
        // outlined. Every span one reports, the other must claim from its end.
        for buf in [
            "=B2*C2",
            "=SUM(D2:D5)+B3",
            "=LOG10(A1)",
            "=Sheet2!A1+B2",
            "=Q1!B2+C3",
            "='Q1'!A1+C3",
            "=\"A1\"&C3",
            "='Bob''s data'!A1+B2",
            "=SUM(T1[Amount])+B2",
        ] {
            for (span, _) in formula_ref_tokens(buf) {
                let caret = buf[..span.end].chars().count();
                assert_eq!(
                    ref_token_at(buf, caret),
                    Some(span.clone()),
                    "{buf:?} at {caret}"
                );
            }
        }
        // And where it reports nothing, a pick inserts rather than replaces.
        for buf in ["='Q1'!A1", "=\"A1 is here\"", "=Sheet2!A1", "=Q1!A1"] {
            assert!(formula_ref_tokens(buf).is_empty(), "{buf:?}");
            for caret in 0..=buf.chars().count() {
                assert_eq!(ref_token_at(buf, caret), None, "{buf:?} at {caret}");
            }
        }
    }

    /// The workbook the chart-reference tests resolve against: the sheet on
    /// screen is index 1, so reading index 0 or 2 can only have come from a
    /// qualifier rather than from a fallback to the active sheet.
    fn book() -> Vec<String> {
        vec!["Budget".into(), "Sheet2".into(), "My Sheet".into()]
    }

    #[test]
    fn chart_ref_of_bounds_what_a_chart_will_read() {
        let b = book();
        // A range a chart can plot comes back parsed, on the sheet in front of
        // you when it named none.
        assert_eq!(chart_ref_of("B2:B5", "B2:B5", &b, 1), Ok((1, (1, 1, 4, 1))));
        assert_eq!(chart_ref_of(" c3 ", "B2:B5", &b, 1), Ok((1, (2, 2, 2, 2))));
        // Anything that isn't a range says so, quoting the field's own example.
        assert_eq!(
            chart_ref_of("total", "A2:A5", &b, 1),
            Err("\"total\" isn't a range like A2:A5".to_string())
        );
        // A whole column parses fine and would allocate a million cells — the
        // point of the cap is that the field declines instead of hanging.
        let err = chart_ref_of("A1:A1048576", "B2:B5", &b, 1).unwrap_err();
        assert!(err.contains("1048576 cells"), "got {err:?}");
        assert!(err.contains("at most 4096"), "got {err:?}");
        // The cap is inclusive at the boundary.
        assert!(chart_ref_of("A1:A4096", "B2:B5", &b, 1).is_ok());
        assert!(chart_ref_of("A1:A4097", "B2:B5", &b, 1).is_err());
    }

    #[test]
    fn chart_ref_of_reads_the_sheet_the_reference_names() {
        let b = book();
        // The bug this whole syntax exists to remove: a qualifier naming
        // another sheet is HONOURED, so these cells come off Budget rather than
        // off Sheet2, which is the one on screen.
        assert_eq!(
            chart_ref_of("=Budget!$A$1:$D$5", "=Sheet1!$A$1:$D$5", &b, 1),
            Ok((0, (0, 0, 4, 3)))
        );
        // Excel matches sheet names case-insensitively, and so does this.
        assert_eq!(
            chart_ref_of("budget!A1:D5", "=Sheet1!$A$1:$D$5", &b, 1),
            Ok((0, (0, 0, 4, 3)))
        );
        // A name needing quotes resolves through the same path.
        assert_eq!(
            chart_ref_of("='My Sheet'!$B$2:$B$5", "=Sheet1!$B$2:$B$5", &b, 1),
            Ok((2, (1, 1, 4, 1)))
        );
        // Naming the sheet you are already looking at is just the active one.
        assert_eq!(
            chart_ref_of("=Sheet2!$B$2:$B$5", "=Sheet1!$B$2:$B$5", &b, 1),
            Ok((1, (1, 1, 4, 1)))
        );
        // Every field's own seed text — `ref_a1` over the active sheet — must
        // commit unchanged, or re-pressing Enter on an untouched field would
        // report an error.
        let seed = super::ref_a1(Some("Sheet2"), (1, 1, 4, 1));
        assert_eq!(
            chart_ref_of(&seed, "=Sheet1!$B$2:$B$5", &b, 1),
            Ok((1, (1, 1, 4, 1)))
        );
    }

    #[test]
    fn chart_ref_of_refuses_a_sheet_the_workbook_hasnt_got() {
        let b = book();
        // Named but absent: refused by name, never redirected to the active
        // sheet's cells of the same address.
        assert_eq!(
            chart_ref_of("=Ledger!$A$1:$D$5", "=Sheet1!$A$1:$D$5", &b, 1),
            Err("there's no sheet called \"Ledger\"".to_string())
        );
        // A foreign sheet does not buy a way past the cell cap: the range is
        // weighed first, so this reports the size rather than the sheet.
        let err = chart_ref_of("=Budget!$A$1:$A$100000", "=Sheet1!$B$2:$B$5", &b, 1).unwrap_err();
        assert!(err.contains("at most 4096"), "got {err:?}");
        // Both wrong: the cap still speaks first, and the range is still the
        // thing to fix.
        let err = chart_ref_of("=Ledger!$A$1:$A$100000", "=Sheet1!$B$2:$B$5", &b, 1).unwrap_err();
        assert!(err.contains("at most 4096"), "got {err:?}");
        // A qualifier over text that isn't cells is still not a range.
        assert_eq!(
            chart_ref_of("Budget!total", "A2:A5", &b, 1),
            Err("\"Budget!total\" isn't a range like A2:A5".to_string())
        );
    }

    #[test]
    fn fill_box_extends_along_whichever_axis_was_pulled_furthest() {
        let src = (1, 1, 2, 2); // B2:C3
        // Pulled down three rows: rows grow, columns don't.
        assert_eq!(fill_box(src, (5, 2)), (1, 1, 5, 2));
        // Pulled right three columns: columns grow, rows don't.
        assert_eq!(fill_box(src, (2, 5)), (1, 1, 2, 5));
        // Diagonal: the longer pull wins; an equal pull goes down (`dr >= dc`).
        assert_eq!(fill_box(src, (6, 4)), (1, 1, 6, 2));
        assert_eq!(fill_box(src, (4, 6)), (1, 1, 2, 6));
        assert_eq!(fill_box(src, (4, 4)), (1, 1, 4, 2));
        // Back onto the source, or up/left off it, is the source itself — which
        // is how `sheet_fill_end` knows there is nothing to fill.
        assert_eq!(fill_box(src, (2, 2)), src);
        assert_eq!(fill_box(src, (0, 0)), src);
        assert_eq!(fill_box(src, (1, 0)), src);
    }

    #[test]
    fn resize_axis_splits_a_grip_drag_into_offset_and_growth() {
        // The far edge (+1) grows in place: no offset, size follows the drag.
        assert_eq!(resize_axis(1, 40.0, 200.0, 100.0), (0.0, 40.0));
        assert_eq!(resize_axis(1, -40.0, 200.0, 100.0), (0.0, -40.0));
        // The near edge (-1) moves the card as it shrinks it.
        assert_eq!(resize_axis(-1, 40.0, 200.0, 100.0), (40.0, -40.0));
        // Neither edge can shrink past the minimum.
        assert_eq!(resize_axis(1, -500.0, 200.0, 100.0), (0.0, -100.0));
        assert_eq!(resize_axis(-1, 500.0, 200.0, 100.0), (100.0, -100.0));
        // A grip that isn't on this axis leaves it alone.
        assert_eq!(resize_axis(0, 40.0, 200.0, 100.0), (0.0, 0.0));
    }

    #[test]
    fn shift_col_and_row_land_on_the_nearest_boundary() {
        // Default sheet: uniform columns and rows, so the arithmetic is checkable.
        let sh = gridcore::sheet::Sheet::default();
        let cw = col_px(sh.col_width(0));
        let rh = row_height_px(sh.row_height(0), super::SHEET_ROW_H) + 1.0;
        // Less than half a cell rounds back; more than half rounds on.
        assert_eq!(shift_col(&sh, 4, cw * 0.4), 4);
        assert_eq!(shift_col(&sh, 4, cw * 0.6), 5);
        assert_eq!(shift_col(&sh, 4, cw * 2.6), 7);
        // The same rule going backwards.
        assert_eq!(shift_col(&sh, 4, -cw * 0.4), 4);
        assert_eq!(shift_col(&sh, 4, -cw * 0.6), 3);
        assert_eq!(shift_col(&sh, 4, -cw * 2.6), 1);
        // Column 0 is the wall — a drag off the left edge stops there.
        assert_eq!(shift_col(&sh, 1, -cw * 50.0), 0);
        // Rows behave the same, over their own heights.
        assert_eq!(shift_row(&sh, 10, rh * 0.6), 11);
        assert_eq!(shift_row(&sh, 10, -rh * 3.6), 6);
        assert_eq!(shift_row(&sh, 2, -rh * 50.0), 0);
        // No movement is no movement.
        assert_eq!(shift_col(&sh, 7, 0.0), 7);
        assert_eq!(shift_row(&sh, 7, 0.0), 7);
    }

    #[test]
    fn replace_ref_writes_over_the_reference_or_inserts() {
        // Inserting after an open bracket leaves the rest alone.
        assert_eq!(
            replace_ref("=SUM(", 5, "A2:A5"),
            ("=SUM(A2:A5".to_string(), 10)
        );
        // Standing on a reference replaces exactly it.
        assert_eq!(replace_ref("=B2*C2", 3, "D9"), ("=D9*C2".to_string(), 3));
        assert_eq!(
            replace_ref("=SUM(D2:D5)", 9, "B2:B4"),
            ("=SUM(B2:B4)".to_string(), 10)
        );
        // The caret always lands after what was written, ready for the next
        // character.
        let (buf, caret) = replace_ref("=", 1, "A1");
        assert_eq!((buf.as_str(), caret), ("=A1", 3));
        // Multibyte text before the caret doesn't shift the splice.
        let (buf, caret) = replace_ref("=\"café\"&B2", 9, "C3");
        assert_eq!(buf, "=\"café\"&C3");
        // 10 CHARS, not the 11 bytes é costs — the caret is counted in chars.
        assert_eq!(caret, 10);
    }

    #[test]
    fn pointing_at_cells_while_typing_a_formula() {
        use super::{range_text, replace_ref};
        // The decision the grid makes on each move: take the buffer and caret
        // as they were when the press landed, and splice in the range the drag
        // has reached so far.
        let point = |buf: &str, caret: usize, anchor: (u32, u32), to: (u32, u32)| {
            let text = if anchor == to {
                gridcore::sheet::cell_name(to.0, to.1)
            } else {
                range_text(anchor, to)
            };
            replace_ref(buf, caret, &text)
        };
        // A click after "=SUM(" inserts one cell — A1, not A1:A1.
        assert_eq!(
            point("=SUM(", 5, (0, 0), (0, 0)),
            ("=SUM(A1".to_string(), 7)
        );
        // Dragging on rewrites that same reference rather than appending.
        assert_eq!(
            point("=SUM(", 5, (0, 0), (3, 0)),
            ("=SUM(A1:A4".to_string(), 10)
        );
        assert_eq!(
            point("=SUM(", 5, (0, 0), (3, 2)),
            ("=SUM(A1:C4".to_string(), 10)
        );
        // Standing on an existing reference replaces it, keeping the rest.
        assert_eq!(
            point("=B2*C2", 3, (8, 3), (8, 3)),
            ("=D9*C2".to_string(), 3)
        );
        // Dragging backwards names the same box.
        assert_eq!(
            point("=SUM(", 5, (3, 2), (0, 0)),
            ("=SUM(A1:C4".to_string(), 10)
        );
    }

    #[test]
    fn formula_ref_tokens_finds_every_reference_and_where_it_sits() {
        let ranges = |f: &str| {
            formula_ref_tokens(f)
                .into_iter()
                .map(|(_, r)| r)
                .collect::<Vec<_>>()
        };
        // Single cells and ranges, in the order they are written.
        assert_eq!(ranges("=B2*C2"), vec![(1, 1, 1, 1), (1, 2, 1, 2)]);
        assert_eq!(ranges("=SUM(D2:D5)"), vec![(1, 3, 4, 3)]);
        assert_eq!(
            ranges("=B2*C2+SUM(D2:D5)"),
            vec![(1, 1, 1, 1), (1, 2, 1, 2), (1, 3, 4, 3)]
        );
        // The span is where the text can be coloured.
        assert_eq!(formula_ref_tokens("=SUM(D2:D5)")[0].0, 5..10);
        assert_eq!(formula_ref_tokens("=B2*C2")[1].0, 4..6);
        // Function names are not references, even when they read like cells.
        assert_eq!(ranges("=LOG10(A1)"), vec![(0, 0, 0, 0)], "only A1 counts");
        assert_eq!(ranges("=SUM(A1)"), vec![(0, 0, 0, 0)]);
        // Nor is a table name in a structured reference, which is cell-shaped
        // whenever it is short: `T1` would otherwise outline cell T1.
        assert!(ranges("=SUM(T1[Amount])").is_empty());
        assert_eq!(ranges("=SUM(T1[Amount])+B2"), vec![(1, 1, 1, 1)]);
        // Another sheet's cells can't be outlined here, so they're skipped.
        assert!(ranges("=Sheet2!A1").is_empty());
        assert_eq!(
            ranges("=Sheet2!A1+B2"),
            vec![(1, 1, 1, 1)],
            "the local one still counts"
        );
        // Nor is anything inside a string.
        assert!(ranges("=\"A1 is here\"").is_empty());
        assert_eq!(ranges("=\"A1\"&C3"), vec![(2, 2, 2, 2)]);
        // A QUOTED sheet name is a sheet, never a cell: `'Q1'!A1` names a cell
        // on the sheet Q1. Read as a cell it would outline Q1 here, steal the
        // first reference colour, and a pick with the caret after the name would
        // rewrite the sheet's name to a cell reference.
        assert!(ranges("='Q1'!A1").is_empty());
        assert_eq!(ranges("='Q1'!A1+B2"), vec![(1, 1, 1, 1)]);
        assert_eq!(ranges("='Bob''s data'!A1+B2"), vec![(1, 1, 1, 1)]);
        assert!(ranges("='H1").is_empty(), "half-typed, still no cell");
        // UNquoted and cell-shaped is the same sheet, and it is the form
        // `translate_formula` writes: `sheet_prefix` quotes only names with
        // non-identifier characters, so a fill or a row insert turns `'Q1'!B2`
        // into `Q1!B2`. Both halves have to come out the same as above.
        assert!(ranges("=Q1!A1").is_empty());
        assert_eq!(ranges("=Q1!B2+C3"), vec![(2, 2, 2, 2)]);
        assert_eq!(ranges("=FY1!A1+D4"), vec![(3, 3, 3, 3)]);
        // Half-typed formulas are the normal case mid-edit.
        assert!(ranges("=SUM(").is_empty());
        assert!(ranges("=1+2").is_empty());
    }

    #[test]
    fn pointing_maps_rows_past_hidden_ones() {
        use super::{row_at_index, row_index_of};
        // Rows 2 and 3 hidden: the list holds 0,1,4,5,… so a press on the third
        // rendered row is sheet row 4, not row 2.
        let hidden = |r: u32| r == 2 || r == 3;
        assert_eq!(row_at_index(hidden, 0, 0), Some(0));
        assert_eq!(row_at_index(hidden, 0, 1), Some(1));
        assert_eq!(row_at_index(hidden, 0, 2), Some(4));
        assert_eq!(row_at_index(hidden, 0, 3), Some(5));
        // Frozen rows render above the list, so the list starts past them.
        assert_eq!(row_at_index(hidden, 2, 0), Some(4));
        // Nothing hidden: the index is the row.
        assert_eq!(row_at_index(|_| false, 0, 7), Some(7));

        // The inverse agrees with it, so a pick and a scroll-to name the same row.
        for ix in 0..4 {
            let row = row_at_index(hidden, 0, ix).unwrap();
            assert_eq!(row_index_of(hidden, 0, row), ix, "row {row} at index {ix}");
        }
        assert_eq!(row_index_of(hidden, 2, 4), 0);
        // A hidden row has no index of its own; it reports the next one's place,
        // which is where a reveal-scroll should land.
        assert_eq!(row_index_of(hidden, 0, 2), 2);
        assert_eq!(row_index_of(hidden, 0, 3), 2);
    }

    #[test]
    fn a_formula_that_never_parses_still_edits() {
        // Nothing here is a reference, and none of it parses; the scan and the
        // run split must still return something sane rather than panic or
        // colour the wrong text, or ordinary typing would break.
        for broken in ["=SUM(((", "=+*/", "=)(", "=A", "=:", "=SUM(D2:", "="] {
            let n = broken.chars().count();
            let runs = edit_runs(broken, n);
            assert_eq!(
                runs.iter().map(|(_, s, _)| s.as_str()).collect::<String>(),
                broken,
                "runs must rebuild {broken:?}"
            );
            for c in 0..=n {
                // The invariant holds at EVERY caret, not just the end: a run
                // split that dropped or duplicated text would still rebuild the
                // buffer at one position by luck.
                assert_eq!(
                    edit_runs(broken, c)
                        .iter()
                        .map(|(_, s, _)| s.as_str())
                        .collect::<String>(),
                    broken,
                    "runs must rebuild {broken:?} at caret {c}"
                );
                // A pick writes its cell in and keeps everything it didn't
                // stand on, wherever the caret is.
                let (out, caret) = replace_ref(broken, c, "B2");
                assert!(out.contains("B2"), "{broken:?} at {c} lost the pick");
                let span = ref_token_at(broken, c)
                    .unwrap_or_else(|| char_to_byte(broken, c)..char_to_byte(broken, c));
                assert_eq!(
                    out,
                    format!("{}B2{}", &broken[..span.start], &broken[span.end..]),
                    "{broken:?} at {c}"
                );
                assert!(caret <= out.chars().count(), "{broken:?} at {c}");
            }
        }
        // A half-typed range colours nothing, since it isn't a range yet.
        assert!(formula_ref_tokens("=SUM(D2:").is_empty());
        // But pointing into it still works: the reference under the caret is
        // replaced whole, not appended to.
        assert_eq!(
            replace_ref("=SUM(D2:", 8, "D2:D5"),
            ("=SUM(D2:D5".to_string(), 10)
        );
    }

    #[test]
    fn ref_color_is_stable_per_reference_and_wraps() {
        // The same index always draws the same colour, on the grid and in the
        // text; past the palette it wraps rather than running out.
        assert_eq!(ref_color(0), ref_color(0));
        assert_ne!(ref_color(0), ref_color(1));
        assert_eq!(ref_color(0), ref_color(6));
        assert_eq!(ref_color(2), ref_color(8));
        // Telling references apart IS the feature, so every colour in the
        // palette has to differ from every other — a duplicated constant would
        // draw two references identically and pass everything above.
        let palette: Vec<u32> = (0..6).map(ref_color).collect();
        for (i, a) in palette.iter().enumerate() {
            for (j, b) in palette.iter().enumerate() {
                assert!(i == j || a != b, "colours {i} and {j} are both {a:#08X}");
            }
        }
    }

    #[test]
    fn edit_runs_splits_at_the_caret_and_at_every_reference() {
        // (text, colour index) — the offsets are checked separately below.
        let runs = |b: &str, c: usize| {
            edit_runs(b, c)
                .into_iter()
                .map(|(_, s, i)| (s, i))
                .collect::<Vec<_>>()
        };
        let plain = |s: &str| (s.to_string(), None);
        let refd = |s: &str, i: usize| (s.to_string(), Some(i));

        // Each reference is its own run, in the numbering the grid outlines use.
        assert_eq!(
            runs("=B2*C2", 6),
            vec![plain("="), refd("B2", 0), plain("*"), refd("C2", 1)]
        );
        assert_eq!(
            runs("=B2*C2+SUM(D2:D5)", 17),
            vec![
                plain("="),
                refd("B2", 0),
                plain("*"),
                refd("C2", 1),
                plain("+SUM("),
                refd("D2:D5", 2),
                plain(")")
            ]
        );

        // The caret cuts a run in two so the bar can sit between the halves —
        // both halves keep the colour of the reference they came from.
        assert_eq!(
            runs("=B2*C2", 2),
            vec![
                plain("="),
                refd("B", 0),
                refd("2", 0),
                plain("*"),
                refd("C2", 1)
            ]
        );
        // A caret already on a boundary adds no cut.
        assert_eq!(
            runs("=B2*C2", 3),
            vec![plain("="), refd("B2", 0), plain("*"), refd("C2", 1)]
        );

        // Not a formula: one run per side of the caret, uncoloured. `A1` here
        // is text, not a reference.
        assert_eq!(runs("A1 note", 3), vec![plain("A1 "), plain("note")]);
        assert_eq!(runs("", 0), Vec::new());

        // Offsets are char indices into the whole buffer, so a click in any run
        // lands on the right caret position even past multibyte text.
        assert_eq!(
            edit_runs("=\"é\"&C3", 7)
                .iter()
                .map(|(o, _, _)| *o)
                .collect::<Vec<_>>(),
            vec![0, 5]
        );
        assert_eq!(runs("=\"é\"&C3", 7), vec![plain("=\"é\"&"), refd("C3", 0)]);
        // Half-typed input colours nothing and still splits at the caret.
        assert_eq!(runs("=SUM(", 5), vec![plain("=SUM(")]);
        assert_eq!(runs("=SUM(", 2), vec![plain("=S"), plain("UM(")]);
    }

    #[test]
    fn series_remove_keeps_the_last_one() {
        use gridcore::sheet::ChartSeries;
        let named = |n: &str| ChartSeries {
            name: n.into(),
            ..Default::default()
        };
        let mut list = vec![named("Qty"), named("Price"), named("Total")];
        assert!(series_remove(&mut list, 1));
        assert_eq!(
            list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Qty", "Total"]
        );
        // Out of range does nothing.
        assert!(!series_remove(&mut list, 9));
        assert_eq!(list.len(), 2);
        // The last one stays: a chart with no series has nothing to draw.
        assert!(series_remove(&mut list, 0));
        assert!(!series_remove(&mut list, 0));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn series_move_reorders_and_clamps_at_the_ends() {
        use gridcore::sheet::ChartSeries;
        let named = |n: &str, colour: u32| ChartSeries {
            name: n.into(),
            color: Some(colour),
            ..Default::default()
        };
        let mut list = vec![named("Qty", 1), named("Price", 2), named("Total", 3)];
        assert_eq!(series_move(&mut list, 2, -1), Some(1));
        assert_eq!(
            list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Qty", "Total", "Price"]
        );
        // A colour belongs to its series, so it travels with it.
        assert_eq!(list[1].color, Some(3));
        // Already at an end: nothing to do, reported as None.
        assert_eq!(series_move(&mut list, 0, -1), None);
        assert_eq!(series_move(&mut list, 2, 1), None);
        assert_eq!(series_move(&mut list, 9, 1), None);
        assert_eq!(
            list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Qty", "Total", "Price"]
        );
        // Further than one place: the ones it passes close up behind it. A swap
        // would give ["Price", "Total", "Qty"] here — the arrows only ever send
        // ±1, where the two agree, so nothing else would notice.
        let mut four = vec![named("A", 1), named("B", 2), named("C", 3), named("D", 4)];
        assert_eq!(series_move(&mut four, 0, 3), Some(3));
        assert_eq!(
            four.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["B", "C", "D", "A"]
        );
        // And back again, clamped past the end.
        assert_eq!(series_move(&mut four, 3, -9), Some(0));
        assert_eq!(
            four.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C", "D"]
        );
    }

    #[test]
    fn range_a1_round_trips_through_parse_ref_text() {
        // What a field shows for a range is what it parses back to.
        for range in [(0, 0, 4, 3), (1, 1, 1, 1), (9, 25, 20, 27)] {
            assert_eq!(
                parse_ref_text(&range_a1(range)),
                Some(RefText { sheet: None, range })
            );
        }
        assert_eq!(range_a1((1, 1, 4, 1)), "B2:B5");
    }

    #[test]
    fn ref_a1_round_trips_through_parse_ref_text() {
        // Whatever a field shows, the field can be committed unchanged and mean
        // the same cells on the same sheet — including names that need quoting.
        for sheet in [
            None,
            Some("Sheet1"),
            Some("Budget"),
            Some("My Sheet"),
            Some("Bob's Data"),
            Some("2024"),
            // The split is on the LAST `!` precisely so a quoted name may hold
            // one; nothing else in the round trip may notice.
            Some("Odd!Name"),
        ] {
            for range in [(0, 0, 4, 3), (1, 1, 1, 1), (9, 25, 20, 27)] {
                assert_eq!(
                    parse_ref_text(&ref_a1(sheet, range)),
                    Some(RefText {
                        sheet: sheet.map(str::to_string),
                        range,
                    }),
                    "{sheet:?} {range:?}"
                );
            }
        }
    }

    #[test]
    fn ref_a1_writes_the_form_excel_shows() {
        // Qualified and anchored, with the leading = a field carries.
        assert_eq!(ref_a1(Some("Budget"), (0, 0, 4, 3)), "=Budget!$A$1:$D$5");
        // A one-cell range still names both corners.
        assert_eq!(ref_a1(Some("Budget"), (2, 2, 2, 2)), "=Budget!$C$3:$C$3");
        // No sheet means no `!` at all — a bare `!$A$1` isn't a reference.
        assert_eq!(ref_a1(None, (0, 0, 4, 3)), "=$A$1:$D$5");
        // A name needing no quotes gets none; one needing them gets the
        // writer's quoting, apostrophes doubled.
        assert_eq!(ref_a1(Some("Sheet1"), (1, 1, 4, 1)), "=Sheet1!$B$2:$B$5");
        assert_eq!(
            ref_a1(Some("My Sheet"), (1, 1, 4, 1)),
            "='My Sheet'!$B$2:$B$5"
        );
        assert_eq!(
            ref_a1(Some("Bob's Data"), (0, 0, 0, 0)),
            "='Bob''s Data'!$A$1:$A$1"
        );
        // The same cells as the `<c:f>` the writer saves, bar the `=`.
        let src = gridcore::sheet::ChartSource {
            sheet: "My Sheet".into(),
            range: (1, 1, 4, 1),
            cat_col: 1,
        };
        assert_eq!(
            ref_a1(Some("My Sheet"), src.range),
            format!("={}", src.to_ref())
        );
    }

    #[test]
    fn range_edit_selection_tracks_the_anchor() {
        use super::{RangeEdit, RefTarget};
        let edit = |caret: usize, anchor: usize| RangeEdit {
            target: RefTarget::ChartRange,
            buf: "A1:D5".to_string(),
            caret,
            anchor,
            dragging: false,
        };
        // Collapsed caret selects nothing, in either order.
        assert_eq!(edit(2, 2).selection(), None);
        // Ordered whichever way the drag went.
        assert_eq!(edit(4, 1).selection(), Some((1, 4)));
        assert_eq!(edit(1, 4).selection(), Some((1, 4)));
        // Out-of-range indices clamp to the buffer rather than panicking.
        assert_eq!(edit(99, 0).selection(), Some((0, 5)));

        // set_caret extends from the anchor or collapses onto the caret.
        let mut e = edit(0, 0);
        e.set_caret(3, false);
        assert_eq!((e.caret, e.anchor), (3, 3));
        e.set_caret(5, true);
        assert_eq!(e.selection(), Some((3, 5)));
        e.set_caret(99, false);
        assert_eq!(
            (e.caret, e.anchor),
            (5, 5),
            "the caret clamps to the buffer length"
        );
    }

    #[test]
    fn range_edit_delete_selection_removes_exactly_the_selected_chars() {
        use super::{RangeEdit, RefTarget};
        let mut e = RangeEdit {
            target: RefTarget::ChartTitle,
            buf: "café latte".to_string(),
            caret: 5,
            anchor: 0,
            dragging: false,
        };
        assert!(e.delete_selection(), "a live selection is deleted");
        // Multibyte-safe: "café " is 5 chars but 6 bytes.
        assert_eq!(e.buf, "latte");
        assert_eq!((e.caret, e.anchor), (0, 0));
        // With nothing selected it reports so and leaves the buffer alone.
        assert!(!e.delete_selection());
        assert_eq!(e.buf, "latte");
    }

    #[test]
    fn range_text_normalises_a_drag_in_any_direction() {
        // A1 -> D5 and the same drag backwards both name A1:D5.
        assert_eq!(range_text((0, 0), (4, 3)), "A1:D5");
        assert_eq!(range_text((4, 3), (0, 0)), "A1:D5");
        // Mixed directions still order both axes.
        assert_eq!(range_text((4, 0), (0, 3)), "A1:D5");
        // A single cell picks itself.
        assert_eq!(range_text((2, 2), (2, 2)), "C3:C3");
    }

    /// A field's text and the drag that fills it have to name the same cells on
    /// the same sheet, or the reference would seem to change when the mouse came
    /// up and the field was re-rendered from the model.
    #[test]
    fn a_drag_into_a_field_writes_the_form_the_field_keeps() {
        // The same rectangles `range_text` reports, qualified.
        assert_eq!(ref_pick_text("Sheet1", (1, 1), (4, 1)), "=Sheet1!$B$2:$B$5");
        assert_eq!(ref_pick_text("Sheet1", (4, 1), (1, 1)), "=Sheet1!$B$2:$B$5");
        assert_eq!(ref_pick_text("Sheet1", (0, 0), (4, 3)), "=Sheet1!$A$1:$D$5");
        // Mixed directions still order both axes; one cell picks itself.
        assert_eq!(ref_pick_text("Sheet1", (4, 0), (0, 3)), "=Sheet1!$A$1:$D$5");
        assert_eq!(ref_pick_text("Sheet1", (2, 2), (2, 2)), "=Sheet1!$C$3:$C$3");
        // And it parses back to exactly the sheet and cells picked.
        for (anchor, to) in [((1, 1), (4, 1)), ((4, 0), (0, 3)), ((2, 2), (2, 2))] {
            let text = ref_pick_text("My Sheet", anchor, to);
            assert_eq!(
                parse_ref_text(&text),
                Some(RefText {
                    sheet: Some("My Sheet".to_string()),
                    range: super::sel_range(to, anchor),
                }),
                "{text:?}"
            );
        }
        // A drag into a CELL keeps the bare form — see `range_a1`.
        assert_eq!(range_text((1, 1), (4, 1)), "B2:B5");
    }

    /// A pick REPLACES the field's text rather than editing it, so whatever
    /// qualifier was in there — including another sheet's — gives way to the
    /// sheet the cells were actually dragged out of.
    #[test]
    fn a_pick_names_the_sheet_it_was_picked_from() {
        // The field held Budget's cells; the drag happened on Sheet1.
        let before = "=Budget!$A$1:$D$5";
        assert_eq!(
            parse_ref_text(before).and_then(|r| r.sheet),
            Some("Budget".to_string())
        );
        let after = ref_pick_text("Sheet1", (1, 1), (4, 1));
        assert_eq!(after, "=Sheet1!$B$2:$B$5");
        assert_eq!(
            parse_ref_text(&after),
            Some(RefText {
                sheet: Some("Sheet1".to_string()),
                range: (1, 1, 4, 1),
            })
        );
        // And the wash follows it back onto the sheet you can see.
        let wb = names(&["Sheet1", "Budget"]);
        assert_eq!(preview_range(before, &wb, 0), None);
        assert_eq!(preview_range(&after, &wb, 0), Some((1, 1, 4, 1)));
    }

    /// The wash may only cover cells the reference really reads. A ref naming
    /// another sheet washes nothing here, however well its A1 half parses.
    #[test]
    fn the_wash_only_covers_the_sheet_in_front_of_you() {
        let wb = names(&["Sheet1", "Budget", "My Sheet", "Bob's Data"]);
        // No qualifier means this sheet, whichever it is.
        assert_eq!(preview_range("A1:D5", &wb, 0), Some((0, 0, 4, 3)));
        assert_eq!(preview_range("=$A$1:$D$5", &wb, 1), Some((0, 0, 4, 3)));
        // Naming this sheet is the same thing, in any case and quoted or not.
        assert_eq!(
            preview_range("=Sheet1!$A$1:$D$5", &wb, 0),
            Some((0, 0, 4, 3))
        );
        assert_eq!(preview_range("sheet1!a1:d5", &wb, 0), Some((0, 0, 4, 3)));
        assert_eq!(
            preview_range("='My Sheet'!$C$3:$C$3", &wb, 2),
            Some((2, 2, 2, 2))
        );
        // Naming another sheet washes nothing — the whole point.
        assert_eq!(preview_range("=Budget!$A$1:$D$5", &wb, 0), None);
        assert_eq!(preview_range("Budget!A1:D5", &wb, 0), None);
        assert_eq!(preview_range("='Bob''s Data'!$A$1", &wb, 0), None);
        // Even a sheet the workbook hasn't got: it isn't THIS one either.
        assert_eq!(preview_range("=Nowhere!$A$1:$B$2", &wb, 0), None);
        // And text that isn't a range at all never washes.
        assert_eq!(preview_range("total", &wb, 0), None);
        assert_eq!(preview_range("", &wb, 0), None);
        assert_eq!(preview_range("=Sheet1!", &wb, 0), None);
    }

    /// The wash must answer for the sheet a COMMIT will act on, and a commit
    /// resolves the qualifier through `sheet_index_of` — first case-insensitive
    /// match wins. Excel forbids two sheets differing only in case; this code
    /// tolerates them, so the two lookups have to agree on which one is meant.
    #[test]
    fn the_wash_resolves_a_qualifier_the_way_a_commit_does() {
        let wb = names(&["Budget", "budget"]);
        // Sheet 1 is active and the ref says `budget` — but `budget` resolves
        // to sheet 0, so those are not the cells in front of you.
        assert_eq!(preview_range("=budget!$A$1", &wb, 1), None);
        assert_eq!(sheet_index_of(&wb, Some("budget"), 1), Ok(0));
        // From sheet 0 the same ref does wash, because that is where it reads.
        assert_eq!(preview_range("=budget!$A$1", &wb, 0), Some((0, 0, 0, 0)));
        assert_eq!(preview_range("=BUDGET!$A$1", &wb, 0), Some((0, 0, 0, 0)));
    }

    #[test]
    fn a_field_shows_the_sheet_its_source_names() {
        let src = |sheet: &str| gridcore::sheet::ChartSource {
            sheet: sheet.to_string(),
            range: (1, 1, 4, 1),
            cat_col: 1,
        };
        // The chart's own sheet, another sheet, and one needing quotes.
        assert_eq!(source_ref_text(&src("Sheet1")), "=Sheet1!$B$2:$B$5");
        assert_eq!(source_ref_text(&src("Budget")), "=Budget!$B$2:$B$5");
        assert_eq!(source_ref_text(&src("My Sheet")), "='My Sheet'!$B$2:$B$5");
        // A source naming no sheet shows no qualifier rather than a bare `!`,
        // which is not a reference either app would read.
        assert_eq!(source_ref_text(&src("")), "=$B$2:$B$5");
        // Whatever it shows, the field can read back.
        for name in ["Sheet1", "Budget", "My Sheet", "Bob's Data"] {
            assert_eq!(
                parse_ref_text(&source_ref_text(&src(name))),
                Some(RefText {
                    sheet: Some(name.to_string()),
                    range: (1, 1, 4, 1)
                })
            );
        }
    }

    #[test]
    fn a_series_name_field_shows_its_reference_or_the_literal_name() {
        // Named from a header cell: the field holds the reference, the way
        // Excel's Series name box does.
        assert_eq!(
            series_name_shown("Q1", Some("Budget!$B$1")),
            "=Budget!$B$1:$B$1"
        );
        assert_eq!(
            series_name_shown("Q1", Some("'My Sheet'!$B$1")),
            "='My Sheet'!$B$1:$B$1"
        );
        // Typed in by hand: there is no reference to show, so the name stands.
        assert_eq!(series_name_shown("Q1", None), "Q1");
        assert_eq!(series_name_shown("Total sales", None), "Total sales");
        // A ref the writer can't parse is not silently blanked — the name it
        // resolved to is still what the series is called.
        assert_eq!(series_name_shown("Q1", Some("total")), "Q1");
        // Which is exactly what `series_apply_name` compares against, so a name
        // shown as a reference and committed untouched changes nothing.
        let shown = series_name_shown("Q1", Some("Budget!$B$1"));
        assert!(matches!(
            super::series_name_commit(&shown, &shown),
            super::NameCommit::Unchanged
        ));
        // And THIS is what pins the coupling: the panel has to render
        // `series_name_shown`, not the bare `sr.name`. Were it to drift back,
        // an untouched ref-backed field would hand `series_name_commit` the
        // literal `Q1` against the shown reference — they differ, `Q1` parses
        // as a cell, and the series would be rebound to cell Q1's usually-empty
        // contents. That is the exact failure `series_name_commit`'s doc
        // comment exists to warn about, so assert across the two sides rather
        // than comparing one value with itself, which no implementation of
        // `series_name_shown` could ever fail.
        assert!(
            matches!(
                super::series_name_commit("Q1", &shown),
                super::NameCommit::Ref(_)
            ),
            "the literal name held against its own shown reference must read as \
             a reference — nothing else catches the panel rendering `sr.name`"
        );
    }

    #[test]
    fn bar_target_routes_each_action_to_its_field() {
        use super::{RefTarget, SheetAct, bar_target};
        assert_eq!(
            bar_target(SheetAct::CondFormat),
            Some(RefTarget::CondFormat)
        );
        assert_eq!(
            bar_target(SheetAct::DataValidation),
            Some(RefTarget::Validation)
        );
        assert_eq!(bar_target(SheetAct::CustomSort), Some(RefTarget::Sort));
        assert_eq!(
            bar_target(SheetAct::TextToColumns),
            Some(RefTarget::TextToColumns)
        );
        // The bars without a range field — and everything else — get none.
        assert_eq!(bar_target(SheetAct::Filter), None);
        assert_eq!(bar_target(SheetAct::RowHeight), None);
        assert_eq!(bar_target(SheetAct::Bold), None);
    }

    #[test]
    fn every_ribbon_command_that_touches_cells_takes_the_selection() {
        use super::{SheetAct, act_targets_cells};
        // The plain writers, and the destructive one the rule exists for.
        for act in [
            SheetAct::Copy,
            SheetAct::Cut,
            SheetAct::Paste,
            SheetAct::Bold,
            SheetAct::DeleteRow,
            SheetAct::DeleteCol,
            SheetAct::Merge,
            SheetAct::AutoSum,
            SheetAct::SortAsc,
            SheetAct::RemoveDuplicates,
            SheetAct::Subtotal,
        ] {
            assert!(act_targets_cells(act));
        }
        // The ones that read the selection without looking like it: the freeze
        // splits AT the selected cell, the comment steps MOVE the selection,
        // the pickers paint it, and a new chart plots it.
        for act in [
            SheetAct::FreezePanes,
            SheetAct::NextComment,
            SheetAct::PrevComment,
            SheetAct::FillColor,
            SheetAct::FontColor,
            SheetAct::InsertChart("bar"),
            SheetAct::InsertPivot,
        ] {
            assert!(act_targets_cells(act));
        }
        // Every command that opens a bar seeded from the selection is one, or
        // the bar would open on cells nothing on screen marked.
        for act in [
            SheetAct::CondFormat,
            SheetAct::DataValidation,
            SheetAct::CustomSort,
            SheetAct::TextToColumns,
        ] {
            assert!(super::bar_target(act).is_some() && act_targets_cells(act));
        }
        // The three exceptions: two whole-sheet properties and the inert stub.
        assert!(!act_targets_cells(SheetAct::ProtectSheet));
        assert!(!act_targets_cells(SheetAct::Outline));
        assert!(!act_targets_cells(SheetAct::Todo));
    }

    #[test]
    fn bar_fields_are_ranges_and_take_the_keyboard_first() {
        use super::RefTarget;
        for t in [
            RefTarget::CondFormat,
            RefTarget::Validation,
            RefTarget::Sort,
            RefTarget::TextToColumns,
        ] {
            assert!(
                t.is_bar(),
                "{t:?} sits inside a bar, so it is asked before the bar's own buffer"
            );
            assert!(t.is_range(), "{t:?} points at cells");
        }
        // The Chart panel's fields are not in a bar and must not steal its keys.
        for t in [
            RefTarget::ChartRange,
            RefTarget::ChartTitle,
            RefTarget::Categories,
            RefTarget::SeriesValues(0),
            RefTarget::SeriesName(1),
        ] {
            assert!(!t.is_bar(), "{t:?} is a panel field");
        }
    }

    #[test]
    fn a_bar_seeds_its_field_from_the_selection() {
        use super::{ref_a1, sel_range};
        // What `bar_range_field` builds: the selection, qualified with the sheet
        // it was made on, so the field reads like the one in Excel.
        let seed = |sel, anchor| ref_a1(Some("Sheet1"), sel_range(sel, anchor));
        // A block selection, dragged from either corner.
        assert_eq!(seed((1, 1), (4, 3)), "=Sheet1!$B$2:$D$5");
        assert_eq!(seed((4, 3), (1, 1)), "=Sheet1!$B$2:$D$5");
        assert_eq!(seed((4, 1), (1, 3)), "=Sheet1!$B$2:$D$5");
        // One cell seeds itself, and still parses back.
        assert_eq!(seed((0, 0), (0, 0)), "=Sheet1!$A$1:$A$1");
        assert_eq!(
            parse_ref_text(&seed((0, 0), (0, 0))),
            Some(RefText {
                sheet: Some("Sheet1".to_string()),
                range: (0, 0, 0, 0)
            })
        );
        // And the bar accepts its own seed rather than reading the `=` as a
        // sheet name — the regression the qualifier introduces.
        assert_eq!(
            super::bar_range_text(&seed((1, 1), (4, 3)), "Sheet1"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        // A name needing quotes survives the round trip too.
        let quoted = ref_a1(Some("Bob's Data"), (1, 1, 4, 3));
        assert_eq!(quoted, "='Bob''s Data'!$B$2:$D$5");
        assert_eq!(
            super::bar_range_text(&quoted, "Bob's Data"),
            Ok(quoted.clone())
        );
    }

    #[test]
    fn bar_range_text_normalises_or_explains_itself() {
        let bar_range_text = |t: &str| super::bar_range_text(t, "Sheet1");
        // What is typed comes back in the form the field will hold: the way
        // Excel writes it, qualified with the sheet the bar acts on.
        assert_eq!(bar_range_text("b2:d5"), Ok("=Sheet1!$B$2:$D$5".to_string()));
        assert_eq!(
            bar_range_text("  B2:D5 "),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        // Anchors are accepted and ignored; a backwards range is ordered.
        assert_eq!(
            bar_range_text("$D$5:$B$2"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        // This sheet's own name is kept, however it is spelled.
        assert_eq!(
            bar_range_text("Sheet1!B2:D5"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        assert_eq!(
            bar_range_text("sheet1!B2:D5"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        assert_eq!(
            bar_range_text("'Sheet1'!B2:D5"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        // The field's own `=` is not a sheet name.
        assert_eq!(
            bar_range_text("=Sheet1!$B$2:$D$5"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        assert_eq!(
            bar_range_text("=B2:D5"),
            Ok("=Sheet1!$B$2:$D$5".to_string())
        );
        // A single cell is a range of one.
        assert_eq!(bar_range_text("C3"), Ok("=Sheet1!$C$3:$C$3".to_string()));
        // Anything else is reported under the field, quoting what was typed and
        // showing the shape the field wants.
        assert_eq!(
            bar_range_text(" hello "),
            Err("\"hello\" isn't a range like =Sheet1!$A$1:$D$5".to_string())
        );
        assert!(bar_range_text("").is_err());
        assert!(bar_range_text("A1:").is_err());
    }

    #[test]
    fn a_bar_refuses_another_sheets_cells() {
        // A rule, a split or a sort acts on the sheet in view. Dropping the
        // prefix (which a chart resolves instead) would apply it to THIS sheet's
        // B2:D5 and say "Applies to B2:D5" — right-looking, wrong cells.
        assert_eq!(
            super::bar_range_text("Sheet2!B2:D5", "Sheet1"),
            Err("\"Sheet2\" is another sheet; this acts on Sheet1".to_string())
        );
        // Quoting doesn't get round it, doubled apostrophe and all.
        assert!(super::bar_range_text("'Bob''s data'!A1:A9", "Sheet1").is_err());
        assert_eq!(
            super::bar_range_text("'Bob''s data'!A1:A9", "Bob's data"),
            Ok("='Bob''s data'!$A$1:$A$9".to_string())
        );
    }

    #[test]
    fn each_range_target_says_whether_it_reads_another_sheet() {
        use super::{RefTarget::*, target_takes_foreign_sheet as takes};
        // A chart plots cells it doesn't float over, so every chart field
        // resolves a qualifier.
        assert!(takes(ChartRange));
        assert!(takes(SeriesValues(0)));
        assert!(takes(SeriesName(2)));
        assert!(takes(Categories));
        // A dropdown is read where the boxes are, which is commonly not the
        // sheet the list was typed on.
        assert!(takes(Validation));
        // A rule, a sort and a split act on the rows in front of you.
        assert!(!takes(CondFormat));
        assert!(!takes(Sort));
        assert!(!takes(TextToColumns));
        // Not a range at all — the question doesn't apply.
        assert!(!takes(ChartTitle));
    }

    #[test]
    fn validation_takes_another_sheets_cells() {
        use super::{RefTarget, bar_ref_text};
        let wb: Vec<String> = ["Sheet1", "Lookup", "My Sheet"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let dv = |t: &str| bar_ref_text(t, RefTarget::Validation, &wb, 0);
        // The sheet named is the sheet acted on, and the field answers with the
        // index the rule will be written to.
        assert_eq!(dv("Lookup!A1:A9"), Ok((1, "=Lookup!$A$1:$A$9".to_string())));
        // However it was spelled, the answer spells it the workbook's way.
        assert_eq!(dv("lookup!a1:a9"), Ok((1, "=Lookup!$A$1:$A$9".to_string())));
        assert_eq!(
            dv("='LOOKUP'!$A$1:$A$9"),
            Ok((1, "=Lookup!$A$1:$A$9".to_string()))
        );
        // A name needing quotes keeps them, and still names its own sheet.
        assert_eq!(
            dv("'My Sheet'!C3"),
            Ok((2, "='My Sheet'!$C$3:$C$3".to_string()))
        );
        // No qualifier is still the sheet in front of you.
        assert_eq!(dv("B2:D5"), Ok((0, "=Sheet1!$B$2:$D$5".to_string())));
        // And "in front of you" follows the active sheet, not the first one.
        assert_eq!(
            bar_ref_text("B2:D5", RefTarget::Validation, &wb, 2),
            Ok((2, "='My Sheet'!$B$2:$D$5".to_string()))
        );
    }

    #[test]
    fn validation_refuses_a_sheet_the_workbook_hasnt_got() {
        use super::{RefTarget, bar_ref_text};
        let wb: Vec<String> = ["Sheet1", "Lookup"].iter().map(|s| s.to_string()).collect();
        // Resolving is not the same as accepting anything: a name matching no
        // tab is refused by name rather than quietly applied to this sheet.
        assert_eq!(
            bar_ref_text("Budget!A1:A9", RefTarget::Validation, &wb, 0),
            Err("there's no sheet called \"Budget\"".to_string())
        );
        // What isn't a range at all reads the same as it does on the other
        // bars, with the field's own `=` off the front of the complaint.
        assert_eq!(
            bar_ref_text(" hello ", RefTarget::Validation, &wb, 0),
            Err("\"hello\" isn't a range like =Sheet1!$A$1:$D$5".to_string())
        );
        assert_eq!(
            bar_ref_text("=Lookup!", RefTarget::Validation, &wb, 0),
            Err("\"Lookup!\" isn't a range like =Sheet1!$A$1:$D$5".to_string())
        );
        assert!(bar_ref_text("", RefTarget::Validation, &wb, 0).is_err());
    }

    #[test]
    fn a_rule_a_sort_and_a_split_still_refuse_another_sheet() {
        use super::{RefTarget, bar_ref_text};
        let wb: Vec<String> = ["Sheet1", "Lookup"].iter().map(|s| s.to_string()).collect();
        for target in [
            RefTarget::CondFormat,
            RefTarget::Sort,
            RefTarget::TextToColumns,
        ] {
            // Refused even though the sheet EXISTS — the objection is that
            // these act on the rows in view, not that the name is unknown.
            assert_eq!(
                bar_ref_text("Lookup!A1:A9", target, &wb, 0),
                Err("\"Lookup\" is another sheet; this acts on Sheet1".to_string()),
                "{target:?} took a foreign sheet"
            );
            // A range on the sheet in front of you is what they want, and the
            // index they answer with is always that sheet.
            assert_eq!(
                bar_ref_text("B2:D5", target, &wb, 0),
                Ok((0, "=Sheet1!$B$2:$D$5".to_string()))
            );
            assert_eq!(
                bar_ref_text("A1:A9", target, &wb, 1),
                Ok((1, "=Lookup!$A$1:$A$9".to_string()))
            );
        }
    }

    #[test]
    fn no_bar_refuses_its_own_seeded_value() {
        use super::{RefTarget, bar_ref_text, ref_a1};
        // Every bar's field SEEDS itself qualified with the active sheet, so
        // the regression to watch is a bar objecting to text it wrote: pressing
        // Enter on an untouched field must pin exactly what it shows.
        let wb: Vec<String> = ["Sheet1", "Lookup", "Bob's Data"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for target in [
            RefTarget::CondFormat,
            RefTarget::Validation,
            RefTarget::Sort,
            RefTarget::TextToColumns,
        ] {
            for active in 0..wb.len() {
                let seed = ref_a1(Some(wb[active].as_str()), (1, 1, 4, 3));
                assert_eq!(
                    bar_ref_text(&seed, target, &wb, active),
                    Ok((active, seed.clone())),
                    "{target:?} refused its own seed {seed}"
                );
            }
        }
    }

    #[test]
    fn sort_uses_the_field_only_when_it_names_rows() {
        use super::sort_rows_from;
        let region = Some((1, 8));
        // An explicit multi-row range sorts exactly those rows.
        assert_eq!(sort_rows_from(Some((3, 0, 6, 2)), region), Some((3, 6)));
        // One row, or one cell, says nothing useful — keep the found region.
        assert_eq!(sort_rows_from(Some((3, 0, 3, 2)), region), region);
        assert_eq!(sort_rows_from(None, region), region);
        // With no region either, there is nothing to sort.
        assert_eq!(sort_rows_from(Some((3, 0, 3, 2)), None), None);
        assert_eq!(sort_rows_from(None, None), None);
        // A field range still wins when no region was found.
        assert_eq!(sort_rows_from(Some((0, 0, 4, 1)), None), Some((0, 4)));
    }

    #[test]
    fn row_height_px_maps_points_and_defaults() {
        assert_eq!(row_height_px(None, 21.0), 21.0); // default
        assert_eq!(row_height_px(Some(15.0), 21.0), 21.0); // 15pt == the base
        assert_eq!(row_height_px(Some(30.0), 21.0), 42.0); // double height
    }

    // ---- the pointed range's dashed border ------------------------------

    /// The mask a cell would draw, spelled out as `"trbl"` so a test reads like
    /// the picture it describes.
    fn mask(m: EdgeMask) -> String {
        let f = |on: bool, ch: char| if on { ch } else { '.' };
        [
            f(m.top, 't'),
            f(m.right, 'r'),
            f(m.bottom, 'b'),
            f(m.left, 'l'),
        ]
        .iter()
        .collect()
    }

    fn masks(plan: &RangeBorderPlan) -> Vec<(u32, u32, String)> {
        plan.cells
            .iter()
            .map(|&(r, c, m)| (r, c, mask(m)))
            .collect()
    }

    /// A one-cell range is its own border: that single cell draws all four
    /// sides. Its neighbours, and the cells inside a bigger range, draw none.
    #[test]
    fn range_edges_at_gives_a_cell_the_sides_it_owns() {
        assert_eq!(mask(range_edges_at((3, 3, 3, 3), 3, 3)), "trbl");
        assert!(range_edges_at((3, 3, 3, 3), 3, 4).is_empty()); // just outside
        assert!(range_edges_at((3, 3, 3, 3), 2, 3).is_empty());

        // A 3×3 range: corners own two sides, edges one, the middle none.
        let rg = (1, 1, 3, 3);
        assert_eq!(mask(range_edges_at(rg, 1, 1)), "t..l"); // top-left
        assert_eq!(mask(range_edges_at(rg, 1, 3)), "tr.."); // top-right
        assert_eq!(mask(range_edges_at(rg, 3, 3)), ".rb."); // bottom-right
        assert_eq!(mask(range_edges_at(rg, 3, 1)), "..bl"); // bottom-left
        assert_eq!(mask(range_edges_at(rg, 1, 2)), "t..."); // top run
        assert_eq!(mask(range_edges_at(rg, 2, 1)), "...l"); // left run
        assert!(range_edges_at(rg, 2, 2).is_empty()); // the middle
    }

    /// A hidden first or last row has no cell on screen to draw that edge, so
    /// the range is snapped onto the rows the grid really walks — otherwise the
    /// rectangle is drawn with one side missing, over neighbours that a
    /// collapsed row has left sitting flush.
    #[test]
    fn snap_range_rows_pulls_a_range_onto_the_rows_that_are_drawn() {
        // Rows 2 and 5 are hidden (an autofilter, say); the grid renders 8.
        let vis: Vec<u32> = vec![0, 1, 3, 4, 6, 7];
        let snap = |rg| snap_range_rows(rg, &vis, 8);

        // Both ends hidden: the outline moves in to the first and last row that
        // is actually drawn.
        assert_eq!(snap((2, 1, 5, 3)), (3, 1, 4, 3));
        // One end hidden.
        assert_eq!(snap((2, 1, 4, 3)), (3, 1, 4, 3));
        assert_eq!(snap((1, 1, 5, 3)), (1, 1, 4, 3));
        // Nothing hidden at the ends: unchanged, columns untouched throughout.
        assert_eq!(snap((1, 1, 4, 3)), (1, 1, 4, 3));
        assert_eq!(snap((0, 0, 7, 9)), (0, 0, 7, 9));

        // A range that is ENTIRELY hidden comes back as it went in: no cell of
        // it is rendered either way, and a total function keeps a caller's
        // indices — which are what pick a reference's colour.
        assert_eq!(
            snap_range_rows((5, 1, 5, 3), &vec![0, 1, 3, 4, 6][..], 8),
            (5, 1, 5, 3)
        );
        assert_eq!(snap_range_rows((2, 1, 2, 3), &[][..], 8), (2, 1, 2, 3));

        // Past the END of the rendered grid is not hidden: `=SUM(A1:A800)` on a
        // 500-row grid keeps its `r1`, so the rectangle stays open where the
        // reference really carries on. Only the top is pulled in.
        assert_eq!(snap_range_rows((2, 1, 40, 3), &vis, 8), (3, 1, 40, 3));
        assert_eq!(snap_range_rows((0, 0, 8, 0), &vis, 8), (0, 0, 8, 0));
        // Row 7 is the last one drawn, and it owns no bottom edge.
        assert_eq!(mask(range_edges_at((3, 1, 40, 3), 7, 2)), "....");

        // Snapping can collapse a multi-row selection to one visible row, and
        // then `border_range` drops the border for the ring to carry alone —
        // which is what the user sees: one cell. `sheet_el` composes the two in
        // exactly this order, snap first, so the guard sees what gets drawn.
        let snapped = snap_range_rows((2, 1, 3, 1), &vec![3][..], 8);
        assert_eq!(snapped, (3, 1, 3, 1));
        assert_eq!(border_range(None, Some(snapped)), None);

        // And the edges then land on the rows that ARE drawn.
        let rg = snap((2, 1, 5, 3));
        assert_eq!(mask(range_edges_at(rg, 3, 1)), "t..l");
        assert_eq!(mask(range_edges_at(rg, 4, 3)), ".rb.");
    }

    /// One cell, every side, one entry — and the plan says dashed, because one
    /// cell is nowhere near the cap.
    #[test]
    fn border_plan_one_cell_range_owns_all_four_edges() {
        let plan = range_border_plan((5, 5, 5, 5), (0, 0, 20, 20));
        assert_eq!(masks(&plan), vec![(5, 5, "trbl".into())]);
        assert!(plan.dashed);
    }

    /// A single row: every cell carries top AND bottom, and the two ends add
    /// their left/right. Nothing is drawn twice — the corners merge.
    #[test]
    fn border_plan_single_row_merges_top_and_bottom_on_every_cell() {
        let plan = range_border_plan((2, 1, 2, 4), (0, 0, 10, 10));
        assert_eq!(
            masks(&plan),
            vec![
                (2, 1, "t.bl".into()),
                (2, 2, "t.b.".into()),
                (2, 3, "t.b.".into()),
                (2, 4, "trb.".into()),
            ]
        );
    }

    /// A single column is the same picture turned ninety degrees.
    #[test]
    fn border_plan_single_column_merges_left_and_right() {
        let plan = range_border_plan((1, 3, 4, 3), (0, 0, 10, 10));
        assert_eq!(
            masks(&plan),
            vec![
                (1, 3, "tr.l".into()),
                (2, 3, ".r.l".into()),
                (3, 3, ".r.l".into()),
                (4, 3, ".rbl".into()),
            ]
        );
    }

    /// Scrolled so the top and the left edge are off screen: those runs are not
    /// drawn at all, and the cells that remain are only the visible parts of the
    /// bottom and right. Cells off screen never appear — the row renderer never
    /// asks about them.
    #[test]
    fn border_plan_clips_to_the_visible_window() {
        // Range B2:F6 (rows 1..=5, cols 1..=5), viewport shows rows 3..=9 and
        // cols 3..=9: the top row and the left column are scrolled away.
        let plan = range_border_plan((1, 1, 5, 5), (3, 3, 9, 9));
        assert_eq!(
            masks(&plan),
            vec![
                (3, 5, ".r..".into()),
                (4, 5, ".r..".into()),
                (5, 3, "..b.".into()),
                (5, 4, "..b.".into()),
                (5, 5, ".rb.".into()),
            ]
        );
        assert!(plan.dashed);

        // Scrolled clean past it: nothing to draw, and no panic on the empty box.
        let gone = range_border_plan((1, 1, 5, 5), (40, 40, 60, 60));
        assert!(gone.cells.is_empty());
        // Nothing is drawn, so there is no cost to fall back from: solid is
        // an answer about an expensive border, and this one is free.
        assert!(gone.dashed);

        // Off in one axis only is still off.
        assert!(
            range_border_plan((1, 1, 5, 5), (0, 40, 9, 60))
                .cells
                .is_empty()
        );
    }

    /// A range far wider than the cap still costs only its perimeter — but past
    /// `RANGE_BORDER_CELL_CAP` visible cells it drops to a solid border. The
    /// cells are the same either way; only `dashed` changes.
    #[test]
    fn border_plan_falls_back_to_solid_past_the_cap() {
        let wide = RANGE_BORDER_CELL_CAP as u32 + 10;

        // One row, `wide` columns, all visible: one cell per column, over cap.
        let plan = range_border_plan((0, 0, 0, wide - 1), (0, 0, 50, wide));
        assert_eq!(plan.cells.len(), wide as usize);
        assert!(!plan.dashed);

        // The same enormous range, scrolled so only a handful is on screen:
        // back under the cap, so back to dashes. Cost follows the viewport.
        let narrow = range_border_plan((0, 0, 0, wide - 1), (0, 0, 50, 9));
        assert_eq!(narrow.cells.len(), 10);
        assert!(narrow.dashed);

        // Exactly at the cap is still dashed — the cap is inclusive.
        let at_cap = range_border_plan(
            (0, 0, 0, RANGE_BORDER_CELL_CAP as u32 - 1),
            (0, 0, 50, 100_000),
        );
        assert_eq!(at_cap.cells.len(), RANGE_BORDER_CELL_CAP);
        assert!(at_cap.dashed);
    }

    /// A whole-sheet selection is bounded by what is on screen, not by the
    /// 1,048,576 rows it names — this is the property the cap relies on.
    #[test]
    fn border_plan_cost_is_bounded_by_the_viewport() {
        // The densest realistic viewport: 28px columns and 21px rows on a
        // 1920×1200 grid ≈ 69 columns × 55 rows.
        let plan = range_border_plan((0, 0, 1_048_575, 16_383), (0, 0, 54, 68));
        // Only the visible top-left corner of the range is on screen, so only
        // its top and left runs are drawn: 55 + 69 − 1 shared corner.
        assert_eq!(plan.cells.len(), 55 + 69 - 1);
        assert!(plan.cells.len() < RANGE_BORDER_CELL_CAP);
        assert!(plan.dashed);
    }

    /// The dash arithmetic at the widths the app actually uses. The first dash
    /// starts flush at 0 and the last ends flush at the far end, which is what
    /// makes a run of cells read as one border.
    #[test]
    fn dash_fit_matches_the_shader_at_real_grid_widths() {
        // A default column: 8.43 char units → 65px.
        let w = col_px(gridcore::sheet::DEFAULT_COL_WIDTH);
        assert!((w - 65.01).abs() < 1e-3, "default column is {w}px");
        let f = dash_fit(w, RANGE_BORDER_W).expect("a default column dashes");
        assert_eq!(f.count, 11);
        assert_eq!(f.dash_px, 4.0); // 2 × border width
        assert_eq!(f.offsets[0], 0.0);
        assert!((f.offsets.last().unwrap() + f.dash_px - w).abs() < 1e-3);
        assert_eq!(f.offsets.len(), f.count as usize);
        // The pitch is stretched from the nominal 6px so the dashes divide evenly.
        assert!(f.pitch_px > 6.0 && f.pitch_px < 6.2, "pitch {}", f.pitch_px);

        // The narrowest column `col_px` allows still dashes: 28px sits exactly
        // on a dash-count boundary (4.0 periods), so which side of it f32
        // rounding lands on is not worth asserting — what matters is that it
        // dashes, at close to the nominal 6px pitch, and still ends flush.
        let n = dash_fit(28.0, RANGE_BORDER_W).expect("the narrowest column dashes");
        assert!((4..=5).contains(&n.count), "count {}", n.count);
        assert!(
            n.pitch_px >= 6.0 && n.pitch_px <= 8.1,
            "pitch {}",
            n.pitch_px
        );
        assert!((n.offsets.last().unwrap() + n.dash_px - 28.0).abs() < 1e-3);

        // A row's vertical edge: 21px.
        let v = dash_fit(SHEET_ROW_H, RANGE_BORDER_W).expect("a default row dashes");
        assert_eq!(v.count, 3);
        assert!((v.offsets.last().unwrap() + v.dash_px - SHEET_ROW_H).abs() < 1e-3);
    }

    /// Too short to dash: the shader paints those edges solid, so `dash_fit`
    /// says so rather than inventing a fit nobody will see.
    #[test]
    fn dash_fit_gives_up_on_an_edge_shorter_than_a_dash_pitch() {
        // Shorter than one 6px pitch.
        assert_eq!(dash_fit(5.0, RANGE_BORDER_W), None);
        // The threshold is 4 × border width: at it, solid; just past it, two
        // dashes with the gap squeezed.
        assert_eq!(dash_fit(8.0, RANGE_BORDER_W), None);
        let f = dash_fit(9.0, RANGE_BORDER_W).expect("just past the threshold");
        assert_eq!(f.count, 2);
        assert_eq!(f.offsets[0], 0.0);
        assert!((f.offsets[1] + f.dash_px - 9.0).abs() < 1e-3);

        // Degenerate inputs are solid too, not a panic or an empty dash list.
        assert_eq!(dash_fit(0.0, RANGE_BORDER_W), None);
        assert_eq!(dash_fit(-3.0, RANGE_BORDER_W), None);
        assert_eq!(dash_fit(65.0, 0.0), None);

        // The threshold scales with the width, as the shader's does.
        assert_eq!(dash_fit(16.0, 4.0), None); // 4 × 4px
        assert!(dash_fit(20.0, 4.0).is_some());
    }

    // ---- Task 2: what the renderer outlines, and whether it dashes ----

    /// `shown_sel` is the one gate the border goes through: `sheet_el` resolves
    /// the border's range once (`GridOverlay::border_rg`) and both users read
    /// that — the rows draw it, and the dash cap is costed against it — so the
    /// drawn border and the costed one cannot disagree, and neither call site
    /// has to remember `sel_hidden` for itself.
    #[test]
    fn shown_sel_withholds_the_selection_while_a_chart_owns_it() {
        let sel = (2, 2, 6, 6);
        let ov = GridOverlay::default();
        assert_eq!(shown_sel(&ov, sel), Some(sel));
        let hidden = GridOverlay {
            sel_hidden: true,
            ..ov
        };
        assert_eq!(shown_sel(&hidden, sel), None);
        // And so the selection borders nothing while the chart has it — but a
        // POINTED range still does, which is the one indicator left on.
        assert_eq!(border_range(None, shown_sel(&hidden, sel)), None);
        assert_eq!(
            border_range(Some((0, 0, 1, 1)), shown_sel(&hidden, sel)),
            Some((0, 0, 1, 1))
        );
    }

    /// A focused range field owns the border while it has the keyboard; without
    /// one the selection gets it, but only when it spans more than one cell —
    /// a lone cell already wears the active ring.
    /// Dashes say a field is POINTING, not that a range is big. Excel reserves
    /// marching ants for a copy or a dialog picker; a range swept with the
    /// mouse gets a solid box. Dashing both made a plain drag look like a
    /// formula was reading the cells.
    #[test]
    fn only_a_pointed_range_is_dashed() {
        use super::border_is_dashed as dashed;
        // A field is pointing and the range is small enough to draw: dashed.
        assert!(dashed(true, true));
        // The regression: an ordinary mouse selection must NOT dash, however
        // wide it is.
        assert!(
            !dashed(false, true),
            "a range swept with the mouse wears a solid border"
        );
        // Pointing, but past the boundary-cell cap — solid, because the border
        // is drawn cell by cell and the cap is what bounds that.
        assert!(!dashed(true, false));
        assert!(!dashed(false, false));
    }

    #[test]
    fn border_range_prefers_the_pointed_range_over_the_selection() {
        let sel = Some((2, 2, 6, 6));
        // A field is pointing: its range wins even though the selection is wide.
        assert_eq!(border_range(Some((0, 0, 1, 1)), sel), Some((0, 0, 1, 1)));
        // Nothing pointing: the selection, because it spans more than one cell.
        assert_eq!(border_range(None, sel), sel);
        // A one-cell selection draws no border — the ring is the indicator.
        assert_eq!(border_range(None, Some((3, 4, 3, 4))), None);
        // A one-cell POINTED range still does: nothing else marks it.
        assert_eq!(border_range(Some((3, 4, 3, 4)), sel), Some((3, 4, 3, 4)));
        // A single row and a single column both span more than one cell.
        assert_eq!(border_range(None, Some((3, 4, 3, 9))), Some((3, 4, 3, 9)));
        assert_eq!(border_range(None, Some((3, 4, 8, 4))), Some((3, 4, 8, 4)));
        // A chart owns the selection, so the grid is showing none of it: there
        // is no selection to outline, and the border goes with the ring.
        assert_eq!(border_range(None, None), None);
        // A POINTED range still outlines in that state — it is the selected
        // chart's own field pointing, and showing where is its whole job.
        assert_eq!(border_range(Some((0, 0, 1, 1)), None), Some((0, 0, 1, 1)));
    }

    /// One selection at a time, from the cells' side: a press on a cell takes
    /// the selection back from whatever chart was holding it, and the panel
    /// field that belonged to that chart goes with it.
    #[test]
    fn a_press_on_a_cell_takes_the_selection_back_from_a_chart() {
        let after = press_selection(SelectTarget::Cell, Some(3), false);
        assert_eq!(
            after,
            SelectionAfter {
                chart: None,
                cell_moves: true,
                drop_field: true,
            }
        );
        // The grid has the selection, so it draws it again.
        assert!(cell_selection_shown(after.chart));

        // Nothing was selected either: the cell still moves, and the field is
        // still dropped, because the grid has taken the keyboard regardless.
        let after = press_selection(SelectTarget::Cell, None, false);
        assert_eq!(
            after,
            SelectionAfter {
                chart: None,
                cell_moves: true,
                drop_field: true,
            }
        );
    }

    /// And from the chart's side: selecting a chart takes the selection away
    /// from the cells, so the ring cannot compete with the chart's handles.
    #[test]
    fn a_press_on_a_chart_takes_the_selection_from_the_cells() {
        let after = press_selection(SelectTarget::Chart(2), None, false);
        assert_eq!(
            after,
            SelectionAfter {
                chart: Some(2),
                // The cell selection does NOT move — it stays where it was and
                // stops being drawn, so dismissing the chart puts it back.
                cell_moves: false,
                drop_field: true,
            }
        );
        assert!(!cell_selection_shown(after.chart));

        // Swapping charts drops the field too: it is keyed by series position
        // within the chart being left, so it means something else on this one.
        let after = press_selection(SelectTarget::Chart(2), Some(5), false);
        assert_eq!(after.chart, Some(2)); // the pressed chart, not the held one
        assert!(after.drop_field);
    }

    /// Pressing the chart that is ALREADY selected is a no-op on the selection.
    /// It has to be: every press on a selected card starts a move drag, and
    /// dropping the field on each of them would close the panel entry you were
    /// halfway through typing.
    #[test]
    fn a_press_on_the_selected_chart_keeps_its_panel_field() {
        let after = press_selection(SelectTarget::Chart(4), Some(4), false);
        assert_eq!(
            after,
            SelectionAfter {
                chart: Some(4),
                cell_moves: false,
                drop_field: false,
            }
        );
        // A resize grip presses the same chart it grips, so a resize is this
        // case, not a cell selection.
        assert_eq!(
            press_selection(SelectTarget::Chart(4), Some(4), true),
            after
        );
    }

    /// Only the PANEL's own field survives a press on the card it belongs to.
    /// A bar field — Data Validation, Sort — belongs to the sheet, and
    /// `run_sheet_act` opens one while leaving the panel up, so the two really
    /// do coexist. Keeping it would hand the chart the selection and the bar
    /// the keyboard: two things selected, which is what the rule forbids.
    #[test]
    fn only_the_panels_own_field_survives_a_press_on_its_chart() {
        use super::{RefTarget, keeps_panel_field};

        // The panel's own fields, on the chart the panel shows: kept.
        for f in [
            RefTarget::ChartRange,
            RefTarget::ChartTitle,
            RefTarget::SeriesName(0),
            RefTarget::SeriesValues(2),
            RefTarget::Categories,
        ] {
            assert!(
                keeps_panel_field(Some(3), 3, Some(f)),
                "{f:?} is the panel's"
            );
        }
        // No field at all is still "nothing of the panel's to lose".
        assert!(keeps_panel_field(Some(3), 3, None));

        // A sheet bar pointing at the grid over an open panel: dropped.
        for f in [
            RefTarget::CondFormat,
            RefTarget::Validation,
            RefTarget::Sort,
            RefTarget::TextToColumns,
        ] {
            assert!(
                !keeps_panel_field(Some(3), 3, Some(f)),
                "{f:?} is the sheet's"
            );
        }

        // A different chart, or no panel: the field belongs to what is left.
        assert!(!keeps_panel_field(Some(3), 4, Some(RefTarget::ChartRange)));
        assert!(!keeps_panel_field(None, 3, Some(RefTarget::ChartRange)));
    }

    /// Point mode: while a range field or a half-typed formula has the
    /// keyboard, a click on the grid POINTS. Nothing is selected or deselected
    /// — the chart being edited must survive the clicks that edit it, which is
    /// the entire reason its range fields are pointable.
    #[test]
    fn a_click_while_pointing_points_instead_of_selecting() {
        for chart in [None, Some(0), Some(7)] {
            let after = press_selection(SelectTarget::Cell, chart, true);
            assert_eq!(
                after,
                SelectionAfter {
                    chart,
                    cell_moves: false,
                    drop_field: false,
                },
                "pointing must not disturb {chart:?}"
            );
        }
        // Pressing another CHART still swaps while pointing: the field belongs
        // to the chart being left, so it cannot survive the move.
        let after = press_selection(SelectTarget::Chart(1), Some(0), true);
        assert_eq!(after.chart, Some(1));
        assert!(after.drop_field);
    }

    /// A grid navigation key is aimed at the cells, so it takes the selection
    /// back the way a click does. Without that an arrow would move a selection
    /// hidden behind the chart that owns it.
    #[test]
    fn navigation_keys_take_the_selection_back_from_a_chart() {
        let after = press_selection(SelectTarget::NavKey, Some(1), false);
        assert_eq!(
            after,
            SelectionAfter {
                chart: None,
                cell_moves: true,
                drop_field: true,
            }
        );
        assert!(cell_selection_shown(after.chart));
        // While pointing, the same key is the FIELD's, not the grid's.
        let after = press_selection(SelectTarget::NavKey, Some(1), true);
        assert_eq!(after.chart, Some(1));
        assert!(!after.cell_moves);
    }

    /// The invariant the whole task exists for, over every combination: the
    /// cell selection is shown exactly when no chart is selected, and no
    /// outcome ever both selects a chart and moves the cells.
    #[test]
    fn exactly_one_thing_is_selected_after_any_press() {
        let targets = [
            SelectTarget::Cell,
            SelectTarget::NavKey,
            SelectTarget::Chart(0),
            SelectTarget::Chart(1),
        ];
        for target in targets {
            for chart in [None, Some(0), Some(1)] {
                for pointing in [false, true] {
                    let after = press_selection(target, chart, pointing);
                    assert_eq!(
                        cell_selection_shown(after.chart),
                        after.chart.is_none(),
                        "{target:?} {chart:?} {pointing}"
                    );
                    assert!(
                        !(after.chart.is_some() && after.cell_moves),
                        "{target:?} {chart:?} {pointing}: both selected"
                    );
                    // A press on a chart always ends with THAT chart selected,
                    // pointing or not — there is no state in which clicking a
                    // card fails to select it.
                    if let SelectTarget::Chart(i) = target {
                        assert_eq!(after.chart, Some(i));
                    }
                }
            }
        }
    }

    /// The cap is bounded by the VIEWPORT, not the range: the widths and row
    /// counts the app actually reaches all dash, and only a window nobody has
    /// falls back to solid.
    #[test]
    fn range_border_dashes_at_every_realistic_viewport() {
        // A 1920px grid at the narrowest column shows ~69 columns; the row bound
        // is `GRID_MAX_VISIBLE_ROWS`. Even select-all stays far under the cap.
        let cols = (0, 68);
        assert!(range_border_dashed(
            (0, 0, 0, 16_383),
            cols,
            GRID_MAX_VISIBLE_ROWS
        )); // a full row
        assert!(range_border_dashed(
            (0, 0, 1_048_575, 0),
            cols,
            GRID_MAX_VISIBLE_ROWS
        )); // a column
        assert!(range_border_dashed(
            (0, 0, 1_048_575, 16_383),
            cols,
            GRID_MAX_VISIBLE_ROWS
        )); // select-all
        assert!(range_border_dashed(
            (5, 5, 5, 5),
            cols,
            GRID_MAX_VISIBLE_ROWS
        )); // one cell

        // A range scrolled entirely right of the window plans no cells at all.
        // A border that costs nothing has no reason to fall back, so it still
        // reads as dashed — the flag says "cheap enough", and nothing is
        // cheaper than nothing. Scrolling it back in recomputes against the new
        // window and finds the same answer.
        assert!(range_border_dashed(
            (0, 900, 4, 910),
            cols,
            GRID_MAX_VISIBLE_ROWS
        ));
        assert!(
            range_border_plan((0, 900, 4, 910), (0, 0, 127, 68))
                .cells
                .is_empty()
        );
    }

    /// `range_border_dashed` reads its count off closed-form arithmetic rather
    /// than off the plan, so the render path allocates nothing to answer it.
    /// The two must not drift: the plan is the definition, this is the shortcut.
    #[test]
    fn the_closed_form_cell_count_matches_the_plan_it_replaces() {
        let cases = [
            ((0, 0, 0, 0), (0, 0, 9, 9)),                 // one cell
            ((2, 2, 2, 7), (0, 0, 9, 9)),                 // a single row
            ((2, 2, 7, 2), (0, 0, 9, 9)),                 // a single column
            ((1, 1, 5, 5), (0, 0, 9, 9)),                 // wholly inside
            ((1, 1, 5, 5), (3, 3, 9, 9)),                 // top-left corner clipped off
            ((1, 1, 5, 5), (0, 0, 3, 3)),                 // bottom-right clipped off
            ((1, 1, 5, 5), (2, 2, 4, 4)),                 // every edge clipped off
            ((0, 0, 1_048_575, 16_383), (7, 3, 134, 71)), // select-all, scrolled
            ((1, 1, 5, 5), (40, 40, 60, 60)),             // scrolled clean past it
            ((1, 1, 5, 5), (0, 40, 9, 60)),               // off in one axis only
        ];
        for (range, view) in cases {
            assert_eq!(
                range_border_cell_count(range, view),
                range_border_plan(range, view).cells.len() as u64,
                "range {range:?} in view {view:?}"
            );
        }
    }

    /// Past the cap the SAME edges are still planned — they just stop dashing.
    /// That is the guarantee: degrade to solid, never to a partial border.
    #[test]
    fn range_border_falls_back_to_solid_past_the_cap() {
        let wide = RANGE_BORDER_CELL_CAP as u32 + 10;
        let cols = (0, wide);
        assert!(!range_border_dashed((0, 0, 0, wide - 1), cols, 1));
        // The edges themselves are unchanged — a boundary cell still owns its
        // sides, so the border is drawn either way.
        let plan = range_border_plan((0, 0, 0, wide - 1), (0, 0, 0, wide));
        assert!(!plan.dashed);
        assert_eq!(plan.cells.len(), wide as usize);
        assert_eq!(mask(plan.cells[0].2), "t.bl");

        // Narrow the column window and the very same range dashes again: the
        // cost is the viewport's, not the range's.
        assert!(range_border_dashed((0, 0, 0, wide - 1), (0, 40), 1));
    }

    /// The row bound is what keeps a tall selection under the cap, and it is
    /// applied from the range's own first row.
    #[test]
    fn range_border_cap_counts_only_the_rows_a_viewport_can_show() {
        let cols = (0, 3);
        // 4 columns × a million rows: unbounded this is 2M cells, but only
        // `GRID_MAX_VISIBLE_ROWS` of them can ever be on screen.
        assert!(range_border_dashed(
            (0, 0, 1_048_575, 3),
            cols,
            GRID_MAX_VISIBLE_ROWS
        ));
        // Lift the bound past the cap and it gives up, as designed.
        assert!(!range_border_dashed(
            (0, 0, 1_048_575, 3),
            cols,
            RANGE_BORDER_CELL_CAP as u32
        ));
        // A degenerate bound of zero rows must not underflow.
        assert!(range_border_dashed((7, 0, 9, 3), cols, 0));
    }

    /// The edges a row renderer asks for, for the shapes Task 2 draws: the two
    /// ends of a single row own three sides each, and the middle owns one.
    #[test]
    fn border_edges_cover_the_shapes_the_renderer_draws() {
        let row = (4, 2, 4, 5);
        assert_eq!(mask(range_edges_at(row, 4, 2)), "t.bl");
        assert_eq!(mask(range_edges_at(row, 4, 3)), "t.b.");
        assert_eq!(mask(range_edges_at(row, 4, 5)), "trb.");
        // A cell the border does not reach draws nothing, so the renderer skips
        // the deferred element entirely.
        assert!(range_edges_at((1, 1, 5, 5), 3, 3).is_empty());
        assert!(range_edges_at(row, 5, 3).is_empty());
    }

    // ---- a selected chart's source areas --------------------------------

    fn chart_src(sheet: &str, range: (u32, u32, u32, u32)) -> gridcore::sheet::ChartSource {
        gridcore::sheet::ChartSource {
            sheet: sheet.into(),
            range,
            cat_col: range.1,
        }
    }

    fn chart_series(
        name_ref: Option<&str>,
        values_ref: Option<gridcore::sheet::ChartSource>,
    ) -> gridcore::sheet::ChartSeries {
        gridcore::sheet::ChartSeries {
            name_ref: name_ref.map(str::to_string),
            values_ref,
            ..Default::default()
        }
    }

    /// A two-series chart over `Data!A1:C5`: a header cell naming each series
    /// in row 1, labels down column A, numbers in B and C.
    fn data_chart() -> gridcore::sheet::ChartData {
        gridcore::sheet::ChartData {
            series: vec![
                chart_series(Some("Data!$B$1"), Some(chart_src("Data", (1, 1, 4, 1)))),
                chart_series(Some("Data!$C$1"), Some(chart_src("Data", (1, 2, 4, 2)))),
            ],
            categories_ref: Some(chart_src("Data", (1, 0, 4, 0))),
            source: Some(chart_src("Data", (0, 0, 4, 2))),
            ..Default::default()
        }
    }

    /// `(range, slot)` as a test reads it, so a failure names the slot rather
    /// than an enum buried in a tuple.
    fn areas(cd: &gridcore::sheet::ChartData, sheet: &str) -> Vec<((u32, u32, u32, u32), char)> {
        chart_source_areas(cd, sheet)
            .into_iter()
            .map(|a| {
                (
                    a.range,
                    match a.slot {
                        ChartSlot::Values => 'v',
                        ChartSlot::Categories => 'c',
                        ChartSlot::Name => 'n',
                    },
                )
            })
            .collect()
    }

    /// Every slot the chart holds a reference for becomes an area, in the
    /// model's own fold order: the numbers, then the labels, then the names.
    /// The chart's BOX is not among them — it is their union, and outlining it
    /// would say nothing about what any part of it does.
    #[test]
    fn chart_source_areas_lists_one_area_per_slot() {
        let listed = areas(&data_chart(), "Data");
        assert_eq!(
            listed,
            vec![
                ((1, 1, 4, 1), 'v'), // B2:B5
                ((1, 2, 4, 2), 'v'), // C2:C5
                ((1, 0, 4, 0), 'c'), // A2:A5
                ((0, 1, 0, 1), 'n'), // B1
                ((0, 2, 0, 2), 'n'), // C1
            ]
        );
        // The box `A1:C5` is not drawn, though the chart holds it.
        assert!(!listed.iter().any(|a| a.0 == (0, 0, 4, 2)));

        // A chart holding no references outlines nothing rather than falling
        // back to its box.
        let bare = gridcore::sheet::ChartData {
            source: Some(chart_src("Data", (0, 0, 4, 2))),
            series: vec![chart_series(None, None)],
            ..Default::default()
        };
        assert!(areas(&bare, "Data").is_empty());
        assert!(areas(&Default::default(), "Data").is_empty());
    }

    /// A scatter's and a bubble's numbers live in `point_refs`, not in
    /// `values_ref`. Reading only the latter would outline nothing at all for
    /// the one chart kind whose plot IS its refs.
    #[test]
    fn chart_source_areas_reads_a_scatters_points() {
        let mut s = chart_series(Some("Data!$B$1"), None);
        s.point_refs = vec![
            chart_src("Data", (1, 0, 3, 0)), // xVal A2:A4
            chart_src("Data", (1, 1, 3, 1)), // yVal B2:B4
        ];
        let cd = gridcore::sheet::ChartData {
            series: vec![s],
            ..Default::default()
        };
        assert_eq!(
            areas(&cd, "Data"),
            vec![
                ((1, 0, 3, 0), 'v'),
                ((1, 1, 3, 1), 'v'),
                ((0, 1, 0, 1), 'n'),
            ]
        );
    }

    /// The outlines answer "what does this chart read", and they are worth
    /// drawing only while that is the question being asked.
    #[test]
    fn chart_outlines_stand_down_while_a_range_is_being_pointed() {
        use super::chart_outlines_shown as shown;
        // Selected and idle: the whole point of the feature.
        assert!(shown(true, false));
        // Selected, but a range field has the keyboard. The chart is still
        // selected — that is what keeps the panel live — so gating on the
        // selection alone leaves three coloured boxes under the dashed preview
        // being dragged over them. This is the case that made the grid feel
        // busy while picking.
        assert!(
            !shown(true, true),
            "picking a range must stand the source outlines down"
        );
        // No chart, nothing to outline, pointing or not.
        assert!(!shown(false, false));
        assert!(!shown(false, true));
    }

    /// A reference naming another sheet draws NOTHING — not this sheet's cells
    /// of the same address, which is the lie `preview_range` refuses for the
    /// pointed range too. A reference naming NO sheet is the chart's own.
    #[test]
    fn chart_source_areas_draws_only_the_sheet_in_front_of_you() {
        let cd = data_chart();
        // Seen from another sheet, a chart reading `Data` outlines nothing.
        assert!(areas(&cd, "Sheet2").is_empty());
        // Excel matches sheet names case-insensitively, and so does this.
        assert_eq!(areas(&cd, "data").len(), 5);

        // Mixed: the numbers are here, the labels and the name on `Ref`. Only
        // the numbers are drawn.
        let mixed = gridcore::sheet::ChartData {
            series: vec![chart_series(
                Some("Ref!$B$1"),
                Some(chart_src("Data", (1, 1, 4, 1))),
            )],
            categories_ref: Some(chart_src("Ref", (1, 0, 4, 0))),
            ..Default::default()
        };
        assert_eq!(areas(&mixed, "Data"), vec![((1, 1, 4, 1), 'v')]);

        // An unqualified ref — a chart authored before its refs carried a
        // sheet — belongs to the sheet it floats over, which is this one.
        let unqualified = gridcore::sheet::ChartData {
            series: vec![chart_series(
                Some("$B$1"),
                Some(chart_src("", (1, 1, 4, 1))),
            )],
            ..Default::default()
        };
        assert_eq!(
            areas(&unqualified, "Data"),
            vec![((1, 1, 4, 1), 'v'), ((0, 1, 0, 1), 'n')]
        );

        // A `name_ref` that is not a reference at all (a typed name) is not an
        // area, and does not stop the slots after it being read.
        let typed = gridcore::sheet::ChartData {
            series: vec![chart_series(
                Some("not a ref"),
                Some(chart_src("Data", (1, 1, 4, 1))),
            )],
            ..Default::default()
        };
        assert_eq!(areas(&typed, "Data"), vec![((1, 1, 4, 1), 'v')]);
    }

    /// Two series pointed at one cell draw one box — not the same box twice
    /// for no visible difference.
    #[test]
    fn chart_source_areas_folds_a_duplicated_reference() {
        let cd = gridcore::sheet::ChartData {
            series: vec![
                chart_series(Some("Data!$B$1"), Some(chart_src("Data", (1, 1, 4, 1)))),
                chart_series(Some("Data!$B$1"), Some(chart_src("Data", (1, 1, 4, 1)))),
            ],
            ..Default::default()
        };
        assert_eq!(
            areas(&cd, "Data"),
            vec![((1, 1, 4, 1), 'v'), ((0, 1, 0, 1), 'n')]
        );
        // Same cells, different slot, is NOT a duplicate: both claims are real,
        // and the overlap rule below is what picks between them.
        let both = gridcore::sheet::ChartData {
            series: vec![chart_series(
                Some("Data!$B$1"),
                Some(chart_src("Data", (0, 1, 0, 1))),
            )],
            ..Default::default()
        };
        assert_eq!(
            areas(&both, "Data"),
            vec![((0, 1, 0, 1), 'v'), ((0, 1, 0, 1), 'n')]
        );
    }

    /// The overlap rule as paint order: every area covering a cell is drawn,
    /// largest first, so the SMALLEST one lands last and owns any edge they
    /// share — earliest index on a tie. Both halves matter. Without the rule a
    /// series' name cell would be swallowed by the values box it heads; without
    /// the loser being drawn at all, that values box would be left with no top
    /// edge, because its top row is the one cell the name won.
    #[test]
    fn chart_areas_at_paints_the_tightest_slot_last() {
        let cd = gridcore::sheet::ChartData {
            series: vec![chart_series(
                Some("Data!$B$1"),
                // The values ref covers its own header cell, as a chart
                // re-pointed at a whole column does.
                Some(chart_src("Data", (0, 1, 4, 1))),
            )],
            ..Default::default()
        };
        let a = chart_source_areas(&cd, "Data");
        // What the renderer draws, in the order it draws it.
        let drawn = |r, c| -> Vec<ChartSlot> {
            chart_areas_at(&a, r, c)
                .into_iter()
                .map(|i| a[i].slot)
                .collect()
        };
        // The winner is the LAST drawn, so it paints over the others.
        let slot = |r, c| drawn(r, c).last().copied();
        assert_eq!(slot(0, 1), Some(ChartSlot::Name)); // B1 → the one cell
        assert_eq!(slot(2, 1), Some(ChartSlot::Values)); // B3 → only the values
        assert_eq!(slot(0, 0), None); // A1 → neither
        assert_eq!(slot(5, 1), None); // B6 → below both

        // ...and the loser is still drawn, under it. B1 is the whole top row of
        // `B1:B5`, so dropping the values area here would leave the blue box
        // open at the top — the bug this ordering replaced a lookup to fix.
        assert_eq!(drawn(0, 1), vec![ChartSlot::Values, ChartSlot::Name]);
        assert_eq!(drawn(2, 1), vec![ChartSlot::Values]);
        assert!(drawn(0, 0).is_empty());

        // Earliest wins a tie of equal size: the values ref is folded first, so
        // a name cell that IS the whole values ref reads as values.
        let tie = gridcore::sheet::ChartData {
            series: vec![chart_series(
                Some("Data!$B$1"),
                Some(chart_src("Data", (0, 1, 0, 1))),
            )],
            ..Default::default()
        };
        let a = chart_source_areas(&tie, "Data");
        // Both are drawn — they are the same box twice — and the earliest is
        // last, so `Values` is what you see.
        assert_eq!(
            chart_areas_at(&a, 0, 1)
                .into_iter()
                .map(|i| a[i].slot)
                .collect::<Vec<_>>(),
            vec![ChartSlot::Name, ChartSlot::Values]
        );
        // Nothing to own a cell, and nothing to panic on.
        assert!(chart_areas_at(&[], 0, 0).is_empty());
    }

    /// The labels stay apart from the numbers, and the box owns nothing.
    #[test]
    fn chart_areas_at_keeps_the_labels_apart_from_the_numbers() {
        let a = chart_source_areas(&data_chart(), "Data");
        let slot = |r, c| chart_areas_at(&a, r, c).last().map(|&i| a[i].slot);
        assert_eq!(slot(1, 0), Some(ChartSlot::Categories)); // A2, a label
        assert_eq!(slot(1, 1), Some(ChartSlot::Values)); // B2, a number
        assert_eq!(slot(0, 1), Some(ChartSlot::Name)); // B1, a header
        assert_eq!(slot(0, 2), Some(ChartSlot::Name)); // C1, a header
        // A1 is inside the chart's BOX but in none of its slots, and the box is
        // not drawn — so nothing owns it.
        assert_eq!(slot(0, 0), None);
    }

    /// The three role colours are Excel's: distinct from each other, and from
    /// every colour `ref_color` hands a formula reference — a source outline
    /// must never be mistaken for "the Nth reference of what you are typing".
    #[test]
    fn chart_slot_colors_are_excels_and_not_the_ref_palette() {
        assert_eq!(chart_slot_color(ChartSlot::Values), CHART_VALUES_COLOR);
        assert_eq!(
            chart_slot_color(ChartSlot::Categories),
            CHART_CATEGORIES_COLOR
        );
        assert_eq!(chart_slot_color(ChartSlot::Name), CHART_NAME_COLOR);

        let roles = [CHART_VALUES_COLOR, CHART_CATEGORIES_COLOR, CHART_NAME_COLOR];
        for (i, a) in roles.iter().enumerate() {
            for b in &roles[i + 1..] {
                assert_ne!(a, b);
            }
            // `ref_color` cycles through six; none of them may collide.
            for j in 0..6 {
                assert_ne!(*a, ref_color(j), "role {a:#08x} collides with ref {j}");
            }
        }
    }

    /// An area becomes SIDES through `range_edges_at` — the same geometry the
    /// pointed range's border uses, so a one-cell name owns all four and a
    /// column of values owns three at each end and two down the middle.
    #[test]
    fn chart_source_areas_feed_the_same_edge_geometry_as_the_range_border() {
        let a = chart_source_areas(&data_chart(), "Data");
        let one = ChartSourceArea {
            range: (0, 1, 0, 1),
            slot: ChartSlot::Name,
        };
        assert!(a.contains(&one));
        assert_eq!(mask(range_edges_at(one.range, 0, 1)), "trbl"); // B1

        let vals = a[0].range; // B2:B5
        assert_eq!(mask(range_edges_at(vals, 1, 1)), "tr.l"); // B2, the top
        assert_eq!(mask(range_edges_at(vals, 2, 1)), ".r.l"); // B3, the middle
        assert_eq!(mask(range_edges_at(vals, 4, 1)), ".rbl"); // B5, the bottom
        assert!(range_edges_at(vals, 1, 2).is_empty()); // C2 is another slot's
    }
    /// The sticky rule itself: a chart losing the selection does NOT close the
    /// panel showing it. This is the whole of complaint 4 — the panel used to be
    /// gated on `chart_sel`, so any click on the grid shut it mid-edit.
    #[test]
    fn the_panel_stays_open_when_its_chart_is_deselected() {
        let shut = None;
        let open = chart_panel_after(shut, PanelEvent::Select(2));
        assert_eq!(open, Some(2), "pressing a card opens the panel on it");
        let after_click = chart_panel_after(open, PanelEvent::Deselect);
        assert_eq!(after_click, Some(2), "a click on a cell must not close it");
        // Repeated deselection (a sweep across cells, then an arrow key) is
        // idempotent — nothing about it counts down towards a close.
        let mut s = after_click;
        for _ in 0..5 {
            s = chart_panel_after(s, PanelEvent::Deselect);
        }
        assert_eq!(s, Some(2));
    }

    /// Selecting another chart swaps the panel rather than stacking; selecting
    /// the same one again is the same answer, so a press on a card is safe to
    /// fire unconditionally from `chart_press`.
    #[test]
    fn selecting_another_chart_swaps_the_panel() {
        let open = chart_panel_after(None, PanelEvent::Select(0));
        assert_eq!(chart_panel_after(open, PanelEvent::Select(3)), Some(3));
        assert_eq!(chart_panel_after(open, PanelEvent::Select(0)), Some(0));
        // And it reopens a panel that was dismissed, from either state.
        assert_eq!(chart_panel_after(None, PanelEvent::Select(1)), Some(1));
    }

    /// The two deliberate ways out — the panel's `×` and Escape — both fully
    /// dismiss, and a later deselection does not resurrect it.
    #[test]
    fn dismissing_closes_the_panel_for_good() {
        let open = chart_panel_after(None, PanelEvent::Select(1));
        let shut = chart_panel_after(open, PanelEvent::Dismiss);
        assert_eq!(shut, None);
        assert_eq!(chart_panel_after(shut, PanelEvent::Deselect), None);
        assert_eq!(chart_panel_after(shut, PanelEvent::Dismiss), None);
        // A press on a card is still the way back in.
        assert_eq!(chart_panel_after(shut, PanelEvent::Select(1)), Some(1));
    }

    /// A delete, a sheet switch, a tab switch or an undo all reach
    /// `chart_drop_selection`, and all mean the same thing to the panel: the
    /// index it holds names a different chart now, so it closes outright. This
    /// is the difference between sticky and stale.
    #[test]
    fn the_chart_list_changing_closes_the_panel() {
        let open = chart_panel_after(None, PanelEvent::Select(2));
        assert_eq!(chart_panel_after(open, PanelEvent::Invalidate), None);
        assert_eq!(chart_panel_after(None, PanelEvent::Invalidate), None);
    }

    /// Second line of defence for "it must not display a chart that no longer
    /// exists": whatever the events did, the index is checked against the sheet
    /// in front of you before anything is rendered.
    #[test]
    fn the_panel_never_shows_a_chart_that_is_gone() {
        assert_eq!(chart_panel_shown(Some(0), 3), Some(0));
        assert_eq!(chart_panel_shown(Some(2), 3), Some(2));
        // Deleted the last of three, panel was on it.
        assert_eq!(chart_panel_shown(Some(2), 2), None);
        // Switched to a sheet with no charts at all.
        assert_eq!(chart_panel_shown(Some(0), 0), None);
        // Shut stays shut however many charts there are.
        assert_eq!(chart_panel_shown(None, 5), None);
    }

    /// The flow the sticky rule exists for, start to finish: select a chart,
    /// click a cell mid-edit, point a field at the grid, then dismiss. The
    /// panel is open for every step in between and shut only at the end.
    #[test]
    fn a_range_edit_survives_a_click_on_the_grid() {
        let n = 2; // the sheet has two charts throughout
        let mut panel = None;
        let open = |p: Option<usize>| chart_panel_shown(p, n).is_some();

        panel = chart_panel_after(panel, PanelEvent::Select(1));
        assert!(open(panel));
        // A click on a cell while a range field has the keyboard POINTS: the
        // chart is not even deselected (`press_selection` leaves it alone), so
        // the panel is untouched.
        let after = press_selection(SelectTarget::Cell, Some(1), true);
        assert_eq!(after.chart, Some(1));
        assert!(!after.cell_moves && !after.drop_field);
        assert!(open(panel));
        // A click with NO field focused deselects — and the panel stays, which
        // is what lets the next click land in one of its fields.
        let after = press_selection(SelectTarget::Cell, Some(1), false);
        assert_eq!(after.chart, None);
        panel = chart_panel_after(panel, PanelEvent::Deselect);
        assert_eq!(chart_panel_shown(panel, n), Some(1), "still on that chart");
        // Now point that field from a deselected panel: still nothing moves.
        let after = press_selection(SelectTarget::Cell, None, true);
        assert_eq!(after.chart, None);
        assert!(!after.cell_moves && !after.drop_field);
        assert!(open(panel));
        // Escape (or the `×`) is the only thing that shuts it.
        panel = chart_panel_after(panel, PanelEvent::Dismiss);
        assert!(!open(panel));
    }

    /// Deleting the chart the panel shows closes it by both routes at once:
    /// `chart_delete_selected` fires `Invalidate` through
    /// `chart_drop_selection`, and the index would fail the bounds check even
    /// if it hadn't.
    #[test]
    fn deleting_the_shown_chart_closes_the_panel_either_way() {
        let panel = chart_panel_after(None, PanelEvent::Select(1)); // of two
        let by_event = chart_panel_after(panel, PanelEvent::Invalidate);
        assert_eq!(chart_panel_shown(by_event, 1), None);
        // Suppose the event were ever missed: one chart left, index 1 is gone.
        assert_eq!(chart_panel_shown(panel, 1), None);
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

#[cfg(test)]
mod config_root_tests {
    use super::{CONFIG_DIR_ENV, config_root, config_root_from, hot_dir, session_path};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// With no override, the app writes where the OS says config lives.
    #[test]
    fn no_override_uses_the_os_config_dir() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        assert_eq!(config_root_from(None, Some(os.clone())), os);
    }

    /// The override wins outright — that is the whole point of it existing.
    #[test]
    fn override_replaces_the_os_config_dir() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        let over = OsString::from(r"D:\runs\harness-42");
        assert_eq!(
            config_root_from(Some(over), Some(os)),
            PathBuf::from(r"D:\runs\harness-42")
        );
    }

    /// An exported-but-blank variable is a shell accident (`export VAR=`), not a
    /// request to scatter session.json into the working directory.
    #[test]
    fn empty_override_counts_as_unset() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        assert_eq!(
            config_root_from(Some(OsString::new()), Some(os.clone())),
            os
        );
    }

    /// A relative override is honoured verbatim, resolved against the working
    /// directory like any other relative path a caller types.
    #[test]
    fn relative_override_is_honoured_verbatim() {
        let root = config_root_from(
            Some(OsString::from("target/harness-cfg")),
            Some(PathBuf::from(r"C:\Users\someone\AppData\Roaming")),
        );
        assert_eq!(root, PathBuf::from("target/harness-cfg"));
        assert!(root.is_relative());
    }

    /// Both the OS lookup and the override can be absent (a headless/odd
    /// profile); fall back to the working directory rather than panicking.
    #[test]
    fn no_override_and_no_os_config_falls_back_to_cwd() {
        assert_eq!(config_root_from(None, None), PathBuf::from("."));
    }

    /// The isolation guarantee itself: BOTH persisted locations sit under
    /// whatever `config_root` returns, and both move when the override moves.
    /// Every env-touching assertion lives in this one test so it cannot race a
    /// sibling test running on another thread.
    #[test]
    fn session_and_hot_both_follow_the_override() {
        let saved = std::env::var_os(CONFIG_DIR_ENV);

        // Under an override: both land inside it, and neither is the real profile.
        let over = Path::new(r"D:\runs\harness-42");
        unsafe { std::env::set_var(CONFIG_DIR_ENV, over) };
        assert_eq!(config_root(), over);
        assert_eq!(session_path(), over.join("docxy").join("session.json"));
        assert_eq!(hot_dir(), over.join("docxy").join("hot"));
        assert!(session_path().starts_with(over));
        assert!(hot_dir().starts_with(over));

        // Unset again: back to the OS config dir, which is what a normal launch
        // must keep doing — the harness is opt-in, never a mode you fall into.
        unsafe { std::env::remove_var(CONFIG_DIR_ENV) };
        let real = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(config_root(), real);
        assert_eq!(session_path(), real.join("docxy").join("session.json"));
        assert_eq!(hot_dir(), real.join("docxy").join("hot"));
        assert!(!session_path().starts_with(over));
        assert!(!hot_dir().starts_with(over));

        if let Some(v) = saved {
            unsafe { std::env::set_var(CONFIG_DIR_ENV, v) };
        }
    }
}
