use super::*;
use crate::{Kind, SheetView, new_sheet_surface};
use ctlcore::json::Json;
use gridcore::sheet::Cell;

fn tab() -> DocTab {
    DocTab {
        kind: Kind::Xlsx,
        title: "Book.xlsx".into(),
        path: None,
        surface: new_sheet_surface(),
        dirty: false,
        status: "".into(),
        comments: vec![],
        tracked_comment_ids: Default::default(),
        comment_undo: Default::default(),
        comments_removed_all: false,
        used_comment_ids: Default::default(),
        pictures: Default::default(),
        run_styles: Default::default(),
        pkg: None,
        notes: vec![],
        markdown: false,
        hf_edit: None,
        bundle_html: None,
        load_failed: false,
        dialogs: crate::dialog::DialogStack::default(),
        access: crate::open_mode::Access::default(),
        last_hot: Default::default(),
        converted_docx: None,
        pending_conversion: false,
        mail: Default::default(),
        field_codes: false,
        import: Default::default(),
    }
}

fn view(t: &mut DocTab) -> &mut SheetView {
    match &mut t.surface {
        Surface::Sheet(v) => v,
        _ => panic!("a sheet"),
    }
}

fn at(name: &str) -> (u32, u32) {
    gridcore::sheet::parse_cell_name(name).unwrap()
}

/// `name` holds `value` in the style `xf`.
fn put(t: &mut DocTab, name: &str, value: CellValue, xf: Xf) {
    let (r, c) = at(name);
    let v = view(t);
    let style = v.pkg.workbook.styles.intern(xf);
    let s = v.active;
    v.engine.set_cell(
        &mut v.pkg.workbook,
        (s, r, c),
        Cell {
            value,
            style,
            ..Cell::default()
        },
    );
}

fn coded(code: &str) -> Xf {
    let mut xf = Xf::default();
    xf.set_code(Some(code.into()));
    xf
}

/// Select `from` (the active cell) through `to`.
fn select(t: &mut DocTab, from: &str, to: &str) {
    let v = view(t);
    v.sel = at(from);
    v.anchor = at(to);
}

fn xf_at(t: &mut DocTab, name: &str) -> Xf {
    let (r, c) = at(name);
    let v = view(t);
    v.pkg
        .workbook
        .styles
        .xf(v.sheet().cell(r, c).map_or(0, |cl| cl.style))
}

fn shows(t: &mut DocTab, name: &str) -> String {
    let (r, c) = at(name);
    view(t).cell_text(r, c)
}

fn open_on(t: &mut DocTab) {
    open(t).unwrap();
}

/// Set `control`, on its own tab.
fn set(t: &mut DocTab, control: &str, value: Json) {
    let d = t.dialogs.top_dialog_mut().expect("Format Cells is up");
    let page = d
        .controls
        .iter()
        .find(|c| c.name == control)
        .and_then(|c| c.page);
    if let Some(p) = page {
        let label = d.tabs[p].clone();
        d.select_tab(&label).unwrap();
    }
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap();
}

fn s(v: &str) -> Json {
    Json::Str(v.into())
}

fn top(t: &DocTab) -> &Dialog {
    t.dialogs.top().expect("Format Cells is up")
}

fn read(t: &DocTab, name: &str) -> String {
    top(t)
        .controls
        .iter()
        .find(|c| c.name == name)
        .map(Control::text)
        .unwrap_or_default()
}

fn visible(t: &DocTab, name: &str) -> bool {
    top(t).controls.iter().any(|c| c.name == name && c.visible)
}

fn press(t: &mut DocTab, button: &str) -> Result<(), String> {
    crate::dialog_host::dialog_click(t, button)
}

fn undo_depth(t: &mut DocTab) -> usize {
    view(t).undo.len()
}

fn boxes_of(category: usize) -> NumberBoxes {
    NumberBoxes {
        category,
        ..NumberBoxes::default()
    }
}

// ---- the code core ---------------------------------------------------------

