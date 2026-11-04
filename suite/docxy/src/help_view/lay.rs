//! A manual page laid out for drawing (#1855): one [`Row`] per top-level
//! block, so a row's index is its block's (what an `#anchor` scrolls to),
//! with list markers, padded tables, image lookups and inline runs decided
//! here. `render.rs` only maps rows to gpui elements, so everything the
//! viewer decides is tested without a window.

use super::super::help_manual::Bundle;
use helpdocs::md::{Align, Block, Inline, ListItem};

/// The body text size, px.
pub(crate) const BODY_PX: f32 = 14.;

/// A heading's text size by level (1 to 4; deeper levels draw as 4).
pub(crate) fn heading_px(level: u8) -> f32 {
    match level {
        1 => 24.,
        2 => 20.,
        3 => 17.,
        _ => 15.,
    }
}

/// A piece of inline text with its look. `link` is the index of its link in
/// the page's link order ([`helpdocs::md::links`]), which `help-read`'s
/// `links` and the `help-link:<n>` probes use.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Run {
    Text {
        text: String,
        bold: bool,
        italic: bool,
        code: bool,
        link: Option<usize>,
    },
    /// A key chord: each key drawn as a key cap, joined by `+`.
    Keys {
        caps: Vec<String>,
        link: Option<usize>,
    },
}

impl Run {
    pub(crate) fn link(&self) -> Option<usize> {
        match self {
            Run::Text { link, .. } | Run::Keys { link, .. } => *link,
        }
    }
}

/// One list item: its marker (`3.` or a bullet), its text, and what sits
/// under it in the order written.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub marker: String,
    pub runs: Vec<Run>,
    pub children: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    Heading {
        level: u8,
        px: f32,
        slug: String,
        runs: Vec<Run>,
    },
    Para(Vec<Run>),
    List {
        /// 0 at the top; a nested list's items are one deeper.
        depth: usize,
        items: Vec<Item>,
    },
    Code {
        lang: String,
        lines: Vec<String>,
    },
    /// Every row has `cols` cells, the header too, and `align` has one entry
    /// per column.
    Table {
        cols: usize,
        align: Vec<Align>,
        header: Vec<Vec<Run>>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    /// `found`: the bundle holds the file and it is a picture gpui can draw
    /// (PNG, JPEG, GIF, BMP); otherwise a bordered box with the alt text.
    Image {
        src: String,
        alt: String,
        found: bool,
    },
}

/// The bullet of a list at `depth`.
pub(crate) fn bullet(depth: usize) -> &'static str {
    ["\u{2022}", "\u{25E6}", "\u{25AA}"][depth % 3]
}

/// Whether `bytes` are a picture the viewer draws.
pub(crate) fn drawable(bytes: &[u8]) -> bool {
    docxcore::image::sniff(bytes)
        .is_some_and(|i| matches!(i.ext, "png" | "jpg" | "jpeg" | "gif" | "bmp"))
}

/// Lay out `blocks` (a page's, from `bundle`).
pub(crate) fn lay(blocks: &[Block], bundle: &Bundle) -> Vec<Row> {
    let mut links = 0;
    rows(blocks, 0, bundle, &mut links)
}

fn rows(blocks: &[Block], depth: usize, bundle: &Bundle, links: &mut usize) -> Vec<Row> {
    blocks
        .iter()
        .map(|b| row(b, depth, bundle, links))
        .collect()
}

fn row(b: &Block, depth: usize, bundle: &Bundle, links: &mut usize) -> Row {
    match b {
        Block::Heading { level, text, slug } => Row::Heading {
            level: *level,
            px: heading_px(*level),
            slug: slug.clone(),
            runs: runs(text, links),
        },
        Block::Paragraph(text) => Row::Para(runs(text, links)),
        Block::List {
            ordered,
            start,
            items,
        } => Row::List {
            depth,
            items: items
                .iter()
                .enumerate()
                .map(|(i, item)| list_item(item, *ordered, *start, i, depth, bundle, links))
                .collect(),
        },
        Block::Code { lang, text } => Row::Code {
            lang: lang.clone(),
            lines: text.lines().map(str::to_string).collect(),
        },
        Block::Table {
            align,
            header,
            rows,
        } => {
            let cols = rows
                .iter()
                .map(Vec::len)
                .chain([header.len(), align.len()])
                .max()
                .unwrap_or(0);
            let mut cells = |row: &[Vec<Inline>]| {
                let mut out: Vec<Vec<Run>> = row.iter().map(|c| runs(c, links)).collect();
                out.resize(cols, Vec::new());
                out
            };
            let header = cells(header);
            let rows = rows.iter().map(|r| cells(r)).collect();
            let mut align = align.clone();
            align.resize(cols, Align::None);
            Row::Table {
                cols,
                align,
                header,
                rows,
            }
        }
        Block::Image { alt, src } => Row::Image {
            src: src.clone(),
            alt: alt.clone(),
            found: bundle.image(src).is_some_and(|b| drawable(b)),
        },
    }
}

fn list_item(
    item: &ListItem,
    ordered: bool,
    start: u32,
    i: usize,
    depth: usize,
    bundle: &Bundle,
    links: &mut usize,
) -> Item {
    let marker = if ordered {
        format!("{}.", u64::from(start) + i as u64)
    } else {
        bullet(depth).to_string()
    };
    // The item's own text comes before its children, as the page's link
    // order has it.
    let runs = runs(&item.text, links);
    Item {
        marker,
        runs,
        children: rows(&item.children, depth + 1, bundle, links),
    }
}

#[derive(Clone, Copy, Default)]
struct Look {
    bold: bool,
    italic: bool,
    link: Option<usize>,
}

/// The runs of `inlines`, links numbered from `*links` on.
pub(crate) fn runs(inlines: &[Inline], links: &mut usize) -> Vec<Run> {
    let mut out = Vec::new();
    push_runs(inlines, Look::default(), links, &mut out);
    out
}

fn push_runs(inlines: &[Inline], look: Look, links: &mut usize, out: &mut Vec<Run>) {
    for i in inlines {
        match i {
            Inline::Text(t) => push_text(out, t, look, false),
            Inline::Code(t) => push_text(out, t, look, true),
            Inline::Bold(x) => push_runs(x, Look { bold: true, ..look }, links, out),
            Inline::Italic(x) => push_runs(
                x,
                Look {
                    italic: true,
                    ..look
                },
                links,
                out,
            ),
            Inline::Link { text, .. } => {
                let n = *links;
                *links += 1;
                push_runs(
                    text,
                    Look {
                        link: Some(n),
                        ..look
                    },
                    links,
                    out,
                );
            }
            Inline::Keys(k) => out.push(Run::Keys {
                caps: k.clone(),
                link: look.link,
            }),
        }
    }
}

/// Add text, merged into the run before when it has the same look.
fn push_text(out: &mut Vec<Run>, t: &str, look: Look, code: bool) {
    if let Some(Run::Text {
        text,
        bold,
        italic,
        code: c,
        link,
    }) = out.last_mut()
        && (*bold, *italic, *c, *link) == (look.bold, look.italic, code, look.link)
    {
        text.push_str(t);
        return;
    }
    out.push(Run::Text {
        text: t.to_string(),
        bold: look.bold,
        italic: look.italic,
        code,
        link: look.link,
    });
}

/// The text of runs, keys joined by `+` (for tests and the read).
#[cfg(test)]
pub(crate) fn plain(runs: &[Run]) -> String {
    runs.iter()
        .map(|r| match r {
            Run::Text { text, .. } => text.clone(),
            Run::Keys { caps, .. } => caps.join("+"),
        })
        .collect()
}
