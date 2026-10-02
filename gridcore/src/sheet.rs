//! The workbook model: sheets, sparse cells, values, and the display-level
//! style subset (number formats, bold/italic/color).
//!
//! Coordinates are 0-based `(row, col)` everywhere in the model; A1 notation
//! is converted at the boundaries (parsing, display, formulas). Cells live in
//! a sparse `BTreeMap` so memory is proportional to content, and iteration is
//! naturally row-major (the order worksheet XML wants).

use std::collections::{BTreeMap, HashMap, HashSet};

// ---------------------------------------------------------------------------
// A1 reference math
// ---------------------------------------------------------------------------

/// Excel's hard grid limits (XLSX): rows 1..=1,048,576 and columns A..=XFD.
pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;

/// 0-based column index → letters: 0 → "A", 25 → "Z", 26 → "AA".
pub fn col_name(col: u32) -> String {
    let mut n = col + 1; // bijective base-26 works on 1-based
    let mut s = Vec::new();
    while n > 0 {
        let r = ((n - 1) % 26) as u8;
        s.push(b'A' + r);
        n = (n - 1) / 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// Parse leading column letters ("AB" → 27). Returns (0-based col, chars used);
/// `None` if `s` doesn't start with an ASCII letter or the column exceeds XFD.
pub fn parse_col(s: &str) -> Option<(u32, usize)> {
    let b = s.as_bytes();
    let mut n: u32 = 0;
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        n = n
            .checked_mul(26)?
            .checked_add((b[i].to_ascii_uppercase() - b'A') as u32 + 1)?;
        if n > MAX_COLS {
            return None;
        }
        i += 1;
    }
    if i == 0 { None } else { Some((n - 1, i)) }
}

/// 0-based (row, col) → "A1" notation.
pub fn cell_name(row: u32, col: u32) -> String {
    format!("{}{}", col_name(col), row + 1)
}

/// Parse an A1 cell reference ("B12", "$C$4") → 0-based (row, col).
/// `$` anchors are accepted and ignored; the whole string must be consumed.
pub fn parse_cell_name(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let (col, used) = parse_col(s)?;
    let rest = &s[used..];
    let rest = rest.strip_prefix('$').unwrap_or(rest);
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row: u32 = rest.parse().ok()?;
    if row == 0 || row > MAX_ROWS {
        return None;
    }
    Some((row - 1, col))
}

/// Parse "A1:C3" (or a single "B2") → 0-based (r1, c1, r2, c2), normalized so
/// r1 ≤ r2 and c1 ≤ c2.
pub fn parse_range_name(s: &str) -> Option<(u32, u32, u32, u32)> {
    match s.split_once(':') {
        Some((a, b)) => {
            let (r1, c1) = parse_cell_name(a)?;
            let (r2, c2) = parse_cell_name(b)?;
            Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
        }
        None => {
            let (r, c) = parse_cell_name(s)?;
            Some((r, c, r, c))
        }
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// A computed / stored cell value. For formula cells this is the *cached*
/// result (what Excel last computed, or what our engine recomputed).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CellValue {
    #[default]
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    /// An Excel error code, e.g. "#DIV/0!".
    Error(String),
}

impl CellValue {
    pub fn is_empty(&self) -> bool {
        matches!(self, CellValue::Empty)
    }
}

/// One cell: value, optional formula, and the style (xf) index from the file.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Cell {
    pub value: CellValue,
    /// Formula source *without* the leading `=`.
    pub formula: Option<String>,
    /// Raw attributes of a `<f>` element we must preserve verbatim
    /// (data-table formulas, unparseable shared groups). Cells carrying this
    /// are never re-evaluated and their `<f>` is written back exactly — with
    /// one exception: array formulas (`t="array"`) are evaluated by the
    /// engine (a dynamic array spills, a legacy CSE block fills its `ref`),
    /// which tracks their extent in [`Cell::spill`].
    pub f_attrs: Option<String>,
    /// Index into [`Styles::xfs`] (`s=` attribute); 0 is the default style.
    pub style: u32,
    /// (rows, cols) of the array anchored here, including this cell: a
    /// dynamic array's spill, or the fixed block a legacy CSE array filled —
    /// set by the recalc engine (or from `<f t="array" ref="…">` at load).
    /// The other cells are plain values owned by this anchor. `Some((1, 1))`
    /// for a modern formula whose array-shaped result is 1x1 (`SEQUENCE(1)`),
    /// so `A1#` resolves to the anchor; `None` for a non-array result (a
    /// scalar or a single-cell range), a one-cell CSE block, or a blocked one.
    pub spill: Option<(u32, u32)>,
    /// `<c>` metadata attributes kept from the file (`cm`, `vm`, `ph`), and
    /// what the engine knows about a formula typed here (`modern`,
    /// `dynamic`); boxed because only formulas typed here and the few cells a
    /// file marks have any.
    pub meta: Option<Box<CellMeta>>,
}

/// A cell's metadata. From the file, the `<c>` attributes we write back: Excel
/// marks a dynamic-array anchor with `cm` (without it the spill reopens as a
/// legacy Ctrl+Shift+Enter array), and a rich value (image, data type,
/// `#SPILL!` details) with `vm`, both indices into `xl/metadata.xml`. From the
/// engine, in-session only: whether a formula was typed here (`modern`) and
/// whether it is a dynamic array (`dynamic`), for which save resolves a `cm`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CellMeta {
    /// Cell-metadata index; written only on an array `<f>`.
    pub cm: Option<String>,
    /// Value-metadata index and the value it was loaded with: it describes
    /// that value, so it is written only while the cell still holds it.
    pub vm: Option<(String, CellValue)>,
    /// The `<v>` text the file wrote for a rich error whose real value was
    /// decoded from the value metadata (`#VALUE!` standing in for `#SPILL!`,
    /// `#CALC!` or `#GETTING_DATA`). Written back instead of the value while
    /// `vm` is.
    pub vm_body: Option<String>,
    /// `ph="1"`: show phonetic text.
    pub ph: bool,
    /// The formula was typed here: set when [`crate::engine::Engine::set_cell`]
    /// gets new formula text (the same text keeps what the formula was), and
    /// carried by copies ([`crate::edit`]'s rebase). Evaluated with spill
    /// semantics (not implicit intersection), also after the engine is
    /// rebuilt. Never written to the file.
    pub modern: bool,
    /// The engine evaluated this typed formula as an array (a multi-cell range
    /// or any computed array, even 1x1), so it is a dynamic array: the writer
    /// gives it a `cm` naming an `fDynamic` entry in `xl/metadata.xml`. Sticky,
    /// like a loaded `cm`: a later scalar result (`FILTER` → `#CALC!`) keeps it.
    pub dynamic: bool,
}

/// Do preserved `<f>` attributes (see [`Cell::f_attrs`]) mark an array
/// formula (`t="array"`)?
pub fn is_array_f(attrs: &str) -> bool {
    attrs.contains("t=\"array\"")
}

/// The `ref` named by preserved `<f>` attributes, if any.
pub(crate) fn f_ref(fa: &str) -> Option<&str> {
    let start = fa.find(" ref=\"")? + " ref=\"".len();
    let end = fa[start..].find('"').map_or(fa.len(), |e| start + e);
    Some(&fa[start..end])
}

/// Preserved `<f>` attributes with `ref` set to `r` (added if absent).
pub(crate) fn with_ref(fa: &str, r: &str) -> String {
    match fa.find(" ref=\"") {
        Some(i) => {
            let start = i + " ref=\"".len();
            let end = fa[start..].find('"').map_or(fa.len(), |e| start + e);
            format!("{}{r}{}", &fa[..start], &fa[end..])
        }
        None => format!("{fa} ref=\"{r}\""),
    }
}

/// Does the `ref` in preserved `<f>` attributes start at `anchor` (a cell
/// name)? A block's ref always starts at the cell that holds it; one that
/// starts elsewhere, or is missing, names a block this cell doesn't own.
pub(crate) fn ref_starts_at(fa: &str, anchor: &str) -> bool {
    f_ref(fa)
        .and_then(|r| r.split(':').next())
        .is_some_and(|tl| tl.eq_ignore_ascii_case(anchor))
}

/// Does the `ref` in preserved `<f>` attributes cover `(row, col)`?
pub(crate) fn ref_covers(fa: &str, row: u32, col: u32) -> bool {
    f_ref(fa)
        .and_then(parse_range_name)
        .is_some_and(|(r1, c1, r2, c2)| (r1..=r2).contains(&row) && (c1..=c2).contains(&col))
}

/// An array formula at `(row, col)` whose `ref` doesn't start there names
/// another block (a clone, or a cell moved without the engine): it covers
/// its own cell instead. A ref that does start there is left as it is.
pub(crate) fn own_array_ref(cell: &mut Cell, row: u32, col: u32) {
    if let Some(fa) = cell.f_attrs.as_deref().filter(|a| is_array_f(a)) {
        let anchor = cell_name(row, col);
        if !ref_starts_at(fa, &anchor) {
            cell.f_attrs = Some(with_ref(fa, &anchor));
        }
    }
}

/// The block an array formula's stored `ref` names, as 0-based
/// `(r1, c1, r2, c2)`: `None` for a cell that is not an array formula or
/// whose `ref` is missing or unreadable.
pub fn array_block(cell: &Cell) -> Option<(u32, u32, u32, u32)> {
    let fa = cell.f_attrs.as_deref().filter(|a| is_array_f(a))?;
    parse_range_name(f_ref(fa)?)
}

/// Undo/redo snapshots of the cells at `keys` as the sheet holds them now: a
/// plain cell inside the spill of a live anchor is spill output, and is
/// snapshotted as a blank ([`Cell::blank_like`]) the anchor re-spills over.
/// Put back as a plain value it would block the anchor with `#SPILL!` (the
/// engine drops a submitted extent and recomputes it). An anchor `frozen`
/// reports as kept on its cached values (asked only of anchors over a key)
/// never re-spills, so its cells are kept as they are.
pub fn snapshot_cells(
    sheet: &Sheet,
    keys: &[(u32, u32)],
    mut frozen: impl FnMut(u32, u32) -> bool,
) -> Vec<Option<Cell>> {
    let anchors: Vec<((u32, u32), (u32, u32))> = sheet
        .cells
        .iter()
        .filter(|(_, cl)| cl.formula.is_some())
        .filter_map(|(&at, cl)| cl.spill.map(|ext| (at, ext)))
        .collect();
    let mut live: HashMap<(u32, u32), bool> = HashMap::new();
    keys.iter()
        .map(|&(r, c)| {
            let cell = sheet.cell(r, c)?;
            let spilled = cell.formula.is_none()
                && anchors.iter().any(|&((ar, ac), (h, w))| {
                    (r, c) != (ar, ac)
                        && r >= ar
                        && r < ar + h
                        && c >= ac
                        && c < ac + w
                        && *live.entry((ar, ac)).or_insert_with(|| !frozen(ar, ac))
                });
            Some(if spilled {
                cell.blank_like()
            } else {
                cell.clone()
            })
        })
        .collect()
}

/// The cells an undo group over `keys` must record: `keys` in order,
/// followed by what a frozen spill anchor (one `frozen` reports as kept on
/// its cached values, which never re-spills) needs to come back whole.
/// [`crate::engine::Engine::restore_cells`] puts its extent and cached
/// values back only from cells in the same snapshot, and an edit can take
/// either outside the keys ([`crate::engine::Engine::set_cell`]):
///
/// - a key that is the anchor: replacing it clears its spill values;
/// - a key inside its block: a value typed there drops the anchor's extent.
///   The anchor is added, and its values too: restoring the anchor clears
///   them before it refills the ones the group holds.
///
/// Each is added once, and not if `keys` names it already. A live anchor
/// adds nothing: it re-spills from its formula.
///
/// Only the values clearing takes are added: plain non-empty cells the
/// sheet holds in the extent (its `spill`, not its `ref`). An empty cell is
/// the same before and after, and the extent comes unbounded from a loaded
/// `ref` (`A1:XFD1048576`), so the walk costs the cells held in its rows,
/// not its area. `frozen` — an evaluation, in the hosts — is asked once per
/// anchor, and of a key's own anchor only when it holds such a value
/// outside `keys`; a live anchor typed over, or over a key, is still asked.
/// The order within the group is not load-bearing: an anchor's restore
/// clears or refills its block whichever comes first.
pub fn frozen_spill_keys(
    sheet: &Sheet,
    keys: &[(u32, u32)],
    mut frozen: impl FnMut(u32, u32) -> bool,
) -> Vec<(u32, u32)> {
    let anchor_ext = |at: (u32, u32)| {
        sheet
            .cell(at.0, at.1)
            .filter(|cl| cl.formula.is_some())
            .and_then(|cl| cl.spill)
    };
    // Every spill anchor, found once, for the keys that are not one.
    let mut all_anchors = None;
    let mut candidates = Vec::new();
    for &(r, c) in keys {
        if let Some(ext) = anchor_ext((r, c)) {
            candidates.push(((r, c), ext, false));
            continue;
        }
        let anchors = all_anchors.get_or_insert_with(|| {
            sheet
                .cells
                .iter()
                .filter(|(_, cl)| cl.formula.is_some())
                .filter_map(|(&at, cl)| cl.spill.map(|ext| (at, ext)))
                .collect::<Vec<_>>()
        });
        for &((ar, ac), (h, w)) in anchors.iter() {
            if r >= ar && r < ar.saturating_add(h) && c >= ac && c < ac.saturating_add(w) {
                candidates.push(((ar, ac), (h, w), true));
            }
        }
    }
    let mut out = keys.to_vec();
    let mut seen: HashSet<(u32, u32)> = keys.iter().copied().collect();
    let mut asked = HashSet::new();
    for ((r, c), (h, w), owner) in candidates {
        if !asked.insert((r, c)) {
            continue;
        }
        let cols = c..c.saturating_add(w);
        let held: Vec<(u32, u32)> = sheet
            .cells
            .range((r, c)..(r.saturating_add(h), 0))
            .filter(|&(&(_, cc), cl)| {
                cols.contains(&cc) && cl.formula.is_none() && !cl.value.is_empty()
            })
            .map(|(&at, _)| at)
            .filter(|at| !seen.contains(at))
            .collect();
        if (held.is_empty() && !owner) || !frozen(r, c) {
            continue;
        }
        for at in owner.then_some((r, c)).into_iter().chain(held) {
            if seen.insert(at) {
                out.push(at);
            }
        }
    }
    out
}

