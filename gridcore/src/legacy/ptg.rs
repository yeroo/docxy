//! The formula-token (`Ptg`) decompiler shared by the BIFF8 (`.xls`) and
//! BIFF12 (`.xlsb`) readers: a parsed-expression byte stream (`rgce`, in
//! reverse Polish order) back to A1 formula text that [`crate::formula::parse`]
//! reads.
//!
//! The two formats use the same token ids and the same layouts except for
//! widths: BIFF8 rows are 16-bit and BIFF12 rows 32-bit; both keep the
//! column in the low 14 bits of a 16-bit word whose top two bits flag a
//! relative column (bit 14) and row (bit 15). Strings are 8/16-bit
//! `ShortXLUnicodeString`s in BIFF8 and UTF-16 with a 16-bit count in BIFF12.
//!
//! Parentheses come only from `ptgParen`, as Excel wrote them. Any token
//! this doesn't know makes the whole formula `None`: the reader then keeps
//! the cell's cached value without a formula.

use super::{biff_error, ftab};

/// Which record format a token stream comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Biff {
    V8,
    V12,
}

impl Biff {
    fn max_row(self) -> u32 {
        match self {
            Biff::V8 => 0xFFFF,
            Biff::V12 => 0xF_FFFF,
        }
    }
    fn max_col(self) -> u32 {
        match self {
            Biff::V8 => 0xFF,
            Biff::V12 => 0x3FFF,
        }
    }
}

/// What a token stream refers to outside itself: the workbook's sheets
/// (through the XTI table), its defined names and external names.
pub(crate) trait Names {
    /// The sheet prefix (`Data!`, `'Q1:Q3'!`) of XTI entry `ixti`; `None`
    /// for a reference into another workbook or a missing entry.
    fn xti(&self, ixti: u32) -> Option<String>;
    /// The defined name with 1-based index `index`. A hidden function name
    /// (`_xlfn.MAXIFS`) comes back as written.
    fn name(&self, index: u32) -> Option<String>;
    /// The external name `index` (1-based) of XTI entry `ixti`: an add-in
    /// function such as `EDATE`.
    fn name_x(&self, ixti: u32, index: u32) -> Option<String>;
    /// The table with id `id` (BIFF12 only; `.xls` has no tables).
    fn table(&self, _id: u32) -> Option<Table> {
        None
    }
}

/// What a structured reference needs of its table: where it is.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    /// The table's sheet as a reference prefix (`Orders!`).
    pub prefix: String,
    /// Its range, header row and totals row included: (r1, r2, c1, c2).
    pub range: (u32, u32, u32, u32),
    /// Header rows (0 or 1) and totals rows (0 or 1).
    pub header: u32,
    pub totals: u32,
}

/// Where a token stream's relative references are anchored.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Base {
    /// A cell's own formula (at the cell, when there is one: a defined name
    /// has none): references are absolute coordinates.
    Cell(Option<(u32, u32)>),
    /// A shared formula evaluated at (row, col): the `N` tokens, and the
    /// relative parts of 3D ones, are offsets from it.
    Shared(u32, u32),
}

/// A little-endian reader over a byte slice.
struct Rd<'a> {
    b: &'a [u8],
    at: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn f64(&mut self) -> Option<f64> {
        let s = self.take(8)?;
        Some(f64::from_le_bytes(s.try_into().ok()?))
    }
    fn row(&mut self, v: Biff) -> Option<u32> {
        match v {
            Biff::V8 => self.u16().map(u32::from),
            Biff::V12 => self.u32(),
        }
    }
    /// A BIFF8 `ShortXLUnicodeString` (8-bit count) or `XLUnicodeString`
    /// (16-bit count), or a BIFF12 UTF-16 string with a 16- or 32-bit count.
    fn string(&mut self, v: Biff, wide_count: bool) -> Option<String> {
        match v {
            Biff::V8 => {
                let cch = if wide_count {
                    self.u16()? as usize
                } else {
                    self.u8()? as usize
                };
                let high = self.u8()? & 1 == 1;
                if high {
                    let raw = self.take(cch * 2)?;
                    Some(utf16(raw))
                } else {
                    Some(self.take(cch)?.iter().map(|&c| c as char).collect())
                }
            }
            Biff::V12 => {
                let cch = if wide_count {
                    self.u32()? as usize
                } else {
                    self.u16()? as usize
                };
                Some(utf16(self.take(cch.checked_mul(2)?)?))
            }
        }
    }
}

