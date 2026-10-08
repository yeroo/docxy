//! Content controls showing their placeholder (`w:showingPlcHdr`), at every
//! level a caret can be inside one: inline, block, and cell-level (`w:sdt`
//! around `w:tc`, the cover page's Abstract, #1102). A click inside one
//! selects all of its placeholder, so typing replaces it, as in Word; text
//! going into a cell-level or block-level one clears its flag (an inline one
//! is cleared where the text is inserted, see `clear_showing_placeholder`).

use super::{Caret, Editor, para_text_len, resolve_para};
use crate::model::{Block, Inline, Row, VMerge};

const FLAG: &str = "<w:showingPlcHdr";

/// Whether a content control's opening boundary says it shows its
/// placeholder: a `w:showingPlcHdr` that is on (`ST_OnOff`: `w:val` absent,
/// or anything but `false`, `0` and `off`).
fn shows_placeholder(open: &str) -> bool {
    crate::sect::has_flag(open, "w:showingPlcHdr")
}

/// One step down a caret path: into a table cell, or into a text box.
enum Step {
    Cell {
        table: usize,
        row: usize,
        cell: usize,
    },
    TextBox {
        para: usize,
        inline: usize,
    },
}

impl Editor {
    /// The placeholder a click at `caret` selects: the start and end of the
    /// innermost content control around it still showing its placeholder
    /// whose content the editor can select as one range. A cell-level
    /// control's range runs from its first cell's start to its last cell's
    /// end. A control the editor cannot select exactly is skipped, and an
    /// enclosing one answers instead: one whose content starts or ends in a
    /// nested table (a selection across containers), or one over several
    /// cells where a vertical merge would widen the cell selection. `None`
    /// when no selectable control holds the caret.
    pub(crate) fn placeholder_range_at(&self, caret: &Caret) -> Option<(Caret, Caret)> {
        let path = &caret.path;
        let para = resolve_para(&self.doc.body, path)?;
        // Outermost first: the last found is the innermost.
        let mut found = None;
        let mut blocks: &[Block] = &self.doc.body;
        let mut at = 0;
        loop {
            let prefix = &path[..at];
            let idx = *path.get(at)?;
            if let Some(range) = block_placeholder(blocks, idx, prefix) {
                found = Some(range);
            }
            match step(blocks, path, at) {
                Some(Step::Cell { table, row, cell }) => {
                    let Block::Table(t) = &blocks[table] else {
                        return None;
                    };
                    let mut row_path = path[..at].to_vec();
                    row_path.extend([table, row]);
                    if let Some(range) = cell_placeholder(&t.rows[row], cell, &row_path) {
                        found = Some(range);
                    }
                    blocks = &t.rows[row].cells[cell].blocks;
                    at += 3;
                }
                Some(Step::TextBox { para, inline }) => {
                    let Block::Paragraph(p) = &blocks[para] else {
                        return None;
                    };
                    let Inline::TextBox { blocks: inner, .. } = &p.content[inline] else {
                        return None;
                    };
                    blocks = inner;
                    at += 2;
                }
                None => break,
            }
        }
        if let Some((start, end)) = inline_placeholder(&para.content, caret.offset) {
            found = Some((Caret::at(path.clone(), start), Caret::at(path.clone(), end)));
        }
        found
    }

    /// Select the placeholder around the caret ([`Self::placeholder_range_at`]),
    /// as a click there does. Whether there was one.
    pub fn select_placeholder_at_caret(&mut self) -> bool {
        let Some((start, end)) = self.placeholder_range_at(&self.caret) else {
            return false;
        };
        self.anchor = Some(start);
        self.caret = end;
        true
    }

