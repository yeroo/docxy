//! Exercise the actual CLI argument/load/write path for imported workbooks.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("xlsxy-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        for entry in std::fs::read_dir(&self.0).unwrap().flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn run(source: &Path, mode: &str, target: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xlsxy"))
        .arg(source)
        .arg(mode)
        .arg(target)
        .output()
        .unwrap()
}

#[test]
fn csv_and_recalc_refuse_the_actual_csv_or_tsv_input() {
    let dir = Dir::new("import-guard");
    for (extension, bytes) in [
        ("csv", "first;second\n1;2\n"),
        ("tsv", "first\tsecond\n1\t2\n"),
    ] {
        let source = dir.0.join(format!("input.{extension}"));
        std::fs::write(&source, bytes).unwrap();
        let alternate_spelling = dir.0.join(format!("./input.{extension}"));
        let hard_link = dir.0.join(format!("alias.{extension}"));
        std::fs::hard_link(&source, &hard_link).unwrap();
        for mode in ["--csv", "--recalc"] {
            for alias in [&alternate_spelling, &hard_link] {
                let result = run(&source, mode, alias);
                assert!(
                    !result.status.success(),
                    "{extension} {mode} unexpectedly succeeded"
                );
                assert!(
                    String::from_utf8_lossy(&result.stderr)
                        .contains("cannot overwrite the source document")
                );
                assert_eq!(std::fs::read(&source).unwrap(), bytes.as_bytes());
                assert_eq!(std::fs::read(alias).unwrap(), bytes.as_bytes());
            }
        }
    }
}

/// #727 r1: a headless `--csv` from a template never writes over the
/// template, even through another spelling or a hard link. (The editor binds
/// a template to a new `<stem>N.xlsx`; a headless run does not.)
#[test]
fn csv_export_of_a_template_refuses_the_template_itself() {
    use gridcore::xlsx::{SpreadsheetKind, new_xlsx, save_xlsx_as};
    let dir = Dir::new("template-guard");
    let source = dir.0.join("Budget.xltx");
    let bytes = save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template);
    std::fs::write(&source, &bytes).unwrap();
    let hard_link = dir.0.join("alias.xltx");
    std::fs::hard_link(&source, &hard_link).unwrap();
    for target in [source.clone(), dir.0.join("./Budget.xltx"), hard_link] {
        let result = run(&source, "--csv", &target);
        assert!(
            !result.status.success(),
            "{target:?} unexpectedly succeeded"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("cannot overwrite the source document"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }
    let out = dir.0.join("out.csv");
    let result = run(&source, "--csv", &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(out.is_file());
    assert!(!dir.0.join("Budget1.xlsx").exists());
}

#[test]
fn imported_export_protects_rebound_workbook_and_recalc_still_saves_xlsx_in_place() {
    let dir = Dir::new("rebound-guard");
    let source = dir.0.join("input.csv");
    let binding = dir.0.join("input.xlsx");
    let alias = dir.0.join("binding-alias.csv");
    std::fs::write(&source, b"first,second\n1,2\n").unwrap();
    let result = run(&source, "--recalc", &binding);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let workbook = std::fs::read(&binding).unwrap();
    assert!(gridcore::xlsx::load_xlsx(&workbook).is_ok());
    // input.xlsx is taken now, so the CSV binds to input1.xlsx (#876); an
    // export still never replaces input.xlsx, by its name or a link.
    std::fs::hard_link(&binding, &alias).unwrap();
    for out in [&alias, &binding] {
        let result = run(&source, "--csv", out);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("cannot overwrite"));
    }
    assert_eq!(std::fs::read(&binding).unwrap(), workbook);
    assert_eq!(std::fs::read(&alias).unwrap(), workbook);
    assert!(!dir.0.join("input1.xlsx").exists());
    let result = run(&binding, "--recalc", &binding);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(gridcore::xlsx::load_xlsx(&std::fs::read(&binding).unwrap()).is_ok());
}

fn content_types(path: &Path) -> String {
    let pkg = gridcore::xlsx::load_xlsx(&std::fs::read(path).unwrap()).unwrap();
    String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned()
}

/// #601: `xlsxy budget.xltx --recalc out.xlsx` writes a workbook, not a
/// template Excel refuses under an .xlsx name.
#[test]
fn recalc_of_a_template_to_xlsx_writes_a_workbook() {
    use gridcore::xlsx::{SpreadsheetKind, new_xlsx, save_xlsx_as};
    let dir = Dir::new("template-to-xlsx");
    let source = dir.0.join("budget.xltx");
    std::fs::write(
        &source,
        save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template),
    )
    .unwrap();
    let out = dir.0.join("out.xlsx");
    let result = run(&source, "--recalc", &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let ct = content_types(&out);
    assert!(ct.contains("spreadsheetml.sheet.main+xml"), "{ct}");
    assert!(!ct.contains("template"), "{ct}");
}

/// #727: `xlsxy in.xlsm --recalc out.xlsx` drops Excel 4.0 macro sheets,
/// saying so; `--recalc out.xlsm` keeps them.
#[test]
fn recalc_of_a_workbook_with_macro_sheets_to_xlsx_drops_them() {
    use gridcore::xlsx::{SpreadsheetKind, load_xlsx, new_xlsx, save_xlsx, save_xlsx_as};
    let dir = Dir::new("xlm-to-xlsx");
    let mut pkg = load_xlsx(&save_xlsx_as(&new_xlsx(), SpreadsheetKind::MacroWorkbook)).unwrap();
    pkg.add_sheet("Macro1");
    let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
        .replace(
            r#"relationships/worksheet" Target="worksheets/sheet2.xml""#,
            r#"relationships/xlMacrosheet" Target="worksheets/sheet2.xml""#,
        )
        .replace(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/xlMacrosheet",
            "http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet",
        );
    pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
    assert!(pkg.has_macro_sheets());
    let source = dir.0.join("in.xlsm");
    std::fs::write(&source, save_xlsx(&pkg)).unwrap();

    let out = dir.0.join("out.xlsx");
    let result = run(&source, "--recalc", &out);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("note: Excel 4.0 macro sheets not saved in macro-free .xlsx workbook"),
        "{stderr}"
    );
    assert!(!stderr.contains("VB project"), "{stderr}");
    let saved = load_xlsx(&std::fs::read(&out).unwrap()).unwrap();
    assert!(!saved.has_macro_sheets());
    assert_eq!(saved.workbook.sheets.len(), 1);

    let kept = dir.0.join("kept.xlsm");
    let result = run(&source, "--recalc", &kept);
    assert!(result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("note:"));
    let saved = load_xlsx(&std::fs::read(&kept).unwrap()).unwrap();
    assert!(saved.has_macro_sheets());
}

