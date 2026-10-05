//! Sort options against the spec's QA cases (DAT-CASE-009, 010 and 037),
//! their data built here.

use super::*;
use crate::sheet::{CfKind, CfRule, Cfvo, CondFormat, Dxf, Sheet, Xf, parse_range_name};

fn book(cells: &[(&str, Cell)]) -> Workbook {
    let mut s = Sheet {
        name: "Sheet1".into(),
        ..Sheet::default()
    };
    for (at, cell) in cells {
        let (r, c) = crate::sheet::parse_cell_name(at).unwrap();
        s.set_cell(r, c, cell.clone());
    }
    let mut wb = Workbook {
        sheets: vec![s],
        ..Workbook::default()
    };
    wb.styles.xfs.push(Xf::default());
    wb
}

fn t(s: &str) -> Cell {
    Cell::text(s)
}

fn n(v: f64) -> Cell {
    Cell::number(v)
}

fn area(a: &str) -> Area {
    parse_range_name(a).unwrap()
}

/// The texts (numbers as written) of `col` from `r1` to `r2` (1-based).
fn column(wb: &Workbook, col: u32, r1: u32, r2: u32) -> Vec<String> {
    (r1 - 1..r2)
        .map(|r| crate::filter::shown_text(wb, 0, r, col))
        .collect()
}

fn row(wb: &Workbook, r: u32, c1: u32, c2: u32) -> Vec<String> {
    (c1..=c2)
        .map(|c| crate::filter::shown_text(wb, 0, r - 1, c))
        .collect()
}

fn asc() -> SortOn {
    SortOn::Value {
        asc: true,
        list: None,
    }
}

const HEADER: SortOptions = SortOptions {
    case_sensitive: false,
    left_to_right: false,
    header: true,
};

/// DAT-CASE-009's sheet.
fn listsort() -> Workbook {
    let mut cells = vec![("A1", t("Month"))];
    let months = ["Mar", "jan", "Dec", "Feb", "Smarch", "Jan", "Nov", "feb"];
    let names: Vec<String> = (2..=9).map(|r| format!("A{r}")).collect();
    for (at, m) in names.iter().zip(months) {
        cells.push((at.as_str(), t(m)));
    }
    let words = [
        ("C1", "Word"),
        ("C2", "apple"),
        ("C3", "Apple"),
        ("C4", "APPLE"),
        ("C5", "banana"),
        ("C6", "apple"),
    ];
    for (at, w) in words {
        cells.push((at, t(w)));
    }
    // E1:I2: row 1 = 3, 1, 2, 5, 4; row 2 = c, a, b, e, d.
    let row = [(3.0, "c"), (1.0, "a"), (2.0, "b"), (5.0, "e"), (4.0, "d")];
    let tops = ["E1", "F1", "G1", "H1", "I1"];
    let lows = ["E2", "F2", "G2", "H2", "I2"];
    for ((v, l), (a, b)) in row.into_iter().zip(tops.into_iter().zip(lows)) {
        cells.push((a, n(v)));
        cells.push((b, t(l)));
    }
    book(&cells)
}

