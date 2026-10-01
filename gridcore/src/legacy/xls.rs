//! Excel 97-2003 `.xls`: the BIFF8 record stream in the compound file's
//! `Workbook` stream ([MS-XLS]).
//!
//! The globals substream gives the sheets (BOUNDSHEET8), shared strings
//! (SST), number formats (FORMAT, XF), the date system (DATEMODE), the
//! reference tables (SUPBOOK, EXTERNSHEET, EXTERNNAME) and defined names
//! (NAME). Each worksheet substream gives its cells. Records not listed here
//! are skipped.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::ptg::{self, Base, Biff, Names};
use super::{
    BookIn, Limits, OpenError, SheetIn, biff_error, builtin_format, on_grid, rk, set_array,
    sheet_prefix,
};
use crate::sheet::{Cell, CellValue, DefinedName};

/// One record, with any CONTINUE records that follow it appended to `data`.
/// `breaks` holds the offsets in `data` where each CONTINUE began, which a
/// string split across them needs (each piece re-sends its high-byte flag).
struct Rec {
    ty: u16,
    data: Vec<u8>,
    breaks: Vec<usize>,
    /// Stream offset of the record header (BOUNDSHEET8 points at these).
    pos: usize,
}

const CONTINUE: u16 = 0x003C;

/// The stream's records. A record whose length runs past the end of the
/// stream ends the list: a truncated file loses its tail, not the rest.
fn records(s: &[u8]) -> Vec<Rec> {
    let mut out: Vec<Rec> = Vec::new();
    let mut at = 0;
    while at + 4 <= s.len() {
        let ty = u16::from_le_bytes([s[at], s[at + 1]]);
        let len = u16::from_le_bytes([s[at + 2], s[at + 3]]) as usize;
        let Some(body) = s.get(at + 4..at + 4 + len) else {
            break;
        };
        match out.last_mut() {
            Some(prev) if ty == CONTINUE => {
                prev.breaks.push(prev.data.len());
                prev.data.extend_from_slice(body);
            }
            _ => out.push(Rec {
                ty,
                data: body.to_vec(),
                breaks: Vec::new(),
                pos: at,
            }),
        }
        at += 4 + len;
    }
    out
}

/// A little-endian cursor over one record's data.
struct Cur<'a> {
    d: &'a [u8],
    breaks: &'a [usize],
    at: usize,
}

impl<'a> Cur<'a> {
    fn new(r: &'a Rec) -> Cur<'a> {
        Cur {
            d: &r.data,
            breaks: &r.breaks,
            at: 0,
        }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
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
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn rest(&mut self) -> &'a [u8] {
        let s = self.d.get(self.at..).unwrap_or(&[]);
        self.at = self.d.len();
        s
    }

    /// `cch` characters starting with high-byte mode `high`; where the data
    /// crosses into a CONTINUE, that piece's first byte re-sends the flag.
    fn chars(&mut self, cch: usize, mut high: bool) -> Option<String> {
        let mut units: Vec<u16> = Vec::with_capacity(cch.min(1 << 16));
        let mut left = cch;
        while left > 0 {
            // `breaks` is ascending: binary searches, so a string split over
            // many CONTINUEs isn't quadratic.
            if self.breaks.binary_search(&self.at).is_ok() {
                high = self.u8()? & 1 == 1;
            }
            let next = self.breaks.partition_point(|&b| b <= self.at);
            let lim = self.breaks.get(next).copied().unwrap_or(self.d.len());
            let width = if high { 2 } else { 1 };
            let n = ((lim - self.at) / width).min(left);
            if n == 0 {
                return None;
            }
            let raw = self.take(n * width)?;
            if high {
                units.extend(
                    raw.chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]])),
                );
            } else {
                units.extend(raw.iter().map(|&b| b as u16));
            }
            left -= n;
        }
        Some(String::from_utf16_lossy(&units))
    }

    /// `XLUnicodeString`: 16-bit count, flags, characters.
    fn xl_string(&mut self) -> Option<String> {
        let cch = self.u16()? as usize;
        let high = self.u8()? & 1 == 1;
        self.chars(cch, high)
    }

    /// `ShortXLUnicodeString`: 8-bit count, flags, characters.
    fn short_string(&mut self) -> Option<String> {
        let cch = self.u8()? as usize;
        let high = self.u8()? & 1 == 1;
        self.chars(cch, high)
    }

    /// `XLUnicodeRichExtendedString` (an SST entry): the text, with its
    /// formatting runs and phonetic block skipped.
    fn rich_string(&mut self) -> Option<String> {
        let cch = self.u16()? as usize;
        let flags = self.u8()?;
        let runs = if flags & 0x08 != 0 {
            self.u16()? as usize
        } else {
            0
        };
        let ext = if flags & 0x04 != 0 {
            self.u32()? as usize
        } else {
            0
        };
        let s = self.chars(cch, flags & 1 == 1)?;
        self.take(runs * 4 + ext)?;
        Some(s)
    }
}

#[derive(Debug, PartialEq)]
enum Book {
    /// The workbook itself (a SUPBOOK with the 0x0401 marker).
    Own,
    /// An add-in function book (0x3A01): its EXTERNNAMEs are functions.
    AddIn,
    /// Another workbook, or DDE/OLE: references into it don't decompile.
    External,
}

struct SupBook {
    kind: Book,
    names: Vec<String>,
}

/// A NAME record as read, decompiled once every NAME is known.
struct RawName {
    name: String,
    /// Hidden function names (`_xlfn.MAXIFS`) and macro names are no
    /// defined names: they are how a formula calls a function.
    function: bool,
    /// 1-based sheet index of a sheet-scoped name, else 0.
    itab: u16,
    rgce: Vec<u8>,
    extra: Vec<u8>,
}

