//! Excel 97-2003 `.xls`: the BIFF8 record stream in the compound file's
//! `Workbook` stream ([MS-XLS]).
//!
//! The globals substream gives the sheets (BOUNDSHEET8), shared strings
//! (SST), cell formats (FONT, FORMAT, XF, PALETTE), the date system
//! (DATEMODE), the reference tables (SUPBOOK, EXTERNSHEET, EXTERNNAME),
//! defined names (NAME) and the active sheet (WINDOW1). Each worksheet
//! substream gives its cells, merged cells (MERGECELLS), column widths
//! (COLINFO), row heights (ROW) and frozen panes (WINDOW2, PANE). Records
//! not listed here are skipped, and those whose content the import leaves
//! out (charts, comments, conditional formatting, ...) are named in
//! [`BookIn::dropped`], as are the formats a cell uses that the model can't
//! hold (underline, borders other than a thin box, ...).

use std::collections::{BTreeMap, HashMap, HashSet};

use super::ftab;
use super::ptg::{self, Base, Biff, Names};
use super::{
    BookIn, ExternalLink, Le, Limits, OpenError, SheetIn, biff_error, on_grid, rk, set_array,
    sheet_prefix,
};
use crate::sheet::{Align, Cell, CellValue, DefinedName, Xf};

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

/// A little-endian cursor over one record's data, which knows where its
/// CONTINUE pieces start.
struct Cur<'a> {
    le: Le<'a>,
    breaks: &'a [usize],
}

impl<'a> std::ops::Deref for Cur<'a> {
    type Target = Le<'a>;
    fn deref(&self) -> &Le<'a> {
        &self.le
    }
}

impl<'a> std::ops::DerefMut for Cur<'a> {
    fn deref_mut(&mut self) -> &mut Le<'a> {
        &mut self.le
    }
}

impl<'a> Cur<'a> {
    fn new(r: &'a Rec) -> Cur<'a> {
        Cur {
            le: Le::new(&r.data),
            breaks: &r.breaks,
        }
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
                    raw.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&[a, b]| u16::from_le_bytes([a, b])),
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
    /// formatting runs and phonetic block skipped; `rich` is set when it has
    /// runs.
    fn rich_string(&mut self, rich: &mut bool) -> Option<String> {
        let cch = self.u16()? as usize;
        let flags = self.u8()?;
        let runs = if flags & 0x08 != 0 {
            self.u16()? as usize
        } else {
            0
        };
        *rich |= runs > 0;
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
    /// An add-in function book (0x3A01): its EXTERNNAMEs are functions an
    /// XLL registers, spelled as [`ftab::xll_name`] gives.
    AddIn,
    /// Another workbook, or DDE/OLE: its place in [`BookIn::external_links`]
    /// when it is a Library book with a function name (see
    /// [`library_file`]), else `None` and references into it don't
    /// decompile.
    External(Option<usize>),
}

struct SupBook {
    kind: Book,
    /// Every EXTERNNAME under the book, in order (ptgNameX counts them
    /// all): the name, or `None` for one that isn't imported or didn't
    /// parse.
    names: Vec<Option<String>>,
    /// An external book in Excel's Library directory: its file name and
    /// sheet names, which make its external link.
    lib: Option<(String, Vec<String>)>,
}

/// The relationship type of an external link to a book in Excel's Library
/// directory, as Excel's `.xlsx` of corpus/legacy/addin/addin-udf writes
/// it.
const XL_LIBRARY: &str =
    "http://schemas.microsoft.com/office/2006/relationships/xlExternalLinkPath/xlLibrary";

/// The file name of a SUPBOOK's encoded virtPath ([MS-XLS] 2.5.277
/// VirtualPath) that names a file in Excel's Library directory: `0x01`
/// (encoded), `0x08` (the Library directory), then the file name. That is
/// how Excel writes Library\EUROTOOL.XLAM (corpus/legacy/addin/addin-udf.xls).
/// Any other form (a volume, a subdirectory, a plain path) has no file
/// showing how the `.xlsx` spells it, so it gives `None`.
fn library_file(path: &str) -> Option<String> {
    let file = path.strip_prefix("\u{1}\u{8}")?;
    let plain = !file.is_empty()
        && !file
            .chars()
            .any(|ch| ch < ' ' || matches!(ch, '\\' | '/' | ':'));
    plain.then(|| file.to_string())
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
        let &(book, first, _) = self.xti.get(ixti as usize)?;
        let book = self.books.get(book as usize)?;
        let ext_name = || book.names.get(index.checked_sub(1)? as usize)?.clone();
        match book.kind {
            Book::AddIn => ext_name(),
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
            // A function name of the whole Library book (first sheet -2),
            // as the `.xlsx` spells it: `[1]!EUROCONVERT`.
            Book::External(Some(k)) if first == -2 => Some(format!("[{}]!{}", k + 1, ext_name()?)),
            Book::External(_) => None,
        }
    }
}

/// The `_xlnm.` names of BIFF8's built-in name codes.
fn builtin_name(code: u8) -> String {
    match super::BUILTIN_NAMES.get(code as usize) {
        Some(n) => format!("_xlnm.{n}"),
        None => format!("_xlnm.Builtin_{code}"),
    }
}

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;

/// A FONT record's fields the import reads.
struct FontIn {
    /// Twips.
    height: u16,
    grbit: u16,
    icv: u16,
    bls: u16,
    sss: u16,
    uls: u8,
    name: String,
}

/// The colours of indices 0-7, which no palette changes.
pub(super) const FIXED_COLORS: [(u8, u8, u8); 8] = [
    (0, 0, 0),
    (0xFF, 0xFF, 0xFF),
    (0xFF, 0, 0),
    (0, 0xFF, 0),
    (0, 0, 0xFF),
    (0xFF, 0xFF, 0),
    (0xFF, 0, 0xFF),
    (0, 0xFF, 0xFF),
];

/// The workbook's FONT, FORMAT, XF and PALETTE records, made into the
/// model's cell formats as the cells use them.
struct Styles {
    fonts: Vec<FontIn>,
    /// Each XF record's 20 bytes (a short one padded).
    xfs: Vec<[u8; 20]>,
    palette: [(u8, u8, u8); 56],
    codes: HashMap<u16, String>,
    /// XF index → `book.xfs` index, for the XFs made so far.
    made: HashMap<u16, u32>,
    /// XFEXT's colours by XF index.
    ext: HashMap<u16, XfExt>,
    /// XFCRC's count of XF records. XFEXT is used only when it matches the
    /// XFs read: another writer that changed the XFs without rewriting
    /// their XFEXTs (which Excel detects the same way) has stale ones.
    xf_count: Option<u16>,
}

/// What an XFEXT says of an XF beyond its palette indices.
#[derive(Default)]
struct XfExt {
    /// The fill's and the text's exact colours (xclrType 2), which replace
    /// the palette's nearest.
    fill: Option<(u8, u8, u8)>,
    color: Option<(u8, u8, u8)>,
    /// A theme colour other than the default text's (theme 1, no tint):
    /// the palette's nearest is kept, the theme is lost.
    theme: bool,
    /// A gradient fill.
    gradient: bool,
}

impl XfExt {
    /// An XFEXT's ExtProp array (`cexts` of them); a malformed one ends
    /// the read with what came before it.
    fn read(c: &mut Cur, cexts: u16) -> XfExt {
        let mut e = XfExt::default();
        for _ in 0..cexts {
            let (Some(ty), Some(cb)) = (c.u16(), c.u16()) else {
                break;
            };
            let Some(body) = (cb as usize).checked_sub(4).and_then(|n| c.take(n)) else {
                break;
            };
            // FullColorExt: xclrType, nTintShade, xclrValue.
            let color = (body.len() >= 8).then(|| {
                let kind = u16::from_le_bytes([body[0], body[1]]);
                let tint = i16::from_le_bytes([body[2], body[3]]);
                let value = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                (kind, tint, value, (body[4], body[5], body[6]))
            });
            match (ty, color) {
                // Cell (pattern foreground) and text colours.
                (4 | 13, Some((kind, tint, value, rgb))) => match kind {
                    2 if ty == 4 => e.fill = Some(rgb),
                    2 => e.color = Some(rgb),
                    3 if !(ty == 13 && value == 1 && tint == 0) => e.theme = true,
                    _ => {}
                },
                (6, _) => e.gradient = true,
                _ => {}
            }
        }
        e
    }
}

impl Styles {
    /// The colour of palette index `icv`; `None` for the automatic and
    /// system colours.
    fn rgb(&self, icv: u16) -> Option<(u8, u8, u8)> {
        super::icv_rgb(icv as u32, &self.palette)
    }

    /// The `book.xfs` index of XF `ixfe`, made the first time a cell uses
    /// it. An XF the model can't hold all of is held as near as it can, and
    /// what it lacks is named in `book.dropped`.
    fn of(&mut self, ixfe: u16, book: &mut BookIn) -> u32 {
        if let Some(&i) = self.made.get(&ixfe) {
            return i;
        }
        let i = match self.xfs.get(ixfe as usize) {
            Some(raw) => {
                let mut xf = self.xf(raw, &mut book.dropped);
                let current = self.xf_count == Some(self.xfs.len() as u16);
                if let Some(e) = self.ext.get(&ixfe).filter(|_| current) {
                    if xf.fill.is_some() {
                        xf.fill = e.fill.or(xf.fill);
                    }
                    if let Some(c) = e.color {
                        xf.color = (c != (0, 0, 0)).then_some(c);
                    }
                    if e.theme {
                        book.dropped.insert("theme colors");
                    }
                    if e.gradient {
                        book.dropped.insert("patterned fills");
                    }
                }
                book.add_xf(xf)
            }
            None => 0,
        };
        self.made.insert(ixfe, i);
        i
    }

