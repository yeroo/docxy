//! The `inputs-typing-*.uit` cases, generated from the catalogue (#1029).
//!
//! One file per surface, one test per field and step, so a field broken in
//! one way fails only that test, and `expected-failures.txt` can list it
//! without hiding the field's other steps. Every key is a real one
//! (`real-key`, `real-type`) and every focus a real click or Tab: no
//! `dialog-set`, no `key`, no `type`.

use super::{Entry, Field, Refuse, Surface, catalog};
use crate::dialog::ControlKind;

/// The generated files: (file name under `uiharness/cases/`, text). A
/// surface with nothing to type into has no file.
pub(crate) fn files() -> Vec<(String, String)> {
    let mut names: Vec<&str> = catalog().iter().map(|e| e.file).collect();
    names.dedup();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .filter_map(|file| {
            let entries: Vec<&Entry> = catalog()
                .iter()
                .filter(|e| e.file == file && e.unreachable.is_none())
                .collect();
            let tests: Vec<String> = entries.iter().flat_map(|e| entry_tests(e)).collect();
            (!tests.is_empty()).then(|| {
                let name = format!("inputs-typing-{file}.uit");
                (name, file_text(entries[0].surface, &tests))
            })
        })
        .chain(std::iter::once((
            "inputs-typing-themes.uit".to_string(),
            themes_text(),
        )))
        .collect()
}

/// One field of each kind (the catalogue's first text, number and
/// dropdown field) typed or stepped under the dark theme and the light one,
/// so the drawn field and its caret are exercised in both.
fn themes_text() -> String {
    let first = |kind: ControlKind| {
        catalog()
            .iter()
            .filter(|e| e.unreachable.is_none())
            .find_map(|e| {
                e.fields
                    .iter()
                    .find(|f| f.kind == kind && f.skip.is_none())
                    .map(|f| (e, f))
            })
    };
    let mut out = String::from(
        "# GENERATED with the inputs-typing-*.uit cases (see there); do not edit\n\
         # by hand (#1029). A text, a number and a dropdown field under each theme.\n",
    );
    for theme in ["dark", "light"] {
        for kind in [
            ControlKind::Text,
            ControlKind::Number,
            ControlKind::Dropdown,
        ] {
            let Some((e, f)) = first(kind) else {
                continue;
            };
            let (step, mut t) = if kind == ControlKind::Dropdown {
                ("Up and Down step", dropdown_tests(e, f))
            } else {
                ("types", text_tests(e, f))
            };
            let mut t = t.remove(
                t.iter()
                    .position(|t| t.name.ends_with(step))
                    .expect("the step"),
            );
            t.name = format!("{theme} theme {}", t.name);
            t.lines
                .insert(1, format!("call theme-set {{\"theme\":\"{theme}\"}}"));
            let at = t.lines.len() - 1;
            t.lines.insert(at, "shot window".into());
            t.line("call theme-set {\"theme\":\"auto\"}");
            out.push('\n');
            out.push_str(&t.text());
        }
    }
    out
}

fn file_text(s: Surface, tests: &[String]) -> String {
    let mut out = format!(
        "# GENERATED from suite/docxy/src/dialog/catalog/entries.rs by\n\
         # `UPDATE_INPUTS_TYPING=1 cargo test --manifest-path suite/Cargo.toml\n\
         # inputs_typing_case_is_current`; do not edit by hand (#1029).\n\
         #\n\
         # Every editable field of every {} dialog, typed into with real keys\n\
         # only: `real-key` and `real-type` go through the window's own input\n\
         # path, and the field is focused by a real click or by Tab. A test is\n\
         # one field and one step, named `<surface> <dialog>/<field>: <step>`.\n\
         # See qa/inputs-typing.md.\n",
        s.name()
    );
    for t in tests {
        out.push('\n');
        out.push_str(t);
    }
    out
}

/// What the edit steps type: digits into a number field, letters elsewhere.
fn edit_text(f: &Field) -> (&'static str, &'static str, &'static str) {
    // (typed, the character typed back in, what pasting puts in)
    match f.kind {
        ControlKind::Number => ("1234", "9", "42"),
        _ => ("abcd", "X", "Pasted"),
    }
}