impl Cell {
    /// An empty cell with this one's style: what a spilled value is put back
    /// as, ahead of the anchor that refills it.
    pub fn blank_like(&self) -> Cell {
        Cell {
            style: self.style,
            ..Cell::default()
        }
    }

    pub fn number(n: f64) -> Cell {
        Cell {
            value: CellValue::Number(n),
            ..Cell::default()
        }
    }
    pub fn text(s: &str) -> Cell {
        Cell {
            value: CellValue::Text(s.to_string()),
            ..Cell::default()
        }
    }
    pub fn formula(src: &str) -> Cell {
        Cell {
            formula: Some(src.to_string()),
            ..Cell::default()
        }
    }
    /// Empty value, no formula — but possibly still worth keeping for `style`.
    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none()
    }
    /// Is the formula an array one, evaluated as an array by the engine (a
    /// spill, or a legacy CSE block's fill): a `t="array"` `<f>`, or a dynamic array ([`Cell::is_dynamic`]) whose
    /// `f_attrs` an edit dropped ([`crate::engine::Engine::set_cell`])?
    pub fn is_array_formula(&self) -> bool {
        self.f_attrs.as_deref().is_some_and(is_array_f) || self.is_dynamic()
    }
    /// Did Excel mark this cell a dynamic array (`cm`)?
    pub fn has_cm(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.cm.is_some())
    }
    /// Is this a dynamic array: marked by Excel (`cm`), or a typed formula the
    /// engine evaluated as an array ([`CellMeta::dynamic`])?
    pub fn is_dynamic(&self) -> bool {
        self.meta
            .as_ref()
            .is_some_and(|m| m.cm.is_some() || m.dynamic)
    }
    /// Was the formula typed or edited here ([`CellMeta::modern`])?
    pub fn is_modern(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.modern)
    }
}

// ---------------------------------------------------------------------------
// Sheets
// ---------------------------------------------------------------------------

/// A column-range definition from `<cols>`: width plus any attributes we don't
/// model (style, hidden, bestFit…), preserved verbatim.
#[derive(Clone, Debug, PartialEq)]
pub struct ColDef {
    /// 0-based inclusive column range this definition covers.
    pub min: u32,
    pub max: u32,
    /// Width in Excel's character units (None = default width).
    pub width: Option<f64>,
    /// Raw leftover attributes (everything but min/max/width/customWidth).
    pub attrs: String,
}

/// Excel's default column width in character units.
pub const DEFAULT_COL_WIDTH: f64 = 8.43;

/// Whether a raw attribute string carries a truthy `hidden` flag.
fn attr_hidden(attrs: &str) -> bool {
    attrs.contains("hidden=\"1\"") || attrs.contains("hidden=\"true\"")
}

/// Remove `name="…"` from a space-separated attribute string, tidying whitespace.
fn strip_xml_attr(s: &str, name: &str) -> String {
    let key = format!("{name}=\"");
    let out = if let Some(i) = s.find(&key) {
        let after = i + key.len();
        match s[after..].find('"') {
            Some(q) => format!("{}{}", &s[..i], &s[after + q + 1..]),
            None => s.to_string(),
        }
    } else {
        s.to_string()
    };
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub name: String,
    /// Sparse grid, keyed by 0-based (row, col); row-major iteration order.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// Column widths & preserved column attributes, from `<cols>`.
    pub col_defs: Vec<ColDef>,
    /// Raw `<row>` attributes (heights etc.) minus `r`/`spans`, preserved so
    /// regenerating `<sheetData>` doesn't drop row formatting.
    pub row_attrs: BTreeMap<u32, String>,
    /// Merged regions (r1, c1, r2, c2), 0-based inclusive. Rendered read-only
    /// and preserved on save.
    pub merges: Vec<(u32, u32, u32, u32)>,
    /// Frozen panes as (rows, cols) from the sheet's `<pane state="frozen">`
    /// (0 = not frozen in that axis). Preserved on save via the worksheet splice;
    /// the viewer freezes the leading rows/cols on open.
    pub freeze: (u32, u32),
    /// Conditional-formatting blocks (`<conditionalFormatting>`), evaluated at
    /// render time to overlay a differential format on matching cells.
    pub cond_formats: Vec<CondFormat>,
    /// Cell hyperlinks, keyed by 0-based (row, col). The value is an external URL
    /// or an in-workbook location as `#Sheet!A1`. Rendered underlined; a click
    /// opens the URL (external) or jumps (internal).
    pub hyperlinks: std::collections::BTreeMap<(u32, u32), String>,
    /// Data-validation rules (`<dataValidation>`): the constraint on a cell's
    /// value (a dropdown list, a number range, …). Surfaced in the UI, not
    /// enforced on edit.
    pub validations: Vec<DataValidation>,
    /// Floating drawings anchored to the grid (`xl/drawings/*`): pictures and
    /// charts. Rendered as an overlay; only their anchors are editable.
    pub drawings: Vec<Drawing>,
    /// The part path these `drawings` were read from, so a save can write their
    /// anchors back into it (the part itself round-trips verbatim otherwise).
    pub drawing_part: Option<String>,
    /// [`Drawing::anchor_ix`] of drawings deleted since the file was loaded —
    /// the same round-trip means a save has to strike them from the part too.
    pub drawings_removed: Vec<usize>,
    /// [`CondFormat::ix`] of blocks a structural edit deleted (every range
    /// gone): the worksheet part still holds them, so a save strikes them.
    pub cf_removed: Vec<usize>,
    /// [`DataValidation::ix`] of rules a structural edit deleted, likewise.
    pub dv_removed: Vec<usize>,
    /// Sheet protection: `Some(attrs)` holds the raw attribute string of the
    /// worksheet's `<sheetProtection>` element (e.g. `sheet="1" objects="1"`),
    /// serialized verbatim so any existing password hash / flag set round-trips.
    /// `None` when the sheet is unprotected. Advisory in the viewer; enforced by
    /// Excel on open.
    pub protection: Option<String>,
    /// Page breaks (manual and automatic) from the sheet's own `<rowBreaks>` /
    /// `<colBreaks>`, not a custom view's, in the file's order, which need
    /// not be sorted. A save rewrites those elements only when these differ
    /// from what the part holds, and adds one only for breaks inserted on a
    /// sheet that had none ([`crate::print::area::insert_page_break`]).
    pub row_breaks: Vec<PageBreak>,
    pub col_breaks: Vec<PageBreak>,
    /// Margins, paper, scaling, print options and headers/footers. Edit
    /// this; a save writes what differs from
    /// [`Sheet::page_setup_loaded`].
    pub page_setup: crate::print::setup::PageSetup,
    /// The page setup the worksheet part held at load (the default for a
    /// sheet with no part yet), so a save patches only what changed.
    pub page_setup_loaded: crate::print::setup::PageSetup,
    /// `<sheetFormatPr>`'s default column width and row height, read only:
    /// the save leaves the element as it is.
    pub format: SheetFormat,
    /// `<sheet state="hidden|veryHidden">` in workbook.xml, read only. A
    /// hidden sheet is left out when the entire workbook prints; a named or
    /// active hidden sheet prints.
    pub hidden: bool,
    /// Rows an applied filter hid, as opposed to rows hidden by hand: derived
    /// at load from the `<autoFilter>` criteria, and kept by the editor's own
    /// filter. In memory only. `SUBTOTAL(1..11)` skips these rows but counts
    /// hand-hidden ones. See [`Sheet::row_filtered`].
    pub filtered_rows: std::collections::BTreeSet<u32>,
    /// Where the sheet's own top-level `<autoFilter>` sits (not a custom
    /// view's or a table's), so structural edits can move it. `None` when the
    /// part has none, or once a delete took all of its rows or columns. A save
    /// rewrites the element only when this differs from what the part holds,
    /// and never adds one the file didn't have.
    pub auto_filter: Option<SheetAutoFilter>,
}

/// A worksheet's `<sheetFormatPr>` sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SheetFormat {
    /// `defaultColWidth`, in the same units as `<col width>` (padding
    /// included).
    pub default_col_width: Option<f64>,
    /// `baseColWidth`: characters, padding excluded (schema default 8).
    pub base_col_width: u32,
    /// `defaultRowHeight` in points.
    pub default_row_height: Option<f64>,
}

impl Default for SheetFormat {
    fn default() -> Self {
        SheetFormat {
            default_col_width: None,
            base_col_width: 8,
            default_row_height: None,
        }
    }
}

/// The position of a sheet's `<autoFilter>`: its range and the column each
/// `<filterColumn>` filters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetAutoFilter {
    /// (r1, c1, r2, c2), 0-based, header row included.
    pub range: (u32, u32, u32, u32),
    /// The absolute column of each `<filterColumn>` in document order (the
    /// range's left column plus its `colId`); `None` once a delete removed
    /// that column.
    pub columns: Vec<Option<u32>>,
}

/// One `<brk>`: `id` is the 0-based first row (column) of the page that starts
/// at the break, so a break follows that row through inserts and deletes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageBreak {
    pub id: u32,
    /// The element's other attributes (`min`, `max`, `man`, `pt`), raw and
    /// with a leading space, written back as they came.
    pub attrs: String,
}

impl PageBreak {
    /// Manual (user-inserted) as opposed to automatic.
    pub fn is_manual(&self) -> bool {
        self.attrs.contains(" man=\"1\"") || self.attrs.contains(" man=\"true\"")
    }
}

impl Sheet {
    /// Whether the sheet carries a `<sheetProtection>` element.
    pub fn is_protected(&self) -> bool {
        self.protection.is_some()
    }

    /// Toggle sheet protection. Protecting writes Excel's default flag set
    /// (lock the sheet, objects, and scenarios; no password); unprotecting drops
    /// the element entirely.
    pub fn set_protected(&mut self, on: bool) {
        self.protection = on.then(|| "sheet=\"1\" objects=\"1\" scenarios=\"1\"".to_string());
    }
}

/// A floating drawing anchored over a cell rectangle (a picture or a chart).
#[derive(Clone, Debug)]
pub struct Drawing {
    /// This drawing's position among ALL anchors in its part — anchors we can't
    /// render (shapes, text boxes) are skipped here but still occupy a slot, so
    /// a save needs this to rewrite the right element. A drawing `add_chart`
    /// spliced in gets the index it landed at, so it is addressable too.
    pub anchor_ix: usize,
    /// Top-left anchor cell `(row, col)`, 0-based.
    pub from: (u32, u32),
    /// Bottom-right extent `(row, col)`, 0-based, inclusive-ish. For a
    /// `oneCellAnchor` it's estimated from the drawing's EMU size.
    pub to: (u32, u32),
    pub kind: DrawingKind,
}

/// What a [`Drawing`] holds.
#[derive(Clone, Debug)]
pub enum DrawingKind {
    /// A picture: the package part path of its media and a display name. The
    /// bytes are read from the package on demand (the model stays light).
    Image { part: String, name: String },
    /// A chart, with the cached category/series data needed to draw it.
    Chart(ChartData),
}

/// The cached data of a chart (`xl/charts/chartN.xml`), enough to draw a simple
/// bar/pie/line representation without re-running the plot area.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartData {
    pub title: String,
    /// `bar` / `pie` / `line` / `area` / `scatter` … (the plot element's local name).
    pub kind: String,
    pub categories: Vec<String>,
    pub series: Vec<ChartSeries>,
    /// The cells the chart plots, when it is range-backed rather than a frozen
    /// snapshot — the whole box, header row and labels included. Read from the
    /// `<c:f>` refs on load; written back on save, so Excel sees a live chart too.
    pub source: Option<ChartSource>,
    /// The cells holding the category labels, when known. A series UI edits this
    /// on its own, so it can't be derived from `source` alone.
    pub categories_ref: Option<ChartSource>,
    /// The chart part this came from (`xl/charts/chartN.xml`), so an edit can be
    /// written back into it.
    pub part: Option<String>,
    /// Set once the user changes something here. The part round-trips verbatim
    /// otherwise; only an edited chart is regenerated (and so loses whatever
    /// formatting we don't model).
    pub edited: bool,
    /// The plot area holds something the writer cannot reproduce: a grouping it
    /// doesn't emit (stacked, percentStacked), or more than one plot group (a
    /// combo chart — bars and a line sharing one plot area, often on two axes).
    /// `kind` records only the FIRST group, so regenerating such a part would
    /// silently turn a stacked chart into a clustered one, or fold every series
    /// of a combo onto one axis pair as bars. Those parts round-trip verbatim
    /// instead, the same escape hatch scatter and area use.
    ///
    /// A `<c:pieChart>` holding several `<c:ser>` is NOT one of those shapes,
    /// though it used to be. `CT_PieChart` declares `ser` with
    /// `maxOccurs="unbounded"`, so the file is valid and Excel merely plots the
    /// first; the hold-back existed only because `chart_space_xml`'s pie arm
    /// wrote `series.first()` and regenerating such a part came back short. The
    /// arm writes every series now, so the shape round-trips and the chart
    /// stays editable — see `suite/docs/pie-series.md`, which also records
    /// what an imported pie trades for that: like every other writable chart,
    /// its part is regenerated on edit, so per-slice `<c:dPt>` fills, data
    /// labels and legend placement go the way they do everywhere else.
    pub complex: bool,
    /// Which way round the chart reads its range: `false` (the default) is
    /// Excel's column orientation — each column of `source` is a series, the
    /// first text column supplies the category labels. `true` is the transpose:
    /// each row is a series, the first text row supplies the categories.
    ///
    /// SpreadsheetML has no orientation element, so this is NOT stored in the
    /// file: Excel infers it from the shape of the refs a chart holds (a
    /// `<c:val>` spanning `$B$2:$B$5` is a column series, `$B$2:$D$2` a row
    /// one), and so does the loader. The flag exists so the UI can show and
    /// flip the choice, and so the chart can be re-derived from its range.
    pub by_row: bool,
}

