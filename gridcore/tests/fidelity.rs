//! Round-trip fidelity gate for xlsx (#1064, the xlsx half of #1060): open
//! every corpus `.xlsx`, save it with no edits, and compare every package part
//! with the original. Fails on any loss not covered by
//! `fidelity/allowlist.txt` or `fidelity/baseline.txt`, and on a baseline
//! entry that no longer reproduces. See `docs/fidelity-gate.md`.
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use comparator::*;
use gridcore::xlsx::{load_xlsx, save_xlsx_for_path};

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
        name: "ZIP64 container is refused (XlsxError::CorruptPart); nothing is saved",
        issue: "#1094",
        matches: |e| e.kind == Kind::LoadError && e.file.contains("zip64"),
    },
    LossClass {
        name: "part names with backslash separators (xl\\workbook.xml) are not found \
               (XlsxError::MissingWorkbook); nothing is saved",
        issue: "#1095",
        matches: |e| e.kind == Kind::LoadError && e.file.ends_with("/tdf76115.xlsx"),
    },
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
        name: "formula text is re-serialized (spaces dropped, function names upper-cased, \
               _xlfn. added to newer functions): one entry per cell",
        issue: "#1096",
        matches: |e| is_cell(e, "formula"),
    },
    LossClass {
        name: "cell values changed: a cached string's literal CR LF, which XML reads \
               as LF, is written back with the CR escaped (tdf169326: the reader skips \
               XML line-end normalization); a cell past the last column (XFE1) lands \
               on XFC1 (too-many-cols-rows): one entry per cell",
        issue: "#1091",
        matches: |e| is_cell(e, "value"),
    },
    LossClass {
        name: "the dimension is recomputed from the cells the model keeps (a source \
               dimension covering dropped empty cells, or a wrong one, changes)",
        issue: "#1096",
        matches: |e| {
            e.part.starts_with("xl/worksheets/") && local(&e.path) == "/worksheet/dimension/@ref"
        },
    },
    LossClass {
        name: "column definitions are regenerated: customWidth=\"1\" on every explicit \
               width (also where the source said false or nothing), <col> split or added",
        issue: "#1093",
        matches: |e| {
            e.part.starts_with("xl/worksheets/") && local(&e.path).starts_with("/worksheet/cols")
        },
    },
    LossClass {
        name: "docProps/app.xml sheet titles follow the workbook's sheets (a stale \
               source list is corrected; named-range titles in it are dropped)",
        issue: "#1096",
        matches: |e| e.part == "docProps/app.xml",
    },
    LossClass {
        name: "cells are re-encoded: a shared-strings part is added (with its override \
               and relationship) or recounted when cached string results and inline \
               strings move to it, and row numbers are written where the source \
               omitted them (the cell check finds the cells unchanged)",
        issue: "#1091",
        matches: |e| {
            e.part.starts_with("xl/sharedStrings")
                || (e.kind == Kind::ExtraElement
                    && (e.part == "[Content_Types].xml" || e.part == "xl/_rels/workbook.xml.rels"))
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
// The cell check
//
// A worksheet's <sheetData> is regenerated on every save, so its XML differs
// from the source in ways that change nothing a reader sees (0.14000000000000001
// written 0.14, an inline string moved to the shared strings, a shared formula
// expanded per cell), and the structural findings for them cannot be told
// from a real loss by path or detail. So each worksheet is also read the way
// a spreadsheet reads it: per cell, its value (shared and inline strings
// resolved, with their run formatting; numbers as doubles), its formula and
// its effective style (the cell's own, else its row's, else its column's).
// Any difference is a finding at `/cells/<ref>/value|formula|style`: one
// baseline line per cell, so a listed loss covers that cell only. The
// structural findings this check covers (cells, their values, formulas and
// the cell and formula attributes it reads) are then dropped.

/// A cell's value as a reader sees it. Text keeps its runs: (the run
/// properties' canonical hash, the text), with adjacent runs of the same
/// properties merged; phonetic runs are not read.
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

/// A cell's formula: its text, and its kind and range for an array or data
/// table formula. A shared formula's follower has no text of its own: the
/// writer expands it, so any formula there agrees with it.
#[derive(Clone, Debug, PartialEq)]
enum Formula {
    Own { text: String, kind: Option<String> },
    SharedFollower,
}

#[derive(Clone, Debug, PartialEq)]
struct CellRead {
    value: Value,
    formula: Option<Formula>,
    style: String,
}

#[derive(Default)]
struct SheetRead {
    cells: BTreeMap<(u32, u32), CellRead>,
    /// The style of a row with `customFormat`, which its missing cells take.
    row_styles: HashMap<u32, String>,
    /// (first col, last col, style), 0-based, from `<cols>`.
    col_styles: Vec<(u32, u32, String)>,
}

impl SheetRead {
    /// What a cell the sheet does not list reads as.
    fn missing(&self, row: u32, col: u32) -> CellRead {
        let style = self
            .row_styles
            .get(&row)
            .or_else(|| {
                self.col_styles
                    .iter()
                    .find(|(lo, hi, _)| (*lo..=*hi).contains(&col))
                    .map(|(_, _, s)| s)
            })
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

/// The runs of an `<si>` or `<is>`.
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

/// `B12` as 0-based (row, col).
fn parse_ref(r: &str) -> Option<(u32, u32)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(split);
    if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let col = letters.bytes().try_fold(0u32, |n, b| {
        n.checked_mul(26)?.checked_add(u32::from(b - b'A') + 1)
    })?;
    let row: u32 = digits.parse().ok()?;
    Some((row.checked_sub(1)?, col.checked_sub(1)?))
}

fn ref_name(row: u32, col: u32) -> String {
    let mut letters = Vec::new();
    let mut n = col + 1;
    while n > 0 {
        letters.push(b'A' + ((n - 1) % 26) as u8);
        n = (n - 1) / 26;
    }
    letters.reverse();
    format!("{}{}", String::from_utf8(letters).unwrap(), row + 1)
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true"))
}

fn read_cell(c: &Elem, sst: &[Vec<(Option<u64>, String)>]) -> CellRead {
    let t = attr(c, "t").unwrap_or("n");
    let v = elems(c, "v").next().map(text_of);
    let value = match (t, v) {
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
    };
    let formula = elems(c, "f").next().map(|f| {
        let kind = attr(f, "t").filter(|k| !matches!(*k, "normal" | "shared"));
        let text = text_of(f);
        if attr(f, "t") == Some("shared") && text.is_empty() {
            Formula::SharedFollower
        } else {
            Formula::Own {
                text,
                kind: kind.map(|k| format!("{k} {}", attr(f, "ref").unwrap_or(""))),
            }
        }
    });
    CellRead {
        value,
        formula,
        style: attr(c, "s").unwrap_or("0").to_string(),
    }
}

fn read_sheet(root: &Elem, sst: &[Vec<(Option<u64>, String)>]) -> SheetRead {
    let mut sheet = SheetRead::default();
    for cols in elems(root, "cols") {
        for col in elems(cols, "col") {
            let n = |a| attr(col, a).and_then(|v| v.parse::<u32>().ok());
            if let (Some(lo), Some(hi)) = (n("min"), n("max")) {
                let style = attr(col, "style").unwrap_or("0").to_string();
                sheet
                    .col_styles
                    .push((lo.saturating_sub(1), hi.saturating_sub(1), style));
            }
        }
    }
    let mut next_row = 0;
    for data in elems(root, "sheetData") {
        for row in elems(data, "row") {
            let r = attr(row, "r")
                .and_then(|r| r.parse::<u32>().ok())
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
                sheet.cells.insert(at, read_cell(c, sst));
            }
        }
    }
    sheet
}

/// The worksheet and shared-strings parts of a package, parsed, by name.
fn trees(bytes: &[u8]) -> BTreeMap<String, Elem> {
    let Some(zip) = opccore::zip::ZipArchive::open(bytes) else {
        return BTreeMap::new();
    };
    zip.entries()
        .iter()
        .filter(|e| {
            let name = e.name.to_ascii_lowercase();
            name.ends_with(".xml")
                && (name.starts_with("xl/worksheets/") || name.contains("sharedstrings"))
        })
        .filter_map(|e| Some((e.name.clone(), parse_xml(&zip.extract(e)?)?)))
        .collect()
}

fn shared_strings(trees: &BTreeMap<String, Elem>) -> Vec<Vec<(Option<u64>, String)>> {
    trees
        .values()
        .find(|t| t.local == "sst")
        .map(|sst| elems(sst, "si").map(runs).collect())
        .unwrap_or_default()
}

/// The cell findings between two worksheets.
fn compare_cells(part: &str, a: &SheetRead, b: &SheetRead) -> Vec<Finding> {
    let keys: BTreeSet<(u32, u32)> = a.cells.keys().chain(b.cells.keys()).copied().collect();
    let mut out = Vec::new();
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
        let mut finding = |what: &str, detail: String| {
            out.push(Finding {
                part: part.to_string(),
                kind: Kind::ChangedValue,
                path: format!("/cells/{cell}/{what}"),
                detail,
            })
        };
        if x.value != y.value {
            finding("value", format!("{} -> {}", x.value.show(), y.value.show()));
        }
        let formula_agrees = match (&x.formula, &y.formula) {
            (Some(Formula::SharedFollower), Some(_)) => true,
            (a, b) => a == b,
        };
        if !formula_agrees {
            finding("formula", format!("{:?} -> {:?}", x.formula, y.formula));
        }
        if x.style != y.style {
            finding("style", format!("{:?} -> {:?}", x.style, y.style));
        }
    }
    out
}

/// Whether the cell check covers a structural finding: the cell elements and
/// what it reads of them. Row attributes, cell attributes other than r, s
/// and t, and formula attributes other than t, si and ref stay structural.
fn covered_by_cells(f: &Finding, original: &Elem, saved: &Elem) -> bool {
    let path = local(&f.key_path());
    let Some(rest) = path.strip_prefix("/worksheet/sheetData/row") else {
        return false;
    };
    match rest {
        // A row element itself is covered when it carries nothing but its
        // number and span hint: its cells are compared one by one.
        "" => {
            let tree = if f.kind == Kind::ExtraElement {
                saved
            } else {
                original
            };
            matches!(f.kind, Kind::LostElement | Kind::ExtraElement)
                && at(tree, &f.path).is_some_and(|row| {
                    row.attrs
                        .iter()
                        .all(|a| a.uri.is_empty() && matches!(a.local.as_str(), "r" | "spans"))
                })
        }
        "/c" | "/c/@r" | "/c/@s" | "/c/@t" => true,
        "/c/f" | "/c/f/@t" | "/c/f/@si" | "/c/f/@ref" | "/c/f/@space" | "/c/f/text()" => true,
        _ => rest.starts_with("/c/v") || rest.starts_with("/c/is"),
    }
}

/// Replace the structural findings of each worksheet both packages hold
/// readably by the cell check's; `covered` counts those dropped.
fn check_cells(
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
        let checked = sheets.contains(&f.part);
        if checked && covered_by_cells(&f, &a[&f.part], &b[&f.part]) {
            *covered += 1;
        } else {
            out.push(f);
        }
    }
    for name in sheets {
        let (x, y) = (read_sheet(&a[name], &sst_a), read_sheet(&b[name], &sst_b));
        out.extend(compare_cells(name, &x, &y));
    }
    out
}

