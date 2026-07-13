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

/// Which region of a table a structured reference addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableItem {
    /// The data region (between header and totals) — the default.
    Data,
    All,
    Headers,
    Totals,
    /// `@` — the formula's own row.
    ThisRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Pos,
    /// Postfix `%`.
    Percent,
    /// Prefix `@` — implicit intersection (stored as `_xlfn.SINGLE(…)`).
    Implicit,
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
    /// A 3D span: `Sheet1:Sheet3!A1:B2` — the same rect across a run of
    /// sheets (tab order). Supported in aggregate contexts.
    Ref3D {
        first: String,
        last: String,
        a: CellRef,
        b: CellRef,
    },
    /// A structured (table) reference: `Table1[Amount]`, `[@Price]`,
    /// `Table1[[#Totals],[Amount]]`. Resolved through the workbook's table
    /// definitions at evaluation time.
    Structured {
        /// None = the table enclosing the formula's cell (bare `[@Col]`).
        table: Option<String>,
        item: TableItem,
        /// Column (or first column of a span).
        col1: Option<String>,
        /// Second column of a `[[A]:[B]]` span.
        col2: Option<String>,
    },
    /// A defined name, resolved through the workbook at evaluation time.
    Name(String),
    /// `A1#` — the spill range of the dynamic-array anchor at `A1` (stored in
    /// files as `_xlfn.ANCHORARRAY(A1)`). Resolves to the anchor's current
    /// spill extent; `#REF!` when the anchor doesn't spill.
    SpillRef(CellRef),
    /// `{1,2;3,4}` — an array constant (rows separated by `;`).
    ArrayLit(Vec<Vec<Expr>>),
    Func(String, Vec<Expr>),
    /// Calling the result of an expression: `LAMBDA(x,x*2)(5)`. (Named calls
    /// like `f(5)` parse as [`Expr::Func`] and resolve to a lambda at
    /// evaluation time.)
    Call(Box<Expr>, Vec<Expr>),
    /// An already-evaluated scalar, injected when a scalar function is lifted
    /// elementwise over an array. Never produced by the parser.
    Lit(Value),
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
    /// `[...]` — a structured-reference spec, raw inner text (nesting kept).
    Bracket(String),
    Err(ExcelError),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Colon,
    Bang,
    At,
    /// Postfix `#` (spill reference).
    Hash,
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
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b';' => Tok::Semi,
            b'@' => Tok::At,
            b'[' => {
                // Scan to the matching ']' (structured refs nest one level:
                // Table1[[#Totals],[My Col]]); a single quote escapes the
                // next character inside the spec.
                let start = self.pos;
                let mut depth = 1usize;
                while self.pos < self.src.len() {
                    match self.src[self.pos] {
                        b'\'' => {
                            self.pos += 1; // escape: skip the next byte too
                            if self.pos < self.src.len() {
                                self.pos += 1;
                            }
                            continue;
                        }
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                if depth != 0 {
                    return Err("unterminated [ in structured reference".into());
                }
                let inner = std::str::from_utf8(&self.src[start..self.pos])
                    .map_err(|_| "bad structured reference".to_string())?
                    .to_string();
                self.pos += 1; // consume ']'
                Tok::Bracket(inner)
            }
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
                    // A bare `#` (nothing error-like after it) is the postfix
                    // spill-reference operator: `A1#`.
                    None if lit == "#" => Tok::Hash,
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
    /// Recursion depth of the descent (bumped at each nesting point: parens,
    /// unary chains, function args). Capped so a hostile formula like
    /// `((((…))))` or `----…` from an untrusted file can't overflow the
    /// stack — it fails to parse instead, and the engine keeps the cached
    /// value like any other unparseable formula.
    depth: u32,
}

/// Cap on parser recursion. Bumped ~twice per parenthesis/unary level, so
/// this allows ~64 levels of nesting — matching Excel's own limit on nested
/// functions — while keeping the worst-case frame count small enough to stay
/// safe even on a 2 MB thread stack. Deeper input fails to parse (and the
/// engine keeps the cached value) instead of overflowing the native stack.
const MAX_PARSE_DEPTH: u32 = 128;

/// Parse a formula body (no leading `=`). Errors are strings — the engine
/// treats any parse failure as "unsupported: preserve, don't evaluate".
pub fn parse(src: &str) -> Result<Expr, String> {
    let mut lex = Lexer::new(src);
    let tok = lex.next_tok()?;
    let mut p = Parser { lex, tok, depth: 0 };
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

    /// The recursion entry points (`compare`, `unary`) run their body through
    /// this so depth is bumped and restored exactly once per level, aborting
    /// before the native stack can overflow.
    fn nest(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err("formula nesting too deep".into());
        }
        let r = body(self);
        self.depth -= 1;
        r
    }

    fn compare(&mut self) -> Result<Expr, String> {
        self.nest(Self::compare_inner)
    }

    fn compare_inner(&mut self) -> Result<Expr, String> {
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
        self.nest(Self::unary_inner)
    }

    fn unary_inner(&mut self) -> Result<Expr, String> {
        match self.tok {
            Tok::Minus => {
                self.bump()?;
                Ok(Expr::Un(UnOp::Neg, Box::new(self.unary()?)))
            }
            Tok::Plus => {
                self.bump()?;
                Ok(Expr::Un(UnOp::Pos, Box::new(self.unary()?)))
            }
            Tok::At => {
                self.bump()?;
                Ok(Expr::Un(UnOp::Implicit, Box::new(self.unary()?)))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            match self.tok {
                Tok::Percent => {
                    self.bump()?;
                    e = Expr::Un(UnOp::Percent, Box::new(e));
                }
                Tok::Hash => {
                    self.bump()?;
                    let Expr::Ref(r) = e else {
                        return Err("# after a non-cell reference".into());
                    };
                    e = Expr::SpillRef(r);
                }
                // Immediate invocation: LAMBDA(x,x*2)(5).
                Tok::LParen => {
                    self.bump()?;
                    let args = self.call_args()?;
                    e = Expr::Call(Box::new(e), args);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// An argument list after a consumed `(`, through the closing `)`.
    fn call_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.tok == Tok::RParen {
            self.bump()?;
            return Ok(args);
        }
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
        Ok(args)
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
            Tok::LBrace => {
                // `{1,2;3,4}` — an array constant. Rows must be rectangular.
                self.bump()?;
                let mut rows: Vec<Vec<Expr>> = vec![Vec::new()];
                loop {
                    rows.last_mut().unwrap().push(self.compare()?);
                    match self.tok {
                        Tok::Comma => self.bump()?,
                        Tok::Semi => {
                            self.bump()?;
                            rows.push(Vec::new());
                        }
                        Tok::RBrace => {
                            self.bump()?;
                            break;
                        }
                        _ => return Err("expected , ; or } in array constant".into()),
                    }
                }
                let w = rows[0].len();
                if rows.iter().any(|r| r.len() != w) {
                    return Err("ragged array constant".into());
                }
                Ok(Expr::ArrayLit(rows))
            }
            Tok::Quoted(name) => {
                self.bump()?;
                if self.tok == Tok::Colon {
                    self.bump()?;
                    let last = match std::mem::replace(&mut self.tok, Tok::Eof) {
                        Tok::Ident(l) => {
                            self.bump()?;
                            l
                        }
                        Tok::Quoted(l) => {
                            self.bump()?;
                            l
                        }
                        t => return Err(format!("expected sheet name after :, got {t:?}")),
                    };
                    if self.tok != Tok::Bang {
                        return Err("expected ! after 3D sheet span".into());
                    }
                    self.bump()?;
                    return self.three_d(name, last);
                }
                if self.tok != Tok::Bang {
                    return Err("quoted name without !".into());
                }
                self.bump()?;
                self.sheet_ref(Some(name))
            }
            Tok::Bracket(spec) => {
                // Bare `[@Col]` / `[Col]` — the enclosing table's reference.
                self.bump()?;
                parse_spec(None, &spec)
            }
            Tok::Ident(id) => {
                self.bump()?;
                match self.tok {
                    Tok::Bang => {
                        self.bump()?;
                        self.sheet_ref(Some(id))
                    }
                    Tok::Bracket(_) => {
                        let Tok::Bracket(spec) = std::mem::replace(&mut self.tok, Tok::Eof) else {
                            unreachable!()
                        };
                        self.bump()?;
                        parse_spec(Some(id), &spec)
                    }
                    Tok::LParen => {
                        self.bump()?;
                        let args = self.call_args()?;
                        // Excel writes post-2007 functions as _xlfn.NAME, and
                        // spells the dynamic-array operators as functions in
                        // stored formulas: `A1#` is `_xlfn.ANCHORARRAY(A1)`,
                        // `@x` is `_xlfn.SINGLE(x)`.
                        let name = id
                            .strip_prefix("_xlfn.")
                            .unwrap_or(&id)
                            .to_ascii_uppercase();
                        let name = name.strip_prefix("_XLWS.").unwrap_or(&name).to_string();
                        match (name.as_str(), args.len()) {
                            ("ANCHORARRAY", 1) => {
                                if let Expr::Ref(r) = &args[0] {
                                    return Ok(Expr::SpillRef(r.clone()));
                                }
                                Err("ANCHORARRAY needs a cell reference".into())
                            }
                            ("SINGLE", 1) => Ok(Expr::Un(
                                UnOp::Implicit,
                                Box::new(args.into_iter().next().unwrap()),
                            )),
                            _ => Ok(Expr::Func(name, args)),
                        }
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
            r.sheet = sheet.clone();
            if self.tok == Tok::Colon {
                self.bump()?;
                let (second, id2) = match std::mem::replace(&mut self.tok, Tok::Eof) {
                    Tok::Ident(id2) => {
                        self.bump()?;
                        (parse_ref_text(&id2), id2)
                    }
                    Tok::Quoted(l) => {
                        // `Q1:'My Last'!A1` — a 3D span whose first sheet
                        // name happens to look like a cell reference.
                        self.bump()?;
                        if sheet.is_some() {
                            return Err("nested sheet qualifiers".into());
                        }
                        if self.tok != Tok::Bang {
                            return Err("expected ! after 3D sheet span".into());
                        }
                        self.bump()?;
                        return self.three_d(id, l);
                    }
                    t => return Err(format!("expected range end, got {t:?}")),
                };
                // `Q1:Q3!A1` — both endpoints looked like cell refs, but the
                // trailing ! reveals a 3D sheet span (sheets named Q1..Q3).
                if self.tok == Tok::Bang {
                    if sheet.is_some() {
                        return Err("nested sheet qualifiers".into());
                    }
                    self.bump()?;
                    return self.three_d(id, id2);
                }
                let second = second.ok_or("bad range end")?;
                return Ok(Expr::Range(r, second));
            }
            return Ok(Expr::Ref(r));
        }
        // After `X:` the next tokens decide what X was: `X:Y!ref` is a 3D
        // sheet span (even when X looks like column letters — sheets can be
        // named "One"); otherwise `A:C` is a whole-column range.
        if self.tok == Tok::Colon {
            self.bump()?;
            match std::mem::replace(&mut self.tok, Tok::Eof) {
                Tok::Quoted(l) => {
                    self.bump()?;
                    if sheet.is_some() {
                        return Err("nested sheet qualifiers".into());
                    }
                    if self.tok != Tok::Bang {
                        return Err("expected ! after 3D sheet span".into());
                    }
                    self.bump()?;
                    return self.three_d(id, l);
                }
                Tok::Ident(id2) => {
                    self.bump()?;
                    if self.tok == Tok::Bang {
                        if sheet.is_some() {
                            return Err("nested sheet qualifiers".into());
                        }
                        self.bump()?;
                        return self.three_d(id, id2);
                    }
                    let (Some((c1, abs1)), Some((c2, abs2))) =
                        (parse_col_text(&id), parse_col_text(&id2))
                    else {
                        return Err("bad column range".into());
                    };
                    return Ok(Expr::ColRange {
                        sheet,
                        c1,
                        c2,
                        abs1,
                        abs2,
                    });
                }
                t => return Err(format!("expected name after :, got {t:?}")),
            }
        }
        if sheet.is_some() {
            return Err("sheet-qualified name".into());
        }
        Ok(Expr::Name(id))
    }

    /// The reference part of `First:Last!…`.
    fn three_d(&mut self, first: String, last: String) -> Result<Expr, String> {
        match std::mem::replace(&mut self.tok, Tok::Eof) {
            Tok::Ident(id) => {
                self.bump()?;
                let a = parse_ref_text(&id).ok_or("bad 3D reference")?;
                if self.tok == Tok::Colon {
                    self.bump()?;
                    let b = match std::mem::replace(&mut self.tok, Tok::Eof) {
                        Tok::Ident(id2) => {
                            self.bump()?;
                            parse_ref_text(&id2).ok_or("bad 3D range end")?
                        }
                        t => return Err(format!("expected range end, got {t:?}")),
                    };
                    return Ok(Expr::Ref3D { first, last, a, b });
                }
                let b = a.clone();
                Ok(Expr::Ref3D { first, last, a, b })
            }
            t => Err(format!("expected reference in 3D span, got {t:?}")),
        }
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

/// Unescape a structured-ref name: a single quote escapes the next char
/// (`'[`, `']`, `'#`, `'@`, `''`).
fn unescape_spec(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Split a multi-part spec body on top-level commas: `[#Totals],[Amount]` →
/// two bracketed parts. (The lexer already balanced the outer brackets.)
fn split_spec_parts(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' => i += 1, // escape: skip next
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(s[start..].trim());
    parts
}

/// One part of a spec: `#Item`, `@`, `@Name`, a column name, or a bracketed
/// version of any of those.
enum SpecPart {
    Item(TableItem),
    ThisRow(Option<String>),
    Col(String),
    Span(String, String),
}

fn parse_spec_part(raw: &str) -> Result<SpecPart, String> {
    let t = raw.trim();
    // `[A]:[B]` — a column span.
    if let Some((a, b)) = split_top_level_colon(t) {
        let name = |x: &str| -> Result<String, String> {
            let x = x.trim();
            let inner = x
                .strip_prefix('[')
                .and_then(|y| y.strip_suffix(']'))
                .unwrap_or(x);
            Ok(unescape_spec(inner))
        };
        return Ok(SpecPart::Span(name(a)?, name(b)?));
    }
    // Strip one level of brackets: `[#Totals]` → `#Totals`.
    let t = t
        .strip_prefix('[')
        .and_then(|y| y.strip_suffix(']'))
        .unwrap_or(t)
        .trim();
    if let Some(rest) = t.strip_prefix('#') {
        return match rest.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(SpecPart::Item(TableItem::All)),
            "data" => Ok(SpecPart::Item(TableItem::Data)),
            "headers" => Ok(SpecPart::Item(TableItem::Headers)),
            "totals" => Ok(SpecPart::Item(TableItem::Totals)),
            "this row" => Ok(SpecPart::Item(TableItem::ThisRow)),
            other => Err(format!("unknown table item #{other}")),
        };
    }
    if let Some(rest) = t.strip_prefix('@') {
        let rest = rest.trim();
        if rest.is_empty() {
            return Ok(SpecPart::ThisRow(None));
        }
        let inner = rest
            .strip_prefix('[')
            .and_then(|y| y.strip_suffix(']'))
            .unwrap_or(rest);
        return Ok(SpecPart::ThisRow(Some(unescape_spec(inner))));
    }
    Ok(SpecPart::Col(unescape_spec(t)))
}

/// Split `X:Y` at a top-level colon (outside brackets/escapes).
fn split_top_level_colon(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' => i += 1,
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a full bracket spec into a structured-reference expression.
fn parse_spec(table: Option<String>, spec: &str) -> Result<Expr, String> {
    let body = spec.trim();
    if body.is_empty() {
        // `Table1[]` — the whole data region.
        return Ok(Expr::Structured {
            table,
            item: TableItem::Data,
            col1: None,
            col2: None,
        });
    }
    let mut item: Option<TableItem> = None;
    let mut col1: Option<String> = None;
    let mut col2: Option<String> = None;
    for part in split_spec_parts(body) {
        match parse_spec_part(part)? {
            SpecPart::Item(i) => {
                if item.is_some() {
                    // e.g. `[#Headers],[#Data]` — not modeled yet.
                    return Err("unsupported multi-item structured reference".into());
                }
                item = Some(i);
            }
            SpecPart::ThisRow(name) => {
                item = Some(TableItem::ThisRow);
                if name.is_some() {
                    col1 = name;
                }
            }
            SpecPart::Col(name) => {
                if col1.is_none() {
                    col1 = Some(name);
                } else {
                    return Err("unsupported structured reference".into());
                }
            }
            SpecPart::Span(a, b) => {
                col1 = Some(a);
                col2 = Some(b);
            }
        }
    }
    Ok(Expr::Structured {
        table,
        item: item.unwrap_or(TableItem::Data),
        col1,
        col2,
    })
}

/// Escape a column name for printing inside a structured reference.
fn escape_spec(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if matches!(ch, '[' | ']' | '\'' | '#' | '@') {
            out.push('\'');
        }
        out.push(ch);
    }
    out
}

/// Print a structured reference back to canonical text.
fn structured_to_string(
    table: &Option<String>,
    item: TableItem,
    col1: &Option<String>,
    col2: &Option<String>,
) -> String {
    let prefix = table.clone().unwrap_or_default();
    let body = match (item, col1, col2) {
        (TableItem::Data, None, _) => String::new(),
        (TableItem::Data, Some(c), None) => escape_spec(c),
        (TableItem::Data, Some(a), Some(b)) => {
            format!("[{}]:[{}]", escape_spec(a), escape_spec(b))
        }
        (TableItem::ThisRow, None, _) => "@".to_string(),
        (TableItem::ThisRow, Some(c), _) => format!("@{}", escape_spec(c)),
        (TableItem::All, None, _) => "#All".to_string(),
        (TableItem::Headers, None, _) => "#Headers".to_string(),
        (TableItem::Totals, None, _) => "#Totals".to_string(),
        (item, Some(c), _) => {
            let tag = match item {
                TableItem::All => "#All",
                TableItem::Headers => "#Headers",
                TableItem::Totals => "#Totals",
                _ => "#Data",
            };
            format!("[{tag}],[{}]", escape_spec(c))
        }
    };
    format!("{prefix}[{body}]")
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
        Expr::Un(UnOp::Neg | UnOp::Pos | UnOp::Implicit, _) => 6,
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
        Expr::Ref3D { first, last, a, b } => {
            let f = sheet_prefix(first);
            let f = f.trim_end_matches('!');
            let l = sheet_prefix(last);
            let l = l.trim_end_matches('!');
            let head = format!("{f}:{l}!");
            if a == b {
                format!("{head}{}", ref_to_string(a))
            } else {
                format!("{head}{}:{}", ref_to_string(a), ref_to_string(b))
            }
        }
        Expr::Structured {
            table,
            item,
            col1,
            col2,
        } => structured_to_string(table, *item, col1, col2),
        Expr::Name(n) => n.clone(),
        Expr::SpillRef(r) => format!("{}#", ref_to_string(r)),
        Expr::ArrayLit(rows) => {
            let body: Vec<String> = rows
                .iter()
                .map(|row| row.iter().map(to_string).collect::<Vec<_>>().join(","))
                .collect();
            format!("{{{}}}", body.join(";"))
        }
        Expr::Func(name, args) => {
            let list: Vec<String> = args.iter().map(to_string).collect();
            format!("{}({})", name, list.join(","))
        }
        Expr::Call(callee, args) => {
            let list: Vec<String> = args.iter().map(to_string).collect();
            let head = match callee.as_ref() {
                Expr::Func(..) | Expr::Call(..) | Expr::Name(_) => to_string(callee),
                other => format!("({})", to_string(other)),
            };
            format!("{}({})", head, list.join(","))
        }
        Expr::Lit(v) => match v {
            Value::Num(n) => fmt_general(*n),
            Value::Str(s) => format!("\"{}\"", s.replace('"', "\"\"")),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            Value::Err(x) => x.code().to_string(),
            Value::Empty => String::new(),
        },
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
                UnOp::Implicit => format!("@{inner}"),
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
        Expr::SpillRef(r) => Expr::SpillRef(translate_ref(r, dr, dc)),
        Expr::ArrayLit(rows) => Expr::ArrayLit(
            rows.iter()
                .map(|row| row.iter().map(|x| translate(x, dr, dc)).collect())
                .collect(),
        ),
        Expr::Range(a, b) => Expr::Range(translate_ref(a, dr, dc), translate_ref(b, dr, dc)),
        Expr::Ref3D { first, last, a, b } => Expr::Ref3D {
            first: first.clone(),
            last: last.clone(),
            a: translate_ref(a, dr, dc),
            b: translate_ref(b, dr, dc),
        },
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
        Expr::Call(callee, args) => Expr::Call(
            Box::new(translate(callee, dr, dc)),
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
        // A spill ref follows its anchor cell.
        Expr::SpillRef(r) => match recur(&Expr::Ref(r.clone())) {
            Expr::Ref(r2) => Expr::SpillRef(r2),
            other => other,
        },
        Expr::ArrayLit(rows) => Expr::ArrayLit(
            rows.iter()
                .map(|row| row.iter().map(recur).collect())
                .collect(),
        ),
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
        Expr::Call(callee, args) => {
            Expr::Call(Box::new(recur(callee)), args.iter().map(recur).collect())
        }
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
            Expr::SpillRef(r) => Expr::SpillRef(CellRef {
                sheet: fix(&r.sheet),
                ..r.clone()
            }),
            Expr::ArrayLit(rows) => Expr::ArrayLit(
                rows.iter()
                    .map(|row| row.iter().map(|x| walk(x, old, new)).collect())
                    .collect(),
            ),
            Expr::Ref3D { first, last, a, b } => {
                let ren = |n: &String| -> String {
                    if n.eq_ignore_ascii_case(old) {
                        new.to_string()
                    } else {
                        n.clone()
                    }
                };
                Expr::Ref3D {
                    first: ren(first),
                    last: ren(last),
                    a: a.clone(),
                    b: b.clone(),
                }
            }
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
            Expr::Call(callee, args) => Expr::Call(
                Box::new(walk(callee, old, new)),
                args.iter().map(|a| walk(a, old, new)).collect(),
            ),
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
        // The anchor cell itself; the engine widens spill refs to the
        // anchor's current extent separately (see `collect_spillrefs`).
        Expr::Ref(r) | Expr::SpillRef(r) => {
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
        Expr::Call(callee, args) => {
            collect_refs(callee, out);
            for a in args {
                collect_refs(a, out);
            }
        }
        Expr::ArrayLit(rows) => {
            for row in rows {
                for x in row {
                    collect_refs(x, out);
                }
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

/// Collect every spill reference (`A1#`) in a formula: (sheet, row, col) of
/// the anchor. The engine widens each to the anchor's current spill extent.
pub fn collect_spillrefs(e: &Expr, out: &mut Vec<(Option<String>, u32, u32)>) {
    match e {
        Expr::SpillRef(r) => {
            if r.row >= 0 && r.col >= 0 {
                out.push((r.sheet.clone(), r.row as u32, r.col as u32));
            }
        }
        Expr::Func(_, args) => {
            for a in args {
                collect_spillrefs(a, out);
            }
        }
        Expr::Call(callee, args) => {
            collect_spillrefs(callee, out);
            for a in args {
                collect_spillrefs(a, out);
            }
        }
        Expr::ArrayLit(rows) => {
            for row in rows {
                for x in row {
                    collect_spillrefs(x, out);
                }
            }
        }
        Expr::Un(_, x) => collect_spillrefs(x, out),
        Expr::Bin(_, l, r) => {
            collect_spillrefs(l, out);
            collect_spillrefs(r, out);
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
        Expr::Call(callee, args) => {
            collect_names(callee, out);
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

/// Collect every 3D span in a formula: (first, last, r1, c1, r2, c2).
#[allow(clippy::type_complexity)]
pub fn collect_ref3d(e: &Expr, out: &mut Vec<(String, String, u32, u32, u32, u32)>) {
    match e {
        Expr::Ref3D { first, last, a, b } => {
            if a.row >= 0 && a.col >= 0 && b.row >= 0 && b.col >= 0 {
                out.push((
                    first.clone(),
                    last.clone(),
                    a.row.min(b.row) as u32,
                    a.col.min(b.col) as u32,
                    a.row.max(b.row) as u32,
                    a.col.max(b.col) as u32,
                ));
            }
        }
        Expr::Func(_, args) => {
            for a in args {
                collect_ref3d(a, out);
            }
        }
        Expr::Call(callee, args) => {
            collect_ref3d(callee, out);
            for a in args {
                collect_ref3d(a, out);
            }
        }
        Expr::Un(_, x) => collect_ref3d(x, out),
        Expr::Bin(_, l, r) => {
            collect_ref3d(l, out);
            collect_ref3d(r, out);
        }
        _ => {}
    }
}

/// Collect every structured (table) reference in a formula. `None` table =
/// the enclosing table of the formula's own cell.
#[allow(clippy::type_complexity)]
pub fn collect_structured(
    e: &Expr,
    out: &mut Vec<(Option<String>, TableItem, Option<String>, Option<String>)>,
) {
    match e {
        Expr::Structured {
            table,
            item,
            col1,
            col2,
        } => out.push((table.clone(), *item, col1.clone(), col2.clone())),
        Expr::Func(_, args) => {
            for a in args {
                collect_structured(a, out);
            }
        }
        Expr::Call(callee, args) => {
            collect_structured(callee, out);
            for a in args {
                collect_structured(a, out);
            }
        }
        Expr::Un(_, x) => collect_structured(x, out),
        Expr::Bin(_, l, r) => {
            collect_structured(l, out);
            collect_structured(r, out);
        }
        _ => {}
    }
}

/// Collect every function-call name in a formula. The engine checks these
/// against the workbook's defined names: a name defined as `LAMBDA(...)` is a
/// custom function, and its body's references become dependencies.
pub fn collect_called_names(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Func(name, args) => {
            out.push(name.clone());
            for a in args {
                collect_called_names(a, out);
            }
        }
        Expr::Call(callee, args) => {
            collect_called_names(callee, out);
            for a in args {
                collect_called_names(a, out);
            }
        }
        Expr::ArrayLit(rows) => {
            for row in rows {
                for x in row {
                    collect_called_names(x, out);
                }
            }
        }
        Expr::Un(_, x) => collect_called_names(x, out),
        Expr::Bin(_, l, r) => {
            collect_called_names(l, out);
            collect_called_names(r, out);
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
                "NOW"
                    | "TODAY"
                    | "RAND"
                    | "RANDBETWEEN"
                    | "RANDARRAY"
                    | "INDIRECT"
                    | "OFFSET"
                    | "CELL"
                    | "INFO"
            ) || args.iter().any(is_volatile)
        }
        Expr::ArrayLit(rows) => rows.iter().flatten().any(is_volatile),
        Expr::Call(callee, args) => is_volatile(callee) || args.iter().any(is_volatile),
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
    /// An Excel Table by displayName (for structured references).
    fn table(&self, name: &str) -> Option<TableInfo> {
        let _ = name;
        None
    }
    /// The table containing a cell (for bare `[@Col]` references).
    fn table_at(&self, sheet: usize, row: u32, col: u32) -> Option<TableInfo> {
        let _ = (sheet, row, col);
        None
    }
    /// (rows, cols) of the dynamic-array spill anchored at a cell, including
    /// the anchor itself. `None` = the cell doesn't anchor a spill (an `A1#`
    /// reference to it is `#REF!`).
    fn spill_extent(&self, sheet: usize, row: u32, col: u32) -> Option<(u32, u32)> {
        let _ = (sheet, row, col);
        None
    }
    /// The source formula text of a cell (without the leading `=`), if it holds
    /// one. Used by `FORMULATEXT` and by `SUBTOTAL`/`AGGREGATE` to exclude cells
    /// that are themselves nested subtotals.
    fn cell_formula(&self, sheet: usize, row: u32, col: u32) -> Option<String> {
        let _ = (sheet, row, col);
        None
    }
    /// Whether a worksheet row is hidden (manually or by a filter). Used by the
    /// `10x` `SUBTOTAL` codes and the hidden-ignoring `AGGREGATE` options.
    fn row_hidden(&self, sheet: usize, row: u32) -> bool {
        let _ = (sheet, row);
        false
    }
}

/// A table's geometry, as the evaluator needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct TableInfo {
    pub sheet: usize,
    /// Full region incl. header/totals rows (r1, c1, r2, c2), 0-based.
    pub range: (u32, u32, u32, u32),
    pub header_rows: u32,
    pub totals_rows: u32,
    pub columns: Vec<String>,
}

impl TableInfo {
    /// Resolve one structured reference against this table. `cur_row` is the
    /// formula's own row (for `@`). Returns the rect, or None → `#REF!`.
    pub fn resolve(
        &self,
        item: TableItem,
        col1: &Option<String>,
        col2: &Option<String>,
        cur_row: u32,
    ) -> Option<(u32, u32, u32, u32)> {
        let (r1, c1, r2, c2) = self.range;
        let col_of = |name: &str| -> Option<u32> {
            self.columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(name))
                .map(|i| c1 + i as u32)
        };
        let (cc1, cc2) = match (col1, col2) {
            (None, _) => (c1, c2),
            (Some(a), None) => {
                let c = col_of(a)?;
                (c, c)
            }
            (Some(a), Some(b)) => {
                let x = col_of(a)?;
                let y = col_of(b)?;
                (x.min(y), x.max(y))
            }
        };
        let (rr1, rr2) = match item {
            TableItem::All => (r1, r2),
            TableItem::Data => {
                let lo = r1 + self.header_rows;
                let hi = r2.checked_sub(self.totals_rows)?;
                if lo > hi {
                    return None;
                }
                (lo, hi)
            }
            TableItem::Headers => {
                if self.header_rows == 0 {
                    return None;
                }
                (r1, r1 + self.header_rows - 1)
            }
            TableItem::Totals => {
                if self.totals_rows == 0 {
                    return None;
                }
                (r2 + 1 - self.totals_rows, r2)
            }
            TableItem::ThisRow => {
                let lo = r1 + self.header_rows;
                let hi = r2.checked_sub(self.totals_rows)?;
                if cur_row < lo || cur_row > hi {
                    return None;
                }
                (cur_row, cur_row)
            }
        };
        Some((rr1, cc1, rr2, cc2))
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
    /// `LET` bindings currently in scope, innermost last.
    lets: Vec<(String, Arg)>,
    /// Lambda parameters omitted at the current call site (`ISOMITTED`).
    omitted: Vec<String>,
}

/// A matrix of computed values (a dynamic-array result). Always non-empty
/// and rectangular.
pub type Matrix = Vec<Vec<Value>>;

/// Ceiling on materialized array size (cells). Excel errors with `#NUM!`
/// when an array result won't fit; we draw the line well before memory pain.
const MAX_ARRAY_CELLS: u64 = 2_000_000;

/// An evaluated argument: scalar, a still-lazy range, a computed array, or
/// a lambda (a function value awaiting invocation).
#[derive(Clone)]
enum Arg {
    Scalar(Value),
    Range(usize, u32, u32, u32, u32),
    Matrix(Matrix),
    Lambda(Box<LambdaVal>),
}

/// A `LAMBDA` value: parameters (name, optional?), unevaluated body, and
/// the `LET` bindings visible where it was defined (lexical capture).
#[derive(Clone)]
struct LambdaVal {
    params: Vec<(String, bool)>,
    body: Expr,
    captured: Vec<(String, Arg)>,
}

/// A formula's overall result: a single value, or an array to be spilled
/// into the cells below/right of the anchor.
pub enum DynResult {
    Scalar(Value),
    Array(Matrix),
}

impl<'a> Eval<'a> {
    pub fn new(res: &'a dyn Resolver, sheet: usize, cell: (u32, u32)) -> Self {
        Eval {
            res,
            sheet,
            cell,
            unsupported: false,
            depth: 0,
            lets: Vec::new(),
            omitted: Vec::new(),
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
            Arg::Matrix(m) => {
                if m.len() == 1 && m[0].len() == 1 {
                    m[0][0].clone()
                } else {
                    Value::Err(ExcelError::Value)
                }
            }
            // A lambda that never got called (Excel shows #CALC!).
            Arg::Lambda(_) => Value::Err(ExcelError::Calc),
        }
    }

    /// Evaluate a whole formula with dynamic-array semantics: a multi-cell
    /// range or computed array becomes an [`DynResult::Array`] for the engine
    /// to spill; everything else stays scalar.
    pub fn eval_dynamic(&mut self, e: &Expr) -> DynResult {
        self.eval_dynamic_as(e, true)
    }

    /// As [`Self::eval_dynamic`], but `spill = false` for a **legacy** formula
    /// (one not saved as a dynamic array, `t="array"`): a multi-cell *range* it
    /// produces (e.g. from `INDIRECT`, `OFFSET`, or a table column) is reduced by
    /// **implicit intersection** to the value on the formula's own row/column,
    /// exactly as pre-dynamic-array Excel does, instead of spilling.
    pub fn eval_dynamic_as(&mut self, e: &Expr, spill: bool) -> DynResult {
        match self.eval_arg(e) {
            Arg::Scalar(v) => DynResult::Scalar(v),
            Arg::Range(s, r1, c1, r2, c2) => {
                let (r1, c1, r2, c2) = self.clamp_huge(s, r1, c1, r2, c2);
                if r1 == r2 && c1 == c2 {
                    DynResult::Scalar(self.res.value(s, r1, c1))
                } else if !spill {
                    DynResult::Scalar(self.implicit_intersect_range(s, r1, c1, r2, c2))
                } else {
                    DynResult::Array(self.range_matrix(s, r1, c1, r2, c2))
                }
            }
            Arg::Matrix(m) => {
                if m.len() == 1 && m[0].len() == 1 {
                    DynResult::Scalar(m[0][0].clone())
                } else {
                    DynResult::Array(m)
                }
            }
            Arg::Lambda(_) => DynResult::Scalar(Value::Err(ExcelError::Calc)),
        }
    }

    /// Implicit intersection of a range against the formula's cell: a single
    /// column picks the formula's row, a single row picks its column (both must
    /// fall inside the range, else `#VALUE!`); a 2-D range has no intersection.
    fn implicit_intersect_range(&self, s: usize, r1: u32, c1: u32, r2: u32, c2: u32) -> Value {
        let (row, col) = self.cell;
        if c1 == c2 {
            if row >= r1 && row <= r2 {
                return self.res.value(s, row, c1);
            }
        } else if r1 == r2 && col >= c1 && col <= c2 {
            return self.res.value(s, r1, col);
        }
        Value::Err(ExcelError::Value)
    }

    /// Materialize a rect as a matrix (dense, empties included).
    fn range_matrix(&self, s: usize, r1: u32, c1: u32, r2: u32, c2: u32) -> Matrix {
        (r1..=r2)
            .map(|r| (c1..=c2).map(|c| self.res.value(s, r, c)).collect())
            .collect()
    }

    /// Clamp only oversized (whole-column/row style) rects to the used range;
    /// explicit small ranges keep their exact shape so `=A1:B5` spills 5×2
    /// even past the used area (as Excel does).
    fn clamp_huge(&self, s: usize, r1: u32, c1: u32, r2: u32, c2: u32) -> (u32, u32, u32, u32) {
        if r2 - r1 >= 65_535 || c2 - c1 >= 16_383 {
            self.clamp(s, r1, c1, r2, c2)
        } else {
            (r1, c1, r2, c2)
        }
    }

    /// Any argument as a matrix: scalars become 1×1, ranges materialize.
    /// Scalar errors propagate.
    fn materialize(&mut self, a: Arg) -> Result<Matrix, ExcelError> {
        match a {
            Arg::Scalar(Value::Err(e)) => Err(e),
            Arg::Scalar(v) => Ok(vec![vec![v]]),
            Arg::Range(s, r1, c1, r2, c2) => {
                let (r1, c1, r2, c2) = self.clamp_huge(s, r1, c1, r2, c2);
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > MAX_ARRAY_CELLS {
                    return Err(ExcelError::Num);
                }
                Ok(self.range_matrix(s, r1, c1, r2, c2))
            }
            Arg::Matrix(m) => Ok(m),
            Arg::Lambda(_) => Err(ExcelError::Calc),
        }
    }

    /// Elementwise binary op with Excel's broadcast rules: a 1-sized axis
    /// stretches; positions outside a non-conforming operand get `#N/A`.
    fn broadcast_bin(&mut self, op: BinOp, l: Arg, r: Arg) -> Arg {
        if let (Arg::Scalar(a), Arg::Scalar(b)) = (&l, &r) {
            return Arg::Scalar(bin_op(op, a, b));
        }
        let lm = match self.materialize(l) {
            Ok(m) => m,
            Err(e) => return Arg::Scalar(Value::Err(e)),
        };
        let rm = match self.materialize(r) {
            Ok(m) => m,
            Err(e) => return Arg::Scalar(Value::Err(e)),
        };
        let (lr, lc) = (lm.len(), lm[0].len());
        let (rr, rc) = (rm.len(), rm[0].len());
        let rows = lr.max(rr);
        let cols = lc.max(rc);
        let pick = |m: &Matrix, mr: usize, mc: usize, i: usize, j: usize| -> Option<Value> {
            let ri = if mr == 1 { 0 } else { i };
            let ci = if mc == 1 { 0 } else { j };
            if ri < mr && ci < mc {
                Some(m[ri][ci].clone())
            } else {
                None
            }
        };
        let out: Matrix = (0..rows)
            .map(|i| {
                (0..cols)
                    .map(
                        |j| match (pick(&lm, lr, lc, i, j), pick(&rm, rr, rc, i, j)) {
                            (Some(a), Some(b)) => bin_op(op, &a, &b),
                            _ => Value::Err(ExcelError::NA),
                        },
                    )
                    .collect()
            })
            .collect();
        Arg::Matrix(out)
    }

    /// Elementwise unary op (`-`, `+`, `%`) over a non-scalar operand.
    fn broadcast_un(&mut self, op: UnOp, x: Arg) -> Arg {
        let un = |v: &Value| -> Value {
            match op {
                UnOp::Neg => match to_num(v) {
                    Ok(n) => num(-n),
                    Err(e) => Value::Err(e),
                },
                UnOp::Pos => v.clone(),
                UnOp::Percent => match to_num(v) {
                    Ok(n) => num(n / 100.0),
                    Err(e) => Value::Err(e),
                },
                UnOp::Implicit => v.clone(),
            }
        };
        match x {
            Arg::Scalar(v) => Arg::Scalar(un(&v)),
            other => match self.materialize(other) {
                Ok(m) => Arg::Matrix(
                    m.into_iter()
                        .map(|row| row.iter().map(&un).collect())
                        .collect(),
                ),
                Err(e) => Arg::Scalar(Value::Err(e)),
            },
        }
    }

    /// `@x` — implicit intersection: pick the operand's value in the
    /// formula's own row (single-column ranges) or column (single-row
    /// ranges); a computed array yields its top-left value.
    fn implicit_intersect(&mut self, x: Arg) -> Arg {
        let (cur_r, cur_c) = self.cell;
        Arg::Scalar(match x {
            Arg::Scalar(v) => v,
            Arg::Range(s, r1, c1, r2, c2) => {
                if r1 == r2 && c1 == c2 {
                    self.res.value(s, r1, c1)
                } else if c1 == c2 && cur_r >= r1 && cur_r <= r2 {
                    self.res.value(s, cur_r, c1)
                } else if r1 == r2 && cur_c >= c1 && cur_c <= c2 {
                    self.res.value(s, r1, cur_c)
                } else {
                    Value::Err(ExcelError::Value)
                }
            }
            Arg::Matrix(m) => m[0][0].clone(),
            Arg::Lambda(_) => Value::Err(ExcelError::Calc),
        })
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
            Expr::Lit(v) => Arg::Scalar(v.clone()),
            // Immediate invocation: LAMBDA(x,x*2)(5).
            Expr::Call(callee, args) => {
                let f = self.eval_arg(callee);
                match f {
                    Arg::Lambda(lam) => {
                        let vals: Vec<Arg> = args.iter().map(|a| self.eval_arg(a)).collect();
                        let omitted: Vec<bool> =
                            args.iter().map(|a| matches!(a, Expr::Missing)).collect();
                        self.invoke_lambda(&lam, vals, &omitted)
                    }
                    Arg::Scalar(v) if v.is_err() => Arg::Scalar(v),
                    _ => Arg::Scalar(Value::Err(ExcelError::Value)),
                }
            }
            // 3D spans only make sense in aggregate argument positions,
            // where the caller expands them via [`Self::resolve_3d`].
            Expr::Ref3D { .. } => Arg::Scalar(Value::Err(ExcelError::Value)),
            Expr::Structured {
                table,
                item,
                col1,
                col2,
            } => {
                let info = match table {
                    Some(name) => self.res.table(name),
                    None => self.res.table_at(self.sheet, self.cell.0, self.cell.1),
                };
                let Some(info) = info else {
                    // Table definitions we can't see (or a bare ref outside
                    // any table) — don't guess.
                    self.unsupported = true;
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                };
                match info.resolve(*item, col1, col2, self.cell.0) {
                    Some((r1, c1, r2, c2)) => Arg::Range(info.sheet, r1, c1, r2, c2),
                    None => Arg::Scalar(Value::Err(ExcelError::Ref)),
                }
            }
            Expr::SpillRef(r) => {
                if r.row < 0 || r.col < 0 {
                    return Arg::Scalar(Value::Err(ExcelError::Ref));
                }
                match self.resolve_sheet(&r.sheet) {
                    Ok(s) => {
                        let (ar, ac) = (r.row as u32, r.col as u32);
                        match self.res.spill_extent(s, ar, ac) {
                            Some((h, w)) => Arg::Range(s, ar, ac, ar + h - 1, ac + w - 1),
                            None => Arg::Scalar(Value::Err(ExcelError::Ref)),
                        }
                    }
                    Err(v) => Arg::Scalar(v),
                }
            }
            Expr::ArrayLit(rows) => Arg::Matrix(
                rows.iter()
                    .map(|row| row.iter().map(|x| self.eval(x)).collect())
                    .collect(),
            ),
            Expr::Name(n) => {
                // `LET` bindings shadow workbook names, innermost first.
                if let Some(i) = self
                    .lets
                    .iter()
                    .rposition(|(name, _)| name.eq_ignore_ascii_case(n))
                {
                    return self.lets[i].1.clone();
                }
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
            Expr::Un(UnOp::Implicit, x) => {
                let v = self.eval_arg(x);
                self.implicit_intersect(v)
            }
            Expr::Un(op, x) => {
                let v = self.eval_arg(x);
                self.broadcast_un(*op, v)
            }
            Expr::Bin(op, l, r) => {
                let lv = self.eval_arg(l);
                let rv = self.eval_arg(r);
                self.broadcast_bin(*op, lv, rv)
            }
            // INDIRECT and OFFSET can *return references*, so they are
            // resolved here where a range result is expressible. Both are
            // volatile (the dependency graph can't see through them).
            Expr::Func(name, args) if name == "INDIRECT" => self.indirect(args),
            Expr::Func(name, args) if name == "OFFSET" => self.offset(args),
            // LET and the dynamic-array functions return arrays/ranges, so
            // they resolve here where non-scalar results are expressible.
            Expr::Func(name, args) if name == "LET" => self.let_fn(args),
            Expr::Func(name, args) if name == "LAMBDA" => self.lambda_fn(args),
            // MATCH lifts over an array first argument (returns positions).
            Expr::Func(name, args) if name == "MATCH" => self.match_call(args),
            // ISOMITTED reflects the current lambda call's omitted params —
            // it needs the argument's *name*, not its value.
            Expr::Func(name, args) if name == "ISOMITTED" => {
                if args.len() != 1 {
                    return Arg::Scalar(Value::Err(ExcelError::Value));
                }
                let hit = match &args[0] {
                    Expr::Name(n) => self.omitted.iter().any(|o| o.eq_ignore_ascii_case(n)),
                    _ => false,
                };
                Arg::Scalar(Value::Bool(hit))
            }
            Expr::Func(name, args) if is_higher_order_fn(name) => self.ho_fn(name, args),
            Expr::Func(name, args) => {
                let invoke = |ev: &mut Self, lam: &LambdaVal| {
                    let vals: Vec<Arg> = args.iter().map(|a| ev.eval_arg(a)).collect();
                    let omitted: Vec<bool> =
                        args.iter().map(|a| matches!(a, Expr::Missing)).collect();
                    ev.invoke_lambda(lam, vals, &omitted)
                };
                // An explicit LET-bound lambda overrides even a builtin.
                if let Some(lam) = self.let_lambda(name) {
                    return invoke(self, &lam);
                }
                if is_array_fn(name) {
                    return self.array_fn(name, args);
                }
                if is_liftable_fn(name) {
                    return self.lift_call(name, args);
                }
                let saved = self.unsupported;
                let v = self.call(name, args);
                // A workbook-defined LAMBDA (custom function) applies only when
                // the name isn't a builtin — Excel keeps builtins even if a
                // same-named defined name exists.
                if matches!(v, Value::Err(ExcelError::Name)) {
                    if let Some(lam) = self.defined_lambda(name) {
                        self.unsupported = saved;
                        return invoke(self, &lam);
                    }
                }
                Arg::Scalar(v)
            }
        }
    }

    /// `LAMBDA(param1, …, body)` — build a function value. The body stays
    /// unevaluated; the current `LET` bindings are captured lexically.
    fn lambda_fn(&mut self, args: &[Expr]) -> Arg {
        if args.is_empty() {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        let mut params = Vec::new();
        let mut seen_optional = false;
        for p in &args[..args.len() - 1] {
            match p {
                Expr::Name(n) => {
                    // Required parameters must precede optional ones.
                    if seen_optional {
                        return Arg::Scalar(Value::Err(ExcelError::Value));
                    }
                    params.push((n.clone(), false));
                }
                // `[y]` — Excel's optional-parameter syntax (lexes as a
                // bare structured reference).
                Expr::Structured {
                    table: None,
                    item: TableItem::Data,
                    col1: Some(n),
                    col2: None,
                } => {
                    seen_optional = true;
                    params.push((n.clone(), true));
                }
                _ => return Arg::Scalar(Value::Err(ExcelError::Value)),
            }
        }
        Arg::Lambda(Box::new(LambdaVal {
            params,
            body: args[args.len() - 1].clone(),
            captured: self.lets.clone(),
        }))
    }

    /// Call a lambda with pre-evaluated arguments. The body sees the
    /// lambda's captured environment plus its parameters — not the caller's
    /// bindings (lexical scoping). `omitted[i]` marks an argument written as
    /// an empty slot (`f(,2)`); optional parameters may be left off entirely.
    fn invoke_lambda(&mut self, lam: &LambdaVal, vals: Vec<Arg>, omitted: &[bool]) -> Arg {
        let required = lam.params.iter().filter(|(_, opt)| !opt).count();
        if vals.len() > lam.params.len() || vals.len() < required {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        if self.depth >= 32 {
            // Runaway recursion through a named lambda.
            return Arg::Scalar(Value::Err(ExcelError::Num));
        }
        let saved = std::mem::replace(&mut self.lets, lam.captured.clone());
        let saved_omitted = std::mem::take(&mut self.omitted);
        let mut vals = vals.into_iter();
        for (i, (p, _)) in lam.params.iter().enumerate() {
            match vals.next() {
                Some(v) if !omitted.get(i).copied().unwrap_or(false) => {
                    self.lets.push((p.clone(), v));
                }
                _ => {
                    // Omitted: binds as blank, visible to ISOMITTED.
                    self.lets.push((p.clone(), Arg::Scalar(Value::Empty)));
                    self.omitted.push(p.clone());
                }
            }
        }
        self.depth += 1;
        let out = self.eval_arg(&lam.body);
        self.depth -= 1;
        self.lets = saved;
        self.omitted = saved_omitted;
        out
    }

    /// A `LET`-bound lambda used as a function — an explicit local override that
    /// wins even over a builtin of the same name.
    fn let_lambda(&self, name: &str) -> Option<Box<LambdaVal>> {
        let i = self
            .lets
            .iter()
            .rposition(|(n, v)| n.eq_ignore_ascii_case(name) && matches!(v, Arg::Lambda(_)))?;
        match &self.lets[i].1 {
            Arg::Lambda(l) => Some(l.clone()),
            _ => None,
        }
    }

    /// A workbook defined name whose definition is `LAMBDA(…)` — Excel's named
    /// custom functions. Applied only when the name is *not* a builtin, so a
    /// stray defined name (e.g. `SUM`) can't shadow the real function.
    fn defined_lambda(&mut self, name: &str) -> Option<Box<LambdaVal>> {
        let def = self.res.defined_name(name, self.sheet)?;
        let ast = parse(&def).ok()?;
        if let Expr::Func(n, _) = &ast {
            if n == "LAMBDA" {
                if let Arg::Lambda(l) = self.eval_arg(&ast) {
                    return Some(l);
                }
            }
        }
        None
    }

    /// Collapse an argument to one value the way lambda-element results
    /// must: 1×1 shapes flatten; anything bigger is a nested array → #CALC!.
    fn collapse(&mut self, a: Arg) -> Value {
        match a {
            Arg::Scalar(v) => v,
            Arg::Range(s, r1, c1, r2, c2) => {
                if r1 == r2 && c1 == c2 {
                    self.res.value(s, r1, c1)
                } else {
                    Value::Err(ExcelError::Calc)
                }
            }
            Arg::Matrix(m) => {
                if m.len() == 1 && m[0].len() == 1 {
                    m[0][0].clone()
                } else {
                    Value::Err(ExcelError::Calc)
                }
            }
            Arg::Lambda(_) => Value::Err(ExcelError::Calc),
        }
    }

    /// The lambda higher-order functions: MAP, REDUCE, SCAN, BYROW, BYCOL,
    /// MAKEARRAY. The lambda always comes where Excel puts it.
    fn ho_fn(&mut self, name: &str, args: &[Expr]) -> Arg {
        let err = |e: ExcelError| Arg::Scalar(Value::Err(e));
        // Fetch the lambda argument at `idx`.
        macro_rules! lam_at {
            ($idx:expr) => {
                match self.eval_arg(&args[$idx]) {
                    Arg::Lambda(l) => l,
                    Arg::Scalar(v) if v.is_err() => return Arg::Scalar(v),
                    _ => return err(ExcelError::Value),
                }
            };
        }
        match name {
            "MAP" => {
                if args.len() < 2 {
                    return err(ExcelError::Value);
                }
                let lam = lam_at!(args.len() - 1);
                let mut mats: Vec<Matrix> = Vec::new();
                for a in &args[..args.len() - 1] {
                    match self.arg_matrix(a) {
                        Ok(m) => mats.push(m),
                        Err(v) => return Arg::Scalar(v),
                    }
                }
                if lam.params.len() != mats.len() {
                    return err(ExcelError::Value);
                }
                let rows = mats.iter().map(|m| m.len()).max().unwrap();
                let cols = mats.iter().map(|m| m[0].len()).max().unwrap();
                let mut out: Matrix = Vec::with_capacity(rows);
                for i in 0..rows {
                    let mut row = Vec::with_capacity(cols);
                    for j in 0..cols {
                        // Broadcast 1-sized axes; non-conforming → #N/A.
                        let mut vals = Vec::with_capacity(mats.len());
                        let mut oob = false;
                        for m in &mats {
                            let ri = if m.len() == 1 { 0 } else { i };
                            let ci = if m[0].len() == 1 { 0 } else { j };
                            match m.get(ri).and_then(|r| r.get(ci)) {
                                Some(v) => vals.push(Arg::Scalar(v.clone())),
                                None => {
                                    oob = true;
                                    break;
                                }
                            }
                        }
                        row.push(if oob {
                            Value::Err(ExcelError::NA)
                        } else {
                            let r = self.invoke_lambda(&lam, vals, &[]);
                            self.collapse(r)
                        });
                    }
                    out.push(row);
                }
                Arg::Matrix(out)
            }
            "REDUCE" | "SCAN" => {
                if args.len() != 3 {
                    return err(ExcelError::Value);
                }
                let init = self.eval(&args[0]);
                let m = match self.arg_matrix(&args[1]) {
                    Ok(m) => m,
                    Err(v) => return Arg::Scalar(v),
                };
                let lam = lam_at!(2);
                if lam.params.len() != 2 {
                    return err(ExcelError::Value);
                }
                let mut acc = init;
                let mut trace: Matrix = Vec::with_capacity(m.len());
                for row in &m {
                    let mut trow = Vec::with_capacity(row.len());
                    for v in row {
                        let r = self.invoke_lambda(
                            &lam,
                            vec![Arg::Scalar(acc.clone()), Arg::Scalar(v.clone())],
                            &[],
                        );
                        acc = self.collapse(r);
                        trow.push(acc.clone());
                    }
                    trace.push(trow);
                }
                if name == "REDUCE" {
                    Arg::Scalar(acc)
                } else {
                    Arg::Matrix(trace)
                }
            }
            "BYROW" | "BYCOL" => {
                if args.len() != 2 {
                    return err(ExcelError::Value);
                }
                let m = match self.arg_matrix(&args[0]) {
                    Ok(m) => m,
                    Err(v) => return Arg::Scalar(v),
                };
                let lam = lam_at!(1);
                if lam.params.len() != 1 {
                    return err(ExcelError::Value);
                }
                if name == "BYROW" {
                    let mut out: Matrix = Vec::with_capacity(m.len());
                    for row in &m {
                        let r = self.invoke_lambda(&lam, vec![Arg::Matrix(vec![row.clone()])], &[]);
                        out.push(vec![self.collapse(r)]);
                    }
                    Arg::Matrix(out)
                } else {
                    let cols = m[0].len();
                    let mut out_row = Vec::with_capacity(cols);
                    for j in 0..cols {
                        let col: Matrix = m.iter().map(|r| vec![r[j].clone()]).collect();
                        let r = self.invoke_lambda(&lam, vec![Arg::Matrix(col)], &[]);
                        out_row.push(self.collapse(r));
                    }
                    Arg::Matrix(vec![out_row])
                }
            }
            "MAKEARRAY" => {
                if args.len() != 3 {
                    return err(ExcelError::Value);
                }
                let rows = match to_num(&self.eval(&args[0])) {
                    Ok(n) => n.trunc() as i64,
                    Err(e) => return err(e),
                };
                let cols = match to_num(&self.eval(&args[1])) {
                    Ok(n) => n.trunc() as i64,
                    Err(e) => return err(e),
                };
                let lam = lam_at!(2);
                if rows < 1 || cols < 1 || lam.params.len() != 2 {
                    return err(ExcelError::Value);
                }
                if rows as u64 * cols as u64 > MAX_ARRAY_CELLS {
                    return err(ExcelError::Num);
                }
                let mut out: Matrix = Vec::with_capacity(rows as usize);
                for i in 0..rows {
                    let mut row = Vec::with_capacity(cols as usize);
                    for j in 0..cols {
                        let r = self.invoke_lambda(
                            &lam,
                            vec![
                                Arg::Scalar(Value::Num((i + 1) as f64)),
                                Arg::Scalar(Value::Num((j + 1) as f64)),
                            ],
                            &[],
                        );
                        row.push(self.collapse(r));
                    }
                    out.push(row);
                }
                Arg::Matrix(out)
            }
            _ => err(ExcelError::Name),
        }
    }

    /// Lift a scalar function elementwise over array arguments — the
    /// dynamic-array behavior of `ABS(A1:A3)` or `IF(A1:A3>1,"y","n")`.
    /// `IF` with an already-evaluated scalar selector: return the chosen branch
    /// as an `Arg`, preserving an array branch (so it can spill) rather than
    /// collapsing it to a scalar.
    fn if_scalar_branch(&mut self, sel: Arg, args: &[Expr]) -> Arg {
        if args.is_empty() || args.len() > 3 {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        let cond = match to_bool(&self.collapse(sel)) {
            Ok(b) => b,
            Err(e) => return Arg::Scalar(Value::Err(e)),
        };
        if cond {
            match args.get(1) {
                Some(Expr::Missing) | None => Arg::Scalar(Value::Num(0.0)),
                Some(e) => self.eval_arg(e),
            }
        } else {
            match args.get(2) {
                Some(Expr::Missing) | None => Arg::Scalar(if args.len() < 3 {
                    Value::Bool(false)
                } else {
                    Value::Num(0.0)
                }),
                Some(e) => self.eval_arg(e),
            }
        }
    }

    fn lift_call(&mut self, name: &str, args: &[Expr]) -> Arg {
        fn is_multi(a: &Arg) -> bool {
            match a {
                Arg::Range(_, r1, c1, r2, c2) => !(r1 == r2 && c1 == c2),
                Arg::Matrix(m) => !(m.len() == 1 && m[0].len() == 1),
                _ => false,
            }
        }
        // The branching functions must stay lazy when nothing lifts —
        // their unselected branch is never meant to be evaluated. Probe
        // only the selector; a scalar one takes the normal path.
        if matches!(name, "IF" | "IFERROR") {
            match args.first() {
                Some(a) => {
                    let sel = self.eval_arg(a);
                    if !is_multi(&sel) {
                        // A scalar selector picks one branch. For IF, return that
                        // branch as an Arg so an array branch (IF(TRUE, F5:F8, …))
                        // stays an array to spill, rather than collapsing.
                        if name == "IF" {
                            return self.if_scalar_branch(sel, args);
                        }
                        return Arg::Scalar(self.call(name, args));
                    }
                }
                None => return Arg::Scalar(self.call(name, args)),
            }
        }
        let vals: Vec<Arg> = args.iter().map(|a| self.eval_arg(a)).collect();
        if !vals.iter().any(is_multi) {
            // All scalar: one plain call over the already-evaluated values.
            let lit_args: Vec<Expr> = args
                .iter()
                .zip(vals)
                .map(|(a, v)| match a {
                    Expr::Missing => Expr::Missing,
                    _ => Expr::Lit(self.collapse(v)),
                })
                .collect();
            return Arg::Scalar(self.call(name, &lit_args));
        }
        // At least one array argument: broadcast and call per element.
        enum Item {
            S(Value),
            M(Matrix),
        }
        let mut items = Vec::with_capacity(vals.len());
        for v in vals {
            if is_multi(&v) {
                match self.materialize(v) {
                    Ok(m) => items.push(Item::M(m)),
                    Err(e) => return Arg::Scalar(Value::Err(e)),
                }
            } else {
                let sv = self.collapse(v);
                items.push(Item::S(sv));
            }
        }
        let rows = items
            .iter()
            .map(|it| match it {
                Item::M(m) => m.len(),
                Item::S(_) => 1,
            })
            .max()
            .unwrap_or(1);
        let cols = items
            .iter()
            .map(|it| match it {
                Item::M(m) => m[0].len(),
                Item::S(_) => 1,
            })
            .max()
            .unwrap_or(1);
        if rows as u64 * cols as u64 > MAX_ARRAY_CELLS {
            return Arg::Scalar(Value::Err(ExcelError::Num));
        }
        let mut out: Matrix = Vec::with_capacity(rows);
        for i in 0..rows {
            let mut row = Vec::with_capacity(cols);
            for j in 0..cols {
                let mut cell_args = Vec::with_capacity(items.len());
                let mut oob = false;
                for (orig, it) in args.iter().zip(&items) {
                    if matches!(orig, Expr::Missing) {
                        cell_args.push(Expr::Missing);
                        continue;
                    }
                    match it {
                        Item::S(v) => cell_args.push(Expr::Lit(v.clone())),
                        Item::M(m) => {
                            let ri = if m.len() == 1 { 0 } else { i };
                            let ci = if m[0].len() == 1 { 0 } else { j };
                            match m.get(ri).and_then(|r| r.get(ci)) {
                                Some(v) => cell_args.push(Expr::Lit(v.clone())),
                                None => {
                                    oob = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                row.push(if oob {
                    Value::Err(ExcelError::NA)
                } else {
                    self.call(name, &cell_args)
                });
            }
            out.push(row);
        }
        Arg::Matrix(out)
    }

    /// `LET(name1, value1, …, calculation)` — scoped bindings, evaluated
    /// left to right (later bindings may use earlier ones).
    fn let_fn(&mut self, args: &[Expr]) -> Arg {
        if args.len() < 3 || args.len().is_multiple_of(2) {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        let mark = self.lets.len();
        for pair in args[..args.len() - 1].chunks(2) {
            let Expr::Name(name) = &pair[0] else {
                self.lets.truncate(mark);
                return Arg::Scalar(Value::Err(ExcelError::Value));
            };
            let v = self.eval_arg(&pair[1]);
            self.lets.push((name.clone(), v));
        }
        let out = self.eval_arg(&args[args.len() - 1]);
        self.lets.truncate(mark);
        out
    }

    /// An argument materialized as a matrix (scalar → 1×1; errors propagate).
    fn arg_matrix(&mut self, e: &Expr) -> Result<Matrix, Value> {
        let a = self.eval_arg(e);
        self.materialize(a).map_err(Value::Err)
    }

    /// `MATCH`, lifted over its first argument only: with an array of lookup
    /// values it returns an array of positions against the (whole) lookup lane —
    /// the array-formula idiom `MATCH(range, range, 0)`. A scalar first argument
    /// takes the ordinary scalar path unchanged.
    fn match_call(&mut self, args: &[Expr]) -> Arg {
        if args.len() < 2 || args.len() > 3 {
            return Arg::Scalar(Value::Err(ExcelError::Value));
        }
        let first = self.eval_arg(&args[0]);
        let multi = match &first {
            Arg::Range(_, r1, c1, r2, c2) => !(r1 == r2 && c1 == c2),
            Arg::Matrix(m) => !(m.len() == 1 && m[0].len() == 1),
            _ => false,
        };
        if !multi {
            return Arg::Scalar(self.call("MATCH", args));
        }
        let lookup = match self.arg_matrix(&args[1]) {
            Ok(m) => m,
            Err(v) => return Arg::Scalar(v),
        };
        if lookup.len() != 1 && lookup[0].len() != 1 {
            return Arg::Scalar(Value::Err(ExcelError::NA));
        }
        let vals: Vec<Value> = lookup.into_iter().flatten().collect();
        let mode = match args.get(2) {
            None | Some(Expr::Missing) => 1.0,
            Some(e) => match to_num(&self.eval(e)) {
                Ok(n) => n,
                Err(er) => return Arg::Scalar(Value::Err(er)),
            },
        };
        let needles = match self.materialize(first) {
            Ok(m) => m,
            Err(e) => return Arg::Scalar(Value::Err(e)),
        };
        // Exact mode over many needles: a first-occurrence map makes it O(n+m).
        let index: Option<std::collections::HashMap<String, usize>> = (mode == 0.0).then(|| {
            let mut map = std::collections::HashMap::new();
            for (i, v) in vals.iter().enumerate() {
                if let Some(k) = match_key(v) {
                    map.entry(k).or_insert(i);
                }
            }
            map
        });
        let out: Matrix = needles
            .iter()
            .map(|row| {
                row.iter()
                    .map(|needle| {
                        if let Value::Err(e) = needle {
                            return Value::Err(*e);
                        }
                        if let Some(map) = &index {
                            if let Some(k) = match_key(needle) {
                                if let Some(&i) = map.get(&k) {
                                    return Value::Num(i as f64 + 1.0);
                                }
                            }
                            match_scan(&vals, needle, 0.0)
                        } else {
                            match_scan(&vals, needle, mode)
                        }
                    })
                    .collect()
            })
            .collect();
        Arg::Matrix(out)
    }

    /// Optional numeric argument with a default.
    fn opt_num(&mut self, args: &[Expr], i: usize, default: f64) -> Result<f64, Value> {
        match args.get(i) {
            None | Some(Expr::Missing) => Ok(default),
            Some(e) => to_num(&self.eval(e)).map_err(Value::Err),
        }
    }

    /// The dynamic-array function library. Everything here can return a
    /// matrix; scalar errors come back as `Arg::Scalar(Value::Err(…))`.
    fn array_fn(&mut self, name: &str, args: &[Expr]) -> Arg {
        match self.array_fn_inner(name, args) {
            Ok(m) => {
                if m.is_empty() || m[0].is_empty() {
                    Arg::Scalar(Value::Err(ExcelError::Calc))
                } else {
                    Arg::Matrix(m)
                }
            }
            Err(v) => Arg::Scalar(v),
        }
    }

    fn array_fn_inner(&mut self, name: &str, args: &[Expr]) -> Result<Matrix, Value> {
        let err = |e: ExcelError| -> Result<Matrix, Value> { Err(Value::Err(e)) };
        match name {
            // FREQUENCY(data, bins): a column of len(bins)+1 counts. count[0] =
            // #data ≤ bins[0]; count[i] = #data in (bins[i-1], bins[i]]; the last =
            // #data > bins[last]. Non-numbers in either array are ignored.
            "FREQUENCY" => {
                if args.len() != 2 {
                    return err(ExcelError::Value);
                }
                let dm = {
                    let a = self.eval_arg(&args[0]);
                    self.materialize(a).map_err(Value::Err)?
                };
                let bm = {
                    let a = self.eval_arg(&args[1]);
                    self.materialize(a).map_err(Value::Err)?
                };
                let nums = |m: &Matrix| -> Vec<f64> {
                    m.iter()
                        .flatten()
                        .filter_map(|v| match v {
                            Value::Num(n) => Some(*n),
                            _ => None,
                        })
                        .collect()
                };
                let (data, bins) = (nums(&dm), nums(&bm));
                let mut counts = vec![0.0f64; bins.len() + 1];
                for x in data {
                    // First bin ≥ x (bins are used in the order given); anything
                    // past the last bin falls in the overflow bucket.
                    let idx = bins.iter().position(|&b| x <= b).unwrap_or(bins.len());
                    counts[idx] += 1.0;
                }
                Ok(counts.into_iter().map(|c| vec![num(c)]).collect())
            }
            "SEQUENCE" => {
                if args.is_empty() || args.len() > 4 {
                    return err(ExcelError::Value);
                }
                let rows = to_num(&self.eval(&args[0])).map_err(Value::Err)?.trunc() as i64;
                let cols = self.opt_num(args, 1, 1.0)?.trunc() as i64;
                let start = self.opt_num(args, 2, 1.0)?;
                let step = self.opt_num(args, 3, 1.0)?;
                if rows < 1 || cols < 1 {
                    return err(ExcelError::Value);
                }
                if rows as u64 * cols as u64 > MAX_ARRAY_CELLS {
                    return err(ExcelError::Num);
                }
                let mut v = start;
                Ok((0..rows)
                    .map(|_| {
                        (0..cols)
                            .map(|_| {
                                let out = v;
                                v += step;
                                num(out)
                            })
                            .collect()
                    })
                    .collect())
            }
            "RANDARRAY" => {
                if args.len() > 5 {
                    return err(ExcelError::Value);
                }
                let rows = self.opt_num(args, 0, 1.0)?.trunc() as i64;
                let cols = self.opt_num(args, 1, 1.0)?.trunc() as i64;
                let lo = self.opt_num(args, 2, 0.0)?;
                let hi = self.opt_num(args, 3, 1.0)?;
                let whole = match args.get(4) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => to_bool(&self.eval(e)).map_err(Value::Err)?,
                };
                if rows < 1 || cols < 1 || lo > hi {
                    return err(ExcelError::Value);
                }
                if rows as u64 * cols as u64 > MAX_ARRAY_CELLS {
                    return err(ExcelError::Num);
                }
                if whole && (lo.fract() != 0.0 || hi.fract() != 0.0) {
                    return err(ExcelError::Value);
                }
                let mut cells = Vec::new();
                for _ in 0..rows * cols {
                    match self.res.rand() {
                        Some(r) => cells.push(if whole {
                            num(lo + (r * (hi - lo + 1.0)).floor())
                        } else {
                            num(lo + r * (hi - lo))
                        }),
                        None => {
                            self.unsupported = true;
                            return err(ExcelError::Value);
                        }
                    }
                }
                Ok(cells
                    .chunks(cols as usize)
                    .map(|row| row.to_vec())
                    .collect())
            }
            "TRANSPOSE" => {
                if args.len() != 1 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                Ok((0..m[0].len())
                    .map(|j| (0..m.len()).map(|i| m[i][j].clone()).collect())
                    .collect())
            }
            "SORT" => {
                if args.is_empty() || args.len() > 4 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let idx = self.opt_num(args, 1, 1.0)?.trunc() as i64;
                let order = self.opt_num(args, 2, 1.0)?.trunc() as i64;
                let by_col = match args.get(3) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => to_bool(&self.eval(e)).map_err(Value::Err)?,
                };
                if order != 1 && order != -1 {
                    return err(ExcelError::Value);
                }
                let m = if by_col { transpose(&m) } else { m };
                if idx < 1 || idx as usize > m[0].len() {
                    return err(ExcelError::Value);
                }
                let key = idx as usize - 1;
                let mut rows = m;
                rows.sort_by(|a, b| {
                    let ord = compare(&a[key], &b[key]).unwrap_or(std::cmp::Ordering::Equal);
                    if order == 1 { ord } else { ord.reverse() }
                });
                Ok(if by_col { transpose(&rows) } else { rows })
            }
            "SORTBY" => {
                if args.len() < 2 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                // (key vector, order) pairs; a trailing pair may omit order.
                let mut keys: Vec<(Vec<Value>, i64)> = Vec::new();
                let mut i = 1;
                while i < args.len() {
                    let by = self.arg_matrix(&args[i])?;
                    let vec = flatten_vector(&by).ok_or(Value::Err(ExcelError::Value))?;
                    if vec.len() != m.len() {
                        return err(ExcelError::Value);
                    }
                    let order = if i + 1 < args.len() {
                        self.opt_num(args, i + 1, 1.0)?.trunc() as i64
                    } else {
                        1
                    };
                    if order != 1 && order != -1 {
                        return err(ExcelError::Value);
                    }
                    keys.push((vec, order));
                    i += 2;
                }
                let mut order_idx: Vec<usize> = (0..m.len()).collect();
                order_idx.sort_by(|&a, &b| {
                    for (vec, ord) in &keys {
                        let o = compare(&vec[a], &vec[b]).unwrap_or(std::cmp::Ordering::Equal);
                        let o = if *ord == 1 { o } else { o.reverse() };
                        if o != std::cmp::Ordering::Equal {
                            return o;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                Ok(order_idx.into_iter().map(|i| m[i].clone()).collect())
            }
            "UNIQUE" => {
                if args.is_empty() || args.len() > 3 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let by_col = match args.get(1) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => to_bool(&self.eval(e)).map_err(Value::Err)?,
                };
                let once = match args.get(2) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => to_bool(&self.eval(e)).map_err(Value::Err)?,
                };
                let m = if by_col { transpose(&m) } else { m };
                let eq = |a: &[Value], b: &[Value]| {
                    a.len() == b.len()
                        && a.iter().zip(b).all(|(x, y)| {
                            matches!(compare(x, y), Ok(std::cmp::Ordering::Equal))
                                && std::mem::discriminant(x) == std::mem::discriminant(y)
                        })
                };
                let mut out: Vec<Vec<Value>> = Vec::new();
                let mut counts: Vec<usize> = Vec::new();
                for row in &m {
                    match out.iter().position(|o| eq(o, row)) {
                        Some(i) => counts[i] += 1,
                        None => {
                            out.push(row.clone());
                            counts.push(1);
                        }
                    }
                }
                if once {
                    out = out
                        .into_iter()
                        .zip(&counts)
                        .filter(|&(_, &c)| c == 1)
                        .map(|(r, _)| r)
                        .collect();
                }
                Ok(if by_col { transpose(&out) } else { out })
            }
            "FILTER" => {
                if args.len() < 2 || args.len() > 3 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let inc = self.arg_matrix(&args[1])?;
                let filtered: Matrix = if inc[0].len() == 1 && inc.len() == m.len() {
                    // R×1 include → keep matching rows.
                    let mut keep = Vec::new();
                    for (i, row) in inc.iter().enumerate() {
                        if to_bool(&row[0]).map_err(Value::Err)? {
                            keep.push(m[i].clone());
                        }
                    }
                    keep
                } else if inc.len() == 1 && inc[0].len() == m[0].len() {
                    // 1×C include → keep matching columns.
                    let cols: Vec<usize> = {
                        let mut v = Vec::new();
                        for (j, val) in inc[0].iter().enumerate() {
                            if to_bool(val).map_err(Value::Err)? {
                                v.push(j);
                            }
                        }
                        v
                    };
                    m.iter()
                        .map(|row| cols.iter().map(|&j| row[j].clone()).collect())
                        .collect()
                } else {
                    return err(ExcelError::Value);
                };
                if filtered.is_empty() || filtered.first().is_some_and(|r| r.is_empty()) {
                    return match args.get(2) {
                        None | Some(Expr::Missing) => err(ExcelError::Calc),
                        Some(e) => self.arg_matrix(e),
                    };
                }
                Ok(filtered)
            }
            "CHOOSEROWS" | "CHOOSECOLS" => {
                if args.len() < 2 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let m = if name == "CHOOSECOLS" {
                    transpose(&m)
                } else {
                    m
                };
                let n = m.len() as i64;
                let mut picks = Vec::new();
                for e in &args[1..] {
                    let km = self.arg_matrix(e)?;
                    for v in km.iter().flatten() {
                        let k = to_num(v).map_err(Value::Err)?.trunc() as i64;
                        let i = if k > 0 && k <= n {
                            k - 1
                        } else if k < 0 && -k <= n {
                            n + k
                        } else {
                            return err(ExcelError::Value);
                        };
                        picks.push(m[i as usize].clone());
                    }
                }
                Ok(if name == "CHOOSECOLS" {
                    transpose(&picks)
                } else {
                    picks
                })
            }
            "TAKE" | "DROP" => {
                if args.len() < 2 || args.len() > 3 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let (nr, nc) = (m.len() as i64, m[0].len() as i64);
                let rows = match args.get(1) {
                    Some(Expr::Missing) => {
                        if name == "TAKE" {
                            nr
                        } else {
                            0
                        }
                    }
                    _ => to_num(&self.eval(&args[1])).map_err(Value::Err)?.trunc() as i64,
                };
                let cols = match args.get(2) {
                    None | Some(Expr::Missing) => {
                        if name == "TAKE" {
                            nc
                        } else {
                            0
                        }
                    }
                    Some(e) => to_num(&self.eval(e)).map_err(Value::Err)?.trunc() as i64,
                };
                let span = |take: bool, k: i64, n: i64| -> Option<(i64, i64)> {
                    // Half-open [lo, hi) of surviving indices along one axis.
                    if take {
                        if k == 0 {
                            return None;
                        }
                        let k = k.clamp(-n, n);
                        Some(if k > 0 { (0, k) } else { (n + k, n) })
                    } else {
                        let k = k.clamp(-n, n);
                        let (lo, hi) = if k >= 0 { (k, n) } else { (0, n + k) };
                        if lo >= hi { None } else { Some((lo, hi)) }
                    }
                };
                let take = name == "TAKE";
                let (r_lo, r_hi) = span(take, rows, nr).ok_or(Value::Err(ExcelError::Calc))?;
                let (c_lo, c_hi) = span(take, cols, nc).ok_or(Value::Err(ExcelError::Calc))?;
                Ok(m[r_lo as usize..r_hi as usize]
                    .iter()
                    .map(|row| row[c_lo as usize..c_hi as usize].to_vec())
                    .collect())
            }
            "HSTACK" | "VSTACK" => {
                if args.is_empty() {
                    return err(ExcelError::Value);
                }
                let mut parts = Vec::new();
                for e in args {
                    parts.push(self.arg_matrix(e)?);
                }
                if name == "VSTACK" {
                    let cols = parts.iter().map(|p| p[0].len()).max().unwrap();
                    let mut out = Vec::new();
                    for p in parts {
                        for row in p {
                            let mut r = row;
                            r.resize(cols, Value::Err(ExcelError::NA));
                            out.push(r);
                        }
                    }
                    Ok(out)
                } else {
                    let rows = parts.iter().map(|p| p.len()).max().unwrap();
                    let mut out: Matrix = vec![Vec::new(); rows];
                    for p in parts {
                        let w = p[0].len();
                        for (i, out_row) in out.iter_mut().enumerate() {
                            match p.get(i) {
                                Some(row) => out_row.extend(row.iter().cloned()),
                                None => {
                                    out_row.extend((0..w).map(|_| Value::Err(ExcelError::NA)));
                                }
                            }
                        }
                    }
                    Ok(out)
                }
            }
            "TOCOL" | "TOROW" => {
                if args.is_empty() || args.len() > 3 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let ignore = self.opt_num(args, 1, 0.0)?.trunc() as i64;
                let by_col = match args.get(2) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => to_bool(&self.eval(e)).map_err(Value::Err)?,
                };
                let m = if by_col { transpose(&m) } else { m };
                let vals: Vec<Value> = m
                    .into_iter()
                    .flatten()
                    .filter(|v| {
                        !((ignore & 1 != 0 && matches!(v, Value::Empty))
                            || (ignore & 2 != 0 && v.is_err()))
                    })
                    .collect();
                if vals.is_empty() {
                    return err(ExcelError::Calc);
                }
                Ok(if name == "TOCOL" {
                    vals.into_iter().map(|v| vec![v]).collect()
                } else {
                    vec![vals]
                })
            }
            "EXPAND" => {
                if args.len() < 2 || args.len() > 4 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let rows = to_num(&self.eval(&args[1])).map_err(Value::Err)?.trunc() as i64;
                let cols = self.opt_num(args, 2, m[0].len() as f64)?.trunc() as i64;
                let pad = match args.get(3) {
                    None | Some(Expr::Missing) => Value::Err(ExcelError::NA),
                    Some(e) => self.eval(e),
                };
                if rows < m.len() as i64 || cols < m[0].len() as i64 {
                    return err(ExcelError::Value);
                }
                if rows as u64 * cols as u64 > MAX_ARRAY_CELLS {
                    return err(ExcelError::Num);
                }
                Ok((0..rows as usize)
                    .map(|i| {
                        (0..cols as usize)
                            .map(|j| {
                                m.get(i)
                                    .and_then(|row| row.get(j))
                                    .cloned()
                                    .unwrap_or_else(|| pad.clone())
                            })
                            .collect()
                    })
                    .collect())
            }
            "WRAPROWS" | "WRAPCOLS" => {
                if args.len() < 2 || args.len() > 3 {
                    return err(ExcelError::Value);
                }
                let m = self.arg_matrix(&args[0])?;
                let vec = flatten_vector(&m).ok_or(Value::Err(ExcelError::Value))?;
                let count = to_num(&self.eval(&args[1])).map_err(Value::Err)?.trunc() as i64;
                let pad = match args.get(2) {
                    None | Some(Expr::Missing) => Value::Err(ExcelError::NA),
                    Some(e) => self.eval(e),
                };
                if count < 1 {
                    return err(ExcelError::Num);
                }
                let mut rows: Matrix = vec
                    .chunks(count as usize)
                    .map(|chunk| {
                        let mut r = chunk.to_vec();
                        r.resize(count as usize, pad.clone());
                        r
                    })
                    .collect();
                if name == "WRAPCOLS" {
                    rows = transpose(&rows);
                }
                Ok(rows)
            }
            "MUNIT" => {
                if args.len() != 1 {
                    return err(ExcelError::Value);
                }
                let n = to_num(&self.eval(&args[0])).map_err(Value::Err)?.trunc() as i64;
                if n < 1 {
                    return err(ExcelError::Value);
                }
                if (n * n) as u64 > MAX_ARRAY_CELLS {
                    return err(ExcelError::Num);
                }
                Ok((0..n)
                    .map(|i| {
                        (0..n)
                            .map(|j| num(if i == j { 1.0 } else { 0.0 }))
                            .collect()
                    })
                    .collect())
            }
            "MINVERSE" => {
                if args.len() != 1 {
                    return err(ExcelError::Value);
                }
                let m = to_num_matrix(&self.arg_matrix(&args[0])?)?;
                match matrix_inverse(&m) {
                    Some(inv) => Ok(inv
                        .into_iter()
                        .map(|row| row.into_iter().map(num).collect())
                        .collect()),
                    None => err(ExcelError::Num),
                }
            }
            "MMULT" => {
                if args.len() != 2 {
                    return err(ExcelError::Value);
                }
                let a = to_num_matrix(&self.arg_matrix(&args[0])?)?;
                let b = to_num_matrix(&self.arg_matrix(&args[1])?)?;
                let (ar, ac) = (a.len(), a[0].len());
                let (br, bc) = (b.len(), b[0].len());
                if ac != br || a.iter().any(|r| r.len() != ac) || b.iter().any(|r| r.len() != bc) {
                    return err(ExcelError::Value);
                }
                if (ar * bc) as u64 > MAX_ARRAY_CELLS {
                    return err(ExcelError::Num);
                }
                Ok((0..ar)
                    .map(|i| {
                        (0..bc)
                            .map(|j| num((0..ac).map(|k| a[i][k] * b[k][j]).sum()))
                            .collect()
                    })
                    .collect())
            }
            "TEXTSPLIT" => {
                // TEXTSPLIT(text, col_delim, [row_delim], [ignore_empty],
                //           [match_mode], [pad_with]).
                if args.len() < 2 || args.len() > 6 {
                    return err(ExcelError::Value);
                }
                let text = to_text(&self.eval(&args[0])).map_err(Value::Err)?;
                let col_delims = self.delim_list(args.get(1))?;
                let row_delims = self.delim_list(args.get(2))?;
                let ignore_empty = match args.get(3) {
                    None | Some(Expr::Missing) => row_delims.is_empty(),
                    Some(e) => to_bool(&self.eval(e)).map_err(Value::Err)?,
                };
                let case_insensitive = match args.get(4) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => to_num(&self.eval(e)).map_err(Value::Err)?.trunc() == 1.0,
                };
                let pad = match args.get(5) {
                    None | Some(Expr::Missing) => Value::Err(ExcelError::NA),
                    Some(e) => self.eval(e),
                };
                if col_delims.is_empty() && row_delims.is_empty() {
                    return err(ExcelError::Value);
                }
                let split = |s: &str, delims: &[String]| -> Vec<String> {
                    if delims.is_empty() {
                        return vec![s.to_string()];
                    }
                    split_on_any(s, delims, case_insensitive)
                };
                let row_strs = split(&text, &row_delims);
                let mut grid: Vec<Vec<String>> =
                    row_strs.iter().map(|r| split(r, &col_delims)).collect();
                if ignore_empty {
                    for r in &mut grid {
                        r.retain(|c| !c.is_empty());
                    }
                    grid.retain(|r| !r.is_empty());
                }
                if grid.is_empty() {
                    return err(ExcelError::Value);
                }
                // Rectangularize with the pad value.
                let width = grid.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
                let out: Matrix = grid
                    .iter()
                    .map(|r| {
                        (0..width)
                            .map(|i| match r.get(i) {
                                Some(s) => Value::Str(s.clone()),
                                None => pad.clone(),
                            })
                            .collect()
                    })
                    .collect();
                Ok(out)
            }
            _ => err(ExcelError::Name),
        }
    }

    /// A TEXTSPLIT delimiter argument: a scalar or an array of strings, each
    /// non-empty. `None`/`Missing` → no delimiters.
    fn delim_list(&mut self, e: Option<&Expr>) -> Result<Vec<String>, Value> {
        let e = match e {
            None | Some(Expr::Missing) => return Ok(Vec::new()),
            Some(e) => e,
        };
        let mut out = Vec::new();
        for v in self.flat_dense(e)? {
            let s = to_text(&v).map_err(Value::Err)?;
            if !s.is_empty() {
                out.push(s);
            }
        }
        Ok(out)
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
                // OFFSET needs an actual reference; computed arrays and
                // scalars don't qualify.
                Arg::Scalar(v) => {
                    return Arg::Scalar(if v.is_err() {
                        v
                    } else {
                        Value::Err(ExcelError::Value)
                    });
                }
                Arg::Matrix(_) => return Arg::Scalar(Value::Err(ExcelError::Value)),
                Arg::Lambda(_) => return Arg::Scalar(Value::Err(ExcelError::Calc)),
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

/// Scan a 1-D lookup lane for `MATCH`: `mode` 0 = exact (with wildcards), 1 =
/// largest ≤ needle (assumes ascending), −1 = smallest ≥ needle (descending).
/// Returns the 1-based position or `#N/A`.
fn match_scan(vals: &[Value], needle: &Value, mode: f64) -> Value {
    use std::cmp::Ordering;
    let mut best: Option<usize> = None;
    for (i, v) in vals.iter().enumerate() {
        if matches!(v, Value::Empty) {
            continue;
        }
        match compare(v, needle) {
            Ok(Ordering::Equal) => {
                best = Some(i);
                if mode == 0.0 {
                    break;
                }
            }
            Ok(Ordering::Less) if mode > 0.0 => best = Some(i),
            Ok(Ordering::Greater) if mode < 0.0 => best = Some(i),
            _ => {}
        }
    }
    if best.is_none() && mode == 0.0 {
        if let Value::Str(pat) = needle {
            if pat.contains(['*', '?']) {
                for (i, v) in vals.iter().enumerate() {
                    if let Value::Str(t) = v {
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

/// A hashable key mirroring `compare`'s equality (numbers exact, text
/// case-insensitive), for building a first-occurrence index in exact `MATCH`.
fn match_key(v: &Value) -> Option<String> {
    match v {
        Value::Num(n) => Some(format!("n{}", n.to_bits())),
        Value::Str(s) => Some(format!("s{}", s.to_lowercase())),
        Value::Bool(b) => Some(format!("b{}", *b as u8)),
        _ => None,
    }
}

/// Turn a stored formula into the text Excel *shows*: strip the future-function
/// (`_xlfn.`), worksheet (`_xlws.`) and lambda-parameter (`_xlpm.`) prefixes, and
/// rewrite the internal spill/implicit operators — `ANCHORARRAY(A1)` → `A1#`,
/// `SINGLE(x)` → `@x`. Used by `FORMULATEXT`.
pub fn display_formula(src: &str) -> String {
    let s = src
        .replace("_xlfn._xlws.", "")
        .replace("_xlfn.", "")
        .replace("_xlws.", "")
        .replace("_xlpm.", "");
    let s = rewrite_call(&s, "ANCHORARRAY", |arg| format!("{arg}#"));
    rewrite_call(&s, "SINGLE", |arg| format!("@{arg}"))
}

/// Replace every `name(arg)` (balanced parens) with `f(arg)`.
fn rewrite_call(s: &str, name: &str, f: impl Fn(&str) -> String) -> String {
    let pat = format!("{name}(");
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(&pat) {
        out.push_str(&rest[..i]);
        let after = &rest[i + pat.len()..];
        let mut depth = 1usize;
        let mut end = None;
        for (j, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(e) => {
                out.push_str(&f(&after[..e]));
                rest = &after[e + 1..];
            }
            None => {
                // Unbalanced — leave the rest verbatim.
                out.push_str(&rest[i..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Wrap a distribution result: a finite value → number, anything else → `#NUM!`.
fn domo(x: Option<f64>) -> Value {
    match x {
        Some(v) if v.is_finite() => num(v),
        _ => Value::Err(ExcelError::Num),
    }
}

/// Whether `name` is one of the statistical-distribution functions.
fn is_stat_fn(name: &str) -> bool {
    matches!(
        name,
        "NORM.DIST"
            | "NORMDIST"
            | "NORM.S.DIST"
            | "NORMSDIST"
            | "NORM.INV"
            | "NORMINV"
            | "NORM.S.INV"
            | "NORMSINV"
            | "PHI"
            | "GAUSS"
            | "STANDARDIZE"
            | "FISHER"
            | "FISHERINV"
            | "CONFIDENCE"
            | "CONFIDENCE.NORM"
            | "CONFIDENCE.T"
            | "GAMMALN"
            | "GAMMALN.PRECISE"
            | "GAMMA"
            | "GAMMA.DIST"
            | "GAMMADIST"
            | "GAMMA.INV"
            | "GAMMAINV"
            | "CHISQ.DIST"
            | "CHISQ.DIST.RT"
            | "CHIDIST"
            | "CHISQ.INV"
            | "CHISQ.INV.RT"
            | "CHIINV"
            | "EXPON.DIST"
            | "EXPONDIST"
            | "POISSON.DIST"
            | "POISSON"
            | "WEIBULL.DIST"
            | "WEIBULL"
            | "LOGNORM.DIST"
            | "LOGNORMDIST"
            | "LOGNORM.INV"
            | "LOGINV"
            | "BINOM.DIST"
            | "BINOMDIST"
            | "BINOM.INV"
            | "CRITBINOM"
            | "NEGBINOM.DIST"
            | "NEGBINOMDIST"
            | "HYPGEOM.DIST"
            | "HYPGEOMDIST"
            | "BETA.DIST"
            | "BETADIST"
            | "BETA.INV"
            | "BETAINV"
            | "F.DIST"
            | "F.DIST.RT"
            | "FDIST"
            | "F.INV"
            | "F.INV.RT"
            | "FINV"
            | "T.DIST"
            | "T.DIST.RT"
            | "T.DIST.2T"
            | "TDIST"
            | "T.INV"
            | "T.INV.2T"
            | "TINV"
    )
}

/// Log of the binomial coefficient C(n, k) (−∞ outside 0 ≤ k ≤ n → probability 0).
fn lcomb(n: f64, k: f64) -> f64 {
    if k < 0.0 || k > n {
        return f64::NEG_INFINITY;
    }
    crate::stats::lgamma(n + 1.0) - crate::stats::lgamma(k + 1.0) - crate::stats::lgamma(n - k + 1.0)
}

/// Lognormal PDF/CDF.
fn lognorm(x: f64, mean: f64, sd: f64, cumulative: bool) -> Option<f64> {
    if x <= 0.0 || sd <= 0.0 {
        return None;
    }
    let z = (x.ln() - mean) / sd;
    Some(if cumulative {
        crate::stats::norm_cdf(z)
    } else {
        crate::stats::norm_pdf(z) / (x * sd)
    })
}

/// Binomial PMF/CDF at `k` successes in `n_` trials with probability `p`.
fn binom(k: f64, n_: f64, p: f64, cumulative: bool) -> Option<f64> {
    let (k, n_) = (k.floor(), n_.floor());
    if k < 0.0 || k > n_ || !(0.0..=1.0).contains(&p) {
        return None;
    }
    if cumulative {
        if k >= n_ {
            return Some(1.0);
        }
        return Some(crate::stats::betai(n_ - k, k + 1.0, 1.0 - p));
    }
    if p == 0.0 {
        return Some(if k == 0.0 { 1.0 } else { 0.0 });
    }
    if p == 1.0 {
        return Some(if k == n_ { 1.0 } else { 0.0 });
    }
    Some((lcomb(n_, k) + k * p.ln() + (n_ - k) * (1.0 - p).ln()).exp())
}

/// Hypergeometric PMF/CDF: `x` successes in a sample of `n`, drawn from a
/// population of `pop` with `succ` successes.
fn hypgeom(x: f64, n: f64, succ: f64, pop: f64, cumulative: bool) -> Option<f64> {
    let (x, n, succ, pop) = (x.floor(), n.floor(), succ.floor(), pop.floor());
    if x < 0.0 || n < 0.0 || succ < 0.0 || pop < 0.0 || n > pop || succ > pop || x > n || x > succ {
        return None;
    }
    let pmf = |i: f64| (lcomb(succ, i) + lcomb(pop - succ, n - i) - lcomb(pop, n)).exp();
    if cumulative {
        let lo = (n - (pop - succ)).max(0.0);
        let mut s = 0.0;
        let mut i = lo;
        while i <= x {
            s += pmf(i);
            i += 1.0;
        }
        Some(s)
    } else if n - x > pop - succ {
        Some(0.0)
    } else {
        Some(pmf(x))
    }
}

/// F distribution PDF/CDF.
fn f_dist(x: f64, d1: f64, d2: f64, cumulative: bool) -> Option<f64> {
    if x < 0.0 || d1 < 1.0 || d2 < 1.0 {
        return None;
    }
    if cumulative {
        return Some(crate::stats::f_cdf(x, d1, d2));
    }
    if x == 0.0 {
        return Some(if d1 < 2.0 {
            f64::INFINITY
        } else if d1 == 2.0 {
            1.0
        } else {
            0.0
        });
    }
    let lb =
        crate::stats::lgamma(d1 / 2.0) + crate::stats::lgamma(d2 / 2.0) - crate::stats::lgamma((d1 + d2) / 2.0);
    let logpdf = 0.5 * (d1 * (d1 * x).ln() + d2 * d2.ln() - (d1 + d2) * (d1 * x + d2).ln())
        - x.ln()
        - lb;
    Some(logpdf.exp())
}

/// Student-t distribution PDF/CDF.
fn t_dist(x: f64, df: f64, cumulative: bool) -> Option<f64> {
    if df < 1.0 {
        return None;
    }
    if cumulative {
        return Some(crate::stats::t_cdf(x, df));
    }
    let pi = std::f64::consts::PI;
    let logpdf = crate::stats::lgamma((df + 1.0) / 2.0)
        - crate::stats::lgamma(df / 2.0)
        - 0.5 * (df * pi).ln()
        - ((df + 1.0) / 2.0) * (1.0 + x * x / df).ln();
    Some(logpdf.exp())
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
pub(crate) fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering, ExcelError> {
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

/// A lookup table for VLOOKUP/MATCH/INDEX and friends: an on-sheet rect or
/// a computed matrix (e.g. `INDEX(UNIQUE(A1:A9),2)`).
enum TView {
    Range(usize, u32, u32, u32, u32),
    Mat(Matrix),
}

trait NumOrZero {
    fn num_or_zero(self, nums: &[f64]) -> Value;
}
impl NumOrZero for Value {
    /// MAXX/MINX over rows yielding no numbers give 0 (like pivot Max/Min).
    fn num_or_zero(self, nums: &[f64]) -> Value {
        if nums.is_empty() {
            Value::Num(0.0)
        } else {
            self
        }
    }
}

/// Collect the table names iterated by SUMX-family calls (their rows are
/// real dependencies even though the name isn't a reference).
pub fn collect_iterated_tables(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Func(name, args) => {
            if matches!(
                name.as_str(),
                "SUMX" | "AVERAGEX" | "MAXX" | "MINX" | "COUNTX" | "COUNTAX"
            ) {
                match args.first() {
                    Some(Expr::Name(n)) => out.push(n.clone()),
                    Some(Expr::Structured { table: Some(t), .. }) => out.push(t.clone()),
                    _ => {}
                }
            }
            for a in args {
                collect_iterated_tables(a, out);
            }
        }
        Expr::Call(callee, args) => {
            collect_iterated_tables(callee, out);
            for a in args {
                collect_iterated_tables(a, out);
            }
        }
        Expr::ArrayLit(rows) => {
            for row in rows {
                for x in row {
                    collect_iterated_tables(x, out);
                }
            }
        }
        Expr::Un(_, x) => collect_iterated_tables(x, out),
        Expr::Bin(_, l, r) => {
            collect_iterated_tables(l, out);
            collect_iterated_tables(r, out);
        }
        _ => {}
    }
}

/// Is this one of the dynamic-array functions resolved in `eval_arg` (they
/// can return matrices)?
fn is_array_fn(name: &str) -> bool {
    matches!(
        name,
        "SEQUENCE"
            | "RANDARRAY"
            | "FREQUENCY"
            | "TRANSPOSE"
            | "SORT"
            | "SORTBY"
            | "UNIQUE"
            | "FILTER"
            | "CHOOSEROWS"
            | "CHOOSECOLS"
            | "TAKE"
            | "DROP"
            | "HSTACK"
            | "VSTACK"
            | "TOCOL"
            | "TOROW"
            | "EXPAND"
            | "WRAPROWS"
            | "WRAPCOLS"
            | "TEXTSPLIT"
            | "MMULT"
            | "MINVERSE"
            | "MUNIT"
    )
}

/// The lambda-consuming higher-order functions (resolved in `eval_arg`).
fn is_higher_order_fn(name: &str) -> bool {
    matches!(
        name,
        "MAP" | "REDUCE" | "SCAN" | "BYROW" | "BYCOL" | "MAKEARRAY"
    )
}

/// Scalar functions lifted elementwise over array arguments. Only pure
/// scalar-in/scalar-out functions belong here (IF/IFERROR get special lazy
/// handling for scalar selectors).
fn is_liftable_fn(name: &str) -> bool {
    matches!(
        name,
        "ABS"
            | "SIGN"
            | "INT"
            | "TRUNC"
            | "SQRT"
            | "EXP"
            | "LN"
            | "LOG"
            | "LOG10"
            | "SIN"
            | "COS"
            | "TAN"
            | "ASIN"
            | "ACOS"
            | "ATAN"
            | "DEGREES"
            | "RADIANS"
            | "ROUND"
            | "ROUNDUP"
            | "ROUNDDOWN"
            | "MOD"
            | "POWER"
            | "LEN"
            | "UPPER"
            | "LOWER"
            | "PROPER"
            | "TRIM"
            | "VALUE"
            | "TEXT"
            | "LEFT"
            | "RIGHT"
            | "MID"
            | "SUBSTITUTE"
            | "NOT"
            | "ISBLANK"
            | "ISNUMBER"
            | "ISTEXT"
            | "ISERROR"
            | "IF"
            | "IFERROR"
            | "YEAR"
            | "MONTH"
            | "DAY"
            | "HOUR"
            | "MINUTE"
            | "SECOND"
            | "WEEKDAY"
            | "DATE"
    )
}

fn transpose(m: &Matrix) -> Matrix {
    if m.is_empty() || m[0].is_empty() {
        return Vec::new();
    }
    (0..m[0].len())
        .map(|j| (0..m.len()).map(|i| m[i][j].clone()).collect())
        .collect()
}

/// A single row or column matrix as a flat vector; `None` for 2D shapes.
fn flatten_vector(m: &Matrix) -> Option<Vec<Value>> {
    if m.len() == 1 {
        Some(m[0].clone())
    } else if m.iter().all(|r| r.len() == 1) {
        Some(m.iter().map(|r| r[0].clone()).collect())
    } else {
        None
    }
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
///
/// Iterative two-pointer matcher with a single backtrack point for `*`, so
/// it runs in O(pattern × text) rather than the exponential time a naive
/// recursive `*`-backtracker would take on adversarial patterns like
/// `a*a*a*…z` (both pattern and text come from untrusted workbooks).
fn wildcard_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();

    let (mut pi, mut ti) = (0usize, 0usize);
    // Where to resume if a tentative `*` match fails: the `*`'s pattern index
    // and the text position it was last tried against.
    let (mut star, mut star_ti): (Option<usize>, usize) = (None, 0);

    while ti < t.len() {
        // A literal or `?` (or `~`-escaped literal) that matches consumes one
        // char from each side.
        let matched = match p.get(pi) {
            Some('*') => {
                star = Some(pi);
                star_ti = ti;
                pi += 1;
                continue;
            }
            Some('~') if pi + 1 < p.len() => {
                if p[pi + 1] == t[ti] {
                    pi += 2;
                    ti += 1;
                    continue;
                }
                false
            }
            Some('?') => {
                pi += 1;
                ti += 1;
                continue;
            }
            Some(&c) => c == t[ti],
            None => false,
        };
        if matched {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            // Backtrack: let the last `*` swallow one more text char.
            pi = s + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    // Trailing pattern must be all `*` to match the exhausted text.
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
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
            if matches!(a, Expr::Ref3D { .. }) {
                for (s, r1, c1, r2, c2) in self.resolve_3d(a)? {
                    for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                        match v {
                            Value::Num(n) => out.push(n),
                            Value::Err(e) => return Err(e),
                            Value::Bool(b) if !numbers_only => out.push(if b { 1.0 } else { 0.0 }),
                            _ => {}
                        }
                    }
                }
                continue;
            }
            match self.eval_arg(a) {
                Arg::Scalar(Value::Err(e)) => return Err(e),
                Arg::Scalar(Value::Empty) => {}
                Arg::Scalar(v) => out.push(to_num(&v)?),
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
                Arg::Lambda(_) => return Err(ExcelError::Calc),
                // Computed arrays aggregate by the same rules as ranges,
                // e.g. SUM((A1:A3)*2) or SUM(SEQUENCE(5)).
                Arg::Matrix(m) => {
                    for v in m.iter().flatten() {
                        match v {
                            Value::Num(n) => out.push(*n),
                            Value::Err(e) => return Err(*e),
                            Value::Bool(b) if !numbers_only => out.push(if *b { 1.0 } else { 0.0 }),
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Like `collect_values` but for the …A functions (AVERAGEA, MAXA, …):
    /// text counts as 0 and logicals as 1/0, everywhere (ranges included).
    fn collect_a(&mut self, args: &[Expr]) -> Result<Vec<f64>, ExcelError> {
        let mut out = Vec::new();
        let push = |out: &mut Vec<f64>, v: &Value| -> Result<(), ExcelError> {
            match v {
                Value::Num(n) => out.push(*n),
                Value::Bool(b) => out.push(if *b { 1.0 } else { 0.0 }),
                Value::Str(_) => out.push(0.0),
                Value::Err(e) => return Err(*e),
                Value::Empty => {}
            }
            Ok(())
        };
        for a in args {
            if matches!(a, Expr::Ref3D { .. }) {
                for (s, r1, c1, r2, c2) in self.resolve_3d(a)? {
                    for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                        push(&mut out, &v)?;
                    }
                }
                continue;
            }
            match self.eval_arg(a) {
                Arg::Scalar(v) => push(&mut out, &v)?,
                Arg::Range(s, r1, c1, r2, c2) => {
                    for (_, v) in self.res.cells_in(s, r1, c1, r2, c2) {
                        push(&mut out, &v)?;
                    }
                }
                Arg::Matrix(m) => {
                    for v in m.iter().flatten() {
                        push(&mut out, v)?;
                    }
                }
                Arg::Lambda(_) => return Err(ExcelError::Calc),
            }
        }
        Ok(out)
    }

    /// Floored serials of every holiday cell/scalar in `args` (bad values
    /// simply drop out — WORKDAY/NETWORKDAYS ignore non-dates).
    fn holiday_set(&mut self, args: &[Expr]) -> Vec<i64> {
        match self.collect_values(args, true) {
            Ok(v) => v.into_iter().map(|d| d.floor() as i64).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Paired-array moments: `(n, mean_x, mean_y, Sxx, Syy, Sxy)` for the
    /// numeric pairs the two arrays share. Empty → `#DIV/0!`.
    #[allow(clippy::type_complexity)]
    fn pair_stats(
        &mut self,
        xa: &Expr,
        ya: &Expr,
    ) -> Result<(usize, f64, f64, f64, f64, f64), Value> {
        let (xs, ys) = self.two_arrays(xa, ya)?;
        let n = xs.len();
        if n == 0 {
            return Err(Value::Err(ExcelError::Div0));
        }
        let mx = xs.iter().sum::<f64>() / n as f64;
        let my = ys.iter().sum::<f64>() / n as f64;
        let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
        for (x, y) in xs.iter().zip(ys.iter()) {
            sxx += (x - mx) * (x - mx);
            syy += (y - my) * (y - my);
            sxy += (x - mx) * (y - my);
        }
        Ok((n, mx, my, sxx, syy, sxy))
    }

    /// Expand a 3D span into one rect per sheet in the run (tab order).
    #[allow(clippy::type_complexity)]
    fn resolve_3d(&mut self, e: &Expr) -> Result<Vec<(usize, u32, u32, u32, u32)>, ExcelError> {
        let Expr::Ref3D { first, last, a, b } = e else {
            return Err(ExcelError::Value);
        };
        if a.row < 0 || a.col < 0 || b.row < 0 || b.col < 0 {
            return Err(ExcelError::Ref);
        }
        let (Some(s1), Some(s2)) = (self.res.sheet_index(first), self.res.sheet_index(last)) else {
            return Err(ExcelError::Ref);
        };
        let (lo, hi) = (s1.min(s2), s1.max(s2));
        let rect = (
            a.row.min(b.row) as u32,
            a.col.min(b.col) as u32,
            a.row.max(b.row) as u32,
            a.col.max(b.col) as u32,
        );
        Ok((lo..=hi)
            .map(|s| (s, rect.0, rect.1, rect.2, rect.3))
            .collect())
    }

    /// A range argument or an error value (scalars and computed arrays
    /// don't qualify — criteria functions need actual references).
    fn arg_range(&mut self, e: &Expr) -> Result<(usize, u32, u32, u32, u32), Value> {
        match self.eval_arg(e) {
            Arg::Range(s, a, b, c, d) => Ok((s, a, b, c, d)),
            Arg::Scalar(v) => Err(if v.is_err() {
                v
            } else {
                Value::Err(ExcelError::Value)
            }),
            Arg::Matrix(_) => Err(Value::Err(ExcelError::Value)),
            Arg::Lambda(_) => Err(Value::Err(ExcelError::Calc)),
        }
    }

    /// A lookup table argument: an on-sheet rect or a computed matrix
    /// (scalars don't qualify).
    fn arg_view(&mut self, e: &Expr) -> Result<TView, Value> {
        match self.eval_arg(e) {
            Arg::Range(s, a, b, c, d) => Ok(TView::Range(s, a, b, c, d)),
            Arg::Matrix(m) => Ok(TView::Mat(m)),
            Arg::Scalar(v) => Err(if v.is_err() {
                v
            } else {
                Value::Err(ExcelError::Value)
            }),
            Arg::Lambda(_) => Err(Value::Err(ExcelError::Calc)),
        }
    }

    /// Logical (unclamped) dims of a view.
    fn view_dims(&self, v: &TView) -> (u32, u32) {
        match v {
            TView::Range(_, r1, c1, r2, c2) => (r2 - r1 + 1, c2 - c1 + 1),
            TView::Mat(m) => (m.len() as u32, m[0].len() as u32),
        }
    }

    /// Dims clamped to the used range, for dense scans (whole-column refs
    /// would otherwise walk a million cells).
    fn view_scan_dims(&self, v: &TView) -> (u32, u32) {
        match v {
            TView::Range(s, r1, c1, r2, c2) => {
                let (r1c, c1c, r2c, c2c) = self.clamp(*s, *r1, *c1, *r2, *c2);
                (r2c - r1c + 1, c2c - c1c + 1)
            }
            TView::Mat(m) => (m.len() as u32, m[0].len() as u32),
        }
    }

    /// Element at (row, col) offsets from a view's top-left.
    fn view_get(&self, v: &TView, dr: u32, dc: u32) -> Value {
        match v {
            TView::Range(s, r1, c1, ..) => self.res.value(*s, r1 + dr, c1 + dc),
            TView::Mat(m) => m
                .get(dr as usize)
                .and_then(|row| row.get(dc as usize))
                .cloned()
                .unwrap_or(Value::Empty),
        }
    }

    /// A 1-D vector (single row or column) read densely, clamped to the used
    /// range — the shape XLOOKUP/LOOKUP/MATCH vectors want.
    fn arg_vector(&mut self, e: &Expr) -> Result<Vec<Value>, Value> {
        let view = self.arg_view(e)?;
        let (h, w) = self.view_scan_dims(&view);
        if h != 1 && w != 1 {
            return Err(Value::Err(ExcelError::Value));
        }
        let mut vals = Vec::new();
        if w == 1 {
            for r in 0..h {
                vals.push(self.view_get(&view, r, 0));
            }
        } else {
            for c in 0..w {
                vals.push(self.view_get(&view, 0, c));
            }
        }
        Ok(vals)
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
        let (s, r1, c1, r2, c2) = self.arg_range(range)?;
        let (ss, sr1, sc1) = match sum_range {
            None => (s, r1, c1),
            Some(e) => {
                let (ss, a, b, _, _) = self.arg_range(e)?;
                (ss, a, b)
            }
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
            "ASINH" => self.one_num(args, |n| num(n.asinh())),
            "ACOSH" => self.one_num(args, |n| {
                if n < 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                num(n.acosh())
            }),
            "ATANH" => self.one_num(args, |n| {
                if n <= -1.0 || n >= 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                num(n.atanh())
            }),
            "SEC" => self.one_num(args, |n| num(1.0 / n.cos())),
            "CSC" => self.one_num(args, |n| num(1.0 / n.sin())),
            "COT" => self.one_num(args, |n| {
                if n == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(1.0 / n.tan())
            }),
            "SECH" => self.one_num(args, |n| num(1.0 / n.cosh())),
            "CSCH" => self.one_num(args, |n| num(1.0 / n.sinh())),
            "COTH" => self.one_num(args, |n| {
                if n == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(1.0 / n.tanh())
            }),
            "ACOT" => self.one_num(args, |n| {
                // Excel returns a principal value in (0, π).
                num(std::f64::consts::FRAC_PI_2 - n.atan())
            }),
            "ACOTH" => self.one_num(args, |n| {
                if n.abs() <= 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                num(0.5 * ((n + 1.0) / (n - 1.0)).ln())
            }),
            "FLOOR" => self.two_num(args, |n, sig| {
                if sig == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                // Classic FLOOR requires matching signs.
                if n * sig < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((n / sig).floor() * sig)
            }),
            "CEILING" => self.two_num(args, |n, sig| {
                if sig == 0.0 {
                    return Value::Num(0.0);
                }
                if n > 0.0 && sig < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((n / sig).ceil() * sig)
            }),
            // CEILING.MATH(number, [significance=1], [mode=0]) — significance
            // sign is ignored; for negatives, mode 0 rounds toward zero, mode≠0
            // rounds away from zero. FLOOR.MATH is the mirror.
            "CEILING.MATH" | "FLOOR.MATH" => {
                if args.is_empty() || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let sig = if args.len() >= 2 {
                    let s = try_num!(self.eval(&args[1]));
                    if s == 0.0 {
                        return Value::Num(0.0);
                    }
                    s.abs()
                } else {
                    1.0
                };
                let mode = if args.len() == 3 {
                    try_num!(self.eval(&args[2]))
                } else {
                    0.0
                };
                let q = n / sig;
                let up = name == "CEILING.MATH";
                // Default direction: ceiling→+dir, floor→−dir; the mode flag
                // flips only the negative side.
                let toward_pos = if up {
                    !(n < 0.0 && mode != 0.0)
                } else {
                    n < 0.0 && mode != 0.0
                };
                num(if toward_pos { q.ceil() } else { q.floor() } * sig)
            }
            // CEILING.PRECISE / ISO.CEILING / FLOOR.PRECISE: round to a multiple
            // of |significance| (default 1), toward +inf (ceiling) or -inf
            // (floor) regardless of sign. Significance sign is ignored.
            "CEILING.PRECISE" | "ISO.CEILING" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let sig = if args.len() == 2 {
                    try_num!(self.eval(&args[1])).abs()
                } else {
                    1.0
                };
                if sig == 0.0 {
                    return Value::Num(0.0);
                }
                num((n / sig).ceil() * sig)
            }
            "FLOOR.PRECISE" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let sig = if args.len() == 2 {
                    try_num!(self.eval(&args[1])).abs()
                } else {
                    1.0
                };
                if sig == 0.0 {
                    return Value::Num(0.0);
                }
                num((n / sig).floor() * sig)
            }
            // SUBTOTAL(code, ref1, …): aggregate ignoring nested subtotals, and
            // (for 101–111) hidden rows.
            "SUBTOTAL" => {
                if args.len() < 2 {
                    return Value::Err(ExcelError::Value);
                }
                let code = try_num!(self.eval(&args[0])).trunc() as i64;
                let base = match code {
                    1..=11 => code,
                    101..=111 => code - 100,
                    _ => return Value::Err(ExcelError::Value),
                };
                // Excel: codes 1–11 already exclude *filter*-hidden rows (the
                // common case); 101–111 additionally exclude *manually*-hidden
                // rows. We can't always tell the two apart, and SUBTOTAL is
                // almost always used over filtered data, so we exclude hidden
                // rows for both — matching Excel-with-filter and our oracle.
                match self.collect_subtotal(&args[1..], true, false) {
                    Ok((nums, counta)) => self.apply_agg(base, &nums, counta),
                    Err(e) => Value::Err(e),
                }
            }
            // AGGREGATE(func, options, ref1, …) / AGGREGATE(14–19, options, array, k).
            "AGGREGATE" => {
                if args.len() < 3 {
                    return Value::Err(ExcelError::Value);
                }
                let func = try_num!(self.eval(&args[0])).trunc() as i64;
                let opts = try_num!(self.eval(&args[1])).trunc() as i64;
                if !(1..=19).contains(&func) || !(0..=7).contains(&opts) {
                    return Value::Err(ExcelError::Value);
                }
                let ignore_hidden = matches!(opts, 1 | 3 | 5 | 7);
                let ignore_errors = matches!(opts, 2 | 3 | 6 | 7);
                if func <= 13 {
                    match self.collect_subtotal(&args[2..], ignore_hidden, ignore_errors) {
                        Ok((nums, counta)) => self.apply_agg(func, &nums, counta),
                        Err(e) => Value::Err(e),
                    }
                } else if args.len() != 4 {
                    // 14–19 take exactly one array plus a k argument.
                    Value::Err(ExcelError::Value)
                } else {
                    let nums =
                        match self.collect_subtotal(&args[2..3], ignore_hidden, ignore_errors) {
                            Ok((nums, _)) => nums,
                            Err(e) => return Value::Err(e),
                        };
                    let k = try_num!(self.eval(&args[3]));
                    self.apply_agg_k(func, &nums, k)
                }
            }
            // FORMULATEXT(ref): the source formula of the reference's top-left
            // cell, with a leading `=`; #N/A if it has none. Coordinates come
            // from the reference *structurally* (not by evaluating it), so it
            // works even when the target is an array/spill anchor.
            "FORMULATEXT" => {
                if args.len() != 1 {
                    return Value::Err(ExcelError::Value);
                }
                match self.ref_coords(&args[0]) {
                    Some((s, r, c)) => match self.res.cell_formula(s, r, c) {
                        Some(f) => Value::Str(format!("={}", display_formula(&f))),
                        None => Value::Err(ExcelError::NA),
                    },
                    None => Value::Err(ExcelError::NA),
                }
            }
            "CELL" => self.cell_info(args),
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
            // ---- row-context iterators (DAX-flavored) -----------------
            // SUMX(Table, expr) evaluates expr once per data row of the
            // table, with bare [@Col] references resolving to that row.
            "SUMX" | "AVERAGEX" | "MAXX" | "MINX" | "COUNTX" | "COUNTAX" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let info = match &args[0] {
                    Expr::Name(n) => self.res.table(n),
                    Expr::Structured {
                        table: Some(t),
                        item: TableItem::Data | TableItem::All,
                        col1: None,
                        ..
                    } => self.res.table(t),
                    _ => None,
                };
                let Some(info) = info else {
                    // A table we can't see — don't guess.
                    self.unsupported = true;
                    return Value::Err(ExcelError::Name);
                };
                let (r1, c1, r2, _) = info.range;
                let lo = r1 + info.header_rows;
                let Some(hi) = r2.checked_sub(info.totals_rows) else {
                    return Value::Err(ExcelError::Ref);
                };
                if lo > hi {
                    return Value::Err(ExcelError::Ref);
                }
                let (save_sheet, save_cell) = (self.sheet, self.cell);
                let mut results: Vec<Value> = Vec::with_capacity((hi - lo + 1) as usize);
                for r in lo..=hi {
                    self.sheet = info.sheet;
                    self.cell = (r, c1);
                    results.push(self.eval(&args[1]));
                }
                self.sheet = save_sheet;
                self.cell = save_cell;
                let mut nums = Vec::with_capacity(results.len());
                let mut nonempty = 0usize;
                for v in &results {
                    match v {
                        Value::Err(e) => return Value::Err(*e),
                        Value::Num(x) => {
                            nums.push(*x);
                            nonempty += 1;
                        }
                        Value::Empty => {}
                        _ => nonempty += 1,
                    }
                }
                match name {
                    "SUMX" => num(nums.iter().sum()),
                    "AVERAGEX" => {
                        if nums.is_empty() {
                            Value::Err(ExcelError::Div0)
                        } else {
                            num(nums.iter().sum::<f64>() / nums.len() as f64)
                        }
                    }
                    "MAXX" => {
                        Value::Num(nums.iter().copied().fold(f64::MIN, f64::max)).num_or_zero(&nums)
                    }
                    "MINX" => {
                        Value::Num(nums.iter().copied().fold(f64::MAX, f64::min)).num_or_zero(&nums)
                    }
                    "COUNTX" => Value::Num(nums.len() as f64),
                    _ => Value::Num(nonempty as f64), // COUNTAX
                }
            }
            "SUMPRODUCT" => {
                // Same-shape factors, elementwise product summed. Factors may
                // be ranges or computed arrays: SUMPRODUCT((A1:A3>2)*1,B1:B3).
                let mut mats: Vec<Matrix> = Vec::new();
                for a in args {
                    let arg = self.eval_arg(a);
                    match self.materialize(arg) {
                        Ok(m) => mats.push(m),
                        Err(e) => return Value::Err(e),
                    }
                }
                if mats.is_empty() {
                    return Value::Err(ExcelError::Value);
                }
                let (rows, cols) = (mats[0].len(), mats[0][0].len());
                if mats.iter().any(|m| m.len() != rows || m[0].len() != cols) {
                    return Value::Err(ExcelError::Value);
                }
                let mut total = 0.0;
                for i in 0..rows {
                    for j in 0..cols {
                        let mut p = 1.0;
                        for m in &mats {
                            p *= match &m[i][j] {
                                Value::Num(n) => *n,
                                Value::Err(e) => return Value::Err(*e),
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
                    if matches!(a, Expr::Ref3D { .. }) {
                        if let Ok(rects) = self.resolve_3d(a) {
                            for (s, r1, c1, r2, c2) in rects {
                                n += self
                                    .res
                                    .cells_in(s, r1, c1, r2, c2)
                                    .iter()
                                    .filter(|(_, v)| matches!(v, Value::Num(_)))
                                    .count();
                            }
                        }
                        continue;
                    }
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
                        Arg::Matrix(m) => {
                            n += m
                                .iter()
                                .flatten()
                                .filter(|v| matches!(v, Value::Num(_)))
                                .count();
                        }
                        Arg::Lambda(_) => return Value::Err(ExcelError::Calc),
                    }
                }
                Value::Num(n as f64)
            }
            "COUNTA" => {
                let mut n = 0usize;
                for a in args {
                    if matches!(a, Expr::Ref3D { .. }) {
                        if let Ok(rects) = self.resolve_3d(a) {
                            for (s, r1, c1, r2, c2) in rects {
                                n += self.res.cells_in(s, r1, c1, r2, c2).len();
                            }
                        }
                        continue;
                    }
                    match self.eval_arg(a) {
                        Arg::Scalar(Value::Empty) => {}
                        Arg::Scalar(_) => n += 1,
                        Arg::Range(s, r1, c1, r2, c2) => {
                            n += self.res.cells_in(s, r1, c1, r2, c2).len();
                        }
                        Arg::Matrix(m) => {
                            n += m
                                .iter()
                                .flatten()
                                .filter(|v| !matches!(v, Value::Empty))
                                .count();
                        }
                        Arg::Lambda(_) => return Value::Err(ExcelError::Calc),
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
                    Arg::Matrix(m) => Value::Num(
                        m.iter()
                            .flatten()
                            .filter(|v| {
                                matches!(v, Value::Empty)
                                    || matches!(v, Value::Str(s) if s.is_empty())
                            })
                            .count() as f64,
                    ),
                    Arg::Scalar(_) => Value::Err(ExcelError::Value),
                    Arg::Lambda(_) => Value::Err(ExcelError::Calc),
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
                        Arg::Lambda(_) => return Value::Err(ExcelError::Calc),
                        // Array results contribute like ranges: bools and
                        // numbers count, text/empty skipped, errors propagate.
                        Arg::Matrix(m) => {
                            for v in m.iter().flatten() {
                                match v {
                                    Value::Bool(_) | Value::Num(_) => {
                                        let b = *v != Value::Bool(false) && *v != Value::Num(0.0);
                                        any = true;
                                        acc = match name {
                                            "AND" => acc && b,
                                            "OR" => acc || b,
                                            _ => acc ^ b,
                                        };
                                    }
                                    Value::Err(e) => return Value::Err(*e),
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
                // Clamp both bounds to the string length *before* adding — a
                // huge `count` (e.g. 1e20) saturates `as usize` to usize::MAX,
                // and `from + that` would overflow and panic on the slice.
                let from = ((start.trunc() as usize).saturating_sub(1)).min(chars.len());
                let count = (count.trunc() as usize).min(chars.len());
                let to = from.saturating_add(count).min(chars.len());
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
                        Arg::Matrix(m) => {
                            for v in m.into_iter().flatten() {
                                out.push_str(&try_text!(v));
                            }
                        }
                        Arg::Lambda(_) => return Value::Err(ExcelError::Calc),
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
                        Arg::Matrix(m) => {
                            for v in m.into_iter().flatten() {
                                if !matches!(v, Value::Empty) {
                                    parts.push(try_text!(v));
                                }
                            }
                        }
                        Arg::Lambda(_) => return Value::Err(ExcelError::Calc),
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
                // Clamp before adding (see MID) — a huge `count` would
                // otherwise overflow `from + count` and panic.
                let from = ((start.trunc() as usize).saturating_sub(1)).min(chars.len());
                let count = (count.trunc() as usize).min(chars.len());
                let to = from.saturating_add(count).min(chars.len());
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
                let y = try_num!(self.eval(&args[0])).trunc();
                let m = try_num!(self.eval(&args[1])).trunc();
                let d = try_num!(self.eval(&args[2])).trunc();
                // Excel caps years at 9999; bounding here also keeps the
                // month/day rollover arithmetic well inside i64 (a huge year
                // argument would otherwise overflow `y * 12`).
                if !(0.0..=10_000.0).contains(&y) || m.abs() > 1_200_000.0 || d.abs() > 1e9 {
                    return Value::Err(ExcelError::Num);
                }
                let (y, m, d) = (y as i64, m as i64, d as i64);
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
                        // The weekday formula assumes the 1900 epoch (serial 1
                        // = "Sunday"); a 1904 workbook's serials are shifted by
                        // 1462 days, so rebase before the mod-7.
                        let day1900 =
                            serial.floor() as i64 + if self.res.date1904() { 1462 } else { 0 };
                        let sun0 = ((day1900 - 1) % 7 + 7) % 7; // 0=Sun … 6=Sat
                        let mon0 = (sun0 + 6) % 7; // 0=Mon … 6=Sun
                        match mode {
                            1 => Value::Num(sun0 as f64 + 1.0),      // Sun=1 … Sat=7
                            2 | 11 => Value::Num(mon0 as f64 + 1.0), // Mon=1 … Sun=7
                            3 => Value::Num(mon0 as f64),            // Mon=0 … Sun=6
                            // 12..17: week starts Tue..Sun, result 1..7.
                            12..=17 => {
                                let k = mode - 11; // days after Monday the week starts
                                Value::Num(((mon0 - k).rem_euclid(7)) as f64 + 1.0)
                            }
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
                let view = match self.arg_view(&args[1]) {
                    Ok(v) => v,
                    Err(v) => return v,
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
                let (h, w) = self.view_dims(&view);
                let (sh, sw) = self.view_scan_dims(&view);
                let mut best: Option<u32> = None;
                for lane in 0..(if vertical { sh } else { sw }) {
                    let key = if vertical {
                        self.view_get(&view, lane, 0)
                    } else {
                        self.view_get(&view, 0, lane)
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
                        if idx >= if vertical { w } else { h } {
                            return Value::Err(ExcelError::Ref);
                        }
                        if vertical {
                            self.view_get(&view, lane, idx)
                        } else {
                            self.view_get(&view, idx, lane)
                        }
                    }
                }
            }
            "MATCH" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = self.eval(&args[0]);
                let view = match self.eval_arg(&args[1]) {
                    Arg::Range(s, a, b, c, d) => TView::Range(s, a, b, c, d),
                    Arg::Matrix(m) => TView::Mat(m),
                    Arg::Lambda(_) => return Value::Err(ExcelError::Calc),
                    Arg::Scalar(v) => {
                        return if v.is_err() {
                            v
                        } else {
                            Value::Err(ExcelError::NA)
                        };
                    }
                };
                let (h, w) = self.view_scan_dims(&view);
                if h != 1 && w != 1 {
                    return Value::Err(ExcelError::NA);
                }
                let mut vals = Vec::new();
                if w == 1 {
                    for r in 0..h {
                        vals.push(self.view_get(&view, r, 0));
                    }
                } else {
                    for c in 0..w {
                        vals.push(self.view_get(&view, 0, c));
                    }
                }
                let mode = match args.get(2) {
                    Some(e) => try_num!(self.eval(e)),
                    None => 1.0,
                };
                match_scan(&vals, &needle, mode)
            }
            "INDEX" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let view = match self.arg_view(&args[0]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                let (h, w) = self.view_dims(&view);
                let ri = try_num!(self.eval(&args[1])).trunc() as i64;
                let ci = match args.get(2) {
                    Some(e) => try_num!(self.eval(e)).trunc() as i64,
                    None => {
                        // One-dimensional form: index along the single lane.
                        if h == 1 {
                            if ri < 1 || ri > w as i64 {
                                return Value::Err(ExcelError::Ref);
                            }
                            return self.view_get(&view, 0, ri as u32 - 1);
                        }
                        1
                    }
                };
                if ri < 1 || ci < 1 {
                    return Value::Err(ExcelError::Value);
                }
                if ri > h as i64 || ci > w as i64 {
                    return Value::Err(ExcelError::Ref);
                }
                self.view_get(&view, ri as u32 - 1, ci as u32 - 1)
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
                let lookup = match self.arg_vector(&args[1]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                let ret = match self.arg_vector(&args[2]) {
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
                    let v = lookup[i].clone();
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
                    Some(i) => ret[i].clone(),
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
                let lookup = match self.arg_vector(&args[1]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                let ret = match args.get(2) {
                    Some(e) => match self.arg_vector(e) {
                        Ok(v) => v,
                        Err(v) => return v,
                    },
                    None => lookup.clone(),
                };
                let mut best: Option<usize> = None;
                for (i, v) in lookup.iter().enumerate() {
                    if matches!(v, Value::Empty) {
                        continue;
                    }
                    match compare(v, &needle) {
                        Ok(std::cmp::Ordering::Equal) | Ok(std::cmp::Ordering::Less) => {
                            best = Some(i);
                        }
                        _ => {}
                    }
                }
                match best {
                    Some(i) if i < ret.len() => ret[i].clone(),
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

            // ---- more date/time ------------------------------------------------
            "DATEVALUE" => {
                if args.len() != 1 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                match parse_date_text(&s, self.res.date1904()) {
                    Some(serial) => num(serial.floor()),
                    None => Value::Err(ExcelError::Value),
                }
            }
            "TIMEVALUE" => self.one_text(args, |s| {
                // Accept a leading date part (Excel ignores it), keep the time.
                let frac = parse_time_text(&s)
                    .or_else(|| s.rsplit_once(' ').and_then(|(_, t)| parse_time_text(t)));
                match frac {
                    Some(f) => num(f),
                    None => Value::Err(ExcelError::Value),
                }
            }),
            "YEARFRAC" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let start = try_num!(self.eval(&args[0]));
                let end = try_num!(self.eval(&args[1]));
                let basis = match args.get(2) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                }
                .trunc();
                let d1904 = self.res.date1904();
                let (mut s, mut e) = (start.floor(), end.floor());
                if s > e {
                    std::mem::swap(&mut s, &mut e);
                }
                let frac = match basis as i64 {
                    0 | 4 => {
                        let (Some(a), Some(b)) =
                            (serial_to_parts(s, d1904), serial_to_parts(e, d1904))
                        else {
                            return Value::Err(ExcelError::Num);
                        };
                        let days =
                            days_360(a.year, a.month, a.day, b.year, b.month, b.day, basis == 4.0);
                        days as f64 / 360.0
                    }
                    1 => {
                        let (Some(a), Some(b)) =
                            (serial_to_parts(s, d1904), serial_to_parts(e, d1904))
                        else {
                            return Value::Err(ExcelError::Num);
                        };
                        let denom = if a.year == b.year {
                            let leap = days_in_month(a.year, 2) == 29;
                            if leap { 366.0 } else { 365.0 }
                        } else {
                            let years = (b.year - a.year + 1) as f64;
                            let span = parts_to_serial(b.year + 1, 1, 1, 0, d1904)
                                - parts_to_serial(a.year, 1, 1, 0, d1904);
                            span / years
                        };
                        (e - s) / denom
                    }
                    2 => (e - s) / 360.0,
                    3 => (e - s) / 365.0,
                    _ => return Value::Err(ExcelError::Num),
                };
                num(frac)
            }
            "DAYS360" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let start = try_num!(self.eval(&args[0]));
                let end = try_num!(self.eval(&args[1]));
                let european = match args.get(2) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => try_num!(self.eval(e)) != 0.0,
                };
                let d1904 = self.res.date1904();
                let (Some(a), Some(b)) = (
                    serial_to_parts(start.floor(), d1904),
                    serial_to_parts(end.floor(), d1904),
                ) else {
                    return Value::Err(ExcelError::Num);
                };
                num(days_360(a.year, a.month, a.day, b.year, b.month, b.day, european) as f64)
            }
            "WORKDAY" | "WORKDAY.INTL" => {
                if args.len() < 2 {
                    return Value::Err(ExcelError::Value);
                }
                let start = try_num!(self.eval(&args[0]));
                let days = try_num!(self.eval(&args[1]));
                let intl = name == "WORKDAY.INTL";
                let (mask, hol_idx) = if intl {
                    let m = match args.get(2) {
                        None | Some(Expr::Missing) => {
                            [false, false, false, false, false, true, true]
                        }
                        Some(e) => match weekend_mask(&self.eval(e)) {
                            Some(m) => m,
                            None => return Value::Err(ExcelError::Value),
                        },
                    };
                    (m, 3)
                } else {
                    ([false, false, false, false, false, true, true], 2)
                };
                if mask.iter().all(|&b| b) {
                    return Value::Err(ExcelError::Num);
                }
                let holidays = self.holiday_set(args.get(hol_idx..).unwrap_or(&[]));
                let d1904 = self.res.date1904();
                let mut serial = start.floor() as i64;
                let step = if days >= 0.0 { 1 } else { -1 };
                let mut remaining = days.trunc().abs() as i64;
                let mut guard = 0;
                while remaining > 0 {
                    serial += step;
                    guard += 1;
                    if guard > 10_000_000 {
                        return Value::Err(ExcelError::Num);
                    }
                    if is_weekend(serial, d1904, &mask) || holidays.contains(&serial) {
                        continue;
                    }
                    remaining -= 1;
                }
                num(serial as f64)
            }
            "NETWORKDAYS" | "NETWORKDAYS.INTL" => {
                if args.len() < 2 {
                    return Value::Err(ExcelError::Value);
                }
                let start = try_num!(self.eval(&args[0]));
                let end = try_num!(self.eval(&args[1]));
                let intl = name == "NETWORKDAYS.INTL";
                let (mask, hol_idx) = if intl {
                    let m = match args.get(2) {
                        None | Some(Expr::Missing) => {
                            [false, false, false, false, false, true, true]
                        }
                        Some(e) => match weekend_mask(&self.eval(e)) {
                            Some(m) => m,
                            None => return Value::Err(ExcelError::Value),
                        },
                    };
                    (m, 3)
                } else {
                    ([false, false, false, false, false, true, true], 2)
                };
                let holidays = self.holiday_set(args.get(hol_idx..).unwrap_or(&[]));
                let d1904 = self.res.date1904();
                let (a, b) = (start.floor() as i64, end.floor() as i64);
                let (lo, hi, sign) = if a <= b { (a, b, 1) } else { (b, a, -1) };
                if hi - lo > 10_000_000 {
                    return Value::Err(ExcelError::Num);
                }
                let mut count = 0i64;
                for s in lo..=hi {
                    if !is_weekend(s, d1904, &mask) && !holidays.contains(&s) {
                        count += 1;
                    }
                }
                num((count * sign) as f64)
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

            "IPMT" | "PPMT" => {
                if args.len() < 4 || args.len() > 6 {
                    return Value::Err(ExcelError::Value);
                }
                let rate = try_num!(self.eval(&args[0]));
                let per = try_num!(self.eval(&args[1]));
                let nper = try_num!(self.eval(&args[2]));
                let pv = try_num!(self.eval(&args[3]));
                let fv = match args.get(4) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                let t = match args.get(5) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => {
                        if try_num!(self.eval(e)) != 0.0 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                };
                if per < 1.0 || per > nper {
                    return Value::Err(ExcelError::Num);
                }
                let ipmt = fin_ipmt(rate, per, nper, pv, fv, t);
                if name == "IPMT" {
                    num(ipmt)
                } else {
                    num(fin_pmt(rate, nper, pv, fv, t) - ipmt)
                }
            }
            "ISPMT" => {
                if args.len() != 4 {
                    return Value::Err(ExcelError::Value);
                }
                let rate = try_num!(self.eval(&args[0]));
                let per = try_num!(self.eval(&args[1]));
                let nper = try_num!(self.eval(&args[2]));
                let pv = try_num!(self.eval(&args[3]));
                if nper == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(pv * rate * (per / nper - 1.0))
            }
            "SLN" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let cost = try_num!(self.eval(&args[0]));
                let salvage = try_num!(self.eval(&args[1]));
                let life = try_num!(self.eval(&args[2]));
                if life == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num((cost - salvage) / life)
            }
            "SYD" => {
                if args.len() != 4 {
                    return Value::Err(ExcelError::Value);
                }
                let cost = try_num!(self.eval(&args[0]));
                let salvage = try_num!(self.eval(&args[1]));
                let life = try_num!(self.eval(&args[2]));
                let per = try_num!(self.eval(&args[3]));
                if life <= 0.0 || per < 1.0 || per > life {
                    return Value::Err(ExcelError::Num);
                }
                num((cost - salvage) * (life - per + 1.0) * 2.0 / (life * (life + 1.0)))
            }
            "DDB" => {
                if args.len() < 4 || args.len() > 5 {
                    return Value::Err(ExcelError::Value);
                }
                let cost = try_num!(self.eval(&args[0]));
                let salvage = try_num!(self.eval(&args[1]));
                let life = try_num!(self.eval(&args[2]));
                let period = try_num!(self.eval(&args[3]));
                let factor = match args.get(4) {
                    None | Some(Expr::Missing) => 2.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                if cost < 0.0
                    || salvage < 0.0
                    || life <= 0.0
                    || period < 1.0
                    || period > life
                    || factor <= 0.0
                {
                    return Value::Err(ExcelError::Num);
                }
                let interest = (factor / life).min(1.0);
                let old_val = if interest >= 1.0 {
                    if period == 1.0 { cost } else { 0.0 }
                } else {
                    cost * (1.0 - interest).powf(period - 1.0)
                };
                let new_val = cost * (1.0 - interest).powf(period);
                let ddb = if new_val < salvage {
                    old_val - salvage
                } else {
                    old_val - new_val
                };
                num(ddb.max(0.0))
            }
            "DB" => {
                if args.len() < 4 || args.len() > 5 {
                    return Value::Err(ExcelError::Value);
                }
                let cost = try_num!(self.eval(&args[0]));
                let salvage = try_num!(self.eval(&args[1]));
                let life = try_num!(self.eval(&args[2]));
                let period = try_num!(self.eval(&args[3]));
                let month = match args.get(4) {
                    None | Some(Expr::Missing) => 12.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                match fin_db(cost, salvage, life, period, month) {
                    Some(v) => num(v),
                    None => Value::Err(ExcelError::Num),
                }
            }
            "EFFECT" => self.two_num(args, |nominal, npery| {
                let npery = npery.trunc();
                if nominal <= 0.0 || npery < 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((1.0 + nominal / npery).powf(npery) - 1.0)
            }),
            "NOMINAL" => self.two_num(args, |effect, npery| {
                let npery = npery.trunc();
                if effect <= 0.0 || npery < 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                num(npery * ((1.0 + effect).powf(1.0 / npery) - 1.0))
            }),
            "PDURATION" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let rate = try_num!(self.eval(&args[0]));
                let pv = try_num!(self.eval(&args[1]));
                let fv = try_num!(self.eval(&args[2]));
                if rate <= 0.0 || pv <= 0.0 || fv <= 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((fv.ln() - pv.ln()) / (1.0 + rate).ln())
            }
            "RRI" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let nper = try_num!(self.eval(&args[0]));
                let pv = try_num!(self.eval(&args[1]));
                let fv = try_num!(self.eval(&args[2]));
                if nper <= 0.0 || pv == 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((fv / pv).powf(1.0 / nper) - 1.0)
            }
            "DOLLARDE" | "DOLLARFR" => self.two_num(args, |d, frac| {
                let frac = frac.trunc();
                if frac < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                if frac == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                let sign = if d < 0.0 { -1.0 } else { 1.0 };
                let d = d.abs();
                let int_part = d.trunc();
                let frac_part = d - int_part;
                // Number of decimal positions the fraction occupies.
                let digits = 10f64.powf(frac.log10().ceil().max(0.0));
                let r = if name == "DOLLARDE" {
                    int_part + frac_part * digits / frac
                } else {
                    int_part + frac_part * frac / digits
                };
                num(sign * r)
            }),
            "MIRR" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let vals = match self.collect_values(&args[..1], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                let finance = try_num!(self.eval(&args[1]));
                let reinvest = try_num!(self.eval(&args[2]));
                let n = vals.len();
                if n < 2 || !vals.iter().any(|&v| v > 0.0) || !vals.iter().any(|&v| v < 0.0) {
                    return Value::Err(ExcelError::Div0);
                }
                let mut pv_neg = 0.0;
                let mut fv_pos = 0.0;
                for (i, &v) in vals.iter().enumerate() {
                    if v < 0.0 {
                        pv_neg += v / (1.0 + finance).powi(i as i32);
                    } else {
                        fv_pos += v * (1.0 + reinvest).powi((n - 1 - i) as i32);
                    }
                }
                if pv_neg == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num((-fv_pos / pv_neg).powf(1.0 / (n as f64 - 1.0)) - 1.0)
            }
            "XNPV" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let rate = try_num!(self.eval(&args[0]));
                let vals = match self.collect_values(&args[1..2], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                let dates = match self.collect_values(&args[2..3], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                if vals.len() != dates.len() || vals.is_empty() || rate <= -1.0 {
                    return Value::Err(ExcelError::Num);
                }
                let d0 = dates[0];
                let mut total = 0.0;
                for (v, d) in vals.iter().zip(dates.iter()) {
                    total += v / (1.0 + rate).powf((d.trunc() - d0.trunc()) / 365.0);
                }
                num(total)
            }
            "XIRR" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let vals = match self.collect_values(&args[..1], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                let dates = match self.collect_values(&args[1..2], true) {
                    Ok(v) => v,
                    Err(e) => return Value::Err(e),
                };
                if vals.len() != dates.len() || vals.len() < 2 {
                    return Value::Err(ExcelError::Num);
                }
                if !vals.iter().any(|&v| v > 0.0) || !vals.iter().any(|&v| v < 0.0) {
                    return Value::Err(ExcelError::Num);
                }
                let guess = match args.get(2) {
                    None | Some(Expr::Missing) => 0.1,
                    Some(e) => try_num!(self.eval(e)),
                };
                let d0 = dates[0].trunc();
                let xnpv = |r: f64| -> f64 {
                    vals.iter()
                        .zip(dates.iter())
                        .map(|(v, d)| v / (1.0 + r).powf((d.trunc() - d0) / 365.0))
                        .sum()
                };
                match solve_fn(xnpv, guess) {
                    Some(r) => num(r),
                    None => Value::Err(ExcelError::Num),
                }
            }
            "CUMIPMT" | "CUMPRINC" => {
                if args.len() != 6 {
                    return Value::Err(ExcelError::Value);
                }
                let rate = try_num!(self.eval(&args[0]));
                let nper = try_num!(self.eval(&args[1]));
                let pv = try_num!(self.eval(&args[2]));
                let start = try_num!(self.eval(&args[3]));
                let end = try_num!(self.eval(&args[4]));
                let t = if try_num!(self.eval(&args[5])) != 0.0 {
                    1.0
                } else {
                    0.0
                };
                if rate <= 0.0
                    || nper <= 0.0
                    || pv <= 0.0
                    || start < 1.0
                    || end < start
                    || end > nper
                {
                    return Value::Err(ExcelError::Num);
                }
                let pmt = fin_pmt(rate, nper, pv, 0.0, t);
                let mut total = 0.0;
                for per in (start.trunc() as i64)..=(end.trunc() as i64) {
                    let ipmt = fin_ipmt(rate, per as f64, nper, pv, 0.0, t);
                    total += if name == "CUMIPMT" { ipmt } else { pmt - ipmt };
                }
                num(total)
            }

            // ---- database functions --------------------------------------------
            "DSUM" | "DAVERAGE" | "DMAX" | "DMIN" | "DPRODUCT" | "DCOUNT" | "DCOUNTA" | "DGET"
            | "DVAR" | "DVARP" | "DSTDEV" | "DSTDEVP" => {
                let cells = match self.db_query(args) {
                    Ok(c) => c,
                    Err(v) => return v,
                };
                if name == "DCOUNTA" {
                    return num(cells.iter().filter(|v| !matches!(v, Value::Empty)).count() as f64);
                }
                if name == "DGET" {
                    let non_blank: Vec<&Value> = cells
                        .iter()
                        .filter(|v| !matches!(v, Value::Empty))
                        .collect();
                    return match non_blank.len() {
                        0 => Value::Err(ExcelError::Value),
                        1 => non_blank[0].clone(),
                        _ => Value::Err(ExcelError::Num),
                    };
                }
                // Everything else works over the numeric field values.
                let nums: Vec<f64> = cells
                    .iter()
                    .filter_map(|v| match v {
                        Value::Num(n) => Some(*n),
                        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                        _ => None,
                    })
                    .collect();
                match name {
                    "DCOUNT" => num(nums.len() as f64),
                    "DSUM" => num(nums.iter().sum()),
                    "DPRODUCT" => {
                        if nums.is_empty() {
                            num(0.0)
                        } else {
                            num(nums.iter().product())
                        }
                    }
                    "DAVERAGE" => {
                        if nums.is_empty() {
                            Value::Err(ExcelError::Div0)
                        } else {
                            num(nums.iter().sum::<f64>() / nums.len() as f64)
                        }
                    }
                    "DMAX" => {
                        if nums.is_empty() {
                            num(0.0)
                        } else {
                            num(nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                        }
                    }
                    "DMIN" => {
                        if nums.is_empty() {
                            num(0.0)
                        } else {
                            num(nums.iter().cloned().fold(f64::INFINITY, f64::min))
                        }
                    }
                    _ => {
                        // DVAR/DVARP/DSTDEV/DSTDEVP.
                        let pop = name == "DVARP" || name == "DSTDEVP";
                        let min_n = if pop { 1 } else { 2 };
                        if nums.len() < min_n {
                            return Value::Err(ExcelError::Div0);
                        }
                        let m = nums.iter().sum::<f64>() / nums.len() as f64;
                        let ss: f64 = nums.iter().map(|x| (x - m) * (x - m)).sum();
                        let denom = if pop {
                            nums.len() as f64
                        } else {
                            nums.len() as f64 - 1.0
                        };
                        let var = ss / denom;
                        if name.starts_with("DSTDEV") {
                            num(var.sqrt())
                        } else {
                            num(var)
                        }
                    }
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
                // The result exceeds f64 range long before k is large; cap the
                // loop so a huge argument can't spin ~10^14 iterations.
                let iters = (k as u64).min(1_000_000);
                for i in 0..iters {
                    r = r * (n - i as f64) / (i as f64 + 1.0);
                    if !r.is_finite() {
                        return Value::Err(ExcelError::Num);
                    }
                }
                num(r.round())
            }),
            "PERMUT" => self.two_num(args, |n, k| {
                let (n, k) = (n.trunc(), k.trunc());
                if n < 0.0 || k < 0.0 || k > n {
                    return Value::Err(ExcelError::Num);
                }
                let mut r = 1.0f64;
                let iters = (k as u64).min(1_000_000);
                for i in 0..iters {
                    r *= n - i as f64;
                    if !r.is_finite() {
                        return Value::Err(ExcelError::Num);
                    }
                }
                num(r)
            }),
            "SUMSQ" => match self.collect_values(args, true) {
                Ok(v) => num(v.iter().map(|x| x * x).sum()),
                Err(e) => Value::Err(e),
            },
            "COMBINA" => self.two_num(args, |n, k| {
                // Combinations with repetition = C(n+k-1, k).
                let (n, k) = (n.trunc(), k.trunc());
                if n < 0.0 || k < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                if n == 0.0 {
                    return num(if k == 0.0 { 1.0 } else { 0.0 });
                }
                let top = n + k - 1.0;
                let kk = k.min(top - k);
                let iters = (kk as u64).min(1_000_000);
                let mut r = 1.0f64;
                for i in 0..iters {
                    r = r * (top - i as f64) / (i as f64 + 1.0);
                    if !r.is_finite() {
                        return Value::Err(ExcelError::Num);
                    }
                }
                num(r.round())
            }),
            "FACTDOUBLE" => self.one_num(args, |n| {
                let n = n.trunc();
                if n < -1.0 {
                    return Value::Err(ExcelError::Num);
                }
                let mut r = 1.0f64;
                let mut i = n;
                while i > 1.0 {
                    r *= i;
                    if !r.is_finite() {
                        return Value::Err(ExcelError::Num);
                    }
                    i -= 2.0;
                }
                num(r)
            }),

            // ---- regression & correlation --------------------------------------
            "CORREL" | "PEARSON" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let (_, _, _, sxx, syy, sxy) = match self.pair_stats(&args[0], &args[1]) {
                    Ok(s) => s,
                    Err(v) => return v,
                };
                if sxx == 0.0 || syy == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(sxy / (sxx * syy).sqrt())
            }
            "RSQ" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let (_, _, _, sxx, syy, sxy) = match self.pair_stats(&args[0], &args[1]) {
                    Ok(s) => s,
                    Err(v) => return v,
                };
                if sxx == 0.0 || syy == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                let r = sxy / (sxx * syy).sqrt();
                num(r * r)
            }
            "COVAR" | "COVARIANCE.P" | "COVARIANCE.S" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let (n, _, _, _, _, sxy) = match self.pair_stats(&args[0], &args[1]) {
                    Ok(s) => s,
                    Err(v) => return v,
                };
                if name == "COVARIANCE.S" {
                    if n < 2 {
                        return Value::Err(ExcelError::Div0);
                    }
                    num(sxy / (n as f64 - 1.0))
                } else {
                    num(sxy / n as f64)
                }
            }
            "SLOPE" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                // SLOPE(known_ys, known_xs): x is the second array.
                let (_, _, _, sxx, _, sxy) = match self.pair_stats(&args[1], &args[0]) {
                    Ok(s) => s,
                    Err(v) => return v,
                };
                if sxx == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(sxy / sxx)
            }
            "INTERCEPT" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let (_, mx, my, sxx, _, sxy) = match self.pair_stats(&args[1], &args[0]) {
                    Ok(s) => s,
                    Err(v) => return v,
                };
                if sxx == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(my - (sxy / sxx) * mx)
            }
            "FORECAST" | "FORECAST.LINEAR" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let x = try_num!(self.eval(&args[0]));
                let (_, mx, my, sxx, _, sxy) = match self.pair_stats(&args[2], &args[1]) {
                    Ok(s) => s,
                    Err(v) => return v,
                };
                if sxx == 0.0 {
                    return Value::Err(ExcelError::Div0);
                }
                num(my + (sxy / sxx) * (x - mx))
            }

            // ---- distribution shape & spread -----------------------------------
            "DEVSQ" => match self.collect_values(args, true) {
                Ok(v) if !v.is_empty() => {
                    let m = v.iter().sum::<f64>() / v.len() as f64;
                    num(v.iter().map(|x| (x - m) * (x - m)).sum())
                }
                Ok(_) => Value::Num(0.0),
                Err(e) => Value::Err(e),
            },
            "AVEDEV" => match self.collect_values(args, true) {
                Ok(v) if !v.is_empty() => {
                    let m = v.iter().sum::<f64>() / v.len() as f64;
                    num(v.iter().map(|x| (x - m).abs()).sum::<f64>() / v.len() as f64)
                }
                Ok(_) => Value::Err(ExcelError::Num),
                Err(e) => Value::Err(e),
            },
            "GEOMEAN" => match self.collect_values(args, true) {
                Ok(v) if !v.is_empty() => {
                    if v.iter().any(|&x| x <= 0.0) {
                        return Value::Err(ExcelError::Num);
                    }
                    num((v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp())
                }
                Ok(_) => Value::Err(ExcelError::Num),
                Err(e) => Value::Err(e),
            },
            "HARMEAN" => match self.collect_values(args, true) {
                Ok(v) if !v.is_empty() => {
                    if v.iter().any(|&x| x <= 0.0) {
                        return Value::Err(ExcelError::Num);
                    }
                    num(v.len() as f64 / v.iter().map(|x| 1.0 / x).sum::<f64>())
                }
                Ok(_) => Value::Err(ExcelError::Num),
                Err(e) => Value::Err(e),
            },
            "STANDARDIZE" => {
                if args.len() != 3 {
                    return Value::Err(ExcelError::Value);
                }
                let x = try_num!(self.eval(&args[0]));
                let mean = try_num!(self.eval(&args[1]));
                let sd = try_num!(self.eval(&args[2]));
                if sd <= 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((x - mean) / sd)
            }
            "TRIMMEAN" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let pct = try_num!(self.eval(&args[1]));
                if !(0.0..1.0).contains(&pct) {
                    return Value::Err(ExcelError::Num);
                }
                match self.collect_values(&args[..1], true) {
                    Ok(mut v) if !v.is_empty() => {
                        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        // Trim floor(n*pct) values total, split evenly (even count) each end.
                        let k = ((v.len() as f64 * pct) / 2.0).floor() as usize;
                        let slice = &v[k..v.len() - k];
                        if slice.is_empty() {
                            return Value::Err(ExcelError::Num);
                        }
                        num(slice.iter().sum::<f64>() / slice.len() as f64)
                    }
                    Ok(_) => Value::Err(ExcelError::Num),
                    Err(e) => Value::Err(e),
                }
            }
            "SKEW" | "KURT" => match self.collect_values(args, true) {
                Ok(v) => {
                    let n = v.len();
                    let min_n = if name == "SKEW" { 3 } else { 4 };
                    if n < min_n {
                        return Value::Err(ExcelError::Div0);
                    }
                    let nf = n as f64;
                    let m = v.iter().sum::<f64>() / nf;
                    let var = v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (nf - 1.0);
                    if var == 0.0 {
                        return Value::Err(ExcelError::Div0);
                    }
                    let sd = var.sqrt();
                    if name == "SKEW" {
                        let s: f64 = v.iter().map(|x| ((x - m) / sd).powi(3)).sum();
                        num(nf / ((nf - 1.0) * (nf - 2.0)) * s)
                    } else {
                        let s: f64 = v.iter().map(|x| ((x - m) / sd).powi(4)).sum();
                        let a = nf * (nf + 1.0) / ((nf - 1.0) * (nf - 2.0) * (nf - 3.0));
                        let b = 3.0 * (nf - 1.0) * (nf - 1.0) / ((nf - 2.0) * (nf - 3.0));
                        num(a * s - b)
                    }
                }
                Err(e) => Value::Err(e),
            },
            "FISHER" => self.one_num(args, |x| {
                if x <= -1.0 || x >= 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                num(0.5 * ((1.0 + x) / (1.0 - x)).ln())
            }),
            "FISHERINV" => self.one_num(args, |y| {
                let e = (2.0 * y).exp();
                num((e - 1.0) / (e + 1.0))
            }),
            "PERCENTRANK" | "PERCENTRANK.INC" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let x = try_num!(self.eval(&args[1]));
                let sig = match args.get(2) {
                    None | Some(Expr::Missing) => 3.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                if sig < 1.0 {
                    return Value::Err(ExcelError::Num);
                }
                match self.collect_values(&args[..1], true) {
                    Ok(mut v) if !v.is_empty() => {
                        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let (lo, hi) = (v[0], v[v.len() - 1]);
                        if x < lo || x > hi {
                            return Value::Err(ExcelError::NA);
                        }
                        let nm1 = (v.len() - 1) as f64;
                        let pr = if let Some(i) = v.iter().position(|&w| w == x) {
                            i as f64 / nm1
                        } else {
                            let i = v.iter().rposition(|&w| w < x).unwrap();
                            (i as f64 + (x - v[i]) / (v[i + 1] - v[i])) / nm1
                        };
                        let scale = 10f64.powi(sig as i32);
                        num((pr * scale).trunc() / scale)
                    }
                    Ok(_) => Value::Err(ExcelError::Num),
                    Err(e) => Value::Err(e),
                }
            }
            "PERCENTILE.EXC" | "QUARTILE.EXC" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let k = try_num!(self.eval(&args[1]));
                let k = if name == "QUARTILE.EXC" {
                    if k.fract() != 0.0 || !(1.0..=3.0).contains(&k) {
                        return Value::Err(ExcelError::Num);
                    }
                    k / 4.0
                } else {
                    k
                };
                match self.collect_values(&args[..1], true) {
                    Ok(mut v) if !v.is_empty() => {
                        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let n = v.len();
                        let pos = k * (n as f64 + 1.0);
                        if pos < 1.0 || pos > n as f64 {
                            return Value::Err(ExcelError::Num);
                        }
                        let lo = pos.floor() as usize; // 1-based
                        let frac = pos - lo as f64;
                        if lo >= n {
                            num(v[n - 1])
                        } else {
                            num(v[lo - 1] + frac * (v[lo] - v[lo - 1]))
                        }
                    }
                    Ok(_) => Value::Err(ExcelError::Num),
                    Err(e) => Value::Err(e),
                }
            }

            // ---- …A variants (text counts as 0) --------------------------------
            "AVERAGEA" => match self.collect_a(args) {
                Ok(v) if !v.is_empty() => num(v.iter().sum::<f64>() / v.len() as f64),
                Ok(_) => Value::Err(ExcelError::Div0),
                Err(e) => Value::Err(e),
            },
            "MAXA" => match self.collect_a(args) {
                Ok(v) if !v.is_empty() => num(v.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                Ok(_) => Value::Num(0.0),
                Err(e) => Value::Err(e),
            },
            "MINA" => match self.collect_a(args) {
                Ok(v) if !v.is_empty() => num(v.iter().cloned().fold(f64::INFINITY, f64::min)),
                Ok(_) => Value::Num(0.0),
                Err(e) => Value::Err(e),
            },
            "VARA" | "STDEVA" | "VARPA" | "STDEVPA" => match self.collect_a(args) {
                Ok(v) => {
                    let pop = name == "VARPA" || name == "STDEVPA";
                    let min_n = if pop { 1 } else { 2 };
                    if v.len() < min_n {
                        return Value::Err(ExcelError::Div0);
                    }
                    let m = v.iter().sum::<f64>() / v.len() as f64;
                    let ss: f64 = v.iter().map(|x| (x - m) * (x - m)).sum();
                    let denom = if pop {
                        v.len() as f64
                    } else {
                        v.len() as f64 - 1.0
                    };
                    let var = ss / denom;
                    if name.starts_with("STDEV") {
                        num(var.sqrt())
                    } else {
                        num(var)
                    }
                }
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
                // The real format-code runtime; formats it can't honestly
                // render mark the formula unsupported (keep the cache).
                let Some(fmt) = crate::numfmt::parse_format(&code) else {
                    self.unsupported = true;
                    return Value::Err(ExcelError::Value);
                };
                match &v {
                    Value::Str(s) => Value::Str(fmt.format_text(s)),
                    Value::Empty => Value::Str(fmt.format_text("")),
                    Value::Bool(b) => Value::Str(if *b { "TRUE" } else { "FALSE" }.to_string()),
                    Value::Num(n) => match fmt.format_number(*n, self.res.date1904()) {
                        Some(s) => Value::Str(s),
                        None => {
                            self.unsupported = true;
                            Value::Err(ExcelError::Value)
                        }
                    },
                    Value::Err(_) => unreachable!(),
                }
            }

            // ---- more text -------------------------------------------------
            "TEXTBEFORE" | "TEXTAFTER" => {
                if args.len() < 2 || args.len() > 6 {
                    return Value::Err(ExcelError::Value);
                }
                let text = try_text!(self.eval(&args[0]));
                let delims = match self.delim_list(args.get(1)) {
                    Ok(d) => d,
                    Err(v) => return v,
                };
                if delims.is_empty() {
                    return Value::Err(ExcelError::Value);
                }
                let instance = match args.get(2) {
                    None | Some(Expr::Missing) => 1i64,
                    Some(e) => try_num!(self.eval(e)).trunc() as i64,
                };
                let ci = match args.get(3) {
                    None | Some(Expr::Missing) => false,
                    Some(e) => try_num!(self.eval(e)).trunc() == 1.0,
                };
                let if_not_found = args.get(5);
                let not_found = |ev: &mut Self| -> Value {
                    match if_not_found {
                        Some(e) if !matches!(e, Expr::Missing) => ev.eval(e),
                        _ => Value::Err(ExcelError::NA),
                    }
                };
                // Char-index (start, end) of every delimiter occurrence.
                let chars: Vec<char> = text.chars().collect();
                let mut hits: Vec<(usize, usize)> = Vec::new();
                let mut i = 0;
                while i < chars.len() {
                    let m = delims
                        .iter()
                        .filter(|d| !d.is_empty())
                        .filter_map(|d| {
                            let dc: Vec<char> = d.chars().collect();
                            let fits = i + dc.len() <= chars.len()
                                && (0..dc.len()).all(|k| {
                                    if ci {
                                        chars[i + k].eq_ignore_ascii_case(&dc[k])
                                            || chars[i + k].to_lowercase().eq(dc[k].to_lowercase())
                                    } else {
                                        chars[i + k] == dc[k]
                                    }
                                });
                            fits.then_some(dc.len())
                        })
                        .max();
                    if let Some(len) = m {
                        hits.push((i, i + len));
                        i += len;
                    } else {
                        i += 1;
                    }
                }
                if hits.is_empty() {
                    return not_found(self);
                }
                let idx = if instance < 0 {
                    hits.len() as i64 + instance
                } else {
                    instance - 1
                };
                if idx < 0 || idx as usize >= hits.len() {
                    return not_found(self);
                }
                let (start, end) = hits[idx as usize];
                let slice: String = if name == "TEXTBEFORE" {
                    chars[..start].iter().collect()
                } else {
                    chars[end..].iter().collect()
                };
                Value::Str(slice)
            }
            "DOLLAR" | "FIXED" => {
                if args.is_empty() || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                let decimals = match args.get(1) {
                    None | Some(Expr::Missing) => 2.0,
                    Some(e) => try_num!(self.eval(e)),
                }
                .trunc() as i64;
                if name == "FIXED" {
                    let no_commas = match args.get(2) {
                        None | Some(Expr::Missing) => false,
                        Some(e) => try_bool!(self.eval(e)),
                    };
                    let mag = grouped_magnitude(n, decimals, !no_commas);
                    Value::Str(if n < 0.0 { format!("-{mag}") } else { mag })
                } else {
                    let mag = grouped_magnitude(n, decimals, true);
                    Value::Str(if n < 0.0 {
                        format!("(${mag})")
                    } else {
                        format!("${mag}")
                    })
                }
            }

            // ---- more lookup & info ----------------------------------------
            "XMATCH" => {
                if args.len() < 2 || args.len() > 4 {
                    return Value::Err(ExcelError::Value);
                }
                let needle = self.eval(&args[0]);
                let vals = match self.arg_vector(&args[1]) {
                    Ok(v) => v,
                    Err(v) => return v,
                };
                let match_mode = match args.get(2) {
                    None | Some(Expr::Missing) => 0i64,
                    Some(e) => try_num!(self.eval(e)).trunc() as i64,
                };
                let search_mode = match args.get(3) {
                    None | Some(Expr::Missing) => 1i64,
                    Some(e) => try_num!(self.eval(e)).trunc() as i64,
                };
                let n = vals.len();
                let order: Vec<usize> = if search_mode < 0 {
                    (0..n).rev().collect()
                } else {
                    (0..n).collect()
                };
                let mut exact: Option<usize> = None;
                let mut alt: Option<usize> = None;
                for &i in &order {
                    let v = &vals[i];
                    if matches!(v, Value::Empty) {
                        continue;
                    }
                    if match_mode == 2 {
                        if let (Value::Str(pat), Value::Str(t)) = (&needle, v) {
                            if wildcard_match(pat, t) {
                                exact = Some(i);
                                break;
                            }
                        }
                        continue;
                    }
                    match compare(v, &needle) {
                        Ok(std::cmp::Ordering::Equal) => {
                            exact = Some(i);
                            break;
                        }
                        // Largest value ≤ needle.
                        Ok(std::cmp::Ordering::Less)
                            if match_mode == -1
                                && alt.is_none_or(|j| {
                                    matches!(compare(v, &vals[j]), Ok(std::cmp::Ordering::Greater))
                                }) =>
                        {
                            alt = Some(i);
                        }
                        // Smallest value ≥ needle.
                        Ok(std::cmp::Ordering::Greater)
                            if match_mode == 1
                                && alt.is_none_or(|j| {
                                    matches!(compare(v, &vals[j]), Ok(std::cmp::Ordering::Less))
                                }) =>
                        {
                            alt = Some(i);
                        }
                        _ => {}
                    }
                }
                match exact.or(alt) {
                    Some(i) => Value::Num(i as f64 + 1.0),
                    None => Value::Err(ExcelError::NA),
                }
            }
            "ADDRESS" => {
                if args.len() < 2 || args.len() > 5 {
                    return Value::Err(ExcelError::Value);
                }
                let row = try_num!(self.eval(&args[0])).trunc() as i64;
                let col = try_num!(self.eval(&args[1])).trunc() as i64;
                let abs = match args.get(2) {
                    None | Some(Expr::Missing) => 1i64,
                    Some(e) => try_num!(self.eval(e)).trunc() as i64,
                };
                let a1 = match args.get(3) {
                    None | Some(Expr::Missing) => true,
                    Some(e) => try_bool!(self.eval(e)),
                };
                if row < 1 || col < 1 || !(1..=4).contains(&abs) {
                    return Value::Err(ExcelError::Value);
                }
                let core = if a1 {
                    let cn = col_name((col - 1) as u32);
                    match abs {
                        1 => format!("${cn}${row}"),
                        2 => format!("{cn}${row}"),
                        3 => format!("${cn}{row}"),
                        _ => format!("{cn}{row}"),
                    }
                } else {
                    match abs {
                        1 => format!("R{row}C{col}"),
                        2 => format!("R{row}C[{col}]"),
                        3 => format!("R[{row}]C{col}"),
                        _ => format!("R[{row}]C[{col}]"),
                    }
                };
                match args.get(4) {
                    None | Some(Expr::Missing) => Value::Str(core),
                    Some(e) => {
                        let sheet = try_text!(self.eval(e));
                        if sheet.is_empty() {
                            Value::Str(core)
                        } else if sheet.contains([' ', '\'']) {
                            Value::Str(format!("'{}'!{core}", sheet.replace('\'', "''")))
                        } else {
                            Value::Str(format!("{sheet}!{core}"))
                        }
                    }
                }
            }
            "ERROR.TYPE" => {
                if args.len() != 1 {
                    return Value::Err(ExcelError::Value);
                }
                match self.eval(&args[0]) {
                    Value::Err(e) => Value::Num(match e {
                        ExcelError::Null => 1.0,
                        ExcelError::Div0 => 2.0,
                        ExcelError::Value => 3.0,
                        ExcelError::Ref => 4.0,
                        ExcelError::Name => 5.0,
                        ExcelError::Num => 6.0,
                        ExcelError::NA => 7.0,
                        ExcelError::Spill => 9.0,
                        ExcelError::Calc => 14.0,
                        ExcelError::Cycle => 5.0,
                    }),
                    _ => Value::Err(ExcelError::NA),
                }
            }
            "TYPE" => {
                if args.len() != 1 {
                    return Value::Err(ExcelError::Value);
                }
                // An array argument reports 64; a single reference reports its
                // value's type.
                let code = match self.eval_arg(&args[0]) {
                    Arg::Matrix(_) => 64.0,
                    Arg::Range(s, r1, c1, r2, c2) if r1 != r2 || c1 != c2 => {
                        let _ = (s, c1, c2);
                        64.0
                    }
                    other => {
                        let v = match other {
                            Arg::Scalar(v) => v,
                            Arg::Range(s, r, c, ..) => self.res.value(s, r, c),
                            _ => Value::Err(ExcelError::Value),
                        };
                        match v {
                            Value::Num(_) | Value::Empty => 1.0,
                            Value::Str(_) => 2.0,
                            Value::Bool(_) => 4.0,
                            Value::Err(_) => 16.0,
                        }
                    }
                };
                Value::Num(code)
            }

            // ---- more math -------------------------------------------------
            "MROUND" => self.two_num(args, |n, m| {
                if m == 0.0 {
                    return Value::Num(0.0);
                }
                if n != 0.0 && (n < 0.0) != (m < 0.0) {
                    return Value::Err(ExcelError::Num);
                }
                num((n / m).round() * m)
            }),
            "SQRTPI" => self.one_num(args, |n| {
                if n < 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                num((n * std::f64::consts::PI).sqrt())
            }),
            "MDETERM" => {
                if args.len() != 1 {
                    return Value::Err(ExcelError::Value);
                }
                let m = match self.arg_matrix(&args[0]) {
                    Ok(m) => match to_num_matrix(&m) {
                        Ok(nm) => nm,
                        Err(v) => return v,
                    },
                    Err(v) => return v,
                };
                match matrix_det(&m) {
                    Some(d) => num(d),
                    None => Value::Err(ExcelError::Value),
                }
            }
            "ROMAN" => {
                // Optional form arg is accepted but ignored (classic form only).
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0]));
                match to_roman(n.trunc() as i64) {
                    Some(s) => Value::Str(s),
                    None => Value::Err(ExcelError::Value),
                }
            }
            "ARABIC" => self.one_text(args, |s| {
                if s.chars().count() > 255 {
                    return Value::Err(ExcelError::Value);
                }
                let (neg, body) = match s.trim().strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, s.trim()),
                };
                match from_roman(body) {
                    Some(v) => num(if neg { -v } else { v }),
                    None => Value::Err(ExcelError::Value),
                }
            }),
            "BASE" => {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Err(ExcelError::Value);
                }
                let n = try_num!(self.eval(&args[0])).trunc();
                let radix = try_num!(self.eval(&args[1])).trunc();
                if n < 0.0 || n >= 2f64.powi(53) || !(2.0..=36.0).contains(&radix) {
                    return Value::Err(ExcelError::Num);
                }
                let mut s = to_base(n as u64, radix as u32);
                if let Some(e) = args.get(2) {
                    let min_len = try_num!(self.eval(e)).trunc();
                    if !(0.0..=255.0).contains(&min_len) {
                        return Value::Err(ExcelError::Num);
                    }
                    let w = min_len as usize;
                    if s.len() < w {
                        s = format!("{s:0>w$}");
                    }
                }
                Value::Str(s)
            }
            "DECIMAL" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let s = try_text!(self.eval(&args[0]));
                let radix = try_num!(self.eval(&args[1])).trunc();
                if !(2.0..=36.0).contains(&radix) {
                    return Value::Err(ExcelError::Num);
                }
                match from_base(&s, radix as u32) {
                    Some(v) => num(v),
                    None => Value::Err(ExcelError::Num),
                }
            }
            "SUMX2MY2" | "SUMX2PY2" | "SUMXMY2" => {
                if args.len() != 2 {
                    return Value::Err(ExcelError::Value);
                }
                let (xs, ys) = match self.two_arrays(&args[0], &args[1]) {
                    Ok(p) => p,
                    Err(v) => return v,
                };
                let total: f64 = xs
                    .iter()
                    .zip(ys.iter())
                    .map(|(&x, &y)| match name {
                        "SUMX2MY2" => x * x - y * y,
                        "SUMX2PY2" => x * x + y * y,
                        _ => (x - y) * (x - y),
                    })
                    .sum();
                num(total)
            }
            "MULTINOMIAL" => match self.collect_values(args, true) {
                Ok(vals) => {
                    let mut sum = 0.0f64;
                    let mut denom = 1.0f64;
                    for v in &vals {
                        if *v < 0.0 {
                            return Value::Err(ExcelError::Num);
                        }
                        let k = v.trunc();
                        sum += k;
                        for i in 2..=(k as u64) {
                            denom *= i as f64;
                        }
                    }
                    if sum > 170.0 {
                        return Value::Err(ExcelError::Num);
                    }
                    let mut numer = 1.0f64;
                    for i in 2..=(sum as u64) {
                        numer *= i as f64;
                    }
                    num(numer / denom)
                }
                Err(e) => Value::Err(e),
            },
            "SERIESSUM" => {
                if args.len() != 4 {
                    return Value::Err(ExcelError::Value);
                }
                let x = try_num!(self.eval(&args[0]));
                let n = try_num!(self.eval(&args[1]));
                let m = try_num!(self.eval(&args[2]));
                let coeffs = match self.collect_values(&args[3..], true) {
                    Ok(c) => c,
                    Err(e) => return Value::Err(e),
                };
                let mut total = 0.0f64;
                for (i, &c) in coeffs.iter().enumerate() {
                    total += c * x.powf(n + i as f64 * m);
                }
                num(total)
            }

            // ---- engineering: bit ops & base conversion --------------------
            "BITAND" | "BITOR" | "BITXOR" => self.two_num(args, |a, b| {
                if a < 0.0 || b < 0.0 || a.fract() != 0.0 || b.fract() != 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                let lim = 2f64.powi(48);
                if a >= lim || b >= lim {
                    return Value::Err(ExcelError::Num);
                }
                let (x, y) = (a as u64, b as u64);
                num(match name {
                    "BITAND" => x & y,
                    "BITOR" => x | y,
                    _ => x ^ y,
                } as f64)
            }),
            "BITLSHIFT" | "BITRSHIFT" => self.two_num(args, |a, shift| {
                if a < 0.0 || a.fract() != 0.0 || a >= 2f64.powi(48) {
                    return Value::Err(ExcelError::Num);
                }
                if shift.abs() > 53.0 || shift.fract() != 0.0 {
                    return Value::Err(ExcelError::Num);
                }
                let sh = if name == "BITRSHIFT" { -shift } else { shift };
                let r = (a * 2f64.powi(sh as i32)).floor();
                if r >= 2f64.powi(53) {
                    return Value::Err(ExcelError::Num);
                }
                num(r)
            }),
            "DEC2BIN" => self.dec_base_call(args, 2, -512.0, 511.0),
            "DEC2OCT" => self.dec_base_call(args, 8, -536_870_912.0, 536_870_911.0),
            "DEC2HEX" => self.dec_base_call(args, 16, -549_755_813_888.0, 549_755_813_887.0),
            "BIN2DEC" => self.one_text(args, |s| base_to_dec(&s, 2, 10)),
            "OCT2DEC" => self.one_text(args, |s| base_to_dec(&s, 8, 10)),
            "HEX2DEC" => self.one_text(args, |s| base_to_dec(&s, 16, 10)),
            "BIN2OCT" => self.cross_base_call(args, 2, 10, 8, -536_870_912.0, 536_870_911.0),
            "BIN2HEX" => {
                self.cross_base_call(args, 2, 10, 16, -549_755_813_888.0, 549_755_813_887.0)
            }
            "OCT2BIN" => self.cross_base_call(args, 8, 10, 2, -512.0, 511.0),
            "OCT2HEX" => {
                self.cross_base_call(args, 8, 10, 16, -549_755_813_888.0, 549_755_813_887.0)
            }
            "HEX2BIN" => self.cross_base_call(args, 16, 10, 2, -512.0, 511.0),
            "HEX2OCT" => self.cross_base_call(args, 16, 10, 8, -536_870_912.0, 536_870_911.0),
            "DELTA" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let a = try_num!(self.eval(&args[0]));
                let b = match args.get(1) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                num(if a == b { 1.0 } else { 0.0 })
            }
            "GESTEP" => {
                if args.is_empty() || args.len() > 2 {
                    return Value::Err(ExcelError::Value);
                }
                let a = try_num!(self.eval(&args[0]));
                let step = match args.get(1) {
                    None | Some(Expr::Missing) => 0.0,
                    Some(e) => try_num!(self.eval(e)),
                };
                num(if a >= step { 1.0 } else { 0.0 })
            }

            // ---- unknown ---------------------------------------------------
            _ => match self.stat_call(name, args) {
                Some(v) => v,
                None => {
                    self.unsupported = true;
                    Value::Err(ExcelError::Name)
                }
            },
        }
    }

    /// Evaluate every argument to a number (bools coerce: TRUE→1, FALSE→0).
    fn stat_nums(&mut self, args: &[Expr]) -> Result<Vec<f64>, ExcelError> {
        let mut v = Vec::with_capacity(args.len());
        for a in args {
            if matches!(a, Expr::Missing) {
                return Err(ExcelError::Value);
            }
            v.push(to_num(&self.eval(a))?);
        }
        Ok(v)
    }

    /// The Excel statistical-distribution family (NORM/GAMMA/CHISQ/BETA/F/T/
    /// BINOM/POISSON/…, plus the legacy pre-2010 spellings). Returns `None` when
    /// `name` isn't one of them, so the caller can report `#NAME?`.
    fn stat_call(&mut self, name: &str, args: &[Expr]) -> Option<Value> {
        use crate::stats as st;
        if !is_stat_fn(name) {
            return None;
        }
        let n = match self.stat_nums(args) {
            Ok(v) => v,
            Err(e) => return Some(Value::Err(e)),
        };
        // Arity guard: `[lo, hi]` inclusive count of numeric args.
        macro_rules! arity {
            ($lo:expr, $hi:expr) => {
                if n.len() < $lo || n.len() > $hi {
                    return Some(Value::Err(ExcelError::Value));
                }
            };
        }
        let cum = |i: usize| n.get(i).map(|x| *x != 0.0).unwrap_or(false);
        // Positive standard-t quantile for q ≥ 0.5.
        let inv_t = |p: f64, df: f64| -> Option<f64> {
            if p <= 0.0 || p >= 1.0 {
                None
            } else if p == 0.5 {
                Some(0.0)
            } else if p > 0.5 {
                st::invert_cdf(p, 0.0, 1.0, |x| st::t_cdf(x, df))
            } else {
                st::invert_cdf(1.0 - p, 0.0, 1.0, |x| st::t_cdf(x, df)).map(|x| -x)
            }
        };
        let r: Value = match name {
            // ---- normal ----
            "NORM.DIST" | "NORMDIST" => {
                arity!(4, 4);
                if n[2] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                let z = (n[0] - n[1]) / n[2];
                domo(Some(if cum(3) {
                    st::norm_cdf(z)
                } else {
                    st::norm_pdf(z) / n[2]
                }))
            }
            "NORM.S.DIST" => {
                arity!(2, 2);
                domo(Some(if cum(1) {
                    st::norm_cdf(n[0])
                } else {
                    st::norm_pdf(n[0])
                }))
            }
            "NORMSDIST" => {
                arity!(1, 1);
                domo(Some(st::norm_cdf(n[0])))
            }
            "NORM.INV" | "NORMINV" => {
                arity!(3, 3);
                if n[2] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(st::norm_inv(n[0]).map(|z| n[1] + n[2] * z))
            }
            "NORM.S.INV" | "NORMSINV" => {
                arity!(1, 1);
                domo(st::norm_inv(n[0]))
            }
            "PHI" => {
                arity!(1, 1);
                domo(Some(st::norm_pdf(n[0])))
            }
            "GAUSS" => {
                arity!(1, 1);
                domo(Some(st::norm_cdf(n[0]) - 0.5))
            }
            "STANDARDIZE" => {
                arity!(3, 3);
                if n[2] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some((n[0] - n[1]) / n[2]))
            }
            "FISHER" => {
                arity!(1, 1);
                if n[0] <= -1.0 || n[0] >= 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(n[0].atanh()))
            }
            "FISHERINV" => {
                arity!(1, 1);
                domo(Some(n[0].tanh()))
            }
            "CONFIDENCE" | "CONFIDENCE.NORM" => {
                arity!(3, 3);
                if n[0] <= 0.0 || n[0] >= 1.0 || n[1] <= 0.0 || n[2] < 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(st::norm_inv(1.0 - n[0] / 2.0).map(|z| z * n[1] / n[2].sqrt()))
            }
            "CONFIDENCE.T" => {
                arity!(3, 3);
                if n[0] <= 0.0 || n[0] >= 1.0 || n[1] <= 0.0 || n[2] < 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(inv_t(1.0 - n[0] / 2.0, n[2] - 1.0).map(|t| t * n[1] / n[2].sqrt()))
            }

            // ---- gamma / chi-square ----
            "GAMMALN" | "GAMMALN.PRECISE" => {
                arity!(1, 1);
                if n[0] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(st::lgamma(n[0])))
            }
            "GAMMA" => {
                arity!(1, 1);
                domo(st::gamma(n[0]))
            }
            "GAMMA.DIST" | "GAMMADIST" => {
                arity!(4, 4);
                domo(st::gamma_dist(n[0], n[1], n[2], cum(3)))
            }
            "GAMMA.INV" | "GAMMAINV" => {
                arity!(3, 3);
                if n[1] <= 0.0 || n[2] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(st::invert_cdf(n[0], 0.0, 1.0, |x| st::gammp(n[1], x / n[2])))
            }
            "CHISQ.DIST" => {
                arity!(3, 3);
                domo(st::gamma_dist(n[0], n[1] / 2.0, 2.0, cum(2)))
            }
            "CHISQ.DIST.RT" | "CHIDIST" => {
                arity!(2, 2);
                if n[0] < 0.0 || n[1] < 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(st::gammq(n[1] / 2.0, n[0] / 2.0)))
            }
            "CHISQ.INV" => {
                arity!(2, 2);
                domo(st::invert_cdf(n[0], 0.0, 1.0, |x| st::gammp(n[1] / 2.0, x / 2.0)))
            }
            "CHISQ.INV.RT" | "CHIINV" => {
                arity!(2, 2);
                domo(st::invert_cdf(1.0 - n[0], 0.0, 1.0, |x| {
                    st::gammp(n[1] / 2.0, x / 2.0)
                }))
            }

            // ---- exponential / poisson / weibull ----
            "EXPON.DIST" | "EXPONDIST" => {
                arity!(3, 3);
                if n[0] < 0.0 || n[1] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(if cum(2) {
                    1.0 - (-n[1] * n[0]).exp()
                } else {
                    n[1] * (-n[1] * n[0]).exp()
                }))
            }
            "POISSON.DIST" | "POISSON" => {
                arity!(3, 3);
                let k = n[0].floor();
                if k < 0.0 || n[1] < 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(if cum(2) {
                    st::gammq(k + 1.0, n[1])
                } else {
                    (-n[1] + k * n[1].ln() - st::lgamma(k + 1.0)).exp()
                }))
            }
            "WEIBULL.DIST" | "WEIBULL" => {
                arity!(4, 4);
                if n[0] < 0.0 || n[1] <= 0.0 || n[2] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                let (x, a, b) = (n[0], n[1], n[2]);
                domo(Some(if cum(3) {
                    1.0 - (-(x / b).powf(a)).exp()
                } else {
                    (a / b) * (x / b).powf(a - 1.0) * (-(x / b).powf(a)).exp()
                }))
            }

            // ---- lognormal ----
            "LOGNORM.DIST" => {
                arity!(4, 4);
                domo(lognorm(n[0], n[1], n[2], cum(3)))
            }
            "LOGNORMDIST" => {
                arity!(3, 3);
                domo(lognorm(n[0], n[1], n[2], true))
            }
            "LOGNORM.INV" | "LOGINV" => {
                arity!(3, 3);
                if n[2] <= 0.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(st::norm_inv(n[0]).map(|z| (n[1] + n[2] * z).exp()))
            }

            // ---- binomial / negative binomial / hypergeometric ----
            "BINOM.DIST" | "BINOMDIST" => {
                arity!(4, 4);
                domo(binom(n[0], n[1], n[2], cum(3)))
            }
            "BINOM.INV" | "CRITBINOM" => {
                arity!(3, 3);
                let (trials, p, alpha) = (n[0].floor(), n[1], n[2]);
                if trials < 0.0 || !(0.0..=1.0).contains(&p) || !(0.0..=1.0).contains(&alpha) {
                    return Some(Value::Err(ExcelError::Num));
                }
                let mut cumv = 0.0;
                let mut out = trials;
                let mut k = 0.0;
                while k <= trials {
                    cumv += binom(k, trials, p, false).unwrap_or(0.0);
                    if cumv >= alpha {
                        out = k;
                        break;
                    }
                    k += 1.0;
                }
                num(out)
            }
            "NEGBINOM.DIST" | "NEGBINOMDIST" => {
                arity!(3, 4);
                let (f, s, p) = (n[0].floor(), n[1].floor(), n[2]);
                if f < 0.0 || s < 1.0 || !(0.0..=1.0).contains(&p) {
                    return Some(Value::Err(ExcelError::Num));
                }
                let cumulative = n.len() == 4 && cum(3);
                domo(Some(if cumulative {
                    st::betai(s, f + 1.0, p)
                } else {
                    (st::lgamma(f + s) - st::lgamma(f + 1.0) - st::lgamma(s)
                        + s * p.ln()
                        + f * (1.0 - p).ln())
                    .exp()
                }))
            }
            "HYPGEOM.DIST" => {
                arity!(5, 5);
                domo(hypgeom(n[0], n[1], n[2], n[3], cum(4)))
            }
            "HYPGEOMDIST" => {
                arity!(4, 4);
                domo(hypgeom(n[0], n[1], n[2], n[3], false))
            }

            // ---- beta ----
            "BETA.DIST" => {
                arity!(4, 6);
                let (lo, hi) = (n.get(4).copied().unwrap_or(0.0), n.get(5).copied().unwrap_or(1.0));
                domo(st::beta_dist(n[0], n[1], n[2], cum(3), lo, hi))
            }
            "BETADIST" => {
                arity!(3, 5);
                let (lo, hi) = (n.get(3).copied().unwrap_or(0.0), n.get(4).copied().unwrap_or(1.0));
                domo(st::beta_dist(n[0], n[1], n[2], true, lo, hi))
            }
            "BETA.INV" | "BETAINV" => {
                arity!(3, 5);
                let (lo, hi) = (n.get(3).copied().unwrap_or(0.0), n.get(4).copied().unwrap_or(1.0));
                if n[1] <= 0.0 || n[2] <= 0.0 || hi <= lo {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(
                    st::invert_cdf(n[0], 0.0, 1.0, |z| st::betai(n[1], n[2], z))
                        .map(|z| lo + z * (hi - lo)),
                )
            }

            // ---- F ----
            "F.DIST" => {
                arity!(4, 4);
                domo(f_dist(n[0], n[1], n[2], cum(3)))
            }
            "F.DIST.RT" | "FDIST" => {
                arity!(3, 3);
                if n[0] < 0.0 || n[1] < 1.0 || n[2] < 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(1.0 - st::f_cdf(n[0], n[1], n[2])))
            }
            "F.INV" => {
                arity!(3, 3);
                domo(st::invert_cdf(n[0], 0.0, 1.0, |x| st::f_cdf(x, n[1], n[2])))
            }
            "F.INV.RT" | "FINV" => {
                arity!(3, 3);
                domo(st::invert_cdf(1.0 - n[0], 0.0, 1.0, |x| st::f_cdf(x, n[1], n[2])))
            }

            // ---- Student t ----
            "T.DIST" => {
                arity!(3, 3);
                domo(t_dist(n[0], n[1], cum(2)))
            }
            "T.DIST.RT" => {
                arity!(2, 2);
                if n[1] < 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(1.0 - st::t_cdf(n[0], n[1])))
            }
            "T.DIST.2T" => {
                arity!(2, 2);
                if n[0] < 0.0 || n[1] < 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(Some(2.0 * (1.0 - st::t_cdf(n[0], n[1]))))
            }
            "TDIST" => {
                arity!(3, 3);
                let tails = n[2];
                if n[0] < 0.0 || n[1] < 1.0 || (tails != 1.0 && tails != 2.0) {
                    return Some(Value::Err(ExcelError::Num));
                }
                let rt = 1.0 - st::t_cdf(n[0], n[1]);
                domo(Some(if tails == 2.0 { 2.0 * rt } else { rt }))
            }
            "T.INV" => {
                arity!(2, 2);
                domo(inv_t(n[0], n[1]))
            }
            "T.INV.2T" | "TINV" => {
                arity!(2, 2);
                // Two-tailed: the positive x with 2·(1−F(x)) = p.
                if n[0] <= 0.0 || n[0] > 1.0 {
                    return Some(Value::Err(ExcelError::Num));
                }
                domo(inv_t(1.0 - n[0] / 2.0, n[1]))
            }

            _ => return None,
        };
        Some(r)
    }

    /// Collect numeric values (and a COUNTA count of non-empty entries) for
    /// SUBTOTAL/AGGREGATE: skip cells that are themselves nested
    /// SUBTOTAL/AGGREGATE, optionally skip hidden rows, and either propagate or
    /// ignore error values.
    fn collect_subtotal(
        &mut self,
        args: &[Expr],
        ignore_hidden: bool,
        ignore_errors: bool,
    ) -> Result<(Vec<f64>, usize), ExcelError> {
        let mut nums = Vec::new();
        let mut counta = 0usize;
        for a in args {
            match self.eval_arg(a) {
                Arg::Scalar(Value::Err(e)) => {
                    if !ignore_errors {
                        return Err(e);
                    }
                }
                Arg::Scalar(Value::Empty) => {}
                Arg::Scalar(v) => {
                    counta += 1;
                    if let Value::Num(n) = v {
                        nums.push(n);
                    }
                }
                Arg::Range(s, r1, c1, r2, c2) => {
                    for ((r, c), v) in self.res.cells_in(s, r1, c1, r2, c2) {
                        if ignore_hidden && self.res.row_hidden(s, r) {
                            continue;
                        }
                        if self.is_nested_subtotal(s, r, c) {
                            continue;
                        }
                        match v {
                            Value::Err(e) => {
                                if !ignore_errors {
                                    return Err(e);
                                }
                            }
                            Value::Num(n) => {
                                counta += 1;
                                nums.push(n);
                            }
                            Value::Empty => {}
                            _ => counta += 1,
                        }
                    }
                }
                Arg::Matrix(m) => {
                    for v in m.iter().flatten() {
                        match v {
                            Value::Err(e) => {
                                if !ignore_errors {
                                    return Err(*e);
                                }
                            }
                            Value::Num(n) => {
                                counta += 1;
                                nums.push(*n);
                            }
                            Value::Empty => {}
                            _ => counta += 1,
                        }
                    }
                }
                Arg::Lambda(_) => return Err(ExcelError::Calc),
            }
        }
        Ok((nums, counta))
    }

    /// Does the cell hold a SUBTOTAL/AGGREGATE formula (which an enclosing
    /// SUBTOTAL/AGGREGATE must skip)?
    fn is_nested_subtotal(&self, sheet: usize, row: u32, col: u32) -> bool {
        match self.res.cell_formula(sheet, row, col) {
            Some(f) => {
                let t = f.trim_start();
                let t = t
                    .strip_prefix("_xlfn.")
                    .or_else(|| t.strip_prefix("_XLFN."))
                    .unwrap_or(t);
                let up = t.to_ascii_uppercase();
                up.starts_with("SUBTOTAL(") || up.starts_with("AGGREGATE(")
            }
            None => false,
        }
    }

    /// Apply a SUBTOTAL/AGGREGATE aggregate code (1–13) to collected values.
    fn apply_agg(&self, code: i64, nums: &[f64], counta: usize) -> Value {
        let n = nums.len();
        let sum: f64 = nums.iter().sum();
        let mean = if n > 0 { sum / n as f64 } else { 0.0 };
        let var = |ddof: usize| -> Option<f64> {
            if n <= ddof {
                return None;
            }
            let ss: f64 = nums.iter().map(|x| (x - mean) * (x - mean)).sum();
            Some(ss / (n - ddof) as f64)
        };
        match code {
            1 => {
                if n == 0 {
                    Value::Err(ExcelError::Div0)
                } else {
                    num(mean)
                }
            }
            2 => Value::Num(n as f64),
            3 => Value::Num(counta as f64),
            4 => {
                if n == 0 {
                    Value::Num(0.0)
                } else {
                    num(nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                }
            }
            5 => {
                if n == 0 {
                    Value::Num(0.0)
                } else {
                    num(nums.iter().cloned().fold(f64::INFINITY, f64::min))
                }
            }
            6 => num(if n == 0 { 0.0 } else { nums.iter().product() }),
            7 => match var(1) {
                Some(v) => num(v.sqrt()),
                None => Value::Err(ExcelError::Div0),
            },
            8 => match var(0) {
                Some(v) => num(v.sqrt()),
                None => Value::Err(ExcelError::Div0),
            },
            9 => num(sum),
            10 => var(1).map(num).unwrap_or(Value::Err(ExcelError::Div0)),
            11 => var(0).map(num).unwrap_or(Value::Err(ExcelError::Div0)),
            12 => {
                if n == 0 {
                    return Value::Err(ExcelError::Num);
                }
                let mut s = nums.to_vec();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let mid = n / 2;
                num(if n % 2 == 1 {
                    s[mid]
                } else {
                    (s[mid - 1] + s[mid]) / 2.0
                })
            }
            13 => {
                // MODE.SNGL: most frequent value (first-seen wins ties); #N/A if
                // every value is unique.
                let mut counts: Vec<(f64, usize)> = Vec::new();
                for &x in nums {
                    if let Some(e) = counts.iter_mut().find(|(v, _)| *v == x) {
                        e.1 += 1;
                    } else {
                        counts.push((x, 1));
                    }
                }
                let mut chosen: Option<f64> = None;
                let mut best_c = 1usize;
                for (v, c) in &counts {
                    if *c > best_c {
                        best_c = *c;
                        chosen = Some(*v);
                    }
                }
                chosen.map(num).unwrap_or(Value::Err(ExcelError::NA))
            }
            _ => Value::Err(ExcelError::Value),
        }
    }

    /// AGGREGATE functions 14–19 (LARGE/SMALL/PERCENTILE/QUARTILE) with a k arg.
    fn apply_agg_k(&self, func: i64, nums: &[f64], k: f64) -> Value {
        let n = nums.len();
        if n == 0 {
            return Value::Err(ExcelError::Num);
        }
        let mut s = nums.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pctl_inc = |p: f64| -> Value {
            if !(0.0..=1.0).contains(&p) {
                return Value::Err(ExcelError::Num);
            }
            let rank = p * (n as f64 - 1.0);
            let lo = rank.floor() as usize;
            let frac = rank - lo as f64;
            if lo + 1 < n {
                num(s[lo] + frac * (s[lo + 1] - s[lo]))
            } else {
                num(s[lo])
            }
        };
        let pctl_exc = |p: f64| -> Value {
            let rank = p * (n as f64 + 1.0) - 1.0;
            if rank < 0.0 || rank > (n as f64 - 1.0) {
                return Value::Err(ExcelError::Num);
            }
            let lo = rank.floor() as usize;
            let frac = rank - lo as f64;
            if lo + 1 < n {
                num(s[lo] + frac * (s[lo + 1] - s[lo]))
            } else {
                num(s[lo])
            }
        };
        match func {
            14 => {
                let kk = k.trunc() as i64;
                if kk < 1 || kk as usize > n {
                    return Value::Err(ExcelError::Num);
                }
                num(s[n - kk as usize])
            }
            15 => {
                let kk = k.trunc() as i64;
                if kk < 1 || kk as usize > n {
                    return Value::Err(ExcelError::Num);
                }
                num(s[kk as usize - 1])
            }
            16 => pctl_inc(k),
            17 => {
                let q = k.trunc() as i64;
                if !(0..=4).contains(&q) {
                    return Value::Err(ExcelError::Num);
                }
                pctl_inc(q as f64 / 4.0)
            }
            18 => pctl_exc(k),
            19 => {
                let q = k.trunc() as i64;
                if !(1..=3).contains(&q) {
                    return Value::Err(ExcelError::Num);
                }
                pctl_exc(q as f64 / 4.0)
            }
            _ => Value::Err(ExcelError::Value),
        }
    }

    /// Resolve a reference expression to the (sheet, row, col) of its top-left
    /// cell *structurally* — without evaluating the target, so it works on
    /// array/spill anchors. `None` if it isn't a plain reference.
    fn ref_coords(&self, e: &Expr) -> Option<(usize, u32, u32)> {
        let (sheet_name, row, col) = match e {
            Expr::Ref(r) | Expr::SpillRef(r) => (r.sheet.as_deref(), r.row, r.col),
            Expr::Range(a, b) => (a.sheet.as_deref(), a.row.min(b.row), a.col.min(b.col)),
            _ => return None,
        };
        if row < 0 || col < 0 {
            return None;
        }
        let sheet = match sheet_name {
            Some(n) => self.res.sheet_index(n)?,
            None => self.sheet,
        };
        Some((sheet, row as u32, col as u32))
    }

    /// CELL(info_type, [reference]) — the common info types. Anything needing
    /// styling or workbook metadata we don't model marks the result unsupported
    /// so the engine keeps Excel's cached value.
    fn cell_info(&mut self, args: &[Expr]) -> Value {
        if args.is_empty() || args.len() > 2 {
            return Value::Err(ExcelError::Value);
        }
        let info = try_text!(self.eval(&args[0])).to_ascii_lowercase();
        let (s, r, c) = if args.len() == 2 {
            match self.ref_coords(&args[1]) {
                Some(rc) => rc,
                None => return Value::Err(ExcelError::Value),
            }
        } else {
            (self.sheet, self.cell.0, self.cell.1)
        };
        match info.as_str() {
            "row" => Value::Num((r + 1) as f64),
            "col" => Value::Num((c + 1) as f64),
            "address" => Value::Str(format!("${}${}", col_name(c), r + 1)),
            "contents" => self.res.value(s, r, c),
            "type" => Value::Str(
                match self.res.value(s, r, c) {
                    Value::Empty => "b",
                    Value::Str(_) => "l",
                    _ => "v",
                }
                .to_string(),
            ),
            "prefix" => Value::Str(String::new()),
            _ => {
                self.unsupported = true;
                Value::Err(ExcelError::NA)
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

    /// DEC2BIN/OCT/HEX dispatch: number plus an optional `places` width.
    fn dec_base_call(&mut self, args: &[Expr], radix: u32, lo: f64, hi: f64) -> Value {
        if args.is_empty() || args.len() > 2 {
            return Value::Err(ExcelError::Value);
        }
        let n = try_num!(self.eval(&args[0]));
        let places = match args.get(1) {
            None | Some(Expr::Missing) => None,
            Some(e) => Some(try_num!(self.eval(e))),
        };
        dec_to_base(n, radix, places, lo, hi)
    }

    /// BIN2HEX / OCT2BIN / … — parse in one base, re-render two's-complement in
    /// another, honoring the destination's valid range and optional `places`.
    fn cross_base_call(
        &mut self,
        args: &[Expr],
        from_radix: u32,
        from_digits: u32,
        to_radix: u32,
        lo: f64,
        hi: f64,
    ) -> Value {
        if args.is_empty() || args.len() > 2 {
            return Value::Err(ExcelError::Value);
        }
        let s = try_text!(self.eval(&args[0]));
        let dec = match base_to_dec(&s, from_radix, from_digits) {
            Value::Num(n) => n,
            other => return other,
        };
        let places = match args.get(1) {
            None | Some(Expr::Missing) => None,
            Some(e) => Some(try_num!(self.eval(e))),
        };
        dec_to_base(dec, to_radix, places, lo, hi)
    }

    /// Flatten any argument to a dense, row-major `Vec<Value>` (blanks kept as
    /// `Value::Empty`), so paired-array functions can align by position.
    fn flat_dense(&mut self, e: &Expr) -> Result<Vec<Value>, Value> {
        match self.eval_arg(e) {
            Arg::Scalar(Value::Err(er)) => Err(Value::Err(er)),
            Arg::Scalar(v) => Ok(vec![v]),
            Arg::Range(s, r1, c1, r2, c2) => {
                let (a, b, c, d) = self.clamp(s, r1, c1, r2, c2);
                let mut out = Vec::new();
                for r in a..=c {
                    for col in b..=d {
                        out.push(self.res.value(s, r, col));
                    }
                }
                Ok(out)
            }
            Arg::Matrix(m) => Ok(m.into_iter().flatten().collect()),
            Arg::Lambda(_) => Err(Value::Err(ExcelError::Calc)),
        }
    }

    /// A 2-D dense grid of an argument (row-major), clamped to the used range.
    /// Scalars become a 1×1 grid; the shape database functions want.
    fn flat_grid(&mut self, e: &Expr) -> Result<Vec<Vec<Value>>, Value> {
        match self.eval_arg(e) {
            Arg::Scalar(Value::Err(er)) => Err(Value::Err(er)),
            Arg::Scalar(v) => Ok(vec![vec![v]]),
            Arg::Range(s, r1, c1, r2, c2) => {
                let (a, b, c, d) = self.clamp(s, r1, c1, r2, c2);
                Ok((a..=c)
                    .map(|r| (b..=d).map(|col| self.res.value(s, r, col)).collect())
                    .collect())
            }
            Arg::Matrix(m) => Ok(m),
            Arg::Lambda(_) => Err(Value::Err(ExcelError::Calc)),
        }
    }

    /// Shared core of the D-functions: the `field` column's values over every
    /// database row that satisfies the criteria range.
    /// `args` = [database, field, criteria].
    fn db_query(&mut self, args: &[Expr]) -> Result<Vec<Value>, Value> {
        if args.len() != 3 {
            return Err(Value::Err(ExcelError::Value));
        }
        let db = self.flat_grid(&args[0])?;
        let field = self.eval(&args[1]);
        let crit = self.flat_grid(&args[2])?;
        if db.len() < 2 || db[0].is_empty() || crit.is_empty() {
            return Err(Value::Err(ExcelError::Value));
        }
        let headers = &db[0];
        let text_eq = |a: &Value, b: &str| {
            to_text(a)
                .map(|t| t.eq_ignore_ascii_case(b))
                .unwrap_or(false)
        };
        // Resolve the field to a column index (1-based number, or header name).
        let col = match &field {
            Value::Num(n) => {
                let i = n.trunc() as i64 - 1;
                if i < 0 || i as usize >= headers.len() {
                    return Err(Value::Err(ExcelError::Value));
                }
                i as usize
            }
            _ => {
                let name = to_text(&field).map_err(Value::Err)?;
                match headers.iter().position(|h| text_eq(h, &name)) {
                    Some(i) => i,
                    None => return Err(Value::Err(ExcelError::Value)),
                }
            }
        };
        // Map each criteria column to a database column via its header text.
        let crit_headers = &crit[0];
        let crit_cols: Vec<Option<usize>> = crit_headers
            .iter()
            .map(|ch| {
                to_text(ch)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .and_then(|s| headers.iter().position(|h| text_eq(h, &s)))
            })
            .collect();

        let mut out = Vec::new();
        for row in &db[1..] {
            // Criteria rows are OR'd; cells within a row are AND'd.
            let mut matched = crit.len() <= 1; // no criteria rows → all match
            for cr in &crit[1..] {
                let mut row_ok = true;
                for (ci, dbcol) in crit_cols.iter().enumerate() {
                    let Some(dbcol) = dbcol else { continue };
                    let cval = cr.get(ci).cloned().unwrap_or(Value::Empty);
                    if matches!(cval, Value::Empty) {
                        continue;
                    }
                    let c = parse_criteria(&cval);
                    let cell = row.get(*dbcol).cloned().unwrap_or(Value::Empty);
                    if !criteria_match(&c, &cell) {
                        row_ok = false;
                        break;
                    }
                }
                if row_ok {
                    matched = true;
                    break;
                }
            }
            if matched {
                out.push(row.get(col).cloned().unwrap_or(Value::Empty));
            }
        }
        Ok(out)
    }

    /// Two equal-shaped arrays reduced to the numeric pairs they share
    /// (positions where either side is non-numeric are dropped). Mismatched
    /// lengths yield `#N/A`, matching Excel's paired-array functions.
    fn two_arrays(&mut self, a: &Expr, b: &Expr) -> Result<(Vec<f64>, Vec<f64>), Value> {
        let xa = self.flat_dense(a)?;
        let xb = self.flat_dense(b)?;
        if xa.len() != xb.len() {
            return Err(Value::Err(ExcelError::NA));
        }
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for (u, v) in xa.iter().zip(xb.iter()) {
            if let (Value::Num(x), Value::Num(y)) = (u, v) {
                xs.push(*x);
                ys.push(*y);
            }
        }
        Ok((xs, ys))
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Render a non-negative integer in `radix` (2..=36) using upper-case digits.
fn to_base(mut n: u64, radix: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut s = Vec::new();
    while n > 0 {
        s.push(DIGITS[(n % radix as u64) as usize]);
        n /= radix as u64;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

/// Parse a string of base-`radix` digits into a value (case-insensitive).
fn from_base(s: &str, radix: u32) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut acc: f64 = 0.0;
    for c in s.chars() {
        let d = c.to_digit(radix)?;
        acc = acc * radix as f64 + d as f64;
    }
    Some(acc)
}

/// Integer 1..=3999 → classic (subtractive) Roman numerals.
fn to_roman(mut n: i64) -> Option<String> {
    if !(1..=3999).contains(&n) {
        return None;
    }
    const TABLE: &[(i64, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for &(v, sym) in TABLE {
        while n >= v {
            s.push_str(sym);
            n -= v;
        }
    }
    Some(s)
}

/// Roman numerals (subtractive) → integer; empty string is 0.
fn from_roman(s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_uppercase();
    if s.is_empty() {
        return Some(0.0);
    }
    let val = |c: char| -> i64 {
        match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => 0,
        }
    };
    let mut total = 0i64;
    let mut prev = 0i64;
    for c in s.chars().rev() {
        let v = val(c);
        if v == 0 {
            return None;
        }
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    Some(total as f64)
}

/// DEC2BIN/OCT/HEX: two's-complement render into `radix` over 10 digits.
fn dec_to_base(n: f64, radix: u32, places: Option<f64>, lo: f64, hi: f64) -> Value {
    let n = n.trunc();
    if !(lo..=hi).contains(&n) {
        return Value::Err(ExcelError::Num);
    }
    if n < 0.0 {
        // 10-digit two's complement; `places` is ignored for negatives.
        let m = (radix as i128).pow(10) + n as i128;
        return Value::Str(to_base(m as u64, radix));
    }
    let mut s = to_base(n as u64, radix);
    if let Some(p) = places {
        let p = p.trunc();
        if !(1.0..=10.0).contains(&p) {
            return Value::Err(ExcelError::Num);
        }
        let p = p as usize;
        if s.len() > p {
            return Value::Err(ExcelError::Num);
        }
        s = format!("{s:0>p$}");
    }
    Value::Str(s)
}

/// BIN2DEC/OCT2DEC/HEX2DEC: signed two's-complement over `digits` positions.
fn base_to_dec(s: &str, radix: u32, digits: u32) -> Value {
    let s = s.trim();
    if s.is_empty() {
        return Value::Num(0.0);
    }
    if s.chars().count() as u32 > digits {
        return Value::Err(ExcelError::Num);
    }
    let mut acc: i128 = 0;
    for c in s.chars() {
        match c.to_digit(radix) {
            Some(d) => acc = acc * radix as i128 + d as i128,
            None => return Value::Err(ExcelError::Num),
        }
    }
    let full = (radix as i128).pow(digits);
    let out = if acc >= full / 2 { acc - full } else { acc };
    Value::Num(out as f64)
}

/// Split `s` at every occurrence of any delimiter in `delims` (longest match
/// wins at each position). Used by TEXTSPLIT / TEXTBEFORE / TEXTAFTER.
fn split_on_any(s: &str, delims: &[String], ci: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let bytes: Vec<char> = s.chars().collect();
    let matches_at = |pos: usize, d: &str| -> bool {
        let dc: Vec<char> = d.chars().collect();
        if pos + dc.len() > bytes.len() {
            return false;
        }
        (0..dc.len()).all(|k| {
            let (a, b) = (bytes[pos + k], dc[k]);
            if ci {
                a.eq_ignore_ascii_case(&b) || a.to_lowercase().eq(b.to_lowercase())
            } else {
                a == b
            }
        })
    };
    let mut i = 0;
    while i < bytes.len() {
        // Prefer the longest delimiter that matches here.
        let hit = delims
            .iter()
            .filter(|d| !d.is_empty() && matches_at(i, d))
            .max_by_key(|d| d.chars().count());
        if let Some(d) = hit {
            out.push(std::mem::take(&mut cur));
            i += d.chars().count();
        } else {
            cur.push(bytes[i]);
            i += 1;
        }
    }
    out.push(cur);
    out
}

/// Group an integer digit string with thousands commas: "1234567" → "1,234,567".
fn group_thousands(digits: &str) -> String {
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

/// Magnitude of `n` rounded to `decimals` places (may be negative), optionally
/// with thousands separators. Sign and currency are added by the caller.
fn grouped_magnitude(n: f64, decimals: i64, commas: bool) -> String {
    let x = n.abs();
    let (rounded, frac_digits) = if decimals < 0 {
        let f = 10f64.powi((-decimals) as i32);
        ((x / f).round() * f, 0usize)
    } else {
        (x, decimals as usize)
    };
    let s = format!("{rounded:.frac_digits$}");
    let (int_part, frac_part) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s.as_str(), None),
    };
    let mut out = if commas {
        group_thousands(int_part)
    } else {
        int_part.to_string()
    };
    if let Some(fp) = frac_part {
        out.push('.');
        out.push_str(fp);
    }
    out
}

/// Month index (1..=12) from a 3+ letter English name prefix.
fn month_num(s: &str) -> Option<u32> {
    const M: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let s = s.trim().to_ascii_lowercase();
    M.iter()
        .position(|m| s.starts_with(m))
        .map(|i| i as u32 + 1)
}

/// Two-digit year → four-digit, Excel's 0-29 → 2000s, 30-99 → 1900s rule.
fn norm_year(y: i64) -> i64 {
    if y < 30 {
        2000 + y
    } else if y < 100 {
        1900 + y
    } else {
        y
    }
}

/// Best-effort DATEVALUE: ISO, US M/D/Y, and month-name forms. Returns the
/// date serial in the workbook's date system, or `None` if unrecognized.
fn parse_date_text(s: &str, d1904: bool) -> Option<f64> {
    let parts: Vec<&str> = s
        .trim()
        .split(['/', '-', ' ', ','])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let (mut y, mut d) = (None, None);
    let mo;
    if let Some(idx) = parts.iter().position(|p| month_num(p).is_some()) {
        mo = month_num(parts[idx]);
        for (i, p) in parts.iter().enumerate() {
            if i == idx {
                continue;
            }
            let v: i64 = p.parse().ok()?;
            if v > 31 || p.len() == 4 {
                y = Some(v);
            } else {
                d = Some(v);
            }
        }
        y?; // a year is required (no clock for "current year")
        d = d.or(Some(1));
    } else {
        // All numeric.
        let nums: Vec<i64> = parts
            .iter()
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?;
        if nums.len() != 3 {
            return None;
        }
        if parts[0].len() == 4 {
            y = Some(nums[0]);
            mo = Some(nums[1] as u32);
            d = Some(nums[2]);
        } else {
            mo = Some(nums[0] as u32);
            d = Some(nums[1]);
            y = Some(nums[2]);
        }
    }
    let y = norm_year(y?);
    let mo = mo?;
    let d = d? as u32;
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) {
        return None;
    }
    let serial = parts_to_serial(y, mo, d, 0, d1904);
    if serial < 0.0 { None } else { Some(serial) }
}

/// Best-effort TIMEVALUE: "H:M", "H:M:S", optional AM/PM. Returns a day
/// fraction in [0, 1), or `None`.
fn parse_time_text(s: &str) -> Option<f64> {
    let mut body = s.trim().to_ascii_lowercase();
    let mut pm = None;
    for (suf, is_pm) in [("am", false), ("pm", true), ("a", false), ("p", true)] {
        if let Some(rest) = body.strip_suffix(suf) {
            pm = Some(is_pm);
            body = rest.trim().to_string();
            break;
        }
    }
    let parts: Vec<&str> = body.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut h: i64 = parts[0].trim().parse().ok()?;
    let m: i64 = parts.get(1).map_or(Ok(0), |p| p.trim().parse()).ok()?;
    let sec: f64 = parts.get(2).map_or(Ok(0.0), |p| p.trim().parse()).ok()?;
    if let Some(is_pm) = pm {
        if !(1..=12).contains(&h) {
            return None;
        }
        h %= 12;
        if is_pm {
            h += 12;
        }
    }
    if !(0..24).contains(&h) || !(0..60).contains(&m) || !(0.0..60.0).contains(&sec) {
        return None;
    }
    Some((h as f64 * 3600.0 + m as f64 * 60.0 + sec) / 86_400.0)
}

/// Monday-indexed weekday (Mon=0 … Sun=6) of a workbook serial.
fn weekday_mon0(serial: i64, d1904: bool) -> usize {
    let s1900 = serial + if d1904 { 1462 } else { 0 };
    ((s1900 + 5).rem_euclid(7)) as usize
}

/// Is `serial` a non-working day under `mask` (Mon..Sun weekend flags)?
fn is_weekend(serial: i64, d1904: bool, mask: &[bool; 7]) -> bool {
    mask[weekday_mon0(serial, d1904)]
}

/// Parse a WORKDAY.INTL/NETWORKDAYS.INTL weekend argument: a numeric code
/// (1-7, 11-17) or a 7-char "0/1" string (Mon..Sun). `None` if invalid.
fn weekend_mask(v: &Value) -> Option<[bool; 7]> {
    if let Value::Str(s) = v {
        if s.len() == 7 && s.chars().all(|c| c == '0' || c == '1') {
            let mut m = [false; 7];
            for (i, c) in s.chars().enumerate() {
                m[i] = c == '1';
            }
            return Some(m);
        }
    }
    let code = to_num(v).ok()?.trunc() as i64;
    let mut m = [false; 7];
    match code {
        1 => {
            m[5] = true;
            m[6] = true;
        }
        2 => {
            m[6] = true;
            m[0] = true;
        }
        3 => {
            m[0] = true;
            m[1] = true;
        }
        4 => {
            m[1] = true;
            m[2] = true;
        }
        5 => {
            m[2] = true;
            m[3] = true;
        }
        6 => {
            m[3] = true;
            m[4] = true;
        }
        7 => {
            m[4] = true;
            m[5] = true;
        }
        11..=17 => m[(code - 11) as usize] = true,
        _ => return None,
    }
    Some(m)
}

/// 30/360 day count between two dates. `european` toggles the day-of-31 rule.
fn days_360(y1: i64, m1: u32, mut d1: u32, y2: i64, m2: u32, mut d2: u32, european: bool) -> i64 {
    if european {
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 {
            d2 = 30;
        }
    } else {
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 && d1 == 30 {
            d2 = 30;
        }
    }
    (y2 - y1) * 360 + (m2 as i64 - m1 as i64) * 30 + (d2 as i64 - d1 as i64)
}

/// Convert a value matrix to `f64`s, or a `#VALUE!` on any non-number.
fn to_num_matrix(m: &Matrix) -> Result<Vec<Vec<f64>>, Value> {
    let mut out = Vec::with_capacity(m.len());
    for row in m {
        let mut r = Vec::with_capacity(row.len());
        for v in row {
            r.push(to_num(v).map_err(Value::Err)?);
        }
        out.push(r);
    }
    Ok(out)
}

/// Determinant of a square matrix via LU with partial pivoting.
fn matrix_det(a: &[Vec<f64>]) -> Option<f64> {
    let n = a.len();
    if n == 0 || a.iter().any(|r| r.len() != n) {
        return None;
    }
    let mut m: Vec<Vec<f64>> = a.to_vec();
    let mut det = 1.0f64;
    for col in 0..n {
        // Partial pivot.
        let mut piv = col;
        for r in col + 1..n {
            if m[r][col].abs() > m[piv][col].abs() {
                piv = r;
            }
        }
        if m[piv][col] == 0.0 {
            return Some(0.0);
        }
        if piv != col {
            m.swap(piv, col);
            det = -det;
        }
        det *= m[col][col];
        for r in col + 1..n {
            let f = m[r][col] / m[col][col];
            for c in col..n {
                m[r][c] -= f * m[col][c];
            }
        }
    }
    Some(det)
}

/// Inverse of a square matrix via Gauss-Jordan; `None` if singular.
fn matrix_inverse(a: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    if n == 0 || a.iter().any(|r| r.len() != n) {
        return None;
    }
    // Augment [A | I].
    let mut m: Vec<Vec<f64>> = a
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r = row.clone();
            r.extend((0..n).map(|j| if i == j { 1.0 } else { 0.0 }));
            r
        })
        .collect();
    for col in 0..n {
        let mut piv = col;
        for r in col + 1..n {
            if m[r][col].abs() > m[piv][col].abs() {
                piv = r;
            }
        }
        if m[piv][col].abs() < 1e-300 {
            return None;
        }
        m.swap(piv, col);
        let d = m[col][col];
        for c in 0..2 * n {
            m[col][c] /= d;
        }
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = m[r][col];
            for c in 0..2 * n {
                m[r][c] -= f * m[col][c];
            }
        }
    }
    Some(m.iter().map(|row| row[n..].to_vec()).collect())
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

/// Find a root of `f` (rate ≥ -1) via Newton with a bisection fallback.
/// Used by XIRR, whose objective isn't the plain cash-flow polynomial.
fn solve_fn(f: impl Fn(f64) -> f64, guess: f64) -> Option<f64> {
    let mut r = guess.max(-0.99);
    for _ in 0..80 {
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
        if (next - r).abs() < 1e-10 {
            return Some(next);
        }
        r = next;
    }
    let (mut lo, mut hi) = (-0.999_999, 1e6);
    let (flo, fhi) = (f(lo), f(hi));
    if !flo.is_finite() || !fhi.is_finite() || flo * fhi > 0.0 {
        return None;
    }
    for _ in 0..300 {
        let mid = (lo + hi) / 2.0;
        let fm = f(mid);
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

/// Payment per period (Excel PMT sign convention).
fn fin_pmt(rate: f64, nper: f64, pv: f64, fv: f64, t: f64) -> f64 {
    if rate == 0.0 {
        -(pv + fv) / nper
    } else {
        let f = (1.0 + rate).powf(nper);
        -(fv + pv * f) * rate / ((f - 1.0) * (1.0 + rate * t))
    }
}

/// Future value (Excel FV sign convention).
fn fin_fv(rate: f64, nper: f64, pmt: f64, pv: f64, t: f64) -> f64 {
    if rate == 0.0 {
        -(pv + pmt * nper)
    } else {
        let f = (1.0 + rate).powf(nper);
        -(pv * f + pmt * (1.0 + rate * t) * (f - 1.0) / rate)
    }
}

/// Interest portion of the `per`-th payment (Excel IPMT).
fn fin_ipmt(rate: f64, per: f64, nper: f64, pv: f64, fv: f64, t: f64) -> f64 {
    let pmt = fin_pmt(rate, nper, pv, fv, t);
    let temp = if per == 1.0 {
        if t == 1.0 { 0.0 } else { -pv }
    } else if t == 1.0 {
        fin_fv(rate, per - 2.0, pmt, pv, 1.0) - pmt
    } else {
        fin_fv(rate, per - 1.0, pmt, pv, 0.0)
    };
    temp * rate
}

/// Fixed-declining-balance depreciation for one period (Excel DB).
fn fin_db(cost: f64, salvage: f64, life: f64, period: f64, month: f64) -> Option<f64> {
    if cost < 0.0 || salvage < 0.0 || life <= 0.0 || period < 1.0 || !(1.0..=12.0).contains(&month)
    {
        return None;
    }
    let rate = if cost == 0.0 {
        0.0
    } else {
        1.0 - (salvage / cost).powf(1.0 / life)
    };
    let rate = (rate * 1000.0).round() / 1000.0;
    let first = cost * rate * month / 12.0;
    let p = period.trunc() as i64;
    if p == 1 {
        return Some(first);
    }
    let life_i = life.trunc() as i64;
    let mut total = first;
    let mut dep = first;
    for i in 2..=p {
        dep = if i == life_i + 1 {
            (cost - total) * rate * (12.0 - month) / 12.0
        } else {
            (cost - total) * rate
        };
        total += dep;
    }
    Some(dep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test resolver over a single sheet of literal values.
    struct Grid {
        cells: HashMap<(u32, u32), Value>,
        names: HashMap<String, String>,
        table: Option<TableInfo>,
        formulas: HashMap<(u32, u32), String>,
        hidden: std::collections::HashSet<u32>,
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
                table: None,
                formulas: HashMap::new(),
                hidden: std::collections::HashSet::new(),
            }
        }
        fn with_name(mut self, name: &str, def: &str) -> Grid {
            self.names.insert(name.to_uppercase(), def.to_string());
            self
        }
        fn with_table(mut self, info: TableInfo) -> Grid {
            self.table = Some(info);
            self
        }
        /// Record a cell's source formula text (without the leading `=`).
        fn with_formula(mut self, name: &str, src: &str) -> Grid {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            self.formulas.insert((r, c), src.to_string());
            self
        }
        /// Mark a 1-based worksheet row hidden.
        fn with_hidden(mut self, row_1based: u32) -> Grid {
            self.hidden.insert(row_1based - 1);
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
        fn table(&self, name: &str) -> Option<TableInfo> {
            self.table
                .clone()
                .filter(|_| name.eq_ignore_ascii_case("Sales"))
        }
        fn table_at(&self, sheet: usize, row: u32, col: u32) -> Option<TableInfo> {
            self.table.clone().filter(|t| {
                sheet == t.sheet
                    && row >= t.range.0
                    && row <= t.range.2
                    && col >= t.range.1
                    && col <= t.range.3
            })
        }
        fn cell_formula(&self, _sheet: usize, row: u32, col: u32) -> Option<String> {
            self.formulas.get(&(row, col)).cloned()
        }
        fn row_hidden(&self, _sheet: usize, row: u32) -> bool {
            self.hidden.contains(&row)
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
    fn match_lifts_over_array_first_arg() {
        let g = Grid::new(&[
            ("A1", Value::Str("x".into())),
            ("A2", Value::Str("y".into())),
            ("A3", Value::Str("x".into())),
        ]);
        // MATCH(range, range, 0) → each value's first-occurrence position {1;2;1}.
        assert_eq!(n("SUM(MATCH(A1:A3,A1:A3,0))", &g), 4.0);
        assert_eq!(n("INDEX(MATCH(A1:A3,A1:A3,0),2)", &g), 2.0);
        assert_eq!(n("INDEX(MATCH(A1:A3,A1:A3,0),3)", &g), 1.0);
        // A scalar first argument still returns a scalar (unchanged path).
        assert_eq!(n("MATCH(\"y\",A1:A3,0)", &g), 2.0);
    }

    #[test]
    fn builtin_wins_over_same_named_defined_lambda() {
        // A workbook defined name `SUM = LAMBDA(x, x+10)` must NOT shadow the
        // builtin SUM (Excel keeps builtins); a non-builtin name still resolves
        // to its defined lambda.
        struct R;
        impl Resolver for R {
            fn value(&self, _: usize, _: u32, _: u32) -> Value {
                Value::Empty
            }
            fn cells_in(&self, _: usize, _: u32, _: u32, _: u32, _: u32) -> Vec<((u32, u32), Value)> {
                Vec::new()
            }
            fn sheet_index(&self, _: &str) -> Option<usize> {
                None
            }
            fn defined_name(&self, name: &str, _: usize) -> Option<String> {
                match name {
                    "SUM" => Some("LAMBDA(x, x+10)".into()),
                    "PLUS2" => Some("LAMBDA(x, x+2)".into()),
                    _ => None,
                }
            }
        }
        let r = R;
        let mut ev = Eval::new(&r, 0, (0, 0));
        // Builtin SUM aggregates, not the defined lambda (which would give 6+10).
        assert_eq!(ev.eval(&parse("SUM(1,2,3)").unwrap()), Value::Num(6.0));
        // A non-builtin defined lambda is still callable.
        assert_eq!(ev.eval(&parse("PLUS2(5)").unwrap()), Value::Num(7.0));
    }

    #[test]
    fn if_with_scalar_selector_keeps_array_branch() {
        let g = Grid::new(&[
            ("A1", Value::Num(2.0)),
            ("A2", Value::Num(4.0)),
            ("A3", Value::Num(6.0)),
        ]);
        // IF(TRUE, A1:A3, …) returns the array branch (spills), not a collapse.
        let ast = parse("IF(TRUE, A1:A3, A1:A1)").unwrap();
        let mut ev = Eval::new(&g, 0, (10, 0));
        match ev.eval_dynamic_as(&ast, true) {
            DynResult::Array(m) => {
                assert_eq!(m.len(), 3);
                assert_eq!(m[0][0], Value::Num(2.0));
            }
            DynResult::Scalar(v) => panic!("expected a 3-row array, got scalar {v:?}"),
        }
        // The false branch is chosen likewise.
        let ast = parse("IF(FALSE, A1:A1, A1:A3)").unwrap();
        let mut ev = Eval::new(&g, 0, (10, 0));
        assert!(matches!(ev.eval_dynamic_as(&ast, true), DynResult::Array(m) if m.len() == 3));
    }

    #[test]
    fn formulatext_strips_internal_prefixes() {
        // FORMULATEXT shows the display form Excel does.
        assert_eq!(display_formula("_xlfn._xlws.SORT(A2:A5)"), "SORT(A2:A5)");
        assert_eq!(display_formula("_xlfn.ANCHORARRAY(B2)"), "B2#");
        assert_eq!(
            display_formula("_xlfn.UNIQUE(_xlfn.ANCHORARRAY(B2))"),
            "UNIQUE(B2#)"
        );
        assert_eq!(display_formula("SUM(_xlfn.ANCHORARRAY(B2))"), "SUM(B2#)");
        assert_eq!(
            display_formula("_xlfn.LAMBDA(_xlpm.a, 2)(1)"),
            "LAMBDA(a, 2)(1)"
        );
        assert_eq!(display_formula("_xlfn.SINGLE(A1:A3)"), "@A1:A3");
    }

    #[test]
    fn frequency_counts_into_bins() {
        let mut cells: Vec<(&str, Value)> = Vec::new();
        let names: Vec<String> = (1..=10).map(|i| format!("A{i}")).collect();
        for (i, nm) in names.iter().enumerate() {
            cells.push((nm.as_str(), Value::Num((i + 1) as f64)));
        }
        cells.push(("B1", Value::Num(3.0)));
        cells.push(("B2", Value::Num(6.0)));
        cells.push(("B3", Value::Num(9.0)));
        let g = Grid::new(&cells);
        // Bins {3,6,9} split 1..10 into {≤3, (3,6], (6,9], >9} = {3,3,3,1}.
        assert_eq!(n("SUM(FREQUENCY(A1:A10,B1:B3))", &g), 10.0);
        assert_eq!(n("INDEX(FREQUENCY(A1:A10,B1:B3),1)", &g), 3.0);
        assert_eq!(n("INDEX(FREQUENCY(A1:A10,B1:B3),2)", &g), 3.0);
        assert_eq!(n("INDEX(FREQUENCY(A1:A10,B1:B3),4)", &g), 1.0);
        // A whole-column data array clamps to the used range (same answer).
        assert_eq!(n("SUM(FREQUENCY(A:A,B1:B3))", &g), 10.0);
        assert_eq!(n("INDEX(FREQUENCY(A:A,B1:B3),4)", &g), 1.0);
    }

    #[test]
    fn statistical_distributions_match_excel() {
        let g = empty();
        let close = |src: &str, want: f64| {
            let got = n(src, &g);
            assert!(
                (got - want).abs() <= 1e-6 * (1.0 + want.abs()),
                "{src} = {got}, want {want}"
            );
        };
        // Normal.
        close("NORM.S.DIST(1.96,TRUE)", 0.975_002_104_851_780);
        close("NORM.DIST(0,0,1,TRUE)", 0.5);
        close("NORM.INV(0.975,0,1)", 1.959_963_984_540_054);
        close("NORMSDIST(0)", 0.5);
        close("STANDARDIZE(12,10,2)", 1.0);
        // Gamma / chi-square (3.8415 is the 1-df 5% χ² critical value).
        close("GAMMALN(5)", 24.0f64.ln());
        close("CHISQ.DIST.RT(3.8414588,1)", 0.05);
        close("CHISQ.INV.RT(0.05,1)", 3.841_458_8);
        close("GAMMA.DIST(2,1,1,TRUE)", 1.0 - (-2.0f64).exp());
        // Binomial (n=10, p=0.5): P(X≤3)=176/1024, P(X=3)=120/1024.
        close("BINOM.DIST(3,10,0.5,TRUE)", 176.0 / 1024.0);
        close("BINOM.DIST(3,10,0.5,FALSE)", 120.0 / 1024.0);
        close("BINOM.INV(10,0.5,0.5)", 5.0);
        // Poisson, exponential, Weibull.
        close("POISSON.DIST(2,3,TRUE)", 0.423_190_081);
        close("EXPON.DIST(1,1,TRUE)", 1.0 - (-1.0f64).exp());
        close("WEIBULL.DIST(1,1,1,TRUE)", 1.0 - (-1.0f64).exp());
        // Student-t.
        close("T.INV.2T(0.05,10)", 2.228_138_852);
        close("T.DIST.2T(2.228138852,10)", 0.05);
        // Confidence + lognormal round-trip.
        close("CONFIDENCE.NORM(0.05,1,100)", 0.195_996_398);
        close("LOGNORM.INV(0.5,0,1)", 1.0);
        // F distribution inverse round-trips its right tail.
        close("F.DIST.RT(F.INV.RT(0.1,5,10),5,10)", 0.1);
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
    fn ceiling_floor_precise_and_math() {
        let g = empty();
        // .PRECISE / ISO.CEILING: multiple of |sig|, toward ±inf, sign-agnostic.
        assert_eq!(n("CEILING.PRECISE(4.1,2)", &g), 6.0);
        assert_eq!(n("CEILING.PRECISE(-4.1,2)", &g), -4.0);
        assert_eq!(n("CEILING.PRECISE(-4.1,-2)", &g), -4.0); // sig sign ignored
        assert_eq!(n("ISO.CEILING(4.1)", &g), 5.0); // default sig 1
        assert_eq!(n("FLOOR.PRECISE(4.1,2)", &g), 4.0);
        assert_eq!(n("FLOOR.PRECISE(-4.1,2)", &g), -6.0);
        // .MATH: optional significance (default 1) and mode.
        assert_eq!(n("CEILING.MATH(24)", &g), 24.0);
        assert_eq!(n("CEILING.MATH(23.1)", &g), 24.0);
        assert_eq!(n("CEILING.MATH(-5.5,2)", &g), -4.0); // mode 0: toward zero
        assert_eq!(n("CEILING.MATH(-5.5,2,-1)", &g), -6.0); // mode≠0: away
        assert_eq!(n("FLOOR.MATH(-5.5,2)", &g), -6.0); // mode 0: away from zero
        assert_eq!(n("FLOOR.MATH(-5.5,2,-1)", &g), -4.0); // mode≠0: toward zero
    }

    #[test]
    fn subtotal_basic_nested_and_hidden() {
        let g = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("A2", Value::Num(2.0)),
            ("A3", Value::Num(3.0)),
            ("A4", Value::Num(4.0)),
        ]);
        assert_eq!(n("SUBTOTAL(9,A1:A4)", &g), 10.0); // SUM
        assert_eq!(n("SUBTOTAL(1,A1:A4)", &g), 2.5); // AVERAGE
        assert_eq!(n("SUBTOTAL(2,A1:A4)", &g), 4.0); // COUNT
        assert_eq!(n("SUBTOTAL(4,A1:A4)", &g), 4.0); // MAX
        assert_eq!(n("SUBTOTAL(5,A1:A4)", &g), 1.0); // MIN

        // A nested SUBTOTAL cell inside the range is excluded (A3 is itself one).
        let g2 = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("A2", Value::Num(2.0)),
            ("A3", Value::Num(3.0)),
            ("A4", Value::Num(4.0)),
        ])
        .with_formula("A3", "SUBTOTAL(9,A1:A2)");
        assert_eq!(n("SUBTOTAL(9,A1:A4)", &g2), 7.0); // 1+2+4, A3 skipped

        // Hidden rows are excluded (filter-hidden is the common case).
        let g3 = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("A2", Value::Num(2.0)),
            ("A3", Value::Num(3.0)),
            ("A4", Value::Num(4.0)),
        ])
        .with_hidden(2)
        .with_hidden(4);
        assert_eq!(n("SUBTOTAL(9,A1:A4)", &g3), 4.0); // 1+3
    }

    #[test]
    fn aggregate_functions() {
        let g = Grid::new(&[
            ("A1", Value::Num(10.0)),
            ("A2", Value::Err(ExcelError::Div0)),
            ("A3", Value::Num(30.0)),
            ("A4", Value::Num(20.0)),
        ]);
        // Option 6 ignores errors → SUM = 60.
        assert_eq!(n("AGGREGATE(9,6,A1:A4)", &g), 60.0);
        // Option 0 does not ignore errors → the #DIV/0! propagates.
        assert_eq!(
            eval_str("AGGREGATE(9,0,A1:A4)", &g),
            Value::Err(ExcelError::Div0)
        );
        assert_eq!(n("AGGREGATE(14,6,A1:A4,1)", &g), 30.0); // LARGE, k=1
        assert_eq!(n("AGGREGATE(15,6,A1:A4,1)", &g), 10.0); // SMALL, k=1
        assert_eq!(n("AGGREGATE(4,6,A1:A4)", &g), 30.0); // MAX
        assert_eq!(n("AGGREGATE(1,6,A1:A4)", &g), 20.0); // AVERAGE (10+30+20)/3
    }

    #[test]
    fn formulatext_and_cell() {
        let g = Grid::new(&[("A1", Value::Num(42.0)), ("B2", Value::Str("hi".into()))])
            .with_formula("A1", "1+2*3");
        assert_eq!(eval_str("FORMULATEXT(A1)", &g), Value::Str("=1+2*3".into()));
        assert_eq!(eval_str("FORMULATEXT(B2)", &g), Value::Err(ExcelError::NA));
        assert_eq!(n("CELL(\"row\",B2)", &g), 2.0);
        assert_eq!(n("CELL(\"col\",B2)", &g), 2.0);
        assert_eq!(
            eval_str("CELL(\"address\",B2)", &g),
            Value::Str("$B$2".into())
        );
        assert_eq!(eval_str("CELL(\"contents\",A1)", &g), Value::Num(42.0));
        assert_eq!(eval_str("CELL(\"type\",B2)", &g), Value::Str("l".into()));
        assert_eq!(eval_str("CELL(\"type\",A1)", &g), Value::Str("v".into()));
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
        let ast = parse("PIVOTBY(3)").unwrap();
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
        // The runtime renders sections, conditions, and literal codes…
        assert_eq!(
            eval_str("TEXT(-1234,\"$#,##0;[Red]($#,##0)\")", &g),
            Value::Str("($1,234)".into())
        );
        assert_eq!(
            eval_str("TEXT(1234,\"\"\"kg\"\"\")", &g),
            Value::Str("kg".into())
        );
        assert_eq!(
            eval_str("TEXT(45306.25,\"dddd h:mm AM/PM\")", &g),
            Value::Str("Monday 6:00 AM".into())
        );
        // …and refuses what it can't honestly do (fractions).
        let ast = parse("TEXT(1234,\"# ?/?\")").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        let _ = ev.eval(&ast);
        assert!(ev.unsupported);
    }

    fn sales_table() -> TableInfo {
        // A1:C5 — header row 1, data rows 2-4, totals row 5.
        TableInfo {
            sheet: 0,
            range: (0, 0, 4, 2),
            header_rows: 1,
            totals_rows: 1,
            columns: vec!["Item".into(), "Qty".into(), "Price".into()],
        }
    }

    fn sales_grid() -> Grid {
        Grid::new(&[
            ("A1", Value::Str("Item".into())),
            ("B1", Value::Str("Qty".into())),
            ("C1", Value::Str("Price".into())),
            ("A2", Value::Str("pen".into())),
            ("B2", Value::Num(3.0)),
            ("C2", Value::Num(1.5)),
            ("A3", Value::Str("pad".into())),
            ("B3", Value::Num(2.0)),
            ("C3", Value::Num(4.0)),
            ("A4", Value::Str("ink".into())),
            ("B4", Value::Num(5.0)),
            ("C4", Value::Num(2.0)),
            ("A5", Value::Str("Total".into())),
            ("B5", Value::Num(10.0)),
            ("C5", Value::Num(7.5)),
        ])
        .with_table(sales_table())
    }

    #[test]
    fn structured_refs_parse_and_print() {
        for (src, printed) in [
            ("SUM(Sales[Qty])", "SUM(Sales[Qty])"),
            ("Sales[]", "Sales[]"),
            ("SUM(Sales[#All])", "SUM(Sales[#All])"),
            ("COUNTA(Sales[#Headers])", "COUNTA(Sales[#Headers])"),
            ("SUM(Sales[#Totals])", "SUM(Sales[#Totals])"),
            ("[@Qty]*[@Price]", "[@Qty]*[@Price]"),
            ("SUM(Sales[[#Totals],[Qty]])", "SUM(Sales[[#Totals],[Qty]])"),
            ("SUM(Sales[[Qty]:[Price]])", "SUM(Sales[[Qty]:[Price]])"),
            ("Sales[@Price]", "Sales[@Price]"),
        ] {
            let ast = parse(src).unwrap_or_else(|e| panic!("parse {src}: {e}"));
            assert_eq!(to_string(&ast), printed, "{src}");
            let re = parse(&to_string(&ast)).unwrap();
            assert_eq!(ast, re, "round-trip {src}");
        }
        // Escaped specials in column names survive.
        let ast = parse("SUM(Sales['[odd'] col])").unwrap();
        if let Expr::Func(_, args) = &ast {
            assert_eq!(
                args[0],
                Expr::Structured {
                    table: Some("Sales".into()),
                    item: TableItem::Data,
                    col1: Some("[odd] col".into()),
                    col2: None,
                }
            );
        } else {
            panic!("not a func");
        }
        assert_eq!(parse(&to_string(&ast)).unwrap(), ast);
    }

    #[test]
    fn three_d_parse_and_print() {
        for src in [
            "SUM(Sheet1:Sheet3!A1)",
            "SUM(Sheet1:Sheet3!A1:B2)",
            "SUM('My First':'My Last'!$A$1)",
            "COUNT(One:Three!A1:B2)",
        ] {
            let ast = parse(src).unwrap_or_else(|e| panic!("parse {src}: {e}"));
            let printed = to_string(&ast);
            assert_eq!(parse(&printed).unwrap(), ast, "{src} → {printed}");
        }
        // Copy translation shifts the rect, keeps the span.
        assert_eq!(
            translate_formula("SUM(One:Three!A1)", 2, 1).unwrap(),
            "SUM(One:Three!B3)"
        );
        // Sheet rename touches matching endpoints.
        assert_eq!(
            rename_sheet_in_formula("SUM(One:Three!A1)", "Three", "Last Q").unwrap(),
            "SUM(One:'Last Q'!A1)"
        );
    }

    #[test]
    fn structured_refs_evaluate() {
        let g = sales_grid();
        assert_eq!(n("SUM(Sales[Qty])", &g), 10.0); // data rows only
        assert_eq!(n("SUM(Sales[#Totals])", &g), 17.5);
        assert_eq!(n("COUNTA(Sales[#Headers])", &g), 3.0);
        assert_eq!(n("COUNTA(Sales[])", &g), 9.0);
        assert_eq!(n("SUM(Sales[[Qty]:[Price]])", &g), 17.5);
        assert_eq!(n("SUM(Sales[#All])", &g), 35.0); // data + totals rows
        // Bare @ refs resolve through the enclosing table, so the formula
        // must live inside it (a calculated column). Row 3 → pad.
        let ast = parse("[@Qty]*[@Price]").unwrap();
        let mut ev = Eval::new(&g, 0, (2, 2));
        assert_eq!(ev.eval(&ast), Value::Num(8.0));
        // Outside the table the qualified form works…
        let ast_q = parse("Sales[@Qty]*Sales[@Price]").unwrap();
        let mut ev = Eval::new(&g, 0, (2, 4));
        assert_eq!(ev.eval(&ast_q), Value::Num(8.0));
        // …but the bare form has no enclosing table → #REF!.
        let mut ev = Eval::new(&g, 0, (20, 20));
        assert_eq!(ev.eval(&ast), Value::Err(ExcelError::Ref));
        // Unknown column → #REF!, still supported (the table was found).
        let g2 = sales_grid();
        let ast = parse("SUM(Sales[Nope])").unwrap();
        let mut ev = Eval::new(&g2, 0, (0, 5));
        assert_eq!(ev.eval(&ast), Value::Err(ExcelError::Ref));
        assert!(!ev.unsupported);
        // Unknown table → unsupported (keep cached).
        let ast = parse("SUM(Ghost[Qty])").unwrap();
        let mut ev = Eval::new(&g2, 0, (0, 5));
        let _ = ev.eval(&ast);
        assert!(ev.unsupported);
    }

    // ---- dynamic arrays ---------------------------------------------------

    /// Evaluate with dynamic semantics, expecting an array result.
    fn eval_array(src: &str, grid: &Grid) -> Matrix {
        let ast = parse(src).unwrap_or_else(|e| panic!("parse {src}: {e}"));
        let mut ev = Eval::new(grid, 0, (0, 0));
        match ev.eval_dynamic(&ast) {
            DynResult::Array(m) => m,
            DynResult::Scalar(v) => panic!("{src} → scalar {v:?}, expected array"),
        }
    }

    fn nums(m: &Matrix) -> Vec<Vec<f64>> {
        m.iter()
            .map(|row| {
                row.iter()
                    .map(|v| match v {
                        Value::Num(n) => *n,
                        other => panic!("expected number, got {other:?}"),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn spill_ref_and_implicit_parse_and_print() {
        // A1# round-trips, and Excel's stored spellings map onto it.
        let ast = parse("SUM(A1#)").unwrap();
        assert_eq!(to_string(&ast), "SUM(A1#)");
        let stored = parse("SUM(_xlfn.ANCHORARRAY(A1))").unwrap();
        assert_eq!(to_string(&stored), "SUM(A1#)");
        let ast = parse("Sheet1!B2#").unwrap();
        assert_eq!(to_string(&ast), "Sheet1!B2#");
        // @ / SINGLE.
        let ast = parse("@A1:A3").unwrap();
        assert_eq!(to_string(&ast), "@A1:A3");
        let stored = parse("_xlfn.SINGLE(A1:A3)").unwrap();
        assert_eq!(to_string(&stored), "@A1:A3");
        // Copy/fill translation follows the anchor.
        assert_eq!(translate_formula("SUM(A1#)", 1, 1).unwrap(), "SUM(B2#)");
        // # after anything but a cell ref is rejected.
        assert!(parse("SUM(A1:A3#)").is_err());
    }

    #[test]
    fn array_literals() {
        let g = empty();
        let m = eval_array("{1,2;3,4}", &g);
        assert_eq!(nums(&m), vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
        let ast = parse("{1,2;3,4}").unwrap();
        assert_eq!(to_string(&ast), "{1,2;3,4}");
        assert_eq!(n("SUM({1,2;3,4})", &g), 10.0);
        assert_eq!(n("INDEX({10,20;30,40},2,1)", &g), 30.0);
        // Ragged constants are rejected.
        assert!(parse("{1,2;3}").is_err());
    }

    #[test]
    fn operator_broadcasting() {
        let g = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("A2", Value::Num(2.0)),
            ("A3", Value::Num(3.0)),
        ]);
        let m = eval_array("A1:A3*2", &g);
        assert_eq!(nums(&m), vec![vec![2.0], vec![4.0], vec![6.0]]);
        // Column vector + row vector broadcasts to the outer shape.
        let m = eval_array("{1;2}+{10,20}", &g);
        assert_eq!(nums(&m), vec![vec![11.0, 21.0], vec![12.0, 22.0]]);
        // Comparisons lift too — the shape FILTER's include argument needs.
        let m = eval_array("A1:A3>1", &g);
        assert_eq!(
            m,
            vec![
                vec![Value::Bool(false)],
                vec![Value::Bool(true)],
                vec![Value::Bool(true)]
            ]
        );
        // Aggregates consume array expressions (old SUMPRODUCT-only idiom).
        assert_eq!(n("SUM((A1:A3)*10)", &g), 60.0);
        // Non-conforming positions become #N/A.
        let m = eval_array("{1;2;3}+{1;2}", &g);
        assert_eq!(m[2][0], Value::Err(ExcelError::NA));
    }

    #[test]
    fn sequence_and_friends() {
        let g = empty();
        assert_eq!(
            nums(&eval_array("SEQUENCE(2,3)", &g)),
            vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]]
        );
        assert_eq!(
            nums(&eval_array("SEQUENCE(3,1,10,-2)", &g)),
            vec![vec![10.0], vec![8.0], vec![6.0]]
        );
        assert_eq!(
            nums(&eval_array("TRANSPOSE({1,2;3,4})", &g)),
            vec![vec![1.0, 3.0], vec![2.0, 4.0]]
        );
        assert_eq!(eval_str("SEQUENCE(0)", &g), Value::Err(ExcelError::Value));
        // 1×1 results stay scalar.
        assert_eq!(n("SEQUENCE(1)", &g), 1.0);
    }

    #[test]
    fn sort_sortby_unique() {
        let g = empty();
        assert_eq!(
            nums(&eval_array("SORT({3;1;2})", &g)),
            vec![vec![1.0], vec![2.0], vec![3.0]]
        );
        assert_eq!(
            nums(&eval_array("SORT({3;1;2},1,-1)", &g)),
            vec![vec![3.0], vec![2.0], vec![1.0]]
        );
        // Sort rows by the second column.
        assert_eq!(
            nums(&eval_array("SORT({1,9;2,7;3,8},2,1)", &g)),
            vec![vec![2.0, 7.0], vec![3.0, 8.0], vec![1.0, 9.0]]
        );
        // SORTBY with an external key vector.
        assert_eq!(
            nums(&eval_array("SORTBY({10;20;30},{3;1;2})", &g)),
            vec![vec![20.0], vec![30.0], vec![10.0]]
        );
        let m = eval_array("UNIQUE({1;2;1;3;2})", &g);
        assert_eq!(nums(&m), vec![vec![1.0], vec![2.0], vec![3.0]]);
        // The lone exactly-once value collapses to a 1×1 scalar.
        assert_eq!(n("UNIQUE({1;2;1;3;2},FALSE,TRUE)", &g), 3.0);
        // Case-insensitive like Excel: "a" and "A" are one value.
        let m = eval_array("UNIQUE({\"a\";\"A\";\"b\"})", &g);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn filter_function() {
        let g = Grid::new(&[
            ("A1", Value::Num(5.0)),
            ("A2", Value::Num(15.0)),
            ("A3", Value::Num(25.0)),
        ]);
        let m = eval_array("FILTER(A1:A3,A1:A3>10)", &g);
        assert_eq!(nums(&m), vec![vec![15.0], vec![25.0]]);
        // No matches → #CALC! without a fallback, the fallback otherwise.
        assert_eq!(
            eval_str("FILTER(A1:A3,A1:A3>99)", &g),
            Value::Err(ExcelError::Calc)
        );
        assert_eq!(n("FILTER(A1:A3,A1:A3>99,-1)", &g), -1.0);
    }

    #[test]
    fn shaping_functions() {
        let g = empty();
        assert_eq!(
            nums(&eval_array("TAKE(SEQUENCE(5),2)", &g)),
            vec![vec![1.0], vec![2.0]]
        );
        assert_eq!(
            nums(&eval_array("TAKE(SEQUENCE(5),-2)", &g)),
            vec![vec![4.0], vec![5.0]]
        );
        assert_eq!(
            nums(&eval_array("DROP(SEQUENCE(5),3)", &g)),
            vec![vec![4.0], vec![5.0]]
        );
        assert_eq!(
            nums(&eval_array("CHOOSEROWS({1;2;3},3,1)", &g)),
            vec![vec![3.0], vec![1.0]]
        );
        assert_eq!(n("CHOOSECOLS({1,2,3},-1)", &g), 3.0);
        assert_eq!(
            nums(&eval_array("VSTACK({1;2},{3;4})", &g)),
            vec![vec![1.0], vec![2.0], vec![3.0], vec![4.0]]
        );
        assert_eq!(
            nums(&eval_array("HSTACK({1;2},{3;4})", &g)),
            vec![vec![1.0, 3.0], vec![2.0, 4.0]]
        );
        // Ragged stacks pad with #N/A.
        let m = eval_array("VSTACK({1,2},{3})", &g);
        assert_eq!(m[1][1], Value::Err(ExcelError::NA));
        assert_eq!(
            nums(&eval_array("TOROW({1;2;3})", &g)),
            vec![vec![1.0, 2.0, 3.0]]
        );
        assert_eq!(
            nums(&eval_array("TOCOL({1,2;3,4})", &g)),
            vec![vec![1.0], vec![2.0], vec![3.0], vec![4.0]]
        );
        assert_eq!(
            nums(&eval_array("WRAPROWS({1;2;3;4;5;6},3)", &g)),
            vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]]
        );
        let m = eval_array("EXPAND({1},2,2,0)", &g);
        assert_eq!(nums(&m), vec![vec![1.0, 0.0], vec![0.0, 0.0]]);
    }

    #[test]
    fn let_bindings() {
        let g = Grid::new(&[("A1", Value::Num(7.0))]);
        assert_eq!(n("LET(x,2,x*3)", &g), 6.0);
        assert_eq!(n("LET(x,2,y,x+1,x*y)", &g), 6.0);
        assert_eq!(n("LET(x,A1,x+1)", &g), 8.0);
        // Bindings feed array functions and can hold ranges.
        assert_eq!(n("LET(v,{3;1;2},SUM(SORT(v)))", &g), 6.0);
        // Malformed shapes are rejected.
        assert_eq!(eval_str("LET(x,1)", &g), Value::Err(ExcelError::Value));
        assert_eq!(eval_str("LET(1,2,3)", &g), Value::Err(ExcelError::Value));
        // LET names shadow defined names, and scope ends with the LET.
        let g = Grid::new(&[("A1", Value::Num(1.0))]).with_name("K", "Sheet1!$A$1");
        assert_eq!(n("LET(K,10,K)+K", &g), 11.0);
    }

    #[test]
    fn implicit_intersection_rules() {
        let g = Grid::new(&[
            ("B1", Value::Num(10.0)),
            ("B2", Value::Num(20.0)),
            ("B3", Value::Num(30.0)),
        ]);
        // Formula in row 2: @B1:B3 picks the same-row value.
        let ast = parse("@B1:B3").unwrap();
        let mut ev = Eval::new(&g, 0, (1, 0));
        assert_eq!(ev.eval(&ast), Value::Num(20.0));
        // Outside the range's rows → #VALUE!.
        let mut ev = Eval::new(&g, 0, (9, 0));
        assert_eq!(ev.eval(&ast), Value::Err(ExcelError::Value));
        // @ on a computed array takes the top-left value.
        assert_eq!(n("@SORT({3;1;2})", &g), 1.0);
    }

    #[test]
    fn lookups_accept_computed_arrays() {
        let g = empty();
        assert_eq!(n("INDEX(SORT({3;1;2}),1)", &g), 1.0);
        assert_eq!(n("MATCH(5,{1;3;5;7},0)", &g), 3.0);
        assert_eq!(n("VLOOKUP(2,{1,10;2,20;3,30},2,FALSE)", &g), 20.0);
        assert_eq!(n("XLOOKUP(\"b\",{\"a\";\"b\";\"c\"},{1;2;3})", &g), 2.0);
        assert_eq!(n("SUMPRODUCT({1;2;3},{4;5;6})", &g), 32.0);
    }

    // ---- LAMBDA & friends --------------------------------------------------

    #[test]
    fn lambda_basics() {
        let g = empty();
        // Immediate invocation.
        assert_eq!(n("LAMBDA(x,x*2)(5)", &g), 10.0);
        assert_eq!(n("LAMBDA(x,y,x^y)(2,10)", &g), 1024.0);
        // Through LET, with lexical capture of earlier bindings.
        assert_eq!(n("LET(f,LAMBDA(x,x+1),f(41))", &g), 42.0);
        assert_eq!(n("LET(a,10,f,LAMBDA(x,x+a),f(5))", &g), 15.0);
        // Lambdas are first-class: pass one to another.
        assert_eq!(
            n("LET(apply,LAMBDA(f,v,f(v)),apply(LAMBDA(x,x*3),7))", &g),
            21.0
        );
        // Arity mismatch and uncalled lambdas error.
        assert_eq!(
            eval_str("LAMBDA(x,y,x+y)(1)", &g),
            Value::Err(ExcelError::Value)
        );
        assert_eq!(eval_str("LAMBDA(x,x*2)", &g), Value::Err(ExcelError::Calc));
        // Calling a non-lambda value.
        assert_eq!(eval_str("(1+2)(3)", &g), Value::Err(ExcelError::Value));
        // Round-trips through the serializer.
        let ast = parse("LAMBDA(x,x*2)(5)").unwrap();
        assert_eq!(to_string(&ast), "LAMBDA(x,x*2)(5)");
    }

    #[test]
    fn named_lambda_is_a_custom_function() {
        let g = Grid::new(&[("A1", Value::Num(5.0))])
            .with_name("DOUBLE", "LAMBDA(x,x*2)")
            .with_name("TAXED", "LAMBDA(x,x*Sheet1!$A$1)");
        assert_eq!(n("DOUBLE(21)", &g), 42.0);
        // The body can reference cells.
        assert_eq!(n("TAXED(3)", &g), 15.0);
        // Recursion is depth-capped, not a hang.
        let g = empty().with_name("LOOP", "LAMBDA(x,LOOP(x))");
        assert_eq!(eval_str("LOOP(1)", &g), Value::Err(ExcelError::Num));
    }

    #[test]
    fn map_reduce_scan() {
        let g = empty();
        assert_eq!(
            nums(&eval_array("MAP({1;2;3},LAMBDA(x,x*10))", &g)),
            vec![vec![10.0], vec![20.0], vec![30.0]]
        );
        // Two arrays zip elementwise.
        assert_eq!(
            nums(&eval_array("MAP({1;2},{10;20},LAMBDA(a,b,a+b))", &g)),
            vec![vec![11.0], vec![22.0]]
        );
        assert_eq!(n("REDUCE(0,{1;2;3;4},LAMBDA(a,v,a+v))", &g), 10.0);
        assert_eq!(n("REDUCE(1,{2;3;4},LAMBDA(a,v,a*v))", &g), 24.0);
        assert_eq!(
            nums(&eval_array("SCAN(0,{1;2;3},LAMBDA(a,v,a+v))", &g)),
            vec![vec![1.0], vec![3.0], vec![6.0]]
        );
        // Lambda arity must match the array count.
        assert_eq!(
            eval_str("MAP({1;2},LAMBDA(a,b,a+b))", &g),
            Value::Err(ExcelError::Value)
        );
    }

    #[test]
    fn byrow_bycol_makearray() {
        let g = empty();
        assert_eq!(
            nums(&eval_array("BYROW({1,2;3,4},LAMBDA(r,SUM(r)))", &g)),
            vec![vec![3.0], vec![7.0]]
        );
        assert_eq!(
            nums(&eval_array("BYCOL({1,2;3,4},LAMBDA(c,SUM(c)))", &g)),
            vec![vec![4.0, 6.0]]
        );
        assert_eq!(
            nums(&eval_array("MAKEARRAY(2,3,LAMBDA(r,c,r*c))", &g)),
            vec![vec![1.0, 2.0, 3.0], vec![2.0, 4.0, 6.0]]
        );
        // Composition with the shaping functions.
        assert_eq!(
            n(
                "SUM(BYROW(MAKEARRAY(3,3,LAMBDA(r,c,r+c)),LAMBDA(r,MAX(r))))",
                &g
            ),
            15.0 // rows max: 4,5,6
        );
    }

    #[test]
    fn scalar_functions_lift_over_arrays() {
        let g = Grid::new(&[
            ("A1", Value::Num(-1.0)),
            ("A2", Value::Num(2.0)),
            ("A3", Value::Num(-3.0)),
        ]);
        assert_eq!(
            nums(&eval_array("ABS(A1:A3)", &g)),
            vec![vec![1.0], vec![2.0], vec![3.0]]
        );
        assert_eq!(
            nums(&eval_array("LEN({\"a\";\"bbb\"})", &g)),
            vec![vec![1.0], vec![3.0]]
        );
        assert_eq!(
            nums(&eval_array("ROUND({1.24;1.26},1)", &g)),
            vec![vec![1.2], vec![1.3]]
        );
        // IF lifts over an array condition…
        let m = eval_array("IF(A1:A3>0,\"pos\",\"neg\")", &g);
        assert_eq!(
            m,
            vec![
                vec![Value::Str("neg".into())],
                vec![Value::Str("pos".into())],
                vec![Value::Str("neg".into())]
            ]
        );
        // …but stays lazy with a scalar condition (the unknown function in
        // the untaken branch must not poison the cell).
        let ast = parse("IF(TRUE,1,PIVOTBY(9))").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        assert_eq!(ev.eval(&ast), Value::Num(1.0));
        assert!(!ev.unsupported);
        // TEXT over an array.
        let m = eval_array("TEXT({1;2},\"0.0\")", &g);
        assert_eq!(
            m,
            vec![
                vec![Value::Str("1.0".into())],
                vec![Value::Str("2.0".into())]
            ]
        );
        // Errors stay per-element.
        let m = eval_array("SQRT({4;-1;9})", &g);
        assert_eq!(nums(&vec![m[0].clone()]), vec![vec![2.0]]);
        assert_eq!(m[1][0], Value::Err(ExcelError::Num));
        assert_eq!(nums(&vec![m[2].clone()]), vec![vec![3.0]]);
        // Scalar calls through the lift path still behave (single eval).
        assert_eq!(n("ABS(-7)", &g), 7.0);
        assert_eq!(
            eval_str("LEFT(\"hello\",2)&RIGHT(\"hello\",2)", &g),
            Value::Str("helo".into())
        );
    }

    #[test]
    fn lambda_in_map_composes_with_lift_and_spill_fns() {
        let g = empty();
        // FILTER over MAP output, reduced — a full pipeline.
        assert_eq!(
            n(
                "SUM(FILTER(MAP(SEQUENCE(6),LAMBDA(x,x*x)),MAP(SEQUENCE(6),LAMBDA(x,ISEVEN(x)))))",
                &g
            ),
            56.0 // 4 + 16 + 36
        );
    }

    #[test]
    fn sumx_family_iterates_rows() {
        // A 2-column table: Qty (col 0) and Price (col 1), rows 1..=3.
        let g = Grid::new(&[
            ("A1", Value::Str("Qty".into())),
            ("B1", Value::Str("Price".into())),
            ("A2", Value::Num(2.0)),
            ("B2", Value::Num(10.0)),
            ("A3", Value::Num(3.0)),
            ("B3", Value::Num(5.0)),
            ("A4", Value::Num(1.0)),
            ("B4", Value::Num(100.0)),
        ])
        .with_table(TableInfo {
            sheet: 0,
            range: (0, 0, 3, 1),
            header_rows: 1,
            totals_rows: 0,
            columns: vec!["Qty".into(), "Price".into()],
        });
        // Row context: [@Qty]*[@Price] per row → 20 + 15 + 100.
        assert_eq!(n("SUMX(Sales,[@Qty]*[@Price])", &g), 135.0);
        assert_eq!(n("AVERAGEX(Sales,[@Qty]*[@Price])", &g), 45.0);
        assert_eq!(n("MAXX(Sales,[@Qty]*[@Price])", &g), 100.0);
        assert_eq!(n("MINX(Sales,[@Qty]*[@Price])", &g), 15.0);
        assert_eq!(n("COUNTX(Sales,[@Price])", &g), 3.0);
        // Conditional row logic.
        assert_eq!(n("SUMX(Sales,IF([@Qty]>1,[@Qty]*[@Price],0))", &g), 35.0);
        // Unknown table: honest #NAME? and unsupported.
        let ast = parse("SUMX(Nope,[@Qty])").unwrap();
        let mut ev = Eval::new(&g, 0, (0, 0));
        assert_eq!(ev.eval(&ast), Value::Err(ExcelError::Name));
        assert!(ev.unsupported);
    }

    #[test]
    fn optional_lambda_params_and_isomitted() {
        let g = empty();
        // [y] is optional: callable with one or two arguments.
        assert_eq!(n("LAMBDA(x,[y],x+IF(ISOMITTED(y),100,y))(1,5)", &g), 6.0);
        assert_eq!(n("LAMBDA(x,[y],x+IF(ISOMITTED(y),100,y))(1)", &g), 101.0);
        // An omitted parameter evaluates as blank (0 in arithmetic).
        assert_eq!(n("LAMBDA(x,[y],x+y)(7)", &g), 7.0);
        // An explicit empty slot counts as omitted too.
        assert_eq!(
            eval_str("LAMBDA(x,[y],ISOMITTED(y))(1,)", &g),
            Value::Bool(true)
        );
        // Provided values are not omitted.
        assert_eq!(
            eval_str("LAMBDA(x,[y],ISOMITTED(y))(1,2)", &g),
            Value::Bool(false)
        );
        // Too few required args still errors; required after optional is
        // rejected at definition.
        assert_eq!(
            eval_str("LAMBDA(x,[y],x)()", &g),
            Value::Err(ExcelError::Value)
        );
        assert_eq!(
            eval_str("LAMBDA([x],y,x)(1,2)", &g),
            Value::Err(ExcelError::Value)
        );
        // Through LET-bound names as well.
        assert_eq!(
            n(
                "LET(f,LAMBDA(a,[b],a*IF(ISOMITTED(b),2,b)),f(10)+f(10,3))",
                &g
            ),
            50.0
        );
    }

    #[test]
    fn adversarial_input_does_not_crash_or_hang() {
        // Deeply nested parens / unary chains must fail to parse, not
        // overflow the stack.
        let deep_parens = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
        assert!(parse(&deep_parens).is_err());
        let deep_unary = format!("{}1", "-".repeat(5000));
        assert!(parse(&deep_unary).is_err());
        // A legitimately (but modestly) nested formula still parses.
        assert_eq!(
            n(&format!("{}5{}", "(".repeat(30), ")".repeat(30)), &empty()),
            5.0
        );
        // MID / REPLACE with an enormous count must not panic.
        let g = empty();
        assert_eq!(eval_str("MID(\"ab\",2,1e20)", &g), Value::Str("b".into()));
        assert_eq!(
            eval_str("REPLACE(\"hello\",2,1e20,\"x\")", &g),
            Value::Str("hx".into())
        );
        // The wildcard matcher is linear, not exponential: an adversarial
        // pattern returns quickly (this test would hang on the old matcher).
        let g = Grid::new(&[("A1", Value::Str("a".repeat(40)))]);
        let pat = "a*".repeat(20) + "z";
        let f = format!("COUNTIF(A1:A2,\"{pat}\")");
        assert_eq!(eval_str(&f, &g), Value::Num(0.0));
        // COMBIN / DATE with extreme arguments are #NUM!, not a hang/overflow.
        assert_eq!(
            eval_str("COMBIN(1e15,5e14)", &g),
            Value::Err(ExcelError::Num)
        );
        assert_eq!(eval_str("DATE(9e18,1,1)", &g), Value::Err(ExcelError::Num));
    }

    #[test]
    fn wildcard_matcher_semantics() {
        assert!(wildcard_match("a*c", "abbbc"));
        assert!(wildcard_match("a*c", "ac"));
        assert!(!wildcard_match("a*c", "abbb"));
        assert!(wildcard_match("h?llo", "hello"));
        assert!(!wildcard_match("h?llo", "hllo"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("**a", "xxa"));
        // ~ escapes a literal * / ?.
        assert!(wildcard_match("a~*b", "a*b"));
        assert!(!wildcard_match("a~*b", "axb"));
        // Case-insensitive incl. non-ASCII.
        assert!(wildcard_match("cafÉ*", "café latte"));
    }

    #[test]
    fn weekday_modes_and_1904() {
        // 2024-01-15 is a Monday. 1900-system serial 45306.
        let g = empty();
        assert_eq!(n("WEEKDAY(45306,1)", &g), 2.0); // Sun=1 -> Mon=2
        assert_eq!(n("WEEKDAY(45306,2)", &g), 1.0); // Mon=1
        assert_eq!(n("WEEKDAY(45306,3)", &g), 0.0); // Mon=0
        assert_eq!(n("WEEKDAY(45306,11)", &g), 1.0); // week starts Mon
        assert_eq!(n("WEEKDAY(45306,12)", &g), 7.0); // week starts Tue -> Mon=7
        assert_eq!(n("WEEKDAY(45306,13)", &g), 6.0); // week starts Wed
        assert_eq!(
            eval_str("WEEKDAY(45306,4)", &g),
            Value::Err(ExcelError::Num)
        );
    }

    #[test]
    fn math_and_engineering_coverage() {
        let g = empty();
        // MROUND / SQRTPI.
        assert_eq!(n("MROUND(10,3)", &g), 9.0);
        assert_eq!(n("MROUND(-10,-3)", &g), -9.0);
        assert_eq!(n("MROUND(1.3,0.2)", &g), 1.4000000000000001);
        assert_eq!(eval_str("MROUND(5,-2)", &g), Value::Err(ExcelError::Num));
        assert!((n("SQRTPI(4)", &g) - (4.0 * std::f64::consts::PI).sqrt()).abs() < 1e-9);
        // ROMAN / ARABIC round-trip.
        assert_eq!(eval_str("ROMAN(1994)", &g), Value::Str("MCMXCIV".into()));
        assert_eq!(n("ARABIC(\"MCMXCIV\")", &g), 1994.0);
        assert_eq!(n("ARABIC(\"-IV\")", &g), -4.0);
        assert_eq!(n("ARABIC(\"\")", &g), 0.0);
        // BASE / DECIMAL.
        assert_eq!(eval_str("BASE(255,16)", &g), Value::Str("FF".into()));
        assert_eq!(eval_str("BASE(7,2,8)", &g), Value::Str("00000111".into()));
        assert_eq!(n("DECIMAL(\"FF\",16)", &g), 255.0);
        assert_eq!(n("DECIMAL(\"111\",2)", &g), 7.0);
        // Paired-array sums.
        let gg = Grid::new(&[
            ("A1", Value::Num(2.0)),
            ("A2", Value::Num(3.0)),
            ("B1", Value::Num(1.0)),
            ("B2", Value::Num(1.0)),
        ]);
        assert_eq!(n("SUMX2MY2(A1:A2,B1:B2)", &gg), 11.0); // (4-1)+(9-1)
        assert_eq!(n("SUMX2PY2(A1:A2,B1:B2)", &gg), 15.0);
        assert_eq!(n("SUMXMY2(A1:A2,B1:B2)", &gg), 5.0); // 1+4
        assert_eq!(
            eval_str("SUMXMY2(A1:A2,B1:B1)", &gg),
            Value::Err(ExcelError::NA)
        );
        assert_eq!(n("MULTINOMIAL(2,3,4)", &g), 1260.0);
        // Bit ops.
        assert_eq!(n("BITAND(12,10)", &g), 8.0);
        assert_eq!(n("BITOR(12,10)", &g), 14.0);
        assert_eq!(n("BITXOR(12,10)", &g), 6.0);
        assert_eq!(n("BITLSHIFT(4,2)", &g), 16.0);
        assert_eq!(n("BITRSHIFT(16,2)", &g), 4.0);
        // Base conversion with two's complement.
        assert_eq!(eval_str("DEC2BIN(9)", &g), Value::Str("1001".into()));
        assert_eq!(eval_str("DEC2BIN(-1)", &g), Value::Str("1111111111".into()));
        assert_eq!(eval_str("DEC2HEX(255)", &g), Value::Str("FF".into()));
        assert_eq!(n("BIN2DEC(\"1111111111\")", &g), -1.0);
        assert_eq!(n("HEX2DEC(\"FF\")", &g), 255.0);
        assert_eq!(n("OCT2DEC(\"777\")", &g), 511.0);
        // DELTA / GESTEP.
        assert_eq!(n("DELTA(5,5)", &g), 1.0);
        assert_eq!(n("DELTA(5,4)", &g), 0.0);
        assert_eq!(n("GESTEP(5,4)", &g), 1.0);
        assert_eq!(n("GESTEP(3,4)", &g), 0.0);
    }

    fn approx(src: &str, grid: &Grid, want: f64) {
        let got = n(src, grid);
        assert!(
            (got - want).abs() < 1e-2,
            "{src} → {got}, expected ≈ {want}"
        );
    }

    #[test]
    fn financial_coverage() {
        let g = empty();
        // IPMT/PPMT of a 3-year, 8000 loan at 10%/yr.
        approx("IPMT(0.1/12,1,36,8000)", &g, -66.67);
        approx("PPMT(0.1/12,1,36,8000)", &g, -191.47);
        approx("ISPMT(0.1,1,3,8000)", &g, -533.33);
        // Depreciation.
        approx("SLN(10000,1000,5)", &g, 1800.0);
        approx("SYD(10000,1000,5,1)", &g, 3000.0);
        approx("DDB(10000,1000,5,1)", &g, 4000.0);
        approx("DB(1000000,100000,6,1,7)", &g, 186083.33);
        // Rate conversion.
        approx("EFFECT(0.0525,4)", &g, 0.053543);
        approx("NOMINAL(0.053543,4)", &g, 0.0525);
        // Growth helpers.
        approx("PDURATION(0.025,2000,2200)", &g, 3.86);
        approx("RRI(96,10000,11000)", &g, 0.000992);
        // Dollar fraction round-trip.
        approx("DOLLARDE(1.02,16)", &g, 1.125);
        approx("DOLLARFR(1.125,16)", &g, 1.02);
        // MIRR.
        let cf = Grid::new(&[
            ("A1", Value::Num(-120000.0)),
            ("A2", Value::Num(39000.0)),
            ("A3", Value::Num(30000.0)),
            ("A4", Value::Num(21000.0)),
            ("A5", Value::Num(37000.0)),
            ("A6", Value::Num(46000.0)),
        ]);
        approx("MIRR(A1:A6,0.1,0.12)", &cf, 0.126094);
        // XNPV / XIRR with explicit dates (serials).
        let xg = Grid::new(&[
            ("A1", Value::Num(-10000.0)),
            ("A2", Value::Num(2750.0)),
            ("A3", Value::Num(4250.0)),
            ("A4", Value::Num(3250.0)),
            ("A5", Value::Num(2750.0)),
            ("B1", Value::Num(39448.0)), // 2008-01-01
            ("B2", Value::Num(39508.0)),
            ("B3", Value::Num(39751.0)),
            ("B4", Value::Num(39859.0)),
            ("B5", Value::Num(39904.0)),
        ]);
        approx("XNPV(0.09,A1:A5,B1:B5)", &xg, 2086.65);
        approx("XIRR(A1:A5,B1:B5)", &xg, 0.373363);
        // CUMIPMT/CUMPRINC — Microsoft's documented examples.
        approx("CUMIPMT(0.09/12,360,125000,13,24,0)", &g, -11135.23);
        approx("CUMPRINC(0.09/12,360,125000,1,1,0)", &g, -68.28);
    }

    #[test]
    fn statistics_coverage() {
        // Regression corpus (Microsoft's SLOPE/INTERCEPT example).
        let g = Grid::new(&[
            ("A1", Value::Num(2.0)),
            ("A2", Value::Num(3.0)),
            ("A3", Value::Num(9.0)),
            ("A4", Value::Num(1.0)),
            ("A5", Value::Num(8.0)),
            ("B1", Value::Num(6.0)),
            ("B2", Value::Num(5.0)),
            ("B3", Value::Num(11.0)),
            ("B4", Value::Num(7.0)),
            ("B5", Value::Num(5.0)),
        ]);
        approx("SLOPE(A1:A5,B1:B5)", &g, 0.669355);
        approx("INTERCEPT(A1:A5,B1:B5)", &g, 0.048387);
        approx("CORREL(A1:A5,B1:B5)", &g, 0.457011);
        approx("RSQ(A1:A5,B1:B5)", &g, 0.208859);
        approx("FORECAST(10,A1:A5,B1:B5)", &g, 6.741935);
        approx("PEARSON(A1:A5,B1:B5)", &g, 0.457011);
        approx("COVARIANCE.P(A1:A5,B1:B5)", &g, 3.32);
        approx("COVARIANCE.S(A1:A5,B1:B5)", &g, 4.15);

        // Single-array shape/spread over 3,4,5,2,4.
        let s = Grid::new(&[
            ("A1", Value::Num(3.0)),
            ("A2", Value::Num(4.0)),
            ("A3", Value::Num(5.0)),
            ("A4", Value::Num(2.0)),
            ("A5", Value::Num(4.0)),
        ]);
        approx("DEVSQ(A1:A5)", &s, 5.2);
        approx("AVEDEV(A1:A5)", &s, 0.88);
        approx("GEOMEAN(A1:A5)", &s, 3.437544);
        approx("HARMEAN(A1:A5)", &s, 3.260870);
        approx("STANDARDIZE(4,3.6,1.140175)", &s, 0.350823);

        // Skew/Kurtosis on Microsoft's documented series 3,4,5,2,3,4,5,6,4,7.
        let k = Grid::new(&[
            ("A1", Value::Num(3.0)),
            ("A2", Value::Num(4.0)),
            ("A3", Value::Num(5.0)),
            ("A4", Value::Num(2.0)),
            ("A5", Value::Num(3.0)),
            ("A6", Value::Num(4.0)),
            ("A7", Value::Num(5.0)),
            ("A8", Value::Num(6.0)),
            ("A9", Value::Num(4.0)),
            ("A10", Value::Num(7.0)),
        ]);
        approx("SKEW(A1:A10)", &k, 0.359543);
        approx("KURT(A1:A10)", &k, -0.151799);

        let g2 = empty();
        approx("FISHER(0.75)", &g2, 0.972955);
        approx("FISHERINV(0.972955)", &g2, 0.75);
        approx("TRIMMEAN(A1:A10,0.2)", &k, 4.25);
        approx("PERCENTRANK(A1:A10,4)", &k, 0.333);
        approx("PERCENTILE.EXC(A1:A10,0.25)", &k, 3.0);
        approx("QUARTILE.EXC(A1:A10,1)", &k, 3.0);

        // …A variants: text counts as 0.
        let a = Grid::new(&[
            ("A1", Value::Num(10.0)),
            ("A2", Value::Str("x".into())),
            ("A3", Value::Num(20.0)),
            ("A4", Value::Bool(true)),
        ]);
        approx("AVERAGEA(A1:A4)", &a, 7.75); // (10+0+20+1)/4
        approx("MAXA(A1:A4)", &a, 20.0);
        approx("MINA(A1:A4)", &a, 0.0);
    }

    #[test]
    fn date_coverage() {
        let g = empty();
        // DATEVALUE across formats (all → 2024-01-15 = serial 45306).
        assert_eq!(n("DATEVALUE(\"2024-01-15\")", &g), 45306.0);
        assert_eq!(n("DATEVALUE(\"1/15/2024\")", &g), 45306.0);
        assert_eq!(n("DATEVALUE(\"15-Jan-2024\")", &g), 45306.0);
        assert_eq!(n("DATEVALUE(\"January 15, 2024\")", &g), 45306.0);
        assert_eq!(
            eval_str("DATEVALUE(\"not a date\")", &g),
            Value::Err(ExcelError::Value)
        );
        // TIMEVALUE.
        approx("TIMEVALUE(\"12:00:00\")", &g, 0.5);
        approx("TIMEVALUE(\"6:00 PM\")", &g, 0.75);
        approx("TIMEVALUE(\"12:00 AM\")", &g, 0.0);
        // YEARFRAC bases (2024-01-01 .. 2024-07-01).
        approx("YEARFRAC(DATE(2024,1,1),DATE(2024,7,1),0)", &g, 0.5);
        approx("YEARFRAC(DATE(2024,1,1),DATE(2024,7,1),1)", &g, 0.497268);
        approx("YEARFRAC(DATE(2024,1,1),DATE(2024,7,1),2)", &g, 0.505556);
        approx("YEARFRAC(DATE(2024,1,1),DATE(2024,7,1),3)", &g, 0.498630);
        approx("YEARFRAC(DATE(2024,1,1),DATE(2024,7,1),4)", &g, 0.5);
        // DAYS360.
        assert_eq!(n("DAYS360(DATE(2024,1,31),DATE(2024,3,31))", &g), 60.0);
        assert_eq!(n("DAYS360(DATE(2024,1,1),DATE(2024,12,31))", &g), 360.0);
        // WORKDAY / NETWORKDAYS. 2024-01-15 is a Monday.
        assert_eq!(
            n("WORKDAY(DATE(2024,1,15),5)", &g),
            n("DATE(2024,1,22)", &g)
        );
        assert_eq!(n("NETWORKDAYS(DATE(2024,1,15),DATE(2024,1,19))", &g), 5.0);
        assert_eq!(n("NETWORKDAYS(DATE(2024,1,13),DATE(2024,1,14))", &g), 0.0);
        // Holiday excluded.
        let h = Grid::new(&[("Z1", eval_str("DATE(2024,1,17)", &g))]);
        assert_eq!(
            n("NETWORKDAYS(DATE(2024,1,15),DATE(2024,1,19),Z1)", &h),
            4.0
        );
        // INTL: Fri/Sat weekend (code 7).
        assert_eq!(
            n("NETWORKDAYS.INTL(DATE(2024,1,15),DATE(2024,1,21),7)", &g),
            5.0
        );
        // INTL: string mask, Sundays only off ("0000001").
        assert_eq!(
            n(
                "NETWORKDAYS.INTL(DATE(2024,1,15),DATE(2024,1,21),\"0000001\")",
                &g
            ),
            6.0
        );
    }

    #[test]
    fn text_info_lookup_coverage() {
        let g = empty();
        // TEXTBEFORE / TEXTAFTER.
        assert_eq!(
            eval_str("TEXTBEFORE(\"a-b-c\",\"-\")", &g),
            Value::Str("a".into())
        );
        assert_eq!(
            eval_str("TEXTAFTER(\"a-b-c\",\"-\")", &g),
            Value::Str("b-c".into())
        );
        assert_eq!(
            eval_str("TEXTBEFORE(\"a-b-c\",\"-\",2)", &g),
            Value::Str("a-b".into())
        );
        assert_eq!(
            eval_str("TEXTAFTER(\"a-b-c\",\"-\",-1)", &g),
            Value::Str("c".into())
        );
        assert_eq!(
            eval_str("TEXTBEFORE(\"abc\",\"-\",1,0,0,\"none\")", &g),
            Value::Str("none".into())
        );
        // TEXTSPLIT into a spilled array.
        assert_eq!(
            eval_array("TEXTSPLIT(\"a,b,c\",\",\")", &g),
            vec![vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
                Value::Str("c".into())
            ]]
        );
        assert_eq!(
            eval_array("TEXTSPLIT(\"a,b;c,d\",\",\",\";\")", &g),
            vec![
                vec![Value::Str("a".into()), Value::Str("b".into())],
                vec![Value::Str("c".into()), Value::Str("d".into())],
            ]
        );
        // DOLLAR / FIXED.
        assert_eq!(
            eval_str("DOLLAR(1234.567)", &g),
            Value::Str("$1,234.57".into())
        );
        assert_eq!(
            eval_str("DOLLAR(-1234.567,2)", &g),
            Value::Str("($1,234.57)".into())
        );
        assert_eq!(
            eval_str("FIXED(1234.567,1)", &g),
            Value::Str("1,234.6".into())
        );
        assert_eq!(
            eval_str("FIXED(1234.567,1,TRUE)", &g),
            Value::Str("1234.6".into())
        );
        assert_eq!(eval_str("FIXED(1234.5,-2)", &g), Value::Str("1,200".into()));
        // XMATCH.
        let x = Grid::new(&[
            ("A1", Value::Num(10.0)),
            ("A2", Value::Num(20.0)),
            ("A3", Value::Num(30.0)),
            ("A4", Value::Num(40.0)),
        ]);
        assert_eq!(n("XMATCH(30,A1:A4)", &x), 3.0);
        assert_eq!(n("XMATCH(25,A1:A4,-1)", &x), 2.0); // next smaller
        assert_eq!(n("XMATCH(25,A1:A4,1)", &x), 3.0); // next larger
        assert_eq!(n("XMATCH(40,A1:A4,0,-1)", &x), 4.0); // last-to-first
        assert_eq!(eval_str("XMATCH(99,A1:A4)", &x), Value::Err(ExcelError::NA));
        // ADDRESS.
        assert_eq!(eval_str("ADDRESS(1,1)", &g), Value::Str("$A$1".into()));
        assert_eq!(eval_str("ADDRESS(2,3,4)", &g), Value::Str("C2".into()));
        assert_eq!(
            eval_str("ADDRESS(1,1,1,FALSE)", &g),
            Value::Str("R1C1".into())
        );
        assert_eq!(
            eval_str("ADDRESS(1,1,1,TRUE,\"Sheet1\")", &g),
            Value::Str("Sheet1!$A$1".into())
        );
        // ERROR.TYPE / TYPE.
        assert_eq!(n("ERROR.TYPE(1/0)", &g), 2.0);
        assert_eq!(n("ERROR.TYPE(NA())", &g), 7.0);
        assert_eq!(eval_str("ERROR.TYPE(5)", &g), Value::Err(ExcelError::NA));
        assert_eq!(n("TYPE(42)", &g), 1.0);
        assert_eq!(n("TYPE(\"hi\")", &g), 2.0);
        assert_eq!(n("TYPE(TRUE)", &g), 4.0);
        assert_eq!(n("TYPE(1/0)", &g), 16.0);
        assert_eq!(n("TYPE(A1:A4)", &x), 64.0);
    }

    #[test]
    fn trig_and_engineering_extras() {
        let g = empty();
        approx("ASINH(1)", &g, 0.881374);
        approx("ACOSH(2)", &g, 1.316958);
        approx("ATANH(0.5)", &g, 0.549306);
        approx("SEC(1)", &g, 1.850816);
        approx("CSC(1)", &g, 1.188395);
        approx("COT(1)", &g, 0.642093);
        approx("ACOT(1)", &g, std::f64::consts::FRAC_PI_4);
        approx("ACOTH(2)", &g, 0.549306);
        approx("SECH(1)", &g, 0.648054);
        assert_eq!(eval_str("ATANH(1)", &g), Value::Err(ExcelError::Num));
        // Counting.
        assert_eq!(n("COMBINA(4,3)", &g), 20.0);
        assert_eq!(n("FACTDOUBLE(7)", &g), 105.0);
        assert_eq!(n("FACTDOUBLE(6)", &g), 48.0);
        // Cross-base conversion.
        assert_eq!(eval_str("BIN2HEX(\"1111\")", &g), Value::Str("F".into()));
        assert_eq!(eval_str("HEX2BIN(\"F\")", &g), Value::Str("1111".into()));
        assert_eq!(eval_str("OCT2HEX(\"17\")", &g), Value::Str("F".into()));
        assert_eq!(eval_str("BIN2OCT(\"1000\")", &g), Value::Str("10".into()));
        assert_eq!(n("HEX2DEC(BIN2HEX(\"1010\"))", &g), 10.0);
    }

    #[test]
    fn database_functions() {
        // A little orchard database (Microsoft's classic D-function example).
        let g = Grid::new(&[
            ("A1", Value::Str("Tree".into())),
            ("B1", Value::Str("Height".into())),
            ("C1", Value::Str("Profit".into())),
            ("A2", Value::Str("Apple".into())),
            ("B2", Value::Num(18.0)),
            ("C2", Value::Num(105.0)),
            ("A3", Value::Str("Pear".into())),
            ("B3", Value::Num(12.0)),
            ("C3", Value::Num(96.0)),
            ("A4", Value::Str("Apple".into())),
            ("B4", Value::Num(13.0)),
            ("C4", Value::Num(105.0)),
            ("A5", Value::Str("Cherry".into())),
            ("B5", Value::Num(9.0)),
            ("C5", Value::Num(75.0)),
            ("A6", Value::Str("Apple".into())),
            ("B6", Value::Num(8.0)),
            ("C6", Value::Num(76.0)),
            // Criteria block: Tree = Apple, Height > 10.
            ("E1", Value::Str("Tree".into())),
            ("F1", Value::Str("Height".into())),
            ("E2", Value::Str("Apple".into())),
            ("F2", Value::Str(">10".into())),
        ]);
        // Apples taller than 10: rows 2 (18,105) and 4 (13,105).
        assert_eq!(n("DSUM(A1:C6,\"Profit\",E1:F2)", &g), 210.0);
        assert_eq!(n("DSUM(A1:C6,3,E1:F2)", &g), 210.0);
        assert_eq!(n("DCOUNT(A1:C6,\"Height\",E1:F2)", &g), 2.0);
        assert_eq!(n("DCOUNTA(A1:C6,\"Tree\",E1:F2)", &g), 2.0);
        assert_eq!(n("DAVERAGE(A1:C6,\"Profit\",E1:F2)", &g), 105.0);
        assert_eq!(n("DMAX(A1:C6,\"Height\",E1:F2)", &g), 18.0);
        assert_eq!(n("DMIN(A1:C6,\"Height\",E1:F2)", &g), 13.0);
        assert_eq!(
            eval_str("DGET(A1:C6,\"Tree\",E1:F2)", &g),
            Value::Err(ExcelError::Num) // two matches
        );

        // Narrow to a single row (Height > 15) so DGET returns it.
        let g2 = Grid::new(&[
            ("A1", Value::Str("Tree".into())),
            ("B1", Value::Str("Height".into())),
            ("A2", Value::Str("Apple".into())),
            ("B2", Value::Num(18.0)),
            ("A3", Value::Str("Pear".into())),
            ("B3", Value::Num(12.0)),
            ("E1", Value::Str("Height".into())),
            ("E2", Value::Str(">15".into())),
        ]);
        assert_eq!(
            eval_str("DGET(A1:B3,\"Tree\",E1:E2)", &g2),
            Value::Str("Apple".into())
        );
    }

    #[test]
    fn matrix_functions() {
        // 2×2 with determinant 1×4 − 2×3 = −2.
        let g = Grid::new(&[
            ("A1", Value::Num(1.0)),
            ("B1", Value::Num(2.0)),
            ("A2", Value::Num(3.0)),
            ("B2", Value::Num(4.0)),
        ]);
        approx("MDETERM(A1:B2)", &g, -2.0);
        // Inverse of [[1,2],[3,4]] = [[-2,1],[1.5,-0.5]].
        let inv = eval_array("MINVERSE(A1:B2)", &g);
        assert!((nums(&inv)[0][0] - -2.0).abs() < 1e-9);
        assert!((nums(&inv)[1][0] - 1.5).abs() < 1e-9);
        // A · A⁻¹ = I.
        let prod = eval_array("MMULT(A1:B2,MINVERSE(A1:B2))", &g);
        let p = nums(&prod);
        assert!((p[0][0] - 1.0).abs() < 1e-9 && (p[1][1] - 1.0).abs() < 1e-9);
        assert!(p[0][1].abs() < 1e-9 && p[1][0].abs() < 1e-9);
        // MUNIT identity.
        assert_eq!(
            nums(&eval_array("MUNIT(2)", &g)),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );
        // 2×2 · 2×1 = 2×1: [[1·1+2·3],[3·1+4·3]] = [[7],[15]].
        assert_eq!(
            nums(&eval_array("MMULT(A1:B2,A1:A2)", &g)),
            vec![vec![7.0], vec![15.0]]
        );
    }
}
