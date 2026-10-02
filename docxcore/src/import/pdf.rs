//! PDF → [`Document`] (#633): the text of a PDF, in reading order, as
//! editable paragraphs: what Word does when it opens a PDF, without its
//! layout fidelity.
//!
//! Objects are found by scanning for `n g obj … endobj` rather than
//! trusting the cross-reference table, so a file with a broken or missing
//! xref still reads, and a later definition (an incremental update) wins.
//! Stream bodies are skipped by their `/Length` (or up to `endstream` when
//! the length is indirect or wrong), so nothing inside compressed data is
//! mistaken for an object. Object streams (`/Type /ObjStm`) are unpacked.
//! An encrypted file is refused.
//!
//! Pages come from `/Root → /Pages → /Kids` in order (inherited
//! `/Resources` honoured). Each content stream is interpreted for its text
//! operators (`BT ET Tf Tm Td TD T* TL Tc Tw Tz Tj TJ ' "`, with `cm`, `q`,
//! `Q` and Form XObjects through `Do`); a glyph's text comes from the font's
//! `/ToUnicode` CMap, else its `/Encoding` (`/Differences` glyph names over
//! WinAnsi). Glyphs on one baseline make a line, with a space where the gap
//! is wider than a fraction of the font size and a tab where it is much
//! wider (table columns, tab stops); lines make paragraphs at a vertical
//! gap, a line that ends short of the margin, a change of size, or a list
//! marker. Bold and italic come from the font's name, headings from sizes
//! larger than the body's. Images, vector graphics and annotations are not
//! converted.

use super::{Budget, Builder, TOO_BIG, heading_props, marker_is_numbered};
use crate::model::{Document, ParProps, RunProps};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// What one font's ToUnicode CMap, or its widths, may map.
const MAX_CMAP_ENTRIES: usize = 1 << 20;

/// Read a PDF. `Err` when it is encrypted, damaged past reading, has no
/// text (a scan, or images only), or would cost more than an import may.
pub fn import_pdf(bytes: &[u8]) -> Result<Document, String> {
    import_pdf_within(bytes, Budget::standard())
}

fn import_pdf_within(bytes: &[u8], budget: Budget) -> Result<Document, String> {
    let file = File::scan(bytes, budget);
    if file.budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    if file.encrypted {
        return Err("the PDF is encrypted (password-protected), so its text cannot be read".into());
    }
    let pages = file.pages();
    if pages.is_empty() {
        return Err("no pages found in the PDF (the file may be damaged)".into());
    }
    let mut lines_by_page = Vec::new();
    for page in &pages {
        let glyphs = file.page_glyphs(page);
        if file.budget.exhausted() {
            return Err(TOO_BIG.into());
        }
        lines_by_page.push(make_lines(glyphs));
    }
    let doc = layout(&lines_by_page, &file.budget);
    if file.budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    if doc.body.is_empty() {
        return Err("the PDF has no text to convert (it may be a scan or images only)".into());
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Objects

#[derive(Clone, Debug, PartialEq)]
enum Obj {
    Null,
    Bool(bool),
    Num(f64),
    Name(String),
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    Ref(u32),
    Stream(Dict, Vec<u8>),
}

type Dict = Vec<(String, Obj)>;

fn dict_get<'a>(d: &'a Dict, key: &str) -> Option<&'a Obj> {
    d.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

impl Obj {
    fn as_dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) | Obj::Stream(d, _) => Some(d),
            _ => None,
        }
    }
    fn as_num(&self) -> Option<f64> {
        match self {
            Obj::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn as_name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Lexer (file syntax and content streams share it)

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Name(String),
    Str(Vec<u8>),
    ArrOpen,
    ArrClose,
    DictOpen,
    DictClose,
    Kw(Vec<u8>),
}

struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
    /// Charged for every byte the lexer advances over (white space,
    /// comments, strings and nested operands included); `None` for the
    /// few lexers that read a bounded slice. Once it is out the lexer ends.
    budget: Option<&'a Budget>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\0')
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

impl<'a> Lexer<'a> {
    fn new(s: &'a [u8]) -> Self {
        Lexer {
            s,
            pos: 0,
            budget: None,
        }
    }

    fn charged(s: &'a [u8], budget: &'a Budget) -> Self {
        Lexer {
            s,
            pos: 0,
            budget: Some(budget),
        }
    }

    /// The next token, its bytes charged to the budget.
    fn next(&mut self) -> Option<Tok> {
        let start = self.pos;
        let tok = self.next_token();
        if let Some(b) = self.budget {
            if !b.scanned(self.pos - start) {
                self.pos = self.s.len();
                return None;
            }
        }
        tok
    }

