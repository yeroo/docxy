use super::*;
use crate::layout_tab::tests::{ed, ed_mut, setups, three_sections};
use core::prelude::v1::test;
use ctlcore::json::Json;

fn open(t: &mut DocTab, page: PageSetupTab) {
    let d = page_setup_dialog(t, page).unwrap();
    t.dialogs.push(d);
}

fn set(t: &mut DocTab, control: &str, value: Json) {
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap_or_else(|e| panic!("{control}: {e}"));
}

fn s(v: &str) -> Json {
    Json::Str(v.into())
}

fn ok(t: &mut DocTab) -> Result<(), String> {
    crate::dialog_host::dialog_click(t, "OK")
}

fn items(t: &DocTab, name: &str) -> Vec<String> {
    let d = t.dialogs.top().unwrap();
    d.controls
        .iter()
        .find(|c| c.name == name)
        .unwrap()
        .items
        .clone()
}

fn shown(t: &DocTab, name: &str) -> String {
    let d = t.dialogs.top().unwrap();
    d.controls.iter().find(|c| c.name == name).unwrap().text()
}

/// Give each section its own left margin: 1", 1.5", 2".
fn distinct_margins(t: &mut DocTab) {
    let ed = ed_mut(t);
    for (k, left) in [(0, 1440), (1, 2160), (2, 2880)] {
        ed.edit_section_setups(&[k], |s| s.margins.left = left);
    }
}

#[test]
fn it_opens_on_the_caret_sections_values() {
    let mut t = three_sections();
    ed_mut(&mut t).edit_section_setups(&[1], |s| {
        s.margins.top = 1800;
        s.margins.gutter = 360;
        s.start = SectionStart::Continuous;
        s.page.set_paper(Paper::A4);
    });
    open(&mut t, PageSetupTab::Margins);
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.id, "page-setup");
    assert_eq!(d.tabs, ["Margins", "Paper", "Layout"]);
    assert_eq!(d.tab, 0);
    assert_eq!(shown(&t, "top"), "1.25");
    assert_eq!(shown(&t, "gutter"), "0.25");
    assert_eq!(shown(&t, "left"), "1");
    assert_eq!(shown(&t, "orientation"), "Portrait");
    assert_eq!(shown(&t, "paper"), "A4");
    assert_eq!(shown(&t, "width"), "8.27");
    assert_eq!(shown(&t, "start"), "Continuous");
    assert_eq!(shown(&t, "header"), "0.5");
    assert_eq!(
        items(&t, "apply"),
        ["This section", "This point forward", "Whole document"]
    );
    assert_eq!(shown(&t, "apply"), "This section");
    // Size's More Paper Sizes... opens it on the Paper tab.
    let d = page_setup_dialog(&t, PageSetupTab::Paper).unwrap();
    assert_eq!(d.tab, 1);
}

#[test]
fn apply_to_follows_the_document_and_the_selection() {
    let mut t = three_sections();
    ed_mut(&mut t).anchor = Some(Caret::top(0, 0));
    open(&mut t, PageSetupTab::Margins);
    assert_eq!(
        items(&t, "apply"),
        ["Selected sections", "This point forward", "Whole document"]
    );
    // A one-section document defaults to Whole document.
    let mut t = three_sections();
    let ed = ed_mut(&mut t);
    ed.doc.body.truncate(2);
    if let Block::Paragraph(p) = &mut ed.doc.body[0] {
        p.props.section_break = None;
    }
    open(&mut t, PageSetupTab::Margins);
    assert_eq!(items(&t, "apply"), ["Whole document", "This point forward"]);
    assert_eq!(shown(&t, "apply"), "Whole document");
}

#[test]
fn ok_on_this_section_changes_it_alone_in_one_undo_step() {
    let mut t = three_sections();
    let before = ed(&t).doc.clone();
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "top", s("0.5"));
    set(&mut t, "Right", Json::Num(1.5));
    ok(&mut t).unwrap();
    assert!(!t.dialogs.is_open());
    assert!(t.dirty);
    let s = setups(&t);
    assert_eq!((s[1].margins.top, s[1].margins.right), (720, 2160));
    assert_eq!(s[0].margins.top, 1440);
    assert_eq!(s[2].margins.top, 1440);
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).doc, before);
}

