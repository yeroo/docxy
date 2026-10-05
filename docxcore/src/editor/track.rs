//! Track Changes recording (#624).
//!
//! With tracking on, typing and pasting record a tracked insertion and every
//! deletion records a tracked deletion, instead of changing the text outright.
//!
//! * An insertion is ordinary editable text carrying
//!   [`crate::model::TrackedInsert`] on its runs (so offsets, formatting, copy
//!   and rendering see plain runs); consecutive typing extends the same
//!   insertion, and a save wraps it in `<w:ins>`.
//! * A deletion moves the text into an [`Inline::Revision`] of kind
//!   [`RevisionKind::Delete`], which the editor counts as zero-width: the
//!   caret steps over it and offsets after it are unchanged. Adjacent
//!   deletions by the same author merge into one wrapper.
//! * Deleting text inside the author's own insertion removes it outright, as
//!   Word does.
//!
//! Not recorded (they change the document as without tracking): paragraph
//! marks (Enter, and a Backspace or Delete that joins paragraphs), text inside
//! a hyperlink, fields and other one-unit inlines, formatting, and table
//! structure. A selection across paragraphs records the deletion of its text
//! in each paragraph and keeps the paragraph marks.

use super::{
    Editor, content_delete, content_insert_at, inline_len, map_prop_range, para_mut, resolve_para,
};
use crate::model::{Inline, RevisionKind, RevisionMetadata, Run, RunProps, TrackedInsert};

/// Who Track Changes records edits as, and the clock that stamps each one.
/// A plain function keeps `docxcore` free of a time dependency and the tests
/// deterministic: the host passes its own (an `xsd:dateTime` in UTC).
#[derive(Clone)]
pub struct TrackAuthor {
    pub author: String,
    pub clock: fn() -> String,
}

impl Editor {
    /// Turn Track Changes on, recording as `track`, or off with `None`.
    /// Not an edit: no undo step.
    pub fn set_track_changes(&mut self, track: Option<TrackAuthor>) {
        self.track = track;
    }

    /// Whether edits are being recorded.
    pub fn track_changes(&self) -> bool {
        self.track.is_some()
    }

    /// The reviewer edits are recorded as.
    pub fn track_author(&self) -> Option<&str> {
        self.track.as_ref().map(|t| t.author.as_str())
    }

    /// The metadata of the next revision recorded: an id above every revision
    /// and comment-marker id in the document, the author, and the clock's now.
    fn fresh_revision_metadata(&self) -> Option<RevisionMetadata> {
        let track = self.track.as_ref()?;
        let mut max = 0u64;
        let numeric = |id: &str| id.parse::<u64>().ok();
        for revision in self.doc.revisions() {
            max = max.max(
                revision
                    .metadata
                    .id
                    .as_deref()
                    .and_then(numeric)
                    .unwrap_or(0),
            );
        }
        for id in crate::inspect::comment_marker_ids(&self.doc) {
            max = max.max(numeric(&id).unwrap_or(0));
        }
        Some(RevisionMetadata {
            id: Some((max + 1).to_string()),
            author: Some(track.author.clone()),
            date: Some((track.clock)()),
            ..RevisionMetadata::default()
        })
    }

    /// Text was just inserted at `[start, start + len)` of the paragraph at
    /// `path`: record it as a tracked insertion when tracking, unless it
    /// extends the author's own insertion; and when not tracking, keep it out
    /// of an insertion it landed in the middle of.
    pub(super) fn settle_inserted(&mut self, path: &[usize], start: usize, len: usize) {
        if len == 0 {
            return;
        }
        let Some(para) = resolve_para(&self.doc.body, path) else {
            return;
        };
        let at = what_is_at(&para.content, start);
        match (&self.track, at) {
            (Some(_), What::InLink) => {}
            (Some(track), What::Recorded(by)) if by.as_deref() == Some(track.author.as_str()) => {}
            (Some(_), _) => {
                let Some(meta) = self.fresh_revision_metadata() else {
                    return;
                };
                if let Some(p) = para_mut(&mut self.doc.body, path) {
                    map_prop_range(&mut p.content, start, start + len, &|props| {
                        record_insert(props, &meta)
                    });
                }
                self.doc.initialize_revision_targets();
            }
            (None, What::Recorded(_)) => {
                if let Some(p) = para_mut(&mut self.doc.body, path) {
                    map_prop_range(&mut p.content, start, start + len, &|props| {
                        clear_insert_record(props)
                    });
                }
            }
            (None, _) => {}
        }
    }

