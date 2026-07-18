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

/// Word/character/paragraph/block counts over the document's plain text
/// (`(words, chars, paragraphs, blocks)`). `words` splits on whitespace over
/// [`Document::plain_text`]; `chars` counts everything in that same text
/// except the block-separator newlines `plain_text` inserts, so it's a visible
/// character count rather than a byte count. `paragraphs` counts only
/// paragraph-kind top-level blocks; `blocks` is the raw body length (so it
/// also includes tables/raw blocks that `paragraphs` excludes).
pub fn stats(doc: &Document) -> (usize, usize, usize, usize) {
    let text = doc.plain_text();
    let words = text.split_whitespace().count();
    let chars = text.chars().filter(|&c| c != '\n').count();
    let paragraphs = doc
        .body
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(_)))
        .count();
    let blocks = doc.body.len();
    (words, chars, paragraphs, blocks)
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
/// into one or more paragraphs). Returns `(replaced, undo_steps)`: the number
/// of paragraphs replaced, and the number of native undo checkpoints this one
/// call pushed onto the editor's stack.
///
/// This selects `[start..=end]` (anchor at the head, caret at the true end of
/// the last, in the editor's own offset units) then pastes. When the selection
/// is non-empty, `paste` deletes it first (one checkpoint) and then inserts
/// (a second checkpoint) — **two** undo steps. But when the selection collapses
/// to nothing — the sole case being a single **empty** paragraph, where
/// `move_end()` leaves the caret at offset 0 exactly on the anchor — `paste`
/// skips the delete and checkpoints only once: **one** undo step. A caller that
/// replays undo/redo to keep a host stack in lockstep (e.g. the offxy VS Code
/// tab) must replay exactly this many steps, not a hard-coded two, or a single
/// host undo would over-unwind and silently destroy the user's prior edit.
///
/// The caller is responsible for its own post-edit bookkeeping (clearing the
/// selection, marking the document modified, etc.).
pub fn replace_range(
    ed: &mut Editor,
    start: usize,
    end: usize,
    text: &str,
) -> Result<(usize, usize), String> {
    let n = ed.doc.body.len();
    bounds(start, end, n)?;
    require_para(&ed.doc.body, start)?;
    require_para(&ed.doc.body, end)?;

    ed.anchor = None;
    ed.caret = Caret::top(end, 0);
    ed.move_end();
    ed.anchor = Some(Caret::top(start, 0));
    // A non-empty selection means `paste` will delete-then-insert (2
    // checkpoints); an empty one (single empty paragraph) means insert only (1).
    let deleted = ed.has_selection();
    ed.paste(&Clip::from_text(text));

    let undo_steps = if deleted { 2 } else { 1 };
    Ok((end - start + 1, undo_steps))
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
// Markdown block-splice verbs (undoable, via the Editor)
// ---------------------------------------------------------------------------

/// Parse `text` as Markdown into the top-level [`Block`]s a splice verb below
/// consumes — the same blocks [`crate::markdown::from_markdown`] would put in a
/// fresh document's body (headings, styled runs, links, lists, tables, …).
/// Errors `"empty markdown"` when `text` has no non-whitespace content, so a
/// caller can reject a would-be no-op splice before touching the editor at
/// all: nothing spliced, no undo entry pushed.
pub fn parse_markdown_blocks(text: &str) -> Result<Vec<Block>, String> {
    if text.trim().is_empty() {
        return Err("empty markdown".to_string());
    }
    Ok(crate::markdown::from_markdown(text).body)
}

/// Overwrite `ed.doc.body[start..start + blocks.len()]` in place with `blocks`.
/// Used right after a placeholder [`Editor::paste`] has already opened up
/// exactly that many paragraph slots (and taken the call's one checkpoint): a
/// direct assignment into `doc.body` doesn't itself checkpoint, so it rides on
/// that same undo step and can turn the placeholder paragraphs into whatever
/// `blocks` actually holds — headings, styled runs, tables — none of which
/// `Clip`/`paste` can carry (a `Clip` is inline content only, one entry per
/// paragraph, no block kind or paragraph-level styling).
fn overwrite_blocks(ed: &mut Editor, start: usize, blocks: Vec<Block>) {
    for (i, b) in blocks.into_iter().enumerate() {
        ed.doc.body[start + i] = b;
    }
}

/// Insert `blocks` before block `at` (or at the document end if
/// `at == doc.body.len()`, equivalent to [`append_blocks`]) — the block-splice
/// counterpart to [`insert`]. Pastes a placeholder clip of `blocks.len() + 1`
/// empty paragraphs at the head of block `at` (the trailing empty entry pushes
/// the original paragraph down intact, exactly as `insert`'s `"{text}\n"`
/// trick does for plain text), then [`overwrite_blocks`] turns the
/// `blocks.len()` opened slots into the real content. One [`Editor::paste`]
/// call is made, so this is **one** undo checkpoint, matching `insert`.
pub fn insert_blocks(ed: &mut Editor, at: usize, blocks: Vec<Block>) -> Result<(), String> {
    let n = ed.doc.body.len();
    if at > n {
        return Err(format!("'at' {at} out of bounds (0..={n})"));
    }
    if blocks.is_empty() {
        return Err("empty markdown".to_string());
    }
    if at == n {
        append_blocks(ed, blocks);
        return Ok(());
    }
    require_para(&ed.doc.body, at)?;
    let count = blocks.len();
    ed.anchor = None;
    ed.caret = Caret::top(at, 0);
    ed.paste(&Clip {
        paras: vec![Vec::new(); count + 1],
    });
    overwrite_blocks(ed, at, blocks);
    Ok(())
}

/// Append `blocks` after the document's last block — the block-splice
/// counterpart to [`append`]. Pastes a placeholder clip of `blocks.len() + 1`
/// empty paragraphs at the document end (the leading empty entry starts a
/// fresh run after the current last paragraph, exactly as `append`'s
/// `"\n{text}"` trick does for plain text), then [`overwrite_blocks`] turns
/// the opened slots into the real content. One [`Editor::paste`] call is made,
/// so this is **one** undo checkpoint, matching `append`. A no-op (empty
/// `blocks`) touches nothing and pushes no checkpoint.
pub fn append_blocks(ed: &mut Editor, blocks: Vec<Block>) {
    if blocks.is_empty() {
        return;
    }
    let start = ed.doc.body.len();
    let count = blocks.len();
    ed.anchor = None;
    ed.move_doc_end();
    ed.paste(&Clip {
        paras: vec![Vec::new(); count + 1],
    });
    overwrite_blocks(ed, start, blocks);
}

/// Replace paragraphs `[start..=end]` (inclusive) with `blocks` — the
/// block-splice counterpart to [`replace_range`]. Selects `[start..=end]`
/// exactly as `replace_range` does (anchor at the head, caret at the true end
/// of the last), then pastes a placeholder clip of `blocks.len()` empty
/// paragraphs over that selection before [`overwrite_blocks`] fills them in.
/// Same checkpoint accounting as `replace_range`, for the same reason (a
/// non-empty selection is a delete-then-insert): **two** undo steps when the
/// deleted range was non-empty, **one** when it collapsed to nothing (the sole
/// case being a single empty paragraph). Returns `(replaced, undo_steps)`:
/// the number of original paragraphs replaced, and the checkpoint count.
pub fn replace_range_blocks(
    ed: &mut Editor,
    start: usize,
    end: usize,
    blocks: Vec<Block>,
) -> Result<(usize, usize), String> {
    if blocks.is_empty() {
        return Err("empty markdown".to_string());
    }
    let n = ed.doc.body.len();
    bounds(start, end, n)?;
    require_para(&ed.doc.body, start)?;
    require_para(&ed.doc.body, end)?;

    ed.anchor = None;
    ed.caret = Caret::top(end, 0);
    ed.move_end();
    ed.anchor = Some(Caret::top(start, 0));
    let deleted = ed.has_selection();
    ed.paste(&Clip {
        paras: vec![Vec::new(); blocks.len()],
    });
    overwrite_blocks(ed, start, blocks);

    let undo_steps = if deleted { 2 } else { 1 };
    Ok((end - start + 1, undo_steps))
}

/// Replace every occurrence of `query` with `text` across the whole document
/// (all paragraphs at any nesting depth, including table cells; case
/// sensitivity per `case_sensitive`). Returns `(replaced, undo_steps)`: the
/// number of matches replaced, and the number of native undo checkpoints this
/// call pushed onto the editor's stack.
///
/// **Empirical finding** (read from [`Editor::replace_all`]'s implementation
/// and pinned by this module's tests): it calls `checkpoint` exactly **once**,
/// before the match-rewriting loop — not once per match — so a single call
/// always produces **one** undo checkpoint total, regardless of whether it
/// rewrites one match or a hundred. When there are no matches at all,
/// `Editor::replace_all` returns early *before* checkpointing, so nothing is
/// pushed onto the undo stack and `undo_steps` is `0` — a would-be no-op call
/// must not report a phantom undo step. So `undo_steps` is always `1` when
/// `replaced > 0`, and `0` when `replaced == 0`. A caller replaying undo to
/// keep a host stack in lockstep (e.g. the offxy VS Code tab) must replay
/// exactly this many undos, not one per replaced match.
pub fn replace_all(
    ed: &mut Editor,
    query: &str,
    text: &str,
    case_sensitive: bool,
) -> (usize, usize) {
    let replaced = ed.replace_all(query, text, case_sensitive);
    let undo_steps = if replaced > 0 { 1 } else { 0 };
    (replaced, undo_steps)
}

/// Undo the last edit, if any. Returns whether anything was undone; on an
/// empty undo stack (a fresh document, or one already unwound to its start)
/// this returns `false` and leaves the document untouched.
pub fn undo(ed: &mut Editor) -> bool {
    ed.undo()
}

/// Redo the last undone edit, if any. Returns whether anything was redone; on
/// an empty redo stack this returns `false` and leaves the document untouched.
pub fn redo(ed: &mut Editor) -> bool {
    ed.redo()
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
        let (replaced, steps) = replace_range(&mut ed, 1, 2, "X\nY").unwrap();
        assert_eq!(replaced, 2);
        // A non-empty range is a delete-then-insert: two native undo steps.
        assert_eq!(steps, 2);
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
    fn replace_range_of_empty_paragraph_is_one_undo_step() {
        // Replacing a single EMPTY paragraph collapses the selection to
        // nothing, so `paste` inserts without a preceding delete — ONE
        // checkpoint, not two. A host replaying `steps` undos must restore the
        // prior document in exactly that many; replaying two would over-unwind
        // and destroy the edit before it (regression guard for the offxy VS
        // Code tab's undo-lockstep desync).
        let mut ed = Editor::new(doc_with(&["keep", "", "tail"]));
        let (replaced, steps) = replace_range(&mut ed, 1, 1, "filled").unwrap();
        assert_eq!(replaced, 1);
        assert_eq!(steps, 1, "empty-paragraph replace is a single checkpoint");
        assert_eq!(paras(&ed.doc), vec!["keep", "filled", "tail"]);
        // Exactly `steps` (== 1) undos restores the prior document; a second
        // undo here would be a separate action, so one is both necessary and
        // sufficient.
        assert!(ed.undo());
        assert_eq!(paras(&ed.doc), vec!["keep", "", "tail"]);
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
    fn markdown_insert_splices_formatted_blocks_with_one_checkpoint() {
        let mut ed = Editor::new(doc_with(&["existing"]));
        let blocks = parse_markdown_blocks("# Title\n\nbody with **bold**").unwrap();
        insert_blocks(&mut ed, 0, blocks).unwrap();
        assert_eq!(ed.doc.body.len(), 3);
        // The heading landed as a styled paragraph — assert via the model
        // (heading level / plain text), not just its rendered text.
        match &ed.doc.body[0] {
            Block::Paragraph(p) => {
                assert_eq!(p.props.heading_level, Some(1));
                assert_eq!(p.plain_text(), "Title");
            }
            other => panic!("expected a heading paragraph, got {other:?}"),
        }
        // The body paragraph carries a genuinely bold run, not just text
        // that happens to contain asterisks.
        match &ed.doc.body[1] {
            Block::Paragraph(p) => {
                assert!(
                    p.content
                        .iter()
                        .any(|i| matches!(i, Inline::Run(r) if r.props.bold && r.text == "bold")),
                    "{:?}",
                    p.content
                );
            }
            other => panic!("expected a body paragraph, got {other:?}"),
        }
        // The original paragraph is untouched and pushed to the end.
        assert_eq!(ed.doc.body[2].plain_text(), "existing");

        // One undo removes the whole splice.
        assert!(ed.undo());
        assert_eq!(ed.doc.body.len(), 1);
        assert_eq!(paras(&ed.doc), vec!["existing"]);
        // Nothing else was pushed onto the stack by this call.
        assert!(!ed.undo());
    }

    #[test]
    fn markdown_append_is_a_single_undo_step() {
        let mut ed = Editor::new(doc_with(&["existing"]));
        let blocks = parse_markdown_blocks("## Heading").unwrap();
        append_blocks(&mut ed, blocks);
        assert_eq!(paras(&ed.doc), vec!["existing", "Heading"]);
        match &ed.doc.body[1] {
            Block::Paragraph(p) => assert_eq!(p.props.heading_level, Some(2)),
            other => panic!("expected a heading paragraph, got {other:?}"),
        }
        assert!(ed.undo());
        assert_eq!(paras(&ed.doc), vec!["existing"]);
        assert!(!ed.undo());
    }

    #[test]
    fn markdown_replace_range_matches_text_variant_step_counts() {
        // Non-empty range (two populated paragraphs) → 2 undo steps, mirroring
        // `replace_range_is_single_paste`.
        let mut ed = Editor::new(doc_with(&["A", "B", "C", "D"]));
        let blocks = parse_markdown_blocks("# X\n\nY").unwrap();
        let (replaced, steps) = replace_range_blocks(&mut ed, 1, 2, blocks).unwrap();
        assert_eq!(replaced, 2);
        assert_eq!(steps, 2);
        assert_eq!(paras(&ed.doc), vec!["A", "X", "Y", "D"]);
        match &ed.doc.body[1] {
            Block::Paragraph(p) => assert_eq!(p.props.heading_level, Some(1)),
            other => panic!("expected a heading paragraph, got {other:?}"),
        }
        assert!(ed.undo());
        assert!(ed.undo());
        assert_eq!(paras(&ed.doc), vec!["A", "B", "C", "D"]);
        assert!(!ed.undo());

        // A single EMPTY paragraph range → 1 undo step, mirroring
        // `replace_range_of_empty_paragraph_is_one_undo_step`.
        let mut ed2 = Editor::new(doc_with(&["keep", "", "tail"]));
        let blocks2 = parse_markdown_blocks("filled").unwrap();
        let (replaced2, steps2) = replace_range_blocks(&mut ed2, 1, 1, blocks2).unwrap();
        assert_eq!(replaced2, 1);
        assert_eq!(steps2, 1, "empty-paragraph replace is a single checkpoint");
        assert_eq!(paras(&ed2.doc), vec!["keep", "filled", "tail"]);
        assert!(ed2.undo());
        assert_eq!(paras(&ed2.doc), vec!["keep", "", "tail"]);
        assert!(!ed2.undo());
    }

    #[test]
    fn empty_markdown_errors_and_touches_nothing() {
        assert_eq!(
            parse_markdown_blocks("   \n").unwrap_err(),
            "empty markdown"
        );
        // Nothing was ever spliced or checkpointed: an editor left untouched.
        let mut ed = Editor::new(doc_with(&["A"]));
        assert!(!ed.undo());
    }

    #[test]
    fn stats_counts_words_chars_paragraphs_and_blocks() {
        let doc = doc_with(&["one two", "three"]);
        let (words, chars, paragraphs, blocks) = stats(&doc);
        assert_eq!(words, 3);
        // "one two" (7) + "three" (5) = 12 visible chars, newlines excluded.
        assert_eq!(chars, 12);
        assert_eq!(paragraphs, 2);
        assert_eq!(blocks, 2);
    }

    #[test]
    fn find_locates_across_blocks() {
        let doc = doc_with(&["hello world", "goodbye world"]);
        let matches = find(&doc, "world", false);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].path, vec![0]);
        assert_eq!(matches[1].path, vec![1]);
    }

    #[test]
    fn replace_all_reports_count_and_a_single_undo_checkpoint() {
        let mut ed = Editor::new(doc_with(&["a foo b foo c", "foo"]));
        let (replaced, steps) = replace_all(&mut ed, "foo", "BAR", false);
        assert_eq!(replaced, 3);
        // Empirical finding under test: Editor::replace_all checkpoints ONCE
        // total, not once per match, so exactly one undo (not three) must
        // restore every rewritten paragraph.
        assert_eq!(
            steps, 1,
            "replace_all checkpoints once regardless of match count"
        );
        assert_eq!(paras(&ed.doc), vec!["a BAR b BAR c", "BAR"]);
        for _ in 0..steps {
            assert!(ed.undo());
        }
        assert_eq!(
            paras(&ed.doc),
            vec!["a foo b foo c", "foo"],
            "exactly `steps` undos must restore the original text"
        );
        // No further undo is available — the whole edit was one checkpoint.
        assert!(!ed.undo());
    }

    #[test]
    fn replace_all_no_matches_pushes_no_undo_step() {
        let mut ed = Editor::new(doc_with(&["hello world"]));
        let (replaced, steps) = replace_all(&mut ed, "xyz", "BAR", false);
        assert_eq!(replaced, 0);
        assert_eq!(steps, 0, "a no-op call must not report a phantom undo step");
        assert!(
            !ed.undo(),
            "no checkpoint was pushed, so there is nothing to undo"
        );
    }

    #[test]
    fn replace_all_is_case_insensitive_when_requested() {
        let mut ed = Editor::new(doc_with(&["Foo and foo"]));
        let (replaced, _) = replace_all(&mut ed, "foo", "X", false);
        assert_eq!(replaced, 2);
        assert_eq!(paras(&ed.doc), vec!["X and X"]);
    }

    #[test]
    fn undo_redo_report_whether_anything_happened() {
        let mut ed = Editor::new(doc_with(&["A"]));
        // Fresh document: nothing to undo or redo.
        assert!(!undo(&mut ed));
        assert!(!redo(&mut ed));

        replace_all(&mut ed, "A", "B", false);
        assert_eq!(paras(&ed.doc), vec!["B"]);
        assert!(undo(&mut ed));
        assert_eq!(paras(&ed.doc), vec!["A"]);
        // The stack is empty again.
        assert!(!undo(&mut ed));

        assert!(redo(&mut ed));
        assert_eq!(paras(&ed.doc), vec!["B"]);
        // The redo stack is empty again.
        assert!(!redo(&mut ed));
    }
}
