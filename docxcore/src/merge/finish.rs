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
/// Bookmarks, comment ranges and references, and footnote/endnote
/// references are kept in the first copy only, so their ids do not repeat.
/// Tracked changes (`w:ins`, `w:del`, `w:moveTo`, move ranges) keep their own
/// ids in every copy. Merge fields in headers and footers (shared parts) stay
/// fields. The result has no `w:mailMerge`.
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

/// Check for Errors: the merge fields in `doc` that name no column of the
/// recipient list (through Match Fields where they are mapped), each once, in
/// document order. Empty when the merge would complete without errors.
pub fn check_errors(
    doc: &crate::model::Document,
    recipients: &Recipients,
    map: &FieldMap,
) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    let row = recipients.included_rows().first().copied().unwrap_or(0);
    super::preview::each_field(&doc.body, &mut |raw| {
        let Some(kind @ MergeFieldKind::MergeField { .. }) = field_kind(raw) else {
            return;
        };
        let ctx = MergeContext {
            recipients,
            map,
            row,
            seq: 1,
        };
        if eval(&kind, &ctx).is_none() {
            let MergeFieldKind::MergeField { name, .. } = kind else {
                return;
            };
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
    });
    missing
}

/// The merge fields in `doc` that a merge leaves as they are: inside a
/// tracked move (`w:moveTo`/`w:moveFrom`) or another element docxy keeps as
/// raw XML (an inline `w:customXml`, a block content control), which merging,
/// previewing and Check for Errors's column test do not reach. Each is named
/// as Check for Errors lists it: a MERGEFIELD by its column, any other by its
/// placeholder.
pub fn unmerged_fields(doc: &crate::model::Document) -> Vec<String> {
    fn scan(raw: &str, out: &mut Vec<String>) {
        let mut instrs: Vec<String> = crate::load::start_tags(raw, "w:fldSimple")
            .into_iter()
            .filter_map(|(_, el)| crate::load::xml_attr_value(el, "w:instr"))
            .map(|i| crate::field::xml_unescape(&i))
            .collect();
        let mut cur: Option<String> = None;
        for event in crate::field::field_events(raw) {
            match event {
                crate::field::FieldEvent::Begin => cur = Some(String::new()),
                crate::field::FieldEvent::Instr(t) => {
                    if let Some(c) = cur.as_mut() {
                        c.push_str(&crate::field::xml_unescape(&t));
                    }
                }
                _ => instrs.extend(cur.take()),
            }
        }
        for instr in instrs {
            let name = match super::fields::instr_kind(&instr) {
                Some(MergeFieldKind::MergeField { name, .. }) => name,
                Some(kind) => kind.placeholder(),
                None => continue,
            };
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    fn inlines(items: &[Inline], out: &mut Vec<String>) {
        for i in items {
            match i {
                Inline::Raw(raw) | Inline::UnsupportedRevision { raw, .. } => scan(raw, out),
                Inline::Hyperlink(h) => inlines(&h.content, out),
                Inline::Revision { content, .. } => inlines(content, out),
                Inline::TextBox { blocks, .. } => walk(blocks, out),
                _ => {}
            }
        }
    }
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => inlines(&p.content, out),
                Block::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            walk(&cell.blocks, out);
                        }
                    }
                }
                Block::Raw(raw) => scan(raw, out),
                Block::SectionProperties(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.body, &mut out);
    out
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
                // A loaded link is saved from its raw XML unless its content
                // is marked changed, which would keep the field in the copy.
                Inline::Hyperlink(h) => {
                    let before = h.content.clone();
                    self.inlines(&mut h.content);
                    if h.content != before {
                        h.content_changed = true;
                    }
                }
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

/// The elements whose `w:id` must stay unique across the copies.
const ID_MARKERS: [&str; 7] = [
    "w:bookmarkStart",
    "w:bookmarkEnd",
    "w:commentRangeStart",
    "w:commentRangeEnd",
    "w:commentReference",
    "w:footnoteReference",
    "w:endnoteReference",
];

