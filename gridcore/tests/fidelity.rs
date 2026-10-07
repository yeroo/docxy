//! Round-trip fidelity gate for xlsx (#1064, the xlsx half of #1060): open
//! every corpus `.xlsx`, save it with no edits, and compare every package part
//! with the original. Fails on any loss not covered by
//! `fidelity/allowlist.txt` or `fidelity/baseline.txt`, on a baseline
//! entry that no longer reproduces, and on any package or schema violation
//! the save added (`fidelity/schema.rs`, #1156): those have no allowlist and
//! no baseline, also under `FIDELITY_UPDATE_BASELINE`. See
//! `docs/fidelity-gate.md`.
//!
//! - `FIDELITY_XLSX_CORPUS=<dir>`: the docxy-corpus `xlsx-ext/` checkout
//!   (default `corpus/xlsx-ext`; relative paths resolve against the workspace
//!   root).
//! - `FIDELITY_REQUIRE_CORPUS=1`: fail instead of skipping when it is absent.
//! - `FIDELITY_UPDATE_BASELINE=1`: rewrite the baseline for the files in this
//!   run instead of judging against it.

// The comparator is docx's (#1060), shared rather than copied; the gate
// drivers differ only in their corpus, round trip and loss classes.
#[path = "../../docxcore/tests/fidelity/mod.rs"]
#[allow(dead_code)]
mod comparator;
#[path = "fidelity/schema.rs"]
mod sml_schema;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use comparator::*;
use gridcore::xlsx::{load_xlsx, save_xlsx_for_path};
use sml_schema::{Violation, new_violations, validate_package};

/// Repo-tracked `.xlsx` that always run, so the gate is never vacuous.
/// `offxy-jetbrains` test resources are copies of files listed here.
const TRACKED: &[&str] = &[
    "assets",
    "corpus/xlsx",
    "corpus/legacy/addin",
    "corpus/legacy/extra",
    "offxy-vscode/mcp/templates",
    "uiharness/fixtures",
];

/// Every class of known loss and the issue that owns its fix, first match
/// wins. A baseline entry no class claims fails the gate.
const CLASSES: &[LossClass] = &[
    LossClass {
        name: "corrupt input (fuzzed: bad CRCs, an invalid deflate stream) is refused",
        issue: "by design: XlsxError::CorruptPart",
        matches: |e| e.kind == Kind::LoadError && e.file.ends_with("/forcepoint107.xlsx"),
    },
    LossClass {
        name: "formula calculation flags are dropped (f/@ca, f/@aca when true)",
        issue: "#1092",
        matches: |e| {
            in_sheet_data(e)
                && e.kind == Kind::LostAttr
                && ends_with_any(e, &["/c/f/@ca", "/c/f/@aca"])
        },
    },
    LossClass {
        name: "formula text is re-serialized (spaces dropped, names upper-cased, _xlfn. \
               added to newer functions; the sheet check finds the same formula \
               otherwise): one entry per cell",
        issue: "#1096",
        matches: |e| is_cell(e, "formula-text"),
    },
    LossClass {
        name: "cell values changed: a cached string's literal CR LF, which XML reads \
               as LF, is written back with the CR escaped (tdf169326: the reader skips \
               XML line-end normalization); a cell past the last column (XFE1) lands \
               on XFC1 (too-many-cols-rows): one entry per cell",
        issue: "#1091",
        matches: |e| {
            is_cell(e, "value")
                && (e
                    .file
                    .ends_with("/tdf169326_ignore_line_breaks_in_referenced_cells.xlsx")
                    || e.file.ends_with("/too-many-cols-rows.xlsx"))
        },
    },
    LossClass {
        name: "the dimension is recomputed from the cells the model keeps (a source \
               dimension covering dropped empty cells, or a wrong one, changes)",
        issue: "#1096",
        matches: |e| {
            e.part.starts_with("xl/worksheets/")
                && e.kind == Kind::ChangedValue
                && local(&e.path) == "/worksheet/dimension/@ref"
        },
    },
    LossClass {
        name: "customWidth=\"1\" is written on every column with an explicit width, \
               also where the source said false or nothing: one entry per column",
        issue: "#1093",
        matches: |e| {
            e.part.starts_with("xl/worksheets/")
                && e.kind == Kind::ChangedValue
                && e.path.strip_prefix("/cols/").is_some_and(|p| {
                    p.split_once('/').is_some_and(|(span, a)| {
                        let (lo, hi) = span.split_once(':').unwrap_or((span, span));
                        col_number(lo).is_some() && col_number(hi).is_some() && a == "customWidth"
                    })
                })
        },
    },
    LossClass {
        name: "docProps/app.xml sheet titles follow the workbook's sheets (a stale \
               source list is corrected; named-range titles in it are dropped)",
        issue: "#1096",
        matches: |e| {
            e.part == "docProps/app.xml"
                && ["/Properties/TitlesOfParts", "/Properties/HeadingPairs"]
                    .iter()
                    .any(|p| e.path.starts_with(p))
        },
    },
    LossClass {
        name: "cells are re-encoded: a shared-strings part is added or recounted when \
               cached string results and inline strings move to it, and row numbers \
               are written where the source omitted them (the sheet check finds the \
               cells unchanged)",
        issue: "#1091",
        matches: |e| {
            let sst = e.part.starts_with("xl/sharedStrings");
            (sst && e.kind == Kind::PartExtra)
                || (sst && e.kind == Kind::ExtraElement && e.path == "/sst/si")
                || (sst
                    && e.kind == Kind::ChangedValue
                    && matches!(e.path.as_str(), "/sst/@count" | "/sst/@uniqueCount"))
                || (in_sheet_data(e)
                    && e.kind == Kind::ExtraAttr
                    && local(&e.path) == "/worksheet/sheetData/row/@r")
        },
    },
];

