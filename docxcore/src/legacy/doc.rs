//! Word 97-2003 binary documents (`.doc`, MS-DOC) imported into a package.
//!
//! The reader takes the main document text from the piece table, the run
//! formatting from the CHPX pages and the paragraph properties from the
//! PAPX pages, and builds paragraphs and tables from them:
//!
//! - text: every piece of the main document (`[0, ccpText)`), 8-bit
//!   (cp1252, MS-DOC's compressed form) or UTF-16; footnote, header,
//!   comment and text-box stories are dropped;
//! - runs: bold, italic, underline, strike, size, font and colour, as the
//!   CHPX states them (direct formatting, not the style's);
//! - paragraphs: alignment, and the built-in Heading 1-9 styles;
//! - tables: rows and cells with their paragraphs (nested tables flatten
//!   into their outer cell);
//! - fields keep their result and drop their instructions; section breaks
//!   become page breaks.
//!
//! The reader is lenient. An unknown sprm is skipped by its size, and a
//! formatting page, piece or style that points outside its stream is
//! ignored, so the text still imports. The hard errors are the files that
//! can't be read at all: see [`DocImportError`]. Every offset, length and
//! count read from the file is checked against the stream it indexes before
//! anything is allocated or looped over.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use opccore::cfb::Cfb;

use crate::model::{
    Align, Block, BreakKind, Cell, Document, Inline, ParProps, Paragraph, Row, Run, RunProps,
};
use crate::package::{Package, new_package};

/// Why [`import_doc`] could not read a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocImportError {
    /// A `.doc` whose FIB says it is encrypted or XOR-obfuscated.
    Encrypted,
    /// An OLE2 file holding an encrypted OOXML package (`EncryptionInfo` /
    /// `EncryptedPackage`): a password-protected `.docx`.
    EncryptedPackage,
    /// A Word 6.0 or Word 95 document (`nFib` below Word 97's).
    Word95 { nfib: u16 },
    /// An OLE2 file with no `WordDocument` stream, or one that isn't a Word
    /// FIB (an `.xls`, an `.mpp`, ...).
    NotWordDocument,
    /// The compound file or the document's own structure can't be read.
    Corrupt(String),
}

impl fmt::Display for DocImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocImportError::Encrypted => {
                f.write_str("encrypted Word 97-2003 document (decryption is not supported)")
            }
            DocImportError::EncryptedPackage => {
                f.write_str("password-protected document (decryption is not supported)")
            }
            DocImportError::Word95 { nfib } => write!(
                f,
                "Word 6.0/95 document (nFib {nfib:#06x}): only Word 97-2003 documents can be imported"
            ),
            DocImportError::NotWordDocument => {
                f.write_str("OLE2 compound file that is not a Word document")
            }
            DocImportError::Corrupt(why) => write!(f, "damaged Word 97-2003 document: {why}"),
        }
    }
}

impl std::error::Error for DocImportError {}

/// Import a Word 97-2003 `.doc` as a new package in Word 2003 Compatibility
/// Mode (`compatibilityMode` 11).
pub fn import_doc(bytes: &[u8]) -> Result<Package, DocImportError> {
    let document = read_doc(bytes)?;
    let mut headings = BTreeSet::new();
    collect_headings(&document.body, &mut headings);
    let mut pkg = new_package(document);
    let ids: Vec<String> = headings.iter().map(|n| format!("Heading{n}")).collect();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    pkg.ensure_styles(&ids);
    pkg.set_compatibility_mode(11);
    Ok(pkg)
}

fn collect_headings(blocks: &[Block], out: &mut BTreeSet<u8>) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => out.extend(p.props.heading_level),
            Block::Table(t) => {
                for c in t.rows.iter().flat_map(|r| &r.cells) {
                    collect_headings(&c.blocks, out);
                }
            }
            _ => {}
        }
    }
}

/// Read a Word 97-2003 `.doc` into a document tree.
pub fn read_doc(bytes: &[u8]) -> Result<Document, DocImportError> {
    let cfb = Cfb::open(bytes).map_err(DocImportError::Corrupt)?;
    let names = cfb.stream_names();
    let has = |n: &str| names.iter().any(|s| s == n);
    if has("EncryptionInfo") || has("EncryptedPackage") {
        return Err(DocImportError::EncryptedPackage);
    }
    let word = cfb
        .read_stream("WordDocument")
        .ok_or(DocImportError::NotWordDocument)?;
    let fib = Fib::parse(&word)?;
    let table = cfb
        .read_stream(if fib.which_table { "1Table" } else { "0Table" })
        .ok_or_else(|| DocImportError::Corrupt("the table stream is missing".into()))?;
    let (prcs, pieces) = parse_clx(&table, fib.clx)?;
    let chars = decode_text(&word, &pieces, fib.ccp_text);
    let fmt = Formatting {
        chpx: fkp_runs(&word, &table, fib.bte_chpx, FkpKind::Chpx),
        papx: fkp_runs(&word, &table, fib.bte_papx, FkpKind::Papx),
        styles: Styles::parse(&table, fib.stshf),
        fonts: parse_fonts(&table, fib.sttbf_ffn),
        prcs,
    };
    let section_ends = plc_cps(&table, fib.plcf_sed, 12);
    Ok(Document {
        body: Builder::new(&fmt, &pieces).build(&chars, &section_ends),
    })
}

// ---- bytes ----