/// The worksheet range a chart plots: the sheet by name (as the `<c:f>` refs
/// spell it) and the 0-based inclusive cell box, header row and label column
/// included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChartSource {
    pub sheet: String,
    pub range: (u32, u32, u32, u32),
    /// The column inside `range` holding the category labels — when the chart
    /// is read COLUMN-wise. A row-oriented chart ([`ChartData::by_row`]) takes
    /// its labels from a row, which no column index can express, so this holds
    /// the column its SERIES NAMES come from instead. The writer must therefore
    /// derive a row chart's `<c:cat>` from `ChartData::categories_ref`, never
    /// from here.
    pub cat_col: u32,
}

/// A sheet name as a formula/`<c:f>` reference spells it. Anything that isn't a
/// bare identifier has to be quoted — spaces, but also `-`, `(`, `.`, `&`, a
/// leading digit — and an apostrophe inside the name is doubled. Excel reports a
/// workbook whose chart refs get this wrong as needing repair, and drops the
/// chart.
pub fn quote_sheet_name(name: &str) -> String {
    if name.is_empty() {
        return String::new(); // no sheet part at all
    }
    let bare = !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        // A name shaped like a cell reference (`A1`, `XFD1048576`) must be
        // quoted too, or `A1!$B$2` reads as a range.
        && parse_cell_name(name).is_none();
    if bare {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

impl ChartSource {
    /// The `Sheet1!` a ref carries in front of its cells — nothing at all when
    /// the source names no sheet, since a bare `!$A$1` is not a reference Excel
    /// will read.
    fn prefix(&self) -> String {
        match quote_sheet_name(&self.sheet) {
            n if n.is_empty() => String::new(),
            n => format!("{n}!"),
        }
    }

    /// The `Sheet1!$A$1:$D$5` form a chart's `<c:f>` refs use. `rows` narrows it
    /// to one column of the box (a series), leaving the header row out.
    pub fn f_ref(&self, c1: u32, c2: u32, skip_header: bool) -> String {
        let (r1, _, r2, _) = self.range;
        let top = if skip_header {
            r1.saturating_add(1).min(r2)
        } else {
            r1
        };
        let name = self.prefix();
        format!(
            "{name}${}${}:${}${}",
            col_name(c1),
            top + 1,
            col_name(c2),
            r2 + 1
        )
    }

    /// This source's own cells as an absolute ref — for a per-series range,
    /// which already excludes the header row.
    pub fn to_ref(&self) -> String {
        let (r1, c1, r2, c2) = self.range;
        let name = self.prefix();
        format!(
            "{name}${}${}:${}${}",
            col_name(c1),
            r1 + 1,
            col_name(c2),
            r2 + 1
        )
    }

    /// The single header cell above `col` — a series' name ref.
    pub fn header_ref(&self, col: u32) -> String {
        let name = self.prefix();
        format!("{name}${}${}", col_name(col), self.range.0 + 1)
    }

    /// The single label cell left of `row` — a row-oriented series' name ref,
    /// the transpose of [`header_ref`](Self::header_ref).
    pub fn label_ref(&self, row: u32) -> String {
        let name = self.prefix();
        format!("{name}${}${}", col_name(self.range.1), row + 1)
    }

    /// Parse a `Sheet1!$A$1:$D$5` ref (the sheet part optional).
    pub fn parse_f_ref(s: &str) -> Option<ChartSource> {
        let (sheet, cells) = match s.rsplit_once('!') {
            // The inverse of `quote_sheet_name`: unwrap the quotes and undo the
            // apostrophe doubling, so a sheet called `Bob's data` survives a
            // round trip instead of coming back as `Bob''s data`.
            Some((a, b)) => {
                let name = match a.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
                    Some(inner) => inner.replace("''", "'"),
                    None => a.to_string(),
                };
                (name, b)
            }
            None => (String::new(), s),
        };
        let range = parse_range_name(cells)?;
        Some(ChartSource {
            sheet,
            range,
            cat_col: range.1,
        })
    }

    /// Grow to also cover `other`'s cells (same sheet assumed — a chart drawing
    /// from two sheets keeps only the first).
    pub fn union(&mut self, other: &ChartSource) {
        let (r1, c1, r2, c2) = self.range;
        let (or1, oc1, or2, oc2) = other.range;
        self.range = (r1.min(or1), c1.min(oc1), r2.max(or2), c2.max(oc2));
    }
}

/// The numbers in a range, row-major, with blanks and text as 0 — what a chart
/// series plots when it is pointed at those cells.
pub fn range_numbers(sheet: &Sheet, range: (u32, u32, u32, u32)) -> Vec<f64> {
    let (r1, c1, r2, c2) = range;
    (r1..=r2)
        .flat_map(|r| (c1..=c2).map(move |c| (r, c)))
        .map(|(r, c)| match sheet.cell(r, c).map(|cl| &cl.value) {
            Some(CellValue::Number(n)) => *n,
            _ => 0.0,
        })
        .collect()
}

/// One cell as a chart reads it: the text of a label, a series name or a
/// category.
///
/// Every chart path goes through here so that one cell cannot read two ways.
/// `format_with` is what makes a boolean Excel's `TRUE` rather than Rust's
/// `true` and a number the General spelling of itself; the panel's fields reach
/// the same answer through [`range_labels`], so naming a series by hand and
/// deriving the same name from the range agree, and `Switch Row/Column` cannot
/// silently respell one.
fn cell_text(sheet: &Sheet, r: u32, c: u32) -> String {
    match sheet.cell(r, c).map(|cl| &cl.value) {
        Some(CellValue::Text(t)) => t.clone(),
        Some(v @ (CellValue::Number(_) | CellValue::Bool(_) | CellValue::Error(_))) => {
            format_with(&Xf::default(), v, false)
        }
        _ => String::new(),
    }
}

/// The text in a range, row-major — category labels, or a series name.
pub fn range_labels(sheet: &Sheet, range: (u32, u32, u32, u32)) -> Vec<String> {
    let (r1, c1, r2, c2) = range;
    (r1..=r2)
        .flat_map(|r| (c1..=c2).map(move |c| (r, c)))
        .map(|(r, c)| cell_text(sheet, r, c))
        .collect()
}

/// Read a chart's data out of a worksheet range, either way round.
///
/// With `by_row` false — Excel's default, and the only thing docxy wrote before
/// orientation existed — the first row names the series, one column of labels
/// becomes the categories, and every column that holds numbers becomes a
/// series. With `by_row` true it is the transpose: the first column names the
/// series, one row of labels becomes the categories, and every numeric row is a
/// series.
///
/// This is what the Insert button plots, what re-pointing a chart at a new range
/// replots, and what Switch Row/Column re-derives.
pub fn chart_from_range(
    sheet: &Sheet,
    sheet_name: &str,
    range: (u32, u32, u32, u32),
    kind: &str,
    by_row: bool,
) -> Option<ChartData> {
    if by_row {
        chart_from_rows(sheet, sheet_name, range, kind)
    } else {
        chart_from_columns(sheet, sheet_name, range, kind)
    }
}

/// The column reading of a range: one series per numeric column. Kept as it was
/// before orientation existed, so a column chart still comes out byte-for-byte
/// what it always did — with one carve-out. Its own `text_of` used to spell a
/// boolean label Rust's way (`true`); lifting it into the shared [`cell_text`]
/// gives it Excel's `TRUE`, the spelling every other chart path and the panel's
/// own fields already used. Nothing else moved: numbers and errors render
/// identically, and `text_of` never fed the numeric-vs-text classification
/// below, which reads `sheet.cell` directly.
fn chart_from_columns(
    sheet: &Sheet,
    sheet_name: &str,
    range: (u32, u32, u32, u32),
    kind: &str,
) -> Option<ChartData> {
    let (r0, c0, r1, c1) = range;
    if r1 <= r0 {
        return None; // header row only — nothing to plot
    }
    let text_of = |r: u32, c: u32| cell_text(sheet, r, c);
    // A column is a series if it is mostly numbers; the first that isn't
    // supplies the category labels.
    let (mut cat_col, mut num_cols) = (None, Vec::new());
    for c in c0..=c1 {
        let (mut nums, mut txts) = (0u32, 0u32);
        for r in (r0 + 1)..=r1 {
            match sheet.cell(r, c).map(|cl| &cl.value) {
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
    if num_cols.is_empty() {
        return None;
    }
    // Keep "we found a label column" apart from "we had to pick one". Every
    // column being numeric (`Year | Sales`) falls back to the first, which is
    // itself plotted — naming it in `<c:cat>` would label the numbers with
    // themselves. Excel writes literal categories in that case, so we do too.
    let label_col = cat_col;
    let cat_col = cat_col.unwrap_or(c0);
    let rows: Vec<u32> = (r0 + 1..=r1).collect();
    let title = text_of(r0, cat_col);
    let src = |c1: u32, c2: u32| ChartSource {
        sheet: sheet_name.to_string(),
        range: (r0 + 1, c1, r1, c2),
        cat_col,
    };
    let series = num_cols
        .iter()
        .map(|&c| ChartSeries {
            name: text_of(r0, c),
            col: Some(c),
            values_ref: Some(src(c, c)),
            name_ref: Some(
                ChartSource {
                    sheet: sheet_name.to_string(),
                    range,
                    cat_col,
                }
                .header_ref(c),
            ),
            values: rows
                .iter()
                .map(|&r| match sheet.cell(r, c).map(|cl| &cl.value) {
                    Some(CellValue::Number(n)) => *n,
                    _ => 0.0,
                })
                .collect(),
            color: None,
            // Authored from a range, so it plots through `<c:val>`; only a
            // scatter or bubble read from a file carries point refs.
            point_refs: Vec::new(),
            points_unheld: false,
            points_ref_unheld: false,
        })
        .collect();
    Some(ChartData {
        title: if title.is_empty() {
            "Chart".into()
        } else {
            title
        },
        kind: kind.to_string(),
        categories: rows.iter().map(|&r| text_of(r, cat_col)).collect(),
        series,
        source: Some(ChartSource {
            sheet: sheet_name.to_string(),
            range,
            cat_col,
        }),
        categories_ref: label_col.map(|c| src(c, c)),
        part: None,
        edited: true,
        // Authored here, so it is exactly what the writer emits.
        complex: false,
        by_row: false,
    })
}

/// The row reading of a range: one series per numeric row, the transpose of
/// [`chart_from_columns`]. The first column holds the series names, the first
/// row that isn't numeric supplies the category labels.
fn chart_from_rows(
    sheet: &Sheet,
    sheet_name: &str,
    range: (u32, u32, u32, u32),
    kind: &str,
) -> Option<ChartData> {
    let (r0, c0, r1, c1) = range;
    if c1 <= c0 {
        return None; // label column only — nothing to plot
    }
    let text_of = |r: u32, c: u32| cell_text(sheet, r, c);
    // A row is a series if it is mostly numbers; the first that isn't supplies
    // the category labels.
    let (mut cat_row, mut num_rows) = (None, Vec::new());
    for r in r0..=r1 {
        let (mut nums, mut txts) = (0u32, 0u32);
        for c in (c0 + 1)..=c1 {
            match sheet.cell(r, c).map(|cl| &cl.value) {
                Some(CellValue::Number(_)) => nums += 1,
                Some(CellValue::Text(_)) => txts += 1,
                _ => {}
            }
        }
        if nums > 0 && nums >= txts {
            num_rows.push(r);
        } else if cat_row.is_none() {
            cat_row = Some(r);
        }
    }
    if num_rows.is_empty() {
        return None;
    }
    // Same split as the column branch: "we found a label row" is not "we had to
    // pick one". Every row being numeric means the fallback row is itself
    // plotted, and naming it in `<c:cat>` would label the numbers with
    // themselves — so the categories go out as literals instead.
    let label_row = cat_row;
    let cat_row = cat_row.unwrap_or(r0);
    let cols: Vec<u32> = (c0 + 1..=c1).collect();
    let title = text_of(cat_row, c0);
    // `cat_col` names the column the SERIES NAMES come from here, not the
    // categories: it is a column index and a row chart takes its labels from a
    // row, which no column index can express. The writer must therefore derive a
    // row chart's `<c:cat>` from `categories_ref`, never from `cat_col`.
    let src = |r_a: u32, r_b: u32| ChartSource {
        sheet: sheet_name.to_string(),
        range: (r_a, c0 + 1, r_b, c1),
        cat_col: c0,
    };
    let whole = ChartSource {
        sheet: sheet_name.to_string(),
        range,
        cat_col: c0,
    };
    let series = num_rows
        .iter()
        .map(|&r| ChartSeries {
            name: text_of(r, c0),
            // A row series occupies no single column, and `col` feeds two
            // column-shaped decisions (the writer's fallback ref and
            // `claimed_col`). A row index here would make both quietly wrong
            // rather than inapplicable; `values_ref` is always set, so the
            // fallback is never reached.
            col: None,
            values_ref: Some(src(r, r)),
            name_ref: Some(whole.label_ref(r)),
            values: cols
                .iter()
                .map(|&c| match sheet.cell(r, c).map(|cl| &cl.value) {
                    Some(CellValue::Number(n)) => *n,
                    _ => 0.0,
                })
                .collect(),
            color: None,
            // Authored from a range, so it plots through `<c:val>`; only a
            // scatter or bubble read from a file carries point refs.
            point_refs: Vec::new(),
            points_unheld: false,
            points_ref_unheld: false,
        })
        .collect();
    Some(ChartData {
        title: if title.is_empty() {
            "Chart".into()
        } else {
            title
        },
        kind: kind.to_string(),
        categories: cols.iter().map(|&c| text_of(cat_row, c)).collect(),
        series,
        source: Some(whole),
        categories_ref: label_row.map(|r| src(r, r)),
        part: None,
        edited: true,
        // Authored here, so it is exactly what the writer emits.
        complex: false,
        by_row: true,
    })
}

/// One data series of a [`ChartData`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartSeries {
    pub name: String,
    pub values: Vec<f64>,
    /// Explicit series colour (`0xRRGGBB`) from its `<c:spPr>` solid fill;
    /// `None` leaves it to the renderer's palette.
    pub color: Option<u32>,
    /// The worksheet column this series reads, when the chart is range-backed
    /// and read COLUMN-wise. Always `None` for a row-oriented series
    /// ([`ChartData::by_row`]), which occupies every column of its ref rather
    /// than one — `None` here means "no single column", not "not range-backed".
    /// It feeds the writer's column-shaped fallback ref and `claimed_col`, so a
    /// row series' left-hand column here would make both quietly wrong rather
    /// than inapplicable.
    pub col: Option<u32>,
    /// The cells this series' values come from. Set independently of the chart's
    /// overall box, so one series can be re-pointed without touching the others.
    pub values_ref: Option<ChartSource>,
    /// The `<c:f>` ref naming this series (usually its header cell), verbatim.
    pub name_ref: Option<String>,
    /// The cells a SCATTER's or BUBBLE's points come from: its `<c:xVal>`,
    /// `<c:yVal>` and `<c:bubbleSize>` refs, in document order.
    ///
    /// Those kinds plot from their own elements rather than from `<c:val>`, so
    /// the loader leaves `values_ref` `None` for them. They are still NUMBERS,
    /// and the chart's box is built from the numbers — keeping them here is
    /// what lets the panel's `rebuild_source` see the same cells the loader
    /// folded, instead of rebuilding a scatter's box out of its label cells
    /// alone and collapsing the DATA RANGE it shows.
    ///
    /// The two slots CAN coexist: the panel's SERIES VALUES field is offered
    /// for every kind, so re-pointing a scatter's series installs a
    /// `values_ref` beside these. That is deliberate — the part still
    /// round-trips verbatim, so the next `parse_chart` reads these refs back
    /// out of it, and clearing them on a re-point would collapse the box the
    /// reload rebuilds. `rebuild_source` folds both, in that order.
    ///
    /// Empty for every kind the writer authors: nothing here derives them for
    /// one, and `chart_set_kind` — the one door that converts an imported chart
    /// into a writable kind — leaves none behind, because the writer regenerates
    /// such a part from `values_ref`, `categories_ref` and the box alone. It
    /// gets there two ways: a series that gained a `values_ref` from a re-point
    /// simply has these cleared (`chart_take_kind`), while one still carrying
    /// nothing but points would regenerate as an EMPTY chart, so that chart is
    /// re-derived from its box instead (`chart_reauthored`) and comes back with
    /// real `values_ref`s and no points at all.
    ///
    /// Re-based by `edit::rename_sheet_in_chart` and `edit::shift_chart_refs`
    /// like every other [`ChartSource`] a chart holds.
    pub point_refs: Vec<ChartSource>,
    /// This series had `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` points the model
    /// could not turn into a ref: a `<c:numLit>` (literal points, no `<c:f>` at
    /// all) or an `<c:f>` [`ChartSource::parse_f_ref`] refuses — a whole column,
    /// a defined name, a multi-area ref.
    ///
    /// Set per point ELEMENT, not per series: whenever ANY ONE of the series'
    /// `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` held points it yielded no ref for.
    /// So it can sit beside a NON-EMPTY `point_refs` when only one half was
    /// readable — a `<c:xVal>` naming `Sheet1!$A:$A` next to a `<c:yVal>` naming
    /// `Sheet1!$B$2:$B$3` leaves one ref and this mark.
    ///
    /// It has to be recorded separately because nothing else on the series
    /// remembers the unreadable half: `values_ref` and `col` are `None` and
    /// `values` empty for a scatter either way, so a series whose points were ALL
    /// unreadable is indistinguishable from the empty one "+ Series" pushes.
    /// `chart_would_lose_points` asks both slots, which is what
    /// keeps picking a writable type from relabelling this chart and letting the
    /// next save write `<c:ptCount val="0"/>` over a plot it could never re-read.
    ///
    /// Never set by anything docxy authors — like `point_refs`, it only ever
    /// arrives from a file, and `chart_take_kind` clears it with them.
    pub points_unheld: bool,
    /// The narrower half of [`Self::points_unheld`]: one of those point elements
    /// held an `<c:f>` NAMING cells that [`ChartSource::parse_f_ref`] refused —
    /// a whole column, a defined name, a multi-area ref.
    ///
    /// The two are worth telling apart because they answer different questions
    /// about the chart's BOX. Literal points (`<c:numLit>`) live in no cells at
    /// all, so no box could have covered them and the one the chart has is the
    /// best that exists. A refused REF names cells the fold then skipped, so the
    /// box is provably short of the plot and re-deriving from it would drop the
    /// half that is off it. Only this mark says the second thing; `points_unheld`
    /// says either, which is all `chart_would_lose_points` needs to know.
    ///
    /// Set per point ELEMENT like its wider half, and cleared with it.
    pub points_ref_unheld: bool,
}

/// One data-validation rule over a set of cell ranges.
#[derive(Clone, Debug, Default)]
pub struct DataValidation {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    /// `list` / `whole` / `decimal` / `date` / `time` / `textLength` / `custom`.
    pub kind: String,
    /// `between` / `greaterThan` / … (for the numeric/date kinds).
    pub operator: String,
    pub formula1: String,
    pub formula2: String,
    /// The input-message prompt, if the file supplies one.
    pub prompt: Option<String>,
    /// The ordinal of this rule's element among the `<dataValidation>`
    /// children of the worksheet's top-level `<dataValidations>`, so a save
    /// can write a structural edit's move back to it. `None` for a rule the
    /// part doesn't hold (an x14 one in `extLst`, one built in memory).
    pub ix: Option<usize>,
}

impl DataValidation {
    /// Whether any of this rule's ranges covers cell (row, col).
    pub fn covers(&self, row: u32, col: u32) -> bool {
        self.ranges
            .iter()
            .any(|&(r1, c1, r2, c2)| row >= r1 && row <= r2 && col >= c1 && col <= c2)
    }

    /// For a `list` validation, the allowed values when they're given inline as a
    /// quoted CSV (`"Yes,No,Maybe"`). `None` when the list is a range reference.
    pub fn list_values(&self) -> Option<Vec<String>> {
        if self.kind != "list" {
            return None;
        }
        let f = self.formula1.trim();
        let inner = f.strip_prefix('"').and_then(|s| s.strip_suffix('"'))?;
        Some(inner.split(',').map(|s| s.trim().to_string()).collect())
    }

    /// Whether this rule imposes anything worth surfacing (a real constraint or
    /// an input message). A bare `type="none"` with no prompt is inert.
    pub fn is_meaningful(&self) -> bool {
        !matches!(self.kind.as_str(), "" | "none") || self.prompt.is_some()
    }

    /// A short human description of the constraint, for the status bar. The
    /// `list`/`custom` kinds ignore the (often-boilerplate) `operator`; the
    /// numeric/date kinds render it.
    pub fn describe(&self) -> String {
        match self.kind.as_str() {
            "list" => {
                let body = self
                    .list_values()
                    .map(|v| v.join(", "))
                    .unwrap_or_else(|| self.formula1.clone());
                format!("List: {body}")
            }
            "custom" => format!("Custom: {}", self.formula1),
            "" | "none" => self.prompt.clone().unwrap_or_default(),
            _ => {
                let name = match self.kind.as_str() {
                    "whole" => "Whole number",
                    "decimal" => "Decimal",
                    "date" => "Date",
                    "time" => "Time",
                    "textLength" => "Text length",
                    other => other,
                };
                let op = match self.operator.as_str() {
                    "notBetween" => format!("not between {} and {}", self.formula1, self.formula2),
                    "greaterThan" => format!("> {}", self.formula1),
                    "lessThan" => format!("< {}", self.formula1),
                    "greaterThanOrEqual" => format!(">= {}", self.formula1),
                    "lessThanOrEqual" => format!("<= {}", self.formula1),
                    "equal" => format!("= {}", self.formula1),
                    "notEqual" => format!("<> {}", self.formula1),
                    // "between" is also the default when the operator is omitted.
                    _ if !self.formula2.is_empty() => {
                        format!("between {} and {}", self.formula1, self.formula2)
                    }
                    _ if !self.formula1.is_empty() => self.formula1.clone(),
                    _ => String::new(),
                };
                if op.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} {op}")
                }
            }
        }
    }
}

impl Sheet {
    pub fn cell(&self, row: u32, col: u32) -> Option<&Cell> {
        self.cells.get(&(row, col))
    }

    /// Set (or clear, when the cell is blank and unstyled) a cell.
    pub fn set_cell(&mut self, row: u32, col: u32, cell: Cell) {
        if cell.is_blank() && cell.style == 0 {
            self.cells.remove(&(row, col));
        } else {
            self.cells.insert((row, col), cell);
        }
    }

    /// Clear a cell's content but keep its style (what Del does in Excel).
    pub fn clear_cell(&mut self, row: u32, col: u32) {
        let style = self.cells.get(&(row, col)).map(|c| c.style).unwrap_or(0);
        self.set_cell(
            row,
            col,
            Cell {
                style,
                ..Cell::default()
            },
        );
    }

    /// (rows, cols) of the used range — the smallest grid containing all cells.
    pub fn used_size(&self) -> (u32, u32) {
        let mut rows = 0;
        let mut cols = 0;
        for &(r, c) in self.cells.keys() {
            rows = rows.max(r + 1);
            cols = cols.max(c + 1);
        }
        (rows, cols)
    }

    /// Display width of a column in character units.
    pub fn col_width(&self, col: u32) -> f64 {
        for d in &self.col_defs {
            if col >= d.min && col <= d.max {
                return d.width.unwrap_or(DEFAULT_COL_WIDTH);
            }
        }
        DEFAULT_COL_WIDTH
    }

    /// Whether a row is hidden — by a manual hide, an outline group, or an
    /// applied auto-filter (Excel persists all three as `hidden="1"`).
    pub fn row_hidden(&self, row: u32) -> bool {
        self.row_attrs.get(&row).is_some_and(|a| attr_hidden(a))
    }

    /// Whether a row is hidden by a filter: hidden, and marked filtered. A
    /// filtered row the user unhid is not.
    pub fn row_filtered(&self, row: u32) -> bool {
        self.filtered_rows.contains(&row) && self.row_hidden(row)
    }

    /// Hide or unhide a row as a filter does: hiding marks it filter-hidden,
    /// unhiding clears the mark.
    pub fn set_row_filtered(&mut self, row: u32, hidden: bool) {
        self.set_row_hidden(row, hidden);
        if hidden {
            self.filtered_rows.insert(row);
        } else {
            self.filtered_rows.remove(&row);
        }
    }

    /// Hide or unhide a row, preserving its other `<row>` attributes (e.g. `ht`).
    pub fn set_row_hidden(&mut self, row: u32, hidden: bool) {
        let cur = self.row_attrs.get(&row).cloned().unwrap_or_default();
        let cleaned = strip_xml_attr(&cur, "hidden");
        let next = if hidden {
            if cleaned.is_empty() {
                "hidden=\"1\"".to_string()
            } else {
                format!("{cleaned} hidden=\"1\"")
            }
        } else {
            cleaned
        };
        if next.is_empty() {
            self.row_attrs.remove(&row);
        } else {
            self.row_attrs.insert(row, next);
        }
    }

    /// The row's outline (grouping) level from its `<row outlineLevel="N">`
    /// attribute; 0 when ungrouped.
    pub fn row_outline(&self, row: u32) -> u8 {
        self.row_attrs
            .get(&row)
            .and_then(|a| {
                a.find("outlineLevel=\"")
                    .map(|i| i + "outlineLevel=\"".len())
                    .and_then(|s| {
                        a[s..]
                            .find('"')
                            .and_then(|e| a[s..s + e].parse::<u8>().ok())
                    })
            })
            .unwrap_or(0)
    }

    /// Set the row's outline (grouping) level, preserving its other `<row>`
    /// attributes. Level 0 removes the grouping.
    pub fn set_row_outline(&mut self, row: u32, level: u8) {
        let cur = self.row_attrs.get(&row).cloned().unwrap_or_default();
        let cleaned = strip_xml_attr(&cur, "outlineLevel");
        let next = if level > 0 {
            if cleaned.is_empty() {
                format!("outlineLevel=\"{level}\"")
            } else {
                format!("{cleaned} outlineLevel=\"{level}\"")
            }
        } else {
            cleaned
        };
        if next.is_empty() {
            self.row_attrs.remove(&row);
        } else {
            self.row_attrs.insert(row, next);
        }
    }

    /// The deepest outline level used by any row (for `<sheetFormatPr
    /// outlineLevelRow>` and collapse controls). 0 when the sheet is flat.
    pub fn max_row_outline(&self) -> u8 {
        self.row_attrs
            .keys()
            .map(|&r| self.row_outline(r))
            .max()
            .unwrap_or(0)
    }

    /// The row's explicit height in points (`<row ht="…">`), or `None` when it
    /// uses the sheet default.
    pub fn row_height(&self, row: u32) -> Option<f64> {
        self.row_attrs.get(&row).and_then(|a| {
            a.find("ht=\"").map(|i| i + "ht=\"".len()).and_then(|s| {
                a[s..]
                    .find('"')
                    .and_then(|e| a[s..s + e].parse::<f64>().ok())
            })
        })
    }

    /// Set (or clear, with `None`) the row's explicit height in points,
    /// preserving its other `<row>` attributes. A concrete height also stamps
    /// `customHeight="1"` so Excel honours it rather than auto-fitting.
    pub fn set_row_height(&mut self, row: u32, pts: Option<f64>) {
        let cur = self.row_attrs.get(&row).cloned().unwrap_or_default();
        let cleaned = strip_xml_attr(&strip_xml_attr(&cur, "ht"), "customHeight");
        let next = match pts {
            Some(h) => {
                let h = format!("ht=\"{h}\" customHeight=\"1\"");
                if cleaned.is_empty() {
                    h
                } else {
                    format!("{cleaned} {h}")
                }
            }
            None => cleaned,
        };
        if next.is_empty() {
            self.row_attrs.remove(&row);
        } else {
            self.row_attrs.insert(row, next);
        }
    }

    /// Whether a column is hidden (its `<col>` definition carries `hidden="1"`).
    pub fn col_hidden(&self, col: u32) -> bool {
        self.col_defs
            .iter()
            .any(|d| col >= d.min && col <= d.max && attr_hidden(&d.attrs))
    }

    /// Set one column's width, splitting any range definition that covers it.
    pub fn set_col_width(&mut self, col: u32, width: f64) {
        let mut out: Vec<ColDef> = Vec::with_capacity(self.col_defs.len() + 2);
        let mut placed = false;
        for d in self.col_defs.drain(..) {
            if col < d.min || col > d.max {
                out.push(d);
                continue;
            }
            // Split [min..max] around `col`, keeping the other attrs on all parts.
            if d.min < col {
                out.push(ColDef {
                    min: d.min,
                    max: col - 1,
                    ..d.clone()
                });
            }
            out.push(ColDef {
                min: col,
                max: col,
                width: Some(width),
                attrs: d.attrs.clone(),
            });
            if d.max > col {
                out.push(ColDef {
                    min: col + 1,
                    max: d.max,
                    ..d
                });
            }
            placed = true;
        }
        if !placed {
            out.push(ColDef {
                min: col,
                max: col,
                width: Some(width),
                attrs: String::new(),
            });
        }
        out.sort_by_key(|d| d.min);
        self.col_defs = out;
    }

    /// The merged region containing (row, col), if any.
    pub fn merge_at(&self, row: u32, col: u32) -> Option<(u32, u32, u32, u32)> {
        self.merges
            .iter()
            .copied()
            .find(|&(r1, c1, r2, c2)| row >= r1 && row <= r2 && col >= c1 && col <= c2)
    }
}

// ---------------------------------------------------------------------------
// Workbook
// ---------------------------------------------------------------------------

/// An Excel Table (ListObject): a named rectangular region with headers,
/// resolvable by structured references (`Table1[Amount]`, `[@Price]`).
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    /// The displayName — what formulas use.
    pub name: String,
    /// Owning sheet index in `Workbook::sheets`.
    pub sheet: usize,
    /// Full region incl. header and totals rows: (r1, c1, r2, c2), 0-based.
    pub range: (u32, u32, u32, u32),
    pub header_rows: u32,
    pub totals_rows: u32,
    /// Column names, left to right.
    pub columns: Vec<String>,
    /// The xl/tables/*.xml part backing this table (its `ref` is patched on
    /// save when the range moved).
    pub part: String,
}

