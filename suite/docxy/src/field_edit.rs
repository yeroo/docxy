//! Caret edits for a one-line text field kept as a plain `String` (the sheet
//! Find bar's Find and Replace fields, #1342, and the Name Box, #1343).
//! Char-indexed, no gpui, so the edits are unit-testable. `sel` is a
//! whole-field selection: Ctrl+A makes one, as in the cell entry (#1340), and a
//! click on the Name Box does too.

use crate::{buf_backspace, buf_delete, buf_insert};

pub(crate) enum Op<'a> {
    Insert(&'a str),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    SelectAll,
}

/// Whether a key-down is Excel's Alt+R / Alt+A in the Find bar: `Some(true)`
/// for Replace All (`a`), `Some(false)` for Replace (`r`). Not Ctrl+Alt (AltGr
/// types text), and not when ⌥ made another character (macOS ⌥R is "®", ⌥A
/// "å"): that is typing, not a chord (#1162). Off macOS (`mac` false) Alt is
/// never text, so the layout's character (Russian Alt+R is "к") is ignored.
pub(crate) fn alt_replace(
    key: &str,
    key_char: Option<&str>,
    ctrl: bool,
    alt: bool,
    mac: bool,
) -> Option<bool> {
    if !alt || ctrl || !matches!(key, "r" | "a") {
        return None;
    }
    if mac && key_char.is_some_and(|c| !c.eq_ignore_ascii_case(key)) {
        return None;
    }
    Some(key == "a")
}

/// Whether a key-down is Ctrl+S, which a focused one-line field lets through to
/// the sheet's save. Not Ctrl+Alt: that is AltGr on Windows, where S is text
/// (Polish "ś").
pub(crate) fn is_save_chord(ctrl: bool, alt: bool, key: &str) -> bool {
    ctrl && !alt && key == "s"
}

