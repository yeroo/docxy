//! Word's Manual Hyphenation (#1266): the Layout tab's Hyphenation > Manual.
//! It walks the words that may take an optional hyphen
//! (`docxcore::hyphenate`), one dialog each: the word in a "Hyphenate at:"
//! box with a `-` where one may go. Yes puts a soft hyphen there (the `-` may
//! be moved in the box first), No leaves the word, Cancel stops. A soft
//! hyphen is `w:softHyphen`, which Word draws only where the word breaks at a
//! line end; docxy's own layout does not hyphenate, so it is invisible here.
//!
//! The walk is every eligible word, not only those at a line end, as Word
//! proposes: wrapping differs per host. It runs from the caret, or over the
//! selection. The candidate is the editor's selection, so the dialog holds
//! only the box, the walk's end (`to`, a paragraph path and offset, empty for
//! the document's end) and the count added so far, both in hidden labels.
use crate::dialog::catalog;
use crate::dialog::{ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, Surface};
use docxcore::editor::{Editor, HyphenCandidate, Match};

/// Where a walk ends: a paragraph path and an editor offset.
type End = Option<(Vec<usize>, usize)>;

fn encode(end: &End) -> String {
    match end {
        None => String::new(),
        Some((path, off)) => {
            let path: Vec<String> = path.iter().map(usize::to_string).collect();
            format!("{}:{off}", path.join("."))
        }
    }
}

fn decode(text: &str) -> End {
    let (path, off) = text.split_once(':')?;
    let path = path
        .split('.')
        .map(|n| n.parse().ok())
        .collect::<Option<Vec<usize>>>()?;
    Some((path, off.parse().ok()?))
}

