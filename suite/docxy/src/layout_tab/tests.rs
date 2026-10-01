use super::*;
use core::prelude::v1::test;
use docxcore::model::{ClearKind, Inline, ParProps, Run, SectionProperties};

fn sect(extra: &str) -> String {
    format!(
        "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" \
         w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" \
         w:footer=\"720\" w:gutter=\"0\"/><w:cols w:space=\"720\"/>{extra}</w:sectPr>"
    )
}

fn para(text: &str, sect: Option<String>) -> Block {
    Block::Paragraph(docxcore::model::Paragraph {
        props: ParProps {
            section_break: sect,
            ..ParProps::default()
        },
        content: vec![Inline::Run(Run {
            text: text.into(),
            ..Run::default()
        })],
    })
}

/// A .docx tab with three sections: "one" | "two", "two b" | "three", with
/// the caret in section 2 ("t|wo").
pub(crate) fn three_sections() -> DocTab {
    let mut t = tab_from_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    );
    let ed = ed_mut(&mut t);
    let mut doc = ed.doc.clone();
    doc.body = vec![
        para("one", Some(sect("<w:docGrid w:linePitch=\"360\"/>"))),
        para("two", None),
        para("two b", Some(sect(""))),
        para("three", None),
    ];
    doc.set_trailing_section_properties(SectionProperties {
        raw: sect(""),
        property_change: None,
    });
    *ed = Editor::new(doc);
    ed.caret = Caret::top(1, 1);
    t
}

pub(crate) fn ed(t: &DocTab) -> &Editor {
    let Surface::Doc(ed) = &t.surface else {
        panic!()
    };
    ed
}

pub(crate) fn ed_mut(t: &mut DocTab) -> &mut Editor {
    let Surface::Doc(ed) = &mut t.surface else {
        panic!()
    };
    ed
}

pub(crate) fn setups(t: &DocTab) -> Vec<SectionSetup> {
    ed(t)
        .sections()
        .iter()
        .map(|s| SectionSetup::parse(s))
        .collect()
}

fn apply(t: &mut DocTab, act: LayoutAct) {
    layout_apply(t, act).unwrap_or_else(|e| panic!("{act:?}: {e}"));
}

/// Every section command on the caret's section 2 changes only its sectPr,
/// as one undo step that redo reapplies.
#[test]
fn each_section_command_changes_only_the_caret_section() {
    use LayoutAct as L;
    let commands = [
        L::Margins(MarginPreset::Narrow),
        L::Margins(MarginPreset::Moderate),
        L::Margins(MarginPreset::Wide),
        L::Margins(MarginPreset::Mirrored),
        L::Orient(true),
        L::Paper(Paper::A4),
        L::Paper(Paper::Legal),
        L::Columns(ColumnsPreset::Two),
        L::Columns(ColumnsPreset::Three),
        L::Columns(ColumnsPreset::Left),
        L::Columns(ColumnsPreset::Right),
        L::LineNumbers(LnChoice::Continuous),
        L::LineNumbers(LnChoice::RestartEachPage),
        L::LineNumbers(LnChoice::RestartEachSection),
    ];
    for act in commands {
        let mut t = three_sections();
        let before = ed(&t).doc.clone();
        let raw_before = ed(&t).sections();
        apply(&mut t, act);
        let raw_after = ed(&t).sections();
        assert_ne!(raw_after[1], raw_before[1], "{act:?} changed section 2");
        assert_eq!(raw_after[0], raw_before[0], "{act:?} left section 1");
        assert_eq!(raw_after[2], raw_before[2], "{act:?} left section 3");
        assert!(t.dirty);
        assert!(layout_checked(&t, act), "{act:?} is checked after it runs");
        let after = ed(&t).doc.clone();
        assert!(ed_mut(&mut t).undo(), "{act:?}");
        assert_eq!(ed(&t).doc, before, "{act:?} undoes in one step");
        assert!(ed_mut(&mut t).redo());
        assert_eq!(ed(&t).doc, after, "{act:?} redoes");
    }
}

