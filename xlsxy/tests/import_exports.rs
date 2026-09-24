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
