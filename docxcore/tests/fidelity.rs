//! Round-trip fidelity gate (#1060): open every corpus `.docx`, save it with
//! no edits, and compare every package part with the original. Fails on any
//! loss not covered by `fidelity/allowlist.txt` or `fidelity/baseline.txt`,
//! on a baseline entry that no longer reproduces, and on a schema violation
//! the save added (`fidelity/schema.rs`, #1083). See `docs/fidelity-gate.md`.
//!
//! - `FIDELITY_CORPUS=<dir>`: the docxy-corpus `files/` checkout (default
//!   `corpus/files`; relative paths resolve against the workspace root).
//! - `FIDELITY_REQUIRE_CORPUS=1`: fail instead of skipping when it is absent.
//! - `FIDELITY_UPDATE_BASELINE=1`: rewrite the baseline for the files in this
//!   run instead of judging against it.

// `mod fidelity;` would be ambiguous with this file's own name.
#[path = "fidelity/mod.rs"]
mod comparator;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use comparator::schema::{Violation, new_violations, validate, validate_package};
use comparator::*;
use docxcore::editor::Editor;
use docxcore::package::{load_package, save_package, save_package_preserving_document};

/// Repo-tracked `.docx` that always run, so the gate is never vacuous.
const TRACKED: &[&str] = &[
    "assets",
    "corpus/word-import",
    "corpus/legacy/word",
    "docxcore/tests/fixtures",
];

/// Every class of known loss and the issue that owns its fix, first match
/// wins. A baseline entry no class claims fails the gate.
const CLASSES: &[LossClass] = &[
    LossClass {
        name: "password-encrypted package (an OLE2 file) is refused; nothing is saved",
        issue: "by design: LoadError::Ole2",
        matches: |e| e.kind == Kind::LoadError && e.file.contains("password"),
    },
    LossClass {
        name: "a w14:paraId the source repeats is dropped on save (paragraph ids must be unique)",
        issue: "by design: #1063",
        matches: |e| {
            e.kind == Kind::LostAttr
                && e.file.contains("conflicting IDs")
                && e.path.ends_with("/@w14:paraId")
        },
    },
    LossClass {
        name: "modeled property values rewritten on save (values outside the supported set, \
               properties added)",
        issue: "#1068",
        matches: |e| e.part == "word/document.xml" && in_properties(&e.path) && !misaligned_runs(e),
    },
    LossClass {
        name: "run and paragraph content restructured on save (runs merged or split, \
               lastRenderedPageBreak, row-level sdt, special characters)",
        issue: "#1069",
        matches: |e| e.part == "word/document.xml" && e.path.starts_with("/w:document/w:body/"),
    },
];

/// Run-property findings that are no property loss: a run split by #1069 (a
/// `w:tab` split off its text, a dropped `w:lastRenderedPageBreak`) makes the comparator pair a run with a different
/// one, so intact properties read as lost, changed or added. Checked per file
/// when #1063 was fixed: every character's run properties and run attributes
/// are the same in the original and the saved file. Listed entry by entry (file,
/// kind, index-free path under `/w:document/w:body`), so the list cannot claim
/// a loss in a file or at a path it does not name; such a loss is classed by
/// the rules below like any other.
const MISALIGNED_RUNS: &[(&str, &[(Kind, &str)])] = &[
    (
        "ext:Normalize/complex0.docx",
        &[(Kind::LostElement, "/w:p/w:r/w:rPr")],
    ),
    (
        "ext:bookmark/bookmark.docx",
        &[
            (Kind::LostElement, "/w:p/w:r/w:rPr"),
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:vertAlign"),
        ],
    ),
    (
        "ext:complex0.docx",
        &[(Kind::LostElement, "/w:p/w:r/w:rPr")],
    ),
    (
        "ext:complexDocx/complex0.docx",
        &[(Kind::LostElement, "/w:p/w:r/w:rPr")],
    ),
    (
        "ext:footer & header/BackgroundReport - THE WORKING GROUP ON INTERNET GOVERNANCE.docx",
        &[
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:color"),
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:i"),
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:lang"),
            (Kind::ExtraElement, "/w:p/w:r/w:rPr/w:b"),
            (Kind::ExtraElement, "/w:p/w:r/w:rPr/w:lang"),
        ],
    ),
    (
        "ext:mixed features/BackgroundReport - THE WORKING GROUP ON INTERNET GOVERNANCE.docx",
        &[
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:color"),
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:i"),
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:lang"),
            (Kind::ExtraElement, "/w:p/w:r/w:rPr/w:b"),
            (Kind::ExtraElement, "/w:p/w:r/w:rPr/w:lang"),
        ],
    ),
    (
        "ext:table/2007.docx",
        &[
            (Kind::LostElement, "/w:p/w:r/w:rPr/w:spacing"),
            (Kind::ExtraElement, "/w:p/w:r/w:rPr/w:spacing"),
        ],
    ),
];

/// Whether `e` is one of [`MISALIGNED_RUNS`]; the #1069 class then claims it.
fn misaligned_runs(e: &Entry) -> bool {
    e.part == "word/document.xml"
        && e.path
            .strip_prefix("/w:document/w:body")
            .is_some_and(|path| {
                MISALIGNED_RUNS
                    .iter()
                    .any(|(file, entries)| *file == e.file && entries.contains(&(e.kind, path)))
            })
}

/// Property containers of modeled elements. A path through one is property
/// state; a path ending at one is the whole container.
const PROPERTIES: &[&str] = &[
    "w:pPr",
    "w:rPr",
    "w:tblPr",
    "w:tblPrEx",
    "w:trPr",
    "w:tcPr",
    "w:sectPr",
    "w:numPr",
];

