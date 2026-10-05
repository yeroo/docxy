use super::*;
use crate::edit::{FillReq, Filled, autofill, fill_series};
use crate::edit::{STEP_OUT_OF_RANGE, STOP_UNREACHABLE};
use crate::sheet::{Sheet, Workbook, Xf, cell_name, parse_cell_name};

fn book(cells: &[(&str, Cell)]) -> Workbook {
    let mut sheet = Sheet {
        name: "Sheet1".to_string(),
        ..Sheet::default()
    };
    for (name, cell) in cells {
        let (r, c) = parse_cell_name(name).unwrap();
        sheet.set_cell(r, c, cell.clone());
    }
    let mut wb = Workbook {
        sheets: vec![sheet],
        ..Workbook::default()
    };
    wb.styles.xfs.push(Xf::default());
    wb
}

/// A style index with the format `code`.
fn style(wb: &mut Workbook, code: &str) -> u32 {
    wb.styles.intern(Xf {
        code: Some(code.to_string()),
        numfmt: classify_format_code(code),
        ..Xf::default()
    })
}

fn shown(wb: &Workbook, name: &str) -> CellValue {
    let (r, c) = parse_cell_name(name).unwrap();
    wb.sheets[0]
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn texts(wb: &Workbook, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|n| match shown(wb, n) {
            CellValue::Text(t) => t,
            v => format!("{v:?}"),
        })
        .collect()
}

fn nums(wb: &Workbook, names: &[&str]) -> Vec<f64> {
    names
        .iter()
        .map(|n| match shown(wb, n) {
            CellValue::Number(x) => x,
            v => panic!("{n} is {v:?}"),
        })
        .collect()
}

fn fill(wb: &mut Workbook, src: &str, to: &str) -> Option<Filled> {
    fill_with(wb, src, to, FillKind::Auto, false, &[])
}

fn fill_with(
    wb: &mut Workbook,
    src: &str,
    to: &str,
    kind: FillKind,
    ctrl: bool,
    lists: &[Vec<String>],
) -> Option<Filled> {
    let (a, b) = src.split_once(':').unwrap_or((src, src));
    let (r0, c0) = parse_cell_name(a).unwrap();
    let (r1, c1) = parse_cell_name(b).unwrap();
    let to = parse_cell_name(to).unwrap();
    autofill(
        wb,
        0,
        &FillReq {
            src: (r0, c0, r1, c1),
            to,
            kind,
            ctrl,
            lists,
        },
    )
}

