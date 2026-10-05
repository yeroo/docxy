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

use crate::entry::{parse_entry, probe_ctx, stays_text};
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
/// data down to the first empty one — or from the row below, when the top
/// row is a header (the column's top cell is empty, or its examples show no
/// pattern with it and do without it); the sources are the non-empty columns
/// next to it on either side, up to the first empty column; the fill runs to
/// the sources' last row and leaves every cell that already holds a value
/// (ENT-107). Columns only (ENT-111).
pub fn flash_fill(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Result<FlashFill, NoFill> {
    let sh = wb.sheets.get(sheet).ok_or(NoFill::Nothing)?;
    run_plan(wb, sh, &plan(wb, sh, row, col, MAX_LEN)?)
}

/// The automatic Flash Fill preview (ENT-105), after a typed commit at
/// (row, col): only when the column's examples (below a header row, if
/// there is one) run to `row`, `row` is the second example or later, every
/// cell below in the data is empty, and the program fills every one of
/// them. Nothing else shows a preview. It runs on every typed commit, so it
/// reads shorter values than Ctrl+E does ([`PREVIEW_MAX_LEN`]).
pub fn flash_preview(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Option<FlashFill> {
    let sh = wb.sheets.get(sheet)?;
    let plan = plan(wb, sh, row, col, PREVIEW_MAX_LEN).ok()?;
    let (top, ex_end) = plan.examples;
    if ex_end != row || row < top + 1 {
        return None;
    }
    if !(row + 1..=plan.region.bottom).all(|r| is_empty(sh, r, col)) {
        return None;
    }
    let fill = run_plan(wb, sh, &plan).ok()?;
    fill.blank.is_empty().then_some(fill)
}

/// What a Flash Fill reads and runs: the region, the examples' first and
/// last rows, and the program they show.
struct Plan {
    region: Region,
    examples: (u32, u32),
    program: Vec<Atom>,
}

/// The plan for a Flash Fill at (row, col): the examples from the top of the
/// data, else from the row below it (a header row).
fn plan(wb: &Workbook, sh: &Sheet, row: u32, col: u32, max_len: usize) -> Result<Plan, NoFill> {
    let region = Region::around(sh, row, col).ok_or(NoFill::Nothing)?;
    let mut found = NoFill::Nothing;
    for start in [region.top, region.top + 1] {
        let Some(end) = region.examples_from(sh, start) else {
            continue;
        };
        if end >= region.bottom {
            // Nothing below the examples to fill.
            continue;
        }
        let examples: Vec<Example> = (start..=end)
            .map(|r| Example {
                sources: region
                    .sources
                    .iter()
                    .map(|&c| shown(wb, sh, r, c))
                    .collect(),
                output: shown(wb, sh, r, col),
            })
            .collect();
        match synthesize(&examples, max_len) {
            Some(program) => {
                return Ok(Plan {
                    region,
                    examples: (start, end),
                    program,
                });
            }
            None => found = NoFill::NoPattern,
        }
    }
    Err(found)
}

/// Run a plan's program on the empty cells below its examples.
fn run_plan(wb: &Workbook, sh: &Sheet, plan: &Plan) -> Result<FlashFill, NoFill> {
    let Plan {
        region,
        examples: (top, ex_end),
        program,
    } = plan;
    let (top, ex_end, col) = (*top, *ex_end, region.col);
    let as_text = (top..=ex_end).all(|r| typed_as_text(sh, r, col));
    let mut fill = FlashFill {
        col,
        examples: (top, ex_end),
        fills: Vec::new(),
        blank: Vec::new(),
        last_row: region.bottom,
    };
    for r in (ex_end + 1..=region.bottom).filter(|&r| is_empty(sh, r, col)) {
        let sources: Vec<String> = region
            .sources
            .iter()
            .map(|&c| shown(wb, sh, r, c))
            .collect();
        match run(program, &sources) {
            Some(out) if !out.is_empty() => {
                let style = sh.cell(r, col).map_or(0, |c| c.style);
                let text_target = crate::entry::is_text(&wb.styles.xf(style));
                fill.fills.push((r, typed(&out, as_text, text_target)));
            }
            _ => fill.blank.push(r),
        }
    }
    if fill.fills.is_empty() && fill.blank.is_empty() {
        return Err(NoFill::Nothing);
    }
    if fill.fills.is_empty() {
        return Err(NoFill::NoPattern);
    }
    Ok(fill)
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

    /// The last row of the column's run of non-empty cells from `start`
    /// (`None` when the cell at `start` is empty): the examples.
    fn examples_from(&self, sh: &Sheet, start: u32) -> Option<u32> {
        let mut end = None;
        let mut r = start;
        while r <= self.bottom && !is_empty(sh, r, self.col) {
            end = Some(r);
            r += 1;
        }
        end
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

/// Is the example at (r, c) text that typing would read as something else
/// (`'007`, entered with an apostrophe or into a Text cell)? When every
/// example is, the results are text too (ENT-108). A text that stays text
/// typed bare (`'Ada`) says nothing about the results.
fn typed_as_text(sh: &Sheet, r: u32, c: u32) -> bool {
    match sh.cell(r, c).map(|cell| &cell.value) {
        Some(CellValue::Text(t)) => !stays_text(t),
        _ => false,
    }
}

/// The text a host types for a result: with an apostrophe when the examples
/// were text, or when typing it bare would make a formula — Flash Fill writes
/// constants (ENT-106). A Text-formatted target takes any text as it is, so
/// it gets none: there the apostrophe would be kept as a character.
fn typed(out: &str, as_text: bool, text_target: bool) -> String {
    if text_target {
        return out.to_string();
    }
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
    parse_entry(text, &Xf::default(), &probe_ctx())
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

/// One example's graph: from each char position of its output, the atoms
/// that produce the text starting there and the position after it; and the
/// same edges indexed by atom, for the other examples' lookups.
struct Graph {
    edges: Vec<Vec<(Atom, usize)>>,
    index: Vec<HashMap<Atom, usize>>,
}

/// The longest example or source text Ctrl+E reads, in chars, and the
/// automatic preview's (it runs on every typed commit). A graph has a
/// constant edge from each position to each later piece's start, so it grows
/// with the square of its example's length, and a pattern in values longer
/// than this is not what Flash Fill is for.
const MAX_LEN: usize = 1000;
const PREVIEW_MAX_LEN: usize = 200;
/// The most edges one example's graph may have, and the most edges the
/// search may follow: past either, Flash Fill sees no pattern rather than
/// keep the user waiting (text of many repeated words makes every piece
/// match everywhere).
const MAX_EDGES: usize = 200_000;
const MAX_STEPS: usize = 2_000_000;

/// The graph of one example, or `None` when it would pass [`MAX_EDGES`].
/// Constant atoms run from a position to the start of a Sub or to the end,
/// never a char at a time, so the graphs stay small.
fn graph(ex: &Example) -> Option<Graph> {
    let out: Vec<char> = ex.output.chars().collect();
    let n = out.len();
    let mut char_at = vec![0; ex.output.len() + 1];
    for (ci, (bi, _)) in ex.output.char_indices().enumerate() {
        char_at[bi] = ci;
    }
    char_at[ex.output.len()] = n;
    // The atoms grouped by the text they produce, so each text is searched
    // for once; each keeps its place in `subs` for a stable edge order.
    let mut groups: Vec<(String, Vec<(usize, Atom)>)> = Vec::new();
    let mut by_text: HashMap<String, usize> = HashMap::new();
    for (order, (atom, text)) in subs(&ex.sources).into_iter().enumerate() {
        let g = *by_text.entry(text.clone()).or_insert_with(|| {
            groups.push((text, Vec::new()));
            groups.len() - 1
        });
        groups[g].1.push((order, atom));
    }
    let mut ordered: Vec<Vec<(usize, Atom, usize)>> = vec![Vec::new(); n + 1];
    let mut count = 0usize;
    for (text, atoms) in &groups {
        let len = text.chars().count();
        let mut from = 0;
        while let Some(i) = ex.output[from..].find(text.as_str()) {
            let b = from + i;
            let p = char_at[b];
            count += atoms.len();
            if count > MAX_EDGES {
                return None;
            }
            ordered[p].extend(atoms.iter().map(|(o, a)| (*o, a.clone(), p + len)));
            from = b + out[p].len_utf8();
        }
    }
    let mut starts: Vec<usize> = (0..n).filter(|&p| !ordered[p].is_empty()).collect();
    starts.push(n);
    let mut edges: Vec<Vec<(Atom, usize)>> = Vec::with_capacity(n + 1);
    for (p, mut subs_here) in ordered.into_iter().enumerate() {
        subs_here.sort_by_key(|(o, _, _)| *o);
        let mut here: Vec<(Atom, usize)> = subs_here.into_iter().map(|(_, a, q)| (a, q)).collect();
        for &q in starts.iter().filter(|&&q| q > p) {
            count += 1;
            if count > MAX_EDGES {
                return None;
            }
            here.push((Atom::Const(out[p..q].iter().collect()), q));
        }
        edges.push(here);
    }
    let index = edges
        .iter()
        .map(|here| here.iter().map(|(a, q)| (a.clone(), *q)).collect())
        .collect();
    Some(Graph { edges, index })
}

/// The cheapest program every example's graph has a path for, holding at
/// least one Sub; `None` when there is none, when an example or source is
/// longer than `max_len` chars, or when the search passes its budget.
fn synthesize(examples: &[Example], max_len: usize) -> Option<Vec<Atom>> {
    if examples.is_empty() || examples.iter().any(|e| e.output.is_empty()) {
        return None;
    }
    let long = |s: &String| s.chars().count() > max_len;
    if examples
        .iter()
        .any(|e| long(&e.output) || e.sources.iter().any(long))
    {
        return None;
    }
    let graphs: Vec<Graph> = examples.iter().map(graph).collect::<Option<_>>()?;
    let ends: Vec<usize> = examples.iter().map(|e| e.output.chars().count()).collect();
    let mut search = Search {
        graphs: &graphs,
        ends: &ends,
        memo: HashMap::new(),
        steps: 0,
    };
    let start = vec![0; examples.len()];
    search.best(start.clone(), false).ok()??;
    // Follow the choices the search kept from the start to the ends.
    let (mut at, mut has_sub, mut program) = (start, false, Vec::new());
    while at != ends {
        let (_, atom, next) = search.memo.get(&(at, has_sub))?.clone()?;
        has_sub |= matches!(atom, Atom::Sub { .. });
        program.push(atom);
        at = next;
    }
    Some(program)
}

/// The search for the cheapest program: per state (one position per
/// example, and whether a Sub came yet) its cost to the ends, the atom that
/// starts it and the state after that atom.
struct Search<'a> {
    graphs: &'a [Graph],
    ends: &'a [usize],
    memo: HashMap<(Vec<usize>, bool), Option<Choice>>,
    steps: usize,
}

/// A state's cheapest way on: the cost to the ends, the atom that starts it
/// and the state after that atom.
type Choice = (Cost, Atom, Vec<usize>);

/// The search passed [`MAX_STEPS`].
struct OverBudget;

impl Search<'_> {
    /// The cheapest cost from `at` to the ends (`None`: no program).
    fn best(&mut self, at: Vec<usize>, has_sub: bool) -> Result<Option<Cost>, OverBudget> {
        if at == self.ends {
            return Ok(has_sub.then_some([0; 5]));
        }
        let key = (at, has_sub);
        if let Some(hit) = self.memo.get(&key) {
            return Ok(hit.as_ref().map(|(c, _, _)| *c));
        }
        let at = &key.0;
        let graphs = self.graphs;
        let mut found: Option<Choice> = None;
        // An atom moves every example at once; the first graph proposes, the
        // others must have the same atom at their position.
        for (atom, next0) in &graphs[0].edges[at[0]] {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return Err(OverBudget);
            }
            let mut next = Vec::with_capacity(at.len());
            next.push(*next0);
            let fits = graphs[1..]
                .iter()
                .zip(&at[1..])
                .all(|(g, &p)| match g.index[p].get(atom) {
                    Some(&q) => {
                        next.push(q);
                        true
                    }
                    None => false,
                });
            if !fits {
                continue;
            }
            let sub = has_sub || matches!(atom, Atom::Sub { .. });
            if let Some(rest) = self.best(next.clone(), sub)? {
                let cost = add(atom.cost(), rest);
                if found.as_ref().is_none_or(|(c, _, _)| cost < *c) {
                    found = Some((cost, atom.clone(), next));
                }
            }
        }
        let cost = found.as_ref().map(|(c, _, _)| *c);
        self.memo.insert(key, found);
        Ok(cost)
    }
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
        assert_eq!(typed("=x", false, false), "'=x");
        assert_eq!(typed("-5", false, false), "-5");
        assert_eq!(typed("+a", false, false), "'+a");
        assert_eq!(typed("Ada", false, false), "Ada");
        assert_eq!(typed("007", true, false), "'007");
        // A Text-formatted target keeps whatever it is given.
        assert_eq!(typed("=x", false, true), "=x");
        assert_eq!(typed("007", true, true), "007");
    }

    /// FIX r1 M1: a header row above the examples.
    fn with_header(b: &[&str]) -> Workbook {
        let mut a = vec!["Full Name"];
        a.extend(NAMES);
        wb_with(&[&a, b])
    }

    #[test]
    fn a_header_row_is_not_an_example() {
        // B1 `First Name`, B2 `Ada`: Ctrl+E on B3 (Enter moved there) and on B2.
        let wb = with_header(&["First Name", "Ada"]);
        for row in [2, 1] {
            let f = flash_fill(&wb, 0, row, 1).unwrap();
            assert_eq!(f.examples, (1, 1));
            assert_eq!(
                filled(&f),
                [
                    (2, "Alan"),
                    (3, "Grace"),
                    (4, "Edsger"),
                    (5, "Barbara"),
                    (6, "Donald")
                ]
            );
        }
        // B1 empty above the example.
        let f = flash_fill(&with_header(&["", "Ada"]), 0, 1, 1).unwrap();
        assert_eq!(f.examples, (1, 1));
        assert_eq!(f.fills.len(), 5);
        // A header that is itself the pattern's output stays an example.
        let wb = wb_with(&[&["Ada Lovelace", "Alan Turing", "Grace Hopper"], &["Ada"]]);
        assert_eq!(flash_fill(&wb, 0, 0, 1).unwrap().examples, (0, 0));
    }

    #[test]
    fn the_preview_works_under_a_header_row() {
        let wb = with_header(&["First Name", "Ada", "Alan"]);
        let f = flash_preview(&wb, 0, 2, 1).expect("a preview under the header");
        assert_eq!(
            filled(&f),
            [(3, "Grace"), (4, "Edsger"), (5, "Barbara"), (6, "Donald")]
        );
        assert_eq!(
            flash_preview(&with_header(&["First Name", "Ada"]), 0, 1, 1),
            None
        );
        assert!(flash_preview(&with_header(&["", "Ada", "Alan"]), 0, 2, 1).is_some());
    }

    /// FIX r1 M2: the text-ness of the examples and of the target.
    #[test]
    fn text_targets_and_quoted_examples_get_no_literal_apostrophe() {
        // B1:B6 formatted Text, `Ada` in B1.
        let mut wb = wb_with(&[&NAMES]);
        let mut xf = Xf::default();
        xf.set_code(Some("@".into()));
        let text_style = wb.styles.intern(xf);
        for r in 0..6 {
            wb.sheets[0].set_cell(
                r,
                1,
                Cell {
                    style: text_style,
                    ..Cell::default()
                },
            );
        }
        let cell = crate::entry::entry_cell(&mut wb, 0, 0, 1, "Ada", None).unwrap();
        wb.sheets[0].set_cell(0, 1, cell);
        let f = flash_fill(&wb, 0, 0, 1).unwrap();
        assert_eq!(filled(&f)[0], (1, "Alan"));
        apply(&mut wb, &f);
        assert_eq!(
            wb.sheets[0].cell(1, 1).unwrap().value,
            CellValue::Text("Alan".into())
        );
        // A quote-prefixed example that stays text: plain results.
        let mut wb = wb_with(&[&NAMES]);
        let cell = crate::entry::entry_cell(&mut wb, 0, 0, 1, "'Ada", None).unwrap();
        wb.sheets[0].set_cell(0, 1, cell);
        let f = flash_fill(&wb, 0, 0, 1).unwrap();
        assert_eq!(filled(&f)[0], (1, "Alan"));
        // `'007` on a General target still gives text `042`.
        let mut wb = wb_with(&[&["x-007", "y-042"]]);
        let cell = crate::entry::entry_cell(&mut wb, 0, 0, 1, "'007", None).unwrap();
        wb.sheets[0].set_cell(0, 1, cell);
        let f = flash_fill(&wb, 0, 0, 1).unwrap();
        assert_eq!(filled(&f), [(1, "'042")]);
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

    /// FIX r1 m2: text of many repeated words matches every piece
    /// everywhere; the search stays bounded either way.
    #[test]
    fn repeated_words_stay_fast() {
        let words = |n: usize, w: &str| vec![w; n].join(" ");
        let mut sh = Sheet::default();
        for r in 0..3u32 {
            let src = format!("{} end{r}", words(80, "alpha beta"));
            sh.set_cell(r, 0, Cell::text(&src));
        }
        for r in 0..2u32 {
            let out = format!("{} END{r}", words(60, "beta alpha"));
            sh.set_cell(r, 1, Cell::text(&out));
        }
        let wb = Workbook {
            sheets: vec![sh],
            ..Workbook::default()
        };
        let t = std::time::Instant::now();
        let _ = flash_fill(&wb, 0, 2, 1);
        let _ = flash_preview(&wb, 0, 1, 1);
        let took = t.elapsed();
        assert!(took.as_secs_f64() < 2.0, "{took:?}");
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
