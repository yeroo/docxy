//! Desktop-only check of Protected View (#610) against a real downloaded
//! file: a workbook carrying a `Zone.Identifier` stream with `ZoneId=3`
//! opens protected, every way of editing or saving it through the app's own
//! entry points changes nothing, and Enable Editing lets an edit stick.
//! A `.uit` script cannot write an alternate data stream, so this is Rust.
//! Run after building suite:
//! cargo test -p uiharness --test protected_view -- --ignored --nocapture
#![cfg(windows)]

use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use uiharness::{Driver, Run, launch};

fn call(driver: &Driver, verb: &str, args: Vec<(&str, Json)>) -> Result<Json, String> {
    driver.call(verb, Json::obj(args))
}

fn ok(driver: &Driver, verb: &str, args: Vec<(&str, Json)>) -> Json {
    call(driver, verb, args).unwrap_or_else(|e| panic!("{verb}: {e}"))
}

fn s(text: &str) -> Json {
    Json::Str(text.into())
}

fn active_tab(driver: &Driver) -> Json {
    let list = ok(driver, "tab-list", vec![]);
    let active = list.get("active").and_then(Json::as_f64).unwrap() as usize;
    list.get("tabs").and_then(Json::as_array).unwrap()[active].clone()
}

fn cell(driver: &Driver, at: &str) -> String {
    ok(driver, "cell", vec![("cell", s(at))])
        .get("value")
        .and_then(Json::as_str)
        .unwrap()
        .to_string()
}

fn flag(tab: &Json, key: &str) -> bool {
    tab.get(key) == Some(&Json::Bool(true))
}

