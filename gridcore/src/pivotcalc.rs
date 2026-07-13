//! Pivot **calculated fields**: a small expression language over field names.
//!
//! Excel calculated-field formulas (e.g. `'Sales' - 'Cost'`, `SUM(a, b)`,
//! `IF(x>0, x, 0)`) operate on the **group sum** of each referenced field within
//! a pivot cell. References to other calculated fields are inlined (recursively),
//! so the parsed [`CalcExpr`] only ever names base-field columns.

use crate::formula::{ExcelError, Value};

/// A parsed calculated-field expression; field references are base-field columns.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcExpr {
    Num(f64),
    /// The group sum of a base field column.
    Field(usize),
    Neg(Box<CalcExpr>),
    Bin(char, Box<CalcExpr>, Box<CalcExpr>),
    /// A comparison (`>`, `<`, `=`, `>=`, `<=`, `<>`) yielding a boolean.
    Cmp(String, Box<CalcExpr>, Box<CalcExpr>),
    /// `SUM(a, b, …)` — the sum of its arguments (each a field group-sum or expr).
    Sum(Vec<CalcExpr>),
    If(Box<CalcExpr>, Box<CalcExpr>, Box<CalcExpr>),
}

/// Parse a calculated-field formula. `base` resolves a field name to its base
/// column; `calc` resolves it to another calculated field's formula (inlined).
/// Returns `None` on unknown names, cycles, unsupported functions, or syntax we
/// don't model — the caller then leaves the pivot on its cached values.
pub fn parse(
    formula: &str,
    base: &impl Fn(&str) -> Option<usize>,
    calc: &impl Fn(&str) -> Option<String>,
) -> Option<CalcExpr> {
    let mut visiting = Vec::new();
    parse_inner(formula, base, calc, &mut visiting)
}

fn parse_inner(
    formula: &str,
    base: &impl Fn(&str) -> Option<usize>,
    calc: &impl Fn(&str) -> Option<String>,
    visiting: &mut Vec<String>,
) -> Option<CalcExpr> {
    let tokens = tokenize(formula)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        base,
        calc,
        visiting,
    };
    let e = p.expr()?;
    if p.pos == p.tokens.len() {
        Some(e)
    } else {
        None // trailing garbage
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(String),
    LParen,
    RParen,
    Comma,
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '\'' {
            // A single-quoted field name; '' is an escaped quote.
            let mut name = String::new();
            i += 1;
            while i < cs.len() {
                if cs[i] == '\'' {
                    if i + 1 < cs.len() && cs[i + 1] == '\'' {
                        name.push('\'');
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    name.push(cs[i]);
                    i += 1;
                }
            }
            out.push(Tok::Ident(name));
        } else if c.is_ascii_digit() || (c == '.' && i + 1 < cs.len() && cs[i + 1].is_ascii_digit())
        {
            let start = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            out.push(Tok::Num(cs[start..i].iter().collect::<String>().parse().ok()?));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_' || cs[i] == '.') {
                i += 1;
            }
            out.push(Tok::Ident(cs[start..i].iter().collect()));
        } else {
            match c {
                '(' => out.push(Tok::LParen),
                ')' => out.push(Tok::RParen),
                ',' => out.push(Tok::Comma),
                '+' | '-' | '*' | '/' | '^' | '&' => out.push(Tok::Op(c.to_string())),
                '>' | '<' | '=' => {
                    // Two-char comparisons: >=, <=, <>.
                    let mut op = c.to_string();
                    if (c == '>' || c == '<') && i + 1 < cs.len() && (cs[i + 1] == '=' || cs[i + 1] == '>')
                    {
                        op.push(cs[i + 1]);
                        i += 1;
                    }
                    out.push(Tok::Op(op));
                }
                _ => return None, // unsupported character
            }
            i += 1;
        }
    }
    Some(out)
}

struct Parser<'a, B, C> {
    tokens: Vec<Tok>,
    pos: usize,
    base: &'a B,
    calc: &'a C,
    visiting: &'a mut Vec<String>,
}

