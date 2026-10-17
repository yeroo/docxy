//! The Header & Footer tab's Insert group and Insert Alignment Tab (#768)
//! over the PAG-CASE-019 shape (see `hf::tests`): each command inserts at
//! the caret of the open header, and what it inserts is in that header's
//! part after a save and a reload.

use super::*;
use crate::hf::tests::{caret_in, ed, saved, three_sections, three_sections_docx, tmp_dir};
use crate::hf_tab::{HfMenu, hf_apply, hf_tab, menu_items};
use core::prelude::v1::test;
use docxcore::model::TabAlign;
use docxcore::package::load_package;
use docxcore::ptab::PtabLeader;

/// Section 1's header open, the caret at its start.
fn open_header(t: &mut DocTab) {
    caret_in(t, 0);
    hf_apply(t, HfAct::Edit(true)).unwrap();
    assert!(t.hf_edit.is_some(), "{}", t.status);
}

fn header_tab(name: &str) -> DocTab {
    let mut t = three_sections(name, true);
    open_header(&mut t);
    t
}

fn hf_ed(t: &mut DocTab) -> &mut Editor {
    &mut t.hf_edit.as_mut().unwrap().editor
}

fn part_name(t: &DocTab) -> String {
    t.hf_edit.as_ref().unwrap().part_name.clone()
}

/// What Save writes, reloaded, and the edited header part's XML there.
fn saved_header(t: &mut DocTab) -> (Package, String) {
    let part = part_name(t);
    let pkg = saved(t);
    let xml = pkg.part_text(&part).unwrap();
    (pkg, xml)
}

/// June 23, 2026, a Tuesday, at 14:05:09.
fn tuesday() -> DateTime {
    DateTime {
        year: 2026,
        month: 6,
        day: 23,
        hour: 14,
        min: 5,
        sec: 9,
        weekday: 2,
    }
}

fn set_choice(d: &mut Dialog, name: &str, i: usize) {
    d.controls
        .iter_mut()
        .find(|c| c.name == name)
        .unwrap()
        .value = Value::Choice(Some(i));
}

