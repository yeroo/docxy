//! What the model does not represent survives a save (#1063): the start-tag
//! attributes of `w:p`/`w:r`/`w:tr` (rsids, `w14:paraId`), and everything an
//! element the model reads a value from carries besides that value
//! (`w:rFonts w:hAnsi`, `w:szCs`, `w:color w:themeColor`, `w:ind
//! w:firstLineChars`, clear tabs, left/right borders, …). An edit rewrites
//! only the property it changes.

// The fidelity gate's canonical comparator (`tests/fidelity/mod.rs`).
#[allow(dead_code)]
#[path = "fidelity/mod.rs"]
mod comparator;

use comparator::{Finding, apply_allowlist, compare_packages, parse_allowlist, parse_xml};
use docxcore::editor::{Caret, Editor};
use docxcore::model::{Align, Block, Inline};
use docxcore::package::{Package, load_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// A minimal package whose `word/document.xml` body is `body`, under a root
/// that declares `w14` (ignorable), as Word writes it.
fn docx(body: &str) -> Vec<u8> {
    docx_with_root(
        &format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:w14=\"{W14_NS}\" xmlns:mc=\"{MC_NS}\" \
             mc:Ignorable=\"w14\">"
        ),
        &format!("<w:body>{body}</w:body>"),
    )
}

/// A minimal package whose `word/document.xml` is `root` (the `w:document`
/// start tag), then `body` (the whole `w:body` element).
fn docx_with_root(root: &str, body: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n{root}{body}</w:document>"
    );
    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document.into_bytes()),
    ])
}

fn open(bytes: &[u8]) -> (Package, Editor) {
    let pkg = load_package(bytes).expect("fixture loads");
    let editor = Editor::new(pkg.document.clone());
    (pkg, editor)
}

/// Save the editor's document into `pkg` (the save path of an edited file).
fn save(mut pkg: Package, editor: Editor) -> Vec<u8> {
    pkg.document = editor.doc;
    save_package(&pkg)
}

fn document_xml(bytes: &[u8]) -> String {
    let archive = ZipArchive::open(bytes).expect("saved package is a ZIP");
    let xml = String::from_utf8(archive.read("word/document.xml").expect("document part"))
        .expect("UTF-8 document part");
    assert!(
        parse_xml(xml.as_bytes()).is_some(),
        "saved document.xml is not well-formed:\n{xml}"
    );
    xml
}

/// The differences in `word/document.xml` the gate would report: those its
/// allowlist (`xml:space` on every saved `w:t`, …) does not cover.
fn losses(original: &[u8], saved: &[u8]) -> Vec<Finding> {
    let rules = parse_allowlist(include_str!("fidelity/allowlist.txt")).expect("allowlist");
    let mut counts = vec![0; rules.len()];
    apply_allowlist(compare_packages(original, saved), &rules, &mut counts)
        .into_iter()
        .filter(|f| f.part == "word/document.xml")
        .collect()
}

fn round_trip(body: &str) -> (Vec<u8>, Vec<u8>) {
    let original = docx(body);
    let (pkg, editor) = open(&original);
    let saved = save(pkg, editor);
    (original, saved)
}

/// Load `body`, run `edit` on the whole document selected, save.
fn edited(body: &str, edit: impl FnOnce(&mut Editor)) -> String {
    let (pkg, mut editor) = open(&docx(body));
    editor.select_all();
    edit(&mut editor);
    document_xml(&save(pkg, editor))
}

/// The `w:rPr` of the first run whose text is `text`.
fn rpr_of<'a>(xml: &'a str, text: &str) -> &'a str {
    let t = xml
        .find(&format!(">{text}</w:t>"))
        .unwrap_or_else(|| panic!("no run {text:?} in {xml}"));
    let r = xml[..t]
        .rfind("<w:r>")
        .max(xml[..t].rfind("<w:r "))
        .expect("run start");
    let run = &xml[r..t];
    match (run.find("<w:rPr>"), run.find("</w:rPr>")) {
        (Some(a), Some(b)) => &run[a..b + "</w:rPr>".len()],
        _ => "",
    }
}