fn text_of(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

/// The dialog on `c`, which the walk ends at `to` and has added `added` for.
fn dialog(c: &HyphenCandidate, to: &End, added: usize) -> Dialog {
    let mut d = Dialog::message(
        catalog::MANUAL_HYPHENATION,
        "Manual Hyphenation",
        String::new(),
        &[
            ("Yes", ButtonRole::Accept),
            ("No", ButtonRole::Apply),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::ManualHyphen,
    );
    d.text = None;
    let shown: String = c
        .word
        .chars()
        .enumerate()
        .flat_map(|(i, ch)| (i == c.point).then_some('-').into_iter().chain([ch]))
        .collect();
    let hidden = |name, value: String| Control {
        visible: false,
        ..Control::new(name, "", ControlKind::Label, Value::Text(value))
    };
    d.controls = vec![
        Control::new(
            "at",
            "&Hyphenate at:",
            ControlKind::Text,
            Value::Text(shown),
        ),
        hidden("to", encode(to)),
        hidden("added", added.to_string()),
    ];
    d.mark_opened();
    d
}

fn editor(tab: &mut DocTab) -> Result<(&mut Editor, bool), String> {
    match (&mut tab.surface, tab.pkg.as_ref()) {
        (Surface::Doc(ed), Some(pkg)) => Ok((ed, pkg.hyphenate_caps())),
        (Surface::Doc(_), None) => Err("Manual hyphenation needs a .docx (not Markdown)".into()),
        _ => Err("Manual hyphenation needs a document".into()),
    }
}

fn plural(n: usize) -> String {
    match n {
        1 => "1 hyphen added".into(),
        n => format!("{n} hyphens added"),
    }
}

/// Look from `from` for the next word and ask about it, or finish the walk.
fn advance(tab: &mut DocTab, from: (Vec<usize>, usize), to: End, added: usize) {
    let found = match editor(tab) {
        Ok((ed, caps)) => {
            let limit = to.as_ref().map(|(p, o)| (p.as_slice(), *o));
            let found = ed.next_hyphen_candidate((&from.0, from.1), limit, caps);
            match &found {
                Some(c) => ed.select_match(&Match {
                    path: c.path.clone(),
                    start: c.start,
                    end: c.end,
                }),
                None => {
                    // The walk's end: the caret rests where it got to.
                    ed.anchor = None;
                    ed.set_caret(docxcore::editor::Caret {
                        path: from.0,
                        offset: from.1,
                    });
                }
            }
            found
        }
        Err(e) => {
            tab.status = e.into();
            return;
        }
    };
    match found {
        Some(c) => tab.dialogs.push(dialog(&c, &to, added)),
        None if added == 0 => tab.status = "Manual hyphenation complete".into(),
        None => {
            tab.status = format!("Manual hyphenation complete: {}", plural(added)).into();
        }
    }
}

/// Start the walk at the caret, or over the selection.
pub(crate) fn start(tab: &mut DocTab) -> Result<(), String> {
    if tab.access.locked() {
        return Err(tab.access.locked_status().into());
    }
    // The header or footer being edited has its own editor; the walk reads
    // the body.
    if tab.hf_edit.is_some() {
        return Err("Close the header or footer first".into());
    }
    let (ed, _) = editor(tab)?;
    let (from, to) = match ed.selection_range() {
        Some((lo, hi)) => ((lo.path, lo.offset), Some((hi.path, hi.offset))),
        None => ((ed.caret.path.clone(), ed.caret.offset), None),
    };
    advance(tab, from, to, 0);
    Ok(())
}

/// The press of Yes, No or Cancel on the Manual Hyphenation dialog; `None`
/// for any other dialog.
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if top.owner != DialogOwner::ManualHyphen {
        return None;
    }
    let want = button.replace('&', "");
    let Some(b) = top
        .buttons
        .iter()
        .find(|b| b.label.replace('&', "").eq_ignore_ascii_case(want.trim()))
    else {
        let labels: Vec<&str> = top.buttons.iter().map(|b| b.label.as_str()).collect();
        return Some(Err(format!(
            "no button '{button}'; buttons: {}",
            labels.join(", ")
        )));
    };
    if !b.enabled {
        return None;
    }
    // Yes accepts, No is the Apply button, and only the Cancel one stops.
    let role = b.role;
    let to = decode(&text_of(top, "to"));
    let added: usize = text_of(top, "added").parse().unwrap_or(0);
    let at = text_of(top, "at");
    Some(answer(tab, role, &at, to, added))
}

fn answer(
    tab: &mut DocTab,
    role: ButtonRole,
    at: &str,
    to: End,
    added: usize,
) -> Result<(), String> {
    let yes = role == ButtonRole::Accept;
    let no = role == ButtonRole::Apply;
    let (ed, _) = editor(tab)?;
    // The candidate is the selection; the caret ends it.
    let (path, start, end) = match ed.selection_range() {
        Some((lo, hi)) if lo.path == hi.path => (lo.path, lo.offset, hi.offset),
        _ => {
            tab.dialogs.pop();
            return Err("Manual hyphenation lost its word".into());
        }
    };
    if !yes && !no {
        // Cancel: stop where the walk is.
        ed.anchor = None;
        ed.set_caret(docxcore::editor::Caret { path, offset: end });
        tab.dialogs.pop();
        tab.status = format!("Manual hyphenation cancelled: {}", plural(added)).into();
        return Ok(());
    }
    let mut now = (path.clone(), end);
    let mut to = to;
    let mut added = added;
    if yes {
        let word = ed.selection_text();
        let at_chars: Vec<char> = at.chars().collect();
        let dashes = at_chars.iter().filter(|&&c| c == '-').count();
        let point = at_chars.iter().position(|&c| c == '-');
        let plain: String = at_chars.iter().filter(|&&c| c != '-').collect();
        let inside = point.is_some_and(|p| p > 0 && p < at_chars.len() - 1);
        if dashes != 1 || !inside || plain != word {
            return Err(format!(
                "Type \"{word}\" with one hyphen inside it, where it may be hyphenated"
            ));
        }
        let at = start + point.unwrap_or(0);
        if !ed.insert_soft_hyphen(&path, at) {
            tab.dialogs.pop();
            return Err("Manual hyphenation could not insert the hyphen".into());
        }
        tab.mark_dirty();
        added += 1;
        now.1 += 1;
        if let Some((p, o)) = &mut to {
            if *p == path {
                *o += 1;
            }
        }
    }
    tab.dialogs.pop();
    advance(tab, now, to, added);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::Dialog;
    use crate::layout_tab::tests::{ed, ed_mut};
    use docxcore::editor::{Caret, Editor};
    use docxcore::model::{Block, Inline, ParProps, Paragraph, Run};
    use std::path::PathBuf;

    fn para(text: &str) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: text.into(),
                ..Run::default()
            })],
        })
    }

    /// A .docx tab with one paragraph per text, the caret at the start.
    fn tab(paras: &[&str]) -> DocTab {
        let mut t = crate::tab_from_path(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
        );
        let e = ed_mut(&mut t);
        let mut doc = e.doc.clone();
        doc.body = paras.iter().map(|p| para(p)).collect();
        *e = Editor::new(doc);
        e.caret = Caret::top(0, 0);
        t
    }

    fn text(t: &DocTab, i: usize) -> String {
        match &ed(t).doc.body[i] {
            Block::Paragraph(p) => p.content.iter().map(Inline::text).collect(),
            _ => panic!(),
        }
    }

    fn shown(t: &DocTab) -> String {
        t.dialogs.top().unwrap().controls[0].text()
    }

    fn set_box(t: &mut DocTab, v: &str) {
        t.dialogs.top_dialog_mut().unwrap().controls[0].value = Value::Text(v.into());
    }

    fn press(t: &mut DocTab, b: &str) -> Result<(), String> {
        crate::dialog_host::dialog_click(t, b)
    }

    /// Criteria 1 and 2: the first word, the hyphen in the box, Yes then No
    /// then the walk's end.
    #[test]
    fn yes_no_and_the_end() {
        let mut t = tab(&["computer to hyphenation"]);
        start(&mut t).unwrap();
        let d: &Dialog = t.dialogs.top().unwrap();
        assert_eq!(d.title, "Manual Hyphenation");
        assert_eq!(d.id, catalog::MANUAL_HYPHENATION);
        assert_eq!(shown(&t), "com-puter");
        assert_eq!(ed(&t).selection_text(), "computer");
        press(&mut t, "Yes").unwrap();
        assert_eq!(text(&t, 0), "com\u{ad}puter to hyphenation");
        assert!(t.dirty);
        // The next word is asked about, the selection on it.
        assert_eq!(ed(&t).selection_text(), "hyphenation");
        assert_eq!(t.dialogs.depth(), 1);
        press(&mut t, "No").unwrap();
        assert_eq!(text(&t, 0), "com\u{ad}puter to hyphenation");
        assert!(!t.dialogs.is_open());
        assert_eq!(&*t.status, "Manual hyphenation complete: 1 hyphen added");
    }

    /// Criterion 2: Yes at the box's own `-` after the person moved it.
    #[test]
    fn the_edited_position_is_the_one_used() {
        let mut t = tab(&["computer"]);
        start(&mut t).unwrap();
        set_box(&mut t, "compu-ter");
        press(&mut t, "Yes").unwrap();
        assert_eq!(text(&t, 0), "compu\u{ad}ter");
    }

    /// Criterion 2: a box that is not the word with one inner `-` is refused
    /// and the dialog stays on the same word, with nothing written.
    #[test]
    fn a_bad_box_is_refused_and_the_dialog_stays() {
        for bad in [
            "computer",
            "com-pu-ter",
            "-computer",
            "computer-",
            "comp-uterx",
            "c-omputer-",
        ] {
            let mut t = tab(&["computer"]);
            start(&mut t).unwrap();
            set_box(&mut t, bad);
            let e = press(&mut t, "Yes").unwrap_err();
            assert!(e.contains("computer"), "{bad}: {e}");
            assert_eq!(t.dialogs.depth(), 1, "{bad}");
            assert_eq!(text(&t, 0), "computer", "{bad}");
            assert!(!t.dirty, "{bad}");
            assert_eq!(ed(&t).selection_text(), "computer", "{bad}");
        }
    }

    /// Criterion 2: Cancel (and Escape) stop and leave the text alone.
    #[test]
    fn cancel_and_escape_stop_the_walk() {
        let mut t = tab(&["computer hyphenation"]);
        start(&mut t).unwrap();
        press(&mut t, "Cancel").unwrap();
        assert!(!t.dialogs.is_open());
        assert_eq!(text(&t, 0), "computer hyphenation");
        assert!(!t.dirty);
        assert!(t.status.starts_with("Manual hyphenation cancelled"));
        start(&mut t).unwrap();
        assert!(crate::dialog_host::dialog_key(
            &mut t,
            "escape",
            None,
            Default::default()
        ));
        assert!(!t.dialogs.is_open());
    }

    /// Enter presses Yes.
    #[test]
    fn enter_presses_yes() {
        let mut t = tab(&["computer"]);
        start(&mut t).unwrap();
        crate::dialog_host::dialog_key(&mut t, "enter", None, Default::default());
        assert_eq!(text(&t, 0), "com\u{ad}puter");
    }

    /// Criterion 2: each Yes is one undo step.
    #[test]
    fn each_yes_is_one_undo_step() {
        let mut t = tab(&["computer hyphenation"]);
        start(&mut t).unwrap();
        press(&mut t, "Yes").unwrap();
        press(&mut t, "Yes").unwrap();
        assert!(text(&t, 0).matches('\u{ad}').count() == 2);
        assert!(ed_mut(&mut t).undo());
        assert_eq!(text(&t, 0).matches('\u{ad}').count(), 1);
        assert!(ed_mut(&mut t).undo());
        assert_eq!(text(&t, 0), "computer hyphenation");
    }

    /// Criterion 1: the walk starts at the caret, and over a selection it
    /// stops at the selection's end.
    #[test]
    fn it_starts_at_the_caret_and_honours_a_selection() {
        let mut t = tab(&["computer hyphenation wanted"]);
        ed_mut(&mut t).caret = Caret::top(0, 9);
        start(&mut t).unwrap();
        assert_eq!(ed(&t).selection_text(), "hyphenation");
        press(&mut t, "Cancel").unwrap();

        let mut t = tab(&["computer hyphenation wanted"]);
        let e = ed_mut(&mut t);
        e.anchor = Some(Caret::top(0, 0));
        e.caret = Caret::top(0, 20);
        start(&mut t).unwrap();
        press(&mut t, "No").unwrap();
        assert_eq!(ed(&t).selection_text(), "hyphenation");
        press(&mut t, "No").unwrap();
        // "wanted" lies past the selection: the walk is over.
        assert!(!t.dialogs.is_open());
        assert_eq!(&*t.status, "Manual hyphenation complete");
    }

    /// A Yes inside the selected paragraph moves the walk's end with the
    /// text, so a word at the end of the selection is still asked about.
    #[test]
    fn a_yes_keeps_the_selections_end_in_place() {
        let mut t = tab(&["computer hyphenation"]);
        let e = ed_mut(&mut t);
        e.anchor = Some(Caret::top(0, 0));
        e.caret = Caret::top(0, 20);
        start(&mut t).unwrap();
        press(&mut t, "Yes").unwrap();
        assert_eq!(ed(&t).selection_text(), "hyphenation");
    }

    /// Nothing to hyphenate says so and opens nothing.
    #[test]
    fn nothing_to_do_opens_nothing() {
        let mut t = tab(&["a few short words"]);
        start(&mut t).unwrap();
        assert!(!t.dialogs.is_open());
        assert_eq!(&*t.status, "Manual hyphenation complete");
    }

    /// Criterion 4: words in capitals follow the settings.
    #[test]
    fn capitals_follow_the_settings() {
        let mut t = tab(&["COMPUTER"]);
        start(&mut t).unwrap();
        assert!(t.dialogs.is_open());
        press(&mut t, "Cancel").unwrap();
        t.pkg.as_mut().unwrap().set_hyphenate_caps(false);
        start(&mut t).unwrap();
        assert!(!t.dialogs.is_open());
    }

    /// An unknown label is an error that leaves the dialog and the text alone,
    /// not a Cancel.
    #[test]
    fn an_unknown_button_is_an_error_not_a_cancel() {
        let mut t = tab(&["computer"]);
        start(&mut t).unwrap();
        for label in ["OK", "Yess", ""] {
            let e = press(&mut t, label).unwrap_err();
            assert!(e.contains("no button"), "{label}: {e}");
            assert_eq!(t.dialogs.depth(), 1, "{label}");
            assert_eq!(ed(&t).selection_text(), "computer", "{label}");
        }
        assert_eq!(text(&t, 0), "computer");
        // The accelerator spelling of a real one still works.
        press(&mut t, "&No").unwrap();
        assert!(!t.dialogs.is_open());
    }

    /// While a header or footer is being edited the walk would edit the
    /// hidden body: refused.
    #[test]
    fn it_refuses_while_a_header_or_footer_is_edited() {
        let mut t = tab(&["computer"]);
        t.hf_edit = Some(crate::HfEdit {
            editor: Editor::new(ed(&t).doc.clone()),
            part_name: "word/header1.xml".into(),
            is_header: true,
            section: 0,
            variant: docxcore::package::HeaderVariant::Default,
            show_text: true,
        });
        let e = start(&mut t).unwrap_err();
        assert_eq!(e, "Close the header or footer first");
        assert!(!t.dialogs.is_open());
    }

    /// Criterion 5: a locked document and Markdown refuse.
    #[test]
    fn locked_and_markdown_tabs_refuse() {
        let mut t = tab(&["computer"]);
        t.access.protected = true;
        let e = start(&mut t).unwrap_err();
        assert_eq!(e, t.access.locked_status());
        assert!(!t.dialogs.is_open());

        let mut t = tab(&["computer"]);
        t.pkg = None;
        assert!(start(&mut t).unwrap_err().contains("Markdown"));
    }

    /// Criterion 3: a save writes the element, and the text reloads the same.
    #[test]
    fn a_saved_hyphen_is_the_element() {
        let mut t = tab(&["computer"]);
        start(&mut t).unwrap();
        press(&mut t, "Yes").unwrap();
        let xml = docxcore::serialize::document_to_xml(&ed(&t).doc);
        assert!(xml.contains("<w:softHyphen/>"), "{xml}");
        assert!(!xml.contains('\u{ad}'), "{xml}");
    }

    #[test]
    fn the_walk_end_round_trips() {
        for end in [None, Some((vec![0], 5)), Some((vec![2, 1, 0], 0))] {
            assert_eq!(decode(&encode(&end)), end);
        }
    }
}
