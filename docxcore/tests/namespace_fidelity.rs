//! Namespace fidelity of captured subtrees on save: a default namespace and
//! markup-compatibility rules declared below the part root (#1089).

use docxcore::editor::{Caret, Editor};
use docxcore::package::{load_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;
use opccore::xml::{Event, XmlParser};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// `document` as `word/document.xml` of a minimal package, loaded and saved
/// again (a part root is written back as loaded), then its saved part.
fn saved(document: &str) -> String {
    saved_after(document, |_| {})
}

/// [`saved`] with `edit` applied to an editor on the document first.
fn saved_after(document: &str, edit: impl FnOnce(&mut Editor)) -> String {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        (
            "word/document.xml".to_string(),
            document.as_bytes().to_vec(),
        ),
    ]);
    let mut pkg = load_package(&bytes).expect("fixture loads");
    let mut editor = Editor::new(pkg.document.clone());
    edit(&mut editor);
    pkg.document = editor.doc;
    let out = save_package(&pkg);
    let archive = ZipArchive::open(&out).expect("saved package is a ZIP");
    String::from_utf8(archive.read("word/document.xml").expect("document part"))
        .expect("document part is UTF-8")
}

fn doc(body_attrs: &str, body: &str) -> String {
    format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:mc=\"{MC_NS}\">\
         <w:body{body_attrs}>{body}</w:body></w:document>"
    )
}

/// The namespace each element with local name `local` resolves to, in
/// document order; unprefixed names take the in-scope default.
fn namespaces_of(xml: &str, local: &str) -> Vec<String> {
    let mut scopes: Vec<Vec<(String, String)>> = Vec::new();
    let mut found = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                scopes.push(
                    p.attrs()
                        .iter()
                        .filter_map(|a| {
                            let prefix = if a.name == "xmlns" {
                                Some("")
                            } else {
                                a.name.strip_prefix("xmlns:")
                            }?;
                            Some((prefix.to_string(), a.value.to_string()))
                        })
                        .collect(),
                );
                let (prefix, name) = p.name().split_once(':').unwrap_or(("", p.name()));
                if name == local {
                    let ns = scopes
                        .iter()
                        .rev()
                        .flatten()
                        .find(|(pf, _)| pf == prefix)
                        .map_or("", |(_, ns)| ns.as_str());
                    found.push(ns.to_string());
                }
            }
            Event::End => {
                scopes.pop();
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    found
}

/// Load, save and reload: the element still resolves as in the source.
fn assert_ns_survives(src: &str, local: &str, expected: &str) {
    assert_eq!(namespaces_of(src, local), [expected], "source");
    let once = saved(src);
    assert_eq!(namespaces_of(&once, local), [expected], "saved: {once}");
    let twice = saved(&once);
    assert_eq!(namespaces_of(&twice, local), [expected], "resaved: {twice}");
}

#[test]
fn default_namespace_below_root_survives_rpr_child() {
    let src = doc(
        " xmlns=\"urn:vendor\"",
        "<w:p><w:pPr><w:rPr><effect/></w:rPr></w:pPr><w:r><w:t>a</w:t></w:r></w:p>",
    );
    assert_ns_survives(&src, "effect", "urn:vendor");
    let src = doc(
        "",
        "<w:p><w:r><w:rPr xmlns=\"urn:vendor\"><effect/></w:rPr><w:t>a</w:t></w:r></w:p>",
    );
    assert_ns_survives(&src, "effect", "urn:vendor");
}

#[test]
fn default_namespace_below_root_survives_ppr_child() {
    let src = doc(
        "",
        "<w:p><w:pPr xmlns=\"urn:vendor\"><effect/></w:pPr><w:r><w:t>a</w:t></w:r></w:p>",
    );
    assert_ns_survives(&src, "effect", "urn:vendor");
    let src = doc(
        "",
        &format!(
            "<w:p><w:pPr xmlns=\"{W_NS}\"><keepNext/><effect xmlns=\"urn:vendor\"/></w:pPr></w:p>"
        ),
    );
    let out = saved(&src);
    assert_eq!(namespaces_of(&out, "effect"), ["urn:vendor"], "{out}");
}

#[test]
fn default_namespace_below_root_survives_property_change_snapshot() {
    let src = doc(
        " xmlns=\"urn:vendor\"",
        "<w:p><w:pPr><w:keepNext/><w:pPrChange w:id=\"1\" w:author=\"a\">\
         <w:pPr><effect/></w:pPr></w:pPrChange></w:pPr><w:r><w:t>a</w:t></w:r></w:p>",
    );
    assert_ns_survives(&src, "effect", "urn:vendor");
    let src = doc(
        " xmlns=\"urn:vendor\"",
        "<w:p><w:r><w:rPr><w:b/><w:rPrChange w:id=\"2\" w:author=\"a\">\
         <w:rPr><effect/></w:rPr></w:rPrChange></w:rPr><w:t>a</w:t></w:r></w:p>",
    );
    assert_ns_survives(&src, "effect", "urn:vendor");
}

