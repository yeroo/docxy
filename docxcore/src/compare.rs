//! Word's Review > Compare: build a document whose tracked changes turn an
//! *original* document into a *revised* one.
//!
//! [`compare_packages`] returns a copy of the revised package (styles,
//! numbering, headers/footers, relationships and media all come from it) whose
//! body holds `w:ins`/`w:del` revisions: accepting them all yields the revised
//! text, rejecting them all yields the original text.
//!
//! How it works:
//! 1. Both bodies are cloned and their own tracked changes accepted (Word
//!    compares the accepted states too).
//! 2. Blocks are aligned per container (the body, then each table cell) with a
//!    Myers diff over a text key. Within each gap between matched blocks, a
//!    deleted and an inserted paragraph that share at least half their words
//!    are paired as one *modified* paragraph; the rest are whole-paragraph
//!    deletions/insertions.
//! 3. A modified paragraph gets a word-level diff (runs of word characters,
//!    runs of whitespace, single punctuation characters; each object is one
//!    token). Unchanged and inserted text keep the revised run formatting;
//!    deleted text keeps the original run formatting.
//! 4. A whole-paragraph insertion/deletion also marks its paragraph mark, so
//!    accepting or rejecting it adds or removes the paragraph. The final mark
//!    before the end of a container (or before a table) cannot be marked: the
//!    mark *before* the changed run is marked instead, which is Word's own
//!    convention, and a mixed run of deletions and insertions at the end pairs
//!    its last deletion and insertion as one modified paragraph.
//!
//! Limits (reported in [`CompareResult::skipped`] where content is involved):
//! formatting-only differences are not marked; headers, footers, notes and
//! comments come from the revised package unchanged; a table whose shape
//! changed is kept as revised; deleted objects that reference a relationship
//! of the original package (images, charts, links' targets) and deleted note
//! references cannot be carried into the revised package. Zero-width markers
//! (bookmarks, comment ranges, proofing marks, field characters) are not
//! compared: the original's are dropped and the revised document's are kept
//! where they are. After a paragraph merge the surviving paragraph takes the
//! properties of the later paragraph (Word's rule), so accepting the deletion
//! of a container's last paragraph leaves the original's last paragraph
//! properties on the merged paragraph.

use std::collections::HashSet;

use crate::load::parse_document_xml;
use crate::model::{
    Block, BreakKind, Document, Hyperlink, Inline, ParProps, Paragraph, PropertyChange,
    PropertyScope, PropertySnapshot, RevisionKind, RevisionMetadata, Run, RunProps, Table,
    UnsupportedRevisionKind,
};
use crate::package::Package;
use crate::review::RevisionOutcome;
use crate::serialize::{document_to_xml, esc_attr};
use crate::xml::{Event, XmlParser};

/// Who and when the comparison's revisions are attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareOptions {
    pub author: String,
    /// `w:date`, normally UTC ISO-8601 `YYYY-MM-DDTHH:MM:SSZ`.
    pub date: String,
}

/// Content the comparison could not express as tracked changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareSkip {
    /// A table whose shape changed (kept as revised), or a table present in
    /// only one document. `index` is the top-level block index in the result.
    Table { index: usize },
    /// A deleted object that references the original package, or a block-level
    /// element (such as a content control) present in only one document.
    Object,
    /// Deleted original run/paragraph property children (formatting) dropped
    /// because they use a namespace prefix the revised package binds to
    /// another namespace. Reported once per comparison.
    Formatting,
    /// A deleted footnote/endnote reference.
    NoteRef,
    /// A tracked change in an input that could not be accepted first
    /// (moves, custom-XML ranges, table-cell revisions).
    UnsupportedRevision { revision: UnsupportedRevisionKind },
    /// A whole-paragraph change whose paragraph mark could not be marked; it
    /// was compared in place, so accepting or rejecting leaves an extra empty
    /// paragraph. `index` is the top-level block index in the result.
    ParagraphMark { index: usize },
}

impl CompareSkip {
    /// The stable kind name (`table`, `object`, `formatting`, `note-ref`,
    /// `unsupported-revision`, `paragraph-mark`).
    pub fn kind(&self) -> &'static str {
        match self {
            CompareSkip::Table { .. } => "table",
            CompareSkip::Object => "object",
            CompareSkip::Formatting => "formatting",
            CompareSkip::NoteRef => "note-ref",
            CompareSkip::UnsupportedRevision { .. } => "unsupported-revision",
            CompareSkip::ParagraphMark { .. } => "paragraph-mark",
        }
    }
}

/// Count skipped items per kind, in first-seen order: `[("table", 2), …]`.
pub fn skip_counts(skipped: &[CompareSkip]) -> Vec<(&'static str, usize)> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for skip in skipped {
        match counts.iter_mut().find(|(kind, _)| *kind == skip.kind()) {
            Some((_, n)) => *n += 1,
            None => counts.push((skip.kind(), 1)),
        }
    }
    counts
}

/// The comparison: the result package and what it holds.
#[derive(Debug, Clone)]
pub struct CompareResult {
    pub package: Package,
    /// Insertion revisions created (inline and paragraph-mark).
    pub insertions: usize,
    /// Deletion revisions created (inline and paragraph-mark).
    pub deletions: usize,
    pub skipped: Vec<CompareSkip>,
}

/// Compare `original` with `revised`; neither is modified.
pub fn compare_packages(
    original: &Package,
    revised: &Package,
    opts: &CompareOptions,
) -> CompareResult {
    let mut skipped = Vec::new();
    // Deleted content is original XML: the result's root must bind its
    // namespace prefixes too. A prefix bound differently in the two roots
    // cannot be carried; markup using it is dropped and reported.
    let mut package = revised.clone();
    let conflicts = package.adopt_root_namespaces(original);
    let original_doc = accepted(&original.document, &mut skipped);
    let revised_doc = accepted(&revised.document, &mut skipped);
    let mut cx = Compare {
        opts,
        styles: part_attr_values(revised, "word/styles.xml", "w:style", "w:styleId"),
        nums: part_attr_values(revised, "word/numbering.xml", "w:num", "w:numId"),
        next_id: max_revision_id(revised) + 1,
        insertions: 0,
        deletions: 0,
        skipped,
        conflicts,
        dropped_conflicting: false,
    };
    let body = cx.compare_document(&original_doc, &revised_doc);
    let rels = revised.document_rels();
    // Round-trip through WordprocessingML so the loader builds the revision
    // nodes (targets, raw wrappers, display cues) exactly as for a file.
    package.document = parse_document_xml(&document_to_xml(&body), &rels);
    CompareResult {
        package,
        insertions: cx.insertions,
        deletions: cx.deletions,
        skipped: cx.skipped,
    }
}