/// #1165: the issue's codes open on the boxes that make them.
#[test]
fn a_cells_code_opens_on_its_categorys_boxes() {
    let b = decompose(Some("#,##0.00"));
    assert_eq!(
        (b.category, b.decimals, b.thousands, b.negative),
        (NUMBER, 2, true, 0)
    );
    let b = decompose(Some("$#,##0.00_);[Red]($#,##0.00)"));
    assert_eq!(
        (b.category, b.decimals, b.symbol, b.negative),
        (CURRENCY, 2, 1, 3)
    );
    let b = decompose(Some("0.0%"));
    assert_eq!((b.category, b.decimals), (PERCENTAGE, 1));
    let b = decompose(Some("\"$\"#,##0"));
    assert_eq!((b.category, b.decimals, b.symbol), (CURRENCY, 0, 1));
    let b = decompose(Some("[$€-2]\\ #,##0.00"));
    assert_eq!((b.category, b.symbol), (CURRENCY, 2));
    let b = decompose(Some("0.00E+00"));
    assert_eq!((b.category, b.decimals), (SCIENTIFIC, 2));
    let b = decompose(Some("# ??/??"));
    assert_eq!((b.category, b.fraction), (FRACTION, 1));
    assert_eq!(decompose(Some("@")).category, TEXT);
    assert_eq!(decompose(None).category, GENERAL);
    assert_eq!(decompose(Some("General")).category, GENERAL);
    assert_eq!(decompose(None).custom, "General");
    // A code no category builds opens as Custom, the code in its Type box.
    let b = decompose(Some("0.0 \"kg\""));
    assert_eq!((b.category, b.custom.as_str()), (CUSTOM, "0.0 \"kg\""));
    // Every box keeps the cell's code for Custom.
    assert_eq!(decompose(Some("0.0%")).custom, "0.0%");
}

/// #1165: Excel's built-ins 37–40 (no `_)`) are Number with the separator,
/// as the ribbon names them.
#[test]
fn the_builtin_parenthesis_codes_are_number() {
    for (code, dec, neg) in [
        ("#,##0;(#,##0)", 0, 2),
        ("#,##0;[Red](#,##0)", 0, 3),
        ("#,##0.00;(#,##0.00)", 2, 2),
        ("#,##0.00;[Red](#,##0.00)", 2, 3),
    ] {
        let b = decompose(Some(code));
        assert_eq!(
            (b.category, b.decimals, b.thousands, b.negative),
            (NUMBER, dec, true, neg),
            "{code}"
        );
    }
}

/// #1165: the dialog and the ribbon's Number Format box name a code's
/// category alike, except where Format Cells has no boxes for it.
#[test]
fn categories_agree_with_the_ribbons_names() {
    // Scientific's boxes cannot build engineering notation, and the runtime
    // draws tenths of a second as whole ones.
    let deliberate = ["##0.0E+0", "mm:ss.0"];
    let codes = (0..=49)
        .filter_map(gridcore::numfmt::builtin_code)
        .chain(crate::NUM_FORMATS.iter().map(|f| f.1));
    for code in codes {
        let ribbon = crate::numfmt_category(Some(code));
        let ours = CATEGORIES[decompose(Some(code)).category];
        if deliberate.contains(&code) {
            assert_eq!(ours, "Custom", "{code}");
        } else {
            assert_eq!(ours, ribbon, "{code}");
        }
    }
}

/// Every set of boxes, each category's own lists included.
fn every_box() -> Vec<NumberBoxes> {
    let mut all: Vec<NumberBoxes> = option_grid().collect();
    for (cat, n) in [
        (DATE, DATES.len()),
        (TIME, TIMES.len()),
        (FRACTION, FRACTIONS.len()),
    ] {
        for i in 0..n {
            let mut b = boxes_of(cat);
            b.date = i;
            b.time = i;
            b.fraction = i;
            all.push(b);
        }
    }
    all.push(boxes_of(GENERAL));
    all.push(boxes_of(TEXT));
    all
}

/// #1165: every code the Number tab builds can be rendered, and reads back
/// as the same code.
#[test]
fn every_built_code_parses_and_round_trips() {
    for b in every_box() {
        let code = build(&b);
        if !code.is_empty() {
            assert!(
                gridcore::numfmt::parse_format(&code).is_some(),
                "{code} does not parse"
            );
        }
        assert_eq!(build(&decompose(Some(&code))), code, "{code}");
    }
}