    fn skip_ws(&mut self) {
        while self.pos < self.s.len() {
            let b = self.s[self.pos];
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.pos < self.s.len() && !matches!(self.s[self.pos], b'\n' | b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> Option<Tok> {
        self.skip_ws();
        let b = *self.s.get(self.pos)?;
        self.pos += 1;
        Some(match b {
            b'[' => Tok::ArrOpen,
            b']' => Tok::ArrClose,
            b'<' if self.s.get(self.pos) == Some(&b'<') => {
                self.pos += 1;
                Tok::DictOpen
            }
            b'>' if self.s.get(self.pos) == Some(&b'>') => {
                self.pos += 1;
                Tok::DictClose
            }
            b'<' => Tok::Str(self.hex_string()),
            b'(' => Tok::Str(self.literal_string()),
            b'/' => Tok::Name(self.name()),
            b'{' | b'}' | b')' | b'>' => Tok::Kw(vec![b]),
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let start = self.pos - 1;
                while self.pos < self.s.len()
                    && matches!(self.s[self.pos], b'0'..=b'9' | b'.' | b'-' | b'+')
                {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap_or("0");
                // `--5` and such: read what parses, else 0.
                Tok::Num(text.parse().unwrap_or_else(|_| {
                    text.trim_start_matches(['+', '-'])
                        .parse::<f64>()
                        .map(|n| if text.starts_with('-') { -n } else { n })
                        .unwrap_or(0.0)
                }))
            }
            _ => {
                let start = self.pos - 1;
                while self.pos < self.s.len()
                    && !is_ws(self.s[self.pos])
                    && !is_delim(self.s[self.pos])
                {
                    self.pos += 1;
                }
                Tok::Kw(self.s[start..self.pos].to_vec())
            }
        })
    }

    fn hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(&b) = self.s.get(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let v = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => continue,
            };
            match hi.take() {
                Some(h) => out.push(h << 4 | v),
                None => hi = Some(v),
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(&b) = self.s.get(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(&e) = self.s.get(self.pos) else {
                        break;
                    };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.s.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        b'\r' => {
                            if self.s.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        _ => out.push(e),
                    }
                }
                _ => out.push(b),
            }
        }
        out
    }

    fn name(&mut self) -> String {
        let mut out = Vec::new();
        while let Some(&b) = self.s.get(self.pos) {
            if is_ws(b) || is_delim(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hex = self.s.get(self.pos..self.pos + 2);
                if let Some(v) = hex
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(v);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Parse one object (no stream body); `None` at the end or at a token
    /// that starts no object.
    fn object(&mut self, depth: usize) -> Option<Obj> {
        let tok = self.next()?;
        self.object_from(tok, depth)
    }

    fn object_from(&mut self, tok: Tok, depth: usize) -> Option<Obj> {
        if depth > 64 {
            return Some(Obj::Null);
        }
        Some(match tok {
            Tok::Num(n) => {
                // `n g R`?
                let save = self.pos;
                if n >= 0.0 && n.fract() == 0.0 {
                    if let Some(Tok::Num(g)) = self.next() {
                        if g >= 0.0 && g.fract() == 0.0 {
                            if let Some(Tok::Kw(k)) = self.next() {
                                if k == b"R" {
                                    return Some(Obj::Ref(n as u32));
                                }
                            }
                        }
                    }
                }
                self.pos = save;
                Obj::Num(n)
            }
            Tok::Name(n) => Obj::Name(n),
            Tok::Str(s) => Obj::Str(s),
            Tok::ArrOpen => {
                let mut v = Vec::new();
                loop {
                    match self.next() {
                        None | Some(Tok::ArrClose) => break,
                        Some(Tok::DictClose) => break,
                        Some(t) => {
                            if let Some(o) = self.object_from(t, depth + 1) {
                                v.push(o);
                            }
                        }
                    }
                }
                Obj::Array(v)
            }
            Tok::DictOpen => {
                let mut d = Dict::new();
                loop {
                    match self.next() {
                        None | Some(Tok::DictClose) => break,
                        Some(Tok::Name(k)) => {
                            let Some(t) = self.next() else { break };
                            if t == Tok::DictClose {
                                break;
                            }
                            let v = self.object_from(t, depth + 1).unwrap_or(Obj::Null);
                            d.push((k, v));
                        }
                        Some(_) => {}
                    }
                }
                Obj::Dict(d)
            }
            Tok::Kw(k) => match k.as_slice() {
                b"true" => Obj::Bool(true),
                b"false" => Obj::Bool(false),
                b"null" => Obj::Null,
                _ => return None,
            },
            Tok::ArrClose | Tok::DictClose => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// The file

struct File {
    objects: HashMap<u32, Obj>,
    root: Option<u32>,
    encrypted: bool,
    budget: Budget,
    /// Streams decoded so far, by object: a form drawn a thousand times,
    /// or a content stream listed a thousand times, is decoded once.
    streams: RefCell<HashMap<u32, Rc<Vec<u8>>>>,
    /// Fonts loaded so far, by object, for every page; shared, never
    /// copied on a lookup.
    fonts: RefCell<HashMap<u32, Rc<Font>>>,
}

impl File {
    fn scan(bytes: &[u8], budget: Budget) -> File {
        let mut objects: HashMap<u32, Obj> = HashMap::new();
        let mut root = None;
        let mut encrypted = false;
        let mut lx = Lexer::charged(bytes, &budget);
        // The last two tokens, to recognise `n g obj`.
        let mut prev: [Option<Tok>; 2] = [None, None];
        while let Some(tok) = lx.next() {
            match &tok {
                Tok::Kw(k) if k == b"obj" => {
                    let id = match (&prev[0], &prev[1]) {
                        (Some(Tok::Num(n)), Some(Tok::Num(g)))
                            if *n >= 0.0 && n.fract() == 0.0 && g.fract() == 0.0 =>
                        {
                            Some(*n as u32)
                        }
                        _ => None,
                    };
                    prev = [None, None];
                    let Some(id) = id else { continue };
                    let Some(obj) = lx.object(0) else { continue };
                    let obj = File::stream_body(&mut lx, obj);
                    if let Some(d) = obj.as_dict() {
                        if dict_get(d, "Type").and_then(Obj::as_name) == Some("XRef") {
                            File::trailer_keys(d, &mut root, &mut encrypted);
                        }
                    }
                    objects.insert(id, obj);
                    continue;
                }
                Tok::Kw(k) if k == b"trailer" => {
                    if let Some(Obj::Dict(d)) = lx.object(0) {
                        File::trailer_keys(&d, &mut root, &mut encrypted);
                    }
                    prev = [None, None];
                    continue;
                }
                _ => {}
            }
            prev = [prev[1].take(), Some(tok)];
        }
        // Object streams: their objects, where no direct definition exists.
        let streams: Vec<(Dict, Vec<u8>)> = objects
            .values()
            .filter_map(|o| match o {
                Obj::Stream(d, data)
                    if dict_get(d, "Type").and_then(Obj::as_name) == Some("ObjStm") =>
                {
                    Some((d.clone(), data.clone()))
                }
                _ => None,
            })
            .collect();
        for (d, raw) in streams {
            let data = decode_stream(&d, &raw, &objects, &budget);
            let n = dict_get(&d, "N").and_then(Obj::as_num).unwrap_or(0.0) as usize;
            let first = dict_get(&d, "First").and_then(Obj::as_num).unwrap_or(0.0) as usize;
            let first = first.min(data.len());
            let mut header = Lexer::charged(&data[..first], &budget);
            let mut entries = Vec::new();
            for _ in 0..n.min(100_000) {
                match (header.next(), header.next()) {
                    (Some(Tok::Num(id)), Some(Tok::Num(off))) if id >= 0.0 && off >= 0.0 => {
                        entries.push((first.saturating_add(off as usize), id as u32))
                    }
                    _ => break,
                }
            }
            // Each object is parsed only from its own span, up to the next
            // entry's offset, and an offset listed twice only once: many
            // entries pointing at one large object must not multiply it.
            entries.sort_unstable();
            entries.dedup_by_key(|(at, _)| *at);
            for (k, &(at, id)) in entries.iter().enumerate() {
                let end = entries.get(k + 1).map_or(data.len(), |&(next, _)| next);
                if objects.contains_key(&id) || at >= end || at >= data.len() {
                    continue;
                }
                let mut lx = Lexer::charged(&data[at..end.min(data.len())], &budget);
                if let Some(o) = lx.object(0) {
                    objects.insert(id, o);
                }
            }
        }
        if root.is_none() {
            root = objects
                .iter()
                .filter(|(_, o)| {
                    o.as_dict()
                        .and_then(|d| dict_get(d, "Type"))
                        .and_then(Obj::as_name)
                        == Some("Catalog")
                })
                .map(|(id, _)| *id)
                .max();
        }
        File {
            objects,
            root,
            encrypted,
            budget,
            streams: RefCell::new(HashMap::new()),
            fonts: RefCell::new(HashMap::new()),
        }
    }

    fn trailer_keys(d: &Dict, root: &mut Option<u32>, encrypted: &mut bool) {
        if let Some(Obj::Ref(r)) = dict_get(d, "Root") {
            *root = Some(*r);
        }
        if dict_get(d, "Encrypt").is_some_and(|e| *e != Obj::Null) {
            *encrypted = true;
        }
    }

    /// After a dictionary: its stream body, when `stream` follows.
    fn stream_body(lx: &mut Lexer, obj: Obj) -> Obj {
        let Obj::Dict(d) = obj else { return obj };
        let save = lx.pos;
        match lx.next() {
            Some(Tok::Kw(k)) if k == b"stream" => {}
            _ => {
                lx.pos = save;
                return Obj::Dict(d);
            }
        }
        let s = lx.s;
        let mut start = lx.pos;
        if s.get(start) == Some(&b'\r') {
            start += 1;
        }
        if s.get(start) == Some(&b'\n') {
            start += 1;
        }
        let declared = dict_get(&d, "Length")
            .and_then(Obj::as_num)
            .map(|n| n.max(0.0) as usize);
        // The declared length is right when `endstream` follows it, after
        // at most a few bytes of white space (looked at, not lexed: a token
        // there could run to the end of the file).
        let fits = |len: usize| {
            let end = start.checked_add(len)?;
            let after = s.get(end..)?;
            let ws = after.iter().take(16).take_while(|&&b| is_ws(b)).count();
            after[ws..].starts_with(b"endstream").then_some(end)
        };
        let end = match declared.and_then(fits) {
            Some(end) => end,
            None => {
                // Indirect or wrong length: up to `endstream`, less its EOL.
                let found = super::find(&s[start..], b"endstream")
                    .map(|i| start + i)
                    .unwrap_or(s.len());
                if let Some(b) = lx.budget {
                    b.scanned(found - start);
                }
                let mut e = found;
                if e > start && s[e - 1] == b'\n' {
                    e -= 1;
                }
                if e > start && s[e - 1] == b'\r' {
                    e -= 1;
                }
                e
            }
        };
        let data = s[start..end].to_vec();
        lx.pos = end;
        if let Some(i) = super::find(&s[end..s.len().min(end + 64)], b"endstream") {
            lx.pos = end + i + "endstream".len();
        }
        Obj::Stream(d, data)
    }

    fn get(&self, id: u32) -> Option<&Obj> {
        self.objects.get(&id)
    }

    /// `o` with references followed (a few levels).
    fn resolve<'b>(&'b self, mut o: &'b Obj) -> &'b Obj {
        for _ in 0..16 {
            match o {
                Obj::Ref(r) => match self.get(*r) {
                    Some(next) => o = next,
                    None => return &Obj::Null,
                },
                _ => return o,
            }
        }
        &Obj::Null
    }

    fn dict_of<'b>(&'b self, o: &'b Obj) -> Option<&'b Dict> {
        self.resolve(o).as_dict()
    }

    fn lookup<'b>(&'b self, d: &'b Dict, key: &str) -> Option<&'b Obj> {
        dict_get(d, key).map(|o| self.resolve(o))
    }

    /// The pages in order, each with its (possibly inherited) resources.
    fn pages(&self) -> Vec<PageRef> {
        let mut out = Vec::new();
        let root = self
            .root
            .and_then(|r| self.get(r))
            .and_then(|o| self.dict_of(o));
        if let Some(pages) = root.and_then(|c| dict_get(c, "Pages")) {
            let mut seen = HashSet::new();
            self.walk_pages(pages, None, &mut out, &mut seen, 0);
        }
        if out.is_empty() {
            // No usable page tree: every page object, in object order.
            let mut ids: Vec<u32> = self
                .objects
                .iter()
                .filter(|(_, o)| {
                    o.as_dict()
                        .and_then(|d| dict_get(d, "Type"))
                        .and_then(Obj::as_name)
                        == Some("Page")
                })
                .map(|(id, _)| *id)
                .collect();
            ids.sort_unstable();
            for id in ids {
                let d = self
                    .get(id)
                    .and_then(Obj::as_dict)
                    .cloned()
                    .unwrap_or_default();
                let resources = dict_get(&d, "Resources").cloned().map(Rc::new);
                out.push(PageRef { dict: d, resources });
            }
        }
        out
    }

    fn walk_pages(
        &self,
        node: &Obj,
        inherited: Option<&Rc<Obj>>,
        out: &mut Vec<PageRef>,
        seen: &mut HashSet<u32>,
        depth: usize,
    ) {
        if depth > 64 || out.len() > 100_000 {
            return;
        }
        if let Obj::Ref(r) = node {
            if !seen.insert(*r) {
                return;
            }
        }
        let Some(d) = self.dict_of(node) else { return };
        // A node's own resources are copied once and shared by every page
        // under it, never copied per page.
        let own = dict_get(d, "Resources").map(|r| Rc::new(r.clone()));
        let resources = own.as_ref().or(inherited);
        match dict_get(d, "Kids").map(|k| self.resolve(k)) {
            Some(Obj::Array(kids)) => {
                for kid in kids {
                    self.walk_pages(kid, resources, out, seen, depth + 1);
                }
            }
            _ => out.push(PageRef {
                dict: d.clone(),
                resources: resources.cloned(),
            }),
        }
    }

    fn stream_data(&self, o: &Obj) -> Option<Rc<Vec<u8>>> {
        if let Obj::Ref(id) = o {
            if let Some(data) = self.streams.borrow().get(id) {
                return Some(data.clone());
            }
        }
        let data = match self.resolve(o) {
            Obj::Stream(d, raw) => Rc::new(decode_stream(d, raw, &self.objects, &self.budget)),
            _ => return None,
        };
        if let Obj::Ref(id) = o {
            self.streams.borrow_mut().insert(*id, data.clone());
        }
        Some(data)
    }

    fn page_glyphs(&self, page: &PageRef) -> Vec<Glyph> {
        let mut content: Rc<Vec<u8>> = Rc::default();
        match dict_get(&page.dict, "Contents").map(|c| self.resolve(c)) {
            Some(Obj::Array(parts)) => {
                for p in parts {
                    if let Some(data) = self.stream_data(p) {
                        // Joined, the parts count again: one stream listed
                        // many times must not multiply into gigabytes.
                        if !self.budget.take_bytes(data.len() + 1) {
                            break;
                        }
                        let joined = Rc::make_mut(&mut content);
                        joined.extend_from_slice(&data);
                        joined.push(b'\n');
                    }
                }
            }
            // Shared, not copied: the lexer charges every pass over it.
            Some(o @ Obj::Stream(..)) => {
                if let Some(data) = self.stream_data(o) {
                    content = data;
                }
            }
            _ => {}
        }
        let empty = Dict::new();
        let resources = page
            .resources
            .as_deref()
            .and_then(|r| self.dict_of(r))
            .unwrap_or(&empty);
        let mut run = Interp {
            file: self,
            glyphs: Vec::new(),
            fonts: HashMap::new(),
            form_stack: Vec::new(),
        };
        run.content(&content, resources, IDENTITY, 0);
        run.glyphs
    }
}

struct PageRef {
    dict: Dict,
    resources: Option<Rc<Obj>>,
}

/// A stream's data with its filters undone (Flate, ASCIIHex, ASCII85;
/// anything else, an image codec among them, gives nothing).
///
/// Decoded bytes are spent from `budget`; a stream that would pass it
/// stops there, and the import fails.
fn decode_stream(d: &Dict, raw: &[u8], objects: &HashMap<u32, Obj>, budget: &Budget) -> Vec<u8> {
    let resolve = |o: &Obj| -> Obj {
        match o {
            Obj::Ref(r) => objects.get(r).cloned().unwrap_or(Obj::Null),
            _ => o.clone(),
        }
    };
    let filters: Vec<String> = match dict_get(d, "Filter").map(resolve) {
        Some(Obj::Name(n)) => vec![n],
        Some(Obj::Array(a)) => a
            .iter()
            .filter_map(|f| resolve(f).as_name().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let parms: Vec<Option<Dict>> = match dict_get(d, "DecodeParms").map(resolve) {
        Some(Obj::Dict(p)) => vec![Some(p)],
        Some(Obj::Array(a)) => a.iter().map(|p| resolve(p).as_dict().cloned()).collect(),
        _ => Vec::new(),
    };
    let mut data = raw.to_vec();
    for (i, f) in filters.iter().enumerate() {
        data = match f.as_str() {
            "FlateDecode" | "Fl" => {
                let Ok(out) = inflate_within(zlib_body(&data), budget) else {
                    return Vec::new();
                };
                match parms.get(i).cloned().flatten() {
                    Some(p) => png_predictor(&out, &p),
                    None => out,
                }
            }
            "ASCIIHexDecode" | "AHx" => {
                let mut lx = Lexer::new(&data);
                lx.hex_string()
            }
            "ASCII85Decode" | "A85" => ascii85(&data),
            _ => return Vec::new(),
        };
    }
    data
}

/// Inflate `body` spending decoded bytes from `budget`, capped inside each
/// block at what is left. Past it: `Err` with what was decoded up to the cap
/// (no more), and the budget out.
fn inflate_within(body: &[u8], budget: &Budget) -> Result<Vec<u8>, Vec<u8>> {
    let room = budget.room();
    if room == 0 {
        budget.fail();
        return Err(Vec::new());
    }
    let out = opccore::inflate::inflate_partial(body, room);
    if out.len() >= room || !budget.take_bytes(out.len()) {
        budget.fail();
        return Err(out);
    }
    Ok(out)
}

/// A zlib stream's DEFLATE body (its two-byte header skipped when present).
fn zlib_body(data: &[u8]) -> &[u8] {
    match data {
        [cmf, flg, rest @ ..]
            if cmf & 0x0F == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0 =>
        {
            rest
        }
        _ => data,
    }
}

/// Undo PNG row predictors (`/Predictor` 10-15).
fn png_predictor(data: &[u8], parms: &Dict) -> Vec<u8> {
    let predictor = dict_get(parms, "Predictor")
        .and_then(Obj::as_num)
        .unwrap_or(1.0) as u32;
    if predictor < 10 {
        return data.to_vec();
    }
    let colors = dict_get(parms, "Colors")
        .and_then(Obj::as_num)
        .unwrap_or(1.0)
        .max(1.0) as usize;
    let bpc = dict_get(parms, "BitsPerComponent")
        .and_then(Obj::as_num)
        .unwrap_or(8.0)
        .max(1.0) as usize;
    let columns = dict_get(parms, "Columns")
        .and_then(Obj::as_num)
        .unwrap_or(1.0)
        .max(1.0) as usize;
    // The sizes come from the file: checked, and a row longer than the
    // data is nonsense (it would only allocate), so the data is kept as is.
    let Some(bits) = colors.checked_mul(bpc).and_then(|b| b.checked_mul(columns)) else {
        return data.to_vec();
    };
    let row = bits.div_ceil(8);
    if row == 0 || row > data.len() {
        return data.to_vec();
    }
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.len() < 2 {
            break;
        }
        let kind = chunk[0];
        let mut cur: Vec<u8> = chunk[1..].to_vec();
        cur.resize(row, 0);
        for i in 0..row {
            let left = if i >= bpp { cur[i - bpp] } else { 0 };
            let up = prev[i];
            let ul = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = cur[i].wrapping_add(match kind {
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => {
                    let p = i16::from(left) + i16::from(up) - i16::from(ul);
                    let (pa, pb, pc) = (
                        (p - i16::from(left)).abs(),
                        (p - i16::from(up)).abs(),
                        (p - i16::from(ul)).abs(),
                    );
                    if pa <= pb && pa <= pc {
                        left
                    } else if pb <= pc {
                        up
                    } else {
                        ul
                    }
                }
                _ => 0,
            });
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    out
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0;
    for &b in data {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |a, &d| a * 85 + u64::from(d));
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |a, &d| a * 85 + u64::from(d));
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    out
}

// ---------------------------------------------------------------------------
// Fonts

#[derive(Clone, Default)]
struct Font {
    two_byte: bool,
    to_unicode: HashMap<u32, String>,
    /// Single-byte code → character, from `/Encoding`.
    encoding: Option<Box<[Option<char>; 256]>>,
    widths: HashMap<u32, f64>,
    default_width: f64,
    bold: bool,
    italic: bool,
    /// Code → Unicode when the CMap is a Unicode one (`Uni…-UCS2-H`).
    ucs2: bool,
}

impl Font {
    fn load(file: &File, d: &Dict) -> Font {
        let mut f = Font {
            default_width: 500.0,
            ..Font::default()
        };
        let subtype = file
            .lookup(d, "Subtype")
            .and_then(Obj::as_name)
            .unwrap_or("");
        let base = file
            .lookup(d, "BaseFont")
            .and_then(Obj::as_name)
            .unwrap_or("")
            .to_string();
        // A subset's `ABCDEF+` prefix says nothing about the face.
        let face = base
            .split_once('+')
            .map(|(_, r)| r)
            .unwrap_or(&base)
            .to_ascii_lowercase();
        f.bold = ["bold", "black", "heavy", "semibold", "demi"]
            .iter()
            .any(|w| face.contains(w));
        f.italic = face.contains("italic") || face.contains("oblique");
        if face.starts_with("courier") {
            f.default_width = 600.0;
        }
        if let Some(cmap) = file.lookup(d, "ToUnicode") {
            if let Some(data) = file.stream_data(cmap) {
                f.to_unicode = parse_cmap(&data, &file.budget);
            }
        }
        if subtype == "Type0" {
            f.two_byte = true;
            if let Some(Obj::Name(enc)) = file.lookup(d, "Encoding") {
                f.ucs2 = enc.contains("UCS2") || enc.contains("UTF16");
            }
            let desc = match file.lookup(d, "DescendantFonts") {
                Some(Obj::Array(a)) => a.first().and_then(|x| file.dict_of(x)),
                _ => None,
            };
            if let Some(desc) = desc {
                f.default_width = file
                    .lookup(desc, "DW")
                    .and_then(Obj::as_num)
                    .unwrap_or(1000.0);
                if let Some(Obj::Array(w)) = file.lookup(desc, "W") {
                    // At most MAX_CMAP_ENTRIES widths a font, each a unit
                    // of work; codes saturate rather than wrap.
                    let mut room = MAX_CMAP_ENTRIES;
                    let mut put = |widths: &mut HashMap<u32, f64>, c: u32, wd: f64| {
                        if room == 0 || !file.budget.op() {
                            return false;
                        }
                        room -= 1;
                        widths.insert(c, wd);
                        true
                    };
                    let mut i = 0;
                    'w: while i < w.len() {
                        let Some(c0) = file.resolve(&w[i]).as_num() else {
                            break;
                        };
                        let c0 = c0.clamp(0.0, f64::from(u32::MAX)) as u32;
                        match w.get(i + 1).map(|o| file.resolve(o)) {
                            Some(Obj::Array(list)) => {
                                for (k, wd) in list.iter().enumerate() {
                                    if let Some(wd) = file.resolve(wd).as_num() {
                                        let c = c0.saturating_add(k as u32);
                                        if !put(&mut f.widths, c, wd) {
                                            break 'w;
                                        }
                                    }
                                }
                                i += 2;
                            }
                            Some(Obj::Num(c1)) => {
                                let wd = w
                                    .get(i + 2)
                                    .and_then(|o| file.resolve(o).as_num())
                                    .unwrap_or(f.default_width);
                                let c1 = c1.clamp(0.0, f64::from(u32::MAX)) as u32;
                                let hi = c1.min(c0.saturating_add(65535));
                                for c in c0..=hi {
                                    if !put(&mut f.widths, c, wd) {
                                        break 'w;
                                    }
                                }
                                i += 3;
                            }
                            _ => break,
                        }
                    }
                }
            }
        } else {
            let first = file
                .lookup(d, "FirstChar")
                .and_then(Obj::as_num)
                .unwrap_or(0.0) as u32;
            if let Some(Obj::Array(w)) = file.lookup(d, "Widths") {
                for (k, wd) in w.iter().take(MAX_CMAP_ENTRIES).enumerate() {
                    if !file.budget.op() {
                        break;
                    }
                    if let Some(wd) = file.resolve(wd).as_num() {
                        f.widths.insert(first.saturating_add(k as u32), wd);
                    }
                }
            }
            let mut table: [Option<char>; 256] = std::array::from_fn(|b| win_ansi(b as u8));
            match file.lookup(d, "Encoding") {
                Some(Obj::Name(n)) if n == "MacRomanEncoding" => {
                    table = std::array::from_fn(|b| mac_roman(b as u8));
                }
                Some(o @ Obj::Dict(_)) => {
                    let ed = o.as_dict().cloned().unwrap_or_default();
                    if file.lookup(&ed, "BaseEncoding").and_then(Obj::as_name)
                        == Some("MacRomanEncoding")
                    {
                        table = std::array::from_fn(|b| mac_roman(b as u8));
                    }
                    if let Some(Obj::Array(diffs)) = file.lookup(&ed, "Differences") {
                        let mut code = 0usize;
                        for o in diffs {
                            match file.resolve(o) {
                                Obj::Num(n) => code = *n as usize,
                                Obj::Name(g) => {
                                    if code < 256 {
                                        table[code] = glyph_char(g);
                                    }
                                    code += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
            if face.starts_with("symbol") || face.contains("wingdings") {
                // A symbol font's bytes are its own glyphs, as Word maps them.
                table = std::array::from_fn(|b| char::from_u32(0xF000 + b as u32));
            }
            f.encoding = Some(Box::new(table));
        }
        f
    }

    /// The codes in a shown string, one at a time.
    fn codes<'s>(&self, s: &'s [u8]) -> impl Iterator<Item = u32> + 's {
        let step = if self.two_byte { 2 } else { 1 };
        s.chunks(step).map(move |p| {
            if step == 2 {
                u32::from(p[0]) << 8 | u32::from(*p.get(1).unwrap_or(&0))
            } else {
                u32::from(p[0])
            }
        })
    }

    fn text(&self, code: u32) -> String {
        if let Some(t) = self.to_unicode.get(&code) {
            return t.clone();
        }
        if let Some(table) = &self.encoding {
            return table
                .get(code as usize)
                .copied()
                .flatten()
                .map(String::from)
                .unwrap_or_default();
        }
        if self.ucs2 {
            return char::from_u32(code).map(String::from).unwrap_or_default();
        }
        "\u{fffd}".into()
    }

    fn width(&self, code: u32) -> f64 {
        self.widths
            .get(&code)
            .copied()
            .unwrap_or(self.default_width)
            / 1000.0
    }
}

/// `bfchar` / `bfrange` mappings of a ToUnicode CMap.
///
/// Every token and every mapping spends a unit of `budget`, and one CMap
/// makes at most [`MAX_CMAP_ENTRIES`] mappings: a range is clamped per
/// entry, so many (or overlapping) ranges must not add up to millions.
fn parse_cmap(data: &[u8], budget: &Budget) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    let mut lx = Lexer::charged(data, budget);
    let code = |s: &[u8]| s.iter().fold(0u32, |a, &b| a << 8 | u32::from(b));
    let utf16 = |s: &[u8]| -> String {
        let units: Vec<u16> = s
            .chunks(2)
            .map(|p| u16::from(p[0]) << 8 | u16::from(*p.get(1).unwrap_or(&0)))
            .collect();
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{fffd}'))
            .collect()
    };
    let mut toks = Vec::new();
    while let Some(t) = lx.next() {
        toks.push(t);
        if toks.len() > 2_000_000 {
            break;
        }
    }
    // Spend one more mapping (and a unit of work); `false` once the CMap
    // has made MAX_CMAP_ENTRIES. Counted per mapping made, not by the map's
    // size: overlapping ranges overwrite the same codes again and again.
    let made = Cell::new(0usize);
    let may_map = || {
        made.set(made.get() + 1);
        made.get() <= MAX_CMAP_ENTRIES && budget.op()
    };
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Kw(k) if k == b"beginbfchar" => {
                i += 1;
                while i + 1 < toks.len() {
                    match (&toks[i], &toks[i + 1]) {
                        (Tok::Str(src), Tok::Str(dst)) => {
                            if !may_map() {
                                return map;
                            }
                            map.insert(code(src), utf16(dst));
                            i += 2;
                        }
                        (Tok::Str(src), Tok::Name(n)) => {
                            if !may_map() {
                                return map;
                            }
                            if let Some(c) = glyph_char(n) {
                                map.insert(code(src), c.to_string());
                            }
                            i += 2;
                        }
                        _ => break,
                    }
                }
            }
            Tok::Kw(k) if k == b"beginbfrange" => {
                i += 1;
                while i + 2 < toks.len() {
                    let (Tok::Str(lo), Tok::Str(hi)) = (&toks[i], &toks[i + 1]) else {
                        break;
                    };
                    let (lo, hi) = (code(lo), code(hi));
                    let hi = hi.min(lo.saturating_add(65535));
                    match &toks[i + 2] {
                        Tok::Str(dst) => {
                            let base: Vec<u16> = dst
                                .chunks(2)
                                .map(|p| u16::from(p[0]) << 8 | u16::from(*p.get(1).unwrap_or(&0)))
                                .collect();
                            for (k, c) in (lo..=hi).enumerate() {
                                if !may_map() {
                                    return map;
                                }
                                let mut units = base.clone();
                                if let Some(last) = units.last_mut() {
                                    *last = last.wrapping_add(k as u16);
                                }
                                let s: String = char::decode_utf16(units)
                                    .map(|r| r.unwrap_or('\u{fffd}'))
                                    .collect();
                                map.insert(c, s);
                            }
                            i += 3;
                        }
                        Tok::ArrOpen => {
                            let mut j = i + 3;
                            let mut c = lo;
                            while j < toks.len() && toks[j] != Tok::ArrClose {
                                if let Tok::Str(dst) = &toks[j] {
                                    if c <= hi {
                                        if !may_map() {
                                            return map;
                                        }
                                        map.insert(c, utf16(dst));
                                    }
                                    c += 1;
                                }
                                j += 1;
                            }
                            i = j + 1;
                        }
                        _ => break,
                    }
                }
            }
            _ => i += 1,
        }
    }
    map
}

fn win_ansi(b: u8) -> Option<char> {
    match b {
        0x20..=0x7E => Some(char::from(b)),
        0x80..=0xFF => opccore::codepage::decode(1252, b),
        _ => None,
    }
}

fn mac_roman(b: u8) -> Option<char> {
    const HIGH: [u16; 128] = [
        0xc4, 0xc5, 0xc7, 0xc9, 0xd1, 0xd6, 0xdc, 0xe1, 0xe0, 0xe2, 0xe4, 0xe3, 0xe5, 0xe7, 0xe9,
        0xe8, 0xea, 0xeb, 0xed, 0xec, 0xee, 0xef, 0xf1, 0xf3, 0xf2, 0xf4, 0xf6, 0xf5, 0xfa, 0xf9,
        0xfb, 0xfc, 0x2020, 0xb0, 0xa2, 0xa3, 0xa7, 0x2022, 0xb6, 0xdf, 0xae, 0xa9, 0x2122, 0xb4,
        0xa8, 0x2260, 0xc6, 0xd8, 0x221e, 0xb1, 0x2264, 0x2265, 0xa5, 0xb5, 0x2202, 0x2211, 0x220f,
        0x3c0, 0x222b, 0xaa, 0xba, 0x3a9, 0xe6, 0xf8, 0xbf, 0xa1, 0xac, 0x221a, 0x192, 0x2248,
        0x2206, 0xab, 0xbb, 0x2026, 0xa0, 0xc0, 0xc3, 0xd5, 0x152, 0x153, 0x2013, 0x2014, 0x201c,
        0x201d, 0x2018, 0x2019, 0xf7, 0x25ca, 0xff, 0x178, 0x2044, 0x20ac, 0x2039, 0x203a, 0xfb01,
        0xfb02, 0x2021, 0xb7, 0x201a, 0x201e, 0x2030, 0xc2, 0xca, 0xc1, 0xcb, 0xc8, 0xcd, 0xce,
        0xcf, 0xcc, 0xd3, 0xd4, 0xf8ff, 0xd2, 0xda, 0xdb, 0xd9, 0x131, 0x2c6, 0x2dc, 0xaf, 0x2d8,
        0x2d9, 0x2da, 0xb8, 0x2dd, 0x2db, 0x2c7,
    ];
    match b {
        0x20..=0x7E => Some(char::from(b)),
        0x80..=0xFF => char::from_u32(u32::from(HIGH[(b - 0x80) as usize])),
        _ => None,
    }
}

/// A glyph name's character (the Adobe names a text font uses, `uniXXXX`
/// and `uXXXX[XX]`).
fn glyph_char(name: &str) -> Option<char> {
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() >= 4 {
            return u32::from_str_radix(&hex[..4], 16)
                .ok()
                .and_then(char::from_u32);
        }
    }
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                return char::from_u32(v);
            }
        }
    }
    let base = name.split('.').next().unwrap_or(name);
    if base.len() == 1 {
        return base.chars().next();
    }
    const NAMES: [(&str, u32); 98] = [
        ("space", 0x20),
        ("exclam", 0x21),
        ("quotedbl", 0x22),
        ("numbersign", 0x23),
        ("dollar", 0x24),
        ("percent", 0x25),
        ("ampersand", 0x26),
        ("quotesingle", 0x27),
        ("quoteright", 0x2019),
        ("parenleft", 0x28),
        ("parenright", 0x29),
        ("asterisk", 0x2a),
        ("plus", 0x2b),
        ("comma", 0x2c),
        ("hyphen", 0x2d),
        ("period", 0x2e),
        ("slash", 0x2f),
        ("zero", 0x30),
        ("one", 0x31),
        ("two", 0x32),
        ("three", 0x33),
        ("four", 0x34),
        ("five", 0x35),
        ("six", 0x36),
        ("seven", 0x37),
        ("eight", 0x38),
        ("nine", 0x39),
        ("colon", 0x3a),
        ("semicolon", 0x3b),
        ("less", 0x3c),
        ("equal", 0x3d),
        ("greater", 0x3e),
        ("question", 0x3f),
        ("at", 0x40),
        ("bracketleft", 0x5b),
        ("backslash", 0x5c),
        ("bracketright", 0x5d),
        ("asciicircum", 0x5e),
        ("underscore", 0x5f),
        ("grave", 0x60),
        ("quoteleft", 0x2018),
        ("braceleft", 0x7b),
        ("bar", 0x7c),
        ("braceright", 0x7d),
        ("asciitilde", 0x7e),
        ("bullet", 0x2022),
        ("endash", 0x2013),
        ("emdash", 0x2014),
        ("quotedblleft", 0x201c),
        ("quotedblright", 0x201d),
        ("quotesinglbase", 0x201a),
        ("quotedblbase", 0x201e),
        ("ellipsis", 0x2026),
        ("Euro", 0x20ac),
        ("trademark", 0x2122),
        ("copyright", 0xa9),
        ("registered", 0xae),
        ("degree", 0xb0),
        ("section", 0xa7),
        ("paragraph", 0xb6),
        ("periodcentered", 0xb7),
        ("nbspace", 0xa0),
        ("fi", 0xfb01),
        ("fl", 0xfb02),
        ("dagger", 0x2020),
        ("daggerdbl", 0x2021),
        ("guillemotleft", 0xab),
        ("guillemotright", 0xbb),
        ("sterling", 0xa3),
        ("yen", 0xa5),
        ("cent", 0xa2),
        ("eacute", 0xe9),
        ("egrave", 0xe8),
        ("ecircumflex", 0xea),
        ("edieresis", 0xeb),
        ("aacute", 0xe1),
        ("agrave", 0xe0),
        ("acircumflex", 0xe2),
        ("adieresis", 0xe4),
        ("aring", 0xe5),
        ("ccedilla", 0xe7),
        ("iacute", 0xed),
        ("idieresis", 0xef),
        ("ntilde", 0xf1),
        ("oacute", 0xf3),
        ("odieresis", 0xf6),
        ("uacute", 0xfa),
        ("udieresis", 0xfc),
        ("germandbls", 0xdf),
        ("Eacute", 0xc9),
        ("Adieresis", 0xc4),
        ("Odieresis", 0xd6),
        ("Udieresis", 0xdc),
        ("minus", 0x2212),
        ("multiply", 0xd7),
        ("divide", 0xf7),
        ("plusminus", 0xb1),
        ("onehalf", 0xbd),
    ];
    NAMES
        .iter()
        .find(|(n, _)| *n == base)
        .and_then(|&(_, c)| char::from_u32(c))
}

// ---------------------------------------------------------------------------
// Content streams

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// One glyph, placed: its baseline origin, advance and size in user space.
#[derive(Clone, Debug)]
struct Glyph {
    x: f64,
    y: f64,
    w: f64,
    size: f64,
    text: String,
    bold: bool,
    italic: bool,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    font: Option<String>,
    size: f64,
    char_space: f64,
    word_space: f64,
    scale: f64,
    leading: f64,
}

struct Interp<'a> {
    file: &'a File,
    glyphs: Vec<Glyph>,
    fonts: HashMap<String, Rc<Font>>,
    /// Form XObjects being run, against a form that draws itself.
    form_stack: Vec<u32>,
}

impl Interp<'_> {
    fn font(&mut self, resources: &Dict, name: &str) -> Rc<Font> {
        // Keyed by the font dictionary's identity, so two forms' `/F1` differ.
        let fonts = self
            .file
            .lookup(resources, "Font")
            .and_then(|f| f.as_dict());
        let entry = fonts.and_then(|f| dict_get(f, name));
        // A font object is loaded once for the document; a font written
        // inline in a resource dictionary only for this page.
        let shared = match entry {
            Some(Obj::Ref(r)) => Some(*r),
            _ => None,
        };
        let key = format!("{name}@{:p}", resources);
        if let Some(f) = shared.and_then(|r| self.file.fonts.borrow().get(&r).cloned()) {
            return f;
        }
        if let Some(f) = self.fonts.get(&key) {
            return f.clone();
        }
        let font = Rc::new(
            entry
                .and_then(|e| self.file.dict_of(e))
                .map(|d| Font::load(self.file, d))
                .unwrap_or(Font {
                    default_width: 500.0,
                    encoding: Some(Box::new(std::array::from_fn(|b| win_ansi(b as u8)))),
                    ..Font::default()
                }),
        );
        match shared {
            Some(r) => {
                self.file.fonts.borrow_mut().insert(r, font.clone());
            }
            None => {
                self.fonts.insert(key, font.clone());
            }
        }
        font
    }

    fn content(&mut self, data: &[u8], resources: &Dict, ctm: Matrix, depth: usize) {
        if depth > 8 || self.glyphs.len() > 2_000_000 {
            return;
        }
        let mut gs = GState {
            ctm,
            font: None,
            size: 0.0,
            char_space: 0.0,
            word_space: 0.0,
            scale: 1.0,
            leading: 0.0,
        };
        let mut stack: Vec<GState> = Vec::new();
        let mut tm = IDENTITY;
        let mut tlm = IDENTITY;
        let mut font: Rc<Font> = Rc::default();
        let mut ops: Vec<Obj> = Vec::new();
        // Charged per byte scanned, every run: a form drawn again is lexed
        // again, and pays again; so do operands, strings and white space.
        let budget = &self.file.budget;
        let mut lx = Lexer::charged(data, budget);
        while let Some(tok) = lx.next() {
            let Tok::Kw(op) = tok else {
                if let Some(o) = lx.object_from(tok, 0) {
                    ops.push(o);
                    if ops.len() > 10_000 {
                        ops.clear();
                    }
                }
                continue;
            };
            let num = |i: usize| -> f64 {
                ops.len()
                    .checked_sub(i)
                    .and_then(|k| ops.get(k))
                    .and_then(Obj::as_num)
                    .unwrap_or(0.0)
            };
            match op.as_slice() {
                b"q" => {
                    if stack.len() < 64 {
                        stack.push(gs.clone());
                    }
                }
                b"Q" => {
                    if let Some(prev) = stack.pop() {
                        gs = prev;
                        if let Some(name) = gs.font.clone() {
                            font = self.font(resources, &name);
                        }
                    }
                }
                b"cm" => {
                    let m = [num(6), num(5), num(4), num(3), num(2), num(1)];
                    gs.ctm = mul(&m, &gs.ctm);
                }
                b"BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                b"Tf" => {
                    if let Some(name) = ops.len().checked_sub(2).and_then(|k| ops[k].as_name()) {
                        let name = name.to_string();
                        font = self.font(resources, &name);
                        gs.font = Some(name);
                    }
                    gs.size = num(1);
                }
                b"Tc" => gs.char_space = num(1),
                b"Tw" => gs.word_space = num(1),
                b"Tz" => gs.scale = num(1) / 100.0,
                b"TL" => gs.leading = num(1),
                b"Td" => {
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, num(2), num(1)], &tlm);
                    tm = tlm;
                }
                b"TD" => {
                    gs.leading = -num(1);
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, num(2), num(1)], &tlm);
                    tm = tlm;
                }
                b"Tm" => {
                    tlm = [num(6), num(5), num(4), num(3), num(2), num(1)];
                    tm = tlm;
                }
                b"T*" => {
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -gs.leading], &tlm);
                    tm = tlm;
                }
                b"Tj" | b"'" | b"\"" => {
                    if op.as_slice() != b"Tj" {
                        if op.as_slice() == b"\"" {
                            gs.word_space = num(3);
                            gs.char_space = num(2);
                        }
                        tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -gs.leading], &tlm);
                        tm = tlm;
                    }
                    if let Some(Obj::Str(s)) = ops.last() {
                        self.show(s, &font, &gs, &mut tm);
                    }
                }
                b"TJ" => {
                    if let Some(Obj::Array(items)) = ops.last() {
                        for item in items {
                            match item {
                                Obj::Str(s) => self.show(s, &font, &gs, &mut tm),
                                Obj::Num(n) => {
                                    let tx = -n / 1000.0 * gs.size * gs.scale;
                                    tm = mul(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], &tm);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                b"Do" => {
                    if let Some(name) = ops.last().and_then(Obj::as_name) {
                        let name = name.to_string();
                        self.form(resources, &name, &gs.ctm, depth);
                    }
                }
                b"BI" => {
                    // An inline image: skip its binary data to `EI`.
                    // `ID` is found once; the data runs from there to the
                    // first `EI` standing alone. What is skipped is charged.
                    let rest = &data[lx.pos..];
                    let mut at = None;
                    if let Some(id) = super::find(rest, b"ID") {
                        let mut i = id + 2;
                        while let Some(k) = super::find(&rest[i..], b"EI") {
                            let p = i + k;
                            let before = is_ws(rest[p - 1]);
                            let after = rest.get(p + 2).is_none_or(|&b| is_ws(b));
                            if before && after {
                                at = Some(p + 2);
                                break;
                            }
                            i = p + 2;
                        }
                    }
                    let skipped = at.unwrap_or(rest.len());
                    if !budget.scanned(skipped) {
                        return;
                    }
                    lx.pos += skipped;
                }
                _ => {}
            }
            ops.clear();
        }
    }

    fn form(&mut self, resources: &Dict, name: &str, ctm: &Matrix, depth: usize) {
        let xobjects = self
            .file
            .lookup(resources, "XObject")
            .and_then(|x| x.as_dict());
        let Some(entry) = xobjects.and_then(|x| dict_get(x, name)) else {
            return;
        };
        let id = match entry {
            Obj::Ref(r) => *r,
            _ => u32::MAX,
        };
        if self.form_stack.contains(&id) {
            return;
        }
        let Obj::Stream(d, _) = self.file.resolve(entry) else {
            return;
        };
        if dict_get(d, "Subtype")
            .map(|s| self.file.resolve(s))
            .and_then(Obj::as_name)
            != Some("Form")
        {
            return;
        }
        let Some(data) = self.file.stream_data(entry) else {
            return;
        };
        let m = match self.file.lookup(d, "Matrix") {
            Some(Obj::Array(a)) if a.len() == 6 => {
                let n: Vec<f64> = a
                    .iter()
                    .map(|o| self.file.resolve(o).as_num().unwrap_or(0.0))
                    .collect();
                [n[0], n[1], n[2], n[3], n[4], n[5]]
            }
            _ => IDENTITY,
        };
        // The form's own resources, or the caller's: borrowed, not copied
        // for every draw.
        let file = self.file;
        let res: &Dict = file
            .lookup(d, "Resources")
            .and_then(|r| r.as_dict())
            .unwrap_or(resources);
        self.form_stack.push(id);
        self.content(&data, res, mul(&m, ctm), depth + 1);
        self.form_stack.pop();
    }

    /// Every glyph is a unit of work: one long string cannot make millions
    /// of them for free.
    fn show(&mut self, s: &[u8], font: &Font, gs: &GState, tm: &mut Matrix) {
        for code in font.codes(s) {
            if !self.file.budget.op() {
                return;
            }
            let w0 = font.width(code);
            let text = font.text(code);
            let is_space = !font.two_byte && code == 32;
            let trm = mul(
                &[gs.size * gs.scale, 0.0, 0.0, gs.size, 0.0, 0.0],
                &mul(tm, &gs.ctm),
            );
            let tx = (w0 * gs.size + gs.char_space + if is_space { gs.word_space } else { 0.0 })
                * gs.scale;
            let full = mul(tm, &gs.ctm);
            let hscale = (full[0] * full[0] + full[1] * full[1]).sqrt();
            let vscale = (full[2] * full[2] + full[3] * full[3]).sqrt();
            if !text.is_empty() {
                self.glyphs.push(Glyph {
                    x: trm[4],
                    y: trm[5],
                    w: tx * hscale,
                    size: (gs.size * vscale).abs(),
                    text,
                    bold: font.bold,
                    italic: font.italic,
                });
            }
            *tm = mul(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], tm);
        }
    }
}