/// A clone of `document` with its own revisions accepted; revisions that
/// cannot be accepted are reported.
fn accepted(document: &Document, skipped: &mut Vec<CompareSkip>) -> Document {
    let mut document = document.clone();
    for outcome in document.accept_all_revisions() {
        if let RevisionOutcome::Unsupported { kind, .. } = outcome {
            skipped.push(CompareSkip::UnsupportedRevision { revision: kind });
        }
    }
    document
}

/// Every `attr` value on `element` start tags in a package part.
fn part_attr_values(pkg: &Package, part: &str, element: &str, attr: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(xml) = pkg.part(part).and_then(|b| std::str::from_utf8(b).ok()) else {
        return out;
    };
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Start if parser.name() == element => {
                let value = parser.attr(attr);
                if !value.is_empty() {
                    out.insert(value.to_string());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The largest numeric `w:id` in the revised package's WordprocessingML parts,
/// so the comparison's revision ids never collide with existing ones.
fn max_revision_id(pkg: &Package) -> u64 {
    const NEEDLE: &str = " w:id=\"";
    let mut max = 0;
    for name in pkg.part_names() {
        if !(name.starts_with("word/") && name.ends_with(".xml")) {
            continue;
        }
        let Some(xml) = pkg.part(name).and_then(|b| std::str::from_utf8(b).ok()) else {
            continue;
        };
        let mut rest = xml;
        while let Some(at) = rest.find(NEEDLE) {
            rest = &rest[at + NEEDLE.len()..];
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if let Ok(id) = rest[..digits].parse::<u64>() {
                max = max.max(id);
            }
        }
    }
    max
}

struct Compare<'o> {
    opts: &'o CompareOptions,
    styles: HashSet<String>,
    nums: HashSet<String>,
    next_id: u64,
    insertions: usize,
    deletions: usize,
    skipped: Vec<CompareSkip>,
    /// Namespace prefixes the original and revised roots bind differently.
    conflicts: Vec<String>,
    /// Whether markup using one of them was dropped (reported once).
    dropped_conflicting: bool,
}

/// One aligned unit of a container's result.
enum Item<'a> {
    /// A block unchanged between the documents (emitted from the revised one).
    Same(&'a Block),
    Modified(&'a Paragraph, &'a Paragraph),
    Inserted(&'a Paragraph),
    Deleted(&'a Paragraph),
    Tables(&'a Table, &'a Table),
    /// A non-paragraph block only in the revised document (kept unmarked).
    RevisedOnly(&'a Block),
}

impl Item<'_> {
    fn is_paragraph(&self) -> bool {
        match self {
            Item::Same(block) => matches!(block, Block::Paragraph(_)),
            Item::Modified(..) | Item::Inserted(_) | Item::Deleted(_) => true,
            Item::Tables(..) | Item::RevisedOnly(_) => false,
        }
    }

    /// Whether this paragraph's mark carries a section break.
    fn ends_section(&self) -> bool {
        let props = match self {
            Item::Same(Block::Paragraph(p)) => &p.props,
            Item::Modified(_, p) | Item::Inserted(p) | Item::Deleted(p) => &p.props,
            _ => return false,
        };
        props.section_break.is_some() || props.section_property_change.is_some()
    }

    fn change_kind(&self) -> Option<RevisionKind> {
        match self {
            Item::Inserted(_) => Some(RevisionKind::Insert),
            Item::Deleted(_) => Some(RevisionKind::Delete),
            _ => None,
        }
    }
}

impl<'o> Compare<'o> {
    fn compare_document(&mut self, original: &Document, revised: &Document) -> Document {
        let content = |d: &Document| -> Vec<Block> {
            d.body
                .iter()
                .filter(|b| !matches!(b, Block::SectionProperties(_)))
                .cloned()
                .collect()
        };
        let mut body = self.compare_blocks(&content(original), &content(revised), None);
        body.extend(
            revised
                .body
                .iter()
                .filter(|b| matches!(b, Block::SectionProperties(_)))
                .cloned(),
        );
        Document { body }
    }

    /// Compare one container's blocks. `top` is the top-level block index of
    /// the enclosing table when comparing a cell.
    fn compare_blocks(
        &mut self,
        original: &[Block],
        revised: &[Block],
        top: Option<usize>,
    ) -> Vec<Block> {
        let original_keys: Vec<String> = original.iter().map(block_key).collect();
        let revised_keys: Vec<String> = revised.iter().map(block_key).collect();
        let mut items: Vec<Item> = Vec::new();
        let (mut deleted, mut inserted): (Vec<&Block>, Vec<&Block>) = (Vec::new(), Vec::new());
        for op in diff(&original_keys, &revised_keys) {
            match op {
                Op::Equal(_, j) => {
                    self.flush_gap(&mut deleted, &mut inserted, &mut items, top);
                    items.push(Item::Same(&revised[j]));
                }
                Op::Delete(i) => deleted.push(&original[i]),
                Op::Insert(j) => inserted.push(&revised[j]),
            }
        }
        self.flush_gap(&mut deleted, &mut inserted, &mut items, top);
        fix_mixed_tails(&mut items);
        let marks = self.plan_marks(&items, top);

        let mut out = Vec::with_capacity(items.len());
        for (item, mark) in items.into_iter().zip(marks) {
            let index = top.unwrap_or(out.len());
            let inserted = matches!(item, Item::Inserted(_));
            let mut block = match item {
                Item::Same(block) => block.clone(),
                Item::Modified(o, r) => Block::Paragraph(Paragraph {
                    props: r.props.clone(),
                    content: self.diff_paragraph(o, r),
                }),
                Item::Inserted(r) => Block::Paragraph(Paragraph {
                    props: r.props.clone(),
                    content: self.whole_paragraph(r, RevisionKind::Insert),
                }),
                Item::Deleted(o) => Block::Paragraph(Paragraph {
                    props: self.original_par_props(&o.props),
                    content: self.whole_paragraph(o, RevisionKind::Delete),
                }),
                Item::Tables(o, r) => Block::Table(self.compare_tables(o, r, index)),
                Item::RevisedOnly(block) => {
                    self.skipped.push(match block {
                        Block::Table(_) => CompareSkip::Table { index },
                        _ => CompareSkip::Object,
                    });
                    block.clone()
                }
            };
            if let (Some(kind), Block::Paragraph(p)) = (mark, &mut block) {
                self.mark_paragraph(&mut p.props, kind);
            }
            // An inserted paragraph whose mark could not be marked stays on
            // Reject All; when it ends a section, track the break itself as
            // new (a section change from no section properties) so rejecting
            // removes it.
            if let (None, true, Block::Paragraph(p)) = (mark, inserted, &mut block) {
                if p.props.section_break.is_some() && p.props.section_property_change.is_none() {
                    p.props.section_property_change = Some(self.new_section_change());
                }
            }
            out.push(block);
        }
        out
    }

    /// Turn a gap (unmatched original blocks, unmatched revised blocks) into
    /// items, pairing similar paragraphs and tables in order.
    fn flush_gap<'a>(
        &mut self,
        deleted: &mut Vec<&'a Block>,
        inserted: &mut Vec<&'a Block>,
        items: &mut Vec<Item<'a>>,
        top: Option<usize>,
    ) {
        // How far ahead a deleted block looks for a partner, so a large
        // rewrite stays linear.
        const WINDOW: usize = 16;
        let mut j = 0;
        for d in deleted.drain(..) {
            let partner = (j..inserted.len().min(j + WINDOW)).find(|&k| pairable(d, inserted[k]));
            match partner {
                Some(k) => {
                    for r in &inserted[j..k] {
                        items.push(inserted_item(r));
                    }
                    items.push(match (d, inserted[k]) {
                        (Block::Paragraph(o), Block::Paragraph(r)) => Item::Modified(o, r),
                        (Block::Table(o), Block::Table(r)) => Item::Tables(o, r),
                        _ => unreachable!("pairable"),
                    });
                    j = k + 1;
                }
                None => match d {
                    Block::Paragraph(o) => items.push(Item::Deleted(o)),
                    Block::Table(_) => self.skipped.push(CompareSkip::Table {
                        index: top.unwrap_or(items.len()),
                    }),
                    _ => self.skipped.push(CompareSkip::Object),
                },
            }
        }
        for r in &inserted[j..] {
            items.push(inserted_item(r));
        }
        inserted.clear();
    }

    /// Which paragraph marks to mark, per item (see the module docs).
    fn plan_marks(&mut self, items: &[Item], top: Option<usize>) -> Vec<Option<RevisionKind>> {
        let mut marks = vec![None; items.len()];
        for (start, end) in paragraph_segments(items) {
            for k in start..end.saturating_sub(1) {
                marks[k] = items[k].change_kind();
            }
            let mut tail = end;
            while tail > start && items[tail - 1].change_kind().is_some() {
                tail -= 1;
            }
            if tail == end {
                continue;
            }
            // A single-kind run of changes ends the segment (mixed tails were
            // paired by `fix_mixed_tails`): mark the mark before it, unless a
            // section break is involved. Removing a mark that ends a section
            // merges the break away; merging into a run paragraph that ends a
            // section takes its break. Either way the change stays in place.
            let kind = items[end - 1].change_kind();
            let sections = items[tail - 1..end].iter().any(Item::ends_section);
            if tail > start && !sections {
                marks[tail - 1] = kind;
            } else {
                self.skipped.push(CompareSkip::ParagraphMark {
                    index: top.unwrap_or(end - 1),
                });
            }
        }
        marks
    }

    fn compare_tables(&mut self, original: &Table, revised: &Table, index: usize) -> Table {
        let same_shape = original.rows.len() == revised.rows.len()
            && original
                .rows
                .iter()
                .zip(&revised.rows)
                .all(|(o, r)| o.cells.len() == r.cells.len());
        let mut table = revised.clone();
        if !same_shape {
            self.skipped.push(CompareSkip::Table { index });
            return table;
        }
        for (row, original_row) in table.rows.iter_mut().zip(&original.rows) {
            for (cell, original_cell) in row.cells.iter_mut().zip(&original_row.cells) {
                cell.blocks = self.compare_blocks(&original_cell.blocks, &cell.blocks, Some(index));
            }
        }
        table
    }

    /// A paragraph's content wholly inserted (revised) or deleted (original).
    fn whole_paragraph(&mut self, paragraph: &Paragraph, kind: RevisionKind) -> Vec<Inline> {
        let (atoms, links) = atoms(paragraph);
        let tokens = (0..atoms.len()).filter(|&i| !atoms[i].is_anchor());
        match kind {
            RevisionKind::Insert => {
                let steps = with_anchors(tokens.map(Op::Insert), &atoms);
                self.emit(&steps, &[], &atoms, &links)
            }
            RevisionKind::Delete => {
                let steps: Vec<Op> = tokens.map(Op::Delete).collect();
                self.emit(&steps, &atoms, &[], &[])
            }
        }
    }

    /// The word-level diff of a modified paragraph.
    fn diff_paragraph(&mut self, original: &Paragraph, revised: &Paragraph) -> Vec<Inline> {
        let (original_atoms, _) = atoms(original);
        let (revised_atoms, links) = atoms(revised);
        let original_tokens: Vec<usize> = (0..original_atoms.len())
            .filter(|&i| !original_atoms[i].is_anchor())
            .collect();
        let revised_tokens: Vec<usize> = (0..revised_atoms.len())
            .filter(|&i| !revised_atoms[i].is_anchor())
            .collect();
        let original_keys: Vec<&str> = original_tokens
            .iter()
            .map(|&i| original_atoms[i].key.as_str())
            .collect();
        let revised_keys: Vec<&str> = revised_tokens
            .iter()
            .map(|&i| revised_atoms[i].key.as_str())
            .collect();
        let ops = diff(&original_keys, &revised_keys)
            .into_iter()
            .map(|op| match op {
                Op::Equal(i, j) => Op::Equal(original_tokens[i], revised_tokens[j]),
                Op::Delete(i) => Op::Delete(original_tokens[i]),
                Op::Insert(j) => Op::Insert(revised_tokens[j]),
            });
        let steps = with_anchors(ops, &revised_atoms);
        self.emit(&steps, &original_atoms, &revised_atoms, &links)
    }

    /// Build inlines from diff steps: equal and inserted atoms from the revised
    /// paragraph (re-wrapped in their hyperlinks), deleted ones from the
    /// original, each maximal inserted/deleted stretch in one revision.
    fn emit(
        &mut self,
        steps: &[Op],
        original: &[Atom],
        revised: &[Atom],
        links: &[Hyperlink],
    ) -> Vec<Inline> {
        // The hyperlink each step belongs to: a deleted stretch joins a link
        // only when the revised text on both sides of it is in that link.
        let link_of = |step: &Op| match step {
            Op::Equal(_, j) | Op::Insert(j) => revised[*j].link,
            Op::Delete(_) => None,
        };
        let mut step_links: Vec<Option<usize>> = steps.iter().map(link_of).collect();
        for k in 0..steps.len() {
            if matches!(steps[k], Op::Delete(_)) {
                let before = steps[..k]
                    .iter()
                    .rev()
                    .find(|s| !matches!(s, Op::Delete(_)))
                    .and_then(link_of);
                let after = steps[k + 1..]
                    .iter()
                    .find(|s| !matches!(s, Op::Delete(_)))
                    .and_then(link_of);
                step_links[k] = if before == after { before } else { None };
            }
        }

        let mut out = Vec::new();
        let mut k = 0;
        while k < steps.len() {
            let link = step_links[k];
            let mut end = k + 1;
            while end < steps.len() && step_links[end] == link {
                end += 1;
            }
            let inlines = self.emit_wrapped(&steps[k..end], original, revised);
            match link {
                Some(index) => {
                    let mut hyperlink = links[index].clone();
                    hyperlink.runs.clear();
                    hyperlink.content = inlines;
                    hyperlink.content_changed = true;
                    out.push(Inline::Hyperlink(hyperlink));
                }
                None => out.extend(inlines),
            }
            k = end;
        }
        out
    }

    fn emit_wrapped(&mut self, steps: &[Op], original: &[Atom], revised: &[Atom]) -> Vec<Inline> {
        let class = |step: &Op| match step {
            Op::Equal(..) => None,
            Op::Insert(j) if revised[*j].is_anchor() => None,
            Op::Insert(_) => Some(RevisionKind::Insert),
            Op::Delete(_) => Some(RevisionKind::Delete),
        };
        let mut out = Vec::new();
        let mut k = 0;
        while k < steps.len() {
            let kind = class(&steps[k]);
            let mut end = k + 1;
            while end < steps.len() && class(&steps[end]) == kind {
                end += 1;
            }
            let mut content = Vec::new();
            for step in &steps[k..end] {
                match step {
                    Op::Equal(_, j) | Op::Insert(j) => {
                        push_atom(&mut content, &revised[*j], kind.is_some())
                    }
                    Op::Delete(i) => self.push_deleted_atom(&mut content, &original[*i]),
                }
            }
            match kind {
                None => out.extend(content),
                Some(_) if content.is_empty() => {}
                Some(kind) => out.push(self.revision(kind, content)),
            }
            k = end;
        }
        out
    }

    fn push_deleted_atom(&mut self, content: &mut Vec<Inline>, atom: &Atom) {
        match &atom.kind {
            AtomKind::Text(pieces) => {
                for (text, props) in pieces {
                    push_text(content, text, self.original_run_props(props));
                }
            }
            AtomKind::Tab(props) => content.push(Inline::Tab(self.original_run_props(props))),
            AtomKind::Break(kind, props) => {
                content.push(Inline::Break(*kind, self.original_run_props(props)))
            }
            AtomKind::Object(inline) => match deletable_object(inline)
                .filter(|inline| !self.uses_conflicting_prefix(object_xml(inline)))
            {
                Some(inline) => content.push(inline),
                None => self.skipped.push(CompareSkip::Object),
            },
            AtomKind::NoteRef(_) => self.skipped.push(CompareSkip::NoteRef),
            AtomKind::Anchor(_) => {}
        }
    }

    fn metadata(&mut self) -> (u64, String) {
        let id = self.next_id;
        self.next_id += 1;
        let mut attrs = format!(" w:id=\"{id}\" w:author=\"");
        esc_attr(&self.opts.author, &mut attrs);
        attrs.push_str("\" w:date=\"");
        esc_attr(&self.opts.date, &mut attrs);
        attrs.push('"');
        (id, attrs)
    }

    fn count(&mut self, kind: RevisionKind) {
        match kind {
            RevisionKind::Insert => self.insertions += 1,
            RevisionKind::Delete => self.deletions += 1,
        }
    }

    fn revision(&mut self, kind: RevisionKind, content: Vec<Inline>) -> Inline {
        self.count(kind);
        let (id, attrs) = self.metadata();
        Inline::Revision {
            kind,
            metadata: RevisionMetadata {
                id: Some(id.to_string()),
                author: Some(self.opts.author.clone()),
                date: Some(self.opts.date.clone()),
                ..RevisionMetadata::default()
            },
            raw: format!("<w:{}{attrs}>", revision_tag(kind)),
            content,
            content_changed: true,
        }
    }

    /// Mark a paragraph mark inserted/deleted: a `w:ins`/`w:del` first in the
    /// paragraph-mark `w:rPr` (where CT_ParaRPr puts it).
    fn mark_paragraph(&mut self, props: &mut ParProps, kind: RevisionKind) {
        self.count(kind);
        let (_, attrs) = self.metadata();
        let record = format!("<w:{}{attrs}/>", revision_tag(kind));
        match props.raw_props.iter_mut().find(|raw| {
            raw.strip_prefix("<w:rPr")
                .is_some_and(|rest| rest.starts_with(['>', ' ', '/']))
        }) {
            Some(rpr) if rpr.trim_end().ends_with("/>") && !rpr.contains("</") => {
                *rpr = format!("<w:rPr>{record}</w:rPr>");
            }
            Some(rpr) => {
                let open = rpr.find('>').map_or(rpr.len(), |at| at + 1);
                rpr.insert_str(open, &record);
            }
            None => props.raw_props.push(format!("<w:rPr>{record}</w:rPr>")),
        }
    }

    /// A `w:sectPrChange` with no prior section properties: rejecting it
    /// removes the section break it is attached to.
    fn new_section_change(&mut self) -> PropertyChange {
        let (id, attrs) = self.metadata();
        PropertyChange {
            scope: PropertyScope::Section,
            metadata: RevisionMetadata {
                id: Some(id.to_string()),
                author: Some(self.opts.author.clone()),
                date: Some(self.opts.date.clone()),
                ..RevisionMetadata::default()
            },
            raw: format!("<w:sectPrChange{attrs}/>"),
            previous: PropertySnapshot::Absent,
        }
    }

    /// Original paragraph properties made safe for the revised package: no
    /// section break (its header/footer references belong to the original),
    /// and only styles and lists the revised package defines.
    fn original_par_props(&mut self, props: &ParProps) -> ParProps {
        let mut props = props.clone();
        props.section_break = None;
        props.section_property_change = None;
        props.property_change = None;
        props.mark_revisions.clear();
        self.drop_conflicting(&mut props.raw_props);
        if props
            .style_id
            .as_ref()
            .is_some_and(|s| !self.styles.contains(s))
        {
            props.style_id = None;
            props.heading_level = None;
        }
        if props
            .num_id
            .is_some_and(|n| !self.nums.contains(&n.to_string()))
        {
            props.num_id = None;
            props.ilvl = 0;
        }
        props
    }

    fn original_run_props(&mut self, props: &RunProps) -> RunProps {
        let mut props = props.clone();
        props.property_change = None;
        props.revision_cues = Default::default();
        self.drop_conflicting(&mut props.raw_props);
        if props
            .style_id
            .as_ref()
            .is_some_and(|s| !self.styles.contains(s))
        {
            props.style_id = None;
        }
        props
    }
}

impl Compare<'_> {
    /// Whether original XML uses a prefix the result binds to another
    /// namespace.
    fn uses_conflicting_prefix(&self, xml: &str) -> bool {
        self.conflicts.iter().any(|prefix| {
            xml.contains(&format!("<{prefix}:")) || xml.contains(&format!(" {prefix}:"))
        })
    }

    /// Drop preserved original property children that use such a prefix.
    fn drop_conflicting(&mut self, raw_props: &mut Vec<String>) {
        let before = raw_props.len();
        raw_props.retain(|raw| !self.uses_conflicting_prefix(raw));
        if raw_props.len() < before && !self.dropped_conflicting {
            self.dropped_conflicting = true;
            self.skipped.push(CompareSkip::Formatting);
        }
    }
}

fn object_xml(inline: &Inline) -> &str {
    match inline {
        Inline::Field { raw, .. } | Inline::Equation { raw, .. } | Inline::Raw(raw) => raw,
        _ => "",
    }
}

fn revision_tag(kind: RevisionKind) -> &'static str {
    match kind {
        RevisionKind::Insert => "ins",
        RevisionKind::Delete => "del",
    }
}

fn inserted_item(block: &Block) -> Item<'_> {
    match block {
        Block::Paragraph(r) => Item::Inserted(r),
        other => Item::RevisedOnly(other),
    }
}

