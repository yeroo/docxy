//! Caret edits for a one-line text field kept as a plain `String` (the sheet
//! Find bar's Find and Replace fields, #1342). Char-indexed, no gpui, so the
//! edits are unit-testable. `sel` is a whole-field selection: only Ctrl+A makes
//! one, as in the cell entry (#1340).

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

/// Apply `op`; `caret` is clamped to the text first. Returns whether the text
/// changed.
pub(crate) fn apply(text: &mut String, caret: &mut usize, sel: &mut bool, op: Op) -> bool {
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
        Op::Insert(s) => {
            buf_insert(text, caret, s);
            return cleared || !s.is_empty();
        }
        Op::Backspace if !cleared => {
            let before = text.len();
            buf_backspace(text, caret);
            return text.len() != before;
        }
        Op::Delete if !cleared => {
            let before = text.len();
            buf_delete(text, *caret);
            return text.len() != before;
        }
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
    cleared
}

#[cfg(test)]
mod tests {
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
