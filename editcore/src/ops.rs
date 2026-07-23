//! Edit operations on an [`Editor`]: text insertion/deletion, paragraph
//! splitting, inline style toggles, links, lists, indentation, cursor
//! movement, and undo/redo.
//!
//! Every op here is *total*: none of them may panic regardless of the
//! buffer's contents or the current selection's validity. Positions and
//! selections are clamped to the document before use, so a stale or
//! out-of-range `Pos` (e.g. left over after an edit shrank the document)
//! degrades gracefully instead of indexing out of bounds.

use crate::cursor::{Pos, Selection};
use crate::history::History;
use crate::model::{Block, RichText, Run};

/// A headless rich-text editor: the document, the current selection, and
/// undo/redo history.
#[derive(Debug, Clone)]
pub struct Editor {
    pub text: RichText,
    pub sel: Selection,
    history: History,
}

impl Editor {
    /// A new editor over an empty document.
    pub fn new() -> Editor {
        Editor {
            text: RichText::new(),
            sel: Selection::default(),
            history: History::new(),
        }
    }
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new()
    }
}

impl From<RichText> for Editor {
    fn from(text: RichText) -> Editor {
        Editor {
            text,
            sel: Selection::default(),
            history: History::new(),
        }
    }
}

// ---------------------------------------------------------------------
// Position helpers: total (clamping, never panicking) navigation over a
// RichText's blocks/runs/offsets.
// ---------------------------------------------------------------------

fn block_runs(text: &RichText, block: usize) -> &[Run] {
    match &text.blocks[block] {
        Block::Paragraph(runs) => runs,
        Block::ListItem { runs, .. } => runs,
    }
}

fn block_runs_mut(text: &mut RichText, block: usize) -> &mut Vec<Run> {
    match &mut text.blocks[block] {
        Block::Paragraph(runs) => runs,
        Block::ListItem { runs, .. } => runs,
    }
}

/// Rounds `idx` down to the nearest UTF-8 char boundary in `s`. `idx` must
/// already be `<= s.len()`.
fn clamp_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Clamps `pos` to a valid position within `text`: block index in range,
/// run index in range (or 0 with offset 0 if the block has no runs), and
/// offset a valid byte index within that run's text.
fn clamp_in(text: &RichText, pos: Pos) -> Pos {
    if text.blocks.is_empty() {
        return Pos::default();
    }
    let block = pos.block.min(text.blocks.len() - 1);
    let runs = block_runs(text, block);
    if runs.is_empty() {
        return Pos {
            block,
            run: 0,
            offset: 0,
        };
    }
    let run = pos.run.min(runs.len() - 1);
    let len = runs[run].text.len();
    let offset = clamp_char_boundary(&runs[run].text, pos.offset.min(len));
    Pos { block, run, offset }
}

/// The cumulative byte offset of `pos` within its block, counting through
/// preceding runs. Used to carry a selection across a run-splitting
/// mutation (e.g. a style toggle) that changes run indices but not the
/// block's text.
fn block_offset(text: &RichText, pos: Pos) -> usize {
    let runs = block_runs(text, pos.block);
    if runs.is_empty() {
        return 0;
    }
    let run = pos.run.min(runs.len() - 1);
    let before: usize = runs[..run].iter().map(|r| r.text.len()).sum();
    before + pos.offset.min(runs[run].text.len())
}

/// The inverse of [`block_offset`]: the `(run, offset)` position in `block`
/// that is `offset` bytes into its concatenated run text.
fn pos_from_block_offset(text: &RichText, block: usize, mut offset: usize) -> Pos {
    let runs = block_runs(text, block);
    if runs.is_empty() {
        return Pos {
            block,
            run: 0,
            offset: 0,
        };
    }
    for (i, r) in runs.iter().enumerate() {
        if offset <= r.text.len() {
            return Pos {
                block,
                run: i,
                offset,
            };
        }
        offset -= r.text.len();
    }
    let last = runs.len() - 1;
    Pos {
        block,
        run: last,
        offset: runs[last].text.len(),
    }
}

fn next_char_len(s: &str, offset: usize) -> usize {
    s[offset..].chars().next().map_or(1, char::len_utf8)
}

fn prev_char_len(s: &str, offset: usize) -> usize {
    s[..offset].chars().next_back().map_or(1, char::len_utf8)
}

/// The position one char after `pos`, moving into the next run/block at a
/// run/block boundary. Returns `pos` unchanged at the very end of the
/// document.
fn next_pos(text: &RichText, pos: Pos) -> Pos {
    let pos = clamp_in(text, pos);
    let runs = block_runs(text, pos.block);
    if !runs.is_empty() {
        let run_text = &runs[pos.run].text;
        if pos.offset < run_text.len() {
            let advance = next_char_len(run_text, pos.offset);
            return Pos {
                block: pos.block,
                run: pos.run,
                offset: pos.offset + advance,
            };
        }
        if pos.run + 1 < runs.len() {
            return Pos {
                block: pos.block,
                run: pos.run + 1,
                offset: 0,
            };
        }
    }
    if pos.block + 1 < text.blocks.len() {
        return Pos {
            block: pos.block + 1,
            run: 0,
            offset: 0,
        };
    }
    pos
}

