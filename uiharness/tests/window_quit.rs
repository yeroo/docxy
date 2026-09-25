//! Desktop-only check that `quit` keeps an uncommitted cell edit across a
//! relaunch. Run after building suite:
//! cargo test -p uiharness --test window_quit -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn quit_keeps_a_pending_cell_edit_across_relaunch() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/window-quit-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let book = sandbox.join("basic.xlsx");
    std::fs::copy(root.join("fixtures/basic.xlsx"), &book).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
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
        &[("text", Json::Str("QuitPendingMarker".into()))],
    );
    // Still in the cell editor, never committed: exactly what quit must fold in.
    assert_eq!(state.get("editing"), Some(&Json::Bool(true)), "{state}");
    assert_eq!(state.get("dirty"), Some(&Json::Bool(false)), "{state}");
    // `shutdown` sends `quit` and waits for the process to exit.
    app.shutdown(Some(&driver));
    drop(driver);

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let state = call(&driver, "selection", &[]);
    assert_eq!(state.get_str("title"), Some("basic.xlsx"), "{state}");
    assert_eq!(state.get("dirty"), Some(&Json::Bool(true)), "{state}");
    let value = call(&driver, "cell", &[("cell", Json::Str("A1".into()))]);
    assert_eq!(value.get_str("value"), Some("QuitPendingMarker"), "{value}");
    // The fixture itself was never saved over.
    assert_eq!(
        std::fs::read(&book).unwrap(),
        std::fs::read(root.join("fixtures/basic.xlsx")).unwrap()
    );
    app.shutdown(Some(&driver));
}
