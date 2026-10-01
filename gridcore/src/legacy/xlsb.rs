//! Excel Binary Workbook `.xlsb`: BIFF12 records in an OPC package
//! ([MS-XLSB]). The parts are the `.xlsx` ones with `.bin` bodies:
//! `xl/workbook.bin` (sheets, names, externals, the date system),
//! `xl/sharedStrings.bin`, `xl/styles.bin` and one `.bin` per worksheet.
//!
//! A record is a varint type (1-2 bytes) and a varint size (1-4 bytes) and
//! its body. Records not listed here are skipped.

use std::collections::{BTreeMap, HashMap, HashSet};

use opccore::zip::ZipArchive;

use super::ptg::{self, Base, Biff, Names, Table, utf16};
use super::{
    BookIn, ExternalLink, Le, Limits, OpenError, SheetIn, XfFormats, biff_error, on_grid, rk,
    set_array, sheet_prefix,
};
use crate::sheet::{Cell, CellValue, DefinedName};
use crate::xlsx::{parse_rels, parse_rels_mode, rels_part_name, resolve_relative};

/// The records of a part: (type, body). A record cut off by the end of the
/// part ends the list.
fn records(b: &[u8]) -> Vec<(u32, &[u8])> {
    fn varint(b: &[u8], at: &mut usize, max: usize) -> Option<u32> {
        let mut v = 0u32;
        for k in 0..max {
            let c = *b.get(*at)?;
            *at += 1;
            v |= ((c & 0x7F) as u32) << (7 * k);
            if c & 0x80 == 0 {
                return Some(v);
            }
        }
        Some(v)
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let (Some(ty), Some(len)) = (varint(b, &mut at, 2), varint(b, &mut at, 4)) else {
            break;
        };
        let Some(body) = b.get(at..at + len as usize) else {
            break;
        };
        out.push((ty, body));
        at += len as usize;
    }
    out
}

/// A little-endian cursor over one record body.
type Cur<'a> = Le<'a>;

/// BIFF12's own fields.
impl<'a> Le<'a> {
    /// `XLWideString`: a 32-bit count of UTF-16 units. `0xFFFFFFFF` (a null
    /// `XLNullableWideString`) reads as empty.
    fn wide(&mut self) -> Option<String> {
        let n = self.u32()?;
        if n == 0xFFFF_FFFF {
            return Some(String::new());
        }
        Some(utf16(self.take((n as usize).checked_mul(2)?)?))
    }
    /// A parsed formula: `cce`, the tokens, `cb`, the extra data.
    fn formula(&mut self) -> Option<(&'a [u8], &'a [u8])> {
        let cce = self.u32()? as usize;
        let rgce = self.take(cce)?;
        let cb = self.u32()? as usize;
        let extra = self.take(cb)?;
        Some((rgce, extra))
    }
}

const BRT_ROW_HDR: u32 = 0;
const BRT_NAME: u32 = 39;
const BRT_FMT: u32 = 44;
const BRT_XF: u32 = 47;
const BRT_WB_PROP: u32 = 153;
const BRT_BUNDLE_SH: u32 = 156;
const BRT_SST_ITEM: u32 = 19;
const BRT_SUP_BOOK_SRC: u32 = 355;
const BRT_SUP_SELF: u32 = 357;
const BRT_SUP_SAME: u32 = 358;
const BRT_SUP_TABS: u32 = 359;
const BRT_BEGIN_SUP_BOOK: u32 = 360;
const BRT_EXTERN_SHEET: u32 = 362;
const BRT_ARR_FMLA: u32 = 426;
const BRT_SHR_FMLA: u32 = 427;
const BRT_SUP_NAME_START: u32 = 577;
/// A name's definition in an external link part: `cce` and tokens (see
/// [`read_link`]; corpus/legacy/addin has both kinds).
const BRT_SUP_NAME_FMLA: u32 = 585;
const BRT_BEGIN_CELL_XFS: u32 = 617;
const BRT_END_CELL_XFS: u32 = 618;
const BRT_SUP_ADDIN: u32 = 667;

/// A BrtName as read, decompiled once every name is known.
struct RawName {
    name: String,
    /// Hidden function names (`_xlfn.MAXIFS`) and macro names: how a
    /// formula calls a function, not defined names.
    function: bool,
    /// 0-based sheet of a sheet-scoped name; `0xFFFFFFFF` for the workbook.
    itab: u32,
    rgce: Vec<u8>,
    extra: Vec<u8>,
}

/// What a SUPBOOK is.
#[derive(Debug, PartialEq)]
enum Book {
    /// This workbook (BrtSupSelf, BrtSupSame).
    Own,
    /// Another workbook or an add-in workbook (BrtSupBookSrc): its place in
    /// [`Globals::links`], or `None` when its link part isn't read.
    External(Option<usize>),
    /// An XLL add-in (BrtSupAddin): its names aren't read.
    Other,
}

/// The globals a token stream refers to.
struct Globals {
    sheets: Vec<String>,
    /// Per SUPBOOK, what it is.
    books: Vec<Book>,
    /// The external books with a function name formulas can call
    /// ([`read_link`]), `[1]` first.
    links: Vec<ExternalLink>,
    xti: Vec<(u32, i32, i32)>,
    /// Each BrtName's name and sheet (`0xFFFFFFFF` for the workbook).
    names: Vec<(String, u32)>,
    /// Tables by id, for structured references.
    tables: HashMap<u32, Table>,
}

