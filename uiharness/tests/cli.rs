//! Process-level CLI contracts that unit tests below `main` cannot prove.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn help_prints_usage_and_exits_successfully() {
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .arg("--help")
        .output()
        .expect("run uiharness --help");
    assert!(out.status.success(), "status: {:?}", out.status.code());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("usage:"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_bad_command_fails_before_trying_to_connect() {
    let empty = std::env::temp_dir().join(format!("uiharness-no-instance-{}", std::process::id()));
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .arg("--config")
        .arg(empty)
        .arg("frobnicate")
        .output()
        .expect("run malformed uiharness command");
    let elapsed = started.elapsed();
    assert!(!out.status.success());
    assert!(
        elapsed < Duration::from_secs(5),
        "a local error waited for the 20-second connector timeout: {elapsed:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown command 'frobnicate'"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn desktop_without_a_name_is_a_usage_error() {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .arg("run")
        .arg("--desktop")
        .output()
        .expect("run uiharness run --desktop");
    let elapsed = started.elapsed();
    assert!(!out.status.success());
    assert!(
        elapsed < Duration::from_secs(5),
        "a usage error waited on something: {elapsed:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--desktop needs a value"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// #975: a directory holding an executable the `run` under test is told to
/// launch by a path relative to its own working directory. The executable is
/// a copy of this harness binary itself: handed `--harness` it refuses at
/// once (`unknown option`), so the outer harness waits out its connect
/// timeout and reports the fake's absolute path in an `exited:` line —
/// without the fix the spawn itself fails with `os error 2`.
fn fake_suite_dir(tag: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("uiharness-975-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The child absolutizes the relative name against getcwd(), the physical
    // path — resolve symlinks so the expected path matches what it reports.
    #[cfg(unix)]
    let dir = std::fs::canonicalize(&dir).unwrap();
    let name = format!("fake-suite{}", std::env::consts::EXE_SUFFIX);
    std::fs::copy(env!("CARGO_BIN_EXE_uiharness"), dir.join(&name)).unwrap();
    (dir, name)
}

/// The `run` under test starts its child with the sandbox as the child's cwd,
/// so a relative `--suite` name would be resolved there — the launch fails
/// with `os error 2` unless `find_suite` absolutizes it first.
#[test]
fn a_relative_suite_path_is_launched_from_outside_the_sandbox() {
    let (dir, name) = fake_suite_dir("arg");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("doc-state.uit");
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .current_dir(&dir)
        .env_remove("UIHARNESS_SUITE")
        .arg("run")
        .arg(&script)
        .arg("--suite")
        .arg(&name)
        .arg("--run")
        .arg("runs")
        .output()
        .expect("run uiharness run with a relative --suite");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let launched = std::path::absolute(dir.join(&name)).unwrap();
    assert!(
        !out.status.success(),
        "status: {:?}\n{stderr}",
        out.status.code()
    );
    assert!(!stderr.contains("os error 2"), "{stderr}");
    assert!(stderr.contains(" exited: "), "{stderr}");
    assert!(stderr.contains(&launched.display().to_string()), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// #975, the `UIHARNESS_SUITE` route: same absolutizing obligation as the
/// `--suite` flag — both reach the launcher through `find_suite`.
#[test]
fn a_relative_uiharness_suite_is_launched_from_outside_the_sandbox() {
    let (dir, name) = fake_suite_dir("env");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("doc-state.uit");
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .current_dir(&dir)
        .env("UIHARNESS_SUITE", &name)
        .arg("run")
        .arg(&script)
        .arg("--run")
        .arg("runs")
        .output()
        .expect("run uiharness run with a relative UIHARNESS_SUITE");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let launched = std::path::absolute(dir.join(&name)).unwrap();
    assert!(
        !out.status.success(),
        "status: {:?}\n{stderr}",
        out.status.code()
    );
    assert!(!stderr.contains("os error 2"), "{stderr}");
    assert!(stderr.contains(" exited: "), "{stderr}");
    assert!(stderr.contains(&launched.display().to_string()), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