/// Whole document with only Orientation changed rotates each section's own
/// margins; it does not copy the caret section's onto the others.
#[test]
fn whole_document_writes_only_the_changed_fields() {
    let mut t = three_sections();
    distinct_margins(&mut t);
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "orientation", s("Landscape"));
    // The dialog turns the page it shows.
    assert_eq!(
        (shown(&t, "width"), shown(&t, "height")),
        ("11".into(), "8.5".into())
    );
    assert_eq!(
        shown(&t, "top"),
        "1.5",
        "the caret section's left is now its top"
    );
    set(&mut t, "apply", s("Whole document"));
    ok(&mut t).unwrap();
    let s = setups(&t);
    for (k, left) in [(0, 1440), (1, 2160), (2, 2880)] {
        assert!(s[k].page.landscape, "section {k}");
        assert_eq!(
            s[k].margins.top, left,
            "section {k} kept its own margin, rotated"
        );
        assert_eq!((s[k].page.w, s[k].page.h), (15840, 12240));
    }
    assert!(ed_mut(&mut t).undo());
    assert!(setups(&t).iter().all(|s| !s.page.landscape), "one step");
}

/// A margin typed before Orientation changes goes where the dialog shows it
/// (r1 M1): its changed-ness turns with the page.
#[test]
fn a_typed_margin_turns_with_the_page() {
    // One section: Top 2", then Landscape; the dialog shows Right 2".
    let mut t = three_sections();
    let ed = ed_mut(&mut t);
    ed.doc.body.truncate(2);
    if let Block::Paragraph(p) = &mut ed.doc.body[0] {
        p.props.section_break = None;
    }
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "top", s("2"));
    set(&mut t, "orientation", s("Landscape"));
    let shown_sides = ["top", "right", "bottom", "left"].map(|n| shown(&t, n));
    assert_eq!(shown_sides, ["1", "2", "1", "1"]);
    ok(&mut t).unwrap();
    let m = setups(&t)[0].margins;
    assert_eq!((m.top, m.right, m.bottom, m.left), (1440, 2880, 1440, 1440));

    // Three sections with their own left margins, Whole document: each turns
    // its own margins, and the typed one lands on every section's right.
    let mut t = three_sections();
    distinct_margins(&mut t);
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "top", s("2"));
    set(&mut t, "orientation", s("Landscape"));
    set(&mut t, "apply", s("Whole document"));
    ok(&mut t).unwrap();
    for (k, left) in [(0, 1440), (1, 2160), (2, 2880)] {
        let m = setups(&t)[k].margins;
        assert_eq!((m.top, m.right), (left, 2880), "section {k}");
    }
}

/// A one-section document, the caret in it.
fn one_section() -> DocTab {
    let mut t = three_sections();
    let ed = ed_mut(&mut t);
    ed.doc.body.truncate(2);
    if let Block::Paragraph(p) = &mut ed.doc.body[0] {
        p.props.section_break = None;
    }
    t
}

/// The four margins the dialog shows, in twips.
fn shown_margins(t: &DocTab) -> [i32; 4] {
    ["top", "right", "bottom", "left"].map(|n| twips_of(&shown(t, n)).unwrap())
}

fn section_margins(t: &DocTab) -> [i32; 4] {
    let m = setups(t)[0].margins;
    [m.top, m.right, m.bottom, m.left]
}

/// Give the one section margins that differ on every side.
fn lopsided(t: &mut DocTab) {
    ed_mut(t).edit_section_setups(&[0], |s| {
        (
            s.margins.top,
            s.margins.right,
            s.margins.bottom,
            s.margins.left,
        ) = (720, 1080, 1440, 1800)
    });
}

