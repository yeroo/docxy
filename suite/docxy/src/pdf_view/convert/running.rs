//! Running headers and footers (#2257): the text a PDF repeats at the top
//! or bottom of its pages, found on the pages before PdfCraft's export
//! (which has no headers or footers) and taken out of them, to become the
//! converted document's header and footer.
//!
//! A candidate is an unrotated block wholly in the top or bottom [`BAND`]
//! of its page. Two candidates are the same running piece when their text
//! is the same with every digit run read as `#`, they are in the same band,
//! their middles are within [`Y_TOL`] of each other and their left edges,
//! centres or right edges within [`X_TOL`]. A piece is running when it is
//! on at least two pages and on more than half of them. A digit run that is
//! the page's own number on every page it is on is the PAGE field, one that
//! is the page count everywhere NUMPAGES; any other must be the same on
//! every page, or the piece is no running one (its text differs per page:
//! dates, amounts, numbers that do not count the pages from 1) and stays.

#[cfg(test)]
use pdfcraft_export::Block;
use pdfcraft_export::Page;

/// The part of a page's height, at its top and at its bottom, a running
/// block lies wholly in.
pub(crate) const BAND: f64 = 0.12;
/// How far apart (points) the middles of one running piece may be.
pub(crate) const Y_TOL: f64 = 6.0;
/// How far apart (points) the aligned edge of one running piece may be.
pub(crate) const X_TOL: f64 = 12.0;
/// The most candidates one page gives each band, nearest its edge first, so
/// a page of many tiny blocks costs no more than an ordinary one.
const PER_BAND: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Left,
    Centre,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seg {
    Text(String),
    /// The page's number (`PAGE`), and what the first page shows.
    Page(String),
    /// The number of pages (`NUMPAGES`), and what it shows.
    NumPages(String),
}

/// One running piece as its first page has it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Piece {
    pub(crate) segs: Vec<Seg>,
    pub(crate) slot: Slot,
    /// Where it was read: its page and its block there.
    pub(crate) page: usize,
    pub(crate) block: usize,
}

/// What [`find`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Running {
    /// The header's lines, top to bottom, each line's pieces left to right.
    pub(crate) header: Vec<Vec<Piece>>,
    pub(crate) footer: Vec<Vec<Piece>>,
    /// The first page has none of them (a title page): it gets an empty
    /// header and footer of its own.
    pub(crate) title_page: bool,
    /// From the page's top edge to the header's top, and from its bottom
    /// edge to the footer's bottom (points).
    pub(crate) header_from_top: Option<f64>,
    pub(crate) footer_from_bottom: Option<f64>,
    /// Per page, the blocks that are running: they leave the body.
    pub(crate) removed: Vec<Vec<bool>>,
}

impl Running {
    pub(crate) fn is_empty(&self) -> bool {
        self.header.is_empty() && self.footer.is_empty()
    }
}

struct Cand {
    page: usize,
    block: usize,
    top: bool,
    key: String,
    digits: Vec<String>,
    y: f64,
    x: [f64; 3],
}

