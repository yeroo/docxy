//! A one-line text buffer with an editing caret, shared by the terminal
//! prompts (Find, Replace, comment, Compare, vim `:`; #1372).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key did to a [`LineEdit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    /// The key is not an editing key.
    None,
    /// Only the caret moved (or nothing changed).
    Moved,
    /// The text changed.
    Changed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LineEdit {
    buf: String,
    /// The caret, in chars.
    caret: usize,
    /// The other end of a selection, in chars; the selection runs between it
    /// and the caret (#1375).
    anchor: Option<usize>,
}

impl From<&str> for LineEdit {
    /// The caret goes at the end, as a prompt seeded with text opens.
    fn from(s: &str) -> Self {
        LineEdit {
            buf: s.to_string(),
            caret: s.chars().count(),
            anchor: None,
        }
    }
}

impl From<String> for LineEdit {
    fn from(s: String) -> Self {
        let caret = s.chars().count();
        LineEdit {
            buf: s,
            caret,
            anchor: None,
        }
    }
}

impl LineEdit {
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn len(&self) -> usize {
        self.buf.chars().count()
    }

    fn byte(&self, chars: usize) -> usize {
        self.buf
            .char_indices()
            .nth(chars)
            .map_or(self.buf.len(), |(i, _)| i)
    }

    /// The text before the caret.
    pub fn before(&self) -> &str {
        &self.buf[..self.byte(self.caret)]
    }

    /// The text after the caret.
    pub fn after(&self) -> &str {
        &self.buf[self.byte(self.caret)..]
    }

    /// The text with the caret mark `mark` at the caret, as a prompt draws it.
    pub fn with_mark(&self, mark: &str) -> String {
        format!("{}{mark}{}", self.before(), self.after())
    }

    /// Selects the whole text, the caret at its end. An empty field selects
    /// nothing.
    pub fn select_all(&mut self) {
        self.caret = self.len();
        self.anchor = Some(0);
    }

    /// The selected span `(from, to)` in chars, `from < to`, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    /// Removes the selection, leaving the caret where it began; whether
    /// there was one.
    pub fn delete_selection(&mut self) -> bool {
        let sel = self.selection();
        self.anchor = None;
        let Some((from, to)) = sel else {
            return false;
        };
        let (a, b) = (self.byte(from), self.byte(to));
        self.buf.replace_range(a..b, "");
        self.caret = from;
        true
    }

    pub fn insert(&mut self, c: char) {
        self.delete_selection();
        let at = self.byte(self.caret);
        self.buf.insert(at, c);
        self.caret += 1;
    }