fn in_properties(path: &str) -> bool {
    path.split('/').any(|step| PROPERTIES.contains(&step))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("docxcore sits in the workspace root")
        .to_path_buf()
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Collect the `.docx` under `dir`. An unreadable directory is an error, not
/// an empty one: a silently shrunken corpus would pass vacuously.
fn docx_files(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("fidelity: {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("fidelity: {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            if recursive {
                docx_files(&path, true, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
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
        docx_files(&root.join(dir), false, &mut found);
        files.extend(found.into_iter().map(|p| (key("repo", root, &p), p)));
    }
    let ext = std::env::var("FIDELITY_CORPUS").unwrap_or_else(|_| "corpus/files".into());
    let ext = root.join(ext); // an absolute FIDELITY_CORPUS replaces the root
    let ext = ext.is_dir().then_some(ext);
    if let Some(dir) = &ext {
        let mut found = Vec::new();
        docx_files(dir, true, &mut found);
        files.extend(found.into_iter().map(|p| (key("ext", dir, &p), p)));
    }
    files.sort();
    (files, ext)
}

/// One file's round trip: its findings, whether the preserving save is
/// byte-identical, the schema violations the save introduced, and how the
/// document's effective text changed, if it did.
struct RoundTrip {
    findings: Vec<Finding>,
    preserving: Result<(), String>,
    schema: Vec<Violation>,
    text_change: Option<String>,
}

/// The round trip under test: what docxy's save does for a modified
/// document's untouched content, and what the suite's save does for every
/// document, edited or not (#1083). docxy's own no-edit save takes the
/// preserving path instead; `preserving` reports whether that one is
/// byte-identical.
fn round_trip(bytes: &[u8]) -> RoundTrip {
    let mut pkg = match load_package(bytes) {
        Ok(pkg) => pkg,
        Err(e) => {
            let f = Finding {
                part: String::new(),
                kind: Kind::LoadError,
                path: String::new(),
                detail: format!("{e:?}"),
            };
            return RoundTrip {
                findings: vec![f],
                preserving: Ok(()),
                schema: Vec::new(),
                text_change: None,
            };
        }
    };
    let preserving = parts_identical(bytes, &save_package_preserving_document(&pkg));
    pkg.document = Editor::new(pkg.document.clone()).doc;
    let saved = save_package(&pkg);
    RoundTrip {
        findings: compare_packages(bytes, &saved),
        preserving,
        schema: new_violations(&validate_package(bytes), &validate_package(&saved)),
        text_change: effective_text_change(bytes, &saved),
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
            "fidelity: FIDELITY_REQUIRE_CORPUS=1 but no external corpus at FIDELITY_CORPUS \
             (default corpus/files); fetch it with corpus/tools/fetch-corpus.sh"
        ),
        None => eprintln!(
            "fidelity: SKIP external corpus: no docxy-corpus checkout at FIDELITY_CORPUS \
             (default corpus/files). Running the repo-tracked .docx only; fetch the rest \
             with corpus/tools/fetch-corpus.sh"
        ),
    }

    let mut findings: Vec<(String, Finding)> = Vec::new();
    let mut present = BTreeSet::new();
    let mut not_preserved = Vec::new();
    let mut invalid = Vec::new();
    let mut text_lost = Vec::new();
    let mut allowed = vec![0usize; allow.len()];
    for (file, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        present.insert(file.clone());
        let mut preserved = Ok(());
        let mut schema = Vec::new();
        let mut text_change = None;
        let found = guarded(|| {
            let rt = round_trip(&bytes);
            preserved = rt.preserving;
            schema = rt.schema;
            text_change = rt.text_change;
            rt.findings
        });
        if let Some(why) = text_change {
            text_lost.push(format!("{file}: {why}"));
        }
        if let Err(why) = preserved {
            not_preserved.push(format!("{file}: {why}"));
        }
        invalid.extend(schema.iter().map(|v| v.line(file)));
        for f in apply_allowlist(found, &allow, &mut allowed) {
            findings.push((file.clone(), f));
        }
    }
    eprintln!(
        "fidelity: {} files, {} findings, {} allowlisted",
        files.len(),
        findings.len(),
        allowed.iter().sum::<usize>()
    );
    for (rule, n) in allow.iter().zip(&allowed) {
        eprintln!(
            "fidelity:   {n:>6} allowed: {} ({})",
            rule.path, rule.reason
        );
    }
    assert!(
        not_preserved.is_empty(),
        "save_package_preserving_document (the no-edit save) changed these packages: {not_preserved:?}"
    );
    // The text Word shows (`effective_text`: hyphens, tabs and breaks as its
    // control characters) must survive the save. An element compare can file
    // a character lost in a restructured run as one more #1069 entry; this
    // cannot, and it is never baselined (#1101).
    assert!(
        text_lost.is_empty(),
        "fidelity gate: the save changed the effective text of {} files (#1101)\n{}",
        text_lost.len(),
        text_lost.join("\n")
    );
    // Word rejects these as corrupt, lost or not: never baselined (#1083).
    assert!(
        invalid.is_empty(),
        "fidelity gate: save introduced {} schema violations (docs/fidelity-gate.md)\n{}",
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

// ---------------------------------------------------------------------------
// The comparator itself, on tiny synthetic XML (no corpus needed).

fn diff(a: &str, b: &str) -> Vec<Finding> {
    compare_xml(
        "word/document.xml",
        &parse_xml(a.as_bytes()).expect("original parses"),
        &parse_xml(b.as_bytes()).expect("saved parses"),
    )
}

fn kinds(findings: &[Finding]) -> Vec<(Kind, String)> {
    findings.iter().map(|f| (f.kind, f.path.clone())).collect()
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

#[test]
fn prefix_rename_and_declaration_are_equal() {
    let a = format!(r#"<?xml version="1.0"?><w:document xmlns:w="{W}"><w:body/></w:document>"#);
    let b =
        format!(r#"<x:document xmlns:x="{W}" xmlns:unused="urn:u"><x:body></x:body></x:document>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn attribute_order_is_ignored() {
    let a = format!(r#"<w:p xmlns:w="{W}"><w:ind w:left="1" w:right="2"/></w:p>"#);
    let b = format!(r#"<w:p xmlns:w="{W}"><w:ind w:right="2" w:left="1"/></w:p>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn whitespace_only_text_between_elements_is_ignored() {
    let a = format!("<w:p xmlns:w=\"{W}\">\n  <w:r>\n    <w:t>x</w:t>\n  </w:r>\n</w:p>");
    let b = format!(r#"<w:p xmlns:w="{W}"><w:r><w:t>x</w:t></w:r></w:p>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn whitespace_only_text_content_is_kept() {
    let a = format!(r#"<w:r xmlns:w="{W}"><w:t xml:space="preserve"> </w:t></w:r>"#);
    let b = format!(r#"<w:r xmlns:w="{W}"><w:t xml:space="preserve"></w:t></w:r>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::LostElement, "/w:r/w:t/text()".into())]
    );
}

/// `w:t` / `w:delText` text compares as Word reads it: outside
/// `xml:space="preserve"` its edge whitespace is not text (#1084).
#[test]
fn unpreserved_run_text_compares_without_edge_whitespace() {
    let t =
        |attrs: &str, text: &str| format!(r#"<w:r xmlns:w="{W}"><w:t{attrs}>{text}</w:t></w:r>"#);
    let preserve = r#" xml:space="preserve""#;
    // The serializer's save of Word's "SDT": the same text, plus the attr.
    assert_eq!(
        kinds(&diff(&t("", "SDT "), &t(preserve, "SDT"))),
        vec![(Kind::ExtraAttr, "/w:r/w:t/@xml:space".into())]
    );
    // Keeping the space under preserve changes what Word shows.
    assert_eq!(
        kinds(&diff(&t("", "SDT "), &t(preserve, "SDT "))),
        vec![
            (Kind::ExtraAttr, "/w:r/w:t/@xml:space".into()),
            (Kind::ChangedValue, "/w:r/w:t/text()".into()),
        ]
    );
    // Only XML whitespace at the ends: interior spaces and NBSP are text.
    assert_eq!(
        kinds(&diff(&t("", " \t\nA  B\r\n"), &t("", "A  B"))),
        vec![]
    );
    assert_eq!(kinds(&diff(&t("", "A\u{a0}"), &t("", "A"))).len(), 1);
    // All whitespace is no text at all.
    assert_eq!(kinds(&diff(&t("", " \t "), &t(preserve, ""))).len(), 1);
    // An ancestor's preserve applies; a nearer `default` cancels it.
    let a = format!(r#"<w:p xmlns:w="{W}" xml:space="preserve"><w:r><w:t> x</w:t></w:r></w:p>"#);
    let b = format!(r#"<w:p xmlns:w="{W}" xml:space="preserve"><w:r><w:t>x</w:t></w:r></w:p>"#);
    assert_eq!(
        kinds(&diff(&a, &b)).len(),
        1,
        "inherited preserve keeps the space"
    );
    let a = format!(
        r#"<w:p xmlns:w="{W}" xml:space="preserve"><w:r xml:space="default"><w:t> x</w:t></w:r></w:p>"#
    );
    let b = format!(
        r#"<w:p xmlns:w="{W}" xml:space="preserve"><w:r xml:space="default"><w:t>x</w:t></w:r></w:p>"#
    );
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
    // By namespace, not prefix; delText too; other vocabularies' `t` are exact.
    let a = format!(r#"<x:delText xmlns:x="{W}"> gone </x:delText>"#);
    let b = format!(r#"<w:delText xmlns:w="{W}">gone</w:delText>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
    let a = r#"<t xmlns="urn:other"> x</t>"#;
    let b = r#"<t xmlns="urn:other">x</t>"#;
    assert_eq!(kinds(&diff(a, b)).len(), 1);
}

#[test]
fn entities_compare_decoded() {
    let a = format!(r#"<w:t xmlns:w="{W}" w:x="a&#38;b">1 &lt; 2 &amp; 3</w:t>"#);
    let b = format!(r#"<w:t xmlns:w="{W}" w:x="a&amp;b">1 &#60; 2 &#x26; 3</w:t>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn long_numeric_references_decode() {
    let a =
        format!(r#"<w:t xmlns:w="{W}" w:x="&#000000000065;">&#x00000000041;&#0000000066;</w:t>"#);
    let b = format!(r#"<w:t xmlns:w="{W}" w:x="A">AB</w:t>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn line_ends_normalize_to_lf() {
    let a = format!("<w:t xmlns:w=\"{W}\">a\r\nb\rc</w:t>");
    let b = format!("<w:t xmlns:w=\"{W}\">a\nb\nc</w:t>");
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn attribute_whitespace_normalizes_before_references_decode() {
    // Literal tab, CR and LF in a value are spaces...
    let a = format!("<w:x xmlns:w=\"{W}\" w:v=\"a\tb\r\nc\"/>");
    let b = format!(r#"<w:x xmlns:w="{W}" w:v="a b c"/>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
    // ...but a tab written as a reference stays a tab.
    let a = format!(r#"<w:x xmlns:w="{W}" w:v="a&#9;b"/>"#);
    let b = format!(r#"<w:x xmlns:w="{W}" w:v="a b"/>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::ChangedValue, "/w:x/@w:v".into())]
    );
}

#[test]
fn non_breaking_space_between_elements_is_text() {
    const NBSP: char = '\u{a0}';
    let a = format!("<w:p xmlns:w=\"{W}\"><w:r/>{NBSP}<w:r/></w:p>");
    let b = format!(r#"<w:p xmlns:w="{W}"><w:r/><w:r/></w:p>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::LostElement, "/w:p/text()".into())]
    );
}

#[test]
fn cdata_is_text() {
    let a = format!(r#"<w:t xmlns:w="{W}">a&lt;b</w:t>"#);
    let b = format!(r#"<w:t xmlns:w="{W}"><![CDATA[a<b]]></w:t>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
}

#[test]
fn dropped_child_is_one_lost_element() {
    let a = format!(
        r#"<w:pPr xmlns:w="{W}"><w:pStyle w:val="A"/><w:keepNext/><w:spacing w:after="0"/></w:pPr>"#
    );
    let b =
        format!(r#"<w:pPr xmlns:w="{W}"><w:pStyle w:val="A"/><w:spacing w:after="0"/></w:pPr>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::LostElement, "/w:pPr/w:keepNext".into())]
    );
}

#[test]
fn dropped_middle_sibling_of_the_same_name_does_not_cascade() {
    let a = format!(
        r#"<w:body xmlns:w="{W}"><w:p><w:r><w:t>1</w:t></w:r></w:p><w:p><w:r><w:t>2</w:t></w:r></w:p><w:p><w:r><w:t>3</w:t></w:r></w:p></w:body>"#
    );
    let b = format!(
        r#"<w:body xmlns:w="{W}"><w:p><w:r><w:t>1</w:t></w:r></w:p><w:p><w:r><w:t>3</w:t></w:r></w:p></w:body>"#
    );
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::LostElement, "/w:body/w:p[2]".into())]
    );
}

#[test]
fn property_lost_inside_a_paragraph_is_reported_at_its_path() {
    let a = format!(
        r#"<w:body xmlns:w="{W}"><w:p/><w:p><w:pPr><w:foo/><w:jc w:val="center"/></w:pPr></w:p></w:body>"#
    );
    let b = format!(
        r#"<w:body xmlns:w="{W}"><w:p/><w:p><w:pPr><w:jc w:val="center"/></w:pPr></w:p></w:body>"#
    );
    let found = diff(&a, &b);
    assert_eq!(
        kinds(&found),
        vec![(Kind::LostElement, "/w:body/w:p[2]/w:pPr/w:foo".into())]
    );
    assert_eq!(found[0].key_path(), "/w:body/w:p/w:pPr/w:foo");
}

#[test]
fn changed_lost_and_extra_attributes() {
    let a = format!(r#"<w:jc xmlns:w="{W}" w:val="center" w:a="1"/>"#);
    let b = format!(r#"<w:jc xmlns:w="{W}" w:val="left" w:b="2"/>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![
            (Kind::LostAttr, "/w:jc/@w:a".into()),
            (Kind::ChangedValue, "/w:jc/@w:val".into()),
            (Kind::ExtraAttr, "/w:jc/@w:b".into()),
        ]
    );
}

#[test]
fn text_change_fails() {
    let a = format!(r#"<w:t xmlns:w="{W}">hello</w:t>"#);
    let b = format!(r#"<w:t xmlns:w="{W}">hullo</w:t>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::ChangedValue, "/w:t/text()".into())]
    );
}

#[test]
fn extra_element_is_reported() {
    let a = format!(r#"<w:rPr xmlns:w="{W}"><w:b/></w:rPr>"#);
    let b = format!(r#"<w:rPr xmlns:w="{W}"><w:b/><w:i/></w:rPr>"#);
    assert_eq!(
        kinds(&diff(&a, &b)),
        vec![(Kind::ExtraElement, "/w:rPr/w:i".into())]
    );
}

#[test]
fn moved_child_is_lost_plus_extra() {
    let a = format!(r#"<w:rPr xmlns:w="{W}"><w:b/><w:i/></w:rPr>"#);
    let b = format!(r#"<w:rPr xmlns:w="{W}"><w:i/><w:b/></w:rPr>"#);
    let found = kinds(&diff(&a, &b));
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().any(|(k, _)| *k == Kind::LostElement));
    assert!(found.iter().any(|(k, _)| *k == Kind::ExtraElement));
}

#[test]
fn malformed_or_non_utf8_xml_is_not_parsed() {
    assert!(parse_xml(b"<a><b></a>").is_none());
    // Balanced, but the end tags do not match their start tags.
    assert!(parse_xml(b"<a><b></c></a>").is_none());
    assert!(parse_xml(b"<a><b/></a>").is_some());
    // Duplicate attributes: by name, by expanded name, and namespace declarations.
    assert!(parse_xml(br#"<a k="1" k="2"/>"#).is_none());
    assert!(parse_xml(br#"<a xmlns:p="urn:u" xmlns:q="urn:u" p:k="1" q:k="1"/>"#).is_none());
    assert!(parse_xml(br#"<a xmlns:p="urn:u" xmlns:p="urn:v"/>"#).is_none());
    // Content outside the root; whitespace there is fine.
    assert!(parse_xml(b"<a/>garbage").is_none());
    assert!(parse_xml(b"junk<a/>").is_none());
    assert!(parse_xml(b"<a/><![CDATA[x]]>").is_none());
    assert!(parse_xml(b"<?xml version=\"1.0\"?>\r\n<a/>\n").is_some());
    // A bare `&`, in text or in an attribute value.
    assert!(parse_xml(b"<a>A&B</a>").is_none());
    assert!(parse_xml(br#"<a k="A&B"/>"#).is_none());
    assert!(parse_xml(b"<a>A&bogus;B</a>").is_none());
    assert!(parse_xml(b"<a>&#65;&#x41;&amp;</a>").is_some());
    // A raw `<` in an attribute value.
    assert!(parse_xml(br#"<a k="a<b"/>"#).is_none());
    // References to characters XML does not allow, and overflowing ones (no panic).
    for bad in [
        "&#0;",
        "&#xD800;",
        "&#x110000;",
        "&#9999999999;",
        "&#x;",
        "&#;",
    ] {
        assert!(
            parse_xml(format!("<a>{bad}</a>").as_bytes()).is_none(),
            "{bad}"
        );
        assert!(
            parse_xml(format!(r#"<a k="{bad}"/>"#).as_bytes()).is_none(),
            "{bad}"
        );
    }
    assert!(parse_xml(b"\xff\xfe<\0a\0/\0>\0").is_none());
}

fn document_package(body: &str) -> Vec<u8> {
    let part = |n: &str, b: String| (n.to_string(), b.into_bytes());
    docxcore::zipwrite::write_zip(&[
        part(
            "[Content_Types].xml",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
             </Types>"
                .into(),
        ),
        part(
            "_rels/.rels",
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
             </Relationships>"
                .into(),
        ),
        part(
            "word/document.xml",
            format!("<w:document {W_NS}><w:body>{body}</w:body></w:document>"),
        ),
    ])
}

#[test]
fn effective_text_has_word_characters_for_run_content() {
    let xml = format!(
        "<w:document {W_NS} xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\">\
         <w:body><w:p><w:pPr><w:tabs><w:tab w:val=\"left\" w:pos=\"1\"/></w:tabs></w:pPr>\
         <w:r><w:t>e</w:t><w:noBreakHyphen/><w:t>c</w:t><w:softHyphen/><w:tab/><w:br/><w:cr/>\
         <w:br w:type=\"page\"/><w:br w:type=\"column\"/><w:sym w:char=\"F0E0\"/></w:r>\
         <w:r><w:instrText> PAGE </w:instrText></w:r>\
         <mc:AlternateContent><mc:Choice Requires=\"x\"><w:r><w:t>1</w:t></w:r></mc:Choice>\
         <mc:Fallback><w:r><w:t>1</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>\
         <w:p><w:r><w:delText>d</w:delText></w:r></w:p></w:body></w:document>"
    );
    let root = parse_xml(xml.as_bytes()).unwrap();
    assert_eq!(
        effective_text(&root),
        "e\u{1e}c\u{1f}\t\u{b}\u{b}\u{c}\u{e}(1\rd\r"
    );
}

#[test]
fn a_lost_hyphen_is_an_effective_text_change() {
    let original =
        document_package("<w:p><w:r><w:t>e</w:t><w:noBreakHyphen/><w:t>commerce</w:t></w:r></w:p>");
    // Split into runs, the text compares the same.
    let split = document_package(
        "<w:p><w:r><w:t>e</w:t></w:r><w:r><w:noBreakHyphen/></w:r><w:r><w:t>commerce</w:t></w:r></w:p>",
    );
    assert_eq!(effective_text_change(&original, &split), None);
    let lost = document_package("<w:p><w:r><w:t>e</w:t><w:t>commerce</w:t></w:r></w:p>");
    let why = effective_text_change(&original, &lost).expect("a change");
    assert!(why.contains("first difference at 1"), "{why}");
    // A literal U+2011 is not the element either.
    let literal = document_package("<w:p><w:r><w:t>e\u{2011}commerce</w:t></w:r></w:p>");
    assert!(effective_text_change(&original, &literal).is_some());
}

/// The gate's own round trip keeps every hyphen and the run that holds only
/// one (#1101).
#[test]
fn the_round_trip_keeps_the_effective_text_of_hyphens() {
    let original = document_package(
        "<w:p><w:r><w:t>ITU</w:t><w:noBreakHyphen/><w:t>T</w:t></w:r></w:p>\
         <w:p><w:r><w:t>insufficien</w:t><w:softHyphen/><w:t>tly</w:t></w:r></w:p>\
         <w:p><w:r><w:softHyphen/></w:r><w:bookmarkStart w:id=\"1\" w:name=\"A\"/>\
         <w:bookmarkEnd w:id=\"1\"/></w:p>",
    );
    let rt = round_trip(&original);
    assert_eq!(rt.text_change, None);
    let lost: Vec<_> = rt
        .findings
        .iter()
        .filter(|f| f.part == "word/document.xml" && f.path.contains("Hyphen"))
        .collect();
    assert!(lost.is_empty(), "{lost:?}");
}

/// The issue's runs (#1084): Word reads `<w:t>SDT </w:t><w:t> Run</w:t>`
/// as "SDTRun", and so must the save that rewrites them with `preserve`.
#[test]
fn the_round_trip_keeps_words_reading_of_unpreserved_whitespace() {
    let original = document_package(
        "<w:p><w:r><w:t>SDT </w:t></w:r><w:r><w:t> Run</w:t></w:r></w:p>\
         <w:p><w:r><w:t xml:space=\"preserve\">kept </w:t></w:r><w:r><w:t>x</w:t></w:r></w:p>",
    );
    let text = |bytes: &[u8]| {
        effective_text(&parse_xml(&read_parts(bytes).unwrap()["word/document.xml"]).unwrap())
    };
    assert_eq!(text(&original), "SDTRun\rkept x\r");
    let rt = round_trip(&original);
    assert_eq!(rt.text_change, None);
    let text_findings: Vec<_> = rt
        .findings
        .iter()
        .filter(|f| f.part == "word/document.xml" && f.path.ends_with("/text()"))
        .collect();
    assert!(text_findings.is_empty(), "{text_findings:?}");
}

/// The corpus files the Windows oracle found (#1084) read as Word reads them,
/// and their round trip keeps that text. Skips when the docxy-corpus checkout
/// is absent, like the gate itself.
#[test]
fn corpus_sdt_files_read_sdtrun_as_word_does_1084() {
    let (_, Some(dir)) = corpus(&workspace_root()) else {
        eprintln!("fidelity: SKIP corpus_sdt_files_read_sdtrun_as_word_does_1084: no corpus");
        return;
    };
    for name in [
        "SDT/SDT.docx",
        "SDT/Sdt/SDT Run.docx",
        "SDT/SdtRun/SDT Run.docx",
    ] {
        let path = dir.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("fidelity: SKIP {name}: not in the corpus");
            continue;
        };
        let doc = load_package(&bytes).expect(name).document;
        let text = doc.plain_text();
        assert!(text.contains("SDTRun"), "{name}: {text:?}");
        assert!(!text.contains("SDT  Run"), "{name}: {text:?}");
        assert_eq!(round_trip(&bytes).text_change, None, "{name}");
    }
}

#[test]
fn packages_compare_xml_canonically_and_other_parts_by_bytes() {
    use docxcore::zipwrite::write_zip;
    let part = |n: &str, b: &[u8]| (n.to_string(), b.to_vec());
    let original = write_zip(&[
        part("word/document.xml", br#"<a xmlns="urn:x" k="1"/>"#),
        part("word/media/image1.png", b"\x89PNG1"),
        part("word/gone.xml", b"<g/>"),
    ]);
    let saved = write_zip(&[
        part(
            "word/document.xml",
            br#"<?xml version="1.0"?><p:a xmlns:p="urn:x" k="1"></p:a>"#,
        ),
        part("word/media/image1.png", b"\x89PNG2"),
        part("word/new.xml", b"<n/>"),
    ]);
    let found: Vec<_> = compare_packages(&original, &saved)
        .into_iter()
        .map(|f| (f.part, f.kind))
        .collect();
    assert_eq!(
        found,
        vec![
            ("word/gone.xml".to_string(), Kind::PartMissing),
            ("word/media/image1.png".to_string(), Kind::PartBytes),
            ("word/new.xml".to_string(), Kind::PartExtra),
        ]
    );
}

#[test]
fn ill_formed_xml_falls_back_to_a_byte_compare() {
    use docxcore::zipwrite::write_zip;
    let pkg = |doc: &[u8]| write_zip(&[("word/document.xml".to_string(), doc.to_vec())]);
    for (original, saved) in [
        (&br#"<a k="1"/>"#[..], &br#"<a k="1" k="2"/>"#[..]),
        (b"<a/>", b"<a/>garbage"),
        (b"<a>A&amp;B</a>", b"<a>A&B</a>"),
        (br#"<a k="a&lt;b"/>"#, br#"<a k="a<b"/>"#),
    ] {
        let found = compare_packages(&pkg(original), &pkg(saved));
        assert_eq!(
            found.iter().map(|f| f.kind).collect::<Vec<_>>(),
            vec![Kind::PartBytes],
            "{saved:?}"
        );
    }
}

#[test]
fn parts_identical_compares_bytes_not_canonical_xml() {
    use docxcore::zipwrite::write_zip;
    let pkg = |doc: &[u8], extra: Option<&str>| {
        let mut parts = vec![("word/document.xml".to_string(), doc.to_vec())];
        parts.extend(extra.map(|n| (n.to_string(), b"<x/>".to_vec())));
        write_zip(&parts)
    };
    let original = pkg(br#"<a xmlns="urn:x" k="1" j="2"/>"#, None);
    assert_eq!(parts_identical(&original, &original), Ok(()));
    // Canonically equal (attribute order), but not the same bytes.
    let reordered = pkg(br#"<a xmlns="urn:x" j="2" k="1"/>"#, None);
    assert!(compare_packages(&original, &reordered).is_empty());
    assert_eq!(
        parts_identical(&original, &reordered),
        Err("word/document.xml: bytes differ".into())
    );
    assert_eq!(
        parts_identical(
            &original,
            &pkg(br#"<a xmlns="urn:x" k="1" j="2"/>"#, Some("b.xml"))
        ),
        Err("b.xml: extra".into())
    );
}

/// The [Content_Types].xml difference between an original with `before`
/// overrides and a saved package with `after` overrides.
fn content_type_findings(before: &str, after: &str) -> Vec<Finding> {
    const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
    let types = |overrides: &str| {
        format!(
            r#"<Types xmlns="{CT}"><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
        )
    };
    compare_xml(
        "[Content_Types].xml",
        &parse_xml(types(before).as_bytes()).unwrap(),
        &parse_xml(types(after).as_bytes()).unwrap(),
    )
}

const STYLES_OVERRIDE: &str = r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#;

fn real_allowlist() -> Vec<AllowRule> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fidelity");
    parse_allowlist(&std::fs::read_to_string(here.join("allowlist.txt")).unwrap()).unwrap()
}

/// What the real allowlist leaves of `found`, for one file.
fn not_allowed(found: Vec<Finding>) -> Vec<(Kind, String)> {
    let allow = real_allowlist();
    let mut counts = vec![0; allow.len()];
    apply_allowlist(found, &allow, &mut counts)
        .into_iter()
        .map(|f| (f.kind, f.part))
        .collect()
}

fn styles_part_added() -> Finding {
    Finding {
        part: "word/styles.xml".into(),
        kind: Kind::PartExtra,
        path: String::new(),
        detail: String::new(),
    }
}

#[test]
fn allowlist_tolerates_only_the_styles_content_type_override() {
    // The styles part was added, with its override: both tolerated.
    let mut found = content_type_findings("", STYLES_OVERRIDE);
    found.push(styles_part_added());
    assert_eq!(not_allowed(found), vec![]);

    // An override for another part, or with another content type: not.
    for other in [
        r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
        r#"<Override PartName="/word/styles.xml" ContentType="application/xml"/>"#,
    ] {
        let mut found = content_type_findings("", other);
        found.push(styles_part_added());
        assert_eq!(
            not_allowed(found),
            vec![(Kind::ExtraElement, "[Content_Types].xml".into())]
        );
    }
}

#[test]
fn styles_override_rule_needs_the_added_part_and_applies_once() {
    // The styles part already existed; save repeats its override: not tolerated.
    let found = content_type_findings(STYLES_OVERRIDE, &STYLES_OVERRIDE.repeat(2));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        not_allowed(found),
        vec![(Kind::ExtraElement, "[Content_Types].xml".into())]
    );

    // The part was added but the override appears twice: one is tolerated.
    let mut found = content_type_findings("", &STYLES_OVERRIDE.repeat(2));
    found.push(styles_part_added());
    assert_eq!(
        not_allowed(found),
        vec![(Kind::ExtraElement, "[Content_Types].xml".into())]
    );
}

#[test]
fn once_with_needs_a_known_kind_a_part_and_a_reason() {
    assert!(parse_allowlist("part-extra | * | * | * | once-with bogus x | r").is_err());
    assert!(parse_allowlist("part-extra | * | * | * | once-with part-extra | r").is_err());
    assert!(parse_allowlist("part-extra | * | * | * | once-with part-extra x |").is_err());
    let rules = parse_allowlist("part-extra | * | * | * | once-with part-bytes a.xml | r").unwrap();
    assert_eq!(rules[0].once_with, Some((Kind::PartBytes, "a.xml".into())));
    assert_eq!(rules[0].reason, "r");
}

#[test]
#[should_panic(expected = "fidelity:")]
fn unreadable_corpus_directory_is_an_error() {
    docx_files(
        &workspace_root().join("corpus/no-such-dir"),
        false,
        &mut Vec::new(),
    );
}

#[test]
fn panic_becomes_a_finding() {
    let found = guarded(|| panic!("boom"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, Kind::Panic);
    assert_eq!(found[0].detail, "boom");
}

#[test]
fn allowlist_needs_a_reason_and_matches_by_glob() {
    assert!(parse_allowlist("extra-attr | word/document.xml | */w:t/@xml:space | * |").is_err());
    assert!(parse_allowlist("extra-attr | word/document.xml | */w:t/@xml:space | *").is_err());
    assert!(parse_allowlist("bogus | * | * | * | r").is_err());
    let rules = parse_allowlist(
        "# c\nextra-attr | word/*.xml | */w:t/@xml:space | * | serializer adds it\n",
    )
    .unwrap();
    let f = |kind, path: &str| Finding {
        part: "word/document.xml".into(),
        kind,
        path: path.into(),
        detail: String::new(),
    };
    assert!(rules[0].matches(&f(
        Kind::ExtraAttr,
        "/w:document/w:body/w:p[2]/w:r/w:t/@xml:space"
    )));
    assert!(!rules[0].matches(&f(
        Kind::LostAttr,
        "/w:document/w:body/w:p/w:r/w:t/@xml:space"
    )));
    assert!(!rules[0].matches(&f(Kind::ExtraAttr, "/w:document/w:body/w:p/w:r/@xml:space")));
}

fn finding(kind: Kind, path: &str) -> Finding {
    Finding {
        part: "word/document.xml".into(),
        kind,
        path: path.into(),
        detail: String::new(),
    }
}

fn entry(file: &str, kind: Kind, path: &str) -> Entry {
    Entry::of(file, &finding(kind, path))
}

#[test]
fn baseline_judges_new_and_stale_losses_of_present_files_only() {
    let baseline: BTreeSet<Entry> = [
        entry("ext:a.docx", Kind::LostElement, "/w:p/w:pPr/w:foo"),
        entry("ext:a.docx", Kind::LostAttr, "/w:p/@w:fixed"),
        entry("ext:absent.docx", Kind::LostElement, "/w:p/w:x"),
    ]
    .into();
    let findings = vec![
        // In the baseline at another index: same entry.
        (
            "ext:a.docx".to_string(),
            finding(Kind::LostElement, "/w:p[7]/w:pPr/w:foo"),
        ),
        (
            "ext:a.docx".to_string(),
            finding(Kind::LostElement, "/w:p[2]/w:pPr/w:bar"),
        ),
    ];
    let present: BTreeSet<String> = ["ext:a.docx".to_string()].into();
    let v = judge(&findings, &present, &baseline);
    assert_eq!(
        v.new.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>(),
        vec![entry("ext:a.docx", Kind::LostElement, "/w:p/w:pPr/w:bar")]
    );
    // The absent file's entry is not stale: this run never saw that file.
    assert_eq!(
        v.stale,
        vec![entry("ext:a.docx", Kind::LostAttr, "/w:p/@w:fixed")]
    );
    assert!(!v.passed());
}

#[test]
fn baseline_update_keeps_entries_of_absent_files() {
    let baseline: BTreeSet<Entry> = [
        entry("ext:a.docx", Kind::LostAttr, "/w:p/@w:fixed"),
        entry("ext:absent.docx", Kind::LostElement, "/w:p/w:x"),
    ]
    .into();
    let findings = vec![(
        "ext:a.docx".to_string(),
        finding(Kind::LostElement, "/w:p[2]/w:y"),
    )];
    let present: BTreeSet<String> = ["ext:a.docx".to_string()].into();
    let next = updated_baseline(&findings, &present, &baseline);
    assert_eq!(
        next,
        [
            entry("ext:a.docx", Kind::LostElement, "/w:p/w:y"),
            entry("ext:absent.docx", Kind::LostElement, "/w:p/w:x"),
        ]
        .into()
    );
}

#[test]
fn baseline_round_trips_through_render_and_parse_with_classes() {
    let entries: BTreeSet<Entry> = [
        entry("ext:a.docx", Kind::LostElement, "/w:p/w:pPr/w:foo"),
        entry("ext:b.docx", Kind::Panic, ""),
    ]
    .into();
    let classes = [LossClass {
        name: "property loss",
        issue: "#1063",
        matches: |e| e.path.contains("/w:pPr/"),
    }];
    let (text, unclassified) = render_baseline(&entries, &classes);
    assert!(text.contains("# property loss (#1063)\n"));
    assert_eq!(unclassified, vec![entry("ext:b.docx", Kind::Panic, "")]);
    assert_eq!(parse_baseline(&text).unwrap(), entries);
}

#[test]
fn glob_matches_runs() {
    assert!(glob("*", ""));
    assert!(glob(
        "*/w:t/@xml:space",
        "/w:document/w:p/w:r/w:t/@xml:space"
    ));
    assert!(glob("word/*.xml", "word/header1.xml"));
    assert!(!glob("word/*.xml", "word/media/a.png"));
    assert!(glob("a*b*c", "a-b-b-c"));
}

/// `MISALIGNED_RUNS` shrinks with the baseline: each of its entries is a
/// baseline line, claimed by the #1069 class.
#[test]
fn misaligned_runs_are_baseline_entries_of_1069() {
    let baseline = parse_baseline(include_str!("fidelity/baseline.txt")).expect("baseline.txt");
    for (file, entries) in MISALIGNED_RUNS {
        for (kind, path) in *entries {
            let e = Entry {
                file: file.to_string(),
                part: "word/document.xml".to_string(),
                kind: *kind,
                path: format!("/w:document/w:body{path}"),
            };
            assert!(baseline.contains(&e), "not in baseline.txt: {e:?}");
            let class = CLASSES.iter().find(|c| (c.matches)(&e)).expect("classed");
            assert_eq!(class.issue, "#1069", "{e:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// Schema validation (#1083), on tiny synthetic XML.

const W_NS: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

/// The violations of a `w:body`'s content: (rule, parent path, child).
fn schema(body: &str) -> Vec<(&'static str, String, String)> {
    let xml = format!("<w:document {W_NS}><w:body>{body}</w:body></w:document>");
    schema_of(&xml)
}

fn schema_of(xml: &str) -> Vec<(&'static str, String, String)> {
    let root = parse_xml(xml.as_bytes()).expect("well-formed");
    validate("word/document.xml", &root)
        .into_iter()
        .map(|v| (v.rule.as_str(), v.parent, v.child))
        .collect()
}

fn violation(rule: &'static str, parent: &str, child: &str) -> (&'static str, String, String) {
    (rule, parent.to_string(), child.to_string())
}

#[test]
fn schema_accepts_a_valid_document() {
    let body = "<w:p><w:pPr><w:pStyle w:val=\"T\"/><w:spacing w:after=\"0\"/><w:jc w:val=\"center\"/>\
                <w:rPr><w:ins w:id=\"1\" w:author=\"a\"/><w:b/></w:rPr></w:pPr>\
                <w:bookmarkStart w:id=\"0\" w:name=\"b\"/><w:r><w:rPr><w:b/><w:sz w:val=\"20\"/></w:rPr>\
                <w:t>a</w:t><w:tab/><w:t>b</w:t></w:r><w:bookmarkEnd w:id=\"0\"/><w:r><w:t>c</w:t></w:r>\
                <w:hyperlink w:anchor=\"x\"><w:r><w:t>d</w:t></w:r></w:hyperlink>\
                <w:smartTag w:element=\"place\"><w:smartTagPr><w:attr w:name=\"n\" w:val=\"v\"/></w:smartTagPr>\
                <w:r><w:t>e</w:t></w:r></w:smartTag></w:p>\
                <w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders><w:top w:val=\"single\"/>\
                <w:start w:val=\"single\"/><w:insideV w:val=\"single\"/></w:tblBorders></w:tblPr>\
                <w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:trPr><w:tblHeader/><w:cantSplit/></w:trPr>\
                <w:tc><w:tcPr><w:tcW w:w=\"1\"/><w:vAlign w:val=\"top\"/></w:tcPr><w:bookmarkStart w:id=\"1\" w:name=\"c\"/>\
                <w:p/></w:tc></w:tr></w:tbl>\
                <w:sectPr><w:headerReference r:id=\"h\" xmlns:r=\"urn:r\"/><w:pgSz w:w=\"1\"/><w:pgMar w:top=\"1\"/>\
                <w:cols/><w:docGrid/></w:sectPr>";
    assert_eq!(schema(body), []);
}

#[test]
fn schema_reports_children_out_of_order() {
    // The mutation proof's ordering case: `w:jc` written before `w:spacing`.
    assert_eq!(
        schema("<w:p><w:pPr><w:jc w:val=\"left\"/><w:spacing w:after=\"0\"/></w:pPr></w:p>"),
        [violation(
            "order",
            "/w:document/w:body/w:p/w:pPr",
            "w:spacing"
        )]
    );
    assert_eq!(
        schema("<w:p><w:r><w:t>a</w:t></w:r><w:pPr/></w:p>"),
        [violation("order", "/w:document/w:body/w:p", "w:pPr")]
    );
}

#[test]
fn schema_reports_a_duplicate_singleton() {
    assert_eq!(
        schema("<w:p><w:pPr><w:jc w:val=\"left\"/><w:jc w:val=\"right\"/></w:pPr></w:p>"),
        [violation(
            "duplicate",
            "/w:document/w:body/w:p/w:pPr",
            "w:jc"
        )]
    );
}

#[test]
fn schema_run_properties_are_a_repeatable_choice() {
    // Transitional EG_RPrBase is an unbounded choice: any order, repeats.
    assert_eq!(
        schema(
            "<w:p><w:pPr><w:rPr><w:ins w:id=\"1\" w:author=\"a\"/><w:sz w:val=\"2\"/><w:b/>\
             <w:b/></w:rPr></w:pPr><w:r><w:rPr><w:b/><w:i/><w:b/><w:kern w:val=\"2\"/>\
             <w:kern w:val=\"2\"/><w:rStyle w:val=\"s\"/><w:rPrChange w:id=\"2\" w:author=\"a\">\
             <w:rPr><w:i/><w:b/></w:rPr></w:rPrChange></w:rPr><w:t>a</w:t></w:r></w:p>"
        ),
        []
    );
    // Only the paragraph mark's revision marks and rPrChange are placed.
    assert_eq!(
        schema(
            "<w:p><w:pPr><w:rPr><w:b/><w:ins w:id=\"1\" w:author=\"a\"/></w:rPr></w:pPr>\
             <w:r><w:rPr><w:rPrChange w:id=\"2\" w:author=\"a\"/><w:b/></w:rPr></w:r></w:p>"
        ),
        [
            violation("order", "/w:document/w:body/w:p/w:pPr/w:rPr", "w:ins"),
            violation("order", "/w:document/w:body/w:p/w:r/w:rPr", "w:b"),
        ]
    );
}

#[test]
fn schema_border_sides_are_physical_and_logical() {
    let borders = |sides: &str| {
        schema(&format!(
            "<w:tbl><w:tblPr><w:tblBorders>{sides}</w:tblBorders></w:tblPr><w:tblGrid/></w:tbl>"
        ))
    };
    assert_eq!(
        borders("<w:top/><w:start/><w:left/><w:bottom/><w:end/><w:right/><w:insideH/>"),
        []
    );
    assert_eq!(
        borders("<w:left/><w:start/>"),
        [violation(
            "order",
            "/w:document/w:body/w:tbl/w:tblPr/w:tblBorders",
            "w:start"
        )]
    );
}

#[test]
fn schema_row_revisions_follow_the_row_properties() {
    let row = |trpr: &str| {
        schema(&format!(
            "<w:tbl><w:tblPr/><w:tblGrid/><w:tr><w:trPr>{trpr}</w:trPr><w:tc><w:p/></w:tc></w:tr></w:tbl>"
        ))
    };
    assert_eq!(
        row("<w:tblHeader/><w:cantSplit/><w:tblHeader/><w:ins w:id=\"1\" w:author=\"a\"/>"),
        []
    );
    assert_eq!(
        row("<w:ins w:id=\"1\" w:author=\"a\"/><w:cantSplit/>"),
        [violation(
            "order",
            "/w:document/w:body/w:tbl/w:tr/w:trPr",
            "w:cantSplit"
        )]
    );
}

#[test]
fn schema_only_range_markup_precedes_the_table_properties() {
    assert_eq!(
        schema("<w:tbl><w:bookmarkStart w:id=\"0\" w:name=\"t\"/><w:tblPr/><w:tblGrid/></w:tbl>"),
        []
    );
    assert_eq!(
        schema("<w:tbl><w:proofErr w:type=\"spellStart\"/><w:tblPr/><w:tblGrid/></w:tbl>"),
        [
            violation("order", "/w:document/w:body/w:tbl", "w:tblPr"),
            violation("order", "/w:document/w:body/w:tbl", "w:tblGrid"),
        ]
    );
}

#[test]
fn schema_revision_snapshots_have_their_own_models() {
    // A paragraph mark's snapshot (CT_ParaRPrOriginal) has the revision marks.
    assert_eq!(
        schema(
            "<w:p><w:pPr><w:rPr><w:rPrChange w:id=\"1\" w:author=\"a\"><w:rPr>\
             <w:ins w:id=\"2\" w:author=\"a\"/><w:b/></w:rPr></w:rPrChange></w:rPr></w:pPr></w:p>"
        ),
        []
    );
    // A run's (CT_RPrOriginal) does not, nor a nested rPrChange.
    assert_eq!(
        schema(
            "<w:p><w:r><w:rPr><w:rPrChange w:id=\"1\" w:author=\"a\"><w:rPr>\
             <w:ins w:id=\"2\" w:author=\"a\"/><w:rPrChange/></w:rPr></w:rPrChange></w:rPr></w:r></w:p>"
        ),
        [
            violation(
                "not-allowed",
                "/w:document/w:body/w:p/w:r/w:rPr/w:rPrChange/w:rPr",
                "w:ins"
            ),
            violation(
                "not-allowed",
                "/w:document/w:body/w:p/w:r/w:rPr/w:rPrChange/w:rPr",
                "w:rPrChange"
            ),
        ]
    );
    // A paragraph's snapshot (CT_PPrBase) ends at cnfStyle.
    assert_eq!(
        schema(
            "<w:p><w:pPr><w:jc w:val=\"left\"/><w:pPrChange w:id=\"1\" w:author=\"a\"><w:pPr>\
             <w:jc w:val=\"right\"/><w:sectPr/></w:pPr></w:pPrChange></w:pPr></w:p>"
        ),
        [violation(
            "not-allowed",
            "/w:document/w:body/w:p/w:pPr/w:pPrChange/w:pPr",
            "w:sectPr"
        )]
    );
}

#[test]
fn schema_reports_smart_tag_properties_under_a_paragraph_1083() {
    // What save wrote for an unwrapped smart tag before #1083.
    assert_eq!(
        schema(
            "<w:p><w:smartTagPr><w:attr w:name=\"n\" w:val=\"v\"/></w:smartTagPr>\
             <w:r><w:t>2003</w:t></w:r></w:p>"
        ),
        [violation(
            "not-allowed",
            "/w:document/w:body/w:p",
            "w:smartTagPr"
        )]
    );
}

#[test]
fn schema_wants_the_section_last_in_the_body() {
    assert_eq!(
        schema("<w:p/><w:sectPr/><w:p/>"),
        [violation("order", "/w:document/w:body", "w:p")]
    );
}

#[test]
fn schema_reports_missing_required_children() {
    assert_eq!(
        schema("<w:tbl><w:tblPr/><w:tr><w:tc><w:tcPr/></w:tc></w:tr></w:tbl>"),
        [
            violation("missing", "/w:document/w:body/w:tbl", "w:tblGrid"),
            violation("missing", "/w:document/w:body/w:tbl/w:tr/w:tc", "w:p"),
        ]
    );
}

#[test]
fn schema_paragraph_mark_and_run_properties_differ() {
    // Revision marks lead a paragraph mark's rPr (CT_ParaRPr) but are not run
    // properties.
    assert_eq!(
        schema(
            "<w:p><w:pPr><w:rPr><w:del w:id=\"1\" w:author=\"a\"/><w:b/></w:rPr></w:pPr>\
             <w:r><w:rPr><w:del w:id=\"2\" w:author=\"a\"/></w:rPr></w:r></w:p>"
        ),
        [violation(
            "not-allowed",
            "/w:document/w:body/w:p/w:r/w:rPr",
            "w:del"
        )]
    );
}

#[test]
fn schema_ignores_foreign_namespaces_and_alternate_content() {
    let body = "<w:p xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" \
                xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\">\
                <w:pPr><w14:foo/><w:jc w:val=\"left\"/></w:pPr>\
                <mc:AlternateContent><mc:Choice Requires=\"w14\"><w:p><w:pPr><w:jc/><w:spacing/></w:pPr></w:p>\
                </mc:Choice></mc:AlternateContent><w:r><w:t>x</w:t></w:r></w:p>";
    assert_eq!(schema(body), []);
    // A `w:` container inside foreign content (a text box) is still checked.
    let body = "<w:p><w:r><w:drawing><wp:inline xmlns:wp=\"urn:wp\"><w:txbxContent><w:p>\
                <w:smartTagPr/></w:p></w:txbxContent></wp:inline></w:drawing></w:r></w:p>";
    assert_eq!(
        schema(body),
        [violation(
            "not-allowed",
            "/w:document/w:body/w:p/w:r/w:drawing/wp:inline/w:txbxContent/w:p",
            "w:smartTagPr"
        )]
    );
}

#[test]
fn schema_keys_on_the_namespace_not_the_prefix() {
    let xml = "<x:document xmlns:x=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <x:body><x:p><x:smartTagPr/></x:p></x:body></x:document>";
    assert_eq!(
        schema_of(xml),
        [violation(
            "not-allowed",
            "/w:document/w:body/w:p",
            "w:smartTagPr"
        )]
    );
}

#[test]
fn schema_counts_only_violations_the_save_added() {
    let doc = |body: &str| {
        let xml = format!("<w:document {W_NS}><w:body>{body}</w:body></w:document>");
        validate("word/document.xml", &parse_xml(xml.as_bytes()).unwrap())
    };
    let bad_order = "<w:p><w:pPr><w:jc/><w:spacing/></w:pPr></w:p>";
    // One already in the original is not new, wherever the save put it.
    let original = doc(&format!("<w:p/>{bad_order}"));
    let saved = doc(&format!("{bad_order}<w:p/>"));
    assert_eq!(new_violations(&original, &saved), []);
    // A second one at the same place is.
    let saved = doc(&format!("{bad_order}{bad_order}"));
    let new = new_violations(&original, &saved);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].rule.as_str(), "order");
    // So is another child broken under the same parent by the same rule: a
    // fixed violation does not pay for a new one.
    let original = doc("<w:p><w:pPr><w:jc/><w:spacing/></w:pPr></w:p>");
    let saved = doc("<w:p><w:pPr><w:jc/><w:ind/></w:pPr></w:p>");
    let new = new_violations(&original, &saved);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0].child, "w:ind");
    assert_eq!(
        new[0].line("ext:a.docx"),
        "SCHEMA ext:a.docx | word/document.xml | order | /w:document/w:body/w:p/w:pPr/w:ind"
    );
}

#[test]
fn schema_places_math_without_checking_it() {
    let m = "xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\"";
    // A cell holding only math has its content; math interleaves with runs.
    assert_eq!(
        schema(&format!(
            "<w:tbl {m}><w:tblPr/><w:tblGrid/><w:tr><w:tc><m:oMathPara><m:oMath><m:r><w:jc/>\
             <w:jc/></m:r></m:oMath></m:oMathPara></w:tc></w:tr></w:tbl>\
             <w:p {m}><w:r><w:t>x</w:t></w:r><m:oMath/><w:r><w:t>y</w:t></w:r></w:p>"
        )),
        []
    );
    // But not before the paragraph's properties.
    assert_eq!(
        schema(&format!("<w:p {m}><m:oMath/><w:pPr/></w:p>")),
        [violation("order", "/w:document/w:body/w:p", "w:pPr")]
    );
}

#[test]
fn schema_style_properties_have_the_general_models() {
    let xml = format!(
        "<w:styles {W_NS}><w:style w:styleId=\"s\"><w:pPr><w:jc w:val=\"left\"/><w:rPr/>\
         <w:sectPr/></w:pPr><w:tblPr><w:tblW w:w=\"0\"/><w:tblPrChange/></w:tblPr>\
         <w:tblStylePr w:type=\"firstRow\"><w:pPr><w:pPrChange/></w:pPr><w:tblPr>\
         <w:tblPrChange/></w:tblPr></w:tblStylePr></w:style></w:styles>"
    );
    assert_eq!(
        schema_of(&xml),
        [
            violation("not-allowed", "/w:styles/w:style/w:pPr", "w:rPr"),
            violation("not-allowed", "/w:styles/w:style/w:pPr", "w:sectPr"),
            violation("not-allowed", "/w:styles/w:style/w:tblPr", "w:tblPrChange"),
            violation(
                "not-allowed",
                "/w:styles/w:style/w:tblStylePr/w:tblPr",
                "w:tblPrChange"
            ),
        ]
    );
}

#[test]
fn schema_validates_utf16_parts() {
    use docxcore::zipwrite::write_zip;
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><w:hdr {W_NS}><w:p><w:smartTagPr/></w:p></w:hdr>"
    );
    let le: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(xml.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let be: Vec<u8> = xml.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let pkg = write_zip(&[
        ("word/header1.xml".to_string(), le),
        ("word/header2.xml".to_string(), be),
    ]);
    let found: Vec<_> = validate_package(&pkg)
        .into_iter()
        .map(|v| (v.part, v.rule.as_str(), v.child))
        .collect();
    assert_eq!(
        found,
        [
            (
                "word/header1.xml".to_string(),
                "not-allowed",
                "w:smartTagPr".to_string()
            ),
            (
                "word/header2.xml".to_string(),
                "not-allowed",
                "w:smartTagPr".to_string()
            ),
        ]
    );
}

#[test]
fn schema_is_clean_on_the_saved_smart_tag_fixture_1083() {
    let path = workspace_root().join("docxcore/tests/fixtures/smarttag-pr.docx");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(validate_package(&bytes), []);
    let rt = round_trip(&bytes);
    assert_eq!(rt.schema, []);
    assert!(rt.preserving.is_ok());
}
