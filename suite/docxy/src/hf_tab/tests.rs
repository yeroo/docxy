//! The Header & Footer tab (#641) over the PAG-CASE-019 shape (see
//! `hf::tests`): labels, Link to Previous, navigation, options, position,
//! the Header/Footer galleries and Remove, and the ruler's tab stops.

use super::*;
use crate::hf::tests::{
    caret_in, ed, ed_mut, edited_text, part_text, saved, saved_hf, saved_sections, three_sections,
    three_sections_docx, tmp_dir,
};
use core::prelude::v1::test;
use docxcore::model::TabAlign;

/// Edit the header (`is_header`) of the section holding body block `block`,
/// as Insert › Header › Edit Header does.
fn edit_at(t: &mut DocTab, block: usize, is_header: bool) {
    caret_in(t, block);
    hf_apply(t, HfAct::Edit(is_header)).unwrap();
    assert!(t.hf_edit.is_some(), "{}", t.status);
}

fn state(t: &DocTab) -> Json {
    hf_state(Some(t))
}

fn get<'a>(j: &'a Json, key: &str) -> &'a Json {
    j.get(key).unwrap_or_else(|| panic!("no {key} in {j:?}"))
}

fn type_at_start(t: &mut DocTab, text: &str) {
    let editor = &mut t.hf_edit.as_mut().unwrap().editor;
    editor.caret = docxcore::editor::Caret {
        path: vec![0],
        offset: 0,
    };
    editor.insert_str(text);
}

fn slot(t: &DocTab) -> (usize, HeaderVariant, bool) {
    let h = t.hf_edit.as_ref().unwrap();
    (h.section, h.variant, h.is_header)
}

