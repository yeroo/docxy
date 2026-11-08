//! A page laid out as wrapped lines of styled text, for the terminal editors'
//! F1 overlay (#1856). Every block of the subset is drawn: headings (with
//! their slug recorded as an anchor line), wrapped paragraphs, nested lists
//! with hanging indents (everything under an item at its indent), fenced code
//! verbatim, pipe tables as a padded grid (or, when the grid is wider than the
//! page, each body row as `Header: cell` lines), images as `[image: alt]` and
//! key chords as `Ctrl+B`. Links carry their index in [`md::links`](crate::md::links) order.
//!
//! The width of a piece of text is the host's (`measure`), so a terminal host
//! passes its display-column width and this crate needs no dependency.

use crate::Page;
use crate::md::{Align, Block, Inline, Link, ListItem};

/// How a span looks. A host maps it to its own style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// The heading level (1 to 4) of a heading's text.
    pub heading: Option<u8>,
    pub bold: bool,
    pub italic: bool,
    /// Inline code, a fenced code block's text.
    pub code: bool,
    /// A key chord (`Ctrl+B`).
    pub keys: bool,
    /// An `[image: alt]` placeholder.
    pub image: bool,
    /// The link's index in the page's [`md::links`](crate::md::links) order.
    pub link: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub look: Look,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    /// The line's text, looks dropped.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// A link of the page and the first line it is drawn on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkAt {
    pub link: Link,
    pub line: usize,
}

/// A laid-out page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Laid {
    pub lines: Vec<Line>,
    /// Each heading's slug and its line.
    pub anchors: Vec<(String, usize)>,
    /// The page's links in [`md::links`](crate::md::links) order (a span's `look.link` indexes it).
    pub links: Vec<LinkAt>,
}

impl Laid {
    /// The line of the heading whose slug is `slug`.
    pub fn anchor(&self, slug: &str) -> Option<usize> {
        self.anchors
            .iter()
            .find(|(s, _)| s == slug)
            .map(|&(_, line)| line)
    }
}

/// The marker drawn before a bullet list's items.
pub const BULLET: &str = "•";

/// Lay `page` out `width` columns wide (at least 1), `measure` giving a
/// piece of text's width in columns.
pub fn lay(page: &Page, width: usize, measure: &dyn Fn(&str) -> usize) -> Laid {
    lay_blocks(&page.blocks, width, measure)
}

/// [`lay`] for blocks: one blank line between top-level blocks.
pub fn lay_blocks(blocks: &[Block], width: usize, measure: &dyn Fn(&str) -> usize) -> Laid {
    let mut l = Layout {
        width: width.max(1),
        measure,
        out: Laid::default(),
        next_link: 0,
        placed: Vec::new(),
    };
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            l.out.lines.push(Line::default());
        }
        l.block(b, 0);
    }
    let last = l.out.lines.len().saturating_sub(1);
    for at in &mut l.out.links {
        at.line = at.line.min(last);
    }
    l.out
}

/// A piece of inline text with its look, before wrapping.
type Piece = (String, Look);

struct Layout<'a> {
    width: usize,
    measure: &'a dyn Fn(&str) -> usize,
    out: Laid,
    /// The index the next link met gets ([`md::links`](crate::md::links) order).
    next_link: usize,
    /// Whether a span of each link has been placed on a line yet.
    placed: Vec<bool>,
}

