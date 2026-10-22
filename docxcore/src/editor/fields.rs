//! Update Field (F9) and Edit Field (#1241).
//!
//! A field showing a result is one editor offset (#642). Updating it
//! recomputes the result from its instruction ([`crate::field::eval_field_ctx`])
//! and writes it into the field's XML ([`crate::field::with_result`]), so Save
//! writes the new value. Only fields that stay one offset change: a result is
//! never emptied, and a field showing none is not given one, so the caret and
//! the selection keep their offsets. A field whose value cannot be computed
//! (PAGE, REF, TOC, …) or whose result holds more than text keeps its cache,
//! as Word keeps it when it cannot resolve.

use super::{EditKind, Editor, inline_len, is_field_unit, link_runs_len, para_mut, resolve_para};
use crate::field::{
    FieldContext, eval_field_ctx, instr_of, stored_result, with_instr, with_result,
};
use crate::model::Inline;

/// The editor offsets of the field units in `content` within `[start, end)`,
/// looking inside hyperlinks' `content`.
fn unit_offsets(
    content: &[Inline],
    start: usize,
    end: usize,
    pos: &mut usize,
    out: &mut Vec<usize>,
) {
    for inline in content {
        match inline {
            Inline::Hyperlink(h) => {
                *pos += link_runs_len(h);
                unit_offsets(&h.content, start, end, pos, out);
            }
            _ => {
                if is_field_unit(inline) && (start..end).contains(pos) {
                    out.push(*pos);
                }
                *pos += inline_len(inline);
            }
        }
    }
}

/// The inline holding editor offset `idx`, mutably, looking inside a
/// hyperlink's `content` (as `inline_covering`). It marks each raw-backed
/// hyperlink it enters `content_changed`, so call it only to rewrite what it
/// returns (#1637).
fn inline_covering_mut(content: &mut [Inline], idx: usize) -> Option<&mut Inline> {
    let mut acc = 0;
    for inline in content {
        let l = inline_len(inline);
        if idx < acc + l {
            let local = idx - acc;
            let runs = match &*inline {
                Inline::Hyperlink(h) if local >= link_runs_len(h) => link_runs_len(h),
                _ => return Some(inline),
            };
            let Inline::Hyperlink(h) = inline else {
                return None;
            };
            // A raw-backed link is saved as loaded unless marked (#1637).
            h.content_changed |= h.raw.is_some();
            return inline_covering_mut(&mut h.content, local - runs);
        }
        acc += l;
    }
    None
}

/// A field's raw XML and text after an update against `ctx`, or `None` when
/// it cannot be computed, would be empty, cannot be written into its XML, or
/// is the same value as now: shown, and stored in its XML. The XML's bytes
/// are not compared (a rewrite drops run attributes and `xml:space`), but
/// its stored text is, since the loader shows a formula field's value
/// computed afresh over a stale cache.
fn updated(raw: &str, text: &str, ctx: &FieldContext) -> Option<(String, String)> {
    let value = eval_field_ctx(&instr_of(raw)?, ctx).filter(|v| !v.is_empty())?;
    if value == text && stored_result(raw).as_deref() == Some(value.as_str()) {
        return None;
    }
    Some((with_result(raw, &value)?, value))
}

impl Editor {
    /// The field units (paragraph path, offset) F9 and Edit Field act on:
    /// those in the selection (body and table-cell paragraphs, a cell range
    /// cell by cell, inside hyperlinks), or with none the field just right of
    /// the caret, else the one just left of it.
    fn target_fields(&self) -> Vec<(Vec<usize>, usize)> {
        if self.has_selection() || self.cell_range().is_some() {
            let mut out = Vec::new();
            for (path, start, end) in self.selection_spans() {
                let Some(p) = resolve_para(&self.doc.body, &path) else {
                    continue;
                };
                let mut offsets = Vec::new();
                unit_offsets(&p.content, start, end, &mut 0, &mut offsets);
                out.extend(offsets.into_iter().map(|o| (path.clone(), o)));
            }
            return out;
        }
        let Some(p) = resolve_para(&self.doc.body, &self.caret.path) else {
            return Vec::new();
        };
        let at = self.caret.offset;
        let unit = |idx: usize| {
            let mut found = Vec::new();
            unit_offsets(&p.content, idx, idx + 1, &mut 0, &mut found);
            found.first().copied()
        };
        unit(at)
            .or_else(|| at.checked_sub(1).and_then(unit))
            .map(|o| (self.caret.path.clone(), o))
            .into_iter()
            .collect()
    }

