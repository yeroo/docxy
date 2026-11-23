//! Import of the spreadsheet formats that are not SpreadsheetML (#603):
//! Excel 97-2003 `.xls` (BIFF8 in an OLE2 compound file), Excel Binary
//! Workbook `.xlsb` (BIFF12 records in a ZIP) and OpenDocument Spreadsheet
//! `.ods`.
//!
//! Opening one is an *import*, like a CSV: the reader builds a fresh
//! [`SheetPackage`] (as `new_xlsx` makes one) with the sheets, values, formulas,
//! number formats, date system, defined names and the external links that
//! calls to add-in functions and references into other workbooks need
//! (`.xlsb`, `.xls`; #888, #903), so everything downstream
//! (engine, editor, save) sees an ordinary workbook. Nothing the readers don't
//! model survives the import. All three read the cell formats the model holds
//! (fonts, fills, thin borders), column widths, merged cells, comments, data
//! validation and conditional formats (#884). The `.xls` reader also reads
//! alignment, wrap, row heights, hidden rows, columns and sheets and frozen
//! panes, and names what it skipped ([`Opened::dropped`]).
//!
//! An `.xls` can be written back too ([`write_xls`], #874): a host in
//! Excel's Compatibility Mode saves the workbook as BIFF8 again, and names
//! what the file can't hold ([`XlsOut::lost`]) before it does.
//!
//! The readers are lenient: a record or token they don't understand is
//! skipped, and a formula they can't decompile keeps its cached value and
//! loses only the formula. The hard errors are the files that can't be read
//! at all: encrypted ones, Excel 5.0/95 workbooks, OLE2 files that are not
//! spreadsheets, broken containers, and files that ask for more than
//! [`Limits`] allows (too many cells, sheets or external links, or `.ods`
//! repeats that would expand past their budget). A limit is never met by
//! truncating.

mod ftab;
mod hlink;
mod ods;
mod page;
mod ptg;
mod ptg_write;
mod xls;
mod xls_write;
mod xlsb;
mod xlsb_style;

use std::collections::{BTreeMap, BTreeSet, HashMap};

pub use xls_write::{XlsOut, check_xls, write_xls};

/// The colour of BIFF palette index `icv` under `palette` (the file's
/// PALETTE, or the default one); `None` for the automatic and system
/// colours.
pub(crate) fn icv_rgb(icv: u32, palette: &[(u8, u8, u8); 56]) -> Option<(u8, u8, u8)> {
    match icv {
        0..=7 => Some(xls::FIXED_COLORS[icv as usize]),
        8..=63 => Some(palette[icv as usize - 8]),
        _ => None,
    }
}

/// [`icv_rgb`] under the default palette (an `.xlsx`'s `indexed` colour).
pub(crate) fn default_icv_rgb(icv: u32) -> Option<(u8, u8, u8)> {
    icv_rgb(icv, &xls_write::DEFAULT_PALETTE)
}

/// Whether a thin-box side drawn in `rgb` (`None`: automatic) keeps its
/// colour in the model's `border`, which an `.xls` writes in black: the
/// one rule for the `.xls` import and for Save As `.xls` from an `.xlsx`.
/// A box in another colour is still a box, but its colour is lost.
pub(crate) fn plain_border_color(rgb: Option<(u8, u8, u8)>) -> bool {
    rgb.is_none_or(|c| c == (0, 0, 0))
}

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
    /// allows (too many cells, sheets, external links or repeated cells).
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
/// The most bytes a `mimetype` entry may declare to be read as the ODS mark.
const MAX_MIMETYPE: u64 = 256;

/// Open a workbook of any supported format, chosen by its bytes (never by a
/// file extension): an OLE2 file with a `Workbook` stream is `.xls`, a ZIP
/// with `xl/workbook.bin` is `.xlsb`, a ZIP whose `mimetype` is ODF
/// spreadsheet is `.ods`, and anything else goes to [`load_xlsx`] unchanged.
pub fn open_workbook(data: &[u8]) -> Result<(SheetPackage, SourceFormat), OpenError> {
    open_workbook_ex(data).map(|o| (o.pkg, o.format))
}

/// A workbook [`open_workbook_ex`] read.
pub struct Opened {
    pub pkg: SheetPackage,
    pub format: SourceFormat,
    /// What the file has that the import left out, one entry per kind, in
    /// words for a "will be lost" list (`comments`, `underlined text`,
    /// ...), sorted. Only the `.xls` reader names them; empty for the other
    /// formats.
    pub dropped: Vec<String>,
}

/// [`open_workbook`], with what the import left out.
pub fn open_workbook_ex(data: &[u8]) -> Result<Opened, OpenError> {
    let opened = |pkg, format| Opened {
        pkg,
        format,
        dropped: Vec::new(),
    };
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
        let mut book = xls::read(&stream)?;
        // Every stream and storage beside the Workbook stream, by its
        // top-level name.
        let mut other: BTreeSet<String> = BTreeSet::new();
        for path in cfb.paths() {
            let top = path.split(['/', '\\']).next().unwrap_or(&path);
            let lower = top.to_ascii_lowercase();
            match lower.as_str() {
                "workbook" | "book" | "\u{1}compobj" | "\u{1}ole" => {}
                // Title, author and the rest: property-set streams the
                // import doesn't read.
                "\u{5}summaryinformation" | "\u{5}documentsummaryinformation" => {
                    book.dropped.insert("document properties");
                }
                "_vba_project_cur" => {
                    book.dropped.insert("VB project");
                }
                // A PivotTable's cache.
                "_sx_db_cur" => {
                    book.dropped.insert("PivotTables");
                }
                "revision log" | "user names" => {
                    book.dropped.insert("shared workbook revision history");
                }
                "msodatastore" => {
                    book.dropped.insert("custom XML data");
                }
                "ctls" => {
                    book.dropped.insert("embedded objects and controls");
                }
                _ if lower.starts_with("mbd") => {
                    book.dropped.insert("embedded objects and controls");
                }
                _ => {
                    other.insert(top.to_string());
                }
            }
        }
        let mut dropped: Vec<String> = book.dropped.iter().map(|d| d.to_string()).collect();
        if !book.unknown_records.is_empty() {
            let ids: Vec<String> = book
                .unknown_records
                .iter()
                .map(|t| format!("0x{t:04X}"))
                .collect();
            dropped.push(format!("other features (records {})", ids.join(", ")));
        }
        if !other.is_empty() {
            let names: Vec<String> = other.into_iter().collect();
            dropped.push(format!("other workbook content ({})", names.join(", ")));
        }
        let (formulas, names) = (book.unread_formulas, book.unread_names);
        let (pkg, more_formulas, more_names) = book.build_counted();
        let plural = xls_write::plural;
        if formulas + more_formulas > 0 {
            dropped.push(plural(
                formulas + more_formulas,
                "formula that refers to another workbook or can't be read (kept as its value)",
                "formulas that refer to other workbooks or can't be read (kept as values)",
            ));
        }
        if names + more_names > 0 {
            dropped.push(plural(
                names + more_names,
                "defined name that can't be read",
                "defined names that can't be read",
            ));
        }
        dropped.sort();
        return Ok(Opened {
            pkg,
            format: SourceFormat::Xls,
            dropped,
        });
    }
    if let Some(zip) = opccore::zip::ZipArchive::open(data) {
        if zip.find("xl/workbook.bin").is_some() {
            return Ok(opened(xlsb::read(&zip)?.build(), SourceFormat::Xlsb));
        }
        // The `mimetype` entry is a few dozen bytes; one declaring more is
        // not read here, before `load_xlsx`'s size budget (#851).
        let mime_entry = zip
            .find("mimetype")
            .filter(|e| e.uncomp_size <= MAX_MIMETYPE);
        if let Some(mime) = mime_entry.and_then(|e| zip.extract(e)) {
            if String::from_utf8_lossy(&mime).trim() == ODS_MIMETYPE {
                return Ok(opened(ods::read(&zip)?.build(), SourceFormat::Ods));
            }
        }
    }
    load_xlsx(data)
        .map(|pkg| opened(pkg, SourceFormat::Xlsx))
        .map_err(OpenError::Xlsx)
}

/// What an imported file may make the import allocate. Past any of these
/// but [`Limits::styled_blanks`] and [`Limits::column_walk`] the file is
/// refused ([`OpenError::Corrupt`]), never silently truncated; past those
/// two, formatting is left out (they hold no value).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Cells across the workbook, every reader ("too many cells"). Far
    /// past any real workbook, which memory bounds long before this.
    pub cells: usize,
    /// The copies `.ods` repeats add (a repeated cell's or row's 2nd, 3rd,
    /// ... instance, and the merges a repeated row's copies make): the
    /// guard against a tiny file that expands to billions ("too many
    /// repeated cells").
    pub repeat_cells: usize,
    /// The text and formula bytes those copies add.
    pub repeat_bytes: usize,
    /// Sheets ("too many sheets").
    pub sheets: usize,
    /// Distinct external books the import links: every `.xls` SUPBOOK
    /// whose path it decodes, every `.xlsb` link part it reads ("too many
    /// external links"). Their cached cells count as `cells`.
    pub links: usize,
    /// Empty cells an `.ods` keeps only for their format (#884), across
    /// the workbook. Past it, further ones are left out, as they hold no
    /// value. That loses formatting, never data, and is no error.
    pub styled_blanks: usize,
    /// Column default styles an `.ods` looks up for empty cells without a
    /// style of their own (#884): a run across many columns visits each
    /// style it crosses. Past it, empty cells take no column style (no
    /// styled blanks or conditional styles from it), never a refusal.
    pub column_walk: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            cells: 50_000_000,
            repeat_cells: 4_000_000,
            repeat_bytes: 256 << 20,
            sheets: 4_096,
            links: 4_096,
            styled_blanks: 1_000_000,
            column_walk: 4_000_000,
        }
    }
}

