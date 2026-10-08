//! Office's file keys on F12 (#1141): F12 is Save As, Shift+F12 is Save and
//! Ctrl+F12 (⌘F12 on a Mac) is Open, on every tab kind.
use super::*;

/// What a file key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileKey {
    SaveAs,
    Save,
    Open,
}

/// The status line when Ctrl+F12 meets a harness instance: Open would raise
/// the native dialog.
pub(crate) const OPEN_HARNESS: &str =
    "A harness instance cannot open the Open dialog; use the harness open verb";

/// The file key `k` is, if any. Any other modifier (Alt, Fn, a second of
/// Ctrl and Shift) makes it some other chord.
pub(crate) fn file_key(k: &Keystroke) -> Option<FileKey> {
    if k.key != "f12" {
        return None;
    }
    let m = &k.modifiers;
    if m.alt || m.function {
        return None;
    }
    match (m.control || m.platform, m.shift) {
        (false, false) => Some(FileKey::SaveAs),
        (false, true) => Some(FileKey::Save),
        (true, false) => Some(FileKey::Open),
        (true, true) => None,
    }
}

impl Docxy {
    /// Run a file key from `document_key`.
    pub(crate) fn file_key_act(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        self.keytips = KeyTip::Off;
        match key {
            FileKey::SaveAs => self.save_as(window, cx),
            FileKey::Save => {
                // Save is Ctrl+S: a document that is locked or shown as No
                // Markup / Original refuses it the way the document's Ctrl+S does.
                if !self.active_is_sheet()
                    && !self.active_is_project()
                    && (self.active_locked() || self.view_only_active())
                {
                    self.protected_refused(cx);
                    return;
                }
                self.save_active(window, cx)
            }
            FileKey::Open => {
                // ⚠️ Never in a harness instance: `rfd` would stop the
                // control pump (see `save_sheet_tab`).
                if self.harness {
                    self.set_status(OPEN_HARNESS);
                    cx.notify();
                    return self.refocus(window, cx);
                }
                self.open_file(window, cx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn f12_chords() {
        assert_eq!(file_key(&ks("f12")), Some(FileKey::SaveAs));
        assert_eq!(file_key(&ks("shift-f12")), Some(FileKey::Save));
        assert_eq!(file_key(&ks("ctrl-f12")), Some(FileKey::Open));
        assert_eq!(file_key(&ks("cmd-f12")), Some(FileKey::Open));
    }

    #[test]
    fn other_chords_are_not_file_keys() {
        for s in [
            "ctrl-shift-f12",
            "alt-f12",
            "fn-f12",
            "ctrl-alt-f12",
            "f11",
            "f1",
            "ctrl-s",
            "s",
        ] {
            assert_eq!(file_key(&ks(s)), None, "{s}");
        }
    }

    #[test]
    fn open_refusal_is_fixed_text() {
        assert!(OPEN_HARNESS.starts_with("A harness instance cannot open"));
    }
}
