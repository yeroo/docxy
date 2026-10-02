//! Typed cell entry: what a committed editor buffer becomes, the way Excel
//! reads it.
//!
//! Every host that commits typed text (the xlsxy grid and its control verbs,
//! gridwasm, the desktop suite's sheet tab) goes through [`entry_cell`], so
//! they cannot drift apart. The rules, in order:
//!
//! 1. More than [`MAX_CELL_CHARS`] UTF-16 units is refused.
//! 2. A cell formatted Text (`@`) takes the entry exactly as typed.
//! 3. A leading `'` makes the rest text and sets the xf's `quotePrefix`.
//! 4. `=…` is a formula.
//! 5. `TRUE`/`FALSE` and error literals.
//! 6. Numbers: sign, parentheses, `$`, thousands, `%` before or after, and an
//!    exponent. Then fractions (`1 1/4`), then dates and times.
//! 7. A `+`, `-` or `@` prefix that makes a valid formula.
//! 8. Anything else is text.
//!
//! A recognised shape carries Excel's matching number format, applied only
//! when the cell's format is General — except a date or time, which also
//! replaces a number format that is not a date format itself (Excel's rule:
//! `1/15/2024` typed into a `0.00` cell shows as a date). A plain number typed into a percent
//! cell is divided by 100 when its magnitude is at least 1 (Excel's
//! automatic percent entry).

use crate::formula::{ExcelError, days_in_month, norm_year};
use crate::sheet::{
    Cell, CellValue, NumFmt, Sheet, Styles, Workbook, Xf, classify_format_code, parts_to_serial,
    serial_to_parts,
};

/// Excel's cell limit, in UTF-16 code units.
pub const MAX_CELL_CHARS: usize = 32_767;

/// What the parser needs besides the text and the cell's format.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntryCtx {
    /// The workbook uses the 1904 date system.
    pub date1904: bool,
    /// The host's clock as a 1900-system serial (the same value it gives
    /// `Engine::clock`). A date typed without a year (`3/4`) takes its year;
    /// with no clock it stays text rather than guess.
    pub today: Option<f64>,
    /// Excel's "Automatically insert a decimal point": the places a typed
    /// number without a decimal point, percent or exponent is shifted by
    /// (`1234` with 2 is 12.34, with -2 is 123400). Only a host's typed
    /// commit sets it ([`entry_cell_ctx`]); paste, Find & Replace, imports and
    /// automation leave it `None`, so their numbers are never shifted.
    pub fixed_decimal: Option<i16>,
}

/// A parsed entry: the cell (its `style` unset) and what the entry asks of
/// the cell's format.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub cell: Cell,
    /// The number format the recognised shape carries (`#,##0`, `m/d/yyyy`…).
    pub format: Option<&'static str>,
    /// Entered with a leading apostrophe.
    pub quote_prefix: bool,
    /// The entry holds a line feed (Alt+Enter), which turns on Wrap Text.
    pub wrap: bool,
}

/// Why an entry was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryError {
    /// Longer than [`MAX_CELL_CHARS`]; `len` is its UTF-16 length.
    TooLong { len: usize },
}

impl std::fmt::Display for EntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EntryError::TooLong { len } => write!(
                f,
                "the entry is {len} characters; a cell holds at most {MAX_CELL_CHARS}"
            ),
        }
    }
}

impl std::error::Error for EntryError {}

/// Refuse text a cell cannot hold.
pub fn check_len(text: &str) -> Result<(), EntryError> {
    let len = text.encode_utf16().count();
    if len > MAX_CELL_CHARS {
        return Err(EntryError::TooLong { len });
    }
    Ok(())
}

// These predicates read the code when the xf has one and fall back to
// the `NumFmt` classification only for a code-less xf: a writer that set one
// half and not the other must not change how an entry is read.

/// The cell's format is General: no code, or the `General` code the loader
/// synthesizes for `numFmtId="0"`.
pub fn is_general(xf: &Xf) -> bool {
    match xf.code.as_deref() {
        Some(c) => c.is_empty() || c.eq_ignore_ascii_case("general"),
        None => xf.numfmt == NumFmt::General,
    }
}

/// The cell's format is Text (`@`, builtin 49).
pub fn is_text(xf: &Xf) -> bool {
    match xf.code.as_deref() {
        Some(c) => c == "@",
        None => xf.numfmt == NumFmt::Text,
    }
}

/// The cell's format is a percent format.
pub fn is_percent(xf: &Xf) -> bool {
    let class = match xf.code.as_deref() {
        Some(c) => classify_format_code(c),
        None => xf.numfmt,
    };
    matches!(class, NumFmt::Percent { .. })
}

/// The cell's format is a date, time or date-time format.
pub fn is_date(xf: &Xf) -> bool {
    let class = match xf.code.as_deref() {
        Some(c) => classify_format_code(c),
        None => xf.numfmt,
    };
    matches!(class, NumFmt::Date | NumFmt::Time | NumFmt::DateTime)
}

/// Parse typed `text` as an entry into a cell formatted `xf`.
pub fn parse_entry(text: &str, xf: &Xf, ctx: &EntryCtx) -> Result<Entry, EntryError> {
    check_len(text)?;
    let wrap = text.contains('\n');
    let entry = |cell: Cell, format: Option<&'static str>| Entry {
        cell,
        format,
        quote_prefix: false,
        wrap,
    };
    if text.is_empty() {
        return Ok(entry(Cell::default(), None));
    }
    if is_text(xf) {
        return Ok(entry(Cell::text(text), None));
    }
    if let Some(rest) = text.strip_prefix('\'') {
        return Ok(Entry {
            quote_prefix: true,
            ..entry(Cell::text(rest), None)
        });
    }
    if let Some(body) = text.strip_prefix('=') {
        if !body.is_empty() {
            return Ok(entry(Cell::formula(body), None));
        }
    }
    let t = text.trim();
    if t.eq_ignore_ascii_case("TRUE") || t.eq_ignore_ascii_case("FALSE") {
        let b = t.eq_ignore_ascii_case("TRUE");
        return Ok(entry(value_cell(CellValue::Bool(b)), None));
    }
    if ExcelError::from_code(t).is_some() {
        return Ok(entry(
            value_cell(CellValue::Error(t.to_ascii_uppercase())),
            None,
        ));
    }
    if let Some((mut n, shape)) = parse_number(t) {
        // The shift comes first: the percent-cell rule below sees the
        // shifted number, so `5` with 2 places in a 0% cell is 0.05 (5%).
        if let Some(places) = ctx.fixed_decimal {
            if !shape.point && !shape.percent && !shape.exponent {
                n = fixed_decimal(n, places);
            }
        }
        if !shape.percent && is_percent(xf) && n.abs() >= 1.0 {
            n /= 100.0;
        }
        return Ok(entry(Cell::number(n), shape.format()));
    }
    if let Some((n, format)) = parse_fraction(t) {
        return Ok(entry(Cell::number(n), Some(format)));
    }
    if let Some((n, format)) = parse_date_time(t, ctx) {
        return Ok(entry(Cell::number(n), Some(format)));
    }
    if let Some(src) = formula_prefix(t) {
        return Ok(entry(Cell::formula(&src), None));
    }
    Ok(entry(Cell::text(text), None))
}

/// The xf a committed entry leaves on a cell formatted `base`. A recognised
/// format lands on a General cell; a recognised date or time also replaces a
/// number format that is not a date format (Excel switches `0.00` or a
/// currency cell to the date it was given), while a date/time cell keeps its
/// own format (#654) and a Text cell never recognises anything.
pub fn entry_xf(base: &Xf, e: &Entry) -> Xf {
    let mut xf = base.clone();
    if let Some(code) = e.format {
        let dated = classify_format_code(code);
        let date_entry = matches!(dated, NumFmt::Date | NumFmt::Time | NumFmt::DateTime);
        if is_general(base) || (date_entry && !is_date(base) && !is_text(base)) {
            xf.set_code(Some(code.to_string()));
        }
    }
    xf.quote_prefix = e.quote_prefix;
    if e.wrap {
        xf.wrap = true;
    }
    xf
}

/// The style index for `e` on a cell whose style is `base`: `base` itself
/// when nothing changes, else an interned xf.
pub fn entry_style(styles: &mut Styles, base: u32, e: &Entry) -> u32 {
    let old = styles.xf(base);
    let new = entry_xf(&old, e);
    if new == old { base } else { styles.intern(new) }
}

/// The context a workbook gives an entry, with the host's clock. It never
/// shifts a number ([`EntryCtx::fixed_decimal`] is `None`).
pub fn entry_ctx(wb: &Workbook, today: Option<f64>) -> EntryCtx {
    EntryCtx {
        date1904: wb.date1904,
        today,
        fixed_decimal: None,
    }
}

/// `n` with its decimal point moved `places` to the left (right when
/// negative), kept to 15 digits; a shift that overflows leaves `n` as it was.
fn fixed_decimal(n: f64, places: i16) -> f64 {
    let shifted = round15(n / 10f64.powi(i32::from(places)));
    if shifted.is_finite() { shifted } else { n }
}

/// Parse `text` typed into (sheet, row, col) and resolve its style: the cell
/// ready for `Engine::set_cell`. Only the style table is touched here (a new
/// xf may be interned); the cell itself is left to the caller. Never shifts a
/// number: a host's typed commit uses [`entry_cell_ctx`].
pub fn entry_cell(
    wb: &mut Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    text: &str,
    today: Option<f64>,
) -> Result<Cell, EntryError> {
    let ctx = entry_ctx(wb, today);
    entry_cell_ctx(wb, sheet, row, col, text, &ctx)
}

/// [`entry_cell`] under an explicit context: what a host's typed commit
/// calls, with the user's fixed-decimal places in `ctx`.
pub fn entry_cell_ctx(
    wb: &mut Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    text: &str,
    ctx: &EntryCtx,
) -> Result<Cell, EntryError> {
    let base = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.cell(row, col))
        .map(|c| c.style)
        .unwrap_or(0);
    let e = parse_entry(text, &wb.styles.xf(base), ctx)?;
    let style = entry_style(&mut wb.styles, base, &e);
    Ok(Cell { style, ..e.cell })
}