/// The position one char before `pos`, moving into the previous run/block
/// at a run/block boundary. Returns `pos` unchanged at the very start of
/// the document.
fn prev_pos(text: &RichText, pos: Pos) -> Pos {
    let pos = clamp_in(text, pos);
    let runs = block_runs(text, pos.block);
    if !runs.is_empty() {
        let run_text = &runs[pos.run].text;
        if pos.offset > 0 {
            let back = prev_char_len(run_text, pos.offset);
            return Pos {
                block: pos.block,
                run: pos.run,
                offset: pos.offset - back,
            };
        }
        if pos.run > 0 {
            let prev_run = pos.run - 1;
            return Pos {
                block: pos.block,
                run: prev_run,
                offset: runs[prev_run].text.len(),
            };
        }
    }
    if pos.block > 0 {
        let prev_block = pos.block - 1;
        let prev_runs = block_runs(text, prev_block);
        if prev_runs.is_empty() {
            return Pos {
                block: prev_block,
                run: 0,
                offset: 0,
            };
        }
        let last = prev_runs.len() - 1;
        return Pos {
            block: prev_block,
            run: last,
            offset: prev_runs[last].text.len(),
        };
    }
    pos
}

// ---------------------------------------------------------------------
// Range removal: deletes the text between two clamped, ordered positions,
// splicing runs and merging blocks as needed.
// ---------------------------------------------------------------------

fn truncate_block_to_prefix(runs: &mut Vec<Run>, run_idx: usize, offset: usize) {
    if runs.is_empty() {
        return;
    }
    let run_idx = run_idx.min(runs.len() - 1);
    let len = runs[run_idx].text.len();
    let off = clamp_char_boundary(&runs[run_idx].text, offset.min(len));
    if off == 0 {
        runs.truncate(run_idx);
    } else {
        runs[run_idx].text.truncate(off);
        runs.truncate(run_idx + 1);
    }
}

fn suffix_runs(runs: &[Run], run_idx: usize, offset: usize) -> Vec<Run> {
    if runs.is_empty() {
        return Vec::new();
    }
    let run_idx = run_idx.min(runs.len() - 1);
    let len = runs[run_idx].text.len();
    let off = clamp_char_boundary(&runs[run_idx].text, offset.min(len));
    let mut out = Vec::new();
    if off < len {
        let mut r = runs[run_idx].clone();
        r.text = runs[run_idx].text[off..].to_string();
        out.push(r);
    }
    out.extend(runs[run_idx + 1..].iter().cloned());
    out
}

fn remove_run_range(
    runs: &mut Vec<Run>,
    from_run: usize,
    from_offset: usize,
    to_run: usize,
    to_offset: usize,
) {
    if runs.is_empty() {
        return;
    }
    let from_run = from_run.min(runs.len() - 1);
    let to_run = to_run.min(runs.len() - 1);
    if from_run == to_run {
        let run = &mut runs[from_run];
        let len = run.text.len();
        let f = clamp_char_boundary(&run.text, from_offset.min(len));
        let t = clamp_char_boundary(&run.text, to_offset.min(len)).max(f);
        run.text.replace_range(f..t, "");
        return;
    }
    let from_len = runs[from_run].text.len();
    let f = clamp_char_boundary(&runs[from_run].text, from_offset.min(from_len));
    runs[from_run].text.truncate(f);

    let to_len = runs[to_run].text.len();
    let t = clamp_char_boundary(&runs[to_run].text, to_offset.min(to_len));
    runs[to_run].text.replace_range(0..t, "");

    // Defensive: `from_run < to_run` holds whenever the caller passes a
    // properly ordered range, but a stale/out-of-range `Selection` (set
    // directly via the public `sel` field) can clamp down onto indices that
    // end up reversed. Guard the drain so that degrades to a no-op instead
    // of panicking with "slice index starts at N but ends at M".
    if to_run > from_run + 1 {
        runs.drain(from_run + 1..to_run);
    }
}

/// Removes the text between `start` and `end` (already clamped, `start <=
/// end`), merging blocks together when the range spans more than one.
fn remove_range(text: &mut RichText, start: Pos, end: Pos) {
    if start.block == end.block {
        let runs = block_runs_mut(text, start.block);
        remove_run_range(runs, start.run, start.offset, end.run, end.offset);
        return;
    }
    truncate_block_to_prefix(block_runs_mut(text, start.block), start.run, start.offset);
    let suffix = suffix_runs(block_runs(text, end.block), end.run, end.offset);
    block_runs_mut(text, start.block).extend(suffix);
    text.blocks.drain(start.block + 1..=end.block);
}

