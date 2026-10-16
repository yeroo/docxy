//! Section addressing for the page Layout commands (#649).
//!
//! A document's sections are its body paragraphs that carry a `section_break`
//! (each closes the section it ends), in body order, then the body-level
//! trailing `w:sectPr`, which describes the final section. Section `k` is the
//! k-th of these. A paragraph carrying a break belongs to the section that
//! break closes, and a table belongs to the section of the body block it sits
//! in. Every edit here is one undo step on the editor's document, which is
//! what the page view and Save read.

use super::{
    Caret, EditKind, Editor, inline_len, resolve_para, spans_containers, split_paragraph_content,
    tab_props_at,
};
use crate::model::{Block, BreakKind, Inline, Paragraph, SectionProperties};
use crate::sect::{SectionSetup, SectionStart};

use super::Clip;

/// Where one section's properties live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SectAt {
    /// The `section_break` of the body paragraph at this index.
    Para(usize),
    /// The body-level trailing section properties.
    Trailing,
}

/// How one end of a selection cuts its section.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cut {
    /// Already at the section's edge: nothing to cut.
    Edge,
    /// Between paragraphs: the mark of this body paragraph ends the section.
    Gap(usize),
    /// Inside a paragraph (or beside a table): split it.
    Mid,
}

/// A raw sectPr for a section that has none of its own yet.
const EMPTY_SECT: &str = "<w:sectPr></w:sectPr>";

impl Editor {
    pub(super) fn section_slots(&self) -> Vec<SectAt> {
        let mut out: Vec<SectAt> = self
            .doc
            .body
            .iter()
            .enumerate()
            .filter_map(|(i, b)| match b {
                Block::Paragraph(p) if p.props.section_break.is_some() => Some(SectAt::Para(i)),
                _ => None,
            })
            .collect();
        out.push(SectAt::Trailing);
        out
    }

    /// Every section's `w:sectPr`, in body order. The final section reads as an
    /// empty sectPr when the document has no trailing one.
    pub fn sections(&self) -> Vec<String> {
        self.section_slots()
            .into_iter()
            .map(|at| self.sect_raw(at).to_string())
            .collect()
    }

    pub(super) fn sect_raw(&self, at: SectAt) -> &str {
        match at {
            SectAt::Para(i) => match &self.doc.body[i] {
                Block::Paragraph(p) => p.props.section_break.as_deref().unwrap_or(EMPTY_SECT),
                _ => EMPTY_SECT,
            },
            SectAt::Trailing => self
                .doc
                .trailing_section_properties()
                .map_or(EMPTY_SECT, |s| s.raw.as_str()),
        }
    }