/// r2 M1: OK writes the margins the dialog shows, however Orientation was
/// reached: the dialog and `SectionSetup::set_landscape` turn the page by one
/// rule (the choice changed), whatever the sides say.
#[test]
fn ok_writes_the_margins_the_dialog_shows_after_orientation() {
    // Sides typed first, already landscape-shaped, then Landscape.
    let mut t = one_section();
    lopsided(&mut t);
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "width", s("11"));
    set(&mut t, "height", s("8.5"));
    t.dialogs.select_tab("Margins").unwrap();
    set(&mut t, "orientation", s("Landscape"));
    assert_eq!(
        (shown(&t, "width"), shown(&t, "height")),
        ("11".into(), "8.5".into())
    );
    let want = shown_margins(&t);
    assert_eq!(want, [1800, 720, 1080, 1440], "the margins turned");
    ok(&mut t).unwrap();
    assert_eq!(section_margins(&t), want);
    assert!(setups(&t)[0].page.landscape);
    assert_eq!((setups(&t)[0].page.w, setups(&t)[0].page.h), (15840, 12240));

    // A square page: no swap, the margins still turn.
    let mut t = one_section();
    lopsided(&mut t);
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "width", s("10"));
    set(&mut t, "height", s("10"));
    t.dialogs.select_tab("Margins").unwrap();
    set(&mut t, "orientation", s("Landscape"));
    let want = shown_margins(&t);
    assert_eq!(want, [1800, 720, 1080, 1440]);
    ok(&mut t).unwrap();
    assert_eq!(section_margins(&t), want);

    // Width emptied, then Landscape: the margins turn all the same, and the
    // sides swap by the section's own shape, so the empty side is now the
    // height. OK refuses it, and with sides again writes what is shown.
    let mut t = one_section();
    lopsided(&mut t);
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "width", s(""));
    t.dialogs.select_tab("Margins").unwrap();
    set(&mut t, "orientation", s("Landscape"));
    let want = shown_margins(&t);
    assert_eq!(want, [1800, 720, 1080, 1440]);
    assert_eq!(
        (shown(&t, "width"), shown(&t, "height")),
        ("11".into(), "".into())
    );
    assert_eq!(ok(&mut t).unwrap_err(), "Height: takes a number");
    t.dialogs.select_tab("Paper").unwrap();
    set(&mut t, "height", s("8.5"));
    ok(&mut t).unwrap();
    assert_eq!(section_margins(&t), want);

    // A landscape-shaped pgSz without w:orient opens as Portrait; Landscape
    // keeps its sides and turns its margins.
    let mut t = one_section();
    lopsided(&mut t);
    ed_mut(&mut t).edit_section_setups(&[0], |s| (s.page.w, s.page.h) = (15840, 12240));
    open(&mut t, PageSetupTab::Margins);
    assert_eq!(shown(&t, "orientation"), "Portrait");
    set(&mut t, "orientation", s("Landscape"));
    assert_eq!(
        (shown(&t, "width"), shown(&t, "height")),
        ("11".into(), "8.5".into())
    );
    let want = shown_margins(&t);
    assert_eq!(want, [1800, 720, 1080, 1440]);
    ok(&mut t).unwrap();
    assert_eq!(section_margins(&t), want);
    assert_eq!((setups(&t)[0].page.w, setups(&t)[0].page.h), (15840, 12240));

    // Choosing the orientation it already has turns nothing; there and back
    // is where it started.
    let mut t = one_section();
    lopsided(&mut t);
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "orientation", s("Portrait"));
    assert_eq!(shown_margins(&t), [720, 1080, 1440, 1800]);
    set(&mut t, "orientation", s("Landscape"));
    set(&mut t, "orientation", s("Portrait"));
    assert_eq!(shown_margins(&t), [720, 1080, 1440, 1800]);
    t.dirty = false;
    ok(&mut t).unwrap();
    assert!(!t.dirty, "nothing changed");
}

/// Section 0 a narrow 4" page, the caret in section 1 (Letter), and a
/// forward selection from section 0 into it: This point forward breaks where
/// the selection starts, in section 0.
fn narrow_first_section_and_a_forward_selection() -> DocTab {
    let mut t = three_sections();
    ed_mut(&mut t).edit_section_setups(&[0], |s| s.page.set_custom(5760, 15840));
    let ed = ed_mut(&mut t);
    ed.anchor = Some(Caret::top(0, 1));
    ed.caret = Caret::top(1, 1);
    assert_eq!(ed.break_section(), Ok(0));
    t
}

/// r3 M1: This point forward checks the section the break lands in, not the
/// caret's.
#[test]
fn this_point_forward_checks_the_section_the_break_lands_in() {
    let mut t = narrow_first_section_and_a_forward_selection();
    let before = ed(&t).doc.clone();
    open(&mut t, PageSetupTab::Margins);
    // 3" + 2" fits the caret's 8.5" page, not section 0's 4" one.
    set(&mut t, "left", s("3"));
    set(&mut t, "right", s("2"));
    set(&mut t, "apply", s("This point forward"));
    assert_eq!(
        ok(&mut t).unwrap_err(),
        "The margins leave no room for text on the page"
    );
    assert!(t.dialogs.is_open());
    assert_eq!(ed(&t).doc, before, "nothing changed");

    let mut t = narrow_first_section_and_a_forward_selection();
    let before = ed(&t).doc.clone();
    open_columns(&mut t);
    // Two equal columns across the caret section's 6.5" of text do not fit
    // section 0's 2" of text.
    set(&mut t, "Presets", s("Two"));
    set(&mut t, "apply", s("This point forward"));
    assert_eq!(
        ok(&mut t).unwrap_err(),
        "The columns take 6.5\" but the text is 2\" wide"
    );
    assert_eq!(ed(&t).doc, before, "nothing changed");
}

