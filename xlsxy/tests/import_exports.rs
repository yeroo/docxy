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
    std::fs::hard_link(&binding, &alias).unwrap();
    let result = run(&source, "--csv", &alias);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot overwrite"));
    assert_eq!(std::fs::read(&binding).unwrap(), workbook);
    assert_eq!(std::fs::read(&alias).unwrap(), workbook);
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