// ---------------------------------------------------------------------
// Style application: splits boundary runs so a style toggle (or a link)
// affects exactly the selected span, leaving the rest of each run intact.
// ---------------------------------------------------------------------

/// Splits/rebuilds `runs` so the sub-range `[from_run, to_run]` (with
/// per-run byte bounds `from_offset`/`to_offset` at the ends) has `set`
/// applied, while text outside that range is left untouched.
fn apply_range(
    runs: &mut Vec<Run>,
    from_run: usize,
    from_offset: usize,
    to_run: usize,
    to_offset: usize,
    target: bool,
    set: &dyn Fn(&mut Run, bool),
) {
    if runs.is_empty() {
        return;
    }
    let from_run = from_run.min(runs.len() - 1);
    let to_run = to_run.min(runs.len() - 1);
    if from_run > to_run {
        return;
    }
    let mut new_runs: Vec<Run> = Vec::with_capacity(runs.len() + 2);
    for (idx, run) in runs.iter().enumerate() {
        if idx < from_run || idx > to_run {
            new_runs.push(run.clone());
            continue;
        }
        let len = run.text.len();
        let seg_from = if idx == from_run {
            clamp_char_boundary(&run.text, from_offset.min(len))
        } else {
            0
        };
        let seg_to = if idx == to_run {
            clamp_char_boundary(&run.text, to_offset.min(len)).max(seg_from)
        } else {
            len
        };
        if seg_from > 0 {
            let mut prefix = run.clone();
            prefix.text = run.text[..seg_from].to_string();
            new_runs.push(prefix);
        }
        if seg_to > seg_from {
            let mut middle = run.clone();
            middle.text = run.text[seg_from..seg_to].to_string();
            set(&mut middle, target);
            new_runs.push(middle);
        }
        if seg_to < len {
            let mut suffix = run.clone();
            suffix.text = run.text[seg_to..].to_string();
            new_runs.push(suffix);
        }
    }
    *runs = new_runs;
}

impl Editor {
    fn clamp(&self, pos: Pos) -> Pos {
        clamp_in(&self.text, pos)
    }

    fn ordered_clamped(&self) -> (Pos, Pos) {
        let (a, b) = self.sel.ordered();
        let (a, b) = (self.clamp(a), self.clamp(b));
        // Clamping each endpoint independently (e.g. because one endpoint's
        // block/run index was stale or out of range) can reverse their
        // relative order even though `sel.ordered()` sorted the originals.
        // Every downstream consumer assumes `from <= to`, so restore that
        // invariant here rather than at each call site.
        if a <= b { (a, b) } else { (b, a) }
    }

    fn record(&mut self) {
        self.history.record(&self.text, &self.sel);
    }

    fn ensure_nonempty(&mut self) {
        if self.text.blocks.is_empty() {
            self.text.blocks.push(Block::Paragraph(Vec::new()));
        }
    }

    fn remove_range_at_selection(&mut self) {
        let (start, end) = self.ordered_clamped();
        if start == end {
            self.sel = Selection {
                anchor: start,
                caret: start,
            };
            return;
        }
        remove_range(&mut self.text, start, end);
        self.ensure_nonempty();
        self.sel = Selection {
            anchor: start,
            caret: start,
        };
    }

    fn insert_at(&mut self, pos: Pos, s: &str) -> Pos {
        let block = pos.block;
        let runs = block_runs_mut(&mut self.text, block);
        if runs.is_empty() {
            runs.push(Run::plain(s));
            return Pos {
                block,
                run: 0,
                offset: s.len(),
            };
        }
        let run_idx = pos.run.min(runs.len() - 1);
        let run = &mut runs[run_idx];
        let off = clamp_char_boundary(&run.text, pos.offset.min(run.text.len()));
        run.text.insert_str(off, s);
        Pos {
            block,
            run: run_idx,
            offset: off + s.len(),
        }
    }

    /// Inserts `s` at the caret, replacing the selection first if it isn't
    /// collapsed.
    pub fn insert_text(&mut self, s: &str) {
        self.record();
        if s.is_empty() {
            return;
        }
        if !self.sel.is_collapsed() {
            self.remove_range_at_selection();
        }
        let caret = self.clamp(self.sel.caret);
        let new_caret = self.insert_at(caret, s);
        self.sel = Selection {
            anchor: new_caret,
            caret: new_caret,
        };
    }

    /// Deletes the character before the caret, or the selection if one is
    /// active. At the very start of a block, merges with the previous
    /// block. A no-op (never panics) at the very start of the document.
    pub fn delete_backward(&mut self) {
        self.record();
        if !self.sel.is_collapsed() {
            self.remove_range_at_selection();
            return;
        }
        let caret = self.clamp(self.sel.caret);
        let before = prev_pos(&self.text, caret);
        if before == caret {
            self.sel = Selection {
                anchor: caret,
                caret,
            };
            return;
        }
        remove_range(&mut self.text, before, caret);
        self.ensure_nonempty();
        self.sel = Selection {
            anchor: before,
            caret: before,
        };
    }