impl Table {
    /// The data region (between header and totals), if non-empty.
    pub fn data_rows(&self) -> Option<(u32, u32)> {
        let r1 = self.range.0 + self.header_rows;
        let r2 = self.range.2.checked_sub(self.totals_rows)?;
        (r1 <= r2).then_some((r1, r2))
    }

    /// 0-based sheet column of a named table column.
    pub fn column_index(&self, name: &str) -> Option<u32> {
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
            .map(|i| self.range.1 + i as u32)
    }

    pub fn contains(&self, sheet: usize, row: u32, col: u32) -> bool {
        sheet == self.sheet
            && row >= self.range.0
            && row <= self.range.2
            && col >= self.range.1
            && col <= self.range.3
    }
}

/// A workbook-level defined name: `TaxRate` → `0.21`, `Data` →
/// `Sheet1!$A$1:$B$9`. `scope` restricts the name to one sheet
/// (`localSheetId`); None = workbook-global.
#[derive(Clone, Debug, PartialEq)]
pub struct DefinedName {
    pub name: String,
    pub scope: Option<usize>,
    /// The definition as formula text (no leading `=`).
    pub formula: String,
}

#[derive(Clone, Debug, Default)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub styles: Styles,
    pub defined_names: Vec<DefinedName>,
    pub tables: Vec<Table>,
    /// Pivot tables (parsed read-only from their preserved parts, so they
    /// can be refreshed from current source data).
    pub pivots: Vec<crate::pivot::Pivot>,
    /// True when the workbook uses the 1904 date system (Mac legacy).
    pub date1904: bool,
    /// Iterative calculation opt-in from `<calcPr iterate="1">`:
    /// (max iterations, convergence delta). None = cycles are errors.
    pub iterate: Option<(u32, f64)>,
    /// The active sheet (`<workbookView activeTab>`): the sheet the workbook
    /// opens on and the one a CSV export writes. Saved back to `activeTab`;
    /// when the file marks a selected tab (`tabSelected`, as Excel's files
    /// do), the mark moves to this sheet alone. A file that marks none is
    /// left unmarked.
    pub active_tab: usize,
}

