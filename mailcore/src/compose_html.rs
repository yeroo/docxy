//! RichText <-> email HTML conversion for compose.
//!
//! [`to_html`] serializes an `editcore::RichText` (the compose editor's
//! buffer) into email-safe HTML for sending. [`from_html`] does the
//! reverse: parsing an existing message/draft body HTML into a `RichText`
//! so a reply/forward quote can be loaded into the editor. [`from_html`]
//! reuses `crate::htmlrender`'s tokenizer (`Tokenizer`/`Token`, made
//! `pub(crate)` there) rather than re-scanning HTML from scratch.

use crate::htmlrender::{SKIP_CONTENT_TAGS, Token, Tokenizer};
use editcore::model::{Block, RichText, Run};

/// Serializes a `RichText` document to email-safe HTML: one `<p>` per
/// paragraph, `<b>/<i>/<u>` nested (in that order) for emphasis with
/// adjacent same-style runs coalesced into a single tagged span, `<a
/// href>` for links, and consecutive `ListItem`s of the same `ordered`
/// grouped into a single `<ul>`/`<ol>` (one `<li>` per item). All text is
/// entity-escaped.
pub fn to_html(rt: &RichText) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < rt.blocks.len() {
        match &rt.blocks[i] {
            Block::Paragraph(runs) => {
                out.push_str("<p>");
                out.push_str(&render_runs(runs));
                out.push_str("</p>");
                i += 1;
            }
            Block::ListItem { ordered, .. } => {
                let ordered = *ordered;
                let tag = if ordered { "ol" } else { "ul" };
                out.push('<');
                out.push_str(tag);
                out.push('>');
                while let Some(Block::ListItem {
                    ordered: o,
                    runs,
                    level: _,
                }) = rt.blocks.get(i)
                {
                    if *o != ordered {
                        break;
                    }
                    out.push_str("<li>");
                    out.push_str(&render_runs(runs));
                    out.push_str("</li>");
                    i += 1;
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
    out
}

/// Flattens a `RichText` to its plain-text alternative (`rt.plain()`).
pub fn to_text(rt: &RichText) -> String {
    rt.plain()
}

/// Renders one block's runs: adjacent runs sharing the same style/link are
/// coalesced into a single tagged span (so `<b>` isn't closed and reopened
/// between two bold runs), then wrapped `<a>` outermost, `<b>`, `<i>`,
/// `<u>` innermost, around the entity-escaped text.
fn render_runs(runs: &[Run]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < runs.len() {
        let r = &runs[i];
        let (bold, italic, underline, link) = (r.bold, r.italic, r.underline, r.link.clone());
        let mut text = String::new();
        while i < runs.len() {
            let r2 = &runs[i];
            if r2.bold != bold
                || r2.italic != italic
                || r2.underline != underline
                || r2.link != link
            {
                break;
            }
            text.push_str(&r2.text);
            i += 1;
        }
        let mut piece = escape_html(&text);
        if underline {
            piece = format!("<u>{piece}</u>");
        }
        if italic {
            piece = format!("<i>{piece}</i>");
        }
        if bold {
            piece = format!("<b>{piece}</b>");
        }
        if let Some(href) = &link {
            piece = format!("<a href=\"{}\">{piece}</a>", escape_html(href));
        }
        out.push_str(&piece);
    }
    out
}

/// Entity-escapes the handful of characters that matter in email HTML text
/// content and (quoted) attribute values: `& < > "`.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Which kind of block is currently accumulating runs in [`from_html`].
#[derive(Clone, Copy)]
enum BlockKind {
    Para,
    Item { ordered: bool, level: u8 },
}

/// Parses message/draft body HTML into an editable `RichText`, so a
/// reply/forward quote can be loaded into the compose editor. Walks
/// `crate::htmlrender`'s tokenizer directly (rather than a second parser),
/// mapping its token stream to blocks/runs: `<p>`/`<br>` are paragraph
/// boundaries, `<b>/<strong>`, `<i>/<em>`, `<u>` toggle `Run` flags,
/// `<a href>` sets `Run::link`, and `<ul>/<ol>` + `<li>` become
/// `Block::ListItem`s. Unknown tags are transparent (their text still
/// flows into the current block); `<script>/<style>/<head>/<title>`
/// subtrees are dropped, mirroring `htmlrender::render_html`. Never
/// panics on malformed input — the tokenizer itself already guarantees
/// termination without panicking (see its doc comment and
/// `htmlrender::malformed_html_does_not_panic`), and this function does no
/// indexing/unwrapping that could fail on top of that.
pub fn from_html(html: &str) -> RichText {
    let mut tok = Tokenizer::new(html);
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur_runs: Vec<Run> = Vec::new();
    let mut kind = BlockKind::Para;
    let mut bold: u32 = 0;
    let mut italic: u32 = 0;
    let mut underline: u32 = 0;
    // Href of each open `<a>`, possibly empty (no href attribute).
    let mut link_stack: Vec<String> = Vec::new();
    // `ordered` flag of each open `<ul>`/`<ol>`.
    let mut list_stack: Vec<bool> = Vec::new();
    let mut skip: Option<(String, u32)> = None;

    loop {
        let token = tok.next();
        if let Some((tag, depth)) = &mut skip {
            match &token {
                Token::TagOpen { name, .. } if name == tag => *depth += 1,
                Token::TagClose { name } if name == tag => {
                    *depth -= 1;
                    if *depth == 0 {
                        skip = None;
                    }
                }
                Token::Eof => break,
                _ => {}
            }
            continue;
        }
        match token {
            Token::Eof => break,
            Token::Text(t) => {
                let link = current_link(&link_stack);
                push_run(
                    &mut cur_runs,
                    &normalize_ws(&t),
                    bold > 0,
                    italic > 0,
                    underline > 0,
                    link,
                );
            }
            Token::TagOpen { name, attrs, .. } => {
                if SKIP_CONTENT_TAGS.contains(&name.as_str()) {
                    skip = Some((name, 1));
                    continue;
                }
                match name.as_str() {
                    "b" | "strong" => bold += 1,
                    "i" | "em" => italic += 1,
                    "u" => underline += 1,
                    "br" => {
                        let link = current_link(&link_stack);
                        push_run(
                            &mut cur_runs,
                            "\n",
                            bold > 0,
                            italic > 0,
                            underline > 0,
                            link,
                        );
                    }
                    "p" => {
                        flush_block(&mut blocks, &mut cur_runs, kind);
                        kind = BlockKind::Para;
                    }
                    "ul" | "ol" => {
                        flush_block(&mut blocks, &mut cur_runs, kind);
                        kind = BlockKind::Para;
                        list_stack.push(name == "ol");
                    }
                    "li" => {
                        flush_block(&mut blocks, &mut cur_runs, kind);
                        let ordered = *list_stack.last().unwrap_or(&false);
                        let level = list_stack.len().saturating_sub(1) as u8;
                        kind = BlockKind::Item { ordered, level };
                    }
                    "a" => {
                        let href = attrs
                            .iter()
                            .find(|(k, _)| k == "href")
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default();
                        link_stack.push(href);
                    }
                    _ => {}
                }
            }
            Token::TagClose { name } => match name.as_str() {
                "b" | "strong" => bold = bold.saturating_sub(1),
                "i" | "em" => italic = italic.saturating_sub(1),
                "u" => underline = underline.saturating_sub(1),
                "p" => {
                    flush_block(&mut blocks, &mut cur_runs, kind);
                    kind = BlockKind::Para;
                }
                "ul" | "ol" => {
                    flush_block(&mut blocks, &mut cur_runs, kind);
                    kind = BlockKind::Para;
                    list_stack.pop();
                }
                "li" => {
                    flush_block(&mut blocks, &mut cur_runs, kind);
                    kind = BlockKind::Para;
                }
                "a" => {
                    link_stack.pop();
                }
                _ => {}
            },
        }
    }
    flush_block(&mut blocks, &mut cur_runs, kind);

    if blocks.is_empty() {
        RichText::new()
    } else {
        RichText { blocks }
    }
}

/// The innermost non-empty href on the open-`<a>` stack, if any.
fn current_link(link_stack: &[String]) -> Option<String> {
    link_stack.iter().rev().find(|h| !h.is_empty()).cloned()
}

/// Pushes `text` onto `cur_runs` with the given style/link, merging into
/// the previous run when it shares the same style/link (so e.g. two
/// adjacent bold text tokens end up as one `Run`, not two). A no-op for
/// empty text (block-tag housekeeping produces plenty of these).
fn push_run(
    cur_runs: &mut Vec<Run>,
    text: &str,
    bold: bool,
    italic: bool,
    underline: bool,
    link: Option<String>,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = cur_runs.last_mut() {
        if last.bold == bold
            && last.italic == italic
            && last.underline == underline
            && last.link == link
        {
            last.text.push_str(text);
            return;
        }
    }
    cur_runs.push(Run {
        text: text.to_string(),
        bold,
        italic,
        underline,
        link,
    });
}

/// Collapses any run of whitespace (including newlines/tabs from
/// pretty-printed source HTML) to a single space, mirroring ordinary HTML
/// whitespace handling. Applied per text token, not across tag boundaries.
fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_ws {
                out.push(' ');
            }
            last_ws = true;
        } else {
            out.push(c);
            last_ws = false;
        }
    }
    out
}