    /// Text is going into the paragraph at `path`: every cell-level and
    /// block-level content control around it stops showing its placeholder,
    /// as Word clears `w:showingPlcHdr` on typing (else Word would take the
    /// text for the placeholder). A control over several cells is cleared on
    /// its first cell, where it opens.
    pub(super) fn clear_placeholders_around(&mut self, path: &[usize]) {
        let mut blocks: &mut Vec<Block> = &mut self.doc.body;
        let mut at = 0;
        while let Some(&idx) = path.get(at) {
            clear_enclosing_blocks(blocks, idx);
            match step(blocks, path, at) {
                Some(Step::Cell { table, row, cell }) => {
                    let Some(Block::Table(t)) = blocks.get_mut(table) else {
                        return;
                    };
                    let row = &mut t.rows[row];
                    for (c, k) in row.cell_controls(cell) {
                        let raw = &mut row.cells[c].sdt_open[k];
                        if raw.contains(FLAG) {
                            *raw = crate::sect::remove_element(raw, "w:showingPlcHdr");
                        }
                    }
                    blocks = &mut row.cells[cell].blocks;
                    at += 3;
                }
                Some(Step::TextBox { para, inline }) => {
                    let Some(Block::Paragraph(p)) = blocks.get_mut(para) else {
                        return;
                    };
                    let Some(Inline::TextBox { blocks: inner, .. }) = p.content.get_mut(inline)
                    else {
                        return;
                    };
                    blocks = inner;
                    at += 2;
                }
                None => return,
            }
        }
    }
}

impl Editor {
    /// Text `[start, end)` of the paragraph at `path` is being changed
    /// (deleted, replaced, recased): each inline content control whose
    /// content it touches stops showing its placeholder. An empty range
    /// touches the character at `start`.
    pub(super) fn clear_inline_placeholders(&mut self, path: &[usize], start: usize, end: usize) {
        let Some(p) = super::para_mut(&mut self.doc.body, path) else {
            return;
        };
        let end = end.max(start + 1);
        for i in inline_controls_over(&p.content, start, end) {
            if let Inline::Raw(raw) = &mut p.content[i] {
                if raw.contains(FLAG) {
                    *raw = crate::sect::remove_element(raw, "w:showingPlcHdr");
                }
            }
        }
    }
}

impl Editor {
    /// Text is going in at offset `at` of the paragraph at `path`: each inline
    /// content control it lands strictly inside stops showing its
    /// placeholder. At a control's very edge it keeps the flag; an insertion
    /// into an empty control clears it where it is inserted.
    pub(super) fn clear_inline_placeholders_inside(&mut self, path: &[usize], at: usize) {
        let Some(p) = super::para_mut(&mut self.doc.body, path) else {
            return;
        };
        for i in inline_controls_inside(&p.content, at) {
            if let Inline::Raw(raw) = &mut p.content[i] {
                if raw.contains(FLAG) {
                    *raw = crate::sect::remove_element(raw, "w:showingPlcHdr");
                }
            }
        }
    }
}

/// The inline content controls whose content holds offset `at` strictly
/// inside it, by the index of their opening boundary.
fn inline_controls_inside(content: &[Inline], at: usize) -> Vec<usize> {
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut offset = 0;
    for (i, inline) in content.iter().enumerate() {
        match inline {
            Inline::Raw(raw) if crate::hf::is_sdt_open(raw) => open.push((i, offset)),
            Inline::Raw(raw) if crate::hf::is_sdt_close(raw) => {
                if let Some((o, from)) = open.pop() {
                    if from < at && at < offset {
                        out.push(o);
                    }
                }
            }
            other => offset += super::inline_len(other),
        }
    }
    out
}

/// The inline content controls whose content overlaps `[start, end)`, by
/// the index of their opening boundary.
fn inline_controls_over(content: &[Inline], start: usize, end: usize) -> Vec<usize> {
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut offset = 0;
    for (i, inline) in content.iter().enumerate() {
        match inline {
            Inline::Raw(raw) if crate::hf::is_sdt_open(raw) => open.push((i, offset)),
            Inline::Raw(raw) if crate::hf::is_sdt_close(raw) => {
                if let Some((o, from)) = open.pop() {
                    if from < end && start < offset {
                        out.push(o);
                    }
                }
            }
            other => offset += super::inline_len(other),
        }
    }
    out.extend(
        open.into_iter()
            .filter(|&(_, from)| from < end && start < offset)
            .map(|(o, _)| o),
    );
    out
}