    /// Deletes the current selection, collapsing the caret to its start.
    /// A no-op when the selection is already collapsed.
    pub fn delete_selection(&mut self) {
        self.record();
        self.remove_range_at_selection();
    }

    /// Splits the current block into two at the caret (replacing the
    /// selection first if one is active). Preserves the block's kind
    /// (paragraph, or list item at the same level/ordering) on both halves.
    pub fn split_paragraph(&mut self) {
        self.record();
        if !self.sel.is_collapsed() {
            self.remove_range_at_selection();
        }
        let caret = self.clamp(self.sel.caret);
        let block_idx = caret.block;
        let list_kind = match &self.text.blocks[block_idx] {
            Block::ListItem { ordered, level, .. } => Some((*ordered, *level)),
            Block::Paragraph(_) => None,
        };
        let runs = block_runs_mut(&mut self.text, block_idx);
        let mut before: Vec<Run> = Vec::new();
        let mut after: Vec<Run> = Vec::new();
        if !runs.is_empty() {
            let run_idx = caret.run.min(runs.len() - 1);
            for (i, run) in runs.iter().enumerate() {
                match i.cmp(&run_idx) {
                    std::cmp::Ordering::Less => before.push(run.clone()),
                    std::cmp::Ordering::Greater => after.push(run.clone()),
                    std::cmp::Ordering::Equal => {
                        let off = clamp_char_boundary(&run.text, caret.offset.min(run.text.len()));
                        if off > 0 {
                            let mut b = run.clone();
                            b.text = run.text[..off].to_string();
                            before.push(b);
                        }
                        if off < run.text.len() {
                            let mut a = run.clone();
                            a.text = run.text[off..].to_string();
                            after.push(a);
                        }
                    }
                }
            }
        }
        let make = |kind: Option<(bool, u8)>, runs: Vec<Run>| match kind {
            Some((ordered, level)) => Block::ListItem {
                ordered,
                level,
                runs,
            },
            None => Block::Paragraph(runs),
        };
        let block_a = make(list_kind, before);
        let block_b = make(list_kind, after);
        self.text
            .blocks
            .splice(block_idx..=block_idx, [block_a, block_b]);
        let new_caret = Pos {
            block: block_idx + 1,
            run: 0,
            offset: 0,
        };
        self.sel = Selection {
            anchor: new_caret,
            caret: new_caret,
        };
    }

    fn block_style_bounds(
        &self,
        b: usize,
        start: Pos,
        end: Pos,
    ) -> Option<(usize, usize, usize, usize)> {
        let runs = block_runs(&self.text, b);
        if runs.is_empty() {
            return None;
        }
        let from_run = if b == start.block {
            start.run.min(runs.len() - 1)
        } else {
            0
        };
        let from_offset = if b == start.block {
            start.offset.min(runs[from_run].text.len())
        } else {
            0
        };
        let to_run = if b == end.block {
            end.run.min(runs.len() - 1)
        } else {
            runs.len() - 1
        };
        let to_offset = if b == end.block {
            end.offset.min(runs[to_run].text.len())
        } else {
            runs[to_run].text.len()
        };
        if from_run > to_run {
            return None;
        }
        Some((from_run, from_offset, to_run, to_offset))
    }

    fn style_target(&self, start: Pos, end: Pos, get: &dyn Fn(&Run) -> bool) -> bool {
        let mut any = false;
        let mut all_true = true;
        for b in start.block..=end.block {
            let Some((fr, fo, tr, to)) = self.block_style_bounds(b, start, end) else {
                continue;
            };
            let runs = block_runs(&self.text, b);
            for idx in fr..=tr {
                let run = &runs[idx];
                let len = run.text.len();
                let seg_from = if idx == fr { fo } else { 0 };
                let seg_to = if idx == tr { to } else { len };
                if seg_to <= seg_from {
                    continue;
                }
                any = true;
                if !get(run) {
                    all_true = false;
                }
            }
        }
        !(any && all_true)
    }

    fn apply_style_range(
        &mut self,
        start: Pos,
        end: Pos,
        target: bool,
        set: &dyn Fn(&mut Run, bool),
    ) {
        for b in start.block..=end.block {
            let Some((fr, fo, tr, to)) = self.block_style_bounds(b, start, end) else {
                continue;
            };
            let runs = block_runs_mut(&mut self.text, b);
            apply_range(runs, fr, fo, tr, to, target, set);
        }
    }

