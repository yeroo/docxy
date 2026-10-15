//! Evaluate Formula (#675): a formula taken apart in the order Excel
//! calculates it, one step at a time.
//!
//! An [`Evaluation`] holds the formula's expression tree and, each step,
//! replaces the next part to calculate by its value: the leftmost, innermost
//! part whose arguments are values already. A reference or a range is such a
//! part, so is a function call or an operator whose operands are values. The
//! part to calculate next is shown underlined in the formula as it stands
//! ([`View::underline`]).
//!
//! `IF`, `IFS`, `IFERROR`, `IFNA` and `CHOOSE` calculate only what they
//! choose: their condition first, then the branch it picks, so a branch the
//! formula does not take is never calculated. A call that binds names
//! (`LET`, `LAMBDA`, `MAP`, ...) is one step, as is a call of a lambda.
//! A reference to a cell that holds a formula can be stepped into: that
//! formula is then evaluated in its own right, and stepping out puts its
//! result where the reference was.
//!
//! The values come from the workbook as it stands, so a volatile function may
//! give another result than the cell shows ([`View::volatile`]).

use std::ops::Range;

use crate::engine::eval_expr_at;
use crate::formula::{self, DynResult, Expr, UnOp, Value};
use crate::sheet::{Workbook, col_name};

/// Calls that bind names, or are lambdas themselves: calculated whole.
const ATOMIC: [&str; 8] = [
    "LET",
    "LAMBDA",
    "MAP",
    "REDUCE",
    "SCAN",
    "BYROW",
    "BYCOL",
    "MAKEARRAY",
];

/// Calls that calculate their first argument and then pick what else to.
const LAZY: [&str; 5] = ["IF", "IFS", "IFERROR", "IFNA", "CHOOSE"];

/// One formula being evaluated: the cell it is in and what is left of it.
#[derive(Clone, Debug)]
struct Frame {
    sheet: usize,
    row: u32,
    col: u32,
    /// The formula as parsed, to start over from.
    first: Expr,
    expr: Expr,
}

/// A formula evaluated step by step, with the formulas stepped into on top.
#[derive(Clone, Debug)]
pub struct Evaluation {
    frames: Vec<Frame>,
    clock: Option<f64>,
    volatile: bool,
    constant: bool,
}

/// What the Evaluate Formula dialog shows of an [`Evaluation`].
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// The cell being evaluated, as `Sheet1!$A$1`.
    pub reference: String,
    /// The formula as it stands, with its `=`; just the value once finished.
    pub text: String,
    /// The part to calculate next, as a byte range of `text`; `None` once the
    /// formula is down to its value.
    pub underline: Option<Range<usize>>,
    /// The formula's value when there is nothing left to calculate.
    pub result: Option<Value>,
    /// The part to calculate next is a reference to a formula cell, which
    /// Step In shows.
    pub step_in: bool,
    /// A formula was stepped into, and Step Out finishes it.
    pub step_out: bool,
    /// The formula calls a volatile function.
    pub volatile: bool,
    /// The cell holds a constant, not a formula.
    pub constant: bool,
}

impl Evaluation {
    /// Start on the cell at (`sheet`, `row`, `col`). A constant gives an
    /// evaluation that is finished from the start. `Err` for a formula that
    /// does not parse.
    pub fn new(
        wb: &Workbook,
        sheet: usize,
        row: u32,
        col: u32,
        clock: Option<f64>,
    ) -> Result<Self, String> {
        let cell = wb.sheets.get(sheet).and_then(|s| s.cell(row, col));
        let (expr, constant) = match cell.and_then(|c| c.formula.as_deref()) {
            Some(src) => (
                formula::parse(src).map_err(|e| format!("The formula does not parse: {e}"))?,
                false,
            ),
            None => (
                Expr::Lit(cell.map_or(Value::Empty, |c| crate::engine::cell_to_value(&c.value))),
                true,
            ),
        };
        let volatile = formula::is_volatile(&expr);
        Ok(Evaluation {
            frames: vec![Frame {
                sheet,
                row,
                col,
                first: expr.clone(),
                expr,
            }],
            clock,
            volatile,
            constant,
        })
    }

