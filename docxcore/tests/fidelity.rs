//! Round-trip fidelity gate (#1060): open every corpus `.docx`, save it with
//! no edits, and compare every package part with the original. Fails on any
//! loss not covered by `fidelity/allowlist.txt` or `fidelity/baseline.txt`,
//! and on a baseline entry that no longer reproduces. See
//! `docs/fidelity-gate.md`.
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
        name: "unmodeled property children and attributes dropped on save",
        issue: "#1063",
        matches: |e| {
            matches!(e.kind, Kind::LostElement | Kind::LostAttr)
                && (in_properties(&e.path) || is_modeled_element_attr(e))
        },
    },
    LossClass {
        name: "modeled property values rewritten on save (values outside the supported set, \
               properties added)",
        issue: "#1068",
        matches: |e| e.part == "word/document.xml" && in_properties(&e.path),
    },
    LossClass {
        name: "run and paragraph content restructured on save (runs merged or split, \
               lastRenderedPageBreak, smartTag, row-level sdt, special characters)",
        issue: "#1069",
        matches: |e| e.part == "word/document.xml" && e.path.starts_with("/w:document/w:body/"),
    },
];

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

/// An attribute directly on a modeled element (`w:p/@w:rsidR`, `w:tr/@w14:paraId`).
fn is_modeled_element_attr(e: &Entry) -> bool {
    let Some((owner, attr)) = e.path.rsplit_once("/@") else {
        return false;
    };
    !attr.contains('/')
        && owner
            .rsplit('/')
            .next()
            .is_some_and(|el| matches!(el, "w:p" | "w:r" | "w:tbl" | "w:tr" | "w:tc"))
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

/// The round trip under test: what docxy's save does for a modified
/// document's untouched content. Users' no-edit saves take the preserving
/// path instead; `preserving` reports whether that one is byte-identical.
fn round_trip(bytes: &[u8]) -> (Vec<Finding>, Result<(), String>) {
    let mut pkg = match load_package(bytes) {
        Ok(pkg) => pkg,
        Err(e) => {
            let f = Finding {
                part: String::new(),
                kind: Kind::LoadError,
                path: String::new(),
                detail: format!("{e:?}"),
            };
            return (vec![f], Ok(()));
        }
    };
    let preserving = parts_identical(bytes, &save_package_preserving_document(&pkg));
    pkg.document = Editor::new(pkg.document.clone()).doc;
    (compare_packages(bytes, &save_package(&pkg)), preserving)
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
    let mut allowed = vec![0usize; allow.len()];
    for (file, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        present.insert(file.clone());
        let mut preserved = Ok(());
        let found = guarded(|| {
            let (found, ok) = round_trip(&bytes);
            preserved = ok;
            found
        });
        if let Err(why) = preserved {
            not_preserved.push(format!("{file}: {why}"));
        }
        for f in found {
            if let Some(rule) = allow.iter().position(|r| r.matches(&f)) {
                allowed[rule] += 1;
            } else {
                findings.push((file.clone(), f));
            }
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

#[test]
fn entities_compare_decoded() {
    let a = format!(r#"<w:t xmlns:w="{W}" w:x="a&#38;b">1 &lt; 2 &amp; 3</w:t>"#);
    let b = format!(r#"<w:t xmlns:w="{W}" w:x="a&amp;b">1 &#60; 2 &#x26; 3</w:t>"#);
    assert_eq!(kinds(&diff(&a, &b)), vec![]);
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
    assert!(parse_xml(b"\xff\xfe<\0a\0/\0>\0").is_none());
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

#[test]
fn allowlist_tolerates_only_the_styles_content_type_override() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fidelity");
    let allow =
        parse_allowlist(&std::fs::read_to_string(here.join("allowlist.txt")).unwrap()).unwrap();
    const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
    let types = |overrides: &str| {
        format!(
            r#"<Types xmlns="{CT}"><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
        )
    };
    let original = types("");
    let tolerated = |added: &str| {
        let found = compare_xml(
            "[Content_Types].xml",
            &parse_xml(original.as_bytes()).unwrap(),
            &parse_xml(types(added).as_bytes()).unwrap(),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, Kind::ExtraElement);
        allow.iter().any(|r| r.matches(&found[0]))
    };
    assert!(tolerated(
        r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#
    ));
    assert!(!tolerated(
        r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#
    ));
    assert!(!tolerated(
        r#"<Override PartName="/word/styles.xml" ContentType="application/xml"/>"#
    ));
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