#[test]
fn default_namespace_below_root_survives_capture_element() {
    let src = doc(
        " xmlns=\"urn:vendor\"",
        "<w:p><w:pPr><w:keepNext/><effect/></w:pPr><w:r><w:t>a</w:t></w:r></w:p>",
    );
    assert_ns_survives(&src, "effect", "urn:vendor");
}

#[test]
fn default_namespace_reset_inside_subtree_is_honoured() {
    let src = doc(
        " xmlns=\"urn:vendor\"",
        "<w:p><w:pPr><w:rPr><outer><inner xmlns=\"urn:other\"/><bare xmlns=\"\"/></outer>\
         </w:rPr></w:pPr></w:p>",
    );
    let out = saved(&src);
    assert_eq!(namespaces_of(&out, "outer"), ["urn:vendor"], "{out}");
    assert_eq!(namespaces_of(&out, "inner"), ["urn:other"], "{out}");
    assert_eq!(namespaces_of(&out, "bare"), [""], "{out}");
}

#[test]
fn reset_of_a_root_default_below_the_root_is_kept() {
    let src = format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns=\"urn:root\"><w:body>\
         <w:p><w:pPr xmlns=\"\"><effect/></w:pPr></w:p></w:body></w:document>"
    );
    assert_ns_survives(&src, "effect", "");
}

#[test]
fn root_default_namespace_adds_no_declarations() {
    let src = format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns=\"urn:root\"><w:body>\
         <w:p><w:pPr><w:keepNext/><effect/></w:pPr></w:p></w:body></w:document>"
    );
    let out = saved(&src);
    assert_eq!(namespaces_of(&out, "effect"), ["urn:root"], "{out}");
    assert_eq!(out.matches("xmlns=").count(), 1, "{out}");
}

#[test]
fn prefixed_only_properties_get_no_default_declaration() {
    let src = doc(
        " xmlns=\"urn:vendor\"",
        "<w:p><w:pPr><w:keepNext/></w:pPr><w:r><w:t>a</w:t></w:r></w:p>",
    );
    assert!(!saved(&src).contains("urn:vendor"));
}

/// The tokens of `mc:Ignorable` in effect at each element named `name`
/// (declared on it or an ancestor), in document order.
fn ignorable_at(xml: &str, name: &str) -> Vec<Vec<String>> {
    let mut scopes: Vec<Vec<String>> = Vec::new();
    let mut found = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                let own: Vec<String> = p
                    .attrs()
                    .iter()
                    .filter(|a| a.name.ends_with(":Ignorable"))
                    .flat_map(|a| a.value.split_whitespace().map(str::to_string))
                    .collect();
                scopes.push(own);
                if p.name() == name {
                    let mut all: Vec<String> = scopes.iter().flatten().cloned().collect();
                    all.sort();
                    all.dedup();
                    found.push(all);
                }
            }
            Event::End => {
                scopes.pop();
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    found
}

/// Prefix `ux` is still bound at each element named `name`.
fn ux_bound_at(xml: &str, name: &str) -> bool {
    namespaces_of_prefixed(xml, name, "ux") == Some(true)
}

fn namespaces_of_prefixed(xml: &str, name: &str, prefix: &str) -> Option<bool> {
    let decl = format!("xmlns:{prefix}");
    let mut scopes: Vec<bool> = Vec::new();
    let mut result = None;
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                scopes.push(p.attrs().iter().any(|a| a.name == decl));
                if p.name() == name {
                    result = Some(scopes.iter().any(|b| *b));
                }
            }
            Event::End => {
                scopes.pop();
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    result
}

fn assert_ignorable_survives(src: &str, element: &str) {
    let want = vec!["ux".to_string()];
    let once = saved(src);
    assert_eq!(ignorable_at(&once, element), [want], "saved: {once}");
    assert!(ux_bound_at(&once, element), "saved: {once}");
    // A second round neither loses nor changes it.
    assert_eq!(once, saved(&once), "stable: {once}");
}

#[test]
fn mce_ignorable_from_body_reaches_paragraph_attr() {
    let src = doc(
        " xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"",
        "<w:p ux:label=\"x\"><w:r><w:t>a</w:t></w:r></w:p>",
    );
    assert_ignorable_survives(&src, "w:p");
}