impl<B, C> Parser<'_, B, C>
where
    B: Fn(&str) -> Option<usize>,
    C: Fn(&str) -> Option<String>,
{
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn eat_op(&mut self, ops: &[&str]) -> Option<String> {
        if let Some(Tok::Op(o)) = self.peek() {
            if ops.contains(&o.as_str()) {
                let o = o.clone();
                self.pos += 1;
                return Some(o);
            }
        }
        None
    }

    // comparison < add/sub < mul/div < pow < unary < atom
    fn expr(&mut self) -> Option<CalcExpr> {
        let l = self.additive()?;
        if let Some(op) = self.eat_op(&[">", "<", "=", ">=", "<=", "<>"]) {
            let r = self.additive()?;
            return Some(CalcExpr::Cmp(op, Box::new(l), Box::new(r)));
        }
        Some(l)
    }
    fn additive(&mut self) -> Option<CalcExpr> {
        let mut l = self.mul()?;
        while let Some(op) = self.eat_op(&["+", "-"]) {
            let r = self.mul()?;
            l = CalcExpr::Bin(op.chars().next().unwrap(), Box::new(l), Box::new(r));
        }
        Some(l)
    }
    fn mul(&mut self) -> Option<CalcExpr> {
        let mut l = self.pow()?;
        while let Some(op) = self.eat_op(&["*", "/"]) {
            let r = self.pow()?;
            l = CalcExpr::Bin(op.chars().next().unwrap(), Box::new(l), Box::new(r));
        }
        Some(l)
    }
    fn pow(&mut self) -> Option<CalcExpr> {
        let l = self.unary()?;
        if self.eat_op(&["^"]).is_some() {
            let r = self.pow()?; // right-associative
            return Some(CalcExpr::Bin('^', Box::new(l), Box::new(r)));
        }
        Some(l)
    }
    fn unary(&mut self) -> Option<CalcExpr> {
        if let Some(op) = self.eat_op(&["-", "+"]) {
            let x = self.unary()?;
            return Some(if op == "-" {
                CalcExpr::Neg(Box::new(x))
            } else {
                x
            });
        }
        self.atom()
    }
    fn atom(&mut self) -> Option<CalcExpr> {
        match self.peek()?.clone() {
            Tok::Num(n) => {
                self.pos += 1;
                Some(CalcExpr::Num(n))
            }
            Tok::LParen => {
                self.pos += 1;
                let e = self.expr()?;
                matches!(self.peek(), Some(Tok::RParen)).then_some(())?;
                self.pos += 1;
                Some(e)
            }
            Tok::Ident(name) => {
                self.pos += 1;
                // A function call if followed by '('.
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.pos += 1;
                    let args = self.args()?;
                    return self.function(&name, args);
                }
                self.resolve_field(&name)
            }
            _ => None,
        }
    }
    fn args(&mut self) -> Option<Vec<CalcExpr>> {
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Tok::RParen)) {
            self.pos += 1;
            return Some(args);
        }
        loop {
            args.push(self.expr()?);
            match self.peek() {
                Some(Tok::Comma) => self.pos += 1,
                Some(Tok::RParen) => {
                    self.pos += 1;
                    return Some(args);
                }
                _ => return None,
            }
        }
    }
    fn function(&mut self, name: &str, args: Vec<CalcExpr>) -> Option<CalcExpr> {
        match name.to_ascii_uppercase().as_str() {
            "SUM" if !args.is_empty() => Some(CalcExpr::Sum(args)),
            "IF" if args.len() == 3 => {
                let mut it = args.into_iter();
                Some(CalcExpr::If(
                    Box::new(it.next().unwrap()),
                    Box::new(it.next().unwrap()),
                    Box::new(it.next().unwrap()),
                ))
            }
            _ => None, // unsupported function → whole field unsupported
        }
    }
    fn resolve_field(&mut self, name: &str) -> Option<CalcExpr> {
        if let Some(col) = (self.base)(name) {
            return Some(CalcExpr::Field(col));
        }
        if let Some(formula) = (self.calc)(name) {
            if self.visiting.iter().any(|n| n == name) {
                return None; // cycle
            }
            self.visiting.push(name.to_string());
            let e = parse_inner(&formula, self.base, self.calc, self.visiting);
            self.visiting.pop();
            return e;
        }
        None
    }
}

