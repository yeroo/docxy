//! #583: `uiharness run a.uit b.uit` must give every script file the fresh
//! instance the sweep gives it — no file inherits another file's tabs.
//! Run after building suite, on a display (Xvfb + Openbox on Linux,
//! scripts/ui-linux.py):
//!
//! cargo test -p uiharness --test multi_file -- --ignored --nocapture
//!
//! The second script asserts the absolute tab count a fresh instance has
//! (the untouched sample tab plus its own opened document); with the old
//! one-instance-for-all-files run, file one's tab is still there and the
//! assert sees 3. The display-free half of the contract (the refusal of a
//! caller-named --sandbox, the per-file sandbox naming) is cli.rs.

use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn two_scripts_in_one_run_do_not_share_tabs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "uiharness-583-multi-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = root.join("fixtures");
    std::fs::write(
        dir.join("first.uit"),
        format!(
            "test first file opens fresh\n  open copy:{}/gantt-empty.xml\n  assert tabs is 2\n  assert tasks is 0\n",
            fixtures.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("second.uit"),
        format!(
            "test second file starts clean\n  open copy:{}/project-clipboard.xml\n  assert tabs is 2\n  assert tasks is 2\n",
            fixtures.display()
        ),
    )
    .unwrap();

    let exe = uiharness::launch::find_suite(None).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .current_dir(&dir)
        .env_remove("UIHARNESS_SUITE")
        .arg("run")
        .arg(dir.join("first.uit"))
        .arg(dir.join("second.uit"))
        .arg("--suite")
        .arg(&exe)
        .arg("--run")
        .arg(dir.join("runs"))
        .output()
        .expect("run uiharness run with two script files");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "status: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    for case in ["first file opens fresh", "second file starts clean"] {
        assert!(
            stdout.contains(&format!("case: {case} — ok")),
            "missing '{case} — ok' in:\n{stdout}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
