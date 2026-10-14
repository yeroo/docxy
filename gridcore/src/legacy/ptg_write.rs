//! The BIFF8 formula-token compiler: formula text as the model stores it,
//! parsed by [`crate::formula::parse`], to a parsed-expression byte stream
//! (`rgce`, reverse Polish) and its extra data (array constants). It is the
//! inverse of [`super::ptg::decompile`], which reads back what this writes.
//!
//! Functions are written the way Excel writes them to an `.xls`:
//!
//! - a function in BIFF8's own table (Ftab 0..=379) as `ptgFunc` (fixed
//!   arity) or `ptgFuncVar`;
//! - an Analysis ToolPak function (Ftab 384..=476: EDATE, NETWORKDAYS, ...)
//!   or an XLL add-in's (`_xll.NAME`) as `ptgFuncVar` 255 over a `ptgNameX`
//!   into the add-in SUPBOOK;
//! - any newer function (IFERROR, TEXTJOIN, ...) as `ptgFuncVar` 255 over a
//!   `ptgName` naming a hidden `_xlfn.NAME`, `@x` as `_xlfn.SINGLE` and `A1#`
//!   as `_xlfn.ANCHORARRAY`; a function nobody defines (a VBA UDF) likewise,
//!   under its own name.
//!
//! Each operand token carries the class (reference, value or array) Excel
//! gives it, which Excel's evaluator uses: a range passed to SUM is a
//! reference, one in SUMPRODUCT an array, one an operator consumes a value.
//! The classes follow each function's parameter types ([`params`]), as
//! corpus/legacy's `.xls` files show Excel writing them.
//!
//! Parentheses are written where the precedence needs them, as
//! [`crate::formula::to_string`] prints them, so the text read back parses
//! to the same expression; a redundant pair the user typed is not kept.
//!
//! What BIFF8 can't hold makes the formula fail with a [`Why`], and the
//! writer keeps the cell's value without it.

use crate::formula::{BinOp, CellRef, ExcelError, Expr, UnOp, is_volatile, parse, prec};
use crate::sheet::Workbook;

use super::ftab;

/// Why a formula can't be written to an `.xls`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Why {
    /// A reference past row 65,536 or column IV.
    PastGrid,
    /// A string literal over 255 characters.
    LongText,
    /// LAMBDA, LET or calling an expression's result.
    Lambda,
    /// A reference into another workbook that isn't a Library add-in's
    /// function, or to a sheet the workbook doesn't have.
    OtherBook,
    /// Longer than BIFF8 allows (a token stream, too many arguments).
    TooLong,
    /// Anything else this can't express (an error value BIFF8 lacks, a
    /// structured reference to a table that isn't there, text that doesn't
    /// parse).
    Unsupported,
}

/// Which book a reference-table entry (XTI) is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Book {
    /// This workbook (the internal SUPBOOK).
    Own,
    /// The add-in functions book (the 0x3A01 SUPBOOK).
    AddIn,
    /// The `k`-th (0-based) external link of the package: a Library book.
    Lib(usize),
}

/// What a token stream needs from the workbook being written. The writer
/// collects the reference tables as formulas ask for entries.
pub(crate) trait Tables {
    fn workbook(&self) -> &Workbook;
    /// The XTI index of `book`'s sheets `first..=last` (-2 for a whole
    /// book's names).
    fn xti(&mut self, book: Book, first: i16, last: i16) -> u16;
    /// The 1-based NAME index of defined name `name` as seen from `sheet`
    /// (a sheet-scoped name shadows a global one), or of a placeholder NAME
    /// when no name is defined (Excel shows `#NAME?`).
    fn name(&mut self, name: &str, sheet: Option<usize>) -> u32;
    /// The 1-based NAME index of the hidden function name `name`
    /// (`_xlfn.TEXTJOIN`, or a UDF's own name).
    fn function(&mut self, name: &str) -> u32;
    /// The 1-based EXTERNNAME index of `name` under `book`, or `None` when
    /// the book can't take it (a link that is no Library book).
    fn ext_name(&mut self, book: Book, name: &str) -> Option<u32>;
}

/// A compiled formula: the tokens and their extra data.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Compiled {
    pub rgce: Vec<u8>,
    pub extra: Vec<u8>,
}

/// Where a formula is.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Site {
    /// A cell's formula: its sheet and cell; `array` for a legacy (CSE)
    /// array formula, whose operands are arrays.
    Cell {
        sheet: usize,
        row: u32,
        col: u32,
        array: bool,
    },
    /// A defined name's formula (`scope`: its sheet, if scoped).
    Name { scope: Option<usize> },
}

/// BIFF8's grid.
pub(crate) const MAX_ROW: i64 = 0xFFFF;
pub(crate) const MAX_COL: i64 = 0xFF;
/// The longest `rgce` Excel writes (its formula limit, in bytes).
const MAX_RGCE: usize = 1800;
/// The most tokens and extra data one record holds: its body's limit
/// less the fixed fields before them. An ARRAY, which can't be split,
/// has 14; a FORMULA 22, the most, so a formula fits either.
const MAX_TOKENS: usize = super::xls_write::MAX_BODY - 22;
/// `ptgFuncVar` counts arguments in 7 bits.
const MAX_ARGS: usize = 127;

/// An operand's class: what the token pushes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Ref,
    Val,
    Arr,
}