/// The text with whitespace collapsed and every digit run as `#`, and the
/// digit runs.
fn key_of(text: &str) -> (String, Vec<String>) {
    let mut key = String::new();
    let mut digits = Vec::new();
    let mut run = String::new();
    let mut space = false;
    for c in text.trim().chars() {
        if c.is_ascii_digit() {
            run.push(c);
            continue;
        }
        if !run.is_empty() {
            key.push('#');
            digits.push(std::mem::take(&mut run));
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !key.is_empty() {
            key.push(' ');
        }
        space = false;
        key.push(c);
    }
    if !run.is_empty() {
        if space && !key.is_empty() {
            key.push(' ');
        }
        key.push('#');
        digits.push(run);
    }
    (key, digits)
}

/// The running headers and footers of `pages`; `None` with fewer than two
/// pages or nothing running.
pub(crate) fn find(pages: &[Page]) -> Option<Running> {
    let n = pages.len();
    if n < 2 {
        return None;
    }
    let mut cands = Vec::new();
    for (p, page) in pages.iter().enumerate() {
        let h = page.height;
        if !(h.is_finite() && h > 0.0) {
            continue;
        }
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for (i, b) in page.blocks.iter().enumerate() {
            if b.rotation.abs() > 0.1 || !b.rect.iter().all(|v| v.is_finite()) {
                continue;
            }
            if b.rect[1] >= h * (1.0 - BAND) {
                top.push((h - b.rect[3], i));
            } else if b.rect[3] <= h * BAND {
                bottom.push((b.rect[1], i));
            }
        }
        for (is_top, mut found) in [(true, top), (false, bottom)] {
            found.sort_by(|a, b| a.0.total_cmp(&b.0));
            found.truncate(PER_BAND);
            for (_, i) in found {
                let b = &page.blocks[i];
                let (key, digits) = key_of(&b.text);
                if key.is_empty() {
                    continue;
                }
                cands.push(Cand {
                    page: p,
                    block: i,
                    top: is_top,
                    key,
                    digits,
                    y: (b.rect[1] + b.rect[3]) / 2.0,
                    x: [b.rect[0], (b.rect[0] + b.rect[2]) / 2.0, b.rect[2]],
                });
            }
        }
    }
    cands.sort_by(|a, b| {
        b.top
            .cmp(&a.top)
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.y.total_cmp(&b.y))
    });
    // Groups of one key and band whose middles are within Y_TOL of the
    // group's first, then split by an aligned edge.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut i = 0;
    while i < cands.len() {
        let mut j = i + 1;
        while j < cands.len()
            && cands[j].top == cands[i].top
            && cands[j].key == cands[i].key
            && cands[j].y - cands[i].y <= Y_TOL
        {
            j += 1;
        }
        let mut subs: Vec<Vec<usize>> = Vec::new();
        for k in i..j {
            let fits = |s: &Vec<usize>| {
                let f = &cands[s[0]];
                (0..3).any(|e| (cands[k].x[e] - f.x[e]).abs() <= X_TOL)
            };
            match subs.iter_mut().find(|s| fits(s)) {
                Some(s) => s.push(k),
                None => subs.push(vec![k]),
            }
        }
        groups.extend(subs);
        i = j;
    }
    let mut running = Running {
        removed: pages.iter().map(|p| vec![false; p.blocks.len()]).collect(),
        ..Running::default()
    };
    let mut header = Vec::new();
    let mut footer = Vec::new();
    let mut on_first = false;
    for g in groups {
        let mut on: Vec<usize> = g.iter().map(|&k| cands[k].page).collect();
        on.sort_unstable();
        on.dedup();
        if on.len() < 2 || on.len() * 2 <= n || !digits_repeat(&g, &cands, n) {
            continue;
        }
        for &k in &g {
            running.removed[cands[k].page][cands[k].block] = true;
        }
        on_first |= on[0] == 0;
        let first = g
            .iter()
            .copied()
            .min_by_key(|&k| (cands[k].page, cands[k].block))
            .unwrap_or(g[0]);
        let rep = &cands[first];
        let page = &pages[rep.page];
        let piece = Piece {
            segs: segs(&page.blocks[rep.block].text, &g, &cands, n),
            slot: slot(&g, &cands, page.width),
            page: rep.page,
            block: rep.block,
        };
        let rect = page.blocks[rep.block].rect;
        if rep.top {
            header.push((piece, rect, page.height));
        } else {
            footer.push((piece, rect, page.height));
        }
    }
    running.header_from_top = header
        .iter()
        .map(|(_, r, h)| (h - r[3]).max(0.0))
        .min_by(f64::total_cmp);
    running.footer_from_bottom = footer
        .iter()
        .map(|(_, r, _)| r[1].max(0.0))
        .min_by(f64::total_cmp);
    running.header = lines(header);
    running.footer = lines(footer);
    if running.is_empty() {
        return None;
    }
    running.title_page = n >= 3 && !on_first;
    Some(running)
}

/// Pieces in lines, top to bottom (middles within [`Y_TOL`]), each left to
/// right.
fn lines(mut pieces: Vec<(Piece, [f64; 4], f64)>) -> Vec<Vec<Piece>> {
    let mid = |r: &[f64; 4]| (r[1] + r[3]) / 2.0;
    pieces.sort_by(|a, b| mid(&b.1).total_cmp(&mid(&a.1)));
    let mut out: Vec<Vec<(Piece, [f64; 4])>> = Vec::new();
    let mut line_y = f64::NAN;
    for (p, r, _) in pieces {
        match out.last_mut() {
            Some(line) if (line_y - mid(&r)).abs() <= Y_TOL => line.push((p, r)),
            _ => {
                line_y = mid(&r);
                out.push(vec![(p, r)]);
            }
        }
    }
    out.into_iter()
        .map(|mut line| {
            line.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]));
            line.into_iter().map(|(p, _)| p).collect()
        })
        .collect()
}