/// A command that changes nothing (the current orientation, Normal margins
/// on Normal margins, hyphenation already off) leaves a clean tab clean.
#[test]
fn a_command_that_changes_nothing_leaves_the_tab_clean() {
    use LayoutAct as L;
    for act in [
        L::Orient(false),
        L::Margins(MarginPreset::Normal),
        // Not Letter here: the fixture's pgSz has no w:code, and choosing
        // Letter writes one.
        L::Columns(ColumnsPreset::One),
        L::LineNumbers(LnChoice::None),
        L::Hyphen(false),
    ] {
        let mut t = three_sections();
        t.dirty = false;
        let before = ed(&t).doc.clone();
        apply(&mut t, act);
        assert!(!t.dirty, "{act:?} changed nothing");
        assert_eq!(ed(&t).doc, before, "{act:?}");
        assert!(!ed_mut(&mut t).undo(), "{act:?} recorded no undo step");
    }
}

#[test]
fn a_selection_over_two_sections_changes_both() {
    let mut t = three_sections();
    ed_mut(&mut t).anchor = Some(Caret::top(0, 1));
    apply(&mut t, LayoutAct::Orient(true));
    let s = setups(&t);
    assert!(s[0].page.landscape && s[1].page.landscape);
    assert!(!s[2].page.landscape);
    assert!(ed_mut(&mut t).undo());
    assert!(setups(&t).iter().all(|s| !s.page.landscape), "one step");
}

#[test]
fn presets_write_words_values() {
    let mut t = three_sections();
    apply(&mut t, LayoutAct::Margins(MarginPreset::Moderate));
    let m = setups(&t)[1].margins;
    assert_eq!((m.top, m.bottom, m.left, m.right), (1440, 1440, 1080, 1080));
    // Mirrored: inside 1.25", outside 1", and w:mirrorMargins on; any other
    // preset turns it off again.
    apply(&mut t, LayoutAct::Margins(MarginPreset::Mirrored));
    assert!(t.pkg.as_ref().unwrap().has_mirror_margins());
    assert_eq!(setups(&t)[1].margins.left, 1800);
    assert!(layout_checked(
        &t,
        LayoutAct::Margins(MarginPreset::Mirrored)
    ));
    apply(&mut t, LayoutAct::Margins(MarginPreset::Normal));
    assert!(!t.pkg.as_ref().unwrap().has_mirror_margins());
    assert!(layout_checked(&t, LayoutAct::Margins(MarginPreset::Normal)));
    assert!(!layout_checked(
        &t,
        LayoutAct::Margins(MarginPreset::Mirrored)
    ));
    // Landscape, then a paper size: kept in landscape, with its code.
    apply(&mut t, LayoutAct::Orient(true));
    apply(&mut t, LayoutAct::Paper(Paper::A4));
    let p = setups(&t)[1].page;
    assert_eq!((p.w, p.h, p.code), (16838, 11906, Some(9)));
    assert!(layout_checked(&t, LayoutAct::Paper(Paper::A4)));
    assert!(layout_checked(&t, LayoutAct::Orient(true)));
    // Left columns on a 6.5" text width.
    let mut t = three_sections();
    apply(&mut t, LayoutAct::Columns(ColumnsPreset::Left));
    let c = &setups(&t)[1].columns;
    assert_eq!(c.cols.iter().map(|c| c.w).collect::<Vec<_>>(), [2640, 6000]);
    // Line numbers keep w:start and w:distance, and write countBy 1.
    let mut t = three_sections();
    ed_mut(&mut t).edit_section_setups(&[1], |s| {
        s.line_numbers = Some(docxcore::sect::LineNumbering {
            count_by: 5,
            start: Some(3),
            distance: Some(400),
            restart: LnRestart::NewPage,
        })
    });
    apply(&mut t, LayoutAct::LineNumbers(LnChoice::RestartEachSection));
    let ln = setups(&t)[1].line_numbers.unwrap();
    assert_eq!(
        (ln.count_by, ln.start, ln.distance, ln.restart),
        (1, Some(3), Some(400), LnRestart::NewSection)
    );
    apply(&mut t, LayoutAct::LineNumbers(LnChoice::None));
    assert!(setups(&t)[1].line_numbers.is_none());
    assert!(layout_checked(&t, LayoutAct::LineNumbers(LnChoice::None)));
}

#[test]
fn breaks_go_in_at_the_caret() {
    for (choice, kind) in [
        (BreakChoice::Page, BreakKind::Page),
        (BreakChoice::Column, BreakKind::Column),
        (BreakChoice::TextWrapping, BreakKind::Clear(ClearKind::All)),
    ] {
        let mut t = three_sections();
        apply(&mut t, LayoutAct::Break(choice));
        let Block::Paragraph(p) = &ed(&t).doc.body[1] else {
            panic!()
        };
        assert!(
            p.content
                .iter()
                .any(|i| matches!(i, Inline::Break(k, _) if *k == kind)),
            "{choice:?}"
        );
    }
    let mut t = three_sections();
    apply(
        &mut t,
        LayoutAct::Break(BreakChoice::Section(SectionStart::Continuous)),
    );
    let s = setups(&t);
    assert_eq!(s.len(), 4);
    assert_eq!(
        s[1].start,
        SectionStart::NextPage,
        "the copy keeps the old type"
    );
    assert_eq!(s[2].start, SectionStart::Continuous);
}

