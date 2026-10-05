//! Pure tracked-change review operations over the document model.

use crate::model::{
    Block, Cell, Document, Inline, ParProps, PropertyScope, PropertySnapshot, PropertyState,
    RevisionAddress, RevisionCategory, RevisionDisplayCues, RevisionKind, RevisionTarget, Row,
    RunProps, SectionProperties, UnsupportedRevisionKind, VMerge,
};
use crate::xml::{Event, XmlParser};

/// The operation requested for a tracked revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionAction {
    Accept,
    Reject,
}

/// Why a modeled property revision could not be transformed safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MalformedRevisionReason {
    PropertyScopeMismatch {
        expected: PropertyScope,
        actual: PropertyScope,
    },
    PropertySnapshot {
        scope: PropertyScope,
    },
    PropertySnapshotScopeMismatch {
        expected: PropertyScope,
        actual: PropertyScope,
    },
}

/// Explicit result of one accept/reject attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionOutcome {
    Applied {
        target: RevisionTarget,
        action: RevisionAction,
        category: RevisionCategory,
    },
    Stale {
        target: RevisionTarget,
        action: RevisionAction,
    },
    Unsupported {
        target: RevisionTarget,
        action: RevisionAction,
        kind: UnsupportedRevisionKind,
    },
    Malformed {
        target: RevisionTarget,
        action: RevisionAction,
        reason: MalformedRevisionReason,
    },
}

impl RevisionOutcome {
    pub fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }
}

type TransformResult = Result<RevisionCategory, MalformedRevisionReason>;

impl Document {
    /// Accept one current revision by its stable document-local identity.
    pub fn accept_revision(&mut self, target: RevisionTarget) -> RevisionOutcome {
        self.apply_revision_action(target, RevisionAction::Accept)
    }

    /// Reject one current revision by its stable document-local identity.
    pub fn reject_revision(&mut self, target: RevisionTarget) -> RevisionOutcome {
        self.apply_revision_action(target, RevisionAction::Reject)
    }

    /// Accept every revision that exists when the operation begins.
    ///
    /// Nested revisions are transformed in post-order (innermost first), while
    /// returned outcomes retain the original document order.
    pub fn accept_all_revisions(&mut self) -> Vec<RevisionOutcome> {
        self.apply_all_revision_actions(RevisionAction::Accept)
    }

    /// Reject every revision that exists when the operation begins.
    ///
    /// Nested revisions are transformed in post-order (innermost first), while
    /// returned outcomes retain the original document order.
    pub fn reject_all_revisions(&mut self) -> Vec<RevisionOutcome> {
        self.apply_all_revision_actions(RevisionAction::Reject)
    }

    pub fn apply_revision_action(
        &mut self,
        target: RevisionTarget,
        action: RevisionAction,
    ) -> RevisionOutcome {
        self.initialize_revision_targets();
        let addresses = self.revisions();
        let Some(address) = addresses.iter().find(|address| address.target == target) else {
            return RevisionOutcome::Stale { target, action };
        };
        if let RevisionCategory::Unsupported(kind) = &address.category {
            return RevisionOutcome::Unsupported {
                target,
                action,
                kind: kind.clone(),
            };
        }
        let discards_content = matches!(
            (&address.category, action),
            (
                RevisionCategory::Inline(RevisionKind::Insert),
                RevisionAction::Reject
            ) | (
                RevisionCategory::Inline(RevisionKind::Delete),
                RevisionAction::Accept
            )
        );
        if discards_content
            && let Some(kind) = addresses
                .iter()
                .skip(address.ordinal + 1)
                .take_while(|candidate| candidate.depth > address.depth)
                .find_map(|candidate| match &candidate.category {
                    RevisionCategory::Unsupported(kind) => Some(kind.clone()),
                    _ => None,
                })
        {
            return RevisionOutcome::Unsupported {
                target,
                action,
                kind,
            };
        }

        // A tracked insertion recorded on live runs (TrackedInsert) may be on
        // runs in several paragraphs: act on every one of them.
        if sweep_tracked_inserts(&mut self.body, target, action) > 0 {
            self.initialize_revision_targets();
            return RevisionOutcome::Applied {
                target,
                action,
                category: RevisionCategory::Inline(RevisionKind::Insert),
            };
        }

        match transform_blocks(&mut self.body, target, action) {
            Some(Ok(category)) => {
                // A rejected property snapshot can expose previously nested
                // revision data. Assign identities without renumbering survivors.
                self.initialize_revision_targets();
                RevisionOutcome::Applied {
                    target,
                    action,
                    category,
                }
            }
            Some(Err(reason)) => RevisionOutcome::Malformed {
                target,
                action,
                reason,
            },
            None => RevisionOutcome::Stale { target, action },
        }
    }