fn col(prefix: char, rows: std::ops::RangeInclusive<u32>) -> Vec<String> {
    rows.map(|r| format!("{prefix}{r}")).collect()
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

// ---- criterion 1: text with a number counts ----------------------------------

#[test]
fn text_with_a_number_counts_and_keeps_its_padding() {
    let mut wb = book(&[
        ("A1", Cell::text("Item 1")),
        ("B1", Cell::text("Unit7")),
        ("C1", Cell::text("A001")),
    ]);
    fill(&mut wb, "A1:C1", "C4");
    assert_eq!(
        texts(&wb, &["A2", "A3", "A4"]),
        ["Item 2", "Item 3", "Item 4"]
    );
    assert_eq!(texts(&wb, &["B2", "B3"]), ["Unit8", "Unit9"]);
    assert_eq!(texts(&wb, &["C2", "C4"]), ["A002", "A004"]);
}

#[test]
fn two_text_seeds_step_by_their_difference() {
    let mut wb = book(&[("A1", Cell::text("Item 1")), ("A2", Cell::text("Item 3"))]);
    fill(&mut wb, "A1:A2", "A4");
    assert_eq!(texts(&wb, &["A3", "A4"]), ["Item 5", "Item 7"]);
}

#[test]
fn mixed_seeds_each_run_their_own_series() {
    // R9: each position continues the seeds sharing its pattern.
    let mut wb = book(&[("A1", Cell::text("Item 1")), ("A2", Cell::text("x"))]);
    fill(&mut wb, "A1:A2", "A6");
    assert_eq!(
        texts(&wb, &["A3", "A4", "A5", "A6"]),
        ["Item 2", "x", "Item 3", "x"]
    );
}

#[test]
fn a_leading_number_counts_when_there_is_no_trailing_one() {
    let mut wb = book(&[("A1", Cell::text("1 apple")), ("B1", Cell::text("a1b"))]);
    fill(&mut wb, "A1:B1", "B3");
    assert_eq!(texts(&wb, &["A2", "A3"]), ["2 apple", "3 apple"]);
    // A number in the middle doesn't count: copied.
    assert_eq!(texts(&wb, &["B2", "B3"]), ["a1b", "a1b"]);
}

#[test]
fn text_without_a_number_cycles_as_before() {
    let mut wb = book(&[("A1", Cell::text("x")), ("A2", Cell::text("y"))]);
    fill(&mut wb, "A1:A2", "A5");
    assert_eq!(texts(&wb, &["A3", "A4", "A5"]), ["x", "y", "x"]);
}

// ---- criterion 2: ordinals and quarters ---------------------------------------

#[test]
fn ordinals_take_their_proper_suffix() {
    let mut wb = book(&[("A1", Cell::text("1st"))]);
    fill(&mut wb, "A1", "A21");
    let got = texts(&wb, &refs(&col('A', 2..=21)));
    assert_eq!(&got[..3], ["2nd", "3rd", "4th"]);
    assert_eq!(&got[9..12], ["11th", "12th", "13th"]);
    assert_eq!(got[19], "21st");
    let mut wb = book(&[("A1", Cell::text("1ST"))]);
    fill(&mut wb, "A1", "A2");
    assert_eq!(texts(&wb, &["A2"]), ["2ND"]);
}

#[test]
fn quarters_wrap_at_four_and_keep_their_spelling() {
    let mut wb = book(&[
        ("A1", Cell::text("Q3")),
        ("B1", Cell::text("Qtr 4")),
        ("C1", Cell::text("quarter 2")),
    ]);
    fill(&mut wb, "A1:C1", "C3");
    assert_eq!(texts(&wb, &["A2", "A3"]), ["Q4", "Q1"]);
    assert_eq!(texts(&wb, &["B2", "B3"]), ["Qtr 1", "Qtr 2"]);
    assert_eq!(texts(&wb, &["C2", "C3"]), ["quarter 3", "quarter 4"]);
}

// ---- criterion 3: built-in lists ----------------------------------------------

#[test]
fn day_and_month_names_continue_in_their_case() {
    let mut wb = book(&[
        ("A1", Cell::text("Wed")),
        ("B1", Cell::text("November")),
        ("C1", Cell::text("MON")),
        ("D1", Cell::text("monday")),
    ]);
    fill(&mut wb, "A1:D1", "D3");
    assert_eq!(texts(&wb, &["A2", "A3"]), ["Thu", "Fri"]);
    assert_eq!(texts(&wb, &["B2", "B3"]), ["December", "January"]);
    assert_eq!(texts(&wb, &["C2", "C3"]), ["TUE", "WED"]);
    assert_eq!(texts(&wb, &["D2", "D3"]), ["tuesday", "wednesday"]);
}

#[test]
fn two_list_seeds_step_by_their_distance() {
    let mut wb = book(&[("A1", Cell::text("Mon")), ("A2", Cell::text("Wed"))]);
    fill(&mut wb, "A1:A2", "A5");
    assert_eq!(texts(&wb, &["A3", "A4", "A5"]), ["Fri", "Sun", "Tue"]);
}

// ---- criterion 4: numbers -------------------------------------------------------

#[test]
fn three_numbers_fill_along_the_least_squares_trend() {
    let mut wb = book(&[
        ("C1", Cell::number(1.0)),
        ("C2", Cell::number(2.0)),
        ("C3", Cell::number(4.0)),
    ]);
    fill(&mut wb, "C1:C3", "C5");
    let got = nums(&wb, &["C4", "C5"]);
    assert!((got[0] - 16.0 / 3.0).abs() < 1e-9, "{got:?}");
    assert!((got[1] - 41.0 / 6.0).abs() < 1e-9, "{got:?}");
}

#[test]
fn an_exact_run_extends_by_its_step_exactly() {
    // R13: least squares would leave float dust on 0.1, 0.2, 0.3.
    let mut wb = book(&[
        ("A1", Cell::number(0.1)),
        ("A2", Cell::number(0.2)),
        ("A3", Cell::number(0.3)),
    ]);
    fill(&mut wb, "A1:A3", "A5");
    let step = 0.2 - 0.1;
    assert_eq!(nums(&wb, &["A4", "A5"]), [0.3 + step, 0.3 + step * 2.0]);
    // Two numbers keep their step.
    let mut wb = book(&[("A1", Cell::number(0.0)), ("A2", Cell::number(5.0))]);
    fill(&mut wb, "A1:A2", "A4");
    assert_eq!(nums(&wb, &["A3", "A4"]), [10.0, 15.0]);
}

#[test]
fn one_number_is_copied_and_ctrl_counts_it() {
    let mut wb = book(&[("A1", Cell::number(7.0))]);
    fill(&mut wb, "A1", "A3");
    assert_eq!(nums(&wb, &["A2", "A3"]), [7.0, 7.0]);
    let mut wb = book(&[("A1", Cell::number(7.0))]);
    fill_with(&mut wb, "A1", "A3", FillKind::Auto, true, &[]);
    assert_eq!(nums(&wb, &["A2", "A3"]), [8.0, 9.0]);
    // Fill Series counts it too.
    let mut wb = book(&[("A1", Cell::number(7.0))]);
    fill_with(&mut wb, "A1", "A3", FillKind::Series, false, &[]);
    assert_eq!(nums(&wb, &["A2", "A3"]), [8.0, 9.0]);
}

#[test]
fn ctrl_copies_a_series() {
    let mut wb = book(&[("A1", Cell::number(1.0)), ("A2", Cell::number(2.0))]);
    fill_with(&mut wb, "A1:A2", "A5", FillKind::Auto, true, &[]);
    assert_eq!(nums(&wb, &["A3", "A4", "A5"]), [1.0, 2.0, 1.0]);
    let mut wb = book(&[("A1", Cell::text("Item 1"))]);
    fill_with(&mut wb, "A1", "A2", FillKind::Auto, true, &[]);
    assert_eq!(texts(&wb, &["A2"]), ["Item 1"]);
}

// ---- criterion 5: dates and times ----------------------------------------------

fn date(y: i64, m: u32, d: u32) -> f64 {
    parts_to_serial(y, m, d, 0, false)
}

#[test]
fn one_date_counts_days_and_two_step_by_their_difference() {
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    let mut d = Cell::number(date(2024, 1, 30));
    d.style = s;
    wb.sheets[0].set_cell(0, 0, d.clone());
    fill(&mut wb, "A1", "A3");
    assert_eq!(
        nums(&wb, &["A2", "A3"]),
        [date(2024, 1, 31), date(2024, 2, 1)]
    );
    assert_eq!(wb.sheets[0].cell(1, 0).unwrap().style, s, "format kept");
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    for (r, v) in [(0, date(2024, 1, 1)), (1, date(2024, 1, 8))] {
        let mut c = Cell::number(v);
        c.style = s;
        wb.sheets[0].set_cell(r, 0, c);
    }
    fill(&mut wb, "A1:A2", "A3");
    assert_eq!(nums(&wb, &["A3"]), [date(2024, 1, 15)]);
}

#[test]
fn month_steps_clamp_month_ends_from_the_first_seed() {
    // Fill Months from 31 Jan.
    let mut one = book(&[]);
    let s1 = style(&mut one, "d mmm yyyy");
    let mut c = Cell::number(date(2024, 1, 31));
    c.style = s1;
    one.sheets[0].set_cell(0, 0, c);
    fill_with(&mut one, "A1", "A4", FillKind::Months, false, &[]);
    assert_eq!(
        nums(&one, &["A2", "A3", "A4"]),
        [date(2024, 2, 29), date(2024, 3, 31), date(2024, 4, 30)]
    );
    // Two seeds on the same day of different months step by months.
    let mut two = book(&[]);
    let s2 = style(&mut two, "d mmm yyyy");
    for (r, v) in [(0, date(2024, 1, 31)), (1, date(2024, 3, 31))] {
        let mut c = Cell::number(v);
        c.style = s2;
        two.sheets[0].set_cell(r, 0, c);
    }
    fill(&mut two, "A1:A2", "A4");
    assert_eq!(
        nums(&two, &["A3", "A4"]),
        [date(2024, 5, 31), date(2024, 7, 31)]
    );
}

#[test]
fn year_steps_and_weekdays() {
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    for (r, v) in [(0, date(2020, 2, 29)), (1, date(2021, 2, 28))] {
        let mut c = Cell::number(v);
        c.style = s;
        wb.sheets[0].set_cell(r, 0, c);
    }
    fill_with(&mut wb, "A1", "A3", FillKind::Years, false, &[]);
    assert_eq!(
        nums(&wb, &["A2", "A3"]),
        [date(2021, 2, 28), date(2022, 2, 28)]
    );
    // Friday 2024-10-04 by weekdays: Mon 7th, Tue 8th.
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    let mut c = Cell::number(date(2024, 10, 4));
    c.style = s;
    wb.sheets[0].set_cell(0, 0, c);
    fill_with(&mut wb, "A1", "A3", FillKind::Weekdays, false, &[]);
    assert_eq!(
        nums(&wb, &["A2", "A3"]),
        [date(2024, 10, 7), date(2024, 10, 8)]
    );
}

#[test]
fn times_step_by_an_hour_or_their_difference() {
    let mut wb = book(&[]);
    let s = style(&mut wb, "h:mm");
    let mut c = Cell::number(9.0 / 24.0);
    c.style = s;
    wb.sheets[0].set_cell(0, 0, c);
    fill(&mut wb, "A1", "A3");
    let got = nums(&wb, &["A2", "A3"]);
    assert!((got[0] - 10.0 / 24.0).abs() < 1e-12 && (got[1] - 11.0 / 24.0).abs() < 1e-12);
    let mut wb = book(&[]);
    let s = style(&mut wb, "h:mm");
    for (r, v) in [(0, 9.0 / 24.0), (1, 9.5 / 24.0)] {
        let mut c = Cell::number(v);
        c.style = s;
        wb.sheets[0].set_cell(r, 0, c);
    }
    fill(&mut wb, "A1:A2", "A3");
    assert!((nums(&wb, &["A3"])[0] - 10.0 / 24.0).abs() < 1e-12);
}

// ---- criterion 6: up and left ---------------------------------------------------

#[test]
fn up_and_left_run_the_series_backwards() {
    let mut wb = book(&[("A3", Cell::number(3.0)), ("A4", Cell::number(4.0))]);
    assert_eq!(
        fill(&mut wb, "A3:A4", "A1"),
        Some(Filled::Extended((0, 0, 1, 0)))
    );
    assert_eq!(nums(&wb, &["A1", "A2"]), [1.0, 2.0]);
    let mut wb = book(&[("C1", Cell::text("Item 5"))]);
    fill(&mut wb, "C1", "A1");
    assert_eq!(texts(&wb, &["B1", "A1"]), ["Item 4", "Item 3"]);
}

#[test]
fn a_formula_filled_up_translates_backwards() {
    let mut wb = book(&[]);
    wb.sheets[0].set_cell(4, 1, Cell::formula("A5*2"));
    fill(&mut wb, "B5", "B3");
    let f = |r: u32| wb.sheets[0].cell(r, 1).and_then(|c| c.formula.clone());
    assert_eq!(f(3).as_deref(), Some("A4*2"));
    assert_eq!(f(2).as_deref(), Some("A3*2"));
}

// ---- criterion 7: dragging back inside clears ----------------------------------

#[test]
fn dragging_back_inside_clears_the_cells_left_behind() {
    let mut wb = book(&[
        ("A1", Cell::number(1.0)),
        ("A2", Cell::number(2.0)),
        ("A3", Cell::number(3.0)),
    ]);
    let s = style(&mut wb, "0.00");
    wb.sheets[0].cells.get_mut(&(2, 0)).unwrap().style = s;
    assert_eq!(
        fill(&mut wb, "A1:A3", "A1"),
        Some(Filled::Cleared((1, 0, 2, 0)))
    );
    assert_eq!(nums(&wb, &["A1"]), [1.0]);
    assert_eq!(shown(&wb, "A2"), CellValue::Empty);
    // Contents only: the format stays.
    assert_eq!(wb.sheets[0].cell(2, 0).unwrap().style, s);
    assert_eq!(fill(&mut wb, "A1", "A1"), None);
}

#[test]
fn fill_target_picks_the_axis_and_the_selection() {
    let src = (1, 1, 2, 2);
    let t = fill_target(src, (5, 2));
    assert_eq!(
        t,
        FillTarget::Extend {
            dir: FillDir::Down,
            dest: (3, 1, 5, 2)
        }
    );
    assert_eq!(t.selection(src), (1, 1, 5, 2));
    let t = fill_target(src, (0, 1));
    assert_eq!(
        t,
        FillTarget::Extend {
            dir: FillDir::Up,
            dest: (0, 1, 0, 2)
        }
    );
    assert_eq!(t.selection(src), (0, 1, 2, 2));
    assert_eq!(
        fill_target(src, (2, 0)),
        FillTarget::Extend {
            dir: FillDir::Left,
            dest: (1, 0, 2, 0)
        }
    );
    let t = fill_target(src, (2, 1));
    assert_eq!(t, FillTarget::Clear((1, 2, 2, 2)));
    assert_eq!(t.selection(src), (1, 1, 2, 1));
    assert_eq!(fill_target(src, (2, 2)), FillTarget::None);
}

// ---- criterion 9: fill kinds ----------------------------------------------------

#[test]
fn growth_trend_multiplies() {
    let mut wb = book(&[("A1", Cell::number(2.0)), ("A2", Cell::number(4.0))]);
    fill_with(&mut wb, "A1:A2", "A4", FillKind::GrowthTrend, false, &[]);
    let got = nums(&wb, &["A3", "A4"]);
    assert!((got[0] - 8.0).abs() < 1e-9 && (got[1] - 16.0).abs() < 1e-9);
    let mut wb = book(&[
        ("A1", Cell::number(1.0)),
        ("A2", Cell::number(2.0)),
        ("A3", Cell::number(4.0)),
    ]);
    fill_with(&mut wb, "A1:A3", "A4", FillKind::GrowthTrend, false, &[]);
    assert!((nums(&wb, &["A4"])[0] - 8.0).abs() < 1e-9);
}

#[test]
fn linear_trend_and_copy_cells() {
    let mut wb = book(&[("A1", Cell::number(1.0)), ("A2", Cell::number(3.0))]);
    fill_with(&mut wb, "A1:A2", "A3", FillKind::LinearTrend, false, &[]);
    assert_eq!(nums(&wb, &["A3"]), [5.0]);
    let mut wb = book(&[("A1", Cell::number(1.0)), ("A2", Cell::number(3.0))]);
    fill_with(&mut wb, "A1:A2", "A4", FillKind::Copy, false, &[]);
    assert_eq!(nums(&wb, &["A3", "A4"]), [1.0, 3.0]);
}

#[test]
fn formats_only_and_without_formatting() {
    let mut wb = book(&[("A1", Cell::number(1.0)), ("A2", Cell::number(2.0))]);
    let bold = wb.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    let pct = style(&mut wb, "0%");
    wb.sheets[0].cells.get_mut(&(0, 0)).unwrap().style = bold;
    wb.sheets[0].cells.get_mut(&(1, 0)).unwrap().style = bold;
    let mut dest = Cell::number(99.0);
    dest.style = pct;
    wb.sheets[0].set_cell(2, 0, dest);
    let mut a = wb.clone();
    fill_with(&mut a, "A1:A2", "A3", FillKind::FormatsOnly, false, &[]);
    let a3 = a.sheets[0].cell(2, 0).unwrap();
    assert_eq!(
        (a3.value.clone(), a3.style),
        (CellValue::Number(99.0), bold)
    );
    let mut b = wb.clone();
    fill_with(
        &mut b,
        "A1:A2",
        "A3",
        FillKind::WithoutFormatting,
        false,
        &[],
    );
    let b3 = b.sheets[0].cell(2, 0).unwrap();
    assert_eq!((b3.value.clone(), b3.style), (CellValue::Number(3.0), pct));
}

#[test]
fn fill_kind_labels_round_trip() {
    for k in FillKind::ALL {
        assert_eq!(FillKind::from_label(k.label()), Some(k));
    }
    assert_eq!(FillKind::from_label("growth"), Some(FillKind::GrowthTrend));
    assert_eq!(FillKind::from_label("nope"), None);
}

// ---- criterion 12: the Series dialog --------------------------------------------

fn spec(rows: bool, kind: SeriesType, step: f64, stop: Option<f64>, trend: bool) -> SeriesSpec {
    SeriesSpec {
        rows,
        kind,
        step,
        stop,
        trend,
    }
}

#[test]
fn series_linear_growth_and_date_steps() {
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    fill_series(
        &mut wb,
        0,
        (0, 0, 4, 0),
        &spec(false, SeriesType::Linear, 2.5, None, false),
        &[],
    )
    .unwrap();
    assert_eq!(nums(&wb, &["A2", "A3", "A4", "A5"]), [3.5, 6.0, 8.5, 11.0]);
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    fill_series(
        &mut wb,
        0,
        (0, 0, 0, 3),
        &spec(true, SeriesType::Growth, 3.0, None, false),
        &[],
    )
    .unwrap();
    assert_eq!(nums(&wb, &["B1", "C1", "D1"]), [3.0, 9.0, 27.0]);
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    let mut c = Cell::number(date(2024, 1, 31));
    c.style = s;
    wb.sheets[0].set_cell(0, 0, c);
    fill_series(
        &mut wb,
        0,
        (0, 0, 2, 0),
        &spec(false, SeriesType::Date(FillKind::Months), 1.0, None, false),
        &[],
    )
    .unwrap();
    assert_eq!(
        nums(&wb, &["A2", "A3"]),
        [date(2024, 2, 29), date(2024, 3, 31)]
    );
    assert_eq!(wb.sheets[0].cell(1, 0).unwrap().style, s);
}

#[test]
fn series_stop_value_runs_past_a_single_cell() {
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    let n = fill_series(
        &mut wb,
        0,
        (0, 0, 0, 0),
        &spec(false, SeriesType::Linear, 2.0, Some(8.0), false),
        &[],
    )
    .unwrap();
    assert_eq!(n, 3);
    assert_eq!(nums(&wb, &["A2", "A3", "A4"]), [3.0, 5.0, 7.0]);
    assert_eq!(shown(&wb, "A5"), CellValue::Empty);
    // Within a selection, it stops at the stop value too.
    let mut wb = book(&[("A1", Cell::number(10.0))]);
    fill_series(
        &mut wb,
        0,
        (0, 0, 9, 0),
        &spec(false, SeriesType::Linear, -3.0, Some(2.0), false),
        &[],
    )
    .unwrap();
    assert_eq!(nums(&wb, &["A2", "A3"]), [7.0, 4.0]);
    assert_eq!(shown(&wb, "A4"), CellValue::Empty);
}

#[test]
fn series_trend_replaces_the_seeds_with_the_fit() {
    let mut wb = book(&[
        ("A1", Cell::number(1.0)),
        ("A2", Cell::number(2.0)),
        ("A3", Cell::number(4.0)),
    ]);
    fill_series(
        &mut wb,
        0,
        (0, 0, 4, 0),
        &spec(false, SeriesType::Linear, 99.0, None, true),
        &[],
    )
    .unwrap();
    let got = nums(&wb, &["A1", "A2", "A3", "A4", "A5"]);
    let want = [5.0 / 6.0, 7.0 / 3.0, 23.0 / 6.0, 16.0 / 3.0, 41.0 / 6.0];
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-9, "{got:?}");
    }
}