const RICH_PARAGRAPH: &str = "<w:p w:rsidR=\"00A1B2C3\" w:rsidRPr=\"00D4E5F6\" \
    w:rsidRDefault=\"00A1B2C3\" w:rsidP=\"00112233\" w14:paraId=\"1A2B3C4D\" \
    w14:textId=\"77DE0FA1\">\
    <w:pPr><w:pStyle w:val=\"Body\"/>\
    <w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"3\"/>\
    <w:ins w:id=\"9\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\"/></w:numPr>\
    <w:pBdr><w:top w:val=\"single\" w:sz=\"12\" w:space=\"4\" w:color=\"FF0000\"/>\
    <w:left w:val=\"double\" w:sz=\"4\" w:space=\"4\" w:color=\"auto\"/>\
    <w:right w:val=\"double\" w:sz=\"4\" w:space=\"4\" w:color=\"auto\"/></w:pBdr>\
    <w:tabs><w:tab w:val=\"clear\" w:pos=\"720\"/><w:tab w:val=\"decimal\" w:pos=\"4320\"/></w:tabs>\
    <w:ind w:leftChars=\"200\" w:left=\"420\" w:firstLineChars=\"100\" w:firstLine=\"210\"/>\
    <w:jc w:val=\"left\"/></w:pPr>\
    <w:r w:rsidR=\"00A1B2C3\" w:rsidRPr=\"00D4E5F6\"><w:rPr>\
    <w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:eastAsia=\"SimSun\" w:cs=\"Arial\" w:hint=\"eastAsia\"/>\
    <w:b w:val=\"1\"/><w:bCs/>\
    <w:color w:val=\"1F3864\" w:themeColor=\"accent1\" w:themeShade=\"80\"/>\
    <w:sz w:val=\"20\"/><w:szCs w:val=\"21\"/><w:highlight w:val=\"none\"/>\
    <w:u w:val=\"single\" w:color=\"00FF00\"/><w:lang w:val=\"fr-FR\" w:eastAsia=\"zh-CN\"/>\
    </w:rPr><w:t>Rich</w:t></w:r>\
    <w:r w:rsidR=\"00A1B2C3\"><w:rPr><w:rFonts w:hint=\"eastAsia\"/></w:rPr><w:tab/></w:r>\
    <w:r w:rsidRPr=\"00D4E5F6\"><w:rPr><w:szCs w:val=\"24\"/></w:rPr><w:br/></w:r>\
    <w:r><w:rPr><w:rStyle w:val=\"\"/><w:color w:val=\"auto\"/></w:rPr><w:t>tail</w:t></w:r></w:p>";

#[test]
fn an_untouched_document_keeps_attributes_and_property_remainders() {
    let body = format!(
        "{RICH_PARAGRAPH}\
         <w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr w:rsidR=\"00ABCDEF\" w:rsidTr=\"00FEDCBA\" w14:paraId=\"2B3C4D5E\" w14:textId=\"77DE0FA1\">\
         <w:tc><w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/></w:tcPr>\
         <w:p w:rsidR=\"00ABCDEF\" w14:paraId=\"3C4D5E6F\"><w:r w:rsidRPr=\"00ABCDEF\">\
         <w:rPr><w:rFonts w:hAnsi=\"Calibri\"/></w:rPr><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
         <w:p/>"
    );
    let (original, saved) = round_trip(&body);
    let xml = document_xml(&saved);
    assert!(
        losses(&original, &saved).is_empty(),
        "{:#?}\n{xml}",
        losses(&original, &saved)
    );
    let root = &xml[xml.find("<w:document").unwrap()..];
    assert!(root[..root.find('>').unwrap()].contains(&format!("xmlns:w14=\"{W14_NS}\"")));
}