    /// Start over on the cell the evaluation began with.
    pub fn restart(&mut self) {
        self.frames.truncate(1);
        let f = &mut self.frames[0];
        f.expr = f.first.clone();
    }

    fn top(&self) -> &Frame {
        self.frames.last().expect("an evaluation has a frame")
    }

    /// What the dialog shows now.
    pub fn view(&self, wb: &Workbook) -> View {
        let f = self.top();
        let shown = blank_shown(&f.expr);
        let path = next(&f.expr);
        let result = path.is_none().then(|| match &f.expr {
            Expr::Lit(v) => v.clone(),
            other => value_of(other),
        });
        let (mut text, mut underline) = match &path {
            Some(p) => {
                let node = get(&shown, p);
                formula::to_string_marked(&shown, node)
            }
            None => (formula::to_string(&shown), None),
        };
        // A finished evaluation is the value itself; a formula has its `=`.
        if path.is_some() && !self.constant {
            text.insert(0, '=');
            underline = underline.map(|r| r.start + 1..r.end + 1);
        }
        let step_in = path
            .as_ref()
            .is_some_and(|p| self.step_target(wb, &get(&f.expr, p).clone()).is_some());
        View {
            reference: reference_of(wb, f),
            text,
            underline,
            result,
            step_in,
            step_out: self.frames.len() > 1,
            volatile: self.volatile,
            constant: self.constant,
        }
    }

    /// The cell a reference stands for, when it is a formula's to step into.
    fn step_target(&self, wb: &Workbook, node: &Expr) -> Option<Frame> {
        let Expr::Ref(r) = node else { return None };
        if r.row < 0 || r.col < 0 {
            return None;
        }
        let here = self.top();
        let sheet = match &r.sheet {
            Some(name) => wb.sheet_index(name)?,
            None => here.sheet,
        };
        let src = wb
            .sheets
            .get(sheet)?
            .cell(r.row as u32, r.col as u32)?
            .formula
            .as_deref()?;
        let expr = formula::parse(src).ok()?;
        Some(Frame {
            sheet,
            row: r.row as u32,
            col: r.col as u32,
            first: expr.clone(),
            expr,
        })
    }

    /// Calculate the underlined part. `false` when there is none.
    pub fn evaluate(&mut self, wb: &Workbook) -> bool {
        let clock = self.clock;
        let f = self.frames.last_mut().expect("an evaluation has a frame");
        let Some(path) = next(&f.expr) else {
            return false;
        };
        let node = get(&f.expr, &path).clone();
        let replacement = choose(&node).unwrap_or_else(|| {
            match eval_expr_at(wb, f.sheet, f.row, f.col, &node, clock) {
                DynResult::Scalar(v) => Expr::Lit(v),
                DynResult::Array(m) => Expr::ArrayLit(
                    m.into_iter()
                        .map(|row| row.into_iter().map(Expr::Lit).collect())
                        .collect(),
                ),
            }
        });
        put(&mut f.expr, &path, replacement);
        true
    }

    /// Show the formula of the cell the underlined reference names. `false`
    /// when the part to calculate next is not a reference to a formula cell.
    pub fn step_in(&mut self, wb: &Workbook) -> bool {
        let f = self.top();
        let Some(path) = next(&f.expr) else {
            return false;
        };
        let Some(frame) = self.step_target(wb, &get(&f.expr, &path).clone()) else {
            return false;
        };
        self.volatile |= formula::is_volatile(&frame.first);
        self.frames.push(frame);
        true
    }

    /// Finish the formula stepped into and put its value where the reference
    /// to it was. `false` when none was stepped into.
    pub fn step_out(&mut self, wb: &Workbook) -> bool {
        if self.frames.len() < 2 {
            return false;
        }
        while self.evaluate(wb) {}
        let done = self.frames.pop().expect("a stepped-into frame");
        let value = done.expr;
        let f = self.frames.last_mut().expect("the frame stepped from");
        if let Some(path) = next(&f.expr) {
            put(&mut f.expr, &path, value);
        }
        true
    }
}

/// `Sheet1!$A$1`, with the sheet's name quoted when it needs it.
fn reference_of(wb: &Workbook, f: &Frame) -> String {
    let name = wb.sheets.get(f.sheet).map_or("", |s| s.name.as_str());
    let plain = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    let name = if plain {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    };
    format!("{name}!${}${}", col_name(f.col), f.row + 1)
}

