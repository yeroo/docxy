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
    /// A defined name (or anything identifier-like we don't model yet).
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
            t => Err(format!("expected reference after sheet name, got {t:?}")),
        }
    }

    /// An identifier outside call position: cell ref, range start, TRUE/FALSE,
    /// or a defined name.
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
        if sheet.is_some() {
            return Err("sheet-qualified name".into());
        }
        Ok(Expr::Name(id))
    }
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

/// Does the formula call a volatile function (must recalc on every pass)?
pub fn is_volatile(e: &Expr) -> bool {
    match e {
        Expr::Func(name, args) => {
            matches!(name.as_str(), "NOW" | "TODAY" | "RAND" | "RANDBETWEEN")
                || args.iter().any(is_volatile)
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
}

/// One evaluation: tracks the current sheet/cell (for `ROW()`, sheet-less
/// refs) and whether anything unsupported was hit.
pub struct Eval<'a> {
    pub res: &'a dyn Resolver,
    pub sheet: usize,
    pub cell: (u32, u32),
    /// Set when the formula used something we don't model (unknown function,
    /// defined name, missing clock…). The engine then keeps the cached value.
    pub unsupported: bool,
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
            Expr::Name(_) => {
                // Defined names arrive in phase B; don't guess.
                self.unsupported = true;
                Arg::Scalar(Value::Err(ExcelError::Name))
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
            Expr::Func(name, args) => Arg::Scalar(self.call(name, args)),
        }
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
                        Arg::Range(s, r1, c1, r2, c2) => rects.push((s, r1, c1, r2, c2)),
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
                Value::Str(s.split_whitespace().collect::<Vec<_>>().join(" "))
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
                _ => Value::Err(ExcelError::Value),
            },
            "COLUMNS" => match args {
                [Expr::Range(a, b)] => Value::Num((a.col - b.col).abs() as f64 + 1.0),
                [Expr::Ref(_)] => Value::Num(1.0),
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
                let lanes = if vertical { r1..=r2 } else { c1..=c2 };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test resolver over a single sheet of literal values.
    struct Grid {
        cells: HashMap<(u32, u32), Value>,
    }

    impl Grid {
        fn new(cells: &[(&str, Value)]) -> Grid {
            let mut m = HashMap::new();
            for (name, v) in cells {
                let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
                m.insert((r, c), v.clone());
            }
            Grid { cells: m }
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
        let ast = parse("XLOOKUP(1,A1:A3,B1:B3)").unwrap();
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
}