    pub fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret == 0 {
            return false;
        }
        self.caret -= 1;
        let at = self.byte(self.caret);
        self.buf.remove(at);
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret >= self.len() {
            return false;
        }
        let at = self.byte(self.caret);
        self.buf.remove(at);
        true
    }

    /// With a selection, Left collapses to its start instead of stepping.
    pub fn left(&mut self) {
        if let Some((from, _)) = self.selection() {
            self.anchor = None;
            self.caret = from;
            return;
        }
        self.anchor = None;
        self.caret = self.caret.saturating_sub(1);
    }

    /// With a selection, Right collapses to its end instead of stepping.
    pub fn right(&mut self) {
        if let Some((_, to)) = self.selection() {
            self.anchor = None;
            self.caret = to;
            return;
        }
        self.anchor = None;
        self.caret = (self.caret + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.anchor = None;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.anchor = None;
        self.caret = self.len();
    }

    /// 1 when the caret mark drawn before the first char of a non-empty field
    /// would otherwise be the cell `field-read`'s `at` names.
    pub fn mark_lead(&self) -> u16 {
        u16::from(self.caret == 0 && !self.buf.is_empty())
    }

    /// Whether `key` is one [`LineEdit::handle`] takes: Backspace, Delete, the
    /// arrows, Home or End, with no Ctrl or Alt, so the chords the prompts and
    /// the document own keep their meaning.
    pub fn is_edit_key(key: &KeyEvent) -> bool {
        matches!(
            key.code,
            KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
        ) && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    }

    /// The shared plain-key set: Backspace, Delete, Left, Right, Home, End.
    /// Typed characters go through [`LineEdit::insert`], and the caller
    /// decides which modifiers it lets through.
    pub fn handle(&mut self, code: KeyCode) -> Edit {
        match code {
            KeyCode::Backspace => {
                if self.backspace() {
                    Edit::Changed
                } else {
                    Edit::Moved
                }
            }
            KeyCode::Delete => {
                if self.delete() {
                    Edit::Changed
                } else {
                    Edit::Moved
                }
            }
            KeyCode::Left => {
                self.left();
                Edit::Moved
            }
            KeyCode::Right => {
                self.right();
                Edit::Moved
            }
            KeyCode::Home => {
                self.home();
                Edit::Moved
            }
            KeyCode::End => {
                self.end();
                Edit::Moved
            }
            _ => Edit::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> LineEdit {
        let mut l = LineEdit::default();
        for c in s.chars() {
            l.insert(c);
        }
        l
    }

    #[test]
    fn edits_at_the_caret() {
        let mut l = typed("abcd");
        assert_eq!(l.handle(KeyCode::Backspace), Edit::Changed);
        l.handle(KeyCode::Home);
        l.handle(KeyCode::Right);
        assert_eq!(l.handle(KeyCode::Delete), Edit::Changed);
        l.handle(KeyCode::End);
        l.handle(KeyCode::Left);
        l.insert('X');
        assert_eq!((l.as_str(), l.caret()), ("aXc", 2));
        assert_eq!((l.before(), l.after()), ("aX", "c"));
        assert_eq!(l.with_mark("|"), "aX|c");
    }

    #[test]
    fn edges_do_nothing() {
        let mut l = typed("ab");
        assert_eq!(l.handle(KeyCode::Delete), Edit::Moved);
        l.handle(KeyCode::Right);
        assert_eq!(l.caret(), 2);
        l.home();
        assert_eq!(l.handle(KeyCode::Backspace), Edit::Moved);
        l.left();
        assert_eq!((l.as_str(), l.caret()), ("ab", 0));
        assert_eq!(l.handle(KeyCode::Up), Edit::None);
    }

    #[test]
    fn multibyte_chars_count_as_one() {
        let mut l = typed("é日ñ");
        l.left();
        assert!(l.backspace());
        assert_eq!((l.as_str(), l.caret()), ("éñ", 1));
        l.insert('ü');
        assert_eq!(l.with_mark("|"), "éü|ñ");
        l.end();
        assert!(!l.delete());
        assert_eq!(LineEdit::from("日本").caret(), 2);
    }

    #[test]
    fn select_all_is_replaced_by_typing_and_deleted_by_backspace_and_delete() {
        let mut l = typed("é日ñ");
        assert_eq!(l.selection(), None);
        l.select_all();
        assert_eq!(l.selection(), Some((0, 3)));
        l.insert('X');
        assert_eq!((l.as_str(), l.caret(), l.selection()), ("X", 1, None));
        for code in [KeyCode::Backspace, KeyCode::Delete] {
            let mut l = typed("é日ñ");
            l.select_all();
            assert_eq!(l.handle(code), Edit::Changed);
            assert_eq!((l.as_str(), l.caret(), l.selection()), ("", 0, None));
        }
    }

    #[test]
    fn moving_drops_the_selection() {
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
            let mut l = typed("abc");
            l.select_all();
            assert_eq!(l.handle(code), Edit::Moved);
            assert_eq!((l.as_str(), l.selection()), ("abc", None));
        }
    }

    #[test]
    fn left_and_right_collapse_to_the_selection_edges() {
        let mut l = typed("abc");
        l.select_all();
        l.left();
        assert_eq!((l.caret(), l.selection()), (0, None));
        l.select_all();
        l.right();
        assert_eq!((l.caret(), l.selection()), (3, None));
        // A selection with the caret at its start, inside the text.
        for (anchor, caret, left, right) in [(3, 1, 1, 3), (1, 3, 1, 3), (1, 2, 1, 2)] {
            for go_left in [true, false] {
                let mut l = typed("abcd");
                l.anchor = Some(anchor);
                l.caret = caret;
                if go_left {
                    l.left();
                } else {
                    l.right();
                }
                assert_eq!(l.caret(), if go_left { left } else { right });
                assert_eq!(l.selection(), None);
            }
        }
    }

    #[test]
    fn selecting_an_empty_field_selects_nothing() {
        let mut l = LineEdit::default();
        l.select_all();
        assert_eq!(l.selection(), None);
        assert_eq!(l.handle(KeyCode::Backspace), Edit::Moved);
    }
}