    fn apply_all_revision_actions(&mut self, action: RevisionAction) -> Vec<RevisionOutcome> {
        self.initialize_revision_targets();
        let addresses = self.revisions();
        let targets = revision_postorder(&addresses, action);
        let mut outcomes = targets
            .into_iter()
            .map(|(ordinal, target)| (ordinal, self.apply_revision_action(target, action)))
            .collect::<Vec<_>>();
        outcomes.sort_by_key(|(ordinal, _)| *ordinal);
        outcomes.into_iter().map(|(_, outcome)| outcome).collect()
    }
}

fn revision_postorder(
    addresses: &[RevisionAddress],
    action: RevisionAction,
) -> Vec<(usize, RevisionTarget)> {
    // `Document::revisions` is pre-order, so reversing it guarantees every
    // descendant is transformed before its ancestor. Original ordinals are
    // retained and used to restore report order after the actions run.
    //
    // Paragraph-mark records are the exception: removing a mark merges the
    // next paragraph in and replaces this paragraph's properties, so every
    // other record of the paragraph (its pPrChange/sectPrChange, a second
    // mark record that keeps the mark) must act first or it would come back
    // stale. Marks are therefore deferred until just before the next record
    // that encloses them (a shallower wrapper, such as a revision around a
    // text box), or the end. Within a deferred batch the records that keep a
    // mark go first; keeping changes no structure, and a merge keeps the
    // absorbed paragraph's records, so the order across paragraphs is free.
    fn flush<'a>(
        pending: &mut Vec<&'a RevisionAddress>,
        order: &mut Vec<&'a RevisionAddress>,
        above: Option<usize>,
        action: RevisionAction,
    ) {
        // `above: None` flushes every pending mark.
        let (mut ready, rest): (Vec<_>, Vec<_>) = pending
            .drain(..)
            .partition(|mark| above.is_none_or(|depth| mark.depth > depth));
        ready.sort_by_key(|mark| match mark.category {
            RevisionCategory::ParagraphMark(kind) => removes_mark(action, kind),
            _ => false,
        });
        order.extend(ready);
        *pending = rest;
    }
    let mut order: Vec<&RevisionAddress> = Vec::with_capacity(addresses.len());
    let mut pending: Vec<&RevisionAddress> = Vec::new();
    for address in addresses.iter().rev() {
        if matches!(address.category, RevisionCategory::ParagraphMark(_)) {
            pending.push(address);
            continue;
        }
        flush(&mut pending, &mut order, Some(address.depth), action);
        order.push(address);
    }
    flush(&mut pending, &mut order, None, action);
    order
        .into_iter()
        .map(|address| (address.ordinal, address.target))
        .collect()
}

/// Whether acting on a paragraph-mark record removes the mark (merging the
/// paragraph with the next one) rather than keeping it.
fn removes_mark(action: RevisionAction, kind: RevisionKind) -> bool {
    matches!(
        (action, kind),
        (RevisionAction::Accept, RevisionKind::Delete)
            | (RevisionAction::Reject, RevisionKind::Insert)
    )
}

