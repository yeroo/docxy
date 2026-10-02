//! Desktop-only check of #633 in the real app: an RTF, a Web Page and a PDF
//! open converted, through the converting child process the suite spawns
//! (`docxy --convert-import`), and Save never writes over them.
//! Run after building suite:
//! cargo test -p uiharness --test open_converted -- --ignored --nocapture
#![cfg(windows)]

use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::{Driver, Run, launch};

fn ok(driver: &Driver, verb: &str, args: Vec<(&str, Json)>) -> Json {
    driver
        .call(verb, Json::obj(args))
        .unwrap_or_else(|e| panic!("{verb}: {e}"))
}

fn s(text: &str) -> Json {
    Json::Str(text.into())
}

#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn rtf_html_and_pdf_open_converted_in_a_child_process() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/open-converted-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = run.dir().join("sandbox");
    let files = run.dir().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let corpus = root.join("../corpus/word-import");
    let exe = launch::find_suite(None).unwrap();
    let app = launch::launch(&exe, &sandbox).unwrap();
    let driver = Driver::connect(&app.ctl_dir(), None).unwrap();

    for (name, status) in [
        ("source.rtf", "loaded (converted from RTF)"),
        ("source.htm", "loaded (converted from HTML)"),
        ("source.pdf", "loaded (converted from PDF)"),
    ] {
        let path = files.join(name);
        std::fs::copy(corpus.join(name), &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let st = ok(
            &driver,
            "open",
            vec![("path", s(&path.display().to_string()))],
        );
        assert_eq!(
            st.get("status").and_then(Json::as_str),
            Some(status),
            "{name}: {st}"
        );
        let text = ok(&driver, "doc", vec![])
            .get("mail")
            .and_then(|m| m.get("text"))
            .and_then(Json::as_str)
            .unwrap()
            .to_string();
        assert!(text.contains("Import fixture"), "{name}: {text}");
        // Save is Save As for a converted tab; a harness instance says so.
        let st = ok(&driver, "key", vec![("keys", Json::Arr(vec![s("ctrl+s")]))]);
        let said = st.get("status").and_then(Json::as_str).unwrap_or_default();
        assert!(
            said.contains("converted from another format"),
            "{name}: {said}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{name} untouched");
        // Save As .docx writes a Word document beside it.
        let out = files.join(format!("{name}.docx"));
        ok(
            &driver,
            "save-as",
            vec![("path", s(&out.display().to_string()))],
        );
        let saved = std::fs::read(&out).unwrap();
        assert!(saved.starts_with(b"PK\x03\x04"), "{name}: a Word package");
    }
    let _ = ok(&driver, "quit", vec![]);
}
