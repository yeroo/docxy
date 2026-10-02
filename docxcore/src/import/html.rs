//! HTML → [`Document`] (#633): a Web Page Word saved (Web Page or Web Page,
//! Filtered), or any simple HTML, read with a lenient tag tokenizer rather
//! than a DOM.
//!
//! The encoding comes from a byte-order mark, `<meta charset>` or
//! `http-equiv … charset=`, as UTF-8, UTF-16 or a Windows single-byte page
//! (opccore's tables; ISO-8859-1 reads as 1252, as browsers do). `<head>`,
//! `<style>`, `<script>` and comments are skipped. Word's downlevel blocks
//! (`<![if !supportLists]>…<![endif]>`) are not text: the list marker in
//! one only tells a bullet from a number. Filtered HTML keeps that marker as
//! plain text in a `MsoList…` paragraph, so there it is taken off the front.
//!
//! Kept: `p div h1-h6 li blockquote pre` paragraphs (headings styled),
//! `br`, `ul`/`ol` lists, tables, `b strong i em u s strike del sup sub`
//! and `font-weight` / `font-style` / `text-decoration` in a `style`,
//! alignment. White space collapses as a browser collapses it, except in
//! `pre`. Images, links' targets, CSS classes' formatting and forms are not
//! converted.

use super::{Builder, heading_props, marker_is_numbered};
use crate::model::{Align, Document, ParProps, RunProps, VertAlign};

/// Read an HTML page. `Err` only when it holds no text.
pub fn import_html(bytes: &[u8]) -> Result<Document, String> {
    let text = decode(bytes);
    let mut h = Html::new(&text);
    h.run();
    if h.out.is_empty() {
        return Err("the HTML file holds no text".into());
    }
    let props = h.par_props();
    Ok(h.out.finish(props))
}

/// The page's text, in the encoding it declares.
pub(crate) fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, true);
    }
    match declared_charset(bytes).as_deref() {
        Some("utf-8" | "utf8") => String::from_utf8_lossy(bytes).into_owned(),
        Some(cs) => match code_page(cs) {
            Some(page) => single_byte(bytes, page),
            None => String::from_utf8_lossy(bytes).into_owned(),
        },
        // No declaration: UTF-8 when it is valid, else Windows' Western page.
        None => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => single_byte(bytes, 1252),
        },
    }
}

fn utf16(bytes: &[u8], big_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|p| {
        if big_endian {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            u16::from_le_bytes([p[0], p[1]])
        }
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{fffd}'))
        .collect()
}

fn single_byte(bytes: &[u8], page: i32) -> String {
    bytes
        .iter()
        .map(|&b| opccore::codepage::decode(page, b).unwrap_or('\u{fffd}'))
        .collect()
}

/// The Windows code page a charset label names.
fn code_page(label: &str) -> Option<i32> {
    let l = label.trim().to_ascii_lowercase();
    if let Some(n) = l
        .strip_prefix("windows-")
        .or_else(|| l.strip_prefix("cp"))
        .or_else(|| l.strip_prefix("x-cp"))
    {
        return n.parse().ok();
    }
    Some(match l.as_str() {
        "iso-8859-1" | "iso8859-1" | "latin1" | "l1" | "us-ascii" | "ascii" => 1252,
        "iso-8859-2" | "latin2" => 1250,
        "iso-8859-5" | "koi8-r" => return None,
        "iso-8859-7" => 1253,
        "iso-8859-9" | "latin5" => 1254,
        "tis-620" | "iso-8859-11" => 874,
        _ => return None,
    })
}

/// The `charset` a page declares in its first 4 KB.
fn declared_charset(bytes: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let at = head.find("charset")?;
    let rest = head[at + "charset".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_string())
}

/// What a block element says about the paragraphs in it.
#[derive(Clone, Default)]
struct Block {
    heading: Option<u8>,
    align: Align,
    /// A Word list paragraph (class `MsoList…`): its marker is text in
    /// Filtered HTML.
    mso_list: bool,
    /// Inside an `li`: (numbered, level).
    item: Option<(bool, i32)>,
}

struct Html<'a> {
    s: &'a str,
    pos: usize,
    out: Builder,
    /// Open inline elements and the run properties inside each.
    inline: Vec<(String, RunProps)>,
    blocks: Vec<(String, Block)>,
    /// Open `ul` / `ol`: whether each is ordered.
    lists: Vec<bool>,
    table_depth: usize,
    pre: usize,
    /// A space is owed before the next visible character, in these
    /// properties.
    space: Option<RunProps>,
    /// Nothing visible yet in this paragraph (leading spaces are dropped).
    para_start: bool,
    /// Inside `<![if !supportLists]>`: text is the list marker.
    in_marker: bool,
    marker: Option<String>,
    /// Inside another `<![if …]>` block, dropped.
    in_downlevel: bool,
}

