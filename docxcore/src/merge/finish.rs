//! Finish & Merge ▸ Edit Individual Documents: the main document merged with
//! its recipients into a new package.

use super::csv::Recipients;
use super::fields::{FieldMap, MergeContext, MergeFieldKind, eval, field_kind};
use crate::model::{Block, BreakKind, Inline, Run};
use crate::package::{MainDocType, Package};
use crate::sect::{SectionSetup, SectionStart};

/// Which records Merge to New Document takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MergeRange {
    /// Every included record.
    #[default]
    All,
    /// One data-source row (the previewed one), included or not.
    Current(usize),
    /// Data-source rows `from..=to` (0-based), the included ones.
    FromTo(usize, usize),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOptions {
    pub range: MergeRange,
    pub doc_type: MainDocType,
    pub map: FieldMap,
}

/// The rows a merge takes, in order.
pub fn merge_rows(recipients: &Recipients, range: MergeRange) -> Vec<usize> {
    match range {
        MergeRange::All => recipients.included_rows(),
        MergeRange::Current(row) => (row < recipients.rows.len())
            .then_some(row)
            .into_iter()
            .collect(),
        MergeRange::FromTo(from, to) => recipients
            .included_rows()
            .into_iter()
            .filter(|r| (from..=to).contains(r))
            .collect(),
    }
}

/// Merge `main` with `recipients`: a clone of the package (styles,
/// numbering, headers and footers, media, relationships) whose body is one
/// copy of the main document per record, every merge field replaced by its
/// value as plain text in the field result's formatting. A `NEXT` field
/// moves the rest of its copy to the next record, so a copy can take several
/// (a sheet of labels). Letters, e-mail, envelopes and labels put each copy
/// in its own next-page section; a directory runs them on.
///
/// Bookmarks, comment anchors and footnote/endnote references are kept in
/// the first copy only, so no `w:id` repeats. Merge fields in headers and
/// footers (shared parts) stay fields. The result has no `w:mailMerge`.
pub fn merge_package(
    main: &Package,
    recipients: &Recipients,
    opts: &MergeOptions,
) -> Result<Package, String> {
    let rows = merge_rows(recipients, opts.range);
    if rows.is_empty() {
        return Err("there are no records to merge".into());
    }
    let body = &main.document.body;
    let template = &body[..main.document.content_block_count()];
    let trailing = body[template.len()..].to_vec();
    let sectioned = opts.doc_type != MainDocType::Directory;
    let break_sect = {
        let sect = main.sect_pr();
        let mut setup = SectionSetup::parse(sect);
        setup.start = SectionStart::NextPage;
        setup.apply(sect)
    };

    let mut merged: Vec<Block> = Vec::new();
    let mut at = 0;
    let mut copy = 0;
    while at < rows.len() {
        let mut blocks = template.to_vec();
        let mut st = Fill {
            recipients,
            map: &opts.map,
            rows: &rows,
            at,
        };
        st.blocks(&mut blocks);
        if copy > 0 {
            strip_ids(&mut blocks);
        }
        if sectioned && copy > 0 {
            end_section(&mut merged, &break_sect);
        }
        merged.extend(blocks);
        at = st.at + 1;
        copy += 1;
    }
    merged.extend(trailing);

    let mut out = main.clone();
    out.document.body = merged;
    if sectioned {
        out.set_sect_pr(break_sect);
    }
    out.set_mail_merge(None);
    Ok(out)
}

/// End the section at the last paragraph of `blocks` with `sect`, or with a
/// new empty paragraph when the copy ends in a table or already ends a
/// section there.
fn end_section(blocks: &mut Vec<Block>, sect: &str) {
    if let Some(Block::Paragraph(p)) = blocks.last_mut()
        && p.props.section_break.is_none()
    {
        p.props.section_break = Some(sect.to_string());
        return;
    }
    let mut p = crate::model::Paragraph::default();
    p.props.section_break = Some(sect.to_string());
    blocks.push(Block::Paragraph(p));
}

/// Walks one copy, replacing merge fields.
struct Fill<'a> {
    recipients: &'a Recipients,
    map: &'a FieldMap,
    rows: &'a [usize],
    /// Index into `rows` of the record this part of the copy shows.
    at: usize,
}

