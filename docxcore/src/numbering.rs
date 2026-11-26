//! Parse `word/numbering.xml` and compute real list markers.
//!
//! A paragraph's `numId`/`ilvl` reference a list definition; this module turns
//! that into the actual marker — a nested bullet (`•`/`◦`/`▪`) or a formatted
//! number (`1.`, `a)`, `iii.`, `1.1`) — by walking the document in order and
//! maintaining per-list, per-level counters (deeper levels restart when a
//! shallower level advances), exactly like Word.

use std::collections::HashMap;

use crate::model::{Block, Document, Paragraph, RunProps};
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
    /// `w:pPr/w:ind`: the indent an item at this level takes (#2231).
    indent: LevelIndent,
    /// `w:rPr`: what the level's number is drawn in, over its paragraph
    /// mark's look (#2002).
    run_props: Option<RunProps>,
}

impl Default for Level {
    fn default() -> Self {
        Level {
            fmt: NumFmt::Decimal,
            text: "%1.".to_string(),
            start: 1,
            p_style: None,
            indent: LevelIndent::default(),
            run_props: None,
        }
    }
}

/// A list level's own indent (`w:lvl/w:pPr/w:ind`), in twips: `left` from
/// `w:left` (else `w:start`), `first` from `w:hanging` as a negative first
/// line (else `w:firstLine`). An attribute the level leaves out, or one that
/// is not a number, is `None` (#2231).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelIndent {
    pub left: Option<i32>,
    pub first: Option<i32>,
}

#[derive(Debug, Clone, Default)]
struct AbstractNum {
    levels: Vec<Level>,
    /// `w:styleLink`: the list style this abstract defines.
    style_link: Option<String>,
    /// `w:numStyleLink`: the list style whose abstract (the one whose
    /// `w:styleLink` names it) holds this list's levels (#2231 r2).
    num_style_link: Option<String>,
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
    /// The abstract that holds list `num_id`'s levels, and its id: the one
    /// its `w:num` names, or, when that one only links to a list style
    /// (`w:numStyleLink`), the abstract defining that style (`w:styleLink`),
    /// as Word reads a list made from a list style (#2231 r2). `None` when
    /// the list is not defined or its link resolves to nothing.
    fn resolve(&self, num_id: i32) -> Option<(i32, &AbstractNum)> {
        let id = *self.num_to_abstract.get(&num_id)?;
        let abs = self.abstracts.get(&id)?;
        let Some(style) = abs.num_style_link.as_deref() else {
            return Some((id, abs));
        };
        // One hop: the defining abstract is the one whose own styleLink
        // names the style (never itself, nor another link).
        self.abstracts
            .iter()
            .filter(|&(&other, a)| {
                other != id && a.num_style_link.is_none() && a.style_link.as_deref() == Some(style)
            })
            .min_by_key(|&(&other, _)| other)
            .map(|(&other, a)| (other, a))
    }

    /// The indent level `ilvl` of list `num_id` gives its items, when the
    /// numbering defines that level with a `w:ind`; `None` otherwise (#2231).
    pub fn level_indent(&self, num_id: i32, ilvl: i32) -> Option<LevelIndent> {
        let (_, abs) = self.resolve(num_id)?;
        let level = abs.levels.get(usize::try_from(ilvl).ok()?)?;
        let ind = level.indent;
        (ind.left.is_some() || ind.first.is_some()).then_some(ind)
    }

    /// Level `ilvl` of list `num_id`'s own run properties (`w:lvl/w:rPr`),
    /// a list-style link followed; `None` when the list or the level is not
    /// defined or the level has no `w:rPr` (#2002).
    pub fn level_run_props(&self, num_id: i32, ilvl: i32) -> Option<&RunProps> {
        let (_, abs) = self.resolve(num_id)?;
        abs.levels
            .get(usize::try_from(ilvl).ok()?)?
            .run_props
            .as_ref()
    }