impl Class {
    /// The bits a classed token's id carries.
    fn bits(self) -> u8 {
        match self {
            Class::Ref => 0x20,
            Class::Val => 0x40,
            Class::Arr => 0x60,
        }
    }
    /// The class a parameter of type `param` gets in context `self`: an
    /// array context turns value parameters into array ones.
    fn of_param(self, param: Class) -> Class {
        match (self, param) {
            (Class::Arr, Class::Val) => Class::Arr,
            (_, p) => p,
        }
    }
    /// The class an operator's operands get.
    fn operand(self) -> Class {
        match self {
            Class::Arr => Class::Arr,
            _ => Class::Val,
        }
    }
}

/// Compile the formula `src` (as a cell or name stores it, without `=`) at
/// `site`.
pub(crate) fn compile(src: &str, site: Site, t: &mut dyn Tables) -> Result<Compiled, Why> {
    let text = mark_external_calls(src);
    let e = parse(&text).map_err(|_| {
        if src.contains("]!") || src.contains("['") {
            Why::OtherBook
        } else {
            Why::Unsupported
        }
    })?;
    let mut c = Compiler {
        t,
        out: Compiled::default(),
        site,
    };
    let top = match site {
        Site::Cell { array: true, .. } => Class::Arr,
        Site::Cell { .. } => Class::Val,
        Site::Name { .. } => Class::Ref,
    };
    c.expr(&e, top)?;
    // Excel marks a formula with a volatile function so it recalculates
    // on every calculation: tAttrSemi first.
    if is_volatile(&e) {
        let mut rgce = vec![0x19, 0x01, 0, 0];
        rgce.extend_from_slice(&c.out.rgce);
        c.out.rgce = rgce;
    }
    // The tokens and their extra data go in one record after its fixed
    // fields (a NAME's name too, which the writer checks).
    if c.out.rgce.len() > MAX_RGCE || c.out.rgce.len() + c.out.extra.len() > MAX_TOKENS {
        return Err(Why::TooLong);
    }
    Ok(c.out)
}

/// The spelling the parser can't read, `[k]!NAME(` (a call to a function
/// of external link `k`, as the import stores an add-in Library's
/// EUROCONVERT), as a call it can: `_XLEXT{k}.NAME(`. String literals and
/// quoted names are left alone.
fn mark_external_calls(src: &str) -> std::borrow::Cow<'_, str> {
    if !src.contains("]!") {
        return src.into();
    }
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 8);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                let start = i;
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
                i = (i + 1).min(b.len());
                out.push_str(&src[start..i]);
            }
            b'[' => {
                let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                let close = i + 1 + digits;
                let name_at = close + 2;
                let name_len = b
                    .get(name_at..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.'))
                    .count();
                if digits > 0
                    && b.get(close) == Some(&b']')
                    && b.get(close + 1) == Some(&b'!')
                    && name_len > 0
                    && b.get(name_at + name_len) == Some(&b'(')
                {
                    out.push_str("_XLEXT");
                    out.push_str(&src[i + 1..close]);
                    out.push('.');
                    out.push_str(&src[name_at..name_at + name_len]);
                    i = name_at + name_len;
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            _ => {
                let ch = src[i..].chars().next().unwrap_or_default();
                out.push(ch);
                i += ch.len_utf8().max(1);
            }
        }
    }
    out.into()
}

/// A function's parameter classes, as a pattern: one letter per parameter
/// (`R` reference, `V` value, `A` array), and after a `+` the group that
/// repeats for the rest (`R+RV` is SUMIFS: a range, then range/criteria
/// pairs). Functions not listed take values.
fn params(name: &str) -> &'static str {
    match name {
        "SUM" | "AVERAGE" | "MIN" | "MAX" | "COUNT" | "COUNTA" | "PRODUCT" | "STDEV" | "STDEVP"
        | "VAR" | "VARP" | "MEDIAN" | "SUMSQ" | "AVEDEV" | "DEVSQ" | "GEOMEAN" | "HARMEAN"
        | "KURT" | "SKEW" | "MAXA" | "MINA" | "AVERAGEA" | "STDEVA" | "STDEVPA" | "VARA"
        | "VARPA" | "AND" | "OR" | "XOR" | "CONCAT" | "COUNTBLANK" | "AREAS" | "ROW" | "COLUMN"
        | "ROWS" | "COLUMNS" | "N" | "T" | "ISREF" | "STDEV.S" | "STDEV.P" | "VAR.S" | "VAR.P"
        | "SINGLE" | "ANCHORARRAY" => "+R",
        "SUMPRODUCT" | "MODE" | "MODE.SNGL" | "MODE.MULT" | "MMULT" | "MDETERM" | "MINVERSE"
        | "TRANSPOSE" | "SUMX2MY2" | "SUMX2PY2" | "SUMXMY2" | "CORREL" | "PEARSON" | "RSQ"
        | "SLOPE" | "INTERCEPT" | "STEYX" | "COVAR" | "FTEST" | "CHITEST" | "FREQUENCY" => "+A",
        "COUNTIF" => "RV",
        "SUMIF" | "AVERAGEIF" => "RVR",
        "COUNTIFS" => "+RV",
        "SUMIFS" | "AVERAGEIFS" | "MAXIFS" | "MINIFS" => "R+RV",
        "VLOOKUP" | "HLOOKUP" => "VRVV",
        "MATCH" => "VRV",
        "LOOKUP" => "VRR",
        "INDEX" => "RVVV",
        "OFFSET" => "RVVVV",
        "CELL" => "VR",
        "RANK" | "RANK.EQ" | "RANK.AVG" => "VRV",
        "LARGE" | "SMALL" | "PERCENTILE" | "QUARTILE" | "IRR" => "RV",
        "PERCENTRANK" | "MIRR" => "RVV",
        "NPV" | "CHOOSE" | "SUBTOTAL" => "V+R",
        "AGGREGATE" | "TEXTJOIN" => "VV+R",
        "IF" => "VRR",
        "DAVERAGE" | "DCOUNT" | "DCOUNTA" | "DGET" | "DMAX" | "DMIN" | "DPRODUCT" | "DSTDEV"
        | "DSTDEVP" | "DSUM" | "DVAR" | "DVARP" => "RVR",
        "FORECAST" => "VAA",
        "TTEST" | "PROB" => "AAVV",
        "TREND" | "GROWTH" => "RRRV",
        "LINEST" | "LOGEST" => "RRVV",
        _ => "+V",
    }
}

