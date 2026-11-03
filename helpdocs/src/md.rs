//! The markdown subset the manuals are written in, parsed into blocks a
//! renderer can draw. Anything outside the subset is a paragraph of literal
//! text, never an error.
//!
//! Blocks: ATX headings `#`..`####`, paragraphs, `-`/`*` bullet and `1.`
//! numbered lists nested by indent, fenced code, GFM pipe tables (header and
//! alignment row) and an image on a line of its own. Inline: `**bold**`,
//! `*italic*`, `` `code` ``, `[text](target)` and `<kbd>Ctrl+Shift+L</kbd>`.

use crate::{slug, valid_page_id};

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: Vec<Inline>,
        /// Unique within the page: a repeated slug gets `-1`, `-2`, ...
        slug: String,
    },
    Paragraph(Vec<Inline>),
    List {
        ordered: bool,
        /// The first item's number (`3. foo` starts at 3); 1 for bullets.
        start: u32,
        items: Vec<ListItem>,
    },
    Code {
        lang: String,
        text: String,
    },
    Table {
        align: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Image {
        alt: String,
        src: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub text: Vec<Inline>,
    /// What sits under the item, in order: images and fenced code indented
    /// under it, and nested lists.
    pub children: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Code(String),
    Link {
        text: Vec<Inline>,
        target: Link,
    },
    /// A key chord: `<kbd>Ctrl+Shift+L</kbd>` is `["Ctrl", "Shift", "L"]`.
    Keys(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// Another page, maybe at one of its headings (`page#slug`).
    Page {
        page: String,
        anchor: Option<String>,
    },
    /// A heading of this page (`#slug`).
    Anchor(String),
    /// `http:` or `https:`: kept, flagged so a host can say it leaves the manual.
    External(String),
    /// Anything else: reported as a broken link by the bundle.
    Other(String),
}

impl Link {
    pub fn classify(target: &str) -> Link {
        let t = target.trim();
        if t.starts_with("http://") || t.starts_with("https://") {
            return Link::External(t.to_string());
        }
        if let Some(anchor) = t.strip_prefix('#') {
            return Link::Anchor(anchor.to_string());
        }
        let (page, anchor) = match t.split_once('#') {
            Some((p, a)) => (p, Some(a.to_string())),
            None => (t, None),
        };
        if valid_page_id(page) {
            Link::Page {
                page: page.to_string(),
                anchor,
            }
        } else {
            Link::Other(t.to_string())
        }
    }
}

/// The text of inlines with their markup dropped (keys joined by `+`).
pub fn plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    plain_into(inlines, &mut out);
    out
}

fn plain_into(inlines: &[Inline], out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) | Inline::Code(t) => out.push_str(t),
            Inline::Bold(x) | Inline::Italic(x) => plain_into(x, out),
            Inline::Link { text, .. } => plain_into(text, out),
            Inline::Keys(k) => out.push_str(&k.join("+")),
        }
    }
}

/// Parse a page body (front matter already removed). CRLF is accepted.
pub fn parse(src: &str) -> Vec<Block> {
    let src = src.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = src.lines().collect();
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut slugs: Vec<String> = Vec::new();
    let mut i = 0;
    let flush = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            let text = para.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
            blocks.push(Block::Paragraph(inline(&text)));
            para.clear();
        }
    };
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            flush(&mut para, &mut blocks);
            i += 1;
        } else if t.starts_with("```") {
            flush(&mut para, &mut blocks);
            blocks.push(fence(&lines, &mut i));
        } else if let Some((level, text)) = heading(t) {
            flush(&mut para, &mut blocks);
            let text = inline(text);
            let base = slug(&plain(&text));
            // The first free one of `base`, `base-1`, `base-2`, ...: a
            // heading's own `-1` never collides with a repeat's.
            let mut slug = base.clone();
            let mut n = 0;
            while slugs.contains(&slug) {
                n += 1;
                slug = format!("{base}-{n}");
            }
            slugs.push(slug.clone());
            blocks.push(Block::Heading { level, text, slug });
            i += 1;
        } else if let Some((alt, src)) = image(t) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Image { alt, src });
            i += 1;
        } else if let Some(align) = table_start(&lines, i) {
            flush(&mut para, &mut blocks);
            let header = cells(t).iter().map(|c| inline(c)).collect();
            let mut rows = Vec::new();
            i += 2;
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(cells(lines[i].trim()).iter().map(|c| inline(c)).collect());
                i += 1;
            }
            blocks.push(Block::Table {
                align,
                header,
                rows,
            });
        } else if list_item(line).is_some() {
            flush(&mut para, &mut blocks);
            let items = list_items(&lines, &mut i);
            let mut at = 0;
            while at < items.len() {
                let base = items[at].indent;
                blocks.push(build_list(&items, &mut at, base));
            }
        } else {
            para.push(line);
            i += 1;
        }
    }
    flush(&mut para, &mut blocks);
    blocks
}

