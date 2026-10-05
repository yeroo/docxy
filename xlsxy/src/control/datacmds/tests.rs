//! The Sort & Filter verbs, following the spec's QA cases step by step
//! (DAT-CASE-009, 010, 012–017, 019, 037 and 039); each case's workbook is
//! built here.

use super::super::dispatch;
use super::*;
use gridcore::sheet::{Cell, Xf};
use gridcore::xlsx::new_xlsx;

fn app_with(cells: &[(u32, u32, Cell)]) -> App {
    let mut a = App::new(new_xlsx(), "ctl-filter.xlsx");
    a.os_clip = None;
    for (r, c, cell) in cells {
        a.pkg.workbook.sheets[0].set_cell(*r, *c, cell.clone());
    }
    a.rebuild_engine();
    a
}

fn call(a: &mut App, verb: &str, args: &str) -> Result<Json, String> {
    dispatch(a, verb, &Json::parse(args).expect("test args parse"))
}

fn ok(a: &mut App, verb: &str, args: &str) -> Json {
    call(a, verb, args).unwrap_or_else(|e| panic!("{verb} {args}: {e}"))
}

/// The 1-based rows `sheet.rows` reports visible in `range`.
fn shown(a: &mut App, range: &str) -> Vec<u32> {
    let j = ok(a, "sheet.rows", &format!(r#"{{"range":"{range}"}}"#));
    j.get("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r.get("hidden") == Some(&Json::Bool(false)))
        .map(|r| r.get_usize("row").unwrap() as u32)
        .collect()
}

fn text(a: &App, at: &str) -> String {
    let (r, c) = parse_cell_name(at).unwrap();
    a.pkg.workbook.sheets[0]
        .cell(r, c)
        .map(|cl| match &cl.value {
            gridcore::sheet::CellValue::Number(n) => gridcore::sheet::fmt_general(*n),
            gridcore::sheet::CellValue::Text(t) => t.clone(),
            _ => String::new(),
        })
        .unwrap_or_default()
}

const REPS: [&str; 5] = ["Noor", "Cy", "Bo", "Ann", "Noor"];
const PRODUCTS: [&str; 3] = ["Stapler", "Pen", "Desk"];

/// DAT-CASE-012's `filterlist`: A1:D21 Rep, Product, Units, Price (two
/// decimals).
fn filterlist() -> App {
    let mut cells = vec![
        (0, 0, Cell::text("Rep")),
        (0, 1, Cell::text("Product")),
        (0, 2, Cell::text("Units")),
        (0, 3, Cell::text("Price")),
    ];
    for i in 0..20u32 {
        let r = i + 1;
        cells.push((r, 0, Cell::text(REPS[i as usize % 5])));
        cells.push((r, 1, Cell::text(PRODUCTS[i as usize % 3])));
        cells.push((r, 2, Cell::number(f64::from((i * 7) % 50))));
        cells.push((
            r,
            3,
            Cell::number(if i == 4 { 5.0 } else { 1.25 * f64::from(i) }),
        ));
    }
    let mut a = app_with(&cells);
    let mut xf = Xf::default();
    xf.set_code(Some("0.00".into()));
    let money = a.pkg.workbook.styles.intern(xf);
    for r in 1..=20 {
        a.pkg.workbook.sheets[0]
            .cells
            .get_mut(&(r, 3))
            .unwrap()
            .style = money;
    }
    a
}

fn expect(keep: impl Fn(usize) -> bool) -> Vec<u32> {
    (0..20usize)
        .filter(|&i| keep(i))
        .map(|i| i as u32 + 2)
        .collect()
}

#[test]
fn dat_case_012_two_columns_and_the_record_count() {
    let mut a = filterlist();
    let r = ok(
        &mut a,
        "filter.set",
        r#"{"range":"A1:D21","col":"Rep","criteria":{"values":["Noor","Cy"]}}"#,
    );
    assert_eq!(r.get_usize("total"), Some(20));
    let r = ok(
        &mut a,
        "filter.set",
        r#"{"col":"Product","criteria":{"values":["Stapler"]}}"#,
    );
    let want = expect(|i| matches!(REPS[i % 5], "Noor" | "Cy") && PRODUCTS[i % 3] == "Stapler");
    assert_eq!(shown(&mut a, "A2:A21"), want);
    let status = format!("{} of 20 records found", want.len());
    assert_eq!(r.get_str("status"), Some(status.as_str()));
    assert_eq!(a.status.as_deref(), Some(status.as_str()));
    let rows = ok(&mut a, "sheet.rows", r#"{"range":"A2:A21"}"#);
    for row in rows.get("rows").unwrap().as_array().unwrap() {
        let hidden = row.get("hidden") == Some(&Json::Bool(true));
        let by = row.get_str("hiddenBy");
        assert_eq!(by, hidden.then_some("filter"));
    }
    // One undo step puts the first filter's rows back.
    a.undo();
    assert_eq!(
        shown(&mut a, "A2:A21"),
        expect(|i| matches!(REPS[i % 5], "Noor" | "Cy"))
    );
    a.undo();
    assert_eq!(shown(&mut a, "A2:A21").len(), 20);
    assert!(a.pkg.workbook.sheets[0].auto_filter.is_none());
}

#[test]
fn dat_case_013_custom_conditions() {
    let codes = ["A-1", "a*2", "B?3", "AB12"];
    // A fresh open of the list, with E Code.
    let fresh = || {
        let mut a = filterlist();
        a.pkg.workbook.sheets[0].set_cell(0, 4, Cell::text("Code"));
        for i in 0..20u32 {
            a.pkg.workbook.sheets[0].set_cell(i + 1, 4, Cell::text(codes[i as usize % 4]));
        }
        a
    };
    let code = |i: usize| codes[i % 4];
    let units = |i: usize| ((i * 7) % 50) as f64;
    let cases: Vec<(&str, &str, Vec<u32>)> = vec![
        (
            "Code",
            r#"[["beginsWith","a"]]"#,
            expect(|i| code(i) != "B?3"),
        ),
        (
            "Code",
            r#"[["equal","a~*2"]]"#,
            expect(|i| code(i) == "a*2"),
        ),
        ("Code", r#"[["contains","?"]]"#, expect(|_| true)),
        (
            "Code",
            r#"[["contains","~?"]]"#,
            expect(|i| code(i) == "B?3"),
        ),
        (
            "Units",
            r#"[["greaterThan",10],["lessThanOrEqual",30]], "and":true"#,
            expect(|i| units(i) > 10.0 && units(i) <= 30.0),
        ),
        (
            "Units",
            r#"[["lessThan",5],["greaterThan",40]]"#,
            expect(|i| units(i) < 5.0 || units(i) > 40.0),
        ),
        ("Price", r#"[["equal",5]]"#, vec![6]),
    ];
    for (col, conds, want) in cases {
        let mut a = fresh();
        let args = format!(
            r#"{{"range":"A1:E21","col":"{col}","criteria":{{"custom":{{"conds":{conds}}}}}}}"#
        );
        ok(&mut a, "filter.set", &args);
        assert_eq!(shown(&mut a, "A2:A21"), want, "{col} {conds}");
    }
}

/// DAT-CASE-014's `topfilter`: A1:A13 Score, 90, 85, 85, 70, 60, (blank),
/// text, 85, 40, 20, 10, 95.
fn topfilter() -> App {
    let mut cells = vec![(0, 0, Cell::text("Score"))];
    let vals = [
        90.0, 85.0, 85.0, 70.0, 60.0, -1.0, -2.0, 85.0, 40.0, 20.0, 10.0, 95.0,
    ];
    for (i, v) in vals.into_iter().enumerate() {
        let r = i as u32 + 1;
        match v {
            -1.0 => {}
            -2.0 => cells.push((r, 0, Cell::text("text"))),
            v => cells.push((r, 0, Cell::number(v))),
        }
    }
    let mut a = app_with(&cells);
    // A blank in the middle: the list is A1:A13 all the same.
    ok(
        &mut a,
        "filter.set",
        r#"{"range":"A1:A13","col":"A","criteria":null}"#,
    );
    a
}

#[test]
fn dat_case_014_top_10_and_averages() {
    let mut a = topfilter();
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Score","criteria":{"top":{"n":3}}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A13"), vec![2, 3, 4, 9, 13]);
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Score","criteria":{"top":{"n":25,"percent":true}}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A13"), vec![2, 13]);
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Score","criteria":{"top":{"n":2,"bottom":true}}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A13"), vec![11, 12]);
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Score","criteria":{"dynamic":"aboveAverage"}}"#,
    );
    let first = shown(&mut a, "A2:A13");
    assert_eq!(first, vec![2, 3, 4, 5, 9, 13]);
    ok(&mut a, "cell.set", r#"{"ref":"A11","text":"1000"}"#);
    assert_eq!(shown(&mut a, "A2:A13"), first, "not live");
    ok(&mut a, "filter.reapply", "{}");
    assert_eq!(shown(&mut a, "A2:A13"), vec![11]);
}

