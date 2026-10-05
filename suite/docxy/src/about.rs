//! The suite's build info (#1023): what `--version`, File > Account's About
//! dialog and its Copy button, `app-info` and the crash log say about this
//! binary. The commit, last merged PR and build kind come from the `buildinfo`
//! crate; this module names the product and shapes the text.

use crate::dialog::{
    ButtonRole, Control as Field, ControlKind, Dialog, DialogOwner, NONE_OPEN, Value,
};
use crate::{Context, Docxy, Window};
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
        "about",
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
    /// The Account page's About docxy suite button: the dialog on the active tab.
    pub(crate) fn open_about(&mut self) -> Result<(), String> {
        if !self.bs_account {
            return Err("the Account page is not open; use account open".into());
        }
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        if tab.dialogs.top()?.owner != DialogOwner::About {
            return None;
        }
        let done = tab.dialogs.click(button, |_| Ok(()));
        if done.is_ok() && button == "Copy" {
            self.clipboard_write(copy_text(info()), cx);
        }
        Some(done)
    }
}

/// `--version`: print the block and return, for `main` to exit. A release build
/// on Windows has no console, so it attaches to the parent's first.
pub(crate) fn print_version() {
    let text = version_text(info());
    #[cfg(windows)]
    {
        use std::io::Write;
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // ATTACH_PARENT_PROCESS
        // SAFETY: a plain Win32 call with no pointers.
        if unsafe { AttachConsole(u32::MAX) } != 0
            && let Ok(mut out) = std::fs::OpenOptions::new().write(true).open("CONOUT$")
        {
            let _ = out.write_all(text.as_bytes());
            return;
        }
    }
    print!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::DialogStack;

    #[test]
    fn the_suite_reports_its_own_version() {
        assert_eq!(info().version, env!("CARGO_PKG_VERSION"));
        assert!(
            version_text(info()).starts_with(&format!("{PRODUCT} {}\n", env!("CARGO_PKG_VERSION")))
        );
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
        stack.click("Copy", |_| Ok(())).unwrap();
        assert!(stack.is_open());
        stack.click("Close", |_| Ok(())).unwrap();
        assert!(!stack.is_open());
    }
}
