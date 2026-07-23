//! Undo/redo history for [`crate::ops::Editor`].
//!
//! The simplest correct approach for email-sized buffers: snapshot the whole
//! `(RichText, Selection)` state before every mutating op, and push/pop those
//! snapshots on undo/redo. No diffing, no inverse ops to get wrong.

use crate::cursor::Selection;
use crate::model::RichText;

/// Records `(RichText, Selection)` snapshots so edits can be undone and
/// redone.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo_stack: Vec<(RichText, Selection)>,
    redo_stack: Vec<(RichText, Selection)>,
}

impl History {
    /// An empty history: nothing to undo or redo yet.
    pub fn new() -> History {
        History::default()
    }

    /// Records a snapshot of the state *before* a mutating op runs. Any
    /// existing redo history is discarded, since it no longer follows from
    /// the current state.
    pub fn record(&mut self, text: &RichText, sel: &Selection) {
        self.undo_stack.push((text.clone(), *sel));
        self.redo_stack.clear();
    }

    /// Pops the most recent snapshot and returns it, pushing `current` onto
    /// the redo stack so it can be restored again with [`History::redo`].
    /// Returns `None` (leaving both stacks untouched) when there is nothing
    /// to undo.
    pub fn undo(
        &mut self,
        current: &RichText,
        current_sel: &Selection,
    ) -> Option<(RichText, Selection)> {
        let snapshot = self.undo_stack.pop()?;
        self.redo_stack.push((current.clone(), *current_sel));
        Some(snapshot)
    }

    /// Pops the most recently undone snapshot and returns it, pushing
    /// `current` back onto the undo stack. Returns `None` (leaving both
    /// stacks untouched) when there is nothing to redo.
    pub fn redo(
        &mut self,
        current: &RichText,
        current_sel: &Selection,
    ) -> Option<(RichText, Selection)> {
        let snapshot = self.redo_stack.pop()?;
        self.undo_stack.push((current.clone(), *current_sel));
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Pos;

    fn sel(offset: usize) -> Selection {
        let p = Pos {
            block: 0,
            run: 0,
            offset,
        };
        Selection {
            anchor: p,
            caret: p,
        }
    }

    #[test]
    fn undo_with_empty_history_returns_none() {
        let mut h = History::new();
        assert!(h.undo(&RichText::new(), &sel(0)).is_none());
    }

    #[test]
    fn redo_with_empty_history_returns_none() {
        let mut h = History::new();
        assert!(h.redo(&RichText::new(), &sel(0)).is_none());
    }

    #[test]
    fn record_then_undo_then_redo_round_trips() {
        let mut h = History::new();
        let before = RichText::new();
        h.record(&before, &sel(0));
        let after = RichText::new();
        let (restored, _) = h.undo(&after, &sel(5)).unwrap();
        assert_eq!(restored, before);
        // `undo` pushed (after, sel(5)) onto the redo stack (the state as it
        // was just before the undo), so `redo` hands that pair back.
        let (redone, redone_sel) = h.redo(&restored, &sel(0)).unwrap();
        assert_eq!(redone, after);
        assert_eq!(redone_sel, sel(5));
    }

    #[test]
    fn record_clears_redo_stack() {
        let mut h = History::new();
        h.record(&RichText::new(), &sel(0));
        let _ = h.undo(&RichText::new(), &sel(1));
        assert!(h.redo(&RichText::new(), &sel(0)).is_some());

        // A fresh record after that redo invalidates any further redo.
        let mut h2 = History::new();
        h2.record(&RichText::new(), &sel(0));
        let _ = h2.undo(&RichText::new(), &sel(1));
        h2.record(&RichText::new(), &sel(2));
        assert!(h2.redo(&RichText::new(), &sel(0)).is_none());
    }
}
