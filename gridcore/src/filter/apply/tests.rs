//! The AutoFilter and Advanced Filter commands against the spec's QA cases
//! (DAT-CASE-012 to 017, 019 and 039), with each case's data built here.

use super::*;
use crate::filter::{AdvancedFilter, MENU_LIMIT, Submenu, advanced, menu};
use crate::sheet::{Cell, Sheet, Xf, parts_to_serial};

fn day(y: i64, m: u32, d: u32) -> f64 {
    parts_to_serial(y, m, d, 0, false)
}

/// 2024-03-13, a Wednesday, as the engine's clock gives it.
fn today() -> f64 {
    day(2024, 3, 13)
}

fn book(rows: &[Vec<Cell>]) -> Workbook {
    let mut s = Sheet {
        name: "Sheet1".into(),
        ..Sheet::default()
    };
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if !cell.is_blank() {
                s.set_cell(r as u32, c as u32, cell.clone());
            }
        }
    }
    let mut wb = Workbook {
        sheets: vec![s],
        ..Workbook::default()
    };
    wb.styles.xfs.push(Xf::default());
    wb
}

fn style(wb: &mut Workbook, code: &str) -> u32 {
    let mut xf = Xf::default();
    xf.set_code(Some(code.into()));
    wb.styles.intern(xf)
}

fn t(s: &str) -> Cell {
    Cell::text(s)
}

fn n(v: f64) -> Cell {
    Cell::number(v)
}

/// The data rows (1-based, as the sheet numbers them) left visible.
fn visible(wb: &Workbook, from: u32, to: u32) -> Vec<u32> {
    (from..=to)
        .filter(|&r| !wb.sheets[0].row_hidden(r - 1))
        .collect()
}

const REPS: [&str; 5] = ["Noor", "Cy", "Bo", "Ann", "Noor"];
const PRODUCTS: [&str; 3] = ["Stapler", "Pen", "Desk"];
const CODES: [&str; 6] = ["A-1", "a*2", "B?3", "AB12", "c~4", "Z9"];

/// DAT-CASE-012's list, A1:E21: Rep, Product, Units, Price (two decimals),
/// Code (DAT-CASE-013's column E).
fn filterlist() -> Workbook {
    let mut rows = vec![vec![
        t("Rep"),
        t("Product"),
        t("Units"),
        t("Price"),
        t("Code"),
    ]];
    for i in 0..20usize {
        rows.push(vec![
            t(REPS[i % 5]),
            t(PRODUCTS[i % 3]),
            n(((i * 7) % 50) as f64),
            n(if i == 4 { 5.0 } else { 1.25 * i as f64 }),
            t(CODES[i % 6]),
        ]);
    }
    let mut wb = book(&rows);
    let money = style(&mut wb, "0.00");
    for r in 1..=20 {
        wb.sheets[0].cells.get_mut(&(r, 3)).unwrap().style = money;
    }
    wb
}

/// The 1-based rows of `filterlist` whose record satisfies `keep`.
fn expect(keep: impl Fn(usize) -> bool) -> Vec<u32> {
    (0..20usize)
        .filter(|&i| keep(i))
        .map(|i| i as u32 + 2)
        .collect()
}

fn units(i: usize) -> f64 {
    ((i * 7) % 50) as f64
}

#[test]
fn turning_auto_filter_on_and_off() {
    let mut wb = filterlist();
    assert_eq!(auto_filter_on(&mut wb, 0, (4, 2)), Ok((0, 0, 20, 4)));
    let af = wb.sheets[0].auto_filter.as_ref().unwrap();
    assert!(af.criteria.is_empty());
    assert_eq!(wb.defined_name(FILTER_DB, 0), Some("Sheet1!$A$1:$E$21"));
    set_criterion(
        &mut wb,
        0,
        0,
        Some(ColumnFilter::values(vec!["Bo".into()])),
        today(),
    )
    .unwrap();
    // A row hidden by hand inside the list.
    wb.sheets[0].set_row_hidden(3, true);
    assert!(auto_filter_off(&mut wb, 0));
    assert!(wb.sheets[0].auto_filter.is_none());
    assert_eq!(wb.defined_name(FILTER_DB, 0), None);
    // Every row of the range shows again, the hand-hidden one too (Excel).
    assert_eq!(visible(&wb, 1, 21), (1..=21).collect::<Vec<_>>());
    assert!(wb.sheets[0].filtered_rows.is_empty());
    assert_eq!(wb.sheets[0].filter_mode, Some(false));
    assert!(!auto_filter_off(&mut wb, 0));
    // Nothing to filter on an empty cell.
    assert_eq!(
        auto_filter_on(&mut wb, 0, (40, 40)),
        Err(FilterError::NoData)
    );
}

