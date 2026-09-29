//! Desktop-only check that a relaunch restores the tabs `quit` left: two
//! plans and a blank one, one of them dirty, and which one was active (#396).
//! Run after building suite:
//! cargo test -p uiharness --test tab_restart -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn relaunch_restores_the_tab_list_quit_left() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/tab-restart-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let mut plans = Vec::new();
    for name in ["gantt-summary.xml", "project-rows.xml"] {
        let plan = sandbox.join(name);
        std::fs::copy(root.join("fixtures").join(name), &plan).unwrap();
        plans.push(plan);
    }
    let exe = launch::find_suite(None).unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    for plan in &plans {
        call(
            &driver,
            "proj.open",
            &[("path", Json::Str(plan.display().to_string()))],
        );
    }
    let blank = call(&driver, "proj.new", &[]);
    assert_eq!(blank.get("path"), Some(&Json::Null), "{blank}");
    call(
        &driver,
        "tab-select",
        &[("tab", Json::Str("gantt-summary".into()))],
    );
    call(
        &driver,
        "task.set",
        &[
            ("uid", Json::Num(2.)),
            ("name", Json::Str("Restart".into())),
        ],
    );
    // Leave a different tab active than the one just edited.
    call(
        &driver,
        "tab-select",
        &[("tab", Json::Str("project-rows".into()))],
    );
    let before = call(&driver, "tab-list", &[]);
    let tabs = before.get("tabs").and_then(Json::as_array).unwrap();
    let dirty: Vec<_> = tabs
        .iter()
        .filter(|t| t.get("dirty") == Some(&Json::Bool(true)))
        .filter_map(|t| t.get_str("title"))
        .collect();
    assert_eq!(dirty, ["gantt-summary.xml"], "{before}");
    assert!(
        tabs.iter()
            .any(|t| t.get_str("title") == Some("Untitled.yppx")
                && t.get("path") == Some(&Json::Null)),
        "{before}"
    );
    // `shutdown` sends `quit` and waits for the process to exit.
    app.shutdown(Some(&driver));
    drop(driver);

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let after = call(&driver, "tab-list", &[]);
    assert_eq!(after, before, "restored\n{after}\nquit with\n{before}");
    let state = call(&driver, "selection", &[]);
    assert_eq!(state.get_str("title"), Some("project-rows.xml"), "{state}");
    let task = call(
        &driver,
        "task.get",
        &[
            ("tab", Json::Str("gantt-summary".into())),
            ("uid", Json::Num(2.)),
        ],
    );
    assert_eq!(task.get_str("name"), Some("Restart"), "{task}");
    // The unsaved edit lives in the session, not in the file.
    assert_eq!(
        std::fs::read(&plans[0]).unwrap(),
        std::fs::read(root.join("fixtures/gantt-summary.xml")).unwrap()
    );
    app.shutdown(Some(&driver));
}
