//! Tests for the editable-HTML surface of the bridge: `doc_json` (the rich
//! model), `exec_json`, `state_json`, and the `select` and formatting verbs.

use super::*;
use crate::json::Json;
use docxcore::package::{new_package, save_package};

fn docx_from_body(body: &str) -> Vec<u8> {
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <w:body>{body}</w:body></w:document>"
    );
    let doc = docxcore::load::parse_document_xml(&xml, &Default::default());
    save_package(&new_package(doc))
}

fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// The repository's showcase document: headings, lists, a table, a picture
/// and a hyperlink.
fn showcase() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../assets/sample.docx"
    ))
    .unwrap()
}

fn open(bytes: &[u8]) -> Session {
    Session::open(bytes).expect("open")
}

fn parse(s: &str) -> Json {
    Json::parse(s).unwrap_or_else(|e| panic!("bad JSON ({e}): {s}"))
}

fn arr(j: &Json) -> &[Json] {
    match j {
        Json::Arr(v) => v,
        other => panic!("expected an array, got {other:?}"),
    }
}

/// Every paragraph object in document order, tables included.
fn paragraphs(blocks: &Json, out: &mut Vec<Json>) {
    for b in arr(blocks) {
        match b.get_str("t") {
            Some("p") => out.push(b.clone()),
            Some("tbl") => {
                for row in arr(b.get("rows").unwrap()) {
                    for cell in arr(row) {
                        paragraphs(cell.get("blocks").unwrap(), out);
                    }
                }
            }
            other => panic!("unknown block {other:?}"),
        }
    }
}

fn all_paragraphs(s: &Session) -> Vec<Json> {
    let doc = parse(&s.doc_json());
    let mut out = Vec::new();
    paragraphs(doc.get("blocks").unwrap(), &mut out);
    out
}

fn para_text(p: &Json) -> String {
    arr(p.get("segs").unwrap())
        .iter()
        .filter(|s| s.get_str("k") == Some("t"))
        .map(|s| s.get_str("x").unwrap().to_string())
        .collect()
}

#[test]
fn segment_offsets_tile_every_paragraph() {
    let s = open(&showcase());
    let paras = all_paragraphs(&s);
    assert!(paras.len() > 20, "showcase should have many paragraphs");
    for p in &paras {
        let len = p.get_usize("len").unwrap();
        let mut at = 0;
        for seg in arr(p.get("segs").unwrap()) {
            assert_eq!(seg.get_usize("o").unwrap(), at, "gap or overlap in {p:?}");
            let w = seg.get_usize("w").unwrap();
            if seg.get_str("k") == Some("t") {
                assert_eq!(w, seg.get_str("x").unwrap().chars().count());
            }
            at += w;
        }
        assert_eq!(at, len, "widths must sum to the caret length: {p:?}");
        // And the editor agrees about that length.
        let path = richdoc::parse_path(p.get_str("p").unwrap()).unwrap();
        let para = resolve_para(&s.editor.doc.body, &path).unwrap();
        assert_eq!(para_text_len_via_editor(&s, &path), para_text_len(para));
    }
}

/// The editor's own idea of a paragraph's length: park the caret at its end.
fn para_text_len_via_editor(s: &Session, path: &[usize]) -> usize {
    let mut ed = Editor::new(s.editor.doc.clone());
    ed.set_caret(Caret::at(path.to_vec(), usize::MAX));
    ed.caret.offset
}

#[test]
fn showcase_model_has_tables_lists_links_and_pictures() {
    let mut s = open(&showcase());
    let doc = parse(&s.doc_json());
    let page = doc.get("page").unwrap();
    assert!(page.get_usize("w").unwrap() > 10_000);
    assert!(page.get_usize("left").unwrap() > 0);
    assert!(
        arr(doc.get("blocks").unwrap())
            .iter()
            .any(|b| b.get_str("t") == Some("tbl"))
    );
    let paras = all_paragraphs(&s);
    assert!(
        paras
            .iter()
            .any(|p| p.get_str("p").unwrap().split('.').count() == 4),
        "table-cell paragraphs are addressed table.row.cell.block"
    );
    assert!(paras.iter().any(|p| p.get_str("list").is_some()));
    assert!(paras.iter().any(|p| p.get("head").is_some()));
    let segs: Vec<Json> = paras
        .iter()
        .flat_map(|p| arr(p.get("segs").unwrap()).to_vec())
        .collect();
    assert!(segs.iter().any(|s| s.get_str("href").is_some()));
    // The showcase's diagram is a mermaid drawing: an atom with its labels.
    assert!(segs.iter().any(|s| s.get_str("k") == Some("art")));
    assert!(doc.get("caret").unwrap().get_str("p").is_some());
    assert_eq!(doc.get("dirty"), Some(&Json::Bool(false)));
    // Rendering is read-only.
    assert!(!s.dirty);
    let _ = s.exec_json("move\tright\t0");
    assert!(!s.dirty);
}