/// The formula body `text` makes in the first cell of the range
/// `(r1, c1, r2, c2)` on `sheet` where it is a formula ([`typed_formula`]),
/// for hosts that validate a Ctrl+Enter or a range put before
/// [`entry_range`]: a Text cell takes `=…` as text, so the formula to check
/// may be in any other cell, not only the active one. `None` when no cell of
/// the range reads it as a formula.
pub fn range_formula<'a>(
    wb: &Workbook,
    sheet: usize,
    (r1, c1, r2, c2): (u32, u32, u32, u32),
    text: &'a str,
) -> Option<&'a str> {
    (r1..=r2)
        .flat_map(|r| (c1..=c2).map(move |c| (r, c)))
        .find_map(|(r, c)| typed_formula(wb, sheet, r, c, text))
}

/// Ctrl+Enter: `text`, typed at `active`, entered into every cell of the
/// range `(r1, c1, r2, c2)` on `sheet`. Each cell reads the entry under its
/// own format rules; where that makes a formula (`=A1`, `+A1`, `@SUM(A1:A2)`)
/// its relative references move with the cell, as a fill would. All or
/// nothing: a refused entry changes no style. Never shifts a number: a
/// host's typed commit uses [`entry_range_ctx`].
pub fn entry_range(
    wb: &mut Workbook,
    sheet: usize,
    range: (u32, u32, u32, u32),
    active: (u32, u32),
    text: &str,
    today: Option<f64>,
) -> Result<Vec<(u32, u32, Cell)>, EntryError> {
    let ctx = entry_ctx(wb, today);
    entry_range_ctx(wb, sheet, range, active, text, &ctx)
}

/// [`entry_range`] under an explicit context (a typed Ctrl+Enter, with the
/// user's fixed-decimal places): each cell is read under its own format, so
/// a Text cell keeps `1234` as text while its neighbours shift.
pub fn entry_range_ctx(
    wb: &mut Workbook,
    sheet: usize,
    (r1, c1, r2, c2): (u32, u32, u32, u32),
    active: (u32, u32),
    text: &str,
    ctx: &EntryCtx,
) -> Result<Vec<(u32, u32, Cell)>, EntryError> {
    check_len(text)?;
    let mut out = Vec::new();
    for r in r1..=r2 {
        for c in c1..=c2 {
            let dr = r as i64 - active.0 as i64;
            let dc = c as i64 - active.1 as i64;
            let mut cell = entry_cell_ctx(wb, sheet, r, c, text, ctx)?;
            if let Some(src) = &cell.formula {
                if let Some(moved) = crate::formula::translate_formula(src, dr, dc) {
                    cell.formula = Some(moved);
                }
            }
            out.push((r, c, cell));
        }
    }
    Ok(out)
}

/// Re-read `new` as the entry of an existing `cell` whose text `old` was
/// edited in place (Find & Replace), under the cell's own rules — with one
/// exception for a percent cell holding a number constant. When `old` had no
/// `%` it was that number's plain value (an input text such as `1.5` for
/// 150%), so `new` is read as General rather than divided by 100 again. A
/// text or formula in a percent cell is always read under the cell's rules
/// (`TBD` replaced by `5` is 5%). When `old` showed the `%` (display
/// text such as `150%`), `new` is read as typed into the cell: `160%` is 1.6,
/// and `150` with the `%` removed is divided like any number typed there.
/// The style is resolved into `styles`.
pub fn reenter_cell(
    cell: &Cell,
    styles: &mut Styles,
    ctx: &EntryCtx,
    old: &str,
    new: &str,
) -> Result<Cell, EntryError> {
    let text = new;
    let xf = styles.xf(cell.style);
    // Only a number constant's input text is its plain value; a text such as
    // `TBD` replaced by `5` is a fresh number typed into the percent cell.
    let plain_value =
        matches!(cell.value, CellValue::Number(_)) && cell.formula.is_none() && !old.contains('%');
    let read_as = if is_percent(&xf) && plain_value {
        Xf::default()
    } else {
        xf
    };
    let e = parse_entry(text, &read_as, ctx)?;
    let style = entry_style(styles, cell.style, &e);
    Ok(Cell { style, ..e.cell })
}

/// The formula body `text` would commit into (sheet, row, col) — for hosts
/// that validate a formula before committing. `None` when it is not a
/// `=` formula there: a Text-formatted cell stores `=…` as text, so there is
/// nothing to refuse.
pub fn typed_formula<'a>(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    text: &'a str,
) -> Option<&'a str> {
    let body = text.strip_prefix('=').filter(|b| !b.is_empty())?;
    let style = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.cell(row, col))
        .map_or(0, |c| c.style);
    (!is_text(&wb.styles.xf(style))).then_some(body)
}

/// A pasted field as a cell of style `style`, read as Excel re-reads pasted
/// text: by the typed-entry rules ([`parse_entry`]) under the target's
/// format. A Text-formatted target takes the field exactly as it is;
/// elsewhere a leading `'` is Excel's text marker (the rest is text and the
/// xf gets `quotePrefix`) and a plain field clears a quote prefix it lands
/// on; a date, currency or thousands shape brings its number format, and a
/// percent target divides a plain number as typing does. So a copied `'007`
/// pastes back as the text `007`, and `''abc` (how a text beginning with `'`
/// is copied) as `'abc`. An empty field clears the cell. Paste never turns on
/// Wrap Text. A field over the cell limit is kept as text, unread.
///
/// A TSV clipboard carries no formats, so for text from outside the apostrophe
/// rule is only exact between cells of the same kind: a Text cell's `'abc`
/// is copied bare and, pasted into a non-Text cell, reads as the marker (text
/// `abc`, quote prefix); an escaped `''abc` pasted into a Text cell stays
/// `''abc`. A host's own copy carries the cells, and pastes those instead.
pub fn paste_cell(styles: &mut Styles, style: u32, text: &str, ctx: &EntryCtx) -> Cell {
    let e = match parse_entry(text, &styles.xf(style), ctx) {
        Ok(e) => Entry { wrap: false, ..e },
        Err(EntryError::TooLong { .. }) => Entry {
            cell: Cell::text(text),
            format: None,
            quote_prefix: false,
            wrap: false,
        },
    };
    let style = entry_style(styles, style, &e);
    Cell { style, ..e.cell }
}

/// Does the text the editor starts from (and a copy carries) need a leading
/// `'` so that entering it again gives back the same cell? For a
/// quote-prefixed text, and for a text that itself begins with `'` in a cell
/// that is not Text-formatted — entered bare, its own `'` would be taken as
/// the marker and dropped. A Text cell takes an entry as typed, so it never
/// needs one — not even with a quote prefix (a `'007` later formatted `@`).
pub fn needs_apostrophe(cell: &Cell, xf: &Xf) -> bool {
    let CellValue::Text(s) = &cell.value else {
        return false;
    };
    cell.formula.is_none() && !is_text(xf) && (xf.quote_prefix || s.starts_with('\''))
}

/// A cell's field on a copied TSV: its `shown` text, with the leading `'`
/// [`needs_apostrophe`] asks for, so a paste ([`paste_cell`]) reads it back
/// as the same text.
pub fn copy_field(cell: &Cell, xf: &Xf, shown: String) -> String {
    if needs_apostrophe(cell, xf) {
        format!("'{shown}")
    } else {
        shown
    }
}

/// Find & Replace's entry for one text cell, from `replaced` — its own text
/// (without the `'` re-entry would add) after the replacement — for
/// [`reenter_cell`]. The `'` is decided from the result: added for a
/// non-formula text in a non-Text cell that is quote-prefixed (`007` stays
/// text) or whose new text begins with `'` (kept as its own character),
/// never for an empty result (the cell clears). So a loaded `'5` with `'`
/// removed becomes the number 5.
pub fn replaced_entry(cell: &Cell, xf: &Xf, replaced: String) -> String {
    let text_cell = cell.formula.is_none() && matches!(cell.value, CellValue::Text(_));
    let marker = xf.quote_prefix || replaced.starts_with('\'');
    if !replaced.is_empty() && text_cell && !is_text(xf) && marker {
        format!("'{replaced}")
    } else {
        replaced
    }
}

/// [`seed_text`] for anything but a number constant: [`crate::edit::input_text_of`]
/// (a formula's `=` source, a text, a boolean or an error) with a leading `'`
/// where [`needs_apostrophe`] says re-entering needs one.
fn input_text_styled(cell: &Cell, xf: &Xf) -> String {
    copy_field(cell, xf, crate::edit::input_text_of(cell))
}

/// The text a cell editor opens on (and the formula bar shows). A number is
/// written in full, not rounded to General's 15 digits, in E notation outside
/// `1e-10..1e21` where plain digits would run to hundreds of characters; in
/// a percent cell it is shown as the percent it reads as (`150%` for 1.5),
/// since a plain `1.5` typed back into that cell would be divided by 100.
/// Anything else is [`input_text_styled`]: a formula's `=` source, and text
/// with the `'` its re-entry needs.
///
/// Re-entering the seed gives back the same value except for a number with
/// more than 15 significant digits: typed entry keeps 15, as Excel does, so
/// `0.30000000000000004` re-entered is 0.3. The cell's format may change: an
/// E-notation seed (`1E300`) re-entered into a General cell brings the
/// scientific format `0.00E+00`, as any typed exponent does. What keeps the
/// cell exactly as it was is the host leaving an unchanged seed uncommitted
/// (xlsxy and the suite do; gridwasm re-commits it).
pub fn seed_text(cell: &Cell, xf: &Xf) -> String {
    match &cell.value {
        CellValue::Number(n) if cell.formula.is_none() => {
            if is_percent(xf) {
                percent_seed(*n, xf)
            } else {
                full_number(*n)
            }
        }
        _ => input_text_styled(cell, xf),
    }
}

