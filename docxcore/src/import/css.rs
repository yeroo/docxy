//! The small part of CSS the HTML importer reads (#1872): declarations
//! (`style=""` and `<style>` rule bodies), style sheets of simple selectors,
//! and the values of the properties [`super::html`] maps onto runs and
//! paragraphs.
//!
//! Selectors are type, `*`, `.class`, `#id`, their compounds, and the
//! descendant (space) and child (`>`) combinators, in comma groups. A
//! selector with anything else (`+`, `~`, `[attr]`, a pseudo-class or
//! -element, an escape) is dropped alone; the rest of its group still
//! applies. Every at-rule is skipped with its block, and `<!--`, `-->` and
//! comments are ignored. Names compare ASCII case-insensitively (most email
//! is quirks mode, where classes and ids do).
//!
//! Everything is one forward pass over its text with a depth counter for
//! braces (no recursion). A sheet keeps at most [`MAX_RULES`] rules and a
//! selector at most [`MAX_COMPOUNDS`] compounds; matching one rule against an
//! element costs O(compounds × depth) (see [`Selector::matches`]), all
//! charged to the import's [`Budget`].

use super::Budget;
use std::collections::HashMap;

/// The most rules a sheet keeps (a selector of a comma group is one rule);
/// more are ignored.
pub(crate) const MAX_RULES: usize = 10_000;
/// The most compounds a selector may have; a longer one is dropped alone.
pub(crate) const MAX_COMPOUNDS: usize = 32;
/// The longest tag, class, id or font name kept, in bytes.
pub(crate) const MAX_NAME: usize = 64;
/// The most classes an element keeps.
pub(crate) const MAX_CLASSES: usize = 32;

/// One `property: value [!important]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Decl {
    /// Lowercased.
    pub(crate) name: String,
    /// Trimmed, `!important` removed, case kept (font names keep theirs).
    pub(crate) value: String,
    pub(crate) important: bool,
}

/// The declarations of a `style` attribute or a rule body, in order.
/// Tolerant: a piece without `:` or with an odd name is skipped.
pub(crate) fn parse_declarations(text: &str) -> Vec<Decl> {
    let text = strip_comments(text);
    let mut out = Vec::new();
    for piece in split_top(&text, b';') {
        let Some((k, v)) = piece.split_once(':') else {
            continue;
        };
        let name = k.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            continue;
        }
        let mut value = v.trim();
        let mut important = false;
        if let Some(i) = value.rfind('!') {
            if value[i + 1..].trim().eq_ignore_ascii_case("important") {
                important = true;
                value = value[..i].trim_end();
            }
        }
        if value.is_empty() {
            continue;
        }
        out.push(Decl {
            name: name.to_ascii_lowercase(),
            value: value.to_string(),
            important,
        });
    }
    out
}

/// `text` without `/* … */` comments (an unterminated one runs to the end),
/// strings left alone.
fn strip_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut from = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        match quote {
            Some(q) => {
                if b[i] == q {
                    quote = None;
                }
                i += 1;
            }
            None if b[i] == b'"' || b[i] == b'\'' => {
                quote = Some(b[i]);
                i += 1;
            }
            None if b[i] == b'/' && b.get(i + 1) == Some(&b'*') => {
                out.push_str(&text[from..i]);
                out.push(' ');
                i = match super::find(&b[i + 2..], b"*/") {
                    Some(e) => i + 2 + e + 2,
                    None => b.len(),
                };
                from = i;
            }
            None => i += 1,
        }
    }
    out.push_str(&text[from.min(text.len())..]);
    out
}

/// `text` split at `sep` outside quotes and parentheses.
fn split_top(text: &str, sep: u8) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut start = 0;
    for (i, &c) in b.iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                _ if c == sep && depth == 0 => {
                    out.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    out.push(&text[start..]);
    out
}

/// A value split at white space outside quotes and parentheses, empty
/// pieces dropped.
fn tokens(value: &str) -> Vec<&str> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut start: Option<usize> = None;
    for (i, &c) in b.iter().enumerate() {
        let space = quote.is_none() && depth == 0 && c.is_ascii_whitespace();
        if space {
            if let Some(s) = start.take() {
                out.push(&value[s..i]);
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                _ => {}
            },
        }
    }
    if let Some(s) = start {
        out.push(&value[s..]);
    }
    out
}

// ---------------------------------------------------------------------------
// Selectors and style sheets

/// What a selector compound compares against: an element's tag, id and
/// classes, all lowercased.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ElemKey {
    pub(crate) tag: String,
    pub(crate) id: Option<String>,
    pub(crate) classes: Vec<String>,
}

impl ElemKey {
    /// The key of an element `tag` with attributes `id` and `class` (each
    /// as written): an id or class longer than [`MAX_NAME`] is not kept
    /// (it can never match), classes past [`MAX_CLASSES`] neither.
    pub(crate) fn new(tag: &str, id: Option<&str>, class: Option<&str>) -> Self {
        let id = id
            .map(str::trim)
            .filter(|i| !i.is_empty() && i.len() <= MAX_NAME)
            .map(str::to_ascii_lowercase);
        let mut classes: Vec<String> = Vec::new();
        for c in class.unwrap_or("").split_ascii_whitespace() {
            if classes.len() >= MAX_CLASSES {
                break;
            }
            if c.len() <= MAX_NAME {
                let c = c.to_ascii_lowercase();
                if !classes.contains(&c) {
                    classes.push(c);
                }
            }
        }
        ElemKey {
            tag: tag.to_ascii_lowercase(),
            id,
            classes,
        }
    }

    /// Heap bytes this key holds.
    pub(crate) fn heap(&self) -> usize {
        self.tag.len()
            + self.id.as_ref().map_or(0, String::len)
            + self.classes.iter().map(String::len).sum::<usize>()
    }
}

/// An element and its ancestors, outermost first; the last is the one a
/// selector's rightmost compound is tested on.
pub(crate) trait Path {
    fn len(&self) -> usize;
    fn key(&self, i: usize) -> &ElemKey;
}

impl Path for [ElemKey] {
    fn len(&self) -> usize {
        <[ElemKey]>::len(self)
    }
    fn key(&self, i: usize) -> &ElemKey {
        &self[i]
    }
}

/// One compound: a tag (`None` for `*` or none written), an id, classes.
#[derive(Debug, Clone, PartialEq)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Compound {
    fn matches(&self, e: &ElemKey) -> bool {
        self.tag.as_ref().is_none_or(|t| *t == e.tag)
            && self.id.as_ref().is_none_or(|i| e.id.as_ref() == Some(i))
            && self.classes.iter().all(|c| e.classes.contains(c))
    }

    fn heap(&self) -> usize {
        self.tag.as_ref().map_or(0, String::len)
            + self.id.as_ref().map_or(0, String::len)
            + self.classes.iter().map(String::len).sum::<usize>()
    }
}