impl Fill<'_> {
    fn blocks(&mut self, blocks: &mut [Block]) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => self.inlines(&mut p.content),
                Block::Table(t) => {
                    for row in &mut t.rows {
                        for cell in &mut row.cells {
                            self.blocks(&mut cell.blocks);
                        }
                    }
                }
                Block::SectionProperties(_) | Block::Raw(_) => {}
            }
        }
    }

    fn inlines(&mut self, items: &mut Vec<Inline>) {
        let old = std::mem::take(items);
        for mut inl in old {
            match &mut inl {
                Inline::Field { raw, .. } => {
                    if let Some(kind) = field_kind(raw) {
                        let props = crate::load::field_result_props(raw);
                        for (i, line) in self.value(&kind).split('\n').enumerate() {
                            if i > 0 {
                                items.push(Inline::Break(BreakKind::Line, props.clone()));
                            }
                            if !line.is_empty() {
                                items.push(Inline::Run(Run {
                                    text: line.to_string(),
                                    props: props.clone(),
                                }));
                            }
                        }
                        continue;
                    }
                }
                Inline::TextBox { blocks, .. } => self.blocks(blocks),
                Inline::Hyperlink(h) => self.inlines(&mut h.content),
                Inline::Revision {
                    content,
                    content_changed,
                    ..
                } => {
                    let before = content.clone();
                    self.inlines(content);
                    if *content != before {
                        *content_changed = true;
                    }
                }
                _ => {}
            }
            items.push(inl);
        }
    }

    fn value(&mut self, kind: &MergeFieldKind) -> String {
        if *kind == MergeFieldKind::Next {
            self.at += 1;
            return String::new();
        }
        let Some(&row) = self.rows.get(self.at) else {
            return String::new(); // past the last record: an empty label
        };
        let ctx = MergeContext {
            recipients: self.recipients,
            map: self.map,
            row,
            seq: self.at + 1,
        };
        eval(kind, &ctx).unwrap_or_default()
    }
}

/// Whether raw XML is a marker whose `w:id` must stay unique.
fn is_id_marker(raw: &str) -> bool {
    [
        "<w:bookmarkStart",
        "<w:bookmarkEnd",
        "<w:commentRangeStart",
        "<w:commentRangeEnd",
        "w:commentReference",
    ]
    .iter()
    .any(|m| raw.contains(m))
}

