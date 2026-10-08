//! AutoCorrect and AutoFormat As You Type in the Word tab (#856).
//!
//! Office keeps one AutoCorrect list for all its applications, so the Word
//! tab reads the app's (the one the Sheet tabs use, #667, edited in Settings ›
//! AutoCorrect Options): its replace list, its First Letter exceptions and
//! its Replace text and Capitalize first letter switches. The AutoFormat
//! rules (smart quotes, em dashes, automatic lists) have no switches here
//! yet and keep Word's defaults.

use docxcore::autocorrect::{AutoCorrectOptions, ListKind, WordFixes};
use docxcore::editor::Editor;
use gridcore::autocorrect::AutoCorrect;

/// The app's list as the Word editor's word rules.
struct Shared<'a>(&'a AutoCorrect);

impl WordFixes for Shared<'_> {
    fn replacement(&self, token: &str) -> Option<String> {
        self.0.replacement(token)
    }

    fn first_letter_exception(&self, word: &str) -> bool {
        self.0.is_first_letter_exception(word)
    }
}

/// The Word editor's switches from the app's.
fn options(ac: &AutoCorrect) -> AutoCorrectOptions {
    AutoCorrectOptions {
        replace_text: ac.opts.replace_text,
        capitalize_sentences: ac.opts.first_letter,
        ..AutoCorrectOptions::default()
    }
}

/// Type one key into the Word editor with the app's AutoCorrect.
pub(crate) fn type_key(ed: &mut Editor, ac: &AutoCorrect, ch: char) {
    ed.type_autocorrected(ch, &options(ac), &Shared(ac), &mut |kind| {
        Some(match kind {
            ListKind::Bullet => crate::NUM_BULLET,
            ListKind::Decimal => crate::NUM_DECIMAL,
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::{Block, Document, Paragraph};

    /// An editor on one empty paragraph.
    fn empty() -> Editor {
        Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        })
    }

    fn text(ed: &Editor) -> String {
        match &ed.doc.body[0] {
            Block::Paragraph(p) => p.plain_text(),
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    fn typed(ac: &AutoCorrect, s: &str) -> String {
        let mut ed = empty();
        for ch in s.chars() {
            type_key(&mut ed, ac, ch);
        }
        text(&ed)
    }

    #[test]
    fn the_word_tab_reads_the_apps_list_and_switches_856() {
        let ac = AutoCorrect::default();
        assert_eq!(typed(&ac, "teh "), "The ");

        let mut edited = AutoCorrect::default();
        assert!(edited.delete("teh"));
        edited.add("wrod", "word").unwrap();
        assert_eq!(typed(&edited, "Then teh wrod "), "Then teh word ");

        let mut off = AutoCorrect::default();
        off.opts.replace_text = false;
        assert_eq!(typed(&off, "teh "), "Teh ");

        let mut no_caps = AutoCorrect::default();
        no_caps.opts.first_letter = false;
        assert_eq!(typed(&no_caps, "teh "), "the ");
    }

    #[test]
    fn the_apps_first_letter_exceptions_hold_in_the_word_tab_856() {
        use gridcore::autocorrect::ExceptionKind;
        let mut ac = AutoCorrect::default();
        assert_eq!(typed(&ac, "Call me. now "), "Call me. Now ");
        ac.add_exception(ExceptionKind::FirstLetter, "me.").unwrap();
        assert_eq!(typed(&ac, "Call me. now "), "Call me. now ");
    }

    #[test]
    fn an_automatic_list_uses_the_tabs_lists_856() {
        let ac = AutoCorrect::default();
        let mut ed = empty();
        for ch in "* ".chars() {
            type_key(&mut ed, &ac, ch);
        }
        assert!(ed.all_in_list(crate::NUM_BULLET));
        let mut ed = empty();
        for ch in "1. ".chars() {
            type_key(&mut ed, &ac, ch);
        }
        assert!(ed.all_in_list(crate::NUM_DECIMAL));
        // Word also starts a list on a hyphen and on `1)` (#1080).
        let mut ed = empty();
        for ch in "- foo".chars() {
            type_key(&mut ed, &ac, ch);
        }
        assert!(ed.all_in_list(crate::NUM_BULLET));
        let mut ed = empty();
        for ch in "1) ".chars() {
            type_key(&mut ed, &ac, ch);
        }
        assert!(ed.all_in_list(crate::NUM_DECIMAL));
        let mut ed = empty();
        for ch in "x- ".chars() {
            type_key(&mut ed, &ac, ch);
        }
        assert!(!ed.all_in_list(crate::NUM_BULLET));
    }
}