    /// The section a body block belongs to.
    pub fn section_of_block(&self, block: usize) -> usize {
        self.doc.body[..block.min(self.doc.body.len())]
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(p) if p.props.section_break.is_some()))
            .count()
    }

    /// The caret's section.
    pub fn caret_section(&self) -> usize {
        self.section_of_block(self.caret.path.first().copied().unwrap_or(0))
    }

    /// The sections the Layout commands act on: each section containing a
    /// selected paragraph, else the caret's.
    pub fn target_sections(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .selection_spans()
            .iter()
            .filter_map(|(path, _, _)| path.first())
            .map(|&b| self.section_of_block(b))
            .collect();
        if out.is_empty() {
            out.push(self.caret_section());
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Rewrite the sectPr of each section in `indexes` with `edit` as one undo
    /// step. Nothing is recorded when no section changes. Whether any did.
    pub fn edit_sections(&mut self, indexes: &[usize], edit: impl Fn(&str) -> String) -> bool {
        let slots = self.section_slots();
        let changes: Vec<(SectAt, String)> = indexes
            .iter()
            .filter_map(|&k| slots.get(k).copied())
            .filter_map(|at| {
                let old = self.sect_raw(at);
                let new = edit(old);
                (new != old).then_some((at, new))
            })
            .collect();
        if changes.is_empty() {
            return false;
        }
        self.checkpoint(EditKind::Structural);
        for (at, raw) in changes {
            self.set_sect_raw(at, raw);
        }
        true
    }

    /// Replace whole sectPrs, `(section, raw)`, as one undo step: for edits
    /// computed elsewhere, such as the header references a watermark adds
    /// (`Package::set_text_watermark`). Nothing is recorded when none
    /// changes. Whether any did.
    pub fn replace_sections(&mut self, raws: &[(usize, String)]) -> bool {
        let slots = self.section_slots();
        let changes: Vec<(SectAt, String)> = raws
            .iter()
            .filter_map(|(k, raw)| {
                let at = *slots.get(*k)?;
                (self.sect_raw(at) != raw).then(|| (at, raw.clone()))
            })
            .collect();
        if changes.is_empty() {
            return false;
        }
        self.checkpoint(EditKind::Structural);
        for (at, raw) in changes {
            self.set_sect_raw(at, raw);
        }
        true
    }

    /// [`Editor::edit_sections`] through the typed [`SectionSetup`] view.
    pub fn edit_section_setups(
        &mut self,
        indexes: &[usize],
        edit: impl Fn(&mut SectionSetup),
    ) -> bool {
        self.edit_sections(indexes, |raw| {
            let mut setup = SectionSetup::parse(raw);
            edit(&mut setup);
            setup.apply(raw)
        })
    }

    pub(super) fn set_sect_raw(&mut self, at: SectAt, raw: String) {
        match at {
            SectAt::Para(i) => {
                if let Some(Block::Paragraph(p)) = self.doc.body.get_mut(i) {
                    p.props.section_break = Some(raw);
                }
            }
            SectAt::Trailing => match self.doc.trailing_section_properties_mut() {
                Some(section) => section.raw = raw,
                None => self.doc.set_trailing_section_properties(SectionProperties {
                    raw,
                    property_change: None,
                }),
            },
        }
    }

    /// Insert a section break at the caret, as one undo step. With a
    /// selection, the selected text is deleted and the break goes where it
    /// started, as one step; a selection that cannot be deleted (one that
    /// spans a table) is refused with the reason, as is a caret that is not
    /// in a body paragraph: a table cell, a text box, or another story
    /// cannot carry a sectPr.
    ///
    /// The caret's paragraph P, in section k, splits at the caret. The first
    /// half takes a copy of section k's sectPr with its original `w:type`, so it
    /// closes a section just like the old one. Section k's own sectPr now closes
    /// the section after the break, and `w:type` describes how *that* section
    /// starts, so it takes `start`. The second half keeps P's own break, if P
    /// had one.
    pub fn insert_section_break(&mut self, start: SectionStart) -> Result<(), String> {
        if !self.has_selection() {
            return self.insert_section_break_with(start, |_| {});
        }
        // Validate before anything changes: the selection must start in a
        // body paragraph and be one delete_selection container, so the delete
        // below cannot bail out halfway (a cell range empties cells; a
        // cross-container range just collapses — Word deletes neither here).
        if self.cell_range().is_some() {
            return Err("A section break cannot replace a selection of table cells".into());
        }
        self.break_point()?;
        let (lo, hi) = self.selection_range().expect("checked above");
        if spans_containers(&lo, &hi) {
            return Err("A section break cannot replace a selection that spans a table".into());
        }
        self.one_step("Section break", |e| {
            e.delete_selection();
            let _ = e.insert_section_break_with(start, |_| {});
        });
        Ok(())
    }

    /// Insert a section break at the selection's start (its text is kept,
    /// unlike [`Editor::insert_section_break`]) or at the caret, then `edit`
    /// on the section after the break, all as one undo step: Page Setup's and
    /// Columns' "This point forward".
    pub fn insert_section_break_with(
        &mut self,
        start: SectionStart,
        edit: impl FnOnce(&mut SectionSetup),
    ) -> Result<(), String> {
        let (at, _, _) = self.break_point()?;
        self.anchor = None;
        self.caret = at.clone();
        self.checkpoint(EditKind::Structural);
        self.split_section_at(&at, start, edit)?;
        self.settle_revisions();
        Ok(())
    }

    /// Split the body paragraph at `at` and give the section after the split
    /// `start` and `edit`, with no undo step taken and the selection left to
    /// the caller (the caret ends at the second half's start). The first
    /// half closes a copy of the section's sectPr with its original `w:type`.
    fn split_section_at(
        &mut self,
        at: &Caret,
        start: SectionStart,
        edit: impl FnOnce(&mut SectionSetup),
    ) -> Result<(), String> {
        let &[block] = at.path.as_slice() else {
            return Err("A section break can only go in the body text, not here".into());
        };
        let k = self.section_of_block(block);
        let slot = self.section_slots()[k];
        let old = self.sect_raw(slot).to_string();
        let mut setup = SectionSetup::parse(&old);
        setup.start = start;
        edit(&mut setup);
        let closing = setup.apply(&old);

        let Some(Block::Paragraph(p)) = self.doc.body.get_mut(block) else {
            return Err("A section break needs the caret in a paragraph".into());
        };
        let right = split_paragraph_content(&mut p.content, at.offset);
        let second = Paragraph {
            props: p.props.clone(),
            content: right,
        };
        p.props.section_break = Some(old);
        p.props.section_property_change = None;
        // A tracked change of the paragraph mark stays with the physical
        // mark, which ends the second half.
        crate::review::clear_mark_revisions(&mut p.props);
        self.doc.body.insert(block + 1, Block::Paragraph(second));
        // Section k's sectPr moved one section on; its slot shifts with the
        // inserted paragraph when it was a paragraph at or after the split.
        let moved = match slot {
            SectAt::Para(i) if i == block => SectAt::Para(block + 1),
            SectAt::Para(i) => SectAt::Para(i + 1),
            SectAt::Trailing => SectAt::Trailing,
        };
        self.set_sect_raw(moved, closing);
        self.caret = Caret::at(vec![block + 1], 0);
        Ok(())
    }

    /// End the section after body paragraph `after` without splitting it: its
    /// mark takes a copy of the section's sectPr (original `w:type`), and the
    /// sectPr behind it takes `start` and `edit`.
    fn cut_section_after(
        &mut self,
        after: usize,
        start: SectionStart,
        edit: impl FnOnce(&mut SectionSetup),
    ) {
        let k = self.section_of_block(after);
        let slot = self.section_slots()[k];
        let old = self.sect_raw(slot).to_string();
        let mut setup = SectionSetup::parse(&old);
        setup.start = start;
        edit(&mut setup);
        let closing = setup.apply(&old);
        if let Some(Block::Paragraph(p)) = self.doc.body.get_mut(after) {
            p.props.section_break = Some(old);
            p.props.section_property_change = None;
        }
        self.set_sect_raw(slot, closing);
    }

    /// How the selection's two ends cut their section for
    /// [`Editor::wrap_selection_in_section`], or `None` when it cannot be
    /// wrapped: no selection, a table-cell range, an end outside a top-level
    /// body paragraph, ends in different sections, or both ends already at
    /// the section's edges (nothing would be cut).
    fn selection_cuts(&self) -> Option<(Caret, Caret, Cut, Cut)> {
        if self.cell_range().is_some() {
            return None;
        }
        let (mut lo, mut hi) = self.selection_range()?;
        let (&[mut lb], &[mut hb]) = (lo.path.as_slice(), hi.path.as_slice()) else {
            return None;
        };
        let len = |b: usize| match self.doc.body.get(b) {
            Some(Block::Paragraph(p)) => Some(p.content.iter().map(inline_len).sum::<usize>()),
            _ => None,
        };
        let is_para = |b: usize| matches!(self.doc.body.get(b), Some(Block::Paragraph(_)));
        // Edges of an empty paragraph stay: its one offset is both its start
        // and its end, and the paragraph is selected text.
        let mut lo_gap = None;
        let mut hi_moved = false;
        if len(lb)? > 0 && len(lb)? == lo.offset && lb < hb {
            // From a paragraph's end: the selection starts with its mark, so
            // the text starts at the next block. A table there cannot start
            // the section by a split without a blank paragraph before it, so
            // the mark ends the section instead.
            if is_para(lb + 1) {
                lb += 1;
                lo = Caret::at(vec![lb], 0);
            } else {
                lo_gap = Some(lb);
            }
        }
        if hi.offset == 0 && len(hb)? > 0 && hb > lb && is_para(hb - 1) {
            // To a paragraph's start: the text ends with the previous mark.
            hb -= 1;
            hi_moved = true;
            hi = Caret::at(vec![hb], len(hb)?);
        }
        let hlen = len(hb)?;
        // An empty paragraph's own mark is a selection (moved onto by the end
        // rule); a bare paragraph mark before it is not.
        if (hb, hi.offset) < (lb, lo.offset) || ((hb, hi.offset) == (lb, lo.offset) && !hi_moved) {
            return None;
        }
        let k = self.section_of_block(lb);
        if self.section_of_block(hb) != k {
            return None;
        }
        let lo_cut = if let Some(p) = lo_gap {
            Cut::Gap(p)
        } else if lo.offset > 0 {
            Cut::Mid
        } else if lb == 0 || self.section_of_block(lb - 1) != k {
            Cut::Edge
        } else if is_para(lb - 1) {
            Cut::Gap(lb - 1)
        } else {
            Cut::Mid
        };
        let hi_cut = if hi.offset == hlen {
            let carries = matches!(
                &self.doc.body[hb],
                Block::Paragraph(p) if p.props.section_break.is_some()
            );
            if carries || hb + 1 >= self.doc.content_block_count() {
                Cut::Edge
            } else {
                Cut::Gap(hb)
            }
        } else {
            Cut::Mid
        };
        if lo_cut == Cut::Edge && hi_cut == Cut::Edge {
            return None;
        }
        Some((lo, hi, lo_cut, hi_cut))
    }

    /// The section [`Editor::wrap_selection_in_section`] edits: the one the
    /// selected text lies in.
    pub fn selection_wrap_section(&self) -> Option<usize> {
        let (lo, ..) = self.selection_cuts()?;
        lo.path.first().map(|&b| self.section_of_block(b))
    }

    /// Whether [`Editor::wrap_selection_in_section`] applies: a text
    /// selection inside one section that does not cover all of it.
    pub fn selection_section_wrappable(&self) -> bool {
        self.selection_cuts().is_some()
    }

    /// Wrap the selection in section breaks and `edit` the section made of
    /// just the selected text (Word's "Apply to: Selected text"), as one
    /// undo step. The middle section starts as `start` says (unless the
    /// selection starts at its section's start, where the existing break
    /// stays), and so does the section after it. No empty paragraph is made
    /// where a selection edge is at a paragraph edge: the neighbouring
    /// paragraph's mark carries the break, except where a table is the
    /// neighbour before the text: a table cannot carry a sectPr, so an empty
    /// paragraph after it does. The selection stays on the same text.
    pub fn wrap_selection_in_section(
        &mut self,
        start: SectionStart,
        edit: impl FnOnce(&mut SectionSetup),
    ) -> Result<(), String> {
        let Some((lo, hi, lo_cut, hi_cut)) = self.selection_cuts() else {
            return Err("Selected text needs a text selection inside one section".into());
        };
        let anchor = self.anchor.clone();
        let caret = self.caret.clone();
        self.checkpoint(EditKind::Structural);
        // The end first, so the start's position stays valid.
        match &hi_cut {
            Cut::Edge => {}
            Cut::Gap(p) => self.cut_section_after(*p, start, |_| {}),
            Cut::Mid => self.split_section_at(&hi, start, |_| {})?,
        }
        match &lo_cut {
            Cut::Edge => {
                let slot = self.section_slots()[self.section_of_block(lo.path[0])];
                let old = self.sect_raw(slot).to_string();
                let mut setup = SectionSetup::parse(&old);
                edit(&mut setup);
                let new = setup.apply(&old);
                self.set_sect_raw(slot, new);
            }
            Cut::Gap(p) => self.cut_section_after(*p, start, edit),
            Cut::Mid => self.split_section_at(&lo, start, edit)?,
        }
        // Keep the selection on its text: a split at the start moved
        // everything from it on into the next paragraph.
        let shift = |c: &Caret| match (&lo_cut, c.path.as_slice()) {
            (Cut::Mid, &[b]) if b > lo.path[0] => Caret::at(vec![b + 1], c.offset),
            (Cut::Mid, &[b]) if b == lo.path[0] && c.offset >= lo.offset => {
                Caret::at(vec![b + 1], c.offset - lo.offset)
            }
            _ => c.clone(),
        };
        self.anchor = anchor.as_ref().map(shift);
        self.caret = shift(&caret);
        self.settle_revisions();
        Ok(())
    }

    /// Put `blocks` at the start of the document as a section of their own,
    /// closed by `sect` (carried by their last paragraph, or a new empty one
    /// when they end in a table), as one undo step. The section that was
    /// first now starts on a new page and keeps its own sectPr (headers,
    /// footers, page setup). Mail merge's envelope (#628) uses it.
    pub fn insert_section_at_start(&mut self, mut blocks: Vec<Block>, sect: String) {
        match blocks.last_mut() {
            Some(Block::Paragraph(p)) if p.props.section_break.is_none() => {
                p.props.section_break = Some(sect);
            }
            _ => {
                let mut p = Paragraph::default();
                p.props.section_break = Some(sect);
                blocks.push(Block::Paragraph(p));
            }
        }
        self.checkpoint(EditKind::Structural);
        let first = self.section_slots()[0];
        let old = self.sect_raw(first).to_string();
        let mut setup = SectionSetup::parse(&old);
        setup.start = SectionStart::NextPage;
        let next = setup.apply(&old);
        if next != old {
            self.set_sect_raw(first, next);
        }
        self.doc.body.splice(0..0, blocks);
        self.anchor = None;
        self.caret = Caret::at(vec![0], 0);
        self.doc.initialize_revision_targets();
    }

    /// Replace the whole body with `blocks` as one section whose sectPr is
    /// the final section's rewritten by `edit`, as one undo step (Start Mail
    /// Merge ▸ Labels, after Word's "delete the contents" confirmation). The
    /// caret goes to the first paragraph.
    pub fn replace_body(&mut self, blocks: Vec<Block>, edit: impl FnOnce(&str) -> String) {
        self.checkpoint(EditKind::Structural);
        let last = *self.section_slots().last().expect("a final section");
        let sect = edit(self.sect_raw(last));
        let keep = self.doc.content_block_count();
        self.doc.body.splice(0..keep, blocks);
        self.set_sect_raw(SectAt::Trailing, sect);
        self.anchor = None;
        let first = super::first_paragraph_path(&self.doc.body).unwrap_or_else(|| vec![0]);
        self.caret = Caret::at(first, 0);
        self.doc.initialize_revision_targets();
    }

    /// Where a section break goes: the selection's start, else the caret (for
    /// [`Editor::insert_section_break`] the start after the selection's
    /// deletion); its body block; and the section it lands in, the one whose
    /// setup the section after the break takes. Refused outside a body
    /// paragraph.
    fn break_point(&self) -> Result<(Caret, usize, usize), String> {
        let at = match self.selection_range() {
            Some((lo, _)) => lo,
            None => self.caret.clone(),
        };
        let &[block] = at.path.as_slice() else {
            return Err("A section break can only go in the body text, not here".into());
        };
        if !matches!(self.doc.body.get(block), Some(Block::Paragraph(_))) {
            return Err("A section break needs the caret in a paragraph".into());
        }
        let k = self.section_of_block(block);
        Ok((at, block, k))
    }

    /// The section a break at this point lands in: the one
    /// [`Editor::insert_section_break_with`] edits for the section after the
    /// break. Page Setup's and Columns' This point forward check that one.
    pub fn break_section(&self) -> Result<usize, String> {
        self.break_point().map(|(_, _, k)| k)
    }

    /// Insert a page, column or clearing line break at the caret.
    ///
    /// Like a tab (see [`Editor::insert_tab`]), the break takes the formatting
    /// typing at the caret would, except next to a hyperlink, and text typed
    /// after it keeps that formatting (#279).
    pub fn insert_break(&mut self, kind: BreakKind) {
        self.delete_selection_to_replace();
        let props = resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| tab_props_at(&p.content, self.caret.offset))
            .unwrap_or_default();
        self.paste(&Clip {
            paras: vec![vec![Inline::Break(kind, props)]],
        });
    }

    /// Whether the caret's paragraph suppresses line numbers
    /// (`w:suppressLineNumbers`).
    pub fn caret_suppresses_line_numbers(&self) -> bool {
        super::resolve_para(&self.doc.body, &self.caret.path).is_some_and(suppresses_line_numbers)
    }

    /// Toggle `w:suppressLineNumbers` on the selected paragraphs (else the
    /// caret's), as one undo step: on unless the caret's paragraph has it.
    pub fn toggle_suppress_line_numbers(&mut self) {
        let on = !self.caret_suppresses_line_numbers();
        self.for_each_para(|props| {
            props
                .raw_props
                .retain(|r| !is_element(r, "w:suppressLineNumbers"));
            if on {
                props.raw_props.push("<w:suppressLineNumbers/>".into());
            }
        });
    }
}

