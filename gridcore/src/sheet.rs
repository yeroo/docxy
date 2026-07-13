//! The workbook model: sheets, sparse cells, values, and the display-level
//! style subset (number formats, bold/italic/color).
//!
//! Coordinates are 0-based `(row, col)` everywhere in the model; A1 notation
//! is converted at the boundaries (parsing, display, formulas). Cells live in
//! a sparse `BTreeMap` so memory is proportional to content, and iteration is
//! naturally row-major (the order worksheet XML wants).

use std::collections::BTreeMap;

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
    /// dynamic-array engine, which tracks their extent in [`Cell::spill`].
    pub f_attrs: Option<String>,
    /// Index into [`Styles::xfs`] (`s=` attribute); 0 is the default style.
    pub style: u32,
    /// (rows, cols) of the dynamic-array spill anchored here, including this
    /// cell — set by the recalc engine (or from `<f t="array" ref="…">` at
    /// load). The spilled cells themselves are plain values owned by this
    /// anchor. `None` = no spill (scalar result).
    pub spill: Option<(u32, u32)>,
}

impl Cell {
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
    /// charts. Rendered as an overlay; not editable.
    pub drawings: Vec<Drawing>,
}

/// A floating drawing anchored over a cell rectangle (a picture or a chart).
#[derive(Clone, Debug)]
pub struct Drawing {
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
#[derive(Clone, Debug, Default)]
pub struct ChartData {
    pub title: String,
    /// `bar` / `pie` / `line` / `area` / `scatter` … (the plot element's local name).
    pub kind: String,
    pub categories: Vec<String>,
    pub series: Vec<ChartSeries>,
}

/// One data series of a [`ChartData`].
#[derive(Clone, Debug, Default)]
pub struct ChartSeries {
    pub name: String,
    pub values: Vec<f64>,
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
        self.row_attrs
            .get(&row)
            .is_some_and(|a| attr_hidden(a))
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
    Other,
}

/// A conditional-formatting block: its `rules` apply over `ranges` (`sqref`).
#[derive(Clone, Debug, Default)]
pub struct CondFormat {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    pub rules: Vec<CfRule>,
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

/// Render a cell value through its full style: the real format-code runtime
/// when the code is known and renderable, the classified approximation
/// otherwise.
pub fn format_with(xf: &Xf, value: &CellValue, date1904: bool) -> String {
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

/// Export one sheet as RFC-4180-ish CSV (display values, formulas evaluated
/// to their cached results).
pub fn sheet_to_csv(sheet: &Sheet, styles: &Styles, date1904: bool) -> String {
    let (rows, cols) = sheet.used_size();
    let mut out = String::new();
    for r in 0..rows {
        for c in 0..cols {
            if c > 0 {
                out.push(',');
            }
            if let Some(cell) = sheet.cell(r, c) {
                let text = format_with(&styles.xf(cell.style), &cell.value, date1904);
                if text.contains([',', '"', '\n', '\r']) {
                    out.push('"');
                    out.push_str(&text.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(&text);
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn csv_export() {
        let mut s = Sheet::default();
        s.set_cell(0, 0, Cell::text("a,b"));
        s.set_cell(0, 1, Cell::number(2.0));
        s.set_cell(1, 0, Cell::text("plain"));
        let csv = sheet_to_csv(&s, &Styles::default(), false);
        assert_eq!(csv, "\"a,b\",2\nplain,\n");
    }

    #[test]
    fn serial_bounds_and_weekday_helpers() {
        // Out-of-range serials are rejected, not overflowed.
        assert!(serial_to_parts(1e19, false).is_none());
        assert!(serial_to_parts(2_958_466.0, false).is_none()); // past 9999-12-31
        assert!(serial_to_parts(-1.0, false).is_none());
        assert!(serial_to_parts(45306.0, false).is_some()); // 2024-01-15 ok
    }
}
