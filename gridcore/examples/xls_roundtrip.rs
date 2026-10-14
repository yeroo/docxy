//! Open a workbook (any format gridcore reads) and write it as an `.xls`
//! with the BIFF8 writer, checking the file's structure first. Used by
//! `scripts/check-xls-writer.ps1` (#874), which opens the result in Excel.
//!
//!     cargo run -p gridcore --example xls_roundtrip -- in.xls out.xls
//!     cargo run -p gridcore --example xls_roundtrip -- --synthetic out.xlsx
//!
//! `--synthetic` writes a small `.xlsx` of what the corpus lacks, for the
//! script to write as `.xls` and compare in Excel: dynamic arrays (a
//! SEQUENCE spill, SORT, FILTER), a text-valued legacy array, a custom
//! colour and a bordered cell.

use gridcore::sheet::{Cell, Xf};

fn synthetic() -> gridcore::xlsx::SheetPackage {
    let mut pkg = gridcore::xlsx::new_xlsx();
    let wb = &mut pkg.workbook;
    let mut fill = Xf::default();
    fill.fill = Some((0x12, 0x34, 0x56));
    let fill = wb.styles.intern(fill);
    let mut boxed = Xf::default();
    boxed.border = true;
    let boxed = wb.styles.intern(boxed);
    let s = &mut wb.sheets[0];
    for (r, v) in [5.0, 3.0, 9.0, 1.0, 7.0].into_iter().enumerate() {
        s.set_cell(r as u32, 0, Cell::number(v));
    }
    let f = |src: &str| Cell {
        formula: Some(src.to_string()),
        ..Cell::default()
    };
    s.set_cell(0, 5, Cell::text("x"));
    s.set_cell(1, 5, Cell::text("y"));
    let mut cse = f("F1:F2&\"!\"");
    cse.f_attrs = Some(" t=\"array\" ref=\"G1:G2\"".into());
    s.set_cell(0, 6, cse);
    s.set_cell(
        0,
        7,
        Cell {
            style: fill,
            ..Cell::number(1.0)
        },
    );
    s.set_cell(
        1,
        7,
        Cell {
            style: boxed,
            ..Cell::number(2.0)
        },
    );
    let mut engine = gridcore::engine::Engine::new(wb);
    engine.recalc_all(wb);
    // Typed, as a person enters them, so they spill.
    for (col, src) in [
        (2, "_xlfn.SEQUENCE(3)"),
        (3, "_xlfn._xlws.SORT(A1:A5)"),
        (4, "_xlfn._xlws.FILTER(A1:A5,A1:A5>4)"),
    ] {
        engine.set_cell(wb, (0, 0, col), f(src));
    }
    pkg
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, out] = args.as_slice() {
        if flag == "--synthetic" {
            let bytes = gridcore::xlsx::save_xlsx(&synthetic());
            return match std::fs::write(out, bytes) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{out}: {e}");
                    std::process::ExitCode::FAILURE
                }
            };
        }
    }
    let [input, output] = args.as_slice() else {
        eprintln!("usage: xls_roundtrip <in> <out.xls>");
        return std::process::ExitCode::from(2);
    };
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let opened = match gridcore::legacy::open_workbook_ex(&data) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{input}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let out = gridcore::legacy::write_xls(&opened.pkg);
    if let Err(e) = gridcore::legacy::check_xls(&out.bytes) {
        eprintln!("{input}: structure: {e}");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(output, &out.bytes) {
        eprintln!("{output}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    for d in &opened.dropped {
        println!("dropped: {d}");
    }
    for l in &out.lost {
        println!("lost: {l}");
    }
    std::process::ExitCode::SUCCESS
}
