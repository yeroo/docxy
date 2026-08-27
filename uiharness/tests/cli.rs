//! Process-level CLI contracts that unit tests below `main` cannot prove.

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