/// How two compounds relate.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Comb {
    Descendant,
    Child,
}

/// A complex selector: compounds left to right, `combs[i]` between
/// `parts[i]` and `parts[i + 1]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Selector {
    parts: Vec<Compound>,
    combs: Vec<Comb>,
    /// (ids, classes, tags) packed so that a larger number is more
    /// specific.
    spec: u32,
}

impl Selector {
    /// A selector of the kinds kept, or `None` (dropped alone).
    pub(crate) fn parse(text: &str) -> Option<Selector> {
        let b = text.trim().as_bytes();
        let text = text.trim();
        if b.is_empty() {
            return None;
        }
        let mut parts: Vec<Compound> = Vec::new();
        let mut combs: Vec<Comb> = Vec::new();
        let mut cur: Option<Compound> = None;
        // A combinator seen since the last compound ended: none, space, `>`.
        let mut pending: Option<Comb> = None;
        let mut i = 0;
        let ident = |i: &mut usize| -> Option<String> {
            let start = *i;
            while *i < b.len()
                && (b[*i].is_ascii_alphanumeric()
                    || b[*i] == b'-'
                    || b[*i] == b'_'
                    || b[*i] >= 0x80)
            {
                *i += 1;
            }
            let name = &text[start..*i];
            (!name.is_empty() && name.len() <= MAX_NAME).then(|| name.to_ascii_lowercase())
        };
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_whitespace() {
                if let Some(done) = cur.take() {
                    parts.push(done);
                }
                if pending.is_none() && !parts.is_empty() {
                    pending = Some(Comb::Descendant);
                }
                i += 1;
                continue;
            }
            if c == b'>' {
                if let Some(done) = cur.take() {
                    parts.push(done);
                }
                if parts.is_empty() || pending == Some(Comb::Child) {
                    return None;
                }
                pending = Some(Comb::Child);
                i += 1;
                continue;
            }
            // Something that belongs to a compound: start one if needed.
            if cur.is_none() {
                if !parts.is_empty() {
                    combs.push(pending.take()?);
                }
                pending = None;
                let mut compound = Compound {
                    tag: None,
                    id: None,
                    classes: Vec::new(),
                };
                if c == b'*' {
                    i += 1;
                } else if c != b'.' && c != b'#' {
                    if !(c.is_ascii_alphabetic() || c == b'_' || c >= 0x80) {
                        return None;
                    }
                    compound.tag = Some(ident(&mut i)?);
                }
                cur = Some(compound);
                continue;
            }
            let compound = cur.as_mut()?;
            match c {
                b'.' => {
                    i += 1;
                    let name = ident(&mut i)?;
                    compound.classes.push(name);
                }
                b'#' => {
                    i += 1;
                    let name = ident(&mut i)?;
                    if compound.id.is_some() {
                        // `#a#b` only matches an element with both ids: never.
                        return None;
                    }
                    compound.id = Some(name);
                }
                // `+ ~ [ : \ ( ,` and anything else: not a kind we keep.
                _ => return None,
            }
            if parts.len() + 1 > MAX_COMPOUNDS {
                return None;
            }
        }
        if let Some(done) = cur.take() {
            parts.push(done);
        }
        if parts.is_empty() || pending == Some(Comb::Child) || parts.len() > MAX_COMPOUNDS {
            return None;
        }
        // A trailing space leaves a descendant pending with nothing after it.
        if combs.len() + 1 != parts.len() {
            return None;
        }
        let (mut ids, mut classes, mut tags) = (0u32, 0u32, 0u32);
        for p in &parts {
            ids += p.id.is_some() as u32;
            classes += p.classes.len() as u32;
            tags += p.tag.is_some() as u32;
        }
        let spec = (ids.min(1023) << 20) | (classes.min(1023) << 10) | tags.min(1023);
        Some(Selector { parts, combs, spec })
    }

    fn heap(&self) -> usize {
        self.parts.iter().map(Compound::heap).sum::<usize>()
            + self.parts.capacity() * std::mem::size_of::<Compound>()
            + self.combs.capacity()
    }

    /// Whether the last element of `path` matches, right to left.
    ///
    /// `reach` is the set of elements where the compounds from `j` to the
    /// right end can match with compound `j` on that element. For the
    /// rightmost compound it is the subject alone. Going left, a child
    /// combinator keeps the elements whose child is in the set, and a
    /// descendant combinator every element before the set's last one. Each
    /// step looks at each element once: O(compounds × depth), no
    /// backtracking. One unit of work per element looked at.
    pub(crate) fn matches<P: Path + ?Sized>(&self, path: &P, budget: &Budget) -> bool {
        let d = path.len();
        let k = self.parts.len();
        if d == 0 || k == 0 || !self.parts[k - 1].matches(path.key(d - 1)) {
            return false;
        }
        if k == 1 {
            return true;
        }
        let mut reach = vec![false; d];
        reach[d - 1] = true;
        // The last element in `reach`; every element of it is <= hi.
        let mut hi = d - 1;
        for j in (0..k - 1).rev() {
            if !budget.work(hi / 4 + 1) {
                return false;
            }
            let mut next = vec![false; d];
            let mut new_hi: Option<usize> = None;
            for e in 0..hi {
                let ok = match self.combs[j] {
                    Comb::Child => reach[e + 1],
                    Comb::Descendant => true,
                } && self.parts[j].matches(path.key(e));
                if ok {
                    next[e] = true;
                    new_hi = Some(e);
                }
            }
            match new_hi {
                Some(h) => hi = h,
                None => return false,
            }
            reach = next;
        }
        true
    }
}

#[derive(Debug)]
struct Rule {
    sel: Selector,
    order: u32,
    /// Index into [`StyleSheet::blocks`].
    block: u32,
}

/// A rule that matched an element: what [`StyleSheet::matching`] returns,
/// in cascade order (specificity, then source order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Hit {
    spec: u32,
    order: u32,
    block: u32,
}

/// The rules of every `<style>` block of a page.
#[derive(Debug, Default)]
pub(crate) struct StyleSheet {
    rules: Vec<Rule>,
    /// Declaration blocks, shared by a comma group's selectors.
    blocks: Vec<Vec<Decl>>,
    by_id: HashMap<String, Vec<u32>>,
    by_class: HashMap<String, Vec<u32>>,
    by_tag: HashMap<String, Vec<u32>>,
    universal: Vec<u32>,
}

