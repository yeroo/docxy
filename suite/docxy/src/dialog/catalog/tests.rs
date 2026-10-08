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

/// Production code: `.rs` files outside the catalogue, minus the modules
/// compiled only for tests: the files a `#[cfg(test)] mod name;` declares
/// (and everything under them), and the bodies of inline
/// `#[cfg(test)] mod name { .. }` modules.
fn production(files: &[(PathBuf, String)]) -> Vec<(&PathBuf, String)> {
    let test_dirs: Vec<PathBuf> = files
        .iter()
        .flat_map(|(p, text)| {
            let dir = module_dir(p);
            test_modules(text)
                .into_iter()
                .filter(|m| !m.inline)
                .map(move |m| dir.join(m.name))
        })
        .collect();
    files
        .iter()
        .filter(|(p, _)| {
            let declared_for_tests = test_dirs
                .iter()
                .any(|d| p.starts_with(d) || *p == d.with_extension("rs"));
            !p.starts_with(src().join("dialog/catalog"))
                && !p.ends_with("dialog/catalog.rs")
                && !declared_for_tests
        })
        .map(|(p, text)| (p, without_test_modules(text)))
        .collect()
}

/// Where the modules `file` declares live: beside it for `main.rs`,
/// `lib.rs` and `mod.rs`, else in the directory named after it.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new(""));
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("main" | "lib" | "mod") | None => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
    }
}

/// A `#[cfg(test)]` module in a file's text.
struct TestModule<'a> {
    name: &'a str,
    /// Declared with a body (`{`), not `;`.
    inline: bool,
    /// Where `#[cfg(test)]` starts, and where the body's `{` is.
    at: usize,
    body: usize,
}

/// The `#[cfg(test)]` modules of `text`: the attribute, then any other
/// attributes, then `mod name` with an optional visibility (`pub`,
/// `pub(crate)`, `pub(super)`, `pub(in ..)`).
fn test_modules(text: &str) -> Vec<TestModule<'_>> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices("#[cfg(test)]") {
        let mut rest = &text[at + "#[cfg(test)]".len()..];
        loop {
            rest = rest.trim_start();
            match rest.strip_prefix("#[") {
                Some(attr) => rest = attr.find(']').map_or("", |e| &attr[e + 1..]),
                None => break,
            }
        }
        if let Some(vis) = rest.strip_prefix("pub") {
            let vis = vis.trim_start();
            rest = match vis.strip_prefix('(') {
                Some(inner) => inner.find(')').map_or("", |e| &inner[e + 1..]),
                None => vis,
            }
            .trim_start();
        }
        let Some(decl) = rest.strip_prefix("mod ") else {
            continue;
        };
        let decl = decl.trim_start();
        let end = decl
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(decl.len());
        let after = decl[end..].trim_start();
        let inline = after.starts_with('{');
        if !inline && !after.starts_with(';') {
            continue;
        }
        let body = text.len() - after.len();
        found.push(TestModule {
            name: &decl[..end],
            inline,
            at,
            body,
        });
    }
    found
}

/// `text` without its inline `#[cfg(test)]` modules (braces counted).
fn without_test_modules(text: &str) -> String {
    let mut out = String::new();
    let mut from = 0;
    for m in test_modules(text).into_iter().filter(|m| m.inline) {
        if m.at < from {
            continue;
        }
        out.push_str(&text[from..m.at]);
        let mut depth = 0usize;
        let mut end = text.len();
        for (i, c) in text[m.body..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = m.body + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        from = end;
    }
    out.push_str(&text[from..]);
    out
}

/// `text` names `catalog::KEY` as a whole identifier (not `KEY_2`).
fn names(text: &str, key: &str) -> bool {
    let want = format!("catalog::{key}");
    text.match_indices(&want).any(|(at, _)| {
        !text[at + want.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// #1029: every dialog carries a catalogued id (a `DialogId` is only made in
/// `catalog.rs`, and `Dialog::message` takes one), so this is the reverse
/// check: every entry is a dialog production code still builds, ids name one
/// dialog per surface, and each entry is either reachable with a way to open
/// it or says why not. A dialog that reuses another's id is caught by review
/// and, where a case opens it, by `dialog-catalog-check`.
#[test]
fn every_dialog_is_in_the_catalogue() {
    let mut files = Vec::new();
    sources(&src(), &mut files);
    let live = production(&files);
    assert!(names("x catalog::GOTO;", "GOTO") && !names("catalog::GOTO_SPECIAL", "GOTO"));
    let cut = without_test_modules(
        "a\n#[cfg(test)]\n#[allow(dead_code)]\npub(crate) mod t {\n fn f() { catalog::X }\n}\nb\n#[cfg(test)]\nmod u;\n",
    );
    assert_eq!(cut, "a\n\nb\n#[cfg(test)]\nmod u;\n");
    let declared: Vec<(&str, bool)> =
        test_modules("#[cfg(test)]\npub mod u;\n#[cfg(test)] fn g() {}")
            .iter()
            .map(|m| (m.name, m.inline))
            .collect();
    assert_eq!(declared, [("u", false)]);
    let file = |p: &str| live.iter().any(|(f, _)| f.ends_with(p));
    assert!(file("close.rs") && !file("ribbon_export.rs") && !file("dialog/tests.rs"));
    for e in catalog() {
        let key = e.dialog.key();
        let used = live.iter().any(|(_, text)| names(text, key));
        assert!(
            used,
            "{key} ('{}') is in the catalogue but no production code builds a dialog with it",
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
            assert!(
                f.skip_ok.is_some() || !f.applied.is_empty() || f.kept.is_some(),
                "{at}: its OK case needs something to check (applied or kept), or no_ok says why not"
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

/// #1029: while the dialog under test is open, the generated cases drive it
/// with real input only: no `dialog-set`, `dialog-click`, `dialog-tab`, `key`
/// or `type` (as steps or as `call` verbs) between `assert dialog is <id>` and the dialog closing. The
/// steps that open it (and set the document up) come before.
#[test]
fn the_cases_drive_an_open_dialog_with_real_input_only() {
    let banned = [
        "call dialog-set ",
        "call dialog-click ",
        "call dialog-tab ",
        "key ",
        "type ",
    ];
    for (file, text) in cases::files() {
        for test in text.split("\ntest ").skip(1) {
            let name = test.lines().next().unwrap_or_default();
            let mut open: Option<String> = None;
            for line in test.lines().skip(1).map(str::trim) {
                if let Some(id) = line.strip_prefix("assert dialog is ") {
                    open = id.strip_prefix("not ").is_none().then(|| id.to_string());
                    continue;
                }
                if open.is_some() {
                    assert!(
                        !banned.iter().any(|b| line.starts_with(b)),
                        "{file}: '{name}' drives the open dialog with '{line}'"
                    );
                }
            }
        }
    }
}
