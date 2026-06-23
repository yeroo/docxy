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
                    '\r' => {}
                    '\t' => {
                        if !buf.is_empty() {
                            inl.push(Inline::Run(Run {
                                text: std::mem::take(&mut buf),
                                props: RunProps::default(),
                            }));
                        }
                        inl.push(Inline::Tab);
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
    doc: Document,
    caret: Caret,
}

const UNDO_CAP: usize = 500;

/// An editing session over a [`Document`].
pub struct Editor {
    pub doc: Document,
    pub caret: Caret,
    /// Selection anchor (the fixed end); the moving end is the caret.
    pub anchor: Option<Caret>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: EditKind,
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        let path = first_paragraph_path(&doc.body).unwrap_or_else(|| vec![0]);
        Editor {
            doc,
            caret: Caret { path, offset: 0 },
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last: EditKind::None,
        }
    }

    fn cur_len(&self) -> usize {
        resolve_para(&self.doc.body, &self.caret.path)
            .map(para_text_len)
            .unwrap_or(0)
    }

    fn checkpoint(&mut self, kind: EditKind) {
        if self.last != kind || kind == EditKind::Structural {
            self.undo.push(Snapshot {
                doc: self.doc.clone(),
                caret: self.caret.clone(),
            });
            if self.undo.len() > UNDO_CAP {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last = kind;
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(Snapshot {
                doc: self.doc.clone(),
                caret: self.caret.clone(),
            });
            self.doc = prev.doc;
            self.caret = prev.caret;
            self.last = EditKind::None;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(Snapshot {
                doc: self.doc.clone(),
                caret: self.caret.clone(),
            });
            self.doc = next.doc;
            self.caret = next.caret;
            self.last = EditKind::None;
            true
        } else {
            false
        }
    }

    pub fn insert_char(&mut self, ch: char) {
        if self.has_selection() {
            self.delete_selection();
        }
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        self.checkpoint(EditKind::Insert);
        let off = self.caret.offset;
        if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            content_insert(&mut p.content, off, ch);
            self.caret.offset += 1;
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert_char(ch);
        }
    }

    pub fn insert_newline(&mut self) {
        self.checkpoint(EditKind::Structural);
        let off = self.caret.offset;
        let new_idx = {
            let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
                return;
            };
            let Some(Block::Paragraph(p)) = cont.get_mut(idx) else {
                return;
            };
            let right = split_content(&mut p.content, off);
            let props = p.props.clone();
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

    /// Set (or clear) the bottom border of the paragraph at the caret — used by a
    /// menu/command to insert a horizontal line directly.
    pub fn set_hrule(&mut self, kind: Option<BorderKind>) {
        self.checkpoint(EditKind::Structural);
        if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
            p.props.borders.bottom = kind;
        }
    }

    pub fn backspace(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if self.caret.offset > 0 {
            self.checkpoint(EditKind::Delete);
            let off = self.caret.offset;
            if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
                content_delete(&mut p.content, off - 1);
            }
            self.caret.offset -= 1;
            return;
        }
        // At the start of a paragraph: merge into the previous sibling paragraph.
        self.checkpoint(EditKind::Structural);
        let merged = {
            let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
                return;
            };
            if idx == 0 || !matches!(cont.get(idx - 1), Some(Block::Paragraph(_))) {
                None
            } else {
                let prev_len = match &cont[idx - 1] {
                    Block::Paragraph(p) => para_text_len(p),
                    _ => 0,
                };
                let this = match &mut cont[idx] {
                    Block::Paragraph(p) => std::mem::take(&mut p.content),
                    _ => Vec::new(),
                };
                if let Block::Paragraph(prev) = &mut cont[idx - 1] {
                    prev.content.extend(this);
                }
                cont.remove(idx);
                Some((idx - 1, prev_len))
            }
        };
        if let Some((nidx, plen)) = merged {
            if let Some(last) = self.caret.path.last_mut() {
                *last = nidx;
            }
            self.caret.offset = plen;
        } else {
            // nothing to merge; drop the snapshot we may have pushed
            self.last = EditKind::None;
        }
    }

    pub fn delete_forward(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let off = self.caret.offset;
        if off < self.cur_len() {
            self.checkpoint(EditKind::Delete);
            if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
                content_delete(&mut p.content, off);
            }
            return;
        }
        // At the end: pull up the next sibling paragraph.
        self.checkpoint(EditKind::Structural);
        let did = {
            let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
                return;
            };
            if idx + 1 >= cont.len() || !matches!(cont.get(idx + 1), Some(Block::Paragraph(_))) {
                false
            } else {
                let next = match &mut cont[idx + 1] {
                    Block::Paragraph(p) => std::mem::take(&mut p.content),
                    _ => Vec::new(),
                };
                if let Block::Paragraph(p) = &mut cont[idx] {
                    p.content.extend(next);
                }
                cont.remove(idx + 1);
                true
            }
        };
        if !did {
            self.last = EditKind::None;
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

    fn cur_text(&self) -> String {
        resolve_para(&self.doc.body, &self.caret.path)
            .map(|p| p.plain_text())
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
        if resolve_para(&self.doc.body, &self.caret.path).is_none() {
            if let Some(p) = first_paragraph_path(&self.doc.body) {
                self.caret.path = p;
            }
            self.caret.offset = 0;
        }
        let len = self.cur_len();
        if self.caret.offset > len {
            self.caret.offset = len;
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
    pub fn selection_spans(&self) -> Vec<(Vec<usize>, usize, usize)> {
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
    pub fn delete_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.selection_range() else {
            return false;
        };
        self.anchor = None;

        if lo.path == hi.path {
            self.checkpoint(EditKind::Structural);
            if let Some(p) = para_mut(&mut self.doc.body, &lo.path) {
                for _ in lo.offset..hi.offset {
                    content_delete(&mut p.content, lo.offset);
                }
            }
            self.caret = lo;
            return true;
        }

        // Same container (siblings)? Compare the parent path.
        let same_container = lo.path.len() == hi.path.len()
            && lo.path[..lo.path.len() - 1] == hi.path[..hi.path.len() - 1];
        if !same_container {
            self.caret = lo; // cross-container: collapse (rare)
            return false;
        }

        self.checkpoint(EditKind::Structural);
        let li = *lo.path.last().unwrap();
        let hii = *hi.path.last().unwrap();
        if let Some((cont, _)) = container_mut(&mut self.doc.body, &lo.path) {
            // Truncate the first paragraph at lo.offset.
            if let Some(Block::Paragraph(p)) = cont.get_mut(li) {
                let len: usize = p.content.iter().map(inline_len).sum();
                for _ in lo.offset..len {
                    content_delete(&mut p.content, lo.offset);
                }
            }
            // Take the remainder of the last paragraph (after hi.offset).
            let remainder = if let Some(Block::Paragraph(p)) = cont.get_mut(hii) {
                for _ in 0..hi.offset {
                    content_delete(&mut p.content, 0);
                }
                std::mem::take(&mut p.content)
            } else {
                Vec::new()
            };
            // Remove everything strictly between (and the now-empty last).
            for _ in (li + 1)..=hii {
                if li + 1 < cont.len() {
                    cont.remove(li + 1);
                }
            }
            // Merge the remainder onto the first paragraph.
            if let Some(Block::Paragraph(p)) = cont.get_mut(li) {
                p.content.extend(remainder);
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
                paras.push(extract_range(&p.content, s, e));
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

    /// Paste a [`Clip`] at the caret (replacing any selection).
    pub fn paste(&mut self, clip: &Clip) {
        if clip.paras.is_empty() {
            return;
        }
        if self.has_selection() {
            self.delete_selection();
        }
        self.checkpoint(EditKind::Structural);
        let off = self.caret.offset;
        let n = clip.paras.len();

        if n == 1 {
            if let Some(p) = para_mut(&mut self.doc.body, &self.caret.path) {
                let tail = split_content(&mut p.content, off);
                let ins = clip.paras[0].clone();
                let ins_len: usize = ins.iter().map(inline_len).sum();
                p.content.extend(ins);
                p.content.extend(tail);
                self.caret.offset = off + ins_len;
            }
            return;
        }

        let placed = {
            let Some((cont, idx)) = container_mut(&mut self.doc.body, &self.caret.path) else {
                return;
            };
            let Some(Block::Paragraph(p)) = cont.get_mut(idx) else {
                return;
            };
            let props = p.props.clone();
            let tail = split_content(&mut p.content, off);
            p.content.extend(clip.paras[0].clone());

            let mut news: Vec<Block> = Vec::new();
            for mid in &clip.paras[1..n - 1] {
                news.push(Block::Paragraph(Paragraph {
                    props: props.clone(),
                    content: mid.clone(),
                }));
            }
            let last_pasted = clip.paras[n - 1].clone();
            let last_len: usize = last_pasted.iter().map(inline_len).sum();
            let mut last_content = last_pasted;
            last_content.extend(tail);
            news.push(Block::Paragraph(Paragraph {
                props: props.clone(),
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
    }

    // ---- formatting ----

    pub fn toggle_bold(&mut self) {
        self.toggle_run_prop(|p| p.bold, |p, v| p.bold = v);
    }
    pub fn toggle_italic(&mut self) {
        self.toggle_run_prop(|p| p.italic, |p, v| p.italic = v);
    }
    pub fn toggle_underline(&mut self) {
        self.toggle_run_prop(|p| p.underline, |p, v| p.underline = v);
    }
    pub fn toggle_strike(&mut self) {
        self.toggle_run_prop(|p| p.strike, |p, v| p.strike = v);
    }

    /// Toggle a run property over the selection. The new value is "off" only if
    /// every selected character already has it (so it works like Word).
    fn toggle_run_prop(&mut self, get: fn(&RunProps) -> bool, set: fn(&mut RunProps, bool)) {
        let spans = self.selection_spans();
        if spans.is_empty() {
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
    }

    // ---- find / replace ----

    /// All matches of `query` (non-overlapping), in document order.
    pub fn find_all(&self, query: &str, case_sensitive: bool) -> Vec<Match> {
        if query.is_empty() {
            return Vec::new();
        }
        let q: Vec<char> = query.chars().collect();
        let mut out = Vec::new();
        for path in all_paragraph_paths(&self.doc.body) {
            let Some(p) = resolve_para(&self.doc.body, &path) else {
                continue;
            };
            let t: Vec<char> = p.plain_text().chars().collect();
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

    /// The next match relative to the caret (wrapping), forward or backward.
    pub fn find_next(&self, query: &str, case_sensitive: bool, reverse: bool) -> Option<Match> {
        let all = self.find_all(query, case_sensitive);
        if all.is_empty() {
            return None;
        }
        let paths = all_paragraph_paths(&self.doc.body);
        let key = |path: &[usize], off: usize| {
            (
                paths.iter().position(|p| p.as_slice() == path).unwrap_or(0),
                off,
            )
        };
        let ck = key(&self.caret.path, self.caret.offset);
        if reverse {
            all.iter()
                .rev()
                .find(|m| key(&m.path, m.start) < ck)
                .cloned()
                .or_else(|| all.last().cloned())
        } else {
            all.iter()
                .find(|m| key(&m.path, m.start) > ck)
                .cloned()
                .or_else(|| all.first().cloned())
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
    pub fn replace_current_with(&mut self, text: &str) {
        if self.has_selection() {
            self.delete_selection();
            self.insert_str(text);
        }
    }

    /// Replace every match of `query` with `with`. Returns the number replaced.
    pub fn replace_all(&mut self, query: &str, with: &str, case_sensitive: bool) -> usize {
        let matches = self.find_all(query, case_sensitive);
        if matches.is_empty() {
            return 0;
        }
        self.checkpoint(EditKind::Structural);
        // Group consecutive matches by paragraph (find_all already orders them).
        let mut groups: Vec<(Vec<usize>, Vec<Match>)> = Vec::new();
        for m in matches {
            if let Some(last) = groups.last_mut() {
                if last.0 == m.path {
                    last.1.push(m);
                    continue;
                }
            }
            groups.push((m.path.clone(), vec![m]));
        }
        let mut count = 0;
        for (path, mut ms) in groups {
            ms.sort_by_key(|m| std::cmp::Reverse(m.start)); // back-to-front keeps offsets valid
            if let Some(p) = para_mut(&mut self.doc.body, &path) {
                for m in ms {
                    for _ in m.start..m.end {
                        content_delete(&mut p.content, m.start);
                    }
                    for (k, ch) in with.chars().enumerate() {
                        content_insert(&mut p.content, m.start + k, ch);
                    }
                    count += 1;
                }
            }
        }
        self.clear_selection();
        self.clamp();
        count
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

fn para_text_len(p: &Paragraph) -> usize {
    p.content.iter().map(inline_len).sum()
}

fn resolve_para<'a>(body: &'a [Block], path: &[usize]) -> Option<&'a Paragraph> {
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
            Block::Raw(_) => {}
        }
        prefix.pop();
    }
}

// ---- content editing (operate on a paragraph's inline vector) ----

fn inline_len(i: &Inline) -> usize {
    match i {
        Inline::Run(r) => r.text.chars().count(),
        Inline::Hyperlink(h) => h.runs.iter().map(|r| r.text.chars().count()).sum(),
        Inline::Tab | Inline::Break(_) => 1,
        // Zero-length, invisible in the editor (preserved for save only).
        Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::Field { .. }
        | Inline::TextBox { .. }
        | Inline::Raw(_) => 0,
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
                if !runs.is_empty() {
                    out.push(Inline::Hyperlink(Hyperlink {
                        target: h.target.clone(),
                        anchor: h.anchor.clone(),
                        rel_id: h.rel_id.clone(),
                        runs,
                    }));
                }
            }
            Inline::Tab => out.push(Inline::Tab),
            Inline::Break(k) => out.push(Inline::Break(*k)),
            Inline::SmartArt { raw, text } => out.push(Inline::SmartArt {
                raw: raw.clone(),
                text: text.clone(),
            }),
            Inline::Chart { raw, chart } => out.push(Inline::Chart {
                raw: raw.clone(),
                chart: chart.clone(),
            }),
            Inline::Equation { raw, text } => out.push(Inline::Equation {
                raw: raw.clone(),
                text: text.clone(),
            }),
            Inline::Field { raw, text } => out.push(Inline::Field {
                raw: raw.clone(),
                text: text.clone(),
            }),
            Inline::TextBox { raw, blocks } => out.push(Inline::TextBox {
                raw: raw.clone(),
                blocks: blocks.clone(),
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

fn content_insert(content: &mut Vec<Inline>, o: usize, ch: char) {
    let mut acc = 0;
    for i in 0..content.len() {
        let l = inline_len(&content[i]);
        if o <= acc + l {
            let local = o - acc;
            match &mut content[i] {
                Inline::Run(r) => {
                    run_insert(r, local, ch);
                    return;
                }
                Inline::Hyperlink(h) => {
                    runs_insert(&mut h.runs, local, ch);
                    return;
                }
                Inline::Tab
                | Inline::Break(_)
                | Inline::SmartArt { .. }
                | Inline::Chart { .. }
                | Inline::Equation { .. }
                | Inline::Field { .. }
                | Inline::TextBox { .. }
                | Inline::Raw(_) => {
                    if local == 0 {
                        if i > 0 {
                            if let Inline::Run(r) = &mut content[i - 1] {
                                let rl = r.text.chars().count();
                                run_insert(r, rl, ch);
                                return;
                            }
                        }
                        content.insert(
                            i,
                            Inline::Run(Run {
                                text: ch.to_string(),
                                props: RunProps::default(),
                            }),
                        );
                        return;
                    } else {
                        if let Some(Inline::Run(r)) = content.get_mut(i + 1) {
                            run_insert(r, 0, ch);
                            return;
                        }
                        content.insert(
                            i + 1,
                            Inline::Run(Run {
                                text: ch.to_string(),
                                props: RunProps::default(),
                            }),
                        );
                        return;
                    }
                }
            }
        }
        acc += l;
    }
    if let Some(Inline::Run(r)) = content.last_mut() {
        let rl = r.text.chars().count();
        run_insert(r, rl, ch);
    } else {
        content.push(Inline::Run(Run {
            text: ch.to_string(),
            props: RunProps::default(),
        }));
    }
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
                    runs_delete(&mut h.runs, local);
                    if h.runs.is_empty() {
                        content.remove(i);
                    }
                }
                Inline::Tab
                | Inline::Break(_)
                | Inline::SmartArt { .. }
                | Inline::Chart { .. }
                | Inline::Equation { .. }
                | Inline::Field { .. }
                | Inline::TextBox { .. }
                | Inline::Raw(_) => {
                    content.remove(i);
                }
            }
            return;
        }
        acc += l;
    }
}

fn split_content(content: &mut Vec<Inline>, o: usize) -> Vec<Inline> {
    let mut acc = 0;
    for i in 0..content.len() {
        let l = inline_len(&content[i]);
        if o < acc + l {
            let local = o - acc;
            if local == 0 {
                return content.split_off(i);
            }
            if let Inline::Run(r) = &mut content[i] {
                let b = char_byte(&r.text, local);
                let right = r.text.split_off(b);
                let props = r.props.clone();
                let mut rest = content.split_off(i + 1);
                rest.insert(0, Inline::Run(Run { text: right, props }));
                return rest;
            }
            return content.split_off(i);
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
    let mut pos = 0;
    let mut saw = false;
    let mut check = |props: &RunProps, len: usize, pos: &mut usize| -> bool {
        let (a, b) = (*pos, *pos + len);
        let (os, oe) = (start.clamp(a, b), end.clamp(a, b));
        *pos = b;
        if oe > os {
            saw = true;
            if !get(props) {
                return false;
            }
        }
        true
    };
    for inline in content {
        match inline {
            Inline::Run(r) => {
                if !check(&r.props, r.text.chars().count(), &mut pos) {
                    return false;
                }
            }
            Inline::Hyperlink(h) => {
                for run in &h.runs {
                    if !check(&run.props, run.text.chars().count(), &mut pos) {
                        return false;
                    }
                }
            }
            Inline::Tab | Inline::Break(_) => pos += 1,
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::Field { .. }
            | Inline::TextBox { .. }
            | Inline::Raw(_) => {} // zero-length
        }
    }
    saw
}

/// Split a run so that `[start, end)` (absolute char positions, with the run
/// starting at `pos`) becomes its own run with `set(value)` applied.
fn split_run(
    r: Run,
    pos: usize,
    start: usize,
    end: usize,
    set: fn(&mut RunProps, bool),
    value: bool,
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
    let left = r.text[..b1].to_string();
    let mid = r.text[b1..b2].to_string();
    let right = r.text[b2..].to_string();
    let mut out = Vec::new();
    if !left.is_empty() {
        out.push(Run {
            text: left,
            props: r.props.clone(),
        });
    }
    let mut mp = r.props.clone();
    set(&mut mp, value);
    out.push(Run {
        text: mid,
        props: mp,
    });
    if !right.is_empty() {
        out.push(Run {
            text: right,
            props: r.props.clone(),
        });
    }
    out
}

/// Apply `set(value)` to every run-character in `[start, end)`, splitting runs.
fn set_prop_range(
    content: &mut Vec<Inline>,
    start: usize,
    end: usize,
    set: fn(&mut RunProps, bool),
    value: bool,
) {
    let mut out = Vec::new();
    let mut pos = 0;
    for inline in content.drain(..) {
        match inline {
            Inline::Run(r) => {
                let len = r.text.chars().count();
                for nr in split_run(r, pos, start, end, set, value) {
                    out.push(Inline::Run(nr));
                }
                pos += len;
            }
            Inline::Hyperlink(mut h) => {
                let mut new_runs = Vec::new();
                let mut p = pos;
                for run in h.runs.drain(..) {
                    let len = run.text.chars().count();
                    for nr in split_run(run, p, start, end, set, value) {
                        new_runs.push(nr);
                    }
                    p += len;
                }
                h.runs = new_runs;
                pos = p;
                out.push(Inline::Hyperlink(h));
            }
            Inline::Tab => {
                out.push(Inline::Tab);
                pos += 1;
            }
            Inline::Break(k) => {
                out.push(Inline::Break(k));
                pos += 1;
            }
            // zero-length, unchanged
            inline @ (Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::Field { .. }
            | Inline::TextBox { .. }
            | Inline::Raw(_)) => out.push(inline),
        }
    }
    *content = out;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(matches!(c.paras[0][1], Inline::Tab));
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
        };
        Document {
            body: vec![
                para("before"),
                Block::Table(Table {
                    grid: vec![100, 100],
                    rows: vec![
                        Row {
                            cells: vec![cell("A"), cell("B")],
                        },
                        Row {
                            cells: vec![cell("C"), cell("D")],
                        },
                    ],
                }),
                para("after"),
            ],
        }
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
    fn backspace_does_not_escape_cell() {
        let mut ed = Editor::new(table_doc());
        ed.caret = Caret::at(vec![1, 0, 1, 0], 0); // start of cell B's only paragraph
        ed.backspace(); // nothing to merge with inside the cell
        let p = resolve_para(&ed.doc.body, &[1, 0, 1, 0]).unwrap();
        assert_eq!(p.plain_text(), "B");
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 1, 0], 0));
    }
}