/// Drop bookmarks, comment anchors and note references from a later copy.
fn strip_ids(blocks: &mut Vec<Block>) {
    fn inlines(items: &mut Vec<Inline>) {
        items.retain(|i| match i {
            Inline::Raw(raw) => !is_id_marker(raw),
            Inline::FootnoteRef { .. } => false,
            _ => true,
        });
        for i in items.iter_mut() {
            match i {
                Inline::Hyperlink(h) => inlines(&mut h.content),
                Inline::TextBox { blocks, .. } => strip_ids(blocks),
                _ => {}
            }
        }
    }
    blocks.retain(|b| !matches!(b, Block::Raw(raw) if is_id_marker(raw.trim_start())));
    for b in blocks {
        match b {
            Block::Paragraph(p) => inlines(&mut p.content),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        strip_ids(&mut cell.blocks);
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::fields::{merge_field, rule_field};
    use crate::model::{Paragraph, RunProps};
    use crate::package::{MailMerge, load_package, new_package, save_package};

    fn people() -> Recipients {
        Recipients::parse_csv(b"First,City\nJane,Paris\nJohn,Rome\nAmy,Oslo\n").unwrap()
    }

    fn run(t: &str) -> Inline {
        Inline::Run(Run {
            text: t.into(),
            props: RunProps::default(),
        })
    }

    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph {
            content,
            ..Default::default()
        })
    }

    fn opts(recipients: &Recipients, doc_type: MainDocType) -> MergeOptions {
        MergeOptions {
            range: MergeRange::All,
            doc_type,
            map: FieldMap::auto(recipients),
        }
    }

    /// "Dear «First» of «City»." with the first name in bold, as a package
    /// with a mail merge set up.
    fn letter() -> Package {
        let bold = RunProps {
            bold: true,
            ..Default::default()
        };
        let mut pkg = new_package(crate::model::Document {
            body: vec![
                para(vec![
                    run("Dear "),
                    merge_field("First", &bold),
                    run(" of "),
                    merge_field("City", &RunProps::default()),
                    run("."),
                ]),
                Block::SectionProperties(crate::model::SectionProperties {
                    raw: "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:type w:val=\"continuous\"/></w:sectPr>".into(),
                    property_change: None,
                }),
            ],
        });
        pkg.set_mail_merge(Some(&MailMerge {
            doc_type: MainDocType::Letters,
            source: Some("C:\\list.csv".into()),
        }));
        pkg
    }

    /// The merged body's text, one entry per section.
    fn sections(pkg: &Package) -> Vec<String> {
        let mut out = vec![String::new()];
        for b in &pkg.document.body[..pkg.document.content_block_count()] {
            out.last_mut().unwrap().push_str(&b.plain_text());
            out.last_mut().unwrap().push('\n');
            if let Block::Paragraph(p) = b
                && p.props.section_break.is_some()
            {
                out.push(String::new());
            }
        }
        out
    }

    #[test]
    fn letters_one_section_per_record_with_formatting_and_no_mail_merge() {
        let r = people();
        let main = letter();
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        assert_eq!(
            sections(&out),
            [
                "Dear Jane of Paris.\n",
                "Dear John of Rome.\n",
                "Dear Amy of Oslo.\n"
            ]
        );
        // The field result's bold is carried by the merged text.
        let Block::Paragraph(p) = &out.document.body[0] else {
            panic!()
        };
        assert!(
            matches!(&p.content[1], Inline::Run(r) if r.text == "Jane" && r.props.bold),
            "{:?}",
            p.content
        );
        assert!(matches!(&p.content[3], Inline::Run(r) if r.text == "Paris" && !r.props.bold));
        // Each copy starts on a new page: no continuous type anywhere.
        let saved = load_package(&save_package(&out)).unwrap();
        let xml = saved.part_text("word/document.xml").unwrap();
        assert_eq!(xml.matches("<w:sectPr").count(), 3, "{xml}");
        assert!(!xml.contains("continuous"), "{xml}");
        assert!(!xml.contains("MERGEFIELD"), "{xml}");
        // No mail merge in the result; the main document keeps its own.
        assert_eq!(saved.mail_merge(), None);
        let settings = saved.part_text("word/settings.xml").unwrap_or_default();
        assert!(!settings.contains("mailMerge"), "{settings}");
        let rels = saved
            .part_text("word/_rels/settings.xml.rels")
            .unwrap_or_default();
        assert!(!rels.contains("mailMergeSource"), "{rels}");
        assert!(main.mail_merge().is_some());
    }

    #[test]
    fn excluded_records_ranges_and_directory() {
        let mut r = people();
        r.included[1] = false;
        let main = letter();
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        assert_eq!(
            sections(&out),
            ["Dear Jane of Paris.\n", "Dear Amy of Oslo.\n"]
        );
        let mut o = opts(&r, MainDocType::Directory);
        let out = merge_package(&main, &r, &o).unwrap();
        assert_eq!(sections(&out), ["Dear Jane of Paris.\nDear Amy of Oslo.\n"]);
        o.range = MergeRange::Current(1); // the excluded one, previewed
        let out = merge_package(&main, &r, &o).unwrap();
        assert_eq!(sections(&out), ["Dear John of Rome.\n"]);
        o.range = MergeRange::FromTo(1, 2);
        let out = merge_package(&main, &r, &o).unwrap();
        assert_eq!(sections(&out), ["Dear Amy of Oslo.\n"]);
        r.included = vec![false; 3];
        o.range = MergeRange::All;
        assert!(merge_package(&main, &r, &o).is_err());
    }

    #[test]
    fn next_mergerec_and_mergeseq() {
        let r = people();
        let p = RunProps::default();
        let main = new_package(crate::model::Document {
            body: vec![
                para(vec![
                    merge_field("First", &p),
                    run(" #"),
                    rule_field(&MergeFieldKind::MergeSeq, &p).unwrap(),
                ]),
                para(vec![
                    rule_field(&MergeFieldKind::Next, &p).unwrap(),
                    merge_field("First", &p),
                    run(" rec "),
                    rule_field(&MergeFieldKind::MergeRec, &p).unwrap(),
                ]),
            ],
        });
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Labels)).unwrap();
        // Two records per copy; the second copy runs out after Amy.
        assert_eq!(sections(&out), ["Jane #1\nJohn rec 2\n", "Amy #3\n rec \n"]);
    }

    #[test]
    fn address_block_lines_become_line_breaks() {
        let r =
            Recipients::parse_csv(b"First Name,Last Name,Address,City\nJane,Doe,1 Main,Leeds\n")
                .unwrap();
        let main = new_package(crate::model::Document {
            body: vec![para(vec![crate::merge::address_block_field(
                true,
                &RunProps::default(),
            )])],
        });
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        let Block::Paragraph(p) = &out.document.body[0] else {
            panic!()
        };
        let kinds: Vec<String> = p
            .content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => r.text.clone(),
                Inline::Break(BreakKind::Line, _) => "<br>".into(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["Jane Doe", "<br>", "1 Main", "<br>", "Leeds"]);
    }

    #[test]
    fn bookmarks_comments_and_note_refs_only_in_the_first_copy() {
        let r = people();
        let p = RunProps::default();
        let main = new_package(crate::model::Document {
            body: vec![
                Block::Raw("<w:bookmarkStart w:id=\"7\" w:name=\"top\"/>".into()),
                para(vec![
                    Inline::Raw("<w:commentRangeStart w:id=\"0\"/>".into()),
                    merge_field("First", &p),
                    Inline::Raw("<w:commentRangeEnd w:id=\"0\"/>".into()),
                    Inline::Raw("<w:r><w:commentReference w:id=\"0\"/></w:r>".into()),
                    Inline::FootnoteRef {
                        id: 1,
                        endnote: false,
                        raw: "<w:r><w:footnoteReference w:id=\"1\"/></w:r>".into(),
                    },
                    Inline::Raw("<w:bookmarkEnd w:id=\"7\"/>".into()),
                ]),
            ],
        });
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        let xml = crate::serialize::document_to_xml(&out.document);
        for (marker, n) in [
            ("<w:bookmarkStart w:id=\"7\"", 1),
            ("<w:bookmarkEnd w:id=\"7\"", 1),
            ("<w:commentRangeStart w:id=\"0\"", 1),
            ("<w:commentReference w:id=\"0\"", 1),
            ("<w:footnoteReference w:id=\"1\"", 1),
        ] {
            assert_eq!(xml.matches(marker).count(), n, "{marker}: {xml}");
        }
        assert!(xml.contains("Amy"), "{xml}");
    }
}