/// Runs of consecutive paragraph items, as `(start, end)` index ranges.
fn paragraph_segments(items: &[Item]) -> Vec<(usize, usize)> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (k, item) in items.iter().enumerate() {
        if !item.is_paragraph() {
            if start < k {
                segments.push((start, k));
            }
            start = k + 1;
        }
    }
    if start < items.len() {
        segments.push((start, items.len()));
    }
    segments
}

/// When a segment ends in a run of changes holding both deletions and
/// insertions, neither kind can borrow the mark before the run for the other,
/// so the run's last deletion and last insertion become one modified
/// paragraph at the segment end.
fn fix_mixed_tails(items: &mut Vec<Item>) {
    let segments = paragraph_segments(items);
    for &(start, end) in segments.iter().rev() {
        let mut tail = end;
        while tail > start && items[tail - 1].change_kind().is_some() {
            tail -= 1;
        }
        let last = |kind| {
            (tail..end)
                .rev()
                .find(|&k| items[k].change_kind() == Some(kind))
        };
        let (Some(d), Some(i)) = (last(RevisionKind::Delete), last(RevisionKind::Insert)) else {
            continue;
        };
        let (Item::Deleted(o), Item::Inserted(r)) = (&items[d], &items[i]) else {
            unreachable!()
        };
        let modified = Item::Modified(o, r);
        items.remove(d.max(i));
        items.remove(d.min(i));
        items.insert(end - 2, modified);
    }
}

