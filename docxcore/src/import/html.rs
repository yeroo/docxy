//! HTML → [`Document`] (#633): a Web Page Word saved (Web Page or Web Page,
//! Filtered), or any simple HTML, read with a lenient tag tokenizer rather
//! than a DOM.
//!
//! The encoding comes from a byte-order mark, `<meta charset>` or
//! `http-equiv … charset=`, as UTF-8, UTF-16 or a Windows single-byte page
//! (opccore's tables; ISO-8859-1 reads as 1252, as browsers do). `<head>`,
//! `<style>`, `<script>` and comments are not text (a pre-pass reads the
//! `<style>` blocks, wherever they are). Word's downlevel blocks
//! (`<![if !supportLists]>…<![endif]>`) are not text: the list marker in
//! one only tells a bullet from a number. Filtered HTML keeps that marker as
//! plain text in a `MsoList…` paragraph, so there it is taken off the front.
//!
//! Kept: `p div h1-h6 li blockquote pre` paragraphs (headings styled),
//! `br`, `ul`/`ol` lists, tables, `b strong i em u s strike del sup sub`,
//! alignment, and CSS (#1872): inline `style`, `<font>` and every `<style>`
//! block of the page ([`css`]), cascaded in CSS's order over a stack of open
//! elements, each with its computed [`Look`]. Runs take colour, background
//! (as shading), font family and size, weight, style, decoration, vertical
//! alignment, `mso-highlight`, and `display:none` / `mso-hide:all` as
//! hidden; paragraphs take alignment, line height, their block's top and
//! bottom box as spacing, every enclosing block's left and right box as
//! indent, `text-indent`, and a block's background as shading. A table cell
//! starts a fresh block look; an HTML list item keeps its level's hanging
//! indent. A mis-nested inline close ends the element at once (the looks
//! above it are computed again), an end reopens the formatting elements
//! left open inside (three alike at most), and no close reaches across a
//! table, as in a browser. White space collapses as a browser collapses it, except in
//! `pre`. Images, links' targets and forms are not converted.

use super::css::{self, Color, Decl, ElemKey, Len, LineHeight, Path, StyleSheet};
use super::{Budget, Builder, MAX_DEPTH, TOO_BIG, heading_props, marker_is_numbered};
use crate::model::{Align, Document, ParProps, RunProps, VertAlign};

/// The longest tag name or list marker kept, in bytes.
const MAX_TAG: usize = 32;

/// Read an HTML page. `Err` when it holds no text, or would cost more
/// than an import may ([`TOO_BIG`]).
pub fn import_html(bytes: &[u8]) -> Result<Document, String> {
    import_html_within(bytes, &Budget::standard())
}