/// The fenced code block opening at `lines[*i]`; `*i` moves past its
/// closing line, one that is exactly the fence once trimmed (or the end).
/// The opening line's indent is taken off the body's lines.
fn fence(lines: &[&str], i: &mut usize) -> Block {
    let open = lines[*i];
    let indent = open.len() - open.trim_start_matches(' ').len();
    let lang = open.trim().trim_start_matches('`').trim().to_string();
    let mut body = Vec::new();
    *i += 1;
    while *i < lines.len() && lines[*i].trim() != "```" {
        let l = lines[*i];
        let lead = l.len() - l.trim_start_matches(' ').len();
        body.push(&l[lead.min(indent)..]);
        *i += 1;
    }
    *i += 1; // the closing fence (or the end)
    Block::Code {
        lang,
        text: body.join("\n"),
    }
}

/// The items of a list starting at `lines[*i]`, with their wrapped lines and
/// the images and fences indented under them. The list goes on across blank
/// lines while the next item is at its first item's indent or deeper.
fn list_items(lines: &[&str], i: &mut usize) -> Vec<RawItem> {
    let mut items: Vec<RawItem> = Vec::new();
    let Some((base, ordered)) = list_item(lines[*i]).map(|it| (it.indent, it.ordered)) else {
        return items;
    };
    while let Some(item) = lines.get(*i).and_then(|l| list_item(l)) {
        if item.indent < base {
            break;
        }
        items.push(item);
        *i += 1;
        // A wrapped item: its following plain lines are its text.
        while *i < lines.len() && continues_item(lines, *i) {
            if let Some(last) = items.last_mut() {
                last.text.push(' ');
                last.text.push_str(lines[*i].trim());
            }
            *i += 1;
        }
        // Blank lines, and images and fences indented under the item.
        let mut j = *i;
        loop {
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            let Some(l) = lines.get(j) else { break };
            let Some(last) = items.last_mut() else { break };
            let indent = l.len() - l.trim_start_matches(' ').len();
            if indent <= last.indent {
                break;
            }
            if let Some((alt, src)) = image(l.trim()) {
                last.blocks.push(Block::Image { alt, src });
                j += 1;
            } else if l.trim().starts_with("```") {
                last.blocks.push(fence(lines, &mut j));
            } else {
                break;
            }
            *i = j;
        }
        // Go on only when an item of this list comes next: deeper, or at its
        // indent and of its kind (a bullet list after a numbered one, past a
        // blank line, is a list of its own).
        let gap = j > *i;
        match lines.get(j).and_then(|l| list_item(l)) {
            Some(next)
                if next.indent > base
                    || (next.indent == base && (!gap || next.ordered == ordered)) =>
            {
                *i = j
            }
            _ => break,
        }
    }
    items
}

/// The alignments of a table starting at `lines[i]`: a `|` header row, then
/// an alignment row with one cell per header cell. Body rows may be ragged.
fn table_start(lines: &[&str], i: usize) -> Option<Vec<Align>> {
    let t = lines[i].trim();
    if !t.starts_with('|') {
        return None;
    }
    let align = aligns(lines.get(i + 1)?)?;
    (align.len() == cells(t).len()).then_some(align)
}

/// Whether `lines[i]` continues the list item before it: not blank, and not
/// a list item, heading, fence, table or image of its own.
fn continues_item(lines: &[&str], i: usize) -> bool {
    let t = lines[i].trim();
    !t.is_empty()
        && list_item(lines[i]).is_none()
        && heading(t).is_none()
        && !t.starts_with("```")
        && table_start(lines, i).is_none()
        && image(t).is_none()
}