impl StyleSheet {
    /// Parse `css`, charging `budget`: the text scanned once, every rule and
    /// declaration block kept. At most [`MAX_RULES`] rules are kept.
    pub(crate) fn parse(css: &str, budget: &Budget) -> StyleSheet {
        let mut sheet = StyleSheet::default();
        if css.is_empty() || !budget.scanned(css.len()) {
            return sheet;
        }
        let css = strip_comments(css);
        let b = css.as_bytes();
        let mut i = 0;
        while i < b.len() && sheet.rules.len() < MAX_RULES && !budget.exhausted() {
            let c = b[i];
            if c.is_ascii_whitespace() || c == b'}' || c == b';' {
                i += 1;
                continue;
            }
            if b[i..].starts_with(b"<!--") {
                i += 4;
                continue;
            }
            if b[i..].starts_with(b"-->") {
                i += 3;
                continue;
            }
            if c == b'@' {
                i = skip_at_rule(b, i);
                continue;
            }
            let Some(open) = find_unquoted(b, i, b'{') else {
                break;
            };
            let close = block_end(b, open);
            let prelude = &css[i..open];
            let body = &css[open + 1..close.min(b.len())];
            i = close + 1;
            let decls = parse_declarations(body);
            if decls.is_empty() {
                continue;
            }
            let heap: usize = decls
                .iter()
                .map(|d| d.name.len() + d.value.len() + std::mem::size_of::<Decl>())
                .sum();
            let block = sheet.blocks.len() as u32;
            let mut kept_any = false;
            for sel in split_top(prelude, b',') {
                if sheet.rules.len() >= MAX_RULES {
                    break;
                }
                let Some(sel) = Selector::parse(sel) else {
                    continue;
                };
                if !budget.keep::<Rule>(sel.heap()) {
                    return sheet;
                }
                let r = sheet.rules.len() as u32;
                let last = &sel.parts[sel.parts.len() - 1];
                let bucket = if let Some(id) = &last.id {
                    sheet.by_id.entry(id.clone()).or_default()
                } else if let Some(class) = last.classes.first() {
                    sheet.by_class.entry(class.clone()).or_default()
                } else if let Some(tag) = &last.tag {
                    sheet.by_tag.entry(tag.clone()).or_default()
                } else {
                    &mut sheet.universal
                };
                bucket.push(r);
                sheet.rules.push(Rule {
                    sel,
                    order: r,
                    block,
                });
                kept_any = true;
            }
            if kept_any {
                if !budget.keep::<Vec<Decl>>(heap) {
                    return sheet;
                }
                sheet.blocks.push(decls);
            }
        }
        sheet
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rules.len()
    }

    /// The rules matching the last element of `path`, in cascade order.
    /// Only rules whose rightmost compound names the element's id, one of
    /// its classes or its tag, or names none of them, are examined; one
    /// unit of work each, plus what matching costs.
    pub(crate) fn matching<P: Path + ?Sized>(&self, path: &P, budget: &Budget) -> Vec<Hit> {
        let mut hits = Vec::new();
        let d = path.len();
        if self.rules.is_empty() || d == 0 {
            return hits;
        }
        let subject = path.key(d - 1);
        let mut check = |list: Option<&Vec<u32>>| {
            for &r in list.into_iter().flatten() {
                if !budget.op() {
                    return;
                }
                let rule = &self.rules[r as usize];
                if rule.sel.matches(path, budget) {
                    hits.push(Hit {
                        spec: rule.sel.spec,
                        order: rule.order,
                        block: rule.block,
                    });
                }
            }
        };
        if let Some(id) = &subject.id {
            check(self.by_id.get(id));
        }
        for c in &subject.classes {
            check(self.by_class.get(c));
        }
        check(self.by_tag.get(&subject.tag));
        check(Some(&self.universal));
        hits.sort_unstable();
        hits
    }

    /// The declarations of a hit.
    pub(crate) fn decls(&self, hit: Hit) -> &[Decl] {
        &self.blocks[hit.block as usize]
    }
}

/// Where the at-rule starting at `b[at] == '@'` ends: after its `;`, or after
/// its block's closing brace.
fn skip_at_rule(b: &[u8], at: usize) -> usize {
    let mut quote: Option<u8> = None;
    let mut i = at + 1;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b';' => return i + 1,
                b'{' => return block_end(b, i) + 1,
                _ => {}
            },
        }
        i += 1;
    }
    b.len()
}

/// The first `want` at or after `from` outside quotes.
fn find_unquoted(b: &[u8], from: usize, want: u8) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, &c) in b.iter().enumerate().skip(from) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == want => return Some(i),
            None => {}
        }
    }
    None
}

/// The brace closing the block opened at `b[open] == '{'`, or `b.len()`.
/// Nested braces are counted, not recursed into.
fn block_end(b: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            },
        }
    }
    b.len()
}

// ---------------------------------------------------------------------------
// Values

/// What a `color` value does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Color {
    /// `RRGGBB`, uppercase.
    Rgb(String),
    /// `windowtext`, `auto`: the default colour again.
    Reset,
    /// `inherit`, `currentcolor`: the parent's, which is already there.
    Keep,
}

/// A `color` value; `None` for one that does nothing (`transparent`,
/// unparsable).
pub(crate) fn color(value: &str) -> Option<Color> {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    match lower.as_str() {
        "windowtext" | "auto" => return Some(Color::Reset),
        "inherit" | "currentcolor" => return Some(Color::Keep),
        _ => {}
    }
    rgb(&lower).map(Color::Rgb)
}

/// A colour as `RRGGBB`: `#rgb`, `#rrggbb`, `rgb()`/`rgba()` (integers or
/// percentages; fully transparent is none), a named colour.
fn rgb(lower: &str) -> Option<String> {
    if let Some(hex) = lower.strip_prefix('#') {
        return hex_rgb(hex);
    }
    if let Some(args) = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))
    {
        let args = args.strip_suffix(')')?;
        let parts: Vec<&str> = args
            .split([',', ' ', '/'])
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() != 3 && parts.len() != 4 {
            return None;
        }
        if let Some(a) = parts.get(3) {
            let alpha = match a.strip_suffix('%') {
                Some(p) => p.parse::<f64>().ok()? / 100.0,
                None => a.parse::<f64>().ok()?,
            };
            if alpha <= 0.0 {
                return None;
            }
        }
        let mut out = String::with_capacity(6);
        for p in &parts[..3] {
            let n = match p.strip_suffix('%') {
                Some(pc) => pc.parse::<f64>().ok()? * 255.0 / 100.0,
                None => p.parse::<f64>().ok()?,
            };
            if !n.is_finite() {
                return None;
            }
            out.push_str(&format!("{:02X}", n.round().clamp(0.0, 255.0) as u8));
        }
        return Some(out);
    }
    named_color(lower).map(str::to_string)
}

fn hex_rgb(hex: &str) -> Option<String> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => Some(
            hex.chars()
                .flat_map(|c| [c, c])
                .collect::<String>()
                .to_ascii_uppercase(),
        ),
        6 => Some(hex.to_ascii_uppercase()),
        _ => None,
    }
}