#[test]
fn pictures_are_atoms_the_page_can_fetch() {
    let document_xml = r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>x</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:extent cx="914400" cy="457200"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
    let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
    let bytes = docxcore::zipwrite::write_zip(&[
        ("[Content_Types].xml".into(), b"<Types/>".to_vec()),
        ("_rels/.rels".into(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".into(), document_xml.as_bytes().to_vec()),
        (
            "word/_rels/document.xml.rels".into(),
            rels.as_bytes().to_vec(),
        ),
        ("word/media/image1.png".into(), png.clone()),
    ]);
    let s = open(&bytes);
    let p = &all_paragraphs(&s)[0];
    let img = arr(p.get("segs").unwrap())
        .iter()
        .find(|s| s.get_str("k") == Some("img"))
        .expect("a picture segment")
        .clone();
    assert_eq!(img.get_str("rid"), Some("rId9"));
    assert_eq!(img.get_usize("cx"), Some(914_400));
    assert_eq!(img.get_usize("cy"), Some(457_200));
    assert_eq!(img.get_usize("o"), Some(1));
    assert_eq!(s.media("rId9"), Some(png));
}

#[test]
fn tracked_changes_are_zero_width_atoms() {
    let bytes = docx_from_body(
        "<w:p><w:r><w:t>A</w:t></w:r>\
         <w:ins w:id=\"1\" w:author=\"Ada\"><w:r><w:t>new</w:t></w:r></w:ins>\
         <w:del w:id=\"2\" w:author=\"Linus\"><w:r><w:delText>old</w:delText></w:r></w:del>\
         <w:r><w:t>B</w:t></w:r></w:p>",
    );
    let s = open(&bytes);
    let p = &all_paragraphs(&s)[0];
    assert_eq!(p.get_usize("len"), Some(2));
    let segs = arr(p.get("segs").unwrap());
    let revs: Vec<&Json> = segs
        .iter()
        .filter(|s| s.get_str("k") == Some("rev"))
        .collect();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0].get_str("rev"), Some("ins"));
    assert_eq!(revs[0].get_str("x"), Some("new"));
    assert_eq!(revs[0].get_str("author"), Some("Ada"));
    assert_eq!(revs[1].get_str("rev"), Some("del"));
    assert!(revs.iter().all(|r| r.get_usize("w") == Some(0)));
    assert!(revs.iter().all(|r| r.get_usize("o") == Some(1)));
}

#[test]
fn typing_after_a_revision_and_a_link_lands_at_the_right_offset() {
    let bytes = docx_from_body(
        "<w:p><w:r><w:t>A</w:t></w:r>\
         <w:ins w:id=\"1\" w:author=\"Ada\"><w:r><w:t>new</w:t></w:r></w:ins>\
         <w:hyperlink w:anchor=\"x\"><w:r><w:t>link</w:t></w:r></w:hyperlink>\
         <w:r><w:t>B</w:t></w:r></w:p>",
    );
    let mut s = open(&bytes);
    assert_eq!(para_text(&all_paragraphs(&s)[0]), "AlinkB");
    // After "Alink": offset 5.
    let r = parse(&s.exec_json("select\t0\t5\t0\t5"));
    assert_eq!(r.get("anchor"), Some(&Json::Null));
    let r = parse(&s.exec_json("insert\t\u{1F600}"));
    assert_eq!(r.get("applied"), Some(&Json::Bool(true)));
    assert_eq!(r.get("caret").unwrap().get_usize("o"), Some(6));
    assert_eq!(para_text(&all_paragraphs(&s)[0]), "Alink\u{1F600}B");
}

#[test]
fn emoji_counts_as_one_offset() {
    let mut s = open(&docx_from_body(&para("a\u{1F600}b")));
    let p = &all_paragraphs(&s)[0];
    assert_eq!(p.get_usize("len"), Some(3));
    s.exec_json("select\t0\t2\t0\t2");
    s.exec_json("insert\tX");
    assert_eq!(para_text(&all_paragraphs(&s)[0]), "a\u{1F600}Xb");
}

#[test]
fn select_sets_a_logical_selection() {
    let mut s = open(&docx_from_body(&format!(
        "{}{}",
        para("Hello world"),
        para("Second")
    )));
    let r = parse(&s.exec_json("select\t0\t6\t1\t3"));
    assert_eq!(r.get("applied"), Some(&Json::Bool(false)), "not a mutation");
    assert_eq!(r.get("anchor").unwrap().get_usize("o"), Some(6));
    assert_eq!(r.get("caret").unwrap().get_str("p"), Some("1"));
    assert_eq!(s.editor.selection_text(), "world\nSec");
    // A backwards selection keeps its direction.
    s.exec_json("select\t1\t3\t0\t6");
    assert_eq!(s.editor.caret, Caret::at(vec![0], 6));
    assert_eq!(s.editor.selection_text(), "world\nSec");
    // Offsets clamp to the paragraph.
    s.exec_json("select\t0\t99\t0\t99");
    assert_eq!(s.editor.caret, Caret::at(vec![0], 11));
    assert!(!s.editor.has_selection());
}

