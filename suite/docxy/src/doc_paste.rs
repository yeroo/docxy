//! A document's paste choices (#753): Paste's menu and context menu (Keep
//! Source Formatting, Keep Text Only, Paste Special…) and the Paste Special
//! dialog Ctrl+Alt+V opens. Keep Source Formatting is Paste itself
//! (`Docxy::do_paste`); Keep Text Only pastes the clipboard's text as typing
//! at the caret would format it ([`docxcore::editor::Editor::paste_text_only`]).

use crate::dialog::catalog;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{ClipRead, DocClip, Docxy, PasteFrom, doc_paste_clip, doc_paste_from};
use docxcore::editor::Clip;
use gpui::{Context, Window};

/// The dialog's "As" choices, as Word names them.
pub(crate) const FORMATTED: &str = "Formatted Text (RTF)";
/// The formatted choice when the source is another app's HTML (#1950).
pub(crate) const HTML_FORMAT: &str = "HTML Format";
pub(crate) const UNFORMATTED: &str = "Unformatted Text";

/// Keep Text Only's status when the clipboard holds no text.
const NO_TEXT: &str = "There is no text to paste";

/// Another app's pasted list items in the package's own lists (#1967,
/// #2087 r3): its importers name 1 (bullets) and 2 (decimal), which become
/// the lists Bullets and Numbering apply there ([`crate::ListIds`]: 1 and 2,
/// or docxy's reserved 9990 / 9991 where the package's numbering part
/// defines 1 or 2 as the other kind or leaves them undefined), and each kind
/// pasted is defined in the package ([`crate::ListIds::define`]), so the
/// view and a save show it. A clip without lists changes nothing; a tab
/// with no package keeps 1 and 2 and gets the part when its save builds one
/// with a list in it.
pub(crate) fn numbering_for_paste(pkg: Option<&mut docxcore::package::Package>, clip: &mut Clip) {
    let Some(pkg) = pkg.filter(|_| clip.has_lists()) else {
        return;
    };
    let ids = crate::ListIds::of_package(Some(pkg));
    let mut kinds = [false; 2];
    let lists = (0..clip.paras.len())
        .map(|i| {
            clip.list(i).map(|l| {
                let bullet = match l.num_id {
                    crate::NUM_BULLET => true,
                    crate::NUM_DECIMAL => false,
                    _ => return l,
                };
                kinds[usize::from(bullet)] = true;
                docxcore::editor::ClipList {
                    num_id: ids.id(bullet),
                    ..l
                }
            })
        })
        .collect();
    clip.set_lists(lists);
    for bullet in [false, true] {
        if kinds[usize::from(bullet)] {
            ids.define(Some(pkg), bullet);
        }
    }
}

/// The formatted source the clipboard holds, if any: the document's own
/// copy while it is still what the clipboard holds, an RTF the importer
/// reads, or else an HTML it reads (#1950). `html` reads the clipboard's
/// HTML, only when neither of the others is there.
fn rich_source(
    ours: Option<&DocClip>,
    now: &ClipRead,
    rtf: Option<&[u8]>,
    html: impl FnOnce() -> Option<String>,
) -> Option<PasteFrom> {
    doc_paste_from(ours, now, rtf, html)
        .map(|(from, _)| from)
        .filter(|from| *from != PasteFrom::Text)
}

/// What Keep Text Only pastes: the clipboard's own text, else (our copy
/// with the clipboard unreadable, or an RTF or HTML with no text beside it)
/// the text of what Paste would paste. `None` when there is no text at all.
fn text_only_text(
    ours: Option<&DocClip>,
    now: &ClipRead,
    rtf: Option<&[u8]>,
    html: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if let Some(text) = now.text() {
        return Some(text.to_string());
    }
    doc_paste_clip(ours, now, rtf, html)
        .map(|clip| clip.to_text())
        .filter(|text| !text.is_empty())
}