#[test]
fn editing_one_paragraph_leaves_its_neighbour_exact() {
    let body = format!(
        "{RICH_PARAGRAPH}<w:p w:rsidR=\"00CCCCCC\" w14:paraId=\"5E6F7A8B\"><w:r w:rsidR=\"00CCCCCC\"><w:t>second</w:t></w:r></w:p>"
    );
    let original = docx(&body);
    let (pkg, mut editor) = open(&original);
    editor.set_caret(Caret::top(1, 3));
    editor.insert_str("XYZ");
    editor.set_caret(Caret::top(1, 0));
    editor.extend_selection(true);
    editor.set_caret(Caret::top(1, 3));
    editor.toggle_bold();
    let saved = save(pkg, editor);
    let xml = document_xml(&saved);
    assert!(
        xml.contains(">sec</w:t>") && xml.contains(">XYZond</w:t>"),
        "{xml}"
    );
    let first = "/w:document/w:body/w:p[1]";
    let outside: Vec<_> = losses(&original, &saved)
        .into_iter()
        .filter(|f| f.path.starts_with(first))
        .collect();
    assert!(outside.is_empty(), "{outside:#?}\n{xml}");
    // The edited paragraph keeps its own attributes too.
    assert!(
        xml.contains("<w:p w:rsidR=\"00CCCCCC\" w14:paraId=\"5E6F7A8B\">"),
        "{xml}"
    );
}

#[test]
fn toggling_bold_keeps_the_runs_other_properties() {
    let xml = edited(
        "<w:p><w:r w:rsidR=\"00AA0001\"><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Gill Sans\"/>\
         <w:i/><w:szCs w:val=\"30\"/><w:lang w:val=\"fr-FR\"/></w:rPr><w:t>text</w:t></w:r></w:p>",
        Editor::toggle_bold,
    );
    let rpr = rpr_of(&xml, "text");
    assert_eq!(rpr.matches("<w:b/>").count(), 1, "{rpr}");
    for kept in [
        "<w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Gill Sans\"/>",
        "<w:i/>",
        "<w:szCs w:val=\"30\"/>",
        "<w:lang w:val=\"fr-FR\"/>",
    ] {
        assert!(rpr.contains(kept), "{kept} lost: {rpr}");
    }
    assert!(xml.contains("<w:r w:rsidR=\"00AA0001\">"), "{xml}");
}

/// A toggle's complex-script twin used to stay behind when the toggle was
/// turned off, so the next load turned it back on.
#[test]
fn turning_off_a_toggle_also_removes_its_complex_script_twin() {
    let body = "<w:p><w:r><w:rPr><w:b/><w:bCs/><w:i/><w:iCs/><w:dstrike/></w:rPr>\
                <w:t>text</w:t></w:r></w:p>";
    let xml = edited(body, |e| {
        e.toggle_bold();
        e.toggle_italic();
        e.toggle_strike();
    });
    let rpr = rpr_of(&xml, "text");
    for gone in ["w:b", "w:bCs", "w:i", "w:iCs", "w:dstrike", "w:strike"] {
        assert!(!rpr.contains(&format!("<{gone}")), "{gone} stayed: {rpr}");
    }
    let (_, reloaded) = open(&docx(
        &xml[xml.find("<w:body>").unwrap() + 8..xml.find("</w:body>").unwrap()],
    ));
    let Block::Paragraph(p) = &reloaded.doc.body[0] else {
        panic!("paragraph");
    };
    let Inline::Run(r) = &p.content[0] else {
        panic!("run");
    };
    assert!(!r.props.bold && !r.props.italic && !r.props.strike, "{rpr}");
}

