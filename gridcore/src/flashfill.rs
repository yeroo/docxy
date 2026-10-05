//! Flash Fill (#666, ENT-104..111): fill the rest of a column from the
//! examples typed at its top, by finding one small program — a sequence of
//! pieces of the adjacent columns' values and constant text — that turns
//! every example row's sources into its example, then running it on the rows
//! below.
//!
//! The search is deterministic and bounded. A piece (`Atom::Sub`) is a
//! token of one source column's shown text (a word, a run of letters, digits
//! or both, a field between one delimiter, the whole value, or the first
//! character of a word or run), counted from the start or the end, in one of
//! four cases; the rest is constant text (`Atom::Const`). Each example gives a graph
//! of the pieces that produce its text at each position, and the program is
//! the cheapest path through all of them at once: fewest constant
//! characters, then fewest atoms, then the plainest token, case and index.
//! Constant characters count first: with one example, `Lovelace, Ada` from
//! `Ada Lovelace` is the last word, `, ` and the first word — not the last
//! word and the constant `, Ada`, which has fewer atoms. A program
//! of constants alone, or none at all, is no pattern (ENT-110).
//!
//! What it returns is the text each empty cell below would be typed with, so
//! a host commits it like a typed entry (ENT-108): digits become numbers,
//! unless every example was entered as text with an apostrophe, in which case
//! each result carries one too.

use std::collections::HashMap;

use crate::entry::{EntryCtx, parse_entry};
use crate::sheet::{CellValue, Sheet, Workbook, Xf, format_with};

/// A Flash Fill the host can apply: the text to type into each filled cell,
/// and the cells the Flash Fill Options count (ENT-109).
#[derive(Clone, Debug, PartialEq)]
pub struct FlashFill {
    /// The target column.
    pub col: u32,
    /// The examples: rows `examples.0..=examples.1` of the column.
    pub examples: (u32, u32),
    /// (row, typed text) for each cell the program filled, top to bottom.
    pub fills: Vec<(u32, String)>,
    /// Rows the program could not fill (a token it reads is missing): left
    /// empty, and counted as Flash Fill's blank cells.
    pub blank: Vec<u32>,
    /// The last row the fill reaches (the adjacent data's last row).
    pub last_row: u32,
}

impl FlashFill {
    /// The rows that changed: Flash Fill's changed cells.
    pub fn changed(&self) -> Vec<u32> {
        self.fills.iter().map(|(r, _)| *r).collect()
    }

    /// The (row, col) of the first and last cell the fill covers (changed
    /// and blank cells alike), for an on-sheet button or a preview range;
    /// `None` when it covers none.
    pub fn span(&self) -> Option<(u32, u32)> {
        let rows = self.changed().into_iter().chain(self.blank.iter().copied());
        let (lo, hi) = rows.fold((u32::MAX, 0), |(lo, hi), r| (lo.min(r), hi.max(r)));
        (lo <= hi).then_some((lo, hi))
    }
}

/// Why Flash Fill filled nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoFill {
    /// The examples show no pattern (ENT-110): the message the hosts show.
    NoPattern,
    /// There is nothing to fill: no example at the top of the column, no
    /// adjacent data, or no empty cell below the examples.
    Nothing,
}

impl NoFill {
    pub fn message(&self) -> &'static str {
        match self {
            NoFill::NoPattern => {
                "Flash Fill didn't see a pattern. If you've entered a few examples, \
                 make sure they follow a consistent pattern, then try again."
            }
            NoFill::Nothing => {
                "Flash Fill needs an example typed at the top of a column next to your data."
            }
        }
    }
}

impl std::fmt::Display for NoFill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for NoFill {}