fn u8_at(b: &[u8], o: usize) -> Option<u8> {
    b.get(o).copied()
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(o..o.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// `len` bytes at `at`, or `None` when they aren't all there.
fn slice(b: &[u8], at: u32, len: u32) -> Option<&[u8]> {
    let at = at as usize;
    b.get(at..at.checked_add(len as usize)?)
}

// ---- FIB ----

/// The `fc`/`lcb` pair of one FibRgFcLcb97 entry: where a structure sits in
/// the table stream, and its size.
#[derive(Debug, Clone, Copy, Default)]
struct FcLcb {
    fc: u32,
    lcb: u32,
}

struct Fib {
    which_table: bool,
    ccp_text: u32,
    stshf: FcLcb,
    plcf_sed: FcLcb,
    bte_chpx: FcLcb,
    bte_papx: FcLcb,
    sttbf_ffn: FcLcb,
    clx: FcLcb,
}

/// Word 97's `nFib`. Some Word 97 files say 0x00C0 or 0x00C2, and later
/// versions write 0x00C1 here with their own number in `nFibNew`, so only
/// what is below 0x00C0 (Word 6.0's 0x0065, Word 95's 0x0068) is older.
const NFIB_WORD97: u16 = 0x00C0;

impl Fib {
    fn parse(word: &[u8]) -> Result<Fib, DocImportError> {
        let short = || DocImportError::Corrupt("the FIB is truncated".into());
        if u16_at(word, 0).ok_or_else(short)? != 0xA5EC {
            return Err(DocImportError::NotWordDocument);
        }
        let nfib = u16_at(word, 2).ok_or_else(short)?;
        if nfib < NFIB_WORD97 {
            return Err(DocImportError::Word95 { nfib });
        }
        let flags = u16_at(word, 10).ok_or_else(short)?;
        // fEncrypted, fObfuscated.
        if flags & 0x0100 != 0 || flags & 0x8000 != 0 {
            return Err(DocImportError::Encrypted);
        }
        let csw = u16_at(word, 32).ok_or_else(short)? as usize;
        let lw_at = 34 + csw * 2;
        let cslw = u16_at(word, lw_at).ok_or_else(short)? as usize;
        let rglw = lw_at + 2;
        let ccp_text = u32_at(word, rglw + 12).ok_or_else(short)?;
        let fc_at = rglw + cslw * 4;
        let cb_rg_fc_lcb = u16_at(word, fc_at).ok_or_else(short)? as usize;
        let blob = fc_at + 2;
        let pair = |i: usize| {
            if i >= cb_rg_fc_lcb {
                return FcLcb::default();
            }
            let at = blob + i * 8;
            match (u32_at(word, at), u32_at(word, at + 4)) {
                (Some(fc), Some(lcb)) => FcLcb { fc, lcb },
                _ => FcLcb::default(),
            }
        };
        Ok(Fib {
            which_table: flags & 0x0200 != 0,
            ccp_text,
            stshf: pair(1),
            plcf_sed: pair(6),
            bte_chpx: pair(12),
            bte_papx: pair(13),
            sttbf_ffn: pair(15),
            clx: pair(33),
        })
    }
}

// ---- piece table ----

#[derive(Debug, Clone, Copy)]
struct Piece {
    cp_start: u32,
    cp_end: u32,
    /// Byte offset of the piece's first character in the WordDocument stream.
    fc: u32,
    /// 8-bit (cp1252) text rather than UTF-16.
    compressed: bool,
    prm: u16,
}

/// The Clx: the property grpprls (Prc) that pieces' `prm` can point at, and
/// the piece table.
fn parse_clx(table: &[u8], clx: FcLcb) -> Result<(Vec<Vec<u8>>, Vec<Piece>), DocImportError> {
    let bad = |why: &str| DocImportError::Corrupt(why.to_string());
    let clx = slice(table, clx.fc, clx.lcb).ok_or_else(|| bad("the piece table is missing"))?;
    let mut prcs = Vec::new();
    let mut at = 0usize;
    while u8_at(clx, at) == Some(0x01) {
        let cb = u16_at(clx, at + 1).ok_or_else(|| bad("a Prc is truncated"))? as usize;
        let g = clx
            .get(at + 3..at + 3 + cb)
            .ok_or_else(|| bad("a Prc is truncated"))?;
        prcs.push(g.to_vec());
        at += 3 + cb;
    }
    if u8_at(clx, at) != Some(0x02) {
        return Err(bad("the piece table is missing"));
    }
    let lcb = u32_at(clx, at + 1).ok_or_else(|| bad("the piece table is truncated"))? as usize;
    let plc = clx
        .get(at + 5..)
        .and_then(|rest| rest.get(..lcb.min(rest.len())))
        .ok_or_else(|| bad("the piece table is truncated"))?;
    if plc.len() < 4 {
        return Err(bad("the piece table is empty"));
    }
    let n = (plc.len() - 4) / 12;
    let mut pieces = Vec::with_capacity(n);
    for i in 0..n {
        let (Some(cp_start), Some(cp_end)) = (u32_at(plc, i * 4), u32_at(plc, i * 4 + 4)) else {
            break;
        };
        let pcd = (n + 1) * 4 + i * 8;
        let (Some(fc), Some(prm)) = (u32_at(plc, pcd + 2), u16_at(plc, pcd + 6)) else {
            break;
        };
        let compressed = fc & 0x4000_0000 != 0;
        let raw = fc & 0x3FFF_FFFF;
        pieces.push(Piece {
            cp_start,
            cp_end,
            fc: if compressed { raw / 2 } else { raw },
            compressed,
            prm,
        });
    }
    Ok((prcs, pieces))
}

/// One character of the main document, with what locates its formatting.
#[derive(Debug, Clone, Copy)]
struct Ch {
    /// The UTF-16 unit or cp1252 byte as stored (before surrogates join).
    c: char,
    cp: u32,
    /// Byte offset in the WordDocument stream: CHPX and PAPX are keyed by it.
    fc: u32,
    piece: usize,
}

/// MS-DOC's compressed text is cp1252: 0x80-0x9F map to these characters
/// (0 where cp1252 leaves the byte undefined, which then stays U+0080..).
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

fn cp1252(b: u8) -> char {
    match b {
        0x80..=0x9F => match CP1252_HIGH[(b - 0x80) as usize] {
            0 => char::from(b),
            u => char::from_u32(u32::from(u)).unwrap_or(char::from(b)),
        },
        _ => char::from(b),
    }
}

/// The main document's characters, in CP order. Pieces cover ascending,
/// disjoint CP ranges: one that reaches back over CPs already read is
/// clipped to the rest, and one wholly behind is skipped. A piece that runs
/// past its stream stops where the stream does, nothing past `ccp_text` is
/// read, and there are never more characters than the WordDocument stream
/// has bytes, however many pieces claim them.
fn decode_text(word: &[u8], pieces: &[Piece], ccp_text: u32) -> Vec<Ch> {
    let cap = (ccp_text as usize).min(word.len());
    let mut out = Vec::new();
    let mut next_cp = 0u32;
    for (i, p) in pieces.iter().enumerate() {
        let start = p.cp_start.max(next_cp);
        let end = p.cp_end.min(ccp_text);
        if start >= end || out.len() >= cap {
            continue;
        }
        let width = if p.compressed { 1 } else { 2 };
        let Some(first) = ((start - p.cp_start) as usize)
            .checked_mul(width)
            .and_then(|skip| (p.fc as usize).checked_add(skip))
        else {
            continue;
        };
        // Never more characters than the stream holds from there.
        let avail = word.len().saturating_sub(first) / width;
        let count = ((end - start) as usize).min(avail).min(cap - out.len());
        let mut k = 0usize;
        while k < count {
            let fc = first + k * width;
            let cp = start + k as u32;
            if p.compressed {
                out.push(Ch {
                    c: cp1252(word[fc]),
                    cp,
                    fc: fc as u32,
                    piece: i,
                });
                k += 1;
                continue;
            }
            let u = u16::from_le_bytes([word[fc], word[fc + 1]]);
            let pair = (0xD800..0xDC00).contains(&u) && k + 1 < count;
            let lo = if pair {
                u16::from_le_bytes([word[fc + 2], word[fc + 3]])
            } else {
                0
            };
            let (c, used) = match char::decode_utf16([u, lo]).next() {
                Some(Ok(c)) if pair && c.len_utf16() == 2 => (c, 2),
                _ => (
                    char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER),
                    1,
                ),
            };
            out.push(Ch {
                c,
                cp,
                fc: fc as u32,
                piece: i,
            });
            k += used;
        }
        next_cp = start + k as u32;
    }
    out
}

/// The CPs of a PLC whose data entries are `cb_data` bytes each.
fn plc_cps(table: &[u8], at: FcLcb, cb_data: usize) -> BTreeSet<u32> {
    let Some(plc) = slice(table, at.fc, at.lcb) else {
        return BTreeSet::new();
    };
    let n = plc.len().saturating_sub(4) / (4 + cb_data);
    (0..=n).filter_map(|i| u32_at(plc, i * 4)).collect()
}

// ---- formatting pages ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum FkpKind {
    Chpx,
    Papx,
}

/// The formatting a CHPX or PAPX gives the bytes `[start, end)` of the
/// WordDocument stream.
struct FkpRun {
    start: u32,
    end: u32,
    /// The paragraph's style (PAPX only).
    istd: u16,
    grpprl: Vec<u8>,
}

