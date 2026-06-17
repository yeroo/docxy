//! Minimal zero-copy XML pull parser, tuned for OOXML.
//!
//! Ported from rust365 (`src/xml.rs`), unchanged logic, plus unit tests.

pub struct XmlAttr<'a> {
    pub name: &'a str,
    pub value: &'a str, // raw, entities NOT decoded
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Event {
    Start,
    End,
    Text,
    Eof,
}

pub struct XmlParser<'a> {
    xml: &'a [u8],
    pos: usize,
    m_name: &'a str,
    m_text: &'a str,
    m_attrs: Vec<XmlAttr<'a>>,
    pending_end: bool,
    /// Byte index of the `<` of the most recent start tag (for raw capture).
    m_start: usize,
}

fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\r' || c == b'\n'
}
fn is_name_end(c: u8) -> bool {
    is_ws(c) || c == b'>' || c == b'/' || c == b'='
}

fn append_utf8(cp: u32, out: &mut String) {
    if let Some(ch) = char::from_u32(cp) {
        out.push(ch);
    }
}

impl<'a> XmlParser<'a> {
    pub fn new(xml: &'a str) -> Self {
        XmlParser {
            xml: xml.as_bytes(),
            pos: 0,
            m_name: "",
            m_text: "",
            m_attrs: Vec::new(),
            pending_end: false,
            m_start: 0,
        }
    }

