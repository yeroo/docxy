//! Section addressing for the page Layout commands (#649).
//!
//! A document's sections are its body paragraphs that carry a `section_break`
//! (each closes the section it ends), in body order, then the body-level
//! trailing `w:sectPr`, which describes the final section. Section `k` is the
//! k-th of these. A paragraph carrying a break belongs to the section that
//! break closes, and a table belongs to the section of the body block it sits
//! in. Every edit here is one undo step on the editor's document, which is
//! what the page view and Save read.

use super::{Caret, EditKind, Editor, split_content};
use crate::model::{Block, BreakKind, Inline, Paragraph, SectionProperties};
use crate::sect::{SectionSetup, SectionStart};

use super::Clip;

/// Where one section's properties live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectAt {
    /// The `section_break` of the body paragraph at this index.
    Para(usize),
    /// The body-level trailing section properties.
    Trailing,
}

/// A raw sectPr for a section that has none of its own yet.
const EMPTY_SECT: &str = "<w:sectPr></w:sectPr>";

impl Editor {
    fn section_slots(&self) -> Vec<SectAt> {
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

    fn sect_raw(&self, at: SectAt) -> &str {
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

    fn set_sect_raw(&mut self, at: SectAt, raw: String) {
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

    /// Insert a section break at the caret, as one undo step. With a selection,
    /// the break goes at its start and the selection collapses there.
    ///
    /// The caret's paragraph P, in section k, splits at the caret. The first
    /// half takes a copy of section k's sectPr with its original `w:type`, so it
    /// closes a section just like the old one. Section k's own sectPr now closes
    /// the section after the break, and `w:type` describes how *that* section
    /// starts, so it takes `start`. The second half keeps P's own break, if P
    /// had one.
    ///
    /// Refused (with the reason) when the caret is not in a body paragraph:
    /// a table cell, a text box, or another story cannot carry a sectPr.
    pub fn insert_section_break(&mut self, start: SectionStart) -> Result<(), String> {
        self.insert_section_break_with(start, |_| {})
    }

    /// [`Editor::insert_section_break`], then `edit` on the section after the
    /// break, all as one undo step: Page Setup's and Columns' "This point
    /// forward".
    pub fn insert_section_break_with(
        &mut self,
        start: SectionStart,
        edit: impl FnOnce(&mut SectionSetup),
    ) -> Result<(), String> {
        let (at, block, k) = self.break_point()?;
        let slot = self.section_slots()[k];
        let old = self.sect_raw(slot).to_string();
        let mut setup = SectionSetup::parse(&old);
        setup.start = start;
        edit(&mut setup);
        let closing = setup.apply(&old);

        self.anchor = None;
        self.caret = at;
        self.checkpoint(EditKind::Structural);
        let Some(Block::Paragraph(p)) = self.doc.body.get_mut(block) else {
            return Err("A section break needs the caret in a paragraph".into());
        };
        let right = split_content(&mut p.content, self.caret.offset);
        let second = Paragraph {
            props: p.props.clone(),
            content: right,
        };
        p.props.section_break = Some(old);
        p.props.section_property_change = None;
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
        self.doc.initialize_revision_targets();
        Ok(())
    }

    /// Where a section break goes: the selection's start, else the caret; its
    /// body block; and the section it lands in, the one whose setup the
    /// section after the break takes. Refused outside a body paragraph.
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
    pub fn insert_break(&mut self, kind: BreakKind) {
        self.paste(&Clip {
            paras: vec![vec![Inline::Break(kind)]],
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
    fn a_selection_collapses_to_its_start() {
        let mut e = three();
        e.anchor = Some(Caret::top(3, 4));
        e.caret = Caret::top(1, 1);
        e.insert_section_break(SectionStart::Continuous).unwrap();
        assert!(e.anchor.is_none());
        assert_eq!(e.doc.body[1].plain_text(), "t");
        assert_eq!(e.doc.body.last().map(|_| e.sections().len()), Some(4));
        assert!(e.doc.plain_text().contains("three"), "nothing was deleted");
    }

    #[test]
    fn breaks_go_in_at_the_caret() {
        let mut e = three();
        e.caret = Caret::top(1, 1);
        e.insert_break(BreakKind::Clear(crate::model::ClearKind::All));
        let Block::Paragraph(p) = &e.doc.body[1] else {
            panic!()
        };
        assert!(p.content.contains(&Inline::Break(BreakKind::Clear(
            crate::model::ClearKind::All
        ))));
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
    fn backspace_into_a_plain_paragraph_is_unchanged_748() {
        let mut e = three();
        let brk = props_of(&e, 0).section_break.clone();
        e.caret = Caret::top(1, 0);
        e.backspace();
        assert_eq!(text_of(&e, 0), "onetwo");
        assert_eq!(props_of(&e, 0).section_break, brk);
        assert_eq!(e.sections().len(), 3);
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
