//! The suite's build info (#1023): what `--version`, File > Account's About
//! dialog and its Copy button, `app-info` and the crash log say about this
//! binary. The commit, last merged PR and build kind come from the `buildinfo`
//! crate; this module names the product and shapes the text.

use crate::dialog::catalog;
use crate::dialog::{
    ButtonRole, Control as Field, ControlKind, Dialog, DialogOwner, DialogStack, NONE_OPEN, Value,
};
use crate::{Context, Docxy};
use buildinfo::BuildInfo;

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

/// What the About dialog's Copy button puts on the clipboard: the `--version`
/// block plus the one-line summary, ready to paste into a bug report.
pub(crate) fn copy_text(info: &BuildInfo) -> String {
    format!("{}summary:     {}\n", version_text(info), info.short_line())
}

/// The About docxy suite dialog: every field of the build as a labelled row,
/// the "Manual build" marker when it is one, and Copy / Close. Enter and Escape
/// press Close.
pub(crate) fn dialog(info: &BuildInfo) -> Dialog {
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
        let tab = self.tabs.get_mut(self.active).ok_or(NONE_OPEN)?;
        tab.dialogs.push(dialog(info()));
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
                self.clipboard_write(copy_text(info()), cx);
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

    #[test]
    fn copy_text_is_the_version_block_plus_a_summary() {
        let i = info();
        let copy = copy_text(i);
        assert!(copy.starts_with(&version_text(i)));
        assert!(copy.contains(&i.short_line()));
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
        let d = dialog(i);
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