    /// What level `ilvl` of list `num_id` draws its number in (#2002): the
    /// paragraph mark's look `mark` with the level's `w:rPr` over it, as
    /// Word draws it. Of what a list number is drawn in: bold and italic
    /// (on, or an explicit off), colour (an explicit `auto` drops the
    /// mark's), size and font. A bullet level keeps the mark's font: the
    /// view draws its own `•`/`◦`, not the level's `w:lvlText` glyph, which
    /// its font (Symbol, Wingdings) maps. `None` when the level has no
    /// `w:rPr` ([`Self::level_run_props`]): the number is the mark's.
    pub fn marker_props(&self, num_id: i32, ilvl: i32, mark: &RunProps) -> Option<RunProps> {
        use crate::styles::has_raw_child;
        let level = self.level_run_props(num_id, ilvl)?;
        let bullet = self
            .level(num_id, ilvl)
            .is_some_and(|l| l.format == NumFmt::Bullet);
        let mut out = mark.clone();
        let toggle = |on: bool, local: &str, mark: bool| {
            if on {
                true
            } else if has_raw_child(&level.raw_props, local) {
                false
            } else {
                mark
            }
        };
        out.bold = toggle(level.bold, "b", mark.bold);
        out.italic = toggle(level.italic, "i", mark.italic);
        if level.color.is_some() {
            out.color = level.color.clone();
        } else if crate::table_styles::explicit_auto_color(level) {
            out.color = None;
        }
        if level.size_half_pts.is_some() {
            out.size_half_pts = level.size_half_pts;
        }
        if level.font.is_some() && !bullet {
            out.font = level.font.clone();
        }
        Some(out)
    }

    /// The abstract numbering list `num_id` counts in, a list-style link
    /// followed; `None` when the numbering does not define it (#2087). Lists
    /// sharing one abstract share their counts, as [`compute_markers`]
    /// counts them.
    pub fn abstract_of(&self, num_id: i32) -> Option<i32> {
        self.resolve(num_id).map(|(id, _)| id)
    }

    /// Whether the numbering defines list `num_id` (a `w:num` naming an
    /// abstract it has, and a list-style link that resolves).
    pub fn defines(&self, num_id: i32) -> bool {
        self.resolve(num_id).is_some()
    }

    /// Level `ilvl` of list `num_id`, or `None` when the list is not
    /// defined or `ilvl` is past its highest defined level. A level skipped
    /// below the highest one comes back as the default decimal `%1.` level,
    /// as the markers number it.
    pub fn level(&self, num_id: i32, ilvl: i32) -> Option<LevelDef> {
        let (_, abs) = self.resolve(num_id)?;
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
        // Borrowed: a level holds its run properties (#2002).
        let default;
        let level = match absn.levels.get(ilvl) {
            Some(l) => l,
            None => {
                default = Level::default();
                &default
            }
        };
        if level.fmt == NumFmt::Bullet {
            return Some(bullet_for(ilvl));
        }
        Some(fill(absn, &level.text, c))
    }

