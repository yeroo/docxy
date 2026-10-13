//! Desktop-only check of the real window close (#1226): the OS closes the
//! suite's window as its X does, gpui runs the suite's should-close handler,
//! and that handler writes the session once and only then clears the
//! AutoRecover run marker. The harness's `harness-writes.log` is how the
//! test sees the writes of a process that has gone.
//! Run after building suite (Windows, or Linux through scripts/ui-linux.py):
//! cargo test -p uiharness --test window_close -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uiharness::launch::Launched;
use uiharness::{Driver, Run, launch, window};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

/// A fresh sandbox holding a copy of `fixtures/basic.xlsx`.
fn sandbox(name: &str) -> (Run, PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/window-close-tests")
            .join(format!("{name}-{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let book = sandbox.join("basic.xlsx");
    std::fs::copy(root.join("fixtures/basic.xlsx"), &book).unwrap();
    (run, sandbox, book)
}

fn marker(sandbox: &Path) -> PathBuf {
    sandbox.join("docxy").join("running")
}

/// The harness write log's lines; none before the first recorded write.
fn log_lines(sandbox: &Path) -> Vec<String> {
    std::fs::read_to_string(sandbox.join("docxy").join("harness-writes.log"))
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Close the window through the OS and wait for the process to end by
/// itself. The window may still be coming up, so a missing one is retried.
fn close_and_wait(app: &mut Launched) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match window::request_close(app.pid()) {
            Ok(()) => break,
            Err(e) if Instant::now() >= deadline => panic!("request_close: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    // A ceiling, not an expectation: a debug build is slow to wind down.
    app.wait_exit(Duration::from_secs(30))
        .unwrap_or_else(|e| panic!("the OS close did not end the run: {e}"))
}

/// The writes the close added: everything logged after `before` lines.
fn added(sandbox: &Path, before: usize) -> Vec<String> {
    log_lines(sandbox).split_off(before)
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn os_close_persists_once_then_clears_the_marker() {
    if cfg!(target_os = "macos") {
        eprintln!("skipped: request_close is not supported on macOS");
        return;
    }
    let (_run, sandbox, book) = sandbox("persist-once");
    let exe = launch::find_suite(None).unwrap();

    let mut app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    call(
        &driver,
        "open",
        &[("path", Json::Str(book.display().to_string()))],
    );
    call(&driver, "click-cell", &[("cell", Json::Str("A1".into()))]);
    let state = call(
        &driver,
        "type",
        &[("text", Json::Str("OsClosePendingMarker".into()))],
    );
    // Still in the cell editor, never committed: the close must fold it in.
    assert_eq!(state.get("editing"), Some(&Json::Bool(true)), "{state}");
    assert_eq!(state.get("dirty"), Some(&Json::Bool(false)), "{state}");
    assert!(marker(&sandbox).exists(), "a running instance is marked");
    let before = log_lines(&sandbox).len();
    drop(driver);

    let status = close_and_wait(&mut app);
    assert!(status.success(), "the close ended the run with {status}");
    assert!(
        !marker(&sandbox).exists(),
        "an accepted close clears the run marker"
    );
    assert_eq!(
        added(&sandbox, before),
        ["persist All", "clear-running"],
        "one session write, then the marker clear"
    );
    drop(app);

    // The one write carried the pending edit.
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let state = call(&driver, "selection", &[]);
    assert_eq!(state.get_str("title"), Some("basic.xlsx"), "{state}");
    assert_eq!(state.get("dirty"), Some(&Json::Bool(true)), "{state}");
    let value = call(&driver, "cell", &[("cell", Json::Str("A1".into()))]);
    assert_eq!(
        value.get_str("value"),
        Some("OsClosePendingMarker"),
        "{value}"
    );
    app.shutdown(Some(&driver));
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn os_close_never_asks_under_the_harness() {
    if cfg!(target_os = "macos") {
        eprintln!("skipped: request_close is not supported on macOS");
        return;
    }
    let (_run, sandbox, book) = sandbox("never-asks");
    let exe = launch::find_suite(None).unwrap();

    let mut app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    call(&driver, "ask-on-close", &[("on", Json::Bool(true))]);
    call(
        &driver,
        "open",
        &[("path", Json::Str(book.display().to_string()))],
    );
    call(&driver, "click-cell", &[("cell", Json::Str("A1".into()))]);
    call(
        &driver,
        "type",
        &[("text", Json::Str("OsCloseDirtyMarker".into()))],
    );
    let state = call(&driver, "key", &[("key", Json::Str("enter".into()))]);
    assert_eq!(state.get("dirty"), Some(&Json::Bool(true)), "{state}");
    let state = call(&driver, "selection", &[]);
    assert_eq!(
        state.get("ask_on_close"),
        Some(&Json::Bool(true)),
        "{state}"
    );
    let before = log_lines(&sandbox).len();
    drop(driver);

    // The last window's close under the harness is `CloseAsk::HarnessQuit`:
    // a dirty tab with "ask before closing" on still goes without a question.
    let status = close_and_wait(&mut app);
    assert!(status.success(), "the close ended the run with {status}");
    assert!(!marker(&sandbox).exists(), "the run marker is cleared");
    assert_eq!(
        added(&sandbox, before),
        ["persist All", "clear-running"],
        "one session write, then the marker clear"
    );
}