/// Where `path` goes next from the block at `path[at]` in `blocks`, if it
/// goes deeper than that block.
fn step(blocks: &[Block], path: &[usize], at: usize) -> Option<Step> {
    let idx = *path.get(at)?;
    match blocks.get(idx)? {
        Block::Table(t) => {
            let (row, cell) = (*path.get(at + 1)?, *path.get(at + 2)?);
            t.rows.get(row)?.cells.get(cell)?;
            Some(Step::Cell {
                table: idx,
                row,
                cell,
            })
        }
        Block::Paragraph(p) => {
            let inline = *path.get(at + 1)?;
            matches!(p.content.get(inline)?, Inline::TextBox { .. })
                .then_some(Step::TextBox { para: idx, inline })
        }
        _ => None,
    }
}

/// The cell a control opened at `(c, k)` closes on: the last cell of the row
/// when an edit left it open.
fn closing_cell(row: &Row, c: usize, k: usize) -> usize {
    // Controls still open after the opening cell's opens, this one included.
    let mut depth = row.cells[c].sdt_open.len() - k;
    for (i, here) in row.cells.iter().enumerate().skip(c) {
        if i > c {
            depth += here.sdt_open.len();
        }
        if here.sdt_close.len() >= depth {
            return i;
        }
        depth -= here.sdt_close.len();
    }
    row.cells.len() - 1
}

/// The innermost cell-level control around cell `cell` of the row at
/// `row_path` showing its placeholder that the editor can select (see
/// [`cell_control_range`]): its first cell's start to its last cell's end.
fn cell_placeholder(row: &Row, cell: usize, row_path: &[usize]) -> Option<(Caret, Caret)> {
    row.cell_controls(cell)
        .into_iter()
        .rev()
        .filter(|&(c, k)| shows_placeholder(&row.cells[c].sdt_open[k]))
        .find_map(|(c, k)| cell_control_range(row, c, k, row_path))
}

/// The range of the cell-level control opened at `(c, k)`, when the editor
/// can select exactly its content.
fn cell_control_range(row: &Row, c: usize, k: usize, row_path: &[usize]) -> Option<(Caret, Caret)> {
    let last = closing_cell(row, c, k);
    // Over several cells the selection is a cell range, which grows over
    // vertical merges: then it would not be exactly the control's cells.
    if last > c
        && row.cells[c..=last]
            .iter()
            .any(|x| x.v_merge != VMerge::None)
    {
        return None;
    }
    let mut first_prefix = row_path.to_vec();
    first_prefix.push(c);
    let mut last_prefix = row_path.to_vec();
    last_prefix.push(last);
    let start = edge_paragraph(&row.cells[c].blocks, &mut first_prefix, false)?;
    let end = edge_paragraph(&row.cells[last].blocks, &mut last_prefix, true)?;
    // Each end in its own cell's paragraphs, not a nested table's: a selection
    // across containers is not one the editor can type over.
    let top = row_path.len() + 2;
    if start.0.len() != top || end.0.len() != top {
        return None;
    }
    Some((Caret::at(start.0, 0), Caret::at(end.0, end.1)))
}

/// The innermost block-level control around `blocks[idx]` showing its
/// placeholder that the editor can select: its first paragraph's start to
/// its last paragraph's end.
fn block_placeholder(blocks: &[Block], idx: usize, prefix: &[usize]) -> Option<(Caret, Caret)> {
    enclosing_block_opens(blocks, idx)
        .into_iter()
        .rev()
        .filter(|&i| matches!(&blocks[i], Block::Raw(r) if shows_placeholder(r)))
        .find_map(|open| {
            let close = crate::hf::matching_close(blocks, open).unwrap_or(blocks.len());
            let mut path = prefix.to_vec();
            let start = edge_paragraph_in(blocks, open + 1..close, &mut path, false)?;
            let end = edge_paragraph_in(blocks, open + 1..close, &mut path, true)?;
            // Both ends in this container, not in a nested table: a selection
            // across containers is not one the editor can type over.
            let top = prefix.len() + 1;
            (start.0.len() == top && end.0.len() == top)
                .then(|| (Caret::at(start.0, 0), Caret::at(end.0, end.1)))
        })
}

