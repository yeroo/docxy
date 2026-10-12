//! Desktop-only check that the recent-files list survives a relaunch, that
//! restoring the session records nothing, and that a row whose file is gone
//! leaves the list instead of opening (#1194).
//! Run after building suite:
//! cargo test -p uiharness --test recent_files -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

fn open(driver: &Driver, path: &std::path::Path) {
    call(
        driver,
        "open",
        &[("path", Json::Str(path.display().to_string()))],
    );
}

/// The recent files' names, newest first.
fn names(driver: &Driver) -> Vec<String> {
    let reply = call(driver, "recent-files", &[]);
    reply
        .get("files")
        .and_then(Json::as_array)
        .unwrap_or_else(|| panic!("{reply}"))
        .iter()
        .filter_map(|f| f.get_str("name").map(str::to_owned))
        .collect()
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn the_recent_list_survives_a_relaunch_and_drops_a_missing_file() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/recent-files-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let a = sandbox.join("recent-a.docx");
    let b = sandbox.join("recent-b.xlsx");
    std::fs::copy(root.join("fixtures/basic.docx"), &a).unwrap();
    std::fs::copy(root.join("fixtures/basic.xlsx"), &b).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    // A, B, then A again: the list is [A, B] while the tabs are [A, B], so
    // a restore that recorded its tabs in order would leave [B, A].
    open(&driver, &a);
    open(&driver, &b);
    open(&driver, &a);
    assert_eq!(names(&driver), ["recent-a.docx", "recent-b.xlsx"]);
    app.shutdown(Some(&driver));
    drop(driver);

    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let tabs = call(&driver, "tab-list", &[]);
    // A harness instance starts with its own untitled sample tab.
    let titles: Vec<_> = tabs
        .get("tabs")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .filter_map(|t| t.get_str("title"))
        .filter(|t| t.starts_with("recent-"))
        .collect();
    assert_eq!(titles, ["recent-a.docx", "recent-b.xlsx"], "{tabs}");
    let b_tab = tabs
        .get("tabs")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .position(|t| t.get_str("title") == Some("recent-b.xlsx"))
        .unwrap();
    assert_eq!(
        names(&driver),
        ["recent-a.docx", "recent-b.xlsx"],
        "the list survives, and restoring the tabs recorded nothing"
    );

    // B's file is gone: its row leaves the list, and nothing opens.
    call(&driver, "close-tab", &[("index", Json::Num(b_tab as f64))]);
    std::fs::remove_file(&b).unwrap();
    call(
        &driver,
        "backstage",
        &[("action", Json::Str("open".into()))],
    );
    let before = call(&driver, "tab-list", &[]);
    let state = call(&driver, "recent-open", &[("index", Json::Num(1.))]);
    assert_eq!(
        state.get_str("status"),
        Some("\"recent-b.xlsx\" could not be found, so it was removed from the recent list"),
        "{state}"
    );
    assert_eq!(names(&driver), ["recent-a.docx"]);
    assert_eq!(call(&driver, "tab-list", &[]), before, "nothing opened");
    let backstage = call(
        &driver,
        "backstage",
        &[("action", Json::Str("read".into()))],
    );
    assert_eq!(
        backstage.get("open"),
        Some(&Json::Bool(true)),
        "{backstage}"
    );
    app.shutdown(Some(&driver));
}