/// #1165: Excel's codes for the issue's boxes.
#[test]
fn the_built_codes_are_excels() {
    let number = |dec, thousands, negative| NumberBoxes {
        category: NUMBER,
        decimals: dec,
        thousands,
        negative,
        ..NumberBoxes::default()
    };
    assert_eq!(build(&number(2, false, 0)), "0.00");
    assert_eq!(build(&number(0, true, 0)), "#,##0");
    assert_eq!(build(&number(2, true, 1)), "#,##0.00;[Red]#,##0.00");
    assert_eq!(build(&number(1, false, 2)), "0.0_);(0.0)");
    assert_eq!(build(&number(2, true, 3)), "#,##0.00_);[Red](#,##0.00)");
    let currency = NumberBoxes {
        category: CURRENCY,
        symbol: 3,
        ..NumberBoxes::default()
    };
    assert_eq!(build(&currency), "[$£-809]#,##0.00");
    let accounting = NumberBoxes {
        category: ACCOUNTING,
        ..NumberBoxes::default()
    };
    assert_eq!(
        build(&accounting),
        "_($* #,##0.00_);_($* (#,##0.00);_($* \"-\"??_);_(@_)"
    );
    let mut sci = boxes_of(SCIENTIFIC);
    sci.decimals = 0;
    assert_eq!(build(&sci), "0E+00");
    let mut pct = boxes_of(PERCENTAGE);
    pct.decimals = 99;
    assert_eq!(build(&pct), format!("0.{}%", "0".repeat(30)), "held to 30");
    let mut custom = boxes_of(CUSTOM);
    custom.custom = "general".into();
    assert_eq!(build(&custom), "", "Custom General is no code");
}

/// #1165: a list's items are told apart by their text, as a script and a
/// click pick them.
#[test]
fn every_list_shows_each_item_once() {
    let unique = |items: Vec<String>| {
        let mut seen = items.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), items.len(), "{items:?}");
    };
    unique(DATES.iter().map(|c| shown(c, SAMPLE_SERIAL)).collect());
    unique(TIMES.iter().map(|c| shown(c, SAMPLE_SERIAL)).collect());
    for b in option_grid() {
        unique(negative_items(&b));
    }
    assert_eq!(COLOR_NAMES.len(), COLOR_SWATCHES.len());
    let mut names = COLOR_NAMES.to_vec();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), COLOR_SWATCHES.len());
    unique(code_list(&[
        coded("0.0 \"kg\"").clone(),
        coded("0.00").clone(),
    ]));
}

/// #1165: the Negative numbers list shows Excel's four, and says which are
/// red, since the grid does not draw them red.
#[test]
fn the_negative_list_follows_the_boxes() {
    let mut b = boxes_of(NUMBER);
    b.thousands = true;
    assert_eq!(
        negative_items(&b),
        [
            "-1,234.10",
            "1,234.10 (red)",
            "(1,234.10)",
            "(1,234.10) (red)"
        ]
    );
    b.category = CURRENCY;
    b.decimals = 0;
    assert_eq!(negative_items(&b)[0], "-$1,234");
}

// ---- the dialog ------------------------------------------------------------

/// #1165: Format Cells opens on the active cell, its tabs and the Number
/// tab's boxes for the cell's code, with a Sample of the cell.
#[test]
fn the_dialog_opens_on_the_active_cell() {
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(1234.1), coded("#,##0.00"));
    select(&mut t, "A1", "A1");
    open_on(&mut t);
    let d = top(&t);
    assert_eq!(
        (d.id.as_str(), d.title.as_str()),
        ("format-cells", "Format Cells")
    );
    assert_eq!(d.tabs, ["Number", "Alignment", "Font", "Fill", "Border"]);
    let labels: Vec<_> = d.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["OK", "Cancel"]);
    assert_eq!(read(&t, "category"), "Number");
    assert_eq!(read(&t, "decimals"), "2");
    assert_eq!(read(&t, "thousands"), "checked");
    assert_eq!(read(&t, "sample"), "1,234.10");
    assert_eq!(read(&t, "code"), "#,##0.00");
    assert!(d.catalog_check().is_ok(), "{:?}", d.catalog_check());
}