/// `n` in its shortest round-trip digits.
fn full_number(n: f64) -> String {
    if n == 0.0 || (1e-10..1e21).contains(&n.abs()) {
        format!("{n}")
    } else {
        format!("{n:E}")
    }
}

/// `n` as a percent that reads back as `n` in a percent cell `xf`: `n * 100`
/// is rarely exact in binary (0.07 * 100 is 7.000000000000001), so take the
/// fewest significant digits whose `%` text reads back as `n` exactly.
fn percent_seed(n: f64, xf: &Xf) -> String {
    let pct = n * 100.0;
    let ctx = EntryCtx::default();
    for digits in 1..=17 {
        let Ok(r) = format!("{:.*e}", digits - 1, pct).parse::<f64>() else {
            break;
        };
        let text = format!("{}%", full_number(r));
        let back = parse_entry(&text, xf, &ctx).ok().map(|e| e.cell.value);
        if back == Some(CellValue::Number(n)) {
            return text;
        }
    }
    format!("{}%", full_number(pct))
}

fn value_cell(value: CellValue) -> Cell {
    Cell {
        value,
        ..Cell::default()
    }
}

// ---------------------------------------------------------------------------
// AutoComplete
// ---------------------------------------------------------------------------

/// Excel's AutoComplete for cell values: the text value `typed` completes to
/// while it is typed into (row, col) of `sheet`, or `None`.
///
/// The candidates are the text values of the column's contiguous non-empty
/// block around the cell — upward from the cell, then downward, each until
/// the first empty cell — excluding the cell itself (its own old value is
/// not a proposal). Numbers, booleans, errors and formulas belong to the
/// block but are never proposed. The match is a case-insensitive prefix
/// match; values that differ only by case are one value, spelled as the
/// first one found (nearest above first). Nothing is proposed for an empty
/// or `=` entry, when no value or more than one value matches, or when the
/// typed text already is one of the values.
///
/// A text that would not stay text when typed (`007`, `TRUE`, `3/4` held as
/// text) is never proposed: the host commits the proposal as typed, so it
/// would come back a number, a boolean or a date. Excel does not complete
/// entries that are only numbers, dates or times either.
pub fn autocomplete(sheet: &Sheet, row: u32, col: u32, typed: &str) -> Option<String> {
    if typed.is_empty() || typed.starts_with('=') {
        return None;
    }
    let want = typed.to_lowercase();
    let filled = |r: u32| {
        sheet
            .cell(r, col)
            .filter(|c| !c.value.is_empty() || c.formula.is_some())
    };
    let above = (0..row).rev().map_while(&filled);
    let below = (row.saturating_add(1)..crate::sheet::MAX_ROWS).map_while(&filled);
    let mut found: Option<(String, String)> = None;
    for cell in above.chain(below) {
        let CellValue::Text(text) = &cell.value else {
            continue;
        };
        if cell.formula.is_some() {
            continue;
        }
        let lower = text.to_lowercase();
        // Any text value counts for "already one of the values"; only a
        // matching one pays for the stays-text parse.
        if lower == want {
            return None;
        }
        if !lower.starts_with(&want) || !stays_text(text) {
            continue;
        }
        match &found {
            Some((seen, _)) if *seen == lower => {}
            Some(_) => return None,
            None => found = Some((lower, text.clone())),
        }
    }
    found.map(|(_, text)| text)
}

/// Does `text`, typed into a General cell, stay that same text? With a
/// clock, so a yearless date such as `3/4` is read as the date it would be.
fn stays_text(text: &str) -> bool {
    let ctx = EntryCtx {
        today: Some(45_000.0),
        ..EntryCtx::default()
    };
    parse_entry(text, &Xf::default(), &ctx)
        .is_ok_and(|e| e.cell.formula.is_none() && e.cell.value == CellValue::Text(text.into()))
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

/// What a recognised number looked like, which picks its format.
#[derive(Clone, Copy, Debug, Default)]
struct NumShape {
    /// A decimal point was typed, even a bare or trailing one (`1.`, `.5`):
    /// Excel's fixed decimal leaves such a number alone.
    point: bool,
    thousands: bool,
    decimals: bool,
    percent: bool,
    currency: bool,
    exponent: bool,
}

impl NumShape {
    fn format(self) -> Option<&'static str> {
        Some(if self.exponent {
            "0.00E+00"
        } else if self.currency {
            if self.decimals {
                "$#,##0.00_);($#,##0.00)"
            } else {
                "$#,##0_);($#,##0)"
            }
        } else if self.percent {
            if self.decimals { "0.00%" } else { "0%" }
        } else if self.thousands {
            if self.decimals { "#,##0.00" } else { "#,##0" }
        } else {
            return None;
        })
    }
}

/// Excel's number grammar: `(…)` or a sign for negatives, `$`, thousands
/// separators in groups of three, `%` before or after, and an exponent.
fn parse_number(t: &str) -> Option<(f64, NumShape)> {
    let mut shape = NumShape::default();
    let mut s = t;
    let mut negative = false;
    if let Some(inner) = s.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        negative = true;
        s = inner.trim();
    }
    let mut signed = negative;
    let mut take_sign = |s: &mut &str, negative: &mut bool| {
        if signed {
            return;
        }
        if let Some(r) = s.strip_prefix('-') {
            *negative = true;
            signed = true;
            *s = r;
        } else if let Some(r) = s.strip_prefix('+') {
            signed = true;
            *s = r;
        }
    };
    take_sign(&mut s, &mut negative);
    if let Some(r) = s.strip_prefix('$') {
        shape.currency = true;
        s = r.trim_start();
        take_sign(&mut s, &mut negative);
    }
    if let Some(r) = s.strip_prefix('%') {
        shape.percent = true;
        s = r.trim_start();
    } else if let Some(r) = s.strip_suffix('%') {
        shape.percent = true;
        s = r.trim_end();
    }
    if shape.currency && shape.percent {
        return None;
    }
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (int, frac) = match mant.split_once('.') {
        Some((i, f)) => {
            shape.point = true;
            (i, Some(f))
        }
        None => (mant, None),
    };
    if !frac.is_none_or(|f| f.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let digits: String = if int.contains(',') {
        let groups: Vec<&str> = int.split(',').collect();
        let ok = (1..=3).contains(&groups[0].len())
            && groups[1..].iter().all(|g| g.len() == 3)
            && groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_digit()));
        if !ok || exp.is_some() {
            return None;
        }
        shape.thousands = true;
        groups.concat()
    } else {
        if !int.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        int.to_string()
    };
    if digits.is_empty() && frac.is_none_or(str::is_empty) {
        return None;
    }
    shape.decimals = frac.is_some_and(|f| !f.is_empty());
    let mut src = digits;
    if let Some(f) = frac {
        src.push('.');
        src.push_str(f);
    }
    if let Some(e) = exp {
        let body = e.strip_prefix(['+', '-']).unwrap_or(e);
        if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        shape.exponent = true;
        src.push('e');
        src.push_str(e);
    }
    let mut n: f64 = round15(src.parse().ok()?);
    if shape.percent {
        n /= 100.0;
    }
    if negative {
        n = -n;
    }
    n.is_finite().then_some((n, shape))
}

/// `n` kept to Excel's 15 significant digits: a typed or imported
/// `1234567890123456789` is stored as 1.23456789012346E+18.
pub fn round15(n: f64) -> f64 {
    if n == 0.0 || !n.is_finite() {
        return n;
    }
    format!("{n:.14e}").parse().unwrap_or(n)
}

/// `[-]W N/D` — a whole number, a space and a proper fraction.
fn parse_fraction(t: &str) -> Option<(f64, &'static str)> {
    let (negative, s) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (whole, frac) = s.split_once(' ')?;
    let (num, den) = frac.trim_start().split_once('/')?;
    let all_digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(num) || !all_digits(den) {
        return None;
    }
    let (w, n, d): (f64, f64, f64) = (whole.parse().ok()?, num.parse().ok()?, den.parse().ok()?);
    if d == 0.0 {
        return None;
    }
    let v = w + n / d;
    let format = if den.len() == 1 { "# ?/?" } else { "# ??/??" };
    v.is_finite()
        .then_some((if negative { -v } else { v }, format))
}

// ---------------------------------------------------------------------------
// Dates and times
// ---------------------------------------------------------------------------

/// A month from its full English name or three-letter abbreviation.
pub(crate) fn month_name(s: &str) -> Option<u32> {
    const M: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let s = s.to_ascii_lowercase();
    M.iter()
        .position(|m| s == *m || (s.len() == 3 && m.starts_with(&s)))
        .map(|i| i as u32 + 1)
}

pub(crate) fn num(s: &str) -> Option<i64> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A year as typed: four digits as is, one or two by Excel's 00–29/30–99
/// rule; three digits are not a year.
pub(crate) fn year(s: &str) -> Option<i64> {
    let y = num(s)?;
    match s.len() {
        1 | 2 => Some(norm_year(y)),
        4 => Some(y),
        _ => None,
    }
}

/// A date or a time or both: the serial and the format Excel gives it.
fn parse_date_time(t: &str, ctx: &EntryCtx) -> Option<(f64, &'static str)> {
    if let Some((n, f)) = parse_time(t) {
        return Some((n, f));
    }
    if let Some((serial, f)) = parse_date(t, ctx) {
        return Some((serial, f));
    }
    // A date, a space, then a time: try every split, rightmost first.
    for (i, _) in t.match_indices(' ').collect::<Vec<_>>().into_iter().rev() {
        let (d, tm) = (t[..i].trim_end(), t[i + 1..].trim_start());
        if let (Some((day, _)), Some((frac, _))) = (parse_date(d, ctx), parse_time(tm)) {
            if frac < 1.0 {
                return Some((day + frac, "m/d/yyyy h:mm"));
            }
        }
    }
    None
}