impl Names for Globals {
    fn xti(&self, ixti: u32) -> Option<String> {
        let &(book, first, last) = self.xti.get(ixti as usize)?;
        if *self.books.get(book as usize)? != Book::Own {
            return None;
        }
        let name = |i: i32| self.sheets.get(usize::try_from(i).ok()?).cloned();
        Some(sheet_prefix(&name(first)?, &name(last)?))
    }
    fn name(&self, index: u32) -> Option<String> {
        Some(self.names.get(index.checked_sub(1)? as usize)?.0.clone())
    }
    fn name_x(&self, ixti: u32, index: u32) -> Option<String> {
        let &(book, first, _) = self.xti.get(ixti as usize)?;
        match *self.books.get(book as usize)? {
            Book::Own => {
                // A sheet-scoped name is qualified with the XTI's sheet.
                let (name, itab) = self.names.get(index.checked_sub(1)? as usize)?;
                if *itab == 0xFFFF_FFFF {
                    Some(name.clone())
                } else {
                    Some(format!("{}{name}", self.xti(ixti)?))
                }
            }
            // A function name of the whole book (first sheet -2), as the
            // `.xlsx` spells it: `[1]!EUROCONVERT`. One scoped to a sheet of
            // the book, or a range or constant name, isn't read.
            Book::External(Some(k)) if first == -2 => {
                let name = self.links[k].names.get(index.checked_sub(1)? as usize)?;
                Some(format!("[{}]!{}", k + 1, name.as_ref()?))
            }
            Book::External(_) | Book::Other => None,
        }
    }
    fn table(&self, id: u32) -> Option<Table> {
        self.tables.get(&id).cloned()
    }
}

const BRT_BEGIN_LIST: u32 = 343;

/// The tables of sheet `sheet` (its part's table rels), by id.
fn read_tables(zip: &ZipArchive, part: &str, sheet: &str, out: &mut HashMap<u32, Table>) {
    for (ty, target) in rels(zip, part).into_values() {
        if !ty.ends_with("/table") {
            continue;
        }
        let Some(bytes) = zip.read(&target) else {
            continue;
        };
        for (ty, body) in records(&bytes) {
            if ty != BRT_BEGIN_LIST {
                continue;
            }
            let mut c = Cur::new(body);
            let _ = (|| -> Option<()> {
                let range = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
                c.u32()?;
                let id = c.u32()?;
                let header = c.u32()?.min(1);
                let totals = c.u32()?.min(1);
                out.insert(
                    id,
                    Table {
                        prefix: sheet_prefix(sheet, sheet),
                        range,
                        header,
                        totals,
                    },
                );
                Some(())
            })();
        }
    }
}

/// The relationships of `part`, by `Id`: (type, the target part's name).
fn rels(zip: &ZipArchive, part: &str) -> HashMap<String, (String, String)> {
    let Some(bytes) = zip.read(&rels_part_name(part)) else {
        return HashMap::new();
    };
    let dir = part.rsplit_once('/').map_or("", |(d, _)| d);
    parse_rels(&String::from_utf8_lossy(&bytes))
        .into_iter()
        .map(|(id, ty, target)| (id, (ty, resolve_relative(dir, &target))))
        .collect()
}

/// The external link part `part` of a BrtSupBookSrc: the book's rel, sheet
/// names and names (BrtBeginSupBook, BrtSupTabs, BrtSupNameStart) and the
/// part's relationships. `None` unless it is a workbook's (`sbt` 0, not DDE
/// or OLE) whose rel is there and which has a function name: only a
/// function name makes a formula refer to the book in a way the import
/// keeps, and it keeps nothing else of it.
///
/// A name is a function (an add-in's, such as EUROCONVERT) when its
/// BrtSupNameFmla holds no tokens (`cce` 0, as Excel writes EUROTOOL.XLAM's
/// names). A workbook's range or constant name has its definition there
/// (`Prices`: ptgArea3d, `Half`: `#REF!`) and the book's cells cached
/// beside it, neither of which the import reads, so such a name is `None`;
/// so is one with the record missing or cut short.
fn read_link(zip: &ZipArchive, part: &str) -> Option<ExternalLink> {
    let bytes = zip.read(part)?;
    let mut link = ExternalLink::default();
    let mut workbook = false;
    // The latest BrtSupNameStart's name, until its BrtSupNameFmla.
    let mut pending: Option<String> = None;
    for (ty, body) in records(&bytes) {
        let mut c = Cur::new(body);
        match ty {
            BRT_BEGIN_SUP_BOOK => {
                workbook = c.u16()? == 0;
                link.book = c.wide()?;
            }
            BRT_SUP_TABS => {
                for _ in 0..c.u32()? {
                    link.sheets.push(c.wide()?);
                }
            }
            // Every name takes its place: ptgNameX counts them all.
            BRT_SUP_NAME_START => {
                pending = c.wide();
                link.names.push(None);
            }
            BRT_SUP_NAME_FMLA => {
                if let (Some(name), Some(0)) = (pending.take(), c.u32()) {
                    if let Some(slot) = link.names.last_mut() {
                        *slot = Some(name);
                    }
                }
            }
            _ => {}
        }
    }
    let bytes = zip.read(&rels_part_name(part))?;
    link.rels = parse_rels_mode(&String::from_utf8_lossy(&bytes));
    let named = link.rels.iter().any(|(id, ..)| *id == link.book);
    let functions = link.names.iter().any(Option::is_some);
    (workbook && named && functions).then_some(link)
}

/// Read an `.xlsb` package.
pub(crate) fn read(zip: &ZipArchive) -> Result<BookIn, OpenError> {
    read_with(zip, Limits::default())
}

