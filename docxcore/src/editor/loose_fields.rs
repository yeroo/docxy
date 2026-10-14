//! Complex fields left as loose markers, edited as one unit at their edges
//! (#1240).
//!
//! #642 collapses a complex field into one [`Inline::Field`] when its begin
//! and end are sibling runs of one inline loop and everything between them is
//! plain. Any other field (above all a TOC or INDEX whose begin and end sit in
//! different paragraphs, or a one-paragraph field whose result holds a tab, a
//! link or a break) stays as zero-width `Inline::Raw` marker runs around an
//! editable result. Such a field is a unit at its edges, as Word has it: the
//! first Backspace after its end (or Delete before its begin) selects it all,
//! the next press deletes it with every one of its marker runs, and a delete
//! across one of its edges takes all of it, so no delete leaves a field shell
//! or half a field behind. Inside, its result stays editable text.
//!
//! The fields are found on demand from the document, never cached, so they
//! cannot go stale after undo or an automation edit.

use super::*;
use crate::field::{FieldEvent, field_events};
use crate::load::is_field_code_raw;
use std::collections::BTreeSet;

/// Where a field-code run is in a block container: its paragraph's index in
/// the container, the editor offset it sits at, and its index in the
/// paragraph's `content` (a marker inside a link or a tracked change is at
/// the index of that link or change).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkerAt {
    para: usize,
    offset: usize,
    inline: usize,
}

impl MarkerAt {
    /// The caret position, in the container's document order.
    fn pos(self) -> (usize, usize) {
        (self.para, self.offset)
    }
}

/// A complex field whose begin and end were paired in one container.
#[derive(Clone, Copy, Debug)]
struct LooseField {
    begin: MarkerAt,
    end: MarkerAt,
    /// Its begin and end runs are paragraph children holding no other
    /// field's marker, and every marker between them is a paragraph child or
    /// in a link whose fields are its own: removing its marker runs removes
    /// exactly this field (and the fields inside it).
    whole: bool,
}

impl LooseField {
    /// A unit at its edges: whole, with a result to select. An empty one
    /// (b == e) would select nothing, and Backspace would never get past it.
    fn is_unit(&self) -> bool {
        self.whole && self.begin.pos() != self.end.pos()
    }

    /// Whether the selection `lo..hi` covers it, so deleting the selection
    /// deletes it. An empty one goes only from strictly inside: deleting the
    /// word just before an index entry keeps the entry.
    fn covered_by(&self, lo: (usize, usize), hi: (usize, usize)) -> bool {
        let (b, e) = (self.begin.pos(), self.end.pos());
        self.whole && lo <= b && e <= hi && (b != e || (lo < b && e < hi))
    }
}

/// The complex fields paired in the paragraphs of `blocks` (one container: a
/// text box's or a cell's blocks, or the body), inner ones before the fields
/// around them. Markers are paired with a stack in document order, those in
/// links and tracked changes too; a collapsed [`Inline::Field`] is balanced
/// and is skipped.
fn loose_fields(blocks: &[Block]) -> Vec<LooseField> {
    let mut pairing = Pairing::default();
    for (para, block) in blocks.iter().enumerate() {
        let Block::Paragraph(p) = block else {
            // A field cannot be paired across markers kept in raw block XML.
            if matches!(block, Block::Raw(raw) if raw.contains("<w:fldChar")) {
                pairing.taint();
            }
            continue;
        };
        let mut offset = 0;
        for (inline, item) in p.content.iter().enumerate() {
            let at = MarkerAt {
                para,
                offset,
                inline,
            };
            match item {
                Inline::Raw(raw) if is_field_code_raw(raw) => {
                    pairing.step(field_events(raw), at, true);
                }
                // A link's own fields (a TOC entry's PAGEREF) go with it;
                // markers it shares with fields outside it pair as nested.
                Inline::Hyperlink(h) if !link_is_balanced(h) => {
                    pairing.nested(&h.content, at);
                }
                Inline::Revision { content, .. } => pairing.nested(content, at),
                Inline::UnsupportedRevision { raw, .. } => {
                    pairing.step(field_events(raw), at, false);
                }
                _ => {}
            }
            offset += inline_len(item);
        }
    }
    pairing.done
}