#[test]
fn dat_case_012_checklists_combine_with_and() {
    let mut wb = filterlist();
    auto_filter_on_range(&mut wb, 0, (0, 0, 20, 3)).unwrap();
    let rep = ColumnFilter::values(vec!["Noor".into(), "Cy".into()]);
    set_criterion(&mut wb, 0, 0, Some(rep), today()).unwrap();
    let product = ColumnFilter::values(vec!["Stapler".into()]);
    let out = set_criterion(&mut wb, 0, 1, Some(product), today()).unwrap();
    let want = expect(|i| matches!(REPS[i % 5], "Noor" | "Cy") && PRODUCTS[i % 3] == "Stapler");
    assert!(!want.is_empty() && want.len() < 20);
    assert_eq!(visible(&wb, 2, 21), want);
    assert!(!wb.sheets[0].row_hidden(0), "the header never hides");
    assert_eq!(
        out,
        FilterOutcome {
            shown: want.len(),
            total: 20
        }
    );
    assert_eq!(
        status_text(&out),
        format!("{} of 20 records found", want.len())
    );
    for r in 2..=21u32 {
        assert_eq!(
            wb.sheets[0].row_filtered(r - 1),
            !want.contains(&r),
            "row {r}"
        );
    }
    // Rechecking every rep brings their rows back.
    let all = ColumnFilter::values(REPS.iter().map(|s| s.to_string()).collect());
    set_criterion(&mut wb, 0, 0, Some(all), today()).unwrap();
    assert_eq!(
        visible(&wb, 2, 21),
        expect(|i| PRODUCTS[i % 3] == "Stapler")
    );
    // Values match the displayed text: a price of 5 shows `5.00`.
    let price = ColumnFilter::values(vec!["5.00".into()]);
    set_criterion(&mut wb, 0, 1, None, today()).unwrap();
    set_criterion(&mut wb, 0, 3, Some(price), today()).unwrap();
    assert_eq!(visible(&wb, 2, 21), vec![6]);
}

fn custom(conds: &[(&str, &str)], and: bool) -> ColumnFilter {
    ColumnFilter::Custom {
        and,
        conds: conds
            .iter()
            .map(|(o, v)| (o.to_string(), v.to_string()))
            .collect(),
    }
}

#[test]
fn dat_case_013_custom_conditions() {
    let code = |i: usize| CODES[i % 6];
    let cases: Vec<(u32, ColumnFilter, Vec<u32>)> = vec![
        // Code begins with a: case-insensitive.
        (
            4,
            custom(&[("equal", "a*")], false),
            expect(|i| matches!(code(i), "A-1" | "a*2" | "AB12")),
        ),
        // equals a~*2: only the literal `a*2`.
        (
            4,
            custom(&[("equal", "a~*2")], false),
            expect(|i| code(i) == "a*2"),
        ),
        // contains ?: any character, so every code.
        (4, custom(&[("equal", "*?*")], false), expect(|_| true)),
        // contains ~?: a literal question mark.
        (
            4,
            custom(&[("equal", "*~?*")], false),
            expect(|i| code(i) == "B?3"),
        ),
        // ends with 2; does not contain 1.
        (
            4,
            custom(&[("equal", "*2")], false),
            expect(|i| code(i).ends_with('2')),
        ),
        (
            4,
            custom(&[("notEqual", "*1*")], false),
            expect(|i| !code(i).contains('1')),
        ),
        // Units > 10 And <= 30.
        (
            2,
            custom(&[("greaterThan", "10"), ("lessThanOrEqual", "30")], true),
            expect(|i| units(i) > 10.0 && units(i) <= 30.0),
        ),
        // Units < 5 Or > 40.
        (
            2,
            custom(&[("lessThan", "5"), ("greaterThan", "40")], false),
            expect(|i| units(i) < 5.0 || units(i) > 40.0),
        ),
        // Price equals 5, a cell showing 5.00.
        (3, custom(&[("equal", "5")], false), vec![6]),
    ];
    for (col, f, want) in cases {
        let mut wb = filterlist();
        auto_filter_on(&mut wb, 0, (0, 0)).unwrap();
        let out = set_criterion(&mut wb, 0, col, Some(f.clone()), today()).unwrap();
        assert_eq!(visible(&wb, 2, 21), want, "{f:?}");
        assert_eq!(out.shown, want.len());
    }
}