    /// Delete the character at editor offset `idx` of the paragraph at `path`:
    /// recorded as a tracked deletion when tracking, else removed outright.
    pub(super) fn delete_char_at(&mut self, path: &[usize], idx: usize) {
        let Some(author) = self.track.as_ref().map(|t| t.author.clone()) else {
            if let Some(p) = para_mut(&mut self.doc.body, path) {
                content_delete(&mut p.content, idx);
            }
            return;
        };
        let Some(para) = resolve_para(&self.doc.body, path) else {
            return;
        };
        let plan = plan_delete(&para.content, idx, &author);
        match plan {
            Plan::Remove => {
                if let Some(p) = para_mut(&mut self.doc.body, path) {
                    content_delete(&mut p.content, idx);
                }
            }
            Plan::Record { merges } => {
                let meta = if merges {
                    None
                } else {
                    self.fresh_revision_metadata()
                };
                if let Some(p) = para_mut(&mut self.doc.body, path) {
                    record_deletion(&mut p.content, idx, &author, meta);
                }
                if !merges {
                    self.doc.initialize_revision_targets();
                }
            }
        }
    }

    /// Delete the text of the selection `lo..hi` over several sibling
    /// paragraphs as tracked deletions, keeping the paragraph marks. Whether
    /// it applied (it does whenever tracking).
    pub(super) fn delete_text_across_paragraphs(
        &mut self,
        lo: &super::Caret,
        hi: &super::Caret,
    ) -> bool {
        if self.track.is_none() {
            return false;
        }
        let mut path = lo.path.clone();
        let (first, last) = (*lo.path.last().unwrap(), *hi.path.last().unwrap());
        for index in first..=last {
            *path.last_mut().unwrap() = index;
            let Some(para) = resolve_para(&self.doc.body, &path) else {
                continue;
            };
            let len: usize = para.content.iter().map(inline_len).sum();
            let (from, to) = match (index == first, index == last) {
                (true, true) => (lo.offset, hi.offset),
                (true, false) => (lo.offset, len),
                (false, true) => (0, hi.offset.min(len)),
                (false, false) => (0, len),
            };
            for _ in from..to {
                self.delete_char_at(&path, from);
            }
        }
        true
    }

    /// Replace `[start, end)` of the paragraph at `path` with `with`: as a
    /// tracked deletion of the old text and a tracked insertion of the new when
    /// tracking, else in place.
    pub(super) fn replace_text_range(
        &mut self,
        path: &[usize],
        start: usize,
        end: usize,
        with: &str,
    ) {
        if self.track.is_none() {
            if let Some(p) = para_mut(&mut self.doc.body, path) {
                super::replace_range_in_content(&mut p.content, start, end, with);
            }
            return;
        }
        let with = super::without_field_chars(with);
        for _ in start..end {
            self.delete_char_at(path, start);
        }
        let mut n = 0;
        if let Some(p) = para_mut(&mut self.doc.body, path) {
            for (k, ch) in with.chars().enumerate() {
                content_insert_at(&mut p.content, start + k, ch);
                n += 1;
            }
        }
        self.settle_inserted(path, start, n);
    }

    /// `clip` as it should go in: its text recorded as one tracked insertion
    /// when tracking, or with any insertion records taken off when not (a
    /// pasted copy of recorded text is not itself recorded).
    pub(super) fn clip_for_insertion(&self, clip: &super::Clip) -> super::Clip {
        let mut out = clip.clone();
        match self.fresh_revision_metadata() {
            Some(meta) => {
                for inline in out.paras.iter_mut().flatten() {
                    with_props(inline, |props| record_insert(props, &meta));
                }
            }
            None => {
                for inline in out.paras.iter_mut().flatten() {
                    with_props(inline, clear_insert_record);
                }
            }
        }
        out
    }
}