/// `text` as one line of typed text: line breaks and every other control
/// character drop out (a paste into the comment bar, whose text reaches XML).
pub(crate) fn one_line(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// The caret and selection to use for `text`: those kept, unless the text's
/// length is no longer the one they were kept for (another path edited the
/// text), when the caret is at the end and nothing is selected.
pub(crate) fn current(text: &str, caret: usize, sel: bool, seen_len: usize) -> (usize, bool) {
    let len = text.chars().count();
    if len != seen_len {
        (len, false)
    } else {
        (caret.min(len), sel && len > 0)
    }
}

/// Apply `op`; `caret` is clamped to the text first.
pub(crate) fn apply(text: &mut String, caret: &mut usize, sel: &mut bool, op: Op) {
    let len = text.chars().count();
    *caret = (*caret).min(len);
    let had_sel = std::mem::take(sel) && len > 0;
    let mut cleared = false;
    if had_sel && matches!(op, Op::Insert(_) | Op::Backspace | Op::Delete) {
        text.clear();
        *caret = 0;
        cleared = true;
    }
    match op {
        Op::Insert(s) => buf_insert(text, caret, s),
        Op::Backspace if !cleared => buf_backspace(text, caret),
        Op::Delete if !cleared => buf_delete(text, *caret),
        Op::Backspace | Op::Delete => {}
        Op::Left => *caret = if had_sel { 0 } else { caret.saturating_sub(1) },
        Op::Right => *caret = if had_sel { len } else { (*caret + 1).min(len) },
        Op::Home => *caret = 0,
        Op::End => *caret = len,
        Op::SelectAll => {
            *caret = len;
            *sel = len > 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Op, apply};
    use crate::SheetRename;

    fn rename(r: &mut SheetRename, op: Op) {
        apply(&mut r.text, &mut r.caret, &mut r.sel, op);
    }

    #[test]
    fn sheet_rename_opens_with_the_whole_name_selected() {
        let r = SheetRename::open(2, "Sheet1".into());
        assert_eq!(
            (r.idx, r.text.as_str(), r.caret, r.sel),
            (2, "Sheet1", 6, true)
        );
        let empty = SheetRename::open(0, String::new());
        assert_eq!((empty.caret, empty.sel), (0, false));
    }

    #[test]
    fn sheet_rename_typing_replaces_the_selected_name() {
        let mut r = SheetRename::open(0, "Sheet1".into());
        rename(&mut r, Op::Insert("Ab1 "));
        assert_eq!((r.text.as_str(), r.caret, r.sel), ("Ab1 ", 4, false));
    }

    #[test]
    fn sheet_rename_backspaces_after_the_first_delete_nothing_more() {
        let mut r = SheetRename::open(0, "Sheet1".into());
        for _ in 0..6 {
            rename(&mut r, Op::Backspace);
        }
        assert_eq!((r.text.as_str(), r.caret), ("", 0));
    }

    #[test]
    fn sheet_rename_edits_at_a_caret() {
        let mut r = SheetRename::open(0, "x".into());
        rename(&mut r, Op::Insert("abcd"));
        rename(&mut r, Op::Backspace);
        assert_eq!(r.text, "abc");
        rename(&mut r, Op::Home);
        assert_eq!(r.caret, 0);
        rename(&mut r, Op::Right);
        rename(&mut r, Op::Delete);
        assert_eq!(r.text, "ac");
        rename(&mut r, Op::End);
        rename(&mut r, Op::Left);
        assert_eq!(r.caret, 1);
        rename(&mut r, Op::Insert("X"));
        assert_eq!((r.text.as_str(), r.caret), ("aXc", 2));
    }

    #[test]
    fn sheet_rename_select_all_then_type_replaces() {
        let mut r = SheetRename::open(0, "abcd".into());
        rename(&mut r, Op::Home);
        assert!(!r.sel);
        rename(&mut r, Op::SelectAll);
        assert!(r.sel);
        assert_eq!(r.caret, 4);
        rename(&mut r, Op::Insert("X"));
        assert_eq!((r.text.as_str(), r.sel), ("X", false));
    }

    #[test]
    fn sheet_rename_select_all_on_an_empty_name_selects_nothing() {
        let mut r = SheetRename::open(0, String::new());
        rename(&mut r, Op::SelectAll);
        assert!(!r.sel);
        rename(&mut r, Op::Delete);
        assert_eq!(r.text, "");
    }

    #[test]
    fn one_line_drops_line_breaks_and_control_characters() {
        assert_eq!(super::one_line("a\r\nb\nc"), "abc");
        // A sheet copy's TSV pastes into the tab rename as one name.
        assert_eq!(super::one_line("a\tb\r\n"), "ab");
        assert_eq!(super::one_line("\r\n"), "");
        assert_eq!(super::one_line("a\x0bb\x0cc\x1bd\0e\tf"), "abcdef");
        assert_eq!(super::one_line("é日 \u{1F600}"), "é日 \u{1F600}");
    }

    #[test]
    fn only_a_plain_ctrl_s_is_the_save_chord() {
        assert!(is_save_chord(true, false, "s"));
        assert!(!is_save_chord(true, true, "s"), "AltGr+S is text");
        assert!(!is_save_chord(false, false, "s"));
        assert!(!is_save_chord(true, false, "a"));
    }

    use super::*;

    fn run(text: &str, caret: usize, sel: bool, ops: Vec<Op>) -> (String, usize, bool) {
        let (mut t, mut c, mut s) = (text.to_string(), caret, sel);
        for op in ops {
            apply(&mut t, &mut c, &mut s, op);
        }
        (t, c, s)
    }

    #[test]
    fn insert_mid_text() {
        assert_eq!(
            run("ac", 1, false, vec![Op::Insert("b")]),
            ("abc".into(), 2, false)
        );
    }

    #[test]
    fn backspace_and_delete_at_ends_are_no_ops() {
        assert_eq!(
            run("ab", 0, false, vec![Op::Backspace]),
            ("ab".into(), 0, false)
        );
        assert_eq!(
            run("ab", 2, false, vec![Op::Delete]),
            ("ab".into(), 2, false)
        );
        assert_eq!(
            run("abc", 1, false, vec![Op::Backspace]),
            ("bc".into(), 0, false)
        );
        assert_eq!(
            run("abc", 1, false, vec![Op::Delete]),
            ("ac".into(), 1, false)
        );
    }

    #[test]
    fn navigation_clamps() {
        assert_eq!(run("ab", 0, false, vec![Op::Left]).1, 0);
        assert_eq!(run("ab", 2, false, vec![Op::Right]).1, 2);
        assert_eq!(run("ab", 1, false, vec![Op::Home]).1, 0);
        assert_eq!(run("ab", 1, false, vec![Op::End]).1, 2);
        assert_eq!(
            run("ab", 9, false, vec![Op::Left]).1,
            1,
            "stale caret is clamped"
        );
    }

    #[test]
    fn select_all_then_typing_replaces() {
        assert_eq!(
            run("abcd", 4, false, vec![Op::SelectAll]),
            ("abcd".into(), 4, true)
        );
        assert_eq!(
            run("abcd", 4, false, vec![Op::SelectAll, Op::Insert("X")]),
            ("X".into(), 1, false)
        );
    }

    #[test]
    fn select_all_then_backspace_or_delete_clears() {
        for op in [Op::Backspace, Op::Delete] {
            assert_eq!(
                run("abcd", 2, false, vec![Op::SelectAll, op]),
                (String::new(), 0, false)
            );
        }
    }

    #[test]
    fn selection_collapses_on_navigation() {
        assert_eq!(
            run("abcd", 2, false, vec![Op::SelectAll, Op::Left]),
            ("abcd".into(), 0, false)
        );
        assert_eq!(
            run("abcd", 2, false, vec![Op::SelectAll, Op::Right]),
            ("abcd".into(), 4, false)
        );
        assert_eq!(
            run("abcd", 2, false, vec![Op::SelectAll, Op::Home]),
            ("abcd".into(), 0, false)
        );
    }

    #[test]
    fn select_all_of_empty_selects_nothing() {
        assert_eq!(
            run("", 0, false, vec![Op::SelectAll]),
            (String::new(), 0, false)
        );
    }

    #[test]
    fn alt_replace_takes_the_plain_letter_only_on_macos() {
        assert_eq!(alt_replace("r", Some("r"), false, true, true), Some(false));
        assert_eq!(alt_replace("a", Some("a"), false, true, true), Some(true));
        assert_eq!(alt_replace("a", None, false, true, true), Some(true));
        for c in ["®", "å", "ą", "к"] {
            assert_eq!(alt_replace("r", Some(c), false, true, true), None);
            assert_eq!(alt_replace("a", Some(c), false, true, true), None);
        }
        assert_eq!(alt_replace("r", Some("r"), true, true, true), None, "AltGr");
        assert_eq!(alt_replace("r", Some("r"), false, false, true), None);
        assert_eq!(alt_replace("x", Some("x"), false, true, true), None);
    }

    #[test]
    fn alt_replace_ignores_the_layouts_character_off_macos() {
        assert_eq!(alt_replace("r", Some("к"), false, true, false), Some(false));
        assert_eq!(alt_replace("a", Some("ф"), false, true, false), Some(true));
        assert_eq!(
            alt_replace("r", Some("r"), true, true, false),
            None,
            "AltGr"
        );
    }

    #[test]
    fn a_text_edited_elsewhere_resets_caret_and_selection() {
        assert_eq!(current("abcdef", 1, true, 3), (6, false));
        assert_eq!(current("abc", 1, true, 3), (1, true));
        assert_eq!(current("abc", 9, false, 3), (3, false));
        assert_eq!(current("", 0, true, 0), (0, false));
    }

    #[test]
    fn multibyte_chars_count_as_one() {
        assert_eq!(
            run("é😀", 2, false, vec![Op::Backspace]),
            ("é".into(), 1, false)
        );
        assert_eq!(
            run("é😀", 0, false, vec![Op::Delete]),
            ("😀".into(), 0, false)
        );
        assert_eq!(
            run("é😀", 1, false, vec![Op::Insert("x")]),
            ("éx😀".into(), 2, false)
        );
    }
}