#[test]
fn series_autofill_type_extends_like_the_handle() {
    let mut wb = book(&[("A1", Cell::text("Q1"))]);
    fill_series(
        &mut wb,
        0,
        (0, 0, 3, 0),
        &spec(false, SeriesType::AutoFill, 1.0, None, false),
        &[],
    )
    .unwrap();
    assert_eq!(texts(&wb, &["A2", "A3", "A4"]), ["Q2", "Q3", "Q4"]);
}

#[test]
fn series_rows_guess() {
    assert!(series_rows_for((0, 0, 0, 5)));
    assert!(!series_rows_for((0, 0, 5, 0)));
}

// ---- criterion 13: custom lists -------------------------------------------------

#[test]
fn custom_lists_continue_with_wrap_around_any_case() {
    let lists = vec![vec![
        "North".to_string(),
        "East".to_string(),
        "South".to_string(),
        "West".to_string(),
    ]];
    let mut wb = book(&[("A1", Cell::text("south"))]);
    fill_with(&mut wb, "A1", "A4", FillKind::Auto, false, &lists);
    assert_eq!(texts(&wb, &["A2", "A3", "A4"]), ["west", "north", "east"]);
    let mut wb = book(&[("A1", Cell::text("South"))]);
    fill_with(&mut wb, "A1", "A2", FillKind::Auto, false, &lists);
    assert_eq!(texts(&wb, &["A2"]), ["West"]);
    // Without the list, it is copied.
    let mut wb = book(&[("A1", Cell::text("South"))]);
    fill(&mut wb, "A1", "A2");
    assert_eq!(texts(&wb, &["A2"]), ["South"]);
}

