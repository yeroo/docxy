//! The harness's own fixture workbook, and the generator that made it.
//!
//! `fixtures/basic.xlsx` is committed, because a UI test must not depend on a
//! build step running first — but a committed binary nobody can regenerate is
//! the other failure, so the recipe lives here next to the check:
//!
//! ```text
//! UIHARNESS_REGEN_FIXTURE=1 cargo test -p uiharness --test fixture
//! ```
//!
//! Without that variable the test only *reads* the committed file and asserts
//! it still holds what the cases in `cases/` are written against. A fixture
//! that quietly lost its chart would otherwise show up as three unrelated
//! assertion failures against a running window.
//!
//! **Nothing here reads the user's documents.** The workbook is built from
//! literals in this file, which is the whole point of a fixture living under
//! the harness's own directory.

use gridcore::sheet::{Cell, CellValue, ChartData, ChartSeries, DrawingKind};
use gridcore::xlsx::{load_xlsx, new_xlsx, save_xlsx};
use std::path::PathBuf;

/// Where the fixture lives, relative to this crate.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("basic.xlsx")
}

/// The rows the fixture holds: a header row and four regions with two
/// quarters each. Small enough that every cell a case touches is on screen at
/// the default scroll, which is what lets a case name `A1:C5` and mean it.
const HEADERS: [&str; 3] = ["Region", "Q1", "Q2"];
const ROWS: [(&str, f64, f64); 4] = [
    ("North", 10.0, 14.0),
    ("South", 20.0, 24.0),
    ("East", 30.0, 34.0),
    ("West", 40.0, 44.0),
];

/// Build the fixture from literals.
fn build() -> Vec<u8> {
    let mut pkg = new_xlsx();
    {
        let sheet = &mut pkg.workbook.sheets[0];
        for (c, h) in HEADERS.iter().enumerate() {
            sheet.set_cell(0, c as u32, Cell::text(h));
        }
        for (r, (name, q1, q2)) in ROWS.iter().enumerate() {
            let row = r as u32 + 1;
            sheet.set_cell(row, 0, Cell::text(name));
            sheet.set_cell(row, 1, Cell::number(*q1));
            sheet.set_cell(row, 2, Cell::number(*q2));
        }
    }
    // A chart, so the "one selection at a time" case has something to select.
    // Anchored well below and right of A1:D5: every case that drags over cells
    // uses that corner, and a card sitting on top of it would put a chart press
    // where a cell press was meant.
    let data = ChartData {
        title: "Quarters".to_string(),
        kind: "bar".to_string(),
        categories: ROWS.iter().map(|(n, _, _)| n.to_string()).collect(),
        series: vec![
            ChartSeries {
                name: "Q1".to_string(),
                values: ROWS.iter().map(|(_, q, _)| *q).collect(),
                ..ChartSeries::default()
            },
            ChartSeries {
                name: "Q2".to_string(),
                values: ROWS.iter().map(|(_, _, q)| *q).collect(),
                ..ChartSeries::default()
            },
        ],
        ..ChartData::default()
    };
    pkg.add_chart(0, (8, 1), (20, 7), &data);
    save_xlsx(&pkg)
}

/// A cell's value as a string, for the assertions below.
fn shown(v: &CellValue) -> String {
    match v {
        CellValue::Empty => String::new(),
        CellValue::Number(n) => {
            if n.fract() == 0.0 {
                format!("{n:.0}")
            } else {
                n.to_string()
            }
        }
        CellValue::Text(s) => s.clone(),
        CellValue::Bool(b) => b.to_string(),
        CellValue::Error(e) => e.clone(),
    }
}

/// What every case in `cases/` assumes about the fixture. Asserted against
/// whatever is on disk, so a regeneration that changed the shape fails here
/// rather than against a window.
fn check(bytes: &[u8]) {
    let pkg = load_xlsx(bytes).expect("the fixture is a readable xlsx");
    assert_eq!(pkg.workbook.sheets.len(), 1, "one sheet");
    let sheet = &pkg.workbook.sheets[0];
    assert_eq!(sheet.name, "Sheet1");
    for (c, h) in HEADERS.iter().enumerate() {
        let cell = sheet
            .cells
            .get(&(0, c as u32))
            .unwrap_or_else(|| panic!("the header row has a cell at column {c}"));
        assert_eq!(shown(&cell.value), *h);
    }
    for (r, (name, q1, _)) in ROWS.iter().enumerate() {
        let row = r as u32 + 1;
        assert_eq!(shown(&sheet.cells[&(row, 0)].value), *name);
        assert_eq!(shown(&sheet.cells[&(row, 1)].value), q1.to_string());
    }
    let charts = sheet
        .drawings
        .iter()
        .filter(|d| matches!(d.kind, DrawingKind::Chart(_)))
        .count();
    assert_eq!(
        charts, 1,
        "the 'one selection at a time' case selects chart 0"
    );
    let chart = sheet
        .drawings
        .iter()
        .find(|d| matches!(d.kind, DrawingKind::Chart(_)))
        .unwrap();
    assert!(
        chart.from.0 >= 6 && chart.from.1 >= 1,
        "the card must not sit over A1:D5, which the drag cases sweep: {:?}",
        chart.from
    );
}

#[test]
fn the_fixture_workbook_is_what_the_cases_are_written_against() {
    let path = fixture_path();
    if std::env::var_os("UIHARNESS_REGEN_FIXTURE").is_some() || !path.is_file() {
        let bytes = build();
        check(&bytes);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        eprintln!("wrote {}", path.display());
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    check(&bytes);
}

/// The generator and the committed file must not have drifted apart: a change
/// to `build()` that nobody regenerated would leave the recipe lying.
#[test]
fn the_committed_fixture_matches_the_generator() {
    let path = fixture_path();
    if !path.is_file() {
        return; // the test above writes it; ordering between the two is not fixed
    }
    let fresh = build();
    let on_disk = std::fs::read(&path).unwrap();
    // Compared through the loader rather than byte-wise: a zip's stored order
    // and timestamps are not the fixture's content, and a byte compare would
    // fail for reasons no reader could act on.
    let a = load_xlsx(&fresh).unwrap();
    let b = load_xlsx(&on_disk).unwrap();
    assert_eq!(
        a.workbook.sheets[0].cells.len(),
        b.workbook.sheets[0].cells.len(),
        "regenerate with UIHARNESS_REGEN_FIXTURE=1"
    );
    assert_eq!(
        a.workbook.sheets[0].drawings.len(),
        b.workbook.sheets[0].drawings.len(),
        "regenerate with UIHARNESS_REGEN_FIXTURE=1"
    );
}