#[test]
fn turning_on_a_toggle_writes_its_twin_when_the_run_had_one() {
    let xml = edited(
        "<w:p><w:r><w:rPr><w:b w:val=\"0\"/><w:bCs w:val=\"0\"/></w:rPr><w:t>text</w:t></w:r></w:p>",
        Editor::toggle_bold,
    );
    let rpr = rpr_of(&xml, "text");
    assert_eq!(rpr, "<w:rPr><w:b/><w:bCs/></w:rPr>", "{xml}");
}

#[test]
fn a_size_edit_rewrites_the_complex_script_size_with_it() {
    let xml = edited(
        "<w:p><w:r><w:rPr><w:sz w:val=\"20\"/><w:szCs w:val=\"30\"/></w:rPr><w:t>text</w:t></w:r></w:p>",
        |e| e.set_font_size(40),
    );
    assert_eq!(
        rpr_of(&xml, "text"),
        "<w:rPr><w:sz w:val=\"40\"/><w:szCs w:val=\"40\"/></w:rPr>",
        "{xml}"
    );
}

#[test]
fn a_color_edit_drops_the_theme_color_that_would_override_it() {
    let xml = edited(
        "<w:p><w:r><w:rPr><w:color w:val=\"1F3864\" w:themeColor=\"accent1\" w:themeShade=\"80\" \
         w:themeTint=\"99\"/></w:rPr><w:t>text</w:t></w:r></w:p>",
        |e| e.set_color(Some("FF0000".to_string())),
    );
    assert_eq!(
        rpr_of(&xml, "text"),
        "<w:rPr><w:color w:val=\"FF0000\"/></w:rPr>",
        "{xml}"
    );
}

#[test]
fn a_font_edit_replaces_the_latin_slots_and_keeps_the_others() {
    let xml = edited(
        "<w:p><w:r><w:rPr><w:rFonts w:asciiTheme=\"minorHAnsi\" w:hAnsiTheme=\"minorHAnsi\" \
         w:eastAsia=\"MS Mincho\" w:cs=\"Arial\" w:cstheme=\"minorBidi\" w:hint=\"eastAsia\"/>\
         </w:rPr><w:t>text</w:t></w:r></w:p>",
        |e| e.set_font("Courier New"),
    );
    assert_eq!(
        rpr_of(&xml, "text"),
        "<w:rPr><w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\" \
         w:eastAsia=\"MS Mincho\" w:cs=\"Arial\" w:cstheme=\"minorBidi\" w:hint=\"eastAsia\"/></w:rPr>",
        "{xml}"
    );
}

#[test]
fn paragraph_edits_keep_what_they_do_not_change() {
    // Alignment: every other pPr child stays as loaded.
    let xml = edited(RICH_PARAGRAPH, |e| e.set_align(Align::Center));
    let ppr = &xml[xml.find("<w:pPr>").unwrap()..xml.find("</w:pPr>").unwrap()];
    for kept in [
        "<w:pStyle w:val=\"Body\"/>",
        "<w:ins w:id=\"9\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\"/></w:numPr>",
        "<w:left w:val=\"double\" w:sz=\"4\" w:space=\"4\" w:color=\"auto\"/>",
        "<w:top w:val=\"single\" w:sz=\"12\" w:space=\"4\" w:color=\"FF0000\"/>",
        "<w:tab w:val=\"clear\" w:pos=\"720\"/><w:tab w:val=\"decimal\" w:pos=\"4320\"/>",
        "<w:ind w:leftChars=\"200\" w:left=\"420\" w:firstLineChars=\"100\" w:firstLine=\"210\"/>",
        "<w:jc w:val=\"center\"/>",
    ] {
        assert!(ppr.contains(kept), "{kept} lost: {ppr}");
    }
    assert_eq!(ppr.matches("<w:jc ").count(), 1, "{ppr}");

    // A left-indent edit drops the character-unit left indent (it would
    // override the new twips) and keeps the first-line indent as loaded.
    let xml = edited(RICH_PARAGRAPH, |e| e.set_indent(720, 210));
    let ppr = &xml[xml.find("<w:pPr>").unwrap()..xml.find("</w:pPr>").unwrap()];
    assert!(
        ppr.contains("<w:ind w:firstLineChars=\"100\" w:firstLine=\"210\" w:left=\"720\"/>"),
        "{ppr}"
    );
}

