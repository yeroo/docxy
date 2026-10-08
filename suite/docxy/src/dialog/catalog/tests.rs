//! The catalogue's drift guards (#1029).

use super::*;
use std::path::{Path, PathBuf};

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn cases() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/cases")
}

/// Every `.rs` file under `dir`, with its text.
fn sources(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).expect("read the source tree") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).expect("read a source file");
            out.push((path, text));
        }
    }
}

/// #1029: a dialog cannot be built without an entry (a `DialogId` is only
/// made in `catalog.rs`, and `Dialog::message` takes one), so this is the
/// reverse check: every entry is a dialog the suite still builds, ids name
/// one dialog per surface, and each entry is either reachable with a way to
/// open it or says why not.
#[test]
fn every_dialog_is_in_the_catalogue() {
    let mut files = Vec::new();
    sources(&src(), &mut files);
    let outside: Vec<&(PathBuf, String)> = files
        .iter()
        .filter(|(p, _)| {
            !p.starts_with(src().join("dialog/catalog")) && !p.ends_with("dialog/catalog.rs")
        })
        .collect();
    for e in catalog() {
        let key = e.dialog.key();
        let used = outside
            .iter()
            .any(|(_, text)| text.contains(&format!("catalog::{key}")));
        assert!(
            used,
            "{key} ('{}') is in the catalogue but no dialog is built with it",
            e.dialog.as_str()
        );
        match e.unreachable {
            Some(why) => assert!(
                e.open.is_empty() && !why.is_empty(),
                "{key}: an unreachable entry has no opener and says why"
            ),
            None => assert!(
                !e.open.is_empty(),
                "{key}: a reachable entry says how to open it"
            ),
        }
    }
    let mut seen = std::collections::HashSet::new();
    for e in catalog() {
        assert!(
            seen.insert((e.surface, e.dialog.as_str())),
            "two {} dialogs share the id '{}'",
            e.surface.name(),
            e.dialog.as_str()
        );
    }
    // A `Dialog { .. }` literal outside the tests would skip `message` but
    // still needs a `DialogId`, so it cannot dodge the catalogue either.
    for t in [TEST_FORM, TEST_CHILD, TEST_T] {
        assert!(t.entry().is_none(), "{} is test-only", t.key());
    }
}

/// #1029: each field's entry is consistent: typed samples for typed kinds,
/// a refusal for number, date and duration fields, and names unique in
/// their dialog.
#[test]
fn every_field_entry_is_complete() {
    for e in catalog() {
        let mut names = std::collections::HashSet::new();
        for f in e.fields {
            let at = format!("{} {}", e.dialog.key(), f.name);
            assert!(names.insert(f.name), "{at}: listed twice");
            if !f.typed() {
                continue;
            }
            assert!(!f.sample.is_empty(), "{at}: a typed field has a sample");
            let wants_refusal = matches!(
                f.kind,
                ControlKind::Number | ControlKind::Date | ControlKind::Duration
            );
            assert_eq!(
                f.invalid.is_some(),
                wants_refusal,
                "{at}: an invalid sample for number, date and duration fields only"
            );
            if let Some((_, Refuse::AtType)) = f.invalid {
                assert_eq!(
                    f.kind,
                    ControlKind::Number,
                    "{at}: only a number refuses as it is typed"
                );
            }
        }
    }
}

/// #1029: the checked-in `inputs-typing-*.uit` are what the catalogue
/// generates. `UPDATE_INPUTS_TYPING=1` writes them.
#[test]
fn inputs_typing_case_is_current() {
    let want = cases::files();
    let update = std::env::var_os("UPDATE_INPUTS_TYPING").is_some();
    let mut stale = Vec::new();
    for (name, text) in &want {
        let path = cases().join(name);
        if update {
            std::fs::write(&path, text).expect("write the generated case");
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(name.clone());
        }
    }
    // A file for a surface that no longer has one is stale too.
    for entry in std::fs::read_dir(cases()).expect("read the cases") {
        let name = entry
            .expect("a case")
            .file_name()
            .to_string_lossy()
            .into_owned();
        if name.starts_with("inputs-typing-") && !want.iter().any(|(n, _)| *n == name) {
            if update {
                std::fs::remove_file(cases().join(&name)).expect("remove a stale case");
            } else {
                stale.push(name);
            }
        }
    }
    assert!(
        stale.is_empty(),
        "{} differ from the catalogue; regenerate with UPDATE_INPUTS_TYPING=1 cargo test --manifest-path suite/Cargo.toml inputs_typing_case_is_current",
        stale.join(", ")
    );
}