/// Criterion 5: the tab's groups in Word's order, and the tab shows only
/// while a header or footer is open.
#[test]
fn the_contextual_tab_has_words_groups_and_shows_only_while_editing() {
    let tab = hf_tab();
    assert_eq!(tab.name, "Header & Footer");
    let groups: Vec<&str> = tab.groups.iter().map(|g| g.title).collect();
    assert_eq!(
        groups,
        [
            "Header & Footer",
            "Insert",
            "Navigation",
            "Options",
            "Position",
            "Close"
        ]
    );
    // Leaving the header leaves the tab for Insert, where it was reached from.
    assert!(
        valid_ribbon_tab(Kind::Docx, RibbonTab::HeaderFooter, false, false, true)
            == RibbonTab::HeaderFooter
    );
    assert!(
        valid_ribbon_tab(Kind::Docx, RibbonTab::HeaderFooter, false, false, false)
            == RibbonTab::Insert
    );
    // The Insert tab's Header & Footer group is the three menus.
    let insert = ribbon_for(Kind::Docx)
        .tabs
        .into_iter()
        .find(|t| t.name == "Insert")
        .unwrap();
    let group = insert
        .groups
        .iter()
        .find(|g| g.title == "Header & Footer")
        .unwrap();
    let ids: Vec<&str> = group
        .items
        .iter()
        .filter_map(|c| match c {
            Control::Dropdown { cmd, .. } => Some(cmd.id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["hf-header", "hf-footer", "hf-pagenum"]);
}

/// Criterion 6 / PAG-067.
#[test]
fn area_labels_follow_words_names() {
    use HeaderVariant::*;
    assert_eq!(area_label(true, Default, false, 0, 1), "Header");
    assert_eq!(area_label(false, Default, false, 0, 1), "Footer");
    assert_eq!(area_label(true, First, false, 0, 1), "First Page Header");
    assert_eq!(area_label(false, First, true, 0, 1), "First Page Footer");
    assert_eq!(area_label(true, Even, true, 0, 1), "Even Page Header");
    assert_eq!(area_label(true, Default, true, 0, 1), "Odd Page Header");
    assert_eq!(area_label(false, Default, true, 0, 1), "Odd Page Footer");
    assert_eq!(area_label(true, Default, false, 1, 3), "Header -Section 2-");
}

/// PAG-CASE-020 step 2 / PAG-CASE-021 step 1.
#[test]
fn hf_state_reports_the_section_labels_and_same_as_previous() {
    let mut t = three_sections("state", true);
    assert_eq!(get(&state(&t), "editing"), &Json::Bool(false));
    edit_at(&mut t, 1, true);
    let s = state(&t);
    assert_eq!(get(&s, "editing"), &Json::Bool(true));
    assert_eq!(get(&s, "kind").as_str(), Some("header"));
    assert_eq!(get(&s, "section"), &Json::Num(2.0));
    assert_eq!(get(&s, "variant").as_str(), Some("default"));
    assert_eq!(get(&s, "label").as_str(), Some("Header -Section 2-"));
    assert_eq!(get(&s, "same_as_previous"), &Json::Bool(true));
    assert_eq!(get(&s, "text").as_str(), Some("Header A"));
    assert_eq!(get(&s, "header_from_top"), &Json::Num(0.5));
    assert_eq!(get(&s, "footer_from_bottom"), &Json::Num(0.5));
    assert_eq!(get(&s, "different_first_page"), &Json::Bool(false));
    assert_eq!(get(&s, "show_document_text"), &Json::Bool(true));
    // Section 1 and section 3 have their own headers.
    exit_hf_tab(&mut t);
    edit_at(&mut t, 0, true);
    assert_eq!(get(&state(&t), "same_as_previous"), &Json::Bool(false));
    assert_eq!(
        get(&state(&t), "label").as_str(),
        Some("Header -Section 1-")
    );
    exit_hf_tab(&mut t);
    edit_at(&mut t, 2, true);
    assert_eq!(get(&state(&t), "same_as_previous"), &Json::Bool(false));
    assert_eq!(get(&state(&t), "text").as_str(), Some("Header C"));
}

/// Criterion 7 / PAG-CASE-021 step 3: unlinking copies the content into the
/// section's own part; relinking drops the reference; both undo in the body.
#[test]
fn link_to_previous_unlinks_with_a_copy_and_relinks_by_dropping_the_reference() {
    let mut t = three_sections("link", true);
    edit_at(&mut t, 1, true);
    let inherited = t.hf_edit.as_ref().unwrap().part_name.clone();
    assert!(hf_checked(&t, HfAct::LinkToPrevious));
    hf_apply(&mut t, HfAct::LinkToPrevious).unwrap();
    assert!(!linked(&t), "the label goes");
    assert!(!hf_checked(&t, HfAct::LinkToPrevious));
    assert_eq!(edited_text(&t), "Header A", "the text is unchanged");
    let own = t.hf_edit.as_ref().unwrap().part_name.clone();
    assert_ne!(own, inherited);
    // Editing now changes only section 2.
    type_at_start(&mut t, "2");
    let pkg = saved(&mut t);
    let sections = saved_sections(&pkg);
    assert!(sections[1].contains("headerReference"), "{}", sections[1]);
    assert_eq!(saved_hf(&pkg, 0, true), "Header A");
    assert_eq!(saved_hf(&pkg, 1, true), "2Header A");
    assert_eq!(saved_hf(&pkg, 2, true), "Header C");
    // Relink: no reference again, and the section shows section 1's header.
    hf_apply(&mut t, HfAct::LinkToPrevious).unwrap();
    assert!(linked(&t));
    assert_eq!(edited_text(&t), "Header A");
    let pkg = saved(&mut t);
    assert!(!saved_sections(&pkg)[1].contains("headerReference"));
    assert_eq!(saved_hf(&pkg, 1, true), "Header A");
    // Each is one body undo step, readable without saving, after leaving.
    exit_hf_tab(&mut t);
    assert!(ed_mut(&mut t).undo());
    assert!(
        ed(&t).sections()[1].contains("headerReference"),
        "unlinked again"
    );
    assert!(ed_mut(&mut t).undo());
    assert!(
        !ed(&t).sections()[1].contains("headerReference"),
        "linked again"
    );
    edit_at(&mut t, 1, true);
    assert!(linked(&t));
    assert_eq!(get(&state(&t), "same_as_previous"), &Json::Bool(true));
}

#[test]
fn link_to_previous_is_refused_in_the_first_section_and_links_per_variant() {
    let mut t = three_sections("link-variants", true);
    edit_at(&mut t, 0, true);
    assert!(hf_apply(&mut t, HfAct::LinkToPrevious).is_err());
    exit_hf_tab(&mut t);
    // Section 2's first-page header unlinks on its own: the default one stays linked.
    caret_in(&mut t, 1);
    hf_apply(&mut t, HfAct::DifferentFirst).unwrap();
    assert!(crate::hf::open(&mut t, 1, true, HeaderVariant::First));
    hf_apply(&mut t, HfAct::LinkToPrevious).unwrap();
    let sect = &ed(&t).sections()[1];
    assert!(hf_reference(sect, true, "first").is_some(), "{sect}");
    assert!(hf_reference(sect, true, "default").is_none(), "{sect}");
}

/// Criterion 8 / PAG-CASE-021 step 2 and PAG-071.
#[test]
fn go_to_and_next_previous_walk_the_pages_slots() {
    let mut t = three_sections("nav", true);
    edit_at(&mut t, 1, true);
    hf_apply(&mut t, HfAct::GoTo(false)).unwrap();
    assert_eq!(slot(&t), (1, HeaderVariant::Default, false));
    assert_eq!(edited_text(&t), "Footer A");
    hf_apply(&mut t, HfAct::GoTo(true)).unwrap();
    assert_eq!(slot(&t), (1, HeaderVariant::Default, true));
    hf_apply(&mut t, HfAct::Next).unwrap();
    assert_eq!(slot(&t), (2, HeaderVariant::Default, true));
    assert_eq!(edited_text(&t), "Header C");
    hf_apply(&mut t, HfAct::Next).unwrap();
    assert_eq!(slot(&t), (2, HeaderVariant::Default, true), "the last one");
    hf_apply(&mut t, HfAct::Previous).unwrap();
    hf_apply(&mut t, HfAct::Previous).unwrap();
    assert_eq!(slot(&t), (0, HeaderVariant::Default, true));
    hf_apply(&mut t, HfAct::Previous).unwrap();
    assert_eq!(slot(&t), (0, HeaderVariant::Default, true), "the first one");
    // With a first page in section 2, Next from section 1 goes to it.
    exit_hf_tab(&mut t);
    caret_in(&mut t, 1);
    hf_apply(&mut t, HfAct::DifferentFirst).unwrap();
    edit_at(&mut t, 0, true);
    hf_apply(&mut t, HfAct::Next).unwrap();
    assert_eq!(slot(&t), (1, HeaderVariant::First, true));
}

#[test]
fn next_does_nothing_in_a_one_page_document_with_odd_and_even_pages() {
    let base = tab_from_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    );
    let mut t = base;
    assert_eq!(page_ranges(&t).len(), 1);
    hf_apply(&mut t, HfAct::Edit(true)).unwrap();
    hf_apply(&mut t, HfAct::DifferentOddEven).unwrap();
    assert!(hf_checked(&t, HfAct::DifferentOddEven));
    let before = slot(&t);
    hf_apply(&mut t, HfAct::Next).unwrap();
    assert_eq!(slot(&t), before);
    assert_eq!(edit_label(&t).as_deref(), Some("Odd Page Header"));
}

/// Criterion 9.
#[test]
fn show_document_text_is_view_state_kept_while_navigating_and_reset_on_exit() {
    let mut t = three_sections("show-text", true);
    edit_at(&mut t, 0, true);
    assert!(hf_checked(&t, HfAct::ShowText));
    hf_apply(&mut t, HfAct::ShowText).unwrap();
    assert!(!hf_checked(&t, HfAct::ShowText));
    assert!(!t.dirty, "view state only");
    hf_apply(&mut t, HfAct::Next).unwrap();
    assert!(!t.hf_edit.as_ref().unwrap().show_text);
    exit_hf_tab(&mut t);
    edit_at(&mut t, 0, true);
    assert!(t.hf_edit.as_ref().unwrap().show_text);
}

/// Criterion 10: the position boxes edit the edited section's distances.
#[test]
fn header_from_top_and_footer_from_bottom_edit_the_section_undoably() {
    let mut t = three_sections("distance", true);
    edit_at(&mut t, 1, true);
    assert_eq!(distance_of(&t, true), Some(720));
    hf_apply(
        &mut t,
        HfAct::Distance {
            is_header: true,
            twips: 432,
        },
    )
    .unwrap();
    let sections = ed(&t).sections();
    assert_eq!(crate::hf::distance(&sections[1], true), 432);
    assert_eq!(crate::hf::distance(&sections[0], true), 720);
    assert_eq!(get(&state(&t), "header_from_top"), &Json::Num(0.3));
    // The menu ticks the current distance.
    let items = menu_items(Some(&t), HfMenu::HeaderFromTop);
    let ticked: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            menu::MenuItem::Item(e) if e.checked => Some(e.label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ticked, ["0.3\""]);
    // Custom...: the dialog writes the footer distance.
    let mut d = distance_dialog(&t, false).unwrap();
    assert_eq!(d.controls[0].text(), "0.5");
    d.controls[0].value = Value::Text("0.75".into());
    assert!(apply_distance(ed_mut(&mut t), &d, false, 1).unwrap());
    assert_eq!(crate::hf::distance(&ed(&t).sections()[1], false), 1080);
    d.controls[0].value = Value::Text("x".into());
    assert!(apply_distance(ed_mut(&mut t), &d, false, 1).is_err());
    assert!(ed_mut(&mut t).undo());
    assert!(ed_mut(&mut t).undo());
    assert_eq!(crate::hf::distance(&ed(&t).sections()[1], true), 720);
}

/// Criterion 11 / PAG-CASE-020 step 4 / PAG-CASE-021 step 5.
#[test]
fn header_designs_fill_the_section_part_and_remove_empties_it_keeping_the_reference() {
    let mut t = three_sections("designs", true);
    caret_in(&mut t, 1);
    hf_apply(
        &mut t,
        HfAct::Design {
            is_header: true,
            index: 0,
        },
    )
    .unwrap();
    assert_eq!(slot(&t), (1, HeaderVariant::Default, true));
    assert_eq!(edited_text(&t), "[Type here]");
    // Section 2 is linked, so the design is in the part it shares with section 1.
    let pkg = saved(&mut t);
    assert_eq!(saved_hf(&pkg, 0, true), "[Type here]");
    assert!(!saved_sections(&pkg)[1].contains("headerReference"));
    hf_apply(
        &mut t,
        HfAct::Design {
            is_header: true,
            index: 1,
        },
    )
    .unwrap();
    let body = &t.hf_edit.as_ref().unwrap().editor.doc.body;
    let Block::Paragraph(p) = &body[0] else {
        panic!()
    };
    assert_eq!(p.props.style_id.as_deref(), Some("Header"));
    let tabs = p
        .content
        .iter()
        .filter(|i| matches!(i, Inline::Tab(_)))
        .count();
    assert_eq!((body.len(), tabs), (1, 2));
    // A footer design with no footer anywhere creates one from section 1.
    let mut t = three_sections("designs-new", false);
    caret_in(&mut t, 2);
    hf_apply(
        &mut t,
        HfAct::Design {
            is_header: false,
            index: 0,
        },
    )
    .unwrap();
    assert!(hf_reference(&ed(&t).sections()[0], false, "default").is_some());
    // Remove Footer empties the part and keeps the reference.
    hf_apply(&mut t, HfAct::Remove(false)).unwrap();
    assert_eq!(edited_text(&t), "");
    let pkg = saved(&mut t);
    assert!(hf_reference(&saved_sections(&pkg)[0], false, "default").is_some());
    assert_eq!(saved_hf(&pkg, 2, false), "");
    // Remove Header in section 3 of the headed document: its own part.
    let mut t = three_sections("remove", true);
    caret_in(&mut t, 2);
    hf_apply(&mut t, HfAct::Remove(true)).unwrap();
    let pkg = saved(&mut t);
    assert!(hf_reference(&saved_sections(&pkg)[2], true, "default").is_some());
    assert_eq!(saved_hf(&pkg, 2, true), "");
    assert_eq!(saved_hf(&pkg, 0, true), "Header A");
}

/// Criterion 12: every page maps to its own section for a double-click.
#[test]
fn a_page_maps_to_its_section_and_variant() {
    let mut t = three_sections("page-slot", true);
    let got: Vec<Option<(usize, HeaderVariant)>> = (0..4)
        .map(|p| page_slot(&t, p).map(|s| (s.section, s.variant)))
        .collect();
    assert_eq!(
        got,
        vec![
            Some((0, HeaderVariant::Default)),
            Some((1, HeaderVariant::Default)),
            Some((2, HeaderVariant::Default)),
            None
        ]
    );
    // Opening page 2's area edits section 2's (inherited) header.
    let s = page_slot(&t, 1).unwrap();
    assert!(crate::hf::open(&mut t, s.section, true, s.variant));
    assert_eq!(edited_text(&t), "Header A");
    assert!(linked(&t));
}

/// Criterion 13 / PAG-068: a new header paragraph shows the Header style's
/// centre and right stops on the ruler, on a package that had no such style.
#[test]
fn a_new_headers_ruler_shows_the_header_styles_centre_and_right_stops() {
    let dir = tmp_dir("ruler");
    let path = dir.join("in.docx");
    std::fs::write(&path, three_sections_docx(false, false)).unwrap();
    let mut t = tab_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let styles = String::from_utf8_lossy(t.pkg.as_ref().unwrap().part("word/styles.xml").unwrap())
        .into_owned();
    assert!(!styles.contains("w:styleId=\"Header\""));
    edit_at(&mut t, 0, true);
    let (_, tabs) = ruler_para_of(&t);
    let got: Vec<(i32, TabAlign)> = tabs.iter().map(|t| (t.pos, t.align)).collect();
    assert_eq!(got, vec![(4680, TabAlign::Center), (9360, TabAlign::Right)]);
    // The body paragraph's ruler has none.
    exit_hf_tab(&mut t);
    assert!(ruler_para_of(&t).1.is_empty());
}

#[test]
fn the_menus_list_the_gallery_then_edit_and_remove() {
    let labels = |menu: HfMenu| -> Vec<String> {
        menu_items(None, menu)
            .iter()
            .map(|i| match i {
                menu::MenuItem::Item(e) => e.label.clone(),
                menu::MenuItem::Separator => "-".into(),
                menu::MenuItem::Heading(h) => format!("[{h}]"),
            })
            .collect()
    };
    assert_eq!(
        labels(HfMenu::Header),
        [
            "[Built-in]",
            "Blank",
            "Blank (Three Columns)",
            "-",
            "Edit Header",
            "Remove Header"
        ]
    );
    assert_eq!(
        labels(HfMenu::Footer),
        [
            "[Built-in]",
            "Blank",
            "Blank (Three Columns)",
            "-",
            "Edit Footer",
            "Remove Footer"
        ]
    );
}

#[test]
fn saved_links_survive_a_reload() {
    let mut t = three_sections("reload", true);
    edit_at(&mut t, 1, true);
    hf_apply(&mut t, HfAct::LinkToPrevious).unwrap();
    exit_hf_tab(&mut t);
    let pkg = saved(&mut t);
    let part = hf_part_name_typed(&pkg, &saved_sections(&pkg)[1], true, "default").unwrap();
    assert_eq!(part_text(&pkg, &part), "Header A");
}

/// M1 (r2): a part whose root declares only some of the namespaces the
/// header editor writes (`w`, `r`, `m`) gets the missing ones on rewrite;
/// its other root attributes stay as they were.
#[test]
fn a_rewritten_part_declares_the_namespaces_its_content_uses() {
    let mut pkg = docxcore::package::new_package(docxcore::model::Document {
        body: vec![Block::Paragraph(Default::default())],
    });
    let (_, part) = pkg.create_hf_part(true, "<w:p/>").unwrap();
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let r = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let root = format!("<w:hdr xmlns:w=\"{w}\" xmlns:r=\"{r}\" xmlns:x=\"urn:x\" x:keep=\"1\">");
    pkg.set_part(
        &part,
        format!("<?xml version=\"1.0\"?>\n{root}<w:p/></w:hdr>").into_bytes(),
    );
    let math = "<w:p><m:oMathPara><m:oMath><m:r><m:t>x</m:t></m:r></m:oMath></m:oMathPara></w:p>";
    rewrite_part(&mut pkg, &part, true, math);
    let out = String::from_utf8_lossy(pkg.part(&part).unwrap()).into_owned();
    let head = &out[..out.find(math).unwrap()];
    assert!(
        head.contains("xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\""),
        "{out}"
    );
    for kept in [
        format!("xmlns:w=\"{w}\""),
        format!("xmlns:r=\"{r}\""),
        "xmlns:x=\"urn:x\"".into(),
        "x:keep=\"1\"".into(),
    ] {
        assert_eq!(head.matches(kept.as_str()).count(), 1, "{kept} in {out}");
    }
    assert!(out.ends_with(&format!("{math}</w:hdr>")));
    // A part the app creates declares `m` from the start.
    let (_, new) = pkg.create_hf_part(false, "<w:p/>").unwrap();
    let xml = String::from_utf8_lossy(pkg.part(&new).unwrap()).into_owned();
    assert!(xml.contains("xmlns:m="), "{xml}");
}

/// m3 (r2): the ruler's indents (which a drag starts from) are the open
/// header's paragraph's, not the body's.
#[test]
fn the_rulers_indents_follow_the_open_header() {
    let mut t = three_sections("ruler-indent", true);
    edit_at(&mut t, 0, true);
    t.hf_edit.as_mut().unwrap().editor.set_indent(720, 0);
    let header = eff_indent(&t.hf_edit.as_ref().unwrap().editor.caret_para_props());
    let body = eff_indent(&ed(&t).caret_para_props());
    assert_ne!(header, body);
    assert_eq!(ruler_para_of(&t).0, header);
}

/// The KeyTips one tab shows at once: its buttons', not its menus' items.
fn tab_key_tips(tab: &rs::Tab<Act>) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    for group in &tab.groups {
        for control in &group.items {
            let mut push = |c: &rs::Cmd<Act>| {
                if !c.key_tip.is_empty() {
                    out.push((c.key_tip, c.id));
                }
            };
            match control {
                Control::Large(c) | Control::Toggle(c) => push(c),
                Control::Column(cs) => cs.iter().for_each(&mut push),
                Control::Split { primary, .. } => push(primary),
                Control::Dropdown { cmd, .. } => push(cmd),
                Control::Rows(rows) => {
                    for cell in rows.iter().flatten() {
                        match cell {
                            rs::Cell::Btn(c) | rs::Cell::Combo { cmd: c, .. } => push(c),
                        }
                    }
                }
                Control::Gallery(_) | Control::Separator => {}
            }
        }
    }
    out
}

/// m5: no KeyTip on a tab is a prefix of another there, or typing it would
/// run one command when the person meant the other. Every tab of every kind.
#[test]
fn every_tabs_key_tips_are_prefix_free() {
    let mut tabs: Vec<rs::Tab<Act>> = Vec::new();
    for kind in [Kind::Docx, Kind::Project, Kind::Xlsx] {
        tabs.extend(ribbon_for(kind).tabs);
    }
    tabs.push(hf_tab());
    tabs.push(table_tab());
    tabs.push(gantt_format_tab());
    for tab in &tabs {
        let tips = tab_key_tips(tab);
        for (a, ida) in &tips {
            for (b, idb) in &tips {
                if ida != idb {
                    assert!(
                        !b.to_ascii_uppercase().starts_with(&a.to_ascii_uppercase()),
                        "on {}: {ida}'s KeyTip {a} is a prefix of {idb}'s {b}",
                        tab.name
                    );
                }
            }
        }
    }
    // The contextual tab's own KeyTip is the one its strip entry shows.
    assert_eq!(hf_tab().key_tip, "J");
}
