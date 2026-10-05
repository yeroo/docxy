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
//! Addressing is by **top-level content-block index** (position before the
//! trailing body-level section-properties sentinel): a paragraph or table.
//! [`read`] / [`outline`] report each block's `kind`, so a caller knows which
//! indices are paragraphs (the ones the edit verbs accept).

use crate::editor::{Caret, Clip, Editor, Match};
use crate::model::{Align, Block, Document, Inline, ParProps, Paragraph, PropertyChange, RunProps};

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
    let n = doc.content_block_count();
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
/// paragraph-kind top-level blocks; `blocks` counts public content blocks (so
/// it includes tables/raw blocks but excludes the trailing section sentinel).
pub fn stats(doc: &Document) -> (usize, usize, usize, usize) {
    let text = doc.plain_text();
    let words = text.split_whitespace().count();
    let chars = text.chars().filter(|&c| c != '\n').count();
    let paragraphs = doc
        .body
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(_)))
        .count();
    let blocks = doc.content_block_count();
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
    let n = ed.doc.content_block_count();
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
/// `at`, or at the document end if `at == doc.content_block_count()` (equivalent to
/// [`append`]).
pub fn insert(ed: &mut Editor, at: usize, text: &str) -> Result<(), String> {
    let n = ed.doc.content_block_count();
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
/// document's last block. When that block is a paragraph closing a section,
/// the section mark stays on it and `text` lands in the final section, as
/// with [`append_blocks`].
pub fn append(ed: &mut Editor, text: &str) {
    // Paste `\ntext` at the document end: the leading newline starts a fresh
    // paragraph, so `text` lands as new paragraph(s) after the current last one.
    let start = ed.doc.content_block_count();
    ed.anchor = None;
    ed.move_doc_end();
    ed.paste(&Clip::from_text(&format!("\n{text}")));
    restore_last_section_mark(ed, start);
}

/// After an append pasted at the end of the last of the `start` content
/// blocks, put back the section mark the paste moved onto the new last
/// paragraph (#748): the old last paragraph still closes its section, and the
/// appended content lands after it, in the final section.
fn restore_last_section_mark(ed: &mut Editor, start: usize) {
    let moved_to = ed.doc.content_block_count().saturating_sub(1);
    if start == 0
        || moved_to < start
        || !matches!(ed.doc.body.get(start - 1), Some(Block::Paragraph(_)))
    {
        return;
    }
    let Some(Block::Paragraph(p)) = ed.doc.body.get_mut(moved_to) else {
        return;
    };
    if p.props.section_break.is_none() && p.props.section_property_change.is_none() {
        return;
    }
    let brk = p.props.section_break.take();
    let change = p.props.section_property_change.take();
    if let Some(Block::Paragraph(last)) = ed.doc.body.get_mut(start - 1) {
        last.props.section_break = brk;
        last.props.section_property_change = change;
    }
}

// ---------------------------------------------------------------------------
// Markdown block-splice verbs (undoable, via the Editor)
// ---------------------------------------------------------------------------

/// Parse `text` as Markdown into the top-level [`Block`]s a splice verb below
/// consumes — the same blocks [`crate::markdown::from_markdown`] would put in a
/// fresh document's body (headings, styled runs, links, lists, tables, …).
/// Errors `"empty markdown"` when `text` has no non-whitespace content, so a
/// caller can reject a would-be no-op splice before touching the editor at
/// all: nothing spliced, no undo entry pushed. A field's stand-in
/// ([`crate::editor::FIELD_CHAR`], from text an agent read back) is dropped:
/// it is not a character to write into the document.
pub fn parse_markdown_blocks(text: &str) -> Result<Vec<Block>, String> {
    let text = crate::editor::without_field_chars(text);
    if text.trim().is_empty() {
        return Err("empty markdown".to_string());
    }
    Ok(crate::markdown::from_markdown(&text).body)
}

/// The paragraph style ids (`w:pStyle` references) [`crate::markdown::from_markdown`]
/// emits: `Heading1`..`Heading6`, `Quote`, `SourceCode`. (Deliberately not
/// `Code` — that's a run-level *character* style, `w:rStyle`, applied at save
/// time from `RunProps.code`, not a `w:pStyle` any parsed [`Block`] carries;
/// see [`referenced_style_ids`]'s doc comment.)
pub const MARKDOWN_PARAGRAPH_STYLE_IDS: &[&str] = &[
    "Heading1",
    "Heading2",
    "Heading3",
    "Heading4",
    "Heading5",
    "Heading6",
    "Quote",
    "SourceCode",
];

/// Which of [`MARKDOWN_PARAGRAPH_STYLE_IDS`] the top-level paragraphs in
/// `blocks` actually reference (`w:pStyle`), in first-seen order. Markdown
/// table cells never carry one of these ids (`markdown.rs::parse_table`
/// builds cell paragraphs with `ParProps::default()`), so only top-level
/// [`Block::Paragraph`]s need checking, not a recursive scan.
///
/// Pure (no mutation, doesn't touch a `Package`): a control surface that owns
/// a `Package` should call this on the parsed blocks BEFORE splicing, then
/// pass any returned ids to [`crate::package::Package::ensure_styles`] so a
/// target `.docx` that doesn't already define e.g. `Heading1`/`Quote`/
/// `SourceCode` gets a definition before the reference lands — otherwise
/// `<w:pStyle w:val="HeadingN"/>` (etc) renders as plain Normal text in Word.
/// Every control surface (`docxy::control`, `docxwasm::bridge`) shares this
/// exact fixed id list so behavior stays identical across surfaces.
pub fn referenced_style_ids(blocks: &[Block]) -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for b in blocks {
        let Block::Paragraph(p) = b else { continue };
        let Some(style) = p.props.style_id.as_deref() else {
            continue;
        };
        if let Some(&known) = MARKDOWN_PARAGRAPH_STYLE_IDS.iter().find(|&&k| k == style) {
            if !ids.contains(&known) {
                ids.push(known);
            }
        }
    }
    ids
}

/// Whether the top-level paragraphs in `blocks` reference
/// [`crate::markdown::from_markdown`]'s bare bullet (`numId` 1) / ordered
/// (`numId` 2) list ids — `(needs_bullet, needs_decimal)`. Those bare ids
/// only mean something in a package built by `new_markdown_package` (which
/// defines exactly those two ids); splicing into an arbitrary EXISTING
/// package is dangerous taken literally, since that document may already
/// define its own `numId` 1/2 for something unrelated.
///
/// Pure (no mutation, doesn't touch a `Package`): a control surface should
/// call this on the parsed blocks BEFORE splicing, then for each kind
/// actually present call [`crate::package::Package::ensure_list`] (which
/// returns a reserved high id unlikely to collide) and rewrite the parsed
/// blocks' `numId` from the bare 1/2 onto the returned id — this function
/// only detects which kinds are present, it does not remap. Markdown table
/// cells never carry a list paragraph (`markdown.rs::parse_table` builds
/// cell paragraphs with `ParProps::default()`), so only top-level
/// [`Block::Paragraph`]s need checking, matching [`referenced_style_ids`]'s
/// non-recursive shape. Every control surface shares this exact detection so
/// behavior stays identical across surfaces.
pub fn referenced_numbering_kinds(blocks: &[Block]) -> (bool, bool) {
    let is_list =
        |b: &Block, id: i32| matches!(b, Block::Paragraph(p) if p.props.num_id == Some(id));
    let needs_bullet = blocks.iter().any(|b| is_list(b, 1));
    let needs_decimal = blocks.iter().any(|b| is_list(b, 2));
    (needs_bullet, needs_decimal)
}

