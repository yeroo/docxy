//! Import of the spreadsheet formats that are not SpreadsheetML (#603):
//! Excel 97-2003 `.xls` (BIFF8 in an OLE2 compound file), Excel Binary
//! Workbook `.xlsb` (BIFF12 records in a ZIP) and OpenDocument Spreadsheet
//! `.ods`.
//!
//! Opening one is an *import*, like a CSV: the reader builds a fresh
//! [`SheetPackage`] (as `new_xlsx` makes one) with the sheets, values, formulas,
//! number formats, date system and defined names, so everything downstream
//! (engine, editor, save) sees an ordinary workbook and saving writes
//! `.xlsx`. Nothing the readers don't model survives the import.
//!
//! The readers are lenient: a record or token they don't understand is
//! skipped, and a formula they can't decompile keeps its cached value and
//! loses only the formula. The hard errors are the files that can't be read
//! at all: encrypted ones, Excel 5.0/95 workbooks, OLE2 files that are not
//! spreadsheets, broken containers, and files that ask for more than
//! [`Limits`] allows (too many cells or sheets, or `.ods` repeats that
//! would expand past their budget). A limit is never met by truncating.

mod ftab;
mod ods;
mod ptg;
mod xls;
mod xlsb;

use std::collections::{BTreeMap, HashMap};

use crate::sheet::{Cell, DefinedName, Xf};
use crate::xlsx::{SheetPackage, XlsxError, load_xlsx};

/// The file format a workbook was opened from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// SpreadsheetML (`.xlsx`, `.xlsm`, `.xltx`, `.xltm`).
    Xlsx,
    /// Excel 97-2003 Workbook (`.xls`, BIFF8).
    Xls,
    /// Excel Binary Workbook (`.xlsb`, BIFF12).
    Xlsb,
    /// OpenDocument Spreadsheet (`.ods`).
    Ods,
}

impl SourceFormat {
    /// The format's name as Excel's Save As type list spells it.
    pub fn label(self) -> &'static str {
        match self {
            SourceFormat::Xlsx => "Excel Workbook",
            SourceFormat::Xls => "Excel 97-2003 Workbook",
            SourceFormat::Xlsb => "Excel Binary Workbook",
            SourceFormat::Ods => "OpenDocument Spreadsheet",
        }
    }

    /// Whether opening this format is an import (saving writes `.xlsx`
    /// beside it rather than over it).
    pub fn is_import(self) -> bool {
        self != SourceFormat::Xlsx
    }
}

/// Why [`open_workbook`] could not read a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The SpreadsheetML reader's error, for anything that isn't one of the
    /// imported formats.
    Xlsx(XlsxError),
    /// An `.xls` with a FILEPASS record.
    EncryptedXls,
    /// An OLE2 file holding an encrypted OOXML package (`EncryptionInfo`).
    EncryptedPackage,
    /// A BIFF5/BIFF7 workbook (Excel 5.0/95).
    Biff5,
    /// An OLE2 file with no `Workbook` stream (a `.doc`, an `.mpp`, ...).
    NotSpreadsheet,
    /// The container itself can't be read (a broken compound file or ZIP, a
    /// workbook part missing), or the file asks for more than [`Limits`]
    /// allows (too many cells, sheets or repeated cells).
    Corrupt(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Xlsx(e) => e.fmt(f),
            OpenError::EncryptedXls => {
                f.write_str("password-protected .xls files are not supported")
            }
            OpenError::EncryptedPackage => {
                f.write_str("password-protected workbooks are not supported")
            }
            OpenError::Biff5 => f.write_str("Excel 5.0/95 workbooks are not supported"),
            OpenError::NotSpreadsheet => {
                f.write_str("not a spreadsheet: OLE2 file without a Workbook stream")
            }
            OpenError::Corrupt(why) => write!(f, "cannot read workbook: {why}"),
        }
    }
}

impl std::error::Error for OpenError {}

const OLE2: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const ODS_MIMETYPE: &str = "application/vnd.oasis.opendocument.spreadsheet";