/// The globals a token stream refers to.
struct Globals {
    /// Every BOUNDSHEET8's name (charts and macro sheets included, since the
    /// XTI table counts them).
    sheets: Vec<String>,
    xti: Vec<(u16, i16, i16)>,
    books: Vec<SupBook>,
    names: Vec<RawName>,
}

impl Names for Globals {
    fn xti(&self, ixti: u32) -> Option<String> {
        let &(book, first, last) = self.xti.get(ixti as usize)?;
        if self.books.get(book as usize)?.kind != Book::Own {
            return None;
        }
        let name = |i: i16| self.sheets.get(usize::try_from(i).ok()?).cloned();
        Some(sheet_prefix(&name(first)?, &name(last)?))
    }

    fn name(&self, index: u32) -> Option<String> {
        Some(self.names.get(index.checked_sub(1)? as usize)?.name.clone())
    }

    fn name_x(&self, ixti: u32, index: u32) -> Option<String> {
        let &(book, _, _) = self.xti.get(ixti as usize)?;
        let book = self.books.get(book as usize)?;
        match book.kind {
            Book::AddIn => book.names.get(index.checked_sub(1)? as usize).cloned(),
            // A name of this workbook; a sheet-scoped one is qualified with
            // the sheet the XTI names (`Sheet2!Rate`).
            Book::Own => {
                let name = self.names.get(index.checked_sub(1)? as usize)?;
                if name.itab == 0 {
                    Some(name.name.clone())
                } else {
                    Some(format!("{}{}", self.xti(ixti)?, name.name))
                }
            }
            Book::External => None,
        }
    }
}

/// The `_xlnm.` names of BIFF8's built-in name codes.
fn builtin_name(code: u8) -> String {
    let n = match code {
        0x00 => "Consolidate_Area",
        0x01 => "Auto_Open",
        0x02 => "Auto_Close",
        0x03 => "Extract",
        0x04 => "Database",
        0x05 => "Criteria",
        0x06 => "Print_Area",
        0x07 => "Print_Titles",
        0x08 => "Recorder",
        0x09 => "Data_Form",
        0x0A => "Auto_Activate",
        0x0B => "Auto_Deactivate",
        0x0C => "Sheet_Title",
        0x0D => "_FilterDatabase",
        _ => return format!("_xlnm.Builtin_{code}"),
    };
    format!("_xlnm.{n}")
}

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;

/// Read a BIFF8 `Workbook` stream.
pub(crate) fn read(stream: &[u8]) -> Result<BookIn, OpenError> {
    read_with(stream, Limits::default())
}