/// Run `f` on the run properties of a run, tab or break.
pub(super) fn with_props(inline: &mut Inline, f: impl FnOnce(&mut RunProps)) {
    match inline {
        Inline::Run(r) => f(&mut r.props),
        Inline::Tab(props) | Inline::Break(_, props) => f(props),
        _ => {}
    }
}

/// Record `props` as part of the insertion `meta`, with the underline cue a
/// loaded insertion shows. Replaces any insertion it was recorded as.
fn record_insert(props: &mut RunProps, meta: &RevisionMetadata) {
    clear_insert_record(props);
    props.tracked_insert = Some(TrackedInsert {
        metadata: meta.clone(),
    });
    if props.revision_cues.insertions == 0 && !props.underline {
        props.underline = true;
        props.revision_cues.underline_added = true;
    }
    props.revision_cues.insertions = props.revision_cues.insertions.saturating_add(1);
}

/// Take the insertion record and its cue off `props`.
pub(super) fn clear_insert_record(props: &mut RunProps) {
    if props.tracked_insert.take().is_none() {
        return;
    }
    props.revision_cues.insertions = props.revision_cues.insertions.saturating_sub(1);
    if props.revision_cues.insertions == 0 && props.revision_cues.underline_added {
        props.underline = false;
        props.revision_cues.underline_added = false;
    }
}

/// What is at editor offset `idx` of a paragraph's content.
enum What {
    Plain,
    /// A run of a tracked insertion, by this author.
    Recorded(Option<String>),
    /// Inside a hyperlink: not recorded.
    InLink,
}

fn what_is_at(content: &[Inline], idx: usize) -> What {
    let mut acc = 0;
    for inline in content {
        let len = inline_len(inline);
        if idx < acc + len {
            let props = match inline {
                Inline::Run(r) => &r.props,
                Inline::Tab(props) | Inline::Break(_, props) => props,
                Inline::Hyperlink(_) => return What::InLink,
                _ => return What::Plain,
            };
            return match &props.tracked_insert {
                Some(t) => What::Recorded(t.metadata.author.clone()),
                None => What::Plain,
            };
        }
        acc += len;
    }
    What::Plain
}

enum Plan {
    /// Delete for real: not a plain run character, or the author's own
    /// insertion.
    Remove,
    /// Record a deletion; `merges` when an adjacent deletion by the author
    /// takes it (no new revision, so no new id).
    Record { merges: bool },
}

fn is_own_deletion(inline: Option<&Inline>, author: &str) -> bool {
    matches!(
        inline,
        Some(Inline::Revision {
            kind: RevisionKind::Delete,
            metadata,
            ..
        }) if metadata.author.as_deref() == Some(author)
    )
}

fn plan_delete(content: &[Inline], idx: usize, author: &str) -> Plan {
    let mut acc = 0;
    for (i, inline) in content.iter().enumerate() {
        let len = inline_len(inline);
        if idx < acc + len {
            let (props, last) = match inline {
                Inline::Run(r) => (&r.props, idx - acc + 1 == len),
                Inline::Tab(props) | Inline::Break(_, props) => (props, true),
                _ => return Plan::Remove,
            };
            if props
                .tracked_insert
                .as_ref()
                .is_some_and(|t| t.metadata.author.as_deref() == Some(author))
            {
                return Plan::Remove;
            }
            let first = idx == acc;
            let merges = (first && i > 0 && is_own_deletion(content.get(i - 1), author))
                || (last && is_own_deletion(content.get(i + 1), author));
            return Plan::Record { merges };
        }
        acc += len;
    }
    Plan::Remove
}

/// The deleted copy of a run-like inline: no insertion record, struck through.
fn deleted_props(props: &RunProps) -> RunProps {
    let mut p = props.clone();
    clear_insert_record(&mut p);
    if p.revision_cues.deletions == 0 && !p.strike {
        p.strike = true;
        p.revision_cues.strike_added = true;
    }
    p.revision_cues.deletions = p.revision_cues.deletions.saturating_add(1);
    p
}

