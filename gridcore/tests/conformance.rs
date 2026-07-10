//! The conformance gate: recalculate corpus workbooks and diff against the
//! cached values embedded in them (computed by an independent engine — see
//! corpus/xlsx/README.md). Any mismatch is a semantic regression.

use gridcore::engine::Engine;
use gridcore::formula::{is_volatile, parse};
use gridcore::sheet::{CellValue, cell_name};
use gridcore::xlsx::load_xlsx;

fn values_agree(a: &CellValue, b: &CellValue) -> bool {
    match (a, b) {
        (CellValue::Number(x), CellValue::Number(y)) => {
            let scale = x.abs().max(y.abs()).max(1.0);
            (x - y).abs() <= 1e-9 * scale
        }
        _ => a == b,
    }
}

#[test]
fn corpus_oracle_scoreboard_is_clean() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../corpus/xlsx");
    let mut checked_files = 0;
    for entry in std::fs::read_dir(dir).expect("corpus/xlsx exists") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("xlsx") {
            continue;
        }
        checked_files += 1;
        let data = std::fs::read(&path).expect("read corpus file");
        let pkg = load_xlsx(&data).expect("corpus file loads");
        let original = pkg.workbook.clone();
        let mut wb = pkg.workbook.clone();
        let mut engine = Engine::new(&wb);
        engine.recalc_all(&mut wb);

        let mut mismatches = Vec::new();
        let mut compared = 0;
        for (s, sheet) in original.sheets.iter().enumerate() {
            for (&(r, c), cell) in &sheet.cells {
                let Some(src) = &cell.formula else { continue };
                if engine.is_unsupported((s, r, c)) {
                    continue;
                }
                if parse(src).map(|a| is_volatile(&a)).unwrap_or(false) {
                    continue;
                }
                compared += 1;
                let got = wb.sheets[s]
                    .cell(r, c)
                    .map(|cl| cl.value.clone())
                    .unwrap_or(CellValue::Empty);
                if !values_agree(&cell.value, &got) {
                    mismatches.push(format!(
                        "{}: {}!{} ={src} oracle={:?} ours={got:?}",
                        path.display(),
                        sheet.name,
                        cell_name(r, c),
                        cell.value
                    ));
                }
            }
        }
        assert!(
            compared > 0,
            "{}: no formula cells compared",
            path.display()
        );
        assert!(
            mismatches.is_empty(),
            "conformance regressions:\n{}",
            mismatches.join("\n")
        );
    }
    assert!(checked_files > 0, "no corpus workbooks found in {dir}");
}