/// r3 m3: Height cleared, then Landscape, then the sides typed again: OK
/// writes the sides the dialog shows.
#[test]
fn a_cleared_side_turns_with_the_page_by_the_sections_shape() {
    let mut t = one_section();
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "height", s(""));
    t.dialogs.select_tab("Margins").unwrap();
    set(&mut t, "orientation", s("Landscape"));
    // The section is portrait-shaped, so the sides swap as set_landscape
    // will swap them: the empty side is now the width.
    assert_eq!(
        (shown(&t, "width"), shown(&t, "height")),
        ("".into(), "8.5".into())
    );
    t.dialogs.select_tab("Paper").unwrap();
    set(&mut t, "width", s("11"));
    ok(&mut t).unwrap();
    let p = setups(&t)[0].page;
    assert_eq!((p.w, p.h, p.landscape), (15840, 12240, true));
    // Retyping 11 into Height instead leaves the width empty: OK refuses
    // rather than writing sides the dialog does not show.
    let mut t = one_section();
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "height", s(""));
    t.dialogs.select_tab("Margins").unwrap();
    set(&mut t, "orientation", s("Landscape"));
    t.dialogs.select_tab("Paper").unwrap();
    set(&mut t, "height", s("11"));
    assert_eq!(ok(&mut t).unwrap_err(), "Width: takes a number");
}

#[test]
fn this_point_forward_starts_a_new_section_at_the_caret() {
    let mut t = three_sections();
    let before = ed(&t).doc.clone();
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "orientation", s("Landscape"));
    set(&mut t, "apply", s("This point forward"));
    ok(&mut t).unwrap();
    let s = setups(&t);
    assert_eq!(s.len(), 4);
    assert!(!s[0].page.landscape && !s[1].page.landscape);
    assert!(s[2].page.landscape, "the section from the caret");
    assert_eq!(s[2].start, SectionStart::NextPage);
    assert!(!s[3].page.landscape);
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).doc, before, "the break and the setup undo together");
}

#[test]
fn out_of_range_values_refuse_ok_and_keep_the_dialog() {
    let cases: [(&str, &str, &str); 5] = [
        ("top", "-0.5", "Top cannot be negative"),
        (
            "left",
            "4",
            "The margins leave no room for text on the page",
        ),
        (
            "width",
            "0.05",
            "The paper must be between 0.1 and 22 inches on each side",
        ),
        (
            "height",
            "23",
            "The paper must be between 0.1 and 22 inches on each side",
        ),
        ("bottom", "", "Bottom: takes a number"),
    ];
    for (name, value, err) in cases {
        let mut t = three_sections();
        let before = ed(&t).doc.clone();
        open(&mut t, PageSetupTab::Margins);
        if name == "left" {
            set(&mut t, "right", s("4.5"));
        }
        if matches!(name, "width" | "height") {
            t.dialogs.select_tab("Paper").unwrap();
        }
        set(&mut t, name, s(value));
        assert_eq!(ok(&mut t).unwrap_err(), err, "{name}={value}");
        assert!(t.dialogs.is_open(), "{name}: the dialog stays open");
        assert_eq!(shown(&t, name), value, "{name}: the staged value stays");
        assert_eq!(ed(&t).doc, before, "{name}: nothing written");
    }
}

#[test]
fn paper_size_width_and_height_follow_each_other() {
    let mut t = three_sections();
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "paper", s("A4"));
    assert_eq!(
        (shown(&t, "width"), shown(&t, "height")),
        ("8.27".into(), "11.69".into())
    );
    ok(&mut t).unwrap();
    let p = setups(&t)[1].page;
    assert_eq!((p.w, p.h, p.code), (11906, 16838, Some(9)));
    // Editing a side makes it Custom, and the paper code goes.
    open(&mut t, PageSetupTab::Paper);
    assert_eq!(shown(&t, "paper"), "A4");
    set(&mut t, "width", s("7"));
    assert_eq!(shown(&t, "paper"), "Custom");
    ok(&mut t).unwrap();
    let p = setups(&t)[1].page;
    assert_eq!((p.w, p.code), (10080, None));
    // Sides that match a named size name it.
    open(&mut t, PageSetupTab::Paper);
    set(&mut t, "width", s("8.5"));
    set(&mut t, "height", s("14"));
    assert_eq!(shown(&t, "paper"), "Legal");
}