/// The expression with a calculated blank shown as `(blank)`, not as nothing.
fn blank_shown(e: &Expr) -> Expr {
    let mut out = e.clone();
    walk_mut(&mut out, &mut |x| {
        if matches!(x, Expr::Lit(Value::Empty)) {
            *x = Expr::Name("(blank)".into());
        }
    });
    out
}

fn walk_mut(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(e);
    for i in 0..child_count(e) {
        if let Some(c) = child_mut(e, i) {
            walk_mut(c, f);
        }
    }
}

/// A value an expression that is all values stands for (an array holds its
/// first element).
fn value_of(e: &Expr) -> Value {
    match e {
        Expr::Lit(v) => v.clone(),
        Expr::Num(n) => Value::Num(*n),
        Expr::Str(s) => Value::Str(s.clone()),
        Expr::Bool(b) => Value::Bool(*b),
        Expr::Err(x) => Value::Err(*x),
        Expr::Un(UnOp::Neg, x) => match value_of(x) {
            Value::Num(n) => Value::Num(-n),
            other => other,
        },
        Expr::ArrayLit(rows) => rows
            .first()
            .and_then(|r| r.first())
            .map_or(Value::Empty, value_of),
        _ => Value::Empty,
    }
}

/// Whether nothing is left to calculate in `e`.
fn is_value(e: &Expr) -> bool {
    match e {
        Expr::Num(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Err(_)
        | Expr::Missing
        | Expr::Lit(_) => true,
        Expr::Un(UnOp::Neg | UnOp::Pos, x) => matches!(**x, Expr::Num(_) | Expr::Lit(_)),
        Expr::ArrayLit(rows) => rows.iter().flatten().all(is_value),
        _ => false,
    }
}

fn atomic(e: &Expr) -> bool {
    match e {
        Expr::Func(name, _) => ATOMIC.iter().any(|a| a.eq_ignore_ascii_case(name)),
        Expr::Call(..) => true,
        _ => false,
    }
}

fn lazy(e: &Expr) -> bool {
    matches!(e, Expr::Func(name, args) if !args.is_empty()
        && LAZY.iter().any(|l| l.eq_ignore_ascii_case(name)))
}

fn child_count(e: &Expr) -> usize {
    match e {
        Expr::Func(_, args) => args.len(),
        Expr::Call(_, args) => args.len() + 1,
        Expr::Un(_, _) => 1,
        Expr::Bin(..) => 2,
        Expr::ArrayLit(rows) => rows.iter().map(Vec::len).sum(),
        _ => 0,
    }
}

fn child(e: &Expr, i: usize) -> Option<&Expr> {
    match e {
        Expr::Func(_, args) => args.get(i),
        Expr::Call(callee, args) => {
            if i == 0 {
                Some(callee)
            } else {
                args.get(i - 1)
            }
        }
        Expr::Un(_, x) => (i == 0).then_some(&**x),
        Expr::Bin(_, l, r) => match i {
            0 => Some(l),
            1 => Some(r),
            _ => None,
        },
        Expr::ArrayLit(rows) => rows.iter().flatten().nth(i),
        _ => None,
    }
}

fn child_mut(e: &mut Expr, i: usize) -> Option<&mut Expr> {
    match e {
        Expr::Func(_, args) => args.get_mut(i),
        Expr::Call(callee, args) => {
            if i == 0 {
                Some(callee)
            } else {
                args.get_mut(i - 1)
            }
        }
        Expr::Un(_, x) => (i == 0).then_some(&mut **x),
        Expr::Bin(_, l, r) => match i {
            0 => Some(l),
            1 => Some(r),
            _ => None,
        },
        Expr::ArrayLit(rows) => rows.iter_mut().flatten().nth(i),
        _ => None,
    }
}

fn get<'a>(e: &'a Expr, path: &[usize]) -> &'a Expr {
    path.iter().fold(e, |e, &i| child(e, i).unwrap_or(e))
}

fn put(e: &mut Expr, path: &[usize], with: Expr) {
    match path.split_first() {
        None => *e = with,
        Some((&i, rest)) => {
            if let Some(c) = child_mut(e, i) {
                put(c, rest, with);
            }
        }
    }
}

