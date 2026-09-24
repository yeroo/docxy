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