#[test]
fn settings_and_the_layout_tab_are_written() {
    let mut t = three_sections();
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "gutter", s("0.5"));
    set(&mut t, "Gutter position", s("Top"));
    set(&mut t, "Multiple pages", s("Mirror margins"));
    t.dialogs.select_tab("Layout").unwrap();
    set(&mut t, "Section start", s("Odd page"));
    set(&mut t, "Header from edge", s("0.3"));
    ok(&mut t).unwrap();
    let pkg = t.pkg.as_ref().unwrap();
    assert!(pkg.has_gutter_at_top() && pkg.has_mirror_margins());
    let sec = &setups(&t)[1];
    assert_eq!(sec.margins.gutter, 720);
    assert_eq!(sec.start, SectionStart::OddPage);
    assert_eq!(sec.margins.header, 432);
    // An OK that changes nothing leaves the tab clean.
    t.dirty = false;
    open(&mut t, PageSetupTab::Margins);
    ok(&mut t).unwrap();
    assert!(!t.dirty, "an untouched Page Setup changed nothing");
    open_columns(&mut t);
    ok(&mut t).unwrap();
    assert!(!t.dirty, "an untouched Columns changed nothing");
    // Cancel changes nothing.
    let before = ed(&t).doc.clone();
    open(&mut t, PageSetupTab::Margins);
    set(&mut t, "top", s("3"));
    crate::dialog_host::dialog_click(&mut t, "Cancel").unwrap();
    assert_eq!(ed(&t).doc, before);
}

#[test]
fn a_markdown_tab_has_no_page_setup() {
    let mut t = three_sections();
    t.pkg = None;
    let err = page_setup_dialog(&t, PageSetupTab::Margins).unwrap_err();
    assert!(err.contains(".docx"), "{err}");
}

// ---- Columns ----------------------------------------------------------------

fn open_columns(t: &mut DocTab) {
    let d = columns_dialog(t).unwrap();
    t.dialogs.push(d);
}

fn visible(t: &DocTab) -> Vec<&'static str> {
    t.dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .filter(|c| c.visible && (c.name.starts_with("width") || c.name.starts_with("space")))
        .map(|c| c.name)
        .collect()
}

fn enabled(t: &DocTab, name: &str) -> bool {
    let d = t.dialogs.top().unwrap();
    d.controls.iter().find(|c| c.name == name).unwrap().enabled
}

#[test]
fn columns_opens_on_the_caret_sections_columns() {
    let mut t = three_sections();
    open_columns(&mut t);
    let d = t.dialogs.top().unwrap();
    assert_eq!((d.id, d.title.as_str()), ("columns", "Columns"));
    assert_eq!(shown(&t, "preset"), "One");
    assert_eq!(shown(&t, "num"), "1");
    assert_eq!(shown(&t, "width1"), "6.5");
    assert_eq!(visible(&t), ["width1"]);
    assert_eq!(shown(&t, "equal"), "checked");
    assert_eq!(shown(&t, "sep"), "unchecked");
    assert_eq!(
        items(&t, "apply"),
        ["This section", "This point forward", "Whole document"]
    );
}

#[test]
fn a_preset_and_line_between_apply_to_this_section_in_one_step() {
    let mut t = three_sections();
    let before = ed(&t).doc.clone();
    open_columns(&mut t);
    set(&mut t, "Presets", s("Two"));
    assert_eq!(shown(&t, "num"), "2");
    assert_eq!(visible(&t), ["width1", "space1", "width2"]);
    assert_eq!(shown(&t, "width1"), "3");
    assert!(!enabled(&t, "width2"), "equal columns follow column 1");
    set(&mut t, "Line between", Json::Bool(true));
    ok(&mut t).unwrap();
    let s = setups(&t);
    let c = &s[1].columns;
    assert_eq!(
        (c.count(), c.space, c.sep, c.equal_width()),
        (2, 720, true, true)
    );
    assert_eq!(s[0].columns.count(), 1);
    assert_eq!(s[2].columns.count(), 1);
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).doc, before);
}