#[derive(Default)]
struct Pairing {
    /// The open fields, outermost first: where each begins, and whether it is
    /// still whole (see [`LooseField::whole`]).
    open: Vec<(MarkerAt, bool)>,
    done: Vec<LooseField>,
}

impl Pairing {
    /// The fields open now hold a marker that is not a paragraph child.
    fn taint(&mut self) {
        self.open.iter_mut().for_each(|(_, whole)| *whole = false);
    }

    /// The events of one run at `at`; `top` when the run is a paragraph child.
    fn step(&mut self, events: Vec<FieldEvent>, at: MarkerAt, top: bool) {
        let marks = events
            .iter()
            .filter(|e| !matches!(e, FieldEvent::Instr(_)))
            .count();
        if marks == 0 && top {
            return;
        }
        if !top {
            self.taint();
        }
        // Removing a run shared with another field's marker would cut it.
        let alone = top && marks == 1;
        for event in events {
            match event {
                FieldEvent::Begin => self.open.push((at, alone)),
                FieldEvent::End => {
                    if let Some((begin, whole)) = self.open.pop() {
                        self.done.push(LooseField {
                            begin,
                            end: at,
                            whole: whole && alone,
                        });
                    }
                }
                FieldEvent::Instr(_) | FieldEvent::Separate => {}
            }
        }
    }

    /// The markers in `content`, nested in the paragraph child at `at`.
    fn nested(&mut self, content: &[Inline], at: MarkerAt) {
        for item in content {
            match item {
                Inline::Raw(raw) if is_field_code_raw(raw) => {
                    self.step(field_events(raw), at, false);
                }
                Inline::Hyperlink(h) => self.nested(&h.content, at),
                Inline::Revision { content, .. } => self.nested(content, at),
                Inline::UnsupportedRevision { raw, .. } => {
                    self.step(field_events(raw), at, false);
                }
                _ => {}
            }
        }
    }
}

/// Whether a link's field markers are its own children and pair up among
/// themselves (none in a tracked change, none closing a field opened before
/// it or left open after it). Such a link is plain content to the fields
/// around it, and its markers go with it.
fn link_is_balanced(h: &Hyperlink) -> bool {
    let mut depth = 0usize;
    for item in &h.content {
        match item {
            Inline::Raw(raw) if is_field_code_raw(raw) => {
                for event in field_events(raw) {
                    match event {
                        FieldEvent::Begin => depth += 1,
                        FieldEvent::End => match depth.checked_sub(1) {
                            Some(d) => depth = d,
                            None => return false,
                        },
                        FieldEvent::Instr(_) | FieldEvent::Separate => {}
                    }
                }
            }
            Inline::Revision { content, .. } if holds_field_code(content) => return false,
            Inline::UnsupportedRevision { raw, .. } if raw.contains("<w:fldChar") => {
                return false;
            }
            _ => {}
        }
    }
    depth == 0
}

fn holds_field_code(content: &[Inline]) -> bool {
    content.iter().any(|item| match item {
        Inline::Raw(raw) => is_field_code_raw(raw),
        Inline::Hyperlink(h) => holds_field_code(&h.content),
        Inline::Revision { content, .. } => holds_field_code(content),
        Inline::UnsupportedRevision { raw, .. } => raw.contains("<w:fldChar"),
        _ => false,
    })
}

/// Whether `content` has a paragraph-child field-code run holding a begin
/// (`begin`) or an end at editor offset `at`: the cheap test before a
/// Backspace or Delete pairs the container's fields.
fn code_run_at(content: &[Inline], at: usize, begin: bool) -> bool {
    let mut offset = 0;
    for item in content {
        if offset > at {
            break;
        }
        if offset == at {
            if let Inline::Raw(raw) = item {
                if is_field_code_raw(raw)
                    && field_events(raw).iter().any(|e| match e {
                        FieldEvent::Begin => begin,
                        FieldEvent::End => !begin,
                        _ => false,
                    })
                {
                    return true;
                }
            }
        }
        offset += inline_len(item);
    }
    false
}