fn pairable(original: &Block, revised: &Block) -> bool {
    match (original, revised) {
        (Block::Paragraph(o), Block::Paragraph(r)) => similar(o, r),
        (Block::Table(_), Block::Table(_)) => true,
        _ => false,
    }
}

/// Whether two paragraphs share at least half their words (Dice coefficient
/// over word tokens).
fn similar(original: &Paragraph, revised: &Paragraph) -> bool {
    let words = |p: &Paragraph| -> Vec<String> {
        atoms(p)
            .0
            .into_iter()
            .filter(|a| !a.is_anchor() && !a.key.trim().is_empty())
            .map(|a| a.key)
            .collect()
    };
    let (a, b) = (words(original), words(revised));
    if a.is_empty() || b.is_empty() {
        return a.is_empty() && b.is_empty();
    }
    let common = diff(&a, &b)
        .iter()
        .filter(|op| matches!(op, Op::Equal(..)))
        .count();
    2 * common * 2 >= a.len() + b.len()
}

// ---------------------------------------------------------------------------
// Atoms: a paragraph flattened into diff tokens
// ---------------------------------------------------------------------------

struct Atom {
    /// What is compared: the token text, or a normalized object key.
    key: String,
    kind: AtomKind,
    /// Index of the enclosing hyperlink in the paragraph's link table.
    link: Option<usize>,
}