    /// Toggles a style over the selection, splitting boundary runs so only
    /// the selected span changes. At a collapsed caret, toggles the whole
    /// run the caret sits in (there being no selection to bound a split
    /// to).
    fn toggle_style(&mut self, get: &dyn Fn(&Run) -> bool, set: &dyn Fn(&mut Run, bool)) {
        self.record();
        let (start, end) = self.ordered_clamped();
        if start == end {
            if let Some(run) = block_runs_mut(&mut self.text, start.block).get_mut(start.run) {
                let cur = get(run);
                set(run, !cur);
            }
            return;
        }
        // Splitting runs to bound the toggle changes run indices within the
        // affected blocks, so carry the anchor/caret across the mutation as
        // block-relative byte offsets rather than stale (run, offset) pairs.
        let anchor_clamped = self.clamp(self.sel.anchor);
        let caret_clamped = self.clamp(self.sel.caret);
        let anchor_off = block_offset(&self.text, anchor_clamped);
        let caret_off = block_offset(&self.text, caret_clamped);
        let target = self.style_target(start, end, get);
        self.apply_style_range(start, end, target, set);
        self.sel = Selection {
            anchor: pos_from_block_offset(&self.text, anchor_clamped.block, anchor_off),
            caret: pos_from_block_offset(&self.text, caret_clamped.block, caret_off),
        };
    }

    /// Toggles bold over the selection (or the run at a collapsed caret).
    pub fn toggle_bold(&mut self) {
        self.toggle_style(&|r| r.bold, &|r, v| r.bold = v);
    }

    /// Toggles italic over the selection (or the run at a collapsed caret).
    pub fn toggle_italic(&mut self) {
        self.toggle_style(&|r| r.italic, &|r, v| r.italic = v);
    }

    /// Toggles underline over the selection (or the run at a collapsed
    /// caret).
    pub fn toggle_underline(&mut self) {
        self.toggle_style(&|r| r.underline, &|r, v| r.underline = v);
    }

    /// Sets `url` as the link target over the selection, splitting boundary
    /// runs so only the selected span is linked. A no-op at a collapsed
    /// caret (there's no span to link).
    pub fn make_link(&mut self, url: &str) {
        self.record();
        let (start, end) = self.ordered_clamped();
        if start == end {
            return;
        }
        let anchor_clamped = self.clamp(self.sel.anchor);
        let caret_clamped = self.clamp(self.sel.caret);
        let anchor_off = block_offset(&self.text, anchor_clamped);
        let caret_off = block_offset(&self.text, caret_clamped);
        let url = url.to_string();
        let set = move |r: &mut Run, _v: bool| r.link = Some(url.clone());
        self.apply_style_range(start, end, true, &set);
        self.sel = Selection {
            anchor: pos_from_block_offset(&self.text, anchor_clamped.block, anchor_off),
            caret: pos_from_block_offset(&self.text, caret_clamped.block, caret_off),
        };
    }

    /// Toggles the blocks in the selection between list items (of the given
    /// `ordered`-ness) and plain paragraphs.
    pub fn list_toggle(&mut self, ordered: bool) {
        self.record();
        let (start, end) = self.ordered_clamped();
        for b in start.block..=end.block {
            let block = &mut self.text.blocks[b];
            *block = match std::mem::replace(block, Block::Paragraph(Vec::new())) {
                Block::ListItem {
                    ordered: o, runs, ..
                } if o == ordered => Block::Paragraph(runs),
                Block::ListItem { level, runs, .. } => Block::ListItem {
                    ordered,
                    level,
                    runs,
                },
                Block::Paragraph(runs) => Block::ListItem {
                    ordered,
                    level: 0,
                    runs,
                },
            };
        }
    }

    /// Increases the list level of list items in the selection (capped; a
    /// no-op on plain paragraphs).
    pub fn indent(&mut self) {
        self.record();
        let (start, end) = self.ordered_clamped();
        for b in start.block..=end.block {
            if let Block::ListItem { level, .. } = &mut self.text.blocks[b] {
                *level = level.saturating_add(1).min(8);
            }
        }
    }

    /// Decreases the list level of list items in the selection; a list item
    /// already at level 0 becomes a plain paragraph. A no-op on paragraphs.
    pub fn outdent(&mut self) {
        self.record();
        let (start, end) = self.ordered_clamped();
        for b in start.block..=end.block {
            let block = &mut self.text.blocks[b];
            if let Block::ListItem { level, .. } = block {
                if *level > 0 {
                    *level -= 1;
                    continue;
                }
            }
            if let Block::ListItem { runs, .. } = block {
                let runs = std::mem::take(runs);
                *block = Block::Paragraph(runs);
            }
        }
    }

    fn move_caret(&mut self, new_caret: Pos, extend: bool) {
        let anchor = if extend { self.sel.anchor } else { new_caret };
        self.sel = Selection {
            anchor,
            caret: new_caret,
        };
    }

    /// Moves the caret one char left; extends the selection instead of
    /// collapsing it when `extend` is true.
    pub fn move_left(&mut self, extend: bool) {
        let caret = self.clamp(self.sel.caret);
        let new_caret = prev_pos(&self.text, caret);
        self.move_caret(new_caret, extend);
    }

