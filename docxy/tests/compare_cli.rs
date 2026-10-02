//! `docxy compare <original> <revised> -o <out>` (#626).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docxcore::package::load_package;

fn docxy(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args(args)
        .output()
        .expect("run docxy")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("docxy-compare-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `docxy x.md --docx x.docx`, as in the issue's reproduction.
fn docx_from_markdown(dir: &Path, stem: &str, text: &str) -> PathBuf {
    let md = dir.join(format!("{stem}.md"));
    let docx = dir.join(format!("{stem}.docx"));
    std::fs::write(&md, text).unwrap();
    let out = docxy(&[&md, Path::new("--docx"), &docx]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    docx
}

fn resolved_text(path: &Path, accept: bool) -> String {
    let mut document = load_package(&std::fs::read(path).unwrap())
        .expect("compare result loads")
        .document;
    if accept {
        document.accept_all_revisions();
    } else {
        document.reject_all_revisions();
    }
    assert!(document.revisions().is_empty());
    document.plain_text()
}

#[test]
fn compare_writes_tracked_changes_that_resolve_both_ways() {
    let dir = fresh_dir("issue");
    let original = docx_from_markdown(&dir, "orig", "The cat sat on the mat.\n");
    let revised = docx_from_markdown(&dir, "rev", "The black cat sat on a mat.\n");
    let (before_o, before_r) = (
        std::fs::read(&original).unwrap(),
        std::fs::read(&revised).unwrap(),
    );
    let out = dir.join("out.docx");

    let run = docxy(&[
        Path::new("compare"),
        &original,
        &revised,
        Path::new("-o"),
        &out,
    ]);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("(2 insertions, 1 deletions)") && stdout.starts_with("wrote "),
        "{stdout}"
    );
    assert_eq!(resolved_text(&out, true), "The black cat sat on a mat.\n");
    assert_eq!(resolved_text(&out, false), "The cat sat on the mat.\n");
    assert_eq!(
        std::fs::read(&original).unwrap(),
        before_o,
        "original untouched"
    );
    assert_eq!(
        std::fs::read(&revised).unwrap(),
        before_r,
        "revised untouched"
    );

    // Never overwrites: a second run (or an input named as the output) fails.
    let written = std::fs::read(&out).unwrap();
    for target in [&out, &revised] {
        let again = docxy(&[
            Path::new("compare"),
            &original,
            &revised,
            Path::new("-o"),
            target,
        ]);
        assert!(!again.status.success());
        let stderr = String::from_utf8_lossy(&again.stderr);
        assert!(stderr.contains("already exists"), "{stderr}");
    }
    assert_eq!(std::fs::read(&out).unwrap(), written);
    assert_eq!(std::fs::read(&revised).unwrap(), before_r);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compare_refuses_non_docx_inputs_and_bad_usage() {
    let dir = fresh_dir("refuse");
    let revised = docx_from_markdown(&dir, "rev", "Text\n");
    let md = dir.join("rev.md");
    let out = dir.join("out.docx");

    let run = docxy(&[Path::new("compare"), &md, &revised, Path::new("-o"), &out]);
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("is not a .docx file"), "{stderr}");
    assert!(!out.exists());

    let usage = docxy(&[Path::new("compare"), &revised, Path::new("-o"), &out]);
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("usage: docxy compare"));
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