impl Layout<'_> {
    fn w(&self, s: &str) -> usize {
        (self.measure)(s)
    }

    fn block(&mut self, b: &Block, indent: usize) {
        let pad = " ".repeat(indent);
        match b {
            Block::Heading { level, text, slug } => {
                let line = self.out.lines.len();
                self.out.anchors.push((slug.clone(), line));
                let look = Look {
                    heading: Some(*level),
                    bold: true,
                    ..Look::default()
                };
                let pieces = self.pieces(text, look);
                self.wrap(&pieces, vec![span(&pad, Look::default())], &pad);
            }
            Block::Paragraph(text) => {
                let pieces = self.pieces(text, Look::default());
                self.wrap(&pieces, vec![span(&pad, Look::default())], &pad);
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                for (i, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        format!("{}.", u64::from(*start) + i as u64)
                    } else {
                        BULLET.to_string()
                    };
                    self.item(item, &marker, indent);
                }
            }
            Block::Code { text, .. } => {
                let look = Look {
                    code: true,
                    ..Look::default()
                };
                let prefix = format!("{pad}  ");
                for src in text.split('\n') {
                    self.hard(src, look, &prefix);
                }
            }
            Block::Table {
                align,
                header,
                rows,
            } => self.table(align, header, rows, indent),
            Block::Image { alt, .. } => {
                let look = Look {
                    image: true,
                    ..Look::default()
                };
                let text = format!("[image: {alt}]");
                let pieces = vec![(text, look)];
                self.wrap(&pieces, vec![span(&pad, Look::default())], &pad);
            }
        }
    }

    fn item(&mut self, item: &ListItem, marker: &str, indent: usize) {
        let pad = " ".repeat(indent);
        let pieces = self.pieces(&item.text, Look::default());
        let first = vec![span(&format!("{pad}{marker} "), Look::default())];
        let hang = indent + self.w(marker) + 1;
        self.wrap(&pieces, first, &" ".repeat(hang));
        for child in &item.children {
            self.block(child, hang);
        }
    }

    /// Inlines as pieces, links numbered as [`md::links`](crate::md::links) numbers them (a
    /// link, then the links in its text).
    fn pieces(&mut self, inlines: &[Inline], look: Look) -> Vec<Piece> {
        let mut out = Vec::new();
        self.pieces_into(inlines, look, &mut out);
        out
    }

    fn pieces_into(&mut self, inlines: &[Inline], look: Look, out: &mut Vec<Piece>) {
        for i in inlines {
            match i {
                Inline::Text(t) => out.push((t.clone(), look)),
                Inline::Code(t) => out.push((t.clone(), Look { code: true, ..look })),
                Inline::Bold(x) => self.pieces_into(x, Look { bold: true, ..look }, out),
                Inline::Italic(x) => self.pieces_into(
                    x,
                    Look {
                        italic: true,
                        ..look
                    },
                    out,
                ),
                Inline::Keys(k) => out.push((k.join("+"), Look { keys: true, ..look })),
                Inline::Link { text, target } => {
                    let n = self.next_link;
                    self.next_link += 1;
                    // Drawn on the line its text starts at; until a span of
                    // it is placed (an empty text never is), the next line.
                    self.out.links.push(LinkAt {
                        link: target.clone(),
                        line: self.out.lines.len(),
                    });
                    self.placed.push(false);
                    self.pieces_into(
                        text,
                        Look {
                            link: Some(n),
                            ..look
                        },
                        out,
                    );
                }
            }
        }
    }

    /// Note the first line each link's spans are drawn on.
    fn place_links(&mut self, line: &Line, at: usize) {
        for s in &line.spans {
            if let Some(n) = s.look.link {
                if let (Some(l), Some(placed)) = (self.out.links.get_mut(n), self.placed.get_mut(n))
                {
                    if !*placed {
                        l.line = at;
                        *placed = true;
                    }
                }
            }
        }
    }

    fn push(&mut self, line: Line) {
        let at = self.out.lines.len();
        self.place_links(&line, at);
        self.out.lines.push(line);
    }

    /// The widest a line whose prefix is `start` columns wide may be: the
    /// width, but always room for one more column after the prefix.
    fn cap(&self, start: usize) -> usize {
        self.width.max(start + 1)
    }

    /// Word-wrap `pieces`: the first line starts with `first`, the others with
    /// `rest` (spaces). Words break at whitespace; a word wider than the room
    /// is split by character, at least one per line.
    fn wrap(&mut self, pieces: &[Piece], first: Vec<Span>, rest: &str) {
        let mut line = Line::default();
        for s in first {
            add(&mut line, s);
        }
        let mut used: usize = line.spans.iter().map(|s| self.w(&s.text)).sum();
        let mut cap = self.cap(used);
        let mut empty = true;
        let rest_w = self.w(rest);
        for (sep, word) in words(pieces) {
            let ww: usize = word.iter().map(|s| self.w(&s.text)).sum();
            let sep_w = sep.as_ref().map_or(0, |s| self.w(&s.text));
            if !empty && used + sep_w + ww > cap {
                self.push(std::mem::take(&mut line));
                add(&mut line, span(rest, Look::default()));
                used = rest_w;
                cap = self.cap(rest_w);
                empty = true;
            }
            if !empty {
                if let Some(sep) = sep {
                    used += sep_w;
                    add(&mut line, sep);
                }
            }
            if used + ww <= cap {
                for s in word {
                    add(&mut line, s);
                }
                used += ww;
                empty = false;
                continue;
            }
            // Wider than a line: split it by character.
            for s in word {
                for c in s.text.chars() {
                    let mut b = [0; 4];
                    let cs: &str = c.encode_utf8(&mut b);
                    let cw = self.w(cs);
                    if !empty && used + cw > cap {
                        self.push(std::mem::take(&mut line));
                        add(&mut line, span(rest, Look::default()));
                        used = rest_w;
                        cap = self.cap(rest_w);
                    }
                    add(&mut line, span(cs, s.look));
                    used += cw;
                    empty = false;
                }
            }
        }
        self.push(line);
    }

    /// A code line: kept as written, split by character at the width.
    fn hard(&mut self, src: &str, look: Look, prefix: &str) {
        let start = self.w(prefix);
        let cap = self.cap(start);
        let mut line = Line::default();
        add(&mut line, span(prefix, Look::default()));
        let mut used = start;
        for c in src.chars() {
            let mut b = [0; 4];
            let cs: &str = c.encode_utf8(&mut b);
            let cw = self.w(cs);
            if used > start && used + cw > cap {
                self.push(std::mem::take(&mut line));
                add(&mut line, span(prefix, Look::default()));
                used = start;
            }
            add(&mut line, span(cs, look));
            used += cw;
        }
        self.push(line);
    }

    fn table(
        &mut self,
        align: &[Align],
        header: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        indent: usize,
    ) {
        let pad = " ".repeat(indent);
        let bold = Look {
            bold: true,
            ..Look::default()
        };
        // Link numbers go header first, then the rows, as `md::links` has them.
        let head: Vec<Vec<Piece>> = header.iter().map(|c| self.pieces(c, bold)).collect();
        let body: Vec<Vec<Vec<Piece>>> = rows
            .iter()
            .map(|r| r.iter().map(|c| self.pieces(c, Look::default())).collect())
            .collect();
        let cols = head.len().max(body.iter().map(Vec::len).max().unwrap_or(0));
        let cell_w =
            |c: &[Piece], me: &Self| -> usize { c.iter().map(|(t, _)| me.w(&flat(t))).sum() };
        let mut widths = vec![0; cols];
        for row in std::iter::once(&head).chain(&body) {
            for (i, c) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell_w(c, self));
            }
        }
        let grid = indent + widths.iter().sum::<usize>() + 3 * cols.saturating_sub(1);
        if grid <= self.width {
            let row_line = |row: &[Vec<Piece>], me: &Self| -> Line {
                let mut line = Line::default();
                add(&mut line, span(&pad, Look::default()));
                for (i, w) in widths.iter().enumerate() {
                    if i > 0 {
                        add(&mut line, span(" | ", Look::default()));
                    }
                    let cell = row.get(i).map(Vec::as_slice).unwrap_or(&[]);
                    let gap = w - cell_w(cell, me);
                    let (l, r) = match align.get(i).copied().unwrap_or(Align::None) {
                        Align::Right => (gap, 0),
                        Align::Center => (gap / 2, gap - gap / 2),
                        Align::None | Align::Left => (0, gap),
                    };
                    add(&mut line, span(&" ".repeat(l), Look::default()));
                    for (t, look) in cell {
                        add(&mut line, span(&flat(t), *look));
                    }
                    add(&mut line, span(&" ".repeat(r), Look::default()));
                }
                trim_end(&mut line);
                line
            };
            let h = row_line(&head, self);
            self.push(h);
            let rule: Vec<String> = widths.iter().map(|w| "-".repeat((*w).max(1))).collect();
            let rule = format!("{pad}{}", rule.join("-+-"));
            self.push(Line {
                spans: vec![span(&rule, Look::default())],
            });
            for row in &body {
                let l = row_line(row, self);
                self.push(l);
            }
            return;
        }
        // Too wide: each body row as `Header: cell` lines, a blank line
        // between rows, so no cell is cut off.
        let rest = format!("{pad}  ");
        if body.is_empty() {
            let mut pieces = Vec::new();
            for (i, c) in head.iter().enumerate() {
                if i > 0 {
                    pieces.push((" | ".to_string(), Look::default()));
                }
                pieces.extend(c.iter().cloned());
            }
            self.wrap(&pieces, vec![span(&pad, Look::default())], &rest);
            return;
        }
        for (r, row) in body.iter().enumerate() {
            if r > 0 {
                self.out.lines.push(Line::default());
            }
            for i in 0..cols {
                let mut first = vec![span(&pad, Look::default())];
                if let Some(h) = head.get(i) {
                    for (t, look) in h {
                        first.push(span(&flat(t), *look));
                    }
                }
                first.push(span(": ", bold));
                let cell = row.get(i).cloned().unwrap_or_default();
                self.wrap(&cell, first, &rest);
            }
        }
    }
}

