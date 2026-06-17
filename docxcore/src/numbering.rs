//! Parse `word/numbering.xml` and compute real list markers.
//!
//! A paragraph's `numId`/`ilvl` reference a list definition; this module turns
//! that into the actual marker — a nested bullet (`•`/`◦`/`▪`) or a formatted
//! number (`1.`, `a)`, `iii.`, `1.1`) — by walking the document in order and
//! maintaining per-list, per-level counters (deeper levels restart when a
//! shallower level advances), exactly like Word.

use std::collections::HashMap;

use crate::model::{Block, Document};
use crate::xml::{Event, XmlParser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumFmt {
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
}

impl Default for Level {
    fn default() -> Self {
        Level {
            fmt: NumFmt::Decimal,
            text: "%1.".to_string(),
            start: 1,
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

impl Numbering {
    fn marker(
        &self,
        num_id: i32,
        ilvl: i32,
        counters: &mut HashMap<i32, Vec<i32>>,
    ) -> Option<String> {
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

        let level = absn.levels.get(ilvl).cloned().unwrap_or_default();
        if level.fmt == NumFmt::Bullet {
            return Some(bullet_for(ilvl));
        }
        let mut s = level.text.clone();
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
        Some(s)
    }
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
            Block::Raw(_) => {}
        }
        prefix.pop();
    }
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
}