/// UTF-16LE bytes as a string (lone surrogates replaced).
pub(crate) fn utf16(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// A formula string literal: `"a""b"`.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A number as formula text (shortest round-trip, no trailing `.0`).
fn num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// The longest token stream decompiled (Excel writes at most a few KB).
const MAX_RGCE: usize = 16 << 10;
/// The longest formula text produced, in characters (Excel's limit).
const MAX_TEXT: usize = 8_192;
/// The byte length past which a formula can't be within [`MAX_TEXT`]
/// characters (UTF-8 is at most 4 bytes a character): the runaway guard,
/// which can't count characters cheaply on every token.
const MAX_TEXT_BYTES: usize = 4 * MAX_TEXT;

/// Decompile `rgce` (with its trailing extra data `extra`, which holds array
/// constants) to formula text without the leading `=`.
pub(crate) fn decompile(
    v: Biff,
    rgce: &[u8],
    extra: &[u8],
    base: Base,
    names: &dyn Names,
) -> Option<String> {
    // Excel's own limits are far below these; past them a token stream is
    // corrupt, and building ever longer strings from it would be quadratic.
    if rgce.len() > MAX_RGCE {
        return None;
    }
    let mut rd = Rd { b: rgce, at: 0 };
    let mut ex = Rd { b: extra, at: 0 };
    let mut st: Vec<String> = Vec::new();
    while rd.at < rgce.len() {
        if st.last().is_some_and(|t| t.len() > MAX_TEXT_BYTES) {
            return None;
        }
        let ptg = rd.u8()?;
        match ptg {
            // Binary operators.
            0x03..=0x11 => {
                let op = match ptg {
                    0x03 => "+",
                    0x04 => "-",
                    0x05 => "*",
                    0x06 => "/",
                    0x07 => "^",
                    0x08 => "&",
                    0x09 => "<",
                    0x0A => "<=",
                    0x0B => "=",
                    0x0C => ">=",
                    0x0D => ">",
                    0x0E => "<>",
                    0x0F => " ",
                    0x10 => ",",
                    _ => ":",
                };
                let b = st.pop()?;
                let a = st.pop()?;
                st.push(format!("{a}{op}{b}"));
            }
            0x12 => {
                let a = st.pop()?;
                st.push(format!("+{a}"));
            }
            0x13 => {
                let a = st.pop()?;
                st.push(format!("-{a}"));
            }
            0x14 => {
                let a = st.pop()?;
                st.push(format!("{a}%"));
            }
            0x15 => {
                let a = st.pop()?;
                st.push(format!("({a})"));
            }
            // ptgMissArg: an omitted argument.
            0x16 => st.push(String::new()),
            0x17 => st.push(quote(&rd.string(v, false)?)),
            0x19 => {
                let flags = rd.u8()?;
                let data = rd.u16()?;
                if flags & 0x04 != 0 {
                    // tAttrChoose: a jump table of data+1 offsets.
                    rd.take((data as usize + 1) * 2)?;
                }
                if flags & 0x10 != 0 {
                    // tAttrSum: SUM of the one argument on the stack.
                    let a = st.pop()?;
                    st.push(format!("SUM({a})"));
                }
                // tAttrSemi, tAttrIf, tAttrGoto, tAttrSpace, tAttrBaxcel:
                // evaluation hints with no text.
            }
            0x1C => st.push(biff_error(rd.u8()?).to_string()),
            0x1D => st.push(if rd.u8()? != 0 { "TRUE" } else { "FALSE" }.to_string()),
            0x1E => st.push(rd.u16()?.to_string()),
            0x1F => st.push(num(rd.f64()?)),
            // Classed tokens: the base id in the low five bits, the class
            // (reference/value/array) in bits 5-6.
            0x20..=0x7F => {
                let id = ptg & 0x1F | 0x20;
                match id {
                    // ptgArray: the constant is in the extra data.
                    0x20 => {
                        rd.take(match v {
                            Biff::V8 => 7,
                            Biff::V12 => 14,
                        })?;
                        st.push(array_constant(v, &mut ex)?);
                    }
                    // ptgFunc: a fixed-arity built-in.
                    0x21 => {
                        let (name, argc) = ftab::lookup(rd.u16()?)?;
                        let args = pop_args(&mut st, argc? as usize)?;
                        st.push(format!("{name}({})", args.join(",")));
                    }
                    // ptgFuncVar: a built-in (or, with index 255, a named
                    // function) with its argument count.
                    0x22 => {
                        let argc = (rd.u8()? & 0x7F) as usize;
                        let tab = rd.u16()? & 0x7FFF;
                        let mut args = pop_args(&mut st, argc)?;
                        if tab == 255 {
                            if args.is_empty() {
                                return None;
                            }
                            let name = function_name(&args.remove(0));
                            st.push(format!("{name}({})", args.join(",")));
                        } else {
                            let (name, _) = ftab::lookup(tab)?;
                            st.push(format!("{name}({})", args.join(",")));
                        }
                    }
                    0x23 => st.push(names.name(rd.u32()?)?),
                    0x24 => {
                        let row = rd.row(v)?;
                        let col = rd.u16()?;
                        st.push(cell_ref(v, row, col, None));
                    }
                    0x25 => {
                        let r1 = rd.row(v)?;
                        let r2 = rd.row(v)?;
                        let c1 = rd.u16()?;
                        let c2 = rd.u16()?;
                        st.push(area_ref(v, (r1, r2, c1, c2), None));
                    }
                    // ptgMemArea / ptgMemErr / ptgMemNoMem / ptgMemFunc:
                    // wrappers around the subexpression that follows. Only
                    // the area list ptgMemArea leaves in the extra data
                    // matters, so a later array constant is found.
                    0x26..=0x29 => {
                        if id != 0x29 {
                            rd.take(4)?;
                        }
                        rd.u16()?;
                        if id == 0x26 {
                            match v {
                                Biff::V8 => {
                                    let n = ex.u16()? as usize;
                                    ex.take(n * 8)?;
                                }
                                Biff::V12 => {
                                    let n = ex.u32()? as usize;
                                    ex.take(n.checked_mul(16)?)?;
                                }
                            }
                        }
                    }
                    0x2A => {
                        rd.take(match v {
                            Biff::V8 => 4,
                            Biff::V12 => 6,
                        })?;
                        st.push("#REF!".to_string());
                    }
                    0x2B => {
                        rd.take(match v {
                            Biff::V8 => 8,
                            Biff::V12 => 12,
                        })?;
                        st.push("#REF!".to_string());
                    }
                    // ptgRefN / ptgAreaN: shared-formula references, relative
                    // parts as offsets from the cell.
                    0x2C => {
                        let row = rd.row(v)?;
                        let col = rd.u16()?;
                        st.push(cell_ref(v, row, col, Some(shared_at(base)?)));
                    }
                    0x2D => {
                        let r1 = rd.row(v)?;
                        let r2 = rd.row(v)?;
                        let c1 = rd.u16()?;
                        let c2 = rd.u16()?;
                        st.push(area_ref(v, (r1, r2, c1, c2), Some(shared_at(base)?)));
                    }
                    0x39 => {
                        let ixti = rd.u16()? as u32;
                        let index = rd.u32()?;
                        st.push(names.name_x(ixti, index)?);
                    }
                    0x3A => {
                        let prefix = names.xti(rd.u16()? as u32)?;
                        let row = rd.row(v)?;
                        let col = rd.u16()?;
                        let at = shared_or_none(base);
                        st.push(format!("{prefix}{}", cell_ref(v, row, col, at)));
                    }
                    0x3B => {
                        let prefix = names.xti(rd.u16()? as u32)?;
                        let r1 = rd.row(v)?;
                        let r2 = rd.row(v)?;
                        let c1 = rd.u16()?;
                        let c2 = rd.u16()?;
                        let at = shared_or_none(base);
                        st.push(format!("{prefix}{}", area_ref(v, (r1, r2, c1, c2), at)));
                    }
                    0x3C | 0x3D => {
                        let prefix = names.xti(rd.u16()? as u32).unwrap_or_default();
                        rd.take(match (id, v) {
                            (0x3C, Biff::V8) => 4,
                            (0x3C, Biff::V12) => 6,
                            (_, Biff::V8) => 8,
                            (_, Biff::V12) => 12,
                        })?;
                        st.push(format!("{prefix}#REF!"));
                    }
                    _ => return None,
                }
            }
            // ptgList (BIFF12): a structured reference, written as the range
            // it covers, the way Excel writes one to an `.xls`.
            0x18 if v == Biff::V12 && rgce.get(rd.at) == Some(&0x19) => {
                rd.u8()?;
                rd.u16()?;
                let flags = rd.u16()?;
                let table = names.table(rd.u32()?)?;
                let (c1, c2) = (rd.u16()? as u32, rd.u16()? as u32);
                st.push(list_ref(&table, flags, c1, c2, base)?);
            }
            // ptgExp/ptgTbl (resolved by the reader), ptgElf and the other
            // extended tokens, and anything unknown.
            _ => return None,
        }
    }
    match st.pop() {
        Some(f) if st.is_empty() && f.chars().count() <= MAX_TEXT => Some(f),
        _ => None,
    }
}

/// The last `n` stack entries, in argument order.
fn pop_args(st: &mut Vec<String>, n: usize) -> Option<Vec<String>> {
    if st.len() < n {
        return None;
    }
    Some(st.split_off(st.len() - n))
}

/// The name a `ptgFuncVar` 255 call is spelled with. Excel names a function
/// newer than the file format through a hidden defined name (`_xlfn.MAXIFS`,
/// in `.xls`) or an add-in external name (`EDATE`); the call is written as
/// the user sees it and [`crate::formula::file_formula`] puts back whatever
/// prefix the `.xlsx` needs. `_xlfn.SINGLE` and `_xlfn.ANCHORARRAY` keep
/// theirs: they are the stored spellings of `@x` and `A1#`.
fn function_name(raw: &str) -> String {
    let bare = raw
        .trim_start_matches("_xlfn.")
        .trim_start_matches("_xlws.");
    if bare.eq_ignore_ascii_case("SINGLE") || bare.eq_ignore_ascii_case("ANCHORARRAY") {
        format!("_xlfn.{}", bare.to_ascii_uppercase())
    } else {
        bare.to_string()
    }
}

fn shared_at(base: Base) -> Option<(u32, u32)> {
    match base {
        Base::Shared(r, c) => Some((r, c)),
        // An N token outside a shared formula: anchored at A1, as Excel
        // reads one in a defined name.
        Base::Cell(_) => Some((0, 0)),
    }
}

fn shared_or_none(base: Base) -> Option<(u32, u32)> {
    match base {
        Base::Shared(r, c) => Some((r, c)),
        Base::Cell(_) => None,
    }
}

/// One coordinate: absolute, or (for a relative one with an anchor) an
/// offset from it, wrapping round the grid as Excel's does.
fn coord(value: u32, rel: bool, anchor: Option<u32>, max: u32, bits: u32) -> u32 {
    match anchor {
        Some(a) if rel => {
            // Sign-extend the stored offset from its width.
            let shift = 32 - bits;
            let off = ((value << shift) as i32) >> shift;
            (a as i64 + off as i64).rem_euclid(max as i64 + 1) as u32
        }
        _ => value & max,
    }
}

fn col_bits(v: Biff) -> u32 {
    match v {
        Biff::V8 => 8,
        Biff::V12 => 14,
    }
}

fn row_bits(v: Biff) -> u32 {
    match v {
        Biff::V8 => 16,
        Biff::V12 => 20,
    }
}

/// `$A$1`-style text of one cell reference.
fn cell_ref(v: Biff, row: u32, colw: u16, at: Option<(u32, u32)>) -> String {
    let row_rel = colw & 0x8000 != 0;
    let col_rel = colw & 0x4000 != 0;
    let r = coord(row, row_rel, at.map(|a| a.0), v.max_row(), row_bits(v));
    let c = coord(
        (colw & 0x3FFF) as u32,
        col_rel,
        at.map(|a| a.1),
        v.max_col(),
        col_bits(v),
    );
    format!(
        "{}{}{}{}",
        if col_rel { "" } else { "$" },
        crate::sheet::col_name(c),
        if row_rel { "" } else { "$" },
        r + 1
    )
}

/// An area's text: `A1:B2`, or `A:A` / `1:1` when it spans the whole grid in
/// one direction.
fn area_ref(v: Biff, (r1, r2, c1w, c2w): (u32, u32, u16, u16), at: Option<(u32, u32)>) -> String {
    let rel = |w: u16| (w & 0x8000 != 0, w & 0x4000 != 0);
    let (r1rel, c1rel) = rel(c1w);
    let (r2rel, c2rel) = rel(c2w);
    let row = |r: u32, rr: bool| coord(r, rr, at.map(|a| a.0), v.max_row(), row_bits(v));
    let col = |w: u16, cr: bool| {
        coord(
            (w & 0x3FFF) as u32,
            cr,
            at.map(|a| a.1),
            v.max_col(),
            col_bits(v),
        )
    };
    let (ra, rb) = (row(r1, r1rel), row(r2, r2rel));
    let (ca, cb) = (col(c1w, c1rel), col(c2w, c2rel));
    let d = |rel: bool| if rel { "" } else { "$" };
    if ra == 0 && rb == v.max_row() {
        return format!(
            "{}{}:{}{}",
            d(c1rel),
            crate::sheet::col_name(ca),
            d(c2rel),
            crate::sheet::col_name(cb)
        );
    }
    if ca == 0 && cb == v.max_col() {
        return format!("{}{}:{}{}", d(r1rel), ra + 1, d(r2rel), rb + 1);
    }
    format!(
        "{}{}{}{}:{}{}{}{}",
        d(c1rel),
        crate::sheet::col_name(ca),
        d(r1rel),
        ra + 1,
        d(c2rel),
        crate::sheet::col_name(cb),
        d(r2rel),
        rb + 1
    )
}

/// A structured reference's range: `flags` holds the column mode (bits
/// 0-1: all columns, one, or `c1..=c2`) and the row type (bits 2-6: data,
/// all, headers, totals, this row, or the combinations Excel allows).
fn list_ref(t: &Table, flags: u16, c1: u32, c2: u32, base: Base) -> Option<String> {
    let (r1, r2, tc1, tc2) = t.range;
    // A table that is inverted or leaves the grid names no range.
    if r2 < r1 || tc2 < tc1 || !super::on_grid(r2, tc2) {
        return None;
    }
    let (cols_lo, cols_hi) = match flags & 0x3 {
        0 => (tc1, tc2),
        1 => (tc1.checked_add(c1)?, tc1.checked_add(c1)?),
        _ => (tc1.checked_add(c1)?, tc1.checked_add(c2)?),
    };
    if cols_hi > tc2 || cols_lo > cols_hi {
        return None;
    }
    let data = (r1.checked_add(t.header)?, r2.checked_sub(t.totals)?);
    let rows = match (flags >> 2) & 0x1F {
        0x00 | 0x04 => data,
        0x01 => (r1, r2),
        0x02 => (r1, r1.checked_add(t.header.checked_sub(1)?)?),
        0x06 => (r1, data.1),
        0x08 => (r2.checked_add(1)?.checked_sub(t.totals)?, r2),
        0x0C => (data.0, r2),
        0x10 => {
            // `[#This Row]`: the formula's own row, as `$B2`.
            let (Base::Cell(Some((row, _))) | Base::Shared(row, _)) = base else {
                return None;
            };
            let col = |c: u32| format!("${}{}", crate::sheet::col_name(c), row + 1);
            return Some(if cols_lo == cols_hi {
                format!("{}{}", t.prefix, col(cols_lo))
            } else {
                format!("{}{}:{}", t.prefix, col(cols_lo), col(cols_hi))
            });
        }
        _ => return None,
    };
    if rows.0 > rows.1 {
        return None;
    }
    let cell = |r: u32, c: u32| format!("${}${}", crate::sheet::col_name(c), r + 1);
    Some(format!(
        "{}{}:{}",
        t.prefix,
        cell(rows.0, cols_lo),
        cell(rows.1, cols_hi)
    ))
}

/// The next array constant in the extra data: `{1,2;"a",TRUE}`.
fn array_constant(v: Biff, ex: &mut Rd) -> Option<String> {
    let (cols, rows) = match v {
        Biff::V8 => {
            let c = ex.u8()? as usize + 1;
            let r = ex.u16()? as usize + 1;
            (c, r)
        }
        Biff::V12 => {
            let r = ex.u32()? as usize;
            let c = ex.u32()? as usize;
            (c, r)
        }
    };
    if cols == 0 || rows == 0 || cols.checked_mul(rows)? > 1 << 20 {
        return None;
    }
    let mut out = String::from("{");
    for r in 0..rows {
        if r > 0 {
            out.push(';');
        }
        for c in 0..cols {
            if c > 0 {
                out.push(',');
            }
            let ty = ex.u8()?;
            match (v, ty) {
                (Biff::V8, 0x00) => {
                    ex.take(8)?;
                }
                (Biff::V8, 0x01) | (Biff::V12, 0x00) => out.push_str(&num(ex.f64()?)),
                (Biff::V8, 0x02) => out.push_str(&quote(&ex.string(v, true)?)),
                (Biff::V12, 0x01) => out.push_str(&quote(&ex.string(v, false)?)),
                (Biff::V8, 0x04) => {
                    let b = ex.u8()?;
                    ex.take(7)?;
                    out.push_str(if b != 0 { "TRUE" } else { "FALSE" });
                }
                (Biff::V12, 0x02) => {
                    out.push_str(if ex.u8()? != 0 { "TRUE" } else { "FALSE" });
                }
                (Biff::V8, 0x10) => {
                    let e = ex.u8()?;
                    ex.take(7)?;
                    out.push_str(biff_error(e));
                }
                (Biff::V12, 0x04) => out.push_str(biff_error(ex.u8()?)),
                _ => return None,
            }
        }
    }
    out.push('}');
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Names for hand-built streams: XTI 0 is `Data`, 1 spans `Q1:Q3`, 2 is
    /// another workbook; name 1 is `TaxRate`, 2 the hidden `_xlfn.MAXIFS`,
    /// 3 `_xlfn.SINGLE`; external name (3, 1) is the add-in `EDATE`.
    pub(crate) struct TestNames;

    impl Names for TestNames {
        fn xti(&self, ixti: u32) -> Option<String> {
            match ixti {
                0 => Some("Data!".into()),
                1 => Some(super::super::sheet_prefix("Q1", "Q3")),
                _ => None,
            }
        }
        fn name(&self, index: u32) -> Option<String> {
            ["TaxRate", "_xlfn.MAXIFS", "_xlfn.SINGLE"]
                .get(index.checked_sub(1)? as usize)
                .map(|s| s.to_string())
        }
        fn name_x(&self, ixti: u32, index: u32) -> Option<String> {
            (ixti == 3 && index == 1).then(|| "EDATE".into())
        }
    }

    fn d8(rgce: &[u8]) -> Option<String> {
        decompile(Biff::V8, rgce, &[], Base::Cell(None), &TestNames)
    }

    fn r8(row: u16, col: u16) -> Vec<u8> {
        let mut v = row.to_le_bytes().to_vec();
        v.extend_from_slice(&col.to_le_bytes());
        v
    }

    #[test]
    fn operators_keep_excels_parentheses() {
        // (A1+2)*-B$3
        let mut f = vec![0x24];
        f.extend(r8(0, 0xC000));
        f.extend([0x1E, 2, 0, 0x03, 0x15, 0x24]);
        f.extend(r8(2, 0x4001));
        f.extend([0x13, 0x05]);
        assert_eq!(d8(&f).as_deref(), Some("(A1+2)*-B$3"));
    }

    #[test]
    fn functions_fixed_variable_and_attr_sum() {
        // ROUND(PI(),2): ptgFunc 27 with two args.
        let f = [0x41, 19, 0, 0x1E, 2, 0, 0x41, 27, 0];
        assert_eq!(d8(&f).as_deref(), Some("ROUND(PI(),2)"));
        // SUM via tAttrSum over $A$1:$B$2.
        let mut f = vec![0x25];
        f.extend([0, 0, 1, 0, 0, 0, 1, 0]);
        f.extend([0x19, 0x10, 0, 0]);
        assert_eq!(d8(&f).as_deref(), Some("SUM($A$1:$B$2)"));
        // IF(TRUE,"a""b",) through ptgFuncVar with a missing argument.
        let f = [0x1D, 1, 0x17, 3, 0, b'a', b'"', b'b', 0x16, 0x42, 3, 1, 0];
        assert_eq!(d8(&f).as_deref(), Some("IF(TRUE,\"a\"\"b\",)"));
    }

    #[test]
    fn three_d_and_names() {
        // SUM('Q1:Q3'!A1:A2) + Data!$B$1 + TaxRate
        let mut f = vec![0x3B, 1, 0];
        f.extend([0, 0, 1, 0, 0, 0xC0, 0, 0xC0]);
        f.extend([0x22, 1, 4, 0]);
        f.extend([0x3A, 0, 0]);
        f.extend(r8(0, 1));
        f.extend([0x03, 0x23, 1, 0, 0, 0, 0x03]);
        assert_eq!(
            d8(&f).as_deref(),
            Some("SUM('Q1:Q3'!A1:A2)+Data!$B$1+TaxRate")
        );
        // A reference into another workbook doesn't decompile.
        let mut f = vec![0x3A, 2, 0];
        f.extend(r8(0, 0));
        assert_eq!(d8(&f), None);
    }

    #[test]
    fn func_var_255_names_newer_and_add_in_functions() {
        // MAXIFS(A1:A2,B1:B2,"x"): name 2 first, then the arguments.
        let mut f = vec![0x23, 2, 0, 0, 0, 0x25, 0, 0, 1, 0, 0, 0xC0, 0, 0xC0];
        f.extend([0x25, 0, 0, 1, 0, 1, 0xC0, 1, 0xC0, 0x17, 1, 0, b'x']);
        f.extend([0x22, 4, 0xFF, 0]);
        assert_eq!(d8(&f).as_deref(), Some("MAXIFS(A1:A2,B1:B2,\"x\")"));
        // EDATE(A1,1) through ptgNameX.
        let mut f = vec![0x39, 3, 0, 1, 0, 0, 0, 0x24];
        f.extend(r8(0, 0xC000));
        f.extend([0x1E, 1, 0, 0x22, 3, 0xFF, 0]);
        assert_eq!(d8(&f).as_deref(), Some("EDATE(A1,1)"));
        // @A1 is stored as _xlfn.SINGLE(A1), which keeps its prefix.
        let mut f = vec![0x23, 3, 0, 0, 0, 0x24];
        f.extend(r8(0, 0xC000));
        f.extend([0x22, 2, 0xFF, 0]);
        assert_eq!(d8(&f).as_deref(), Some("_xlfn.SINGLE(A1)"));
    }

    #[test]
    fn shared_relative_refs_resolve_against_the_cell() {
        // ptgRefN row -1, col +1 (relative both), and ptgAreaN.
        let mut f = vec![0x2C];
        f.extend(r8(0xFFFF, 0xC001));
        let at = |r, c| decompile(Biff::V8, &f, &[], Base::Shared(r, c), &TestNames);
        assert_eq!(at(4, 2).as_deref(), Some("D4"));
        assert_eq!(at(9, 0).as_deref(), Some("B9"));
        // Absolute parts of an N token stay put.
        let mut f = vec![0x2D];
        f.extend([0, 0, 0, 0, 0, 0x80, 2, 0]);
        let g = decompile(Biff::V8, &f, &[], Base::Shared(5, 0), &TestNames);
        assert_eq!(g.as_deref(), Some("$A6:$C$1"));
    }

    #[test]
    fn whole_columns_and_rows() {
        let f = [0x25, 0, 0, 0xFF, 0xFF, 0, 0, 0, 0];
        assert_eq!(d8(&f).as_deref(), Some("$A:$A"));
        let f = [0x25, 1, 0, 1, 0, 0, 0, 0xFF, 0];
        assert_eq!(d8(&f).as_deref(), Some("$2:$2"));
        // BIFF12: rows are 32-bit, the grid is 1,048,576 x 16,384.
        let mut f = vec![0x25];
        f.extend(0u32.to_le_bytes());
        f.extend(0xF_FFFFu32.to_le_bytes());
        f.extend([1, 0xC0, 1, 0xC0]);
        let g = decompile(Biff::V12, &f, &[], Base::Cell(None), &TestNames);
        assert_eq!(g.as_deref(), Some("B:B"));
    }

    #[test]
    fn array_constants_from_the_extra_data() {
        // SUM({1,"a";TRUE,#N/A})
        let f = [0x60, 0, 0, 0, 0, 0, 0, 0, 0x42, 1, 4, 0];
        let mut ex = vec![1, 1, 0];
        ex.push(0x01);
        ex.extend(1.0f64.to_le_bytes());
        ex.extend([0x02, 1, 0, 0, b'a']);
        ex.extend([0x04, 1, 0, 0, 0, 0, 0, 0, 0]);
        ex.extend([0x10, 0x2A, 0, 0, 0, 0, 0, 0, 0]);
        let g = decompile(Biff::V8, &f, &ex, Base::Cell(None), &TestNames);
        assert_eq!(g.as_deref(), Some("SUM({1,\"a\";TRUE,#N/A})"));
    }

    #[test]
    fn unknown_or_truncated_tokens_fail_the_formula_only() {
        // ptgTbl, ptgElf (0x18), a truncated ptgNum and an unbalanced stack.
        assert_eq!(d8(&[0x02, 0, 0, 0, 0]), None);
        assert_eq!(d8(&[0x18, 0, 0]), None);
        assert_eq!(d8(&[0x1F, 0, 0]), None);
        assert_eq!(d8(&[0x1E, 1, 0, 0x1E, 2, 0]), None);
        assert_eq!(d8(&[0x03]), None);
        assert_eq!(d8(&[0xFF]), None);
    }

    /// Names with one table: id 1, `Orders!A1:D5`, header row, no totals.
    struct TableNames;

    impl Names for TableNames {
        fn xti(&self, _: u32) -> Option<String> {
            None
        }
        fn name(&self, _: u32) -> Option<String> {
            None
        }
        fn name_x(&self, _: u32, _: u32) -> Option<String> {
            None
        }
        fn table(&self, id: u32) -> Option<Table> {
            (id == 1).then(|| Table {
                prefix: "Orders!".into(),
                range: (0, 4, 0, 3),
                header: 1,
                totals: 0,
            })
        }
    }

    #[test]
    fn structured_references_become_the_ranges_they_cover() {
        let list = |flags: u16, c1: u16, c2: u16, base: Base| {
            let mut f = vec![0x18, 0x19, 0, 0];
            f.extend(flags.to_le_bytes());
            f.extend(1u32.to_le_bytes());
            f.extend(c1.to_le_bytes());
            f.extend(c2.to_le_bytes());
            decompile(Biff::V12, &f, &[], base, &TableNames)
        };
        let cell = Base::Cell(Some((2, 4)));
        // Sales[Amount], Sales[[Qty]:[Price]], Sales[#All], Sales[#Headers].
        assert_eq!(list(0x01, 3, 3, cell).as_deref(), Some("Orders!$D$2:$D$5"));
        assert_eq!(list(0x02, 1, 2, cell).as_deref(), Some("Orders!$B$2:$C$5"));
        assert_eq!(list(0x04, 0, 0, cell).as_deref(), Some("Orders!$A$1:$D$5"));
        assert_eq!(list(0x08, 0, 0, cell).as_deref(), Some("Orders!$A$1:$D$1"));
        // Sales[[#This Row],[Qty]] in row 3.
        assert_eq!(list(0x0641, 1, 1, cell).as_deref(), Some("Orders!$B3"));
        // Column offsets past the table (or u32) give nothing.
        assert_eq!(list(0x01, 9, 9, cell), None);
        assert_eq!(list(0x02, 2, 1, cell), None);
        // No totals row to name, an unknown table, and BIFF8 has no ptgList.
        assert_eq!(list(0x21, 0, 0, cell), None);
        let mut f = vec![0x18, 0x19, 0, 0, 1, 0];
        f.extend(9u32.to_le_bytes());
        f.extend([0, 0, 0, 0]);
        assert_eq!(decompile(Biff::V12, &f, &[], cell, &TableNames), None);
        assert_eq!(decompile(Biff::V8, &f, &[], cell, &TableNames), None);
    }

    #[test]
    fn long_or_runaway_streams_give_up_quickly() {
        // A number wrapped in 16,000 parentheses: text grows past the limit.
        let mut f = vec![0x1E, 1, 0];
        f.extend(std::iter::repeat_n(0x15, 16_000));
        let started = std::time::Instant::now();
        assert_eq!(d8(&f), None);
        // Past the stream bound: refused before decompiling.
        let mut g = vec![0x1E, 1, 0];
        g.extend(std::iter::repeat_n(0x15, 20_000));
        assert_eq!(d8(&g), None);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // 5,000 Cyrillic characters (10,000 bytes) are within the limit:
        // twenty 250-character strings joined with &.
        let word: Vec<u8> = "ж"
            .repeat(250)
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut long = Vec::new();
        for i in 0..20 {
            long.extend([0x17, 250, 1]);
            long.extend(&word);
            if i > 0 {
                long.push(0x08);
            }
        }
        let text = d8(&long).unwrap();
        assert_eq!(text.chars().filter(|&c| c == 'ж').count(), 5_000);
        // A modest nesting still decompiles.
        let mut h = vec![0x1E, 1, 0];
        h.extend(std::iter::repeat_n(0x15, 10));
        assert_eq!(d8(&h).as_deref(), Some("((((((((((1))))))))))"));
    }

    #[test]
    fn biff12_strings_are_utf16() {
        let f = [0x17, 2, 0, b'h', 0, 0xE9, 0];
        let g = decompile(Biff::V12, &f, &[], Base::Cell(None), &TestNames);
        assert_eq!(g.as_deref(), Some("\"hé\""));
    }
}