/// Accept (keep the text, drop the record and its underline cue) or reject
/// (remove the text) every run recorded as tracked insertion `target`,
/// wherever it is. How many inlines it acted on.
fn sweep_tracked_inserts(
    blocks: &mut [Block],
    target: RevisionTarget,
    action: RevisionAction,
) -> usize {
    let mut n = 0;
    for block in blocks {
        match block {
            Block::Paragraph(p) => n += sweep_inlines(&mut p.content, target, action),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        n += sweep_tracked_inserts(&mut cell.blocks, target, action);
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
    n
}

fn sweep_inlines(
    content: &mut Vec<Inline>,
    target: RevisionTarget,
    action: RevisionAction,
) -> usize {
    let is_target = |inline: &Inline| {
        let props = match inline {
            Inline::Run(r) => &r.props,
            Inline::Tab(props) | Inline::Break(_, props) => props,
            _ => return false,
        };
        props
            .tracked_insert
            .as_ref()
            .is_some_and(|t| t.metadata.target == target)
    };
    let mut n = 0;
    if action == RevisionAction::Reject {
        let before = content.len();
        content.retain(|i| !is_target(i));
        n += before - content.len();
    } else {
        for inline in content.iter_mut().filter(|i| is_target(i)) {
            strip_revision_cue(inline, RevisionKind::Insert, false);
            match inline {
                Inline::Run(r) => r.props.tracked_insert = None,
                Inline::Tab(props) | Inline::Break(_, props) => props.tracked_insert = None,
                _ => {}
            }
            n += 1;
        }
    }
    for inline in content.iter_mut() {
        match inline {
            Inline::Revision { content, .. } => n += sweep_inlines(content, target, action),
            Inline::Hyperlink(h) => n += sweep_inlines(&mut h.content, target, action),
            Inline::TextBox { blocks, .. } => n += sweep_tracked_inserts(blocks, target, action),
            _ => {}
        }
    }
    n
}

fn transform_blocks(
    blocks: &mut Vec<Block>,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    for index in 0..blocks.len() {
        if let Some(result) = transform_paragraph_mark(blocks, index, target, action) {
            return Some(result);
        }
        let result = match &mut blocks[index] {
            Block::Paragraph(paragraph) => {
                transform_section_props(&mut paragraph.props, target, action)
                    .or_else(|| transform_par_props(&mut paragraph.props, target, action))
                    .or_else(|| transform_inlines(&mut paragraph.content, target, action))
            }
            Block::Table(table) => {
                if let Some(result) = transform_table_props(
                    &mut table.raw_tblpr,
                    &mut table.property_change,
                    target,
                    action,
                ) {
                    Some(result)
                } else {
                    let mut found = None;
                    for row in &mut table.rows {
                        found = transform_row(row, target, action);
                        if found.is_some() {
                            break;
                        }
                    }
                    found
                }
            }
            Block::SectionProperties(section) => {
                transform_trailing_section_props(section, target, action)
            }
            Block::Raw(_) => None,
        };
        if result.is_some() {
            return result;
        }
    }
    None
}

/// Act on the paragraph mark of `blocks[index]` when it is `target`.
///
/// Keeping the mark (accepting an insertion, rejecting a deletion) only drops
/// the revision record. Removing it (accepting a deletion, rejecting an
/// insertion) merges the next paragraph of the same container into this one, as
/// Word does: this paragraph's content, then the next paragraph's content, with
/// the next paragraph's properties (its mark is the one that survives). When no
/// paragraph follows in the container (it is the last block, or a table or
/// other block comes next) there is nothing to merge with, so the record is
/// dropped and the paragraph stays.
fn transform_paragraph_mark(
    blocks: &mut Vec<Block>,
    index: usize,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let Block::Paragraph(paragraph) = &mut blocks[index] else {
        return None;
    };
    let position = paragraph
        .props
        .mark_revisions
        .iter()
        .position(|mark| mark.metadata.target == target)?;
    let kind = paragraph.props.mark_revisions[position].kind;
    clear_mark_revision(&mut paragraph.props, position);
    if removes_mark(action, kind) && matches!(blocks.get(index + 1), Some(Block::Paragraph(_))) {
        let Block::Paragraph(next) = blocks.remove(index + 1) else {
            unreachable!()
        };
        let Block::Paragraph(paragraph) = &mut blocks[index] else {
            unreachable!()
        };
        paragraph.content.extend(next.content);
        paragraph.props = next.props;
    }
    Some(Ok(RevisionCategory::ParagraphMark(kind)))
}

/// Drop one of a paragraph's mark records (`index` into `mark_revisions`): the
/// model entry and the matching `w:ins`/`w:del` child of the verbatim
/// paragraph-mark `w:rPr`, which is removed when that leaves it empty. Both
/// lists are in source order, so the n-th record is the n-th such child.
fn clear_mark_revision(props: &mut ParProps, index: usize) {
    props.mark_revisions.remove(index);
    let Some(position) = props
        .raw_props
        .iter()
        .position(|raw| local_name(raw) == "rPr")
    else {
        return;
    };
    match remove_mark_revision_child(&props.raw_props[position], index) {
        Some(rpr) => props.raw_props[position] = rpr,
        None => {
            props.raw_props.remove(position);
        }
    }
}

/// Drop every paragraph-mark record of `props` (model and raw XML).
pub(crate) fn clear_mark_revisions(props: &mut ParProps) {
    while !props.mark_revisions.is_empty() {
        clear_mark_revision(props, 0);
    }
}

/// Give `kept` the paragraph-mark records of `gone` (model and raw XML),
/// replacing its own: when two paragraphs are joined, the surviving physical
/// mark is the later paragraph's, so its records survive and the earlier
/// paragraph's go.
pub(crate) fn adopt_mark_revisions(kept: &mut ParProps, gone: &ParProps) {
    clear_mark_revisions(kept);
    let records = mark_revision_xml(gone);
    if records.is_empty() {
        return;
    }
    kept.mark_revisions = gone.mark_revisions.clone();
    insert_mark_records(kept, &records.concat());
}

/// Put raw `w:ins`/`w:del` record XML first in the paragraph-mark `w:rPr`
/// (where CT_ParaRPr puts it), creating the rPr when there is none. Only the
/// raw XML changes; callers keep `mark_revisions` in step (or reload).
pub(crate) fn insert_mark_records(props: &mut ParProps, records: &str) {
    match props
        .raw_props
        .iter_mut()
        .find(|raw| local_name(raw) == "rPr")
    {
        Some(rpr) if rpr.trim_end().ends_with("/>") && !rpr.contains("</") => {
            *rpr = format!("<w:rPr>{records}</w:rPr>");
        }
        Some(rpr) => {
            let open = rpr.find('>').map_or(rpr.len(), |at| at + 1);
            rpr.insert_str(open, records);
        }
        None => props.raw_props.push(format!("<w:rPr>{records}</w:rPr>")),
    }
}

/// The raw `w:ins`/`w:del` children of a paragraph-mark `w:rPr`, in order.
fn mark_revision_xml(props: &ParProps) -> Vec<String> {
    let Some(rpr) = props.raw_props.iter().find(|raw| local_name(raw) == "rPr") else {
        return Vec::new();
    };
    let mut parser = XmlParser::new(rpr);
    let mut out = Vec::new();
    if parser.next() != Event::Start {
        return out;
    }
    loop {
        match parser.next() {
            Event::Start => {
                let start = parser.start_pos();
                let record = matches!(parser.name(), "w:ins" | "w:del");
                parser.skip_element();
                if record {
                    out.push(parser.raw_slice(start, parser.pos()).to_string());
                }
            }
            Event::End | Event::Eof => return out,
            Event::Text => {}
        }
    }
}

/// `rpr` without its `index`-th `w:ins`/`w:del` child, or `None` when nothing
/// remains.
fn remove_mark_revision_child(rpr: &str, index: usize) -> Option<String> {
    let mut parser = XmlParser::new(rpr);
    if parser.next() != Event::Start {
        return Some(rpr.to_string());
    }
    let open_end = parser.pos();
    let mut kept = String::new();
    let mut seen = 0;
    loop {
        match parser.next() {
            Event::Start => {
                let start = parser.start_pos();
                let drop = matches!(parser.name(), "w:ins" | "w:del") && {
                    seen += 1;
                    seen - 1 == index
                };
                parser.skip_element();
                if !drop {
                    kept.push_str(parser.raw_slice(start, parser.pos()));
                }
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    if kept.is_empty() {
        return None;
    }
    let open = &rpr[..open_end];
    let open = open
        .strip_suffix("/>")
        .map_or(open.to_string(), |o| format!("{o}>"));
    Some(format!("{open}{kept}</w:rPr>"))
}

fn transform_row(
    row: &mut Row,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    if let Some(result) = transform_row_props(row, target, action) {
        return Some(result);
    }
    for cell in &mut row.cells {
        if let Some(result) = transform_cell(cell, target, action) {
            return Some(result);
        }
    }
    None
}

fn transform_cell(
    cell: &mut Cell,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    transform_cell_props(cell, target, action)
        .or_else(|| transform_blocks(&mut cell.blocks, target, action))
}

fn transform_inlines(
    content: &mut Vec<Inline>,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let mut index = 0;
    while index < content.len() {
        let direct_kind = match &content[index] {
            Inline::Revision { kind, metadata, .. } if metadata.target == target => Some(*kind),
            _ => None,
        };
        if let Some(kind) = direct_kind {
            let Inline::Revision {
                content: mut inner, ..
            } = content.remove(index)
            else {
                unreachable!()
            };
            let unwrap = matches!(
                (action, kind),
                (RevisionAction::Accept, RevisionKind::Insert)
                    | (RevisionAction::Reject, RevisionKind::Delete)
            );
            if unwrap {
                for inline in &mut inner {
                    strip_revision_cue(inline, kind, kind == RevisionKind::Delete);
                }
                content.splice(index..index, inner);
            }
            return Some(Ok(RevisionCategory::Inline(kind)));
        }

        let result = transform_inline(&mut content[index], target, action);
        if result.is_some() {
            return result;
        }
        index += 1;
    }
    None
}

fn transform_inline(
    inline: &mut Inline,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    match inline {
        Inline::Run(run) => transform_run_props(&mut run.props, target, action),
        Inline::Hyperlink(link) => {
            for run in &mut link.runs {
                if let Some(result) = transform_run_props(&mut run.props, target, action) {
                    return Some(result);
                }
            }
            let result = transform_inlines(&mut link.content, target, action);
            if matches!(result, Some(Ok(_))) {
                link.content_changed = true;
            }
            result
        }
        Inline::Tab(props) | Inline::Break(_, props) => transform_run_props(props, target, action),
        Inline::TextBox { blocks, .. } => transform_blocks(blocks, target, action),
        Inline::Revision {
            content,
            content_changed,
            ..
        } => {
            let result = transform_inlines(content, target, action);
            if matches!(result, Some(Ok(_))) {
                *content_changed = true;
            }
            result
        }
        Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::Field { .. }
        | Inline::UnsupportedRevision { .. }
        | Inline::FootnoteRef { .. }
        | Inline::Raw(_) => None,
    }
}

fn transform_run_props(
    props: &mut RunProps,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = props.property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::Run {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::Run,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        props.property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::Run)));
    }

    let previous = change.previous.clone();
    let mut restored = match previous {
        PropertySnapshot::Absent => RunProps::default(),
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::Run,
            }));
        }
        PropertySnapshot::Present(PropertyState::Run(previous)) => *previous,
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::Run,
                    actual: other.scope(),
                },
            ));
        }
    };
    overlay_revision_cues(&mut restored, props.revision_cues);
    restored.property_change = None;
    *props = restored;
    Some(Ok(RevisionCategory::Property(PropertyScope::Run)))
}