/// The class of parameter `i` in pattern `p` ([`params`]).
fn param_class(p: &str, i: usize) -> Class {
    let (head, group) = p.split_once('+').unwrap_or((p, ""));
    let ch = match head.as_bytes().get(i) {
        Some(&c) => c,
        None if !group.is_empty() => group.as_bytes()[(i - head.len()) % group.len()],
        // Past a fixed list (a malformed call): a value.
        None => b'V',
    };
    match ch {
        b'R' => Class::Ref,
        b'A' => Class::Arr,
        _ => Class::Val,
    }
}

/// Functions whose result may be a reference: their token takes the class
/// its context asks for; any other function's is a value (or an array, in
/// an array context).
fn returns_ref(name: &str) -> bool {
    matches!(name, "IF" | "CHOOSE" | "INDEX" | "OFFSET" | "INDIRECT")
}

/// BIFF8's code of an error value.
pub(crate) fn error_code(e: &str) -> Option<u8> {
    Some(match ExcelError::from_code(e)? {
        ExcelError::Null => 0x00,
        ExcelError::Div0 => 0x07,
        ExcelError::Value => 0x0F,
        ExcelError::Ref => 0x17,
        ExcelError::Name => 0x1D,
        ExcelError::Num => 0x24,
        ExcelError::NA => 0x2A,
        ExcelError::GettingData => 0x2B,
        ExcelError::Spill | ExcelError::Calc => return None,
    })
}

/// A `ShortXLUnicodeString` (8-bit count): compressed when every unit fits
/// a byte. `None` past 255 units.
pub(crate) fn short_string(s: &str) -> Option<Vec<u8>> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let cch = u8::try_from(units.len()).ok()?;
    let mut out = vec![cch];
    push_chars(&mut out, &units);
    Some(out)
}

/// The flags byte and characters of a BIFF8 string: compressed (one byte a
/// character) when every unit is below 0x100, else UTF-16LE.
pub(crate) fn push_chars(out: &mut Vec<u8>, units: &[u16]) {
    if units.iter().all(|&u| u < 0x100) {
        out.push(0);
        out.extend(units.iter().map(|&u| u as u8));
    } else {
        out.push(1);
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
}

struct Compiler<'a> {
    t: &'a mut dyn Tables,
    out: Compiled,
    site: Site,
}