/// `h:mm`, `h:mm:ss`, an optional `AM`/`PM`/`a`/`p`, or `h AM`.
pub(crate) fn parse_time(t: &str) -> Option<(f64, &'static str)> {
    let lower = t.to_ascii_lowercase();
    let mut body = lower.as_str();
    let mut pm = None;
    for (suf, is_pm) in [("am", false), ("pm", true), ("a", false), ("p", true)] {
        if let Some(rest) = body.strip_suffix(suf) {
            pm = Some(is_pm);
            body = rest.trim_end();
            break;
        }
    }
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() > 3 || (parts.len() == 1 && pm.is_none()) {
        return None;
    }
    let mut h = num(parts[0])?;
    let m = match parts.get(1) {
        Some(p) if p.len() <= 2 => num(p)?,
        Some(_) => return None,
        None => 0,
    };
    let sec: f64 = match parts.get(2) {
        Some(p) => {
            let (whole, frac) = p.split_once('.').unwrap_or((p, "0"));
            if whole.len() > 2 || num(whole).is_none() || !frac.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            p.parse().ok()?
        }
        None => 0.0,
    };
    if m >= 60 || sec >= 60.0 {
        return None;
    }
    let seconds = parts.len() == 3;
    let format = match pm {
        Some(is_pm) => {
            if !(0..=12).contains(&h) {
                return None;
            }
            h %= 12;
            if is_pm {
                h += 12;
            }
            if seconds {
                "h:mm:ss AM/PM"
            } else {
                "h:mm AM/PM"
            }
        }
        None if h >= 24 => "[h]:mm:ss",
        None if seconds => "h:mm:ss",
        None => "h:mm",
    };
    Some((
        (h as f64 * 3600.0 + m as f64 * 60.0 + sec) / 86_400.0,
        format,
    ))
}

/// A calendar date in the forms Excel's en-US entry reads.
fn parse_date(t: &str, ctx: &EntryCtx) -> Option<(f64, &'static str)> {
    // A date starts and ends on a digit or a letter, and `/`/`-` separate two
    // parts: `-1/2` is a formula prefix, `1//2` nothing at all.
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    if !edge(t.chars().next()) || !edge(t.chars().last()) {
        return None;
    }
    let seps = |a: char, b: char| {
        matches!(a, '/' | '-') && matches!(b, '/' | '-' | ' ' | ',')
            || matches!(a, ' ' | ',') && matches!(b, '/' | '-')
    };
    if t.chars().zip(t.chars().skip(1)).any(|(a, b)| seps(a, b)) {
        return None;
    }
    let parts: Vec<&str> = t
        .split(['/', '-', ' ', ','])
        .filter(|p| !p.is_empty())
        .collect();
    // Separators other than those are not a date.
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let this_year = || {
        let today = ctx.today?;
        serial_to_parts(today.floor(), false).map(|p| p.year)
    };
    let (y, m, d, format): (i64, u32, u32, &'static str) =
        if let Some(mi) = parts.iter().position(|p| month_name(p).is_some()) {
            let m = month_name(parts[mi])?;
            let others: Vec<&str> = parts
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != mi)
                .map(|(_, p)| *p)
                .collect();
            match (mi, others.as_slice()) {
                // 15-Jan-2024, January 15, 2024
                (1, [d, y]) | (0, [d, y]) => (year(y)?, m, num(d)? as u32, "d-mmm-yy"),
                // 15-Jan
                (1, [d]) => (this_year()?, m, num(d)? as u32, "d-mmm"),
                // Jan-2024 (a year), Jan-15 (a day of this year)
                (0, [x]) => {
                    let v = num(x)?;
                    if x.len() <= 2 && (1..=31).contains(&v) {
                        (this_year()?, m, v as u32, "d-mmm")
                    } else {
                        (year(x)?, m, 1, "mmm-yy")
                    }
                }
                _ => return None,
            }
        } else {
            // All numeric, and `/` or `-` only between them.
            if t.contains([' ', ',']) {
                return None;
            }
            let n: Vec<i64> = parts.iter().map(|p| num(p)).collect::<Option<_>>()?;
            match parts.as_slice() {
                [a, _, _] if a.len() == 4 => (n[0], n[1] as u32, n[2] as u32, "m/d/yyyy"),
                [_, _, y] => (year(y)?, n[0] as u32, n[1] as u32, "m/d/yyyy"),
                // 1/2024: a month and a year.
                [_, y] if y.len() == 4 || n[1] > 31 => (year(y)?, n[0] as u32, 1, "mmm-yy"),
                // 3/4: a month and a day of this year.
                [_, _] => (this_year()?, n[0] as u32, n[1] as u32, "d-mmm"),
                _ => return None,
            }
        };
    let first_year = if ctx.date1904 { 1904 } else { 1900 };
    if !(first_year..=9999).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    if d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let serial = parts_to_serial(y, m, d, 0, ctx.date1904);
    (serial >= 0.0).then_some((serial, format))
}

// ---------------------------------------------------------------------------
// Formula prefixes
// ---------------------------------------------------------------------------