// ---------------------------------------------------------------------------
// Layout

/// A piece of a line in one font style.
#[derive(Clone, Debug)]
struct Piece {
    text: String,
    bold: bool,
    italic: bool,
}

#[derive(Clone, Debug)]
struct Line {
    y: f64,
    x1: f64,
    size: f64,
    pieces: Vec<Piece>,
}

impl Line {
    fn text(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }
}

/// Glyphs in content order into lines: a new line where the baseline moves
/// by more than half the size, or the text jumps back left.
fn make_lines(glyphs: Vec<Glyph>) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut cur: Option<Line> = None;
    for g in glyphs {
        if g.size <= 0.0 || !g.x.is_finite() || !g.y.is_finite() {
            continue;
        }
        let blank = g.text.chars().all(char::is_whitespace);
        let same = cur.as_ref().is_some_and(|l| {
            (g.y - l.y).abs() <= 0.5 * l.size.max(g.size) && g.x >= l.x1 - l.size.max(g.size)
        });
        if !same {
            if blank {
                continue;
            }
            if let Some(l) = cur.take() {
                lines.push(l);
            }
            cur = Some(Line {
                y: g.y,
                x1: g.x + g.w,
                size: g.size,
                pieces: Vec::new(),
            });
        }
        let line = cur.as_mut().expect("a line is open");
        let gap = g.x - line.x1;
        let ends_blank = line
            .pieces
            .last()
            .is_none_or(|p| p.text.ends_with(char::is_whitespace));
        let mut sep = "";
        // A wide gap is a tab stop or a table column. Right after a space
        // glyph a smaller one is too: Word draws a list marker, a space, then
        // jumps to the text (0.45 em on after `1.`, 0.75 em after a bullet),
        // while ordinary and justified text leaves no gap after a space (a
        // justified line widens the space's own advance).
        let em = g.size.max(line.size);
        let wide = if ends_blank { 0.3 * em } else { 0.6 * em };
        if gap > wide && !blank && !line.pieces.is_empty() {
            if let Some(p) = line.pieces.last_mut() {
                let kept = p.text.trim_end().len();
                p.text.truncate(kept);
            }
            sep = "\t";
        } else if gap > 0.18 * g.size && !blank && !ends_blank {
            sep = " ";
        }
        let text = format!("{sep}{}", g.text);
        let empty = line.pieces.is_empty();
        match line.pieces.last_mut() {
            Some(p) if p.bold == g.bold && p.italic == g.italic => p.text.push_str(&text),
            _ if blank && empty => {}
            _ => line.pieces.push(Piece {
                text,
                bold: g.bold,
                italic: g.italic,
            }),
        }
        if !blank {
            line.size = line.size.max(g.size);
        }
        line.x1 = line.x1.max(g.x + g.w);
    }
    if let Some(l) = cur {
        lines.push(l);
    }
    // Trailing white space of each line says nothing.
    for l in &mut lines {
        while let Some(p) = l.pieces.last_mut() {
            let t = p.text.trim_end().to_string();
            if t.is_empty() {
                l.pieces.pop();
            } else {
                p.text = t;
                break;
            }
        }
    }
    lines.retain(|l| !l.pieces.is_empty());
    lines
}