enum AtomKind {
    /// A text token, as pieces of the runs it spans.
    Text(Vec<(String, RunProps)>),
    Tab(RunProps),
    Break(BreakKind, RunProps),
    /// A visible object compared as one token.
    Object(Inline),
    NoteRef(Inline),
    /// A zero-width marker (bookmark, comment range, field character, …):
    /// not compared, kept in place on the revised side.
    Anchor(Inline),
}

impl Atom {
    fn is_anchor(&self) -> bool {
        matches!(self.kind, AtomKind::Anchor(_))
    }
}

/// Flatten a paragraph into atoms, with the hyperlinks they sit in.
fn atoms(paragraph: &Paragraph) -> (Vec<Atom>, Vec<Hyperlink>) {
    let mut segments = Vec::new();
    let mut links = Vec::new();
    flatten(&paragraph.content, None, &mut links, &mut segments);
    let mut atoms = Vec::new();
    let mut k = 0;
    while k < segments.len() {
        let link = segments[k].link;
        if matches!(segments[k].kind, SegKind::Text(..)) {
            let mut end = k;
            while end < segments.len()
                && segments[end].link == link
                && matches!(segments[end].kind, SegKind::Text(..))
            {
                end += 1;
            }
            tokenize(&segments[k..end], link, &mut atoms);
            k = end;
            continue;
        }
        let segment = segments[k].clone();
        let (key, kind) = match segment.kind {
            SegKind::Text(..) => unreachable!(),
            SegKind::Tab(props) => ("\t".to_string(), AtomKind::Tab(props)),
            SegKind::Break(kind, props) => (format!("\n{kind:?}"), AtomKind::Break(kind, props)),
            SegKind::Object(key, inline) => (key, AtomKind::Object(inline)),
            SegKind::NoteRef(key, inline) => (key, AtomKind::NoteRef(inline)),
            SegKind::Anchor(inline) => (String::new(), AtomKind::Anchor(inline)),
        };
        atoms.push(Atom { key, kind, link });
        k += 1;
    }
    (atoms, links)
}

