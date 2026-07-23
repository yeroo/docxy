//! Markdown ⇄ [`Document`] conversion.
//!
//! A pragmatic CommonMark-ish bridge so docxy can open/edit/save `.md` files and
//! convert between Markdown and `.docx`. It is deliberately a *semantic* mapping
//! onto the same [`Document`] model the editor and renderer already use — not a
//! spec-complete Markdown engine:
//!
//! - ATX headings (`#`..`######`) ⇄ heading paragraphs (`Heading1`..`Heading6`).
//! - `**bold**`, `*italic*`, `~~strike~~`, `` `code` ``, `[text](url)` links.
//! - `-`/`*`/`+` bullet lists and `1.` ordered lists (nested by indentation).
//! - `---`/`***`/`___` thematic breaks ⇄ a bottom-border (Word "horizontal line").
//! - GFM pipe tables ⇄ [`Table`].
//! - Fenced code blocks become plain paragraphs (the fence isn't re-modeled).
//! - `$…$` / `$$…$$` math ⇄ Word equations (OMML), via [`crate::latex`].
//!
//! Underline and `_emphasis_` are intentionally *not* parsed: Markdown has no
//! underline, and treating `_` as emphasis mangles `snake_case`/URLs. The list
//! markers used on output come from the document's numbering when available, so a
//! real `.docx` exported to Markdown keeps its ordered vs. bulleted lists.
//!
//! These round-trip via dedicated styles so the marker survives both a Markdown →
//! Markdown pass and a `.docx` save/reload:
//! - inline `` `code` `` ⇄ a run with the `Code` character style (`RunProps.code`);
//! - `> blockquote` ⇄ a paragraph with the `Quote` style;
//! - fenced code blocks ⇄ `SourceCode`-styled paragraphs, re-fenced on output.

use std::collections::HashMap;

use crate::model::{
    Block, BorderKind, Cell, Document, Hyperlink, Inline, ParProps, Paragraph, Row, Run, RunProps,
    Table,
};

// ===========================================================================
// Document -> Markdown
// ===========================================================================

/// Convert a document to Markdown text. List paragraphs all render as `-`
/// bullets (no numbering context); use [`to_markdown_with`] to keep ordered
/// lists when a marker map is available.
pub fn to_markdown(doc: &Document) -> String {
    to_markdown_with(doc, &HashMap::new())
}

/// Convert a document to Markdown, using a precomputed marker map (keyed by
/// top-level block path `[i]`, as produced by [`crate::numbering::compute_markers`])
/// to choose `1.` ordered vs `-` bulleted list items.
pub fn to_markdown_with(doc: &Document, markers: &HashMap<Vec<usize>, String>) -> String {
    let mut out = String::new();
    // The list a paragraph belongs to (`num_id`), or `None` when it isn't a list
    // item. Tracked so items of the *same* list pack together while a switch to a
    // different list (e.g. bullets → ordered) still gets a separating blank line.
    let mut prev_list: Option<i32> = None;
    let mut prev_any = false;
    let mut i = 0;
    while i < doc.body.len() {
        let b = &doc.body[i];
        // A run of "SourceCode" paragraphs becomes one fenced code block.
        if matches!(b, Block::Paragraph(p) if is_source_code(p)) {
            if prev_any {
                out.push('\n');
            }
            out.push_str("```\n");
            while let Some(Block::Paragraph(p)) = doc.body.get(i) {
                if !is_source_code(p) {
                    break;
                }
                out.push_str(&p.plain_text());
                out.push('\n');
                i += 1;
            }
            out.push_str("```\n");
            prev_list = None;
            prev_any = true;
            continue;
        }
        let cur_list = match b {
            Block::Paragraph(p) if p.props.num_id.is_some() && !is_hrule_para(p) => p.props.num_id,
            _ => None,
        };
        // Blank line between blocks, except between adjacent items of one list.
        let same_list = cur_list.is_some() && cur_list == prev_list;
        if prev_any && !same_list {
            out.push('\n');
        }
        match b {
            Block::Paragraph(p) => {
                para_to_md(p, markers.get(&vec![i]).map(String::as_str), &mut out)
            }
            Block::Table(t) => table_to_md(t, &mut out),
            Block::Raw(_) => {
                i += 1;
                continue;
            }
        }
        prev_list = cur_list;
        prev_any = true;
        i += 1;
    }
    out
}

/// Whether a paragraph carries the "SourceCode" style (a fenced code-block line).
fn is_source_code(p: &Paragraph) -> bool {
    p.props
        .style_id
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("SourceCode"))
}

/// Whether a paragraph carries the "Quote" style (a blockquote line).
fn is_quote(p: &Paragraph) -> bool {
    p.props
        .style_id
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("Quote"))
}

/// The LaTeX for an equation, or `None` if it isn't native OMML math (a legacy
/// Equation Editor object, which has no LaTeX form). Uses the stored LaTeX when
/// present, else derives it from the OMML.
fn equation_latex(raw: &str, latex: &Option<String>) -> Option<String> {
    if !raw.contains("m:oMath") {
        return None;
    }
    Some(
        latex
            .clone()
            .unwrap_or_else(|| crate::latex::omml_to_latex(raw)),
    )
}

/// If the paragraph is a standalone Mermaid diagram (its only content is a
/// SmartArt drawing carrying embedded Mermaid source), return that source.
fn mermaid_of(p: &Paragraph) -> Option<String> {
    if let [Inline::SmartArt { raw, .. }] = p.content.as_slice() {
        return crate::mermaid::source_of(raw);
    }
    None
}

/// If the paragraph is a standalone display-math block (its only content is an
/// `oMathPara` equation), return its LaTeX.
fn display_math_of(p: &Paragraph) -> Option<String> {
    if let [Inline::Equation { raw, latex, .. }] = p.content.as_slice() {
        if raw.contains("m:oMathPara") {
            return equation_latex(raw, latex);
        }
    }
    None
}

