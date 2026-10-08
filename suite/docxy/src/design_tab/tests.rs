use super::*;
use crate::layout_tab::tests::{ed, ed_mut, three_sections};
use core::prelude::v1::test;
use docxcore::package::{HeaderVariant, load_package};

fn apply(t: &mut DocTab, act: DesignAct) {
    design_apply(t, act).unwrap_or_else(|e| panic!("{act:?}: {e}"));
}

/// The tab saved and loaded again.
pub(crate) fn reopen(t: &DocTab) -> Package {
    let bytes = doc_to_docx(&ed(t).doc, &t.comments, t.pkg.as_ref());
    load_package(&bytes).unwrap()
}

fn watermark_texts(t: &DocTab) -> Vec<String> {
    let pkg = t.pkg.as_ref().unwrap();
    pkg.shown_text_watermarks(&ed(t).sections())
        .into_iter()
        .map(|w| w.text)
        .collect()
}

#[test]
fn the_tab_sits_between_insert_and_layout_for_documents_only() {
    let names = |k| {
        ribbon_for(k)
            .tabs
            .iter()
            .map(|t| t.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(Kind::Docx),
        [
            "Home", "Insert", "Design", "Layout", "Mailings", "Review", "View", "Help"
        ]
    );
    assert!(!names(Kind::Xlsx).contains(&"Design"));
    let set = ribbon_tab_set(Kind::Docx);
    assert!(set[3] == (Some(RibbonTab::Design), "Design", "G"));
    assert!(ribbon_tab_set(Kind::Xlsx).iter().all(|t| t.1 != "Design"));
    let tab = design_tab();
    assert_eq!(tab.key_tip, "G");
    assert_eq!(tab.groups.len(), 1);
    assert_eq!(tab.groups[0].title, "Page Background");
    let ids: Vec<&str> = tab.groups[0]
        .items
        .iter()
        .map(|c| match c {
            Control::Dropdown { cmd, .. } | Control::Large(cmd) => cmd.label,
            _ => "?",
        })
        .collect();
    assert_eq!(ids, ["Page Color", "Watermark", "Page Borders"]);
    // The Design tab is a document tab, and leaves the workbook's alone.
    assert!(
        valid_ribbon_tab(Kind::Docx, RibbonTab::Design, false, false, false) == RibbonTab::Design
    );
    assert!(
        valid_ribbon_tab(Kind::Xlsx, RibbonTab::Design, false, false, false) == RibbonTab::Home
    );
}

#[test]
fn keytips_open_the_menus_and_page_borders() {
    let tab = design_tab();
    let act = |k| match tab_keytip_cmd(&tab, k) {
        Some(Act::Design(a)) => Some(a),
        _ => None,
    };
    assert_eq!(act("PC"), Some(DesignAct::Menu(DesignMenu::PageColor)));
    assert_eq!(act("PW"), Some(DesignAct::Menu(DesignMenu::Watermark)));
    assert_eq!(act("PB"), Some(DesignAct::PageBorders));
    assert!(tab_keytip_starts(&tab, "P"));
}

#[test]
fn menus_follow_word_with_headings_and_separators() {
    let labels = |m| -> Vec<String> {
        menu_items(m, |_| false)
            .iter()
            .map(|i| match i {
                menu::MenuItem::Item(e) => e.label.clone(),
                menu::MenuItem::Separator => "-".into(),
                menu::MenuItem::Heading(h) => format!("# {h}"),
                menu::MenuItem::TableGrid { .. } => "[grid]".into(),
            })
            .collect()
    };
    let color = labels(DesignMenu::PageColor);
    assert_eq!(color[0], "# Theme Colors");
    assert_eq!(color[11], "# Standard Colors");
    assert_eq!(color.len(), 1 + 10 + 1 + 10 + 4);
    assert_eq!(
        color[22..],
        ["-", "No Color", "More Colors...", "Fill Effects..."]
    );
    let wm = labels(DesignMenu::Watermark);
    assert_eq!(
        wm,
        [
            "# Confidential",
            "CONFIDENTIAL 1",
            "CONFIDENTIAL 2",
            "DO NOT COPY 1",
            "DO NOT COPY 2",
            "# Disclaimers",
            "DRAFT 1",
            "DRAFT 2",
            "SAMPLE 1",
            "SAMPLE 2",
            "# Urgent",
            "ASAP 1",
            "ASAP 2",
            "URGENT 1",
            "URGENT 2",
            "-",
            "Custom Watermark...",
            "Remove Watermark"
        ]
    );
}

/// Page Color writes `w:background` and the settings flag, the menu ticks
/// it, the page view paints it, it survives save, and No Color undoes it.
#[test]
fn page_color_writes_checks_paints_and_round_trips() {
    let mut t = three_sections();
    assert_eq!(page_sheet_color(&t), 0xFFFFFF);
    assert!(design_checked(&t, DesignAct::PageColor(None)));
    apply(&mut t, DesignAct::PageColor(Some(0xFFC000)));
    assert!(t.dirty);
    assert_eq!(t.status.as_ref(), "Page color: Gold, Accent 4");
    assert!(design_checked(&t, DesignAct::PageColor(Some(0xFFC000))));
    assert!(!design_checked(&t, DesignAct::PageColor(None)));
    assert!(!design_checked(&t, DesignAct::PageColor(Some(0xFF0000))));
    assert_eq!(page_sheet_color(&t), 0xFFC000);
    let pkg = reopen(&t);
    assert_eq!(pkg.page_background().map(|b| b.color), Some(0xFFC000));
    assert!(pkg.has_display_background_shape());

    apply(&mut t, DesignAct::PageColor(None));
    assert_eq!(page_sheet_color(&t), 0xFFFFFF);
    let pkg = reopen(&t);
    assert_eq!(pkg.page_background(), None);
    assert!(!pkg.has_display_background_shape());
}

#[test]
fn choosing_the_current_colour_leaves_the_tab_clean() {
    let mut t = three_sections();
    apply(&mut t, DesignAct::PageColor(None));
    assert!(!t.dirty);
}

/// A gallery watermark lands in every header the sections show (a new one,
/// here), is ticked, replaces the previous one, survives save, and Remove
/// Watermark takes it out.
#[test]
fn a_gallery_watermark_reaches_every_section_and_is_replaced_and_removed() {
    let mut t = three_sections();
    let draft1 = PRESETS.iter().position(|p| p.label == "DRAFT 1").unwrap();
    let sample2 = PRESETS.iter().position(|p| p.label == "SAMPLE 2").unwrap();
    apply(&mut t, DesignAct::Watermark(draft1));
    assert!(t.dirty);
    assert_eq!(t.status.as_ref(), "Watermark: DRAFT 1");
    assert_eq!(watermark_texts(&t), ["DRAFT"], "one shared new header");
    assert!(design_checked(&t, DesignAct::Watermark(draft1)));
    assert!(
        !design_checked(&t, DesignAct::Watermark(draft1 + 1)),
        "DRAFT 2 is horizontal"
    );
    // The new header is referenced from the first section; the others
    // inherit it.
    let sects = ed(&t).sections();
    assert!(sects[0].contains("w:headerReference"), "{}", sects[0]);
    let pkg = reopen(&t);
    let marks = pkg.watermarks();
    assert_eq!(marks.len(), 3, "every section shows it");
    assert!(
        marks
            .iter()
            .all(|w| w.header.variant == HeaderVariant::Default)
    );

    apply(&mut t, DesignAct::Watermark(sample2));
    assert_eq!(watermark_texts(&t), ["SAMPLE"], "replaced, never two");
    assert!(design_checked(&t, DesignAct::Watermark(sample2)));

    apply(&mut t, DesignAct::RemoveWatermark);
    assert!(watermark_texts(&t).is_empty());
    assert!(
        PRESETS
            .iter()
            .enumerate()
            .all(|(i, _)| !design_checked(&t, DesignAct::Watermark(i)))
    );
    assert!(reopen(&t).watermarks().is_empty());
}

/// A section with a distinct first page gets the watermark on its first
/// page too.
#[test]
fn a_title_page_section_gets_a_first_page_watermark() {
    let mut t = three_sections();
    let ed = ed_mut(&mut t);
    let k = [1];
    ed.edit_sections(&k, |raw| docxcore::sect::set_flag(raw, "w:titlePg", true));
    apply(&mut t, DesignAct::Watermark(0));
    let pkg = reopen(&t);
    let firsts: Vec<usize> = pkg
        .watermarks()
        .into_iter()
        .filter(|w| w.header.variant == HeaderVariant::First)
        .map(|w| w.header.section_index)
        .collect();
    assert_eq!(firsts, [1]);
}

/// The new header's reference is one undo step on the body.
#[test]
fn a_watermarks_new_header_reference_undoes_in_one_step() {
    let mut t = three_sections();
    let before = ed(&t).sections();
    apply(&mut t, DesignAct::Watermark(0));
    assert_ne!(ed(&t).sections(), before);
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).sections(), before);
}