impl<'a> Html<'a> {
    fn new(s: &'a str) -> Self {
        Html {
            s,
            pos: 0,
            out: Builder::new(),
            inline: Vec::new(),
            blocks: Vec::new(),
            lists: Vec::new(),
            table_depth: 0,
            pre: 0,
            space: None,
            para_start: true,
            in_marker: false,
            marker: None,
            in_downlevel: false,
        }
    }

    fn run(&mut self) {
        while self.pos < self.s.len() {
            let rest = &self.s[self.pos..];
            match rest.find('<') {
                Some(0) => self.markup(),
                Some(n) => {
                    let text = &self.s[self.pos..self.pos + n];
                    self.pos += n;
                    self.text(text);
                }
                None => {
                    self.pos = self.s.len();
                    self.text(rest);
                }
            }
        }
    }

    /// Skip to just after `end` (case-insensitive), or to the end.
    fn skip_past(&mut self, end: &str) {
        let rest = &self.s[self.pos..];
        let lower_end = end.to_ascii_lowercase();
        match find_ci(rest, &lower_end) {
            Some(i) => self.pos += i + end.len(),
            None => self.pos = self.s.len(),
        }
    }

    fn markup(&mut self) {
        let rest = &self.s[self.pos..];
        if rest.starts_with("<!--") {
            self.pos += 4;
            return self.skip_past("-->");
        }
        if let Some(cond) = rest.strip_prefix("<![") {
            // `<![if …]>` / `<![endif]>`: Word's downlevel-revealed blocks.
            let end = cond.find("]>").map(|i| i + 2).unwrap_or(cond.len());
            let what = cond[..end].to_ascii_lowercase();
            self.pos += 3 + end;
            if what.starts_with("if") {
                if what.contains("supportlists") {
                    self.in_marker = true;
                    self.marker = Some(String::new());
                } else {
                    self.in_downlevel = true;
                }
            } else if what.starts_with("endif") {
                self.in_marker = false;
                self.in_downlevel = false;
            }
            return;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            return self.skip_past(">");
        }
        // A tag. A '<' not starting one is text.
        let close = rest.as_bytes().get(1) == Some(&b'/');
        let name_start = if close { 2 } else { 1 };
        let name_len = rest[name_start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == ':' || c == '-'))
            .unwrap_or(rest.len() - name_start);
        if name_len == 0 {
            self.pos += 1;
            return self.text("<");
        }
        let name = rest[name_start..name_start + name_len].to_ascii_lowercase();
        let end = tag_end(rest).unwrap_or(rest.len());
        let attrs = &rest[name_start + name_len..end.saturating_sub(1).max(name_start + name_len)];
        self.pos += end;
        if close {
            self.close_tag(&name);
        } else {
            let attrs = attrs.to_string();
            self.open_tag(&name, &attrs);
        }
    }

    fn open_tag(&mut self, name: &str, attrs: &str) {
        match name {
            "head" | "style" | "script" | "title" | "xml" | "noscript" | "template" | "object"
            | "svg" | "iframe" | "select" | "textarea" => {
                let end = format!("</{name}");
                self.skip_past(&end);
                self.skip_past(">");
            }
            "br" => {
                if self.visible() {
                    let props = self.run_props();
                    self.out.line_break(&props);
                    self.space = None;
                    self.para_start = true;
                }
            }
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "blockquote" | "pre"
            | "address" | "dt" | "dd" | "center" | "td" | "th" | "caption" => {
                self.end_para_if_any();
                let mut block = self
                    .blocks
                    .last()
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default();
                if let Some(level) = name.strip_prefix('h').and_then(|n| n.parse::<u8>().ok()) {
                    block.heading = Some(level);
                }
                let class = attr(attrs, "class")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if class.starts_with("mso") {
                    if let Some(level) = class
                        .strip_prefix("msoheading")
                        .and_then(|n| n.parse::<u8>().ok())
                    {
                        block.heading = Some(level);
                    }
                    block.mso_list = class.starts_with("msolist");
                }
                if name == "li" {
                    let numbered = self.lists.last().copied().unwrap_or(false);
                    let level = self.lists.len().saturating_sub(1) as i32;
                    block.item = Some((numbered, level));
                }
                if let Some(a) = align_of(attrs) {
                    block.align = a;
                } else if name == "center" {
                    block.align = Align::Center;
                }
                if name == "pre" {
                    self.pre += 1;
                }
                if name == "td" || name == "th" {
                    // A cell starts with nothing inherited from outside the
                    // table but its alignment.
                    block = Block {
                        align: block.align,
                        ..Block::default()
                    };
                }
                self.blocks.push((name.to_string(), block));
            }
            "ul" | "ol" | "dir" | "menu" => {
                self.end_para_if_any();
                self.lists.push(name == "ol");
            }
            "table" => {
                self.end_para_if_any();
                self.table_depth += 1;
            }
            "tr" => {}
            _ => {
                let mut props = self.run_props();
                let known = match name {
                    "b" | "strong" => {
                        props.bold = true;
                        true
                    }
                    "i" | "em" | "cite" | "dfn" | "var" => {
                        props.italic = true;
                        true
                    }
                    "u" | "ins" => {
                        props.underline = true;
                        true
                    }
                    "s" | "strike" | "del" => {
                        props.strike = true;
                        true
                    }
                    "sup" => {
                        props.vert_align = VertAlign::Superscript;
                        true
                    }
                    "sub" => {
                        props.vert_align = VertAlign::Subscript;
                        true
                    }
                    "span" | "font" | "a" | "small" | "big" | "code" | "tt" | "kbd" | "samp"
                    | "abbr" | "acronym" | "label" | "q" | "mark" | "o:p" => true,
                    _ => false,
                };
                if !known {
                    return;
                }
                if let Some(style) = attr(attrs, "style") {
                    apply_style(&style, &mut props);
                    if name == "span" && style.to_ascii_lowercase().contains("mso-tab-count") {
                        // Word's tab: the spaces inside stand for it.
                        if self.visible() {
                            self.out.tab(&props);
                            self.space = None;
                            self.para_start = false;
                        }
                        self.skip_past("</span");
                        self.skip_past(">");
                        return;
                    }
                }
                self.inline.push((name.to_string(), props));
            }
        }
    }

    fn close_tag(&mut self, name: &str) {
        match name {
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "blockquote" | "pre"
            | "address" | "dt" | "dd" | "center" | "caption" => {
                if let Some(i) = self.blocks.iter().rposition(|(n, _)| n == name) {
                    // A `p` always makes a paragraph, empty or not: Word writes
                    // its empty paragraphs as `<p>&nbsp;</p>`.
                    if name == "p" {
                        self.end_para();
                    } else {
                        self.end_para_if_any();
                    }
                    if name == "pre" {
                        self.pre = self.pre.saturating_sub(1);
                    }
                    self.blocks.truncate(i);
                }
            }
            "td" | "th" => {
                if let Some(i) = self
                    .blocks
                    .iter()
                    .rposition(|(n, _)| n == "td" || n == "th")
                {
                    if self.table_depth == 1 {
                        let props = self.par_props();
                        self.out.end_cell(props);
                        self.reset_para();
                    } else {
                        self.end_para_if_any();
                    }
                    self.blocks.truncate(i);
                }
            }
            "tr" => {
                if self.table_depth == 1 {
                    let props = self.par_props();
                    self.out.end_row(props);
                    self.reset_para();
                }
            }
            "table" => {
                if self.table_depth > 0 {
                    if self.table_depth == 1 {
                        let props = self.par_props();
                        self.out.end_row(props);
                        self.out.end_table();
                    }
                    self.table_depth -= 1;
                }
            }
            "ul" | "ol" | "dir" | "menu" => {
                self.end_para_if_any();
                self.lists.pop();
            }
            _ => {
                if let Some(i) = self.inline.iter().rposition(|(n, _)| n == name) {
                    self.inline.truncate(i);
                }
            }
        }
    }

    fn visible(&self) -> bool {
        !self.in_downlevel && !self.in_marker
    }

    fn run_props(&self) -> RunProps {
        self.inline
            .last()
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    }

    fn text(&mut self, raw: &str) {
        if self.in_downlevel {
            return;
        }
        let text = entities(raw);
        if self.in_marker {
            if let Some(m) = self.marker.as_mut() {
                m.push_str(&text);
            }
            return;
        }
        let props = self.run_props();
        let mut out = String::new();
        for c in text.chars() {
            if self.pre > 0 {
                match c {
                    '\n' => {
                        self.out.text(&out, &props);
                        out.clear();
                        self.out.line_break(&props);
                    }
                    '\r' => {}
                    '\t' => {
                        self.out.text(&out, &props);
                        out.clear();
                        self.out.tab(&props);
                    }
                    _ => out.push(c),
                }
                self.para_start = false;
                continue;
            }
            if c.is_ascii_whitespace() {
                if !self.para_start && self.space.is_none() {
                    self.space = Some(props.clone());
                }
                continue;
            }
            // The owed space is in the formatting it was written in.
            match self.space.take() {
                Some(sp) if sp == props => out.push(' '),
                Some(sp) => self.out.text(" ", &sp),
                None => {}
            }
            self.para_start = false;
            out.push(c);
        }
        self.out.text(&out, &props);
    }

    fn par_props(&self) -> ParProps {
        let block = self
            .blocks
            .last()
            .map(|(_, b)| b.clone())
            .unwrap_or_default();
        let mut p = match block.heading {
            Some(level) => heading_props(level),
            None => ParProps::default(),
        };
        p.align = block.align;
        p
    }

    fn reset_para(&mut self) {
        self.space = None;
        self.para_start = true;
        self.marker = None;
    }

    fn end_para_if_any(&mut self) {
        if !self.out.para_text().is_empty() || self.marker.is_some() {
            self.end_para();
        }
    }

    fn end_para(&mut self) {
        let block = self
            .blocks
            .last()
            .map(|(_, b)| b.clone())
            .unwrap_or_default();
        let mut props = self.par_props();
        let mut marker = self.marker.take();
        if marker.is_none() && block.mso_list && block.heading.is_none() {
            marker = self.out.take_leading_marker();
        }
        if block.heading.is_none() {
            if let Some(m) = &marker {
                props.num_id = Some(if marker_is_numbered(m) { 2 } else { 1 });
            } else if let Some((numbered, level)) = block.item {
                props.num_id = Some(if numbered { 2 } else { 1 });
                props.ilvl = level;
            }
        }
        // Only spaces (Word's `&nbsp;` in an empty paragraph) is empty.
        if self.out.para_text().chars().all(|c| c.is_whitespace()) {
            self.out.clear_para();
        }
        self.out.end_para(props, self.table_depth > 0);
        self.reset_para();
    }
}

