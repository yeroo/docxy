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
//!
//! Underline and `_emphasis_` are intentionally *not* parsed: Markdown has no
//! underline, and treating `_` as emphasis mangles `snake_case`/URLs. The list
//! markers used on output come from the document's numbering when available, so a
//! real `.docx` exported to Markdown keeps its ordered vs. bulleted lists.

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
    let mut prev_is_list = false;
    let mut prev_any = false;
    for (i, b) in doc.body.iter().enumerate() {
        let is_list =
            matches!(b, Block::Paragraph(p) if p.props.num_id.is_some() && !is_hrule_para(p));
        // Blank line between blocks, except between adjacent list items.
        if prev_any && !(prev_is_list && is_list) {
            out.push('\n');
        }
        match b {
            Block::Paragraph(p) => {
                para_to_md(p, markers.get(&vec![i]).map(String::as_str), &mut out)
            }
            Block::Table(t) => table_to_md(t, &mut out),
            Block::Raw(_) => continue,
        }
        prev_is_list = is_list;
        prev_any = true;
    }
    out
}

fn para_to_md(p: &Paragraph, marker: Option<&str>, out: &mut String) {
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
            Inline::Field { text, .. } | Inline::Equation { text, .. } => {
                s.push_str(&escape_inline(text))
            }
            Inline::SmartArt { text, .. } => s.push_str(&escape_inline(&text.join(" "))),
            Inline::Chart { .. } | Inline::TextBox { .. } | Inline::Raw(_) => {}
        }
    }
    s
}

fn run_to_md(text: &str, props: &RunProps) -> String {
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

fn escape_inline(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`' | '[' | ']' | '~' | '|') {
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
        // Fenced code block: keep each inner line as a plain paragraph.
        if let Some(fence) = code_fence(trimmed) {
            i += 1;
            while i < lines.len() && code_fence(lines[i].trim()) != Some(fence) {
                body.push(plain_para(lines[i]));
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume closing fence
            }
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
        // List (consecutive items, possibly nested by indentation).
        if list_item(line).is_some() {
            while i < lines.len() {
                match list_item(lines[i]) {
                    Some((ilvl, ordered, content)) => {
                        body.push(list_para(ilvl, ordered, content));
                        i += 1;
                    }
                    None => break,
                }
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
            });
        }
        out_rows.push(Row { cells });
    }
    Block::Table(Table {
        grid: vec![col_w; ncols],
        rows: out_rows,
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
            indent: 360,
            ..ParProps::default()
        },
        content: parse_inlines(text),
    }
    .into()
}

fn plain_para(text: &str) -> Block {
    Paragraph {
        props: ParProps::default(),
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
            if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                push_run(&mut out, &mut buf, bold, italic, strike);
                let code: String = chars[i + 1..end].iter().collect();
                out.push(Inline::Run(Run {
                    text: code,
                    props: RunProps::default(),
                }));
                i = end + 1;
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
}
