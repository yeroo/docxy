//! Preview Results: merge fields show a recipient's values in place.
//!
//! Display only. A preview rewrites the `text` of merge fields and never
//! their `raw`, so the saved document is the same with or without it. A
//! field keeps a length of one in preview (an empty value shows U+200B,
//! drawn as nothing), so no editor offset moves when the record changes.

use super::csv::Recipients;
use super::fields::{FieldMap, MergeContext, MergeFieldKind, eval, field_kind};
use crate::model::{Block, Document, Inline};

/// What an empty value previews as: a zero-width space, so the field still
/// takes its one offset.
pub const EMPTY_PREVIEW: &str = "\u{200B}";

/// The record Preview Results shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePreview {
    pub recipients: Recipients,
    pub map: FieldMap,
    /// The data-source row shown first; each `NEXT` field moves on to the
    /// next included row.
    pub row: usize,
}

/// Call `f(raw, text)` for every field in `blocks`, in document order,
/// including fields in tables, text boxes, hyperlinks and tracked changes.
pub(crate) fn visit_fields(blocks: &mut [Block], f: &mut dyn FnMut(&str, &mut String)) {
    fn inlines(items: &mut [Inline], f: &mut dyn FnMut(&str, &mut String)) {
        for inl in items {
            match inl {
                Inline::Field { raw, text } => f(raw, text),
                Inline::TextBox { blocks, .. } => visit_fields(blocks, f),
                Inline::Hyperlink(h) => inlines(&mut h.content, f),
                Inline::Revision { content, .. } => inlines(content, f),
                _ => {}
            }
        }
    }
    for b in blocks {
        match b {
            Block::Paragraph(p) => inlines(&mut p.content, f),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        visit_fields(&mut cell.blocks, f);
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
}

/// The result a field's `raw` caches (what Word last showed).
pub(crate) fn cached_text(raw: &str) -> String {
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body><w:p>{raw}</w:p></w:body></w:document>"
    );
    let doc = crate::load::parse_document_xml(&xml, &Default::default());
    match doc.body.first() {
        Some(Block::Paragraph(p)) => p.content.iter().map(Inline::text).collect(),
        _ => String::new(),
    }
}

/// Show `preview`'s record in every merge field of `doc`, or, with `None`,
/// each field's cached result again (its `«Name»` placeholder when the cache
/// is empty, so the field keeps its offset).
pub fn apply_preview(doc: &mut Document, preview: Option<&MergePreview>) {
    let Some(p) = preview else {
        visit_fields(&mut doc.body, &mut |raw, text| {
            if let Some(kind) = field_kind(raw) {
                let cached = cached_text(raw);
                *text = if cached.is_empty() {
                    kind.placeholder()
                } else {
                    cached
                };
            }
        });
        return;
    };
    let rows = p.recipients.included_rows();
    let mut pos = rows.iter().position(|&r| r == p.row);
    let mut row = Some(p.row);
    visit_fields(&mut doc.body, &mut |raw, text| {
        let Some(kind) = field_kind(raw) else {
            return;
        };
        if kind == MergeFieldKind::Next {
            // The rest of the document shows the next included record.
            let next = match pos {
                Some(i) => i + 1,
                None => rows
                    .iter()
                    .position(|&r| row.is_some_and(|c| r > c))
                    .unwrap_or(rows.len()),
            };
            pos = Some(next);
            row = rows.get(next).copied();
            *text = EMPTY_PREVIEW.to_string();
            return;
        }
        let Some(current) = row else {
            *text = EMPTY_PREVIEW.to_string();
            return;
        };
        let ctx = MergeContext {
            recipients: &p.recipients,
            map: &p.map,
            row: current,
            seq: pos.map_or(1, |i| i + 1),
        };
        *text = match eval(&kind, &ctx) {
            None => kind.placeholder(),
            Some(v) if v.is_empty() => EMPTY_PREVIEW.to_string(),
            // One field is one offset: an address block's lines run on.
            Some(v) => v.replace('\n', ", "),
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, para_text_len};
    use crate::merge::fields::merge_field;
    use crate::model::{Paragraph, Run, RunProps};

    fn people() -> Recipients {
        Recipients::parse_csv(b"First,Last,Company\nJane,Doe,Acme\nJohn,Smith,\nAmy,Lee,Zed\n")
            .unwrap()
    }

    fn preview(row: usize) -> MergePreview {
        let recipients = people();
        let map = FieldMap::auto(&recipients);
        MergePreview {
            recipients,
            map,
            row,
        }
    }

    fn run(t: &str) -> Inline {
        Inline::Run(Run {
            text: t.into(),
            props: RunProps::default(),
        })
    }

    /// "Hi «First» «Company»!" in one paragraph.
    fn letter() -> Document {
        let p = RunProps::default();
        Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![
                    run("Hi "),
                    merge_field("First", &p),
                    run(" "),
                    merge_field("Company", &p),
                    run("!"),
                ],
                ..Default::default()
            })],
        }
    }

    fn shown(doc: &Document) -> String {
        doc.plain_text().replace(EMPTY_PREVIEW, "")
    }

    #[test]
    fn preview_shows_the_record_and_none_restores_the_placeholders() {
        let mut doc = letter();
        apply_preview(&mut doc, Some(&preview(0)));
        assert_eq!(shown(&doc), "Hi Jane Acme!\n");
        apply_preview(&mut doc, Some(&preview(2)));
        assert_eq!(shown(&doc), "Hi Amy Zed!\n");
        apply_preview(&mut doc, None);
        assert_eq!(shown(&doc), "Hi \u{AB}First\u{BB} \u{AB}Company\u{BB}!\n");
    }

    #[test]
    fn an_empty_value_keeps_the_field_one_offset_long() {
        let mut doc = letter();
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        let before = para_text_len(p);
        apply_preview(&mut doc, Some(&preview(1))); // John has no company
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        assert_eq!(para_text_len(p), before);
        assert_eq!(shown(&doc), "Hi John !\n");
    }

    #[test]
    fn a_previewed_document_saves_the_same_bytes() {
        let plain = crate::package::save_package(&crate::package::new_package(letter()));
        let mut doc = letter();
        apply_preview(&mut doc, Some(&preview(0)));
        let previewed = crate::package::save_package(&crate::package::new_package(doc));
        assert_eq!(plain, previewed);
    }

    #[test]
    fn next_moves_to_the_next_included_record() {
        let p = RunProps::default();
        let mut doc = Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![
                    merge_field("First", &p),
                    crate::merge::rule_field(&MergeFieldKind::Next, &p).unwrap(),
                    merge_field("First", &p),
                    crate::merge::rule_field(&MergeFieldKind::MergeSeq, &p).unwrap(),
                ],
                ..Default::default()
            })],
        };
        let mut pv = preview(0);
        pv.recipients.included[1] = false; // skip John
        apply_preview(&mut doc, Some(&pv));
        assert_eq!(shown(&doc), "JaneAmy2\n");
    }

    #[test]
    fn editor_reapplies_the_preview_after_undo_and_redo() {
        let mut ed = Editor::new(letter());
        ed.set_merge_preview(Some(preview(0)));
        ed.caret.offset = 0;
        ed.insert_char('X'); // snapshot taken while showing Jane
        ed.set_merge_preview(Some(preview(1)));
        assert!(ed.undo());
        // The snapshot showed Jane; the preview is on record 2 now.
        assert_eq!(shown(&ed.doc), "Hi John !\n");
        assert!(ed.redo());
        assert_eq!(shown(&ed.doc), "XHi John !\n");
        ed.set_merge_preview(None);
        assert!(ed.undo());
        assert_eq!(
            shown(&ed.doc),
            "Hi \u{AB}First\u{BB} \u{AB}Company\u{BB}!\n"
        );
    }

    #[test]
    fn export_doc_has_no_preview() {
        let mut ed = Editor::new(letter());
        ed.set_merge_preview(Some(preview(0)));
        assert_eq!(shown(&ed.doc), "Hi Jane Acme!\n");
        let md = crate::markdown::to_markdown(&ed.export_doc());
        assert!(md.contains("\u{AB}First\u{BB}"), "{md}");
        assert!(!md.contains("Jane"), "{md}");
    }
}