/// Where a tag that starts at `s[0]` ends (just past its `>`), skipping
/// quoted attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, &b) in s.as_bytes().iter().enumerate().skip(1) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i + 1),
            None => {}
        }
    }
    None
}

fn find_ci(hay: &str, lower_needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = lower_needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The value of attribute `name` in a tag's attribute text.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let key = &attrs[start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let vs = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                value = attrs[vs..i].to_string();
                i += 1;
            } else {
                let vs = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = attrs[vs..i].to_string();
            }
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(entities(&value));
        }
        if i == start {
            i += 1;
        }
    }
    None
}

fn align_of(attrs: &str) -> Option<Align> {
    let from = |v: &str| match v.trim().to_ascii_lowercase().as_str() {
        "center" => Some(Align::Center),
        "right" => Some(Align::Right),
        "justify" => Some(Align::Justify),
        "left" => Some(Align::Left),
        _ => None,
    };
    if let Some(style) = attr(attrs, "style") {
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                if k.trim().eq_ignore_ascii_case("text-align") {
                    if let Some(a) = from(v) {
                        return Some(a);
                    }
                }
            }
        }
    }
    attr(attrs, "align").and_then(|v| from(&v))
}

fn apply_style(style: &str, props: &mut RunProps) {
    for decl in style.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let v = v.trim().to_ascii_lowercase();
        match k.trim().to_ascii_lowercase().as_str() {
            "font-weight" => {
                props.bold =
                    v == "bold" || v == "bolder" || v.parse::<u32>().is_ok_and(|w| w >= 600);
            }
            "font-style" => props.italic = v == "italic" || v == "oblique",
            "text-decoration" | "text-decoration-line" => {
                if v.contains("none") {
                    props.underline = false;
                    props.strike = false;
                }
                if v.contains("underline") {
                    props.underline = true;
                }
                if v.contains("line-through") {
                    props.strike = true;
                }
            }
            "vertical-align" => match v.as_str() {
                "super" => props.vert_align = VertAlign::Superscript,
                "sub" => props.vert_align = VertAlign::Subscript,
                _ => {}
            },
            _ => {}
        }
    }
}

