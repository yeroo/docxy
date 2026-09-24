//! Desktop-only checks of the real close driver and its writes. Run after
//! building suite: cargo test -p uiharness --test tab_close -- --ignored --nocapture
use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use uiharness::{Driver, Run, Runner, launch, parse_script, run::slug};

fn call(driver: &Driver, verb: &str, args: &[(&str, Json)]) -> Json {
    driver.call(verb, Json::obj(args.to_vec())).unwrap()
}

fn open(driver: &Driver, path: &Path) {
    call(
        driver,
        "open",
        &[("path", Json::Str(path.display().to_string()))],
    );
}

fn edit(driver: &Driver, cell: Option<&str>, text: &str) {
    if let Some(cell) = cell {
        call(driver, "click-cell", &[("cell", Json::Str(cell.into()))]);
    }
    call(driver, "type", &[("text", Json::Str(text.into()))]);
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
fn tab_close_driver_preserves_work_and_persists_removals() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/tab-close-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    let exe = launch::find_suite(None).unwrap();
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();
    let script = parse_script(include_str!("../cases/tab-close.uit")).unwrap();
    let outcome = Runner::new(&driver, &run, root.join("cases"), &sandbox).run_script(&script);
    println!("{}", outcome.report());
    assert!(outcome.passed(), "{}", outcome.report());
    assert!(
        session_tabs(&sandbox).is_empty(),
        "Discard must persist removal before shutdown"
    );
    assert!(
        !sandbox.join("sample.docx").exists(),
        "pathless close/Save wrote into cwd"
    );
    assert!(!sandbox.join("Untitled.docx").exists());

    for (kind, fixture, cell) in [
        ("docx", "basic.docx", None),
        ("xlsx", "basic.xlsx", Some("A1")),
        ("project", "gantt-summary.xml", Some("B2")),
    ] {
        for on in [true, false] {
            // These are the copies written by the .uit Save cases. Verify
            // actual disk content, then reopen through the real file loader.
            let name = format!("{kind} answers with window ask {on}");
            let saved = sandbox.join(slug(&name)).join(fixture);
            let bytes = std::fs::read(&saved).unwrap();
            if kind == "docx" {
                let zip = opccore::zip::ZipArchive::open(&bytes).unwrap();
                let xml = zip.read("word/document.xml").unwrap();
                let text = String::from_utf8(xml).unwrap();
                assert!(text.contains("CloseCancelMarkerCloseSavedMarker"), "{text}");
            } else if kind == "project" {
                assert!(
                    String::from_utf8(bytes)
                        .unwrap()
                        .contains("CloseSavedMarker")
                );
            }
            open(&driver, &saved);
            if let Some(cell) = cell {
                let value = call(&driver, "cell", &[("cell", Json::Str(cell.into()))]);
                assert_eq!(value.get_str("value"), Some("CloseSavedMarker"));
            }
            call(&driver, "close-tab", &[]);
            assert!(session_tabs(&sandbox).is_empty());

            // A directory at the destination makes write_atomic fail on every
            // platform, without depending on read-only flags or ACLs. Only a
            // private fixture copy is touched, and the tab is inactive at close.
            let fail_path = sandbox.join(format!("fail-{on}-{fixture}"));
            std::fs::copy(root.join("fixtures").join(fixture), &fail_path).unwrap();
            open(&driver, &fail_path);
            edit(&driver, cell, "FailedSaveMarker");
            open(&driver, &root.join("fixtures/basic.docx"));
            std::fs::remove_file(&fail_path).unwrap();
            std::fs::create_dir(&fail_path).unwrap();
            call(&driver, "ask-on-close", &[("on", Json::Bool(on))]);
            let state = call(
                &driver,
                "close-tab",
                &[
                    ("index", Json::Num(0.)),
                    ("answer", Json::Str("save".into())),
                ],
            );
            assert_eq!(state.get_usize("tabs"), Some(2));
            assert_eq!(state.get_usize("tab"), Some(0));
            assert_eq!(state.get("dirty"), Some(&Json::Bool(true)));
            assert!(
                state.get_str("status").unwrap().contains("save failed"),
                "{state}"
            );
            assert_eq!(session_tabs(&sandbox).len(), 2);
            if let Some(cell) = cell {
                let value = call(&driver, "cell", &[("cell", Json::Str(cell.into()))]);
                assert_eq!(value.get_str("value"), Some("FailedSaveMarker"));
            }
            // Discard removes just the failed target from the persisted session.
            call(
                &driver,
                "close-tab",
                &[("answer", Json::Str("discard".into()))],
            );
            let tabs = session_tabs(&sandbox);
            assert_eq!(tabs.len(), 1);
            assert_eq!(tabs[0].get_str("title"), Some("basic.docx"));
            call(&driver, "close-tab", &[]);
        }
    }
    assert!(session_tabs(&sandbox).is_empty());
    app.shutdown(Some(&driver));
}