#[test]
fn a_section_break_is_refused_in_a_header() {
    let mut t = three_sections();
    assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
    let before = ed(&t).doc.clone();
    let err = layout_apply(
        &mut t,
        LayoutAct::Break(BreakChoice::Section(SectionStart::NextPage)),
    )
    .unwrap_err();
    assert!(err.contains("header"), "{err}");
    assert_eq!(ed(&t).doc, before);
    // A page break goes into the header being edited, not the body.
    apply(&mut t, LayoutAct::Break(BreakChoice::Page));
    assert_eq!(ed(&t).doc, before);
    let hf = &t.hf_edit.as_ref().unwrap().editor.doc;
    assert!(format!("{hf:?}").contains("Page"));
    // Section commands still act on the body caret's section.
    apply(&mut t, LayoutAct::Orient(true));
    assert!(setups(&t)[1].page.landscape);
}

#[test]
fn suppress_line_numbers_and_hyphenation() {
    let mut t = three_sections();
    assert!(!layout_checked(&t, LayoutAct::SuppressLineNumbers));
    apply(&mut t, LayoutAct::SuppressLineNumbers);
    assert!(layout_checked(&t, LayoutAct::SuppressLineNumbers));
    apply(&mut t, LayoutAct::SuppressLineNumbers);
    assert!(!layout_checked(&t, LayoutAct::SuppressLineNumbers));
    assert!(layout_checked(&t, LayoutAct::Hyphen(false)));
    apply(&mut t, LayoutAct::Hyphen(true));
    assert!(t.pkg.as_ref().unwrap().has_auto_hyphenation());
    assert!(layout_checked(&t, LayoutAct::Hyphen(true)));
    apply(&mut t, LayoutAct::Hyphen(false));
    assert!(!t.pkg.as_ref().unwrap().has_auto_hyphenation());
}

#[test]
fn everything_survives_save_and_reopen() {
    let mut t = three_sections();
    apply(&mut t, LayoutAct::Orient(true));
    apply(&mut t, LayoutAct::Paper(Paper::A4));
    apply(&mut t, LayoutAct::Columns(ColumnsPreset::Left));
    apply(&mut t, LayoutAct::LineNumbers(LnChoice::Continuous));
    apply(&mut t, LayoutAct::SuppressLineNumbers);
    apply(&mut t, LayoutAct::Margins(MarginPreset::Mirrored));
    apply(&mut t, LayoutAct::Hyphen(true));
    apply(&mut t, LayoutAct::Break(BreakChoice::TextWrapping));
    ed_mut(&mut t).caret = Caret::top(3, 2);
    apply(
        &mut t,
        LayoutAct::Break(BreakChoice::Section(SectionStart::OddPage)),
    );
    let want = ed(&t).sections();
    let bytes = doc_to_docx(&ed(&t).doc, &t.comments, t.pkg.as_ref());
    let pkg = docxcore::package::load_package(&bytes).unwrap();
    assert!(pkg.has_mirror_margins() && pkg.has_auto_hyphenation());
    let reopened = Editor::new(pkg.document.clone());
    let got = reopened.sections();
    assert_eq!(got.len(), 4);
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(SectionSetup::parse(g), SectionSetup::parse(w));
    }
    let s = SectionSetup::parse(&got[1]);
    assert!(s.page.landscape && s.page.code == Some(9));
    assert!(!s.columns.equal_width());
    assert_eq!(s.line_numbers.unwrap().restart, LnRestart::Continuous);
    assert_eq!(SectionSetup::parse(&got[3]).start, SectionStart::OddPage);
    let Block::Paragraph(p) = &reopened.doc.body[1] else {
        panic!()
    };
    assert!(
        p.props
            .raw_props
            .iter()
            .any(|r| r.contains("suppressLineNumbers"))
    );
    assert!(p.content.contains(&Inline::Break(
        BreakKind::Clear(ClearKind::All),
        RunProps::default()
    )));
}

