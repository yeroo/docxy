//! The formula language: lexer → parser → AST → evaluator, plus a serializer.
//!
//! The serializer matters as much as the parser: shared-formula expansion,
//! Excel-style copy/paste, and (later) row/column insertion are all "parse,
//! shift relative references, print back".
//!
//! Design rules:
//! - Semantics follow Excel: the eight error values propagate through
//!   operators, empty cells coerce by context, comparisons are
//!   case-insensitive and ordered Number < Text < Logical.
//! - Anything we cannot understand (unknown function, defined name, 3D or
//!   whole-column reference) evaluates with the `unsupported` flag set so the
//!   engine keeps Excel's cached value instead of writing a wrong one.
//! - Coordinates in the AST are 0-based `i64` so translation can go negative
//!   and be caught (→ `#REF!`) instead of wrapping.

use crate::sheet::{col_name, fmt_general, parse_col, parts_to_serial, serial_to_parts};

// ---------------------------------------------------------------------------
// Values & errors
// ---------------------------------------------------------------------------

/// Excel's error values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExcelError {
    Div0,
    NA,
    Name,
    Null,
    Num,
    Ref,
    Value,
    Spill,
    Calc,
    /// Not a real Excel error: our marker for circular references (Excel
    /// shows a dialog and writes 0; we are honest instead).
    Cycle,
}

impl ExcelError {
    pub fn code(&self) -> &'static str {
        match self {
            ExcelError::Div0 => "#DIV/0!",
            ExcelError::NA => "#N/A",
            ExcelError::Name => "#NAME?",
            ExcelError::Null => "#NULL!",
            ExcelError::Num => "#NUM!",
            ExcelError::Ref => "#REF!",
            ExcelError::Value => "#VALUE!",
            ExcelError::Spill => "#SPILL!",
            ExcelError::Calc => "#CALC!",
            ExcelError::Cycle => "#CYCLE!",
        }
    }

    pub fn from_code(s: &str) -> Option<ExcelError> {
        Some(match s.to_ascii_uppercase().as_str() {
            "#DIV/0!" => ExcelError::Div0,
            "#N/A" => ExcelError::NA,
            "#NAME?" => ExcelError::Name,
            "#NULL!" => ExcelError::Null,
            "#NUM!" => ExcelError::Num,
            "#REF!" => ExcelError::Ref,
            "#VALUE!" => ExcelError::Value,
            "#SPILL!" => ExcelError::Spill,
            "#CALC!" => ExcelError::Calc,
            "#CYCLE!" => ExcelError::Cycle,
            _ => return None,
        })
    }
}

/// A value during evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ExcelError),
}

impl Value {
    pub fn is_err(&self) -> bool {
        matches!(self, Value::Err(_))
    }
}