    fn xf(&self, b: &[u8; 20], dropped: &mut std::collections::BTreeSet<&'static str>) -> Xf {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let mut xf = Xf::default();
        let code = super::format_code(&self.codes, u16_at(2));
        if code.is_some() {
            xf.set_code(code);
        }
        // Index 4 doesn't exist: the 5th FONT record is font 5.
        let ifnt = u16_at(0) as usize;
        let font = match ifnt {
            4 => None,
            i if i > 4 => self.fonts.get(i - 1),
            i => self.fonts.get(i),
        };
        if let Some(f) = font {
            xf.bold = f.bls >= 600;
            xf.italic = f.grbit & 0x0002 != 0;
            // Black is the automatic colour as the model sees it.
            xf.color = self.rgb(f.icv).filter(|&c| c != (0, 0, 0));
            let size = f.height as f64 / 20.0;
            xf.font_size = (size != 11.0).then_some(size);
            xf.font_name = (!f.name.eq_ignore_ascii_case("Calibri")).then(|| f.name.clone());
            if f.uls != 0 {
                dropped.insert("underlined text");
            }
            if f.grbit & 0x0008 != 0 {
                dropped.insert("strikethrough");
            }
            if f.sss != 0 {
                dropped.insert("superscript and subscript");
            }
        }
        xf.quote_prefix = u16_at(4) & 0x0008 != 0;
        xf.align = match b[6] & 0x07 {
            0 => Align::General,
            1 => Align::Left,
            2 => Align::Center,
            3 => Align::Right,
            _ => {
                dropped.insert("horizontal alignment other than left, center or right");
                Align::General
            }
        };
        xf.wrap = b[6] & 0x08 != 0;
        if (b[6] >> 4) & 0x07 != 2 {
            dropped.insert("vertical alignment");
        }
        if b[7] != 0 {
            dropped.insert("rotated text");
        }
        if b[8] & 0x0F != 0 {
            dropped.insert("indented text");
        }
        if b[8] & 0x10 != 0 {
            dropped.insert("shrink to fit");
        }
        let dg = u32_at(10);
        let sides = [dg & 0xF, (dg >> 4) & 0xF, (dg >> 8) & 0xF, (dg >> 12) & 0xF];
        let more = u32_at(14);
        let diagonal = dg >> 30 != 0 && (more >> 21) & 0xF != 0;
        if sides == [1; 4] {
            xf.border = true;
            // Its sides' colours: left, right, top, bottom.
            let icvs = [
                (dg >> 16) & 0x7F,
                (dg >> 23) & 0x7F,
                more & 0x7F,
                (more >> 7) & 0x7F,
            ];
            if !icvs
                .iter()
                .all(|&icv| super::plain_border_color(self.rgb(icv as u16)))
            {
                dropped.insert("borders other than a thin box");
            }
        } else if sides != [0; 4] {
            dropped.insert("borders other than a thin box");
        }
        if diagonal {
            dropped.insert("borders other than a thin box");
        }
        let fls = (more >> 26) & 0x3F;
        let fore = u16_at(18) & 0x7F;
        if fls >= 1 {
            xf.fill = self.rgb(fore);
        }
        if fls > 1 {
            dropped.insert("patterned fills");
        }
        xf
    }
}

/// The workbook globals' records the import reads (a guarded arm whose
/// guard didn't hold is a default, so no loss), and those that hold nothing
/// a person sees in the workbook. Any other record is named in
/// `BookIn::unknown_records`, so a record nobody classified can't be lost
/// without a word.
#[rustfmt::skip]
const GLOBALS_KNOWN: &[u16] = &[
    // Read.
    0x000E, 0x0012, 0x0017, 0x0018, 0x0019, 0x0022, 0x0023, 0x002F, 0x0031, 0x003D, 0x0040,
    0x005B, 0x0085, 0x008D, 0x0092, 0x00DA, 0x00E0, 0x00FC, 0x01A9, 0x01AE, 0x01AF, 0x0293,
    0x041E, 0x0863, 0x087C, 0x087D, 0x088C,
    // CONTINUE (joined to its record), BOF.
    0x003C, 0x0809,
    // PASSWORD (PROTECT's hash; PROTECT is named), CODEPAGE, XCT and CRN
    // (another workbook's cached cells: formulas into it are counted),
    // WRITEACCESS, COUNTRY, FNGROUPCOUNT, MMS, INTERFACEHDR,
    // INTERFACEEND.
    0x0013, 0x0042, 0x0059, 0x005A, 0x005C, 0x008C, 0x009C, 0x00C1, 0x00E1, 0x00E2,
    // MSODRAWINGGROUP, and HFPICTURE with the drawing group
    // (fIsDrawingGroup), which Excel writes beside it: the drawings, and a
    // sheet's own header and footer pictures (its HFPICTURE), are named
    // where a sheet has them. EXTSST, TABID, USESELFS, DSF, REFRESHALL,
    // PROT4REVPASS (PROT4REV's hash), EXCEL9FILE, RECALCID.
    0x00EB, 0x0866, 0x00FF, 0x013D, 0x0160, 0x0161, 0x01B7, 0x01BC, 0x01C0, 0x01C1,
    // TABLESTYLES, STYLEEXT, THEME (the theme XFEXT's theme colours
    // resolve with; those are named), MTRSETTINGS, COMPRESSPICTURES,
    // FORCEFULLCALCULATION.
    0x088E, 0x0892, 0x0896, 0x089A, 0x089B, 0x08A3,
];

/// A worksheet's records, as [`GLOBALS_KNOWN`] is the globals'.
#[rustfmt::skip]
const SHEET_KNOWN: &[u16] = &[
    // Read.
    0x0006, 0x000C, 0x000D, 0x000F, 0x0010, 0x0011, 0x0012, 0x0014, 0x0015, 0x001A, 0x001B, 0x001C,
    0x0026, 0x0027, 0x0028, 0x0029, 0x002A, 0x002B, 0x0041, 0x0055, 0x005D, 0x007D, 0x0081, 0x0083,
    0x0084, 0x0099, 0x009D, 0x00A0, 0x00A1, 0x00AE, 0x00B0, 0x00BD, 0x00BE, 0x00E5, 0x00E9, 0x00EC,
    0x00FD, 0x01AA, 0x01AD, 0x01B0, 0x01B1, 0x01B2, 0x01B8, 0x01BE, 0x0201, 0x0204, 0x0205, 0x0207,
    0x0208, 0x0221, 0x0225, 0x0236, 0x023E, 0x027E, 0x04BC, 0x0800, 0x0862, 0x0868, 0x0872, 0x0879, 0x087A,
    0x088B, 0x005F, 0x0866, 0x089C,
    // CONTINUE, and USERSVIEWEND (its view's BEGIN is named).
    0x003C, 0x01AB,
    // PASSWORD (PROTECT's hash), SELECTION, GUTS (the outline gutters'
    // size), GRIDSET, OBJPROTECT and SCENPROTECT (PROTECT's parts),
    // DBCELL, DIMENSIONS, INDEX, TXO (a note's text: notes are named).
    0x0013, 0x001D, 0x0080, 0x0082, 0x0063, 0x00DD, 0x00D7, 0x0200, 0x020B, 0x01B6,
    // FEATHDR, FEATHDR11 and LIST12 (FEAT and FEATURE11 are named), and
    // 0x01C2, which Excel writes beside spilled dynamic arrays (the arrays
    // are read from their ARRAY records).
    0x0867, 0x0871, 0x0877, 0x01C2,
];

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
    // (stream offset, dt, hidden) of each BOUNDSHEET8, in order.
    let mut plies: Vec<(usize, u8, bool)> = Vec::new();
    let mut sst: Vec<String> = Vec::new();
    let mut rich = false;
    let mut styles = Styles {
        fonts: Vec::new(),
        xfs: Vec::new(),
        palette: super::xls_write::DEFAULT_PALETTE,
        codes: HashMap::new(),
        made: HashMap::new(),
        ext: HashMap::new(),
        xf_count: None,
    };
    let mut itab_cur = 0usize;

