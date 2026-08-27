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

use gridcore::sheet::{Cell, CellValue, ChartData, ChartSeries, ChartSource, DrawingKind};
use gridcore::xlsx::{load_xlsx, new_xlsx, save_xlsx};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
    let source = |range| ChartSource {
        sheet: "Sheet1".to_string(),
        range,
        cat_col: 0,
    };
    let data = ChartData {
        title: "Quarters".to_string(),
        kind: "bar".to_string(),
        categories: ROWS.iter().map(|(n, _, _)| n.to_string()).collect(),
        series: vec![ChartSeries {
            name: "Q1".to_string(),
            values: ROWS.iter().map(|(_, q, _)| *q).collect(),
            col: Some(1),
            values_ref: Some(source((1, 1, 4, 1))),
            name_ref: Some("Sheet1!$B$1".to_string()),
            ..ChartSeries::default()
        }],
        source: Some(source((0, 0, 4, 1))),
        categories_ref: Some(source((1, 0, 4, 0))),
        edited: true,
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
    let DrawingKind::Chart(data) = &chart.kind else {
        unreachable!("the drawing was filtered to a chart")
    };
    assert_eq!(data.source.as_ref().map(|s| s.range), Some((0, 0, 4, 1)));
    assert_eq!(
        data.categories_ref.as_ref().map(|s| s.range),
        Some((1, 0, 4, 0))
    );
    assert_eq!(data.categories, ["North", "South", "East", "West"]);
    assert_eq!(data.series.len(), 1);
    assert_eq!(data.series[0].name, "Q1");
    assert_eq!(data.series[0].name_ref.as_deref(), Some("Sheet1!$B$1"));
    assert_eq!(
        data.series[0].values_ref.as_ref().map(|s| s.range),
        Some((1, 1, 4, 1))
    );
    assert!(
        chart.from.0 >= 6 && chart.from.1 >= 1,
        "the card must not sit over A1:D5, which the drag cases sweep: {:?}",
        chart.from
    );
}

/// The committed fixture's bytes. It is written only when the caller explicitly
/// asks for regeneration; a missing committed asset is a test failure.
///
/// ⚠️ Shared through a `OnceLock` because both tests below need it and they run
/// on different threads of one binary: two `fs::write`s racing a `fs::read`
/// would let a test see a half-written zip and blame the generator for it.
fn fixture_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let path = fixture_path();
        read_fixture(&path, std::env::var_os("UIHARNESS_REGEN_FIXTURE").is_some())
            .unwrap_or_else(|e| panic!("{e}"))
    })
}

fn read_fixture(path: &Path, regenerate: bool) -> Result<Vec<u8>, String> {
    if regenerate {
        let bytes = build();
        check(&bytes);
        std::fs::create_dir_all(path.parent().expect("the fixture has a parent"))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::write(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    std::fs::read(path).map_err(|e| {
        format!(
            "{}: {e}; restore the committed fixture or regenerate it with \
             UIHARNESS_REGEN_FIXTURE=1 cargo test -p uiharness --test fixture",
            path.display()
        )
    })
}

#[test]
fn a_missing_fixture_fails_without_recreating_the_source_tree() {
    let dir =
        std::env::temp_dir().join(format!("uiharness-missing-fixture-{}", std::process::id()));
    let path = dir.join("basic.xlsx");
    let _ = std::fs::remove_file(&path);
    let err = read_fixture(&path, false).expect_err("a missing committed fixture must fail");
    assert!(err.contains("restore the committed fixture"), "{err}");
    assert!(!path.exists(), "the fixture check must not write {path:?}");
}

#[test]
fn the_fixture_workbook_is_what_the_cases_are_written_against() {
    check(fixture_bytes());
}

/// The stable chart content this fixture generator deliberately authors.
///
/// `title`, `kind`, `categories`, and `series` are serialized directly into
/// the chart part. `categories_ref` is serialized there too and must be kept
/// separate from the cached category text: the cache can stay unchanged while
/// the cells supplying it move. `source` is included as the loader reconstructs
/// it from those serialized references, even though the box is not an
/// independent chart element.
///
/// `ChartData::edited` is intentionally absent. It is a runtime instruction to
/// regenerate the chart part, not workbook content, and both sides reset it
/// when `load_xlsx` reads them. Comparing it would therefore prove nothing
/// about generator drift.
#[derive(Debug, PartialEq)]
struct StableFixtureChart<'a> {
    title: &'a str,
    kind: &'a str,
    categories: &'a [String],
    series: &'a [ChartSeries],
    source: Option<&'a ChartSource>,
    categories_ref: Option<&'a ChartSource>,
}

fn stable_fixture_chart(data: &ChartData) -> StableFixtureChart<'_> {
    StableFixtureChart {
        title: &data.title,
        kind: &data.kind,
        categories: &data.categories,
        series: &data.series,
        source: data.source.as_ref(),
        categories_ref: data.categories_ref.as_ref(),
    }
}