// ---- criterion 11: justify ------------------------------------------------------

#[test]
fn justify_rewraps_to_the_width_one_word_at_least() {
    let lines = justify_lines(
        &["the quick brown".to_string(), "fox jumps".to_string()],
        10,
    );
    assert_eq!(lines, ["the quick", "brown fox", "jumps"]);
    assert_eq!(
        justify_lines(&["supercalifragilistic word".to_string()], 5),
        ["supercalifragilistic", "word"]
    );
    assert!(justify_lines(&[], 5).is_empty());
}

#[test]
fn builtin_lists_are_excels_four() {
    let l = builtin_lists();
    assert_eq!(l.len(), 4);
    assert_eq!(l[0][0], "Sun");
    assert_eq!(l[3][11], "December");
    let _ = cell_name(0, 0);
}

// ---- criterion 8: double-clicking the handle ------------------------------------

#[test]
fn a_double_click_fills_to_the_neighbours_block() {
    let mut wb = book(&[("B1", Cell::number(1.0))]);
    for r in 0..5 {
        wb.sheets[0].set_cell(r, 0, Cell::number(f64::from(r)));
    }
    assert_eq!(fill_down_to(&wb.sheets[0], (0, 1, 0, 1)), Some(4));
    // Right when the left runs out first.
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    for r in 0..3 {
        wb.sheets[0].set_cell(r, 1, Cell::number(1.0));
    }
    assert_eq!(fill_down_to(&wb.sheets[0], (0, 0, 0, 0)), Some(2));
    let wb = book(&[("A1", Cell::number(1.0))]);
    assert_eq!(fill_down_to(&wb.sheets[0], (0, 0, 0, 0)), None);
}