#[derive(Clone)]
struct Segment {
    kind: SegKind,
    link: Option<usize>,
}

#[derive(Clone)]
enum SegKind {
    Text(String, RunProps),
    Tab(RunProps),
    Break(BreakKind, RunProps),
    Object(String, Inline),
    NoteRef(String, Inline),
    Anchor(Inline),
}

fn flatten(
    content: &[Inline],
    link: Option<usize>,
    links: &mut Vec<Hyperlink>,
    out: &mut Vec<Segment>,
) {
    for inline in content {
        let kind = match inline {
            Inline::Run(run) => SegKind::Text(run.text.clone(), run.props.clone()),
            Inline::Hyperlink(h) => {
                let inner = link.or_else(|| {
                    links.push(h.clone());
                    Some(links.len() - 1)
                });
                let runs: Vec<Inline> = h.runs.iter().cloned().map(Inline::Run).collect();
                flatten(&runs, inner, links, out);
                flatten(&h.content, inner, links, out);
                continue;
            }
            Inline::Tab(props) => SegKind::Tab(props.clone()),
            Inline::Break(kind, props) => SegKind::Break(*kind, props.clone()),
            Inline::Field { text, .. } => SegKind::Object(format!("field:{text}"), inline.clone()),
            Inline::Equation { text, .. } => {
                SegKind::Object(format!("equation:{text}"), inline.clone())
            }
            Inline::SmartArt { raw, .. }
            | Inline::Chart { raw, .. }
            | Inline::TextBox { raw, .. } => SegKind::Object(
                format!("object:{}", blank_relationships(raw)),
                inline.clone(),
            ),
            Inline::FootnoteRef { endnote, .. } => {
                SegKind::NoteRef(format!("note:{endnote}"), inline.clone())
            }
            Inline::Raw(raw) if visible_raw(raw) => {
                SegKind::Object(format!("raw:{}", blank_relationships(raw)), inline.clone())
            }
            Inline::Raw(_) | Inline::Revision { .. } | Inline::UnsupportedRevision { .. } => {
                SegKind::Anchor(inline.clone())
            }
        };
        out.push(Segment { kind, link });
    }
}