/// Where a piece sits: centred when its middle stays within [`X_TOL`] and
/// is near the page's middle, else by the edge that stays within it.
fn slot(g: &[usize], cands: &[Cand], width: f64) -> Slot {
    let f = &cands[g[0]];
    let holds = |e: usize| g.iter().all(|&k| (cands[k].x[e] - f.x[e]).abs() <= X_TOL);
    let middle = (f.x[1] - width / 2.0).abs() <= width * 0.1;
    // Text of one width holds every edge: then where it is decides.
    let right_side = f.x[1] > width * 0.6;
    if holds(1) && middle {
        Slot::Centre
    } else if holds(2) && (right_side || !holds(0)) {
        Slot::Right
    } else if holds(0) {
        Slot::Left
    } else if holds(2) {
        Slot::Right
    } else if holds(1) {
        Slot::Centre
    } else {
        Slot::Left
    }
}

/// What one digit run of a group is across its pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Digits {
    /// Each page's own number: the PAGE field.
    Page,
    /// The page count everywhere: NUMPAGES.
    NumPages,
    /// The same digits on every page: kept as they are.
    Same,
}

/// What digit run `j` of group `g` is; `None` when it differs from page to
/// page and counts nothing (a date, an amount, a Bates number, a count that
/// does not start at 1). The page count is NUMPAGES only beside a page
/// number or after "of" or "/" ("Page 1 of 2", "1/2", "of 2"): elsewhere a
/// number that happens to be the count ("Rev 2" in a 2-page file) is text.
fn digits_of(g: &[usize], cands: &[Cand], n: usize, j: usize) -> Option<Digits> {
    let digits = |j: usize, k: usize| cands[k].digits.get(j);
    let value = |j: usize, k: usize| digits(j, k).and_then(|d| d.parse::<usize>().ok());
    let page = |j: usize| g.iter().all(|&k| value(j, k) == Some(cands[k].page + 1));
    let counted = || {
        let before = cands[g[0]]
            .key
            .split('#')
            .nth(j)
            .unwrap_or("")
            .trim_end()
            .to_lowercase();
        before.ends_with("of")
            || before.ends_with('/')
            || (0..cands[g[0]].digits.len()).any(|i| i != j && page(i))
    };
    if page(j) {
        Some(Digits::Page)
    } else if g.iter().all(|&k| value(j, k) == Some(n)) && counted() {
        Some(Digits::NumPages)
    } else if g.iter().all(|&k| digits(j, k) == digits(j, g[0])) {
        Some(Digits::Same)
    } else {
        None
    }
}

/// Whether every digit run of a group repeats ([`digits_of`]): otherwise
/// the group is no running piece and stays in the body, where each page
/// has its own text.
fn digits_repeat(g: &[usize], cands: &[Cand], n: usize) -> bool {
    (0..cands[g[0]].digits.len()).all(|j| digits_of(g, cands, n, j).is_some())
}

/// A field's [`Seg`] made from what the first page shows.
type Field = fn(String) -> Seg;

/// The first page's text of a running piece, its digit runs as fields
/// where every page's digits say so.
fn segs(text: &str, g: &[usize], cands: &[Cand], n: usize) -> Vec<Seg> {
    let count = cands[g[0]].digits.len();
    let field = |j: usize| -> Option<Field> {
        match digits_of(g, cands, n, j)? {
            Digits::Page => Some(Seg::Page),
            Digits::NumPages => Some(Seg::NumPages),
            Digits::Same => None,
        }
    };
    let fields: Vec<Option<Field>> = (0..count).map(field).collect();
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut run = String::new();
    let mut j = 0;
    let flush_run = |run: &mut String, lit: &mut String, out: &mut Vec<Seg>, j: &mut usize| {
        match fields.get(*j).copied().flatten() {
            Some(f) => {
                if !lit.is_empty() {
                    out.push(Seg::Text(std::mem::take(lit)));
                }
                out.push(f(std::mem::take(run)));
            }
            None => lit.push_str(&std::mem::take(run)),
        }
        *j += 1;
    };
    for c in text.trim().chars() {
        if c.is_ascii_digit() {
            run.push(c);
            continue;
        }
        if !run.is_empty() {
            flush_run(&mut run, &mut lit, &mut out, &mut j);
        }
        lit.push(c);
    }
    if !run.is_empty() {
        flush_run(&mut run, &mut lit, &mut out, &mut j);
    }
    if !lit.is_empty() {
        out.push(Seg::Text(lit));
    }
    out
}

