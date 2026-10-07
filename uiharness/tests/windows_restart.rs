//! Desktop-only check that two windows' tabs reach the one session.json
//! with distinct hot sidecars, and a relaunch restores them all into one
//! window (#587). Run after building suite:
//! cargo test -p uiharness --test windows_restart -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn two_windows_restore_into_one_on_relaunch() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/windows-restart-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let doc = sandbox.join("basic.docx");
    std::fs::copy(root.join("fixtures/basic.docx"), &doc).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    call(
        &driver,
        "open",
        &[("path", Json::Str(doc.display().to_string()))],
    );
    // Two tabs on window 1; New Window moves the active one (basic.docx)
    // to a second window.
    call(&driver, "window-new", &[]);
    let listed = call(&driver, "window-list", &[]);
    assert_eq!(listed.get("count"), Some(&Json::Num(2.)), "{listed}");
    // An unsaved edit in each window.
    call(&driver, "window-select", &[("window", Json::Num(1.))]);
    call(&driver, "type", &[("text", Json::Str("one".into()))]);
    call(&driver, "window-select", &[("window", Json::Num(2.))]);
    call(&driver, "type", &[("text", Json::Str("two".into()))]);
    // Persist both windows' unions, as each window's own AutoRecover would.
    call(&driver, "window-select", &[("window", Json::Num(1.))]);
    let wrote = call(&driver, "autorecover-now", &[]);
    assert_eq!(wrote.get("wrote"), Some(&Json::Bool(true)), "{wrote}");
    call(&driver, "window-select", &[("window", Json::Num(2.))]);
    let wrote = call(&driver, "autorecover-now", &[]);
    assert_eq!(wrote.get("wrote"), Some(&Json::Bool(true)), "{wrote}");

    // The one session holds BOTH windows' tabs, under distinct sidecar names
    // (the second window's registry seq offsets its hot index base).
    let session =
        Json::parse(&std::fs::read_to_string(sandbox.join("docxy/session.json")).unwrap()).unwrap();
    let tabs = session.get("tabs").and_then(Json::as_array).unwrap();
    assert_eq!(tabs.len(), 2, "{session}");
    let hot = |title: &str| -> String {
        tabs.iter()
            .find(|t| t.get_str("title") == Some(title))
            .and_then(|t| t.get_str("hot"))
            .unwrap_or_else(|| panic!("{title} in the session: {session}"))
            .to_string()
    };
    let hot1 = hot("sample.docx");
    let hot2 = hot("basic.docx");
    assert!(hot1.ends_with("tab-0.docx"), "{hot1}");
    assert!(hot2.ends_with("tab-1000.docx"), "{hot2}");
    assert!(PathBuf::from(&hot1).exists(), "{hot1}");
    assert!(PathBuf::from(&hot2).exists(), "{hot2}");

    // `shutdown` sends `quit` and waits for the process to exit.
    app.shutdown(Some(&driver));
    drop(driver);

    // A restart restores every window's tabs into the one window. The
    // session lists the last-persisting window's own tabs first (each
    // write appends the others' snapshots), and `quit` persisted from
    // window 2, so basic.docx leads.
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let state = call(&driver, "selection", &[]);
    assert_eq!(state.get("windows"), Some(&Json::Num(1.)), "{state}");
    let tabs = call(&driver, "tab-list", &[]);
    let titles: Vec<_> = tabs
        .get("tabs")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .filter_map(|t| t.get_str("title"))
        .collect();
    assert_eq!(titles, ["basic.docx", "sample.docx"], "{tabs}");
    app.shutdown(Some(&driver));
}