fn para_ids(xml: &str) -> Vec<&str> {
    xml.match_indices("w14:paraId=\"")
        .map(|(at, m)| {
            let v = &xml[at + m.len()..];
            &v[..v.find('"').unwrap()]
        })
        .collect()
}

#[test]
fn a_split_paragraph_keeps_its_rsids_and_its_para_id_once() {
    let (pkg, mut editor) = open(&docx(
        "<w:p w:rsidR=\"00AA0001\" w14:paraId=\"1A2B3C4D\"><w:r w:rsidR=\"00AA0001\">\
         <w:t>HelloWorld</w:t></w:r></w:p>",
    ));
    editor.set_caret(Caret::top(0, 5));
    editor.insert_newline();
    let xml = document_xml(&save(pkg, editor));
    assert_eq!(xml.matches("<w:p w:rsidR=\"00AA0001\"").count(), 2, "{xml}");
    assert_eq!(para_ids(&xml), ["1A2B3C4D"], "{xml}");
}

/// A new row is a copy of its template row, start-tag attributes and all
/// (cell paragraphs included); the copies' `w14:paraId`s are dropped.
#[test]
fn an_inserted_table_row_does_not_repeat_para_ids() {
    let (pkg, mut editor) = open(&docx(
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr w:rsidR=\"00AA0001\" w14:paraId=\"1A2B3C4D\"><w:tc>\
         <w:p w:rsidR=\"00AA0001\" w14:paraId=\"2B3C4D5E\"><w:r><w:t>cell</w:t></w:r></w:p>\
         </w:tc></w:tr></w:tbl><w:p/>",
    ));
    editor.set_caret(Caret::at(vec![0, 0, 0, 0], 0));
    editor.select_row().expect("row selected");
    editor.insert_rows(false).expect("row inserted");
    editor.insert_rows(false).expect("row inserted");
    let xml = document_xml(&save(pkg, editor));
    assert_eq!(xml.matches("<w:tr ").count(), 3, "{xml}");
    assert_eq!(
        xml.matches("<w:tr w:rsidR=\"00AA0001\"").count(),
        3,
        "{xml}"
    );
    assert_eq!(para_ids(&xml), ["1A2B3C4D", "2B3C4D5E"], "{xml}");
}

/// Start-tag attributes are bookkeeping, never formatting: runs that differ
/// only in their rsids are the same formatting.
#[test]
fn rsids_do_not_make_runs_different_formatting() {
    let (_, editor) = open(&docx(
        "<w:p><w:r w:rsidR=\"00AA0001\"><w:rPr><w:b/></w:rPr><w:t>ab</w:t></w:r>\
         <w:r w:rsidR=\"00BB0002\" w:rsidRPr=\"00CC0003\"><w:rPr><w:b/></w:rPr><w:t>cd</w:t></w:r></w:p>",
    ));
    let Block::Paragraph(p) = &editor.doc.body[0] else {
        panic!("paragraph");
    };
    let (Inline::Run(a), Inline::Run(b)) = (&p.content[0], &p.content[1]) else {
        panic!("two runs");
    };
    assert_ne!(a.props.element_attrs.0, b.props.element_attrs.0);
    assert_eq!(a.props, b.props);
}

/// A save writes property children in schema order; the shadow of source in
/// another order is kept in that order too, so a saved and reloaded document
/// equals the one loaded.
#[test]
fn out_of_order_properties_reload_to_the_same_document() {
    let original = docx(
        "<w:p><w:pPr><w:bidi/><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr>\
         <w:r><w:rPr><w:szCs w:val=\"24\"/><w:b/></w:rPr><w:t>text</w:t></w:r></w:p>",
    );
    let (pkg, editor) = open(&original);
    let loaded = editor.doc.clone();
    let (_, reloaded) = open(&save(pkg, editor));
    assert_eq!(reloaded.doc, loaded);
}