// ---------------------------------------------------------------------------
// Equivalences

/// An equivalence a glob cannot express: a finding that, checked against
/// the original part, changes nothing a reader sees. Applied before the
/// allowlist and counted like its rules.
struct Equivalence {
    reason: &'static str,
    /// The finding, and the root of the original part it is in.
    holds: fn(&Finding, &Elem) -> bool,
}

const EQUIVALENCES: &[Equivalence] = &[Equivalence {
    reason: "a column width in another lexical form of the same double (4.0 -> 4)",
    holds: same_width,
}];

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

/// `"old" -> "new"` of a changed value, unquoted (the comparator's Debug
/// quoting; the values compared here need no unescaping).
fn change(f: &Finding) -> Option<(&str, &str)> {
    let (a, b) = f.detail.split_once(" -> ")?;
    Some((
        a.strip_prefix('"')?.strip_suffix('"')?,
        b.strip_prefix('"')?.strip_suffix('"')?,
    ))
}

fn same_width(f: &Finding, _: &Elem) -> bool {
    let number = |s: &str| s.parse::<f64>().ok().filter(|n| n.is_finite());
    f.kind == Kind::ChangedValue
        && local(&f.key_path()) == "/worksheet/cols/col/@width"
        && change(f).is_some_and(|(a, b)| number(a).is_some() && number(a) == number(b))
}