/// Every run of the CHPX or PAPX pages the bin table names, ordered by `start`.
fn fkp_runs(word: &[u8], table: &[u8], bte: FcLcb, kind: FkpKind) -> Vec<FkpRun> {
    let Some(plc) = slice(table, bte.fc, bte.lcb) else {
        return Vec::new();
    };
    let n = plc.len().saturating_sub(4) / 8;
    // Each page once, however often (or in whatever order) the bin table
    // names it, and only the pages the stream has.
    let in_stream = (word.len() / 512) as u32;
    let mut pages: Vec<u32> = (0..n)
        .filter_map(|i| u32_at(plc, (n + 1) * 4 + i * 4))
        .map(|pn| pn & 0x003F_FFFF)
        .filter(|&pn| pn < in_stream)
        .collect();
    pages.sort_unstable();
    pages.dedup();
    let mut runs = Vec::new();
    for pn in pages {
        let Some(page) = (pn as usize)
            .checked_mul(512)
            .and_then(|at| word.get(at..at.checked_add(512)?))
        else {
            continue;
        };
        let count = page[511] as usize;
        // rgfc[count + 1], then one byte (CHPX) or a 13-byte BxPap (PAPX) per run.
        let entry = if kind == FkpKind::Chpx { 1 } else { 13 };
        if (count + 1) * 4 + count * entry > 511 {
            continue;
        }
        for i in 0..count {
            let (Some(start), Some(end)) = (u32_at(page, i * 4), u32_at(page, i * 4 + 4)) else {
                break;
            };
            let at = (count + 1) * 4 + i * entry;
            let offset = page[at] as usize * 2;
            let (istd, grpprl) = match kind {
                _ if offset == 0 => (0, Vec::new()),
                FkpKind::Chpx => {
                    let cb = page.get(offset).copied().unwrap_or(0) as usize;
                    let g = page.get(offset + 1..offset + 1 + cb).unwrap_or(&[]);
                    (0, g.to_vec())
                }
                FkpKind::Papx => {
                    // PapxInFkp: cb, or 0 then cb'; the grpprlInPapx is
                    // `2*cb - 1` or `2*cb'` bytes, an istd and then sprms.
                    let cb = page.get(offset).copied().unwrap_or(0) as usize;
                    let (from, len) = if cb == 0 {
                        let cb2 = page.get(offset + 1).copied().unwrap_or(0) as usize;
                        (offset + 2, cb2 * 2)
                    } else {
                        (offset + 1, (cb * 2).saturating_sub(1))
                    };
                    let g = page.get(from..from + len).unwrap_or(&[]);
                    match u16_at(g, 0) {
                        Some(istd) => (istd, g[2..].to_vec()),
                        None => (0, Vec::new()),
                    }
                }
            };
            runs.push(FkpRun {
                start,
                end,
                istd,
                grpprl,
            });
        }
    }
    runs.sort_by_key(|r| r.start);
    runs
}

/// The index of the run covering stream offset `fc`.
fn run_at(runs: &[FkpRun], fc: u32) -> Option<usize> {
    let i = runs.partition_point(|r| r.start <= fc).checked_sub(1)?;
    (fc < runs[i].end).then_some(i)
}

// ---- sprms ----

/// Call `f` with each sprm of `grpprl` and its operand (for a variable-size
/// operand, including its length prefix). Each operand's size comes from
/// the sprm's `spra` bits, so an unknown sprm is skipped exactly; a sprm
/// whose operand runs past the end stops the walk.
fn walk_sprms(grpprl: &[u8], mut f: impl FnMut(u16, &[u8])) {
    let mut at = 0usize;
    while let Some(sprm) = u16_at(grpprl, at) {
        at += 2;
        let len = match sprm >> 13 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => match sprm {
                // sprmTDefTable, sprmTDefTable10: a 2-byte cb, which counts
                // the operand's remaining bytes plus one.
                0xD608 | 0xD606 => match u16_at(grpprl, at) {
                    Some(cb) => 2 + (cb as usize).saturating_sub(1),
                    None => break,
                },
                // sprmPChgTabs: cb 255 means the size is in the operand.
                0xC615 if u8_at(grpprl, at) == Some(255) => {
                    let Some(del) = u8_at(grpprl, at + 1).map(usize::from) else {
                        break;
                    };
                    let Some(add) = u8_at(grpprl, at + 2 + del * 4).map(usize::from) else {
                        break;
                    };
                    1 + (1 + del * 4) + (1 + add * 3)
                }
                _ => match u8_at(grpprl, at) {
                    Some(cb) => 1 + cb as usize,
                    None => break,
                },
            },
        };
        let Some(op) = grpprl.get(at..at + len) else {
            break;
        };
        f(sprm, op);
        at += len;
    }
}

const SPRM_C_F_BOLD: u16 = 0x0835;
const SPRM_C_F_ITALIC: u16 = 0x0836;
const SPRM_C_F_STRIKE: u16 = 0x0837;
const SPRM_C_KUL: u16 = 0x2A3E;
const SPRM_C_ICO: u16 = 0x2A42;
const SPRM_C_HPS: u16 = 0x4A43;
const SPRM_C_ISTD: u16 = 0x4A30;
const SPRM_C_RG_FTC0: u16 = 0x4A4F;
const SPRM_C_CV: u16 = 0x6870;
const SPRM_P_JC80: u16 = 0x2403;
const SPRM_P_JC: u16 = 0x2461;
const SPRM_P_F_IN_TABLE: u16 = 0x2416;
const SPRM_P_F_TTP: u16 = 0x2417;
const SPRM_P_F_INNER_TTP: u16 = 0x244C;
const SPRM_P_ITAP: u16 = 0x6649;
const SPRM_T_DEF_TABLE: u16 = 0xD608;

/// The toggles a CHPX can set: bold, italic, strike.
const TOGGLES: [u16; 3] = [SPRM_C_F_BOLD, SPRM_C_F_ITALIC, SPRM_C_F_STRIKE];

/// Character formatting as a grpprl states it: `None` where it says nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Chp {
    /// Raw toggle operands (0, 1, 0x80 = the style's value, 0x81 = its
    /// opposite) for bold, italic, strike.
    toggles: [Option<u8>; 3],
    underline: Option<bool>,
    size: Option<u16>,
    font: Option<u16>,
    /// `Some(None)` is an explicit automatic colour.
    color: Option<Option<String>>,
    char_style: Option<u16>,
}

/// Word's 16 colour indexes (`ico`), 1-16.
const ICO: [&str; 16] = [
    "000000", "0000FF", "00FFFF", "00FF00", "FF00FF", "FF0000", "FFFF00", "FFFFFF", "000080",
    "008080", "008000", "800080", "800000", "808000", "808080", "C0C0C0",
];

impl Chp {
    fn apply(&mut self, grpprl: &[u8]) {
        walk_sprms(grpprl, |sprm, op| match sprm {
            _ if TOGGLES.contains(&sprm) => {
                let i = TOGGLES.iter().position(|&t| t == sprm).unwrap_or(0);
                self.toggles[i] = Some(op[0]);
            }
            SPRM_C_KUL => self.underline = Some(op[0] != 0),
            SPRM_C_HPS => self.size = u16_at(op, 0),
            SPRM_C_RG_FTC0 => self.font = u16_at(op, 0),
            SPRM_C_ISTD => self.char_style = u16_at(op, 0),
            SPRM_C_ICO => {
                self.color = Some(match op[0] {
                    i @ 1..=16 => Some(ICO[i as usize - 1].to_string()),
                    _ => None,
                })
            }
            SPRM_C_CV => {
                // COLORREF: red, green, blue, then fAuto (0xFF = automatic).
                self.color = Some(
                    (op[3] != 0xFF).then(|| format!("{:02X}{:02X}{:02X}", op[0], op[1], op[2])),
                )
            }
            _ => {}
        });
    }
}