fn heading(t: &str) -> Option<(u8, &str)> {
    let level = t.bytes().take_while(|b| *b == b'#').count();
    if !(1..=4).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    rest.starts_with(' ').then(|| (level as u8, rest.trim()))
}

fn image(t: &str) -> Option<(String, String)> {
    let rest = t.strip_prefix("![")?;
    let (alt, rest) = rest.split_once("](")?;
    let src = rest.strip_suffix(')')?;
    (!src.contains(')') && !src.trim().is_empty())
        .then(|| (alt.to_string(), src.trim().to_string()))
}

fn cells(row: &str) -> Vec<String> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    // A `\|` at the row's end is a cell's text, not the closing pipe.
    let row = match row.strip_suffix('|') {
        Some(r) if !r.ends_with('\\') => r,
        _ => row,
    };
    // Split on a `|` that is neither escaped nor in a code span; `\|` is `|`.
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut in_code = false;
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cell.push(c);
            }
            '|' if !in_code => out.push(std::mem::take(&mut cell).trim().to_string()),
            _ => cell.push(c),
        }
    }
    out.push(cell.trim().to_string());
    out
}

fn aligns(line: &str) -> Option<Vec<Align>> {
    let t = line.trim();
    if !t.starts_with('|') {
        return None;
    }
    cells(t)
        .iter()
        .map(|c| {
            let dashes = c.trim_matches(':');
            if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
                return None;
            }
            Some(match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            })
        })
        .collect()
}

struct RawItem {
    indent: usize,
    ordered: bool,
    /// The item's number; 1 for a bullet.
    number: u32,
    text: String,
    /// Images and fences indented under the item.
    blocks: Vec<Block>,
}

fn list_item(line: &str) -> Option<RawItem> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let t = &line[indent..];
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some(RawItem {
            indent,
            ordered: false,
            number: 1,
            text: rest.trim().to_string(),
            blocks: Vec::new(),
        });
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let rest = t[digits..].strip_prefix(". ")?;
    (digits > 0).then(|| RawItem {
        indent,
        ordered: true,
        number: t[..digits].parse().unwrap_or(u32::MAX),
        text: rest.trim().to_string(),
        blocks: Vec::new(),
    })
}

/// One list from `items[*at]` on: the items at `base` or deeper, a deeper
/// item nesting under the item before it.
fn build_list(items: &[RawItem], at: &mut usize, base: usize) -> Block {
    let ordered = items[*at].ordered;
    let start = items[*at].number;
    let mut out: Vec<ListItem> = Vec::new();
    while *at < items.len() && items[*at].indent >= base {
        let indent = items[*at].indent;
        if indent > base && !out.is_empty() {
            let child = build_list(items, at, indent);
            if let Some(last) = out.last_mut() {
                last.children.push(child);
            }
            continue;
        }
        out.push(ListItem {
            text: inline(&items[*at].text),
            children: items[*at].blocks.clone(),
        });
        *at += 1;
    }
    Block::List {
        ordered,
        start,
        items: out,
    }
}

/// Parse inline markup. An opener with no closer is literal text.
pub fn inline(s: &str) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        // An inline image is outside the subset: literal text, not a link.
        if rest.starts_with("![") {
            if let Some((_, used)) = link(&rest[1..]) {
                text.push_str(&rest[..1 + used]);
                rest = &rest[1 + used..];
                continue;
            }
        }
        let parsed = match c {
            '`' => rest[1..]
                .find('`')
                .map(|e| (Inline::Code(rest[1..1 + e].to_string()), 2 + e)),
            '<' if rest.starts_with("<kbd>") => rest
                .find("</kbd>")
                .and_then(|e| keys(&rest[5..e]).map(|k| (Inline::Keys(k), e + 6))),
            '*' if rest.starts_with("**") && opens(&rest[2..]) => closer(&rest[2..], "**")
                .filter(|e| *e > 0)
                .map(|e| (Inline::Bold(inline(&rest[2..2 + e])), 4 + e)),
            '*' if opens(&rest[1..]) => closer(&rest[1..], "*")
                .filter(|e| *e > 0)
                .map(|e| (Inline::Italic(inline(&rest[1..1 + e])), 2 + e)),
            '[' => link(rest),
            _ => None,
        };
        match parsed {
            Some((node, used)) => {
                if !text.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut text)));
                }
                out.push(node);
                rest = &rest[used..];
            }
            None => {
                text.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    if !text.is_empty() {
        out.push(Inline::Text(text));
    }
    out
}