/// DAT-CASE-014's column: Score, then 90, 85, 85, 70, 60, (blank), text, 85,
/// 40, 20, 10, 95 in A2:A13.
fn scores() -> Workbook {
    let mut rows = vec![vec![t("Score")]];
    for v in [
        Some(90.0),
        Some(85.0),
        Some(85.0),
        Some(70.0),
        Some(60.0),
        None,
        Some(-1.0),
        Some(85.0),
        Some(40.0),
        Some(20.0),
        Some(10.0),
        Some(95.0),
    ] {
        rows.push(vec![match v {
            Some(-1.0) => t("text"),
            Some(x) => n(x),
            None => Cell::default(),
        }]);
    }
    let mut wb = book(&rows);
    auto_filter_on_range(&mut wb, 0, (0, 0, 12, 0)).unwrap();
    wb
}

fn top(top: bool, percent: bool, val: f64) -> ColumnFilter {
    ColumnFilter::Top10 {
        top,
        percent,
        val,
        filter_val: None,
    }
}

#[test]
fn dat_case_014_top_10_and_averages() {
    let mut wb = scores();
    // Top 3 items: 95, 90 and all three 85s.
    set_criterion(&mut wb, 0, 0, Some(top(true, false, 3.0)), today()).unwrap();
    assert_eq!(visible(&wb, 2, 13), vec![2, 3, 4, 9, 13]);
    // The cut-off is kept, as Excel saves it.
    let held = &wb.sheets[0].auto_filter.as_ref().unwrap().criteria[0].1;
    assert!(matches!(
        held,
        ColumnFilter::Top10 {
            filter_val: Some(85.0),
            ..
        }
    ));
    // Top 25 percent of the 10 numbers: k = floor(2.5) = 2, so 95 and 90.
    set_criterion(&mut wb, 0, 0, Some(top(true, true, 25.0)), today()).unwrap();
    assert_eq!(visible(&wb, 2, 13), vec![2, 13]);
    // Bottom 2 items: 10 and 20; never the blank or the text.
    set_criterion(&mut wb, 0, 0, Some(top(false, false, 2.0)), today()).unwrap();
    assert_eq!(visible(&wb, 2, 13), vec![11, 12]);
    // Above average: the numbers' average is 64.
    let above = ColumnFilter::Dynamic {
        kind: "aboveAverage".into(),
        val: None,
        max_val: None,
    };
    set_criterion(&mut wb, 0, 0, Some(above), today()).unwrap();
    let first = visible(&wb, 2, 13);
    assert_eq!(first, vec![2, 3, 4, 5, 9, 13]);
    // A11 (10) becomes 1000: nothing moves until Reapply.
    wb.sheets[0].set_cell(11, 0, n(1000.0));
    assert_eq!(visible(&wb, 2, 13), first);
    reapply(&mut wb, 0, today()).unwrap();
    assert_eq!(visible(&wb, 2, 13), vec![12]);
    let below = ColumnFilter::Dynamic {
        kind: "belowAverage".into(),
        val: None,
        max_val: None,
    };
    set_criterion(&mut wb, 0, 0, Some(below), today()).unwrap();
    assert_eq!(visible(&wb, 2, 13), vec![2, 3, 4, 5, 6, 9, 10, 11, 13]);
}

/// DAT-CASE-015's dates: 24 dates over 2023–2025, with 2024-03-10 (a
/// Sunday) and 2024-03-16 (Saturday).
fn datelist() -> (Workbook, Vec<(i64, u32, u32)>) {
    let dates: Vec<(i64, u32, u32)> = vec![
        (2023, 1, 5),
        (2023, 3, 9),
        (2023, 3, 30),
        (2023, 7, 1),
        (2023, 12, 31),
        (2024, 1, 1),
        (2024, 2, 29),
        (2024, 3, 1),
        (2024, 3, 9),
        (2024, 3, 10),
        (2024, 3, 12),
        (2024, 3, 13),
        (2024, 3, 14),
        (2024, 3, 16),
        (2024, 3, 17),
        (2024, 3, 31),
        (2024, 4, 1),
        (2024, 6, 15),
        (2024, 12, 25),
        (2025, 1, 1),
        (2025, 3, 3),
        (2025, 3, 13),
        (2025, 8, 8),
        (2025, 11, 30),
    ];
    let mut rows = vec![vec![t("Date")]];
    for &(y, m, d) in &dates {
        rows.push(vec![n(day(y, m, d))]);
    }
    let mut wb = book(&rows);
    let date = style(&mut wb, "yyyy-mm-dd");
    for r in 1..=24 {
        wb.sheets[0].cells.get_mut(&(r, 0)).unwrap().style = date;
    }
    auto_filter_on_range(&mut wb, 0, (0, 0, 24, 0)).unwrap();
    (wb, dates)
}