impl Workbook {
    /// Sheet index by name, case-insensitive (as Excel resolves references).
    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// A table by displayName, case-insensitive.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// The table containing a cell, if any (for bare `[@Col]` references).
    pub fn table_at(&self, sheet: usize, row: u32, col: u32) -> Option<&Table> {
        self.tables.iter().find(|t| t.contains(sheet, row, col))
    }

    /// Resolve a defined name as seen from `current_sheet`: a name scoped to
    /// that sheet shadows a global one (Excel's rule).
    pub fn defined_name(&self, name: &str, current_sheet: usize) -> Option<&str> {
        let find = |scope: Option<usize>| {
            self.defined_names
                .iter()
                .find(|d| d.scope == scope && d.name.eq_ignore_ascii_case(name))
                .map(|d| d.formula.as_str())
        };
        find(Some(current_sheet)).or_else(|| find(None))
    }
}

// ---------------------------------------------------------------------------
// Styles (display subset)
// ---------------------------------------------------------------------------

/// What a number format means for display. Classified once at load from the
/// builtin numFmtId or the custom format code; the original id is preserved
/// on the cell's xf, so files round-trip regardless of how well we classify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NumFmt {
    #[default]
    General,
    /// Fixed decimals; `thousands` adds a separator ("#,##0.00").
    Number {
        decimals: u8,
        thousands: bool,
    },
    Percent {
        decimals: u8,
    },
    Scientific,
    Date,
    Time,
    DateTime,
    /// "@" — display as entered.
    Text,
}

/// Horizontal cell alignment (the subset xlsxy authors/renders).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Excel's "General": numbers right, text left.
    #[default]
    General,
    Left,
    Center,
    Right,
}

impl Align {
    /// The `horizontal="…"` attribute value, or `None` for General.
    pub fn attr(self) -> Option<&'static str> {
        match self {
            Align::General => None,
            Align::Left => Some("left"),
            Align::Center => Some("center"),
            Align::Right => Some("right"),
        }
    }

    pub fn from_attr(s: &str) -> Align {
        match s {
            "left" => Align::Left,
            "center" => Align::Center,
            "right" => Align::Right,
            _ => Align::General,
        }
    }
}

/// One resolved cell format (`<xf>` joined with its font): everything the
/// terminal renders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Xf {
    pub numfmt: NumFmt,
    /// The raw format code, when known — rendered by [`crate::numfmt`];
    /// [`NumFmt`] classification is the fallback (and drives alignment).
    pub code: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// Font color as (r, g, b) when the file gives a concrete RGB.
    pub color: Option<(u8, u8, u8)>,
    /// Solid fill (background) color as (r, g, b), when set.
    pub fill: Option<(u8, u8, u8)>,
    pub align: Align,
    /// Font size in points (`None` = the default 11).
    pub font_size: Option<f64>,
    /// Font family name (`None` = the default Calibri).
    pub font_name: Option<String>,
    /// A thin box border around each cell, when set.
    pub border: bool,
    /// Wrap long text onto multiple lines within the cell (`<alignment
    /// wrapText="1">`). Rendered as wrapped lines; drives auto-fit row height.
    pub wrap: bool,
    /// The cell's text was entered with a leading apostrophe (`quotePrefix="1"`):
    /// the value is text even where it reads as a number, and the editor shows
    /// the apostrophe again.
    pub quote_prefix: bool,
    /// The `<cellXfs>` index this xf was loaded from, kept by every copy an
    /// edit derives from it. Save writes a derived xf by reusing that source
    /// element's font, fill, border, number format, alignment and protection
    /// wherever the modeled fields still match it, so what the model does not
    /// carry (underline, real borders, pattern fills, vertical alignment,
    /// indent, protection) survives an edit. `None` for an xf built from
    /// scratch.
    pub loaded_from: Option<u32>,
}

impl Xf {
    /// Set the number-format code, keeping the [`NumFmt`] classification in
    /// step with it (`None` is General). Code that reads either half — the
    /// entry rules, the `####` check, the classified display fallback —
    /// then agrees.
    pub fn set_code(&mut self, code: Option<String>) {
        self.numfmt = code
            .as_deref()
            .map(classify_format_code)
            .unwrap_or(NumFmt::General);
        self.code = code;
    }
}

/// A differential format (`<dxf>`) referenced by a conditional-formatting rule.
/// Only the properties the rule overrides are `Some`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dxf {
    pub fill: Option<(u8, u8, u8)>,
    pub color: Option<(u8, u8, u8)>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

/// One conditional-formatting rule (`<cfRule>`).
#[derive(Clone, Debug)]
pub struct CfRule {
    pub kind: CfKind,
    /// Index into [`Styles::dxfs`], applied when the rule matches.
    pub dxf_id: Option<usize>,
    /// Excel `priority`: lower = higher precedence.
    pub priority: i32,
}

/// The kind of a conditional-formatting rule that the engine can evaluate.
#[derive(Clone, Debug)]
pub enum CfKind {
    /// `cellIs` with an operator and one or two operand formulas.
    CellIs { op: String, formulas: Vec<String> },
    /// `expression`: a formula truthy when the rule applies.
    Expression { formula: String },
    /// Anything else (colorScale/dataBar/iconSet/top10/…) — not evaluated.
    /// Its `<formula>` children are kept so structural edits can move them.
    Other { formulas: Vec<String> },
}

impl CfRule {
    /// The rule's formulas in document order, whatever its kind.
    pub fn formulas(&self) -> Vec<&String> {
        match &self.kind {
            CfKind::CellIs { formulas, .. } | CfKind::Other { formulas } => {
                formulas.iter().collect()
            }
            CfKind::Expression { formula } => vec![formula],
        }
    }

    /// [`Self::formulas`], mutably.
    pub fn formulas_mut(&mut self) -> Vec<&mut String> {
        match &mut self.kind {
            CfKind::CellIs { formulas, .. } | CfKind::Other { formulas } => {
                formulas.iter_mut().collect()
            }
            CfKind::Expression { formula } => vec![formula],
        }
    }
}

/// A conditional-formatting block: its `rules` apply over `ranges` (`sqref`).
#[derive(Clone, Debug, Default)]
pub struct CondFormat {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    pub rules: Vec<CfRule>,
    /// The ordinal of this block's element among the worksheet's top-level
    /// `<conditionalFormatting>` children, so a save can write a structural
    /// edit's move back to it. `None` for a block the part doesn't hold (an
    /// x14 one in `extLst`, one built in memory).
    pub ix: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Styles {
    /// Indexed by a cell's `s=` attribute. Index 0 (default style) is always
    /// present after load.
    pub xfs: Vec<Xf>,
    /// Differential formats (`<dxfs>`) referenced by conditional formatting.
    pub dxfs: Vec<Dxf>,
}

impl Styles {
    pub fn xf(&self, idx: u32) -> Xf {
        self.xfs.get(idx as usize).cloned().unwrap_or_default()
    }