/// Paragraph properties as a PAPX states them.
#[derive(Debug, Clone, Default)]
struct Pap {
    align: Option<Align>,
    in_table: bool,
    /// The paragraph is a table row's end mark (TTP).
    row_end: bool,
    inner_row_end: bool,
    /// Table depth (`sprmPItap`): 1 in a table, 2+ in a nested one.
    depth: u32,
    /// Column boundaries of the row this mark ends (`sprmTDefTable`).
    columns: Vec<i16>,
}

impl Pap {
    fn parse(grpprl: &[u8]) -> Pap {
        let mut pap = Pap::default();
        walk_sprms(grpprl, |sprm, op| match sprm {
            SPRM_P_JC80 | SPRM_P_JC => {
                pap.align = Some(match op[0] {
                    1 => Align::Center,
                    2 => Align::Right,
                    3..=9 => Align::Justify,
                    _ => Align::Left,
                })
            }
            SPRM_P_F_IN_TABLE => pap.in_table = op[0] != 0,
            SPRM_P_F_TTP => pap.row_end = op[0] != 0,
            SPRM_P_F_INNER_TTP => pap.inner_row_end = op[0] != 0,
            SPRM_P_ITAP => pap.depth = u32_at(op, 0).unwrap_or(0),
            SPRM_T_DEF_TABLE => {
                // cb (2), itcMac (1), rgdxaCenter[itcMac + 1], ...
                let n = op.get(2).copied().unwrap_or(0) as usize;
                pap.columns = (0..=n)
                    .map_while(|i| u16_at(op, 3 + i * 2).map(|v| v as i16))
                    .collect();
            }
            _ => {}
        });
        if pap.in_table && pap.depth == 0 {
            pap.depth = 1;
        }
        pap
    }
}

// ---- styles ----

/// What the reader uses of one style sheet entry.
#[derive(Debug, Clone, Default)]
struct Style {
    /// The built-in style identifier: 0 Normal, 1-9 Heading 1-9.
    sti: u16,
    base: Option<u16>,
    /// The style's own character formatting (its UpxChpx).
    chpx: Vec<u8>,
}

#[derive(Default)]
struct Styles {
    styles: Vec<Option<Style>>,
    /// Each style's resolved bold/italic/strike, through its base chain.
    toggles: Vec<[bool; 3]>,
}

impl Styles {
    fn parse(table: &[u8], at: FcLcb) -> Styles {
        let mut styles = Vec::new();
        if let Some(stsh) = slice(table, at.fc, at.lcb) {
            let cb_stshi = u16_at(stsh, 0).unwrap_or(0) as usize;
            let cstd = u16_at(stsh, 2).unwrap_or(0) as usize;
            let cb_base = u16_at(stsh, 4).unwrap_or(10) as usize;
            let mut at = 2 + cb_stshi;
            for _ in 0..cstd {
                let Some(cb) = u16_at(stsh, at) else {
                    break;
                };
                let Some(std) = stsh.get(at + 2..at + 2 + cb as usize) else {
                    break;
                };
                at += 2 + cb as usize;
                styles.push((!std.is_empty()).then(|| parse_std(std, cb_base)).flatten());
            }
        }
        let mut s = Styles {
            toggles: vec![[false; 3]; styles.len()],
            styles,
        };
        for i in 0..s.styles.len() {
            s.toggles[i] = s.resolve(i, 0);
        }
        s
    }

    /// Style `i`'s toggles: its base's, then its own CHPX over them.
    fn resolve(&self, i: usize, depth: u32) -> [bool; 3] {
        let Some(Some(style)) = self.styles.get(i) else {
            return [false; 3];
        };
        let mut t = match style.base {
            Some(b) if depth < 16 && (b as usize) != i => self.resolve(b as usize, depth + 1),
            _ => [false; 3],
        };
        let mut chp = Chp::default();
        chp.apply(&style.chpx);
        for (k, v) in chp.toggles.iter().enumerate() {
            if let Some(v) = v {
                t[k] = toggle(*v, t[k]);
            }
        }
        t
    }

    fn sti(&self, istd: u16) -> Option<u16> {
        Some(self.styles.get(istd as usize)?.as_ref()?.sti)
    }

    fn toggles(&self, istd: u16) -> [bool; 3] {
        self.toggles
            .get(istd as usize)
            .copied()
            .unwrap_or([false; 3])
    }
}

/// A toggle operand applied over the value it inherits.
fn toggle(op: u8, inherited: bool) -> bool {
    match op {
        0 => false,
        1 => true,
        0x80 => inherited,
        0x81 => !inherited,
        _ => inherited,
    }
}

fn parse_std(std: &[u8], cb_base: usize) -> Option<Style> {
    let sti = u16_at(std, 0)? & 0x0FFF;
    let w = u16_at(std, 2)?;
    let stk = w & 0x000F;
    let base = w >> 4;
    let cupx = (u16_at(std, 4)? & 0x000F) as usize;
    // The name (Xstz: cch, then cch UTF-16 units and a terminator).
    let cch = u16_at(std, cb_base)? as usize;
    let mut at = cb_base + 2 + cch * 2 + 2;
    let mut upx = Vec::new();
    for _ in 0..cupx {
        at += at % 2;
        let cb = u16_at(std, at)? as usize;
        upx.push(std.get(at + 2..at + 2 + cb)?);
        at += 2 + cb;
    }
    // A paragraph style's UPXs are its PAPX then its CHPX; a character
    // style has only the CHPX.
    let chpx = match stk {
        1 => upx.get(1),
        2 => upx.first(),
        _ => None,
    };
    Some(Style {
        sti,
        base: (base != 0x0FFF).then_some(base),
        chpx: chpx.map(|g| g.to_vec()).unwrap_or_default(),
    })
}

/// The font names of the SttbfFfn, by index.
fn parse_fonts(table: &[u8], at: FcLcb) -> Vec<String> {
    let Some(sttb) = slice(table, at.fc, at.lcb) else {
        return Vec::new();
    };
    let count = u16_at(sttb, 0).unwrap_or(0) as usize;
    let mut fonts = Vec::new();
    let mut at = 4usize;
    for _ in 0..count {
        let Some(cb) = u8_at(sttb, at).map(usize::from) else {
            break;
        };
        let Some(ffn) = sttb.get(at + 1..at + 1 + cb) else {
            break;
        };
        at += 1 + cb;
        // FFN: 39 bytes of metrics, then the name as NUL-terminated UTF-16.
        let units: Vec<u16> = ffn
            .get(39..)
            .unwrap_or(&[])
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        fonts.push(String::from_utf16_lossy(&units));
    }
    fonts
}

// ---- building the document ----

struct Formatting {
    chpx: Vec<FkpRun>,
    papx: Vec<FkpRun>,
    styles: Styles,
    fonts: Vec<String>,
    prcs: Vec<Vec<u8>>,
}

/// A paragraph being collected, and the table it may belong to.
struct Builder<'a> {
    fmt: &'a Formatting,
    pieces: &'a [Piece],
    body: Vec<Block>,
    rows: Vec<Row>,
    cells: Vec<Cell>,
    cell: Vec<Block>,
    columns: Vec<i16>,
    /// Run properties per (CHPX run, piece, paragraph style).
    props: HashMap<(Option<usize>, usize, u16), RunProps>,
}

/// What ends a paragraph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    /// A paragraph mark (0x0D) or a section mark.
    Paragraph,
    /// A cell or row end mark (0x07).
    Cell,
}