/// Mark `path` as downloaded from the Internet; `false` when this volume
/// keeps no alternate data streams.
fn mark_downloaded(path: &Path) -> bool {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    match std::fs::write(
        PathBuf::from(stream),
        "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/book.xlsx\r\n",
    ) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("SKIP protected_view: no alternate data streams here ({e})");
            false
        }
    }
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn a_downloaded_workbook_opens_protected_until_enable_editing() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/protected-view-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    let files = run.dir().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let book = files.join("book.xlsx");
    std::fs::copy(root.join("fixtures/basic.xlsx"), &book).unwrap();
    if !mark_downloaded(&book) {
        return;
    }
    let original = std::fs::read(&book).unwrap();
    eprintln!("ADS written: {}:Zone.Identifier", book.display());

    let exe = launch::find_suite(None).unwrap();
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();

    ok(
        &driver,
        "open",
        vec![("path", s(&book.display().to_string()))],
    );
    let tab = active_tab(&driver);
    assert!(flag(&tab, "protected"), "{tab}");
    assert_eq!(
        tab.get("caption").and_then(Json::as_str),
        Some("book.xlsx [Protected View]")
    );
    let before: Vec<String> = ["B2", "B3", "B4", "C2"]
        .iter()
        .map(|c| cell(&driver, c))
        .collect();
    let protected_status = "Protected View — select Enable Editing to edit";

    // Looking still works: select, move, extend, copy.
    let st = ok(&driver, "click-cell", vec![("cell", s("B2"))]);
    assert_eq!(st.get("sel").and_then(Json::as_str), Some("B2"), "{st}");
    let st = ok(&driver, "key", vec![("keys", Json::Arr(vec![s("down")]))]);
    assert_eq!(st.get("sel").and_then(Json::as_str), Some("B3"), "{st}");
    let st = ok(
        &driver,
        "key",
        vec![("keys", Json::Arr(vec![s("shift+down"), s("ctrl+c")]))],
    );
    assert_ne!(
        st.get("status").and_then(Json::as_str),
        Some(protected_status),
        "{st}"
    );

    // Ctrl+Shift+U expands the formula bar (#672): view state, so Protected
    // View does not refuse it.
    let st = ok(
        &driver,
        "key",
        vec![("keys", Json::Arr(vec![s("ctrl+shift+u")]))],
    );
    assert_eq!(st.get("fx_expanded"), Some(&Json::Bool(true)), "{st}");
    assert_ne!(
        st.get("status").and_then(Json::as_str),
        Some(protected_status),
        "{st}"
    );
    assert_eq!(st.get("dirty"), Some(&Json::Bool(false)), "{st}");
    let st = ok(
        &driver,
        "key",
        vec![("keys", Json::Arr(vec![s("ctrl+shift+u")]))],
    );
    assert_eq!(st.get("fx_expanded"), Some(&Json::Bool(false)), "{st}");

    // Every way of editing is refused and leaves the cells and the tab alone.
    ok(&driver, "click-cell", vec![("cell", s("B2"))]);
    let st = ok(&driver, "type", vec![("text", s("99"))]);
    assert_eq!(
        st.get("status").and_then(Json::as_str),
        Some(protected_status),
        "{st}"
    );
    assert_eq!(st.get("editing"), Some(&Json::Bool(false)), "{st}");
    for keys in [
        vec!["enter"],
        vec!["delete"],
        vec!["backspace"],
        vec!["f2"],
        vec!["ctrl+b"],
        vec!["ctrl+x"],
        vec!["ctrl+v"],
        vec!["ctrl+d"],
        vec!["ctrl+z"],
    ] {
        let st = ok(
            &driver,
            "key",
            vec![("keys", Json::Arr(keys.iter().map(|k| s(k)).collect()))],
        );
        assert_eq!(st.get("dirty"), Some(&Json::Bool(false)), "{keys:?}: {st}");
        assert_eq!(
            st.get("editing"),
            Some(&Json::Bool(false)),
            "{keys:?}: {st}"
        );
    }
    let st = ok(
        &driver,
        "click-cell",
        vec![("cell", s("C2")), ("double", Json::Bool(true))],
    );
    assert_eq!(
        st.get("editing"),
        Some(&Json::Bool(false)),
        "double-click: {st}"
    );
    for command in ["Bold", "Italic", "Freeze Panes"] {
        let st = ok(
            &driver,
            "ribbon-click",
            vec![
                (
                    "tab",
                    s(if command == "Freeze Panes" {
                        "View"
                    } else {
                        "Home"
                    }),
                ),
                ("command", s(command)),
            ],
        );
        assert_eq!(
            st.get("status").and_then(Json::as_str),
            Some(protected_status),
            "{command}: {st}"
        );
        assert_eq!(st.get("dirty"), Some(&Json::Bool(false)), "{command}: {st}");
    }
    // The fill handle does not arm.
    let fill = call(
        &driver,
        "fill-drag",
        vec![("from", s("B2:B3")), ("to", s("B5"))],
    );
    assert_eq!(
        fill.unwrap_err(),
        format!("the fill did not arm: {protected_status}"),
        "the refusal says why"
    );
    // #610 r2: a bar that acts on Enter, left open on another tab, does not
    // follow into the protected one. Open an ordinary workbook, open its
    // AutoFilter bar and type a criterion, then come back and press Enter.
    let local = files.join("local.xlsx");
    std::fs::copy(root.join("fixtures/basic.xlsx"), &local).unwrap();
    ok(
        &driver,
        "open",
        vec![("path", s(&local.display().to_string()))],
    );
    assert!(!flag(&active_tab(&driver), "protected"));
    ok(
        &driver,
        "ribbon-click",
        vec![("tab", s("Home")), ("command", s("Filter"))],
    );
    ok(&driver, "type", vec![("text", s("North"))]);
    let back = ok(&driver, "tab-select", vec![("tab", s("book.xlsx"))]);
    assert_eq!(back.get("protected"), Some(&Json::Bool(true)), "{back}");
    let st = ok(&driver, "key", vec![("keys", Json::Arr(vec![s("enter")]))]);
    assert_eq!(st.get("dirty"), Some(&Json::Bool(false)), "{st}");
    // Enter moved the selection, as on any sheet: no filter bar was there to
    // take it (a carried one swallows Enter and the selection stays put).
    assert_ne!(
        st.get("sel"),
        back.get("sel"),
        "the other tab's filter bar followed: {st}"
    );
    let after: Vec<String> = ["B2", "B3", "B4", "C2"]
        .iter()
        .map(|c| cell(&driver, c))
        .collect();
    assert_eq!(after, before, "the cells did not change");
    assert!(!flag(&active_tab(&driver), "dirty"));

    // Nothing is saved, Save As included.
    let st = ok(&driver, "key", vec![("keys", Json::Arr(vec![s("ctrl+s")]))]);
    assert_eq!(
        st.get("status").and_then(Json::as_str),
        Some(protected_status),
        "{st}"
    );
    let out = files.join("out.xlsx");
    let refused = call(
        &driver,
        "save-as",
        vec![("path", s(&out.display().to_string()))],
    );
    assert_eq!(refused.unwrap_err(), protected_status);
    let over = call(
        &driver,
        "save-as",
        vec![
            ("path", s(&book.display().to_string())),
            ("overwrite", Json::Bool(true)),
        ],
    );
    assert_eq!(over.unwrap_err(), protected_status);
    assert!(!out.exists());
    assert_eq!(
        std::fs::read(&book).unwrap(),
        original,
        "the downloaded file is untouched"
    );

    // Open as Copy is no way around it.
    ok(
        &driver,
        "open",
        vec![
            ("path", s(&book.display().to_string())),
            ("mode", s("copy")),
        ],
    );
    let copy = active_tab(&driver);
    assert_eq!(
        copy.get("title").and_then(Json::as_str),
        Some("Copy (1)book.xlsx")
    );
    assert!(flag(&copy, "protected"), "{copy}");
    // The copy is still downloaded (#610 r5): opened again from disk, as
    // the backstage or a double-click would, it is protected on its own.
    let copy_path = files.join("Copy (1)book.xlsx");
    ok(
        &driver,
        "open",
        vec![("path", s(&copy_path.display().to_string()))],
    );
    let reopened = active_tab(&driver);
    assert!(flag(&reopened, "protected"), "{reopened}");

    // Enable Editing: an edit now sticks, and Save writes the copy.
    let st = ok(&driver, "enable-editing", vec![]);
    assert_eq!(st.get("protected"), Some(&Json::Bool(false)), "{st}");
    ok(&driver, "click-cell", vec![("cell", s("B2"))]);
    ok(&driver, "type", vec![("text", s("99"))]);
    let st = ok(&driver, "key", vec![("keys", Json::Arr(vec![s("enter")]))]);
    assert_eq!(st.get("dirty"), Some(&Json::Bool(true)), "{st}");
    assert_eq!(cell(&driver, "B2"), "99");
    let st = ok(&driver, "key", vec![("keys", Json::Arr(vec![s("ctrl+s")]))]);
    assert_eq!(st.get("dirty"), Some(&Json::Bool(false)), "{st}");
    assert_eq!(
        std::fs::read(&book).unwrap(),
        original,
        "the source is still untouched"
    );
    let _ = ok(&driver, "quit", vec![]);
}