#[test]
fn left_and_right_presets_write_unequal_widths() {
    let mut t = three_sections();
    open_columns(&mut t);
    set(&mut t, "Presets", s("Left"));
    assert_eq!(
        (
            shown(&t, "width1"),
            shown(&t, "space1"),
            shown(&t, "width2")
        ),
        ("1.83".into(), "0.5".into(), "4.17".into())
    );
    assert_eq!(shown(&t, "equal"), "unchecked");
    ok(&mut t).unwrap();
    let c = setups(&t)[1].columns.clone();
    assert_eq!(c.cols.iter().map(|c| c.w).collect::<Vec<_>>(), [2635, 6005]);
    assert!(!c.equal_width());
}

#[test]
fn the_number_and_column_one_spread_equal_columns() {
    let mut t = three_sections();
    open_columns(&mut t);
    set(&mut t, "Number of columns", Json::Num(4.0));
    assert_eq!(visible(&t).len(), 7, "4 widths and 3 spacings");
    assert_eq!(shown(&t, "width3"), "1.25");
    assert_eq!(shown(&t, "preset"), "", "no preset has four columns");
    set(&mut t, "Number of columns", Json::Num(3.0));
    assert_eq!(shown(&t, "preset"), "Three");
    // With equal columns, column 1's width sets the spacing and every width.
    set(&mut t, "Width 1", s("2"));
    assert_eq!(
        (shown(&t, "space1"), shown(&t, "width3")),
        ("0.25".into(), "2".into())
    );
    ok(&mut t).unwrap();
    let c = &setups(&t)[1].columns;
    assert_eq!((c.count(), c.space, c.equal_width()), (3, 360, true));
}

#[test]
fn unequal_widths_must_fit_the_text_width() {
    let mut t = three_sections();
    open_columns(&mut t);
    set(&mut t, "Number of columns", Json::Num(2.0));
    set(&mut t, "Equal column width", Json::Bool(false));
    assert!(enabled(&t, "width2"));
    set(&mut t, "Width 1", s("2"));
    set(&mut t, "Width 2", s("3"));
    assert_eq!(shown(&t, "preset"), "Left");
    let before = ed(&t).doc.clone();
    assert_eq!(
        ok(&mut t).unwrap_err(),
        "The columns take 5.5\" but the text is 6.5\" wide"
    );
    assert!(t.dialogs.is_open());
    assert_eq!(ed(&t).doc, before);
    set(&mut t, "Width 2", s("4"));
    ok(&mut t).unwrap();
    let c = &setups(&t)[1].columns;
    assert_eq!(
        c.cols.iter().map(|c| (c.w, c.space)).collect::<Vec<_>>(),
        [(2880, 720), (5760, 0)]
    );
    // Too many columns refuses.
    open_columns(&mut t);
    set(&mut t, "Number of columns", Json::Num(13.0));
    assert_eq!(
        ok(&mut t).unwrap_err(),
        "Number of columns must be a whole number from 1 to 12"
    );
}

#[test]
fn this_point_forward_starts_a_continuous_section() {
    let mut t = three_sections();
    let before = ed(&t).doc.clone();
    open_columns(&mut t);
    set(&mut t, "Presets", s("Three"));
    set(&mut t, "apply", s("This point forward"));
    ok(&mut t).unwrap();
    let s = setups(&t);
    assert_eq!(s.len(), 4);
    assert_eq!(s[1].columns.count(), 1);
    assert_eq!(s[2].columns.count(), 3);
    assert_eq!(s[2].start, SectionStart::Continuous);
    assert!(ed_mut(&mut t).undo());
    assert_eq!(ed(&t).doc, before);
}

#[test]
fn whole_document_with_only_line_between_keeps_each_sections_columns() {
    let mut t = three_sections();
    ed_mut(&mut t).edit_section_setups(&[2], |s| s.columns = s.columns.equal(3, 720));
    open_columns(&mut t);
    set(&mut t, "Line between", Json::Bool(true));
    set(&mut t, "apply", s("Whole document"));
    ok(&mut t).unwrap();
    let s = setups(&t);
    assert_eq!(
        s.iter()
            .map(|s| (s.columns.count(), s.columns.sep))
            .collect::<Vec<_>>(),
        [(1, true), (1, true), (3, true)]
    );
}