#[test]
fn select_rejects_paths_that_are_not_paragraphs() {
    let mut s = open(&docx_from_body(&para("Hello")));
    s.exec_json("select\t0\t2\t0\t2");
    for bad in [
        "select\t7\t0\t7\t0",
        "select\t0.1\t0\t0\t0",
        "select\t\t0\t0\t0",
        "select\tx\t0\t0\t0",
        "select\t0\tNaN\t0\t0",
        "select\t0\t0",
    ] {
        let r = parse(&s.exec_json(bad));
        assert_eq!(r.get("applied"), Some(&Json::Bool(false)));
        assert_eq!(
            s.editor.caret,
            Caret::at(vec![0], 2),
            "{bad} moved the caret"
        );
    }
}

#[test]
fn select_reaches_table_cells_and_state_reports_in_table() {
    let mut s = open(&showcase());
    let cell = all_paragraphs(&s)
        .into_iter()
        .find(|p| p.get_str("p").unwrap().split('.').count() == 4)
        .unwrap();
    let path = cell.get_str("p").unwrap().to_string();
    assert_eq!(
        parse(&s.state_json()).get("inTable"),
        Some(&Json::Bool(false))
    );
    s.exec_json(&format!("select\t{path}\t0\t{path}\t1"));
    let st = parse(&s.state_json());
    assert_eq!(st.get("inTable"), Some(&Json::Bool(true)));
    assert_eq!(st.get("selection"), Some(&Json::Bool(true)));
}