fn transform_par_props(
    props: &mut ParProps,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = props.property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::Paragraph {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::Paragraph,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        props.property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::Paragraph)));
    }

    let previous = change.previous.clone();
    let mut restored = match previous {
        PropertySnapshot::Absent => ParProps::default(),
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::Paragraph,
            }));
        }
        PropertySnapshot::Present(PropertyState::Paragraph(previous)) => *previous,
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::Paragraph,
                    actual: other.scope(),
                },
            ));
        }
    };
    // Section properties are a separately reviewable scope even though sectPr
    // is physically nested in pPr. A paragraph-property action must not consume
    // or replace that independent revision record.
    restored.section_break = props.section_break.take();
    restored.section_property_change = props.section_property_change.take();
    // Nor is the paragraph mark's own rPr (CT_PPrBase has none): keep it, and
    // any tracked insertion/deletion of the mark it carries.
    restored.raw_props.retain(|raw| local_name(raw) != "rPr");
    restored.raw_props.extend(
        props
            .raw_props
            .iter()
            .filter(|raw| local_name(raw) == "rPr")
            .cloned(),
    );
    restored.mark_revisions = std::mem::take(&mut props.mark_revisions);
    restored.property_change = None;
    *props = restored;
    Some(Ok(RevisionCategory::Property(PropertyScope::Paragraph)))
}