fn para_to_md(p: &Paragraph, marker: Option<&str>, out: &mut String) {
    if let Some(src) = mermaid_of(p) {
        out.push_str("```mermaid\n");
        out.push_str(&src);
        out.push('\n');
        out.push_str("```\n");
        return;
    }
    if let Some(latex) = display_math_of(p) {
        out.push_str("$$\n");
        out.push_str(&latex);
        out.push_str("\n$$\n");
        return;
    }
    if let Some(level) = heading_level_of(p) {
        out.push_str(&"#".repeat(level));
        out.push(' ');
        out.push_str(&inlines_to_md(&p.content));
        out.push('\n');
        return;
    }
    if is_hrule_para(p) {
        out.push_str("---\n");
        return;
    }
    if is_quote(p) {
        out.push_str("> ");
        out.push_str(&inlines_to_md(&p.content));
        out.push('\n');
        return;
    }
    let text = inlines_to_md(&p.content);
    if p.props.num_id.is_some() {
        let indent = "  ".repeat(p.props.ilvl.max(0) as usize);
        match marker {
            // A numeric marker ("1.", "2)") makes an ordered item; anything else
            // (bullets, letters, roman) renders as a dash bullet.
            Some(m) if m.starts_with(|c: char| c.is_ascii_digit()) => {
                out.push_str(&format!("{indent}{m} {text}\n"));
            }
            _ => out.push_str(&format!("{indent}- {text}\n")),
        }
        return;
    }
    out.push_str(&text);
    out.push('\n');
}

