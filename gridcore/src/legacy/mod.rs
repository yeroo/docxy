//! Import of the spreadsheet formats that are not SpreadsheetML (#603):
//! Excel 97-2003 `.xls` (BIFF8 in an OLE2 compound file), Excel Binary
//! Workbook `.xlsb` (BIFF12 records in a ZIP) and OpenDocument Spreadsheet
//! `.ods`.
//!
//! Opening one is an *import*, like a CSV: the reader builds a fresh
//! [`SheetPackage`] from [`new_xlsx`] with the sheets, values, formulas,
//! number formats, date system and defined names, so everything downstream
//! (engine, editor, save) sees an ordinary workbook and saving writes
//! `.xlsx`. Nothing the readers don't model survives the import.
//!
//! The readers are lenient: a record or token they don't understand is
//! skipped, and a formula they can't decompile keeps its cached value and
//! loses only the formula. The hard errors are the files that can't be read
//! at all: encrypted ones, Excel 5.0/95 workbooks and OLE2 files that are not
//! spreadsheets.

mod ftab;
mod ods;
mod ptg;
mod xls;
mod xlsb;

use std::collections::BTreeMap;

use crate::sheet::{Cell, DefinedName, Xf};
use crate::xlsx::{SheetPackage, XlsxError, load_xlsx, new_xlsx};

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
    /// workbook part missing).
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
    pub names: Vec<DefinedName>,
    pub date1904: bool,
}

impl BookIn {
    pub fn new() -> BookIn {
        BookIn {
            formats: vec!["General".to_string()],
            ..BookIn::default()
        }
    }

    /// The `formats` index of `code`, added when new.
    pub fn format_index(&mut self, code: &str) -> u32 {
        if code.eq_ignore_ascii_case("General") || code.is_empty() {
            return 0;
        }
        match self.formats.iter().position(|c| c == code) {
            Some(i) => i as u32,
            None => {
                self.formats.push(code.to_string());
                (self.formats.len() - 1) as u32
            }
        }
    }

    /// The package: [`new_xlsx`] with these sheets, cells, number formats,
    /// names and date system. Formula text goes through
    /// [`crate::formula::file_formula`], so it is stored as [`load_xlsx`]
    /// would store it.
    pub fn build(mut self) -> SheetPackage {
        let mut pkg = new_xlsx();
        if self.sheets.is_empty() {
            self.sheets.push(SheetIn {
                name: "Sheet1".to_string(),
                ..SheetIn::default()
            });
        }
        // Each format code becomes an xf; General stays the default xf 0.
        let mut xf_of = vec![0u32; self.formats.len()];
        for (i, code) in self.formats.iter().enumerate().skip(1) {
            let mut xf = Xf::default();
            xf.set_code(Some(code.clone()));
            xf_of[i] = pkg.workbook.styles.intern(xf);
        }
        for (i, sheet) in self.sheets.into_iter().enumerate() {
            let at = if i == 0 {
                pkg.workbook.sheets[0].name = sheet.name.clone();
                0
            } else {
                pkg.add_sheet(&sheet.name)
            };
            let cells = &mut pkg.workbook.sheets[at].cells;
            for (key, mut cell) in sheet.cells {
                cell.style = xf_of.get(cell.style as usize).copied().unwrap_or(0);
                if let Some(f) = cell.formula.take() {
                    cell.formula = Some(crate::formula::file_formula(&f).into_owned());
                }
                cells.insert(key, cell);
            }
        }
        for mut name in self.names {
            name.formula = crate::formula::file_formula(&name.formula).into_owned();
            pkg.workbook.defined_names.push(name);
        }
        pkg.workbook.date1904 = self.date1904;
        pkg
    }
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
    let q1 = crate::sheet::quote_sheet_name(first);
    let q2 = crate::sheet::quote_sheet_name(last);
    if q1.starts_with('\'') || q2.starts_with('\'') {
        format!(
            "'{}:{}'!",
            first.replace('\'', "''"),
            last.replace('\'', "''")
        )
    } else {
        format!("{q1}:{q2}!")
    }
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