/// Map a computed float back into a value, turning IEEE overflow into `#NUM!`
/// the way Excel does.
fn num(v: f64) -> Value {
    if v.is_finite() {
        Value::Num(v)
    } else {
        Value::Err(ExcelError::Num)
    }
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// A cell reference: 0-based coordinates plus `$` anchoring. `sheet` is the
/// optional `Sheet1!` qualifier (unquoted form stored verbatim).
#[derive(Clone, Debug, PartialEq)]
pub struct CellRef {
    pub sheet: Option<String>,
    pub row: i64,
    pub col: i64,
    pub abs_row: bool,
    pub abs_col: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Pos,
    /// Postfix `%`.
    Percent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ExcelError),
    /// An omitted argument, e.g. the second slot of `IF(A1,,2)`.
    Missing,
    Ref(CellRef),
    /// `A1:B2`; the sheet qualifier of the first ref covers the range.
    Range(CellRef, CellRef),
    /// `A:C` — whole columns. Evaluation clamps to the sheet's used range.
    ColRange {
        sheet: Option<String>,
        c1: i64,
        c2: i64,
        abs1: bool,
        abs2: bool,
    },
    /// `1:3` — whole rows.
    RowRange {
        sheet: Option<String>,
        r1: i64,
        r2: i64,
        abs1: bool,
        abs2: bool,
    },
    /// A defined name, resolved through the workbook at evaluation time.
    Name(String),
    Func(String, Vec<Expr>),
    Un(UnOp, Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    /// Identifier-ish: function names, cell refs, sheet names, `$A$1`.
    Ident(String),
    /// `'Sheet name'` — always a sheet qualifier.
    Quoted(String),
    Err(ExcelError),
    LParen,
    RParen,
    Comma,
    Colon,
    Bang,
    Percent,
    Amp,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Eof,
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            src: src.as_bytes(),
            pos: 0,
        }
    }

    fn next_tok(&mut self) -> Result<Tok, String> {
        while self.pos < self.src.len() && (self.src[self.pos] as char).is_whitespace() {
            self.pos += 1;
        }
        if self.pos >= self.src.len() {
            return Ok(Tok::Eof);
        }
        let c = self.src[self.pos];
        self.pos += 1;
        Ok(match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            b'!' => Tok::Bang,
            b'%' => Tok::Percent,
            b'&' => Tok::Amp,
            b'+' => Tok::Plus,
            b'-' => Tok::Minus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'^' => Tok::Caret,
            b'=' => Tok::Eq,
            b'<' => {
                if self.peek() == Some(b'>') {
                    self.pos += 1;
                    Tok::Ne
                } else if self.peek() == Some(b'=') {
                    self.pos += 1;
                    Tok::Le
                } else {
                    Tok::Lt
                }
            }
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.pos += 1;
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            b'"' => {
                let mut s = String::new();
                loop {
                    match self.take() {
                        Some(b'"') => {
                            if self.peek() == Some(b'"') {
                                self.pos += 1;
                                s.push('"');
                            } else {
                                break;
                            }
                        }
                        Some(b) => s.push_str(&self.push_utf8(b)),
                        None => return Err("unterminated string".into()),
                    }
                }
                Tok::Str(s)
            }
            b'\'' => {
                // 'Sheet name' — '' escapes a quote inside.
                let mut s = String::new();
                loop {
                    match self.take() {
                        Some(b'\'') => {
                            if self.peek() == Some(b'\'') {
                                self.pos += 1;
                                s.push('\'');
                            } else {
                                break;
                            }
                        }
                        Some(b) => s.push_str(&self.push_utf8(b)),
                        None => return Err("unterminated sheet name".into()),
                    }
                }
                Tok::Quoted(s)
            }
            b'#' => {
                // Error literal: read through the terminating '!' or '?'; #N/A
                // ends bare.
                let start = self.pos - 1;
                while self.pos < self.src.len() {
                    let b = self.src[self.pos];
                    self.pos += 1;
                    if b == b'!' || b == b'?' {
                        break;
                    }
                    if !(b.is_ascii_alphanumeric() || b == b'/') {
                        self.pos -= 1;
                        break;
                    }
                }
                let lit = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
                match ExcelError::from_code(lit) {
                    Some(e) => Tok::Err(e),
                    None => return Err(format!("bad error literal {lit}")),
                }
            }
            b'0'..=b'9' | b'.' => {
                self.pos -= 1;
                let start = self.pos;
                while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
                if self.peek() == Some(b'.') {
                    self.pos += 1;
                    while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                }
                if matches!(self.peek(), Some(b'e') | Some(b'E')) {
                    let save = self.pos;
                    self.pos += 1;
                    if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                        self.pos += 1;
                    }
                    if matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                        while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                            self.pos += 1;
                        }
                    } else {
                        self.pos = save; // "1E" was a ref like E1? No — bare ident; back off.
                    }
                }
                let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
                match text.parse::<f64>() {
                    Ok(n) => Tok::Num(n),
                    Err(_) => return Err(format!("bad number {text}")),
                }
            }
            _ if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80 => {
                self.pos -= 1;
                let start = self.pos;
                while self.pos < self.src.len() {
                    let b = self.src[self.pos];
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'$' || b >= 0x80
                    {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let s = std::str::from_utf8(&self.src[start..self.pos])
                    .map_err(|_| "bad identifier".to_string())?;
                Tok::Ident(s.to_string())
            }
            _ => return Err(format!("unexpected character {:?}", c as char)),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }
    fn take(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }
    /// Consume the remaining bytes of a UTF-8 char whose first byte is `b`.
    fn push_utf8(&mut self, b: u8) -> String {
        let len = if b < 0x80 {
            1
        } else if b >= 0xF0 {
            4
        } else if b >= 0xE0 {
            3
        } else {
            2
        };
        let start = self.pos - 1;
        let end = (start + len).min(self.src.len());
        self.pos = end;
        String::from_utf8_lossy(&self.src[start..end]).into_owned()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    lex: Lexer<'a>,
    tok: Tok,
}

/// Parse a formula body (no leading `=`). Errors are strings — the engine
/// treats any parse failure as "unsupported: preserve, don't evaluate".
pub fn parse(src: &str) -> Result<Expr, String> {
    let mut lex = Lexer::new(src);
    let tok = lex.next_tok()?;
    let mut p = Parser { lex, tok };
    let e = p.compare()?;
    if p.tok != Tok::Eof {
        return Err(format!("unexpected trailing token {:?}", p.tok));
    }
    Ok(e)
}

impl<'a> Parser<'a> {
    fn bump(&mut self) -> Result<(), String> {
        self.tok = self.lex.next_tok()?;
        Ok(())
    }

    fn compare(&mut self) -> Result<Expr, String> {
        let mut e = self.concat()?;
        loop {
            let op = match self.tok {
                Tok::Eq => BinOp::Eq,
                Tok::Ne => BinOp::Ne,
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.bump()?;
            let r = self.concat()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn concat(&mut self) -> Result<Expr, String> {
        let mut e = self.addsub()?;
        while self.tok == Tok::Amp {
            self.bump()?;
            let r = self.addsub()?;
            e = Expr::Bin(BinOp::Concat, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn addsub(&mut self) -> Result<Expr, String> {
        let mut e = self.muldiv()?;
        loop {
            let op = match self.tok {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump()?;
            let r = self.muldiv()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn muldiv(&mut self) -> Result<Expr, String> {
        let mut e = self.pow()?;
        loop {
            let op = match self.tok {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => break,
            };
            self.bump()?;
            let r = self.pow()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn pow(&mut self) -> Result<Expr, String> {
        // Left-associative in Excel: 2^3^2 = 64. Unary minus binds tighter:
        // -2^2 = 4.
        let mut e = self.unary()?;
        while self.tok == Tok::Caret {
            self.bump()?;
            let r = self.unary()?;
            e = Expr::Bin(BinOp::Pow, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.tok {
            Tok::Minus => {
                self.bump()?;
                Ok(Expr::Un(UnOp::Neg, Box::new(self.unary()?)))
            }
            Tok::Plus => {
                self.bump()?;
                Ok(Expr::Un(UnOp::Pos, Box::new(self.unary()?)))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        while self.tok == Tok::Percent {
            self.bump()?;
            e = Expr::Un(UnOp::Percent, Box::new(e));
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match std::mem::replace(&mut self.tok, Tok::Eof) {
            Tok::Num(n) => {
                self.bump()?;
                // `1:3` — a whole-row range.
                if self.tok == Tok::Colon {
                    if let Some((r1, abs1)) = row_from_num(n) {
                        self.bump()?;
                        let (r2, abs2) = self.row_range_end()?;
                        return Ok(Expr::RowRange {
                            sheet: None,
                            r1,
                            r2,
                            abs1,
                            abs2,
                        });
                    }
                }
                Ok(Expr::Num(n))
            }
            Tok::Str(s) => {
                self.bump()?;
                Ok(Expr::Str(s))
            }
            Tok::Err(e) => {
                self.bump()?;
                Ok(Expr::Err(e))
            }
            Tok::LParen => {
                self.bump()?;
                let e = self.compare()?;
                if self.tok != Tok::RParen {
                    return Err("expected )".into());
                }
                self.bump()?;
                Ok(e)
            }
            Tok::Quoted(name) => {
                self.bump()?;
                if self.tok != Tok::Bang {
                    return Err("quoted name without !".into());
                }
                self.bump()?;
                self.sheet_ref(Some(name))
            }
            Tok::Ident(id) => {
                self.bump()?;
                match self.tok {
                    Tok::Bang => {
                        self.bump()?;
                        self.sheet_ref(Some(id))
                    }
                    Tok::LParen => {
                        self.bump()?;
                        let mut args = Vec::new();
                        if self.tok == Tok::RParen {
                            self.bump()?;
                        } else {
                            loop {
                                if self.tok == Tok::Comma {
                                    args.push(Expr::Missing);
                                    self.bump()?;
                                    continue;
                                }
                                args.push(self.compare()?);
                                match self.tok {
                                    Tok::Comma => {
                                        self.bump()?;
                                        if self.tok == Tok::RParen {
                                            args.push(Expr::Missing);
                                            self.bump()?;
                                            break;
                                        }
                                    }
                                    Tok::RParen => {
                                        self.bump()?;
                                        break;
                                    }
                                    _ => return Err("expected , or ) in arguments".into()),
                                }
                            }
                        }
                        // Excel writes post-2007 functions as _xlfn.NAME.
                        let name = id
                            .strip_prefix("_xlfn.")
                            .unwrap_or(&id)
                            .to_ascii_uppercase();
                        Ok(Expr::Func(name, args))
                    }
                    _ => self.ident_expr(id, None),
                }
            }
            t => Err(format!("unexpected token {t:?}")),
        }
    }

    /// After `Sheet!` — parse the cell or range the qualifier applies to.
    fn sheet_ref(&mut self, sheet: Option<String>) -> Result<Expr, String> {
        match std::mem::replace(&mut self.tok, Tok::Eof) {
            Tok::Ident(id) => {
                self.bump()?;
                self.ident_expr(id, sheet)
            }
            Tok::Num(n) => {
                // `Sheet1!1:3` — whole rows.
                self.bump()?;
                let (r1, abs1) = row_from_num(n).ok_or("bad row reference")?;
                if self.tok != Tok::Colon {
                    return Err("expected : in row reference".into());
                }
                self.bump()?;
                let (r2, abs2) = self.row_range_end()?;
                Ok(Expr::RowRange {
                    sheet,
                    r1,
                    r2,
                    abs1,
                    abs2,
                })
            }
            t => Err(format!("expected reference after sheet name, got {t:?}")),
        }
    }

    /// The right-hand side of a `N:` row range: a number or `$N`.
    fn row_range_end(&mut self) -> Result<(i64, bool), String> {
        match std::mem::replace(&mut self.tok, Tok::Eof) {
            Tok::Num(n) => {
                self.bump()?;
                row_from_num(n).ok_or_else(|| "bad row range end".to_string())
            }
            Tok::Ident(id) => {
                self.bump()?;
                parse_row_text(&id).ok_or_else(|| "bad row range end".to_string())
            }
            t => Err(format!("expected row after :, got {t:?}")),
        }
    }

    /// An identifier outside call position: cell ref, range start, whole-column
    /// range, TRUE/FALSE, or a defined name.
    fn ident_expr(&mut self, id: String, sheet: Option<String>) -> Result<Expr, String> {
        match id.to_ascii_uppercase().as_str() {
            "TRUE" if sheet.is_none() => return Ok(Expr::Bool(true)),
            "FALSE" if sheet.is_none() => return Ok(Expr::Bool(false)),
            _ => {}
        }
        if let Some(mut r) = parse_ref_text(&id) {
            r.sheet = sheet;
            if self.tok == Tok::Colon {
                self.bump()?;
                let second = match std::mem::replace(&mut self.tok, Tok::Eof) {
                    Tok::Ident(id2) => {
                        self.bump()?;
                        parse_ref_text(&id2).ok_or("bad range end")?
                    }
                    t => return Err(format!("expected range end, got {t:?}")),
                };
                return Ok(Expr::Range(r, second));
            }
            return Ok(Expr::Ref(r));
        }
        // `A:C` / `$A:$A` — whole columns (only valid when a `:` follows).
        if self.tok == Tok::Colon {
            if let Some((c1, abs1)) = parse_col_text(&id) {
                self.bump()?;
                let (c2, abs2) = match std::mem::replace(&mut self.tok, Tok::Eof) {
                    Tok::Ident(id2) => {
                        self.bump()?;
                        parse_col_text(&id2).ok_or("bad column range end")?
                    }
                    t => return Err(format!("expected column after :, got {t:?}")),
                };
                return Ok(Expr::ColRange {
                    sheet,
                    c1,
                    c2,
                    abs1,
                    abs2,
                });
            }
        }
        if sheet.is_some() {
            return Err("sheet-qualified name".into());
        }
        Ok(Expr::Name(id))
    }
}

/// `$3` / a numeric literal used as a row reference → 0-based row + anchor.
fn parse_row_text(s: &str) -> Option<(i64, bool)> {
    let (abs, rest) = match s.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, s),
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row: u32 = rest.parse().ok()?;
    if row == 0 || row > crate::sheet::MAX_ROWS {
        return None;
    }
    Some((row as i64 - 1, abs))
}

fn row_from_num(n: f64) -> Option<(i64, bool)> {
    if n.fract() != 0.0 || n < 1.0 || n > crate::sheet::MAX_ROWS as f64 {
        return None;
    }
    Some((n as i64 - 1, false))
}

/// `"A"` / `"$C"` — pure column letters → 0-based column + anchor.
fn parse_col_text(s: &str) -> Option<(i64, bool)> {
    let (abs, rest) = match s.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (col, used) = parse_col(rest)?;
    if used != rest.len() {
        return None;
    }
    Some((col as i64, abs))
}

/// Parse "$B$12" / "B12" into a CellRef (no sheet). None if not a cell ref.
fn parse_ref_text(s: &str) -> Option<CellRef> {
    let (abs_col, rest) = match s.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (col, used) = parse_col(rest)?;
    let rest = &rest[used..];
    let (abs_row, rest) = match rest.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row: u32 = rest.parse().ok()?;
    if row == 0 || row > crate::sheet::MAX_ROWS {
        return None;
    }
    Some(CellRef {
        sheet: None,
        row: row as i64 - 1,
        col: col as i64,
        abs_row,
        abs_col,
    })
}

// ---------------------------------------------------------------------------
// Serializer
// ---------------------------------------------------------------------------

fn prec(e: &Expr) -> u8 {
    match e {
        Expr::Bin(op, ..) => match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 1,
            BinOp::Concat => 2,
            BinOp::Add | BinOp::Sub => 3,
            BinOp::Mul | BinOp::Div => 4,
            BinOp::Pow => 5,
        },
        Expr::Un(UnOp::Neg | UnOp::Pos, _) => 6,
        Expr::Un(UnOp::Percent, _) => 7,
        _ => 8,
    }
}

fn bin_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Pow => "^",
        BinOp::Concat => "&",
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
    }
}

fn sheet_prefix(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c as u32 >= 0x80)
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit());
    if plain {
        format!("{name}!")
    } else {
        format!("'{}'!", name.replace('\'', "''"))
    }
}

fn ref_to_string(r: &CellRef) -> String {
    let mut s = String::new();
    if let Some(sheet) = &r.sheet {
        s.push_str(&sheet_prefix(sheet));
    }
    if r.row < 0 || r.col < 0 {
        s.push_str("#REF!");
        return s;
    }
    if r.abs_col {
        s.push('$');
    }
    s.push_str(&col_name(r.col as u32));
    if r.abs_row {
        s.push('$');
    }
    s.push_str(&(r.row + 1).to_string());
    s
}

/// Print an AST back to formula text (no leading `=`), with minimal parens.
pub fn to_string(e: &Expr) -> String {
    match e {
        Expr::Num(n) => fmt_general(*n),
        Expr::Str(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        Expr::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Expr::Err(x) => x.code().to_string(),
        Expr::Missing => String::new(),
        Expr::Ref(r) => ref_to_string(r),
        Expr::Range(a, b) => format!("{}:{}", ref_to_string(a), ref_to_string(b)),
        Expr::ColRange {
            sheet,
            c1,
            c2,
            abs1,
            abs2,
        } => {
            let mut s = sheet.as_deref().map(sheet_prefix).unwrap_or_default();
            if *c1 < 0 || *c2 < 0 {
                s.push_str("#REF!");
                return s;
            }
            s.push_str(&format!(
                "{}{}:{}{}",
                if *abs1 { "$" } else { "" },
                col_name(*c1 as u32),
                if *abs2 { "$" } else { "" },
                col_name(*c2 as u32)
            ));
            s
        }
        Expr::RowRange {
            sheet,
            r1,
            r2,
            abs1,
            abs2,
        } => {
            let mut s = sheet.as_deref().map(sheet_prefix).unwrap_or_default();
            if *r1 < 0 || *r2 < 0 {
                s.push_str("#REF!");
                return s;
            }
            s.push_str(&format!(
                "{}{}:{}{}",
                if *abs1 { "$" } else { "" },
                r1 + 1,
                if *abs2 { "$" } else { "" },
                r2 + 1
            ));
            s
        }
        Expr::Name(n) => n.clone(),
        Expr::Func(name, args) => {
            let list: Vec<String> = args.iter().map(to_string).collect();
            format!("{}({})", name, list.join(","))
        }
        Expr::Un(op, x) => {
            let inner = if prec(x) < prec(e) {
                format!("({})", to_string(x))
            } else {
                to_string(x)
            };
            match op {
                UnOp::Neg => format!("-{inner}"),
                UnOp::Pos => format!("+{inner}"),
                UnOp::Percent => format!("{inner}%"),
            }
        }
        Expr::Bin(op, l, r) => {
            let lp = prec(l) < prec(e);
            // Same-precedence right operands need parens for - / ^ etc.
            let rp = prec(r) <= prec(e);
            let ls = if lp {
                format!("({})", to_string(l))
            } else {
                to_string(l)
            };
            let rs = if rp {
                format!("({})", to_string(r))
            } else {
                to_string(r)
            };
            format!("{ls}{}{rs}", bin_symbol(*op))
        }
    }
}

// ---------------------------------------------------------------------------
// Reference translation
// ---------------------------------------------------------------------------

fn translate_ref(r: &CellRef, dr: i64, dc: i64) -> CellRef {
    let mut out = r.clone();
    if !r.abs_row {
        out.row += dr;
    }
    if !r.abs_col {
        out.col += dc;
    }
    if out.row < 0
        || out.col < 0
        || out.row >= crate::sheet::MAX_ROWS as i64
        || out.col >= crate::sheet::MAX_COLS as i64
    {
        // Off-grid → poison so serialization prints #REF!.
        out.row = -1;
        out.col = -1;
    }
    out
}

/// Shift every relative reference by (dr, dc) — Excel's copy/fill semantics.
pub fn translate(e: &Expr, dr: i64, dc: i64) -> Expr {
    match e {
        Expr::Ref(r) => Expr::Ref(translate_ref(r, dr, dc)),
        Expr::Range(a, b) => Expr::Range(translate_ref(a, dr, dc), translate_ref(b, dr, dc)),
        Expr::ColRange {
            sheet,
            c1,
            c2,
            abs1,
            abs2,
        } => {
            let shift = |c: i64, abs: bool| {
                let n = if abs { c } else { c + dc };
                if (0..crate::sheet::MAX_COLS as i64).contains(&n) {
                    n
                } else {
                    -1
                }
            };
            Expr::ColRange {
                sheet: sheet.clone(),
                c1: shift(*c1, *abs1),
                c2: shift(*c2, *abs2),
                abs1: *abs1,
                abs2: *abs2,
            }
        }
        Expr::RowRange {
            sheet,
            r1,
            r2,
            abs1,
            abs2,
        } => {
            let shift = |r: i64, abs: bool| {
                let n = if abs { r } else { r + dr };
                if (0..crate::sheet::MAX_ROWS as i64).contains(&n) {
                    n
                } else {
                    -1
                }
            };
            Expr::RowRange {
                sheet: sheet.clone(),
                r1: shift(*r1, *abs1),
                r2: shift(*r2, *abs2),
                abs1: *abs1,
                abs2: *abs2,
            }
        }
        Expr::Func(n, args) => Expr::Func(
            n.clone(),
            args.iter().map(|a| translate(a, dr, dc)).collect(),
        ),
        Expr::Un(op, x) => Expr::Un(*op, Box::new(translate(x, dr, dc))),
        Expr::Bin(op, l, r) => Expr::Bin(
            *op,
            Box::new(translate(l, dr, dc)),
            Box::new(translate(r, dr, dc)),
        ),
        other => other.clone(),
    }
}

/// Parse–shift–print in one step; `None` when the source doesn't parse.
pub fn translate_formula(src: &str, dr: i64, dc: i64) -> Option<String> {
    let ast = parse(src).ok()?;
    Some(to_string(&translate(&ast, dr, dc)))
}

// ---------------------------------------------------------------------------
// Structural edits (insert / delete rows & columns)
// ---------------------------------------------------------------------------

/// A row/column insertion or deletion on one sheet, as seen by formulas.
#[derive(Clone, Copy, Debug)]
pub struct EditShift {
    /// True = rows, false = columns.
    pub rows: bool,
    /// 0-based index where the insert/delete begins.
    pub at: u32,
    /// Positive = insert this many, negative = delete this many.
    pub delta: i64,
}

/// Adjust one coordinate for the shift. `None` = the coordinate was deleted.
/// Unlike copy/paste translation, `$` anchoring is irrelevant here — Excel
/// shifts absolute references too when the grid itself moves.
fn shift_point(v: i64, shift: &EditShift) -> Option<i64> {
    let at = shift.at as i64;
    if shift.delta >= 0 {
        Some(if v >= at { v + shift.delta } else { v })
    } else {
        let n = -shift.delta;
        if v < at {
            Some(v)
        } else if v < at + n {
            None
        } else {
            Some(v - n)
        }
    }
}

/// Adjust a range endpoint pair; deletes clamp endpoints into the surviving
/// area, and a fully-deleted range becomes `None` (→ `#REF!`).
fn shift_span(a: i64, b: i64, shift: &EditShift) -> Option<(i64, i64)> {
    let at = shift.at as i64;
    let (lo, hi) = (a.min(b), a.max(b));
    let lo2 = shift_point(lo, shift).unwrap_or(at);
    let hi2 = shift_point(hi, shift).unwrap_or(at - 1);
    if lo2 > hi2 { None } else { Some((lo2, hi2)) }
}

/// Does a reference (with optional sheet qualifier) target the edited sheet?
/// `home_is_target` says whether the formula itself lives on that sheet.
fn targets(sheet: &Option<String>, home_is_target: bool, target: &str) -> bool {
    match sheet {
        None => home_is_target,
        Some(n) => n.eq_ignore_ascii_case(target),
    }
}

/// Rewrite every reference in a formula for a row/column insert or delete on
/// sheet `target`. References into deleted cells become `#REF!` — exactly
/// Excel's behavior.
pub fn adjust_for_edit(e: &Expr, home_is_target: bool, target: &str, shift: &EditShift) -> Expr {
    let recur = |x: &Expr| adjust_for_edit(x, home_is_target, target, shift);
    match e {
        Expr::Ref(r) => {
            if !targets(&r.sheet, home_is_target, target) || r.row < 0 || r.col < 0 {
                return e.clone();
            }
            let mut out = r.clone();
            let v = if shift.rows { r.row } else { r.col };
            match shift_point(v, shift) {
                Some(n) if n < axis_max(shift.rows) => {
                    if shift.rows {
                        out.row = n;
                    } else {
                        out.col = n;
                    }
                }
                _ => {
                    out.row = -1;
                    out.col = -1; // poison → #REF!
                }
            }
            Expr::Ref(out)
        }
        Expr::Range(p, q) => {
            if !targets(&p.sheet, home_is_target, target)
                || p.row < 0
                || p.col < 0
                || q.row < 0
                || q.col < 0
            {
                return e.clone();
            }
            let (mut p2, mut q2) = (p.clone(), q.clone());
            let span = if shift.rows {
                shift_span(p.row, q.row, shift)
            } else {
                shift_span(p.col, q.col, shift)
            };
            match span {
                Some((lo, hi)) if hi < axis_max(shift.rows) => {
                    if shift.rows {
                        p2.row = lo;
                        q2.row = hi;
                    } else {
                        p2.col = lo;
                        q2.col = hi;
                    }
                    Expr::Range(p2, q2)
                }
                _ => {
                    p2.row = -1;
                    p2.col = -1;
                    q2.row = -1;
                    q2.col = -1;
                    Expr::Range(p2, q2)
                }
            }
        }
        Expr::ColRange {
            sheet,
            c1,
            c2,
            abs1,
            abs2,
        } => {
            if shift.rows || !targets(sheet, home_is_target, target) || *c1 < 0 || *c2 < 0 {
                return e.clone();
            }
            match shift_span(*c1, *c2, shift) {
                Some((lo, hi)) if hi < axis_max(false) => Expr::ColRange {
                    sheet: sheet.clone(),
                    c1: lo,
                    c2: hi,
                    abs1: *abs1,
                    abs2: *abs2,
                },
                _ => Expr::ColRange {
                    sheet: sheet.clone(),
                    c1: -1,
                    c2: -1,
                    abs1: *abs1,
                    abs2: *abs2,
                },
            }
        }
        Expr::RowRange {
            sheet,
            r1,
            r2,
            abs1,
            abs2,
        } => {
            if !shift.rows || !targets(sheet, home_is_target, target) || *r1 < 0 || *r2 < 0 {
                return e.clone();
            }
            match shift_span(*r1, *r2, shift) {
                Some((lo, hi)) if hi < axis_max(true) => Expr::RowRange {
                    sheet: sheet.clone(),
                    r1: lo,
                    r2: hi,
                    abs1: *abs1,
                    abs2: *abs2,
                },
                _ => Expr::RowRange {
                    sheet: sheet.clone(),
                    r1: -1,
                    r2: -1,
                    abs1: *abs1,
                    abs2: *abs2,
                },
            }
        }
        Expr::Func(n, args) => Expr::Func(n.clone(), args.iter().map(recur).collect()),
        Expr::Un(op, x) => Expr::Un(*op, Box::new(recur(x))),
        Expr::Bin(op, l, r) => Expr::Bin(*op, Box::new(recur(l)), Box::new(recur(r))),
        other => other.clone(),
    }
}

fn axis_max(rows: bool) -> i64 {
    if rows {
        crate::sheet::MAX_ROWS as i64
    } else {
        crate::sheet::MAX_COLS as i64
    }
}

/// Parse–adjust–print for a structural edit; `None` when the source doesn't
/// parse (the caller leaves such formulas untouched).
pub fn adjust_formula_for_edit(
    src: &str,
    home_is_target: bool,
    target: &str,
    shift: &EditShift,
) -> Option<String> {
    let ast = parse(src).ok()?;
    Some(to_string(&adjust_for_edit(
        &ast,
        home_is_target,
        target,
        shift,
    )))
}

/// Rewrite sheet qualifiers after a sheet rename (Excel updates formulas on
/// rename). Returns the new text, or `None` if the source doesn't parse.
pub fn rename_sheet_in_formula(src: &str, old: &str, new: &str) -> Option<String> {
    fn walk(e: &Expr, old: &str, new: &str) -> Expr {
        let fix = |s: &Option<String>| -> Option<String> {
            match s {
                Some(n) if n.eq_ignore_ascii_case(old) => Some(new.to_string()),
                other => other.clone(),
            }
        };
        match e {
            Expr::Ref(r) => Expr::Ref(CellRef {
                sheet: fix(&r.sheet),
                ..r.clone()
            }),
            Expr::Range(a, b) => Expr::Range(
                CellRef {
                    sheet: fix(&a.sheet),
                    ..a.clone()
                },
                b.clone(),
            ),
            Expr::ColRange {
                sheet,
                c1,
                c2,
                abs1,
                abs2,
            } => Expr::ColRange {
                sheet: fix(sheet),
                c1: *c1,
                c2: *c2,
                abs1: *abs1,
                abs2: *abs2,
            },
            Expr::RowRange {
                sheet,
                r1,
                r2,
                abs1,
                abs2,
            } => Expr::RowRange {
                sheet: fix(sheet),
                r1: *r1,
                r2: *r2,
                abs1: *abs1,
                abs2: *abs2,
            },
            Expr::Func(n, args) => {
                Expr::Func(n.clone(), args.iter().map(|a| walk(a, old, new)).collect())
            }
            Expr::Un(op, x) => Expr::Un(*op, Box::new(walk(x, old, new))),
            Expr::Bin(op, l, r) => Expr::Bin(
                *op,
                Box::new(walk(l, old, new)),
                Box::new(walk(r, old, new)),
            ),
            other => other.clone(),
        }
    }
    let ast = parse(src).ok()?;
    Some(to_string(&walk(&ast, old, new)))
}

/// Collect every reference in a formula (for dependency-graph edges).
/// Ranges are reported normalized. Negative (poisoned) refs are skipped.
pub fn collect_refs(e: &Expr, out: &mut Vec<(Option<String>, u32, u32, u32, u32)>) {
    match e {
        Expr::Ref(r) => {
            if r.row >= 0 && r.col >= 0 {
                out.push((
                    r.sheet.clone(),
                    r.row as u32,
                    r.col as u32,
                    r.row as u32,
                    r.col as u32,
                ));
            }
        }
        Expr::Range(a, b) => {
            if a.row >= 0 && a.col >= 0 && b.row >= 0 && b.col >= 0 {
                let (r1, r2) = (a.row.min(b.row) as u32, a.row.max(b.row) as u32);
                let (c1, c2) = (a.col.min(b.col) as u32, a.col.max(b.col) as u32);
                out.push((a.sheet.clone(), r1, c1, r2, c2));
            }
        }
        Expr::ColRange { sheet, c1, c2, .. } => {
            if *c1 >= 0 && *c2 >= 0 {
                out.push((
                    sheet.clone(),
                    0,
                    (*c1).min(*c2) as u32,
                    crate::sheet::MAX_ROWS - 1,
                    (*c1).max(*c2) as u32,
                ));
            }
        }
        Expr::RowRange { sheet, r1, r2, .. } => {
            if *r1 >= 0 && *r2 >= 0 {
                out.push((
                    sheet.clone(),
                    (*r1).min(*r2) as u32,
                    0,
                    (*r1).max(*r2) as u32,
                    crate::sheet::MAX_COLS - 1,
                ));
            }
        }
        Expr::Func(_, args) => {
            for a in args {
                collect_refs(a, out);
            }
        }
        Expr::Un(_, x) => collect_refs(x, out),
        Expr::Bin(_, l, r) => {
            collect_refs(l, out);
            collect_refs(r, out);
        }
        _ => {}
    }
}

/// Collect every defined name referenced by a formula (the engine expands
/// them into dependency rects through the workbook).
pub fn collect_names(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Name(n) => out.push(n.clone()),
        Expr::Func(_, args) => {
            for a in args {
                collect_names(a, out);
            }
        }
        Expr::Un(_, x) => collect_names(x, out),
        Expr::Bin(_, l, r) => {
            collect_names(l, out);
            collect_names(r, out);
        }
        _ => {}
    }
}

/// Does the formula call a volatile function (must recalc on every pass)?
pub fn is_volatile(e: &Expr) -> bool {
    match e {
        Expr::Func(name, args) => {
            matches!(
                name.as_str(),
                "NOW" | "TODAY" | "RAND" | "RANDBETWEEN" | "INDIRECT" | "OFFSET"
            ) || args.iter().any(is_volatile)
        }
        Expr::Un(_, x) => is_volatile(x),
        Expr::Bin(_, l, r) => is_volatile(l) || is_volatile(r),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// What the evaluator needs from the outside world. Implemented by the recalc
/// engine over a workbook; tests implement it over simple maps.
pub trait Resolver {
    /// Value of a cell (already recalculated if it holds a formula).
    fn value(&self, sheet: usize, row: u32, col: u32) -> Value;
    /// Sheet name → index, case-insensitive.
    fn sheet_index(&self, name: &str) -> Option<usize>;
    /// Non-empty cells within the rect, row-major.
    fn cells_in(
        &self,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    ) -> Vec<((u32, u32), Value)>;
    /// The current moment as an Excel date serial (date + time-of-day
    /// fraction), if a clock is available. `TODAY()` floors it.
    fn today(&self) -> Option<f64> {
        None
    }
    /// Uniform random in [0, 1), if a randomness source is available.
    fn rand(&self) -> Option<f64> {
        None
    }
    /// The workbook's date system, for date functions.
    fn date1904(&self) -> bool {
        false
    }
    /// (rows, cols) of a sheet's used range — whole-column/row references
    /// clamp their dense iteration to this. The default (grid maximum) means
    /// "no clamping".
    fn used_size(&self, sheet: usize) -> (u32, u32) {
        let _ = sheet;
        (crate::sheet::MAX_ROWS, crate::sheet::MAX_COLS)
    }
    /// The definition text of a defined name (a formula body, e.g.
    /// `Sheet1!$A$1:$B$5`), preferring a name scoped to `current_sheet`.
    fn defined_name(&self, name: &str, current_sheet: usize) -> Option<String> {
        let _ = (name, current_sheet);
        None
    }
}

/// One evaluation: tracks the current sheet/cell (for `ROW()`, sheet-less
/// refs) and whether anything unsupported was hit.
pub struct Eval<'a> {
    pub res: &'a dyn Resolver,
    pub sheet: usize,
    pub cell: (u32, u32),
    /// Set when the formula used something we don't model (unknown function,
    /// unresolvable name, missing clock…). The engine then keeps the cached
    /// value.
    pub unsupported: bool,
    /// Defined-name expansion depth (guards against name→name cycles).
    depth: u32,
}

/// An evaluated argument: scalar, or a still-lazy range.
enum Arg {
    Scalar(Value),
    Range(usize, u32, u32, u32, u32),
}

impl<'a> Eval<'a> {
    pub fn new(res: &'a dyn Resolver, sheet: usize, cell: (u32, u32)) -> Self {
        Eval {
            res,
            sheet,
            cell,
            unsupported: false,
            depth: 0,
        }
    }

    pub fn eval(&mut self, e: &Expr) -> Value {
        match self.eval_arg(e) {
            Arg::Scalar(v) => v,
            // A bare range in scalar context (legacy implicit intersection):
            // a 1×1 range collapses; anything else is #VALUE!.
            Arg::Range(s, r1, c1, r2, c2) => {
                if r1 == r2 && c1 == c2 {
                    self.res.value(s, r1, c1)
                } else {
                    Value::Err(ExcelError::Value)
                }
            }
        }
    }

    fn resolve_sheet(&mut self, name: &Option<String>) -> Result<usize, Value> {
        match name {
            None => Ok(self.sheet),
            Some(n) => match self.res.sheet_index(n) {
                Some(i) => Ok(i),
                None => Err(Value::Err(ExcelError::Ref)),
            },
        }
    }

    fn eval_arg(&mut self, e: &Expr) -> Arg {
        match e {
            Expr::Num(n) => Arg::Scalar(Value::Num(*n)),
            Expr::Str(s) => Arg::Scalar(Value::Str(s.clone())),
            Expr::Bool(b) => Arg::Scalar(Value::Bool(*b)),
            Expr::Err(x) => Arg::Scalar(Value::Err(*x)),
            Expr::Missing => Arg::Scalar(Value::Empty),
            Expr::Name(n) => {
                // A defined name expands to its definition formula. Depth cap
                // guards name→name cycles (Excel rejects those at entry).
                match self.res.defined_name(n, self.sheet) {
                    Some(def) if self.depth < 32 => match parse(&def) {
                        Ok(ast) => {
                            self.depth += 1;
                            let arg = self.eval_arg(&ast);
                            self.depth -= 1;
                            arg
                        }
                        Err(_) => {
                            self.unsupported = true;
                            Arg::Scalar(Value::Err(ExcelError::Name))
                        }
                    },
                    Some(_) => Arg::Scalar(Value::Err(ExcelError::Name)),
                    None => {
                        self.unsupported = true;
                        Arg::Scalar(Value::Err(ExcelError::Name))
                    }
                }
            }
            Expr::ColRange { sheet, c1, c2, .. } => {
                if *c1 < 0 || *c2 < 0 {
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                }
                match self.resolve_sheet(sheet) {
                    Ok(s) => Arg::Range(
                        s,
                        0,
                        (*c1).min(*c2) as u32,
                        crate::sheet::MAX_ROWS - 1,
                        (*c1).max(*c2) as u32,
                    ),
                    Err(v) => Arg::Scalar(v),
                }
            }
            Expr::RowRange { sheet, r1, r2, .. } => {
                if *r1 < 0 || *r2 < 0 {
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                }
                match self.resolve_sheet(sheet) {
                    Ok(s) => Arg::Range(
                        s,
                        (*r1).min(*r2) as u32,
                        0,
                        (*r1).max(*r2) as u32,
                        crate::sheet::MAX_COLS - 1,
                    ),
                    Err(v) => Arg::Scalar(v),
                }
            }
            Expr::Ref(r) => {
                if r.row < 0 || r.col < 0 {
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                }
                match self.resolve_sheet(&r.sheet) {
                    Ok(s) => Arg::Scalar(self.res.value(s, r.row as u32, r.col as u32)),
                    Err(v) => Arg::Scalar(v),
                }
            }
            Expr::Range(a, b) => {
                if a.row < 0 || a.col < 0 || b.row < 0 || b.col < 0 {
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                }
                match self.resolve_sheet(&a.sheet) {
                    Ok(s) => Arg::Range(
                        s,
                        a.row.min(b.row) as u32,
                        a.col.min(b.col) as u32,
                        a.row.max(b.row) as u32,
                        a.col.max(b.col) as u32,
                    ),
                    Err(v) => Arg::Scalar(v),
                }
            }
            Expr::Un(op, x) => {
                let v = self.eval(x);
                Arg::Scalar(match op {
                    UnOp::Neg => match to_num(&v) {
                        Ok(n) => num(-n),
                        Err(e) => Value::Err(e),
                    },
                    UnOp::Pos => v,
                    UnOp::Percent => match to_num(&v) {
                        Ok(n) => num(n / 100.0),
                        Err(e) => Value::Err(e),
                    },
                })
            }
            Expr::Bin(op, l, r) => {
                let lv = self.eval(l);
                let rv = self.eval(r);
                Arg::Scalar(bin_op(*op, &lv, &rv))
            }
            // INDIRECT and OFFSET can *return references*, so they are
            // resolved here where a range result is expressible. Both are
            // volatile (the dependency graph can't see through them).
            Expr::Func(name, args) if name == "INDIRECT" => self.indirect(args),
            Expr::Func(name, args) if name == "OFFSET" => self.offset(args),
            Expr::Func(name, args) => Arg::Scalar(self.call(name, args)),
        }
    }

    /// `INDIRECT(ref_text)` — build a reference from a string at runtime.
    fn indirect(&mut self, args: &[Expr]) -> Arg {
        if args.is_empty() || args.len() > 2 {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        if let Some(e) = args.get(1) {
            // Only A1-style (the default). R1C1 requests are unsupported.
            match to_bool(&self.eval(e)) {
                Ok(true) => {}
                Ok(false) => {
                    self.unsupported = true;
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                }
                Err(er) => return Arg::Scalar(Value::Err(er)),
            }
        }
        let text = match to_text(&self.eval(&args[0])) {
            Ok(t) => t,
            Err(er) => return Arg::Scalar(Value::Err(er)),
        };
        // Parse the text as a reference expression; only refs/ranges qualify.
        match parse(&text) {
            Ok(
                ast @ (Expr::Ref(_)
                | Expr::Range(..)
                | Expr::ColRange { .. }
                | Expr::RowRange { .. }),
            ) => self.eval_arg(&ast),
            Ok(Expr::Name(_)) if self.depth < 32 => {
                self.depth += 1;
                let arg = self.eval_arg(&Expr::Name(text));
                self.depth -= 1;
                arg
            }
            _ => Arg::Scalar(Value::Err(ExcelError::Ref)),
        }
    }

    /// `OFFSET(reference, rows, cols, [height], [width])`.
    fn offset(&mut self, args: &[Expr]) -> Arg {
        if args.len() < 3 || args.len() > 5 {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        let (s, r1, c1, r2, c2) = match &args[0] {
            // A bare cell ref means the *position*, not the value.
            Expr::Ref(r) if r.row >= 0 && r.col >= 0 => match self.resolve_sheet(&r.sheet) {
                Ok(s) => (s, r.row as u32, r.col as u32, r.row as u32, r.col as u32),
                Err(v) => return Arg::Scalar(v),
            },
            e => match self.eval_arg(e) {
                Arg::Range(s, a, b, c, d) => (s, a, b, c, d),
                Arg::Scalar(v) => {
                    return Arg::Scalar(if v.is_err() {
                        v
                    } else {
                        Value::Err(ExcelError::Value)
                    });
                }
            },
        };
        let dr = match to_num(&self.eval(&args[1])) {
            Ok(n) => n.trunc() as i64,
            Err(e) => return Arg::Scalar(Value::Err(e)),
        };
        let dc = match to_num(&self.eval(&args[2])) {
            Ok(n) => n.trunc() as i64,
            Err(e) => return Arg::Scalar(Value::Err(e)),
        };
        let height = match args.get(3) {
            None | Some(Expr::Missing) => (r2 - r1 + 1) as i64,
            Some(e) => match to_num(&self.eval(e)) {
                Ok(n) => n.trunc() as i64,
                Err(er) => return Arg::Scalar(Value::Err(er)),
            },
        };
        let width = match args.get(4) {
            None | Some(Expr::Missing) => (c2 - c1 + 1) as i64,
            Some(e) => match to_num(&self.eval(e)) {
                Ok(n) => n.trunc() as i64,
                Err(er) => return Arg::Scalar(Value::Err(er)),
            },
        };
        let nr = r1 as i64 + dr;
        let nc = c1 as i64 + dc;
        if height < 1
            || width < 1
            || nr < 0
            || nc < 0
            || nr + height > crate::sheet::MAX_ROWS as i64
            || nc + width > crate::sheet::MAX_COLS as i64
        {
            return Arg::Scalar(Value::Err(ExcelError::Ref));
        }
        Arg::Range(
            s,
            nr as u32,
            nc as u32,
            (nr + height - 1) as u32,
            (nc + width - 1) as u32,
        )
    }

    /// Clamp a rect to the sheet's used range for dense iteration (whole-row/
    /// column references would otherwise walk a million cells).
    fn clamp(&self, s: usize, r1: u32, c1: u32, r2: u32, c2: u32) -> (u32, u32, u32, u32) {
        let (rows, cols) = self.res.used_size(s);
        (
            r1,
            c1,
            r2.min(rows.saturating_sub(1).max(r1)),
            c2.min(cols.saturating_sub(1).max(c1)),
        )
    }
}

// --- coercions -------------------------------------------------------------

/// Numeric coercion (Excel VALUE-style): empty → 0, bool → 0/1, numeric text
/// parses, other text → #VALUE!.
pub fn to_num(v: &Value) -> Result<f64, ExcelError> {
    match v {
        Value::Num(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Empty => Ok(0.0),
        Value::Err(e) => Err(*e),
        Value::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(ExcelError::Value);
            }
            if let Some(pct) = t.strip_suffix('%') {
                if let Ok(n) = pct.trim().parse::<f64>() {
                    return Ok(n / 100.0);
                }
            }
            t.parse::<f64>().map_err(|_| ExcelError::Value)
        }
    }
}

/// Text coercion: numbers via General format, TRUE/FALSE, empty → "".
pub fn to_text(v: &Value) -> Result<String, ExcelError> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::Num(n) => Ok(fmt_general(*n)),
        Value::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
        Value::Empty => Ok(String::new()),
        Value::Err(e) => Err(*e),
    }
}

/// Boolean coercion: numbers ≠ 0, "TRUE"/"FALSE" text, empty → false.
pub fn to_bool(v: &Value) -> Result<bool, ExcelError> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Num(n) => Ok(*n != 0.0),
        Value::Empty => Ok(false),
        Value::Err(e) => Err(*e),
        Value::Str(s) => {
            if s.eq_ignore_ascii_case("TRUE") {
                Ok(true)
            } else if s.eq_ignore_ascii_case("FALSE") {
                Ok(false)
            } else {
                Err(ExcelError::Value)
            }
        }
    }
}

/// Excel's comparison: case-insensitive text; cross-type ordering
/// Number < Text < Logical; empty coerces to the other side's zero value.
fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering, ExcelError> {
    use std::cmp::Ordering;
    if let Value::Err(e) = a {
        return Err(*e);
    }
    if let Value::Err(e) = b {
        return Err(*e);
    }
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Num(_) => 0,
            Value::Str(_) => 1,
            Value::Bool(_) => 2,
            _ => 0,
        }
    }
    let (a2, b2): (Value, Value) = match (a, b) {
        (Value::Empty, Value::Empty) => return Ok(Ordering::Equal),
        (Value::Empty, other) => (zero_of(other), other.clone()),
        (other, Value::Empty) => (other.clone(), zero_of(other)),
        _ => (a.clone(), b.clone()),
    };
    Ok(match (&a2, &b2) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => {
            let xl = x.to_lowercase();
            let yl = y.to_lowercase();
            xl.cmp(&yl)
        }
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => rank(&a2).cmp(&rank(&b2)),
    })
}

fn zero_of(v: &Value) -> Value {
    match v {
        Value::Str(_) => Value::Str(String::new()),
        Value::Bool(_) => Value::Bool(false),
        _ => Value::Num(0.0),
    }
}

fn bin_op(op: BinOp, l: &Value, r: &Value) -> Value {
    use std::cmp::Ordering;
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
            let a = match to_num(l) {
                Ok(n) => n,
                Err(e) => return Value::Err(e),
            };
            let b = match to_num(r) {
                Ok(n) => n,
                Err(e) => return Value::Err(e),
            };
            match op {
                BinOp::Add => num(a + b),
                BinOp::Sub => num(a - b),
                BinOp::Mul => num(a * b),
                BinOp::Div => {
                    if b == 0.0 {
                        Value::Err(ExcelError::Div0)
                    } else {
                        num(a / b)
                    }
                }
                BinOp::Pow => {
                    if a == 0.0 && b == 0.0 {
                        Value::Err(ExcelError::Num)
                    } else {
                        num(a.powf(b))
                    }
                }
                _ => unreachable!(),
            }
        }
        BinOp::Concat => {
            let a = match to_text(l) {
                Ok(s) => s,
                Err(e) => return Value::Err(e),
            };
            let b = match to_text(r) {
                Ok(s) => s,
                Err(e) => return Value::Err(e),
            };
            Value::Str(a + &b)
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            match compare(l, r) {
                Err(e) => Value::Err(e),
                Ok(ord) => Value::Bool(match op {
                    BinOp::Eq => ord == Ordering::Equal,
                    BinOp::Ne => ord != Ordering::Equal,
                    BinOp::Lt => ord == Ordering::Less,
                    BinOp::Le => ord != Ordering::Greater,
                    BinOp::Gt => ord == Ordering::Greater,
                    BinOp::Ge => ord != Ordering::Less,
                    _ => unreachable!(),
                }),
            }
        }
    }
}

// --- criteria (COUNTIF / SUMIF / AVERAGEIF) ---------------------------------

struct Criteria {
    op: BinOp,
    val: Value,
}

fn parse_criteria(v: &Value) -> Criteria {
    if let Value::Str(s) = v {
        let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
            (BinOp::Ge, r)
        } else if let Some(r) = s.strip_prefix("<=") {
            (BinOp::Le, r)
        } else if let Some(r) = s.strip_prefix("<>") {
            (BinOp::Ne, r)
        } else if let Some(r) = s.strip_prefix('>') {
            (BinOp::Gt, r)
        } else if let Some(r) = s.strip_prefix('<') {
            (BinOp::Lt, r)
        } else if let Some(r) = s.strip_prefix('=') {
            (BinOp::Eq, r)
        } else {
            (BinOp::Eq, s.as_str())
        };
        let val = match rest.trim().parse::<f64>() {
            Ok(n) => Value::Num(n),
            Err(_) => {
                if rest.eq_ignore_ascii_case("TRUE") {
                    Value::Bool(true)
                } else if rest.eq_ignore_ascii_case("FALSE") {
                    Value::Bool(false)
                } else {
                    Value::Str(rest.to_string())
                }
            }
        };
        return Criteria { op, val };
    }
    Criteria {
        op: BinOp::Eq,
        val: v.clone(),
    }
}

/// `*` / `?` wildcards, `~` escapes — case-insensitive.
fn wildcard_match(pat: &str, text: &str) -> bool {
    fn inner(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            '*' => (0..=t.len()).any(|k| inner(&p[1..], &t[k..])),
            '?' => !t.is_empty() && inner(&p[1..], &t[1..]),
            '~' if p.len() > 1 => {
                !t.is_empty() && p[1].eq_ignore_ascii_case(&t[0]) && inner(&p[2..], &t[1..])
            }
            c => !t.is_empty() && c.eq_ignore_ascii_case(&t[0]) && inner(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    inner(&p, &t)
}

fn criteria_match(c: &Criteria, v: &Value) -> bool {
    // Blank cells match nothing except an explicitly empty criteria.
    if matches!(v, Value::Empty) {
        return matches!(&c.val, Value::Str(s) if s.is_empty()) && c.op == BinOp::Eq;
    }
    if let (BinOp::Eq | BinOp::Ne, Value::Str(pat), Value::Str(text)) = (c.op, &c.val, v) {
        if pat.contains(['*', '?']) {
            let hit = wildcard_match(pat, text);
            return if c.op == BinOp::Eq { hit } else { !hit };
        }
    }
    // Type-mismatched comparisons never match in criteria context (unlike
    // plain comparison operators, which order across types).
    let same_type = matches!(
        (&c.val, v),
        (Value::Num(_), Value::Num(_))
            | (Value::Str(_), Value::Str(_))
            | (Value::Bool(_), Value::Bool(_))
    );
    if !same_type {
        return false;
    }
    match compare(v, &c.val) {
        Ok(ord) => match c.op {
            BinOp::Eq => ord == std::cmp::Ordering::Equal,
            BinOp::Ne => ord != std::cmp::Ordering::Equal,
            BinOp::Lt => ord == std::cmp::Ordering::Less,
            BinOp::Le => ord != std::cmp::Ordering::Greater,
            BinOp::Gt => ord == std::cmp::Ordering::Greater,
            BinOp::Ge => ord != std::cmp::Ordering::Less,
            _ => false,
        },
        Err(_) => false,
    }
}

// --- function library --------------------------------------------------------

macro_rules! try_num {
    ($e:expr) => {
        match to_num(&$e) {
            Ok(n) => n,
            Err(er) => return Value::Err(er),
        }
    };
}
macro_rules! try_text {
    ($e:expr) => {
        match to_text(&$e) {
            Ok(s) => s,
            Err(er) => return Value::Err(er),
        }
    };
}
macro_rules! try_bool {
    ($e:expr) => {
        match to_bool(&$e) {
            Ok(b) => b,
            Err(er) => return Value::Err(er),
        }
    };
}

impl<'a> Eval<'a> {
    /// Every value inside range args plus scalars; used by aggregate helpers.
    /// `numbers_only` implements the SUM rule: range text/bools are skipped,
    /// while *direct* scalar args coerce (SUM("2",TRUE) = 3).
    fn collect_values(
        &mut self,
        args: &[Expr],
        numbers_only: bool,
    ) -> Result<Vec<f64>, ExcelError> {
        let mut out = Vec::new();
        for a in args {
            match self.eval_arg(a) {
                Arg::Scalar(Value::Err(e)) => return Err(e),
                Arg::Scalar(Value::Empty) => {}
                Arg::Scalar(v) => match to_num(&v) {
                    Ok(n) => out.push(n),
                    Err(e) => return Err(e),
                },
                Arg::Range(s, r1, c1, r2, c2) => {
                    for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                        match v {
                            Value::Num(n) => out.push(n),
                            Value::Err(e) => return Err(e),
                            Value::Bool(b) if !numbers_only => out.push(if b { 1.0 } else { 0.0 }),
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// A range argument or an error value (scalars don't qualify).
    fn arg_range(&mut self, e: &Expr) -> Result<(usize, u32, u32, u32, u32), Value> {
        match self.eval_arg(e) {
            Arg::Range(s, a, b, c, d) => Ok((s, a, b, c, d)),
            Arg::Scalar(v) => Err(if v.is_err() {
                v
            } else {
                Value::Err(ExcelError::Value)
            }),
        }
    }

    /// A 1-D range (single row or column) read densely, clamped to the used
    /// range — the shape XLOOKUP/LOOKUP/MATCH vectors want.
    fn arg_vector(&mut self, e: &Expr) -> Result<(usize, Vec<(u32, u32)>), Value> {
        let (s, r1, c1, r2, c2) = self.arg_range(e)?;
        let (r1, c1, r2, c2) = self.clamp(s, r1, c1, r2, c2);
        if r1 != r2 && c1 != c2 {
            return Err(Value::Err(ExcelError::Value));
        }
        let mut cells = Vec::new();
        if c1 == c2 {
            for r in r1..=r2 {
                cells.push((r, c1));
            }
        } else {
            for c in c1..=c2 {
                cells.push((r1, c));
            }
        }
        Ok((s, cells))
    }

    /// The multi-criteria core of SUMIFS/COUNTIFS/AVERAGEIFS/MAXIFS/MINIFS:
    /// `pairs` is (criteria_range, criteria)+; `agg_range` supplies the values
    /// aggregated for matching offsets (None = count only).
    fn ifs_family(
        &mut self,
        agg_range: Option<&Expr>,
        pairs: &[Expr],
    ) -> Result<(usize, Vec<f64>), Value> {
        if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
            return Err(Value::Err(ExcelError::Value));
        }
        let mut rects = Vec::new();
        let mut crits = Vec::new();
        for chunk in pairs.chunks(2) {
            rects.push(self.arg_range(&chunk[0])?);
            crits.push(parse_criteria(&self.eval(&chunk[1])));
        }
        let agg = match agg_range {
            Some(e) => Some(self.arg_range(e)?),
            None => None,
        };
        // All ranges must be the same shape.
        let shape = |r: &(usize, u32, u32, u32, u32)| (r.3 - r.1, r.4 - r.2);
        let base_shape = shape(&rects[0]);
        if rects.iter().any(|r| shape(r) != base_shape)
            || agg.as_ref().is_some_and(|r| shape(r) != base_shape)
        {
            return Err(Value::Err(ExcelError::Value));
        }
        let (s0, br1, bc1, br2, bc2) = rects[0];
        let (br1, bc1, br2, bc2) = self.clamp(s0, br1, bc1, br2, bc2);
        let mut count = 0usize;
        let mut matched = Vec::new();
        for r in br1..=br2 {
            for c in bc1..=bc2 {
                let (dr, dc) = (r - rects[0].1, c - rects[0].2);
                let hit = rects.iter().zip(&crits).all(|(&(s, r1, c1, _, _), crit)| {
                    criteria_match(crit, &self.res.value(s, r1 + dr, c1 + dc))
                });
                if hit {
                    count += 1;
                    if let Some((sa, ar1, ac1, _, _)) = agg {
                        if let Value::Num(n) = self.res.value(sa, ar1 + dr, ac1 + dc) {
                            matched.push(n);
                        }
                    }
                }
            }
        }
        Ok((count, matched))
    }

    /// Evaluate a (range, criteria[, range2]) trio shared by COUNTIF/SUMIF/
    /// AVERAGEIF. Returns matched values from the sum range.
    fn eval_if_ranges(
        &mut self,
        range: &Expr,
        criteria: &Expr,
        sum_range: Option<&Expr>,
    ) -> Result<(usize, Vec<f64>), Value> {
        let (s, r1, c1, r2, c2) = match self.eval_arg(range) {
            Arg::Range(s, r1, c1, r2, c2) => (s, r1, c1, r2, c2),
            Arg::Scalar(v) => {
                return Err(if v.is_err() {
                    v
                } else {
                    Value::Err(ExcelError::Value)
                });
            }
        };
        let (ss, sr1, sc1) = match sum_range {
            None => (s, r1, c1),
            Some(e) => match self.eval_arg(e) {
                Arg::Range(ss, a, b, _, _) => (ss, a, b),
                Arg::Scalar(v) => {
                    return Err(if v.is_err() {
                        v
                    } else {
                        Value::Err(ExcelError::Value)
                    });
                }
            },
        };
        let crit = parse_criteria(&self.eval(criteria));
        let mut count = 0usize;
        let mut matched = Vec::new();
        let (r1, c1, r2, c2) = self.clamp(s, r1, c1, r2, c2);
        for r in r1..=r2 {
            for c in c1..=c2 {
                let v = self.res.value(s, r, c);
                if criteria_match(&crit, &v) {
                    count += 1;
                    let sv = if sum_range.is_none() {
                        v
                    } else {
                        self.res.value(ss, sr1 + (r - r1), sc1 + (c - c1))
                    };
                    if let Value::Num(n) = sv {
                        matched.push(n);
                    }
                }
            }
        }
        Ok((count, matched))
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> Value {
        match name {
            // ---- math -------------------------------------------------
            "SUM" => match self.collect_values(args, true) {
                Ok(v) => num(v.iter().sum()),
                Err(e) => Value::Err(e),
            },
            "PRODUCT" => match self.collect_values(args, true) {
                Ok(v) => num(v.iter().product()),
                Err(e) => Value::Err(e),
            },
            "ABS" => self.one_num(args, |n| num(n.abs())),
            "SIGN" => self.one_num(args, |n| {
                Value::Num(if n > 0.0 {
                    1.0
                } else if n < 0.0 {
                    -1.0
                } else {
                    0.0
                })
            }),
            "INT" => self.one_num(args, |n| num(n.floor())),
            "TRUNC" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let d = if args.len() == 2 {
                    try_num!(self.eval(&args[1])).trunc() as i32
                } else {
                    0
                };
                let f = 10f64.powi(d);
                num((n * f).trunc() / f)
            }
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let d = try_num!(self.eval(&args[1])).trunc() as i32;
                let f = 10f64.powi(d);
                let scaled = n * f;
                let r = match name {
                    "ROUND" => scaled.abs().round() * scaled.signum(),
                    "ROUNDUP" => scaled.abs().ceil() * scaled.signum(),
                    _ => scaled.abs().floor() * scaled.signum(),
                };
                num(r / f)
            }
            "SQRT" => self.one_num(args, |n| {
                if n < 0.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    num(n.sqrt())
                }
            }),
            "EXP" => self.one_num(args, |n| num(n.exp())),
            "LN" => self.one_num(args, |n| {
                if n <= 0.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    num(n.ln())
                }
            }),
            "LOG10" => self.one_num(args, |n| {
                if n <= 0.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    num(n.log10())
                }
            }),
            "LOG" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let base = if args.len() == 2 {
                    try_num!(self.eval(&args[1]))
                } else {
                    10.0
                };
                if n <= 0.0 || base <= 0.0 || base == 1.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    num(n.log(base))
                }
            }
            "POWER" => self.two_num(args, |a, b| {
                if a == 0.0 && b == 0.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    num(a.powf(b))
                }
            }),
            "MOD" => self.two_num(args, |a, b| {
                if b == 0.0 {
                    Value::Err(ExcelError::Div0)
                } else {
                    num(a - b * (a / b).floor())
                }
            }),
            "QUOTIENT" => self.two_num(args, |a, b| {
                if b == 0.0 {
                    Value::Err(ExcelError::Div0)
                } else {
                    num((a / b).trunc())
                }
            }),
            "PI" => {
                if args.is_empty() {
                    Value::Num(std::f64::consts::PI)
                } else {
                    Value::Err(ExcelError::Value)
                }
            }
            "DEGREES" => self.one_num(args, |n| num(n.to_degrees())),
            "RADIANS" => self.one_num(args, |n| num(n.to_radians())),
            "SIN" => self.one_num(args, |n| num(n.sin())),
            "COS" => self.one_num(args, |n| num(n.cos())),
            "TAN" => self.one_num(args, |n| num(n.tan())),
            "ASIN" => self.one_num(args, |n| num(n.asin())),
            "ACOS" => self.one_num(args, |n| num(n.acos())),
            "ATAN" => self.one_num(args, |n| num(n.atan())),
            "ATAN2" => self.two_num(args, |x, y| {
                if x == 0.0 && y == 0.0 {
                    Value::Err(ExcelError::Div0)
                } else {
                    num(y.atan2(x))
                }
            }),
            "SINH" => self.one_num(args, |n| num(n.sinh())),
            "COSH" => self.one_num(args, |n| num(n.cosh())),
            "TANH" => self.one_num(args, |n| num(n.tanh())),
            "FLOOR" | "FLOOR.MATH" => self.two_num(args, |n, sig| {
                if sig == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                // Classic FLOOR requires matching signs.
                if n * sig < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((n / sig).floor() * sig)
            }),
            "CEILING" | "CEILING.MATH" => self.two_num(args, |n, sig| {
                if sig == 0.0 {
                    return Value::Num(0.0);
                }
                if n > 0.0 && sig < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((n / sig).ceil() * sig)
            }),
            "EVEN" => self.one_num(args, |n| {
                let r = (n.abs() / 2.0).ceil() * 2.0;
                num(r * if n < 0.0 { -1.0 } else { 1.0 })
            }),
            "ODD" => self.one_num(args, |n| {
                let a = n.abs();
                let mut r = a.ceil();
                if r % 2.0 == 0.0 {
                    r += 1.0;
                }
                if r < 1.0 {
                    r = 1.0;
                }
                num(r * if n < 0.0 { -1.0 } else { 1.0 })
            }),
            "FACT" => self.one_num(args, |n| {
                if !(0.0..=170.0).contains(&n) {
                    return Value::Err(ExcelError::Num);
                }
                let mut r = 1.0;
                for i in 2..=(n.trunc() as u64) {
                    r *= i as f64;
                }
                num(r)
            }),
            "GCD" => match self.collect_values(args, true) {
                Ok(v) => {
                    let mut g: u64 = 0;
                    for n in v {
                        if n < 0.0 {
                            return Value::Err(ExcelError::Num);
                        }
                        g = gcd(g, n.trunc() as u64);
                    }
                    Value::Num(g as f64)
                }
                Err(e) => Value::Err(e),
            },
            "LCM" => match self.collect_values(args, true) {
                Ok(v) => {
                    let mut l: u64 = 1;
                    for n in v {
                        if n < 0.0 {
                            return Value::Err(ExcelError::Num);
                        }
                        let k = n.trunc() as u64;
                        if k == 0 {
                            return Value::Num(0.0);
                        }
                        l = l / gcd(l, k) * k;
                    }
                    Value::Num(l as f64)
                }
                Err(e) => Value::Err(e),
            },
            "SUMIF" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                match self.eval_if_ranges(&args[0], &args[1], args.get(2)) {
                    Ok((_, vals)) => num(vals.iter().sum()),
                    Err(v) => v,
                }
            }
            "AVERAGEIF" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                match self.eval_if_ranges(&args[0], &args[1], args.get(2)) {
                    Ok((_, vals)) if vals.is_empty() => Value::Err(ExcelError::Div0),
                    Ok((_, vals)) => num(vals.iter().sum::<f64>() / vals.len() as f64),
                    Err(v) => v,
                }
            }
            "COUNTIF" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                match self.eval_if_ranges(&args[0], &args[1], None) {
                    Ok((count, _)) => Value::Num(count as f64),
                    Err(v) => v,
                }
            }
            "SUMPRODUCT" => {
                // Same-shape ranges only (the overwhelmingly common form);
                // scalar/array-expression factors arrive with dynamic arrays.
                let mut rects = Vec::new();
                for a in args {
                    match self.eval_arg(a) {
                        Arg::Range(s, r1, c1, r2, c2) => {
                            let (r1, c1, r2, c2) = self.clamp(s, r1, c1, r2, c2);
                            rects.push((s, r1, c1, r2, c2));
                        }
                        Arg::Scalar(Value::Err(e)) => return Value::Err(e),
                        Arg::Scalar(_) => return Value::Err(ExcelError::Value),
                    }
                }
                if rects.is_empty() {
                    return Value::Err(ExcelError::Value);
                }
                let (rows, cols) = (rects[0].3 - rects[0].1 + 1, rects[0].4 - rects[0].2 + 1);
                for r in &rects {
                    if r.3 - r.1 + 1 != rows || r.4 - r.2 + 1 != cols {
                        return Value::Err(ExcelError::Value);
                    }
                }
                let mut total = 0.0;
                for dr in 0..rows {
                    for dc in 0..cols {
                        let mut p = 1.0;
                        for &(s, r1, c1, _, _) in &rects {
                            let v = self.res.value(s, r1 + dr, c1 + dc);
                            p *= match v {
                                Value::Num(n) => n,
                                Value::Err(e) => return Value::Err(e),
                                _ => 0.0, // text/bool/empty count as 0
                            };
                        }
                        total += p;
                    }
                }
                num(total)
            }
            "RAND" => match self.res.rand() {
                Some(r) => Value::Num(r),
                None => {
                    self.unsupported = true;
                    Value::Err(ExcelError::Value)
                }
            },
            "RANDBETWEEN" => {
                let (a, b) = match (args.first(), args.get(1)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return Value::Err(ExcelError::Value),
                };
                let lo = try_num!(self.eval(a)).ceil();
                let hi = try_num!(self.eval(b)).floor();
                if lo > hi {
                    return Value::Err(ExcelError::Num);
                }
                match self.res.rand() {
                    Some(r) => Value::Num(lo + (r * (hi - lo + 1.0)).floor()),
                    None => {
                        self.unsupported = true;
                        Value::Err(ExcelError::Value)
                    }
                }
            }

            // ---- statistics --------------------------------------------
            "AVERAGE" => match self.collect_values(args, true) {
                Ok(v) if v.is_empty() => Value::Err(ExcelError::Div0),
                Ok(v) => num(v.iter().sum::<f64>() / v.len() as f64),
                Err(e) => Value::Err(e),
            },
            "MIN" | "MAX" => match self.collect_values(args, true) {
                Ok(v) if v.is_empty() => Value::Num(0.0),
                Ok(v) => {
                    let r = v.iter().copied().fold(
                        if name == "MIN" { f64::MAX } else { f64::MIN },
                        |a, b| {
                            if name == "MIN" { a.min(b) } else { a.max(b) }
                        },
                    );
                    Value::Num(r)
                }
                Err(e) => Value::Err(e),
            },
            "MEDIAN" => match self.collect_values(args, true) {
                Ok(mut v) if !v.is_empty() => {
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let mid = v.len() / 2;
                    if v.len() % 2 == 1 {
                        Value::Num(v[mid])
                    } else {
                        Value::Num((v[mid - 1] + v[mid]) / 2.0)
                    }
                }
                Ok(_) => Value::Err(ExcelError::Num),
                Err(e) => Value::Err(e),
            },
            "LARGE" | "SMALL" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let k = try_num!(self.eval(&args[1])).trunc() as usize;
                match self.collect_values(&args[..1], true) {
                    Ok(mut v) => {
                        if k == 0 || k > v.len() {
                            return Value::Err(ExcelError::Num);
                        }
                        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        Value::Num(if name == "SMALL" {
                            v[k - 1]
                        } else {
                            v[v.len() - k]
                        })
                    }
                    Err(e) => Value::Err(e),
                }
            }
            "STDEV" | "STDEV.S" | "STDEVP" | "STDEV.P" | "VAR" | "VAR.S" | "VARP" | "VAR.P" => {
                match self.collect_values(args, true) {
                    Ok(v) => {
                        let population = name.contains('P');
                        let denom = if population {
                            v.len()
                        } else {
                            v.len().saturating_sub(1)
                        };
                        if denom == 0 {
                            return Value::Err(ExcelError::Div0);
                        }
                        let mean = v.iter().sum::<f64>() / v.len() as f64;
                        let var =
                            v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / denom as f64;
                        if name.starts_with("VAR") {
                            num(var)
                        } else {
                            num(var.sqrt())
                        }
                    }
                    Err(e) => Value::Err(e),
                }
            }
            "COUNT" => {
                let mut n = 0usize;
                for a in args {
                    match self.eval_arg(a) {
                        Arg::Scalar(v) => {
                            if to_num(&v).is_ok() && !matches!(v, Value::Empty) {
                                n += 1;
                            }
                        }
                        Arg::Range(s, r1, c1, r2, c2) => {
                            n += self
                                .res
                                .cells_in(s, r1, c1, r2, c2)
                                .iter()
                                .filter(|(_, v)| matches!(v, Value::Num(_)))
                                .count();
                        }
                    }
                }
                Value::Num(n as f64)
            }
            "COUNTA" => {
                let mut n = 0usize;
                for a in args {
                    match self.eval_arg(a) {
                        Arg::Scalar(Value::Empty) => {}
                        Arg::Scalar(_) => n += 1,
                        Arg::Range(s, r1, c1, r2, c2) => {
                            n += self.res.cells_in(s, r1, c1, r2, c2).len();
                        }
                    }
                }
                Value::Num(n as f64)
            }
            "COUNTBLANK" => match args {
                [range] => match self.eval_arg(range) {
                    Arg::Range(s, r1, c1, r2, c2) => {
                        let area = (r2 - r1 + 1) as usize * (c2 - c1 + 1) as usize;
                        let filled = self
                            .res
                            .cells_in(s, r1, c1, r2, c2)
                            .iter()
                            .filter(|(_, v)| !matches!(v, Value::Str(s) if s.is_empty()))
                            .count();
                        Value::Num((area - filled) as f64)
                    }
                    Arg::Scalar(_) => Value::Err(ExcelError::Value),
                },
                _ => Value::Err(ExcelError::Value),
            },

            // ---- logic --------------------------------------------------
            "IF" => {
                if args.is_empty() || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let cond = try_bool!(self.eval(&args[0]));
                if cond {
                    match args.get(1) {
                        Some(Expr::Missing) | None => Value::Num(0.0),
                        Some(e) => self.eval(e),
                    }
                } else {
                    match args.get(2) {
                        Some(Expr::Missing) | None => {
                            if args.len() < 3 {
                                Value::Bool(false)
                            } else {
                                Value::Num(0.0)
                            }
                        }
                        Some(e) => self.eval(e),
                    }
                }
            }
            "IFERROR" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let v = self.eval(&args[0]);
                if v.is_err() { self.eval(&args[1]) } else { v }
            }
            "IFNA" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let v = self.eval(&args[0]);
                if v == Value::Err(ExcelError::NA) {
                    self.eval(&args[1])
                } else {
                    v
                }
            }
            "AND" | "OR" | "XOR" => {
                let mut acc = name == "AND";
                let mut any = false;
                for a in args {
                    match self.eval_arg(a) {
                        Arg::Scalar(Value::Empty) => {}
                        Arg::Scalar(v) => {
                            let b = try_bool!(v);
                            any = true;
                            acc = match name {
                                "AND" => acc && b,
                                "OR" => acc || b,
                                _ => acc ^ b,
                            };
                        }
                        Arg::Range(s, r1, c1, r2, c2) => {
                            for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                                match v {
                                    Value::Bool(b) => {
                                        any = true;
                                        acc = match name {
                                            "AND" => acc && b,
                                            "OR" => acc || b,
                                            _ => acc ^ b,
                                        };
                                    }
                                    Value::Num(n) => {
                                        any = true;
                                        let b = n != 0.0;
                                        acc = match name {
                                            "AND" => acc && b,
                                            "OR" => acc || b,
                                            _ => acc ^ b,
                                        };
                                    }
                                    Value::Err(e) => return Value::Err(e),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                if !any {
                    return Value::Err(ExcelError::Value);
                }
                Value::Bool(acc)
            }
            "NOT" => match args {
                [a] => Value::Bool(!try_bool!(self.eval(a))),
                _ => Value::Err(ExcelError::Value),
            },
            "TRUE" => Value::Bool(true),
            "FALSE" => Value::Bool(false),
            "NA" => Value::Err(ExcelError::NA),
            "ISBLANK" => self.one_val(args, |v| Value::Bool(matches!(v, Value::Empty))),
            "ISNUMBER" => self.one_val(args, |v| Value::Bool(matches!(v, Value::Num(_)))),
            "ISTEXT" => self.one_val(args, |v| Value::Bool(matches!(v, Value::Str(_)))),
            "ISNONTEXT" => self.one_val(args, |v| Value::Bool(!matches!(v, Value::Str(_)))),
            "ISLOGICAL" => self.one_val(args, |v| Value::Bool(matches!(v, Value::Bool(_)))),
            "ISERROR" => self.one_val(args, |v| Value::Bool(v.is_err())),
            "ISERR" => self.one_val(args, |v| {
                Value::Bool(v.is_err() && v != Value::Err(ExcelError::NA))
            }),
            "ISNA" => self.one_val(args, |v| Value::Bool(v == Value::Err(ExcelError::NA))),
            "ISEVEN" | "ISODD" => match args {
                [a] => {
                    let n = try_num!(self.eval(a)).trunc() as i64;
                    let even = n % 2 == 0;
                    Value::Bool(if name == "ISEVEN" { even } else { !even })
                }
                _ => Value::Err(ExcelError::Value),
            },
            "N" => self.one_val(args, |v| match v {
                Value::Num(n) => Value::Num(n),
                Value::Bool(b) => Value::Num(if b { 1.0 } else { 0.0 }),
                Value::Err(e) => Value::Err(e),
                _ => Value::Num(0.0),
            }),
            "T" => self.one_val(args, |v| match v {
                Value::Str(s) => Value::Str(s),
                Value::Err(e) => Value::Err(e),
                _ => Value::Str(String::new()),
            }),

            // ---- text ---------------------------------------------------
            "LEN" => match args {
                [a] => Value::Num(try_text!(self.eval(a)).chars().count() as f64),
                _ => Value::Err(ExcelError::Value),
            },
            "LEFT" | "RIGHT" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                let n = if args.len() == 2 {
                    let k = try_num!(self.eval(&args[1]));
                    if k < 0.0 {
                        return Value::Err(ExcelError::Value);
                    }
                    k.trunc() as usize
                } else {
                    1
                };
                let chars: Vec<char> = s.chars().collect();
                let n = n.min(chars.len());
                Value::Str(if name == "LEFT" {
                    chars[..n].iter().collect()
                } else {
                    chars[chars.len() - n..].iter().collect()
                })
            }
            "MID" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                let start = try_num!(self.eval(&args[1]));
                let count = try_num!(self.eval(&args[2]));
                if start < 1.0 || count < 0.0 {
                    return Value::Err(ExcelError::Value);
                }
                let chars: Vec<char> = s.chars().collect();
                let from = ((start.trunc() as usize) - 1).min(chars.len());
                let to = (from + count.trunc() as usize).min(chars.len());
                Value::Str(chars[from..to].iter().collect())
            }
            "LOWER" => self.one_text(args, |s| Value::Str(s.to_lowercase())),
            "UPPER" => self.one_text(args, |s| Value::Str(s.to_uppercase())),
            "PROPER" => self.one_text(args, |s| {
                let mut out = String::with_capacity(s.len());
                let mut boundary = true;
                for ch in s.chars() {
                    if ch.is_alphanumeric() {
                        out.extend(if boundary {
                            ch.to_uppercase().collect::<Vec<_>>()
                        } else {
                            ch.to_lowercase().collect::<Vec<_>>()
                        });
                        boundary = false;
                    } else {
                        out.push(ch);
                        boundary = true;
                    }
                }
                Value::Str(out)
            }),
            "TRIM" => self.one_text(args, |s| {
                // Excel semantics: strips and collapses *spaces* only (0x20);
                // tabs and newlines pass through untouched.
                let mut out = String::with_capacity(s.len());
                let mut pending_space = false;
                for ch in s.trim_matches(' ').chars() {
                    if ch == ' ' {
                        pending_space = true;
                    } else {
                        if pending_space {
                            out.push(' ');
                            pending_space = false;
                        }
                        out.push(ch);
                    }
                }
                Value::Str(out)
            }),
            "CLEAN" => self.one_text(args, |s| {
                Value::Str(s.chars().filter(|c| !c.is_control()).collect())
            }),
            "CONCATENATE" => {
                let mut out = String::new();
                for a in args {
                    out.push_str(&try_text!(self.eval(a)));
                }
                Value::Str(out)
            }
            "CONCAT" => {
                let mut out = String::new();
                for a in args {
                    match self.eval_arg(a) {
                        Arg::Scalar(v) => out.push_str(&try_text!(v)),
                        Arg::Range(s, r1, c1, r2, c2) => {
                            for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                                out.push_str(&try_text!(v));
                            }
                        }
                    }
                }
                Value::Str(out)
            }
            "TEXTJOIN" => {
                if args.len() < 3 {
                    return Value::Err(ExcelError::Value);
                }
                let delim = try_text!(self.eval(&args[0]));
                let skip_empty = try_bool!(self.eval(&args[1]));
                let mut parts: Vec<String> = Vec::new();
                for a in &args[2..] {
                    match self.eval_arg(a) {
                        Arg::Scalar(v) => parts.push(try_text!(v)),
                        Arg::Range(s, r1, c1, r2, c2) => {
                            for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                                parts.push(try_text!(v));
                            }
                        }
                    }
                }
                if skip_empty {
                    parts.retain(|p| !p.is_empty());
                }
                Value::Str(parts.join(&delim))
            }
            "REPT" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                let n = try_num!(self.eval(&args[1]));
                if n < 0.0 || s.len() as f64 * n > 32_767.0 {
                    return Value::Err(ExcelError::Value);
                }
                Value::Str(s.repeat(n.trunc() as usize))
            }
            "REPLACE" => {
                if args.len() != 4 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                let start = try_num!(self.eval(&args[1]));
                let count = try_num!(self.eval(&args[2]));
                let new = try_text!(self.eval(&args[3]));
                if start < 1.0 || count < 0.0 {
                    return Value::Err(ExcelError::Value);
                }
                let chars: Vec<char> = s.chars().collect();
                let from = ((start.trunc() as usize) - 1).min(chars.len());
                let to = (from + count.trunc() as usize).min(chars.len());
                let mut out: String = chars[..from].iter().collect();
                out.push_str(&new);
                out.extend(&chars[to..]);
                Value::Str(out)
            }
            "SUBSTITUTE" => {
                if args.len() < 3 || args.len() > 4 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                let old = try_text!(self.eval(&args[1]));
                let new = try_text!(self.eval(&args[2]));
                if old.is_empty() {
                    return Value::Str(s);
                }
                if args.len() == 4 {
                    let nth = try_num!(self.eval(&args[3]));
                    if nth < 1.0 {
                        return Value::Err(ExcelError::Value);
                    }
                    let nth = nth.trunc() as usize;
                    let mut count = 0usize;
                    let mut out = String::new();
                    let mut rest = s.as_str();
                    while let Some(i) = rest.find(&old) {
                        count += 1;
                        if count == nth {
                            out.push_str(&rest[..i]);
                            out.push_str(&new);
                            out.push_str(&rest[i + old.len()..]);
                            return Value::Str(out);
                        }
                        out.push_str(&rest[..i + old.len()]);
                        rest = &rest[i + old.len()..];
                    }
                    out.push_str(rest);
                    Value::Str(out)
                } else {
                    Value::Str(s.replace(&old, &new))
                }
            }
            "FIND" | "SEARCH" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = try_text!(self.eval(&args[0]));
                let hay = try_text!(self.eval(&args[1]));
                let start = if args.len() == 3 {
                    let k = try_num!(self.eval(&args[2]));
                    if k < 1.0 {
                        return Value::Err(ExcelError::Value);
                    }
                    k.trunc() as usize - 1
                } else {
                    0
                };
                let hay_chars: Vec<char> = hay.chars().collect();
                if start > hay_chars.len() {
                    return Value::Err(ExcelError::Value);
                }
                let hay_tail: String = hay_chars[start..].iter().collect();
                let pos = if name == "FIND" {
                    hay_tail.find(&needle)
                } else {
                    hay_tail.to_lowercase().find(&needle.to_lowercase())
                };
                match pos {
                    Some(byte) => {
                        let chars_before = hay_tail[..byte].chars().count();
                        Value::Num((start + chars_before + 1) as f64)
                    }
                    None => Value::Err(ExcelError::Value),
                }
            }
            "EXACT" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let a = try_text!(self.eval(&args[0]));
                let b = try_text!(self.eval(&args[1]));
                Value::Bool(a == b)
            }
            "VALUE" | "NUMBERVALUE" => match args {
                [a] => {
                    let v = self.eval(a);
                    match to_num(&v) {
                        Ok(n) => Value::Num(n),
                        Err(e) => Value::Err(e),
                    }
                }
                _ => Value::Err(ExcelError::Value),
            },
            "CHAR" | "UNICHAR" => match args {
                [a] => {
                    let n = try_num!(self.eval(a)).trunc() as u32;
                    match char::from_u32(n) {
                        Some(c) if n >= 1 => Value::Str(c.to_string()),
                        _ => Value::Err(ExcelError::Value),
                    }
                }
                _ => Value::Err(ExcelError::Value),
            },
            "CODE" | "UNICODE" => match args {
                [a] => {
                    let s = try_text!(self.eval(a));
                    match s.chars().next() {
                        Some(c) => Value::Num(c as u32 as f64),
                        None => Value::Err(ExcelError::Value),
                    }
                }
                _ => Value::Err(ExcelError::Value),
            },

            // ---- dates ---------------------------------------------------
            "DATE" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let y = try_num!(self.eval(&args[0])).trunc() as i64;
                let m = try_num!(self.eval(&args[1])).trunc() as i64;
                let d = try_num!(self.eval(&args[2])).trunc() as i64;
                // Excel normalizes out-of-range months/days by rolling over.
                let y = if y < 1900 { y + 1900 } else { y };
                let total_months = y * 12 + (m - 1);
                let ny = total_months.div_euclid(12);
                let nm = total_months.rem_euclid(12) as u32 + 1;
                let serial = parts_to_serial(ny, nm, 1, 0, self.res.date1904()) + (d - 1) as f64;
                if serial < 0.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    Value::Num(serial)
                }
            }
            "TIME" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let h = try_num!(self.eval(&args[0])).trunc();
                let m = try_num!(self.eval(&args[1])).trunc();
                let s = try_num!(self.eval(&args[2])).trunc();
                let total = h * 3600.0 + m * 60.0 + s;
                if total < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                Value::Num((total % 86_400.0) / 86_400.0)
            }
            "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND" | "WEEKDAY" => {
                if args.is_empty() {
                    return Value::Err(ExcelError::Value);
                }
                let serial = try_num!(self.eval(&args[0]));
                let parts = match serial_to_parts(serial, self.res.date1904()) {
                    Some(p) => p,
                    None => return Value::Err(ExcelError::Num),
                };
                match name {
                    "YEAR" => Value::Num(parts.year as f64),
                    "MONTH" => Value::Num(parts.month as f64),
                    "DAY" => Value::Num(parts.day as f64),
                    "HOUR" => Value::Num(parts.hour as f64),
                    "MINUTE" => Value::Num(parts.minute as f64),
                    "SECOND" => Value::Num(parts.second as f64),
                    _ => {
                        // WEEKDAY(serial [, mode]) — default mode 1: Sunday=1.
                        // Excel's convention: serial 1 (1900-01-01) is a
                        // "Sunday" (the famous Lotus leap-bug artifact), so
                        // Sunday-index = (serial - 1) mod 7.
                        let mode = if args.len() == 2 {
                            try_num!(self.eval(&args[1])).trunc() as i64
                        } else {
                            1
                        };
                        let sun0 = (((serial.floor() as i64 - 1) % 7) + 7) % 7;
                        match mode {
                            1 => Value::Num(sun0 as f64 + 1.0),
                            2 => Value::Num(((sun0 + 6) % 7) as f64 + 1.0),
                            3 => Value::Num(((sun0 + 6) % 7) as f64),
                            _ => Value::Err(ExcelError::Num),
                        }
                    }
                }
            }
            "DAYS" => self.two_num(args, |end, start| num(end.floor() - start.floor())),
            "TODAY" | "NOW" => match self.res.today() {
                // The resolver supplies the current moment (date + time
                // fraction); TODAY truncates to midnight.
                Some(serial) => Value::Num(if name == "TODAY" {
                    serial.floor()
                } else {
                    serial
                }),
                None => {
                    self.unsupported = true;
                    Value::Err(ExcelError::Value)
                }
            },

            // ---- lookup ---------------------------------------------------
            "CHOOSE" => {
                if args.len() < 2 {
                    return Value::Err(ExcelError::Value);
                }
                let k = try_num!(self.eval(&args[0])).trunc() as usize;
                if k == 0 || k >= args.len() {
                    return Value::Err(ExcelError::Value);
                }
                self.eval(&args[k])
            }
            "ROW" => match args {
                [] => Value::Num(self.cell.0 as f64 + 1.0),
                [Expr::Ref(r)] => Value::Num(r.row as f64 + 1.0),
                [Expr::Range(a, _)] => Value::Num(a.row as f64 + 1.0),
                _ => Value::Err(ExcelError::Value),
            },
            "COLUMN" => match args {
                [] => Value::Num(self.cell.1 as f64 + 1.0),
                [Expr::Ref(r)] => Value::Num(r.col as f64 + 1.0),
                [Expr::Range(a, _)] => Value::Num(a.col as f64 + 1.0),
                _ => Value::Err(ExcelError::Value),
            },
            "ROWS" => match args {
                [Expr::Range(a, b)] => Value::Num((a.row - b.row).abs() as f64 + 1.0),
                [Expr::Ref(_)] => Value::Num(1.0),
                [Expr::ColRange { .. }] => Value::Num(crate::sheet::MAX_ROWS as f64),
                [Expr::RowRange { r1, r2, .. }] => Value::Num((r1 - r2).abs() as f64 + 1.0),
                _ => Value::Err(ExcelError::Value),
            },
            "COLUMNS" => match args {
                [Expr::Range(a, b)] => Value::Num((a.col - b.col).abs() as f64 + 1.0),
                [Expr::Ref(_)] => Value::Num(1.0),
                [Expr::RowRange { .. }] => Value::Num(crate::sheet::MAX_COLS as f64),
                [Expr::ColRange { c1, c2, .. }] => Value::Num((c1 - c2).abs() as f64 + 1.0),
                _ => Value::Err(ExcelError::Value),
            },
            "VLOOKUP" | "HLOOKUP" => {
                if args.len() < 3 || args.len() > 4 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = self.eval(&args[0]);
                if let Value::Err(e) = needle {
                    return Value::Err(e);
                }
                let (s, r1, c1, r2, c2) = match self.eval_arg(&args[1]) {
                    Arg::Range(s, a, b, c, d) => (s, a, b, c, d),
                    Arg::Scalar(v) => {
                        return if v.is_err() {
                            v
                        } else {
                            Value::Err(ExcelError::Value)
                        };
                    }
                };
                let idx = try_num!(self.eval(&args[2])).trunc();
                if idx < 1.0 {
                    return Value::Err(ExcelError::Value);
                }
                let idx = idx as u32 - 1;
                let approx = match args.get(3) {
                    Some(e) => try_bool!(self.eval(e)),
                    None => true,
                };
                let vertical = name == "VLOOKUP";
                let (r1, c1, r2c, c2c) = self.clamp(s, r1, c1, r2, c2);
                let lanes = if vertical { r1..=r2c } else { c1..=c2c };
                let mut best: Option<u32> = None;
                for lane in lanes {
                    let key = if vertical {
                        self.res.value(s, lane, c1)
                    } else {
                        self.res.value(s, r1, lane)
                    };
                    if matches!(key, Value::Empty) {
                        continue;
                    }
                    match compare(&key, &needle) {
                        Ok(std::cmp::Ordering::Equal) => {
                            best = Some(lane);
                            if !approx {
                                break;
                            }
                        }
                        Ok(std::cmp::Ordering::Less) if approx => best = Some(lane),
                        _ => {}
                    }
                }
                match best {
                    None => Value::Err(ExcelError::NA),
                    Some(lane) => {
                        if vertical {
                            if c1 + idx > c2 {
                                return Value::Err(ExcelError::Ref);
                            }
                            self.res.value(s, lane, c1 + idx)
                        } else {
                            if r1 + idx > r2 {
                                return Value::Err(ExcelError::Ref);
                            }
                            self.res.value(s, r1 + idx, lane)
                        }
                    }
                }
            }
            "MATCH" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = self.eval(&args[0]);
                let (s, r1, c1, r2, c2) = match self.eval_arg(&args[1]) {
                    Arg::Range(s, a, b, c, d) => (s, a, b, c, d),
                    Arg::Scalar(v) => {
                        return if v.is_err() {
                            v
                        } else {
                            Value::Err(ExcelError::NA)
                        };
                    }
                };
                let mode = match args.get(2) {
                    Some(e) => try_num!(self.eval(e)),
                    None => 1.0,
                };
                let vertical = c1 == c2;
                if !vertical && r1 != r2 {
                    return Value::Err(ExcelError::NA);
                }
                let (r1, c1, r2, c2) = self.clamp(s, r1, c1, r2, c2);
                let len = if vertical { r2 - r1 } else { c2 - c1 } + 1;
                let mut best: Option<u32> = None;
                for i in 0..len {
                    let v = if vertical {
                        self.res.value(s, r1 + i, c1)
                    } else {
                        self.res.value(s, r1, c1 + i)
                    };
                    if matches!(v, Value::Empty) {
                        continue;
                    }
                    match compare(&v, &needle) {
                        Ok(std::cmp::Ordering::Equal) => {
                            best = Some(i);
                            if mode == 0.0 {
                                break;
                            }
                        }
                        Ok(std::cmp::Ordering::Less) if mode > 0.0 => best = Some(i),
                        Ok(std::cmp::Ordering::Greater) if mode < 0.0 => best = Some(i),
                        _ => {}
                    }
                }
                // Wildcards with exact match mode.
                if best.is_none() && mode == 0.0 {
                    if let Value::Str(pat) = &needle {
                        if pat.contains(['*', '?']) {
                            for i in 0..len {
                                let v = if vertical {
                                    self.res.value(s, r1 + i, c1)
                                } else {
                                    self.res.value(s, r1, c1 + i)
                                };
                                if let Value::Str(t) = &v {
                                    if wildcard_match(pat, t) {
                                        best = Some(i);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                match best {
                    Some(i) => Value::Num(i as f64 + 1.0),
                    None => Value::Err(ExcelError::NA),
                }
            }
            "INDEX" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let (s, r1, c1, r2, c2) = match self.eval_arg(&args[0]) {
                    Arg::Range(s, a, b, c, d) => (s, a, b, c, d),
                    Arg::Scalar(v) => {
                        return if v.is_err() {
                            v
                        } else {
                            Value::Err(ExcelError::Value)
                        };
                    }
                };
                let ri = try_num!(self.eval(&args[1])).trunc() as i64;
                let ci = match args.get(2) {
                    Some(e) => try_num!(self.eval(e)).trunc() as i64,
                    None => {
                        // One-dimensional form: index along the single lane.
                        if r1 == r2 {
                            let k = ri;
                            if k < 1 || c1 as i64 + k - 1 > c2 as i64 {
                                return Value::Err(ExcelError::Ref);
                            }
                            return self.res.value(s, r1, (c1 as i64 + k - 1) as u32);
                        }
                        1
                    }
                };
                if ri < 1 || ci < 1 {
                    return Value::Err(ExcelError::Value);
                }
                let (r, c) = (r1 as i64 + ri - 1, c1 as i64 + ci - 1);
                if r > r2 as i64 || c > c2 as i64 {
                    return Value::Err(ExcelError::Ref);
                }
                self.res.value(s, r as u32, c as u32)
            }

            // ---- multi-criteria aggregation ----------------------------------
            "COUNTIFS" => match self.ifs_family(None, args) {
                Ok((count, _)) => Value::Num(count as f64),
                Err(v) => v,
            },
            "SUMIFS" => {
                if args.len() < 3 {
                    return Value::Err(ExcelError::Value);
                }
                match self.ifs_family(Some(&args[0]), &args[1..]) {
                    Ok((_, vals)) => num(vals.iter().sum()),
                    Err(v) => v,
                }
            }
            "AVERAGEIFS" => {
                if args.len() < 3 {
                    return Value::Err(ExcelError::Value);
                }
                match self.ifs_family(Some(&args[0]), &args[1..]) {
                    Ok((_, vals)) if vals.is_empty() => Value::Err(ExcelError::Div0),
                    Ok((_, vals)) => num(vals.iter().sum::<f64>() / vals.len() as f64),
                    Err(v) => v,
                }
            }
            "MAXIFS" | "MINIFS" => {
                if args.len() < 3 {
                    return Value::Err(ExcelError::Value);
                }
                match self.ifs_family(Some(&args[0]), &args[1..]) {
                    Ok((_, vals)) if vals.is_empty() => Value::Num(0.0),
                    Ok((_, vals)) => {
                        let it = vals.iter().copied();
                        Value::Num(if name == "MAXIFS" {
                            it.fold(f64::MIN, f64::max)
                        } else {
                            it.fold(f64::MAX, f64::min)
                        })
                    }
                    Err(v) => v,
                }
            }

            // ---- modern logic / lookup ---------------------------------------
            "IFS" => {
                if args.is_empty() || !args.len().is_multiple_of(2) {
                    return Value::Err(ExcelError::Value);
                }
                for chunk in args.chunks(2) {
                    match to_bool(&self.eval(&chunk[0])) {
                        Ok(true) => return self.eval(&chunk[1]),
                        Ok(false) => {}
                        Err(e) => return Value::Err(e),
                    }
                }
                Value::Err(ExcelError::NA)
            }
            "SWITCH" => {
                if args.len() < 3 {
                    return Value::Err(ExcelError::Value);
                }
                let subject = self.eval(&args[0]);
                if let Value::Err(e) = subject {
                    return Value::Err(e);
                }
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = self.eval(&args[i]);
                    if compare(&subject, &key) == Ok(std::cmp::Ordering::Equal) {
                        return self.eval(&args[i + 1]);
                    }
                    i += 2;
                }
                if i < args.len() {
                    self.eval(&args[i]) // the default
                } else {
                    Value::Err(ExcelError::NA)
                }
            }
            "XLOOKUP" => {
                if args.len() < 3 || args.len() > 6 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = self.eval(&args[0]);
                if let Value::Err(e) = needle {
                    return Value::Err(e);
                }
                let (ls, lookup) = match self.arg_vector(&args[1]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                let (rs, ret) = match self.arg_vector(&args[2]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                if lookup.len() != ret.len() {
                    return Value::Err(ExcelError::Value);
                }
                let match_mode = match args.get(4) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                let reverse = match args.get(5) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => try_num!(self.eval(e)) < 0.0,
                };
                let order: Vec<usize> = if reverse {
                    (0..lookup.len()).rev().collect()
                } else {
                    (0..lookup.len()).collect()
                };
                let mut exact: Option<usize> = None;
                let mut nearest: Option<(usize, Value)> = None;
                for i in order {
                    let (r, c) = lookup[i];
                    let v = self.res.value(ls, r, c);
                    if match_mode == 2.0 {
                        if let (Value::Str(pat), Value::Str(t)) = (&needle, &v) {
                            if wildcard_match(pat, t) {
                                exact = Some(i);
                                break;
                            }
                        }
                        continue;
                    }
                    match compare(&v, &needle) {
                        Ok(std::cmp::Ordering::Equal) => {
                            exact = Some(i);
                            break;
                        }
                        Ok(std::cmp::Ordering::Less) if match_mode == -1.0 => {
                            // Best "next smaller": the largest value ≤ needle.
                            let better = match &nearest {
                                None => true,
                                Some((_, best)) => {
                                    compare(&v, best) == Ok(std::cmp::Ordering::Greater)
                                }
                            };
                            if better {
                                nearest = Some((i, v));
                            }
                        }
                        Ok(std::cmp::Ordering::Greater) if match_mode == 1.0 => {
                            let better = match &nearest {
                                None => true,
                                Some((_, best)) => {
                                    compare(&v, best) == Ok(std::cmp::Ordering::Less)
                                }
                            };
                            if better {
                                nearest = Some((i, v));
                            }
                        }
                        _ => {}
                    }
                }
                match exact.or(nearest.map(|(i, _)| i)) {
                    Some(i) => {
                        let (r, c) = ret[i];
                        self.res.value(rs, r, c)
                    }
                    None => match args.get(3) {
                        None | Some(Expr::Missing) => Value::Err(ExcelError::NA),
                        Some(e) => self.eval(e),
                    },
                }
            }
            "LOOKUP" => {
                // Vector form: LOOKUP(value, lookup_vector, [result_vector]) —
                // largest lookup value ≤ needle (assumes ascending order).
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = self.eval(&args[0]);
                let (ls, lookup) = match self.arg_vector(&args[1]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                let (rs, ret) = match args.get(2) {
                    Some(e) => match self.arg_vector(e) {
                        Ok(v) => v,
                        Err(v) => return v,
                    },
                    None => (ls, lookup.clone()),
                };
                let mut best: Option<usize> = None;
                for (i, &(r, c)) in lookup.iter().enumerate() {
                    let v = self.res.value(ls, r, c);
                    if matches!(v, Value::Empty) {
                        continue;
                    }
                    match compare(&v, &needle) {
                        Ok(std::cmp::Ordering::Equal) | Ok(std::cmp::Ordering::Less) => {
                            best = Some(i);
                        }
                        _ => {}
                    }
                }
                match best {
                    Some(i) if i < ret.len() => {
                        let (r, c) = ret[i];
                        self.res.value(rs, r, c)
                    }
                    _ => Value::Err(ExcelError::NA),
                }
            }
            "HYPERLINK" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let link = try_text!(self.eval(&args[0]));
                match args.get(1) {
                    Some(Expr::Missing) | None => Value::Str(link),
                    Some(e) => self.eval(e),
                }
            }

            // ---- more dates -----------------------------------------------------
            "EDATE" | "EOMONTH" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let serial = try_num!(self.eval(&args[0]));
                let months = try_num!(self.eval(&args[1])).trunc() as i64;
                let p = match serial_to_parts(serial, self.res.date1904()) {
                    Some(p) => p,
                    None => return Value::Err(ExcelError::Num),
                };
                let total = p.year * 12 + p.month as i64 - 1 + months;
                let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
                let day = if name == "EOMONTH" {
                    days_in_month(ny, nm)
                } else {
                    p.day.min(days_in_month(ny, nm))
                };
                let out = parts_to_serial(ny, nm, day, 0, self.res.date1904());
                if out < 0.0 {
                    Value::Err(ExcelError::Num)
                } else {
                    Value::Num(out)
                }
            }
            "DATEDIF" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let start = try_num!(self.eval(&args[0]));
                let end = try_num!(self.eval(&args[1]));
                if end < start {
                    return Value::Err(ExcelError::Num);
                }
                let unit = try_text!(self.eval(&args[2])).to_ascii_uppercase();
                let d1904 = self.res.date1904();
                let (a, b) = match (serial_to_parts(start, d1904), serial_to_parts(end, d1904)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return Value::Err(ExcelError::Num),
                };
                let months = (b.year * 12 + b.month as i64)
                    - (a.year * 12 + a.month as i64)
                    - i64::from(b.day < a.day);
                Value::Num(match unit.as_str() {
                    "D" => end.floor() - start.floor(),
                    "M" => months as f64,
                    "Y" => (months / 12) as f64,
                    "YM" => (months % 12) as f64,
                    "MD" => {
                        // Days ignoring months and years.
                        let mut anchor_m = b.month as i64 - 1;
                        let mut anchor_y = b.year;
                        if b.day < a.day {
                            anchor_m -= 1;
                        }
                        let anchor_y2 = anchor_y + anchor_m.div_euclid(12);
                        let anchor_m2 = anchor_m.rem_euclid(12) as u32 + 1;
                        anchor_y = anchor_y2;
                        anchor_m = anchor_m2 as i64;
                        let day = a.day.min(days_in_month(anchor_y, anchor_m as u32));
                        let anchor = parts_to_serial(anchor_y, anchor_m as u32, day, 0, d1904);
                        end.floor() - anchor
                    }
                    "YD" => {
                        // Days ignoring years.
                        let mut y = a.year + (months / 12);
                        let mut anchor = parts_to_serial(
                            y,
                            a.month,
                            a.day.min(days_in_month(y, a.month)),
                            0,
                            d1904,
                        );
                        if anchor > end.floor() {
                            y -= 1;
                            anchor = parts_to_serial(
                                y,
                                a.month,
                                a.day.min(days_in_month(y, a.month)),
                                0,
                                d1904,
                            );
                        }
                        end.floor() - anchor
                    }
                    _ => return Value::Err(ExcelError::Num),
                })
            }
            "WEEKNUM" | "ISOWEEKNUM" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let serial = try_num!(self.eval(&args[0]));
                let mode = if name == "ISOWEEKNUM" {
                    21
                } else {
                    match args.get(1) {
                        None | Some(Expr::Missing) => 1,
                        Some(e) => try_num!(self.eval(e)).trunc() as i64,
                    }
                };
                let d1904 = self.res.date1904();
                let p = match serial_to_parts(serial, d1904) {
                    Some(p) => p,
                    None => return Value::Err(ExcelError::Num),
                };
                let jan1 = parts_to_serial(p.year, 1, 1, 0, d1904);
                let doy = (serial.floor() - jan1) as i64 + 1;
                match mode {
                    1 | 2 => {
                        // Week 1 contains Jan 1; weeks start Sunday (1) or
                        // Monday (2). Day-of-week index of Jan 1, 0-based from
                        // the week start:
                        let sun0 = (((jan1 as i64 - 1) % 7) + 7) % 7; // 0=Sunday
                        let start0 = if mode == 1 { sun0 } else { (sun0 + 6) % 7 };
                        Value::Num(((doy + start0 - 1) / 7 + 1) as f64)
                    }
                    21 => {
                        // ISO 8601: weeks start Monday, week 1 contains the
                        // first Thursday.
                        let dow_iso = ((serial.floor() as i64 - 2).rem_euclid(7)) + 1; // Mon=1
                        let week = (doy - dow_iso + 10) / 7;
                        if week < 1 {
                            // Belongs to the previous year's last ISO week.
                            let prev_dec31 = jan1 - 1.0;
                            let pp = serial_to_parts(prev_dec31, d1904).unwrap_or(p);
                            let prev_jan1 = parts_to_serial(pp.year, 1, 1, 0, d1904);
                            let pdoy = (prev_dec31 - prev_jan1) as i64 + 1;
                            let pdow = ((prev_dec31 as i64 - 2).rem_euclid(7)) + 1;
                            Value::Num(((pdoy - pdow + 10) / 7) as f64)
                        } else {
                            // Week 53 spillover into next year's week 1.
                            let dec31 = parts_to_serial(p.year, 12, 31, 0, d1904);
                            let last_doy = (dec31 - jan1) as i64 + 1;
                            let last_dow = ((dec31 as i64 - 2).rem_euclid(7)) + 1;
                            let max_week = (last_doy - last_dow + 10) / 7;
                            Value::Num(if week > max_week { 1.0 } else { week as f64 })
                        }
                    }
                    _ => Value::Err(ExcelError::Num),
                }
            }

            // ---- financial -----------------------------------------------------
            "PMT" | "PV" | "FV" | "NPER" => {
                if args.len() < 3 || args.len() > 5 {
                    return Value::Err(ExcelError::Value);
                }
                let a0 = try_num!(self.eval(&args[0]));
                let a1 = try_num!(self.eval(&args[1]));
                let a2 = try_num!(self.eval(&args[2]));
                let a3 = match args.get(3) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                let type_ = match args.get(4) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                let t = if type_ != 0.0 { 1.0 } else { 0.0 };
                match name {
                    "PMT" => {
                        let (rate, nper, pv, fv) = (a0, a1, a2, a3);
                        if nper == 0.0 {
                            return Value::Err(ExcelError::Num);
                        }
                        if rate == 0.0 {
                            return num(-(pv + fv) / nper);
                        }
                        let f = (1.0 + rate).powf(nper);
                        num(-(fv + pv * f) * rate / ((f - 1.0) * (1.0 + rate * t)))
                    }
                    "PV" => {
                        let (rate, nper, pmt, fv) = (a0, a1, a2, a3);
                        if rate == 0.0 {
                            return num(-(fv + pmt * nper));
                        }
                        let f = (1.0 + rate).powf(nper);
                        num(-(fv + pmt * (1.0 + rate * t) * (f - 1.0) / rate) / f)
                    }
                    "FV" => {
                        let (rate, nper, pmt, pv) = (a0, a1, a2, a3);
                        if rate == 0.0 {
                            return num(-(pv + pmt * nper));
                        }
                        let f = (1.0 + rate).powf(nper);
                        num(-(pv * f + pmt * (1.0 + rate * t) * (f - 1.0) / rate))
                    }
                    _ => {
                        // NPER(rate, pmt, pv, [fv], [type])
                        let (rate, pmt, pv, fv) = (a0, a1, a2, a3);
                        if rate == 0.0 {
                            if pmt == 0.0 {
                                return Value::Err(ExcelError::Div0);
                            }
                            return num(-(pv + fv) / pmt);
                        }
                        let adj = pmt * (1.0 + rate * t) / rate;
                        let ratio = (adj - fv) / (pv + adj);
                        if ratio <= 0.0 {
                            return Value::Err(ExcelError::Num);
                        }
                        num(ratio.ln() / (1.0 + rate).ln())
                    }
                }
            }
            "NPV" => {
                if args.len() < 2 {
                    return Value::Err(ExcelError::Value);
                }
                let rate = try_num!(self.eval(&args[0]));
                if rate == -1.0 {
                    return Value::Err(ExcelError::Div0);
                }
                match self.collect_values(&args[1..], true) {
                    Ok(vals) => {
                        let mut total = 0.0;
                        for (i, v) in vals.iter().enumerate() {
                            total += v / (1.0 + rate).powi(i as i32 + 1);
                        }
                        num(total)
                    }
                    Err(e) => Value::Err(e),
                }
            }
            "IRR" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let vals = match self.collect_values(&args[..1], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                if !vals.iter().any(|&v| v > 0.0) || !vals.iter().any(|&v| v < 0.0) {
                    return Value::Err(ExcelError::Num);
                }
                let guess = match args.get(1) {
                    None | Some(Expr::Missing) => 0.1,
                    Some(e) => try_num!(self.eval(e)),
                };
                match solve_irr(&vals, guess) {
                    Some(r) => num(r),
                    None => Value::Err(ExcelError::Num),
                }
            }
            "RATE" => {
                if args.len() < 3 || args.len() > 6 {
                    return Value::Err(ExcelError::Value);
                }
                let nper = try_num!(self.eval(&args[0]));
                let pmt = try_num!(self.eval(&args[1]));
                let pv = try_num!(self.eval(&args[2]));
                let fv = match args.get(3) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                let t = match args.get(4) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => {
                        if try_num!(self.eval(e)) != 0.0 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                };
                let guess = match args.get(5) {
                    None | Some(Expr::Missing) => 0.1,
                    Some(e) => try_num!(self.eval(e)),
                };
                match solve_rate(nper, pmt, pv, fv, t, guess) {
                    Some(r) => num(r),
                    None => Value::Err(ExcelError::Num),
                }
            }

            // ---- more statistics --------------------------------------------------
            "RANK" | "RANK.EQ" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let x = try_num!(self.eval(&args[0]));
                let vals = match self.collect_values(&args[1..2], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                let ascending = match args.get(2) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => try_num!(self.eval(e)) != 0.0,
                };
                if !vals.contains(&x) {
                    return Value::Err(ExcelError::NA);
                }
                let rank = 1 + vals
                    .iter()
                    .filter(|&&v| if ascending { v < x } else { v > x })
                    .count();
                Value::Num(rank as f64)
            }
            "MODE" | "MODE.SNGL" => match self.collect_values(args, true) {
                Ok(vals) => {
                    // Most frequent value; ties keep the first-seen (Excel).
                    let mut best: Option<(f64, usize)> = None;
                    for (i, &v) in vals.iter().enumerate() {
                        if vals[..i].contains(&v) {
                            continue;
                        }
                        let count = vals.iter().filter(|&&w| w == v).count();
                        if count > 1 && best.is_none_or(|(_, bc)| count > bc) {
                            best = Some((v, count));
                        }
                    }
                    match best {
                        Some((v, _)) => Value::Num(v),
                        None => Value::Err(ExcelError::NA),
                    }
                }
                Err(e) => Value::Err(e),
            },
            "PERCENTILE" | "PERCENTILE.INC" | "QUARTILE" | "QUARTILE.INC" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let k = try_num!(self.eval(&args[1]));
                let k = if name.starts_with("QUARTILE") {
                    if k.fract() != 0.0 || !(0.0..=4.0).contains(&k) {
                        return Value::Err(ExcelError::Num);
                    }
                    k / 4.0
                } else {
                    k
                };
                if !(0.0..=1.0).contains(&k) {
                    return Value::Err(ExcelError::Num);
                }
                match self.collect_values(&args[..1], true) {
                    Ok(mut vals) if !vals.is_empty() => {
                        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let pos = k * (vals.len() - 1) as f64;
                        let lo = pos.floor() as usize;
                        let hi = pos.ceil() as usize;
                        num(vals[lo] + (vals[hi] - vals[lo]) * (pos - lo as f64))
                    }
                    Ok(_) => Value::Err(ExcelError::Num),
                    Err(e) => Value::Err(e),
                }
            }
            "COMBIN" => self.two_num(args, |n, k| {
                let (n, k) = (n.trunc(), k.trunc());
                if n < 0.0 || k < 0.0 || k > n {
                    return Value::Err(ExcelError::Num);
                }
                let mut r = 1.0f64;
                let k = k.min(n - k);
                for i in 0..(k as u64) {
                    r = r * (n - i as f64) / (i as f64 + 1.0);
                }
                num(r.round())
            }),
            "PERMUT" => self.two_num(args, |n, k| {
                let (n, k) = (n.trunc(), k.trunc());
                if n < 0.0 || k < 0.0 || k > n {
                    return Value::Err(ExcelError::Num);
                }
                let mut r = 1.0f64;
                for i in 0..(k as u64) {
                    r *= n - i as f64;
                }
                num(r)
            }),
            "SUMSQ" => match self.collect_values(args, true) {
                Ok(v) => num(v.iter().map(|x| x * x).sum()),
                Err(e) => Value::Err(e),
            },

            // ---- TEXT (best-effort number-format rendering) ---------------------
            "TEXT" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let v = self.eval(&args[0]);
                if let Value::Err(e) = v {
                    return Value::Err(e);
                }
                let code = try_text!(self.eval(&args[1]));
                let fmt = crate::sheet::classify_format_code(&code);
                let bare = code.trim().to_ascii_lowercase();
                // Only claim formats our classifier genuinely understood; an
                // unrecognized code must not fabricate output.
                if fmt == crate::sheet::NumFmt::General && !(bare == "general" || bare.is_empty()) {
                    self.unsupported = true;
                    return Value::Err(ExcelError::Value);
                }
                match &v {
                    Value::Str(s) => Value::Str(s.clone()),
                    Value::Empty => Value::Str(String::new()),
                    Value::Bool(b) => Value::Str(if *b { "TRUE" } else { "FALSE" }.to_string()),
                    Value::Num(n) => Value::Str(crate::sheet::format_value(
                        &crate::sheet::CellValue::Number(*n),
                        fmt,
                        self.res.date1904(),
                    )),
                    Value::Err(_) => unreachable!(),
                }
            }

            // ---- unknown ---------------------------------------------------
            _ => {
                self.unsupported = true;
                Value::Err(ExcelError::Name)
            }
        }
    }

    fn one_num(&mut self, args: &[Expr], f: impl FnOnce(f64) -> Value) -> Value {
        match args {
            [a] => {
                let n = try_num!(self.eval(a));
                f(n)
            }
            _ => Value::Err(ExcelError::Value),
        }
    }

    fn two_num(&mut self, args: &[Expr], f: impl FnOnce(f64, f64) -> Value) -> Value {
        match args {
            [a, b] => {
                let x = try_num!(self.eval(a));
                let y = try_num!(self.eval(b));
                f(x, y)
            }
            _ => Value::Err(ExcelError::Value),
        }
    }

    fn one_val(&mut self, args: &[Expr], f: impl FnOnce(Value) -> Value) -> Value {
        match args {
            [a] => {
                let v = self.eval(a);
                f(v)
            }
            _ => Value::Err(ExcelError::Value),
        }
    }

    fn one_text(&mut self, args: &[Expr], f: impl FnOnce(String) -> Value) -> Value {
        match args {
            [a] => {
                let s = try_text!(self.eval(a));
                f(s)
            }
            _ => Value::Err(ExcelError::Value),
        }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
    }
}