/// `path` without namespace prefixes, so `/x:worksheet/x:sheetData` reads
/// `/worksheet/sheetData`.
fn local(path: &str) -> String {
    path.split('/')
        .map(|step| match step.rsplit_once(':') {
            Some((p, l)) if p.starts_with('@') => format!("@{l}"),
            Some((_, l)) => l.to_string(),
            None => step.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn in_sheet_data(e: &Entry) -> bool {
    e.part.starts_with("xl/worksheets/") && local(&e.path).starts_with("/worksheet/sheetData/")
}

/// A cell check entry: `/cells/<ref>/<what>`.
fn is_cell(e: &Entry, what: &str) -> bool {
    e.part.starts_with("xl/worksheets/")
        && e.kind == Kind::ChangedValue
        && e.path
            .strip_prefix("/cells/")
            .and_then(|p| p.split_once('/'))
            .is_some_and(|(cell, w)| parse_ref(cell).is_some() && w == what)
}

fn ends_with_any(e: &Entry, tails: &[&str]) -> bool {
    let path = local(&e.path);
    tails.iter().any(|t| path.ends_with(t))
}

// ---------------------------------------------------------------------------
// The sheet check
//
// A worksheet's <sheetData>, <cols> and <dimension> are regenerated on every
// save, so their XML differs from the source in ways that change nothing a
// reader sees (0.14000000000000001 written 0.14, an inline string moved to the
// shared strings, a shared formula expanded per cell, customWidth="true"
// written "1"), and the structural findings for them cannot be told from a
// real loss by path or detail. So each worksheet is also read the way a
// spreadsheet reads it:
//
// - per cell: its value (shared and inline strings resolved, with their run
//   formatting; numbers as doubles), its formula (a shared formula's follower
//   resolved from its master) and its effective style (the cell's own, else
//   its row's, else its column's). A difference is a finding at
//   `/cells/<ref>/value|formula|formula-text|style`.
// - per column: the attributes of the `<col>` covering it. A difference is a
//   finding at `/cols/<letters>/<attribute>`.
//
// So a baseline line covers one cell or one column. The structural findings
// this check covers are then dropped (see `covered_by_check`).

/// A cell's value as a reader sees it. Text keeps its runs: (the run
/// properties' canonical hash, the text), with adjacent runs of the same
/// properties merged.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Empty,
    Number(f64),
    Bool(bool),
    Error(String),
    Date(String),
    Text(Vec<(Option<u64>, String)>),
    /// A value that does not read as its type says (an `n` cell that is no
    /// number, a shared-string index out of range): compared as written.
    Unread(String, String),
}

impl Value {
    fn show(&self) -> String {
        match self {
            Value::Empty => "empty".into(),
            Value::Number(n) => format!("{n}"),
            Value::Bool(b) => format!("{b}"),
            Value::Error(e) => format!("error {e}"),
            Value::Date(d) => format!("date {d}"),
            Value::Text(runs) => {
                let text: String = runs.iter().map(|(_, t)| t.as_str()).collect();
                let rich = if runs.iter().any(|(p, _)| p.is_some()) {
                    " (rich)"
                } else {
                    ""
                };
                format!("{text:?}{rich}")
            }
            Value::Unread(t, v) => format!("t={t} {v:?}"),
        }
    }
}

/// A cell's formula: its text (a shared follower's shifted from its master),
/// and its kind and range for an array or data table formula.
#[derive(Clone, Debug, PartialEq)]
enum Formula {
    Own {
        text: String,
        kind: Option<String>,
    },
    /// A shared follower whose master is missing or does not cover it.
    Unresolved(String),
}

#[derive(Clone, Debug, PartialEq)]
struct CellRead {
    value: Value,
    formula: Option<Formula>,
    style: String,
}

/// The attributes of a `<col>` that say how its columns show.
#[derive(Clone, Debug, PartialEq, Default)]
struct ColRead {
    width: Option<f64>,
    custom_width: bool,
    style: String,
    hidden: bool,
    best_fit: bool,
    phonetic: bool,
    outline_level: String,
    collapsed: bool,
}

#[derive(Default)]
struct SheetRead {
    cells: BTreeMap<(u32, u32), CellRead>,
    /// The style of a row with `customFormat`, which its missing cells take.
    row_styles: HashMap<u32, String>,
    /// (first col, last col, its attributes), 0-based, from `<cols>`.
    cols: Vec<(u32, u32, ColRead)>,
}

impl SheetRead {
    fn col(&self, col: u32) -> Option<&ColRead> {
        self.cols
            .iter()
            .find(|(lo, hi, _)| (*lo..=*hi).contains(&col))
            .map(|(_, _, c)| c)
    }

    /// What a cell the sheet does not list reads as.
    fn missing(&self, row: u32, col: u32) -> CellRead {
        let style = self
            .row_styles
            .get(&row)
            .or_else(|| self.col(col).map(|c| &c.style))
            .cloned()
            .unwrap_or_else(|| "0".into());
        CellRead {
            value: Value::Empty,
            formula: None,
            style,
        }
    }
}

fn elems<'a>(e: &'a Elem, local: &'a str) -> impl Iterator<Item = &'a Elem> + 'a {
    e.children.iter().filter_map(move |c| match c {
        Node::Elem(x) if x.local == local => Some(x),
        _ => None,
    })
}

fn text_of(e: &Elem) -> String {
    e.children
        .iter()
        .filter_map(|c| match c {
            Node::Text(t) => Some(t.as_str()),
            Node::Elem(_) => None,
        })
        .collect()
}

/// The runs of an `<si>` or `<is>`; its phonetic runs are not read.
fn runs(e: &Elem) -> Vec<(Option<u64>, String)> {
    let mut out: Vec<(Option<u64>, String)> = Vec::new();
    let mut push = |props: Option<u64>, text: String| match out.last_mut() {
        Some((p, t)) if *p == props => t.push_str(&text),
        _ => out.push((props, text)),
    };
    for c in &e.children {
        let Node::Elem(c) = c else { continue };
        match c.local.as_str() {
            "t" => push(None, text_of(c)),
            "r" => {
                let props = elems(c, "rPr").next().map(|p| p.hash);
                let text: String = elems(c, "t").map(text_of).collect();
                push(props, text);
            }
            _ => {}
        }
    }
    out
}

/// Column letters as a 1-based number, if they name a column of the grid.
fn col_number(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 || !letters.chars().all(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    let n = letters.bytes().fold(0u32, |n, b| {
        n * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1
    });
    (n <= 16_384).then_some(n)
}

fn col_letters(n: u32) -> String {
    let mut letters = Vec::new();
    let mut n = n;
    while n > 0 {
        letters.push(b'A' + ((n - 1) % 26) as u8);
        n = (n - 1) / 26;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap()
}

/// `B12` as 0-based (row, col).
fn parse_ref(r: &str) -> Option<(u32, u32)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(split);
    if !letters.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let col = col_number(letters)?;
    let row: u32 = digits.parse().ok()?;
    Some((row.checked_sub(1)?, col - 1))
}

fn ref_name(row: u32, col: u32) -> String {
    format!("{}{}", col_letters(col + 1), row + 1)
}

/// `A1:C3` (or `B2`) as 0-based (first row, first col, last row, last col).
fn parse_range(r: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    let ((r0, c0), (r1, c1)) = (parse_ref(a)?, parse_ref(b)?);
    Some((r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1)))
}

/// `formula` with its relative references moved by (rows, cols), as Excel
/// fills a shared formula from its master; a reference that would leave the
/// grid becomes `#REF!`. Independent of gridcore's own translation, which is
/// under test: a reference is `$`-optional column letters and row digits (or
/// a column or row range) standing alone, not inside a string, a quoted
/// sheet name, a `[...]` (structured or external reference), a name or a
/// function call.
fn shift_formula(formula: &str, rows: i64, cols: i64) -> String {
    let s: Vec<char> = formula.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        // Literals and quoted or bracketed names pass through.
        if c == '"' || c == '\'' {
            out.push(c);
            i += 1;
            while i < s.len() {
                out.push(s[i]);
                if s[i] == c {
                    if s.get(i + 1) == Some(&c) {
                        out.push(c);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '[' {
            let mut depth = 0;
            while i < s.len() {
                out.push(s[i]);
                match s[i] {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            continue;
        }
        let at_boundary = i == 0 || !(is_word(s[i - 1]) || s[i - 1] == '$');
        if at_boundary {
            if let Some((text, len)) = shift_ref(&s[i..], rows, cols) {
                let next = s.get(i + len).copied();
                if !next.is_some_and(|n| is_word(n) || n == '(' || n == '!') {
                    out.push_str(&text);
                    i += len;
                    continue;
                }
            }
        }
        // Anything else, a whole word at a time, so `LOG10` stays a name.
        if is_word(c) {
            while i < s.len() && is_word(s[i]) {
                out.push(s[i]);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// A character of a name, a number or a reference.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c == '\\'
}

/// One side of a reference: `$`-optional column letters and/or row digits.
struct RefPart {
    col_abs: bool,
    letters: String,
    row_abs: bool,
    digits: String,
    len: usize,
}

impl RefPart {
    fn read(s: &[char]) -> Option<RefPart> {
        let mut i = 0;
        let lead = s.first() == Some(&'$');
        if lead {
            i += 1;
        }
        let ls = i;
        while s.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
        }
        let letters: String = s[ls..i].iter().collect();
        let mid = !letters.is_empty() && s.get(i) == Some(&'$');
        if mid {
            i += 1;
        }
        let ds = i;
        while s.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
        let digits: String = s[ds..i].iter().collect();
        if letters.is_empty() && digits.is_empty() {
            return None;
        }
        // A `$` before digits alone belongs to the row.
        let (col_abs, row_abs) = if letters.is_empty() {
            (false, lead)
        } else {
            (lead, mid)
        };
        Some(RefPart {
            col_abs,
            letters,
            row_abs,
            digits,
            len: i,
        })
    }

    /// Shifted, or `None` when it leaves the grid. Its column and row must
    /// name the grid before the shift too, else it is no reference: the
    /// outer `Option` is `None`.
    fn shifted(&self, rows: i64, cols: i64) -> Option<Option<String>> {
        let mut out = String::new();
        if !self.letters.is_empty() {
            let n = i64::from(col_number(&self.letters)?);
            let n = if self.col_abs { n } else { n + cols };
            if !(1..=16_384).contains(&n) {
                return Some(None);
            }
            out.push_str(if self.col_abs { "$" } else { "" });
            out.push_str(&col_letters(n as u32));
        }
        if !self.digits.is_empty() {
            let n: i64 = self.digits.parse().ok()?;
            if !(1..=1_048_576).contains(&n) {
                return None;
            }
            let n = if self.row_abs { n } else { n + rows };
            if !(1..=1_048_576).contains(&n) {
                return Some(None);
            }
            out.push_str(if self.row_abs { "$" } else { "" });
            out.push_str(&n.to_string());
        }
        Some(Some(out))
    }
}

/// A reference at the start of `s`: (its shifted text, `#REF!` when it
/// leaves the grid; its length). A cell (`$A$1`), a cell range, a column
/// range (`A:$C`) or a row range (`1:$3`); a lone column or row is a name or
/// a number, not a reference.
fn shift_ref(s: &[char], rows: i64, cols: i64) -> Option<(String, usize)> {
    let first = RefPart::read(s)?;
    let cell = !first.letters.is_empty() && !first.digits.is_empty();
    let range = (s.get(first.len) == Some(&':'))
        .then(|| RefPart::read(&s[first.len + 1..]))
        .flatten()
        .filter(|second| {
            first.letters.is_empty() == second.letters.is_empty()
                && first.digits.is_empty() == second.digits.is_empty()
        });
    let refs = |text: Option<String>| text.unwrap_or_else(|| "#REF!".into());
    match range {
        Some(second) => {
            let (a, b) = (first.shifted(rows, cols)?, second.shifted(rows, cols)?);
            let text = a.zip(b).map(|(a, b)| format!("{a}:{b}"));
            Some((refs(text), first.len + 1 + second.len))
        }
        None if cell => Some((refs(first.shifted(rows, cols)?), first.len)),
        None => None,
    }
}

/// A formula as Excel compares it: spaces outside strings dropped, names
/// upper-cased, the `_xlfn.`/`_xlws.` future-function prefixes dropped where
/// a name starts (not inside a string or a quoted sheet name).
fn normalized(formula: &str) -> String {
    let s: Vec<char> = formula.chars().collect();
    let mut out = String::new();
    let mut quote = None;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                out.push(c);
            }
            None if c.is_whitespace() => {}
            None => {
                let starts_name = i == 0 || !is_word(s[i - 1]);
                let prefix: String = s[i..s.len().min(i + 6)].iter().collect();
                if starts_name && ["_XLFN.", "_XLWS."].contains(&prefix.to_uppercase().as_str()) {
                    i += 6;
                    continue;
                }
                out.extend(c.to_uppercase());
            }
        }
        i += 1;
    }
    out
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true"))
}

fn read_value(c: &Elem, sst: &[Vec<(Option<u64>, String)>]) -> Value {
    let t = attr(c, "t").unwrap_or("n");
    let v = elems(c, "v").next().map(text_of);
    match (t, v) {
        ("inlineStr", _) => match elems(c, "is").next() {
            Some(is) => Value::Text(runs(is)),
            None => Value::Empty,
        },
        (_, None) => Value::Empty,
        ("s", Some(v)) => match v.trim().parse::<usize>().ok().and_then(|i| sst.get(i)) {
            Some(runs) => Value::Text(runs.clone()),
            None => Value::Unread(t.into(), v),
        },
        ("str", Some(v)) => Value::Text(vec![(None, v)]),
        ("b", Some(v)) => match v.trim() {
            "1" | "true" => Value::Bool(true),
            "0" | "false" => Value::Bool(false),
            _ => Value::Unread(t.into(), v),
        },
        ("e", Some(v)) => Value::Error(v),
        ("d", Some(v)) => Value::Date(v),
        ("n", Some(v)) => match v.trim().parse::<f64>() {
            Ok(n) => Value::Number(n),
            Err(_) => Value::Unread(t.into(), v),
        },
        (_, Some(v)) => Value::Unread(t.into(), v),
    }
}

/// A shared formula's master: its cell, its text and the range it covers.
type Master = ((u32, u32), String, Option<(u32, u32, u32, u32)>);

/// A column attribute's difference: (attribute, from, to).
type ColChange = (&'static str, String, String);

fn read_sheet(root: &Elem, sst: &[Vec<(Option<u64>, String)>]) -> SheetRead {
    let mut sheet = SheetRead::default();
    for cols in elems(root, "cols") {
        for col in elems(cols, "col") {
            let n = |a| attr(col, a).and_then(|v| v.trim().parse::<u32>().ok());
            if let (Some(lo), Some(hi)) = (n("min"), n("max")) {
                let read = ColRead {
                    width: attr(col, "width").and_then(|w| w.trim().parse().ok()),
                    custom_width: is_true(attr(col, "customWidth")),
                    style: attr(col, "style").unwrap_or("0").to_string(),
                    hidden: is_true(attr(col, "hidden")),
                    best_fit: is_true(attr(col, "bestFit")),
                    phonetic: is_true(attr(col, "phonetic")),
                    outline_level: attr(col, "outlineLevel").unwrap_or("0").to_string(),
                    collapsed: is_true(attr(col, "collapsed")),
                };
                sheet
                    .cols
                    .push((lo.saturating_sub(1), hi.saturating_sub(1), read));
            }
        }
    }
    // Cells first, formulas as written; shared followers resolve after.
    let mut masters: HashMap<String, Master> = HashMap::new();
    let mut followers: Vec<((u32, u32), String)> = Vec::new();
    let mut next_row = 0;
    for data in elems(root, "sheetData") {
        for row in elems(data, "row") {
            let r = attr(row, "r")
                .and_then(|r| r.trim().parse::<u32>().ok())
                .and_then(|r| r.checked_sub(1))
                .unwrap_or(next_row);
            next_row = r + 1;
            if is_true(attr(row, "customFormat")) {
                let style = attr(row, "s").unwrap_or("0").to_string();
                sheet.row_styles.insert(r, style);
            }
            let mut next_col = 0;
            for c in elems(row, "c") {
                let at = attr(c, "r").and_then(parse_ref).unwrap_or((r, next_col));
                next_col = at.1 + 1;
                let formula = elems(c, "f").next().and_then(|f| {
                    let text = text_of(f);
                    let shared = attr(f, "t") == Some("shared");
                    let si = attr(f, "si").unwrap_or("").to_string();
                    if shared && text.is_empty() {
                        followers.push((at, si));
                        return None;
                    }
                    if shared {
                        let range = attr(f, "ref").and_then(parse_range);
                        masters.insert(si, (at, text.clone(), range));
                    }
                    let kind = attr(f, "t").filter(|k| !matches!(*k, "normal" | "shared"));
                    Some(Formula::Own {
                        text,
                        kind: kind.map(|k| format!("{k} {}", attr(f, "ref").unwrap_or(""))),
                    })
                });
                sheet.cells.insert(
                    at,
                    CellRead {
                        value: read_value(c, sst),
                        formula,
                        style: attr(c, "s").unwrap_or("0").to_string(),
                    },
                );
            }
        }
    }
    for (at, si) in followers {
        let resolved = masters.get(&si).and_then(|((mr, mc), text, range)| {
            let (r0, c0, r1, c1) = (*range)?;
            ((r0..=r1).contains(&at.0) && (c0..=c1).contains(&at.1)).then_some(())?;
            Some(shift_formula(
                text,
                i64::from(at.0) - i64::from(*mr),
                i64::from(at.1) - i64::from(*mc),
            ))
        });
        if let Some(cell) = sheet.cells.get_mut(&at) {
            cell.formula = Some(match resolved {
                Some(text) => Formula::Own { text, kind: None },
                None => Formula::Unresolved(si),
            });
        }
    }
    sheet
}

/// A part's relationship targets of type `.../<kind>`, resolved to part
/// names.
fn rel_targets(trees: &BTreeMap<String, Elem>, part: &str, kind: &str) -> Vec<String> {
    let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
    let rels = if dir.is_empty() {
        format!("_rels/{name}.rels")
    } else {
        format!("{dir}/_rels/{name}.rels")
    };
    let Some(rels) = trees.get(&rels) else {
        return Vec::new();
    };
    elems(rels, "Relationship")
        .filter(|r| attr(r, "Type").is_some_and(|t| t.ends_with(&format!("/{kind}"))))
        .filter_map(|r| attr(r, "Target"))
        .map(|t| match t.strip_prefix('/') {
            Some(abs) => abs.to_string(),
            None => {
                let mut steps: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
                for step in t.split('/') {
                    match step {
                        ".." => {
                            steps.pop();
                        }
                        "." | "" => {}
                        s => steps.push(s),
                    }
                }
                steps.join("/")
            }
        })
        .collect()
}

/// The parts the sheet check reads, parsed, by name: the relationships,
/// the worksheets (in xl/worksheets/ or wherever the workbook's
/// relationships put them) and the workbook's shared strings (found
/// through the package and workbook relationships, whatever their name).
fn trees(bytes: &[u8]) -> BTreeMap<String, Elem> {
    let Some(zip) = opccore::zip::ZipArchive::open(bytes) else {
        return BTreeMap::new();
    };
    // By OPC name, as `read_parts` keys them (#1095).
    let name = |e: &opccore::zip::ZipEntry| e.name.replace('\\', "/");
    let parse = |e: &opccore::zip::ZipEntry| Some((name(e), parse_xml(&zip.extract(e)?)?));
    let mut trees: BTreeMap<String, Elem> = zip
        .entries()
        .iter()
        .filter(|e| {
            let name = name(e).to_ascii_lowercase();
            name.ends_with(".rels")
                || (name.starts_with("xl/worksheets/") && name.ends_with(".xml"))
        })
        .filter_map(parse)
        .collect();
    let entry = |n: &str| zip.entries().iter().find(|e| name(e) == n);
    let workbooks = rel_targets(&trees, "", "officeDocument");
    // A worksheet outside xl/worksheets/ (tdf76115.xlsx keeps it at
    // xl/sheet1.xml) is found through the workbook's relationships.
    let sheets: Vec<String> = workbooks
        .iter()
        .flat_map(|wb| rel_targets(&trees, wb, "worksheet"))
        .filter(|n| !trees.contains_key(n))
        .collect();
    for sheet in sheets {
        if let Some((n, tree)) = entry(&sheet).and_then(parse) {
            trees.insert(n, tree);
        }
    }
    let sst = workbooks
        .iter()
        .flat_map(|wb| rel_targets(&trees, wb, "sharedStrings"))
        .next();
    if let Some((_, tree)) = sst.as_deref().and_then(entry).and_then(parse) {
        trees.insert(SHARED_STRINGS.to_string(), tree);
    }
    trees
}

/// Where [`trees`] keeps the shared strings (no part has this name).
const SHARED_STRINGS: &str = "\0sharedStrings";

/// The workbook's shared strings.
fn shared_strings(trees: &BTreeMap<String, Elem>) -> Vec<Vec<(Option<u64>, String)>> {
    trees
        .get(SHARED_STRINGS)
        .filter(|sst| sst.local == "sst")
        .map(|sst| elems(sst, "si").map(runs).collect())
        .unwrap_or_default()
}

/// The cell and column findings between two worksheets.
fn compare_sheets(part: &str, a: &SheetRead, b: &SheetRead) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut finding = |path: String, detail: String| {
        out.push(Finding {
            part: part.to_string(),
            kind: Kind::ChangedValue,
            path,
            detail,
        })
    };
    let keys: BTreeSet<(u32, u32)> = a.cells.keys().chain(b.cells.keys()).copied().collect();
    for (row, col) in keys {
        let x = a
            .cells
            .get(&(row, col))
            .cloned()
            .unwrap_or_else(|| a.missing(row, col));
        let y = b
            .cells
            .get(&(row, col))
            .cloned()
            .unwrap_or_else(|| b.missing(row, col));
        let cell = ref_name(row, col);
        if x.value != y.value {
            finding(
                format!("/cells/{cell}/value"),
                format!("{} -> {}", x.value.show(), y.value.show()),
            );
        }
        // A formula written in other words (spaces, case, `_xlfn.`) is
        // `formula-text`; any other change, a lost or added formula, or one
        // whose shared master cannot be resolved, is `formula`.
        let what = match (&x.formula, &y.formula) {
            (a, b) if a == b => None,
            (
                Some(Formula::Own { text: t1, kind: k1 }),
                Some(Formula::Own { text: t2, kind: k2 }),
            ) if k1 == k2 && normalized(t1) == normalized(t2) => Some("formula-text"),
            _ => Some("formula"),
        };
        if let Some(what) = what {
            finding(
                format!("/cells/{cell}/{what}"),
                format!("{:?} -> {:?}", x.formula, y.formula),
            );
        }
        if x.style != y.style {
            finding(
                format!("/cells/{cell}/style"),
                format!("{:?} -> {:?}", x.style, y.style),
            );
        }
    }
    // Columns: each attribute's difference, over runs of adjacent columns
    // that differ the same way (one `<col>` may span the whole grid).
    let bounds = |s: &SheetRead| {
        s.cols
            .iter()
            .map(|(lo, hi, _)| (*lo, *hi))
            .collect::<Vec<_>>()
    };
    let spans: BTreeSet<u32> = bounds(a)
        .into_iter()
        .chain(bounds(b))
        .flat_map(|(lo, hi)| lo..=hi.min(16_383))
        .collect();
    let none = ColRead {
        style: "0".into(),
        outline_level: "0".into(),
        ..ColRead::default()
    };
    let attrs = |c: &ColRead| -> [(&'static str, String); 8] {
        [
            ("width", format!("{:?}", c.width)),
            ("customWidth", c.custom_width.to_string()),
            ("style", c.style.clone()),
            ("hidden", c.hidden.to_string()),
            ("bestFit", c.best_fit.to_string()),
            ("phonetic", c.phonetic.to_string()),
            ("outlineLevel", c.outline_level.clone()),
            ("collapsed", c.collapsed.to_string()),
        ]
    };
    // (attribute, from, to) -> runs of (first col, last col).
    let mut runs: BTreeMap<ColChange, Vec<(u32, u32)>> = BTreeMap::new();
    for col in spans {
        let (x, y) = (a.col(col).unwrap_or(&none), b.col(col).unwrap_or(&none));
        for ((name, p), (_, q)) in attrs(x).into_iter().zip(attrs(y)) {
            if p != q {
                let list = runs.entry((name, p, q)).or_default();
                match list.last_mut() {
                    Some((_, hi)) if *hi + 1 == col => *hi = col,
                    _ => list.push((col, col)),
                }
            }
        }
    }
    let mut cols: Vec<(u32, Finding)> = Vec::new();
    for ((name, p, q), list) in runs {
        for (lo, hi) in list {
            let span = if lo == hi {
                col_letters(lo + 1)
            } else {
                format!("{}:{}", col_letters(lo + 1), col_letters(hi + 1))
            };
            cols.push((
                lo,
                Finding {
                    part: part.to_string(),
                    kind: Kind::ChangedValue,
                    path: format!("/cols/{span}/{name}"),
                    detail: format!("{p} -> {q}"),
                },
            ));
        }
    }
    cols.sort_by_key(|(lo, _)| *lo);
    out.extend(cols.into_iter().map(|(_, f)| f));
    out
}

/// Whether every attribute of `e` (no namespace) is among `names`.
fn attrs_within(e: &Elem, names: &[&str]) -> bool {
    e.attrs
        .iter()
        .all(|a| a.uri.is_empty() && names.contains(&a.local.as_str()))
}

/// Whether `e`'s child elements all have local names among `names`.
fn children_within(e: &Elem, names: &[&str]) -> bool {
    e.children.iter().all(|c| match c {
        Node::Elem(x) => names.contains(&x.local.as_str()),
        Node::Text(_) => true,
    })
}

/// A string's content the check reads: `<t>` and runs of `<rPr>` and `<t>`.
fn read_string(e: &Elem) -> bool {
    children_within(e, &["t", "r"]) && elems(e, "r").all(|r| children_within(r, &["rPr", "t"]))
}

/// A formula's attributes the check reads: t, si and ref (and `xml:space`).
fn read_formula(f: &Elem) -> bool {
    f.attrs.iter().all(|a| {
        (a.uri.is_empty() && matches!(a.local.as_str(), "t" | "si" | "ref")) || a.local == "space"
    })
}

/// A cell whose every part the check reads: attributes r, s and t; a `<v>`,
/// an `<f>` and an `<is>` it reads.
fn read_whole(c: &Elem) -> bool {
    attrs_within(c, &["r", "s", "t"])
        && children_within(c, &["v", "f", "is"])
        && elems(c, "f").all(read_formula)
        && elems(c, "is").all(read_string)
}

/// Whether the sheet check covers a structural finding: the cells and
/// columns it reads, as far as it reads them. Row attributes, cell
/// attributes other than r, s and t, formula attributes other than t, si
/// and ref, and phonetic runs stay structural.
fn covered_by_check(f: &Finding, original: &Elem, saved: &Elem) -> bool {
    let path = local(&f.key_path());
    if path.starts_with("/worksheet/cols") {
        return true;
    }
    let Some(rest) = path.strip_prefix("/worksheet/sheetData/row") else {
        return false;
    };
    // The element a lost or extra finding is about, on its side.
    let element = || {
        let tree = if f.kind == Kind::ExtraElement {
            saved
        } else {
            original
        };
        at(tree, &f.path)
    };
    let whole = matches!(f.kind, Kind::LostElement | Kind::ExtraElement);
    match rest {
        // A row carrying nothing but its number and span hint, and cells the
        // check reads whole: those are compared one by one.
        "" => {
            whole
                && element().is_some_and(|row| {
                    attrs_within(row, &["r", "spans"])
                        && children_within(row, &["c"])
                        && elems(row, "c").all(read_whole)
                })
        }
        "/c" => whole && element().is_some_and(read_whole),
        "/c/@r" | "/c/@s" | "/c/@t" => true,
        "/c/f" => whole && element().is_some_and(read_formula),
        "/c/f/@t" | "/c/f/@si" | "/c/f/@ref" | "/c/f/@space" | "/c/f/text()" => true,
        "/c/is" => whole && element().is_some_and(read_string),
        _ if rest.starts_with("/c/v") => true,
        _ => {
            ["/c/is/t", "/c/is/r/t", "/c/is/r/rPr"]
                .iter()
                .any(|p| rest.starts_with(p))
                || rest == "/c/is/r"
                    && whole
                    && element().is_some_and(|r| children_within(r, &["rPr", "t"]))
        }
    }
}

/// Replace the structural findings of each worksheet both packages hold
/// readably by the sheet check's; `covered` counts those dropped.
fn check_sheets(
    original: &[u8],
    saved: &[u8],
    found: Vec<Finding>,
    covered: &mut usize,
) -> Vec<Finding> {
    let (a, b) = (trees(original), trees(saved));
    let (sst_a, sst_b) = (shared_strings(&a), shared_strings(&b));
    let sheets: BTreeSet<&String> = a
        .iter()
        .filter(|(name, t)| {
            t.local == "worksheet" && b.get(*name).is_some_and(|s| s.local == "worksheet")
        })
        .map(|(name, _)| name)
        .collect();
    let mut out: Vec<Finding> = Vec::new();
    for f in found {
        if sheets.contains(&f.part) && covered_by_check(&f, &a[&f.part], &b[&f.part]) {
            *covered += 1;
        } else {
            out.push(f);
        }
    }
    for name in sheets {
        let (x, y) = (read_sheet(&a[name], &sst_a), read_sheet(&b[name], &sst_b));
        out.extend(compare_sheets(name, &x, &y));
    }
    for (name, t) in &b {
        if t.local == "worksheet" {
            out.extend(bad_cols(name, t));
        }
    }
    out
}

/// What no save may write, whatever the original said (#1152): a `<col>`
/// without a width, which Excel opens zero wide, at
/// `/cols/<span>/no-width`; and a `<col>` starting at or before the end of
/// the one before it (overlapping, duplicated or out of order), at
/// `/cols/<span>/overlap`. No loss class claims either.
fn bad_cols(part: &str, root: &Elem) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut previous: Option<(u32, u32)> = None;
    for cols in elems(root, "cols") {
        for col in elems(cols, "col") {
            let n = |a| attr(col, a).and_then(|v| v.trim().parse::<u32>().ok());
            let (Some(lo), Some(hi)) = (n("min"), n("max")) else {
                continue;
            };
            let span = if lo == hi {
                col_letters(lo)
            } else {
                format!("{}:{}", col_letters(lo), col_letters(hi))
            };
            let mut finding = |what: &str, detail: String| {
                out.push(Finding {
                    part: part.to_string(),
                    kind: Kind::ChangedValue,
                    path: format!("/cols/{span}/{what}"),
                    detail,
                })
            };
            if attr(col, "width").is_none() {
                finding(
                    "no-width",
                    format!("<col min=\"{lo}\" max=\"{hi}\"> has no width"),
                );
            }
            if let Some((p_lo, p_hi)) = previous.filter(|(_, p_hi)| lo <= *p_hi) {
                finding(
                    "overlap",
                    format!("starts at or before the end of the <col> before it ({p_lo}..={p_hi})"),
                );
            }
            previous = Some((lo, hi));
        }
    }
    out
}

/// The element at `path` (the comparator's `qname[n]` steps) under `root`.
fn at<'a>(root: &'a Elem, path: &str) -> Option<&'a Elem> {
    let mut steps = path.strip_prefix('/')?.split('/');
    if steps.next()? != root.qname {
        return None;
    }
    let mut cur = root;
    for step in steps {
        let (name, n) = match step.split_once('[') {
            Some((name, n)) => (name, n.strip_suffix(']')?.parse::<usize>().ok()?),
            None => (step, 1),
        };
        cur = cur
            .children
            .iter()
            .filter_map(|c| match c {
                Node::Elem(e) if e.qname == name => Some(e),
                _ => None,
            })
            .nth(n.checked_sub(1)?)?;
    }
    Some(cur)
}

fn attr<'a>(e: &'a Elem, local: &str) -> Option<&'a str> {
    e.attrs
        .iter()
        .find(|a| a.uri.is_empty() && a.local == local)
        .map(|a| a.value.as_str())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("gridcore sits in the workspace root")
        .to_path_buf()
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Collect the `.xlsx` under `dir`. An unreadable directory is an error, not
/// an empty one: a silently shrunken corpus would pass vacuously.
fn xlsx_files(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("fidelity: {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("fidelity: {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            if recursive {
                xlsx_files(&path, true, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
        {
            out.push(path);
        }
    }
}

fn key(prefix: &str, root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!("{prefix}:{rel}")
}

/// The gate's file set: (baseline key, path), sorted by key.
fn corpus(root: &Path) -> (Vec<(String, PathBuf)>, Option<PathBuf>) {
    let mut files = Vec::new();
    for dir in TRACKED {
        let mut found = Vec::new();
        xlsx_files(&root.join(dir), false, &mut found);
        files.extend(found.into_iter().map(|p| (key("repo", root, &p), p)));
    }
    let ext = std::env::var("FIDELITY_XLSX_CORPUS").unwrap_or_else(|_| "corpus/xlsx-ext".into());
    let ext = root.join(ext); // an absolute FIDELITY_XLSX_CORPUS replaces the root
    let ext = ext.is_dir().then_some(ext);
    if let Some(dir) = &ext {
        let mut found = Vec::new();
        xlsx_files(dir, true, &mut found);
        files.extend(found.into_iter().map(|p| (key("ext", dir, &p), p)));
    }
    files.sort();
    (files, ext)
}

/// The round trip under test: what xlsxy's save writes for the file
/// (`save_xlsx_for_path`, so the target kind is the file's own), without
/// `stamp_save`, which rewrites docProps with the current time on purpose.
/// xlsx has no separate no-edit save: every save regenerates the worksheets.
/// `covered` counts the structural findings the cell check replaced.
fn round_trip(bytes: &[u8], path: &Path, covered: &mut usize) -> Vec<Finding> {
    round_trip_validated(bytes, path, covered).0
}

/// [`round_trip`], with the package and schema violations the save added
/// (#1156): Excel repairs those, value loss or not.
fn round_trip_validated(
    bytes: &[u8],
    path: &Path,
    covered: &mut usize,
) -> (Vec<Finding>, Vec<Violation>) {
    match load_xlsx(bytes) {
        Ok(pkg) => {
            let saved = save_xlsx_for_path(&pkg, path);
            let schema = new_violations(&validate_package(bytes), &validate_package(&saved));
            let found = check_sheets(bytes, &saved, compare_packages(bytes, &saved), covered);
            (found, schema)
        }
        Err(e) => {
            let f = Finding {
                part: String::new(),
                kind: Kind::LoadError,
                path: String::new(),
                detail: format!("{e:?}"),
            };
            (vec![f], Vec::new())
        }
    }
}

#[test]
fn round_trip_fidelity_gate() {
    let root = workspace_root();
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fidelity");
    let allow = parse_allowlist(&std::fs::read_to_string(here.join("allowlist.txt")).unwrap())
        .expect("allowlist.txt");
    let baseline_path = here.join("baseline.txt");
    let baseline = parse_baseline(&std::fs::read_to_string(&baseline_path).unwrap_or_default())
        .expect("baseline.txt");

    let (files, ext) = corpus(&root);
    match &ext {
        Some(dir) => eprintln!("fidelity: external corpus {}", dir.display()),
        None if flag("FIDELITY_REQUIRE_CORPUS") => panic!(
            "fidelity: FIDELITY_REQUIRE_CORPUS=1 but no external corpus at FIDELITY_XLSX_CORPUS \
             (default corpus/xlsx-ext); fetch it with corpus/tools/fetch-corpus.sh"
        ),
        None => eprintln!(
            "fidelity: SKIP external corpus: no docxy-corpus checkout at FIDELITY_XLSX_CORPUS \
             (default corpus/xlsx-ext). Running the repo-tracked .xlsx only; fetch the rest \
             with corpus/tools/fetch-corpus.sh"
        ),
    }

    let mut findings: Vec<(String, Finding)> = Vec::new();
    let mut present = BTreeSet::new();
    let mut allowed = vec![0usize; allow.len()];
    let mut covered = 0usize;
    let mut invalid = Vec::new();
    for (file, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        present.insert(file.clone());
        let mut schema = Vec::new();
        let found = guarded(|| {
            let (found, violations) = round_trip_validated(&bytes, path, &mut covered);
            schema = violations;
            found
        });
        invalid.extend(schema.iter().map(|v| v.line(file)));
        for f in apply_allowlist(found, &allow, &mut allowed) {
            findings.push((file.clone(), f));
        }
    }
    eprintln!(
        "fidelity: {} files, {} findings, {} covered by the sheet check, {} allowlisted",
        files.len(),
        findings.len(),
        covered,
        allowed.iter().sum::<usize>()
    );
    for (rule, n) in allow.iter().zip(&allowed) {
        eprintln!(
            "fidelity:   {n:>6} allowed: {} {} ({})",
            rule.part, rule.path, rule.reason
        );
    }

    // Excel repairs these, lost or not: never baselined (#1156).
    assert!(
        invalid.is_empty(),
        "fidelity gate: save introduced {} package or schema violations \
         (docs/fidelity-gate.md)\n{}",
        invalid.len(),
        invalid.join("\n")
    );

    if flag("FIDELITY_UPDATE_BASELINE") {
        let next = updated_baseline(&findings, &present, &baseline);
        let (text, unclassified) = render_baseline(&next, CLASSES);
        std::fs::write(&baseline_path, text).unwrap();
        eprintln!(
            "fidelity: wrote {} entries to {} ({} unclassified)",
            next.len(),
            baseline_path.display(),
            unclassified.len()
        );
        return;
    }

    let (_, unclassified) = render_baseline(&baseline, CLASSES);
    let verdict = judge(&findings, &present, &baseline);
    let mut report = String::new();
    for (e, f) in &verdict.new {
        report.push_str(&format!(
            "NEW   {} | {} | {} | {}  {}\n",
            e.file,
            e.part,
            e.kind.as_str(),
            f.path,
            f.detail
        ));
    }
    for e in &verdict.stale {
        report.push_str(&format!(
            "STALE {} | {} | {} | {}  (fixed: remove it from baseline.txt)\n",
            e.file,
            e.part,
            e.kind.as_str(),
            e.path
        ));
    }
    for e in &unclassified {
        report.push_str(&format!(
            "UNCLASSIFIED {} | {} | {} | {}  (add a loss class with an issue)\n",
            e.file,
            e.part,
            e.kind.as_str(),
            e.path
        ));
    }
    assert!(
        verdict.passed() && unclassified.is_empty(),
        "fidelity gate: {} new, {} stale, {} unclassified (docs/fidelity-gate.md)\n{report}",
        verdict.new.len(),
        verdict.stale.len(),
        unclassified.len()
    );
}

/// The files whose unedited save Excel opened with a column zero wide
/// (#1152, found by the real-Excel oracle), and three more the same
/// control anchors hit. Paths under the external corpus.
const ZERO_WIDE_COLUMN_FILES: &[&str] = &[
    "libreoffice/sc/qa/unit/data/xlsx/activex_checkbox.xlsx",
    "libreoffice/chart2/qa/extras/data/xlsx/tdf111173.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/button-form-control.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/checkbox-form-control.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/singlecontrol.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/pivot_dark1.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/pivottable_date_field_filter.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/tdf120301_xmlSpaceParsing.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/tdf134769.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/tdf161365.xlsx",
    "libreoffice/sc/qa/unit/data/xlsx/tdf60673.xlsx",
    "openoffice/test/testgui/data/pvt/complex_29s.xlsx",
];

#[test]
fn the_zero_wide_column_files_save_their_columns_as_they_were() {
    let (_, ext) = corpus(&workspace_root());
    let Some(dir) = ext else {
        assert!(
            !flag("FIDELITY_REQUIRE_CORPUS"),
            "fidelity: FIDELITY_REQUIRE_CORPUS=1 but no external corpus"
        );
        eprintln!("fidelity: SKIP the #1152 files: no external corpus");
        return;
    };
    for file in ZERO_WIDE_COLUMN_FILES {
        let path = dir.join(file);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let pkg = load_xlsx(&bytes).unwrap_or_else(|e| panic!("{file}: {e:?}"));
        let saved = save_xlsx_for_path(&pkg, &path);
        let (a, b) = (trees(&bytes), trees(&saved));
        let (sst_a, sst_b) = (shared_strings(&a), shared_strings(&b));
        for (name, t) in &b {
            if t.local != "worksheet" {
                continue;
            }
            // No `<col>` without a width, none overlapping another.
            assert_eq!(bad_cols(name, t), vec![], "{file} {name}");
            // Every column as wide as it was.
            let original = a
                .get(name)
                .unwrap_or_else(|| panic!("{file}: {name} is new"));
            let widths: Vec<Finding> =
                compare_sheets(name, &read_sheet(original, &sst_a), &read_sheet(t, &sst_b))
                    .into_iter()
                    .filter(|f| f.path.starts_with("/cols/") && f.path.ends_with("/width"))
                    .collect();
            assert_eq!(widths, vec![], "{file} {name}");
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests: the xlsx side of the shared comparator, the sheet check and
// the real allowlist.

const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

fn sheet(rows: &str) -> Vec<u8> {
    format!(r#"<worksheet xmlns="{MAIN}"><sheetData>{rows}</sheetData></worksheet>"#).into_bytes()
}

fn package(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let parts: Vec<_> = parts
        .iter()
        .map(|(n, b)| (n.to_string(), b.clone()))
        .collect();
    opccore::zipwrite::write_zip(&parts)
}

fn sst(items: &str) -> Vec<u8> {
    format!(r#"<sst xmlns="{MAIN}">{items}</sst>"#).into_bytes()
}

const PACKAGE_RELS: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const WORKBOOK_RELS: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="strings.xml"/></Relationships>"#;

/// A package of one worksheet (`<worksheet>` content) and, when given,
/// shared strings, found through the workbook's relationship (at
/// `xl/strings.xml`, not the conventional name).
fn workbook(content: &str, strings: &str) -> Vec<u8> {
    let ws = format!(r#"<worksheet xmlns="{MAIN}">{content}</worksheet>"#).into_bytes();
    let mut parts = vec![("xl/worksheets/sheet1.xml", ws)];
    if !strings.is_empty() {
        parts.push(("_rels/.rels", PACKAGE_RELS.as_bytes().to_vec()));
        parts.push((
            "xl/_rels/workbook.xml.rels",
            WORKBOOK_RELS.as_bytes().to_vec(),
        ));
        parts.push(("xl/strings.xml", sst(strings)));
    }
    package(&parts)
}

/// What the sheet check leaves of the worksheet's findings between two
/// packages of (`<worksheet>` content, shared strings).
fn after_check(original: (&str, &str), saved: (&str, &str)) -> Vec<(Kind, String)> {
    let (original, saved) = (workbook(original.0, original.1), workbook(saved.0, saved.1));
    let mut covered = 0;
    check_sheets(
        &original,
        &saved,
        compare_packages(&original, &saved),
        &mut covered,
    )
    .into_iter()
    .filter(|f| f.part == "xl/worksheets/sheet1.xml")
    .map(|f| (f.kind, f.path))
    .collect()
}

/// [`after_check`] of two `<sheetData>` contents without shared strings.
fn cells(original: &str, saved: &str) -> Vec<(Kind, String)> {
    let data = |rows: &str| format!("<sheetData>{rows}</sheetData>");
    after_check((&data(original), ""), (&data(saved), ""))
}

fn at_cell(cell: &str, what: &str) -> Vec<(Kind, String)> {
    vec![(Kind::ChangedValue, format!("/cells/{cell}/{what}"))]
}

#[test]
fn a_dropped_cell_is_one_lost_element() {
    // The shared comparator, seen from the xlsx side: a lost cell with a
    // value is one finding at its path, not a cascade over its row.
    let pkg = |rows: &str| package(&[("xl/worksheets/sheet1.xml", sheet(rows))]);
    let (three, two) = (
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c></row>"#,
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="C1"><v>3</v></c></row>"#,
    );
    let found: Vec<_> = compare_packages(&pkg(three), &pkg(two))
        .into_iter()
        .map(|f| (f.kind, f.path))
        .collect();
    assert_eq!(
        found,
        vec![(
            Kind::LostElement,
            "/worksheet/sheetData/row/c[2]".to_string()
        )]
    );
    // The sheet check reports it at the cell, and covers the structural one.
    assert_eq!(cells(three, two), at_cell("B1", "value"));
}

#[test]
fn a_number_is_read_as_a_double_only_in_a_number_cell() {
    let row = |t: &str, v: &str| format!(r#"<row r="1"><c r="A1"{t}><v>{v}</v></c></row>"#);
    for (from, to) in [
        ("0.0", "0"),
        ("0.14000000000000001", "0.14"),
        ("1E+3", "1000"),
    ] {
        assert_eq!(cells(&row("", from), &row("", to)), vec![], "{from}");
        assert_eq!(cells(&row(r#" t="n""#, from), &row("", to)), vec![]);
    }
    assert_eq!(
        cells(&row("", "0.1"), &row("", "0.2")),
        at_cell("A1", "value")
    );
    // The same digits in a string cell are text.
    assert_eq!(
        cells(&row(r#" t="str""#, "1.0"), &row(r#" t="str""#, "1")),
        at_cell("A1", "value")
    );
    // A number that became a string, though it reads the same.
    assert_eq!(
        cells(&row("", "1"), &row(r#" t="str""#, "1")),
        at_cell("A1", "value")
    );
}

#[test]
fn a_boolean_reads_the_same_spelled_one_or_true() {
    let row = |v: &str| format!(r#"<row r="1"><c r="A1" t="b"><v>{v}</v></c></row>"#);
    assert_eq!(cells(&row("true"), &row("1")), vec![]);
    assert_eq!(cells(&row("false"), &row("0")), vec![]);
    assert_eq!(cells(&row("true"), &row("0")), at_cell("A1", "value"));
}

#[test]
fn strings_are_read_through_the_workbooks_shared_strings_with_their_runs() {
    let data = |rows: &str| format!("<sheetData>{rows}</sheetData>");
    let s = |i: &str| {
        data(&format!(
            r#"<row r="1"><c r="A1" t="s"><v>{i}</v></c></row>"#
        ))
    };
    let inline = data(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>dup</t></is></c></row>"#);
    let strings = "<si><t>dup</t></si><si><t>dup</t></si><si><t>other</t></si>";
    // A duplicate remapped to its first copy, an inline string moved to the
    // shared strings, a cached string result moved there: the same text.
    assert_eq!(after_check((&s("1"), strings), (&s("0"), strings)), vec![]);
    assert_eq!(after_check((&inline, strings), (&s("0"), strings)), vec![]);
    let str_result = data(r#"<row r="1"><c r="A1" t="str"><f>B1</f><v>dup</v></c></row>"#);
    let s_result = data(r#"<row r="1"><c r="A1" t="s"><f>B1</f><v>0</v></c></row>"#);
    assert_eq!(
        after_check((&str_result, strings), (&s_result, strings)),
        vec![]
    );
    // Another string, or the same text without its run formatting.
    assert_eq!(
        after_check((&s("2"), strings), (&s("0"), strings)),
        at_cell("A1", "value")
    );
    let rich = r#"<si><r><rPr><b/></rPr><t>dup</t></r></si>"#;
    assert_eq!(
        after_check((&s("0"), rich), (&s("0"), "<si><t>dup</t></si>")),
        at_cell("A1", "value")
    );
}

#[test]
fn shared_formulas_shift_like_excel_fills_them() {
    let shift = shift_formula;
    assert_eq!(shift("C1+1", 0, 1), "D1+1");
    assert_eq!(shift("$C$1+C$1+$C1", 2, 3), "$C$1+F$1+$C3");
    assert_eq!(shift("SUM(A1:B2)*LOG10(A1)", 1, 0), "SUM(A2:B3)*LOG10(A2)");
    assert_eq!(shift("SUM(A:A,1:1)", 1, 1), "SUM(B:B,2:2)");
    // Strings, sheet names, structured references and names stay.
    assert_eq!(
        shift(r#"'A1 x'!A1&"A1"&Sheet1!A1&T[A1]&Tax"#, 1, 0),
        r#"'A1 x'!A2&"A1"&Sheet1!A2&T[A1]&Tax"#
    );
    // A number is no row.
    assert_eq!(shift("A1*10", 1, 0), "A2*10");
    // A reference that leaves the grid is #REF!, the rest still moves.
    assert_eq!(shift("XFD1+A1", 0, 1), "#REF!+B1");
    assert_eq!(shift("SUM(A1:XFD1)+A1", 0, 1), "SUM(#REF!)+B1");
    assert_eq!(shift("A1", -1, 0), "#REF!");
}

#[test]
fn an_expansion_off_the_grid_agrees_with_ref() {
    // What gridcore writes for a follower whose master points past the last
    // column: the sheet check resolves the follower to the same #REF!.
    let master = r#"<c r="A1"><f t="shared" ref="A1:B1" si="0">XFD1+A1</f><v>1</v></c>"#;
    let original =
        format!(r#"<row r="1">{master}<c r="B1"><f t="shared" si="0"/><v>1</v></c></row>"#);
    // gridcore's own blank workbook with this sheet in place of its first.
    let blank = load_xlsx(&gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx())).unwrap();
    let parts: Vec<(String, Vec<u8>)> = blank
        .part_names()
        .into_iter()
        .map(|n| {
            let bytes = if n == "xl/worksheets/sheet1.xml" {
                sheet(&original)
            } else {
                blank.part(n).unwrap().to_vec()
            };
            (n.to_string(), bytes)
        })
        .collect();
    let bytes = opccore::zipwrite::write_zip(&parts);
    let saved = save_xlsx_for_path(&load_xlsx(&bytes).unwrap(), Path::new("book.xlsx"));
    let sheet1 = opccore::zip::ZipArchive::open(&saved)
        .and_then(|z| z.read("xl/worksheets/sheet1.xml"))
        .unwrap();
    assert!(
        String::from_utf8_lossy(&sheet1).contains("<f>#REF!+B1</f>"),
        "{}",
        String::from_utf8_lossy(&sheet1)
    );
    let mut covered = 0;
    let found: Vec<_> = check_sheets(
        &bytes,
        &saved,
        compare_packages(&bytes, &saved),
        &mut covered,
    )
    .into_iter()
    .filter(|f| f.path.starts_with("/cells/"))
    .map(|f| (f.path, f.detail))
    .collect();
    assert_eq!(found, vec![]);
}

#[test]
fn a_shared_formulas_follower_must_be_its_masters_formula_shifted() {
    let master = r#"<c r="A1"><f t="shared" ref="A1:B1" si="0">C1+1</f><v>1</v></c>"#;
    let original =
        format!(r#"<row r="1">{master}<c r="B1"><f t="shared" si="0"/><v>1</v></c></row>"#);
    let saved = |b1: &str| format!(r#"<row r="1"><c r="A1"><f>C1+1</f><v>1</v></c>{b1}</row>"#);
    assert_eq!(
        cells(&original, &saved(r#"<c r="B1"><f>D1+1</f><v>1</v></c>"#)),
        vec![]
    );
    for wrong in [
        r#"<c r="B1"><f>C1+1</f><v>1</v></c>"#,
        r#"<c r="B1"><f>E1+1</f><v>1</v></c>"#,
        r#"<c r="B1"><f/><v>1</v></c>"#,
        r#"<c r="B1"><v>1</v></c>"#,
    ] {
        assert_eq!(
            cells(&original, &saved(wrong)),
            at_cell("B1", "formula"),
            "{wrong}"
        );
    }
    // A `_xlfn.` inside a string is text, not a prefix (#1064 r3).
    let literal = |text: &str| format!(r#"<row r="1"><c r="A1"><f>{text}</f><v>1</v></c></row>"#);
    assert_eq!(
        cells(&literal(r#""_XLFN.Error""#), &literal(r#""Error""#)),
        at_cell("A1", "formula")
    );
    assert_eq!(
        cells(
            &literal("ISO.CEILING(A2)"),
            &literal("_xlfn.ISO.CEILING(A2)")
        ),
        at_cell("A1", "formula-text")
    );
    // The same formula in other words is formula-text.
    assert_eq!(
        cells(&original, &saved(r#"<c r="B1"><f>d1 + 1</f><v>1</v></c>"#)),
        at_cell("B1", "formula-text")
    );
    // A follower its master's range does not cover, or with no master, does
    // not resolve, whatever the save wrote.
    let outside = r#"<row r="1"><c r="A1"><f t="shared" ref="A1:A1" si="0">C1+1</f><v>1</v></c><c r="B1"><f t="shared" si="0"/><v>1</v></c></row>"#;
    assert_eq!(
        cells(outside, &saved(r#"<c r="B1"><f>D1+1</f><v>1</v></c>"#)),
        at_cell("B1", "formula")
    );
    // An array formula that lost its range.
    let array = r#"<row r="1"><c r="A1"><f t="array" ref="A1:A2">C1:C2</f><v>1</v></c></row>"#;
    let plain = r#"<row r="1"><c r="A1"><f>C1:C2</f><v>1</v></c></row>"#;
    assert_eq!(cells(array, plain), at_cell("A1", "formula"));
}

#[test]
fn a_dropped_empty_cell_reads_as_its_row_or_column_style() {
    let a1 = r#"<c r="A1"><v>1</v></c>"#;
    let kept = format!(r#"<row r="1">{a1}</row>"#);
    // An empty cell with the default style, and a row of them, may go.
    for empty in [r#"<c r="B1"/>"#, r#"<c r="B1" s="0"/>"#] {
        assert_eq!(
            cells(&format!(r#"<row r="1">{a1}{empty}</row>"#), &kept),
            vec![],
            "{empty}"
        );
    }
    let empty_row = format!(r#"{kept}<row r="2" spans="1:2"><c r="A2"/></row>"#);
    assert_eq!(cells(&empty_row, &kept), vec![]);
    // A styled empty cell may not, nor a row with a height.
    assert_eq!(
        cells(&format!(r#"<row r="1">{a1}<c r="B1" s="3"/></row>"#), &kept),
        at_cell("B1", "style")
    );
    assert_eq!(
        cells(
            &format!(r#"{kept}<row r="2" ht="30" customHeight="1"/>"#),
            &kept
        ),
        vec![(Kind::LostElement, "/worksheet/sheetData/row[2]".to_string())]
    );
    // An explicit default style under a row style (customFormat) or a column
    // style: dropped, the cell would take that style (#1064 r1).
    let styled_row =
        |cells: &str| format!(r#"<row r="1" s="5" customFormat="1">{a1}{cells}</row>"#);
    assert_eq!(
        cells(&styled_row(r#"<c r="B1"/>"#), &styled_row("")),
        at_cell("B1", "style")
    );
    let col = r#"<cols><col min="2" max="2" width="9" style="7"/></cols>"#;
    assert_eq!(
        after_check(
            (
                &format!(r#"{col}<sheetData><row r="1">{a1}<c r="B1"/></row></sheetData>"#),
                ""
            ),
            (&format!("{col}<sheetData>{kept}</sheetData>"), ""),
        ),
        at_cell("B1", "style")
    );
}

#[test]
fn what_the_check_does_not_read_stays_structural() {
    // Row attributes, cell attributes but r, s and t, formula attributes but
    // t, si and ref.
    let found = cells(
        r#"<row r="1" ht="20" customHeight="1"><c r="A1" vm="1"><f ca="1">NOW()</f><v>1</v></c></row>"#,
        r#"<row r="1"><c r="A1"><f>NOW()</f><v>1</v></c></row>"#,
    );
    let paths: Vec<_> = found.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/worksheet/sheetData/row/@customHeight",
            "/worksheet/sheetData/row/@ht",
            "/worksheet/sheetData/row/c/@vm",
            "/worksheet/sheetData/row/c/f/@ca",
        ]
    );
    // A lost cell carrying more than the check reads stays a lost cell too.
    let found = cells(
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1" vm="1"><v>2</v></c></row>"#,
        r#"<row r="1"><c r="A1"><v>1</v></c></row>"#,
    );
    assert_eq!(
        found,
        vec![
            (
                Kind::LostElement,
                "/worksheet/sheetData/row/c[2]".to_string()
            ),
            (Kind::ChangedValue, "/cells/B1/value".to_string()),
        ]
    );
    // A dropped row whose cell carries more than the check reads stays a
    // lost row (#1064 r3).
    let found = cells(
        r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2" ph="1"/></row>"#,
        r#"<row r="1"><c r="A1"><v>1</v></c></row>"#,
    );
    assert_eq!(
        found,
        vec![(Kind::LostElement, "/worksheet/sheetData/row[2]".to_string())]
    );
    // A phonetic run of an inline string is not read.
    let found = cells(
        r#"<row r="1"><c r="A1" t="inlineStr"><is><t>x</t><rPh sb="0" eb="1"><t>y</t></rPh></is></c></row>"#,
        r#"<row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row>"#,
    );
    assert_eq!(
        found,
        vec![(
            Kind::LostElement,
            "/worksheet/sheetData/row/c/is/rPh".to_string()
        )]
    );
}

#[test]
fn columns_are_compared_one_by_one() {
    let cols = |c: &str| format!("<cols>{c}</cols><sheetData/>");
    let check = |a: &str, b: &str| after_check((&cols(a), ""), (&cols(b), ""));
    // The same columns split, or a boolean spelled 1: the same.
    assert_eq!(
        check(
            r#"<col min="1" max="2" width="9" customWidth="true"/>"#,
            r#"<col min="1" max="1" width="9.0" customWidth="1"/><col min="2" max="2" width="9" customWidth="1"/>"#,
        ),
        vec![]
    );
    // customWidth added; a width, style or hidden changed: per column.
    let col = |c: &str, what: &str| (Kind::ChangedValue, format!("/cols/{c}/{what}"));
    assert_eq!(
        check(
            r#"<col min="2" max="3" width="9"/>"#,
            r#"<col min="2" max="3" width="9" customWidth="1"/>"#
        ),
        vec![col("B:C", "customWidth")]
    );
    assert_eq!(
        check(
            r#"<col min="1" max="1" width="9" style="2" hidden="1"/>"#,
            r#"<col min="1" max="1" width="10"/>"#
        ),
        vec![col("A", "hidden"), col("A", "style"), col("A", "width")]
    );
}

#[test]
fn a_saved_col_without_a_width_or_overlapping_another_fails() {
    let cols = |c: &str| format!("<cols>{c}</cols><sheetData/>");
    let check = |a: &str, b: &str| after_check((&cols(a), ""), (&cols(b), ""));
    let col = |c: &str, what: &str| (Kind::ChangedValue, format!("/cols/{c}/{what}"));
    // What the suite wrote for a control's anchor (#1152): no width, twice.
    // The column read compares them as no `<col>` at all, so only this
    // check finds them.
    assert_eq!(
        check("", r#"<col min="1" max="1"/><col min="1" max="1"/>"#),
        vec![
            col("A", "no-width"),
            col("A", "no-width"),
            col("A", "overlap")
        ]
    );
    // Overlapping and out of order, each where it starts.
    let ok = r#"<col min="1" max="2" width="9"/><col min="4" max="4" width="9"/>"#;
    assert_eq!(check(ok, ok), vec![]);
    assert_eq!(
        check(
            ok,
            r#"<col min="1" max="2" width="9"/><col min="2" max="3" width="9"/><col min="1" max="1" width="9"/>"#
        )
        .into_iter()
        .filter(|(_, p)| p.ends_with("/overlap"))
        .collect::<Vec<_>>(),
        vec![col("B:C", "overlap"), col("A", "overlap")]
    );
    // Judged on the saved side only: an original's bad `<col>` is not the
    // save's, and a save that fixes it is clean of these.
    assert_eq!(
        check(
            r#"<col min="2" max="2"/>"#,
            r#"<col min="2" max="2" width="0" customWidth="1"/>"#
        )
        .into_iter()
        .filter(|(_, p)| p.ends_with("/no-width") || p.ends_with("/overlap"))
        .collect::<Vec<_>>(),
        vec![]
    );
    // No loss class claims them: they always fail the gate.
    for (kind, path) in check("", r#"<col min="1" max="1"/><col min="1" max="1"/>"#) {
        let e = Entry {
            file: "ext:libreoffice/sc/qa/unit/data/xlsx/activex_checkbox.xlsx".into(),
            part: "xl/worksheets/sheet1.xml".into(),
            kind,
            path: path.clone(),
        };
        assert!(!CLASSES.iter().any(|c| (c.matches)(&e)), "{path}");
    }
}

#[test]
fn cell_references_read_as_written_or_by_position() {
    assert_eq!(parse_ref("A1"), Some((0, 0)));
    assert_eq!(parse_ref("AB12"), Some((11, 27)));
    assert_eq!(parse_ref("1_1"), None);
    assert_eq!(parse_ref("XFE1"), None);
    assert_eq!(ref_name(11, 27), "AB12");
    // Cells without r are placed after the previous one: the writer's added
    // r on them agrees. The row's added r is a row attribute and stays.
    assert_eq!(
        cells(
            r#"<row><c><v>1</v></c><c><v>2</v></c></row>"#,
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row>"#,
        ),
        vec![(Kind::ExtraAttr, "/worksheet/sheetData/row/@r".to_string())]
    );
}

fn real_allowlist() -> Vec<AllowRule> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fidelity");
    parse_allowlist(&std::fs::read_to_string(here.join("allowlist.txt")).unwrap()).unwrap()
}

/// What the real allowlist leaves of one file's findings.
fn not_allowed(found: Vec<Finding>) -> Vec<(Kind, String)> {
    let allow = real_allowlist();
    let mut counts = vec![0; allow.len()];
    apply_allowlist(found, &allow, &mut counts)
        .into_iter()
        .map(|f| (f.kind, f.part))
        .collect()
}

const CALC_CHAIN_OVERRIDE: &str = r#"<Override PartName="/xl/calcChain.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/>"#;
const CALC_CHAIN_REL: &str = r#"<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calcChain.xml"/>"#;
const STRICT_CALC_CHAIN_REL: &str = r#"<Relationship Id="rId9" Type="http://purl.oclc.org/ooxml/officeDocument/relationships/calcChain" Target="calcChain.xml"/>"#;

/// A package with the given content-type overrides, workbook relationships
/// and, when `chain`, a calc chain.
fn calc_package(overrides: &str, rels: &str, chain: bool) -> Vec<u8> {
    let types = format!(
        r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
    );
    let rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
    );
    let mut parts = vec![
        ("[Content_Types].xml", types.into_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.into_bytes()),
    ];
    if chain {
        parts.push((
            "xl/calcChain.xml",
            format!(r#"<calcChain xmlns="{MAIN}"/>"#).into_bytes(),
        ));
    }
    package(&parts)
}

#[test]
fn allowlist_tolerates_the_calc_chain_only_where_it_was_dropped() {
    for rel in [CALC_CHAIN_REL, STRICT_CALC_CHAIN_REL] {
        let original = calc_package(CALC_CHAIN_OVERRIDE, rel, true);
        // Dropped with its override and relationship: all tolerated.
        let dropped = calc_package("", "", false);
        assert_eq!(not_allowed(compare_packages(&original, &dropped)), vec![]);
    }
    // The chain kept, but its override or relationship lost: both reported.
    let original = calc_package(CALC_CHAIN_OVERRIDE, CALC_CHAIN_REL, true);
    let kept = calc_package("", "", true);
    assert_eq!(
        not_allowed(compare_packages(&original, &kept)),
        vec![
            (Kind::LostElement, "[Content_Types].xml".to_string()),
            (Kind::LostElement, "xl/_rels/workbook.xml.rels".to_string()),
        ]
    );
    // Another part's override is never tolerated, chain dropped or not.
    let other = r#"<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#;
    let original = calc_package(&format!("{CALC_CHAIN_OVERRIDE}{other}"), "", true);
    let dropped = calc_package("", "", false);
    assert_eq!(
        not_allowed(compare_packages(&original, &dropped)),
        vec![(Kind::LostElement, "[Content_Types].xml".to_string())]
    );
}

#[test]
fn a_workbook_gridcore_wrote_round_trips_clean() {
    // Everything the writer itself produces survives open + save: what is
    // left after the sheet check and the allowlist is a regression.
    use gridcore::sheet::Cell;
    let mut pkg = gridcore::xlsx::new_xlsx();
    let s = &mut pkg.workbook.sheets[0];
    s.set_cell(0, 0, Cell::number(0.1));
    s.set_cell(0, 1, Cell::text("text"));
    s.set_cell(2, 3, Cell::formula("A1*2"));
    let bytes = gridcore::xlsx::save_xlsx(&pkg);
    let mut covered = 0;
    let found = round_trip(&bytes, Path::new("book.xlsx"), &mut covered);
    assert_eq!(not_allowed(found), vec![]);
}

/// #1108: a package shaped like tdf76115.xlsx (entries named with `\`, the
/// worksheet at xl/sheet1.xml and declared ISO-8859-1, with Latin-1 inline
/// strings) is compared part by part with its save: parts by OPC name, the
/// sheet read by its declaration and checked cell by cell. All that is left
/// is the shared-strings part the inline strings move to (#1091's class).
#[test]
fn a_backslash_latin1_package_is_compared_by_part_and_cell() {
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root = format!(
        r#"<Relationships xmlns="{RELS}"><Relationship Id="rId1" Type="{R}/officeDocument" Target="/xl/workbook.xml"/></Relationships>"#
    );
    let wb = format!(
        r#"<workbook xmlns="{MAIN}" xmlns:r="{R}"><sheets><sheet name="Plan1" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let wb_rels = format!(
        r#"<Relationships xmlns="{RELS}"><Relationship Id="rId1" Type="{R}/worksheet" Target="/xl/sheet1.xml"/></Relationships>"#
    );
    let mut ws = format!(
        r#"<?xml version="1.0" encoding="ISO-8859-1"?><worksheet xmlns="{MAIN}"><sheetData><row r="1" spans="1:2"><c r="A1" t="inlineStr"><is><t>S"#
    )
    .into_bytes();
    ws.extend_from_slice(
        b"\xe9rie</t></is></c><c r=\"B1\"><v>7</v></c></row></sheetData></worksheet>",
    );
    let bytes = package(&[
        ("[Content_Types].xml", types.as_bytes().to_vec()),
        ("_rels\\.rels", root.into_bytes()),
        ("xl\\workbook.xml", wb.into_bytes()),
        ("xl\\_rels\\workbook.xml.rels", wb_rels.into_bytes()),
        ("xl\\sheet1.xml", ws),
    ]);
    let mut covered = 0;
    let found = round_trip(&bytes, Path::new("tdf76115.xlsx"), &mut covered);
    assert!(covered > 0, "the sheet check reads xl/sheet1.xml");
    assert_eq!(
        not_allowed(found),
        vec![(Kind::PartExtra, "xl/sharedStrings.xml".to_string())]
    );
}

/// #1108 r1: a directory entry written with `\` is a directory too, not a
/// part the save (which writes it with `/`) lost.
#[test]
fn a_backslash_directory_entry_is_no_part() {
    let types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#;
    let original = package(&[
        ("[Content_Types].xml", types.to_vec()),
        ("xl\\media\\", Vec::new()),
    ]);
    let saved = package(&[
        ("[Content_Types].xml", types.to_vec()),
        ("xl/media/", Vec::new()),
    ]);
    assert_eq!(compare_packages(&original, &saved).len(), 0);
    assert_eq!(parts_identical(&original, &saved), Ok(()));
}

/// #1108 r1: two entries that differ only in their separators would be one
/// part; the comparator cannot tell which one a loader read.
#[test]
fn entries_that_name_one_part_are_unreadable() {
    let two = package(&[
        ("xl/a.xml", b"<a/>".to_vec()),
        ("xl\\a.xml", b"<b/>".to_vec()),
    ]);
    let one = package(&[("xl/a.xml", b"<a/>".to_vec())]);
    let kinds = |found: Vec<Finding>| found.into_iter().map(|f| f.kind).collect::<Vec<_>>();
    assert_eq!(kinds(compare_packages(&two, &one)), [Kind::LoadError]);
    assert_eq!(kinds(compare_packages(&one, &two)), [Kind::PartBytes]);
    assert!(parts_identical(&two, &one).is_err());
    // The same name twice is no separator question: as before, the last
    // entry is the part.
    let twice = package(&[
        ("xl/a.xml", b"<b/>".to_vec()),
        ("xl/a.xml", b"<a/>".to_vec()),
    ]);
    assert_eq!(compare_packages(&twice, &one).len(), 0);
}

/// #1156: an empty entry with no extension, or with entries under it, is a
/// directory too: tdf124525.xlsx marks `_rels`, `xl`, ... as directories
/// only in their ZIP attributes, and the save no longer writes them.
#[test]
fn an_empty_entry_named_like_a_directory_is_no_part() {
    let types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#;
    let original = package(&[
        ("_rels", Vec::new()),
        ("xl", Vec::new()),
        ("xl/media", Vec::new()),
        ("[Content_Types].xml", types.to_vec()),
        ("xl/a.xml", b"<a/>".to_vec()),
    ]);
    let saved = package(&[
        ("[Content_Types].xml", types.to_vec()),
        ("xl/a.xml", b"<a/>".to_vec()),
    ]);
    assert_eq!(compare_packages(&original, &saved).len(), 0);
    assert_eq!(parts_identical(&original, &saved), Ok(()));
    // An empty part with an extension is still a part.
    let thumb = package(&[
        ("[Content_Types].xml", types.to_vec()),
        ("xl/a.xml", b"<a/>".to_vec()),
        ("docProps/thumbnail.wmf", Vec::new()),
    ]);
    let kinds: Vec<Kind> = compare_packages(&thumb, &saved)
        .into_iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(kinds, [Kind::PartMissing]);
    // So is an empty extensionless part an Override names (#1156 r1).
    let named = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/XL/customData" ContentType="application/octet-stream"/></Types>"#;
    let custom = package(&[
        ("[Content_Types].xml", named.to_vec()),
        ("xl/a.xml", b"<a/>".to_vec()),
        ("xl/customData", Vec::new()),
    ]);
    let without = package(&[
        ("[Content_Types].xml", named.to_vec()),
        ("xl/a.xml", b"<a/>".to_vec()),
    ]);
    let kinds: Vec<Kind> = compare_packages(&custom, &without)
        .into_iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(kinds, [Kind::PartMissing]);
}

/// `zip` with the external attributes of the central directory entries
/// named in `dirs` set to the MS-DOS directory bit, as tdf124525.xlsx has
/// them.
fn mark_directories(mut zip: Vec<u8>, dirs: &[&str]) -> Vec<u8> {
    let eocd = zip.len() - 22;
    let len = |zip: &[u8], at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]) as usize;
    let count = len(&zip, eocd + 10);
    let mut p = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    for _ in 0..count {
        let name_len = len(&zip, p + 28);
        let name = std::str::from_utf8(&zip[p + 46..p + 46 + name_len]).unwrap();
        if dirs.contains(&name) {
            zip[p + 38..p + 42].copy_from_slice(&0x10u32.to_le_bytes());
        }
        p += 46 + name_len + len(&zip, p + 30) + len(&zip, p + 32);
    }
    zip
}

/// The directory entries tdf124525.xlsx has.
const TDF124525_DIRS: &[&str] = &[
    "_rels",
    "docProps",
    "xl",
    "xl/_rels",
    "xl/theme",
    "xl/worksheets",
];

/// A workbook gridcore wrote, with tdf124525.xlsx's directory entries: empty,
/// no trailing `/`, directories only by their ZIP attributes.
fn with_tdf124525_directories() -> (Vec<u8>, Vec<String>) {
    let mut pkg = gridcore::xlsx::new_xlsx();
    pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(42.0));
    let parts = read_parts(&gridcore::xlsx::save_xlsx(&pkg)).unwrap();
    let names: Vec<String> = parts.keys().cloned().collect();
    let mut entries: Vec<(String, Vec<u8>)> = TDF124525_DIRS
        .iter()
        .map(|d| (d.to_string(), Vec::new()))
        .collect();
    entries.extend(parts);
    (
        mark_directories(opccore::zipwrite::write_zip(&entries), TDF124525_DIRS),
        names,
    )
}

/// The entry names of `zip`, sorted.
fn entry_names(zip: &[u8]) -> Vec<String> {
    let arc = opccore::zip::ZipArchive::open(zip).unwrap();
    let mut names: Vec<String> = arc.entries().iter().map(|e| e.name.clone()).collect();
    names.sort();
    names
}

/// #1156: the save of a package with tdf124525.xlsx's directory entries
/// writes exactly its parts, and none without a content type. Before the
/// fix it wrote `_rels`, `xl`, ... back as empty parts, and Excel repaired
/// the file.
#[test]
fn a_package_with_directory_entries_saves_only_its_parts() {
    let (original, names) = with_tdf124525_directories();
    assert_eq!(validate_package(&original), vec![]);
    let pkg = load_xlsx(&original).unwrap();
    let saved = save_xlsx_for_path(&pkg, Path::new("tdf124525.xlsx"));
    assert_eq!(validate_package(&saved), vec![]);
    assert_eq!(entry_names(&saved), names);
    let mut covered = 0;
    let (found, schema) =
        round_trip_validated(&original, Path::new("tdf124525.xlsx"), &mut covered);
    assert_eq!(not_allowed(found), vec![]);
    assert_eq!(schema, vec![]);
}

/// The (rule, part, child) of each violation in `pkg`.
fn violations(pkg: &[u8]) -> Vec<(&'static str, String, String)> {
    validate_package(pkg)
        .into_iter()
        .map(|v| (v.rule.as_str(), v.part, v.child))
        .collect()
}

const TYPES_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";

/// A package of a workbook and one worksheet, with `extra` entries.
fn validated(workbook: &str, worksheet: &str, extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
    validated_with_types(workbook, worksheet, extra, "")
}

/// [`validated`], with `types` added to `[Content_Types].xml`.
fn validated_with_types(
    workbook: &str,
    worksheet: &str,
    extra: &[(&str, Vec<u8>)],
    types: &str,
) -> Vec<u8> {
    let types = format!(
        r#"<Types xmlns="{TYPES_NS}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="XML" ContentType="application/xml"/><Default Extension="bin" ContentType="application/octet-stream"/>{types}<Override PartName="/XL/Workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    let wb_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet%201.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/x.xml" TargetMode="External"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="/xl/./styles.xml"/></Relationships>"#;
    let mut parts = vec![
        ("[Content_Types].xml", types.into_bytes()),
        ("_rels/.rels", PACKAGE_RELS.as_bytes().to_vec()),
        (
            "xl/workbook.xml",
            format!(r#"<workbook xmlns="{MAIN}">{workbook}</workbook>"#).into_bytes(),
        ),
        ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes().to_vec()),
        (
            "xl/worksheets/sheet 1.xml",
            format!(r#"<worksheet xmlns="{MAIN}">{worksheet}</worksheet>"#).into_bytes(),
        ),
        (
            "xl/styles.xml",
            format!(r#"<styleSheet xmlns="{MAIN}"/>"#).into_bytes(),
        ),
    ];
    parts.extend(extra.iter().cloned());
    package(&parts)
}

const SHEETS: &str = r#"<sheets><sheet name="A" sheetId="1"/></sheets>"#;

/// #1156: a valid package has no violations: names match case-insensitively,
/// targets are percent-decoded and resolved, external ones are skipped.
#[test]
fn the_validator_accepts_a_valid_package() {
    let ws = r#"<dimension ref="A1"/><sheetData><row r="1"><c r="A1"/><c r="B1"/></row><row><c/></row><row r="3"><c/><c r="C3"/></row></sheetData><pageMargins left="1" right="1" top="1" bottom="1" header="0" footer="0"/>"#;
    assert_eq!(violations(&validated(SHEETS, ws, &[])), vec![]);
    let mut covered = 0;
    let book = gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx());
    assert_eq!(validate_package(&book), vec![]);
    let (_, schema) = round_trip_validated(&book, Path::new("book.xlsx"), &mut covered);
    assert_eq!(schema, vec![]);
}

/// #1156: what Excel repairs is reported: an empty part with no content type
/// (the saved directory entry), a worksheet out of schema order, rows and
/// cells out of order, a workbook with no `sheets`, an Override and a
/// relationship naming no part.
#[test]
fn the_validator_reports_what_excel_repairs() {
    let ws = r#"<sheetData><row r="2"><c r="B2"/><c r="A2"/></row><row r="1"/><foo/></sheetData><dimension ref="A1"/><sheetData/>"#;
    let wb = r#"<bookViews/><bookViews/>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="t" Target="../nowhere.xml"/></Relationships>"#;
    let types = format!(
        r#"<Types xmlns="{TYPES_NS}"><Default Extension="rels" ContentType="r"/><Default Extension="xml" ContentType="x"/><Override PartName="/xl/gone.xml" ContentType="x"/></Types>"#
    );
    let pkg = validated(
        wb,
        ws,
        &[
            ("xl", Vec::new()),
            (
                "xl/worksheets/_rels/sheet 1.xml.rels",
                rels.as_bytes().to_vec(),
            ),
        ],
    );
    let mut found = violations(&pkg);
    found.sort();
    let s = |r: &'static str, p: &str, c: &str| (r, p.to_string(), c.to_string());
    let ws = "xl/worksheets/sheet 1.xml";
    let mut want = vec![
        s("no-content-type", "xl", "xl"),
        s(
            "dangling-target",
            "xl/worksheets/_rels/sheet 1.xml.rels",
            "xl/nowhere.xml",
        ),
        s("order", ws, "dimension"),
        s("duplicate", ws, "sheetData"),
        s("not-allowed", ws, "foo"),
        s("r-order", ws, "row"),
        s("r-order", ws, "c"),
        s("duplicate", "xl/workbook.xml", "bookViews"),
        s("missing", "xl/workbook.xml", "sheets"),
    ];
    want.sort();
    assert_eq!(found, want);
    // An Override naming no part, and a missing package relationships part.
    let pkg = package(&[("[Content_Types].xml", types.into_bytes())]);
    let mut found = violations(&pkg);
    found.sort();
    let mut want = vec![
        s("missing-part", "_rels/.rels", "_rels/.rels"),
        s("override-no-part", "[content_types].xml", "xl/gone.xml"),
    ];
    want.sort();
    assert_eq!(found, want);
}

/// A worksheet relationships part with one relationship to `target`.
fn sheet_rels(target: &str) -> (&'static str, Vec<u8>) {
    let rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="t" Target="{target}"/></Relationships>"#
    );
    ("xl/worksheets/_rels/sheet 1.xml.rels", rels.into_bytes())
}

/// #1156 r1: entry names, Override names and targets are compared
/// percent-decoded alike, so a part named with an escape is the target that
/// names it, and losing it is a new dangling target, not one the original
/// already (falsely) had.
#[test]
fn a_percent_named_part_is_the_target_that_names_it() {
    let original = validated(
        SHEETS,
        "<sheetData/>",
        &[
            sheet_rels("../media/a%23.bin"),
            ("xl/media/a%23.bin", b"1".to_vec()),
        ],
    );
    assert_eq!(violations(&original), vec![]);
    let dropped = validated(SHEETS, "<sheetData/>", &[sheet_rels("../media/a%23.bin")]);
    let added: Vec<_> = new_violations(&validate_package(&original), &validate_package(&dropped))
        .into_iter()
        .map(|v| (v.rule.as_str(), v.child))
        .collect();
    assert_eq!(added, [("dangling-target", "xl/media/a#.bin".to_string())]);
}

/// #1156 r1: a cell reference too long for a column number is skipped, not
/// an overflow.
#[test]
fn an_overlong_cell_reference_is_skipped() {
    let ws = r#"<sheetData><row r="1"><c r="ZZZZZZZZZZZZZZZZ1"/><c r="A1"/></row></sheetData>"#;
    assert_eq!(violations(&validated(SHEETS, ws, &[])), vec![]);
}

/// #1156 r1: a `_RELS` directory holds relationships too.
#[test]
fn relationships_in_an_upper_case_rels_directory_are_checked() {
    let (_, rels) = sheet_rels("../gone.xml");
    let pkg = validated(
        SHEETS,
        "<sheetData/>",
        &[("xl/worksheets/_RELS/sheet 1.xml.rels", rels)],
    );
    assert_eq!(
        violations(&pkg),
        [(
            "dangling-target",
            "xl/worksheets/_rels/sheet 1.xml.rels".to_string(),
            "xl/gone.xml".to_string()
        )]
    );
}

/// #1156 r1: a worksheet is found by its content type, not by a `.xml`
/// name: `xl/sheet2` with an Override is checked.
#[test]
fn a_worksheet_without_an_xml_name_is_checked() {
    let ws = format!(
        r#"<worksheet xmlns="{MAIN}"><dimension ref="A1"/><sheetPr/><sheetData/></worksheet>"#
    );
    let pkg = validated_with_types(
        SHEETS,
        "<sheetData/>",
        &[("xl/sheet2", ws.into_bytes())],
        r#"<Override PartName="/xl/sheet2" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
    );
    assert_eq!(
        violations(&pkg),
        [("order", "xl/sheet2".to_string(), "sheetPr".to_string())]
    );
}

/// #1156: Excel repaired the suite's unedited save of tdf124525.xlsx. Its
/// save has exactly its parts, and no package or schema violation the
/// original does not have.
#[test]
fn tdf124525_saves_without_its_directory_entries() {
    let (_, ext) = corpus(&workspace_root());
    let Some(dir) = ext else {
        assert!(
            !flag("FIDELITY_REQUIRE_CORPUS"),
            "fidelity: FIDELITY_REQUIRE_CORPUS=1 but no external corpus"
        );
        eprintln!("fidelity: SKIP tdf124525.xlsx: no external corpus");
        return;
    };
    let path = dir.join("libreoffice/sc/qa/unit/data/xlsx/tdf124525.xlsx");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let pkg = load_xlsx(&bytes).expect("tdf124525.xlsx loads");
    let saved = save_xlsx_for_path(&pkg, &path);
    let mut parts: Vec<String> = read_parts(&bytes).unwrap().into_keys().collect();
    parts.sort();
    assert_eq!(entry_names(&saved), parts);
    assert_eq!(validate_package(&saved), vec![]);
    assert_eq!(
        new_violations(&validate_package(&bytes), &validate_package(&saved)),
        vec![]
    );
}

#[test]
fn every_baseline_entry_is_classified() {
    // The gate checks this on every run; here it also runs without the
    // external corpus, for the entries of files it cannot see.
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fidelity");
    let baseline =
        parse_baseline(&std::fs::read_to_string(here.join("baseline.txt")).unwrap()).unwrap();
    assert_eq!(render_baseline(&baseline, CLASSES).1, vec![]);
}