/// [`read`] under `limits`.
pub(crate) fn read_with(stream: &[u8], limits: Limits) -> Result<BookIn, OpenError> {
    let recs = records(stream);
    let first = recs
        .first()
        .ok_or_else(|| OpenError::Corrupt("empty Workbook stream".into()))?;
    // BIFF8's BOF says version 0x0600 and BIFF5/7's 0x0500; anything else
    // (older BOF record ids, garbage) is no workbook this reads.
    let version = (first.ty == BOF)
        .then(|| {
            first
                .data
                .get(..2)
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
        })
        .flatten();
    match version {
        Some(0x0600) => {}
        Some(0x0500) => return Err(OpenError::Biff5),
        _ => {
            return Err(OpenError::Corrupt(
                "the Workbook stream is not a BIFF8 workbook".into(),
            ));
        }
    }

    let mut book = BookIn::with_limits(limits);
    let mut g = Globals {
        sheets: Vec::new(),
        xti: Vec::new(),
        books: Vec::new(),
        names: Vec::new(),
    };
    // (stream offset, dt) of each BOUNDSHEET8, in order.
    let mut plies: Vec<(usize, u8)> = Vec::new();
    let mut sst: Vec<String> = Vec::new();
    let mut fmt_codes: HashMap<u16, String> = HashMap::new();
    let mut xf_fmt: Vec<u16> = Vec::new();

    for r in recs.iter().skip(1) {
        if r.ty == EOF {
            break;
        }
        let mut c = Cur::new(r);
        // A record too short for its fields is skipped (`None`).
        let _ = (|| -> Option<()> {
            match r.ty {
                0x002F => return Some(()), // FILEPASS: handled below
                0x0022 => book.date1904 = c.u16()? == 1,
                0x0085 => {
                    let pos = c.u32()? as usize;
                    c.u8()?;
                    let dt = c.u8()?;
                    g.sheets.push(c.short_string()?);
                    plies.push((pos, dt));
                }
                0x00FC => {
                    c.u32()?;
                    let unique = c.u32()? as usize;
                    for _ in 0..unique {
                        sst.push(c.rich_string()?);
                    }
                }
                0x041E => {
                    let id = c.u16()?;
                    fmt_codes.insert(id, c.xl_string()?);
                }
                0x00E0 => {
                    c.u16()?;
                    xf_fmt.push(c.u16()?);
                }
                0x01AE => {
                    c.u16()?;
                    let kind = match c.u16()? {
                        0x0401 => Book::Own,
                        0x3A01 => Book::AddIn,
                        _ => Book::External,
                    };
                    g.books.push(SupBook {
                        kind,
                        names: Vec::new(),
                    });
                }
                0x0023 => {
                    c.u16()?;
                    c.u32()?;
                    let name = c.short_string()?;
                    g.books.last_mut()?.names.push(name);
                }
                0x0017 => {
                    let n = c.u16()?;
                    for _ in 0..n {
                        g.xti.push((c.u16()?, c.u16()? as i16, c.u16()? as i16));
                    }
                }
                0x0018 => {
                    let flags = c.u16()?;
                    c.u8()?;
                    let cch = c.u8()? as usize;
                    let cce = c.u16()? as usize;
                    c.u16()?;
                    let itab = c.u16()?;
                    c.take(4)?;
                    let high = c.u8()? & 1 == 1;
                    let mut name = c.chars(cch, high)?;
                    if flags & 0x20 != 0 {
                        name = builtin_name(name.chars().next().map_or(0xFF, |ch| ch as u8));
                    }
                    let rgce = c.take(cce)?.to_vec();
                    let extra = c.rest().to_vec();
                    g.names.push(RawName {
                        // fFunc / fOB (VBA) / the _xlfn. future-function names.
                        function: flags & 0x0E != 0 || name.starts_with("_xlfn."),
                        name,
                        itab,
                        rgce,
                        extra,
                    });
                }
                _ => {}
            }
            Some(())
        })();
        if r.ty == 0x002F {
            return Err(OpenError::EncryptedXls);
        }
    }

    let format_of = |ixfe: u16, book: &mut BookIn| -> u32 {
        let ifmt = xf_fmt.get(ixfe as usize).copied().unwrap_or(0);
        match fmt_codes
            .get(&ifmt)
            .cloned()
            .or_else(|| builtin_format(ifmt as u32))
        {
            Some(code) => book.format_index(&code),
            None => 0,
        }
    };

    // Worksheets, in BOUNDSHEET8 order; `imported[i]` is the model index
    // of BOUNDSHEET8 `i`, for sheet-scoped names.
    let mut imported: Vec<Option<usize>> = Vec::new();
    let index: HashMap<usize, usize> = recs.iter().enumerate().map(|(i, r)| (r.pos, i)).collect();
    // A substream two BOUNDSHEET8s point at is read once (a crafted file
    // could otherwise multiply one sheet's cells by its sheet count).
    let mut seen: HashSet<usize> = HashSet::new();
    for (i, &(pos, dt)) in plies.iter().enumerate() {
        let start = index.get(&pos).copied().filter(|_| seen.insert(pos));
        let is_sheet = dt == 0
            && start
                .and_then(|s| recs[s].data.get(2..4))
                .is_some_and(|d| u16::from_le_bytes([d[0], d[1]]) == 0x0010);
        let (Some(start), true) = (start, is_sheet) else {
            imported.push(None);
            continue;
        };
        let sheet = read_sheet(&recs[start + 1..], &g, &sst, &mut book, &format_of)?;
        imported.push(Some(book.sheets.len()));
        book.push_sheet(SheetIn {
            name: g.sheets[i].clone(),
            cells: sheet,
        })?;
    }

    for n in &g.names {
        if n.function {
            continue;
        }
        let scope = match n.itab {
            0 => None,
            t => match imported.get(t as usize - 1).copied().flatten() {
                Some(s) => Some(s),
                // Scoped to a chart or macro sheet that wasn't imported.
                None => continue,
            },
        };
        let Some(formula) = ptg::decompile(Biff::V8, &n.rgce, &n.extra, Base::Cell(None), &g)
        else {
            continue;
        };
        book.names.push(DefinedName {
            name: n.name.clone(),
            scope,
            formula,
        });
    }
    Ok(book)
}

/// A formula cell whose tokens are a ptgExp: it is resolved once the
/// sheet's shared and array formulas (which follow their first cell) are
/// all known.
struct Pending {
    /// The cell.
    at: (u32, u32),
    /// The shared or array formula's master cell, as ptgExp names it.
    master: (u32, u32),
}

/// A shared or array formula: its range (r1, r2, c1, c2), tokens and extra
/// data.
type Group = ((u32, u32, u32, u32), Vec<u8>, Vec<u8>);

