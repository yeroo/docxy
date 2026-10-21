//! Parse `word/numbering.xml` and compute real list markers.
//!
//! A paragraph's `numId`/`ilvl` reference a list definition; this module turns
//! that into the actual marker — a nested bullet (`•`/`◦`/`▪`) or a formatted
//! number (`1.`, `a)`, `iii.`, `1.1`) — by walking the document in order and
//! maintaining per-list, per-level counters (deeper levels restart when a
//! shallower level advances), exactly like Word.

use std::collections::HashMap;

use crate::model::{Block, Document, Paragraph};
use crate::styles::StyleSheet;
use crate::xml::{Event, XmlParser};

/// A list level's number format (`w:numFmt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumFmt {
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    Bullet,
}

fn parse_fmt(s: &str) -> NumFmt {
    match s {
        "bullet" => NumFmt::Bullet,
        "lowerLetter" => NumFmt::LowerLetter,
        "upperLetter" => NumFmt::UpperLetter,
        "lowerRoman" => NumFmt::LowerRoman,
        "upperRoman" => NumFmt::UpperRoman,
        _ => NumFmt::Decimal,
    }
}

#[derive(Debug, Clone)]
struct Level {
    fmt: NumFmt,
    text: String,
    start: i32,
    /// `w:pStyle`: the paragraph style this level belongs to.
    p_style: Option<String>,
}

