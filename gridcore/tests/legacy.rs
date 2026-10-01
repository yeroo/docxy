//! The legacy-format oracle (#603): `corpus/legacy/<stem>.{xls,xlsb,ods}`
//! are the `corpus/xlsx/<stem>.xlsx` workbooks saved by Excel itself in the
//! other formats (scripts/make-legacy-fixtures.ps1). Importing one must give
//! the workbook the `.xlsx` holds: the same sheets, values, formulas, number
//! formats, date system and names, and the same results on recalculation.
//! Every exception is an entry in [`ALLOW`], with its reason.

use gridcore::engine::Engine;
use gridcore::formula::{is_volatile, parse};
use gridcore::legacy::{SourceFormat, open_workbook};
use gridcore::sheet::{CellValue, Workbook, cell_name};
use gridcore::xlsx::{load_xlsx, save_xlsx};

/// What an allowlist entry exempts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Check {
    /// (a): the sheet's name (the cells are still compared by position).
    SheetName,
    /// (b): the cached value the file holds.
    Value,
    /// (d): the recalculated value.
    Recalc,
    /// (e): the formula's text.
    Formula,
    /// (f): the number-format code.
    Format,
}

/// One exemption: in `file`, the `cells` of sheet `sheet` (`["*"]` for
/// every cell of it) skip `checks`, because `why`.
struct Allow {
    file: &'static str,
    sheet: &'static str,
    cells: &'static [&'static str],
    checks: &'static [Check],
    why: &'static str,
}

/// shape-salestable's cells whose formulas use structured references.
const STRUCTURED_REFS: &[&str] = &[
    "D2", "D3", "D4", "D5", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8",
];

const ALLOW: &[Allow] = &[
    // The source formulas are `SUM(Q1:Q3!A1:A1)` and so on, unquoted. When
    // Excel opened that .xlsx to make the fixtures it read `Q1` as a cell,
    // so its cached results are #VALUE! (and 0 for COUNT), and what it wrote
    // to the .xls is `SUM(Q1:'Q3'!A1:A1)`. The import keeps Excel's cached
    // values; gridcore reads the formula as the 3D reference, so (d) holds.
    Allow {
        file: "calc-3d.xlsb",
        sheet: "Total",
        cells: &["*"],
        checks: &[Check::Value],
        why: "Excel cached its misreading of Q1:Q3! (Q1 as a cell)",
    },
    Allow {
        file: "calc-3d.xls",
        sheet: "Total",
        cells: &["*"],
        checks: &[Check::Value],
        why: "Excel cached its misreading of Q1:Q3! (Q1 as a cell)",
    },
    Allow {
        file: "calc-3d.ods",
        sheet: "Total",
        cells: &["*"],
        checks: &[Check::Value],
        why: "Excel cached its misreading of Q1:Q3! (Q1 as a cell)",
    },
    // Excel's ODS writer renames sheet "Calc Zone" to "Calc_Zone" in the
    // file itself (table:name), and the formula naming it follows.
    Allow {
        file: "calc-refs.ods",
        sheet: "Calc Zone",
        cells: &["*"],
        checks: &[Check::SheetName],
        why: "Excel's .ods export renamed the sheet to Calc_Zone",
    },
    Allow {
        file: "calc-refs.ods",
        sheet: "Calc Zone",
        cells: &["A7"],
        checks: &[Check::Formula],
        why: "refers to the renamed sheet: =Calc_Zone!A1+1",
    },
    // The cells of shape-salestable that hold structured references: the
    // calculated column D2:D5 and the summaries F1:F8. Each format stores
    // them as the ranges they cover (`Sales[Qty]` → `Orders!$B$2:$B$5`):
    // an .xls has no tables, Excel's .ods export writes ranges, and the
    // .xlsb's table isn't imported (tables are a follow-up), so its ptgList
    // tokens are read as the ranges, as Excel writes them to an .xls. The
    // values and recalculation agree.
    Allow {
        file: "shape-salestable.xls",
        sheet: "Orders",
        cells: STRUCTURED_REFS,
        checks: &[Check::Formula],
        why: ".xls has no tables; structured refs are stored as ranges",
    },
    Allow {
        file: "shape-salestable.ods",
        sheet: "Orders",
        cells: STRUCTURED_REFS,
        checks: &[Check::Formula],
        why: "Excel's .ods export writes structured refs as ranges",
    },
    Allow {
        file: "shape-salestable.xlsb",
        sheet: "Orders",
        cells: STRUCTURED_REFS,
        checks: &[Check::Formula],
        why: "tables are not imported; structured refs become ranges",
    },
];