impl<'a> Builder<'a> {
    fn new(fmt: &'a Formatting, pieces: &'a [Piece]) -> Builder<'a> {
        Builder {
            fmt,
            pieces,
            body: Vec::new(),
            rows: Vec::new(),
            cells: Vec::new(),
            cell: Vec::new(),
            columns: Vec::new(),
            props: HashMap::new(),
        }
    }

    fn build(mut self, chars: &[Ch], section_ends: &BTreeSet<u32>) -> Vec<Block> {
        let mut start = 0usize;
        for (i, ch) in chars.iter().enumerate() {
            let end = match ch.c {
                '\r' => End::Paragraph,
                '\u{7}' => End::Cell,
                '\u{c}' if section_ends.contains(&(ch.cp + 1)) => End::Paragraph,
                _ => continue,
            };
            self.paragraph(&chars[start..=i], end);
            start = i + 1;
        }
        if start < chars.len() {
            self.paragraph(&chars[start..], End::Paragraph);
        }
        self.end_table();
        self.body
    }

    /// One paragraph: `chars` with its terminator last.
    fn paragraph(&mut self, chars: &[Ch], end: End) {
        let Some(mark) = chars.last() else {
            return;
        };
        let papx = run_at(&self.fmt.papx, mark.fc).map(|i| &self.fmt.papx[i]);
        let istd = papx.map_or(0, |r| r.istd);
        let pap = papx.map(|r| Pap::parse(&r.grpprl)).unwrap_or_default();
        let mut props = ParProps {
            align: pap.align.unwrap_or_default(),
            ..ParProps::default()
        };
        if let Some(n @ 1..=9) = self.fmt.styles.sti(istd) {
            props.style_id = Some(format!("Heading{n}"));
            props.heading_level = Some(n as u8);
        }
        let para = Paragraph {
            props,
            content: self.content(chars, istd, mark.c == '\u{c}'),
        };
        if pap.depth == 0 {
            self.end_table();
            self.body.push(Block::Paragraph(para));
            return;
        }
        // A nested table's row end mark has no text of its own.
        if pap.depth > 1 && pap.inner_row_end {
            return;
        }
        if pap.depth == 1 && pap.row_end {
            self.end_row(pap.columns);
            return;
        }
        self.cell.push(Block::Paragraph(para));
        if end == End::Cell && pap.depth == 1 {
            self.cells.push(Cell {
                blocks: std::mem::take(&mut self.cell),
                ..Cell::default()
            });
        }
    }

    fn end_row(&mut self, columns: Vec<i16>) {
        if !self.cell.is_empty() {
            let blocks = std::mem::take(&mut self.cell);
            self.cells.push(Cell {
                blocks,
                ..Cell::default()
            });
        }
        if !columns.is_empty() {
            self.columns = columns;
        }
        let mut cells = std::mem::take(&mut self.cells);
        if cells.is_empty() {
            cells.push(crate::table::empty_cell());
        }
        self.rows.push(Row {
            cells,
            ..Row::default()
        });
    }

    fn end_table(&mut self) {
        if !self.cell.is_empty() || !self.cells.is_empty() {
            self.end_row(Vec::new());
        }
        if self.rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        let cols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(1);
        let widths: Vec<u32> = self
            .columns
            .windows(2)
            .map(|w| (i32::from(w[1]) - i32::from(w[0])).max(1) as u32)
            .collect();
        let width = crate::table::DEFAULT_TEXT_WIDTH;
        let mut table = crate::table::new_table(1, cols, width, crate::table::AutoFit::Default);
        if widths.len() == cols {
            table.grid = widths;
        }
        table.rows = rows;
        self.columns.clear();
        self.body.push(Block::Table(table));
    }

    /// The inlines of a paragraph's characters (its terminator excluded):
    /// runs of equal formatting, tabs and breaks; field instructions and
    /// control characters are dropped. `section_end` adds the page break a
    /// section mark stands for.
    fn content(&mut self, chars: &[Ch], istd: u16, section_end: bool) -> Vec<Inline> {
        let mut out = Vec::new();
        let mut text = String::new();
        let mut text_props: Option<RunProps> = None;
        // One entry per open field: whether its result has begun.
        let mut fields: Vec<bool> = Vec::new();
        // How many of them are still in their instructions (hidden).
        let mut in_instructions = 0usize;
        let body = &chars[..chars.len().saturating_sub(1)];
        let flush = |out: &mut Vec<Inline>, text: &mut String, props: &Option<RunProps>| {
            if !text.is_empty() {
                out.push(Inline::Run(Run {
                    text: std::mem::take(text),
                    props: props.clone().unwrap_or_default(),
                }));
            }
        };
        for ch in body {
            match ch.c {
                '\u{13}' => {
                    fields.push(false);
                    in_instructions += 1;
                    continue;
                }
                '\u{14}' => {
                    if let Some(top) = fields.last_mut().filter(|result| !**result) {
                        *top = true;
                        in_instructions -= 1;
                    }
                    continue;
                }
                '\u{15}' => {
                    if fields.pop() == Some(false) {
                        in_instructions -= 1;
                    }
                    continue;
                }
                _ if in_instructions > 0 => continue,
                _ => {}
            }
            let props = self.run_props(ch, istd);
            let inline = match ch.c {
                '\t' => Some(Inline::Tab(props.clone())),
                '\u{b}' => Some(Inline::Break(BreakKind::Line, props.clone())),
                '\u{c}' => Some(Inline::Break(BreakKind::Page, props.clone())),
                _ => None,
            };
            if let Some(inline) = inline {
                flush(&mut out, &mut text, &text_props);
                out.push(inline);
                continue;
            }
            let c = match ch.c {
                '\u{1e}' => '\u{2011}',
                '\u{1f}' => '\u{ad}',
                c if (c as u32) < 0x20 => continue,
                c => c,
            };
            if text_props.as_ref() != Some(&props) {
                flush(&mut out, &mut text, &text_props);
                text_props = Some(props);
            }
            text.push(c);
        }
        flush(&mut out, &mut text, &text_props);
        if section_end {
            let props = chars
                .last()
                .map(|ch| self.run_props(ch, istd))
                .unwrap_or_default();
            out.push(Inline::Break(BreakKind::Page, props));
        }
        out
    }

    /// The direct run formatting of `ch` in a paragraph of style `istd`.
    fn run_props(&mut self, ch: &Ch, istd: u16) -> RunProps {
        let run = run_at(&self.fmt.chpx, ch.fc);
        let key = (run, ch.piece, istd);
        if let Some(p) = self.props.get(&key) {
            return p.clone();
        }
        let fmt = self.fmt;
        let mut chp = Chp::default();
        if let Some(i) = run {
            chp.apply(&fmt.chpx[i].grpprl);
        }
        // A piece's prm can point at a Prc whose sprms apply over the CHPX.
        if let Some(piece) = self.pieces.get(ch.piece) {
            if piece.prm & 1 != 0 {
                if let Some(g) = fmt.prcs.get((piece.prm >> 1) as usize) {
                    chp.apply(g);
                }
            }
        }
        let mut inherited = fmt.styles.toggles(istd);
        if let Some(cs) = chp.char_style {
            let c = fmt.styles.toggles(cs);
            for k in 0..3 {
                inherited[k] ^= c[k];
            }
        }
        let mut props = RunProps::default();
        const OFF: [&str; 3] = [
            "<w:b w:val=\"0\"/>",
            "<w:i w:val=\"0\"/>",
            "<w:strike w:val=\"0\"/>",
        ];
        let mut on = [false; 3];
        for k in 0..3 {
            // 0x80 says "as the style has it": no direct formatting.
            match chp.toggles[k] {
                Some(op) if op != 0x80 => {
                    on[k] = toggle(op, inherited[k]);
                    // Turning off what the style turns on is direct formatting
                    // Word writes as `w:val="0"`.
                    if !on[k] && inherited[k] {
                        props.raw_props.push(OFF[k].to_string());
                    }
                }
                _ => {}
            }
        }
        [props.bold, props.italic, props.strike] = on;
        props.underline = chp.underline.unwrap_or(false);
        props.size_half_pts = chp.size.filter(|&s| s > 0).map(u32::from);
        props.font = chp
            .font
            .and_then(|f| fmt.fonts.get(f as usize))
            .filter(|f| !f.is_empty())
            .cloned();
        props.color = chp.color.flatten();
        self.props.insert(key, props.clone());
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opccore::cfb::write_cfb;

    /// Where the synthetic documents put things in the WordDocument stream.
    const TEXT_AT: usize = 0x400;
    const CHPX_PAGE: u32 = 3;
    const PAPX_PAGE: u32 = 4;

    /// A minimal Word 97 `.doc`: a FIB, `text` as one piece (compressed when
    /// every unit fits a byte), and, when given, a CHPX page giving all of it
    /// `chpx`, a PAPX page giving every paragraph `papx` (and style 0), and a
    /// style sheet `stsh` (see [`normal_style_sheet`]). The WordDocument
    /// stream grows to hold a long text, which then can't have CHPX or PAPX
    /// pages (they sit at fixed pages after the text's start).
    struct Synth {
        text: Vec<u16>,
        compressed: bool,
        chpx: Option<Vec<u8>>,
        papx: Option<Vec<u8>>,
        stsh: Option<Vec<u8>>,
        nfib: u16,
        flags: u16,
    }

    /// A style sheet whose only style, 0 (Normal, a paragraph style), has
    /// the character formatting `chpx`.
    fn normal_style_sheet(chpx: &[u8]) -> Vec<u8> {
        let mut std = Vec::new();
        std.extend(0u16.to_le_bytes()); // sti 0: Normal
        std.extend((1u16 | 0x0FFF << 4).to_le_bytes()); // stk paragraph, no base
        std.extend(2u16.to_le_bytes()); // cupx 2: a PAPX and a CHPX
        std.extend([0; 4]); // bchUpe, grfstd
        std.extend(1u16.to_le_bytes()); // the name: 1 unit,
        std.extend(u16::from(b'N').to_le_bytes()); // "N",
        std.extend(0u16.to_le_bytes()); // and its terminator
        std.extend(2u16.to_le_bytes()); // UpxPapx: just the istd
        std.extend(0u16.to_le_bytes());
        std.extend((chpx.len() as u16).to_le_bytes()); // UpxChpx
        std.extend(chpx);
        let mut stsh = Vec::new();
        stsh.extend(4u16.to_le_bytes()); // cbStshi
        stsh.extend(1u16.to_le_bytes()); // cstd
        stsh.extend(10u16.to_le_bytes()); // cbSTDBaseInFile
        stsh.extend((std.len() as u16).to_le_bytes());
        stsh.extend(std);
        stsh
    }

    impl Synth {
        fn new(text: &str) -> Synth {
            Synth {
                text: text.encode_utf16().collect(),
                compressed: text.chars().all(|c| (c as u32) < 0x80),
                chpx: None,
                papx: None,
                stsh: None,
                nfib: 0x00C1,
                flags: 0x0200, // fWhichTblStm: 1Table
            }
        }

        fn bytes(&self) -> Vec<u8> {
            let (word, table) = self.streams();
            write_cfb(&[("WordDocument", word), ("1Table", table)])
        }

        fn streams(&self) -> (Vec<u8>, Vec<u8>) {
            let width = if self.compressed { 1 } else { 2 };
            let text_bytes = TEXT_AT + self.text.len() * width;
            assert!(
                text_bytes <= (CHPX_PAGE * 512) as usize
                    || (self.chpx.is_none() && self.papx.is_none()),
                "a long text has no room for CHPX or PAPX pages"
            );
            let mut word = vec![0u8; text_bytes.max(512 * 5)];
            let put16 = |b: &mut Vec<u8>, at: usize, v: u16| {
                b[at..at + 2].copy_from_slice(&v.to_le_bytes())
            };
            let put32 = |b: &mut Vec<u8>, at: usize, v: u32| {
                b[at..at + 4].copy_from_slice(&v.to_le_bytes())
            };
            put16(&mut word, 0, 0xA5EC);
            put16(&mut word, 2, self.nfib);
            put16(&mut word, 10, self.flags);
            put16(&mut word, 32, 14); // csw
            put16(&mut word, 62, 22); // cslw
            put32(&mut word, 64 + 12, self.text.len() as u32); // ccpText
            put16(&mut word, 152, 93); // cbRgFcLcb
            let pair = |word: &mut Vec<u8>, i: usize, fc: u32, lcb: u32| {
                put32(word, 154 + i * 8, fc);
                put32(word, 154 + i * 8 + 4, lcb);
            };
            for (k, &u) in self.text.iter().enumerate() {
                if self.compressed {
                    word[TEXT_AT + k] = u as u8;
                } else {
                    put16(&mut word, TEXT_AT + k * 2, u);
                }
            }
            let text_end = text_bytes as u32;

            let mut table = Vec::new();
            // Clx: a Pcdt with one piece.
            let clx_at = table.len() as u32;
            let fc = if self.compressed {
                (TEXT_AT as u32 * 2) | 0x4000_0000
            } else {
                TEXT_AT as u32
            };
            let mut plc = Vec::new();
            plc.extend(0u32.to_le_bytes());
            plc.extend((self.text.len() as u32).to_le_bytes());
            plc.extend(0u16.to_le_bytes());
            plc.extend(fc.to_le_bytes());
            plc.extend(0u16.to_le_bytes());
            table.push(0x02);
            table.extend((plc.len() as u32).to_le_bytes());
            table.extend(&plc);
            pair(&mut word, 33, clx_at, table.len() as u32 - clx_at);

            if let Some(g) = &self.chpx {
                // One run covering the whole text.
                let page = (CHPX_PAGE * 512) as usize;
                put32(&mut word, page, TEXT_AT as u32);
                put32(&mut word, page + 4, text_end);
                word[page + 8] = 0x80; // chpx at byte 0x100
                word[page + 0x100] = g.len() as u8;
                word[page + 0x101..page + 0x101 + g.len()].copy_from_slice(g);
                word[page + 511] = 1;
                let at = table.len() as u32;
                table.extend((TEXT_AT as u32).to_le_bytes());
                table.extend(text_end.to_le_bytes());
                table.extend(CHPX_PAGE.to_le_bytes());
                pair(&mut word, 12, at, 12);
            }
            if let Some(g) = &self.papx {
                // One PAPX for every paragraph: istd 0, then `g`.
                let page = (PAPX_PAGE * 512) as usize;
                put32(&mut word, page, TEXT_AT as u32);
                put32(&mut word, page + 4, text_end);
                word[page + 8] = 0x80; // BxPap.bOffset: papx at 0x100
                let mut grpprl = vec![0, 0];
                grpprl.extend(g);
                if grpprl.len() % 2 == 1 {
                    word[page + 0x100] = grpprl.len().div_ceil(2) as u8;
                    word[page + 0x101..page + 0x101 + grpprl.len()].copy_from_slice(&grpprl);
                } else {
                    word[page + 0x100] = 0;
                    word[page + 0x101] = (grpprl.len() / 2) as u8;
                    word[page + 0x102..page + 0x102 + grpprl.len()].copy_from_slice(&grpprl);
                }
                word[page + 511] = 1;
                let at = table.len() as u32;
                table.extend((TEXT_AT as u32).to_le_bytes());
                table.extend(text_end.to_le_bytes());
                table.extend(PAPX_PAGE.to_le_bytes());
                pair(&mut word, 13, at, 12);
            }
            if let Some(stsh) = &self.stsh {
                let at = table.len() as u32;
                table.extend(stsh);
                pair(&mut word, 1, at, stsh.len() as u32); // fcStshf
            }
            (word, table)
        }
    }

    fn paragraphs(doc: &Document) -> Vec<String> {
        doc.body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p.plain_text()),
                _ => None,
            })
            .collect()
    }

    fn first_run(doc: &Document) -> RunProps {
        match &doc.body[0] {
            Block::Paragraph(p) => match &p.content[0] {
                Inline::Run(r) => r.props.clone(),
                other => panic!("not a run: {other:?}"),
            },
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    #[test]
    fn plain_text_splits_into_paragraphs() {
        let doc = read_doc(&Synth::new("One\rTwo\tthree\x0bfour\r").bytes()).unwrap();
        assert_eq!(paragraphs(&doc), ["One", "Two\tthree\nfour"]);
        let Block::Paragraph(p) = &doc.body[1] else {
            panic!()
        };
        assert!(matches!(p.content[1], Inline::Tab(_)));
        assert!(matches!(p.content[3], Inline::Break(BreakKind::Line, _)));
    }

    #[test]
    fn compressed_text_is_cp1252() {
        // 0x80 euro, 0x93/0x94 curly double quotes, 0x97 em dash, 0xE9 é.
        let mut synth = Synth::new("");
        synth.text = [
            0x80u16,
            b' '.into(),
            0x93,
            b'q'.into(),
            0x94,
            0x97,
            0xE9,
            0x0D,
        ]
        .to_vec();
        synth.compressed = true;
        let doc = read_doc(&synth.bytes()).unwrap();
        assert_eq!(
            paragraphs(&doc),
            ["\u{20ac} \u{201c}q\u{201d}\u{2014}\u{e9}"]
        );
    }

    #[test]
    fn utf16_text_joins_surrogate_pairs() {
        let doc = read_doc(&Synth::new("Привет 😀 中文\r").bytes()).unwrap();
        assert_eq!(paragraphs(&doc), ["Привет 😀 中文"]);
    }

    #[test]
    fn fields_keep_their_result_and_drop_the_instruction() {
        let text = "Page \x13 PAGE \x141\x15 of \x13 NUMPAGES \x15here\r";
        let doc = read_doc(&Synth::new(text).bytes()).unwrap();
        assert_eq!(paragraphs(&doc), ["Page 1 of here"]);
    }

    /// Nested fields: an instruction holding a field shows nothing, and a
    /// result holding one shows that field's result.
    #[test]
    fn nested_fields_show_only_results() {
        let text = "a\x13 IF \x13 PAGE \x142\x15 \x14b\x13 PAGE \x143\x15c\x15d\r";
        let doc = read_doc(&Synth::new(text).bytes()).unwrap();
        assert_eq!(paragraphs(&doc), ["ab3cd"]);
    }

    /// FIX r2 m1: a paragraph of 100k fields, each left in its result, then
    /// 100k characters, reads in linear time (a scan of every open field per
    /// character would be 10^10 steps).
    #[test]
    fn many_open_fields_read_in_linear_time() {
        let n = 100_000;
        let mut text = "\x13\x14".repeat(n);
        text.push_str(&"x".repeat(n));
        text.push('\r');
        let bytes = Synth::new(&text).bytes();
        let started = std::time::Instant::now();
        let doc = read_doc(&bytes).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(paragraphs(&doc), ["x".repeat(n)]);
    }

    /// FIX r2 m2: bold toggles against a style that is bold. MS-DOC's
    /// toggle operands are 0 (off), 1 (on), 0x80 (as the style has it) and
    /// 0x81 (the opposite of the style). RunProps holds direct formatting:
    /// an explicit off over a bold style is Word's `<w:b w:val="0"/>`, kept
    /// in `raw_props`; "as the style has it" is no direct formatting.
    #[test]
    fn bold_toggles_resolve_against_a_bold_style() {
        const OFF: &str = "<w:b w:val=\"0\"/>";
        let run = |op: Option<u8>| {
            let mut synth = Synth::new("x\r");
            synth.stsh = Some(normal_style_sheet(&[0x35, 0x08, 1]));
            synth.chpx = op.map(|op| vec![0x35, 0x08, op]);
            first_run(&read_doc(&synth.bytes()).unwrap())
        };
        for (op, bold, raw) in [
            (Some(0x00), false, true),
            (Some(0x81), false, true),
            (Some(0x01), true, false),
            (Some(0x80), false, false),
            (None, false, false),
        ] {
            let p = run(op);
            assert_eq!(p.bold, bold, "{op:?}");
            assert_eq!(p.raw_props.iter().any(|r| r == OFF), raw, "{op:?}");
        }
        // Over a style that is not bold, 0x81 turns bold on and an off
        // is nothing to write.
        let plain = |op: u8| {
            let mut synth = Synth::new("x\r");
            synth.stsh = Some(normal_style_sheet(&[]));
            synth.chpx = Some(vec![0x35, 0x08, op]);
            first_run(&read_doc(&synth.bytes()).unwrap())
        };
        assert!(plain(0x81).bold);
        let off = plain(0x00);
        assert!(!off.bold && off.raw_props.is_empty());
    }

    /// FIX r1 M1: however many pieces claim the same CPs, and whatever
    /// ccpText says, the text is read once and never outgrows its stream.
    #[test]
    fn overlapping_pieces_are_read_once() {
        let word = vec![b'x'; 4096];
        let n = 20_000u32;
        let pieces: Vec<Piece> = (0..n)
            .map(|i| Piece {
                // Alternately the whole CP range and a backwards one.
                cp_start: if i % 2 == 0 { 0 } else { u32::MAX },
                cp_end: if i % 2 == 0 { u32::MAX } else { 0 },
                fc: 0,
                compressed: true,
                prm: 0,
            })
            .collect();
        let started = std::time::Instant::now();
        let chars = decode_text(&word, &pieces, u32::MAX);
        assert_eq!(chars.len(), word.len());
        assert!(chars.windows(2).all(|w| w[0].cp < w[1].cp));
        // A piece reaching back is clipped to the CPs after what was read.
        let clipped = [
            Piece {
                cp_start: 0,
                cp_end: 10,
                fc: 0,
                compressed: true,
                prm: 0,
            },
            Piece {
                cp_start: 5,
                cp_end: 20,
                fc: 100,
                compressed: true,
                prm: 0,
            },
        ];
        let chars = decode_text(&word, &clipped, 1000);
        assert_eq!(chars.len(), 20);
        assert_eq!(chars[10].fc, 105);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// FIX r1 M2: a bin table naming the same pages over and over (in any
    /// order) parses each page once.
    #[test]
    fn a_repeating_bin_table_parses_each_page_once() {
        let mut word = vec![0u8; 512 * 6];
        for pn in [3usize, 4] {
            let page = pn * 512;
            word[page..page + 4].copy_from_slice(&(pn as u32 * 100).to_le_bytes());
            word[page + 4..page + 8].copy_from_slice(&(pn as u32 * 100 + 50).to_le_bytes());
            word[page + 8] = 0x80;
            word[page + 0x100] = 3;
            word[page + 0x101..page + 0x104].copy_from_slice(&[0x35, 0x08, 1]);
            word[page + 511] = 1;
        }
        let n = 20_000usize;
        let mut table = Vec::new();
        for i in 0..=n {
            table.extend((i as u32).to_le_bytes());
        }
        for i in 0..n {
            // A, B, A, B, ..., and a page past the stream now and then.
            let pn: u32 = match i % 3 {
                0 => 3,
                1 => 4,
                _ => 9_999,
            };
            table.extend(pn.to_le_bytes());
        }
        let bte = FcLcb {
            fc: 0,
            lcb: table.len() as u32,
        };
        let started = std::time::Instant::now();
        let runs = fkp_runs(&word, &table, bte, FkpKind::Chpx);
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].start, runs[1].start), (300, 400));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn character_sprms_map_to_run_props() {
        let mut synth = Synth::new("x\r");
        synth.chpx = Some(vec![
            0x35, 0x08, 1, // sprmCFBold
            0x36, 0x08, 1, // sprmCFItalic
            0x37, 0x08, 1, // sprmCFStrike
            0x3E, 0x2A, 1, // sprmCKul single
            0x43, 0x4A, 32, 0, // sprmCHps 16pt
            0x70, 0x68, 0x12, 0x34, 0x56, 0, // sprmCCv
        ]);
        let p = first_run(&read_doc(&synth.bytes()).unwrap());
        assert!(p.bold && p.italic && p.strike && p.underline);
        assert_eq!(p.size_half_pts, Some(32));
        assert_eq!(p.color.as_deref(), Some("123456"));
    }

    #[test]
    fn unknown_sprms_are_skipped_by_their_size() {
        let mut synth = Synth::new("x\r");
        synth.chpx = Some(vec![
            0x99, 0x26, 7, // spra 1: one byte
            0x99, 0x46, 1, 2, // spra 2: two bytes
            0x99, 0x66, 1, 2, 3, 4, // spra 3: four bytes
            0x99, 0xE6, 1, 2, 3, // spra 7: three bytes
            0x99, 0xC6, 3, 9, 9, 9, // spra 6: a length byte, then that many
            0x35, 0x08, 1, // sprmCFBold, after all of them
            0x99, 0x66, 1, // a four-byte operand cut short: the walk stops
        ]);
        assert!(first_run(&read_doc(&synth.bytes()).unwrap()).bold);
    }

    #[test]
    fn paragraph_alignment_from_papx() {
        let mut synth = Synth::new("a\rb\r");
        synth.papx = Some(vec![0x03, 0x24, 1]); // sprmPJc80 center
        let doc = read_doc(&synth.bytes()).unwrap();
        for b in &doc.body {
            let Block::Paragraph(p) = b else { panic!() };
            assert_eq!(p.props.align, Align::Center);
        }
    }

    #[test]
    fn table_cells_and_rows_from_cell_marks() {
        // One PAPX for every paragraph, so there is no TTP: the cells
        // collect into one row, which the table's end closes.
        let mut synth = Synth::new("a\x07b\x07");
        synth.papx = Some(vec![0x16, 0x24, 1]); // sprmPFInTable
        let doc = read_doc(&synth.bytes()).unwrap();
        let Block::Table(t) = &doc.body[0] else {
            panic!("{doc:?}")
        };
        assert_eq!(t.rows.len(), 1);
        let texts: Vec<String> = t.rows[0]
            .cells
            .iter()
            .map(|c| c.blocks.iter().map(Block::plain_text).collect())
            .collect();
        assert_eq!(texts, ["a", "b"]);
    }

    #[test]
    fn import_sets_compatibility_mode_11() {
        let pkg = import_doc(&Synth::new("Hello\r").bytes()).unwrap();
        assert_eq!(pkg.compatibility_mode(), Some(11));
        let saved = crate::package::save_package(&pkg);
        let back = crate::package::load_package(&saved).unwrap();
        assert_eq!(back.compatibility_mode(), Some(11));
        assert_eq!(paragraphs(&back.document), ["Hello"]);
    }

    #[test]
    fn hard_errors() {
        let mut word95 = Synth::new("x\r");
        word95.nfib = 0x0065;
        assert_eq!(
            read_doc(&word95.bytes()),
            Err(DocImportError::Word95 { nfib: 0x0065 })
        );
        let mut word97 = Synth::new("x\r");
        word97.nfib = 0x00C0;
        assert!(read_doc(&word97.bytes()).is_ok());

        let mut encrypted = Synth::new("x\r");
        encrypted.flags |= 0x0100;
        assert_eq!(read_doc(&encrypted.bytes()), Err(DocImportError::Encrypted));
        let mut obfuscated = Synth::new("x\r");
        obfuscated.flags |= 0x8000;
        assert_eq!(
            read_doc(&obfuscated.bytes()),
            Err(DocImportError::Encrypted)
        );

        let package = write_cfb(&[
            ("EncryptionInfo", vec![4, 0, 4, 0]),
            ("EncryptedPackage", vec![0; 16]),
        ]);
        let e = read_doc(&package).unwrap_err();
        assert_eq!(e, DocImportError::EncryptedPackage);
        assert!(!e.to_string().contains("97-2003"), "{e}");

        let workbook = write_cfb(&[("Workbook", vec![0; 64])]);
        assert_eq!(read_doc(&workbook), Err(DocImportError::NotWordDocument));

        let (mut word, table) = Synth::new("x\r").streams();
        word[0] = 0;
        let not_fib = write_cfb(&[("WordDocument", word), ("1Table", table)]);
        assert_eq!(read_doc(&not_fib), Err(DocImportError::NotWordDocument));

        assert!(matches!(
            read_doc(b"PK\x03\x04 not a compound file"),
            Err(DocImportError::Corrupt(_))
        ));
        for e in [
            DocImportError::Encrypted,
            DocImportError::EncryptedPackage,
            DocImportError::Word95 { nfib: 0x65 },
            DocImportError::NotWordDocument,
            DocImportError::Corrupt("x".into()),
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    /// Truncations and bit flips of a document never panic, and the whole
    /// loop stays fast: a length read from the file can't make the reader
    /// allocate or loop past what the streams hold.
    #[test]
    fn damaged_documents_never_panic() {
        let mut synth = Synth::new("Hello\x07world\x07\x07after \x13 PAGE \x141\x15\r");
        synth.chpx = Some(vec![0x35, 0x08, 1, 0x43, 0x4A, 24, 0]);
        synth.papx = Some(vec![0x16, 0x24, 1, 0x17, 0x24, 1]);
        let (word, table) = synth.streams();
        let started = std::time::Instant::now();
        let mut state = 0x2545_F491u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for round in 0..2000 {
            let (mut w, mut t) = (word.clone(), table.clone());
            let target = if round % 2 == 0 { &mut w } else { &mut t };
            match round % 3 {
                0 => {
                    let n = next() as usize % target.len();
                    target.truncate(n);
                }
                _ => {
                    for _ in 0..4 {
                        let at = next() as usize % target.len().max(1);
                        if let Some(b) = target.get_mut(at) {
                            *b ^= 1 << (next() % 8);
                        }
                    }
                }
            }
            // Also set the lengths that size allocations and loops: ccpText,
            // and an fc or lcb of the FIB's table of structures.
            if round % 5 == 0 && w.len() > 80 {
                w[76..80].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            if round % 7 == 0 {
                let at = 154 + (next() as usize % (93 * 2)) * 4;
                let v = [u32::MAX, next(), 0x7FFF_FFFF, 1][next() as usize % 4];
                if let Some(b) = w.get_mut(at..at + 4) {
                    b.copy_from_slice(&v.to_le_bytes());
                }
            }
            let _ = read_doc(&write_cfb(&[("WordDocument", w), ("1Table", t)]));
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }
}
