//! Excel Binary Workbook `.xlsb`: BIFF12 records in an OPC package
//! ([MS-XLSB]). The parts are the `.xlsx` ones with `.bin` bodies:
//! `xl/workbook.bin` (sheets, names, externals, the date system),
//! `xl/sharedStrings.bin`, `xl/styles.bin` and one `.bin` per worksheet.
//!
//! A record is a varint type (1-2 bytes) and a varint size (1-4 bytes) and
//! its body. Records not listed here are skipped.

use std::collections::{BTreeMap, HashMap, HashSet};

use opccore::zip::ZipArchive;

use super::ftab;
use super::ptg::{self, Base, Biff, Names, Table, utf16};
use super::xlsb_style::Styles;
use super::{
    BookIn, ColIn, ExtName, ExtValue, ExternalLink, Le, Limits, OpenError, SheetIn, biff_error,
    ext_definition, external_prefix, on_grid, rk, set_array, sheet_prefix,
};
use crate::sheet::{Cell, CellValue, DefinedName};
use crate::xlsx::{parse_rels, parse_rels_mode, rels_part_name, resolve_relative};

/// The records of a part: (type, body). A record cut off by the end of the
/// part ends the list.
pub(super) fn records(b: &[u8]) -> Vec<(u32, &[u8])> {
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
    pub(super) fn wide(&mut self) -> Option<String> {
        let n = self.u32()?;
        if n == 0xFFFF_FFFF {
            return Some(String::new());
        }
        Some(utf16(self.take((n as usize).checked_mul(2)?)?))
    }
    /// A parsed formula: `cce`, the tokens, `cb`, the extra data.
    pub(super) fn formula(&mut self) -> Option<(&'a [u8], &'a [u8])> {
        let cce = self.u32()? as usize;
        let rgce = self.take(cce)?;
        let cb = self.u32()? as usize;
        let extra = self.take(cb)?;
        Some((rgce, extra))
    }
}

const BRT_ROW_HDR: u32 = 0;
const BRT_NAME: u32 = 39;
const BRT_WB_PROP: u32 = 153;
const BRT_BUNDLE_SH: u32 = 156;
const BRT_SST_ITEM: u32 = 19;
const BRT_SUP_BOOK_SRC: u32 = 355;
const BRT_SUP_SELF: u32 = 357;
const BRT_SUP_SAME: u32 = 358;
const BRT_SUP_TABS: u32 = 359;
const BRT_BEGIN_SUP_BOOK: u32 = 360;
/// An external link part's cached cells ([`read_link`]): the sheet, then
/// per row its header and cells ([MS-XLSB] 2.4: BrtExternTableStart,
/// BrtExternTableEnd, BrtExternRowHdr, BrtExternCellBlank, ...Real,
/// ...Bool, ...Error, ...String).
const BRT_EXTERN_TABLE_START: u32 = 363;
const BRT_EXTERN_TABLE_END: u32 = 364;
const BRT_EXTERN_ROW_HDR: u32 = 366;
const BRT_EXTERN_CELL_BLANK: u32 = 367;
const BRT_EXTERN_CELL_REAL: u32 = 368;
const BRT_EXTERN_CELL_BOOL: u32 = 369;
const BRT_EXTERN_CELL_ERROR: u32 = 370;
const BRT_EXTERN_CELL_STRING: u32 = 371;
/// A name of the BrtSupAddin book before it: one function the XLL
/// registers (corpus/legacy/addin/xll-udf.xlsb has XLLTWICE's).
const BRT_PLACEHOLDER_NAME: u32 = 361;
const BRT_EXTERN_SHEET: u32 = 362;
const BRT_ARR_FMLA: u32 = 426;
const BRT_SHR_FMLA: u32 = 427;
const BRT_SUP_NAME_START: u32 = 577;
/// A name's definition in an external link part: `cce` and tokens (see
/// [`read_link`]; corpus/legacy/addin has both kinds).
const BRT_SUP_NAME_FMLA: u32 = 585;
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
    /// [`Globals::links`], or `None` when its link part isn't read
    /// ([`read_link`]).
    External(Option<usize>),
    /// An XLL add-in (BrtSupAddin): the functions it registers, from the
    /// BrtPlaceholderName records after it, spelled as [`ftab::xll_name`]
    /// gives; `None` for one cut short (ptgNameX still counts it).
    AddIn(Vec<Option<String>>),
}