#[test]
fn dat_case_009_custom_list_case_and_left_to_right() {
    let mut wb = listsort();
    let months = BUILTIN_SORT_LISTS[2]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let by_month = SortLevel {
        key: 0,
        on: SortOn::Value {
            asc: true,
            list: Some(months),
        },
    };
    assert_eq!(
        sort_range(&mut wb, 0, area("A1:A9"), &[by_month], &HEADER),
        Ok(8)
    );
    assert_eq!(
        column(&wb, 0, 2, 9),
        ["jan", "Jan", "Feb", "feb", "Mar", "Nov", "Dec", "Smarch"]
    );
    let case = SortOptions {
        case_sensitive: true,
        ..HEADER
    };
    sort_range(
        &mut wb,
        0,
        area("C1:C6"),
        &[SortLevel { key: 2, on: asc() }],
        &case,
    )
    .unwrap();
    assert_eq!(
        column(&wb, 2, 2, 6),
        ["apple", "apple", "Apple", "APPLE", "banana"]
    );
    // Case-insensitive, equal words keep their order.
    let mut wb2 = listsort();
    sort_range(
        &mut wb2,
        0,
        area("C1:C6"),
        &[SortLevel { key: 2, on: asc() }],
        &HEADER,
    )
    .unwrap();
    assert_eq!(
        column(&wb2, 2, 2, 6),
        ["apple", "Apple", "APPLE", "apple", "banana"]
    );
    // Left to right by row 1: the columns move, the rows don't.
    let ltr = SortOptions {
        left_to_right: true,
        ..SortOptions::default()
    };
    sort_range(
        &mut wb,
        0,
        area("E1:I2"),
        &[SortLevel { key: 0, on: asc() }],
        &ltr,
    )
    .unwrap();
    assert_eq!(row(&wb, 1, 4, 8), ["1", "2", "3", "4", "5"]);
    assert_eq!(row(&wb, 2, 4, 8), ["a", "b", "c", "d", "e"]);
    // A descending custom list reverses it; unlisted values stay last.
    let mut wb = listsort();
    let desc = SortLevel {
        key: 0,
        on: SortOn::Value {
            asc: false,
            list: Some(
                BUILTIN_SORT_LISTS[2]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        },
    };
    sort_range(&mut wb, 0, area("A1:A9"), &[desc], &HEADER).unwrap();
    assert_eq!(
        column(&wb, 0, 2, 9),
        ["Dec", "Nov", "Mar", "Feb", "feb", "jan", "Jan", "Smarch"]
    );
}

/// DAT-CASE-010's sheet: A1:C11 with green, yellow and red fills on some A
/// cells (one red by conditional formatting), red font on some B cells and
/// a 3Arrows icon set on C2:C11.
fn colorsort() -> Workbook {
    let mut cells: Vec<(String, Cell)> = vec![
        ("A1".into(), t("Item")),
        ("B1".into(), t("Note")),
        ("C1".into(), t("Score")),
    ];
    for i in 0..10u32 {
        cells.push((format!("A{}", i + 2), t(&format!("i{i}"))));
        cells.push((format!("B{}", i + 2), t(&format!("n{i}"))));
        cells.push((format!("C{}", i + 2), n(f64::from(i) * 10.0)));
    }
    let refs: Vec<(&str, Cell)> = cells.iter().map(|(a, c)| (a.as_str(), c.clone())).collect();
    let mut wb = book(&refs);
    let fill = |wb: &mut Workbook, rgb| {
        wb.styles.intern(Xf {
            fill: Some(rgb),
            ..Xf::default()
        })
    };
    let (green, yellow, red) = (
        fill(&mut wb, (0, 176, 80)),
        fill(&mut wb, (255, 255, 0)),
        fill(&mut wb, (255, 0, 0)),
    );
    let red_font = wb.styles.intern(Xf {
        color: Some((255, 0, 0)),
        ..Xf::default()
    });
    let s = &mut wb.sheets[0];
    // i0 red, i2 yellow, i3 green, i5 yellow, i7 green, i9 red by CF.
    for (r, st) in [(1, red), (3, yellow), (4, green), (6, yellow), (8, green)] {
        s.cells.get_mut(&(r, 0)).unwrap().style = st;
    }
    for r in [2, 5, 9] {
        s.cells.get_mut(&(r, 1)).unwrap().style = red_font;
    }
    s.cond_formats.push(CondFormat {
        ranges: vec![area("A2:A11")],
        rules: vec![CfRule {
            kind: CfKind::Expression {
                formula: "A2=\"i9\"".into(),
            },
            dxf_id: Some(0),
            priority: 1,
        }],
        ix: None,
    });
    s.cond_formats.push(CondFormat {
        ranges: vec![area("C2:C11")],
        rules: vec![CfRule {
            kind: CfKind::IconSet {
                set: "3Arrows".into(),
                reverse: false,
                cfvos: ["0", "33", "67"]
                    .iter()
                    .map(|v| Cfvo {
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
    wb.styles.dxfs.push(Dxf {
        fill: Some((255, 0, 0)),
        ..Dxf::default()
    });
    wb
}

#[test]
fn dat_case_010_colour_and_icon_levels() {
    let mut wb = colorsort();
    let fill = |rgb| SortLevel {
        key: 0,
        on: SortOn::CellColor {
            rgb: Some(rgb),
            top: true,
        },
    };
    let levels = [fill((0, 176, 80)), fill((255, 255, 0)), fill((255, 0, 0))];
    sort_range(&mut wb, 0, area("A1:C11"), &levels, &HEADER).unwrap();
    // Greens, yellows, reds (the CF one too), then the rest in their order.
    // The CF rule reads A's text, so i9 stays red wherever it lands.
    assert_eq!(
        column(&wb, 0, 2, 11),
        ["i3", "i7", "i2", "i5", "i0", "i9", "i1", "i4", "i6", "i8"]
    );
    // Rows move whole across the range.
    assert_eq!(column(&wb, 2, 2, 3), ["30", "70"]);
    let mut wb = colorsort();
    let font = SortLevel {
        key: 1,
        on: SortOn::FontColor {
            rgb: Some((255, 0, 0)),
            top: false,
        },
    };
    sort_range(&mut wb, 0, area("A1:C11"), &[font], &HEADER).unwrap();
    assert_eq!(
        column(&wb, 1, 2, 11),
        ["n0", "n2", "n3", "n5", "n6", "n7", "n9", "n1", "n4", "n8"]
    );
    let mut wb = colorsort();
    let icon = SortLevel {
        key: 2,
        on: SortOn::Icon {
            set: "3Arrows".into(),
            id: 2,
            top: true,
        },
    };
    sort_range(&mut wb, 0, area("A1:C11"), &[icon], &HEADER).unwrap();
    assert_eq!(
        column(&wb, 2, 2, 11),
        ["70", "80", "90", "0", "10", "20", "30", "40", "50", "60"]
    );
}

/// DAT-CASE-037's list A1:C6 (Region, Rep, Amount) and D2 beside it.
fn regions() -> Workbook {
    book(&[
        ("A1", t("Region")),
        ("B1", t("Rep")),
        ("C1", t("Amount")),
        ("A2", t("West")),
        ("B2", t("Eve")),
        ("C2", n(5.0)),
        ("A3", t("East")),
        ("B3", t("Ann")),
        ("C3", n(1.0)),
        ("A4", t("North")),
        ("B4", t("Dan")),
        ("C4", n(4.0)),
        ("A5", t("East")),
        ("B5", t("Cara")),
        ("C5", n(3.0)),
        ("A6", t("South")),
        ("B6", t("Bob")),
        ("C6", n(2.0)),
    ])
}

#[test]
fn dat_case_037_partial_selection_and_merges() {
    let mut wb = regions();
    let by_b = [SortLevel { key: 1, on: asc() }];
    // B2:B6 sits in the wider list A1:C6: Excel asks first.
    assert_eq!(
        sort_warning(&wb, 0, area("B2:B6")),
        Some((area("A1:C6"), true))
    );
    assert_eq!(sort_warning(&wb, 0, area("A1:C6")), None);
    assert_eq!(sort_warning(&wb, 0, area("B2:B2")), None);
    // Continue with the current selection: B alone.
    sort_range(&mut wb, 0, area("B2:B6"), &by_b, &SortOptions::default()).unwrap();
    assert_eq!(column(&wb, 1, 2, 6), ["Ann", "Bob", "Cara", "Dan", "Eve"]);
    assert_eq!(
        column(&wb, 0, 2, 6),
        ["West", "East", "North", "East", "South"]
    );
    // Expand the selection: A2:C6 by B, records whole.
    let mut wb = regions();
    sort_range(&mut wb, 0, area("A1:C6"), &by_b, &HEADER).unwrap();
    assert_eq!(column(&wb, 1, 2, 6), ["Ann", "Bob", "Cara", "Dan", "Eve"]);
    assert_eq!(
        column(&wb, 0, 2, 6),
        ["East", "South", "East", "North", "West"]
    );
    assert_eq!(column(&wb, 2, 2, 6), ["1", "2", "3", "4", "5"]);

    // H1:I4 with one two-cell merge: refused, nothing moves.
    let merged = || {
        book(&[
            ("H1", n(4.0)),
            ("H2", n(3.0)),
            ("H3", n(2.0)),
            ("H4", n(1.0)),
            ("I1", t("d")),
            ("I3", t("b")),
            ("I4", t("a")),
        ])
    };
    let by_h = [SortLevel { key: 7, on: asc() }];
    let mut wb = merged();
    wb.sheets[0].merges.push(area("H2:I2"));
    let before = wb.sheets[0].cells.clone();
    let got = sort_range(&mut wb, 0, area("H1:I4"), &by_h, &SortOptions::default());
    assert_eq!(got, Err(SortError::MergedSizes));
    assert_eq!(got.unwrap_err().message(), SORT_MERGED);
    assert_eq!(wb.sheets[0].cells, before);
    // A merge crossing the range's edge, or down two rows: refused too.
    for m in ["I2:J2", "H2:H3"] {
        let mut wb = merged();
        wb.sheets[0].merges.push(area(m));
        assert_eq!(
            sort_range(&mut wb, 0, area("H1:I4"), &by_h, &SortOptions::default()),
            Err(SortError::MergedSizes),
            "{m}"
        );
    }
    // Every row merged H:I the same: it sorts, and the merges stay.
    let mut wb = merged();
    for r in 1..=4 {
        wb.sheets[0].merges.push(area(&format!("H{r}:I{r}")));
    }
    sort_range(&mut wb, 0, area("H1:I4"), &by_h, &SortOptions::default()).unwrap();
    assert_eq!(column(&wb, 7, 1, 4), ["1", "2", "3", "4"]);
    assert_eq!(wb.sheets[0].merges.len(), 4);
}

#[test]
fn a_sort_moves_only_its_range_and_leaves_hidden_rows() {
    let mut wb = regions();
    wb.sheets[0].set_cell(1, 4, t("beside"));
    // Row 4 (North, Dan) hidden: it stays in row 4.
    wb.sheets[0].set_row_hidden(3, true);
    sort_range(
        &mut wb,
        0,
        area("A1:C6"),
        &[SortLevel { key: 1, on: asc() }],
        &HEADER,
    )
    .unwrap();
    assert_eq!(column(&wb, 1, 2, 6), ["Ann", "Bob", "Dan", "Cara", "Eve"]);
    assert_eq!(column(&wb, 0, 4, 4), ["North"]);
    assert_eq!(crate::filter::shown_text(&wb, 0, 1, 4), "beside");
    // Blanks sort last both ways.
    let mut wb = book(&[("A1", n(2.0)), ("A3", n(1.0)), ("A4", n(3.0))]);
    let desc = SortLevel {
        key: 0,
        on: SortOn::Value {
            asc: false,
            list: None,
        },
    };
    sort_range(&mut wb, 0, area("A1:A4"), &[desc], &SortOptions::default()).unwrap();
    assert_eq!(column(&wb, 0, 1, 4), ["3", "2", "1", ""]);
}

#[test]
fn several_levels_on_one_column_and_a_refused_spill() {
    // Two value levels on the same column are both read (no dedup).
    let mut wb = regions();
    let levels = [
        SortLevel { key: 0, on: asc() },
        SortLevel {
            key: 0,
            on: SortOn::Value {
                asc: false,
                list: None,
            },
        },
        SortLevel { key: 2, on: asc() },
    ];
    sort_range(&mut wb, 0, area("A1:C6"), &levels, &HEADER).unwrap();
    assert_eq!(column(&wb, 1, 2, 6), ["Ann", "Cara", "Dan", "Bob", "Eve"]);
    // Left to right, a block two columns wide is cut.
    let mut wb = book(&[
        ("A1", n(2.0)),
        ("B1", n(1.0)),
        ("A2", Cell::formula("SEQUENCE(1,2)")),
    ]);
    wb.sheets[0].cells.get_mut(&(1, 0)).unwrap().spill = Some((1, 2));
    let ltr = SortOptions {
        left_to_right: true,
        ..SortOptions::default()
    };
    assert_eq!(
        sort_range(
            &mut wb,
            0,
            area("A1:B2"),
            &[SortLevel { key: 0, on: asc() }],
            &ltr
        ),
        Err(SortError::CutsSpill)
    );
}
