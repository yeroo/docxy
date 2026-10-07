//! Typing with AutoCorrect and AutoFormat As You Type (#856): each
//! correction is an undo step of its own, after the typing step, so the first
//! Ctrl+Z takes back the correction and keeps what was typed, as in Word.

use super::{EditKind, Editor, editor_text, inline_len, para_mut, resolve_para};
use crate::autocorrect::{
    AUTOFORMAT, AutoCorrectOptions, Fix, ListKind, WordFixes, auto_list, fixes_for,
};
use crate::model::{Inline, Run, RunProps};

impl Editor {
    /// Type `ch` from the keyboard, then make the corrections it triggers
    /// ([`crate::autocorrect`]), each as its own named undo step: typing goes
    /// on after them as a new step. `list` gives the numbering id of the list
    /// an automatic list starts (`None` declines it, as a host does when it
    /// may not change formatting). Returns how many corrections it made.
    ///
    /// Only a plain key corrects: nothing happens after typing over a
    /// selection, while Track Changes is on, when the key did not type (a
    /// field's stand-in), or when the text to correct is anything but plain
    /// run text (a field, a hyperlink, a tab or break, a tracked change).
    pub fn type_autocorrected(
        &mut self,
        ch: char,
        opts: &AutoCorrectOptions,
        fixes: &dyn WordFixes,
        list: &mut dyn FnMut(ListKind) -> Option<i32>,
    ) -> usize {
        let before = self.caret.clone();
        let plain = !self.has_selection() && !self.track_changes() && self.grouping.is_none();
        self.insert_char(ch);
        let typed = plain
            && self.caret.path == before.path
            && self.caret.offset == before.offset + 1
            && self.last_typed().is_some_and(|t| t.ends_with(ch));
        if !typed {
            return 0;
        }
        let path = self.caret.path.clone();
        let Some(p) = resolve_para(&self.doc.body, &path) else {
            return 0;
        };
        let text: Vec<char> = editor_text(&p.content).chars().collect();
        let caret = self.caret.offset;
        if let Some(kind) = auto_list(&text, caret, ch, p.props.num_id.is_some(), opts) {
            if !plain_text(&p.content, 0, text.len()) {
                return 0;
            }
            let Some(num_id) = list(kind) else {
                return 0;
            };
            // The item's text goes on in the typing's formatting (Bold on,
            // `1. `: the item is bold), held by an empty run where the
            // text was, which the next character typed joins.
            let (at, props) = first_run(&p.content);
            self.one_step(AUTOFORMAT, |ed| {
                ed.checkpoint(EditKind::Structural);
                ed.replace_text_range(&path, 0, text.len(), "");
                if let Some(p) = para_mut(&mut ed.doc.body, &path) {
                    p.props.num_id = Some(num_id);
                    p.props.ilvl = 0;
                    let at = at.min(p.content.len());
                    p.content.insert(
                        at,
                        Inline::Run(Run {
                            text: String::new(),
                            props,
                        }),
                    );
                }
                ed.caret.offset = 0;
            });
            return 1;
        }
        let mut made = 0;
        for fix in fixes_for(&text, caret, ch, opts, fixes) {
            // The fixes after one that cannot be made were worked out on
            // its result: stop there.
            if !self.apply_fix(&path, &fix) {
                break;
            }
            made += 1;
        }
        made
    }

    /// Make `fix` in the paragraph at `path` as one undo step named for it;
    /// false, with nothing changed, when its text is not plain run text.
    fn apply_fix(&mut self, path: &[usize], fix: &Fix) -> bool {
        let ok = resolve_para(&self.doc.body, path)
            .is_some_and(|p| plain_text(&p.content, fix.start, fix.end));
        if !ok {
            return false;
        }
        let removed = fix.end - fix.start;
        let added = fix.with.chars().count();
        self.one_step(fix.name, |ed| {
            ed.checkpoint(EditKind::Structural);
            ed.replace_text_range(path, fix.start, fix.end, &fix.with);
            if ed.caret.offset >= fix.end {
                ed.caret.offset = ed.caret.offset - removed + added;
            }
        });
        true
    }
}

/// Where the first run of `content` is, and its formatting.
fn first_run(content: &[Inline]) -> (usize, RunProps) {
    content
        .iter()
        .enumerate()
        .find_map(|(i, inline)| match inline {
            Inline::Run(r) => Some((i, r.props.clone())),
            _ => None,
        })
        .unwrap_or_default()
}

