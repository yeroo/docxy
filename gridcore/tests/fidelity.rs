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

use std::collections::BTreeSet;
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
        name: "shared formulas are expanded to one formula per cell (f/@t, @si, @ref \
               dropped, formula text added); checked cell by cell when baselined: \
               same formulas, same values",
        issue: "by design: gridcore/src/xlsx.rs module docs",
        matches: |e| {
            in_sheet_data(e)
                && match e.kind {
                    Kind::LostAttr => ends_with_any(e, &["/c/f/@t", "/c/f/@si", "/c/f/@ref"]),
                    Kind::ExtraElement => ends_with_any(e, &["/c/f/text()"]),
                    _ => false,
                }
        },
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
        name: "formula text is re-serialized (spaces after commas dropped, _xlfn. added \
               to newer functions)",
        issue: "#1096",
        matches: |e| {
            in_sheet_data(e) && e.kind == Kind::ChangedValue && ends_with_any(e, &["/c/f/text()"])
        },
    },
    LossClass {
        name: "cells are re-encoded: cached string results (t=str) and inline strings \
               move to shared strings, duplicate shared strings remap to the first, \
               omitted or invalid cell references are written. Checked cell by cell \
               when baselined: values unchanged except a CR LF in a cached string \
               (tdf169326, the reader skips XML line-end normalization) and empty \
               cached strings of external array formulas (tdf76047); tdf100034's \
               style and cell findings are the comparator pairing cells after its \
               empty cells were dropped",
        issue: "#1091",
        matches: in_sheet_data,
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
        name: "a shared-strings part is added (with its override and relationship) or \
               recounted when cells move their strings to it",
        issue: "#1091",
        matches: |e| {
            e.part.starts_with("xl/sharedStrings")
                || (e.kind == Kind::ExtraElement
                    && (e.part == "[Content_Types].xml" || e.part == "xl/_rels/workbook.xml.rels"))
        },
    },
];

/// `path` without namespace prefixes, so `/x:worksheet/x:sheetData` reads
/// `/worksheet/sheetData`.
fn local(path: &str) -> String {
    path.split('/')
        .map(|step| step.rsplit_once(':').map_or(step, |(_, l)| l))
        .collect::<Vec<_>>()
        .join("/")
}

fn in_sheet_data(e: &Entry) -> bool {
    e.part.starts_with("xl/worksheets/") && local(&e.path).starts_with("/worksheet/sheetData/")
}

fn ends_with_any(e: &Entry, tails: &[&str]) -> bool {
    let path = local(&e.path);
    tails.iter().any(|t| path.ends_with(t))
}

/// An equivalence a glob cannot express: a finding that, checked against
/// the original part, changes nothing a reader sees. Applied before the
/// allowlist and counted like its rules.
struct Equivalence {
    reason: &'static str,
    /// The finding, and the root of the original part it is in.
    holds: fn(&Finding, &Elem) -> bool,
}

const EQUIVALENCES: &[Equivalence] = &[
    Equivalence {
        reason: "a number cell's cached value, or a column width, in another lexical form \
                 of the same double (0.0 -> 0, 0.14000000000000001 -> 0.14, 4.0 -> 4)",
        holds: same_number,
    },
    Equivalence {
        reason: "a boolean cell's cached value spelled 1/0 instead of true/false",
        holds: same_boolean,
    },
    Equivalence {
        reason: "an empty cell with the default style (<c r=\"..\"/>, s absent or 0) is not \
                 written: it holds no value, formula or format",
        holds: empty_cell,
    },
    Equivalence {
        reason: "a row with no attributes but r and spans, holding only such empty cells, \
                 is not written",
        holds: empty_row,
    },
];

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

/// The cell whose `<v>` text the finding is at.
fn value_cell<'a>(f: &Finding, root: &'a Elem) -> Option<&'a Elem> {
    let v = f.path.strip_suffix("/text()")?;
    let (cell, v) = v.rsplit_once('/')?;
    (v == "v" || v.ends_with(":v")).then_some(())?;
    at(root, cell).filter(|c| c.local == "c")
}

