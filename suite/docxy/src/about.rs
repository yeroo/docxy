//! The suite's build info (#1023): what `--version`, File > Account's About
//! dialog and its Copy button, `app-info` and the crash log say about this
//! binary. The commit, last merged PR and build kind come from the `buildinfo`
//! crate; this module names the product and shapes the text. About and its
//! Copy also say which manual each editor's Help shows (#1857): its revision
//! and whether it is built in or installed.

use crate::dialog::catalog;
use crate::dialog::{
    ButtonRole, Control as Field, ControlKind, Dialog, DialogOwner, DialogStack, NONE_OPEN, Value,
};
use crate::help_manual::{Bundle, Editor};
use crate::{Context, Docxy};
use buildinfo::BuildInfo;
use ctlcore::json::Json;
use std::sync::Arc;

/// The product name `--version` and the About dialog print.
pub(crate) const PRODUCT: &str = "docxy suite";

/// This binary's build info; the suite's own `CARGO_PKG_VERSION`, not the
/// terminal editors'.
pub(crate) fn info() -> &'static BuildInfo {
    buildinfo::get(env!("CARGO_PKG_VERSION"))
}

/// What `--version` prints, and the first part of what Copy copies.
pub(crate) fn version_text(info: &BuildInfo) -> String {
    info.version_block(PRODUCT)
}

/// One editor's manual as About shows it (#1857): the row's name and label,
/// and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManualRow {
    pub editor: Editor,
    pub value: String,
}

impl ManualRow {
    /// The control's name, for scripts (`word manual`).
    pub(crate) fn name(&self) -> &'static str {
        match self.editor {
            Editor::Word => "word manual",
            Editor::Excel => "excel manual",
            Editor::Project => "project manual",
        }
    }

    /// The label a person reads (`Word manual`).
    pub(crate) fn label(&self) -> String {
        format!("{} manual", self.editor.title())
    }
}

/// The manual each editor's Help shows, as `load` (the app's
/// `help_bundle`) gives it: `<revision> (built in)`, `<revision>
/// (installed)`, `unknown revision (<source>)` without a REVISION, or `none`
/// when the editor has no manual.
pub(crate) fn manual_rows(load: impl Fn(Editor) -> Option<Arc<Bundle>>) -> Vec<ManualRow> {
    Editor::ALL
        .into_iter()
        .map(|editor| ManualRow {
            editor,
            value: load(editor).map_or_else(
                || "none".to_string(),
                |b| {
                    let rev = b.manual.revision.as_deref().unwrap_or("unknown revision");
                    format!("{rev} ({})", b.source.words())
                },
            ),
        })
        .collect()
}

/// `app-info`'s `manuals` (#1857): per editor, as `load` gives it, its
/// `revision` (null without one), `short` (`Manual::short_revision`) and
/// `source` (`built-in` or `installed`), or null when it has no manual.
pub(crate) fn manuals_json(load: impl Fn(Editor) -> Option<Arc<Bundle>>) -> Json {
    let text = |t: Option<&str>| t.map_or(Json::Null, |t| Json::Str(t.to_string()));
    Json::obj(
        Editor::ALL
            .into_iter()
            .map(|e| {
                let manual = load(e).map_or(Json::Null, |b| {
                    Json::obj(vec![
                        ("revision", text(b.manual.revision.as_deref())),
                        ("short", text(b.manual.short_revision())),
                        ("source", Json::Str(b.source.name().to_string())),
                    ])
                });
                (e.name(), manual)
            })
            .collect(),
    )
}

/// What the About dialog's Copy button puts on the clipboard: the `--version`
/// block plus the one-line summary, then the manuals, ready to paste into a
/// bug report. The manual rows' labels are padded among themselves.
pub(crate) fn copy_text(info: &BuildInfo, manuals: &[ManualRow]) -> String {
    let mut s = format!("{}summary:     {}\n", version_text(info), info.short_line());
    for row in manuals {
        s.push_str(&format!(
            "{:<16}{}\n",
            format!("{}:", row.name()),
            row.value
        ));
    }
    s
}