#[test]
fn mce_ignorable_from_tc_reaches_run() {
    let src = doc(
        "",
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr><w:tc xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"><w:p><w:r ux:label=\"x\">\
         <w:t>a</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>",
    );
    assert_ignorable_survives(&src, "w:r");
}

#[test]
fn mce_ignorable_from_rpr_reaches_shadow() {
    let src = doc(
        "",
        "<w:p><w:r><w:rPr xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"><w:b ux:label=\"x\"/></w:rPr>\
         <w:t>a</w:t></w:r></w:p>",
    );
    let out = saved(&src);
    assert_eq!(ignorable_at(&out, "w:b"), [vec!["ux".to_string()]], "{out}");
    assert!(ux_bound_at(&out, "w:b"), "{out}");
}

#[test]
fn mce_rule_redeclares_only_the_prefixes_the_element_uses() {
    let src = doc(
        " xmlns:ux=\"urn:ux\" xmlns:vy=\"urn:vy\" mc:Ignorable=\"ux vy\"",
        "<w:p ux:label=\"x\"><w:r><w:t>a</w:t></w:r></w:p>",
    );
    let out = saved(&src);
    assert_eq!(ignorable_at(&out, "w:p"), [vec!["ux".to_string()]], "{out}");
    assert!(!out.contains("urn:vy"), "{out}");
}