/// The "As" list: only the formats the clipboard holds, as Word's is.
/// `rich` is the formatted source there, if any: "HTML Format" for another
/// app's HTML (#1950), else "Formatted Text (RTF)". `text` when there is
/// text (an image-only copy of ours has none). Empty when there is nothing
/// to paste.
fn as_items(rich: Option<PasteFrom>, text: bool) -> Vec<String> {
    let mut items = Vec::new();
    match rich {
        Some(PasteFrom::Html) => items.push(HTML_FORMAT.to_string()),
        Some(_) => items.push(FORMATTED.to_string()),
        None => {}
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
        self.cancel_pointer_modes();
        if self.protected_refused(cx) {
            return self.refocus(window, cx);
        }
        let now = self.clipboard_read(cx);
        let rtf = self.clipboard_read_rtf();
        let html = || self.clipboard_read_html();
        match text_only_text(self.clip.as_ref(), &now, rtf.as_deref(), html) {
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
        // Read once, by whichever of the two needs it first.
        let read = std::cell::OnceCell::new();
        let html = || read.get_or_init(|| self.clipboard_read_html()).clone();
        let items = as_items(
            rich_source(ours, &now, rtf.as_deref(), html),
            text_only_text(ours, &now, rtf.as_deref(), html).is_some(),
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
    use docxcore::editor::Clip;

    const BOLD_RTF: &[u8] = b"{\\rtf1\\ansi Plain {\\b BOLD}\\par}";

    fn text(s: &str) -> ClipRead {
        ClipRead::Text(s.into())
    }

    fn ours(s: &str) -> DocClip {
        DocClip {
            media: Default::default(),
            clip: Clip::from_text(s),
            text: s.into(),
        }
    }

    /// Another app's list paste gives a package without a numbering part
    /// the one that defines numIds 1 and 2 (#1967); a numbering part of its
    /// own, a clip without lists or no package changes nothing.
    #[test]
    fn a_list_paste_gives_the_package_its_numbering_1967() {
        use docxcore::package::{Package, new_package};
        const NUMBERING: &str = "word/numbering.xml";
        let blank = || new_package(docxcore::model::Document { body: vec![] });
        let list = Clip::from_html("<ul><li>a</li></ul><ol><li>b</li></ol>", None).unwrap();
        assert!(list.has_lists());
        let mut pkg = blank();
        assert!(pkg.part(NUMBERING).is_none());
        let mut first = list.clone();
        numbering_for_paste(Some(&mut pkg), &mut first);
        // No part: docxy's own 1 and 2, kept.
        assert_eq!(first, list);
        let xml = pkg.part_text(NUMBERING).expect("a numbering part");
        assert!(
            xml.contains("w:numId=\"1\"") && xml.contains("w:numId=\"2\""),
            "{xml}"
        );
        assert!(
            pkg.part_text("word/_rels/document.xml.rels")
                .is_some_and(|r| r.contains("numbering.xml")),
            "the document part names it"
        );
        // Once: a second paste, or a package with its own part, keeps it.
        let parts = |p: &Package| {
            (
                p.part_text(NUMBERING),
                p.part_text("word/_rels/document.xml.rels"),
                p.part_text("[Content_Types].xml"),
            )
        };
        let before = parts(&pkg);
        let mut again = list.clone();
        numbering_for_paste(Some(&mut pkg), &mut again);
        assert_eq!(parts(&pkg), before);
        assert_eq!(again, list, "1 and 2 kept where the part defines them");
        // No lists: no part.
        let mut plain = blank();
        numbering_for_paste(Some(&mut plain), &mut Clip::from_text("a\nb"));
        assert!(plain.part(NUMBERING).is_none());
        let mut none = list.clone();
        numbering_for_paste(None, &mut none);
        assert_eq!(none, list);
    }

    /// The document's own copy carries no list membership (#2087 r3
    /// follow-up): `Editor::copy` makes a `Clip::new`, so a copied numId-1
    /// decimal item pasted back into a package whose 1 is decimal is left to
    /// the paste's own paragraph rules, its ids untouched, the package
    /// unchanged. Only another app's RTF or HTML (`set_lists`) is remapped.
    #[test]
    fn the_documents_own_copy_is_never_remapped_2087() {
        use docxcore::model::{Block, Document, Inline, ParProps, Paragraph, Run};
        use docxcore::package::new_package;
        const NUMBERING: &str = "word/numbering.xml";
        let item = Block::Paragraph(Paragraph {
            props: ParProps {
                num_id: Some(1),
                ..ParProps::default()
            },
            content: vec![Inline::Run(Run {
                text: "Plan".into(),
                props: Default::default(),
            })],
        });
        let doc = Document {
            body: vec![item.clone(), item],
        };
        let mut pkg = new_package(doc.clone());
        pkg.add_markdown_numbering();
        let decimal_one = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
        assert!(pkg.set_part(NUMBERING, decimal_one.as_bytes().to_vec()));
        let mut ed = docxcore::editor::Editor::new(doc);
        ed.select_all();
        let own = ed.copy().expect("a clip");
        assert!(!own.has_lists(), "an own copy names no lists");
        let mut clip = own.clone();
        let stamp = pkg.parts_stamp();
        numbering_for_paste(Some(&mut pkg), &mut clip);
        assert_eq!(clip, own);
        assert_eq!(pkg.parts_stamp(), stamp, "the package is not touched");
        assert_eq!(pkg.part_text(NUMBERING).as_deref(), Some(decimal_one));
    }

    /// A list pasted into a package whose numbering part defines 1 as
    /// decimal, or leaves 1 and 2 undefined (#2087 r3): its items take the
    /// lists Bullets and Numbering apply there, docxy's reserved 9990 /
    /// 9991, defined in the part, so the bullets draw as bullets.
    #[test]
    fn a_list_paste_takes_the_documents_lists_2087() {
        use docxcore::package::{BULLET_LIST_NUM_ID, NUMBER_LIST_NUM_ID, new_package};
        const NUMBERING: &str = "word/numbering.xml";
        let list = Clip::from_html("<ul><li>a</li></ul><ol><li>b</li></ol>", None).unwrap();
        let kinds = |c: &Clip| (c.list(0).map(|l| l.num_id), c.list(1).map(|l| l.num_id));
        assert_eq!(kinds(&list), (Some(1), Some(2)));
        for part in [
            // numId 1 decimal, 2 undefined: the dark-mode fixture's shape.
            r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
            // Neither defined.
            r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"></w:numbering>"#,
        ] {
            let mut pkg = new_package(docxcore::model::Document { body: vec![] });
            pkg.add_markdown_numbering();
            assert!(pkg.set_part(NUMBERING, part.as_bytes().to_vec()));
            let mut clip = list.clone();
            numbering_for_paste(Some(&mut pkg), &mut clip);
            assert_eq!(
                kinds(&clip),
                (Some(BULLET_LIST_NUM_ID), Some(NUMBER_LIST_NUM_ID)),
                "{part}"
            );
            let numbering =
                docxcore::numbering::parse_numbering_xml(&pkg.part_text(NUMBERING).unwrap());
            assert!(numbering.defines(BULLET_LIST_NUM_ID), "{part}");
            assert!(numbering.defines(NUMBER_LIST_NUM_ID), "{part}");
            assert_eq!(
                crate::ListIds::of(Some(&numbering)).bullet,
                BULLET_LIST_NUM_ID
            );
        }
    }

    /// Formatted Text is offered only for our live copy or a readable RTF.
    #[test]
    fn a_rich_source_is_our_live_copy_or_an_rtf_753() {
        let mine = ours("mine");
        assert!(rich_source(Some(&mine), &text("mine"), None, || None).is_some());
        // Another copy since: plain text from another app.
        assert!(rich_source(Some(&mine), &text("other"), None, || None).is_none());
        assert!(rich_source(None, &text("Plain BOLD"), Some(BOLD_RTF), || None).is_some());
        assert!(rich_source(None, &text("words"), Some(b"\xff not rtf"), || None).is_none());
        assert!(rich_source(None, &text("plain"), None, || None).is_none());
        assert!(rich_source(None, &ClipRead::Nothing, None, || None).is_none());
    }

    /// Keep Text Only pastes the clipboard's own text, never the RTF's.
    #[test]
    fn keep_text_only_takes_the_clipboards_text_753() {
        let mine = ours("mine");
        assert_eq!(
            text_only_text(Some(&mine), &text("mine"), None, || None).as_deref(),
            Some("mine")
        );
        assert_eq!(
            text_only_text(None, &text("Plain BOLD\n"), Some(BOLD_RTF), || None).as_deref(),
            Some("Plain BOLD\n")
        );
        // An RTF with no text beside it: its own text.
        assert_eq!(
            text_only_text(None, &ClipRead::Nothing, Some(BOLD_RTF), || None).as_deref(),
            Some("Plain BOLD")
        );
        // Our copy while the clipboard cannot be read.
        assert_eq!(
            text_only_text(Some(&mine), &ClipRead::Nothing, None, || None).as_deref(),
            Some("mine")
        );
        assert_eq!(
            text_only_text(None, &ClipRead::Nothing, None, || None),
            None
        );
    }

    /// The "As" list holds only what the clipboard holds: with no formatted
    /// source Formatted Text is not there to choose.
    #[test]
    fn the_as_list_offers_only_what_the_clipboard_holds_753() {
        let rich = doc_paste_special_dialog(as_items(Some(PasteFrom::Rtf), true));
        assert_eq!(rich.controls[0].items, [FORMATTED, UNFORMATTED]);
        assert!(!takes_text_only(&rich), "Formatted Text is the choice");
        let mut plain = doc_paste_special_dialog(as_items(None, true));
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
        assert!(as_items(None, false).is_empty(), "nothing to paste");
    }

    /// Another app's HTML with no RTF (a browser's copy, #1950) is a
    /// formatted source offered as "HTML Format", never as "Formatted Text
    /// (RTF)"; an RTF or our own copy beside it keeps "Formatted Text (RTF)"
    /// and the HTML is not read; HTML with no text is no source.
    #[test]
    fn an_html_source_is_offered_as_html_format_1950() {
        let html = || Some("<p>Plain <b>BOLD</b></p>".to_string());
        let unread = || -> Option<String> { panic!("the HTML is not read") };
        let now = text("Plain BOLD");
        assert_eq!(rich_source(None, &now, None, html), Some(PasteFrom::Html));
        let items = as_items(rich_source(None, &now, None, html), true);
        assert_eq!(items, [HTML_FORMAT, UNFORMATTED]);
        let mut d = doc_paste_special_dialog(items);
        assert!(!takes_text_only(&d), "HTML Format pastes formatted");
        let err = d
            .set(
                "as",
                &Json::obj(vec![("value", Json::Str(FORMATTED.into()))]),
            )
            .unwrap_err();
        assert!(err.contains("has no item"), "{err}");
        assert_eq!(
            rich_source(None, &now, Some(BOLD_RTF), unread),
            Some(PasteFrom::Rtf)
        );
        let mine = ours("mine");
        assert_eq!(
            as_items(rich_source(Some(&mine), &text("mine"), None, unread), true),
            [FORMATTED, UNFORMATTED]
        );
        assert_eq!(
            rich_source(None, &text("words"), None, || Some("<img src=x>".into())),
            None
        );
        // Keep Text Only takes the clipboard's text, never the HTML's; with
        // no text beside it, the HTML's own text.
        assert_eq!(
            text_only_text(None, &now, None, unread).as_deref(),
            Some("Plain BOLD")
        );
        assert_eq!(
            text_only_text(None, &ClipRead::Nothing, None, html).as_deref(),
            Some("Plain BOLD")
        );
    }

    /// Our copy of only an image: its text is empty and the clipboard holds
    /// no text, so the clip is still ours and pastes, but there is no text
    /// for Unformatted Text or Keep Text Only (#753 r1).
    #[test]
    fn an_image_only_copy_offers_only_formatted_text_753() {
        let image_only = DocClip {
            media: Default::default(),
            clip: Clip::default(),
            text: String::new(),
        };
        let now = ClipRead::NotText;
        assert!(rich_source(Some(&image_only), &now, None, || None).is_some());
        assert_eq!(text_only_text(Some(&image_only), &now, None, || None), None);
        let items = as_items(
            rich_source(Some(&image_only), &now, None, || None),
            text_only_text(Some(&image_only), &now, None, || None).is_some(),
        );
        assert_eq!(items, [FORMATTED]);
    }
}