/// NPV of a cash-flow series at rate `r` (first value at t=0).
fn npv_at(vals: &[f64], r: f64) -> f64 {
    vals.iter()
        .enumerate()
        .map(|(i, v)| v / (1.0 + r).powi(i as i32))
        .sum()
}

/// IRR via Newton's method with a bisection fallback.
fn solve_irr(vals: &[f64], guess: f64) -> Option<f64> {
    let mut r = guess.max(-0.99);
    for _ in 0..60 {
        let f = npv_at(vals, r);
        if f.abs() < 1e-9 {
            return Some(r);
        }
        let h = 1e-6;
        let df = (npv_at(vals, r + h) - f) / h;
        if df.abs() < 1e-12 {
            break;
        }
        let next = r - f / df;
        if !next.is_finite() || next <= -1.0 {
            break;
        }
        if (next - r).abs() < 1e-10 {
            return Some(next);
        }
        r = next;
    }
    // Bisection over a wide bracket.
    let (mut lo, mut hi) = (-0.999_999, 10.0);
    let (flo, fhi) = (npv_at(vals, lo), npv_at(vals, hi));
    if flo * fhi > 0.0 {
        return None;
    }
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        let fm = npv_at(vals, mid);
        if fm.abs() < 1e-9 {
            return Some(mid);
        }
        if flo * fm < 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some((lo + hi) / 2.0)
}