/// `raw` without its id markers, and without a run that held nothing but
/// one (and its properties). Everything else stays: the text of a moved
/// range, a content control or a custom XML element is kept in every copy.
fn strip_marker_xml(raw: &str) -> String {
    let mut xml = raw.to_string();
    for name in ID_MARKERS {
        while let Some((a, b)) = crate::sect::find_element(&xml, name) {
            xml.replace_range(a..b, "");
        }
    }
    if xml.len() == raw.len() {
        return xml;
    }
    // Runs the markers emptied.
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml.as_str();
    while let Some((a, b)) = crate::sect::find_element(rest, "w:r") {
        out.push_str(&rest[..a]);
        let run = &rest[a..b];
        // What the run holds besides its properties (nothing for `<w:r/>`).
        let open_end = run.find('>').map_or(run.len(), |gt| gt + 1);
        let inner = if run[..open_end].ends_with("/>") {
            ""
        } else {
            run[open_end..]
                .strip_suffix("</w:r>")
                .unwrap_or(&run[open_end..])
        };
        let inner = crate::sect::remove_element(inner, "w:rPr");
        if !inner.trim().is_empty() {
            out.push_str(run);
        }
        rest = &rest[b..];
    }
    out.push_str(rest);
    out
}

/// Drop bookmarks, comment anchors and note references from a later copy,
/// also inside links and tracked changes (whose raw XML is then rebuilt from
/// their content on save). A container holding one keeps all but the
/// marker; nothing that holds text is dropped.
fn strip_ids(blocks: &mut Vec<Block>) {
    /// Whether anything was dropped or changed.
    fn inlines(items: &mut Vec<Inline>) -> bool {
        let mut changed = false;
        items.retain_mut(|i| match i {
            Inline::Raw(raw) | Inline::UnsupportedRevision { raw, .. } => {
                let kept = strip_marker_xml(raw);
                if kept.len() == raw.len() {
                    return true;
                }
                changed = true;
                *raw = kept;
                !raw.trim().is_empty()
            }
            Inline::FootnoteRef { .. } => {
                changed = true;
                false
            }
            _ => true,
        });
        for i in items.iter_mut() {
            match i {
                Inline::Hyperlink(h) => {
                    if inlines(&mut h.content) {
                        h.content_changed = true;
                        changed = true;
                    }
                }
                Inline::Revision {
                    content,
                    content_changed,
                    ..
                } => {
                    if inlines(content) {
                        *content_changed = true;
                        changed = true;
                    }
                }
                Inline::TextBox { blocks, .. } => strip_ids(blocks),
                _ => {}
            }
        }
        changed
    }
    blocks.retain_mut(|b| match b {
        Block::Raw(raw) => {
            *raw = strip_marker_xml(raw);
            !raw.trim().is_empty()
        }
        _ => true,
    });
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                inlines(&mut p.content);
            }
            Block::Table(t) => {
                // Bookmarks and comment ranges between rows.
                t.row_boundaries.retain_mut(|b| match &mut b.kind {
                    crate::model::TableRowBoundaryKind::Raw(raw) => {
                        *raw = strip_marker_xml(raw);
                        !raw.trim().is_empty()
                    }
                    _ => true,
                });
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
    fn check_errors_lists_fields_with_no_column() {
        let r = people();
        let p = RunProps::default();
        let doc = crate::model::Document {
            body: vec![para(vec![
                merge_field("First", &p),
                merge_field("Zip", &p),
                merge_field("City", &p),
                merge_field("Zip", &p),
                merge_field("Phone", &p),
            ])],
        };
        let map = FieldMap::auto(&r);
        assert_eq!(check_errors(&doc, &r, &map), ["Zip", "Phone"]);
        assert!(check_errors(&letter().document, &r, &map).is_empty());
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
                // The same markers inside a tracked insertion, as Word loads it.
                tracked_markers(),
            ],
        });
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        let xml = crate::serialize::document_to_xml(&out.document);
        for (marker, n) in [
            ("<w:bookmarkStart w:id=\"8\"", 1),
            ("<w:commentReference w:id=\"3\"", 1),
            ("<w:footnoteReference w:id=\"2\"", 1),
            ("<w:bookmarkStart w:id=\"7\"", 1),
            ("<w:bookmarkEnd w:id=\"7\"", 1),
            ("<w:commentRangeStart w:id=\"0\"", 1),
            ("<w:commentReference w:id=\"0\"", 1),
            ("<w:footnoteReference w:id=\"1\"", 1),
        ] {
            assert_eq!(xml.matches(marker).count(), n, "{marker}: {xml}");
        }
        assert!(xml.contains("Amy"), "{xml}");
        // The insertion's own text is in every copy.
        assert_eq!(xml.matches(">ins<").count(), 3, "{xml}");
    }

    /// r2 M1: a container that holds a marker loses only the marker. The
    /// moved text of a w:moveTo and a custom XML element's run stay in every
    /// copy; their bookmark and comment reference only in the first.
    #[test]
    fn containers_keep_their_text_and_lose_only_their_markers() {
        let r = people();
        let xml = format!(
            "<w:document xmlns:w=\"{W}\"><w:body><w:p>\
             <w:moveTo w:id=\"70\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\">\
             <w:r><w:t>moved</w:t></w:r><w:r><w:commentReference w:id=\"5\"/></w:r>\
             </w:moveTo>\
             <w:customXml w:element=\"x\"><w:bookmarkStart w:id=\"6\" w:name=\"c\"/>\
             <w:r><w:t>custom</w:t></w:r><w:bookmarkEnd w:id=\"6\"/></w:customXml>\
             </w:p></w:body></w:document>"
        );
        let doc = crate::load::parse_document_xml(&xml, &Default::default());
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        assert!(
            p.content.iter().any(
                |i| matches!(i, Inline::UnsupportedRevision { raw, .. } if raw.contains("moved"))
            ),
            "{:?}",
            p.content
        );
        let main = new_package(doc);
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        let xml = crate::serialize::document_to_xml(&out.document);
        assert_eq!(xml.matches(">moved<").count(), 3, "{xml}");
        assert_eq!(xml.matches(">custom<").count(), 3, "{xml}");
        assert_eq!(xml.matches("<w:moveTo ").count(), 3, "{xml}");
        assert_eq!(
            xml.matches("<w:commentReference w:id=\"5\"").count(),
            1,
            "{xml}"
        );
        assert_eq!(
            xml.matches("<w:bookmarkStart w:id=\"6\"").count(),
            1,
            "{xml}"
        );
        assert_eq!(xml.matches("<w:bookmarkEnd w:id=\"6\"").count(), 1, "{xml}");
    }

    /// r3 m1: a merge field a merge cannot reach is reported, not lost.
    #[test]
    fn unmerged_fields_lists_fields_inside_moves_and_raw_wrappers() {
        let xml = format!(
            "<w:document xmlns:w=\"{W}\"><w:body><w:p>\
             <w:moveTo w:id=\"70\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\">\
             <w:fldSimple w:instr=\" MERGEFIELD City \"><w:r><w:t>\u{AB}City\u{BB}</w:t></w:r></w:fldSimple>\
             </w:moveTo>\
             <w:customXml w:element=\"x\">\
             <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> GREETING</w:instrText></w:r>\
             <w:r><w:instrText>LINE </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
             <w:r><w:t>x</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:customXml>\
             <w:fldSimple w:instr=\" MERGEFIELD First \"><w:r><w:t>f</w:t></w:r></w:fldSimple>\
             </w:p></w:body></w:document>"
        );
        let doc = crate::load::parse_document_xml(&xml, &Default::default());
        assert_eq!(
            unmerged_fields(&doc),
            ["City".to_string(), "\u{AB}GreetingLine\u{BB}".to_string()]
        );
        assert!(unmerged_fields(&letter().document).is_empty());
    }

    /// r3 m2: a bookmark between table rows is a row boundary; it too is
    /// kept in the first copy only.
    #[test]
    fn table_row_boundary_markers_only_in_the_first_copy() {
        let r = people();
        let xml = format!(
            "<w:document xmlns:w=\"{W}\"><w:body><w:tbl><w:tblGrid><w:gridCol w:w=\"100\"/></w:tblGrid>\
             <w:bookmarkStart w:id=\"40\" w:name=\"rows\"/>\
             <w:tr><w:tc><w:p><w:fldSimple w:instr=\" MERGEFIELD First \"><w:r><w:t>f</w:t></w:r></w:fldSimple></w:p></w:tc></w:tr>\
             <w:bookmarkEnd w:id=\"40\"/>\
             </w:tbl><w:p/></w:body></w:document>"
        );
        let doc = crate::load::parse_document_xml(&xml, &Default::default());
        assert!(
            matches!(&doc.body[0], Block::Table(t) if !t.row_boundaries.is_empty()),
            "{:?}",
            doc.body[0]
        );
        let out = merge_package(&new_package(doc), &r, &opts(&r, MainDocType::Letters)).unwrap();
        for b in &out.document.body {
            if let Block::Table(t) = b {
                assert_eq!(t.validate_row_boundaries(), Ok(()));
            }
        }
        let xml = crate::serialize::document_to_xml(&out.document);
        assert_eq!(
            xml.matches("<w:bookmarkStart w:id=\"40\"").count(),
            1,
            "{xml}"
        );
        assert_eq!(
            xml.matches("<w:bookmarkEnd w:id=\"40\"").count(),
            1,
            "{xml}"
        );
        assert!(xml.contains("Amy"), "{xml}");
    }

    #[test]
    fn strip_marker_xml_keeps_everything_else() {
        assert_eq!(
            strip_marker_xml("<w:bookmarkStart w:id=\"1\" w:name=\"a\"/>"),
            ""
        );
        assert_eq!(
            strip_marker_xml("<w:r><w:rPr><w:b/></w:rPr><w:endnoteReference w:id=\"2\"/></w:r>"),
            ""
        );
        assert_eq!(
            strip_marker_xml(
                "<w:sdt><w:sdtContent><w:r><w:t>k</w:t></w:r><w:r><w:footnoteReference w:id=\"3\"/></w:r></w:sdtContent></w:sdt>"
            ),
            "<w:sdt><w:sdtContent><w:r><w:t>k</w:t></w:r></w:sdtContent></w:sdt>"
        );
        let plain = "<w:r><w:t>text</w:t></w:r>";
        assert_eq!(strip_marker_xml(plain), plain);
    }

    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

    /// A paragraph whose tracked insertion holds a bookmark, a comment
    /// reference and a footnote reference (r1 M3).
    fn tracked_markers() -> Block {
        let xml = format!(
            "<w:document xmlns:w=\"{W}\"><w:body><w:p>\
             <w:ins w:id=\"90\" w:author=\"A\" w:date=\"2026-01-01T00:00:00Z\">\
             <w:bookmarkStart w:id=\"8\" w:name=\"b\"/>\
             <w:r><w:t>ins</w:t></w:r>\
             <w:r><w:commentReference w:id=\"3\"/></w:r>\
             <w:r><w:footnoteReference w:id=\"2\"/></w:r>\
             <w:bookmarkEnd w:id=\"8\"/>\
             </w:ins></w:p></w:body></w:document>"
        );
        let doc = crate::load::parse_document_xml(&xml, &Default::default());
        assert!(
            matches!(&doc.body[0], Block::Paragraph(p) if matches!(p.content[0], Inline::Revision { .. })),
            "{:?}",
            doc.body
        );
        doc.body[0].clone()
    }

    /// A loaded hyperlink is saved from its raw XML unless its content is
    /// marked changed: a merge field inside one must still merge (r1 M2).
    #[test]
    fn a_merge_field_inside_a_loaded_hyperlink_merges() {
        let r = Recipients::parse_csv(b"Email\njane@x.org\njohn@y.org\n").unwrap();
        let body = format!(
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"{W}\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
             <w:body><w:p><w:hyperlink r:id=\"rId9\">\
             <w:r><w:t xml:space=\"preserve\">Mail </w:t></w:r>\
             <w:fldSimple w:instr=\" MERGEFIELD Email \"><w:r><w:t>\u{AB}Email\u{BB}</w:t></w:r></w:fldSimple>\
             </w:hyperlink></w:p><w:sectPr/></w:body></w:document>"
        );
        let docx = crate::zipwrite::write_zip(&[
            (
                "[Content_Types].xml".into(),
                b"<?xml version=\"1.0\"?><Types/>".to_vec(),
            ),
            (
                "_rels/.rels".into(),
                b"<?xml version=\"1.0\"?><Relationships><Relationship Id=\"rId1\" Target=\"word/document.xml\"/></Relationships>"
                    .to_vec(),
            ),
            ("word/document.xml".into(), body.into_bytes()),
            (
                "word/_rels/document.xml.rels".into(),
                b"<?xml version=\"1.0\"?><Relationships><Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"mailto:x@y.z\" TargetMode=\"External\"/></Relationships>"
                    .to_vec(),
            ),
        ]);
        let main = load_package(&docx).unwrap();
        let Block::Paragraph(p) = &main.document.body[0] else {
            panic!()
        };
        assert!(
            matches!(&p.content[0], Inline::Hyperlink(h) if h.raw.is_some() && !h.content.is_empty()),
            "{:?}",
            p.content
        );
        let out = merge_package(&main, &r, &opts(&r, MainDocType::Letters)).unwrap();
        let saved = load_package(&save_package(&out)).unwrap();
        let xml = saved.part_text("word/document.xml").unwrap();
        assert!(!xml.contains("MERGEFIELD"), "{xml}");
        assert!(
            xml.contains("jane@x.org") && xml.contains("john@y.org"),
            "{xml}"
        );
        assert_eq!(xml.matches("<w:hyperlink").count(), 2, "{xml}");
    }
}