#[test]
fn dat_case_015_date_filters() {
    let (mut wb, dates) = datelist();
    let rows = |keep: &dyn Fn(i64, u32, u32) -> bool| -> Vec<u32> {
        dates
            .iter()
            .enumerate()
            .filter(|(_, (y, m, d))| keep(*y, *m, *d))
            .map(|(i, _)| i as u32 + 2)
            .collect()
    };
    let dynamic = |k: &str| ColumnFilter::Dynamic {
        kind: k.into(),
        val: None,
        max_val: None,
    };
    let cases: Vec<(&str, Vec<u32>)> = vec![
        (
            "thisWeek",
            rows(&|y, m, d| y == 2024 && m == 3 && (10..=16).contains(&d)),
        ),
        ("thisMonth", rows(&|y, m, _| y == 2024 && m == 3)),
        (
            "yearToDate",
            rows(&|y, m, d| y == 2024 && (m, d) <= (3, 13)),
        ),
        ("M3", rows(&|_, m, _| m == 3)),
        ("Q1", rows(&|_, m, _| m <= 3)),
        ("today", rows(&|y, m, d| (y, m, d) == (2024, 3, 13))),
        ("lastYear", rows(&|y, _, _| y == 2023)),
        ("nextYear", rows(&|y, _, _| y == 2025)),
    ];
    for (k, want) in cases {
        set_criterion(&mut wb, 0, 0, Some(dynamic(k)), today()).unwrap();
        assert_eq!(visible(&wb, 2, 25), want, "{k}");
    }
    // The window This Week was applied with is what it holds.
    let held = &wb.sheets[0].auto_filter.as_ref().unwrap().criteria[0].1;
    assert_eq!(
        held,
        &ColumnFilter::Dynamic {
            kind: "nextYear".into(),
            val: Some(day(2025, 1, 1)),
            max_val: Some(day(2026, 1, 1)),
        }
    );
    // The checklist with only 2024 › March checked.
    let march = ColumnFilter::Values {
        vals: Vec::new(),
        blank: false,
        dates: vec![DateGroup {
            year: 2024,
            month: Some(3),
            day: None,
        }],
    };
    set_criterion(&mut wb, 0, 0, Some(march), today()).unwrap();
    assert_eq!(visible(&wb, 2, 25), rows(&|y, m, _| y == 2024 && m == 3));
    // The drop-down: Date Filters, a year › month › day tree.
    let m = menu(&wb, 0, 0, None).unwrap();
    assert_eq!(m.submenu, Submenu::Date);
    assert_eq!((m.items[0].label.as_str(), m.items[0].depth), ("2023", 0));
    assert_eq!(
        (m.items[1].label.as_str(), m.items[1].depth),
        ("January", 1)
    );
    assert_eq!((m.items[2].label.as_str(), m.items[2].depth), ("05", 2));
    let checked: Vec<&str> = m
        .items
        .iter()
        .filter(|i| i.checked && i.depth == 1)
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(checked, vec!["March"]);
}