/// #601: `xlsxy in.xlsm --recalc out.xlsx` drops the VBA project (saying so)
/// and the macro type; `--recalc out.xlsm` keeps both.
#[test]
fn recalc_of_a_macro_workbook_to_xlsx_drops_the_vba_project() {
    use gridcore::xlsx::{SpreadsheetKind, load_xlsx, new_xlsx, save_xlsx, save_xlsx_as};
    let dir = Dir::new("xlsm-to-xlsx");
    let mut pkg = load_xlsx(&save_xlsx_as(&new_xlsx(), SpreadsheetKind::MacroWorkbook)).unwrap();
    let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap()).replace(
        "</Relationships>",
        r#"<Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#,
    );
    pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
    let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).replace(
        "</Types>",
        r#"<Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/></Types>"#,
    );
    pkg.set_part("[Content_Types].xml", ct.into_bytes());
    pkg.set_part("xl/vbaProject.bin", b"VBA".to_vec());
    let source = dir.0.join("in.xlsm");
    std::fs::write(&source, save_xlsx(&pkg)).unwrap();

    let out = dir.0.join("out.xlsx");
    let result = run(&source, "--recalc", &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("note: VB project not saved in macro-free .xlsx workbook")
    );
    let ct = content_types(&out);
    assert!(ct.contains("spreadsheetml.sheet.main+xml"), "{ct}");
    assert!(
        !ct.contains("macroEnabled") && !ct.contains("vbaProject"),
        "{ct}"
    );
    let saved = load_xlsx(&std::fs::read(&out).unwrap()).unwrap();
    assert!(saved.part("xl/vbaProject.bin").is_none());
    assert!(!saved.has_vba_project());

    let kept = dir.0.join("kept.xlsm");
    let result = run(&source, "--recalc", &kept);
    assert!(result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("VB project"));
    let saved = load_xlsx(&std::fs::read(&kept).unwrap()).unwrap();
    assert!(saved.has_vba_project());
    assert!(content_types(&kept).contains("sheet.macroEnabled.main+xml"));
}

