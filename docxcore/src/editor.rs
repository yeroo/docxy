//! Editor core: a path-addressed caret over the document plus text edit
//! operations and undo/redo. Pure (no terminal), so it is unit-tested directly.
//!
//! The caret is a **path** into the document tree ending at a paragraph:
//! `[block]` for a top-level paragraph, or `[table, row, cell, block, ...]` to
//! reach a paragraph inside a table cell (recursively for nested tables). This
//! lets the cursor move into and edit table cells.
//!
//! Structural merges (Backspace at start / Delete at end) only join *sibling*
//! paragraphs in the same container, so editing never escapes a table cell.

use crate::model::*;
use crate::review::{RevisionAction, RevisionOutcome};
use std::collections::VecDeque;
use std::sync::Arc;

mod cover;
mod flat;
mod sections;
mod table_design;
mod table_layout;
mod tables;
mod track;
mod visible;
pub use flat::{FlatDocument, FlatStory, StoryOffset};
pub use table_design::BorderCmd;
pub use table_layout::{AutoFitKind, CellSep, DeleteShift, SortKey, SortKind, SortSpec};
pub use tables::{CellRange, TablePos};
pub use track::TrackAuthor;
pub use visible::{FoundMatch, step_found};

/// A path into the document tree (to a paragraph) plus a character offset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Caret {
    pub path: Vec<usize>,
    pub offset: usize,
}

impl Caret {
    /// A caret in a top-level paragraph block.
    pub fn top(block: usize, offset: usize) -> Self {
        Caret {
            path: vec![block],
            offset,
        }
    }
    pub fn at(path: Vec<usize>, offset: usize) -> Self {
        Caret { path, offset }
    }
}

/// A search match: a character range within a paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub path: Vec<usize>,
    pub start: usize,
    pub end: usize,
}

/// A revision together with the valid editor caret range used to review it.
///
/// Inline wrappers are non-editable and therefore use a collapsed range at
/// their boundary. Property changes use the paragraph or run range whose
/// properties they affect. The stable target in [`RevisionAddress`] remains
/// the action identity; the carets are recalculated after every edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionLocation {
    pub address: RevisionAddress,
    pub start: Caret,
    pub end: Caret,
}

impl RevisionLocation {
    pub fn contains(&self, caret: &Caret) -> bool {
        if self.start.path != caret.path || self.end.path != caret.path {
            return false;
        }
        if self.start == self.end {
            return *caret == self.start;
        }
        self.start.offset <= caret.offset && caret.offset <= self.end.offset
    }
}

/// Clipboard contents: styled inline content, one entry per (partial) paragraph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clip {
    pub paras: Vec<Vec<Inline>>,
}

impl Clip {
    /// The plain text of this clip (paragraphs joined by newlines) — used to put
    /// the selection on the OS clipboard.
    pub fn to_text(&self) -> String {
        self.paras
            .iter()
            .map(|p| p.iter().map(|i| i.text()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Build a clip from external plain text (newlines split paragraphs, tabs
    /// become tab inlines) — used to paste text from the OS clipboard.
    pub fn from_text(s: &str) -> Clip {
        let mut paras = Vec::new();
        for line in s.split('\n') {
            let mut inl: Vec<Inline> = Vec::new();
            let mut buf = String::new();
            for ch in line.chars() {
                match ch {
                    // A field's stand-in (from editor or automation text) is
                    // not text: pasting it back must not add a character.
                    '\r' | FIELD_CHAR => {}
                    '\t' => {
                        if !buf.is_empty() {
                            inl.push(Inline::Run(Run {
                                text: std::mem::take(&mut buf),
                                props: RunProps::default(),
                            }));
                        }
                        inl.push(Inline::Tab(RunProps::default()));
                    }
                    _ => buf.push(ch),
                }
            }
            if !buf.is_empty() {
                inl.push(Inline::Run(Run {
                    text: buf,
                    props: RunProps::default(),
                }));
            }
            paras.push(inl);
        }
        Clip { paras }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Insert,
    Delete,
    Structural,
}

#[derive(Clone)]
struct Snapshot {
    /// The document's top-level blocks. Those in the common prefix and suffix
    /// with the step it was taken next to are that step's blocks, shared (see
    /// [`share_body`]); the ones in between are copies. A step costs the
    /// top-level blocks it changed, whole: an edit in a table cell copies
    /// the table (#853).
    body: Vec<Arc<Block>>,
    /// About how many bytes the step's own copies take ([`block_weight`]),
    /// what the history's memory budget counts.
    weight: usize,
    caret: Caret,
    anchor: Option<Caret>,
    review_target: Option<RevisionTarget>,
    /// What the step is called in an undo list (#619); travels with the step
    /// between the undo and redo stacks.
    name: StepName,
    /// The step's identity, unique across every editor in the process
    /// (0 until the step is pushed); travels with the step too.
    serial: u64,
    /// The formatting toggled at the caret, so undoing or redoing a Bold
    /// at an insertion point switches it back off or on (#854).
    pending: Option<PendingFormat>,
}

/// Formatting toggled at an insertion point (Ctrl+B with nothing
/// selected, #854): what the next character typed there takes, as Word
/// does. Each toggle is a setter and the value it sets, applied in order
/// over the props the character would get anyway.
#[derive(Clone)]
struct PendingFormat {
    caret: Caret,
    /// The toggle's own undo step. Any later step, from whatever edit,
    /// leaves the toggles behind.
    serial: Option<u64>,
    toggles: Vec<Toggle>,
}

/// A run property's setter and the value a toggle sets it to.
type Toggle = (fn(&mut RunProps, bool), bool);

impl PendingFormat {
    fn apply(&self, props: &mut RunProps) {
        for (set, value) in &self.toggles {
            set(props, *value);
        }
    }
}

/// An undo step's name. Typing is named by its text as it coalesces; a host
/// names a command's steps with [`Editor::name_command`]; anything else
/// falls back to what kind of edit pushed it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StepName {
    Typing(String),
    Named(String),
    Unnamed(EditKind),
}

/// The longest typed text an undo-list label shows before it is shortened.
const TYPING_LABEL_CHARS: usize = 30;

/// A manual line break (Shift+Enter, `<w:br/>`) as typed text: what
/// [`Editor::insert_char`] turns into one and what a typing step records for
/// one, so Repeat types it again as a line break, not a paragraph. The same
/// character Word's own text uses for it.
pub const LINE_BREAK: char = '\u{000b}';

impl StepName {
    fn label(&self) -> String {
        match self {
            StepName::Typing(text) if text.is_empty() => "Typing".into(),
            StepName::Typing(text) => {
                // A line break shows as an arrow, not a control character.
                let shown = |text: &str| text.replace(LINE_BREAK, "\u{21b5}");
                if text.chars().count() > TYPING_LABEL_CHARS {
                    let short: String = text.chars().take(TYPING_LABEL_CHARS).collect();
                    format!("Typing \"{}\u{2026}\"", shown(&short))
                } else {
                    format!("Typing \"{}\"", shown(text))
                }
            }
            StepName::Named(name) => name.clone(),
            StepName::Unnamed(EditKind::Insert) => "Typing".into(),
            StepName::Unnamed(EditKind::Delete) => "Delete".into(),
            StepName::Unnamed(_) => "Edit".into(),
        }
    }
}

/// The last undo-step serial handed out, shared by every editor so that a
/// serial names one step in the whole process (a host's Repeat record can
/// then never match another document's step by coincidence).
static UNDO_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// The last serial issued on this thread.
    static LAST_SERIAL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The most recent undo-step serial issued on this thread, by any editor.
/// Every step pushed after this call has a larger serial (see
/// [`Editor::name_command`]), and it changes exactly when a step is pushed
/// on this thread, so a host can tell that no edit pushed one since. Per
/// thread, so editors on other threads never disturb it.
pub fn undo_serial_counter() -> u64 {
    LAST_SERIAL.with(|c| c.get())
}

/// The most undo steps an editor keeps. Word has no undo-levels option and
/// keeps every step (#853); this and [`HISTORY_BUDGET`] only guard memory.
const UNDO_CAP: usize = 20_000;

/// About how many bytes of copied blocks the undo history may hold (see
/// [`Snapshot::weight`]) before its oldest steps go, though never below
/// [`UNDO_FLOOR`] steps: steps that each copy a large table reach it early.
///
/// A step's weight estimates what that step copied when it was taken, not
/// what dropping it frees: a block an old step copied may still be shared by
/// newer ones, so the memory the history keeps can exceed the budget. The
/// floor and [`UNDO_CAP`] bound how far.
const HISTORY_BUDGET: usize = 256 << 20;

/// The undo steps the history always keeps, budget or not: the 500 it kept
/// before #853.
const UNDO_FLOOR: usize = 500;

/// The most characters one typing step holds, as in Word: typing on past
/// them starts a new step (#853). A line break counts as one.
const TYPING_STEP_CHARS: usize = 128;

/// `body` as an undo step holds it, and about how many bytes it copied:
/// the common prefix and suffix with `like` (an inserted or removed
/// paragraph shifts the blocks after it) are `like`'s blocks, shared; only
/// the top-level blocks in between are copied. Shared only when strictly
/// equal, preserved element attributes too, so a step never swaps a
/// look-alike paragraph's `w14:paraId` or rsids for another's.
fn share_body(body: &[Block], like: Option<&[Arc<Block>]>) -> (Vec<Arc<Block>>, usize) {
    let like = like.unwrap_or_default();
    let same = |(b, l): &(&Block, &Arc<Block>)| strictly_equal(*b, l.as_ref());
    let prefix = body.iter().zip(like).take_while(same).count();
    let room = body.len().min(like.len()) - prefix;
    let suffix = body
        .iter()
        .rev()
        .zip(like.iter().rev())
        .take(room)
        .take_while(same)
        .count();
    let copied = &body[prefix..body.len() - suffix];
    let mut out = Vec::with_capacity(body.len());
    out.extend(like[..prefix].iter().cloned());
    out.extend(copied.iter().map(|b| Arc::new(b.clone())));
    out.extend(like[like.len() - suffix..].iter().cloned());
    // The step's own list of blocks is a copy too, one pointer a block.
    let list = out.capacity() * std::mem::size_of::<Arc<Block>>();
    (out, list + copied.iter().map(block_weight).sum::<usize>())
}

/// About how many bytes a copy of `block` takes, erring high: the length of
/// its debug form, which spells out every field, string and child (raw XML,
/// text boxes, hyperlinks, revisions, cells), and never less than the
/// block's own size. Counted, not built. An estimate for the history's
/// budget only.
fn block_weight(block: &Block) -> usize {
    struct Count(usize);
    impl std::fmt::Write for Count {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0 += s.len();
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = std::fmt::Write::write_fmt(&mut count, format_args!("{block:?}"));
    count.0.max(std::mem::size_of::<Block>())
}

#[cfg(test)]
thread_local! {
    /// How many undo snapshots this thread has built, for tests to count.
    static SNAPSHOTS_TAKEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The document an undo step holds.
fn unshare_body(body: Vec<Arc<Block>>) -> Document {
    Document {
        body: body.into_iter().map(Arc::unwrap_or_clone).collect(),
    }
}

/// An editing session over a [`Document`].
pub struct Editor {
    pub doc: Document,
    pub caret: Caret,
    /// Selection anchor (the fixed end); the moving end is the caret.
    pub anchor: Option<Caret>,
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    /// [`HISTORY_BUDGET`], smaller in tests.
    history_budget: usize,
    /// While [`Editor::one_step`] runs: `Some(pushed)`, whether its one step
    /// is pushed yet. Once it is, edits take no snapshot ([`Editor::grouped`]).
    grouping: Option<bool>,
    last: EditKind,
    review_target: Option<RevisionTarget>,
    /// Mail merge's Preview Results record (#628), shown in merge fields'
    /// `text` only. Re-applied after undo/redo, since a snapshot holds
    /// whatever record was shown when it was taken.
    merge_preview: Option<crate::merge::MergePreview>,
    /// A preview has been shown since this editor opened, so merge fields'
    /// text may need putting back.
    merge_previewed: bool,
    /// Track Changes (#624): edits are recorded as tracked changes by this
    /// reviewer while `Some`. Not saved with the document: the host sets it
    /// from the settings and its own identity.
    track: Option<TrackAuthor>,
    /// Formatting toggled at the caret, for the next character typed (#854);
    /// see [`Editor::pending_format`] for while it holds.
    pending: Option<PendingFormat>,
}

impl Editor {
    pub fn new(mut doc: Document) -> Self {
        doc.initialize_revision_targets();
        let path = first_paragraph_path(&doc.body).unwrap_or_else(|| vec![0]);
        Editor {
            doc,
            caret: Caret { path, offset: 0 },
            anchor: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_budget: HISTORY_BUDGET,
            grouping: None,
            last: EditKind::None,
            review_target: None,
            merge_preview: None,
            merge_previewed: false,
            track: None,
            pending: None,
        }
    }

    /// Show a mail-merge record in the merge fields (Preview Results), or
    /// their placeholders again with `None`. Display only: no undo step, and
    /// `raw` (what is saved) never changes.
    pub fn set_merge_preview(&mut self, preview: Option<crate::merge::MergePreview>) {
        self.merge_preview = preview;
        self.refresh_merge_preview();
    }

    /// Re-apply the merge preview (or its absence) to every merge field. Hosts
    /// call it after an edit that may have added a merge field; undo and redo
    /// call it themselves. Free when no preview was ever shown.
    pub fn refresh_merge_preview(&mut self) {
        if self.merge_preview.is_none() && !self.merge_previewed {
            return;
        }
        self.merge_previewed = true;
        crate::merge::preview::apply_preview(&mut self.doc, self.merge_preview.as_ref());
        // A field whose cache was empty now takes an offset; keep the caret
        // and anchor inside their paragraphs.
        let clamp = |doc: &Document, c: &mut Caret| {
            if let Some(p) = resolve_para(&doc.body, &c.path) {
                c.offset = c.offset.min(para_text_len(p));
            }
        };
        clamp(&self.doc, &mut self.caret);
        if let Some(a) = self.anchor.as_mut() {
            clamp(&self.doc, a);
        }
    }

    /// The document to export as text (Markdown, HTML, plain text): merge
    /// fields show their placeholders, not a previewed record.
    pub fn export_doc(&self) -> std::borrow::Cow<'_, Document> {
        if self.merge_preview.is_none() {
            return std::borrow::Cow::Borrowed(&self.doc);
        }
        let mut doc = self.doc.clone();
        crate::merge::preview::apply_preview(&mut doc, None);
        std::borrow::Cow::Owned(doc)
    }

    /// The current state as an undo step, sharing the blocks it has in
    /// common with the newest step.
    fn snapshot(&self) -> Snapshot {
        self.snapshot_like(self.undo.back())
    }

    fn snapshot_like(&self, like: Option<&Snapshot>) -> Snapshot {
        #[cfg(test)]
        SNAPSHOTS_TAKEN.with(|n| n.set(n.get() + 1));
        let (body, weight) = share_body(&self.doc.body, like.map(|s| s.body.as_slice()));
        Snapshot {
            body,
            weight,
            caret: self.caret.clone(),
            anchor: self.anchor.clone(),
            review_target: self.review_target,
            name: StepName::Unnamed(EditKind::Structural),
            serial: 0,
            pending: self.pending.clone(),
        }
    }

    fn push_undo(&mut self, mut snapshot: Snapshot) {
        match self.grouping {
            Some(true) => return,
            Some(false) => self.grouping = Some(true),
            None => {}
        }
        snapshot.serial = UNDO_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        LAST_SERIAL.with(|c| c.set(snapshot.serial));
        self.undo.push_back(snapshot);
        self.redo.clear();
        // The oldest steps go past the cap, or past the memory budget while
        // more than the floor are left.
        let mut weight: usize = self.undo.iter().map(|s| s.weight).sum();
        while self.undo.len() > UNDO_CAP
            || (weight > self.history_budget && self.undo.len() > UNDO_FLOOR)
        {
            let Some(oldest) = self.undo.pop_front() else {
                break;
            };
            weight -= oldest.weight;
        }
    }

    fn cur_len(&self) -> usize {
        resolve_para(&self.doc.body, &self.caret.path)
            .map(para_text_len)
            .unwrap_or(0)
    }

    /// Push an undo step for an edit of `kind`, unless it continues the
    /// newest step: only typing coalesces. Each Backspace or Delete is a step
    /// of its own, as in Word (#853).
    fn checkpoint(&mut self, kind: EditKind) {
        if self.grouped() {
            self.last = kind;
            return;
        }
        if self.last != kind || matches!(kind, EditKind::Structural | EditKind::Delete) {
            let mut snapshot = self.snapshot();
            snapshot.name = match kind {
                EditKind::Insert => StepName::Typing(String::new()),
                kind => StepName::Unnamed(kind),
            };
            self.push_undo(snapshot);
        }
        self.last = kind;
    }

    /// The current state as the entry that undoing or redoing `step` leaves
    /// on the other stack: the same step, so it keeps its name and serial,
    /// and it shares the blocks the step did not change.
    fn snapshot_as(&self, step: &Snapshot) -> Snapshot {
        Snapshot {
            name: step.name.clone(),
            serial: step.serial,
            ..self.snapshot_like(Some(step))
        }
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The undo steps' names, newest first (Word's Undo drop-down, #619).
    pub fn undo_names(&self) -> Vec<String> {
        self.undo.iter().rev().map(|s| s.name.label()).collect()
    }

    /// The newest undo step's serial, or `None` with nothing to undo. Redo
    /// brings a step back with its serial, so a host can tell whether the
    /// step it recorded is still the newest one.
    pub fn undo_serial(&self) -> Option<u64> {
        self.undo.back().map(|s| s.serial)
    }

    /// The undo steps pushed after `since` (a value of
    /// [`undo_serial_counter`] taken before a command ran) are one command's:
    /// collapse them into one step, the oldest (its snapshot is the state
    /// before the command, and it keeps its serial), named `name`. One Undo
    /// then undoes the whole command. Steps from before, such as a typing
    /// step the command did not start, are left alone. Typing after a named
    /// step starts a step of its own rather than growing it.
    pub fn name_command(&mut self, since: u64, name: &str) {
        let first = self
            .undo
            .iter()
            .rposition(|step| step.serial <= since)
            .map_or(0, |i| i + 1);
        if first == self.undo.len() {
            return;
        }
        self.undo.truncate(first + 1);
        self.undo[first].name = StepName::Named(name.into());
        self.last = EditKind::None;
    }

    /// Run `edit` as one command: the undo steps it pushes become one, named
    /// `name` (see [`Editor::name_command`]), apart from any typing before
    /// it. For a host command made of several edits, such as a counted
    /// delete or text inserted as if typed, which would otherwise be a step
    /// per Delete or per 128 characters (#853).
    ///
    /// Only the first edit that takes a step does (the state before the
    /// command); the edits after it build no snapshot at all, so a long
    /// command costs one copy of the document and can never push its own
    /// first step out of the history. Typing inside it is not split at 128
    /// characters.
    pub fn one_step(&mut self, name: &str, edit: impl FnOnce(&mut Self)) {
        if self.grouping.is_some() {
            edit(self);
            return;
        }
        /// Ends the grouping however `edit` exits, a panic too, and with it
        /// the typing run, so the editor goes on taking steps.
        struct Ungroup<'a>(&'a mut Editor);
        impl Drop for Ungroup<'_> {
            fn drop(&mut self) {
                self.0.grouping = None;
                self.0.last = EditKind::None;
            }
        }
        let since = undo_serial_counter();
        self.break_undo_group();
        self.grouping = Some(false);
        let group = Ungroup(self);
        edit(&mut *group.0);
        drop(group);
        self.name_command(since, name);
    }

    /// Inside [`Editor::one_step`] with its step taken: an edit takes no
    /// snapshot at all.
    fn grouped(&self) -> bool {
        self.grouping == Some(true)
    }

    /// The state before a transaction that pushes its step only if it
    /// changed something ([`Editor::finish_review_transaction`]); none when
    /// grouped, whose step is taken already.
    fn transaction_start(&self) -> Option<Snapshot> {
        (!self.grouped()).then(|| self.snapshot())
    }

    /// The text of the newest undo step when it is typing.
    pub fn last_typed(&self) -> Option<&str> {
        match self.undo.back().map(|s| &s.name) {
            Some(StepName::Typing(text)) => Some(text),
            _ => None,
        }
    }

    /// End the current typing run, so the next character typed starts a new
    /// undo step instead of coalescing into the newest one.
    pub fn break_undo_group(&mut self) {
        self.last = EditKind::None;
    }

    /// Undo the `n` newest steps in one call (choosing an entry in the Undo
    /// drop-down, #619). They go onto the redo stack in order, so Redo then
    /// brings them back one at a time. False, and no change, for `n == 0`
    /// or more steps than there are.
    pub fn undo_to(&mut self, n: usize) -> bool {
        if n == 0 || n > self.undo.len() {
            return false;
        }
        for _ in 0..n {
            self.undo();
        }
        true
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo.pop_back() {
            self.redo.push(self.snapshot_as(&prev));
            self.doc = unshare_body(prev.body);
            self.caret = prev.caret;
            self.anchor = prev.anchor;
            self.review_target = prev.review_target;
            self.pending = prev.pending;
            self.last = EditKind::None;
            self.refresh_merge_preview();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push_back(self.snapshot_as(&next));
            self.doc = unshare_body(next.body);
            self.caret = next.caret;
            self.anchor = next.anchor;
            self.review_target = next.review_target;
            self.pending = next.pending;
            self.last = EditKind::None;
            self.refresh_merge_preview();
            true
        } else {
            false
        }
    }

    // ---- tracked-change review ----

    /// Enumerate current revisions in source order with editor-safe locations.
    pub fn revision_locations(&self) -> Vec<RevisionLocation> {
        let addresses = self.doc.revisions();
        let positions = collect_revision_positions(&self.doc.body);
        let fallback = first_paragraph_path(&self.doc.body).map(|path| Caret { path, offset: 0 });

        addresses
            .into_iter()
            .filter_map(|address| {
                let position = positions
                    .iter()
                    .find(|position| position.target == address.target)
                    .cloned()
                    .or_else(|| {
                        fallback.as_ref().map(|caret| RevisionPosition {
                            target: address.target,
                            start: caret.clone(),
                            end: caret.clone(),
                        })
                    })?;
                Some(RevisionLocation {
                    address,
                    start: position.start,
                    end: position.end,
                })
            })
            .collect()
    }

    /// The selected revision, or the first revision whose range contains the
    /// caret when navigation has not selected a specific target.
    pub fn current_revision(&self) -> Option<RevisionLocation> {
        let locations = self.revision_locations();
        if let Some(target) = self.review_target {
            if let Some(location) = locations
                .iter()
                .find(|location| location.address.target == target)
                .filter(|location| location.contains(&self.caret))
            {
                return Some(location.clone());
            }
        }
        locations
            .into_iter()
            .find(|location| location.contains(&self.caret))
    }

    /// Select a stable revision target and move the caret to its review range.
    pub fn select_revision(&mut self, target: RevisionTarget) -> Option<RevisionLocation> {
        let location = self
            .revision_locations()
            .into_iter()
            .find(|location| location.address.target == target)?;
        self.caret = location.start.clone();
        self.anchor = None;
        self.review_target = Some(target);
        self.last = EditKind::None;
        Some(location)
    }

    /// Move to the next revision in source order, wrapping at the document end.
    pub fn next_revision(&mut self) -> Option<RevisionLocation> {
        self.navigate_revision(false)
    }

    /// Move to the previous revision in source order, wrapping at the start.
    pub fn previous_revision(&mut self) -> Option<RevisionLocation> {
        self.navigate_revision(true)
    }

    fn navigate_revision(&mut self, reverse: bool) -> Option<RevisionLocation> {
        let locations = self.revision_locations();
        if locations.is_empty() {
            self.review_target = None;
            return None;
        }
        let paths = all_paragraph_paths(&self.doc.body);
        let key = |caret: &Caret| {
            (
                paths
                    .iter()
                    .position(|path| *path == caret.path)
                    .unwrap_or(usize::MAX),
                caret.offset,
            )
        };
        let current = self
            .review_target
            .and_then(|target| {
                locations.iter().position(|location| {
                    location.address.target == target && location.contains(&self.caret)
                })
            })
            .or_else(|| {
                locations
                    .iter()
                    .position(|location| location.contains(&self.caret))
            });
        let index = if let Some(current) = current {
            if reverse {
                current.checked_sub(1).unwrap_or(locations.len() - 1)
            } else {
                (current + 1) % locations.len()
            }
        } else {
            let caret_key = key(&self.caret);
            if reverse {
                locations
                    .iter()
                    .rposition(|location| key(&location.start) < caret_key)
                    .unwrap_or(locations.len() - 1)
            } else {
                locations
                    .iter()
                    .position(|location| key(&location.start) > caret_key)
                    .unwrap_or(0)
            }
        };
        let location = locations[index].clone();
        self.caret = location.start.clone();
        self.anchor = None;
        self.review_target = Some(location.address.target);
        self.last = EditKind::None;
        Some(location)
    }

    /// Accept one stable revision as one native undo transaction.
    pub fn accept_revision(&mut self, target: RevisionTarget) -> RevisionOutcome {
        self.apply_revision_action(target, RevisionAction::Accept)
    }

    /// Reject one stable revision as one native undo transaction.
    pub fn reject_revision(&mut self, target: RevisionTarget) -> RevisionOutcome {
        self.apply_revision_action(target, RevisionAction::Reject)
    }

    /// Accept the revision selected by navigation or located at the caret.
    pub fn accept_current_revision(&mut self) -> Option<RevisionOutcome> {
        let target = self.current_revision()?.address.target;
        Some(self.accept_revision(target))
    }

    /// Reject the revision selected by navigation or located at the caret.
    pub fn reject_current_revision(&mut self) -> Option<RevisionOutcome> {
        let target = self.current_revision()?.address.target;
        Some(self.reject_revision(target))
    }

    /// Accept all current revisions as one native undo transaction.
    pub fn accept_all_revisions(&mut self) -> Vec<RevisionOutcome> {
        self.apply_all_revision_actions(RevisionAction::Accept)
    }

    /// Reject all current revisions as one native undo transaction.
    pub fn reject_all_revisions(&mut self) -> Vec<RevisionOutcome> {
        self.apply_all_revision_actions(RevisionAction::Reject)
    }

    /// Document Inspector: remove every hidden run, tab and break
    /// ([`crate::inspect::remove_hidden_text`]) as one undo step, none when
    /// nothing was hidden. Returns how many were removed.
    pub fn remove_hidden_text(&mut self) -> usize {
        let before = self.transaction_start();
        let removed = crate::inspect::remove_hidden_text(&mut self.doc);
        self.finish_review_transaction(before);
        removed
    }

    /// Document Inspector: remove the markers of every comment
    /// ([`crate::inspect::remove_all_comment_markers`]) as one undo step, none
    /// when there were none. Returns how many were removed.
    pub fn remove_all_comment_markers(&mut self) -> usize {
        let before = self.transaction_start();
        let removed = crate::inspect::remove_all_comment_markers(&mut self.doc);
        self.finish_review_transaction(before);
        removed
    }

    fn apply_revision_action(
        &mut self,
        target: RevisionTarget,
        action: RevisionAction,
    ) -> RevisionOutcome {
        let before = self.transaction_start();
        let outcome = self.doc.apply_revision_action(target, action);
        self.finish_review_transaction(before);
        outcome
    }

    fn apply_all_revision_actions(&mut self, action: RevisionAction) -> Vec<RevisionOutcome> {
        let before = self.transaction_start();
        let outcomes = match action {
            RevisionAction::Accept => self.doc.accept_all_revisions(),
            RevisionAction::Reject => self.doc.reject_all_revisions(),
        };
        self.finish_review_transaction(before);
        outcomes
    }

    fn finish_review_transaction(&mut self, before: Option<Snapshot>) {
        let Some(before) = before else {
            self.last = EditKind::None;
            self.clamp();
            return;
        };
        let same = self.doc.body.len() == before.body.len()
            && self
                .doc
                .body
                .iter()
                .zip(&before.body)
                .all(|(b, s)| b == s.as_ref());
        if same {
            return;
        }
        self.push_undo(before);
        self.last = EditKind::None;
        self.clamp();
    }

    pub fn insert_char(&mut self, ch: char) {
        // A field's stand-in is not text: typing it does nothing at all.
        if ch == FIELD_CHAR {
            return;
        }
        self.drop_collapsed_anchor();
        // Read before the typing step is pushed, which leaves it behind (that
        // step keeps it, so undoing the typing brings it back); text typed
        // after this character takes the character's own props.
        let pending = self.pending_format().cloned();
        let pushed_before = undo_serial_counter();
        if self.has_selection() {
            self.delete_selection();
        }
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        // Typing over a selection is one step, as in Word: the step the
        // deletion pushed (its snapshot is the state before it) becomes the
        // typing step, and the characters coalesce into it.
        if undo_serial_counter() > pushed_before {
            if let Some(step) = self.undo.back_mut() {
                step.name = StepName::Typing(String::new());
                self.last = EditKind::Insert;
            }
        }
        // A full typing step is closed: the character starts the next (#853).
        // Not inside one_step, whose one step holds all its text.
        if self.grouping.is_none()
            && self.last == EditKind::Insert
            && self
                .last_typed()
                .is_some_and(|text| text.chars().count() >= TYPING_STEP_CHARS)
        {
            self.last = EditKind::None;
        }
        self.checkpoint(EditKind::Insert);
        let off = self.caret.offset;
        if ch == LINE_BREAK {
            // A break is an inline of its own, formatted as typing here
            // would be (see `insert_break`), and part of the typing step.
            let mut props = resolve_para(&self.doc.body, &self.caret.path)
                .map(|p| tab_props_at(&p.content, off))
                .unwrap_or_default();
            if let Some(pending) = &pending {
                pending.apply(&mut props);
            }
            self.paste_at_caret(&Clip {
                paras: vec![vec![Inline::Break(BreakKind::Line, props)]],
            });
            self.settle_revisions();
            if self.caret.offset == off {
                return;
            }
        } else if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            content_insert(&mut p.content, off, ch);
            // The toggles go on before the change is recorded, which may
            // set the props of a tracked insertion over them.
            if let Some(pending) = &pending {
                map_prop_range(&mut p.content, off, off + 1, &|props| pending.apply(props));
            }
            self.caret.offset += 1;
            let path = self.caret.path.clone();
            self.settle_inserted(&path, off, 1);
        } else {
            return;
        }
        if let Some(StepName::Typing(text)) = self.undo.back_mut().map(|s| &mut s.name) {
            text.push(ch);
        }
    }

    /// Shift+Enter: a manual line break at the caret. Typed as Word types it,
    /// inside the typing step around it (#853), so one Undo takes back the
    /// text before and after it with it.
    pub fn insert_line_break(&mut self) {
        self.insert_char(LINE_BREAK);
    }

    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert_char(ch);
        }
    }

    /// Insert a tab (`<w:tab/>`) at the caret. A tab is its own inline in the
    /// model (not a `\t` character in run text), so it advances to the next tab
    /// stop when rendered — a literal `\t` would collapse to nothing.
    ///
    /// The tab takes the formatting typing at the caret would (a tab typed in
    /// bold text is bold, as in Word), so text typed after it keeps that too,
    /// except next to a hyperlink: the tab never lands inside the link, so it
    /// takes the formatting before the link, not the link's style (see
    /// `tab_props_at`).
    pub fn insert_tab(&mut self) {
        let pending = self.pending_format().cloned();
        if self.has_selection() {
            self.delete_selection();
        }
        let mut props = resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| tab_props_at(&p.content, self.caret.offset))
            .unwrap_or_default();
        if let Some(pending) = &pending {
            pending.apply(&mut props);
        }
        self.paste(&Clip {
            paras: vec![vec![Inline::Tab(props)]],
        });
    }