/// Review r1: an attribute in a prefix declared below the root (on `w:body`,
/// `w:tc`) brings its declaration along; `w:body` and `w:tc` are rebuilt on
/// save without theirs, which would leave the prefix unbound.
#[test]
fn attributes_keep_namespaces_declared_below_the_root() {
    let original = docx_with_root(
        &format!("<w:document xmlns:w=\"{W_NS}\">"),
        &format!(
            "<w:body xmlns:w14=\"{W14_NS}\"><w:p w14:paraId=\"1A2B3C4D\"><w:r><w:t>body</w:t></w:r></w:p>\
             <w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc xmlns:ux=\"urn:ux\">\
             <w:p ux:mark=\"1\"><w:r ux:mark=\"2\"><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
             <w:p/></w:body>"
        ),
    );
    let (pkg, editor) = open(&original);
    let xml = document_xml(&save(pkg, editor));
    assert!(
        xml.contains(&format!(
            "<w:p w14:paraId=\"1A2B3C4D\" xmlns:w14=\"{W14_NS}\">"
        )),
        "{xml}"
    );
    assert!(
        xml.contains("<w:p ux:mark=\"1\" xmlns:ux=\"urn:ux\">")
            && xml.contains("<w:r ux:mark=\"2\" xmlns:ux=\"urn:ux\">"),
        "{xml}"
    );
}

/// Review r1: a header or footer is written through `blocks_to_xml`, which
/// keeps its paragraph ids unique as the document body does.
#[test]
fn a_split_header_paragraph_keeps_its_para_id_once() {
    let header = format!(
        "<w:hdr xmlns:w=\"{W_NS}\" xmlns:w14=\"{W14_NS}\">\
         <w:p w:rsidR=\"00AA0001\" w14:paraId=\"1A2B3C4D\"><w:r><w:t>HeaderText</w:t></w:r></w:p></w:hdr>"
    );
    let blocks = docxcore::load::parse_header_footer(&header, &Default::default());
    let mut editor = Editor::new(docxcore::model::Document { body: blocks });
    editor.set_caret(Caret::top(0, 6));
    editor.insert_newline();
    let xml = docxcore::serialize::blocks_to_xml(&editor.doc.body);
    assert_eq!(xml.matches("<w:p w:rsidR=\"00AA0001\"").count(), 2, "{xml}");
    assert_eq!(para_ids(&xml), ["1A2B3C4D"], "{xml}");
}

/// Review r1: the shadow is kept in schema order, so a twin before its
/// primary (`w:szCs` before `w:sz`, `w:bCs` before `w:b`) must parse the same
/// either way; otherwise an untouched size reads as edited and both are
/// rewritten to one value.
#[test]
fn twins_before_their_primary_survive_an_untouched_save() {
    let body = "<w:p><w:r><w:rPr><w:bCs/><w:b w:val=\"0\"/><w:iCs w:val=\"0\"/><w:i/>\
                <w:szCs w:val=\"30\"/><w:sz w:val=\"20\"/></w:rPr><w:t>text</w:t></w:r></w:p>";
    let (original, saved) = round_trip(body);
    let rpr = rpr_of(&document_xml(&saved), "text").to_string();
    for kept in [
        "<w:b w:val=\"0\"/>",
        "<w:bCs/>",
        "<w:i/>",
        "<w:iCs w:val=\"0\"/>",
        "<w:sz w:val=\"20\"/>",
        "<w:szCs w:val=\"30\"/>",
    ] {
        assert!(rpr.contains(kept), "{kept} lost: {rpr}");
    }
    // Reordered into schema order, the values are the same.
    let (_, loaded) = open(&original);
    let (_, reloaded) = open(&saved);
    let Block::Paragraph(p) = &loaded.doc.body[0] else {
        panic!("paragraph");
    };
    let Inline::Run(r) = &p.content[0] else {
        panic!("run");
    };
    assert!(!r.props.bold && r.props.italic, "{rpr}");
    assert_eq!(r.props.size_half_pts, Some(20));
    assert_eq!(reloaded.doc, loaded.doc);
}