/// RATE via Newton on the annuity equation, bisection fallback.
fn solve_rate(nper: f64, pmt: f64, pv: f64, fv: f64, t: f64, guess: f64) -> Option<f64> {
    let f = |r: f64| -> f64 {
        if r.abs() < 1e-12 {
            pv + pmt * nper + fv
        } else {
            let g = (1.0 + r).powf(nper);
            pv * g + pmt * (1.0 + r * t) * (g - 1.0) / r + fv
        }
    };
    let mut r = guess;
    for _ in 0..60 {
        let y = f(r);
        if y.abs() < 1e-9 {
            return Some(r);
        }
        let h = 1e-6;
        let dy = (f(r + h) - y) / h;
        if dy.abs() < 1e-12 {
            break;
        }
        let next = r - y / dy;
        if !next.is_finite() || next <= -1.0 {
            break;
        }
        if (next - r).abs() < 1e-12 {
            return Some(next);
        }
        r = next;
    }
    let (mut lo, mut hi) = (-0.999_999, 10.0);
    if f(lo) * f(hi) > 0.0 {
        return None;
    }
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if f(mid).abs() < 1e-9 {
            return Some(mid);
        }
        if f(lo) * f(mid) < 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some((lo + hi) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test resolver over a single sheet of literal values.
    struct Grid {
        cells: HashMap<(u32, u32), Value>,
        names: HashMap<String, String>,
    }

    impl Grid {
        fn new(cells: &[(&str, Value)]) -> Grid {
            let mut m = HashMap::new();
            for (name, v) in cells {
                let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
                m.insert((r, c), v.clone());
            }
            Grid {
                cells: m,
                names: HashMap::new(),
            }
        }
        fn with_name(mut self, name: &str, def: &str) -> Grid {
            self.names.insert(name.to_uppercase(), def.to_string());
            self
        }
    }

    impl Resolver for Grid {
        fn value(&self, _sheet: usize, row: u32, col: u32) -> Value {
            self.cells.get(&(row, col)).cloned().unwrap_or(Value::Empty)
        }
        fn sheet_index(&self, name: &str) -> Option<usize> {
            (name == "Sheet1").then_some(0)
        }
        fn cells_in(
            &self,
            _sheet: usize,
            r1: u32,
            c1: u32,
            r2: u32,
            c2: u32,
        ) -> Vec<((u32, u32), Value)> {
            let mut out: Vec<_> = self
                .cells
                .iter()
                .filter(|((r, c), _)| *r >= r1 && *r <= r2 && *c >= c1 && *c <= c2)
                .map(|(k, v)| (*k, v.clone()))
                .collect();
            out.sort_by_key(|(k, _)| *k);
            out
        }
        fn today(&self) -> Option<f64> {
            Some(45_306.0) // 2024-01-15
        }
        fn used_size(&self, _sheet: usize) -> (u32, u32) {
            let mut rows = 0;
            let mut cols = 0;
            for &(r, c) in self.cells.keys() {
                rows = rows.max(r + 1);
                cols = cols.max(c + 1);
            }
            (rows, cols)
        }
        fn defined_name(&self, name: &str, _current_sheet: usize) -> Option<String> {
            self.names.get(&name.to_uppercase()).cloned()
        }
    }

    fn eval_str(src: &str, grid: &Grid) -> Value {
        let ast = parse(src).unwrap_or_else(|e| panic!("parse {src}: {e}"));
        let mut ev = Eval::new(grid, 0, (0, 0));
        ev.eval(&ast)
    }

    fn n(src: &str, grid: &Grid) -> f64 {
        match eval_str(src, grid) {
            Value::Num(x) => x,
            v => panic!("{src} → {v:?}, expected number"),
        }
    }

    fn empty() -> Grid {
        Grid::new(&[])
    }

    #[test]
    fn arithmetic_and_precedence() {
        let g = empty();
        assert_eq!(n("1+2*3", &g), 7.0);
        assert_eq!(n("(1+2)*3", &g), 9.0);
        assert_eq!(n("-2^2", &g), 4.0); // Excel: unary minus binds tighter
        assert_eq!(n("2^3^2", &g), 64.0); // left-associative
        assert_eq!(n("10%", &g), 0.1);
        assert_eq!(n("50%%", &g), 0.005);
        assert_eq!(eval_str("1/0", &g), Value::Err(ExcelError::Div0));
        assert_eq!(n("2+2%", &g), 2.02);
    }

    #[test]
    fn string_ops_and_compare() {
        let g = empty();
        assert_eq!(eval_str("\"foo\"&\"bar\"", &g), Value::Str("foobar".into()));
        assert_eq!(eval_str("\"a\"=\"A\"", &g), Value::Bool(true)); // case-insensitive
        assert_eq!(eval_str("\"b\">\"a\"", &g), Value::Bool(true));
        assert_eq!(eval_str("1=\"1\"", &g), Value::Bool(false)); // number ≠ text
        assert_eq!(eval_str("2>1", &g), Value::Bool(true));
        assert_eq!(
            eval_str("\"say \"\"hi\"\"\"", &g),
            Value::Str("say \"hi\"".into())
        );
    }

    #[test]
    fn refs_and_ranges() {
        let g = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("A2", Value::Num(2.0)),
            ("A3", Value::Num(3.0)),
            ("B1", Value::Str("x".into())),
        ]);
        assert_eq!(n("A1+A2", &g), 3.0);
        assert_eq!(n("SUM(A1:A3)", &g), 6.0);
        assert_eq!(n("SUM(A1:B3)", &g), 6.0); // text in range ignored
        assert_eq!(n("Sheet1!A1", &g), 1.0);
        assert_eq!(n("'Sheet1'!A2", &g), 2.0);
        assert_eq!(n("SUM($A$1:$A$2)", &g), 3.0);
        assert_eq!(eval_str("Sheet9!A1", &g), Value::Err(ExcelError::Ref));
        assert_eq!(n("COUNT(A1:B3)", &g), 3.0);
        assert_eq!(n("COUNTA(A1:B3)", &g), 4.0);
        assert_eq!(n("COUNTBLANK(A1:B3)", &g), 2.0);
    }

    #[test]
    fn functions_math_stats() {
        let g = Grid::new(&[
            ("A1", Value::Num(4.0)),
            ("A2", Value::Num(9.0)),
            ("A3", Value::Num(2.0)),
        ]);
        assert_eq!(n("AVERAGE(A1:A3)", &g), 5.0);
        assert_eq!(n("MIN(A1:A3)", &g), 2.0);
        assert_eq!(n("MAX(A1:A3)", &g), 9.0);
        assert_eq!(n("MEDIAN(A1:A3)", &g), 4.0);
        assert_eq!(n("LARGE(A1:A3,1)", &g), 9.0);
        assert_eq!(n("SMALL(A1:A3,2)", &g), 4.0);
        assert_eq!(n("ROUND(2.345,2)", &g), 2.35);
        assert_eq!(n("ROUND(-2.5,0)", &g), -3.0); // round half away from zero
        assert_eq!(n("ROUNDDOWN(2.9,0)", &g), 2.0);
        assert_eq!(n("ROUNDUP(2.1,0)", &g), 3.0);
        assert_eq!(n("MOD(-3,2)", &g), 1.0); // sign follows divisor
        assert_eq!(n("INT(-1.5)", &g), -2.0);
        assert_eq!(n("TRUNC(-1.5)", &g), -1.0);
        assert_eq!(n("SQRT(A2)", &g), 3.0);
        assert_eq!(n("POWER(2,10)", &g), 1024.0);
        assert_eq!(n("SUMPRODUCT(A1:A3,A1:A3)", &g), 16.0 + 81.0 + 4.0);
        assert_eq!(n("GCD(12,18)", &g), 6.0);
        assert_eq!(n("LCM(4,6)", &g), 12.0);
        assert_eq!(n("EVEN(1.5)", &g), 2.0);
        assert_eq!(n("ODD(2.5)", &g), 3.0);
        assert_eq!(n("FACT(5)", &g), 120.0);
        assert_eq!(n("STDEVP(A1:A3)", &g), {
            let m = 5.0;
            let var = ((4.0f64 - m).powi(2) + (9.0 - m).powi(2) + (2.0 - m).powi(2)) / 3.0;
            var.sqrt()
        });
    }

    #[test]
    fn functions_logic() {
        let g = Grid::new(&[("A1", Value::Num(5.0)), ("B1", Value::Str("".into()))]);
        assert_eq!(n("IF(A1>3,10,20)", &g), 10.0);
        assert_eq!(n("IF(A1<3,10,20)", &g), 20.0);
        assert_eq!(eval_str("IF(A1<3,10)", &g), Value::Bool(false));
        assert_eq!(eval_str("AND(TRUE,A1>1)", &g), Value::Bool(true));
        assert_eq!(eval_str("OR(FALSE,FALSE)", &g), Value::Bool(false));
        assert_eq!(eval_str("NOT(TRUE)", &g), Value::Bool(false));
        assert_eq!(n("IFERROR(1/0,42)", &g), 42.0);
        assert_eq!(eval_str("ISBLANK(C9)", &g), Value::Bool(true));
        assert_eq!(eval_str("ISNUMBER(A1)", &g), Value::Bool(true));
        assert_eq!(eval_str("ISTEXT(B1)", &g), Value::Bool(true));
        assert_eq!(eval_str("ISNA(NA())", &g), Value::Bool(true));
    }

    #[test]
    fn functions_text() {
        let g = empty();
        assert_eq!(eval_str("LEFT(\"hello\",2)", &g), Value::Str("he".into()));
        assert_eq!(eval_str("RIGHT(\"hello\",3)", &g), Value::Str("llo".into()));
        assert_eq!(eval_str("MID(\"hello\",2,3)", &g), Value::Str("ell".into()));
        assert_eq!(n("LEN(\"héllo\")", &g), 5.0);
        assert_eq!(eval_str("UPPER(\"abc\")", &g), Value::Str("ABC".into()));
        assert_eq!(
            eval_str("PROPER(\"war and peace\")", &g),
            Value::Str("War And Peace".into())
        );
        assert_eq!(eval_str("TRIM(\"  a   b \")", &g), Value::Str("a b".into()));
        assert_eq!(
            eval_str("SUBSTITUTE(\"aaa\",\"a\",\"b\",2)", &g),
            Value::Str("aba".into())
        );
        assert_eq!(n("FIND(\"l\",\"hello\")", &g), 3.0);
        assert_eq!(n("SEARCH(\"L\",\"hello\")", &g), 3.0);
        assert_eq!(
            eval_str("FIND(\"z\",\"hello\")", &g),
            Value::Err(ExcelError::Value)
        );
        assert_eq!(eval_str("REPT(\"ab\",3)", &g), Value::Str("ababab".into()));
        assert_eq!(
            eval_str("CONCATENATE(\"a\",1,TRUE)", &g),
            Value::Str("a1TRUE".into())
        );
        assert_eq!(
            eval_str("TEXTJOIN(\"-\",TRUE,\"a\",\"\",\"b\")", &g),
            Value::Str("a-b".into())
        );
        assert_eq!(n("VALUE(\"12.5\")", &g), 12.5);
        assert_eq!(n("VALUE(\"15%\")", &g), 0.15);
        assert_eq!(eval_str("CHAR(65)", &g), Value::Str("A".into()));
        assert_eq!(n("CODE(\"A\")", &g), 65.0);
        assert_eq!(eval_str("EXACT(\"a\",\"A\")", &g), Value::Bool(false));
    }

    #[test]
    fn functions_dates() {
        let g = empty();
        assert_eq!(n("DATE(2024,1,15)", &g), 45_306.0);
        assert_eq!(n("YEAR(45306)", &g), 2024.0);
        assert_eq!(n("MONTH(45306)", &g), 1.0);
        assert_eq!(n("DAY(45306)", &g), 15.0);
        assert_eq!(n("DATE(2023,13,1)", &g), n("DATE(2024,1,1)", &g)); // month rollover
        assert_eq!(n("TODAY()", &g), 45_306.0);
        assert_eq!(n("DAYS(45310,45306)", &g), 4.0);
        assert_eq!(n("HOUR(0.75)", &g), 18.0);
        // 2024-01-15 is a Monday → WEEKDAY mode 1 (Sun=1) = 2.
        assert_eq!(n("WEEKDAY(45306)", &g), 2.0);
        assert_eq!(n("WEEKDAY(45306,2)", &g), 1.0);
    }

    #[test]
    fn functions_lookup() {
        let g = Grid::new(&[
            ("A1", Value::Str("apple".into())),
            ("B1", Value::Num(10.0)),
            ("A2", Value::Str("banana".into())),
            ("B2", Value::Num(20.0)),
            ("A3", Value::Str("cherry".into())),
            ("B3", Value::Num(30.0)),
        ]);
        assert_eq!(n("VLOOKUP(\"banana\",A1:B3,2,FALSE)", &g), 20.0);
        assert_eq!(
            eval_str("VLOOKUP(\"kiwi\",A1:B3,2,FALSE)", &g),
            Value::Err(ExcelError::NA)
        );
        assert_eq!(n("MATCH(\"cherry\",A1:A3,0)", &g), 3.0);
        assert_eq!(n("INDEX(A1:B3,2,2)", &g), 20.0);
        assert_eq!(n("CHOOSE(2,10,20,30)", &g), 20.0);
        assert_eq!(n("ROWS(A1:B3)", &g), 3.0);
        assert_eq!(n("COLUMNS(A1:B3)", &g), 2.0);
    }

    #[test]
    fn criteria_functions() {
        let g = Grid::new(&[
            ("A1", Value::Num(5.0)),
            ("A2", Value::Num(15.0)),
            ("A3", Value::Num(25.0)),
            ("B1", Value::Str("red".into())),
            ("B2", Value::Str("blue".into())),
            ("B3", Value::Str("red".into())),
        ]);
        assert_eq!(n("COUNTIF(A1:A3,\">10\")", &g), 2.0);
        assert_eq!(n("COUNTIF(B1:B3,\"red\")", &g), 2.0);
        assert_eq!(n("COUNTIF(B1:B3,\"r*\")", &g), 2.0);
        assert_eq!(n("COUNTIF(B1:B3,\"b???\")", &g), 1.0);
        assert_eq!(n("SUMIF(A1:A3,\">10\")", &g), 40.0);
        assert_eq!(n("SUMIF(B1:B3,\"red\",A1:A3)", &g), 30.0);
        assert_eq!(n("AVERAGEIF(A1:A3,\"<20\")", &g), 10.0);
    }

    #[test]
    fn serializer_round_trip() {
        for src in [
            "1+2*3",
            "(1+2)*3",
            "-A1",
            "A1+B2",
            "SUM(A1:B3,2)",
            "IF(A1>2,\"yes\",\"no\")",
            "'My Sheet'!A1",
            "Sheet1!$A$1:B2",
            "2^3^2",
            "1<=2",
            "A1&\" \"&B1",
            "10%",
            "-2^2",
            "SUM(A1,,3)",
        ] {
            let ast = parse(src).unwrap_or_else(|e| panic!("parse {src}: {e}"));
            let printed = to_string(&ast);
            let ast2 = parse(&printed).unwrap_or_else(|e| panic!("reparse {printed}: {e}"));
            assert_eq!(ast, ast2, "{src} → {printed}");
        }
    }

    #[test]
    fn translation() {
        assert_eq!(translate_formula("A1+B2", 1, 0).unwrap(), "A2+B3");
        assert_eq!(translate_formula("$A$1+B2", 1, 1).unwrap(), "$A$1+C3");
        assert_eq!(
            translate_formula("SUM(A1:A10)", 0, 2).unwrap(),
            "SUM(C1:C10)"
        );
        // Off-grid → #REF!
        assert_eq!(translate_formula("A1", -1, 0).unwrap(), "#REF!");
        assert_eq!(translate_formula("Sheet2!A1", 2, 0).unwrap(), "Sheet2!A3");
    }

    #[test]
    fn unknown_function_sets_unsupported() {
        let g = empty();
        let ast = parse("SEQUENCE(3)").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        let v = ev.eval(&ast);
        assert_eq!(v, Value::Err(ExcelError::Name));
        assert!(ev.unsupported);
        // Defined names too.
        let ast = parse("MyName+1").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        let _ = ev.eval(&ast);
        assert!(ev.unsupported);
    }

    #[test]
    fn xlfn_prefix_is_stripped() {
        let ast = parse("_xlfn.CONCAT(\"a\",\"b\")").unwrap();
        let g = empty();
        let mut ev = Eval::new(&g, 0, (0, 0));
        assert_eq!(ev.eval(&ast), Value::Str("ab".into()));
        assert!(!ev.unsupported);
    }

    #[test]
    fn error_literals() {
        let g = empty();
        assert_eq!(eval_str("#N/A", &g), Value::Err(ExcelError::NA));
        assert_eq!(eval_str("ISERROR(#DIV/0!)", &g), Value::Bool(true));
    }

    #[test]
    fn volatile_detection() {
        assert!(is_volatile(&parse("NOW()").unwrap()));
        assert!(is_volatile(&parse("1+RAND()").unwrap()));
        assert!(!is_volatile(&parse("SUM(A1:B2)").unwrap()));
    }

    #[test]
    fn collect_refs_finds_everything() {
        let ast = parse("A1+SUM(B2:C3)+Sheet2!D4").unwrap();
        let mut refs = Vec::new();
        collect_refs(&ast, &mut refs);
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0], (None, 0, 0, 0, 0));
        assert_eq!(refs[1], (None, 1, 1, 2, 2));
        assert_eq!(refs[2], (Some("Sheet2".into()), 3, 3, 3, 3));
    }

    #[test]
    fn whole_column_and_row_refs() {
        let g = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("A2", Value::Num(2.0)),
            ("A100", Value::Num(3.0)),
            ("B1", Value::Num(10.0)),
            ("C1", Value::Num(20.0)),
        ]);
        assert_eq!(n("SUM(A:A)", &g), 6.0);
        assert_eq!(n("SUM($A:$A)", &g), 6.0);
        assert_eq!(n("SUM(A:B)", &g), 16.0);
        assert_eq!(n("SUM(1:1)", &g), 31.0);
        assert_eq!(n("COUNT(A:A)", &g), 3.0);
        assert_eq!(n("COUNTIF(A:A,\">1\")", &g), 2.0);
        assert_eq!(n("SUMIF(A:A,\">1\")", &g), 5.0);
        assert_eq!(n("ROWS(A:A)", &g), crate::sheet::MAX_ROWS as f64);
        assert_eq!(n("COLUMNS(A:C)", &g), 3.0);
        // Serializer round-trips the compact form.
        for src in ["SUM(A:A)", "SUM($A:C)", "SUM(1:3)", "SUM(Sheet1!A:A)"] {
            let ast = parse(src).unwrap();
            assert_eq!(parse(&to_string(&ast)).unwrap(), ast, "{src}");
        }
        // Translation shifts relative whole-column refs.
        assert_eq!(translate_formula("SUM(A:A)", 0, 1).unwrap(), "SUM(B:B)");
        assert_eq!(translate_formula("SUM($A:$A)", 0, 5).unwrap(), "SUM($A:$A)");
        assert_eq!(translate_formula("SUM(2:3)", 1, 0).unwrap(), "SUM(3:4)");
    }

    #[test]
    fn defined_names_resolve() {
        let g = Grid::new(&[("A1", Value::Num(100.0)), ("A2", Value::Num(200.0))])
            .with_name("TaxRate", "0.25")
            .with_name("Data", "Sheet1!$A$1:$A$2");
        assert_eq!(n("TaxRate*4", &g), 1.0);
        assert_eq!(n("SUM(Data)", &g), 300.0);
        // Unknown names stay unsupported.
        let ast = parse("Mystery+1").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        assert_eq!(ev.eval(&ast), Value::Err(ExcelError::Name));
        assert!(ev.unsupported);
        // Known names do NOT mark the formula unsupported.
        let ast = parse("SUM(Data)").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        assert_eq!(ev.eval(&ast), Value::Num(300.0));
        assert!(!ev.unsupported);
    }

    #[test]
    fn indirect_and_offset() {
        let g = Grid::new(&[
            ("A1", Value::Num(7.0)),
            ("A2", Value::Num(8.0)),
            ("B1", Value::Str("A2".into())),
            ("C1", Value::Num(1.0)),
            ("C2", Value::Num(2.0)),
            ("C3", Value::Num(3.0)),
        ]);
        assert_eq!(n("INDIRECT(\"A1\")", &g), 7.0);
        assert_eq!(n("INDIRECT(B1)", &g), 8.0);
        assert_eq!(n("SUM(INDIRECT(\"C1:C3\"))", &g), 6.0);
        assert_eq!(n("INDIRECT(\"A\"&2)", &g), 8.0);
        assert_eq!(
            eval_str("INDIRECT(\"nope!!\")", &g),
            Value::Err(ExcelError::Ref)
        );
        assert_eq!(n("OFFSET(A1,1,0)", &g), 8.0);
        assert_eq!(n("SUM(OFFSET(C1,0,0,3,1))", &g), 6.0);
        assert_eq!(n("SUM(OFFSET(C2,-1,0,2,1))", &g), 3.0);
        assert_eq!(eval_str("OFFSET(A1,-5,0)", &g), Value::Err(ExcelError::Ref));
        assert!(is_volatile(&parse("INDIRECT(\"A1\")").unwrap()));
        assert!(is_volatile(&parse("SUM(OFFSET(A1,0,0,2,2))").unwrap()));
    }

    #[test]
    fn xlookup_modes() {
        let g = Grid::new(&[
            ("A1", Value::Str("apple".into())),
            ("A2", Value::Str("banana".into())),
            ("A3", Value::Str("cherry".into())),
            ("B1", Value::Num(10.0)),
            ("B2", Value::Num(20.0)),
            ("B3", Value::Num(30.0)),
            ("C1", Value::Num(5.0)),
            ("C2", Value::Num(15.0)),
            ("C3", Value::Num(25.0)),
        ]);
        assert_eq!(n("XLOOKUP(\"banana\",A1:A3,B1:B3)", &g), 20.0);
        assert_eq!(
            eval_str("XLOOKUP(\"kiwi\",A1:A3,B1:B3)", &g),
            Value::Err(ExcelError::NA)
        );
        assert_eq!(
            eval_str("XLOOKUP(\"kiwi\",A1:A3,B1:B3,\"none\")", &g),
            Value::Str("none".into())
        );
        // match_mode -1: next smaller. 20 not present in C → 15 → B2.
        assert_eq!(n("XLOOKUP(20,C1:C3,B1:B3,,-1)", &g), 20.0);
        // match_mode 1: next larger. 20 → 25 → B3.
        assert_eq!(n("XLOOKUP(20,C1:C3,B1:B3,,1)", &g), 30.0);
        // wildcards with match_mode 2.
        assert_eq!(n("XLOOKUP(\"che*\",A1:A3,B1:B3,,2)", &g), 30.0);
    }

    #[test]
    fn ifs_family_and_switch() {
        let g = Grid::new(&[
            ("A1", Value::Str("east".into())),
            ("A2", Value::Str("west".into())),
            ("A3", Value::Str("east".into())),
            ("B1", Value::Num(10.0)),
            ("B2", Value::Num(20.0)),
            ("B3", Value::Num(30.0)),
            ("C1", Value::Num(1.0)),
            ("C2", Value::Num(2.0)),
            ("C3", Value::Num(2.0)),
        ]);
        assert_eq!(n("COUNTIFS(A1:A3,\"east\",C1:C3,2)", &g), 1.0);
        assert_eq!(n("SUMIFS(B1:B3,A1:A3,\"east\",C1:C3,\">=1\")", &g), 40.0);
        assert_eq!(n("AVERAGEIFS(B1:B3,A1:A3,\"east\")", &g), 20.0);
        assert_eq!(n("MAXIFS(B1:B3,A1:A3,\"east\")", &g), 30.0);
        assert_eq!(n("MINIFS(B1:B3,A1:A3,\"east\")", &g), 10.0);
        assert_eq!(n("IFS(FALSE,1,TRUE,2)", &g), 2.0);
        assert_eq!(eval_str("IFS(FALSE,1)", &g), Value::Err(ExcelError::NA));
        assert_eq!(
            eval_str("SWITCH(2,1,\"one\",2,\"two\",\"other\")", &g),
            Value::Str("two".into())
        );
        assert_eq!(
            eval_str("SWITCH(9,1,\"one\",\"other\")", &g),
            Value::Str("other".into())
        );
    }

    #[test]
    fn more_dates() {
        let g = empty();
        // 2024-01-31 = 45322; EDATE +1 month clamps to Feb 29 (leap year).
        assert_eq!(n("EDATE(45322,1)", &g), n("DATE(2024,2,29)", &g));
        assert_eq!(n("EOMONTH(45306,0)", &g), n("DATE(2024,1,31)", &g));
        assert_eq!(n("EOMONTH(45306,1)", &g), n("DATE(2024,2,29)", &g));
        assert_eq!(n("DATEDIF(DATE(2020,1,15),DATE(2024,3,10),\"Y\")", &g), 4.0);
        assert_eq!(
            n("DATEDIF(DATE(2020,1,15),DATE(2024,3,10),\"M\")", &g),
            49.0
        );
        assert_eq!(n("DATEDIF(DATE(2024,1,1),DATE(2024,1,15),\"D\")", &g), 14.0);
        assert_eq!(
            n("DATEDIF(DATE(2020,1,15),DATE(2024,3,10),\"YM\")", &g),
            1.0
        );
        // 2024-01-15 (Monday): week 3 both US modes; ISO week 3 too.
        assert_eq!(n("WEEKNUM(45306)", &g), 3.0);
        assert_eq!(n("WEEKNUM(45306,2)", &g), 3.0);
        assert_eq!(n("ISOWEEKNUM(45306)", &g), 3.0);
        // 2023-01-01 (Sunday) is ISO week 52 of 2022.
        assert_eq!(n("ISOWEEKNUM(DATE(2023,1,1))", &g), 52.0);
    }

    #[test]
    fn financial_functions() {
        let g = empty();
        // Canonical Excel examples.
        let pmt = n("PMT(0.08/12,10,10000)", &g);
        assert!((pmt - -1037.0320893).abs() < 1e-6, "{pmt}");
        let fv = n("FV(0.06/12,10,-200,-500,1)", &g);
        assert!((fv - 2581.4033740).abs() < 1e-6, "{fv}");
        let pv = n("PV(0.08/12,20*12,500,,0)", &g);
        assert!((pv - -59777.145851).abs() < 1e-5, "{pv}");
        let nper = n("NPER(0.12/12,-100,-1000,10000,1)", &g);
        assert!((nper - 59.6738656742).abs() < 1e-6, "{nper}");
        let npv = n("NPV(0.1,-10000,3000,4200,6800)", &g);
        assert!((npv - 1188.4434123).abs() < 1e-6, "{npv}");
        let g2 = Grid::new(&[
            ("A1", Value::Num(-70000.0)),
            ("A2", Value::Num(12000.0)),
            ("A3", Value::Num(15000.0)),
            ("A4", Value::Num(18000.0)),
            ("A5", Value::Num(21000.0)),
            ("A6", Value::Num(26000.0)),
        ]);
        let irr = n("IRR(A1:A6)", &g2);
        assert!((irr - 0.086630948036).abs() < 1e-6, "{irr}");
        let rate = n("RATE(4*12,-200,8000)", &g);
        assert!((rate - 0.0077014724882).abs() < 1e-8, "{rate}");
    }

    #[test]
    fn more_statistics() {
        let g = Grid::new(&[
            ("A1", Value::Num(7.0)),
            ("A2", Value::Num(3.5)),
            ("A3", Value::Num(3.5)),
            ("A4", Value::Num(1.0)),
            ("A5", Value::Num(2.0)),
        ]);
        assert_eq!(n("RANK(3.5,A1:A5)", &g), 2.0);
        assert_eq!(n("RANK(3.5,A1:A5,1)", &g), 3.0);
        assert_eq!(n("MODE(A1:A5)", &g), 3.5);
        assert_eq!(n("PERCENTILE(A1:A5,0.5)", &g), 3.5);
        assert_eq!(n("QUARTILE(A1:A5,0)", &g), 1.0);
        assert_eq!(n("QUARTILE(A1:A5,4)", &g), 7.0);
        assert_eq!(n("COMBIN(8,2)", &g), 28.0);
        assert_eq!(n("PERMUT(8,2)", &g), 56.0);
        assert_eq!(n("SUMSQ(3,4)", &g), 25.0);
        assert_eq!(n("LOOKUP(3.6,A4:A5)", &g), 2.0);
        assert_eq!(
            eval_str("HYPERLINK(\"http://x.example\",\"link\")", &g),
            Value::Str("link".into())
        );
    }

    #[test]
    fn text_function_best_effort() {
        let g = empty();
        assert_eq!(
            eval_str("TEXT(1234.567,\"#,##0.00\")", &g),
            Value::Str("1,234.57".into())
        );
        assert_eq!(
            eval_str("TEXT(0.285,\"0.0%\")", &g),
            Value::Str("28.5%".into())
        );
        assert_eq!(
            eval_str("TEXT(45306,\"yyyy-mm-dd\")", &g),
            Value::Str("2024-01-15".into())
        );
        // A format we can't honestly render must mark unsupported, not guess.
        let ast = parse("TEXT(1234,\"$#,##0;[Red]($#,##0)\")").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        let _ = ev.eval(&ast);
        // ($#,##0 classifies as thousands-number, so this one actually works;
        // use a truly opaque code instead.)
        let ast = parse("TEXT(1234,\"\"\"kg\"\"\")").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        let _ = ev.eval(&ast);
        assert!(ev.unsupported);
    }
}