/// #707 r1 m2: Fill Weekdays reads the 1904 date system's weekdays.
#[test]
fn weekdays_in_a_1904_workbook() {
    // Friday 2024-10-04: Monday 7th, Tuesday 8th.
    let mut wb = book(&[]);
    wb.date1904 = true;
    let s = style(&mut wb, "yyyy-mm-dd");
    let fri = parts_to_serial(2024, 10, 4, 0, true);
    let mut c = Cell::number(fri);
    c.style = s;
    wb.sheets[0].set_cell(0, 0, c);
    fill_with(&mut wb, "A1", "A3", FillKind::Weekdays, false, &[]);
    assert_eq!(
        nums(&wb, &["A2", "A3"]),
        [
            parts_to_serial(2024, 10, 7, 0, true),
            parts_to_serial(2024, 10, 8, 0, true)
        ]
    );
}

// ---- #707 r2: a stop value that bounds, and steps that cannot run --------------

/// M1: AutoFill fills the selection and ignores a stop value; with one cell
/// selected it writes nothing, so a table below is safe.
#[test]
fn series_autofill_ignores_the_stop_value() {
    let mut wb = book(&[("A1", Cell::text("Mon")), ("A30", Cell::text("table"))]);
    let n = fill_series(
        &mut wb,
        0,
        (0, 0, 0, 0),
        &spec(false, SeriesType::AutoFill, 1.0, Some(5.0), false),
        &[],
    )
    .unwrap();
    assert_eq!(n, 0);
    assert_eq!(texts(&wb, &["A30"]), ["table"]);
    assert_eq!(shown(&wb, "A2"), CellValue::Empty);
}