/// [`import_html`] against `budget`. Decoding, the `<style>` scan and the
/// tokenizer are each one forward pass over the input (charged once here);
/// the style sheet is charged as it is parsed and matched; the element
/// stacks are at most [`MAX_DEPTH`] deep, so a closing tag's search is
/// bounded; the document is charged as it is built.
pub(crate) fn import_html_within(bytes: &[u8], budget: &Budget) -> Result<Document, String> {
    // Three passes: decoding, the `<style>` scan, then the tokenizer.
    if !budget.work(3 * (bytes.len() / 16 + 1)) {
        return Err(TOO_BIG.into());
    }
    let text = decode(bytes);
    let sheet = StyleSheet::parse(&collect_styles(&text), budget);
    let mut h = Html::new(&text, budget, sheet);
    h.run();
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    if h.out.is_empty() {
        return Err("the HTML file holds no text".into());
    }
    let props = h.par_props();
    let doc = h.out.finish(props);
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    Ok(doc)
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
    let units = bytes.as_chunks::<2>().0.iter().map(|&p| {
        if big_endian {
            u16::from_be_bytes(p)
        } else {
            u16::from_le_bytes(p)
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

/// What a block element says about the paragraphs in it: structure only.
/// How they look is the element stack's [`Look`].
#[derive(Clone, Default)]
struct Block {
    heading: Option<u8>,
    /// A Word list paragraph (class `MsoList…`): its marker is text in
    /// Filtered HTML.
    mso_list: bool,
    /// Inside an `li`: (numbered, level).
    item: Option<(bool, i32)>,
}

/// How an element takes part in the page, for its look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Makes paragraphs: its box is paragraph spacing and indent, its
    /// background paragraph shading.
    Block,
    /// `td` / `th`: a fresh block look (#1872 9a).
    Cell,
    /// `ul ol dir menu dl`: the level indent stands for its box.
    List,
    /// `table tr thead tbody tfoot`: its box is the table's (#1878).
    Table,
    /// A known inline element: its background is run shading.
    Inline,
    /// An HTML5 block-level container (`section`, `article`, …): it ends
    /// paragraphs and closes an open `p` like a `div`, but its box and
    /// background are not read.
    Container,
    /// Anything else (`html`, `body`, a custom tag): only what is inherited.
    Other,
}

/// The HTML5 containers that are [`Kind::Container`].
const CONTAINERS: [&str; 16] = [
    "article",
    "aside",
    "details",
    "dialog",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hgroup",
    "main",
    "nav",
    "search",
    "section",
    "summary",
];

impl Kind {
    fn of(tag: &str) -> Kind {
        match tag {
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "blockquote" | "pre"
            | "address" | "dt" | "dd" | "center" | "caption" => Kind::Block,
            "td" | "th" => Kind::Cell,
            "ul" | "ol" | "dir" | "menu" | "dl" => Kind::List,
            "table" | "tr" | "thead" | "tbody" | "tfoot" => Kind::Table,
            "b" | "strong" | "i" | "em" | "cite" | "dfn" | "var" | "u" | "ins" | "s" | "strike"
            | "del" | "sup" | "sub" | "span" | "font" | "a" | "small" | "big" | "code" | "tt"
            | "kbd" | "samp" | "abbr" | "acronym" | "label" | "q" | "mark" | "o:p" => Kind::Inline,
            _ if CONTAINERS.contains(&tag) => Kind::Container,
            _ => Kind::Other,
        }
    }

    /// Whether it bounds paragraphs (an inline element closed under one is
    /// taken out alone, [`Html::close_elem`]).
    fn structural(self) -> bool {
        matches!(
            self,
            Kind::Block | Kind::Cell | Kind::List | Kind::Table | Kind::Container
        )
    }

    /// A table or a cell: what a close never reaches across.
    fn table(self) -> bool {
        matches!(self, Kind::Cell | Kind::Table)
    }
}

/// A paragraph's line spacing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Line {
    /// In 240ths of a line.
    Auto(i32),
    /// In twips: at least, or exactly with `mso-line-height-rule: exactly`.
    Twips(i32),
}

/// What an element's text and paragraphs look like: the computed style of
/// one element, from its parent's and the cascade.
#[derive(Clone, Debug, Default)]
struct Look {
    /// The runs inside (inherited, run shading of an inline ancestor too).
    run: RunProps,
    /// The cascade said not bold in so many words (`font-weight: normal`):
    /// under a heading that is an explicit off (#1872 6b).
    bold_off: bool,
    align: Option<Align>,
    line: Option<Line>,
    line_exact: bool,
    /// `text-indent`, in twips (inherited).
    first_line: i32,
    /// Left and right box of every enclosing block, summed, in twips.
    indent: i32,
    indent_right: i32,
    /// Paragraph shading (`RRGGBB`) of the nearest shaded block.
    shade: Option<String>,
    /// The element's own top and bottom box, in twips (not inherited).
    before: Option<i32>,
    after: Option<i32>,
}

/// An open element.
struct Elem {
    key: ElemKey,
    kind: Kind,
    look: Look,
    /// Where its attribute text is in the page: what its look is computed
    /// from again when an element below it closes (#1872 r1).
    attrs: (usize, usize),
    /// `blocks` and `lists` when it opened: what its close truncates them to
    /// when it is not on them itself (#1872 r2).
    blocks_at: usize,
    lists_at: usize,
}

/// The HTML formatting elements: a block end reopens those left open in it
/// (as a browser's list of active formatting elements does).
const FORMATTING: [&str; 14] = [
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// Elements whose content is not markup: skipped by the tokenizer and by
/// the `<style>` scan (whose own `style` is the one it reads).
const RAW_TEXT: [&str; 11] = [
    "style", "script", "title", "xml", "noscript", "template", "object", "svg", "iframe", "select",
    "textarea",
];

/// Whether `key` starts a heading: `h1`-`h6`, or Word's `MsoHeadingN`.
fn starts_heading(key: &ElemKey) -> bool {
    key.tag
        .strip_prefix('h')
        .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6"))
        || key.classes.first().is_some_and(|c| {
            c.strip_prefix("msoheading")
                .is_some_and(|n| n.parse::<u8>().is_ok())
        })
}

/// An element about to open, below its open ancestors: what selectors
/// match against.
struct Ancestry<'a> {
    elems: &'a [Elem],
    subject: &'a ElemKey,
}

impl Path for Ancestry<'_> {
    fn len(&self) -> usize {
        self.elems.len() + 1
    }
    fn key(&self, i: usize) -> &ElemKey {
        match self.elems.get(i) {
            Some(e) => &e.key,
            None => self.subject,
        }
    }
}

/// The explicit bold off a run under a heading gets (#1427's form).
const BOLD_OFF: &str = "<w:b w:val=\"0\"/>";

/// Shading of `fill` (`RRGGBB`), run or paragraph.
fn shd(fill: &str) -> String {
    format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>")
}

/// The attributes the importer reads, from one scan of a tag.
#[derive(Default)]
struct Attrs {
    style: Option<String>,
    class: Option<String>,
    id: Option<String>,
    align: Option<String>,
    color: Option<String>,
    face: Option<String>,
    size: Option<String>,
}

/// One element's style, being computed: its look so far, and the box
/// values kept until its font size is known.
struct Cascade {
    look: Look,
    kind: Kind,
    parent_pt: f64,
    margin: [Option<Len>; 4],
    padding: [Option<Len>; 4],
    text_indent: Option<Len>,
    line: Option<LineHeight>,
    background: Option<String>,
}

/// A twips sum kept in Word's range.
fn clamp_twips(v: i64) -> i32 {
    v.clamp(-31_680, 31_680) as i32
}

fn font_pt(run: &RunProps) -> f64 {
    run.size_half_pts
        .map_or(css::BASE_PT, |h| f64::from(h) / 2.0)
}

fn align_value(v: &str) -> Option<Align> {
    match v.trim().to_ascii_lowercase().as_str() {
        "center" | "-webkit-center" | "-moz-center" => Some(Align::Center),
        "right" | "end" | "-webkit-right" => Some(Align::Right),
        "justify" => Some(Align::Justify),
        "left" | "start" | "-webkit-left" => Some(Align::Left),
        _ => None,
    }
}

impl Cascade {
    fn new(parent: Option<&Look>, kind: Kind) -> Self {
        let mut look = parent.cloned().unwrap_or_default();
        look.before = None;
        look.after = None;
        if kind == Kind::Cell {
            // A cell starts a fresh block look; what is inherited (the runs,
            // alignment, line height) still passes in.
            look.indent = 0;
            look.indent_right = 0;
            look.first_line = 0;
            look.shade = None;
        }
        let parent_pt = font_pt(&look.run);
        Cascade {
            look,
            kind,
            parent_pt,
            margin: [None; 4],
            padding: [None; 4],
            text_indent: None,
            line: None,
            background: None,
        }
    }

    fn set_bold(&mut self, bold: bool) {
        self.look.run.bold = bold;
        self.look.bold_off = !bold;
    }

    /// The user agent's look of `key`: tag semantics (`b`, `i`, …), the
    /// `center` alignment, the blockquote margins. A heading starts with no
    /// explicit not-bold: only a declaration on it or inside it makes one
    /// (an ancestor's `font:` shorthand must not, #1872 r1).
    fn user_agent(&mut self, key: &ElemKey) {
        if starts_heading(key) {
            self.look.bold_off = false;
        }
        let tag = key.tag.as_str();
        let run = &mut self.look.run;
        match tag {
            "b" | "strong" => self.set_bold(true),
            "i" | "em" | "cite" | "dfn" | "var" => run.italic = true,
            "u" | "ins" => run.underline = true,
            "s" | "strike" | "del" => run.strike = true,
            "sup" => run.vert_align = VertAlign::Superscript,
            "sub" => run.vert_align = VertAlign::Subscript,
            "center" => self.look.align = Some(Align::Center),
            "blockquote" => {
                let m = Some(Len {
                    v: 40.0,
                    unit: css::Unit::Px,
                });
                self.margin[1] = m;
                self.margin[3] = m;
            }
            _ => {}
        }
    }

    /// Presentational attributes: `align` on a block or cell, `<font color
    /// face size>`.
    fn presentational(&mut self, tag: &str, attrs: &Attrs) {
        if matches!(self.kind, Kind::Block | Kind::Cell) {
            if let Some(a) = attrs.align.as_deref().and_then(align_value) {
                self.look.align = Some(a);
            }
        }
        if tag == "font" {
            if let Some(c) = attrs.color.as_deref().and_then(css::attr_color) {
                self.look.run.color = Some(c);
            }
            if let Some(f) = attrs.face.as_deref().and_then(css::font_family) {
                self.look.run.font = Some(f);
            }
            if let Some(pt) = attrs.size.as_deref().and_then(css::font_attr_size) {
                self.look.run.size_half_pts = Some(css::half_points(pt));
            }
        }
    }

    fn apply(&mut self, d: &Decl) {
        let v = d.value.as_str();
        let run = &mut self.look.run;
        let side = |name: &str| match name {
            "top" => Some(0),
            "right" => Some(1),
            "bottom" => Some(2),
            "left" => Some(3),
            _ => None,
        };
        match d.name.as_str() {
            "color" => match css::color(v) {
                Some(Color::Rgb(c)) => run.color = Some(c),
                Some(Color::Reset) => run.color = None,
                Some(Color::Keep) | None => {}
            },
            "background" | "background-color" => {
                if let Some(c) = css::background(v) {
                    self.background = Some(c);
                }
            }
            "font-family" => {
                if let Some(f) = css::font_family(v) {
                    run.font = Some(f);
                }
            }
            "font-size" => {
                if let Some(pt) = css::font_size_pt(v, self.parent_pt) {
                    run.size_half_pts = Some(css::half_points(pt));
                }
            }
            "font-weight" => {
                if let Some(b) = css::weight(v) {
                    self.set_bold(b);
                }
            }
            "font-style" => {
                if let Some(i) = css::italic(v) {
                    run.italic = i;
                }
            }
            "font" => {
                if let Some(f) = css::font_shorthand(v, self.parent_pt) {
                    run.italic = f.italic;
                    run.size_half_pts = Some(css::half_points(f.size_pt));
                    if let Some(family) = f.family {
                        run.font = Some(family);
                    }
                    self.set_bold(f.bold);
                    self.line = Some(f.line);
                }
            }
            "text-decoration" | "text-decoration-line" => {
                let v = v.to_ascii_lowercase();
                if v.contains("none") {
                    run.underline = false;
                    run.strike = false;
                }
                if v.contains("underline") {
                    run.underline = true;
                }
                if v.contains("line-through") {
                    run.strike = true;
                }
            }
            "vertical-align" => match v.trim().to_ascii_lowercase().as_str() {
                "super" => run.vert_align = VertAlign::Superscript,
                "sub" => run.vert_align = VertAlign::Subscript,
                "baseline" => run.vert_align = VertAlign::Baseline,
                _ => {}
            },
            "text-align" => {
                if let Some(a) = align_value(v) {
                    self.look.align = Some(a);
                }
            }
            "line-height" => {
                if let Some(l) = css::line_height(v) {
                    self.line = Some(l);
                }
            }
            "mso-line-height-rule" => match v.trim().to_ascii_lowercase().as_str() {
                "exactly" => self.look.line_exact = true,
                "at-least" => self.look.line_exact = false,
                _ => {}
            },
            "margin" | "padding" => {
                if let Some(sides) = css::box_sides(v) {
                    if d.name == "margin" {
                        self.margin = sides;
                    } else {
                        self.padding = sides;
                    }
                }
            }
            "text-indent" => {
                if let Some(l) = css::length(v).filter(|l| l.unit != css::Unit::Percent) {
                    self.text_indent = Some(l);
                }
            }
            "display" => {
                if v.trim().eq_ignore_ascii_case("none") {
                    run.vanish = true;
                }
            }
            "mso-hide" => {
                if v.trim().eq_ignore_ascii_case("all") {
                    run.vanish = true;
                }
            }
            "mso-highlight" => match css::highlight(v) {
                Some("none") => run.highlight = None,
                Some(h) => run.highlight = Some(h.to_string()),
                None => {}
            },
            name => {
                let longhand = name
                    .strip_prefix("margin-")
                    .map(|s| (s, true))
                    .or_else(|| name.strip_prefix("padding-").map(|s| (s, false)));
                if let Some((s, margin)) = longhand {
                    if let (Some(i), Ok(len)) = (side(s), css::side(v)) {
                        if margin {
                            self.margin[i] = len;
                        } else {
                            self.padding[i] = len;
                        }
                    }
                }
            }
        }
    }

    /// The look, the lengths resolved against the element's own font size.
    fn finish(mut self) -> Look {
        let pt = font_pt(&self.look.run);
        let look = &mut self.look;
        match self.line {
            None => {}
            Some(LineHeight::Normal) => look.line = None,
            Some(LineHeight::Multiple(m)) => {
                let n = (m * 240.0).round();
                look.line = (n >= 1.0).then(|| Line::Auto(n.min(31_680.0) as i32));
            }
            Some(LineHeight::Length(l)) => {
                look.line = l.twips(pt).filter(|&t| t > 0).map(Line::Twips);
            }
        }
        if let Some(t) = self.text_indent.and_then(|l| l.twips(pt)) {
            look.first_line = t;
        }
        let tw = |l: Option<Len>| l.and_then(|l| l.twips(pt));
        let sum = |a: Option<Len>, b: Option<Len>| match (tw(a), tw(b)) {
            (None, None) => None,
            (a, b) => Some(clamp_twips(
                i64::from(a.unwrap_or(0)) + i64::from(b.unwrap_or(0)),
            )),
        };
        match self.kind {
            Kind::Block => {
                // Spacing is unsigned (ST_TwipsMeasure): a negative margin is 0.
                look.before = sum(self.margin[0], self.padding[0]).map(|t| t.max(0));
                look.after = sum(self.margin[2], self.padding[2]).map(|t| t.max(0));
                if let Some(l) = sum(self.margin[3], self.padding[3]) {
                    look.indent = clamp_twips(i64::from(look.indent) + i64::from(l));
                }
                if let Some(r) = sum(self.margin[1], self.padding[1]) {
                    look.indent_right = clamp_twips(i64::from(look.indent_right) + i64::from(r));
                }
                if let Some(fill) = self.background {
                    look.shade = Some(fill);
                }
            }
            Kind::Inline => {
                if let Some(fill) = self.background {
                    look.run.raw_props.retain(|r| !r.starts_with("<w:shd"));
                    look.run.raw_props.push(shd(&fill));
                }
            }
            Kind::Cell | Kind::List | Kind::Table | Kind::Container | Kind::Other => {}
        }
        self.look
    }
}

/// What computing a look reads besides the element stack.
#[derive(Clone, Copy)]
struct Ctx<'c> {
    sheet: &'c StyleSheet,
    budget: &'c Budget,
}