/// One sheet as a reader collects it.
#[derive(Debug, Default)]
pub(crate) struct SheetIn {
    pub name: String,
    /// 0-based (row, col) → cell; `Cell::style` here is an index into
    /// [`BookIn::xfs`], not yet into the workbook's styles.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// Merged ranges (r1, c1, r2, c2).
    pub merges: Vec<(u32, u32, u32, u32)>,
    /// Column definitions.
    pub cols: Vec<ColIn>,
    /// Rows with a height, hidden or in an outline.
    pub rows: Vec<RowIn>,
    /// Frozen rows and columns.
    pub freeze: (u32, u32),
    pub hidden: bool,
    /// The sheet's default row height (points), base column width
    /// (characters) and default column width, when the file gives them.
    pub default_row_height: Option<f64>,
    pub base_col_width: Option<u32>,
    pub default_col_width: Option<f64>,
    /// Cell hyperlinks in file order: (range, model target). A later one wins
    /// over a cell an earlier one covers.
    pub hyperlinks: Vec<(hlink::Rect, String)>,
    /// The sheet's page setup; the readers that don't read one leave the
    /// model's default.
    pub page_setup: crate::print::setup::PageSetup,
    /// Manual page breaks, as the model holds them.
    pub row_breaks: Vec<crate::sheet::PageBreak>,
    pub col_breaks: Vec<crate::sheet::PageBreak>,
    /// Where outline summaries sit.
    pub outline: crate::sheet::OutlineSettings,
    /// Cell comments (#884): (row, col, author, text).
    pub comments: Vec<(u32, u32, String, String)>,
    /// Data-validation rules (#884).
    pub validations: Vec<DvIn>,
    /// Conditional-formatting blocks (#884), in file order.
    pub cond_formats: Vec<CfIn>,
}

/// A data-validation rule as a reader collects it (#884): its formulas in
/// Excel's syntax, relative to `base`, the cell the file's format bases
/// them at. [`BookIn::build`] moves them to the model's anchor
/// ([`anchor_of`] its ranges).
#[derive(Clone, Debug)]
pub(crate) struct DvIn {
    pub rule: crate::sheet::DataValidation,
    pub base: (u32, u32),
}

impl DvIn {
    /// The rule, its formulas moved from `base` to the model's anchor;
    /// `None` when one must move and doesn't parse.
    pub(crate) fn placed(self) -> Option<crate::sheet::DataValidation> {
        let DvIn { rule: mut dv, base } = self;
        let to = anchor_of(&dv.ranges).unwrap_or(base);
        dv.formula1 = rebase(&dv.formula1, Some(base), to)?;
        dv.formula2 = rebase(&dv.formula2, Some(base), to)?;
        Some(dv)
    }
}

impl CfIn {
    /// The block, its rules' formulas moved from `base` to the model's
    /// anchor (now its `base`); a rule whose formula must move and
    /// doesn't parse is left out.
    pub(crate) fn placed(self) -> CfIn {
        let to = anchor_of(&self.ranges).unwrap_or(self.base);
        let rules = self
            .rules
            .into_iter()
            .filter_map(|(mut rule, dxf)| {
                for f in rule.formulas_mut() {
                    *f = rebase(f, Some(self.base), to)?;
                }
                Some((rule, dxf))
            })
            .collect();
        CfIn {
            ranges: self.ranges,
            rules,
            base: to,
        }
    }
}

/// The validation rules of `sheet` as the build places them (#884).
#[cfg(test)]
pub(crate) fn placed_dvs(sheet: &SheetIn) -> Vec<crate::sheet::DataValidation> {
    sheet
        .validations
        .iter()
        .filter_map(|v| v.clone().placed())
        .collect()
}

/// A conditional-formatting block as a reader collects it (#884): its
/// ranges and its rules, each `cellIs` or `expression` with the
/// differential format it applies, their formulas in Excel's syntax
/// relative to `base` (the cell the file's format bases them at).
/// [`BookIn::build`] moves them to the model's anchor.
#[derive(Clone, Debug, Default)]
pub(crate) struct CfIn {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    pub rules: Vec<(crate::xlsx::NewCfRule, crate::sheet::Dxf)>,
    pub base: (u32, u32),
}

/// The cell a rule's relative formulas are written for in the model: the
/// least row and least column of its ranges, as
/// `validation::anchor_of` and `cf::cell_dxf` take it. `None` for no
/// ranges.
pub(crate) fn anchor_of(ranges: &[(u32, u32, u32, u32)]) -> Option<(u32, u32)> {
    (!ranges.is_empty()).then(|| crate::validation::anchor_of(ranges))
}

/// `f`, its relative references moved from being based at `from` to being
/// based at `to` (`from` none: they are already); `None` when it must move
/// and doesn't parse, or when the move pushes a reference off the grid
/// (`translate_formula` prints it `#REF!`, which every cell would then
/// see: the rule is better left out). A `#REF!` the formula already had
/// stays.
pub(crate) fn rebase(f: &str, from: Option<(u32, u32)>, to: (u32, u32)) -> Option<String> {
    let Some(from) = from.filter(|&b| b != to && !f.is_empty()) else {
        return Some(f.to_string());
    };
    let (dr, dc) = (
        i64::from(to.0) - i64::from(from.0),
        i64::from(to.1) - i64::from(from.1),
    );
    let moved = crate::formula::translate_formula(f, dr, dc)?;
    let refs = |s: &str| s.matches("#REF!").count();
    (refs(&moved) <= refs(f)).then_some(moved)
}

/// One column definition as a reader collects it.
#[derive(Debug, Default)]
pub(crate) struct ColIn {
    pub first: u32,
    pub last: u32,
    /// Width in characters, when set.
    pub width: Option<f64>,
    pub hidden: bool,
    /// A width the file did not mark as user-set.
    pub plain: bool,
    /// Outline level (0 when ungrouped) and collapsed flag.
    pub level: u8,
    pub collapsed: bool,
}

/// One row as a reader collects it.
#[derive(Debug, Default)]
pub(crate) struct RowIn {
    pub row: u32,
    /// Height in points, when set.
    pub height: Option<f64>,
    pub hidden: bool,
    /// Outline level (0 when ungrouped) and collapsed flag.
    pub level: u8,
    pub collapsed: bool,
}

/// Another workbook or add-in whose names or cells formulas refer to, as
/// `[k]!NAME` or `[k]Sheet!A1`, with `k` its 1-based place in
/// [`BookIn::external_links`] (#888, #903). [`BookIn::build`] writes it as
/// `xl/externalLinks/externalLink{k}.xml`.
#[derive(Debug, Default)]
pub(crate) struct ExternalLink {
    /// The `Id` in `rels` that names the book (`<externalBook r:id>`).
    pub book: String,
    /// The link part's relationships as the file has them: (Id, Type,
    /// Target, TargetMode).
    pub rels: Vec<(String, String, String, Option<String>)>,
    /// The book's sheet names.
    pub sheets: Vec<String>,
    /// The book's names, in the file's order (ptgNameX counts them all),
    /// or `None` for a name that isn't imported.
    pub names: Vec<Option<ExtName>>,
    /// The book's cells as the file cached them: (sheet, row, col), the
    /// sheet an index into `sheets`. Written as `<sheetDataSet>`, which
    /// Excel shows until it updates the link.
    pub cache: BTreeMap<(u32, u32, u32), ExtValue>,
}

/// One name of an external book.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExtName {
    pub name: String,
    /// Its definition as the `.xlsx` spells it (`refersTo`), when the
    /// reader could read it ([`ext_definition`]); `None` for a function
    /// name, or a definition of another shape.
    pub refers_to: Option<String>,
    /// The 0-based sheet of the book the name is scoped to; `None` for a
    /// workbook name.
    pub sheet: Option<u32>,
}

impl ExtName {
    /// A workbook-scoped name with no definition (an add-in's function).
    pub fn function(name: impl Into<String>) -> ExtName {
        ExtName {
            name: name.into(),
            refers_to: None,
            sheet: None,
        }
    }
}

/// A test's shorthand for a function name.
#[cfg(test)]
impl From<&str> for ExtName {
    fn from(name: &str) -> ExtName {
        ExtName::function(name)
    }
}

/// A cached cell of an external book.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ExtValue {
    Num(f64),
    Str(String),
    Bool(bool),
    /// The error's text (`#N/A`).
    Err(&'static str),
}