/// M1: a stop the step never reaches is refused, nothing written.
#[test]
fn an_unreachable_stop_is_refused() {
    for (kind, step, stop) in [
        (SeriesType::Linear, 0.0, 9.0),
        (SeriesType::Linear, -1.0, 9.0),
        (SeriesType::Growth, 1.0, 9.0),
        (SeriesType::Growth, 2.0, 0.5),
        (SeriesType::Date(FillKind::Days), 0.0, 99.0),
    ] {
        let mut wb = book(&[("A1", Cell::number(1.0))]);
        assert_eq!(
            fill_series(
                &mut wb,
                0,
                (0, 0, 0, 0),
                &spec(false, kind, step, Some(stop), false),
                &[]
            ),
            Err(STOP_UNREACHABLE),
            "{kind:?} {step} {stop}"
        );
        assert_eq!(shown(&wb, "A2"), CellValue::Empty);
    }
    // Reachable ones still run.
    let mut wb = book(&[("A1", Cell::number(10.0))]);
    let n = fill_series(
        &mut wb,
        0,
        (0, 0, 0, 0),
        &spec(false, SeriesType::Linear, -2.0, Some(5.0), false),
        &[],
    );
    assert_eq!(n, Ok(2));
}

/// M2: a step that cannot run is refused; a whole column is capped.
#[test]
fn series_steps_out_of_range_and_the_cap() {
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    for step in [f64::NAN, f64::INFINITY, 1e300] {
        assert_eq!(
            fill_series(
                &mut wb,
                0,
                (0, 0, 9, 0),
                &spec(false, SeriesType::Linear, step, None, false),
                &[]
            ),
            Err(STEP_OUT_OF_RANGE)
        );
    }
    let all = (0, 0, crate::sheet::MAX_ROWS - 1, 0);
    let n = fill_series(
        &mut wb,
        0,
        all,
        &spec(false, SeriesType::Linear, 1.0, None, false),
        &[],
    )
    .unwrap();
    assert_eq!(n, crate::edit::MAX_PASTE_CELLS as usize - 1);
}