#[test]
fn bold_over_a_selection_round_trips_through_save() {
    let mut s = open(&docx_from_body(&para("plain text")));
    s.exec_json("select\t0\t6\t0\t10");
    let r = parse(&s.exec_json("bold"));
    assert_eq!(r.get("applied"), Some(&Json::Bool(true)));
    assert_eq!(r.get("dirty"), Some(&Json::Bool(true)));
    let st = parse(&s.state_json());
    assert_eq!(st.get("bold"), Some(&Json::Bool(true)));
    let saved = s.save();
    let pkg = docxcore::package::load_package(&saved).unwrap();
    let xml = String::from_utf8(pkg.part("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(xml.contains("<w:b/>"), "{xml}");
    let segs = arr(all_paragraphs(&s)[0].get("segs").unwrap()).to_vec();
    assert!(
        segs.iter()
            .any(|g| g.get_str("x") == Some("text") && g.get("b") == Some(&Json::Bool(true)))
    );
}

#[test]
fn formatting_verbs_apply_over_the_selection() {
    let mut s = open(&docx_from_body(&para("abcdef")));
    let sel = |s: &mut Session| {
        s.exec_json("select\t0\t0\t0\t6");
    };
    sel(&mut s);
    s.exec_json("vertalign\tsuper");
    assert_eq!(s.editor.caret_props().vert_align, VertAlign::Superscript);
    s.exec_json("vertalign\tsub");
    assert_eq!(s.editor.caret_props().vert_align, VertAlign::Subscript);
    s.exec_json("highlight\tyellow");
    s.exec_json("font\tArial");
    s.exec_json("setsize\t28");
    let st = parse(&s.state_json());
    assert_eq!(st.get_str("font"), Some("Arial"));
    assert_eq!(st.get_usize("size"), Some(28));
    let seg = arr(all_paragraphs(&s)[0].get("segs").unwrap())[0].clone();
    assert_eq!(seg.get_str("hl"), Some("yellow"));
    assert_eq!(seg.get_str("va"), Some("sub"));
    s.exec_json("clearfmt");
    let props = s.editor.caret_props();
    assert!(props.font.is_none() && props.highlight.is_none());
    assert_eq!(props.vert_align, VertAlign::Baseline);
    // Bad arguments are no-ops, not mutations.
    for bad in ["setsize\t0", "setsize\tbig", "font\t", "linespacing\t-1"] {
        assert_eq!(
            parse(&s.exec_json(bad)).get("applied"),
            Some(&Json::Bool(false)),
            "{bad}"
        );
    }
    s.exec_json("case");
    // Word's Shift+F3 cycle: lowercase text becomes Capitalized first.
    assert_eq!(para_text(&all_paragraphs(&s)[0]), "Abcdef");
}

#[test]
fn paragraph_verbs_match_the_suite() {
    let mut s = open(&docx_from_body(&format!("{}{}", para("b"), para("a"))));
    s.exec_json("style\tHeading2");
    assert_eq!(parse(&s.state_json()).get_str("style"), Some("Heading2"));
    assert_eq!(all_paragraphs(&s)[0].get_usize("head"), Some(2));
    s.exec_json("style\tNormal");
    assert_eq!(parse(&s.state_json()).get("style"), Some(&Json::Null));
    s.exec_json("nospacing");
    assert_eq!(
        parse(&s.state_json()).get("noSpacing"),
        Some(&Json::Bool(true))
    );
    s.exec_json("linespacing\t360\tauto");
    assert_eq!(
        parse(&s.state_json()).get("lineSpacing"),
        Some(&Json::Num(1.5))
    );
    s.exec_json("borders");
    assert_eq!(
        parse(&s.state_json()).get("borderBottom"),
        Some(&Json::Bool(true))
    );
    s.exec_json("borders");
    assert_eq!(
        parse(&s.state_json()).get("borderBottom"),
        Some(&Json::Bool(false))
    );
    s.exec_json("list\tbullet");
    let st = parse(&s.state_json());
    assert_eq!(st.get("bullets"), Some(&Json::Bool(true)));
    assert_eq!(st.get("numbers"), Some(&Json::Bool(false)));
    assert!(all_paragraphs(&s)[0].get_str("list").is_some());
    s.exec_json("select\t0\t0\t1\t1");
    s.exec_json("sort");
    let texts: Vec<String> = all_paragraphs(&s).iter().map(para_text).collect();
    assert_eq!(texts, ["a", "b"]);
}

#[test]
fn tab_inserts_a_tab_inline() {
    let mut s = open(&docx_from_body(&para("ab")));
    s.exec_json("select\t0\t1\t0\t1");
    let r = parse(&s.exec_json("tab"));
    assert_eq!(r.get("applied"), Some(&Json::Bool(true)));
    assert_eq!(r.get("caret").unwrap().get_usize("o"), Some(2));
    let p = &all_paragraphs(&s)[0];
    let segs = arr(p.get("segs").unwrap());
    let tab = segs.iter().find(|g| g.get_str("k") == Some("tab")).unwrap();
    assert_eq!(tab.get_usize("o"), Some(1));
    assert_eq!(tab.get_usize("w"), Some(1));
    assert_eq!(para_text(p), "ab");
}

#[test]
fn copy_reports_text_without_mutating() {
    let mut s = open(&docx_from_body(&para("Hello")));
    s.exec_json("select\t0\t1\t0\t4");
    let r = parse(&s.exec_json("copy"));
    assert_eq!(r.get_str("copied"), Some("ell"));
    assert_eq!(r.get("applied"), Some(&Json::Bool(false)));
    assert_eq!(r.get("dirty"), Some(&Json::Bool(false)));
    let r = parse(&s.exec_json("cut"));
    assert_eq!(r.get_str("copied"), Some("ell"));
    assert_eq!(para_text(&all_paragraphs(&s)[0]), "Ho");
    let r = parse(&s.exec_json("undo"));
    assert_eq!(r.get("applied"), Some(&Json::Bool(true)));
    assert_eq!(para_text(&all_paragraphs(&s)[0]), "Hello");
}

#[test]
fn read_only_documents_refuse_the_new_verbs() {
    let mut s = open(&protected_docx());
    s.exec_json("select\t0\t0\t0\t3");
    for cmd in [
        "vertalign\tsuper",
        "clearfmt",
        "highlight\tyellow",
        "font\tArial",
        "setsize\t30",
        "linespacing\t480",
        "style\tHeading1",
        "nospacing",
        "case",
        "borders",
        "sort",
        "hrule",
        "tab",
    ] {
        let r = parse(&s.exec_json(cmd));
        assert_eq!(r.get("applied"), Some(&Json::Bool(false)), "{cmd}");
    }
    assert!(!s.dirty);
}

fn protected_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>locked</w:t></w:r></w:p></w:body></w:document>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/></Relationships>"#;
    let settings_xml = r#"<?xml version="1.0"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:documentProtection w:edit="readOnly" w:enforcement="1"/></w:settings>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
    docxcore::zipwrite::write_zip(&[
        ("[Content_Types].xml".into(), b"<Types/>".to_vec()),
        ("_rels/.rels".into(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".into(), document_xml.as_bytes().to_vec()),
        (
            "word/_rels/document.xml.rels".into(),
            document_rels.as_bytes().to_vec(),
        ),
        ("word/settings.xml".into(), settings_xml.as_bytes().to_vec()),
        ("word/styles.xml".into(), b"<w:styles/>".to_vec()),
    ])
}
