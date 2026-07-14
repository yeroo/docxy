//! Round-trip and robustness sweeps over the xlsx corpus: every corpus
//! workbook must load, save losslessly, reload identically, render every
//! cell, and recalculate idempotently. These are the invariants that keep
//! real users' files safe.

use gridcore::engine::Engine;
use gridcore::sheet::format_value;
use gridcore::sheet::sheet_to_csv;
use gridcore::xlsx::{SheetPackage, load_xlsx, save_xlsx};

fn corpus_files() -> Vec<std::path::PathBuf> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../corpus/xlsx");
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .expect("corpus/xlsx exists")
        .filter_map(|e| {
            let p = e.expect("dir entry").path();
            (p.extension().and_then(|x| x.to_str()) == Some("xlsx")).then_some(p)
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no corpus workbooks found");
    out
}

fn assert_same_model(a: &SheetPackage, b: &SheetPackage, ctx: &str) {
    let (wa, wb) = (&a.workbook, &b.workbook);
    assert_eq!(wa.sheets.len(), wb.sheets.len(), "{ctx}: sheet count");
    assert_eq!(wa.date1904, wb.date1904, "{ctx}: date system");
    assert_eq!(wa.defined_names, wb.defined_names, "{ctx}: defined names");
    for (sa, sb) in wa.sheets.iter().zip(&wb.sheets) {
        assert_eq!(sa.name, sb.name, "{ctx}: sheet name");
        assert_eq!(sa.merges, sb.merges, "{ctx}: merges in {}", sa.name);
        assert_eq!(
            sa.col_defs, sb.col_defs,
            "{ctx}: column defs in {}",
            sa.name
        );
        assert_eq!(
            sa.row_attrs, sb.row_attrs,
            "{ctx}: row attrs in {}",
            sa.name
        );
        assert_eq!(
            sa.cells.len(),
            sb.cells.len(),
            "{ctx}: cell count in {}",
            sa.name
        );
        for (ka, ca) in &sa.cells {
            let cb = sb
                .cells
                .get(ka)
                .unwrap_or_else(|| panic!("{ctx}: {}!{:?} missing after round-trip", sa.name, ka));
            assert_eq!(ca, cb, "{ctx}: cell {}!{:?}", sa.name, ka);
        }
    }
}

/// load → save → reload is semantically identical, and a second save is
/// byte-identical (the writer is deterministic).
#[test]
fn corpus_round_trips_losslessly() {
    for path in corpus_files() {
        let ctx = path.display().to_string();
        let data = std::fs::read(&path).expect("read corpus file");
        let pkg = load_xlsx(&data).unwrap_or_else(|e| panic!("{ctx}: load: {e}"));
        let saved = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&saved).unwrap_or_else(|e| panic!("{ctx}: reload: {e}"));
        assert_same_model(&pkg, &pkg2, &ctx);
        let saved2 = save_xlsx(&pkg2);
        assert_eq!(saved, saved2, "{ctx}: second save not byte-identical");
    }
}

/// Parts we don't model must survive byte-for-byte. (Worksheets, shared
/// strings, workbook.xml, content types and workbook rels are legitimately
/// regenerated; everything else must be untouched.)
#[test]
fn corpus_preserves_unmodeled_parts() {
    let regenerated = |name: &str| {
        name.starts_with("xl/worksheets/")
            || name == "xl/sharedStrings.xml"
            || name == "xl/workbook.xml"
            || name == "xl/calcChain.xml" // deliberately dropped
            || name == "xl/_rels/workbook.xml.rels"
            || name == "[Content_Types].xml"
    };
    for path in corpus_files() {
        let ctx = path.display().to_string();
        let data = std::fs::read(&path).expect("read");
        let pkg = load_xlsx(&data).expect("load");
        let saved = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&saved).expect("reload");
        for name in pkg.part_names() {
            if regenerated(name) {
                continue;
            }
            let before = pkg.part(name).unwrap();
            let after = pkg2
                .part(name)
                .unwrap_or_else(|| panic!("{ctx}: part {name} lost on save"));
            assert_eq!(before, after, "{ctx}: part {name} modified on save");
        }
    }
}

/// Every cell of every corpus file renders (number formats, dates, errors)
/// and exports to CSV without panicking — the display path the TUI uses.
#[test]
fn corpus_renders_every_cell() {
    for path in corpus_files() {
        let data = std::fs::read(&path).expect("read");
        let pkg = load_xlsx(&data).expect("load");
        let wb = &pkg.workbook;
        for sheet in &wb.sheets {
            for cell in sheet.cells.values() {
                let xf = wb.styles.xf(cell.style);
                let _ = format_value(&cell.value, xf.numfmt, wb.date1904);
            }
            let _ = sheet_to_csv(sheet, &wb.styles, wb.date1904);
        }
    }
}

/// Recalculation is idempotent: a second full recalc changes nothing.
#[test]
fn corpus_recalc_is_idempotent() {
    for path in corpus_files() {
        let ctx = path.display().to_string();
        let data = std::fs::read(&path).expect("read");
        let pkg = load_xlsx(&data).expect("load");
        let mut wb = pkg.workbook.clone();
        let mut engine = Engine::new(&wb);
        engine.recalc_all(&mut wb);
        let mut wb2 = wb.clone();
        engine.recalc_all(&mut wb2);
        for (i, (a, b)) in wb.sheets.iter().zip(&wb2.sheets).enumerate() {
            assert_eq!(a.cells, b.cells, "{ctx}: sheet {i} changed on 2nd recalc");
        }
    }
}