    /// Advance list `num_id`'s counters for a paragraph at `ilvl`, as
    /// [`compute_markers`] does, and return that level's number (#1948: an
    /// e-mail `<ol>`'s `start`). `None` when the numbering does not define
    /// `num_id`.
    pub(crate) fn step(
        &self,
        num_id: i32,
        ilvl: i32,
        counters: &mut HashMap<i32, Vec<i32>>,
    ) -> Option<i32> {
        let (_, ilvl, c) = self.count(num_id, ilvl, counters)?;
        c.get(ilvl).copied()
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
        let (abs_id, absn) = self.resolve(num_id)?;
        // OOXML has nine levels: a larger `w:ilvl` must not grow the
        // counters to it.
        let ilvl = ilvl.clamp(0, 8) as usize;

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
        let (_, absn) = self.resolve(num_id)?;
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
        // Borrowed: a level holds its run properties (#2002).
        let default;
        let level = match absn.levels.get(ilvl) {
            Some(l) => l,
            None => {
                default = Level::default();
                &default
            }
        };
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
            let fmt = absn
                .levels
                .get(k - 1)
                .map(|l| l.fmt)
                .unwrap_or(NumFmt::Decimal);
            // A decimal counter at 0 draws 0, as Word draws it (#1989): a
            // `w:start 0` level (an HTML `<ol start=0>`), and a parent level
            // not yet advanced (`%1.%2` before any level-0 item is "0.1",
            // Word's "1.0.1" for a heading under a skipped level). A letter
            // or roman numeral has no 0: it draws as 1.
            let least = if fmt == NumFmt::Decimal { 0 } else { 1 };
            let val = c.get(k - 1).copied().map_or(1, |v| v.max(least));
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
            Event::Start => match p.name() {
                "w:lvl" => {
                    let ilvl = parse_int(p.attr("w:ilvl")).max(0) as usize;
                    let lvl = parse_level(p);
                    while an.levels.len() <= ilvl {
                        an.levels.push(Level::default());
                    }
                    an.levels[ilvl] = lvl;
                }
                name @ ("w:styleLink" | "w:numStyleLink") => {
                    let v = p.attr("w:val").trim();
                    let v = (!v.is_empty()).then(|| v.to_string());
                    if name == "w:styleLink" {
                        an.style_link = v;
                    } else {
                        an.num_style_link = v;
                    }
                    p.skip_element();
                }
                _ => p.skip_element(),
            },
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
                    "w:pPr" => {
                        lvl.indent = parse_level_ind(p);
                        continue;
                    }
                    "w:rPr" => {
                        let mut props = RunProps::default();
                        crate::load::parse_rpr(p, &mut props);
                        lvl.run_props = Some(props);
                        continue;
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

/// A level's `w:pPr` (the parser just past its start tag), read to its end
/// tag for the `w:ind`.
fn parse_level_ind(p: &mut XmlParser) -> LevelIndent {
    let mut ind = LevelIndent::default();
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:ind" {
                    (ind.left, ind.first) = crate::styles::ind_twips(p);
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    ind
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

    /// A list's abstract (#2087): the one its `w:num` names, the defining
    /// one for a list-style link, and none for an undefined list.
    #[test]
    fn abstract_of_names_the_abstract_a_list_counts_in_2087() {
        let num = parse_numbering_xml(
            r#"<w:numbering>
            <w:abstractNum w:abstractNumId="5"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
            <w:abstractNum w:abstractNumId="6"><w:styleLink w:val="L"/><w:lvl w:ilvl="0"/></w:abstractNum>
            <w:abstractNum w:abstractNumId="7"><w:numStyleLink w:val="L"/></w:abstractNum>
            <w:num w:numId="3"><w:abstractNumId w:val="5"/></w:num>
            <w:num w:numId="4"><w:abstractNumId w:val="5"/></w:num>
            <w:num w:numId="8"><w:abstractNumId w:val="7"/></w:num>
            </w:numbering>"#,
        );
        assert_eq!(num.abstract_of(3), Some(5));
        assert_eq!(num.abstract_of(4), Some(5));
        assert_eq!(num.abstract_of(8), Some(6));
        assert_eq!(num.abstract_of(9), None);
    }

    /// #2231: a level's `w:pPr/w:ind`, `w:hanging` as a negative first line;
    /// a level without one, or a list not defined, has none.
    #[test]
    fn level_indent_reads_the_levels_ind() {
        let n = parse_numbering_xml(
            r#"<w:numbering><w:abstractNum w:abstractNumId="7">
            <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/>
              <w:pPr><w:tabs><w:tab w:val="num" w:pos="720"/></w:tabs><w:ind w:left="720" w:hanging="360"/></w:pPr>
              <w:rPr><w:rFonts w:ascii="Symbol"/></w:rPr></w:lvl>
            <w:lvl w:ilvl="1"><w:numFmt w:val="decimal"/><w:pPr><w:ind w:start="1440" w:firstLine="200"/></w:pPr></w:lvl>
            <w:lvl w:ilvl="2"><w:numFmt w:val="decimal"/><w:lvlText w:val="%3)"/></w:lvl>
            <w:lvl w:ilvl="3"><w:pPr><w:ind w:left="x" w:hanging="360"/></w:pPr></w:lvl>
            </w:abstractNum><w:num w:numId="4"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#,
        );
        let ind = |l, f| Some(LevelIndent { left: l, first: f });
        assert_eq!(n.level_indent(4, 0), ind(Some(720), Some(-360)));
        assert_eq!(n.level_indent(4, 1), ind(Some(1440), Some(200)));
        assert_eq!(n.level_indent(4, 2), None);
        assert_eq!(n.level_indent(4, 3), ind(None, Some(-360)));
        assert_eq!(n.level_indent(5, 0), None);
        assert!(n.defines(4) && !n.defines(5));
        // The rest of the level still reads past its w:pPr.
        assert_eq!(n.level(4, 2).map(|l| l.text), Some("%3)".to_string()));
        assert_eq!(n.level(4, 0).map(|l| l.format), Some(NumFmt::Bullet));
    }

    /// #2231 r2: a list made from a list style (`w:numStyleLink`) takes the
    /// levels of the abstract whose `w:styleLink` defines that style; a link
    /// that resolves to nothing defines nothing.
    #[test]
    fn a_num_style_link_takes_the_linked_styles_levels() {
        let lvl = |i: i32| {
            format!(
                "<w:lvl w:ilvl=\"{i}\"><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%{}.\"/>\
                 <w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr></w:lvl>",
                i + 1,
                720 * (i + 1)
            )
        };
        let n = parse_numbering_xml(&format!(
            "<w:numbering>\
             <w:abstractNum w:abstractNumId=\"10\"><w:styleLink w:val=\"CorpList\"/>{}{}{}</w:abstractNum>\
             <w:abstractNum w:abstractNumId=\"11\"><w:numStyleLink w:val=\"CorpList\"/></w:abstractNum>\
             <w:abstractNum w:abstractNumId=\"12\"><w:numStyleLink w:val=\"Nowhere\"/></w:abstractNum>\
             <w:num w:numId=\"1\"><w:abstractNumId w:val=\"10\"/></w:num>\
             <w:num w:numId=\"2\"><w:abstractNumId w:val=\"11\"/></w:num>\
             <w:num w:numId=\"3\"><w:abstractNumId w:val=\"12\"/></w:num></w:numbering>",
            lvl(0),
            lvl(1),
            lvl(2)
        ));
        let lefts: Vec<Option<i32>> = (0..3)
            .map(|i| n.level_indent(2, i).and_then(|l| l.left))
            .collect();
        assert_eq!(lefts, [Some(720), Some(1440), Some(2160)]);
        assert_eq!(n.level(2, 1).map(|l| l.text), Some("%2.".to_string()));
        assert!(n.defines(2));
        assert!(!n.defines(3), "an unresolved link defines nothing");
        assert_eq!(n.level_indent(3, 0), None);
        assert_eq!(n.level(3, 0), None);
    }

    /// #2002: a level's `w:rPr` is read (bold, colour, size, font, an
    /// explicit off kept), through a list-style link too; a level without
    /// one, an undefined level or list, has none.
    #[test]
    fn level_rpr_is_read_2002() {
        let n = parse_numbering_xml(
            r#"<w:numbering>
            <w:abstractNum w:abstractNumId="1"><w:styleLink w:val="Corp"/>
              <w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>
                <w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr>
                <w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia"/><w:b/><w:color w:val="ff0000"/><w:sz w:val="28"/></w:rPr></w:lvl>
              <w:lvl w:ilvl="1"><w:numFmt w:val="lowerLetter"/><w:rPr><w:b w:val="0"/><w:i/></w:rPr><w:lvlText w:val="%2)"/></w:lvl>
              <w:lvl w:ilvl="2"><w:numFmt w:val="decimal"/></w:lvl>
            </w:abstractNum>
            <w:abstractNum w:abstractNumId="2"><w:numStyleLink w:val="Corp"/></w:abstractNum>
            <w:num w:numId="3"><w:abstractNumId w:val="1"/></w:num>
            <w:num w:numId="4"><w:abstractNumId w:val="2"/></w:num>
            </w:numbering>"#,
        );
        for num_id in [3, 4] {
            let l0 = n.level_run_props(num_id, 0).expect("level 0 has a w:rPr");
            assert!(l0.bold && !l0.italic);
            assert_eq!(l0.color.as_deref(), Some("FF0000"));
            assert_eq!(l0.size_half_pts, Some(28));
            assert_eq!(l0.font.as_deref(), Some("Georgia"));
            let l1 = n.level_run_props(num_id, 1).expect("level 1 has a w:rPr");
            assert!(!l1.bold && l1.italic);
            assert!(crate::styles::has_raw_child(&l1.raw_props, "b"));
            assert_eq!(n.level_run_props(num_id, 2), None);
            assert_eq!(n.level_run_props(num_id, 3), None);
            assert_eq!(n.level_run_props(num_id, -1), None);
        }
        assert_eq!(n.level_run_props(9, 0), None);
        // The rest of the level still reads past its w:rPr.
        assert_eq!(n.level(3, 1).map(|l| l.text), Some("%2)".to_string()));
        assert_eq!(n.level_indent(3, 0).and_then(|i| i.left), Some(720));
    }

    /// #2002: the level's `w:rPr` over the paragraph mark's look: each of
    /// bold, italic, colour, size and font where the level sets it, the
    /// mark's where it does not; an explicit off and an explicit `auto`
    /// colour clear the mark's; a bullet level keeps the mark's font; no
    /// `w:rPr` gives `None`.
    #[test]
    fn marker_props_lays_the_level_over_the_mark_2002() {
        let n = parse_numbering_xml(
            r#"<w:numbering><w:abstractNum w:abstractNumId="0">
            <w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:rPr><w:rFonts w:ascii="Georgia"/><w:b/><w:color w:val="FF0000"/><w:sz w:val="28"/></w:rPr></w:lvl>
            <w:lvl w:ilvl="1"><w:numFmt w:val="decimal"/><w:rPr><w:b w:val="0"/><w:i w:val="0"/><w:color w:val="auto"/></w:rPr></w:lvl>
            <w:lvl w:ilvl="2"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/><w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol" w:hint="default"/><w:i/><w:color w:val="00B050"/></w:rPr></w:lvl>
            <w:lvl w:ilvl="3"><w:numFmt w:val="decimal"/><w:rPr><w:rFonts w:hint="default"/></w:rPr></w:lvl>
            <w:lvl w:ilvl="4"><w:numFmt w:val="decimal"/></w:lvl>
            </w:abstractNum><w:num w:numId="5"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
        );
        let mark = RunProps {
            bold: true,
            italic: true,
            color: Some("000000".into()),
            size_half_pts: Some(24),
            font: Some("Segoe UI".into()),
            ..RunProps::default()
        };
        let on = n.marker_props(5, 0, &mark).unwrap();
        assert!(on.bold && on.italic);
        assert_eq!(on.color.as_deref(), Some("FF0000"));
        assert_eq!(on.size_half_pts, Some(28));
        assert_eq!(on.font.as_deref(), Some("Georgia"));
        let off = n.marker_props(5, 1, &mark).unwrap();
        assert!(
            !off.bold && !off.italic,
            "an explicit off clears the mark's"
        );
        assert_eq!(off.color, None, "an explicit auto colour clears the mark's");
        assert_eq!(off.size_half_pts, Some(24));
        assert_eq!(off.font.as_deref(), Some("Segoe UI"));
        let bullet = n.marker_props(5, 2, &mark).unwrap();
        assert_eq!(
            bullet.font.as_deref(),
            Some("Segoe UI"),
            "a bullet keeps the mark's font"
        );
        assert!(bullet.bold && bullet.italic);
        assert_eq!(bullet.color.as_deref(), Some("00B050"));
        // A level setting nothing drawn keeps the mark's look whole.
        let hint = n.marker_props(5, 3, &mark).unwrap();
        assert_eq!(
            (
                hint.bold,
                hint.italic,
                hint.color,
                hint.size_half_pts,
                hint.font
            ),
            (
                true,
                true,
                Some("000000".into()),
                Some(24),
                Some("Segoe UI".into())
            )
        );
        // Off where the level is on: on; the mark's plain look elsewhere.
        let plain = RunProps::default();
        let from_plain = n.marker_props(5, 0, &plain).unwrap();
        assert!(from_plain.bold && !from_plain.italic);
        assert_eq!(n.marker_props(5, 4, &mark), None);
        assert_eq!(n.marker_props(5, 9, &mark), None);
        assert_eq!(n.marker_props(6, 0, &mark), None);
    }

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

    /// A decimal counter at 0 draws 0 (#1989), as Word does: a `w:start 0`
    /// level, and a parent level not yet advanced; a letter or roman level
    /// at 0 still draws as 1.
    #[test]
    fn a_decimal_counter_at_zero_draws_zero() {
        let num = parse_numbering_xml(
            r#"<w:numbering>
            <w:abstractNum w:abstractNumId="0">
              <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
              <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%2"/></w:lvl></w:abstractNum>
            <w:abstractNum w:abstractNumId="1">
              <w:lvl w:ilvl="0"><w:start w:val="0"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>
            <w:abstractNum w:abstractNumId="2">
              <w:lvl w:ilvl="0"><w:start w:val="0"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1)"/></w:lvl></w:abstractNum>
            <w:abstractNum w:abstractNumId="3">
              <w:lvl w:ilvl="0"><w:start w:val="0"/><w:numFmt w:val="lowerRoman"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
            <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
            <w:num w:numId="3"><w:abstractNumId w:val="2"/></w:num>
            <w:num w:numId="4"><w:abstractNumId w:val="3"/></w:num>
            </w:numbering>"#,
        );
        let doc = Document {
            body: vec![
                list_para(1, 1),
                list_para(1, 0),
                list_para(1, 1),
                list_para(2, 0),
                list_para(2, 0),
                list_para(3, 0),
                list_para(4, 0),
            ],
        };
        let m = compute_markers(&doc, &num);
        let got: Vec<&str> = (0..7).map(|i| m[&vec![i]].as_str()).collect();
        assert_eq!(got, ["0.1", "1.", "1.1", "0.", "1.", "a)", "i."]);
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

    /// A huge `w:ilvl` is level 8: the counters never grow past nine
    /// levels (#1948 r3).
    #[test]
    fn a_huge_ilvl_is_the_last_level() {
        let n = parse_numbering_xml(
            "<w:numbering><w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\">\
             <w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/></w:lvl></w:abstractNum>\
             <w:num w:numId=\"1\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>",
        );
        let mut counters = HashMap::new();
        assert_eq!(n.step(1, 2_000_000_000, &mut counters), Some(1));
        assert_eq!(n.step(1, i32::MAX, &mut counters), Some(2));
        assert!(counters.values().all(|c| c.len() <= 9), "{counters:?}");
    }
}