impl Compiler<'_> {
    fn sheet(&self) -> Option<usize> {
        match self.site {
            Site::Cell { sheet, .. } => Some(sheet),
            Site::Name { scope } => scope,
        }
    }

    fn row(&self) -> u32 {
        match self.site {
            Site::Cell { row, .. } => row,
            Site::Name { .. } => 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.out.rgce.extend_from_slice(bytes);
    }

    /// `e`, in parentheses when `paren`.
    fn operand(&mut self, e: &Expr, cls: Class, paren: bool) -> Result<(), Why> {
        self.expr(e, cls)?;
        if paren {
            self.push(&[0x15]);
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr, cls: Class) -> Result<(), Why> {
        match e {
            Expr::Num(n) => self.number(*n),
            Expr::Str(s) => {
                let b = short_string(s).ok_or(Why::LongText)?;
                self.push(&[0x17]);
                self.push(&b);
            }
            Expr::Bool(b) => self.push(&[0x1D, *b as u8]),
            Expr::Err(x) => self.push(&[0x1C, error_code(x.code()).ok_or(Why::Unsupported)?]),
            Expr::Missing => self.push(&[0x16]),
            Expr::Ref(r) => self.cell_ref(r, cls)?,
            Expr::Range(a, b) => {
                self.area(a.sheet.as_deref(), (a, b), cls)?;
            }
            Expr::ColRange {
                sheet,
                c1,
                c2,
                abs1,
                abs2,
            } => {
                let r = |col: i64, abs: bool| CellRef {
                    sheet: None,
                    row: 0,
                    col,
                    abs_row: true,
                    abs_col: abs,
                };
                let (mut a, mut b) = (r(*c1, *abs1), r(*c2, *abs2));
                b.row = MAX_ROW;
                a.sheet.clone_from(sheet);
                self.area(sheet.as_deref(), (&a, &b), cls)?;
            }
            Expr::RowRange {
                sheet,
                r1,
                r2,
                abs1,
                abs2,
            } => {
                let r = |row: i64, abs: bool| CellRef {
                    sheet: None,
                    row,
                    col: 0,
                    abs_row: abs,
                    abs_col: true,
                };
                let (a, mut b) = (r(*r1, *abs1), r(*r2, *abs2));
                b.col = MAX_COL;
                self.area(sheet.as_deref(), (&a, &b), cls)?;
            }
            Expr::Ref3D { first, last, a, b } => {
                let wb = self.t.workbook();
                let (Some(f), Some(l)) = (wb.sheet_index(first), wb.sheet_index(last)) else {
                    return Err(Why::OtherBook);
                };
                let ixti = self.t.xti(Book::Own, f as i16, l as i16);
                self.area_tokens(Some(ixti), a, b, cls)?;
            }
            Expr::Structured {
                table,
                item,
                col1,
                col2,
            } => {
                let wb = self.t.workbook();
                let t = match (table, self.site) {
                    (Some(n), _) => wb.table(n),
                    (
                        None,
                        Site::Cell {
                            sheet, row, col, ..
                        },
                    ) => wb.table_at(sheet, row, col),
                    (None, Site::Name { .. }) => None,
                }
                .ok_or(Why::Unsupported)?;
                let (r1, c1, r2, c2) = t
                    .info()
                    .resolve(*item, col1, col2, self.row())
                    .ok_or(Why::Unsupported)?;
                let range = table_range(wb, t.sheet, self.sheet(), (r1, c1, r2, c2));
                self.expr(&range, cls)?;
            }
            Expr::Name(n) => {
                // A table's bare name is its data body, as the engine reads
                // it; a defined name wins.
                let wb = self.t.workbook();
                let scope = self.sheet();
                let defined = wb.defined_names.iter().any(|d| {
                    d.name.eq_ignore_ascii_case(n) && (d.scope.is_none() || d.scope == scope)
                });
                if !defined {
                    if let Some(t) = wb.table(n) {
                        let rect = t
                            .info()
                            .resolve(crate::formula::TableItem::Data, &None, &None, 0)
                            .ok_or(Why::Unsupported)?;
                        let range = table_range(wb, t.sheet, scope, rect);
                        return self.expr(&range, cls);
                    }
                }
                let ix = self.t.name(n, scope);
                self.push(&[0x03 | cls.bits()]);
                self.push(&ix.to_le_bytes());
            }
            Expr::SpillRef(r) => {
                self.call("_XLFN.ANCHORARRAY", &[Expr::Ref(r.clone())], cls)?;
            }
            Expr::ArrayLit(rows) => self.array(rows, cls)?,
            Expr::Func(name, args) => self.call(name, args, cls)?,
            Expr::Call(..) | Expr::Lit(_) => return Err(Why::Lambda),
            Expr::Un(UnOp::Implicit, x) => {
                self.call("_XLFN.SINGLE", std::slice::from_ref(x), cls)?;
            }
            // A negative number literal is the number, as Excel stores it.
            Expr::Un(UnOp::Neg, x) if matches!(**x, Expr::Num(_)) => {
                let Expr::Num(n) = **x else { unreachable!() };
                self.number(-n);
            }
            Expr::Un(op, x) => {
                self.operand(x, cls.operand(), prec(x) < prec(e))?;
                self.push(&[match op {
                    UnOp::Pos => 0x12,
                    UnOp::Neg => 0x13,
                    _ => 0x14,
                }]);
            }
            Expr::Bin(op, l, r) => {
                self.operand(l, cls.operand(), prec(l) < prec(e))?;
                self.operand(r, cls.operand(), prec(r) <= prec(e))?;
                self.push(&[match op {
                    BinOp::Add => 0x03,
                    BinOp::Sub => 0x04,
                    BinOp::Mul => 0x05,
                    BinOp::Div => 0x06,
                    BinOp::Pow => 0x07,
                    BinOp::Concat => 0x08,
                    BinOp::Lt => 0x09,
                    BinOp::Le => 0x0A,
                    BinOp::Eq => 0x0B,
                    BinOp::Ge => 0x0C,
                    BinOp::Gt => 0x0D,
                    BinOp::Ne => 0x0E,
                }]);
            }
        }
        if self.out.rgce.len() > MAX_RGCE {
            return Err(Why::TooLong);
        }
        Ok(())
    }

    fn number(&mut self, n: f64) {
        if n.fract() == 0.0 && (0.0..=65535.0).contains(&n) && !(n == 0.0 && n.is_sign_negative()) {
            self.push(&[0x1E]);
            self.push(&(n as u16).to_le_bytes());
        } else {
            self.push(&[0x1F]);
            self.push(&n.to_le_bytes());
        }
    }

    /// The XTI of a reference's sheet qualifier, or `None` for none.
    fn qualifier(&mut self, sheet: Option<&str>) -> Result<Option<u16>, Why> {
        let Some(name) = sheet else {
            return Ok(None);
        };
        let i = self.t.workbook().sheet_index(name).ok_or(Why::OtherBook)?;
        Ok(Some(self.t.xti(Book::Own, i as i16, i as i16)))
    }

    /// A name's references are always 3-D, as Excel writes them (the
    /// corpus's TaxRate/TheData/Rate): a name has no sheet of its own. An
    /// unqualified one in a sheet-scoped name is on that sheet; in a global
    /// name it has no sheet to be on, and the name can't be written.
    fn name_xti(&mut self, ixti: Option<u16>) -> Result<Option<u16>, Why> {
        match (ixti, self.site) {
            (Some(x), _) => Ok(Some(x)),
            (None, Site::Name { scope: Some(s) }) => {
                Ok(Some(self.t.xti(Book::Own, s as i16, s as i16)))
            }
            (None, Site::Name { scope: None }) => Err(Why::Unsupported),
            (None, Site::Cell { .. }) => Ok(None),
        }
    }

    fn cell_ref(&mut self, r: &CellRef, cls: Class) -> Result<(), Why> {
        let ixti = self.qualifier(r.sheet.as_deref())?;
        let ixti = self.name_xti(ixti)?;
        let (row, col) = coords(r)?;
        match ixti {
            Some(x) => {
                self.push(&[0x1A | cls.bits()]);
                self.push(&x.to_le_bytes());
            }
            None => self.push(&[0x04 | cls.bits()]),
        }
        self.push(&row.to_le_bytes());
        self.push(&col.to_le_bytes());
        Ok(())
    }

    fn area(
        &mut self,
        sheet: Option<&str>,
        (a, b): (&CellRef, &CellRef),
        cls: Class,
    ) -> Result<(), Why> {
        let ixti = self.qualifier(sheet)?;
        let ixti = self.name_xti(ixti)?;
        self.area_tokens(ixti, a, b, cls)
    }

    fn area_tokens(
        &mut self,
        ixti: Option<u16>,
        a: &CellRef,
        b: &CellRef,
        cls: Class,
    ) -> Result<(), Why> {
        let (r1, c1) = coords(a)?;
        let (r2, c2) = coords(b)?;
        if a == b && ixti.is_some() {
            // `Q1:Q3!A1`: one cell across sheets.
            self.push(&[0x1A | cls.bits()]);
            self.push(&ixti.unwrap_or_default().to_le_bytes());
            self.push(&r1.to_le_bytes());
            self.push(&c1.to_le_bytes());
            return Ok(());
        }
        match ixti {
            Some(x) => {
                self.push(&[0x1B | cls.bits()]);
                self.push(&x.to_le_bytes());
            }
            None => self.push(&[0x05 | cls.bits()]),
        }
        self.push(&r1.to_le_bytes());
        self.push(&r2.to_le_bytes());
        self.push(&c1.to_le_bytes());
        self.push(&c2.to_le_bytes());
        Ok(())
    }

    fn array(&mut self, rows: &[Vec<Expr>], cls: Class) -> Result<(), Why> {
        let cols = rows.first().map_or(0, Vec::len);
        if cols == 0 || cols > 256 || rows.len() > 65536 {
            return Err(Why::Unsupported);
        }
        self.push(&[cls.bits()]);
        self.push(&[0; 7]);
        let ex = &mut self.out.extra;
        ex.push((cols - 1) as u8);
        ex.extend_from_slice(&((rows.len() - 1) as u16).to_le_bytes());
        for row in rows {
            for v in row {
                let neg = |x: &Expr| match x {
                    Expr::Num(n) => Some(-n),
                    _ => None,
                };
                match v {
                    Expr::Num(n) => {
                        ex.push(0x01);
                        ex.extend_from_slice(&n.to_le_bytes());
                    }
                    Expr::Un(UnOp::Neg, x) if neg(x).is_some() => {
                        ex.push(0x01);
                        ex.extend_from_slice(&neg(x).unwrap_or_default().to_le_bytes());
                    }
                    Expr::Str(s) => {
                        let units: Vec<u16> = s.encode_utf16().collect();
                        let n = u16::try_from(units.len()).map_err(|_| Why::LongText)?;
                        ex.push(0x02);
                        ex.extend_from_slice(&n.to_le_bytes());
                        push_chars(ex, &units);
                    }
                    Expr::Bool(b) => {
                        ex.push(0x04);
                        ex.push(*b as u8);
                        ex.extend_from_slice(&[0; 7]);
                    }
                    Expr::Err(x) => {
                        ex.push(0x10);
                        ex.push(error_code(x.code()).ok_or(Why::Unsupported)?);
                        ex.extend_from_slice(&[0; 7]);
                    }
                    _ => return Err(Why::Unsupported),
                }
            }
        }
        Ok(())
    }

    /// A call of function `name` (as the parser spells it: upper case, no
    /// `_xlfn.`; `_XLFN.SINGLE` / `_XLFN.ANCHORARRAY` for `@` and `#`).
    fn call(&mut self, name: &str, args: &[Expr], cls: Class) -> Result<(), Why> {
        if args.len() > MAX_ARGS {
            return Err(Why::TooLong);
        }
        if matches!(name, "LET" | "LAMBDA") {
            return Err(Why::Lambda);
        }
        let bare = name.strip_prefix("_XLFN.").unwrap_or(name);
        let token = if returns_ref(bare) {
            cls
        } else if cls == Class::Arr {
            Class::Arr
        } else {
            Class::Val
        };
        let builtin = (0..=379u16).find_map(|i| {
            let (n, argc) = ftab::lookup(i)?;
            n.eq_ignore_ascii_case(bare).then_some((i, argc))
        });
        if let Some((i, argc)) = builtin {
            let p = params(bare);
            if bare == "SUM" && args.len() == 1 {
                // Excel's one-argument SUM: tAttrSum.
                self.expr(&args[0], cls.of_param(Class::Ref))?;
                self.push(&[0x19, 0x10, 0, 0]);
                return Ok(());
            }
            self.args(args, p, cls)?;
            match argc {
                Some(n) if n as usize == args.len() => {
                    self.push(&[0x01 | token.bits()]);
                    self.push(&i.to_le_bytes());
                }
                _ => {
                    self.push(&[0x02 | token.bits(), args.len() as u8]);
                    self.push(&i.to_le_bytes());
                }
            }
            return Ok(());
        }
        // A named function: its name token first, then the arguments, then
        // ptgFuncVar 255 counting the name as one.
        if args.len() + 1 > MAX_ARGS {
            return Err(Why::TooLong);
        }
        let atp = (384..=476u16).any(|i| ftab::lookup(i).is_some_and(|(n, _)| n == bare));
        let (book, ext) = if atp {
            (Some(Book::AddIn), bare.to_string())
        } else if let Some(x) = name.strip_prefix("_XLL.") {
            (Some(Book::AddIn), x.to_string())
        } else if let Some(rest) = name.strip_prefix("_XLEXT") {
            let (k, n) = rest.split_once('.').ok_or(Why::OtherBook)?;
            let k: usize = k.parse().map_err(|_| Why::OtherBook)?;
            (
                Some(Book::Lib(k.checked_sub(1).ok_or(Why::OtherBook)?)),
                n.to_string(),
            )
        } else {
            (None, String::new())
        };
        match book {
            Some(book) => {
                let index = self.t.ext_name(book, &ext).ok_or(Why::OtherBook)?;
                let ixti = self.t.xti(book, -2, -2);
                self.push(&[0x19 | Class::Ref.bits()]);
                self.push(&ixti.to_le_bytes());
                self.push(&index.to_le_bytes());
                // An add-in's parameters are references, as for any UDF.
                self.args(args, "+R", cls)?;
            }
            None => {
                // SORT and FILTER are `_xlfn._xlws.`, as a file spells them.
                let stored = if is_new_function(bare) {
                    let prefix = crate::formula::future_prefix(bare).unwrap_or("_xlfn.");
                    format!("{prefix}{bare}")
                } else {
                    bare.to_string()
                };
                let ix = self.t.function(&stored);
                self.push(&[0x03 | Class::Ref.bits()]);
                self.push(&ix.to_le_bytes());
                let p = if is_new_function(bare) {
                    params(bare)
                } else {
                    "+R"
                };
                self.args(args, p, cls)?;
            }
        }
        self.push(&[0x02 | token.bits(), (args.len() + 1) as u8, 0xFF, 0x00]);
        Ok(())
    }

    fn args(&mut self, args: &[Expr], p: &str, cls: Class) -> Result<(), Why> {
        for (i, a) in args.iter().enumerate() {
            self.expr(a, cls.of_param(param_class(p, i)))?;
        }
        Ok(())
    }
}

/// Whether Excel spells function `name` (upper case) `_xlfn.NAME` in a
/// file: one newer than BIFF8's table (Excel 2007's IFERROR, SUMIFS, the
/// cube functions, and every later one).
fn is_new_function(name: &str) -> bool {
    (380..=383u16)
        .chain(477..=484)
        .any(|i| ftab::lookup(i).is_some_and(|(n, _)| n == name))
        || crate::formula::future_prefix(name).is_some()
        || matches!(name, "SINGLE" | "ANCHORARRAY")
}

/// A reference's row and column words: 0-based coordinates, the column
/// word flagging a relative column (bit 14) and row (bit 15).
fn coords(r: &CellRef) -> Result<(u16, u16), Why> {
    if r.row < 0 || r.col < 0 || r.row > MAX_ROW || r.col > MAX_COL {
        return Err(Why::PastGrid);
    }
    let mut col = r.col as u16;
    if !r.abs_col {
        col |= 0x4000;
    }
    if !r.abs_row {
        col |= 0x8000;
    }
    Ok((r.row as u16, col))
}

/// The absolute range a structured reference covers, qualified with its
/// table's sheet unless that is `here`.
fn table_range(
    wb: &Workbook,
    sheet: usize,
    here: Option<usize>,
    (r1, c1, r2, c2): (u32, u32, u32, u32),
) -> Expr {
    let q = (here != Some(sheet)).then(|| wb.sheets[sheet].name.clone());
    let at = |row: u32, col: u32| CellRef {
        sheet: None,
        row: row as i64,
        col: col as i64,
        abs_row: true,
        abs_col: true,
    };
    let mut a = at(r1, c1);
    a.sheet = q;
    let b = at(r2, c2);
    if (r1, c1) == (r2, c2) {
        Expr::Ref(a)
    } else {
        Expr::Range(a, b)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::legacy::ptg::{self, Base, Biff, Names};
    use std::collections::HashMap;

    /// Tables for tests: sheets of `wb`, names in order of asking.
    pub(crate) struct T {
        pub wb: Workbook,
        pub xti: Vec<(Book, i16, i16)>,
        pub names: Vec<String>,
        pub ext: HashMap<Book, Vec<String>>,
    }

    impl T {
        pub fn new(sheets: &[&str]) -> T {
            let mut wb = crate::xlsx::new_xlsx_sheets(
                &sheets.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
            .workbook;
            wb.defined_names.push(crate::sheet::DefinedName {
                name: "Rate".into(),
                scope: None,
                formula: "Sheet1!$A$1".into(),
            });
            T {
                wb,
                xti: Vec::new(),
                names: vec!["Rate".into()],
                ext: HashMap::new(),
            }
        }
    }

    impl Tables for T {
        fn workbook(&self) -> &Workbook {
            &self.wb
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
        fn name(&mut self, name: &str, _: Option<usize>) -> u32 {
            self.function(name)
        }
        fn function(&mut self, name: &str) -> u32 {
            match self.names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
                Some(i) => i as u32 + 1,
                None => {
                    self.names.push(name.to_string());
                    self.names.len() as u32
                }
            }
        }
        fn ext_name(&mut self, book: Book, name: &str) -> Option<u32> {
            let v = self.ext.entry(book).or_default();
            Some(match v.iter().position(|n| n == name) {
                Some(i) => i as u32 + 1,
                None => {
                    v.push(name.to_string());
                    v.len() as u32
                }
            })
        }
    }

    /// The decompiler's view of the same tables.
    impl Names for T {
        fn xti(&self, ixti: u32) -> Option<String> {
            let (book, f, l) = *self.xti.get(ixti as usize)?;
            (book == Book::Own).then(|| {
                crate::legacy::sheet_prefix(
                    &self.wb.sheets[f as usize].name,
                    &self.wb.sheets[l as usize].name,
                )
            })
        }
        fn name(&self, index: u32) -> Option<String> {
            self.names.get(index as usize - 1).cloned()
        }
        fn name_x(&self, ixti: u32, index: u32) -> Option<String> {
            let (book, ..) = *self.xti.get(ixti as usize)?;
            let n = self.ext.get(&book)?.get(index as usize - 1)?;
            Some(match book {
                Book::AddIn => ftab::xll_name(n),
                Book::Lib(k) => format!("[{}]!{n}", k + 1),
                Book::Own => n.clone(),
            })
        }
    }

    pub(crate) fn cell(row: u32, col: u32) -> Site {
        Site::Cell {
            sheet: 0,
            row,
            col,
            array: false,
        }
    }

    /// `src` compiled at A1 and read back.
    fn round(src: &str) -> Result<String, Why> {
        let mut t = T::new(&["Sheet1", "Data", "Q1", "Q2"]);
        let c = compile(src, cell(0, 0), &mut t)?;
        Ok(
            ptg::decompile(Biff::V8, &c.rgce, &c.extra, Base::Cell(Some((0, 0))), &t)
                .unwrap_or_else(|| panic!("{src}: {:02x?}", c.rgce)),
        )
    }

    /// Read back as the same expression.
    fn same(src: &str) {
        let back = round(src).unwrap_or_else(|w| panic!("{src}: {w:?}"));
        let want = parse(&crate::formula::display_formula(src)).unwrap();
        let got = parse(&back).unwrap_or_else(|e| panic!("{src} -> {back}: {e}"));
        assert_eq!(got, want, "{src} -> {back}");
    }

    #[test]
    fn operators_get_the_parentheses_precedence_needs() {
        for src in [
            "(1+2)*3",
            "1+2*3",
            "1-(2-3)",
            "1-2-3",
            "2^(3^2)",
            "-(1+2)",
            "-2^2",
            "(A1+B1)%",
            "A1&\"x\"=\"1x\"",
            "--A1",
            "+A1",
            "((1))",
        ] {
            same(src);
        }
        assert_eq!(round("(1+2)*3").unwrap(), "(1+2)*3");
        assert_eq!(round("((1))+2").unwrap(), "1+2");
    }

    #[test]
    fn references_whole_columns_rows_3d_and_names() {
        for src in [
            "A1",
            "$B$2+B$2+$B2",
            "SUM(A1:C3)",
            "SUM(A:A)",
            "SUM($C:$D)",
            "SUM(2:2)",
            "Data!A1+Data!B2:C3",
            "SUM(Q1:Q2!A1:B2)",
            "Rate*2",
            "Undefined+1",
            "SUM('Data'!A:A)",
        ] {
            same(src);
        }
        assert_eq!(round("SUM(A:A)").unwrap(), "SUM(A:A)");
        assert_eq!(round("IV65536").unwrap(), "IV65536");
        assert_eq!(round("A1048576"), Err(Why::PastGrid));
        assert_eq!(round("IW1"), Err(Why::PastGrid));
        assert_eq!(round("SUM(A1:A100000)"), Err(Why::PastGrid));
        assert_eq!(round("Missing!A1"), Err(Why::OtherBook));
        // A defined name's references are 3-D, as Excel's; an unqualified
        // one in a global name has no sheet.
        let mut t = T::new(&["Sheet1", "Data"]);
        let c = compile("Data!$A$1", Site::Name { scope: None }, &mut t).unwrap();
        assert_eq!(c.rgce[0], 0x3A);
        let c = compile("$A$1:$B$2", Site::Name { scope: Some(1) }, &mut t).unwrap();
        assert_eq!(c.rgce[0], 0x3B);
        assert_eq!(
            compile("$A$1", Site::Name { scope: None }, &mut t),
            Err(Why::Unsupported)
        );
    }

    #[test]
    fn functions_builtin_new_add_in_and_udf() {
        for src in [
            "SUM(A1,B1)",
            "SUM(A1:A3)",
            "ROUND(A1,2)",
            "IF(A1,\"y\",)",
            "_xlfn.TEXTJOIN(\",\",TRUE,A1:A3)",
            "IFERROR(1/0,2)",
            "_xlfn.XLOOKUP(A1,B:B,C:C)",
            "_xlfn.SINGLE(A1:A3)",
            "_xlfn.ANCHORARRAY(A1)",
            "EDATE(A1,1)",
            "_xll.FOO(A1)",
            "MYUDF(1,2)",
            "{1,-2;\"a\",TRUE}",
            "NOW()",
            "PI()",
        ] {
            same(src);
        }
        assert_eq!(round("_xlfn.SINGLE(A1:A3)").unwrap(), "_xlfn.SINGLE(A1:A3)");
        assert_eq!(round("EDATE(A1,1)").unwrap(), "EDATE(A1,1)");
        assert_eq!(round("_xll.FOO(A1)").unwrap(), "_xll.FOO(A1)");
        assert_eq!(round("_xlfn.LAMBDA(_xlpm.x,_xlpm.x)(1)"), Err(Why::Lambda));
        assert_eq!(round("_xlfn.LET(_xlpm.x,1,_xlpm.x)"), Err(Why::Lambda));
        // A named function counts its name as an argument: 127 at most.
        let args = |n: usize| vec!["1"; n].join(",");
        assert!(round(&format!("MYUDF({})", args(126))).is_ok());
        assert_eq!(round(&format!("MYUDF({})", args(127))), Err(Why::TooLong));
        // An array constant too big for one record.
        let row = vec!["\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\""; 30].join(",");
        let big = format!("{{{}}}", vec![row; 10].join(";"));
        assert_eq!(round(&big), Err(Why::TooLong));
        let long = format!("\"{}\"", "x".repeat(256));
        assert_eq!(round(&long), Err(Why::LongText));
        assert!(round(&format!("\"{}\"", "x".repeat(255))).is_ok());
    }

    #[test]
    fn functions_take_excels_token_classes() {
        let mut t = T::new(&["Sheet1"]);
        let c = |src: &str, t: &mut T| compile(src, cell(5, 5), t).unwrap().rgce;
        // SUM(A1:A3): the area is a reference (0x25), the one-argument SUM
        // tAttrSum.
        assert_eq!(
            c("SUM(A1:A3)", &mut t),
            [0x25, 0, 0, 2, 0, 0, 0xC0, 0, 0xC0, 0x19, 0x10, 0, 0]
        );
        // A1+1: a value (0x44).
        assert_eq!(c("A1+1", &mut t)[0], 0x44);
        // SUMPRODUCT's arguments are arrays (0x65), the call a value.
        let sp = c("SUMPRODUCT(A1:A2,B1:B2)", &mut t);
        assert_eq!((sp[0], sp[9]), (0x65, 0x65));
        assert_eq!(sp[18], 0x42);
        // VLOOKUP(A1,B1:C2,2,FALSE): value, reference, value.
        let v = c("VLOOKUP(A1,B1:C2,2,FALSE)", &mut t);
        assert_eq!((v[0], v[5]), (0x44, 0x25));
        // IF nested in IF's branch is a reference-class call.
        let i = c("IF(A1,1,IF(B1,2,3))", &mut t);
        assert_eq!(&i[i.len() - 8..i.len() - 4], &[0x22, 3, 1, 0]);
        assert_eq!(&i[i.len() - 4..], &[0x42, 3, 1, 0]);
        // A volatile formula starts with tAttrSemi.
        assert_eq!(&c("NOW()", &mut t)[..4], &[0x19, 0x01, 0, 0]);
    }

    #[test]
    fn library_link_calls_go_through_their_supbook() {
        assert_eq!(
            mark_external_calls("[1]!EUROCONVERT(A1,\"[2]!X(\")+'[3]!Y('!A1"),
            "_XLEXT1.EUROCONVERT(A1,\"[2]!X(\")+'[3]!Y('!A1"
        );
        assert_eq!(
            round("[1]!EUROCONVERT(1,\"DEM\")").unwrap(),
            "[1]!EUROCONVERT(1,\"DEM\")"
        );
    }

    #[test]
    fn structured_references_become_ranges() {
        let mut t = T::new(&["Sheet1", "Data"]);
        t.wb.tables.push(crate::sheet::Table {
            name: "Orders".into(),
            sheet: 1,
            range: (0, 0, 3, 1),
            header_rows: 1,
            columns: vec!["Item".into(), "Qty".into()],
            ..Default::default()
        });
        let c = compile("SUM(Orders[Qty])+SUM(Orders)", cell(0, 0), &mut t).unwrap();
        let back = ptg::decompile(Biff::V8, &c.rgce, &c.extra, Base::Cell(Some((0, 0))), &t);
        assert_eq!(
            back.as_deref(),
            Some("SUM(Data!$B$2:$B$4)+SUM(Data!$A$2:$B$4)")
        );
    }
}
