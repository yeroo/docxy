//! The legacy-format oracle (#603): `corpus/legacy/<stem>.{xls,xlsb,ods}`
//! are the `corpus/xlsx/<stem>.xlsx` workbooks saved by Excel itself in the
//! other formats (scripts/make-legacy-fixtures.ps1). Importing one must give
//! the workbook the `.xlsx` holds: the same sheets, values, formulas, number
//! formats, date system and names, and the same results on recalculation.
//! Every exception is an entry in [`ALLOW`], with its reason.

use gridcore::engine::Engine;
use gridcore::formula::{display_formula, is_volatile, parse};
use gridcore::legacy::{SourceFormat, open_workbook};
use gridcore::sheet::{CellValue, Workbook, cell_name};
use gridcore::xlsx::{load_xlsx, save_xlsx};

/// What an allowlist entry exempts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Check {
    /// (b): the cached value the file holds.
    Value,
    /// (d): the recalculated value.
    Recalc,
    /// (e): the formula's text.
    Formula,
    /// (f): the number-format code.
    Format,
}

/// One exemption: in `file`, cell `cell` of sheet `sheet` ("*" for every
/// cell of it) skips `checks`, because `why`.
struct Allow {
    file: &'static str,
    sheet: &'static str,
    cell: &'static str,
    checks: &'static [Check],
    why: &'static str,
}

const ALLOW: &[Allow] = &[
    // The source formulas are `SUM(Q1:Q3!A1:A1)` and so on, unquoted. When
    // Excel opened that .xlsx to make the fixtures it read `Q1` as a cell,
    // so its cached results are #VALUE! (and 0 for COUNT), and what it wrote
    // to the .xls is `SUM(Q1:'Q3'!A1:A1)`. The import keeps Excel's cached
    // values; gridcore reads the formula as the 3D reference, so (d) holds.
    Allow {
        file: "calc-3d.xlsb",
        sheet: "Total",
        cell: "*",
        checks: &[Check::Value],
        why: "Excel cached its misreading of Q1:Q3! (Q1 as a cell)",
    },
    Allow {
        file: "calc-3d.xls",
        sheet: "Total",
        cell: "*",
        checks: &[Check::Value],
        why: "Excel cached its misreading of Q1:Q3! (Q1 as a cell)",
    },
    // BIFF8 has no tables: Excel writes structured references as the
    // ranges they cover (`Sales[Qty]` → `Orders!$B$2:$B$5`). The values
    // and recalculation agree; tables themselves are not imported.
    Allow {
        file: "shape-salestable.xls",
        sheet: "Orders",
        cell: "*",
        checks: &[Check::Formula],
        why: ".xls has no tables; structured refs are stored as ranges",
    },
    // The .xlsb keeps the table, but the import doesn't (tables are a
    // follow-up), so its structured references are written as the ranges
    // they cover, as Excel itself writes them to an .xls.
    Allow {
        file: "shape-salestable.xlsb",
        sheet: "Orders",
        cell: "*",
        checks: &[Check::Formula],
        why: "tables are not imported; structured refs become ranges",
    },
];

fn allowed(file: &str, sheet: &str, cell: &str, check: Check) -> bool {
    ALLOW.iter().any(|a| {
        a.file == file
            && a.sheet == sheet
            && (a.cell == "*" || a.cell == cell)
            && a.checks.contains(&check)
    })
}

fn values_agree(a: &CellValue, b: &CellValue) -> bool {
    match (a, b) {
        (CellValue::Number(x), CellValue::Number(y)) => {
            let scale = x.abs().max(y.abs()).max(1.0);
            (x - y).abs() <= 1e-9 * scale
        }
        _ => a == b,
    }
}