/// The look of an element opening on top of `below`: its parent's, then the
/// user agent's, the presentational attributes, the matched rules, the
/// inline style, the important rules and the important inline style.
fn look_of(ctx: Ctx, below: &[Elem], key: &ElemKey, kind: Kind, attrs: &Attrs) -> Look {
    let mut c = Cascade::new(below.last().map(|e| &e.look), kind);
    c.user_agent(key);
    c.presentational(&key.tag, attrs);
    let hits = ctx.sheet.matching(
        &Ancestry {
            elems: below,
            subject: key,
        },
        ctx.budget,
    );
    let inline = attrs
        .style
        .as_deref()
        .map(css::parse_declarations)
        .unwrap_or_default();
    for important in [false, true] {
        for &hit in &hits {
            for d in ctx.sheet.decls(hit) {
                if d.important == important {
                    c.apply(d);
                }
            }
        }
        for d in &inline {
            if d.important == important {
                c.apply(d);
            }
        }
    }
    c.finish()
}

/// The CSS text of every `<style>` element of the page, in order: one
/// forward scan, skipping comments and the contents of elements whose text
/// is not markup (a `<style>` inside a comment or a script does not apply).
fn collect_styles(s: &str) -> String {
    let mut css = String::new();
    let mut i = 0;
    while let Some(off) = s[i..].find('<') {
        let at = i + off;
        let rest = &s[at..];
        if let Some(after) = rest.strip_prefix("<!--") {
            i = match after.find("-->") {
                Some(e) => at + 4 + e + 3,
                None => s.len(),
            };
            continue;
        }
        let name_len = rest[1..]
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len() - 1);
        let name = rest[1..1 + name_len.min(MAX_TAG)].to_ascii_lowercase();
        if !RAW_TEXT.contains(&name.as_str()) {
            i = at + 1;
            continue;
        }
        let Some(open_end) = tag_end(rest) else {
            break;
        };
        let content = at + open_end;
        let close = format!("</{name}");
        let end = find_ci(&s[content..], &close).map_or(s.len(), |e| content + e);
        if name == "style" {
            css.push_str(&s[content..end]);
            css.push('\n');
        }
        i = end;
    }
    css
}

