//! The Excel 97-2003 `.xls` writer (#874): a workbook as a BIFF8 `Workbook`
//! stream in a compound file, the inverse of [`super::xls`]. Saving a
//! workbook in Compatibility Mode writes this.
//!
//! The whole file is written from the model, so it reflects every edit:
//! sheets, cells, formulas, number formats, defined names, the date system,
//! the formatting the model holds (fonts, fills, alignment, wrap, a thin
//! box border, quote prefix), merged cells, column widths, row heights,
//! hidden rows, columns and sheets, and frozen panes. The record layouts and
//! their order follow what Excel writes (corpus/legacy's `.xls` files).
//!
//! What BIFF8 or this writer can't hold is left out and named in
//! [`XlsOut::lost`]: cells past the 65,536 × 256 grid, formulas that can't
//! be compiled ([`super::ptg_write::Why`]; the cell keeps its value), and
//! the model features this writer has no records for (tables, conditional
//! formatting, data validation, hyperlinks, drawings, comments, ...).

use std::collections::{BTreeMap, HashMap};

use crate::sheet::{Align, Cell, CellValue, Sheet, Workbook, Xf};
use crate::xlsx::SheetPackage;

use super::ptg_write::{self, Book, Compiled, Site, Tables, Why};

/// A written `.xls` and what it couldn't hold.
#[derive(Clone, Debug, Default)]
pub struct XlsOut {
    /// The compound file.
    pub bytes: Vec<u8>,
    /// What the workbook has that the file lacks, one entry per kind, in
    /// words for a "will be lost" list ("3 formulas with references past
    /// row 65,536 or column IV (saved as values)"). Empty when nothing is
    /// lost.
    pub lost: Vec<String>,
}

/// The longest record body; the rest of a longer one goes in CONTINUE
/// records.
pub(crate) const MAX_BODY: usize = 8224;
const CONTINUE: u16 = 0x003C;
const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
/// The style XFs every BIFF8 file starts with; cell XFs follow.
const STYLE_XFS: usize = 15;
/// The most XF records Excel 97-2003 takes.
const MAX_XFS: usize = 4050;
/// The most FONT records (index 4 is never used).
const MAX_FONTS: usize = 511;
/// The most characters a cell's text holds.
const MAX_TEXT: usize = 32_767;

/// BIFF8's default palette: the colours of indices 8..=63.
pub(crate) const DEFAULT_PALETTE: [(u8, u8, u8); 56] = [
    (0x00, 0x00, 0x00),
    (0xFF, 0xFF, 0xFF),
    (0xFF, 0x00, 0x00),
    (0x00, 0xFF, 0x00),
    (0x00, 0x00, 0xFF),
    (0xFF, 0xFF, 0x00),
    (0xFF, 0x00, 0xFF),
    (0x00, 0xFF, 0xFF),
    (0x80, 0x00, 0x00),
    (0x00, 0x80, 0x00),
    (0x00, 0x00, 0x80),
    (0x80, 0x80, 0x00),
    (0x80, 0x00, 0x80),
    (0x00, 0x80, 0x80),
    (0xC0, 0xC0, 0xC0),
    (0x80, 0x80, 0x80),
    (0x99, 0x99, 0xFF),
    (0x99, 0x33, 0x66),
    (0xFF, 0xFF, 0xCC),
    (0xCC, 0xFF, 0xFF),
    (0x66, 0x00, 0x66),
    (0xFF, 0x80, 0x80),
    (0x00, 0x66, 0xCC),
    (0xCC, 0xCC, 0xFF),
    (0x00, 0x00, 0x80),
    (0xFF, 0x00, 0xFF),
    (0xFF, 0xFF, 0x00),
    (0x00, 0xFF, 0xFF),
    (0x80, 0x00, 0x80),
    (0x80, 0x00, 0x00),
    (0x00, 0x80, 0x80),
    (0x00, 0x00, 0xFF),
    (0x00, 0xCC, 0xFF),
    (0xCC, 0xFF, 0xFF),
    (0xCC, 0xFF, 0xCC),
    (0xFF, 0xFF, 0x99),
    (0x99, 0xCC, 0xFF),
    (0xFF, 0x99, 0xCC),
    (0xCC, 0x99, 0xFF),
    (0xFF, 0xCC, 0x99),
    (0x33, 0x66, 0xFF),
    (0x33, 0xCC, 0xCC),
    (0x99, 0xCC, 0x00),
    (0xFF, 0xCC, 0x00),
    (0xFF, 0x99, 0x00),
    (0xFF, 0x66, 0x00),
    (0x66, 0x66, 0x99),
    (0x96, 0x96, 0x96),
    (0x00, 0x33, 0x66),
    (0x33, 0x99, 0x66),
    (0x00, 0x33, 0x00),
    (0x33, 0x33, 0x00),
    (0x99, 0x33, 0x00),
    (0x99, 0x33, 0x66),
    (0x33, 0x33, 0x99),
    (0x33, 0x33, 0x33),
];

/// The built-in number formats an `.xls` names by id: the fixed ones (0-4,
/// 9-22, 37-40, 45-49) and the currency and accounting ones Excel writes a
/// FORMAT record for (5-8, 41-44). The others (23-36, 50-58) depend on the
/// locale with no record, so a code that matches one is written as a
/// custom format.
const BUILTIN_FORMAT_IDS: [u16; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 37, 38, 39,
    40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
];
const LOCALE_FORMAT_IDS: [u16; 8] = [5, 6, 7, 8, 41, 42, 43, 44];

/// The automatic (window text) colour index.
const ICV_AUTO: u16 = 0x7FFF;

/// What a save lost, counted by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Loss {
    Formula(Why),
    CellsPastGrid,
    MergesPastGrid,
    Names,
    LongText,
    Xfs,
    Fonts,
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

impl Loss {
    fn describe(self, n: usize) -> String {
        let f = |what: &str| {
            format!(
                "{} {what} (saved as values)",
                plural(n, "formula", "formulas")
            )
        };
        match self {
            Loss::Formula(Why::PastGrid) => f("with references past row 65,536 or column IV"),
            Loss::Formula(Why::LongText) => f("with text longer than 255 characters"),
            Loss::Formula(Why::Lambda) => f("with LAMBDA or LET"),
            Loss::Formula(Why::OtherBook) => f("with references to other workbooks"),
            Loss::Formula(Why::TooLong) => f("longer than Excel 97-2003 allows"),
            Loss::Formula(Why::Unsupported) => f("Excel 97-2003 cannot hold"),
            Loss::CellsPastGrid => format!(
                "{} beyond row 65,536 or column IV",
                plural(n, "cell", "cells")
            ),
            Loss::MergesPastGrid => format!(
                "{} beyond row 65,536 or column IV",
                plural(n, "merged range", "merged ranges")
            ),
            Loss::Names => format!(
                "{} Excel 97-2003 cannot hold",
                plural(n, "defined name", "defined names")
            ),
            Loss::LongText => format!(
                "{} longer than 32,767 characters (shortened)",
                plural(n, "cell text", "cell texts")
            ),
            Loss::Xfs => format!(
                "{} past Excel 97-2003's limit of 4,000 (merged into the default)",
                plural(n, "cell format", "cell formats")
            ),
            Loss::Fonts => format!(
                "{} past Excel 97-2003's limit of 512 (merged into the default)",
                plural(n, "font", "fonts")
            ),
        }
    }
}

/// Write `pkg`'s workbook as an `.xls`.
pub fn write_xls(pkg: &SheetPackage) -> XlsOut {
    let wb = &pkg.workbook;
    let mut lost: BTreeMap<Loss, usize> = BTreeMap::new();
    let extras = loaded_extras(pkg);
    let styles = StyleTable::new(wb, &extras, &mut lost);
    let mut refs = Refs::new(pkg);

    // Defined names first: their NAME records come first, so the indices
    // formulas name them by are known before any formula is compiled.
    let mut name_rgce: Vec<Option<Compiled>> = Vec::new();
    for d in &wb.defined_names {
        // A NAME is one record: its 14 fixed bytes, the name (a flags byte
        // and up to two bytes a character), the tokens and extra data.
        let fits = |c: &Compiled| {
            14 + 1 + 2 * d.name.encode_utf16().count() + c.rgce.len() + c.extra.len() <= MAX_BODY
        };
        let c = ptg_write::compile(&d.formula, Site::Name { scope: d.scope }, &mut refs)
            .ok()
            .filter(fits);
        if c.is_none() {
            *lost.entry(Loss::Names).or_default() += 1;
        }
        name_rgce.push(c);
    }

    let mut sst = Sst::default();
    let sheets: Vec<SheetOut> = wb
        .sheets
        .iter()
        .enumerate()
        .map(|(i, s)| sheet_records(wb, i, s, &styles, &mut refs, &mut sst, &mut lost))
        .collect();

    let mut book = Vec::new();
    let mut ws_pos = Vec::new();
    globals(&mut book, wb, &styles, &refs, &name_rgce, &sst, &mut ws_pos);
    for (i, sheet) in sheets.iter().enumerate() {
        let at = book.len() as u32;
        // BOUNDSHEET8.lbPlyPos: where the sheet's BOF is.
        book[ws_pos[i]..ws_pos[i] + 4].copy_from_slice(&at.to_le_bytes());
        book.extend_from_slice(&sheet.bytes);
    }
    // Excel-compatible writers keep the stream out of the mini stream.
    if book.len() < 4096 {
        book.resize(4096, 0);
    }

    let mut out: Vec<String> = lost.into_iter().map(|(kind, n)| kind.describe(n)).collect();
    out.extend(unsupported_features(pkg, &extras));
    XlsOut {
        bytes: opccore::cfb::write_cfb(&[("Workbook", book)]),
        lost: out,
    }
}

/// The model features this writer has no records for, in Excel's words,
/// each named once when the workbook has any.
fn unsupported_features(pkg: &SheetPackage, extras: &[crate::xlsx::LoadedXfExtras]) -> Vec<String> {
    let wb = &pkg.workbook;
    let any = |f: &dyn Fn(&Sheet) -> bool| wb.sheets.iter().any(f);
    let mut out: Vec<&str> = Vec::new();
    if !wb.tables.is_empty() {
        out.push("tables (their cells are kept as plain ranges)");
    }
    if any(&|s| !s.cond_formats.is_empty()) {
        out.push("conditional formatting");
    }
    if any(&|s| !s.validations.is_empty()) {
        out.push("data validation");
    }
    if any(&|s| !s.hyperlinks.is_empty()) {
        out.push("hyperlinks");
    }
    if any(&|s| !s.drawings.is_empty()) {
        out.push("pictures, shapes and charts");
    }
    if !pkg.comments().is_empty() {
        out.push("comments");
    }
    if !wb.pivots.is_empty() {
        out.push("PivotTables");
    }
    if any(&|s| s.auto_filter.is_some()) {
        out.push("AutoFilter");
    }
    if any(&|s| s.protection.is_some()) {
        out.push("sheet protection");
    }
    if any(&|s| custom_page_setup(&s.page_setup)) {
        out.push("page setup");
    }
    if any(&|s| !s.row_breaks.is_empty() || !s.col_breaks.is_empty()) {
        out.push("page breaks");
    }
    if any(&|s| s.max_row_outline() > 0 || s.max_col_outline() > 0) {
        out.push("outlines (grouped rows and columns)");
    }
    let used: std::collections::BTreeSet<u32> = wb
        .sheets
        .iter()
        .flat_map(|s| s.cells.values().map(|c| c.style))
        .collect();
    if used.iter().any(|&i| {
        let x = wb.styles.xf(i);
        x.fill_unresolved || x.color_unresolved
    }) {
        out.push("theme colors");
    }
    // What a loaded .xlsx format has beyond the model's xf (its border,
    // underline, vertical alignment, ...), which the .xlsx writer keeps
    // through `Xf::loaded_from` and this writer can't.
    for &i in &used {
        let Some(e) = wb
            .styles
            .xf(i)
            .loaded_from
            .and_then(|k| extras.get(k as usize))
        else {
            continue;
        };
        for &what in &e.lost {
            if !out.contains(&what) {
                out.push(what);
            }
        }
    }
    // A column's or row's own format (`<col style>`, `<row s>`).
    if any(&|s| {
        s.col_defs
            .iter()
            .any(|d| crate::sheet::xml_attr(&d.attrs, "style").is_some_and(|v| v != "0"))
            || s.row_attrs
                .values()
                .any(|a| crate::sheet::xml_attr(a, "s").is_some_and(|v| v != "0"))
    }) {
        out.push("column and row formats");
    }
    // The .xls gets no property-set streams (#874 follow-up).
    let p = pkg.doc_properties();
    let set = |v: &Option<String>| v.as_deref().is_some_and(|t| !t.is_empty());
    if [
        &p.title,
        &p.subject,
        &p.creator,
        &p.keywords,
        &p.description,
        &p.last_modified_by,
        &p.category,
        &p.company,
        &p.manager,
        &p.hyperlink_base,
    ]
    .into_iter()
    .any(set)
        || !p.custom.is_empty()
    {
        out.push("document properties");
    }
    let mut v: Vec<String> = out.into_iter().map(str::to_string).collect();
    // What the .xlsx writer keeps from the loaded parts and this writer has
    // no record for.
    for what in crate::xlsx::settings_xls_lacks(pkg) {
        if !v.contains(&what) {
            v.push(what);
        }
    }
    for m in pkg.macro_features() {
        v.push(m.to_string());
    }
    v
}