/// A presentational `color=` / `bgcolor=` attribute: a named colour, `#hex`,
/// or six hex digits without `#`, as browsers read it.
pub(crate) fn attr_color(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    rgb(&lower).or_else(|| (lower.len() == 6).then(|| hex_rgb(&lower)).flatten())
}

/// The colour of a `background` / `background-color` value: its first
/// token that is a colour (`none`, `transparent`, images: none).
pub(crate) fn background(value: &str) -> Option<String> {
    tokens(value)
        .into_iter()
        .find_map(|t| rgb(&t.to_ascii_lowercase()))
}

/// The 17 values of ST_HighlightColor, in Word's case.
const HIGHLIGHTS: [&str; 17] = [
    "black",
    "blue",
    "cyan",
    "green",
    "magenta",
    "red",
    "yellow",
    "white",
    "darkBlue",
    "darkCyan",
    "darkGreen",
    "darkMagenta",
    "darkRed",
    "darkYellow",
    "darkGray",
    "lightGray",
    "none",
];

/// An `mso-highlight` value as Word's highlight name; anything else is
/// none (a `w:highlight` outside the list is a file Word refuses).
pub(crate) fn highlight(value: &str) -> Option<&'static str> {
    let v = value.trim();
    HIGHLIGHTS
        .iter()
        .copied()
        .find(|h| h.eq_ignore_ascii_case(v))
}

/// A length's unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Unit {
    Pt,
    Px,
    In,
    Cm,
    Mm,
    Pc,
    Em,
    Ex,
    Rem,
    Percent,
}

/// A CSS length or percentage, kept until the element's font size is
/// known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Len {
    pub(crate) v: f64,
    pub(crate) unit: Unit,
}

/// The size `rem` (and the font-size base) is counted from, in points.
pub(crate) const BASE_PT: f64 = 12.0;

impl Len {
    /// In points, against a font of `font_pt`; `None` for a percentage.
    pub(crate) fn points(self, font_pt: f64) -> Option<f64> {
        let k = match self.unit {
            Unit::Pt => 1.0,
            Unit::Px => 0.75,
            Unit::In => 72.0,
            Unit::Cm => 72.0 / 2.54,
            Unit::Mm => 72.0 / 25.4,
            Unit::Pc => 12.0,
            Unit::Em => font_pt,
            Unit::Ex => font_pt / 2.0,
            Unit::Rem => BASE_PT,
            Unit::Percent => return None,
        };
        Some(self.v * k)
    }

    /// In twips (rounded), against a font of `font_pt`.
    pub(crate) fn twips(self, font_pt: f64) -> Option<i32> {
        self.points(font_pt)
            .map(|pt| (pt * 20.0).round().clamp(-31_680.0, 31_680.0) as i32)
    }
}

/// A leading number of `s` (`12`, `-.25`, `+1.5`) and what follows it.
fn number(s: &str) -> Option<(f64, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == digits || (i == digits + 1 && b[digits] == b'.') {
        return None;
    }
    let n: f64 = s[..i].parse().ok()?;
    n.is_finite().then_some((n, &s[i..]))
}

/// A length or percentage; a bare `0` is a length, other bare numbers are
/// not.
pub(crate) fn length(value: &str) -> Option<Len> {
    let v = value.trim().to_ascii_lowercase();
    let (n, unit) = number(&v)?;
    let unit = match unit {
        "pt" => Unit::Pt,
        "px" => Unit::Px,
        "in" => Unit::In,
        "cm" => Unit::Cm,
        "mm" => Unit::Mm,
        "pc" => Unit::Pc,
        "em" => Unit::Em,
        "ex" => Unit::Ex,
        "rem" => Unit::Rem,
        "%" => Unit::Percent,
        "" if n == 0.0 => Unit::Pt,
        _ => return None,
    };
    Some(Len { v: n, unit })
}

/// The largest and smallest font size a run takes, in points.
const MAX_FONT_PT: f64 = 1638.0;
const MIN_FONT_PT: f64 = 1.0;

/// Points as half-points, rounded to the nearest half point and clamped to
/// 1..1638 pt.
pub(crate) fn half_points(pt: f64) -> u32 {
    (pt.clamp(MIN_FONT_PT, MAX_FONT_PT) * 2.0).round() as u32
}

/// A `font-size` value in points, against a parent of `parent_pt`.
pub(crate) fn font_size_pt(value: &str, parent_pt: f64) -> Option<f64> {
    let v = value.trim().to_ascii_lowercase();
    let pt = match v.as_str() {
        "xx-small" => 7.0,
        "x-small" => 7.5,
        "small" => 10.0,
        "medium" => 12.0,
        "large" => 13.5,
        "x-large" => 18.0,
        "xx-large" => 24.0,
        "xxx-large" => 36.0,
        "smaller" => parent_pt / 1.2,
        "larger" => parent_pt * 1.2,
        _ => {
            let len = length(&v)?;
            if len.v < 0.0 {
                return None;
            }
            match len.unit {
                Unit::Percent => parent_pt * len.v / 100.0,
                // `em` of a font size is the parent's.
                _ => len.points(parent_pt)?,
            }
        }
    };
    (pt.is_finite() && pt > 0.0).then_some(pt)
}

/// The sizes of `<font size=1..7>`, in points.
const FONT_ATTR_PT: [f64; 7] = [7.5, 10.0, 12.0, 13.5, 18.0, 24.0, 36.0];

/// A `<font size>` attribute (`1`..`7`, or `+n` / `-n` from 3) in points.
pub(crate) fn font_attr_size(value: &str) -> Option<f64> {
    let v = value.trim();
    let n: i64 = v.strip_prefix('+').unwrap_or(v).parse().ok()?;
    let level = if v.starts_with('+') || v.starts_with('-') {
        3 + n
    } else {
        n
    };
    Some(FONT_ATTR_PT[(level.clamp(1, 7) - 1) as usize])
}

/// Generic and system families: never a family to write.
const GENERIC: [&str; 15] = [
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "math",
    "emoji",
    "fangsong",
    "inherit",
    "initial",
];

/// The family a `font-family` list names first that is not generic or a
/// system alias (`-apple-system`, `BlinkMacSystemFont`); with none, a
/// generic `serif`, `sans-serif` or `monospace` as a common font. Names over
/// [`MAX_NAME`] bytes or holding control characters are skipped.
pub(crate) fn font_family(value: &str) -> Option<String> {
    let mut generic: Option<&'static str> = None;
    for piece in split_top(value, b',') {
        let p = piece.trim();
        let quoted = p.len() >= 2
            && ((p.starts_with('"') && p.ends_with('"'))
                || (p.starts_with('\'') && p.ends_with('\'')));
        let name = if quoted { &p[1..p.len() - 1] } else { p };
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if !quoted && GENERIC.contains(&lower.as_str()) {
            if generic.is_none() {
                generic = match lower.as_str() {
                    "serif" => Some("Times New Roman"),
                    "sans-serif" => Some("Arial"),
                    "monospace" => Some("Courier New"),
                    _ => None,
                };
            }
            continue;
        }
        if lower.starts_with('-') || lower == "blinkmacsystemfont" {
            continue;
        }
        return Some(name);
    }
    generic.map(str::to_string)
}