/// The path to the part to calculate next, `None` when `e` is a value.
fn next(e: &Expr) -> Option<Vec<usize>> {
    if is_value(e) {
        return None;
    }
    if atomic(e) {
        return Some(Vec::new());
    }
    if lazy(e) {
        // Its first argument first; when that is a value the call itself is
        // the step, which picks what to calculate.
        let first = child(e, 0)?;
        return match next(first) {
            Some(mut p) => {
                p.insert(0, 0);
                Some(p)
            }
            None => Some(Vec::new()),
        };
    }
    for i in 0..child_count(e) {
        if let Some(mut p) = child(e, i).and_then(next) {
            p.insert(0, i);
            return Some(p);
        }
    }
    Some(Vec::new())
}

/// The truth of a value an `IF` condition has come to, or the error it is;
/// `None` for an array of several values, which the call is lifted over.
fn truth(e: &Expr) -> Option<Result<bool, formula::ExcelError>> {
    if matches!(e, Expr::ArrayLit(_)) && !is_scalar_array(e) {
        return None;
    }
    Some(match value_of(e) {
        Value::Err(x) => Err(x),
        v => formula::to_bool(&v),
    })
}

fn is_scalar_array(e: &Expr) -> bool {
    matches!(e, Expr::ArrayLit(rows) if rows.len() == 1 && rows[0].len() == 1)
}