/// Remove the marker runs of `fields` from `blocks`: every field-code run
/// from each one's begin run through its end run, paragraph children and
/// those in links alike. They are zero-width, so no offset moves.
fn remove_marker_runs(blocks: &mut [Block], fields: &[LooseField]) {
    // (paragraph, inline) of each run to remove, and of each link to clear.
    let mut runs = BTreeSet::new();
    let mut links = BTreeSet::new();
    for f in fields {
        for para in f.begin.para..=f.end.para {
            let Some(Block::Paragraph(p)) = blocks.get(para) else {
                continue;
            };
            let from = if para == f.begin.para {
                f.begin.inline
            } else {
                0
            };
            let to = if para == f.end.para {
                f.end.inline + 1
            } else {
                p.content.len()
            };
            for inline in from..to.min(p.content.len()) {
                match &p.content[inline] {
                    Inline::Raw(raw) if is_field_code_raw(raw) => {
                        runs.insert((para, inline));
                    }
                    Inline::Hyperlink(h) if holds_field_code(&h.content) => {
                        links.insert((para, inline));
                    }
                    _ => {}
                }
            }
        }
    }
    for &(para, inline) in &links {
        if let Some(Block::Paragraph(p)) = blocks.get_mut(para) {
            if let Some(Inline::Hyperlink(h)) = p.content.get_mut(inline) {
                h.content
                    .retain(|i| !matches!(i, Inline::Raw(raw) if is_field_code_raw(raw)));
                h.content_changed = true;
            }
        }
    }
    for &(para, inline) in runs.iter().rev() {
        if let Some(Block::Paragraph(p)) = blocks.get_mut(para) {
            p.content.remove(inline);
        }
    }
}

/// `path` with its last index (the paragraph's in its container) set to `para`.
fn sibling(path: &[usize], para: usize) -> Vec<usize> {
    let mut path = path.to_vec();
    if let Some(last) = path.last_mut() {
        *last = para;
    }
    path
}

impl Editor {
    /// Backspace at the end of a loose field (`at_end`), or Delete at its
    /// start: select the whole field, as Word does, and return true. The
    /// outermost of several fields ending (or starting) there is selected.
    /// Selecting is not an edit: no undo step. The caret keeps its side.
    pub(super) fn select_loose_field(&mut self, at_end: bool) -> bool {
        let off = self.caret.offset;
        let gate = resolve_para(&self.doc.body, &self.caret.path)
            .is_some_and(|p| code_run_at(&p.content, off, !at_end));
        if !gate {
            return false;
        }
        let Some((blocks, idx)) = container(&self.doc.body, &self.caret.path) else {
            return false;
        };
        let here = (idx, off);
        let fields = loose_fields(blocks);
        let mut units = fields.iter().filter(|f| f.is_unit());
        // Inner fields are listed first: the outermost ending here is the
        // last of them, the outermost starting here the first begin run.
        let pick = if at_end {
            units.rfind(|f| f.end.pos() == here)
        } else {
            units
                .filter(|f| f.begin.pos() == here)
                .min_by_key(|f| f.begin.inline)
        };
        let Some(field) = pick else {
            return false;
        };
        let other = if at_end { field.begin } else { field.end };
        self.anchor = Some(Caret {
            path: sibling(&self.caret.path, other.para),
            offset: other.offset,
        });
        self.last = EditKind::None;
        true
    }