/// Open a workbook of any supported format, chosen by its bytes (never by a
/// file extension): an OLE2 file with a `Workbook` stream is `.xls`, a ZIP
/// with `xl/workbook.bin` is `.xlsb`, a ZIP whose `mimetype` is ODF
/// spreadsheet is `.ods`, and anything else goes to [`load_xlsx`] unchanged.
pub fn open_workbook(data: &[u8]) -> Result<(SheetPackage, SourceFormat), OpenError> {
    if data.len() >= 8 && data[..8] == OLE2 {
        let cfb = opccore::cfb::Cfb::open(data).map_err(OpenError::Corrupt)?;
        let names = cfb.stream_names();
        let has = |n: &str| names.iter().any(|s| s.eq_ignore_ascii_case(n));
        if has("EncryptionInfo") || has("EncryptedPackage") {
            return Err(OpenError::EncryptedPackage);
        }
        let stream = ["Workbook", "Book"]
            .iter()
            .find_map(|n| cfb.read_stream(n))
            .ok_or(OpenError::NotSpreadsheet)?;
        return Ok((xls::read(&stream)?.build(), SourceFormat::Xls));
    }
    if let Some(zip) = opccore::zip::ZipArchive::open(data) {
        if zip.find("xl/workbook.bin").is_some() {
            return Ok((xlsb::read(&zip)?.build(), SourceFormat::Xlsb));
        }
        if let Some(mime) = zip.read("mimetype") {
            if String::from_utf8_lossy(&mime).trim() == ODS_MIMETYPE {
                return Ok((ods::read(&zip)?.build(), SourceFormat::Ods));
            }
        }
    }
    load_xlsx(data)
        .map(|pkg| (pkg, SourceFormat::Xlsx))
        .map_err(OpenError::Xlsx)
}

/// What an imported file may make the import allocate. Past any of these
/// the file is refused ([`OpenError::Corrupt`]), never silently truncated.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Cells across the workbook, every reader ("too many cells"). Far
    /// past any real workbook, which memory bounds long before this.
    pub cells: usize,
    /// The copies `.ods` repeats add (a repeated cell's or row's 2nd, 3rd,
    /// ... instance): the guard against a tiny file that expands to
    /// billions ("too many repeated cells").
    pub repeat_cells: usize,
    /// The text and formula bytes those copies add.
    pub repeat_bytes: usize,
    /// Sheets ("too many sheets").
    pub sheets: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            cells: 50_000_000,
            repeat_cells: 4_000_000,
            repeat_bytes: 256 << 20,
            sheets: 4_096,
        }
    }
}

/// One sheet as a reader collects it.
#[derive(Debug, Default)]
pub(crate) struct SheetIn {
    pub name: String,
    /// 0-based (row, col) → cell; `Cell::style` here is an index into
    /// [`BookIn::formats`], not yet into the workbook's styles.
    pub cells: BTreeMap<(u32, u32), Cell>,
}

/// A workbook as a reader collects it, before it becomes a package.
#[derive(Debug, Default)]
pub(crate) struct BookIn {
    pub sheets: Vec<SheetIn>,
    /// Number-format codes the cells' `style` indices name; index 0 is
    /// always General.
    pub formats: Vec<String>,
    /// `formats` by code.
    format_ix: HashMap<String, u32>,
    pub names: Vec<DefinedName>,
    pub date1904: bool,
    pub limits: Limits,
    /// What has been charged against `limits` so far.
    cells: usize,
    repeat_cells: usize,
    repeat_bytes: usize,
}