/// Flash Fill on the column of (row, col) of `sheet` (Ctrl+E, Data › Flash
/// Fill). The examples are the column's non-empty cells from the top of the
/// data down to the first empty one; the sources are the non-empty columns
/// next to it on either side, up to the first empty column; the fill runs to
/// the sources' last row and leaves every cell that already holds a value
/// (ENT-107). Columns only (ENT-111).
pub fn flash_fill(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Result<FlashFill, NoFill> {
    let sh = wb.sheets.get(sheet).ok_or(NoFill::Nothing)?;
    let region = Region::around(sh, row, col).ok_or(NoFill::Nothing)?;
    let text = |r: u32, c: u32| shown(wb, sh, r, c);
    let (top, ex_end) = region.examples(sh);
    let ex_end = ex_end.ok_or(NoFill::Nothing)?;
    let targets: Vec<u32> = (ex_end + 1..=region.bottom)
        .filter(|&r| is_empty(sh, r, col))
        .collect();
    if targets.is_empty() {
        return Err(NoFill::Nothing);
    }
    let examples: Vec<Example> = (top..=ex_end)
        .map(|r| Example {
            sources: region.sources.iter().map(|&c| text(r, c)).collect(),
            output: text(r, col),
        })
        .collect();
    let program = synthesize(&examples).ok_or(NoFill::NoPattern)?;
    let as_text = (top..=ex_end).all(|r| typed_as_text(wb, sh, r, col));
    let mut fill = FlashFill {
        col,
        examples: (top, ex_end),
        fills: Vec::new(),
        blank: Vec::new(),
        last_row: region.bottom,
    };
    for r in targets {
        let sources: Vec<String> = region.sources.iter().map(|&c| text(r, c)).collect();
        match run(&program, &sources) {
            Some(out) if !out.is_empty() => fill.fills.push((r, typed(&out, as_text))),
            _ => fill.blank.push(r),
        }
    }
    if fill.fills.is_empty() {
        return Err(NoFill::NoPattern);
    }
    Ok(fill)
}

/// The automatic Flash Fill preview (ENT-105), after a typed commit at
/// (row, col): only when the column's examples run from the top of the data
/// to `row`, `row` is the second example or later, and every cell below in
/// the data is empty. Nothing else shows a preview.
pub fn flash_preview(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Option<FlashFill> {
    let sh = wb.sheets.get(sheet)?;
    let region = Region::around(sh, row, col)?;
    let (top, ex_end) = region.examples(sh);
    if ex_end != Some(row) || row < top + 1 {
        return None;
    }
    if !(row + 1..=region.bottom).all(|r| is_empty(sh, r, col)) {
        return None;
    }
    let fill = flash_fill(wb, sheet, row, col).ok()?;
    fill.blank.is_empty().then_some(fill)
}

// ---------------------------------------------------------------------------
// The region
// ---------------------------------------------------------------------------

/// The data Flash Fill reads around the target column.
struct Region {
    col: u32,
    /// The first row of the data.
    top: u32,
    /// The last row: the last one with a value in some source column.
    bottom: u32,
    /// The source columns, nearest first on each side (left, then right).
    sources: Vec<u32>,
}

impl Region {
    fn around(sh: &Sheet, row: u32, col: u32) -> Option<Region> {
        let near = |r: u32| {
            [col.checked_sub(1), Some(col), col.checked_add(1)]
                .into_iter()
                .flatten()
                .any(|c| !is_empty(sh, r, c))
        };
        let mut top = row;
        while top > 0 && near(top - 1) {
            top -= 1;
        }
        let mut bottom = row;
        let mut sources = Vec::new();
        // The sources decide the rows and the rows the sources: a few rounds
        // settle it (each round can only grow both).
        for _ in 0..8 {
            let next = Self::sources(sh, col, top, bottom);
            let mut last = top;
            while last < crate::sheet::MAX_ROWS - 1
                && next.iter().any(|&c| !is_empty(sh, last + 1, c))
            {
                last += 1;
            }
            let last = last.max(row);
            if next == sources && last == bottom {
                break;
            }
            sources = next;
            bottom = last;
        }
        (!sources.is_empty()).then_some(Region {
            col,
            top,
            bottom,
            sources,
        })
    }

    /// The non-empty columns next to `col` over rows `top..=bottom`, up to
    /// the first empty column on each side.
    fn sources(sh: &Sheet, col: u32, top: u32, bottom: u32) -> Vec<u32> {
        let filled = |c: u32| (top..=bottom).any(|r| !is_empty(sh, r, c));
        let mut out = Vec::new();
        let mut c = col;
        while c > 0 && filled(c - 1) {
            c -= 1;
            out.push(c);
        }
        let mut c = col;
        while c < crate::sheet::MAX_COLS - 1 && filled(c + 1) {
            c += 1;
            out.push(c);
        }
        out
    }

    /// The examples: from the top, the column's run of non-empty cells
    /// (`None` when the top cell is empty).
    fn examples(&self, sh: &Sheet) -> (u32, Option<u32>) {
        let mut end = None;
        let mut r = self.top;
        while r <= self.bottom && !is_empty(sh, r, self.col) {
            end = Some(r);
            r += 1;
        }
        (self.top, end)
    }
}

fn is_empty(sh: &Sheet, r: u32, c: u32) -> bool {
    sh.cell(r, c)
        .is_none_or(|cell| cell.value.is_empty() && cell.formula.is_none())
}

/// A cell's text as shown: what Flash Fill reads and matches.
fn shown(wb: &Workbook, sh: &Sheet, r: u32, c: u32) -> String {
    let Some(cell) = sh.cell(r, c) else {
        return String::new();
    };
    match &cell.value {
        CellValue::Text(t) => t.clone(),
        CellValue::Empty => String::new(),
        v => format_with(&wb.styles.xf(cell.style), v, wb.date1904),
    }
}

/// Was the example at (r, c) entered as text that typing would read as
/// something else (`'007`)? Then the results are text too (ENT-108).
fn typed_as_text(wb: &Workbook, sh: &Sheet, r: u32, c: u32) -> bool {
    let Some(cell) = sh.cell(r, c) else {
        return false;
    };
    let CellValue::Text(t) = &cell.value else {
        return false;
    };
    let xf = wb.styles.xf(cell.style);
    xf.quote_prefix || crate::entry::is_text(&xf) || !stays_text(t)
}

fn stays_text(text: &str) -> bool {
    let ctx = EntryCtx {
        today: Some(45_000.0),
        ..EntryCtx::default()
    };
    parse_entry(text, &Xf::default(), &ctx)
        .is_ok_and(|e| e.cell.formula.is_none() && e.cell.value == CellValue::Text(text.into()))
}

/// The text a host types for a result: with an apostrophe when the examples
/// were text, or when typing it bare would make a formula — Flash Fill writes
/// constants (ENT-106).
fn typed(out: &str, as_text: bool) -> String {
    let formula = matches!(out.chars().next(), Some('=' | '\''))
        || ((out.starts_with(['+', '-', '@'])) && !stays_text(out) && !is_value(out));
    if as_text || formula {
        format!("'{out}")
    } else {
        out.to_string()
    }
}

/// Does `text` type as a number, date, logical or error (not a formula)?
fn is_value(text: &str) -> bool {
    parse_entry(text, &Xf::default(), &EntryCtx::default())
        .is_ok_and(|e| e.cell.formula.is_none() && !matches!(e.cell.value, CellValue::Text(_)))
}

// ---------------------------------------------------------------------------
// The program
// ---------------------------------------------------------------------------

/// What a [`Atom::Sub`] cuts its source into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Token {
    /// Whitespace-separated words.
    Word,
    /// Fields between one delimiter (empty fields count).
    Field(char),
    /// Maximal runs of letters.
    Letters,
    /// Maximal runs of digits.
    Digits,
    /// Maximal runs of letters and digits.
    Alnum,
    /// The whole value.
    Whole,
    /// The first character of a word.
    InitialWord,
    /// The first character of a letters run.
    InitialLetters,
}

const DELIMS: [char; 8] = ['-', '_', ',', '.', '/', '@', ';', ':'];

impl Token {
    /// The plainer the token, the lower: word, field, run, whole.
    fn rank(self) -> u32 {
        match self {
            Token::Word => 0,
            Token::Field(_) => 1,
            Token::InitialWord => 1,
            Token::Letters | Token::Digits | Token::Alnum | Token::InitialLetters => 2,
            Token::Whole => 3,
        }
    }

    fn pieces(self, s: &str) -> Vec<String> {
        let runs = |keep: fn(char) -> bool| -> Vec<String> {
            let mut out = Vec::new();
            let mut cur = String::new();
            for ch in s.chars() {
                if keep(ch) {
                    cur.push(ch);
                } else if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            if !cur.is_empty() {
                out.push(cur);
            }
            out
        };
        let initials = |words: Vec<String>| {
            words
                .iter()
                .filter_map(|w| w.chars().next().map(String::from))
                .collect()
        };
        match self {
            Token::Word => s.split_whitespace().map(str::to_string).collect(),
            Token::Field(d) if s.contains(d) => s.split(d).map(str::to_string).collect(),
            Token::Field(_) => Vec::new(),
            Token::Letters => runs(char::is_alphabetic),
            Token::Digits => runs(|c| c.is_ascii_digit()),
            Token::Alnum => runs(char::is_alphanumeric),
            Token::Whole if !s.is_empty() => vec![s.to_string()],
            Token::Whole => Vec::new(),
            Token::InitialWord => initials(Token::Word.pieces(s)),
            Token::InitialLetters => initials(Token::Letters.pieces(s)),
        }
    }

    fn all() -> impl Iterator<Item = Token> {
        [
            Token::Word,
            Token::Letters,
            Token::Digits,
            Token::Alnum,
            Token::Whole,
            Token::InitialWord,
            Token::InitialLetters,
        ]
        .into_iter()
        .chain(DELIMS.into_iter().map(Token::Field))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Case {
    AsIs,
    Upper,
    Lower,
    /// First letter capital, the rest lowercase.
    Proper,
}

impl Case {
    const ALL: [Case; 4] = [Case::AsIs, Case::Upper, Case::Lower, Case::Proper];

    fn apply(self, s: &str) -> String {
        match self {
            Case::AsIs => s.to_string(),
            Case::Upper => s.to_uppercase(),
            Case::Lower => s.to_lowercase(),
            Case::Proper => {
                let mut cs = s.chars();
                match cs.next() {
                    Some(c) => c
                        .to_uppercase()
                        .chain(cs.flat_map(char::to_lowercase))
                        .collect(),
                    None => String::new(),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Atom {
    Const(String),
    Sub {
        /// Index into the example's sources.
        src: usize,
        token: Token,
        /// 1-based from the start, or -1-based from the end.
        index: i32,
        case: Case,
    },
}

impl Atom {
    fn eval(&self, sources: &[String]) -> Option<String> {
        match self {
            Atom::Const(s) => Some(s.clone()),
            Atom::Sub {
                src,
                token,
                index,
                case,
            } => {
                let pieces = token.pieces(sources.get(*src)?);
                let i = if *index > 0 {
                    usize::try_from(*index - 1).ok()?
                } else {
                    pieces.len().checked_sub(usize::try_from(-*index).ok()?)?
                };
                let piece = pieces.get(i)?;
                (!piece.is_empty()).then(|| case.apply(piece))
            }
        }
    }

    /// (constant chars, atoms, token rank, case rank, index rank): compared
    /// in that order, summed along a program.
    fn cost(&self) -> Cost {
        match self {
            Atom::Const(s) => [s.chars().count() as u32, 1, 0, 0, 0],
            Atom::Sub {
                token, index, case, ..
            } => [
                0,
                1,
                token.rank(),
                u32::from(*case != Case::AsIs),
                u32::from(*index < 0),
            ],
        }
    }
}

type Cost = [u32; 5];

fn add(a: Cost, b: Cost) -> Cost {
    std::array::from_fn(|i| a[i] + b[i])
}

struct Example {
    sources: Vec<String>,
    output: String,
}

fn run(program: &[Atom], sources: &[String]) -> Option<String> {
    program.iter().map(|a| a.eval(sources)).collect()
}

/// Every Sub atom that produces a non-empty text in `sources`, with that
/// text.
fn subs(sources: &[String]) -> Vec<(Atom, String)> {
    let mut out = Vec::new();
    for (src, s) in sources.iter().enumerate() {
        for token in Token::all() {
            let pieces = token.pieces(s);
            let n = pieces.len() as i32;
            for (i, piece) in pieces.iter().enumerate() {
                if piece.is_empty() {
                    continue;
                }
                let i = i as i32;
                for index in [i + 1, i - n] {
                    for case in Case::ALL {
                        out.push((
                            Atom::Sub {
                                src,
                                token,
                                index,
                                case,
                            },
                            case.apply(piece),
                        ));
                    }
                }
            }
        }
    }
    out
}

/// The edges of one example's graph: from each char position of its output,
/// the atoms that produce the text starting there and the position after it.
/// Constant atoms run from a position to the start of a Sub or to the end,
/// never a char at a time, so the graphs stay small.
fn graph(ex: &Example) -> Vec<Vec<(Atom, usize)>> {
    let out: Vec<char> = ex.output.chars().collect();
    let n = out.len();
    let mut edges: Vec<Vec<(Atom, usize)>> = vec![Vec::new(); n + 1];
    let subs = subs(&ex.sources);
    let mut starts = vec![false; n + 1];
    starts[n] = true;
    for p in 0..n {
        let rest: String = out[p..].iter().collect();
        for (atom, text) in &subs {
            if rest.starts_with(text.as_str()) {
                edges[p].push((atom.clone(), p + text.chars().count()));
                starts[p] = true;
            }
        }
    }
    for p in 0..n {
        for q in p + 1..=n {
            if starts[q] {
                edges[p].push((Atom::Const(out[p..q].iter().collect()), q));
            }
        }
    }
    edges
}

/// The longest example or source text the search reads, in chars: the graphs
/// grow with the square of an example's length, and a pattern in values
/// longer than this is not what Flash Fill is for.
const MAX_LEN: usize = 1000;

/// The cheapest program every example's graph has a path for, holding at
/// least one Sub; `None` when there is none.
fn synthesize(examples: &[Example]) -> Option<Vec<Atom>> {
    if examples.is_empty() || examples.iter().any(|e| e.output.is_empty()) {
        return None;
    }
    let long = |s: &String| s.chars().count() > MAX_LEN;
    if examples
        .iter()
        .any(|e| long(&e.output) || e.sources.iter().any(long))
    {
        return None;
    }
    let graphs: Vec<_> = examples.iter().map(graph).collect();
    let ends: Vec<usize> = examples.iter().map(|e| e.output.chars().count()).collect();
    let mut memo = HashMap::new();
    best(&graphs, &ends, vec![0; examples.len()], false, &mut memo).map(|(_, p)| p)
}

type Memo = HashMap<(Vec<usize>, bool), Option<(Cost, Vec<Atom>)>>;

/// The cheapest program from positions `at` (one per example) to the ends,
/// `has_sub` telling whether the program so far has a Sub.
fn best(
    graphs: &[Vec<Vec<(Atom, usize)>>],
    ends: &[usize],
    at: Vec<usize>,
    has_sub: bool,
    memo: &mut Memo,
) -> Option<(Cost, Vec<Atom>)> {
    if at == ends {
        return has_sub.then(|| ([0; 5], Vec::new()));
    }
    let key = (at.clone(), has_sub);
    if let Some(hit) = memo.get(&key) {
        return hit.clone();
    }
    let mut found: Option<(Cost, Vec<Atom>)> = None;
    // An atom moves every example at once; the first graph proposes, the
    // others must have the same atom at their position.
    for (atom, next0) in &graphs[0][at[0]] {
        let mut next = vec![*next0];
        let fits = graphs[1..].iter().zip(&at[1..]).all(|(g, &p)| {
            match g[p].iter().find(|(a, _)| a == atom) {
                Some((_, q)) => {
                    next.push(*q);
                    true
                }
                None => false,
            }
        });
        if !fits {
            continue;
        }
        let sub = has_sub || matches!(atom, Atom::Sub { .. });
        if let Some((cost, rest)) = best(graphs, ends, next, sub, memo) {
            let cost = add(atom.cost(), cost);
            if found.as_ref().is_none_or(|(c, _)| cost < *c) {
                let mut prog = vec![atom.clone()];
                prog.extend(rest);
                found = Some((cost, prog));
            }
        }
    }
    memo.insert(key, found.clone());
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Cell;

    fn wb_with(cols: &[&[&str]]) -> Workbook {
        let mut sh = Sheet::default();
        for (c, col) in cols.iter().enumerate() {
            for (r, v) in col.iter().enumerate() {
                if !v.is_empty() {
                    sh.set_cell(r as u32, c as u32, Cell::text(v));
                }
            }
        }
        Workbook {
            sheets: vec![sh],
            ..Workbook::default()
        }
    }

    const NAMES: [&str; 6] = [
        "Ada Lovelace",
        "Alan Turing",
        "Grace Hopper",
        "Edsger Dijkstra",
        "Barbara Liskov",
        "Donald Knuth",
    ];

    fn filled(f: &FlashFill) -> Vec<(u32, &str)> {
        f.fills.iter().map(|(r, t)| (*r, t.as_str())).collect()
    }

    /// Type each fill the way a host does.
    fn apply(wb: &mut Workbook, f: &FlashFill) {
        for (r, t) in &f.fills {
            let cell = crate::entry::entry_cell(wb, 0, *r, f.col, t, None).unwrap();
            wb.sheets[0].set_cell(*r, f.col, cell);
        }
    }

    #[test]
    fn ent_case_038_extracts_first_names_and_keeps_a_typed_value() {
        // B1 `Ada`, B5 already `Babs`; Ctrl+E on B2 (Enter moved there).
        let mut wb = wb_with(&[
            &NAMES,
            &["Ada", "", "", "", "Babs"],
            &["x-007", "y-042", "z-100"],
        ]);
        let f = flash_fill(&wb, 0, 1, 1).unwrap();
        assert_eq!(
            filled(&f),
            [(1, "Alan"), (2, "Grace"), (3, "Edsger"), (5, "Donald")]
        );
        assert!(f.blank.is_empty());
        apply(&mut wb, &f);
        let sh = &wb.sheets[0];
        assert_eq!(sh.cell(4, 1).unwrap().value, CellValue::Text("Babs".into()));
        assert!(sh.cell(1, 1).unwrap().formula.is_none(), "a constant");
        // From the example's own cell too.
        assert_eq!(
            flash_fill(&wb_with(&[&NAMES, &["Ada"]]), 0, 0, 1)
                .unwrap()
                .fills
                .len(),
            5
        );
    }

    #[test]
    fn ent_case_038_a_text_example_gives_text_results() {
        let mut wb = wb_with(&[&NAMES, &["Ada"], &["x-007", "y-042", "z-100"]]);
        // D1 `'007`: text with its apostrophe.
        let cell = crate::entry::entry_cell(&mut wb, 0, 0, 3, "'007", None).unwrap();
        wb.sheets[0].set_cell(0, 3, cell);
        let f = flash_fill(&wb, 0, 0, 3).unwrap();
        assert_eq!(filled(&f), [(1, "'042"), (2, "'100")]);
        // Sources A:C reach row 6; C4:C6 are empty, so D4:D6 are blank.
        assert_eq!(f.blank, [3, 4, 5]);
        apply(&mut wb, &f);
        assert_eq!(
            wb.sheets[0].cell(1, 3).unwrap().value,
            CellValue::Text("042".into())
        );
        // A number example gives numbers.
        let mut wb = wb_with(&[&["x-7", "y-042", "z-100"]]);
        wb.sheets[0].set_cell(0, 1, Cell::number(7.0));
        let f = flash_fill(&wb, 0, 0, 1).unwrap();
        assert_eq!(filled(&f), [(1, "042"), (2, "100")]);
        apply(&mut wb, &f);
        assert_eq!(
            wb.sheets[0].cell(1, 1).unwrap().value,
            CellValue::Number(42.0)
        );
    }

    #[test]
    fn ent_case_039_preview_after_the_second_example() {
        let mut wb = wb_with(&[&NAMES, &["Ada"]]);
        assert_eq!(flash_preview(&wb, 0, 0, 1), None, "one example");
        wb.sheets[0].set_cell(1, 1, Cell::text("Alan"));
        let f = flash_preview(&wb, 0, 1, 1).expect("a preview");
        assert_eq!(
            filled(&f),
            [(2, "Grace"), (3, "Edsger"), (4, "Barbara"), (5, "Donald")]
        );
        assert_eq!(f.changed(), [2, 3, 4, 5]);
        assert!(f.blank.is_empty());
        assert_eq!(f.span(), Some((2, 5)));
        // Not for a commit above the last example, or with values below.
        assert_eq!(flash_preview(&wb, 0, 0, 1), None);
        wb.sheets[0].set_cell(4, 1, Cell::text("Babs"));
        assert_eq!(flash_preview(&wb, 0, 1, 1), None);
    }

    #[test]
    fn ent_case_039_no_pattern() {
        // E1 `zzz` next to D1:D3 and A:C.
        let wb = wb_with(&[
            &NAMES,
            &["Ada", "Alan", "Grace", "Edsger", "Barbara", "Donald"],
            &["x-007", "y-042", "z-100"],
            &["007", "042", "100"],
            &["zzz"],
        ]);
        assert_eq!(flash_fill(&wb, 0, 0, 4), Err(NoFill::NoPattern));
        assert_eq!(flash_fill(&wb, 0, 1, 4), Err(NoFill::NoPattern));
        assert!(
            NoFill::NoPattern
                .message()
                .starts_with("Flash Fill didn't see a pattern")
        );
        // Nothing to fill: no data next to the column, or no example.
        let wb = wb_with(&[&["a"], &[], &["x"]]);
        assert_eq!(flash_fill(&wb, 0, 0, 2), Err(NoFill::Nothing));
        let wb = wb_with(&[&NAMES]);
        assert_eq!(flash_fill(&wb, 0, 0, 1), Err(NoFill::Nothing));
    }

    #[test]
    fn joins_cases_initials_and_fields() {
        let cases: &[(&[&str], &str, &str)] = &[
            (&["Ada Lovelace"], "Lovelace, Ada", "Turing, Alan"),
            (&["Ada Lovelace"], "ADA", "ALAN"),
            (&["Ada Lovelace"], "AL", "AT"),
            (&["Ada Lovelace"], "ada.lovelace", "alan.turing"),
            (&["ada@example.com"], "ada", "alan"),
        ];
        for (src, example, want) in cases {
            let second = match src[0] {
                "ada@example.com" => "alan@example.com",
                _ => "Alan Turing",
            };
            let wb = wb_with(&[&[src[0], second], &[example]]);
            let f = flash_fill(&wb, 0, 0, 1).unwrap_or_else(|e| panic!("{example}: {e}"));
            assert_eq!(filled(&f), [(1, *want)], "{example}");
        }
        // Two source columns joined.
        let wb = wb_with(&[&["Ada", "Alan"], &["Lovelace", "Turing"], &["Ada L."]]);
        let f = flash_fill(&wb, 0, 0, 2).unwrap();
        assert_eq!(filled(&f), [(1, "Alan T.")]);
    }

    #[test]
    fn a_second_example_settles_an_ambiguous_first() {
        // From `Ada Lovelace` alone, `Lovelace` is the second word; with a
        // three-word name as the second example, it is the last.
        let wb = wb_with(&[
            &["Ada Lovelace", "Mary Ann Evans", "Alan Turing"],
            &["Lovelace", "Evans"],
        ]);
        let f = flash_fill(&wb, 0, 1, 1).unwrap();
        assert_eq!(filled(&f), [(2, "Turing")]);
    }

    #[test]
    fn results_are_constants_even_when_they_look_like_formulas() {
        assert_eq!(typed("=x", false), "'=x");
        assert_eq!(typed("-5", false), "-5");
        assert_eq!(typed("+a", false), "'+a");
        assert_eq!(typed("Ada", false), "Ada");
        assert_eq!(typed("007", true), "'007");
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use crate::sheet::Cell;

    #[test]
    fn long_examples_stay_fast() {
        let mut sh = Sheet::default();
        for r in 0..2000u32 {
            let a = format!("Customer {r} - North-West region, account {r}-{}", r * 7);
            let b = format!("ref/{r}/2024:Q{} id_{r}@example.com", r % 4 + 1);
            sh.set_cell(r, 0, Cell::text(&a));
            sh.set_cell(r, 1, Cell::text(&b));
        }
        for r in 0..3u32 {
            let out = format!(
                "{} | Q{} | id_{r}@example.com | North-West region account {r}",
                r * 7,
                r % 4 + 1
            );
            sh.set_cell(r, 2, Cell::text(&out));
        }
        let wb = Workbook {
            sheets: vec![sh],
            ..Workbook::default()
        };
        let t = std::time::Instant::now();
        let f = flash_fill(&wb, 0, 3, 2);
        let took = t.elapsed();
        assert!(took.as_secs_f64() < 2.0, "{took:?}");
        assert_eq!(
            f.unwrap().fills[0].1,
            "21 | Q4 | id_3@example.com | North-West region account 3"
        );
    }

    #[test]
    fn values_past_the_length_cap_show_no_pattern() {
        let long = "x".repeat(MAX_LEN + 1);
        let mut sh = Sheet::default();
        sh.set_cell(0, 0, Cell::text(&long));
        sh.set_cell(1, 0, Cell::text("y"));
        sh.set_cell(0, 1, Cell::text(&long));
        let wb = Workbook {
            sheets: vec![sh],
            ..Workbook::default()
        };
        assert_eq!(flash_fill(&wb, 0, 0, 1), Err(NoFill::NoPattern));
    }
}
