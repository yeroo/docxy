//! Bold and Italic over the styles a run is drawn with (#1427): turning one
//! off in text a table, paragraph or character style makes bold writes the
//! explicit `<w:b w:val="0"/>` Word writes, and turning it on again goes
//! back to inheriting.

use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline, RunProps};
use docxcore::serialize::document_to_xml;
use std::sync::Arc;

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="table" w:styleId="Grid"><w:name w:val="Grid"/>
  <w:tblStylePr w:type="firstRow"><w:rPr><w:b/></w:rPr></w:tblStylePr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/>
  <w:rPr><w:b/><w:i/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/>
  <w:rPr><w:b/></w:rPr></w:style>
</w:styles>"#;

const TABLE: &str = "<w:tbl><w:tblPr><w:tblStyle w:val=\"Grid\"/>\
    <w:tblLook w:val=\"0420\" w:firstRow=\"1\" w:lastRow=\"0\" w:firstColumn=\"0\" \
    w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/></w:tblPr>\
    <w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
    <w:tr><w:tc><w:p><w:r><w:t>Head</w:t></w:r></w:p></w:tc></w:tr>\
    <w:tr><w:tc><w:p><w:r><w:t>Body</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";

fn parse(inner: &str) -> Document {
    parse_document_xml(
        &format!("<w:document {W}><w:body>{inner}</w:body></w:document>"),
        &Relationships::default(),
    )
}

fn editor(inner: &str, styled: bool) -> Editor {
    let mut ed = Editor::new(parse(inner));
    if styled {
        ed.set_styles_xml(Some(Arc::from(STYLES)));
    }
    ed
}

fn select(ed: &mut Editor, path: &[usize], from: usize, to: usize) {
    ed.anchor = Some(Caret::at(path.to_vec(), from));
    ed.caret = Caret::at(path.to_vec(), to);
}

fn xml(ed: &Editor) -> String {
    document_to_xml(&ed.doc)
}

/// The header cell's paragraph.
const HEAD: [usize; 4] = [0, 0, 0, 0];
const BODY: [usize; 4] = [0, 1, 0, 0];