    /// Update the fields in the selection, or the field at the caret, to a
    /// freshly computed value (Word's F9), as one undo step named "Update
    /// Field"; none when nothing changed. How many fields changed. Not
    /// recorded by Track Changes.
    pub fn update_fields(&mut self, ctx: &FieldContext) -> usize {
        let changes: Vec<(Vec<usize>, usize, String, String)> = self
            .target_fields()
            .into_iter()
            .filter_map(|(path, off)| {
                let p = resolve_para(&self.doc.body, &path)?;
                match super::inline_covering(&p.content, off)? {
                    Inline::Field { raw, text } => {
                        updated(raw, text, ctx).map(|(r, t)| (path, off, r, t))
                    }
                    _ => None,
                }
            })
            .collect();
        let count = changes.len();
        if count == 0 {
            return 0;
        }
        self.one_step("Update Field", |ed| {
            ed.checkpoint(EditKind::Structural);
            for (path, off, new_raw, new_text) in changes {
                if let Some(Inline::Field { raw, text }) = para_mut(&mut ed.doc.body, &path)
                    .and_then(|p| inline_covering_mut(&mut p.content, off))
                {
                    *raw = new_raw;
                    *text = new_text;
                }
            }
        });
        count
    }

    /// The one field Edit Field acts on: the only field unit in the
    /// selection, or with none the field just right of the caret, else just
    /// left of it.
    fn edit_target(&self) -> Option<(Vec<usize>, usize)> {
        let mut targets = self.target_fields();
        (targets.len() == 1).then(|| targets.remove(0))
    }

    /// The instruction of the field Edit Field acts on (see
    /// [`Editor::set_field_instr`]), or `None` when there is no such field
    /// (a symbol, no field, or several selected).
    pub fn field_at_caret(&self) -> Option<String> {
        let (path, off) = self.edit_target()?;
        let p = resolve_para(&self.doc.body, &path)?;
        match super::inline_covering(&p.content, off)? {
            Inline::Field { raw, .. } => instr_of(raw),
            _ => None,
        }
    }