/// #657: a typed `#SPILL!` Excel saved as a rich error (`<v>#VALUE!</v>` plus
/// `vm`) survives `--recalc`: the cell keeps its `vm` and `#VALUE!` body, and
/// ERROR.TYPE of it recalculates to 9, not 3.
#[test]
fn recalc_keeps_a_rich_spill_error() {
    use gridcore::xlsx::{load_xlsx, new_xlsx, save_xlsx};
    let dir = Dir::new("rich-error");
    let mut pkg = load_xlsx(&save_xlsx(&new_xlsx())).unwrap();
    let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap()).replace(
        "</Relationships>",
        r#"<Relationship Id="rId91" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata" Target="metadata.xml"/><Relationship Id="rId92" Type="http://schemas.microsoft.com/office/2017/06/relationships/rdRichValue" Target="richData/rdrichvalue.xml"/><Relationship Id="rId93" Type="http://schemas.microsoft.com/office/2017/06/relationships/rdRichValueStructure" Target="richData/rdrichvaluestructure.xml"/></Relationships>"#,
    );
    pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
    pkg.set_part(
        "xl/metadata.xml",
        br#"<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xlrd="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata"><metadataTypes count="1"><metadataType name="XLRICHVALUE"/></metadataTypes><futureMetadata name="XLRICHVALUE" count="1"><bk><extLst><ext uri="{3e2802c4-a4d2-4d8b-9148-e3be6c30e623}"><xlrd:rvb i="0"/></ext></extLst></bk></futureMetadata><valueMetadata count="1"><bk><rc t="1" v="0"/></bk></valueMetadata></metadata>"#.to_vec(),
    );
    pkg.set_part(
        "xl/richData/rdrichvalue.xml",
        br#"<rvData xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata" count="1"><rv s="0"><v>8</v></rv></rvData>"#.to_vec(),
    );
    pkg.set_part(
        "xl/richData/rdrichvaluestructure.xml",
        br#"<rvStructures xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata" count="1"><s t="_error"><k n="errorType" t="i"/></s></rvStructures>"#.to_vec(),
    );
    // Save regenerates worksheets from the model, so the sheet goes into the
    // zip afterwards, exactly as Excel wrote it.
    let saved = save_xlsx(&pkg);
    let zip = opccore::zip::ZipArchive::open(&saved).unwrap();
    let entries: Vec<(String, Vec<u8>)> = zip
        .entries()
        .iter()
        .map(|e| {
            let bytes = if e.name == "xl/worksheets/sheet1.xml" {
                br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="e" vm="1"><v>#VALUE!</v></c><c r="B1"><f>ERROR.TYPE(A1)</f><v>3</v></c></row></sheetData></worksheet>"#.to_vec()
            } else {
                zip.extract(e).unwrap()
            };
            (e.name.clone(), bytes)
        })
        .collect();
    let source = dir.0.join("rich.xlsx");
    std::fs::write(&source, opccore::zipwrite::write_zip(&entries)).unwrap();

    let out = dir.0.join("out.xlsx");
    let result = run(&source, "--recalc", &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let saved = load_xlsx(&std::fs::read(&out).unwrap()).unwrap();
    let ws = String::from_utf8_lossy(saved.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
    assert!(
        ws.contains(r#"<c r="A1" t="e" vm="1"><v>#VALUE!</v></c>"#),
        "{ws}"
    );
    assert!(
        ws.contains(r#"<c r="B1"><f>ERROR.TYPE(A1)</f><v>9</v></c>"#),
        "{ws}"
    );
}

/// #604's workbook as openpyxl writes it on Windows: the second sheet is
/// active, and a line break inside a cell is a literal CR LF in the XML.
fn active_second_sheet_xlsx() -> Vec<u8> {
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let sheet1 = format!(
        r#"<worksheet xmlns="{ns}"><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>wrong sheet</t></is></c></row></sheetData></worksheet>"#
    );
    let sheet2 = format!(
        "<worksheet xmlns=\"{ns}\"><sheetViews><sheetView tabSelected=\"1\" workbookViewId=\"0\"/></sheetViews><sheetData>\
         <row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>Name</t></is></c><c r=\"B1\" t=\"inlineStr\"><is><t>Note</t></is></c></row>\
         <row r=\"2\"><c r=\"A2\" t=\"inlineStr\"><is><t>Z\u{fc}rich</t></is></c><c r=\"B2\" t=\"inlineStr\"><is><t>line1\r\nline2</t></is></c></row>\
         </sheetData></worksheet>"
    );
    let workbook = format!(
        r#"<workbook xmlns="{ns}" xmlns:r="{rel}"><bookViews><workbookView activeTab="1"/></bookViews><sheets><sheet name="First" sheetId="1" r:id="rId1"/><sheet name="Data" sheetId="2" r:id="rId2"/></sheets></workbook>"#
    );
    let wb_rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{rel}/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#
    );
    let root_rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
    );
    let content_types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
    opccore::zipwrite::write_zip(&[
        (
            "[Content_Types].xml".into(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".into(), root_rels.into_bytes()),
        ("xl/workbook.xml".into(), workbook.into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), wb_rels.into_bytes()),
        ("xl/worksheets/sheet1.xml".into(), sheet1.into_bytes()),
        ("xl/worksheets/sheet2.xml".into(), sheet2.into_bytes()),
    ])
}

/// #604: `--csv` writes Excel's CSV UTF-8 of the active sheet, byte for byte.
#[test]
fn csv_export_is_excels_csv_utf8_of_the_active_sheet() {
    let dir = Dir::new("active-csv");
    let source = dir.0.join("in.xlsx");
    std::fs::write(&source, active_second_sheet_xlsx()).unwrap();
    let out = dir.0.join("out.csv");
    let result = run(&source, "--csv", &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut want = b"\xEF\xBB\xBF".to_vec();
    want.extend_from_slice("Name,Note\r\nZ\u{fc}rich,\"line1\nline2\"\r\n".as_bytes());
    assert_eq!(std::fs::read(&out).unwrap(), want);
}

/// #607: headless runs import a .txt with the Text Import Wizard's defaults
/// (tab-delimited, fields converted) rather than failing to read it as XLSX.
#[test]
fn a_text_file_converts_headlessly_with_the_wizard_defaults() {
    let dir = Dir::new("txt-headless");
    let source = dir.0.join("in.txt");
    std::fs::write(&source, "name\tqty\r\nPen\t4\r\n").unwrap();
    let out = dir.0.join("out.csv");
    let result = run(&source, "--csv", &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read(&out).unwrap(),
        b"\xEF\xBB\xBFname,qty\r\nPen,4\r\n"
    );
}

fn corpus(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("corpus")
        .join(dir)
}

/// #603: an .xls, .xlsb or .ods input recalculates to a valid .xlsx and
/// exports CSV, as an .xlsx does.
#[test]
fn legacy_inputs_recalc_to_xlsx_and_export_csv() {
    let dir = Dir::new("legacy-in");
    for ext in ["xls", "xlsb", "ods"] {
        let source = corpus("legacy").join(format!("calc-refs.{ext}"));
        let out = dir.0.join(format!("out-{ext}.xlsx"));
        let result = run(&source, "--recalc", &out);
        assert!(
            result.status.success(),
            "{ext}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let pkg = gridcore::xlsx::load_xlsx(&std::fs::read(&out).unwrap()).unwrap();
        let names: Vec<_> = pkg
            .workbook
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names[0], "Data", "{ext}");
        assert_eq!(pkg.workbook.defined_names.len(), 2, "{ext}");
        let csv = dir.0.join(format!("out-{ext}.csv"));
        let result = run(&source, "--csv", &csv);
        assert!(result.status.success(), "{ext} --csv");
        assert!(!std::fs::read(&csv).unwrap().is_empty(), "{ext} --csv");
    }
}

/// #603: `--recalc` to an .xls, .xlsb, .ods or .xml (types Save As refuses)
/// exits 2 and writes nothing, rather than .xlsx bytes under that name.
#[test]
fn recalc_refuses_to_write_types_it_cannot_save() {
    let dir = Dir::new("legacy-out");
    let source = corpus("xlsx").join("calc-refs.xlsx");
    for (ext, label) in [
        ("xls", "Excel 97-2003 Workbook"),
        ("xlsb", "Excel Binary Workbook"),
        ("ods", "OpenDocument Spreadsheet"),
        ("xml", "XML Data"),
    ] {
        let out = dir.0.join(format!("out.{ext}"));
        let result = run(&source, "--recalc", &out);
        assert_eq!(result.status.code(), Some(2), "{ext}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&format!(
                "error: cannot save as .{ext} ({label}): write .xlsx instead"
            )),
            "{ext}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!out.exists(), "{ext}");
    }
}

/// #603: `--verify` scores an imported workbook's cached values.
#[test]
fn verify_accepts_legacy_inputs() {
    for ext in ["xls", "xlsb", "ods"] {
        let result = Command::new(env!("CARGO_BIN_EXE_xlsxy"))
            .arg(corpus("legacy").join(format!("oracle-basic.{ext}")))
            .arg("--verify")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{ext}: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

/// #882: `--read-only` never writes the input, so recalculating it in place
/// (under any spelling) is refused; another output is written as usual.
#[test]
fn recalc_in_place_is_refused_under_read_only() {
    let dir = Dir::new("read-only-recalc");
    let book = dir.0.join("book.xlsx");
    std::fs::write(
        &book,
        gridcore::xlsx::save_xlsx(&gridcore::xlsx::new_xlsx()),
    )
    .unwrap();
    let before = std::fs::read(&book).unwrap();
    for target in [book.clone(), dir.0.join("./book.xlsx")] {
        let result = Command::new(env!("CARGO_BIN_EXE_xlsxy"))
            .arg(&book)
            .arg("--read-only")
            .arg("--recalc")
            .arg(&target)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("\"book.xlsx\" is read-only. Save a copy under a new name."),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(std::fs::read(&book).unwrap(), before);
    }
    let out = dir.0.join("out.xlsx");
    let result = Command::new(env!("CARGO_BIN_EXE_xlsxy"))
        .arg(&book)
        .arg("-r")
        .arg("--recalc")
        .arg(&out)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(out.is_file());
    assert_eq!(std::fs::read(&book).unwrap(), before);
}