impl Default for Level {
    fn default() -> Self {
        Level {
            fmt: NumFmt::Decimal,
            text: "%1.".to_string(),
            start: 1,
            p_style: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct AbstractNum {
    levels: Vec<Level>,
}

#[derive(Debug, Clone, Default)]
pub struct Numbering {
    abstracts: HashMap<i32, AbstractNum>,
    num_to_abstract: HashMap<i32, i32>,
}

/// One list level as `numbering.xml` defines it: what an exporter that
/// writes its own list definitions (RTF) needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelDef {
    pub format: NumFmt,
    /// `w:start`.
    pub start: i32,
    /// `w:lvlText`, `%1`…`%9` standing for the levels' numbers.
    pub text: String,
}

impl Numbering {
    /// Level `ilvl` of list `num_id`, or `None` when the list is not
    /// defined or `ilvl` is past its highest defined level. A level skipped
    /// below the highest one comes back as the default decimal `%1.` level,
    /// as the markers number it.
    pub fn level(&self, num_id: i32, ilvl: i32) -> Option<LevelDef> {
        let abs = self.abstracts.get(self.num_to_abstract.get(&num_id)?)?;
        let level = abs.levels.get(usize::try_from(ilvl).ok()?)?;
        Some(LevelDef {
            format: level.fmt,
            start: level.start,
            text: level.text.clone(),
        })
    }

    fn marker(
        &self,
        num_id: i32,
        ilvl: i32,
        counters: &mut HashMap<i32, Vec<i32>>,
    ) -> Option<String> {
        let (absn, ilvl, c) = self.count(num_id, ilvl, counters)?;
        let level = absn.levels.get(ilvl).cloned().unwrap_or_default();
        if level.fmt == NumFmt::Bullet {
            return Some(bullet_for(ilvl));
        }
        Some(fill(absn, &level.text, c))
    }

    /// Advance list `num_id`'s counters for a paragraph at `ilvl`: that
    /// level counts one more and every deeper level starts over. The list's
    /// definition, the level, and its counters after the step.
    fn count<'c>(
        &self,
        num_id: i32,
        ilvl: i32,
        counters: &'c mut HashMap<i32, Vec<i32>>,
    ) -> Option<(&AbstractNum, usize, &'c [i32])> {
        let abs_id = *self.num_to_abstract.get(&num_id)?;
        let absn = self.abstracts.get(&abs_id)?;
        let ilvl = ilvl.max(0) as usize;

        let c = counters.entry(abs_id).or_default();
        while c.len() <= ilvl {
            let start = absn.levels.get(c.len()).map(|l| l.start).unwrap_or(1);
            c.push(start - 1);
        }
        c[ilvl] += 1;
        for k in (ilvl + 1)..c.len() {
            c[k] = absn.levels.get(k).map(|l| l.start).unwrap_or(1) - 1;
        }
        Some((absn, ilvl, c))
    }

    /// The level of list `num_id` whose `w:pStyle` names `style`.
    fn level_of_style(&self, num_id: i32, style: &str) -> Option<i32> {
        let absn = self.abstracts.get(self.num_to_abstract.get(&num_id)?)?;
        let at = absn
            .levels
            .iter()
            .position(|l| l.p_style.as_deref() == Some(style))?;
        i32::try_from(at).ok()
    }

    /// The chapter number a heading at `ilvl` of `num_id` shows, for a page
    /// number's `w:chapStyle` prefix, after counting it like [`Self::marker`]:
    /// the level's `w:lvlText` with literal text before its first `%`
    /// placeholder and after its last one left out (`%1.` and `Chapter %1`
    /// give `1`, `%1.%2` gives `1.2`). That trimming is this exporter's
    /// choice. `None` for a bullet level or one with no placeholder.
    fn chapter(
        &self,
        num_id: i32,
        ilvl: i32,
        counters: &mut HashMap<i32, Vec<i32>>,
    ) -> Option<String> {
        let (absn, ilvl, c) = self.count(num_id, ilvl, counters)?;
        let level = absn.levels.get(ilvl).cloned().unwrap_or_default();
        if level.fmt == NumFmt::Bullet {
            return None;
        }
        let text = &level.text;
        let marks: Vec<usize> = text
            .match_indices('%')
            .map(|(at, _)| at)
            .filter(|&at| matches!(text.as_bytes().get(at + 1), Some(b'1'..=b'9')))
            .collect();
        let (first, last) = (*marks.first()?, *marks.last()?);
        Some(fill(absn, &text[first..last + 2], c))
    }
}

/// `text` with each `%k` replaced by level k's counter in its format.
fn fill(absn: &AbstractNum, text: &str, c: &[i32]) -> String {
    let mut s = text.to_string();
    for k in 1..=9usize {
        let pat = format!("%{k}");
        if s.contains(&pat) {
            let val = c.get(k - 1).copied().unwrap_or(0).max(1);
            let fmt = absn
                .levels
                .get(k - 1)
                .map(|l| l.fmt)
                .unwrap_or(NumFmt::Decimal);
            s = s.replace(&pat, &format_num(val, fmt));
        }
    }
    s
}

fn bullet_for(ilvl: usize) -> String {
    ["•", "◦", "▪", "‣"][ilvl % 4].to_string()
}

fn format_num(n: i32, fmt: NumFmt) -> String {
    match fmt {
        NumFmt::Decimal => n.to_string(),
        NumFmt::LowerLetter => alpha(n, false),
        NumFmt::UpperLetter => alpha(n, true),
        NumFmt::LowerRoman => roman(n, false),
        NumFmt::UpperRoman => roman(n, true),
        NumFmt::Bullet => String::new(),
    }
}

fn alpha(n: i32, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let mut n = n;
    let mut chars = Vec::new();
    while n > 0 {
        n -= 1;
        chars.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    let s: String = chars.into_iter().rev().collect();
    if upper { s.to_ascii_uppercase() } else { s }
}

fn roman(mut n: i32, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    const VALS: [(i32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    for (v, sym) in VALS {
        while n >= v {
            s.push_str(sym);
            n -= v;
        }
    }
    if upper { s.to_ascii_uppercase() } else { s }
}

fn parse_int(s: &str) -> i32 {
    let b = s.as_bytes();
    let mut v = 0i32;
    let mut i = 0;
    let neg = b.first() == Some(&b'-');
    if neg {
        i = 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add((b[i] - b'0') as i32);
        i += 1;
    }
    if neg { -v } else { v }
}

/// Parse `numbering.xml`.
pub fn parse_numbering_xml(xml: &str) -> Numbering {
    let mut num = Numbering::default();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:abstractNum" => {
                    let id = parse_int(p.attr("w:abstractNumId"));
                    let an = parse_abstract(&mut p);
                    num.abstracts.insert(id, an);
                }
                "w:num" => {
                    let id = parse_int(p.attr("w:numId"));
                    if let Some(abs) = parse_num(&mut p) {
                        num.num_to_abstract.insert(id, abs);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    num
}

fn parse_abstract(p: &mut XmlParser) -> AbstractNum {
    let mut an = AbstractNum::default();
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:lvl" {
                    let ilvl = parse_int(p.attr("w:ilvl")).max(0) as usize;
                    let lvl = parse_level(p);
                    while an.levels.len() <= ilvl {
                        an.levels.push(Level::default());
                    }
                    an.levels[ilvl] = lvl;
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    an
}

fn parse_level(p: &mut XmlParser) -> Level {
    let mut lvl = Level::default();
    loop {
        match p.next() {
            Event::Start => {
                match p.name() {
                    "w:start" => lvl.start = parse_int(p.attr("w:val")),
                    "w:numFmt" => lvl.fmt = parse_fmt(p.attr("w:val")),
                    "w:pStyle" => {
                        let v = p.attr("w:val");
                        if !v.is_empty() {
                            lvl.p_style = Some(v.to_string());
                        }
                    }
                    "w:lvlText" => {
                        let mut t = String::new();
                        XmlParser::append_decoded(p.attr("w:val"), &mut t);
                        lvl.text = t;
                    }
                    _ => {}
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    lvl
}

fn parse_num(p: &mut XmlParser) -> Option<i32> {
    let mut abs = None;
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:abstractNumId" {
                    abs = Some(parse_int(p.attr("w:val")));
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    abs
}

/// Compute the marker text for every list paragraph (keyed by its tree path).
pub fn compute_markers(doc: &Document, num: &Numbering) -> HashMap<Vec<usize>, String> {
    let mut counters: HashMap<i32, Vec<i32>> = HashMap::new();
    let mut out = HashMap::new();
    let mut prefix = Vec::new();
    walk(&doc.body, &mut prefix, num, &mut counters, &mut out);
    out
}

fn walk(
    blocks: &[Block],
    prefix: &mut Vec<usize>,
    num: &Numbering,
    counters: &mut HashMap<i32, Vec<i32>>,
    out: &mut HashMap<Vec<usize>, String>,
) {
    for (i, b) in blocks.iter().enumerate() {
        prefix.push(i);
        match b {
            Block::Paragraph(p) => {
                if let Some(nid) = p.props.num_id {
                    if let Some(m) = num.marker(nid, p.props.ilvl, counters) {
                        out.insert(prefix.clone(), m);
                    }
                }
            }
            Block::Table(t) => {
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        prefix.push(ri);
                        prefix.push(ci);
                        walk(&cell.blocks, prefix, num, counters, out);
                        prefix.pop();
                        prefix.pop();
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
        prefix.pop();
    }
}

/// The chapter number of every heading in `doc`, for page numbers with a
/// `w:chapStyle` prefix: `(heading level, number text)` keyed by the
/// paragraph's address in `doc` (see [`Numbering::chapter`] for the text).
/// An unnumbered heading is there too, with no text: it is still the
/// chapter heading of its page, and that page shows a plain number.
///
/// A paragraph's numbering is its direct `w:numPr`, else its paragraph
/// style's (through `w:basedOn`), as Word numbers its outline headings; a
/// style that names no level takes the one whose `w:pStyle` is the style.
/// A direct `w:numId` of 0 switches the style's numbering off. Every
/// numbered paragraph counts, headings or not, table cells included, so a
/// list sharing the headings' definition moves their numbers as in Word.
/// `w:lvlOverride`/`w:startOverride` are not read, as the list markers
/// don't read them either.
pub(crate) fn heading_numbers(
    doc: &Document,
    num: &Numbering,
    styles: &StyleSheet,
) -> HashMap<*const Paragraph, (u8, Option<String>)> {
    fn walk(
        blocks: &[Block],
        num: &Numbering,
        styles: &StyleSheet,
        counters: &mut HashMap<i32, Vec<i32>>,
        out: &mut HashMap<*const Paragraph, (u8, Option<String>)>,
    ) {
        for block in blocks {
            match block {
                Block::Paragraph(p) => {
                    let style = p.props.style_id.as_deref();
                    let numbering = match p.props.num_id {
                        Some(n) => Some((n, p.props.ilvl)),
                        None => styles.style_num_pr(style).map(|(n, l)| {
                            let l = l.or_else(|| style.and_then(|s| num.level_of_style(n, s)));
                            (n, l.unwrap_or(0))
                        }),
                    }
                    .filter(|&(n, _)| n > 0);
                    match (p.props.heading_level, numbering) {
                        (Some(level), Some((num_id, ilvl))) => {
                            let text = num.chapter(num_id, ilvl, counters);
                            out.insert(p as *const Paragraph, (level, text));
                        }
                        (Some(level), None) => {
                            out.insert(p as *const Paragraph, (level, None));
                        }
                        (None, Some((num_id, ilvl))) => {
                            num.count(num_id, ilvl, counters);
                        }
                        (None, None) => {}
                    }
                }
                Block::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            walk(&cell.blocks, num, styles, counters, out);
                        }
                    }
                }
                Block::SectionProperties(_) | Block::Raw(_) => {}
            }
        }
    }
    let mut out = HashMap::new();
    walk(&doc.body, num, styles, &mut HashMap::new(), &mut out);
    out
}

/// [`heading_numbers`] by top-level body block: `(block index, heading level,
/// chapter number or `None` when unnumbered)` in document order. Only
/// top-level headings are listed: a heading in a table cell still moves the
/// numbering, but never sets a page's chapter (export.rs flattens a table's
/// cells without announcing their headings). For hosts that place pages by
/// body block and cannot hold the paragraph addresses the map is keyed by.
pub fn heading_chapters(
    doc: &Document,
    num: &Numbering,
    styles: &StyleSheet,
) -> Vec<(usize, u8, Option<String>)> {
    let map = heading_numbers(doc, num, styles);
    doc.body
        .iter()
        .enumerate()
        .filter_map(|(i, block)| match block {
            Block::Paragraph(p) => map
                .get(&(p as *const Paragraph))
                .map(|(level, text)| (i, *level, text.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Inline, ParProps, Paragraph, Run, RunProps};

    const NUM_XML: &str = r#"<w:numbering>
        <w:abstractNum w:abstractNumId="0">
            <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
            <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/></w:lvl>
        </w:abstractNum>
        <w:abstractNum w:abstractNumId="1">
            <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="·"/></w:lvl>
        </w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
        <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
        </w:numbering>"#;

    fn list_para(num_id: i32, ilvl: i32) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps {
                num_id: Some(num_id),
                ilvl,
                ..ParProps::default()
            },
            content: vec![Inline::Run(Run {
                text: "x".to_string(),
                props: RunProps::default(),
            })],
        })
    }

    #[test]
    fn decimal_list_numbers_increment() {
        let num = parse_numbering_xml(NUM_XML);
        let doc = Document {
            body: vec![list_para(1, 0), list_para(1, 0), list_para(1, 0)],
        };
        let m = compute_markers(&doc, &num);
        assert_eq!(m.get(&vec![0]).map(String::as_str), Some("1."));
        assert_eq!(m.get(&vec![1]).map(String::as_str), Some("2."));
        assert_eq!(m.get(&vec![2]).map(String::as_str), Some("3."));
    }

    #[test]
    fn nested_levels_and_restart() {
        let num = parse_numbering_xml(NUM_XML);
        // 1.  a)  b)  2.  a)
        let doc = Document {
            body: vec![
                list_para(1, 0),
                list_para(1, 1),
                list_para(1, 1),
                list_para(1, 0),
                list_para(1, 1),
            ],
        };
        let m = compute_markers(&doc, &num);
        assert_eq!(m.get(&vec![0]).map(String::as_str), Some("1."));
        assert_eq!(m.get(&vec![1]).map(String::as_str), Some("a)"));
        assert_eq!(m.get(&vec![2]).map(String::as_str), Some("b)"));
        assert_eq!(m.get(&vec![3]).map(String::as_str), Some("2."));
        assert_eq!(m.get(&vec![4]).map(String::as_str), Some("a)")); // level restarted
    }

    #[test]
    fn bullet_list_uses_nested_glyphs() {
        let num = parse_numbering_xml(NUM_XML);
        let doc = Document {
            body: vec![list_para(2, 0)],
        };
        let m = compute_markers(&doc, &num);
        assert_eq!(m.get(&vec![0]).map(String::as_str), Some("•"));
    }

    #[test]
    fn roman_and_alpha_formatting() {
        assert_eq!(roman(4, false), "iv");
        assert_eq!(roman(9, true), "IX");
        assert_eq!(alpha(1, false), "a");
        assert_eq!(alpha(27, false), "aa");
        assert_eq!(alpha(2, true), "B");
    }

    #[test]
    fn unknown_num_id_is_none() {
        let num = parse_numbering_xml(NUM_XML);
        let doc = Document {
            body: vec![list_para(99, 0)],
        };
        assert!(compute_markers(&doc, &num).is_empty());
    }

    /// An outline list numbering Heading1 (`%1.`) and Heading2 (`%1.%2`)
    /// through their styles, plus a "List Number" style sharing it.
    const OUTLINE_XML: &str = r#"<w:numbering>
        <w:abstractNum w:abstractNumId="5">
            <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:pStyle w:val="Heading1"/><w:lvlText w:val="%1."/></w:lvl>
            <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:pStyle w:val="Heading2"/><w:lvlText w:val="%1.%2"/></w:lvl>
        </w:abstractNum>
        <w:num w:numId="7"><w:abstractNumId w:val="5"/></w:num>
        </w:numbering>"#;

    const OUTLINE_STYLES: &str = r#"<w:styles>
        <w:style w:type="paragraph" w:styleId="Heading1"><w:pPr><w:numPr><w:numId w:val="7"/></w:numPr></w:pPr></w:style>
        <w:style w:type="paragraph" w:styleId="Heading2"><w:basedOn w:val="Heading1"/><w:pPr><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr></w:style>
        <w:style w:type="paragraph" w:styleId="ListNumber"><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="7"/></w:numPr></w:pPr></w:style>
        </w:styles>"#;

    fn styled(style: &str, level: Option<u8>) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps {
                style_id: Some(style.to_string()),
                heading_level: level,
                ..ParProps::default()
            },
            content: vec![],
        })
    }

    /// The chapter numbers of `body`'s headings, in body order.
    fn chapters(body: Vec<Block>, num_xml: &str) -> Vec<Option<(u8, Option<String>)>> {
        let doc = Document { body };
        let num = parse_numbering_xml(num_xml);
        let styles = crate::styles::parse_styles_xml(OUTLINE_STYLES);
        let map = heading_numbers(&doc, &num, &styles);
        doc.body
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => map.get(&(p as *const Paragraph)).cloned(),
                _ => None,
            })
            .collect()
    }

    fn ch(level: u8, text: &str) -> Option<(u8, Option<String>)> {
        Some((level, Some(text.to_string())))
    }

    /// A heading with no number.
    fn bare(level: u8) -> Option<(u8, Option<String>)> {
        Some((level, None))
    }

    #[test]
    fn heading_chapters_list_top_level_headings_by_block() {
        let table = Block::Table(crate::model::Table {
            rows: vec![crate::model::Row {
                cells: vec![crate::model::Cell {
                    blocks: vec![styled("Heading1", Some(1))],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        let doc = Document {
            body: vec![
                styled("Normal", None),
                styled("Heading1", Some(1)),
                table,
                styled("Heading2", Some(2)),
                styled("Plain", Some(1)),
            ],
        };
        let num = parse_numbering_xml(OUTLINE_XML);
        let styles = crate::styles::parse_styles_xml(OUTLINE_STYLES);
        assert_eq!(
            heading_chapters(&doc, &num, &styles),
            vec![
                (1, 1, Some("1".to_string())),
                // The table's heading (numbered 2) is not listed, but moved
                // the counter.
                (3, 2, Some("2.1".to_string())),
                (4, 1, None),
            ]
        );
    }

    #[test]
    fn heading_numbers_follow_style_num_pr() {
        let got = chapters(
            vec![
                styled("Heading1", Some(1)),
                styled("Heading2", Some(2)),
                styled("Heading2", Some(2)),
                styled("Heading1", Some(1)),
                styled("Heading2", Some(2)),
                styled("Normal", None),
            ],
            OUTLINE_XML,
        );
        assert_eq!(
            got,
            vec![
                ch(1, "1"),
                ch(2, "1.1"),
                ch(2, "1.2"),
                ch(1, "2"),
                ch(2, "2.1"),
                None
            ]
        );
        // Direct numbering numbers a heading too, and the list markers
        // still number only direct numbering.
        let mut direct = styled("Plain", Some(1));
        if let Block::Paragraph(p) = &mut direct {
            p.props.num_id = Some(7);
        }
        assert_eq!(
            chapters(vec![direct.clone()], OUTLINE_XML),
            vec![ch(1, "1")]
        );
        let doc = Document {
            body: vec![styled("Heading1", Some(1)), direct],
        };
        let markers = compute_markers(&doc, &parse_numbering_xml(OUTLINE_XML));
        assert_eq!(markers.get(&vec![1]).map(String::as_str), Some("1."));
        assert_eq!(markers.len(), 1);
    }

    #[test]
    fn heading_numbers_trim_literal_text() {
        let xml = |text: &str, fmt: &str| {
            OUTLINE_XML
                .replace(
                    r#"<w:lvlText w:val="%1."/>"#,
                    &format!(r#"<w:lvlText w:val="{text}"/>"#),
                )
                .replacen(
                    r#"<w:numFmt w:val="decimal"/>"#,
                    &format!(r#"<w:numFmt w:val="{fmt}"/>"#),
                    1,
                )
        };
        let one = || vec![styled("Heading1", Some(1))];
        assert_eq!(
            chapters(one(), &xml("Chapter %1", "decimal")),
            vec![ch(1, "1")]
        );
        assert_eq!(
            chapters(one(), &xml("Глава %1:", "decimal")),
            vec![ch(1, "1")]
        );
        assert_eq!(
            chapters(one(), &xml("Part %1.", "upperRoman")),
            vec![ch(1, "I")]
        );
        assert_eq!(chapters(one(), &xml("No number", "decimal")), vec![bare(1)]);
        assert_eq!(chapters(one(), &xml("\u{2022}", "bullet")), vec![bare(1)]);
        let two = vec![styled("Heading1", Some(1)), styled("Heading2", Some(2))];
        let nested = OUTLINE_XML.replace(r#"w:val="%1.%2""#, r#"w:val="(%1.%2)""#);
        assert_eq!(chapters(two, &nested), vec![ch(1, "1"), ch(2, "1.1")]);
    }

    #[test]
    fn heading_numbers_count_style_numbered_list_paragraphs() {
        let got = chapters(
            vec![
                styled("ListNumber", None),
                styled("ListNumber", None),
                styled("Heading1", Some(1)),
            ],
            OUTLINE_XML,
        );
        assert_eq!(got, vec![None, None, ch(1, "3")]);
    }

    #[test]
    fn heading_numbers_num_id_zero_cancels_style_numbering() {
        let mut off = styled("Heading1", Some(1));
        if let Block::Paragraph(p) = &mut off {
            p.props.num_id = Some(0);
        }
        let got = chapters(vec![off, styled("Heading1", Some(1))], OUTLINE_XML);
        assert_eq!(got, vec![bare(1), ch(1, "1")]);
    }
}
