use std::path::Path;
use std::process::Command;

#[test]
fn headless_save_adds_native_extension_and_refuses_mpp() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("plan");
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg("--save")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let actual = target.with_extension("yppx");
    assert!(projcore::yppx::read_yppx(&std::fs::read(&actual).unwrap()).is_ok());
    assert!(!target.exists());
    let binary = dir.join("plan.mpp");
    std::fs::write(&binary, b"original binary schedule").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg("--save")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("Project schedules can only be saved")
    );
    assert_eq!(std::fs::read(&binary).unwrap(), b"original binary schedule");
    std::fs::remove_file(actual).unwrap();
    std::fs::remove_file(binary).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

const SAVE_FORMAT_ERROR: &str = "Project schedules can only be saved as .yppx or .xml (MSPDI)";

fn yppxy_save(input: Option<&Path>, target: &Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_yppxy"));
    if let Some(input) = input {
        cmd.arg(input);
    }
    cmd.arg("--save").arg(target).output().unwrap()
}

/// Issue #78: `--save` once wrote MSPDI XML under any extension and exited 0.
/// Every row of the issue's table must either write the named format or refuse
/// without creating the file.
#[test]
fn headless_save_never_writes_one_format_under_another_name() {
    let dir = std::env::temp_dir().join(format!("yppxy-cli-save-issue78-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Distinct stems: the file system may be case-insensitive.
    for name in [
        "out.mpp",
        "out.mpt",
        "out.xlsx",
        "out.csv",
        "out.pdf",
        "upper.MPP",
    ] {
        let target = dir.join(name);
        let result = yppxy_save(None, &target);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{name}: expected refusal");
        assert!(stderr.contains(SAVE_FORMAT_ERROR), "{name}: {stderr}");
        assert!(!target.exists(), "{name}: refused save created the file");
    }

    // The issue's exact shape: an MSPDI input converted to a refused target.
    let input = dir.join("plan.xml");
    let project = projcore::editor::untitled_project();
    std::fs::write(&input, projcore::mspdi::write_mspdi(&project)).unwrap();
    let target = dir.join("converted.mpp");
    let result = yppxy_save(Some(&input), &target);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains(SAVE_FORMAT_ERROR));
    assert!(!target.exists());

    let target = dir.join("out.yppx");
    let result = yppxy_save(None, &target);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = std::fs::read(&target).unwrap();
    assert!(bytes.starts_with(b"PK"), "out.yppx is not a ZIP package");
    assert!(projcore::yppx::read_yppx(&bytes).is_ok());

    for name in ["out.xml", "upper.XML"] {
        let target = dir.join(name);
        let result = yppxy_save(None, &target);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let text = String::from_utf8(std::fs::read(&target).unwrap()).unwrap();
        assert!(text.starts_with('<'), "{name} is not XML");
        assert!(
            projcore::mspdi::read_mspdi(&text).is_ok(),
            "{name} is not MSPDI"
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}