/// One worksheet substream (the records after its BOF) to cells.
fn read_sheet(
    recs: &[Rec],
    g: &Globals,
    sst: &[String],
    book: &mut BookIn,
    format_of: &dyn Fn(u16, &mut BookIn) -> u32,
) -> Result<BTreeMap<(u32, u32), Cell>, OpenError> {
    let mut cells = BTreeMap::new();
    // Cells charged to the workbook's budget so far.
    let mut charged = 0usize;
    // The FORMULA whose string result the next STRING record holds.
    let mut want_string: Option<(u32, u32)> = None;
    let mut pending: Vec<Pending> = Vec::new();
    // Shared formulas by their master: the FORMULA just before SHRFMLA,
    // which is the cell every member's ptgExp names (not necessarily the
    // range's top-left), and the range's top-left as well.
    let mut shared: HashMap<(u32, u32), Group> = HashMap::new();
    // The last FORMULA cell read: a SHRFMLA's master.
    let mut last_formula: Option<(u32, u32)> = None;
    // Array formulas: anchor → (range, rgce, extra).
    let mut arrays: HashMap<(u32, u32), Group> = HashMap::new();

    let put = |cells: &mut BTreeMap<(u32, u32), Cell>,
               r: u16,
               c: u16,
               ixfe: u16,
               value: CellValue,
               book: &mut BookIn| {
        if !on_grid(r as u32, c as u32) {
            return;
        }
        cells.insert(
            (r as u32, c as u32),
            Cell {
                value,
                style: format_of(ixfe, book),
                ..Cell::default()
            },
        );
    };

    // A substream nested in the sheet's (an embedded chart's BOF..EOF) is
    // skipped whole: its records (cached series values among them) are not
    // the sheet's cells, and its EOF doesn't end the sheet.
    let mut depth = 0usize;
    for r in recs {
        match r.ty {
            BOF => {
                depth += 1;
                continue;
            }
            EOF if depth > 0 => {
                depth -= 1;
                continue;
            }
            EOF => break,
            _ if depth > 0 => continue,
            _ => {}
        }
        let mut c = Cur::new(r);
        let _ = (|| -> Option<()> {
            match r.ty {
                0x0203 => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    put(&mut cells, row, col, xf, CellValue::Number(c.f64()?), book);
                }
                0x027E => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    put(
                        &mut cells,
                        row,
                        col,
                        xf,
                        CellValue::Number(rk(c.u32()?)),
                        book,
                    );
                }
                0x00BD => {
                    let row = c.u16()?;
                    let first = c.u16()?;
                    let n = (r.data.len().checked_sub(6)?) / 6;
                    for i in 0..n {
                        let xf = c.u16()?;
                        let v = rk(c.u32()?);
                        let col = first.checked_add(u16::try_from(i).ok()?)?;
                        put(&mut cells, row, col, xf, CellValue::Number(v), book);
                    }
                }
                0x00FD => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    let s = sst.get(c.u32()? as usize)?.clone();
                    put(&mut cells, row, col, xf, CellValue::Text(s), book);
                }
                0x0204 => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    let s = c.xl_string()?;
                    put(&mut cells, row, col, xf, CellValue::Text(s), book);
                }
                0x0205 => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    let v = c.u8()?;
                    let value = if c.u8()? == 1 {
                        CellValue::Error(biff_error(v).to_string())
                    } else {
                        CellValue::Bool(v != 0)
                    };
                    put(&mut cells, row, col, xf, value, book);
                }
                0x0201 => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    put(&mut cells, row, col, xf, CellValue::Empty, book);
                }
                0x00BE => {
                    let row = c.u16()?;
                    let first = c.u16()?;
                    let n = (r.data.len().checked_sub(6)?) / 2;
                    for i in 0..n {
                        let xf = c.u16()?;
                        let col = first.checked_add(u16::try_from(i).ok()?)?;
                        put(&mut cells, row, col, xf, CellValue::Empty, book);
                    }
                }
                0x0006 => {
                    let (row, col, xf) = (c.u16()?, c.u16()?, c.u16()?);
                    // A SHRFMLA after this record belongs to this cell, even
                    // when the rest of the record doesn't read.
                    last_formula = Some((row as u32, col as u32));
                    let val = c.take(8)?;
                    c.u16()?;
                    c.u32()?;
                    let cce = c.u16()? as usize;
                    let rgce = c.take(cce)?;
                    let extra = c.rest();
                    let value = if val[6] == 0xFF && val[7] == 0xFF {
                        match val[0] {
                            0 => {
                                want_string = Some((row as u32, col as u32));
                                CellValue::Text(String::new())
                            }
                            1 => CellValue::Bool(val[2] != 0),
                            2 => CellValue::Error(biff_error(val[2]).to_string()),
                            _ => CellValue::Text(String::new()),
                        }
                    } else {
                        CellValue::Number(f64::from_le_bytes(val.try_into().ok()?))
                    };
                    put(&mut cells, row, col, xf, value, book);
                    let at = (row as u32, col as u32);
                    if rgce.first() == Some(&0x01) && rgce.len() >= 5 {
                        let mr = u16::from_le_bytes([rgce[1], rgce[2]]) as u32;
                        let mc = u16::from_le_bytes([rgce[3], rgce[4]]) as u32;
                        pending.push(Pending {
                            at,
                            master: (mr, mc),
                        });
                    } else if let Some(f) =
                        ptg::decompile(Biff::V8, rgce, extra, Base::Cell(Some(at)), g)
                    {
                        cells.get_mut(&at)?.formula = Some(f);
                    }
                }
                0x0207 => {
                    let at = want_string.take()?;
                    let s = c.xl_string()?;
                    cells.get_mut(&at)?.value = CellValue::Text(s);
                }
                0x04BC => {
                    let range = ref_u(&mut c)?;
                    c.u8()?;
                    c.u8()?;
                    let cce = c.u16()? as usize;
                    let rgce = c.take(cce)?.to_vec();
                    let group = (range, rgce, c.rest().to_vec());
                    let corner = (range.0, range.2);
                    let master = last_formula.unwrap_or(corner);
                    // The corner is only an alias: it never replaces a
                    // group whose master it is, whichever arrived first.
                    if master != corner {
                        shared.entry(corner).or_insert_with(|| group.clone());
                    }
                    shared.insert(master, group);
                }
                0x0221 => {
                    let range = ref_u(&mut c)?;
                    c.u16()?;
                    c.u32()?;
                    let cce = c.u16()? as usize;
                    let rgce = c.take(cce)?.to_vec();
                    arrays.insert((range.0, range.2), (range, rgce, c.rest().to_vec()));
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

    for p in pending {
        if let Some((range, rgce, extra)) = arrays.get(&p.master) {
            // An array formula lives on its anchor; the other cells of the
            // range hold plain values, as an .xlsx keeps them.
            if p.at == p.master {
                let Some(f) = ptg::decompile(Biff::V8, rgce, extra, Base::Cell(Some(p.at)), g)
                else {
                    continue;
                };
                if let Some(cell) = cells.get_mut(&p.at) {
                    set_array(cell, *range, f);
                }
            }
            continue;
        }
        // A cell outside the group's stated range still takes it: writers
        // get the range wrong, and the tokens are relative to the cell.
        let Some((_, rgce, extra)) = shared.get(&p.master) else {
            continue;
        };
        let base = Base::Shared(p.at.0, p.at.1);
        if let Some(f) = ptg::decompile(Biff::V8, rgce, extra, base, g) {
            if let Some(cell) = cells.get_mut(&p.at) {
                cell.formula = Some(f);
            }
        }
    }
    Ok(cells)
}