    pub fn insert_newline(&mut self) {
        // Only a collapsed anchor: `insert_hrule` moves the caret to the end
        // of the paragraph before calling this, so deleting a real selection
        // here would delete from the anchor to the paragraph end.
        self.drop_collapsed_anchor();
        self.checkpoint(EditKind::Structural);
        let off = self.caret.offset;
        let in_cover = self.caret_in_cover();
        let new_idx = {
            let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
                return;
            };
            let Some(Block::Paragraph(p)) = cont.get_mut(idx) else {
                return;
            };
            let right = split_paragraph_at(&mut p.content, off, in_cover).0;
            let props = p.props.clone();
            // A section break ends the section after the split, so it (and
            // its tracked change, which Save also writes as a sectPr) moves
            // with the paragraph's second half; Word keeps the section mark
            // last (#748).
            p.props.section_break = None;
            p.props.section_property_change = None;
            // Likewise a tracked insertion/deletion of the paragraph mark:
            // the physical mark stays at the end, on the second half.
            crate::review::clear_mark_revisions(&mut p.props);
            cont.insert(
                idx + 1,
                Block::Paragraph(Paragraph {
                    props,
                    content: right,
                }),
            );
            idx + 1
        };
        if let Some(last) = self.caret.path.last_mut() {
            *last = new_idx;
        }
        self.caret.offset = 0;
        self.settle_revisions();
    }

    /// Word-style autoformat: if the current paragraph's whole text is three or
    /// more of the same border character (`-` `_` `=` `*` `~` `#`), turn it into a
    /// horizontal rule (a bottom paragraph border) and move to a fresh paragraph
    /// below. Returns true if it fired (so the caller skips the normal newline).
    pub fn hrule_autoformat(&mut self) -> bool {
        let kind = match para_mut(&mut self.doc.body, &self.caret.path) {
            Some(p) => match hrule_kind(&p.plain_text()) {
                Some(k) => k,
                None => return false,
            },
            None => return false,
        };
        // Before the caret moves to 0, or `insert_newline` would no longer
        // see the anchor as collapsed.
        self.drop_collapsed_anchor();
        self.checkpoint(EditKind::Structural);
        if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            p.content.clear();
            p.props.borders.bottom = Some(kind);
        }
        self.caret.offset = 0;
        // A fresh paragraph below for the caret, without inheriting the rule.
        self.insert_newline();
        if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            p.props.borders = ParBorders::default();
        }
        true
    }

    /// Insert a horizontal line at the caret (Insert ▸ Horizontal Line): give the
    /// current paragraph a bottom border, then drop to a fresh paragraph below.
    pub fn insert_hrule(&mut self) {
        // Before `move_end` moves the caret off a click's anchor.
        self.drop_collapsed_anchor();
        self.checkpoint(EditKind::Structural);
        if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            p.props.borders.bottom = Some(BorderKind::Single);
        }
        self.move_end();
        self.insert_newline();
        if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            p.props.borders = ParBorders::default();
        }
    }

    /// Insert a math equation from LaTeX at the caret. Generates OMML (so it
    /// saves as real Word math), renders Unicode for display, and keeps the
    /// LaTeX source. `display` selects a block (`oMathPara`) over an inline one.
    pub fn insert_equation(&mut self, latex: &str, display: bool) {
        let raw = crate::latex::latex_to_omml(latex, display);
        let text = crate::omath::render_omath(&raw);
        let inl = Inline::Equation {
            raw,
            text,
            latex: Some(latex.to_string()),
        };
        self.paste(&Clip {
            paras: vec![vec![inl]],
        });
    }

    /// If the character at editor offset `idx` of the caret's paragraph is a
    /// field ([`is_field_unit`]), select it and return true: the first
    /// Backspace or Delete next to a field selects the whole field, as in Word,
    /// and the next press deletes it. Selecting is not an edit, so it pushes no
    /// undo step. The caret keeps its side of the field.
    fn select_field_unit(&mut self, idx: usize) -> bool {
        let is_field = resolve_para(&self.doc.body, &self.caret.path)
            .and_then(|p| inline_covering(&p.content, idx))
            .is_some_and(is_field_unit);
        if is_field {
            let other = if self.caret.offset == idx {
                idx + 1
            } else {
                idx
            };
            self.anchor = Some(Caret {
                path: self.caret.path.clone(),
                offset: other,
            });
            self.last = EditKind::None;
        }
        is_field
    }

    pub fn backspace(&mut self) {
        self.drop_collapsed_anchor();
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if self.caret.offset > 0 {
            let off = self.caret.offset;
            if self.select_field_unit(off - 1) {
                return;
            }
            self.checkpoint(EditKind::Delete);
            let path = self.caret.path.clone();
            self.delete_char_at(&path, off - 1);
            self.caret.offset -= 1;
            return;
        }
        // At the start of a paragraph: merge into the previous sibling
        // paragraph, if there is one; with none it is no edit, and no step.
        if !self.has_sibling_paragraph(false) {
            self.last = EditKind::None;
            return;
        }
        self.checkpoint(EditKind::Structural);
        let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
            return;
        };
        let gone = cont.remove(idx);
        let prev_len = match (&mut cont[idx - 1], gone) {
            (Block::Paragraph(prev), Block::Paragraph(gone)) => {
                let len = para_text_len(prev);
                join_paragraph_content(&mut prev.content, gone.content);
                keep_section_mark(&mut prev.props, gone.props);
                len
            }
            _ => 0,
        };
        if let Some(last) = self.caret.path.last_mut() {
            *last = idx - 1;
        }
        self.caret.offset = prev_len;
    }

    /// Whether the caret's paragraph has a paragraph right after it (`after`)
    /// or right before it in its container, for Delete or Backspace to merge.
    fn has_sibling_paragraph(&self, after: bool) -> bool {
        let Some((cont, idx)) = container(&self.doc.body, &self.caret.path) else {
            return false;
        };
        let sibling = if after {
            idx.checked_add(1)
        } else {
            idx.checked_sub(1)
        };
        matches!(sibling.and_then(|i| cont.get(i)), Some(Block::Paragraph(_)))
    }

    pub fn delete_forward(&mut self) {
        self.drop_collapsed_anchor();
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let off = self.caret.offset;
        if off < self.cur_len() {
            if self.select_field_unit(off) {
                return;
            }
            self.checkpoint(EditKind::Delete);
            let path = self.caret.path.clone();
            self.delete_char_at(&path, off);
            return;
        }
        // At the end: pull up the next sibling paragraph, if there is one.
        if !self.has_sibling_paragraph(true) {
            self.last = EditKind::None;
            return;
        }
        self.checkpoint(EditKind::Structural);
        let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
            return;
        };
        let gone = cont.remove(idx + 1);
        if let (Block::Paragraph(p), Block::Paragraph(gone)) = (&mut cont[idx], gone) {
            join_paragraph_content(&mut p.content, gone.content);
            keep_section_mark(&mut p.props, gone.props);
        }
    }

    // ---- movement ----

    pub fn move_left(&mut self) {
        self.last = EditKind::None;
        if self.caret.offset > 0 {
            self.caret.offset -= 1;
            return;
        }
        let paths = all_paragraph_paths(&self.doc.body);
        if let Some(pos) = paths.iter().position(|p| *p == self.caret.path) {
            if pos > 0 {
                self.caret.path = paths[pos - 1].clone();
                self.caret.offset = resolve_para(&self.doc.body, &self.caret.path)
                    .map(para_text_len)
                    .unwrap_or(0);
            }
        }
    }

    pub fn move_right(&mut self) {
        self.last = EditKind::None;
        if self.caret.offset < self.cur_len() {
            self.caret.offset += 1;
            return;
        }
        let paths = all_paragraph_paths(&self.doc.body);
        if let Some(pos) = paths.iter().position(|p| *p == self.caret.path) {
            if pos + 1 < paths.len() {
                self.caret.path = paths[pos + 1].clone();
                self.caret.offset = 0;
            }
        }
    }

    /// Move to the start of the previous word (Ctrl-Left), crossing paragraphs.
    pub fn move_word_left(&mut self) {
        self.last = EditKind::None;
        if self.caret.offset == 0 {
            self.move_left();
            return;
        }
        let text: Vec<char> = self.cur_text().chars().collect();
        let mut o = self.caret.offset.min(text.len());
        while o > 0 && text[o - 1].is_whitespace() {
            o -= 1;
        }
        while o > 0 && !text[o - 1].is_whitespace() {
            o -= 1;
        }
        self.caret.offset = o;
    }

    /// Move to the start of the next word (Ctrl-Right), crossing paragraphs.
    pub fn move_word_right(&mut self) {
        self.last = EditKind::None;
        let text: Vec<char> = self.cur_text().chars().collect();
        let len = text.len();
        if self.caret.offset >= len {
            self.move_right();
            return;
        }
        let mut o = self.caret.offset;
        while o < len && !text[o].is_whitespace() {
            o += 1;
        }
        while o < len && text[o].is_whitespace() {
            o += 1;
        }
        self.caret.offset = o;
    }

    /// Move to the end of the current/next word (vim `e`).
    pub fn move_word_end(&mut self) {
        self.last = EditKind::None;
        let text: Vec<char> = self.cur_text().chars().collect();
        let len = text.len();
        if self.caret.offset >= len {
            self.move_right();
            return;
        }
        let mut o = self.caret.offset + 1;
        while o < len && text[o].is_whitespace() {
            o += 1;
        }
        while o + 1 < len && !text[o + 1].is_whitespace() {
            o += 1;
        }
        self.caret.offset = o.min(len);
    }

    /// Select `count` whole paragraphs starting at the caret (vim linewise).
    pub fn select_lines(&mut self, count: usize) {
        let count = count.max(1);
        let paths = all_paragraph_paths(&self.doc.body);
        let cur = paths
            .iter()
            .position(|p| *p == self.caret.path)
            .unwrap_or(0);
        self.anchor = Some(Caret {
            path: paths[cur].clone(),
            offset: 0,
        });
        let end_idx = cur + count;
        if end_idx < paths.len() {
            self.caret = Caret {
                path: paths[end_idx].clone(),
                offset: 0,
            };
        } else {
            let last = paths.last().cloned().unwrap_or_else(|| paths[cur].clone());
            let end = resolve_para(&self.doc.body, &last)
                .map(para_text_len)
                .unwrap_or(0);
            self.caret = Caret {
                path: last,
                offset: end,
            };
        }
    }

    /// The caret paragraph's text in editor offsets (see [`editor_text`]).
    fn cur_text(&self) -> String {
        resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| editor_text(&p.content))
            .unwrap_or_default()
    }

    pub fn move_doc_start(&mut self) {
        self.last = EditKind::None;
        if let Some(p) = first_paragraph_path(&self.doc.body) {
            self.caret = Caret { path: p, offset: 0 };
        }
    }

    pub fn move_doc_end(&mut self) {
        self.last = EditKind::None;
        let paths = all_paragraph_paths(&self.doc.body);
        if let Some(last) = paths.last() {
            let end = resolve_para(&self.doc.body, last)
                .map(para_text_len)
                .unwrap_or(0);
            self.caret = Caret {
                path: last.clone(),
                offset: end,
            };
        }
    }

    pub fn set_caret(&mut self, caret: Caret) {
        self.last = EditKind::None;
        self.caret = caret;
        self.clamp();
    }

    pub fn move_home(&mut self) {
        self.last = EditKind::None;
        self.caret.offset = 0;
    }

    pub fn move_end(&mut self) {
        self.last = EditKind::None;
        self.caret.offset = self.cur_len();
    }

    /// Clamp the caret to a valid position.
    pub fn clamp(&mut self) {
        clamp_caret(&self.doc.body, &mut self.caret);
        if let Some(anchor) = &mut self.anchor {
            clamp_caret(&self.doc.body, anchor);
        }
        if self
            .review_target
            .is_some_and(|target| self.doc.revision(target).is_none())
        {
            self.review_target = None;
        }
    }

    // ---- selection ----

    /// Begin/extend (on=true) or clear (on=false) the selection. The app calls
    /// this before a movement, based on whether Shift is held.
    pub fn extend_selection(&mut self, on: bool) {
        if on {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret.clone());
            }
        } else {
            self.anchor = None;
        }
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Forget an anchor that sits on the caret. A click plants one there so a
    /// drag can extend from it, and it is no selection; but an edit moves the
    /// caret and would leave it behind, turning what was just typed (or the
    /// paragraph mark Enter made) into a selection the next key replaces.
    /// Every edit that moves the caret through content calls this first,
    /// before its undo checkpoint and before it moves the caret (so undo
    /// restores a plain caret), including the ones that reach
    /// `insert_newline` only after moving it (`insert_hrule`,
    /// `hrule_autoformat`).
    fn drop_collapsed_anchor(&mut self) {
        if self.anchor.as_ref() == Some(&self.caret) {
            self.anchor = None;
        }
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    fn order_key(&self, c: &Caret, paths: &[Vec<usize>]) -> (usize, usize) {
        (
            paths
                .iter()
                .position(|p| *p == c.path)
                .unwrap_or(usize::MAX),
            c.offset,
        )
    }

    /// The selection as an ordered (low, high) caret pair, or None if empty.
    pub fn selection_range(&self) -> Option<(Caret, Caret)> {
        let a = self.anchor.as_ref()?;
        if *a == self.caret {
            return None;
        }
        let paths = all_paragraph_paths(&self.doc.body);
        if self.order_key(a, &paths) <= self.order_key(&self.caret, &paths) {
            Some((a.clone(), self.caret.clone()))
        } else {
            Some((self.caret.clone(), a.clone()))
        }
    }

    /// The selection split per paragraph: `(path, start_offset, end_offset)`.
    ///
    /// A selection across cells of one table (see [`Editor::cell_range`]) is
    /// the rectangle of cells: every paragraph of each, whole, empty ones
    /// included, and nothing from the cells between them in reading order.
    pub fn selection_spans(&self) -> Vec<(Vec<usize>, usize, usize)> {
        if let Some(range) = self.cell_range() {
            return self.cell_range_spans(&range);
        }
        let Some((lo, hi)) = self.selection_range() else {
            return Vec::new();
        };
        let paths = all_paragraph_paths(&self.doc.body);
        let lo_i = paths.iter().position(|p| *p == lo.path).unwrap_or(0);
        let hi_i = paths.iter().position(|p| *p == hi.path).unwrap_or(0);
        let mut out = Vec::new();
        for path in paths.iter().take(hi_i + 1).skip(lo_i) {
            let len = resolve_para(&self.doc.body, path)
                .map(para_text_len)
                .unwrap_or(0);
            let s = if *path == lo.path { lo.offset } else { 0 };
            let e = if *path == hi.path { hi.offset } else { len };
            if e > s {
                out.push((path.clone(), s, e));
            }
        }
        out
    }

    /// Delete the current selection. Handles a single paragraph and a range of
    /// sibling paragraphs (merging the ends). A selection spanning different
    /// containers (e.g. body into a table cell) just collapses to the start.
    ///
    /// Over a cell range it empties the selected cells, as Word's Delete does.
    pub fn delete_selection(&mut self) -> bool {
        if let Some(range) = self.cell_range() {
            return self.clear_cells(&range);
        }
        let Some((mut lo, hi)) = self.selection_range() else {
            return false;
        };
        // The anchor goes after the checkpoint, so that undoing the delete
        // selects the text again, as in Word (#853).

        if lo.path == hi.path {
            self.checkpoint(EditKind::Structural);
            self.anchor = None;
            for _ in lo.offset..hi.offset {
                self.delete_char_at(&lo.path, lo.offset);
            }
            self.caret = lo;
            return true;
        }

        // Same container (siblings)? Compare the parent path.
        let same_container = lo.path.len() == hi.path.len()
            && lo.path[..lo.path.len() - 1] == hi.path[..hi.path.len() - 1];
        if !same_container {
            self.anchor = None;
            self.caret = lo; // cross-container: collapse (rare)
            return false;
        }

        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        // Tracked: the text of each paragraph is recorded as deleted and the
        // paragraph marks stay (see the `track` module).
        if self.delete_text_across_paragraphs(&lo, &hi) {
            self.caret = lo;
            return true;
        }
        let li = *lo.path.last().unwrap();
        let hii = *hi.path.last().unwrap();
        if let Some((cont, _)) = container_mut(&mut self.doc.body, &lo.path) {
            // A block content control (a placed page number, a cover page)
            // that loses one boundary to this delete goes as a control; what
            // is left of its content stays. Pair the boundaries now, before
            // the range goes, so each drop is the removed one's own partner.
            let partners = crate::hf::sdt_partners_outside(cont, li + 1, hii);
            // Truncate the first paragraph at lo.offset.
            if let Some(Block::Paragraph(p)) = cont.get_mut(li) {
                let len: usize = p.content.iter().map(inline_len).sum();
                for _ in lo.offset..len {
                    content_delete(&mut p.content, lo.offset);
                }
            }
            // Take the remainder of the last paragraph (after hi.offset),
            // and its props for the section mark it may carry.
            let (remainder, last_props) = if let Some(Block::Paragraph(p)) = cont.get_mut(hii) {
                for _ in 0..hi.offset {
                    content_delete(&mut p.content, 0);
                }
                (
                    std::mem::take(&mut p.content),
                    Some(std::mem::take(&mut p.props)),
                )
            } else {
                (Vec::new(), None)
            };
            // Remove everything strictly between (and the now-empty last).
            let removed = (hii + 1).min(cont.len()).saturating_sub(li + 1);
            for _ in (li + 1)..=hii {
                if li + 1 < cont.len() {
                    cont.remove(li + 1);
                }
            }
            // Merge the remainder onto the first paragraph.
            if let Some(Block::Paragraph(p)) = cont.get_mut(li) {
                join_paragraph_content(&mut p.content, remainder);
                if let Some(gone) = last_props {
                    keep_section_mark(&mut p.props, gone);
                }
            }
            // Then the partners, from the back; those after the range moved
            // up by what it held. The caret's paragraph moves up by the ones
            // before it.
            for &i in partners.iter().rev() {
                let at = if i > hii { i - removed } else { i };
                if at < cont.len() {
                    cont.remove(at);
                }
            }
            let shift = partners.iter().filter(|&&i| i < li).count();
            if let Some(last) = lo.path.last_mut() {
                *last -= shift;
            }
        }
        self.caret = lo;
        true
    }

    /// Select the whole document.
    pub fn select_all(&mut self) {
        let paths = all_paragraph_paths(&self.doc.body);
        if let (Some(first), Some(last)) = (paths.first(), paths.last()) {
            self.anchor = Some(Caret {
                path: first.clone(),
                offset: 0,
            });
            let end = resolve_para(&self.doc.body, last)
                .map(para_text_len)
                .unwrap_or(0);
            self.caret = Caret {
                path: last.clone(),
                offset: end,
            };
            self.last = EditKind::None;
        }
    }

    // ---- clipboard ----

    /// Copy the current selection into a [`Clip`] (preserving run styling).
    pub fn copy(&self) -> Option<Clip> {
        let spans = self.selection_spans();
        if spans.is_empty() {
            return None;
        }
        let mut paras = Vec::new();
        for (path, s, e) in spans {
            if let Some(p) = resolve_para(&self.doc.body, &path) {
                let mut inlines = extract_range(&p.content, s, e);
                // A copy of a recorded insertion is plain text, not a second
                // record of it.
                for inline in &mut inlines {
                    track::with_props(inline, track::clear_insert_record);
                }
                paras.push(inlines);
            }
        }
        // A previewed record is display only: copied merge fields carry
        // their placeholders, as a save would.
        if self.merge_preview.is_some() {
            for para in &mut paras {
                crate::merge::preview::unpreview_inlines(para);
            }
        }
        Some(Clip { paras })
    }

    /// Cut: copy the selection, then delete it.
    pub fn cut(&mut self) -> Option<Clip> {
        let clip = self.copy()?;
        self.delete_selection();
        Some(clip)
    }

    /// Paste a [`Clip`] at the caret (replacing any selection). Pasting inside
    /// a hyperlink splits it, so the pasted content lands between two links
    /// to the same target, not inside the link (see `split_content`).
    pub fn paste(&mut self, clip: &Clip) {
        if clip.paras.is_empty() {
            return;
        }
        self.drop_collapsed_anchor();
        if self.has_selection() {
            self.delete_selection();
        }
        self.checkpoint(EditKind::Structural);
        self.paste_at_caret(clip);
        self.settle_revisions();
    }

    /// [`Editor::paste`]'s insertion at the caret, with no undo step of its
    /// own: a caller that inserts several pieces as one edit checkpoints once.
    fn paste_at_caret(&mut self, clip: &Clip) {
        if clip.paras.is_empty() {
            return;
        }
        // Recorded as one tracked insertion when tracking; a copy of recorded
        // text is not itself a record when not.
        let recorded = self.clip_for_insertion(clip);
        let clip = &recorded;
        let off = self.caret.offset;
        let n = clip.paras.len();
        let in_cover = self.caret_in_cover();

        if n == 1 {
            if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
                // Into an empty content control at the caret, or the end of a
                // cover placeholder, as typing goes.
                let ins_len =
                    insert_inlines(&mut p.content, off, clip.paras[0].clone(), false, in_cover);
                self.caret.offset = off + ins_len;
            }
            self.doc.initialize_revision_targets();
            return;
        }

        let placed = {
            let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
                return;
            };
            let Some(Block::Paragraph(p)) = cont.get_mut(idx) else {
                return;
            };
            // Only the last paragraph, which gets the tail, keeps a section
            // break and its tracked change: the section still ends there (#748).
            let props = p.props.clone();
            p.props.section_break = None;
            p.props.section_property_change = None;
            // As does a tracked change of the paragraph mark.
            crate::review::clear_mark_revisions(&mut p.props);
            let inner = p.props.clone();
            // The first piece goes in at the caret as a one-paragraph paste
            // does (into an emptied content control there), and the paragraph
            // splits after it: the content controls the split continues are
            // closed after it and open again around the last piece.
            let first_len =
                insert_inlines(&mut p.content, off, clip.paras[0].clone(), true, in_cover);
            let (tail, inside) = split_paragraph_at(&mut p.content, off + first_len, in_cover);

            // A middle piece pasted inside content controls is inside them
            // too: a copy of each opens and closes around it.
            let close = || Inline::Raw(crate::load::SDT_BLOCK_CLOSE.to_string());
            let mut news: Vec<Block> = Vec::new();
            for mid in &clip.paras[1..n - 1] {
                let mut content = tail[..inside].to_vec();
                content.extend(mid.iter().cloned());
                content.extend((0..inside).map(|_| close()));
                news.push(Block::Paragraph(Paragraph {
                    props: inner.clone(),
                    content,
                }));
            }
            let last_pasted = clip.paras[n - 1].clone();
            let last_len: usize = last_pasted.iter().map(inline_len).sum();
            let mut last_content = tail;
            last_content.splice(inside..inside, last_pasted);
            news.push(Block::Paragraph(Paragraph {
                props,
                content: last_content,
            }));

            let count = news.len();
            for (k, b) in news.into_iter().enumerate() {
                cont.insert(idx + 1 + k, b);
            }
            (idx + count, last_len)
        };
        if let Some(l) = self.caret.path.last_mut() {
            *l = placed.0;
        }
        self.caret.offset = placed.1;
        self.doc.initialize_revision_targets();
    }

    // ---- formatting ----

    pub fn toggle_bold(&mut self) {
        self.toggle_run_prop(|p| p.bold, |p, v| p.bold = v);
    }
    pub fn toggle_italic(&mut self) {
        self.toggle_run_prop(|p| p.italic, |p, v| p.italic = v);
    }
    /// Underline over the selection. The underline a tracked insertion is drawn
    /// with is a display cue, not the user's: it does not count as "already
    /// underlined", and an explicit underline replaces it (so it is saved).
    pub fn toggle_underline(&mut self) {
        self.toggle_run_prop(RunProps::user_underline, RunProps::set_user_underline);
    }
    /// Strike over the selection, as [`Editor::toggle_underline`] treats the
    /// cue of a tracked deletion.
    pub fn toggle_strike(&mut self) {
        self.toggle_run_prop(RunProps::user_strike, RunProps::set_user_strike);
    }

    /// Run properties at the caret (used for toggles and the ribbon's
    /// on-states): what a character typed there takes, formatting toggled at
    /// the caret included (#854).
    pub fn caret_props(&self) -> RunProps {
        let mut props = self.text_props_at_caret();
        if let Some(pending) = self.pending_format() {
            pending.apply(&mut props);
        }
        props
    }

    /// The props the text around the caret gives a character typed there.
    fn text_props_at_caret(&self) -> RunProps {
        resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| run_props_at(&p.content, self.caret.offset))
            .unwrap_or_default()
    }

    /// The formatting toggled at the caret, while it still holds: the caret
    /// has not moved, nothing is selected, and no undo step has been pushed
    /// since the toggle's own (#854).
    fn pending_format(&self) -> Option<&PendingFormat> {
        self.pending.as_ref().filter(|p| {
            p.caret == self.caret && p.serial == self.undo_serial() && !self.has_selection()
        })
    }

    /// Paragraph properties of the paragraph at the caret.
    pub fn caret_para_props(&self) -> ParProps {
        resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| p.props.clone())
            .unwrap_or_default()
    }

    /// Apply `f` to the run properties over the selection (or the run at the caret
    /// when there is no selection, so the next typed text takes the change).
    fn map_props(&mut self, f: impl Fn(&mut RunProps)) {
        let spans = self.selection_spans();
        if spans.is_empty() {
            return;
        }
        self.checkpoint(EditKind::Structural);
        for (path, s, e) in &spans {
            if let Some(p) = para_mut(&mut self.doc.body, path) {
                map_prop_range(&mut p.content, *s, *e, &f);
            }
        }
        self.doc.initialize_revision_targets();
    }

    /// Grow (or shrink) the font size of the selection by `delta` half-points,
    /// defaulting from 11pt when unset.
    pub fn resize_font(&mut self, delta: i32) {
        self.map_props(|p| {
            let cur = p.size_half_pts.unwrap_or(22) as i32;
            p.size_half_pts = Some((cur + delta).clamp(2, 264) as u32);
        });
    }
    pub fn set_font_size(&mut self, half_pts: u32) {
        self.map_props(move |p| {
            p.size_half_pts = Some(half_pts);
            p.forget_loaded_size();
        });
    }
    pub fn set_font(&mut self, name: &str) {
        let name = name.to_string();
        self.map_props(move |p| {
            p.font = Some(name.clone());
            p.forget_loaded_font();
        });
    }
    pub fn set_color(&mut self, hex: Option<String>) {
        self.map_props(move |p| {
            p.color = hex.clone();
            p.forget_loaded_color();
        });
    }
    pub fn set_highlight(&mut self, name: Option<String>) {
        self.map_props(move |p| p.highlight = name.clone());
    }

    /// Toggle subscript/superscript: turn it off if already on, else set it.
    pub fn toggle_vert_align(&mut self, target: VertAlign) {
        let new = if self.caret_props().vert_align == target {
            VertAlign::Baseline
        } else {
            target
        };
        self.map_props(move |p| p.vert_align = new);
    }

    /// Reset character formatting (Ctrl+Space) over the selection. Text
    /// highlight survives, as in Word: it is a review mark, not character
    /// formatting.
    pub fn clear_run_formatting(&mut self) {
        self.map_props(|p| {
            let highlight = p.highlight.take();
            // The run's rsids are bookkeeping, not formatting.
            let element_attrs = std::mem::take(&mut p.element_attrs);
            // Nor is how its hyphens are written (#1101).
            let hyphen_elements = p.hyphen_elements;
            *p = RunProps::default();
            p.highlight = highlight;
            p.element_attrs = element_attrs;
            p.hyphen_elements = hyphen_elements;
        });
    }

    /// Cycle the case of the selected text (Word's Shift+F3): all-caps →
    /// lowercase → Capitalize Each Word → all-caps …
    pub fn cycle_case(&mut self) {
        let text = self.selection_text();
        if text.trim().is_empty() {
            return;
        }
        let has_upper = text.chars().any(|c| c.is_uppercase());
        let has_lower = text.chars().any(|c| c.is_lowercase());
        let spans = self.selection_spans();
        self.checkpoint(EditKind::Structural);
        let f: Box<dyn Fn(&str) -> String> = if has_upper && !has_lower {
            Box::new(|s: &str| s.to_lowercase()) // ALL CAPS → lower
        } else if !has_upper {
            Box::new(|s: &str| title_case(s)) // lower → Capitalize
        } else {
            Box::new(|s: &str| s.to_uppercase()) // mixed → ALL CAPS
        };
        for (path, s, e) in &spans {
            if let Some(p) = para_mut(&mut self.doc.body, path) {
                map_text_range(&mut p.content, *s, *e, &*f);
            }
        }
        self.doc.initialize_revision_targets();
    }

    /// Paths of the paragraphs touched by the selection (or the caret's paragraph).
    fn selected_para_paths(&self) -> Vec<Vec<usize>> {
        let spans = self.selection_spans();
        if spans.is_empty() {
            return vec![self.caret.path.clone()];
        }
        let mut paths: Vec<Vec<usize>> = Vec::new();
        for (path, _, _) in spans {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }

    /// Apply `f` to each paragraph touched by the selection (one undo step).
    fn for_each_para(&mut self, f: impl Fn(&mut ParProps)) {
        self.checkpoint(EditKind::Structural);
        for path in self.selected_para_paths() {
            if let Some(p) = para_mut(&mut self.doc.body, &path) {
                f(&mut p.props);
            }
        }
    }

    /// Increase/decrease the left indent of the selected paragraphs by `delta`
    /// twips (clamped at 0).
    pub fn change_indent(&mut self, delta: i32) {
        self.for_each_para(|pr| {
            pr.indent = (pr.indent + delta).max(0);
            // The loaded left indent goes, character units included: a
            // decrease clamped to zero twips must still clear `w:leftChars`.
            if delta != 0 {
                pr.forget_loaded_indent(&[IndentSide::Left]);
            }
        });
    }

    /// Set the left indent and first-line delta (twips) of the selected
    /// paragraphs. `first_line` > 0 is a first-line indent, < 0 a hanging indent,
    /// 0 none. Used by the Paragraph dialog.
    pub fn set_indent(&mut self, left: i32, first_line: i32) {
        self.for_each_para(move |pr| {
            pr.indent = left.max(0);
            pr.first_line = first_line;
            pr.forget_loaded_indent(&[IndentSide::Left, IndentSide::FirstLine]);
        });
    }

    /// Set just the first-line delta (twips) of the selected paragraphs, leaving
    /// the left indent alone. Used by the First-line / Hanging ribbon buttons.
    pub fn set_first_line(&mut self, first_line: i32) {
        self.for_each_para(move |pr| {
            pr.first_line = first_line;
            pr.forget_loaded_indent(&[IndentSide::FirstLine]);
        });
    }

    /// Set the right indent (twips, clamped at 0) of the selected paragraphs.
    pub fn set_right_indent(&mut self, right: i32) {
        self.for_each_para(move |pr| {
            pr.indent_right = right.max(0);
            pr.forget_loaded_indent(&[IndentSide::Right]);
        });
    }

    /// The left indent and first-line delta at the caret (for syncing the
    /// Paragraph dialog and ribbon state).
    pub fn caret_para_indent(&self) -> (i32, i32) {
        let pr = self.caret_para_props();
        (pr.indent, pr.first_line)
    }

    /// Set the line spacing (`w:line` + `w:lineRule`) of the selected paragraphs.
    /// Word's presets are all `auto`-rule: 240 = single, 276 = 1.15, 360 = 1.5,
    /// 480 = double. Space before/after is left untouched.
    pub fn set_line_spacing(&mut self, line: i32, rule: &str) {
        let rule = rule.to_string();
        self.for_each_para(move |pr| {
            pr.spacing.line = Some(line);
            pr.spacing.line_rule = Some(rule.clone());
        });
    }

    /// The line-spacing multiple at the caret (1.0 = single, 1.5, 2.0, …), or
    /// `None` when the paragraph uses an exact/at-least rule or no line spacing.
    /// Used to light the active line-spacing choice in the ribbon.
    pub fn caret_line_multiple(&self) -> Option<f32> {
        self.caret_para_props().spacing.line_multiple()
    }

    /// Set the space before / after the selected paragraphs, in twips (`None`
    /// removes the attribute). Word's "Add Space Before/After Paragraph".
    pub fn set_space_before(&mut self, twips: Option<i32>) {
        self.for_each_para(move |pr| pr.spacing.before = twips);
    }
    pub fn set_space_after(&mut self, twips: Option<i32>) {
        self.for_each_para(move |pr| pr.spacing.after = twips);
    }
    /// The space before / after (twips) at the caret, for the ribbon menu state.
    pub fn caret_space_before(&self) -> Option<i32> {
        self.caret_para_props().spacing.before
    }
    pub fn caret_space_after(&self) -> Option<i32> {
        self.caret_para_props().spacing.after
    }

    /// Replace the direct tab stops of the selected paragraphs.
    pub fn set_tabs(&mut self, tabs: Vec<crate::model::TabStop>) {
        self.for_each_para(move |pr| pr.tabs = tabs.clone());
    }

    /// Add (or move) a tab stop at `pos` twips with the given alignment on the
    /// caret's paragraph; a nearby existing stop (within ~1/8") is replaced.
    pub fn add_tab_stop(&mut self, pos: i32, align: crate::model::TabAlign) {
        use crate::model::{TabLeader, TabStop};
        self.for_each_para(move |pr| {
            pr.tabs.retain(|t| (t.pos - pos).abs() > 180);
            pr.tabs.push(TabStop {
                pos,
                align,
                leader: TabLeader::None,
            });
            pr.tabs.sort_by_key(|t| t.pos);
        });
    }

    /// Remove the tab stop nearest `pos` (within `tol` twips) on the caret's
    /// paragraph. Returns true if one was removed.
    pub fn remove_tab_stop_near(&mut self, pos: i32, tol: i32) -> bool {
        let before = self.caret_para_props().tabs.len();
        self.for_each_para(move |pr| {
            if let Some((i, _)) = pr
                .tabs
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| (t.pos - pos).abs())
            {
                if (pr.tabs[i].pos - pos).abs() <= tol {
                    pr.tabs.remove(i);
                }
            }
        });
        self.caret_para_props().tabs.len() != before
    }

    /// Apply a paragraph style (`w:pStyle`) to the selected paragraphs, updating
    /// each one's heading level so headings render with their rule. `None` clears
    /// the style back to the default.
    pub fn set_para_style(&mut self, style_id: Option<&str>) {
        let sid = style_id.map(str::to_string);
        self.for_each_para(move |pr| {
            pr.heading_level = sid.as_deref().and_then(crate::load::heading_level);
            pr.style_id = sid.clone();
        });
    }

    /// The paragraph style id at the caret (for syncing the Styles ribbon/dialog).
    pub fn caret_para_style(&self) -> Option<String> {
        self.caret_para_props().style_id
    }

    /// Set (or clear) the list membership of the selected paragraphs.
    pub fn set_list(&mut self, num_id: Option<i32>) {
        self.for_each_para(move |pr| {
            pr.num_id = num_id;
            if num_id.is_none() {
                pr.ilvl = 0;
            }
        });
    }

    /// Whether every selected paragraph is already in list `num_id` (for toggling).
    pub fn all_in_list(&self, num_id: i32) -> bool {
        let paths = self.selected_para_paths();
        !paths.is_empty()
            && paths.iter().all(|path| {
                resolve_para(&self.doc.body, path).map(|p| p.props.num_id) == Some(Some(num_id))
            })
    }

    /// Set the bottom border of the selected paragraphs (Word's Borders ▸ Bottom).
    pub fn set_para_border(&mut self, borders: ParBorders) {
        self.for_each_para(move |pr| pr.borders = borders);
    }

    /// Wrap the current selection in comment markers for comment `id` (the
    /// reference run + range start/end go around the selected text), as one
    /// undo step. Returns false if there is no selection.
    pub fn add_comment(&mut self, id: &str) -> bool {
        let spans = self.selection_spans();
        let (Some((spath, soff, _)), Some((epath, _, eoff))) = (spans.first(), spans.last()) else {
            return false;
        };
        let (spath, soff, epath, eoff) = (spath.clone(), *soff, epath.clone(), *eoff);
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        // End marker first, so the start offset stays valid within one paragraph.
        self.caret = Caret {
            path: epath,
            offset: eoff,
        };
        self.paste_at_caret(&Clip {
            paras: vec![vec![
                Inline::Raw(format!("<w:commentRangeEnd w:id=\"{id}\"/>")),
                Inline::Raw(format!("<w:r><w:commentReference w:id=\"{id}\"/></w:r>")),
            ]],
        });
        self.caret = Caret {
            path: spath,
            offset: soff,
        };
        self.paste_at_caret(&Clip {
            paras: vec![vec![Inline::Raw(format!(
                "<w:commentRangeStart w:id=\"{id}\"/>"
            ))]],
        });
        // Markers inside a recorded insertion interrupt it.
        self.settle_revisions();
        true
    }

    /// Remove the markers (range start/end + reference) of comment `id`
    /// wherever they are ([`crate::inspect::remove_comment_markers`]), keeping
    /// any text or wrapper that shares raw XML with them, as one undo step,
    /// none when there were none. Returns how many were removed.
    pub fn remove_comment_markers(&mut self, id: &str) -> usize {
        let before = self.transaction_start();
        let removed = crate::inspect::remove_comment_markers(&mut self.doc, id);
        self.finish_review_transaction(before);
        removed
    }

    /// Sort the selected top-level paragraphs alphabetically (Word's A→Z Sort).
    pub fn sort_paragraphs(&mut self) {
        let mut idxs: Vec<usize> = self
            .selected_para_paths()
            .iter()
            .filter(|p| p.len() == 1)
            .map(|p| p[0])
            .collect();
        idxs.sort_unstable();
        idxs.dedup();
        if idxs.len() < 2 {
            return;
        }
        let (lo, hi) = (idxs[0], *idxs.last().unwrap());
        self.checkpoint(EditKind::Structural);
        let mut slice: Vec<Block> = self.doc.body[lo..=hi].to_vec();
        slice.sort_by_key(|b| match b {
            Block::Paragraph(p) => p.plain_text().to_lowercase(),
            _ => String::new(),
        });
        for (k, b) in slice.into_iter().enumerate() {
            self.doc.body[lo + k] = b;
        }
    }

    /// The plain text currently selected (empty if no selection). Selection
    /// offsets are editor offsets, so this slices [`editor_text`], not
    /// `plain_text` (which also holds zero-width inlines' text), except that a
    /// selected field gives its result text, not its [`FIELD_CHAR`].
    pub fn selection_text(&self) -> String {
        self.selection_spans()
            .iter()
            .filter_map(|(path, s, e)| {
                resolve_para(&self.doc.body, path).map(|p| display_text(&p.content, *s, *e))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Toggle a run property over the selection. The new value is "off" only if
    /// every selected character already has it (so it works like Word).
    ///
    /// With nothing selected it switches the property for the next character
    /// typed at the caret instead, as an undo step of its own (#854).
    fn toggle_run_prop(&mut self, get: fn(&RunProps) -> bool, set: fn(&mut RunProps, bool)) {
        let spans = self.selection_spans();
        if spans.is_empty() {
            if !self.has_selection() {
                self.toggle_at_caret(get, set);
            }
            return;
        }
        self.checkpoint(EditKind::Structural);
        let mut all = true;
        for (path, s, e) in &spans {
            if let Some(p) = resolve_para(&self.doc.body, path) {
                if !range_all_have(&p.content, *s, *e, get) {
                    all = false;
                    break;
                }
            }
        }
        let value = !all;
        for (path, s, e) in &spans {
            if let Some(p) = para_mut(&mut self.doc.body, path) {
                set_prop_range(&mut p.content, *s, *e, set, value);
            }
        }
        self.doc.initialize_revision_targets();
    }

    fn toggle_at_caret(&mut self, get: fn(&RunProps) -> bool, set: fn(&mut RunProps, bool)) {
        let value = !get(&self.caret_props());
        let mut toggles = self
            .pending_format()
            .map(|p| p.toggles.clone())
            .unwrap_or_default();
        toggles.push((set, value));
        // The step holds the state before the toggle, pending format and all.
        self.checkpoint(EditKind::Structural);
        let pending = PendingFormat {
            caret: self.caret.clone(),
            serial: self.undo_serial(),
            toggles,
        };
        // Toggles that cancel out (Bold twice) leave nothing to apply, so
        // the text typed next joins the run it is typed in.
        let base = self.text_props_at_caret();
        let mut toggled = base.clone();
        pending.apply(&mut toggled);
        self.pending = (toggled != base).then_some(pending);
    }

    // ---- find / replace ----

    /// All matches of `query` (non-overlapping), in document order. The
    /// search itself lives in [`crate::agent::find`] (a pure `Document`
    /// function, reusable by hosts that only have a bare document); this is a
    /// thin wrapper over `self.doc`.
    pub fn find_all(&self, query: &str, case_sensitive: bool) -> Vec<Match> {
        crate::agent::find(&self.doc, query, case_sensitive)
    }
}

/// The search core behind [`Editor::find_all`] — a free function over
/// `&[Block]` so [`crate::agent::find`] can call it without needing a live
/// `Editor`.
///
/// It searches the text the editor can address ([`editor_text`]), so match
/// offsets are editor offsets that selection and editing can use directly.
/// Text the editor gives zero width (tracked changes, footnote refs, …,
/// including those inside a hyperlink) is drawn but not searched: a match
/// there could be neither selected nor replaced. Nor is a field's result: the
/// field is one unit, [`FIELD_CHAR`] in the searched text. A hyperlink's plain
/// runs are searched, whatever else the link holds. The UI's Find also shows
/// that drawn text, as read-only matches: [`Editor::find_visible`] (#211).
pub(crate) fn find_all_in_body(body: &[Block], query: &str, case_sensitive: bool) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let q: Vec<char> = query.chars().collect();
    let mut out = Vec::new();
    for path in all_paragraph_paths(body) {
        let Some(p) = resolve_para(body, &path) else {
            continue;
        };
        let t: Vec<char> = editor_text(&p.content).chars().collect();
        if t.len() < q.len() {
            continue;
        }
        let mut i = 0;
        while i + q.len() <= t.len() {
            if (0..q.len()).all(|j| char_eq(t[i + j], q[j], case_sensitive)) {
                out.push(Match {
                    path: path.clone(),
                    start: i,
                    end: i + q.len(),
                });
                i += q.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

impl Editor {
    /// The next match relative to the caret (wrapping), forward or backward.
    pub fn find_next(&self, query: &str, case_sensitive: bool, reverse: bool) -> Option<Match> {
        let all = self.find_all(query, case_sensitive);
        let starts: Vec<(&[usize], usize)> =
            all.iter().map(|m| (m.path.as_slice(), m.start)).collect();
        let i = self.index_from_caret(&starts, reverse)?;
        all.into_iter().nth(i)
    }

    /// Of positions `starts` (paragraph path, offset) in document order: the
    /// index of the first one after the caret (in reverse, the last one
    /// before it), wrapping. `None` when there are none.
    fn index_from_caret(&self, starts: &[(&[usize], usize)], reverse: bool) -> Option<usize> {
        if starts.is_empty() {
            return None;
        }
        let paths = all_paragraph_paths(&self.doc.body);
        let key = |path: &[usize], off: usize| {
            (
                paths.iter().position(|p| p.as_slice() == path).unwrap_or(0),
                off,
            )
        };
        let caret = key(&self.caret.path, self.caret.offset);
        if reverse {
            starts
                .iter()
                .rposition(|&(path, off)| key(path, off) < caret)
                .or(Some(starts.len() - 1))
        } else {
            starts
                .iter()
                .position(|&(path, off)| key(path, off) > caret)
                .or(Some(0))
        }
    }

    /// Select a match (so it is highlighted, with the caret at its end).
    pub fn select_match(&mut self, m: &Match) {
        self.caret = Caret {
            path: m.path.clone(),
            offset: m.end,
        };
        self.anchor = Some(Caret {
            path: m.path.clone(),
            offset: m.start,
        });
        self.last = EditKind::None;
    }

    /// Replace the current selection with plain text (used by replace-current).
    ///
    /// A selection within one paragraph is rewritten in place, exactly as
    /// [`Editor::replace_all`] rewrites a match (one undo step), so Replace
    /// and Replace All agree. A selection across paragraphs, or text holding
    /// a newline (which splits the paragraph), deletes then types instead.
    pub fn replace_current_with(&mut self, text: &str) {
        let Some((lo, hi)) = self.selection_range() else {
            return;
        };
        let text = &without_field_chars(text);
        if lo.path == hi.path && !text.contains('\n') {
            self.checkpoint(EditKind::Structural);
            self.replace_text_range(&lo.path, lo.offset, hi.offset, text);
            self.anchor = None;
            self.caret = Caret {
                offset: lo.offset + text.chars().count(),
                path: lo.path,
            };
            return;
        }
        self.delete_selection();
        self.insert_str(text);
    }

    /// Replace every match of `query` with `with`. Returns the number replaced.
    /// The editor search ([`Editor::find_all`]): agents and automation use
    /// this; the UI's Replace All uses [`Editor::replace_all_visible`].
    pub fn replace_all(&mut self, query: &str, with: &str, case_sensitive: bool) -> usize {
        let matches = self.find_all(query, case_sensitive);
        self.replace_matches(matches, with)
    }

    /// Set paragraph alignment on the selected paragraphs (or the caret's).
    pub fn set_align(&mut self, align: Align) {
        let spans = self.selection_spans();
        let paths: Vec<Vec<usize>> = if spans.is_empty() {
            vec![self.caret.path.clone()]
        } else {
            spans.into_iter().map(|(p, _, _)| p).collect()
        };
        self.checkpoint(EditKind::Structural);
        for path in paths {
            if let Some(p) = para_mut(&mut self.doc.body, &path) {
                p.props.align = align;
            }
        }
    }

    /// Replace the document's final section properties as one undo step.
    pub fn set_trailing_section_properties(&mut self, section: SectionProperties) {
        self.checkpoint(EditKind::Structural);
        self.doc.set_trailing_section_properties(section);
    }

    /// Set (or clear) the section break carried by the caret's paragraph. A
    /// section break ends a section here, so the following content becomes a new
    /// section. Returns false if the caret isn't in a paragraph.
    pub fn set_caret_section_break(&mut self, sect: Option<String>) -> bool {
        self.checkpoint(EditKind::Structural);
        match para_mut(&mut self.doc.body, &self.caret.path.clone()) {
            Some(p) => {
                p.props.section_break = sect;
                true
            }
            None => false,
        }
    }
}

// ---- tree navigation ----

#[derive(Clone)]
struct RevisionPosition {
    target: RevisionTarget,
    start: Caret,
    end: Caret,
}

fn collect_revision_positions(body: &[Block]) -> Vec<RevisionPosition> {
    let mut positions = Vec::new();
    let mut prefix = Vec::new();
    collect_block_revision_positions(body, &mut prefix, None, &mut positions);
    positions
}

fn property_position(
    change: &Option<PropertyChange>,
    span: Option<(Caret, Caret)>,
    positions: &mut Vec<RevisionPosition>,
) {
    let (Some(change), Some((start, end))) = (change, span) else {
        return;
    };
    positions.push(RevisionPosition {
        target: change.metadata.target,
        start,
        end,
    });
}

fn paragraph_span(path: &[usize], paragraph: &Paragraph) -> (Caret, Caret) {
    (
        Caret::at(path.to_vec(), 0),
        Caret::at(path.to_vec(), para_text_len(paragraph)),
    )
}

fn forced_span(caret: &Option<Caret>) -> Option<(Caret, Caret)> {
    caret.as_ref().map(|caret| (caret.clone(), caret.clone()))
}

fn first_paragraph_span(body: &[Block], prefix: &mut Vec<usize>) -> Option<(Caret, Caret)> {
    for (index, block) in body.iter().enumerate() {
        prefix.push(index);
        let found = match block {
            Block::Paragraph(paragraph) => Some(paragraph_span(prefix, paragraph)),
            Block::Table(table) => first_table_paragraph_span(table, prefix),
            Block::SectionProperties(_) | Block::Raw(_) => None,
        };
        prefix.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn first_table_paragraph_span(
    table: &Table,
    table_path: &mut Vec<usize>,
) -> Option<(Caret, Caret)> {
    for (row_index, row) in table.rows.iter().enumerate() {
        for (cell_index, cell) in row.cells.iter().enumerate() {
            table_path.push(row_index);
            table_path.push(cell_index);
            let found = first_paragraph_span(&cell.blocks, table_path);
            table_path.pop();
            table_path.pop();
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

fn first_row_paragraph_span(row: &Row, row_path: &mut Vec<usize>) -> Option<(Caret, Caret)> {
    for (cell_index, cell) in row.cells.iter().enumerate() {
        row_path.push(cell_index);
        let found = first_paragraph_span(&cell.blocks, row_path);
        row_path.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn collect_block_revision_positions(
    body: &[Block],
    prefix: &mut Vec<usize>,
    forced: Option<Caret>,
    positions: &mut Vec<RevisionPosition>,
) {
    for (block_index, block) in body.iter().enumerate() {
        prefix.push(block_index);
        match block {
            Block::Paragraph(paragraph) => {
                let span = forced_span(&forced).or_else(|| Some(paragraph_span(prefix, paragraph)));
                property_position(
                    &paragraph.props.section_property_change,
                    span.clone(),
                    positions,
                );
                property_position(&paragraph.props.property_change, span, positions);
                collect_inline_revision_positions(
                    &paragraph.content,
                    prefix,
                    forced.clone(),
                    positions,
                );
                // A paragraph mark sits at the paragraph's end.
                for mark in &paragraph.props.mark_revisions {
                    let (start, end) = forced_span(&forced).unwrap_or_else(|| {
                        let end = paragraph_span(prefix, paragraph).1;
                        (end.clone(), end)
                    });
                    positions.push(RevisionPosition {
                        target: mark.metadata.target,
                        start,
                        end,
                    });
                }
            }
            Block::Table(table) => {
                let table_span =
                    forced_span(&forced).or_else(|| first_table_paragraph_span(table, prefix));
                property_position(&table.property_change, table_span, positions);
                for (row_index, row) in table.rows.iter().enumerate() {
                    prefix.push(row_index);
                    let row_span =
                        forced_span(&forced).or_else(|| first_row_paragraph_span(row, prefix));
                    property_position(&row.property_change, row_span, positions);
                    for (cell_index, cell) in row.cells.iter().enumerate() {
                        prefix.push(cell_index);
                        let cell_span = forced_span(&forced)
                            .or_else(|| first_paragraph_span(&cell.blocks, prefix));
                        property_position(&cell.property_change, cell_span, positions);
                        for unsupported in &cell.unsupported_revisions {
                            let Some((start, end)) = forced_span(&forced)
                                .or_else(|| first_paragraph_span(&cell.blocks, prefix))
                            else {
                                continue;
                            };
                            positions.push(RevisionPosition {
                                target: unsupported.metadata.target,
                                start,
                                end,
                            });
                        }
                        collect_block_revision_positions(
                            &cell.blocks,
                            prefix,
                            forced.clone(),
                            positions,
                        );
                        prefix.pop();
                    }
                    prefix.pop();
                }
            }
            Block::SectionProperties(section) => {
                let span =
                    forced_span(&forced).or_else(|| first_paragraph_span(body, &mut Vec::new()));
                property_position(&section.property_change, span, positions);
            }
            Block::Raw(_) => {}
        }
        prefix.pop();
    }
}

fn collect_inline_revision_positions(
    content: &[Inline],
    path: &mut Vec<usize>,
    forced: Option<Caret>,
    positions: &mut Vec<RevisionPosition>,
) {
    collect_inline_revision_positions_at(content, path, forced, positions, 0, false);
}

/// [`collect_inline_revision_positions`] over `content` starting at editor
/// offset `offset`. `in_link` marks a hyperlink's `content`: its inline
/// indices are not the paragraph's, so a text box there anchors at its point.
fn collect_inline_revision_positions_at(
    content: &[Inline],
    path: &mut Vec<usize>,
    forced: Option<Caret>,
    positions: &mut Vec<RevisionPosition>,
    mut offset: usize,
    in_link: bool,
) {
    for (inline_index, inline) in content.iter().enumerate() {
        let point = forced
            .clone()
            .unwrap_or_else(|| Caret::at(path.clone(), offset));
        match inline {
            Inline::Run(run) => {
                let end = if forced.is_some() {
                    point.clone()
                } else {
                    Caret::at(path.clone(), offset + run.text.chars().count())
                };
                property_position(
                    &run.props.property_change,
                    Some((point.clone(), end)),
                    positions,
                );
            }
            Inline::Hyperlink(link) => {
                let mut run_offset = offset;
                for run in &link.runs {
                    let start = forced
                        .clone()
                        .unwrap_or_else(|| Caret::at(path.clone(), run_offset));
                    let end = if forced.is_some() {
                        start.clone()
                    } else {
                        Caret::at(path.clone(), run_offset + run.text.chars().count())
                    };
                    property_position(&run.props.property_change, Some((start, end)), positions);
                    run_offset += run.text.chars().count();
                }
                collect_inline_revision_positions_at(
                    &link.content,
                    path,
                    forced.clone(),
                    positions,
                    run_offset,
                    true,
                );
            }
            Inline::Tab(props) | Inline::Break(_, props) => {
                let end = if forced.is_some() {
                    point.clone()
                } else {
                    Caret::at(path.clone(), offset + 1)
                };
                property_position(
                    &props.property_change,
                    Some((point.clone(), end)),
                    positions,
                );
            }
            Inline::TextBox { blocks, .. } => {
                if forced.is_some() || in_link {
                    collect_block_revision_positions(blocks, path, Some(point.clone()), positions);
                } else {
                    path.push(inline_index);
                    collect_block_revision_positions(blocks, path, None, positions);
                    path.pop();
                }
            }
            Inline::Revision {
                metadata, content, ..
            } => {
                positions.push(RevisionPosition {
                    target: metadata.target,
                    start: point.clone(),
                    end: point.clone(),
                });
                collect_inline_revision_positions(content, path, Some(point.clone()), positions);
            }
            Inline::UnsupportedRevision { metadata, .. } => {
                positions.push(RevisionPosition {
                    target: metadata.target,
                    start: point.clone(),
                    end: point.clone(),
                });
            }
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::Field { .. }
            | Inline::FootnoteRef { .. }
            | Inline::Raw(_) => {}
        }
        if forced.is_none() {
            offset += inline_len(inline);
        }
    }
}

fn clamp_caret(body: &[Block], caret: &mut Caret) {
    if resolve_para(body, &caret.path).is_none() {
        if let Some(path) = first_paragraph_path(body) {
            caret.path = path;
        }
        caret.offset = 0;
    }
    let len = resolve_para(body, &caret.path)
        .map(para_text_len)
        .unwrap_or(0);
    caret.offset = caret.offset.min(len);
}

/// A paragraph's caret length: the sum of its inlines' [`inline_len`].
pub fn para_text_len(p: &Paragraph) -> usize {
    p.content.iter().map(inline_len).sum()
}

/// The paragraph a caret path names: a table step is `table, row, cell`, and a
/// step past a paragraph enters the text box at that inline index. `None` when
/// the path does not end on a paragraph.
pub fn resolve_para<'a>(body: &'a [Block], path: &[usize]) -> Option<&'a Paragraph> {
    let (i, rest) = path.split_first()?;
    match body.get(*i)? {
        Block::Paragraph(p) if rest.is_empty() => Some(p),
        // A deeper path descends into a text box embedded in this paragraph:
        // rest[0] is the text box's inline index, rest[1..] the path inside it.
        Block::Paragraph(p) => {
            let (k, inner) = rest.split_first()?;
            match p.content.get(*k)? {
                Inline::TextBox { blocks, .. } => resolve_para(blocks, inner),
                _ => None,
            }
        }
        Block::Table(t) if rest.len() >= 2 => {
            let cell = t.rows.get(rest[0])?.cells.get(rest[1])?;
            resolve_para(&cell.blocks, &rest[2..])
        }
        _ => None,
    }
}

/// [`container_mut`], read-only.
fn container<'a>(body: &'a [Block], path: &[usize]) -> Option<(&'a [Block], usize)> {
    if path.len() <= 1 {
        let i = *path.first()?;
        return Some((body, i));
    }
    match body.get(path[0])? {
        Block::Table(t) => {
            let cell = t.rows.get(path[1])?.cells.get(path[2])?;
            container(&cell.blocks, &path[3..])
        }
        Block::Paragraph(p) => match p.content.get(path[1])? {
            Inline::TextBox { blocks, .. } => container(blocks, &path[2..]),
            _ => None,
        },
        _ => None,
    }
}

/// Resolve the `Vec<Block>` that directly contains the target, and its index.
fn container_mut<'a>(
    body: &'a mut Vec<Block>,
    path: &[usize],
) -> Option<(&'a mut Vec<Block>, usize)> {
    if path.len() <= 1 {
        let i = *path.first()?;
        return Some((body, i));
    }
    match body.get_mut(path[0])? {
        Block::Table(t) => {
            let cell = t.rows.get_mut(path[1])?.cells.get_mut(path[2])?;
            container_mut(&mut cell.blocks, &path[3..])
        }
        // Descend into a text box (inline index `path[1]`) within this paragraph.
        Block::Paragraph(p) => match p.content.get_mut(path[1])? {
            Inline::TextBox { blocks, .. } => container_mut(blocks, &path[2..]),
            _ => None,
        },
        _ => None,
    }
}

/// The border kind for an autoformat trigger: a string of three or more of the
/// same border character. `None` otherwise.
fn hrule_kind(text: &str) -> Option<BorderKind> {
    let t = text.trim();
    let mut chars = t.chars();
    let first = chars.next()?;
    let kind = match first {
        '-' => BorderKind::Single,
        '_' | '#' => BorderKind::Thick,
        '=' => BorderKind::Double,
        '*' => BorderKind::Dotted,
        '~' => BorderKind::Wavy,
        _ => return None,
    };
    (t.chars().count() >= 3 && t.chars().all(|c| c == first)).then_some(kind)
}

fn para_mut<'a>(body: &'a mut Vec<Block>, path: &[usize]) -> Option<&'a mut Paragraph> {
    let (cont, idx) = container_mut(body, path)?;
    match cont.get_mut(idx)? {
        Block::Paragraph(p) => Some(p),
        _ => None,
    }
}

fn first_paragraph_path(body: &[Block]) -> Option<Vec<usize>> {
    all_paragraph_paths(body).into_iter().next()
}

/// All paragraph paths in document (reading) order.
fn all_paragraph_paths(body: &[Block]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    collect_paths(body, &mut prefix, &mut out);
    out
}

fn collect_paths(body: &[Block], prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    for (i, b) in body.iter().enumerate() {
        prefix.push(i);
        match b {
            Block::Paragraph(p) => {
                out.push(prefix.clone());
                // Text-box paragraphs are addressable just after their host.
                for (k, inl) in p.content.iter().enumerate() {
                    if let Inline::TextBox { blocks, .. } = inl {
                        prefix.push(k);
                        collect_paths(blocks, prefix, out);
                        prefix.pop();
                    }
                }
            }
            Block::Table(t) => {
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        prefix.push(ri);
                        prefix.push(ci);
                        collect_paths(&cell.blocks, prefix, out);
                        prefix.pop();
                        prefix.pop();
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
        prefix.pop();
    }
}

// ---- content editing (operate on a paragraph's inline vector) ----

/// How many caret offsets an inline occupies: its characters for a run, one
/// for a tab or break, one for a field that shows a result (edited as one unit,
/// like Word), and zero for everything the editor treats
/// as an opaque, uneditable anchor (revisions, drawings, a field with nothing
/// to show, …). A hyperlink occupies its
/// `runs` plus its `content` counted by these same rules, so the plain text of
/// a link that also holds revisions, bookmarks or proofing marks is editable
/// while those children stay zero-width.
/// Hosts that map their own positions to editor offsets (the browser editor's
/// `docx_doc` model) use this so the two never disagree.
pub fn inline_len(i: &Inline) -> usize {
    match i {
        Inline::Run(r) => r.text.chars().count(),
        Inline::Hyperlink(h) => link_runs_len(h) + h.content.iter().map(inline_len).sum::<usize>(),
        Inline::Tab(_) | Inline::Break(..) => 1,
        Inline::Field { text, .. } => usize::from(!text.is_empty()),
        // Zero-length, invisible in the editor (preserved for save only).
        Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::TextBox { .. }
        | Inline::Revision { .. }
        | Inline::UnsupportedRevision { .. }
        | Inline::FootnoteRef { .. }
        | Inline::Raw(_) => 0,
    }
}

/// A paragraph's text in editor offset space: one char per offset, matching
/// [`inline_len`] inline by inline (zero-width inlines contribute nothing, a
/// hyperlink contributes its `runs` and then its `content` by these same rules,
/// a tab is `'\t'`, a break is `'\n'`, a field is [`FIELD_CHAR`]).
/// Unlike [`Paragraph::plain_text`], every char here is one the caret can
/// reach, so offsets into it can be selected and edited.
fn editor_text(content: &[Inline]) -> String {
    let mut out = String::new();
    push_editor_text(content, &mut out);
    out
}

fn push_editor_text(content: &[Inline], out: &mut String) {
    for inline in content {
        match inline {
            Inline::Run(r) => out.push_str(&r.text),
            Inline::Hyperlink(h) => {
                h.runs.iter().for_each(|r| out.push_str(&r.text));
                push_editor_text(&h.content, out);
            }
            Inline::Tab(_) => out.push('\t'),
            Inline::Break(..) => out.push('\n'),
            Inline::Field { text, .. } => {
                if !text.is_empty() {
                    out.push(FIELD_CHAR);
                }
            }
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::TextBox { .. }
            | Inline::Revision { .. }
            | Inline::UnsupportedRevision { .. }
            | Inline::FootnoteRef { .. }
            | Inline::Raw(_) => {}
        }
    }
}

/// The character a field stands for in editor text ([`editor_text`], and the
/// automation text built from it): U+FFFC OBJECT REPLACEMENT CHARACTER. It is
/// never inserted as text: typing, pasting, replacing or splicing Markdown
/// (`agent::parse_markdown_blocks`) with it adds nothing.
pub const FIELD_CHAR: char = '\u{FFFC}';

/// Whether an inline is a field edited as one unit: Backspace or Delete next to
/// it selects it first, and the next press deletes all of it (as Word does). A
/// `w:sym` symbol run, which the loader also keeps as a [`Inline::Field`], is a
/// plain character instead and is deleted at once.
pub(crate) fn is_field_unit(inline: &Inline) -> bool {
    match inline {
        Inline::Field { raw, text } => {
            let symbol = raw.contains("<w:sym")
                && !raw.contains("<w:fldChar")
                && !raw.trim_start().starts_with("<w:fldSimple");
            !text.is_empty() && !symbol
        }
        _ => false,
    }
}

/// `text` without [`FIELD_CHAR`]s, for the entry points that insert text: the
/// stand-in adds nothing, so callers must count what is left.
pub(crate) fn without_field_chars(text: &str) -> String {
    text.chars().filter(|&c| c != FIELD_CHAR).collect()
}

/// The inline holding editor offset `idx` (the character at `[idx, idx + 1)`),
/// looking inside a hyperlink's `content`.
fn inline_covering(content: &[Inline], idx: usize) -> Option<&Inline> {
    let mut acc = 0;
    for inline in content {
        let l = inline_len(inline);
        if idx < acc + l {
            let local = idx - acc;
            return match inline {
                Inline::Hyperlink(h) if local >= link_runs_len(h) => {
                    inline_covering(&h.content, local - link_runs_len(h))
                }
                _ => Some(inline),
            };
        }
        acc += l;
    }
    None
}

/// The text a user sees in editor offsets `[start, end)` of `content`: the
/// editor text, with each field's result in place of its [`FIELD_CHAR`].
fn display_text(content: &[Inline], start: usize, end: usize) -> String {
    let mut out = String::new();
    push_display_text(content, start, end, &mut 0, &mut out);
    out
}

fn push_display_text(
    content: &[Inline],
    start: usize,
    end: usize,
    pos: &mut usize,
    out: &mut String,
) {
    for inline in content {
        let len = inline_len(inline);
        let a = *pos;
        match inline {
            Inline::Hyperlink(h) => {
                let runs: String = h.runs.iter().map(|r| r.text.as_str()).collect();
                let (s, e) = (
                    start.clamp(a, a + runs.chars().count()),
                    end.clamp(a, a + runs.chars().count()),
                );
                out.extend(runs.chars().skip(s - a).take(e.saturating_sub(s)));
                *pos = a + runs.chars().count();
                push_display_text(&h.content, start, end, pos, out);
            }
            Inline::Field { text, .. } if len == 1 => {
                if start <= a && a < end {
                    out.push_str(text);
                }
                *pos += 1;
            }
            _ => {
                let t = editor_text(std::slice::from_ref(inline));
                let (s, e) = (start.clamp(a, a + len), end.clamp(a, a + len));
                out.extend(t.chars().skip(s - a).take(e.saturating_sub(s)));
                *pos += len;
            }
        }
    }
}

/// Replace editor offsets `[start, end)` with `with`, in place.
///
/// The replacement is inserted *inside* the match, after its first char, and
/// the matched chars are deleted afterwards. `content_insert` at offset
/// `start + 1` resolves to the inline holding the first matched char, because
/// every earlier inline ends at or before `start`; so nothing is inserted on
/// the far side of an adjacent zero-width inline (a tracked change, …). When
/// that inline is a run or hyperlink, the replacement lands in it and takes its
/// formatting (as Word does), and it never goes empty mid-edit. When it is a
/// tab, break or field (a match starting with `\t`/`\n`/[`FIELD_CHAR`]), there is no
/// run to take: the replacement takes the formatting typing there would (see
/// [`typing_props`]: a tab's own), joining the following run only when that
/// matches, right where the tab or break was.
fn replace_range_in_content(content: &mut Vec<Inline>, start: usize, end: usize, with: &str) {
    let with = &without_field_chars(with);
    if end <= start {
        for (k, ch) in with.chars().enumerate() {
            content_insert_at(content, start + k, ch);
        }
        return;
    }
    let w = with.chars().count();
    for (k, ch) in with.chars().enumerate() {
        content_insert_at(content, start + 1 + k, ch);
    }
    content_delete(content, start);
    for _ in start + 1..end {
        content_delete(content, start + w);
    }
}

/// Extract the inline content in `[start, end)` (char offsets), styling intact.
fn extract_range(content: &[Inline], start: usize, end: usize) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut pos = 0;
    for inline in content {
        let len = inline_len(inline);
        let (a, b) = (pos, pos + len);
        let (os, oe) = (start.clamp(a, b), end.clamp(a, b));
        pos = b;
        if oe <= os {
            continue;
        }
        match inline {
            Inline::Run(r) => {
                let b1 = char_byte(&r.text, os - a);
                let b2 = char_byte(&r.text, oe - a);
                out.push(Inline::Run(Run {
                    text: r.text[b1..b2].to_string(),
                    props: r.props.clone(),
                }));
            }
            Inline::Hyperlink(h) => {
                let mut runs = Vec::new();
                let mut p = a;
                for run in &h.runs {
                    let rl = run.text.chars().count();
                    let (ra, rb) = (p, p + rl);
                    let (ros, roe) = (os.clamp(ra, rb), oe.clamp(ra, rb));
                    if roe > ros {
                        let bb1 = char_byte(&run.text, ros - ra);
                        let bb2 = char_byte(&run.text, roe - ra);
                        runs.push(Run {
                            text: run.text[bb1..bb2].to_string(),
                            props: run.props.clone(),
                        });
                    }
                    p = rb;
                }
                // The link's `content` holds the rest of its offsets; its
                // zero-width children fall outside any non-empty range, so a
                // copy never duplicates a bookmark or a tracked change.
                let inner = if h.content.is_empty() || oe <= p {
                    Vec::new()
                } else {
                    extract_range(&h.content, os.max(p) - p, oe - p)
                };
                let (runs, content) = if inner.iter().all(|i| matches!(i, Inline::Run(_))) {
                    runs.extend(inner.into_iter().filter_map(|i| match i {
                        Inline::Run(r) => Some(r),
                        _ => None,
                    }));
                    (runs, Vec::new())
                } else {
                    let mut all: Vec<Inline> = runs.into_iter().map(Inline::Run).collect();
                    all.extend(inner);
                    (Vec::new(), all)
                };
                if !runs.is_empty() || !content.is_empty() {
                    out.push(Inline::Hyperlink(Hyperlink {
                        target: h.target.clone(),
                        anchor: h.anchor.clone(),
                        rel_id: h.rel_id.clone(),
                        runs,
                        content,
                        raw: None,
                        content_changed: false,
                    }));
                }
            }
            Inline::Tab(rp) => out.push(Inline::Tab(rp.clone())),
            Inline::Break(k, rp) => out.push(Inline::Break(*k, rp.clone())),
            Inline::SmartArt { raw, text } => out.push(Inline::SmartArt {
                raw: raw.clone(),
                text: text.clone(),
            }),
            Inline::Chart { raw, chart } => out.push(Inline::Chart {
                raw: raw.clone(),
                chart: chart.clone(),
            }),
            Inline::Equation { raw, text, latex } => out.push(Inline::Equation {
                raw: raw.clone(),
                text: text.clone(),
                latex: latex.clone(),
            }),
            Inline::Field { raw, text } => out.push(Inline::Field {
                raw: raw.clone(),
                text: text.clone(),
            }),
            Inline::TextBox { raw, blocks } => out.push(Inline::TextBox {
                raw: raw.clone(),
                blocks: blocks.clone(),
            }),
            Inline::Revision {
                kind,
                metadata,
                raw,
                content,
                content_changed,
            } => out.push(Inline::Revision {
                kind: *kind,
                metadata: metadata.clone(),
                raw: raw.clone(),
                content: content.clone(),
                content_changed: *content_changed,
            }),
            Inline::UnsupportedRevision {
                kind,
                metadata,
                raw,
            } => out.push(Inline::UnsupportedRevision {
                kind: kind.clone(),
                metadata: metadata.clone(),
                raw: raw.clone(),
            }),
            Inline::FootnoteRef { id, endnote, raw } => out.push(Inline::FootnoteRef {
                id: *id,
                endnote: *endnote,
                raw: raw.clone(),
            }),
            Inline::Raw(s) => out.push(Inline::Raw(s.clone())),
        }
    }
    out
}

fn char_eq(a: char, b: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.eq_ignore_ascii_case(&b)
    }
}

fn char_byte(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(b, _)| b).unwrap_or(s.len())
}

fn run_insert(r: &mut Run, local: usize, ch: char) {
    let b = char_byte(&r.text, local);
    r.text.insert(b, ch);
}

fn runs_insert(runs: &mut Vec<Run>, o: usize, ch: char) {
    let mut acc = 0;
    for r in runs.iter_mut() {
        let l = r.text.chars().count();
        if o <= acc + l {
            run_insert(r, o - acc, ch);
            return;
        }
        acc += l;
    }
    if let Some(last) = runs.last_mut() {
        let l = last.text.chars().count();
        run_insert(last, l, ch);
    } else {
        runs.push(Run {
            text: ch.to_string(),
            props: RunProps::default(),
        });
    }
}

fn runs_delete(runs: &mut Vec<Run>, idx: usize) {
    let mut acc = 0;
    for i in 0..runs.len() {
        let l = runs[i].text.chars().count();
        if idx < acc + l {
            let local = idx - acc;
            let b = char_byte(&runs[i].text, local);
            let nb = char_byte(&runs[i].text, local + 1);
            runs[i].text.replace_range(b..nb, "");
            if runs[i].text.is_empty() {
                runs.remove(i);
            }
            return;
        }
        acc += l;
    }
}

fn link_runs_len(h: &Hyperlink) -> usize {
    h.runs.iter().map(|r| r.text.chars().count()).sum()
}

/// Where caret offset `local` inside a hyperlink lives: in its `runs`, or in
/// its `content` (at the returned offset within it).
enum LinkPart {
    Runs(usize),
    Content(usize),
}

/// A simple link edits its `runs`; a complex one (children in `content`)
/// edits `content`, except inside any `runs` it also has. The first check
/// keeps an empty-runs complex link from growing a stray default-props run.
fn link_part(h: &Hyperlink, local: usize) -> LinkPart {
    if h.content.is_empty() {
        return LinkPart::Runs(local);
    }
    let runs_len = link_runs_len(h);
    if !h.runs.is_empty() && local <= runs_len {
        LinkPart::Runs(local)
    } else {
        LinkPart::Content(local - runs_len)
    }
}

/// A zero-width child that shows nothing: a bookmark, proofing or permission
/// mark, or a comment range. These may leave a link whose text is all deleted.
fn is_marker(inline: &Inline) -> bool {
    const MARKERS: &[&str] = &[
        "w:proofErr",
        "w:bookmarkStart",
        "w:bookmarkEnd",
        "w:commentRangeStart",
        "w:commentRangeEnd",
        "w:permStart",
        "w:permEnd",
    ];
    match inline {
        Inline::Raw(raw) => {
            let name = raw
                .trim_start()
                .strip_prefix('<')
                .unwrap_or_default()
                .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
                .next()
                .unwrap_or_default();
            MARKERS.contains(&name)
        }
        _ => false,
    }
}

/// Where text typed or pasted at caret offset `o` goes when an empty inline
/// content control (a cleared cover-page placeholder, #652) sits exactly
/// there: the index just past its opening boundary, so the text lands inside
/// the control rather than before it, as in Word. Zero-width markers between
/// the boundaries still count as empty. `None` when there is no such control;
/// typing next to a control with content keeps the usual rules.
fn empty_sdt_at(content: &[Inline], o: usize) -> Option<usize> {
    let mut acc = 0;
    for (i, inline) in content.iter().enumerate() {
        if acc > o {
            return None;
        }
        if acc == o {
            if let Inline::Raw(raw) = inline {
                if crate::hf::is_sdt_open(raw) {
                    let close = content[i + 1..]
                        .iter()
                        .find(|x| !is_marker(x))
                        .is_some_and(|x| matches!(x, Inline::Raw(r) if crate::hf::is_sdt_close(r)));
                    if close {
                        return Some(i + 1);
                    }
                }
            }
        }
        acc += inline_len(inline);
    }
    None
}

/// The formatting of text typed into the empty content control whose
/// content starts at `at` (see [`empty_sdt_at`]): the control's own run
/// properties (`w:sdtPr/w:rPr`, which Word and our cover placeholders write),
/// else the nearest formatting source around it.
fn sdt_typing_props(content: &[Inline], at: usize) -> RunProps {
    if let Some(Inline::Raw(open)) = at.checked_sub(1).and_then(|k| content.get(k)) {
        if let Some(props) = crate::load::sdt_run_props(open) {
            return props;
        }
    }
    source_before(content, at)
        .or_else(|| source_from(content, at))
        .cloned()
        .unwrap_or_default()
}

/// Type `ch` at caret offset `o`: into an empty content control there (see
/// [`empty_sdt_at`]), else as [`content_insert_at`] places it.
fn content_insert(content: &mut Vec<Inline>, o: usize, ch: char) {
    content_insert_with(content, o, ch, true);
}

/// Insert `ch` at caret offset `o` into the inline holding it, never into an
/// empty content control at `o`: Replace's edits inside a match, which must
/// stay where the matched text was.
fn content_insert_at(content: &mut Vec<Inline>, o: usize, ch: char) {
    content_insert_with(content, o, ch, false);
}

fn content_insert_with(content: &mut Vec<Inline>, o: usize, ch: char, into_empty: bool) {
    if let Some(at) = empty_sdt_at(content, o).filter(|_| into_empty) {
        let props = sdt_typing_props(content, at);
        content.insert(
            at,
            Inline::Run(Run {
                text: ch.to_string(),
                props,
            }),
        );
        clear_showing_placeholder(content, at - 1);
        return;
    }
    let Some((i, local)) = locate(content, o) else {
        if let Some(Inline::Run(r)) = content.last_mut() {
            let rl = r.text.chars().count();
            run_insert(r, rl, ch);
        } else {
            let props = end_props(content);
            content.push(Inline::Run(Run {
                text: ch.to_string(),
                props,
            }));
        }
        return;
    };
    match &mut content[i] {
        Inline::Run(r) => run_insert(r, local, ch),
        Inline::Hyperlink(h) => match link_part(h, local) {
            LinkPart::Runs(local) => runs_insert(&mut h.runs, local, ch),
            LinkPart::Content(local) => {
                content_insert_with(&mut h.content, local, ch, into_empty);
                h.content_changed = true;
            }
        },
        Inline::Tab(_)
        | Inline::Break(..)
        | Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::Field { .. }
        | Inline::TextBox { .. }
        | Inline::Revision { .. }
        | Inline::UnsupportedRevision { .. }
        | Inline::FootnoteRef { .. }
        | Inline::Raw(_) => {
            let props = typing_props(content, i, local);
            // `local == 0` only happens at `i == 0`: any later caret at an
            // inline's start is claimed by the inline before it.
            let at = if local == 0 { i } else { i + 1 };
            if local > 0 {
                if let Some(Inline::Run(r)) = content.get_mut(at) {
                    if r.props == props {
                        run_insert(r, 0, ch);
                        return;
                    }
                }
            }
            content.insert(
                at,
                Inline::Run(Run {
                    text: ch.to_string(),
                    props,
                }),
            );
        }
    }
}

/// The inline holding caret offset `o`, and the offset within it: the first
/// inline whose end is at or past `o`, so a caret between two inlines belongs
/// to the one before it. `None` past the end of the content.
fn locate(content: &[Inline], o: usize) -> Option<(usize, usize)> {
    let mut acc = 0;
    for (i, inline) in content.iter().enumerate() {
        let l = inline_len(inline);
        if o <= acc + l {
            return Some((i, o - acc));
        }
        acc += l;
    }
    None
}

/// The formatting an inline hands to text typed next to it: a run's, a tab's
/// or a break's (each is a run in OOXML and keeps its `w:rPr`). Hyperlinks and
/// zero-width inlines are not sources; a link's style is not extended to text
/// typed outside the link.
fn source_props(inline: &Inline) -> Option<&RunProps> {
    match inline {
        Inline::Run(r) => Some(&r.props),
        Inline::Tab(props) | Inline::Break(_, props) => Some(props),
        _ => None,
    }
}

/// The nearest formatting source before inline `i`.
fn source_before(content: &[Inline], i: usize) -> Option<&RunProps> {
    content[..i.min(content.len())]
        .iter()
        .rev()
        .find_map(source_props)
}

/// The nearest formatting source at or after inline `i`.
fn source_from(content: &[Inline], i: usize) -> Option<&RunProps> {
    content.iter().skip(i).find_map(source_props)
}

/// Run properties a character typed at `local` within the non-run inline at
/// `i` takes, following Word (the formatting of the character before it; at
/// the paragraph start, of the character after it):
/// - at the paragraph start (`local == 0`): the nearest source at or after `i`;
/// - after a tab or a break: its own formatting (#279);
/// - after a zero-width inline: the run or tab right after it; failing that,
///   the nearest source before, then after, it.
fn typing_props(content: &[Inline], i: usize, local: usize) -> RunProps {
    if local == 0 {
        return source_from(content, i).cloned().unwrap_or_default();
    }
    if let Inline::Tab(props) | Inline::Break(_, props) = &content[i] {
        return props.clone();
    }
    content
        .get(i + 1)
        .and_then(source_props)
        .or_else(|| source_before(content, i))
        .or_else(|| source_from(content, i + 1))
        .cloned()
        .unwrap_or_default()
}

/// Run properties a character typed past the end of `content` takes, when the
/// last inline is not a run.
fn end_props(content: &[Inline]) -> RunProps {
    source_before(content, content.len())
        .cloned()
        .unwrap_or_default()
}

fn content_delete(content: &mut Vec<Inline>, idx: usize) {
    let mut acc = 0;
    for i in 0..content.len() {
        let l = inline_len(&content[i]);
        if idx < acc + l {
            let local = idx - acc;
            match &mut content[i] {
                Inline::Run(r) => {
                    let b = char_byte(&r.text, local);
                    let nb = char_byte(&r.text, local + 1);
                    r.text.replace_range(b..nb, "");
                    if r.text.is_empty() {
                        content.remove(i);
                    }
                }
                Inline::Hyperlink(h) => {
                    let runs_len = link_runs_len(h);
                    if local < runs_len {
                        runs_delete(&mut h.runs, local);
                    } else {
                        content_delete(&mut h.content, local - runs_len);
                        h.content_changed = true;
                    }
                    // A link with no editable text left goes, as in Word. Its
                    // bookmarks and proofing marks stay where it was; anything
                    // still showing inside it (a tracked change, a picture, an
                    // empty field) keeps the link, zero-width. A field with a
                    // result is editable, one offset, so its link is not empty.
                    if h.runs.is_empty() && h.content.iter().all(is_marker) {
                        let kept = std::mem::take(&mut h.content);
                        content.splice(i..=i, kept);
                    }
                }
                Inline::Tab(_)
                | Inline::Break(..)
                | Inline::SmartArt { .. }
                | Inline::Chart { .. }
                | Inline::Equation { .. }
                | Inline::Field { .. }
                | Inline::TextBox { .. }
                | Inline::Revision { .. }
                | Inline::UnsupportedRevision { .. }
                | Inline::FootnoteRef { .. }
                | Inline::Raw(_) => {
                    content.remove(i);
                }
            }
            return;
        }
        acc += l;
    }
}

/// A merge that removes the paragraph `gone` into `kept`: the paragraph mark
/// deleted is always `kept`'s, so the surviving mark is `gone`'s (its section
/// break and tracked change, or none), as in Word. When `kept` closed a section
/// its break is removed and the text before it joins the following section
/// (#645); when `gone` is the section's last paragraph its break survives on
/// the merged paragraph (#748). Its tracked insertion/deletion records replace
/// `kept`'s too.
fn keep_section_mark(kept: &mut ParProps, gone: ParProps) {
    crate::review::adopt_mark_revisions(kept, &gone);
    kept.section_break = gone.section_break;
    kept.section_property_change = gone.section_property_change;
}

/// Split `content` at caret offset `o`: `content` keeps what is before it,
/// and the rest is returned. A zero-width inline exactly at `o` stays on the
/// left. A run or a hyperlink that `o` falls strictly inside is split in two:
/// Enter, paste and Tab inside a link leave a link on each side of what they
/// insert (#352), while typing inside a link extends it (`content_insert`).
fn split_content(content: &mut Vec<Inline>, o: usize) -> Vec<Inline> {
    let mut acc = 0;
    for i in 0..content.len() {
        let l = inline_len(&content[i]);
        if o < acc + l {
            let local = o - acc;
            if local == 0 {
                return content.split_off(i);
            }
            let right = match &mut content[i] {
                Inline::Run(r) => {
                    let b = char_byte(&r.text, local);
                    let text = r.text.split_off(b);
                    Inline::Run(Run {
                        text,
                        props: r.props.clone(),
                    })
                }
                Inline::Hyperlink(h) => Inline::Hyperlink(split_link(h, local)),
                _ => return content.split_off(i),
            };
            let mut rest = content.split_off(i + 1);
            rest.insert(0, right);
            return rest;
        }
        acc += l;
    }
    Vec::new()
}

/// Insert `ins` at caret offset `o`, as a paste does: into an empty content
/// control there (see [`empty_sdt_at`]), else splitting a run or a link `o`
/// falls inside. Content controls whose boundaries sit at `o` decide which
/// side of them it goes:
/// - `before_opens` (the first piece of a multi-paragraph paste): before a
///   control that opens at `o`, so the paragraph break after it moves that
///   control down whole, as Enter there does;
/// - `placeholders` (the caret is in a cover page): inside a cover
///   placeholder whose content ends at `o`, as typing there goes, so the
///   pasted text is the placeholder's.
///
/// Otherwise it lands after a control ending at `o` and inside one opening
/// there. The length inserted.
fn insert_inlines(
    content: &mut Vec<Inline>,
    o: usize,
    ins: Vec<Inline>,
    before_opens: bool,
    placeholders: bool,
) -> usize {
    let tail = match empty_sdt_at(content, o) {
        Some(at) => {
            clear_showing_placeholder(content, at - 1);
            content.split_off(at)
        }
        None => {
            let mut tail = split_content(content, o);
            if before_opens {
                move_trailing_opens(content, &mut tail);
            }
            if placeholders {
                if let Some(k) = trailing_placeholder_close(content) {
                    let moved: Vec<Inline> = content.drain(k..).collect();
                    tail.splice(0..0, moved);
                }
            }
            tail
        }
    };
    let len = ins.iter().map(inline_len).sum();
    content.extend(ins);
    content.extend(tail);
    len
}

/// Move the content controls that open at the end of `head` (nothing of
/// their content in it, zero-width markers aside) to the start of `tail`:
/// the trailing zero-width inlines from the first open among them on.
fn move_trailing_opens(head: &mut Vec<Inline>, tail: &mut Vec<Inline>) {
    let mut k = head.len();
    while k > 0
        && (is_marker(&head[k - 1])
            || matches!(&head[k - 1], Inline::Raw(r) if crate::hf::is_sdt_open(r)))
    {
        k -= 1;
    }
    if let Some(m) =
        (k..head.len()).find(|&i| matches!(&head[i], Inline::Raw(r) if crate::hf::is_sdt_open(r)))
    {
        let moved: Vec<Inline> = head.drain(m..).collect();
        tail.splice(0..0, moved);
    }
}

/// The index of the close of a cover placeholder ending `content` (zero-width
/// markers after it aside), if one does.
fn trailing_placeholder_close(content: &[Inline]) -> Option<usize> {
    let k = content.iter().rposition(|x| !is_marker(x))?;
    match &content[k] {
        Inline::Raw(r) if crate::hf::is_sdt_close(r) => {
            let open = open_of(content, k)?;
            is_placeholder_open(&content[open]).then_some(k)
        }
        _ => None,
    }
}

/// Join paragraph `gone`'s content onto `kept`'s (Backspace at a paragraph
/// start, Delete at its end, a selection across paragraphs). A content
/// control a paragraph break split in two (see [`split_paragraph_at`]) is
/// one again: where `kept` ends with a control's close and `gone` starts with
/// the reopened copy of that control (markers aside), the close and the copy
/// go, nested ones in order, so Enter then Backspace inside a control leaves
/// it as it was. Only a split's copy heals: it is exactly [`reopened`] of
/// the control before it, with no `w:id`, which Word always writes; two
/// separate controls, alike but for their ids, stay two.
fn join_paragraph_content(kept: &mut Vec<Inline>, mut gone: Vec<Inline>) {
    loop {
        let k = kept.iter().rposition(|x| !is_marker(x));
        let g = gone.iter().position(|x| !is_marker(x));
        let (Some(k), Some(g)) = (k, g) else {
            break;
        };
        let (Inline::Raw(close), Inline::Raw(open)) = (&kept[k], &gone[g]) else {
            break;
        };
        if !crate::hf::is_sdt_close(close) || !crate::hf::is_sdt_open(open) {
            break;
        }
        let Some(o) = open_of(kept, k) else {
            break;
        };
        if reopened(&kept[o]) != gone[g] {
            break;
        }
        kept.remove(k);
        gone.remove(g);
    }
    kept.extend(gone);
}

/// The control opened at `open` holds text now: it no longer shows its
/// placeholder, so its `w:showingPlcHdr` goes, as Word clears it on typing
/// (else Word would take the text for the placeholder).
fn clear_showing_placeholder(content: &mut [Inline], open: usize) {
    if let Some(Inline::Raw(raw)) = content.get_mut(open) {
        if raw.contains("<w:showingPlcHdr") {
            *raw = crate::sect::remove_element(raw, "w:showingPlcHdr");
        }
    }
}

/// The opening boundary of a content control again, for the half of a split
/// it continues into: without its `w:id` (optional in `w:sdtPr`), so ids stay
/// unique, and without `w:showingPlcHdr` and `w:dataBinding`, which belong
/// to the original (a second control bound to the same property would show
/// its value; a copy is not showing the placeholder).
fn reopened(open: &Inline) -> Inline {
    match open {
        Inline::Raw(raw) => {
            let mut raw = raw.clone();
            for name in ["w:id", "w:showingPlcHdr", "w:dataBinding"] {
                raw = crate::sect::remove_element(&raw, name);
            }
            Inline::Raw(raw)
        }
        other => other.clone(),
    }
}

/// After `head` and `tail` were split apart: close each inline content
/// control the split cut (opened in `head`, closed in `tail`) at the end of
/// `head`, and open it again at the start of `tail`, outermost first, as Word
/// does, so both halves serialize to well-formed XML. Only a control whose
/// close is in `tail` is reopened. The number of boundaries put at `tail`'s
/// start.
fn repair_cut_controls(head: &mut Vec<Inline>, tail: &mut Vec<Inline>) -> usize {
    let mut open: Vec<usize> = Vec::new();
    for (i, inline) in head.iter().enumerate() {
        match inline {
            Inline::Raw(raw) if crate::hf::is_sdt_open(raw) => open.push(i),
            Inline::Raw(raw) if crate::hf::is_sdt_close(raw) => {
                open.pop();
            }
            _ => {}
        }
    }
    // The closes in `tail` with no open there: the cut controls', innermost
    // first.
    let mut depth = 0usize;
    let mut closes = 0usize;
    for inline in tail.iter() {
        match inline {
            Inline::Raw(raw) if crate::hf::is_sdt_open(raw) => depth += 1,
            Inline::Raw(raw) if crate::hf::is_sdt_close(raw) => {
                if depth == 0 {
                    closes += 1;
                } else {
                    depth -= 1;
                }
            }
            _ => {}
        }
    }
    let cut = &open[open.len() - closes.min(open.len())..];
    let copies: Vec<Inline> = cut.iter().map(|&i| reopened(&head[i])).collect();
    for _ in cut {
        head.push(Inline::Raw(crate::load::SDT_BLOCK_CLOSE.to_string()));
    }
    let n = copies.len();
    tail.splice(0..0, copies);
    n
}

/// The index of the opening boundary that the closing one at `close` pairs
/// with, walking back through `content`.
fn open_of(content: &[Inline], close: usize) -> Option<usize> {
    let mut nest = 0usize;
    content[..close].iter().rposition(|x| match x {
        Inline::Raw(r) if crate::hf::is_sdt_close(r) => {
            nest += 1;
            false
        }
        Inline::Raw(r) if crate::hf::is_sdt_open(r) => {
            if nest == 0 {
                true
            } else {
                nest -= 1;
                false
            }
        }
        _ => false,
    })
}

/// Split a paragraph's content at caret offset `o` for a paragraph break
/// (Enter, a multi-paragraph paste, Blank Page, a section break, a table
/// inserted mid-paragraph), keeping inline content controls whole (#652):
/// - a control that starts right at `o` (nothing of its content before `o`,
///   zero-width markers aside) moves to the second half whole, with its id;
/// - a control the split cuts is closed at the end of the first half and
///   opened again at the start of the second ([`repair_cut_controls`]);
/// - with `placeholders` (the split is in a cover page), a split right at the
///   end of a cover placeholder's content, where the first half ends with its
///   close (markers aside), counts as cutting it too: the second half starts
///   with an empty copy of it, so text typed or pasted there goes into the
///   placeholder, as a second line. Any other control, and a split whose
///   first half does not end with a placeholder's close, splits as before: a
///   caret at a control's end is also just after it, and Enter there in Word
///   gives a plain paragraph.
///
/// The second half, and the index in it where its own content starts: after
/// the reopened boundaries, inside the controls the split continues.
fn split_paragraph_at(
    content: &mut Vec<Inline>,
    o: usize,
    placeholders: bool,
) -> (Vec<Inline>, usize) {
    let mut tail = split_content(content, o);
    // Controls opening right at the split go down whole.
    move_trailing_opens(content, &mut tail);
    // The placeholders whose content ends right at the split: the closes
    // ending the first half, outermost first.
    let mut ends: Vec<Inline> = Vec::new();
    let mut k = content.len();
    while placeholders && k > 0 {
        match &content[k - 1] {
            Inline::Raw(raw) if crate::hf::is_sdt_close(raw) => match open_of(content, k - 1) {
                Some(i) if is_placeholder_open(&content[i]) => ends.push(reopened(&content[i])),
                _ => break,
            },
            x if is_marker(x) => {}
            _ => break,
        }
        k -= 1;
    }
    let cut = repair_cut_controls(content, &mut tail);
    let n = ends.len();
    let closes = (0..n).map(|_| Inline::Raw(crate::load::SDT_BLOCK_CLOSE.to_string()));
    let empties: Vec<Inline> = ends.into_iter().chain(closes).collect();
    tail.splice(cut..cut, empties);
    (tail, cut + n)
}

/// Whether an inline opens a cover-page placeholder control (one whose
/// alias or tag names a [`crate::cover::Placeholder`]).
fn is_placeholder_open(inline: &Inline) -> bool {
    matches!(inline, Inline::Raw(raw) if crate::hf::is_sdt_open(raw)
        && crate::cover::placeholder_of(raw).is_some())
}

/// [`split_paragraph_at`]'s second half, outside a cover page.
fn split_paragraph_content(content: &mut Vec<Inline>, o: usize) -> Vec<Inline> {
    split_paragraph_at(content, o, false).0
}

/// Split a hyperlink at `local`, strictly inside its text: `h` keeps what is
/// before it, and the returned link, with the same target, anchor and
/// relationship, takes the rest. Each half keeps its own children. A link
/// loaded from XML rebuilds both halves from `raw`'s opening tag on save, so
/// each keeps the original `w:hyperlink` attributes.
fn split_link(h: &mut Hyperlink, local: usize) -> Hyperlink {
    let (runs, content) = match link_part(h, local) {
        LinkPart::Runs(local) => (
            split_runs(&mut h.runs, local),
            std::mem::take(&mut h.content),
        ),
        LinkPart::Content(local) => {
            let mut rest = split_content(&mut h.content, local);
            // A content control in the link that the split cuts closes in
            // each half.
            repair_cut_controls(&mut h.content, &mut rest);
            (Vec::new(), rest)
        }
    };
    h.content_changed |= h.raw.is_some();
    Hyperlink {
        target: h.target.clone(),
        anchor: h.anchor.clone(),
        rel_id: h.rel_id.clone(),
        runs,
        content,
        raw: h.raw.clone(),
        content_changed: h.raw.is_some(),
    }
}

/// Split `runs` at char offset `o`: `runs` keeps what is before it, and the
/// rest is returned.
fn split_runs(runs: &mut Vec<Run>, o: usize) -> Vec<Run> {
    let mut acc = 0;
    for i in 0..runs.len() {
        let l = runs[i].text.chars().count();
        if o < acc + l {
            let local = o - acc;
            if local == 0 {
                return runs.split_off(i);
            }
            let b = char_byte(&runs[i].text, local);
            let text = runs[i].text.split_off(b);
            let props = runs[i].props.clone();
            let mut rest = runs.split_off(i + 1);
            rest.insert(0, Run { text, props });
            return rest;
        }
        acc += l;
    }
    Vec::new()
}

/// True if every run-character in `[start, end)` already satisfies `get`.
fn range_all_have(
    content: &[Inline],
    start: usize,
    end: usize,
    get: fn(&RunProps) -> bool,
) -> bool {
    let (mut pos, mut saw) = (0, false);
    range_all_have_at(content, start, end, get, &mut pos, &mut saw) && saw
}

/// [`range_all_have`] over `content` starting at offset `*pos`, advancing it;
/// false as soon as a character in range lacks the property.
fn range_all_have_at(
    content: &[Inline],
    start: usize,
    end: usize,
    get: fn(&RunProps) -> bool,
    pos: &mut usize,
    saw: &mut bool,
) -> bool {
    let check = |props: &RunProps, len: usize, pos: &mut usize, saw: &mut bool| -> bool {
        let (a, b) = (*pos, *pos + len);
        let (os, oe) = (start.clamp(a, b), end.clamp(a, b));
        *pos = b;
        if oe > os {
            *saw = true;
            if !get(props) {
                return false;
            }
        }
        true
    };
    for inline in content {
        match inline {
            Inline::Run(r) => {
                if !check(&r.props, r.text.chars().count(), pos, saw) {
                    return false;
                }
            }
            Inline::Hyperlink(h) => {
                for run in &h.runs {
                    if !check(&run.props, run.text.chars().count(), pos, saw) {
                        return false;
                    }
                }
                if !range_all_have_at(&h.content, start, end, get, pos, saw) {
                    return false;
                }
            }
            // A tab or a break is a formatted character (see `edit_run_range`).
            Inline::Tab(props) | Inline::Break(_, props) => {
                if !check(props, 1, pos, saw) {
                    return false;
                }
            }
            // Not a formatted character: a field keeps its own result formatting.
            Inline::Field { .. } => *pos += inline_len(inline),
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::TextBox { .. }
            | Inline::Revision { .. }
            | Inline::UnsupportedRevision { .. }
            | Inline::FootnoteRef { .. }
            | Inline::Raw(_) => {} // zero-length
        }
    }
    true
}

/// Split a run at `[start, end)` (absolute char positions, run starting at `pos`)
/// and rebuild it, applying `mid` to the in-range slice's run. `mid` may change the
/// properties and/or the text of that slice.
fn split_run_with(
    r: Run,
    pos: usize,
    start: usize,
    end: usize,
    mid_fn: &dyn Fn(&str, &RunProps) -> Run,
) -> Vec<Run> {
    let len = r.text.chars().count();
    let (a, b) = (pos, pos + len);
    let os = start.clamp(a, b) - a;
    let oe = end.clamp(a, b) - a;
    if os >= oe {
        return vec![r];
    }
    let b1 = char_byte(&r.text, os);
    let b2 = char_byte(&r.text, oe);
    let (left, mid, right) = (&r.text[..b1], &r.text[b1..b2], &r.text[b2..]);
    let mut out = Vec::new();
    if !left.is_empty() {
        out.push(Run {
            text: left.to_string(),
            props: r.props.clone(),
        });
    }
    out.push(mid_fn(mid, &r.props));
    if !right.is_empty() {
        out.push(Run {
            text: right.to_string(),
            props: r.props.clone(),
        });
    }
    out
}

/// Rebuild `content`, applying `mid_fn` to the run slice in `[start, end)`.
fn edit_run_range(
    content: &mut Vec<Inline>,
    start: usize,
    end: usize,
    mid_fn: &dyn Fn(&str, &RunProps) -> Run,
) {
    edit_run_range_at(content, start, end, mid_fn, &mut 0);
}

/// [`edit_run_range`] over `content` starting at offset `*pos`, advancing it.
fn edit_run_range_at(
    content: &mut Vec<Inline>,
    start: usize,
    end: usize,
    mid_fn: &dyn Fn(&str, &RunProps) -> Run,
    pos: &mut usize,
) {
    let mut out = Vec::new();
    for inline in content.drain(..) {
        match inline {
            Inline::Run(r) => {
                let len = r.text.chars().count();
                for nr in split_run_with(r, *pos, start, end, mid_fn) {
                    out.push(Inline::Run(nr));
                }
                *pos += len;
            }
            Inline::Hyperlink(mut h) => {
                let mut new_runs = Vec::new();
                for run in h.runs.drain(..) {
                    let len = run.text.chars().count();
                    for nr in split_run_with(run, *pos, start, end, mid_fn) {
                        new_runs.push(nr);
                    }
                    *pos += len;
                }
                h.runs = new_runs;
                // Only a range over the link's content rebuilds it on save; an
                // untouched complex link keeps writing its original XML.
                let len: usize = h.content.iter().map(inline_len).sum();
                if len > 0 && start < *pos + len && *pos < end {
                    edit_run_range_at(&mut h.content, start, end, mid_fn, pos);
                    h.content_changed = true;
                } else {
                    *pos += len;
                }
                out.push(Inline::Hyperlink(h));
            }
            // A tab is a run in OOXML: formatting applies to it like to any
            // character, and typing after it takes its props. Only the props
            // of `mid_fn`'s result are kept; its text stays a tab.
            Inline::Tab(rp) => {
                let rp = if (start..end).contains(pos) {
                    mid_fn("\t", &rp).props
                } else {
                    rp
                };
                out.push(Inline::Tab(rp));
                *pos += 1;
            }
            // A break is a run in OOXML too: it takes the formatting like a
            // tab does, so text typed after it keeps it (#279).
            Inline::Break(k, rp) => {
                let rp = if (start..end).contains(pos) {
                    mid_fn("\n", &rp).props
                } else {
                    rp
                };
                out.push(Inline::Break(k, rp));
                *pos += 1;
            }
            // A field (one offset, its result formatting its own) or a
            // zero-length inline: unchanged.
            other => {
                *pos += inline_len(&other);
                out.push(other);
            }
        }
    }
    *content = out;
}

/// Apply `set(value)` to every run-character in `[start, end)`, splitting runs.
fn set_prop_range(
    content: &mut Vec<Inline>,
    start: usize,
    end: usize,
    set: fn(&mut RunProps, bool),
    value: bool,
) {
    edit_run_range(content, start, end, &|text, props| {
        let mut p = props.clone();
        set(&mut p, value);
        Run {
            text: text.to_string(),
            props: p,
        }
    });
}

/// Apply `f` to the run properties of every character in `[start, end)`.
fn map_prop_range(content: &mut Vec<Inline>, start: usize, end: usize, f: &dyn Fn(&mut RunProps)) {
    edit_run_range(content, start, end, &|text, props| {
        let mut p = props.clone();
        f(&mut p);
        Run {
            text: text.to_string(),
            props: p,
        }
    });
}

/// Apply a text transform `f` to every character in `[start, end)` (formatting
/// preserved).
fn map_text_range(content: &mut Vec<Inline>, start: usize, end: usize, f: &dyn Fn(&str) -> String) {
    edit_run_range(content, start, end, &|text, props| Run {
        text: f(text),
        props: props.clone(),
    });
}

/// Capitalize the first letter of each whitespace-separated word.
fn title_case(s: &str) -> String {
    let mut out = String::new();
    let mut start_of_word = true;
    for c in s.chars() {
        if c.is_whitespace() {
            start_of_word = true;
            out.push(c);
        } else if start_of_word {
            out.extend(c.to_uppercase());
            start_of_word = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Run properties that `content_insert` will give a character at this caret.
fn run_props_at(content: &[Inline], offset: usize) -> RunProps {
    // Typing into an emptied content control takes the control's formatting.
    if let Some(at) = empty_sdt_at(content, offset) {
        return sdt_typing_props(content, at);
    }
    let Some((i, local)) = locate(content, offset) else {
        return match content.last() {
            Some(Inline::Run(r)) => r.props.clone(),
            _ => end_props(content),
        };
    };
    match &content[i] {
        Inline::Run(r) => r.props.clone(),
        Inline::Hyperlink(h) => match link_part(h, local) {
            LinkPart::Runs(local) => {
                let mut run_acc = 0;
                h.runs
                    .iter()
                    .find(|r| {
                        let found = local <= run_acc + r.text.chars().count();
                        run_acc += r.text.chars().count();
                        found
                    })
                    .or_else(|| h.runs.last())
                    .map(|r| r.props.clone())
                    .unwrap_or_default()
            }
            LinkPart::Content(local) => run_props_at(&h.content, local),
        },
        _ => typing_props(content, i, local),
    }
}

/// Run properties a tab (or a break) inserted at this caret takes: what typing
/// there would give, except next to a hyperlink. The tab never lands inside
/// the link, so it takes the nearest source before the link rather than the
/// link's style.
fn tab_props_at(content: &[Inline], offset: usize) -> RunProps {
    match locate(content, offset) {
        // A tab into an emptied content control goes inside it, as typing.
        _ if empty_sdt_at(content, offset).is_some() => run_props_at(content, offset),
        Some((i, _)) if matches!(content[i], Inline::Hyperlink(_)) => {
            source_before(content, i).cloned().unwrap_or_default()
        }
        _ => run_props_at(content, offset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_run_formatting_keeps_highlight_622() {
        let styled = RunProps {
            bold: true,
            highlight: Some("yellow".into()),
            ..Default::default()
        };
        let mut ed = selected(vec![run("Mark", styled)], 0, 4);
        ed.clear_run_formatting();
        let Inline::Run(r) = &first_para(&ed).content[0] else {
            panic!("expected a run");
        };
        assert!(!r.props.bold);
        assert_eq!(r.props.highlight.as_deref(), Some("yellow"));
        let xml = crate::serialize::run_xml(r);
        assert!(xml.contains("<w:highlight w:val=\"yellow\"/>"), "{xml}");
        assert!(!xml.contains("<w:b/>"), "{xml}");
        // One undo step restores the bold.
        assert!(ed.undo());
        let Inline::Run(r) = &first_para(&ed).content[0] else {
            panic!("expected a run");
        };
        assert!(r.props.bold);
        assert_eq!(r.props.highlight.as_deref(), Some("yellow"));
    }

    #[test]
    fn clear_run_formatting_without_highlight_leaves_no_rpr_622() {
        let mut ed = selected(
            vec![run(
                "Mark",
                RunProps {
                    bold: true,
                    ..Default::default()
                },
            )],
            0,
            4,
        );
        ed.clear_run_formatting();
        let Inline::Run(r) = &first_para(&ed).content[0] else {
            panic!("expected a run");
        };
        assert_eq!(r.props, RunProps::default());
        assert!(!crate::serialize::run_xml(r).contains("w:rPr"));
    }

    #[test]
    fn run_props_at_paragraph_start_reads_first_run() {
        let content = vec![
            Inline::Run(Run {
                text: "a".into(),
                props: RunProps::default(),
            }),
            Inline::Run(Run {
                text: "b".into(),
                props: RunProps {
                    bold: true,
                    ..Default::default()
                },
            }),
        ];
        assert!(!run_props_at(&content, 0).bold);
        assert!(run_props_at(&content, 2).bold);
        let with_tab = vec![
            Inline::Tab(RunProps::default()),
            content[0].clone(),
            content[1].clone(),
        ];
        assert!(!run_props_at(&with_tab, 0).bold);
        assert!(!run_props_at(&with_tab, 1).bold);
        // A break is a formatted character, like a tab (#279): typing before
        // it (the character after) and right after it takes its own props.
        let with_break = vec![
            Inline::Break(BreakKind::Line, RunProps::default()),
            content[1].clone(),
        ];
        assert!(!run_props_at(&with_break, 0).bold);
        assert!(!run_props_at(&with_break, 1).bold);
        let bold = RunProps {
            bold: true,
            ..Default::default()
        };
        let with_bold_break = vec![Inline::Break(BreakKind::Line, bold), content[0].clone()];
        assert!(run_props_at(&with_bold_break, 0).bold);
        assert!(run_props_at(&with_bold_break, 1).bold);
        let with_empty = vec![
            Inline::Run(Run {
                text: String::new(),
                props: RunProps {
                    bold: true,
                    ..Default::default()
                },
            }),
            content[0].clone(),
        ];
        assert!(run_props_at(&with_empty, 0).bold);
    }

    #[test]
    fn run_props_at_matches_inserted_character_before_leading_inlines() {
        let bold = RunProps {
            bold: true,
            ..Default::default()
        };
        for mut content in [
            vec![
                Inline::Tab(RunProps::default()),
                Inline::Run(Run {
                    text: "x".into(),
                    props: bold.clone(),
                }),
            ],
            vec![
                Inline::Break(BreakKind::Line, RunProps::default()),
                Inline::Run(Run {
                    text: "x".into(),
                    props: bold.clone(),
                }),
            ],
            vec![
                Inline::Run(Run {
                    text: String::new(),
                    props: bold.clone(),
                }),
                Inline::Tab(RunProps::default()),
            ],
        ] {
            let expected = run_props_at(&content, 0);
            content_insert(&mut content, 0, 'z');
            let Inline::Run(inserted) = &content[0] else {
                panic!("inserted character is not in the first run")
            };
            assert!(inserted.text.starts_with('z'));
            assert_eq!(inserted.props, expected);
        }
    }

    /// Word's rule next to a tab, break or zero-width inline (#120): a typed
    /// character takes the formatting of the character before it (a tab's or a
    /// break's own, #279); at the paragraph start, that of the character after
    /// it. `run_props_at` must predict exactly what
    /// the insert produces.
    #[test]
    fn typing_next_to_a_non_run_follows_words_rule_120() {
        let go_back = || Inline::Raw(r#"<w:bookmarkStart w:id="0" w:name="_GoBack"/>"#.into());
        let plain = RunProps::default;
        let cases: Vec<(&str, Vec<Inline>, usize, bool)> = vec![
            (
                "after a trailing bold tab",
                vec![run("Name:", bold()), Inline::Tab(bold())],
                6,
                true,
            ),
            (
                "after a trailing plain tab: the tab's own formatting wins",
                vec![run("Name:", bold()), Inline::Tab(plain())],
                6,
                false,
            ),
            (
                "after a trailing bold line break",
                vec![run("Name", bold()), Inline::Break(BreakKind::Line, bold())],
                5,
                true,
            ),
            (
                "after a plain break in its own run, before a zero-width inline (#279)",
                vec![
                    run("Name", bold()),
                    Inline::Break(BreakKind::Line, plain()),
                    go_back(),
                ],
                5,
                false,
            ),
            (
                "after a bold break, before a plain run (#279)",
                vec![Inline::Break(BreakKind::Line, bold()), run("x", plain())],
                1,
                true,
            ),
            (
                "after a plain break, the next run does not win (#279)",
                vec![
                    run("a", plain()),
                    Inline::Break(BreakKind::Line, plain()),
                    run("b", bold()),
                ],
                2,
                false,
            ),
            (
                "after a bold break, before a bold tab",
                vec![
                    run("a", plain()),
                    Inline::Break(BreakKind::Line, bold()),
                    Inline::Tab(bold()),
                    run("x", bold()),
                ],
                2,
                true,
            ),
            (
                "after a bold tab starting a bold run (r1)",
                vec![
                    run("Name:", plain()),
                    Inline::Tab(bold()),
                    run("John", bold()),
                ],
                6,
                true,
            ),
            (
                "after a bold tab, before a zero-width inline",
                vec![Inline::Tab(bold()), go_back(), run("b", plain())],
                1,
                true,
            ),
            (
                "before a leading raw bookmark",
                vec![go_back(), run("x", bold())],
                0,
                true,
            ),
            (
                "before a leading field",
                vec![field("F"), run("x", bold())],
                0,
                true,
            ),
            (
                "before a leading bold tab",
                vec![Inline::Tab(bold()), run("x", plain())],
                0,
                true,
            ),
            (
                "before a leading plain tab: the tab is the character after",
                vec![Inline::Tab(plain()), run("x", bold())],
                0,
                false,
            ),
            (
                "before a leading plain break: the break is the character after",
                vec![Inline::Break(BreakKind::Line, plain()), run("x", bold())],
                0,
                false,
            ),
            (
                "before a leading bold break",
                vec![Inline::Break(BreakKind::Line, bold()), run("x", plain())],
                0,
                true,
            ),
        ];
        for (name, mut content, offset, want_bold) in cases {
            let expected = run_props_at(&content, offset);
            content_insert(&mut content, offset, 'z');
            assert_eq!(
                editor_text(&content).chars().nth(offset),
                Some('z'),
                "{name}"
            );
            let inserted = content
                .iter()
                .find_map(|i| match i {
                    Inline::Run(r) if r.text.contains('z') => Some(r),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name}: no run holds the typed char"));
            assert_eq!(
                inserted.props, expected,
                "{name}: run_props_at mispredicted"
            );
            assert_eq!(inserted.props.bold, want_bold, "{name}");
        }
    }

    #[test]
    fn typing_after_a_tab_or_break_joins_the_next_run_only_when_props_match_120() {
        let plain = RunProps::default;
        let mut same = vec![run("a", plain()), Inline::Tab(plain()), run("b", plain())];
        content_insert(&mut same, 2, 'z');
        assert_eq!(
            same,
            vec![run("a", plain()), Inline::Tab(plain()), run("zb", plain())]
        );
        let mut differs = vec![
            run("Name:", bold()),
            Inline::Tab(bold()),
            run("John", plain()),
        ];
        content_insert(&mut differs, 6, 'z');
        assert_eq!(
            differs,
            vec![
                run("Name:", bold()),
                Inline::Tab(bold()),
                run("z", bold()),
                run("John", plain()),
            ]
        );
        let mut after_break = vec![
            run("a", plain()),
            Inline::Break(BreakKind::Line, bold()),
            run("b", bold()),
        ];
        content_insert(&mut after_break, 2, 'z');
        assert_eq!(
            after_break,
            vec![
                run("a", plain()),
                Inline::Break(BreakKind::Line, bold()),
                run("zb", bold()),
            ]
        );
        // A plain break before a bold run: the typed char is plain, its own
        // run (#279).
        let mut differs_break = vec![
            run("a", plain()),
            Inline::Break(BreakKind::Line, plain()),
            run("b", bold()),
        ];
        content_insert(&mut differs_break, 2, 'z');
        assert_eq!(
            differs_break,
            vec![
                run("a", plain()),
                Inline::Break(BreakKind::Line, plain()),
                run("z", plain()),
                run("b", bold()),
            ]
        );
    }

    #[test]
    fn typing_after_an_inserted_tab_keeps_bold_120() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![run("Name:", bold())],
            })],
        });
        ed.caret = Caret::at(vec![0], 5);
        ed.insert_tab();
        assert_eq!(
            first_para(&ed).content[1],
            Inline::Tab(bold()),
            "a tab typed in bold text is bold"
        );
        assert!(ed.caret_props().bold, "the ribbon shows Bold after the tab");
        ed.insert_char('x');
        assert_eq!(etext(&ed), "Name:\tx");
        assert_eq!(
            first_para(&ed).content,
            vec![run("Name:", bold()), Inline::Tab(bold()), run("x", bold())]
        );
    }

    #[test]
    fn a_tab_inserted_next_to_a_hyperlink_does_not_take_its_style_120() {
        let link_props = RunProps {
            underline: true,
            color: Some("0563C1".into()),
            style_id: Some("Hyperlink".into()),
            ..Default::default()
        };
        let link = Inline::Hyperlink(Hyperlink {
            target: Some("https://example.com".into()),
            anchor: None,
            rel_id: None,
            runs: vec![Run {
                text: "site".into(),
                props: link_props.clone(),
            }],
            content: Vec::new(),
            raw: None,
            content_changed: false,
        });
        for (content, caret, want) in [
            (vec![link.clone()], 4, RunProps::default()),
            (vec![run("see ", bold()), link.clone()], 8, bold()),
        ] {
            let mut ed = Editor::new(Document {
                body: vec![Block::Paragraph(Paragraph {
                    props: ParProps::default(),
                    content,
                })],
            });
            ed.caret = Caret::at(vec![0], caret);
            assert_eq!(ed.caret_props(), link_props, "typing here extends the link");
            ed.insert_tab();
            ed.insert_char('x');
            let tab = first_para(&ed)
                .content
                .iter()
                .find_map(|i| match i {
                    Inline::Tab(props) => Some(props.clone()),
                    _ => None,
                })
                .expect("a tab was inserted");
            assert_eq!(tab, want, "the tab outside the link");
            let x = first_para(&ed)
                .content
                .iter()
                .find_map(|i| match i {
                    Inline::Run(r) if r.text.contains('x') => Some(r.props.clone()),
                    _ => None,
                })
                .expect("x is in a plain run outside the link");
            assert_eq!(x, want, "text typed after the tab");
        }
    }

    /// An editor over one paragraph holding `content`, with `[start, end)`
    /// selected.
    fn selected(content: Vec<Inline>, start: usize, end: usize) -> Editor {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content,
            })],
        });
        ed.anchor = Some(Caret::at(vec![0], start));
        ed.caret = Caret::at(vec![0], end);
        ed
    }

    /// Formatting a selection formats the tabs in it too (a tab is a run in
    /// OOXML), so typing after a tab keeps the line's formatting (#120 r2).
    #[test]
    fn bolding_a_line_bolds_its_tab_and_typing_after_it_is_bold_120() {
        let plain = RunProps::default;
        let mut ed = selected(
            vec![
                run("Name:", plain()),
                Inline::Tab(plain()),
                run("John", plain()),
            ],
            0,
            10,
        );
        ed.toggle_bold();
        assert_eq!(
            first_para(&ed).content,
            vec![
                run("Name:", bold()),
                Inline::Tab(bold()),
                run("John", bold())
            ]
        );
        ed.anchor = None;
        ed.caret = Caret::at(vec![0], 6);
        assert!(ed.caret_props().bold);
        ed.insert_char('x');
        assert_eq!(
            first_para(&ed).content,
            vec![
                run("Name:", bold()),
                Inline::Tab(bold()),
                run("xJohn", bold())
            ]
        );
    }

    #[test]
    fn unbolding_a_line_unbolds_its_tab_120() {
        let plain = RunProps::default;
        let mut ed = selected(vec![run("Name:", bold()), Inline::Tab(bold())], 0, 6);
        ed.toggle_bold();
        assert_eq!(
            first_para(&ed).content,
            vec![run("Name:", plain()), Inline::Tab(plain())]
        );
        ed.anchor = None;
        ed.caret = Caret::at(vec![0], 6);
        ed.insert_char('x');
        assert_eq!(
            first_para(&ed).content,
            vec![
                run("Name:", plain()),
                Inline::Tab(plain()),
                run("x", plain())
            ]
        );
    }

    #[test]
    fn a_plain_tab_in_bold_text_makes_the_toggle_bold_everything_120() {
        let mut ed = selected(
            vec![
                run("Name:", bold()),
                Inline::Tab(RunProps::default()),
                run("John", bold()),
            ],
            0,
            10,
        );
        ed.toggle_bold();
        assert_eq!(
            first_para(&ed).content,
            vec![
                run("Name:", bold()),
                Inline::Tab(bold()),
                run("John", bold())
            ],
            "not every selected character was bold, so Ctrl+B turns bold on"
        );
    }

    #[test]
    fn a_tab_only_selection_formats_just_the_tab_120() {
        let plain = RunProps::default;
        let mut ed = selected(
            vec![run("a", plain()), Inline::Tab(plain()), run("b", plain())],
            1,
            2,
        );
        ed.toggle_bold();
        assert_eq!(
            first_para(&ed).content,
            vec![run("a", plain()), Inline::Tab(bold()), run("b", plain())]
        );
        ed.set_font_size(28);
        ed.set_color(Some("FF0000".into()));
        let Inline::Tab(props) = &first_para(&ed).content[1] else {
            panic!("the tab is still a tab")
        };
        assert_eq!(props.size_half_pts, Some(28));
        assert_eq!(props.color.as_deref(), Some("FF0000"));
        assert_eq!(first_para(&ed).content[0], run("a", plain()));
        assert_eq!(first_para(&ed).content[2], run("b", plain()));
    }

    #[test]
    fn clearing_formatting_clears_a_tabs_props_120() {
        let mut ed = selected(vec![run("a", bold()), Inline::Tab(bold())], 0, 2);
        ed.clear_run_formatting();
        assert_eq!(
            first_para(&ed).content,
            vec![
                run("a", RunProps::default()),
                Inline::Tab(RunProps::default())
            ]
        );
    }

    #[test]
    fn changing_case_over_a_tab_keeps_it_a_tab_120() {
        let plain = RunProps::default;
        let mut ed = selected(
            vec![run("ab", plain()), Inline::Tab(plain()), run("cd", plain())],
            0,
            5,
        );
        ed.cycle_case();
        assert_eq!(
            first_para(&ed).content,
            vec![run("Ab", plain()), Inline::Tab(plain()), run("Cd", plain())]
        );
    }

    fn para(text: &str) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: text.to_string(),
                props: RunProps::default(),
            })],
        })
    }
    fn doc(paras: &[&str]) -> Document {
        Document {
            body: paras.iter().map(|t| para(t)).collect(),
        }
    }
    fn top_text(ed: &Editor) -> Vec<String> {
        ed.doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p.plain_text()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn editor_remove_hidden_text_one_undo_step_and_caret_clamped() {
        let hidden = RunProps {
            vanish: true,
            ..RunProps::default()
        };
        let mut d = doc(&["first", "seen"]);
        if let Block::Paragraph(p) = &mut d.body[1] {
            p.content.push(Inline::Run(Run {
                text: " unseen".into(),
                props: hidden,
            }));
        }
        let mut ed = Editor::new(d);
        ed.caret = Caret {
            path: vec![1],
            offset: "seen unseen".len(),
        };
        assert_eq!(ed.remove_hidden_text(), 1);
        assert_eq!(top_text(&ed), ["first", "seen"]);
        assert_eq!(ed.caret.offset, "seen".len(), "caret clamped");
        // A no-op pushes no undo step: one undo restores the hidden run and
        // the caret, and there is nothing further to undo.
        assert_eq!(ed.remove_hidden_text(), 0);
        assert!(ed.undo());
        assert_eq!(top_text(&ed), ["first", "seen unseen"]);
        assert_eq!(ed.caret.offset, "seen unseen".len());
        assert!(!ed.undo());
    }

    /// #917: deleting one comment keeps the text that shares a raw run with
    /// its reference, leaves other comments alone, and is one undo step.
    #[test]
    fn editor_remove_comment_markers_keeps_text_and_is_one_undo_step_917() {
        let mut d = doc(&["a"]);
        if let Block::Paragraph(p) = &mut d.body[0] {
            p.content.extend([
                Inline::Raw("<w:commentRangeStart w:id=\"10\"/>".into()),
                Inline::Raw("<w:r><w:t>hello</w:t><w:commentReference w:id=\"0\"/></w:r>".into()),
                Inline::Raw("<w:r><w:commentReference w:id=\"10\"/></w:r>".into()),
            ]);
        }
        let mut ed = Editor::new(d.clone());
        assert_eq!(ed.remove_comment_markers("0"), 1);
        let xml = crate::serialize::document_to_xml(&ed.doc);
        assert!(xml.contains("<w:r><w:t>hello</w:t></w:r>"), "{xml}");
        assert!(!xml.contains("w:id=\"0\""), "{xml}");
        assert_eq!(xml.matches("w:id=\"10\"").count(), 2, "{xml}");
        // Nothing left of comment 0: no change, no undo step.
        assert_eq!(ed.remove_comment_markers("0"), 0);
        assert!(ed.undo());
        assert_eq!(ed.doc, d);
        assert!(!ed.undo());
    }

    /// #620: adding a comment is one undo step that takes all three markers,
    /// and redo puts all three back where they were.
    #[test]
    fn add_comment_is_one_undo_step() {
        let before = doc(&["The quick brown fox."]);
        let mut ed = Editor::new(before.clone());
        ed.anchor = Some(Caret {
            path: vec![0],
            offset: 10,
        });
        ed.caret.offset = 15;
        assert!(ed.add_comment("1"));
        let added = ed.doc.clone();
        let xml = crate::serialize::document_to_xml(&added);
        assert_eq!(xml.matches("w:id=\"1\"").count(), 3, "{xml}");
        assert!(ed.undo());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo(), "one step only");
        assert!(ed.redo());
        assert_eq!(ed.doc, added);
        assert_eq!(
            crate::inspect::comment_marker_ids(&ed.doc)
                .into_iter()
                .collect::<Vec<_>>(),
            ["1"]
        );
    }

    #[test]
    fn editor_remove_all_comment_markers_is_one_undo_step() {
        let mut d = doc(&["a", "b"]);
        for (i, b) in d.body.iter_mut().enumerate() {
            if let Block::Paragraph(p) = b {
                p.content.insert(
                    0,
                    Inline::Raw(format!("<w:commentRangeStart w:id=\"{i}\"/>")),
                );
                p.content
                    .push(Inline::Raw(format!("<w:commentRangeEnd w:id=\"{i}\"/>")));
            }
        }
        let mut ed = Editor::new(d.clone());
        assert_eq!(ed.remove_all_comment_markers(), 4);
        assert_eq!(ed.remove_all_comment_markers(), 0);
        assert!(ed.undo());
        assert_eq!(ed.doc, d);
        assert!(!ed.undo());
    }

    #[test]
    fn caret_enters_and_edits_a_text_box() {
        // A host paragraph carrying a text box whose content is "hi".
        let host = Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::TextBox {
                raw: "<w:r><w:pict><w:txbxContent><w:p/></w:txbxContent></w:pict></w:r>"
                    .to_string(),
                blocks: vec![para("hi")],
            }],
        });
        let mut ed = Editor::new(Document { body: vec![host] });
        // Navigation reaches the text box paragraph (path [0, 0, 0]).
        let paths = all_paragraph_paths(&ed.doc.body);
        assert!(
            paths.contains(&vec![0, 0, 0]),
            "text box not navigable: {paths:?}"
        );
        // Place the caret inside the box and type.
        ed.caret = Caret::at(vec![0, 0, 0], 2);
        ed.insert_char('!');
        // The edit lands in the text box's content, not the host paragraph.
        if let Block::Paragraph(p) = &ed.doc.body[0] {
            match &p.content[0] {
                Inline::TextBox { blocks, .. } => {
                    assert_eq!(blocks[0].plain_text(), "hi!");
                }
                other => panic!("expected TextBox, got {other:?}"),
            }
        }
    }

    #[test]
    fn paragraph_indent_list_and_sort() {
        let mut ed = Editor::new(doc(&["banana", "apple", "cherry"]));
        ed.select_all();
        ed.change_indent(720);
        ed.sort_paragraphs();
        assert_eq!(top_text(&ed), vec!["apple", "banana", "cherry"]);
        for b in &ed.doc.body {
            if let Block::Paragraph(p) = b {
                assert_eq!(p.props.indent, 720);
            }
        }
        // decrease clamps at 0
        ed.select_all();
        ed.change_indent(-9999);
        if let Block::Paragraph(p) = &ed.doc.body[0] {
            assert_eq!(p.props.indent, 0);
        }
        // list toggle
        ed.select_all();
        ed.set_list(Some(42));
        assert!(ed.all_in_list(42));
        ed.set_list(None);
        assert!(!ed.all_in_list(42));
    }

    #[test]
    fn font_formatting_applies_over_the_selection() {
        let mut ed = Editor::new(doc(&["hello"]));
        ed.select_all();
        ed.resize_font(2); // 11pt default (22) → 12pt (24)
        ed.set_color(Some("FF0000".to_string()));
        ed.set_font("Arial");
        ed.toggle_vert_align(VertAlign::Superscript);
        match &ed.doc.body[0] {
            Block::Paragraph(p) => match &p.content[0] {
                Inline::Run(r) => {
                    assert_eq!(r.props.size_half_pts, Some(24));
                    assert_eq!(r.props.color.as_deref(), Some("FF0000"));
                    assert_eq!(r.props.font.as_deref(), Some("Arial"));
                    assert_eq!(r.props.vert_align, VertAlign::Superscript);
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
        // cycle case: all-lower → Capitalize
        ed.select_all();
        ed.cycle_case();
        assert_eq!(top_text(&ed), vec!["Hello"]);
        // clear formatting resets the run props
        ed.select_all();
        ed.clear_run_formatting();
        match &ed.doc.body[0] {
            Block::Paragraph(p) => match &p.content[0] {
                Inline::Run(r) => assert_eq!(r.props, RunProps::default()),
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn hrule_autoformat_makes_a_horizontal_line() {
        let mut ed = Editor::new(doc(&["---", "next"]));
        ed.caret.offset = 3; // end of "---"
        assert!(ed.hrule_autoformat());
        // The "---" paragraph is now an empty rule (bottom border).
        match &ed.doc.body[0] {
            Block::Paragraph(p) => {
                assert!(p.content.is_empty());
                assert_eq!(p.props.borders.bottom, Some(BorderKind::Single));
            }
            _ => panic!(),
        }
        // A fresh paragraph below holds the caret and carries no border.
        match &ed.doc.body[1] {
            Block::Paragraph(p) => assert_eq!(p.props.borders.bottom, None),
            _ => panic!(),
        }
        // Plain text is not a trigger.
        let mut ed2 = Editor::new(doc(&["hello"]));
        ed2.caret.offset = 5;
        assert!(!ed2.hrule_autoformat());
    }

    #[test]
    fn insert_in_middle() {
        let mut ed = Editor::new(doc(&["helo"]));
        ed.caret.offset = 3;
        ed.insert_char('l');
        assert_eq!(top_text(&ed), vec!["hello"]);
        assert_eq!(ed.caret.offset, 4);
    }

    #[test]
    fn typing_inherits_run_style() {
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let d = Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "ab".to_string(),
                    props: bold,
                })],
            })],
        };
        let mut ed = Editor::new(d);
        ed.caret.offset = 1;
        ed.insert_char('X');
        if let Block::Paragraph(p) = &ed.doc.body[0] {
            assert_eq!(p.content.len(), 1);
            if let Inline::Run(r) = &p.content[0] {
                assert_eq!(r.text, "aXb");
                assert!(r.props.bold);
            }
        }
    }

    #[test]
    fn backspace_deletes_and_merges() {
        let mut ed = Editor::new(doc(&["ab", "cd"]));
        ed.caret = Caret::top(0, 2);
        ed.backspace();
        assert_eq!(top_text(&ed), vec!["a", "cd"]);
        ed.caret = Caret::top(1, 0);
        ed.backspace();
        assert_eq!(top_text(&ed), vec!["acd"]);
        assert_eq!(ed.caret, Caret::top(0, 1));
    }

    #[test]
    fn newline_splits_paragraph() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.caret.offset = 2;
        ed.insert_newline();
        assert_eq!(top_text(&ed), vec!["ab", "cd"]);
        assert_eq!(ed.caret, Caret::top(1, 0));
    }

    #[test]
    fn delete_forward_and_merge_next() {
        let mut ed = Editor::new(doc(&["ab", "cd"]));
        ed.caret = Caret::top(0, 0);
        ed.delete_forward();
        assert_eq!(top_text(&ed), vec!["b", "cd"]);
        ed.caret = Caret::top(0, 1);
        ed.delete_forward();
        assert_eq!(top_text(&ed), vec!["bcd"]);
    }

    /// "xy" with the caret at 1 and an anchor on it: the state a plain click
    /// (or an empty-range harness `selection-set`) leaves (#698).
    fn clicked_at_1() -> Editor {
        let mut ed = Editor::new(doc(&["xy"]));
        ed.set_caret(Caret::top(0, 1));
        ed.extend_selection(true);
        assert!(!ed.has_selection());
        ed
    }

    #[test]
    fn typing_after_a_click_leaves_no_selection_698() {
        let mut ed = clicked_at_1();
        ed.insert_str("ab");
        assert_eq!(top_text(&ed), vec!["xaby"]);
        assert_eq!(ed.caret, Caret::top(0, 3));
        assert_eq!(ed.anchor, None);
        assert!(!ed.has_selection());
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["xy"]);
        assert!(!ed.has_selection());
    }

    #[test]
    fn deleting_after_a_click_leaves_no_selection_698() {
        let mut ed = clicked_at_1();
        ed.backspace();
        assert_eq!(top_text(&ed), vec!["y"]);
        assert_eq!(ed.caret, Caret::top(0, 0));
        assert!(!ed.has_selection());

        let mut ed = clicked_at_1();
        ed.delete_forward();
        assert_eq!(top_text(&ed), vec!["x"]);
        assert_eq!(ed.caret, Caret::top(0, 1));
        assert!(!ed.has_selection());
    }

    #[test]
    fn tab_after_a_click_leaves_no_selection_698() {
        let mut ed = clicked_at_1();
        ed.insert_tab();
        assert_eq!(ed.caret, Caret::top(0, 2));
        assert!(!ed.has_selection());
        ed.insert_char('z');
        assert_eq!(top_text(&ed), vec!["x\tzy"]);
    }

    #[test]
    fn enter_after_a_click_leaves_no_selection_698() {
        let mut ed = clicked_at_1();
        ed.insert_newline();
        assert_eq!(top_text(&ed), vec!["x", "y"]);
        assert_eq!(ed.caret, Caret::top(1, 0));
        assert_eq!(ed.anchor, None);
        // A stale anchor would select the new paragraph mark, and this key
        // would merge the paragraphs back together.
        ed.insert_char('z');
        assert_eq!(top_text(&ed), vec!["x", "zy"]);
    }

    #[test]
    fn horizontal_line_after_a_click_leaves_no_selection_698() {
        let mut ed = clicked_at_1();
        ed.insert_hrule();
        assert_eq!(ed.anchor, None);
        // A stale anchor at (0, 1) would select "y" and the new paragraph
        // mark, and this key would replace them.
        ed.insert_char('z');
        assert_eq!(top_text(&ed), vec!["xy", "z"]);
    }

    #[test]
    fn rule_autoformat_after_a_click_leaves_no_selection_698() {
        let mut ed = Editor::new(doc(&["---"]));
        ed.set_caret(Caret::top(0, 3));
        ed.extend_selection(true);
        assert!(ed.hrule_autoformat());
        assert_eq!(ed.anchor, None);
        ed.insert_char('z');
        assert_eq!(top_text(&ed), vec!["", "z"]);
    }

    /// #619: coalesced typing is one step named by its text, and other
    /// edits fall back to their kind's name.
    #[test]
    fn undo_steps_are_named_newest_first() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("one");
        ed.insert_newline();
        ed.insert_str("two");
        ed.backspace();
        assert_eq!(
            ed.undo_names(),
            vec!["Delete", "Typing \"two\"", "Edit", "Typing \"one\""]
        );
        assert_eq!(ed.last_typed(), None, "the newest step is the delete");
        ed.insert_str("o");
        assert_eq!(ed.last_typed(), Some("o"));
    }

    #[test]
    fn long_typing_label_is_shortened_but_the_text_is_kept() {
        let mut ed = Editor::new(doc(&[""]));
        let text = "abcdefghijklmnopqrstuvwxyz0123456789";
        ed.insert_str(text);
        assert_eq!(
            ed.undo_names(),
            vec!["Typing \"abcdefghijklmnopqrstuvwxyz0123\u{2026}\""]
        );
        assert_eq!(ed.last_typed(), Some(text));
    }

    #[test]
    fn step_names_and_serials_travel_through_undo_and_redo() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("ab");
        let typed = ed.undo_serial();
        ed.select_all();
        ed.toggle_bold();
        let bold = ed.undo_serial();
        assert_ne!(typed, bold);
        ed.name_command(typed.unwrap(), "Bold");
        assert_eq!(ed.undo_names(), vec!["Bold", "Typing \"ab\""]);
        assert!(ed.undo());
        assert_eq!(ed.undo_serial(), typed);
        assert_eq!(ed.undo_names(), vec!["Typing \"ab\""]);
        assert!(ed.can_redo());
        assert!(ed.redo());
        assert_eq!(
            ed.undo_serial(),
            bold,
            "redo brings the step back as it was"
        );
        assert_eq!(ed.undo_names(), vec!["Bold", "Typing \"ab\""]);
        assert!(!ed.can_redo());
    }

    #[test]
    fn undo_serials_are_unique_across_editors() {
        let mut a = Editor::new(doc(&[""]));
        let mut b = Editor::new(doc(&[""]));
        a.insert_str("x");
        b.insert_str("x");
        assert!(a.undo_serial().is_some() && b.undo_serial().is_some());
        assert_ne!(a.undo_serial(), b.undo_serial());
        let before = undo_serial_counter();
        a.insert_newline();
        assert!(a.undo_serial().unwrap() > before);
    }

    #[test]
    fn name_command_merges_only_newer_steps() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("ab");
        let since = undo_serial_counter();
        // Coalesces into the typing step from before: no new step to name.
        ed.insert_str("c");
        ed.name_command(since, "Symbol");
        assert_eq!(ed.undo_names(), vec!["Typing \"abc\""]);
        ed.break_undo_group();
        ed.insert_str("d");
        ed.insert_newline();
        ed.name_command(since, "Insert Stuff");
        assert_eq!(ed.undo_names(), vec!["Insert Stuff", "Typing \"abc\""]);
        assert!(ed.undo());
        assert_eq!(
            top_text(&ed),
            vec!["abc"],
            "one undo takes the whole command"
        );
        assert!(ed.redo());
        // Typing after a named command starts its own step.
        let since = undo_serial_counter();
        ed.insert_str("e");
        ed.name_command(since, "Named");
        ed.insert_str("f");
        assert_eq!(ed.undo_names()[..2], ["Typing \"f\"", "Named"]);
    }

    /// A command that edits through several calls (No Spacing: style, two
    /// spacings, line spacing) is one step once named.
    #[test]
    fn name_command_makes_a_multi_call_command_one_step() {
        let mut ed = Editor::new(doc(&["a", "b"]));
        ed.insert_str("x");
        let typed = ed.undo_serial();
        let before = ed.doc.clone();
        let since = undo_serial_counter();
        ed.break_undo_group();
        ed.set_space_before(Some(0));
        ed.set_space_after(Some(0));
        ed.set_line_spacing(240, "auto");
        assert_eq!(ed.undo_names().len(), 4);
        let last = undo_serial_counter();
        ed.name_command(since, "Style");
        assert_eq!(ed.undo_names(), vec!["Style", "Typing \"x\""]);
        let kept = ed.undo_serial().unwrap();
        assert!(kept > since && kept < last, "the oldest new step's serial");
        let after = ed.doc.clone();
        assert!(ed.undo());
        assert_eq!(ed.doc, before);
        assert_eq!(ed.undo_serial(), typed);
        assert!(ed.redo());
        assert_eq!(ed.doc, after);
        // Nothing new since: nothing changes.
        ed.name_command(undo_serial_counter(), "Other");
        assert_eq!(ed.undo_names(), vec!["Style", "Typing \"x\""]);
    }

    /// Typing over a selection is one `Typing` step: one undo puts the
    /// selected text back and removes what was typed.
    #[test]
    fn typing_over_a_selection_is_one_typing_step() {
        let mut ed = Editor::new(doc(&["hello world"]));
        let before = ed.doc.clone();
        ed.anchor = Some(Caret::at(vec![0], 6));
        ed.caret = Caret::at(vec![0], 11);
        ed.insert_str("rust");
        assert_eq!(top_text(&ed), vec!["hello rust"]);
        assert_eq!(ed.undo_names(), vec!["Typing \"rust\""]);
        assert!(ed.undo());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    #[test]
    fn break_undo_group_starts_a_new_typing_step() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("abc");
        ed.break_undo_group();
        ed.insert_str("abc");
        assert_eq!(top_text(&ed), vec!["abcabc"]);
        assert_eq!(ed.undo_names(), vec!["Typing \"abc\""; 2]);
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["abc"]);
    }

    #[test]
    fn undo_to_undoes_n_steps_and_redo_walks_them_back() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("one");
        ed.insert_newline();
        ed.insert_str("two");
        ed.select_all();
        ed.toggle_bold();
        assert_eq!(ed.undo_names().len(), 4);
        let full = ed.doc.clone();
        assert!(!ed.undo_to(0));
        assert!(!ed.undo_to(5), "more steps than there are");
        assert_eq!(ed.doc, full, "a refused undo_to changes nothing");
        assert!(!ed.can_redo());
        assert!(ed.undo_to(3));
        assert_eq!(top_text(&ed), vec!["one"]);
        assert_eq!(ed.undo_names(), vec!["Typing \"one\""]);
        assert!(ed.redo());
        assert_eq!(top_text(&ed), vec!["one", ""], "redo restores Enter only");
        assert!(ed.redo() && ed.redo());
        assert_eq!(ed.doc, full);
        assert!(!ed.redo());
    }

    #[test]
    fn undo_cap_drops_the_oldest_name_with_its_step() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("first");
        ed.break_undo_group();
        // Two steps a round in one short paragraph, so the cap is cheap to
        // reach: a typed character and the Backspace that takes it away.
        for _ in 0..UNDO_CAP / 2 {
            ed.insert_char('x');
            ed.backspace();
        }
        let names = ed.undo_names();
        assert_eq!(names.len(), UNDO_CAP);
        assert_eq!(names.last().map(String::as_str), Some("Typing \"x\""));
        assert!(
            !names.iter().any(|n| n == "Typing \"first\""),
            "the typing step is gone"
        );
    }

    /// The text of the caret's paragraph after each of `n` undos.
    fn texts_after_undos(ed: &mut Editor, n: usize) -> Vec<String> {
        (0..n)
            .map(|_| {
                assert!(ed.undo());
                ed.cur_text()
            })
            .collect()
    }

    /// #853: a typing step holds at most 128 characters, as in Word: 300
    /// typed characters are three steps, the newest holding the last 44.
    #[test]
    fn a_typing_step_holds_at_most_128_characters_853() {
        let mut ed = Editor::new(doc(&[""]));
        let typed: String = ('a'..='z').cycle().take(300).collect();
        ed.insert_str(&typed);
        let names = ed.undo_names();
        assert_eq!(names.len(), 3);
        assert!(names.iter().all(|n| n.starts_with("Typing")));
        assert_eq!(ed.last_typed().map(|t| t.chars().count()), Some(44));
        let lens: Vec<usize> = texts_after_undos(&mut ed, 3)
            .iter()
            .map(|t| t.chars().count())
            .collect();
        assert_eq!(lens, vec![256, 128, 0]);
        assert_eq!(top_text(&ed), vec![""]);
        assert!(!ed.undo());
    }

    /// #853: each Backspace is an undo step of its own.
    #[test]
    fn each_backspace_is_one_undo_step_853() {
        let mut ed = Editor::new(doc(&["Hello world"]));
        ed.caret.offset = 11;
        for _ in 0..3 {
            ed.backspace();
        }
        assert_eq!(top_text(&ed), vec!["Hello wo"]);
        assert_eq!(ed.undo_names(), vec!["Delete"; 3]);
        assert_eq!(
            texts_after_undos(&mut ed, 3),
            vec!["Hello wor", "Hello worl", "Hello world"]
        );
        assert!(!ed.undo());
    }

    /// #853: each Delete is an undo step of its own.
    #[test]
    fn each_delete_is_one_undo_step_853() {
        let mut ed = Editor::new(doc(&["Hello world"]));
        for _ in 0..3 {
            ed.delete_forward();
        }
        assert_eq!(top_text(&ed), vec!["lo world"]);
        assert_eq!(
            texts_after_undos(&mut ed, 3),
            vec!["llo world", "ello world", "Hello world"]
        );
        assert!(!ed.undo());
    }

    /// #853: Word's sequence: typing, two Backspaces, typing are four steps.
    #[test]
    fn backspaces_between_typing_are_steps_of_their_own_853() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("Abcdef");
        ed.backspace();
        ed.backspace();
        ed.insert_str("gh");
        assert_eq!(top_text(&ed), vec!["Abcdgh"]);
        assert_eq!(
            texts_after_undos(&mut ed, 4),
            vec!["Abcd", "Abcde", "Abcdef", ""]
        );
        assert!(!ed.undo());
    }

    /// #853: a line break typed with the text around it is part of its
    /// typing step, so one Undo empties the paragraph.
    #[test]
    fn a_line_break_stays_inside_the_typing_step_853() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_str("Six");
        ed.insert_line_break();
        ed.insert_str("Seven eight");
        assert_eq!(ed.doc.body.len(), 1, "a line break, not a paragraph");
        let Some(Block::Paragraph(p)) = ed.doc.body.first() else {
            panic!("a paragraph");
        };
        assert!(matches!(p.content[1], Inline::Break(BreakKind::Line, _)));
        assert_eq!(ed.undo_names(), vec!["Typing \"Six\u{21b5}Seven eight\""]);
        assert_eq!(ed.last_typed(), Some("Six\u{000b}Seven eight"));
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec![""]);
        assert!(!ed.undo());
    }

    /// #853: typing the recorded text again (Repeat) types the line break
    /// as a line break, not a new paragraph.
    #[test]
    fn a_recorded_line_break_types_again_as_a_line_break_853() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_line_break();
        ed.insert_str("a");
        let typed = ed.last_typed().unwrap().to_owned();
        ed.break_undo_group();
        ed.insert_str(&typed);
        assert_eq!(ed.doc.body.len(), 1);
        let Some(Block::Paragraph(p)) = ed.doc.body.first() else {
            panic!("a paragraph");
        };
        let breaks = p
            .content
            .iter()
            .filter(|i| matches!(i, Inline::Break(BreakKind::Line, _)))
            .count();
        assert_eq!(breaks, 2);
    }

    /// #853: undoing a deleted selection selects the text again.
    #[test]
    fn undoing_a_deleted_selection_selects_it_again_853() {
        let mut ed = Editor::new(doc(&["Hello world"]));
        ed.anchor = Some(Caret::at(vec![0], 6));
        ed.caret = Caret::at(vec![0], 11);
        ed.backspace();
        assert_eq!(top_text(&ed), vec!["Hello "]);
        assert_eq!(ed.anchor, None);
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["Hello world"]);
        assert_eq!(ed.anchor, Some(Caret::at(vec![0], 6)));
        assert_eq!(ed.caret, Caret::at(vec![0], 11));
        // Redo deletes it again, leaving no selection.
        assert!(ed.redo());
        assert_eq!(top_text(&ed), vec!["Hello "]);
        assert_eq!(ed.anchor, None);
    }

    /// #853: so does undoing typing over a selection, and across paragraphs.
    #[test]
    fn undoing_typing_over_a_selection_selects_it_again_853() {
        let mut ed = Editor::new(doc(&["Hello world"]));
        ed.anchor = Some(Caret::at(vec![0], 6));
        ed.caret = Caret::at(vec![0], 11);
        ed.insert_str("rust");
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["Hello world"]);
        assert_eq!(ed.anchor, Some(Caret::at(vec![0], 6)));
        assert_eq!(ed.caret, Caret::at(vec![0], 11));

        let mut ed = Editor::new(doc(&["one", "two"]));
        ed.anchor = Some(Caret::at(vec![0], 1));
        ed.caret = Caret::at(vec![1], 2);
        ed.delete_forward();
        assert_eq!(top_text(&ed), vec!["oo"]);
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["one", "two"]);
        assert_eq!(ed.anchor, Some(Caret::at(vec![0], 1)));
        assert_eq!(ed.caret, Caret::at(vec![1], 2));
    }

    /// #853: the history reaches back past 500 steps: 2,000 Enters undo.
    #[test]
    fn two_thousand_enters_all_undo_853() {
        let mut ed = Editor::new(doc(&[""]));
        for _ in 0..2000 {
            ed.insert_newline();
        }
        assert_eq!(ed.doc.body.len(), 2001);
        assert_eq!(ed.undo_names().len(), 2000);
        for _ in 0..2000 {
            assert!(ed.undo());
        }
        assert_eq!(top_text(&ed), vec![""]);
        assert!(!ed.undo());
    }

    /// #853: the history's memory budget drops the oldest steps once their
    /// copies pass it, but never below the 500 steps kept before: an edit in
    /// a table cell copies the whole table.
    #[test]
    fn the_history_budget_keeps_at_least_500_steps_853() {
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(s)],
            ..Default::default()
        };
        let row = |i: usize| Row {
            cells: (0..4).map(|c| cell(&format!("cell {i} {c}"))).collect(),
            ..Default::default()
        };
        let table = Document {
            body: vec![Block::Table(Table {
                grid: vec![100; 4],
                rows: (0..20).map(row).collect(),
                ..Default::default()
            })],
        };
        let table_weight = block_weight(&table.body[0]);
        let edits = |budget: usize| {
            let mut ed = Editor::new(table.clone());
            ed.history_budget = budget;
            ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
            for _ in 0..600 {
                ed.break_undo_group();
                ed.insert_char('x');
            }
            let weight: usize = ed.undo.iter().map(|s| s.weight).sum();
            (ed.undo.len(), weight)
        };
        // Each step copies the table, about `table_weight` bytes.
        let (kept, _) = edits(usize::MAX);
        assert_eq!(kept, 600, "no budget: every step");
        let budget = table_weight * 550;
        let (kept, weight) = edits(budget);
        assert!(kept > UNDO_FLOOR && kept < 600, "{kept} steps");
        assert!(weight <= budget);
        let (kept, _) = edits(table_weight);
        assert_eq!(kept, UNDO_FLOOR, "never fewer than the floor");
    }

    /// #853: a command run through `one_step` takes one step however many
    /// edits it makes, so even past the floor and the budget one undo puts
    /// back the state before it (`600x` in a heavy table cell).
    #[test]
    fn one_step_never_pushes_out_its_own_first_step_853() {
        let long = "z".repeat(700);
        let mut ed = Editor::new(Document {
            body: vec![Block::Table(Table {
                grid: vec![100],
                rows: vec![Row {
                    cells: vec![Cell {
                        grid_span: 1,
                        v_merge: VMerge::None,
                        blocks: vec![para(&long)],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            })],
        });
        let before = ed.doc.clone();
        ed.history_budget = 1;
        ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
        ed.one_step("Delete", |ed| {
            for _ in 0..600 {
                ed.delete_forward();
            }
        });
        assert_eq!(ed.undo_names(), vec!["Delete"]);
        assert!(ed.undo());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    /// #853: inside `one_step` only the first edit builds a snapshot: a
    /// 500-line paste as if typed, or 500 Deletes, copy the document once.
    #[test]
    fn one_step_builds_one_snapshot_853() {
        let taken = || SNAPSHOTS_TAKEN.with(std::cell::Cell::get);
        let mut ed = Editor::new(doc(&["abc"]));
        let lines = "line of text\n".repeat(500);
        let start = taken();
        ed.one_step("Paste", |ed| ed.insert_str(&lines));
        assert_eq!(taken() - start, 1);
        assert_eq!(ed.doc.body.len(), 501);
        assert_eq!(ed.undo_names(), vec!["Paste"]);

        let mut ed = Editor::new(doc(&[&"d".repeat(600)]));
        let start = taken();
        ed.one_step("Delete", |ed| {
            for _ in 0..500 {
                ed.delete_forward();
            }
        });
        assert_eq!(taken() - start, 1);
        assert_eq!(top_text(&ed), vec!["d".repeat(100)]);
        // A review transaction inside a group builds none either.
        let mut ed = Editor::new(doc(&["a"]));
        let start = taken();
        ed.one_step("Clean", |ed| {
            ed.insert_char('x');
            ed.remove_hidden_text();
            ed.remove_all_comment_markers();
        });
        assert_eq!(taken() - start, 1);
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["a"]);
    }

    /// #853: a command that panics leaves no grouping behind: the editor
    /// keeps the step it took and goes on taking steps.
    #[test]
    fn one_step_ends_its_grouping_on_a_panic_853() {
        let mut ed = Editor::new(doc(&["abc"]));
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ed.one_step("Broken", |ed| {
                ed.insert_char('x');
                panic!("the command fails");
            })
        }));
        assert!(run.is_err());
        assert_eq!(ed.grouping, None);
        assert_eq!(ed.undo.len(), 1, "the step before the command stays");
        ed.caret.offset = 0;
        ed.insert_char('y');
        assert_eq!(ed.undo.len(), 2, "a new step");
        assert!(ed.undo() && ed.undo());
        assert_eq!(top_text(&ed), vec!["abc"]);
    }

    /// #853: the weight counts everything a copy holds, raw XML too, so a
    /// paragraph carrying a large preserved payload trips the budget.
    #[test]
    fn the_history_budget_counts_raw_payloads_853() {
        let mut heavy = para("");
        if let Block::Paragraph(p) = &mut heavy {
            p.content
                .push(Inline::Raw(format!("<w:x>{}</w:x>", "r".repeat(64 << 10))));
        }
        let mut ed = Editor::new(Document { body: vec![heavy] });
        ed.history_budget = 10 << 20;
        for _ in 0..600 {
            ed.break_undo_group();
            ed.insert_char('x');
        }
        assert_eq!(ed.undo.len(), UNDO_FLOOR);
    }

    /// #853: each step's own list of block pointers counts: in a document of
    /// 10,000 paragraphs a one-character step still copies 10,000 pointers.
    #[test]
    fn the_history_budget_counts_each_steps_block_list_853() {
        let texts = vec![""; 10_000];
        let mut ed = Editor::new(doc(&texts));
        ed.caret = Caret::at(vec![5_000], 0);
        ed.insert_char('x');
        let list = 10_000 * std::mem::size_of::<Arc<Block>>();
        ed.history_budget = ed.undo[0].weight + 100 * list;
        for _ in 0..300 {
            ed.backspace();
            ed.insert_char('x');
        }
        assert_eq!(ed.undo.len(), UNDO_FLOOR);
    }

    /// #853: blocks equal but for their preserved element attributes are not
    /// shared, or undo could give one paragraph another's `w14:paraId`.
    #[test]
    fn undo_steps_never_share_look_alike_blocks_853() {
        let empty = |id: &str| {
            let mut props = ParProps::default();
            props.element_attrs = ElementAttrs(vec![("w14:paraId".into(), id.into())]);
            Block::Paragraph(Paragraph {
                props,
                content: Vec::new(),
            })
        };
        let attrs = |ed: &Editor, i: usize| match &ed.doc.body[i] {
            Block::Paragraph(p) => p.props.element_attrs.0.clone(),
            _ => panic!("a paragraph"),
        };
        let mut ed = Editor::new(Document {
            body: vec![para("x"), empty("A"), empty("B")],
        });
        ed.caret = Caret::at(vec![0], 1);
        ed.insert_char('z');
        // Backspace at the start of A merges it into `xz`; then B is
        // second, where A was, and an edit there takes a step.
        ed.caret = Caret::at(vec![1], 0);
        ed.backspace();
        assert_eq!(ed.doc.body.len(), 2);
        ed.caret = Caret::at(vec![1], 0);
        ed.insert_char('q');
        assert!(ed.undo());
        assert_eq!(attrs(&ed, 1), vec![("w14:paraId".into(), "B".into())]);
        assert!(ed.undo());
        assert_eq!(attrs(&ed, 1), vec![("w14:paraId".into(), "A".into())]);
        assert_eq!(attrs(&ed, 2), vec![("w14:paraId".into(), "B".into())]);
        assert!(ed.redo() && ed.redo());
        assert_eq!(attrs(&ed, 1), vec![("w14:paraId".into(), "B".into())]);
    }

    /// Backspace at the start of the first paragraph and Delete at the end
    /// of the last edit nothing, so they push no step and keep Redo.
    #[test]
    fn a_merge_with_nothing_to_merge_pushes_no_step_853() {
        let mut ed = Editor::new(doc(&["ab"]));
        ed.caret.offset = 2;
        ed.insert_char('x');
        assert!(ed.undo());
        ed.caret.offset = 2;
        ed.delete_forward();
        ed.caret.offset = 0;
        ed.backspace();
        assert!(ed.undo_names().is_empty());
        assert!(ed.can_redo());
        assert!(ed.redo());
        assert_eq!(top_text(&ed), vec!["abx"]);
    }

    /// #853: a host command made of many edits is one named step.
    #[test]
    fn one_step_makes_a_command_one_named_step_853() {
        let mut ed = Editor::new(doc(&["abcdef"]));
        ed.insert_str("t");
        let long: String = "y".repeat(300);
        ed.one_step("Paste", |ed| ed.insert_str(&long));
        ed.caret.offset = 0;
        ed.one_step("Delete", |ed| {
            for _ in 0..3 {
                ed.delete_forward();
            }
        });
        assert_eq!(ed.undo_names(), vec!["Delete", "Paste", "Typing \"t\""]);
        assert!(ed.undo());
        assert!(top_text(&ed)[0].starts_with("ty"));
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["tabcdef"]);
    }

    /// #853: an undo step shares the blocks it did not change with its
    /// neighbours, so a long history of small edits stays small.
    #[test]
    fn undo_steps_share_the_blocks_they_did_not_change_853() {
        let texts: Vec<String> = (0..1000).map(|i| format!("paragraph {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut ed = Editor::new(doc(&refs));
        for i in 0..3 {
            ed.caret = Caret::at(vec![500], 0);
            ed.insert_char('x');
            ed.caret = Caret::at(vec![500 + i], 0);
            ed.insert_newline();
        }
        assert_eq!(ed.undo.len(), 6);
        // The first paragraph and the last are unchanged in every step:
        // each step holds the same block.
        for at in [0, 999] {
            let first = &ed.undo[0].body;
            let index = if at == 0 { 0 } else { first.len() - 1 };
            let block = &first[index];
            assert_eq!(Arc::strong_count(block), 6, "block {at}: one copy");
            for step in &ed.undo {
                let i = if at == 0 { 0 } else { step.body.len() - 1 };
                assert!(Arc::ptr_eq(&step.body[i], block));
            }
        }
        // Undo and redo keep sharing and restore the document exactly.
        let full = ed.doc.clone();
        while ed.undo() {}
        assert_eq!(top_text(&ed), texts);
        assert!(Arc::ptr_eq(&ed.redo[0].body[0], &ed.redo[5].body[0]));
        while ed.redo() {}
        assert_eq!(ed.doc, full);
    }

    #[test]
    fn undo_redo_restores() {
        let mut ed = Editor::new(doc(&["a"]));
        ed.caret.offset = 1;
        ed.insert_str("bc");
        assert_eq!(top_text(&ed), vec!["abc"]);
        assert!(ed.undo());
        assert_eq!(top_text(&ed), vec!["a"]);
        assert!(ed.redo());
        assert_eq!(top_text(&ed), vec!["abc"]);
    }

    #[test]
    fn movement_crosses_paragraphs() {
        let mut ed = Editor::new(doc(&["ab", "cd"]));
        ed.caret = Caret::top(0, 2);
        ed.move_right();
        assert_eq!(ed.caret, Caret::top(1, 0));
        ed.move_left();
        assert_eq!(ed.caret, Caret::top(0, 2));
    }

    #[test]
    fn word_movement() {
        let mut ed = Editor::new(doc(&["the quick  brown"]));
        ed.caret.offset = 0;
        ed.move_word_right(); // -> start of "quick"
        assert_eq!(ed.caret.offset, 4);
        ed.move_word_right(); // -> start of "brown" (skips double space)
        assert_eq!(ed.caret.offset, 11);
        ed.move_word_right(); // -> end of line
        assert_eq!(ed.caret.offset, 16);
        ed.move_word_left(); // -> start of "brown"
        assert_eq!(ed.caret.offset, 11);
        ed.move_word_left(); // -> start of "quick"
        assert_eq!(ed.caret.offset, 4);
    }

    #[test]
    fn word_movement_crosses_paragraphs() {
        let mut ed = Editor::new(doc(&["ab", "cd"]));
        ed.caret = Caret::top(0, 2); // end of first
        ed.move_word_right(); // cross to next paragraph
        assert_eq!(ed.caret, Caret::top(1, 0));
        ed.move_word_left(); // back to end of first
        assert_eq!(ed.caret, Caret::top(0, 2));
    }

    // ---- selection + formatting ----

    #[test]
    fn select_and_bold_splits_runs() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(0, 3); // select "bc"
        assert!(ed.has_selection());
        ed.toggle_bold();
        if let Block::Paragraph(p) = &ed.doc.body[0] {
            let runs: Vec<(&str, bool)> = p
                .content
                .iter()
                .filter_map(|i| {
                    if let Inline::Run(r) = i {
                        Some((r.text.as_str(), r.props.bold))
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(runs, vec![("a", false), ("bc", true), ("d", false)]);
        } else {
            panic!();
        }
    }

    #[test]
    fn bold_toggles_off_when_all_bold() {
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let d = Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "abc".to_string(),
                    props: bold,
                })],
            })],
        };
        let mut ed = Editor::new(d);
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(0, 3);
        ed.toggle_bold();
        if let Block::Paragraph(p) = &ed.doc.body[0] {
            if let Inline::Run(r) = &p.content[0] {
                assert!(!r.props.bold);
            }
        }
    }

    #[test]
    fn typing_replaces_selection() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(0, 3);
        ed.insert_char('X');
        assert_eq!(top_text(&ed), vec!["aXd"]);
        assert!(!ed.has_selection());
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(0, 3);
        ed.backspace();
        assert_eq!(top_text(&ed), vec!["ad"]);
    }

    #[test]
    fn selection_spans_across_paragraphs() {
        let mut ed = Editor::new(doc(&["abc", "def"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(1, 2);
        assert_eq!(ed.selection_spans(), vec![(vec![0], 1, 3), (vec![1], 0, 2)]);
    }

    #[test]
    fn insert_equation_builds_omml_and_text() {
        let mut ed = Editor::new(doc(&[""]));
        ed.insert_equation("x^2", false);
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!()
        };
        let eq = p
            .content
            .iter()
            .find_map(|i| {
                if let Inline::Equation { raw, text, latex } = i {
                    Some((raw, text, latex))
                } else {
                    None
                }
            })
            .expect("equation inline");
        assert!(eq.0.contains("<m:oMath"), "no OMML: {}", eq.0);
        assert_eq!(eq.2.as_deref(), Some("x^2"));
        assert!(!eq.1.is_empty(), "no rendered text");
    }

    #[test]
    fn insert_tab_adds_a_tab_inline() {
        let mut ed = Editor::new(doc(&["ab"]));
        ed.caret = Caret::at(vec![0], 1); // between a and b
        ed.insert_tab();
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!()
        };
        assert!(
            p.content.iter().any(|i| matches!(i, Inline::Tab(_))),
            "no tab inline inserted: {:?}",
            p.content
        );
        // The tab counts as one caret position and the caret advanced past it.
        assert_eq!(ed.caret.offset, 2);
    }

    #[test]
    fn line_spacing_sets_and_reads_multiple() {
        let mut ed = Editor::new(doc(&["a", "b"]));
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(1, 1);
        ed.set_line_spacing(360, "auto"); // 1.5×
        for b in &ed.doc.body {
            if let Block::Paragraph(p) = b {
                assert_eq!(p.props.spacing.line, Some(360));
                assert_eq!(p.props.spacing.line_rule.as_deref(), Some("auto"));
            }
        }
        ed.caret = Caret::top(0, 0);
        assert_eq!(ed.caret_line_multiple(), Some(1.5));
        // An exact rule is not a plain multiple.
        ed.set_line_spacing(240, "exact");
        assert_eq!(ed.caret_line_multiple(), None);
    }

    #[test]
    fn multi_paragraph_bold_applies_to_all() {
        let mut ed = Editor::new(doc(&["abc", "def"]));
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(1, 3);
        ed.toggle_bold();
        for b in &ed.doc.body {
            if let Block::Paragraph(p) = b {
                for i in &p.content {
                    if let Inline::Run(r) = i {
                        assert!(r.props.bold, "run {:?} not bold", r.text);
                    }
                }
            }
        }
    }

    #[test]
    fn set_align_on_selected_paragraphs() {
        let mut ed = Editor::new(doc(&["a", "b"]));
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(1, 1);
        ed.set_align(Align::Center);
        for b in &ed.doc.body {
            if let Block::Paragraph(p) = b {
                assert_eq!(p.props.align, Align::Center);
            }
        }
    }

    #[test]
    fn find_all_case_insensitive_and_sensitive() {
        let ed = Editor::new(doc(&["the cat sat", "a Cat"]));
        let ms = ed.find_all("cat", false);
        assert_eq!(ms.len(), 2);
        assert_eq!(
            ms[0],
            Match {
                path: vec![0],
                start: 4,
                end: 7
            }
        );
        assert_eq!(
            ms[1],
            Match {
                path: vec![1],
                start: 2,
                end: 5
            }
        );
        assert_eq!(ed.find_all("cat", true).len(), 1);
    }

    #[test]
    fn select_match_creates_selection() {
        let mut ed = Editor::new(doc(&["hello world"]));
        let ms = ed.find_all("world", false);
        ed.select_match(&ms[0]);
        assert!(ed.has_selection());
        assert_eq!(ed.selection_spans(), vec![(vec![0], 6, 11)]);
    }

    #[test]
    fn replace_all_counts_and_rewrites() {
        let mut ed = Editor::new(doc(&["a foo b foo c", "foo"]));
        let n = ed.replace_all("foo", "BAR", false);
        assert_eq!(n, 3);
        assert_eq!(top_text(&ed), vec!["a BAR b BAR c", "BAR"]);
    }

    #[test]
    fn replace_current_uses_selection() {
        let mut ed = Editor::new(doc(&["one two"]));
        let ms = ed.find_all("two", false);
        ed.select_match(&ms[0]);
        ed.replace_current_with("three");
        assert_eq!(top_text(&ed), vec!["one three"]);
    }

    // ---- #279: a break keeps its run's formatting ----

    /// The text run that holds the typed `z`'s formatting.
    fn typed_props(ed: &Editor) -> RunProps {
        first_para(ed)
            .content
            .iter()
            .find_map(|i| match i {
                Inline::Run(r) if r.text.contains('z') => Some(r.props.clone()),
                _ => None,
            })
            .expect("no run holds the typed char")
    }

    /// The issue's examples, as loaded: typing right after a break takes the
    /// break's own run formatting (Word's rule), and `run_props_at` predicts
    /// exactly what the insert gives.
    #[test]
    fn typing_after_a_break_takes_its_runs_formatting_279() {
        let cases = [
            (
                "<w:r><w:rPr><w:b/></w:rPr><w:t>Name</w:t></w:r><w:r><w:br/></w:r>\
                 <w:bookmarkStart w:id=\"0\" w:name=\"_GoBack\"/>",
                false,
            ),
            (
                "<w:r><w:rPr><w:b/></w:rPr><w:br/></w:r><w:r><w:t>x</w:t></w:r>",
                true,
            ),
        ];
        for (xml, want_bold) in cases {
            let mut ed = Editor::new(xml_doc(xml));
            let after = etext(&ed).chars().position(|c| c == '\n').unwrap() + 1;
            ed.caret = Caret::at(vec![0], after);
            let predicted = ed.caret_props();
            assert_eq!(predicted.bold, want_bold, "{xml}");
            ed.insert_char('z');
            assert_eq!(typed_props(&ed), predicted, "{xml}");
        }
    }

    /// Formatting a selection that covers a break formats the break too, so
    /// text typed after it keeps the formatting (#279 r0).
    #[test]
    fn bold_over_a_break_reaches_the_break_279() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t>Name</w:t><w:br/></w:r><w:bookmarkStart w:id=\"0\" w:name=\"_GoBack\"/>",
        ));
        ed.anchor = Some(Caret::at(vec![0], 0));
        ed.caret = Caret::at(vec![0], 5);
        ed.toggle_bold();
        assert!(
            matches!(&first_para(&ed).content[1], Inline::Break(_, rp) if rp.bold),
            "{:?}",
            first_para(&ed).content
        );
        ed.clear_selection();
        ed.caret = Caret::at(vec![0], 5);
        assert!(ed.caret_props().bold);
        ed.insert_char('z');
        assert!(typed_props(&ed).bold);
        // The whole range now counts as bold, so the toggle turns it off,
        // break included.
        ed.anchor = Some(Caret::at(vec![0], 0));
        ed.caret = Caret::at(vec![0], 5);
        ed.toggle_bold();
        assert!(matches!(&first_para(&ed).content[1], Inline::Break(_, rp) if !rp.bold));
    }

    /// A break inserted in bold text is bold (like a tab), typing after it
    /// stays bold, and copying it keeps its formatting.
    #[test]
    fn an_inserted_break_takes_the_typing_formatting_279() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![run("Name", bold())],
            })],
        });
        ed.caret = Caret::at(vec![0], 4);
        ed.insert_break(BreakKind::Page);
        assert_eq!(
            first_para(&ed).content[1],
            Inline::Break(BreakKind::Page, bold())
        );
        assert!(ed.caret_props().bold);
        ed.anchor = Some(Caret::at(vec![0], 4));
        let clip = ed.copy().unwrap();
        assert_eq!(
            clip.paras,
            vec![vec![Inline::Break(BreakKind::Page, bold())]]
        );
        ed.clear_selection();
        ed.insert_char('z');
        assert_eq!(etext(&ed), "Name\nz");
        assert!(typed_props(&ed).bold);
    }

    /// A break inside a tracked insertion carries the insertion's display cue
    /// like any run; accepting the insertion clears it from the break too.
    #[test]
    fn accepting_an_insertion_clears_its_cue_from_a_break_279() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t>a</w:t></w:r><w:ins w:id=\"1\" w:author=\"A\"><w:r><w:br/></w:r></w:ins>",
        ));
        fn break_props(content: &[Inline]) -> Option<RunProps> {
            content.iter().find_map(|i| match i {
                Inline::Break(_, rp) => Some(rp.clone()),
                Inline::Revision { content, .. } => break_props(content),
                _ => None,
            })
        }
        let shown = break_props(&first_para(&ed).content).expect("the inserted break");
        assert!(shown.underline && shown.revision_cues.underline_added);
        ed.accept_all_revisions();
        let accepted = break_props(&first_para(&ed).content).expect("the accepted break");
        assert_eq!(accepted, RunProps::default());
    }

    // ---- #197: find / replace in editor offsets around zero-width inlines ----

    /// A one-paragraph document loaded from `<w:p>` inner XML, so the inlines
    /// are exactly what the loader produces for real files.
    fn xml_doc(p_inner: &str) -> Document {
        crate::load::parse_document_xml(
            &format!("<w:document><w:body><w:p>{p_inner}</w:p></w:body></w:document>"),
            &crate::load::Relationships::default(),
        )
    }

    fn first_para(ed: &Editor) -> &Paragraph {
        match &ed.doc.body[0] {
            Block::Paragraph(p) => p,
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    fn etext(ed: &Editor) -> String {
        editor_text(&first_para(ed).content)
    }

    /// Every inline that is not a plain run, in order: what Replace All must
    /// leave alone.
    fn non_runs(ed: &Editor) -> Vec<Inline> {
        first_para(ed)
            .content
            .iter()
            .filter(|i| !matches!(i, Inline::Run(_)))
            .cloned()
            .collect()
    }

    /// The issue's repro: a plain link and two tracked changes before ` end.`.
    const REPRO_197: &str = "<w:r><w:t xml:space=\"preserve\">Start </w:t></w:r>\
        <w:hyperlink w:anchor=\"top\"><w:r><w:t>link</w:t></w:r></w:hyperlink>\
        <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>added</w:t></w:r></w:ins>\
        <w:del w:id=\"2\" w:author=\"A\"><w:r><w:delText>removed</w:delText></w:r></w:del>\
        <w:r><w:t xml:space=\"preserve\"> end.</w:t></w:r>";

    const FIELD_197: &str = "<w:r><w:t xml:space=\"preserve\">Page </w:t></w:r>\
        <w:fldSimple w:instr=\" REF fig \"><w:r><w:t>Figure9</w:t></w:r></w:fldSimple>\
        <w:r><w:t xml:space=\"preserve\"> end.</w:t></w:r>";

    const FOOTNOTE_197: &str = "<w:r><w:t>Note</w:t></w:r>\
        <w:r><w:rPr><w:rStyle w:val=\"FootnoteReference\"/></w:rPr>\
        <w:footnoteReference w:id=\"7\"/></w:r>\
        <w:r><w:t xml:space=\"preserve\"> end.</w:t></w:r>";

    const COMPLEX_LINK_197: &str = "<w:r><w:t xml:space=\"preserve\">See </w:t></w:r>\
        <w:hyperlink w:anchor=\"top\"><w:r><w:t>here</w:t></w:r>\
        <w:ins w:id=\"3\" w:author=\"A\"><w:r><w:t>more</w:t></w:r></w:ins></w:hyperlink>\
        <w:r><w:t xml:space=\"preserve\"> end.</w:t></w:r>";

    /// Find `end` selects exactly `end`, and Replace All rewrites exactly it,
    /// leaving every non-run inline untouched and in order.
    fn assert_find_and_replace_end(p_inner: &str) {
        let mut ed = Editor::new(xml_doc(p_inner));
        let before = etext(&ed);
        let at = before.chars().count() - "end.".chars().count();
        let ms = ed.find_all("end", false);
        assert_eq!(
            ms,
            vec![Match {
                path: vec![0],
                start: at,
                end: at + 3
            }],
            "match offsets must be editor offsets in {before:?}"
        );
        ed.select_match(&ms[0]);
        assert_eq!(ed.selection_text(), "end");
        let kept = non_runs(&ed);
        assert_eq!(ed.replace_all("end", "finish", false), 1);
        assert_eq!(etext(&ed), before.replacen("end", "finish", 1));
        assert_eq!(non_runs(&ed), kept, "zero-width inlines must survive");
        match first_para(&ed).content.last() {
            Some(Inline::Run(r)) => assert_eq!(r.text, " finish."),
            other => panic!("replacement not in the trailing run: {other:?}"),
        }
    }

    #[test]
    fn replace_all_repro_197_edits_the_matched_text() {
        let ed = Editor::new(xml_doc(REPRO_197));
        let p = first_para(&ed);
        // The loader's shape: a plain link (runs filled, no content) and two
        // tracked changes, all before the match.
        assert!(
            matches!(&p.content[1], Inline::Hyperlink(h) if h.runs.len() == 1 && h.content.is_empty())
        );
        assert!(matches!(
            &p.content[2],
            Inline::Revision {
                kind: RevisionKind::Insert,
                ..
            }
        ));
        assert!(matches!(
            &p.content[3],
            Inline::Revision {
                kind: RevisionKind::Delete,
                ..
            }
        ));
        assert_eq!(etext(&ed), "Start link end.");
        assert_find_and_replace_end(REPRO_197);
        let mut ed = Editor::new(xml_doc(REPRO_197));
        ed.replace_all("end", "finish", false);
        assert_eq!(etext(&ed), "Start link finish.");
    }

    #[test]
    fn find_next_repro_197_selects_the_matched_text() {
        let mut ed = Editor::new(xml_doc(REPRO_197));
        let m = ed.find_next("end", false, false).expect("a match");
        assert_eq!((m.start, m.end), (11, 14));
        ed.select_match(&m);
        assert_eq!(ed.caret.offset, 14);
        assert_eq!(ed.selection_text(), "end");
    }

    #[test]
    fn find_and_replace_after_a_field_197() {
        let ed = Editor::new(xml_doc(FIELD_197));
        assert!(
            first_para(&ed)
                .content
                .iter()
                .any(|i| matches!(i, Inline::Field { text, .. } if text == "Figure9"))
        );
        assert_find_and_replace_end(FIELD_197);
    }

    #[test]
    fn find_and_replace_after_a_footnote_ref_197() {
        let ed = Editor::new(xml_doc(FOOTNOTE_197));
        assert!(
            first_para(&ed)
                .content
                .iter()
                .any(|i| matches!(i, Inline::FootnoteRef { id: 7, .. }))
        );
        assert_find_and_replace_end(FOOTNOTE_197);
    }

    #[test]
    fn find_and_replace_after_a_complex_hyperlink_197() {
        let ed = Editor::new(xml_doc(COMPLEX_LINK_197));
        // A link holding a revision keeps its text in `content`, not `runs`.
        assert!(first_para(&ed).content.iter().any(
            |i| matches!(i, Inline::Hyperlink(h) if h.runs.is_empty() && !h.content.is_empty())
        ));
        assert_find_and_replace_end(COMPLEX_LINK_197);
    }

    #[test]
    fn text_only_inside_zero_width_inlines_is_not_matched_197() {
        for (xml, query) in [
            (REPRO_197, "added"),
            (REPRO_197, "removed"),
            (FIELD_197, "Figure9"),
            (FOOTNOTE_197, "7"),
            // The tracked change inside a complex link stays zero-width; the
            // link's plain `here` is editable text since #212 (see below).
            (COMPLEX_LINK_197, "more"),
        ] {
            let mut ed = Editor::new(xml_doc(xml));
            let before = ed.doc.clone();
            assert!(ed.find_all(query, false).is_empty(), "{query:?} matched");
            assert_eq!(ed.find_next(query, false, false), None);
            assert_eq!(
                crate::agent::replace_all(&mut ed, query, "X", false),
                (0, 0),
                "{query:?} replaced"
            );
            assert_eq!(ed.doc, before);
            assert!(!ed.undo(), "no-op Replace All must not checkpoint");
        }
    }

    // ---- #212: plain text inside complex hyperlinks ----

    fn proof_err() -> Inline {
        Inline::Raw(r#"<w:proofErr w:type="spellStart"/>"#.into())
    }

    fn bookmark(start: bool) -> Inline {
        Inline::Raw(if start {
            r#"<w:bookmarkStart w:id="1" w:name="b"/>"#.into()
        } else {
            r#"<w:bookmarkEnd w:id="1"/>"#.into()
        })
    }

    fn ins(text: &str) -> Inline {
        Inline::Revision {
            kind: RevisionKind::Insert,
            metadata: RevisionMetadata::default(),
            raw: format!("<w:ins><w:r><w:t>{text}</w:t></w:r></w:ins>"),
            content: vec![run(text, RunProps::default())],
            content_changed: false,
        }
    }

    fn complex_link(content: Vec<Inline>) -> Inline {
        Inline::Hyperlink(Hyperlink {
            anchor: Some("top".into()),
            content,
            raw: Some("<w:hyperlink w:anchor=\"top\">…</w:hyperlink>".into()),
            ..Default::default()
        })
    }

    fn link_at(content: &[Inline], i: usize) -> &Hyperlink {
        match &content[i] {
            Inline::Hyperlink(h) => h,
            other => panic!("expected a hyperlink at {i}, got {other:?}"),
        }
    }

    /// `[ab, link(proofErr, Contoso, ins X, proofErr), cd]`
    fn contoso() -> Vec<Inline> {
        vec![
            run("ab", RunProps::default()),
            complex_link(vec![
                proof_err(),
                run("Contoso", RunProps::default()),
                ins("X"),
                proof_err(),
            ]),
            run("cd", RunProps::default()),
        ]
    }

    #[test]
    fn plain_text_inside_a_complex_hyperlink_is_found_and_replaced_212() {
        let mut ed = Editor::new(xml_doc(COMPLEX_LINK_197));
        assert_eq!(etext(&ed), "See here end.");
        let ms = ed.find_all("here", false);
        assert_eq!(
            ms,
            vec![Match {
                path: vec![0],
                start: 4,
                end: 8
            }]
        );
        ed.select_match(&ms[0]);
        assert_eq!(ed.selection_text(), "here");
        assert_eq!(
            crate::agent::replace_all(&mut ed, "here", "there", false),
            (1, 1)
        );
        assert_eq!(etext(&ed), "See there end.");
        let link = first_para(&ed)
            .content
            .iter()
            .find_map(|i| match i {
                Inline::Hyperlink(h) => Some(h),
                _ => None,
            })
            .expect("the link survives");
        assert!(link.content_changed);
        assert!(
            matches!(&link.content[..], [Inline::Run(r), Inline::Revision { .. }] if r.text == "there"),
            "{:?}",
            link.content
        );
    }

    // ---- #352: Enter, paste and Tab inside a hyperlink split it ----

    const SIMPLE_LINK_352: &str = "<w:r><w:t xml:space=\"preserve\">a </w:t></w:r>\
        <w:hyperlink w:anchor=\"top\"><w:r><w:t>Contoso</w:t></w:r></w:hyperlink>";

    /// A complex link: proofing marks at the split point (`Con|toso`) and at
    /// its end, and an attribute (`w:history`) only its raw XML keeps.
    const COMPLEX_LINK_352: &str = "<w:r><w:t xml:space=\"preserve\">a </w:t></w:r>\
        <w:hyperlink w:anchor=\"top\" w:history=\"1\"><w:r><w:t>Con</w:t></w:r>\
        <w:proofErr w:type=\"spellStart\"/><w:r><w:t>toso</w:t></w:r>\
        <w:proofErr w:type=\"spellEnd\"/></w:hyperlink>";

    fn link_text(h: &Hyperlink) -> String {
        editor_text(&[Inline::Hyperlink(h.clone())])
    }

    fn holds_raw(h: &Hyperlink, needle: &str) -> bool {
        h.content
            .iter()
            .any(|i| matches!(i, Inline::Raw(raw) if raw.contains(needle)))
    }

    /// Every paragraph's text after a save and reload.
    fn reloaded_texts(ed: &Editor) -> Vec<String> {
        let xml = crate::serialize::document_to_xml(&ed.doc);
        let back = crate::load::parse_document_xml(&xml, &crate::load::Relationships::default());
        back.body.iter().map(Block::plain_text).collect()
    }

    /// Checks shared by both halves of a split complex link: each keeps the
    /// original `w:hyperlink` attributes on save, the proofing mark at the
    /// split point goes left and the one at the end stays right, and the
    /// saved XML reloads to the same text.
    fn check_complex_halves(ed: &Editor, left: &Hyperlink, right: &Hyperlink) {
        let xml = crate::serialize::document_to_xml(&ed.doc);
        assert_eq!(
            xml.matches("<w:hyperlink w:anchor=\"top\" w:history=\"1\">")
                .count(),
            2,
            "{xml}"
        );
        assert!(left.content_changed && right.content_changed);
        assert!(holds_raw(left, "spellStart") && !holds_raw(left, "spellEnd"));
        assert!(holds_raw(right, "spellEnd") && !holds_raw(right, "spellStart"));
        let before: Vec<String> = ed.doc.body.iter().map(Block::plain_text).collect();
        assert_eq!(reloaded_texts(ed), before);
    }

    #[test]
    fn enter_inside_a_link_splits_it_352() {
        for xml in [SIMPLE_LINK_352, COMPLEX_LINK_352] {
            let mut ed = Editor::new(xml_doc(xml));
            ed.caret = Caret::at(vec![0], 5); // a Con|toso
            ed.insert_newline();
            let Block::Paragraph(p0) = &ed.doc.body[0] else {
                panic!()
            };
            let Block::Paragraph(p1) = &ed.doc.body[1] else {
                panic!()
            };
            let (left, right) = (link_at(&p0.content, 1), link_at(&p1.content, 0));
            assert_eq!(p0.content.len(), 2, "{xml}");
            assert_eq!(
                (link_text(left), link_text(right)),
                ("Con".into(), "toso".into())
            );
            assert_eq!(left.anchor.as_deref(), Some("top"));
            assert_eq!(right.anchor.as_deref(), Some("top"));
            assert_eq!(ed.caret, Caret::at(vec![1], 0));
            if xml == COMPLEX_LINK_352 {
                check_complex_halves(&ed, left, right);
            }
        }
    }

    #[test]
    fn pasting_inside_a_link_lands_between_its_halves_352() {
        for xml in [SIMPLE_LINK_352, COMPLEX_LINK_352] {
            let mut ed = Editor::new(xml_doc(xml));
            ed.caret = Caret::at(vec![0], 5);
            ed.paste(&Clip {
                paras: vec![vec![run("X", RunProps::default())]],
            });
            assert_eq!(etext(&ed), "a ConXtoso", "{xml}");
            assert_eq!(ed.caret, Caret::at(vec![0], 6));
            assert_eq!(
                etext(&ed).chars().nth(5),
                Some('X'),
                "the char before the caret"
            );
            let content = &first_para(&ed).content;
            assert_eq!(content.len(), 4, "{content:?}");
            assert!(matches!(&content[2], Inline::Run(r) if r.text == "X"));
            let (left, right) = (link_at(content, 1), link_at(content, 3));
            assert_eq!(
                (link_text(left), link_text(right)),
                ("Con".into(), "toso".into())
            );
            assert_eq!(left.anchor, right.anchor);
            if xml == COMPLEX_LINK_352 {
                check_complex_halves(&ed, left, right);
            }
        }
    }

    #[test]
    fn a_tab_inside_a_link_lands_between_its_halves_352() {
        for xml in [SIMPLE_LINK_352, COMPLEX_LINK_352] {
            let mut ed = Editor::new(xml_doc(xml));
            ed.caret = Caret::at(vec![0], 5);
            ed.insert_tab();
            assert_eq!(etext(&ed), "a Con\ttoso", "{xml}");
            assert_eq!(ed.caret, Caret::at(vec![0], 6));
            let content = &first_para(&ed).content;
            assert!(matches!(content[2], Inline::Tab(_)), "{content:?}");
            let (left, right) = (link_at(content, 1), link_at(content, 3));
            assert_eq!(
                (link_text(left), link_text(right)),
                ("Con".into(), "toso".into())
            );
            if xml == COMPLEX_LINK_352 {
                check_complex_halves(&ed, left, right);
            }
        }
    }

    /// A link with both plain `runs` and other `content` splits on the side
    /// the caret is in (`link_part`).
    #[test]
    fn a_link_with_runs_and_content_splits_on_the_carets_side_352() {
        let link = || {
            vec![Inline::Hyperlink(Hyperlink {
                anchor: Some("top".into()),
                runs: vec![Run {
                    text: "ab".into(),
                    props: RunProps::default(),
                }],
                content: vec![run("cd", RunProps::default())],
                ..Default::default()
            })]
        };
        for (at, left, right) in [(1, "a", "bcd"), (2, "ab", "cd"), (3, "abc", "d")] {
            let mut content = link();
            let rest = split_content(&mut content, at);
            assert_eq!(editor_text(&content), left, "split at {at}");
            assert_eq!(editor_text(&rest), right, "split at {at}");
            let (l, r) = (link_at(&content, 0), link_at(&rest, 0));
            assert_eq!(l.anchor, r.anchor);
            assert!(
                !l.content_changed && !r.content_changed,
                "no raw, nothing to rebuild"
            );
        }
    }

    #[test]
    fn a_complex_link_counts_its_plain_runs_212() {
        let content = contoso();
        assert_eq!(inline_len(&content[1]), 7);
        assert_eq!(editor_text(&content), "abContosocd");
    }

    #[test]
    fn typing_inside_a_complex_link_edits_its_run_212() {
        let mut content = contoso();
        content_insert(&mut content, 3, 'z'); // C|ontoso
        content_insert(&mut content, 10, 'y'); // Contoso| (the link claims its end)
        assert_eq!(editor_text(&content), "abCzontosoycd");
        let h = link_at(&content, 1);
        assert!(h.content_changed && h.runs.is_empty());
        assert!(matches!(&h.content[1], Inline::Run(r) if r.text == "Czontosoy"));
        assert!(matches!(h.content[2], Inline::Revision { .. }));
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn typing_at_the_start_of_a_leading_complex_link_stays_in_it_212() {
        let mut content = vec![complex_link(vec![
            proof_err(),
            run("Contoso", bold()),
            proof_err(),
        ])];
        let expected = run_props_at(&content, 0);
        content_insert(&mut content, 0, 'z');
        assert_eq!(editor_text(&content), "zContoso");
        let h = link_at(&content, 0);
        assert!(h.runs.is_empty(), "no stray default-props run in `runs`");
        match &h.content[0] {
            Inline::Run(r) => {
                assert_eq!(r.text, "z");
                assert_eq!(r.props, expected);
                assert!(r.props.bold);
            }
            other => panic!("typed char not in the link: {other:?}"),
        }
    }

    #[test]
    fn deleting_inside_a_complex_link_keeps_its_other_children_212() {
        let mut content = contoso();
        content_delete(&mut content, 2); // C
        content_delete(&mut content, 7); // o (last)
        assert_eq!(editor_text(&content), "abontoscd");
        let h = link_at(&content, 1);
        assert!(h.content_changed);
        assert_eq!(h.content.len(), 4);
        assert_eq!(h.content[0], proof_err());
        assert!(matches!(&h.content[1], Inline::Run(r) if r.text == "ontos"));
    }

    #[test]
    fn emptying_a_complex_link_of_markers_drops_it_in_place_212() {
        let mut content = vec![
            run("x", RunProps::default()),
            complex_link(vec![
                proof_err(),
                bookmark(true),
                run("ab", RunProps::default()),
                bookmark(false),
                proof_err(),
            ]),
            run("y", RunProps::default()),
        ];
        content_delete(&mut content, 1);
        content_delete(&mut content, 1);
        assert_eq!(
            content,
            vec![
                run("x", RunProps::default()),
                proof_err(),
                bookmark(true),
                bookmark(false),
                proof_err(),
                run("y", RunProps::default()),
            ]
        );
    }

    #[test]
    fn emptying_a_complex_link_that_still_shows_something_keeps_it_212() {
        let mut content = vec![
            run("x", RunProps::default()),
            complex_link(vec![
                run("ab", RunProps::default()),
                ins("more"),
                proof_err(),
            ]),
            run("y", RunProps::default()),
        ];
        content_delete(&mut content, 1);
        content_delete(&mut content, 1);
        assert_eq!(editor_text(&content), "xy");
        assert_eq!(content.len(), 3);
        let h = link_at(&content, 1);
        assert!(h.content_changed);
        assert_eq!(h.content, vec![ins("more"), proof_err()]);
    }

    #[test]
    fn run_props_at_predicts_typing_inside_a_complex_link_212() {
        fn typed(content: &[Inline]) -> Option<&Run> {
            content.iter().find_map(|i| match i {
                Inline::Run(r) if r.text.contains('z') => Some(r),
                Inline::Hyperlink(h) => h
                    .runs
                    .iter()
                    .find(|r| r.text.contains('z'))
                    .or_else(|| typed(&h.content)),
                _ => None,
            })
        }
        let cases: Vec<(&str, Vec<Inline>, usize, bool)> = vec![
            (
                "leading link, before its proofErr",
                vec![complex_link(vec![proof_err(), run("Co", bold())])],
                0,
                true,
            ),
            (
                "inside the link's run",
                vec![
                    run("a", RunProps::default()),
                    complex_link(vec![proof_err(), run("Co", bold()), ins("X")]),
                ],
                2,
                true,
            ),
            (
                "at the link's end, before its revision",
                vec![
                    run("a", RunProps::default()),
                    complex_link(vec![run("Co", bold()), ins("X")]),
                    run("b", RunProps::default()),
                ],
                3,
                true,
            ),
            (
                "after a tab inside the link",
                vec![complex_link(vec![
                    run("a", bold()),
                    Inline::Tab(RunProps::default()),
                    run("b", bold()),
                ])],
                2,
                false,
            ),
        ];
        for (name, mut content, offset, want_bold) in cases {
            let expected = run_props_at(&content, offset);
            content_insert(&mut content, offset, 'z');
            assert_eq!(
                editor_text(&content).chars().nth(offset),
                Some('z'),
                "{name}"
            );
            let inserted = typed(&content).unwrap_or_else(|| panic!("{name}: no run holds z"));
            assert_eq!(
                inserted.props, expected,
                "{name}: run_props_at mispredicted"
            );
            assert_eq!(inserted.props.bold, want_bold, "{name}");
        }
    }

    #[test]
    fn formatting_reaches_a_complex_links_runs_only_when_in_range_212() {
        let mut content = contoso();
        // "ab" only: next to the link, not over it.
        set_prop_range(&mut content, 0, 2, |p, v| p.bold = v, true);
        assert!(!link_at(&content, 1).content_changed);
        assert!(!range_all_have(&content, 0, 4, |p| p.bold));
        // "bCon": into the link.
        set_prop_range(&mut content, 1, 5, |p, v| p.bold = v, true);
        assert!(range_all_have(&content, 0, 5, |p| p.bold));
        assert!(!range_all_have(&content, 0, 6, |p| p.bold));
        let h = link_at(&content, 2); // after "a", "b"
        assert!(h.content_changed);
        assert!(matches!(&h.content[1], Inline::Run(r) if r.text == "Con" && r.props.bold));
        assert!(matches!(&h.content[2], Inline::Run(r) if r.text == "toso" && !r.props.bold));
        assert_eq!(h.content[3], ins("X"), "the revision is untouched");
        assert_eq!(editor_text(&content), "abContosocd");
    }

    #[test]
    fn copying_from_a_complex_link_takes_only_its_text_212() {
        let content = contoso();
        let out = extract_range(&content, 1, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], run("b", RunProps::default()));
        let h = link_at(&out, 1);
        assert_eq!(h.anchor.as_deref(), Some("top"));
        assert_eq!(
            h.runs,
            vec![Run {
                text: "Con".into(),
                props: RunProps::default()
            }]
        );
        assert!(h.content.is_empty() && h.raw.is_none() && !h.content_changed);
        // A range holding a tab inside the link keeps it in `content`.
        let tabbed = vec![complex_link(vec![
            proof_err(),
            run("a", RunProps::default()),
            Inline::Tab(RunProps::default()),
            run("b", RunProps::default()),
        ])];
        let out = extract_range(&tabbed, 0, 3);
        let h = link_at(&out, 0);
        assert!(h.runs.is_empty());
        assert_eq!(
            h.content,
            vec![
                run("a", RunProps::default()),
                Inline::Tab(RunProps::default()),
                run("b", RunProps::default()),
            ]
        );
    }

    #[test]
    fn editing_a_nested_link_marks_every_level_changed_212() {
        let inner = complex_link(vec![proof_err(), run("in", RunProps::default())]);
        let mut content = vec![complex_link(vec![bookmark(true), inner, bookmark(false)])];
        assert_eq!(editor_text(&content), "in");
        content_insert(&mut content, 1, 'z');
        assert_eq!(editor_text(&content), "izn");
        let outer = link_at(&content, 0);
        assert!(outer.content_changed);
        assert!(link_at(&outer.content, 1).content_changed);
    }

    fn bold() -> RunProps {
        RunProps {
            bold: true,
            ..Default::default()
        }
    }

    fn run(text: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: text.into(),
            props,
        })
    }

    fn field(text: &str) -> Inline {
        Inline::Field {
            raw: "<w:fldSimple/>".into(),
            text: text.into(),
        }
    }

    #[test]
    fn replace_range_keeps_a_whole_hyperlink_197() {
        let mut content = vec![
            run("Go ", RunProps::default()),
            Inline::Hyperlink(Hyperlink {
                anchor: Some("top".into()),
                runs: vec![Run {
                    text: "here".into(),
                    props: RunProps::default(),
                }],
                ..Default::default()
            }),
            run(".", RunProps::default()),
        ];
        replace_range_in_content(&mut content, 3, 7, "there");
        assert_eq!(editor_text(&content), "Go there.");
        match &content[1] {
            Inline::Hyperlink(h) => {
                assert_eq!(h.anchor.as_deref(), Some("top"));
                assert_eq!(h.runs.len(), 1);
                assert_eq!(h.runs[0].text, "there");
            }
            other => panic!("hyperlink lost: {other:?}"),
        }
    }

    #[test]
    fn replace_all_keeps_a_whole_hyperlink_197() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t xml:space=\"preserve\">Go </w:t></w:r>\
             <w:hyperlink w:anchor=\"top\"><w:r><w:t>here</w:t></w:r></w:hyperlink>\
             <w:r><w:t>.</w:t></w:r>",
        ));
        assert_eq!(ed.replace_all("here", "there", false), 1);
        assert_eq!(etext(&ed), "Go there.");
        assert!(matches!(
            &first_para(&ed).content[1],
            Inline::Hyperlink(h) if h.runs.iter().map(|r| r.text.as_str()).collect::<String>() == "there"
        ));
    }

    #[test]
    fn replace_all_keeps_a_whole_bold_run_bold_197() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![
                    run("a ", RunProps::default()),
                    run("bold", bold()),
                    run(" c", RunProps::default()),
                ],
            })],
        });
        assert_eq!(ed.replace_all("bold", "heavy", false), 1);
        assert_eq!(etext(&ed), "a heavy c");
        assert_eq!(first_para(&ed).content[1], run("heavy", bold()));
    }

    #[test]
    fn replace_range_after_a_field_stays_after_it_197() {
        // The field is offset 0 (#642), so "end" is [1, 4).
        let mut content = vec![field("F"), run("end.", RunProps::default())];
        replace_range_in_content(&mut content, 1, 4, "finish");
        assert_eq!(
            content,
            vec![field("F"), run("finish.", RunProps::default())]
        );
        // The whole trailing run matched: it must not reappear before the field.
        let mut content = vec![field("F"), run("end", bold())];
        replace_range_in_content(&mut content, 1, 4, "finish");
        assert_eq!(content, vec![field("F"), run("finish", bold())]);
    }

    #[test]
    fn replace_range_up_to_a_field_keeps_the_field_197() {
        let mut content = vec![
            run("ab", RunProps::default()),
            field("F"),
            run("cd", RunProps::default()),
        ];
        replace_range_in_content(&mut content, 0, 2, "X");
        assert_eq!(editor_text(&content), "X\u{FFFC}cd");
        assert!(content.contains(&field("F")));
        // A field is one offset (#642): a range over it replaces it, as in Word.
        let mut content = vec![
            run("ab", RunProps::default()),
            field("F"),
            run("cd", RunProps::default()),
        ];
        replace_range_in_content(&mut content, 0, 5, "X");
        assert_eq!(content, vec![run("X", RunProps::default())]);
    }

    #[test]
    fn replace_range_with_nothing_deletes_the_match_197() {
        let mut content = vec![run("a bold c", RunProps::default())];
        replace_range_in_content(&mut content, 2, 7, "");
        assert_eq!(content, vec![run("a c", RunProps::default())]);
    }

    #[test]
    fn selection_text_uses_editor_offsets_197() {
        let mut ed = Editor::new(xml_doc(REPRO_197));
        ed.select_match(&Match {
            path: vec![0],
            start: 6,
            end: 14,
        });
        assert_eq!(ed.selection_text(), "link end");
    }

    #[test]
    fn word_motion_uses_editor_offsets_197() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t xml:space=\"preserve\">Start </w:t></w:r>\
             <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>added</w:t></w:r></w:ins>\
             <w:r><w:t xml:space=\"preserve\"> end</w:t></w:r>",
        ));
        assert_eq!(etext(&ed), "Start  end");
        ed.caret = Caret::at(vec![0], 0);
        ed.move_word_right();
        assert_eq!(ed.caret.offset, 7, "Ctrl-Right stops at `end`");
        ed.move_word_right();
        assert_eq!(ed.caret.offset, 10, "then at the paragraph end");
        ed.caret = Caret::at(vec![0], 10);
        ed.move_word_left();
        assert_eq!(ed.caret.offset, 7, "Ctrl-Left stops at `end`");
    }

    #[test]
    fn replace_range_starting_at_a_tab_takes_the_tabs_place_197() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![
                    run("a", RunProps::default()),
                    Inline::Tab(RunProps::default()),
                    run("b c", bold()),
                ],
            })],
        });
        let ms = ed.find_all("\tb", false);
        assert_eq!((ms[0].start, ms[0].end), (1, 3));
        assert_eq!(ed.replace_all("\tb", "X", false), 1);
        assert_eq!(etext(&ed), "aX c");
        // No run to inherit from at a tab: the replacement takes the props typing
        // there would, the tab's own (#120; Word's replace takes the first matched
        // character's formatting), not the next run's.
        assert_eq!(
            first_para(&ed).content,
            vec![
                run("a", RunProps::default()),
                run("X", RunProps::default()),
                run(" c", bold())
            ]
        );
        // A tab with nothing after it: a new run in the tab's place.
        let mut content = vec![
            run("a", RunProps::default()),
            Inline::Tab(RunProps::default()),
            field("F"),
        ];
        replace_range_in_content(&mut content, 1, 2, "X");
        assert_eq!(editor_text(&content), "aX\u{FFFC}");
        assert_eq!(content.last(), Some(&field("F")));
    }

    fn text_box(text: &str) -> Inline {
        Inline::TextBox {
            raw: "<w:txbxContent/>".into(),
            blocks: vec![para(text)],
        }
    }

    /// The text of the paragraph inside the host's `n`th text box.
    fn box_text(ed: &Editor, n: usize) -> String {
        first_para(ed)
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::TextBox { blocks, .. } => Some(blocks),
                _ => None,
            })
            .nth(n)
            .map(|blocks| match &blocks[0] {
                Block::Paragraph(p) => p.plain_text(),
                other => panic!("expected a paragraph, got {other:?}"),
            })
            .expect("text box")
    }

    /// Emptying a host run shifts the host's inline indices. Text-box paths
    /// go through those indices, so they must be edited first, or they
    /// resolve to the wrong text box (or none).
    #[test]
    fn replace_all_edits_text_boxes_before_their_host_197() {
        // An empty replacement of a whole run, and a non-empty one whose
        // match crosses runs (its second run empties): both remove a run.
        for (runs, with, host, tb1) in [
            (
                vec![run("x ", RunProps::default()), run("ab", bold())],
                "",
                "x ",
                "",
            ),
            (
                vec![run("x a", RunProps::default()), run("b", bold())],
                "Q",
                "x Q",
                "Q",
            ),
        ] {
            let mut content = runs;
            content.extend([text_box("ab"), text_box("zz")]);
            let mut ed = Editor::new(Document {
                body: vec![Block::Paragraph(Paragraph {
                    props: ParProps::default(),
                    content,
                })],
            });
            assert_eq!(ed.find_all("ab", false).len(), 2);
            assert_eq!(ed.replace_all("ab", with, false), 2, "with {with:?}");
            assert_eq!(etext(&ed), host, "with {with:?}");
            assert_eq!(box_text(&ed, 0), tb1, "with {with:?}");
            assert_eq!(box_text(&ed, 1), "zz", "with {with:?}");
        }
    }

    /// Single Replace (select the match, replace it) must give the same
    /// document as Replace All, and undo in one step.
    fn assert_replace_current_matches_replace_all(doc: Document, query: &str, with: &str) {
        let mut all = Editor::new(doc.clone());
        assert_eq!(all.replace_all(query, with, false), 1);

        let mut one = Editor::new(doc);
        let original = one.doc.clone();
        let ms = one.find_all(query, false);
        assert_eq!(ms.len(), 1);
        one.select_match(&ms[0]);
        one.replace_current_with(with);
        assert_eq!(one.doc, all.doc, "Replace and Replace All disagree");
        assert!(!one.has_selection());
        assert_eq!(one.caret.offset, ms[0].start + with.chars().count());
        assert!(one.undo());
        assert_eq!(one.doc, original, "one undo restores the document");
        assert!(!one.undo(), "single Replace is one undo step");
    }

    #[test]
    fn replace_current_after_a_field_stays_after_it_197() {
        let doc = xml_doc(
            "<w:fldSimple w:instr=\" REF fig \"><w:r><w:t>F</w:t></w:r></w:fldSimple>\
             <w:r><w:t>end.</w:t></w:r>",
        );
        assert!(
            matches!(doc.body[0], Block::Paragraph(ref p) if matches!(p.content[0], Inline::Field { .. }))
        );
        assert_replace_current_matches_replace_all(doc.clone(), "end", "finish");
        let mut ed = Editor::new(doc);
        let ms = ed.find_all("end", false);
        ed.select_match(&ms[0]);
        ed.replace_current_with("finish");
        assert!(matches!(first_para(&ed).content[0], Inline::Field { .. }));
        assert_eq!(etext(&ed), "\u{FFFC}finish.");
    }

    #[test]
    fn replace_current_keeps_a_whole_hyperlink_197() {
        let doc = xml_doc(
            "<w:r><w:t xml:space=\"preserve\">Go </w:t></w:r>\
             <w:hyperlink w:anchor=\"top\"><w:r><w:t>here</w:t></w:r></w:hyperlink>\
             <w:r><w:t>.</w:t></w:r>",
        );
        assert_replace_current_matches_replace_all(doc.clone(), "here", "there");
        let mut ed = Editor::new(doc);
        let ms = ed.find_all("here", false);
        ed.select_match(&ms[0]);
        ed.replace_current_with("there");
        assert!(matches!(
            &first_para(&ed).content[1],
            Inline::Hyperlink(h) if h.runs.iter().map(|r| r.text.as_str()).collect::<String>() == "there"
        ));
    }

    #[test]
    fn replace_current_keeps_a_whole_bold_run_bold_197() {
        let doc = Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![
                    run("a ", RunProps::default()),
                    run("bold", bold()),
                    run(" c", RunProps::default()),
                ],
            })],
        };
        assert_replace_current_matches_replace_all(doc.clone(), "bold", "heavy");
        let mut ed = Editor::new(doc);
        let ms = ed.find_all("bold", false);
        ed.select_match(&ms[0]);
        ed.replace_current_with("heavy");
        assert_eq!(first_para(&ed).content[1], run("heavy", bold()));
    }

    #[test]
    fn replace_current_with_a_newline_still_splits_the_paragraph() {
        let mut ed = Editor::new(doc(&["one two"]));
        let ms = ed.find_all("two", false);
        ed.select_match(&ms[0]);
        ed.replace_current_with("x\ny");
        assert_eq!(top_text(&ed), vec!["one x", "y"]);
    }

    /// One of every `Inline` variant, each with visible text where it has any.
    /// `variant_name` has no wildcard arm, so a new variant fails to compile
    /// here until it is added to this list.
    fn every_inline() -> Vec<Inline> {
        fn variant_name(i: &Inline) -> &'static str {
            match i {
                Inline::Run(_) => "Run",
                Inline::Hyperlink(_) => "Hyperlink",
                Inline::Break(..) => "Break",
                Inline::Tab(_) => "Tab",
                Inline::SmartArt { .. } => "SmartArt",
                Inline::Chart { .. } => "Chart",
                Inline::Equation { .. } => "Equation",
                Inline::TextBox { .. } => "TextBox",
                Inline::Field { .. } => "Field",
                Inline::Revision { .. } => "Revision",
                Inline::UnsupportedRevision { .. } => "UnsupportedRevision",
                Inline::FootnoteRef { .. } => "FootnoteRef",
                Inline::Raw(_) => "Raw",
            }
        }
        let all = vec![
            run("run", RunProps::default()),
            Inline::Hyperlink(Hyperlink {
                runs: vec![Run {
                    text: "link".into(),
                    props: RunProps::default(),
                }],
                ..Default::default()
            }),
            Inline::Hyperlink(Hyperlink {
                content: vec![run("complex", RunProps::default())],
                ..Default::default()
            }),
            // #212: a link's plain runs, tabs and nested links count, and a
            // field is one unit (#642); its markers and revisions don't.
            Inline::Hyperlink(Hyperlink {
                content: vec![
                    Inline::Raw(r#"<w:proofErr w:type="spellStart"/>"#.into()),
                    Inline::Raw(r#"<w:bookmarkStart w:id="1" w:name="b"/>"#.into()),
                    run("mixed", RunProps::default()),
                    Inline::Tab(RunProps::default()),
                    field("F"),
                    Inline::Revision {
                        kind: RevisionKind::Insert,
                        metadata: RevisionMetadata::default(),
                        raw: String::new(),
                        content: vec![run("rev", RunProps::default())],
                        content_changed: false,
                    },
                    Inline::Hyperlink(Hyperlink {
                        content: vec![
                            Inline::Break(BreakKind::Line, RunProps::default()),
                            run("nested", RunProps::default()),
                        ],
                        ..Default::default()
                    }),
                ],
                ..Default::default()
            }),
            Inline::Break(BreakKind::Line, RunProps::default()),
            Inline::Tab(RunProps::default()),
            Inline::SmartArt {
                raw: String::new(),
                text: vec!["smart".into()],
            },
            Inline::Chart {
                raw: String::new(),
                chart: crate::chart::Chart {
                    kind: crate::chart::ChartKind::Bar,
                    title: Some("title".into()),
                    series: Vec::new(),
                },
            },
            Inline::Equation {
                raw: String::new(),
                text: "x+y".into(),
                latex: None,
            },
            Inline::TextBox {
                raw: String::new(),
                blocks: vec![para("box")],
            },
            field("result"),
            Inline::Revision {
                kind: RevisionKind::Insert,
                metadata: RevisionMetadata::default(),
                raw: String::new(),
                content: vec![run("added", RunProps::default())],
                content_changed: false,
            },
            Inline::UnsupportedRevision {
                kind: UnsupportedRevisionKind::MoveFrom,
                metadata: RevisionMetadata::default(),
                raw: String::new(),
            },
            Inline::FootnoteRef {
                id: 7,
                endnote: false,
                raw: String::new(),
            },
            Inline::Raw("<w:bookmarkStart/>".into()),
        ];
        let names: std::collections::BTreeSet<_> = all.iter().map(variant_name).collect();
        assert_eq!(names.len(), 13, "every Inline variant is listed");
        all
    }

    #[test]
    fn editor_text_matches_inline_len_for_every_variant_197() {
        for inline in every_inline() {
            let text = editor_text(std::slice::from_ref(&inline));
            assert_eq!(
                text.chars().count(),
                inline_len(&inline),
                "editor_text and inline_len disagree on {inline:?}"
            );
        }
        let all = every_inline();
        assert_eq!(
            editor_text(&all).chars().count(),
            all.iter().map(inline_len).sum::<usize>()
        );
        assert_eq!(editor_text(&all[3..4]), "mixed\t\u{FFFC}\nnested");
    }

    /// #211: the UI's visible walk and the editor agree on every offset. Its
    /// editable chars are exactly the editor's text minus field units, each
    /// at its own offset, and a field's result spans exactly its unit. Adding
    /// an `Inline` variant without teaching both walks fails here.
    #[test]
    fn the_visible_walk_keeps_the_editors_offsets_211() {
        let all = every_inline();
        let etext: Vec<char> = editor_text(&all).chars().collect();
        let shown: Vec<_> = visible::shown_segments(&all)
            .into_iter()
            .flatten()
            .collect();
        let editable: Vec<(usize, char)> = shown
            .iter()
            .filter(|s| s.editable)
            .inspect(|s| assert_eq!(s.end, s.start + 1, "{s:?}"))
            .map(|s| (s.start, s.ch))
            .collect();
        let expected: Vec<(usize, char)> = etext
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, c)| *c != FIELD_CHAR)
            .collect();
        assert_eq!(editable, expected);
        for s in shown.iter().filter(|s| !s.editable && s.end > s.start) {
            assert_eq!(s.end, s.start + 1, "{s:?}");
            assert_eq!(etext[s.start], FIELD_CHAR, "{s:?} is a field's unit");
        }
        // What is drawn but not editable: field results, a tracked change's
        // runs, SmartArt node text, an equation, a footnote mark. Not a
        // chart's title (charts are not searched) or the text box's text
        // (searched by its own path).
        let read_only: String = shown.iter().filter(|s| !s.editable).map(|s| s.ch).collect();
        assert_eq!(read_only, "Frevsmartx+yresultadded⁷");
    }

    #[test]
    fn copy_paste_within_paragraph() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(0, 3); // "bc"
        let clip = ed.copy().unwrap();
        ed.clear_selection();
        ed.caret = Caret::top(0, 4);
        ed.paste(&clip);
        assert_eq!(top_text(&ed), vec!["abcdbc"]);
        assert_eq!(ed.caret.offset, 6);
    }

    #[test]
    fn cut_removes_and_returns_clip() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(0, 3);
        let clip = ed.cut().unwrap();
        assert_eq!(top_text(&ed), vec!["ad"]);
        assert_eq!(clip.paras.len(), 1);
    }

    #[test]
    fn copy_preserves_run_style() {
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let d = Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "ab".to_string(),
                    props: bold,
                })],
            })],
        };
        let mut ed = Editor::new(d);
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(0, 2);
        let clip = ed.copy().unwrap();
        if let Inline::Run(r) = &clip.paras[0][0] {
            assert!(r.props.bold);
        } else {
            panic!();
        }
    }

    #[test]
    fn multi_paragraph_delete_merges_ends() {
        let mut ed = Editor::new(doc(&["abc", "def", "ghi"]));
        ed.anchor = Some(Caret::top(0, 1));
        ed.caret = Caret::top(2, 2);
        ed.delete_selection();
        assert_eq!(top_text(&ed), vec!["ai"]);
        assert_eq!(ed.caret, Caret::top(0, 1));
    }

    #[test]
    fn paste_multi_paragraph_clip_splits() {
        let mut ed = Editor::new(doc(&["XY"]));
        let r = |s: &str| {
            Inline::Run(Run {
                text: s.to_string(),
                props: RunProps::default(),
            })
        };
        let clip = Clip {
            paras: vec![vec![r("A")], vec![r("B")]],
        };
        ed.caret = Caret::top(0, 1); // between X and Y
        ed.paste(&clip);
        assert_eq!(top_text(&ed), vec!["XA", "BY"]);
        assert_eq!(ed.caret, Caret::top(1, 1));
    }

    #[test]
    fn clip_text_roundtrip() {
        let c = Clip::from_text("hello\tworld\nsecond");
        assert_eq!(c.paras.len(), 2);
        assert_eq!(c.to_text(), "hello\tworld\nsecond");
        assert!(matches!(c.paras[0][1], Inline::Tab(_)));
    }

    #[test]
    fn paste_plain_text_is_multiline() {
        let mut ed = Editor::new(doc(&["X"]));
        ed.caret = Caret::top(0, 1);
        ed.paste(&Clip::from_text("a\nb"));
        assert_eq!(top_text(&ed), vec!["Xa", "b"]);
    }

    #[test]
    fn select_all_spans_document() {
        let mut ed = Editor::new(doc(&["ab", "cd"]));
        ed.select_all();
        assert_eq!(ed.selection_spans(), vec![(vec![0], 0, 2), (vec![1], 0, 2)]);
    }

    #[test]
    fn extend_then_clear_selection() {
        let mut ed = Editor::new(doc(&["abcd"]));
        ed.extend_selection(true);
        ed.move_right();
        ed.move_right();
        assert!(ed.has_selection());
        ed.extend_selection(false);
        assert!(!ed.has_selection());
    }

    // ---- table navigation/editing ----

    fn table_doc() -> Document {
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(s)],
            ..Default::default()
        };
        Document {
            body: vec![
                para("before"),
                Block::Table(Table {
                    grid: vec![100, 100],
                    rows: vec![
                        Row {
                            cells: vec![cell("A"), cell("B")],
                            ..Default::default()
                        },
                        Row {
                            cells: vec![cell("C"), cell("D")],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }),
                para("after"),
            ],
        }
    }

    fn controlled_table_doc() -> Document {
        let mut document = table_doc();
        let Block::Table(table) = &mut document.body[1] else {
            unreachable!();
        };
        table.row_boundaries = vec![
            TableRowBoundary::sdt_open(
                0,
                "<w:sdt><w:sdtPr><w:alias w:val=\"rows\"/></w:sdtPr><w:sdtContent>",
            ),
            TableRowBoundary::sdt_close(2, "</w:sdtContent></w:sdt>"),
        ];
        document
    }

    fn controlled_table_boundaries(editor: &Editor) -> Vec<TableRowBoundary> {
        let Block::Table(table) = &editor.doc.body[1] else {
            unreachable!();
        };
        table.row_boundaries.clone()
    }

    #[test]
    fn caret_visits_cells_in_reading_order() {
        let paths = all_paragraph_paths(&table_doc().body);
        // before, (r0c0)A, (r0c1)B, (r1c0)C, (r1c1)D, after
        assert_eq!(
            paths,
            vec![
                vec![0],
                vec![1, 0, 0, 0],
                vec![1, 0, 1, 0],
                vec![1, 1, 0, 0],
                vec![1, 1, 1, 0],
                vec![2],
            ]
        );
    }

    #[test]
    fn move_right_enters_and_exits_table() {
        let mut ed = Editor::new(table_doc());
        // start at end of "before"
        ed.caret = Caret::top(0, "before".len());
        ed.move_right(); // into cell A (start)
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 0, 0], 0));
        ed.move_end(); // end of "A"
        ed.move_right(); // into cell B
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 1, 0], 0));
    }

    #[test]
    fn edit_inside_a_cell() {
        let mut ed = Editor::new(table_doc());
        ed.caret = Caret::at(vec![1, 0, 0, 0], 1); // after "A"
        ed.insert_str("!!");
        // The cell paragraph now reads "A!!"
        let p = resolve_para(&ed.doc.body, &[1, 0, 0, 0]).unwrap();
        assert_eq!(p.plain_text(), "A!!");
        // undo restores
        assert!(ed.undo());
        let p = resolve_para(&ed.doc.body, &[1, 0, 0, 0]).unwrap();
        assert_eq!(p.plain_text(), "A");
    }

    #[test]
    fn controlled_row_edit_undo_and_redo_preserve_boundaries() {
        let mut ed = Editor::new(controlled_table_doc());
        let boundaries = controlled_table_boundaries(&ed);
        ed.caret = Caret::at(vec![1, 0, 0, 0], 1);

        ed.insert_str("!");
        assert_eq!(controlled_table_boundaries(&ed), boundaries);
        assert_eq!(
            resolve_para(&ed.doc.body, &[1, 0, 0, 0])
                .unwrap()
                .plain_text(),
            "A!"
        );

        assert!(ed.undo());
        assert_eq!(controlled_table_boundaries(&ed), boundaries);
        assert_eq!(
            resolve_para(&ed.doc.body, &[1, 0, 0, 0])
                .unwrap()
                .plain_text(),
            "A"
        );

        assert!(ed.redo());
        assert_eq!(controlled_table_boundaries(&ed), boundaries);
        assert_eq!(
            resolve_para(&ed.doc.body, &[1, 0, 0, 0])
                .unwrap()
                .plain_text(),
            "A!"
        );
    }

    #[test]
    fn controlled_row_copy_paste_and_paragraph_split_merge_keep_boundaries() {
        let mut ed = Editor::new(controlled_table_doc());
        let boundaries = controlled_table_boundaries(&ed);

        ed.anchor = Some(Caret::at(vec![1, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![1, 0, 0, 0], 1);
        let clip = ed.copy().unwrap();
        ed.clear_selection();
        ed.caret = Caret::at(vec![1, 1, 0, 0], 1);
        ed.paste(&clip);
        assert_eq!(controlled_table_boundaries(&ed), boundaries);
        assert_eq!(
            resolve_para(&ed.doc.body, &[1, 1, 0, 0])
                .unwrap()
                .plain_text(),
            "CA"
        );

        ed.caret = Caret::at(vec![1, 0, 1, 0], 1);
        ed.insert_newline();
        assert_eq!(controlled_table_boundaries(&ed), boundaries);
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 1, 1], 0));

        ed.backspace();
        assert_eq!(controlled_table_boundaries(&ed), boundaries);
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 1, 0], 1));
        assert_eq!(
            resolve_para(&ed.doc.body, &[1, 0, 1, 0])
                .unwrap()
                .plain_text(),
            "B"
        );
    }

    #[test]
    fn newline_inside_cell_adds_sibling_paragraph() {
        let mut ed = Editor::new(table_doc());
        ed.caret = Caret::at(vec![1, 0, 0, 0], 1);
        ed.insert_newline();
        // cell now has two paragraphs; caret on the second
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 0, 1], 0));
        if let Block::Table(t) = &ed.doc.body[1] {
            assert_eq!(t.rows[0].cells[0].blocks.len(), 2);
        } else {
            panic!();
        }
    }

    #[test]
    fn partial_edit_rekeys_cloned_property_revision_targets() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:b/><w:rPrChange w:id="90"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr><w:t>abcd</w:t></w:r></w:p></w:body></w:document>"#;
        let document = crate::load::parse_document_xml(xml, &Default::default());
        let mut editor = Editor::new(document);
        editor.anchor = Some(Caret::top(0, 1));
        editor.caret = Caret::top(0, 3);
        editor.toggle_bold();

        let revisions = editor.doc.revisions();
        let unique = revisions
            .iter()
            .map(|revision| revision.target)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), revisions.len());
        assert!(revisions.iter().all(|revision| revision.target.0 != 0));

        let outcomes = editor.reject_all_revisions();
        assert_eq!(outcomes.len(), revisions.len());
        assert!(outcomes.iter().all(RevisionOutcome::is_applied));
        assert!(editor.doc.revisions().is_empty());
    }

    #[test]
    fn backspace_does_not_escape_cell() {
        let mut ed = Editor::new(table_doc());
        ed.caret = Caret::at(vec![1, 0, 1, 0], 0); // start of cell B's only paragraph
        ed.backspace(); // nothing to merge with inside the cell
        let p = resolve_para(&ed.doc.body, &[1, 0, 1, 0]).unwrap();
        assert_eq!(p.plain_text(), "B");
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 1, 0], 0));
    }

    // ---- #642: a field is one editing unit ----

    /// A complex PAGE field whose cached result is `1`.
    const PAGE_642: &str = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText xml:space=\"preserve\"> PAGE \\* MERGEFORMAT </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:r><w:t>1</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
    const SIMPLE_PAGE_642: &str =
        "<w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple>";

    fn body_then(field_xml: &str) -> Editor {
        Editor::new(xml_doc(&format!("<w:r><w:t>Body</w:t></w:r>{field_xml}")))
    }

    fn inline_kinds(ed: &Editor) -> Vec<String> {
        first_para(ed)
            .content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => format!("Run {}", r.text),
                Inline::Field { text, .. } => format!("Field {text}"),
                _ => "Other".to_string(),
            })
            .collect()
    }

    fn saved(ed: &Editor) -> String {
        crate::serialize::document_to_xml(&ed.doc)
    }

    #[test]
    fn a_field_is_one_editor_offset_642() {
        for xml in [PAGE_642, SIMPLE_PAGE_642] {
            let ed = body_then(xml);
            assert_eq!(inline_kinds(&ed), ["Run Body", "Field 1"]);
            assert_eq!(etext(&ed), "Body\u{FFFC}");
            assert_eq!(para_text_len(first_para(&ed)), 5);
            let flat = flat::FlatDocument::new(&ed.doc);
            assert_eq!(flat.main().text, "Body\u{FFFC}\n");
        }
    }

    #[test]
    fn backspace_after_a_field_selects_it_then_deletes_it_642() {
        for xml in [PAGE_642, SIMPLE_PAGE_642] {
            let mut ed = body_then(xml);
            let before = ed.doc.clone();
            ed.set_caret(Caret::at(vec![0], 5));
            ed.backspace();
            let (lo, hi) = ed.selection_range().expect("the field is selected");
            assert_eq!((lo.offset, hi.offset), (4, 5));
            assert_eq!(ed.doc, before, "selecting changes nothing");
            assert!(!ed.undo(), "selecting is not an undo step");
            ed.backspace();
            assert_eq!(inline_kinds(&ed), ["Run Body"]);
            assert_eq!(ed.caret.offset, 4);
            let xml_out = saved(&ed);
            for gone in ["fldChar", "instrText", "fldSimple", ">1<"] {
                assert!(!xml_out.contains(gone), "{gone} left in {xml_out}");
            }
            ed.backspace();
            assert_eq!(etext(&ed), "Bod", "the next Backspace deletes a character");
            assert!(ed.undo() && ed.undo());
            assert_eq!(ed.doc, before, "undo brings the whole field back");
        }
    }

    #[test]
    fn delete_before_a_field_selects_it_then_deletes_it_642() {
        for xml in [PAGE_642, SIMPLE_PAGE_642] {
            let mut ed = body_then(xml);
            ed.set_caret(Caret::at(vec![0], 4));
            ed.delete_forward();
            let (lo, hi) = ed.selection_range().expect("the field is selected");
            assert_eq!((lo.offset, hi.offset), (4, 5));
            ed.delete_forward();
            assert_eq!(inline_kinds(&ed), ["Run Body"]);
            assert!(!saved(&ed).contains("fldChar"));
        }
    }

    #[test]
    fn backspace_selects_a_field_inside_a_link_642() {
        let mut ed = Editor::new(xml_doc(&format!(
            "<w:hyperlink w:anchor=\"_Toc1\"><w:r><w:t>Intro</w:t></w:r>{PAGE_642}</w:hyperlink>"
        )));
        assert_eq!(etext(&ed), "Intro\u{FFFC}");
        ed.set_caret(Caret::at(vec![0], 6));
        ed.backspace();
        let (lo, hi) = ed.selection_range().expect("the field is selected");
        assert_eq!((lo.offset, hi.offset), (5, 6));
        ed.backspace();
        assert_eq!(etext(&ed), "Intro");
    }

    #[test]
    fn a_symbol_is_one_character_deleted_at_once_642() {
        let mut ed = body_then("<w:r><w:sym w:font=\"Symbol\" w:char=\"F0B7\"/></w:r>");
        assert!(matches!(first_para(&ed).content[1], Inline::Field { .. }));
        assert_eq!(etext(&ed), "Body\u{FFFC}");
        ed.set_caret(Caret::at(vec![0], 5));
        ed.backspace();
        assert!(!ed.has_selection());
        assert_eq!(inline_kinds(&ed), ["Run Body"]);
    }

    #[test]
    fn a_field_inserted_at_another_fields_left_edge_goes_before_it_642() {
        let mut ed = body_then(PAGE_642);
        ed.set_caret(Caret::at(vec![0], 4));
        let new_field = Inline::Field {
            raw: SIMPLE_PAGE_642.into(),
            text: "1".into(),
        };
        ed.paste(&Clip {
            paras: vec![vec![new_field]],
        });
        assert_eq!(inline_kinds(&ed), ["Run Body", "Field 1", "Field 1"]);
        assert_eq!(ed.caret.offset, 5);
        let xml_out = saved(&ed);
        let simple = xml_out.find("<w:fldSimple").unwrap();
        assert!(simple < xml_out.find("fldCharType=\"begin\"").unwrap());
        assert!(xml_out.contains(PAGE_642), "the old field is untouched");
    }

    #[test]
    fn typing_at_a_fields_edges_lands_outside_it_642() {
        let mut ed = body_then(PAGE_642);
        ed.set_caret(Caret::at(vec![0], 5));
        ed.insert_char('x');
        assert_eq!(inline_kinds(&ed), ["Run Body", "Field 1", "Run x"]);
        let mut ed = body_then(PAGE_642);
        ed.set_caret(Caret::at(vec![0], 4));
        ed.insert_char('x');
        assert_eq!(inline_kinds(&ed), ["Run Bodyx", "Field 1"]);
        // A field first in the paragraph: typing before it stays before it.
        let mut ed = Editor::new(xml_doc(PAGE_642));
        ed.set_caret(Caret::at(vec![0], 0));
        ed.insert_char('x');
        assert_eq!(inline_kinds(&ed), ["Run x", "Field 1"]);
    }

    #[test]
    fn a_field_with_no_result_stays_zero_width_642() {
        let xe = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
            <w:r><w:instrText> XE \"term\" </w:instrText></w:r>\
            <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        let empty = "<w:fldSimple w:instr=\" AUTHOR \"/>";
        for xml in [xe, empty] {
            let mut ed = Editor::new(xml_doc(&format!("<w:r><w:t>term</w:t></w:r>{xml}")));
            assert_eq!(etext(&ed), "term");
            ed.set_caret(Caret::at(vec![0], 4));
            ed.insert_char('s');
            ed.backspace();
            ed.backspace();
            assert_eq!(etext(&ed), "ter");
            assert!(saved(&ed).contains(xml), "{xml} survives");
        }
    }

    #[test]
    fn typing_over_a_selection_holding_a_field_replaces_it_642() {
        let mut ed = Editor::new(xml_doc(&format!(
            "<w:r><w:t>Body</w:t></w:r>{PAGE_642}<w:r><w:t>xy</w:t></w:r>"
        )));
        ed.set_caret(Caret::at(vec![0], 4));
        ed.extend_selection(true);
        ed.set_caret(Caret::at(vec![0], 6));
        ed.insert_char('Z');
        assert_eq!(etext(&ed), "BodyZy");
        assert!(!saved(&ed).contains("fldChar"));
        // Replace (find/replace's path) of a range starting at the field.
        let mut content = first_para(&body_then(PAGE_642)).content.clone();
        content.push(run("xy", RunProps::default()));
        replace_range_in_content(&mut content, 4, 6, "Z");
        assert_eq!(editor_text(&content), "BodyZy");
    }

    #[test]
    fn the_field_character_is_never_inserted_as_text_642() {
        let mut ed = body_then(PAGE_642);
        ed.set_caret(Caret::at(vec![0], 5));
        ed.insert_char(FIELD_CHAR);
        assert_eq!(inline_kinds(&ed), ["Run Body", "Field 1"]);
        ed.paste(&Clip::from_text("a\u{FFFC}b"));
        assert_eq!(etext(&ed), "Body\u{FFFC}ab");
        // An automation rewrite that sends the paragraph's text back: the
        // stand-in adds nothing, and the undo contract is unchanged.
        let mut ed = body_then(PAGE_642);
        let (n, steps) = crate::agent::replace_range(&mut ed, 0, 0, "Body text\u{FFFC}").unwrap();
        assert_eq!((n, steps), (1, 2));
        assert_eq!(etext(&ed), "Body text");
    }

    #[test]
    fn the_field_character_never_shifts_the_caret_or_a_replace_642() {
        // Typed in the middle of a paragraph: nothing, not even a caret step.
        let mut ed = Editor::new(xml_doc("<w:r><w:t>XY</w:t></w:r>"));
        ed.set_caret(Caret::at(vec![0], 1));
        ed.insert_str("a\u{FFFC}b");
        assert_eq!(etext(&ed), "XabY");
        assert_eq!(ed.caret.offset, 3);
        // A replacement holding it: the rest is counted without it.
        let mut content = vec![run("abcdef", RunProps::default())];
        replace_range_in_content(&mut content, 0, 2, "X\u{FFFC}");
        assert_eq!(editor_text(&content), "Xcdef");
        let mut ed = Editor::new(xml_doc("<w:r><w:t>abcdef</w:t></w:r>"));
        ed.select_match(&Match {
            path: vec![0],
            start: 1,
            end: 3,
        });
        ed.replace_current_with("Z\u{FFFC}");
        assert_eq!(etext(&ed), "aZdef");
        assert_eq!(ed.caret.offset, 2);
        assert_eq!(ed.replace_all("de", "\u{FFFC}Q", false), 1);
        assert_eq!(etext(&ed), "aZQf");
    }

    #[test]
    fn a_selected_field_copies_as_its_result_642() {
        let mut ed = body_then(PAGE_642);
        ed.set_caret(Caret::at(vec![0], 5));
        ed.backspace();
        assert_eq!(ed.selection_text(), "1");
        let clip = ed.copy().expect("a clip");
        ed.clear_selection();
        ed.set_caret(Caret::at(vec![0], 5));
        ed.paste(&clip);
        assert_eq!(inline_kinds(&ed), ["Run Body", "Field 1", "Field 1"]);
        assert_eq!(saved(&ed).matches(PAGE_642).count(), 2);
        // Selecting across text and fields gives the visible text.
        ed.set_caret(Caret::at(vec![0], 2));
        ed.extend_selection(true);
        ed.set_caret(Caret::at(vec![0], 6));
        assert_eq!(ed.selection_text(), "dy11");
    }

    #[test]
    fn find_never_matches_a_fields_result_642() {
        let ed = body_then(PAGE_642);
        assert!(ed.find_all("Body1", false).is_empty());
        assert_eq!(ed.find_all("Body", false).len(), 1);
    }

    #[test]
    fn bold_over_a_field_skips_it_and_keeps_offsets_642() {
        let mut ed = Editor::new(xml_doc(&format!(
            "<w:r><w:t>ab</w:t></w:r>{PAGE_642}<w:r><w:t>cd</w:t></w:r>"
        )));
        ed.set_caret(Caret::at(vec![0], 0));
        ed.extend_selection(true);
        ed.set_caret(Caret::at(vec![0], 4));
        ed.toggle_bold();
        let p = first_para(&ed);
        assert!(matches!(&p.content[0], Inline::Run(r) if r.text == "ab" && r.props.bold));
        assert!(matches!(&p.content[1], Inline::Field { .. }));
        assert!(matches!(&p.content[2], Inline::Run(r) if r.text == "c" && r.props.bold));
        assert!(matches!(&p.content[3], Inline::Run(r) if r.text == "d" && !r.props.bold));
    }
}