/// Formatting of a header or footer run.
#[derive(Clone, Debug)]
pub(crate) struct RunLook {
    pub(crate) font: Option<String>,
    pub(crate) size: f64,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    /// `RRGGBB`, `None` for black.
    pub(crate) color: Option<String>,
}

impl RunLook {
    /// A run of no look of its own: 11 pt, the document's font, black.
    pub(crate) fn plain() -> RunLook {
        RunLook {
            font: None,
            size: 11.0,
            bold: false,
            italic: false,
            color: None,
        }
    }
}

/// The XML of a header or footer part's paragraphs for `lines`: one
/// paragraph per line in the Header or Footer style, a centre and a right
/// tab stop at the middle and the end of `text_width` (twips); one piece is
/// aligned by its slot, several are put at the stops by tabs.
pub(crate) fn part_xml(
    lines: &[Vec<Piece>],
    is_header: bool,
    text_width: i32,
    look: &dyn Fn(&Piece) -> RunLook,
) -> String {
    let style = crate::hf::style_id(is_header);
    let mut xml = String::new();
    for line in lines {
        xml.push_str(&format!(
            "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/><w:tabs><w:tab w:val=\"center\" w:pos=\"{}\"/>\
             <w:tab w:val=\"right\" w:pos=\"{}\"/></w:tabs><w:spacing w:before=\"0\" w:after=\"0\"/>",
            text_width / 2,
            text_width
        ));
        if let [one] = line.as_slice() {
            match one.slot {
                Slot::Centre => xml.push_str("<w:jc w:val=\"center\"/>"),
                Slot::Right => xml.push_str("<w:jc w:val=\"right\"/>"),
                Slot::Left => {}
            }
        }
        xml.push_str("</w:pPr>");
        let mut at = Slot::Left;
        for (i, piece) in line.iter().enumerate() {
            let rpr = rpr_xml(&look(piece));
            if line.len() > 1 {
                let tabs = (piece.slot as usize).saturating_sub(at as usize);
                for _ in 0..tabs {
                    xml.push_str(&format!("<w:r>{rpr}<w:tab/></w:r>"));
                }
                if tabs == 0 && i > 0 {
                    xml.push_str(&format!(
                        "<w:r>{rpr}<w:t xml:space=\"preserve\"> </w:t></w:r>"
                    ));
                }
                at = piece.slot;
            }
            for seg in &piece.segs {
                match seg {
                    Seg::Text(t) => xml.push_str(&format!(
                        "<w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                        escape(t)
                    )),
                    Seg::Page(shown) | Seg::NumPages(shown) => {
                        let instr = if matches!(seg, Seg::Page(_)) {
                            "PAGE"
                        } else {
                            "NUMPAGES"
                        };
                        xml.push_str(&format!(
                            "<w:fldSimple w:instr=\" {instr} \"><w:r>{rpr}<w:t>{}</w:t></w:r></w:fldSimple>",
                            escape(shown)
                        ));
                    }
                }
            }
        }
        xml.push_str("</w:p>");
    }
    xml
}

/// A run's `w:rPr`, its children in schema order.
fn rpr_xml(look: &RunLook) -> String {
    let mut s = String::from("<w:rPr>");
    if let Some(f) = &look.font {
        let f = escape(f);
        s.push_str(&format!(
            "<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:eastAsia=\"{f}\" w:cs=\"{f}\"/>"
        ));
    }
    if look.bold {
        s.push_str("<w:b/>");
    }
    if look.italic {
        s.push_str("<w:i/>");
    }
    if let Some(c) = &look.color {
        s.push_str(&format!("<w:color w:val=\"{c}\"/>"));
    }
    let half = half_points(look.size);
    s.push_str(&format!(
        "<w:sz w:val=\"{half}\"/><w:szCs w:val=\"{half}\"/></w:rPr>"
    ));
    s
}