/// DAT-CASE-010/016's colours: A has green, yellow and red fills (and a CF
/// rule that fills A11 red), B red font on some rows, C a 3Arrows icon set.
fn colours() -> Workbook {
    let mut rows = vec![vec![t("Item"), t("Note"), t("Score")]];
    let items = [
        "Pen", "Cup", "Pen", "Box", "Pen", "Cup", "Box", "Pen", "Cup", "Box",
    ];
    for (i, item) in items.iter().enumerate() {
        rows.push(vec![t(item), t(&format!("n{i}")), n(i as f64 * 10.0)]);
    }
    let mut wb = book(&rows);
    let fill = |wb: &mut Workbook, rgb| {
        wb.styles.intern(Xf {
            fill: Some(rgb),
            ..Xf::default()
        })
    };
    let green = fill(&mut wb, (0, 176, 80));
    let yellow = fill(&mut wb, (255, 255, 0));
    let red = fill(&mut wb, (255, 0, 0));
    let red_font = wb.styles.intern(Xf {
        color: Some((255, 0, 0)),
        ..Xf::default()
    });
    let s = &mut wb.sheets[0];
    for (r, st) in [(1, green), (2, yellow), (4, red), (5, green), (8, yellow)] {
        s.cells.get_mut(&(r, 0)).unwrap().style = st;
    }
    for r in [2, 3, 7] {
        s.cells.get_mut(&(r, 1)).unwrap().style = red_font;
    }
    // A10 turns red through conditional formatting.
    s.cond_formats.push(crate::sheet::CondFormat {
        ranges: vec![(10, 0, 10, 0)],
        rules: vec![crate::sheet::CfRule {
            kind: crate::sheet::CfKind::Expression {
                formula: "TRUE".into(),
            },
            dxf_id: Some(0),
            priority: 1,
        }],
        ix: None,
    });
    s.cond_formats.push(crate::sheet::CondFormat {
        ranges: vec![(1, 2, 10, 2)],
        rules: vec![crate::sheet::CfRule {
            kind: crate::sheet::CfKind::IconSet {
                set: "3Arrows".into(),
                reverse: false,
                cfvos: ["0", "33", "67"]
                    .iter()
                    .map(|v| crate::sheet::Cfvo {
                        kind: "percent".into(),
                        val: (*v).into(),
                        gte: true,
                    })
                    .collect(),
                formulas: Vec::new(),
            },
            dxf_id: None,
            priority: 2,
        }],
        ix: None,
    });
    wb.styles.dxfs.push(crate::sheet::Dxf {
        fill: Some((255, 0, 0)),
        ..crate::sheet::Dxf::default()
    });
    auto_filter_on_range(&mut wb, 0, (0, 0, 10, 2)).unwrap();
    wb
}

fn color(cell: bool, rgb: Option<(u8, u8, u8)>) -> ColumnFilter {
    ColumnFilter::Color {
        cell,
        rgb,
        dxf_id: None,
    }
}

#[test]
fn dat_case_016_colour_icon_and_selected_cell() {
    let mut wb = colours();
    set_criterion(
        &mut wb,
        0,
        0,
        Some(color(true, Some((0, 176, 80)))),
        today(),
    )
    .unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![2, 6]);
    // A second colour replaces the first: No Fill.
    set_criterion(&mut wb, 0, 0, Some(color(true, None)), today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![4, 7, 8, 10]);
    // Red includes the conditional-format red of A11.
    set_criterion(&mut wb, 0, 0, Some(color(true, Some((255, 0, 0)))), today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![5, 11]);
    clear(&mut wb, 0, Some(0), today()).unwrap();
    set_criterion(
        &mut wb,
        0,
        1,
        Some(color(false, Some((255, 0, 0)))),
        today(),
    )
    .unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![3, 4, 8]);
    clear(&mut wb, 0, None, today()).unwrap();
    // The green up arrow: the top third of 0..90.
    let up = ColumnFilter::Icon {
        set: "3Arrows".into(),
        id: 2,
    };
    set_criterion(&mut wb, 0, 2, Some(up), today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![9, 10, 11]);
    clear(&mut wb, 0, None, today()).unwrap();
    // Filter by Selected Cell's Value on a `Pen`.
    filter_by_cell(&mut wb, 0, (1, 0), ByCell::Value, today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![2, 4, 6, 9]);
    filter_by_cell(&mut wb, 0, (2, 0), ByCell::CellColor, today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![3, 9]);
    filter_by_cell(&mut wb, 0, (3, 1), ByCell::FontColor, today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![3]);
    clear(&mut wb, 0, None, today()).unwrap();
    filter_by_cell(&mut wb, 0, (1, 2), ByCell::Icon, today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![2, 3, 4]);
    // A theme colour is unknown: neither No Fill nor a colour.
    let themed = wb.styles.intern(Xf {
        fill_unresolved: true,
        ..Xf::default()
    });
    wb.sheets[0].cells.get_mut(&(7, 0)).unwrap().style = themed;
    clear(&mut wb, 0, None, today()).unwrap();
    set_criterion(&mut wb, 0, 0, Some(color(true, None)), today()).unwrap();
    assert_eq!(visible(&wb, 2, 11), vec![4, 7, 10]);
    assert_eq!(
        filter_by_cell(&mut wb, 0, (7, 0), ByCell::CellColor, today()),
        Err(FilterError::NoColor)
    );
}