    /// Widen the selection `lo..hi` about to be deleted over every loose
    /// field it cuts (one edge strictly inside it), until it cuts none, and
    /// return the fields it then covers, whose marker runs the delete removes.
    /// Nothing when `lo` and `hi` are in different containers.
    pub(super) fn widen_over_loose_fields(
        &self,
        lo: &mut Caret,
        hi: &mut Caret,
    ) -> Vec<LooseFieldCut> {
        if spans_containers(lo, hi) {
            return Vec::new();
        }
        let Some((blocks, li)) = container(&self.doc.body, &lo.path) else {
            return Vec::new();
        };
        let fields = loose_fields(blocks);
        if fields.is_empty() {
            return Vec::new();
        }
        let hii = *hi.path.last().unwrap_or(&li);
        let (mut l, mut h) = ((li, lo.offset), (hii, hi.offset));
        loop {
            let mut widened = false;
            for f in fields.iter().filter(|f| f.is_unit()) {
                let (b, e) = (f.begin.pos(), f.end.pos());
                let cuts_begin = l < b && b < h && h < e;
                let cuts_end = b < l && l < e && e < h;
                if cuts_begin || cuts_end {
                    l = l.min(b);
                    h = h.max(e);
                    widened = true;
                }
            }
            if !widened {
                break;
            }
        }
        *lo = Caret {
            path: sibling(&lo.path, l.0),
            offset: l.1,
        };
        *hi = Caret {
            path: sibling(&hi.path, h.0),
            offset: h.1,
        };
        fields
            .into_iter()
            .filter(|f| f.covered_by(l, h))
            .map(LooseFieldCut)
            .collect()
    }

    /// Remove the marker runs of the fields a delete covers (from
    /// [`Editor::widen_over_loose_fields`]) in the container of `path`. Not
    /// while tracking: a tracked delete records the result as deleted and
    /// keeps the markers, balanced.
    pub(super) fn remove_loose_fields(&mut self, path: &[usize], cut: &[LooseFieldCut]) {
        if cut.is_empty() || self.track.is_some() {
            return;
        }
        let fields: Vec<LooseField> = cut.iter().map(|c| c.0).collect();
        if let Some((blocks, _)) = container_mut(&mut self.doc.body, path) {
            remove_marker_runs(blocks, &fields);
        }
    }
}

/// A loose field a selection delete covers.
pub(super) struct LooseFieldCut(LooseField);

/// Whether `content[k]` is a field-code run holding an end and no begin.
fn ends_a_field(content: &[Inline], k: usize) -> Option<bool> {
    match content.get(k)? {
        Inline::Raw(raw) if is_field_code_raw(raw) => {
            let events = field_events(raw);
            if events.iter().any(|e| matches!(e, FieldEvent::Begin)) {
                return None;
            }
            Some(events.iter().any(|e| matches!(e, FieldEvent::End)))
        }
        _ => Some(false),
    }
}

/// Where text typed at the zero-width inlines from `content[from]` on goes
/// (#1240): past the last field end among them, so text typed at a field's
/// end lands after it rather than in its result. The scan stops at the first
/// inline with a width and at a field's begin, so text typed where one field
/// ends and the next begins lands between them. `None` when no end is there.
pub(super) fn past_field_ends(content: &[Inline], from: usize) -> Option<usize> {
    let mut past = None;
    let mut k = from;
    while k < content.len() && inline_len(&content[k]) == 0 {
        match ends_a_field(content, k) {
            None => break,
            Some(true) => past = Some(k + 1),
            Some(false) => {}
        }
        k += 1;
    }
    past
}

/// The index in `head` (a paragraph's content before an insertion point)
/// from which its trailing zero-width inlines start at a field's begin: text
/// pasted there goes before that field, not into its result (#1240).
pub(super) fn first_trailing_begin(head: &[Inline]) -> Option<usize> {
    let mut k = head.len();
    while k > 0 && inline_len(&head[k - 1]) == 0 {
        k -= 1;
    }
    (k..head.len()).find(|&i| {
        matches!(&head[i], Inline::Raw(raw) if is_field_code_raw(raw)
            && field_events(raw).iter().any(|e| matches!(e, FieldEvent::Begin)))
    })
}

#[cfg(test)]
mod tests;