fn run_props(ed: &Editor, path: &[usize], ch: char) -> RunProps {
    let mut p = &ed.doc.body[path[0]];
    let mut rest = &path[1..];
    while let Block::Table(t) = p {
        let cell = &t.rows[rest[0]].cells[rest[1]];
        p = &cell.blocks[rest[2]];
        rest = &rest[3..];
    }
    let Block::Paragraph(p) = p else {
        panic!("not a paragraph")
    };
    p.content
        .iter()
        .find_map(|i| match i {
            Inline::Run(r) if r.text.contains(ch) => Some(r.props.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no run holds {ch:?}"))
}

#[test]
fn bold_off_in_table_style_bold_cell_writes_b_val_0() {
    let mut ed = editor(TABLE, true);
    select(&mut ed, &HEAD, 0, 4);
    ed.toggle_bold();
    let out = xml(&ed);
    assert!(out.contains("<w:b w:val=\"0\"/>"), "{out}");
    assert_eq!(out.matches("<w:b ").count(), 1, "{out}");
    // Reloaded, the run shows not bold over the style.
    let inner = out
        .split("<w:body>")
        .nth(1)
        .unwrap()
        .split("</w:body>")
        .next()
        .unwrap();
    let mut back = Editor::new(parse(inner));
    back.set_styles_xml(Some(Arc::from(STYLES)));
    back.caret = Caret::at(HEAD.to_vec(), 1);
    assert!(!back.caret_props_shown().bold);
    // The body row is untouched, and one undo restores the header.
    assert!(!run_props(&ed, &BODY, 'B').bold);
    ed.undo();
    assert!(!xml(&ed).contains("w:val=\"0\""), "{}", xml(&ed));
    assert!(ed.caret_props_shown().bold);
    ed.redo();
    assert!(xml(&ed).contains("<w:b w:val=\"0\"/>"));
}

#[test]
fn bold_on_again_drops_explicit_off() {
    let mut ed = editor(TABLE, true);
    select(&mut ed, &HEAD, 0, 4);
    ed.toggle_bold();
    ed.toggle_bold();
    let out = xml(&ed);
    assert!(
        !out.contains("<w:b/>") && !out.contains("<w:b "),
        "inherits again: {out}"
    );
    let p = run_props(&ed, &HEAD, 'H');
    assert!(!p.bold && p.raw_props.is_empty(), "{p:?}");
}

#[test]
fn italic_off_from_paragraph_style_writes_i_val_0() {
    let mut ed = editor(
        "<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>",
        true,
    );
    select(&mut ed, &[0], 0, 5);
    ed.toggle_italic();
    let out = xml(&ed);
    assert!(out.contains("<w:i w:val=\"0\"/>"), "{out}");
    assert!(
        !out.contains("<w:b/>") && !out.contains("<w:b "),
        "bold is left to the style: {out}"
    );
    ed.toggle_italic();
    assert!(
        !xml(&ed).contains("<w:i/>") && !xml(&ed).contains("<w:i "),
        "{}",
        xml(&ed)
    );
}

#[test]
fn bold_off_mixed_strong_and_plain_runs() {
    let mut ed = editor(
        "<w:p><w:r><w:rPr><w:rStyle w:val=\"Strong\"/></w:rPr><w:t>aa</w:t></w:r>\
         <w:r><w:t>bb</w:t></w:r></w:p>",
        true,
    );
    select(&mut ed, &[0], 0, 4);
    // Not every run is bold, so the first toggle turns it on.
    ed.toggle_bold();
    assert!(run_props(&ed, &[0], 'b').bold);
    assert!(!run_props(&ed, &[0], 'a').bold, "Strong already gives it");
    assert!(run_props(&ed, &[0], 'a').raw_props.is_empty());
    // Now all are bold: off writes the marker on the Strong run only.
    ed.toggle_bold();
    assert!(!run_props(&ed, &[0], 'b').bold);
    assert!(run_props(&ed, &[0], 'b').raw_props.is_empty());
    let a = run_props(&ed, &[0], 'a');
    assert!(
        !a.bold && a.raw_props.iter().any(|r| r.contains("w:val=\"0\"")),
        "{a:?}"
    );
}

#[test]
fn bold_off_at_caret_in_table_bold_cell_types_b_val_0() {
    let mut ed = editor(TABLE, true);
    ed.caret = Caret::at(HEAD.to_vec(), 4);
    assert!(ed.caret_props_shown().bold);
    ed.toggle_bold();
    assert!(!ed.caret_props_shown().bold, "the ribbon shows it off");
    ed.insert_str("x");
    let x = run_props(&ed, &HEAD, 'x');
    assert!(
        !x.bold && x.raw_props.iter().any(|r| r.contains("w:val=\"0\"")),
        "{x:?}"
    );
    let out = xml(&ed);
    assert!(out.contains("<w:b w:val=\"0\"/>"), "{out}");
    // Toggling twice leaves nothing pending: typed text inherits.
    let mut ed = editor(TABLE, true);
    ed.caret = Caret::at(HEAD.to_vec(), 4);
    ed.toggle_bold();
    ed.toggle_bold();
    ed.insert_str("y");
    assert!(run_props(&ed, &HEAD, 'y').raw_props.is_empty());
    assert!(ed.caret_props_shown().bold);
}

#[test]
fn toggle_bold_without_styles_context_unchanged() {
    let mut ed = editor(TABLE, false);
    select(&mut ed, &HEAD, 0, 4);
    ed.toggle_bold();
    let p = run_props(&ed, &HEAD, 'H');
    assert!(p.bold && p.raw_props.is_empty(), "{p:?}");
    assert!(!xml(&ed).contains("val=\"0\""));
    ed.toggle_bold();
    assert!(!run_props(&ed, &HEAD, 'H').bold);
}

#[test]
fn caret_props_shown_reflects_table_bold() {
    let mut ed = editor(TABLE, true);
    ed.caret = Caret::at(HEAD.to_vec(), 2);
    assert!(ed.caret_props_shown().bold);
    assert!(!ed.caret_props().bold, "caret_props stays direct");
    ed.caret = Caret::at(BODY.to_vec(), 2);
    assert!(!ed.caret_props_shown().bold);
    let mut plain = editor(TABLE, false);
    plain.caret = Caret::at(HEAD.to_vec(), 2);
    assert!(!plain.caret_props_shown().bold);
}

#[test]
fn direct_bold_in_bold_style_round_trips_off_and_on() {
    let body = "<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>\
                <w:r><w:rPr><w:b/></w:rPr><w:t>Title</w:t></w:r></w:p>";
    let mut ed = editor(body, true);
    select(&mut ed, &[0], 0, 5);
    ed.toggle_bold();
    let out = xml(&ed);
    assert!(out.contains("<w:b w:val=\"0\"/>"), "{out}");
    assert_eq!(
        out.matches("<w:b ").count() + out.matches("<w:b/>").count(),
        1,
        "{out}"
    );
    let inner = out
        .split("<w:body>")
        .nth(1)
        .unwrap()
        .split("</w:body>")
        .next()
        .unwrap();
    let mut back = Editor::new(parse(inner));
    back.set_styles_xml(Some(Arc::from(STYLES)));
    select(&mut back, &[0], 0, 5);
    assert!(!back.caret_props_shown().bold);
    back.toggle_bold();
    let out = xml(&back);
    assert!(
        !out.contains("<w:b/>") && !out.contains("<w:b "),
        "inherits the style's bold again: {out}"
    );
}