fn transform_section_props(
    props: &mut ParProps,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = props.section_property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::Section {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::Section,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        props.section_property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::Section)));
    }

    let previous = change.previous.clone();
    props.section_break = match previous {
        PropertySnapshot::Absent => None,
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::Section,
            }));
        }
        PropertySnapshot::Present(PropertyState::Section(previous)) => Some(previous),
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::Section,
                    actual: other.scope(),
                },
            ));
        }
    };
    props.section_property_change = None;
    Some(Ok(RevisionCategory::Property(PropertyScope::Section)))
}

fn transform_trailing_section_props(
    section: &mut SectionProperties,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = section.property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::Section {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::Section,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        section.property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::Section)));
    }

    section.raw = match change.previous.clone() {
        // The body must retain a final section-property container even when
        // the previous state had no explicit properties.
        PropertySnapshot::Absent => "<w:sectPr/>".to_string(),
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::Section,
            }));
        }
        PropertySnapshot::Present(PropertyState::Section(previous)) => previous,
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::Section,
                    actual: other.scope(),
                },
            ));
        }
    };
    section.property_change = None;
    Some(Ok(RevisionCategory::Property(PropertyScope::Section)))
}

fn transform_table_props(
    raw_props: &mut Option<String>,
    property_change: &mut Option<crate::model::PropertyChange>,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::Table {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::Table,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        *property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::Table)));
    }
    *raw_props = match change.previous.clone() {
        PropertySnapshot::Absent => None,
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::Table,
            }));
        }
        PropertySnapshot::Present(PropertyState::Table(previous)) => Some(previous),
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::Table,
                    actual: other.scope(),
                },
            ));
        }
    };
    *property_change = None;
    Some(Ok(RevisionCategory::Property(PropertyScope::Table)))
}

