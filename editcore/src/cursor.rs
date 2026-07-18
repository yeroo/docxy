//! Cursor and selection types: a position within a [`crate::model::RichText`]
//! document, and a selection spanning two such positions.

/// A position within a document, addressed by block index, run index within
/// that block, and a UTF-8 byte offset within that run's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub block: usize,
    pub run: usize,
    pub offset: usize,
}

/// A selection: an anchor (where selecting began) and a caret (the active
/// end, which moves as the selection is extended).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: Pos,
    pub caret: Pos,
}

impl Selection {
    /// True when the selection has no extent (anchor and caret coincide).
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.caret
    }

    /// The selection's endpoints in document order: `(start, end)`, ordered
    /// by `(block, run, offset)` regardless of which end is the caret.
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.caret {
            (self.anchor, self.caret)
        } else {
            (self.caret, self.anchor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_selection_has_equal_anchor_and_caret() {
        let pos = Pos {
            block: 1,
            run: 2,
            offset: 3,
        };
        let sel = Selection {
            anchor: pos,
            caret: pos,
        };
        assert!(sel.is_collapsed());
    }

    #[test]
    fn non_collapsed_selection_is_not_collapsed() {
        let sel = Selection {
            anchor: Pos {
                block: 0,
                run: 0,
                offset: 0,
            },
            caret: Pos {
                block: 0,
                run: 0,
                offset: 1,
            },
        };
        assert!(!sel.is_collapsed());
    }

    #[test]
    fn ordered_returns_min_then_max_by_block_run_offset() {
        let earlier = Pos {
            block: 0,
            run: 5,
            offset: 9,
        };
        let later = Pos {
            block: 1,
            run: 0,
            offset: 0,
        };

        let forward = Selection {
            anchor: earlier,
            caret: later,
        };
        assert_eq!(forward.ordered(), (earlier, later));

        let backward = Selection {
            anchor: later,
            caret: earlier,
        };
        assert_eq!(backward.ordered(), (earlier, later));
    }

    #[test]
    fn ordered_compares_run_before_offset_within_same_block() {
        let a = Pos {
            block: 0,
            run: 0,
            offset: 100,
        };
        let b = Pos {
            block: 0,
            run: 1,
            offset: 0,
        };
        let sel = Selection {
            anchor: b,
            caret: a,
        };
        assert_eq!(sel.ordered(), (a, b));
    }
}