/// Review r1: picking the colour or font a run already has, while a theme
/// value overrides it, makes the pick take effect.
#[test]
fn picking_the_loaded_color_or_font_drops_the_theme_that_overrides_it() {
    let xml = edited(
        "<w:p><w:r><w:rPr><w:rFonts w:ascii=\"Arial\" w:asciiTheme=\"minorHAnsi\" w:cs=\"Arial\"/>\
         <w:color w:val=\"FF0000\" w:themeColor=\"accent1\"/></w:rPr><w:t>text</w:t></w:r></w:p>",
        |e| {
            e.set_color(Some("FF0000".to_string()));
            e.set_font("Arial");
        },
    );
    assert_eq!(
        rpr_of(&xml, "text"),
        "<w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/>\
         <w:color w:val=\"FF0000\"/></w:rPr>",
        "{xml}"
    );
}

/// Review r1: a font edit fills the high-ANSI slot too, also on a run with no
/// direct `w:rFonts`.
#[test]
fn a_new_font_fills_the_ascii_and_high_ansi_slots() {
    let xml = edited("<w:p><w:r><w:t>text</w:t></w:r></w:p>", |e| {
        e.set_font("Courier New")
    });
    assert_eq!(
        rpr_of(&xml, "text"),
        "<w:rPr><w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\"/></w:rPr>",
        "{xml}"
    );
}

/// Review r1: rejecting a tracked property change restores the properties,
/// not the start-tag attributes, which are not properties.
#[test]
fn rejecting_a_property_change_keeps_rsids_and_para_id() {
    let date = "2026-01-01T00:00:00Z";
    let (pkg, mut editor) = open(&docx(&format!(
        "<w:p w:rsidR=\"00AA0001\" w14:paraId=\"1A2B3C4D\"><w:pPr><w:jc w:val=\"center\"/>\
         <w:pPrChange w:id=\"1\" w:author=\"A\" w:date=\"{date}\"><w:pPr/></w:pPrChange></w:pPr>\
         <w:r w:rsidR=\"00AA0002\"><w:rPr><w:b/>\
         <w:rPrChange w:id=\"2\" w:author=\"A\" w:date=\"{date}\"><w:rPr/></w:rPrChange></w:rPr>\
         <w:t>text</w:t></w:r></w:p>"
    )));
    let outcomes = editor.reject_all_revisions();
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    let xml = document_xml(&save(pkg, editor));
    assert!(!xml.contains("w:jc") && !xml.contains("<w:b/>"), "{xml}");
    assert!(
        xml.contains("<w:p w:rsidR=\"00AA0001\" w14:paraId=\"1A2B3C4D\">")
            && xml.contains("<w:r w:rsidR=\"00AA0002\">"),
        "{xml}"
    );
}

/// Review r1: tabs and line ends in a kept attribute value are written as
/// character references, so they are not normalized to spaces on reload.
#[test]
fn whitespace_references_in_kept_attributes_survive() {
    let original = docx_with_root(
        &format!("<w:document xmlns:w=\"{W_NS}\" xmlns:ux=\"urn:ux\">"),
        "<w:body><w:p ux:label=\"A&#10;B&#9;C&#13;D\"><w:r><w:t>text</w:t></w:r></w:p></w:body>",
    );
    let (pkg, editor) = open(&original);
    let xml = document_xml(&save(pkg, editor));
    assert!(
        xml.contains("<w:p ux:label=\"A&#10;B&#9;C&#13;D\">"),
        "{xml}"
    );
}
