//! `docxy merge <main.docx> <data.csv> -o <out>` (#628).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docxcore::merge::merge_field;
use docxcore::model::{Block, Document, Inline, Paragraph, Run, RunProps};
use docxcore::package::{load_package, new_package, save_package};

fn docxy(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args(args)
        .output()
        .expect("run docxy")
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("docxy-merge-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// "Dear «First» of «City»." as a .docx.
fn main_docx(dir: &Path) -> PathBuf {
    let run = |t: &str| {
        Inline::Run(Run {
            text: t.into(),
            props: RunProps::default(),
        })
    };
    let p = RunProps::default();
    let doc = Document {
        body: vec![Block::Paragraph(Paragraph {
            content: vec![
                run("Dear "),
                merge_field("First", &p),
                run(" of "),
                merge_field("City", &p),
                run("."),
            ],
            ..Default::default()
        })],
    };
    let path = dir.join("main.docx");
    std::fs::write(&path, save_package(&new_package(doc))).unwrap();
    path
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn merge_writes_one_copy_per_recipient() {
    let dir = fresh_dir("ok");
    let main = main_docx(&dir);
    let csv = dir.join("people.csv");
    std::fs::write(
        &csv,
        "First,City\r\nJane,Paris\r\n\"John\",\"Rome, Italy\"\r\n",
    )
    .unwrap();
    let out = dir.join("out.docx");
    let run = docxy(&[Path::new("merge"), &main, &csv, Path::new("-o"), &out]);
    assert!(run.status.success(), "{}", stderr(&run));
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.starts_with("wrote ") && stdout.contains("(2 records)"),
        "{stdout}"
    );
    let merged = load_package(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(
        merged.document.plain_text(),
        "Dear Jane of Paris.\nDear John of Rome, Italy.\n"
    );
    let xml = merged.part_text("word/document.xml").unwrap();
    assert!(!xml.contains("MERGEFIELD"), "{xml}");
}

#[test]
fn merge_never_overwrites() {
    let dir = fresh_dir("exists");
    let main = main_docx(&dir);
    let csv = dir.join("people.csv");
    std::fs::write(&csv, "First,City\nJane,Paris\n").unwrap();
    let out = dir.join("out.docx");
    std::fs::write(&out, b"keep me").unwrap();
    let run = docxy(&[Path::new("merge"), &main, &csv, Path::new("-o"), &out]);
    assert!(!run.status.success());
    assert!(stderr(&run).contains("already exists"), "{}", stderr(&run));
    assert_eq!(std::fs::read(&out).unwrap(), b"keep me");
}

#[test]
fn merge_errors_cleanly_on_a_missing_or_empty_list() {
    let dir = fresh_dir("bad");
    let main = main_docx(&dir);
    let out = dir.join("out.docx");
    let cases: [(&str, Option<&str>, &str); 3] = [
        ("missing.csv", None, "cannot read the recipient list"),
        ("empty.csv", Some(""), "no header row"),
        ("header.csv", Some("First,City\n"), "no records to merge"),
    ];
    for (name, content, want) in cases {
        let csv = dir.join(name);
        if let Some(c) = content {
            std::fs::write(&csv, c).unwrap();
        }
        let run = docxy(&[Path::new("merge"), &main, &csv, Path::new("-o"), &out]);
        assert!(!run.status.success(), "{name}");
        assert!(stderr(&run).contains(want), "{name}: {}", stderr(&run));
        assert!(!out.exists(), "{name}: nothing is written");
    }
    // Usage errors.
    let run = docxy(&[Path::new("merge"), &main]);
    assert_eq!(run.status.code(), Some(2));
    assert!(
        stderr(&run).contains("usage: docxy merge"),
        "{}",
        stderr(&run)
    );
}