impl BookIn {
    #[cfg(test)]
    pub fn new() -> BookIn {
        BookIn::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> BookIn {
        BookIn {
            formats: vec!["General".to_string()],
            limits,
            ..BookIn::default()
        }
    }

    /// The `formats` index of `code`, added when new.
    pub fn format_index(&mut self, code: &str) -> u32 {
        if code.eq_ignore_ascii_case("General") || code.is_empty() {
            return 0;
        }
        if let Some(&i) = self.format_ix.get(code) {
            return i;
        }
        self.formats.push(code.to_string());
        let i = (self.formats.len() - 1) as u32;
        self.format_ix.insert(code.to_string(), i);
        i
    }

    /// Start a new sheet, unless the workbook already has as many as
    /// `limits` allows.
    pub fn push_sheet(&mut self, sheet: SheetIn) -> Result<(), OpenError> {
        if self.sheets.len() >= self.limits.sheets {
            return Err(OpenError::Corrupt(format!(
                "too many sheets (more than {})",
                self.limits.sheets
            )));
        }
        self.sheets.push(sheet);
        Ok(())
    }

    /// Account for `n` more cells in the workbook.
    pub fn charge_cells(&mut self, n: usize) -> Result<(), OpenError> {
        self.cells = self.cells.saturating_add(n);
        if self.cells > self.limits.cells {
            return Err(OpenError::Corrupt(format!(
                "too many cells (more than {})",
                self.limits.cells
            )));
        }
        Ok(())
    }

    /// Account for `copies` cells a repeat adds, holding `bytes` of text
    /// and formulas between them. Charged before the copies are made.
    pub fn charge_repeats(&mut self, copies: usize, bytes: usize) -> Result<(), OpenError> {
        self.repeat_cells = self.repeat_cells.saturating_add(copies);
        self.repeat_bytes = self.repeat_bytes.saturating_add(bytes);
        if self.repeat_cells > self.limits.repeat_cells
            || self.repeat_bytes > self.limits.repeat_bytes
        {
            return Err(OpenError::Corrupt("too many repeated cells".into()));
        }
        Ok(())
    }

    /// The package: a fresh workbook with these sheets, cells, number
    /// formats, names and date system, built in time linear in its size.
    /// Formula text goes through [`crate::formula::file_formula`], so it is
    /// stored as [`load_xlsx`] would store it. Sheet names are made valid
    /// for Excel ([`valid_sheet_names`]).
    pub fn build(mut self) -> SheetPackage {
        if self.sheets.is_empty() {
            self.sheets.push(SheetIn {
                name: "Sheet1".to_string(),
                ..SheetIn::default()
            });
        }
        let raw: Vec<String> = self.sheets.iter().map(|s| s.name.clone()).collect();
        let names = valid_sheet_names(raw.iter().map(String::as_str));
        let renames = sheet_renames(&raw, &names);
        let mut pkg = crate::xlsx::new_xlsx_sheets(&names);
        // Each format code becomes an xf (the codes are distinct, so none
        // needs interning); General stays the default xf 0.
        let mut xf_of = vec![0u32; self.formats.len()];
        for (i, code) in self.formats.iter().enumerate().skip(1) {
            let mut xf = Xf::default();
            xf.set_code(Some(code.clone()));
            pkg.workbook.styles.xfs.push(xf);
            xf_of[i] = (pkg.workbook.styles.xfs.len() - 1) as u32;
        }
        for (at, sheet) in self.sheets.into_iter().enumerate() {
            let cells = &mut pkg.workbook.sheets[at].cells;
            for (key, mut cell) in sheet.cells {
                cell.style = xf_of.get(cell.style as usize).copied().unwrap_or(0);
                if let Some(f) = cell.formula.take() {
                    let f = retarget(&f, &renames, false).unwrap_or(f);
                    cell.formula = Some(crate::formula::file_formula(&f).into_owned());
                }
                cells.insert(key, cell);
            }
        }
        for mut name in self.names {
            let f = retarget(&name.formula, &renames, true).unwrap_or(name.formula);
            name.formula = crate::formula::file_formula(&f).into_owned();
            pkg.workbook.defined_names.push(name);
        }
        pkg.workbook.date1904 = self.date1904;
        pkg
    }
}

/// Sheet names Excel accepts, in order: at most 31 characters, none of
/// `[]:*?/\`, not empty (`SheetN` instead), and unique ignoring case (a
/// repeat becomes `Name (2)`). References to a renamed sheet follow it
/// ([`sheet_renames`], [`retarget`]).
pub(crate) fn valid_sheet_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    fn cut(s: &str, max: usize) -> String {
        s.chars().take(max).collect()
    }
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Per base name, the next suffix to try, so N repeats of one name cost
    // N probes, not N squared.
    let mut next: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (i, raw) in names.enumerate() {
        let clean: String = raw
            .chars()
            .map(|c| if "[]:*?/\\".contains(c) { '_' } else { c })
            .collect();
        let mut name = cut(clean.trim_matches('\''), 31);
        if name.trim().is_empty() {
            name = format!("Sheet{}", i + 1);
        }
        let base = name.clone();
        let n = next.entry(base.to_lowercase()).or_insert(2);
        while taken.contains(&name.to_lowercase()) {
            let suffix = format!(" ({n})");
            name = format!("{}{suffix}", cut(&base, 31 - suffix.chars().count()));
            *n += 1;
        }
        taken.insert(name.to_lowercase());
        out.push(name);
    }
    out
}