/// Raw inline XML that shows something (a picture, an embedded object) rather
/// than a zero-width marker.
fn visible_raw(raw: &str) -> bool {
    [
        "<w:drawing",
        "<w:pict",
        "<w:object",
        "<mc:AlternateContent",
        "<w:ruby",
    ]
    .iter()
    .any(|tag| raw.contains(tag))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// Split consecutive text segments into tokens; a token may span runs, and
/// keeps one piece per run it touches.
fn tokenize(segments: &[Segment], link: Option<usize>, out: &mut Vec<Atom>) {
    let mut key = String::new();
    let mut pieces: Vec<(String, RunProps)> = Vec::new();
    // The segment the last piece came from.
    let mut piece_segment = None;
    let mut class = None;
    for (index, segment) in segments.iter().enumerate() {
        let SegKind::Text(text, props) = &segment.kind else {
            continue;
        };
        for c in text.chars() {
            let this = char_class(c);
            if (class != Some(this) || this == CharClass::Other) && !key.is_empty() {
                out.push(Atom {
                    key: std::mem::take(&mut key),
                    kind: AtomKind::Text(std::mem::take(&mut pieces)),
                    link,
                });
                piece_segment = None;
            }
            class = Some(this);
            key.push(c);
            match pieces.last_mut() {
                Some((piece, _)) if piece_segment == Some(index) => piece.push(c),
                _ => {
                    pieces.push((c.to_string(), props.clone()));
                    piece_segment = Some(index);
                }
            }
        }
    }
    if !key.is_empty() {
        out.push(Atom {
            key,
            kind: AtomKind::Text(pieces),
            link,
        });
    }
}

/// Append a revised atom; `in_revision` when it goes inside a `w:ins`.
fn push_atom(content: &mut Vec<Inline>, atom: &Atom, in_revision: bool) {
    match &atom.kind {
        AtomKind::Object(Inline::Field { raw, text }) if in_revision => {
            content.push(Inline::Field {
                raw: run_level_field(raw),
                text: text.clone(),
            })
        }
        AtomKind::Text(pieces) => {
            for (text, props) in pieces {
                push_text(content, text, props.clone());
            }
        }
        AtomKind::Tab(props) => content.push(Inline::Tab(props.clone())),
        AtomKind::Break(kind, props) => content.push(Inline::Break(*kind, props.clone())),
        AtomKind::Object(inline) | AtomKind::NoteRef(inline) | AtomKind::Anchor(inline) => {
            content.push(inline.clone())
        }
    }
}

/// Append text, extending the previous run when it has the same formatting.
fn push_text(content: &mut Vec<Inline>, text: &str, props: RunProps) {
    if let Some(Inline::Run(run)) = content.last_mut() {
        if run.props == props {
            run.text.push_str(text);
            return;
        }
    }
    content.push(Inline::Run(Run {
        text: text.to_string(),
        props,
    }));
}

/// A deleted original object that can live in the revised package, with its
/// text elements turned into their deleted forms; `None` when it references
/// a relationship of the original package or cannot be carried.
fn deletable_object(inline: &Inline) -> Option<Inline> {
    let carry = |raw: &str| (!has_relationship(raw)).then(|| deleted_text_xml(raw));
    match inline {
        Inline::Field { raw, text } => Some(Inline::Field {
            raw: carry(&run_level_field(raw))?,
            text: text.clone(),
        }),
        Inline::Equation { raw, .. } if !has_relationship(raw) => Some(inline.clone()),
        Inline::Raw(raw) => Some(Inline::Raw(carry(raw)?)),
        _ => None,
    }
}

/// A field as run-level XML, which `w:ins`/`w:del` may contain: a
/// `w:fldSimple` (paragraph-level content) becomes the equivalent complex
/// field — begin, instruction, separate, its result runs, end. Other field
/// forms (complex fields, `w:sym` runs) are already runs.
fn run_level_field(raw: &str) -> String {
    let mut parser = XmlParser::new(raw);
    if parser.next() != Event::Start || parser.name() != "w:fldSimple" {
        return raw.to_string();
    }
    let mut instr = String::new();
    XmlParser::append_decoded(parser.attr("w:instr"), &mut instr);
    let open_end = parser.pos();
    let self_closing = raw[..open_end].trim_end().ends_with("/>");
    let result = if self_closing {
        ""
    } else {
        let close = raw.rfind("</w:fldSimple>").unwrap_or(raw.len());
        &raw[open_end.min(close)..close]
    };
    let mut out = String::from(
        "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\">",
    );
    esc_attr(&instr, &mut out);
    out.push_str("</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>");
    out.push_str(result);
    out.push_str("<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>");
    out
}

fn has_relationship(raw: &str) -> bool {
    ["r:id=\"", "r:embed=\"", "r:link=\"", "r:dm=\"", "r:pict=\""]
        .iter()
        .any(|attr| raw.contains(attr))
}

fn deleted_text_xml(raw: &str) -> String {
    raw.replace("<w:t>", "<w:delText>")
        .replace("<w:t ", "<w:delText ")
        .replace("</w:t>", "</w:delText>")
        .replace("<w:instrText", "<w:delInstrText")
        .replace("</w:instrText>", "</w:delInstrText>")
}

/// `raw` with the values of relationship attributes (`r:id`, `r:embed`, …)
/// blanked, so the same picture compares equal although each package numbers
/// its relationships.
fn blank_relationships(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find(" r:") {
        let name_start = at + 3;
        let name_len = rest[name_start..]
            .bytes()
            .take_while(u8::is_ascii_alphabetic)
            .count();
        let value_start = name_start + name_len + 2;
        let is_attr = name_len > 0 && rest[name_start + name_len..].starts_with("=\"");
        let close = is_attr.then(|| rest[value_start..].find('"')).flatten();
        match close {
            Some(close) => {
                out.push_str(&rest[..value_start]);
                out.push('"');
                rest = &rest[value_start + close + 1..];
            }
            None => {
                out.push_str(&rest[..name_start]);
                rest = &rest[name_start..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Merge diff steps with the revised paragraph's anchors: each anchor is
/// emitted (unmarked) just before the revised atom that follows it, the rest
/// at the end.
fn with_anchors(ops: impl Iterator<Item = Op>, revised: &[Atom]) -> Vec<Op> {
    let mut out = Vec::new();
    let mut next = 0;
    for op in ops {
        if let Op::Equal(_, j) | Op::Insert(j) = op {
            while next < j {
                if revised[next].is_anchor() {
                    out.push(Op::Insert(next));
                }
                next += 1;
            }
            next = j + 1;
        }
        out.push(op);
    }
    for (j, atom) in revised.iter().enumerate().skip(next) {
        if atom.is_anchor() {
            out.push(Op::Insert(j));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Block keys
// ---------------------------------------------------------------------------

fn paragraph_key(paragraph: &Paragraph) -> String {
    let mut key = String::from("p");
    for atom in atoms(paragraph).0.iter().filter(|a| !a.is_anchor()) {
        key.push('\u{1}');
        key.push_str(&atom.key);
    }
    key
}

fn block_key(block: &Block) -> String {
    match block {
        Block::Paragraph(p) => paragraph_key(p),
        Block::Table(table) => {
            let mut key = String::from("t");
            for row in &table.rows {
                key.push('\u{2}');
                for cell in &row.cells {
                    key.push('\u{3}');
                    for block in &cell.blocks {
                        key.push('\u{4}');
                        key.push_str(&block_key(block));
                    }
                }
            }
            key
        }
        Block::Raw(raw) => format!("r{}", blank_relationships(raw)),
        Block::SectionProperties(_) => "s".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Myers diff
// ---------------------------------------------------------------------------

/// One step of an edit script: `Equal(i, j)` keeps `a[i] == b[j]`,
/// `Delete(i)` drops `a[i]`, `Insert(j)` adds `b[j]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Edit distance beyond which the middle of a diff is treated as wholly
/// replaced (memory for the trace grows with its square).
const MAX_EDIT_DISTANCE: usize = 2000;

/// A shortest edit script from `a` to `b` (Myers' O(ND) algorithm), with
/// deletions before insertions in each changed stretch.
fn diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Op> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut ops: Vec<Op> = (0..prefix).map(|i| Op::Equal(i, i)).collect();
    for op in myers(a_mid, b_mid) {
        ops.push(match op {
            Op::Equal(i, j) => Op::Equal(i + prefix, j + prefix),
            Op::Delete(i) => Op::Delete(i + prefix),
            Op::Insert(j) => Op::Insert(j + prefix),
        });
    }
    ops.extend((0..suffix).map(|k| Op::Equal(a.len() - suffix + k, b.len() - suffix + k)));
    normalize_changes(ops)
}

fn myers<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let replace_all = || {
        (0..a.len())
            .map(Op::Delete)
            .chain((0..b.len()).map(Op::Insert))
            .collect::<Vec<_>>()
    };
    if n == 0 || m == 0 {
        return replace_all();
    }
    let max = (n + m) as usize;
    let offset = max as isize;
    let mut v = vec![0isize; 2 * max + 1];
    // trace[d] = v[-d..=d] after step d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=max as isize {
        if d as usize > MAX_EDIT_DISTANCE {
            return replace_all();
        }
        let mut k = -d;
        while k <= d {
            let at = |k: isize| (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                trace.push(v[at(-d)..=at(d)].to_vec());
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
        trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
    }
    let Some(depth) = found else {
        return replace_all();
    };

    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=depth).rev() {
        let previous = &trace[d as usize - 1];
        let get = |k: isize| previous[(k + d - 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Equal(x as usize, y as usize));
        }
        if x == prev_x {
            ops.push(Op::Insert(prev_y as usize));
        } else {
            ops.push(Op::Delete(prev_x as usize));
        }
        x = prev_x;
        y = prev_y;
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(Op::Equal(x as usize, y as usize));
    }
    ops.reverse();
    ops
}

/// Within each stretch between equal steps, put deletions before insertions.
fn normalize_changes(ops: Vec<Op>) -> Vec<Op> {
    let mut out = Vec::with_capacity(ops.len());
    let mut deletes = Vec::new();
    let mut inserts = Vec::new();
    for op in ops {
        match op {
            Op::Equal(..) => {
                out.append(&mut deletes);
                out.append(&mut inserts);
                out.push(op);
            }
            Op::Delete(_) => deletes.push(op),
            Op::Insert(_) => inserts.push(op),
        }
    }
    out.append(&mut deletes);
    out.append(&mut inserts);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(a: &str, b: &str) -> String {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        diff(&a, &b)
            .into_iter()
            .map(|op| match op {
                Op::Equal(i, _) => format!("={}", a[i]),
                Op::Delete(i) => format!("-{}", a[i]),
                Op::Insert(j) => format!("+{}", b[j]),
            })
            .collect()
    }

    #[test]
    fn myers_finds_a_shortest_script() {
        assert_eq!(script("abc", "abc"), "=a=b=c");
        assert_eq!(script("", "ab"), "+a+b");
        assert_eq!(script("ab", ""), "-a-b");
        assert_eq!(script("abcabba", "cbabac").matches(['-', '+']).count(), 5);
        assert_eq!(script("kitten", "sitting"), "-k+s=i=t=t-e+i=n+g");
    }

    #[test]
    fn diff_applies_back_to_the_target() {
        for (a, b) in [
            ("the cat sat", "the black cat sat"),
            ("xyz", "abc"),
            ("aaaa", "aa"),
            ("abcdefg", "gfedcba"),
        ] {
            let (av, bv): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
            let mut from_a = String::new();
            let mut to_b = String::new();
            for op in diff(&av, &bv) {
                match op {
                    Op::Equal(i, j) => {
                        assert_eq!(av[i], bv[j]);
                        from_a.push(av[i]);
                        to_b.push(bv[j]);
                    }
                    Op::Delete(i) => from_a.push(av[i]),
                    Op::Insert(j) => to_b.push(bv[j]),
                }
            }
            assert_eq!((from_a.as_str(), to_b.as_str()), (a, b));
        }
    }

    #[test]
    fn relationship_values_are_blanked_for_comparison() {
        assert_eq!(
            blank_relationships("<a:blip r:embed=\"rId5\" x=\"1\"/>"),
            blank_relationships("<a:blip r:embed=\"rId9\" x=\"1\"/>")
        );
        assert_ne!(
            blank_relationships("<a:blip r:embed=\"rId5\" x=\"1\"/>"),
            blank_relationships("<a:blip r:embed=\"rId5\" x=\"2\"/>")
        );
    }

    #[test]
    fn tokens_split_words_spaces_and_punctuation_across_runs() {
        let paragraph = Paragraph {
            props: ParProps::default(),
            content: vec![
                Inline::Run(Run {
                    text: "ca".into(),
                    props: RunProps::default(),
                }),
                Inline::Run(Run {
                    text: "t, sat".into(),
                    props: RunProps {
                        bold: true,
                        ..RunProps::default()
                    },
                }),
            ],
        };
        let (atoms, _) = atoms(&paragraph);
        let keys: Vec<&str> = atoms.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["cat", ",", " ", "sat"]);
        let AtomKind::Text(pieces) = &atoms[0].kind else {
            panic!("text")
        };
        assert_eq!(pieces.len(), 2, "'ca' plain + 't' bold");
        assert!(!pieces[0].1.bold && pieces[1].1.bold);
    }
}
