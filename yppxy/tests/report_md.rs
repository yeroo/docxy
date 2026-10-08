//! `yppxy <in> --report-md <report> <out.md>`: a View Report, headless (#1123).
use std::path::Path;
use std::process::Command;

#[test]
fn headless_report_md_writes_the_report_and_refuses_an_unknown_one() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-report-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/10-summary.xml");
    let out = dir.join("costs.md");
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg(&input)
        .args(["--report-md", "task-cost-overview"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(
        md.starts_with("# ") && md.contains(": Task Cost Overview\n"),
        "{md}"
    );
    assert!(md.contains("| **Total** |"), "{md}");

    let bad = dir.join("bad.md");
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg(&input)
        .args(["--report-md", "burndown"])
        .arg(&bad)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("unknown report: burndown"), "{stderr}");
    assert!(stderr.contains("--report-md <report> <out>"), "{stderr}");
    assert!(!bad.exists());
    std::fs::remove_dir_all(dir).unwrap();
}