/// A `font-weight` value: `Some(true)` bold, `Some(false)` not.
pub(crate) fn weight(value: &str) -> Option<bool> {
    let v = value.trim().to_ascii_lowercase();
    match v.as_str() {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        _ => v.parse::<u32>().ok().map(|w| w >= 600),
    }
}

/// A `font-style` value: `Some(true)` italic.
pub(crate) fn italic(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "italic" | "oblique" => Some(true),
        "normal" => Some(false),
        v if v.starts_with("oblique") => Some(true),
        _ => None,
    }
}

/// A `line-height` value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LineHeight {
    Normal,
    /// A multiple of single spacing.
    Multiple(f64),
    Length(Len),
}

pub(crate) fn line_height(value: &str) -> Option<LineHeight> {
    let v = value.trim().to_ascii_lowercase();
    if v == "normal" {
        return Some(LineHeight::Normal);
    }
    if let Some((n, "")) = number(&v) {
        return (n >= 0.0).then_some(LineHeight::Multiple(n));
    }
    let len = length(&v)?;
    if len.v < 0.0 {
        return None;
    }
    Some(match len.unit {
        Unit::Percent => LineHeight::Multiple(len.v / 100.0),
        _ => LineHeight::Length(len),
    })
}

/// What the `font` shorthand sets. Style and weight not written are
/// normal, line height not written is `normal` (CSS resets them).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FontShorthand {
    pub(crate) italic: bool,
    pub(crate) bold: bool,
    pub(crate) size_pt: f64,
    pub(crate) line: LineHeight,
    pub(crate) family: Option<String>,
}

/// The `font` shorthand: `[style] [variant] [weight] size[/line] family`.
/// `None` without a size (or for a system font keyword).
pub(crate) fn font_shorthand(value: &str, parent_pt: f64) -> Option<FontShorthand> {
    let toks = tokens(value);
    let mut italic = false;
    let mut bold = false;
    for (i, t) in toks.iter().enumerate() {
        let (size_tok, line_tok) = match t.split_once('/') {
            Some((s, l)) => (s, Some(l)),
            None => (*t, None),
        };
        if let Some(size_pt) = font_size_pt(size_tok, parent_pt) {
            // A bare number is a weight, never a size (`font_size_pt` reads
            // none), so the first size-shaped token is the size.
            let mut rest = i + 1;
            let line = match line_tok {
                Some("") => {
                    // `12px/ 1.5`: the line height is the next token.
                    rest += 1;
                    toks.get(i + 1).and_then(|l| line_height(l))?
                }
                Some(l) => line_height(l)?,
                None => match toks.get(i + 1) {
                    Some(l) if l.starts_with('/') => {
                        rest += 1;
                        let l = &l[1..];
                        if l.is_empty() {
                            rest += 1;
                            toks.get(i + 2).and_then(|l| line_height(l))?
                        } else {
                            line_height(l)?
                        }
                    }
                    _ => LineHeight::Normal,
                },
            };
            let family = toks
                .get(rest..)
                .map(|r| r.join(" "))
                .and_then(|f| font_family(&f));
            return Some(FontShorthand {
                italic,
                bold,
                size_pt,
                line,
                family,
            });
        }
        let lower = t.to_ascii_lowercase();
        if let Some(it) = italic_kw(&lower) {
            italic = it;
        } else if let Some(w) = weight(&lower) {
            bold = w;
        } else if !matches!(
            lower.as_str(),
            "normal" | "small-caps" | "condensed" | "expanded"
        ) && !lower.ends_with("-condensed")
            && !lower.ends_with("-expanded")
        {
            return None;
        }
    }
    None
}

fn italic_kw(lower: &str) -> Option<bool> {
    match lower {
        "italic" | "oblique" => Some(true),
        _ => None,
    }
}

/// A box shorthand (`margin`, `padding`): one to four values as top, right,
/// bottom, left. A side that is `auto` or a percentage is `None` (not
/// read); a value that is neither nor a length makes the whole declaration
/// invalid (`None`).
pub(crate) fn box_sides(value: &str) -> Option<[Option<Len>; 4]> {
    let toks = tokens(value);
    let mut vals: Vec<Option<Len>> = Vec::with_capacity(4);
    for t in &toks {
        if t.eq_ignore_ascii_case("auto") {
            vals.push(None);
            continue;
        }
        let len = length(t)?;
        vals.push((len.unit != Unit::Percent).then_some(len));
    }
    Some(match vals.as_slice() {
        [a] => [*a, *a, *a, *a],
        [a, b] => [*a, *b, *a, *b],
        [a, b, c] => [*a, *b, *c, *b],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => return None,
    })
}

/// One side's value (`margin-left: …`): a length, else `None` for `auto` or
/// a percentage; `Err` when invalid.
pub(crate) fn side(value: &str) -> Result<Option<Len>, ()> {
    if value.trim().eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let len = length(value).ok_or(())?;
    Ok((len.unit != Unit::Percent).then_some(len))
}

