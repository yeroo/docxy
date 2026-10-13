//! A document's paste choices (#753): Paste's menu and context menu (Keep
//! Source Formatting, Keep Text Only, Paste Special…) and the Paste Special
//! dialog Ctrl+Alt+V opens. Keep Source Formatting is Paste itself
//! (`Docxy::do_paste`); Keep Text Only pastes the clipboard's text as typing
//! at the caret would format it ([`docxcore::editor::Editor::paste_text_only`]).

use crate::dialog::catalog;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{ClipRead, DocClip, Docxy, doc_clip_ours, doc_paste_clip};
use docxcore::editor::Clip;
use gpui::{Context, Window};

/// The dialog's "As" choices, as Word names them.
pub(crate) const FORMATTED: &str = "Formatted Text (RTF)";
pub(crate) const UNFORMATTED: &str = "Unformatted Text";

/// Keep Text Only's status when the clipboard holds no text.
const NO_TEXT: &str = "There is no text to paste";

/// Whether the clipboard holds a formatted source: the document's own copy
/// while it is still what the clipboard holds, or an RTF the importer reads.
fn rich_source(ours: Option<&DocClip>, now: &ClipRead, rtf: Option<&[u8]>) -> bool {
    ours.is_some_and(|ours| doc_clip_ours(ours, now, rtf.is_some()))
        || rtf.is_some_and(|rtf| Clip::from_rtf(rtf, now.text()).is_some())
}

/// What Keep Text Only pastes: the clipboard's own text, else (our copy
/// with the clipboard unreadable, or an RTF with no text beside it) the
/// text of what Paste would paste. `None` when there is no text at all.
fn text_only_text(ours: Option<&DocClip>, now: &ClipRead, rtf: Option<&[u8]>) -> Option<String> {
    if let Some(text) = now.text() {
        return Some(text.to_string());
    }
    doc_paste_clip(ours, now, rtf)
        .map(|clip| clip.to_text())
        .filter(|text| !text.is_empty())
}

/// The "As" list: only the formats the clipboard holds, as Word's is.
/// `rich` when a formatted source is there, `text` when there is text (an
/// image-only copy of ours has none). Empty when there is nothing to paste.
fn as_items(rich: bool, text: bool) -> Vec<String> {
    let mut items = Vec::new();
    if rich {
        items.push(FORMATTED.to_string());
    }
    if text {
        items.push(UNFORMATTED.to_string());
    }
    items
}

/// Paste Special… in a document: the "As" radio over `items` (from
/// [`as_items`], the first chosen), OK and Cancel.
pub(crate) fn doc_paste_special_dialog(items: Vec<String>) -> Dialog {
    let mut d = Dialog::message(
        catalog::DOC_PASTE_SPECIAL,
        "Paste Special",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::DocPasteSpecial,
    );
    d.text = None;
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: b.label == "OK",
            ..b
        })
        .collect();
    let mut how = Control::new("as", "As", ControlKind::Radio, Value::Choice(Some(0)));
    how.items = items;
    d.controls = vec![how];
    d.mark_opened();
    d
}

/// Whether the dialog's "As" choice is Unformatted Text.
fn takes_text_only(d: &Dialog) -> bool {
    d.controls
        .iter()
        .find(|c| c.name == "as")
        .and_then(|c| match c.value {
            Value::Choice(Some(i)) => c.items.get(i),
            _ => None,
        })
        .is_some_and(|item| item == UNFORMATTED)
}

fn presses_ok(d: &Dialog, button: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case("OK")
        && d.buttons.iter().any(|b| b.label == "OK" && b.enabled)
}