/// `[text](target)` at the start of `s`, and the bytes it takes. The first
/// `]` must be followed by `(`; otherwise the `[` is literal.
fn link(s: &str) -> Option<(Inline, usize)> {
    let m = 1 + closer(s.get(1..)?, "]")?;
    let after = s[m + 1..].strip_prefix('(')?;
    let close = after.find(')')?;
    let target = Link::classify(&after[..close]);
    Some((
        Inline::Link {
            text: inline(&s[1..m]),
            target,
        },
        m + 3 + close,
    ))
}

/// Whether emphasis can open before `s`: a character follows, not a space.
fn opens(s: &str) -> bool {
    s.chars().next().is_some_and(|c| !c.is_whitespace())
}

/// Where `delim` closes in `s`, skipping code spans. An emphasis closer
/// (`*`, `**`) must follow a non-space; it is the last `delim.len()` stars of
/// its run (`***x***`), and an italic closer skips a whole `**...**` span.
fn closer(s: &str, delim: &str) -> Option<usize> {
    let emphasis = delim.starts_with('*');
    let mut at = 0;
    while at < s.len() {
        let rest = &s[at..];
        if let Some(e) = rest.strip_prefix('`').and_then(|r| r.find('`')) {
            at += 2 + e;
            continue;
        }
        if emphasis && rest.starts_with('*') {
            let run = rest.bytes().take_while(|b| *b == b'*').count();
            let after_space = s[..at].chars().next_back().is_none_or(char::is_whitespace);
            if !after_space && run >= delim.len() {
                return Some(at + run - delim.len());
            }
            if delim == "*" && run >= 2 && opens(&rest[2..]) {
                if let Some(e) = closer(&rest[2..], "**") {
                    at += 4 + e;
                    continue;
                }
            }
            at += run;
            continue;
        }
        if !emphasis && rest.starts_with(delim) {
            return Some(at);
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// The keys of a chord split on `+`. A `+` where a key is due is the `+`
/// key, so `Ctrl++`, `Ctrl++Shift` and `+A` each hold one. An empty chord is
/// not one.
fn keys(chord: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in chord.chars() {
        if c != '+' {
            if cur == "+" {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        } else if cur.trim().is_empty() {
            cur = "+".to_string();
        } else {
            out.push(std::mem::take(&mut cur).trim().to_string());
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    (!out.is_empty()).then_some(out)
}

/// Every image source in the blocks, list items' included, in order.
pub fn images(blocks: &[Block]) -> Vec<&str> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Image { src, .. } => out.push(src.as_str()),
            Block::List { items, .. } => {
                for item in items {
                    out.extend(images(&item.children));
                }
            }
            _ => {}
        }
    }
    out
}

/// Every link in the blocks, in order.
pub fn links(blocks: &[Block]) -> Vec<&Link> {
    let mut out = Vec::new();
    for_inlines(blocks, &mut |inl| collect_links(inl, &mut out));
    out
}

fn collect_links<'a>(inlines: &'a [Inline], out: &mut Vec<&'a Link>) {
    for i in inlines {
        match i {
            Inline::Link { text, target } => {
                out.push(target);
                collect_links(text, out);
            }
            Inline::Bold(x) | Inline::Italic(x) => collect_links(x, out),
            _ => {}
        }
    }
}

/// Call `f` on every run of inlines in the blocks, in order.
pub fn for_inlines<'a>(blocks: &'a [Block], f: &mut impl FnMut(&'a [Inline])) {
    for b in blocks {
        match b {
            Block::Heading { text, .. } | Block::Paragraph(text) => f(text),
            Block::List { items, .. } => {
                for item in items {
                    f(&item.text);
                    for_inlines(&item.children, f);
                }
            }
            Block::Table { header, rows, .. } => {
                for c in header.iter().chain(rows.iter().flatten()) {
                    f(c);
                }
            }
            Block::Code { .. } | Block::Image { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    #[test]
    fn headings_of_each_level_get_unique_slugs() {
        let b = parse(
            "# One\n## Two words\n### Three!\n#### Four\n## Two words\n## Two words\n##### Five",
        );
        let got: Vec<(u8, &str)> = b
            .iter()
            .filter_map(|b| match b {
                Block::Heading { level, slug, .. } => Some((*level, slug.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            [
                (1, "one"),
                (2, "two-words"),
                (3, "three"),
                (4, "four"),
                (2, "two-words-1"),
                (2, "two-words-2")
            ]
        );
        // A fifth level is outside the subset: literal text.
        assert_eq!(b.last(), Some(&Block::Paragraph(vec![t("##### Five")])));
        // `#word` is not a heading.
        assert_eq!(parse("#tag"), vec![Block::Paragraph(vec![t("#tag")])]);
    }

    #[test]
    fn lists_nest_by_indent() {
        let b = parse("- a\n  - a1\n    1. deep\n  - a2\n- b\n\n1. one\n2. two\n* star");
        let Block::List { ordered, items, .. } = &b[0] else {
            panic!("{b:?}")
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, vec![t("a")]);
        let Block::List { items: sub, .. } = &items[0].children[0] else {
            panic!()
        };
        assert_eq!(sub.len(), 2);
        assert_eq!(sub[1].text, vec![t("a2")]);
        let Block::List {
            ordered,
            items: deep,
            ..
        } = &sub[0].children[0]
        else {
            panic!()
        };
        assert!(ordered);
        assert_eq!(deep[0].text, vec![t("deep")]);
        assert_eq!(items[1].children, vec![]);
        // A numbered list after a blank line, then a `*` item joining it.
        let Block::List { ordered, items, .. } = &b[1] else {
            panic!("{b:?}")
        };
        assert!(ordered);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn a_table_keeps_its_alignment() {
        let b = parse(
            "| Key | What | Where |\n|:---|:---:|---:|\n| **B** | Bold | Home |\n| I | Italic |",
        );
        let Block::Table {
            align,
            header,
            rows,
        } = &b[0]
        else {
            panic!("{b:?}")
        };
        assert_eq!(align, &[Align::Left, Align::Center, Align::Right]);
        assert_eq!(header[1], vec![t("What")]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], vec![Inline::Bold(vec![t("B")])]);
        assert_eq!(rows[1].len(), 2);
        // Without an alignment row a `|` line is text.
        assert_eq!(parse("| a |"), vec![Block::Paragraph(vec![t("| a |")])]);
    }

    #[test]
    fn fenced_code_is_literal() {
        let b = parse("```rust\nlet **x** = `y`;\n\n# not a heading\n```\nafter");
        assert_eq!(
            b[0],
            Block::Code {
                lang: "rust".into(),
                text: "let **x** = `y`;\n\n# not a heading".into()
            }
        );
        assert_eq!(b[1], Block::Paragraph(vec![t("after")]));
    }

    #[test]
    fn an_image_on_its_own_line() {
        let b = parse("![The Home tab](images/home.png)\n\ntext ![x](y.png) inline");
        assert_eq!(
            b[0],
            Block::Image {
                alt: "The Home tab".into(),
                src: "images/home.png".into()
            }
        );
        assert_eq!(b[1], Block::Paragraph(vec![t("text ![x](y.png) inline")]));
    }

    #[test]
    fn links_are_classified() {
        let b =
            inline("[a](home-font) [b](insert#tables) [c](#keys) [d](https://x.org) [e](Bad.md)");
        let targets: Vec<&Link> = b
            .iter()
            .filter_map(|i| match i {
                Inline::Link { target, .. } => Some(target),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            [
                &Link::Page {
                    page: "home-font".into(),
                    anchor: None
                },
                &Link::Page {
                    page: "insert".into(),
                    anchor: Some("tables".into())
                },
                &Link::Anchor("keys".into()),
                &Link::External("https://x.org".into()),
                &Link::Other("Bad.md".into()),
            ]
        );
    }

    #[test]
    fn kbd_is_a_key_chord() {
        assert_eq!(
            inline("Press <kbd>Ctrl+Shift+L</kbd>."),
            vec![
                t("Press "),
                Inline::Keys(vec!["Ctrl".into(), "Shift".into(), "L".into()]),
                t(".")
            ]
        );
        assert_eq!(
            inline("<kbd>Ctrl+Plus</kbd>"),
            vec![Inline::Keys(vec!["Ctrl".into(), "Plus".into()])]
        );
    }

    #[test]
    fn inline_bold_italic_code() {
        assert_eq!(
            inline("**bold *it* x** and *x* `a*b`"),
            vec![
                Inline::Bold(vec![t("bold "), Inline::Italic(vec![t("it")]), t(" x")]),
                t(" and "),
                Inline::Italic(vec![t("x")]),
                t(" "),
                Inline::Code("a*b".into()),
            ]
        );
    }

    #[test]
    fn unknown_syntax_is_text() {
        assert_eq!(
            parse("> a quote\n<div>x</div>\n~~gone~~ 2 * 3 [no link"),
            vec![Block::Paragraph(vec![t(
                "> a quote <div>x</div> ~~gone~~ 2 * 3 [no link"
            )])]
        );
        assert_eq!(inline("**open"), vec![t("**open")]);
        assert_eq!(inline("`open"), vec![t("`open")]);
    }

    #[test]
    fn crlf_input_parses_as_lf() {
        assert_eq!(
            parse("# Title\r\n\r\n- a\r\n- b\r\n\r\npara\r\nline"),
            parse("# Title\n\n- a\n- b\n\npara\nline")
        );
        assert_eq!(
            parse("para\r\nline")[0],
            Block::Paragraph(vec![t("para line")])
        );
    }

    fn slugs(src: &str) -> Vec<String> {
        parse(src)
            .into_iter()
            .filter_map(|b| match b {
                Block::Heading { slug, .. } => Some(slug),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_heading_slug_never_collides_with_a_bumped_one() {
        assert_eq!(slugs("# A 1\n# A\n# A"), ["a-1", "a", "a-2"]);
        assert_eq!(slugs("# A\n# A\n# A 1"), ["a", "a-1", "a-1-1"]);
    }

    #[test]
    fn a_bracket_without_a_link_is_literal() {
        let link = Inline::Link {
            text: vec![t("Home")],
            target: Link::Page {
                page: "home".into(),
                anchor: None,
            },
        };
        assert_eq!(
            inline("Press [Enter] then see [Home](home)"),
            vec![t("Press [Enter] then see "), link]
        );
    }

    #[test]
    fn a_wrapped_list_item_keeps_its_lines() {
        for (src, ordered) in [
            ("- one\n  wraps here\n- two", false),
            ("1. one\nwraps here\n2. two", true),
        ] {
            let b = parse(src);
            assert_eq!(b.len(), 1, "{b:?}");
            let Block::List {
                ordered: o, items, ..
            } = &b[0]
            else {
                panic!("{b:?}")
            };
            assert_eq!(*o, ordered);
            assert_eq!(items[0].text, vec![t("one wraps here")]);
            assert_eq!(items[1].text, vec![t("two")]);
        }
        // A heading after an item is its own block.
        assert!(matches!(parse("- a\n# H")[1], Block::Heading { .. }));
    }

    #[test]
    fn an_inline_image_is_literal_text() {
        assert_eq!(
            inline("text ![x](y.png) inline"),
            vec![t("text ![x](y.png) inline")]
        );
    }

    #[test]
    fn emphasis_closers_skip_code_spans() {
        assert_eq!(
            inline("**a `**` b**"),
            vec![Inline::Bold(vec![
                t("a "),
                Inline::Code("**".into()),
                t(" b")
            ])]
        );
    }

    #[test]
    fn a_plus_key_and_an_empty_chord() {
        assert_eq!(
            inline("<kbd>Ctrl++</kbd>"),
            vec![Inline::Keys(vec!["Ctrl".into(), "+".into()])]
        );
        assert_eq!(inline("<kbd></kbd>"), vec![t("<kbd></kbd>")]);
    }

    #[test]
    fn a_table_needs_one_alignment_per_header_cell() {
        assert!(matches!(
            parse("| a | b |\n|---|\n| 1 | 2 |")[0],
            Block::Paragraph(_)
        ));
    }

    fn list(b: &Block) -> (bool, u32, &[ListItem]) {
        match b {
            Block::List {
                ordered,
                start,
                items,
            } => (*ordered, *start, items),
            _ => panic!("not a list: {b:?}"),
        }
    }

    #[test]
    fn emphasis_needs_flanking_and_skips_a_bold_span() {
        assert_eq!(
            inline("*a **b** c*"),
            vec![Inline::Italic(vec![
                t("a "),
                Inline::Bold(vec![t("b")]),
                t(" c")
            ])]
        );
        assert_eq!(
            inline("***x***"),
            vec![Inline::Bold(vec![Inline::Italic(vec![t("x")])])]
        );
        assert_eq!(
            inline("**a *b***"),
            vec![Inline::Bold(vec![t("a "), Inline::Italic(vec![t("b")])])]
        );
        assert_eq!(inline("2 * 3 * 4"), vec![t("2 * 3 * 4")]);
    }

    #[test]
    fn a_loose_numbered_list_is_one_list() {
        let b = parse("1. one\n\n2. two\n\n3. three\n\nafter");
        assert_eq!(b.len(), 2, "{b:?}");
        let (ordered, start, items) = list(&b[0]);
        assert!(ordered);
        assert_eq!(start, 1);
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].text, vec![t("three")]);
    }

    #[test]
    fn a_step_keeps_its_indented_screenshot_and_code() {
        let b = parse(
            "1. Open the tab.\n\n   ![The tab](images/tab.png)\n\n   ```\n   code\n   ```\n2. Click.",
        );
        assert_eq!(b.len(), 1, "{b:?}");
        let (_, _, items) = list(&b[0]);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].children,
            vec![
                Block::Image {
                    alt: "The tab".into(),
                    src: "images/tab.png".into()
                },
                Block::Code {
                    lang: String::new(),
                    text: "code".into()
                }
            ]
        );
        assert_eq!(images(&b), ["images/tab.png"]);
        // An image not indented under the item ends the list.
        assert_eq!(parse("1. a\n\n![x](y.png)\n2. b").len(), 3);
    }

    #[test]
    fn a_list_keeps_its_start_number() {
        assert_eq!(list(&parse("3. foo\n4. bar")[0]).1, 3);
        assert_eq!(list(&parse("- foo")[0]).1, 1);
    }

    #[test]
    fn a_nested_item_after_a_blank_line_nests() {
        let b = parse("- a\n\n  - a1\n- b");
        assert_eq!(b.len(), 1, "{b:?}");
        let (_, _, items) = list(&b[0]);
        assert_eq!(items.len(), 2);
        let (_, _, sub) = list(&items[0].children[0]);
        assert_eq!(sub[0].text, vec![t("a1")]);
    }

    #[test]
    fn a_plus_key_between_keys_and_first() {
        let k = |v: &[&str]| vec![Inline::Keys(v.iter().map(|s| s.to_string()).collect())];
        assert_eq!(inline("<kbd>Ctrl++Shift</kbd>"), k(&["Ctrl", "+", "Shift"]));
        assert_eq!(inline("<kbd>+A</kbd>"), k(&["+", "A"]));
        assert_eq!(inline("<kbd>++A</kbd>"), k(&["+", "A"]));
    }

    #[test]
    fn a_fence_closes_only_at_a_bare_fence_line() {
        let b = parse("```\na\n```rust\nb\n```  \nafter");
        assert_eq!(
            b[0],
            Block::Code {
                lang: String::new(),
                text: "a\n```rust\nb".into()
            }
        );
        assert_eq!(b[1], Block::Paragraph(vec![t("after")]));
    }

    #[test]
    fn a_link_text_may_hold_a_code_span_with_a_bracket() {
        assert_eq!(
            inline("[`]`](keyboard)"),
            vec![Inline::Link {
                text: vec![Inline::Code("]".into())],
                target: Link::Page {
                    page: "keyboard".into(),
                    anchor: None
                }
            }]
        );
    }

    #[test]
    fn table_pipes_escaped_or_in_code_stay_in_the_cell() {
        let b = parse("| `a|b` | c \\| d |\n|---|---|\n| x \\| y | `p|q` |");
        let Block::Table { header, rows, .. } = &b[0] else {
            panic!("{b:?}")
        };
        assert_eq!(header.len(), 2);
        assert_eq!(header[0], vec![Inline::Code("a|b".into())]);
        assert_eq!(header[1], vec![t("c | d")]);
        assert_eq!(
            rows[0],
            vec![vec![t("x | y")], vec![Inline::Code("p|q".into())]]
        );
    }
}