/// A formula as compared: the text Excel shows (no `_xlfn.`, `@x` for
/// `_xlfn.SINGLE(x)`) without the implicit-intersection `@`s Excel adds
/// when it writes IFS/SWITCH to an older format, parsed. Whitespace, case
/// and redundant spelling differences vanish in the AST.
fn formula_ast(src: &str) -> Result<gridcore::formula::Expr, String> {
    let shown = display_formula(src);
    let mut bare = String::with_capacity(shown.len());
    let mut in_str = false;
    for ch in shown.chars() {
        if ch == '"' {
            in_str = !in_str;
        }
        if ch == '@' && !in_str {
            continue;
        }
        bare.push(ch);
    }
    parse(&bare)
}

/// The effective number-format code of a cell's style, with its bracketed
/// sections (`[Red]`, `[$$-409]`, `[h]`) in upper case: Excel reads those
/// in any case and rewrites `[RED]` as `[Red]` when it saves.
fn code(wb: &Workbook, style: u32) -> String {
    let raw = wb
        .styles
        .xf(style)
        .code
        .unwrap_or_else(|| "General".to_string());
    let mut out = String::with_capacity(raw.len());
    let (mut in_quote, mut in_bracket) = (false, false);
    for ch in raw.chars() {
        match ch {
            '"' if !in_bracket => in_quote = !in_quote,
            '[' if !in_quote => in_bracket = true,
            ']' if !in_quote => in_bracket = false,
            _ => {}
        }
        if in_bracket {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn corpus(dir: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("corpus")
        .join(dir)
}

/// The checks that hold for an imported workbook and for it after a save
/// and reopen: (a) sheets, (b) values both ways, (c) formula presence,
/// (f) number formats, (g) date system and names.
fn compare_static(file: &str, src: &Workbook, got: &Workbook, errs: &mut Vec<String>) {
    let names = |wb: &Workbook| wb.sheets.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    if names(src) != names(got) {
        errs.push(format!(
            "{file}: (a) sheets {:?} != {:?}",
            names(got),
            names(src)
        ));
        return;
    }
    for (s, sheet) in src.sheets.iter().enumerate() {
        let theirs = &got.sheets[s];
        for (&(r, c), cell) in &sheet.cells {
            let at = format!("{file}: {}!{}", sheet.name, cell_name(r, c));
            if cell.value.is_empty() {
                continue;
            }
            let Some(mine) = theirs.cell(r, c) else {
                errs.push(format!("{at}: (b) missing, source {:?}", cell.value));
                continue;
            };
            if !values_agree(&cell.value, &mine.value)
                && !allowed(file, &sheet.name, &cell_name(r, c), Check::Value)
            {
                errs.push(format!(
                    "{at}: (b) {:?} != source {:?}",
                    mine.value, cell.value
                ));
            }
            if let (Some(f), None) = (&cell.formula, &mine.formula) {
                errs.push(format!("{at}: (c) formula ={f} lost"));
            }
            let (want, have) = (code(src, cell.style), code(got, mine.style));
            if want != have && !allowed(file, &sheet.name, &cell_name(r, c), Check::Format) {
                errs.push(format!("{at}: (f) format {have:?} != source {want:?}"));
            }
        }
        for (&(r, c), mine) in &theirs.cells {
            let empty_in_source = sheet.cell(r, c).is_none_or(|x| x.value.is_empty());
            if !mine.value.is_empty() && empty_in_source {
                errs.push(format!(
                    "{file}: {}!{}: (b) {:?} where the source is empty",
                    sheet.name,
                    cell_name(r, c),
                    mine.value
                ));
            }
        }
    }
    if src.date1904 != got.date1904 {
        errs.push(format!(
            "{file}: (g) date1904 {} != source {}",
            got.date1904, src.date1904
        ));
    }
    let key = |wb: &Workbook| {
        let mut v: Vec<_> = wb
            .defined_names
            .iter()
            .map(|n| {
                (
                    n.name.to_ascii_uppercase(),
                    n.scope,
                    formula_ast(&n.formula).ok(),
                )
            })
            .collect();
        v.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        v
    };
    if key(src) != key(got) {
        errs.push(format!(
            "{file}: (g) names {:?} != source {:?}",
            got.defined_names, src.defined_names
        ));
    }
}

/// (d) and (e), on the import only.
fn compare_formulas(file: &str, src: &Workbook, got: &Workbook, errs: &mut Vec<String>) {
    let mut wb = got.clone();
    let mut engine = Engine::new(&wb);
    engine.recalc_all(&mut wb);
    for (s, sheet) in src.sheets.iter().enumerate() {
        for (&(r, c), cell) in &sheet.cells {
            let Some(want) = &cell.formula else { continue };
            let Some(have) = got.sheets[s].cell(r, c).and_then(|x| x.formula.clone()) else {
                continue; // (c) reported it
            };
            let name = cell_name(r, c);
            let at = format!("{file}: {}!{name}", sheet.name);
            if !allowed(file, &sheet.name, &name, Check::Formula) {
                match (formula_ast(want), formula_ast(&have)) {
                    (Ok(a), Ok(b)) if a == b => {}
                    (Ok(_), Ok(_)) => errs.push(format!("{at}: (e) ={have} != source ={want}")),
                    (a, b) => errs.push(format!(
                        "{at}: (e) does not parse: ={have} ({:?}) / source ={want} ({:?})",
                        b.err(),
                        a.err()
                    )),
                }
            }
            if allowed(file, &sheet.name, &name, Check::Recalc) {
                continue;
            }
            let volatile = parse(want).map(|a| is_volatile(&a)).unwrap_or(false);
            if volatile {
                continue;
            }
            if engine.is_unsupported((s, r, c)) {
                errs.push(format!("{at}: (d) ={have} is unsupported"));
                continue;
            }
            let value = wb.sheets[s]
                .cell(r, c)
                .map(|x| x.value.clone())
                .unwrap_or_default();
            if !values_agree(&cell.value, &value) {
                errs.push(format!(
                    "{at}: (d) ={have} gives {value:?}, source {:?}",
                    cell.value
                ));
            }
        }
    }
}

/// The formats under test, each with its fixtures' extension.
const FORMATS: &[(&str, SourceFormat)] =
    &[("xls", SourceFormat::Xls), ("xlsb", SourceFormat::Xlsb)];

#[test]
fn legacy_imports_match_their_xlsx_originals() {
    let mut errs = Vec::new();
    let mut files = 0;
    let mut stems: Vec<_> = std::fs::read_dir(corpus("xlsx"))
        .expect("corpus/xlsx")
        .filter_map(|e| {
            let p = e.ok()?.path();
            if p.extension()? != "xlsx" {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    stems.sort();
    for stem in &stems {
        let src = load_xlsx(&std::fs::read(corpus("xlsx").join(format!("{stem}.xlsx"))).unwrap())
            .expect("corpus xlsx loads")
            .workbook;
        for &(ext, format) in FORMATS {
            let file = format!("{stem}.{ext}");
            let path = corpus("legacy").join(&file);
            let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
            files += 1;
            let (pkg, got_format) = match open_workbook(&data) {
                Ok(x) => x,
                Err(e) => {
                    errs.push(format!("{file}: does not open: {e}"));
                    continue;
                }
            };
            assert_eq!(got_format, format, "{file}");
            compare_static(&file, &src, &pkg.workbook, &mut errs);
            compare_formulas(&file, &src, &pkg.workbook, &mut errs);
            // (h) what a user gets: open, save as .xlsx, reopen.
            let back = load_xlsx(&save_xlsx(&pkg)).expect("saved import reloads");
            let mut round = Vec::new();
            compare_static(&file, &src, &back.workbook, &mut round);
            errs.extend(round.into_iter().map(|e| format!("(h) after save: {e}")));
        }
    }
    println!(
        "legacy oracle: {files} files, {} allowlist entries",
        ALLOW.len()
    );
    for a in ALLOW {
        println!(
            "  allow {} {}!{} {:?}: {}",
            a.file, a.sheet, a.cell, a.checks, a.why
        );
    }
    assert_eq!(
        files,
        17 * FORMATS.len(),
        "expected 17 workbooks per format"
    );
    assert!(
        errs.is_empty(),
        "{} problems:\n{}",
        errs.len(),
        errs.join("\n")
    );
}
