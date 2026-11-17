//! The harness's own fixture workbook, and the generator that made it.
//!
//! `fixtures/basic.xlsx` and `fixtures/chart-kinds.xlsx` are committed, because
//! a UI test must not depend on a build step running first — but committed
//! binaries nobody can regenerate are the other failure, so both recipes live
//! here next to their checks:
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
use gridcore::xlsx::{SheetPackage, load_xlsx, new_xlsx, save_xlsx};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where the fixture lives, relative to this crate.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("basic.xlsx")
}

fn chart_kinds_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("chart-kinds.xlsx")
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
    populate_cells(&mut pkg);
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

fn populate_cells(pkg: &mut SheetPackage) {
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

/// A focused visual fixture for the two renderer paths that cannot be proved
/// by the selection fixture's bar chart. Both cards fit in the initial grid
/// viewport so PrintWindow crops contain the same pixels a user sees.
fn build_chart_kinds() -> Vec<u8> {
    let mut pkg = new_xlsx();
    populate_cells(&mut pkg);
    let source = |range| ChartSource {
        sheet: "Sheet1".to_string(),
        range,
        cat_col: 0,
    };
    let categories = ROWS
        .iter()
        .map(|(name, _, _)| name.to_string())
        .collect::<Vec<_>>();
    let q1 = ChartSeries {
        name: "Q1".to_string(),
        values: ROWS.iter().map(|(_, value, _)| *value).collect(),
        col: Some(1),
        values_ref: Some(source((1, 1, 4, 1))),
        name_ref: Some("Sheet1!$B$1".to_string()),
        ..ChartSeries::default()
    };
    let q2 = ChartSeries {
        name: "Q2".to_string(),
        values: ROWS.iter().map(|(_, _, value)| *value).collect(),
        col: Some(2),
        values_ref: Some(source((1, 2, 4, 2))),
        name_ref: Some("Sheet1!$C$1".to_string()),
        ..ChartSeries::default()
    };
    let common = || ChartData {
        categories: categories.clone(),
        source: Some(source((0, 0, 4, 2))),
        categories_ref: Some(source((1, 0, 4, 0))),
        edited: true,
        ..ChartData::default()
    };
    pkg.add_chart(
        0,
        (1, 4),
        (10, 10),
        &ChartData {
            title: "Quarter trend".to_string(),
            kind: "line".to_string(),
            series: vec![q1.clone(), q2],
            ..common()
        },
    );
    pkg.add_chart(
        0,
        (11, 4),
        (20, 10),
        &ChartData {
            title: "Q1 share".to_string(),
            kind: "pie".to_string(),
            series: vec![q1],
            ..common()
        },
    );
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

fn check_chart_kinds(bytes: &[u8]) {
    let pkg = load_xlsx(bytes).expect("the chart-kinds fixture is a readable xlsx");
    let charts = pkg.workbook.sheets[0]
        .drawings
        .iter()
        .filter_map(|drawing| match &drawing.kind {
            DrawingKind::Chart(chart) => Some(chart),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(charts.len(), 2);
    assert_eq!(charts[0].kind, "line");
    assert_eq!(charts[0].series.len(), 2);
    assert_eq!(charts[1].kind, "pie");
    assert_eq!(charts[1].series.len(), 1);
    assert_eq!(charts[1].categories, ["North", "South", "East", "West"]);
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

fn chart_kinds_fixture_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let path = chart_kinds_fixture_path();
        read_generated_fixture(
            &path,
            std::env::var_os("UIHARNESS_REGEN_FIXTURE").is_some(),
            build_chart_kinds,
            check_chart_kinds,
        )
        .unwrap_or_else(|e| panic!("{e}"))
    })
}

fn read_fixture(path: &Path, regenerate: bool) -> Result<Vec<u8>, String> {
    read_generated_fixture(path, regenerate, build, check)
}

fn read_generated_fixture(
    path: &Path,
    regenerate: bool,
    build_fixture: fn() -> Vec<u8>,
    check_fixture: fn(&[u8]),
) -> Result<Vec<u8>, String> {
    if regenerate {
        let bytes = build_fixture();
        check_fixture(&bytes);
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

#[test]
fn the_chart_kinds_fixture_is_what_the_visual_case_expects() {
    check_chart_kinds(chart_kinds_fixture_bytes());
}

/// The stable chart content this fixture generator deliberately authors.
///
/// `title`, `kind`, cached `categories`, and every `series` are serialized
/// directly into the chart part. `categories_ref` is serialized there too and
/// must be kept separate from the cached category text: the cache can stay
/// unchanged while the cells supplying it move. `source` is included as the
/// loader reconstructs it from those serialized references, even though the box
/// is not an independent chart element. `complex` is parsed from serialized
/// grouping and plot structure; it decides whether editing can regenerate the
/// chart part, so a change to it is observable fixture drift.
///
/// The remaining loaded fields are not independent stable content. `part` tells
/// save which package part to rewrite, but is a loader-assigned location;
/// `by_row` guides later range editing, but is inferred from the series and
/// category reference shapes already compared above. `ChartData::edited` is
/// intentionally absent too: it is a runtime instruction to regenerate the
/// chart part, not workbook content, and both sides reset it when `load_xlsx`
/// reads them. Comparing it would therefore prove nothing about generator
/// drift.
#[derive(Debug, PartialEq)]
struct StableFixtureChart<'a> {
    title: &'a str,
    kind: &'a str,
    categories: &'a [String],
    series: &'a [ChartSeries],
    source: Option<&'a ChartSource>,
    categories_ref: Option<&'a ChartSource>,
    complex: bool,
}

fn stable_fixture_chart(data: &ChartData) -> StableFixtureChart<'_> {
    StableFixtureChart {
        title: &data.title,
        kind: &data.kind,
        categories: &data.categories,
        series: &data.series,
        source: data.source.as_ref(),
        categories_ref: data.categories_ref.as_ref(),
        complex: data.complex,
    }
}

fn stable_fixture_cells(sheet: &gridcore::sheet::Sheet) -> Vec<((u32, u32), Cell)> {
    let mut cells = sheet
        .cells
        .iter()
        .map(|(position, cell)| (*position, cell.clone()))
        .collect::<Vec<_>>();
    cells.sort_by_key(|(position, _)| *position);
    cells
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
    assert_eq!(stable_fixture_cells(sa), stable_fixture_cells(sb), "{why}");
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
fn the_committed_chart_kinds_fixture_matches_the_generator() {
    let generated = load_xlsx(&build_chart_kinds()).unwrap();
    let committed = load_xlsx(chart_kinds_fixture_bytes()).unwrap();
    let generated = &generated.workbook.sheets[0];
    let committed = &committed.workbook.sheets[0];
    assert_eq!(
        stable_fixture_cells(generated),
        stable_fixture_cells(committed)
    );
    assert_eq!(generated.drawings.len(), committed.drawings.len());
    for (left, right) in generated.drawings.iter().zip(&committed.drawings) {
        assert_eq!((left.from, left.to), (right.from, right.to));
        let (DrawingKind::Chart(left), DrawingKind::Chart(right)) = (&left.kind, &right.kind)
        else {
            panic!("the chart-kinds fixture contains a non-chart drawing")
        };
        assert_eq!(stable_fixture_chart(left), stable_fixture_chart(right));
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

#[test]
fn fixture_drift_detects_a_complexity_only_change() {
    let simple = ChartData {
        title: "Quarterly sales".into(),
        kind: "column".into(),
        categories: vec!["Q1".into(), "Q2".into()],
        series: vec![ChartSeries {
            name: "North".into(),
            values: vec![10.0, 20.0],
            ..ChartSeries::default()
        }],
        ..ChartData::default()
    };
    let mut complex = simple.clone();
    complex.complex = true;

    assert_eq!(simple.title, complex.title);
    assert_eq!(simple.kind, complex.kind);
    assert_eq!(simple.categories, complex.categories);
    assert_eq!(simple.series, complex.series);
    assert_eq!(simple.source, complex.source);
    assert_eq!(simple.categories_ref, complex.categories_ref);
    assert_ne!(
        stable_fixture_chart(&simple),
        stable_fixture_chart(&complex),
        "a grouping-only change must fail fixture parity"
    );
}

// ---- #712: the typing-assistance cases' workbooks ---------------------------

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

/// A workbook whose first sheet holds `cells` (row, col, typed text).
fn typed_workbook(cells: &[(u32, u32, &str)]) -> SheetPackage {
    let mut pkg = new_xlsx();
    for &(r, c, t) in cells {
        let cell = gridcore::entry::entry_cell(&mut pkg.workbook, 0, r, c, t, None)
            .expect("a fixture entry");
        pkg.workbook.sheets[0].set_cell(r, c, cell);
    }
    pkg
}

fn text_at(pkg: &SheetPackage, r: u32, c: u32) -> String {
    pkg.workbook.sheets[0]
        .cells
        .get(&(r, c))
        .map(|cell| shown(&cell.value))
        .unwrap_or_default()
}

/// ENT-CASE-029/030's column: A1:A5 `Widgets`, `Gadgets`, `Gizmos`, `100`,
/// `Grommets`; A6 empty; A7 `Sprockets`.
const AUTOCOMPLETE: [(u32, u32, &str); 6] = [
    (0, 0, "Widgets"),
    (1, 0, "Gadgets"),
    (2, 0, "Gizmos"),
    (3, 0, "100"),
    (4, 0, "Grommets"),
    (6, 0, "Sprockets"),
];

fn build_autocomplete() -> Vec<u8> {
    save_xlsx(&typed_workbook(&AUTOCOMPLETE))
}

fn check_autocomplete(bytes: &[u8]) {
    let pkg = load_xlsx(bytes).expect("autocomplete.xlsx reads");
    for (r, c, t) in AUTOCOMPLETE {
        assert_eq!(text_at(&pkg, r, c), t);
    }
    assert_eq!(text_at(&pkg, 5, 0), "", "A6 is empty");
}

/// ENT-CASE-038/039's sheet: A1:A6 names, B5 `Babs`, C1:C3 `x-007`, `y-042`,
/// `z-100`.
const FLASH_FILL: [(u32, u32, &str); 10] = [
    (0, 0, "Ada Lovelace"),
    (1, 0, "Alan Turing"),
    (2, 0, "Grace Hopper"),
    (3, 0, "Edsger Dijkstra"),
    (4, 0, "Barbara Liskov"),
    (5, 0, "Donald Knuth"),
    (4, 1, "Babs"),
    (0, 2, "x-007"),
    (1, 2, "y-042"),
    (2, 2, "z-100"),
];

fn build_flash_fill() -> Vec<u8> {
    save_xlsx(&typed_workbook(&FLASH_FILL))
}

fn check_flash_fill(bytes: &[u8]) {
    let pkg = load_xlsx(bytes).expect("flash-fill.xlsx reads");
    for (r, c, t) in FLASH_FILL {
        assert_eq!(text_at(&pkg, r, c), t);
    }
}

/// TBL-CASE-021's table `Sales` (Item, Qty, Price, Region) over A1:D4.
const SALES: [(u32, u32, &str); 16] = [
    (0, 0, "Item"),
    (0, 1, "Qty"),
    (0, 2, "Price"),
    (0, 3, "Region"),
    (1, 0, "Pens"),
    (1, 1, "3"),
    (1, 2, "2"),
    (1, 3, "North"),
    (2, 0, "Ink"),
    (2, 1, "5"),
    (2, 2, "7"),
    (2, 3, "South"),
    (3, 0, "Pads"),
    (3, 1, "2"),
    (3, 2, "4"),
    (3, 3, "East"),
];

fn build_sales() -> Vec<u8> {
    let mut pkg = typed_workbook(&SALES);
    let i = pkg
        .add_table(0, (0, 0, 3, 3), true, "TableStyleMedium2")
        .expect("a table over A1:D4");
    let old = pkg.workbook.tables[i].name.clone();
    gridcore::edit::rename_table(&mut pkg.workbook, &old, "Sales").expect("renamed Sales");
    save_xlsx(&pkg)
}

fn check_sales(bytes: &[u8]) {
    let pkg = load_xlsx(bytes).expect("sales.xlsx reads");
    let t = pkg.workbook.table("Sales").expect("the table Sales");
    assert_eq!(t.columns, ["Item", "Qty", "Price", "Region"]);
    assert_eq!(t.range, (0, 0, 3, 3));
}

/// A generated fixture: its file name, builder and check.
type Generated = (&'static str, fn() -> Vec<u8>, fn(&[u8]));

#[test]
fn the_typing_assistance_fixtures_are_what_their_cases_assume() {
    let regen = std::env::var_os("UIHARNESS_REGEN_FIXTURE").is_some();
    let fixtures: [Generated; 3] = [
        ("autocomplete.xlsx", build_autocomplete, check_autocomplete),
        ("flash-fill.xlsx", build_flash_fill, check_flash_fill),
        ("sales.xlsx", build_sales, check_sales),
    ];
    for (name, build_fixture, check_fixture) in fixtures {
        let bytes = read_generated_fixture(&fixture(name), regen, build_fixture, check_fixture)
            .unwrap_or_else(|e| panic!("{e}"));
        check_fixture(&bytes);
    }
}

// ---- #2228: floating pictures on a sheet -----------------------------------

/// A picture workbook like the ones the issue came from (screenshots with
/// notes), made of literals: one sheet, three floating pictures, custom row
/// heights and a hidden column under the first one.
///
/// - Picture 0: a PNG, `twoCellAnchor editAs="oneCell"` over A2:G54, with
///   offsets into both corners.
/// - Picture 1: a JPEG, `twoCellAnchor` over J14:O28.
/// - Picture 2: a PNG, `oneCellAnchor` from J31 with an extent, written the
///   way Excel writes one (`noChangeAspect`, an `a:extLst/a:ext uri` in the
///   blip, `a:stretch/a:fillRect`).
fn pictures_fixture_path() -> PathBuf {
    fixture("pictures.xlsx")
}

/// The JPEG of picture 1: 16×16, orange, its top-left quarter blue, so the
/// way up shows in a screenshot. A baseline JPEG made once with Pillow
/// (`quality=90, subsampling=0`); the harness has no JPEG encoder, and this
/// is its whole content.
const PICTURE_JPEG: [u8; 642] = [
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x03, 0x02, 0x02, 0x03,
    0x03, 0x03, 0x03, 0x04, 0x03, 0x03, 0x04, 0x05, 0x08, 0x05, 0x05, 0x04, 0x04, 0x05, 0x0a, 0x07,
    0x07, 0x06, 0x08, 0x0c, 0x0a, 0x0c, 0x0c, 0x0b, 0x0a, 0x0b, 0x0b, 0x0d, 0x0e, 0x12, 0x10, 0x0d,
    0x0e, 0x11, 0x0e, 0x0b, 0x0b, 0x10, 0x16, 0x10, 0x11, 0x13, 0x14, 0x15, 0x15, 0x15, 0x0c, 0x0f,
    0x17, 0x18, 0x16, 0x14, 0x18, 0x12, 0x14, 0x15, 0x14, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x03, 0x04,
    0x04, 0x05, 0x04, 0x05, 0x09, 0x05, 0x05, 0x09, 0x14, 0x0d, 0x0b, 0x0d, 0x14, 0x14, 0x14, 0x14,
    0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14,
    0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14,
    0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0xff, 0xc0,
    0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x10, 0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xff, 0xc4, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
    0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05,
    0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
    0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23,
    0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17,
    0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a,
    0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a,
    0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
    0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
    0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7,
    0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5,
    0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1,
    0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xc4, 0x00, 0x1f, 0x01, 0x00, 0x03,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x11, 0x00,
    0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77, 0x00,
    0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13,
    0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15,
    0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27,
    0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6,
    0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4,
    0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9,
    0xfa, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0xf9,
    0x1e, 0xbf, 0x78, 0x3e, 0x1c, 0xfb, 0x62, 0xbf, 0xce, 0x23, 0xfb, 0x2c, 0x28, 0x00, 0xa0, 0x0f,
    0xff, 0xd9,
];

/// A `w`×`h` PNG of `body`, its top-left quarter `corner`.
fn picture_png(w: u32, h: u32, body: [u8; 3], corner: [u8; 3]) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if x < w / 2 && y < h / 2 { corner } else { body };
            px.extend_from_slice(&[c[0], c[1], c[2], 0xff]);
        }
    }
    uiharness::png::encode(&uiharness::image::Image { w, h, px })
}

/// The rows given 30 pt under picture 0, and the column hidden under it (C).
const PICTURE_TALL_ROWS: [u32; 3] = [5, 6, 7];
const PICTURE_HIDDEN_COL: u32 = 2;

const PICTURE_DRAWING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:twoCellAnchor editAs="oneCell"><xdr:from><xdr:col>0</xdr:col><xdr:colOff>152400</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>47625</xdr:rowOff></xdr:from><xdr:to><xdr:col>6</xdr:col><xdr:colOff>304800</xdr:colOff><xdr:row>53</xdr:row><xdr:rowOff>95250</xdr:rowOff></xdr:to><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Picture 1"/><xdr:cNvPicPr><a:picLocks noChangeAspect="1"/></xdr:cNvPicPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x="152400" y="238125"/><a:ext cx="4419600" cy="10096500"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/></xdr:twoCellAnchor><xdr:twoCellAnchor><xdr:from><xdr:col>9</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>13</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>14</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>27</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="3" name="Picture 2"/><xdr:cNvPicPr><a:picLocks noChangeAspect="1"/></xdr:cNvPicPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId2"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x="5486400" y="2476500"/><a:ext cx="3048000" cy="2667000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/></xdr:twoCellAnchor><xdr:oneCellAnchor><xdr:from><xdr:col>9</xdr:col><xdr:colOff>95250</xdr:colOff><xdr:row>30</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="2438400" cy="1143000"/><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="4" name="Picture 3"/><xdr:cNvPicPr><a:picLocks noChangeAspect="1"/></xdr:cNvPicPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId3"><a:extLst><a:ext uri="{28A0092B-C50C-407E-A947-70E740481C1C}"/></a:extLst></a:blip><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x="5581650" y="5715000"/><a:ext cx="2438400" cy="1143000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"#;

const PICTURE_DRAWING_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image2.jpeg"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image3.png"/></Relationships>"#;

const PICTURE_SHEET_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#;

/// The media parts of the pictures fixture, by part name.
fn picture_media() -> [(&'static str, Vec<u8>); 3] {
    [
        (
            "xl/media/image1.png",
            picture_png(48, 96, [0x9f, 0xc5, 0xe8], [0xc0, 0x30, 0x30]),
        ),
        ("xl/media/image2.jpeg", PICTURE_JPEG.to_vec()),
        (
            "xl/media/image3.png",
            picture_png(64, 32, [0x8f, 0xd1, 0x8a], [0x40, 0x40, 0x40]),
        ),
    ]
}

fn build_pictures() -> Vec<u8> {
    let mut pkg = typed_workbook(&[
        (0, 0, "Pictures fixture (#2228)"),
        (0, 9, "Beside it: a JPEG"),
        (29, 9, "Below it: a one-cell picture"),
        // Under picture 1, for the case's Delete with the picture selected.
        (16, 10, "Under the JPEG"),
    ]);
    let sheet = &mut pkg.workbook.sheets[0];
    for r in PICTURE_TALL_ROWS {
        sheet.set_row_height(r, Some(30.0));
    }
    sheet.set_col_hidden(PICTURE_HIDDEN_COL, true);
    // The drawing goes in as parts: the model reads pictures but has no way
    // to make one, so the worksheet names the drawing part, which names the
    // media, as Excel writes them.
    let mut pkg = load_xlsx(&save_xlsx(&pkg)).expect("the base workbook");
    let ws_name = "xl/worksheets/sheet1.xml";
    let ws = String::from_utf8(pkg.part(ws_name).expect("the worksheet").to_vec()).unwrap();
    assert!(
        !ws.contains("<drawing"),
        "the base sheet has no drawing yet"
    );
    // `<drawing>` is the last child this sheet has room for (after
    // `sheetData`; it has no page setup or table parts), and its `r:` prefix
    // needs declaring, which a sheet with no relationships did not.
    let ws = ws.replacen("</worksheet>", "<drawing r:id=\"rId1\"/></worksheet>", 1);
    let rel_ns = "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    let ws = if ws.contains("xmlns:r=") {
        ws
    } else {
        ws.replacen("<worksheet ", &format!("<worksheet {rel_ns} "), 1)
    };
    pkg.set_part(ws_name, ws.into_bytes());
    pkg.set_part(
        "xl/worksheets/_rels/sheet1.xml.rels",
        PICTURE_SHEET_RELS.into(),
    );
    pkg.set_part("xl/drawings/drawing1.xml", PICTURE_DRAWING.into());
    pkg.set_part(
        "xl/drawings/_rels/drawing1.xml.rels",
        PICTURE_DRAWING_RELS.into(),
    );
    for (name, bytes) in picture_media() {
        pkg.set_part(name, bytes);
    }
    let types = String::from_utf8(pkg.part("[Content_Types].xml").unwrap().to_vec()).unwrap();
    let types = types.replacen(
        "</Types>",
        "<Default Extension=\"png\" ContentType=\"image/png\"/>\
         <Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
         <Override PartName=\"/xl/drawings/drawing1.xml\" \
         ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>",
        1,
    );
    pkg.set_part("[Content_Types].xml", types.into_bytes());
    save_xlsx(&pkg)
}

fn check_pictures(bytes: &[u8]) {
    use gridcore::sheet::AnchorKind;
    let pkg = load_xlsx(bytes).expect("the pictures fixture is a readable xlsx");
    let sheet = &pkg.workbook.sheets[0];
    let pictures: Vec<_> = sheet
        .drawings
        .iter()
        .filter_map(|d| match &d.kind {
            DrawingKind::Image { part, name } => Some((d, part.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(pictures.len(), 3, "three pictures");
    let (p0, part0, name0) = pictures[0];
    assert_eq!((part0, name0), ("xl/media/image1.png", "Picture 1"));
    assert_eq!((p0.from, p0.to), ((1, 0), (53, 6)), "A2:G54");
    assert_eq!(p0.anchor.from_off, (47625, 152400));
    let (p1, part1, _) = pictures[1];
    assert_eq!(part1, "xl/media/image2.jpeg");
    assert_eq!((p1.from, p1.to), ((13, 9), (27, 14)), "J14:O28");
    let (p2, part2, _) = pictures[2];
    assert_eq!(part2, "xl/media/image3.png");
    assert_eq!(p2.anchor.kind, AnchorKind::OneCell);
    assert_eq!(p2.anchor.ext, Some((2438400, 1143000)));
    for r in PICTURE_TALL_ROWS {
        assert_eq!(sheet.row_height(r), Some(30.0), "row {}", r + 1);
    }
    assert!(sheet.col_hidden(PICTURE_HIDDEN_COL), "column C is hidden");
    for (name, media) in picture_media() {
        assert_eq!(pkg.part(name), Some(media.as_slice()), "{name}");
    }
}

fn pictures_fixture_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        read_generated_fixture(
            &pictures_fixture_path(),
            std::env::var_os("UIHARNESS_REGEN_FIXTURE").is_some(),
            build_pictures,
            check_pictures,
        )
        .unwrap_or_else(|e| panic!("{e}"))
    })
}

#[test]
fn the_pictures_fixture_is_what_its_case_expects() {
    check_pictures(pictures_fixture_bytes());
}

#[test]
fn the_committed_pictures_fixture_matches_the_generator() {
    // Through the loader, as for the other fixtures: the parts that hold the
    // pictures, and what the model reads of them.
    let generated = load_xlsx(&build_pictures()).unwrap();
    let committed = load_xlsx(pictures_fixture_bytes()).unwrap();
    let why = "regenerate with UIHARNESS_REGEN_FIXTURE=1 cargo test -p uiharness --test fixture";
    for part in [
        "xl/drawings/drawing1.xml",
        "xl/drawings/_rels/drawing1.xml.rels",
        "xl/worksheets/_rels/sheet1.xml.rels",
        "xl/media/image1.png",
        "xl/media/image2.jpeg",
        "xl/media/image3.png",
    ] {
        assert_eq!(generated.part(part), committed.part(part), "{part} — {why}");
    }
    let (g, c) = (&generated.workbook.sheets[0], &committed.workbook.sheets[0]);
    assert_eq!(stable_fixture_cells(g), stable_fixture_cells(c), "{why}");
    assert_eq!(g.drawings, c.drawings, "{why}");
}

/// Opening the pictures workbook and saving it, nothing edited, keeps every
/// drawing part, its relationships and the media byte for byte (#2228).
#[test]
fn an_unedited_save_keeps_the_pictures_byte_for_byte() {
    let loaded = load_xlsx(pictures_fixture_bytes()).unwrap();
    let saved = load_xlsx(&save_xlsx(&loaded)).unwrap();
    let parts: Vec<&str> = loaded
        .part_names()
        .into_iter()
        .filter(|n| n.starts_with("xl/drawings/") || n.starts_with("xl/media/"))
        .collect();
    assert_eq!(parts.len(), 5, "{parts:?}");
    for part in parts {
        assert_eq!(
            saved.part(part),
            loaded.part(part),
            "{part} changed on save"
        );
    }
    assert_eq!(
        saved.workbook.sheets[0].drawings,
        loaded.workbook.sheets[0].drawings
    );
}
