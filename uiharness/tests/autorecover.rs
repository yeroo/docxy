//! Desktop-only check of AutoRecover (#632): an unsaved edit that an
//! AutoRecover tick wrote survives a kill, and the relaunch offers it as a
//! recovered, unsaved copy without touching the original file.
//! Run after building suite:
//! cargo test -p uiharness --test autorecover -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn a_killed_instance_relaunches_with_the_autorecover_copy() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/autorecover-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let original = sandbox.join("basic.docx");
    std::fs::copy(root.join("fixtures/basic.docx"), &original).unwrap();
    let before = std::fs::read(&original).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let on = call(&driver, "selection", &[]);
    assert_eq!(on.get("autorecover_minutes"), Some(&Json::Num(10.)), "{on}");
    call(
        &driver,
        "open",
        &[("path", Json::Str(original.display().to_string()))],
    );
    call(
        &driver,
        "key",
        &[("keys", Json::Arr(vec![Json::Str("ctrl+end".into())]))],
    );
    let typed = call(&driver, "type", &[("text", Json::Str("recover me".into()))]);
    assert_eq!(typed.get("dirty"), Some(&Json::Bool(true)), "{typed}");
    let tick = call(&driver, "autorecover-now", &[]);
    assert_eq!(tick.get("wrote"), Some(&Json::Bool(true)), "{tick}");

    // Kill it: `Launched`'s drop kills a live child, so no `quit` runs and
    // nothing gets a chance to persist or clear the run marker.
    let ctl_dir = app.ctl_dir();
    let stale: Vec<_> = std::fs::read_dir(&ctl_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    drop(driver);
    drop(app);
    // The issue's "also observed": the dead instance's discovery file stays
    // behind. Keep one there even if the kill somehow removed it, plus one
    // under another name, so the relaunch has to come up past both.
    for p in &stale {
        if !p.exists() {
            std::fs::write(p, r#"{"instance":"suite-0","port":1,"token":"x","pid":1}"#).unwrap();
        }
    }
    std::fs::write(
        ctl_dir.join("suite-424242.json"),
        r#"{"instance":"suite-424242","port":1,"token":"x","pid":424242}"#,
    )
    .unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let state = call(&driver, "tab-select", &[("tab", Json::Str("basic".into()))]);
    assert_eq!(state.get("dirty"), Some(&Json::Bool(true)), "{state}");
    let status = state.get_str("status").unwrap_or_default();
    assert!(status.starts_with("recovered"), "{state}");
    let doc = call(&driver, "doc", &[]);
    assert!(
        doc.get_str("text")
            .is_some_and(|t| t.contains("recover me")),
        "{doc}"
    );
    assert_eq!(
        std::fs::read(&original).unwrap(),
        before,
        "the original is untouched"
    );
    app.shutdown(Some(&driver));
}
