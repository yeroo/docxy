//! The Mac text-navigation chords of the document editor (#1073): ⌘←/→ line
//! start/end, ⌥←/→ previous word / end of word, ⌘↑/↓ document start/end (⇧
//! extends the selection) and ⌥⌫ deleting the word before the caret.
//!
//! [`mac_nav`] takes the platform as a parameter, as `text_input::route`
//! does, so the table is tested on every host; off macOS it is always `None`
//! and the Windows/Linux Ctrl bindings are untouched.

use docxcore::editor::Editor;
use gpui::Modifiers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MacNav {
    LineStart,
    LineEnd,
    WordLeft,
    WordEnd,
    DocStart,
    DocEnd,
    DeleteWordBack,
}

/// The chord `key` with modifiers `m` as a Mac navigation, if it is one: ⌘
/// alone (plus ⇧) on the four arrows, ⌥ alone on left/right (plus ⇧) and
/// on ⌫ (no ⇧).
pub(crate) fn mac_nav(macos: bool, key: &str, m: Modifiers) -> Option<MacNav> {
    if !macos || m.control || m.function {
        return None;
    }
    match (m.platform, m.alt, key) {
        (true, false, "left") => Some(MacNav::LineStart),
        (true, false, "right") => Some(MacNav::LineEnd),
        (true, false, "up") => Some(MacNav::DocStart),
        (true, false, "down") => Some(MacNav::DocEnd),
        (false, true, "left") => Some(MacNav::WordLeft),
        (false, true, "right") => Some(MacNav::WordEnd),
        (false, true, "backspace") if !m.shift => Some(MacNav::DeleteWordBack),
        _ => None,
    }
}

/// Run `nav` on the editor; whether it may have changed the document. The
/// arrows collapse the selection, or extend it with `shift`, as every
/// navigation key does.
pub(crate) fn run(ed: &mut Editor, shift: bool, nav: MacNav) -> bool {
    if nav != MacNav::DeleteWordBack {
        ed.extend_selection(shift);
    }
    match nav {
        MacNav::LineStart => ed.move_home(),
        MacNav::LineEnd => ed.move_end(),
        MacNav::WordLeft => ed.move_word_left(),
        MacNav::WordEnd => ed.move_word_end_caret(),
        MacNav::DocStart => ed.move_doc_start(),
        MacNav::DocEnd => ed.move_doc_end(),
        MacNav::DeleteWordBack => {
            if !ed.has_selection() {
                ed.extend_selection(true);
                ed.move_word_left();
            }
            ed.backspace();
            return true;
        }
    }
    false
}