#[test]
fn mce_without_extension_attrs_is_byte_identical() {
    let body = "<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>a</w:t></w:r></w:p>";
    let with = saved(&doc(" xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"", body));
    assert_eq!(with, saved(&doc("", body)));
}

/// `src` has a rebuilt ancestor's rule (`ux`) and the captured element's own
/// (`vy`); both stay in effect at `element` after a save and a resave.
fn assert_both_rules(src: &str, element: &str) {
    let both = vec!["ux".to_string(), "vy".to_string()];
    let once = saved(src);
    assert_eq!(ignorable_at(&once, element), [both], "saved: {once}");
    assert_eq!(once, saved(&once), "stable: {once}");
}

const OWN_RULE: &str = "mc:Ignorable=\"vy\" xmlns:vy=\"urn:vy\" ux:a=\"1\" vy:b=\"2\"";

#[test]
fn an_inherited_rule_joins_the_captured_elements_own_rule() {
    let ancestor = " xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"";
    assert_both_rules(
        &doc(
            ancestor,
            &format!("<w:p><w:r><w:rPr><w:b {OWN_RULE}/></w:rPr><w:t>a</w:t></w:r></w:p>"),
        ),
        "w:b",
    );
    assert_both_rules(
        &doc(
            ancestor,
            &format!("<w:p><w:pPr><w:keepNext {OWN_RULE}/></w:pPr></w:p>"),
        ),
        "w:keepNext",
    );
    for (wrapper, inner) in [
        (
            "w:tblPr",
            "<w:tblPr><x:p {OWN}/></w:tblPr><w:tblGrid/><w:tr><w:tc><w:p/></w:tc></w:tr>",
        ),
        (
            "w:trPr",
            "<w:tblGrid/><w:tr><w:trPr><x:p {OWN}/></w:trPr><w:tc><w:p/></w:tc></w:tr>",
        ),
        (
            "w:tcPr",
            "<w:tblGrid/><w:tr><w:tc><w:tcPr><x:p {OWN}/></w:tcPr><w:p/></w:tc></w:tr>",
        ),
    ] {
        let body = format!("<w:tbl>{}</w:tbl><w:p/>", inner.replace("{OWN}", OWN_RULE));
        let src = doc(ancestor, &body);
        let once = saved(&src);
        assert_eq!(
            ignorable_at(&once, "x:p"),
            [vec!["ux".to_string(), "vy".to_string()]],
            "{wrapper}: {once}"
        );
    }
}

#[test]
fn an_inherited_rule_joins_an_own_rule_spelled_with_another_prefix() {
    let src = doc(
        " xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"",
        "<w:p><w:r><w:rPr><w:b ve:Ignorable=\"vy\" xmlns:ve=\"{MC}\" xmlns:vy=\"urn:vy\" \
         ux:a=\"1\" vy:b=\"2\"/></w:rPr><w:t>a</w:t></w:r></w:p>"
            .replace("{MC}", MC_NS)
            .as_str(),
    );
    let out = saved(&src);
    assert_eq!(
        ignorable_at(&out, "w:b"),
        [vec!["ux".to_string(), "vy".to_string()]],
        "{out}"
    );
    assert_eq!(out.matches("Ignorable=").count(), 1, "{out}");
}

/// A rule on a `w:p` start tag is written back on both halves of a split
/// paragraph, so a captured child that depends on it stays covered (the
/// reason `w:p` is not an ancestor to redeclare from).
#[test]
fn a_split_paragraph_keeps_its_rule_on_both_halves() {
    let src = doc(
        "",
        "<w:p xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"><w:r><w:rPr><w:b ux:a=\"1\"/></w:rPr>\
         <w:t>HelloWorld</w:t></w:r></w:p>",
    );
    let out = saved_after(&src, |editor| {
        editor.set_caret(Caret::top(0, 5));
        editor.insert_newline();
    });
    assert_eq!(out.matches("<w:p ").count(), 2, "{out}");
    assert_eq!(out.matches("mc:Ignorable=\"ux\"").count(), 2, "{out}");
}

/// Rejecting a property change restores its snapshot, whose `ux:` attribute
/// needs the rule the rebuilt `w:body` declared (the snapshot is read apart
/// from the part root that binds `mc`).
#[test]
fn rejected_property_changes_restore_the_inherited_rule() {
    let ancestor = " xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"";
    let cases = [
        (
            "w:b",
            "<w:p><w:r><w:rPr><w:i/><w:rPrChange w:id=\"1\" w:author=\"a\">\
             <w:rPr><w:b ux:a=\"1\"/></w:rPr></w:rPrChange></w:rPr><w:t>a</w:t></w:r></w:p>",
        ),
        (
            "w:keepNext",
            "<w:p><w:pPr><w:jc w:val=\"center\"/><w:pPrChange w:id=\"1\" w:author=\"a\">\
             <w:pPr><w:keepNext ux:a=\"1\"/></w:pPr></w:pPrChange></w:pPr></w:p>",
        ),
        (
            "w:tblStyle",
            "<w:tbl><w:tblPr><w:tblW w:w=\"0\"/><w:tblPrChange w:id=\"1\" w:author=\"a\">\
             <w:tblPr><w:tblStyle w:val=\"Old\" ux:a=\"1\"/></w:tblPr></w:tblPrChange></w:tblPr>\
             <w:tblGrid/><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p/>",
        ),
    ];
    for (element, body) in cases {
        let src = doc(ancestor, body);
        let out = saved_after(&src, |editor| {
            editor.reject_all_revisions();
        });
        assert!(out.contains("ux:a=\"1\""), "{element} restored: {out}");
        assert_eq!(
            ignorable_at(&out, element),
            [vec!["ux".to_string()]],
            "{element}: {out}"
        );
        assert!(ux_bound_at(&out, element), "{element}: {out}");
        // Left alone, the change keeps the same.
        let kept = saved(&src);
        assert_eq!(kept, saved(&kept), "{element} stable: {kept}");
        assert!(kept.contains("ux:a=\"1\""), "{element} kept: {kept}");
    }
}

/// Review r3: a change wrapper or a property container that carries a rule of
/// its own gets the inherited one merged in, with the MC prefix declared on
/// the tag, so the snapshot read apart from the root keeps both.
#[test]
fn rejected_changes_under_a_wrapper_with_its_own_rule_keep_both_rules() {
    let ancestor = " xmlns:ux=\"urn:ux\" mc:Ignorable=\"ux\"";
    let own = "mc:Ignorable=\"vy\" xmlns:vy=\"urn:vy\"";
    let cases = [
        (
            "w:b",
            format!(
                "<w:p><w:r><w:rPr><w:i/><w:rPrChange w:id=\"1\" w:author=\"a\" {own}>\
                 <w:rPr><w:b ux:a=\"1\" vy:b=\"2\"/></w:rPr></w:rPrChange></w:rPr>\
                 <w:t>a</w:t></w:r></w:p>"
            ),
        ),
        (
            "w:tblStyle",
            format!(
                "<w:tbl><w:tblPr {own}><w:tblW w:w=\"0\"/><w:tblPrChange w:id=\"1\" w:author=\"a\">\
                 <w:tblPr><w:tblStyle w:val=\"Old\" ux:a=\"1\" vy:b=\"2\"/></w:tblPr>\
                 </w:tblPrChange></w:tblPr><w:tblGrid/><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p/>"
            ),
        ),
    ];
    for (element, body) in cases {
        let src = doc(ancestor, &body);
        let out = saved_after(&src, |editor| {
            editor.reject_all_revisions();
        });
        assert!(out.contains("ux:a=\"1\""), "{element} restored: {out}");
        let rule = ignorable_at(&out, element);
        assert_eq!(
            rule,
            [vec!["ux".to_string(), "vy".to_string()]],
            "{element}: {out}"
        );
        assert!(ux_bound_at(&out, element), "{element}: {out}");
    }
}
