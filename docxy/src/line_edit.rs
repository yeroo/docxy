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
}

impl From<&str> for LineEdit {
    /// The caret goes at the end, as a prompt seeded with text opens.
    fn from(s: &str) -> Self {
        LineEdit {
            buf: s.to_string(),
            caret: s.chars().count(),
        }
    }
}

impl From<String> for LineEdit {
    fn from(s: String) -> Self {
        let caret = s.chars().count();
        LineEdit { buf: s, caret }
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

    pub fn insert(&mut self, c: char) {
        let at = self.byte(self.caret);
        self.buf.insert(at, c);
        self.caret += 1;
    }

    pub fn backspace(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.caret -= 1;
        let at = self.byte(self.caret);
        self.buf.remove(at);
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.caret >= self.len() {
            return false;
        }
        let at = self.byte(self.caret);
        self.buf.remove(at);
        true
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
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
}
