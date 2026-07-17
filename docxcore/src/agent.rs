//! The document-verb core shared by every control surface (the `docxy` TUI's
//! [`crate`][crate]-external `control.rs`, and later `docxwasm`'s agent
//! bindings): pure `Document`/[`Editor`] logic for the handful of verbs an
//! external agent uses to read and edit a live document.
//!
//! This module is deliberately **host-agnostic**: it takes and returns plain
//! Rust types only (no JSON, no host `App`/`Json` types), so any host can wrap
//! it in whatever wire format it needs. Hosts still own:
//! - argument parsing (turning e.g. a JSON object into `start`/`end`/`text`),
//! - `finish_edit`-style bookkeeping (clearing the selection, marking the
//!   document modified, requesting a repaint) after a mutating verb,
//! - save/reload/open and anything else that touches the filesystem.
//!
//! Addressing is by **top-level block index** (position in `doc.body`): a
//! paragraph or table. [`read`] / [`outline`] report each block's `kind`, so a
//! caller knows which indices are paragraphs (the ones the edit verbs accept).

use crate::editor::{Caret, Clip, Editor, Match};
use crate::model::{Block, Document};

/// One block's read-only summary, as reported by [`read`].
pub struct BlockInfo {
    pub index: usize,
    pub kind: &'static str,
    pub text: String,
    pub heading: Option<u8>,
}