/// M2: add_weekdays in constant time matches the day-by-day walk.
#[test]
fn add_weekdays_matches_the_walk() {
    let walk = |serial: f64, k: i64, d1904: bool| {
        let mut s = serial;
        let dir = if k < 0 { -1.0 } else { 1.0 };
        let mut left = k.abs();
        while left > 0 {
            s += dir;
            if !weekend(s, d1904) {
                left -= 1;
            }
        }
        s
    };
    for d1904 in [false, true] {
        let base = parts_to_serial(2024, 10, 1, 0, d1904);
        for start in 0..7 {
            let from = base + f64::from(start);
            for k in -20..=20 {
                assert_eq!(
                    add_weekdays(from, k, d1904),
                    walk(from, k, d1904),
                    "{from} {k}"
                );
            }
        }
    }
    let t = std::time::Instant::now();
    let far = add_weekdays(parts_to_serial(2024, 10, 4, 0, false), 5_000_000, false);
    assert!(t.elapsed() < std::time::Duration::from_millis(50));
    assert_eq!(far - parts_to_serial(2024, 10, 4, 0, false), 7_000_000.0);
}

// ---- #707 r3 -------------------------------------------------------------------

fn one_cell(
    v0: f64,
    kind: SeriesType,
    step: f64,
    stop: f64,
) -> (Workbook, Result<usize, &'static str>) {
    let mut wb = book(&[("A1", Cell::number(v0))]);
    let r = fill_series(
        &mut wb,
        0,
        (0, 0, 0, 0),
        &spec(false, kind, step, Some(stop), false),
        &[],
    );
    (wb, r)
}