    /// Return the index of an `xf` equal to `xf`, appending it if new. Used by
    /// the editor to author cell formatting without duplicating styles.
    pub fn intern(&mut self, xf: Xf) -> u32 {
        if let Some(i) = self.xfs.iter().position(|x| *x == xf) {
            return i as u32;
        }
        self.xfs.push(xf);
        (self.xfs.len() - 1) as u32
    }
}

/// Classify a number-format code string (custom formats). Sections are split
/// on `;` and the first (positive) section drives the classification. Quoted
/// literals, `[...]` blocks and escaped chars are ignored while scanning.
pub fn classify_format_code(code: &str) -> NumFmt {
    let section = code.split(';').next().unwrap_or("");
    let mut bare = String::new();
    let mut chars = section.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                }
            }
            '[' => {
                for q in chars.by_ref() {
                    if q == ']' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                let _ = chars.next();
            }
            _ => bare.push(ch.to_ascii_lowercase()),
        }
    }
    if bare.trim() == "general" || bare.is_empty() {
        return NumFmt::General;
    }
    if bare.contains('@') {
        return NumFmt::Text;
    }
    let has_date = bare.contains('y') || bare.contains('d');
    let has_time = bare.contains('h') || bare.contains('s');
    // 'm' is ambiguous (month/minute) — with neither y/d nor h/s present and no
    // digit placeholders, treat a lone m-stream as months.
    let has_m = bare.contains('m');
    if has_date && has_time {
        return NumFmt::DateTime;
    }
    if has_date || (has_m && !has_time && !bare.contains('0') && !bare.contains('#')) {
        return NumFmt::Date;
    }
    if has_time {
        return NumFmt::Time;
    }
    if bare.contains("e+") || bare.contains("e-") {
        return NumFmt::Scientific;
    }
    let decimals = match bare.find('.') {
        Some(dot) => bare[dot + 1..]
            .bytes()
            .take_while(|&b| b == b'0' || b == b'#' || b == b'?')
            .count() as u8,
        None => 0,
    };
    if bare.contains('%') {
        return NumFmt::Percent { decimals };
    }
    if bare.contains('0') || bare.contains('#') {
        return NumFmt::Number {
            decimals,
            thousands: bare.contains(','),
        };
    }
    NumFmt::General
}

/// Classify a builtin numFmtId (ECMA-376 §18.8.30). Ids ≥ 164 are custom and
/// must be classified from their code with [`classify_format_code`].
pub fn classify_builtin(id: u32) -> NumFmt {
    match id {
        1 => NumFmt::Number {
            decimals: 0,
            thousands: false,
        },
        2 => NumFmt::Number {
            decimals: 2,
            thousands: false,
        },
        3 | 37 | 38 => NumFmt::Number {
            decimals: 0,
            thousands: true,
        },
        4 | 39 | 40 | 44 => NumFmt::Number {
            decimals: 2,
            thousands: true,
        },
        9 => NumFmt::Percent { decimals: 0 },
        10 => NumFmt::Percent { decimals: 2 },
        11 | 48 => NumFmt::Scientific,
        14..=17 => NumFmt::Date,
        18..=21 | 45..=47 => NumFmt::Time,
        22 => NumFmt::DateTime,
        49 => NumFmt::Text,
        _ => NumFmt::General,
    }
}

// ---------------------------------------------------------------------------
// Date serials
// ---------------------------------------------------------------------------

/// Days from 1970-01-01 (civil calendar), Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 1970-01-01-based day count → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// An Excel date serial expanded to calendar parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateParts {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Excel serial number → calendar parts, honoring the workbook's date system.
///
/// 1900 system: serial 1 = 1900-01-01, with Excel's deliberate Lotus bug
/// (a phantom 1900-02-29 at serial 60). We use the standard workaround:
/// epoch 1899-12-30 for serials ≥ 61, one day later below that.
pub fn serial_to_parts(serial: f64, date1904: bool) -> Option<DateParts> {
    // Reject non-finite, negative, and out-of-Excel-range serials. The upper
    // bound is Excel's own ceiling (9999-12-31 ≈ serial 2,958,465); without it
    // a huge value like 1e19 would overflow the civil-date arithmetic below.
    if !serial.is_finite() || !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let days = serial.floor() as i64;
    let unix_days = if date1904 {
        days + days_from_civil(1904, 1, 1)
    } else {
        // 1899-12-30 epoch = unix day -25569.
        days - 25_569 + if days < 61 { 1 } else { 0 }
    };
    let (year, month, day) = civil_from_days(unix_days);
    // Round to the nearest second to hide float dust (Excel does likewise).
    let mut secs = (serial.fract() * 86_400.0).round() as u32;
    if secs >= 86_400 {
        secs = 86_399;
    }
    Some(DateParts {
        year,
        month,
        day,
        hour: secs / 3600,
        minute: (secs % 3600) / 60,
        second: secs % 60,
    })
}

/// Calendar date (+ optional time of day in seconds) → Excel serial.
pub fn parts_to_serial(y: i64, m: u32, d: u32, day_secs: u32, date1904: bool) -> f64 {
    let unix_days = days_from_civil(y, m, d);
    let days = if date1904 {
        unix_days - days_from_civil(1904, 1, 1)
    } else {
        let s = unix_days + 25_569;
        if s < 61 { s - 1 } else { s }
    };
    days as f64 + day_secs as f64 / 86_400.0
}

// ---------------------------------------------------------------------------
// Value display
// ---------------------------------------------------------------------------

