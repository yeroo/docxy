//! Insert › Pages (#652): the group's order and key tips, the Cover Page
//! menu, what each command does to the body, and when they are disabled.

use super::*;
use crate::hf::tests::{ed, ed_mut, saved, three_sections};
use core::prelude::v1::test;
use docxcore::cover::is_cover_open;

fn labels(items: &[menu::MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            menu::MenuItem::Item(e) if e.enabled => e.label.clone(),
            menu::MenuItem::Item(e) => format!("({})", e.label),
            menu::MenuItem::Separator => "-".into(),
            menu::MenuItem::Heading(h) => format!("[{h}]"),
            menu::MenuItem::TableGrid { .. } => "[grid]".into(),
        })
        .collect()
}

fn covers(t: &DocTab) -> usize {
    ed(t).doc.body.iter().filter(|b| is_cover_open(b)).count()
}

/// Each Pages control's label, key tip and command.
fn pages_cmds() -> Vec<(String, String, Act)> {
    let ribbon = docxy_ribbon();
    let insert = ribbon.tabs.iter().find(|t| t.name == "Insert").unwrap();
    let pages = insert.groups.iter().find(|g| g.title == "Pages").unwrap();
    pages
        .items
        .iter()
        .map(|c| match c {
            Control::Large(cmd) | Control::Dropdown { cmd, .. } => {
                (cmd.label.to_string(), cmd.key_tip.to_string(), cmd.act)
            }
            _ => panic!("unexpected control in Pages"),
        })
        .collect()
}

#[test]
fn the_pages_group_is_cover_page_blank_page_page_break() {
    let cmds = pages_cmds();
    let names: Vec<(&str, &str)> = cmds.iter().map(|c| (&*c.0, &*c.1)).collect();
    assert_eq!(
        names,
        [
            ("Cover Page", "V"),
            ("Blank Page", "NP"),
            ("Page Break", "B")
        ]
    );
    assert!(matches!(cmds[0].2, Act::Cover(CoverAct::Menu)));
    assert!(matches!(cmds[1].2, Act::BlankPage));
    // The drop-down lists the gallery and Remove for ribbon-read.
    let items: Vec<&str> = ribbon_items().iter().map(|c| c.label).collect();
    assert_eq!(
        items,
        [
            "Plain",
            "Centered",
            "Title Band",
            "Ruled",
            "Remove Current Cover Page"
        ]
    );
}

#[test]
fn remove_is_available_only_with_a_cover() {
    let mut t = three_sections("cover-menu", false);
    assert_eq!(
        labels(&menu_items(Some(&t))),
        [
            "[Built-in]",
            "Plain",
            "Centered",
            "Title Band",
            "Ruled",
            "-",
            "(Remove Current Cover Page)"
        ]
    );
    assert!(!cover_enabled(Some(&t), CoverAct::Remove));
    cover_apply(&mut t, CoverAct::Design(1)).unwrap();
    assert!(cover_enabled(Some(&t), CoverAct::Remove));
    assert_eq!(
        labels(&menu_items(Some(&t))).last().unwrap(),
        "Remove Current Cover Page"
    );
}

#[test]
fn a_design_inserts_then_replaces_and_remove_takes_it_away() {
    let mut t = three_sections("cover-apply", false);
    let before = ed(&t).doc.clone();
    cover_apply(&mut t, CoverAct::Design(0)).unwrap();
    assert!(t.dirty);
    assert_eq!(t.status.as_ref(), "Cover page: Plain");
    assert!(is_cover_open(&ed(&t).doc.body[0]));
    assert!(docxcore::sect::has_flag(&ed(&t).sections()[0], "w:titlePg"));

    cover_apply(&mut t, CoverAct::Design(2)).unwrap();
    assert_eq!(t.status.as_ref(), "Cover page: Title Band");
    assert_eq!(covers(&t), 1, "replaced, not added");

    t.dirty = false;
    cover_apply(&mut t, CoverAct::Remove).unwrap();
    assert!(t.dirty);
    assert_eq!(t.status.as_ref(), "Cover page removed");
    assert_eq!(covers(&t), 0);
    // Three steps back is the document as it was.
    for _ in 0..3 {
        assert!(ed_mut(&mut t).undo());
    }
    assert_eq!(ed(&t).doc, before);

    let err = cover_apply(&mut t, CoverAct::Remove).unwrap_err();
    assert!(err.contains("no cover page"), "{err}");
}

