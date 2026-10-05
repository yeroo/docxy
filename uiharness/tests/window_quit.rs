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

/// Whether `docx`'s main document part holds `text`.
fn docx_has(docx: &std::path::Path, text: &str) -> bool {
    let bytes = std::fs::read(docx).unwrap();
    let zip = opccore::zip::ZipArchive::open(&bytes).expect("a .docx");
    let xml = zip.read("word/document.xml").expect("a main part");
    String::from_utf8_lossy(&xml).contains(text)
}

/// #630: with "Ask before closing the window" on, closing the window asks
/// Word's question once per unsaved document, in tab order, and never for a
/// clean one. Cancel keeps the window; Don't Save leaves the file as it is and
/// the edits do not come back; Save writes it; the last answer ends the app as
/// a clean exit.
#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn closing_the_window_asks_once_per_unsaved_document() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/window-quit-tests")
            .join(format!("ask-{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let first = sandbox.join("first.docx");
    let second = sandbox.join("second.docx");
    let book = sandbox.join("clean.xlsx");
    std::fs::copy(root.join("fixtures/basic.docx"), &first).unwrap();
    std::fs::copy(root.join("fixtures/basic.docx"), &second).unwrap();
    std::fs::copy(root.join("fixtures/basic.xlsx"), &book).unwrap();
    let original = std::fs::read(&first).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let mut app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    call(&driver, "ask-on-close", &[("on", Json::Bool(true))]);
    // The fresh instance's never-saved sample tab is clean: never asked.
    for (path, marker) in [
        (&first, Some("QuitDiscardMarker")),
        (&second, Some("QuitSaveMarker")),
        (&book, None),
    ] {
        call(
            &driver,
            "open",
            &[("path", Json::Str(path.display().to_string()))],
        );
        if let Some(marker) = marker {
            call(&driver, "type", &[("text", Json::Str(marker.into()))]);
        }
    }
    let ask = |driver: &Driver| call(driver, "close-window", &[]);

    // The first unsaved document comes to the front with the question.
    let state = ask(&driver);
    assert_eq!(state.get_str("dialog"), Some("save-on-close"), "{state}");
    assert_eq!(state.get_str("title"), Some("first.docx"), "{state}");
    // Cancel: the window stays, and closing again asks again from the start.
    call(
        &driver,
        "dialog-click",
        &[("button", Json::Str("Cancel".into()))],
    );
    let state = ask(&driver);
    assert_eq!(state.get_str("title"), Some("first.docx"), "{state}");
    // Only one question at a time: the close is refused under it.
    assert!(driver.call("close-window", Json::obj(vec![])).is_err());

    let state = call(
        &driver,
        "dialog-click",
        &[("button", Json::Str("Don't Save".into()))],
    );
    assert_eq!(
        state.get("dialog").and_then(|d| d.get_str("id")),
        Some("save-on-close"),
        "{state}"
    );
    assert_eq!(state.get_str("title"), Some("second.docx"), "{state}");
    assert!(
        app.exited().is_none(),
        "the app went before the last answer"
    );
    // The last answer: Save, and the app ends as after `quit`.
    driver
        .call(
            "dialog-click",
            Json::obj(vec![("button", Json::Str("Save".into()))]),
        )
        .unwrap();
    drop(driver);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.exited().is_none() {
        assert!(std::time::Instant::now() < deadline, "the app did not quit");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert_eq!(
        std::fs::read(&first).unwrap(),
        original,
        "Don't Save wrote the file"
    );
    assert!(docx_has(&second, "QuitSaveMarker"), "Save did not write");
    // A clean exit: no crash to report next time.
    assert!(!sandbox.join("docxy/running").exists());

    // The next launch has every tab, none of them with the discarded edits.
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let state = call(
        &driver,
        "tab-select",
        &[("tab", Json::Str("first.docx".into()))],
    );
    assert_eq!(state.get("dirty"), Some(&Json::Bool(false)), "{state}");
    let doc = call(&driver, "doc", &[]);
    let text = doc
        .get("mail")
        .and_then(|m| m.get_str("text"))
        .unwrap_or_else(|| panic!("no body text in {doc}"));
    assert!(
        !text.contains("QuitDiscardMarker"),
        "the discarded edit came back: {text}"
    );
    let state = call(
        &driver,
        "tab-select",
        &[("tab", Json::Str("second.docx".into()))],
    );
    assert_eq!(state.get("dirty"), Some(&Json::Bool(false)), "{state}");
    // The saved one reopens from its file, with what was typed.
    let doc = call(&driver, "doc", &[]);
    let text = doc.get("mail").and_then(|m| m.get_str("text"));
    assert!(text.is_some_and(|t| t.contains("QuitSaveMarker")), "{doc}");
    app.shutdown(Some(&driver));
}

/// #630 r1: Save on a never-saved document during a window close names it
/// anew (its first words, in the location), and the quit goes on to the next
/// unsaved tab rather than taking that for a change of tabs.
#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn a_quit_goes_on_after_saving_a_new_document_under_its_first_words() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/window-quit-tests")
            .join(format!("new-{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let doc = sandbox.join("later.docx");
    std::fs::copy(root.join("fixtures/basic.docx"), &doc).unwrap();
    let exe = launch::find_suite(None).unwrap();

    let mut app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    call(&driver, "ask-on-close", &[("on", Json::Bool(true))]);
    // The sample tab is clean; a new document first, then a file after it.
    call(&driver, "key", &[("key", Json::Str("ctrl+n".into()))]);
    call(
        &driver,
        "type",
        &[(
            "text",
            Json::Str("Quarterly report for the north region".into()),
        )],
    );
    call(
        &driver,
        "open",
        &[("path", Json::Str(doc.display().to_string()))],
    );
    call(
        &driver,
        "type",
        &[("text", Json::Str("LaterMarker".into()))],
    );

    let state = call(&driver, "close-window", &[]);
    assert_eq!(state.get_str("title"), Some("Document1"), "{state}");
    let state = call(
        &driver,
        "dialog-click",
        &[("button", Json::Str("Save".into()))],
    );
    // Not cancelled: the next unsaved tab is asked.
    assert_eq!(state.get_str("title"), Some("later.docx"), "{state}");
    assert_eq!(
        state.get("dialog").and_then(|d| d.get_str("id")),
        Some("save-on-close"),
        "{state}"
    );
    driver
        .call(
            "dialog-click",
            Json::obj(vec![("button", Json::Str("Save".into()))]),
        )
        .unwrap();
    drop(driver);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.exited().is_none() {
        assert!(std::time::Instant::now() < deadline, "the app did not quit");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let named = sandbox.join("Quarterly report for the north region.docx");
    assert!(docx_has(&named, "Quarterly report"), "{}", named.display());
    assert!(docx_has(&doc, "LaterMarker"));
}