/// `text` with character references decoded.
fn entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .map(|e| e + 1)
            .unwrap_or(rest.len());
        let name = &rest[1..end];
        let decoded = if let Some(num) = name.strip_prefix('#') {
            let n = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => num.parse::<u32>().ok(),
            };
            n.map(numeric_char)
        } else {
            named(name)
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end..];
                if rest.starts_with(';') {
                    rest = &rest[1..];
                }
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A numeric reference's character; 128-159 are read as Windows-1252, as
/// browsers read them.
fn numeric_char(n: u32) -> char {
    if (0x80..0xA0).contains(&n) {
        return opccore::codepage::decode(1252, n as u8).unwrap_or('\u{fffd}');
    }
    char::from_u32(n)
        .filter(|&c| c != '\0')
        .unwrap_or('\u{fffd}')
}

fn named(name: &str) -> Option<char> {
    Some(match name {
        "nbsp" => '\u{a0}',
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "hellip" => '\u{2026}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201a}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bdquo" => '\u{201e}',
        "bull" => '\u{2022}',
        "middot" => '\u{b7}',
        "euro" => '\u{20ac}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        "deg" => '\u{b0}',
        "plusmn" => '\u{b1}',
        "times" => '\u{d7}',
        "divide" => '\u{f7}',
        "sect" => '\u{a7}',
        "para" => '\u{b6}',
        "cent" => '\u{a2}',
        "pound" => '\u{a3}',
        "yen" => '\u{a5}',
        "iexcl" => '\u{a1}',
        "iquest" => '\u{bf}',
        "shy" => '\u{ad}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "lrm" => '\u{200e}',
        "rlm" => '\u{200f}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "permil" => '\u{2030}',
        "prime" => '\u{2032}',
        "frac12" => '\u{bd}',
        "frac14" => '\u{bc}',
        "frac34" => '\u{be}',
        "sup1" => '\u{b9}',
        "sup2" => '\u{b2}',
        "sup3" => '\u{b3}',
        "micro" => '\u{b5}',
        "ordf" => '\u{aa}',
        "ordm" => '\u{ba}',
        "not" => '\u{ac}',
        "macr" => '\u{af}',
        "acute" => '\u{b4}',
        "cedil" => '\u{b8}',
        "uml" => '\u{a8}',
        "curren" => '\u{a4}',
        "brvbar" => '\u{a6}',
        _ => latin1_letter(name)?,
    })
}