/// #1165: each category shows its own boxes; the Sample follows every
/// change.
#[test]
fn the_boxes_and_sample_follow_the_category() {
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(1234.1), Xf::default());
    select(&mut t, "A1", "A1");
    open_on(&mut t);
    assert_eq!(read(&t, "category"), "General");
    assert_eq!(read(&t, "sample"), "1234.1");
    for name in [
        "decimals",
        "thousands",
        "symbol",
        "negative",
        "code",
        "codes",
    ] {
        assert!(!visible(&t, name), "{name} under General");
    }
    set(&mut t, "category", s("Number"));
    for name in ["decimals", "thousands", "negative"] {
        assert!(visible(&t, name), "{name} under Number");
    }
    assert!(!visible(&t, "symbol"));
    set(&mut t, "decimals", s("3"));
    set(&mut t, "thousands", Json::Bool(true));
    assert_eq!(read(&t, "sample"), "1,234.100");
    assert_eq!(read(&t, "negative"), "-1,234.100");
    set(&mut t, "category", s("Currency"));
    assert!(visible(&t, "symbol") && !visible(&t, "thousands"));
    assert_eq!(read(&t, "sample"), "$1,234.100");
    set(&mut t, "category", s("Date"));
    assert!(visible(&t, "date-type") && !visible(&t, "decimals"));
    set(&mut t, "category", s("Fraction"));
    assert!(visible(&t, "fraction-type"));
    assert_eq!(read(&t, "sample"), "1234 1/9");
    set(&mut t, "category", s("Custom"));
    assert!(visible(&t, "code") && visible(&t, "codes"));
    assert_eq!(read(&t, "code"), "General", "the cell's code");
    set(&mut t, "code", s("0.0 \"kg\""));
    assert_eq!(read(&t, "sample"), "1234.1 kg");
    // A pick from the code list goes into the Type box.
    set(&mut t, "codes", s("0.00%"));
    assert_eq!(read(&t, "code"), "0.00%");
    assert_eq!(read(&t, "sample"), "123410.00%");
}

/// #1165: an empty cell's Sample is empty; a text cell shows its text.
#[test]
fn the_sample_of_an_empty_or_text_cell() {
    let mut t = tab();
    put(&mut t, "A2", CellValue::Text("abc".into()), Xf::default());
    select(&mut t, "A1", "A1");
    open_on(&mut t);
    set(&mut t, "category", s("Number"));
    assert_eq!(read(&t, "sample"), "");
    t.dialogs.pop();
    select(&mut t, "A2", "A2");
    open_on(&mut t);
    set(&mut t, "category", s("Custom"));
    set(&mut t, "code", s("\"<\"@\">\""));
    assert_eq!(read(&t, "sample"), "<abc>");
}

/// #1165: OK applies what changed to the whole selection as one undo step,
/// and leaves what it did not touch; undo restores both cells.
#[test]
fn ok_applies_only_what_changed_as_one_undo_step() {
    let mut t = tab();
    let bold = Xf {
        bold: true,
        ..Xf::default()
    };
    put(&mut t, "A1", CellValue::Number(1.5), bold);
    put(&mut t, "A2", CellValue::Number(2.25), Xf::default());
    select(&mut t, "A1", "A2");
    let before = undo_depth(&mut t);
    open_on(&mut t);
    set(&mut t, "category", s("Number"));
    set(&mut t, "decimals", s("3"));
    press(&mut t, "OK").unwrap();
    assert!(t.dialogs.top().is_none());
    assert_eq!(undo_depth(&mut t), before + 1);
    assert!(t.dirty);
    assert_eq!(shows(&mut t, "A1"), "1.500");
    assert_eq!(shows(&mut t, "A2"), "2.250");
    assert!(xf_at(&mut t, "A1").bold, "A1 stays bold");
    assert!(!xf_at(&mut t, "A2").bold, "A2 stays plain");
    assert!(view(&mut t).undo_step());
    assert_eq!(shows(&mut t, "A1"), "1.5");
    assert_eq!(shows(&mut t, "A2"), "2.25");
    assert!(xf_at(&mut t, "A1").bold);
}

/// #1165: OK with nothing changed changes nothing and adds no undo step;
/// Cancel applies nothing.
#[test]
fn ok_unchanged_and_cancel_change_nothing() {
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(5.0), coded("#,##0;(#,##0)"));
    select(&mut t, "A1", "A1");
    let before = undo_depth(&mut t);
    let xf = xf_at(&mut t, "A1");
    open_on(&mut t);
    press(&mut t, "OK").unwrap();
    assert!(t.dialogs.top().is_none());
    assert_eq!(undo_depth(&mut t), before);
    assert!(!t.dirty);
    assert_eq!(
        xf_at(&mut t, "A1"),
        xf,
        "the built-in code is kept as it was"
    );

    open_on(&mut t);
    set(&mut t, "decimals", s("4"));
    set(&mut t, "bold", Json::Bool(true));
    press(&mut t, "Cancel").unwrap();
    assert!(t.dialogs.top().is_none());
    assert_eq!(undo_depth(&mut t), before);
    assert_eq!(xf_at(&mut t, "A1"), xf);
}

