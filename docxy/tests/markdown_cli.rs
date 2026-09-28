//! The terminal entry point must reject Markdown it cannot decode exactly.

use std::path::Path;
use std::process::Command;

#[test]
fn undecodable_markdown_fails_before_creating_output() {
    let dir = std::env::temp_dir().join(format!("docxy-markdown-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("bad.md");
    let output = dir.join("out.md");
    let original = b"caf\xE9\n";
    std::fs::write(&source, original).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args([source.as_path(), Path::new("--md"), output.as_path()])
        .output()
        .expect("run docxy");
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("error:") && stderr.contains("not UTF-8 text"),
        "{stderr}"
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    std::fs::remove_file(source).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