/// Whether parsed Markdown blocks carry formatting in addition to their
/// structural/text payload. Control hosts use this after parsing and before
/// mutating package parts so formatting-only protection cannot be bypassed by
/// sending formatted content through a structure verb.
pub fn blocks_carry_formatting(blocks: &[Block]) -> bool {
    fn inline_carries_formatting(inline: &Inline) -> bool {
        match inline {
            Inline::Run(run) => run.props != RunProps::default(),
            Inline::Hyperlink(link) => {
                link.runs.iter().any(|run| run.props != RunProps::default())
                    || link.content.iter().any(inline_carries_formatting)
            }
            Inline::Tab(props) | Inline::Break(_, props) => *props != RunProps::default(),
            Inline::TextBox { blocks, .. } => blocks_carry_formatting(blocks),
            Inline::Revision { content, .. } => content.iter().any(inline_carries_formatting),
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::Field { .. }
            | Inline::UnsupportedRevision { .. }
            | Inline::FootnoteRef { .. }
            | Inline::Raw(_) => false,
        }
    }

    blocks.iter().any(|block| match block {
        Block::Paragraph(paragraph) => {
            paragraph.props != Default::default()
                || paragraph.content.iter().any(inline_carries_formatting)
        }
        Block::Table(table) => table.rows.iter().any(|row| {
            row.cells
                .iter()
                .any(|cell| blocks_carry_formatting(&cell.blocks))
        }),
        Block::SectionProperties(_) | Block::Raw(_) => false,
    })
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

/// Validate that `at` is a splice position [`insert_blocks`] (or plain-text
/// [`insert`]) can use: `0..=doc.content_block_count()`, and — unless `at` is the
/// document-end/append case — that block `at` is itself a paragraph.
///
/// Pure (no mutation, doesn't even need a live `Editor`): a caller that must
/// prepare package-level state before splicing — e.g. `docxy::control`'s
/// ensure-numbering/ensure-styles step, which mutates the `Package` a splice
/// verb never sees — should call this (or [`validate_replace_range`]) FIRST,
/// and only do that package mutation once the splice itself is known not to
/// fail on bounds/paragraph-kind. Otherwise a rejected out-of-bounds call
/// still leaves behind a numbering/styles part nothing actually used. Every
/// block-splice verb below validates in this same order — bounds/paragraph-
/// kind before content (e.g. "empty markdown") — so a caller pre-validating
/// this way never diverges from what the verb itself would reject.
pub fn validate_insert_at(doc: &Document, at: usize) -> Result<(), String> {
    let n = doc.content_block_count();
    if at > n {
        return Err(format!("'at' {at} out of bounds (0..={n})"));
    }
    if at < n {
        require_para(&doc.body, at)?;
    }
    Ok(())
}

/// Insert `blocks` before block `at` (or at the document end if
/// `at == doc.content_block_count()`, equivalent to [`append_blocks`]) — the block-splice
/// counterpart to [`insert`]. Pastes a placeholder clip of `blocks.len() + 1`
/// empty paragraphs at the head of block `at` (the trailing empty entry pushes
/// the original paragraph down intact, exactly as `insert`'s `"{text}\n"`
/// trick does for plain text), then [`overwrite_blocks`] turns the
/// `blocks.len()` opened slots into the real content. One [`Editor::paste`]
/// call is made, so this is **one** undo checkpoint, matching `insert`.
///
/// Validates bounds/paragraph-kind ([`validate_insert_at`]) before the
/// "non-empty `blocks`" check — see that function's doc comment for why a
/// caller doing pre-splice package prep must match this order.
pub fn insert_blocks(ed: &mut Editor, at: usize, blocks: Vec<Block>) -> Result<(), String> {
    validate_insert_at(&ed.doc, at)?;
    if blocks.is_empty() {
        return Err("empty markdown".to_string());
    }
    let n = ed.doc.content_block_count();
    if at == n {
        append_blocks(ed, blocks);
        return Ok(());
    }
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
/// `blocks`) touches nothing and pushes no checkpoint. As with [`append`], a
/// section mark on the last paragraph stays there and `blocks` land in the
/// final section.
pub fn append_blocks(ed: &mut Editor, blocks: Vec<Block>) {
    if blocks.is_empty() {
        return;
    }
    let start = ed.doc.content_block_count();
    let count = blocks.len();
    ed.anchor = None;
    ed.move_doc_end();
    ed.paste(&Clip {
        paras: vec![Vec::new(); count + 1],
    });
    // Before `overwrite_blocks` replaces the placeholder holding it.
    restore_last_section_mark(ed, start);
    overwrite_blocks(ed, start, blocks);
}

/// Validate that `[start..=end]` is a range [`replace_range_blocks`] (or
/// plain-text [`replace_range`]) can use: in bounds, and both ends
/// paragraphs. Pure, for the same pre-mutation-validation reason as
/// [`validate_insert_at`] (see its doc comment) — bounds/paragraph-kind
/// first, matching `replace_range_blocks`'s own check order.
pub fn validate_replace_range(doc: &Document, start: usize, end: usize) -> Result<(), String> {
    let n = doc.content_block_count();
    bounds(start, end, n)?;
    require_para(&doc.body, start)?;
    require_para(&doc.body, end)?;
    Ok(())
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
///
/// The end paragraph's section mark (its sectPr and tracked sectPr change)
/// stays on the last replacing paragraph, or on a new empty paragraph after a
/// trailing table, which adds one block beyond `blocks.len()`. Marks on
/// paragraphs before `end` are removed with the range (#801).
///
/// Validates bounds/paragraph-kind ([`validate_replace_range`]) before the
/// "non-empty `blocks`" check — same order as [`insert_blocks`], see
/// [`validate_insert_at`]'s doc comment for why.
pub fn replace_range_blocks(
    ed: &mut Editor,
    start: usize,
    end: usize,
    blocks: Vec<Block>,
) -> Result<(usize, usize), String> {
    validate_replace_range(&ed.doc, start, end)?;
    if blocks.is_empty() {
        return Err("empty markdown".to_string());
    }

    // The end paragraph's section mark, read before the paste puts it on a
    // placeholder that `overwrite_blocks` replaces (#801).
    let mark = match &ed.doc.body[end] {
        Block::Paragraph(p) => (
            p.props.section_break.clone(),
            p.props.section_property_change.clone(),
        ),
        _ => (None, None),
    };

    ed.anchor = None;
    ed.caret = Caret::top(end, 0);
    ed.move_end();
    ed.anchor = Some(Caret::top(start, 0));
    let deleted = ed.has_selection();
    let last = start + blocks.len() - 1;
    ed.paste(&Clip {
        paras: vec![Vec::new(); blocks.len()],
    });
    overwrite_blocks(ed, start, blocks);
    restore_end_section_mark(ed, last, mark);

    let undo_steps = if deleted { 2 } else { 1 };
    Ok((end - start + 1, undo_steps))
}

/// Put the section mark of a replaced range's end paragraph back on the
/// replacing content: on block `last` when it is a paragraph, else (a table)
/// on a new empty paragraph after it, since only a paragraph carries a sectPr.
/// Marks before the range's end went with the range. Called inside the
/// replace's last undo checkpoint, so it adds no step (#801).
fn restore_end_section_mark(
    ed: &mut Editor,
    last: usize,
    (brk, change): (Option<String>, Option<PropertyChange>),
) {
    if brk.is_none() && change.is_none() {
        return;
    }
    let at = if matches!(ed.doc.body[last], Block::Paragraph(_)) {
        last
    } else {
        ed.doc
            .body
            .insert(last + 1, Block::Paragraph(Paragraph::default()));
        last + 1
    };
    if let Block::Paragraph(p) = &mut ed.doc.body[at] {
        p.props.section_break = brk;
        p.props.section_property_change = change;
    }
    ed.doc.initialize_revision_targets();
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
// Formatting verbs (undoable, via the Editor) — `doc.format` / `doc.set-style`
// ---------------------------------------------------------------------------

/// The character-formatting names [`RunPatch::parse`]'s `highlight` key
/// accepts — the exact strings [`Editor::set_highlight`] stores verbatim on
/// [`RunProps::highlight`], matching every value `docxy`'s own Highlight
/// ribbon picker ever produces (`docxy/src/main.rs`'s `highlight_name`).
/// `"none"` is also accepted, as a 9th wire value, but *clears* the highlight
/// rather than naming a color — [`parse_highlight`] handles it specially, so
/// it isn't listed here.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "yellow",
    "green",
    "cyan",
    "magenta",
    "red",
    "blue",
    "lightGray",
    "darkYellow",
];

/// A `doc.format` patch: each field `Some` means the wire patch set that key;
/// `None` means the key was absent, so [`format_range`] leaves that aspect of
/// every touched run untouched. Field types mirror [`RunProps`]'s directly,
/// except:
/// - `bold`/`italic`/`underline`/`strike` are plain `bool`s (SET-to-value),
///   not toggles — see [`format_range`]'s doc comment.
/// - `size_half_pts` is this crate's half-point unit (what
///   [`Editor::set_font_size`] takes); the wire key is `size` in whole/
///   fractional **points** — [`RunPatch::parse`] converts.
///
/// Deliberately JSON-free, per `gridcore::format::FormatPatch`'s precedent (a
/// sibling crate this one can't depend on, so the shape — not the code — is
/// shared): a host builds `&[(String, String)]` wire pairs from its own JSON
/// and hands them to [`RunPatch::parse`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunPatch {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub color: Option<(u8, u8, u8)>,
    /// `Some(None)` clears the highlight (wire `"none"`); `Some(Some(name))`
    /// sets it to one of [`HIGHLIGHT_NAMES`]; `None` (the key was absent from
    /// the patch) leaves every touched run's highlight untouched.
    pub highlight: Option<Option<String>>,
    pub font: Option<String>,
    pub size_half_pts: Option<u32>,
}

impl RunPatch {
    /// Parse wire key/value pairs into a [`RunPatch`].
    ///
    /// Errors (all `String`, meant to surface to the agent verbatim) mirror
    /// `gridcore::format::FormatPatch::parse`'s family:
    /// - no pairs at all → `"patch needs at least one key"`
    /// - an unrecognized key → names the key
    /// - `bold`/`italic`/`underline`/`strike` not `"true"`/`"false"`
    /// - `color` not `"#RRGGBB"` → `"bad color '<v>' (want \"#RRGGBB\")"`
    /// - `highlight` outside [`HIGHLIGHT_NAMES`] ∪ `{"none"}`
    /// - `size` not a positive finite number
    pub fn parse(pairs: &[(String, String)]) -> Result<RunPatch, String> {
        if pairs.is_empty() {
            return Err("patch needs at least one key".to_string());
        }
        let mut patch = RunPatch::default();
        for (key, value) in pairs {
            match key.as_str() {
                "bold" => patch.bold = Some(parse_patch_bool("bold", value)?),
                "italic" => patch.italic = Some(parse_patch_bool("italic", value)?),
                "underline" => patch.underline = Some(parse_patch_bool("underline", value)?),
                "strike" => patch.strike = Some(parse_patch_bool("strike", value)?),
                "color" => patch.color = Some(parse_hex_color(value)?),
                "highlight" => patch.highlight = Some(parse_highlight(value)?),
                "font" => patch.font = Some(value.clone()),
                "size" => patch.size_half_pts = Some(parse_size(value)?),
                other => return Err(format!("unknown patch key '{other}'")),
            }
        }
        Ok(patch)
    }
}

fn parse_patch_bool(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("'{key}' must be true or false, got '{other}'")),
    }
}

