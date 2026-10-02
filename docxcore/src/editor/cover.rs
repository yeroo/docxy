//! Insert › Cover Page and Blank Page (#652) on the body editor.
//!
//! A cover is a block content control at the start of the body (see
//! [`crate::cover`]); inserting, replacing and removing one is one undo step,
//! and the body text after it keeps its identity: the caret and the selection
//! follow it as the block count changes.

use super::{Caret, Clip, EditKind, Editor, all_paragraph_paths, resolve_para, tab_props_at};
use crate::cover::{
    COVER_DESIGNS, body_blocks, cover_sdt_xml, find_cover, free_sdt_ids, sdt_ids,
    typed_placeholders,
};
use crate::model::{Block, BreakKind, Inline, Paragraph};

impl Editor {
    /// Whether the document has a cover page (ours or Word's).
    pub fn has_cover_page(&self) -> bool {
        find_cover(&self.doc.body).is_some()
    }

    /// Whether the caret is in the cover page, where a paragraph break at
    /// the end of a placeholder continues it (see `split_paragraph_at`).
    pub(super) fn caret_in_cover(&self) -> bool {
        let Some(&block) = self.caret.path.first() else {
            return false;
        };
        find_cover(&self.doc.body).is_some_and(|(open, close)| (open..=close).contains(&block))
    }

    /// Put the cover `design` (an index into [`COVER_DESIGNS`]) at the start
    /// of the document, or in place of the cover it has, carrying the text
    /// typed into the old cover's placeholders into the new one's. The
    /// cover's section gets Different First Page (`w:titlePg`), so the cover
    /// shows no page number. One undo step. `used_ids` are the content-control ids
    /// the document's other parts (headers, footers) hold; the new controls'
    /// ids avoid them and the body's.
    pub fn set_cover_page(&mut self, design: usize, used_ids: &[i64]) -> Result<(), String> {
        let design = COVER_DESIGNS
            .get(design)
            .ok_or("There is no such cover page design")?;
        let old = find_cover(&self.doc.body);
        let typed = old
            .map(|(a, b)| typed_placeholders(&self.doc.body[a..=b]))
            .unwrap_or_default();
        let mut used = sdt_ids(&crate::serialize::blocks_to_xml(&self.doc.body));
        used.extend_from_slice(used_ids);
        let ids = free_sdt_ids(&used, design.paras.len() + 1);
        let blocks = body_blocks(&cover_sdt_xml(design, &typed, &ids));

        self.checkpoint(EditKind::Structural);
        let (at, removed) = old.map_or((0, 0), |(a, b)| (a, b - a + 1));
        let added = blocks.len();
        self.doc.body.splice(at..at + removed, blocks);
        self.follow_blocks(at, removed, added, at + added);

        // The cover's own section: a Word cover need not open the body.
        let slot = self.section_slots()[self.section_of_block(at)];
        let raw = crate::sect::set_flag(self.sect_raw(slot), "w:titlePg", true);
        if raw != self.sect_raw(slot) {
            self.set_sect_raw(slot, raw);
        }
        self.doc.initialize_revision_targets();
        Ok(())
    }

    /// Remove Current Cover Page: delete the cover control with all it holds,
    /// its page break included, as one undo step. Different First Page stays,
    /// as in Word. Whether there was a cover; without one nothing is recorded.
    pub fn remove_cover_page(&mut self) -> bool {
        let Some((at, end)) = find_cover(&self.doc.body) else {
            return false;
        };
        self.checkpoint(EditKind::Structural);
        self.doc.body.drain(at..=end);
        let mut added = 0;
        if !self
            .doc
            .body
            .iter()
            .any(|b| matches!(b, Block::Paragraph(_) | Block::Table(_)))
        {
            // A body needs a paragraph for the caret.
            self.doc
                .body
                .insert(at, Block::Paragraph(Paragraph::default()));
            added = 1;
        }
        self.follow_blocks(at, end - at + 1, added, at);
        self.doc.initialize_revision_targets();
        true
    }

    /// Insert two page breaks at the caret (Blank Page), replacing any
    /// selection, as Page Break does: the caret's paragraph ends with the
    /// first, a paragraph of its own holds the second, and the text after the
    /// caret starts the page after the blank one, where the caret goes. A
    /// break ending each paragraph is what pages a paragraph-by-paragraph
    /// layout (the suite's page view) as Word does: one break in the middle
    /// of a paragraph would leave the blank page to a renderer that splits
    /// paragraphs. One undo step (a selection's deletion aside). The breaks
    /// take the formatting typing at the caret would.
    pub fn insert_blank_page(&mut self) {
        if self.has_selection() {
            self.delete_selection();
        }
        let props = resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| tab_props_at(&p.content, self.caret.offset))
            .unwrap_or_default();
        self.paste(&Clip {
            paras: vec![
                vec![Inline::Break(BreakKind::Page, props.clone())],
                vec![Inline::Break(BreakKind::Page, props)],
                Vec::new(),
            ],
        });
    }

    /// Keep the caret and the selection on the same text after the top-level
    /// blocks `at..at + removed` were replaced by `added` new ones. An end that
    /// was inside the removed blocks goes to the start of the first paragraph
    /// from `land` on, and the selection is dropped.
    fn follow_blocks(&mut self, at: usize, removed: usize, added: usize, land: usize) {
        let body = &self.doc.body;
        let landing = || -> Caret {
            let path = all_paragraph_paths(body)
                .into_iter()
                .find(|p| p[0] >= land)
                .or_else(|| all_paragraph_paths(body).into_iter().last())
                .unwrap_or_else(|| vec![0]);
            Caret { path, offset: 0 }
        };
        // Whether the end stayed put (`Some(shifted)`) or was inside (`None`).
        let follow = |c: &Caret| -> Option<Caret> {
            let first = *c.path.first()?;
            if first < at {
                return Some(c.clone());
            }
            if first >= at + removed {
                let mut c = c.clone();
                c.path[0] = first - removed + added;
                return Some(c);
            }
            None
        };
        let caret = follow(&self.caret);
        let anchor = self.anchor.as_ref().map(follow);
        let lost = caret.is_none() || matches!(anchor, Some(None));
        self.caret = caret.unwrap_or_else(landing);
        self.anchor = match anchor {
            Some(Some(a)) if !lost => Some(a),
            _ => None,
        };
        self.clamp();
    }
}

#[cfg(test)]
mod tests;