/// A list marker at the start of a line: a bullet glyph, or `1.` / `a)`,
/// followed by a tab (a gap). Returns whether it is numbered and how many
/// characters it takes, with the tab.
fn list_marker(text: &str) -> Option<(bool, usize)> {
    let (marker, rest) = text.split_once('\t')?;
    let m = marker.trim();
    if m.is_empty() || m.chars().count() > 4 || rest.trim().is_empty() {
        return None;
    }
    let bullet = m.chars().count() == 1
        && matches!(
            m.chars().next(),
            Some(
                '\u{f0b7}'
                    | '\u{2022}'
                    | '\u{b7}'
                    | 'o'
                    | '\u{a7}'
                    | '\u{f0a7}'
                    | '\u{25aa}'
                    | '\u{25cf}'
                    | '\u{2013}'
                    | '-'
                    | '\u{f076}'
                    | '\u{f0d8}'
            )
        );
    let numbered = marker_is_numbered(m);
    (bullet || numbered).then(|| (numbered, marker.chars().count() + 1))
}

fn layout(pages: &[Vec<Line>], budget: &Budget) -> Document {
    // The body size: the most common line size, weighted by text length.
    let mut weight: HashMap<i64, usize> = HashMap::new();
    for l in pages.iter().flatten() {
        *weight.entry((l.size * 2.0).round() as i64).or_default() += l.text().chars().count();
    }
    let body = weight
        .iter()
        .max_by_key(|(size, n)| (**n, -**size))
        .map(|(s, _)| *s as f64 / 2.0)
        .unwrap_or(11.0);
    // Sizes clearly above the body's are headings, the largest level 1.
    let mut heads: Vec<i64> = weight
        .keys()
        .copied()
        .filter(|&s| s as f64 / 2.0 > body * 1.12)
        .collect();
    heads.sort_unstable_by(|a, b| b.cmp(a));
    heads.truncate(6);
    let level_of = |size: f64| -> Option<u8> {
        let k = (size * 2.0).round() as i64;
        heads.iter().position(|&h| h == k).map(|i| i as u8 + 1)
    };

    let mut out = Builder::new(budget);
    for lines in pages {
        let right = lines.iter().map(|l| l.x1).fold(f64::MIN, f64::max);
        let mut prev: Option<&Line> = None;
        let mut pitch: Option<f64> = None;
        let mut props = ParProps::default();
        for line in lines {
            let text = line.text();
            let marker = list_marker(&text);
            let level = level_of(line.size);
            let new_para = match prev {
                None => true,
                Some(p) => {
                    let dy = p.y - line.y;
                    let usual = pitch.unwrap_or(1.25 * p.size.max(line.size));
                    dy < 0.0
                        || dy > usual * 1.3 + 0.5
                        || (p.size - line.size).abs() > 0.5
                        || p.x1 < right - 2.5 * p.size
                        || marker.is_some()
                        || level.is_some()
                }
            };
            if new_para {
                if prev.is_some() {
                    out.end_para(std::mem::take(&mut props), false);
                }
                pitch = None;
                props = match level {
                    Some(l) => heading_props(l),
                    None => ParProps::default(),
                };
                if let Some((numbered, _)) = marker.filter(|_| level.is_none()) {
                    props.num_id = Some(if numbered { 2 } else { 1 });
                }
            } else {
                if let Some(p) = prev {
                    pitch = Some(p.y - line.y);
                }
                out.text(" ", &RunProps::default());
            }
            let mut skip = match (new_para, marker) {
                (true, Some((_, n))) if level.is_none() => n,
                _ => 0,
            };
            for piece in &line.pieces {
                let mut t: String = piece.text.chars().skip(skip).collect();
                skip = skip.saturating_sub(piece.text.chars().count());
                if t.is_empty() {
                    continue;
                }
                let props = RunProps {
                    bold: piece.bold,
                    italic: piece.italic,
                    ..RunProps::default()
                };
                // Tabs between columns stay tabs.
                while let Some(i) = t.find('\t') {
                    out.text(&t[..i], &props);
                    out.tab(&props);
                    t = t[i + 1..].to_string();
                }
                out.text(&t, &props);
            }
            prev = Some(line);
        }
        if prev.is_some() {
            out.end_para(std::mem::take(&mut props), false);
        }
    }
    out.finish(ParProps::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::paragraph_texts;
    use crate::model::{Block, Inline};

    /// A PDF from objects (`n 0 obj … endobj` bodies, 1-based), with a
    /// correct xref and a trailer naming object 1 the catalog.
    fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend(body);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend(b"\nendstream");
        v
    }

    /// A zlib stream of stored DEFLATE blocks (no compressor needed).
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x78, 0x01];
        let chunks: Vec<&[u8]> = if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(65535).collect()
        };
        for (i, c) in chunks.iter().enumerate() {
            v.push(u8::from(i + 1 == chunks.len()));
            let len = c.len() as u16;
            v.extend(len.to_le_bytes());
            v.extend((!len).to_le_bytes());
            v.extend_from_slice(c);
        }
        v.extend([0, 0, 0, 0]); // adler32, unchecked
        v
    }

    /// One page, Helvetica as F1 (and F2 bold), drawing `content`.
    fn one_page(content: &[u8]) -> Vec<u8> {
        pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>".to_vec(),
            stream("", content),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec(),
        ])
    }

    #[test]
    fn lines_paragraphs_and_spaces() {
        let content = b"BT /F1 11 Tf 72 700 Td (Hello world, this line runs on) Tj ET\n\
            BT /F1 11 Tf 72 687 Td (and wraps here.) Tj ET\n\
            BT /F1 11 Tf 72 660 Td [(Second)-300(para)] TJ ET\n\
            BT /F2 11 Tf 72 640 Td (Bold) Tj /F1 11 Tf ( tail) Tj ET";
        let doc = import_pdf(&one_page(content)).unwrap();
        assert_eq!(
            paragraph_texts(&doc),
            [
                "Hello world, this line runs on and wraps here.",
                "Second para",
                "Bold tail"
            ]
        );
        let Block::Paragraph(p) = &doc.body[2] else {
            panic!()
        };
        let Inline::Run(r) = &p.content[0] else {
            panic!()
        };
        assert!(r.props.bold && r.text == "Bold");
    }

    #[test]
    fn flate_content_and_a_to_unicode_cmap() {
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
            1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
            2 beginbfchar <0001> <0416> <0002> <0061> endbfchar\n\
            1 beginbfrange <0003> <0005> <0062> endbfrange\n\
            1 beginbfrange <0006> <0007> [<00660069> <D83DDE00>] endbfrange\n\
            endcmap CMapName currentdict /CMap defineresource pop end end";
        let content = b"BT /F1 12 Tf 72 700 Td <000100020003000400050006 0007> Tj ET";
        let file = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
            stream("/Filter /FlateDecode", &zlib_stored(content)),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+Calibri-Italic /Encoding /Identity-H /DescendantFonts [7 0 R] /ToUnicode 6 0 R >>".to_vec(),
            stream("", cmap),
            b"<< /Type /Font /Subtype /CIDFontType2 /DW 500 /W [1 [600 500] 3 7 500] >>".to_vec(),
        ]);
        let doc = import_pdf(&file).unwrap();
        assert_eq!(paragraph_texts(&doc), ["\u{416}abcdfi\u{1f600}"]);
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        let Inline::Run(r) = &p.content[0] else {
            panic!()
        };
        assert!(r.props.italic);
    }

    #[test]
    fn differences_encoding_and_object_streams() {
        // The page and font live in an object stream; the font maps 1 and 2
        // to glyph names.
        let inner = b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 6 0 R >> >> /Contents 4 0 R >> << /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding << /Differences [1 /eacute /Euro] >> >>";
        let second = inner.windows(4).position(|w| w == b">> <").unwrap() + 3;
        let header = format!("3 0 6 {second} ");
        let mut objstm = header.clone().into_bytes();
        objstm.extend_from_slice(inner);
        let file = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"null".to_vec(),
            stream("", b"BT /F1 10 Tf 50 50 Td (caf\\001 5\\002) Tj ET"),
            stream(
                &format!(
                    "/Type /ObjStm /N 2 /First {} /Filter /FlateDecode",
                    header.len()
                ),
                &zlib_stored(&objstm),
            ),
        ]);
        // Object 3 is defined directly as `null`, but the page in the stream
        // must still be found: drop the direct one.
        let mut file = file;
        let direct = b"3 0 obj\nnull\nendobj\n";
        let at = super::super::find(&file, direct).unwrap();
        file.drain(at..at + direct.len());
        let doc = import_pdf(&file).unwrap();
        assert_eq!(paragraph_texts(&doc), ["caf\u{e9} 5\u{20ac}"]);
    }

    #[test]
    fn our_own_pdf_export_reads_back() {
        let doc = crate::markdown::from_markdown(
            "# Report\n\nFirst paragraph with **bold** words.\n\nSecond paragraph.\n",
        );
        let bytes = crate::export::to_pdf(&doc, &crate::export::PdfOptions::default());
        let back = import_pdf(&bytes).unwrap();
        let text = paragraph_texts(&back).join("\n");
        for want in [
            "Report",
            "First paragraph with bold words.",
            "Second paragraph.",
        ] {
            assert!(text.contains(want), "{want:?} in {text:?}");
        }
    }

    #[test]
    fn encrypted_and_textless_files_are_refused() {
        let mut enc = one_page(b"BT /F1 11 Tf 72 700 Td (x) Tj ET");
        let at = super::super::find(&enc, b"/Root 1 0 R").unwrap();
        enc.splice(at..at, b"/Encrypt 9 0 R ".iter().copied());
        assert!(import_pdf(&enc).unwrap_err().contains("encrypted"));
        let empty = one_page(b"0 0 m 100 100 l S");
        assert!(import_pdf(&empty).unwrap_err().contains("no text"));
        assert!(import_pdf(b"%PDF-1.4\n%%EOF").is_err());
    }

    #[test]
    fn indirect_length_and_binary_data_do_not_confuse_the_scan() {
        // A stream whose length is an indirect object, holding bytes that
        // look like `9 0 obj`.
        let content = b"BT /F1 11 Tf 72 700 Td (Safe) Tj ET\n% 9 0 obj << /Type /Page >> endobj\n";
        let mut file = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
            [b"<< /Length 6 0 R >>\nstream\n".as_slice(), content, b"\nendstream"].concat(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            format!("{}", content.len()).into_bytes(),
        ]);
        let doc = import_pdf(&file).unwrap();
        assert_eq!(paragraph_texts(&doc), ["Safe"]);
        // No xref or trailer at all: the catalog is found by its type.
        let cut = super::super::find(&file, b"xref").unwrap();
        file.truncate(cut);
        assert_eq!(paragraph_texts(&import_pdf(&file).unwrap()), ["Safe"]);
    }

    #[test]
    fn forms_tables_and_list_markers() {
        let content = b"BT /F1 11 Tf 72 700 Td (\\225) Tj 90 0 Td (Apples) Tj ET\n\
            BT /F1 11 Tf 72 686 Td (1.) Tj 24 0 Td (First) Tj ET\n\
            BT /F1 11 Tf 72 650 Td (North) Tj 150 0 Td (South) Tj 150 0 Td (East) Tj ET\n\
            /Fm1 Do";
        let file = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >> >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_vec(),
            stream("", content),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
            stream(
                "/Type /XObject /Subtype /Form /Matrix [1 0 0 1 0 -100] /Resources << /Font << /F1 5 0 R >> /XObject << /Me 6 0 R >> >>",
                b"BT /F1 11 Tf 72 500 Td (In a form) Tj ET /Me Do",
            ),
        ]);
        let doc = import_pdf(&file).unwrap();
        assert_eq!(
            paragraph_texts(&doc),
            ["Apples", "First", "North\tSouth\tEast", "In a form"]
        );
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        assert_eq!(p.props.num_id, Some(1));
        let Block::Paragraph(p) = &doc.body[1] else {
            panic!()
        };
        assert_eq!(p.props.num_id, Some(2));
    }

    #[test]
    fn headings_come_from_larger_sizes() {
        let content = b"BT /F2 16 Tf 72 720 Td (Title) Tj ET\n\
            BT /F1 11 Tf 72 690 Td (Body text that is the most common size here.) Tj ET\n\
            BT /F2 13 Tf 72 660 Td (Sub) Tj ET\n\
            BT /F1 11 Tf 72 640 Td (More body text in the usual size.) Tj ET";
        let doc = import_pdf(&one_page(content)).unwrap();
        let styles: Vec<Option<String>> = doc
            .body
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => p.props.style_id.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(
            styles,
            [Some("Heading1".into()), None, Some("Heading2".into()), None]
        );
    }

    #[test]
    fn cut_or_garbage_input_never_panics() {
        let file = one_page(b"BT /F1 11 Tf 72 700 Td [(A)-500(B)] TJ T* (C) ' ET BI /W 1 ID \x00\xff EI q 1 0 0 1 5 5 cm Q");
        for n in (0..file.len()).step_by(3) {
            let _ = import_pdf(&file[..n]);
        }
        let _ = import_pdf(b"%PDF-1.7 1 0 obj << /Length 99999999999 >> stream\nabc");
        let _ = import_pdf(b"%PDF-1.7 1 0 obj [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[ endobj");
        let _ = import_pdf(b"%PDF 1 0 obj << /Type /Pages /Kids [1 0 R] >> endobj trailer << /Root 2 0 R >> 2 0 obj << /Pages 1 0 R >> endobj");
    }

    /// A small budget, so a bomb that got through fails fast and small.
    fn small() -> Budget {
        Budget::new(1 << 20, 100_000)
    }

    /// A zlib stream (no checksum checked) of one fixed-Huffman block: `a`,
    /// then `matches` copies of 258 bytes at distance 1, about 1000:1.
    fn zlib_bomb(matches: usize) -> Vec<u8> {
        let mut bits: Vec<bool> = vec![true, true, false];
        let push = |code: u32, len: u32, bits: &mut Vec<bool>| {
            for i in (0..len).rev() {
                bits.push(code >> i & 1 == 1);
            }
        };
        push(0x30 + u32::from(b'a'), 8, &mut bits);
        for _ in 0..matches {
            push(0xC5, 8, &mut bits);
            push(0, 5, &mut bits);
        }
        push(0, 7, &mut bits);
        let mut out = vec![0x78, 0x01];
        let start = out.len();
        out.resize(start + bits.len().div_ceil(8), 0);
        for (i, bit) in bits.iter().enumerate() {
            if *bit {
                out[start + i / 8] |= 1 << (i % 8);
            }
        }
        out
    }

    #[test]
    fn a_flate_bomb_fails_the_import_within_its_budget() {
        // Content that inflates to 100 MiB, against a 1 MiB budget.
        let bomb = zlib_bomb((100 << 20) / 258);
        let file = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_vec(),
            stream("/Filter /FlateDecode", &bomb),
        ]);
        assert_eq!(import_pdf_within(&file, small()).unwrap_err(), TOO_BIG);
        // The cap held during the decode, not after it: what came out is at
        // most the room (an uncapped inflate would hand back 100 MiB).
        let budget = Budget::new(1 << 20, usize::MAX);
        let over = inflate_within(zlib_body(&bomb), &budget).unwrap_err();
        assert!(over.capacity() <= 4 << 20, "grew to {}", over.capacity());
        assert!(budget.exhausted());
        // An object stream is decoded before any page: the same.
        let objstm = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            stream("/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode", &bomb),
        ]);
        assert_eq!(import_pdf_within(&objstm, small()).unwrap_err(), TOO_BIG);
        // One 2 KB stream listed a thousand times joins to 2 MB.
        let content = b"BT ET ".repeat(400);
        let parts = "4 0 R ".repeat(1000);
        let listed = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!("<< /Type /Page /Parent 2 0 R /Contents [{parts}] >>").into_bytes(),
            stream("", &content),
        ]);
        // Ample work: only the joined bytes can fail it.
        let budget = Budget::new(1 << 20, usize::MAX);
        assert_eq!(import_pdf_within(&listed, budget).unwrap_err(), TOO_BIG);
    }

    #[test]
    fn predictor_sizes_from_the_file_are_checked() {
        let data = vec![2u8, 1, 2, 3];
        for parms in [
            "/Predictor 12 /Columns 1000000000000000",
            "/Predictor 12 /Colors 10000000000 /BitsPerComponent 10000000000 /Columns 10000000000",
        ] {
            let text = format!("<< {parms} >>");
            let mut lx = Lexer::new(text.as_bytes());
            let Some(Obj::Dict(d)) = lx.object(0) else {
                panic!()
            };
            assert_eq!(png_predictor(&data, &d), data, "{parms}");
        }
    }

    #[test]
    fn a_fan_of_forms_runs_out_of_budget_not_time() {
        // F1 draws F2 ten times, F2 draws F3 ten times ... F8: 10^8 calls
        // with no cycle and no text.
        let mut objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /F 5 0 R >> >> >>".to_vec(),
            stream("", b"/F Do"),
        ];
        for level in 0..8 {
            let next = 6 + level;
            let draws = if level < 7 {
                "/F Do ".repeat(10)
            } else {
                String::new()
            };
            objs.push(stream(
                &format!(
                    "/Type /XObject /Subtype /Form /Resources << /XObject << /F {next} 0 R >> >>"
                ),
                draws.as_bytes(),
            ));
        }
        let file = pdf(&objs);
        assert_eq!(import_pdf_within(&file, small()).unwrap_err(), TOO_BIG);
        // An inline image with no `ID` and many `EI`s is skipped in one pass.
        let mut content = b"BT /F1 11 Tf 72 700 Td (Shown) Tj ET BI /W 1 ".to_vec();
        content.extend(b"xEIx ".repeat(200_000));
        let file = one_page(&content);
        assert_eq!(
            paragraph_texts(&import_pdf_within(&file, Budget::new(1 << 24, 1_000_000)).unwrap()),
            ["Shown"]
        );
    }

    #[test]
    fn a_cmap_of_huge_ranges_is_bounded() {
        // Overlapping ranges: a small map, but the work is still counted.
        let mut cmap = b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n".to_vec();
        cmap.extend(b"1000 beginbfrange\n");
        for k in 0..1000u32 {
            cmap.extend(format!("<{:04X}> <FFFF> <0041>\n", k).as_bytes());
        }
        cmap.extend(b"endbfrange");
        let budget = small();
        parse_cmap(&cmap, &budget);
        assert!(budget.exhausted());
        // 17 disjoint ranges of 4-byte codes ask for 1 114 112 distinct
        // mappings; with unlimited work the CMap stops at MAX_CMAP_ENTRIES.
        let mut cmap = b"1 begincodespacerange <00000000> <FFFFFFFF> endcodespacerange\n".to_vec();
        cmap.extend(b"17 beginbfrange\n");
        for k in 0..17u32 {
            cmap.extend(format!("<{:04X}0000> <{:04X}FFFF> <0041>\n", k, k).as_bytes());
        }
        cmap.extend(b"endbfrange");
        let budget = Budget::new(1 << 20, usize::MAX);
        assert_eq!(parse_cmap(&cmap, &budget).len(), MAX_CMAP_ENTRIES);
    }

    /// FIX r2: an object stream listing many ids at one offset holds that
    /// object once, and each entry is parsed only within its own span.
    #[test]
    fn object_stream_entries_are_parsed_once_within_their_span() {
        let array = format!("[{}]", "7 ".repeat(20_000));
        let header: String = (100..300).map(|id| format!("{id} 0 ")).collect();
        let mut body = header.clone().into_bytes();
        body.extend(array.as_bytes());
        let file = pdf(&[stream(
            &format!("/Type /ObjStm /N 200 /First {}", header.len()),
            &body,
        )]);
        let f = File::scan(&file, Budget::standard());
        let arrays = f
            .objects
            .values()
            .filter(|o| matches!(o, Obj::Array(_)))
            .count();
        assert_eq!(arrays, 1);
        // Two entries: the first ends where the second begins.
        let body = b"[1 2 3 4 5 6] (second)";
        let file = pdf(&[stream(
            "/Type /ObjStm /N 2 /First 8",
            &[b"8 0 9 3 ".as_slice(), body].concat(),
        )]);
        let f = File::scan(&file, Budget::standard());
        assert_eq!(f.objects.get(&8), Some(&Obj::Array(vec![Obj::Num(1.0)])));
    }

    /// FIX r2: a cached form is lexed again on every draw, and each pass is
    /// charged by the bytes it scans, not by its handful of tokens.
    #[test]
    fn a_large_form_drawn_many_times_runs_out_of_budget() {
        let mut form = b"(".to_vec();
        form.extend(vec![b'a'; 200_000]);
        form.extend(b") pop");
        let content = "/F Do ".repeat(1000);
        let file = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /F 5 0 R >> >> >>".to_vec(),
            stream("", content.as_bytes()),
            stream("/Type /XObject /Subtype /Form", &form),
        ]);
        let budget = Budget::new(1 << 24, 1_000_000);
        assert_eq!(import_pdf_within(&file, budget).unwrap_err(), TOO_BIG);
    }

    /// FIX r2: every glyph is charged, so one long string cannot make
    /// millions of them for the price of one token.
    #[test]
    fn every_glyph_is_charged() {
        let mut content = b"BT /F1 11 Tf 72 700 Td (".to_vec();
        content.extend(vec![b'a'; 200_000]);
        content.extend(b") Tj ET");
        let file = one_page(&content);
        // Scanning the string costs 12.5k units; its glyphs 200k.
        let budget = Budget::new(1 << 24, 100_000);
        assert_eq!(import_pdf_within(&file, budget).unwrap_err(), TOO_BIG);
    }

    /// FIX r2: a font looked up again is the same font, not a copy of its
    /// maps.
    #[test]
    fn fonts_are_shared_not_copied() {
        let file = one_page(b"BT /F1 11 Tf ET");
        let f = File::scan(&file, Budget::standard());
        let pages = f.pages();
        let resources = pages[0]
            .resources
            .as_deref()
            .and_then(|r| f.dict_of(r))
            .unwrap();
        let mut run = Interp {
            file: &f,
            glyphs: Vec::new(),
            fonts: HashMap::new(),
            form_stack: Vec::new(),
        };
        let a = run.font(resources, "F1");
        let b = run.font(resources, "F1");
        assert!(Rc::ptr_eq(&a, &b));
    }

    /// FIX r2: CID widths are capped per font, charged, and their code
    /// arithmetic saturates.
    #[test]
    fn cid_widths_are_bounded() {
        let triples: String = (0..20u64)
            .map(|k| format!("{} {} 500 ", k << 16, (k << 16) | 0xFFFF))
            .collect();
        let font = |w: &str| -> Dict {
            let text =
                format!("<< /Subtype /Type0 /BaseFont /X /DescendantFonts [<< /W [{w}] >>] >>");
            let mut lx = Lexer::new(text.as_bytes());
            match lx.object(0) {
                Some(Obj::Dict(d)) => d,
                o => panic!("{o:?}"),
            }
        };
        let f = File::scan(b"", Budget::new(1 << 20, usize::MAX));
        assert_eq!(
            Font::load(&f, &font(&triples)).widths.len(),
            MAX_CMAP_ENTRIES
        );
        let f = File::scan(b"", Budget::new(1 << 20, 50_000));
        assert!(Font::load(&f, &font(&triples)).widths.len() < 50_000);
        assert!(f.budget.exhausted());
        // Codes at the top of the range neither panic nor wrap.
        let f = File::scan(b"", Budget::standard());
        let top = Font::load(&f, &font("4294967295 [500 600] 4294967290 4294967295 700"));
        assert_eq!(top.widths.get(&u32::MAX), Some(&700.0));
    }
}