/// What a lazy call comes to once its first argument is a value: the part it
/// picks (still to calculate), or `None` when it is calculated whole.
fn choose(e: &Expr) -> Option<Expr> {
    let Expr::Func(name, args) = e else {
        return None;
    };
    if !lazy(e) {
        return None;
    }
    let first = &args[0];
    let missing = |x: Option<&Expr>, empty: Value| match x {
        None | Some(Expr::Missing) => Expr::Lit(empty),
        Some(x) => x.clone(),
    };
    match name.to_ascii_uppercase().as_str() {
        "IF" => match truth(first)? {
            Ok(true) => Some(missing(args.get(1), Value::Num(0.0))),
            Ok(false) => Some(missing(args.get(2), Value::Bool(false))),
            Err(x) => Some(Expr::Lit(Value::Err(x))),
        },
        "IFS" => match truth(first)? {
            Ok(true) => Some(
                args.get(1)
                    .cloned()
                    .unwrap_or(Expr::Lit(Value::Err(formula::ExcelError::NA))),
            ),
            Ok(false) if args.len() > 2 => Some(Expr::Func(name.clone(), args[2..].to_vec())),
            Ok(false) => Some(Expr::Lit(Value::Err(formula::ExcelError::NA))),
            Err(x) => Some(Expr::Lit(Value::Err(x))),
        },
        "IFERROR" | "IFNA" => {
            let only_na = name.eq_ignore_ascii_case("IFNA");
            match value_of(first) {
                Value::Err(x) if !only_na || x == formula::ExcelError::NA => {
                    Some(missing(args.get(1), Value::Empty))
                }
                _ => Some(first.clone()),
            }
        }
        "CHOOSE" => match value_of(first) {
            Value::Num(n) if n >= 1.0 && (n as usize) < args.len() => {
                Some(args[n as usize].clone())
            }
            Value::Err(x) => Some(Expr::Lit(Value::Err(x))),
            _ => Some(Expr::Lit(Value::Err(formula::ExcelError::Value))),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Cell, Sheet};

    fn wb(cells: &[(&str, Cell)]) -> Workbook {
        let mut sheet = Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        };
        for (name, cell) in cells {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        let mut w = Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        };
        let mut eng = crate::engine::Engine::new(&w);
        eng.recalc_all(&mut w);
        w
    }

    fn start(w: &Workbook, at: &str) -> Evaluation {
        let (r, c) = crate::sheet::parse_cell_name(at).unwrap();
        Evaluation::new(w, 0, r, c, None).unwrap()
    }

    /// The text with the part to calculate next in `[ ]`.
    fn shown(ev: &Evaluation, w: &Workbook) -> String {
        let v = ev.view(w);
        match v.underline {
            Some(r) => format!(
                "{}[{}]{}",
                &v.text[..r.start],
                &v.text[r.clone()],
                &v.text[r.end..]
            ),
            None => v.text,
        }
    }

    /// Every step's text, the first before any step and the last the value.
    fn steps(w: &Workbook, at: &str) -> Vec<String> {
        let mut ev = start(w, at);
        let mut out = vec![shown(&ev, w)];
        while ev.evaluate(w) {
            out.push(shown(&ev, w));
            assert!(out.len() < 50, "a runaway evaluation");
        }
        out
    }

    fn num(n: f64) -> Cell {
        Cell::number(n)
    }

    fn f(src: &str) -> Cell {
        Cell::formula(src)
    }

    #[test]
    fn a_sum_and_a_product_go_in_calculation_order() {
        let w = wb(&[
            ("A1", num(1.0)),
            ("A2", num(2.0)),
            ("B1", f("SUM(A1:A2)*2")),
        ]);
        assert_eq!(
            steps(&w, "B1"),
            ["=SUM([A1:A2])*2", "=[SUM({1;2})]*2", "=[3*2]", "6",]
        );
    }

    #[test]
    fn references_go_left_to_right_and_the_last_step_is_the_value() {
        let w = wb(&[("A1", num(3.0)), ("A2", num(4.0)), ("B1", f("A1+A2*2"))]);
        assert_eq!(
            steps(&w, "B1"),
            ["=[A1]+A2*2", "=3+[A2]*2", "=3+[4*2]", "=[3+8]", "11"]
        );
    }

    #[test]
    fn if_calculates_its_condition_and_then_only_the_branch_it_takes() {
        let w = wb(&[("A1", num(5.0)), ("B1", f("IF(A1>3,A1*2,1/0)"))]);
        assert_eq!(
            steps(&w, "B1"),
            [
                "=IF([A1]>3,A1*2,1/0)",
                "=IF([5>3],A1*2,1/0)",
                "=[IF(TRUE,A1*2,1/0)]",
                "=[A1]*2",
                "=[5*2]",
                "10",
            ]
        );
    }

    #[test]
    fn a_branch_not_taken_is_never_calculated() {
        // 1/0 would be #DIV/0! if calculated; the formula's value is 7.
        let w = wb(&[("A1", num(5.0)), ("B1", f("IF(A1>3,7,1/0)"))]);
        let last = steps(&w, "B1").pop().unwrap();
        assert_eq!(last, "7");
        let mut ev = start(&w, "B1");
        while ev.evaluate(&w) {}
        assert_eq!(ev.view(&w).result, Some(Value::Num(7.0)));
    }

    #[test]
    fn nested_if_and_iferror_and_choose_pick_before_they_calculate() {
        let w = wb(&[
            ("A1", num(2.0)),
            ("B1", f("IF(A1=1,\"one\",IF(A1=2,\"two\",\"many\"))")),
            ("B2", f("IFERROR(1/0,\"oops\")")),
            ("B3", f("CHOOSE(A1,10,20,30)")),
            ("B4", f("IFS(A1=1,\"a\",A1=2,\"b\")")),
        ]);
        let result = |at: &str| {
            let mut ev = start(&w, at);
            while ev.evaluate(&w) {}
            ev.view(&w).result.unwrap()
        };
        assert_eq!(result("B1"), Value::Str("two".into()));
        assert_eq!(result("B2"), Value::Str("oops".into()));
        assert_eq!(result("B3"), Value::Num(20.0));
        assert_eq!(result("B4"), Value::Str("b".into()));
    }

    #[test]
    fn an_error_result_is_shown_as_the_error() {
        let w = wb(&[("A1", num(1.0)), ("B1", f("A1/0"))]);
        let last = steps(&w, "B1").pop().unwrap();
        assert_eq!(last, "#DIV/0!");
        let mut ev = start(&w, "B1");
        while ev.evaluate(&w) {}
        assert_eq!(
            ev.view(&w).result,
            Some(Value::Err(formula::ExcelError::Div0))
        );
    }

    #[test]
    fn a_blank_reference_shows_as_blank_and_calculates_as_nothing() {
        let w = wb(&[("B1", f("A1+1"))]);
        assert_eq!(steps(&w, "B1"), ["=[A1]+1", "=[(blank)+1]", "1"]);
    }

    #[test]
    fn parentheses_stay_outside_the_underlined_part() {
        let w = wb(&[("A1", num(5.0)), ("A2", num(2.0)), ("B1", f("(A1+A2)*3"))]);
        assert_eq!(
            steps(&w, "B1"),
            ["=([A1]+A2)*3", "=(5+[A2])*3", "=([5+2])*3", "=[7*3]", "21"]
        );
    }

    #[test]
    fn a_volatile_function_is_flagged_and_a_constant_is_just_its_value() {
        let w = wb(&[("A1", num(1.0)), ("B1", f("A1+RAND()")), ("C1", num(42.0))]);
        assert!(start(&w, "B1").view(&w).volatile);
        assert!(!start(&w, "A1").view(&w).volatile);
        let c = start(&w, "C1").view(&w);
        assert!(c.constant);
        assert_eq!(c.text, "42");
        assert_eq!(c.result, Some(Value::Num(42.0)));
        assert_eq!(c.underline, None);
    }

    #[test]
    fn step_in_shows_the_referenced_formula() {
        let w = wb(&[("A1", num(2.0)), ("A2", f("A1*10")), ("B1", f("A2+1"))]);
        let mut ev = start(&w, "B1");
        let v = ev.view(&w);
        assert!(v.step_in && !v.step_out, "A2 holds a formula");
        assert_eq!(v.reference, "Sheet1!$B$1");
        assert!(ev.step_in(&w));
        let v = ev.view(&w);
        assert_eq!(v.reference, "Sheet1!$A$2");
        assert_eq!(shown(&ev, &w), "=[A1]*10");
        assert!(v.step_out);
        assert!(!v.step_in, "A1 is a constant: nothing to step into");
    }

    #[test]
    fn step_in_is_offered_only_for_a_reference_to_a_formula() {
        let w = wb(&[("A1", num(2.0)), ("B1", f("A1+1"))]);
        let mut ev = start(&w, "B1");
        assert!(!ev.view(&w).step_in);
        assert!(!ev.step_in(&w));
        assert!(!ev.step_out(&w), "nothing was stepped into");
    }

    #[test]
    fn step_out_finishes_the_inner_formula_and_goes_on_with_its_value() {
        let w = wb(&[("A1", num(2.0)), ("A2", f("A1*10")), ("B1", f("A2+1"))]);
        let mut ev = start(&w, "B1");
        assert!(ev.step_in(&w));
        assert!(ev.step_out(&w));
        assert_eq!(ev.view(&w).reference, "Sheet1!$B$1");
        assert_eq!(shown(&ev, &w), "=[20+1]");
        assert!(ev.evaluate(&w));
        assert_eq!(ev.view(&w).result, Some(Value::Num(21.0)));
    }

    #[test]
    fn restart_goes_back_to_the_formula_as_it_began() {
        let w = wb(&[("A1", num(2.0)), ("A2", f("A1*10")), ("B1", f("A2+1"))]);
        let mut ev = start(&w, "B1");
        let first = shown(&ev, &w);
        ev.step_in(&w);
        ev.evaluate(&w);
        ev.restart();
        assert_eq!(shown(&ev, &w), first);
        assert!(!ev.view(&w).step_out);
    }

    #[test]
    fn a_whole_call_that_binds_names_is_one_step() {
        let w = wb(&[("A1", num(2.0)), ("B1", f("LET(x,A1,x*x)"))]);
        assert_eq!(steps(&w, "B1"), ["=[LET(x,A1,x*x)]", "4"]);
    }

    #[test]
    fn the_marked_text_is_the_printed_text_and_the_mark_the_node() {
        let e = formula::parse("(A1+B1)*SUM(C1:C2)").unwrap();
        let formula::Expr::Bin(_, _, r) = &e else {
            panic!("a product")
        };
        let (text, span) = formula::to_string_marked(&e, r);
        assert_eq!(text, formula::to_string(&e));
        assert_eq!(&text[span.unwrap()], "SUM(C1:C2)");
        let other = formula::parse("1").unwrap();
        assert_eq!(
            formula::to_string_marked(&e, &other).1,
            None,
            "not a node of it"
        );
    }
}