/// The CSS named colours, sorted by name.
const NAMED: [(&str, &str); 148] = [
    ("aliceblue", "F0F8FF"),
    ("antiquewhite", "FAEBD7"),
    ("aqua", "00FFFF"),
    ("aquamarine", "7FFFD4"),
    ("azure", "F0FFFF"),
    ("beige", "F5F5DC"),
    ("bisque", "FFE4C4"),
    ("black", "000000"),
    ("blanchedalmond", "FFEBCD"),
    ("blue", "0000FF"),
    ("blueviolet", "8A2BE2"),
    ("brown", "A52A2A"),
    ("burlywood", "DEB887"),
    ("cadetblue", "5F9EA0"),
    ("chartreuse", "7FFF00"),
    ("chocolate", "D2691E"),
    ("coral", "FF7F50"),
    ("cornflowerblue", "6495ED"),
    ("cornsilk", "FFF8DC"),
    ("crimson", "DC143C"),
    ("cyan", "00FFFF"),
    ("darkblue", "00008B"),
    ("darkcyan", "008B8B"),
    ("darkgoldenrod", "B8860B"),
    ("darkgray", "A9A9A9"),
    ("darkgreen", "006400"),
    ("darkgrey", "A9A9A9"),
    ("darkkhaki", "BDB76B"),
    ("darkmagenta", "8B008B"),
    ("darkolivegreen", "556B2F"),
    ("darkorange", "FF8C00"),
    ("darkorchid", "9932CC"),
    ("darkred", "8B0000"),
    ("darksalmon", "E9967A"),
    ("darkseagreen", "8FBC8F"),
    ("darkslateblue", "483D8B"),
    ("darkslategray", "2F4F4F"),
    ("darkslategrey", "2F4F4F"),
    ("darkturquoise", "00CED1"),
    ("darkviolet", "9400D3"),
    ("deeppink", "FF1493"),
    ("deepskyblue", "00BFFF"),
    ("dimgray", "696969"),
    ("dimgrey", "696969"),
    ("dodgerblue", "1E90FF"),
    ("firebrick", "B22222"),
    ("floralwhite", "FFFAF0"),
    ("forestgreen", "228B22"),
    ("fuchsia", "FF00FF"),
    ("gainsboro", "DCDCDC"),
    ("ghostwhite", "F8F8FF"),
    ("gold", "FFD700"),
    ("goldenrod", "DAA520"),
    ("gray", "808080"),
    ("green", "008000"),
    ("greenyellow", "ADFF2F"),
    ("grey", "808080"),
    ("honeydew", "F0FFF0"),
    ("hotpink", "FF69B4"),
    ("indianred", "CD5C5C"),
    ("indigo", "4B0082"),
    ("ivory", "FFFFF0"),
    ("khaki", "F0E68C"),
    ("lavender", "E6E6FA"),
    ("lavenderblush", "FFF0F5"),
    ("lawngreen", "7CFC00"),
    ("lemonchiffon", "FFFACD"),
    ("lightblue", "ADD8E6"),
    ("lightcoral", "F08080"),
    ("lightcyan", "E0FFFF"),
    ("lightgoldenrodyellow", "FAFAD2"),
    ("lightgray", "D3D3D3"),
    ("lightgreen", "90EE90"),
    ("lightgrey", "D3D3D3"),
    ("lightpink", "FFB6C1"),
    ("lightsalmon", "FFA07A"),
    ("lightseagreen", "20B2AA"),
    ("lightskyblue", "87CEFA"),
    ("lightslategray", "778899"),
    ("lightslategrey", "778899"),
    ("lightsteelblue", "B0C4DE"),
    ("lightyellow", "FFFFE0"),
    ("lime", "00FF00"),
    ("limegreen", "32CD32"),
    ("linen", "FAF0E6"),
    ("magenta", "FF00FF"),
    ("maroon", "800000"),
    ("mediumaquamarine", "66CDAA"),
    ("mediumblue", "0000CD"),
    ("mediumorchid", "BA55D3"),
    ("mediumpurple", "9370DB"),
    ("mediumseagreen", "3CB371"),
    ("mediumslateblue", "7B68EE"),
    ("mediumspringgreen", "00FA9A"),
    ("mediumturquoise", "48D1CC"),
    ("mediumvioletred", "C71585"),
    ("midnightblue", "191970"),
    ("mintcream", "F5FFFA"),
    ("mistyrose", "FFE4E1"),
    ("moccasin", "FFE4B5"),
    ("navajowhite", "FFDEAD"),
    ("navy", "000080"),
    ("oldlace", "FDF5E6"),
    ("olive", "808000"),
    ("olivedrab", "6B8E23"),
    ("orange", "FFA500"),
    ("orangered", "FF4500"),
    ("orchid", "DA70D6"),
    ("palegoldenrod", "EEE8AA"),
    ("palegreen", "98FB98"),
    ("paleturquoise", "AFEEEE"),
    ("palevioletred", "DB7093"),
    ("papayawhip", "FFEFD5"),
    ("peachpuff", "FFDAB9"),
    ("peru", "CD853F"),
    ("pink", "FFC0CB"),
    ("plum", "DDA0DD"),
    ("powderblue", "B0E0E6"),
    ("purple", "800080"),
    ("rebeccapurple", "663399"),
    ("red", "FF0000"),
    ("rosybrown", "BC8F8F"),
    ("royalblue", "4169E1"),
    ("saddlebrown", "8B4513"),
    ("salmon", "FA8072"),
    ("sandybrown", "F4A460"),
    ("seagreen", "2E8B57"),
    ("seashell", "FFF5EE"),
    ("sienna", "A0522D"),
    ("silver", "C0C0C0"),
    ("skyblue", "87CEEB"),
    ("slateblue", "6A5ACD"),
    ("slategray", "708090"),
    ("slategrey", "708090"),
    ("snow", "FFFAFA"),
    ("springgreen", "00FF7F"),
    ("steelblue", "4682B4"),
    ("tan", "D2B48C"),
    ("teal", "008080"),
    ("thistle", "D8BFD8"),
    ("tomato", "FF6347"),
    ("turquoise", "40E0D0"),
    ("violet", "EE82EE"),
    ("wheat", "F5DEB3"),
    ("white", "FFFFFF"),
    ("whitesmoke", "F5F5F5"),
    ("yellow", "FFFF00"),
    ("yellowgreen", "9ACD32"),
];