/// [`read`] under `limits`.
pub(crate) fn read_with(zip: &ZipArchive, limits: Limits) -> Result<BookIn, OpenError> {
    let wb = zip
        .read("xl/workbook.bin")
        .ok_or_else(|| OpenError::Corrupt("unreadable xl/workbook.bin".into()))?;
    let rel = rels(zip, "xl/workbook.bin");
    let mut book = BookIn::with_limits(limits);
    let mut g = Globals {
        sheets: Vec::new(),
        books: Vec::new(),
        links: Vec::new(),
        xti: Vec::new(),
        names: Vec::new(),
        tables: HashMap::new(),
    };
    // Each BrtBundleSh's part, in sheet order; None for one without a rel.
    let mut parts: Vec<Option<String>> = Vec::new();
    let mut raw_names: Vec<RawName> = Vec::new();
    // Each external link part read so far: its place in `g.links`.
    let mut link_of: HashMap<String, Option<usize>> = HashMap::new();
    let mut too_many_links = false;
    for (ty, body) in records(&wb) {
        let mut c = Cur::new(body);
        let _ = (|| -> Option<()> {
            match ty {
                BRT_WB_PROP => book.date1904 = c.u32()? & 1 == 1,
                BRT_BUNDLE_SH => {
                    c.u32()?;
                    c.u32()?;
                    let rid = c.wide()?;
                    g.sheets.push(c.wide()?);
                    parts.push(rel.get(&rid).map(|(_, target)| target.clone()));
                }
                BRT_SUP_SELF | BRT_SUP_SAME => g.books.push(Book::Own),
                BRT_SUP_BOOK_SRC => {
                    // Links are numbered in the order of the books that
                    // have one: those with a function name. That is
                    // Excel's `[k]` unless a book without one comes first
                    // (one only cells refer to, or whose range or constant
                    // names formulas call, neither imported): then the
                    // numbers differ from Excel's but still name the links
                    // this import writes. A part several books name is
                    // read once and is one link.
                    let part = c.wide().and_then(|rid| rel.get(&rid));
                    let k = match part {
                        None => None,
                        Some((_, part)) => match link_of.get(part) {
                            Some(&k) => k,
                            None => {
                                let k = match read_link(zip, part) {
                                    Some(_) if g.links.len() >= book.limits.links => {
                                        too_many_links = true;
                                        None
                                    }
                                    Some(l) => {
                                        g.links.push(l);
                                        Some(g.links.len() - 1)
                                    }
                                    None => None,
                                };
                                link_of.insert(part.clone(), k);
                                k
                            }
                        },
                    };
                    g.books.push(Book::External(k));
                }
                BRT_SUP_ADDIN => g.books.push(Book::Other),
                BRT_EXTERN_SHEET => {
                    let n = c.u32()?;
                    for _ in 0..n {
                        g.xti.push((c.u32()?, c.u32()? as i32, c.u32()? as i32));
                    }
                }
                BRT_NAME => {
                    let flags = c.u32()?;
                    c.u8()?;
                    let itab = c.u32()?;
                    let name = c.wide()?;
                    let (rgce, extra) = c.formula()?;
                    // fFunc / fOB / fProc, and the _xlfn. future functions.
                    let function = flags & 0x0E != 0 || name.starts_with("_xlfn.");
                    g.names.push((name.clone(), itab));
                    raw_names.push(RawName {
                        name,
                        function,
                        itab,
                        rgce: rgce.to_vec(),
                        extra: extra.to_vec(),
                    });
                }
                _ => {}
            }
            Some(())
        })();
    }
    if too_many_links {
        return Err(OpenError::Corrupt(format!(
            "too many external links (more than {})",
            book.limits.links
        )));
    }

    let sst: Vec<String> = zip
        .read("xl/sharedStrings.bin")
        .map(|b| {
            records(&b)
                .into_iter()
                .filter(|(ty, _)| *ty == BRT_SST_ITEM)
                .map(|(_, body)| {
                    let mut c = Cur::new(body);
                    c.u8().and_then(|_| c.wide()).unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();

    // styles.bin: format codes, and the cell XFs' format ids.
    let mut fmt_codes: HashMap<u16, String> = HashMap::new();
    let mut xf_fmt: Vec<u16> = Vec::new();
    if let Some(b) = zip.read("xl/styles.bin") {
        let mut in_cell_xfs = false;
        for (ty, body) in records(&b) {
            let mut c = Cur::new(body);
            match ty {
                BRT_FMT => {
                    if let (Some(id), Some(code)) = (c.u16(), c.wide()) {
                        fmt_codes.insert(id, code);
                    }
                }
                BRT_BEGIN_CELL_XFS => in_cell_xfs = true,
                BRT_END_CELL_XFS => in_cell_xfs = false,
                BRT_XF if in_cell_xfs => {
                    let _ = c.u16();
                    xf_fmt.push(c.u16().unwrap_or(0));
                }
                _ => {}
            }
        }
    }
    let formats = XfFormats {
        xf_fmt,
        codes: fmt_codes,
    };

    // Every table first: a formula may name one on another sheet.
    for (i, part) in parts.iter().enumerate() {
        if let Some(part) = part {
            let mut tables = std::mem::take(&mut g.tables);
            read_tables(zip, part, &g.sheets[i], &mut tables);
            g.tables = tables;
        }
    }
    let mut imported: Vec<Option<usize>> = Vec::new();
    // A part two sheets name is read once.
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, part) in parts.iter().enumerate() {
        // Only worksheets: a chartsheet's or macro sheet's part is no grid.
        let Some(bytes) = part
            .as_ref()
            .filter(|p| p.contains("worksheets/") && seen.insert(p.as_str()))
            .and_then(|p| zip.read(p))
        else {
            imported.push(None);
            continue;
        };
        let cells = read_sheet(&bytes, &g, &sst, &mut book, &formats)?;
        imported.push(Some(book.sheets.len()));
        book.push_sheet(SheetIn {
            name: g.sheets[i].clone(),
            cells,
        })?;
    }

    for RawName {
        name,
        function,
        itab,
        rgce,
        extra,
    } in &raw_names
    {
        if *function {
            continue;
        }
        let scope = match *itab {
            0xFFFF_FFFF => None,
            t => match imported.get(t as usize).copied().flatten() {
                Some(s) => Some(s),
                None => continue,
            },
        };
        let Some(formula) = ptg::decompile(Biff::V12, rgce, extra, Base::Cell(None), &g) else {
            continue;
        };
        book.names.push(DefinedName {
            name: name.clone(),
            scope,
            formula,
        });
    }
    book.external_links = g.links;
    Ok(book)
}

/// A shared or array formula: its range (r1, r2, c1, c2), tokens and extra
/// data.
type Group = ((u32, u32, u32, u32), Vec<u8>, Vec<u8>);

/// A sheet part's cells.
fn read_sheet(
    bytes: &[u8],
    g: &Globals,
    sst: &[String],
    book: &mut BookIn,
    formats: &XfFormats,
) -> Result<BTreeMap<(u32, u32), Cell>, OpenError> {
    let mut cells = BTreeMap::new();
    let mut charged = 0usize;
    let mut row = 0u32;
    // Cells whose formula is a ptgExp, resolved once every shared and array
    // formula of the sheet is known.
    // (cell, master): the master's row is in ptgExp, its column in the
    // formula's extra data.
    let mut pending: Vec<((u32, u32), (u32, u32))> = Vec::new();
    // Shared formulas by their master (the formula cell before
    // BrtShrFmla) and, as an alias that never displaces a master, their
    // range's top-left; array formulas by their anchor.
    let mut shared: HashMap<(u32, u32), Group> = HashMap::new();
    let mut arrays: HashMap<(u32, u32), Group> = HashMap::new();
    // The last formula cell read: a BrtShrFmla's master, the cell its
    // members' ptgExp names (not necessarily the range's top-left).
    let mut last_formula: Option<(u32, u32)> = None;
    for (ty, body) in records(bytes) {
        let mut c = Cur::new(body);
        let _ = (|| -> Option<()> {
            match ty {
                BRT_ROW_HDR => row = c.u32()?,
                // BrtCellBlank .. BrtFmlaError: column, style, then the value.
                1..=11 => {
                    let col = c.u32()?;
                    if ty >= 8 {
                        // A BrtShrFmla after this record belongs to this
                        // cell, even when the rest doesn't read.
                        last_formula = Some((row, col));
                    }
                    if !on_grid(row, col) {
                        return None;
                    }
                    let style = c.u32()? & 0x00FF_FFFF;
                    let value = match ty {
                        1 => CellValue::Empty,
                        2 => CellValue::Number(rk(c.u32()?)),
                        3 | 11 => CellValue::Error(biff_error(c.u8()?).to_string()),
                        4 | 10 => CellValue::Bool(c.u8()? != 0),
                        5 | 9 => CellValue::Number(c.f64()?),
                        6 | 8 => CellValue::Text(c.wide()?),
                        7 => CellValue::Text(sst.get(c.u32()? as usize)?.clone()),
                        _ => return None,
                    };
                    let mut cell = Cell {
                        value,
                        style: formats.format_of(style, book),
                        ..Cell::default()
                    };
                    if ty >= 8 {
                        c.u16()?;
                        let (rgce, extra) = c.formula()?;
                        if rgce.first() == Some(&0x01) {
                            let mr = rgce
                                .get(1..5)
                                .map_or(row, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                            let mc = extra
                                .get(..4)
                                .map_or(col, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                            pending.push(((row, col), (mr, mc)));
                        } else {
                            let base = Base::Cell(Some((row, col)));
                            cell.formula = ptg::decompile(Biff::V12, rgce, extra, base, g);
                        }
                    }
                    cells.insert((row, col), cell);
                }
                BRT_SHR_FMLA | BRT_ARR_FMLA => {
                    let range = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
                    if ty == BRT_ARR_FMLA {
                        c.u8()?;
                    }
                    let (rgce, extra) = c.formula()?;
                    let group = (range, rgce.to_vec(), extra.to_vec());
                    let corner = (range.0, range.2);
                    if ty == BRT_ARR_FMLA {
                        arrays.insert(corner, group);
                    } else {
                        let master = last_formula.unwrap_or(corner);
                        // The corner is only an alias: it never replaces
                        // a group whose master it is, in either order.
                        if master != corner {
                            shared.entry(corner).or_insert_with(|| group.clone());
                        }
                        shared.insert(master, group);
                    }
                }
                _ => {}
            }
            Some(())
        })();
        if cells.len() > charged {
            book.charge_cells(cells.len() - charged)?;
            charged = cells.len();
        }
    }
    for (at, master) in pending {
        if let Some((range, rgce, extra)) = arrays.get(&master) {
            // The array formula lives on its anchor; the rest are values.
            if at != master {
                continue;
            }
            let Some(f) = ptg::decompile(Biff::V12, rgce, extra, Base::Cell(Some(at)), g) else {
                continue;
            };
            if let Some(cell) = cells.get_mut(&at) {
                set_array(cell, *range, f);
            }
            continue;
        }
        // A cell outside the group's stated range still takes it: writers
        // get the range wrong, and the tokens are relative to the cell.
        let Some((_, rgce, extra)) = shared.get(&master) else {
            continue;
        };
        let f = ptg::decompile(Biff::V12, rgce, extra, Base::Shared(at.0, at.1), g);
        if let (Some(f), Some(cell)) = (f, cells.get_mut(&at)) {
            cell.formula = Some(f);
        }
    }
    Ok(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record: varint type and size, then the body.
    fn rec(ty: u32, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let mut put = |mut n: u32, max: usize| {
            for _ in 0..max {
                let b = (n & 0x7F) as u8;
                n >>= 7;
                if n == 0 {
                    v.push(b);
                    return;
                }
                v.push(b | 0x80);
            }
        };
        put(ty, 2);
        put(body.len() as u32, 4);
        v.extend_from_slice(body);
        v
    }

    fn wide(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut v = (units.len() as u32).to_le_bytes().to_vec();
        for u in units {
            v.extend(u.to_le_bytes());
        }
        v
    }

    fn cell(col: u32, style: u32) -> Vec<u8> {
        let mut v = col.to_le_bytes().to_vec();
        v.extend(style.to_le_bytes());
        v
    }

    fn fmla(rgce: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut v = 0u16.to_le_bytes().to_vec();
        v.extend((rgce.len() as u32).to_le_bytes());
        v.extend_from_slice(rgce);
        v.extend((extra.len() as u32).to_le_bytes());
        v.extend_from_slice(extra);
        v
    }

    /// An `.xlsb` with one sheet "Data" holding `sheet` records, plus
    /// `workbook` records after the sheet list.
    fn xlsb(workbook: &[Vec<u8>], sheet: &[Vec<u8>]) -> Vec<u8> {
        xlsb_links(workbook, sheet, &[])
    }

    /// [`xlsb`] with external link parts: each (rId, its part's records,
    /// its rels part), as `externalLinks/externalLink{k}.bin`, k from 1.
    fn xlsb_links(
        workbook: &[Vec<u8>],
        sheet: &[Vec<u8>],
        links: &[(&str, Vec<u8>, &str)],
    ) -> Vec<u8> {
        let mut bundle = vec![0u8; 8];
        bundle.extend(wide("rId1"));
        bundle.extend(wide("Data"));
        let mut wb = rec(BRT_BUNDLE_SH, &bundle);
        for r in workbook {
            wb.extend_from_slice(r);
        }
        let mut rels = String::from(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x" Target="worksheets/sheet1.bin"/>"#,
        );
        let mut parts = Vec::new();
        for (k, (rid, part, part_rels)) in links.iter().enumerate() {
            let k = k + 1;
            rels.push_str(&format!(
                r#"<Relationship Id="{rid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink" Target="externalLinks/externalLink{k}.bin"/>"#
            ));
            parts.push((
                format!("xl/externalLinks/externalLink{k}.bin"),
                part.clone(),
            ));
            parts.push((
                format!("xl/externalLinks/_rels/externalLink{k}.bin.rels"),
                part_rels.as_bytes().to_vec(),
            ));
        }
        rels.push_str("</Relationships>");
        parts.push(("xl/workbook.bin".to_string(), wb));
        parts.push(("xl/_rels/workbook.bin.rels".to_string(), rels.into_bytes()));
        parts.push(("xl/worksheets/sheet1.bin".to_string(), sheet.concat()));
        opccore::zipwrite::write_zip(&parts)
    }

    /// An external link part: BrtBeginSupBook (`sbt`, the book's rel
    /// `rid`), BrtSupTabs and a BrtSupNameStart per name.
    fn link_part(sbt: u16, rid: &str, sheets: &[&str], names: &[&str]) -> Vec<u8> {
        let mut begin = sbt.to_le_bytes().to_vec();
        begin.extend(wide(rid));
        begin.extend(0xFFFF_FFFFu32.to_le_bytes());
        let mut v = rec(BRT_BEGIN_SUP_BOOK, &begin);
        let mut tabs = (sheets.len() as u32).to_le_bytes().to_vec();
        for s in sheets {
            tabs.extend(wide(s));
        }
        v.extend(rec(BRT_SUP_TABS, &tabs));
        for n in names {
            // A function name: no tokens.
            v.extend(rec(BRT_SUP_NAME_START, &wide(n)));
            v.extend(rec(BRT_SUP_NAME_FMLA, &[0; 4]));
        }
        v
    }

    /// A link part's rels: `rId1`, the library add-in it names.
    const LINK_RELS: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/xlExternalLinkPath/xlLibrary" Target="TOOLS.XLAM" TargetMode="External"/></Relationships>"#;

    /// BrtExternSheet with these XTIs (book, first sheet, last sheet).
    fn extern_sheet(xti: &[(u32, i32, i32)]) -> Vec<u8> {
        let mut v = (xti.len() as u32).to_le_bytes().to_vec();
        for &(book, first, last) in xti {
            v.extend(book.to_le_bytes());
            v.extend(first.to_le_bytes());
            v.extend(last.to_le_bytes());
        }
        rec(BRT_EXTERN_SHEET, &v)
    }

    /// A BrtFmlaNum in column `col` caching `value`, with tokens `rgce`.
    fn fmla_num(col: u32, value: f64, rgce: &[u8]) -> Vec<u8> {
        let mut v = cell(col, 0);
        v.extend(value.to_le_bytes());
        v.extend(fmla(rgce, &[]));
        rec(9, &v)
    }

    /// ptgNameX: XTI `ixti`, 1-based name `index`.
    fn name_x(ixti: u16, index: u32) -> Vec<u8> {
        let mut v = vec![0x39];
        v.extend(ixti.to_le_bytes());
        v.extend(index.to_le_bytes());
        v
    }

    fn open(bytes: &[u8]) -> BookIn {
        read(&ZipArchive::open(bytes).unwrap()).unwrap()
    }

    #[test]
    fn cells_rows_and_formulas() {
        let mut real = cell(0, 0);
        real.extend(2.5f64.to_le_bytes());
        let mut st = cell(1, 0);
        st.extend(wide("hé"));
        let mut num = cell(2, 0);
        num.extend(5.0f64.to_le_bytes());
        // =A2*2: ptgRef row 1 col 0 (relative), ptgInt 2, ptgMul.
        num.extend(fmla(&[0x24, 1, 0, 0, 0, 0, 0xC0, 0x1E, 2, 0, 0x05], &[]));
        let book = open(&xlsb(
            &[],
            &[
                rec(BRT_ROW_HDR, &1u32.to_le_bytes()),
                rec(5, &real),
                rec(6, &st),
                rec(9, &num),
            ],
        ));
        let c = &book.sheets[0].cells;
        assert_eq!(book.sheets[0].name, "Data");
        assert_eq!(c[&(1, 0)].value, CellValue::Number(2.5));
        assert_eq!(c[&(1, 1)].value, CellValue::Text("hé".into()));
        assert_eq!(c[&(1, 2)].formula.as_deref(), Some("A2*2"));
    }

    #[test]
    fn shared_formulas_and_their_extra_column() {
        // C1:C2 share =A1+1 (ptgRefN row 0 col -2); ptgExp carries the row,
        // its column rides in the extra data.
        let exp = [0x01, 0, 0, 0, 0];
        let mut a = cell(2, 0);
        a.extend(1.0f64.to_le_bytes());
        a.extend(fmla(&exp, &2u32.to_le_bytes()));
        let mut shr = Vec::new();
        for v in [0u32, 1, 2, 2] {
            shr.extend(v.to_le_bytes());
        }
        let rgce = [0x4C, 0, 0, 0, 0, 0xFE, 0xFF, 0x1E, 1, 0, 0x03];
        shr.extend((rgce.len() as u32).to_le_bytes());
        shr.extend(rgce);
        shr.extend(0u32.to_le_bytes());
        let mut b = cell(2, 0);
        b.extend(2.0f64.to_le_bytes());
        b.extend(fmla(&exp, &2u32.to_le_bytes()));
        let book = open(&xlsb(
            &[],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                rec(9, &a),
                rec(BRT_SHR_FMLA, &shr),
                rec(BRT_ROW_HDR, &1u32.to_le_bytes()),
                rec(9, &b),
            ],
        ));
        let c = &book.sheets[0].cells;
        assert_eq!(c[&(0, 2)].formula.as_deref(), Some("A1+1"));
        assert_eq!(c[&(1, 2)].formula.as_deref(), Some("A2+1"));
    }

    /// ptgExp names the group's first formula cell (B1), not the stated
    /// range's top-left (A1:B2); members are B1, A2, B2.
    #[test]
    fn shared_formulas_keyed_by_the_cell_ptg_exp_names() {
        let member = |col: u32| {
            let mut v = cell(col, 0);
            v.extend(0.0f64.to_le_bytes());
            v.extend(fmla(&[0x01, 0, 0, 0, 0], &1u32.to_le_bytes()));
            v
        };
        let mut shr = Vec::new();
        for v in [0u32, 1, 0, 1] {
            shr.extend(v.to_le_bytes());
        }
        // =<cell one row down>+1: ptgRefN row +1, col +0.
        let rgce = [0x4C, 1, 0, 0, 0, 0, 0xC0, 0x1E, 1, 0, 0x03];
        shr.extend((rgce.len() as u32).to_le_bytes());
        shr.extend(rgce);
        shr.extend(0u32.to_le_bytes());
        let book = open(&xlsb(
            &[],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                rec(9, &member(1)),
                rec(BRT_SHR_FMLA, &shr),
                rec(BRT_ROW_HDR, &1u32.to_le_bytes()),
                rec(9, &member(0)),
                rec(9, &member(1)),
                rec(BRT_ROW_HDR, &2u32.to_le_bytes()),
                rec(9, &member(1)),
            ],
        ));
        let c = &book.sheets[0].cells;
        assert_eq!(c[&(0, 1)].formula.as_deref(), Some("B2+1"));
        assert_eq!(c[&(1, 0)].formula.as_deref(), Some("A3+1"));
        assert_eq!(c[&(1, 1)].formula.as_deref(), Some("B3+1"));
        assert_eq!(c[&(2, 1)].formula.as_deref(), Some("B4+1"));
    }

    /// Group 1 is A1:A3 with master A1; group 2's master is C1 but its
    /// stated range is A1:C3, whose top-left is group 1's master. The alias
    /// must not take A1's group, in either order of arrival.
    #[test]
    fn a_corner_alias_never_replaces_a_master() {
        let member = |row: u32, col: u32, master_col: u32| {
            let mut v = cell(col, 0);
            v.extend(0.0f64.to_le_bytes());
            v.extend(fmla(&[0x01, 0, 0, 0, 0], &master_col.to_le_bytes()));
            (row, rec(9, &v))
        };
        let shr = |c1: u32, c2: u32, op: u8| {
            let mut v = Vec::new();
            for x in [0u32, 2, c1, c2] {
                v.extend(x.to_le_bytes());
            }
            // =<cell one row down> op 2
            let rgce = [0x4C, 1, 0, 0, 0, 0, 0xC0, 0x1E, 2, 0, op];
            v.extend((rgce.len() as u32).to_le_bytes());
            v.extend(rgce);
            v.extend(0u32.to_le_bytes());
            (0, rec(BRT_SHR_FMLA, &v))
        };
        let group1 = vec![
            member(0, 0, 0),
            shr(0, 0, 0x03),
            member(1, 0, 0),
            member(2, 0, 0),
        ];
        let group2 = vec![
            member(0, 2, 2),
            shr(0, 2, 0x05),
            member(1, 2, 2),
            member(2, 2, 2),
        ];
        for order in [
            [group1.clone(), group2.clone()].concat(),
            [group2, group1].concat(),
        ] {
            let mut sheet = Vec::new();
            for (row, r) in order {
                sheet.push(rec(BRT_ROW_HDR, &row.to_le_bytes()));
                sheet.push(r);
            }
            // A BrtRowHdr before the BrtShrFmla resets nothing it needs.
            let book = open(&xlsb(&[], &sheet));
            let c = &book.sheets[0].cells;
            assert_eq!(c[&(0, 0)].formula.as_deref(), Some("A2+2"));
            assert_eq!(c[&(2, 0)].formula.as_deref(), Some("A4+2"));
            assert_eq!(c[&(0, 2)].formula.as_deref(), Some("C2*2"));
            assert_eq!(c[&(2, 2)].formula.as_deref(), Some("C4*2"));
        }
    }

    #[test]
    fn names_externals_and_the_1904_flag() {
        // XTI 0 → this workbook, sheet 0; TheData = Data!$A$1:$A$2, and a
        // hidden _xlfn.MAXIFS that is not a defined name.
        let mut xti = 1u32.to_le_bytes().to_vec();
        for v in [0u32, 0, 0] {
            xti.extend(v.to_le_bytes());
        }
        let name = |flags: u32, text: &str, rgce: &[u8]| {
            let mut n = flags.to_le_bytes().to_vec();
            n.push(0);
            n.extend(0xFFFF_FFFFu32.to_le_bytes());
            n.extend(wide(text));
            n.extend((rgce.len() as u32).to_le_bytes());
            n.extend_from_slice(rgce);
            n.extend(0u32.to_le_bytes());
            rec(BRT_NAME, &n)
        };
        let mut area = vec![0x3B, 0, 0];
        area.extend(0u32.to_le_bytes());
        area.extend(1u32.to_le_bytes());
        area.extend([0, 0, 0, 0]);
        let book = open(&xlsb(
            &[
                rec(BRT_WB_PROP, &[1, 0, 0, 0]),
                rec(BRT_SUP_SELF, &[]),
                rec(BRT_EXTERN_SHEET, &xti),
                name(0, "TheData", &area),
                name(0x0B, "_xlfn.MAXIFS", &[]),
            ],
            &[],
        ));
        assert!(book.date1904);
        assert_eq!(book.names.len(), 1);
        assert_eq!(book.names[0].name, "TheData");
        assert_eq!(book.names[0].formula, "Data!$A$1:$A$2");
    }

    /// `=Sheet2!Rate*2` with a name scoped to Sheet2 keeps the sheet.
    #[test]
    fn name_x_into_this_workbook_keeps_the_sheet() {
        let g = Globals {
            sheets: vec!["Data".into(), "Sheet2".into()],
            books: vec![Book::Own],
            links: Vec::new(),
            xti: vec![(0, 1, 1)],
            names: vec![("Global".into(), 0xFFFF_FFFF), ("Rate".into(), 1)],
            tables: HashMap::new(),
        };
        let f = [0x39, 0, 0, 2, 0, 0, 0, 0x1E, 2, 0, 0x05];
        let got = ptg::decompile(Biff::V12, &f, &[], Base::Cell(None), &g);
        assert_eq!(got.as_deref(), Some("Sheet2!Rate*2"));
        let f = [0x39, 0, 0, 1, 0, 0, 0];
        let got = ptg::decompile(Biff::V12, &f, &[], Base::Cell(None), &g);
        assert_eq!(got.as_deref(), Some("Global"));
    }

    /// ptgNameX into another book's function name reads `[k]!NAME`, `k`
    /// counting the books that have a function name: one without (the
    /// first) gets no link.
    #[test]
    fn name_x_into_an_external_book_is_numbered_and_qualified() {
        let src = |rid: &str| rec(BRT_SUP_BOOK_SRC, &wide(rid));
        // EUROCONVERT(2): the name through ptgNameX, then FuncVar 255.
        let mut call = name_x(2, 2);
        call.extend([0x1E, 2, 0, 0x22, 2, 0xFF, 0]);
        let book = open(&xlsb_links(
            &[
                src("rId2"),
                src("rId3"),
                src("rId4"),
                rec(BRT_SUP_SELF, &[]),
                extern_sheet(&[(0, -2, -2), (1, -2, -2), (2, -2, -2), (3, 0, 0)]),
            ],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                fmla_num(0, 1.0, &name_x(1, 1)),
                fmla_num(1, 2.0, &call),
            ],
            &[
                ("rId2", link_part(0, "rId1", &["S"], &[]), LINK_RELS),
                ("rId3", link_part(0, "rId1", &[], &["ONE"]), LINK_RELS),
                (
                    "rId4",
                    link_part(0, "rId1", &["1028", "1030"], &["X", "EUROCONVERT"]),
                    LINK_RELS,
                ),
            ],
        ));
        let c = &book.sheets[0].cells;
        assert_eq!(c[&(0, 0)].formula.as_deref(), Some("[1]!ONE"));
        assert_eq!(c[&(0, 1)].formula.as_deref(), Some("[2]!EUROCONVERT(2)"));
        let links = &book.external_links;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].names, [Some("ONE".to_string())]);
        assert_eq!(links[1].book, "rId1");
        assert_eq!(links[1].sheets, ["1028", "1030"]);
        assert_eq!(
            links[1].names,
            [Some("X".to_string()), Some("EUROCONVERT".to_string())]
        );
        assert_eq!(
            links[1].rels,
            [(
                "rId1".to_string(),
                "http://schemas.microsoft.com/office/2006/relationships/xlExternalLinkPath/xlLibrary".to_string(),
                "TOOLS.XLAM".to_string(),
                Some("External".to_string()),
            )]
        );
    }

    /// A ptgNameX that names nothing readable keeps its cell's value and
    /// loses the formula: a book with no part, a DDE book, an XLL add-in,
    /// a name scoped to a sheet of the book, a name past the book's names,
    /// a book whose own rel is missing, and an XTI past the books.
    #[test]
    fn name_x_leniency() {
        let src = |rid: &str| rec(BRT_SUP_BOOK_SRC, &wide(rid));
        let book = open(&xlsb_links(
            &[
                src("rId9"),
                src("rId2"),
                rec(BRT_SUP_ADDIN, &[]),
                src("rId3"),
                src("rId4"),
                rec(BRT_SUP_SELF, &[]),
                extern_sheet(&[
                    (0, -2, -2),
                    (1, -2, -2),
                    (2, -2, -2),
                    (3, 0, 0),
                    (3, -2, -2),
                    (4, -2, -2),
                    (9, -2, -2),
                ]),
            ],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                fmla_num(0, 10.0, &name_x(0, 1)),
                fmla_num(1, 11.0, &name_x(1, 1)),
                fmla_num(2, 12.0, &name_x(2, 1)),
                fmla_num(3, 13.0, &name_x(3, 1)),
                fmla_num(4, 14.0, &name_x(4, 2)),
                fmla_num(5, 15.0, &name_x(5, 1)),
                fmla_num(6, 16.0, &name_x(6, 1)),
                // The one that resolves.
                fmla_num(7, 17.0, &name_x(4, 1)),
            ],
            &[
                ("rId2", link_part(1, "rId1", &[], &["F"]), LINK_RELS),
                ("rId3", link_part(0, "rId1", &["S"], &["F"]), LINK_RELS),
                ("rId4", link_part(0, "rId7", &[], &["G"]), LINK_RELS),
            ],
        ));
        let c = &book.sheets[0].cells;
        for col in 0..7 {
            assert_eq!(c[&(0, col)].formula, None, "column {col}");
            assert_eq!(c[&(0, col)].value, CellValue::Number(10.0 + col as f64));
        }
        assert_eq!(c[&(0, 7)].formula.as_deref(), Some("[1]!F"));
        assert_eq!(book.external_links.len(), 1);
    }

    /// Only a function name (no tokens in its BrtSupNameFmla) is read: a
    /// range or constant name of the book, or one whose definition is
    /// missing or cut short, keeps its place among the names but gives no
    /// formula, and only function names are written. A book with no
    /// function name gets no link.
    #[test]
    fn only_function_names_of_an_external_book_are_read() {
        let src = |rid: &str| rec(BRT_SUP_BOOK_SRC, &wide(rid));
        let name = |n: &str, fmla: Option<&[u8]>| {
            let mut v = rec(BRT_SUP_NAME_START, &wide(n));
            if let Some(f) = fmla {
                v.extend(rec(BRT_SUP_NAME_FMLA, f));
            }
            v
        };
        // Excel's: Prices = ptgArea3d, Half = #REF!.
        let prices: &[u8] = &[13, 0, 0, 0, 0x3B, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        let half: &[u8] = &[2, 0, 0, 0, 0x1C, 0x17];
        let mut part = link_part(0, "rId1", &["P"], &["F"]);
        part.extend(name("Prices", Some(prices)));
        part.extend(name("Half", Some(half)));
        part.extend(name("Missing", None));
        part.extend(name("Cut", Some(&[0, 0])));
        part.extend(name("G", Some(&[0; 4])));
        let mut ranges = link_part(0, "rId1", &["P"], &[]);
        ranges.extend(name("Prices", Some(prices)));
        let book = open(&xlsb_links(
            &[
                src("rId2"),
                src("rId3"),
                extern_sheet(&[(0, -2, -2), (1, -2, -2)]),
            ],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                fmla_num(0, 1.0, &name_x(0, 1)),
                fmla_num(1, 2.0, &name_x(0, 2)),
                fmla_num(2, 3.0, &name_x(0, 3)),
                fmla_num(3, 4.0, &name_x(0, 4)),
                fmla_num(4, 5.0, &name_x(0, 5)),
                fmla_num(5, 6.0, &name_x(0, 6)),
                fmla_num(6, 7.0, &name_x(1, 1)),
            ],
            &[("rId2", part, LINK_RELS), ("rId3", ranges, LINK_RELS)],
        ));
        let c = &book.sheets[0].cells;
        let got: Vec<Option<&str>> = (0..7).map(|col| c[&(0, col)].formula.as_deref()).collect();
        assert_eq!(
            got,
            [Some("[1]!F"), None, None, None, None, Some("[1]!G"), None]
        );
        for col in 0..7 {
            assert_eq!(c[&(0, col)].value, CellValue::Number(col as f64 + 1.0));
        }
        assert_eq!(book.external_links.len(), 1);
        let pkg = book.build();
        let link = String::from_utf8(
            pkg.part("xl/externalLinks/externalLink1.xml")
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            link.contains(
                r#"<definedNames><definedName name="F"/><definedName name="G"/></definedNames>"#
            ),
            "{link}"
        );
        assert!(pkg.part("xl/externalLinks/externalLink2.xml").is_none());
    }

    /// Books naming the same link part are one link, read once, however
    /// many there are; and the link budget refuses a workbook past it.
    #[test]
    fn a_repeated_book_is_one_link_and_links_are_budgeted() {
        let src = |rid: &str| rec(BRT_SUP_BOOK_SRC, &wide(rid));
        let mut workbook = vec![src("rId2"); 100_000];
        workbook.push(extern_sheet(&[(0, -2, -2), (99_999, -2, -2)]));
        let started = std::time::Instant::now();
        let book = open(&xlsb_links(
            &workbook,
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                fmla_num(0, 1.0, &name_x(0, 1)),
                fmla_num(1, 2.0, &name_x(1, 1)),
            ],
            &[("rId2", link_part(0, "rId1", &[], &["F"]), LINK_RELS)],
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(book.external_links.len(), 1);
        let c = &book.sheets[0].cells;
        assert_eq!(c[&(0, 0)].formula.as_deref(), Some("[1]!F"));
        assert_eq!(c[&(0, 1)].formula.as_deref(), Some("[1]!F"));

        let two = xlsb_links(
            &[src("rId2"), src("rId3")],
            &[],
            &[
                ("rId2", link_part(0, "rId1", &[], &["F"]), LINK_RELS),
                ("rId3", link_part(0, "rId1", &[], &["G"]), LINK_RELS),
            ],
        );
        assert_eq!(open(&two).external_links.len(), 2);
        let tight = Limits {
            links: 1,
            ..Limits::default()
        };
        let err = read_with(&ZipArchive::open(&two).unwrap(), tight)
            .err()
            .unwrap();
        assert!(err.to_string().contains("too many external links"), "{err}");
    }

    /// Two sheets naming the same part: one sheet, read once; and the cell
    /// budget refuses a workbook past it.
    #[test]
    fn a_part_two_sheets_name_is_read_once_and_cells_are_budgeted() {
        let mut again = vec![0u8; 8];
        again.extend(wide("rId1"));
        again.extend(wide("Again"));
        let mut a = cell(0, 0);
        a.extend(1.0f64.to_le_bytes());
        let mut b = cell(1, 0);
        b.extend(2.0f64.to_le_bytes());
        let bytes = xlsb(
            &[rec(BRT_BUNDLE_SH, &again)],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                rec(5, &a),
                rec(5, &b),
            ],
        );
        let book = open(&bytes);
        assert_eq!(book.sheets.len(), 1);
        let tight = Limits {
            cells: 1,
            ..Limits::default()
        };
        let err = read_with(&ZipArchive::open(&bytes).unwrap(), tight)
            .err()
            .unwrap();
        assert!(err.to_string().contains("too many cells"), "{err}");
    }

    #[test]
    fn cells_past_the_grid_are_dropped() {
        let mut far = cell(16384, 0);
        far.extend(1.0f64.to_le_bytes());
        let mut deep = cell(0, 0);
        deep.extend(2.0f64.to_le_bytes());
        let mut ok = cell(16383, 0);
        ok.extend(3.0f64.to_le_bytes());
        let book = open(&xlsb(
            &[],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                rec(5, &far),
                rec(5, &ok),
                rec(BRT_ROW_HDR, &2_000_000u32.to_le_bytes()),
                rec(5, &deep),
            ],
        ));
        let c = &book.sheets[0].cells;
        assert_eq!(c.len(), 1);
        assert_eq!(c[&(0, 16383)].value, CellValue::Number(3.0));
    }

    #[test]
    fn leniency_unknown_records_ptgs_and_truncation() {
        let mut bad = cell(0, 0);
        bad.extend(7.0f64.to_le_bytes());
        bad.extend(fmla(&[0x18, 0x19, 0, 0], &[]));
        let mut ok = cell(1, 0);
        ok.extend(1.0f64.to_le_bytes());
        let mut sheet = vec![
            rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
            rec(9999, &[1, 2, 3]),
            rec(9, &bad),
            rec(5, &[0, 0]),
            rec(5, &ok),
        ];
        // A record claiming 20 bytes with 2 left.
        sheet.push(vec![5, 20, 1, 2]);
        let book = open(&xlsb(&[rec(9998, &[])], &sheet));
        let c = &book.sheets[0].cells;
        assert_eq!(c[&(0, 0)].value, CellValue::Number(7.0));
        assert_eq!(c[&(0, 0)].formula, None);
        assert_eq!(c[&(0, 1)].value, CellValue::Number(1.0));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn opens_through_the_sniffer() {
        let bytes = xlsb(&[], &[]);
        let (pkg, fmt) = super::super::open_workbook(&bytes).unwrap();
        assert_eq!(fmt, super::super::SourceFormat::Xlsb);
        assert_eq!(pkg.workbook.sheets[0].name, "Data");
    }
}