/// Whether the KeyTips Alt raised must go down for this key: on a Mac the ⌥
/// key-down comes before ⌥←, which the KeyTips would otherwise swallow.
pub(crate) fn lowers_keytips(nav: Option<MacNav>, keytips_up: bool) -> bool {
    nav.is_some() && keytips_up
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::editor::Caret;

    fn mods(platform: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers {
            platform,
            alt,
            shift,
            ..Default::default()
        }
    }

    fn ed(text: &str, offset: usize) -> Editor {
        let mut ed = Editor::new(docxcore::markdown::from_markdown(text));
        ed.set_caret(Caret::at(vec![0], offset));
        ed
    }

    const LINE: &str = "hello brave world";

    #[test]
    fn the_chord_table_1073() {
        use MacNav::*;
        for shift in [false, true] {
            for (key, cmd, opt) in [("left", LineStart, WordLeft), ("right", LineEnd, WordEnd)] {
                assert_eq!(mac_nav(true, key, mods(true, false, shift)), Some(cmd));
                assert_eq!(mac_nav(true, key, mods(false, true, shift)), Some(opt));
            }
            assert_eq!(
                mac_nav(true, "up", mods(true, false, shift)),
                Some(DocStart)
            );
            assert_eq!(
                mac_nav(true, "down", mods(true, false, shift)),
                Some(DocEnd)
            );
        }
        assert_eq!(
            mac_nav(true, "backspace", mods(false, true, false)),
            Some(DeleteWordBack)
        );
        assert_eq!(mac_nav(true, "backspace", mods(false, true, true)), None);
        // ⌥↑/↓, plain keys, ⌘⌥ and ⌃ combinations are not these chords.
        assert_eq!(mac_nav(true, "up", mods(false, true, false)), None);
        assert_eq!(mac_nav(true, "left", mods(false, false, false)), None);
        assert_eq!(mac_nav(true, "left", mods(true, true, false)), None);
        assert_eq!(mac_nav(true, "backspace", mods(false, false, false)), None);
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(mac_nav(true, "left", ctrl), None);
        // ⌘⇧Z stays the Redo alias.
        assert_eq!(mac_nav(true, "z", mods(true, false, true)), None);
        // Off macOS nothing changes.
        assert_eq!(mac_nav(false, "left", mods(true, false, false)), None);
        assert_eq!(mac_nav(false, "left", mods(false, true, false)), None);
        assert_eq!(mac_nav(false, "backspace", mods(false, true, false)), None);
    }

    #[test]
    fn the_chords_are_not_menu_chords_1073() {
        for (key, m) in [
            ("left", mods(true, false, false)),
            ("right", mods(true, false, false)),
            ("up", mods(true, false, false)),
            ("down", mods(true, false, false)),
            ("left", mods(false, true, false)),
            ("right", mods(false, true, false)),
        ] {
            let k = gpui::Keystroke {
                modifiers: m,
                key: key.into(),
                key_char: None,
            };
            assert!(crate::macos_menu::chord_role(&k).is_none(), "{key}");
        }
    }

    #[test]
    fn lines_words_and_document_1073() {
        let mut e = ed(LINE, 8); // hello br|ave
        run(&mut e, false, MacNav::LineEnd);
        assert_eq!(e.caret.offset, 17);
        run(&mut e, false, MacNav::LineStart);
        assert_eq!(e.caret.offset, 0);
        e.set_caret(Caret::at(vec![0], 8));
        run(&mut e, false, MacNav::WordEnd);
        assert_eq!(e.caret.offset, 11);
        e.set_caret(Caret::at(vec![0], 8));
        run(&mut e, false, MacNav::WordLeft);
        assert_eq!(e.caret.offset, 6);
        run(&mut e, false, MacNav::DocEnd);
        assert_eq!(e.caret.offset, 17);
        run(&mut e, false, MacNav::DocStart);
        assert_eq!(e.caret.offset, 0);
    }

    #[test]
    fn shift_extends_the_selection_1073() {
        let mut e = ed(LINE, 8);
        run(&mut e, true, MacNav::WordEnd);
        assert_eq!(e.selection_text(), "ave");
        run(&mut e, true, MacNav::LineEnd);
        assert_eq!(e.selection_text(), "ave world");
        let mut e = ed(LINE, 8);
        run(&mut e, true, MacNav::WordLeft);
        assert_eq!(e.selection_text(), "br");
        let mut e = ed(LINE, 8);
        run(&mut e, true, MacNav::LineStart);
        assert_eq!(e.selection_text(), "hello br");
    }

    /// The data-loss case: ⌘↓ over a selection collapses it, so the next
    /// keystroke types instead of replacing the selected text.
    #[test]
    fn doc_end_collapses_a_selection_1073() {
        for nav in [MacNav::DocEnd, MacNav::DocStart, MacNav::LineEnd] {
            let mut e = ed(LINE, 6);
            e.extend_selection(true);
            e.set_caret(Caret::at(vec![0], 11));
            assert!(e.has_selection());
            run(&mut e, false, nav);
            assert!(!e.has_selection(), "{nav:?}");
            e.insert_str("!");
            assert!(e.doc.body[0].plain_text().contains("brave"), "{nav:?}");
        }
    }

    #[test]
    fn delete_word_back_1073() {
        let mut e = ed(LINE, 11); // hello brave| world
        assert!(run(&mut e, false, MacNav::DeleteWordBack));
        assert_eq!(e.doc.body[0].plain_text(), "hello  world");
        assert_eq!(e.caret.offset, 6);
        e.undo();
        assert_eq!(e.doc.body[0].plain_text(), LINE);
        // A selection is what goes, not the word before the caret.
        let mut e = ed(LINE, 0);
        e.extend_selection(true);
        e.set_caret(Caret::at(vec![0], 5));
        run(&mut e, false, MacNav::DeleteWordBack);
        assert_eq!(e.doc.body[0].plain_text(), " brave world");
        e.undo();
        assert_eq!(e.doc.body[0].plain_text(), LINE);
        // At the paragraph start it merges into the previous one.
        let mut e = Editor::new(docxcore::markdown::from_markdown("ab\n\ncd\n"));
        e.set_caret(Caret::at(vec![1], 0));
        run(&mut e, false, MacNav::DeleteWordBack);
        assert_eq!(e.doc.body.len(), 1);
        // At the very start it is no edit.
        let mut e = ed(LINE, 0);
        run(&mut e, false, MacNav::DeleteWordBack);
        assert_eq!(e.doc.body[0].plain_text(), LINE);
        assert!(!e.has_selection());
    }

    #[test]
    fn alt_keytips_go_down_for_the_chord_1073() {
        let nav = mac_nav(true, "left", mods(false, true, false));
        assert!(lowers_keytips(nav, true));
        assert!(!lowers_keytips(nav, false));
        assert!(!lowers_keytips(None, true));
    }
}