/// With the header open, the watermark closes it first: its editor would
/// otherwise write the part back without the watermark.
#[test]
fn a_watermark_with_the_header_open_survives_closing_it() {
    let mut t = three_sections();
    assert!(crate::hf::open(&mut t, 1, true, HeaderVariant::Default));
    assert!(t.hf_edit.is_some());
    apply(&mut t, DesignAct::Watermark(4));
    assert!(t.hf_edit.is_none(), "the header editor was closed");
    // Opening and closing the header again keeps it.
    assert!(crate::hf::open(&mut t, 1, true, HeaderVariant::Default));
    exit_hf_tab(&mut t);
    assert_eq!(watermark_texts(&t), ["DRAFT"]);
    let pkg = reopen(&t);
    assert_eq!(pkg.watermarks().len(), 3);
}

/// What is typed into a header the watermark created is the header's own
/// text: Remove Watermark leaves it.
#[test]
fn text_typed_into_a_watermarks_new_header_survives_remove() {
    let mut t = three_sections();
    apply(&mut t, DesignAct::Watermark(0));
    assert!(crate::hf::open(&mut t, 0, true, HeaderVariant::Default));
    t.hf_edit.as_mut().unwrap().editor.insert_str("Acme Corp");
    exit_hf_tab(&mut t);
    apply(&mut t, DesignAct::RemoveWatermark);
    assert!(watermark_texts(&t).is_empty());
    let pkg = reopen(&t);
    let hdr: String = pkg
        .part_names()
        .iter()
        .filter(|n| n.starts_with("word/header"))
        .map(|n| pkg.part_text(n).unwrap())
        .collect();
    assert!(hdr.contains("Acme Corp"), "{hdr}");
}