/// #1165: a code the runtime cannot use is refused on OK with the dialog
/// kept open; a cell on such a code keeps it when only Font changes.
#[test]
fn an_unusable_code_is_refused_only_when_the_number_tab_changed() {
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(5.0), coded("0 ?/4"));
    select(&mut t, "A1", "A1");
    let before = undo_depth(&mut t);
    open_on(&mut t);
    assert_eq!(read(&t, "category"), "Custom");
    assert_eq!(read(&t, "code"), "0 ?/4");
    set(&mut t, "bold", Json::Bool(true));
    press(&mut t, "OK").unwrap();
    assert!(xf_at(&mut t, "A1").bold);
    assert_eq!(xf_at(&mut t, "A1").code.as_deref(), Some("0 ?/4"));

    open_on(&mut t);
    set(&mut t, "code", s("0 ?/8"));
    let err = press(&mut t, "OK").unwrap_err();
    assert_eq!(err, "docxy cannot use this number format: 0 ?/8");
    assert_eq!(top(&t).id.as_str(), "format-cells", "kept open");
    assert_eq!(xf_at(&mut t, "A1").code.as_deref(), Some("0 ?/4"));
    assert_eq!(undo_depth(&mut t), before + 1, "only the bold step");
}

/// #1165: colours by Excel's names; a colour that is no swatch opens on
/// its own item and survives an OK that changed something else.
#[test]
fn colours_by_name_and_a_custom_colour_survives() {
    let mut t = tab();
    let teal = Xf {
        fill: Some((0x12, 0x34, 0x56)),
        color_unresolved: true,
        ..Xf::default()
    };
    put(&mut t, "A1", CellValue::Number(1.0), teal);
    select(&mut t, "A1", "A1");
    open_on(&mut t);
    assert_eq!(read(&t, "fill"), "Custom (#123456)");
    assert_eq!(read(&t, "color"), "Theme colour");
    set(&mut t, "category", s("Percentage"));
    press(&mut t, "OK").unwrap();
    let xf = xf_at(&mut t, "A1");
    assert_eq!(xf.fill, Some((0x12, 0x34, 0x56)));
    assert!(xf.color_unresolved);
    assert_eq!(shows(&mut t, "A1"), "100.00%");

    open_on(&mut t);
    set(&mut t, "color", s("Red"));
    set(&mut t, "fill", s("No Color"));
    set(&mut t, "horizontal", s("Center"));
    set(&mut t, "italic", Json::Bool(true));
    set(&mut t, "outline", Json::Bool(true));
    press(&mut t, "OK").unwrap();
    let xf = xf_at(&mut t, "A1");
    assert_eq!(xf.color, Some((0xFF, 0, 0)));
    assert!(!xf.color_unresolved);
    assert_eq!(xf.fill, None);
    assert_eq!(xf.align, Align::Center);
    assert!(xf.italic && xf.border);
}

/// #1165: F4 repeats the whole dialog's change on another selection.
#[test]
fn f4_repeats_format_cells() {
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(1.0), Xf::default());
    put(&mut t, "B1", CellValue::Number(2.0), Xf::default());
    select(&mut t, "A1", "A1");
    open_on(&mut t);
    set(&mut t, "category", s("Percentage"));
    set(&mut t, "decimals", s("0"));
    set(&mut t, "bold", Json::Bool(true));
    press(&mut t, "OK").unwrap();
    select(&mut t, "B1", "B1");
    assert!(view(&mut t).repeat_format());
    assert_eq!(shows(&mut t, "B1"), "200%");
    assert!(xf_at(&mut t, "B1").bold);
}

/// #1165: Protected View, a final workbook and a protected sheet refuse it.
#[test]
fn protection_refuses_the_dialog() {
    let mut t = tab();
    view(&mut t).pkg.workbook.sheets[0].set_protected(true);
    assert_eq!(
        open(&mut t).unwrap_err(),
        crate::sheet_goto::SHEET_PROTECTED
    );
    assert!(t.dialogs.top().is_none());

    let mut t = tab();
    t.access.protected = true;
    assert_eq!(open(&mut t).unwrap_err(), t.access.locked_status());
    assert!(t.dialogs.top().is_none());
}

/// #1165: Ctrl+1 or a launcher again leaves the one dialog that is up.
#[test]
fn opening_again_keeps_one_dialog() {
    let mut t = tab();
    open_on(&mut t);
    set(&mut t, "category", s("Number"));
    open_on(&mut t);
    assert_eq!(t.dialogs.depth(), 1);
    assert_eq!(read(&t, "category"), "Number", "the same dialog");
}