/// Whether a sheet's page setup has something a person set, which the
/// `.xls` would lose: Excel's defaults (portrait, 100%, Normal margins, no
/// header or footer, ...) aren't worth a warning. Nor is the paper size,
/// which is the printer's locale default as often as a choice.
fn custom_page_setup(p: &crate::print::setup::PageSetup) -> bool {
    let d = crate::print::setup::PageSetup::default();
    let hf = &p.header_footer;
    let text = [
        &hf.odd_header,
        &hf.odd_footer,
        &hf.even_header,
        &hf.even_footer,
        &hf.first_header,
        &hf.first_footer,
    ]
    .iter()
    .any(|t| t.as_deref().is_some_and(|t| !t.is_empty()));
    p.orientation.is_landscape()
        || p.scale != d.scale
        || p.fit_to_page
        || p.margins != d.margins
        || p.first_page_number.is_some()
        || p.page_order != d.page_order
        || p.black_and_white
        || p.draft
        || p.cell_comments != d.cell_comments
        || p.errors != d.errors
        || p.grid_lines
        || p.headings
        || p.h_centered
        || p.v_centered
        || text
}

/// What each loaded `<cellXfs>` entry of the package's styles part has
/// beyond the model's xf ([`crate::xlsx::loaded_xf_extras`]).
fn loaded_extras(pkg: &SheetPackage) -> Vec<crate::xlsx::LoadedXfExtras> {
    pkg.part("xl/styles.xml")
        .map(|b| crate::xlsx::loaded_xf_extras(&String::from_utf8_lossy(b)))
        .unwrap_or_default()
}

// ---- records ----------------------------------------------------------------

/// A record: id, length, body. The body must fit [`MAX_BODY`].
fn rec(out: &mut Vec<u8>, ty: u16, body: &[u8]) {
    debug_assert!(body.len() <= MAX_BODY, "{ty:#06x}: {}", body.len());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(body);
}

/// A record whose body ends with a string's characters (`units`), split
/// over CONTINUE records where it runs past [`MAX_BODY`]: each piece starts
/// with the string's flags byte again. `head` (the fields before the
/// characters, the string's count and flags included) never splits.
fn rec_with_string(out: &mut Vec<u8>, ty: u16, head: &[u8], units: &[u16]) {
    let high = units.iter().any(|&u| u >= 0x100);
    let width = if high { 2 } else { 1 };
    let mut body = head.to_vec();
    let mut left = units;
    let mut ty = ty;
    loop {
        let room = (MAX_BODY - body.len()) / width;
        let n = room.min(left.len());
        for &u in &left[..n] {
            if high {
                body.extend_from_slice(&u.to_le_bytes());
            } else {
                body.push(u as u8);
            }
        }
        rec(out, ty, &body);
        left = &left[n..];
        if left.is_empty() {
            return;
        }
        ty = CONTINUE;
        body = vec![high as u8];
    }
}

fn u16s(vals: &[u16]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A BOF: BIFF8, of substream type `dt`, with the build Excel 2007+ writes.
fn bof(out: &mut Vec<u8>, dt: u16) {
    let mut b = u16s(&[0x0600, dt, 0x4F5A, 0x07CD]);
    b.extend_from_slice(&0x0002_00C1u32.to_le_bytes());
    b.extend_from_slice(&0x0000_0806u32.to_le_bytes());
    rec(out, BOF, &b);
}

/// An `XLUnicodeString` (16-bit count) as bytes.
fn xl_string(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = (units.len() as u16).to_le_bytes().to_vec();
    ptg_write::push_chars(&mut out, &units);
    out
}

// ---- styles -----------------------------------------------------------------

/// A FONT as the model's xf describes it.
#[derive(Clone, Debug, PartialEq)]
struct Font {
    name: String,
    /// Twips.
    height: u16,
    bold: bool,
    italic: bool,
    color: Option<(u8, u8, u8)>,
}

impl Font {
    fn of(xf: &Xf) -> Font {
        let mut name = xf.font_name.clone().unwrap_or_else(|| "Calibri".into());
        // A font name is at most 31 characters in BIFF8.
        if name.chars().count() > 31 {
            name = name.chars().take(31).collect();
        }
        let size = xf.font_size.unwrap_or(11.0);
        Font {
            name,
            height: (size * 20.0).round().clamp(20.0, 8191.0) as u16,
            bold: xf.bold,
            italic: xf.italic,
            color: xf.color,
        }
    }
}

/// The workbook's FONT, FORMAT, XF and PALETTE records, and each model xf's
/// XF index.
struct StyleTable {
    fonts: Vec<Font>,
    /// (id, code) of each FORMAT a custom code needs.
    formats: Vec<(u16, String)>,
    /// One cell XF per model xf (or per the first [`MAX_XFS`] of them):
    /// (font index, format id, xf).
    xfs: Vec<(u16, u16, Xf)>,
    /// The XF index of each model xf.
    xf_of: Vec<u16>,
    palette: [(u8, u8, u8); 56],
    custom_palette: bool,
}

impl StyleTable {
    fn new(
        wb: &Workbook,
        extras: &[crate::xlsx::LoadedXfExtras],
        lost: &mut BTreeMap<Loss, usize>,
    ) -> StyleTable {
        let model = &wb.styles.xfs;
        let default = Font::of(&Xf::default());
        // Fonts 0-3 are the default font, as Excel writes them; 4 is never
        // used, so the next font is 5.
        let mut fonts = vec![default.clone(); 4];
        let mut formats: Vec<(u16, String)> = Vec::new();
        let mut xfs = Vec::new();
        let mut xf_of = Vec::new();
        let mut colors: Vec<(u8, u8, u8)> = Vec::new();
        let mut want = |c: Option<(u8, u8, u8)>| {
            if let Some(c) = c {
                if !colors.contains(&c) {
                    colors.push(c);
                }
            }
        };
        for (i, x) in model.iter().enumerate() {
            if STYLE_XFS + i >= MAX_XFS {
                *lost.entry(Loss::Xfs).or_default() += 1;
                xf_of.push(STYLE_XFS as u16);
                continue;
            }
            let font = Font::of(x);
            let ifnt = match fonts.iter().position(|f| *f == font) {
                Some(i) => i,
                None if fonts.len() + 1 < MAX_FONTS => {
                    want(font.color);
                    fonts.push(font);
                    fonts.len() - 1
                }
                None => {
                    *lost.entry(Loss::Fonts).or_default() += 1;
                    0
                }
            };
            // Index 4 doesn't exist: fonts from the 5th on are numbered one up.
            let ifnt = if ifnt >= 4 { ifnt + 1 } else { ifnt } as u16;
            want(x.fill);
            let ifmt = match x.code.as_deref() {
                None => 0,
                Some(code) => match BUILTIN_FORMAT_IDS
                    .iter()
                    .copied()
                    .find(|&id| crate::numfmt::builtin_code(id as u32).is_some_and(|b| b == code))
                {
                    // The currency and accounting formats depend on the
                    // locale, so Excel writes a FORMAT record for them,
                    // keeping the id; the rest are fixed.
                    Some(id) => {
                        if LOCALE_FORMAT_IDS.contains(&id) && !formats.iter().any(|f| f.0 == id) {
                            formats.push((id, code.to_string()));
                        }
                        id
                    }
                    None => match formats.iter().find(|(_, c)| c == code) {
                        Some((id, _)) => *id,
                        None => {
                            let id = 164 + formats.iter().filter(|f| f.0 >= 164).count() as u16;
                            formats.push((id, code.to_string()));
                            id
                        }
                    },
                },
            };
            xf_of.push((STYLE_XFS + xfs.len()) as u16);
            // A loaded .xlsx format whose border is a thin box (which the
            // .xlsx writer keeps through `loaded_from`) has the .xls one.
            let mut x = x.clone();
            x.border |= x
                .loaded_from
                .and_then(|k| extras.get(k as usize))
                .is_some_and(|e| e.thin_box);
            xfs.push((ifnt, ifmt, x));
        }
        if xfs.is_empty() {
            xfs.push((0, 0, Xf::default()));
        }
        // The palette: the default one, with each colour it lacks put in a
        // slot no needed colour uses; past 56 colours, the nearest.
        let mut palette = DEFAULT_PALETTE;
        // Slot 0 (palette 8, black) stays black: a thin box border is
        // written in it.
        let mut taken = [false; 56];
        taken[0] = true;
        for c in &colors {
            if let Some(i) = palette.iter().position(|p| p == c) {
                taken[i] = true;
            }
        }
        let mut custom_palette = false;
        for c in &colors {
            if palette.contains(c) {
                continue;
            }
            if let Some(i) = taken.iter().position(|t| !t) {
                palette[i] = *c;
                taken[i] = true;
                custom_palette = true;
            }
        }
        StyleTable {
            fonts,
            formats,
            xfs,
            xf_of,
            palette,
            custom_palette,
        }
    }

    /// The XF index of model xf `style`.
    fn xf(&self, style: u32) -> u16 {
        self.xf_of
            .get(style as usize)
            .copied()
            .unwrap_or(STYLE_XFS as u16)
    }

    /// The palette index of `c`: its slot, else the nearest colour's.
    fn icv(&self, c: (u8, u8, u8)) -> u16 {
        let d = |p: &(u8, u8, u8)| {
            let s = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
            s(p.0, c.0) + s(p.1, c.1) + s(p.2, c.2)
        };
        let i = (0..56).min_by_key(|&i| d(&self.palette[i])).unwrap_or(0);
        8 + i as u16
    }

    fn write(&self, out: &mut Vec<u8>) {
        for f in &self.fonts {
            let mut b = Vec::new();
            b.extend_from_slice(&f.height.to_le_bytes());
            b.extend_from_slice(&(if f.italic { 0x0002u16 } else { 0 }).to_le_bytes());
            let icv = f.color.map_or(ICV_AUTO, |c| self.icv(c));
            b.extend_from_slice(&icv.to_le_bytes());
            b.extend_from_slice(&(if f.bold { 700u16 } else { 400 }).to_le_bytes());
            // sss, uls, bFamily, bCharSet, reserved.
            b.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
            b.extend_from_slice(&ptg_write::short_string(&f.name).unwrap_or_else(|| vec![0, 0]));
            rec(out, 0x0031, &b);
        }
        for (id, code) in &self.formats {
            let mut b = id.to_le_bytes().to_vec();
            b.extend_from_slice(&xl_string(code));
            rec(out, 0x041E, &b);
        }
        // The style XFs, as Excel writes them: Normal and 14 copies.
        let style = [
            0, 0, 0, 0, 0xF5, 0xFF, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0x20,
        ];
        for _ in 0..STYLE_XFS {
            rec(out, 0x00E0, &style);
        }
        for (ifnt, ifmt, x) in &self.xfs {
            rec(out, 0x00E0, &self.cell_xf(*ifnt, *ifmt, x));
        }
        // STYLE: Normal is built-in style 0 on XF 0.
        rec(out, 0x0293, &[0x00, 0x80, 0x00, 0xFF]);
        if self.custom_palette {
            let mut b = 56u16.to_le_bytes().to_vec();
            for &(r, g, bl) in &self.palette {
                b.extend_from_slice(&[r, g, bl, 0]);
            }
            rec(out, 0x0092, &b);
        }
    }

    fn cell_xf(&self, ifnt: u16, ifmt: u16, x: &Xf) -> [u8; 20] {
        let mut b = [0u8; 20];
        b[0..2].copy_from_slice(&ifnt.to_le_bytes());
        b[2..4].copy_from_slice(&ifmt.to_le_bytes());
        // Locked, a cell XF on the Normal style; f123Prefix for a quote prefix.
        let flags: u16 = 0x0001 | if x.quote_prefix { 0x0008 } else { 0 };
        b[4..6].copy_from_slice(&flags.to_le_bytes());
        let alc = match x.align {
            Align::General => 0,
            Align::Left => 1,
            Align::Center => 2,
            Align::Right => 3,
        };
        // Horizontal alignment, wrap, and bottom vertical alignment (2).
        b[6] = alc | if x.wrap { 0x08 } else { 0 } | 0x20;
        // The attribute groups this XF sets apart from its style.
        b[9] = 0xFC;
        if x.border {
            // Thin (1) on every side, in black (palette 8).
            let dg: u32 = 0x1111 | (8 << 16) | (8 << 23);
            b[10..14].copy_from_slice(&dg.to_le_bytes());
            let top: u32 = 8 | (8 << 7);
            b[14..18].copy_from_slice(&top.to_le_bytes());
        }
        let (fls, fore) = match x.fill {
            Some(c) => (1u32, self.icv(c)),
            None => (0, 0x40),
        };
        let b14 = u32::from_le_bytes([b[14], b[15], b[16], b[17]]) | (fls << 26);
        b[14..18].copy_from_slice(&b14.to_le_bytes());
        let pat: u16 = fore | (0x41 << 7);
        b[18..20].copy_from_slice(&pat.to_le_bytes());
        b
    }
}

// ---- shared strings ---------------------------------------------------------

#[derive(Default)]
struct Sst {
    strings: Vec<Vec<u16>>,
    index: HashMap<String, u32>,
    total: u32,
}

impl Sst {
    fn add(&mut self, s: &str) -> u32 {
        self.total += 1;
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.encode_utf16().collect());
        self.index.insert(s.to_string(), i);
        i
    }

    /// SST with its CONTINUEs, then EXTSST; `base` is where SST starts in
    /// the stream (EXTSST points into it).
    fn write(&self, out: &mut Vec<u8>, base: usize) {
        let n = self.strings.len();
        // Strings per EXTSST bucket: at most 128 buckets.
        let dsst = n.div_ceil(128).max(8);
        let mut inf: Vec<(u32, u16)> = Vec::new();
        let mut recs: Vec<(u16, Vec<u8>)> = Vec::new();
        let mut body = Vec::new();
        body.extend_from_slice(&self.total.to_le_bytes());
        body.extend_from_slice(&(n as u32).to_le_bytes());
        let mut ty = 0x00FCu16;
        // Stream offset of the current record's header.
        let mut at = base;
        for (k, units) in self.strings.iter().enumerate() {
            let high = units.iter().any(|&u| u >= 0x100);
            let width = if high { 2 } else { 1 };
            // A string's header (count, flags) and first character never
            // split from each other.
            if MAX_BODY - body.len() < 3 + width {
                at += 4 + body.len();
                recs.push((ty, std::mem::take(&mut body)));
                ty = CONTINUE;
            }
            if k % dsst == 0 {
                inf.push(((at + 4 + body.len()) as u32, (4 + body.len()) as u16));
            }
            body.extend_from_slice(&(units.len() as u16).to_le_bytes());
            body.push(high as u8);
            let mut left = &units[..];
            loop {
                let room = (MAX_BODY - body.len()) / width;
                let take = room.min(left.len());
                for &u in &left[..take] {
                    if high {
                        body.extend_from_slice(&u.to_le_bytes());
                    } else {
                        body.push(u as u8);
                    }
                }
                left = &left[take..];
                if left.is_empty() {
                    break;
                }
                at += 4 + body.len();
                recs.push((ty, std::mem::take(&mut body)));
                ty = CONTINUE;
                body.push(high as u8);
            }
        }
        recs.push((ty, body));
        for (ty, b) in &recs {
            rec(out, *ty, b);
        }
        let mut b = (dsst as u16).to_le_bytes().to_vec();
        for (ib, cb) in inf {
            b.extend_from_slice(&ib.to_le_bytes());
            b.extend_from_slice(&cb.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        rec(out, 0x00FF, &b);
    }
}

// ---- reference tables -------------------------------------------------------

/// One NAME record.
enum NameRec {
    /// `Workbook::defined_names[i]`.
    Defined(usize),
    /// A hidden function name (`_xlfn.TEXTJOIN`, a UDF).
    Function(String),
    /// A name formulas use that nothing defines.
    Placeholder(String),
}

/// A Library book an external link names: its file and sheet names.
struct Link {
    file: String,
    sheets: Vec<String>,
}

/// The reference tables formulas fill: XTIs, NAMEs, external names.
struct Refs<'a> {
    wb: &'a Workbook,
    xti: Vec<(Book, i16, i16)>,
    names: Vec<NameRec>,
    ext: Vec<(Book, Vec<String>)>,
    /// The package's external links, `[1]` first; `None` for one that is
    /// no Library book.
    links: Vec<Option<Link>>,
}

impl<'a> Refs<'a> {
    fn new(pkg: &'a SheetPackage) -> Refs<'a> {
        Refs {
            wb: &pkg.workbook,
            xti: Vec::new(),
            names: (0..pkg.workbook.defined_names.len())
                .map(NameRec::Defined)
                .collect(),
            ext: Vec::new(),
            links: external_links(pkg),
        }
    }

    /// The books the tables use, in SUPBOOK order: this workbook, the
    /// add-in book, then the Library books.
    fn books(&self) -> Vec<Book> {
        let mut v: Vec<Book> = self.xti.iter().map(|x| x.0).collect();
        v.extend(self.ext.iter().map(|e| e.0));
        if !self.xti.is_empty() {
            v.push(Book::Own);
        }
        let rank = |b: &Book| match b {
            Book::Own => (0, 0),
            Book::AddIn => (1, 0),
            Book::Lib(k) => (2, *k),
        };
        v.sort_by_key(rank);
        v.dedup();
        v
    }
}

impl Tables for Refs<'_> {
    fn workbook(&self) -> &Workbook {
        self.wb
    }

    fn xti(&mut self, book: Book, first: i16, last: i16) -> u16 {
        let e = (book, first, last);
        match self.xti.iter().position(|x| *x == e) {
            Some(i) => i as u16,
            None => {
                self.xti.push(e);
                (self.xti.len() - 1) as u16
            }
        }
    }

    fn name(&mut self, name: &str, sheet: Option<usize>) -> u32 {
        let defined = |scope: Option<usize>| {
            self.names.iter().position(|n| match n {
                NameRec::Defined(i) => {
                    let d = &self.wb.defined_names[*i];
                    d.scope == scope && d.name.eq_ignore_ascii_case(name)
                }
                _ => false,
            })
        };
        if let Some(i) = sheet
            .and_then(|s| defined(Some(s)))
            .or_else(|| defined(None))
        {
            return i as u32 + 1;
        }
        let placeholder = self
            .names
            .iter()
            .position(|n| matches!(n, NameRec::Placeholder(p) if p.eq_ignore_ascii_case(name)));
        match placeholder {
            Some(i) => i as u32 + 1,
            None => {
                self.names.push(NameRec::Placeholder(name.to_string()));
                self.names.len() as u32
            }
        }
    }

    fn function(&mut self, name: &str) -> u32 {
        let found = self
            .names
            .iter()
            .position(|n| matches!(n, NameRec::Function(f) if f.eq_ignore_ascii_case(name)));
        match found {
            Some(i) => i as u32 + 1,
            None => {
                self.names.push(NameRec::Function(name.to_string()));
                self.names.len() as u32
            }
        }
    }

    fn ext_name(&mut self, book: Book, name: &str) -> Option<u32> {
        if let Book::Lib(k) = book {
            self.links.get(k)?.as_ref()?;
        }
        let names = match self.ext.iter().position(|e| e.0 == book) {
            Some(i) => &mut self.ext[i].1,
            None => {
                self.ext.push((book, Vec::new()));
                &mut self.ext.last_mut()?.1
            }
        };
        Some(
            match names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
                Some(i) => i as u32 + 1,
                None => {
                    names.push(name.to_string());
                    names.len() as u32
                }
            },
        )
    }
}