#[test]
fn dat_case_017_filters_are_not_live() {
    let mut wb = filterlist();
    auto_filter_on(&mut wb, 0, (0, 0)).unwrap();
    set_criterion(
        &mut wb,
        0,
        0,
        Some(ColumnFilter::values(vec!["Noor".into()])),
        today(),
    )
    .unwrap();
    let noor = expect(|i| REPS[i % 5] == "Noor");
    assert_eq!(visible(&wb, 2, 21), noor);
    // Set a visible row's Rep to Bo: it stays until Reapply.
    let edited = noor[0];
    wb.sheets[0].set_cell(edited - 1, 0, t("Bo"));
    assert_eq!(visible(&wb, 2, 21), noor);
    reapply(&mut wb, 0, today()).unwrap();
    assert_eq!(visible(&wb, 2, 21), noor[1..].to_vec());
    // Clear on the column: every row shows, and the filter stays.
    let out = clear(&mut wb, 0, Some(0), today()).unwrap();
    assert_eq!(
        out,
        FilterOutcome {
            shown: 20,
            total: 20
        }
    );
    assert_eq!(visible(&wb, 2, 21).len(), 20);
    assert!(wb.sheets[0].auto_filter.is_some());
    // Filter off: none left.
    auto_filter_off(&mut wb, 0);
    assert!(wb.sheets[0].auto_filter.is_none());
    assert_eq!(reapply(&mut wb, 0, today()), Err(FilterError::NoFilter));
}