fn transform_row_props(
    row: &mut Row,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = row.property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::TableRow {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::TableRow,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        row.property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::TableRow)));
    }
    let previous = match change.previous.clone() {
        PropertySnapshot::Absent => None,
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::TableRow,
            }));
        }
        PropertySnapshot::Present(PropertyState::TableRow(previous)) => Some(previous),
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::TableRow,
                    actual: other.scope(),
                },
            ));
        }
    };
    replace_row_property_xml(&mut row.raw_props, previous);
    row.property_change = None;
    Some(Ok(RevisionCategory::Property(PropertyScope::TableRow)))
}

fn transform_cell_props(
    cell: &mut Cell,
    target: RevisionTarget,
    action: RevisionAction,
) -> Option<TransformResult> {
    let change = cell.property_change.as_ref()?;
    if change.metadata.target != target {
        return None;
    }
    if change.scope != PropertyScope::TableCell {
        return Some(Err(MalformedRevisionReason::PropertyScopeMismatch {
            expected: PropertyScope::TableCell,
            actual: change.scope,
        }));
    }
    if action == RevisionAction::Accept {
        cell.property_change = None;
        return Some(Ok(RevisionCategory::Property(PropertyScope::TableCell)));
    }
    let previous = match change.previous.clone() {
        PropertySnapshot::Absent => None,
        PropertySnapshot::Malformed(_) => {
            return Some(Err(MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::TableCell,
            }));
        }
        PropertySnapshot::Present(PropertyState::TableCell(previous)) => Some(previous),
        PropertySnapshot::Present(other) => {
            return Some(Err(
                MalformedRevisionReason::PropertySnapshotScopeMismatch {
                    expected: PropertyScope::TableCell,
                    actual: other.scope(),
                },
            ));
        }
    };
    let unsupported_xml = cell_unsupported_revision_xml(cell);
    apply_cell_property_xml(cell, merge_cell_revision_xml(previous, &unsupported_xml));
    cell.property_change = None;
    Some(Ok(RevisionCategory::Property(PropertyScope::TableCell)))
}