/// The package's external links in `<externalReferences>` order: the
/// Library file each names (`xlLibrary` relationship to a plain file name),
/// with its sheet names.
fn external_links(pkg: &SheetPackage) -> Vec<Option<Link>> {
    use crate::xlsx::{decode, local, parse_rels_mode};
    use opccore::xml::{Event, XmlParser};
    let text = |name: &str| {
        pkg.part(name)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    };
    let Some(wb_xml) = text("xl/workbook.xml") else {
        return Vec::new();
    };
    let rels = text("xl/_rels/workbook.xml.rels")
        .map(|r| parse_rels_mode(&r))
        .unwrap_or_default();
    let mut ids = Vec::new();
    let mut p = XmlParser::new(&wb_xml);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "externalReference" => {
                ids.push(decode(p.attr("r:id")));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ids.iter()
        .map(|id| {
            let (_, _, target, _) = rels.iter().find(|r| &r.0 == id)?;
            let part = format!(
                "xl/{}",
                target.trim_start_matches('/').trim_start_matches("xl/")
            );
            let xml = text(&part)?;
            let (dir, file) = part.rsplit_once('/')?;
            let link_rels = text(&format!("{dir}/_rels/{file}.rels"))
                .map(|r| parse_rels_mode(&r))
                .unwrap_or_default();
            let lib = link_rels
                .iter()
                .find(|r| r.1.ends_with("/xlExternalLinkPath/xlLibrary"))?;
            let plain = !lib.2.is_empty()
                && !lib
                    .2
                    .chars()
                    .any(|c| c < ' ' || matches!(c, '\\' | '/' | ':'));
            if !plain {
                return None;
            }
            let mut sheets = Vec::new();
            let mut p = XmlParser::new(&xml);
            loop {
                match p.next() {
                    Event::Start if local(p.name()) == "sheetName" => {
                        sheets.push(decode(p.attr("val")));
                    }
                    Event::Eof => break,
                    _ => {}
                }
            }
            Some(Link {
                file: lib.2.clone(),
                sheets,
            })
        })
        .collect()
}

/// The BIFF8 code of a built-in name (`_xlnm.Print_Area`), as the reader's
/// table spells them.
fn builtin_code(name: &str) -> Option<u8> {
    let n = name.strip_prefix("_xlnm.")?;
    super::BUILTIN_NAMES
        .iter()
        .position(|c| c.eq_ignore_ascii_case(n))
        .map(|i| i as u8)
}

// ---- globals ----------------------------------------------------------------

/// The globals substream. `ws_pos` gets where each BOUNDSHEET8's
/// `lbPlyPos` is, to patch once the sheets are placed.
fn globals(
    out: &mut Vec<u8>,
    wb: &Workbook,
    styles: &StyleTable,
    refs: &Refs,
    name_rgce: &[Option<Compiled>],
    sst: &Sst,
    ws_pos: &mut Vec<usize>,
) {
    bof(out, 0x0005);
    rec(out, 0x0042, &1200u16.to_le_bytes()); // CODEPAGE: UTF-16
    let active = wb.active_tab.min(wb.sheets.len().saturating_sub(1)) as u16;
    let mut w1 = u16s(&[0x0168, 0x005A, 0x3FCF, 0x2A4E, 0x0038, active, 0, 1, 0x0258]);
    w1.truncate(18);
    rec(out, 0x003D, &w1); // WINDOW1
    rec(out, 0x0022, &(wb.date1904 as u16).to_le_bytes()); // DATEMODE
    rec(out, 0x000E, &1u16.to_le_bytes()); // PRECISION: as displayed off
    styles.write(out);
    for s in &wb.sheets {
        ws_pos.push(out.len() + 4);
        let mut b = 0u32.to_le_bytes().to_vec();
        b.push(s.hidden as u8);
        b.push(0); // a worksheet
        b.extend_from_slice(&ptg_write::short_string(&s.name).unwrap_or_else(|| vec![0, 0]));
        rec(out, 0x0085, &b);
    }
    // SUPBOOKs (each with its EXTERNNAMEs), then EXTERNSHEET.
    let books = refs.books();
    for book in &books {
        match book {
            Book::Own => rec(out, 0x01AE, &u16s(&[wb.sheets.len() as u16, 0x0401])),
            Book::AddIn => rec(out, 0x01AE, &u16s(&[1, 0x3A01])),
            Book::Lib(k) => {
                let Some(Some(link)) = refs.links.get(*k) else {
                    continue;
                };
                let path: Vec<u16> = format!("\u{1}\u{8}{}", link.file).encode_utf16().collect();
                let mut b = u16s(&[link.sheets.len() as u16, path.len() as u16]);
                ptg_write::push_chars(&mut b, &path);
                for s in &link.sheets {
                    b.extend_from_slice(&xl_string(s));
                }
                rec(out, 0x01AE, &b);
            }
        }
        let names = refs
            .ext
            .iter()
            .find(|e| e.0 == *book)
            .map(|e| e.1.as_slice())
            .unwrap_or_default();
        for n in names {
            let mut b = vec![0u8; 6];
            b.extend_from_slice(&ptg_write::short_string(n).unwrap_or_else(|| vec![0, 0]));
            match book {
                // An add-in function: `#REF!` as its definition, as Excel
                // writes one.
                Book::AddIn => b.extend_from_slice(&[2, 0, 0x1C, 0x17]),
                _ => b.extend_from_slice(&[0, 0]),
            }
            rec(out, 0x0023, &b);
        }
    }
    if !refs.xti.is_empty() {
        let mut b = (refs.xti.len() as u16).to_le_bytes().to_vec();
        for &(book, first, last) in &refs.xti {
            let i = books.iter().position(|b| *b == book).unwrap_or(0) as u16;
            b.extend_from_slice(&u16s(&[i, first as u16, last as u16]));
        }
        rec_split(out, 0x0017, &b);
    }
    for n in &refs.names {
        let (name, scope, flags, c) = match n {
            NameRec::Defined(k) => {
                let d = &wb.defined_names[*k];
                (d.name.as_str(), d.scope, 0u16, name_rgce[*k].clone())
            }
            NameRec::Function(f) => (f.as_str(), None, 0x000B, None),
            NameRec::Placeholder(p) => (p.as_str(), None, 0, None),
        };
        let builtin = builtin_code(name);
        let flags = flags | if builtin.is_some() { 0x0020 } else { 0 };
        let flags = flags | if builtin == Some(0x0D) { 0x0001 } else { 0 };
        let units: Vec<u16> = match builtin {
            Some(code) => vec![code as u16],
            None => name.encode_utf16().take(255).collect(),
        };
        let c = match n {
            // A hidden function name's definition is `#NAME?`, as Excel's.
            NameRec::Function(_) => Some(Compiled {
                rgce: vec![0x1C, 0x1D],
                extra: Vec::new(),
            }),
            _ => c,
        }
        .unwrap_or_default();
        let mut b = flags.to_le_bytes().to_vec();
        b.push(0);
        b.push(units.len() as u8);
        b.extend_from_slice(&(c.rgce.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        let itab = scope.map_or(0, |s| s as u16 + 1);
        b.extend_from_slice(&itab.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        ptg_write::push_chars(&mut b, &units);
        b.extend_from_slice(&c.rgce);
        b.extend_from_slice(&c.extra);
        rec_split(out, 0x0018, &b);
    }
    let base = out.len();
    sst.write(out, base);
    rec(out, EOF, &[]);
}

/// A record whose body may be longer than [`MAX_BODY`]: the rest in
/// CONTINUE records.
fn rec_split(out: &mut Vec<u8>, ty: u16, body: &[u8]) {
    let mut chunks = body.chunks(MAX_BODY);
    rec(out, ty, chunks.next().unwrap_or_default());
    for c in chunks {
        rec(out, CONTINUE, c);
    }
}

// ---- sheets -----------------------------------------------------------------

/// A range: (r1, c1, r2, c2).
type Rect = (u32, u32, u32, u32);

struct SheetOut {
    bytes: Vec<u8>,
}

fn on_grid(r: u32, c: u32) -> bool {
    r as i64 <= ptg_write::MAX_ROW && c as i64 <= ptg_write::MAX_COL
}

/// One worksheet substream.
fn sheet_records(
    wb: &Workbook,
    index: usize,
    s: &Sheet,
    styles: &StyleTable,
    refs: &mut Refs,
    sst: &mut Sst,
    lost: &mut BTreeMap<Loss, usize>,
) -> SheetOut {
    let mut out = Vec::new();
    bof(&mut out, 0x0010);
    // The calculation block: automatic, A1, and iterative calculation as
    // the workbook has it (Excel keeps a copy in every sheet).
    let (iterate, count, delta) = match wb.iterate {
        Some((n, d)) => (1u16, n.min(32_767) as u16, d),
        None => (0, 100, 0.001),
    };
    rec(&mut out, 0x000D, &1u16.to_le_bytes()); // CALCMODE
    rec(&mut out, 0x000C, &count.to_le_bytes()); // CALCCOUNT
    rec(&mut out, 0x000F, &1u16.to_le_bytes()); // REFMODE
    rec(&mut out, 0x0011, &iterate.to_le_bytes()); // ITERATION
    rec(&mut out, 0x0010, &delta.to_le_bytes()); // DELTA
    // The grid's cells, and the array formulas whose ranges they're in.
    let mut cells: BTreeMap<(u32, u32), &Cell> = BTreeMap::new();
    for (&(r, c), cell) in &s.cells {
        if on_grid(r, c) {
            cells.insert((r, c), cell);
        } else if cell.value != CellValue::Empty || cell.formula.is_some() {
            *lost.entry(Loss::CellsPastGrid).or_default() += 1;
        }
    }
    // Array anchors: (range, tokens). The range's other cells point at it.
    let mut arrays: BTreeMap<(u32, u32), (Rect, Compiled)> = BTreeMap::new();
    let mut member: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut formulas: HashMap<(u32, u32), Compiled> = HashMap::new();
    for (&(r, c), cell) in &cells {
        let Some(f) = cell.formula.as_deref() else {
            continue;
        };
        // The block an array formula covers, as the .xlsx writer decides
        // it: a spilling formula its spill as it is now (its stored ref may
        // be stale); a legacy CSE block its stored ref, unless other content
        // sits in it now; any other array formula (a dynamic one that
        // doesn't spill, typed or with a stale ref, or a taken block) its
        // anchor alone. A block never covers another cell's own formula,
        // nor a block already written: it shrinks to its anchor. So no cell
        // with its own formula is ever another block's member.
        let spill = cell
            .spill
            .filter(|&ext| ext != (1, 1) && ext.0 > 0 && ext.1 > 0)
            .map(|(h, w)| (r, c, r + h - 1, c + w - 1));
        let legacy = || {
            (!cell.is_dynamic() && !crate::xlsx::block_taken(s, r, c, cell))
                .then(|| crate::sheet::array_block(cell))
                .flatten()
        };
        let alone = (crate::sheet::array_block(cell).is_some() || cell.is_dynamic())
            .then_some((r, c, r, c));
        let fits = |&(r1, c1, r2, c2): &Rect| {
            (r1, c1) == (r, c) && r2 >= r1 && c2 >= c1 && on_grid(r2, c2)
        };
        let free = |&(r1, c1, r2, c2): &Rect| {
            (r1..=r2).all(|rr| {
                (c1..=c2).all(|cc| {
                    let own = (rr, cc) != (r, c)
                        && cells.get(&(rr, cc)).is_some_and(|x| x.formula.is_some());
                    !own && !member.contains_key(&(rr, cc))
                })
            })
        };
        let range = [spill, legacy(), alone]
            .into_iter()
            .flatten()
            .find(|b| fits(b) && free(b));
        let site = Site::Cell {
            sheet: index,
            row: r,
            col: c,
            array: range.is_some(),
        };
        match ptg_write::compile(f, site, refs) {
            Ok(tokens) => match range {
                Some(range) => {
                    for rr in range.0..=range.2 {
                        for cc in range.1..=range.3 {
                            member.insert((rr, cc), (r, c));
                        }
                    }
                    arrays.insert((r, c), (range, tokens));
                }
                None => {
                    formulas.insert((r, c), tokens);
                }
            },
            Err(why) => *lost.entry(Loss::Formula(why)).or_default() += 1,
        }
    }

    // DIMENSIONS: the used rows and columns, one past the last.
    let keys: Vec<(u32, u32)> = cells
        .keys()
        .copied()
        .chain(member.keys().copied())
        .collect();
    let (r0, r1, c0, c1) = if keys.is_empty() {
        (0, 0, 0, 0)
    } else {
        (
            keys.iter().map(|k| k.0).min().unwrap_or(0),
            keys.iter().map(|k| k.0).max().unwrap_or(0) + 1,
            keys.iter().map(|k| k.1).min().unwrap_or(0),
            keys.iter().map(|k| k.1).max().unwrap_or(0) + 1,
        )
    };
    // DEFAULTROWHEIGHT, DEFCOLWIDTH and STANDARDWIDTH, from the sheet's
    // own defaults when it has them.
    let default_miy = s
        .format
        .default_row_height
        .map(|h| (h * 20.0).round().clamp(1.0, 8192.0) as u16);
    if let Some(miy) = default_miy {
        // fUnsynced: a height other than the default font's 15 points.
        rec(&mut out, 0x0225, &u16s(&[u16::from(miy != 300), miy]));
    }
    let base = s.format.base_col_width.clamp(0, 255) as u16;
    rec(&mut out, 0x0055, &base.to_le_bytes());
    if let Some(w) = s.format.default_col_width {
        let w = (w * 256.0).round().clamp(0.0, 65535.0) as u16;
        rec(&mut out, 0x0099, &w.to_le_bytes());
    }
    for d in &s.col_defs {
        if d.min as i64 > ptg_write::MAX_COL {
            continue;
        }
        let last = (d.max as i64).min(ptg_write::MAX_COL) as u16;
        let hidden = s.col_hidden(d.min);
        if d.width.is_none() && !hidden {
            continue;
        }
        let width = d.width.unwrap_or(crate::sheet::DEFAULT_COL_WIDTH);
        let coldx = (width * 256.0).round().clamp(0.0, 65535.0) as u16;
        let flags: u16 =
            if hidden { 0x0001 } else { 0 } | if d.width.is_some() { 0x0002 } else { 0 };
        let b = u16s(&[d.min as u16, last, coldx, STYLE_XFS as u16, flags, 0]);
        rec(&mut out, 0x007D, &b);
    }
    let mut dim = r0.to_le_bytes().to_vec();
    dim.extend_from_slice(&r1.to_le_bytes());
    dim.extend_from_slice(&u16s(&[c0 as u16, c1 as u16, 0]));
    rec(&mut out, 0x0200, &dim);

    // Rows in blocks of 32: their ROW records, then their cells.
    let mut rows: BTreeMap<u32, (u16, u16)> = BTreeMap::new();
    for &(r, c) in &keys {
        let e = rows.entry(r).or_insert((u16::MAX, 0));
        e.0 = e.0.min(c as u16);
        e.1 = e.1.max(c as u16 + 1);
    }
    for &r in s.row_attrs.keys() {
        if r as i64 <= ptg_write::MAX_ROW {
            rows.entry(r).or_insert((0, 0));
        }
    }
    let row_list: Vec<u32> = rows.keys().copied().collect();
    for block in row_list.chunks(32) {
        for &r in block {
            let (lo, hi) = rows[&r];
            let lo = if lo == u16::MAX { 0 } else { lo };
            let height = s.row_height(r);
            let miy = height.map_or(default_miy.unwrap_or(300), |h| {
                (h * 20.0).round().clamp(0.0, 8192.0) as u16
            });
            let mut flags: u16 = 0x0100;
            if s.row_hidden(r) {
                flags |= 0x0020;
            }
            if height.is_some() {
                flags |= 0x0040;
            }
            let b = u16s(&[r as u16, lo, hi, miy, 0, 0, flags, STYLE_XFS as u16]);
            rec(&mut out, 0x0208, &b);
        }
        let (first, last) = (block[0], block[block.len() - 1]);
        let in_block = |&&(r, _): &&(u32, u32)| r >= first && r <= last;
        let mut at: Vec<(u32, u32)> = keys.iter().filter(in_block).copied().collect();
        at.sort_unstable();
        at.dedup();
        for (r, c) in at {
            let cell = cells.get(&(r, c)).copied();
            let ixfe = styles.xf(cell.map_or(0, |x| x.style));
            let head = u16s(&[r as u16, c as u16, ixfe]);
            if let Some(&anchor) = member.get(&(r, c)) {
                let value = cell.map_or(&CellValue::Empty, |x| &x.value);
                let mut rgce = vec![0x01];
                rgce.extend_from_slice(&u16s(&[anchor.0 as u16, anchor.1 as u16]));
                // The anchor's ARRAY goes right after its FORMULA, before
                // the STRING of a text result.
                let array = (anchor == (r, c)).then(|| {
                    let (range, tokens) = &arrays[&anchor];
                    let mut b = u16s(&[range.0 as u16, range.2 as u16]);
                    b.push(range.1 as u8);
                    b.push(range.3 as u8);
                    b.extend_from_slice(&0u16.to_le_bytes());
                    b.extend_from_slice(&0u32.to_le_bytes());
                    b.extend_from_slice(&(tokens.rgce.len() as u16).to_le_bytes());
                    b.extend_from_slice(&tokens.rgce);
                    b.extend_from_slice(&tokens.extra);
                    b
                });
                formula_rec(&mut out, &head, value, &rgce, &[], false, array.as_deref());
                continue;
            }
            let Some(cell) = cell else {
                continue;
            };
            if let Some(tokens) = formulas.get(&(r, c)) {
                let volatile = tokens.rgce.starts_with(&[0x19, 0x01]);
                formula_rec(
                    &mut out,
                    &head,
                    &cell.value,
                    &tokens.rgce,
                    &tokens.extra,
                    volatile,
                    None,
                );
                continue;
            }
            match &cell.value {
                CellValue::Number(n) => match rk(*n) {
                    Some(v) => {
                        let mut b = head.clone();
                        b.extend_from_slice(&v.to_le_bytes());
                        rec(&mut out, 0x027E, &b);
                    }
                    None => {
                        let mut b = head.clone();
                        b.extend_from_slice(&n.to_le_bytes());
                        rec(&mut out, 0x0203, &b);
                    }
                },
                CellValue::Text(t) => {
                    let text = if t.encode_utf16().count() > MAX_TEXT {
                        *lost.entry(Loss::LongText).or_default() += 1;
                        let units: Vec<u16> = t.encode_utf16().take(MAX_TEXT).collect();
                        String::from_utf16_lossy(&units)
                    } else {
                        t.clone()
                    };
                    let mut b = head.clone();
                    b.extend_from_slice(&sst.add(&text).to_le_bytes());
                    rec(&mut out, 0x00FD, &b);
                }
                CellValue::Bool(v) => {
                    let mut b = head.clone();
                    b.extend_from_slice(&[*v as u8, 0]);
                    rec(&mut out, 0x0205, &b);
                }
                CellValue::Error(e) => {
                    let mut b = head.clone();
                    b.extend_from_slice(&[ptg_write::error_code(e).unwrap_or(0x2A), 1]);
                    rec(&mut out, 0x0205, &b);
                }
                CellValue::Empty => {
                    if cell.style != 0 {
                        rec(&mut out, 0x0201, &head);
                    }
                }
            }
        }
    }

    // WINDOW2: gridlines, headers, zeros, default header colour, outline
    // symbols; the active sheet selected and shown.
    let active = wb.active_tab.min(wb.sheets.len().saturating_sub(1)) == index;
    let frozen = s.freeze != (0, 0)
        && s.freeze.0 as i64 <= ptg_write::MAX_ROW
        && s.freeze.1 as i64 <= ptg_write::MAX_COL;
    let mut grbit: u16 = 0x00B6;
    if frozen {
        grbit |= 0x0108;
    }
    if active {
        grbit |= 0x0600;
    }
    let mut w2 = u16s(&[grbit, 0, 0, 0x40, 0, 0, 0]);
    w2.extend_from_slice(&[0, 0, 0, 0]);
    rec(&mut out, 0x023E, &w2);
    if frozen {
        let (rows, cols) = (s.freeze.0 as u16, s.freeze.1 as u16);
        let pane = match (rows > 0, cols > 0) {
            (true, true) => 0,
            (true, false) => 2,
            _ => 1,
        };
        let mut b = u16s(&[cols, rows, rows, cols]);
        b.extend_from_slice(&[pane, 0]);
        rec(&mut out, 0x0041, &b);
    }
    let mut merges: Vec<[u16; 4]> = Vec::new();
    for &(r1, c1, r2, c2) in &s.merges {
        if on_grid(r2, c2) && on_grid(r1, c1) {
            merges.push([r1 as u16, r2 as u16, c1 as u16, c2 as u16]);
        } else {
            *lost.entry(Loss::MergesPastGrid).or_default() += 1;
        }
    }
    for chunk in merges.chunks(1026) {
        let mut b = (chunk.len() as u16).to_le_bytes().to_vec();
        for m in chunk {
            b.extend_from_slice(&u16s(m));
        }
        rec(&mut out, 0x00E5, &b);
    }
    rec(&mut out, EOF, &[]);
    SheetOut { bytes: out }
}

/// A FORMULA record (and a STRING after it for a text result): `head` is
/// row, column and XF.
#[allow(clippy::too_many_arguments)]
fn formula_rec(
    out: &mut Vec<u8>,
    head: &[u8],
    value: &CellValue,
    rgce: &[u8],
    extra: &[u8],
    always: bool,
    array: Option<&[u8]>,
) {
    let mut b = head.to_vec();
    let mut text: Option<&str> = None;
    match value {
        CellValue::Number(n) => b.extend_from_slice(&n.to_le_bytes()),
        CellValue::Text(t) if !t.is_empty() => {
            b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
            text = Some(t);
        }
        CellValue::Bool(v) => b.extend_from_slice(&[1, 0, *v as u8, 0, 0, 0, 0xFF, 0xFF]),
        CellValue::Error(e) => {
            let code = ptg_write::error_code(e).unwrap_or(0x2A);
            b.extend_from_slice(&[2, 0, code, 0, 0, 0, 0xFF, 0xFF]);
        }
        // Empty text, or no value yet: the empty string.
        _ => b.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0xFF, 0xFF]),
    }
    b.extend_from_slice(&(if always { 0x0001u16 } else { 0 }).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&(rgce.len() as u16).to_le_bytes());
    b.extend_from_slice(rgce);
    b.extend_from_slice(extra);
    rec_split(out, 0x0006, &b);
    if let Some(a) = array {
        rec(out, 0x0221, a);
    }
    if let Some(t) = text {
        let units: Vec<u16> = t.encode_utf16().take(MAX_TEXT).collect();
        let mut head = (units.len() as u16).to_le_bytes().to_vec();
        head.push(units.iter().any(|&u| u >= 0x100) as u8);
        rec_with_string(out, 0x0207, &head, &units);
    }
}

/// `x` as an RK number, when one holds it exactly.
fn rk(x: f64) -> Option<u32> {
    let try_int = |v: f64, flag: u32| -> Option<u32> {
        if v.fract() != 0.0 || !(-(1i64 << 29) as f64..(1i64 << 29) as f64).contains(&v) {
            return None;
        }
        let r = ((v as i32) << 2) as u32 | 2 | flag;
        (super::rk(r) == x).then_some(r)
    };
    let try_float = |v: f64, flag: u32| -> Option<u32> {
        let bits = v.to_bits();
        if bits & 0x3_FFFF_FFFF != 0 {
            return None;
        }
        let r = (bits >> 32) as u32 | flag;
        (super::rk(r) == x).then_some(r)
    };
    if x == 0.0 && x.is_sign_negative() {
        return None;
    }
    try_int(x, 0)
        .or_else(|| try_float(x, 0))
        .or_else(|| try_int((x * 100.0).round(), 1))
        .or_else(|| try_float(x * 100.0, 1))
}

// ---- the structural check ---------------------------------------------------

/// Check that `file` (an `.xls`) keeps the rules Excel's reader holds a
/// BIFF8 stream to, which a lenient reader would let pass: the style and
/// cell XFs, the FONT table without index 4, BOUNDSHEET offsets at their
/// sheets' BOFs, record lengths, shared-string indices and counts, one
/// selected sheet, DIMENSIONS around the cells, balanced BOF/EOF. The first
/// rule broken is the error.
#[doc(hidden)]
pub fn check_xls(file: &[u8]) -> Result<(), String> {
    let cfb = opccore::cfb::Cfb::open(file)?;
    let entry = cfb
        .entries()
        .into_iter()
        .find(|e| e.name == "Workbook")
        .ok_or("no Workbook stream")?;
    if entry.size() < 4096 {
        return Err(format!(
            "Workbook stream is {} bytes, under 4096",
            entry.size()
        ));
    }
    let s = cfb
        .read_stream("Workbook")
        .ok_or("unreadable Workbook stream")?;
    // (offset, type, body)
    let mut recs: Vec<(usize, u16, &[u8])> = Vec::new();
    let mut at = 0;
    while at + 4 <= s.len() {
        let ty = u16::from_le_bytes([s[at], s[at + 1]]);
        let len = u16::from_le_bytes([s[at + 2], s[at + 3]]) as usize;
        if ty == 0 && len == 0 {
            // Padding after the last EOF.
            if s[at..].iter().any(|&b| b != 0) {
                return Err(format!("{at}: data after padding"));
            }
            break;
        }
        if len > MAX_BODY {
            return Err(format!(
                "{at}: record {ty:#06x} is {len} bytes, over {MAX_BODY}"
            ));
        }
        let body = s
            .get(at + 4..at + 4 + len)
            .ok_or(format!("{at}: record runs past the stream"))?;
        recs.push((at, ty, body));
        at += 4 + len;
    }
    let u16_at = |b: &[u8], i: usize| -> Result<u16, String> {
        b.get(i..i + 2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .ok_or_else(|| "short record".to_string())
    };
    let u32_at = |b: &[u8], i: usize| -> Result<u32, String> {
        b.get(i..i + 4)
            .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .ok_or_else(|| "short record".to_string())
    };
    // BOF/EOF balance, and the substreams.
    let mut depth = 0i32;
    let mut subs: Vec<(usize, usize)> = Vec::new();
    let mut open = 0;
    for (i, &(_, ty, _)) in recs.iter().enumerate() {
        if ty == BOF {
            if depth == 0 {
                open = i;
            }
            depth += 1;
        } else if ty == EOF {
            depth -= 1;
            if depth < 0 {
                return Err("EOF without BOF".into());
            }
            if depth == 0 {
                subs.push((open, i));
            }
        }
    }
    if depth != 0 || subs.is_empty() {
        return Err("BOF/EOF unbalanced".into());
    }
    // A STRING (a formula's text result) follows its FORMULA, directly or
    // after the FORMULA's ARRAY, SHRFMLA or TABLE (and CONTINUEs).
    for (i, &(at, ty, _)) in recs.iter().enumerate() {
        if ty != 0x0207 {
            continue;
        }
        let before = recs[..i]
            .iter()
            .rev()
            .map(|r| r.1)
            .find(|&t| !matches!(t, CONTINUE | 0x0221 | 0x04BC | 0x0236));
        if before != Some(0x0006) {
            return Err(format!(
                "{at}: STRING after {before:#06x?}, not its FORMULA"
            ));
        }
    }
    // An ARRAY directly follows its anchor's FORMULA.
    for (i, &(at, ty, _)) in recs.iter().enumerate() {
        if ty == 0x0221
            && recs[..i].iter().rev().map(|r| r.1).find(|&t| t != CONTINUE) != Some(0x0006)
        {
            return Err(format!("{at}: ARRAY not after its FORMULA"));
        }
    }
    // CONTINUE only after a record that may continue.
    for w in recs.windows(2) {
        if w[1].1 == CONTINUE
            && !matches!(
                w[0].1,
                0x00FC | CONTINUE | 0x0006 | 0x0207 | 0x0017 | 0x0018 | 0x01AE
            )
        {
            return Err(format!("{}: CONTINUE after {:#06x}", w[1].0, w[0].1));
        }
    }
    let (g0, g1) = subs[0];
    let globals = &recs[g0..=g1];
    let of = |ty: u16| globals.iter().filter(move |r| r.1 == ty);
    // Fonts: at least 5 (0-3, then 5 on); every XF names one.
    let fonts = of(0x0031).count();
    let xfs: Vec<&[u8]> = of(0x00E0).map(|r| r.2).collect();
    if xfs.len() < STYLE_XFS + 1 {
        return Err(format!(
            "{} XFs: 15 style XFs and a cell XF needed",
            xfs.len()
        ));
    }
    if xfs.len() > MAX_XFS {
        return Err(format!("{} XFs, over {MAX_XFS}", xfs.len()));
    }
    for (i, x) in xfs.iter().enumerate() {
        if x.len() != 20 {
            return Err(format!("XF {i} is {} bytes", x.len()));
        }
        let style = u16_at(x, 4)? & 0x0004 != 0;
        if style != (i < STYLE_XFS) {
            return Err(format!(
                "XF {i}: a {} XF",
                if style { "style" } else { "cell" }
            ));
        }
        let ifnt = u16_at(x, 0)? as usize;
        if ifnt == 4 || ifnt >= fonts + usize::from(fonts >= 4) {
            return Err(format!("XF {i} names font {ifnt} of {fonts}"));
        }
    }
    // BOUNDSHEET offsets: each a worksheet BOF.
    let sheets: Vec<&[u8]> = of(0x0085).map(|r| r.2).collect();
    if sheets.len() + 1 != subs.len() {
        return Err(format!(
            "{} BOUNDSHEETs for {} sheet substreams",
            sheets.len(),
            subs.len() - 1
        ));
    }
    for (k, b) in sheets.iter().enumerate() {
        let pos = u32_at(b, 0)? as usize;
        let (start, _) = subs[k + 1];
        if recs[start].0 != pos {
            return Err(format!(
                "BOUNDSHEET {k} points at {pos}, its BOF is at {}",
                recs[start].0
            ));
        }
        if u16_at(recs[start].2, 2)? != 0x0010 {
            return Err(format!("sheet {k} is no worksheet BOF"));
        }
    }
    // SST counts; each LABELSST index in range.
    let sst_at = globals.iter().position(|r| r.1 == 0x00FC);
    let (total, unique) = match sst_at {
        Some(i) => (u32_at(globals[i].2, 0)?, u32_at(globals[i].2, 4)?),
        None => (0, 0),
    };
    let mut labels = 0u32;
    // Exactly one selected sheet, the WINDOW1 itabCur one.
    let itab = of(0x003D)
        .next()
        .map(|r| u16_at(r.2, 10))
        .transpose()?
        .unwrap_or(0);
    let mut selected = Vec::new();
    for (k, &(start, end)) in subs.iter().skip(1).enumerate() {
        let body = &recs[start..=end];
        let mut cells: Vec<(u32, u32)> = Vec::new();
        // (r1, r2, c1, c2) of each ARRAY.
        let mut arrays: Vec<(u16, u16, u8, u8)> = Vec::new();
        let mut dims = None;
        let mut nested = 0;
        for &(_, ty, b) in body.iter().skip(1) {
            match ty {
                BOF => nested += 1,
                EOF if nested > 0 => nested -= 1,
                _ if nested > 0 => {}
                0x0200 => {
                    dims = Some((u32_at(b, 0)?, u32_at(b, 4)?, u16_at(b, 8)?, u16_at(b, 10)?))
                }
                0x00FD => {
                    labels += 1;
                    let i = u32_at(b, 6)?;
                    if i >= unique {
                        return Err(format!("sheet {k}: LABELSST {i} of {unique} strings"));
                    }
                    cells.push((u16_at(b, 0)? as u32, u16_at(b, 2)? as u32));
                }
                0x0203 | 0x027E | 0x0205 | 0x0201 | 0x0006 => {
                    let ixfe = u16_at(b, 4)? as usize;
                    if ixfe < STYLE_XFS || ixfe >= xfs.len() {
                        return Err(format!("sheet {k}: a cell on XF {ixfe}"));
                    }
                    cells.push((u16_at(b, 0)? as u32, u16_at(b, 2)? as u32));
                }
                0x023E if u16_at(b, 0)? & 0x0200 != 0 => selected.push(k),
                0x0221 if b.len() >= 6 => {
                    arrays.push((u16_at(b, 0)?, u16_at(b, 2)?, b[4], b[5]));
                }
                _ => {}
            }
        }
        // ARRAY blocks never overlap.
        for (i, x) in arrays.iter().enumerate() {
            for y in &arrays[..i] {
                if x.0 <= y.1 && y.0 <= x.1 && x.2 <= y.3 && y.2 <= x.3 {
                    return Err(format!("sheet {k}: ARRAY blocks {y:?} and {x:?} overlap"));
                }
            }
        }
        let (r0, r1, c0, c1) = dims.ok_or(format!("sheet {k}: no DIMENSIONS"))?;
        for (r, c) in cells {
            if r < r0 || r >= r1 || c < c0 as u32 || c >= c1 as u32 {
                return Err(format!("sheet {k}: cell ({r},{c}) outside DIMENSIONS"));
            }
        }
    }
    if labels != total {
        return Err(format!(
            "SST says {total} strings in cells, the cells have {labels}"
        ));
    }
    if selected != [itab as usize] {
        return Err(format!("selected sheets {selected:?}, WINDOW1 says {itab}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{SourceFormat, open_workbook};
    use crate::sheet::Cell;

    fn book(cells: &[(u32, u32, Cell)]) -> SheetPackage {
        let mut pkg = crate::xlsx::new_xlsx_sheets(&["Sheet1".into(), "Data".into()]);
        for (r, c, cell) in cells {
            pkg.workbook.sheets[0].set_cell(*r, *c, cell.clone());
        }
        pkg
    }

    fn formula(f: &str, v: CellValue) -> Cell {
        Cell {
            value: v,
            formula: Some(f.into()),
            ..Cell::default()
        }
    }

    /// Write, check the structure, read back.
    fn round(pkg: &SheetPackage) -> (SheetPackage, Vec<String>) {
        let out = write_xls(pkg);
        check_xls(&out.bytes).unwrap();
        let (back, format) = open_workbook(&out.bytes).unwrap();
        assert_eq!(format, SourceFormat::Xls);
        (back, out.lost)
    }

    #[test]
    fn values_formulas_and_names_round_trip() {
        let mut pkg = book(&[
            (0, 0, Cell::number(1.5)),
            (0, 1, Cell::number(1e100)),
            (0, 2, Cell::number(-7.0)),
            (0, 3, Cell::number(0.01)),
            (1, 0, Cell::text("héllo")),
            (1, 1, Cell::text("日本")),
            (
                1,
                2,
                Cell {
                    value: CellValue::Bool(true),
                    ..Cell::default()
                },
            ),
            (
                1,
                3,
                Cell {
                    value: CellValue::Error("#DIV/0!".into()),
                    ..Cell::default()
                },
            ),
            (2, 0, formula("SUM(A1:D1)", CellValue::Number(-5.49))),
            (2, 1, formula("A2&\"!\"", CellValue::Text("héllo!".into()))),
            (
                2,
                2,
                formula(
                    "_xlfn.TEXTJOIN(\",\",TRUE,A2:B2)",
                    CellValue::Text("x".into()),
                ),
            ),
            (2, 3, formula("Data!A1*Rate", CellValue::Number(0.0))),
            (3, 0, formula("1=1", CellValue::Bool(true))),
            (3, 1, formula("1/0", CellValue::Error("#DIV/0!".into()))),
            (3, 2, formula("\"\"", CellValue::Text(String::new()))),
        ]);
        pkg.workbook.defined_names.push(crate::sheet::DefinedName {
            name: "Rate".into(),
            scope: None,
            formula: "Data!$A$1".into(),
        });
        pkg.workbook.date1904 = true;
        pkg.workbook.sheets[1].hidden = true;
        let (back, lost) = round(&pkg);
        assert!(lost.is_empty(), "{lost:?}");
        let wb = &back.workbook;
        assert_eq!(wb.sheets.len(), 2);
        assert!(wb.date1904);
        assert_eq!(wb.defined_names.len(), 1);
        assert_eq!(wb.defined_names[0].formula, "Data!$A$1");
        for (&k, cell) in &pkg.workbook.sheets[0].cells {
            let got = wb.sheets[0]
                .cells
                .get(&k)
                .unwrap_or_else(|| panic!("{k:?}"));
            assert_eq!(got.value, cell.value, "{k:?}");
            assert_eq!(got.formula, cell.formula, "{k:?}");
        }
    }

    #[test]
    fn uncompilable_formulas_and_cells_past_the_grid_are_counted() {
        let mut pkg = book(&[
            (0, 0, formula("A1048576+1", CellValue::Number(3.0))),
            (0, 1, formula("A1048576+2", CellValue::Number(4.0))),
            (
                0,
                2,
                formula("_xlfn.LAMBDA(_xlpm.x,_xlpm.x)(1)", CellValue::Number(1.0)),
            ),
            (70_000, 0, Cell::number(1.0)),
            (0, 300, Cell::number(1.0)),
        ]);
        pkg.workbook.sheets[0].merges.push((0, 0, 70_000, 0));
        let (back, lost) = round(&pkg);
        assert_eq!(
            lost,
            [
                "2 formulas with references past row 65,536 or column IV (saved as values)",
                "1 formula with LAMBDA or LET (saved as values)",
                "2 cells beyond row 65,536 or column IV",
                "1 merged range beyond row 65,536 or column IV",
            ]
        );
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(0, 0)].value, CellValue::Number(3.0));
        assert_eq!(cells[&(0, 0)].formula, None);
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn model_features_without_records_are_named() {
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        assert!(write_xls(&pkg).lost.is_empty());
        pkg.workbook.sheets[0]
            .hyperlinks
            .insert((0, 0), "https://example.com".into());
        pkg.workbook.sheets[0].set_row_outline(1, 1);
        assert_eq!(
            write_xls(&pkg).lost,
            ["hyperlinks", "outlines (grouped rows and columns)"]
        );
        // Excel's own page setup isn't a loss (an .xlsx's says portrait and
        // A4), a landscape one is.
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        let ps = &mut pkg.workbook.sheets[0].page_setup;
        ps.orientation = crate::print::setup::Orientation::Portrait;
        ps.paper_size = 9;
        assert!(write_xls(&pkg).lost.is_empty());
        pkg.workbook.sheets[0].page_setup.orientation = crate::print::setup::Orientation::Landscape;
        assert_eq!(write_xls(&pkg).lost, ["page setup"]);
        // Document properties go in streams this doesn't write.
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        let mut p = pkg.doc_properties();
        p.title = Some("Budget".into());
        pkg.set_doc_properties(&p).unwrap();
        assert_eq!(write_xls(&pkg).lost, ["document properties"]);
    }

    #[test]
    fn long_shared_strings_split_across_continue() {
        // Strings past one record, compressed and UTF-16, crossing record
        // boundaries mid-string and between strings.
        let mut cells = Vec::new();
        for i in 0..40u32 {
            let s = if i % 3 == 0 {
                format!("{i}:{}", "ü".repeat(700))
            } else {
                format!("{i}:{}", "€x".repeat(500 + i as usize))
            };
            cells.push((i, 0, Cell::text(&s)));
        }
        cells.push((40, 0, Cell::text(&"y".repeat(32_000))));
        let pkg = book(&cells);
        let (back, lost) = round(&pkg);
        assert!(lost.is_empty());
        for (&k, cell) in &pkg.workbook.sheets[0].cells {
            assert_eq!(back.workbook.sheets[0].cells[&k].value, cell.value, "{k:?}");
        }
        // A long text result goes in STRING with CONTINUE.
        let long = "€".repeat(9000);
        let pkg = book(&[(
            0,
            0,
            formula("REPT(\"€\",9000)", CellValue::Text(long.clone())),
        )]);
        let (back, _) = round(&pkg);
        assert_eq!(
            back.workbook.sheets[0].cells[&(0, 0)].value,
            CellValue::Text(long)
        );
    }

    #[test]
    fn array_formulas_keep_their_range() {
        let mut anchor = formula("A1:A2*2", CellValue::Number(2.0));
        anchor.f_attrs = Some(" t=\"array\" ref=\"B1:B2\"".into());
        // As the import and the engine leave an evaluated CSE block.
        anchor.spill = Some((2, 1));
        let pkg = book(&[
            (0, 0, Cell::number(1.0)),
            (1, 0, Cell::number(2.0)),
            (0, 1, anchor),
            (1, 1, Cell::number(4.0)),
        ]);
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(0, 1)].formula.as_deref(), Some("A1:A2*2"));
        assert_eq!(cells[&(0, 1)].spill, Some((2, 1)));
        assert_eq!(cells[&(1, 1)].value, CellValue::Number(4.0));
        assert_eq!(cells[&(1, 1)].formula, None);
    }

    #[test]
    fn rk_holds_what_it_can_exactly() {
        for x in [0.0, 1.0, -1.0, 123.45, 0.5, 1e10, 536_870_911.0, 2.71] {
            if let Some(r) = rk(x) {
                assert_eq!(super::super::rk(r), x);
            }
        }
        assert!(rk(1.0).is_some() && rk(123.45).is_some() && rk(0.5).is_some());
        assert_eq!(rk(1.0 / 3.0), None);
    }

    #[test]
    fn the_check_catches_what_excel_refuses() {
        let pkg = book(&[(0, 0, Cell::text("a"))]);
        let good = write_xls(&pkg).bytes;
        check_xls(&good).unwrap();
        let cfb = opccore::cfb::Cfb::open(&good).unwrap();
        let stream = cfb.read_stream("Workbook").unwrap();
        let wrap = |s: Vec<u8>| opccore::cfb::write_cfb(&[("Workbook", s)]);
        // A BOUNDSHEET pointing one byte off.
        let mut s = stream.clone();
        let bs = find(&s, 0x0085);
        s[bs + 4] = s[bs + 4].wrapping_add(1);
        assert!(check_xls(&wrap(s)).unwrap_err().contains("BOUNDSHEET"));
        // An SST that miscounts.
        let mut s = stream.clone();
        let sst = find(&s, 0x00FC);
        s[sst + 4] = 7;
        assert!(check_xls(&wrap(s)).unwrap_err().contains("SST"));
        // A short stream (the mini stream).
        let mut s = stream.clone();
        let end = s.iter().rposition(|&b| b != 0).unwrap() + 1;
        s.truncate(end);
        assert!(check_xls(&wrap(s)).unwrap_err().contains("4096"));
    }

    /// A workbook with one number cell styled `xf`, written and read back:
    /// the cell's format as read.
    fn format_round_trip(xf: Xf) -> Xf {
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        let style = pkg.workbook.styles.intern(xf);
        pkg.workbook.sheets[0].cells.get_mut(&(0, 0)).unwrap().style = style;
        let (back, lost) = round(&pkg);
        assert!(lost.is_empty(), "{lost:?}");
        let wb = &back.workbook;
        wb.styles.xf(wb.sheets[0].cells[&(0, 0)].style)
    }

    #[test]
    fn fonts_round_trip() {
        let mut xf = Xf::default();
        xf.bold = true;
        xf.italic = true;
        xf.font_size = Some(14.0);
        xf.font_name = Some("Arial".into());
        xf.color = Some((0xFF, 0, 0));
        let got = format_round_trip(xf.clone());
        assert_eq!(
            (
                got.bold,
                got.italic,
                got.font_size,
                got.font_name,
                got.color
            ),
            (
                true,
                true,
                Some(14.0),
                Some("Arial".into()),
                Some((0xFF, 0, 0))
            )
        );
        // The default font reads back as the default.
        assert_eq!(format_round_trip(Xf::default()), Xf::default());
    }

    #[test]
    fn fills_and_a_custom_palette_round_trip() {
        let mut xf = Xf::default();
        xf.fill = Some((0xFF, 0xFF, 0x00));
        assert_eq!(format_round_trip(xf.clone()).fill, Some((0xFF, 0xFF, 0x00)));
        // A colour the default palette lacks takes a slot of a PALETTE.
        xf.fill = Some((0x12, 0x34, 0x56));
        xf.color = Some((0xAB, 0xCD, 0xEF));
        let got = format_round_trip(xf);
        assert_eq!(
            (got.fill, got.color),
            (Some((0x12, 0x34, 0x56)), Some((0xAB, 0xCD, 0xEF)))
        );
    }

    #[test]
    fn alignment_wrap_border_quote_prefix_and_number_format_round_trip() {
        for align in [Align::Left, Align::Center, Align::Right, Align::General] {
            let mut xf = Xf::default();
            xf.align = align;
            xf.wrap = true;
            xf.border = true;
            xf.quote_prefix = true;
            xf.set_code(Some("0.000%".into()));
            let got = format_round_trip(xf);
            assert_eq!(
                (
                    got.align,
                    got.wrap,
                    got.border,
                    got.quote_prefix,
                    got.code.as_deref()
                ),
                (align, true, true, true, Some("0.000%"))
            );
        }
        // A built-in format is written as its id.
        let mut xf = Xf::default();
        xf.set_code(Some("0.00%".into()));
        assert_eq!(format_round_trip(xf).code.as_deref(), Some("0.00%"));
    }

    #[test]
    fn merges_widths_heights_hidden_and_panes_round_trip() {
        let mut pkg = book(&[(0, 0, Cell::text("a")), (5, 5, Cell::number(2.0))]);
        {
            let s = &mut pkg.workbook.sheets[0];
            s.merges.push((0, 0, 1, 2));
            s.set_col_width(3, 20.5);
            s.set_col_hidden(4, true);
            s.set_row_height(2, Some(30.0));
            s.set_row_hidden(3, true);
            s.freeze = (1, 2);
        }
        pkg.workbook.sheets[1].hidden = true;
        pkg.workbook.active_tab = 0;
        let (back, _) = round(&pkg);
        let s = &back.workbook.sheets[0];
        assert_eq!(s.merges, [(0, 0, 1, 2)]);
        assert_eq!(s.col_width(3), 20.5);
        assert!(s.col_hidden(4) && !s.col_hidden(3));
        assert_eq!(s.row_height(2), Some(30.0));
        assert!(s.row_hidden(3) && !s.row_hidden(2));
        assert_eq!(s.freeze, (1, 2));
        assert!(back.workbook.sheets[1].hidden);
        // Only rows frozen, and the second sheet active.
        pkg.workbook.sheets[0].freeze = (3, 0);
        pkg.workbook.sheets[1].hidden = false;
        pkg.workbook.active_tab = 1;
        let (back, _) = round(&pkg);
        assert_eq!(back.workbook.sheets[0].freeze, (3, 0));
        assert_eq!(back.workbook.active_tab, 1);
    }

    /// A defined name whose record can't hold its name, tokens and array
    /// constant is left out and counted, not split.
    #[test]
    fn a_name_too_long_for_its_record_is_counted() {
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        // ~7,800 bytes of array constant: fits a formula, not with a
        // 255-character UTF-16 name (510 more bytes) beside it.
        let row = vec!["\"xxxxxxxxxxxxxxxxxxxxxx\""; 30].join(",");
        let big = format!("{{{}}}", vec![row; 10].join(";"));
        pkg.workbook.defined_names.push(crate::sheet::DefinedName {
            name: format!("N\u{e9}{}", "x".repeat(253)),
            scope: None,
            formula: big,
        });
        let (back, lost) = round(&pkg);
        assert_eq!(lost, ["1 defined name Excel 97-2003 cannot hold"]);
        assert!(back.workbook.defined_names.is_empty());
    }

    /// A spilling formula is written over its spill as it is now, whatever
    /// its stored `ref` says: larger or smaller, the range reads back as the
    /// spill, and a cell past it keeps its own value.
    #[test]
    fn dynamic_arrays_are_written_over_their_spill() {
        for stale in [None, Some("A1:A5"), Some("A1")] {
            let mut anchor = formula("_xlfn.SEQUENCE(3)", CellValue::Number(1.0));
            anchor.spill = Some((3, 1));
            anchor.f_attrs = stale.map(|r| format!(" t=\"array\" ref=\"{r}\""));
            let pkg = book(&[
                (0, 0, anchor),
                (1, 0, Cell::number(2.0)),
                (2, 0, Cell::number(3.0)),
                (4, 0, Cell::number(9.0)),
            ]);
            let (back, lost) = round(&pkg);
            assert!(lost.is_empty(), "{lost:?}");
            let cells = &back.workbook.sheets[0].cells;
            assert_eq!(
                cells[&(0, 0)].formula.as_deref(),
                Some("_xlfn.SEQUENCE(3)"),
                "{stale:?}"
            );
            assert_eq!(cells[&(0, 0)].spill, Some((3, 1)), "{stale:?}");
            assert_eq!(cells[&(2, 0)].value, CellValue::Number(3.0));
            assert_eq!(cells[&(4, 0)].value, CellValue::Number(9.0));
            assert_eq!(cells[&(4, 0)].formula, None);
        }
    }

    /// An array's block is never written over content that isn't its own,
    /// as the .xlsx writer decides it: a frozen CSE block with a formula
    /// typed into it, a dynamic array that no longer spills over its stale
    /// ref, and a stale ref overlapping another anchor's spill.
    #[test]
    fn array_blocks_never_take_other_content() {
        // (a) {=A1:A3*2} in B1:B3, frozen (no spill), `=5` typed into B2.
        let mut cse = formula("A1:A3*2", CellValue::Number(2.0));
        cse.f_attrs = Some(" t=\"array\" ref=\"B1:B3\"".into());
        let pkg = book(&[
            (0, 1, cse),
            (1, 1, formula("5", CellValue::Number(5.0))),
            (2, 1, Cell::number(6.0)),
        ]);
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(0, 1)].spill, Some((1, 1)));
        assert_eq!(cells[&(1, 1)].formula.as_deref(), Some("5"));
        assert_eq!(cells[&(2, 1)].value, CellValue::Number(6.0));

        // (b) A dynamic SEQUENCE whose stale ref is A1:A3, not spilling, and
        // `x` in A2.
        let mut seq = formula("_xlfn.SEQUENCE(3)", CellValue::Error("#SPILL!".into()));
        seq.f_attrs = Some(" t=\"array\" ref=\"A1:A3\"".into());
        seq.meta.get_or_insert_default().dynamic = true;
        let pkg = book(&[(0, 0, seq), (1, 0, Cell::text("x"))]);
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(1, 0)].value, CellValue::Text("x".into()));
        assert_eq!(cells[&(0, 0)].spill, Some((1, 1)));

        // (c) A stale ref C1:C4 overlapping B2's spill B2:D2: no overlap.
        let mut a = formula("_xlfn.SEQUENCE(1,3)", CellValue::Number(1.0));
        a.spill = Some((1, 3));
        let mut b = formula("A1:A4", CellValue::Number(1.0));
        b.f_attrs = Some(" t=\"array\" ref=\"C1:C4\"".into());
        let pkg = book(&[
            (1, 1, a),
            (1, 2, Cell::number(2.0)),
            (1, 3, Cell::number(3.0)),
            (0, 2, b),
        ]);
        let out = write_xls(&pkg);
        check_xls(&out.bytes).unwrap();
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(1, 1)].spill, Some((1, 3)));
        assert_eq!(cells[&(0, 2)].spill, Some((1, 1)));
        // The check refuses overlapping ARRAY blocks: B1:B2 and D1:D2,
        // the second moved onto column B.
        let arr = |f: &str, r: &str| {
            let mut c = formula(f, CellValue::Number(1.0));
            c.f_attrs = Some(format!(" t=\"array\" ref=\"{r}\""));
            c.spill = Some((2, 1));
            c
        };
        let pkg = book(&[(0, 1, arr("A1:A2", "B1:B2")), (0, 3, arr("A1:A2", "D1:D2"))]);
        let out = write_xls(&pkg).bytes;
        let cfb = opccore::cfb::Cfb::open(&out).unwrap();
        let mut st = cfb.read_stream("Workbook").unwrap();
        let first = find(&st, 0x0221);
        let len = 4 + u16::from_le_bytes([st[first + 2], st[first + 3]]) as usize;
        let second = first + len + find(&st[first + len..], 0x0221);
        st[second + 8] = 1;
        st[second + 9] = 1;
        let bad = opccore::cfb::write_cfb(&[("Workbook", st)]);
        assert!(check_xls(&bad).unwrap_err().contains("overlap"));
    }

    /// A typed dynamic array that doesn't spill (a 1×1 result) is an array
    /// on its anchor alone, as the .xlsx writer writes it; two spills that
    /// overlap (as an .xls with overlapping ARRAY records reads) keep each
    /// cell's own formula and write no overlapping block.
    #[test]
    fn unspilled_dynamic_arrays_and_overlapping_spills() {
        let mut one = formula("_xlfn.SEQUENCE(1)", CellValue::Number(1.0));
        one.meta.get_or_insert_default().dynamic = true;
        let (back, _) = round(&book(&[(0, 0, one)]));
        assert_eq!(back.workbook.sheets[0].cells[&(0, 0)].spill, Some((1, 1)));

        // B1 spills B1:B3 over B2, which has its own array B2:B3.
        let arr = |f: &str, h: u32| {
            let mut c = formula(f, CellValue::Number(1.0));
            c.f_attrs = Some(format!(" t=\"array\" ref=\"B1:B{h}\""));
            c.spill = Some((h, 1));
            c
        };
        let mut b2 = arr("A1:A2*3", 2);
        b2.f_attrs = Some(" t=\"array\" ref=\"B2:B3\"".into());
        let pkg = book(&[
            (0, 1, arr("A1:A3*2", 3)),
            (1, 1, b2),
            (2, 1, Cell::number(6.0)),
        ]);
        let out = write_xls(&pkg);
        check_xls(&out.bytes).unwrap();
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(0, 1)].spill, Some((1, 1)));
        assert_eq!(cells[&(1, 1)].formula.as_deref(), Some("A1:A2*3"));
        assert_eq!(cells[&(1, 1)].spill, Some((2, 1)));
    }

    /// SORT and FILTER's hidden names are spelled `_xlfn._xlws.`, as Excel
    /// stores them, and read back as SORT and FILTER.
    #[test]
    fn sort_and_filter_names_carry_xlws() {
        let pkg = book(&[
            (
                0,
                1,
                formula("_xlfn._xlws.SORT(A1:A3)", CellValue::Number(1.0)),
            ),
            (
                0,
                2,
                formula("_xlfn._xlws.FILTER(A1:A3,A1:A3>1)", CellValue::Number(2.0)),
            ),
        ]);
        let out = write_xls(&pkg);
        let cfb = opccore::cfb::Cfb::open(&out.bytes).unwrap();
        let s = cfb.read_stream("Workbook").unwrap();
        let text = String::from_utf8_lossy(&s);
        assert!(text.contains("_xlfn._xlws.SORT") && text.contains("_xlfn._xlws.FILTER"));
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(
            cells[&(0, 1)].formula.as_deref(),
            Some("_xlfn._xlws.SORT(A1:A3)")
        );
        assert_eq!(
            cells[&(0, 2)].formula.as_deref(),
            Some("_xlfn._xlws.FILTER(A1:A3,A1:A3>1)")
        );
    }

    /// A text-valued legacy array: FORMULA, then ARRAY, then STRING, and
    /// the check refuses another order.
    #[test]
    fn a_text_array_anchor_writes_array_before_string() {
        let mut anchor = formula("A1:A2&\"!\"", CellValue::Text("a!".into()));
        anchor.f_attrs = Some(" t=\"array\" ref=\"B1:B2\"".into());
        anchor.spill = Some((2, 1));
        let pkg = book(&[
            (0, 0, Cell::text("a")),
            (1, 0, Cell::text("b")),
            (0, 1, anchor),
            (1, 1, Cell::text("b!")),
        ]);
        let (back, _) = round(&pkg);
        let cells = &back.workbook.sheets[0].cells;
        assert_eq!(cells[&(0, 1)].value, CellValue::Text("a!".into()));
        assert_eq!(cells[&(0, 1)].spill, Some((2, 1)));
        // Swap the anchor's ARRAY and STRING: refused.
        let out = write_xls(&pkg).bytes;
        let cfb = opccore::cfb::Cfb::open(&out).unwrap();
        let mut s = cfb.read_stream("Workbook").unwrap();
        let a = find(&s, 0x0221);
        let alen = 4 + u16::from_le_bytes([s[a + 2], s[a + 3]]) as usize;
        let st = a + alen;
        assert_eq!(u16::from_le_bytes([s[st], s[st + 1]]), 0x0207);
        let slen = 4 + u16::from_le_bytes([s[st + 2], s[st + 3]]) as usize;
        let array: Vec<u8> = s[a..st].to_vec();
        let string: Vec<u8> = s[st..st + slen].to_vec();
        s.splice(a..st + slen, string.into_iter().chain(array));
        let bad = opccore::cfb::write_cfb(&[("Workbook", s)]);
        assert!(check_xls(&bad).unwrap_err().contains("ARRAY"));
    }

    /// A sheet's default row height and column widths round-trip.
    #[test]
    fn default_row_height_and_column_widths_round_trip() {
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        let f = &mut pkg.workbook.sheets[0].format;
        f.default_row_height = Some(18.0);
        f.base_col_width = 10;
        f.default_col_width = Some(12.5);
        let (back, _) = round(&pkg);
        let f = &back.workbook.sheets[0].format;
        assert_eq!(
            (f.default_row_height, f.base_col_width, f.default_col_width),
            (Some(18.0), 10, Some(12.5))
        );
    }

    /// Iterative calculation round-trips; off stays off.
    #[test]
    fn iterative_calculation_round_trips() {
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        assert_eq!(round(&pkg).0.workbook.iterate, None);
        pkg.workbook.iterate = Some((250, 0.0005));
        assert_eq!(round(&pkg).0.workbook.iterate, Some((250, 0.0005)));
    }

    /// More colours than the palette has free slots never take black's:
    /// a bordered cell's border stays black (palette 8).
    #[test]
    fn custom_colours_leave_the_border_colour_black() {
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        for i in 0..70u32 {
            let mut xf = Xf::default();
            xf.fill = Some((1, (i * 3) as u8, 200));
            xf.border = i == 69;
            let style = pkg.workbook.styles.intern(xf);
            pkg.workbook.sheets[0].set_cell(
                i + 1,
                0,
                Cell {
                    style,
                    ..Cell::number(1.0)
                },
            );
        }
        let out = write_xls(&pkg);
        check_xls(&out.bytes).unwrap();
        let cfb = opccore::cfb::Cfb::open(&out.bytes).unwrap();
        let s = cfb.read_stream("Workbook").unwrap();
        let pal = find(&s, 0x0092);
        assert_eq!(&s[pal + 6..pal + 9], &[0, 0, 0], "palette 8 is black");
        // Every bordered XF draws its sides in palette 8.
        let mut at = 0;
        let mut bordered = 0;
        while at + 4 <= s.len() {
            let ty = u16::from_le_bytes([s[at], s[at + 1]]);
            let len = u16::from_le_bytes([s[at + 2], s[at + 3]]) as usize;
            if ty == 0x00E0 {
                let b = &s[at + 4..at + 4 + len];
                let dg = u32::from_le_bytes([b[10], b[11], b[12], b[13]]);
                if dg & 0xFFFF == 0x1111 {
                    bordered += 1;
                    assert_eq!((dg >> 16) & 0x7F, 8);
                    assert_eq!((dg >> 23) & 0x7F, 8);
                    let more = u32::from_le_bytes([b[14], b[15], b[16], b[17]]);
                    assert_eq!((more & 0x7F, (more >> 7) & 0x7F), (8, 8));
                }
            }
            if ty == 0x000A && at > 0 {
                break;
            }
            at += 4 + len;
        }
        assert_eq!(bordered, 1);
    }

    /// A loaded .xlsx format's border, underline and alignment, which the
    /// model's xf doesn't hold: a thin box is written as the .xls border,
    /// the rest is named (#874 r8).
    #[test]
    fn loaded_xlsx_formats_name_what_the_xls_loses() {
        let styles = r#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><u/><sz val="11"/><name val="Calibri"/></font></fonts>
<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>
<borders count="3"><border><left/><right/><top/><bottom/><diagonal/></border><border><left style="thin"><color indexed="64"/></left><right style="thin"><color indexed="64"/></right><top style="thin"><color indexed="64"/></top><bottom style="thin"><color indexed="64"/></bottom><diagonal/></border><border><left style="thick"/><right/><top/><bottom/><diagonal/></border></borders>
<cellXfs count="4"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="0"/><xf numFmtId="0" fontId="0" fillId="0" borderId="1"/><xf numFmtId="0" fontId="0" fillId="0" borderId="2"><alignment vertical="top"/></xf></cellXfs>
</styleSheet>"#;
        let with = |loaded: &[u32]| {
            let mut pkg = book(&[]);
            pkg.set_part("xl/styles.xml", styles.as_bytes().to_vec());
            for (row, &k) in loaded.iter().enumerate() {
                let style = pkg.workbook.styles.intern(Xf {
                    loaded_from: Some(k),
                    ..Xf::default()
                });
                pkg.workbook.sheets[0].set_cell(
                    row as u32,
                    0,
                    Cell {
                        style,
                        ..Cell::number(1.0)
                    },
                );
            }
            pkg
        };
        assert!(write_xls(&with(&[0])).lost.is_empty());
        assert_eq!(write_xls(&with(&[1])).lost, ["underlined text"]);
        assert_eq!(
            write_xls(&with(&[3])).lost,
            ["borders other than a thin box", "vertical alignment"]
        );
        // The thin box is no loss: it comes back as the cell's border.
        let pkg = with(&[2]);
        let (back, lost) = round(&pkg);
        assert!(lost.is_empty(), "{lost:?}");
        let wb = &back.workbook;
        assert!(wb.styles.xf(wb.sheets[0].cells[&(0, 0)].style).border);
        // A column's or row's own format is named.
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        pkg.workbook.sheets[0].col_defs.push(crate::sheet::ColDef {
            min: 1,
            max: 1,
            width: Some(12.0),
            attrs: " style=\"1\"".into(),
            default_width: false,
        });
        assert_eq!(write_xls(&pkg).lost, ["column and row formats"]);
        let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
        pkg.workbook.sheets[0]
            .row_attrs
            .insert(0, " s=\"1\" customFormat=\"1\"".into());
        assert_eq!(write_xls(&pkg).lost, ["column and row formats"]);
    }

    /// A loaded thin box in red is written as the box, its colour named
    /// lost, by the .xls import's rule (#874 r9).
    #[test]
    fn a_loaded_coloured_thin_box_is_written_and_its_colour_named() {
        let side = |s: &str| format!(r#"<{s} style="thin"><color rgb="FFFF0000"/></{s}>"#);
        let styles = format!(
            r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="2"><border/><border>{}{}{}{}<diagonal/></border></borders><cellXfs count="2"><xf fontId="0" fillId="0" borderId="0"/><xf fontId="0" fillId="0" borderId="1"/></cellXfs></styleSheet>"#,
            side("left"),
            side("right"),
            side("top"),
            side("bottom")
        );
        let mut pkg = book(&[]);
        pkg.set_part("xl/styles.xml", styles.into_bytes());
        let style = pkg.workbook.styles.intern(Xf {
            loaded_from: Some(1),
            ..Xf::default()
        });
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                style,
                ..Cell::number(1.0)
            },
        );
        let (back, lost) = round(&pkg);
        assert_eq!(lost, ["borders other than a thin box"]);
        let wb = &back.workbook;
        assert!(wb.styles.xf(wb.sheets[0].cells[&(0, 0)].style).border);
    }

    /// Workbook and sheet settings a loaded .xlsx keeps in its parts, which
    /// the model doesn't hold, are named in the import's words (#874 r9).
    #[test]
    fn loaded_workbook_and_sheet_settings_are_named() {
        let with = |workbook: &str, sheet: &str| {
            let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
            let wb = String::from_utf8(pkg.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
            pkg.set_part(
                "xl/workbook.xml",
                wb.replace("</workbook>", &format!("{workbook}</workbook>"))
                    .into_bytes(),
            );
            let name = pkg.sheet_parts[0].clone();
            let ws = String::from_utf8(pkg.part(&name).unwrap().to_vec()).unwrap();
            pkg.set_part(
                &name,
                ws.replacen("<sheetData", &format!("{sheet}<sheetData"), 1)
                    .into_bytes(),
            );
            write_xls(&pkg).lost
        };
        let cases: [(&str, &str, &str); 11] = [
            (
                r#"<workbookProtection lockStructure="1"/>"#,
                "",
                "workbook protection",
            ),
            (
                r#"<fileSharing readOnlyRecommended="1"/>"#,
                "",
                "password to modify and read-only recommendation",
            ),
            (
                r#"<calcPr calcId="191029" calcMode="manual"/>"#,
                "",
                "manual calculation",
            ),
            (
                r#"<calcPr calcMode="autoNoTable"/>"#,
                "",
                "automatic calculation except data tables",
            ),
            (
                r#"<calcPr fullPrecision="0"/>"#,
                "",
                "precision as displayed",
            ),
            (
                r#"<definedNames><definedName name="Secret" hidden="1">Sheet1!$A$1</definedName></definedNames>"#,
                "",
                "hidden defined names (saved as visible)",
            ),
            (
                r#"<sheets><sheet name="X" sheetId="9" state="veryHidden" r:id="rId9"/></sheets>"#,
                "",
                "very hidden sheets (saved as hidden)",
            ),
            (
                "",
                r#"<sheetViews><sheetView rightToLeft="1" workbookViewId="0"/></sheetViews>"#,
                "sheet view settings",
            ),
            (
                "",
                r#"<sheetViews><sheetView showGridLines="0" workbookViewId="0"/></sheetViews>"#,
                "sheet view settings",
            ),
            (
                "",
                r#"<sheetViews><sheetView zoomScale="85" workbookViewId="0"/></sheetViews>"#,
                "sheet view settings",
            ),
            (
                "",
                r#"<sheetPr><tabColor rgb="FFFF0000"/></sheetPr>"#,
                "sheet tab colors",
            ),
        ];
        for (workbook, sheet, want) in cases {
            assert_eq!(with(workbook, sheet), [want], "{workbook}{sheet}");
        }
        // Their defaults, a built-in hidden name and a function's are none.
        let plain = with(
            r#"<workbookProtection lockStructure="0"/><calcPr calcId="191029" calcMode="auto" fullPrecision="1"/><definedNames><definedName name="_xlnm._FilterDatabase" hidden="1">Sheet1!$A$1</definedName><definedName name="F" function="1" hidden="1">Sheet1!$A$1</definedName></definedNames>"#,
            r#"<sheetViews><sheetView zoomScale="100" showGridLines="1" workbookViewId="0"/></sheetViews>"#,
        );
        // (The function name is named as a macro function, not as hidden.)
        assert_eq!(plain, ["Excel 4.0 function stored in defined names"]);
    }

    /// Every top-level child of the workbook and worksheet parts, and every
    /// package part, is accounted for: the ones the .xls loses are named,
    /// by name where the import has one, else in an "other ..." entry
    /// (#874 r10).
    #[test]
    fn every_loaded_element_and_part_is_written_or_named() {
        let with = |workbook: &str, sheet: &str, part: Option<&str>| {
            let mut pkg = book(&[(0, 0, Cell::number(1.0))]);
            let wb = String::from_utf8(pkg.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
            pkg.set_part(
                "xl/workbook.xml",
                wb.replace("</workbook>", &format!("{workbook}</workbook>"))
                    .into_bytes(),
            );
            let name = pkg.sheet_parts[0].clone();
            let ws = String::from_utf8(pkg.part(&name).unwrap().to_vec()).unwrap();
            pkg.set_part(
                &name,
                ws.replacen("</worksheet>", &format!("{sheet}</worksheet>"), 1)
                    .into_bytes(),
            );
            if let Some(part) = part {
                pkg.set_part(part, b"<x/>".to_vec());
            }
            write_xls(&pkg).lost
        };
        let ext = |inner: &str| format!(r#"<extLst><ext uri="{{X}}">{inner}</ext></extLst>"#);
        let sheet_cases: Vec<(String, &str)> = vec![
            (r#"<picture r:id="rId9"/>"#.into(), "sheet backgrounds"),
            (r#"<scenarios><scenario name="S"/></scenarios>"#.into(), "scenarios"),
            (r#"<customSheetViews><customSheetView guid="{G}"/></customSheetViews>"#.into(), "custom views"),
            (r#"<protectedRanges><protectedRange name="P" sqref="A1"/></protectedRanges>"#.into(), "protected ranges"),
            (r#"<oleObjects><oleObject progId="P" shapeId="1"/></oleObjects>"#.into(), "embedded objects and controls"),
            (r#"<legacyDrawingHF r:id="rId9"/>"#.into(), "header and footer pictures"),
            (ext(r#"<x14:sparklineGroups xmlns:x14="urn:x14"/>"#), "sparklines"),
            (ext(r#"<x14:slicerList xmlns:x14="urn:x14"/>"#), "slicers"),
            (
                r#"<sheetViews><sheetView workbookViewId="0"><pane xSplit="2000" ySplit="1500" topLeftCell="C3"/></sheetView></sheetViews>"#.into(),
                "sheet view settings",
            ),
            (
                r#"<sheetViews><sheetView defaultGridColor="0" colorId="10" workbookViewId="0"/></sheetViews>"#.into(),
                "sheet view settings",
            ),
            (r#"<webPublishItems count="1"/>"#.into(), "other sheet features (webPublishItems)"),
            (ext(r#"<x99:future xmlns:x99="urn:x99"/>"#), "other sheet features (future)"),
        ];
        for (sheet, want) in &sheet_cases {
            assert_eq!(with("", sheet, None), [*want], "{sheet}");
        }
        let book_cases: Vec<(String, &str)> = vec![
            (
                r#"<customWorkbookViews><customWorkbookView name="V"/></customWorkbookViews>"#
                    .into(),
                "custom views",
            ),
            (
                r#"<pivotCaches><pivotCache cacheId="1" r:id="rId9"/></pivotCaches>"#.into(),
                "PivotTables",
            ),
            (ext(r#"<x14:slicerCaches xmlns:x14="urn:x14"/>"#), "slicers"),
            (
                r#"<webPublishObjects count="1"/>"#.into(),
                "other workbook features (webPublishObjects)",
            ),
        ];
        for (workbook, want) in &book_cases {
            assert_eq!(with(workbook, "", None), [*want], "{workbook}");
        }
        for (part, want) in [
            ("customXml/item1.xml", "custom XML data"),
            ("xl/slicers/slicer1.xml", "slicers"),
            ("xl/timelines/timeline1.xml", "timelines"),
            (
                "xl/embeddings/oleObject1.bin",
                "embedded objects and controls",
            ),
            ("xl/connections.xml", "external data ranges"),
            ("xl/pivotTables/pivotTable1.xml", "PivotTables"),
            (
                "xl/richData/rdrichvalue.xml",
                "other workbook content (xl/richData)",
            ),
        ] {
            assert_eq!(with("", "", Some(part)), [want], "{part}");
        }
        // A plain view with a frozen pane, Excel's bookkeeping and the parts
        // every package has name nothing.
        let plain = with(
            &ext(r#"<x15:workbookPr chartTrackingRefBase="1" xmlns:x15="urn:x15"/>"#),
            r#"<sheetViews><sheetView colorId="64" defaultGridColor="1" workbookViewId="0"><pane ySplit="1" topLeftCell="A2" state="frozen"/></sheetView></sheetViews><printOptions/><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#,
            Some("xl/printerSettings/printerSettings1.bin"),
        );
        assert!(plain.is_empty(), "{plain:?}");
    }

    /// A shared string with rich text runs a cell shows is named (#874 r10).
    #[test]
    fn rich_shared_strings_are_named() {
        let saved = crate::xlsx::save_xlsx(&book(&[(0, 0, Cell::text("hi"))]));
        let pkg = crate::xlsx::load_xlsx(&saved).unwrap();
        assert!(write_xls(&pkg).lost.is_empty());
        let mut rich = pkg.clone();
        let sst = "xl/sharedStrings.xml";
        let xml = String::from_utf8(rich.part(sst).unwrap().to_vec()).unwrap();
        let xml = xml.replace(
            "<si><t>hi</t></si>",
            "<si><r><rPr><b/></rPr><t>hi</t></r></si>",
        );
        assert!(xml.contains("<r>"), "{xml}");
        rich.set_part(sst, xml.into_bytes());
        assert_eq!(write_xls(&rich).lost, ["formatting within cell text"]);
    }

    /// Excel's number formats by id: the fixed ones as the id alone, the
    /// currency ones (5-8, 41-44) with a FORMAT record keeping the id, and
    /// custom ones from 164 (#874 r8).
    #[test]
    fn number_formats_get_excels_ids_and_records() {
        let codes = [
            "0.00%",
            "\"$\"#,##0_);\\(\"$\"#,##0\\)",
            "0.0",
            "_(\"$\"* #,##0_);_(\"$\"* \\(#,##0\\);_(\"$\"* \"-\"_);_(@_)",
        ];
        let mut pkg = book(&[]);
        for (row, code) in codes.iter().enumerate() {
            let mut xf = Xf::default();
            xf.set_code(Some(code.to_string()));
            let style = pkg.workbook.styles.intern(xf);
            pkg.workbook.sheets[0].set_cell(
                row as u32,
                0,
                Cell {
                    style,
                    ..Cell::number(1.0)
                },
            );
        }
        let out = write_xls(&pkg);
        check_xls(&out.bytes).unwrap();
        let cfb = opccore::cfb::Cfb::open(&out.bytes).unwrap();
        let s = cfb.read_stream("Workbook").unwrap();
        let mut ids = Vec::new();
        let mut at = 0;
        while at + 4 <= s.len() {
            let ty = u16::from_le_bytes([s[at], s[at + 1]]);
            let len = u16::from_le_bytes([s[at + 2], s[at + 3]]) as usize;
            if ty == 0x041E {
                ids.push(u16::from_le_bytes([s[at + 4], s[at + 5]]));
            }
            if ty == 0x000A {
                break;
            }
            at += 4 + len;
        }
        assert_eq!(ids, [5, 164, 42]);
        let (back, _) = round(&pkg);
        let wb = &back.workbook;
        for (row, code) in codes.iter().enumerate() {
            let style = wb.sheets[0].cells[&(row as u32, 0)].style;
            assert_eq!(wb.styles.xf(style).code.as_deref(), Some(*code));
        }
    }

    /// The offset of the first record of type `ty`.
    fn find(s: &[u8], ty: u16) -> usize {
        let mut at = 0;
        loop {
            let t = u16::from_le_bytes([s[at], s[at + 1]]);
            if t == ty {
                return at;
            }
            at += 4 + u16::from_le_bytes([s[at + 2], s[at + 3]]) as usize;
        }
    }
}