fn span(text: &str, look: Look) -> Span {
    Span {
        text: text.to_string(),
        look,
    }
}

/// Add a span, merged into the last one when it looks the same.
fn add(line: &mut Line, s: Span) {
    if s.text.is_empty() {
        return;
    }
    match line.spans.last_mut() {
        Some(last) if last.look == s.look => last.text.push_str(&s.text),
        _ => line.spans.push(s),
    }
}

fn trim_end(line: &mut Line) {
    while let Some(last) = line.spans.last_mut() {
        let t = last.text.trim_end_matches(' ').len();
        last.text.truncate(t);
        if last.text.is_empty() {
            line.spans.pop();
        } else {
            break;
        }
    }
}

/// Inline text on one line: line breaks read as spaces.
fn flat(t: &str) -> String {
    t.replace(['\n', '\r', '\t'], " ")
}

/// The words of `pieces`, each with the space before it (`None` for the
/// first): whitespace runs collapse to one space in the look of the first.
fn words(pieces: &[Piece]) -> Vec<(Option<Span>, Vec<Span>)> {
    let mut out: Vec<(Option<Span>, Vec<Span>)> = Vec::new();
    let mut sep: Option<Span> = None;
    let mut word: Vec<Span> = Vec::new();
    let mut started = false;
    for (text, look) in pieces {
        for c in text.chars() {
            if c.is_whitespace() {
                if !word.is_empty() {
                    out.push((sep.take().filter(|_| started), std::mem::take(&mut word)));
                    started = true;
                }
                if sep.is_none() {
                    sep = Some(span(" ", *look));
                }
            } else {
                match word.last_mut() {
                    Some(last) if last.look == *look => last.text.push(c),
                    _ => word.push(Span {
                        text: c.to_string(),
                        look: *look,
                    }),
                }
            }
        }
    }
    if !word.is_empty() {
        out.push((sep.take().filter(|_| started), word));
    }
    // The first word has no space before it, whatever led the text.
    if let Some(first) = out.first_mut() {
        first.0 = None;
    }
    out
}