    /// Byte index of the `<` of the current start tag.
    pub fn start_pos(&self) -> usize {
        self.m_start
    }
    /// Current byte position (just past the last token consumed).
    pub fn pos(&self) -> usize {
        self.pos
    }
    /// The raw source between two byte positions (for verbatim preservation).
    pub fn raw_slice(&self, a: usize, b: usize) -> &'a str {
        self.slice(a.min(self.xml.len()), b.min(self.xml.len()))
    }

    fn slice(&self, a: usize, b: usize) -> &'a str {
        std::str::from_utf8(&self.xml[a..b]).unwrap_or("")
    }
    fn find_from(&self, from: usize, byte: u8) -> Option<usize> {
        self.xml[from..]
            .iter()
            .position(|&c| c == byte)
            .map(|x| from + x)
    }
    fn starts_with(&self, at: usize, pat: &[u8]) -> bool {
        self.xml.len() >= at + pat.len() && &self.xml[at..at + pat.len()] == pat
    }

    pub fn name(&self) -> &'a str {
        self.m_name
    }
    pub fn text(&self) -> &'a str {
        self.m_text
    }
    pub fn attr(&self, name: &str) -> &'a str {
        self.m_attrs
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value)
            .unwrap_or("")
    }
    pub fn attrs(&self) -> &[XmlAttr<'a>] {
        &self.m_attrs
    }

    pub fn next(&mut self) -> Event {
        if self.pending_end {
            self.pending_end = false;
            return Event::End;
        }
        let size = self.xml.len();
        loop {
            if self.pos >= size {
                return Event::Eof;
            }
            if self.xml[self.pos] != b'<' {
                let start = self.pos;
                let lt = self.find_from(self.pos, b'<').unwrap_or(size);
                self.pos = lt;
                self.m_text = self.slice(start, lt);
                return Event::Text;
            }
            self.pos += 1; // consume '<'
            if self.pos >= size {
                return Event::Eof;
            }
            let c = self.xml[self.pos];

            if c == b'/' {
                self.pos += 1;
                let start = self.pos;
                let gt = match self.find_from(self.pos, b'>') {
                    Some(x) => x,
                    None => return Event::Eof,
                };
                let mut end = start;
                while end < gt && !is_ws(self.xml[end]) {
                    end += 1;
                }
                self.m_name = self.slice(start, end);
                self.pos = gt + 1;
                return Event::End;
            }

            if c == b'?' {
                self.pos = match self.xml[self.pos..].windows(2).position(|w| w == b"?>") {
                    Some(x) => self.pos + x + 2,
                    None => size,
                };
                continue;
            }

            if c == b'!' {
                if self.starts_with(self.pos, b"!--") {
                    self.pos = match self.xml[self.pos + 3..]
                        .windows(3)
                        .position(|w| w == b"-->")
                    {
                        Some(x) => self.pos + 3 + x + 3,
                        None => size,
                    };
                    continue;
                }
                if self.starts_with(self.pos, b"![CDATA[") {
                    let start = self.pos + 8;
                    let e = self.xml[start..]
                        .windows(3)
                        .position(|w| w == b"]]>")
                        .map(|x| start + x);
                    match e {
                        Some(e) => {
                            self.m_text = self.slice(start, e);
                            self.pos = e + 3;
                        }
                        None => {
                            self.m_text = self.slice(start, size);
                            self.pos = size;
                        }
                    }
                    return Event::Text;
                }
                self.pos = match self.find_from(self.pos, b'>') {
                    Some(e) => e + 1,
                    None => size,
                };
                continue;
            }

            // start tag
            let start = self.pos;
            self.m_start = start - 1; // the '<' was consumed just above
            while self.pos < size && !is_name_end(self.xml[self.pos]) {
                self.pos += 1;
            }
            self.m_name = self.slice(start, self.pos);
            self.m_attrs.clear();

            loop {
                while self.pos < size && is_ws(self.xml[self.pos]) {
                    self.pos += 1;
                }
                if self.pos >= size {
                    return Event::Eof;
                }
                let d = self.xml[self.pos];
                if d == b'>' {
                    self.pos += 1;
                    return Event::Start;
                }
                if d == b'/' {
                    self.pos += 1;
                    if self.pos < size && self.xml[self.pos] == b'>' {
                        self.pos += 1;
                    }
                    self.pending_end = true;
                    return Event::Start;
                }
                // attribute
                let as_ = self.pos;
                while self.pos < size && !is_name_end(self.xml[self.pos]) {
                    self.pos += 1;
                }
                let an = self.slice(as_, self.pos);
                while self.pos < size && is_ws(self.xml[self.pos]) {
                    self.pos += 1;
                }
                if self.pos < size && self.xml[self.pos] == b'=' {
                    self.pos += 1;
                    while self.pos < size && is_ws(self.xml[self.pos]) {
                        self.pos += 1;
                    }
                    if self.pos < size
                        && (self.xml[self.pos] == b'"' || self.xml[self.pos] == b'\'')
                    {
                        let q = self.xml[self.pos];
                        self.pos += 1;
                        let vs = self.pos;
                        let ve = match self.find_from(self.pos, q) {
                            Some(x) => x,
                            None => return Event::Eof,
                        };
                        self.m_attrs.push(XmlAttr {
                            name: an,
                            value: self.slice(vs, ve),
                        });
                        self.pos = ve + 1;
                        continue;
                    }
                }
                self.m_attrs.push(XmlAttr {
                    name: an,
                    value: "",
                });
            }
        }
    }

    /// Call immediately after a Start event: consumes through the matching End.
    pub fn skip_element(&mut self) {
        let mut depth = 1;
        while depth > 0 {
            match self.next() {
                Event::Eof => return,
                Event::Start => depth += 1,
                Event::End => depth -= 1,
                _ => {}
            }
        }
    }

    /// Appends `raw` to `out`, decoding the five XML entities and numeric refs.
    pub fn append_decoded(raw: &str, out: &mut String) {
        let b = raw.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'&' {
                // copy one UTF-8 char starting at i
                let ch_len = utf8_len(b[i]);
                let end = (i + ch_len).min(b.len());
                out.push_str(std::str::from_utf8(&b[i..end]).unwrap_or(""));
                i = end;
                continue;
            }
            let semi = raw[i + 1..].find(';').map(|x| i + 1 + x);
            match semi {
                Some(semi) if semi - i <= 12 => {
                    let ent = &raw[i + 1..semi];
                    match ent {
                        "amp" => out.push('&'),
                        "lt" => out.push('<'),
                        "gt" => out.push('>'),
                        "quot" => out.push('"'),
                        "apos" => out.push('\''),
                        _ if ent.starts_with('#') => {
                            let eb = ent.as_bytes();
                            let mut cp = 0u32;
                            let mut ok = ent.len() > 1;
                            if ent.len() > 2 && (eb[1] == b'x' || eb[1] == b'X') {
                                for &h in &eb[2..] {
                                    cp <<= 4;
                                    match h {
                                        b'0'..=b'9' => cp |= (h - b'0') as u32,
                                        b'a'..=b'f' => cp |= (h - b'a' + 10) as u32,
                                        b'A'..=b'F' => cp |= (h - b'A' + 10) as u32,
                                        _ => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                            } else {
                                for &d in &eb[1..] {
                                    if !d.is_ascii_digit() {
                                        ok = false;
                                        break;
                                    }
                                    cp = cp * 10 + (d - b'0') as u32;
                                }
                            }
                            if ok && cp != 0 && cp <= 0x10FFFF {
                                append_utf8(cp, out);
                            }
                        }
                        _ => out.push_str(&raw[i..semi + 1]),
                    }
                    i = semi + 1;
                }
                _ => {
                    out.push('&');
                    i += 1;
                }
            }
        }
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >= 0xF0 {
        4
    } else if b >= 0xE0 {
        3
    } else if b >= 0xC0 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(raw: &str) -> String {
        let mut s = String::new();
        XmlParser::append_decoded(raw, &mut s);
        s
    }

    #[test]
    fn simple_element_with_text() {
        let mut p = XmlParser::new("<a>hi</a>");
        assert_eq!(p.next(), Event::Start);
        assert_eq!(p.name(), "a");
        assert_eq!(p.next(), Event::Text);
        assert_eq!(p.text(), "hi");
        assert_eq!(p.next(), Event::End);
        assert_eq!(p.name(), "a");
        assert_eq!(p.next(), Event::Eof);
    }

    #[test]
    fn self_closing_emits_start_then_end() {
        let mut p = XmlParser::new("<w:br/>");
        assert_eq!(p.next(), Event::Start);
        assert_eq!(p.name(), "w:br");
        assert_eq!(p.next(), Event::End);
        assert_eq!(p.name(), "w:br");
        assert_eq!(p.next(), Event::Eof);
    }

    #[test]
    fn attributes_quoted_both_styles_and_empty() {
        let mut p = XmlParser::new(r#"<w:t xml:space="preserve" a='1' flag>x</w:t>"#);
        assert_eq!(p.next(), Event::Start);
        assert_eq!(p.name(), "w:t");
        assert_eq!(p.attr("xml:space"), "preserve");
        assert_eq!(p.attr("a"), "1");
        assert_eq!(p.attr("flag"), "");
        assert_eq!(p.attr("missing"), "");
        assert_eq!(p.attrs().len(), 3);
    }

    #[test]
    fn skips_prolog_comments_and_pi() {
        let xml = "<?xml version=\"1.0\"?><!-- c --><root><?pi data?>t</root>";
        let mut p = XmlParser::new(xml);
        assert_eq!(p.next(), Event::Start);
        assert_eq!(p.name(), "root");
        assert_eq!(p.next(), Event::Text);
        assert_eq!(p.text(), "t");
        assert_eq!(p.next(), Event::End);
    }

    #[test]
    fn cdata_is_text() {
        let mut p = XmlParser::new("<a><![CDATA[<b>&raw]]></a>");
        assert_eq!(p.next(), Event::Start);
        assert_eq!(p.next(), Event::Text);
        assert_eq!(p.text(), "<b>&raw");
        assert_eq!(p.next(), Event::End);
    }

    #[test]
    fn skip_element_consumes_nested() {
        let mut p = XmlParser::new("<r><a><b/></a></r><next/>");
        assert_eq!(p.next(), Event::Start); // <r>
        assert_eq!(p.next(), Event::Start); // <a>
        p.skip_element(); // consumes <b/> and </a>
        assert_eq!(p.next(), Event::End); // </r>
        assert_eq!(p.name(), "r");
        assert_eq!(p.next(), Event::Start); // <next/>
        assert_eq!(p.name(), "next");
    }

    #[test]
    fn entity_decoding_named() {
        assert_eq!(
            decode("a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;"),
            "a & b <c> \"d\" 'e'"
        );
    }

    #[test]
    fn entity_decoding_numeric_dec_and_hex() {
        assert_eq!(decode("&#65;&#x42;&#x263A;"), "AB\u{263A}");
    }

    #[test]
    fn lone_ampersand_is_literal() {
        assert_eq!(decode("Tom & Jerry"), "Tom & Jerry");
    }

    #[test]
    fn unknown_entity_passed_through() {
        assert_eq!(decode("&nbsp;"), "&nbsp;");
    }

    #[test]
    fn realistic_run_sequence() {
        // A simplified WordprocessingML run.
        let xml = "<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold&amp;text</w:t></w:r></w:p>";
        let mut p = XmlParser::new(xml);
        let mut names_started = Vec::new();
        let mut text = String::new();
        loop {
            match p.next() {
                Event::Start => names_started.push(p.name().to_string()),
                Event::Text => XmlParser::append_decoded(p.text(), &mut text),
                Event::End => {}
                Event::Eof => break,
            }
        }
        assert_eq!(names_started, ["w:p", "w:r", "w:rPr", "w:b", "w:t"]);
        assert_eq!(text, "Bold&text");
    }
}