/// Parse `"#RRGGBB"` (case-insensitive hex digits) into `(r, g, b)`. Keeps
/// the contract of `gridcore::format::parse_hex_color` (a sibling crate this
/// one can't depend on; it is built on `gridcore::format::hex_rgb`): the same
/// `bad color '<v>' (want "#RRGGBB")` message, and the same strict rule —
/// exactly six ASCII hex digits after the `#`, no `u8::from_str_radix`-style
/// leading `+` — so `doc.format` and `cell.format` accept the same colours.
fn parse_hex_color(s: &str) -> Result<(u8, u8, u8), String> {
    let bad = || format!("bad color '{s}' (want \"#RRGGBB\")");
    let hex = s.strip_prefix('#').ok_or_else(bad)?;
    if hex.len() != 6 || !hex.is_ascii() {
        return Err(bad());
    }
    fn hex_digit(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = hex.as_bytes();
    let byte =
        |i: usize| -> Option<u8> { Some((hex_digit(bytes[i])? << 4) | hex_digit(bytes[i + 1])?) };
    match (byte(0), byte(2), byte(4)) {
        (Some(r), Some(g), Some(b)) => Ok((r, g, b)),
        _ => Err(bad()),
    }
}

fn parse_highlight(value: &str) -> Result<Option<String>, String> {
    if value == "none" {
        return Ok(None);
    }
    if HIGHLIGHT_NAMES.contains(&value) {
        return Ok(Some(value.to_string()));
    }
    Err(format!(
        "bad highlight '{value}' (want one of {}, or none)",
        HIGHLIGHT_NAMES.join(", ")
    ))
}

/// Parse a whole/fractional POINTS value (`doc.format`'s wire `size` key)
/// into the half-points [`Editor::set_font_size`] takes.
fn parse_size(value: &str) -> Result<u32, String> {
    let bad = || format!("bad size '{value}' (want a positive number of points)");
    let pts: f32 = value.parse().map_err(|_| bad())?;
    if !pts.is_finite() || pts <= 0.0 {
        return Err(bad());
    }
    Ok((pts * 2.0).round() as u32)
}

/// Push exactly ONE undo checkpoint for a block-range formatting operation
/// ([`format_range`] / [`set_style_range`]), then leave the editor ready for
/// the caller's own direct mutation of `ed.doc.body[start..=end]`.
///
/// Neither `Editor::checkpoint` nor the private `map_props`/`for_each_para`
/// helpers behind its setters are `pub`, and this module doesn't touch
/// `editor.rs` — so the only way to land exactly one entry on the Editor's
/// own undo stack is through a real `pub` mutating call. [`Editor::set_align`]
/// is the right shape: unlike the run-level setters (`set_color`/`set_font`/…,
/// built on the private `map_props`, which checkpoints only when there's an
/// active selection), `set_align` checkpoints unconditionally, even with none
/// — falling back to just the caret's own paragraph. So clearing `ed.anchor`
/// first and pointing the caret at `start` makes it touch exactly ONE
/// paragraph (`start` itself, already confirmed to be one by the caller's
/// `require_para` check), never any nested table-cell content a genuine
/// multi-paragraph selection over `[start..=end]` could reach if a table sat
/// inside the range — `Editor::selection_spans` flattens ALL paragraphs,
/// including nested ones, between the anchor's and caret's positions in
/// document order (see `all_paragraph_paths`), which is out of bounds for
/// this wave's block-range-only granularity (tables mid-range are left
/// untouched — see [`format_range`]'s doc comment). Passing back `start`'s
/// own current align is an exact no-op value-wise, so the caller is free to
/// immediately overwrite `ed.doc.body[start..=end]` (including `start`'s own
/// align, if that's part of the patch) without colliding with this call's
/// effect.
fn checkpoint_for_block_range(ed: &mut Editor, start: usize) {
    ed.anchor = None;
    ed.caret = Caret::top(start, 0);
    let align = ed.caret_para_props().align;
    ed.set_align(align);
}

/// Apply `patch`'s SET-to-value fields to every run in `content` — bare
/// [`Inline::Run`]s, the runs inside an [`Inline::Hyperlink`] (its `runs`, and
/// the runs, tabs and breaks among its `content`), tabs and breaks (each is a
/// run in OOXML, and text typed after it takes its props), matching
/// `Editor`'s own char-range formatting helpers (`editor.rs`'s
/// `edit_run_range`). Every other inline kind (fields, footnote refs, …) is
/// left untouched.
fn apply_run_patch(content: &mut [Inline], patch: &RunPatch) {
    for inline in content.iter_mut() {
        match inline {
            Inline::Run(r) => apply_run_patch_props(&mut r.props, patch),
            Inline::Tab(props) | Inline::Break(_, props) => apply_run_patch_props(props, patch),
            Inline::Hyperlink(h) => {
                for r in h.runs.iter_mut() {
                    apply_run_patch_props(&mut r.props, patch);
                }
                // The plain runs among a complex link's other children too;
                // the link then saves from its content, not its stale raw.
                if h.content.iter().any(|i| crate::editor::inline_len(i) > 0) {
                    apply_run_patch(&mut h.content, patch);
                    h.content_changed = true;
                }
            }
            _ => {}
        }
    }
}

fn apply_run_patch_props(props: &mut RunProps, patch: &RunPatch) {
    if let Some(v) = patch.bold {
        props.bold = v;
    }
    if let Some(v) = patch.italic {
        props.italic = v;
    }
    if let Some(v) = patch.underline {
        props.underline = v;
    }
    if let Some(v) = patch.strike {
        props.strike = v;
    }
    if let Some((r, g, b)) = patch.color {
        props.color = Some(format!("{r:02X}{g:02X}{b:02X}"));
    }
    if let Some(h) = &patch.highlight {
        props.highlight = h.clone();
    }
    if let Some(f) = &patch.font {
        props.font = Some(f.clone());
    }
    if let Some(sz) = patch.size_half_pts {
        props.size_half_pts = Some(sz);
    }
}

/// Format every run in paragraphs `[start..=end]` (inclusive) per `patch` —
/// **SET-to-value** semantics: `bold:true` makes every touched run bold, full
/// stop, whether it started bold, plain, or mixed within the range; it is
/// **not** the toggle `Editor::toggle_bold` implements, so applying the same
/// patch twice is idempotent. Non-paragraph blocks in the range (tables) are
/// left untouched but still counted — see [`checkpoint_for_block_range`]'s
/// doc comment for why this deliberately never recurses into a table's
/// cells.
///
/// Returns the number of blocks in `[start..=end]` (== `end - start + 1`,
/// matching `doc.format`'s `{formatted:N}` reply). ONE undo checkpoint
/// regardless of how many of `patch`'s fields are set.
pub fn format_range(
    ed: &mut Editor,
    start: usize,
    end: usize,
    patch: &RunPatch,
) -> Result<usize, String> {
    let n = ed.doc.content_block_count();
    bounds(start, end, n)?;
    require_para(&ed.doc.body, start)?;
    require_para(&ed.doc.body, end)?;

    checkpoint_for_block_range(ed, start);
    for block in &mut ed.doc.body[start..=end] {
        if let Block::Paragraph(p) = block {
            apply_run_patch(&mut p.content, patch);
        }
    }
    Ok(end - start + 1)
}

/// Apply paragraph style `style` (`w:pStyle`) directly: `"Normal"` clears it
/// (and the resolved heading level) back to the default, matching
/// `Editor::set_para_style(None)`'s effect; any other value is stored as-is,
/// with the heading level re-resolved from it (`crate::load::heading_level`),
/// matching `Editor::set_para_style(Some(id))`'s effect.
fn apply_style_to_para(pr: &mut ParProps, style: &str) {
    if style == "Normal" {
        pr.style_id = None;
        pr.heading_level = None;
    } else {
        pr.heading_level = crate::load::heading_level(style);
        pr.style_id = Some(style.to_string());
    }
}

/// Validate a would-be [`set_style_range`] call — bounds/paragraph-kind at
/// both endpoints, at least one of `style`/`align` present, and (when given)
/// `style` names one of [`MARKDOWN_PARAGRAPH_STYLE_IDS`] or `"Normal"`.
///
/// Pure (no `Editor`, no `Package`): a control surface must call this FIRST,
/// before its own `Package::ensure_styles` mutation for a markdown-set style
/// id — same validate-before-mutate ordering as `validate_replace_range`
/// before `prepare_markdown_blocks` in `docxy::control` (see that function's
/// doc comment) — then still let [`set_style_range`] re-validate on the real
/// call, exactly as `replace_range_blocks` re-validates after
/// `prepare_markdown_blocks` runs.
pub fn validate_set_style_range(
    doc: &Document,
    start: usize,
    end: usize,
    style: Option<&str>,
    align: Option<Align>,
) -> Result<(), String> {
    let n = doc.content_block_count();
    bounds(start, end, n)?;
    require_para(&doc.body, start)?;
    require_para(&doc.body, end)?;
    if style.is_none() && align.is_none() {
        return Err("set-style needs 'style' or 'align'".to_string());
    }
    if let Some(s) = style {
        if s != "Normal" && !MARKDOWN_PARAGRAPH_STYLE_IDS.contains(&s) {
            return Err(format!(
                "unknown style '{s}' (want one of {}, Normal)",
                MARKDOWN_PARAGRAPH_STYLE_IDS.join(", ")
            ));
        }
    }
    Ok(())
}

/// Set the paragraph style and/or alignment of paragraphs `[start..=end]`
/// (inclusive). `style` is `None` (untouched), `"Normal"` (clears the style),
/// or one of [`MARKDOWN_PARAGRAPH_STYLE_IDS`]; `align` is `None` (untouched)
/// or an [`Align`] value. Non-paragraph blocks in the range are left
/// untouched but still counted, same as [`format_range`].
///
/// The caller (a control surface) is responsible for `Package::ensure_styles`
/// on a markdown-set `style` BEFORE this call — see
/// [`validate_set_style_range`]'s doc comment; this function only touches the
/// live `Document`, never a `Package`, so it never ensures anything itself.
///
/// Returns the number of blocks in `[start..=end]`. ONE undo checkpoint
/// regardless of whether `style`, `align`, or both are set — see
/// [`checkpoint_for_block_range`]'s doc comment.
pub fn set_style_range(
    ed: &mut Editor,
    start: usize,
    end: usize,
    style: Option<&str>,
    align: Option<Align>,
) -> Result<usize, String> {
    validate_set_style_range(&ed.doc, start, end, style, align)?;

    checkpoint_for_block_range(ed, start);
    for block in &mut ed.doc.body[start..=end] {
        if let Block::Paragraph(p) = block {
            if let Some(s) = style {
                apply_style_to_para(&mut p.props, s);
            }
            if let Some(a) = align {
                p.props.align = a;
            }
        }
    }
    Ok(end - start + 1)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub fn block_kind(b: &Block) -> &'static str {
    match b {
        Block::Paragraph(_) => "paragraph",
        Block::Table(_) => "table",
        Block::SectionProperties(_) => "section-properties",
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
    use crate::model::{
        Document, Inline, ParProps, Paragraph, Run, RunProps, SectionProperties, Table,
    };

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
        doc.body[..doc.content_block_count()]
            .iter()
            .map(|b| b.plain_text())
            .collect()
    }

    /// A paragraph with one run per `(text, bold)` pair — used to build
    /// mixed bold/plain selections for the `format_range` determinism tests.
    fn para_with_runs(runs: &[(&str, bool)]) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: runs
                .iter()
                .map(|&(t, bold)| {
                    Inline::Run(Run {
                        text: t.to_string(),
                        props: RunProps {
                            bold,
                            ..RunProps::default()
                        },
                    })
                })
                .collect(),
        })
    }

    /// Every run's bold flag across a paragraph's content, in order — used to
    /// assert `format_range`'s per-run effect on a mixed selection.
    fn bold_flags(b: &Block) -> Vec<bool> {
        let Block::Paragraph(p) = b else {
            panic!("expected a paragraph: {b:?}")
        };
        p.content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => r.props.bold,
                other => panic!("expected a run: {other:?}"),
            })
            .collect()
    }

    /// Build a [`RunPatch`] from `key: value` string pairs via
    /// [`RunPatch::parse`] — the same host-buildable wire shape
    /// `docxy::control` feeds it, so these tests exercise the real parser.
    fn run_patch(pairs: &[(&str, &str)]) -> RunPatch {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|&(k, v)| (k.to_string(), v.to_string()))
            .collect();
        RunPatch::parse(&owned).unwrap()
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
    fn markdown_splices_never_write_the_field_character_642() {
        let blocks = parse_markdown_blocks("Page \u{FFFC} of **\u{FFFC}3**").unwrap();
        match &blocks[0] {
            Block::Paragraph(p) => assert_eq!(p.plain_text(), "Page  of 3"),
            other => panic!("expected a paragraph, got {other:?}"),
        }
        assert_eq!(
            parse_markdown_blocks("\u{FFFC}\n").unwrap_err(),
            "empty markdown"
        );
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

    /// Append after "A", which closes the first section (with a tracked sectPr
    /// change) before an empty final one: the section and the saved sectPr
    /// count survive, "A" keeps its mark, the `count` appended blocks have
    /// none, and one undo restores the document (#748).
    fn check_append_keeps_the_section_748(
        what: &str,
        count: usize,
        append: impl FnOnce(&mut Editor),
    ) {
        use crate::model::{PropertyChange, PropertyScope, PropertySnapshot, PropertyState};
        let brk = "<w:sectPr><w:type w:val=\"nextPage\"/></w:sectPr>".to_string();
        let change = PropertyChange {
            scope: PropertyScope::Section,
            metadata: Default::default(),
            raw: format!("<w:sectPrChange w:id=\"7\">{brk}</w:sectPrChange>"),
            previous: PropertySnapshot::Present(PropertyState::Section(brk.clone())),
        };
        let mark = |ed: &Editor, i: usize| match &ed.doc.body[i] {
            Block::Paragraph(p) => (
                p.props.section_break.clone(),
                p.props
                    .section_property_change
                    .as_ref()
                    .map(|c| c.raw.clone()),
            ),
            _ => (None, None),
        };
        let sect_prs = |ed: &Editor| {
            crate::serialize::document_to_xml(&ed.doc)
                .matches("<w:sectPr")
                .count()
        };
        let mut doc = doc_with(&["A"]);
        if let Block::Paragraph(p) = &mut doc.body[0] {
            p.props.section_break = Some(brk.clone());
            p.props.section_property_change = Some(change.clone());
        }
        doc.body.push(Block::SectionProperties(SectionProperties {
            raw: "<w:sectPr/>".into(),
            property_change: None,
        }));
        let mut ed = Editor::new(doc);
        let before = ed.doc.clone();
        let sections = ed.sections().len();
        let saved = sect_prs(&ed);
        append(&mut ed);
        assert_eq!(ed.doc.content_block_count(), 1 + count, "{what}");
        assert_eq!(ed.sections().len(), sections, "{what}");
        assert_eq!(sect_prs(&ed), saved, "{what}");
        assert_eq!(mark(&ed, 0), (Some(brk), Some(change.raw)), "{what}");
        for i in 1..=count {
            assert_eq!(mark(&ed, i), (None, None), "{what}: block {i}");
        }
        assert!(ed.undo());
        assert_eq!(ed.doc, before, "{what}");
    }

    #[test]
    fn append_blocks_after_a_section_closing_paragraph_keeps_the_section_748() {
        let tables = "| a | b |\n| - | - |\n| 1 | 2 |";
        for md in ["## Heading", "one\n\ntwo", &format!("one\n\n{tables}")] {
            let blocks = parse_markdown_blocks(md).unwrap();
            assert_eq!(
                md.contains('|'),
                blocks.iter().any(|b| matches!(b, Block::Table(_))),
                "{md}"
            );
            check_append_keeps_the_section_748(md, blocks.len(), |ed| append_blocks(ed, blocks));
        }
    }

    #[test]
    fn append_after_a_section_closing_paragraph_keeps_the_section_748() {
        for (text, count) in [("one", 1), ("one\ntwo", 2)] {
            check_append_keeps_the_section_748(text, count, |ed| append(ed, text));
        }
    }

    // ---- #801: replace_range_blocks keeps the range's own section end ----

    fn section_mark_801() -> (String, crate::model::PropertyChange) {
        use crate::model::{PropertyChange, PropertyScope, PropertySnapshot, PropertyState};
        let brk = "<w:sectPr><w:type w:val=\"oddPage\"/></w:sectPr>".to_string();
        let prior = "<w:sectPr><w:type w:val=\"nextPage\"/></w:sectPr>".to_string();
        let change = PropertyChange {
            scope: PropertyScope::Section,
            metadata: Default::default(),
            raw: format!("<w:sectPrChange w:id=\"7\">{prior}</w:sectPrChange>"),
            previous: PropertySnapshot::Present(PropertyState::Section(prior)),
        };
        (brk, change)
    }

    /// "A", "B", "C", "D" before a trailing sectPr; the paragraph at `closes`
    /// ends the first section, with a tracked sectPr change.
    fn sectioned_801(closes: usize) -> Editor {
        let (brk, change) = section_mark_801();
        let mut doc = doc_with(&["A", "B", "C", "D"]);
        if let Block::Paragraph(p) = &mut doc.body[closes] {
            p.props.section_break = Some(brk);
            p.props.section_property_change = Some(change);
        }
        doc.body.push(Block::SectionProperties(SectionProperties {
            raw: "<w:sectPr/>".into(),
            property_change: None,
        }));
        Editor::new(doc)
    }

    fn mark_801(ed: &Editor, i: usize) -> (Option<String>, Option<String>) {
        match &ed.doc.body[i] {
            Block::Paragraph(p) => (
                p.props.section_break.clone(),
                p.props
                    .section_property_change
                    .as_ref()
                    .map(|c| c.raw.clone()),
            ),
            _ => (None, None),
        }
    }

    fn saved_sect_prs_801(ed: &Editor) -> usize {
        crate::serialize::document_to_xml(&ed.doc)
            .matches("<w:sectPr")
            .count()
    }

    /// Replace `[start..=2]` of `sectioned_801(2)` (where "C" closes the first
    /// section) via `replace`: the section, the saved sectPr count and the
    /// tracked change survive, the mark sits on block `marked` alone, the text
    /// reads `texts`, and undoing `undo_steps` restores the document.
    fn check_replace_keeps_the_end_mark_801(
        what: &str,
        start: usize,
        marked: usize,
        texts: &[&str],
        replace: impl FnOnce(&mut Editor) -> (usize, usize),
    ) {
        let (brk, change) = section_mark_801();
        let mut ed = sectioned_801(2);
        let before = ed.doc.clone();
        let sections = ed.sections().len();
        let saved = saved_sect_prs_801(&ed);
        let (replaced, undo_steps) = replace(&mut ed);
        assert_eq!(replaced, 3 - start, "{what}");
        assert_eq!(paras(&ed.doc), texts, "{what}");
        assert_eq!(ed.sections().len(), sections, "{what}");
        assert_eq!(saved_sect_prs_801(&ed), saved, "{what}");
        for i in 0..ed.doc.content_block_count() {
            let want = if i == marked {
                (Some(brk.clone()), Some(change.raw.clone()))
            } else {
                (None, None)
            };
            assert_eq!(mark_801(&ed, i), want, "{what}: block {i}");
        }
        for _ in 0..undo_steps {
            assert!(ed.undo(), "{what}");
        }
        assert_eq!(ed.doc, before, "{what}");
    }

    #[test]
    fn replace_range_blocks_keeps_the_end_section_mark_801() {
        let cases: [(&str, usize, usize, &[&str]); 4] = [
            ("x", 2, 2, &["A", "B", "x", "D"]),
            ("x", 1, 1, &["A", "x", "D"]),
            ("x\n\ny", 1, 2, &["A", "x", "y", "D"]),
            ("x\n\ny\n\nz", 2, 4, &["A", "B", "x", "y", "z", "D"]),
        ];
        for (md, start, marked, texts) in cases {
            let what = format!("{md:?} over {start}..=2");
            let blocks = parse_markdown_blocks(md).unwrap();
            check_replace_keeps_the_end_mark_801(&what, start, marked, texts, |ed| {
                replace_range_blocks(ed, start, 2, blocks).unwrap()
            });
        }
    }

    #[test]
    fn replace_range_blocks_ending_in_a_table_keeps_the_section_mark_801() {
        let table = "| a | b |\n| - | - |\n| 1 | 2 |";
        for (md, start) in [(table.to_string(), 2), (format!("x\n\n{table}"), 1)] {
            let blocks = parse_markdown_blocks(&md).unwrap();
            let n = blocks.len();
            assert!(matches!(blocks.last(), Some(Block::Table(_))), "{md}");
            let mut ed = sectioned_801(2);
            let before = ed.doc.clone();
            let sections = ed.sections().len();
            let saved = saved_sect_prs_801(&ed);
            let (_, steps) = replace_range_blocks(&mut ed, start, 2, blocks).unwrap();
            // The table, then an empty paragraph that holds the mark.
            let table_at = start + n - 1;
            assert!(matches!(ed.doc.body[table_at], Block::Table(_)), "{md}");
            let (brk, change) = section_mark_801();
            assert_eq!(
                mark_801(&ed, table_at + 1),
                (Some(brk), Some(change.raw)),
                "{md}"
            );
            assert_eq!(ed.doc.body[table_at + 1].plain_text(), "", "{md}");
            assert_eq!(ed.doc.body[table_at + 2].plain_text(), "D", "{md}");
            assert_eq!(ed.sections().len(), sections, "{md}");
            assert_eq!(saved_sect_prs_801(&ed), saved, "{md}");
            for _ in 0..steps {
                assert!(ed.undo(), "{md}");
            }
            assert_eq!(ed.doc, before, "{md}");
        }
    }

    #[test]
    fn replace_range_blocks_drops_a_section_mark_inside_the_range_801() {
        // "B" closes the first section and "C", the range's end, closes none:
        // B's break was inside the replaced range, so it goes with it.
        for md in ["x", "x\n\ny"] {
            let mut ed = sectioned_801(1);
            let sections = ed.sections().len();
            replace_range_blocks(&mut ed, 1, 2, parse_markdown_blocks(md).unwrap()).unwrap();
            assert_eq!(ed.sections().len(), sections - 1, "{md}");
            // Only the trailing sectPr is left to save.
            assert_eq!(saved_sect_prs_801(&ed), 1, "{md}");
            for i in 0..ed.doc.content_block_count() {
                assert_eq!(mark_801(&ed, i), (None, None), "{md}: block {i}");
            }
        }
    }

    #[test]
    fn replace_range_keeps_the_end_section_mark_801() {
        // The plain-text verb never lost it: pinned alongside the block one.
        let cases: [(&str, usize, usize, &[&str]); 3] = [
            ("x", 2, 2, &["A", "B", "x", "D"]),
            ("x", 1, 1, &["A", "x", "D"]),
            ("x\ny", 1, 2, &["A", "x", "y", "D"]),
        ];
        for (text, start, marked, texts) in cases {
            let what = format!("{text:?} over {start}..=2");
            check_replace_keeps_the_end_mark_801(&what, start, marked, texts, |ed| {
                replace_range(ed, start, 2, text).unwrap()
            });
        }
    }

    #[test]
    fn public_block_coordinates_keep_trailing_section_properties_out_of_splices() {
        let mut doc = doc_with(&["existing"]);
        doc.body
            .push(Block::SectionProperties(SectionProperties::default()));
        let mut ed = Editor::new(doc);

        assert_eq!(ed.doc.content_block_count(), 1);
        append_blocks(&mut ed, parse_markdown_blocks("## Heading").unwrap());
        assert_eq!(ed.doc.content_block_count(), 2);
        assert_eq!(paras(&ed.doc), vec!["existing", "Heading"]);
        assert!(matches!(
            ed.doc.body.last(),
            Some(Block::SectionProperties(_))
        ));
        assert_eq!(stats(&ed.doc).3, 2);
        assert_eq!(
            crate::markdown::to_markdown(&ed.doc),
            "existing\n\n## Heading\n"
        );

        assert!(ed.undo());
        assert_eq!(ed.doc.content_block_count(), 1);
        assert!(matches!(
            ed.doc.body.last(),
            Some(Block::SectionProperties(_))
        ));
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
    fn referenced_style_ids_finds_known_pstyles_in_first_seen_order() {
        let blocks = parse_markdown_blocks("# Title\n\n> a quote\n\n```\ncode\n```").unwrap();
        assert_eq!(
            referenced_style_ids(&blocks),
            vec!["Heading1", "Quote", "SourceCode"]
        );
        // Plain/bold text with no style-carrying construct references nothing.
        let plain = parse_markdown_blocks("just **bold** text").unwrap();
        assert!(referenced_style_ids(&plain).is_empty());
    }

    #[test]
    fn referenced_numbering_kinds_detects_bullet_and_decimal_separately() {
        let bullet_only = parse_markdown_blocks("- one\n- two").unwrap();
        assert_eq!(referenced_numbering_kinds(&bullet_only), (true, false));

        let decimal_only = parse_markdown_blocks("1. one\n2. two").unwrap();
        assert_eq!(referenced_numbering_kinds(&decimal_only), (false, true));

        let both = parse_markdown_blocks("- a\n\n1. b").unwrap();
        assert_eq!(referenced_numbering_kinds(&both), (true, true));

        let neither = parse_markdown_blocks("plain paragraph").unwrap();
        assert_eq!(referenced_numbering_kinds(&neither), (false, false));
    }

    #[test]
    fn validators_reject_exactly_what_the_splice_verbs_would() {
        // Pure, `Editor`-free pre-checks a caller can run before doing any
        // package-level prep (numbering/styles) — must agree with what
        // `insert_blocks`/`replace_range_blocks` themselves would reject.
        let doc = doc_with(&["A", "B"]);
        assert!(validate_insert_at(&doc, 5).is_err());
        assert!(validate_insert_at(&doc, 2).is_ok()); // == len: the append case
        assert!(validate_insert_at(&doc, 0).is_ok());
        assert!(validate_replace_range(&doc, 0, 5).is_err());
        assert!(validate_replace_range(&doc, 2, 0).is_err()); // start > end
        assert!(validate_replace_range(&doc, 0, 1).is_ok());
    }

    #[test]
    fn insert_blocks_out_of_bounds_does_not_touch_the_document() {
        // Regression guard: bounds validation must run — and fail — before
        // any splicing happens, so a caller that pre-validates the same way
        // (e.g. before mutating a Package for numbering/styles) never ends up
        // with a mutated package backing a rejected, no-op edit.
        let mut ed = Editor::new(doc_with(&["A"]));
        let before = ed.doc.clone();
        let blocks = parse_markdown_blocks("- item").unwrap();
        assert!(insert_blocks(&mut ed, 99, blocks).is_err());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo(), "a rejected splice must push no checkpoint");
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

    /// #197: a link and tracked changes before the match must not shift the
    /// match or the edit. Offsets are editor offsets; revision text is not
    /// searched and stays untouched.
    #[test]
    fn find_and_replace_all_ignore_zero_width_inlines_before_the_match() {
        let xml = "<w:document><w:body><w:p>\
            <w:r><w:t xml:space=\"preserve\">Start </w:t></w:r>\
            <w:hyperlink w:anchor=\"top\"><w:r><w:t>link</w:t></w:r></w:hyperlink>\
            <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>added</w:t></w:r></w:ins>\
            <w:del w:id=\"2\" w:author=\"A\"><w:r><w:delText>removed</w:delText></w:r></w:del>\
            <w:r><w:t xml:space=\"preserve\"> end.</w:t></w:r>\
            </w:p></w:body></w:document>";
        let doc = crate::load::parse_document_xml(xml, &crate::load::Relationships::default());
        let matches = find(&doc, "end", false);
        assert_eq!(matches.len(), 1);
        assert_eq!((matches[0].start, matches[0].end), (11, 14));
        assert!(find(&doc, "added", false).is_empty());

        let mut ed = Editor::new(doc);
        assert_eq!(replace_all(&mut ed, "end", "finish", false), (1, 1));
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!("expected a paragraph");
        };
        // plain_text still includes the (untouched) revision text.
        assert_eq!(p.plain_text(), "Start linkaddedremoved finish.");
        assert_eq!(
            p.content
                .iter()
                .filter(|i| matches!(i, Inline::Revision { .. }))
                .count(),
            2
        );
        assert_eq!(replace_all(&mut ed, "removed", "X", false), (0, 0));
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

    // -----------------------------------------------------------------------
    // RunPatch::parse
    // -----------------------------------------------------------------------

    #[test]
    fn run_patch_parse_empty_pairs_errors() {
        assert_eq!(
            RunPatch::parse(&[]).unwrap_err(),
            "patch needs at least one key"
        );
    }

    #[test]
    fn run_patch_parse_unknown_key_names_itself() {
        let err = RunPatch::parse(&[("wat".to_string(), "1".to_string())]).unwrap_err();
        assert!(err.contains("wat"), "{err}");
    }

    #[test]
    fn run_patch_parse_bad_bool_is_rejected() {
        let err = RunPatch::parse(&[("bold".to_string(), "yes".to_string())]).unwrap_err();
        assert!(err.contains("bold"), "{err}");
    }

    #[test]
    fn run_patch_parse_bad_color_is_rejected() {
        for bad in ["FF0000", "#FF00", "#GGGGGG", "red", "#+00000"] {
            let err = RunPatch::parse(&[("color".to_string(), bad.to_string())]).unwrap_err();
            assert!(err.contains("color"), "input '{bad}': {err}");
        }
    }

    #[test]
    fn run_patch_parse_bad_highlight_is_rejected() {
        let err = RunPatch::parse(&[("highlight".to_string(), "purple".to_string())]).unwrap_err();
        assert!(err.contains("purple"), "{err}");
    }

    #[test]
    fn run_patch_parse_bad_size_is_rejected() {
        for bad in ["", "abc", "0", "-3", "NaN"] {
            let err = RunPatch::parse(&[("size".to_string(), bad.to_string())]).unwrap_err();
            assert!(err.contains("size"), "input '{bad}': {err}");
        }
    }

    #[test]
    fn run_patch_parse_every_key() {
        let p = run_patch(&[
            ("bold", "true"),
            ("italic", "false"),
            ("underline", "true"),
            ("strike", "false"),
            ("color", "#FF0000"),
            ("highlight", "yellow"),
            ("font", "Consolas"),
            ("size", "10.5"),
        ]);
        assert_eq!(p.bold, Some(true));
        assert_eq!(p.italic, Some(false));
        assert_eq!(p.underline, Some(true));
        assert_eq!(p.strike, Some(false));
        assert_eq!(p.color, Some((255, 0, 0)));
        assert_eq!(p.highlight, Some(Some("yellow".to_string())));
        assert_eq!(p.font, Some("Consolas".to_string()));
        // 10.5pt -> 21 half-points.
        assert_eq!(p.size_half_pts, Some(21));
    }

    #[test]
    fn run_patch_parse_highlight_none_clears() {
        let p = run_patch(&[("highlight", "none")]);
        assert_eq!(p.highlight, Some(None));
    }

    // -----------------------------------------------------------------------
    // format_range
    // -----------------------------------------------------------------------

    #[test]
    fn format_range_bold_true_sets_every_run_regardless_of_starting_state() {
        let mut doc = doc_with(&["placeholder"]);
        doc.body[0] = para_with_runs(&[("a", true), ("b", false)]);
        doc.body.push(para_with_runs(&[("c", false)]));
        let mut ed = Editor::new(doc);

        let n = format_range(&mut ed, 0, 1, &run_patch(&[("bold", "true")])).unwrap();
        assert_eq!(n, 2);
        assert_eq!(bold_flags(&ed.doc.body[0]), vec![true, true]);
        assert_eq!(bold_flags(&ed.doc.body[1]), vec![true]);
    }

    #[test]
    fn format_range_formats_tabs_too_120() {
        let mut doc = doc_with(&["placeholder"]);
        doc.body[0] = Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![
                Inline::Run(Run {
                    text: "Name:".into(),
                    props: RunProps::default(),
                }),
                Inline::Tab(RunProps::default()),
            ],
        });
        let mut ed = Editor::new(doc);

        format_range(&mut ed, 0, 0, &run_patch(&[("bold", "true")])).unwrap();
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!("expected a paragraph")
        };
        assert!(
            matches!(&p.content[1], Inline::Tab(props) if props.bold),
            "the tab is bold, so typing after it is too: {:?}",
            p.content
        );
    }

    #[test]
    fn format_range_formats_the_plain_runs_inside_a_complex_link_212() {
        let ins = Inline::Revision {
            kind: crate::model::RevisionKind::Insert,
            metadata: Default::default(),
            raw: "<w:ins/>".into(),
            content: vec![Inline::Run(Run {
                text: "X".into(),
                props: RunProps::default(),
            })],
            content_changed: false,
        };
        let mut doc = doc_with(&["placeholder"]);
        doc.body[0] = Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::Hyperlink(crate::model::Hyperlink {
                anchor: Some("top".into()),
                content: vec![
                    Inline::Raw("<w:proofErr w:type=\"spellStart\"/>".into()),
                    Inline::Run(Run {
                        text: "Contoso".into(),
                        props: RunProps::default(),
                    }),
                    ins,
                ],
                raw: Some("<w:hyperlink w:anchor=\"top\"/>".into()),
                ..Default::default()
            })],
        });
        let mut ed = Editor::new(doc);

        format_range(&mut ed, 0, 0, &run_patch(&[("bold", "true")])).unwrap();
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!("expected a paragraph")
        };
        let Inline::Hyperlink(h) = &p.content[0] else {
            panic!("expected the link: {:?}", p.content)
        };
        assert!(h.content_changed, "the link saves from its content");
        assert!(matches!(&h.content[1], Inline::Run(r) if r.props.bold));
        assert!(
            matches!(&h.content[2], Inline::Revision { content, .. }
                if matches!(&content[0], Inline::Run(r) if !r.props.bold)),
            "a tracked change's own runs are left alone"
        );
    }

    #[test]
    fn format_range_bold_false_clears_every_run() {
        let mut doc = doc_with(&["placeholder"]);
        doc.body[0] = para_with_runs(&[("a", true), ("b", true)]);
        let mut ed = Editor::new(doc);

        format_range(&mut ed, 0, 0, &run_patch(&[("bold", "false")])).unwrap();
        assert_eq!(bold_flags(&ed.doc.body[0]), vec![false, false]);
    }

    #[test]
    fn format_range_is_idempotent() {
        let mut doc = doc_with(&["placeholder"]);
        doc.body[0] = para_with_runs(&[("a", true), ("b", false)]);
        let mut ed = Editor::new(doc);
        let patch = run_patch(&[("bold", "true")]);

        format_range(&mut ed, 0, 0, &patch).unwrap();
        let once = ed.doc.clone();
        format_range(&mut ed, 0, 0, &patch).unwrap();
        assert_eq!(bold_flags(&ed.doc.body[0]), vec![true, true]);
        // A second, identical application changes nothing further beyond the
        // (harmless) checkpoint push — the resulting document is unchanged.
        assert_eq!(ed.doc.body, once.body);
    }

    #[test]
    fn format_range_is_one_checkpoint_and_undo_restores_exact_prior_props() {
        let mut doc = doc_with(&["placeholder", "second"]);
        doc.body[0] = para_with_runs(&[("a", true), ("b", false)]);
        let mut ed = Editor::new(doc);
        let before = ed.doc.clone();

        // A multi-key patch — if this checkpointed once per field, more than
        // one undo would be needed to fully revert.
        let patch = run_patch(&[
            ("bold", "true"),
            ("italic", "true"),
            ("color", "#00FF00"),
            ("highlight", "yellow"),
            ("font", "Consolas"),
            ("size", "14"),
        ]);
        format_range(&mut ed, 0, 1, &patch).unwrap();
        assert_ne!(
            ed.doc, before,
            "sanity: the patch actually changed something"
        );

        assert!(ed.undo());
        assert_eq!(ed.doc, before, "one undo must restore EXACT prior props");
        assert!(!ed.undo(), "only one checkpoint should have been pushed");
    }

    #[test]
    fn format_range_bounds_and_require_para_errors_touch_nothing() {
        let mut doc = doc_with(&["A"]);
        doc.body.push(Block::Table(Table::default()));
        let mut ed = Editor::new(doc);
        let before = ed.doc.clone();
        let patch = run_patch(&[("bold", "true")]);

        let err = format_range(&mut ed, 0, 5, &patch).unwrap_err();
        assert!(err.contains("out of bounds"), "{err}");
        assert_eq!(ed.doc, before);

        // Block 1 (the table) is not a paragraph — rejected as an endpoint.
        let err2 = format_range(&mut ed, 0, 1, &patch).unwrap_err();
        assert!(err2.contains("not a paragraph"), "{err2}");
        assert_eq!(ed.doc, before);

        assert!(!ed.undo(), "a rejected format must push no checkpoint");
    }

    #[test]
    fn format_range_skips_a_table_block_mid_range_but_still_counts_it() {
        let mut doc = doc_with(&["A", "B"]);
        doc.body.insert(1, Block::Table(Table::default()));
        // doc.body is now [A, Table, B]; A and B start plain (not bold).
        let mut ed = Editor::new(doc);
        let before_table = ed.doc.body[1].clone();

        let n = format_range(&mut ed, 0, 2, &run_patch(&[("bold", "true")])).unwrap();
        assert_eq!(n, 3, "the table block is counted");
        assert_eq!(bold_flags(&ed.doc.body[0]), vec![true]);
        assert_eq!(bold_flags(&ed.doc.body[2]), vec![true]);
        assert_eq!(
            ed.doc.body[1], before_table,
            "the table itself must be left untouched"
        );

        assert!(ed.undo());
        assert_eq!(paras(&ed.doc), vec!["A", "", "B"]);
    }

    // -----------------------------------------------------------------------
    // set_style_range
    // -----------------------------------------------------------------------

    #[test]
    fn set_style_range_heading1_sets_style_and_heading_level_one_undo_reverts() {
        let mut ed = Editor::new(doc_with(&["Title", "Also", "tail"]));
        let before = ed.doc.clone();

        let n = set_style_range(&mut ed, 0, 1, Some("Heading1"), None).unwrap();
        assert_eq!(n, 2);
        for i in 0..2 {
            let Block::Paragraph(p) = &ed.doc.body[i] else {
                panic!()
            };
            assert_eq!(p.props.style_id.as_deref(), Some("Heading1"));
            assert_eq!(p.props.heading_level, Some(1));
        }
        // Untouched paragraph outside the range.
        let Block::Paragraph(p2) = &ed.doc.body[2] else {
            panic!()
        };
        assert_eq!(p2.props.style_id, None);

        assert!(ed.undo());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    #[test]
    fn set_style_range_normal_clears_style_without_touching_align() {
        let mut doc = doc_with(&["A"]);
        if let Block::Paragraph(p) = &mut doc.body[0] {
            p.props.style_id = Some("Heading2".to_string());
            p.props.heading_level = Some(2);
            p.props.align = Align::Right;
        }
        let mut ed = Editor::new(doc);

        set_style_range(&mut ed, 0, 0, Some("Normal"), None).unwrap();
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!()
        };
        assert_eq!(p.props.style_id, None);
        assert_eq!(p.props.heading_level, None);
        assert_eq!(
            p.props.align,
            Align::Right,
            "align untouched by style-only call"
        );
    }

    #[test]
    fn set_style_range_align_only_is_one_checkpoint() {
        let mut ed = Editor::new(doc_with(&["A", "B"]));
        let before = ed.doc.clone();

        set_style_range(&mut ed, 0, 1, None, Some(Align::Center)).unwrap();
        for i in 0..2 {
            let Block::Paragraph(p) = &ed.doc.body[i] else {
                panic!()
            };
            assert_eq!(p.props.align, Align::Center);
        }
        assert!(ed.undo());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    #[test]
    fn set_style_range_style_and_align_together_is_one_checkpoint() {
        let mut ed = Editor::new(doc_with(&["A"]));
        let before = ed.doc.clone();

        set_style_range(&mut ed, 0, 0, Some("Quote"), Some(Align::Justify)).unwrap();
        let Block::Paragraph(p) = &ed.doc.body[0] else {
            panic!()
        };
        assert_eq!(p.props.style_id.as_deref(), Some("Quote"));
        assert_eq!(p.props.align, Align::Justify);

        assert!(ed.undo());
        assert_eq!(ed.doc, before, "one undo must restore both style and align");
        assert!(!ed.undo());
    }

    #[test]
    fn set_style_range_needs_style_or_align_and_touches_nothing() {
        let mut ed = Editor::new(doc_with(&["A"]));
        let before = ed.doc.clone();
        let err = set_style_range(&mut ed, 0, 0, None, None).unwrap_err();
        assert_eq!(err, "set-style needs 'style' or 'align'");
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    #[test]
    fn set_style_range_unknown_style_lists_the_accepted_set() {
        let mut ed = Editor::new(doc_with(&["A"]));
        let before = ed.doc.clone();
        let err = set_style_range(&mut ed, 0, 0, Some("Bogus"), None).unwrap_err();
        assert!(err.contains("Bogus"), "{err}");
        for id in MARKDOWN_PARAGRAPH_STYLE_IDS {
            assert!(err.contains(*id), "{err} missing {id}");
        }
        assert!(err.contains("Normal"), "{err}");
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    #[test]
    fn set_style_range_bounds_and_require_para_errors_touch_nothing() {
        let mut ed = Editor::new(doc_with(&["A"]));
        let before = ed.doc.clone();
        assert!(set_style_range(&mut ed, 0, 5, Some("Heading1"), None).is_err());
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());
    }

    #[test]
    fn validate_set_style_range_matches_what_set_style_range_itself_rejects() {
        let doc = doc_with(&["A"]);
        assert!(validate_set_style_range(&doc, 0, 0, None, None).is_err());
        assert!(validate_set_style_range(&doc, 0, 0, Some("Bogus"), None).is_err());
        assert!(validate_set_style_range(&doc, 0, 5, Some("Heading1"), None).is_err());
        assert!(validate_set_style_range(&doc, 0, 0, Some("Heading1"), None).is_ok());
        assert!(validate_set_style_range(&doc, 0, 0, None, Some(Align::Left)).is_ok());
        assert!(validate_set_style_range(&doc, 0, 0, Some("Normal"), None).is_ok());
    }
}