/// Evaluate a calculated-field expression for one pivot cell. `sum_col(c)`
/// returns the group sum of base column `c`.
pub fn eval(expr: &CalcExpr, sum_col: &impl Fn(usize) -> f64) -> Value {
    fn num(v: Value) -> Result<f64, Value> {
        match v {
            Value::Num(n) => Ok(n),
            Value::Bool(b) => Ok(if b { 1.0 } else { 0.0 }),
            Value::Empty => Ok(0.0),
            e @ Value::Err(_) => Err(e),
            Value::Str(_) => Err(Value::Err(ExcelError::Value)),
        }
    }
    fn go(e: &CalcExpr, sum: &impl Fn(usize) -> f64) -> Value {
        match e {
            CalcExpr::Num(n) => Value::Num(*n),
            CalcExpr::Field(c) => Value::Num(sum(*c)),
            CalcExpr::Neg(x) => match num(go(x, sum)) {
                Ok(n) => Value::Num(-n),
                Err(e) => e,
            },
            CalcExpr::Sum(items) => {
                let mut acc = 0.0;
                for it in items {
                    match num(go(it, sum)) {
                        Ok(n) => acc += n,
                        Err(e) => return e,
                    }
                }
                Value::Num(acc)
            }
            CalcExpr::If(c, a, b) => {
                let cond = match num(go(c, sum)) {
                    Ok(n) => n != 0.0,
                    Err(e) => return e,
                };
                go(if cond { a } else { b }, sum)
            }
            CalcExpr::Cmp(op, l, r) => {
                let (a, b) = match (num(go(l, sum)), num(go(r, sum))) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return e,
                };
                let res = match op.as_str() {
                    ">" => a > b,
                    "<" => a < b,
                    "=" => a == b,
                    ">=" => a >= b,
                    "<=" => a <= b,
                    "<>" => a != b,
                    _ => return Value::Err(ExcelError::Value),
                };
                Value::Bool(res)
            }
            CalcExpr::Bin(op, l, r) => {
                let (a, b) = match (num(go(l, sum)), num(go(r, sum))) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return e,
                };
                match op {
                    '+' => Value::Num(a + b),
                    '-' => Value::Num(a - b),
                    '*' => Value::Num(a * b),
                    '/' => {
                        if b == 0.0 {
                            Value::Err(ExcelError::Div0)
                        } else {
                            Value::Num(a / b)
                        }
                    }
                    '^' => Value::Num(a.powf(b)),
                    _ => Value::Err(ExcelError::Value),
                }
            }
        }
    }
    go(expr, sum_col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(name: &str) -> Option<usize> {
        match name.to_lowercase().as_str() {
            "a" | "field a" => Some(0),
            "b" | "field b" => Some(1),
            _ => None,
        }
    }

    #[test]
    fn parses_and_evaluates_arithmetic() {
        let no_calc = |_: &str| None;
        let e = parse("'Field A' * 20", &cols, &no_calc).unwrap();
        // Group sum of A = 5 → 5*20 = 100.
        assert_eq!(eval(&e, &|c| if c == 0 { 5.0 } else { 0.0 }), Value::Num(100.0));

        let e = parse("SUM(A, B)", &cols, &no_calc).unwrap();
        assert_eq!(eval(&e, &|c| [3.0, 4.0][c]), Value::Num(7.0));

        let e = parse("IF(A > 0, A, 0)", &cols, &no_calc).unwrap();
        assert_eq!(eval(&e, &|_| 9.0), Value::Num(9.0));
        assert_eq!(eval(&e, &|_| -1.0), Value::Num(0.0));

        // Division by zero → #DIV/0!.
        let e = parse("A / 0", &cols, &no_calc).unwrap();
        assert!(matches!(eval(&e, &|_| 5.0), Value::Err(ExcelError::Div0)));
    }

    #[test]
    fn inlines_nested_calc_fields() {
        // Field1 = A*B ; Field2 = Field1 * 2 → A*B*2.
        let calc = |n: &str| match n.to_lowercase().as_str() {
            "field1" => Some("A*B".to_string()),
            _ => None,
        };
        let e = parse("Field1 * 2", &cols, &calc).unwrap();
        // A=2, B=3 → 2*3*2 = 12.
        assert_eq!(eval(&e, &|c| [2.0, 3.0][c]), Value::Num(12.0));

        // A cycle is rejected.
        let cyclic = |n: &str| match n.to_lowercase().as_str() {
            "x" => Some("Y+1".to_string()),
            "y" => Some("X+1".to_string()),
            _ => None,
        };
        assert!(parse("X", &cols, &cyclic).is_none());
    }
}