/// A `RefU`: rwFirst, rwLast (16-bit), colFirst, colLast (8-bit), as
/// (r1, r2, c1, c2).
fn ref_u(c: &mut Cur) -> Option<(u32, u32, u32, u32)> {
    Some((
        c.u16()? as u32,
        c.u16()? as u32,
        c.u8()? as u32,
        c.u8()? as u32,
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A record: id, length, body.
    pub(crate) fn rec(ty: u16, body: &[u8]) -> Vec<u8> {
        let mut v = ty.to_le_bytes().to_vec();
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn bof(dt: u16) -> Vec<u8> {
        let mut b = 0x0600u16.to_le_bytes().to_vec();
        b.extend_from_slice(&dt.to_le_bytes());
        b.extend_from_slice(&[0; 12]);
        rec(BOF, &b)
    }

    fn short(s: &str) -> Vec<u8> {
        let mut v = vec![s.len() as u8, 0];
        v.extend_from_slice(s.as_bytes());
        v
    }

    fn cell(row: u16, col: u16, xf: u16) -> Vec<u8> {
        let mut v = row.to_le_bytes().to_vec();
        v.extend_from_slice(&col.to_le_bytes());
        v.extend_from_slice(&xf.to_le_bytes());
        v
    }

    /// A workbook stream: globals (with `globals` records), one worksheet
    /// "Data" with `sheet` records.
    pub(crate) fn workbook(globals: &[Vec<u8>], sheet: &[Vec<u8>]) -> Vec<u8> {
        let mut head = bof(0x0005);
        for g in globals {
            head.extend_from_slice(g);
        }
        // BOUNDSHEET8's offset is patched once the globals' size is known.
        let mut bs = vec![0u8; 4];
        bs.extend_from_slice(&[0, 0]);
        bs.extend(short("Data"));
        let bs_len = rec(0x0085, &bs).len();
        let eof = rec(EOF, &[]);
        let offset = (head.len() + bs_len + eof.len()) as u32;
        bs[..4].copy_from_slice(&offset.to_le_bytes());
        let mut out = head;
        out.extend(rec(0x0085, &bs));
        out.extend(eof.clone());
        out.extend(bof(0x0010));
        for s in sheet {
            out.extend_from_slice(s);
        }
        out.extend(eof);
        out
    }

    fn number(row: u16, col: u16, v: f64) -> Vec<u8> {
        let mut b = cell(row, col, 0);
        b.extend(v.to_le_bytes());
        rec(0x0203, &b)
    }

    fn formula(row: u16, col: u16, cached: f64, rgce: &[u8]) -> Vec<u8> {
        let mut b = cell(row, col, 0);
        b.extend(cached.to_le_bytes());
        b.extend([0, 0, 0, 0, 0, 0]);
        b.extend((rgce.len() as u16).to_le_bytes());
        b.extend_from_slice(rgce);
        rec(0x0006, &b)
    }

    fn data(book: &BookIn) -> &BTreeMap<(u32, u32), Cell> {
        &book.sheets[0].cells
    }

    #[test]
    fn numbers_rk_and_formulas() {
        let rk_100 = ((12345u32 << 2) | 2 | 1).to_le_bytes(); // 123.45
        let mut rkb = cell(1, 0, 0);
        rkb.extend(rk_100);
        let stream = workbook(
            &[],
            &[
                number(0, 0, 2.5),
                rec(0x027E, &rkb),
                formula(
                    2,
                    0,
                    125.95,
                    &[0x24, 0, 0, 0, 0xC0, 0x24, 1, 0, 0, 0xC0, 0x03],
                ),
            ],
        );
        let book = read(&stream).unwrap();
        assert_eq!(book.sheets[0].name, "Data");
        let c = data(&book);
        assert_eq!(c[&(0, 0)].value, CellValue::Number(2.5));
        assert_eq!(c[&(1, 0)].value, CellValue::Number(123.45));
        assert_eq!(c[&(2, 0)].value, CellValue::Number(125.95));
        assert_eq!(c[&(2, 0)].formula.as_deref(), Some("A1+A2"));
    }

    #[test]
    fn sst_strings_split_across_continue() {
        // Two strings: "hello" whole, then "wörld" in UTF-16 split after
        // "wö" — the CONTINUE re-sends the high-byte flag (now 8-bit).
        let mut sst = vec![];
        sst.extend(2u32.to_le_bytes());
        sst.extend(2u32.to_le_bytes());
        sst.extend([5, 0, 0]);
        sst.extend(b"hello");
        sst.extend([5, 0, 1]);
        sst.extend([b'w', 0, 0xF6, 0]);
        let mut cont = vec![0u8];
        cont.extend(b"rld");
        let mut globals = rec(0x00FC, &sst);
        globals.extend(rec(CONTINUE, &cont));
        let mut l1 = cell(0, 0, 0);
        l1.extend(1u32.to_le_bytes());
        let mut l0 = cell(1, 0, 0);
        l0.extend(0u32.to_le_bytes());
        let stream = workbook(&[globals], &[rec(0x00FD, &l1), rec(0x00FD, &l0)]);
        let book = read(&stream).unwrap();
        assert_eq!(data(&book)[&(0, 0)].value, CellValue::Text("wörld".into()));
        assert_eq!(data(&book)[&(1, 0)].value, CellValue::Text("hello".into()));
    }

    #[test]
    fn string_bool_and_error_results() {
        let mut f = cell(0, 0, 0);
        f.extend([0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
        f.extend([0; 6]);
        f.extend([4, 0, 0x17, 1, 0]); // ="x"
        f.extend([b'x']);
        let mut s = vec![2, 0, 0];
        s.extend(b"hi");
        let mut b = cell(1, 0, 0);
        b.extend([1, 0, 1, 0, 0, 0, 0xFF, 0xFF]);
        b.extend([0; 6]);
        b.extend([2, 0, 0x1D, 1]);
        let mut e = cell(2, 0, 0);
        e.extend([2, 0, 0x07, 0, 0, 0, 0xFF, 0xFF]);
        e.extend([0; 6]);
        e.extend([2, 0, 0x1C, 0x07]);
        let stream = workbook(
            &[],
            &[
                rec(0x0006, &f),
                rec(0x0207, &s),
                rec(0x0006, &b),
                rec(0x0006, &e),
            ],
        );
        let book = read(&stream).unwrap();
        let c = data(&book);
        assert_eq!(c[&(0, 0)].value, CellValue::Text("hi".into()));
        assert_eq!(c[&(1, 0)].value, CellValue::Bool(true));
        assert_eq!(c[&(1, 0)].formula.as_deref(), Some("TRUE"));
        assert_eq!(c[&(2, 0)].value, CellValue::Error("#DIV/0!".into()));
    }

    #[test]
    fn shared_formulas_resolve_per_cell() {
        // B1:B3 share =A1*2 (ptgRefN row+0 col-1 relative), SHRFMLA after
        // the first cell, as Excel writes it.
        let exp = [0x01, 0, 0, 1, 0];
        let mut sh = vec![0, 0, 2, 0, 1, 1, 0, 3];
        let rgce = [0x2C, 0, 0, 0xFF, 0xC0, 0x1E, 2, 0, 0x05];
        sh.extend((rgce.len() as u16).to_le_bytes());
        sh.extend(rgce);
        let stream = workbook(
            &[],
            &[
                formula(0, 1, 2.0, &exp),
                rec(0x04BC, &sh),
                formula(1, 1, 4.0, &exp),
                formula(2, 1, 6.0, &exp),
            ],
        );
        let book = read(&stream).unwrap();
        let c = data(&book);
        assert_eq!(c[&(0, 1)].formula.as_deref(), Some("A1*2"));
        assert_eq!(c[&(2, 1)].formula.as_deref(), Some("A3*2"));
    }

    /// ptgExp names the group's first formula cell, B1 here, while the
    /// stated range is A1:B2 and the members are B1, A2, B2.
    #[test]
    fn shared_formulas_keyed_by_the_cell_ptg_exp_names() {
        let exp = [0x01, 0, 0, 1, 0];
        // SHRFMLA A1:B2: =<cell one row down>+1 (ptgRefN row +1, col +0).
        let mut sh = vec![0, 0, 1, 0, 0, 1, 0, 3];
        let rgce = [0x2C, 1, 0, 0, 0xC0, 0x1E, 1, 0, 0x03];
        sh.extend((rgce.len() as u16).to_le_bytes());
        sh.extend(rgce);
        let stream = workbook(
            &[],
            &[
                formula(0, 1, 0.0, &exp),
                rec(0x04BC, &sh),
                formula(1, 0, 0.0, &exp),
                formula(1, 1, 0.0, &exp),
                // Outside A1:B2, still a member.
                formula(2, 1, 0.0, &exp),
            ],
        );
        let book = read(&stream).unwrap();
        let c = data(&book);
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
        let exp = |r: u8, c: u8| [0x01, r, 0, c, 0];
        let shrfmla = |range: [u8; 6], op: u8| {
            let mut sh = range.to_vec();
            sh.extend([0, 3]);
            // =<cell one row down> op 2
            let rgce = [0x2C, 1, 0, 0, 0xC0, 0x1E, 2, 0, op];
            sh.extend((rgce.len() as u16).to_le_bytes());
            sh.extend(rgce);
            rec(0x04BC, &sh)
        };
        let group1 = vec![
            formula(0, 0, 0.0, &exp(0, 0)),
            shrfmla([0, 0, 2, 0, 0, 0], 0x03),
            formula(1, 0, 0.0, &exp(0, 0)),
            formula(2, 0, 0.0, &exp(0, 0)),
        ];
        let group2 = vec![
            formula(0, 2, 0.0, &exp(0, 2)),
            shrfmla([0, 0, 2, 0, 0, 2], 0x05),
            formula(1, 2, 0.0, &exp(0, 2)),
            formula(2, 2, 0.0, &exp(0, 2)),
        ];
        for sheet in [
            [group1.clone(), group2.clone()].concat(),
            [group2, group1].concat(),
        ] {
            let book = read(&workbook(&[], &sheet)).unwrap();
            let c = data(&book);
            assert_eq!(c[&(0, 0)].formula.as_deref(), Some("A2+2"));
            assert_eq!(c[&(2, 0)].formula.as_deref(), Some("A4+2"));
            assert_eq!(c[&(0, 2)].formula.as_deref(), Some("C2*2"));
            assert_eq!(c[&(2, 2)].formula.as_deref(), Some("C4*2"));
        }
    }

    /// A FORMULA too short to read past its cell still is the master of the
    /// SHRFMLA after it.
    #[test]
    fn a_truncated_formula_is_still_the_next_groups_master() {
        // The stated range (B2) isn't the master (B1), so only the master
        // key finds the group.
        let mut sh = vec![1, 0, 1, 0, 1, 1, 0, 2];
        let rgce = [0x2C, 1, 0, 0, 0xC0, 0x1E, 1, 0, 0x03];
        sh.extend((rgce.len() as u16).to_le_bytes());
        sh.extend(rgce);
        let stream = workbook(
            &[],
            &[
                formula(5, 5, 0.0, &[0x1E, 1, 0]),
                rec(0x0006, &cell(0, 1, 0)),
                rec(0x04BC, &sh),
                formula(1, 1, 0.0, &[0x01, 0, 0, 1, 0]),
            ],
        );
        let book = read(&stream).unwrap();
        assert_eq!(data(&book)[&(1, 1)].formula.as_deref(), Some("B3+1"));
    }

    #[test]
    fn array_formula_on_its_anchor() {
        // {=A1:A2*2} over B1:B2.
        let exp = [0x01, 0, 0, 1, 0];
        let mut arr = vec![0, 0, 1, 0, 1, 1];
        arr.extend([0, 0, 0, 0, 0, 0]);
        let rgce = [0x65, 0, 0, 1, 0, 0, 0xC0, 0, 0xC0, 0x1E, 2, 0, 0x05];
        arr.extend((rgce.len() as u16).to_le_bytes());
        arr.extend(rgce);
        let stream = workbook(
            &[],
            &[
                formula(0, 1, 2.0, &exp),
                rec(0x0221, &arr),
                formula(1, 1, 4.0, &exp),
            ],
        );
        let book = read(&stream).unwrap();
        let c = data(&book);
        assert_eq!(c[&(0, 1)].formula.as_deref(), Some("A1:A2*2"));
        assert_eq!(
            c[&(0, 1)].f_attrs.as_deref(),
            Some(" t=\"array\" ref=\"B1:B2\"")
        );
        assert_eq!(c[&(0, 1)].spill, Some((2, 1)));
        assert_eq!(c[&(1, 1)].formula, None);
        assert_eq!(c[&(1, 1)].value, CellValue::Number(4.0));
    }

    #[test]
    fn coordinates_past_the_grid_or_u16_are_dropped() {
        // MULRK from column 16383 (XFD) over three cells: only XFD is on
        // the grid. MULBLANK from 0xFFFF over two cells would overflow u16.
        let mut mulrk = cell(0, 16383, 0)[..4].to_vec();
        for v in [1u32, 2, 3] {
            mulrk.extend([0, 0]);
            mulrk.extend(((v << 2) | 2).to_le_bytes());
        }
        mulrk.extend(16385u16.to_le_bytes());
        let mut mulblank = cell(1, 0xFFFF, 0)[..4].to_vec();
        mulblank.extend([0, 0, 0, 0]);
        mulblank.extend(0u16.to_le_bytes());
        // An ARRAY whose range is upside down: no formula, no panic.
        let exp = [0x01, 2, 0, 0, 0];
        let mut arr = vec![2, 0, 0, 0, 0, 0];
        arr.extend([0, 0, 0, 0, 0, 0]);
        arr.extend(3u16.to_le_bytes());
        arr.extend([0x1E, 1, 0]);
        let stream = workbook(
            &[],
            &[
                rec(0x00BD, &mulrk),
                rec(0x00BE, &mulblank),
                number(0, 20000, 1.0),
                formula(2, 0, 1.0, &exp),
                rec(0x0221, &arr),
            ],
        );
        let book = read(&stream).unwrap();
        let c = data(&book);
        assert_eq!(c[&(0, 16383)].value, CellValue::Number(1.0));
        assert!(c.keys().all(|&(r, col)| on_grid(r, col)));
        assert_eq!(c[&(2, 0)].formula, None);
        assert_eq!(c[&(2, 0)].value, CellValue::Number(1.0));
    }

    /// Two BOUNDSHEET8s at the same substream: one sheet, read once.
    #[test]
    fn a_sheet_two_plies_point_at_is_read_once() {
        let stream = workbook(&[], &[number(0, 0, 1.0)]);
        let recs = records(&stream);
        let bs = recs.iter().find(|r| r.ty == 0x0085).unwrap();
        let len = 4 + bs.data.len();
        let mut dup = stream[..bs.pos + len].to_vec();
        dup.extend_from_slice(&stream[bs.pos..bs.pos + len]);
        dup.extend_from_slice(&stream[bs.pos + len..]);
        // Both offsets move by the inserted record.
        for at in [bs.pos + 4, bs.pos + len + 4] {
            let off = u32::from_le_bytes(dup[at..at + 4].try_into().unwrap()) + len as u32;
            dup[at..at + 4].copy_from_slice(&off.to_le_bytes());
        }
        let book = read(&dup).unwrap();
        assert_eq!(book.sheets.len(), 1);
        assert_eq!(data(&book).len(), 1);
    }

    #[test]
    fn the_cell_budget_refuses_a_workbook() {
        let stream = workbook(&[], &[number(0, 0, 1.0), number(1, 0, 2.0)]);
        let tight = Limits {
            cells: 1,
            ..Limits::default()
        };
        let err = read_with(&stream, tight).err().unwrap();
        assert!(err.to_string().contains("too many cells"), "{err}");
        assert!(read(&stream).is_ok());
    }

    /// An embedded chart is a BOF..EOF substream inside the sheet's: its
    /// records are not cells, and the sheet goes on after its EOF.
    #[test]
    fn an_embedded_chart_substream_is_skipped() {
        let stream = workbook(
            &[],
            &[
                number(0, 0, 1.0),
                bof(0x0020),
                number(0, 0, 99.0),
                rec(EOF, &[]),
                number(1, 0, 2.0),
            ],
        );
        let book = read(&stream).unwrap();
        let c = data(&book);
        assert_eq!(c[&(0, 0)].value, CellValue::Number(1.0));
        assert_eq!(c[&(1, 0)].value, CellValue::Number(2.0));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn leniency_unknown_records_ptgs_and_truncation() {
        // An unknown record, a formula with an unknown ptg (keeps its value),
        // a NUMBER too short for its fields, then a record cut off by the end
        // of the stream.
        let mut stream = workbook(
            &[rec(0x7777, &[1, 2, 3])],
            &[
                rec(0x7778, &[]),
                formula(0, 0, 7.0, &[0x18, 0, 0]),
                rec(0x0203, &[0, 0]),
                number(1, 0, 1.0),
            ],
        );
        // Drop the sheet's EOF and end on a NUMBER that claims 14 bytes.
        stream.truncate(stream.len() - 4);
        stream.extend([0x03, 0x02, 14, 0, 1, 2, 3]);
        let book = read(&stream).unwrap();
        let c = data(&book);
        assert_eq!(c[&(0, 0)].value, CellValue::Number(7.0));
        assert_eq!(c[&(0, 0)].formula, None);
        assert_eq!(c[&(1, 0)].value, CellValue::Number(1.0));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn formats_names_and_date_mode() {
        // DATEMODE 1904, a custom format 164 on XF 1, a global name and a
        // hidden _xlfn. function name (which is not a defined name).
        let mut fmt = 164u16.to_le_bytes().to_vec();
        fmt.extend([10, 0, 0]);
        fmt.extend(b"yyyy-mm-dd");
        let xf0 = vec![0u8; 20];
        let mut xf1 = vec![0u8, 0, 164, 0];
        xf1.extend([0; 16]);
        let name = |flags: u16, text: &str, rgce: &[u8]| {
            let mut n = flags.to_le_bytes().to_vec();
            n.push(0);
            n.push(text.len() as u8);
            n.extend((rgce.len() as u16).to_le_bytes());
            n.extend([0, 0, 0, 0, 0, 0, 0, 0, 0]);
            n.extend(text.as_bytes());
            n.extend_from_slice(rgce);
            rec(0x0018, &n)
        };
        let mut num = 0.21f64.to_le_bytes().to_vec();
        num.insert(0, 0x1F);
        let mut d = cell(0, 0, 1);
        d.extend(100.0f64.to_le_bytes());
        let stream = workbook(
            &[
                rec(0x0022, &[1, 0]),
                rec(0x041E, &fmt),
                rec(0x00E0, &xf0),
                rec(0x00E0, &xf1),
                name(0, "TaxRate", &num),
                name(0x03, "_xlfn.MAXIFS", &[]),
            ],
            &[rec(0x0203, &d)],
        );
        let book = read(&stream).unwrap();
        assert!(book.date1904);
        let style = data(&book)[&(0, 0)].style;
        assert_eq!(book.formats[style as usize], "yyyy-mm-dd");
        assert_eq!(book.names.len(), 1);
        assert_eq!(book.names[0].name, "TaxRate");
        assert_eq!(book.names[0].formula, "0.21");
    }

    /// `=Sheet2!Rate*2` with a name scoped to Sheet2: ptgNameX through an
    /// XTI into this workbook keeps the sheet.
    #[test]
    fn name_x_into_this_workbook_keeps_the_sheet() {
        let raw = |name: &str, itab| RawName {
            name: name.into(),
            function: false,
            itab,
            rgce: Vec::new(),
            extra: Vec::new(),
        };
        let g = Globals {
            sheets: vec!["Data".into(), "Sheet 2".into()],
            xti: vec![(0, 1, 1)],
            books: vec![SupBook {
                kind: Book::Own,
                names: Vec::new(),
            }],
            names: vec![raw("Global", 0), raw("Rate", 2)],
        };
        let f = [0x39, 0, 0, 2, 0, 0, 0, 0x1E, 2, 0, 0x05];
        let got = ptg::decompile(Biff::V8, &f, &[], Base::Cell(None), &g);
        assert_eq!(got.as_deref(), Some("'Sheet 2'!Rate*2"));
        let f = [0x39, 0, 0, 1, 0, 0, 0];
        let got = ptg::decompile(Biff::V8, &f, &[], Base::Cell(None), &g);
        assert_eq!(got.as_deref(), Some("Global"));
    }

    #[test]
    fn encrypted_and_biff5_are_refused() {
        let enc = workbook(&[rec(0x002F, &[1, 0, 1, 0, 1, 0])], &[]);
        assert_eq!(read(&enc).err(), Some(OpenError::EncryptedXls));
        assert_eq!(
            OpenError::EncryptedXls.to_string(),
            "password-protected .xls files are not supported"
        );
        let mut b5 = 0x0500u16.to_le_bytes().to_vec();
        b5.extend([5, 0, 0, 0, 0, 0, 0, 0]);
        let biff5 = rec(BOF, &b5);
        assert_eq!(read(&biff5).err(), Some(OpenError::Biff5));
        // A BIFF4 BOF id, an unknown version, and noise are corrupt, not BIFF5.
        let mut b8 = 0x0600u16.to_le_bytes().to_vec();
        b8.extend([5, 0]);
        for stream in [
            rec(0x0409, &b8),
            rec(BOF, &[0x00, 0x07, 5, 0]),
            vec![1, 2, 3, 4, 5, 6],
        ] {
            assert!(
                matches!(read(&stream), Err(OpenError::Corrupt(_))),
                "{stream:?}"
            );
        }
        assert_eq!(
            OpenError::Biff5.to_string(),
            "Excel 5.0/95 workbooks are not supported"
        );
    }

    #[test]
    fn a_1904_xls_survives_save_and_reopen() {
        let stream = workbook(&[rec(0x0022, &[1, 0])], &[number(0, 0, 100.0)]);
        let cfb = opccore::cfb::write_cfb(&[("Workbook", stream)]);
        let (pkg, fmt) = super::super::open_workbook(&cfb).unwrap();
        assert_eq!(fmt, super::super::SourceFormat::Xls);
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        assert!(back.workbook.date1904);
        assert_eq!(
            back.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Number(100.0)
        );
    }
}