struct Html<'a> {
    s: &'a str,
    pos: usize,
    budget: &'a Budget,
    out: Builder<'a>,
    /// Every `<style>` of the page.
    sheet: StyleSheet,
    /// Every open element (but void and skipped ones) and its look.
    elems: Vec<Elem>,
    blocks: Vec<(String, Block)>,
    /// Open `ul` / `ol`: whether each is ordered.
    lists: Vec<bool>,
    table_depth: usize,
    /// Open `pre` elements on `blocks`.
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
    fn new(s: &'a str, budget: &'a Budget, sheet: StyleSheet) -> Self {
        Html {
            s,
            pos: 0,
            budget,
            out: Builder::new(budget),
            sheet,
            elems: Vec::new(),
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
        while self.pos < self.s.len() && !self.budget.exhausted() {
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
        // A tag name is ASCII (the scan above takes only alphanumerics, `:`
        // and `-`); only its first MAX_TAG bytes are kept and matched, so a
        // long one is not copied onto a stack.
        let name = rest[name_start..name_start + name_len.min(MAX_TAG)].to_ascii_lowercase();
        // The attributes run to the `>` (one byte, a char boundary), or to
        // the end of a page cut inside the tag, never into a character.
        let (end, attrs_end) = match tag_end(rest) {
            Some(end) => (end, end - 1),
            None => (rest.len(), rest.len()),
        };
        let attrs = (
            self.pos + name_start + name_len,
            self.pos + attrs_end.max(name_start + name_len),
        );
        self.pos += end;
        if close {
            self.close_tag(&name);
        } else {
            self.open_tag(&name, attrs);
        }
    }

    fn open_tag(&mut self, name: &str, attrs_at: (usize, usize)) {
        match name {
            _ if name == "head" || RAW_TEXT.contains(&name) => {
                let end = format!("</{name}");
                self.skip_past(&end);
                self.skip_past(">");
                return;
            }
            "br" => {
                if self.visible() {
                    let props = self.run_props();
                    self.out.line_break(&props);
                    self.space = None;
                    self.para_start = true;
                }
                return;
            }
            "hr" => {
                self.implicit_close(&["p"]);
                return;
            }
            // Void elements: nothing opens.
            "img" | "meta" | "link" | "input" | "wbr" | "col" | "area" | "base" | "source"
            | "param" | "embed" | "track" => return,
            _ => {}
        }
        let s = self.s;
        let attrs = attrs_of(&s[attrs_at.0..attrs_at.1]);
        let kind = Kind::of(name);
        // Omitted table end tags (#1872 r3): a cell ends the open cell of its
        // row, a row the open cell and row of its table, a row group both
        // and the open row group; never across a nested table.
        const ROW_GROUPS: [&str; 3] = ["thead", "tbody", "tfoot"];
        match name {
            "td" | "th" => {
                self.close_open(&["td", "th"], &["tr", "table", "thead", "tbody", "tfoot"])
            }
            "tr" | "thead" | "tbody" | "tfoot" => {
                self.close_open(&["td", "th"], &["tr", "table", "thead", "tbody", "tfoot"]);
                self.close_open(&["tr"], &["table", "thead", "tbody", "tfoot"]);
                if ROW_GROUPS.contains(&name) {
                    self.close_open(&ROW_GROUPS, &["table"]);
                }
            }
            _ => {}
        }
        // Malformed input (#1872 13): a block closes an open `p`, an `li` an
        // open `li` of the same list, a `dt` / `dd` an open `dt` / `dd`.
        if matches!(
            name,
            "p" | "div"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "dl"
                | "dir"
                | "menu"
                | "table"
                | "blockquote"
                | "pre"
                | "li"
                | "address"
                | "center"
                | "dt"
                | "dd"
                | "listing"
                | "xmp"
        ) || kind == Kind::Container
        {
            self.implicit_close(&["p"]);
        }
        if kind == Kind::Container {
            self.end_para_if_any();
        }
        match name {
            "li" => self.implicit_close(&["li"]),
            "dt" | "dd" => self.implicit_close(&["dt", "dd"]),
            _ => {}
        }
        let key = ElemKey::new(name, attrs.id.as_deref(), attrs.class.as_deref());
        if name == "span"
            && attrs
                .style
                .as_deref()
                .is_some_and(|s| s.to_ascii_lowercase().contains("mso-tab-count"))
        {
            // Word's tab: the spaces inside stand for it.
            if self.visible() {
                let props = look_of(self.ctx(), &self.elems, &key, kind, &attrs).run;
                self.out.tab(&props);
                self.space = None;
                self.para_start = false;
            }
            self.skip_past("</span");
            self.skip_past(">");
            return;
        }
        // The stacks as they are below this element (#1872 r4: a list's own
        // entry is above its `lists_at`, so its close ends it).
        let (blocks_at, lists_at) = (self.blocks.len(), self.lists.len());
        match name {
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
                let class = attrs.class.as_deref().unwrap_or("").to_ascii_lowercase();
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
                if name == "td" || name == "th" {
                    // A cell starts with nothing structural inherited from
                    // outside the table.
                    block = Block::default();
                }
                // Past MAX_DEPTH an open is ignored (its close finds an
                // outer element, or nothing).
                if self.blocks.len() < MAX_DEPTH {
                    self.blocks.push((name.to_string(), block));
                    self.recount_pre();
                }
            }
            "ul" | "ol" | "dir" | "menu" => {
                self.end_para_if_any();
                if self.lists.len() < MAX_DEPTH {
                    self.lists.push(name == "ol");
                }
            }
            "table" => {
                self.end_para_if_any();
                self.table_depth += 1;
            }
            _ => {}
        }
        if self.elems.len() < MAX_DEPTH && self.budget.work(1 + key.heap() / 16) {
            let look = look_of(self.ctx(), &self.elems, &key, kind, &attrs);
            self.elems.push(Elem {
                key,
                kind,
                look,
                attrs: attrs_at,
                blocks_at,
                lists_at,
            });
        }
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            sheet: &self.sheet,
            budget: self.budget,
        }
    }

    /// Compute again the looks of `elems[from..]`, each from the element
    /// below it and its own tag, classes, id and attributes, after an element
    /// under them closed or the stack under them changed. Each costs the
    /// scan of its attributes and its matching.
    fn restyle(&mut self, from: usize) {
        let s = self.s;
        for j in from..self.elems.len() {
            let (below, rest) = self.elems.split_at_mut(j);
            let e = &mut rest[0];
            let (a0, a1) = e.attrs;
            if !self.budget.scanned(a1 - a0) {
                return;
            }
            let attrs = attrs_of(&s[a0..a1]);
            e.look = look_of(
                Ctx {
                    sheet: &self.sheet,
                    budget: self.budget,
                },
                below,
                &e.key,
                e.kind,
                &attrs,
            );
        }
    }

    /// Truncate the element stack at `i`. The formatting elements left open
    /// above it (outside any table there) are opened again on top of what
    /// remains, as a browser reconstructs its active formatting elements:
    /// `<p><b>a</p><p>b` keeps `b` bold. Not when `elems[i]` is a table or a
    /// cell, whose formatting never leaves it. One is not reopened when three
    /// alike (same tag and attributes) are already open below, back to the
    /// nearest table or cell, or reopened: a browser's Noah's Ark clause,
    /// which keeps an unclosed `<b>` per paragraph from piling up.
    fn truncate_elems(&mut self, i: usize) {
        if i >= self.elems.len() {
            return;
        }
        let mut reopen: Vec<Elem> = Vec::new();
        if !self.elems[i].kind.table() {
            let s = self.s;
            let (below, above) = self.elems.split_at_mut(i + 1);
            // Below the closed element, back to the nearest table or cell.
            let scope = below[..i]
                .iter()
                .rposition(|e| e.kind.table())
                .map_or(0, |t| t + 1);
            let below = &below[scope..i];
            for e in above.iter_mut() {
                // A guard: a close never reaches across a table
                // (`out_of_scope`), and a table's own close reopens nothing.
                if e.kind.table() {
                    break;
                }
                if !FORMATTING.contains(&e.key.tag.as_str())
                    || !self.budget.work(below.len() / 16 + 1)
                {
                    continue;
                }
                let alike = |o: &&Elem| {
                    o.key.tag == e.key.tag && s[o.attrs.0..o.attrs.1] == s[e.attrs.0..e.attrs.1]
                };
                if below.iter().filter(alike).count() + reopen.iter().filter(alike).count() < 3 {
                    reopen.push(Elem {
                        key: std::mem::take(&mut e.key),
                        kind: e.kind,
                        look: Look::default(),
                        attrs: e.attrs,
                        blocks_at: 0,
                        lists_at: 0,
                    });
                }
            }
        }
        self.elems.truncate(i);
        let (blocks_at, lists_at) = (self.blocks.len(), self.lists.len());
        for e in &mut reopen {
            e.blocks_at = blocks_at;
            e.lists_at = lists_at;
        }
        let from = self.elems.len();
        self.elems.extend(reopen);
        self.restyle(from);
    }

    /// Close the innermost open element named in `tags` that the element
    /// opening now ends implicitly: never across a table cell or table,
    /// nor, for `li` / `dt` / `dd`, across a list. Its paragraph ends only
    /// if it has content.
    fn implicit_close(&mut self, tags: &[&str]) {
        if !self.budget.work(self.elems.len() / 16 + 1) {
            return;
        }
        let list_bound = !tags.contains(&"p");
        let mut found = None;
        for (i, e) in self.elems.iter().enumerate().rev() {
            if tags.contains(&e.key.tag.as_str()) {
                found = Some(i);
                break;
            }
            if e.kind.table() || (list_bound && e.kind == Kind::List) {
                break;
            }
        }
        let Some(i) = found else {
            return;
        };
        self.end_para_if_any();
        let tag = self.elems[i].key.tag.clone();
        let lists_at = self.elems[i].lists_at;
        self.truncate_elems(i);
        self.lists.truncate(lists_at);
        if let Some(j) = self.blocks.iter().rposition(|(n, _)| *n == tag) {
            if !self.blocks[j + 1..]
                .iter()
                .any(|(n, _)| n == "td" || n == "th")
            {
                self.blocks.truncate(j);
                self.recount_pre();
            }
        }
    }

    fn recount_pre(&mut self) {
        self.pre = self.blocks.iter().filter(|(n, _)| n == "pre").count();
    }

    fn close_tag(&mut self, name: &str) {
        if self.out_of_scope(name) {
            return;
        }
        match name {
            // A `</li>` whose `li` is below an open list is not in list-item
            // scope: a browser ignores it (#1872 r4).
            "li" => {
                let under_list = self
                    .elems
                    .iter()
                    .rposition(|e| e.key.tag == "li")
                    .is_some_and(|i| {
                        self.elems[i + 1..]
                            .iter()
                            .any(|e| matches!(e.key.tag.as_str(), "ul" | "ol" | "dir" | "menu"))
                    });
                if under_list {
                    return;
                }
            }
            // A row group's end ends its open cell and row first, as its
            // start does.
            "thead" | "tbody" | "tfoot" => {
                self.close_open(&["td", "th"], &["tr", "table", "thead", "tbody", "tfoot"]);
                self.close_open(&["tr"], &["table", "thead", "tbody", "tfoot"]);
            }
            _ => {}
        }
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
                    self.blocks.truncate(i);
                    self.recount_pre();
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
                    self.recount_pre();
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
                // `close_elem` cuts `lists` and `blocks` back to where the
                // list element opened: an `li` left open in it ends, and so
                // does a list opened in it and left open (`</ol>` over an
                // open `ul`).
                self.end_para_if_any();
            }
            _ => {}
        }
        self.close_elem(name);
    }

    /// Pop `name` and what is inside it off the element stack.
    ///
    /// - `html` and `body` never close (text after them is still the page's).
    /// - An inline element closed under an open block (`<b><p>x</b>y`) is
    ///   taken out alone and what is above it restyled (#1872 r1), so `y` and
    ///   what follows are not bold.
    /// - A container or other element closed over an open paragraph ends it
    ///   and the blocks and lists opened inside it (#1872 r2).
    /// - What is left open inside is reopened ([`Self::truncate_elems`]).
    ///
    /// A close never reaches across a table or cell ([`Self::out_of_scope`]).
    fn close_elem(&mut self, name: &str) {
        if name == "html" || name == "body" {
            return;
        }
        let Some(i) = self.elems.iter().rposition(|e| e.key.tag == name) else {
            return;
        };
        let kind = self.elems[i].kind;
        let over_structure = self.elems[i + 1..].iter().any(|e| e.kind.structural());
        if kind == Kind::Inline && over_structure {
            self.elems.remove(i);
            self.restyle(i);
            return;
        }
        if kind == Kind::Container || (kind == Kind::Other && over_structure) {
            self.end_para_if_any();
        }
        // The blocks and lists opened inside it end with it (#1872 r3: a
        // `</div>` or `</td>` over an unclosed `ul` ended no list).
        let (blocks_at, lists_at) = (self.elems[i].blocks_at, self.elems[i].lists_at);
        self.blocks.truncate(blocks_at);
        self.lists.truncate(lists_at);
        self.recount_pre();
        self.truncate_elems(i);
    }

    /// Close the innermost open element named in `tags` (through
    /// [`Self::close_tag`], so the cell or row ends in the document too),
    /// unless one named in `bounds` is open above it.
    fn close_open(&mut self, tags: &[&str], bounds: &[&str]) {
        if !self.budget.work(self.elems.len() / 16 + 1) {
            return;
        }
        let mut found = None;
        for e in self.elems.iter().rev() {
            let t = e.key.tag.as_str();
            if tags.contains(&t) {
                found = Some(t.to_string());
                break;
            }
            if bounds.contains(&t) {
                break;
            }
        }
        if let Some(tag) = found {
            self.close_tag(&tag);
        }
    }

    /// Whether closing `name` would reach across an open table or cell (a
    /// browser ignores such a close): `</ul>` in a cell of a table in an `li`.
    fn out_of_scope(&self, name: &str) -> bool {
        if Kind::of(name).table() {
            return false;
        }
        self.elems
            .iter()
            .rposition(|e| e.key.tag == name)
            .is_some_and(|i| self.elems[i + 1..].iter().any(|e| e.kind.table()))
    }

    fn visible(&self) -> bool {
        !self.in_downlevel && !self.in_marker
    }

    fn run_props(&self) -> RunProps {
        let Some(e) = self.elems.last() else {
            return RunProps::default();
        };
        let mut p = e.look.run.clone();
        if e.look.bold_off
            && !p.bold
            && self.blocks.last().is_some_and(|(_, b)| b.heading.is_some())
        {
            p.raw_props.push(BOLD_OFF.to_string());
        }
        p
    }

    fn text(&mut self, raw: &str) {
        if self.in_downlevel {
            return;
        }
        let text = entities(raw);
        if self.in_marker {
            if let Some(m) = self.marker.as_mut() {
                if m.len() < MAX_TAG {
                    m.push_str(&text);
                }
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
        // The innermost paragraph-making element's look, else the
        // innermost element's (text straight in `body`).
        let Some(look) = self
            .elems
            .iter()
            .rev()
            .find(|e| matches!(e.kind, Kind::Block | Kind::Cell))
            .or(self.elems.last())
            .map(|e| &e.look)
        else {
            return p;
        };
        p.align = look.align.unwrap_or_default();
        p.spacing.before = look.before;
        p.spacing.after = look.after;
        match look.line {
            Some(Line::Auto(n)) => {
                p.spacing.line = Some(n);
                p.spacing.line_rule = Some("auto".into());
            }
            Some(Line::Twips(n)) => {
                p.spacing.line = Some(n);
                p.spacing.line_rule =
                    Some(if look.line_exact { "exact" } else { "atLeast" }.into());
            }
            None => {}
        }
        match block.item {
            // An HTML list item keeps the level's hanging indent; enclosing
            // boxes move it as a whole (#1872 9b).
            Some((_, level)) if !block.mso_list => {
                if look.indent != 0 {
                    p.indent = (720 * (level + 1) + look.indent).max(0);
                    p.first_line = -360;
                }
            }
            _ => {
                p.indent = look.indent.max(0);
                p.first_line = look.first_line;
            }
        }
        p.indent_right = look.indent_right.max(0);
        if let Some(fill) = &look.shade {
            p.raw_props.push(shd(fill));
        }
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

/// The attributes the importer reads, from one scan of a tag's attribute
/// text; the first of a name wins, as in a browser. Values have their
/// character references decoded.
fn attrs_of(attrs: &str) -> Attrs {
    let mut out = Attrs::default();
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
        let mut value = &attrs[i..i];
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
                value = &attrs[vs..i];
                i += 1;
            } else {
                let vs = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = &attrs[vs..i];
            }
        }
        let slot = match key.to_ascii_lowercase().as_str() {
            "style" => Some(&mut out.style),
            "class" => Some(&mut out.class),
            "id" => Some(&mut out.id),
            "align" => Some(&mut out.align),
            "color" => Some(&mut out.color),
            "face" => Some(&mut out.face),
            "size" => Some(&mut out.size),
            _ => None,
        };
        if let Some(slot) = slot.filter(|s| s.is_none()) {
            *slot = Some(entities(value));
        }
        if i == start {
            i += 1;
        }
    }
    out
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
    fn an_unclosed_tag_ending_in_a_non_ascii_character_does_not_panic() {
        // A truncated download: the page stops inside a tag's attribute.
        assert!(import_html("<p>ok</p><p title=\"caf\u{e9}".as_bytes()).is_ok());
        assert!(import_html("<p>ok</p><img alt=na\u{ef}".as_bytes()).is_ok());
        // Windows-1252 bytes decode to multi-byte characters too.
        assert!(import_html(b"<p>ok</p><img alt=\"na\xefve").is_ok());
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

    /// FIX r2: a ragged table is capped at MAX_COLUMNS, not padded to its
    /// widest row; against a small budget it fails.
    #[test]
    fn a_wide_ragged_table_is_capped_not_padded() {
        let mut page = String::from("<table><tr>");
        page.push_str(&"<td>x</td>".repeat(300));
        page.push_str("</tr>");
        page.push_str(&"<tr><td>y</td></tr>".repeat(300));
        page.push_str("</table>");
        let d = doc(&page);
        let DocBlock::Table(t) = &d.body[0] else {
            panic!("{:?}", d.body[0])
        };
        assert_eq!(t.grid.len(), crate::import::MAX_COLUMNS);
        assert!(
            t.rows
                .iter()
                .all(|r| r.cells.len() == crate::import::MAX_COLUMNS)
        );
        let small = Budget::new(1 << 20, 2_000);
        assert_eq!(
            import_html_within(page.as_bytes(), &small).unwrap_err(),
            TOO_BIG
        );
    }

    /// FIX r2: unclosed tags, then closes that match nothing, were a
    /// quadratic scan of an unbounded stack; the stacks stop at MAX_DEPTH.
    #[test]
    fn element_stacks_stop_at_max_depth() {
        // Opens on every stack, then closes that match nothing on any:
        // each stack must have stopped at exactly MAX_DEPTH.
        let page = format!(
            "{}text{}",
            "<b><div><ul>".repeat(10_000),
            "</i></h6></caption>".repeat(10_000)
        );
        let budget = Budget::standard();
        let mut h = Html::new(&page, &budget, StyleSheet::default());
        h.run();
        assert_eq!(h.elems.len(), MAX_DEPTH);
        assert_eq!(h.blocks.len(), MAX_DEPTH);
        assert_eq!(h.lists.len(), MAX_DEPTH);
    }

    // ---- CSS (#1872) ----

    /// Every run of every paragraph (tables row-major), in order.
    fn all_runs(d: &Document) -> Vec<crate::model::Run> {
        fn walk(blocks: &[DocBlock], out: &mut Vec<crate::model::Run>) {
            for b in blocks {
                match b {
                    DocBlock::Paragraph(p) => {
                        for i in &p.content {
                            if let Inline::Run(r) = i {
                                out.push(r.clone());
                            }
                        }
                    }
                    DocBlock::Table(t) => {
                        for c in t.rows.iter().flat_map(|r| &r.cells) {
                            walk(&c.blocks, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&d.body, &mut out);
        out
    }

    /// The run holding `text`.
    fn run_of(d: &Document, text: &str) -> crate::model::Run {
        all_runs(d)
            .into_iter()
            .find(|r| r.text.contains(text))
            .unwrap_or_else(|| panic!("no run holds {text:?}: {:?}", all_runs(d)))
    }

    /// Every paragraph, tables row-major.
    fn all_paras(d: &Document) -> Vec<Paragraph> {
        fn walk(blocks: &[DocBlock], out: &mut Vec<Paragraph>) {
            for b in blocks {
                match b {
                    DocBlock::Paragraph(p) => out.push(p.clone()),
                    DocBlock::Table(t) => {
                        for c in t.rows.iter().flat_map(|r| &r.cells) {
                            walk(&c.blocks, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&d.body, &mut out);
        out
    }

    fn para_of(d: &Document, text: &str) -> Paragraph {
        all_paras(d)
            .into_iter()
            .find(|p| p.plain_text().contains(text))
            .unwrap_or_else(|| panic!("no paragraph holds {text:?}"))
    }

    fn has_shd(raw: &[String], fill: &str) -> bool {
        raw.contains(&shd(fill))
    }

    #[test]
    fn inherited_properties_reach_the_runs() {
        let d = doc(
            "<body style='color:#336699;font-family:Georgia,serif;font-size:10pt'>\
             <p>plain <b>bold</b></p><p style='color:red'>red <b>bold red</b> <span style='font-size:2em'>big</span></p></body>",
        );
        let plain = run_of(&d, "plain");
        assert_eq!(plain.props.color.as_deref(), Some("336699"));
        assert_eq!(plain.props.font.as_deref(), Some("Georgia"));
        assert_eq!(plain.props.size_half_pts, Some(20));
        let bold = run_of(&d, "bold red");
        assert!(bold.props.bold);
        assert_eq!(bold.props.color.as_deref(), Some("FF0000"));
        assert_eq!(bold.props.font.as_deref(), Some("Georgia"));
        assert_eq!(run_of(&d, "big").props.size_half_pts, Some(40));
    }

    #[test]
    fn the_cascade_order() {
        let d = doc("<style>p {color:red} .a {color:green} #x {color:blue} \
             .imp {color:purple !important} span {font-weight:bold}</style>\
             <p>red</p><p class=a>green</p><p class=a id=x>blue</p>\
             <p class=a style='color:orange'>orange</p>\
             <p class=imp style='color:orange'>purple</p>\
             <p class=imp style='color:teal !important'>teal</p>\
             <p><span style='font-weight:normal'>inline wins</span></p>");
        for (text, want) in [
            ("red", "FF0000"),
            ("green", "008000"),
            ("blue", "0000FF"),
            ("orange", "FFA500"),
            ("purple", "800080"),
            ("teal", "008080"),
        ] {
            assert_eq!(
                run_of(&d, text).props.color.as_deref(),
                Some(want),
                "{text}"
            );
        }
        assert!(!run_of(&d, "inline wins").props.bold);
    }

    #[test]
    fn font_attributes_and_css_over_them() {
        let d = doc(
            "<p><font color=ff0000 face='Verdana, Arial' size=5>font</font> \
             <font color=red size=+1 style='color:#00f;font-size:8pt'>css</font> \
             <font size=1>tiny</font></p>",
        );
        let f = run_of(&d, "font");
        assert_eq!(f.props.color.as_deref(), Some("FF0000"));
        assert_eq!(f.props.font.as_deref(), Some("Verdana"));
        assert_eq!(f.props.size_half_pts, Some(36));
        let c = run_of(&d, "css");
        assert_eq!(c.props.color.as_deref(), Some("0000FF"));
        assert_eq!(c.props.size_half_pts, Some(16));
        assert_eq!(run_of(&d, "tiny").props.size_half_pts, Some(15));
    }

    #[test]
    fn backgrounds_shade_runs_or_paragraphs() {
        let d = doc(
            "<html style='background:#123456'><body style='background-color:yellow'>\
             <p>page</p><p style='background:#EEE'>para <span style='background:url(x) #ff0'>run</span></p>\
             <div style='background:silver'><p>inner</p></div></body></html>",
        );
        let page = para_of(&d, "page");
        assert!(
            page.props.raw_props.is_empty(),
            "{:?}",
            page.props.raw_props
        );
        assert!(run_of(&d, "page").props.raw_props.is_empty());
        let para = para_of(&d, "para");
        assert!(has_shd(&para.props.raw_props, "EEEEEE"));
        assert!(run_of(&d, "para").props.raw_props.is_empty());
        assert!(has_shd(&run_of(&d, "run").props.raw_props, "FFFF00"));
        assert!(has_shd(&para_of(&d, "inner").props.raw_props, "C0C0C0"));
    }

    #[test]
    fn highlight_takes_word_s_names_only() {
        let d = doc(
            "<p><span style='mso-highlight:Yellow'>yes</span> <span style='mso-highlight:fuchsia'>no</span> \
             <span style='mso-highlight:darkblue'>dark</span></p>",
        );
        assert_eq!(run_of(&d, "yes").props.highlight.as_deref(), Some("yellow"));
        assert_eq!(run_of(&d, "no").props.highlight, None);
        assert_eq!(
            run_of(&d, "dark").props.highlight.as_deref(),
            Some("darkBlue")
        );
    }

    #[test]
    fn windowtext_resets_the_colour_and_inherit_keeps_it() {
        let d = doc(
            "<div style='color:red'><p><span style='color:windowtext'>black</span> \
             <span style='color:inherit'>still red</span> <span style='color:transparent'>red too</span></p></div>",
        );
        assert_eq!(run_of(&d, "black").props.color, None);
        assert_eq!(
            run_of(&d, "still red").props.color.as_deref(),
            Some("FF0000")
        );
        assert_eq!(run_of(&d, "red too").props.color.as_deref(), Some("FF0000"));
    }

    #[test]
    fn margins_become_spacing_and_indent() {
        let d = doc(
            "<div style='margin-left:20px;padding-left:10px;margin-right:1in'>\
             <p style='margin:6pt 0 12pt;text-indent:-.25in'>outer</p>\
             <blockquote><p>quoted</p><blockquote style='margin-left:0'><p>deeper</p></blockquote></blockquote></div>\
             <p style='margin-left:-50px'>negative</p><p style='margin:5%'>percent</p>",
        );
        let outer = para_of(&d, "outer");
        assert_eq!(outer.props.spacing.before, Some(120));
        assert_eq!(outer.props.spacing.after, Some(240));
        assert_eq!(outer.props.indent, 450);
        assert_eq!(outer.props.first_line, -360);
        assert_eq!(outer.props.indent_right, 1440);
        // The blockquote's default 40 px each side, on top of the div's.
        let quoted = para_of(&d, "quoted");
        assert_eq!(quoted.props.indent, 450 + 600);
        assert_eq!(quoted.props.indent_right, 1440 + 600);
        assert_eq!(quoted.props.spacing.before, None);
        let deeper = para_of(&d, "deeper");
        assert_eq!(deeper.props.indent, 450 + 600);
        assert_eq!(deeper.props.indent_right, 1440 + 600 + 600);
        assert_eq!(para_of(&d, "negative").props.indent, 0);
        let pc = para_of(&d, "percent");
        assert_eq!((pc.props.indent, pc.props.spacing.before), (0, None));
    }

    #[test]
    fn the_page_box_is_not_paragraph_indent() {
        let d = doc(
            "<html style='margin:1in'><body style='padding:20px;margin:10px'><p>text</p></body></html>",
        );
        let p = para_of(&d, "text");
        assert_eq!((p.props.indent, p.props.indent_right), (0, 0));
        assert_eq!(
            (p.props.spacing.before, p.props.spacing.after),
            (None, None)
        );
    }

    #[test]
    fn line_height_forms() {
        let d = doc(
            "<p style='line-height:115%'>pct</p><p style='line-height:1.5'>mult</p>\
             <p style='line-height:18pt'>least</p>\
             <p style='line-height:12.0pt;mso-line-height-rule:exactly'>exact</p>\
             <div style='line-height:2'><p style='line-height:normal'>normal</p><p>inherited</p></div>\
             <p style='font:bold 10pt/14pt Arial'>short</p>",
        );
        let sp = |t: &str| {
            let p = para_of(&d, t);
            (p.props.spacing.line, p.props.spacing.line_rule)
        };
        assert_eq!(sp("pct"), (Some(276), Some("auto".into())));
        assert_eq!(sp("mult"), (Some(360), Some("auto".into())));
        assert_eq!(sp("least"), (Some(360), Some("atLeast".into())));
        assert_eq!(sp("exact"), (Some(240), Some("exact".into())));
        assert_eq!(sp("normal"), (None, None));
        assert_eq!(sp("inherited"), (Some(480), Some("auto".into())));
        assert_eq!(sp("short"), (Some(280), Some("atLeast".into())));
        let r = run_of(&d, "short");
        assert!(r.props.bold);
        assert_eq!(r.props.font.as_deref(), Some("Arial"));
        assert_eq!(r.props.size_half_pts, Some(20));
    }

    #[test]
    fn an_implicit_close_ends_a_paragraph_only_with_content() {
        let d = doc("<p style='color:red'>a<div>b</div>");
        assert_eq!(paragraph_texts(&d), ["a", "b"]);
        assert_eq!(run_of(&d, "b").props.color, None, "no leak from the open p");
        let d = doc("<p class=x><table><tr><td>cell</td></tr></table><p>after");
        assert!(matches!(d.body[0], DocBlock::Table(_)), "{:?}", d.body[0]);
        assert_eq!(paragraph_texts(&d), ["cell", "after"]);
        let d = doc("<p style='margin-bottom:20px'>one<p>two");
        assert_eq!(paragraph_texts(&d), ["one", "two"]);
        assert_eq!(para_of(&d, "one").props.spacing.after, Some(300));
        assert_eq!(para_of(&d, "two").props.spacing.after, None);
    }

    #[test]
    fn an_li_closes_the_open_li_of_its_list_only() {
        let d = doc(
            "<ul><li style='color:red'>one<li>two<ul><li>inner</ul><li>three</ul><dl><dt style='color:red'>t<dd>d</dl>",
        );
        assert_eq!(
            paragraph_texts(&d),
            ["one", "two", "inner", "three", "t", "d"]
        );
        assert_eq!(run_of(&d, "two").props.color, None);
        assert_eq!(run_of(&d, "three").props.color, None);
        assert_eq!(run_of(&d, "d").props.color, None);
        let p = all_paras(&d);
        assert_eq!(p[2].props.ilvl, 1);
        assert_eq!(p[3].props.ilvl, 0);
    }

    #[test]
    fn list_items_keep_their_hanging_indent() {
        let d = doc(
            "<ul><li>plain</li></ul><blockquote><ul><li>quoted<ol><li>nested</li></ol></li></ul></blockquote>\
             <ul style='padding-left:0;margin-left:0'><li>unpadded</li></ul>\
             <div style='margin-left:-100px'><ul><li>pulled</li></ul></div>",
        );
        let plain = para_of(&d, "plain");
        assert_eq!((plain.props.indent, plain.props.first_line), (0, 0));
        let quoted = para_of(&d, "quoted");
        assert_eq!(
            (quoted.props.indent, quoted.props.first_line),
            (720 + 600, -360)
        );
        let nested = para_of(&d, "nested");
        assert_eq!(
            (nested.props.indent, nested.props.first_line),
            (1440 + 600, -360)
        );
        let unpadded = para_of(&d, "unpadded");
        assert_eq!((unpadded.props.indent, unpadded.props.first_line), (0, 0));
        let pulled = para_of(&d, "pulled");
        // 720 - 1500 clamps at the margin; the hanging indent stays.
        assert_eq!((pulled.props.indent, pulled.props.first_line), (0, -360));
    }

    #[test]
    fn a_table_cell_starts_a_fresh_block_look() {
        let d = doc(
            "<div style='background:#CCCCCC;margin-left:30px;line-height:1.5'><table>\
             <tr><td style='padding:10px 20px'><p>cell</p>bare</td></tr></table><p>outside</p></div>",
        );
        let cell = para_of(&d, "cell");
        assert!(
            cell.props.raw_props.is_empty(),
            "{:?}",
            cell.props.raw_props
        );
        assert_eq!((cell.props.indent, cell.props.indent_right), (0, 0));
        assert_eq!(cell.props.spacing.line, Some(360), "line height inherits");
        let bare = para_of(&d, "bare");
        assert_eq!(bare.props.indent, 0);
        assert_eq!(bare.props.spacing.before, None);
        let outside = para_of(&d, "outside");
        assert!(has_shd(&outside.props.raw_props, "CCCCCC"));
        assert_eq!(outside.props.indent, 450);
    }

    #[test]
    fn a_mis_nested_inline_close_ends_at_once() {
        // FIX r1 M2: the `b` ends at `</b>` (as on main and in a browser);
        // the `p` keeps its own look, nothing leaks past it.
        let d = doc("<b><p style='margin-bottom:10pt;color:red'>x</b>y</p><p>z</p>");
        let p = para_of(&d, "xy");
        assert_eq!(p.props.spacing.after, Some(200));
        let x = run_of(&d, "x");
        assert!(x.props.bold);
        assert_eq!(x.props.color.as_deref(), Some("FF0000"));
        let y = run_of(&d, "y");
        assert!(!y.props.bold);
        assert_eq!(y.props.color.as_deref(), Some("FF0000"));
        let z = run_of(&d, "z");
        assert!(!z.props.bold);
        assert_eq!(z.props.color, None);
        assert_eq!(para_of(&d, "z").props.spacing.after, None);
        let d = doc("<span style='color:red'><p>a</span>b</p><p>c</p>");
        assert_eq!(run_of(&d, "a").props.color.as_deref(), Some("FF0000"));
        assert_eq!(run_of(&d, "b").props.color, None);
        assert_eq!(run_of(&d, "c").props.color, None);
        // A `<style>` rule on the element above is matched again without
        // the closed one: `b p` no longer applies once the `b` is gone.
        let d = doc("<style>b p {font-size:20pt}</style><b><p>in</b>out</p>");
        assert_eq!(run_of(&d, "in").props.size_half_pts, Some(40));
        assert_eq!(run_of(&d, "out").props.size_half_pts, None);
        // `</table>` drops an unclosed cell's look.
        let d = doc(
            "<table><tr><td><span style='color:red'>in<td style='color:blue'>two</table><p>after</p>",
        );
        assert_eq!(run_of(&d, "after").props.color, None);
    }

    #[test]
    fn a_block_end_reopens_its_formatting_elements() {
        // FIX r1 m3: as on main and in a browser.
        let d = doc("<p><b>Title<p>body</b>");
        assert!(run_of(&d, "Title").props.bold);
        assert!(run_of(&d, "body").props.bold);
        let d = doc("<p><b>a</p><p>b</b>");
        assert!(run_of(&d, "b").props.bold);
        let d = doc("<ul><li><b>one<li>two</ul>");
        assert!(run_of(&d, "two").props.bold);
        let d = doc("<div><font color=red>red</div><p>still red</p>");
        assert_eq!(
            run_of(&d, "still red").props.color.as_deref(),
            Some("FF0000")
        );
        // A span is not a formatting element; a cell keeps its own.
        let d = doc("<p><span style='color:red'>a</p><p>plain</p>");
        assert_eq!(run_of(&d, "plain").props.color, None);
        let d = doc("<div><table><tr><td><b>cell</td></tr></table></div><p>after</p>");
        assert!(!run_of(&d, "after").props.bold);
        // At most three alike are reopened.
        let page = format!("<p>{}x</p><p>y", "<b>".repeat(10));
        let budget = Budget::standard();
        let mut h = Html::new(&page, &budget, StyleSheet::default());
        h.run();
        assert_eq!(h.elems.iter().filter(|e| e.key.tag == "b").count(), 3);
        assert!(run_of(&doc(&page), "y").props.bold);
    }

    #[test]
    fn a_container_close_ends_what_is_open_in_it() {
        // FIX r2 M1.
        let d = doc("<section style='color:red'><p>one</section><p>two</p><p>three</p>");
        assert_eq!(run_of(&d, "one").props.color.as_deref(), Some("FF0000"));
        assert_eq!(run_of(&d, "two").props.color, None);
        assert_eq!(run_of(&d, "three").props.color, None);
        // A container closes an open `p` as it opens, so the inner `p` does
        // not drain it.
        let d = doc("<p>intro<section style='color:red'><p>inside</p>");
        assert_eq!(paragraph_texts(&d), ["intro", "inside"]);
        assert_eq!(run_of(&d, "inside").props.color.as_deref(), Some("FF0000"));
        let d =
            doc("<style>.post p {color:red}</style><article class=post><p>in</article><p>out</p>");
        assert_eq!(run_of(&d, "in").props.color.as_deref(), Some("FF0000"));
        assert_eq!(run_of(&d, "out").props.color, None);
        // A custom tag over an open paragraph, and containers as paragraph
        // bounds.
        let d = doc("<x-box style='color:red'><p>a</x-box><p>b</p>");
        assert_eq!(run_of(&d, "b").props.color, None);
        let d = doc("<section>one</section><section>two</section><p>a<hr>b");
        assert_eq!(paragraph_texts(&d), ["one", "two", "a", "b"]);
        // `</body>` never ends the page's look.
        let d = doc("<body style='color:red'><p>a</body>b");
        assert_eq!(run_of(&d, "b").props.color.as_deref(), Some("FF0000"));
    }

    #[test]
    fn reopened_formatting_elements_do_not_pile_up() {
        // FIX r2 M2: three alike at most, those still open below counted.
        // A browser nests at most four `small`s here (three reconstructed,
        // one new): 9.6, 7.5, 6, then 4.8pt each time.
        let mut page = String::from("<style>small{font-size:80%}</style>");
        for k in 0..6 {
            page.push_str(&format!("<p><small>s{k}</p>"));
        }
        let d = doc(&page);
        let sizes: Vec<Option<u32>> = (0..6)
            .map(|k| run_of(&d, &format!("s{k}")).props.size_half_pts)
            .collect();
        assert_eq!(
            sizes,
            [Some(19), Some(15), Some(12), Some(10), Some(10), Some(10)]
        );
        let mut page = String::new();
        for k in 0..300 {
            page.push_str(&format!("<p><b>b{k}</p>"));
        }
        page.push_str("<p style='color:red'>last <span style='font-size:30pt'>big</span></p>");
        let budget = Budget::standard();
        let mut h = Html::new(&page, &budget, StyleSheet::default());
        h.run();
        assert!(h.elems.len() <= 4, "{}", h.elems.len());
        let d = doc(&page);
        let big = run_of(&d, "big");
        assert_eq!(big.props.size_half_pts, Some(60));
        assert!(big.props.bold, "the <b>s were never closed");
        assert_eq!(big.props.color.as_deref(), Some("FF0000"));
    }

    #[test]
    fn an_inline_close_reopens_what_it_ended() {
        // As a browser's adoption agency: `</span>` pops the `b`, which
        // comes back without the span's colour; `</b>` pops the `i`.
        let d = doc("<p><span style='color:red'><b>a</span>b</b> <b><i>x</b>y</i>z</p>");
        let b = run_of(&d, "b");
        assert!(b.props.bold);
        assert_eq!(b.props.color, None);
        let y = run_of(&d, "y");
        assert!(y.props.italic && !y.props.bold);
        let z = run_of(&d, "z");
        assert!(!z.props.italic && !z.props.bold);
    }

    #[test]
    fn closes_never_reach_across_a_table() {
        // FIX r2 m1: an unclosed cell's `b` does not leave the table.
        let d = doc("<div><table><tr><td><b>cell</table><p>after</p></div><p>out</p>");
        assert!(run_of(&d, "cell").props.bold);
        assert!(!run_of(&d, "after").props.bold);
        assert!(!run_of(&d, "out").props.bold);
        // `</div>` in a cell is ignored, as in a browser: `after` is still in
        // the cell, in its `b`.
        let d = doc("<div><table><tr><td><b>cell</div><p>after</p>");
        let DocBlock::Table(t) = &d.body[0] else {
            panic!("{:?}", d.body[0])
        };
        let cells: Vec<String> = t
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .flat_map(|c| &c.blocks)
            .filter_map(|b| match b {
                DocBlock::Paragraph(p) => Some(p.plain_text()),
                _ => None,
            })
            .collect();
        assert_eq!(cells, ["cell", "after"]);
        assert!(run_of(&d, "after").props.bold);
        // FIX r2 m2: `</ul>` in a cell is ignored; the item goes on after
        // the table.
        let d = doc("<ul><li>a<table><tr><td></ul>cell</td></tr></table>x</li></ul>");
        let DocBlock::Table(t) = d
            .body
            .iter()
            .find(|b| matches!(b, DocBlock::Table(_)))
            .unwrap()
        else {
            unreachable!()
        };
        let in_cell: Vec<String> = t
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .flat_map(|c| &c.blocks)
            .filter_map(|b| match b {
                DocBlock::Paragraph(p) => Some(p.plain_text()),
                _ => None,
            })
            .collect();
        assert!(in_cell.iter().any(|t| t == "cell"), "{in_cell:?}");
        assert_eq!(para_of(&d, "x").props.num_id, Some(1));
    }

    /// The cells of the first table of `d`, row by row, as their text.
    fn table_texts(d: &Document) -> Vec<Vec<String>> {
        let Some(DocBlock::Table(t)) = d.body.iter().find(|b| matches!(b, DocBlock::Table(_)))
        else {
            panic!("no table: {:?}", d.body)
        };
        t.rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| {
                        c.blocks
                            .iter()
                            .filter_map(|b| match b {
                                DocBlock::Paragraph(p) => Some(p.plain_text()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn an_omitted_cell_or_row_end_is_implied() {
        // FIX r3 M1: the next cell is not inside the previous one.
        let d = doc("<table><tr><td style='color:#fff;background:#003'>Name<td>Value</tr></table>");
        assert_eq!(table_texts(&d), [["Name", "Value"]]);
        assert_eq!(run_of(&d, "Name").props.color.as_deref(), Some("FFFFFF"));
        assert_eq!(run_of(&d, "Value").props.color, None);
        let d = doc("<table><tr><td>a<tr><td>b</table>");
        assert_eq!(table_texts(&d), [["a"], ["b"]]);
        let d = doc("<table><thead><tr><th>h<tbody><tr><td>b<tfoot><tr><td>f</table>");
        assert_eq!(table_texts(&d), [["h"], ["b"], ["f"]]);
        // 300 cells without their ends: the stack does not fill up.
        let mut page = String::from("<table><tr>");
        for k in 0..300 {
            page.push_str(&format!("<td>c{k}"));
        }
        page.push_str("<td><b>bold</b></table>");
        let budget = Budget::standard();
        let mut h = Html::new(&page, &budget, StyleSheet::default());
        h.run();
        assert!(h.elems.len() <= 1, "{}", h.elems.len());
        let d = doc(&page);
        assert!(run_of(&d, "bold").props.bold);
        let rows = table_texts(&d);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), crate::import::MAX_COLUMNS);
        // A nested table without its `</td>` stays in the outer cell.
        let d = doc(
            "<table><tr><td>outer<table><tr><td>in1<td>in2</table>more</td><td>next</td></tr></table>",
        );
        let rows = table_texts(&d);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].len(), 2, "{rows:?}");
        assert!(rows[0][0].contains("more"), "{rows:?}");
        assert_eq!(rows[0][1], "next");
    }

    #[test]
    fn a_close_ends_the_lists_and_blocks_opened_in_it() {
        // FIX r3 m2: a container's.
        let d = doc("<section><h1>T</section><p>body</p>");
        assert_eq!(para_of(&d, "body").props.style_id, None);
        let d = doc("<section><ul><li>a</section><ul><li>b</li></ul>");
        assert_eq!(para_of(&d, "b").props.ilvl, 0);
        // FIX r3: a block's and a cell's (pre-existing: `lists` stayed).
        let d = doc("<div><ul><li>a</div><p>after</p><ul><li>b</li></ul>");
        assert_eq!(para_of(&d, "after").props.num_id, None);
        assert_eq!(para_of(&d, "b").props.ilvl, 0);
        let d = doc("<table><tr><td><ul><li>a</td></tr></table><ul><li>b</li></ul>");
        assert_eq!(para_of(&d, "b").props.ilvl, 0);
    }

    #[test]
    fn list_ends_follow_list_item_scope() {
        // FIX r4 m1: a stray `</li>` under an open list is ignored.
        let d = doc("<ul><li>a<ul><li>b</li></li><li>c</li></ul><li>d</li></ul>");
        assert_eq!(para_of(&d, "c").props.ilvl, 1);
        assert_eq!(para_of(&d, "d").props.ilvl, 0);
        // FIX r4 p1: a list's end ends the lists left open inside it.
        let d = doc("<ol><li>a<ul><li>b</ol><p>after</p><ul><li>c</li></ul>");
        assert_eq!(para_of(&d, "after").props.num_id, None);
        assert_eq!(para_of(&d, "c").props.ilvl, 0);
        let d = doc("<ul><li>a<ol><li>b</ul><p>x</p><ul><li>c</li></ul>");
        assert_eq!(para_of(&d, "x").props.num_id, None);
        assert_eq!(para_of(&d, "c").props.ilvl, 0);
        // Well-formed nesting is unchanged.
        let d = doc("<ul><li>a<ol><li>b</li></ol></li><li>c</li></ul><ol><li>d</li></ol>");
        let p = all_paras(&d);
        let got: Vec<(Option<i32>, i32)> =
            p.iter().map(|p| (p.props.num_id, p.props.ilvl)).collect();
        assert_eq!(
            got,
            [(Some(1), 0), (Some(2), 1), (Some(1), 0), (Some(2), 0)]
        );
    }

    #[test]
    fn a_row_group_end_ends_its_cell_and_row() {
        // FIX r4 p2.
        let d = doc("<table><thead><tr><th>h</thead><tbody><tr><td>b</td></tr></tbody></table>");
        assert_eq!(table_texts(&d), [["h"], ["b"]]);
    }

    #[test]
    fn mso_highlight_none_clears_it() {
        // FIX r4 m2.
        let d = doc(
            "<p><span style='mso-highlight:yellow'>a<span style='mso-highlight:none'>b</span></span></p>",
        );
        assert_eq!(run_of(&d, "a").props.highlight.as_deref(), Some("yellow"));
        assert_eq!(run_of(&d, "b").props.highlight, None);
    }

    #[test]
    fn an_ancestor_s_not_bold_is_no_explicit_off_on_a_heading() {
        // FIX r1 M1.
        let off = "<w:b w:val=\"0\"/>".to_string();
        let d = doc("<div style='font:12px Arial'><h1>Title</h1></div>");
        assert!(run_of(&d, "Title").props.raw_props.is_empty());
        let d = doc("<style>body{font:14px/1.5 Arial}</style><h2>x</h2><p class=MsoHeading3>y</p>");
        assert!(run_of(&d, "x").props.raw_props.is_empty());
        assert!(run_of(&d, "y").props.raw_props.is_empty());
        let d = doc(
            "<div style='font-weight:normal'><h1 style='font-weight:normal'>own</h1><h2><span style='font-weight:400'>inner</span></h2></div>",
        );
        assert_eq!(
            run_of(&d, "own").props.raw_props,
            std::slice::from_ref(&off)
        );
        assert_eq!(run_of(&d, "inner").props.raw_props, [off]);
    }

    #[test]
    fn negative_spacing_and_line_height_are_not_written() {
        // FIX r1 M3: ST_TwipsMeasure is unsigned.
        let d = doc(
            "<p style='margin-top:-10px;margin-bottom:-1pt;padding-bottom:0'>neg</p>\
             <p style='margin-top:-10px;padding-top:15px'>sum</p>\
             <p style='line-height:-5px'>line</p><p style='line-height:0'>zero</p>",
        );
        let neg = para_of(&d, "neg");
        assert_eq!(
            (neg.props.spacing.before, neg.props.spacing.after),
            (Some(0), Some(0))
        );
        assert_eq!(para_of(&d, "sum").props.spacing.before, Some(75));
        assert_eq!(para_of(&d, "line").props.spacing.line, None);
        assert_eq!(para_of(&d, "zero").props.spacing.line, None);
    }

    #[test]
    fn a_font_size_attribute_never_overflows() {
        // FIX r1 m1.
        let d = doc(
            "<p><font size=\"+9223372036854775807\">big</font> <font size=\"-999999999999999999999\">small</font></p>",
        );
        assert_eq!(run_of(&d, "big").props.size_half_pts, Some(72));
        assert_eq!(run_of(&d, "small").props.size_half_pts, Some(15));
    }

    #[test]
    fn a_list_end_ends_its_open_item() {
        // FIX r1: the unclosed `li` no longer makes what follows a bullet.
        let d = doc("<ul><li>a<li>b</ul><p>after</p>");
        assert_eq!(para_of(&d, "after").props.num_id, None);
        let d = doc("<ol><li>a<ol></ol>still</li></ol><p>after</p>");
        assert_eq!(para_of(&d, "still").props.num_id, Some(2));
        assert_eq!(para_of(&d, "after").props.num_id, None);
        let d = doc("<ul><li><table><tr><td><ul></ul>cell</td></tr></table>x</li></ul>");
        assert_eq!(para_of(&d, "x").props.num_id, Some(1));
    }

    #[test]
    fn a_style_block_applies_to_the_whole_page() {
        // In the body, after the elements it styles; in a comment or a
        // script it is not a style block.
        let d = doc("<p class=late>late</p><style>.late {color:#0A0B0C}</style>\
             <!-- <style>p {font-size:40pt}</style> --><script>var s='<style>p {color:red}</style>';</script>");
        let r = run_of(&d, "late");
        assert_eq!(r.props.color.as_deref(), Some("0A0B0C"));
        assert_eq!(r.props.size_half_pts, None);
        assert_eq!(paragraph_texts(&d), ["late"]);
    }

    #[test]
    fn unsupported_selectors_are_never_text() {
        let d = doc(
            "<style>a:hover, p.k {color:red} h1 + p {color:blue} @media (max-width:600px) { p.k {color:green} }\
             p[lang] {color:blue} .k::after {content:'x'}</style><p class=k>kept</p>",
        );
        assert_eq!(paragraph_texts(&d), ["kept"]);
        assert_eq!(run_of(&d, "kept").props.color.as_deref(), Some("FF0000"));
    }

    #[test]
    fn hidden_text_is_vanish() {
        let d = doc(
            "<div style='display:none;max-height:0;mso-hide:all'>Preheader text</div>\
             <p><span style='mso-hide:all'>outlook only</span>shown</p>",
        );
        assert!(run_of(&d, "Preheader").props.vanish);
        assert!(run_of(&d, "outlook only").props.vanish);
        assert!(!run_of(&d, "shown").props.vanish);
    }

    #[test]
    fn bold_off_is_explicit_under_a_heading_only() {
        let d = doc(
            "<style>h1 {font-weight:normal}</style><h1>head <b>bold</b></h1>\
             <p><span style='font-weight:normal'>body</span></p><h2>default</h2>",
        );
        let off = "<w:b w:val=\"0\"/>".to_string();
        let head = run_of(&d, "head");
        assert!(!head.props.bold);
        assert_eq!(head.props.raw_props, [off]);
        let bold = run_of(&d, "bold");
        assert!(bold.props.bold);
        assert!(bold.props.raw_props.is_empty());
        assert!(run_of(&d, "body").props.raw_props.is_empty());
        assert!(run_of(&d, "default").props.raw_props.is_empty());
    }

    #[test]
    fn hostile_style_sheets_are_bounded() {
        // A selector of 200 compounds is dropped alone; the other rules
        // still apply.
        let long = vec!["div"; 200].join(" ");
        let d = doc(&format!(
            "<style>{long} p {{color:red}} p {{color:blue}}</style><div><p>x</p></div>"
        ));
        assert_eq!(run_of(&d, "x").props.color.as_deref(), Some("0000FF"));
        // A 32-compound descendant selector over a stack at MAX_DEPTH:
        // polynomial, within the standard budget.
        let sel = vec!["div"; css::MAX_COMPOUNDS].join(" ");
        let page = format!(
            "<style>{sel} {{color:red}}</style>{}<p>deep</p>",
            "<div>".repeat(MAX_DEPTH)
        );
        let d = doc(&page);
        assert_eq!(run_of(&d, "deep").props.color.as_deref(), Some("FF0000"));
        // 100 000 rules: 10 000 kept, the rest ignored, still a document.
        let mut css_text = String::new();
        for i in 0..100_000 {
            css_text.push_str(&format!(".c{i} {{color:red}}\n"));
        }
        let page = format!("<style>{css_text}</style><p class=c5>five</p><p class=c50000>late</p>");
        let budget = Budget::standard();
        let sheet = StyleSheet::parse(&collect_styles(&page), &budget);
        assert_eq!(sheet.len(), css::MAX_RULES);
        let d = doc(&page);
        assert_eq!(run_of(&d, "five").props.color.as_deref(), Some("FF0000"));
        assert_eq!(run_of(&d, "late").props.color, None);
        // Many universal rules against many elements run out of work, not
        // time.
        let page = format!(
            "<style>{}</style>{}",
            format!("* {} {{color:red}}\n", vec!["*"; 31].join(" ")).repeat(10_000),
            "<div><span>x</span></div>".repeat(5_000)
        );
        let small = Budget::new(1 << 20, 2_000_000);
        assert_eq!(
            import_html_within(page.as_bytes(), &small).unwrap_err(),
            TOO_BIG
        );
    }

    #[test]
    fn every_prefix_of_a_styled_page_imports() {
        let page = "<html><head><style>p.x>b{color:red}@media x{p{}}/*c*/ .y {font:bold 9pt/1.2 \"A B\"}</style></head>\
             <body style='color:blue'><p class=x style='margin:1px 2px;line-height:1.5'><b>A</b><font color=red size=+2>B</font>\
             <span style='background:rgb(1,2,3);mso-highlight:yellow'>C</span></p><blockquote><ul><li>D<li>E</ul></blockquote>\
             <table><tr><td style='padding:3px'>F</td></tr></table></body></html>";
        for i in 0..=page.len() {
            let _ = import_html(&page.as_bytes()[..i]);
        }
    }
}