fn same_number(f: &Finding, root: &Elem) -> bool {
    if f.kind != Kind::ChangedValue {
        return false;
    }
    let number = |s: &str| s.parse::<f64>().ok().filter(|n| n.is_finite());
    let Some((Some(a), Some(b))) = change(f).map(|(a, b)| (number(a), number(b))) else {
        return false;
    };
    if a != b {
        return false;
    }
    let width = f.key_path().ends_with("/col/@width");
    width || value_cell(f, root).is_some_and(|c| matches!(attr(c, "t"), None | Some("n")))
}

fn same_boolean(f: &Finding, root: &Elem) -> bool {
    f.kind == Kind::ChangedValue
        && matches!(change(f), Some(("true", "1") | ("false", "0")))
        && value_cell(f, root).is_some_and(|c| attr(c, "t") == Some("b"))
}

fn is_empty_cell(c: &Elem) -> bool {
    c.local == "c"
        && c.children.is_empty()
        && c.attrs
            .iter()
            .all(|a| a.uri.is_empty() && (a.local == "r" || (a.local == "s" && a.value == "0")))
}

fn empty_cell(f: &Finding, root: &Elem) -> bool {
    f.kind == Kind::LostElement && at(root, &f.path).is_some_and(is_empty_cell)
}

fn empty_row(f: &Finding, root: &Elem) -> bool {
    f.kind == Kind::LostElement
        && at(root, &f.path).is_some_and(|row| {
            row.local == "row"
                && row
                    .attrs
                    .iter()
                    .all(|a| a.uri.is_empty() && matches!(a.local.as_str(), "r" | "spans"))
                && row.children.iter().all(|c| match c {
                    Node::Elem(e) => is_empty_cell(e),
                    Node::Text(_) => false,
                })
        })
}