fn table_to_md(t: &Table, out: &mut String) {
    if t.rows.is_empty() {
        return;
    }
    let ncols = t
        .rows
        .iter()
        .map(|r| r.cells.len())
        .max()
        .unwrap_or(0)
        .max(1);
    let row_md = |cells: &[Cell]| -> String {
        let mut s = String::from("|");
        for c in 0..ncols {
            let text = cells.get(c).map(cell_to_md).unwrap_or_default();
            s.push(' ');
            s.push_str(&text);
            s.push_str(" |");
        }
        s.push('\n');
        s
    };
    out.push_str(&row_md(&t.rows[0].cells));
    // Header separator.
    out.push('|');
    for _ in 0..ncols {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in &t.rows[1..] {
        out.push_str(&row_md(&row.cells));
    }
}

fn cell_to_md(c: &Cell) -> String {
    let mut parts = Vec::new();
    for b in &c.blocks {
        if let Block::Paragraph(p) = b {
            parts.push(inlines_to_md(&p.content));
        }
    }
    // Pipes and newlines would break the row; flatten them.
    parts.join(" ").replace('|', "\\|").replace('\n', " ")
}

fn inlines_to_md(content: &[Inline]) -> String {
    let mut s = String::new();
    for inl in content {
        match inl {
            Inline::Run(r) => s.push_str(&run_to_md(&r.text, &r.props)),
            Inline::Hyperlink(h) => {
                let inner: String = h
                    .runs
                    .iter()
                    .map(|r| run_to_md(&r.text, &r.props))
                    .collect();
                let url = h
                    .target
                    .clone()
                    .or_else(|| h.anchor.as_ref().map(|a| format!("#{a}")))
                    .unwrap_or_default();
                if url.is_empty() {
                    s.push_str(&inner);
                } else {
                    s.push_str(&format!("[{inner}]({url})"));
                }
            }
            Inline::Break(_) => s.push_str("  \n"),
            Inline::Tab(_) => s.push('\t'),
            Inline::Equation { raw, text, latex } => match equation_latex(raw, latex) {
                // Native math → `$…$` (inline) or `$$…$$` (display) with LaTeX.
                Some(l) if raw.contains("m:oMathPara") => s.push_str(&format!("$${l}$$")),
                Some(l) => s.push_str(&format!("${l}$")),
                // Legacy Equation Editor object: fall back to its Unicode text.
                None => s.push_str(&escape_inline(text)),
            },
            Inline::Field { text, .. } => s.push_str(&escape_inline(text)),
            Inline::SmartArt { text, .. } => s.push_str(&escape_inline(&text.join(" "))),
            // Tracked change: emit the inner text (deletions as ~~struck~~).
            Inline::Revision { kind, content, .. } => {
                let text: String = content.iter().map(|i| i.text()).collect();
                match kind {
                    crate::model::RevisionKind::Delete => {
                        s.push_str(&format!("~~{}~~", escape_inline(&text)))
                    }
                    crate::model::RevisionKind::Insert => s.push_str(&escape_inline(&text)),
                }
            }
            // A footnote/endnote reference → a Markdown footnote marker.
            Inline::FootnoteRef { id, endnote, .. } => {
                let p = if *endnote { "e" } else { "" };
                s.push_str(&format!("[^{p}{id}]"))
            }
            Inline::Chart { .. } | Inline::TextBox { .. } | Inline::Raw(_) => {}
        }
    }
    s
}

fn run_to_md(text: &str, props: &RunProps) -> String {
    // Inline code is literal: wrap in a backtick fence (longer than any backtick
    // run inside) and do not escape or apply emphasis markers within.
    if props.code && !text.is_empty() {
        return code_span(text);
    }
    let esc = escape_inline(text);
    if text.trim().is_empty() {
        return esc; // never wrap pure whitespace in ** / * markers
    }
    let mut s = esc;
    if props.italic {
        s = format!("*{s}*");
    }
    if props.bold {
        s = format!("**{s}**");
    }
    if props.strike {
        s = format!("~~{s}~~");
    }
    s
}

/// Wrap `text` as a Markdown code span, using a backtick fence one longer than
/// the longest backtick run inside it (and padding with a space when the content
/// itself starts or ends with a backtick), per CommonMark.
fn code_span(text: &str) -> String {
    let mut longest = 0;
    let mut cur = 0;
    for c in text.chars() {
        if c == '`' {
            cur += 1;
            longest = longest.max(cur);
        } else {
            cur = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

fn escape_inline(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for c in text.chars() {
        // `[`/`]` are only special in link/image syntax, which `to_markdown`
        // emits through its own link path — not via this function. Escaping
        // bare brackets mangles task-list items (`- [ ]` -> `- \[ \]`) and other
        // literal bracket text, so they are left alone.
        // Accepted tradeoff: literal prose containing `[text](url)` (typed as
        // plain text, not created via the link path) re-parses as a real link
        // on the next round-trip. Deliberate, not an oversight — task-list
        // `[ ]`/`[x] ` never forms link syntax (no following `(url)`), and this
        // function never sees text the link path itself emitted (that path
        // brackets its own text independently), so the ambiguity is confined to
        // hand-typed bracket-paren prose, which is rare and recoverable (the
        // "link" just points nowhere useful, it doesn't lose or corrupt text).
        if matches!(c, '\\' | '*' | '`' | '~' | '|') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

fn heading_level_of(p: &Paragraph) -> Option<usize> {
    if let Some(n) = p.props.heading_level {
        return Some((n as usize).clamp(1, 6));
    }
    if let Some(id) = &p.props.style_id {
        if id.eq_ignore_ascii_case("Title") {
            return Some(1);
        }
        if let Some(n) = crate::load::heading_level(id) {
            return Some((n as usize).clamp(1, 6));
        }
    }
    None
}

fn is_hrule_para(p: &Paragraph) -> bool {
    p.props.borders.bottom.is_some() && p.plain_text().trim().is_empty()
}

// ===========================================================================
// Markdown -> Document
// ===========================================================================

/// Parse Markdown text into a [`Document`]. Always yields at least one paragraph.
/// List items use `numId` 1 (bullets) / 2 (ordered) — the ids defined by
/// [`crate::package::new_markdown_package`].
pub fn from_markdown(src: &str) -> Document {
    let lines: Vec<&str> = src.lines().collect();
    let mut body: Vec<Block> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        // Fenced code block. A `mermaid` info string becomes a Word diagram;
        // any other fence becomes verbatim "SourceCode" paragraphs (re-fenced on
        // output), so the block survives a round-trip.
        if let Some(fence) = code_fence(trimmed) {
            let lang = trimmed
                .trim_start_matches(fence)
                .trim()
                .to_ascii_lowercase();
            i += 1;
            let start = i;
            while i < lines.len() && code_fence(lines[i].trim()) != Some(fence) {
                i += 1;
            }
            let inner = &lines[start..i];
            if i < lines.len() {
                i += 1; // consume closing fence
            }
            if lang == "mermaid" {
                body.push(mermaid_para(&inner.join("\n")));
            } else {
                for l in inner {
                    body.push(source_para(l));
                }
            }
            continue;
        }
        // Display math block: `$$ … $$`, on one line or spanning several.
        if let Some(rest) = trimmed.strip_prefix("$$") {
            // Single-line `$$ … $$`.
            if let Some(one) = rest.strip_suffix("$$") {
                if !rest.is_empty() {
                    body.push(display_math_para(one.trim()));
                    i += 1;
                    continue;
                }
            }
            // Opener: gather lines until a closing `$$`.
            let mut content = String::new();
            if !rest.trim().is_empty() {
                content.push_str(rest.trim());
            }
            i += 1;
            while i < lines.len() {
                let lt = lines[i].trim();
                if let Some(before) = lt.strip_suffix("$$") {
                    if !before.trim().is_empty() {
                        if !content.is_empty() {
                            content.push(' ');
                        }
                        content.push_str(before.trim());
                    }
                    i += 1;
                    break;
                }
                if !content.is_empty() {
                    content.push(' ');
                }
                content.push_str(lt);
                i += 1;
            }
            body.push(display_math_para(content.trim()));
            continue;
        }
        // ATX heading.
        if let Some((level, text)) = atx_heading(trimmed) {
            body.push(heading_para(level, text));
            i += 1;
            continue;
        }
        // Thematic break.
        if is_thematic_break(trimmed) {
            body.push(hrule_para());
            i += 1;
            continue;
        }
        // Pipe table: this row plus a separator row beneath it.
        if trimmed.contains('|') && i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
            let start = i;
            i += 2; // header + separator
            while i < lines.len() && lines[i].trim().contains('|') && !lines[i].trim().is_empty() {
                i += 1;
            }
            let mut rows: Vec<&str> = vec![lines[start]];
            rows.extend(&lines[start + 2..i]);
            body.push(parse_table(&rows));
            continue;
        }
        // List (consecutive items; each item may span soft-wrapped/continued
        // lines that are not themselves new list markers or new blocks).
        if list_item(line).is_some() {
            while i < lines.len() {
                let Some((ilvl, ordered, first)) = list_item(lines[i]) else {
                    break;
                };
                i += 1;
                let mut text = first.to_string();
                // Fold in continuation lines belonging to THIS item.
                while i < lines.len() {
                    let l = lines[i];
                    let t = l.trim();
                    if t.is_empty()
                        || list_item(l).is_some()
                        || starts_block(l, lines.get(i + 1).copied())
                    {
                        break;
                    }
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(t);
                    i += 1;
                }
                body.push(list_para(ilvl, ordered, &text));
            }
            continue;
        }
        // Blockquote: strip the marker, render as an indented paragraph.
        if let Some(rest) = trimmed.strip_prefix('>') {
            body.push(quote_para(rest.trim_start()));
            i += 1;
            continue;
        }
        // Plain paragraph: gather soft-wrapped lines until a blank or a new block.
        let mut text = String::new();
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim();
            if t.is_empty() || starts_block(l, lines.get(i + 1).copied()) {
                break;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(t);
            i += 1;
        }
        body.push(
            Paragraph {
                props: ParProps::default(),
                content: parse_inlines(&text),
            }
            .into(),
        );
    }
    if body.is_empty() {
        body.push(Block::Paragraph(Paragraph::default()));
    }
    Document { body }
}

/// Whether `line` begins a block that should end the current plain paragraph.
fn starts_block(line: &str, next: Option<&str>) -> bool {
    let t = line.trim();
    atx_heading(t).is_some()
        || is_thematic_break(t)
        || code_fence(t).is_some()
        || t.starts_with("$$")
        || list_item(line).is_some()
        || t.starts_with('>')
        || (t.contains('|') && next.map(is_table_separator).unwrap_or(false))
}

fn code_fence(t: &str) -> Option<char> {
    ['`', '~']
        .into_iter()
        .find(|&f| t.starts_with(&f.to_string().repeat(3)))
}

fn atx_heading(t: &str) -> Option<(usize, &str)> {
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &t[hashes..];
        if rest.starts_with(' ') || rest.is_empty() {
            return Some((hashes, rest.trim().trim_end_matches('#').trim()));
        }
    }
    None
}

fn is_thematic_break(t: &str) -> bool {
    for c in ['-', '*', '_'] {
        let cleaned: String = t.chars().filter(|ch| !ch.is_whitespace()).collect();
        if cleaned.len() >= 3 && cleaned.chars().all(|ch| ch == c) {
            return true;
        }
    }
    false
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') {
        return false;
    }
    t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// Parse a list item: returns `(level, ordered, content)` or `None`.
fn list_item(line: &str) -> Option<(i32, bool, &str)> {
    let indent = line.len() - line.trim_start().len();
    let level = (indent / 2) as i32; // two spaces per nesting level
    let t = line.trim_start();
    // Unordered: `-`, `*`, `+` followed by a space.
    for m in ['-', '*', '+'] {
        if let Some(rest) = t.strip_prefix(m) {
            if rest.starts_with(' ') {
                // A lone `-` row is a thematic break, not a list.
                if is_thematic_break(t) {
                    return None;
                }
                return Some((level, false, rest.trim_start()));
            }
        }
    }
    // Ordered: digits then `.` or `)` then a space.
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let after = &t[digits..];
        if (after.starts_with('.') || after.starts_with(')')) && after[1..].starts_with(' ') {
            return Some((level, true, after[1..].trim_start()));
        }
    }
    None
}

fn parse_table(rows: &[&str]) -> Block {
    let cells_of = |row: &str| -> Vec<String> {
        let t = row.trim();
        let t = t.strip_prefix('|').unwrap_or(t);
        let t = t.strip_suffix('|').unwrap_or(t);
        split_table_row(t)
    };
    let parsed: Vec<Vec<String>> = rows.iter().map(|r| cells_of(r)).collect();
    let ncols = parsed.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
    let col_w = (9360 / ncols as u32).max(1);
    let mut out_rows = Vec::new();
    for r in parsed {
        let mut cells = Vec::new();
        for c in 0..ncols {
            let text = r.get(c).cloned().unwrap_or_default();
            cells.push(Cell {
                grid_span: 1,
                v_merge: crate::model::VMerge::None,
                blocks: vec![
                    Paragraph {
                        props: ParProps::default(),
                        content: parse_inlines(text.trim()),
                    }
                    .into(),
                ],
                ..Default::default()
            });
        }
        out_rows.push(Row {
            cells,
            ..Default::default()
        });
    }
    Block::Table(Table {
        grid: vec![col_w; ncols],
        rows: out_rows,
        ..Default::default()
    })
}

/// Split a table row body on unescaped `|`.
fn split_table_row(s: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

fn heading_para(level: usize, text: &str) -> Block {
    Paragraph {
        props: ParProps {
            heading_level: Some(level as u8),
            style_id: Some(format!("Heading{level}")),
            ..ParProps::default()
        },
        content: parse_inlines(text),
    }
    .into()
}

fn hrule_para() -> Block {
    Paragraph {
        props: ParProps {
            borders: crate::model::ParBorders {
                top: None,
                bottom: Some(BorderKind::Single),
            },
            ..ParProps::default()
        },
        content: Vec::new(),
    }
    .into()
}

fn list_para(ilvl: i32, ordered: bool, content: &str) -> Block {
    Paragraph {
        props: ParProps {
            num_id: Some(if ordered { 2 } else { 1 }),
            ilvl,
            ..ParProps::default()
        },
        content: parse_inlines(content),
    }
    .into()
}

fn quote_para(text: &str) -> Block {
    Paragraph {
        props: ParProps {
            // The "Quote" style is the round-trip marker; the direct indent keeps
            // it visually offset even when no styles.xml defines "Quote".
            style_id: Some("Quote".to_string()),
            indent: 360,
            ..ParProps::default()
        },
        content: parse_inlines(text),
    }
    .into()
}

/// A Mermaid diagram paragraph: holds a single SmartArt drawing generated from
/// the source (which is also embedded in the drawing for lossless recovery).
fn mermaid_para(src: &str) -> Block {
    let (raw, text) = crate::mermaid::to_drawing(src);
    Paragraph {
        props: ParProps::default(),
        content: vec![Inline::SmartArt { raw, text }],
    }
    .into()
}

/// A standalone display-math paragraph (`$$ … $$`): a paragraph whose only
/// content is a block (`oMathPara`) equation. [`to_markdown_with`] re-emits it as
/// a `$$`-fenced block.
fn display_math_para(latex: &str) -> Block {
    Paragraph {
        props: ParProps::default(),
        content: vec![math_equation(latex, true)],
    }
    .into()
}

/// One verbatim line of a fenced code block: a "SourceCode"-styled paragraph
/// holding the raw text (no inline parsing). [`to_markdown_with`] groups
/// consecutive such paragraphs back into a fence.
fn source_para(text: &str) -> Block {
    Paragraph {
        props: ParProps {
            style_id: Some("SourceCode".to_string()),
            ..ParProps::default()
        },
        content: vec![Inline::Run(Run {
            text: text.to_string(),
            props: RunProps::default(),
        })],
    }
    .into()
}

/// Parse inline Markdown into runs/links. Supports `**bold**`, `*italic*`,
/// `~~strike~~`, `` `code` ``, `[text](url)`, and backslash escapes.
fn parse_inlines(s: &str) -> Vec<Inline> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '[' {
            if let Some((label, url, adv)) = parse_link(&chars, i) {
                push_run(&mut out, &mut buf, bold, italic, strike);
                out.push(Inline::Hyperlink(Hyperlink {
                    target: Some(url),
                    anchor: None,
                    rel_id: None,
                    runs: vec![Run {
                        text: label,
                        props: RunProps {
                            bold,
                            italic,
                            strike,
                            ..RunProps::default()
                        },
                    }],
                }));
                i += adv;
                continue;
            }
        }
        if c == '`' {
            // A code span opens with a run of N backticks and closes with the next
            // run of exactly N backticks (CommonMark), so backticks can appear
            // inside a longer fence.
            let n = chars[i..].iter().take_while(|&&ch| ch == '`').count();
            if let Some((content_end, close_end)) = find_code_close(&chars, i + n, n) {
                push_run(&mut out, &mut buf, bold, italic, strike);
                let mut code: String = chars[i + n..content_end].iter().collect();
                // Strip one surrounding space when present on both ends (lets a
                // span hold leading/trailing backticks): `` `a` `` → `a`.
                if code.len() >= 2 && code.starts_with(' ') && code.ends_with(' ') {
                    code = code[1..code.len() - 1].to_string();
                }
                out.push(Inline::Run(Run {
                    text: code,
                    props: RunProps {
                        code: true,
                        ..RunProps::default()
                    },
                }));
                i = close_end;
                continue;
            }
        }
        // Inline math: `$x^2$` (or display `$$…$$`). The delimiters must hug
        // their content (no inner space at the edges), so prose dollar signs like
        // "$5 and $10" aren't mistaken for math. Literal `\$` is handled above.
        if c == '$' {
            let n = chars[i..]
                .iter()
                .take_while(|&&ch| ch == '$')
                .count()
                .min(2);
            if let Some(close) = find_math_close(&chars, i + n, n) {
                push_run(&mut out, &mut buf, bold, italic, strike);
                let latex: String = chars[i + n..close].iter().collect();
                out.push(math_equation(latex.trim(), n == 2));
                i = close + n;
                continue;
            }
        }
        if c == '~' && chars.get(i + 1) == Some(&'~') {
            push_run(&mut out, &mut buf, bold, italic, strike);
            strike = !strike;
            i += 2;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            push_run(&mut out, &mut buf, bold, italic, strike);
            bold = !bold;
            i += 2;
            continue;
        }
        if c == '*' {
            push_run(&mut out, &mut buf, bold, italic, strike);
            italic = !italic;
            i += 1;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    push_run(&mut out, &mut buf, bold, italic, strike);
    if out.is_empty() {
        out.push(Inline::Run(Run::default()));
    }
    out
}

/// Find the closing run of `n` dollar signs for inline math opened at `from`.
/// Returns the index of the first closing `$`. The content must be non-empty and
/// neither start nor end with whitespace (so currency in prose isn't math).
fn find_math_close(chars: &[char], from: usize, n: usize) -> Option<usize> {
    if chars.get(from).is_some_and(|c| c.is_whitespace()) {
        return None; // opener immediately followed by space → not math
    }
    let mut j = from;
    while j < chars.len() {
        if chars[j] == '$' {
            let run = chars[j..].iter().take_while(|&&c| c == '$').count();
            if run >= n {
                // Need real content and no space hugging the closing delimiter.
                if j > from && !chars[j - 1].is_whitespace() {
                    return Some(j);
                }
                return None;
            }
        }
        j += 1;
    }
    None
}

/// Build an equation inline from LaTeX: generate OMML (so it saves as real Word
/// math), render Unicode for the terminal, and keep the LaTeX for exact Markdown
/// round-trips. `display` selects `$$…$$` (block) over `$…$` (inline).
fn math_equation(latex: &str, display: bool) -> Inline {
    let raw = crate::latex::latex_to_omml(latex, display);
    let text = crate::omath::render_omath(&raw);
    Inline::Equation {
        raw,
        text,
        latex: Some(latex.to_string()),
    }
}

fn push_run(out: &mut Vec<Inline>, buf: &mut String, bold: bool, italic: bool, strike: bool) {
    if !buf.is_empty() {
        out.push(Inline::Run(Run {
            text: std::mem::take(buf),
            props: RunProps {
                bold,
                italic,
                strike,
                ..RunProps::default()
            },
        }));
    }
}

/// Parse `[label](url)` starting at `chars[start] == '['`. Returns
/// `(label, url, chars_consumed)`.
fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = (start + 1..chars.len()).find(|&j| chars[j] == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let url_end = (close + 2..chars.len()).find(|&j| chars[j] == ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    let url: String = chars[close + 2..url_end].iter().collect();
    Some((label, url, url_end + 1 - start))
}

/// Find the closing fence of a code span: the next run of *exactly* `n` backticks
/// at or after `from`. Returns `(content_end, close_end)` — the start of the
/// closing fence and the index just past it — or `None` if unterminated.
fn find_code_close(chars: &[char], from: usize, n: usize) -> Option<(usize, usize)> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == '`' {
            let run = chars[j..].iter().take_while(|&&c| c == '`').count();
            if run == n {
                return Some((j, j + run));
            }
            j += run;
        } else {
            j += 1;
        }
    }
    None
}

// Small ergonomics: build a Block straight from a Paragraph.
impl From<Paragraph> for Block {
    fn from(p: Paragraph) -> Block {
        Block::Paragraph(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, bold: bool, italic: bool) -> Inline {
        Inline::Run(Run {
            text: text.to_string(),
            props: RunProps {
                bold,
                italic,
                ..RunProps::default()
            },
        })
    }

    #[test]
    fn headings_round_trip() {
        let doc = from_markdown("# Title\n\n## Sub");
        assert_eq!(doc.body.len(), 2);
        match &doc.body[0] {
            Block::Paragraph(p) => {
                assert_eq!(p.props.heading_level, Some(1));
                assert_eq!(p.plain_text(), "Title");
            }
            _ => panic!("expected paragraph"),
        }
        let md = to_markdown(&doc);
        assert!(md.contains("# Title"));
        assert!(md.contains("## Sub"));
    }

    #[test]
    fn emphasis_parses_and_serializes() {
        let doc = from_markdown("This is **bold** and *italic*.");
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        assert!(
            p.content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.props.bold))
        );
        assert!(
            p.content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.props.italic))
        );
        let md = to_markdown(&doc);
        assert!(md.contains("**bold**"), "{md}");
        assert!(md.contains("*italic*"), "{md}");
    }

    #[test]
    fn strikethrough_and_code() {
        let doc = from_markdown("~~gone~~ and `code`.");
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        assert!(
            p.content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.props.strike))
        );
        assert!(to_markdown(&doc).contains("~~gone~~"));
    }

    #[test]
    fn links_round_trip() {
        let doc = from_markdown("See [docs](https://example.com/x).");
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        let link = p
            .content
            .iter()
            .find_map(|i| match i {
                Inline::Hyperlink(h) => Some(h),
                _ => None,
            })
            .expect("a link");
        assert_eq!(link.target.as_deref(), Some("https://example.com/x"));
        assert_eq!(link.runs[0].text, "docs");
        assert!(to_markdown(&doc).contains("[docs](https://example.com/x)"));
    }

    #[test]
    fn bullet_and_ordered_lists() {
        let doc = from_markdown("- one\n- two\n\n1. first\n2. second");
        let lists: Vec<_> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) if p.props.num_id.is_some() => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(lists.len(), 4);
        assert_eq!(lists[0].props.num_id, Some(1)); // bullet
        assert_eq!(lists[2].props.num_id, Some(2)); // ordered
    }

    #[test]
    fn adjacent_lists_of_different_kinds_are_separated() {
        // A bullet list then an ordered list: a blank line keeps them distinct,
        // but items within each list stay packed together. Ordered markers come
        // from a marker map, as the app supplies via compute_markers.
        let doc = from_markdown("- a\n  - b\n\n1. one\n2. two");
        let mut markers = HashMap::new();
        markers.insert(vec![2], "1.".to_string());
        markers.insert(vec![3], "2.".to_string());
        let md = to_markdown_with(&doc, &markers);
        assert_eq!(md, "- a\n  - b\n\n1. one\n2. two\n", "{md:?}");
        // The blank line is exactly the list boundary: items within a list pack.
        assert!(!md.contains("- a\n\n  - b"));
    }

    #[test]
    fn nested_list_levels() {
        let doc = from_markdown("- a\n  - b\n    - c");
        let lvls: Vec<i32> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) if p.props.num_id.is_some() => Some(p.props.ilvl),
                _ => None,
            })
            .collect();
        assert_eq!(lvls, vec![0, 1, 2]);
    }

    #[test]
    fn list_item_continuation_lines_merge_into_the_item() {
        // The second line is an indented soft-wrap of the first item, not a new block.
        let src = "- first line\n  still the first item\n- second item\n";
        let doc = from_markdown(src);
        // Exactly two list paragraphs, and the first carries both lines' text.
        let paras: Vec<&Paragraph> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p),
                _ => None,
            })
            .filter(|p| p.props.num_id.is_some())
            .collect();
        assert_eq!(paras.len(), 2, "two list items, not three blocks");
        assert_eq!(paras[0].plain_text(), "first line still the first item");
        // And it round-trips without inserting a blank line inside the list.
        let out = to_markdown(&doc);
        assert!(
            !out.contains("- first line\n\n"),
            "no spurious blank inside the list: {out:?}"
        );
    }

    #[test]
    fn empty_marker_continuation_has_no_leading_space() {
        // An empty first line (`"- "`, nothing after the marker) followed by a
        // continuation: the fold loop must guard the space the same way the
        // plain-paragraph gather does (`if !text.is_empty()`), or the item's
        // text picks up a spurious leading space before "cont".
        let src = "- \n  cont\n";
        let doc = from_markdown(src);
        let paras: Vec<&Paragraph> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p),
                _ => None,
            })
            .filter(|p| p.props.num_id.is_some())
            .collect();
        assert_eq!(paras.len(), 1, "one list item");
        assert_eq!(
            paras[0].plain_text(),
            "cont",
            "no leading space before cont"
        );
    }

    #[test]
    fn ordered_list_marker_kept_on_output() {
        // Simulate compute_markers output for two ordered items.
        let doc = from_markdown("1. first\n2. second");
        let mut markers = HashMap::new();
        markers.insert(vec![0], "1.".to_string());
        markers.insert(vec![1], "2.".to_string());
        let md = to_markdown_with(&doc, &markers);
        assert!(md.contains("1. first"), "{md}");
        assert!(md.contains("2. second"), "{md}");
    }

    #[test]
    fn thematic_break_maps_to_rule() {
        let doc = from_markdown("a\n\n---\n\nb");
        assert!(doc.body.iter().any(|b| matches!(
            b,
            Block::Paragraph(p) if p.props.borders.bottom.is_some()
        )));
        assert!(to_markdown(&doc).contains("---"));
    }

    #[test]
    fn pipe_table_round_trips() {
        let md = "| A | B |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |";
        let doc = from_markdown(md);
        let table = doc
            .body
            .iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .expect("a table");
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[2].cells[1].blocks[0].plain_text(), "4");
        let out = to_markdown(&doc);
        assert!(out.contains("| A | B |"), "{out}");
        assert!(out.contains("| --- | --- |"), "{out}");
    }

    #[test]
    fn soft_wrapped_lines_join() {
        let doc = from_markdown("one\ntwo\nthree");
        assert_eq!(doc.body.len(), 1);
        assert_eq!(doc.body[0].plain_text(), "one two three");
    }

    #[test]
    fn escaping_is_reversible() {
        let doc = from_markdown(r"literal \*not italic\* and \[brackets\]");
        let text = doc.body[0].plain_text();
        assert_eq!(text, "literal *not italic* and [brackets]");
    }

    #[test]
    fn empty_input_yields_one_paragraph() {
        let doc = from_markdown("");
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn inline_code_round_trips() {
        let doc = from_markdown("use `let x = 1;` here");
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        assert!(
            p.content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.props.code && r.text == "let x = 1;")),
            "{:?}",
            p.content
        );
        let md = to_markdown(&doc);
        assert!(md.contains("`let x = 1;`"), "{md}");
        // A backtick inside the span widens the fence (CommonMark) and parses back.
        let doc2 = from_markdown("a ``b`c`` d");
        let md2 = to_markdown(&doc2);
        assert_eq!(from_markdown(&md2).body[0].plain_text(), "a b`c d");
    }

    #[test]
    fn blockquote_round_trips() {
        let doc = from_markdown("> quoted line");
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        assert_eq!(p.props.style_id.as_deref(), Some("Quote"));
        assert_eq!(p.plain_text(), "quoted line");
        assert!(to_markdown(&doc).contains("> quoted line"));
    }

    #[test]
    fn fenced_code_block_round_trips() {
        let src = "```\nlet x = 1;\nlet y = 2;\n```";
        let doc = from_markdown(src);
        // Two SourceCode paragraphs, verbatim.
        let code: Vec<_> = doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) if is_source_code(p) => Some(p.plain_text()),
                _ => None,
            })
            .collect();
        assert_eq!(code, vec!["let x = 1;", "let y = 2;"]);
        // Re-emitted as one fence (not separate paragraphs), and re-parses identically.
        let md = to_markdown(&doc);
        assert!(md.contains("```\nlet x = 1;\nlet y = 2;\n```"), "{md}");
        let again = from_markdown(&md);
        assert_eq!(again, doc);
    }

    #[test]
    fn inline_math_round_trips_and_makes_omml() {
        let doc = from_markdown("Einstein: $E=mc^2$ changed physics.");
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        // Parsed to a native-math equation carrying OMML + the original LaTeX.
        let eq = p
            .content
            .iter()
            .find_map(|i| match i {
                Inline::Equation { raw, latex, .. } => Some((raw, latex)),
                _ => None,
            })
            .expect("an equation inline");
        assert!(
            eq.0.contains("<m:oMath>") && eq.0.contains("<m:sSup>"),
            "{}",
            eq.0
        );
        assert_eq!(eq.1.as_deref(), Some("E=mc^2"));
        // Re-emitted as `$…$` and re-parses to the same document.
        let md = to_markdown(&doc);
        assert!(md.contains("$E=mc^2$"), "{md}");
        assert_eq!(from_markdown(&md), doc);
    }

    #[test]
    fn display_math_block_round_trips() {
        let src = "$$\nx=\\frac{-b\\pm \\sqrt{b^{2}-4ac}}{2a}\n$$";
        let doc = from_markdown(src);
        // One paragraph holding a display (oMathPara) equation.
        let p = match &doc.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        match p.content.as_slice() {
            [Inline::Equation { raw, .. }] => assert!(raw.contains("<m:oMathPara>"), "{raw}"),
            other => panic!("expected one display equation, got {other:?}"),
        }
        let md = to_markdown(&doc);
        assert!(
            md.contains("$$\nx=\\frac{-b\\pm \\sqrt{b^{2}-4ac}}{2a}\n$$"),
            "{md}"
        );
        assert_eq!(from_markdown(&md), doc);
    }

    #[test]
    fn math_survives_a_docx_round_trip() {
        // Markdown math → package → .docx bytes → reload → Markdown again.
        let doc = from_markdown("Mass-energy: $E=mc^2$.\n\n$$\n\\frac{a}{b}\n$$");
        let pkg = crate::package::new_markdown_package(doc);
        let bytes = crate::package::save_package(&pkg);
        let reloaded = crate::load::load(&bytes).expect("reload");
        // The inline and display equations both came back as OMML.
        let omml: Vec<&str> = reloaded
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => p.content.iter().find_map(|i| match i {
                    Inline::Equation { raw, .. } => Some(raw.as_str()),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert!(
            omml.iter().any(|r| r.contains("<m:sSup>")),
            "inline sup lost: {omml:?}"
        );
        assert!(
            omml.iter().any(|r| r.contains("<m:f>")),
            "fraction lost: {omml:?}"
        );
        // And exporting to Markdown again yields `$…$` / `$$…$$` with the LaTeX.
        let md = to_markdown(&reloaded);
        assert!(md.contains("$E=mc^{2}$"), "{md}");
        assert!(md.contains("$$\n\\frac{a}{b}\n$$"), "{md}");
    }

    #[test]
    fn mermaid_block_becomes_a_diagram_and_round_trips() {
        let src = "flowchart TD\nA[Start] --> B{OK?}\nB -->|yes| C[Done]\nB -->|no| A";
        let md = format!("# Diagram\n\n```mermaid\n{src}\n```\n");
        let doc = from_markdown(&md);
        // The fence became a SmartArt drawing carrying the embedded source.
        let sa = doc
            .body
            .iter()
            .find_map(|b| match b {
                Block::Paragraph(p) => p.content.iter().find_map(|i| match i {
                    Inline::SmartArt { raw, .. } => Some(raw),
                    _ => None,
                }),
                _ => None,
            })
            .expect("a mermaid SmartArt");
        assert!(sa.contains("<w:drawing>") && sa.contains("wordprocessingGroup"));
        assert!(sa.contains("descr=\"mermaid:"));
        // Re-emitted as a ```mermaid fence with the exact original source.
        let out = to_markdown(&doc);
        assert!(out.contains(&format!("```mermaid\n{src}\n```")), "{out}");
        // And it parses back identically (the source is the source of truth).
        assert_eq!(from_markdown(&out), doc);
    }

    #[test]
    fn mermaid_survives_a_docx_round_trip() {
        let src = "graph LR\nA[One] --> B[Two]";
        let doc = from_markdown(&format!("```mermaid\n{src}\n```"));
        let pkg = crate::package::new_markdown_package(doc);
        let bytes = crate::package::save_package(&pkg);
        let reloaded = crate::load::load(&bytes).expect("reload");
        // Reloaded as a SmartArt whose embedded source recovers the diagram.
        let md = to_markdown(&reloaded);
        assert!(md.contains(&format!("```mermaid\n{src}\n```")), "{md}");
    }

    #[test]
    fn non_mermaid_fence_stays_source_code() {
        let doc = from_markdown("```rust\nlet x = 1;\n```");
        // Not a diagram — still SourceCode paragraphs.
        assert!(doc.body.iter().all(|b| !matches!(
            b,
            Block::Paragraph(p) if p.content.iter().any(|i| matches!(i, Inline::SmartArt { .. }))
        )));
        assert!(to_markdown(&doc).contains("```\nlet x = 1;\n```"));
    }

    #[test]
    fn lone_dollar_signs_are_not_math() {
        // Currency in prose must not be captured as an equation.
        let doc = from_markdown("It cost $5 and then $10 total.");
        assert!(!doc.body[0].plain_text().is_empty(), "text preserved");
        assert!(
            !matches!(&doc.body[0], Block::Paragraph(p)
                if p.content.iter().any(|i| matches!(i, Inline::Equation { .. }))),
            "no equation should be parsed from prose dollars"
        );
        assert!(to_markdown(&doc).contains("$5 and then $10"));
    }

    #[test]
    fn combined_bold_italic() {
        let para = Paragraph {
            props: ParProps::default(),
            content: vec![run("x", true, true)],
        };
        let doc = Document {
            body: vec![para.into()],
        };
        let md = to_markdown(&doc);
        assert!(md.contains("***x***"), "{md}");
        // And it parses back to a bold+italic run.
        let back = from_markdown(&md);
        let p = match &back.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!(),
        };
        assert!(
            p.content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.props.bold && r.props.italic))
        );
    }

    #[test]
    fn task_list_round_trips_as_literal_text() {
        let src = "- [ ] todo\n- [x] done\n";
        let out = to_markdown(&from_markdown(src));
        assert!(
            out.contains("- [ ] todo"),
            "unchecked task survives: {out:?}"
        );
        assert!(out.contains("- [x] done"), "checked task survives: {out:?}");
        assert!(!out.contains("\\["), "brackets are not escaped: {out:?}");
    }

    /// Run one lap of the editor's real save path: parse Markdown, splice into
    /// a `.docx` package (this is what assigns/reserves numbering ids), save
    /// to bytes, reload (so numbering.xml is parsed back the way a real open
    /// would see it), then serialize back to Markdown using the *marker-aware*
    /// `to_markdown_with` + `compute_markers` — the same pair the editor's
    /// save path uses (see `docxwasm/src/bridge.rs` and `docxy/src/main.rs`).
    fn editor_round_trip(md: &str) -> String {
        let doc = from_markdown(md);
        let pkg = crate::package::new_markdown_package(doc);
        let bytes = crate::package::save_package(&pkg);
        let reloaded = crate::package::load_package(&bytes).expect("reload");
        let numbering = reloaded
            .part("word/numbering.xml")
            .map(|b| {
                crate::numbering::parse_numbering_xml(
                    std::str::from_utf8(b).expect("numbering.xml is utf8"),
                )
            })
            .unwrap_or_default();
        let markers = crate::numbering::compute_markers(&reloaded.document, &numbering);
        to_markdown_with(&reloaded.document, &markers)
    }

    #[test]
    fn markdown_round_trip_is_idempotent_over_the_corpus() {
        // Written in docxy's own canonical output style (one line per paragraph,
        // two-space list nesting) so the FIRST pass is already a fixed point.
        // Exercised through the editor's real save path (marker-aware), not the
        // bare `to_markdown`, so ordered-list markers are preserved as they are
        // in an actual `.md` file save.
        let corpus = "\
# Heading 1

## Heading 2

A paragraph with **bold**, *italic*, ~~strike~~, `code`, and a [link](https://x).

- bullet one
- bullet two
  continued on a soft-wrapped line
  - nested bullet
- [ ] todo
- [x] done

1. first
2. second

> a quote

```
code block
```

| a | b |
| --- | --- |
| 1 | 2 |

---
";
        let once = editor_round_trip(corpus);
        let twice = editor_round_trip(&once);
        assert_eq!(once, twice, "second pass must equal the first (idempotent)");
        // Spot-check the constructs survive the FIRST pass.
        for needle in [
            "# Heading 1",
            "**bold**",
            "~~strike~~",
            "`code`",
            "[link](https://x)",
            "continued on a soft-wrapped line",
            "nested bullet",
            "- [ ] todo",
            "- [x] done",
            "1. first",
            "> a quote",
            "| a | b |",
            "---",
        ] {
            assert!(once.contains(needle), "corpus lost {needle:?}:\n{once}");
        }
    }

    #[test]
    fn escape_inline_still_escapes_real_metacharacters_but_not_brackets() {
        // `*` `` ` `` `~` `|` `\` still escape; `[` `]` do not.
        let e = escape_inline("a*b`c~d|e[f]g\\h");
        assert!(
            e.contains("\\*")
                && e.contains("\\`")
                && e.contains("\\~")
                && e.contains("\\|")
                && e.contains("\\\\")
        );
        assert!(
            !e.contains("\\[") && !e.contains("\\]"),
            "brackets not escaped: {e:?}"
        );
    }
}