    /// Moves the caret one char right; extends the selection instead of
    /// collapsing it when `extend` is true.
    pub fn move_right(&mut self, extend: bool) {
        let caret = self.clamp(self.sel.caret);
        let new_caret = next_pos(&self.text, caret);
        self.move_caret(new_caret, extend);
    }

    /// Moves the caret to the previous block, keeping the same run/offset
    /// where possible (clamped). A headless approximation of "up" without a
    /// visual layout.
    pub fn move_up(&mut self, extend: bool) {
        let caret = self.clamp(self.sel.caret);
        let new_block = caret.block.saturating_sub(1);
        let new_caret = self.clamp(Pos {
            block: new_block,
            run: caret.run,
            offset: caret.offset,
        });
        self.move_caret(new_caret, extend);
    }

    /// Moves the caret to the next block, keeping the same run/offset where
    /// possible (clamped). A headless approximation of "down" without a
    /// visual layout.
    pub fn move_down(&mut self, extend: bool) {
        let caret = self.clamp(self.sel.caret);
        let new_block = (caret.block + 1).min(self.text.blocks.len().saturating_sub(1));
        let new_caret = self.clamp(Pos {
            block: new_block,
            run: caret.run,
            offset: caret.offset,
        });
        self.move_caret(new_caret, extend);
    }

    /// Moves the caret to the start of its current block.
    pub fn move_home(&mut self, extend: bool) {
        let caret = self.clamp(self.sel.caret);
        let new_caret = Pos {
            block: caret.block,
            run: 0,
            offset: 0,
        };
        self.move_caret(new_caret, extend);
    }

    /// Moves the caret to the end of its current block.
    pub fn move_end(&mut self, extend: bool) {
        let caret = self.clamp(self.sel.caret);
        let runs = block_runs(&self.text, caret.block);
        let new_caret = if runs.is_empty() {
            Pos {
                block: caret.block,
                run: 0,
                offset: 0,
            }
        } else {
            let last = runs.len() - 1;
            Pos {
                block: caret.block,
                run: last,
                offset: runs[last].text.len(),
            }
        };
        self.move_caret(new_caret, extend);
    }