/// A workbook as a reader collects it, before it becomes a package.
#[derive(Debug, Default)]
pub(crate) struct BookIn {
    pub sheets: Vec<SheetIn>,
    /// The cell formats the cells' `style` indices name; index 0 is always
    /// the default (General).
    pub xfs: Vec<Xf>,
    /// What the file has that the import leaves out ([`Opened::dropped`]).
    pub dropped: BTreeSet<&'static str>,
    /// The sheet the workbook opens on (an index into `sheets`).
    pub active: usize,
    /// Iterative calculation: (max iterations, max change), when on.
    pub iterate: Option<(u32, f64)>,
    /// Formulas the reader couldn't read, kept as their values (a reference
    /// into another workbook, a token it doesn't know).
    pub unread_formulas: usize,
    /// Defined names the reader couldn't read, left out.
    pub unread_names: usize,
    /// The ids of records the `.xls` reader neither reads nor knows to hold
    /// nothing a person sees: named as "other features".
    pub unknown_records: BTreeSet<u16>,
    pub names: Vec<DefinedName>,
    /// The external books formulas name, `[1]` first.
    pub external_links: Vec<ExternalLink>,
    pub date1904: bool,
    pub limits: Limits,
    /// What has been charged against `limits` so far.
    cells: usize,
    repeat_cells: usize,
    repeat_bytes: usize,
    styled_blanks: usize,
}