/// A size in half points as PdfCraft's export rounds it (12 pt when it is
/// not finite).
pub(crate) fn half_points(size: f64) -> u32 {
    if size.is_finite() {
        (size * 2.0).round().clamp(2.0, 3276.0) as u32
    } else {
        24
    }
}

fn escape(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && c != '\u{FFFE}' && c != '\u{FFFF}')
        })
        .fold(String::with_capacity(s.len()), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                c => out.push(c),
            }
            out
        })
}

/// A block's text alone (tests).
#[cfg(test)]
pub(crate) fn block(text: &str, rect: [f64; 4], size: f64) -> Block {
    Block {
        text: text.into(),
        rect,
        size,
        bold: false,
        italic: false,
        color: [0.0; 3],
        rotation: 0.0,
        space_around: [false; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(blocks: Vec<Block>) -> Page {
        Page {
            width: 612.0,
            height: 792.0,
            blocks,
            ..Page::default()
        }
    }

    fn body(y: f64) -> Block {
        block(
            "Body text that is not repeated.",
            [72.0, y, 400.0, y + 12.0],
            11.0,
        )
    }

    fn header(text: &str, x0: f64, y: f64) -> Block {
        block(text, [x0, y, x0 + text.len() as f64 * 5.0, y + 10.0], 10.0)
    }

    #[test]
    fn a_piece_on_more_than_half_the_pages_runs_and_leaves_the_body() {
        let pages: Vec<Page> = (0..3)
            .map(|i| {
                letter(vec![
                    header("Annual Report", 260.0, 760.0 + i as f64),
                    body(600.0),
                ])
            })
            .collect();
        let r = find(&pages).unwrap();
        assert_eq!(r.header.len(), 1);
        assert_eq!(r.header[0][0].segs, vec![Seg::Text("Annual Report".into())]);
        assert_eq!(r.header[0][0].slot, Slot::Centre);
        assert!(r.footer.is_empty());
        for p in &r.removed {
            assert_eq!(p, &vec![true, false]);
        }
        assert!(!r.title_page);
    }

    #[test]
    fn exactly_half_the_pages_is_not_running() {
        let mut pages: Vec<Page> = (0..4).map(|_| letter(vec![body(600.0)])).collect();
        pages[0].blocks.push(header("Draft", 72.0, 760.0));
        pages[1].blocks.push(header("Draft", 72.0, 760.0));
        assert!(find(&pages).is_none());
        pages[2].blocks.push(header("Draft", 72.0, 760.0));
        assert!(find(&pages).is_some());
    }

    #[test]
    fn positions_cluster_across_a_rounding_edge() {
        // 2.9 and 3.1 would round to different 6 pt buckets.
        let pages = vec![
            letter(vec![header("Notes", 72.0, 762.9), body(600.0)]),
            letter(vec![header("Notes", 72.0, 763.1), body(600.0)]),
        ];
        let r = find(&pages).unwrap();
        assert_eq!(r.header.len(), 1);
        assert_eq!(r.header[0][0].slot, Slot::Left);
    }

    #[test]
    fn page_numbers_and_the_count_become_fields() {
        let pages: Vec<Page> = (1..=3)
            .map(|i| {
                letter(vec![
                    body(600.0),
                    header(&format!("Page {i} of 3"), 280.0, 30.0),
                ])
            })
            .collect();
        let r = find(&pages).unwrap();
        assert_eq!(
            r.footer[0][0].segs,
            vec![
                Seg::Text("Page ".into()),
                Seg::Page("1".into()),
                Seg::Text(" of ".into()),
                Seg::NumPages("3".into())
            ]
        );
        assert_eq!(r.footer_from_bottom, Some(30.0));
        // Digits the same on every page are kept as they are.
        let pages: Vec<Page> = (0..2)
            .map(|_| letter(vec![body(600.0), header("Rev 7 2026", 72.0, 30.0)]))
            .collect();
        let r = find(&pages).unwrap();
        assert_eq!(r.footer[0][0].segs, vec![Seg::Text("Rev 7 2026".into())]);
    }

    #[test]
    fn the_page_count_is_a_field_only_where_it_counts_pages() {
        let footer = |f: &dyn Fn(usize) -> String| -> Vec<Seg> {
            let pages: Vec<Page> = (1..=2)
                .map(|i| letter(vec![body(600.0), header(&f(i), 280.0, 30.0)]))
                .collect();
            find(&pages).unwrap().footer[0][0].segs.clone()
        };
        // "Rev 2" in a 2-page file is text, not the page count.
        assert_eq!(footer(&|_| "Rev 2".into()), vec![Seg::Text("Rev 2".into())]);
        assert_eq!(
            footer(&|i| format!("Page {i} of 2")),
            vec![
                Seg::Text("Page ".into()),
                Seg::Page("1".into()),
                Seg::Text(" of ".into()),
                Seg::NumPages("2".into())
            ]
        );
        assert_eq!(
            footer(&|i| format!("{i}/2")),
            vec![
                Seg::Page("1".into()),
                Seg::Text("/".into()),
                Seg::NumPages("2".into())
            ]
        );
        // After "of" without a page number beside it.
        assert_eq!(
            footer(&|_| "One of 2".into()),
            vec![Seg::Text("One of ".into()), Seg::NumPages("2".into())]
        );
    }

    #[test]
    fn digits_that_differ_per_page_and_count_nothing_stay_in_the_body() {
        // A table's date cell at the same place on every page.
        let pages: Vec<Page> = (0..3)
            .map(|i| {
                letter(vec![
                    body(600.0),
                    header(&format!("2026-11-{:02}", 3 + i * 7), 72.0, 30.0),
                ])
            })
            .collect();
        assert!(find(&pages).is_none());
        // Page numbers that do not start at 1 (front matter, Bates numbers).
        let pages: Vec<Page> = (0..3)
            .map(|i| {
                letter(vec![
                    body(600.0),
                    header(&format!("Page {}", i + 5), 280.0, 30.0),
                ])
            })
            .collect();
        assert!(find(&pages).is_none());
        // A piece whose digits vary stays; a repeating header beside it
        // still runs.
        let pages: Vec<Page> = (0..2)
            .map(|i| {
                letter(vec![
                    header("Annual Report", 260.0, 760.0),
                    body(600.0),
                    header(&format!("Rev {} 2026", 7 + i), 72.0, 30.0),
                ])
            })
            .collect();
        let r = find(&pages).unwrap();
        assert!(r.footer.is_empty());
        assert_eq!(r.removed[0], vec![true, false, false]);
    }

    #[test]
    fn a_first_page_without_them_is_a_title_page() {
        let mut pages: Vec<Page> = (0..3)
            .map(|_| letter(vec![body(600.0), header("Chapter One", 72.0, 760.0)]))
            .collect();
        pages[0].blocks.pop();
        let r = find(&pages).unwrap();
        assert!(r.title_page);
        assert_eq!(r.header[0][0].page, 1);
        // Two pages are never a title page and a body.
        let r = find(&pages[1..]).unwrap();
        assert!(!r.title_page);
    }

    #[test]
    fn one_page_and_unrepeated_text_have_none() {
        assert!(find(&[letter(vec![header("Only", 72.0, 760.0), body(600.0)])]).is_none());
        let pages = vec![
            letter(vec![header("First title", 72.0, 760.0)]),
            letter(vec![header("Second title", 72.0, 760.0)]),
        ];
        assert!(find(&pages).is_none());
    }

    #[test]
    fn pieces_on_one_line_go_to_the_tab_stops() {
        let pages: Vec<Page> = (1..=2)
            .map(|i| {
                letter(vec![
                    header("Field Notes", 72.0, 760.0),
                    header(&format!("{i}"), 535.0, 760.0),
                ])
            })
            .collect();
        let r = find(&pages).unwrap();
        assert_eq!(r.header.len(), 1);
        let line = &r.header[0];
        assert_eq!(line.len(), 2);
        assert_eq!((line[0].slot, line[1].slot), (Slot::Left, Slot::Right));
        let look = |_: &Piece| RunLook {
            font: Some("Arial".into()),
            size: 10.0,
            bold: false,
            italic: false,
            color: None,
        };
        let xml = part_xml(&r.header, true, 9360, &look);
        assert!(
            xml.contains("<w:tab w:val=\"center\" w:pos=\"4680\"/>"),
            "{xml}"
        );
        // Left, then two tabs to the right stop.
        assert_eq!(xml.matches("<w:tab/>").count(), 2, "{xml}");
        assert!(xml.contains("w:instr=\" PAGE \""), "{xml}");
        assert!(xml.contains("<w:rFonts w:ascii=\"Arial\""), "{xml}");
    }
}