/// The globals a token stream refers to.
struct Globals {
    sheets: Vec<String>,
    /// Per SUPBOOK, what it is.
    books: Vec<Book>,
    /// The external books whose link parts were read ([`read_link`]), `[1]`
    /// first.
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
        match *self.books.get(book as usize)? {
            Book::Own => {
                let name = |i: i32| self.sheets.get(usize::try_from(i).ok()?).cloned();
                Some(sheet_prefix(&name(first)?, &name(last)?))
            }
            // One sheet of a linked book (#903): `[1]P!`. A span of its
            // sheets has no file showing its spelling.
            Book::External(Some(k)) if first == last => {
                let sheets = &self.links[k].sheets;
                external_prefix(k + 1, sheets.get(usize::try_from(first).ok()?)?)
            }
            _ => None,
        }
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
            // A name of the whole book (first sheet -2), as the `.xlsx`
            // spells it: `[1]!EUROCONVERT`, `[1]!Prices`. A reference to a
            // name scoped to a sheet of the book isn't read.
            Book::External(Some(k)) if first == -2 => {
                let name = self.links[k].names.get(index.checked_sub(1)? as usize)?;
                Some(format!("[{}]!{}", k + 1, name.as_ref()?.name))
            }
            Book::AddIn(ref names) => names.get(index.checked_sub(1)? as usize)?.clone(),
            Book::External(_) => None,
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

/// BrtCommentAuthor, BrtBeginComment and BrtCommentText: [MS-XLSB] 2.4
/// (record ids 632, 635 and 637), in the comments part
/// ([MS-XLSB] 2.1.7.8) a sheet's rels name.
const BRT_COMMENT_AUTHOR: u32 = 632;
const BRT_BEGIN_COMMENT: u32 = 635;
const BRT_COMMENT_TEXT: u32 = 637;

/// The comments of the sheet in `part` (#884): (row, col, author, text).
/// BrtBeginComment: iauth (into the BrtCommentAuthor list), an RfX
/// (rwFirst, rwLast, colFirst, colLast) and a GUID; BrtCommentText: a
/// RichStr (flags, then the plain text; its runs are not read).
fn read_comments(zip: &ZipArchive, part: &str) -> Vec<(u32, u32, String, String)> {
    let mut out = Vec::new();
    for (ty, target) in rels(zip, part).into_values() {
        if !ty.ends_with("/comments") {
            continue;
        }
        let Some(bytes) = zip.read(&target) else {
            continue;
        };
        let mut authors: Vec<String> = Vec::new();
        let mut at: Option<(u32, u32, String)> = None;
        for (ty, body) in records(&bytes) {
            let mut c = Cur::new(body);
            let _ = (|| -> Option<()> {
                match ty {
                    BRT_COMMENT_AUTHOR => authors.push(c.wide()?),
                    BRT_BEGIN_COMMENT => {
                        let iauth = c.u32()? as usize;
                        let (row, _, col) = (c.u32()?, c.u32()?, c.u32()?);
                        at = on_grid(row, col)
                            .then(|| (row, col, authors.get(iauth).cloned().unwrap_or_default()));
                    }
                    BRT_COMMENT_TEXT => {
                        let (row, col, author) = at.take()?;
                        c.u8()?;
                        out.push((row, col, author, c.wide()?));
                    }
                    _ => {}
                }
                Some(())
            })();
        }
    }
    out
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
/// names, names and cached cells, and the part's relationships. `None`
/// unless it is a workbook's (`sbt` 0, not DDE or OLE) whose rel is there
/// (#888, #903).
///
/// Every name (BrtSupNameStart) is kept, with its definition from the
/// BrtSupNameFmla after it: none for a function (`cce` 0, as Excel writes
/// EUROTOOL.XLAM's names), the `refersTo` [`ext_definition`] reads for a
/// range or constant name (`Prices`: an area, `Half`: `#REF!`), and none
/// when the record is missing, cut short or of another shape. Its scope
/// (in BrtSupNameBits) isn't read: no file shows it, so every name is the
/// book's. A name that can't be read is `None`, keeping its place.
///
/// The cached cells are the BrtExternTableStart blocks: the sheet, then
/// rows (BrtExternRowHdr) of cells. A blank is left out, as is a cell cut
/// short, off the grid or on a sheet the book doesn't list. Each cell kept
/// is charged to `book`'s cell budget as it is read, so a crafted part is
/// refused (the error) before its cache outgrows the budget.
fn read_link(
    zip: &ZipArchive,
    part: &str,
    book: &mut BookIn,
) -> Result<Option<ExternalLink>, OpenError> {
    let Some(bytes) = zip.read(part) else {
        return Ok(None);
    };
    let mut link = ExternalLink::default();
    let mut workbook = false;
    // The sheet and row the cached cells go in.
    let (mut sheet, mut row): (Option<u32>, Option<u32>) = (None, None);
    let mut over: Option<OpenError> = None;
    for (ty, body) in records(&bytes) {
        let mut c = Cur::new(body);
        let _ = (|| -> Option<()> {
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
                    link.names.push(None);
                    *link.names.last_mut()? = Some(ExtName::function(c.wide()?));
                }
                BRT_SUP_NAME_FMLA => {
                    let cce = c.u32()? as usize;
                    let rgce = c.take(cce)?;
                    let refers_to = ext_definition(rgce, &link.sheets);
                    link.names.last_mut()?.as_mut()?.refers_to = refers_to;
                }
                BRT_EXTERN_TABLE_START => {
                    sheet = Some(c.u32()?);
                    row = None;
                }
                BRT_EXTERN_TABLE_END => (sheet, row) = (None, None),
                BRT_EXTERN_ROW_HDR => row = Some(c.u32()?),
                BRT_EXTERN_CELL_BLANK..=BRT_EXTERN_CELL_STRING => {
                    let (s, r) = (sheet?, row?);
                    let col = c.u32()?;
                    let value = match ty {
                        BRT_EXTERN_CELL_REAL => ExtValue::Num(c.f64()?),
                        BRT_EXTERN_CELL_BOOL => ExtValue::Bool(c.u8()? != 0),
                        BRT_EXTERN_CELL_ERROR => ExtValue::Err(biff_error(c.u8()?)),
                        BRT_EXTERN_CELL_STRING => ExtValue::Str(c.wide()?),
                        _ => return None,
                    };
                    if (s as usize) >= link.sheets.len() || !on_grid(r, col) {
                        return None;
                    }
                    if !link.cache.contains_key(&(s, r, col)) {
                        if let Err(e) = book.charge_cells(1) {
                            over = Some(e);
                            return None;
                        }
                    }
                    link.cache.insert((s, r, col), value);
                }
                _ => {}
            }
            Some(())
        })();
        if let Some(e) = over {
            return Err(e);
        }
    }
    let Some(bytes) = zip.read(&rels_part_name(part)) else {
        return Ok(None);
    };
    link.rels = parse_rels_mode(&String::from_utf8_lossy(&bytes));
    let named = link.rels.iter().any(|(id, ..)| *id == link.book);
    Ok((workbook && named).then_some(link))
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
    // A link part whose cache outgrew the cell budget.
    let mut link_err: Option<OpenError> = None;
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
                    // have one: every workbook's whose part is read. That
                    // is Excel's `[k]` unless a book without one (DDE, OLE,
                    // a part missing) comes first: then the numbers differ
                    // from Excel's but still name the links this import
                    // writes. A part several books name is read once and
                    // is one link.
                    let part = c.wide().and_then(|rid| rel.get(&rid));
                    let k = match part {
                        None => None,
                        Some((_, part)) => match link_of.get(part) {
                            Some(&k) => k,
                            None => {
                                let read = match read_link(zip, part, &mut book) {
                                    Ok(l) => l,
                                    Err(e) => {
                                        link_err = Some(e);
                                        None
                                    }
                                };
                                let k = match read {
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
                BRT_SUP_ADDIN => g.books.push(Book::AddIn(Vec::new())),
                // Its slot first, so one cut short doesn't shift the rest.
                BRT_PLACEHOLDER_NAME => {
                    if let Some(Book::AddIn(names)) = g.books.last_mut() {
                        names.push(None);
                        let name = c.wide()?;
                        *names.last_mut()? = Some(ftab::xll_name(&name));
                    }
                }
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
    if let Some(e) = link_err {
        return Err(e);
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

    // styles.bin: the cell formats and the differential formats.
    let mut styles = zip
        .read("xl/styles.bin")
        .map(|b| Styles::read(&b))
        .unwrap_or_default();

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
        let Some((part, bytes)) = part
            .as_ref()
            .filter(|p| p.contains("worksheets/") && seen.insert(p.as_str()))
            .and_then(|p| Some((p, zip.read(p)?)))
        else {
            imported.push(None);
            continue;
        };
        let mut sheet = read_sheet(&bytes, &g, &sst, &mut book, &mut styles)?;
        sheet.name = g.sheets[i].clone();
        sheet.comments = read_comments(zip, part);
        imported.push(Some(book.sheets.len()));
        book.push_sheet(sheet)?;
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

/// BrtColInfo, [MS-XLSB] 2.4 (record id 60).
const BRT_COL_INFO: u32 = 60;
/// BrtMergeCell, [MS-XLSB] 2.4 (record id 176).
const BRT_MERGE_CELL: u32 = 176;
/// BrtWsFmtInfo, [MS-XLSB] 2.4 (record id 485).
const BRT_WS_FMT_INFO: u32 = 485;
/// BrtDVal, [MS-XLSB] 2.4 (record id 64), inside BrtBeginDVals (573).
const BRT_DVAL: u32 = 64;
/// BrtBeginConditionalFormatting and BrtBeginCFRule, [MS-XLSB] 2.4
/// (record ids 461 and 463).
const BRT_BEGIN_CONDITIONAL_FORMATTING: u32 = 461;
/// BrtEndConditionalFormatting, [MS-XLSB] 2.4 (record id 462).
const BRT_END_CONDITIONAL_FORMATTING: u32 = 462;
const BRT_BEGIN_CF_RULE: u32 = 463;

/// BrtBeginCFRule: iType (1 cellIs, 2 expression), iTemplate, dxfId, iPri,
/// iParam (cellIs's operator), two reserved, flags, cbFmla1..3,
/// strParam, then the formulas whose cbFmla isn't 0 (CFParsedFormulas,
/// their relative references based at `at`, the block's ranges' bounds'
/// top-left). `None` for a rule the model doesn't hold.
fn read_cf_rule(
    c: &mut Cur,
    at: (u32, u32),
    styles: &Styles,
    g: &Globals,
) -> Option<(crate::xlsx::NewCfRule, crate::sheet::Dxf)> {
    use crate::xlsx::NewCfRule;
    let (ty, _template, dxf_id) = (c.u32()?, c.u32()?, c.u32()?);
    c.u32()?;
    let param = c.u32()?;
    c.take(10)?;
    let cb = [c.u32()?, c.u32()?, c.u32()?];
    c.wide()?;
    let mut formulas = Vec::new();
    for n in cb {
        if n == 0 {
            continue;
        }
        let (rgce, extra) = c.formula()?;
        formulas.push(ptg::decompile(
            Biff::V12,
            rgce,
            extra,
            Base::Shared(at.0, at.1),
            g,
        )?);
    }
    let dxf = styles
        .dxfs
        .get(dxf_id as usize)
        .cloned()
        .unwrap_or_default();
    let kind = match ty {
        1 if !formulas.is_empty() => NewCfRule::CellIs {
            op: super::cf_operator(param)?.to_string(),
            formulas,
        },
        2 => NewCfRule::Expression {
            formula: formulas.into_iter().next()?,
        },
        _ => return None,
    };
    Some((kind, dxf))
}

/// An UncheckedSqRfX: crfx, then each RfX (rwFirst, rwLast, colFirst,
/// colLast), as (r1, c1, r2, c2); the ones off the grid left out.
fn sqrfx(c: &mut Cur) -> Option<Vec<(u32, u32, u32, u32)>> {
    let n = c.u32()?;
    let mut out = Vec::new();
    for _ in 0..n {
        let (r1, r2, c1, c2) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
        if r2 >= r1 && c2 >= c1 && on_grid(r2, c2) {
            out.push((r1, c1, r2, c2));
        }
    }
    Some(out)
}

/// BrtDVal: flags (as BIFF8's DV, [`super::dv_from_flags`]), sqrfx, then
/// strErrorTitle, strError, strPromptTitle and strPrompt
/// (XLNullableWideStrings), formula1 and formula2 (CellParsedFormulas,
/// their relative references based at the first range's top-left, as
/// BIFF8's DV; the build moves them to the model's anchor).
fn read_dval(c: &mut Cur, g: &Globals) -> Option<super::DvIn> {
    let mut dv = super::dv_from_flags(c.u32()?);
    dv.ranges = sqrfx(c)?;
    dv.error_title = c.wide()?;
    dv.error = c.wide()?;
    dv.prompt_title = c.wide()?;
    let prompt = c.wide()?;
    dv.prompt = (!prompt.is_empty()).then_some(prompt);
    let &(r, col, ..) = dv.ranges.first()?;
    let formula = |c: &mut Cur| -> Option<String> {
        let (rgce, extra) = c.formula()?;
        if rgce.is_empty() {
            return Some(String::new());
        }
        ptg::decompile(Biff::V12, rgce, extra, Base::Shared(r, col), g)
    };
    dv.formula1 = formula(c)?;
    dv.formula2 = formula(c)?;
    if dv.kind == "list" {
        dv.formula1 = super::dv_list_formula(dv.formula1);
    }
    Some(super::DvIn {
        rule: dv,
        base: (r, col),
    })
}

/// A sheet part: its cells, column widths and merged cells (the name is
/// the caller's).
fn read_sheet(
    bytes: &[u8],
    g: &Globals,
    sst: &[String],
    book: &mut BookIn,
    styles: &mut Styles,
) -> Result<SheetIn, OpenError> {
    let mut sheet = SheetIn::default();
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
    // The conditional-formatting block whose rules follow: the index of the
    // one just pushed, none after a block that wasn't (or its end).
    let mut cf_open: Option<usize> = None;
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
                        style: styles.of(style, book),
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
                // BrtWsFmtInfo: dxGCol (the default width, 1/256
                // characters, 0xFFFFFFFF for none), cchDefColWidth (the
                // base width, characters), miyDefRwHeight and flags.
                BRT_WS_FMT_INFO => {
                    let dx = c.u32()?;
                    let base = c.u16()? as u32;
                    sheet.base_col_width = (base != 8).then_some(base);
                    // Excel writes the width the base gives (2194 for 8):
                    // only another one is the sheet's own default.
                    sheet.default_col_width = u16::try_from(dx)
                        .ok()
                        .filter(|&dx| !super::xls::is_default_coldx(sheet.base_col_width, None, dx))
                        .map(|dx| dx as f64 / 256.0);
                }
                // BrtColInfo: colFirst, colLast, coldx (1/256 characters),
                // ixfe, then flags: fHidden (bit 0), fUserSet (1),
                // iOutLevel (8-10) and fCollapsed (12). Read as `.xls`'s
                // COLINFO is: a width at the sheet's default without
                // fUserSet is no width.
                BRT_COL_INFO => {
                    let (first, last, coldx) = (c.u32()?, c.u32()?, c.u32()?);
                    c.u32()?;
                    let flags = c.u16()?;
                    let hidden = flags & 0x0001 != 0;
                    let user = flags & 0x0002 != 0;
                    let default = u16::try_from(coldx).is_ok_and(|dx| {
                        super::xls::is_default_coldx(
                            sheet.base_col_width,
                            sheet.default_col_width,
                            dx,
                        )
                    });
                    let width = (user || !default).then_some(coldx as f64 / 256.0);
                    let level = ((flags >> 8) & 0x0007) as u8;
                    let collapsed = flags & 0x1000 != 0;
                    let last = last.min(crate::sheet::MAX_COLS - 1);
                    if (width.is_some() || hidden || level > 0 || collapsed) && first <= last {
                        sheet.cols.push(ColIn {
                            first,
                            last,
                            width,
                            hidden,
                            plain: width.is_some() && !user,
                            level,
                            collapsed,
                        });
                    }
                }
                BRT_DVAL => {
                    if let Some(dv) = read_dval(&mut c, g) {
                        sheet.validations.push(dv);
                    }
                }
                // BrtBeginConditionalFormatting: ccf, fPivot, sqrfx. Its
                // BrtBeginCFRules follow.
                BRT_BEGIN_CONDITIONAL_FORMATTING => {
                    cf_open = None;
                    c.take(8)?;
                    let ranges = sqrfx(&mut c)?;
                    // Based, as BIFF8's CONDFMT, at the ranges' bounds'
                    // top-left.
                    if let Some(base) = super::anchor_of(&ranges) {
                        sheet.cond_formats.push(super::CfIn {
                            ranges,
                            rules: Vec::new(),
                            base,
                        });
                        cf_open = Some(sheet.cond_formats.len() - 1);
                    }
                }
                BRT_END_CONDITIONAL_FORMATTING => cf_open = None,
                BRT_BEGIN_CF_RULE => {
                    let cf = sheet.cond_formats.get_mut(cf_open?)?;
                    let rule = read_cf_rule(&mut c, cf.base, styles, g)?;
                    cf.rules.push(rule);
                }
                // BrtMergeCell: an RfX (rwFirst, rwLast, colFirst,
                // colLast).
                BRT_MERGE_CELL => {
                    let (r1, r2, c1, c2) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
                    if r2 >= r1 && c2 >= c1 && on_grid(r2, c2) {
                        sheet.merges.push((r1, c1, r2, c2));
                    }
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
    sheet.cells = cells;
    Ok(sheet)
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
        xlsb_with(workbook, sheet, links, Vec::new())
    }

    /// [`xlsb_links`] plus `extra` parts (name, bytes): `xl/styles.bin`,
    /// a sheet's rels and what they name.
    fn xlsb_with(
        workbook: &[Vec<u8>],
        sheet: &[Vec<u8>],
        links: &[(&str, Vec<u8>, &str)],
        extra: Vec<(String, Vec<u8>)>,
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
        parts.extend(extra);
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
    /// counting the books whose parts are read: one with no names (the
    /// first) is a link too, as Excel keeps it (#903).
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
        assert_eq!(c[&(0, 0)].formula.as_deref(), Some("[2]!ONE"));
        assert_eq!(c[&(0, 1)].formula.as_deref(), Some("[3]!EUROCONVERT(2)"));
        let links = &book.external_links;
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].sheets, ["S"]);
        assert!(links[0].names.is_empty());
        assert_eq!(links[1].names, [Some("ONE".into())]);
        assert_eq!(links[2].book, "rId1");
        assert_eq!(links[2].sheets, ["1028", "1030"]);
        assert_eq!(
            links[2].names,
            [Some("X".into()), Some("EUROCONVERT".into())]
        );
        assert_eq!(
            links[2].rels,
            [(
                "rId1".to_string(),
                "http://schemas.microsoft.com/office/2006/relationships/xlExternalLinkPath/xlLibrary".to_string(),
                "TOOLS.XLAM".to_string(),
                Some("External".to_string()),
            )]
        );
    }

    /// A ptgNameX that names nothing readable keeps its cell's value and
    /// loses the formula: a book with no part, a DDE book, an XLL add-in
    /// with no name at that index, a name scoped to a sheet of the book, a
    /// name past the book's names, a book whose own rel is missing, and an
    /// XTI past the books.
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

    /// A BrtSupAddin book's names are the BrtPlaceholderName records after
    /// it (corpus/legacy/addin/xll-udf.xlsb): `_xll.NAME` unless built in.
    /// One cut short keeps its place and gives no formula; one after any
    /// other kind of book is not read.
    #[test]
    fn name_x_into_an_xll_add_in_book() {
        let book = open(&xlsb(
            &[
                rec(BRT_SUP_ADDIN, &[]),
                rec(BRT_PLACEHOLDER_NAME, &[1, 0]),
                rec(BRT_PLACEHOLDER_NAME, &wide("XLLTWICE")),
                rec(BRT_PLACEHOLDER_NAME, &wide("EDATE")),
                rec(BRT_SUP_SELF, &[]),
                rec(BRT_PLACEHOLDER_NAME, &wide("Stray")),
                extern_sheet(&[(0, -2, -2), (1, -2, -2)]),
            ],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                fmla_num(0, 10.0, &name_x(0, 1)),
                fmla_num(1, 11.0, &name_x(0, 2)),
                fmla_num(2, 12.0, &name_x(0, 3)),
                fmla_num(3, 13.0, &name_x(0, 4)),
                fmla_num(4, 14.0, &name_x(1, 1)),
            ],
        ));
        let c = &book.sheets[0].cells;
        let formulas: Vec<_> = (0..5).map(|col| c[&(0, col)].formula.as_deref()).collect();
        assert_eq!(
            formulas,
            [None, Some("_xll.XLLTWICE"), Some("EDATE"), None, None]
        );
        assert_eq!(c[&(0, 3)].value, CellValue::Number(13.0));
        assert!(book.external_links.is_empty());
    }

    /// Every name of an external book is read (#903): a function name (no
    /// tokens in its BrtSupNameFmla), a range or constant name with the
    /// `refersTo` its definition spells (`Prices` and `Half` as Excel wrote
    /// them), and one whose definition is missing, cut short or of another
    /// shape with none. A book with no function name is a link too. Its
    /// cached cells are kept, a blank, a sheet the book doesn't list and a
    /// cell off the grid left out.
    #[test]
    fn every_name_and_cached_cell_of_an_external_book_is_read() {
        let src = |rid: &str| rec(BRT_SUP_BOOK_SRC, &wide(rid));
        let name = |n: &str, fmla: Option<&[u8]>| {
            let mut v = rec(BRT_SUP_NAME_START, &wide(n));
            if let Some(f) = fmla {
                v.extend(rec(BRT_SUP_NAME_FMLA, f));
            }
            v
        };
        let cell = |ty: u32, col: u32, value: &[u8]| {
            let mut b = col.to_le_bytes().to_vec();
            b.extend_from_slice(value);
            rec(ty, &b)
        };
        // Excel's (corpus/legacy/addin/ext-name.xlsb): Prices = 'P'!$A$1:$A$2,
        // Half = #REF!.
        let prices: &[u8] = &[13, 0, 0, 0, 0x3B, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0];
        let half: &[u8] = &[2, 0, 0, 0, 0x1C, 0x17];
        let mut part = link_part(0, "rId1", &["P"], &["F"]);
        part.extend(name("Prices", Some(prices)));
        part.extend(name("Half", Some(half)));
        part.extend(name("Missing", None));
        part.extend(name("Cut", Some(&[0, 0])));
        part.extend(name("G", Some(&[0; 4])));
        part.extend(name("Int", Some(&[3, 0, 0, 0, 0x1E, 1, 0])));
        part.extend(rec(BRT_EXTERN_TABLE_START, &[0, 0, 0, 0, 0]));
        part.extend(rec(BRT_EXTERN_ROW_HDR, &0u32.to_le_bytes()));
        part.extend(cell(BRT_EXTERN_CELL_REAL, 0, &3.0f64.to_le_bytes()));
        part.extend(cell(BRT_EXTERN_CELL_STRING, 1, &wide("a&b")));
        part.extend(rec(BRT_EXTERN_ROW_HDR, &1u32.to_le_bytes()));
        part.extend(cell(BRT_EXTERN_CELL_BOOL, 0, &[1]));
        part.extend(cell(BRT_EXTERN_CELL_ERROR, 1, &[0x2A]));
        part.extend(cell(BRT_EXTERN_CELL_BLANK, 2, &[]));
        part.extend(cell(BRT_EXTERN_CELL_REAL, 20_000, &1.0f64.to_le_bytes()));
        part.extend(cell(BRT_EXTERN_CELL_REAL, 3, &[0, 0]));
        part.extend(rec(BRT_EXTERN_TABLE_END, &[]));
        // A cell outside any table, then a sheet the book doesn't list.
        part.extend(cell(BRT_EXTERN_CELL_REAL, 0, &9.0f64.to_le_bytes()));
        part.extend(rec(BRT_EXTERN_TABLE_START, &[5, 0, 0, 0, 0]));
        part.extend(rec(BRT_EXTERN_ROW_HDR, &0u32.to_le_bytes()));
        part.extend(cell(BRT_EXTERN_CELL_REAL, 0, &9.0f64.to_le_bytes()));
        part.extend(rec(BRT_EXTERN_TABLE_END, &[]));
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
            [
                Some("[1]!F"),
                Some("[1]!Prices"),
                Some("[1]!Half"),
                Some("[1]!Missing"),
                Some("[1]!Cut"),
                Some("[1]!G"),
                Some("[2]!Prices")
            ]
        );
        for col in 0..7 {
            assert_eq!(c[&(0, col)].value, CellValue::Number(col as f64 + 1.0));
        }
        assert_eq!(book.external_links.len(), 2);
        let pkg = book.build();
        let link = |k: usize| {
            String::from_utf8(
                pkg.part(&format!("xl/externalLinks/externalLink{k}.xml"))
                    .unwrap()
                    .to_vec(),
            )
            .unwrap()
        };
        assert!(
            link(1).contains(concat!(
                r#"<definedNames><definedName name="F"/>"#,
                r#"<definedName name="Prices" refersTo="='P'!$A$1:$A$2"/>"#,
                r##"<definedName name="Half" refersTo="#REF!"/>"##,
                r#"<definedName name="Missing"/><definedName name="Cut"/>"#,
                r#"<definedName name="G"/><definedName name="Int"/></definedNames>"#,
                r#"<sheetDataSet><sheetData sheetId="0">"#,
                r#"<row r="1"><cell r="A1"><v>3</v></cell><cell r="B1" t="str"><v>a&amp;b</v></cell></row>"#,
                r##"<row r="2"><cell r="A2" t="b"><v>1</v></cell><cell r="B2" t="e"><v>#N/A</v></cell></row>"##,
                r#"</sheetData></sheetDataSet></externalBook>"#,
            )),
            "{}",
            link(1)
        );
        assert!(
            link(2).contains(r#"<definedNames><definedName name="Prices" refersTo="='P'!$A$1:$A$2"/></definedNames></externalBook>"#),
            "{}",
            link(2)
        );
    }

    /// References into a linked book's sheets (#903), as in the `.xls`:
    /// ptgRef3d and ptgArea3d of each class through an XTI naming one of
    /// its sheets, spelled as Excel's `.xlsx` spells them, and
    /// ptgRefErr3d/ptgAreaErr3d keeping the sheet. A span over its sheets,
    /// a sheet it doesn't list, a book whose part isn't read and one that
    /// isn't a workbook's (`sbt` 1, DDE) keep only their values. A defined
    /// name referring to it is kept too.
    #[test]
    fn references_into_a_linked_book_keep_the_book_and_sheet() {
        let src = |rid: &str| rec(BRT_SUP_BOOK_SRC, &wide(rid));
        let ref3d = |ptg: u8, ixti: u16, row: u32, col: u16| {
            let mut f = vec![ptg];
            f.extend(ixti.to_le_bytes());
            f.extend(row.to_le_bytes());
            f.extend(col.to_le_bytes());
            f
        };
        let mut area = vec![0x5B, 0, 0];
        area.extend(0u32.to_le_bytes());
        area.extend(1u32.to_le_bytes());
        area.extend([0, 0, 1, 0]);
        let mut ref_err = vec![0x3C, 0, 0];
        ref_err.extend([0; 6]);
        let mut area_err = vec![0x3D, 1, 0];
        area_err.extend([0; 12]);
        let my_ref = ref3d(0x3A, 0, 0, 0);
        let mut name = 0u32.to_le_bytes().to_vec();
        name.push(0);
        name.extend(0xFFFF_FFFFu32.to_le_bytes());
        name.extend(wide("MyRef"));
        name.extend((my_ref.len() as u32).to_le_bytes());
        name.extend(&my_ref);
        name.extend(0u32.to_le_bytes());
        let book = open(&xlsb_links(
            &[
                src("rId2"),
                src("rId9"),
                src("rId3"),
                rec(BRT_SUP_SELF, &[]),
                extern_sheet(&[
                    (0, 0, 0),
                    (0, 1, 1),
                    (0, 0, 1),
                    (0, -1, -1),
                    (0, 2, 2),
                    (1, 0, 0),
                    (2, 0, 0),
                ]),
                rec(BRT_NAME, &name),
            ],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                fmla_num(0, 0.0, &ref3d(0x3A, 0, 0, 0xC000)),
                fmla_num(1, 1.0, &area),
                fmla_num(2, 2.0, &ref3d(0x7A, 1, 0, 0)),
                fmla_num(3, 3.0, &ref_err),
                fmla_num(4, 4.0, &area_err),
                fmla_num(5, 5.0, &ref3d(0x3A, 2, 0, 0)),
                fmla_num(6, 6.0, &ref3d(0x3A, 3, 0, 0)),
                fmla_num(7, 7.0, &ref3d(0x3A, 4, 0, 0)),
                fmla_num(8, 8.0, &ref3d(0x3A, 5, 0, 0)),
                fmla_num(9, 9.0, &ref3d(0x3A, 6, 0, 0)),
            ],
            &[
                (
                    "rId2",
                    link_part(0, "rId1", &["P", "My Sheet"], &[]),
                    LINK_RELS,
                ),
                ("rId3", link_part(1, "rId1", &["P"], &[]), LINK_RELS),
            ],
        ));
        let c = &book.sheets[0].cells;
        let got: Vec<Option<&str>> = (0..10).map(|col| c[&(0, col)].formula.as_deref()).collect();
        assert_eq!(
            got,
            [
                Some("[1]P!A1"),
                Some("[1]P!$A$1:$B$2"),
                Some("'[1]My Sheet'!$A$1"),
                Some("[1]P!#REF!"),
                Some("'[1]My Sheet'!#REF!"),
                None,
                None,
                None,
                None,
                None
            ]
        );
        for col in 0..10 {
            assert_eq!(c[&(0, col)].value, CellValue::Number(col as f64));
        }
        assert_eq!(book.names[0].formula, "[1]P!$A$1");
        assert_eq!(book.external_links.len(), 1);
    }

    /// A link part's cached cells count against the cell budget as they
    /// are read (#903 r1): past it the workbook is refused; cells of a
    /// sheet the book doesn't list are neither kept nor charged, and a cell
    /// cached twice is charged once.
    #[test]
    fn a_link_parts_cache_is_budgeted() {
        let part = |itab: u8| {
            let mut v = link_part(0, "rId1", &["P"], &["F"]);
            v.extend(rec(BRT_EXTERN_TABLE_START, &[itab, 0, 0, 0, 0]));
            v.extend(rec(BRT_EXTERN_ROW_HDR, &0u32.to_le_bytes()));
            for col in [0u32, 1, 2, 2] {
                let mut b = col.to_le_bytes().to_vec();
                b.extend(1.0f64.to_le_bytes());
                v.extend(rec(BRT_EXTERN_CELL_REAL, &b));
            }
            v.extend(rec(BRT_EXTERN_TABLE_END, &[]));
            v
        };
        let file = |itab: u8| {
            xlsb_links(
                &[rec(BRT_SUP_BOOK_SRC, &wide("rId2"))],
                &[],
                &[("rId2", part(itab), LINK_RELS)],
            )
        };
        let limits = |cells| Limits {
            cells,
            ..Limits::default()
        };
        let read =
            |itab: u8, cells| read_with(&ZipArchive::open(&file(itab)).unwrap(), limits(cells));
        let err = read(0, 2).err().unwrap();
        assert!(err.to_string().contains("too many cells"), "{err}");
        assert_eq!(read(0, 3).unwrap().external_links[0].cache.len(), 3);
        assert!(read(5, 0).unwrap().external_links[0].cache.is_empty());
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

    // --- #884: cell formats, column widths and merges -------------------

    use crate::sheet::Xf;

    use super::super::xlsb_style::{
        BRT_BEGIN_BORDERS, BRT_BEGIN_CELL_XFS, BRT_BEGIN_FILLS, BRT_BEGIN_FONTS, BRT_BORDER,
        BRT_END_BORDERS, BRT_END_CELL_XFS, BRT_END_FILLS, BRT_END_FONTS, BRT_FILL, BRT_FMT,
        BRT_FONT, BRT_XF,
    };

    /// A BrtColor: automatic, RGB, indexed or theme (fValidRGB set).
    pub(crate) fn auto_color() -> Vec<u8> {
        vec![0x01, 0, 0, 0, 0, 0, 0, 0xFF]
    }
    pub(crate) fn rgb_color(r: u8, g: u8, b: u8) -> Vec<u8> {
        vec![(2 << 1) | 1, 0, 0, 0, r, g, b, 0xFF]
    }
    pub(crate) fn indexed_color(i: u8) -> Vec<u8> {
        vec![(1 << 1) | 1, i, 0, 0, 0, 0, 0, 0xFF]
    }
    pub(crate) fn theme_color(i: u8) -> Vec<u8> {
        vec![(3 << 1) | 1, i, 0, 0, 0, 0, 0, 0xFF]
    }

    /// BrtFont as Excel writes it: dyHeight, grbit, bls, sss, uls,
    /// bFamily, bCharSet, unused, brtColor, bFontScheme, name.
    fn font(pt: u16, italic: bool, bold: bool, color: &[u8], name: &str) -> Vec<u8> {
        let mut v = (pt * 20).to_le_bytes().to_vec();
        v.extend((if italic { 2u16 } else { 0 }).to_le_bytes());
        v.extend((if bold { 700u16 } else { 400 }).to_le_bytes());
        v.extend([0, 0, 0, 2, 1, 0]);
        v.extend_from_slice(color);
        v.push(0);
        v.extend(wide(name));
        rec(BRT_FONT, &v)
    }

    /// BrtFill: fls, the foreground and background colours, then the
    /// gradient fields (68 bytes, as Excel writes them).
    fn fill(fls: u32, fore: &[u8]) -> Vec<u8> {
        let mut v = fls.to_le_bytes().to_vec();
        v.extend_from_slice(fore);
        v.extend(indexed_color(65));
        v.extend([0u8; 48]);
        rec(BRT_FILL, &v)
    }

    /// BrtBorder: flags, then the top, bottom, left, right and diagonal
    /// lines' dg, reserved and colour.
    fn border(dg: [u8; 5]) -> Vec<u8> {
        let mut v = vec![0u8];
        for d in dg {
            v.extend([d, 0]);
            v.extend(auto_color());
        }
        rec(BRT_BORDER, &v)
    }

    /// BrtXF: ixfeParent, iFmt, iFont, iFill, ixBorder, trot, indent,
    /// flags, xfGrbitAtr, unused.
    fn xf(fmt: u16, font: u16, fill: u16, border: u16) -> Vec<u8> {
        let mut v = 0u16.to_le_bytes().to_vec();
        for n in [fmt, font, fill, border] {
            v.extend(n.to_le_bytes());
        }
        v.extend([0, 0, 0x10, 0x10, 0, 0]);
        rec(BRT_XF, &v)
    }

    /// A list of records between its begin and end records.
    fn list(begin: u32, end: u32, items: &[Vec<u8>]) -> Vec<u8> {
        let mut v = rec(begin, &(items.len() as u32).to_le_bytes());
        for i in items {
            v.extend_from_slice(i);
        }
        v.extend(rec(end, &[]));
        v
    }

    /// A styles.bin: Excel's default font 0 (Calibri 11, theme 1) and
    /// fills 0 and 1, then the fonts, fills, borders and cell XFs given.
    fn styles(
        fonts: &[Vec<u8>],
        fills: &[Vec<u8>],
        borders: &[Vec<u8>],
        xfs: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut all_fonts = vec![font(11, false, false, &theme_color(1), "Calibri")];
        all_fonts.extend_from_slice(fonts);
        let mut all_fills = vec![fill(0, &indexed_color(64)), fill(17, &indexed_color(64))];
        all_fills.extend_from_slice(fills);
        let mut all_borders = vec![border([0; 5])];
        all_borders.extend_from_slice(borders);
        let mut all_xfs = vec![xf(0, 0, 0, 0)];
        all_xfs.extend_from_slice(xfs);
        let mut fmt = 164u16.to_le_bytes().to_vec();
        fmt.extend(wide("0.000"));
        [
            rec(BRT_FMT, &fmt),
            list(BRT_BEGIN_FONTS, BRT_END_FONTS, &all_fonts),
            list(BRT_BEGIN_FILLS, BRT_END_FILLS, &all_fills),
            list(BRT_BEGIN_BORDERS, BRT_END_BORDERS, &all_borders),
            list(BRT_BEGIN_CELL_XFS, BRT_END_CELL_XFS, &all_xfs),
        ]
        .concat()
    }

    fn real(col: u32, style: u32, v: f64) -> Vec<u8> {
        let mut b = cell(col, style);
        b.extend(v.to_le_bytes());
        rec(5, &b)
    }

    /// The model formats of the cells of row 0, by column.
    fn row0_xfs(book: &BookIn) -> Vec<Xf> {
        let cells = &book.sheets[0].cells;
        (0..cells.len() as u32)
            .map(|c| book.xfs[cells[&(0, c)].style as usize].clone())
            .collect()
    }

    /// A BrtXF's font, fill and border become the cell's model format; a
    /// patterned fill, a medium box and the default font are none of it.
    #[test]
    fn xlsb_fonts_fills_borders_into_xfs() {
        let styles = styles(
            &[
                // 1: bold italic red 14pt Arial
                font(14, true, true, &rgb_color(255, 0, 0), "Arial"),
                // 2: Calibri 11 in theme colour 4 (accent 1)
                font(11, false, false, &theme_color(4), "Calibri"),
                // 3: Calibri 11 in black, as an RGB and indexed 8
                font(11, false, false, &indexed_color(8), "Calibri"),
            ],
            &[
                // 2: solid yellow; 3: a pattern in yellow; 4: solid theme
                fill(1, &rgb_color(255, 255, 0)),
                fill(2, &rgb_color(255, 255, 0)),
                fill(1, &theme_color(5)),
            ],
            &[
                // 1: a thin box; 2: a medium box; 3: three thin sides
                border([1, 1, 1, 1, 0]),
                border([2, 2, 2, 2, 0]),
                border([1, 1, 1, 0, 0]),
            ],
            &[
                xf(164, 1, 2, 1),
                xf(0, 0, 3, 2),
                xf(0, 2, 4, 3),
                xf(0, 3, 0, 0),
            ],
        );
        let book = open(&xlsb_with(
            &[],
            &[
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                real(0, 1, 1.0),
                real(1, 2, 2.0),
                real(2, 3, 3.0),
                real(3, 4, 4.0),
                real(4, 0, 5.0),
                // A blank cell keeps its format too.
                rec(1, &cell(5, 1)),
            ],
            &[],
            vec![("xl/styles.bin".to_string(), styles)],
        ));
        let x = row0_xfs(&book);
        assert!(x[0].bold && x[0].italic, "{:?}", x[0]);
        assert_eq!(x[0].color, Some((255, 0, 0)));
        assert_eq!(x[0].font_size, Some(14.0));
        assert_eq!(x[0].font_name.as_deref(), Some("Arial"));
        assert_eq!(x[0].fill, Some((255, 255, 0)));
        assert!(x[0].border);
        assert_eq!(x[0].code.as_deref(), Some("0.000"));
        // A pattern isn't a solid fill; a medium box isn't a thin one.
        assert_eq!(
            (x[1].fill, x[1].fill_unresolved, x[1].border),
            (None, false, false)
        );
        // Theme colours other than the default text colour are unresolved.
        assert!(x[2].color_unresolved && x[2].fill_unresolved, "{:?}", x[2]);
        assert_eq!((x[2].color, x[2].fill, x[2].border), (None, None, false));
        // Black, the default font and the default XF are no format at all.
        assert_eq!(x[3], Xf::default());
        assert_eq!(x[4], Xf::default());
        assert_eq!(book.sheets[0].cells[&(0, 4)].style, 0);
        assert_eq!(x[5], x[0]);
        // Each XF is made once.
        assert_eq!(book.xfs.len(), 3);
    }

    /// BrtColInfo gives the column widths (a width at the default without
    /// fUserSet is none), BrtWsFmtInfo the base width, BrtMergeCell merges.
    #[test]
    fn xlsb_col_widths_and_merges() {
        let col_info = |first: u32, last: u32, coldx: u32, flags: u16| {
            let mut v = first.to_le_bytes().to_vec();
            v.extend(last.to_le_bytes());
            v.extend(coldx.to_le_bytes());
            v.extend(0u32.to_le_bytes());
            v.extend(flags.to_le_bytes());
            rec(BRT_COL_INFO, &v)
        };
        let mut fmt_info = 2194u32.to_le_bytes().to_vec();
        fmt_info.extend(8u16.to_le_bytes());
        fmt_info.extend(300u16.to_le_bytes());
        fmt_info.extend([0, 0, 0, 0]);
        let merge = |r1: u32, r2: u32, c1: u32, c2: u32| {
            let v: Vec<u8> = [r1, r2, c1, c2]
                .iter()
                .flat_map(|n| n.to_le_bytes())
                .collect();
            rec(BRT_MERGE_CELL, &v)
        };
        let book = open(&xlsb(
            &[],
            &[
                rec(BRT_WS_FMT_INFO, &fmt_info),
                // B:C 20 characters, user set
                col_info(1, 2, 20 * 256, 0x0002),
                // D hidden, at the default width
                col_info(3, 3, 2194, 0x0001),
                // E at the default width: nothing
                col_info(4, 4, 2194, 0),
                // F wide but not user set: a plain width; outline level 1
                col_info(5, 5, 12 * 256, 0x0100),
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                real(0, 0, 1.0),
                merge(5, 6, 0, 2),
                // Off the grid, or backwards: left out.
                merge(0, 0, 3, 16384),
                merge(4, 2, 0, 0),
            ],
        ));
        let s = &book.sheets[0];
        assert_eq!(s.base_col_width, None);
        assert_eq!(s.default_col_width, None);
        let cols: Vec<_> = s
            .cols
            .iter()
            .map(|c| (c.first, c.last, c.width, c.hidden, c.plain, c.level))
            .collect();
        assert_eq!(
            cols,
            [
                (1, 2, Some(20.0), false, false, 0),
                (3, 3, None, true, false, 0),
                (5, 5, Some(12.0), false, true, 1),
            ]
        );
        assert_eq!(s.merges, [(5, 0, 6, 2)]);
    }

    /// The `.xlsb` of [`round_trip_xlsb`]: a formatted cell, a wide
    /// column and a merge (later slices add to it).
    fn feature_xlsb() -> Vec<u8> {
        let mut styles = styles(
            &[font(14, true, true, &rgb_color(255, 0, 0), "Arial")],
            &[fill(1, &rgb_color(255, 255, 0))],
            &[border([1, 1, 1, 1, 0])],
            &[xf(0, 1, 2, 1)],
        );
        styles.extend(dxf_list());
        let mut col = 1u32.to_le_bytes().to_vec();
        col.extend(1u32.to_le_bytes());
        col.extend((20 * 256u32).to_le_bytes());
        col.extend(0u32.to_le_bytes());
        col.extend(2u16.to_le_bytes());
        let merge: Vec<u8> = [2u32, 3, 0, 1]
            .iter()
            .flat_map(|n| n.to_le_bytes())
            .collect();
        xlsb_with(
            &[],
            &[
                rec(BRT_COL_INFO, &col),
                rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
                real(0, 1, 1.0),
                rec(BRT_MERGE_CELL, &merge),
            ]
            .into_iter()
            .chain(dv_records())
            .chain([rec(BRT_ROW_HDR, &1u32.to_le_bytes()), real(0, 0, 9.0)])
            .chain(cf_records())
            .collect::<Vec<_>>(),
            &[],
            [
                vec![("xl/styles.bin".to_string(), styles)],
                comments_parts(&["Ann"], &[(0, 1, 0, "a note")]),
            ]
            .concat(),
        )
    }

    /// Opened, saved as `.xlsx` and reloaded, an `.xlsb` keeps what the
    /// import read (#884: they were lost on save).
    #[test]
    fn round_trip_xlsb() {
        let (pkg, _) = crate::legacy::open_workbook(&feature_xlsb()).unwrap();
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let wb = &back.workbook;
        let s = &wb.sheets[0];
        let style = s.cell(0, 0).unwrap().style;
        let x = wb.styles.xf(style);
        assert!(x.bold && x.italic, "{x:?}");
        assert!(crate::legacy::saved_thin_box(&back, style));
        assert_eq!(x.color, Some((255, 0, 0)));
        assert_eq!(x.fill, Some((255, 255, 0)));
        assert_eq!(x.font_size, Some(14.0));
        assert_eq!(x.font_name.as_deref(), Some("Arial"));
        let widths: Vec<_> = s.col_defs.iter().map(|d| (d.min, d.max, d.width)).collect();
        assert_eq!(widths, [(1, 1, Some(20.0))]);
        assert_eq!(s.merges, [(2, 0, 3, 1)]);
        let notes: Vec<_> = back
            .comments()
            .into_iter()
            .map(|c| (c.row, c.col, c.author, c.text))
            .collect();
        assert_eq!(notes, [(0, 1, "Ann".to_string(), "a note".to_string())]);
        let got: Vec<_> = s
            .validations
            .iter()
            .map(|d| (d.kind.as_str(), d.operator.as_str(), d.formula1.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("whole", "greaterThan", "5"),
                ("list", "", "\"a,b,c\""),
                ("custom", "", "A3>0"),
                ("custom", "", "A3>0"),
            ]
        );
        // The rules apply: A2's 9 makes B2 italic (`$A2>5`).
        assert_eq!(wb.sheets[0].cond_formats.len(), 2);
        assert_eq!(crate::cf::cell_dxf(wb, 0, 1, 1).unwrap().italic, Some(true));
        assert!(crate::cf::cell_dxf(wb, 0, 2, 1).is_none());
    }

    /// The sheet rels and comments part of a sheet with these comments:
    /// (row, col, author index, text), and the author list.
    fn comments_parts(
        authors: &[&str],
        comments: &[(u32, u32, u32, &str)],
    ) -> Vec<(String, Vec<u8>)> {
        let mut v = Vec::new();
        for a in authors {
            v.extend(rec(BRT_COMMENT_AUTHOR, &wide(a)));
        }
        for &(row, col, iauth, text) in comments {
            let mut b = iauth.to_le_bytes().to_vec();
            for n in [row, row, col, col] {
                b.extend(n.to_le_bytes());
            }
            b.extend([0; 16]);
            v.extend(rec(BRT_BEGIN_COMMENT, &b));
            let mut t = vec![0u8];
            t.extend(wide(text));
            v.extend(rec(BRT_COMMENT_TEXT, &t));
            v.extend(rec(636, &[]));
        }
        vec![
            (
                "xl/worksheets/_rels/sheet1.bin.rels".to_string(),
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.bin"/></Relationships>"#.to_vec(),
            ),
            ("xl/comments1.bin".to_string(), v),
        ]
    }

    /// The comments part's comments: their cell, author and text.
    #[test]
    fn xlsb_comments() {
        let book = open(&xlsb_with(
            &[],
            &[rec(BRT_ROW_HDR, &0u32.to_le_bytes()), real(0, 0, 1.0)],
            &[],
            comments_parts(
                &["Ann", "Bob"],
                &[
                    (3, 1, 1, "Bob:\nsee this"),
                    (0, 0, 0, "first"),
                    (0, 16384, 0, "off"),
                ],
            ),
        ));
        assert_eq!(
            book.sheets[0].comments,
            [
                (3, 1, "Bob".to_string(), "Bob:\nsee this".to_string()),
                (0, 0, "Ann".to_string(), "first".to_string()),
            ]
        );
    }

    /// BrtDVal: flags, sqrfx, the error title, error, prompt title and
    /// prompt, formula1 and formula2.
    fn dval(
        flags: u32,
        ranges: &[(u32, u32, u32, u32)],
        texts: [&str; 4],
        f1: &[u8],
        f2: &[u8],
    ) -> Vec<u8> {
        let mut b = flags.to_le_bytes().to_vec();
        b.extend((ranges.len() as u32).to_le_bytes());
        for &(r1, r2, c1, c2) in ranges {
            for n in [r1, r2, c1, c2] {
                b.extend(n.to_le_bytes());
            }
        }
        for t in texts {
            b.extend(wide(t));
        }
        for f in [f1, f2] {
            b.extend((f.len() as u32).to_le_bytes());
            b.extend_from_slice(f);
            b.extend(0u32.to_le_bytes());
        }
        rec(BRT_DVAL, &b)
    }

    fn dv_records() -> Vec<Vec<u8>> {
        // =A3>0 on C3:C4: ptgRefN (0 rows, -2 columns) and ptgRef (A3).
        let ref_n = [0x2C, 0, 0, 0, 0, 0xFE, 0xFF, 0x1E, 0, 0, 0x0D];
        let ref_a = [0x24, 2, 0, 0, 0, 0, 0xC0, 0x1E, 0, 0, 0x0D];
        let mut list = vec![0x17, 5, 0];
        for u in "a\0b\0c".encode_utf16() {
            list.extend(u.to_le_bytes());
        }
        vec![
            rec(573, &[0; 18]),
            dval(
                1 | (2 << 4) | (1 << 8) | (1 << 18) | (1 << 19) | (4 << 20),
                &[(1, 4, 1, 1)],
                ["Oops", "Too small", "Number", "More than 5"],
                &[0x1E, 5, 0],
                &[],
            ),
            dval(3 | (1 << 9), &[(0, 2, 0, 0)], ["", "", "", ""], &list, &[]),
            dval(7, &[(2, 3, 2, 2)], ["", "", "", ""], &ref_n, &[]),
            dval(7, &[(2, 3, 4, 4)], ["", "", "", ""], &ref_a, &[]),
            rec(574, &[]),
        ]
    }

    /// BrtDVal records are the sheet's validation rules.
    #[test]
    fn xlsb_dv_rules() {
        use crate::sheet::AlertStyle;
        let mut sheet = vec![rec(BRT_ROW_HDR, &0u32.to_le_bytes()), real(0, 0, 1.0)];
        sheet.extend(dv_records());
        let book = open(&xlsb(&[], &sheet));
        let v = &super::super::placed_dvs(&book.sheets[0]);
        assert_eq!(v.len(), 4);
        assert_eq!(
            (v[0].kind.as_str(), v[0].operator.as_str()),
            ("whole", "greaterThan")
        );
        assert_eq!((v[0].formula1.as_str(), v[0].formula2.as_str()), ("5", ""));
        assert_eq!(v[0].ranges, [(1, 1, 4, 1)]);
        assert_eq!(
            (v[0].error_title.as_str(), v[0].error.as_str()),
            ("Oops", "Too small")
        );
        assert_eq!(
            (v[0].prompt_title.as_str(), v[0].prompt.as_deref()),
            ("Number", Some("More than 5"))
        );
        assert_eq!(v[0].error_style, AlertStyle::Information);
        assert!(v[0].allow_blank && v[0].show_input && v[0].show_error);
        assert_eq!(
            (v[1].kind.as_str(), v[1].formula1.as_str()),
            ("list", "\"a,b,c\"")
        );
        assert!(!v[1].show_dropdown && v[1].prompt.is_none());
        assert_eq!(
            (v[2].kind.as_str(), v[2].formula1.as_str()),
            ("custom", "A3>0")
        );
        assert_eq!(v[3].formula1, "A3>0");
    }

    /// BrtDXF: flags, reserved, cprops, then each XFProp (type, cb, value).
    fn dxf_rec(props: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend((props.len() as u16).to_le_bytes());
        for (ty, v) in props {
            b.extend(ty.to_le_bytes());
            b.extend(((v.len() + 4) as u16).to_le_bytes());
            b.extend_from_slice(v);
        }
        rec(super::super::xlsb_style::BRT_DXF, &b)
    }

    /// A styles.bin with two differential formats: 0 bold red text on a
    /// solid yellow fill (its background colour); 1 italic on a fill
    /// pattern of none.
    fn dxf_styles() -> Vec<u8> {
        let mut v = styles(&[], &[], &[], &[]);
        v.extend(dxf_list());
        v
    }

    /// The BrtDXF list of [`dxf_styles`].
    fn dxf_list() -> Vec<u8> {
        use super::super::xlsb_style::{BRT_BEGIN_DXFS, BRT_END_DXFS};
        list(
            BRT_BEGIN_DXFS,
            BRT_END_DXFS,
            &[
                dxf_rec(&[
                    (0x00, vec![1]),
                    (0x02, rgb_color(255, 255, 0)),
                    (0x05, rgb_color(255, 0, 0)),
                    (0x19, 700u16.to_le_bytes().to_vec()),
                ]),
                dxf_rec(&[
                    (0x00, vec![0]),
                    (0x02, rgb_color(0, 0, 255)),
                    (0x1C, vec![1]),
                ]),
            ],
        )
    }

    /// BrtBeginCFRule: iType, iTemplate, dxfId, iPri, iParam, reserved,
    /// flags, cbFmla1..3, strParam, the formulas.
    fn cf_rule(ty: u32, dxf: u32, param: u32, formulas: &[&[u8]]) -> Vec<u8> {
        let mut b = Vec::new();
        for n in [ty, 0, dxf, 1, param, 0, 0] {
            b.extend(n.to_le_bytes());
        }
        b.extend(0u16.to_le_bytes());
        let parsed: Vec<Vec<u8>> = formulas
            .iter()
            .map(|f| {
                let mut v = (f.len() as u32).to_le_bytes().to_vec();
                v.extend_from_slice(f);
                v.extend(0u32.to_le_bytes());
                v
            })
            .collect();
        for i in 0..3 {
            b.extend((parsed.get(i).map_or(0, Vec::len) as u32).to_le_bytes());
        }
        b.extend(0xFFFF_FFFFu32.to_le_bytes());
        for p in &parsed {
            b.extend_from_slice(p);
        }
        rec(BRT_BEGIN_CF_RULE, &b)
    }

    /// The conditional formats of [`xlsb_cf_rules_and_dxfs`]: on B2:B5,
    /// value > 5 (dxf 0), `$A2>5` (dxf 1, ptgRefN based at B2) and a
    /// color scale the model doesn't hold; on D2:D3 `$A2>5` as ptgRef.
    fn cf_records() -> Vec<Vec<u8>> {
        let block = |ranges: &[(u32, u32, u32, u32)]| {
            let mut b = vec![0u8; 8];
            b.extend((ranges.len() as u32).to_le_bytes());
            for &(r1, r2, c1, c2) in ranges {
                for n in [r1, r2, c1, c2] {
                    b.extend(n.to_le_bytes());
                }
            }
            rec(BRT_BEGIN_CONDITIONAL_FORMATTING, &b)
        };
        let ref_n: &[u8] = &[0x2C, 0, 0, 0, 0, 0, 0x80, 0x1E, 5, 0, 0x0D];
        let ref_a: &[u8] = &[0x24, 1, 0, 0, 0, 0, 0x80, 0x1E, 5, 0, 0x0D];
        vec![
            block(&[(1, 4, 1, 1)]),
            cf_rule(1, 0, 5, &[&[0x1E, 5, 0]]),
            rec(464, &[]),
            cf_rule(2, 1, 0, &[ref_n]),
            rec(464, &[]),
            cf_rule(3, 0, 0, &[]),
            rec(464, &[]),
            rec(462, &[]),
            block(&[(1, 2, 3, 3)]),
            cf_rule(2, 1, 0, &[ref_a]),
            rec(464, &[]),
            rec(462, &[]),
        ]
    }

    /// BrtBeginConditionalFormatting and BrtBeginCFRule records are the
    /// sheet's conditional formats, with the BrtDXF their dxfId names.
    #[test]
    fn xlsb_cf_rules_and_dxfs() {
        use crate::sheet::Dxf;
        use crate::xlsx::NewCfRule;
        let mut sheet = vec![rec(BRT_ROW_HDR, &0u32.to_le_bytes()), real(0, 0, 1.0)];
        sheet.extend(cf_records());
        let book = open(&xlsb_with(
            &[],
            &sheet,
            &[],
            vec![("xl/styles.bin".to_string(), dxf_styles())],
        ));
        let cf = &book.sheets[0].cond_formats;
        assert_eq!(cf.len(), 2);
        assert_eq!(cf[0].ranges, [(1, 1, 4, 1)]);
        let expr = NewCfRule::Expression {
            formula: "$A2>5".into(),
        };
        let italic = Dxf {
            italic: Some(true),
            ..Dxf::default()
        };
        assert_eq!(
            cf[0].rules,
            [
                (
                    NewCfRule::CellIs {
                        op: "greaterThan".into(),
                        formulas: vec!["5".into()],
                    },
                    Dxf {
                        bold: Some(true),
                        color: Some((255, 0, 0)),
                        fill: Some((255, 255, 0)),
                        ..Dxf::default()
                    }
                ),
                (expr.clone(), italic.clone()),
            ]
        );
        assert_eq!(cf[1].rules, [(expr, italic)]);
    }

    /// A rule after a block that wasn't kept (its ranges off the grid, or
    /// cut short), or after a block's end, joins no other block (#884 r1).
    #[test]
    fn xlsb_cf_rules_join_only_their_block() {
        let block = |r: u32, c: u32| {
            let mut b = vec![0u8; 8];
            b.extend(1u32.to_le_bytes());
            for n in [r, r, c, c] {
                b.extend(n.to_le_bytes());
            }
            rec(BRT_BEGIN_CONDITIONAL_FORMATTING, &b)
        };
        let gt = cf_rule(1, 0, 5, &[&[0x1E, 5, 0]]);
        let sheet = vec![
            rec(BRT_ROW_HDR, &0u32.to_le_bytes()),
            real(0, 0, 1.0),
            block(0, 0),
            gt.clone(),
            rec(462, &[]),
            // After the end: no block.
            gt.clone(),
            // Off the grid, then cut short.
            block(0, 16384),
            gt.clone(),
            rec(BRT_BEGIN_CONDITIONAL_FORMATTING, &[0; 4]),
            gt,
        ];
        let book = open(&xlsb_with(
            &[],
            &sheet,
            &[],
            vec![("xl/styles.bin".to_string(), dxf_styles())],
        ));
        let cf = &book.sheets[0].cond_formats;
        assert_eq!(cf.len(), 1);
        assert_eq!(cf[0].rules.len(), 1);
    }

    /// As `.xls`'s: a DV based at its first range and a CF based at its
    /// ranges' bounds both land at the least corner (#884 r2).
    #[test]
    fn xlsb_rules_are_anchored_at_their_least_corner() {
        // ptgRefN (0, 0), relative both ways; ptgInt 0; ptgGt.
        let here: &[u8] = &[0x2C, 0, 0, 0, 0, 0, 0xC0, 0x1E, 0, 0, 0x0D];
        let ranges: [(u32, u32, u32, u32); 2] = [(4, 9, 3, 3), (1, 2, 1, 1)];
        let mut block = vec![0u8; 8];
        block.extend(2u32.to_le_bytes());
        for &(r1, r2, c1, c2) in &ranges {
            for n in [r1, r2, c1, c2] {
                block.extend(n.to_le_bytes());
            }
        }
        let sheet = vec![
            rec(BRT_ROW_HDR, &1u32.to_le_bytes()),
            real(1, 0, -1.0),
            rec(BRT_ROW_HDR, &4u32.to_le_bytes()),
            real(3, 0, 5.0),
            dval(7, &ranges, ["", "", "", ""], here, &[]),
            rec(BRT_BEGIN_CONDITIONAL_FORMATTING, &block),
            cf_rule(2, 1, 0, &[here]),
            rec(464, &[]),
            rec(462, &[]),
        ];
        let bytes = xlsb_with(
            &[],
            &sheet,
            &[],
            vec![("xl/styles.bin".to_string(), dxf_styles())],
        );
        let (pkg, _) = crate::legacy::open_workbook(&bytes).unwrap();
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        let wb = &back.workbook;
        assert_eq!(wb.sheets[0].validations[0].formula1, "B2>0");
        assert_eq!(crate::validation::invalid_cells(wb, 0, None), [(1, 1)]);
        assert!(crate::cf::cell_dxf(wb, 0, 4, 3).is_some());
        assert!(crate::cf::cell_dxf(wb, 0, 1, 1).is_none());
    }
}
