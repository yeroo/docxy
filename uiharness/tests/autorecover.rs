//! Desktop-only check of AutoRecover (#632): an unsaved edit that an
//! AutoRecover tick wrote survives a kill, and the relaunch offers it as a
//! recovered, unsaved copy without touching the original file, behind
//! Document Recovery's choice of the copy or the original (#1225).
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
    // #1225: Document Recovery opens first, the recovered copy checked;
    // Cancel keeps it, as OK with it checked would.
    assert_recovery_offered(&driver);
    call(
        &driver,
        "dialog-click",
        &[("button", Json::Str("Cancel".into()))],
    );
    let state = call(&driver, "tab-select", &[("tab", Json::Str("basic".into()))]);
    assert_eq!(state.get("dirty"), Some(&Json::Bool(true)), "{state}");
    let status = state.get_str("status").unwrap_or_default();
    assert!(status.starts_with("recovered"), "{state}");
    // AutoCorrect (#667) capitalises the sentence's first letter.
    let doc = call(&driver, "doc", &[]);
    assert!(
        doc.get_str("text")
            .is_some_and(|t| t.to_lowercase().contains("recover me")),
        "{doc}"
    );
    assert_eq!(
        std::fs::read(&original).unwrap(),
        before,
        "the original is untouched"
    );

    // Crash again with the copy still unsaved, and relaunch as a double
    // click on another file does: the dialog opens after that file, over
    // every tab, and nothing reaches a tab under it (#1225 r1 M1). This time
    // uncheck it: OK reopens the original as last saved.
    let tick = call(&driver, "autorecover-now", &[]);
    assert_eq!(tick.get("wrote"), Some(&Json::Bool(true)), "{tick}");
    drop(driver);
    drop(app);
    let other = sandbox.join("other.docx");
    std::fs::copy(root.join("fixtures/basic.docx"), &other).unwrap();
    let sandbox_abs = std::path::absolute(&sandbox).unwrap();
    let child = launch::command(&exe, &sandbox_abs, std::env::vars_os())
        .arg(&other)
        .spawn()
        .unwrap();
    let app = uiharness::launch::Launched::adopt(child, &exe, &sandbox_abs);
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    assert_recovery_offered(&driver);
    let list = call(&driver, "tab-list", &[]);
    let (Some(Json::Num(active)), Some(Json::Arr(tabs))) = (list.get("active"), list.get("tabs"))
    else {
        panic!("{list}")
    };
    assert_eq!(
        tabs[*active as usize].get_str("title"),
        Some("other.docx"),
        "the startup file opened under the dialog: {list}"
    );
    let refused = driver.call(
        "tab-select",
        Json::obj(vec![("tab", Json::Str("basic".into()))]),
    );
    assert!(refused.is_err(), "{refused:?}");
    call(
        &driver,
        "dialog-set",
        &[
            ("control", Json::Str("documents".into())),
            ("value", Json::Bool(false)),
        ],
    );
    let ok = call(
        &driver,
        "dialog-click",
        &[("button", Json::Str("OK".into()))],
    );
    assert_eq!(
        ok.get("dialog").and_then(|d| d.get("open")),
        Some(&Json::Bool(false)),
        "{ok}"
    );
    let state = call(&driver, "tab-select", &[("tab", Json::Str("basic".into()))]);
    assert_eq!(state.get("dirty"), Some(&Json::Bool(false)), "{state}");
    let status = state.get_str("status").unwrap_or_default();
    assert!(status.starts_with("original opened"), "{state}");
    let doc = call(&driver, "doc", &[]);
    assert!(
        doc.get_str("text")
            .is_some_and(|t| !t.to_lowercase().contains("recover me")),
        "{doc}"
    );
    assert_eq!(std::fs::read(&original).unwrap(), before);
    app.shutdown(Some(&driver));
}

/// The relaunch after a crash shows Document Recovery over one recovered
/// document, checked.
fn assert_recovery_offered(driver: &Driver) {
    let d = call(driver, "dialog-read", &[]);
    assert_eq!(d.get_str("id"), Some("document-recovery"), "{d}");
    let Some(Json::Arr(controls)) = d.get("controls") else {
        panic!("{d}")
    };
    assert_eq!(
        controls[0].get("value"),
        Some(&Json::Arr(vec![Json::Bool(true)])),
        "{d}"
    );
}

/// #613: Excel's "Keep the last AutoRecovered version if I close without
/// saving". A workbook typed into (the cell committed), an AutoRecover tick,
/// then Don't Save: one draft is listed and opens read-only with the text.
/// The harness has no blank-workbook verb, so the workbook is a fixture copy;
/// a draft is the same either way.
#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn dont_save_keeps_a_draft_that_opens_read_only() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/autorecover-tests")
            .join(format!("drafts-{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let book = sandbox.join("basic.xlsx");
    std::fs::copy(root.join("fixtures/basic.xlsx"), &book).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let on = call(&driver, "selection", &[]);
    assert_eq!(on.get("keep_drafts"), Some(&Json::Bool(true)), "{on}");
    let drafts = call(&driver, "drafts", &[]);
    assert_eq!(drafts.get("drafts"), Some(&Json::Arr(vec![])), "{drafts}");
    call(&driver, "autorecover", &[("minutes", Json::Num(1.))]);
    call(
        &driver,
        "open",
        &[("path", Json::Str(book.display().to_string()))],
    );
    call(&driver, "click-cell", &[("cell", Json::Str("A1".into()))]);
    call(&driver, "type", &[("text", Json::Str("draft me".into()))]);
    let typed = call(&driver, "key", &[("key", Json::Str("enter".into()))]);
    assert_eq!(typed.get("dirty"), Some(&Json::Bool(true)), "{typed}");
    let tick = call(&driver, "autorecover-now", &[]);
    assert_eq!(tick.get("wrote"), Some(&Json::Bool(true)), "{tick}");
    call(
        &driver,
        "close-tab",
        &[("answer", Json::Str("discard".into()))],
    );

    let drafts = call(&driver, "drafts", &[]);
    let Some(Json::Arr(list)) = drafts.get("drafts") else {
        panic!("{drafts}")
    };
    assert_eq!(list.len(), 1, "{drafts}");
    assert!(
        list[0]
            .get_str("name")
            .is_some_and(|n| n.starts_with("basic ((Unsaved-")),
        "{drafts}"
    );
    let opened = call(&driver, "open-draft", &[("index", Json::Num(0.))]);
    assert_eq!(opened.get("read_only"), Some(&Json::Bool(true)), "{opened}");
    let cell = call(&driver, "cell", &[("cell", Json::Str("A1".into()))]);
    assert_eq!(cell.get_str("value"), Some("draft me"), "{cell}");
    app.shutdown(Some(&driver));
}