/// M1: Growth from a negative seed runs down, and its stop is tested on
/// that side; one already overshot by the first step writes nothing.
#[test]
fn growth_from_a_negative_seed() {
    let (wb, r) = one_cell(-1.0, SeriesType::Growth, 2.0, -100.0);
    assert_eq!(r, Ok(6));
    assert_eq!(nums(&wb, &["A2", "A3", "A7"]), [-2.0, -4.0, -64.0]);
    assert_eq!(shown(&wb, "A8"), CellValue::Empty);
    let (wb, r) = one_cell(-1.0, SeriesType::Growth, 2.0, -1.5);
    assert_eq!(r, Ok(0), "-2 is past -1.5 at once");
    assert_eq!(shown(&wb, "A2"), CellValue::Empty);
    let (_, r) = one_cell(-1.0, SeriesType::Growth, 2.0, 5.0);
    assert_eq!(r, Err(STOP_UNREACHABLE), "it never turns positive");
    // Shrinking toward zero never reaches zero or past it.
    let (_, r) = one_cell(8.0, SeriesType::Growth, 0.5, 0.0);
    assert_eq!(r, Err(STOP_UNREACHABLE));
    let (wb, r) = one_cell(8.0, SeriesType::Growth, 0.5, 1.0);
    assert_eq!(r, Ok(3));
    assert_eq!(nums(&wb, &["A4"]), [1.0]);
}

/// M2: a series that does not move never reaches its stop, even one equal
/// to its seed.
#[test]
fn a_stationary_series_with_its_seed_as_the_stop_is_refused() {
    for (v0, kind, step) in [
        (5.0, SeriesType::Linear, 0.0),
        (5.0, SeriesType::Growth, 1.0),
        (0.0, SeriesType::Growth, 3.0),
        (5.0, SeriesType::Date(FillKind::Days), 0.0),
    ] {
        let (wb, r) = one_cell(v0, kind, step, v0);
        assert_eq!(r, Err(STOP_UNREACHABLE), "{kind:?} {step}");
        assert_eq!(shown(&wb, "A2"), CellValue::Empty);
    }
}

/// m1: descending weekday seeds count as the day-by-day walk did, from
/// every start day, both ways.
#[test]
fn weekdays_between_matches_the_walk_both_ways() {
    let walk = |a: f64, b: f64, d1904: bool| {
        let mut k = 0i64;
        let mut s = a;
        let dir = if b >= a { 1.0 } else { -1.0 };
        while (dir > 0.0 && s < b) || (dir < 0.0 && s > b) {
            s += dir;
            if !weekend(s, d1904) {
                k += dir as i64;
            }
        }
        k
    };
    for d1904 in [false, true] {
        let base = parts_to_serial(2024, 10, 1, 0, d1904);
        for a in 0..7 {
            for gap in -16..=16 {
                let (x, y) = (base + f64::from(a), base + f64::from(a + gap));
                assert_eq!(weekdays_between(x, y, d1904), walk(x, y, d1904), "{x} {y}");
            }
        }
    }
    // Two seeds on one weekend, descending, step backwards.
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    for (r, (d, m)) in [(6u32, 10u32), (5, 10)].into_iter().enumerate() {
        let mut c = Cell::number(parts_to_serial(2024, m, d, 0, false));
        c.style = s;
        wb.sheets[0].set_cell(r as u32, 0, c);
    }
    fill_with(&mut wb, "A1:A2", "A3", FillKind::Weekdays, false, &[]);
    assert_eq!(nums(&wb, &["A3"]), [parts_to_serial(2024, 10, 4, 0, false)]);
}

/// m2: the weekday arithmetic leaves a non-date alone, and a date series
/// that would step off the calendar is refused.
#[test]
fn weekdays_off_the_calendar_and_huge_steps() {
    let t = std::time::Instant::now();
    assert_eq!(add_weekdays(1e300, 3, false), 1e300);
    assert!(add_weekdays(f64::NAN, 3, false).is_nan());
    assert_eq!(add_weekdays(-5.0, 3, false), -5.0);
    let far = add_weekdays(45000.0, i64::MAX, false);
    assert!(far > MAX_SERIAL);
    assert_eq!(weekdays_between(1e300, 45000.0, false), 0);
    assert!(t.elapsed() < std::time::Duration::from_millis(50));
    let mut wb = book(&[("A1", Cell::number(45000.0))]);
    let r = fill_series(
        &mut wb,
        0,
        (0, 0, 9, 0),
        &spec(false, SeriesType::Date(FillKind::Years), 1e9, None, false),
        &[],
    );
    assert_eq!(r, Err(STEP_OUT_OF_RANGE));
    assert_eq!(shown(&wb, "A2"), CellValue::Empty);
}

/// #707 r4 m1: uneven month seeds continue from the last, by the last step.
#[test]
fn uneven_month_seeds_continue_from_the_last() {
    let mut wb = book(&[]);
    let s = style(&mut wb, "yyyy-mm-dd");
    for (r, m) in [(0u32, 1u32), (1, 3), (2, 4)] {
        let mut c = Cell::number(parts_to_serial(2024, m, 15, 0, false));
        c.style = s;
        wb.sheets[0].set_cell(r, 0, c);
    }
    fill_with(&mut wb, "A1:A3", "A4", FillKind::Months, false, &[]);
    assert_eq!(nums(&wb, &["A4"]), [parts_to_serial(2024, 5, 15, 0, false)]);
}
