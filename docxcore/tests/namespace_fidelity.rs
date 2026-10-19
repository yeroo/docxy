//! Namespace fidelity of captured subtrees on save: a default namespace and
//! markup-compatibility rules declared below the part root (#1089).

use docxcore::package::{load_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;
use opccore::xml::{Event, XmlParser};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// `document` as `word/document.xml` of a minimal package, loaded and saved
/// again (a part root is written back as loaded), then its saved part.
fn saved(document: &str) -> String {
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
        ("word/document.xml".to_string(), document.as_bytes().to_vec()),
    ]);
    let mut pkg = load_package(&bytes).expect("fixture loads");
    pkg.document = pkg.document.clone();
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