    for (k, r) in recs.iter().enumerate().skip(1) {
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
                    let state = c.u8()?;
                    let dt = c.u8()?;
                    g.sheets.push(c.short_string()?);
                    if state & 0x03 == 2 {
                        book.dropped.insert("very hidden sheets (saved as hidden)");
                    }
                    plies.push((pos, dt, state & 0x03 != 0));
                }
                0x00FC => {
                    c.u32()?;
                    let unique = c.u32()? as usize;
                    for _ in 0..unique {
                        sst.push(c.rich_string(&mut rich)?);
                    }
                }
                0x041E => {
                    let id = c.u16()?;
                    styles.codes.insert(id, c.xl_string()?);
                }
                0x00E0 => {
                    let mut raw = [0u8; 20];
                    let n = r.data.len().min(20);
                    raw[..n].copy_from_slice(&r.data[..n]);
                    styles.xfs.push(raw);
                }
                0x0031 => {
                    let height = c.u16()?;
                    let grbit = c.u16()?;
                    let icv = c.u16()?;
                    let bls = c.u16()?;
                    let sss = c.u16()?;
                    let uls = c.u8()?;
                    c.take(3)?;
                    styles.fonts.push(FontIn {
                        height,
                        grbit,
                        icv,
                        bls,
                        sss,
                        uls,
                        name: c.short_string()?,
                    });
                }
                0x0092 => {
                    let n = (c.u16()? as usize).min(56);
                    for i in 0..n {
                        let rgb = c.take(4)?;
                        styles.palette[i] = (rgb[0], rgb[1], rgb[2]);
                    }
                }
                0x003D => {
                    c.take(8)?;
                    let grbit = c.u16()?;
                    itab_cur = c.u16()? as usize;
                    let first = c.u16()?;
                    c.u16()?;
                    let ratio = c.u16()?;
                    // Hidden or minimized, scroll bars or sheet tabs
                    // hidden, no AutoFilter date grouping, the first tab
                    // shown, the tab bar's width: other than Excel's.
                    if grbit & 0x007B != 0x0038 || first != 0 || ratio != 600 {
                        book.dropped.insert("workbook window settings");
                    }
                }
                // HIDEOBJ: objects shown as placeholders, or hidden.
                0x008D if c.u16()? != 0 => {
                    book.dropped.insert("workbook window settings");
                }
                // BACKUP: Always create backup.
                0x0040 if c.u16()? != 0 => {
                    book.dropped.insert("backup copy on save");
                }
                // PROT4REV: sharing with track changes protected.
                0x01AF if c.u16()? != 0 => {
                    book.dropped.insert("workbook protection");
                }
                // XFCRC: how many XFs the XFEXTs were written for.
                0x087C => {
                    c.take(14)?;
                    styles.xf_count = Some(c.u16()?);
                }
                // XFEXT: an XF's exact and theme colours.
                0x087D => {
                    c.take(14)?;
                    let ixfe = c.u16()?;
                    c.u16()?;
                    let cexts = c.u16()?;
                    styles.ext.insert(ixfe, XfExt::read(&mut c, cexts));
                }
                // BOOKBOOL: external links' values not saved
                // (fNoSaveSup), or their update not left to the prompt
                // (grUpdateLinks).
                0x00DA => {
                    let f = c.u16()?;
                    if f & 0x0001 != 0 || f & 0x0060 != 0 {
                        book.dropped.insert("external link settings");
                    }
                }
                // BOOKEXT: no AutoRecover (fDontAutoRecover), or personal
                // information removed on save (fFilterPrivacy).
                0x0863 => {
                    c.take(16)?;
                    if c.u32()? & 0x0005 != 0 {
                        book.dropped.insert("workbook options");
                    }
                }
                // COMPAT12: no Compatibility Checker on save.
                0x088C => {
                    c.take(12)?;
                    if c.u32()? != 0 {
                        book.dropped.insert("workbook options");
                    }
                }
                // USERBVIEW: a custom view.
                0x01A9 => {
                    book.dropped.insert("custom views");
                }
                // STYLE: a cell style of the workbook's own; the import
                // keeps the cells' formats alone. Excel 2007's built-in
                // styles (Accent1, Good, ...) have fBuiltIn clear, and a
                // STYLEEXT after them that says they're built in.
                0x0293 if c.u16()? & 0x8000 == 0 => {
                    let builtin = recs
                        .get(k + 1)
                        .filter(|n| n.ty == 0x0892)
                        .and_then(|n| n.data.get(12))
                        .is_some_and(|f| f & 0x01 != 0);
                    if !builtin {
                        book.dropped.insert("cell styles");
                    }
                }
                0x0012 if c.u16()? != 0 => {
                    book.dropped.insert("workbook protection");
                }
                // FILESHARING: read-only recommended, or a password to
                // modify.
                0x005B if c.u16()? != 0 || c.u16()? != 0 => {
                    book.dropped
                        .insert("password to modify and read-only recommendation");
                }
                0x0019 if c.u16()? != 0 => {
                    book.dropped.insert("workbook protection");
                }
                // PRECISION 0: Set precision as displayed.
                0x000E if c.u16()? == 0 => {
                    book.dropped.insert("precision as displayed");
                }
                // SUPBOOK: the book takes its place before the rest is
                // read, so a record cut short doesn't shift the books after
                // it.
                0x01AE => {
                    g.books.push(SupBook {
                        kind: Book::External(None),
                        names: Vec::new(),
                        lib: None,
                    });
                    let ctab = c.u16()?;
                    let cch = c.u16()?;
                    match cch {
                        0x0401 => g.books.last_mut()?.kind = Book::Own,
                        0x3A01 => g.books.last_mut()?.kind = Book::AddIn,
                        _ => {
                            // virtPath (XLUnicodeStringNoCch, `cch` characters),
                            // then `ctab` sheet names.
                            let high = c.u8()? & 1 == 1;
                            let file = library_file(&c.chars(cch as usize, high)?)?;
                            let mut sheets = Vec::new();
                            for _ in 0..ctab {
                                sheets.push(c.xl_string()?);
                            }
                            g.books.last_mut()?.lib = Some((file, sheets));
                        }
                    }
                }
                // EXTERNNAME: likewise its slot first. An add-in book's
                // (AddinUdf) is a function. A Library book's is one only
                // with no flags and no definition (`cce` 0, as Excel writes
                // EUROCONVERT), as the `.xlsb` reader judges them.
                0x0023 => {
                    let b = g.books.last_mut()?;
                    b.names.push(None);
                    let flags = c.u16()?;
                    c.u32()?;
                    let name = c.short_string()?;
                    let kept = match b.kind {
                        Book::AddIn => Some(ftab::xll_name(&name)),
                        Book::External(_) => {
                            (b.lib.is_some() && flags == 0 && c.u16()? == 0).then_some(name)
                        }
                        Book::Own => None,
                    };
                    *b.names.last_mut()? = kept;
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
                    // A hidden defined name (not a function's, nor a
                    // built-in one) comes back visible.
                    if flags & 0x01 != 0 && flags & 0x2E == 0 {
                        book.dropped
                            .insert("hidden defined names (saved as visible)");
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
                ty if GLOBALS_KNOWN.contains(&ty) => {}
                ty => {
                    book.unknown_records.insert(ty);
                }
            }
            Some(())
        })();
        if r.ty == 0x002F {
            return Err(OpenError::EncryptedXls);
        }
    }

    // Links are numbered in the order of the books that have one: Library
    // books with a function name, as the `.xlsb` reader numbers them.
    for b in &mut g.books {
        let (Book::External(k), Some((file, sheets))) = (&mut b.kind, b.lib.take()) else {
            continue;
        };
        if !b.names.iter().any(Option::is_some) {
            continue;
        }
        if book.external_links.len() >= book.limits.links {
            return Err(OpenError::Corrupt(format!(
                "too many external links (more than {})",
                book.limits.links
            )));
        }
        book.external_links.push(ExternalLink {
            book: "rId1".into(),
            rels: vec![(
                "rId1".into(),
                XL_LIBRARY.into(),
                file,
                Some("External".into()),
            )],
            sheets,
            names: b.names.clone(),
        });
        *k = Some(book.external_links.len() - 1);
    }

    if rich {
        book.dropped.insert("formatting within cell text");
    }

    // Worksheets, in BOUNDSHEET8 order; `imported[i]` is the model index
    // of BOUNDSHEET8 `i`, for sheet-scoped names.
    let mut imported: Vec<Option<usize>> = Vec::new();
    let index: HashMap<usize, usize> = recs.iter().enumerate().map(|(i, r)| (r.pos, i)).collect();
    // A substream two BOUNDSHEET8s point at is read once (a crafted file
    // could otherwise multiply one sheet's cells by its sheet count).
    let mut seen: HashSet<usize> = HashSet::new();
    for (i, &(pos, dt, hidden)) in plies.iter().enumerate() {
        let start = index.get(&pos).copied().filter(|_| seen.insert(pos));
        let is_sheet = dt == 0
            && start
                .and_then(|s| recs[s].data.get(2..4))
                .is_some_and(|d| u16::from_le_bytes([d[0], d[1]]) == 0x0010);
        let (Some(start), true) = (start, is_sheet) else {
            book.dropped.insert(match dt {
                0x01 => "Excel 4.0 macro sheets",
                0x02 => "chart sheets",
                0x06 => "VB modules",
                _ => "sheets that are not worksheets",
            });
            imported.push(None);
            continue;
        };
        let mut sheet = read_sheet(&recs[start + 1..], &g, &sst, &mut book, &mut styles)?;
        if i == itab_cur {
            book.active = book.sheets.len();
        }
        imported.push(Some(book.sheets.len()));
        sheet.name = g.sheets[i].clone();
        sheet.hidden = hidden;
        book.push_sheet(sheet)?;
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
                None => {
                    book.unread_names += 1;
                    continue;
                }
            },
        };
        let Some(formula) = ptg::decompile(Biff::V8, &n.rgce, &n.extra, Base::Cell(None), &g)
        else {
            book.unread_names += 1;
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

/// One worksheet substream (the records after its BOF) to a sheet: its
/// cells and layout.
fn read_sheet(
    recs: &[Rec],
    g: &Globals,
    sst: &[String],
    book: &mut BookIn,
    styles: &mut Styles,
) -> Result<SheetIn, OpenError> {
    let mut sheet = SheetIn::default();
    let mut frozen = false;
    let (mut iteration, mut calc_count, mut delta) = (None, None, None);
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

    let mut put = |cells: &mut BTreeMap<(u32, u32), Cell>,
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
                style: styles.of(ixfe, book),
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
                book.dropped.insert("pictures, shapes and charts");
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
        // HLINK (#1726): one the reader can't read is left out and named. A
        // range spreads its target over its cells: charged as repeats, so a
        // small file can't expand without bound.
        if r.ty == super::hlink::HLINK {
            match super::hlink::parse(&r.data) {
                Some(link) => {
                    let copies = crate::sheet::hyperlink_cells(link.0).count() - 1;
                    book.charge_repeats(copies, copies.saturating_mul(link.1.len()))?;
                    sheet.hyperlinks.push(link);
                }
                None => {
                    book.dropped.insert("hyperlinks");
                }
            }
            continue;
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
                    } else {
                        match ptg::decompile(Biff::V8, rgce, extra, Base::Cell(Some(at)), g) {
                            Some(f) => cells.get_mut(&at)?.formula = Some(f),
                            None if cells.contains_key(&at) => book.unread_formulas += 1,
                            None => {}
                        }
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
                0x00E5 => {
                    let n = c.u16()?;
                    for _ in 0..n {
                        let (r1, r2, c1, c2) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
                        let (r1, r2, c1, c2) = (r1 as u32, r2 as u32, c1 as u32, c2 as u32);
                        if r2 >= r1 && c2 >= c1 && on_grid(r2, c2) {
                            sheet.merges.push((r1, c1, r2, c2));
                        }
                    }
                }
                0x007D => {
                    let (first, last, coldx) = (c.u16()?, c.u16()?, c.u16()?);
                    let ixfe = c.u16()?;
                    let grbit = c.u16()?;
                    let hidden = grbit & 0x0001 != 0;
                    // The width whether or not fUserSet says it was set (as
                    // LibreOffice reads it: other writers leave the flag
                    // clear), unless it is the sheet's default: its
                    // STANDARDWIDTH, or else DEFCOLWIDTH's characters plus
                    // the padding (Excel writes 8 characters as 2194).
                    let default =
                        is_default_coldx(sheet.base_col_width, sheet.default_col_width, coldx);
                    let width = (grbit & 0x0002 != 0 || !default).then_some(coldx as f64 / 256.0);
                    if ixfe != 15 {
                        book.dropped.insert("column and row formats");
                    }
                    if grbit & 0x0700 != 0 {
                        book.dropped.insert("outlines (grouped rows and columns)");
                    }
                    let last = (last as u32).min(crate::sheet::MAX_COLS - 1);
                    if (width.is_some() || hidden) && first as u32 <= last {
                        // fUserSet clear on a width: xlsx's plain width,
                        // without customWidth.
                        let plain = width.is_some() && grbit & 0x0002 == 0;
                        sheet.cols.push((first as u32, last, width, hidden, plain));
                    }
                }
                0x0208 => {
                    let row = c.u16()?;
                    c.take(4)?;
                    let miy = c.u16()?;
                    c.take(4)?;
                    let grbit = c.u16()?;
                    let ixfe = c.u16().unwrap_or(15) & 0x0FFF;
                    let hidden = grbit & 0x0020 != 0;
                    let height = (grbit & 0x0040 != 0).then_some((miy & 0x7FFF) as f64 / 20.0);
                    // fGhostDirty: the row has a format of its own.
                    if grbit & 0x0080 != 0 && ixfe != 15 {
                        book.dropped.insert("column and row formats");
                    }
                    if grbit & 0x0007 != 0 {
                        book.dropped.insert("outlines (grouped rows and columns)");
                    }
                    if height.is_some() || hidden {
                        sheet.rows.push((row as u32, height, hidden));
                    }
                }
                0x023E => {
                    let grbit = c.u16()?;
                    frozen = grbit & 0x0008 != 0;
                    // View settings the model lacks: formulas shown,
                    // gridlines, headings or zeros hidden, right to left.
                    // Also a non-default header colour or outline symbols
                    // hidden.
                    if grbit & 0x0001 != 0
                        || grbit & 0x0016 != 0x0016
                        || grbit & 0x0040 != 0
                        || grbit & 0x00A0 != 0x00A0
                        || grbit & 0x0800 != 0
                    {
                        book.dropped.insert("sheet view settings");
                    }
                }
                // CALCMODE: manual (0), or automatic except data tables (-1).
                0x000D => match c.u16()? {
                    0 => {
                        book.dropped.insert("manual calculation");
                    }
                    0xFFFF => {
                        book.dropped
                            .insert("automatic calculation except data tables");
                    }
                    _ => {}
                },
                // DEFAULTROWHEIGHT, DEFCOLWIDTH, STANDARDWIDTH.
                0x0225 => {
                    // fDyZero: rows are hidden by default, and the height
                    // is a hidden row's.
                    if c.u16()? & 0x0002 != 0 {
                        book.dropped.insert("rows hidden by default");
                    } else {
                        sheet.default_row_height = Some(c.u16()? as f64 / 20.0);
                    }
                }
                // REFMODE 0: R1C1 references.
                0x000F if c.u16()? == 0 => {
                    book.dropped.insert("R1C1 reference style");
                }
                // HEADERFOOTER: different odd and even or first page
                // headers, their text, or headers that don't scale with
                // the document or align with its margins.
                0x089C => {
                    c.take(28)?;
                    let flags = c.u16()?;
                    let cch = [c.u16()?, c.u16()?, c.u16()?, c.u16()?];
                    if flags & 0x0003 != 0
                        || flags & 0x000C != 0x000C
                        || cch.iter().any(|&n| n != 0)
                    {
                        book.dropped.insert("page setup");
                    }
                }
                // SAVERECALC 0: no recalculation before saving.
                0x005F if c.u16()? == 0 => {
                    book.dropped.insert("workbook options");
                }
                // HFPICTURE: the sheet's header and footer pictures.
                0x0866 => {
                    book.dropped.insert("header and footer pictures");
                }
                // USERSVIEWBEGIN: a custom view's settings for the sheet.
                0x01AA => {
                    book.dropped.insert("custom views");
                }
                // FEAT: protected ranges (isf 2), ignored errors (3),
                // smart tags (4).
                0x0868 => {
                    c.take(12)?;
                    match c.u16()? {
                        2 => book.dropped.insert("protected ranges"),
                        3 => book.dropped.insert("ignored errors"),
                        4 => book.dropped.insert("smart tags"),
                        _ => false,
                    };
                }
                // FEATURE11: a table (Excel 2003's list).
                0x0872 => {
                    book.dropped
                        .insert("tables (their cells are kept as plain ranges)");
                }
                // PLV: Page Layout view.
                0x088B => {
                    c.take(14)?;
                    if c.u16()? & 0x0001 != 0 {
                        book.dropped.insert("sheet view settings");
                    }
                }
                0x0055 => sheet.base_col_width = Some(c.u16()? as u32),
                0x0099 => sheet.default_col_width = Some(c.u16()? as f64 / 256.0),
                // SCL: a zoom other than 100%.
                0x00A0 if c.u16()? != c.u16()? => {
                    book.dropped.insert("sheet view settings");
                }
                0x00B0 => {
                    book.dropped.insert("PivotTables");
                }
                0x00E9 => {
                    book.dropped.insert("sheet backgrounds");
                }
                // SHEETEXT: a tab colour other than automatic (0x7F).
                0x0862 => {
                    c.take(12)?;
                    if c.u32()? >= 20 && c.u32()? & 0x7F != 0x7F {
                        book.dropped.insert("sheet tab colors");
                    }
                }
                0x00AE => {
                    book.dropped.insert("scenarios");
                }
                0x01AD => {
                    book.dropped.insert("external data ranges");
                }
                // PRINTHEADERS, PRINTGRIDLINES, HCENTER, VCENTER.
                0x002A | 0x002B | 0x0083 | 0x0084 if c.u16()? != 0 => {
                    book.dropped.insert("page setup");
                }
                // Iterative calculation, the same in every sheet's block.
                0x0011 => iteration = Some(c.u16()? != 0),
                0x000C => calc_count = Some(c.u16()?),
                0x0010 => delta = Some(c.f64()?),
                0x0041 if frozen => {
                    let (x, y) = (c.u16()?, c.u16()?);
                    sheet.freeze = (y as u32, x as u32);
                }
                // A PANE under an unfrozen WINDOW2: a split window.
                0x0041 => {
                    book.dropped.insert("sheet view settings");
                }
                0x005D | 0x00EC => {
                    book.dropped.insert("pictures, shapes and charts");
                }
                0x001C => {
                    book.dropped.insert("comments");
                }
                0x01B0 | 0x01B1 | 0x0879 | 0x087A => {
                    book.dropped.insert("conditional formatting");
                }
                0x01B2 | 0x01BE => {
                    book.dropped.insert("data validation");
                }
                super::hlink::HLINK_TOOLTIP => {
                    book.dropped.insert("hyperlink ScreenTips");
                }
                0x009D => {
                    book.dropped.insert("AutoFilter");
                }
                0x0236 => {
                    book.dropped.insert("data tables");
                }
                0x0012 if c.u16()? != 0 => {
                    book.dropped.insert("sheet protection");
                }
                0x001A | 0x001B if c.u16()? != 0 => {
                    book.dropped.insert("page breaks");
                }
                0x0014 | 0x0015 if !r.data.is_empty() => {
                    book.dropped.insert("page setup");
                }
                0x0081 if c.u16()? & 0x0100 != 0 => {
                    book.dropped.insert("page setup");
                }
                0x00A1 => {
                    c.u16()?;
                    let scale = c.u16()?;
                    c.take(6)?;
                    let grbit = c.u16()?;
                    c.take(4)?;
                    let (header, footer) = (c.f64()?, c.f64()?);
                    // fPortrait off is landscape, unless fNoPls or
                    // fNoOrient says there is no orientation; the header and
                    // footer margins count only with the printer settings.
                    let landscape = grbit & 0x0046 == 0;
                    let set = grbit & 0x0004 == 0;
                    // Over then down, black and white, draft, comments,
                    // a first page number.
                    let options = grbit & 0x00B9 != 0;
                    if (set
                        && (scale != 100 || options || !near(header, 0.3) || !near(footer, 0.3)))
                        || landscape
                    {
                        book.dropped.insert("page setup");
                    }
                }
                // LEFTMARGIN, RIGHTMARGIN, TOPMARGIN, BOTTOMMARGIN: other
                // than Excel's Normal margins.
                0x0026..=0x0029 => {
                    let normal = if r.ty <= 0x0027 { 0.7 } else { 0.75 };
                    if !near(c.f64()?, normal) {
                        book.dropped.insert("page setup");
                    }
                }
                ty if SHEET_KNOWN.contains(&ty) => {}
                ty => {
                    book.unknown_records.insert(ty);
                }
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
                let Some(cell) = cells.get_mut(&p.at) else {
                    continue;
                };
                let kept = ptg::decompile(Biff::V8, rgce, extra, Base::Cell(Some(p.at)), g)
                    .is_some_and(|f| set_array(cell, *range, f));
                book.unread_formulas += usize::from(!kept);
            }
            continue;
        }
        if !cells.contains_key(&p.at) {
            continue;
        }
        // A cell outside the group's stated range still takes it: writers
        // get the range wrong, and the tokens are relative to the cell.
        let base = Base::Shared(p.at.0, p.at.1);
        let f = shared
            .get(&p.master)
            .and_then(|(_, rgce, extra)| ptg::decompile(Biff::V8, rgce, extra, base, g));
        match (f, cells.get_mut(&p.at)) {
            (Some(f), Some(cell)) => cell.formula = Some(f),
            _ => book.unread_formulas += 1,
        }
    }
    sheet.cells = cells;
    if book.sheets.is_empty() && iteration == Some(true) {
        book.iterate = Some((calc_count.unwrap_or(100) as u32, delta.unwrap_or(0.001)));
    }
    Ok(sheet)
}

