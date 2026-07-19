//! Turns an email body (HTML or plain text) into a small, neutral
//! "styled line" representation that a terminal UI can render without this
//! crate knowing anything about ratatui: [`render_html`] for HTML bodies,
//! [`render_text`] for `text/plain` ones.
//!
//! This is a readable reduction, not a browser: a lenient hand-rolled
//! tokenizer (in the spirit of `opccore::xml`'s pull parser, but HTML-messy
//! rather than XML-strict — see [`Tokenizer`]) walks the markup maintaining
//! a small amount of state (bold/italic/underline depth, blockquote/list
//! indent, an open-link stack) and emits word-wrapped [`StyledLine`]s.
//! Unknown tags are transparent: their children still render, just without
//! any special treatment. `<script>`/`<style>`/`<head>`/`<title>` content is
//! dropped outright (real HTML mail nearly always carries a `<style>` block
//! whose CSS would otherwise show up as garbage text).
//!
//! Wrapping is by `char` count, not display width — this crate is plain
//! `std` with no `unicode-width` dependency, so a wide CJK/emoji glyph is
//! counted as one column here. lookxy already depends on `unicode-width`
//! for its own layout math; if wrapping needs to get that precise, it
//! should move there instead of pulling the dependency into `mailcore`.

/// One inline run of text plus the styling flags active when it was
/// produced, and the link target if it sits inside an `<a href>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StyledSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub link: Option<String>,
}

/// One rendered row: its spans, plus a blockquote/list nesting depth. The
/// UI turns `indent` into leading spaces (`indent as usize * N`); this
/// module doesn't hardcode a column count since that's a display concern.
/// `image` is `Some` only for the special marker line an `<img>` produces
/// (see `render_html`'s `"img"` handling) — such a line always has empty
/// `spans`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
    pub indent: u8,
    pub image: Option<ImageRef>,
}

/// The source of an `<img>` in a rendered body.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageSource {
    /// `src="cid:X"` — an inline attachment, resolved by `Content-ID`.
    Cid(String),
    /// `src="data:<mime>;base64,<b64>"` — bytes already decoded here.
    Data { mime: String, bytes: Vec<u8> },
    /// `src="http(s)://…"` — a remote image, deliberately NOT fetched
    /// (tracking-pixel protection); the consumer shows a box.
    Remote(String),
    /// Any other/malformed `src` — the consumer shows a box.
    Unsupported,
}

/// One `<img>` from a body: its source plus the `alt` text (for the fallback
/// box caption).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub src: ImageSource,
    pub alt: String,
}

/// Classifies an `<img src>` value into an [`ImageSource`]. `data:` URIs are
/// base64-decoded here (bounded work); `cid:` keeps the bare id; `http(s)`
/// is marked remote and never fetched.
fn classify_img_src(src: &str) -> ImageSource {
    let s = src.trim();
    if let Some(cid) = s.strip_prefix("cid:").or_else(|| s.strip_prefix("CID:")) {
        return ImageSource::Cid(cid.to_string());
    }
    if let Some(rest) = s.strip_prefix("data:") {
        // rest = "<mime>;base64,<b64>"
        if let Some((meta, b64)) = rest.split_once(',') {
            let is_b64 = meta.rsplit(';').any(|p| p.eq_ignore_ascii_case("base64"));
            let mime = meta.split(';').next().unwrap_or("").to_string();
            if is_b64 && !mime.is_empty() {
                if let Some(bytes) = crate::graph::client::base64_decode(b64.trim()) {
                    return ImageSource::Data { mime, bytes };
                }
            }
        }
        return ImageSource::Unsupported;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return ImageSource::Remote(s.to_string());
    }
    ImageSource::Unsupported
}

/// Spaces per indent level. This module subtracts `indent * INDENT_SPACES`
/// from `width` before wrapping an indented block, so a consumer that
/// prints exactly this many leading spaces per `StyledLine::indent` level
/// (lookxy's `ui::reading` does) gets wrapped text that, plus that leading
/// whitespace, still fits inside `width` — hence `pub`: the leading-space
/// count and the wrap-width math must stay in lockstep.
pub const INDENT_SPACES: usize = 2;