/// Flushes the runs accumulated for the block currently being built into
/// `blocks` as a `Paragraph`/`ListItem` per `kind`. A no-op when empty (an
/// unclosed/empty block, or two block tags back to back), matching
/// `htmlrender::flush`'s leniency.
fn flush_block(blocks: &mut Vec<Block>, cur_runs: &mut Vec<Run>, kind: BlockKind) {
    if cur_runs.is_empty() {
        return;
    }
    let runs = std::mem::take(cur_runs);
    match kind {
        BlockKind::Para => blocks.push(Block::Paragraph(runs)),
        BlockKind::Item { ordered, level } => blocks.push(Block::ListItem {
            ordered,
            level,
            runs,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // editcore's model types live in `editcore::model` (the crate has no
    // root re-export); adapted from the brief's `use editcore::{..}` to
    // match that actual path.
    use editcore::model::{Block, RichText, Run};
    #[test]
    fn to_html_emphasis_and_paragraphs() {
        let rt = RichText {
            blocks: vec![
                Block::Paragraph(vec![
                    Run::plain("Hi "),
                    Run {
                        text: "there".into(),
                        bold: true,
                        ..Run::plain("")
                    },
                ]),
                Block::Paragraph(vec![Run::plain("a < b & c")]),
            ],
        };
        let h = to_html(&rt);
        assert!(h.contains("<p>Hi <b>there</b></p>"));
        assert!(h.contains("a &lt; b &amp; c"));
    }
    #[test]
    fn to_html_lists_group_consecutive_items() {
        let rt = RichText {
            blocks: vec![
                Block::ListItem {
                    ordered: false,
                    level: 0,
                    runs: vec![Run::plain("one")],
                },
                Block::ListItem {
                    ordered: false,
                    level: 0,
                    runs: vec![Run::plain("two")],
                },
            ],
        };
        let h = to_html(&rt);
        assert!(h.contains("<ul><li>one</li><li>two</li></ul>"));
    }
    #[test]
    fn from_html_roundtrips_basic() {
        let rt = from_html("<p>Hello <b>bold</b></p>");
        assert_eq!(rt.plain(), "Hello bold");
        // the bold run survived:
        let runs = match &rt.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!(),
        };
        assert!(runs.iter().any(|r| r.bold && r.text.contains("bold")));
    }
    #[test]
    fn from_html_does_not_panic_on_gnarly_input() {
        let _ = from_html("<p><b>unclosed <i> &amp; <a href=x>link</p><<>>");
    }
}