/// Whether a COLINFO width is the sheet's default: its STANDARDWIDTH, or
/// else DEFCOLWIDTH's characters plus the padding (Excel writes 8
/// characters as 2194). The reader drops such a width unless fUserSet
/// keeps it, and the writer sets fUserSet for one (#1658).
pub(super) fn is_default_coldx(
    base_col_width: Option<u32>,
    default_col_width: Option<f64>,
    coldx: u16,
) -> bool {
    let base = base_col_width.unwrap_or(8).min(255) * 256;
    match default_col_width {
        Some(w) => (coldx as f64 - w * 256.0).abs() < 1.0,
        None => (base..base + 256).contains(&(coldx as u32)),
    }
}

/// Whether two margins (inches) are the same, as Excel rounds them.
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
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
        f.push(b'x');
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

    /// An XF record: font, format, alignment byte, border word, fill.
    fn xf_rec(ifnt: u16, align: u8, dg: u32, fill: u32) -> Vec<u8> {
        let mut b = ifnt.to_le_bytes().to_vec();
        b.extend([0, 0, 1, 0, align, 0, 0, 0xFC]);
        b.extend(dg.to_le_bytes());
        b.extend(fill.to_le_bytes());
        b.extend([0x40, 0x20]);
        rec(0x00E0, &b)
    }

    fn font_rec(bls: u16, uls: u8, grbit: u16) -> Vec<u8> {
        let mut b = 220u16.to_le_bytes().to_vec();
        b.extend(grbit.to_le_bytes());
        b.extend(0x7FFFu16.to_le_bytes());
        b.extend(bls.to_le_bytes());
        b.extend([0, 0, uls, 0, 0, 0]);
        b.extend(short("Calibri"));
        rec(0x0031, &b)
    }

    /// What the model can't hold of a cell's format is named, only for the
    /// formats cells use; what it can is read.
    #[test]
    fn formats_the_model_lacks_are_dropped_by_name() {
        let fonts: Vec<Vec<u8>> = vec![
            font_rec(400, 0, 0),
            font_rec(400, 0, 0),
            font_rec(400, 0, 0),
            font_rec(400, 0, 0),
            // Font 5: bold, underlined, struck through.
            font_rec(700, 1, 0x0008),
        ];
        let mut globals = fonts;
        // XF 0: plain. XF 1: font 5, centred, wrap, top-aligned, bottom
        // border only, a gray-50% pattern. XF 2: an unused format with
        // rotated text.
        globals.push(xf_rec(0, 0x20, 0, 0));
        globals.push(xf_rec(5, 0x02 | 0x08, 0x1000, 2 << 26));
        let mut rot = xf_rec(0, 0x20, 0, 0);
        rot[4 + 7] = 45;
        globals.push(rot);
        let mut d = cell(0, 0, 1);
        d.extend(1.0f64.to_le_bytes());
        let stream = workbook(&globals, &[rec(0x0203, &d), rec(0x001C, &[0; 12])]);
        let book = read(&stream).unwrap();
        let xf = &book.xfs[data(&book)[&(0, 0)].style as usize];
        assert!(xf.bold && xf.wrap && !xf.border);
        assert_eq!(xf.align, Align::Center);
        assert_eq!(
            book.dropped.iter().copied().collect::<Vec<_>>(),
            [
                "borders other than a thin box",
                "comments",
                "patterned fills",
                "strikethrough",
                "underlined text",
                "vertical alignment"
            ]
        );
    }

    /// HLINK records are read; one the reader can't read is named, and so
    /// is a ScreenTip (#1726).
    #[test]
    fn unreadable_hlink_is_dropped_by_name() {
        let good = rec(
            0x01B8,
            &super::super::hlink::body((0, 0, 1, 1), "https://x/"),
        );
        let book = read(&workbook(&[], &[number(0, 0, 1.0), good.clone()])).unwrap();
        assert_eq!(
            book.sheets[0].hyperlinks,
            [((0, 0, 1, 1), "https://x/".to_string())]
        );
        assert!(book.dropped.is_empty(), "{:?}", book.dropped);
        let mut reversed = super::super::hlink::body((0, 0, 1, 1), "https://y/");
        reversed[0] = 5;
        for bad in [vec![0u8; 3], vec![0u8; 40], reversed] {
            let book = read(&workbook(&[], &[good.clone(), rec(0x01B8, &bad)])).unwrap();
            assert_eq!(book.sheets[0].hyperlinks.len(), 1);
            assert_eq!(
                book.dropped.iter().copied().collect::<Vec<_>>(),
                ["hyperlinks"]
            );
            assert!(book.unknown_records.is_empty());
        }
    }

    #[test]
    fn hlink_spread_is_charged_as_repeats() {
        // Two 64x64 ranges: 2 x 4,095 copies of the target past the first
        // cell each.
        let link = |r: u32| {
            rec(
                0x01B8,
                &super::super::hlink::body((r, 0, r + 63, 63), "https://x/"),
            )
        };
        let stream = workbook(&[], &[link(0), link(100)]);
        let tight = Limits {
            repeat_cells: 8_000,
            ..Limits::default()
        };
        let err = read_with(&stream, tight).err().unwrap();
        assert!(err.to_string().contains("too many repeated cells"), "{err}");
        let tight_bytes = Limits {
            repeat_bytes: 50_000,
            ..Limits::default()
        };
        assert!(read_with(&stream, tight_bytes).is_err());
        let book = read(&stream).unwrap();
        assert_eq!(book.sheets[0].hyperlinks.len(), 2);
    }

    /// A column link and a later link on its anchor: the model holds the
    /// anchor's own, and a save writes that alone (the column, which the
    /// model holds only by its anchor, is not written under it).
    #[test]
    fn a_later_link_on_a_range_anchor_breaks_the_range() {
        let hlink = |rect, t| rec(0x01B8, &super::super::hlink::body(rect, t));
        let stream = workbook(
            &[],
            &[
                hlink((0, 2, 65_535, 2), "https://x/"),
                hlink((0, 2, 0, 2), "https://y/"),
            ],
        );
        let pkg = read(&stream).unwrap().build();
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.hyperlinks.len(), 1);
        assert_eq!(s.hyperlinks[&(0, 2)], "https://y/");
        assert_eq!(s.hyperlink_refs[&(0, 2)].target, "https://x/");
        let out = crate::legacy::xls_write::write_xls(&pkg);
        let (back, _) = crate::legacy::open_workbook(&out.bytes).unwrap();
        let b = &back.workbook.sheets[0];
        assert!(b.hyperlink_refs.is_empty());
        assert_eq!(b.hyperlinks, s.hyperlinks);
        let xlsx = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        assert!(
            xlsx.workbook.sheets[0]
                .hyperlink_refs
                .values()
                .all(|l| l.now == (0, 2, 0, 2))
        );
        assert_eq!(xlsx.workbook.sheets[0].hyperlinks, s.hyperlinks);
    }

    /// The trace of #1726 r5: HLINK C:C -> x, then C1 -> y; deleting row 1
    /// re-anchors the column on the new C1, which takes the range's own x,
    /// not the deleted cell's y.
    #[test]
    fn a_reanchored_range_keeps_its_own_target() {
        let hlink = |rect, t| rec(0x01B8, &super::super::hlink::body(rect, t));
        let stream = workbook(
            &[],
            &[
                hlink((0, 2, 65_535, 2), "https://x/"),
                hlink((0, 2, 0, 2), "https://y/"),
            ],
        );
        let mut pkg = read(&stream).unwrap().build();
        crate::edit::delete_rows(&mut pkg.workbook, 0, 0, 1);
        let s = &pkg.workbook.sheets[0];
        assert_eq!(s.hyperlinks.len(), 1);
        assert_eq!(s.hyperlinks[&(0, 2)], "https://x/");
        assert_eq!(s.hyperlink_refs[&(0, 2)].now, (0, 2, 65_534, 2));
        assert_eq!(s.hyperlink_refs[&(0, 2)].target, "https://x/");
        // Intact again: written as the one range.
        let out = crate::legacy::xls_write::write_xls(&pkg);
        let (back, _) = crate::legacy::open_workbook(&out.bytes).unwrap();
        assert_eq!(
            back.workbook.sheets[0].hyperlink_refs[&(0, 2)].now,
            (0, 2, 65_534, 2)
        );
    }

    #[test]
    fn hlink_tooltip_is_named() {
        let good = rec(0x01B8, &super::super::hlink::body((0, 0, 0, 0), "#Data!B2"));
        let mut tip = vec![0x00, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        tip.extend("tip\0".encode_utf16().flat_map(u16::to_le_bytes));
        let book = read(&workbook(&[], &[good, rec(0x0800, &tip)])).unwrap();
        assert_eq!(book.sheets[0].hyperlinks[0].1, "#Data!B2");
        assert_eq!(
            book.dropped.iter().copied().collect::<Vec<_>>(),
            ["hyperlink ScreenTips"]
        );
        assert!(book.unknown_records.is_empty());
    }

    /// A thin box is the model's border in any colour, but a colour other
    /// than black or automatic is named lost (#874 r9).
    #[test]
    fn a_coloured_thin_box_keeps_the_border_and_names_its_colour() {
        let boxed = |left: u32, right: u32, top: u32, bottom: u32| {
            let globals = vec![
                font_rec(400, 0, 0),
                xf_rec(0, 0x20, 0, 0),
                xf_rec(
                    0,
                    0x20,
                    0x1111 | left << 16 | right << 23,
                    top | bottom << 7,
                ),
            ];
            let mut d = cell(0, 0, 1);
            d.extend(1.0f64.to_le_bytes());
            let book = read(&workbook(&globals, &[rec(0x0203, &d)])).unwrap();
            let border = book.xfs[data(&book)[&(0, 0)].style as usize].border;
            (border, book.dropped.iter().copied().collect::<Vec<_>>())
        };
        // Black (palette 8 and fixed 0) and automatic (0x40) sides.
        assert_eq!(boxed(8, 0, 0x40, 0x7F), (true, vec![]));
        // A red side (palette 10).
        assert_eq!(
            boxed(8, 8, 10, 8),
            (true, vec!["borders other than a thin box"])
        );
    }

    /// DEFCOLWIDTH 255 makes the default band end past u16 (#874 r9).
    #[test]
    fn the_widest_defcolwidth_reads() {
        let mut b = Vec::new();
        for v in [0u16, 0, 65300, 15, 0, 0] {
            b.extend(v.to_le_bytes());
        }
        let book = read(&workbook(&[], &[rec(0x0055, &[255, 0]), rec(0x007D, &b)])).unwrap();
        assert!(book.sheets[0].cols.is_empty());
    }

    /// Margins are page setup the import leaves out: Excel's Normal ones
    /// are no loss, others are.
    #[test]
    fn custom_margins_are_dropped_page_setup() {
        let margin = |ty: u16, v: f64| rec(ty, &v.to_le_bytes());
        let normal = [
            margin(0x0026, 0.7),
            margin(0x0027, 0.7),
            margin(0x0028, 0.75),
            margin(0x0029, 0.75),
        ];
        let book = read(&workbook(&[], &normal)).unwrap();
        assert!(book.dropped.is_empty());
        let book = read(&workbook(&[], &[margin(0x0026, 1.0)])).unwrap();
        assert_eq!(
            book.dropped.iter().copied().collect::<Vec<_>>(),
            ["page setup"]
        );
    }

    /// A formula the reader can't read keeps its value and is counted: one
    /// whose 3-D reference names another workbook's sheet, one with a
    /// token it doesn't know; a defined name likewise.
    #[test]
    fn unread_formulas_and_names_are_counted() {
        let other = supbook(b"other.xls", &["Data"]);
        let xti = extern_sheet(&[(0, 0, 0)]);
        // ptgRef3d into the external book, and ptgSxName (0x18 + 0x01), which
        // isn't read.
        let ext = [0x3A, 0, 0, 0, 0, 0, 0];
        let unknown = [0x18, 0x01, 0, 0, 0, 0];
        let mut name = 0u16.to_le_bytes().to_vec();
        name.push(0);
        name.push(4);
        name.extend((ext.len() as u16).to_le_bytes());
        name.extend([0, 0, 0, 0, 0, 0, 0, 0, 0]);
        name.extend(b"Ext1");
        name.extend_from_slice(&ext);
        let stream = workbook(
            &[other, xti, rec(0x0018, &name)],
            &[formula(0, 0, 5.0, &ext), formula(1, 0, 6.0, &unknown)],
        );
        let cfb = opccore::cfb::write_cfb(&[("Workbook", stream)]);
        let o = crate::legacy::open_workbook_ex(&cfb).unwrap();
        assert_eq!(
            o.dropped,
            [
                "1 defined name that can't be read",
                "2 formulas that refer to other workbooks or can't be read (kept as values)"
            ]
        );
        let cells = &o.pkg.workbook.sheets[0].cells;
        assert_eq!(cells[&(0, 0)].value, CellValue::Number(5.0));
        assert_eq!(cells[&(0, 0)].formula, None);
    }

    /// What the model lacks of a sheet's views, PivotTables, scenarios,
    /// external data, print options and very hidden state is named.
    #[test]
    fn unmodelled_sheet_records_are_dropped_by_name() {
        let w2 = |grbit: u16| {
            let mut b = grbit.to_le_bytes().to_vec();
            b.extend([0; 16]);
            rec(0x023E, &b)
        };
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (w2(0x06B4), "sheet view settings"),
            (rec(0x00A0, &[3, 0, 4, 0]), "sheet view settings"),
            (rec(0x00B0, &[0; 4]), "PivotTables"),
            (rec(0x00AE, &[0; 4]), "scenarios"),
            (rec(0x01AD, &[0; 4]), "external data ranges"),
            (rec(0x002B, &[1, 0]), "page setup"),
            (rec(0x0083, &[1, 0]), "page setup"),
            (rec(0x000D, &[0, 0]), "manual calculation"),
            (w2(0x06B7), "sheet view settings"),
            (w2(0x06A6), "sheet view settings"),
            (w2(0x06B2), "sheet view settings"),
            (w2(0x06F6), "sheet view settings"),
            (w2(0x0636), "sheet view settings"),
            (w2(0x0696), "sheet view settings"),
            (
                rec(0x0041, &[0, 4, 0, 0, 0, 0, 0, 0, 3, 0]),
                "sheet view settings",
            ),
        ];
        for (r, want) in cases {
            let book = read(&workbook(&[], &[r])).unwrap();
            assert_eq!(book.dropped.iter().copied().collect::<Vec<_>>(), [want]);
        }
        // Excel's defaults are no loss.
        let plain = [
            w2(0x06B6),
            rec(0x00A0, &[1, 0, 1, 0]),
            rec(0x002B, &[0, 0]),
            rec(0x000D, &[1, 0]),
        ];
        assert!(read(&workbook(&[], &plain)).unwrap().dropped.is_empty());
        // SETUP's draft bit, with printer settings.
        let mut setup = vec![0u8; 34];
        setup[2] = 100;
        setup[10] = 0x12;
        setup[16..24].copy_from_slice(&0.3f64.to_le_bytes());
        setup[24..32].copy_from_slice(&0.3f64.to_le_bytes());
        let book = read(&workbook(&[], &[rec(0x00A1, &setup)])).unwrap();
        assert_eq!(
            book.dropped.iter().copied().collect::<Vec<_>>(),
            ["page setup"]
        );
        setup[10] = 0x02;
        assert!(
            read(&workbook(&[], &[rec(0x00A1, &setup)]))
                .unwrap()
                .dropped
                .is_empty()
        );
        // Precision as displayed (globals' PRECISION 0).
        let book = read(&workbook(&[rec(0x000E, &[0, 0])], &[])).unwrap();
        assert_eq!(
            book.dropped.iter().copied().collect::<Vec<_>>(),
            ["precision as displayed"]
        );
        assert!(
            read(&workbook(&[rec(0x000E, &[1, 0])], &[]))
                .unwrap()
                .dropped
                .is_empty()
        );
        // A very hidden sheet (BOUNDSHEET state 2).
        let mut stream = workbook(&[], &[]);
        let bs = stream.windows(2).position(|w| w == [0x85, 0x00]).unwrap();
        stream[bs + 8] = 2;
        let book = read(&stream).unwrap();
        assert_eq!(
            book.dropped.iter().copied().collect::<Vec<_>>(),
            ["very hidden sheets (saved as hidden)"]
        );
        assert!(book.sheets[0].hidden);
    }

    /// An embedded chart, and a VBA project's storage beside the stream.
    #[test]
    fn charts_and_vba_projects_are_dropped_by_name() {
        let stream = workbook(&[], &[bof(0x0020), rec(EOF, &[])]);
        let cfb = opccore::cfb::write_cfb_tree(&[
            opccore::cfb::Node::Stream("Workbook", stream),
            opccore::cfb::Node::Storage(
                "_VBA_PROJECT_CUR",
                vec![opccore::cfb::Node::Stream("PROJECT", vec![1, 2, 3])],
            ),
        ]);
        let opened = crate::legacy::open_workbook_ex(&cfb).unwrap();
        assert_eq!(
            opened.dropped,
            ["VB project", "pictures, shapes and charts"]
        );
    }

    /// Workbook and sheet settings the model has no place for (#874 r8).
    #[test]
    fn unmodelled_settings_are_dropped_by_name() {
        let name = |flags: u16, text: &str| {
            let mut n = flags.to_le_bytes().to_vec();
            n.extend([0, text.len() as u8, 0, 0]);
            n.extend([0; 9]);
            n.extend(text.as_bytes());
            rec(0x0018, &n)
        };
        let sheetext = |icv: u32| {
            let mut b = vec![0u8; 12];
            b.extend(20u32.to_le_bytes());
            b.extend(icv.to_le_bytes());
            rec(0x0862, &b)
        };
        let colinfo = |ixfe: u16| rec(0x007D, &[0, 0, 0, 0, 0x92, 0x08, ixfe as u8, 0, 0, 0, 0, 0]);
        let row = |grbit: u16, ixfe: u16| {
            let mut b = vec![0u8; 12];
            b[6] = 0x2C;
            b[7] = 0x01;
            b.extend(grbit.to_le_bytes());
            b.extend(ixfe.to_le_bytes());
            rec(0x0208, &b)
        };
        let globals: Vec<(Vec<u8>, &str)> = vec![
            (
                rec(0x005B, &[1, 0, 0, 0, 0, 0]),
                "password to modify and read-only recommendation",
            ),
            (
                rec(0x005B, &[0, 0, 0x34, 0x12, 0, 0]),
                "password to modify and read-only recommendation",
            ),
            (rec(0x0019, &[1, 0]), "workbook protection"),
            (
                name(0x01, "Secret"),
                "hidden defined names (saved as visible)",
            ),
        ];
        for (r, want) in globals {
            let book = read(&workbook(&[r], &[])).unwrap();
            assert_eq!(book.dropped.iter().copied().collect::<Vec<_>>(), [want]);
        }
        let sheet: Vec<(Vec<u8>, &str)> = vec![
            (rec(0x00E9, &[9, 0, 1, 0]), "sheet backgrounds"),
            (sheetext(10), "sheet tab colors"),
            (colinfo(17), "column and row formats"),
            (row(0x0180, 17), "column and row formats"),
        ];
        for (r, want) in sheet {
            let book = read(&workbook(&[], &[r])).unwrap();
            assert_eq!(book.dropped.iter().copied().collect::<Vec<_>>(), [want]);
        }
        // Their defaults are no loss: no share settings, no protection, a
        // hidden function name, an automatic tab colour, the default xf.
        let plain_globals = [
            rec(0x005B, &[0; 6]),
            rec(0x0019, &[0, 0]),
            name(0x03, "_xlfn.MAXIFS"),
        ];
        let plain_sheet = [
            sheetext(0x7F),
            colinfo(15),
            row(0x0100, 15),
            row(0x0100, 17),
        ];
        assert!(
            read(&workbook(&plain_globals, &plain_sheet))
                .unwrap()
                .dropped
                .is_empty()
        );
    }

    /// Every record is read, known to hold nothing a person sees, named, or
    /// named by id as "other features" (#874 r11).
    #[test]
    fn every_record_is_read_known_or_named() {
        let dropped = |globals: &[Vec<u8>], sheet: &[Vec<u8>]| {
            let book = read(&workbook(globals, sheet)).unwrap();
            let mut d: Vec<String> = book.dropped.iter().map(|s| s.to_string()).collect();
            if !book.unknown_records.is_empty() {
                d.push(format!("{:04X?}", book.unknown_records));
            }
            d
        };
        let window1 = |grbit: u16, first: u16, ratio: u16| {
            let mut b = Vec::new();
            for v in [0u16, 0, 0x3FCF, 0x2A4E, grbit, 0, first, 1, ratio] {
                b.extend(v.to_le_bytes());
            }
            rec(0x003D, &b)
        };
        let style = |ixfe: u16| {
            let mut b = ixfe.to_le_bytes().to_vec();
            b.extend([3, 0, 0]);
            b.extend(b"Own");
            rec(0x0293, &b)
        };
        let style_ext = |builtin: bool| {
            let mut b = vec![0u8; 12];
            b.extend([u8::from(builtin), 0, 0, 0]);
            rec(0x0892, &b)
        };
        let frt = |ty: u16, tail: &[u8]| {
            let mut b = vec![0u8; 12];
            b.extend_from_slice(tail);
            rec(ty, &b)
        };
        let globals: Vec<(Vec<Vec<u8>>, &str)> = vec![
            (vec![window1(0x0018, 0, 600)], "workbook window settings"),
            (vec![window1(0x0038, 2, 600)], "workbook window settings"),
            (vec![window1(0x0038, 0, 300)], "workbook window settings"),
            (vec![rec(0x008D, &[2, 0])], "workbook window settings"),
            (vec![rec(0x0040, &[1, 0])], "backup copy on save"),
            (vec![rec(0x01AF, &[1, 0])], "workbook protection"),
            (vec![rec(0x01A9, &[0; 8])], "custom views"),
            (vec![style(0x0010)], "cell styles"),
            (vec![style(0x0010), style_ext(false)], "cell styles"),
        ];
        for (g, want) in globals {
            assert_eq!(dropped(&g, &[]), [want], "{want}");
        }
        let sheet: Vec<(Vec<u8>, &str)> = vec![
            (rec(0x01AA, &[0; 8]), "custom views"),
            (frt(0x0868, &[2, 0]), "protected ranges"),
            (frt(0x0868, &[3, 0]), "ignored errors"),
            (
                frt(0x0872, &[0; 8]),
                "tables (their cells are kept as plain ranges)",
            ),
            (frt(0x088B, &[100, 0, 1, 0]), "sheet view settings"),
            (rec(0x000F, &[0, 0]), "R1C1 reference style"),
            (rec(0x0225, &[2, 0, 0, 0]), "rows hidden by default"),
        ];
        for (r, want) in sheet {
            assert_eq!(dropped(&[], &[r]), [want], "{want}");
        }
        // WINDOW2's page break preview.
        let mut w2 = 0x08B6u16.to_le_bytes().to_vec();
        w2.extend([0; 16]);
        assert_eq!(dropped(&[], &[rec(0x023E, &w2)]), ["sheet view settings"]);
        // fDyZero: the height is a hidden row's, not the default.
        let book = read(&workbook(&[], &[rec(0x0225, &[2, 0, 0, 0])])).unwrap();
        assert_eq!(book.sheets[0].default_row_height, None);
        // Records nobody classified, in the globals and in a sheet.
        assert_eq!(
            dropped(&[rec(0x7777, &[1])], &[rec(0x7778, &[])]),
            ["{7777, 7778}"]
        );
        // Excel's defaults and its bookkeeping name nothing.
        let plain_globals = [
            window1(0x0038, 0, 600),
            rec(0x008D, &[0, 0]),
            rec(0x0040, &[0, 0]),
            rec(0x01AF, &[0, 0]),
            rec(0x0042, &[0xB0, 0x04]),
            style(0x8000),
            style(0x0010),
            style_ext(true),
            frt(0x0896, &[0; 4]),
        ];
        let plain_sheet = [
            rec(0x000F, &[1, 0]),
            rec(0x0225, &[0, 0, 0x2C, 0x01]),
            frt(0x088B, &[100, 0, 0, 0]),
            frt(0x0867, &[0; 4]),
            rec(0x0200, &[0; 14]),
        ];
        assert!(dropped(&plain_globals, &plain_sheet).is_empty());
    }

    /// XFEXT's exact colours replace the palette's nearest; a theme colour
    /// (other than the default text's) and a gradient are named; a
    /// malformed one is ignored (#874 r12).
    #[test]
    fn xfext_colours_are_carried_or_named() {
        let color = |ty: u16, kind: u16, tint: i16, value: u32| {
            let mut b = ty.to_le_bytes().to_vec();
            b.extend(20u16.to_le_bytes());
            b.extend(kind.to_le_bytes());
            b.extend(tint.to_le_bytes());
            b.extend(value.to_le_bytes());
            b.extend([0; 8]);
            b
        };
        let xfext = |props: &[Vec<u8>], cexts: u16| {
            let mut b = vec![0u8; 12];
            for v in [0u16, 1, 0, cexts] {
                b.extend(v.to_le_bytes());
            }
            for p in props {
                b.extend(p);
            }
            rec(0x087D, &b)
        };
        // XF 1: a solid fill in palette red (10), text in palette blue (12).
        let mut xf1 = xf_rec(1, 0x20, 0, 1 << 26);
        xf1[4 + 18] = 10;
        xf1[4 + 19] = 0;
        let fonts = vec![font_rec(400, 0, 0), {
            let mut f = font_rec(400, 0, 0);
            f[4 + 4] = 12;
            f[4 + 5] = 0;
            f
        }];
        let xfcrc = |cxfs: u16| {
            let mut b = vec![0u8; 14];
            b.extend(cxfs.to_le_bytes());
            b.extend([0; 4]);
            rec(0x087C, &b)
        };
        let read_crc = |crc: Option<Vec<u8>>, ext: Option<Vec<u8>>| {
            let mut globals = fonts.clone();
            globals.push(xf_rec(0, 0x20, 0, 0));
            globals.push(xf1.clone());
            globals.extend(crc);
            globals.extend(ext);
            let mut d = cell(0, 0, 1);
            d.extend(1.0f64.to_le_bytes());
            let book = read(&workbook(&globals, &[rec(0x0203, &d)])).unwrap();
            let xf = book.xfs[data(&book)[&(0, 0)].style as usize].clone();
            let dropped: Vec<&str> = book.dropped.iter().copied().collect();
            (xf.fill, xf.color, dropped)
        };
        let read_with_ext = |ext: Option<Vec<u8>>| read_crc(Some(xfcrc(2)), ext);
        let red = Some((0xFF, 0, 0));
        let blue = Some((0, 0, 0xFF));
        assert_eq!(read_with_ext(None), (red, blue, vec![]));
        // Exact RGB colours (LongRGBA: r, g, b, a).
        let ext = xfext(
            &[color(4, 2, 0, 0x00_563412), color(13, 2, 0, 0x00_ABCDEF)],
            2,
        );
        assert_eq!(
            read_with_ext(Some(ext)),
            (Some((0x12, 0x34, 0x56)), Some((0xEF, 0xCD, 0xAB)), vec![])
        );
        // The default text colour (theme 1, no tint) is no loss; another
        // theme colour is, as is a tinted one, and a gradient.
        assert_eq!(
            read_with_ext(Some(xfext(&[color(13, 3, 0, 1)], 1))).2,
            Vec::<&str>::new()
        );
        assert_eq!(
            read_with_ext(Some(xfext(&[color(4, 3, 0, 4)], 1))).2,
            ["theme colors"]
        );
        assert_eq!(
            read_with_ext(Some(xfext(&[color(13, 3, 0x2000, 1)], 1))).2,
            ["theme colors"]
        );
        let mut gradient = 6u16.to_le_bytes().to_vec();
        gradient.extend(8u16.to_le_bytes());
        gradient.extend([0; 4]);
        assert_eq!(
            read_with_ext(Some(xfext(&[gradient], 1))).2,
            ["patterned fills"]
        );
        // Malformed: a cb past the record, or more exts than it holds.
        let mut bad = color(4, 2, 0, 0x00_563412);
        bad[2] = 200;
        assert_eq!(read_with_ext(Some(xfext(&[bad], 1))), (red, blue, vec![]));
        assert_eq!(
            read_with_ext(Some(xfext(&[color(4, 2, 0, 0x00_563412)], 5))).0,
            Some((0x12, 0x34, 0x56))
        );
        // Without an XFCRC, or with one counting other XFs, the XFEXTs are
        // stale: the palette's colours stay and nothing is named.
        for crc in [None, Some(xfcrc(3))] {
            let ext = xfext(&[color(4, 2, 0, 0x00_563412), color(4, 3, 0, 4)], 2);
            assert_eq!(read_crc(crc, Some(ext)), (red, blue, vec![]));
        }
    }

    /// Header and footer, link and workbook options (#874 r12).
    #[test]
    fn header_footer_and_workbook_options_are_named() {
        let frt = |ty: u16, tail: &[u8]| {
            let mut b = vec![0u8; 12];
            b.extend_from_slice(tail);
            rec(ty, &b)
        };
        let hf = |flags: u16, cch: [u16; 4]| {
            let mut b = vec![0u8; 16];
            b.extend(flags.to_le_bytes());
            for n in cch {
                b.extend(n.to_le_bytes());
            }
            frt(0x089C, &b)
        };
        let names = |globals: &[Vec<u8>], sheet: &[Vec<u8>]| {
            let book = read(&workbook(globals, sheet)).unwrap();
            assert!(book.unknown_records.is_empty());
            book.dropped.iter().copied().collect::<Vec<_>>()
        };
        for (r, want) in [
            (hf(0x000D, [0; 4]), "page setup"),
            (hf(0x000E, [0; 4]), "page setup"),
            (hf(0x0008, [0; 4]), "page setup"),
            (hf(0x000C, [0, 5, 0, 0]), "page setup"),
            (rec(0x005F, &[0, 0]), "workbook options"),
            (frt(0x0866, &[1, 0]), "header and footer pictures"),
        ] {
            assert_eq!(names(&[], &[r]), [want]);
        }
        for (r, want) in [
            (rec(0x00DA, &[1, 0]), "external link settings"),
            (rec(0x00DA, &[0x20, 0]), "external link settings"),
            (frt(0x0863, &[22, 0, 0, 0, 1, 0, 0, 0]), "workbook options"),
            (frt(0x0863, &[22, 0, 0, 0, 4, 0, 0, 0]), "workbook options"),
            (frt(0x088C, &[1, 0, 0, 0]), "workbook options"),
        ] {
            assert_eq!(names(&[r], &[]), [want]);
        }
        // Excel's defaults, as the corpus has them (HEADERFOOTER's high
        // bits are left over and ignored).
        let globals = [
            rec(0x00DA, &[0, 0]),
            frt(0x0863, &[22, 0, 0, 0, 0, 0, 0, 0, 2, 0]),
            frt(0x088C, &[0; 4]),
            frt(0x0866, &[2, 0]),
        ];
        let sheet = [hf(0x60CC, [0; 4]), hf(0x333C, [0; 4]), rec(0x005F, &[1, 0])];
        assert!(names(&globals, &sheet).is_empty());
    }

    /// Streams and storages beside the Workbook stream are named (#874 r11).
    #[test]
    fn other_streams_are_named() {
        let stream = workbook(&[], &[]);
        let open = |extra: Vec<opccore::cfb::Node>| {
            let mut nodes = vec![opccore::cfb::Node::Stream("Workbook", stream.clone())];
            nodes.extend(extra);
            crate::legacy::open_workbook_ex(&opccore::cfb::write_cfb_tree(&nodes))
                .unwrap()
                .dropped
        };
        use opccore::cfb::Node::{Storage, Stream};
        assert!(open(vec![Stream("\u{1}CompObj", vec![0; 8])]).is_empty());
        assert_eq!(
            open(vec![
                Stream("Revision Log", vec![0; 8]),
                Stream("User Names", vec![0; 8])
            ]),
            ["shared workbook revision history"]
        );
        assert_eq!(
            open(vec![Storage("MsoDataStore", vec![Stream("Item", vec![1])])]),
            ["custom XML data"]
        );
        assert_eq!(
            open(vec![Storage("MBD0001", vec![Stream("CONTENTS", vec![1])])]),
            ["embedded objects and controls"]
        );
        assert_eq!(
            open(vec![Stream("Mystery", vec![1])]),
            ["other workbook content (Mystery)"]
        );
        let book = read(&workbook(&[rec(0x7777, &[])], &[])).unwrap();
        assert_eq!(book.unknown_records.len(), 1);
        let cfb = opccore::cfb::write_cfb_tree(&[opccore::cfb::Node::Stream(
            "Workbook",
            workbook(&[rec(0x7777, &[])], &[rec(0x7778, &[])]),
        )]);
        assert_eq!(
            crate::legacy::open_workbook_ex(&cfb).unwrap().dropped,
            ["other features (records 0x7777, 0x7778)"]
        );
    }

    /// A COLINFO's width counts without fUserSet (xlwt and other writers
    /// leave it clear), except Excel's default width for the sheet.
    #[test]
    fn colinfo_widths_count_without_f_user_set() {
        let colinfo = |first: u16, last: u16, coldx: u16, grbit: u16| {
            let mut b = Vec::new();
            for v in [first, last, coldx, 15, grbit, 0] {
                b.extend(v.to_le_bytes());
            }
            rec(0x007D, &b)
        };
        let cols = |sheet: &[Vec<u8>]| read(&workbook(&[], sheet)).unwrap().sheets[0].cols.clone();
        // xlwt's 0x0B6D (11.43 characters), flags clear.
        assert_eq!(
            cols(&[colinfo(1, 2, 0x0B6D, 0)]),
            [(1, 2, Some(0x0B6D as f64 / 256.0), false, true)]
        );
        // Excel's default for DEFCOLWIDTH 8 (2194) is no width.
        assert!(cols(&[rec(0x0055, &[8, 0]), colinfo(0, 255, 2194, 0)]).is_empty());
        // Nor is STANDARDWIDTH's, when the sheet has one.
        assert!(cols(&[rec(0x0099, &[0, 12]), colinfo(0, 255, 0x0C00, 0)]).is_empty());
        assert_eq!(
            cols(&[rec(0x0099, &[0, 12]), colinfo(0, 0, 2194, 0)]),
            [(0, 0, Some(2194.0 / 256.0), false, true)]
        );
        // fUserSet keeps a width that equals the default.
        assert_eq!(
            cols(&[colinfo(3, 3, 2194, 2)]),
            [(3, 3, Some(2194.0 / 256.0), false, false)]
        );
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
        assert_eq!(book.xfs[style as usize].code.as_deref(), Some("yyyy-mm-dd"));
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
                lib: None,
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

    /// A SUPBOOK of an external book: `ctab` sheet names after the virtPath
    /// `path` (8-bit characters, as Excel writes EUROTOOL.XLAM's).
    fn supbook(path: &[u8], sheets: &[&str]) -> Vec<u8> {
        let mut b = (sheets.len() as u16).to_le_bytes().to_vec();
        b.extend((path.len() as u16).to_le_bytes());
        b.push(0);
        b.extend_from_slice(path);
        for s in sheets {
            b.extend((s.len() as u16).to_le_bytes());
            b.push(0);
            b.extend(s.as_bytes());
        }
        rec(0x01AE, &b)
    }

    /// EXTERNNAME `name` with `flags` and the definition `rgce`.
    fn extern_name(flags: u16, name: &str, rgce: &[u8]) -> Vec<u8> {
        let mut b = flags.to_le_bytes().to_vec();
        b.extend([0; 4]);
        b.extend(short(name));
        b.extend((rgce.len() as u16).to_le_bytes());
        b.extend_from_slice(rgce);
        rec(0x0023, &b)
    }

    fn own_supbook() -> Vec<u8> {
        rec(0x01AE, &[1, 0, 0x01, 0x04])
    }

    fn addin_supbook() -> Vec<u8> {
        rec(0x01AE, &[1, 0, 0x01, 0x3A])
    }

    fn extern_sheet(xti: &[(u16, i16, i16)]) -> Vec<u8> {
        let mut b = (xti.len() as u16).to_le_bytes().to_vec();
        for &(book, first, last) in xti {
            b.extend(book.to_le_bytes());
            b.extend(first.to_le_bytes());
            b.extend(last.to_le_bytes());
        }
        rec(0x0017, &b)
    }

    /// ptgNameX: name `index` (1-based) of XTI `ixti`.
    fn name_x(ixti: u16, index: u16) -> Vec<u8> {
        let mut f = vec![0x39];
        f.extend(ixti.to_le_bytes());
        f.extend(index.to_le_bytes());
        f.extend([0, 0]);
        f
    }

    /// Cells `(0, i)` with formula `f[i]`, cached `i`.
    fn formulas(f: &[Vec<u8>]) -> Vec<Vec<u8>> {
        f.iter()
            .enumerate()
            .map(|(i, f)| formula(0, i as u16, i as f64, f))
            .collect()
    }

    fn formula_texts(book: &BookIn) -> Vec<Option<String>> {
        data(book).values().map(|c| c.formula.clone()).collect()
    }

    const LIB: &[u8] = b"\x01\x08TOOLS.XLAM";

    #[test]
    fn library_file_takes_only_a_plain_library_file_name() {
        let path = |b: &[u8]| String::from_utf8(b.to_vec()).unwrap();
        assert_eq!(
            library_file(&path(b"\x01\x08EUROTOOL.XLAM")).as_deref(),
            Some("EUROTOOL.XLAM")
        );
        for bad in [
            &b"\x01\x08Analysis\x03ANALYS32.XLL"[..],
            b"\x01\x01C\x03Books\x03A.xlsx",
            b"\x01\x08",
            b"\x01",
            b"",
            b"A.xlsx",
            b"\x08TOOLS.XLAM",
            b"\x01\x08a\\b.xlam",
            b"\x01\x08a/b.xlam",
            b"\x01\x08C:b.xlam",
        ] {
            assert_eq!(library_file(&path(bad)), None, "{bad:?}");
        }
    }

    /// Two Library books with a function name become `[1]` and `[2]`; an
    /// ordinary workbook between them (a plain path) and a Library book
    /// whose only name has a definition get no link and number nothing.
    /// A defined name of a linked book, a flagged name and a reference
    /// scoped to one of its sheets keep only their value.
    #[test]
    fn name_x_into_a_library_book_is_numbered_and_qualified() {
        let globals = [
            supbook(LIB, &["1028", "1030"]),
            extern_name(0, "ONE", &[]),
            supbook(b"\x01\x01C\x03Books\x03Prices.xlsx", &["P"]),
            extern_name(0, "Prices", &[]),
            supbook(b"\x01\x08DEFS.XLAM", &[]),
            extern_name(0, "Rate", &[0x1E, 1, 0]),
            supbook(b"\x01\x08MORE.XLAM", &[]),
            extern_name(0, "Half", &[0x1E, 1, 0]),
            extern_name(0, "TWO", &[]),
            extern_name(0x0002, "Flagged", &[]),
            own_supbook(),
            extern_sheet(&[
                (0, -2, -2),
                (1, -2, -2),
                (2, -2, -2),
                (3, -2, -2),
                (3, 0, 0),
            ]),
        ];
        let book = read(&workbook(
            &globals,
            &formulas(&[
                name_x(0, 1),
                name_x(1, 1),
                name_x(2, 1),
                name_x(3, 2),
                name_x(3, 1),
                name_x(3, 3),
                name_x(4, 2),
                name_x(3, 4),
            ]),
        ))
        .unwrap();
        assert_eq!(
            formula_texts(&book),
            [
                Some("[1]!ONE".to_string()),
                None,
                None,
                Some("[2]!TWO".to_string()),
                None,
                None,
                None,
                None,
            ]
        );
        // Every cell keeps its value.
        assert_eq!(data(&book)[&(0, 7)].value, CellValue::Number(7.0));
        let links = &book.external_links;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].book, "rId1");
        assert_eq!(
            links[0].rels,
            [(
                "rId1".to_string(),
                XL_LIBRARY.to_string(),
                "TOOLS.XLAM".to_string(),
                Some("External".to_string())
            )]
        );
        assert_eq!(links[0].sheets, ["1028", "1030"]);
        assert_eq!(links[0].names, [Some("ONE".to_string())]);
        assert_eq!(links[1].rels[0].2, "MORE.XLAM");
        assert_eq!(links[1].names, [None, Some("TWO".to_string()), None]);
    }

    /// A SUPBOOK or EXTERNNAME cut short still takes its place, so the
    /// books and names after it keep their numbers: a formula naming them
    /// is right or dropped, never wrong.
    #[test]
    fn a_truncated_supbook_or_extern_name_keeps_its_slot() {
        let globals = [
            // Before any SUPBOOK: ignored.
            extern_name(0, "Stray", &[]),
            // Cut off before its cch.
            rec(0x01AE, &[0, 0]),
            // Cut off after ctab and cch.
            rec(0x01AE, &[0, 0, 12, 0]),
            // Cut off in its sheet names: a book with no link.
            rec(0x01AE, &[1, 0, 3, 0, 0, 1, 8, b'A']),
            supbook(LIB, &[]),
            // Flags only, then cut off before the cce.
            rec(0x0023, &[0, 0]),
            rec(0x0023, &[0, 0, 0, 0, 0, 0, 1, 0, b'X']),
            extern_name(0, "F", &[]),
            addin_supbook(),
            rec(0x0023, &[0, 0, 0]),
            extern_name(0, "EDATE", &[]),
            own_supbook(),
            extern_sheet(&[
                (1, -2, -2),
                (2, -2, -2),
                (3, -2, -2),
                (4, -2, -2),
                (0, -2, -2),
            ]),
        ];
        let book = read(&workbook(
            &globals,
            &formulas(&[
                name_x(0, 1),
                name_x(1, 1),
                name_x(2, 1),
                name_x(2, 2),
                name_x(2, 3),
                name_x(3, 1),
                name_x(3, 2),
                name_x(4, 1),
            ]),
        ))
        .unwrap();
        assert_eq!(
            formula_texts(&book),
            [
                None,
                None,
                None,
                None,
                Some("[1]!F".to_string()),
                None,
                Some("EDATE".to_string()),
                None,
            ]
        );
        assert_eq!(book.external_links.len(), 1);
        assert_eq!(book.external_links[0].names, [None, None, Some("F".into())]);
    }

    /// The 0x3A01 book's names are functions an XLL registers: one that is
    /// built in (the Analysis ToolPak's, as Excel writes EDATE) stays bare,
    /// any other is `_xll.NAME`, as Excel's `.xlsx` of
    /// corpus/legacy/addin/xll-udf spells XLLTWICE. Excel gives each a
    /// `#REF!` definition, which doesn't matter.
    #[test]
    fn name_x_into_the_add_in_book_spells_xll_functions() {
        let book = read(&workbook(
            &[
                addin_supbook(),
                extern_name(0, "XLLTWICE", &[0x1C, 0x17]),
                extern_name(0, "EDATE", &[0x1C, 0x17]),
                own_supbook(),
                extern_sheet(&[(0, -2, -2)]),
            ],
            &formulas(&[name_x(0, 1), name_x(0, 2), name_x(0, 3)]),
        ))
        .unwrap();
        assert_eq!(
            formula_texts(&book),
            [
                Some("_xll.XLLTWICE".to_string()),
                Some("EDATE".to_string()),
                None
            ]
        );
        assert!(book.external_links.is_empty());
    }

    /// A workbook naming more Library books with a function name than the
    /// budget allows is refused.
    #[test]
    fn library_links_are_budgeted() {
        let stream = workbook(
            &[
                supbook(b"\x01\x08A.XLAM", &[]),
                extern_name(0, "F", &[]),
                supbook(b"\x01\x08B.XLAM", &[]),
                extern_name(0, "G", &[]),
            ],
            &[],
        );
        assert_eq!(read(&stream).unwrap().external_links.len(), 2);
        let tight = Limits {
            links: 1,
            ..Limits::default()
        };
        let err = read_with(&stream, tight).err().unwrap();
        assert!(err.to_string().contains("too many external links"), "{err}");
    }

    /// The `.xls` import of a Library book's function saves as a `.xlsx`
    /// with its external link, which loads back with the formula.
    #[test]
    fn a_library_link_survives_save_as_xlsx() {
        let stream = workbook(
            &[
                supbook(LIB, &["1028"]),
                extern_name(0, "F", &[]),
                own_supbook(),
                extern_sheet(&[(0, -2, -2)]),
            ],
            &formulas(&[name_x(0, 1)]),
        );
        let cfb = opccore::cfb::write_cfb(&[("Workbook", stream)]);
        let (pkg, _) = super::super::open_workbook(&cfb).unwrap();
        let bytes = crate::xlsx::save_xlsx(&pkg);
        let back = crate::xlsx::load_xlsx(&bytes).unwrap();
        assert_eq!(
            back.workbook.sheets[0]
                .cell(0, 0)
                .unwrap()
                .formula
                .as_deref(),
            Some("[1]!F")
        );
        let zip = opccore::zip::ZipArchive::open(&bytes).unwrap();
        let rels = String::from_utf8(
            zip.read("xl/externalLinks/_rels/externalLink1.xml.rels")
                .unwrap(),
        )
        .unwrap();
        assert!(
            rels.contains(r#"Target="TOOLS.XLAM" TargetMode="External""#),
            "{rels}"
        );
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

    /// The oracle for `builtin_code`'s table (#1279): Excel's own .xls
    /// writes builtins 5-8 and 41-44 as explicit FORMAT records, and the
    /// strings there are the codes the table must carry.
    #[test]
    fn xls_builtin_format_records_match_builtin_code() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../corpus/legacy/oracle-basic.xls"
        );
        // A missing corpus file is a failure, never a skip.
        let data = std::fs::read(path).unwrap();
        let cfb = opccore::cfb::Cfb::open(&data).unwrap();
        let stream = ["Workbook", "Book"]
            .iter()
            .find_map(|n| cfb.read_stream(n))
            .unwrap();
        let mut codes = std::collections::HashMap::new();
        for r in records(&stream) {
            if r.ty == 0x041E {
                let mut c = Cur::new(&r);
                let id = c.u16().unwrap();
                codes.insert(id, c.xl_string().unwrap());
            }
        }
        for id in [5u16, 6, 7, 8, 41, 42, 43, 44] {
            let recorded = codes
                .get(&id)
                .unwrap_or_else(|| panic!("oracle-basic.xls has no FORMAT record for ifmt {id}"));
            assert_eq!(
                Some(recorded.as_str()),
                crate::numfmt::builtin_code(u32::from(id)),
                "ifmt {id}"
            );
        }
    }
}