/// A CSS named colour.
fn named_color(lower: &str) -> Option<&'static str> {
    NAMED
        .binary_search_by(|(n, _)| (*n).cmp(lower))
        .ok()
        .map(|i| NAMED[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget::standard()
    }

    fn key(tag: &str, id: Option<&str>, class: Option<&str>) -> ElemKey {
        ElemKey::new(tag, id, class)
    }

    #[test]
    fn declarations_tolerate_junk() {
        let d = parse_declarations(
            "color: Red ; ;junk; font-family:'a;b', serif !important;/* c:d */ x y:z;\
             background:url(data:x;base64,AA) #fff; margin : 1px",
        );
        let got: Vec<(&str, &str, bool)> = d
            .iter()
            .map(|d| (d.name.as_str(), d.value.as_str(), d.important))
            .collect();
        assert_eq!(
            got,
            [
                ("color", "Red", false),
                ("font-family", "'a;b', serif", true),
                ("background", "url(data:x;base64,AA) #fff", false),
                ("margin", "1px", false),
            ]
        );
        assert!(parse_declarations("").is_empty());
        assert!(parse_declarations(":::;;;!important").is_empty());
    }

    #[test]
    fn every_colour_form() {
        let c = |v| color(v);
        assert_eq!(c("#f0a"), Some(Color::Rgb("FF00AA".into())));
        assert_eq!(c("#1f497d"), Some(Color::Rgb("1F497D".into())));
        assert_eq!(c("rgb(204, 204, 204)"), Some(Color::Rgb("CCCCCC".into())));
        assert_eq!(c("rgb(100%,0%,50%)"), Some(Color::Rgb("FF0080".into())));
        assert_eq!(c("RGBA(0,0,255,0.5)"), Some(Color::Rgb("0000FF".into())));
        assert_eq!(c("rgb(0 128 0 / 50%)"), Some(Color::Rgb("008000".into())));
        assert_eq!(c("rgba(0,0,0,0)"), None);
        assert_eq!(c("rgb(300,-4,0)"), Some(Color::Rgb("FF0000".into())));
        assert_eq!(c("Navy"), Some(Color::Rgb("000080".into())));
        assert_eq!(c("rebeccapurple"), Some(Color::Rgb("663399".into())));
        assert_eq!(c("windowtext"), Some(Color::Reset));
        assert_eq!(c("auto"), Some(Color::Reset));
        assert_eq!(c("inherit"), Some(Color::Keep));
        assert_eq!(c("currentColor"), Some(Color::Keep));
        for none in [
            "transparent",
            "#12",
            "#gggggg",
            "rgb(1,2)",
            "notacolour",
            "",
        ] {
            assert_eq!(c(none), None, "{none}");
        }
        assert_eq!(attr_color("ff0000").as_deref(), Some("FF0000"));
        assert_eq!(attr_color("#00f").as_deref(), Some("0000FF"));
        assert_eq!(attr_color("green").as_deref(), Some("008000"));
        assert_eq!(
            background("url(x.png) no-repeat #EEE").as_deref(),
            Some("EEEEEE")
        );
        assert_eq!(background("rgb(1, 2, 3) none").as_deref(), Some("010203"));
        assert_eq!(background("none"), None);
        assert_eq!(background("transparent"), None);
    }

    #[test]
    fn the_named_colour_table_is_sorted_and_complete() {
        // Binary search needs the table sorted; spot-check both ends.
        assert_eq!(named_color("aliceblue"), Some("F0F8FF"));
        assert_eq!(named_color("yellowgreen"), Some("9ACD32"));
        assert_eq!(named_color("grey"), Some("808080"));
        assert_eq!(named_color("lightgoldenrodyellow"), Some("FAFAD2"));
        assert!(NAMED.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn highlight_names_are_word_s_only() {
        assert_eq!(highlight("Yellow"), Some("yellow"));
        assert_eq!(highlight("darkblue"), Some("darkBlue"));
        assert_eq!(highlight("LIGHTGRAY"), Some("lightGray"));
        assert_eq!(highlight("fuchsia"), None);
        assert_eq!(highlight("#ffff00"), None);
    }

    #[test]
    fn every_length_unit_to_twips() {
        let tw = |v: &str| length(v).and_then(|l| l.twips(12.0));
        assert_eq!(tw("10px"), Some(150));
        assert_eq!(tw("8.0pt"), Some(160));
        assert_eq!(tw(".5in"), Some(720));
        assert_eq!(tw("-.25in"), Some(-360));
        assert_eq!(tw("1cm"), Some(567));
        assert_eq!(tw("10mm"), Some(567));
        assert_eq!(tw("1pc"), Some(240));
        assert_eq!(tw("2em"), Some(480));
        assert_eq!(tw("1ex"), Some(120));
        assert_eq!(tw("1rem"), Some(240));
        assert_eq!(tw("0"), Some(0));
        assert_eq!(tw("50%"), None);
        assert_eq!(tw("12"), None);
        assert_eq!(tw("px"), None);
        assert_eq!(tw("1.2.3pt"), None);
    }

    #[test]
    fn font_sizes_to_half_points() {
        let hp = |v: &str, parent: f64| font_size_pt(v, parent).map(half_points);
        assert_eq!(hp("11.0pt", 12.0), Some(22));
        assert_eq!(hp("16px", 12.0), Some(24));
        assert_eq!(hp("15px", 12.0), Some(23)); // 11.25 pt → 11.5
        assert_eq!(hp("2em", 10.0), Some(40));
        assert_eq!(hp("150%", 12.0), Some(36));
        assert_eq!(hp("1rem", 30.0), Some(24));
        assert_eq!(hp("x-small", 12.0), Some(15));
        assert_eq!(hp("larger", 10.0), Some(24));
        assert_eq!(hp("smaller", 12.0), Some(20));
        assert_eq!(hp("5000pt", 12.0), Some(3276));
        assert_eq!(hp("0.1pt", 12.0), Some(2));
        assert_eq!(hp("-3pt", 12.0), None);
        assert_eq!(hp("big", 12.0), None);
        let attr = |v: &str| font_attr_size(v);
        assert_eq!(attr("1"), Some(7.5));
        assert_eq!(attr("3"), Some(12.0));
        assert_eq!(attr("7"), Some(36.0));
        assert_eq!(attr("+1"), Some(13.5));
        assert_eq!(attr("-2"), Some(7.5));
        assert_eq!(attr("+9"), Some(36.0));
        assert_eq!(attr("x"), None);
    }

    #[test]
    fn font_family_picks_a_real_font() {
        assert_eq!(
            font_family("\"Calibri\",sans-serif").as_deref(),
            Some("Calibri")
        );
        assert_eq!(
            font_family("-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto").as_deref(),
            Some("Segoe UI")
        );
        assert_eq!(
            font_family("Times   New Roman, serif").as_deref(),
            Some("Times New Roman")
        );
        assert_eq!(font_family("sans-serif").as_deref(), Some("Arial"));
        assert_eq!(
            font_family("monospace, serif").as_deref(),
            Some("Courier New")
        );
        assert_eq!(font_family("cursive"), None);
        // A quoted generic name is a family of that name.
        assert_eq!(font_family("'serif'").as_deref(), Some("serif"));
        let long = "x".repeat(65);
        assert_eq!(
            font_family(&format!("{long}, Arial")).as_deref(),
            Some("Arial")
        );
    }

    #[test]
    fn font_shorthand_and_box_sides() {
        let f = font_shorthand("7.0pt \"Times New Roman\"", 12.0).unwrap();
        assert_eq!((f.italic, f.bold, f.size_pt), (false, false, 7.0));
        assert_eq!(f.family.as_deref(), Some("Times New Roman"));
        assert_eq!(f.line, LineHeight::Normal);
        let f = font_shorthand("italic bold 12px/1.5 Arial, sans-serif", 12.0).unwrap();
        assert_eq!((f.italic, f.bold, f.size_pt), (true, true, 9.0));
        assert_eq!(f.line, LineHeight::Multiple(1.5));
        assert_eq!(f.family.as_deref(), Some("Arial"));
        let f = font_shorthand("700 10pt / 14pt Georgia", 12.0).unwrap();
        assert!(f.bold);
        assert_eq!(
            f.line,
            LineHeight::Length(Len {
                v: 14.0,
                unit: Unit::Pt
            })
        );
        assert_eq!(font_shorthand("caption", 12.0), None);
        assert_eq!(font_shorthand("bold Arial", 12.0), None);

        let tw = |s: [Option<Len>; 4]| s.map(|l| l.and_then(|l| l.twips(12.0)));
        assert_eq!(tw(box_sides("1pt").unwrap()), [Some(20); 4]);
        assert_eq!(
            tw(box_sides("1pt 2pt").unwrap()),
            [Some(20), Some(40), Some(20), Some(40)]
        );
        assert_eq!(
            tw(box_sides("0px 0px 0px 0.8ex").unwrap()),
            [Some(0), Some(0), Some(0), Some(96)]
        );
        assert_eq!(
            tw(box_sides("0 auto 10% 3pt").unwrap()),
            [Some(0), None, None, Some(60)]
        );
        assert_eq!(box_sides("1px 2px 3px 4px 5px"), None);
        assert_eq!(box_sides("red"), None);
        assert_eq!(box_sides(""), None);
        assert_eq!(side("auto"), Ok(None));
        assert_eq!(side("x"), Err(()));
    }

    #[test]
    fn line_heights() {
        assert_eq!(line_height("115%"), Some(LineHeight::Multiple(1.15)));
        assert_eq!(line_height("1.5"), Some(LineHeight::Multiple(1.5)));
        assert_eq!(line_height("normal"), Some(LineHeight::Normal));
        assert!(matches!(line_height("18px"), Some(LineHeight::Length(_))));
        assert_eq!(line_height("-1"), None);
        assert_eq!(line_height("tall"), None);
    }

    #[test]
    fn selectors_kept_and_dropped() {
        for kept in [
            "p",
            "*",
            ".MsoNormal",
            "#main",
            "p.MsoNormal",
            "td.x.y#z",
            "div p",
            "div > p",
            "div>p",
            "body  table   td .x",
            "*.a",
        ] {
            assert!(Selector::parse(kept).is_some(), "{kept}");
        }
        for dropped in [
            "a:link",
            "a::before",
            "h1 + p",
            "h1 ~ p",
            "input[type=text]",
            "o\\:p",
            "> p",
            "p >",
            "p > > a",
            "",
            "#a#b",
            ".",
            "p:not(.x)",
            "1p",
        ] {
            assert!(Selector::parse(dropped).is_none(), "{dropped}");
        }
        let too_many = vec!["a"; MAX_COMPOUNDS + 1].join(" ");
        assert!(Selector::parse(&too_many).is_none());
        let just = vec!["a"; MAX_COMPOUNDS].join(" ");
        assert!(Selector::parse(&just).is_some());
    }

    #[test]
    fn specificity_orders_ids_classes_tags() {
        let s = |t: &str| Selector::parse(t).unwrap().spec;
        assert!(s("#a") > s(".a.b.c.d"));
        assert!(s(".a") > s("div p span"));
        assert!(s("p.a") > s(".a"));
        assert!(s("p") > s("*"));
    }

    #[test]
    fn matching_descendant_and_child() {
        let b = budget();
        let path = [
            key("body", None, None),
            key("div", Some("Main"), Some("box wide")),
            key("table", None, None),
            key("td", None, Some("cell")),
            key("p", None, Some("MsoNormal")),
        ];
        let m = |sel: &str| Selector::parse(sel).unwrap().matches(&path[..], &b);
        assert!(m("p"));
        assert!(m(".msonormal"));
        assert!(m("P.MSONORMAL"));
        assert!(m("div p"));
        assert!(m("#main p"));
        assert!(m("div.box.wide td > p"));
        assert!(m("td > p"));
        assert!(m("body div table td p"));
        assert!(m("body * p"));
        assert!(!m("div > p"));
        assert!(!m("table > p"));
        assert!(!m("span p"));
        assert!(!m("p div"));
        assert!(!m("td.other > p"));
        // Greedy matching would take the nearest `td` for `td > table`; the
        // reach sets find the element that works.
        let path = [
            key("div", None, None),
            key("span", None, None),
            key("div", None, None),
            key("p", None, None),
        ];
        let m = |sel: &str| Selector::parse(sel).unwrap().matches(&path[..], &b);
        assert!(m("div > span p"));
        assert!(m("div > span div > p"));
        assert!(!m("span > p"));
        assert!(m("div div p"));
        assert!(!m("div div div p"));
    }

    #[test]
    fn sheets_skip_at_rules_and_comments() {
        let b = budget();
        let sheet = StyleSheet::parse(
            "<!-- /* Font Definitions */ @font-face {font-family:x; panose-1:2 4} \
             p.MsoNormal, li.MsoNormal {margin:0in; font-size:12.0pt}\
             @media screen and (max-width:600px) { p { color:red } .x { color:blue } }\
             @import url(x.css); @page WordSection1 {size:8.5in 11.0in;}\
             a:link, span.MsoHyperlink {color:blue}\
             div.Word { } h1 {color:#2F5496} -->",
            &b,
        );
        // p.MsoNormal, li.MsoNormal, span.MsoHyperlink, h1 (the empty
        // block keeps nothing; a:link is dropped alone).
        assert_eq!(sheet.len(), 4);
        let path = [key("p", None, None)];
        assert!(
            sheet.matching(&path[..], &b).is_empty(),
            "@media must not apply"
        );
        let path = [key("h1", None, None)];
        let hits = sheet.matching(&path[..], &b);
        assert_eq!(hits.len(), 1);
        assert_eq!(sheet.decls(hits[0])[0].value, "#2F5496");
        // An unclosed block or at-rule ends the sheet without a panic.
        for cut in ["p {color:red", "@media x { p {", "p", "{}}}", "\"{"] {
            let _ = StyleSheet::parse(cut, &b);
        }
    }

    #[test]
    fn matching_returns_cascade_order() {
        let b = budget();
        let sheet = StyleSheet::parse(
            "p {color:red} .a {color:green} p {color:blue} #i {color:black} * {color:gray}",
            &b,
        );
        let path = [key("p", Some("i"), Some("a"))];
        let order: Vec<String> = sheet
            .matching(&path[..], &b)
            .into_iter()
            .map(|h| sheet.decls(h)[0].value.clone())
            .collect();
        assert_eq!(order, ["gray", "red", "blue", "green", "black"]);
    }

    #[test]
    fn rules_past_the_cap_are_ignored() {
        let b = budget();
        let css = ".c {color:red}\n".repeat(MAX_RULES + 50);
        let sheet = StyleSheet::parse(&css, &b);
        assert_eq!(sheet.len(), MAX_RULES);
        assert!(!b.exhausted());
    }

    #[test]
    fn deep_matching_is_polynomial() {
        // Every element matches every compound: backtracking would try
        // C(256, 32) ways; the reach sets look at each element once a step.
        let b = budget();
        let path: Vec<ElemKey> = (0..256).map(|_| key("div", None, None)).collect();
        let sel = Selector::parse(&vec!["div"; MAX_COMPOUNDS].join(" ")).unwrap();
        assert!(sel.matches(&path[..], &b));
        let miss =
            Selector::parse(&format!("p {}", vec!["div"; MAX_COMPOUNDS - 1].join(" "))).unwrap();
        assert!(!miss.matches(&path[..], &b));
        assert!(!b.exhausted());
    }
}