#[test]
fn a_pass_shows_a_hand_hidden_row_and_an_opaque_column_leaves_rows_alone() {
    let mut wb = filterlist();
    auto_filter_on(&mut wb, 0, (0, 0)).unwrap();
    wb.sheets[0].set_row_hidden(1, true); // row 2, Noor
    set_criterion(
        &mut wb,
        0,
        0,
        Some(ColumnFilter::values(vec!["Noor".into()])),
        today(),
    )
    .unwrap();
    assert!(!wb.sheets[0].row_hidden(1));
    // With a column we can't evaluate, a passing row keeps its state.
    let raw = ColumnFilter::Raw(
        r#"<filterColumn colId="4"><filters><dateGroupItem year="2024" month="1" day="1" hour="3" dateTimeGrouping="hour"/></filters></filterColumn>"#.into(),
    );
    wb.sheets[0].set_row_filtered(1, true);
    set_criterion(&mut wb, 0, 4, Some(raw), today()).unwrap();
    assert!(wb.sheets[0].row_filtered(1), "kept hidden");
    let noor = expect(|i| REPS[i % 5] == "Noor");
    assert_eq!(visible(&wb, 2, 21), noor[1..].to_vec());
    // A column that only hides its button filters nothing.
    let button = ColumnFilter::Raw(r#"<filterColumn colId="4" hiddenButton="1"/>"#.into());
    set_criterion(&mut wb, 0, 4, Some(button), today()).unwrap();
    assert_eq!(visible(&wb, 2, 21), noor);
}

#[test]
fn dat_case_039_menu_search_limit_and_growth() {
    // A: 12,000 text codes; B: numbers with two text cells; C: dates.
    let mut rows = vec![vec![t("Code"), t("Qty"), t("When")]];
    for i in 0..12_000usize {
        let qty = if i == 5 || i == 9 {
            t("n/a")
        } else {
            n(i as f64)
        };
        rows.push(vec![
            t(&format!("A{i}")),
            qty,
            n(day(2024, 1, 1) + (i % 400) as f64),
        ]);
    }
    let mut wb = book(&rows);
    let date = style(&mut wb, "yyyy-mm-dd");
    for r in 1..=12_000 {
        wb.sheets[0].cells.get_mut(&(r, 2)).unwrap().style = date;
    }
    auto_filter_on(&mut wb, 0, (0, 0)).unwrap();
    let a = menu(&wb, 0, 0, None).unwrap();
    assert_eq!(a.submenu, Submenu::Text);
    assert!(a.truncated);
    assert_eq!((a.items.len(), a.total), (MENU_LIMIT, 12_000));
    assert_eq!(menu(&wb, 0, 1, None).unwrap().submenu, Submenu::Number);
    let c = menu(&wb, 0, 2, None).unwrap();
    assert_eq!(c.submenu, Submenu::Date);
    assert_eq!(c.items[0].label, "2024");
    // Search `7*1` narrows the list.
    let found = menu(&wb, 0, 0, Some("7*1")).unwrap();
    assert!(!found.truncated);
    assert!(found.items.iter().all(|i| i.label.contains('7')));
    assert!(found.items.iter().any(|i| i.label == "A701"));
    assert!(!found.items.iter().any(|i| i.label == "A17"));
    // Codes beginning A1 by a search, then "Add current selection" of 7*1.
    search(&mut wb, 0, 0, "A1", false, today()).unwrap();
    let a1 = visible(&wb, 2, 12_001).len();
    search(&mut wb, 0, 0, "7*1", true, today()).unwrap();
    let both = visible(&wb, 2, 12_001);
    assert!(both.len() > a1);
    assert!(both.contains(&(1 + 2))); // A1 → row 3 (A0 is row 2)
    assert!(both.contains(&(701 + 2)));
    // Without add, the search replaces the criterion.
    search(&mut wb, 0, 0, "7*1", false, today()).unwrap();
    assert!(!visible(&wb, 2, 12_001).contains(&(1 + 2)));
    // A record typed under the list is inside it at the next filter.
    wb.sheets[0].set_cell(12_001, 0, t("A7x1"));
    search(&mut wb, 0, 0, "7*1", false, today()).unwrap();
    assert_eq!(wb.sheets[0].auto_filter.as_ref().unwrap().range.2, 12_001);
    assert!(visible(&wb, 2, 12_002).contains(&12_002));
    assert_eq!(wb.defined_name(FILTER_DB, 0), Some("Sheet1!$A$1:$C$12002"));
}

/// DAT-CASE-019: the list A1:C9 (Region, Rep, Amount) with Ann's record
/// repeated at row 9, criteria E1:E2 Region = East, and H1:I1 Amount, Rep.
fn advanced_book() -> Workbook {
    let recs = [
        ("East", "Ann", 120.0),
        ("West", "Bob", 80.0),
        ("East", "Cara", 200.0),
        ("North", "Dan", 50.0),
        ("East", "Carl", 90.0),
        ("West", "Fay", 40.0),
        ("South", "Eve", 30.0),
        ("East", "Ann", 120.0),
    ];
    let mut rows = vec![vec![t("Region"), t("Rep"), t("Amount")]];
    for (g, r, a) in recs {
        rows.push(vec![t(g), t(r), n(a)]);
    }
    let mut wb = book(&rows);
    let s = &mut wb.sheets[0];
    s.set_cell(0, 4, t("Region"));
    s.set_cell(1, 4, t("East"));
    s.set_cell(0, 7, t("Amount"));
    s.set_cell(0, 8, t("Rep"));
    wb.sheets.push(Sheet {
        name: "Sheet2".into(),
        ..Sheet::default()
    });
    wb
}

fn text_at(wb: &Workbook, r: u32, c: u32) -> String {
    shown_text(wb, 0, r, c)
}

#[test]
fn dat_case_019_advanced_filter() {
    let list = (0, 0, 8, 2);
    let crit = Some((0, (0, 4, 1, 4)));
    // In place: West, North and Eve's rows hide.
    let mut wb = advanced_book();
    let out = advanced(
        &mut wb,
        0,
        &AdvancedFilter {
            list,
            criteria: crit,
            ..AdvancedFilter::default()
        },
    )
    .unwrap();
    assert_eq!(out, FilterOutcome { shown: 4, total: 8 });
    assert_eq!(visible(&wb, 2, 9), vec![2, 4, 6, 9]);
    assert!(wb.sheets[0].row_filtered(2));
    assert_eq!(wb.sheets[0].filter_mode, Some(true));
    assert_eq!(wb.defined_name(FILTER_DB, 0), Some("Sheet1!$A$1:$C$9"));
    assert_eq!(
        wb.defined_name("_xlnm.Criteria", 0),
        Some("Sheet1!$E$1:$E$2")
    );
    // Clear, with no AutoFilter, shows them through _FilterDatabase.
    clear(&mut wb, 0, None, today()).unwrap();
    assert_eq!(visible(&wb, 2, 9).len(), 8);
    assert_eq!(wb.sheets[0].filter_mode, Some(false));

    // Copy to the header subset H1:I1: Amount then Rep.
    let mut wb = advanced_book();
    wb.sheets[0].set_cell(6, 8, t("stale"));
    advanced(
        &mut wb,
        0,
        &AdvancedFilter {
            list,
            criteria: crit,
            copy_to: Some((0, (0, 7, 0, 8))),
            unique: false,
        },
    )
    .unwrap();
    let got: Vec<(String, String)> = (1..=5)
        .map(|r| (text_at(&wb, r, 7), text_at(&wb, r, 8)))
        .collect();
    let want = [
        ("120", "Ann"),
        ("200", "Cara"),
        ("90", "Carl"),
        ("120", "Ann"),
        ("", ""),
    ];
    let want: Vec<(String, String)> = want
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(got, want);
    assert_eq!(text_at(&wb, 6, 8), "", "the old extract rows are cleared");
    assert_eq!(visible(&wb, 1, 9).len(), 9, "copying hides nothing");
    assert_eq!(
        wb.defined_name("_xlnm.Extract", 0),
        Some("Sheet1!$H$1:$I$1")
    );

    // Copy to K1, no criteria, unique: every column, the repeat once.
    let mut wb = advanced_book();
    advanced(
        &mut wb,
        0,
        &AdvancedFilter {
            list,
            criteria: None,
            copy_to: Some((0, (0, 10, 0, 10))),
            unique: true,
        },
    )
    .unwrap();
    let header: Vec<String> = (10..=12).map(|c| text_at(&wb, 0, c)).collect();
    assert_eq!(header, vec!["Region", "Rep", "Amount"]);
    let reps: Vec<String> = (1..=8).map(|r| text_at(&wb, r, 11)).collect();
    assert_eq!(
        reps,
        vec!["Ann", "Bob", "Cara", "Dan", "Carl", "Fay", "Eve", ""]
    );

    // Copy to Sheet2 while Sheet1 is active: refused, nothing changes.
    let mut wb = advanced_book();
    let before = wb.sheets[1].cells.len();
    let names = wb.defined_names.len();
    let err = advanced(
        &mut wb,
        0,
        &AdvancedFilter {
            list,
            criteria: crit,
            copy_to: Some((1, (0, 0, 0, 0))),
            unique: false,
        },
    );
    assert_eq!(err, Err(FilterError::OtherSheet));
    assert_eq!(
        err.unwrap_err().message(),
        "You can only copy filtered data to the active sheet."
    );
    assert_eq!(
        (wb.sheets[1].cells.len(), wb.defined_names.len()),
        (before, names)
    );
}

#[test]
fn advanced_filter_turns_an_auto_filter_off_first() {
    let mut wb = advanced_book();
    auto_filter_on_range(&mut wb, 0, (0, 0, 8, 2)).unwrap();
    set_criterion(
        &mut wb,
        0,
        1,
        Some(ColumnFilter::values(vec!["Bob".into()])),
        today(),
    )
    .unwrap();
    advanced(
        &mut wb,
        0,
        &AdvancedFilter {
            list: (0, 0, 8, 2),
            criteria: Some((0, (0, 4, 1, 4))),
            ..AdvancedFilter::default()
        },
    )
    .unwrap();
    assert!(wb.sheets[0].auto_filter.is_none());
    assert_eq!(visible(&wb, 2, 9), vec![2, 4, 6, 9]);
    assert_eq!(wb.defined_name(FILTER_DB, 0), Some("Sheet1!$A$1:$C$9"));
}

#[test]
fn an_icon_filter_over_ten_thousand_rows_reads_the_rule_once() {
    // Not a timing test: without the per-rule cache this is quadratic.
    let mut rows = vec![vec![t("Score")]];
    for r in 0..10_000u32 {
        rows.push(vec![n(f64::from(r % 97))]);
    }
    let mut wb = book(&rows);
    wb.sheets[0].cond_formats.push(crate::sheet::CondFormat {
        ranges: vec![(1, 0, 10_000, 0)],
        rules: vec![crate::sheet::CfRule {
            kind: crate::sheet::CfKind::IconSet {
                set: "3Arrows".into(),
                reverse: false,
                cfvos: ["0", "33", "67"]
                    .iter()
                    .map(|v| crate::sheet::Cfvo {
                        kind: "percentile".into(),
                        val: (*v).into(),
                        gte: true,
                    })
                    .collect(),
                formulas: Vec::new(),
            },
            dxf_id: None,
            priority: 1,
        }],
        ix: None,
    });
    auto_filter_on(&mut wb, 0, (0, 0)).unwrap();
    let up = ColumnFilter::Icon {
        set: "3Arrows".into(),
        id: 2,
    };
    let out = set_criterion(&mut wb, 0, 0, Some(up), today()).unwrap();
    assert!(out.shown > 0 && out.shown < 10_000);
    menu(&wb, 0, 0, None).unwrap();
}