/// Each block's links in order, as [`lay`] numbers them: [`md::links`](crate::md::links).
#[cfg(test)]
fn link_order(blocks: &[Block]) -> Vec<Link> {
    crate::md::links(blocks).into_iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::parse;

    fn cols(s: &str) -> usize {
        s.chars().count()
    }

    /// A measure where CJK ideographs are two columns wide.
    fn wide(s: &str) -> usize {
        s.chars()
            .map(|c| {
                if ('\u{4e00}'..='\u{9fff}').contains(&c) {
                    2
                } else {
                    1
                }
            })
            .sum()
    }

    fn texts(l: &Laid) -> Vec<String> {
        l.lines.iter().map(Line::text).collect()
    }

    fn lay_src(src: &str, width: usize) -> Laid {
        lay_blocks(&parse(src), width, &cols)
    }

    #[test]
    fn headings_record_their_anchor_line() {
        let l = lay_src("# Title\n\nBody.\n\n## Part\n\nMore.\n\n## Part", 40);
        assert_eq!(
            texts(&l),
            ["Title", "", "Body.", "", "Part", "", "More.", "", "Part"]
        );
        assert_eq!(l.anchor("title"), Some(0));
        assert_eq!(l.anchor("part"), Some(4));
        assert_eq!(l.anchor("part-1"), Some(8));
        let h = &l.lines[0].spans[0];
        assert_eq!(h.look.heading, Some(1));
        assert!(h.look.bold);
    }

    #[test]
    fn paragraphs_wrap_at_spaces() {
        let l = lay_src("one two three four five", 9);
        assert_eq!(texts(&l), ["one two", "three", "four five"]);
        for line in texts(&l) {
            assert!(cols(&line) <= 9, "{line}");
        }
    }

    #[test]
    fn a_long_word_is_split_and_every_split_advances() {
        let l = lay_src("abcdefghij", 4);
        assert_eq!(texts(&l), ["abcd", "efgh", "ij"]);
        for w in [0, 1] {
            let l = lay_src(
                "ab cd\n\n```\nxyz\n```\n\n- a b\n\n| h | i |\n|---|---|\n| j | k |",
                w,
            );
            let t = texts(&l);
            assert_eq!(&t[..4], ["a", "b", "c", "d"], "{w}: {t:?}");
            assert!(t.contains(&"• a".to_string()), "{w}: {t:?}");
            assert!(t.len() < 40, "{w}: {t:?}");
        }
    }

    #[test]
    fn wide_glyphs_wrap_by_their_width() {
        let l = lay_blocks(&parse("中文字 ab 中文"), 6, &wide);
        assert_eq!(texts(&l), ["中文字", "ab", "中文"]);
        let l = lay_blocks(&parse("中文字中"), 5, &wide);
        assert_eq!(texts(&l), ["中文", "字中"]);
    }

    #[test]
    fn lists_hang_their_items_and_children() {
        let src = "- one two three four\n  - nested\n\n  ```\n  code  kept\n  ```\n\n  ![shot](images/a.png)\n\n  more text\n- last";
        let l = lay_src(src, 16);
        assert_eq!(
            texts(&l),
            [
                "• one two three",
                "  four",
                "  • nested",
                "    code  kept",
                "  [image: shot]",
                "  more text",
                "• last",
            ],
        );
        let l = lay_src("3. a\n4. b\n   1. c", 20);
        assert_eq!(texts(&l), ["3. a", "4. b", "   1. c"]);
    }

    #[test]
    fn code_is_verbatim_and_hard_wrapped() {
        let l = lay_src("```\nthe grid draws **as typed**\n\n  x\n```", 12);
        assert_eq!(
            texts(&l),
            ["  the grid d", "  raws **as ", "  typed**", "  ", "    x"]
        );
        assert!(l.lines[0].spans[1].look.code);
    }

    #[test]
    fn tables_pad_and_align_when_they_fit() {
        let src = "| Command | Keys | Where |\n|:---|:---:|---:|\n| Insert table | <kbd>Alt+N+T</kbd> | Insert |\n| Delete | none | x |";
        let l = lay_src(src, 60);
        assert_eq!(
            texts(&l),
            [
                "Command      |  Keys   |  Where",
                "-------------+---------+-------",
                "Insert table | Alt+N+T | Insert",
                "Delete       |  none   |      x",
            ]
        );
        assert!(l.lines[0].spans.iter().any(|s| s.look.bold));
        assert!(l.lines[2].spans.iter().any(|s| s.look.keys));
    }

    #[test]
    fn a_table_too_wide_stacks_its_rows() {
        let src =
            "| Command | Where |\n|---|---|\n| Insert table | Insert tab |\n| Delete | Layout |";
        let l = lay_src(src, 16);
        assert_eq!(
            texts(&l),
            [
                "Command: Insert",
                "  table",
                "Where: Insert",
                "  tab",
                "",
                "Command: Delete",
                "Where: Layout",
            ]
        );
    }

    #[test]
    fn images_and_keys_have_their_look() {
        let l = lay_src(
            "![The ribbon](images/r.png)\n\nPress <kbd>Ctrl+B</kbd> now.",
            40,
        );
        assert_eq!(texts(&l), ["[image: The ribbon]", "", "Press Ctrl+B now."]);
        assert!(l.lines[0].spans[0].look.image);
        let keys = l.lines[2].spans.iter().find(|s| s.look.keys).unwrap();
        assert_eq!(keys.text, "Ctrl+B");
    }

    #[test]
    fn links_are_numbered_in_md_links_order() {
        let src = "# See [a](home)\n\n- [b](#x)\n\n| [c](https://e.org) | d |\n|---|---|\n| [e](tables) | [f](nope!) |\n\nText [g](insert#t).";
        let blocks = parse(src);
        let l = lay_blocks(&blocks, 80, &cols);
        let got: Vec<Link> = l.links.iter().map(|a| a.link.clone()).collect();
        assert_eq!(got, link_order(&blocks));
        for (n, at) in l.links.iter().enumerate() {
            let line = &l.lines[at.line];
            assert!(
                line.spans.iter().any(|s| s.look.link == Some(n)),
                "link {n} on line {}",
                at.line
            );
        }
        // Bold link text keeps both looks.
        let l = lay_src("**[x](home)**", 20);
        assert_eq!(l.lines[0].spans[0].look.link, Some(0));
        assert!(l.lines[0].spans[0].look.bold);
    }

    #[test]
    fn a_wrapped_link_starts_on_its_first_line() {
        let l = lay_src("aaaa [bb cc dd](home)", 7);
        assert_eq!(texts(&l), ["aaaa bb", "cc dd"]);
        assert_eq!(l.links[0].line, 0);
        assert!(l.lines[1].spans.iter().all(|s| s.look.link == Some(0)));
    }
}