/// One heading, as reported by [`outline`].
pub struct Heading {
    pub index: usize,
    pub level: u8,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Read-only verbs
// ---------------------------------------------------------------------------

/// All headings in document order (top-level paragraphs with a heading level).
pub fn outline(doc: &Document) -> Vec<Heading> {
    let mut items = Vec::new();
    for (i, b) in doc.body.iter().enumerate() {
        if let Block::Paragraph(p) = b {
            if let Some(level) = p.props.heading_level {
                items.push(Heading {
                    index: i,
                    level,
                    text: p.plain_text(),
                });
            }
        }
    }
    items
}

/// The blocks in `[start..=end]`, inclusive. Validates the range against the
/// document's block count.
pub fn read(doc: &Document, start: usize, end: usize) -> Result<Vec<BlockInfo>, String> {
    let n = doc.body.len();
    bounds(start, end, n)?;
    let mut out = Vec::new();
    for i in start..=end {
        let b = &doc.body[i];
        let heading = match b {
            Block::Paragraph(p) => p.props.heading_level,
            _ => None,
        };
        out.push(BlockInfo {
            index: i,
            kind: block_kind(b),
            text: b.plain_text(),
            heading,
        });
    }
    Ok(out)
}

/// All matches of `query` across the whole document (paragraphs at any
/// nesting depth, including inside table cells). This is the search core
/// behind [`Editor::find_all`], exposed here as a pure `Document` function so
/// a host that only has a bare document (no live `Editor`) can still search
/// it, and so `docxy`'s `doc.find` control verb can build its JSON straight
/// off these plain [`Match`] values.
pub fn find(doc: &Document, query: &str, case_sensitive: bool) -> Vec<Match> {
    crate::editor::find_all_in_body(&doc.body, query, case_sensitive)
}

// ---------------------------------------------------------------------------
// Mutating verbs (undoable, via the Editor)
// ---------------------------------------------------------------------------

/// Replace paragraphs `[start..=end]` (inclusive) with `text` (newline-split
/// into one or more paragraphs). Returns the number of paragraphs replaced.
///
/// This selects `[start..=end]` (anchor at the head, caret at the true end of
/// the last, in the editor's own offset units) then pastes — `paste` deletes
/// the selection first, so this is one undoable replace. The caller is
/// responsible for its own post-edit bookkeeping (clearing the selection,
/// marking the document modified, etc.).
pub fn replace_range(
    ed: &mut Editor,
    start: usize,
    end: usize,
    text: &str,
) -> Result<usize, String> {
    let n = ed.doc.body.len();
    bounds(start, end, n)?;
    require_para(&ed.doc.body, start)?;
    require_para(&ed.doc.body, end)?;

    ed.anchor = None;
    ed.caret = Caret::top(end, 0);
    ed.move_end();
    ed.anchor = Some(Caret::top(start, 0));
    ed.paste(&Clip::from_text(text));

    Ok(end - start + 1)
}

/// Insert `text` (newline-split into one or more paragraphs) before block
/// `at`, or at the document end if `at == doc.body.len()` (equivalent to
/// [`append`]).
pub fn insert(ed: &mut Editor, at: usize, text: &str) -> Result<(), String> {
    let n = ed.doc.body.len();
    if at > n {
        return Err(format!("'at' {at} out of bounds (0..={n})"));
    }
    if at == n {
        append(ed, text);
        return Ok(());
    }
    require_para(&ed.doc.body, at)?;
    // Paste `text\n` at the head of block `at`: the trailing newline pushes the
    // original paragraph down, so `text` lands as its own paragraph(s) before it.
    ed.anchor = None;
    ed.caret = Caret::top(at, 0);
    ed.paste(&Clip::from_text(&format!("{text}\n")));
    Ok(())
}

/// Append `text` (newline-split into one or more paragraphs) after the
/// document's last block.
pub fn append(ed: &mut Editor, text: &str) {
    // Paste `\ntext` at the document end: the leading newline starts a fresh
    // paragraph, so `text` lands as new paragraph(s) after the current last one.
    ed.anchor = None;
    ed.move_doc_end();
    ed.paste(&Clip::from_text(&format!("\n{text}")));
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub fn block_kind(b: &Block) -> &'static str {
    match b {
        Block::Paragraph(_) => "paragraph",
        Block::Table(_) => "table",
        Block::Raw(_) => "raw",
    }
}

pub fn require_para(body: &[Block], i: usize) -> Result<(), String> {
    match body.get(i) {
        Some(Block::Paragraph(_)) => Ok(()),
        Some(_) => Err(format!("block {i} is not a paragraph; edit verbs need one")),
        None => Err(format!("block {i} out of bounds")),
    }
}

pub fn bounds(start: usize, end: usize, n: usize) -> Result<(), String> {
    if n == 0 {
        return Err("document is empty".into());
    }
    if start >= n || end >= n {
        return Err(format!("range {start}..{end} out of bounds (0..{})", n - 1));
    }
    if start > end {
        return Err(format!("start {start} is after end {end}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, Inline, ParProps, Paragraph, Run, RunProps};

    /// A document of simple text paragraphs (same fixture shape as
    /// `docxy/src/control.rs`'s `doc_with`).
    fn doc_with(paras: &[&str]) -> Document {
        let body = paras
            .iter()
            .map(|t| {
                Block::Paragraph(Paragraph {
                    props: ParProps::default(),
                    content: vec![Inline::Run(Run {
                        text: t.to_string(),
                        props: RunProps::default(),
                    })],
                })
            })
            .collect();
        Document { body }
    }

    fn paras(doc: &Document) -> Vec<String> {
        doc.body.iter().map(|b| b.plain_text()).collect()
    }

    #[test]
    fn outline_reports_heading_levels() {
        let mut doc = doc_with(&["Title", "body", "Section", "more"]);
        for (i, lvl) in [(0usize, 1u8), (2, 2)] {
            if let Block::Paragraph(p) = &mut doc.body[i] {
                p.props.heading_level = Some(lvl);
            }
        }
        let hs = outline(&doc);
        assert_eq!(hs.len(), 2);
        assert_eq!(hs[0].index, 0);
        assert_eq!(hs[0].level, 1);
        assert_eq!(hs[0].text, "Title");
        assert_eq!(hs[1].index, 2);
        assert_eq!(hs[1].level, 2);
        assert_eq!(hs[1].text, "Section");
    }

    #[test]
    fn replace_range_is_single_paste() {
        let mut ed = Editor::new(doc_with(&["A", "B", "C", "D"]));
        let replaced = replace_range(&mut ed, 1, 2, "X\nY").unwrap();
        assert_eq!(replaced, 2);
        assert_eq!(paras(&ed.doc), vec!["A", "X", "Y", "D"]);
        // `paste` consumes the selection it started from.
        assert!(ed.anchor.is_none());
        // A replace is a delete-then-insert (one paste over a selection), so it
        // unwinds in exactly two native undo steps back to the original.
        assert!(ed.undo());
        assert!(ed.undo());
        assert_eq!(paras(&ed.doc), vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn insert_at_end_equals_append() {
        let mut a = Editor::new(doc_with(&["A", "B"]));
        insert(&mut a, 2, "C\nD").unwrap();

        let mut b = Editor::new(doc_with(&["A", "B"]));
        append(&mut b, "C\nD");

        assert_eq!(paras(&a.doc), vec!["A", "B", "C", "D"]);
        assert_eq!(paras(&a.doc), paras(&b.doc));
    }

    #[test]
    fn find_locates_across_blocks() {
        let doc = doc_with(&["hello world", "goodbye world"]);
        let matches = find(&doc, "world", false);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].path, vec![0]);
        assert_eq!(matches[1].path, vec![1]);
    }
}