/// The generator and the committed file must not have drifted apart: a change
/// to `build()` that nobody regenerated would leave the recipe lying.
///
/// ⚠️ Compared by content, not by counting. Swapping two cell values, renaming
/// the chart or moving its anchor leaves every count identical, and those are
/// precisely the edits a case would then be asserting against a workbook nobody
/// can reproduce.
#[test]
fn the_committed_fixture_matches_the_generator() {
    // Compared through the loader rather than byte-wise: a zip's stored order
    // and timestamps are not the fixture's content, and a byte compare would
    // fail for reasons no reader could act on. Both sides are loaded, so the
    // fields the loader fills in (chart part paths and the like) match too.
    let a = load_xlsx(&build()).unwrap();
    let b = load_xlsx(fixture_bytes()).unwrap();
    let why = "regenerate with UIHARNESS_REGEN_FIXTURE=1 cargo test -p uiharness --test fixture";
    let (sa, sb) = (&a.workbook.sheets[0], &b.workbook.sheets[0]);
    assert_eq!(sa.name, sb.name, "{why}");
    let cells = |s: &gridcore::sheet::Sheet| {
        let mut v: Vec<((u32, u32), Cell)> = s.cells.iter().map(|(k, c)| (*k, c.clone())).collect();
        v.sort_by_key(|(k, _)| *k);
        v
    };
    assert_eq!(cells(sa), cells(sb), "{why}");
    assert_eq!(sa.drawings.len(), sb.drawings.len(), "{why}");
    for (x, y) in sa.drawings.iter().zip(&sb.drawings) {
        assert_eq!((x.from, x.to), (y.from, y.to), "the chart moved — {why}");
        match (&x.kind, &y.kind) {
            (DrawingKind::Chart(u), DrawingKind::Chart(v)) => {
                assert_eq!(stable_fixture_chart(u), stable_fixture_chart(v), "{why}");
            }
            (u, v) => panic!("the fixture's drawing changed kind: {u:?} vs {v:?} — {why}"),
        }
    }
}

#[test]
fn fixture_drift_detects_a_category_reference_change_with_the_same_cache() {
    let expected = ChartData {
        categories: vec!["North".into(), "South".into()],
        series: vec![ChartSeries {
            name: "Q1".into(),
            values: vec![10.0, 20.0],
            ..ChartSeries::default()
        }],
        categories_ref: Some(ChartSource {
            sheet: "Sheet1".into(),
            range: (1, 0, 2, 0),
            cat_col: 0,
        }),
        ..ChartData::default()
    };
    let mut drifted = expected.clone();
    drifted.categories_ref.as_mut().unwrap().range = (1, 2, 2, 2);

    assert_eq!(expected.categories, drifted.categories);
    assert_eq!(expected.series, drifted.series);
    assert_ne!(
        stable_fixture_chart(&expected),
        stable_fixture_chart(&drifted),
        "a category-reference-only change must fail fixture parity"
    );
}