impl BookIn {
    #[cfg(test)]
    pub fn new() -> BookIn {
        BookIn::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> BookIn {
        BookIn {
            xfs: vec![Xf::default()],
            limits,
            ..BookIn::default()
        }
    }

    /// The `xfs` index of a new cell format `xf` (0 for the default).
    pub fn add_xf(&mut self, xf: Xf) -> u32 {
        if xf == Xf::default() {
            return 0;
        }
        self.xfs.push(xf);
        (self.xfs.len() - 1) as u32
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

    /// Whether `n` more styled blanks fit [`Limits::styled_blanks`], and
    /// if so, take them. Never an error: what doesn't fit is left out.
    pub fn take_styled_blanks(&mut self, n: usize) -> bool {
        match self.styled_blanks.checked_add(n) {
            Some(t) if t <= self.limits.styled_blanks => {
                self.styled_blanks = t;
                true
            }
            _ => false,
        }
    }

    /// The package: a fresh workbook with these sheets, cells, number
    /// formats, names and date system, built in time linear in its size.
    /// Formula text goes through [`crate::formula::file_formula`], so it is
    /// stored as [`load_xlsx`] would store it. Sheet names are made valid
    /// for Excel ([`valid_sheet_names`]).
    pub fn build(self) -> SheetPackage {
        self.build_counted().0
    }

    /// [`BookIn::build`], with how many formulas and names the reader read
    /// but the build couldn't keep (one naming another book's name on a
    /// renamed sheet), on top of [`BookIn::unread_formulas`] and
    /// [`BookIn::unread_names`].
    pub fn build_counted(mut self) -> (SheetPackage, usize, usize) {
        let (mut lost_formulas, mut lost_names) = (0, 0);
        if self.sheets.is_empty() {
            self.sheets.push(SheetIn {
                name: "Sheet1".to_string(),
                ..SheetIn::default()
            });
        }
        let raw: Vec<String> = self.sheets.iter().map(|s| s.name.clone()).collect();
        let names = valid_sheet_names(raw.iter().map(String::as_str));
        // The readers spell a `:` in a name as COLON (see `sheet_prefix`),
        // so the old names are looked up that way.
        let keys: Vec<String> = raw.iter().map(|n| escape_colon(n)).collect();
        let renames = sheet_renames(&keys, &names);
        // A formula naming another book (`[1]!F(...)`, `[1]P!A1`) doesn't
        // parse, so `retarget` can't move its references to a renamed
        // sheet. Rather than keep a stale one, it is dropped (`None`): the
        // cell keeps its value, as before the import read those books.
        let fix = |f: String, name: bool| -> Option<String> {
            let f = match retarget(&f, &renames, name) {
                Some(f) => f,
                None if (f.contains("]!") || names_external_book(&f))
                    && sheet_qualifiers(&f)
                        .iter()
                        .any(|q| renames.by_old.contains_key(q)) =>
                {
                    return None;
                }
                None => f,
            };
            let f = if f.contains(COLON) {
                restore_colons(&f)
            } else {
                f
            };
            Some(crate::formula::file_formula(&f).into_owned())
        };
        let mut pkg = crate::xlsx::new_xlsx_sheets(&names);
        // Each cell format becomes an xf (the readers made each one once);
        // the default stays xf 0.
        let mut xf_of = vec![0u32; self.xfs.len()];
        for (i, xf) in self.xfs.into_iter().enumerate().skip(1) {
            pkg.workbook.styles.xfs.push(xf);
            xf_of[i] = (pkg.workbook.styles.xfs.len() - 1) as u32;
        }
        for (at, mut sheet) in self.sheets.into_iter().enumerate() {
            // The comments part, its drawing and rels, written once.
            pkg.set_comments(at, &std::mem::take(&mut sheet.comments));
            // The conditional formats, in one pass over the parts; a rule
            // whose formula can't be kept is left out.
            let mut blocks = Vec::new();
            for cf in std::mem::take(&mut sheet.cond_formats) {
                let mut rules = Vec::new();
                // From the reader's base to the model's anchor, as above.
                let cf = cf.placed();
                for (mut kind, dxf) in cf.rules {
                    let mut kept = true;
                    for f in kind.formulas_mut() {
                        match fix(std::mem::take(f), false) {
                            Some(g) => *f = g,
                            None => kept = false,
                        }
                    }
                    if kept {
                        rules.push((kind, dxf));
                    }
                }
                if !rules.is_empty() && !cf.ranges.is_empty() {
                    blocks.push(crate::xlsx::NewCondFormat {
                        sqref: crate::xlsx::sqref_of(&cf.ranges),
                        ranges: cf.ranges,
                        rules,
                    });
                }
            }
            pkg.add_cond_formats(at, blocks);
            let s = &mut pkg.workbook.sheets[at];
            for (key, mut cell) in sheet.cells {
                cell.style = xf_of.get(cell.style as usize).copied().unwrap_or(0);
                if let Some(f) = cell.formula.take() {
                    cell.formula = fix(f, false);
                    lost_formulas += usize::from(cell.formula.is_none());
                }
                s.cells.insert(key, cell);
            }
            s.merges = sheet.merges;
            // Built in memory (`ix` and `orig` none), so a save appends
            // them. Their formulas move from the reader's base to the
            // model's anchor; one that can't move (it doesn't parse), or
            // whose formula can't be kept, is left out.
            for dv in sheet.validations {
                let Some(mut dv) = dv.placed() else {
                    continue;
                };
                let mut kept = true;
                for f in [&mut dv.formula1, &mut dv.formula2] {
                    if !f.is_empty() {
                        match fix(std::mem::take(f), false) {
                            Some(g) => *f = g,
                            None => kept = false,
                        }
                    }
                }
                if kept && !dv.ranges.is_empty() {
                    s.validations.push(dv);
                }
            }
            for (rect, target) in sheet.hyperlinks {
                for cell in crate::sheet::hyperlink_cells(rect) {
                    s.hyperlinks.insert(cell, target.clone());
                }
                // A range is kept as one, as an xlsx `<hyperlink>` is: saves
                // write it whole and edits move it.
                if (rect.0, rect.1) != (rect.2, rect.3) {
                    s.hyperlink_refs.insert(
                        (rect.0, rect.1),
                        crate::sheet::LinkRef {
                            loaded: rect,
                            now: rect,
                            target: target.clone(),
                        },
                    );
                }
            }
            for c in sheet.cols {
                use crate::sheet::with_xml_attr;
                let mut attrs = String::new();
                if c.hidden {
                    attrs = with_xml_attr(&attrs, "hidden", Some("1"));
                }
                if c.level > 0 {
                    attrs = with_xml_attr(&attrs, "outlineLevel", Some(&c.level.to_string()));
                }
                if c.collapsed {
                    attrs = with_xml_attr(&attrs, "collapsed", Some("1"));
                }
                s.col_defs.push(crate::sheet::ColDef {
                    min: c.first,
                    max: c.last,
                    width: c.width,
                    attrs,
                    default_width: c.width.is_none(),
                    plain_width: c.plain,
                });
            }
            for r in sheet.rows {
                if r.height.is_some() {
                    s.set_row_height(r.row, r.height);
                }
                if r.hidden {
                    s.set_row_hidden(r.row, true);
                }
                if r.level > 0 {
                    s.set_row_outline(r.row, r.level);
                }
                if r.collapsed {
                    s.set_row_collapsed(r.row, true);
                }
            }
            // `page_setup_loaded` and `outline_loaded` stay the fresh
            // part's defaults, so a save as .xlsx writes these.
            s.page_setup = sheet.page_setup;
            s.row_breaks = sheet.row_breaks;
            s.col_breaks = sheet.col_breaks;
            s.outline = sheet.outline;
            s.freeze = sheet.freeze;
            s.hidden = sheet.hidden;
            s.format.default_row_height = sheet.default_row_height;
            if let Some(b) = sheet.base_col_width {
                s.format.base_col_width = b;
            }
            s.format.default_col_width = sheet.default_col_width;
        }
        pkg.workbook.active_tab = self.active.min(pkg.workbook.sheets.len() - 1);
        for mut name in self.names {
            let Some(formula) = fix(name.formula, true) else {
                lost_names += 1;
                continue;
            };
            name.formula = formula;
            pkg.workbook.defined_names.push(name);
        }
        pkg.workbook.date1904 = self.date1904;
        pkg.workbook.iterate = self.iterate;
        write_external_links(&mut pkg, &self.external_links);
        (pkg, lost_formulas, lost_names)
    }
}

/// An external book's cached cells as `<sheetDataSet>` (empty for none),
/// those on its `sheets` sheets only:
/// one `<sheetData>` per sheet with cells, rows and cells in order, as
/// Excel writes them (corpus/legacy/addin/ext-name.xlsx). A number has no
/// `t`; text is `str`, a boolean `b` and an error `e`.
fn sheet_data_set(cache: &BTreeMap<(u32, u32, u32), ExtValue>, sheets: usize) -> String {
    use crate::xlsx::{esc_text, num_repr};
    // A sheet the book doesn't list, or a cell off the grid, is left out.
    let mut cells = cache
        .iter()
        .filter(|&(&(s, r, c), _)| (s as usize) < sheets && on_grid(r, c))
        .peekable();
    if cells.peek().is_none() {
        return String::new();
    }
    let mut out = String::from("<sheetDataSet>");
    let mut at: Option<(u32, u32)> = None;
    for (&(sheet, row, col), value) in cells {
        if at.map(|a| a.0) != Some(sheet) {
            if at.is_some() {
                out.push_str("</row></sheetData>");
            }
            out.push_str(&format!(
                r#"<sheetData sheetId="{sheet}"><row r="{}">"#,
                row + 1
            ));
        } else if at.map(|a| a.1) != Some(row) {
            out.push_str(&format!(r#"</row><row r="{}">"#, row + 1));
        }
        at = Some((sheet, row));
        let (t, v) = match value {
            ExtValue::Num(n) => ("", num_repr(*n)),
            ExtValue::Str(s) => (r#" t="str""#, esc_text(s)),
            ExtValue::Bool(b) => (r#" t="b""#, u8::from(*b).to_string()),
            ExtValue::Err(e) => (r#" t="e""#, esc_text(e)),
        };
        out.push_str(&format!(
            r#"<cell r="{}{}"{t}><v>{v}</v></cell>"#,
            crate::sheet::col_name(col),
            row + 1
        ));
    }
    out.push_str("</row></sheetData></sheetDataSet>");
    out
}

/// Each external link as `xl/externalLinks/externalLink{k}.xml` (with its
/// rels, content type and workbook rel), and `<externalReferences>` naming
/// them in order, so `[k]!NAME` names link `k`. `pkg` is the fresh package
/// [`BookIn::build`] makes, whose workbook.xml is `<sheets>` only: its
/// `<externalReferences>` goes right after `</sheets>`, its schema slot,
/// and a save puts `<definedNames>` after it. The shared parts are each
/// edited once, so the time is linear in the links.
fn write_external_links(pkg: &mut SheetPackage, links: &[ExternalLink]) {
    use crate::xlsx::{esc_attr, parse_rels};
    const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const WB_RELS: &str = "xl/_rels/workbook.xml.rels";
    if links.is_empty() {
        return;
    }
    // Each link's workbook rel is numbered past the rIds already there.
    let first = pkg.part(WB_RELS).map_or(0, |b| {
        parse_rels(&String::from_utf8_lossy(b))
            .iter()
            .filter_map(|(id, ..)| id.strip_prefix("rId")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
    });
    let (mut refs, mut wb_rels, mut overrides) = (String::new(), String::new(), String::new());
    for (i, link) in links.iter().enumerate() {
        let part = format!("xl/externalLinks/externalLink{}.xml", i + 1);
        let mut body = String::new();
        if !link.sheets.is_empty() {
            body.push_str("<sheetNames>");
            for s in &link.sheets {
                body.push_str(&format!(r#"<sheetName val="{}"/>"#, esc_attr(s)));
            }
            body.push_str("</sheetNames>");
        }
        // Each name once in its scope (a repeat would make the part
        // invalid), with its definition when the reader read one.
        let mut seen = std::collections::HashSet::new();
        let names: Vec<&ExtName> = link
            .names
            .iter()
            .flatten()
            .filter(|n| seen.insert((n.name.to_lowercase(), n.sheet)))
            .collect();
        if !names.is_empty() {
            body.push_str("<definedNames>");
            for n in names {
                body.push_str(&format!(r#"<definedName name="{}""#, esc_attr(&n.name)));
                if let Some(f) = &n.refers_to {
                    body.push_str(&format!(r#" refersTo="{}""#, esc_attr(f)));
                }
                if let Some(s) = n.sheet {
                    body.push_str(&format!(r#" sheetId="{s}""#));
                }
                body.push_str("/>");
            }
            body.push_str("</definedNames>");
        }
        body.push_str(&sheet_data_set(&link.cache, link.sheets.len()));
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<externalLink xmlns="{MAIN}"><externalBook xmlns:r="{RELS}" r:id="{}">{body}</externalBook></externalLink>"#,
            esc_attr(&link.book)
        );
        let mut rels = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        for (id, ty, target, mode) in &link.rels {
            rels.push_str(&format!(
                r#"<Relationship Id="{}" Type="{}" Target="{}""#,
                esc_attr(id),
                esc_attr(ty),
                esc_attr(target)
            ));
            if let Some(mode) = mode {
                rels.push_str(&format!(r#" TargetMode="{}""#, esc_attr(mode)));
            }
            rels.push_str("/>");
        }
        rels.push_str("</Relationships>");
        overrides.push_str(&format!(
            r#"<Override PartName="/{part}" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml"/>"#
        ));
        let rid = format!("rId{}", first as usize + i + 1);
        wb_rels.push_str(&format!(
            r#"<Relationship Id="{rid}" Type="{RELS}/externalLink" Target="externalLinks/externalLink{}.xml"/>"#,
            i + 1
        ));
        refs.push_str(&format!(r#"<externalReference r:id="{rid}"/>"#));
        pkg.parts.push((
            format!("xl/externalLinks/_rels/externalLink{}.xml.rels", i + 1),
            rels.into_bytes(),
        ));
        pkg.parts.push((part, xml.into_bytes()));
    }
    let mut insert = |name: &str, before: &str, text: &str| {
        if let Some((_, b)) = pkg.parts.iter_mut().find(|(n, _)| n == name) {
            *b = String::from_utf8_lossy(b)
                .replacen(before, &format!("{text}{before}"), 1)
                .into_bytes();
        }
    };
    insert("[Content_Types].xml", "</Types>", &overrides);
    insert(WB_RELS, "</Relationships>", &wb_rels);
    insert(
        "xl/workbook.xml",
        "</workbook>",
        &format!("<externalReferences>{refs}</externalReferences>"),
    );
}

/// `raw` (a file's stem, say) as a name Excel takes for a workbook's only
/// sheet: [`valid_sheet_names`] on it alone, so `[]:*?/\` become `_`, it is
/// cut to 31 characters, apostrophes are trimmed off its ends, and an empty
/// result is `Sheet1` (#2153).
pub fn clean_sheet_name(raw: &str) -> String {
    valid_sheet_names(std::iter::once(raw)).remove(0)
}

/// Sheet names Excel accepts, in order: at most 31 characters, none of
/// `[]:*?/\` nor a control character (`_` for each), not empty (`SheetN` instead), and unique ignoring case (a
/// repeat becomes `Name (2)`). References to a renamed sheet follow it
/// ([`sheet_renames`], [`retarget`]).
pub(crate) fn valid_sheet_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    use crate::sheet::SHEET_NAME_MAX;
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
            .map(|c| {
                if crate::sheet::SHEET_NAME_FORBIDDEN.contains(c)
                    || c == COLON
                    || crate::sheet::sheet_name_control(c)
                {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        // Cut, then trim: a cut may end on an apostrophe Excel refuses there.
        let mut name = cut(&clean, SHEET_NAME_MAX).trim_matches('\'').to_string();
        if name.trim().is_empty() {
            name = format!("Sheet{}", i + 1);
        }
        let base = name.clone();
        let n = next.entry(base.to_lowercase()).or_insert(2);
        while taken.contains(&name.to_lowercase()) {
            let suffix = format!(" ({n})");
            name = format!(
                "{}{suffix}",
                cut(&base, SHEET_NAME_MAX - suffix.chars().count())
            );
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

/// Stands for a `:` inside a sheet name in the formula text a reader
/// builds. A raw name such as `A:B` (a crafted file's; Excel and
/// LibreOffice never write one) would otherwise read as the span A..B, and
/// `'A:B'!A:A` would not parse at all. A Unicode noncharacter, so no sheet
/// name means it: [`valid_sheet_names`] replaces it, and
/// [`BookIn::build`] renames every marked name to its valid one, or puts
/// the `:` back ([`restore_colons`]) for a sheet the workbook doesn't have.
const COLON: char = '\u{FDD0}';

/// `name` with each `:` spelled as [`COLON`].
fn escape_colon(name: &str) -> String {
    name.replace(':', &COLON.to_string())
}

/// A reference's sheet part as a formula spells it: `Data!`, `'My Sheet'!`,
/// or for a 3D span `'Q1:Q3'!` / `Jan:Mar!`. Empty `first` means no sheet.
/// A `:` inside a name is spelled [`COLON`], which is always quoted.
pub(crate) fn sheet_prefix(first: &str, last: &str) -> String {
    if first.is_empty() {
        return String::new();
    }
    let (first, last) = (escape_colon(first), escape_colon(last));
    if first == last {
        return format!("{}!", crate::sheet::quote_sheet_name(&first));
    }
    crate::formula::span_prefix(&first, &last)
}

/// The sheet part of a reference into sheet `sheet` of external book `k`
/// (1-based), as Excel's `.xlsx` spells it: `[1]P!`, or `'[1]My Sheet'!`
/// when the name needs quotes, which then hold the book number too. The
/// quoting rule is our own sheets' ([`crate::sheet::quote_sheet_name`]).
/// `None` for an empty name.
pub(crate) fn external_prefix(k: usize, sheet: &str) -> Option<String> {
    if sheet.is_empty() {
        return None;
    }
    Some(if crate::sheet::quote_sheet_name(sheet) == sheet {
        format!("[{k}]{sheet}!")
    } else {
        format!("'[{k}]{}'!", sheet.replace('\'', "''"))
    })
}

/// The `refersTo` of an external book's name from its definition `rgce`,
/// as Excel's `.xlsx` spells it; `None` for any other shape.
///
/// - ptgErr (2 bytes): the error, `#REF!` (no `=`).
/// - An area (ptgArea3d, any class; 13 bytes) or a cell (ptgRef3d; 9
///   bytes): `='P'!$A$1:$A$2` / `='P'!$A$1`, the sheet always quoted. The
///   token is neither BIFF8's nor BIFF12's ptgArea3d: it is the ptg,
///   itabFirst u16 and itabLast u16 (the book's sheets), then BIFF8's
///   RgceArea (rows u16, columns u16 with the relative flags in bits 14
///   and 15) or RgceLoc, the same in `.xls` and `.xlsb`. Excel wrote
///   `Prices` (`'P'!$A$1:$A$2`) as `3b 0000 0000 0000 0100 0000 0000` in
///   both files of corpus/legacy/addin/ext-name. One spanning sheets, on a
///   sheet the book doesn't list, off the grid or with a relative part
///   (no file shows its spelling) gives `None`.
pub(crate) fn ext_definition(rgce: &[u8], sheets: &[String]) -> Option<String> {
    let mut rd = Le::new(rgce);
    let ptg = rd.u8()?;
    if ptg == 0x1C && rgce.len() == 2 {
        return Some(biff_error(rd.u8()?).to_string());
    }
    let area = match (ptg & 0x9F | 0x20, rgce.len()) {
        (0x3B, 13) if ptg & 0x60 != 0 => true,
        (0x3A, 9) if ptg & 0x60 != 0 => false,
        _ => return None,
    };
    let (first, last) = (rd.u16()?, rd.u16()?);
    let sheet = sheets.get(first as usize).filter(|_| first == last)?;
    let (r1, r2) = if area {
        (rd.u16()?, rd.u16()?)
    } else {
        let r = rd.u16()?;
        (r, r)
    };
    let (c1, c2) = if area {
        (rd.u16()?, rd.u16()?)
    } else {
        let c = rd.u16()?;
        (c, c)
    };
    if (c1 | c2) & 0xC000 != 0 {
        return None;
    }
    let (c1, c2) = (u32::from(c1 & 0x3FFF), u32::from(c2 & 0x3FFF));
    let (r1, r2) = (u32::from(r1), u32::from(r2));
    if !on_grid(r1, c1) || !on_grid(r2, c2) {
        return None;
    }
    let cell = |r: u32, c: u32| format!("${}${}", crate::sheet::col_name(c), r + 1);
    let at = format!("='{}'!{}", sheet.replace('\'', "''"), cell(r1, c1));
    Some(if area {
        format!("{at}:{}", cell(r2, c2))
    } else {
        at
    })
}

/// Whether `src` holds an external book's qualifier, `[k]` (`[1]!F`,
/// `[1]P!A1`), outside string literals and quoted names (skipped as
/// [`sheet_qualifiers`] skips them). Such a formula doesn't parse, so
/// [`BookIn::build`] can't follow a sheet rename through it.
fn names_external_book(src: &str) -> bool {
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'[' => {
                let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits > 0 && b.get(i + 1 + digits) == Some(&b']') {
                    return true;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    false
}

/// `src` with each [`COLON`] left in a sheet qualifier turned back into
/// `:`: one naming a sheet the workbook doesn't have (an ODS `['A:B'.A1]`
/// with no table `A:B`), which no rename removed. Its text is then as the
/// reader would have spelled it without the marker. The qualifier may be
/// quoted (as the readers spell it) or bare (the printer leaves a
/// non-ASCII name bare, and `retarget` reprints a formula that names a
/// renamed sheet too); a bare one is quoted, since a `:` needs it. String
/// literals and structured-reference specs are left alone.
fn restore_colons(src: &str) -> String {
    use crate::formula::{is_quote_open, skip_quoted};
    // What the printer leaves bare in a name (ASCII letters, digits, `_`,
    // anything past ASCII), and `.`.
    let bare = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.') || !c.is_ascii();
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        // A literal, a quoted name or a structured-reference spec, read as
        // the lexer reads it; only a quoted name can be a qualifier.
        if is_quote_open(b[i]) {
            let end = skip_quoted(b, i);
            if b[i] == b'\'' {
                out.push_str(&src[i..end].replace(COLON, ":"));
            } else {
                out.push_str(&src[i..end]);
            }
            i = end;
            continue;
        }
        let start = i;
        for c in src[i..].chars().take_while(|&c| bare(c)) {
            i += c.len_utf8();
        }
        if i == start {
            // Not a name: an operator, a paren, `!`.
            let c = src[i..].chars().next().unwrap_or_default();
            out.push(c);
            i += c.len_utf8().max(1);
            continue;
        }
        // A bare name: a qualifier when `!` follows.
        let name = &src[start..i];
        if name.contains(COLON) && b.get(i) == Some(&b'!') {
            out.push('\'');
            out.push_str(&name.replace(COLON, ":"));
            out.push('\'');
        } else {
            out.push_str(name);
        }
    }
    out
}

/// A little-endian cursor over a byte slice, for the BIFF8 and BIFF12
/// readers. Each format's own strings and layouts are methods in its file
/// (`xls::Cur` wraps one with its CONTINUE breaks).
pub(crate) struct Le<'a> {
    pub(crate) d: &'a [u8],
    pub(crate) at: usize,
}

impl<'a> Le<'a> {
    pub(crate) fn new(d: &'a [u8]) -> Le<'a> {
        Le { d, at: 0 }
    }
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    pub(crate) fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub(crate) fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    /// Everything left.
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let s = self.d.get(self.at..).unwrap_or(&[]);
        self.at = self.d.len();
        s
    }
}

/// The number format code of format id `ifmt` (BIFF8 and BIFF12 alike):
/// the file's own code for it, else the built-in one; `None` for General.
pub(crate) fn format_code(codes: &HashMap<u16, String>, ifmt: u16) -> Option<String> {
    codes
        .get(&ifmt)
        .cloned()
        .or_else(|| builtin_format(ifmt as u32))
        .filter(|c| !c.eq_ignore_ascii_case("General"))
}

/// The `cellIs` operator of a conditional format's CFOper (BIFF8 CF's
/// `cp`, BIFF12 BrtBeginCFRule's `iParam`): 1 between ... 8 less than or
/// equal; `None` for another.
pub(crate) fn cf_operator(op: u32) -> Option<&'static str> {
    Some(match op {
        1 => "between",
        2 => "notBetween",
        3 => "equal",
        4 => "notEqual",
        5 => "greaterThan",
        6 => "lessThan",
        7 => "greaterThanOrEqual",
        8 => "lessThanOrEqual",
        _ => return None,
    })
}

/// The rule kind of a data validation's `valType` (BIFF8 DV and BIFF12
/// BrtDVal alike): `""` is Any value.
pub(crate) fn dv_kind(val_type: u32) -> &'static str {
    match val_type {
        1 => "whole",
        2 => "decimal",
        3 => "list",
        4 => "date",
        5 => "time",
        6 => "textLength",
        7 => "custom",
        _ => "",
    }
}

/// The operator of a data validation's `typOperator`, for the kinds that
/// compare (empty for the others).
pub(crate) fn dv_operator(kind: &str, op: u32) -> String {
    if !crate::validation::takes_operator(kind) {
        return String::new();
    }
    match op {
        1 => "notBetween",
        2 => "equal",
        3 => "notEqual",
        4 => "greaterThan",
        5 => "lessThan",
        6 => "greaterThanOrEqual",
        7 => "lessThanOrEqual",
        _ => "between",
    }
    .to_string()
}

/// A data validation's flags word (BIFF8 DV's dwDvFlags, BIFF12 BrtDVal's
/// flags; [MS-XLS] 2.4.123, [MS-XLSB] 2.4 BrtDVal): valType (bits 0-3),
/// errStyle (4-6), fAllowBlank (8), fSuppressCombo (9), fShowInputMsg
/// (18), fShowErrorMsg (19), typOperator (20-23). The rule they make,
/// without its ranges, texts and formulas.
pub(crate) fn dv_from_flags(flags: u32) -> crate::sheet::DataValidation {
    use crate::sheet::AlertStyle;
    let kind = dv_kind(flags & 0x0F);
    crate::sheet::DataValidation {
        kind: kind.to_string(),
        operator: dv_operator(kind, (flags >> 20) & 0x0F),
        error_style: match (flags >> 4) & 0x07 {
            1 => AlertStyle::Warning,
            2 => AlertStyle::Information,
            _ => AlertStyle::Stop,
        },
        allow_blank: flags & (1 << 8) != 0,
        show_dropdown: flags & (1 << 9) == 0,
        show_input: flags & (1 << 18) != 0,
        show_error: flags & (1 << 19) != 0,
        ..crate::sheet::DataValidation::default()
    }
}

/// A list rule's formula as decompiled: an explicit list is one string of
/// items split by NULs (`"a\0b"`), which an `.xlsx` spells `"a,b"`.
pub(crate) fn dv_list_formula(f: String) -> String {
    if f.contains('\0') {
        f.replace('\0', ",")
    } else {
        f
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

/// BIFF8's built-in defined names (`_xlnm.` and these), by their code
/// (shared by the `.xls` reader and writer).
pub(crate) const BUILTIN_NAMES: [&str; 14] = [
    "Consolidate_Area",
    "Auto_Open",
    "Auto_Close",
    "Extract",
    "Database",
    "Criteria",
    "Print_Area",
    "Print_Titles",
    "Recorder",
    "Data_Form",
    "Auto_Activate",
    "Auto_Deactivate",
    "Sheet_Title",
    "_FilterDatabase",
];

/// BIFF's error codes and their text, both ways (the readers and the
/// `.xls` writer).
pub(crate) const BIFF_ERRORS: [(u8, &str); 8] = [
    (0x00, "#NULL!"),
    (0x07, "#DIV/0!"),
    (0x0F, "#VALUE!"),
    (0x17, "#REF!"),
    (0x1D, "#NAME?"),
    (0x24, "#NUM!"),
    (0x2A, "#N/A"),
    (0x2B, "#GETTING_DATA"),
];

/// The error text of a BIFF error code (`#N/A` for an unknown one).
pub(crate) fn biff_error(code: u8) -> &'static str {
    BIFF_ERRORS
        .iter()
        .find(|e| e.0 == code)
        .map_or("#N/A", |e| e.1)
}

/// The built-in number formats BIFF files leave implicit (no FORMAT record
/// for ids below 164).
pub(crate) fn builtin_format(id: u32) -> Option<String> {
    crate::numfmt::builtin_code(id).map(str::to_string)
}

/// Whether cell format `style` of `pkg` is a thin box: the model's
/// `border`, or a loaded `<xf>` whose `<border>` is one (the `.xlsx` loader
/// keeps the border element through [`Xf::loaded_from`] and never sets
/// `border`). For the round-trip tests of the imports (#884).
#[cfg(test)]
pub(crate) fn saved_thin_box(pkg: &SheetPackage, style: u32) -> bool {
    let xf = pkg.workbook.styles.xf(style);
    let extras = pkg
        .part(pkg.styles_part_name())
        .map(|b| crate::xlsx::loaded_xf_extras(&String::from_utf8_lossy(b)))
        .unwrap_or_default();
    xf.border
        || xf
            .loaded_from
            .and_then(|k| extras.get(k as usize))
            .is_some_and(|e| e.thin_box)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #851: a `mimetype` entry declaring more than [`MAX_MIMETYPE`] is not
    /// read for the ODS sniff (ahead of `load_xlsx`'s budget), even when its
    /// text would match: the workbook opens as the `.xlsx` it is.
    #[test]
    fn a_long_mimetype_entry_is_not_sniffed() {
        let book = crate::xlsx::save_xlsx(&crate::xlsx::new_xlsx());
        let zip = opccore::zip::ZipArchive::open(&book).unwrap();
        let mime = format!("{ODS_MIMETYPE}{}", " ".repeat(MAX_MIMETYPE as usize));
        let mut parts = vec![("mimetype".to_string(), mime.into_bytes())];
        parts.extend(
            zip.entries()
                .iter()
                .map(|e| (e.name.clone(), zip.extract(e).unwrap())),
        );
        let opened = open_workbook_ex(&opccore::zipwrite::write_zip(&parts)).unwrap();
        assert_eq!(opened.format, SourceFormat::Xlsx);
    }

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

    /// #2115: a name cut to 31 characters ending on an apostrophe loses it,
    /// so every cleaned name is one a rename would take.
    #[test]
    fn cleaned_sheet_names_pass_the_rename_rule() {
        let cut_on_quote = format!("{}'b", "a".repeat(30));
        let long = "y".repeat(40);
        let quoted_long = format!("'{}'", "z".repeat(35));
        let raw = [
            cut_on_quote.as_str(),
            "'x'",
            long.as_str(),
            long.as_str(),
            quoted_long.as_str(),
            "''",
            "a:b",
            "A:B",
        ];
        let got = valid_sheet_names(raw.into_iter());
        assert_eq!(got[0], "a".repeat(30));
        assert_eq!(got[1], "x");
        assert_eq!(got[3], format!("{} (2)", "y".repeat(27)));
        assert_eq!(got[5], "Sheet6");
        for n in &got {
            assert_eq!(crate::sheet::sheet_name_error(n), None, "{n:?}");
        }
    }

    /// #2153: a file's stem as a lone sheet's name is cleaned as an import
    /// cleans names, so Excel takes it.
    #[test]
    fn clean_sheet_name_gives_a_name_excel_takes() {
        assert_eq!(clean_sheet_name("a[1]'"), "a_1_");
        assert_eq!(clean_sheet_name("'q'"), "q");
        assert_eq!(clean_sheet_name("x:y*?"), "x_y__");
        assert_eq!(clean_sheet_name(""), "Sheet1");
        assert_eq!(clean_sheet_name("''"), "Sheet1");
        assert_eq!(clean_sheet_name("Report"), "Report");
        assert_eq!(clean_sheet_name("a\u{1}b"), "a_b");
        assert_eq!(clean_sheet_name("\t"), "_");
        let long = "n".repeat(40);
        assert_eq!(clean_sheet_name(&long), "n".repeat(31));
        for raw in [
            "a[1]'",
            "'q'",
            "x:y*?",
            "",
            &long,
            "/\\",
            "a\u{1}b",
            "\t",
            "x\u{ffff}",
        ] {
            let name = clean_sheet_name(raw);
            assert_eq!(crate::sheet::sheet_name_error(&name), None, "{raw:?}");
        }
    }

    /// Each external link becomes a part, numbered in order and named from
    /// `<externalReferences>` right after `<sheets>`; a name repeated in
    /// another case is written once, and attribute text is escaped. A book
    /// with no links gets none of it.
    #[test]
    fn external_links_are_written_in_order() {
        let text = |pkg: &SheetPackage, name: &str| {
            String::from_utf8(pkg.part(name).unwrap_or_default().to_vec()).unwrap()
        };
        let link = |names: &[&str], target: &str| {
            ExternalLink {
            book: "rId1".into(),
            rels: vec![(
                "rId1".into(),
                "http://schemas.microsoft.com/office/2006/relationships/xlExternalLinkPath/xlLibrary".into(),
                target.into(),
                Some("External".into()),
            )],
            sheets: vec!["S&1".into()],
            names: names.iter().map(|&n| Some(n.into())).collect(),
            cache: BTreeMap::new(),
        }
        };
        let mut book = BookIn::new();
        book.external_links = vec![link(&["F"], "A.XLAM"), link(&["X", "G", "x"], "B&C.XLAM")];
        let pkg = book.build();
        let two = text(&pkg, "xl/externalLinks/externalLink2.xml");
        assert!(
            two.ends_with(r#" r:id="rId1"><sheetNames><sheetName val="S&amp;1"/></sheetNames><definedNames><definedName name="X"/><definedName name="G"/></definedNames></externalBook></externalLink>"#),
            "{two}"
        );
        assert!(
            text(&pkg, "xl/externalLinks/_rels/externalLink2.xml.rels")
                .contains(r#"Target="B&amp;C.XLAM" TargetMode="External"/>"#)
        );
        let rels = text(&pkg, "xl/_rels/workbook.xml.rels");
        let rid = |k: usize| {
            let at = rels
                .find(&format!(r#"Target="externalLinks/externalLink{k}.xml""#))
                .unwrap();
            let id = rels[..at].rfind("Id=\"").unwrap() + 4;
            rels[id..].split('"').next().unwrap().to_string()
        };
        assert!(text(&pkg, "xl/workbook.xml").contains(&format!(
            r#"</sheets><externalReferences><externalReference r:id="{}"/><externalReference r:id="{}"/></externalReferences></workbook>"#,
            rid(1),
            rid(2)
        )));
        assert_eq!(
            text(&pkg, "[Content_Types].xml")
                .matches("spreadsheetml.externalLink+xml")
                .count(),
            2
        );

        let pkg = BookIn::new().build();
        assert!(!pkg.part_names().iter().any(|n| n.contains("externalLink")));
        assert!(!text(&pkg, "xl/workbook.xml").contains("externalReferences"));
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
            ..SheetIn::default()
        };
        let num = |v: f64| Cell {
            value: CellValue::Number(v),
            ..Cell::default()
        };
        let f = |text: &str| Cell {
            formula: Some(text.to_string()),
            ..Cell::default()
        };
        // As the readers spell the sheet raw-named `A:B`.
        let colon = sheet_prefix("A:B", "A:B");
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
                    ((4, 0), f(&format!("{colon}A1"))),
                    ((5, 0), f(&format!("SUM({colon}A1:A2)"))),
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
            formula: format!("{colon}$A$2"),
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

    /// A call into another book (`[1]!F(...)`) or a reference to one of its
    /// sheets (`[1]P!A1`, #903) doesn't parse, so a sheet rename can't
    /// reach inside it: one naming a renamed sheet is dropped (a cell keeps
    /// its value, a name is left out), and one that doesn't stays as it is.
    /// A quoted one (`'[1]My Sheet'!A1`) parses, so the rename follows.
    #[test]
    fn a_call_into_another_book_naming_a_renamed_sheet_is_dropped() {
        use crate::sheet::CellValue;
        let long = "L".repeat(33);
        let cut = "L".repeat(31);
        let cell = |f: &str, v: f64| Cell {
            value: CellValue::Number(v),
            formula: Some(f.to_string()),
            ..Cell::default()
        };
        let mut book = BookIn::new();
        book.push_sheet(SheetIn {
            name: long.clone(),
            ..SheetIn::default()
        })
        .unwrap();
        book.push_sheet(SheetIn {
            name: "Calc".into(),
            cells: [
                ((0, 0), cell(&format!("[1]!F('{long}'!A1)"), 9.0)),
                ((1, 0), cell("[1]!F(Calc!A1)", 8.0)),
                ((2, 0), cell(&format!("'{long}'!A1*2"), 7.0)),
                ((3, 0), cell(&format!("[1]P!A1+'{long}'!A1"), 6.0)),
                ((4, 0), cell("[1]P!A1+Calc!A1", 5.0)),
                ((5, 0), cell(&format!("'[1]My Sheet'!A1+'{long}'!A1"), 4.0)),
            ]
            .into_iter()
            .collect(),
            ..SheetIn::default()
        })
        .unwrap();
        for (name, formula) in [
            ("Stale", format!("[1]!F('{long}'!$A$1)")),
            ("Kept", "[1]!F(Calc!$A$1)".to_string()),
        ] {
            book.names.push(DefinedName {
                name: name.into(),
                scope: None,
                formula,
            });
        }
        let pkg = book.build();
        let calc = &pkg.workbook.sheets[1];
        let at = |r| calc.cell(r, 0).unwrap();
        assert_eq!(at(0).formula, None);
        assert_eq!(at(0).value, CellValue::Number(9.0));
        assert_eq!(at(1).formula.as_deref(), Some("[1]!F(Calc!A1)"));
        assert_eq!(
            at(2).formula.as_deref(),
            Some(format!("{cut}!A1*2").as_str())
        );
        assert_eq!(at(3).formula, None);
        assert_eq!(at(3).value, CellValue::Number(6.0));
        assert_eq!(at(4).formula.as_deref(), Some("[1]P!A1+Calc!A1"));
        assert_eq!(
            at(5).formula.as_deref(),
            Some(format!("'[1]My Sheet'!A1+{cut}!A1").as_str())
        );
        let names: Vec<(&str, &str)> = pkg
            .workbook
            .defined_names
            .iter()
            .map(|n| (n.name.as_str(), n.formula.as_str()))
            .collect();
        assert_eq!(names, [("Kept", "[1]!F(Calc!$A$1)")]);
    }

    /// A sheet raw-named `A:B` keeps its whole-column and whole-row
    /// references, which `'A:B'!A:A` (read as the span A..B) lost, and a
    /// span starting at it. No marker reaches the workbook: one to a sheet
    /// it doesn't have turns back into `:`, and a raw name holding the
    /// marker is renamed itself (#876).
    #[test]
    fn sheet_named_with_colon_keeps_whole_column_and_row_refs() {
        use crate::sheet::CellValue;
        let num = |v: f64| Cell {
            value: CellValue::Number(v),
            ..Cell::default()
        };
        let f = |text: String| Cell {
            formula: Some(text),
            ..Cell::default()
        };
        let p = sheet_prefix("A:B", "A:B");
        let span = sheet_prefix("A:B", "C");
        let missing = sheet_prefix("X:Y", "X:Y");
        let marked = format!("M{COLON}N");
        let mut book = BookIn::new();
        for (name, cells) in [
            (
                "A:B",
                vec![((0, 0), num(1.0)), ((1, 0), num(2.0)), ((2, 1), num(4.0))],
            ),
            ("C", vec![((0, 0), num(10.0))]),
            (marked.as_str(), vec![((0, 0), num(100.0))]),
            (
                "Calc",
                vec![
                    ((0, 0), f(format!("SUM({p}A:A)"))),
                    ((1, 0), f(format!("SUM({p}$1:3)"))),
                    ((2, 0), f(format!("{p}A1"))),
                    ((3, 0), f(format!("SUM({span}A1)"))),
                    ((4, 0), f(format!("{missing}A1&\"{COLON}\""))),
                    ((5, 0), f(format!("{}A1", sheet_prefix(&marked, &marked)))),
                    // A renamed sheet and a missing one in one formula: the
                    // rename reprints it, leaving the missing name bare.
                    ((6, 0), f(format!("{p}A1+{missing}A1"))),
                    ((7, 0), f(format!("SUM({}A1)", sheet_prefix("A:B", "X:Y")))),
                ],
            ),
        ] {
            book.push_sheet(SheetIn {
                name: name.to_string(),
                cells: cells.into_iter().collect(),
                ..SheetIn::default()
            })
            .unwrap();
        }
        book.names.push(DefinedName {
            name: "Cols".into(),
            scope: None,
            formula: format!("{p}$A:$B"),
        });
        book.names.push(DefinedName {
            name: "Both".into(),
            scope: None,
            formula: format!("{p}$A$1,{missing}$A$1"),
        });
        let mut pkg = book.build();
        let wb = &mut pkg.workbook;
        let names: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A_B", "C", "M_N", "Calc"]);
        let mut engine = crate::engine::Engine::new(wb);
        engine.recalc_all(wb);
        let calc = &wb.sheets[3];
        let got = |r: u32| {
            let c = calc.cell(r, 0).unwrap();
            (c.formula.clone().unwrap(), c.value.clone())
        };
        assert_eq!(got(0), ("SUM(A_B!A:A)".into(), CellValue::Number(3.0)));
        assert_eq!(got(1), ("SUM(A_B!$1:3)".into(), CellValue::Number(7.0)));
        assert_eq!(got(2), ("A_B!A1".into(), CellValue::Number(1.0)));
        assert_eq!(got(3), ("SUM(A_B:C!A1)".into(), CellValue::Number(11.0)));
        // A string literal holding the marker is the file's own text.
        assert_eq!(got(4).0, format!("'X:Y'!A1&\"{COLON}\""));
        assert_eq!(got(5), ("M_N!A1".into(), CellValue::Number(100.0)));
        assert_eq!(got(6).0, "A_B!A1+'X:Y'!A1");
        assert_eq!(got(7).0, "SUM('A_B:X:Y'!A1)");
        assert_eq!(wb.defined_names[0].formula, "A_B!$A:$B");
        assert_eq!(wb.defined_names[1].formula, "A_B!$A$1,'X:Y'!$A$1");
        for r in 0..8 {
            let text = calc.cell(r, 0).unwrap().formula.clone().unwrap();
            let outside: String = text.split('"').step_by(2).collect();
            assert!(!outside.contains(COLON), "{text}");
        }
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

    /// As many links as the budget allows build in linear time, each with
    /// its own workbook rel, and reload.
    #[test]
    fn many_links_build_in_linear_time() {
        let n = Limits::default().links;
        let mut book = BookIn::new();
        book.external_links = (0..n)
            .map(|i| ExternalLink {
                book: "rId1".into(),
                rels: vec![("rId1".into(), "t".into(), format!("B{i}.XLAM"), None)],
                sheets: Vec::new(),
                names: vec![Some(ExtName::function(format!("F{i}")))],
                cache: BTreeMap::new(),
            })
            .collect();
        let started = std::time::Instant::now();
        let pkg = book.build();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let workbook = String::from_utf8(pkg.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
        let rels = crate::xlsx::parse_rels(&String::from_utf8_lossy(
            pkg.part("xl/_rels/workbook.xml.rels").unwrap(),
        ));
        let ids: std::collections::HashSet<_> = rels.iter().map(|r| r.0.clone()).collect();
        assert_eq!(ids.len(), rels.len());
        assert_eq!(workbook.matches("<externalReference ").count(), n);
        let started = std::time::Instant::now();
        crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&pkg)).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
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
            links: 1,
            styled_blanks: 5,
            column_walk: 0,
        });
        // Styled blanks fit or are left out, never refused.
        assert!(book.take_styled_blanks(5));
        assert!(!book.take_styled_blanks(1));
        assert!(!book.take_styled_blanks(usize::MAX));
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

    /// A spec's `'` escapes its next byte; it opens no quoted name, so what
    /// follows the spec is still read (#876 r3).
    #[test]
    fn restore_colons_reads_structured_references_as_the_lexer_does() {
        assert_eq!(
            restore_colons("T[Item '#]+'X\u{FDD0}Y'!A1+X\u{FDD0}Y!B1"),
            "T[Item '#]+'X:Y'!A1+'X:Y'!B1"
        );
        assert_eq!(
            restore_colons("T[[#This Row],[a'[\u{FDD0}]]&'X\u{FDD0}Y'!A1"),
            "T[[#This Row],[a'[\u{FDD0}]]&'X:Y'!A1"
        );
    }

    /// Another book's sheet is spelled as Excel's `.xlsx` spells it: bare
    /// when its name needs no quotes, else the book inside the quotes.
    #[test]
    fn external_prefixes_quote_as_needed() {
        assert_eq!(external_prefix(1, "P").as_deref(), Some("[1]P!"));
        assert_eq!(
            external_prefix(12, "Sheet.1").as_deref(),
            Some("[12]Sheet.1!")
        );
        assert_eq!(
            external_prefix(1, "My Sheet").as_deref(),
            Some("'[1]My Sheet'!")
        );
        assert_eq!(external_prefix(2, "It's").as_deref(), Some("'[2]It''s'!"));
        assert_eq!(external_prefix(1, "A1").as_deref(), Some("'[1]A1'!"));
        assert_eq!(external_prefix(1, "2024").as_deref(), Some("'[1]2024'!"));
        assert_eq!(external_prefix(1, ""), None);
    }

    /// An external name's definition: an error, an area or a cell of one
    /// of the book's sheets, as Excel's `.xlsx` spells them (#903). Any
    /// other shape gives none.
    #[test]
    fn external_definitions_take_only_the_shapes_excel_wrote() {
        let sheets = ["P".to_string(), "It's".to_string()];
        let def = |b: &[u8]| ext_definition(b, &sheets);
        let area = |ptg: u8, first: u16, last: u16, cols: (u16, u16)| {
            let mut v = vec![ptg];
            for x in [first, last, 0, 1, cols.0, cols.1] {
                v.extend(x.to_le_bytes());
            }
            v
        };
        let cell = |ptg: u8, first: u16, col: u16| {
            let mut v = vec![ptg];
            for x in [first, first, 4, col] {
                v.extend(x.to_le_bytes());
            }
            v
        };
        // Excel's, in corpus/legacy/addin/ext-name.
        assert_eq!(
            def(&[0x3B, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]).as_deref(),
            Some("='P'!$A$1:$A$2")
        );
        assert_eq!(def(&[0x1C, 0x17]).as_deref(), Some("#REF!"));
        assert_eq!(def(&[0x1C, 0x2A]).as_deref(), Some("#N/A"));
        // Every class; the second sheet, its apostrophe doubled.
        assert_eq!(
            def(&area(0x7B, 1, 1, (2, 3))).as_deref(),
            Some("='It''s'!$C$1:$D$2")
        );
        assert_eq!(
            def(&area(0x5B, 0, 0, (0, 0))).as_deref(),
            Some("='P'!$A$1:$A$2")
        );
        assert_eq!(def(&cell(0x3A, 0, 1)).as_deref(), Some("='P'!$B$5"));
        assert_eq!(def(&cell(0x5A, 1, 0)).as_deref(), Some("='It''s'!$A$5"));
        for bad in [
            // A span, a sheet the book doesn't list, a relative column or
            // row, no class, a length of another layout, other tokens.
            area(0x3B, 0, 1, (0, 0)),
            area(0x3B, 2, 2, (0, 0)),
            area(0x3B, 0, 0, (0x4000, 0)),
            area(0x3B, 0, 0, (0, 0x8000)),
            cell(0x3A, 0, 0xC000),
            area(0x1B, 0, 0, (0, 0)),
            cell(0x1A, 0, 0),
            area(0x3B, 0, 0, (0, 0))[..11].to_vec(),
            vec![0x1C],
            vec![0x1C, 0x17, 0],
            vec![0x1E, 1, 0],
            vec![],
        ] {
            assert_eq!(def(&bad), None, "{bad:?}");
        }
    }

    /// An external book's qualifier is found outside literals and quoted
    /// names only.
    #[test]
    fn external_book_qualifiers_are_found_outside_quotes() {
        for yes in ["[1]!F(A1)", "[1]P!A1", "SUM([12]P!A1:A2)", "\"[\"&[1]P!A1"] {
            assert!(names_external_book(yes), "{yes}");
        }
        for no in [
            "'[1]My Sheet'!A1",
            "\"[1]P\"&A1",
            "T[Col]",
            "T[1x]",
            "[]",
            "A1",
            "'it''s [1]'!A1",
        ] {
            assert!(!names_external_book(no), "{no}");
        }
    }

    /// A book's cached cells are written per sheet in order; a sheet the
    /// book doesn't list, or a cell off the grid, is left out, and a book
    /// with none gets no `<sheetDataSet>`.
    #[test]
    fn cached_cells_are_written_per_sheet_in_order() {
        let cache: BTreeMap<(u32, u32, u32), ExtValue> = [
            ((1, 0, 2), ExtValue::Num(0.5)),
            ((0, 4, 0), ExtValue::Err("#DIV/0!")),
            ((0, 0, 1), ExtValue::Str("<x>".into())),
            ((0, 0, 0), ExtValue::Bool(false)),
            ((2, 0, 0), ExtValue::Num(1.0)),
            ((0, 0, crate::sheet::MAX_COLS), ExtValue::Num(1.0)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            sheet_data_set(&cache, 2),
            concat!(
                r#"<sheetDataSet><sheetData sheetId="0"><row r="1">"#,
                r#"<cell r="A1" t="b"><v>0</v></cell><cell r="B1" t="str"><v>&lt;x&gt;</v></cell>"#,
                r##"</row><row r="5"><cell r="A5" t="e"><v>#DIV/0!</v></cell></row></sheetData>"##,
                r#"<sheetData sheetId="1"><row r="1"><cell r="C1"><v>0.5</v></cell></row>"#,
                r#"</sheetData></sheetDataSet>"#,
            )
        );
        assert_eq!(sheet_data_set(&cache, 0), "");
        assert_eq!(sheet_data_set(&BTreeMap::new(), 3), "");
    }

    #[test]
    fn sheet_prefixes_quote_as_needed() {
        assert_eq!(sheet_prefix("Data", "Data"), "Data!");
        assert_eq!(sheet_prefix("My Sheet", "My Sheet"), "'My Sheet'!");
        assert_eq!(sheet_prefix("Jan", "Mar"), "Jan:Mar!");
        assert_eq!(sheet_prefix("Q1", "Q3"), "'Q1:Q3'!");
        assert_eq!(sheet_prefix("Sheet.1", "Sheet.3"), "'Sheet.1:Sheet.3'!");
        assert_eq!(sheet_prefix("", ""), "");
        // A `:` in a name is the marker, always quoted.
        assert_eq!(sheet_prefix("A:B", "A:B"), "'A\u{FDD0}B'!");
        assert_eq!(sheet_prefix("A:B", "C"), "'A\u{FDD0}B:C'!");
        assert_eq!(sheet_prefix("C", "A:B"), "'C:A\u{FDD0}B'!");
        assert_eq!(restore_colons("'A\u{FDD0}B'!A1"), "'A:B'!A1");
        // A bare qualifier (as the printer leaves a non-ASCII name) is
        // quoted; a bare word that is no qualifier is left alone.
        assert_eq!(
            restore_colons("A_B!A1+X\u{FDD0}Y!A1+X\u{FDD0}Y"),
            "A_B!A1+'X:Y'!A1+X\u{FDD0}Y"
        );
        // Every char the printer leaves bare belongs to the name: `·`,
        // a no-break space, an emoji.
        assert_eq!(restore_colons("A_B!A1+·X\u{FDD0}Y!A1"), "A_B!A1+'·X:Y'!A1");
        assert_eq!(restore_colons("X\u{FDD0}Y\u{A0}😀!A1"), "'X:Y\u{A0}😀'!A1");
        assert_eq!(
            restore_colons("'It''s\u{FDD0}'!A1&\"\u{FDD0}'\u{FDD0}\""),
            "'It''s:'!A1&\"\u{FDD0}'\u{FDD0}\""
        );
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

    /// `add_cond_formats` writes no block of no rules (the schema wants
    /// one) and leaves it out of the model; the others are written and
    /// claim their elements (#884 r1).
    #[test]
    fn add_cond_formats_skips_a_block_of_no_rules() {
        use crate::xlsx::{NewCfRule, NewCondFormat};
        let mut pkg = crate::xlsx::new_xlsx_sheets(&["S".to_string()]);
        let block = |rules: Vec<(NewCfRule, crate::sheet::Dxf)>| NewCondFormat {
            ranges: vec![(0, 0, 1, 0)],
            sqref: "A1:A2".into(),
            rules,
        };
        let rule = NewCfRule::Expression {
            formula: "A1>0".into(),
        };
        assert!(pkg.add_cond_formats(
            0,
            vec![
                block(Vec::new()),
                block(vec![(rule, crate::sheet::Dxf::default())])
            ]
        ));
        let part = String::from_utf8_lossy(pkg.part(&pkg.sheet_parts[0]).unwrap()).into_owned();
        assert_eq!(part.matches("<conditionalFormatting").count(), 1, "{part}");
        let cf = &pkg.workbook.sheets[0].cond_formats;
        assert_eq!(cf.len(), 1);
        assert_eq!((cf[0].rules.len(), cf[0].ix), (1, Some(0)));
        // A batch of only empty blocks changes nothing.
        let before = pkg.workbook.styles.dxfs.len();
        assert!(pkg.add_cond_formats(0, vec![block(Vec::new())]));
        assert_eq!(pkg.workbook.sheets[0].cond_formats.len(), 1);
        assert_eq!(pkg.workbook.styles.dxfs.len(), before);
    }

    /// `rebase` refuses a move that pushes a reference off the grid, and
    /// keeps a `#REF!` the formula already had (#884 r3).
    #[test]
    fn rebase_refuses_a_move_off_the_grid() {
        assert_eq!(rebase("A5>0", Some((4, 3)), (1, 1)), None);
        assert_eq!(
            rebase("D5>0", Some((4, 3)), (1, 1)).as_deref(),
            Some("B2>0")
        );
        assert_eq!(
            rebase("#REF!+D5", Some((4, 3)), (1, 1)).as_deref(),
            Some("#REF!+B2")
        );
        assert_eq!(rebase("#REF!+A5", Some((4, 3)), (1, 1)), None);
        assert_eq!(
            rebase("$A$5>0", Some((4, 3)), (1, 1)).as_deref(),
            Some("$A$5>0")
        );
        assert_eq!(rebase("A5", None, (1, 1)).as_deref(), Some("A5"));
    }
}