/// Criterion 1: the Insert group in Word's order, Insert Alignment Tab after
/// Footer from Bottom, and the two menus.
#[test]
fn the_insert_group_and_position_group_follow_word() {
    let tab = hf_tab();
    let group = |title: &str| tab.groups.iter().find(|g| g.title == title).unwrap();
    let ids: Vec<&str> = group("Insert")
        .items
        .iter()
        .filter_map(|c| match c {
            rs::Control::Large(c) => Some(c.id),
            rs::Control::Dropdown { cmd, .. } => Some(cmd.id),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        [
            "hf-datetime",
            "hf-docinfo",
            "hf-quickparts",
            "hf-pictures",
            "hf-online-pictures"
        ]
    );
    let rs::Control::Rows(rows) = &group("Position").items[0] else {
        panic!("the Position group is rows");
    };
    let ids: Vec<&str> = rows
        .iter()
        .flatten()
        .map(|c| match c {
            rs::Cell::Btn(c) | rs::Cell::Combo { cmd: c, .. } => c.id,
        })
        .collect();
    assert_eq!(ids, ["hf-top", "hf-bottom", "hf-align-tab"]);

    let labels = |items: &[menu::MenuItem]| -> Vec<(String, bool, usize)> {
        items
            .iter()
            .filter_map(|i| match i {
                menu::MenuItem::Item(e) => Some((e.label.clone(), e.enabled, e.submenu.len())),
                _ => None,
            })
            .collect()
    };
    let info = menu_items(None, HfMenu::DocumentInfo);
    let item = |l: &str, on: bool, sub: usize| (l.to_string(), on, sub);
    assert_eq!(
        labels(&info),
        [
            item("Author", true, 0),
            item("File Name", true, 0),
            item("File Path", true, 0),
            item("Document Title", true, 0),
            item("Document Property", true, 7),
            item("Field...", true, 0),
        ]
    );
    // Quick Parts: Word's items, those docxy has no building blocks for
    // drawn disabled.
    let quick = menu_items(None, HfMenu::QuickParts);
    assert_eq!(
        labels(&quick),
        [
            item("AutoText", false, 0),
            item("Document Property", true, 7),
            item("Field...", true, 0),
            item("Building Blocks Organizer...", false, 0),
            item("Save Selection to Quick Part Gallery...", false, 0),
        ]
    );
    let menu::MenuItem::Item(props) = &quick[1] else {
        panic!()
    };
    let names: Vec<&str> = props
        .submenu
        .iter()
        .filter_map(|i| match i {
            menu::MenuItem::Item(e) => Some(e.label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        [
            "Author", "Category", "Comments", "Keywords", "Status", "Subject", "Title"
        ]
    );
}

/// The local clock: UTC shifted by the zone's offset, across midnight.
#[test]
fn the_local_clock_is_utc_shifted_by_the_offset() {
    let t = now_local(0, 3 * 3600);
    assert_eq!((t.year, t.month, t.day, t.hour), (1970, 1, 1, 3));
    let t = now_local(86_399, 3600);
    assert_eq!((t.day, t.hour, t.min, t.sec), (2, 0, 59, 59));
    let t = now_local(0, -3600);
    assert_eq!((t.year, t.month, t.day, t.hour), (1969, 12, 31, 23));
}

/// Criterion 2: Word's en-US formats at a fixed clock.
#[test]
fn date_and_time_lists_words_formats() {
    assert_eq!(
        date_choices(&tuesday()),
        [
            "6/23/2026",
            "Tuesday, June 23, 2026",
            "June 23, 2026",
            "6/23/26",
            "2026-06-23",
            "23-Jun-26",
            "6.23.2026",
            "Jun. 23, 26",
            "23 June 2026",
            "June 26",
            "Jun-26",
            "6/23/2026 2:05 PM",
            "6/23/2026 2:05:09 PM",
            "2:05 PM",
            "2:05:09 PM",
            "14:05",
            "14:05:09",
        ]
    );
    let d = date_time_dialog(&tuesday());
    assert_eq!(d.value("formats"), Some(&Value::Choice(Some(0))));
    assert_eq!(d.value("update"), Some(&Value::Bool(false)));
}

/// Criterion 2: OK puts the text, or with Update automatically a DATE
/// field showing it, at the header's caret.
#[test]
fn date_and_time_inserts_text_or_a_field_into_the_header() {
    let mut t = header_tab("datetime");
    let mut d = date_time_dialog(&tuesday());
    set_choice(&mut d, "formats", 2);
    t.dialogs.push(d);
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
    let (_, xml) = saved_header(&mut t);
    assert!(xml.contains(">June 23, 2026</w:t>"), "{xml}");
    assert!(!xml.contains("fldSimple"), "{xml}");
    // The body is untouched.
    assert!(!ed(&t).doc.plain_text().contains("June"));

    let mut t = header_tab("datetime-field");
    let mut d = date_time_dialog(&tuesday());
    d.controls
        .iter_mut()
        .find(|c| c.name == "update")
        .unwrap()
        .value = Value::Bool(true);
    t.dialogs.push(d);
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    let (_, xml) = saved_header(&mut t);
    assert!(
        xml.contains("<w:fldSimple w:instr=\"DATE \\@ &quot;M/d/yyyy&quot;\">"),
        "{xml}"
    );
    assert!(xml.contains(">6/23/2026</w:t>"), "{xml}");
    // One Undo takes it out.
    assert!(hf_ed(&mut t).undo());
    assert!(!crate::hf::tests::edited_text(&t).contains("6/23/2026"));
}

/// Criterion 10 and File Path: the context reads core.xml, the title and
/// the path.
#[test]
fn fields_read_the_documents_properties_and_path() {
    let t = three_sections("context", false);
    let ctx = tab_field_context(Some(&t));
    // basic.docx has no core properties: the author stays docxy.
    assert_eq!(ctx.props.author, "docxy");
    assert_eq!(ctx.filename, t.title.to_string());
    assert!(ctx.path.ends_with("in.docx"), "{}", ctx.path);
    assert_eq!(
        docxcore::field::eval_field_ctx("FILENAME \\p", &ctx).as_deref(),
        Some(ctx.path.as_str())
    );
    let t = with_core("context-core", "Ann", "Quarterly Report");
    let ctx = tab_field_context(Some(&t));
    assert_eq!(ctx.props.author, "Ann");
    assert_eq!(
        docxcore::field::eval_field_ctx("TITLE", &ctx).as_deref(),
        Some("Quarterly Report")
    );
}

/// A headed three-section tab whose `docProps/core.xml` names `author` and
/// `title`.
fn with_core(name: &str, author: &str, title: &str) -> DocTab {
    let pkg = load_package(&three_sections_docx(true, true)).unwrap();
    let mut parts: Vec<(String, Vec<u8>)> = pkg
        .part_names()
        .into_iter()
        .map(|n| (n.to_string(), pkg.part(n).unwrap().to_vec()))
        .collect();
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{title}</dc:title>\
         <dc:creator>{author}</dc:creator><cp:category>Memo</cp:category></cp:coreProperties>"
    );
    parts.push(("docProps/core.xml".into(), core.into_bytes()));
    let dir = tmp_dir(name);
    let path = dir.join("in.docx");
    std::fs::write(&path, docxcore::zipwrite::write_zip(&parts)).unwrap();
    let t = tab_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    t
}

/// Criteria 3 and 4: a property control at the header's caret, bound as
/// Word binds it, showing core.xml's value, its binding kept by a save and
/// a reload; an empty property shows the placeholder.
#[test]
fn a_document_property_control_goes_into_the_header_bound() {
    let mut t = with_core("property", "Ann", "Quarterly Report");
    open_header(&mut t);
    let control = property_control(&t, CoreProp::Title);
    hf_ed(&mut t).paste(&Clip {
        paras: vec![control],
    });
    let control = property_control(&t, CoreProp::Status);
    hf_ed(&mut t).paste(&Clip {
        paras: vec![control],
    });
    let (pkg, xml) = saved_header(&mut t);
    assert!(xml.contains("<w:alias w:val=\"Title\"/>"), "{xml}");
    assert!(
        xml.contains(
            "w:xpath=\"/ns1:coreProperties[1]/ns0:title[1]\" w:storeItemID=\"{6C3C8BC8-F283-45AE-878A-BAB7291924A1}\""
        ),
        "{xml}"
    );
    assert!(xml.contains(">Quarterly Report</w:t>"), "{xml}");
    assert!(
        xml.contains("<w:alias w:val=\"Status\"/><w:id w:val=\"2\"/><w:showingPlcHdr/>"),
        "{xml}"
    );
    assert!(xml.contains(">[Status]</w:t>"), "{xml}");
    // A reload keeps both bindings, and the ids are distinct.
    let again = load_package(&docxcore::package::save_package(&pkg)).unwrap();
    let xml = again.part_text(&part_name(&t)).unwrap();
    assert_eq!(xml.matches("<w:dataBinding ").count(), 2, "{xml}");
    assert!(xml.contains("<w:id w:val=\"1\"/>") && xml.contains("<w:id w:val=\"2\"/>"));
}

/// Control ids are unique across the document: a control already in the
/// body takes its id out of the header's choices.
#[test]
fn a_property_controls_id_is_unique_in_the_document() {
    let mut t = header_tab("property-id");
    if let Surface::Doc(ed) = &mut t.surface {
        let body_control =
            docxcore::sdt::core_property_control(CoreProp::Author, "", 41, &Default::default());
        ed.caret = docxcore::editor::Caret::at(vec![0], 0);
        ed.paste(&Clip {
            paras: vec![body_control],
        });
    }
    let control = property_control(&t, CoreProp::Author);
    let Inline::Raw(open) = &control[0] else {
        panic!()
    };
    assert!(open.contains("<w:id w:val=\"42\"/>"), "{open}");
}

/// A small PNG file in `dir`, `w` x `h` px.
fn png_file(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    let path = dir.join(name);
    std::fs::write(&path, b).unwrap();
    path
}

/// Criterion 6: the picture's media part is related from the header's own
/// rels, not the body's; its run is in the header part, sized at 96 dpi;
/// the content types declare it; all of it survives a save and a reload.
#[test]
fn a_picture_goes_into_the_header_part_related_from_its_own_rels() {
    let dir = tmp_dir("picture");
    let png = png_file(&dir, "logo.png", 96, 48);
    let mut t = header_tab("picture");
    let body_rels = t
        .pkg
        .as_ref()
        .unwrap()
        .part_text("word/_rels/document.xml.rels")
        .unwrap();
    let status = insert_picture(&mut t, &png).unwrap();
    assert_eq!(status, "Inserted picture logo.png");
    let part = part_name(&t);
    let (pkg, xml) = saved_header(&mut t);
    assert!(
        xml.contains("<wp:extent cx=\"914400\" cy=\"457200\"/>"),
        "{xml}"
    );
    let rid =
        docxcore::load::xml_attr_value(&xml[xml.find("<a:blip").unwrap()..], "r:embed").unwrap();
    let again = load_package(&docxcore::package::save_package(&pkg)).unwrap();
    let rels_name = part.replace("word/", "word/_rels/") + ".rels";
    let rels = docxcore::load::parse_rels_xml(&again.part_text(&rels_name).unwrap());
    let target = rels.target(&rid).unwrap().to_string();
    assert_eq!(
        again.part(&format!("word/{target}")).unwrap(),
        &std::fs::read(&png).unwrap()[..]
    );
    assert!(
        again
            .part_text("[Content_Types].xml")
            .unwrap()
            .contains("<Default Extension=\"png\" ContentType=\"image/png\"/>")
    );
    assert_eq!(
        again.part_text("word/_rels/document.xml.rels").unwrap(),
        body_rels,
        "the body's rels are untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A .JPG declares image/jpeg; a picture wider than the text is scaled to
/// it; drawing ids stay unique with an unsaved body picture.
#[test]
fn a_wide_jpeg_is_scaled_and_its_id_is_unique() {
    let dir = tmp_dir("jpeg");
    // SOI, SOF0 4000 x 1000.
    let jpg = dir.join("PHOTO.JPG");
    std::fs::write(
        &jpg,
        [0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0x03, 0xe8, 0x0f, 0xa0, 3],
    )
    .unwrap();
    let mut t = header_tab("jpeg");
    // An unsaved picture in the body, drawing id 5.
    if let Surface::Doc(ed) = &mut t.surface {
        ed.caret = docxcore::editor::Caret::at(vec![0], 0);
        let run = docxcore::image::inline_picture_run("rId99", 1, 1, 5, "body.png");
        ed.paste(&Clip {
            paras: vec![vec![Inline::Raw(run)]],
        });
    }
    insert_picture(&mut t, &jpg).unwrap();
    let (pkg, xml) = saved_header(&mut t);
    assert!(
        xml.contains("<wp:docPr id=\"6\" name=\"Picture 6\"/>"),
        "{xml}"
    );
    // basic.docx's text is 6.5" wide: 5943600 EMU, a quarter of that high.
    assert!(
        xml.contains("<wp:extent cx=\"5943600\" cy=\"1485900\"/>"),
        "{xml}"
    );
    assert!(
        pkg.part_text("[Content_Types].xml")
            .unwrap()
            .contains("<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Criterion 6's errors: nothing is inserted, and the status says why.
#[test]
fn a_picture_is_refused_without_a_header_or_a_readable_picture() {
    let dir = tmp_dir("picture-errors");
    let png = png_file(&dir, "logo.png", 10, 10);
    let mut t = three_sections("no-header-open", true);
    assert!(
        insert_picture(&mut t, &png)
            .unwrap_err()
            .contains("header or footer")
    );

    let mut t = header_tab("bad-picture");
    let parts = t.pkg.as_ref().unwrap().part_names().len();
    let text = dir.join("notes.png");
    std::fs::write(&text, "not a picture").unwrap();
    let e = insert_picture(&mut t, &text).unwrap_err();
    assert_eq!(e, "notes.png is not a PNG, JPEG, GIF or BMP picture");
    let e = insert_picture(&mut t, &dir.join("missing.png")).unwrap_err();
    assert!(e.starts_with("Could not read missing.png"), "{e}");
    assert_eq!(t.pkg.as_ref().unwrap().part_names().len(), parts);
    assert!(!t.dirty);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Side effect: a picture in section 1's header survives Link to Previous
/// off in section 2 (the copy brings its rels): both parts resolve it.
#[test]
fn an_unlinked_copy_of_a_header_keeps_its_picture() {
    let dir = tmp_dir("picture-unlink");
    let png = png_file(&dir, "logo.png", 10, 10);
    let mut t = header_tab("picture-unlink");
    insert_picture(&mut t, &png).unwrap();
    let first = part_name(&t);
    exit_hf_tab(&mut t);
    caret_in(&mut t, 1);
    hf_apply(&mut t, HfAct::Edit(true)).unwrap();
    hf_apply(&mut t, HfAct::LinkToPrevious).unwrap();
    let second = part_name(&t);
    assert_ne!(first, second);
    let pkg = saved(&mut t);
    for part in [first, second] {
        let xml = pkg.part_text(&part).unwrap();
        let rid = docxcore::load::xml_attr_value(&xml[xml.find("<a:blip").unwrap()..], "r:embed")
            .unwrap();
        let rels_name = part.replace("word/", "word/_rels/") + ".rels";
        let rels = docxcore::load::parse_rels_xml(&pkg.part_text(&rels_name).unwrap());
        let target = rels.target(&rid).unwrap();
        assert!(
            pkg.part(&format!("word/{target}")).is_some(),
            "{part}: {target}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Criterion 8: each alignment's XML, in the caret's formatting, at the
/// header's caret through OK, kept by a save.
#[test]
fn the_alignment_tab_dialog_inserts_a_ptab() {
    let mut d = alignment_tab_dialog();
    let props = docxcore::model::RunProps::default();
    assert_eq!(
        ptab_xml(&d, &props),
        "<w:r><w:ptab w:relativeTo=\"margin\" w:alignment=\"left\" w:leader=\"none\"/></w:r>"
    );
    set_choice(&mut d, "alignment", 1);
    set_choice(&mut d, "relative", 1);
    set_choice(&mut d, "leader", 1);
    assert!(
        ptab_xml(&d, &props)
            .contains("w:relativeTo=\"indent\" w:alignment=\"center\" w:leader=\"dot\"")
    );
    set_choice(&mut d, "alignment", 2);
    set_choice(&mut d, "leader", 4);
    assert!(ptab_xml(&d, &props).contains("w:alignment=\"right\" w:leader=\"middleDot\""));

    let mut t = header_tab("ptab");
    hf_ed(&mut t).caret.offset = 8;
    t.dialogs.push(d);
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    let (_, xml) = saved_header(&mut t);
    assert!(
        xml.contains(
            "Header A</w:t></w:r><w:r><w:ptab w:relativeTo=\"indent\" w:alignment=\"right\" w:leader=\"middleDot\"/></w:r>"
        ),
        "{xml}"
    );
}

/// FIX r3 M1/M2: text typed right after Insert Alignment Tab, a picture or a
/// property control goes after it, as in Word, through the paths the tab
/// uses; a caret move between them falls back to the usual rule.
#[test]
fn typing_after_an_inserted_object_goes_after_it_in_the_header() {
    let at = |xml: &str, s: &str| xml.find(s).unwrap_or_else(|| panic!("{s} in {xml}"));
    // `Header A`, Insert Alignment Tab (Right), `Page 1`.
    let mut t = header_tab("type-after-ptab");
    hf_ed(&mut t).caret.offset = 8;
    let mut d = alignment_tab_dialog();
    set_choice(&mut d, "alignment", 2);
    t.dialogs.push(d);
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    hf_ed(&mut t).insert_str("Page 1");
    let (_, xml) = saved_header(&mut t);
    assert!(at(&xml, ">Header A</w:t>") < at(&xml, "<w:ptab "));
    assert!(at(&xml, "<w:ptab ") < at(&xml, ">Page 1</w:t>"), "{xml}");

    // A picture, then `x`.
    let dir = tmp_dir("type-after-picture");
    let png = png_file(&dir, "logo.png", 10, 10);
    let mut t = header_tab("type-after-picture");
    hf_ed(&mut t).caret.offset = 8;
    insert_picture(&mut t, &png).unwrap();
    hf_ed(&mut t).insert_char('x');
    let (_, xml) = saved_header(&mut t);
    assert!(at(&xml, ">Header A</w:t>") < at(&xml, "<w:drawing>"));
    assert!(at(&xml, "</w:drawing>") < at(&xml, ">x</w:t>"), "{xml}");
    let _ = std::fs::remove_dir_all(&dir);

    // The Title control, then ` - draft`: after its close, its content as
    // core.xml has it.
    let mut t = with_core("type-after-control", "Ann", "Quarterly Report");
    open_header(&mut t);
    hf_ed(&mut t).caret.offset = 8;
    let control = property_control(&t, CoreProp::Title);
    hf_ed(&mut t).insert_object(control);
    hf_ed(&mut t).insert_str(" - draft");
    let (_, xml) = saved_header(&mut t);
    assert!(
        xml.contains(">Quarterly Report</w:t></w:r></w:sdtContent></w:sdt>"),
        "{xml}"
    );
    assert!(at(&xml, "</w:sdt>") < at(&xml, "> - draft</w:t>"), "{xml}");

    // A caret move in between: the text goes where the caret went.
    let mut t = header_tab("type-after-move");
    hf_ed(&mut t).caret.offset = 8;
    t.dialogs.push(alignment_tab_dialog());
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    hf_ed(&mut t).caret.offset = 0;
    hf_ed(&mut t).insert_str("My ");
    let (_, xml) = saved_header(&mut t);
    assert!(xml.contains(">My Header A</w:t>"), "{xml}");
}

/// FIX r6 m1: an insert over a selection is one undo step; text typed right
/// after an Alignment Tab that replaced a selection still goes after it.
#[test]
fn an_insert_over_a_selection_is_one_undo_step() {
    // Select `Header`, Date & Time OK: one Undo restores `Header A`.
    let mut t = header_tab("one-step-date");
    let ed = hf_ed(&mut t);
    ed.anchor = Some(docxcore::editor::Caret::at(vec![0], 0));
    ed.caret = docxcore::editor::Caret::at(vec![0], 6);
    let mut d = date_time_dialog(&tuesday());
    set_choice(&mut d, "formats", 4);
    t.dialogs.push(d);
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    assert_eq!(crate::hf::tests::edited_text(&t), "2026-06-23 A");
    assert!(hf_ed(&mut t).undo());
    assert_eq!(crate::hf::tests::edited_text(&t), "Header A");

    // Select ` A`, Insert Alignment Tab, type: after the tab; one Undo for
    // the typing, one for the tab and the replaced text.
    let mut t = header_tab("one-step-ptab");
    let ed = hf_ed(&mut t);
    ed.anchor = Some(docxcore::editor::Caret::at(vec![0], 6));
    ed.caret = docxcore::editor::Caret::at(vec![0], 8);
    t.dialogs.push(alignment_tab_dialog());
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    hf_ed(&mut t).insert_str("Page 1");
    let ed = hf_ed(&mut t);
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("a paragraph");
    };
    let shape: Vec<String> = p
        .content
        .iter()
        .map(|i| match i {
            Inline::Run(r) => r.text.clone(),
            Inline::Raw(r) if r.contains("<w:ptab") => "[PTAB]".into(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(shape, ["Header", "[PTAB]", "Page 1"]);
    assert!(ed.undo());
    assert!(ed.undo());
    assert_eq!(crate::hf::tests::edited_text(&t), "Header A");

    // A picture over a selection: one step too.
    let dir = tmp_dir("one-step-picture");
    let png = png_file(&dir, "logo.png", 10, 10);
    let mut t = header_tab("one-step-picture");
    let ed = hf_ed(&mut t);
    ed.anchor = Some(docxcore::editor::Caret::at(vec![0], 0));
    ed.caret = docxcore::editor::Caret::at(vec![0], 6);
    insert_picture(&mut t, &png).unwrap();
    assert_eq!(crate::hf::tests::edited_text(&t), " A");
    assert!(hf_ed(&mut t).undo());
    assert_eq!(crate::hf::tests::edited_text(&t), "Header A");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Criterion 8's layout: the segment after a right alignment tab ends at
/// the right margin; a centre one centres it; a passed stop adds nothing.
#[test]
fn an_alignment_tab_places_the_text_after_it() {
    let right = crate::Ptab {
        align: TabAlign::Right,
        indent: false,
        leader: PtabLeader::None,
    };
    let stop = crate::ptab_stop(right, 600.0, 30.0, 20.0);
    let gap = crate::ptab_gap(TabAlign::Right, stop, 100.0, 50.0);
    assert_eq!(100.0 + gap + 50.0, 596.0, "ends at the margin, 4 px in");
    let center = crate::Ptab {
        align: TabAlign::Center,
        indent: true,
        leader: PtabLeader::None,
    };
    let stop = crate::ptab_stop(center, 600.0, 40.0, 60.0);
    assert_eq!(stop, 290.0);
    assert_eq!(crate::ptab_gap(TabAlign::Center, stop, 10.0, 100.0), 230.0);
    assert_eq!(crate::ptab_gap(TabAlign::Right, 500.0, 480.0, 50.0), 0.0);
    // The runs they read come from the dialog's XML.
    let run = Inline::Raw(ptab_xml(&alignment_tab_dialog(), &Default::default()));
    assert_eq!(
        crate::ptab_of(&run),
        Some(crate::Ptab {
            align: TabAlign::Left,
            indent: false,
            leader: PtabLeader::None,
        })
    );
    assert_eq!(
        crate::ptab_of(&Inline::Raw("<w:bookmarkStart/>".into())),
        None
    );
    // The leader is read too, and fills the gap by the glyph's width (#1433).
    for (name, leader) in [
        ("dot", PtabLeader::Dot),
        ("hyphen", PtabLeader::Hyphen),
        ("underscore", PtabLeader::Underscore),
        ("middleDot", PtabLeader::MiddleDot),
    ] {
        let raw = format!("<w:r><w:ptab w:alignment=\"right\" w:leader=\"{name}\"/></w:r>");
        assert_eq!(crate::ptab_of(&Inline::Raw(raw)).unwrap().leader, leader);
    }
    assert_eq!(crate::ptab_leader_count(100.0, 4.0), 25);
    assert_eq!(crate::ptab_leader_count(3.0, 4.0), 0);
    assert_eq!(crate::ptab_leader_count(100.0, 0.0), 0);
    // A picture's frame is its extent at the zoom.
    assert_eq!(
        crate::picture_frame_px(914_400, 457_200, 1.5),
        (144.0, 72.0)
    );
}

/// FIX r2 M1: the pagination estimate counts an inline picture's frame, so
/// a body paragraph holding a 4" tall picture takes at least 4" (384 px at
/// zoom 1), and an anchored picture adds nothing.
#[test]
fn a_tall_inline_picture_counts_in_the_blocks_height() {
    let para = |content: Vec<Inline>| {
        Block::Paragraph(docxcore::model::Paragraph {
            props: Default::default(),
            content,
        })
    };
    let text_only = crate::block_height_est(&para(vec![]), 600.0);
    let picture = docxcore::image::inline_picture_run("rId1", 914_400 * 6, 914_400 * 4, 1, "a.png");
    let tall = crate::block_height_est(&para(vec![Inline::Raw(picture)]), 600.0);
    assert!(tall >= 384.0, "{tall}");
    assert!(tall > text_only + 300.0, "{tall} vs {text_only}");
    let small = docxcore::image::inline_picture_run("rId1", 9525, 9525, 1, "a.png");
    assert_eq!(
        crate::block_height_est(&para(vec![Inline::Raw(small)]), 600.0),
        text_only
    );
    let anchored = "<w:r><w:drawing><wp:anchor><wp:extent cx=\"9144000\" cy=\"9144000\"/></wp:anchor></w:drawing></w:r>";
    assert_eq!(
        crate::block_height_est(&para(vec![Inline::Raw(anchored.into())]), 600.0),
        text_only
    );
}

/// Criterion 7: Online Pictures offers a file; its From a File... closes
/// it and runs Pictures..., Cancel only closes it.
#[test]
fn online_pictures_offers_a_file_instead() {
    let d = online_pictures_dialog();
    let buttons: Vec<(&str, bool)> = d
        .buttons
        .iter()
        .map(|b| (b.label.as_str(), b.default))
        .collect();
    assert_eq!(buttons, [(FROM_A_FILE, true), ("Cancel", false)]);
    assert!(
        d.text
            .as_deref()
            .unwrap()
            .contains("does not search the web")
    );
    let mut t = header_tab("online");
    assert_eq!(
        online_pictures_press(&mut t, FROM_A_FILE),
        None,
        "no dialog open"
    );
    t.dialogs.push(online_pictures_dialog());
    assert!(online_pictures_press(&mut t, "Insert").unwrap().is_err());
    assert!(t.dialogs.is_open());
    assert_eq!(online_pictures_press(&mut t, FROM_A_FILE), Some(Ok(true)));
    assert!(!t.dialogs.is_open());
    t.dialogs.push(online_pictures_dialog());
    assert_eq!(online_pictures_press(&mut t, "Cancel"), Some(Ok(false)));
    assert!(!t.dialogs.is_open());
}