/// The Latin-1 letters by entity name (`eacute`, `Auml`, `szlig`, …).
fn latin1_letter(name: &str) -> Option<char> {
    const LETTERS: [(&str, u32); 62] = [
        ("Agrave", 0xc0),
        ("Aacute", 0xc1),
        ("Acirc", 0xc2),
        ("Atilde", 0xc3),
        ("Auml", 0xc4),
        ("Aring", 0xc5),
        ("AElig", 0xc6),
        ("Ccedil", 0xc7),
        ("Egrave", 0xc8),
        ("Eacute", 0xc9),
        ("Ecirc", 0xca),
        ("Euml", 0xcb),
        ("Igrave", 0xcc),
        ("Iacute", 0xcd),
        ("Icirc", 0xce),
        ("Iuml", 0xcf),
        ("ETH", 0xd0),
        ("Ntilde", 0xd1),
        ("Ograve", 0xd2),
        ("Oacute", 0xd3),
        ("Ocirc", 0xd4),
        ("Otilde", 0xd5),
        ("Ouml", 0xd6),
        ("Oslash", 0xd8),
        ("Ugrave", 0xd9),
        ("Uacute", 0xda),
        ("Ucirc", 0xdb),
        ("Uuml", 0xdc),
        ("Yacute", 0xdd),
        ("THORN", 0xde),
        ("szlig", 0xdf),
        ("agrave", 0xe0),
        ("aacute", 0xe1),
        ("acirc", 0xe2),
        ("atilde", 0xe3),
        ("auml", 0xe4),
        ("aring", 0xe5),
        ("aelig", 0xe6),
        ("ccedil", 0xe7),
        ("egrave", 0xe8),
        ("eacute", 0xe9),
        ("ecirc", 0xea),
        ("euml", 0xeb),
        ("igrave", 0xec),
        ("iacute", 0xed),
        ("icirc", 0xee),
        ("iuml", 0xef),
        ("eth", 0xf0),
        ("ntilde", 0xf1),
        ("ograve", 0xf2),
        ("oacute", 0xf3),
        ("ocirc", 0xf4),
        ("otilde", 0xf5),
        ("ouml", 0xf6),
        ("oslash", 0xf8),
        ("ugrave", 0xf9),
        ("uacute", 0xfa),
        ("ucirc", 0xfb),
        ("uuml", 0xfc),
        ("yacute", 0xfd),
        ("thorn", 0xfe),
        ("yuml", 0xff),
    ];
    LETTERS
        .iter()
        .find(|(n, _)| *n == name)
        .and_then(|&(_, c)| char::from_u32(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::paragraph_texts;
    use crate::model::{Block as DocBlock, Inline, Paragraph};

    fn doc(html: &str) -> Document {
        import_html(html.as_bytes()).unwrap()
    }

    fn paras(d: &Document) -> Vec<&Paragraph> {
        d.body
            .iter()
            .filter_map(|b| match b {
                DocBlock::Paragraph(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn paragraphs_headings_and_whitespace() {
        let d = doc(
            "<html><head><title>T</title><style>p {color:red}</style></head><body>\n<h1>Big   title</h1>\n<p class=MsoNormal>Some\n  text <b>bold</b> and<br>\nmore</p><p>&nbsp;</p><h2 align=center>Sub</h2></body></html>",
        );
        assert_eq!(
            paragraph_texts(&d),
            ["Big title", "Some text bold and\nmore", "", "Sub"]
        );
        let p = paras(&d);
        assert_eq!(p[0].props.style_id.as_deref(), Some("Heading1"));
        assert_eq!(p[3].props.style_id.as_deref(), Some("Heading2"));
        assert_eq!(p[3].props.align, Align::Center);
    }

    #[test]
    fn inline_formatting_nests_and_closes() {
        let d = doc(
            "<p>a <b>b <i>bi</i></b> <span style='font-weight:bold;text-decoration:underline'>bu</span> <u>u</u> <span style=\"font-style:italic\">i</span></p>",
        );
        let p = paras(&d);
        let runs: Vec<(String, bool, bool, bool)> = p[0]
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some((
                    r.text.clone(),
                    r.props.bold,
                    r.props.italic,
                    r.props.underline,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            runs,
            [
                ("a ".into(), false, false, false),
                ("b ".into(), true, false, false),
                ("bi".into(), true, true, false),
                (" ".into(), false, false, false),
                ("bu".into(), true, false, true),
                (" ".into(), false, false, false),
                ("u".into(), false, false, true),
                (" ".into(), false, false, false),
                ("i".into(), false, true, false),
            ]
        );
    }

    #[test]
    fn entities_and_charsets() {
        let d =
            doc("<p>Caf&eacute; &#1055;&#x440; 20&nbsp;&euro; &amp; &lt;x&gt; &#150; &bogus;</p>");
        assert_eq!(
            paragraph_texts(&d),
            ["Caf\u{e9} \u{41f}\u{440} 20\u{a0}\u{20ac} & <x> \u{2013} &bogus;"]
        );
        // windows-1252 bytes, declared in a meta tag.
        let page = b"<html><head><meta http-equiv=Content-Type content=\"text/html; charset=windows-1252\"></head><body><p>Caf\xe9 \x80</p></body></html>";
        assert_eq!(
            paragraph_texts(&import_html(page).unwrap()),
            ["Caf\u{e9} \u{20ac}"]
        );
        // Undeclared and not UTF-8: read as 1252.
        assert_eq!(
            paragraph_texts(&import_html(b"<p>na\xefve</p>").unwrap()),
            ["na\u{ef}ve"]
        );
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "<p>\u{416}</p>".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(paragraph_texts(&import_html(&utf16).unwrap()), ["\u{416}"]);
    }

    #[test]
    fn word_list_markers_are_not_text() {
        // Word's Web Page: the marker in a downlevel block.
        let d = doc(
            "<p class=MsoListParagraphCxSpFirst><![if !supportLists]><span style='font-family:Symbol'>\u{b7}<span>&nbsp;&nbsp; </span></span><![endif]>Apples</p><p class=MsoListParagraphCxSpLast><![if !supportLists]>1.<span>&nbsp; </span><![endif]>First</p>",
        );
        assert_eq!(paragraph_texts(&d), ["Apples", "First"]);
        let p = paras(&d);
        assert_eq!(p[0].props.num_id, Some(1));
        assert_eq!(p[1].props.num_id, Some(2));
        // Filtered HTML: the same marker as plain text in a MsoList paragraph.
        let d = doc(
            "<p class=MsoListParagraphCxSpFirst style='text-indent:-.25in'><span style='font-family:Symbol'>\u{b7}<span style='font:7.0pt \"Times New Roman\"'>&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; </span></span>Apples</p><p class=MsoListParagraphCxSpLast><span>2.<span>&nbsp;&nbsp; </span></span>Second step</p><p class=MsoNormal>1. not a list</p>",
        );
        assert_eq!(
            paragraph_texts(&d),
            ["Apples", "Second step", "1. not a list"]
        );
        let p = paras(&d);
        assert_eq!(p[0].props.num_id, Some(1));
        assert_eq!(p[1].props.num_id, Some(2));
        assert_eq!(p[2].props.num_id, None);
    }

    #[test]
    fn html_lists_and_tables() {
        let d = doc(
            "<ul><li>One</li><li>Two<ol><li>Inner</li></ol></li></ul><table border=1><tr><td><p>North</p></td><td>South</td></tr><tr><td>10</td><td>20</td></tr></table><p>After</p>",
        );
        assert_eq!(
            paragraph_texts(&d),
            ["One", "Two", "Inner", "North", "South", "10", "20", "After"]
        );
        let p = paras(&d);
        assert_eq!(p[0].props.num_id, Some(1));
        assert_eq!(p[2].props.num_id, Some(2));
        assert_eq!(p[2].props.ilvl, 1);
        let DocBlock::Table(t) = &d.body[3] else {
            panic!("{:?}", d.body[3])
        };
        assert_eq!(t.rows.len(), 2);
        assert!(t.rows.iter().all(|r| r.cells.len() == 2));
    }

    #[test]
    fn comments_and_downlevel_blocks_are_dropped() {
        let d = doc(
            "<p>a<!-- hidden --><!--[if gte vml 1]><v:shape>x</v:shape><![endif]-->b<![if !vml]><img src=x><![endif]><![if !supportLineBreakNewLine]><br><![endif]>c<script>var x = '<p>';</script></p>",
        );
        assert_eq!(paragraph_texts(&d), ["abc"]);
    }

    #[test]
    fn pre_keeps_white_space() {
        let d = doc("<pre>a  b\n\tc</pre>");
        assert_eq!(paragraph_texts(&d), ["a  b\n\tc"]);
    }

    #[test]
    fn empty_page_is_an_error_and_cut_input_never_panics() {
        assert!(import_html(b"<html><head><title>x</title></head><body></body></html>").is_err());
        let page = "<html><body><p class=MsoListParagraph style='x'><![if !supportLists]>\u{b7}<![endif]>A&eac<b>B</p><table><tr><td>C</td></tr></table><pre>x</pre></body></html>";
        // Every byte prefix, so cut UTF-8 sequences are tried too.
        for i in 0..=page.len() {
            let _ = import_html(&page.as_bytes()[..i]);
        }
        let _ = import_html(b"<<<>>></ / <a b='></p>&#99999999;&#x;&;");
    }
}