fn deletion_wrapper(meta: RevisionMetadata, inner: Inline) -> Inline {
    let mut raw = String::from("<w:del");
    for (name, value) in [
        ("w:id", &meta.id),
        ("w:author", &meta.author),
        ("w:date", &meta.date),
    ] {
        if let Some(value) = value {
            raw.push(' ');
            raw.push_str(name);
            raw.push_str("=\"");
            crate::serialize::esc_attr(value, &mut raw);
            raw.push('"');
        }
    }
    raw.push_str("/>");
    Inline::Revision {
        kind: RevisionKind::Delete,
        metadata: meta,
        raw,
        content: vec![inner],
        content_changed: true,
    }
}

/// Move the character at `idx` of `content` into a tracked deletion by
/// `author`: a new wrapper with `meta`, or merged into an adjacent wrapper of
/// the author's (when `meta` is `None`, as [`plan_delete`] decided).
fn record_deletion(
    content: &mut Vec<Inline>,
    idx: usize,
    author: &str,
    meta: Option<RevisionMetadata>,
) {
    let mut acc = 0;
    let mut found = None;
    for (i, inline) in content.iter().enumerate() {
        let len = inline_len(inline);
        if idx < acc + len {
            found = Some((i, idx - acc));
            break;
        }
        acc += len;
    }
    let Some((i, local)) = found else {
        return;
    };
    let mut pieces: Vec<Inline> = Vec::new();
    let wrapper_at;
    let deleted = match &content[i] {
        Inline::Run(r) => {
            let chars: Vec<char> = r.text.chars().collect();
            let text = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
            if local > 0 {
                pieces.push(Inline::Run(Run {
                    text: text(0..local),
                    props: r.props.clone(),
                }));
            }
            wrapper_at = pieces.len();
            let deleted = Inline::Run(Run {
                text: text(local..local + 1),
                props: deleted_props(&r.props),
            });
            pieces.push(deleted.clone());
            if local + 1 < chars.len() {
                pieces.push(Inline::Run(Run {
                    text: text(local + 1..chars.len()),
                    props: r.props.clone(),
                }));
            }
            deleted
        }
        Inline::Tab(props) => {
            wrapper_at = 0;
            let deleted = Inline::Tab(deleted_props(props));
            pieces.push(deleted.clone());
            deleted
        }
        Inline::Break(kind, props) => {
            wrapper_at = 0;
            let deleted = Inline::Break(*kind, deleted_props(props));
            pieces.push(deleted.clone());
            deleted
        }
        _ => return,
    };
    pieces[wrapper_at] = deletion_wrapper(meta.unwrap_or_default(), deleted);
    content.splice(i..=i, pieces);
    let mut at = i + wrapper_at;

    // Join the author's deletion just before, and just after, into one: the
    // older wrapper stays (its id, date and place), the new text goes into it.
    let prev_own = at > 0 && is_own_deletion(content.get(at - 1), author);
    let next_own = is_own_deletion(content.get(at + 1), author);
    if !prev_own && !next_own {
        return;
    }
    let take = |inline: Inline| match inline {
        Inline::Revision { content, .. } => content,
        _ => Vec::new(),
    };
    let mut added = take(content.remove(at));
    if prev_own {
        at -= 1;
        if next_own {
            added.extend(take(content.remove(at + 1)));
        }
        grow(&mut content[at], Vec::new(), added);
    } else {
        // Only the one after: the new text goes before what it held.
        grow(&mut content[at], added, Vec::new());
    }
}

/// Put `before` ahead of, and `after` behind, the content of the deletion
/// wrapper `into`, joining adjacent runs that read the same.
fn grow(into: &mut Inline, before: Vec<Inline>, after: Vec<Inline>) {
    let Inline::Revision {
        content,
        content_changed,
        ..
    } = into
    else {
        return;
    };
    *content_changed = true;
    let old = std::mem::take(content);
    let mut merged: Vec<Inline> = Vec::new();
    for inline in before.into_iter().chain(old).chain(after) {
        match (merged.last_mut(), inline) {
            (Some(Inline::Run(a)), Inline::Run(b)) if a.props == b.props => {
                a.text.push_str(&b.text)
            }
            (_, inline) => merged.push(inline),
        }
    }
    *content = merged;
}