/// The sheet renames references must follow when `raw` names become
/// `valid`, looked up by old name without regard to case.
struct Renames {
    /// (old, new), in sheet order.
    pairs: Vec<(String, String)>,
    /// Lowercased old name → index into `pairs`.
    by_old: HashMap<String, usize>,
}

/// The renames for `raw` names becoming `valid`. A repeat of an earlier
/// name (a second "Data") is left out: references to "Data" mean the first,
/// as a reader resolves them, and keep doing so. An empty name has no
/// references.
fn sheet_renames(raw: &[String], valid: &[String]) -> Renames {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut r = Renames {
        pairs: Vec::new(),
        by_old: HashMap::new(),
    };
    for (old, new) in raw.iter().zip(valid) {
        let low = old.to_lowercase();
        if seen.insert(low.clone()) && old != new && !old.is_empty() {
            r.by_old.insert(low, r.pairs.len());
            r.pairs.push((old.clone(), new.clone()));
        }
    }
    r
}

/// Every token in `src` that may be a sheet qualifier, lowercased: each
/// quoted `'…'` name (`''` unescaped; a `'First:Last'` span gives both
/// ends) and each bare name directly before `!` or `:`, outside string
/// literals. A superset of the sheets the formula names, found in one pass
/// without parsing.
fn sheet_qualifiers(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i += 1;
                while i < chars.len() {
                    if chars[i] == '"' {
                        if chars.get(i + 1) == Some(&'"') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            '\'' => {
                let mut name = String::new();
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\'' {
                        if chars.get(i + 1) == Some(&'\'') {
                            name.push('\'');
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    name.push(chars[i]);
                    i += 1;
                }
                i += 1;
                let name = name.to_lowercase();
                if let Some((a, b)) = name.split_once(':') {
                    out.push(a.to_string());
                    out.push(b.to_string());
                }
                out.push(name);
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' => {
                let begin = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '.'))
                {
                    i += 1;
                }
                if matches!(chars.get(i), Some('!' | ':')) {
                    out.push(chars[begin..i].iter().collect::<String>().to_lowercase());
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// `src` with its references to each renamed sheet following the rename,
/// or `None` when it names none of them (or doesn't parse). Only a formula
/// with a qualifier that is an old name ([`sheet_qualifiers`], one pass
/// and a hash lookup each) is parsed, so a workbook pays for the formulas a
/// rename touches. Every rename is applied through a placeholder first, so
/// one sheet's new name can be another's old one. A defined name (`name`)
/// goes through [`crate::formula::rewrite_defined_name`], so a union of
/// areas or a `Sheet!#REF!` area follows too.
fn retarget(src: &str, renames: &Renames, name: bool) -> Option<String> {
    use crate::formula::{parse, rename_sheet_in_expr, rewrite_defined_name, to_string};
    if renames.pairs.is_empty() {
        return None;
    }
    let mut hits: Vec<usize> = sheet_qualifiers(src)
        .iter()
        .filter_map(|q| renames.by_old.get(q).copied())
        .collect();
    hits.sort_unstable();
    hits.dedup();
    if hits.is_empty() {
        return None;
    }
    let placeholder = |k: usize| format!("\u{1}legacy sheet {k}\u{1}");
    let steps = hits
        .iter()
        .map(|&k| (renames.pairs[k].0.clone(), placeholder(k)))
        .chain(
            hits.iter()
                .map(|&k| (placeholder(k), renames.pairs[k].1.clone())),
        );
    if name {
        let mut text = src.to_string();
        for (old, new) in steps {
            let f = |e: &crate::formula::Expr| rename_sheet_in_expr(e, &old, &new);
            if let Some(t) = rewrite_defined_name(&text, f, Some((&old, &new))) {
                text = t;
            }
        }
        return (text != src).then_some(text);
    }
    let mut e = parse(src).ok()?;
    for (old, new) in steps {
        e = rename_sheet_in_expr(&e, &old, &new);
    }
    Some(to_string(&e))
}

/// A reference's sheet part as a formula spells it: `Data!`, `'My Sheet'!`,
/// or for a 3D span `'Q1:Q3'!` / `Jan:Mar!`. Empty `first` means no sheet.
pub(crate) fn sheet_prefix(first: &str, last: &str) -> String {
    if first.is_empty() {
        return String::new();
    }
    if first == last {
        return format!("{}!", crate::sheet::quote_sheet_name(first));
    }
    crate::formula::span_prefix(first, last)
}

/// An RK number (BIFF8 and BIFF12): a 30-bit integer or the top of an
/// IEEE double, either optionally scaled by 1/100.
pub(crate) fn rk(v: u32) -> f64 {
    let x = if v & 2 != 0 {
        ((v as i32) >> 2) as f64
    } else {
        f64::from_bits(((v & 0xFFFF_FFFC) as u64) << 32)
    };
    if v & 1 != 0 { x / 100.0 } else { x }
}

/// Whether (row, col) is on the grid. A cell a file puts past it never
/// reaches the model.
pub(crate) fn on_grid(row: u32, col: u32) -> bool {
    row < crate::sheet::MAX_ROWS && col < crate::sheet::MAX_COLS
}

/// Make `cell` the anchor of a legacy (CSE) array formula `formula` over
/// `(r1, r2, c1, c2)`, as an `.xlsx` holds one: the formula and its
/// `t="array" ref` on the anchor, the other cells plain values. A range that
/// is inverted or leaves the grid is not an array this can hold: `false`.
pub(crate) fn set_array(
    cell: &mut Cell,
    (r1, r2, c1, c2): (u32, u32, u32, u32),
    formula: String,
) -> bool {
    if r2 < r1 || c2 < c1 || !on_grid(r2, c2) {
        return false;
    }
    let name = crate::sheet::cell_name;
    let ext = if (r1, c1) == (r2, c2) {
        name(r1, c1)
    } else {
        format!("{}:{}", name(r1, c1), name(r2, c2))
    };
    cell.formula = Some(formula);
    cell.f_attrs = Some(format!(" t=\"array\" ref=\"{ext}\""));
    cell.spill = Some((r2 - r1 + 1, c2 - c1 + 1));
    true
}

/// The error text of a BIFF error code (shared by BIFF8 and BIFF12).
pub(crate) fn biff_error(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        0x2B => "#GETTING_DATA",
        _ => "#N/A",
    }
}

/// The built-in number formats BIFF files leave implicit (no FORMAT record
/// for ids below 164).
pub(crate) fn builtin_format(id: u32) -> Option<String> {
    crate::numfmt::builtin_code(id).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_need_an_upright_range_on_the_grid() {
        let mut c = Cell::default();
        assert!(!set_array(&mut c, (5, 2, 0, 0), "1".into()));
        assert!(!set_array(&mut c, (0, 0, 3, 1), "1".into()));
        assert!(!set_array(&mut c, (0, 2_000_000, 0, 0), "1".into()));
        assert_eq!(c, Cell::default());
        assert!(set_array(&mut c, (0, 1, 1, 1), "A1:A2*2".into()));
        assert_eq!(c.spill, Some((2, 1)));
    }

    #[test]
    fn rk_numbers() {
        assert_eq!(rk((12345u32 << 2) | 2 | 1), 123.45);
        assert_eq!(rk(((-7i32 as u32) << 2) | 2), -7.0);
        assert_eq!(rk((1.5f64.to_bits() >> 32) as u32), 1.5);
    }

    #[test]
    fn sheet_names_are_made_valid() {
        let long = "x".repeat(40);
        let got = valid_sheet_names(
            [
                "Data",
                "",
                "a/b[c]:d*e?f\\g",
                long.as_str(),
                "DATA",
                "Data",
                "'quoted'",
            ]
            .into_iter(),
        );
        assert_eq!(
            got,
            [
                "Data".to_string(),
                "Sheet2".into(),
                "a_b_c__d_e_f_g".into(),
                "x".repeat(31),
                "DATA (2)".into(),
                "Data (3)".into(),
                "quoted".into(),
            ]
        );
        // A long repeat stays within 31 characters.
        let two = valid_sheet_names([long.as_str(), long.as_str()].into_iter());
        assert_eq!(two[1], format!("{} (2)", "x".repeat(27)));
    }

    /// A renamed sheet takes its references along: a formula and a defined
    /// name on a 33-character name, a second "Data" (references stay on the
    /// first), and a truncated name equal to another sheet's own.
    #[test]
    fn references_follow_renamed_sheets() {
        use crate::sheet::CellValue;
        let long = "L".repeat(33);
        let cut = "L".repeat(31);
        let sheet = |name: &str, cells: Vec<((u32, u32), Cell)>| SheetIn {
            name: name.to_string(),
            cells: cells.into_iter().collect(),
        };
        let num = |v: f64| Cell {
            value: CellValue::Number(v),
            ..Cell::default()
        };
        let f = |text: &str| Cell {
            formula: Some(text.to_string()),
            ..Cell::default()
        };
        let mut book = BookIn::new();
        for s in [
            sheet(&long, vec![((0, 0), num(5.0))]),
            sheet(&cut, vec![((0, 0), num(7.0))]),
            sheet("Data", vec![((0, 0), num(1.0))]),
            sheet("Data", vec![((0, 0), num(2.0))]),
            // A raw name with `:`, between sheets named A and B: references
            // to 'A:B'! mean it, not the span A..B.
            sheet("A", vec![((0, 0), num(100.0))]),
            sheet("A:B", vec![((0, 0), num(3.0)), ((1, 0), num(4.0))]),
            sheet("B", vec![((0, 0), num(200.0))]),
            sheet(
                "Calc",
                vec![
                    ((0, 0), f(&format!("'{long}'!A1*2"))),
                    ((1, 0), f(&format!("'{cut}'!A1"))),
                    ((2, 0), f("Data!A1")),
                    // A 3D span, Excel's quoted spelling, with a renamed end.
                    ((3, 0), f(&format!("SUM('{cut}:{long}'!A1)"))),
                    ((4, 0), f("'A:B'!A1")),
                    ((5, 0), f("SUM('A:B'!A1:A2)")),
                ],
            ),
        ] {
            book.push_sheet(s).unwrap();
        }
        book.names.push(DefinedName {
            name: "TheVal".into(),
            scope: None,
            formula: format!("'{long}'!$A$1"),
        });
        book.names.push(DefinedName {
            name: "Colon".into(),
            scope: None,
            formula: "'A:B'!$A$2".into(),
        });
        let mut pkg = book.build();
        let wb = &mut pkg.workbook;
        let names: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                cut.as_str(),
                &format!("{} (2)", "L".repeat(27)),
                "Data",
                "Data (2)",
                "A",
                "A_B",
                "B",
                "Calc"
            ]
        );
        let mut engine = crate::engine::Engine::new(wb);
        engine.recalc_all(wb);
        let calc = &wb.sheets[7];
        assert_eq!(calc.cell(0, 0).unwrap().value, CellValue::Number(10.0));
        assert_eq!(calc.cell(1, 0).unwrap().value, CellValue::Number(7.0));
        assert_eq!(calc.cell(2, 0).unwrap().value, CellValue::Number(1.0));
        // 'cut:long' spans sheets 1..0 → the renamed ends span 0..1: 7 + 5.
        assert_eq!(calc.cell(3, 0).unwrap().value, CellValue::Number(12.0));
        // 'A:B'! is the sheet now named A_B: 3, and 3 + 4, not A..B sums.
        assert_eq!(calc.cell(4, 0).unwrap().formula.as_deref(), Some("A_B!A1"));
        assert_eq!(calc.cell(4, 0).unwrap().value, CellValue::Number(3.0));
        assert_eq!(calc.cell(5, 0).unwrap().value, CellValue::Number(7.0));
        assert_eq!(wb.defined_names[0].formula, format!("{cut}!$A$1"));
        assert_eq!(wb.defined_names[1].formula, "A_B!$A$2");
    }

    /// Renames cost only the formulas they touch: 4,096 renamed sheets and
    /// 200,000 formulas naming none of them build quickly.
    #[test]
    fn many_renames_over_many_formulas_stay_linear() {
        let mut book = BookIn::new();
        for i in 0..4_096 {
            book.push_sheet(SheetIn {
                name: format!("{}{i:04}", "N".repeat(32)),
                ..SheetIn::default()
            })
            .unwrap();
        }
        let cells = &mut book.sheets[0].cells;
        for r in 0..200_000u32 {
            cells.insert(
                (r, 0),
                Cell {
                    formula: Some(format!("Other!A{} + 'Some Sheet'!B2 + \"x!y\"", r + 1)),
                    ..Cell::default()
                },
            );
        }
        let started = std::time::Instant::now();
        let pkg = book.build();
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        assert_eq!(pkg.workbook.sheets[0].name.chars().count(), 31);
    }

    /// A defined name follows a rename area by area, `Sheet!#REF!` too.
    #[test]
    fn defined_names_follow_renames_through_unions_and_ref_errors() {
        let long = "L".repeat(33);
        let cut = "L".repeat(31);
        let renames = sheet_renames(std::slice::from_ref(&long), std::slice::from_ref(&cut));
        let got = retarget(&format!("'{long}'!$A:$A,'{long}'!#REF!"), &renames, true);
        assert_eq!(got, Some(format!("{cut}!$A:$A,{cut}!#REF!")));
    }

    #[test]
    fn qualifiers_are_found_in_one_pass() {
        let q = sheet_qualifiers("SUM('It''s:Q3'!A1, Data!B2, \"no!t\", Jan:Mar!C1, A1:B2)");
        for want in ["it's:q3", "it's", "q3", "data", "jan", "mar", "a1"] {
            assert!(q.contains(&want.to_string()), "{want} in {q:?}");
        }
        assert!(!q.iter().any(|s| s.contains("no")), "{q:?}");
    }

    #[test]
    fn many_sheets_build_in_linear_time_and_too_many_are_refused() {
        let mut book = BookIn::new();
        for i in 0..2_000 {
            book.push_sheet(SheetIn {
                name: format!("S{i}"),
                ..SheetIn::default()
            })
            .unwrap();
        }
        let started = std::time::Instant::now();
        let pkg = book.build();
        assert_eq!(pkg.workbook.sheets.len(), 2_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let back = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        assert_eq!(back.workbook.sheets[1999].name, "S1999");

        let mut book = BookIn::new();
        let started = std::time::Instant::now();
        let err = (0..20_000)
            .map(|i| {
                book.push_sheet(SheetIn {
                    name: format!("S{i}"),
                    ..SheetIn::default()
                })
            })
            .find_map(Result::err)
            .unwrap();
        assert!(err.to_string().contains("too many sheets"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn budgets_refuse_rather_than_truncate() {
        let mut book = BookIn::with_limits(Limits {
            cells: 10,
            repeat_cells: 3,
            repeat_bytes: 100,
            sheets: 2,
        });
        book.charge_cells(10).unwrap();
        assert!(
            book.charge_cells(1)
                .unwrap_err()
                .to_string()
                .contains("too many cells")
        );
        book.charge_repeats(3, 10).unwrap();
        assert!(book.charge_repeats(1, 0).is_err());
        let mut book = BookIn::with_limits(Limits {
            repeat_bytes: 100,
            ..Limits::default()
        });
        assert!(
            book.charge_repeats(1, 101)
                .unwrap_err()
                .to_string()
                .contains("too many repeated cells")
        );
    }

    #[test]
    fn sheet_prefixes_quote_as_needed() {
        assert_eq!(sheet_prefix("Data", "Data"), "Data!");
        assert_eq!(sheet_prefix("My Sheet", "My Sheet"), "'My Sheet'!");
        assert_eq!(sheet_prefix("Jan", "Mar"), "Jan:Mar!");
        assert_eq!(sheet_prefix("Q1", "Q3"), "'Q1:Q3'!");
        assert_eq!(sheet_prefix("", ""), "");
    }

    #[test]
    fn unknown_bytes_go_to_the_xlsx_reader() {
        assert_eq!(
            open_workbook(b"plain text").err(),
            Some(OpenError::Xlsx(XlsxError::NotZip))
        );
    }

    #[test]
    fn ole2_without_a_workbook_is_not_a_spreadsheet() {
        let doc = opccore::cfb::write_cfb(&[("WordDocument", vec![0u8; 600])]);
        assert_eq!(open_workbook(&doc).err(), Some(OpenError::NotSpreadsheet));
        assert_eq!(
            OpenError::NotSpreadsheet.to_string(),
            "not a spreadsheet: OLE2 file without a Workbook stream"
        );
    }

    #[test]
    fn ole2_with_encryption_info_is_an_encrypted_package() {
        let enc = opccore::cfb::write_cfb(&[
            ("EncryptionInfo", vec![4, 0, 4, 0]),
            ("EncryptedPackage", vec![0u8; 16]),
        ]);
        let err = open_workbook(&enc).err().unwrap();
        assert_eq!(err, OpenError::EncryptedPackage);
        assert_eq!(
            err.to_string(),
            "password-protected workbooks are not supported"
        );
    }
}