    /// Give the field [`Editor::field_at_caret`] reads the instruction
    /// `instr`, then update its result against `ctx` (its cached result
    /// stays when the new one cannot be computed), as one undo step named
    /// "Edit Field". False, and no step, when there is no such field, the
    /// instruction is unchanged or cannot be rewritten (a field nested in it).
    pub fn set_field_instr(&mut self, instr: &str, ctx: &FieldContext) -> bool {
        let Some((path, off)) = self.edit_target() else {
            return false;
        };
        let Some(Inline::Field { raw, text }) = resolve_para(&self.doc.body, &path)
            .and_then(|p| super::inline_covering(&p.content, off))
        else {
            return false;
        };
        if instr_of(raw).as_deref() == Some(instr) {
            return false;
        }
        let Some(new_raw) = with_instr(raw, instr) else {
            return false;
        };
        let (new_raw, new_text) =
            updated(&new_raw, text, ctx).unwrap_or_else(|| (new_raw, text.clone()));
        self.one_step("Edit Field", |ed| {
            ed.checkpoint(EditKind::Structural);
            if let Some(Inline::Field { raw, text }) = para_mut(&mut ed.doc.body, &path)
                .and_then(|p| inline_covering_mut(&mut p.content, off))
            {
                *raw = new_raw;
                *text = new_text;
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;
    use crate::field::DateTime;
    use crate::model::Document;

    fn doc(inner: &str) -> Document {
        crate::load::parse_document_xml(
            &format!("<w:document><w:body>{inner}</w:body></w:document>"),
            &crate::load::Relationships::default(),
        )
    }

    fn ctx() -> FieldContext {
        FieldContext {
            now: Some(DateTime {
                year: 2026,
                month: 10,
                day: 14,
                hour: 9,
                min: 5,
                sec: 0,
                weekday: 3,
            }),
            ..Default::default()
        }
    }

    const DATE: &str = "<w:fldSimple w:instr=\" DATE \\@ &quot;M/d/yyyy&quot; \"><w:r><w:t>1/2/2000</w:t></w:r></w:fldSimple>";
    const PAGE: &str = "<w:fldSimple w:instr=\" PAGE \"><w:r><w:t>7</w:t></w:r></w:fldSimple>";

    fn text_run(t: &str) -> String {
        format!("<w:r><w:t xml:space=\"preserve\">{t}</w:t></w:r>")
    }

    fn first_para(ed: &Editor) -> &crate::model::Paragraph {
        resolve_para(&ed.doc.body, &[0]).unwrap()
    }

    /// The field texts of the first paragraph, in order.
    fn field_texts(ed: &Editor) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(content: &[Inline], out: &mut Vec<String>) {
            for i in content {
                match i {
                    Inline::Field { text, .. } => out.push(text.clone()),
                    Inline::Hyperlink(h) => walk(&h.content, out),
                    _ => {}
                }
            }
        }
        walk(&resolve_para(&ed.doc.body, &[0]).unwrap().content, &mut out);
        out
    }

    fn at(ed: &mut Editor, path: &[usize], offset: usize) {
        ed.caret = Caret {
            path: path.to_vec(),
            offset,
        };
        ed.anchor = None;
    }

    fn select(ed: &mut Editor, path: &[usize], a: usize, b: usize) {
        ed.anchor = Some(Caret {
            path: path.to_vec(),
            offset: a,
        });
        ed.caret = Caret {
            path: path.to_vec(),
            offset: b,
        };
    }

    #[test]
    fn f9_updates_a_date_field_and_save_writes_the_new_value_1241() {
        let mut ed = Editor::new(doc(&format!("<w:p>{}{DATE}</w:p>", text_run("On "))));
        at(&mut ed, &[0], 3);
        assert_eq!(ed.update_fields(&ctx()), 1);
        assert_eq!(field_texts(&ed), ["10/14/2026"]);
        assert_eq!(
            ed.undo_names().first().map(String::as_str),
            Some("Update Field")
        );
        assert_eq!(ed.caret.offset, 3, "the caret keeps its offset");
        // Saved and loaded again, the field shows the new value from its XML.
        let xml = crate::serialize::document_to_xml(&ed.doc);
        assert!(xml.contains("10/14/2026") && !xml.contains("1/2/2000"));
        let again = Editor::new(crate::load::parse_document_xml(
            &xml,
            &crate::load::Relationships::default(),
        ));
        assert_eq!(field_texts(&again), ["10/14/2026"]);
    }

    #[test]
    fn undo_brings_back_the_old_result_1241() {
        let mut ed = Editor::new(doc(&format!("<w:p>{DATE}</w:p>")));
        at(&mut ed, &[0], 0);
        assert_eq!(ed.update_fields(&ctx()), 1);
        assert!(ed.undo());
        assert_eq!(field_texts(&ed), ["1/2/2000"]);
        assert!(crate::serialize::document_to_xml(&ed.doc).contains("1/2/2000"));
    }

    #[test]
    fn a_stale_formula_cache_is_written_but_a_current_one_takes_no_step_1241() {
        // The loader shows `= 2+2` computed already; only the XML is stale.
        let stale = "<w:fldSimple w:instr=\" = 2+2 \"><w:r><w:t>5</w:t></w:r></w:fldSimple>";
        let mut ed = Editor::new(doc(&format!("<w:p>{stale}</w:p>")));
        assert_eq!(field_texts(&ed), ["4"]);
        at(&mut ed, &[0], 1);
        assert_eq!(ed.update_fields(&ctx()), 1, "the field left of the caret");
        assert!(crate::serialize::document_to_xml(&ed.doc).contains("<w:t>4</w:t>"));
        let steps = ed.undo_names().len();
        assert_eq!(ed.update_fields(&ctx()), 0, "already current");
        assert_eq!(ed.undo_names().len(), steps, "no empty step");
    }

    #[test]
    fn f9_on_a_current_value_takes_no_step_whatever_its_xml_looks_like_1241() {
        // Insert Field's own form: `<w:t xml:space="preserve">`.
        let inserted = crate::field::fld_simple("= 2+2", "4", &Default::default());
        let Inline::Field { raw, .. } = &inserted else {
            unreachable!()
        };
        let word = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>             <w:r><w:instrText xml:space=\"preserve\"> = 2+2 </w:instrText></w:r>             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>             <w:r w:rsidR=\"00A1B2C3\"><w:t>4</w:t></w:r>             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        for raw in [raw.as_str(), word] {
            let mut ed = Editor::new(doc(&format!("<w:p>{raw}</w:p>")));
            assert_eq!(field_texts(&ed), ["4"]);
            at(&mut ed, &[0], 0);
            assert_eq!(ed.update_fields(&ctx()), 0, "{raw}");
            assert!(ed.undo_names().is_empty());
            let Inline::Field { raw: kept, .. } = &first_para(&ed).content[0] else {
                panic!("a field");
            };
            assert_eq!(kept, raw, "its XML is untouched");
        }
    }

    #[test]
    fn a_field_that_cannot_be_computed_keeps_its_cache_and_takes_no_step_1241() {
        let mut ed = Editor::new(doc(&format!("<w:p>{PAGE}</w:p>")));
        at(&mut ed, &[0], 0);
        assert_eq!(ed.update_fields(&ctx()), 0);
        assert_eq!(field_texts(&ed), ["7"]);
        assert!(ed.undo_names().is_empty());
    }

    #[test]
    fn the_caret_takes_the_field_right_of_it_then_the_one_left_of_it_1241() {
        let formula = "<w:fldSimple w:instr=\" = 1+1 \"><w:r><w:t>9</w:t></w:r></w:fldSimple>";
        // `DATE` at 0, the formula at 1: the caret at 1 takes the formula.
        let mut ed = Editor::new(doc(&format!("<w:p>{DATE}{formula}</w:p>")));
        at(&mut ed, &[0], 1);
        assert_eq!(ed.update_fields(&ctx()), 1);
        assert_eq!(field_texts(&ed), ["1/2/2000", "2"]);
        // At the end of the paragraph, the formula left of it (already done).
        at(&mut ed, &[0], 2);
        assert_eq!(ed.update_fields(&ctx()), 0);
        // Away from any field, nothing.
        let mut ed = Editor::new(doc(&format!("<w:p>{DATE}{}</w:p>", text_run("abc"))));
        at(&mut ed, &[0], 3);
        assert_eq!(ed.update_fields(&ctx()), 0);
    }

    #[test]
    fn a_selection_updates_every_field_in_it_as_one_step_1241() {
        let formula = "<w:fldSimple w:instr=\" = 1+1 \"><w:r><w:t>9</w:t></w:r></w:fldSimple>";
        let mut ed = Editor::new(doc(&format!(
            "<w:p>{DATE}{}{formula}{}{DATE}</w:p>",
            text_run(" and "),
            text_run(" or ")
        )));
        // Offsets: DATE 0, " and " 1..6, formula 6, " or " 7..11, DATE 11.
        select(&mut ed, &[0], 0, 7);
        assert_eq!(ed.update_fields(&ctx()), 2);
        assert_eq!(field_texts(&ed), ["10/14/2026", "2", "1/2/2000"]);
        assert_eq!(ed.undo_names(), ["Update Field"]);
        assert!(ed.undo());
        assert_eq!(field_texts(&ed)[0], "1/2/2000");
    }

    #[test]
    fn fields_in_a_link_and_in_a_table_cell_update_1241() {
        let mut ed = Editor::new(doc(&format!(
            "<w:p><w:hyperlink w:anchor=\"x\">{}{DATE}</w:hyperlink></w:p>\
             <w:tbl><w:tr><w:tc><w:p>{DATE}</w:p></w:tc></w:tr></w:tbl>",
            text_run("Go ")
        )));
        at(&mut ed, &[0], 3);
        assert_eq!(ed.update_fields(&ctx()), 1);
        assert_eq!(field_texts(&ed), ["10/14/2026"]);
        let cell = super::super::all_paragraph_paths(&ed.doc.body)
            .into_iter()
            .find(|p| p.len() > 1)
            .expect("a cell paragraph");
        at(&mut ed, &cell, 0);
        assert_eq!(ed.update_fields(&ctx()), 1);
        let p = resolve_para(&ed.doc.body, &cell).unwrap();
        assert!(matches!(&p.content[0], Inline::Field { text, .. } if text == "10/14/2026"));
    }

    /// A loaded link with a field in it is saved as its raw XML unless its
    /// content changed, so a field rewrite has to mark it (#1637).
    fn link_with_field_and_sibling() -> Editor {
        Editor::new(doc(&format!(
            "<w:p><w:hyperlink w:anchor=\"x\">{}{DATE}</w:hyperlink>\
             <w:hyperlink w:anchor=\"y\">{}</w:hyperlink></w:p>",
            text_run("Go "),
            text_run("Stay")
        )))
    }

    fn untouched_sibling_link(xml: &str) -> bool {
        xml.contains("w:anchor=\"y\"") && xml.contains("Stay")
    }

    #[test]
    fn f9_in_a_raw_hyperlink_survives_save_1637() {
        let mut ed = link_with_field_and_sibling();
        let before = crate::serialize::document_to_xml(&ed.doc);
        at(&mut ed, &[0], 3);
        assert_eq!(ed.update_fields(&ctx()), 1);
        let xml = crate::serialize::document_to_xml(&ed.doc);
        assert!(xml.contains("10/14/2026"), "{xml}");
        assert!(!xml.contains("1/2/2000"), "{xml}");
        assert!(untouched_sibling_link(&xml));
        // The link not on the field's path is still written as loaded.
        let sibling = |x: &str| x[x.find("w:anchor=\"y\"").unwrap()..].to_string();
        assert_eq!(sibling(&xml), sibling(&before));
        let Inline::Hyperlink(other) = &first_para(&ed).content[1] else {
            panic!("the second link");
        };
        assert!(!other.content_changed);
    }

    #[test]
    fn edit_field_in_a_raw_hyperlink_survives_save_1637() {
        let mut ed = link_with_field_and_sibling();
        at(&mut ed, &[0], 3);
        assert!(ed.set_field_instr("= 6*7", &ctx()));
        let xml = crate::serialize::document_to_xml(&ed.doc);
        assert!(
            xml.contains("= 6*7") && xml.contains("<w:t>42</w:t>"),
            "{xml}"
        );
        assert!(!xml.contains("1/2/2000"), "{xml}");
        assert!(untouched_sibling_link(&xml));
    }

    #[test]
    fn a_no_op_update_leaves_a_raw_hyperlink_byte_faithful_1637() {
        let mut ed = Editor::new(doc(&format!(
            "<w:p><w:hyperlink w:anchor=\"x\">{PAGE}</w:hyperlink></w:p>"
        )));
        let before = crate::serialize::document_to_xml(&ed.doc);
        at(&mut ed, &[0], 0);
        assert_eq!(ed.update_fields(&ctx()), 0);
        assert_eq!(crate::serialize::document_to_xml(&ed.doc), before);
    }

    #[test]
    fn edit_field_reads_and_rewrites_the_instruction_1241() {
        let mut ed = Editor::new(doc(&format!("<w:p>{PAGE}</w:p>")));
        at(&mut ed, &[0], 0);
        assert_eq!(ed.field_at_caret().as_deref(), Some(" PAGE "));
        // The same instruction: nothing to do.
        assert!(!ed.set_field_instr(" PAGE ", &ctx()));
        assert!(ed.undo_names().is_empty());
        // A computable one: the instruction and the result change together.
        assert!(ed.set_field_instr("= 6*7", &ctx()));
        assert_eq!(field_texts(&ed), ["42"]);
        assert_eq!(ed.field_at_caret().as_deref(), Some("= 6*7"));
        assert_eq!(ed.undo_names(), ["Edit Field"]);
        // One that cannot be computed keeps the cached result.
        assert!(ed.set_field_instr("NUMPAGES", &ctx()));
        assert_eq!(field_texts(&ed), ["42"]);
        assert!(crate::serialize::document_to_xml(&ed.doc).contains("w:instr=\"NUMPAGES\""));
        assert!(ed.undo());
        assert_eq!(ed.field_at_caret().as_deref(), Some("= 6*7"));
    }

    #[test]
    fn edit_field_needs_exactly_one_field_and_never_takes_a_symbol_1241() {
        let sym = "<w:r><w:sym w:font=\"Symbol\" w:char=\"F0B7\"/></w:r>";
        let mut ed = Editor::new(doc(&format!("<w:p>{sym}</w:p>")));
        at(&mut ed, &[0], 0);
        assert_eq!(ed.field_at_caret(), None);
        assert!(!ed.set_field_instr("PAGE", &ctx()));
        let mut ed = Editor::new(doc(&format!("<w:p>{PAGE}{PAGE}</w:p>")));
        select(&mut ed, &[0], 0, 2);
        assert_eq!(ed.field_at_caret(), None, "two fields selected");
        select(&mut ed, &[0], 1, 2);
        assert_eq!(ed.field_at_caret().as_deref(), Some(" PAGE "));
    }
}