fn cell_unsupported_revision_xml(cell: &Cell) -> Vec<String> {
    let Some(raw) = cell.raw_tcpr.as_deref() else {
        return Vec::new();
    };
    let mut parser = XmlParser::new(raw);
    if parser.next() != Event::Start || parser.name().rsplit(':').next() != Some("tcPr") {
        return Vec::new();
    }
    let mut revisions = Vec::new();
    loop {
        match parser.next() {
            Event::Start => {
                let start = parser.start_pos();
                let preserve = matches!(
                    parser.name().rsplit(':').next(),
                    Some("cellIns" | "cellDel" | "cellMerge")
                );
                parser.skip_element();
                if preserve {
                    revisions.push(parser.raw_slice(start, parser.pos()).to_string());
                }
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    revisions
}

fn merge_cell_revision_xml(previous: Option<String>, revisions: &[String]) -> Option<String> {
    if revisions.is_empty() {
        return previous;
    }
    let missing = |xml: &str| {
        revisions
            .iter()
            .filter(|revision| !xml.contains(revision.as_str()))
            .cloned()
            .collect::<String>()
    };
    match previous {
        Some(mut xml) => {
            let additions = missing(&xml);
            if additions.is_empty() {
                return Some(xml);
            }
            if let Some(close) = xml.rfind("</") {
                xml.insert_str(close, &additions);
            } else if let Some(self_close) = xml.rfind("/>") {
                xml.replace_range(self_close..self_close + 2, ">");
                xml.push_str(&additions);
                xml.push_str("</w:tcPr>");
            }
            Some(xml)
        }
        None => Some(format!("<w:tcPr>{}</w:tcPr>", revisions.concat())),
    }
}

fn replace_row_property_xml(raw_props: &mut Vec<String>, previous: Option<String>) {
    let first = raw_props
        .iter()
        .position(|raw| local_name(raw) == "trPr")
        .unwrap_or(0);
    raw_props.retain(|raw| local_name(raw) != "trPr");
    if let Some(previous) = previous {
        raw_props.insert(first.min(raw_props.len()), previous);
    }
}

fn apply_cell_property_xml(cell: &mut Cell, previous: Option<String>) {
    cell.grid_span = 1;
    cell.v_merge = VMerge::None;
    let Some(previous) = previous else {
        cell.raw_tcpr = None;
        return;
    };

    let mut parser = XmlParser::new(&previous);
    if parser.next() == Event::Start && parser.name() == "w:tcPr" {
        loop {
            match parser.next() {
                Event::Start => {
                    match parser.name() {
                        "w:gridSpan" => {
                            if let Ok(value) = parser.attr("w:val").parse::<u32>() {
                                if value > 0 {
                                    cell.grid_span = value;
                                }
                            }
                        }
                        "w:vMerge" => {
                            cell.v_merge = if parser.attr("w:val") == "restart" {
                                VMerge::Restart
                            } else {
                                VMerge::Continue
                            };
                        }
                        _ => {}
                    }
                    parser.skip_element();
                }
                Event::End | Event::Eof => break,
                Event::Text => {}
            }
        }
    }
    cell.raw_tcpr = Some(previous);
}

fn local_name(raw: &str) -> &str {
    let trimmed = raw.trim_start();
    let Some(rest) = trimmed.strip_prefix('<') else {
        return "";
    };
    let end = rest
        .find([' ', '/', '>', '\t', '\n', '\r'])
        .unwrap_or(rest.len());
    rest[..end].rsplit(':').next().unwrap_or(&rest[..end])
}

fn overlay_revision_cues(props: &mut RunProps, cues: RevisionDisplayCues) {
    props.revision_cues = RevisionDisplayCues::default();
    if cues.insertions > 0 {
        props.revision_cues.insertions = cues.insertions;
        if !props.underline {
            props.underline = true;
            props.revision_cues.underline_added = true;
        }
    }
    if cues.deletions > 0 {
        props.revision_cues.deletions = cues.deletions;
        if !props.strike {
            props.strike = true;
            props.revision_cues.strike_added = true;
        }
    }
}

fn strip_revision_cue(inline: &mut Inline, kind: RevisionKind, normalize_deleted: bool) {
    let strip = |props: &mut RunProps| match kind {
        RevisionKind::Insert => {
            props.revision_cues.insertions = props.revision_cues.insertions.saturating_sub(1);
            if props.revision_cues.insertions == 0 && props.revision_cues.underline_added {
                props.underline = false;
                props.revision_cues.underline_added = false;
            }
        }
        RevisionKind::Delete => {
            props.revision_cues.deletions = props.revision_cues.deletions.saturating_sub(1);
            if props.revision_cues.deletions == 0 && props.revision_cues.strike_added {
                props.strike = false;
                props.revision_cues.strike_added = false;
            }
        }
    };

    match inline {
        Inline::Run(run) => strip(&mut run.props),
        Inline::Hyperlink(link) => {
            for run in &mut link.runs {
                strip(&mut run.props);
            }
            for child in &mut link.content {
                strip_revision_cue(child, kind, normalize_deleted);
            }
        }
        Inline::Tab(props) | Inline::Break(_, props) => strip(props),
        Inline::Revision { content, .. } => {
            for child in content {
                // Nested revision raw remains authoritative until that nested
                // wrapper itself is acted on.
                strip_revision_cue(child, kind, false);
            }
        }
        Inline::TextBox { raw, .. }
        | Inline::SmartArt { raw, .. }
        | Inline::Chart { raw, .. }
        | Inline::Equation { raw, .. }
        | Inline::Field { raw, .. }
        | Inline::FootnoteRef { raw, .. }
        | Inline::Raw(raw)
            if normalize_deleted =>
        {
            normalize_deleted_text(raw);
        }
        Inline::TextBox { .. }
        | Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::Field { .. }
        | Inline::UnsupportedRevision { .. }
        | Inline::FootnoteRef { .. }
        | Inline::Raw(_) => {}
    }
}

fn normalize_deleted_text(raw: &mut String) {
    *raw = raw
        .replace("<w:delText", "<w:t")
        .replace("</w:delText>", "</w:t>")
        .replace("<w:delInstrText", "<w:instrText")
        .replace("</w:delInstrText>", "</w:instrText>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::serialize::document_to_xml;

    fn parse(xml: &str) -> Document {
        parse_document_xml(xml, &Relationships::default())
    }

    #[test]
    fn inline_accept_reject_preserves_direct_formatting_and_raw_boundaries() {
        let xml = concat!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p>",
            "<w:bookmarkStart w:id=\"1\" w:name=\"kept\"/>",
            "<w:ins w:id=\"10\"><w:r><w:rPr><w:u w:val=\"single\"/></w:rPr><w:t>new</w:t></w:r></w:ins>",
            "<w:del w:id=\"11\"><w:r><w:rPr><w:strike/></w:rPr><w:delText>old</w:delText></w:r></w:del>",
            "<w:bookmarkEnd w:id=\"1\"/></w:p></w:body></w:document>"
        );
        let mut document = parse(xml);
        let revisions = document.revisions();
        assert!(document.accept_revision(revisions[0].target).is_applied());
        assert!(document.reject_revision(revisions[1].target).is_applied());

        let Block::Paragraph(paragraph) = &document.body[0] else {
            panic!("paragraph")
        };
        let runs = paragraph
            .content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Run(run) => Some(run),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>(),
            ["new", "old"]
        );
        assert!(runs[0].props.underline, "direct underline was removed");
        assert!(runs[1].props.strike, "direct strike was removed");

        let saved = document_to_xml(&document);
        assert!(!saved.contains("<w:ins"));
        assert!(!saved.contains("<w:del "));
        assert!(!saved.contains("delText"));
        assert!(saved.contains("bookmarkStart"));
        assert!(saved.contains("bookmarkEnd"));
    }

    #[test]
    fn nested_actions_rebuild_outer_wrapper_and_all_is_innermost_first() {
        let xml = concat!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p>",
            "<w:ins w:id=\"20\" w:author=\"outer\"><w:r><w:t>A</w:t></w:r>",
            "<w:del w:id=\"21\"><w:r><w:delText>B</w:delText></w:r></w:del>",
            "<w:r><w:t>C</w:t></w:r></w:ins></w:p></w:body></w:document>"
        );
        let mut current = parse(xml);
        let revisions = current.revisions();
        assert!(current.accept_revision(revisions[1].target).is_applied());
        let saved = document_to_xml(&current);
        assert!(saved.contains("<w:ins w:id=\"20\" w:author=\"outer\""));
        assert!(!saved.contains("<w:del"));
        assert!(!saved.contains(">B<"));
        assert_eq!(parse(&saved).plain_text(), "AC\n");

        let mut accept_all = parse(xml);
        let outcomes = accept_all.accept_all_revisions();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(RevisionOutcome::is_applied));
        assert_eq!(accept_all.plain_text(), "AC\n");
        assert!(!document_to_xml(&accept_all).contains("<w:ins"));

        let mut reject_all = parse(xml);
        assert!(
            reject_all
                .reject_all_revisions()
                .iter()
                .all(RevisionOutcome::is_applied)
        );
        assert_eq!(reject_all.plain_text(), "\n");
    }

    #[test]
    fn stale_unsupported_and_malformed_targets_are_explicit() {
        let xml = concat!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p>",
            "<w:moveFromRangeStart w:id=\"31\"/>",
            "<w:r><w:rPr><w:rPrChange w:id=\"32\"><w:notRPr/></w:rPrChange></w:rPr><w:t>x</w:t></w:r>",
            "</w:p></w:body></w:document>"
        );
        let mut document = parse(xml);
        let revisions = document.revisions();
        assert!(matches!(
            document.accept_revision(revisions[0].target),
            RevisionOutcome::Unsupported { .. }
        ));
        assert!(matches!(
            document.reject_revision(revisions[1].target),
            RevisionOutcome::Malformed { .. }
        ));
        assert!(matches!(
            document.accept_revision(RevisionTarget(u64::MAX)),
            RevisionOutcome::Stale { .. }
        ));
        assert_eq!(document.revisions().len(), 2);
    }
}