/// A script value: quoted when it has spaces at its ends.
fn val(s: &str) -> String {
    if s.trim() != s || s.is_empty() {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// A JSON string literal.
fn js(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Test {
    name: String,
    lines: Vec<String>,
}

impl Test {
    fn new(e: &Entry, f: &Field, step: &str) -> Self {
        Self {
            name: format!(
                "{} {}/{}: {step}",
                e.surface.name(),
                e.dialog.as_str(),
                f.name
            ),
            lines: Vec::new(),
        }
    }

    fn line(&mut self, l: impl Into<String>) -> &mut Self {
        self.lines.push(l.into());
        self
    }

    fn call(&mut self, verb: &str, args: &str) -> &mut Self {
        self.line(format!("call {verb} {args}"))
    }

    fn key(&mut self, key: &str) -> &mut Self {
        self.call("real-key", &format!("{{\"key\":{}}}", js(key)))
    }

    fn typed(&mut self, text: &str) -> &mut Self {
        self.call("real-type", &format!("{{\"text\":{}}}", js(text)))
    }

    fn read(&mut self, f: &Field) -> &mut Self {
        self.call(
            "field-read",
            &format!("{{\"dialog-field\":{}}}", js(f.name)),
        )
    }

    fn click(&mut self, f: &Field) -> &mut Self {
        let target = if f.kind.is_text() {
            "dialog-field"
        } else {
            "dialog-control"
        };
        self.call(
            "pointer-click",
            &format!("{{{}:{}}}", js(target), js(f.name)),
        )
    }

    /// Open the fixture and the dialog, check it against its entry, and show
    /// the field: its tab, then whatever it waits on.
    fn open(&mut self, e: &Entry, f: &Field) -> &mut Self {
        self.line(format!("open {}", e.fixture));
        self.show(e, f, e.open)
    }

    /// Open the dialog over the document as it is (`open`, or its `reopen`
    /// the second time), check it against its entry, and show the field:
    /// what it waits on, then its tab.
    fn show(&mut self, e: &Entry, f: &Field, lines: &[&str]) -> &mut Self {
        for l in lines {
            self.line(*l);
        }
        self.line(format!("assert dialog is {}", e.dialog.as_str()));
        self.call("dialog-catalog-check", "{}");
        for l in f.prep {
            self.line(*l);
        }
        if let Some(tab) = f.tab {
            self.call("pointer-click", &format!("{{\"dialog-tab\":{}}}", js(tab)));
        }
        self
    }

    /// The dialog again, after the test closed it.
    fn reopen(&mut self, e: &Entry, f: &Field) -> &mut Self {
        let lines = if e.reopen.is_empty() {
            e.open
        } else {
            e.reopen
        };
        self.show(e, f, lines)
    }

    /// Focus the field by a click and replace its text with `text`.
    fn fill(&mut self, f: &Field, text: &str) -> &mut Self {
        self.click(f).key("ctrl+a").typed(text)
    }

    fn assert(&mut self, what: &str) -> &mut Self {
        self.line(format!("assert {what}"))
    }

    /// Close whatever dialog the test left open, so the next one starts on
    /// the document.
    fn close(&mut self) -> &mut Self {
        self.key("escape")
    }

    fn text(&self) -> String {
        let mut out = format!("test {}\n", self.name);
        for l in &self.lines {
            out.push_str("  ");
            out.push_str(l);
            out.push('\n');
        }
        out
    }
}

fn entry_tests(e: &Entry) -> Vec<String> {
    // Every dialog the case can open is held to its entry, fields or none.
    let mut check = Test {
        name: format!(
            "{} {}: matches the catalogue",
            e.surface.name(),
            e.dialog.as_str()
        ),
        lines: Vec::new(),
    };
    check.line(format!("open {}", e.fixture));
    for l in e.open {
        check.line(*l);
    }
    check
        .line(format!("assert dialog is {}", e.dialog.as_str()))
        .call("dialog-catalog-check", "{}")
        .close();
    std::iter::once(check.text())
        .chain(
            e.fields
                .iter()
                .filter(|f| f.skip.is_none())
                .flat_map(|f| {
                    if f.kind.is_text() {
                        text_tests(e, f)
                    } else if f.kind == ControlKind::Dropdown {
                        dropdown_tests(e, f)
                    } else {
                        Vec::new()
                    }
                })
                .map(|t| t.text()),
        )
        .collect()
}

fn text_tests(e: &Entry, f: &Field) -> Vec<Test> {
    let mut tests = Vec::new();
    let n = f.sample.chars().count();
    let (edit, back, paste) = edit_text(f);
    let edit_chars: Vec<char> = edit.chars().collect();

    let mut t = Test::new(e, f, "focus by click");
    t.open(e, f)
        .click(f)
        .read(f)
        .assert("reply.focused is true")
        .close();
    tests.push(t);

    let mut t = Test::new(e, f, "focus by Tab");
    t.open(e, f)
        .call(
            "real-key",
            &format!("{{\"key\":\"tab\",\"to-field\":{}}}", js(f.name)),
        )
        .read(f)
        .assert("reply.focused is true")
        .close();
    tests.push(t);

    let mut t = Test::new(e, f, "types");
    t.open(e, f)
        .fill(f, f.sample)
        .read(f)
        .assert(&format!("reply.value is {}", val(f.sample)))
        .assert(&format!("reply.caret is {n}"))
        .close();
    tests.push(t);

    // Backspace, Delete, Left/Right and Home/End: "abcd" -> "abc" -> "ac" -> "aXc".
    let (a, c) = (edit_chars[0], edit_chars[2]);
    let mut t = Test::new(e, f, "edits");
    t.open(e, f)
        .fill(f, edit)
        .key("backspace")
        .read(f)
        .assert(&format!("reply.value is {}", &edit[..3]))
        .key("home")
        .read(f)
        .assert("reply.caret is 0")
        .key("right")
        .key("delete")
        .read(f)
        .assert(&format!("reply.value is {a}{c}"))
        .key("end")
        .key("left")
        .read(f)
        .assert("reply.caret is 1")
        .typed(back)
        .read(f)
        .assert(&format!("reply.value is {a}{back}{c}"))
        .assert("reply.caret is 2")
        .close();
    tests.push(t);

    let mut t = Test::new(e, f, "select-all replaces");
    t.open(e, f)
        .fill(f, edit)
        .key("ctrl+a")
        .read(f)
        .assert("reply.selection.0 is 0")
        .assert(&format!("reply.selection.1 is {}", edit.len()))
        .typed(back)
        .read(f)
        .assert(&format!("reply.value is {back}"))
        .close();
    tests.push(t);

    let mut t = Test::new(e, f, "pastes");
    t.open(e, f)
        .call(
            "clipboard",
            &format!("{{\"action\":\"write\",\"text\":{}}}", js(paste)),
        )
        .click(f)
        .key("ctrl+a")
        .key("ctrl+v")
        .read(f)
        .assert(&format!("reply.value is {paste}"))
        .close();
    tests.push(t);

    let mut t = Test::new(e, f, "copies");
    t.open(e, f)
        .call("clipboard", "{\"action\":\"write\",\"text\":\"before\"}")
        .fill(f, edit)
        .key("ctrl+a")
        .key("ctrl+c")
        .call("clipboard", "{\"action\":\"read\"}")
        .assert(&format!("reply.text is {edit}"))
        .close();
    tests.push(t);

    if f.skip_ok.is_none() {
        let mut t = Test::new(e, f, "OK applies");
        t.open(e, f).fill(f, f.sample).key("enter");
        if e.accept_closes {
            t.assert(&format!("dialog is not {}", e.dialog.as_str()));
        }
        for l in f.applied {
            t.line(*l);
        }
        if let Some(kept) = f.kept {
            if !e.accept_closes {
                t.close();
            }
            t.reopen(e, f)
                .read(f)
                .assert(&format!("reply.value is {}", val(kept)));
            t.close();
        } else if !e.accept_closes {
            t.close();
        }
        tests.push(t);
    }

    // A value no other step writes, so a field that opens on what an
    // earlier case's OK kept cannot pass by chance.
    let cancel = match f.kind {
        ControlKind::Number => "77",
        _ => "Esc9",
    };
    let mut t = Test::new(e, f, "Escape cancels");
    t.open(e, f)
        .fill(f, cancel)
        .key("escape")
        .assert(&format!("dialog is not {}", e.dialog.as_str()))
        .reopen(e, f)
        .read(f)
        .assert(&format!("reply.value is not {cancel}"))
        .close();
    tests.push(t);

    if let Some((bad, refuse)) = f.invalid {
        let mut t = Test::new(e, f, "refuses invalid");
        t.open(e, f);
        match refuse {
            Refuse::AtType => {
                t.fill(f, f.sample)
                    .typed(bad)
                    .read(f)
                    .assert(&format!("reply.value is {}", val(f.sample)));
            }
            Refuse::AtOk => {
                t.fill(f, bad)
                    .key("enter")
                    .assert(&format!("dialog is {}", e.dialog.as_str()));
            }
        }
        t.close();
        tests.push(t);
    }
    tests
}

fn dropdown_tests(e: &Entry, f: &Field) -> Vec<Test> {
    let mut tests = Vec::new();

    let mut t = Test::new(e, f, "focus by click");
    t.open(e, f)
        .click(f)
        .read(f)
        .assert("reply.focused is true")
        .close();
    tests.push(t);

    // Up as often as it can have items reaches the first; Down steps once.
    let mut t = Test::new(e, f, "Up and Down step");
    t.open(e, f)
        .call(
            "real-key",
            &format!("{{\"key\":\"tab\",\"to-field\":{}}}", js(f.name)),
        )
        .read(f)
        .assert("reply.focused is true")
        .call("real-key", "{\"key\":\"up\",\"times\":100}")
        .read(f)
        .assert("reply.index is 0")
        .key("down")
        .read(f)
        .assert("reply.index is 1")
        .close();
    tests.push(t);
    tests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::catalog::USER_NAME;

    fn entry(fields: &'static [Field]) -> Entry {
        Entry {
            dialog: USER_NAME,
            surface: Surface::Doc,
            file: "doc-test",
            fixture: "../fixtures/basic.docx",
            open: &["call user-name {}"],
            fields,
            reopen: &[],
            accept_closes: true,
            unreachable: None,
        }
    }

    fn names(e: &Entry) -> Vec<String> {
        entry_tests(e)
            .iter()
            .map(|t| {
                t.lines()
                    .next()
                    .unwrap()
                    .trim_start_matches("test ")
                    .to_string()
            })
            .collect()
    }

    /// #1029: a text field gets one test per step, every key real, and no
    /// `dialog-set`, `key` or `type` once the dialog is open.
    #[test]
    fn a_text_field_gets_a_test_per_step_with_real_keys_only() {
        static FIELDS: &[Field] = &[Field::text("user-name")];
        let e = entry(FIELDS);
        assert_eq!(
            names(&e),
            [
                "doc user-name: matches the catalogue",
                "doc user-name/user-name: focus by click",
                "doc user-name/user-name: focus by Tab",
                "doc user-name/user-name: types",
                "doc user-name/user-name: edits",
                "doc user-name/user-name: select-all replaces",
                "doc user-name/user-name: pastes",
                "doc user-name/user-name: copies",
                "doc user-name/user-name: OK applies",
                "doc user-name/user-name: Escape cancels",
            ]
        );
        for t in entry_tests(&e) {
            let after_open = t.split("assert dialog is user-name").nth(1).unwrap_or("");
            for banned in ["dialog-set", "\n  key ", "\n  type "] {
                assert!(!after_open.contains(banned), "{banned} in\n{t}");
            }
        }
    }

    /// #1029: a number refuses a letter as it is typed; a date or duration
    /// field is refused on OK, and the dialog stays open.
    #[test]
    fn each_kind_is_refused_its_own_way() {
        static FIELDS: &[Field] = &[
            Field::number("n", "5"),
            Field::date("d", "2026-01-02", "soon"),
            Field::duration("t", "3d", "long"),
        ];
        let tests = entry_tests(&entry(FIELDS));
        let refusal = |name: &str| {
            tests
                .iter()
                .find(|t| t.starts_with(&format!("test doc user-name/{name}: refuses invalid")))
                .unwrap_or_else(|| panic!("{name} has a refusal test"))
                .clone()
        };
        let n = refusal("n");
        assert!(
            n.contains(r#"call real-type {"text":"x"}"#) && n.contains("assert reply.value is 5")
        );
        for (name, bad) in [("d", "soon"), ("t", "long")] {
            let t = refusal(name);
            assert!(
                t.contains(&format!(r#"call real-type {{"text":"{bad}"}}"#)),
                "{t}"
            );
            assert!(
                t.contains("call real-key {\"key\":\"enter\"}\n  assert dialog is user-name"),
                "{t}"
            );
        }
        let edits = tests.iter().find(|t| t.contains("n: edits")).unwrap();
        assert!(
            edits.contains(r#"{"text":"1234"}"#),
            "a number field edits digits"
        );
    }

    /// #1029: a field skipped says why and gets no test; a dropdown gets a
    /// click and an Up/Down test; a checkbox none.
    #[test]
    fn skipped_fields_and_choices_get_only_their_own_tests() {
        static FIELDS: &[Field] = &[
            Field::text("gone").skip("hidden"),
            Field::other("pick", ControlKind::Dropdown),
            Field::other("tick", ControlKind::Checkbox),
        ];
        let e = entry(FIELDS);
        assert_eq!(
            names(&e),
            [
                "doc user-name: matches the catalogue",
                "doc user-name/pick: focus by click",
                "doc user-name/pick: Up and Down step",
            ]
        );
    }
}