/// The About docxy suite dialog: every field of the build as a labelled row,
/// the "Manual build" marker when it is one, each editor's manual, and Copy /
/// Close. Enter and Escape press Close.
pub(crate) fn dialog(info: &BuildInfo, manuals: &[ManualRow]) -> Dialog {
    let mut d = Dialog::message(
        catalog::ABOUT,
        &format!("About {PRODUCT} {}", info.version),
        String::new(),
        &[("Copy", ButtonRole::Apply), ("Close", ButtonRole::Cancel)],
        DialogOwner::About,
    );
    d.text = None;
    let mut controls = Vec::new();
    if info.manual() {
        controls.push(Field::new(
            "manual",
            "",
            ControlKind::Label,
            Value::Text("Manual build".into()),
        ));
    }
    for (label, value) in info.rows() {
        controls.push(Field::new(
            label,
            label,
            ControlKind::Label,
            Value::Text(value),
        ));
    }
    for row in manuals {
        controls.push(Field::new(
            row.name(),
            &row.label(),
            ControlKind::Label,
            Value::Text(row.value.clone()),
        ));
    }
    d.controls = controls;
    for b in &mut d.buttons {
        b.default = b.label == "Close";
    }
    d.mark_opened();
    d
}

impl Docxy {
    /// About docxy suite, from the Account page or Help › About (#1021): the
    /// dialog on the active tab.
    pub(crate) fn open_about(&mut self) -> Result<(), String> {
        let manuals = manual_rows(|e| self.help_bundle(e));
        let tab = self.tabs.get_mut(self.active).ok_or(NONE_OPEN)?;
        tab.dialogs.push(dialog(info(), &manuals));
        Ok(())
    }

    /// The button's click: open the dialog, or say why not on the tab's status.
    pub(crate) fn open_about_clicked(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.open_about()
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            tab.status = e.into();
        }
        cx.notify();
    }

    /// Copy puts [`copy_text`] on the clipboard (the private one in a harness) and
    /// leaves the dialog open; Close closes it. `None` for any other dialog.
    pub(crate) fn about_click(
        &mut self,
        button: &str,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        let done = click(&mut tab.dialogs, button)?;
        Some(done.map(|copy| {
            if copy {
                let manuals = manual_rows(|e| self.help_bundle(e));
                self.clipboard_write(copy_text(info(), &manuals), cx);
            }
        }))
    }
}

/// A press of `button` when the dialog on top is About: `None` for any other
/// dialog. `Ok(true)` when it was Copy (the dialog stays open, the app writes the
/// clipboard), `Ok(false)` for Close (the dialog is closed).
pub(crate) fn click(dialogs: &mut DialogStack, button: &str) -> Option<Result<bool, String>> {
    if dialogs.top()?.owner != DialogOwner::About {
        return None;
    }
    Some(dialogs.click(button, |_| Ok(())).map(|()| button == "Copy"))
}

/// Whether standard output already goes somewhere: a valid, non-null handle of a
/// known file type (a redirect, a pipe, or the console). A release build on Windows
/// is a GUI-subsystem app, so started from a shell it has none, and started with
/// `> v.txt` or by `Command::output()` it has the redirect, which must be honoured.
#[cfg_attr(not(windows), allow(dead_code))]
fn stdout_usable(handle: isize, file_type: u32) -> bool {
    // INVALID_HANDLE_VALUE is -1; FILE_TYPE_UNKNOWN is 0.
    handle != 0 && handle != -1 && file_type != 0
}

/// `--version`: print the block and return, for `main` to exit. On Windows, a
/// release build has no console of its own: when stdout is not already a redirect or
/// pipe it attaches to the parent's console and writes there, and falls back to
/// stdout if that fails. (Windows-only code: not run in this change's CI on Linux.)
pub(crate) fn print_version() {
    let text = version_text(info());
    #[cfg(windows)]
    if !windows_console::stdout_is_usable() && windows_console::write_to_parent_console(&text) {
        return;
    }
    print!("{text}");
}

/// The Win32 calls `print_version` makes, declared as `convert_child.rs` declares its own.
#[cfg(windows)]
mod windows_console {
    use std::ffi::c_void;
    use std::io::Write;

