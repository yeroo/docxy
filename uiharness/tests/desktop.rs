//! #722 end to end: a suite started on a separate Win32 desktop never
//! appears on the user's one, and the harness — its capture thread attached
//! to that desktop — drives and photographs it as usual. Run after building
//! the suite:
//!
//! ```text
//! UIHARNESS_SUITE=<path-to-suite.exe> cargo test -p uiharness --test desktop -- --ignored --nocapture
//! ```
//!
//! Safe to run anywhere: the desktop is created but never switched to, so
//! nothing appears on the user's screen — that is the point of the feature.
#![cfg(windows)]

use std::path::PathBuf;
use uiharness::desktop::Desktop;
use uiharness::launch::{self};
use uiharness::{Driver, Run};

/// A sandbox under the workspace's target dir, unique like the one in
/// `tests/agwinterm_env.rs`: the app persists into it, so two runs must
/// never share one.
fn sandbox(tag: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/desktop-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = std::path::absolute(run.dir().join(tag)).unwrap();
    std::fs::create_dir_all(&sandbox).unwrap();
    sandbox
}

/// The suite starts on `WinSta0\uiharness-test-<pid>` — invisible from this
/// process's desktop, but connected, driven, captured (a real, non-blank
/// frame) and reachable through the CLI's `--desktop` attach, exactly the
/// path `run --desktop --keep` leaves behind for the later `shot`/`window`/
/// `assert` commands.
#[test]
#[ignore = "requires a built suite"]
fn an_instance_on_a_separate_desktop_is_driven_and_captured_but_never_shown() {
    let exe = launch::find_suite(None).unwrap();
    let desk = Desktop::create(&format!("uiharness-test-{}", std::process::id())).unwrap();
    let sandbox = sandbox("desktop");
    let app = launch::launch_on_desktop(&exe, &sandbox, &desk).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None)
        .unwrap_or_else(|e| panic!("the suite never became ready: {e}"));
    assert_eq!(driver.pid(), app.pid());

    // From this thread — still on the user's desktop — the window does not
    // exist: EnumWindows lists only the calling thread's desktop. That is
    // the side effect the feature exists for.
    assert!(
        uiharness::capture::window_of_pid(app.pid()).is_err(),
        "the suite's window must not be reachable from the user's desktop"
    );

    // Captures run on a thread attached to the desktop. A fresh thread owns
    // no windows or hooks, so SetThreadDesktop accepts it.
    let pid = app.pid();
    let capture = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                desk.attach_current_thread().unwrap();
                uiharness::capture::capture_pid(pid).unwrap()
            })
            .join()
            .unwrap()
    });
    assert!(
        !uiharness::capture::is_blank(&capture.image),
        "the desktop capture is a real frame, not a blank"
    );

    // Criterion 5 end to end: the CLI's own verbs, pointed at a kept
    // instance through --desktop, attach their thread and capture too.
    let png = sandbox.join("cli-window.png");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_uiharness"))
        .arg("--config")
        .arg(&sandbox)
        .arg("--desktop")
        .arg(desk.name())
        .arg("window")
        .arg("--out")
        .arg(&png)
        .output()
        .expect("run uiharness --desktop … window");
    assert!(
        out.status.success(),
        "uiharness --desktop window failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = std::fs::metadata(&png).expect("the CLI wrote cli-window.png");
    assert!(meta.len() > 0, "cli-window.png is empty");

    app.shutdown(Some(&driver));
}