/// Tags whose entire subtree is dropped: never contributes words, never
/// changes bold/indent/link state. Covers the common case of a `<style>`
/// block in the body (Outlook loves to inline one) and a stray `<head>`
/// in a malformed/full-document body.
pub(crate) const SKIP_CONTENT_TAGS: [&str; 4] = ["script", "style", "head", "title"];

/// A single word (a whitespace-delimited run of characters) plus the
/// styling/link state active when it was collected. The unit `render_html`
/// accumulates per block and `wrap_words` consumes to build `StyledLine`s.
#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
    link: Option<String>,
}

/// Renders an HTML email body into word-wrapped [`StyledLine`]s at most
/// `width` columns wide (see the module doc comment for the wrapping
/// caveat). Handles `<p> <br> <b>/<strong> <i>/<em> <u> <a href>
/// <blockquote> <ul>/<ol>/<li> <table>/<tr>/<td>`; anything else is
/// transparent. `<a href>` text becomes `text[n]`, with a `[n] url`
/// footnote appendix appended after a blank line.
pub fn render_html(html: &str, width: usize) -> Vec<StyledLine> {
    let width = width.max(1);
    let mut tok = Tokenizer::new(html);
    let mut lines: Vec<StyledLine> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut indent: u8 = 0;
    let mut bold: u32 = 0;
    let mut italic: u32 = 0;
    let mut underline: u32 = 0;
    // (href, footnote number); number is 0 for an `<a>` with no/empty href
    // (no footnote to append on close).
    let mut link_stack: Vec<(String, usize)> = Vec::new();
    let mut footnotes: Vec<String> = Vec::new();
    let mut cells_in_row: u32 = 0;
    // While `Some(tag)`, we're inside a dropped subtree rooted at `tag`
    // (see `SKIP_CONTENT_TAGS`); `depth` counts re-opens of that same tag
    // so nesting (unlikely but cheap to handle) still closes correctly.
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
                for w in t.split_whitespace() {
                    words.push(Word {
                        text: w.to_string(),
                        bold: bold > 0,
                        italic: italic > 0,
                        underline: underline > 0,
                        link: link_stack
                            .last()
                            .and_then(|(h, n)| (*n > 0).then(|| h.clone())),
                    });
                }
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
                    "br" => flush(&mut lines, &mut words, indent, width),
                    "p" => flush(&mut lines, &mut words, indent, width),
                    "blockquote" => {
                        flush(&mut lines, &mut words, indent, width);
                        indent = indent.saturating_add(1);
                    }
                    "ul" | "ol" => {
                        flush(&mut lines, &mut words, indent, width);
                        indent = indent.saturating_add(1);
                    }
                    "li" => {
                        flush(&mut lines, &mut words, indent, width);
                        words.push(Word {
                            text: "-".to_string(),
                            ..Default::default()
                        });
                    }
                    "table" => flush(&mut lines, &mut words, indent, width),
                    "tr" => {
                        flush(&mut lines, &mut words, indent, width);
                        cells_in_row = 0;
                    }
                    "td" | "th" => {
                        if cells_in_row > 0 {
                            words.push(Word {
                                text: "|".to_string(),
                                ..Default::default()
                            });
                        }
                        cells_in_row += 1;
                    }
                    "a" => {
                        let href = attrs
                            .iter()
                            .find(|(k, _)| k == "href")
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default();
                        if href.is_empty() {
                            link_stack.push((String::new(), 0));
                        } else {
                            let n = footnotes.len() + 1;
                            footnotes.push(href.clone());
                            link_stack.push((href, n));
                        }
                    }
                    "img" => {
                        flush(&mut lines, &mut words, indent, width);
                        let src = attrs
                            .iter()
                            .find(|(k, _)| k == "src")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("");
                        let alt = attrs
                            .iter()
                            .find(|(k, _)| k == "alt")
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default();
                        lines.push(StyledLine {
                            spans: Vec::new(),
                            indent,
                            image: Some(ImageRef {
                                src: classify_img_src(src),
                                alt,
                            }),
                        });
                    }
                    _ => {}
                }
            }
            Token::TagClose { name } => match name.as_str() {
                "b" | "strong" => bold = bold.saturating_sub(1),
                "i" | "em" => italic = italic.saturating_sub(1),
                "u" => underline = underline.saturating_sub(1),
                "p" => {
                    flush(&mut lines, &mut words, indent, width);
                    push_blank_separator(&mut lines);
                }
                "blockquote" => {
                    flush(&mut lines, &mut words, indent, width);
                    indent = indent.saturating_sub(1);
                    push_blank_separator(&mut lines);
                }
                "ul" | "ol" => {
                    flush(&mut lines, &mut words, indent, width);
                    indent = indent.saturating_sub(1);
                    push_blank_separator(&mut lines);
                }
                "li" => flush(&mut lines, &mut words, indent, width),
                "table" => {
                    flush(&mut lines, &mut words, indent, width);
                    push_blank_separator(&mut lines);
                }
                "tr" => flush(&mut lines, &mut words, indent, width),
                "a" => {
                    if let Some((href, n)) = link_stack.pop() {
                        if n > 0 {
                            let marker = format!("[{n}]");
                            match words.last_mut() {
                                Some(last) if last.link.as_deref() == Some(href.as_str()) => {
                                    last.text.push_str(&marker);
                                }
                                _ => words.push(Word {
                                    text: marker,
                                    link: Some(href),
                                    ..Default::default()
                                }),
                            }
                        }
                    }
                }
                _ => {}
            },
        }
    }

    flush(&mut lines, &mut words, indent, width);

    if !footnotes.is_empty() {
        push_blank_separator(&mut lines);
        for (i, url) in footnotes.iter().enumerate() {
            lines.push(StyledLine {
                indent: 0,
                spans: vec![StyledSpan {
                    text: format!("[{}] {}", i + 1, url),
                    link: Some(url.clone()),
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
    }

    trim_trailing_blank(&mut lines);
    lines
}

/// Every `<img>` in `html` as an [`ImageRef`], in document order — used by the
/// reader to trigger inline-image fetches without caring about wrap width.
pub fn image_refs(html: &str) -> Vec<ImageRef> {
    let mut tok = Tokenizer::new(html);
    let mut out = Vec::new();
    loop {
        match tok.next() {
            Token::Eof => break,
            Token::TagOpen { name, attrs, .. } if name == "img" => {
                let src = attrs
                    .iter()
                    .find(|(k, _)| k == "src")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                let alt = attrs
                    .iter()
                    .find(|(k, _)| k == "alt")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                out.push(ImageRef {
                    src: classify_img_src(src),
                    alt,
                });
            }
            _ => {}
        }
    }
    out
}

/// Word-wraps a `text/plain` body to `width` columns. Lines whose original
/// text starts with one or more `>` (the universal plain-text reply-quote
/// marker) are indented one level per `>`, mirroring `<blockquote>`'s
/// indentation for HTML bodies — plain-text reply chains get the same
/// visual nesting.
pub fn render_text(plain: &str, width: usize) -> Vec<StyledLine> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in plain.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let mut depth: u8 = 0;
        let mut rest = raw;
        while let Some(r) = rest.strip_prefix('>') {
            depth = depth.saturating_add(1);
            rest = r.strip_prefix(' ').unwrap_or(r);
        }
        let effective = width.saturating_sub(depth as usize * INDENT_SPACES).max(1);
        let words: Vec<Word> = rest
            .split_whitespace()
            .map(|w| Word {
                text: w.to_string(),
                ..Default::default()
            })
            .collect();
        if words.is_empty() {
            lines.push(StyledLine {
                spans: vec![],
                indent: depth,
                ..Default::default()
            });
            continue;
        }
        for spans in wrap_words(&words, effective) {
            lines.push(StyledLine {
                spans,
                indent: depth,
                ..Default::default()
            });
        }
    }
    lines
}

/// Word-wraps the accumulated `words` for the current block (at `indent`)
/// into `StyledLine`s and appends them to `lines`. A no-op when `words` is
/// empty (an unclosed/empty block, or two block tags back to back) so
/// callers can call this unconditionally at every block boundary.
fn flush(lines: &mut Vec<StyledLine>, words: &mut Vec<Word>, indent: u8, width: usize) {
    if words.is_empty() {
        return;
    }
    let taken = std::mem::take(words);
    let effective = width.saturating_sub(indent as usize * INDENT_SPACES).max(1);
    for spans in wrap_words(&taken, effective) {
        lines.push(StyledLine {
            spans,
            indent,
            ..Default::default()
        });
    }
}

/// Greedy word-wrap: packs `words` onto lines of at most `width` chars
/// (a word longer than `width` still gets its own line rather than being
/// split, so it overflows rather than being mangled). Each word after the
/// first on a line gets a leading space folded into its own span's text —
/// so a plain concatenation of a line's span texts reproduces normal
/// spacing without needing separate space-only spans.
fn wrap_words(words: &[Word], width: usize) -> Vec<Vec<StyledSpan>> {
    let mut out = Vec::new();
    let mut cur: Vec<StyledSpan> = Vec::new();
    let mut cur_len = 0usize;
    for w in words {
        let wlen = w.text.chars().count();
        if !cur.is_empty() && cur_len + 1 + wlen > width {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        let text = if cur.is_empty() {
            w.text.clone()
        } else {
            format!(" {}", w.text)
        };
        cur_len += text.chars().count();
        cur.push(StyledSpan {
            text,
            bold: w.bold,
            italic: w.italic,
            underline: w.underline,
            link: w.link.clone(),
        });
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Appends a blank separator line — but only if `lines` already has
/// content and its last line isn't already blank, so paragraph/list/table
/// spacing never opens with a blank line or stacks up multiple in a row.
fn push_blank_separator(lines: &mut Vec<StyledLine>) {
    if lines.last().map(|l| !l.spans.is_empty()).unwrap_or(false) {
        lines.push(StyledLine::default());
    }
}

/// Drops trailing blank lines (paragraph/list/table spacing that ended up
/// with nothing after it — normal at the end of a document).
fn trim_trailing_blank(lines: &mut Vec<StyledLine>) {
    while lines
        .last()
        .map(|l| l.spans.is_empty() && l.image.is_none())
        .unwrap_or(false)
    {
        lines.pop();
    }
}

/// A markup token: the same event shape as `opccore::xml::XmlParser`
/// (start/end/text/eof) but produced by an HTML-lenient scanner rather
/// than an XML one — see [`Tokenizer`].
///
/// `pub(crate)`: reused by `crate::compose_html::from_html`, which walks
/// the same token stream to build a `RichText` instead of `StyledLine`s —
/// see that module rather than duplicating this scanner.
#[derive(Debug, PartialEq)]
pub(crate) enum Token {
    TagOpen {
        name: String,
        attrs: Vec<(String, String)>,
        self_close: bool,
    },
    TagClose {
        name: String,
    },
    Text(String),
    Eof,
}

/// A small hand-rolled, lenient HTML scanner. It adapts the pull-parser
/// shape of `opccore::xml::XmlParser` (byte-position scanning, one token
/// per `next()` call) but diverges where HTML actually is messier than
/// XML:
///
/// - Tags don't need a closing `</tag>` at all (`<br>`, an unclosed `<p>`)
///   — this module never assumes a `TagClose` follows every `TagOpen`;
///   every bit of state it tracks (bold/italic/underline depth, indent,
///   the link stack) is adjusted independently by tag *name*, not by tree
///   position, so a missing close just never decrements what its open
///   incremented (each such counter saturates at zero rather than
///   underflowing).
/// - Attribute values may be unquoted (`href=https://x`) — quoted values
///   stop at the matching quote; unquoted ones stop at whitespace or `>`
///   (not `/`, unlike an XML tag-name terminator would, since `/` is
///   common inside a bare URL and stopping on it there would misparse the
///   rest of the attribute list).
/// - Comments/doctype/processing instructions/unterminated tags never
///   leave `pos` unmoved: every branch below either finds its closing
///   delimiter and jumps past it, or — if the input ends first — jumps to
///   the end of the string. That, plus the fact that a token is always
///   produced (never recursion) on each `next()` call, is what keeps this
///   from ever hanging on malformed input; there's a test
///   (`malformed_html_does_not_panic`) exercising a handful of gnarly
///   cases (unclosed tags, unquoted values with special characters, bare
///   `<`/`>` runs) to guard it.
pub(crate) struct Tokenizer<'a> {
    bytes: &'a [u8],
    pos: usize,
}

fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\r' || c == b'\n'
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Tokenizer {
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn slice(&self, a: usize, b: usize) -> &'a str {
        std::str::from_utf8(&self.bytes[a..b]).unwrap_or("")
    }

    fn find_from(&self, from: usize, byte: u8) -> Option<usize> {
        self.bytes[from..]
            .iter()
            .position(|&c| c == byte)
            .map(|x| from + x)
    }

    fn find_seq(&self, from: usize, seq: &[u8]) -> Option<usize> {
        if from >= self.bytes.len() {
            return None;
        }
        self.bytes[from..]
            .windows(seq.len())
            .position(|w| w == seq)
            .map(|x| from + x)
    }

    pub(crate) fn next(&mut self) -> Token {
        let len = self.bytes.len();
        loop {
            if self.pos >= len {
                return Token::Eof;
            }
            if self.bytes[self.pos] != b'<' {
                let start = self.pos;
                let lt = self.find_from(self.pos, b'<').unwrap_or(len);
                self.pos = lt;
                return Token::Text(decode_entities(self.slice(start, lt)));
            }
            if self.pos + 1 >= len {
                // A lone trailing '<' with nothing after it: consume it and
                // report EOF rather than looping on a token that can never
                // resolve to a real tag.
                self.pos = len;
                return Token::Eof;
            }
            let c = self.bytes[self.pos + 1];
            if c == b'!' {
                if self.bytes[self.pos..].starts_with(b"<!--") {
                    self.pos = match self.find_seq(self.pos + 4, b"-->") {
                        Some(e) => e + 3,
                        None => len,
                    };
                } else {
                    self.pos = match self.find_from(self.pos, b'>') {
                        Some(e) => e + 1,
                        None => len,
                    };
                }
                continue;
            }
            if c == b'?' {
                self.pos = match self.find_seq(self.pos, b"?>") {
                    Some(e) => e + 2,
                    None => match self.find_from(self.pos, b'>') {
                        Some(e) => e + 1,
                        None => len,
                    },
                };
                continue;
            }
            if c == b'/' {
                self.pos += 2; // consume "</"
                let start = self.pos;
                while self.pos < len && !is_ws(self.bytes[self.pos]) && self.bytes[self.pos] != b'>'
                {
                    self.pos += 1;
                }
                let name = self.slice(start, self.pos).to_ascii_lowercase();
                self.pos = match self.find_from(self.pos, b'>') {
                    Some(e) => e + 1,
                    None => len,
                };
                return Token::TagClose { name };
            }

            // Opening tag.
            self.pos += 1; // consume '<'
            let start = self.pos;
            while self.pos < len
                && !is_ws(self.bytes[self.pos])
                && self.bytes[self.pos] != b'>'
                && self.bytes[self.pos] != b'/'
            {
                self.pos += 1;
            }
            let name = self.slice(start, self.pos).to_ascii_lowercase();
            let mut attrs: Vec<(String, String)> = Vec::new();
            let self_close = loop {
                let iter_start = self.pos;
                while self.pos < len && is_ws(self.bytes[self.pos]) {
                    self.pos += 1;
                }
                if self.pos >= len {
                    break false;
                }
                let d = self.bytes[self.pos];
                if d == b'>' {
                    self.pos += 1;
                    break false;
                }
                if d == b'/' {
                    self.pos += 1;
                    if self.pos < len && self.bytes[self.pos] == b'>' {
                        self.pos += 1;
                    }
                    break true;
                }
                // Attribute name: stops at whitespace, '=', '/', or '>'.
                let ns = self.pos;
                while self.pos < len
                    && !is_ws(self.bytes[self.pos])
                    && self.bytes[self.pos] != b'='
                    && self.bytes[self.pos] != b'/'
                    && self.bytes[self.pos] != b'>'
                {
                    self.pos += 1;
                }
                let aname = self.slice(ns, self.pos).to_ascii_lowercase();
                if self.pos < len && self.bytes[self.pos] == b'=' {
                    self.pos += 1;
                    while self.pos < len && is_ws(self.bytes[self.pos]) {
                        self.pos += 1;
                    }
                    if self.pos < len
                        && (self.bytes[self.pos] == b'"' || self.bytes[self.pos] == b'\'')
                    {
                        let q = self.bytes[self.pos];
                        self.pos += 1;
                        let vs = self.pos;
                        let ve = self.find_from(self.pos, q).unwrap_or(len);
                        attrs.push((aname, decode_entities(self.slice(vs, ve))));
                        self.pos = if ve < len { ve + 1 } else { len };
                    } else {
                        // Unquoted value: stops at whitespace or '>' only —
                        // NOT '/', which is common inside a bare URL.
                        let vs = self.pos;
                        while self.pos < len
                            && !is_ws(self.bytes[self.pos])
                            && self.bytes[self.pos] != b'>'
                        {
                            self.pos += 1;
                        }
                        attrs.push((aname, decode_entities(self.slice(vs, self.pos))));
                    }
                } else if !aname.is_empty() {
                    attrs.push((aname, String::new()));
                }
                // Defensive: every branch above should already strictly
                // advance `pos`, but if some edge case doesn't, force it
                // rather than loop forever on malformed input.
                if self.pos == iter_start {
                    self.pos += 1;
                }
            };
            return Token::TagOpen {
                name,
                attrs,
                self_close,
            };
        }
    }
}

/// Decodes the handful of entities that actually show up in email HTML:
/// the five XML entities, numeric refs (decimal and `&#x..;` hex), and
/// `&nbsp;` (folded to a plain space — treating it as a hard space would
/// need width-aware wrapping this crate deliberately doesn't have). Any
/// other named entity (there are hundreds in the HTML5 table) is passed
/// through literally rather than guessed at.
fn decode_entities(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            let step = utf8_len(bytes[i]);
            let end = (i + step).min(bytes.len());
            out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or(""));
            i = end;
            continue;
        }
        let semi = raw[i + 1..].find(';').map(|x| i + 1 + x);
        match semi {
            Some(semi) if semi - i <= 16 => {
                let ent = &raw[i + 1..semi];
                match ent {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" => out.push('\''),
                    "nbsp" => out.push(' '),
                    _ if ent.starts_with('#') => {
                        match parse_numeric_entity(ent).and_then(char::from_u32) {
                            Some(ch) => out.push(ch),
                            None => out.push_str(&raw[i..semi + 1]),
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
    out
}

/// Parses the digits of a `#NN` / `#xNN` numeric character reference
/// (the `#` prefix already stripped from `ent` is expected — `ent` is the
/// full entity name including the leading `#`).
fn parse_numeric_entity(ent: &str) -> Option<u32> {
    let digits = &ent[1..];
    let (radix, digits) = match digits
        .strip_prefix('x')
        .or_else(|| digits.strip_prefix('X'))
    {
        Some(hex) => (16, hex),
        None => (10, digits),
    };
    if digits.is_empty() {
        return None;
    }
    u32::from_str_radix(digits, radix)
        .ok()
        .filter(|&cp| cp != 0)
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

    // --- Task 14 brief tests, verbatim ---

    #[test]
    fn bold_and_paragraphs() {
        let lines = render_html("<p>Hello <b>world</b></p><p>Next</p>", 80);
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.text.contains("world") && s.bold))
        );
        assert!(lines.len() >= 2);
    }
    #[test]
    fn links_become_footnotes() {
        let lines = render_html(r#"<a href="https://x">click</a>"#, 80);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains("click"));
        assert!(joined.contains("https://x"));
    }
    #[test]
    fn decodes_entities_and_wraps() {
        let lines = render_html("<p>a &amp; b</p>", 80);
        let joined: String = lines[0].spans.iter().map(|s| s.text.clone()).collect();
        assert!(joined.contains("a & b"));
    }
    #[test]
    fn blockquote_indents() {
        let lines = render_html("<blockquote>quoted</blockquote>", 80);
        assert!(lines.iter().any(|l| l.indent > 0));
    }

    // --- Additional coverage ---

    #[test]
    fn italic_and_underline_flags() {
        let lines = render_html("<i>slanted</i> <u>lined</u>", 80);
        let spans: Vec<&StyledSpan> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        assert!(spans.iter().any(|s| s.text.contains("slanted") && s.italic));
        assert!(
            spans
                .iter()
                .any(|s| s.text.contains("lined") && s.underline)
        );
    }

    #[test]
    fn br_forces_a_line_break_without_blank_separator() {
        let lines = render_html("one<br>two", 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].text, "one");
        assert_eq!(lines[1].spans[0].text, "two");
    }

    #[test]
    fn wraps_long_text_to_width() {
        let lines = render_html("<p>aaaa bbbb cccc dddd</p>", 9);
        assert!(lines.len() >= 2);
        for l in &lines {
            let w: usize = l.spans.iter().map(|s| s.text.chars().count()).sum();
            assert!(w <= 9, "line {:?} exceeds width", l);
        }
    }

    #[test]
    fn nested_blockquote_indents_more() {
        let lines = render_html("<blockquote>a<blockquote>b</blockquote></blockquote>", 80);
        let max_indent = lines.iter().map(|l| l.indent).max().unwrap_or(0);
        assert!(
            max_indent >= 2,
            "expected nested indent, got lines {:?}",
            lines
        );
    }

    #[test]
    fn list_items_get_bullets_and_indent() {
        let lines = render_html("<ul><li>first</li><li>second</li></ul>", 80);
        assert!(
            lines
                .iter()
                .any(|l| l.indent > 0 && l.spans.iter().any(|s| s.text.contains("first")))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.text.contains("second")))
        );
    }

    #[test]
    fn table_row_cells_are_joined() {
        let lines = render_html("<table><tr><td>a</td><td>b</td></tr></table>", 80);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains('|'));
        assert!(joined.contains('a') && joined.contains('b'));
    }

    #[test]
    fn multiple_links_get_distinct_footnote_numbers() {
        let lines = render_html(r#"<a href="https://a">A</a> <a href="https://b">B</a>"#, 80);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains("A[1]"));
        assert!(joined.contains("B[2]"));
        assert!(joined.contains("[1] https://a"));
        assert!(joined.contains("[2] https://b"));
    }

    #[test]
    fn unknown_tags_are_transparent() {
        let lines = render_html("<div><span>hi <b>there</b></span></div>", 80);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains("hi there"));
    }

    #[test]
    fn style_and_script_content_is_dropped() {
        let lines = render_html(
            "<style>body { color: red; }</style><script>var x = 1;</script><p>real</p>",
            80,
        );
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert_eq!(joined, "real");
    }

    #[test]
    fn nbsp_and_numeric_entities_decode() {
        let lines = render_html("<p>a&nbsp;b &#65; &#x42;</p>", 80);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains("a b A B"));
    }

    #[test]
    fn unquoted_href_with_slashes_does_not_hang() {
        let lines = render_html(r#"<a href=https://example.com/path?a=1>link</a>"#, 80);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains("link"));
        assert!(joined.contains("https://example.com/path?a=1"));
    }

    /// Never panic (or hang) on gnarly/malformed markup: unclosed tags,
    /// stray '<'/'>' runs, unquoted attributes, unterminated comments and
    /// tags. This doesn't assert anything about the *output* — only that
    /// none of these inputs crash or fail to terminate.
    #[test]
    fn malformed_html_does_not_panic() {
        let inputs = [
            "<div><p>unclosed <b>bold <i>italic</p>",
            "<a href=unquoted&amp;value>text",
            "<<<>>>&&&;;;",
            "<script>var x = '<b>fake</b>';</script><p>real</p>",
            "<p>row & col <td>cell</td",
            "<blockquote><blockquote>deep",
            "<!-- unterminated comment <p>hidden</p>",
            "<p ==>weird attrs<//p>",
            "plain text with a stray < and > chars",
            "",
        ];
        for html in inputs {
            let _ = render_html(html, 40);
        }
    }

    #[test]
    fn render_text_wraps_plain_body() {
        let lines = render_text("one two three four", 10);
        assert!(lines.len() >= 2);
        for l in &lines {
            let w: usize = l.spans.iter().map(|s| s.text.chars().count()).sum();
            assert!(w <= 10);
        }
    }

    #[test]
    fn render_text_indents_quoted_reply_lines() {
        let lines = render_text("reply\n> quoted line\n>> deeper quote", 80);
        assert_eq!(lines[0].indent, 0);
        assert!(lines.iter().any(|l| l.indent == 1));
        assert!(lines.iter().any(|l| l.indent == 2));
    }

    #[test]
    fn render_text_empty_line_is_preserved_as_blank() {
        let lines = render_text("first\n\nsecond", 80);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].spans.is_empty());
    }

    #[test]
    fn img_cid_becomes_an_image_marker_line() {
        let lines = render_html(
            r#"<p>before</p><img src="cid:logo123" alt="Logo"><p>after</p>"#,
            80,
        );
        let marker = lines
            .iter()
            .find(|l| l.image.is_some())
            .expect("an image marker line");
        match &marker.image.as_ref().unwrap().src {
            ImageSource::Cid(c) => assert_eq!(c, "logo123"),
            other => panic!("expected Cid, got {other:?}"),
        }
        assert_eq!(marker.image.as_ref().unwrap().alt, "Logo");
        // surrounding text still renders
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.text.clone())
            .collect();
        assert!(joined.contains("before") && joined.contains("after"));
    }
    #[test]
    fn img_data_uri_decodes_bytes() {
        // "R0lGOD" ... use a tiny valid base64: "aGk=" decodes to "hi"
        let lines = render_html(r#"<img src="data:image/png;base64,aGk=">"#, 80);
        let m = lines.iter().find_map(|l| l.image.as_ref()).unwrap();
        match &m.src {
            ImageSource::Data { mime, bytes } => {
                assert_eq!(mime, "image/png");
                assert_eq!(bytes, b"hi");
            }
            other => panic!("expected Data, got {other:?}"),
        }
    }
    #[test]
    fn img_remote_is_marked_remote_and_not_fetched() {
        let lines = render_html(r#"<img src="https://tracker.example/x.png">"#, 80);
        let m = lines.iter().find_map(|l| l.image.as_ref()).unwrap();
        assert!(matches!(m.src, ImageSource::Remote(_)));
    }
    #[test]
    fn img_malformed_data_is_unsupported() {
        let lines = render_html(r#"<img src="data:whoops">"#, 80);
        let m = lines.iter().find_map(|l| l.image.as_ref()).unwrap();
        assert!(matches!(m.src, ImageSource::Unsupported));
    }
    #[test]
    fn image_refs_extracts_all_in_order() {
        let refs = image_refs(r#"<img src="cid:a"><p>x</p><img src="https://y">"#);
        assert_eq!(refs.len(), 2);
        assert!(matches!(refs[0].src, ImageSource::Cid(ref c) if c == "a"));
        assert!(matches!(refs[1].src, ImageSource::Remote(_)));
    }
}