/// #1165: OK while the active cell is being edited formats the cell and
/// keeps the entry, which the commit then writes in the new format.
#[test]
fn ok_while_editing_keeps_the_entry() {
    let mut t = tab();
    select(&mut t, "A1", "A1");
    let v = view(&mut t);
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    v.edit_type("7");
    open_on(&mut t);
    set(&mut t, "category", s("Number"));
    set(&mut t, "decimals", s("1"));
    press(&mut t, "OK").unwrap();
    assert_eq!(view(&mut t).editing.as_deref(), Some("7"));
    view(&mut t).commit_and_move(1, 0);
    assert_eq!(shows(&mut t, "A1"), "7.0");
}

/// #1165 r1: the built-ins Excel writes out itself (5–8, 41–44), with
/// `"$"` and escaped parentheses, open on their category's boxes.
#[test]
fn excels_written_out_builtins_open_on_their_boxes() {
    // (code, category, decimals, symbol, negative)
    let cases: [(&str, usize, u32, usize, usize); 9] = [
        (r##""$"#,##0_);\("$"#,##0\)"##, CURRENCY, 0, 1, 2),
        (r##""$"#,##0_);[Red]\("$"#,##0\)"##, CURRENCY, 0, 1, 3),
        (r##""$"#,##0.00_);\("$"#,##0.00\)"##, CURRENCY, 2, 1, 2),
        (r##""$"#,##0.00_);[Red]\("$"#,##0.00\)"##, CURRENCY, 2, 1, 3),
        (
            r##"_(* #,##0_);_(* \(#,##0\);_(* "-"_);_(@_)"##,
            ACCOUNTING,
            0,
            0,
            0,
        ),
        (
            r##"_("$"* #,##0_);_("$"* \(#,##0\);_("$"* "-"_);_(@_)"##,
            ACCOUNTING,
            0,
            1,
            0,
        ),
        (
            r##"_(* #,##0.00_);_(* \(#,##0.00\);_(* "-"??_);_(@_)"##,
            ACCOUNTING,
            2,
            0,
            0,
        ),
        (
            r##"_("$"* #,##0.00_);_("$"* \(#,##0.00\);_("$"* "-"??_);_(@_)"##,
            ACCOUNTING,
            2,
            1,
            0,
        ),
        (r##"#,##0.00_);\(#,##0.00\)"##, NUMBER, 2, 1, 2),
    ];
    for (code, cat, dec, sym, neg) in cases {
        let b = decompose(Some(code));
        let got_sym = if cat == NUMBER { 1 } else { b.symbol };
        assert_eq!(
            (b.category, b.decimals, got_sym, b.negative),
            (cat, dec, sym, neg),
            "{code}"
        );
        assert_eq!(b.custom, code, "Custom keeps the code as written");
    }
    assert!(decompose(Some(r##"#,##0.00_);\(#,##0.00\)"##)).thousands);
}

/// #1165 r1: a Custom code typed over the cell's code is written even
/// when it builds the same boxes as the code the cell has.
#[test]
fn a_typed_custom_code_is_written_as_typed() {
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(5.0), coded("#,##0;(#,##0)"));
    select(&mut t, "A1", "A1");
    open_on(&mut t);
    set(&mut t, "category", s("Custom"));
    assert_eq!(read(&t, "code"), "#,##0;(#,##0)");
    set(&mut t, "code", s("#,##0_);(#,##0)"));
    press(&mut t, "OK").unwrap();
    assert_eq!(xf_at(&mut t, "A1").code.as_deref(), Some("#,##0_);(#,##0)"));

    // A mixed selection: the active cell's code in another spelling still
    // goes to every cell.
    let mut t = tab();
    put(&mut t, "A1", CellValue::Number(5.0), coded("\"$\"#,##0.00"));
    put(&mut t, "A2", CellValue::Number(6.0), Xf::default());
    select(&mut t, "A1", "A2");
    open_on(&mut t);
    set(&mut t, "category", s("Custom"));
    set(&mut t, "code", s("$#,##0.00"));
    press(&mut t, "OK").unwrap();
    assert_eq!(xf_at(&mut t, "A1").code.as_deref(), Some("$#,##0.00"));
    assert_eq!(xf_at(&mut t, "A2").code.as_deref(), Some("$#,##0.00"));
    assert_eq!(shows(&mut t, "A2"), "$6.00");
}