    type Handle = *mut c_void;
    /// `(DWORD)-11`.
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    /// `(DWORD)-1`.
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const FILE_TYPE_UNKNOWN: u32 = 0;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn GetFileType(handle: Handle) -> u32;
        fn AttachConsole(process_id: u32) -> i32;
    }

    /// Whether stdout already goes somewhere (see [`super::stdout_usable`]).
    pub(super) fn stdout_is_usable() -> bool {
        // SAFETY: plain Win32 calls; the handle is checked before GetFileType.
        let (handle, file_type) = unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            let valid = !h.is_null() && h as isize != INVALID_HANDLE_VALUE;
            (
                h as isize,
                if valid {
                    GetFileType(h)
                } else {
                    FILE_TYPE_UNKNOWN
                },
            )
        };
        super::stdout_usable(handle, file_type)
    }

    /// Attach to the parent's console and write `text` to it; false if either fails.
    pub(super) fn write_to_parent_console(text: &str) -> bool {
        // SAFETY: a plain Win32 call with no pointers.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            return false;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open("CONOUT$")
            .and_then(|mut out| out.write_all(text.as_bytes()))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help_manual::Source;

    #[test]
    fn the_suite_reports_its_own_version() {
        assert_eq!(info().version, env!("CARGO_PKG_VERSION"));
        assert!(
            version_text(info()).starts_with(&format!("{PRODUCT} {}\n", env!("CARGO_PKG_VERSION")))
        );
    }

    #[test]
    fn stdout_is_usable_only_with_a_real_handle_of_a_known_type() {
        assert!(stdout_usable(0x1c4, 3)); // a redirect to a file / pipe / console
        assert!(!stdout_usable(0, 0)); // a GUI process started from Explorer: no handle
        assert!(!stdout_usable(-1, 0)); // INVALID_HANDLE_VALUE
        assert!(!stdout_usable(0x1c4, 0)); // FILE_TYPE_UNKNOWN
    }

    /// A bundle of one page, read from `source`, with `revision` as its
    /// REVISION file.
    fn bundle(editor: Editor, source: Source, revision: Option<&str>) -> Arc<Bundle> {
        let mut files = vec![
            ("contents.txt".to_string(), b"p\n".to_vec()),
            ("p.md".to_string(), b"# P\n".to_vec()),
        ];
        if let Some(r) = revision {
            files.push(("REVISION".into(), r.as_bytes().to_vec()));
        }
        Arc::new(Bundle::from_files(editor, source, files).unwrap())
    }

    const SHA: &str = "0123abcdef0123abcdef0123abcdef0123abcdef";

    /// Word built in with a commit, Excel installed without a REVISION,
    /// Project none.
    fn mixed() -> Vec<ManualRow> {
        manual_rows(|e| match e {
            Editor::Word => Some(bundle(e, Source::BuiltIn, Some(SHA))),
            Editor::Excel => Some(bundle(e, Source::Installed, None)),
            Editor::Project => None,
        })
    }

    #[test]
    fn manual_rows_say_each_editors_revision_and_source() {
        let rows = mixed();
        let got: Vec<(&str, String, &str)> = rows
            .iter()
            .map(|r| (r.name(), r.label(), r.value.as_str()))
            .collect();
        let built_in = format!("{SHA} (built in)");
        assert_eq!(
            got,
            [
                ("word manual", "Word manual".to_string(), built_in.as_str()),
                (
                    "excel manual",
                    "Excel manual".to_string(),
                    "unknown revision (installed)"
                ),
                ("project manual", "Project manual".to_string(), "none"),
            ]
        );
        // Installed with a commit; a blank REVISION is no revision.
        let rows = manual_rows(|e| match e {
            Editor::Word => Some(bundle(e, Source::Installed, Some("v2026.11\n"))),
            Editor::Excel => Some(bundle(e, Source::Installed, Some("\n"))),
            Editor::Project => Some(bundle(e, Source::BuiltIn, Some(""))),
        });
        let values: Vec<&str> = rows.iter().map(|r| r.value.as_str()).collect();
        assert_eq!(
            values,
            [
                "v2026.11 (installed)",
                "unknown revision (installed)",
                "unknown revision (built in)"
            ]
        );
        // No manual at all: every row says none.
        let rows = manual_rows(|_| None);
        assert!(rows.iter().all(|r| r.value == "none"), "{rows:?}");
    }

    #[test]
    fn manuals_json_gives_each_editors_revision_short_and_source() {
        let mut word = bundle(Editor::Word, Source::BuiltIn, Some(SHA));
        let j = manuals_json(|e| match e {
            Editor::Word => Some(word.clone()),
            Editor::Excel => Some(bundle(e, Source::Installed, None)),
            Editor::Project => None,
        });
        let w = j.get("word").unwrap();
        assert_eq!(w.get_str("revision"), Some(SHA));
        assert_eq!(w.get_str("short"), Some("0123abc"));
        assert_eq!(w.get_str("source"), Some("built-in"));
        let x = j.get("excel").unwrap();
        assert_eq!(x.get("revision"), Some(&Json::Null));
        assert_eq!(x.get("short"), Some(&Json::Null));
        assert_eq!(x.get_str("source"), Some("installed"));
        assert_eq!(j.get("project"), Some(&Json::Null));
        word = bundle(Editor::Word, Source::Installed, Some("v2026.11"));
        let j = manuals_json(|e| (e == Editor::Word).then(|| word.clone()));
        let w = j.get("word").unwrap();
        assert_eq!(w.get_str("short"), Some("v2026.11"));
        assert_eq!(w.get_str("source"), Some("installed"));
    }

    #[test]
    fn copy_text_is_the_version_block_plus_a_summary_and_the_manuals() {
        let i = info();
        let copy = copy_text(i, &mixed());
        assert!(copy.starts_with(&version_text(i)));
        let summary = format!("summary:     {}\n", i.short_line());
        let manuals = format!(
            "word manual:    {SHA} (built in)\n\
             excel manual:   unknown revision (installed)\n\
             project manual: none\n"
        );
        assert_eq!(copy, format!("{}{summary}{manuals}", version_text(i)));
    }

    /// `app-info` is the buildinfo JSON; the harness and normal control both parse it.
    #[test]
    fn app_info_json_parses_with_every_documented_key() {
        let j = ctlcore::json::Json::parse(&info().json()).expect("valid JSON");
        for k in [
            "version",
            "commit",
            "short_commit",
            "branch",
            "commit_date",
            "dirty",
            "last_pr",
            "issue",
            "ahead",
            "built_at",
            "profile",
            "target",
            "host",
            "kind",
            "manual",
            "summary",
            "commit_len",
            "commit_hex",
        ] {
            assert!(j.get(k).is_some(), "{k}");
        }
        assert_eq!(j.get_str("version"), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(
            j.get("manual").and_then(ctlcore::json::Json::as_bool),
            Some(info().manual())
        );
    }

    #[test]
    fn the_dialog_lists_every_field_with_copy_and_close() {
        let i = info();
        let d = dialog(i, &mixed());
        let names: Vec<&str> = d.controls.iter().map(|c| c.name).collect();
        for want in [
            "commit",
            "branch",
            "commit date",
            "last PR",
            "dirty",
            "built",
            "profile",
            "target",
            "host",
            "kind",
        ] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        // The marker is a row exactly when the build is manual.
        assert_eq!(names.contains(&"manual"), i.manual());
        // The manuals come last, after the build's rows (#1857).
        let last: Vec<(&str, &str, String)> = d.controls[d.controls.len() - 3..]
            .iter()
            .map(|c| {
                let Value::Text(t) = &c.value else {
                    panic!("{} is a label", c.name)
                };
                (c.name, c.label.as_str(), t.clone())
            })
            .collect();
        assert_eq!(
            last,
            [
                ("word manual", "Word manual", format!("{SHA} (built in)")),
                (
                    "excel manual",
                    "Excel manual",
                    "unknown revision (installed)".to_string()
                ),
                ("project manual", "Project manual", "none".to_string()),
            ]
        );
        assert!(
            d.controls[d.controls.len() - 3..]
                .iter()
                .all(|c| c.kind == ControlKind::Label)
        );
        let labels: Vec<&str> = d.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Copy", "Close"]);
        let mut stack = DialogStack::default();
        stack.push(d);
        // Enter and Escape both press Close; Copy leaves the dialog open.
        assert_eq!(stack.key_button("enter", true).as_deref(), Some("Close"));
        assert_eq!(stack.key_button("escape", true).as_deref(), Some("Close"));
        assert_eq!(click(&mut stack, "Copy"), Some(Ok(true)));
        assert!(stack.is_open(), "Copy leaves the dialog open");
        assert_eq!(click(&mut stack, "Close"), Some(Ok(false)));
        assert!(!stack.is_open());
        // Not About on top: not ours.
        assert_eq!(click(&mut stack, "Close"), None);
        let mut other = DialogStack::default();
        other.push(crate::user_name::dialog("a", "b"));
        assert_eq!(click(&mut other, "Cancel"), None);
    }
}
