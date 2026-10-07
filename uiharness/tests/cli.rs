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

/// #583: `run` gives every script file its own sandbox — the model
/// scripts/ui-linux-sweep.py runs every script under — so no file inherits
/// another file's session. The fake suite cannot serve the control protocol,
/// so the run dies on a connect timeout either way, leaving the first file's
/// sandbox on disk; the directory name must be the per-file form
/// `sandbox-<pid>-<stamp>-<index>`, which a run on current code does not
/// produce (its name has three parts and no index — that is what this test
/// catches). The behavioural half (the second file starts without the first
/// file's tabs) needs a live suite: tests/multi_file.rs, run with
/// `-- --ignored` like tab_close.
#[test]
fn run_gives_each_script_file_its_own_sandbox() {
    let (dir, name) = fake_suite_dir("583");
    let run_dir = dir.join("runs-583");
    for (file, case) in [("first.uit", "first file"), ("second.uit", "second file")] {
        std::fs::write(dir.join(file), format!("test {case}\n  assert tabs is 1\n")).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .current_dir(&dir)
        .env_remove("UIHARNESS_SUITE")
        .arg("run")
        .arg(dir.join("first.uit"))
        .arg(dir.join("second.uit"))
        .arg("--suite")
        .arg(&name)
        .arg("--run")
        .arg(&run_dir)
        .output()
        .expect("run uiharness run with two script files");
    let sandboxes: Vec<_> = std::fs::read_dir(&run_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.starts_with("sandbox-"))
        .collect();
    assert!(
        !sandboxes.is_empty(),
        "a launch must have created its sandbox (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    for name in &sandboxes {
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(
            parts.len(),
            4,
            "sandbox {name} is not sandbox-<pid>-<stamp>-<index> (the pre-#583 form)"
        );
        assert_eq!(parts[3], "0", "the first file's index, in {name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// FIX r1 (#583): a launch or connect failure lands under the file's path and
/// the run still reports the driven binary, the path and the evidence
/// directory — the transcripts collected so far are not dropped. Before the
/// per-file loop these errors returned the bare message and nothing else.
#[test]
fn a_failed_launch_still_reports_the_transcript() {
    let (dir, name) = fake_suite_dir("583b");
    let run_dir = dir.join("runs-583b");
    std::fs::write(dir.join("only.uit"), "test only file\n  assert tabs is 1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .current_dir(&dir)
        .env_remove("UIHARNESS_SUITE")
        .arg("run")
        .arg(dir.join("only.uit"))
        .arg("--suite")
        .arg(&name)
        .arg("--run")
        .arg(&run_dir)
        .output()
        .expect("run uiharness run against a dead fake suite");
    assert!(!out.status.success(), "status: {:?}", out.status.code());
    let stderr = String::from_utf8_lossy(&out.stderr);
    for expected in ["suite:", "only.uit", "ERROR", "evidence:"] {
        assert!(
            stderr.contains(expected),
            "the transcript must name '{expected}':\n{stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// #583: a caller-named `--sandbox` is one config root, and the app restores
/// the previous session from it at launch — fine for one file, but with
/// several files the second instance would reopen the first file's tabs. The
/// run refuses before launching anything.
#[test]
fn run_refuses_a_named_sandbox_for_several_scripts() {
    let started = Instant::now();
    let dir = std::env::temp_dir().join(format!("uiharness-583-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, case) in [("first.uit", "first file"), ("second.uit", "second file")] {
        std::fs::write(dir.join(file), format!("test {case}\n  assert tabs is 1\n")).unwrap();
    }
    let named = dir.join("named-sandbox");
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .current_dir(&dir)
        .env_remove("UIHARNESS_SUITE")
        .arg("run")
        .arg(dir.join("first.uit"))
        .arg(dir.join("second.uit"))
        .arg("--sandbox")
        .arg(&named)
        .output()
        .expect("run uiharness run with a named sandbox and two scripts");
    let elapsed = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "status: {:?}", out.status.code());
    assert!(
        stderr.contains("--sandbox"),
        "the refusal names the flag: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the refusal must come before any launch and its connect timeout: {elapsed:?}"
    );
    assert!(
        !named.exists(),
        "a refused run must not create the named sandbox"
    );
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
