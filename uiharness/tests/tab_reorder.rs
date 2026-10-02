//! Desktop-only: drag-reorder persists the new tab order to session.json
//! with no other action (#545). Run after building suite:
//! cargo test -p uiharness --test tab_reorder -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

fn session_tabs(sandbox: &Path) -> Vec<Json> {
    let text = std::fs::read_to_string(sandbox.join("docxy/session.json")).unwrap();
    Json::parse(&text)
        .unwrap()
        .get("tabs")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec()
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn tab_reorder_persists_new_order_in_session() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/tab-reorder-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    let exe = launch::find_suite(None).unwrap();
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    for name in ["tab-01.docx", "tab-02.docx", "tab-03.docx"] {
        let copy = sandbox.join(name);
        std::fs::copy(root.join("fixtures/basic.docx"), &copy).unwrap();
        call(
            &driver,
            "open",
            &[("path", Json::Str(copy.display().to_string()))],
        );
    }
    // A fresh instance opens a welcome sample tab; close it so the three
    // copies alone occupy the strip.
    call(&driver, "close-tab", &[("index", Json::Num(0.))]);
    call(
        &driver,
        "window-size",
        &[("w", Json::Num(1000.)), ("h", Json::Num(700.))],
    );
    // Pointer verbs hit-test against the last rendered frame; settle one so
    // the chip probes exist before the drag resolves them.
    driver.settle("title-tabs").unwrap();
    // Drag chip 0 onto chip 2: [tab-01, tab-02, tab-03] becomes
    // [tab-02, tab-03, tab-01]. The dragged tab was not active, so the active
    // one (tab-03) follows its own shift to index 1.
    call(
        &driver,
        "pointer-drag",
        &[
            ("from", Json::Str("tab-chip:0".into())),
            ("to", Json::Str("tab-chip:2".into())),
        ],
    );
    let live = call(&driver, "tab-list", &[]);
    assert_eq!(live.get_usize("active"), Some(1));
    let titles: Vec<&str> = live
        .get("tabs")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.get_str("title").unwrap())
        .collect();
    assert_eq!(titles, ["tab-02.docx", "tab-03.docx", "tab-01.docx"]);
    // The side effect under test: the reorder alone reached the session file.
    let session = session_tabs(&sandbox);
    let persisted: Vec<&str> = session
        .iter()
        .map(|t| t.get_str("title").unwrap())
        .collect();
    assert_eq!(
        persisted,
        ["tab-02.docx", "tab-03.docx", "tab-01.docx"],
        "reorder must persist without any other action"
    );
    app.shutdown(Some(&driver));
}