/// `+2+3` → `2+3`, `+A99` → `+A99`, `-L1` → `-L1`, `@SUM(1,2)` → `SUM(1,2)`:
/// the formula source a `+ - @` prefix makes, when it parses.
fn formula_prefix(t: &str) -> Option<String> {
    let mut chars = t.chars();
    let first = chars.next()?;
    let rest = chars.as_str();
    if rest.is_empty() {
        return None;
    }
    let src = match first {
        '+' if rest.starts_with(|c: char| c.is_ascii_digit() || c == '.') => rest,
        '+' | '-' => t,
        '@' => rest,
        _ => return None,
    };
    crate::engine::Engine::validate(src).ok()?;
    Some(src.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-09-30 as a 1900-system serial.
    const TODAY: f64 = 45_565.0;

    fn ctx() -> EntryCtx {
        EntryCtx {
            date1904: false,
            today: Some(TODAY),
            fixed_decimal: None,
        }
    }

    fn general(text: &str) -> Entry {
        parse_entry(text, &Xf::default(), &ctx()).unwrap()
    }

    fn number(text: &str) -> (f64, Option<&'static str>) {
        let e = general(text);
        match e.cell.value {
            CellValue::Number(n) if e.cell.formula.is_none() => (n, e.format),
            other => panic!("{text:?} → {other:?} (formula {:?})", e.cell.formula),
        }
    }

    fn is_text_entry(text: &str) -> bool {
        let e = general(text);
        e.cell.formula.is_none() && e.cell.value == CellValue::Text(text.to_string())
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn numbers_with_separators_parentheses_percent_and_exponent() {
        let rows: &[(&str, f64, Option<&str>)] = &[
            ("42", 42.0, None),
            ("-2.5", -2.5, None),
            ("+5", 5.0, None),
            ("-5", -5.0, None),
            (".5", 0.5, None),
            ("1,234", 1234.0, Some("#,##0")),
            ("1,234.5", 1234.5, Some("#,##0.00")),
            ("1,234,567", 1_234_567.0, Some("#,##0")),
            ("(42)", -42.0, None),
            ("1,000%", 10.0, Some("0%")),
            ("(5%)", -0.05, Some("0%")),
            ("%5", 0.05, Some("0%")),
            ("50%", 0.5, Some("0%")),
            ("12.5%", 0.125, Some("0.00%")),
            ("1E3", 1000.0, Some("0.00E+00")),
            ("1.5e-3", 0.0015, Some("0.00E+00")),
        ];
        for &(text, want, fmt) in rows {
            let (n, f) = number(text);
            assert!(close(n, want), "{text}: {n} != {want}");
            assert_eq!(f, fmt, "{text}");
        }
    }

    #[test]
    fn currency_takes_excels_currency_code() {
        let whole = Some("$#,##0_);($#,##0)");
        let cents = Some("$#,##0.00_);($#,##0.00)");
        for (text, want, fmt) in [
            ("$5", 5.0, whole),
            ("$1,234.56", 1234.56, cents),
            ("-$5", -5.0, whole),
            ("$-5", -5.0, whole),
            ("($5)", -5.0, whole),
            ("$ 5", 5.0, whole),
        ] {
            let (n, f) = number(text);
            assert!(close(n, want), "{text}: {n}");
            assert_eq!(f, fmt, "{text}");
        }
        // And the code renders.
        let xf = Xf {
            code: whole.map(str::to_string),
            ..Xf::default()
        };
        assert_eq!(
            crate::sheet::format_with(&xf, &CellValue::Number(-5.0), false),
            "($5)"
        );
    }

    #[test]
    fn fractions_take_a_fraction_format() {
        for (text, want) in [("0 1/2", 0.5), ("1 1/4", 1.25), ("-1 1/2", -1.5)] {
            let (n, f) = number(text);
            assert!(close(n, want), "{text}: {n}");
            assert_eq!(f, Some("# ?/?"), "{text}");
        }
        assert_eq!(number("3 5/16").1, Some("# ??/??"));
    }

    #[test]
    fn malformed_numbers_stay_text() {
        for text in [
            "1,23", "$", "1 1/0", "1,,2", "1.2.3", "$5%", "(-5)", "1,234E3", "e5", "5e", "12abc",
            "%", "()", "1 1/2x",
        ] {
            assert!(is_text_entry(text), "{text:?} should stay text");
        }
    }

    #[test]
    fn dates_and_times_take_excels_formats() {
        let rows: &[(&str, f64, &str)] = &[
            ("1/15/2024", 45306.0, "m/d/yyyy"),
            ("1/15/24", 45306.0, "m/d/yyyy"),
            ("2024-01-15", 45306.0, "m/d/yyyy"),
            ("15-Jan-2024", 45306.0, "d-mmm-yy"),
            ("January 15, 2024", 45306.0, "d-mmm-yy"),
            ("Jan-2024", 45292.0, "mmm-yy"),
            ("9:30", 0.395833, "h:mm"),
            ("9:30 PM", 0.895833, "h:mm AM/PM"),
            ("9:30 p", 0.895833, "h:mm AM/PM"),
            ("25:00", 1.041667, "[h]:mm:ss"),
            ("12:00 AM", 0.0, "h:mm AM/PM"),
            ("9:30:15", 0.396007, "h:mm:ss"),
            ("1/15/2024 9:30", 45306.395833, "m/d/yyyy h:mm"),
        ];
        for &(text, want, fmt) in rows {
            let (n, f) = number(text);
            assert!(close(n, want), "{text}: {n} != {want}");
            assert_eq!(f, Some(fmt), "{text}");
        }
    }

    #[test]
    fn a_date_without_a_year_takes_the_clocks_year() {
        // 3/4 → 4 March 2024 (the clock's year).
        let (n, f) = number("3/4");
        assert_eq!(n, parts_to_serial(2024, 3, 4, 0, false));
        assert_eq!(f, Some("d-mmm"));
        // With no clock it stays text rather than guess.
        let no_clock = EntryCtx::default();
        let e = parse_entry("3/4", &Xf::default(), &no_clock).unwrap();
        assert_eq!(e.cell.value, CellValue::Text("3/4".into()));
    }

    #[test]
    fn two_digit_years_follow_excels_split() {
        assert_eq!(number("1/1/29").0, parts_to_serial(2029, 1, 1, 0, false));
        assert_eq!(number("1/1/30").0, parts_to_serial(1930, 1, 1, 0, false));
        assert_eq!(number("1/1/99").0, parts_to_serial(1999, 1, 1, 0, false));
        assert_eq!(number("1/1/00").0, parts_to_serial(2000, 1, 1, 0, false));
    }

    #[test]
    fn impossible_dates_and_times_stay_text() {
        for text in [
            "2/30/2024",
            "13/1/2024",
            "1/15/202",
            "9:75",
            "13:00 PM",
            "1/2/3/4",
            "1//2",
            "1/2-",
        ] {
            assert!(is_text_entry(text), "{text:?} should stay text");
        }
    }

    #[test]
    fn the_1904_system_counts_from_1904() {
        let c = EntryCtx {
            date1904: true,
            today: Some(TODAY),
            fixed_decimal: None,
        };
        let serial = |t: &str| match parse_entry(t, &Xf::default(), &c).unwrap().cell.value {
            CellValue::Number(n) => n,
            other => panic!("{other:?}"),
        };
        assert_eq!(serial("1/1/1904"), 0.0);
        assert_eq!(serial("1/15/2024"), 43844.0);
    }

    #[test]
    fn formula_prefixes() {
        for (text, src) in [
            ("+2+3", "2+3"),
            ("-2+3", "-2+3"),
            ("-L1", "-L1"),
            ("@SUM(1,2)", "SUM(1,2)"),
            ("+A99", "+A99"),
        ] {
            let e = general(text);
            assert_eq!(e.cell.formula.as_deref(), Some(src), "{text}");
        }
        // A doubled sign is a formula too, as in Excel.
        assert_eq!(general("--5").cell.formula.as_deref(), Some("--5"));
        // Plain signed numbers stay numbers; bare prefixes and bodies that
        // don't parse stay text.
        assert_eq!(number("+5").0, 5.0);
        assert_eq!(general("-1/2").cell.formula.as_deref(), Some("-1/2"));
        assert_eq!(number("-5").0, -5.0);
        for text in ["+", "-", "@", "+(", "-)x", "@@"] {
            assert!(is_text_entry(text), "{text:?} should stay text");
        }
    }

    #[test]
    fn formulas_booleans_errors_and_text() {
        assert_eq!(general("=A1+1").cell.formula.as_deref(), Some("A1+1"));
        assert!(is_text_entry("="));
        assert_eq!(general("true").cell.value, CellValue::Bool(true));
        assert_eq!(general("#n/a").cell.value, CellValue::Error("#N/A".into()));
        assert!(is_text_entry("hello"));
        assert_eq!(general("").cell, Cell::default());
    }

    #[test]
    fn a_leading_apostrophe_is_a_quote_prefix_not_text() {
        let e = general("'007");
        assert_eq!(e.cell.value, CellValue::Text("007".into()));
        assert!(e.quote_prefix);
        // An apostrophe alone is an empty text cell, still quote-prefixed.
        let e = general("'");
        assert_eq!(e.cell.value, CellValue::Text(String::new()));
        assert!(e.quote_prefix);
        assert!(!general("007").quote_prefix);
    }

    #[test]
    fn a_line_feed_asks_for_wrap() {
        let e = general("ab\ncd");
        assert!(e.wrap);
        assert_eq!(e.cell.value, CellValue::Text("ab\ncd".into()));
        assert!(!general("abcd").wrap);
    }

    #[test]
    fn the_length_limit_counts_utf16_units() {
        let at = "y".repeat(MAX_CELL_CHARS);
        assert!(parse_entry(&at, &Xf::default(), &ctx()).is_ok());
        let over = "y".repeat(MAX_CELL_CHARS + 1);
        assert_eq!(
            parse_entry(&over, &Xf::default(), &ctx()),
            Err(EntryError::TooLong {
                len: MAX_CELL_CHARS + 1
            })
        );
        // 16,384 surrogate pairs are 32,768 units, though only 16,384 chars.
        let pairs = "\u{1F600}".repeat(16_384);
        assert_eq!(pairs.chars().count(), 16_384);
        assert!(parse_entry(&pairs, &Xf::default(), &ctx()).is_err());
    }

    fn fmt_xf(code: &str) -> Xf {
        Xf {
            numfmt: classify_format_code(code),
            code: Some(code.to_string()),
            ..Xf::default()
        }
    }

    #[test]
    fn a_text_cell_takes_every_entry_as_typed() {
        let xf = fmt_xf("@");
        for text in ["007", "1/15/2024", "=1+2", "TRUE", "'x", "+2+3"] {
            let e = parse_entry(text, &xf, &ctx()).unwrap();
            assert_eq!(e.cell.value, CellValue::Text(text.into()), "{text}");
            assert!(e.cell.formula.is_none(), "{text}");
            assert!(!e.quote_prefix, "{text}");
        }
    }

    #[test]
    fn a_percent_cell_divides_a_typed_number_of_at_least_one() {
        let xf = fmt_xf("0.00%");
        let n = |t: &str| match parse_entry(t, &xf, &ctx()).unwrap().cell.value {
            CellValue::Number(n) => n,
            other => panic!("{other:?}"),
        };
        for (text, want) in [
            ("12.5", 0.125),
            ("1", 0.01),
            ("-3", -0.03),
            ("0.5", 0.5),
            ("0", 0.0),
            ("12.5%", 0.125),
        ] {
            assert!(close(n(text), want), "{text}: {}", n(text));
        }
    }

    #[test]
    fn format_predicates_read_codes_and_classifications() {
        assert!(is_general(&Xf::default()));
        assert!(is_general(&Xf {
            code: Some("General".into()),
            ..Xf::default()
        }));
        assert!(!is_general(&fmt_xf("0.00")));
        assert!(is_text(&fmt_xf("@")));
        assert!(is_text(&Xf {
            numfmt: NumFmt::Text,
            ..Xf::default()
        }));
        assert!(is_percent(&fmt_xf("0%")));
        assert!(is_percent(&Xf {
            numfmt: NumFmt::Percent { decimals: 2 },
            ..Xf::default()
        }));
        assert!(!is_percent(&fmt_xf("0.00")));
    }

    #[test]
    fn predicates_follow_the_code_over_a_stale_classification() {
        // What the suite's format setters used to leave: code changed,
        // numfmt not.
        let stale = |code: &str, numfmt: NumFmt| Xf {
            numfmt,
            code: Some(code.to_string()),
            ..Xf::default()
        };
        let comma_over_pct = stale("#,##0", NumFmt::Percent { decimals: 0 });
        assert!(!is_percent(&comma_over_pct));
        let e = parse_entry("5", &comma_over_pct, &ctx()).unwrap();
        assert_eq!(e.cell.value, CellValue::Number(5.0));
        let general_over_date = stale("General", NumFmt::Date);
        assert!(is_general(&general_over_date));
        assert!(!crate::sheet::date_unrepresentable(
            &general_over_date,
            &CellValue::Number(-1.0),
            false
        ));
        assert!(!is_text(&stale("0", NumFmt::Text)));
        assert!(is_text(&stale("@", NumFmt::General)));
        // set_code keeps the pair in step.
        let mut xf = Xf::default();
        xf.set_code(Some("0%".into()));
        assert_eq!(xf.numfmt, NumFmt::Percent { decimals: 0 });
        xf.set_code(None);
        assert_eq!((xf.numfmt, xf.code), (NumFmt::General, None));
    }

    #[test]
    fn a_recognised_format_lands_only_on_a_general_cell() {
        let e = general("1/15/2024");
        let on_general = entry_xf(&Xf::default(), &e);
        assert_eq!(on_general.code.as_deref(), Some("m/d/yyyy"));
        assert_eq!(on_general.numfmt, NumFmt::Date);
        // A cell that already has a format keeps it (#654: 5 into a date cell).
        let dated = fmt_xf("m/d/yyyy");
        let e = parse_entry("5", &dated, &ctx()).unwrap();
        assert_eq!(entry_xf(&dated, &e), dated);
        let fixed = fmt_xf("0.00");
        assert_eq!(entry_xf(&fixed, &general("50%")), fixed);
    }

    #[test]
    fn a_typed_date_switches_a_number_format_to_the_date_format() {
        let code = |base: &Xf, text: &str| {
            let e = parse_entry(text, base, &ctx()).unwrap();
            entry_xf(base, &e).code
        };
        let fixed = fmt_xf("0.00");
        let cents = fmt_xf("$#,##0.00_);($#,##0.00)");
        assert_eq!(code(&fixed, "1/15/2024").as_deref(), Some("m/d/yyyy"));
        assert_eq!(code(&cents, "1/15/2024").as_deref(), Some("m/d/yyyy"));
        assert_eq!(code(&fixed, "9:30").as_deref(), Some("h:mm"));
        assert_eq!(
            code(&fmt_xf("0%"), "1/15/2024").as_deref(),
            Some("m/d/yyyy")
        );
        // A date or time format stays: the entry is shown in the cell's own.
        assert_eq!(code(&fmt_xf("h:mm"), "1/15/2024").as_deref(), Some("h:mm"));
        assert_eq!(
            code(&fmt_xf("d-mmm"), "9:30").as_deref(),
            Some("d-mmm"),
            "a date cell keeps its format for a time too"
        );
        // Only dates switch a number format; other shapes still need General.
        assert_eq!(code(&fixed, "$5").as_deref(), Some("0.00"));
        assert_eq!(code(&fixed, "1 1/4").as_deref(), Some("0.00"));
        assert!(is_date(&fmt_xf("m/d/yyyy")) && is_date(&fmt_xf("[h]:mm")));
        assert!(is_date(&Xf {
            numfmt: NumFmt::DateTime,
            ..Xf::default()
        }));
        assert!(!is_date(&fixed) && !is_date(&Xf::default()) && !is_date(&fmt_xf("@")));
    }

    #[test]
    fn a_long_number_keeps_fifteen_significant_digits() {
        let n = |t: &str| match parse_entry(t, &Xf::default(), &ctx()).unwrap().cell.value {
            CellValue::Number(n) => n,
            v => panic!("{t}: {v:?}"),
        };
        assert_eq!(n("1234567890123456789"), 1.23456789012346e18);
        assert_eq!(n("0.1234567890123456789"), 0.123456789012346);
        assert_eq!(n("123456789012345"), 123_456_789_012_345.0);
        assert_eq!(round15(0.1 + 0.2), 0.3);
        assert_eq!(round15(0.0), 0.0);
    }

    #[test]
    fn entry_style_keeps_the_index_when_nothing_changes() {
        let mut styles = Styles::default();
        let bold = styles.intern(Xf {
            bold: true,
            ..Xf::default()
        });
        assert_eq!(entry_style(&mut styles, bold, &general("hello")), bold);
        // A quote prefix interns a sibling that keeps the bold.
        let q = entry_style(&mut styles, bold, &general("'007"));
        assert_ne!(q, bold);
        assert!(styles.xf(q).bold && styles.xf(q).quote_prefix);
        // Plain input into the quote-prefixed cell clears it again.
        let back = entry_style(&mut styles, q, &general("7"));
        assert!(!styles.xf(back).quote_prefix);
        assert!(styles.xf(back).bold);
    }

    #[test]
    fn styled_input_text_puts_the_apostrophe_back() {
        let quoted = Xf {
            quote_prefix: true,
            ..Xf::default()
        };
        assert_eq!(input_text_styled(&Cell::text("007"), &quoted), "'007");
        assert_eq!(input_text_styled(&Cell::text("007"), &Xf::default()), "007");
        assert_eq!(input_text_styled(&Cell::number(7.0), &quoted), "7");
        // Re-entering the styled text gives back the same cell and prefix.
        let e = parse_entry("'007", &quoted, &ctx()).unwrap();
        assert_eq!(e.cell, Cell::text("007"));
        assert_eq!(entry_xf(&quoted, &e), quoted);
    }

    #[test]
    fn loaded_styles_drive_the_rules() {
        // numFmtId 0 / 49 / 10 exactly as a file's styles.xml gives them.
        let mut pkg = crate::xlsx::new_xlsx();
        let styles_path = "xl/styles.xml";
        let xml = String::from_utf8(pkg.part(styles_path).unwrap().to_vec()).unwrap();
        let xml = xml.replacen(
            "</cellXfs>",
            "<xf numFmtId=\"49\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>\
             <xf numFmtId=\"10\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/></cellXfs>",
            1,
        );
        pkg.set_part(styles_path, xml.into_bytes());
        let pkg = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let styles = &pkg.workbook.styles;
        let n = styles.xfs.len() as u32;
        let (general_xf, text_xf, pct_xf) = (styles.xf(0), styles.xf(n - 2), styles.xf(n - 1));
        assert!(is_general(&general_xf), "{general_xf:?}");
        assert!(is_text(&text_xf), "{text_xf:?}");
        assert!(is_percent(&pct_xf), "{pct_xf:?}");
        let e = parse_entry("007", &text_xf, &ctx()).unwrap();
        assert_eq!(e.cell.value, CellValue::Text("007".into()));
        let e = parse_entry("12.5", &pct_xf, &ctx()).unwrap();
        assert_eq!(e.cell.value, CellValue::Number(0.125));
    }

    #[test]
    fn entry_range_moves_a_prefixed_formula_too() {
        let mut pkg = crate::xlsx::new_xlsx();
        let wb = &mut pkg.workbook;
        for (text, want) in [
            ("+A1", "+A2"),
            ("-B1", "-B2"),
            ("@SUM(A1:A2)", "SUM(A2:A3)"),
        ] {
            let cells = entry_range(wb, 0, (0, 3, 1, 3), (0, 3), text, None).unwrap();
            assert_eq!(cells[1].2.formula.as_deref(), Some(want), "{text}");
        }
    }

    #[test]
    fn a_text_starting_with_an_apostrophe_survives_seed_copy_and_paste() {
        let mut styles = Styles::default();
        let general = styles.intern(Xf::default());
        let text_fmt = styles.intern(Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            ..Xf::default()
        });
        let loaded = Cell::text("'abc");
        // General, no prefix: the seed escapes it, and both re-entry and
        // paste give back 'abc.
        let seed = seed_text(&loaded, &Xf::default());
        assert_eq!(seed, "''abc");
        let typed = parse_entry(&seed, &Xf::default(), &ctx()).unwrap();
        assert_eq!(typed.cell.value, CellValue::Text("'abc".into()));
        assert_eq!(
            paste_cell(&mut styles, general, &seed, &ctx()).value,
            CellValue::Text("'abc".into())
        );
        // A Text cell needs no escape, and a paste into one is as typed.
        let in_text = Cell {
            style: text_fmt,
            ..loaded.clone()
        };
        let seed = seed_text(&in_text, &styles.xf(text_fmt));
        assert_eq!(seed, "'abc");
        let pasted = paste_cell(&mut styles, text_fmt, &seed, &ctx());
        assert_eq!(pasted.value, CellValue::Text("'abc".into()));
        assert!(!styles.xf(pasted.style).quote_prefix);
        assert_eq!(
            paste_cell(&mut styles, text_fmt, "007", &ctx()).value,
            CellValue::Text("007".into())
        );
    }

    #[test]
    fn a_text_cell_never_takes_an_apostrophe_even_with_a_quote_prefix() {
        // `'007` typed into General, then the cell formatted `@`.
        let mut styles = Styles::default();
        let general = styles.intern(Xf::default());
        let text_q = styles.intern(Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            quote_prefix: true,
            ..Xf::default()
        });
        let cell = Cell {
            style: text_q,
            ..Cell::text("007")
        };
        let xf = styles.xf(text_q);
        assert!(!needs_apostrophe(&cell, &xf));
        assert_eq!(seed_text(&cell, &xf), "007");
        let again = parse_entry("007", &xf, &ctx()).unwrap();
        assert_eq!(again.cell.value, CellValue::Text("007".into()));
        assert_eq!(copy_field(&cell, &xf, "007".into()), "007");
        assert_eq!(
            paste_cell(&mut styles, text_q, "007", &ctx()).value,
            CellValue::Text("007".into())
        );
        // An empty field clears, in a Text cell as anywhere.
        let paste = |styles: &mut Styles, style, text| paste_cell(styles, style, text, &ctx());
        assert_eq!(paste(&mut styles, text_q, "").value, CellValue::Empty);
        assert_eq!(paste(&mut styles, general, "").value, CellValue::Empty);
    }

    #[test]
    fn a_replaced_entry_decides_its_apostrophe_from_the_result() {
        let q = Xf {
            quote_prefix: true,
            ..Xf::default()
        };
        let g = Xf::default();
        let text = |s: &str| Cell::text(s);
        assert_eq!(replaced_entry(&text("117"), &q, "117".into()), "'117");
        assert_eq!(replaced_entry(&text("'5"), &g, "5".into()), "5");
        assert_eq!(replaced_entry(&text("'abc"), &g, "xabc".into()), "xabc");
        assert_eq!(replaced_entry(&text("x'abc"), &g, "'abc".into()), "''abc");
        assert_eq!(replaced_entry(&text("007"), &q, String::new()), "");
        let text_fmt = Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            ..Xf::default()
        };
        assert_eq!(replaced_entry(&text("x"), &text_fmt, "'y".into()), "'y");
        assert_eq!(replaced_entry(&Cell::number(5.0), &g, "'5".into()), "'5");
    }

    #[test]
    fn cross_format_paste_is_lossy_for_apostrophes() {
        // The rule for text from OUTSIDE the host: a TSV carries no format,
        // so the field is read as typed into the target, as Excel reads
        // pasted text. (A host's own copy pastes the cells themselves.)
        let mut styles = Styles::default();
        let general = styles.intern(Xf::default());
        let text = styles.intern(Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            ..Xf::default()
        });
        let from_text = Cell {
            style: text,
            ..Cell::text("'abc")
        };
        let field = copy_field(&from_text, &styles.xf(text), "'abc".into());
        assert_eq!(field, "'abc");
        assert_eq!(
            paste_cell(&mut styles, general, &field, &ctx()).value,
            CellValue::Text("abc".into())
        );
        let from_general = Cell::text("'abc");
        let field = copy_field(&from_general, &Xf::default(), "'abc".into());
        assert_eq!(field, "''abc");
        assert_eq!(
            paste_cell(&mut styles, text, &field, &ctx()).value,
            CellValue::Text("''abc".into())
        );
    }

    #[test]
    fn reentry_reads_a_replaced_text_in_a_percent_cell_as_typed() {
        let mut styles = Styles::default();
        let pct = styles.intern(Xf {
            numfmt: NumFmt::Percent { decimals: 0 },
            code: Some("0%".into()),
            ..Xf::default()
        });
        let tbd = Cell {
            style: pct,
            ..Cell::text("TBD")
        };
        let c = reenter_cell(&tbd, &mut styles, &ctx(), "TBD", "5").unwrap();
        assert_eq!(c.value, CellValue::Number(0.05));
    }

    #[test]
    fn reentry_divides_a_percent_only_when_the_edited_text_showed_it() {
        let mut styles = Styles::default();
        let pct = styles.intern(Xf {
            numfmt: NumFmt::Percent { decimals: 0 },
            code: Some("0%".into()),
            ..Xf::default()
        });
        let cell = Cell {
            style: pct,
            ..Cell::number(1.5)
        };
        let n = |old: &str, new: &str, styles: &mut Styles| match reenter_cell(
            &cell,
            styles,
            &ctx(),
            old,
            new,
        )
        .unwrap()
        .value
        {
            CellValue::Number(n) => n,
            other => panic!("{other:?}"),
        };
        // Input text (xlsxy / wb.replace-all): the plain value.
        assert!(close(n("1.5", "1.6", &mut styles), 1.6));
        // Display text (the suite): with and without its %.
        assert!(close(n("150%", "160%", &mut styles), 1.6));
        assert!(close(n("150%", "150", &mut styles), 1.5));
    }

    #[test]
    fn a_pasted_apostrophe_is_a_quote_prefix() {
        let mut styles = Styles::default();
        let bold = styles.intern(Xf {
            bold: true,
            ..Xf::default()
        });
        let c = paste_cell(&mut styles, bold, "'007", &ctx());
        assert_eq!(c.value, CellValue::Text("007".into()));
        let xf = styles.xf(c.style);
        assert!(xf.quote_prefix && xf.bold);
        // A plain field is read as typed and clears the prefix.
        let n = paste_cell(&mut styles, c.style, "007", &ctx());
        assert_eq!(n.value, CellValue::Number(7.0));
        assert_eq!(n.style, bold);
    }

    #[test]
    fn pasted_text_is_read_like_typed_entry() {
        let mut styles = Styles::default();
        let general = styles.intern(Xf::default());
        let mut paste = |style: u32, text: &str| {
            let c = paste_cell(&mut styles, style, text, &ctx());
            (c.value.clone(), styles.xf(c.style).code, c)
        };
        // Recognised shapes bring the format typing would give them (this
        // was text while paste read values only).
        let (v, code, _) = paste(general, "1/15/2024");
        assert_eq!(
            (v, code.as_deref()),
            (CellValue::Number(45_306.0), Some("m/d/yyyy"))
        );
        let (v, code, _) = paste(general, "$5");
        assert_eq!(v, CellValue::Number(5.0));
        assert!(code.is_some_and(|c| c.contains('$')));
        let (v, code, _) = paste(general, "1,234");
        assert_eq!(
            (v, code.as_deref()),
            (CellValue::Number(1234.0), Some("#,##0"))
        );
        let (v, code, _) = paste(general, "50%");
        assert_eq!((v, code.as_deref()), (CellValue::Number(0.5), Some("0%")));
        // A yearless date takes the clock's year.
        let (v, _, _) = paste(general, "3/4");
        assert!(matches!(v, CellValue::Number(n) if n > 45_000.0), "{v:?}");
        // A formula stays a formula (hosts demote one that does not parse).
        let (_, _, c) = paste(general, "=A1+1");
        assert_eq!(c.formula.as_deref(), Some("A1+1"));
        let (_, _, c) = paste(general, "-B2");
        assert_eq!(c.formula.as_deref(), Some("-B2"));
        // Paste never turns on Wrap Text.
        let (_, _, c) = paste(general, "a\nb");
        assert!(!styles.xf(c.style).wrap);
        let pct = styles.intern(Xf {
            numfmt: NumFmt::Percent { decimals: 0 },
            code: Some("0%".into()),
            ..Xf::default()
        });
        let text_fmt = styles.intern(Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            ..Xf::default()
        });
        let mut paste = |style: u32, text: &str| {
            let c = paste_cell(&mut styles, style, text, &ctx());
            (c.value.clone(), styles.xf(c.style).code)
        };
        // A percent target divides a plain number as typing does.
        assert_eq!(paste(pct, "50").0, CellValue::Number(0.5));
        // A Text target keeps the field as it is.
        assert_eq!(
            paste(text_fmt, "1/15/2024"),
            (CellValue::Text("1/15/2024".into()), Some("@".into()))
        );
        // Over the cell limit: kept as text, unread, on the target's style.
        let long = "1".repeat(MAX_CELL_CHARS + 1);
        assert_eq!(paste(general, &long), (CellValue::Text(long.clone()), None));
    }

    #[test]
    fn a_seed_keeps_every_digit_of_a_number() {
        let seed = |n: f64| seed_text(&Cell::number(n), &Xf::default());
        assert_eq!(seed(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(seed(42.0), "42");
        assert_eq!(seed(-1.5), "-1.5");
        assert_eq!(seed(0.0), "0");
        // Plain digits only where they stay short.
        assert_eq!(seed(1e20), "100000000000000000000");
        assert_eq!(seed(1e300), "1E300");
        assert_eq!(seed(-1.5e-300), "-1.5E-300");
        // Each of these (15 significant digits or fewer) reads back through
        // typed entry to the number it shows.
        for n in [1e300, -1.5e-300, 1e21, 123.25] {
            let e = parse_entry(&seed(n), &Xf::default(), &ctx()).unwrap();
            assert_eq!(e.cell.value, CellValue::Number(n), "{n}");
        }
        // The value, not the format: an E-notation seed re-entered into a
        // General cell brings the scientific format with it.
        let e = parse_entry(&seed(1e300), &Xf::default(), &ctx()).unwrap();
        assert_eq!(
            entry_xf(&Xf::default(), &e).code.as_deref(),
            Some("0.00E+00")
        );
        let e = parse_entry(&seed(123.25), &Xf::default(), &ctx()).unwrap();
        assert!(is_general(&entry_xf(&Xf::default(), &e)));
        // Text keeps its re-entry apostrophe; a formula shows its source.
        let quoted = Xf {
            quote_prefix: true,
            ..Xf::default()
        };
        assert_eq!(seed_text(&Cell::text("007"), &quoted), "'007");
        let f = Cell {
            value: CellValue::Number(3.0),
            ..Cell::formula("1+2")
        };
        assert_eq!(seed_text(&f, &Xf::default()), "=1+2");
    }

    #[test]
    fn a_percent_cell_seeds_the_percent_it_reads_back_as() {
        let pct = fmt_xf("0%");
        let seed = |n: f64| seed_text(&Cell::number(n), &pct);
        assert_eq!(seed(1.5), "150%");
        assert_eq!(seed(0.07), "7%");
        assert_eq!(seed(0.5), "50%");
        assert_eq!(seed(-0.125), "-12.5%");
        assert_eq!(seed(0.0), "0%");
        for n in [1.5, 0.07, 0.123456789, -0.125, 1e-12] {
            let text = seed(n);
            let back = parse_entry(&text, &pct, &ctx()).unwrap().cell.value;
            assert_eq!(back, CellValue::Number(n), "{n} seeds {text}");
        }
        // A value no 15-digit entry can give back is shown in full. Only a
        // host that leaves an unedited seed uncommitted (xlsxy, the suite)
        // keeps it exactly; re-entered, it rounds to 15 digits.
        assert_eq!(seed(2.0 / 3.0), "66.66666666666666%");
        // Edited, it reads as typed there: 160% is 1.6, not 0.016.
        let back = parse_entry("160%", &pct, &ctx()).unwrap().cell.value;
        assert_eq!(back, CellValue::Number(1.6));
    }

    #[test]
    fn typed_formula_skips_a_text_cell() {
        let mut pkg = crate::xlsx::new_xlsx();
        let wb = &mut pkg.workbook;
        assert_eq!(typed_formula(wb, 0, 0, 0, "=SUM("), Some("SUM("));
        assert_eq!(typed_formula(wb, 0, 0, 0, "="), None);
        assert_eq!(typed_formula(wb, 0, 0, 0, "abc"), None);
        let text = wb.styles.intern(Xf {
            numfmt: NumFmt::Text,
            code: Some("@".into()),
            ..Xf::default()
        });
        wb.sheets[0].set_cell(
            0,
            0,
            Cell {
                style: text,
                ..Cell::default()
            },
        );
        assert_eq!(typed_formula(wb, 0, 0, 0, "=SUM("), None);
        let cell = entry_cell(wb, 0, 0, 0, "=SUM(", None).unwrap();
        assert_eq!(cell.value, CellValue::Text("=SUM(".into()));
    }

    #[test]
    fn entry_range_moves_a_formula_with_each_cell() {
        let mut pkg = crate::xlsx::new_xlsx();
        let wb = &mut pkg.workbook;
        let cells = entry_range(wb, 0, (0, 1, 2, 1), (0, 1), "=A1*10", None).unwrap();
        let formulas: Vec<_> = cells.iter().map(|(_, _, c)| c.formula.clone()).collect();
        assert_eq!(
            formulas,
            vec![
                Some("A1*10".to_string()),
                Some("A2*10".to_string()),
                Some("A3*10".to_string())
            ]
        );
        // The active cell need not be the top-left one.
        let cells = entry_range(wb, 0, (0, 1, 1, 1), (1, 1), "=A2", None).unwrap();
        assert_eq!(cells[0].2.formula.as_deref(), Some("A1"));
        let cells = entry_range(wb, 0, (0, 0, 0, 1), (0, 0), "5", None).unwrap();
        assert!(
            cells
                .iter()
                .all(|(_, _, c)| c.value == CellValue::Number(5.0))
        );
    }

    #[test]
    fn entry_cell_resolves_the_cells_style_in_the_workbook() {
        let mut pkg = crate::xlsx::new_xlsx();
        let wb = &mut pkg.workbook;
        let cell = entry_cell(wb, 0, 0, 0, "1,234", Some(TODAY)).unwrap();
        assert_eq!(cell.value, CellValue::Number(1234.0));
        assert_eq!(wb.styles.xf(cell.style).code.as_deref(), Some("#,##0"));
        let err = entry_cell(wb, 0, 0, 0, &"y".repeat(MAX_CELL_CHARS + 1), None);
        assert!(matches!(err, Err(EntryError::TooLong { .. })));
    }

    // ---- #672: fixed decimal ----

    fn fixed(text: &str, places: i16, xf: &Xf) -> Entry {
        let c = EntryCtx {
            fixed_decimal: Some(places),
            ..ctx()
        };
        parse_entry(text, xf, &c).unwrap()
    }

    fn fixed_number(text: &str, places: i16) -> f64 {
        match fixed(text, places, &Xf::default()).cell.value {
            CellValue::Number(n) => n,
            other => panic!("{text:?} → {other:?}"),
        }
    }

    #[test]
    fn fixed_decimal_shifts_a_typed_whole_number() {
        assert_eq!(fixed_number("1234", 2), 12.34);
        assert_eq!(fixed_number("1234", -2), 123_400.0);
        assert_eq!(fixed_number("1234", 0), 1234.0);
        assert_eq!(fixed_number("-1,234", 2), -12.34);
        assert_eq!(fixed_number("(1234)", 2), -12.34);
        assert_eq!(fixed_number("$1,234", 2), 12.34);
        assert_eq!(fixed_number("+5", 3), 0.005);
        // Off: the number as typed.
        assert_eq!(number("1234").0, 1234.0);
    }

    #[test]
    fn fixed_decimal_leaves_a_typed_point_percent_exponent_and_non_numbers() {
        for (text, want) in [
            ("1.5", 1.5),
            ("1.", 1.0),
            (".5", 0.5),
            ("1,234.5", 1234.5),
            ("5%", 0.05),
            ("1E3", 1000.0),
        ] {
            assert_eq!(fixed_number(text, 2), want, "{text}");
        }
        let general = Xf::default();
        let date = fixed("3/4/2024", 2, &general);
        assert_eq!(
            date.format,
            Some("m/d/yyyy"),
            "a date is not a number typed"
        );
        assert_eq!(
            fixed("=1234", 2, &general).cell.formula.as_deref(),
            Some("1234")
        );
        assert_eq!(
            fixed("abc", 2, &general).cell.value,
            CellValue::Text("abc".into())
        );
        let text_cell = Xf {
            numfmt: NumFmt::Text,
            ..Xf::default()
        };
        assert_eq!(
            fixed("1234", 2, &text_cell).cell.value,
            CellValue::Text("1234".into())
        );
    }

    #[test]
    fn fixed_decimal_shifts_before_the_percent_cell_rule() {
        let pct = Xf {
            numfmt: NumFmt::Percent { decimals: 0 },
            ..Xf::default()
        };
        // 5 → 0.05, which the percent cell no longer divides: 5%.
        assert_eq!(fixed("5", 2, &pct).cell.value, CellValue::Number(0.05));
        // 500 → 5, which it divides as any typed 5: 5%.
        assert_eq!(fixed("500", 2, &pct).cell.value, CellValue::Number(0.05));
    }

    #[test]
    fn fixed_decimal_keeps_15_digits_and_never_overflows() {
        assert_eq!(fixed_number("1", 300), 1e-300);
        assert_eq!(fixed_number("123456789012345", 2), 1_234_567_890_123.45);
        // 1E+300 × 1E+300 is infinite: the number stays as typed.
        let big = format!("1{}", "0".repeat(300));
        assert_eq!(fixed_number(&big, -300), 1e300);
    }

    #[test]
    fn only_the_ctx_entry_points_shift() {
        let mut pkg = crate::xlsx::new_xlsx();
        let wb = &mut pkg.workbook;
        let c = EntryCtx {
            fixed_decimal: Some(2),
            ..entry_ctx(wb, None)
        };
        let typed = entry_cell_ctx(wb, 0, 0, 0, "1234", &c).unwrap();
        assert_eq!(typed.value, CellValue::Number(12.34));
        // Automation, the DV pick and every other caller of `entry_cell`.
        let plain = entry_cell(wb, 0, 0, 0, "1234", None).unwrap();
        assert_eq!(plain.value, CellValue::Number(1234.0));
        let cells = entry_range(wb, 0, (0, 0, 1, 0), (0, 0), "1234", None).unwrap();
        assert!(
            cells
                .iter()
                .all(|(_, _, c)| c.value == CellValue::Number(1234.0))
        );
        // Paste and Find & Replace build their own ctx: never shifted.
        let ctx0 = entry_ctx(wb, None);
        let pasted = paste_cell(&mut wb.styles, 0, "1234", &ctx0);
        assert_eq!(pasted.value, CellValue::Number(1234.0));
    }

    #[test]
    fn a_typed_ctrl_enter_shifts_each_cell_under_its_own_format() {
        let mut pkg = crate::xlsx::new_xlsx();
        let wb = &mut pkg.workbook;
        let text_style = wb.styles.intern(Xf {
            numfmt: NumFmt::Text,
            ..Xf::default()
        });
        wb.sheets[0].set_cell(
            1,
            0,
            Cell {
                style: text_style,
                ..Cell::default()
            },
        );
        let c = EntryCtx {
            fixed_decimal: Some(2),
            ..entry_ctx(wb, None)
        };
        let cells = entry_range_ctx(wb, 0, (0, 0, 1, 0), (0, 0), "1234", &c).unwrap();
        assert_eq!(cells[0].2.value, CellValue::Number(12.34));
        assert_eq!(cells[1].2.value, CellValue::Text("1234".into()));
    }

    // ---- #672: AutoComplete ----

    fn column(values: &[Option<&str>]) -> Sheet {
        let mut sh = Sheet::default();
        for (r, v) in values.iter().enumerate() {
            match v {
                Some(t) if t.starts_with('=') => sh.set_cell(r as u32, 0, Cell::formula(&t[1..])),
                Some(t) => match t.parse::<f64>() {
                    Ok(n) => sh.set_cell(r as u32, 0, Cell::number(n)),
                    Err(_) => sh.set_cell(r as u32, 0, Cell::text(t)),
                },
                None => {}
            }
        }
        sh
    }

    #[test]
    fn autocomplete_proposes_the_one_matching_text_of_the_block() {
        let sh = column(&[
            Some("Apple"),
            Some("Banana"),
            Some("42"),
            None,
            Some("Cherry"),
        ]);
        assert_eq!(autocomplete(&sh, 5, 0, "b"), None, "a gap ends the block");
        assert_eq!(autocomplete(&sh, 5, 0, "c").as_deref(), Some("Cherry"));
        let sh = column(&[Some("Apple"), Some("Banana"), Some("42"), None]);
        assert_eq!(autocomplete(&sh, 3, 0, "b").as_deref(), Some("Banana"));
        assert_eq!(autocomplete(&sh, 3, 0, "AP").as_deref(), Some("Apple"));
        assert_eq!(
            autocomplete(&sh, 3, 0, "4"),
            None,
            "numbers are never proposed"
        );
        assert_eq!(autocomplete(&sh, 3, 0, ""), None);
        assert_eq!(autocomplete(&sh, 3, 0, "=a"), None);
        assert_eq!(autocomplete(&sh, 3, 0, "x"), None);
        assert_eq!(autocomplete(&sh, 3, 0, "apple"), None, "already a value");
        // Below the cell counts too, up to its gap.
        let sh = column(&[None, None, Some("Kiwi"), None]);
        assert_eq!(autocomplete(&sh, 1, 0, "k").as_deref(), Some("Kiwi"));
    }

    #[test]
    fn autocomplete_excludes_the_edited_cell_and_needs_one_distinct_value() {
        let sh = column(&[Some("Apple"), Some("Apricot"), Some("Avocado")]);
        assert_eq!(autocomplete(&sh, 1, 0, "ap").as_deref(), Some("Apple"));
        assert_eq!(autocomplete(&sh, 3, 0, "ap"), None, "two values match");
        assert_eq!(autocomplete(&sh, 0, 0, "a"), None);
        // Only the edited cell itself matches: nothing to propose.
        let sh = column(&[Some("Pear"), Some("Plum")]);
        assert_eq!(autocomplete(&sh, 0, 0, "pe"), None);
        // Case-only differences are one value, spelled as found nearest above.
        let sh = column(&[Some("apple"), Some("APPLE"), None, Some("x")]);
        assert_eq!(autocomplete(&sh, 2, 0, "a").as_deref(), Some("APPLE"));
        // A text a typed entry would read as something else is not a
        // proposal: it would commit as a number, a boolean or a date.
        let held_as_text = |t: &str| {
            let mut sh = Sheet::default();
            sh.set_cell(0, 0, Cell::text(t));
            sh
        };
        for t in ["007", "TRUE", "3/4", "1E5", "'x"] {
            assert_eq!(autocomplete(&held_as_text(t), 1, 0, &t[..1]), None, "{t}");
        }
        let sh = held_as_text("0abc");
        assert_eq!(autocomplete(&sh, 1, 0, "0").as_deref(), Some("0abc"));
        // A value that would not stay text still is one of the values: `007`
        // typed over a block holding `007` and `007x` proposes nothing.
        let mut sh = held_as_text("007");
        sh.set_cell(1, 0, Cell::text("007x"));
        assert_eq!(autocomplete(&sh, 2, 0, "007"), None);
        // A formula's text result is not a proposal.
        let mut sh = column(&[Some("=\"x\"")]);
        sh.cells.get_mut(&(0, 0)).unwrap().value = CellValue::Text("xyz".into());
        assert_eq!(autocomplete(&sh, 1, 0, "x"), None);
    }
}