/// Drop the findings an equivalence covers, counting them per rule.
fn apply_equivalences(found: Vec<Finding>, original: &[u8], counts: &mut [usize]) -> Vec<Finding> {
    let zip = opccore::zip::ZipArchive::open(original);
    let mut trees: std::collections::HashMap<String, Option<Elem>> = Default::default();
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
fn round_trip(bytes: &[u8], path: &Path) -> Vec<Finding> {
    match load_xlsx(bytes) {
        Ok(pkg) => compare_packages(bytes, &save_xlsx_for_path(&pkg, path)),
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
    for (file, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        present.insert(file.clone());
        let found = guarded(|| round_trip(&bytes, path));
        let found = apply_equivalences(found, &bytes, &mut equivalent);
        for f in apply_allowlist(found, &allow, &mut allowed) {
            findings.push((file.clone(), f));
        }
    }
    eprintln!(
        "fidelity: {} files, {} findings, {} equivalent, {} allowlisted",
        files.len(),
        findings.len(),
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

/// What the equivalences leave of the findings between two worksheets.
fn not_equivalent(original: &str, saved: &str) -> Vec<(Kind, String)> {
    let original = package(&[("xl/worksheets/sheet1.xml", sheet(original))]);
    let saved = package(&[("xl/worksheets/sheet1.xml", sheet(saved))]);
    let mut counts = vec![0; EQUIVALENCES.len()];
    apply_equivalences(compare_packages(&original, &saved), &original, &mut counts)
        .into_iter()
        .map(|f| (f.kind, f.path))
        .collect()
}

#[test]
fn a_dropped_cell_is_one_lost_element() {
    // The shared comparator, seen from the xlsx side: a lost cell with a
    // value is one finding at its path, not a cascade over its row.
    let found = not_equivalent(
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c></row>"#,
        r#"<row r="1"><c r="A1"><v>1</v></c><c r="C1"><v>3</v></c></row>"#,
    );
    assert_eq!(
        found,
        vec![(
            Kind::LostElement,
            "/worksheet/sheetData/row/c[2]".to_string()
        )]
    );
}

#[test]
fn the_same_number_in_another_spelling_is_equivalent_only_in_a_number_cell() {
    let row = |t: &str, v: &str| format!(r#"<row r="1"><c r="A1"{t}><v>{v}</v></c></row>"#);
    for (from, to) in [
        ("0.0", "0"),
        ("0.14000000000000001", "0.14"),
        ("1E+3", "1000"),
    ] {
        assert_eq!(
            not_equivalent(&row("", from), &row("", to)),
            vec![],
            "{from}"
        );
        assert_eq!(
            not_equivalent(&row(r#" t="n""#, from), &row(r#" t="n""#, to)),
            vec![]
        );
    }
    let changed = vec![(
        Kind::ChangedValue,
        "/worksheet/sheetData/row/c/v/text()".to_string(),
    )];
    // Another number, or the same digits in a string or shared-string cell.
    assert_eq!(not_equivalent(&row("", "0.1"), &row("", "0.2")), changed);
    assert_eq!(
        not_equivalent(&row(r#" t="str""#, "1.0"), &row(r#" t="str""#, "1")),
        changed
    );
    assert_eq!(
        not_equivalent(&row(r#" t="s""#, "01"), &row(r#" t="s""#, "1")),
        changed
    );
}

#[test]
fn a_boolean_spelled_one_is_equivalent_only_in_a_boolean_cell() {
    let row = |t: &str, v: &str| format!(r#"<row r="1"><c r="A1" t="{t}"><v>{v}</v></c></row>"#);
    assert_eq!(not_equivalent(&row("b", "true"), &row("b", "1")), vec![]);
    assert_eq!(not_equivalent(&row("b", "false"), &row("b", "0")), vec![]);
    assert_eq!(
        not_equivalent(&row("str", "true"), &row("str", "1")).len(),
        1
    );
    assert_eq!(not_equivalent(&row("b", "true"), &row("b", "0")).len(), 1);
}

#[test]
fn only_an_empty_default_styled_cell_or_row_may_be_dropped() {
    let a1 = r#"<c r="A1"><v>1</v></c>"#;
    let kept = format!(r#"<row r="1">{a1}</row>"#);
    // Dropped and equivalent: no children, no style.
    for empty in [r#"<c r="B1"/>"#, r#"<c r="B1" s="0"/>"#] {
        let original = format!(r#"<row r="1">{a1}{empty}</row>"#);
        assert_eq!(not_equivalent(&original, &kept), vec![], "{empty}");
    }
    // Dropped and reported: a style, a type, a value, a formula.
    for cell in [
        r#"<c r="B1" s="3"/>"#,
        r#"<c r="B1" t="s"/>"#,
        r#"<c r="B1"><v>2</v></c>"#,
        r#"<c r="B1"><f>A1</f></c>"#,
    ] {
        let original = format!(r#"<row r="1">{a1}{cell}</row>"#);
        assert_eq!(not_equivalent(&original, &kept).len(), 1, "{cell}");
    }
    // A row of empty cells may go; one with a height, or a styled cell, may not.
    let rows = |second: &str| format!(r#"{kept}{second}"#);
    assert_eq!(
        not_equivalent(&rows(r#"<row r="2" spans="1:2"><c r="A2"/></row>"#), &kept),
        vec![]
    );
    assert_eq!(not_equivalent(&rows(r#"<row r="2"/>"#), &kept), vec![]);
    assert_eq!(
        not_equivalent(&rows(r#"<row r="2" ht="30" customHeight="1"/>"#), &kept).len(),
        1
    );
    assert_eq!(
        not_equivalent(&rows(r#"<row r="2"><c r="A2" s="4"/></row>"#), &kept).len(),
        1
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
    let found = apply_equivalences(
        round_trip(&bytes, Path::new("book.xlsx")),
        &bytes,
        &mut counts,
    );
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