/// Trusted documents (#882): Enable Editing remembers the file, so opening it
/// again skips Protected View, until a different file is downloaded to the
/// same path.
#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn enable_editing_trusts_the_file_until_it_is_replaced() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/protected-view-tests")
            .join(format!("trusted-{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    let files = run.dir().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let book = files.join("book.xlsx");
    std::fs::copy(root.join("fixtures/basic.xlsx"), &book).unwrap();
    if !mark_downloaded(&book) {
        return;
    }

    let exe = launch::find_suite(None).unwrap();
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let open = |driver: &Driver| {
        ok(
            driver,
            "open",
            vec![("path", s(&book.display().to_string()))],
        );
        active_tab(driver)
    };

    assert!(flag(&open(&driver), "protected"));
    let st = ok(&driver, "enable-editing", vec![]);
    assert_eq!(st.get("protected"), Some(&Json::Bool(false)), "{st}");
    assert_eq!(
        st.get("status").and_then(Json::as_str),
        Some("editing enabled"),
        "{st}"
    );
    assert!(
        sandbox.join("docxy").join("trusted.json").is_file(),
        "the record lives in the sandbox, not the real profile"
    );

    // Closed and opened again: trusted, so not protected.
    ok(&driver, "close-tab", vec![]);
    let again = open(&driver);
    assert!(!flag(&again, "protected"), "{again}");

    // Clearing the trusted documents list (#895): the file opens protected
    // again. The tab stays open after this: the replace step's own
    // close-tab closes it (a second close with no tab open is an error).
    ok(&driver, "close-tab", vec![]);
    let cleared = ok(&driver, "trusted-clear", vec![]);
    assert_eq!(cleared.get("cleared"), Some(&Json::Num(1.0)), "{cleared}");
    assert!(flag(&open(&driver), "protected"));
    let st = ok(&driver, "enable-editing", vec![]);
    assert_eq!(st.get("protected"), Some(&Json::Bool(false)), "{st}");
    assert_eq!(
        st.get("status").and_then(Json::as_str),
        Some("editing enabled"),
        "{st}"
    );

    // Downloaded again to the same path: another file, protected again.
    ok(&driver, "close-tab", vec![]);
    std::fs::copy(root.join("fixtures/chart-kinds.xlsx"), &book).unwrap();
    assert!(mark_downloaded(&book));
    let replaced = open(&driver);
    assert!(flag(&replaced, "protected"), "{replaced}");
    let _ = ok(&driver, "quit", vec![]);
}