impl Docxy {
    /// Keep Text Only: the clipboard's text, formatted as typing at the
    /// caret would be, as one undo step "Paste".
    pub(crate) fn do_paste_text_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_highlight_mode();
        if self.protected_refused(cx) {
            return self.refocus(window, cx);
        }
        let now = self.clipboard_read(cx);
        let rtf = self.clipboard_read_rtf();
        match text_only_text(self.clip.as_ref(), &now, rtf.as_deref()) {
            Some(text) => self.with_editor(window, cx, |e| e.paste_text_only(&text)),
            // Nothing, or only an image: said, not silently nothing (#753 r1).
            None => {
                self.set_status(NO_TEXT);
                self.refocus(window, cx);
            }
        }
    }

    /// Paste Special… (Ctrl+Alt+V): the dialog, when there is something to
    /// paste. OK reads the clipboard again; the dialog keeps only the choice.
    pub(crate) fn open_doc_paste_special(&mut self, cx: &mut Context<Self>) {
        if self.protected_refused(cx) {
            return;
        }
        let now = self.clipboard_read(cx);
        let rtf = self.clipboard_read_rtf();
        let ours = self.clip.as_ref();
        let items = as_items(
            rich_source(ours, &now, rtf.as_deref()),
            text_only_text(ours, &now, rtf.as_deref()).is_some(),
        );
        if items.is_empty() {
            self.set_status("There is nothing to paste");
            return cx.notify();
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.dialogs.push(doc_paste_special_dialog(items));
        }
        cx.notify();
    }

    /// The document Paste Special dialog's OK: closes it and pastes as the
    /// choice says. Its Cancel goes the tab's own way.
    pub(crate) fn doc_paste_dialog_click(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let top = self.tabs.get(self.active)?.dialogs.top()?;
        if !matches!(top.owner, DialogOwner::DocPasteSpecial) || !presses_ok(top, button) {
            return None;
        }
        let text_only = takes_text_only(top);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.dialogs.pop();
        }
        if text_only {
            self.do_paste_text_only(window, cx);
        } else {
            self.do_paste(window, cx);
        }
        Some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctlcore::json::Json;

    const BOLD_RTF: &[u8] = b"{\\rtf1\\ansi Plain {\\b BOLD}\\par}";

    fn text(s: &str) -> ClipRead {
        ClipRead::Text(s.into())
    }

    fn ours(s: &str) -> DocClip {
        DocClip {
            clip: Clip::from_text(s),
            text: s.into(),
        }
    }

    /// Formatted Text is offered only for our live copy or a readable RTF.
    #[test]
    fn a_rich_source_is_our_live_copy_or_an_rtf_753() {
        let mine = ours("mine");
        assert!(rich_source(Some(&mine), &text("mine"), None));
        // Another copy since: plain text from another app.
        assert!(!rich_source(Some(&mine), &text("other"), None));
        assert!(rich_source(None, &text("Plain BOLD"), Some(BOLD_RTF)));
        assert!(!rich_source(None, &text("words"), Some(b"\xff not rtf")));
        assert!(!rich_source(None, &text("plain"), None));
        assert!(!rich_source(None, &ClipRead::Nothing, None));
    }

    /// Keep Text Only pastes the clipboard's own text, never the RTF's.
    #[test]
    fn keep_text_only_takes_the_clipboards_text_753() {
        let mine = ours("mine");
        assert_eq!(
            text_only_text(Some(&mine), &text("mine"), None).as_deref(),
            Some("mine")
        );
        assert_eq!(
            text_only_text(None, &text("Plain BOLD\n"), Some(BOLD_RTF)).as_deref(),
            Some("Plain BOLD\n")
        );
        // An RTF with no text beside it: its own text.
        assert_eq!(
            text_only_text(None, &ClipRead::Nothing, Some(BOLD_RTF)).as_deref(),
            Some("Plain BOLD")
        );
        // Our copy while the clipboard cannot be read.
        assert_eq!(
            text_only_text(Some(&mine), &ClipRead::Nothing, None).as_deref(),
            Some("mine")
        );
        assert_eq!(text_only_text(None, &ClipRead::Nothing, None), None);
    }

    /// The "As" list holds only what the clipboard holds: with no formatted
    /// source Formatted Text is not there to choose.
    #[test]
    fn the_as_list_offers_only_what_the_clipboard_holds_753() {
        let rich = doc_paste_special_dialog(as_items(true, true));
        assert_eq!(rich.controls[0].items, [FORMATTED, UNFORMATTED]);
        assert!(!takes_text_only(&rich), "Formatted Text is the choice");
        let mut plain = doc_paste_special_dialog(as_items(false, true));
        assert_eq!(plain.controls[0].items, [UNFORMATTED]);
        assert!(takes_text_only(&plain));
        let err = plain
            .set(
                "as",
                &Json::obj(vec![("value", Json::Str(FORMATTED.into()))]),
            )
            .unwrap_err();
        assert!(err.contains("has no item"), "{err}");
        assert_eq!(plain.id, catalog::DOC_PASTE_SPECIAL);
        assert_eq!(plain.title, "Paste Special");
        assert!(as_items(false, false).is_empty(), "nothing to paste");
    }

    /// Our copy of only an image: its text is empty and the clipboard holds
    /// no text, so the clip is still ours and pastes, but there is no text
    /// for Unformatted Text or Keep Text Only (#753 r1).
    #[test]
    fn an_image_only_copy_offers_only_formatted_text_753() {
        let image_only = DocClip {
            clip: Clip::default(),
            text: String::new(),
        };
        let now = ClipRead::NotText;
        assert!(rich_source(Some(&image_only), &now, None));
        assert_eq!(text_only_text(Some(&image_only), &now, None), None);
        let items = as_items(
            rich_source(Some(&image_only), &now, None),
            text_only_text(Some(&image_only), &now, None).is_some(),
        );
        assert_eq!(items, [FORMATTED]);
    }
}