/// Clear the flag of every block-level control around `blocks[idx]`.
fn clear_enclosing_blocks(blocks: &mut [Block], idx: usize) {
    for i in enclosing_block_opens(blocks, idx) {
        if let Block::Raw(raw) = &mut blocks[i] {
            if raw.contains(FLAG) {
                *raw = crate::sect::remove_element(raw, "w:showingPlcHdr");
            }
        }
    }
}

/// The block-level control opens still open at `blocks[idx]`, outermost first.
fn enclosing_block_opens(blocks: &[Block], idx: usize) -> Vec<usize> {
    let mut open = Vec::new();
    for (i, block) in blocks.iter().enumerate().take(idx) {
        if let Block::Raw(raw) = block {
            if crate::hf::is_sdt_open(raw) {
                open.push(i);
            } else if crate::hf::is_sdt_close(raw) {
                open.pop();
            }
        }
    }
    open
}

/// The innermost inline control around offset `at` showing its placeholder,
/// as the offsets its content starts and ends at.
fn inline_placeholder(content: &[Inline], at: usize) -> Option<(usize, usize)> {
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0;
    let mut found = None;
    let consider = |i: usize, start: usize, end: usize, found: &mut Option<(usize, usize)>| {
        let flagged = matches!(&content[i], Inline::Raw(r) if shows_placeholder(r));
        // Inner controls close first: the first found is the innermost.
        if flagged && found.is_none() && start <= at && at <= end {
            *found = Some((start, end));
        }
    };
    for (i, inline) in content.iter().enumerate() {
        match inline {
            Inline::Raw(raw) if crate::hf::is_sdt_open(raw) => open.push((i, offset)),
            Inline::Raw(raw) if crate::hf::is_sdt_close(raw) => {
                if let Some((o, start)) = open.pop() {
                    consider(o, start, offset, &mut found);
                }
            }
            other => offset += super::inline_len(other),
        }
    }
    while let Some((o, start)) = open.pop() {
        consider(o, start, offset, &mut found);
    }
    found
}

/// The first (or with `last`, the last) paragraph in `blocks`, not counting
/// text boxes: its path under `prefix`, and its length.
fn edge_paragraph(
    blocks: &[Block],
    prefix: &mut Vec<usize>,
    last: bool,
) -> Option<(Vec<usize>, usize)> {
    edge_paragraph_in(blocks, 0..blocks.len(), prefix, last)
}

fn edge_paragraph_in(
    blocks: &[Block],
    range: std::ops::Range<usize>,
    prefix: &mut Vec<usize>,
    last: bool,
) -> Option<(Vec<usize>, usize)> {
    let order: Box<dyn Iterator<Item = usize>> = if last {
        Box::new(range.rev())
    } else {
        Box::new(range)
    };
    for i in order {
        prefix.push(i);
        let found = match &blocks[i] {
            Block::Paragraph(p) => Some((prefix.clone(), para_text_len(p))),
            Block::Table(t) => {
                let cells: Vec<(usize, usize)> = t
                    .rows
                    .iter()
                    .enumerate()
                    .flat_map(|(r, row)| (0..row.cells.len()).map(move |c| (r, c)))
                    .collect();
                let mut hit = None;
                let ordered: Box<dyn Iterator<Item = &(usize, usize)>> = if last {
                    Box::new(cells.iter().rev())
                } else {
                    Box::new(cells.iter())
                };
                for &(r, c) in ordered {
                    prefix.extend([r, c]);
                    hit = edge_paragraph(&t.rows[r].cells[c].blocks, prefix, last);
                    prefix.truncate(prefix.len() - 2);
                    if hit.is_some() {
                        break;
                    }
                }
                hit
            }
            _ => None,
        };
        prefix.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}