/// Drop the findings an equivalence covers, counting them per rule.
fn apply_equivalences(found: Vec<Finding>, original: &[u8], counts: &mut [usize]) -> Vec<Finding> {
    let zip = opccore::zip::ZipArchive::open(original);
    let mut trees: HashMap<String, Option<Elem>> = HashMap::new();
    let mut kept = Vec::new();
    for f in found {
        let root = trees
            .entry(f.part.clone())
            .or_insert_with(|| zip.as_ref()?.read(&f.part).and_then(|b| parse_xml(&b)));
        let rule = root
            .as_ref()
            .and_then(|root| EQUIVALENCES.iter().position(|e| (e.holds)(&f, root)));
        match rule {
            Some(i) => counts[i] += 1,
            None => kept.push(f),
        }
    }
    kept
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
    match load_xlsx(bytes) {
        Ok(pkg) => {
            let saved = save_xlsx_for_path(&pkg, path);
            check_cells(bytes, &saved, compare_packages(bytes, &saved), covered)
        }
        Err(e) => vec![Finding {
            part: String::new(),
            kind: Kind::LoadError,
            path: String::new(),
            detail: format!("{e:?}"),
        }],
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
    let mut equivalent = vec![0usize; EQUIVALENCES.len()];
    let mut covered = 0usize;
    for (file, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        present.insert(file.clone());
        let found = guarded(|| round_trip(&bytes, path, &mut covered));
        let found = apply_equivalences(found, &bytes, &mut equivalent);
        for f in apply_allowlist(found, &allow, &mut allowed) {
            findings.push((file.clone(), f));
        }
    }
    eprintln!(
        "fidelity: {} files, {} findings, {} covered by the cell check, {} equivalent, \
         {} allowlisted",
        files.len(),
        findings.len(),
        covered,
        equivalent.iter().sum::<usize>(),
        allowed.iter().sum::<usize>()
    );
    for (rule, n) in EQUIVALENCES.iter().zip(&equivalent) {
        eprintln!("fidelity:   {n:>6} equivalent: {}", rule.reason);
    }
    for (rule, n) in allow.iter().zip(&allowed) {
        eprintln!(
            "fidelity:   {n:>6} allowed: {} {} ({})",
            rule.part, rule.path, rule.reason
        );
    }

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

// ---------------------------------------------------------------------------
// Unit tests: the xlsx side of the shared comparator, the equivalences and
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

const SST: &str = "xl/sharedStrings.xml";

fn sst(items: &str) -> Vec<u8> {
    format!(r#"<sst xmlns="{MAIN}">{items}</sst>"#).into_bytes()
}

/// What the cell check (and the equivalences) leave of the findings between
/// two packages of one worksheet and, when given, shared strings.
fn after_check(original: (&str, &str), saved: (&str, &str)) -> Vec<(Kind, String)> {
    let pkg = |(rows, strings): (&str, &str)| {
        let mut parts = vec![("xl/worksheets/sheet1.xml", sheet(rows))];
        if !strings.is_empty() {
            parts.push((SST, sst(strings)));
        }
        package(&parts)
    };
    let (original, saved) = (pkg(original), pkg(saved));
    let mut covered = 0;
    let found = check_cells(
        &original,
        &saved,
        compare_packages(&original, &saved),
        &mut covered,
    );
    let mut counts = vec![0; EQUIVALENCES.len()];
    apply_equivalences(found, &original, &mut counts)
        .into_iter()
        .filter(|f| f.part != SST)
        .map(|f| (f.kind, f.path))
        .collect()
}

/// [`after_check`] of two worksheets without shared strings.
fn cells(original: &str, saved: &str) -> Vec<(Kind, String)> {
    after_check((original, ""), (saved, ""))
}

fn at_cell(cell: &str, what: &str) -> Vec<(Kind, String)> {
    vec![(Kind::ChangedValue, format!("/cells/{cell}/{what}"))]
}

#[test]
fn a_dropped_cell_is_one_lost_element() {
    // The shared comparator, seen from the xlsx side: a lost cell with a
    // value is one finding at its path, not a cascade over its row.
    let pkg = |rows: &str| package(&[("xl/worksheets/sheet1.xml", sheet(rows))]);
    let found: Vec<_> = compare_packages(
        &pkg(r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c></row>"#),
        &pkg(r#"<row r="1"><c r="A1"><v>1</v></c><c r="C1"><v>3</v></c></row>"#),
    )
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
    // The cell check reports it at the cell, and covers the structural one.
    assert_eq!(
        cells(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c></row>"#,
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="C1"><v>3</v></c></row>"#,
        ),
        at_cell("B1", "value")
    );
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
fn strings_are_read_through_the_shared_strings_with_their_runs() {
    let s = |i: &str| format!(r#"<row r="1"><c r="A1" t="s"><v>{i}</v></c></row>"#);
    let inline = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>dup</t></is></c></row>"#;
    let strings = "<si><t>dup</t></si><si><t>dup</t></si><si><t>other</t></si>";
    // A duplicate remapped to its first copy, an inline string moved to the
    // shared strings, a cached string result moved there: the same text.
    assert_eq!(after_check((&s("1"), strings), (&s("0"), strings)), vec![]);
    assert_eq!(after_check((inline, strings), (&s("0"), strings)), vec![]);
    let str_result = r#"<row r="1"><c r="A1" t="str"><f>B1</f><v>dup</v></c></row>"#;
    let s_result = r#"<row r="1"><c r="A1" t="s"><f>B1</f><v>0</v></c></row>"#;
    assert_eq!(
        after_check((str_result, strings), (s_result, strings)),
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
fn an_expanded_shared_formula_agrees_and_a_changed_formula_does_not() {
    let original = r#"<row r="1"><c r="A1"><f t="shared" ref="A1:B1" si="0">C1+1</f><v>1</v></c><c r="B1"><f t="shared" si="0"/><v>1</v></c></row>"#;
    let expanded =
        r#"<row r="1"><c r="A1"><f>C1+1</f><v>1</v></c><c r="B1"><f>D1+1</f><v>1</v></c></row>"#;
    assert_eq!(cells(original, expanded), vec![]);
    let changed =
        r#"<row r="1"><c r="A1"><f>C1 + 1</f><v>1</v></c><c r="B1"><f>D1+1</f><v>1</v></c></row>"#;
    assert_eq!(cells(original, changed), at_cell("A1", "formula"));
    // A follower that lost its formula, an array formula that lost its range.
    let lost = r#"<row r="1"><c r="A1"><f>C1+1</f><v>1</v></c><c r="B1"><v>1</v></c></row>"#;
    assert_eq!(cells(original, lost), at_cell("B1", "formula"));
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
    // style: dropped, the cell would take that style (#1064 r1, M1).
    let styled_row =
        |cells: &str| format!(r#"<row r="1" s="5" customFormat="1">{a1}{cells}</row>"#);
    assert_eq!(
        cells(&styled_row(r#"<c r="B1"/>"#), &styled_row("")),
        at_cell("B1", "style")
    );
    let col_styled = |rows: &str| {
        format!(r#"<cols><col min="2" max="2" style="7"/></cols><sheetData>{rows}</sheetData>"#)
    };
    let sheet_with =
        |inner: String| format!(r#"<worksheet xmlns="{MAIN}">{inner}</worksheet>"#).into_bytes();
    let original = package(&[(
        "xl/worksheets/sheet1.xml",
        sheet_with(col_styled(&format!(r#"<row r="1">{a1}<c r="B1"/></row>"#))),
    )]);
    let saved = package(&[("xl/worksheets/sheet1.xml", sheet_with(col_styled(&kept)))]);
    let mut covered = 0;
    let found: Vec<_> = check_cells(
        &original,
        &saved,
        compare_packages(&original, &saved),
        &mut covered,
    )
    .into_iter()
    .map(|f| (f.kind, f.path))
    .collect();
    assert_eq!(found, at_cell("B1", "style"));
}

#[test]
fn row_and_cell_attributes_the_check_does_not_read_stay_structural() {
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
}

#[test]
fn cell_references_read_as_written_or_by_position() {
    assert_eq!(parse_ref("A1"), Some((0, 0)));
    assert_eq!(parse_ref("AB12"), Some((11, 27)));
    assert_eq!(parse_ref("1_1"), None);
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
    // left after the equivalences and the allowlist is a regression.
    use gridcore::sheet::Cell;
    let mut pkg = gridcore::xlsx::new_xlsx();
    let s = &mut pkg.workbook.sheets[0];
    s.set_cell(0, 0, Cell::number(0.1));
    s.set_cell(0, 1, Cell::text("text"));
    s.set_cell(2, 3, Cell::formula("A1*2"));
    let bytes = gridcore::xlsx::save_xlsx(&pkg);
    let mut counts = vec![0; EQUIVALENCES.len()];
    let mut covered = 0;
    let found = round_trip(&bytes, Path::new("book.xlsx"), &mut covered);
    let found = apply_equivalences(found, &bytes, &mut counts);
    assert_eq!(not_allowed(found), vec![]);
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