fn is_element(raw: &str, name: &str) -> bool {
    raw.strip_prefix('<')
        .and_then(|r| r.strip_prefix(name))
        .is_some_and(|r| r.starts_with([' ', '/', '>', '\t', '\n', '\r']))
}

fn suppresses_line_numbers(p: &Paragraph) -> bool {
    p.props
        .raw_props
        .iter()
        .any(|r| crate::sect::has_flag(r, "w:suppressLineNumbers"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Document, ParProps, PropertyChange, PropertyScope, PropertySnapshot, PropertyState,
        RevisionMetadata, Run, Table,
    };

    fn para(text: &str, sect: Option<&str>) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps {
                section_break: sect.map(str::to_string),
                ..ParProps::default()
            },
            content: vec![Inline::Run(Run {
                text: text.into(),
                ..Run::default()
            })],
        })
    }

    fn sect(tag: &str) -> String {
        format!("<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/>{tag}</w:sectPr>")
    }

    /// Three sections: "one" closes section 0 (continuous), "two" and "two b"
    /// are section 1 (closed by "two b", odd page), "three" is the final one.
    fn three() -> Editor {
        let mut doc = Document {
            body: vec![
                para("one", Some(&sect("<w:type w:val=\"continuous\"/>"))),
                para("two", None),
                para("two b", Some(&sect("<w:type w:val=\"oddPage\"/>"))),
                para("three", None),
            ],
        };
        doc.set_trailing_section_properties(SectionProperties {
            raw: sect(""),
            property_change: None,
        });
        Editor::new(doc)
    }

    fn start_of(raw: &str) -> SectionStart {
        SectionSetup::parse(raw).start
    }

    #[test]
    fn sections_follow_the_body() {
        let mut e = three();
        assert_eq!(e.sections().len(), 3);
        assert_eq!(e.section_of_block(0), 0);
        assert_eq!(e.section_of_block(1), 1);
        assert_eq!(e.section_of_block(2), 1);
        assert_eq!(e.section_of_block(3), 2);
        e.caret = Caret::top(1, 1);
        assert_eq!(e.target_sections(), vec![1]);
        e.anchor = Some(Caret::top(0, 1));
        assert_eq!(e.target_sections(), vec![0, 1]);
    }

    #[test]
    fn edit_sections_changes_only_the_target_in_one_step() {
        let mut e = three();
        e.caret = Caret::top(1, 0);
        let before = e.doc.clone();
        let k = e.target_sections();
        assert!(e.edit_section_setups(&k, |s| s.margins.left = 720));
        let now = e.sections();
        assert_eq!(SectionSetup::parse(&now[1]).margins.left, 720);
        assert_eq!(now[0], before_sections(&before)[0]);
        assert_eq!(now[2], before_sections(&before)[2]);
        assert!(e.undo());
        assert_eq!(e.doc, before);
        assert!(e.redo());
        assert_eq!(SectionSetup::parse(&e.sections()[1]).margins.left, 720);
        // A no-op edit records nothing.
        assert!(!e.edit_section_setups(&[1], |s| s.margins.left = 720));
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    fn before_sections(doc: &Document) -> Vec<String> {
        Editor::new(doc.clone()).sections()
    }

    #[test]
    fn replace_sections_is_one_undo_step() {
        let mut e = three();
        let before = e.doc.clone();
        let mut want = e.sections();
        want[0] =
            "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId9\"/></w:sectPr>".into();
        want[2] = "<w:sectPr><w:pgSz w:w=\"100\"/></w:sectPr>".into();
        let raws = [(0, want[0].clone()), (2, want[2].clone()), (9, "x".into())];
        assert!(e.replace_sections(&raws));
        assert_eq!(e.sections(), want);
        assert!(!e.replace_sections(&raws), "unchanged records nothing");
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    #[test]
    fn edit_sections_reaches_the_final_section() {
        let mut e = three();
        e.caret = Caret::top(3, 0);
        let k = e.target_sections();
        assert_eq!(k, vec![2]);
        e.edit_section_setups(&k, |s| s.set_landscape(true));
        let raw = &e.doc.trailing_section_properties().unwrap().raw;
        assert!(raw.contains("w:orient=\"landscape\""), "{raw}");
    }

    #[test]
    fn a_section_break_types_the_section_after_it() {
        let mut e = three();
        e.caret = Caret::top(1, 1); // "t|wo" in section 1
        let before = e.doc.clone();
        e.insert_section_break(SectionStart::Continuous).unwrap();
        let s = e.sections();
        assert_eq!(s.len(), 4);
        // The new break on "t" keeps section 1's old type ...
        assert_eq!(start_of(&s[1]), SectionStart::OddPage);
        // ... and section 1's sectPr, now after the break, reads continuous.
        assert_eq!(start_of(&s[2]), SectionStart::Continuous);
        assert_eq!(start_of(&s[0]), SectionStart::Continuous);
        let text: Vec<String> = e.doc.body.iter().map(|b| b.plain_text()).collect();
        assert_eq!(text[..5], ["one", "t", "wo", "two b", "three"]);
        assert_eq!(e.caret, Caret::top(2, 0));
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    /// One section "alpha beta", "gamma", "delta" (three paragraphs), a
    /// continuous break after the first section of a longer document in
    /// `wrap_doc`.
    fn wrap_doc() -> Editor {
        let mut doc = Document {
            body: vec![
                para("alpha beta", None),
                para("gamma", None),
                para("delta", None),
                para("tail", Some(&sect("<w:type w:val=\"oddPage\"/>"))),
                para("next", None),
            ],
        };
        doc.set_trailing_section_properties(SectionProperties {
            raw: sect(""),
            property_change: None,
        });
        Editor::new(doc)
    }

    fn texts(e: &Editor) -> Vec<String> {
        e.doc.body.iter().map(|b| b.plain_text()).collect()
    }

    fn select(e: &mut Editor, a: (usize, usize), c: (usize, usize)) {
        e.anchor = Some(Caret::top(a.0, a.1));
        e.caret = Caret::top(c.0, c.1);
    }

    fn selected_text(e: &Editor) -> String {
        assert!(e.has_selection());
        let mut out = Vec::new();
        for (path, a, b) in e.selection_spans() {
            let t = e.doc.body[path[0]].plain_text();
            out.push(t.chars().skip(a).take(b - a).collect::<String>());
        }
        out.join("|")
    }

    fn margin(e: &Editor, k: usize) -> i32 {
        SectionSetup::parse(&e.sections()[k]).margins.left
    }

    #[test]
    fn wrapping_mid_paragraph_text_makes_three_sections_with_the_edit_on_the_middle() {
        let mut e = wrap_doc();
        select(&mut e, (0, 2), (0, 7)); // "pha b"
        let before = e.doc.clone();
        assert!(e.selection_section_wrappable());
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(texts(&e)[..4], ["al", "pha b", "eta", "gamma"]);
        let s = e.sections();
        assert_eq!(s.len(), 4);
        // Before keeps the original, the middle is edited and Continuous,
        // the one after is the original body, Continuous.
        assert_eq!(start_of(&s[0]), SectionStart::OddPage);
        assert_ne!(margin(&e, 0), 720);
        assert_eq!(start_of(&s[1]), SectionStart::Continuous);
        assert_eq!(margin(&e, 1), 720);
        assert_eq!(start_of(&s[2]), SectionStart::Continuous);
        assert_ne!(margin(&e, 2), 720);
        // The selection stays on the same text.
        assert_eq!(selected_text(&e), "pha b");
        assert!(e.undo());
        assert_eq!(e.doc, before);
        assert_eq!(
            e.selection_range(),
            Some((Caret::top(0, 2), Caret::top(0, 7)))
        );
    }

    #[test]
    fn wrapping_inside_one_paragraph_keeps_the_text_and_one_undo_step() {
        let mut e = wrap_doc();
        select(&mut e, (1, 1), (1, 3)); // "am"
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(texts(&e)[1..4], ["g", "am", "ma"]);
        assert_eq!(selected_text(&e), "am");
        assert!(e.undo());
        assert_eq!(texts(&e)[1], "gamma");
        assert_eq!(e.sections().len(), 2);
    }

    #[test]
    fn wrapping_whole_paragraphs_makes_no_empty_paragraph() {
        let mut e = wrap_doc();
        select(&mut e, (1, 0), (1, 5)); // all of "gamma"
        let n = e.doc.body.len();
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(e.doc.body.len(), n, "marks carry the breaks");
        assert_eq!(texts(&e)[..3], ["alpha beta", "gamma", "delta"]);
        let s = e.sections();
        assert_eq!(s.len(), 4);
        assert_eq!(margin(&e, 0), SectionSetup::parse(&sect("")).margins.left);
        assert_eq!(margin(&e, 1), 720);
        assert_eq!(start_of(&s[1]), SectionStart::Continuous);
        assert_eq!(start_of(&s[2]), SectionStart::Continuous);
        assert_eq!(selected_text(&e), "gamma");
        // Selected back to front gives the same result.
        let mut r = wrap_doc();
        select(&mut r, (1, 5), (1, 0));
        r.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(r.doc.body, e.doc.body);
        assert_eq!(r.caret, Caret::top(1, 0));
        assert!(e.undo());
        assert_eq!(e.sections().len(), 2);
    }

    #[test]
    fn a_selection_ending_at_the_next_paragraph_start_cuts_the_same_gap() {
        let mut e = wrap_doc();
        select(&mut e, (1, 0), (2, 0));
        let n = e.doc.body.len();
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(e.doc.body.len(), n);
        assert_eq!(e.sections().len(), 4);
        assert_eq!(margin(&e, 1), 720);
    }

    #[test]
    fn wrapping_from_the_section_start_or_to_its_end_adds_one_break() {
        // Starts at the document start: only the end cuts.
        let mut e = wrap_doc();
        select(&mut e, (0, 0), (0, 5)); // "alpha"
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(texts(&e)[..2], ["alpha", " beta"]);
        let s = e.sections();
        assert_eq!(s.len(), 3);
        assert_eq!(margin(&e, 0), 720);
        assert_eq!(margin(&e, 1), SectionSetup::parse(&sect("")).margins.left);
        assert_eq!(start_of(&s[1]), SectionStart::Continuous);
        assert_eq!(selected_text(&e), "alpha");

        // Reaches the end of its section ("tail" carries the break): only
        // the start cuts, the closing break stays the section's own.
        let mut e = wrap_doc();
        select(&mut e, (3, 2), (3, 4)); // "il"
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(texts(&e)[3..5], ["ta", "il"]);
        let s = e.sections();
        assert_eq!(s.len(), 3);
        assert_eq!(start_of(&s[0]), SectionStart::OddPage, "the part before");
        assert_eq!(margin(&e, 1), 720);
        assert_eq!(start_of(&s[1]), SectionStart::Continuous);
        assert_eq!(selected_text(&e), "il");
    }

    #[test]
    fn wrapping_after_a_table_splits_at_the_paragraph_start() {
        let mut e = wrap_doc();
        e.doc.body.insert(1, Block::Table(Table::default()));
        select(&mut e, (2, 0), (2, 5)); // all of "gamma", a table before it
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(selected_text(&e), "gamma");
        assert_eq!(e.sections().len(), 4);
        assert_eq!(margin(&e, 1), 720);
    }

    #[test]
    fn a_selection_from_a_paragraph_end_makes_no_empty_paragraph_1272() {
        let mut e = wrap_doc();
        select(&mut e, (0, 10), (1, 5)); // "alpha beta" mark, all of "gamma"
        let n = e.doc.body.len();
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(e.doc.body.len(), n, "no blank half");
        assert_eq!(texts(&e)[..3], ["alpha beta", "gamma", "delta"]);
        assert_eq!(margin(&e, 0), SectionSetup::parse(&sect("")).margins.left);
        assert_eq!(margin(&e, 1), 720);
        assert_eq!(e.sections().len(), 4);
        assert_eq!(
            e.selection_range(),
            Some((Caret::top(0, 10), Caret::top(1, 5)))
        );
        // Only a paragraph mark selected: nothing to wrap.
        let mut e = wrap_doc();
        select(&mut e, (0, 10), (1, 0));
        assert!(!e.selection_section_wrappable());
    }

    #[test]
    fn edges_in_the_neighbouring_section_are_taken_into_the_selections_own_1272() {
        // To the start of the next section's first paragraph ("next").
        let mut e = wrap_doc();
        select(&mut e, (3, 2), (4, 0));
        assert!(e.selection_section_wrappable());
        assert_eq!(e.selection_wrap_section(), Some(0));
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(texts(&e)[3..6], ["ta", "il", "next"]);
        assert_eq!(margin(&e, 1), 720);
        // From the end of the last paragraph of the previous section.
        let mut e = wrap_doc();
        select(&mut e, (3, 4), (4, 2)); // "tail" mark, "ne"
        assert!(e.selection_section_wrappable());
        assert_eq!(e.selection_wrap_section(), Some(1));
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(texts(&e)[4..6], ["ne", "xt"]);
        assert_eq!(margin(&e, 1), 720);
        assert_eq!(e.sections().len(), 3);
    }

    #[test]
    fn a_selection_from_a_paragraph_end_before_a_table_ends_the_section_at_its_mark_1272() {
        let mut e = wrap_doc();
        e.doc.body.insert(1, Block::Table(Table::default()));
        select(&mut e, (0, 10), (2, 3)); // mark, table, "gam"
        let n = e.doc.body.len();
        let before = e.doc.clone();
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(e.doc.body.len(), n + 1, "only the end splits");
        assert_eq!(texts(&e)[0], "alpha beta");
        assert_eq!(margin(&e, 1), 720);
        assert_eq!(e.sections().len(), 4);
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    fn blanks(texts: &[&str]) -> Editor {
        let mut doc = Document {
            body: texts.iter().map(|t| para(t, None)).collect(),
        };
        doc.set_trailing_section_properties(SectionProperties {
            raw: sect(""),
            property_change: None,
        });
        Editor::new(doc)
    }

    #[test]
    fn an_empty_paragraph_at_an_edge_stays_in_the_selection_1272() {
        // Select All with a blank first or last paragraph is the whole section.
        let mut e = blanks(&["", "a", "b"]);
        select(&mut e, (0, 0), (2, 1));
        assert!(!e.selection_section_wrappable());
        let mut e = blanks(&["a", "b", ""]);
        select(&mut e, (0, 0), (2, 0));
        assert!(!e.selection_section_wrappable());
        // The blank paragraph is inside the middle section.
        let mut e = blanks(&["alpha", "", "gamma", "delta"]);
        select(&mut e, (1, 0), (2, 5));
        let n = e.doc.body.len();
        e.wrap_selection_in_section(SectionStart::Continuous, |s| s.margins.left = 720)
            .unwrap();
        assert_eq!(e.doc.body.len(), n);
        assert_eq!(texts(&e)[..3], ["alpha", "", "gamma"]);
        // alpha | "" gamma | delta: the break marks are on "alpha" and "gamma".
        assert!(matches!(&e.doc.body[0], Block::Paragraph(p) if p.props.section_break.is_some()));
        assert!(matches!(&e.doc.body[2], Block::Paragraph(p) if p.props.section_break.is_some()));
        assert_eq!(margin(&e, 1), 720);
        // From an empty paragraph to the next one's start.
        let mut e = blanks(&["alpha", "", "gamma"]);
        select(&mut e, (1, 0), (2, 0));
        assert!(e.selection_section_wrappable());
    }

    #[test]
    fn a_selection_that_cannot_be_wrapped_is_refused() {
        let mut e = wrap_doc();
        // A caret only.
        assert!(!e.selection_section_wrappable());
        // The whole section.
        select(&mut e, (0, 0), (3, 4));
        assert!(!e.selection_section_wrappable());
        // Across a section break.
        select(&mut e, (1, 1), (4, 2));
        assert!(!e.selection_section_wrappable());
        let before = e.doc.clone();
        assert!(
            e.wrap_selection_in_section(SectionStart::Continuous, |_| {})
                .is_err()
        );
        assert_eq!(e.doc, before);
        // Inside a table cell.
        let mut e = wrap_doc();
        e.doc.body.insert(0, Block::Table(Table::default()));
        e.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        e.caret = Caret::at(vec![0, 0, 0, 0], 1);
        assert!(!e.selection_section_wrappable());
    }

    #[test]
    fn show_hide_labels_a_new_section_break_by_the_section_it_starts_645() {
        let mut e = three();
        e.caret = Caret::top(1, 1);
        e.insert_section_break(SectionStart::Continuous).unwrap();
        let opts = crate::render::RenderOptions {
            width: 40,
            show_invisibles: true,
            ..Default::default()
        };
        let lines: Vec<String> = crate::render::render(&e.doc, &opts)
            .iter()
            .map(|l| l.plain())
            .collect();
        let marks: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("Section Break"))
            .collect();
        // Each mark reads the sectPr of the section after it: "t" starts odd
        // page (the old type of "two b"), "wo" continuous (the new break's),
        // and "three" next page (the trailing sectPr).
        assert_eq!(marks.len(), 3, "{lines:?}");
        assert!(marks[0].contains("(Odd Page)"), "{marks:?}");
        assert!(marks[1].contains("(Continuous)"), "{marks:?}");
        assert!(marks[2].contains("(Next Page)"), "{marks:?}");
    }

    #[test]
    fn a_break_in_a_closing_paragraph_leaves_its_break_on_the_second_half() {
        let mut e = three();
        e.caret = Caret::top(2, 3); // "two| b", which closes section 1
        e.insert_section_break(SectionStart::EvenPage).unwrap();
        let Block::Paragraph(first) = &e.doc.body[2] else {
            panic!()
        };
        let Block::Paragraph(second) = &e.doc.body[3] else {
            panic!()
        };
        assert_eq!(
            start_of(first.props.section_break.as_deref().unwrap()),
            SectionStart::OddPage
        );
        assert_eq!(
            start_of(second.props.section_break.as_deref().unwrap()),
            SectionStart::EvenPage
        );
        assert_eq!(e.sections().len(), 4);
    }

    #[test]
    fn a_break_in_the_final_section_types_the_trailing_sectpr() {
        let mut e = three();
        e.caret = Caret::top(3, 5);
        e.insert_section_break(SectionStart::NextPage).unwrap();
        let s = e.sections();
        assert_eq!(s.len(), 4);
        assert_eq!(s[2], sect(""), "the copy keeps the old (default) type");
        // nextPage is the default: no w:type is written.
        assert_eq!(start_of(&s[3]), SectionStart::NextPage);
        e.insert_section_break(SectionStart::OddPage).unwrap();
        assert_eq!(start_of(&e.sections()[4]), SectionStart::OddPage);
    }

    #[test]
    fn a_break_and_an_edit_of_the_section_after_it_undo_together() {
        let mut e = three();
        e.caret = Caret::top(1, 1);
        let before = e.doc.clone();
        e.insert_section_break_with(SectionStart::NextPage, |s| s.set_landscape(true))
            .unwrap();
        let s = e.sections();
        assert!(!SectionSetup::parse(&s[1]).page.landscape);
        assert!(SectionSetup::parse(&s[2]).page.landscape);
        assert_eq!(start_of(&s[2]), SectionStart::NextPage);
        assert!(e.undo());
        assert_eq!(e.doc, before, "one undo step");
    }

    #[test]
    fn a_section_break_is_refused_in_a_table_cell() {
        let mut doc = three().doc;
        doc.body.insert(
            0,
            Block::Table(Table {
                rows: vec![crate::model::Row {
                    cells: vec![crate::model::Cell {
                        blocks: vec![para("cell", None)],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
        );
        let mut e = Editor::new(doc);
        e.caret = Caret::at(vec![0, 0, 0, 0], 2);
        let before = e.doc.clone();
        assert!(e.insert_section_break(SectionStart::Continuous).is_err());
        assert_eq!(e.doc, before);
        assert!(!e.undo(), "a refusal records no undo step");
    }

    #[test]
    fn a_selection_is_replaced_by_the_break() {
        let mut e = three();
        e.anchor = Some(Caret::top(1, 0));
        e.caret = Caret::top(1, 1);
        e.insert_section_break(SectionStart::Continuous).unwrap();
        assert!(e.anchor.is_none());
        assert_eq!(text_of(&e, 1), "", "the selected \"t\" is gone");
        assert_eq!(text_of(&e, 2), "wo");
        assert_eq!(text_of(&e, 3), "two b");
        assert_eq!(text_of(&e, 4), "three");
        assert_eq!(e.sections().len(), 4);
    }

    #[test]
    fn a_break_over_a_selection_is_one_undo_step() {
        let mut e = three();
        e.anchor = Some(Caret::top(1, 0));
        e.caret = Caret::top(1, 1);
        let before = e.doc.clone();
        let sections_before = e.sections();
        e.insert_section_break(SectionStart::Continuous).unwrap();
        assert_eq!(text_of(&e, 1), "");
        assert!(e.undo(), "the delete and the break are one undo step");
        assert_eq!(e.doc, before);
        assert_eq!(e.sections(), sections_before);
        assert_eq!(e.anchor, Some(Caret::top(1, 0)), "undo reselects (#853)");
        assert_eq!(e.caret, Caret::top(1, 1));
    }

    #[test]
    fn a_multi_paragraph_selection_is_deleted_by_the_break() {
        let mut e = three();
        e.anchor = Some(Caret::top(0, 1));
        e.caret = Caret::top(2, 2);
        e.insert_section_break(SectionStart::NextPage).unwrap();
        // "one" keeps "o", "two" goes, the "two b" remainder "o b" merges onto
        // it, and the merged paragraph takes the last paragraph's oddPage
        // mark (keep_section_mark): "one"'s continuous mark is replaced.
        assert_eq!(text_of(&e, 0), "o");
        assert_eq!(text_of(&e, 1), "o b");
        assert_eq!(text_of(&e, 2), "three");
        assert!(
            !e.doc.plain_text().contains("two"),
            "the range text is gone"
        );
        // The delete merged two sections (one mark kept); the break adds one.
        assert_eq!(e.sections().len(), 3);
        assert_eq!(
            start_of(&e.sections()[0]),
            SectionStart::OddPage,
            "the merged paragraph keeps the last paragraph's mark"
        );
        assert_eq!(start_of(&e.sections()[1]), SectionStart::NextPage);
    }

    #[test]
    fn an_undeletable_selection_refuses_the_break() {
        let mut e = three();
        e.doc.body.push(Block::Table(Table {
            rows: vec![crate::model::Row {
                cells: vec![crate::model::Cell {
                    blocks: vec![para("cell", None)],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }));
        // The selection starts in a body paragraph and ends in the table: not
        // one delete_selection container, so the break is refused up front.
        e.anchor = Some(Caret::top(0, 1));
        e.caret = Caret::at(vec![4, 0, 0, 0], 2);
        let before = e.doc.clone();
        let err = e
            .insert_section_break(SectionStart::Continuous)
            .unwrap_err();
        assert!(err.contains("section break"), "{err}");
        assert_eq!(e.doc, before, "a refusal changes nothing");
        assert!(!e.undo(), "a refusal records no undo step");
        assert_eq!(e.anchor, Some(Caret::top(0, 1)), "the selection is kept");
    }

    #[test]
    fn a_selection_spanning_a_section_break_is_replaced_by_the_break() {
        let mut e = three();
        let saved = saved_sect_prs(&e);
        e.anchor = Some(Caret::top(1, 1));
        e.caret = Caret::top(3, 2);
        e.insert_section_break(SectionStart::Continuous).unwrap();
        assert_eq!(text_of(&e, 0), "one");
        assert_eq!(text_of(&e, 1), "t");
        assert_eq!(text_of(&e, 2), "ree");
        // The range covered "two b" whole, so its oddPage mark went with it:
        // no section reads oddPage any more, and Save writes as many sectPr
        // as before (the break's own mark replaces the deleted one).
        assert!(
            e.sections()
                .iter()
                .all(|s| start_of(s) != SectionStart::OddPage),
            "the spanned oddPage mark is deleted: {:?}",
            e.sections()
        );
        assert_eq!(e.sections().len(), 3);
        assert_eq!(saved_sect_prs(&e), saved);
    }

    #[test]
    fn insert_section_break_with_keeps_the_selected_text() {
        let mut e = three();
        e.anchor = Some(Caret::top(3, 4));
        e.caret = Caret::top(1, 1);
        e.insert_section_break_with(SectionStart::Continuous, |_| {})
            .unwrap();
        assert!(e.anchor.is_none());
        assert_eq!(text_of(&e, 1), "t", "Page Setup's point-forward keeps text");
        assert_eq!(text_of(&e, 2), "wo");
        assert_eq!(e.sections().len(), 4);
    }

    #[test]
    fn breaks_go_in_at_the_caret() {
        let mut e = three();
        e.caret = Caret::top(1, 1);
        e.insert_break(BreakKind::Clear(crate::model::ClearKind::All));
        let Block::Paragraph(p) = &e.doc.body[1] else {
            panic!()
        };
        assert!(p.content.iter().any(|i| matches!(
            i,
            Inline::Break(BreakKind::Clear(crate::model::ClearKind::All), _)
        )));
    }

    #[test]
    fn an_off_suppress_line_numbers_reads_as_off() {
        for (raw, on) in [
            ("<w:suppressLineNumbers/>", true),
            ("<w:suppressLineNumbers w:val=\"1\"/>", true),
            ("<w:suppressLineNumbers w:val='0'/>", false),
            ("<w:suppressLineNumbers w:val = \"false\"/>", false),
            ("<w:suppressLineNumbers w:val=\"off\"/>", false),
        ] {
            let mut e = three();
            e.caret = Caret::top(1, 0);
            if let Block::Paragraph(p) = &mut e.doc.body[1] {
                p.props.raw_props.push(raw.into());
            }
            assert_eq!(e.caret_suppresses_line_numbers(), on, "{raw}");
            // Toggling from off turns it on, replacing the explicit off.
            e.toggle_suppress_line_numbers();
            assert_eq!(e.caret_suppresses_line_numbers(), !on, "{raw}");
            let Block::Paragraph(p) = &e.doc.body[1] else {
                panic!()
            };
            assert!(
                p.props.raw_props.len() <= 1,
                "{raw}: {:?}",
                p.props.raw_props
            );
        }
    }

    #[test]
    fn break_section_is_where_a_forward_selection_starts() {
        let mut e = three();
        e.anchor = Some(Caret::top(0, 1));
        e.caret = Caret::top(3, 2);
        assert_eq!(e.caret_section(), 2);
        assert_eq!(e.break_section(), Ok(0));
        e.insert_section_break(SectionStart::Continuous).unwrap();
        assert_eq!(start_of(&e.sections()[1]), SectionStart::Continuous);
    }

    #[test]
    fn suppress_line_numbers_toggles_the_selected_paragraphs() {
        let mut e = three();
        e.anchor = Some(Caret::top(1, 0));
        e.caret = Caret::top(2, 2);
        assert!(!e.caret_suppresses_line_numbers());
        e.toggle_suppress_line_numbers();
        for i in [1, 2] {
            let Block::Paragraph(p) = &e.doc.body[i] else {
                panic!()
            };
            assert_eq!(p.props.raw_props, ["<w:suppressLineNumbers/>"]);
        }
        assert!(e.caret_suppresses_line_numbers());
        e.toggle_suppress_line_numbers();
        assert!(!e.caret_suppresses_line_numbers());
        let Block::Paragraph(p) = &e.doc.body[1] else {
            panic!()
        };
        assert!(p.props.raw_props.is_empty());
    }

    // ---- #748: splitting a section-closing paragraph keeps one break ----

    fn props_of(e: &Editor, i: usize) -> &ParProps {
        let Some(Block::Paragraph(p)) = e.doc.body.get(i) else {
            panic!("block {i} is not a paragraph")
        };
        &p.props
    }

    fn text_of(e: &Editor, i: usize) -> String {
        let Some(Block::Paragraph(p)) = e.doc.body.get(i) else {
            panic!("block {i} is not a paragraph")
        };
        p.content.iter().map(Inline::text).collect()
    }

    /// Every `<w:sectPr` Save would write, the body's trailing one included.
    fn saved_sect_prs(e: &Editor) -> usize {
        crate::serialize::document_to_xml(&e.doc)
            .matches("<w:sectPr")
            .count()
    }

    fn sect_change_raw(e: &Editor, i: usize) -> Option<String> {
        props_of(e, i)
            .section_property_change
            .as_ref()
            .map(|c| c.raw.clone())
    }

    fn sect_change() -> PropertyChange {
        let prior = sect("<w:type w:val=\"nextPage\"/>");
        PropertyChange {
            scope: PropertyScope::Section,
            metadata: RevisionMetadata::default(),
            raw: format!("<w:sectPrChange w:id=\"7\">{prior}</w:sectPrChange>"),
            previous: PropertySnapshot::Present(PropertyState::Section(prior)),
        }
    }

    /// `three()` with a tracked sectPr change on "one", the paragraph that
    /// closes section 0.
    fn three_with_a_sect_change() -> Editor {
        let mut e = three();
        if let Some(Block::Paragraph(p)) = e.doc.body.get_mut(0) {
            p.props.section_property_change = Some(sect_change());
        }
        e
    }

    #[test]
    fn enter_in_a_section_closing_paragraph_keeps_one_break_748() {
        let mut e = three();
        let before = e.doc.clone();
        let saved = saved_sect_prs(&e);
        let brk = props_of(&e, 0).section_break.clone();
        e.caret = Caret::top(0, 2);
        e.insert_newline();
        assert_eq!(e.sections().len(), 3);
        assert_eq!((text_of(&e, 0), text_of(&e, 1)), ("on".into(), "e".into()));
        assert_eq!(props_of(&e, 0).section_break, None);
        assert_eq!(props_of(&e, 0).section_property_change, None);
        assert_eq!(props_of(&e, 1).section_break, brk);
        assert_eq!(saved_sect_prs(&e), saved);
        let after = e.doc.clone();
        assert!(e.undo());
        assert_eq!(e.doc, before);
        assert!(e.redo());
        assert_eq!(e.doc, after);
    }

    #[test]
    fn enter_at_the_ends_of_a_section_closing_paragraph_748() {
        for (off, texts) in [(0, ("", "one")), (3, ("one", ""))] {
            let mut e = three();
            let saved = saved_sect_prs(&e);
            let brk = props_of(&e, 0).section_break.clone();
            e.caret = Caret::top(0, off);
            e.insert_newline();
            assert_eq!(e.sections().len(), 3, "offset {off}");
            assert_eq!(
                (text_of(&e, 0), text_of(&e, 1)),
                (texts.0.into(), texts.1.into()),
                "offset {off}"
            );
            assert_eq!(props_of(&e, 0).section_break, None, "offset {off}");
            assert_eq!(props_of(&e, 1).section_break, brk, "offset {off}");
            assert_eq!(saved_sect_prs(&e), saved, "offset {off}");
        }
    }

    #[test]
    fn enter_moves_a_section_property_change_with_the_break_748() {
        let mut e = three_with_a_sect_change();
        let saved = saved_sect_prs(&e);
        let brk = props_of(&e, 0).section_break.clone();
        e.caret = Caret::top(0, 2);
        e.insert_newline();
        assert_eq!(props_of(&e, 0).section_break, None);
        assert_eq!(props_of(&e, 0).section_property_change, None);
        assert_eq!(props_of(&e, 1).section_break, brk);
        assert_eq!(sect_change_raw(&e, 1), Some(sect_change().raw));
        assert_eq!(saved_sect_prs(&e), saved);
    }

    #[test]
    fn multi_paragraph_paste_into_a_section_closing_paragraph_748() {
        let run = |t: &str| {
            vec![Inline::Run(Run {
                text: t.into(),
                ..Run::default()
            })]
        };
        for n in [2, 3] {
            let mut e = three_with_a_sect_change();
            let saved = saved_sect_prs(&e);
            let brk = props_of(&e, 0).section_break.clone();
            let clip = Clip {
                paras: ["A", "B", "C"][..n].iter().map(|t| run(t)).collect(),
            };
            e.caret = Caret::top(0, 2);
            e.paste(&clip);
            assert_eq!(e.sections().len(), 3, "{n} paragraphs");
            assert_eq!(saved_sect_prs(&e), saved, "{n} paragraphs");
            let last = n - 1;
            assert_eq!(text_of(&e, last), format!("{}e", ["A", "B", "C"][last]));
            for i in 0..last {
                assert_eq!(props_of(&e, i).section_break, None, "{n}: para {i}");
                assert_eq!(props_of(&e, i).section_property_change, None, "{n}: {i}");
            }
            assert_eq!(props_of(&e, last).section_break, brk, "{n} paragraphs");
            assert_eq!(
                sect_change_raw(&e, last),
                Some(sect_change().raw),
                "{n} paragraphs"
            );
        }
    }

    #[test]
    fn insert_table_in_a_section_closing_paragraph_moves_its_property_change_748() {
        let mut e = three_with_a_sect_change();
        let saved = saved_sect_prs(&e);
        let brk = props_of(&e, 0).section_break.clone();
        e.caret = Caret::top(0, 2);
        e.insert_table(1, 1, crate::table::AutoFit::Default)
            .unwrap();
        assert!(matches!(e.doc.body[1], Block::Table(_)));
        assert_eq!(props_of(&e, 0).section_break, None);
        assert_eq!(props_of(&e, 0).section_property_change, None);
        assert_eq!(props_of(&e, 2).section_break, brk);
        assert_eq!(sect_change_raw(&e, 2), Some(sect_change().raw));
        assert_eq!(e.sections().len(), 3);
        assert_eq!(saved_sect_prs(&e), saved);
    }

    /// "one" still closes section 0 with its tracked change, and Save writes
    /// as many sectPr as `saved`.
    fn assert_section_0_kept(e: &Editor, brk: &Option<String>, saved: usize, what: &str) {
        assert_eq!(text_of(e, 0), "one", "{what}");
        assert_eq!(&props_of(e, 0).section_break, brk, "{what}");
        assert_eq!(sect_change_raw(e, 0), Some(sect_change().raw), "{what}");
        assert_eq!(e.doc.body.len(), three().doc.body.len(), "{what}");
        assert_eq!(e.sections().len(), 3, "{what}");
        assert_eq!(saved_sect_prs(e), saved, "{what}");
    }

    #[test]
    fn enter_then_backspace_keeps_the_section_748() {
        for off in [3, 0] {
            let mut e = three_with_a_sect_change();
            let saved = saved_sect_prs(&e);
            let brk = props_of(&e, 0).section_break.clone();
            e.caret = Caret::top(0, off);
            e.insert_newline();
            assert_eq!(e.caret, Caret::top(1, 0));
            e.backspace();
            assert_section_0_kept(&e, &brk, saved, &format!("offset {off}"));
        }
    }

    #[test]
    fn enter_then_delete_keeps_the_section_748() {
        let mut e = three_with_a_sect_change();
        let saved = saved_sect_prs(&e);
        let brk = props_of(&e, 0).section_break.clone();
        e.caret = Caret::top(0, 3);
        e.insert_newline();
        e.caret = Caret::top(0, 3);
        e.delete_forward();
        assert_section_0_kept(&e, &brk, saved, "delete");
    }

    #[test]
    fn deleting_across_a_split_section_paragraph_keeps_the_section_748() {
        let mut e = three_with_a_sect_change();
        let saved = saved_sect_prs(&e);
        let brk = props_of(&e, 0).section_break.clone();
        e.caret = Caret::top(0, 3);
        e.insert_str(" more");
        e.caret = Caret::top(0, 4);
        e.insert_newline();
        assert_eq!(
            (text_of(&e, 0), text_of(&e, 1)),
            ("one ".into(), "more".into())
        );
        let before = e.doc.clone();
        e.anchor = Some(Caret::top(0, 2));
        e.caret = Caret::top(1, 2);
        assert!(e.delete_selection());
        assert_eq!(text_of(&e, 0), "onre");
        assert_eq!(props_of(&e, 0).section_break, brk);
        assert_eq!(sect_change_raw(&e, 0), Some(sect_change().raw));
        assert_eq!(e.sections().len(), 3);
        assert_eq!(saved_sect_prs(&e), saved);
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    #[test]
    fn backspace_into_a_section_closing_paragraph_removes_its_break_645() {
        // "one" closes section 0: Backspace at the start of "two" deletes
        // "one"'s paragraph mark, so the break goes and "onetwo" is section 0
        // of what is now two sections.
        let mut e = three();
        let before = e.doc.clone();
        e.caret = Caret::top(1, 0);
        e.backspace();
        assert_eq!(text_of(&e, 0), "onetwo");
        assert_eq!(props_of(&e, 0).section_break, None);
        assert_eq!(e.sections().len(), 2);
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    /// "Wide" (landscape section) / the empty section-break paragraph /
    /// "Tall" in the portrait body section (#645).
    fn wide_tall() -> Editor {
        let mut doc = Document {
            body: vec![
                para("Wide", None),
                para(
                    "",
                    Some(&sect(
                        "<w:pgSz w:w=\"15840\" w:h=\"12240\" w:orient=\"landscape\"/>",
                    )),
                ),
                para("Tall", None),
            ],
        };
        doc.set_trailing_section_properties(SectionProperties {
            raw: sect("<w:pgSz w:w=\"12240\" w:h=\"15840\"/>"),
            property_change: None,
        });
        Editor::new(doc)
    }

    #[test]
    fn delete_on_the_section_break_paragraph_removes_the_break_645() {
        let mut e = wide_tall();
        let before = e.doc.clone();
        e.caret = Caret::top(1, 0);
        e.delete_forward();
        assert_eq!(text_of(&e, 1), "Tall");
        assert_eq!(props_of(&e, 1).section_break, None);
        assert_eq!(e.sections().len(), 1);
        assert_eq!(saved_sect_prs(&e), 1);
        assert!(!crate::serialize::document_to_xml(&e.doc).contains("landscape"));
        assert!(e.undo());
        assert_eq!(e.doc, before);
    }

    #[test]
    fn delete_at_the_end_of_a_text_paragraph_with_the_break_removes_it_645() {
        let mut e = wide_tall();
        e.doc.body[1] = para("end", props_of(&e, 1).section_break.as_deref());
        e.caret = Caret::top(1, 3);
        e.delete_forward();
        assert_eq!(text_of(&e, 1), "endTall");
        assert_eq!(e.sections().len(), 1);
    }

    #[test]
    fn delete_selection_across_the_break_removes_it_645() {
        let mut e = wide_tall();
        e.anchor = Some(Caret::top(1, 0));
        e.caret = Caret::top(2, 1);
        assert!(e.delete_selection());
        assert_eq!(text_of(&e, 1), "all");
        assert_eq!(e.sections().len(), 1);
    }

    #[test]
    fn deleting_a_break_mid_section_keeps_the_later_break_645() {
        // [A¶brk1][B¶][C¶brk2][trailing]: Delete at the end of A joins A and
        // B and drops brk1; brk2 still closes the (now longer) first section.
        let mut e = three();
        e.caret = Caret::top(0, 3);
        e.delete_forward();
        assert_eq!(text_of(&e, 0), "onetwo");
        assert_eq!(props_of(&e, 0).section_break, None);
        assert_eq!(e.sections().len(), 2);
        assert_eq!(start_of(&e.sections()[0]), SectionStart::OddPage);
    }

    #[test]
    fn merging_away_a_section_closing_paragraph_keeps_its_mark_748() {
        // "two b" closes section 1: pulling it up into "two" deletes "two"'s
        // paragraph mark, so "two b"'s section mark ends the merged paragraph.
        let mut e = three();
        let brk = props_of(&e, 2).section_break.clone();
        e.caret = Caret::top(1, 3);
        e.delete_forward();
        assert_eq!(text_of(&e, 1), "twotwo b");
        assert_eq!(props_of(&e, 1).section_break, brk);
        assert_eq!(e.sections().len(), 3);
        // Merging it into "one", which closes section 0, deletes that section's
        // mark instead: the merged paragraph ends section 1, as in Word.
        e.caret = Caret::top(1, 0);
        e.backspace();
        assert_eq!(text_of(&e, 0), "onetwotwo b");
        assert_eq!(props_of(&e, 0).section_break, brk);
        assert_eq!(e.sections().len(), 2);
        assert_eq!(start_of(&e.sections()[0]), SectionStart::OddPage);
    }

    // ---- #801: every paragraph split keeps a tracked pPrChange on both halves ----

    const PPR_CHANGE: &str =
        "<w:pPrChange w:id=\"9\"><w:pPr><w:jc w:val=\"left\"/></w:pPr></w:pPrChange>";

    /// "ab,cd", centred with a tracked change from left, then "z".
    fn ppr_change_editor() -> Editor {
        let xml = format!(
            concat!(
                "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
                "<w:body><w:p><w:pPr><w:jc w:val=\"center\"/>{}</w:pPr>",
                "<w:r><w:t>ab,cd</w:t></w:r></w:p>",
                "<w:p><w:r><w:t>z</w:t></w:r></w:p></w:body></w:document>"
            ),
            PPR_CHANGE
        );
        let doc = crate::load::parse_document_xml(&xml, &crate::load::Relationships::default());
        let e = Editor::new(doc);
        assert!(props_of(&e, 0).property_change.is_some());
        e
    }

    fn ppr_change_raw(e: &Editor, i: usize) -> Option<String> {
        props_of(e, i)
            .property_change
            .as_ref()
            .map(|c| c.raw.clone())
    }

    /// Paragraphs `at` all carry the original pPrChange raw, each with its
    /// own revision target.
    fn assert_ppr_change_on(e: &Editor, raw: &Option<String>, at: &[usize], what: &str) {
        let mut targets = Vec::new();
        for &i in at {
            assert_eq!(&ppr_change_raw(e, i), raw, "{what}: para {i}");
            assert_eq!(
                props_of(e, i).align,
                crate::model::Align::Center,
                "{what}: {i}"
            );
            let c = props_of(e, i).property_change.as_ref().unwrap();
            assert!(c.metadata.target.is_assigned(), "{what}: para {i}");
            targets.push(c.metadata.target);
        }
        targets.sort_by_key(|t| t.0);
        targets.dedup();
        assert_eq!(targets.len(), at.len(), "{what}: distinct targets");
    }

    #[test]
    fn enter_keeps_the_ppr_change_on_both_halves_801() {
        let mut e = ppr_change_editor();
        let raw = ppr_change_raw(&e, 0);
        e.caret = Caret::top(0, 2);
        e.insert_newline();
        assert_eq!(
            (text_of(&e, 0), text_of(&e, 1)),
            ("ab".into(), ",cd".into())
        );
        assert_ppr_change_on(&e, &raw, &[0, 1], "enter");
    }

    #[test]
    fn multi_paragraph_paste_keeps_the_ppr_change_on_every_paragraph_801() {
        let run = |t: &str| {
            vec![Inline::Run(Run {
                text: t.into(),
                ..Run::default()
            })]
        };
        for n in [2, 3] {
            let mut e = ppr_change_editor();
            let raw = ppr_change_raw(&e, 0);
            let clip = Clip {
                paras: ["A", "B", "C"][..n].iter().map(|t| run(t)).collect(),
            };
            e.caret = Caret::top(0, 2);
            e.paste(&clip);
            let at: Vec<usize> = (0..n).collect();
            assert_ppr_change_on(&e, &raw, &at, &format!("{n} paragraphs"));
        }
    }

    #[test]
    fn section_break_split_keeps_the_ppr_change_on_both_halves_801() {
        let mut e = ppr_change_editor();
        let raw = ppr_change_raw(&e, 0);
        e.caret = Caret::top(0, 2);
        e.insert_section_break(SectionStart::NextPage).unwrap();
        assert_eq!(
            (text_of(&e, 0), text_of(&e, 1)),
            ("ab".into(), ",cd".into())
        );
        assert!(props_of(&e, 0).section_break.is_some());
        assert_ppr_change_on(&e, &raw, &[0, 1], "section break");
    }

    #[test]
    fn insert_table_split_keeps_the_ppr_change_on_both_halves_801() {
        let mut e = ppr_change_editor();
        let raw = ppr_change_raw(&e, 0);
        e.caret = Caret::top(0, 2);
        e.insert_table(1, 1, crate::table::AutoFit::Default)
            .unwrap();
        assert!(matches!(e.doc.body[1], Block::Table(_)));
        assert_eq!(
            (text_of(&e, 0), text_of(&e, 2)),
            ("ab".into(), ",cd".into())
        );
        assert_ppr_change_on(&e, &raw, &[0, 2], "insert table");
    }

    #[test]
    fn text_to_table_keeps_the_ppr_change_on_every_piece_801() {
        let mut e = ppr_change_editor();
        let raw = ppr_change_raw(&e, 0);
        e.anchor = Some(Caret::top(0, 0));
        e.caret = Caret::top(0, 5);
        e.text_to_table(super::super::CellSep::Char(','), None)
            .unwrap();
        let Some(Block::Table(t)) = e.doc.body.first() else {
            panic!("no table")
        };
        let pieces: Vec<&Paragraph> = t.rows[0]
            .cells
            .iter()
            .map(|c| match c.blocks.first() {
                Some(Block::Paragraph(p)) => p,
                _ => panic!("cell without a paragraph"),
            })
            .collect();
        assert_eq!(pieces.len(), 2);
        let mut targets = Vec::new();
        for p in pieces {
            let c = p.props.property_change.as_ref().expect("pPrChange kept");
            assert_eq!(Some(c.raw.clone()), raw);
            targets.push(c.metadata.target);
        }
        assert_ne!(targets[0], targets[1]);
    }

    #[test]
    fn rejecting_the_split_off_ppr_change_reverts_only_its_paragraph_801() {
        let mut e = ppr_change_editor();
        e.caret = Caret::top(0, 2);
        e.insert_table(1, 1, crate::table::AutoFit::Default)
            .unwrap();
        let second = props_of(&e, 2).property_change.as_ref().unwrap();
        let target = second.metadata.target;
        assert!(e.reject_revision(target).is_applied());
        assert_eq!(props_of(&e, 2).property_change, None);
        assert_eq!(props_of(&e, 2).align, crate::model::Align::Left);
        assert!(props_of(&e, 0).property_change.is_some());
        assert_eq!(props_of(&e, 0).align, crate::model::Align::Center);
    }
}