/// Dark page colours get light text in Print Layout; light ones keep dark.
#[test]
fn ink_follows_the_page_colours_lightness() {
    let dark = (0xF2F2F2, 0xB0B0B0);
    let light = (0x202020, 0x808080);
    for rgb in [0x000000, 0x002060, 0x44546A, 0xC00000, 0x7030A0, 0x0070C0] {
        assert_eq!(page_ink(rgb), dark, "{rgb:06X}");
    }
    for rgb in [0xFFFFFF, 0xFFC000, 0xFFFF00, 0xE7E6E6, 0x92D050, 0x00B0F0] {
        assert_eq!(page_ink(rgb), light, "{rgb:06X}");
    }
}

/// A package that cannot take a header says so and stays clean; the same
/// watermark applied twice is still a success.
#[test]
fn a_refused_header_is_an_error_and_a_repeat_is_not() {
    let mut t = three_sections();
    let draft = PRESETS.iter().position(|p| p.label == "DRAFT 1").unwrap();
    apply(&mut t, DesignAct::Watermark(draft));
    t.dirty = false;
    apply(&mut t, DesignAct::Watermark(draft));
    assert_eq!(t.status.as_ref(), "Watermark: DRAFT 1");
    assert!(!t.dirty, "nothing changed");

    let mut t = three_sections();
    t.pkg.as_mut().unwrap().set_part_text(
        "word/_rels/document.xml.rels",
        "<?xml version=\"1.0\"?><Other/>",
    );
    let err = design_apply(&mut t, DesignAct::Watermark(draft)).unwrap_err();
    assert!(err.contains("Could not add the watermark"), "{err}");
    assert!(!t.dirty);
    assert!(watermark_texts(&t).is_empty());
}

/// Without a package (Markdown, or a new document), Page Color and
/// Watermark say why; Page Borders works on a new document but not on
/// Markdown.
#[test]
fn without_a_package_page_color_and_watermark_explain_and_borders_follow_the_kind() {
    let mut t = three_sections();
    t.pkg = None;
    for act in [
        DesignAct::PageColor(Some(0xFF0000)),
        DesignAct::Watermark(0),
        DesignAct::RemoveWatermark,
    ] {
        let err = design_apply(&mut t, act).unwrap_err();
        assert!(err.contains(".docx"), "{err}");
    }
    assert!(!t.dirty);
    assert!(design_enabled(Some(&t), DesignAct::PageBorders));
    t.markdown = true;
    assert!(!design_enabled(Some(&t), DesignAct::PageBorders));
    assert!(design_enabled(Some(&t), DesignAct::PageColor(None)));
}