    /// Restores the state before the most recent undoable op. Returns
    /// `false` (leaving the editor untouched) when there's nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.history.undo(&self.text, &self.sel) {
            Some((text, sel)) => {
                self.text = text;
                self.sel = sel;
                true
            }
            None => false,
        }
    }

    /// Re-applies the most recently undone op. Returns `false` (leaving the
    /// editor untouched) when there's nothing to redo.
    pub fn redo(&mut self) -> bool {
        match self.history.redo(&self.text, &self.sel) {
            Some((text, sel)) => {
                self.text = text;
                self.sel = sel;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(block: usize, run: usize, offset: usize) -> Pos {
        Pos { block, run, offset }
    }

    fn collapsed(block: usize, run: usize, offset: usize) -> Selection {
        let p = row(block, run, offset);
        Selection {
            anchor: p,
            caret: p,
        }
    }

    fn range(b1: usize, r1: usize, o1: usize, b2: usize, r2: usize, o2: usize) -> Selection {
        Selection {
            anchor: row(b1, r1, o1),
            caret: row(b2, r2, o2),
        }
    }

    #[test]
    fn insert_then_undo_redo() {
        let mut e = Editor::new();
        e.insert_text("hello");
        assert_eq!(e.text.plain(), "hello");
        e.undo();
        assert_eq!(e.text.plain(), "");
        e.redo();
        assert_eq!(e.text.plain(), "hello");
    }

    #[test]
    fn split_paragraph_creates_two_blocks() {
        let mut e = Editor::new();
        e.insert_text("ab");
        e.sel = collapsed(0, 0, 1); // between a and b
        e.split_paragraph();
        assert_eq!(e.text.blocks.len(), 2);
        assert_eq!(e.text.plain(), "a\nb");
    }

    #[test]
    fn toggle_bold_over_selection_sets_runs_bold() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("abcd")])],
        });
        e.sel = range(0, 0, 1, 0, 0, 3); // select "bc"
        e.toggle_bold();
        let runs = match &e.text.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        let bolded: String = runs
            .iter()
            .filter(|r| r.bold)
            .map(|r| r.text.clone())
            .collect();
        assert_eq!(bolded, "bc");
    }

    #[test]
    fn delete_backward_across_paragraph_merges() {
        let mut e = Editor::from(RichText {
            blocks: vec![
                Block::Paragraph(vec![Run::plain("a")]),
                Block::Paragraph(vec![Run::plain("b")]),
            ],
        });
        e.sel = collapsed(1, 0, 0); // start of 2nd paragraph
        e.delete_backward();
        assert_eq!(e.text.plain(), "ab");
        assert_eq!(e.text.blocks.len(), 1);
    }

    #[test]
    fn empty_editor_delete_backward_is_noop() {
        let mut e = Editor::new();
        e.delete_backward(); // must not panic
        assert!(e.text.is_empty());
    }

    // --- Additional boundary / no-panic coverage ---------------------

    #[test]
    fn split_paragraph_on_completely_empty_document_is_noop_like() {
        let mut e = Editor::new();
        e.split_paragraph(); // caret at {0,0,0} in a paragraph with no runs
        assert_eq!(e.text.blocks.len(), 2);
        assert_eq!(e.text.plain(), "\n");
    }

    #[test]
    fn delete_selection_spanning_whole_document_leaves_one_empty_block() {
        let mut e = Editor::from(RichText {
            blocks: vec![
                Block::Paragraph(vec![Run::plain("ab")]),
                Block::Paragraph(vec![Run::plain("cd")]),
            ],
        });
        e.sel = range(0, 0, 0, 1, 0, 2);
        e.delete_selection();
        assert_eq!(e.text.blocks.len(), 1);
        assert!(e.text.is_empty());
        assert_eq!(e.sel.caret, row(0, 0, 0));
    }

    #[test]
    fn delete_selection_across_blocks_merges_remainder() {
        let mut e = Editor::from(RichText {
            blocks: vec![
                Block::Paragraph(vec![Run::plain("abc")]),
                Block::Paragraph(vec![Run::plain("def")]),
            ],
        });
        e.sel = range(0, 0, 1, 1, 0, 2); // delete "bc" + "de" across blocks
        e.delete_selection();
        assert_eq!(e.text.blocks.len(), 1);
        assert_eq!(e.text.plain(), "af");
    }

    #[test]
    fn toggle_bold_at_collapsed_caret_toggles_run_without_panicking() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("x")])],
        });
        e.sel = collapsed(0, 0, 0);
        e.toggle_bold();
        let runs = match &e.text.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        assert!(runs[0].bold);
    }

    #[test]
    fn toggle_bold_on_empty_paragraph_does_not_panic() {
        let mut e = Editor::new();
        e.toggle_bold(); // no runs at all
        assert!(e.text.is_empty());
    }

    #[test]
    fn toggle_bold_twice_over_selection_toggles_back_off() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("abcd")])],
        });
        e.sel = range(0, 0, 1, 0, 0, 3);
        e.toggle_bold();
        e.toggle_bold();
        let runs = match &e.text.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        assert!(runs.iter().all(|r| !r.bold));
        assert_eq!(e.text.plain(), "abcd");
    }

    #[test]
    fn make_link_sets_link_on_selected_span_only() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("abcd")])],
        });
        e.sel = range(0, 0, 1, 0, 0, 3);
        e.make_link("https://example.com");
        let runs = match &e.text.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        let linked: String = runs
            .iter()
            .filter(|r| r.link.is_some())
            .map(|r| r.text.clone())
            .collect();
        assert_eq!(linked, "bc");
    }

    #[test]
    fn make_link_at_collapsed_caret_is_noop() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("abcd")])],
        });
        e.sel = collapsed(0, 0, 2);
        e.make_link("https://example.com");
        let runs = match &e.text.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        assert!(runs.iter().all(|r| r.link.is_none()));
    }

    #[test]
    fn list_toggle_then_toggle_back_round_trips() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("item")])],
        });
        e.sel = collapsed(0, 0, 0);
        e.list_toggle(false);
        assert!(matches!(
            e.text.blocks[0],
            Block::ListItem { ordered: false, .. }
        ));
        e.list_toggle(false);
        assert!(matches!(e.text.blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn indent_and_outdent_list_item() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::ListItem {
                ordered: true,
                level: 0,
                runs: vec![Run::plain("item")],
            }],
        });
        e.sel = collapsed(0, 0, 0);
        e.indent();
        match &e.text.blocks[0] {
            Block::ListItem { level, .. } => assert_eq!(*level, 1),
            _ => panic!(),
        }
        e.outdent();
        match &e.text.blocks[0] {
            Block::ListItem { level, .. } => assert_eq!(*level, 0),
            _ => panic!(),
        }
        e.outdent(); // at level 0: becomes a plain paragraph
        assert!(matches!(e.text.blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn outdent_on_paragraph_is_noop() {
        let mut e = Editor::new();
        e.insert_text("x");
        e.sel = collapsed(0, 0, 0);
        e.outdent();
        assert!(matches!(e.text.blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn cursor_moves_clamp_at_document_boundaries_without_panicking() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("ab")])],
        });
        e.sel = collapsed(0, 0, 0);
        e.move_left(false); // already at start: no-op
        assert_eq!(e.sel.caret, row(0, 0, 0));
        e.move_up(false); // no previous block: stays put
        assert_eq!(e.sel.caret, row(0, 0, 0));

        e.sel = collapsed(0, 0, 2);
        e.move_right(false); // already at end: no-op
        assert_eq!(e.sel.caret, row(0, 0, 2));
        e.move_down(false); // no next block: stays put
        assert_eq!(e.sel.caret, row(0, 0, 2));
    }

    #[test]
    fn move_right_then_left_round_trips_across_blocks() {
        let mut e = Editor::from(RichText {
            blocks: vec![
                Block::Paragraph(vec![Run::plain("a")]),
                Block::Paragraph(vec![Run::plain("b")]),
            ],
        });
        e.sel = collapsed(0, 0, 1); // end of first block
        e.move_right(false);
        assert_eq!(e.sel.caret, row(1, 0, 0));
        e.move_left(false);
        assert_eq!(e.sel.caret, row(0, 0, 1));
    }

    #[test]
    fn move_right_extends_selection_when_requested() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("abc")])],
        });
        e.sel = collapsed(0, 0, 0);
        e.move_right(true);
        e.move_right(true);
        assert_eq!(e.sel.anchor, row(0, 0, 0));
        assert_eq!(e.sel.caret, row(0, 0, 2));
        assert!(!e.sel.is_collapsed());
    }

    #[test]
    fn home_and_end_move_to_block_boundaries() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("hello")])],
        });
        e.sel = collapsed(0, 0, 3);
        e.move_home(false);
        assert_eq!(e.sel.caret, row(0, 0, 0));
        e.move_end(false);
        assert_eq!(e.sel.caret, row(0, 0, 5));
    }

    #[test]
    fn undo_redo_return_false_when_nothing_to_do() {
        let mut e = Editor::new();
        assert!(!e.undo());
        assert!(!e.redo());
    }

    #[test]
    fn new_edit_after_undo_clears_redo_history() {
        let mut e = Editor::new();
        e.insert_text("a");
        e.undo();
        e.insert_text("b");
        assert!(!e.redo());
        assert_eq!(e.text.plain(), "b");
    }

    #[test]
    fn delete_backward_merges_empty_previous_paragraph() {
        let mut e = Editor::from(RichText {
            blocks: vec![
                Block::Paragraph(vec![]),
                Block::Paragraph(vec![Run::plain("x")]),
            ],
        });
        e.sel = collapsed(1, 0, 0);
        e.delete_backward(); // previous block has zero runs: must not panic
        assert_eq!(e.text.blocks.len(), 1);
        assert_eq!(e.text.plain(), "x");
    }

    // --- Regression: reversed clamped selection (reviewer-found panic) ----
    //
    // `sel` is a public field, so a caller can hand the editor an anchor/
    // caret pair whose block/run indices are individually out of range.
    // `ordered()` sorts by the *raw* Pos values, but clamping each endpoint
    // independently can then reverse their effective order (e.g. one
    // endpoint's out-of-range block collapses onto the same block as the
    // other, and its clamped run index ends up larger). Every op that
    // deletes/replaces a range must survive this without panicking.

    #[test]
    fn delete_selection_with_reversed_clamped_selection_does_not_panic() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("xyz"), Run::plain("uvw")])],
        });
        e.sel = Selection {
            anchor: row(0, 5, 0), // run out of range for a 2-run block
            caret: row(3, 0, 0),  // block way past the end
        };
        e.delete_selection(); // must not panic
        assert_eq!(e.text.blocks.len(), 1);
        assert_eq!(e.text.plain(), "uvw");
    }

    #[test]
    fn insert_text_with_reversed_clamped_selection_does_not_panic() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("ab"), Run::plain("cd")])],
        });
        e.sel = Selection {
            anchor: row(0, 5, 0),
            caret: row(3, 0, 0),
        };
        e.insert_text("Z"); // must not panic
        assert!(e.text.plain().contains('Z'));
    }

    #[test]
    fn split_paragraph_with_reversed_clamped_selection_does_not_panic() {
        let mut e = Editor::from(RichText {
            blocks: vec![Block::Paragraph(vec![Run::plain("ab"), Run::plain("cd")])],
        });
        e.sel = Selection {
            anchor: row(0, 5, 0),
            caret: row(3, 0, 0),
        };
        e.split_paragraph(); // must not panic
        assert_eq!(e.text.blocks.len(), 2);
    }

    #[test]
    fn delete_selection_with_caret_block_far_past_end_does_not_panic() {
        let mut e = Editor::from(RichText {
            blocks: vec![
                Block::Paragraph(vec![Run::plain("a")]),
                Block::Paragraph(vec![Run::plain("b")]),
            ],
        });
        e.sel = Selection {
            anchor: row(0, 0, 0),
            caret: row(999, 9, 9), // block/run/offset all far past the end
        };
        e.delete_selection(); // must not panic
        assert_eq!(e.text.blocks.len(), 1);
        assert!(e.text.is_empty());
    }
}