/// Whether editor offsets `start..end` of `content` are all text of plain,
/// unrecorded runs, with nothing zero-width (a tracked deletion, a bookmark)
/// between them.
fn plain_text(content: &[Inline], start: usize, end: usize) -> bool {
    let mut at = 0;
    for inline in content {
        let len = inline_len(inline);
        if len == 0 {
            if start < at && at < end {
                return false;
            }
            continue;
        }
        if at < end && start < at + len {
            match inline {
                Inline::Run(r) if r.props.tracked_insert.is_none() => {}
                _ => return false,
            }
        }
        at += len;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::{Caret, FIELD_CHAR, TrackAuthor};
    use super::*;
    use crate::autocorrect::{AUTOCORRECT, BuiltinFixes};
    use crate::model::{
        Block, Document, Hyperlink, ParProps, Paragraph, RevisionKind, RevisionMetadata,
    };

    fn editor(content: Vec<Inline>) -> Editor {
        let len = content.iter().map(inline_len).sum();
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content,
            })],
        });
        ed.caret = Caret::at(vec![0], len);
        ed
    }

    fn empty() -> Editor {
        editor(Vec::new())
    }

    fn run(text: &str) -> Inline {
        Inline::Run(Run {
            text: text.into(),
            props: RunProps::default(),
        })
    }

    fn para(ed: &Editor) -> &Paragraph {
        match &ed.doc.body[0] {
            Block::Paragraph(p) => p,
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    fn text(ed: &Editor) -> String {
        editor_text(&para(ed).content)
    }

    /// Lists for the automatic list: 1 for bullets, 2 for numbers.
    fn lists(kind: ListKind) -> Option<i32> {
        Some(match kind {
            ListKind::Bullet => 1,
            ListKind::Decimal => 2,
        })
    }

    fn type_with(ed: &mut Editor, s: &str, opts: &AutoCorrectOptions) {
        for ch in s.chars() {
            ed.type_autocorrected(ch, opts, &BuiltinFixes, &mut lists);
        }
    }

    fn type_keys(ed: &mut Editor, s: &str) {
        type_with(ed, s, &AutoCorrectOptions::default());
    }

    /// The text after each Undo until there is nothing left to undo.
    fn undos(ed: &mut Editor) -> Vec<String> {
        let mut out = Vec::new();
        while ed.undo() {
            out.push(text(ed));
        }
        out
    }

    #[test]
    fn teh_at_the_start_is_two_corrections_after_the_typing_856() {
        let mut ed = empty();
        type_keys(&mut ed, "teh ");
        assert_eq!(text(&ed), "The ");
        assert_eq!(ed.caret.offset, 4);
        assert_eq!(
            ed.undo_names(),
            [AUTOCORRECT, AUTOCORRECT, "Typing \"teh \""]
        );
        assert_eq!(undos(&mut ed), ["the ", "teh ", ""]);
    }

    #[test]
    fn typing_after_a_correction_is_a_step_of_its_own_856() {
        let mut ed = empty();
        type_keys(&mut ed, "Then teh ");
        assert_eq!(text(&ed), "Then the ");
        type_keys(&mut ed, "x");
        assert!(ed.undo());
        assert_eq!(text(&ed), "Then the ");
        assert!(ed.undo());
        assert_eq!(text(&ed), "Then teh ");
        assert!(ed.undo());
        assert_eq!(text(&ed), "");
    }

    #[test]
    fn a_symbol_corrects_on_its_last_key_856() {
        let mut ed = empty();
        type_keys(&mut ed, "Mark (c) ");
        assert_eq!(text(&ed), "Mark \u{a9} ");
        assert_eq!(ed.undo_names()[1], AUTOCORRECT);
        let back = undos(&mut ed);
        assert_eq!(back[..2], ["Mark \u{a9}", "Mark (c)"]);
    }

    #[test]
    fn each_quote_is_its_own_autoformat_step_856() {
        let mut ed = empty();
        type_keys(&mut ed, "He said \"hi\" ok");
        assert_eq!(text(&ed), "He said “hi” ok");
        assert_eq!(
            ed.undo_names()[..4],
            ["Typing \" ok\"", AUTOFORMAT, "Typing \"hi\"\"", AUTOFORMAT]
        );
        let back = undos(&mut ed);
        assert_eq!(
            back[..4],
            ["He said “hi”", "He said “hi\"", "He said “", "He said \""]
        );
    }

    #[test]
    fn one_space_starts_an_automatic_list_856() {
        let mut ed = empty();
        type_keys(&mut ed, "1. ");
        assert_eq!(text(&ed), "");
        assert_eq!(para(&ed).props.num_id, Some(2));
        assert_eq!(ed.caret.offset, 0);
        assert_eq!(ed.undo_names()[0], AUTOFORMAT);
        assert!(ed.undo());
        assert_eq!(text(&ed), "1. ");
        assert_eq!(para(&ed).props.num_id, None);

        let mut ed = empty();
        type_keys(&mut ed, "* ");
        assert_eq!((text(&ed).as_str(), para(&ed).props.num_id), ("", Some(1)));
    }

    #[test]
    fn an_automatic_list_keeps_the_typing_formatting_856() {
        let mut ed = empty();
        ed.toggle_bold();
        type_keys(&mut ed, "1. Item");
        assert_eq!(para(&ed).props.num_id, Some(2));
        assert_eq!(text(&ed), "Item");
        let bold: Vec<_> = para(&ed)
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) if !r.text.is_empty() => Some((r.text.as_str(), r.props.bold)),
                _ => None,
            })
            .collect();
        assert_eq!(bold, [("Item", true)]);
    }

    #[test]
    fn a_with_holding_a_field_stand_in_keeps_the_caret_in_step_856() {
        struct FieldWith;
        impl WordFixes for FieldWith {
            fn replacement(&self, token: &str) -> Option<String> {
                (token == "xx").then(|| format!("{FIELD_CHAR}ab"))
            }
            fn first_letter_exception(&self, _: &str) -> bool {
                false
            }
        }
        let mut ed = empty();
        for ch in "xx ".chars() {
            ed.type_autocorrected(ch, &AutoCorrectOptions::default(), &FieldWith, &mut lists);
        }
        assert_eq!(text(&ed), "Ab ");
        assert_eq!(ed.caret.offset, 3);
    }

    #[test]
    fn a_declined_list_leaves_the_typing_856() {
        let mut ed = empty();
        for ch in "1. ".chars() {
            ed.type_autocorrected(
                ch,
                &AutoCorrectOptions::default(),
                &BuiltinFixes,
                &mut |_| None,
            );
        }
        assert_eq!(text(&ed), "1. ");
        assert_eq!(para(&ed).props.num_id, None);
        assert_eq!(ed.undo_names(), ["Typing \"1. \""]);
    }

    #[test]
    fn two_hyphens_become_an_em_dash_856() {
        let mut ed = empty();
        type_keys(&mut ed, "One--two ");
        assert_eq!(text(&ed), "One\u{2014}two ");
        assert_eq!(ed.undo_names()[0], AUTOFORMAT);
        assert!(ed.undo());
        assert_eq!(text(&ed), "One--two ");
    }

    #[test]
    fn redo_brings_the_correction_back_856() {
        let mut ed = empty();
        type_keys(&mut ed, "Then teh ");
        assert!(ed.undo());
        assert_eq!(text(&ed), "Then teh ");
        assert!(ed.redo());
        assert_eq!(text(&ed), "Then the ");
        // Straight after a correction there is nothing to redo.
        let mut ed = empty();
        type_keys(&mut ed, "Then teh ");
        assert!(!ed.can_redo());
        assert!(!ed.redo());
    }

    #[test]
    fn nothing_corrects_after_typing_over_a_selection_856() {
        let mut ed = editor(vec![run("Then teh")]);
        ed.anchor = Some(Caret::at(vec![0], 7));
        ed.caret = Caret::at(vec![0], 8);
        // `h` over the selected `h`, then a space that would correct.
        type_keys(&mut ed, "h");
        assert_eq!(text(&ed), "Then teh");
        // The space after it is plain typing again, and corrects.
        type_keys(&mut ed, " ");
        assert_eq!(text(&ed), "Then the ");
        // A space over the `x` of `tehx` would end `teh` where the caret
        // lands, but typing over a selection corrects nothing.
        let mut ed = editor(vec![run("tehx")]);
        ed.anchor = Some(Caret::at(vec![0], 4));
        ed.caret = Caret::at(vec![0], 3);
        type_keys(&mut ed, " ");
        assert_eq!(text(&ed), "teh ");
    }

    #[test]
    fn nothing_corrects_with_track_changes_on_856() {
        let mut ed = empty();
        ed.set_track_changes(Some(TrackAuthor {
            author: "A".into(),
            clock: || "2026-10-07T00:00:00Z".into(),
        }));
        type_keys(&mut ed, "teh \"x\" 1. ");
        assert_eq!(text(&ed), "teh \"x\" 1. ");
        // Text that was there before is not replaced as a tracked change
        // either: only the typed space is recorded.
        let mut ed = editor(vec![run("teh")]);
        ed.set_track_changes(Some(TrackAuthor {
            author: "A".into(),
            clock: || "2026-10-07T00:00:00Z".into(),
        }));
        type_keys(&mut ed, " ");
        assert_eq!(text(&ed), "teh ");
        let recorded: Vec<_> = para(&ed)
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some((r.text.as_str(), r.props.tracked_insert.is_some())),
                Inline::Revision { .. } => Some(("<revision>", true)),
                _ => None,
            })
            .collect();
        assert_eq!(recorded, [("teh", false), (" ", true)]);
    }

    #[test]
    fn each_switch_off_leaves_its_typing_856() {
        type Off = fn(&mut AutoCorrectOptions);
        let cases: [(&str, Off, &str); 5] = [
            ("teh ", |o| o.replace_text = false, "Teh "),
            ("teh ", |o| o.capitalize_sentences = false, "the "),
            ("\"x\"", |o| o.smart_quotes = false, "\"x\""),
            ("One--two ", |o| o.dashes = false, "One--two "),
            ("1. ", |o| o.auto_lists = false, "1. "),
        ];
        for (typed, off, want) in cases {
            let mut opts = AutoCorrectOptions::default();
            off(&mut opts);
            let mut ed = empty();
            type_with(&mut ed, typed, &opts);
            assert_eq!(text(&ed), want, "{typed}");
        }
    }

    #[test]
    fn no_capital_after_an_exception_and_no_list_from_other_text_856() {
        let mut ed = empty();
        type_keys(&mut ed, "See e.g. teh ");
        assert_eq!(text(&ed), "See e.g. the ");
        let mut ed = empty();
        type_keys(&mut ed, "1.5 ");
        assert_eq!((text(&ed).as_str(), para(&ed).props.num_id), ("1.5 ", None));
        // Already in a list: `1. ` stays text.
        let mut ed = empty();
        if let Block::Paragraph(p) = &mut ed.doc.body[0] {
            p.props.num_id = Some(2);
        }
        type_keys(&mut ed, "1. ");
        assert_eq!(
            (text(&ed).as_str(), para(&ed).props.num_id),
            ("1. ", Some(2))
        );
    }

    #[test]
    fn a_correction_keeps_the_run_formatting_856() {
        let bold = RunProps {
            bold: true,
            ..Default::default()
        };
        let mut ed = editor(vec![Inline::Run(Run {
            text: "teh".into(),
            props: bold.clone(),
        })]);
        type_keys(&mut ed, " ");
        assert_eq!(text(&ed), "The ");
        let runs: Vec<_> = para(&ed)
            .content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => (r.text.as_str(), r.props.bold),
                other => panic!("expected a run, got {other:?}"),
            })
            .collect();
        assert_eq!(runs, [("The ", true)]);
    }

    #[test]
    fn a_word_across_a_tracked_change_or_in_a_link_is_left_alone_856() {
        // `te`, a tracked deletion (zero-width in the editor), `h`: the
        // editor reads `teh`, which is not one plain word.
        let deleted = Inline::Revision {
            kind: RevisionKind::Delete,
            metadata: RevisionMetadata::default(),
            raw: String::new(),
            content: vec![run("x")],
            content_changed: false,
        };
        let mut ed = editor(vec![run("So te"), deleted, run("h")]);
        type_keys(&mut ed, " ");
        assert_eq!(text(&ed), "So teh ");
        assert_eq!(ed.undo_names().len(), 1, "the typing only");
        let link = Inline::Hyperlink(Hyperlink {
            target: Some("https://example.com".into()),
            runs: vec![Run {
                text: "teh".into(),
                props: RunProps::default(),
            }],
            ..Default::default()
        });
        let mut ed = editor(vec![run("so "), link]);
        type_keys(&mut ed, " ");
        assert_eq!(text(&ed), "so teh ");
    }
}