#[test]
fn dat_case_015_date_filters_with_a_fixed_clock() {
    let dates = [
        "2023-03-09",
        "2023-07-01",
        "2024-01-01",
        "2024-03-09",
        "2024-03-10",
        "2024-03-13",
        "2024-03-16",
        "2024-03-17",
        "2024-04-01",
        "2025-03-03",
    ];
    let mut a = app_with(&[(0, 0, Cell::text("Date"))]);
    for (i, d) in dates.iter().enumerate() {
        ok(
            &mut a,
            "cell.set",
            &format!(r#"{{"ref":"A{}","text":"{d}"}}"#, i + 2),
        );
    }
    ok(&mut a, "wb.clock", r#"{"date":"2024-03-13"}"#);
    ok(&mut a, "cell.set", r#"{"ref":"C1","text":"=TODAY()"}"#);
    assert_eq!(text(&a, "C1"), "45364");
    let set = |a: &mut App, k: &str| {
        ok(
            a,
            "filter.set",
            &format!(r#"{{"range":"A1:A11","col":"Date","criteria":{{"dynamic":"{k}"}}}}"#),
        );
        shown(a, "A2:A11")
    };
    assert_eq!(set(&mut a, "thisWeek"), vec![6, 7, 8]);
    assert_eq!(set(&mut a, "thisMonth"), vec![5, 6, 7, 8, 9]);
    assert_eq!(set(&mut a, "yearToDate"), vec![4, 5, 6, 7]);
    assert_eq!(set(&mut a, "M3"), vec![2, 5, 6, 7, 8, 9, 11]);
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Date","criteria":{"values":[],"dates":[{"year":2024,"month":3}]}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A11"), vec![5, 6, 7, 8, 9]);
    let m = ok(&mut a, "filter.menu", r#"{"col":"Date"}"#);
    assert_eq!(m.get_str("submenu"), Some("Date Filters"));
    ok(&mut a, "wb.clock", r#"{"date":null}"#);
}

/// DAT-CASE-016's colours on A1:B6: A green/red fills, B a red font.
#[test]
fn dat_case_016_colours_and_the_selected_cell() {
    let mut a = app_with(&[
        (0, 0, Cell::text("Item")),
        (0, 1, Cell::text("Note")),
        (1, 0, Cell::text("Pen")),
        (2, 0, Cell::text("Cup")),
        (3, 0, Cell::text("Pen")),
        (4, 0, Cell::text("Box")),
        (5, 0, Cell::text("Pen")),
        (1, 1, Cell::text("x")),
        (2, 1, Cell::text("y")),
        (3, 1, Cell::text("z")),
        (4, 1, Cell::text("w")),
        (5, 1, Cell::text("v")),
    ]);
    let styles = &mut a.pkg.workbook.styles;
    let green = styles.intern(Xf {
        fill: Some((0, 176, 80)),
        ..Xf::default()
    });
    let red_font = styles.intern(Xf {
        color: Some((255, 0, 0)),
        ..Xf::default()
    });
    let s = &mut a.pkg.workbook.sheets[0];
    s.cells.get_mut(&(1, 0)).unwrap().style = green;
    s.cells.get_mut(&(4, 0)).unwrap().style = green;
    s.cells.get_mut(&(2, 1)).unwrap().style = red_font;
    ok(
        &mut a,
        "filter.set",
        r#"{"range":"A1:B6","col":"Item","criteria":{"cellColor":"FF00B050"}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A6"), vec![2, 5]);
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Item","criteria":{"cellColor":null}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A6"), vec![3, 4, 6]);
    ok(&mut a, "filter.clear", "{}");
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Note","criteria":{"fontColor":"FF0000"}}"#,
    );
    assert_eq!(shown(&mut a, "A2:A6"), vec![3]);
    ok(&mut a, "filter.clear", "{}");
    ok(&mut a, "filter.by-cell", r#"{"ref":"A2","by":"value"}"#);
    assert_eq!(shown(&mut a, "A2:A6"), vec![2, 4, 6]);
}

#[test]
fn dat_case_017_reapply_clear_and_off() {
    let mut a = filterlist();
    ok(
        &mut a,
        "filter.set",
        r#"{"range":"A1:D21","col":"Rep","criteria":{"values":["Noor"]}}"#,
    );
    let noor = expect(|i| REPS[i % 5] == "Noor");
    let first = noor[0];
    ok(
        &mut a,
        "cell.set",
        &format!(r#"{{"ref":"A{first}","text":"Bo"}}"#),
    );
    assert_eq!(
        shown(&mut a, "A2:A21"),
        noor,
        "still visible after the edit"
    );
    ok(&mut a, "filter.reapply", "{}");
    assert_eq!(shown(&mut a, "A2:A21"), noor[1..].to_vec());
    let r = ok(&mut a, "filter.clear", r#"{"col":"Rep"}"#);
    assert_eq!(r.get_str("status"), Some("20 of 20 records found"));
    assert!(
        a.pkg.workbook.sheets[0].auto_filter.is_some(),
        "the buttons stay"
    );
    ok(&mut a, "filter.off", "{}");
    assert!(a.pkg.workbook.sheets[0].auto_filter.is_none());
    assert!(call(&mut a, "filter.reapply", "{}").is_err());
}

#[test]
fn dat_case_019_advanced_filter() {
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
    let mut cells = vec![
        (0, 0, Cell::text("Region")),
        (0, 1, Cell::text("Rep")),
        (0, 2, Cell::text("Amount")),
        (0, 4, Cell::text("Region")),
        (1, 4, Cell::text("East")),
        (0, 7, Cell::text("Amount")),
        (0, 8, Cell::text("Rep")),
    ];
    for (i, (g, r, n)) in recs.into_iter().enumerate() {
        let row = i as u32 + 1;
        cells.push((row, 0, Cell::text(g)));
        cells.push((row, 1, Cell::text(r)));
        cells.push((row, 2, Cell::number(n)));
    }
    let mut a = app_with(&cells);
    a.pkg.add_sheet("Sheet2");
    let r = ok(
        &mut a,
        "filter.advanced",
        r#"{"list":"A1:C9","criteria":"E1:E2"}"#,
    );
    assert_eq!(r.get_str("status"), Some("4 of 8 records found"));
    assert_eq!(shown(&mut a, "A2:A9"), vec![2, 4, 6, 9]);
    ok(&mut a, "filter.clear", "{}");
    assert_eq!(shown(&mut a, "A2:A9").len(), 8);
    ok(
        &mut a,
        "filter.advanced",
        r#"{"list":"A1:C9","criteria":"E1:E2","copyTo":"H1:I1"}"#,
    );
    let got: Vec<String> = (2..=5)
        .flat_map(|r| [text(&a, &format!("H{r}")), text(&a, &format!("I{r}"))])
        .collect();
    assert_eq!(
        got,
        ["120", "Ann", "200", "Cara", "90", "Carl", "120", "Ann"]
    );
    ok(
        &mut a,
        "filter.advanced",
        r#"{"list":"A1:C9","copyTo":"K1","unique":true}"#,
    );
    let reps: Vec<String> = (2..=9).map(|r| text(&a, &format!("L{r}"))).collect();
    assert_eq!(
        reps,
        ["Ann", "Bob", "Cara", "Dan", "Carl", "Fay", "Eve", ""]
    );
    let before = a.pkg.workbook.sheets[1].cells.len();
    let err = call(
        &mut a,
        "filter.advanced",
        r#"{"list":"A1:C9","criteria":"E1:E2","copyTo":"Sheet2!A1"}"#,
    );
    assert_eq!(
        err.unwrap_err(),
        "You can only copy filtered data to the active sheet."
    );
    assert_eq!(a.pkg.workbook.sheets[1].cells.len(), before);
    let wb = &a.pkg.workbook;
    assert_eq!(
        wb.defined_name("_xlnm.Criteria", 0),
        Some("Sheet1!$E$1:$E$2")
    );
    assert_eq!(
        wb.defined_name("_xlnm.Extract", 0),
        Some("Sheet1!$K$1:$M$1")
    );
}

#[test]
fn dat_case_039_menu_search_and_growth() {
    let mut cells = vec![(0, 0, Cell::text("Code")), (0, 1, Cell::text("Qty"))];
    for i in 0..12_000u32 {
        cells.push((i + 1, 0, Cell::text(&format!("A{i}"))));
        let q = if i == 3 || i == 8 {
            Cell::text("n/a")
        } else {
            Cell::number(f64::from(i))
        };
        cells.push((i + 1, 1, q));
    }
    let mut a = app_with(&cells);
    ok(
        &mut a,
        "filter.set",
        r#"{"range":"A1:B12001","col":"Code","criteria":null}"#,
    );
    let m = ok(&mut a, "filter.menu", r#"{"col":"Code"}"#);
    assert_eq!(m.get_str("submenu"), Some("Text Filters"));
    assert_eq!(m.get("truncated"), Some(&Json::Bool(true)));
    assert_eq!(m.get("items").unwrap().as_array().unwrap().len(), 10_000);
    let m = ok(&mut a, "filter.menu", r#"{"col":"Qty"}"#);
    assert_eq!(m.get_str("submenu"), Some("Number Filters"));
    let m = ok(&mut a, "filter.menu", r#"{"col":"Code","search":"7*1"}"#);
    assert_eq!(m.get("truncated"), Some(&Json::Bool(false)));
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Code","criteria":{"search":"A1"}}"#,
    );
    let a1 = shown(&mut a, "A2:A12001").len();
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Code","criteria":{"search":"7*1","add":true}}"#,
    );
    let both = shown(&mut a, "A2:A12001");
    assert!(both.len() > a1 && both.contains(&3) && both.contains(&703));
    ok(&mut a, "cell.set", r#"{"ref":"A12002","text":"A7z1"}"#);
    ok(
        &mut a,
        "filter.set",
        r#"{"col":"Code","criteria":{"search":"7*1"}}"#,
    );
    assert!(shown(&mut a, "A12002:A12002").contains(&12002));
    let af = a.pkg.workbook.sheets[0].auto_filter.as_ref().unwrap();
    assert_eq!(af.range.2, 12001);
}

#[test]
fn dat_case_009_and_037_sorts() {
    // 009: a custom list, case sensitive, left to right.
    let months = ["Mar", "jan", "Dec", "Feb", "Smarch", "Jan", "Nov", "feb"];
    let mut cells = vec![(0, 0, Cell::text("Month"))];
    for (i, m) in months.iter().enumerate() {
        cells.push((i as u32 + 1, 0, Cell::text(m)));
    }
    for (i, w) in ["Word", "apple", "Apple", "APPLE", "banana", "apple"]
        .iter()
        .enumerate()
    {
        cells.push((i as u32, 2, Cell::text(w)));
    }
    for (i, (v, l)) in [(3.0, "c"), (1.0, "a"), (2.0, "b"), (5.0, "e"), (4.0, "d")]
        .iter()
        .enumerate()
    {
        cells.push((0, 4 + i as u32, Cell::number(*v)));
        cells.push((1, 4 + i as u32, Cell::text(l)));
    }
    let mut a = app_with(&cells);
    ok(
        &mut a,
        "range.sort",
        r#"{"range":"A1:A9","header":true,"keys":[{"col":"A","order":"list:Jan,Feb,Mar,Apr,May,Jun,Jul,Aug,Sep,Oct,Nov,Dec"}]}"#,
    );
    let got: Vec<String> = (2..=9).map(|r| text(&a, &format!("A{r}"))).collect();
    assert_eq!(
        got,
        ["jan", "Jan", "Feb", "feb", "Mar", "Nov", "Dec", "Smarch"]
    );
    ok(
        &mut a,
        "range.sort",
        r#"{"range":"C1:C6","header":true,"caseSensitive":true,"keys":[{"col":"C"}]}"#,
    );
    let got: Vec<String> = (2..=6).map(|r| text(&a, &format!("C{r}"))).collect();
    assert_eq!(got, ["apple", "apple", "Apple", "APPLE", "banana"]);
    ok(
        &mut a,
        "range.sort",
        r#"{"range":"E1:I2","orientation":"columns","keys":[{"row":1}]}"#,
    );
    let row1: Vec<String> = ["E1", "F1", "G1", "H1", "I1"]
        .iter()
        .map(|c| text(&a, c))
        .collect();
    let row2: Vec<String> = ["E2", "F2", "G2", "H2", "I2"]
        .iter()
        .map(|c| text(&a, c))
        .collect();
    assert_eq!(
        (row1, row2),
        (
            vec!["1", "2", "3", "4", "5"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            vec!["a", "b", "c", "d", "e"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        )
    );

    // 037: the Sort Warning, then each answer; merged cells refuse.
    let recs = [
        ("West", "Eve", 5.0),
        ("East", "Ann", 1.0),
        ("North", "Dan", 4.0),
        ("East", "Cara", 3.0),
        ("South", "Bob", 2.0),
    ];
    let mut cells = vec![
        (0, 0, Cell::text("Region")),
        (0, 1, Cell::text("Rep")),
        (0, 2, Cell::text("Amount")),
    ];
    for (i, (g, r, n)) in recs.into_iter().enumerate() {
        let row = i as u32 + 1;
        cells.push((row, 0, Cell::text(g)));
        cells.push((row, 1, Cell::text(r)));
        cells.push((row, 2, Cell::number(n)));
    }
    cells.extend([
        (0, 7, Cell::number(2.0)),
        (1, 7, Cell::number(1.0)),
        (2, 7, Cell::number(4.0)),
        (3, 7, Cell::number(3.0)),
    ]);
    let fresh = |cells: &[(u32, u32, Cell)]| {
        let mut a = app_with(cells);
        a.pkg.workbook.sheets[0].merges.push((1, 7, 1, 8));
        a
    };
    let mut a = fresh(&cells);
    let asked = ok(
        &mut a,
        "range.sort",
        r#"{"range":"B2:B6","keys":[{"col":"B"}]}"#,
    );
    assert_eq!(asked.get("sorted"), Some(&Json::Bool(false)));
    assert_eq!(asked.get_str("warning"), Some(SORT_WARNING));
    assert_eq!(asked.get_str("expanded"), Some("A1:C6"));
    assert_eq!(text(&a, "B2"), "Eve", "nothing moved");
    ok(
        &mut a,
        "range.sort",
        r#"{"range":"B2:B6","expand":false,"keys":[{"col":"B"}]}"#,
    );
    let reps: Vec<String> = (2..=6).map(|r| text(&a, &format!("B{r}"))).collect();
    assert_eq!(reps, ["Ann", "Bob", "Cara", "Dan", "Eve"]);
    assert_eq!(text(&a, "A2"), "West", "the other columns stay");
    let mut a = fresh(&cells);
    ok(
        &mut a,
        "range.sort",
        r#"{"range":"B2:B6","expand":true,"keys":[{"col":"B"}]}"#,
    );
    let regions: Vec<String> = (2..=6).map(|r| text(&a, &format!("A{r}"))).collect();
    assert_eq!(regions, ["East", "South", "East", "North", "West"]);
    let err = call(
        &mut a,
        "range.sort",
        r#"{"range":"H1:I4","keys":[{"col":"H"}]}"#,
    );
    assert_eq!(
        err.unwrap_err(),
        "To do this, all the merged cells need to be the same size."
    );
    assert_eq!(text(&a, "H1"), "2");
}

#[test]
fn dat_case_010_colour_sort() {
    let mut a = app_with(&[
        (0, 0, Cell::text("Item")),
        (1, 0, Cell::text("i0")),
        (2, 0, Cell::text("i1")),
        (3, 0, Cell::text("i2")),
        (4, 0, Cell::text("i3")),
    ]);
    let green = a.pkg.workbook.styles.intern(Xf {
        fill: Some((0, 176, 80)),
        ..Xf::default()
    });
    a.pkg.workbook.sheets[0]
        .cells
        .get_mut(&(3, 0))
        .unwrap()
        .style = green;
    ok(
        &mut a,
        "range.sort",
        r#"{"range":"A1:A5","header":true,"keys":[{"col":"A","on":"cell-color","color":"FF00B050","position":"top"}]}"#,
    );
    let got: Vec<String> = (2..=5).map(|r| text(&a, &format!("A{r}"))).collect();
    assert_eq!(got, ["i2", "i0", "i1", "i3"]);
}