fn allowed(file: &str, sheet: &str, cell: &str, check: Check) -> bool {
    ALLOW.iter().any(|a| {
        a.file == file
            && a.sheet == sheet
            && a.cells.iter().any(|c| *c == "*" || *c == cell)
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

/// A formula as compared, parsed: whitespace, case and `_xlfn.` spelling
/// vanish in the AST. Excel wraps IFS and SWITCH in an implicit
/// intersection (`_xlfn.SINGLE(…)`, shown `@`) when it writes them to an
/// older format, which the `.xlsx` source doesn't have; both spellings are
/// unwrapped first.
fn formula_ast(src: &str) -> Result<gridcore::formula::Expr, String> {
    const SINGLE: &str = "_XLFN.SINGLE(";
    let mut bare = String::with_capacity(src.len());
    // Per open parenthesis: whether it was a SINGLE( whose `)` goes too.
    let mut parens: Vec<bool> = Vec::new();
    let mut in_str = false;
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        let ch = rest.chars().next().unwrap();
        if ch == '"' {
            in_str = !in_str;
        } else if !in_str {
            if rest.len() >= SINGLE.len() && rest[..SINGLE.len()].eq_ignore_ascii_case(SINGLE) {
                parens.push(true);
                i += SINGLE.len();
                continue;
            }
            match ch {
                '@' => {
                    i += 1;
                    continue;
                }
                '(' => parens.push(false),
                ')' if parens.pop() == Some(true) => {
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        bare.push(ch);
        i += ch.len_utf8();
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
    if src.sheets.len() != got.sheets.len() {
        errs.push(format!(
            "{file}: (a) sheets {:?} != {:?}",
            names(got),
            names(src)
        ));
        return;
    }
    for (s, sheet) in src.sheets.iter().enumerate() {
        let theirs = &got.sheets[s];
        if theirs.name != sheet.name && !allowed(file, &sheet.name, "*", Check::SheetName) {
            errs.push(format!(
                "{file}: (a) sheet {s} is {:?}, source {:?}",
                theirs.name, sheet.name
            ));
        }
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
const FORMATS: &[(&str, SourceFormat)] = &[
    ("xls", SourceFormat::Xls),
    ("xlsb", SourceFormat::Xlsb),
    ("ods", SourceFormat::Ods),
];

#[test]
fn legacy_imports_match_their_xlsx_originals() {
    let mut errs = Vec::new();
    let mut files = 0;
    // (source .xlsx, the directory of its legacy copies, stem): the xlsx
    // corpus, and corpus/legacy/extra, whose sources Excel saved itself.
    let mut books: Vec<(std::path::PathBuf, std::path::PathBuf, String)> = Vec::new();
    for (src_dir, legacy_dir) in [
        (corpus("xlsx"), corpus("legacy")),
        (
            corpus("legacy").join("extra"),
            corpus("legacy").join("extra"),
        ),
    ] {
        for e in std::fs::read_dir(&src_dir).expect("corpus dir") {
            let p = e.expect("dir entry").path();
            if p.extension().is_some_and(|x| x == "xlsx") {
                let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
                books.push((p, legacy_dir.clone(), stem));
            }
        }
    }
    books.sort();
    for (src_path, legacy_dir, stem) in &books {
        let src = load_xlsx(&std::fs::read(src_path).unwrap())
            .expect("corpus xlsx loads")
            .workbook;
        for &(ext, format) in FORMATS {
            let file = format!("{stem}.{ext}");
            let path = legacy_dir.join(&file);
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
            a.file,
            a.sheet,
            a.cells.join(","),
            a.checks,
            a.why
        );
    }
    assert_eq!(
        files,
        18 * FORMATS.len(),
        "expected 18 workbooks per format"
    );
    assert!(
        errs.is_empty(),
        "{} problems:\n{}",
        errs.len(),
        errs.join("\n")
    );
}

/// `corpus/legacy/addin`: Excel's `.xlsx` of a workbook calling a function
/// of the add-in shipped with Office (EUROTOOL.XLAM), and the import of
/// Excel's `addin-udf.<ext>` of it. Excel stores the call as a library
/// external link (#888, #890).
fn add_in_books(ext: &str) -> (Workbook, gridcore::xlsx::SheetPackage) {
    let dir = corpus("legacy").join("addin");
    let src = load_xlsx(&std::fs::read(dir.join("addin-udf.xlsx")).unwrap())
        .expect("addin-udf.xlsx loads")
        .workbook;
    let (pkg, format) =
        open_workbook(&std::fs::read(dir.join(format!("addin-udf.{ext}"))).unwrap())
            .unwrap_or_else(|e| panic!("{ext} opens: {e}"));
    let want = match ext {
        "xls" => SourceFormat::Xls,
        _ => SourceFormat::Xlsb,
    };
    assert_eq!(format, want);
    (src, pkg)
}

/// Every formula of `src` is in `got`, spelled the same: `[1]!EUROCONVERT`
/// doesn't parse, so (e) compares text, not the AST.
fn same_formula_text(file: &str, src: &Workbook, got: &Workbook, errs: &mut Vec<String>) {
    for (s, sheet) in src.sheets.iter().enumerate() {
        for (&(r, c), cell) in &sheet.cells {
            let Some(want) = &cell.formula else { continue };
            let have = got.sheets[s].cell(r, c).and_then(|x| x.formula.as_deref());
            if have != Some(want.as_str()) {
                errs.push(format!(
                    "{file}: {}!{}: (e) ={have:?} != source ={want}",
                    sheet.name,
                    cell_name(r, c)
                ));
            }
        }
    }
}

#[test]
fn xlsb_add_in_function_keeps_its_formula_and_link() {
    add_in_function_keeps_its_formula_and_link("xlsb");
}

#[test]
fn xls_add_in_function_keeps_its_formula_and_link() {
    add_in_function_keeps_its_formula_and_link("xls");
}

fn add_in_function_keeps_its_formula_and_link(ext: &str) {
    let file = format!("addin-udf.{ext}");
    let (src, pkg) = add_in_books(ext);
    let got = &pkg.workbook;
    let formula = |wb: &Workbook, r, c| wb.sheets[0].cell(r, c).and_then(|x| x.formula.clone());
    assert_eq!(
        formula(got, 0, 1).as_deref(),
        Some(r#"[1]!EUROCONVERT(A1,"DEM","EUR")"#)
    );
    assert_eq!(
        formula(got, 1, 1).as_deref(),
        Some(r#"[1]!EUROCONVERT(A1,"FRF","EUR")*2"#)
    );
    let mut errs = Vec::new();
    compare_static(&file, &src, got, &mut errs);
    same_formula_text(&file, &src, got, &mut errs);
    assert!(errs.is_empty(), "{}", errs.join("\n"));

    // Recalculation can't call the add-in, so the cached values stay.
    let mut wb = got.clone();
    Engine::new(&wb).recalc_all(&mut wb);
    let value = |r, c| wb.sheets[0].cell(r, c).map(|x| x.value.clone());
    assert_eq!(value(0, 1), Some(CellValue::Number(51.13)));
    assert_eq!(value(1, 1), Some(CellValue::Number(30.48)));
}

#[test]
fn xlsb_add_in_link_survives_save_as_xlsx() {
    add_in_link_survives_save_as_xlsx("xlsb");
}

#[test]
fn xls_add_in_link_survives_save_as_xlsx() {
    add_in_link_survives_save_as_xlsx("xls");
}

fn add_in_link_survives_save_as_xlsx(ext: &str) {
    let file = format!("addin-udf.{ext} (h)");
    let (src, pkg) = add_in_books(ext);
    let bytes = save_xlsx(&pkg);
    let zip = opccore::zip::ZipArchive::open(&bytes).expect("saved package");
    let part = |name: &str| {
        String::from_utf8(zip.read(name).unwrap_or_else(|| panic!("{name} missing"))).unwrap()
    };

    let link = part("xl/externalLinks/externalLink1.xml");
    assert!(
        link.contains(r#"<externalBook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rId1">"#),
        "{link}"
    );
    assert_eq!(link.matches("<sheetName ").count(), 21, "{link}");
    assert!(link.contains(r#"<sheetName val="1028"/>"#), "{link}");
    assert!(
        link.contains(r#"<definedNames><definedName name="EUROCONVERT"/></definedNames>"#),
        "{link}"
    );
    // The link part's own rels: the library one (rId1) is what sends Excel
    // to its Library folder. The .xlsb also has the absolute path Excel
    // found the add-in at (rId2); the .xls's SUPBOOK names only the Library
    // file, so its import writes rId1 alone.
    let rels = part("xl/externalLinks/_rels/externalLink1.xml.rels");
    assert!(
        rels.contains(r#"<Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/xlExternalLinkPath/xlLibrary" Target="EUROTOOL.XLAM" TargetMode="External"/>"#),
        "{rels}"
    );
    let absolute = r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="file:///C:\Program%20Files\Microsoft%20Office\Root\Office16\Library\EUROTOOL.XLAM" TargetMode="External"/>"#;
    assert_eq!(rels.contains(absolute), ext == "xlsb", "{rels}");
    assert_eq!(
        rels.matches("<Relationship ").count(),
        if ext == "xlsb" { 2 } else { 1 },
        "{rels}"
    );
    assert!(part("[Content_Types].xml").contains(
        r#"<Override PartName="/xl/externalLinks/externalLink1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml"/>"#
    ));

    // workbook.xml refers to it through an externalLink rel, before the
    // defined names (CT_Workbook order).
    let wb_rels = part("xl/_rels/workbook.xml.rels");
    let rel = wb_rels
        .split("<Relationship ")
        .find(|r| r.contains(r#"Target="externalLinks/externalLink1.xml""#))
        .unwrap_or_else(|| panic!("no workbook rel: {wb_rels}"));
    assert!(
        rel.contains(r#"Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink""#),
        "{rel}"
    );
    let rid = rel
        .split("Id=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let workbook = part("xl/workbook.xml");
    let refs = workbook
        .find(&format!(
            r#"<externalReferences><externalReference r:id="{rid}"/></externalReferences>"#
        ))
        .unwrap_or_else(|| panic!("no externalReferences: {workbook}"));
    let names = workbook
        .find("<definedNames>")
        .unwrap_or_else(|| panic!("no definedNames: {workbook}"));
    assert!(
        workbook.find("</sheets>").unwrap() < refs && refs < names,
        "{workbook}"
    );

    // Reopened: the same formulas, values and names.
    let back = load_xlsx(&bytes).expect("saved import reloads").workbook;
    let mut errs = Vec::new();
    compare_static(&file, &src, &back, &mut errs);
    same_formula_text(&file, &src, &back, &mut errs);
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

#[test]
fn xlsb_xll_function_keeps_its_formula() {
    xll_function_keeps_its_formula("xlsb");
}

#[test]
fn xls_xll_function_keeps_its_formula() {
    xll_function_keeps_its_formula("xls");
}

/// `corpus/legacy/addin/xll-udf`: a call to XLLTWICE, which an XLL add-in
/// (scripts/xll-fixture) registers. Excel's `.xlsx` spells it
/// `_xll.XLLTWICE(A1)`, with no external link. The `.xlsb` stores it as a
/// ptgNameX into a BrtSupAddin book, the `.xls` into the 0x3A01 add-in
/// SUPBOOK. Both imports, and their saves as `.xlsx`, spell it the same.
fn xll_function_keeps_its_formula(ext: &str) {
    let dir = corpus("legacy").join("addin");
    let src = load_xlsx(&std::fs::read(dir.join("xll-udf.xlsx")).unwrap())
        .expect("xll-udf.xlsx loads")
        .workbook;
    let (pkg, _) = open_workbook(&std::fs::read(dir.join(format!("xll-udf.{ext}"))).unwrap())
        .unwrap_or_else(|e| panic!("{ext} opens: {e}"));
    let got = &pkg.workbook;
    let formula = |wb: &Workbook, r, c| wb.sheets[0].cell(r, c).and_then(|x| x.formula.clone());
    assert_eq!(formula(got, 0, 1).as_deref(), Some("_xll.XLLTWICE(A1)"));
    assert_eq!(formula(got, 1, 1).as_deref(), Some("_xll.XLLTWICE(A1)+1"));
    let file = format!("xll-udf.{ext}");
    let mut errs = Vec::new();
    compare_static(&file, &src, got, &mut errs);
    same_formula_text(&file, &src, got, &mut errs);

    let bytes = save_xlsx(&pkg);
    let back = load_xlsx(&bytes).expect("saved import reloads");
    assert!(
        !back.part_names().iter().any(|n| n.contains("externalLink")),
        "{:?}",
        back.part_names()
    );
    let file = format!("xll-udf.{ext} (h)");
    compare_static(&file, &src, &back.workbook, &mut errs);
    same_formula_text(&file, &src, &back.workbook, &mut errs);
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

#[test]
fn xlsb_external_workbook_name_keeps_only_its_value() {
    external_workbook_name_keeps_only_its_value("xlsb");
}

#[test]
fn xls_external_workbook_name_keeps_only_its_value() {
    external_workbook_name_keeps_only_its_value("xls");
}

/// A name of an ordinary workbook (`SUM([1]!Prices)`, `[1]!Half*2` in
/// Excel's `.xlsx`) has a definition and cached cells the import doesn't
/// read: its formula is dropped and the value kept, and no link is written.
/// In the `.xls` the book's SUPBOOK path is an ordinary path, not a Library
/// file, so it gets no link either.
fn external_workbook_name_keeps_only_its_value(ext: &str) {
    let dir = corpus("legacy").join("addin");
    let (pkg, _) = open_workbook(&std::fs::read(dir.join(format!("ext-name.{ext}"))).unwrap())
        .unwrap_or_else(|e| panic!("{ext} opens: {e}"));
    let sheet = &pkg.workbook.sheets[0];
    for (r, want) in [(0, 7.0), (1, 1.0)] {
        let cell = sheet.cell(r, 0).expect("cell");
        assert_eq!(cell.value, CellValue::Number(want), "row {r}");
        assert_eq!(cell.formula, None, "row {r}");
    }
    let bytes = save_xlsx(&pkg);
    let back = load_xlsx(&bytes).expect("saved import reloads");
    assert!(
        !back.part_names().iter().any(|n| n.contains("externalLink")),
        "{:?}",
        back.part_names()
    );
}