#[test]
fn menus_follow_word_with_headings_separators_and_placeholders() {
    let labels = |m| -> Vec<String> {
        menu_items(m, |_| false)
            .iter()
            .map(|i| match i {
                menu::MenuItem::Item(e) if !e.enabled => format!("({})", e.label),
                menu::MenuItem::Item(e) => e.label.clone(),
                menu::MenuItem::Separator => "-".into(),
                menu::MenuItem::Heading(h) => format!("# {h}"),
                menu::MenuItem::TableGrid { .. } => "[grid]".into(),
            })
            .collect()
    };
    assert_eq!(
        labels(LayoutMenu::Margins),
        [
            "Normal",
            "Narrow",
            "Moderate",
            "Wide",
            "Mirrored",
            "-",
            "Custom Margins..."
        ]
    );
    assert_eq!(
        labels(LayoutMenu::Breaks),
        [
            "# Page Breaks",
            "Page",
            "Column",
            "Text Wrapping",
            "# Section Breaks",
            "Next Page",
            "Continuous",
            "Even Page",
            "Odd Page"
        ]
    );
    assert_eq!(
        labels(LayoutMenu::LineNumbers),
        [
            "None",
            "Continuous",
            "Restart Each Page",
            "Restart Each Section",
            "-",
            "Suppress for Current Paragraph",
            "-",
            "(Line Numbering Options...)"
        ]
    );
    assert_eq!(
        labels(LayoutMenu::Hyphenation),
        [
            "None",
            "Automatic",
            "(Manual)",
            "-",
            "(Hyphenation Options...)"
        ]
    );
    assert_eq!(
        labels(LayoutMenu::Size).last().unwrap(),
        "More Paper Sizes..."
    );
    assert_eq!(labels(LayoutMenu::Size)[5], "B5 (JIS)");
    assert_eq!(
        labels(LayoutMenu::Columns).last().unwrap(),
        "More Columns..."
    );
    assert_eq!(labels(LayoutMenu::Orientation), ["Portrait", "Landscape"]);
}

#[test]
fn the_tab_sits_between_insert_and_review_for_documents_only() {
    let names = |k| {
        ribbon_for(k)
            .tabs
            .iter()
            .map(|t| t.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(Kind::Docx),
        ["Home", "Insert", "Layout", "Review", "View"]
    );
    assert_eq!(names(Kind::Xlsx), ["Home", "Insert", "Review", "View"]);
    for kind in [Kind::Docx, Kind::Xlsx] {
        let set: Vec<&str> = ribbon_tab_set(kind)[1..].iter().map(|t| t.1).collect();
        assert_eq!(set, names(kind), "tab set and ribbon agree");
    }
    let insert = docxy_ribbon().tabs.swap_remove(1);
    assert_eq!(insert.name, "Insert");
    assert!(insert.groups.iter().all(|g| g.title != "Layout"));
}

#[test]
fn keytips_open_the_menus_and_take_two_letters() {
    let tab = layout_tab();
    assert_eq!(tab.key_tip, "P");
    let menu = |k| match tab_keytip_cmd(&tab, k) {
        Some(Act::Layout(LayoutAct::Menu(m))) => Some(m),
        _ => None,
    };
    assert_eq!(menu("O"), Some(LayoutMenu::Orientation));
    assert_eq!(menu("M"), Some(LayoutMenu::Margins));
    assert_eq!(menu("J"), Some(LayoutMenu::Columns));
    assert_eq!(menu("B"), Some(LayoutMenu::Breaks));
    assert_eq!(menu("H"), Some(LayoutMenu::Hyphenation));
    assert_eq!(menu("S"), None);
    assert!(tab_keytip_starts(&tab, "S"));
    assert_eq!(menu("sz"), Some(LayoutMenu::Size));
    assert!(tab_keytip_starts(&tab, "L"));
    assert_eq!(menu("LN"), Some(LayoutMenu::LineNumbers));
    assert!(!tab_keytip_starts(&tab, "SZ"));
    assert!(!tab_keytip_starts(&tab, "Q"));
    assert!(matches!(
        tab.groups[0].launcher,
        Some(Act::Layout(LayoutAct::PageSetup(PageSetupTab::Margins)))
    ));
}

#[test]
fn a_markdown_tab_refuses_section_commands() {
    let mut t = three_sections();
    t.pkg = None;
    let err = layout_apply(&mut t, LayoutAct::Orient(true)).unwrap_err();
    assert!(err.contains(".docx"), "{err}");
}
