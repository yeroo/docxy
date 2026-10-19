//! Verbatim slices the loader keeps for collapsed complex fields,
//! `w:fldSimple` and `w:ins`/`w:del` declare the prefixes they use that are
//! bound below the part root, so a save is namespace-well-formed (#1397).

use docxcore::load::{Relationships, parse_document_xml};
use docxcore::serialize::document_to_xml;
use opccore::xml::{Event, XmlParser};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Every prefix used by an element or attribute name is bound on an open
/// element (or is `xml`/`xmlns`).
fn unbound_prefixes(xml: &str) -> Vec<String> {
    let mut scopes: Vec<Vec<String>> = Vec::new();
    let mut bad = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                scopes.push(
                    p.attrs()
                        .iter()
                        .filter_map(|a| a.name.strip_prefix("xmlns:").map(str::to_string))
                        .collect(),
                );
                let names = std::iter::once(p.name()).chain(
                    p.attrs()
                        .iter()
                        .map(|a| a.name)
                        .filter(|n| !n.starts_with("xmlns")),
                );
                for name in names {
                    if let Some((prefix, _)) = name.split_once(':') {
                        if prefix != "xml" && !scopes.iter().flatten().any(|s| s == prefix) {
                            bad.push(format!("{prefix} in {name}"));
                        }
                    }
                }
            }
            Event::End => {
                scopes.pop();
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    bad
}

fn cell(para: &str) -> String {
    format!(
        "<w:document xmlns:w=\"{W_NS}\"><w:body><w:tbl><w:tr><w:tc xmlns:x=\"urn:x\">\
         {para}</w:tc></w:tr></w:tbl></w:body></w:document>"
    )
}

fn saved(xml: &str) -> String {
    document_to_xml(&parse_document_xml(xml, &Relationships::default()))
}

fn assert_bound(saved: &str) {
    let bad = unbound_prefixes(saved);
    assert!(bad.is_empty(), "unbound {bad:?}: {saved}");
}

const X_RPR: &str = "<w:rPr><x:foo/></w:rPr>";

fn field_runs(rpr: &str) -> String {
    format!(
        "<w:r>{rpr}<w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r>{rpr}<w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
         <w:r>{rpr}<w:fldChar w:fldCharType=\"separate\"/></w:r>\
         <w:r>{rpr}<w:t>3</w:t></w:r>\
         <w:r>{rpr}<w:fldChar w:fldCharType=\"end\"/></w:r>"
    )
}

#[test]
fn a_collapsed_field_declares_a_prefix_bound_below_the_part_root_on_every_run() {
    let out = saved(&cell(&format!("<w:p>{}</w:p>", field_runs(X_RPR))));
    assert_bound(&out);
    assert_eq!(out.matches("<w:r xmlns:x=\"urn:x\">").count(), 5, "{out}");
}

#[test]
fn nested_and_sibling_fields_declare_every_run() {
    let inner = field_runs(X_RPR);
    let nested = format!(
        "<w:r>{X_RPR}<w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r>{X_RPR}<w:instrText> IF 1 = 1 </w:instrText></w:r>\
         <w:r>{X_RPR}<w:fldChar w:fldCharType=\"separate\"/></w:r>{inner}\
         <w:r>{X_RPR}<w:fldChar w:fldCharType=\"end\"/></w:r>"
    );
    let out = saved(&cell(&format!(
        "<w:p>{nested}<w:r><w:t>,</w:t></w:r>{}</w:p>",
        field_runs(X_RPR)
    )));
    assert_bound(&out);
}

#[test]
fn a_collapsed_field_without_a_below_root_prefix_is_unchanged() {
    let body = format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:x=\"urn:x\"><w:body><w:p>{} <w:r><w:t>b</w:t></w:r>\
         </w:p></w:body></w:document>",
        field_runs(X_RPR)
    );
    // The part root's `x` is kept by the package save, not by this bare one.
    let out = saved(&body);
    assert!(!out.contains("<w:r xmlns:x"), "{out}");
    let plain = saved(&cell(&format!("<w:p>{} </w:p>", field_runs(""))));
    assert!(!plain.contains("xmlns:x"), "{plain}");
    assert!(plain.contains(&field_runs("")), "{plain}");
}

#[test]
fn a_fld_simple_declares_a_prefix_bound_below_the_part_root() {
    let para = format!(
        "<w:p><w:fldSimple w:instr=\" PAGE \"><w:r>{X_RPR}<w:t>3</w:t></w:r></w:fldSimple></w:p>"
    );
    let out = saved(&cell(&para));
    assert_bound(&out);
    assert!(out.contains("<w:fldSimple xmlns:x=\"urn:x\""), "{out}");
}

#[test]
fn a_revision_declares_a_prefix_bound_below_the_part_root() {
    for (tag, text) in [("w:ins", "w:t"), ("w:del", "w:delText")] {
        let para = format!(
            "<w:p><{tag} w:id=\"1\" w:author=\"A\"><w:r>{X_RPR}<{text}>a</{text}></w:r></{tag}></w:p>"
        );
        let out = saved(&cell(&para));
        assert_bound(&out);
        assert!(out.contains(&format!("<{tag} xmlns:x=\"urn:x\"")), "{out}");
    }
}