#[test]
fn a_saved_cover_is_still_one() {
    let mut t = three_sections("cover-save", false);
    cover_apply(&mut t, CoverAct::Design(3)).unwrap();
    let pkg = saved(&mut t);
    let reopened = Editor::new(pkg.document.clone());
    assert!(reopened.has_cover_page());
}

#[test]
fn cover_ids_avoid_the_headers_ones() {
    let mut t = three_sections("cover-ids", true);
    let pkg = t.pkg.as_mut().unwrap();
    let header = pkg
        .part_names()
        .into_iter()
        .find(|n| n.starts_with("word/header"))
        .unwrap()
        .to_string();
    let xml = pkg.part_text(&header).unwrap();
    let with_sdt = xml.replacen(
        "<w:p>",
        "<w:sdt><w:sdtPr><w:id w:val=\"777000\"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>x</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p>",
        1,
    );
    assert!(pkg.set_part_text(&header, &with_sdt));
    cover_apply(&mut t, CoverAct::Design(0)).unwrap();
    let body = docxcore::serialize::blocks_to_xml(&ed(&t).doc.body);
    let ids = sdt_ids(&body);
    assert!(
        !ids.is_empty() && ids.iter().all(|&id| id > 777000),
        "{ids:?}"
    );
}

#[test]
fn blank_page_puts_an_empty_page_at_the_caret() {
    let mut t = three_sections("blank-page", false);
    crate::hf::tests::caret_in(&mut t, 0);
    let before = ed(&t).doc.clone();
    blank_page_apply(&mut t).unwrap();
    assert!(t.dirty);
    let body = &ed(&t).doc.body;
    let breaks = body.iter().take(2).filter(|b| has_page_break(b)).count();
    assert_eq!(breaks, 2);
    // The page view pages them as Word does: the break paragraph is a page
    // of its own between the caret's page and the text after the caret.
    let pages = paginate(body, 1.0e6, 600.0);
    assert_eq!(&pages[..2], &[(0, 1), (1, 2)], "{pages:?}");
    assert_eq!(
        body[1].plain_text(),
        "
"
    );
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).doc, before);
}

#[test]
fn both_are_off_while_a_header_is_edited() {
    let mut t = three_sections("cover-hf", true);
    assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
    assert!(!cover_enabled(Some(&t), CoverAct::Menu));
    assert!(!cover_enabled(Some(&t), CoverAct::Design(0)));
    assert!(!blank_page_enabled(Some(&t)));
    let before = ed(&t).doc.clone();
    let hf_before = t.hf_edit.as_ref().unwrap().editor.doc.clone();
    assert!(cover_apply(&mut t, CoverAct::Design(0)).is_err());
    assert!(blank_page_apply(&mut t).is_err());
    assert_eq!(ed(&t).doc, before);
    assert_eq!(t.hf_edit.as_ref().unwrap().editor.doc, hf_before);
}

#[test]
fn cover_page_needs_a_docx_and_a_document() {
    let mut t = three_sections("cover-md", false);
    t.markdown = true;
    assert!(!cover_enabled(Some(&t), CoverAct::Menu));
    let err = cover_apply(&mut t, CoverAct::Design(0)).unwrap_err();
    assert!(err.contains("Markdown"), "{err}");
    // Page breaks are fine in Markdown, as Page Break is.
    assert!(blank_page_enabled(Some(&t)));
    assert!(!cover_enabled(None, CoverAct::Design(0)));
    assert!(!blank_page_enabled(None));
}