/// Format a number the way Excel's General format does: round to 15
/// significant digits (hiding IEEE-754 noise), integers without a decimal
/// point, scientific notation only at extreme magnitudes.
pub fn fmt_general(n: f64) -> String {
    if !n.is_finite() {
        return "#NUM!".to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    let a = n.abs();
    if !(1e-10..1e21).contains(&a) {
        return fmt_scientific(n, 5);
    }
    let r = round_sig(n, 15);
    if r == r.trunc() && r.abs() < 1e16 {
        format!("{}", r as i64)
    } else {
        // Shortest representation that round-trips the rounded value.
        let mut s = format!("{r}");
        if s.contains('e') {
            s = format!("{r:.15}");
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

/// The most characters Excel's General format shows a number in, however
/// wide the column: 12345678901 is shown whole, 123456789012 as
/// `1.23457E+11`, 0.123456789012345 as `0.123456789`.
pub const GENERAL_MAX_CHARS: usize = 11;

/// A General number as a grid cell `width` characters wide shows it:
/// [`fmt_general_fit`] within General's own [`GENERAL_MAX_CHARS`], and `#`s
/// across the cell when not even scientific notation fits. Display only:
/// the editor, a copy and the saved value keep every digit.
pub fn fmt_general_cell(n: f64, width: usize) -> String {
    let width = width.max(1);
    fmt_general_fit(n, width.min(GENERAL_MAX_CHARS)).unwrap_or_else(|| "#".repeat(width))
}

/// A General number as a cell `width` characters wide shows it: in full
/// when it fits, else with fewer decimals (`0.333333`), else in scientific
/// notation with as many mantissa digits as fit (`1.23E+08`). `None` when
/// not even that fits (the cell shows `#`s).
pub fn fmt_general_fit(n: f64, width: usize) -> Option<String> {
    let full = fmt_general(n);
    if full.chars().count() <= width {
        return Some(full);
    }
    if !n.is_finite() {
        return None;
    }
    // Fewer decimals, while the integer part fits and something is left of
    // the value (0.0000001 must not round to 0).
    let int_len = format!("{}", n.trunc().abs() as u128).len() + usize::from(n < 0.0);
    if int_len <= width && n.abs() >= 1e-10 {
        let decimals = width.saturating_sub(int_len + 1);
        let mut s = format!("{n:.decimals$}");
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        let lost = s
            .trim_start_matches('-')
            .chars()
            .all(|c| c == '0' || c == '.');
        if s.chars().count() <= width && !lost {
            return Some(s);
        }
    }
    // Scientific, trailing mantissa zeros dropped as General does.
    // Excel writes at least two exponent digits (E+08).
    for decimals in (0..width).rev() {
        let raw = format!("{n:.decimals$e}");
        let (mant, exp) = raw.split_once('e')?;
        let exp: i32 = exp.parse().ok()?;
        let mant = if mant.contains('.') {
            mant.trim_end_matches('0').trim_end_matches('.')
        } else {
            mant
        };
        let sign = if exp < 0 { '-' } else { '+' };
        let s = format!("{mant}E{sign}{:02}", exp.abs());
        if s.chars().count() <= width {
            return Some(s);
        }
    }
    None
}

/// Excel-style scientific notation: 1.5E+21, 2.00E-05.
fn fmt_scientific(n: f64, decimals: usize) -> String {
    let s = format!("{:.*E}", decimals, n);
    // Rust writes "1.5E21" / "1.5E-21"; Excel writes "1.5E+21" / "1.5E-21".
    match s.find('E') {
        Some(e) if s.as_bytes().get(e + 1) != Some(&b'-') => {
            format!("{}E+{}", &s[..e], &s[e + 1..])
        }
        _ => s,
    }
}

/// Round to `digits` significant decimal digits.
fn round_sig(n: f64, digits: i32) -> f64 {
    if n == 0.0 || !n.is_finite() {
        return n;
    }
    let mag = n.abs().log10().floor() as i32;
    let factor = 10f64.powi(digits - 1 - mag);
    (n * factor).round() / factor
}

/// Insert thousands separators into the integer part of a formatted number.
fn add_thousands(s: &str) -> String {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", s),
    };
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    let mut out = String::with_capacity(s.len() + int.len() / 3);
    out.push_str(sign);
    let bytes = int.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

/// Render a cell value for display using its resolved number format.
pub fn format_value(value: &CellValue, fmt: NumFmt, date1904: bool) -> String {
    match value {
        CellValue::Empty => String::new(),
        CellValue::Text(s) => s.clone(),
        CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        CellValue::Error(e) => e.clone(),
        CellValue::Number(n) => match fmt {
            NumFmt::General | NumFmt::Text => fmt_general(*n),
            NumFmt::Number {
                decimals,
                thousands,
            } => {
                let s = format!("{:.*}", decimals as usize, n);
                if thousands { add_thousands(&s) } else { s }
            }
            NumFmt::Percent { decimals } => {
                format!("{:.*}%", decimals as usize, n * 100.0)
            }
            NumFmt::Scientific => fmt_scientific(*n, 2),
            NumFmt::Date => match serial_to_parts(*n, date1904) {
                Some(p) => format!("{:04}-{:02}-{:02}", p.year, p.month, p.day),
                None => fmt_general(*n),
            },
            NumFmt::Time => match serial_to_parts(*n, date1904) {
                Some(p) => format!("{:02}:{:02}:{:02}", p.hour, p.minute, p.second),
                None => fmt_general(*n),
            },
            NumFmt::DateTime => match serial_to_parts(*n, date1904) {
                Some(p) => format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}",
                    p.year, p.month, p.day, p.hour, p.minute
                ),
                None => fmt_general(*n),
            },
        },
    }
}

/// What a cell shows for a date or time it cannot display. The grids widen
/// it to fill the column, as Excel does.
pub const UNREPRESENTABLE: &str = "########";

/// Is `value` a number shown through a date/time format that cannot display
/// it (negative, or past 9999-12-31)? Excel fills such a cell with `#`.
pub fn date_unrepresentable(xf: &Xf, value: &CellValue, date1904: bool) -> bool {
    let CellValue::Number(n) = value else {
        return false;
    };
    if !n.is_finite() || serial_to_parts(*n, date1904).is_some() {
        return false;
    }
    // The code decides when there is one; the classification only stands in
    // for a code-less xf, so a stale pair cannot mis-drive the check.
    let class = match xf.code.as_deref() {
        Some(code) => match crate::numfmt::parse_format(code) {
            Some(fmt) => return fmt.is_date_for(*n),
            None => classify_format_code(code),
        },
        None => xf.numfmt,
    };
    matches!(class, NumFmt::Date | NumFmt::Time | NumFmt::DateTime)
}

/// Render a cell value through its full style: the real format-code runtime
/// when the code is known and renderable, the classified approximation
/// otherwise. A date or time that cannot be shown is [`UNREPRESENTABLE`].
pub fn format_with(xf: &Xf, value: &CellValue, date1904: bool) -> String {
    if date_unrepresentable(xf, value, date1904) {
        return UNREPRESENTABLE.to_string();
    }
    if let Some(code) = &xf.code {
        if let Some(fmt) = crate::numfmt::parse_format(code) {
            match value {
                CellValue::Number(n) => {
                    if let Some(s) = fmt.format_number(*n, date1904) {
                        return s;
                    }
                }
                CellValue::Text(s) => return fmt.format_text(s),
                _ => {}
            }
        }
    }
    format_value(value, xf.numfmt, date1904)
}

/// Export one sheet as CSV text the way Excel writes it (display values,
/// formulas as their cached results, CR LF after each record, LF inside a
/// quoted field). The file's encoding (Excel's UTF-8 BOM) is the writer's:
/// see [`crate::textio::encode`].
pub fn sheet_to_csv(sheet: &Sheet, styles: &Styles, date1904: bool) -> String {
    crate::textio::sheet_text(sheet, styles, date1904, ',')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_blank_the_spill_output_of_a_live_anchor() {
        // #777 r1: C1 spills C1:C3 and is not among the keys; E1 is frozen.
        let mut sheet = Sheet::default();
        let anchor = |src: &str| Cell {
            spill: Some((3, 1)),
            ..Cell::formula(src)
        };
        sheet.set_cell(0, 2, anchor("SEQUENCE(3)"));
        sheet.set_cell(
            1,
            2,
            Cell {
                style: 2,
                ..Cell::number(2.0)
            },
        );
        sheet.set_cell(2, 2, Cell::number(3.0));
        sheet.set_cell(0, 4, anchor("PIVOTBY(A1,4)"));
        sheet.set_cell(1, 4, Cell::number(8.0));
        sheet.set_cell(3, 2, Cell::number(4.0)); // below the spill
        let keys = [(0, 2), (1, 2), (2, 2), (3, 2), (1, 4), (5, 5)];
        let mut asked = Vec::new();
        let frozen = |r, c| (r, c) == (0, 4);
        let snap = snapshot_cells(&sheet, &keys, |r, c| {
            asked.push((r, c));
            frozen(r, c)
        });
        assert_eq!(snap[0].as_ref(), sheet.cell(0, 2)); // the anchor itself
        assert_eq!(
            snap[1],
            Some(Cell {
                style: 2,
                ..Cell::default()
            })
        );
        assert_eq!(snap[2], Some(Cell::default()));
        assert_eq!(snap[3], Some(Cell::number(4.0)));
        assert_eq!(snap[4], Some(Cell::number(8.0))); // a frozen anchor's value
        assert_eq!(snap[5], None);
        // Asked once per anchor over a key, never of the others.
        asked.sort();
        assert_eq!(asked, vec![(0, 2), (0, 4)]);
    }

    #[test]
    fn frozen_spill_keys_adds_the_block_of_a_frozen_anchor() {
        // #837: C1 spills C1:C3 live; E1 spills E1:F2 and is frozen: F1
        // holds 8, E2 9, F2 a styled blank.
        let mut sheet = Sheet::default();
        let anchor = |src: &str, ext| Cell {
            spill: Some(ext),
            ..Cell::formula(src)
        };
        sheet.set_cell(0, 2, anchor("SEQUENCE(3)", (3, 1)));
        sheet.set_cell(1, 2, Cell::number(2.0));
        sheet.set_cell(2, 2, Cell::number(3.0));
        sheet.set_cell(0, 4, anchor("PIVOTBY(A1,4)", (2, 2)));
        sheet.set_cell(0, 5, Cell::number(8.0));
        sheet.set_cell(1, 4, Cell::number(9.0));
        sheet.set_cell(
            1,
            5,
            Cell {
                style: 3,
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 6, Cell::number(1.0)); // right of the block
        let asked = std::cell::RefCell::new(Vec::new());
        let frozen = |r, c| {
            asked.borrow_mut().push((r, c));
            (r, c) == (0, 4)
        };
        // A live anchor, a member of its spill, an empty cell: nothing added.
        for keys in [vec![(0, 2)], vec![(1, 2)], vec![(7, 7)]] {
            assert_eq!(frozen_spill_keys(&sheet, &keys, frozen), keys);
        }
        // A frozen anchor adds its held values once, after the keys,
        // whatever of them the keys already name.
        assert_eq!(
            frozen_spill_keys(&sheet, &[(1, 4), (0, 4), (0, 4), (3, 3)], frozen),
            vec![(1, 4), (0, 4), (0, 4), (3, 3), (0, 5)]
        );
        // An anchor whose values the keys all name is not asked.
        let all = [(0, 4), (0, 5), (1, 4)];
        assert_eq!(frozen_spill_keys(&sheet, &all, frozen), all);
        assert_eq!(*asked.borrow(), vec![(0, 2), (0, 2), (0, 4)]);
        asked.borrow_mut().clear();
        // A key inside a frozen block adds the anchor, then its values; one
        // inside a live spill adds nothing; the frozen anchor is asked once.
        assert_eq!(
            frozen_spill_keys(&sheet, &[(1, 5), (2, 2), (1, 4)], frozen),
            vec![(1, 5), (2, 2), (1, 4), (0, 4), (0, 5)]
        );
        assert_eq!(*asked.borrow(), vec![(0, 4), (0, 2)]);
    }

    #[test]
    fn frozen_spill_keys_walks_held_cells_not_a_huge_extent() {
        // #837 r1: a loaded `ref` sizes the extent with no bound.
        let mut sheet = Sheet::default();
        for (ext, at) in [((1_048_576, 1), (0, 0)), ((u32::MAX, u32::MAX), (5, 3))] {
            sheet.set_cell(
                at.0,
                at.1,
                Cell {
                    spill: Some(ext),
                    ..Cell::formula("PIVOTBY(A1,4)")
                },
            );
        }
        sheet.set_cell(9, 0, Cell::number(1.0));
        sheet.set_cell(1_048_575, 0, Cell::number(2.0));
        sheet.set_cell(7, 900, Cell::number(3.0));
        sheet.set_cell(1_048_575, 16_383, Cell::number(4.0));
        let keys = frozen_spill_keys(&sheet, &[(0, 0), (5, 3)], |_, _| true);
        assert_eq!(
            keys,
            vec![
                (0, 0),
                (5, 3),
                (9, 0),
                (1_048_575, 0),
                (7, 900),
                (1_048_575, 16_383)
            ]
        );
    }

    #[test]
    fn an_undisplayable_date_or_time_is_a_hash_run() {
        let coded = |code: &str| Xf {
            numfmt: classify_format_code(code),
            code: Some(code.to_string()),
            ..Xf::default()
        };
        let neg = CellValue::Number(-1.0);
        for code in ["yyyy-mm-dd", "m/d/yyyy", "h:mm", "[h]:mm", "m/d/yyyy h:mm"] {
            assert_eq!(
                format_with(&coded(code), &neg, false),
                UNREPRESENTABLE,
                "{code}"
            );
        }
        // Past 9999-12-31 too.
        let huge = CellValue::Number(3_000_000.0);
        assert_eq!(
            format_with(&coded("m/d/yyyy"), &huge, false),
            UNREPRESENTABLE
        );
        // A classified date with no renderable code.
        let classified = Xf {
            numfmt: NumFmt::Date,
            ..Xf::default()
        };
        assert!(date_unrepresentable(&classified, &neg, false));
        // General still shows -1, a representable date still renders, and a
        // format whose negative section is not a date shows the number.
        assert_eq!(format_with(&Xf::default(), &neg, false), "-1");
        assert_eq!(
            format_with(&coded("yyyy-mm-dd"), &CellValue::Number(45306.0), false),
            "2024-01-15"
        );
        assert_eq!(format_with(&coded("yyyy-mm-dd;0"), &neg, false), "1");
        assert!(!date_unrepresentable(
            &coded("yyyy-mm-dd"),
            &CellValue::Text("x".into()),
            false
        ));
    }

    #[test]
    fn range_readers_take_a_column_of_cells_as_numbers_or_labels() {
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("B1", Cell::text("Qty")),
            ("B2", Cell::number(2.0)),
            ("B3", Cell::text("n/a")),
            ("B4", Cell::number(5.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        // B2:B4 as a series: text and blanks plot as zero, in row order.
        assert_eq!(range_numbers(&sh, (1, 1, 3, 1)), vec![2.0, 0.0, 5.0]);
        // The same cells as labels: numbers render the way the grid shows them.
        assert_eq!(range_labels(&sh, (1, 1, 3, 1)), vec!["2", "n/a", "5"]);
        // A single cell is the usual case for a series name.
        assert_eq!(range_labels(&sh, (0, 1, 0, 1)), vec!["Qty"]);
        // Cells that were never set read as empty rather than panicking.
        assert_eq!(range_numbers(&sh, (10, 10, 10, 11)), vec![0.0, 0.0]);
        assert_eq!(range_labels(&sh, (10, 10, 10, 10)), vec![""]);
    }

    #[test]
    fn a_chart_is_column_oriented_unless_told_otherwise() {
        // Orientation is not stored in the file, so every chart that predates it
        // — which is every chart in every existing workbook — must read as
        // column-oriented. Flipping this default would silently transpose them
        // all, so pin it here rather than trusting `bool::default()` to stay put.
        assert!(!ChartData::default().by_row);

        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        // And a chart built from a range is column-oriented too.
        let cd = chart_from_range(&sh, "Budget", (0, 0, 1, 1), "column", false).expect("chart");
        assert!(!cd.by_row);
    }

    #[test]
    fn chart_from_range_picks_labels_and_numeric_series() {
        // A1:C3 — a label column and two numeric columns under a header row.
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Price")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(1199.0)),
            ("A3", Cell::text("Dock")),
            ("B3", Cell::number(5.0)),
            ("C3", Cell::number(179.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        let cd = chart_from_range(&sh, "Budget", (0, 0, 2, 2), "column", false).expect("chart");
        assert_eq!(cd.title, "Item"); // the label column's header names the chart
        assert_eq!(cd.categories, vec!["Laptop", "Dock"]);
        assert_eq!(cd.series.len(), 2);
        assert_eq!(cd.series[0].name, "Qty");
        assert_eq!(cd.series[0].values, vec![2.0, 5.0]);
        assert_eq!(cd.series[1].values, vec![1199.0, 179.0]);
        // Each series remembers its column, so a save can write live refs.
        assert_eq!((cd.series[0].col, cd.series[1].col), (Some(1), Some(2)));
        let src = cd.source.expect("source");
        assert_eq!(
            (src.sheet.as_str(), src.range, src.cat_col),
            ("Budget", (0, 0, 2, 2), 0)
        );

        // A range with nothing numeric in it can't be plotted.
        assert!(chart_from_range(&sh, "Budget", (0, 0, 2, 0), "column", false).is_none());
        // Neither can a header row on its own.
        assert!(chart_from_range(&sh, "Budget", (0, 0, 0, 2), "column", false).is_none());
        // The label column was found, so its cells name the categories.
        assert_eq!(
            cd.categories_ref.map(|s| s.range),
            Some((1, 0, 2, 0)),
            "categories come from the label column"
        );
    }

    /// The Overview's worked example: one row per item, a header row of column
    /// headings.
    fn overview_sheet() -> Sheet {
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Unit price")),
            ("D1", Cell::text("Total")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(1199.0)),
            ("D2", Cell::number(2398.0)),
            ("A3", Cell::text("Monitor")),
            ("B3", Cell::number(4.0)),
            ("C3", Cell::number(249.5)),
            ("D3", Cell::number(998.0)),
            ("A4", Cell::text("Keyboard")),
            ("B4", Cell::number(6.0)),
            ("C4", Cell::number(39.99)),
            ("D4", Cell::number(239.94)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        sh
    }

    #[test]
    fn chart_from_rows_picks_labels_and_numeric_series() {
        let sh = overview_sheet();
        let cd = chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", true).expect("chart");
        assert!(cd.by_row);
        // The label row's first cell names the chart, as the label column's
        // header does the other way round.
        assert_eq!(cd.title, "Item");
        assert_eq!(cd.categories, vec!["Qty", "Unit price", "Total"]);
        assert_eq!(cd.series.len(), 3);
        let names: Vec<&str> = cd.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Laptop", "Monitor", "Keyboard"]);
        assert_eq!(cd.series[0].values, vec![2.0, 1199.0, 2398.0]);
        assert_eq!(cd.series[2].values, vec![6.0, 39.99, 239.94]);
        // A row series names no column — `col` feeds column-shaped decisions
        // only, and a row index there would be silently wrong.
        assert!(cd.series.iter().all(|s| s.col.is_none()));
        // Each slot's ref is the row rectangle / the label cell left of it.
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|s| s.range),
            Some((1, 1, 1, 3))
        );
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("Budget!$A$2"));
        assert_eq!(cd.series[2].name_ref.as_deref(), Some("Budget!$A$4"));
        assert_eq!(
            cd.series[2].values_ref.as_ref().map(|s| s.to_ref()),
            Some("Budget!$B$4:$D$4".to_string())
        );
        // The label row was found, so its cells name the categories.
        assert_eq!(
            cd.categories_ref.as_ref().map(|s| s.range),
            Some((0, 1, 0, 3)),
            "categories come from the label row"
        );
        let src = cd.source.expect("source");
        assert_eq!(
            (src.sheet.as_str(), src.range, src.cat_col),
            ("Budget", (0, 0, 3, 3), 0)
        );

        // A range with nothing numeric across a row can't be plotted.
        let cd = chart_from_range(&sh, "Budget", (0, 0, 3, 0), "column", true);
        assert!(cd.is_none(), "a label column on its own plots nothing");
        // Nor can a label column plus a row of headings, with no numbers.
        assert!(chart_from_range(&sh, "Budget", (0, 0, 0, 3), "column", true).is_none());
    }

    #[test]
    fn the_two_orientations_of_one_range_are_transposes() {
        let sh = overview_sheet();
        let by_col = chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", false).expect("cols");
        let by_row = chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", true).expect("rows");

        // Series and categories swap places.
        let col_names: Vec<&str> = by_col.series.iter().map(|s| s.name.as_str()).collect();
        let row_names: Vec<&str> = by_row.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(col_names, by_row.categories.iter().collect::<Vec<_>>());
        assert_eq!(row_names, by_col.categories.iter().collect::<Vec<_>>());

        // And the numbers are the same grid, read the other way.
        for (i, s) in by_col.series.iter().enumerate() {
            for (j, v) in s.values.iter().enumerate() {
                assert_eq!(*v, by_row.series[j].values[i], "cell ({j},{i})");
            }
        }
        // Both name the same box, and only the flag differs.
        assert_eq!(
            by_col.source.as_ref().map(|s| s.range),
            by_row.source.as_ref().map(|s| s.range)
        );
        assert!(!by_col.by_row && by_row.by_row);
    }

    #[test]
    fn an_all_numeric_row_table_writes_literal_categories_not_a_plotted_row() {
        // The row analogue of `Year | Sales`: every row is numeric, so the
        // fallback category row is itself plotted. Naming it in `<c:cat>` would
        // label the numbers with themselves.
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Year")),
            ("B1", Cell::number(2024.0)),
            ("C1", Cell::number(2025.0)),
            ("A2", Cell::text("Sales")),
            ("B2", Cell::number(10.0)),
            ("C2", Cell::number(20.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        let cd = chart_from_range(&sh, "Data", (0, 0, 1, 2), "column", true).expect("chart");
        assert_eq!(cd.categories_ref, None);
        // Both rows plot; the first also supplies the labels, as literals.
        let names: Vec<&str> = cd.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Year", "Sales"]);
        assert_eq!(cd.categories, vec!["2024", "2025"]);
    }

    /// A cell must read the same however a chart path reaches it. Deriving a
    /// name through `chart_from_range` and typing the same cell into the
    /// panel's SERIES NAME field (which goes through `range_labels`) used to
    /// disagree on a boolean — Rust's `true` against Excel's `TRUE` — so
    /// `Switch Row/Column` respelled a name it had no business touching.
    #[test]
    fn a_derived_label_reads_the_same_as_the_one_a_field_would_show() {
        let boolean = |b: bool| Cell {
            value: CellValue::Bool(b),
            ..Cell::default()
        };
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Flag")),
            ("B1", Cell::text("Qty")),
            ("A2", boolean(true)),
            ("B2", Cell::number(2.0)),
            ("A3", boolean(false)),
            ("B3", Cell::number(4.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        // Column reading: the boolean cells are the category labels.
        let by_col = chart_from_range(&sh, "Data", (0, 0, 2, 1), "column", false).expect("cols");
        assert_eq!(by_col.categories, range_labels(&sh, (1, 0, 2, 0)));
        assert_eq!(by_col.categories, vec!["TRUE", "FALSE"]);
        // Row reading: the same cells name the series.
        let by_row = chart_from_range(&sh, "Data", (0, 0, 2, 1), "column", true).expect("rows");
        let names: Vec<String> = by_row.series.iter().map(|s| s.name.clone()).collect();
        // Row 0 holds `Qty`, so it is the label row, not a series; the two
        // boolean cells below it name the two series.
        assert_eq!(names, range_labels(&sh, (1, 0, 2, 0)));
        assert_eq!(names, vec!["TRUE", "FALSE"]);
    }

    #[test]
    fn an_all_numeric_table_writes_literal_categories_not_a_plotted_column() {
        // `Year | Sales` has no label column, so `cat_col` falls back to the
        // first — which is itself plotted. Naming it in `<c:cat>` would label
        // the numbers with themselves, on top of the bogus Year series.
        let mut sh = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Year")),
            ("B1", Cell::text("Sales")),
            ("A2", Cell::number(2024.0)),
            ("B2", Cell::number(10.0)),
            ("A3", Cell::number(2025.0)),
            ("B3", Cell::number(20.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        let cd = chart_from_range(&sh, "Data", (0, 0, 2, 1), "column", false).expect("chart");
        assert_eq!(cd.categories_ref, None);
        // The labels are still there, as literals — the writer emits `<c:strLit>`.
        assert_eq!(cd.categories, vec!["2024", "2025"]);
        let out = crate::xlsx::chart_space_xml(&cd);
        assert!(out.contains("<c:cat><c:strLit"), "{out}");
        assert!(!out.contains("<c:cat><c:strRef"), "{out}");
    }

    #[test]
    fn col_names_round_trip() {
        for (idx, name) in [
            (0, "A"),
            (25, "Z"),
            (26, "AA"),
            (51, "AZ"),
            (52, "BA"),
            (701, "ZZ"),
            (702, "AAA"),
            (16_383, "XFD"),
        ] {
            assert_eq!(col_name(idx), name, "col_name({idx})");
            assert_eq!(
                parse_col(name),
                Some((idx, name.len())),
                "parse_col({name})"
            );
        }
    }

    #[test]
    fn cell_names_round_trip() {
        assert_eq!(cell_name(0, 0), "A1");
        assert_eq!(cell_name(11, 1), "B12");
        assert_eq!(parse_cell_name("B12"), Some((11, 1)));
        assert_eq!(parse_cell_name("$C$4"), Some((3, 2)));
        assert_eq!(
            parse_cell_name("xfd1048576"),
            Some((MAX_ROWS - 1, MAX_COLS - 1))
        );
        assert_eq!(parse_cell_name("A0"), None);
        assert_eq!(parse_cell_name("1A"), None);
        assert_eq!(parse_cell_name(""), None);
    }

    #[test]
    fn range_parse_normalizes() {
        assert_eq!(parse_range_name("B2:A1"), Some((0, 0, 1, 1)));
        assert_eq!(parse_range_name("C3"), Some((2, 2, 2, 2)));
    }

    #[test]
    fn used_size_and_clear() {
        let mut s = Sheet::default();
        s.set_cell(4, 2, Cell::number(1.0));
        s.set_cell(1, 7, Cell::text("x"));
        assert_eq!(s.used_size(), (5, 8));
        s.clear_cell(4, 2);
        assert_eq!(s.used_size(), (2, 8));
        // Clearing a styled cell keeps the style marker.
        s.set_cell(
            0,
            0,
            Cell {
                style: 3,
                ..Cell::number(9.0)
            },
        );
        s.clear_cell(0, 0);
        assert_eq!(s.cell(0, 0).map(|c| c.style), Some(3));
        assert!(s.cell(0, 0).unwrap().is_blank());
    }

    #[test]
    fn col_width_split() {
        let mut s = Sheet::default();
        s.col_defs.push(ColDef {
            min: 0,
            max: 4,
            width: Some(12.0),
            attrs: String::new(),
        });
        s.set_col_width(2, 20.0);
        assert_eq!(s.col_width(1), 12.0);
        assert_eq!(s.col_width(2), 20.0);
        assert_eq!(s.col_width(3), 12.0);
        assert_eq!(s.col_width(9), DEFAULT_COL_WIDTH);
    }

    #[test]
    fn row_and_col_hidden_flags() {
        let mut s = Sheet::default();
        s.row_attrs.insert(3, "ht=\"15\" hidden=\"1\"".into());
        s.row_attrs.insert(4, "hidden=\"0\"".into());
        s.row_attrs.insert(5, "customHeight=\"1\"".into());
        assert!(s.row_hidden(3));
        assert!(!s.row_hidden(4)); // hidden="0" is not hidden
        assert!(!s.row_hidden(5));
        assert!(!s.row_hidden(99)); // no attrs at all

        s.col_defs.push(ColDef {
            min: 2,
            max: 4,
            width: None,
            attrs: "hidden=\"1\"".into(),
        });
        assert!(s.col_hidden(2) && s.col_hidden(4));
        assert!(!s.col_hidden(1) && !s.col_hidden(5));
    }

    #[test]
    fn format_classification() {
        assert_eq!(classify_builtin(0), NumFmt::General);
        assert_eq!(classify_builtin(14), NumFmt::Date);
        assert_eq!(classify_builtin(22), NumFmt::DateTime);
        assert_eq!(classify_builtin(10), NumFmt::Percent { decimals: 2 });
        assert_eq!(
            classify_format_code("0.00%"),
            NumFmt::Percent { decimals: 2 }
        );
        assert_eq!(
            classify_format_code("#,##0.00"),
            NumFmt::Number {
                decimals: 2,
                thousands: true
            }
        );
        assert_eq!(classify_format_code("yyyy-mm-dd"), NumFmt::Date);
        assert_eq!(classify_format_code("[h]:mm:ss"), NumFmt::Time);
        assert_eq!(classify_format_code("yyyy-mm-dd hh:mm"), NumFmt::DateTime);
        assert_eq!(classify_format_code("General"), NumFmt::General);
        assert_eq!(classify_format_code("@"), NumFmt::Text);
        // Quoted literals must not look like date tokens.
        assert_eq!(
            classify_format_code("0.0\"kg/day\""),
            NumFmt::Number {
                decimals: 1,
                thousands: false
            }
        );
    }

    #[test]
    fn date_serials() {
        // Known anchors: 2024-01-15 = 45306 (1900 system).
        let p = serial_to_parts(45_306.0, false).unwrap();
        assert_eq!((p.year, p.month, p.day), (2024, 1, 15));
        assert_eq!(parts_to_serial(2024, 1, 15, 0, false), 45_306.0);
        // Serial 1 = 1900-01-01; serial 59 = 1900-02-28; 61 = 1900-03-01.
        let p = serial_to_parts(1.0, false).unwrap();
        assert_eq!((p.year, p.month, p.day), (1900, 1, 1));
        let p = serial_to_parts(59.0, false).unwrap();
        assert_eq!((p.year, p.month, p.day), (1900, 2, 28));
        let p = serial_to_parts(61.0, false).unwrap();
        assert_eq!((p.year, p.month, p.day), (1900, 3, 1));
        // Time of day.
        let p = serial_to_parts(45_306.5, false).unwrap();
        assert_eq!((p.hour, p.minute, p.second), (12, 0, 0));
        // 1904 system: serial 0 = 1904-01-01.
        let p = serial_to_parts(0.0, true).unwrap();
        assert_eq!((p.year, p.month, p.day), (1904, 1, 1));
    }

    #[test]
    fn general_number_formatting() {
        assert_eq!(fmt_general(0.0), "0");
        assert_eq!(fmt_general(42.0), "42");
        assert_eq!(fmt_general(-3.5), "-3.5");
        assert_eq!(fmt_general(0.1 + 0.2), "0.3"); // 15-digit rounding hides IEEE noise
        assert_eq!(fmt_general(1_000_000.0), "1000000");
        assert_eq!(fmt_general(f64::NAN), "#NUM!");
        assert_eq!(fmt_general(1.5e21), "1.50000E+21");
    }

    #[test]
    fn formatted_display() {
        let n = CellValue::Number(1234.567);
        assert_eq!(
            format_value(
                &n,
                NumFmt::Number {
                    decimals: 2,
                    thousands: true
                },
                false
            ),
            "1,234.57"
        );
        assert_eq!(
            format_value(
                &CellValue::Number(0.125),
                NumFmt::Percent { decimals: 1 },
                false
            ),
            "12.5%"
        );
        assert_eq!(
            format_value(&CellValue::Number(45_306.0), NumFmt::Date, false),
            "2024-01-15"
        );
        assert_eq!(
            format_value(&CellValue::Bool(true), NumFmt::General, false),
            "TRUE"
        );
        assert_eq!(
            format_value(&CellValue::Error("#DIV/0!".into()), NumFmt::General, false),
            "#DIV/0!"
        );
        assert_eq!(add_thousands("-1234567.89"), "-1,234,567.89");
    }

    #[test]
    fn general_fits_its_width_as_excel_shows_it() {
        assert_eq!(fmt_general_fit(1.0 / 3.0, 8).as_deref(), Some("0.333333"));
        assert_eq!(fmt_general_fit(12.345678, 8).as_deref(), Some("12.34568"));
        assert_eq!(
            fmt_general_fit(123_456_789.0, 8).as_deref(),
            Some("1.23E+08")
        );
        assert_eq!(
            fmt_general_fit(-123_456_789.0, 8).as_deref(),
            Some("-1.2E+08")
        );
        assert_eq!(fmt_general_fit(1e11, 8).as_deref(), Some("1E+11"));
        assert_eq!(
            fmt_general_fit(0.000_000_123, 8).as_deref(),
            Some("1.23E-07")
        );
        assert_eq!(fmt_general_fit(42.0, 8).as_deref(), Some("42"));
        assert_eq!(fmt_general_fit(123_456_789.0, 3), None);
    }

    #[test]
    fn a_general_cell_shows_at_most_eleven_characters() {
        // However wide the column, General stops at 11 characters.
        assert_eq!(fmt_general_cell(123_456_789_012.0, 20), "1.23457E+11");
        assert_eq!(fmt_general_cell(0.123_456_789_012_345, 20), "0.123456789");
        assert_eq!(fmt_general_cell(12_345_678_901.0, 20), "12345678901");
        assert_eq!(fmt_general_cell(42.0, 20), "42");
        // A narrower column shortens it further, down to `#`s.
        assert_eq!(fmt_general_cell(123_456_789_012.0, 8), "1.23E+11");
        assert_eq!(fmt_general_cell(123_456_789.0, 3), "###");
        assert_eq!(fmt_general_cell(5.0, 0), "5");
    }

    #[test]
    fn csv_export() {
        let mut s = Sheet::default();
        s.set_cell(0, 0, Cell::text("a,b"));
        s.set_cell(0, 1, Cell::number(2.0));
        s.set_cell(1, 0, Cell::text("plain"));
        let csv = sheet_to_csv(&s, &Styles::default(), false);
        // Excel's record end is CR LF.
        assert_eq!(csv, "\"a,b\",2\r\nplain,\r\n");
    }

    #[test]
    fn serial_bounds_and_weekday_helpers() {
        // Out-of-range serials are rejected, not overflowed.
        assert!(serial_to_parts(1e19, false).is_none());
        assert!(serial_to_parts(2_958_466.0, false).is_none()); // past 9999-12-31
        assert!(serial_to_parts(-1.0, false).is_none());
        assert!(serial_to_parts(45306.0, false).is_some()); // 2024-01-15 ok
    }

    #[test]
    fn set_row_filtered_marks_and_clears() {
        // #678: a filter's hide marks the row filter-hidden; unhiding clears
        // the mark; a hand hide is never filtered.
        let mut s = Sheet::default();
        s.set_row_filtered(2, true);
        assert!(s.row_hidden(2) && s.row_filtered(2));
        s.set_row_filtered(2, false);
        assert!(!s.row_hidden(2) && !s.row_filtered(2));
        assert!(s.filtered_rows.is_empty());
        s.set_row_hidden(3, true);
        assert!(s.row_hidden(3) && !s.row_filtered(3));
        // A marked row unhidden by hand is no longer filtered.
        s.set_row_filtered(4, true);
        s.set_row_hidden(4, false);
        assert!(!s.row_filtered(4));
    }
}
