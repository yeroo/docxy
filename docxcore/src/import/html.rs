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
//! `pre`. Forms are not converted.
//!
//! Links (#1879): an `<a href>` to an absolute `http`, `https`, `ftp` or
//! `mailto` URL is a hyperlink to it, and `href="#name"` an anchor link to
//! `name`; any other `href` (`javascript:`, `data:`, `file:`, relative, …)
//! leaves plain text ([`link_of`]). The link is part of the computed look,
//! so it follows the cascade, the reopened formatting elements and
//! mis-nesting as formatting does; an `a` ends an open `a` first. A link
//! has a browser's look (underline, Word's Hyperlink blue) under the page's
//! CSS. A break, tab or picture inside a link is written outside it. The
//! saved package gets the links' relationships.
//!
//! Images (#1880) only through [`import_html_with`] ([`html_image`]): an
//! `<img>` is an inline picture sized by its `width` / `height` (attributes,
//! CSS lengths over them; `auto` clears a side, a percentage leaves the
//! attribute's) and its pixels, scaled down to the body width. A `data:`
//! src is decoded, any other goes to the caller's resolver; one that gives
//! no PNG, JPEG, GIF or BMP is a grey placeholder of the declared size with
//! its `alt` as Alt Text, or, unsized, the `alt` text itself. Word's
//! `<![if !vml]>` image is read (its VML twin in a comment is not); a
//! hidden one, a zero-sized one and one in a list marker make nothing.
//!
//! Tables (#1878) are laid out by [`html_table`]: `colspan` / `rowspan`,
//! the table's and cells' `width` (attribute or CSS), cell shading from the
//! cell's `bgcolor` / CSS background else its row's, row group's or
//! table's, `valign` / `vertical-align` (inherited from the row, a cell
//! with none `center`), a cell's `padding` as its margins, and the table's
//! `border`, `cellpadding`, `cellspacing` and `align`. Nested tables stay
//! nested, to [`MAX_TABLE_NESTING`] levels.
//!
//! Centring (#1922): a table's `w:jc` comes from its `align=left|right`
//! (a float, first), else its auto `margin-left`/`margin-right` (`<table
//! align=center>` is that hint), else its container's `-webkit-` alignment
//! (`<center>`, `align` on a `div`, `p`, `caption` or table part,
//! `text-align: -webkit-center`); plain `text-align` moves no box. A
//! `display: block` image is a paragraph of its own, aligned to its place
//! by the same rule, and a body block with an absolute `width` and both
//! side margins auto is indented evenly.

use super::css::{self, Color, Decl, ElemKey, Len, LineHeight, Path, StyleSheet};
use super::html_image::{self, ImageResolver, Images, ImportedMedia, Source};
use super::html_table::{self, CellSpec, MAX_TABLE_NESTING, TableSpec, VAlign, Width};
use super::{Budget, Builder, Link, MAX_DEPTH, TOO_BIG, heading_props, marker_is_numbered};
use crate::model::{Align, Block as DocBlock, Document, Inline, ParProps, RunProps, VertAlign};
use crate::package::Package;
use std::rc::Rc;

/// The longest tag name or list marker kept, in bytes.
const MAX_TAG: usize = 32;

/// The longest link target or anchor kept (#1879), in bytes, once
/// normalized; a longer `href` is not a link.
pub(crate) const MAX_HREF: usize = 4096;

/// The URL schemes an `href` may name to become a link (#1879), lowercase:
/// an allow-list, so `javascript:`, `data:`, `file:` and any other scheme,
/// however spelled, stays text.
const LINK_SCHEMES: [&str; 4] = ["http", "https", "ftp", "mailto"];

/// The colour of a link's user-agent look (#1879): Word's Hyperlink style.
const LINK_COLOR: &str = "0563C1";

/// Read an HTML page. `Err` when it holds no text, or would cost more
/// than an import may ([`TOO_BIG`]).
/// Images are not converted: a [`Document`] alone cannot hold their media
/// ([`import_html_with`] does).
pub fn import_html(bytes: &[u8]) -> Result<Document, String> {
    import_html_within(bytes, &Budget::standard(), None).map(|(doc, _)| doc)
}

/// An HTML page read with its images (#1880): the document, whose pictures
/// name import-local rids, and the media they name.
#[derive(Debug)]
pub struct HtmlImport {
    pub document: Document,
    pub media: Vec<ImportedMedia>,
}

impl HtmlImport {
    /// The document in a fresh Markdown package (the styles and lists an
    /// import uses, as [`crate::package::new_markdown_package`]) holding its
    /// media: each medium a `word/media/imageN.<ext>` part of
    /// `word/document.xml`, its pictures renamed to the relationship's id.
    /// Each medium's bytes are dropped as soon as the package holds them.
    pub fn package(self) -> Package {
        let HtmlImport {
            mut document,
            media,
        } = self;
        let mut pkg = crate::package::new_markdown_package(Document::default());
        let mut rids = std::collections::HashMap::new();
        for m in media {
            if let Some(rid) = pkg.add_part_media(
                "word/document.xml",
                &m.bytes,
                m.info.ext,
                m.info.content_type,
            ) {
                rids.insert(m.rid, rid);
            }
        }
        rename_embeds(&mut document.body, &rids);
        pkg.document = document;
        pkg
    }
}

/// Every picture run in `blocks` (table cells included) with its rid
/// renamed by `map`.
fn rename_embeds(blocks: &mut [DocBlock], map: &std::collections::HashMap<String, String>) {
    for b in blocks {
        match b {
            DocBlock::Paragraph(p) => {
                for inline in &mut p.content {
                    if let Inline::Raw(raw) = inline {
                        if let Some(new) = crate::image::replace_inline_embeds(raw, map) {
                            *raw = new;
                        }
                    }
                }
            }
            DocBlock::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        rename_embeds(&mut cell.blocks, map);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Read an HTML page with its images (#1880): a `data:` src is decoded,
/// any other is asked of `resolve` ([`ImageResolver`]). `Err` as
/// [`import_html`]'s.
pub fn import_html_with(
    bytes: &[u8],
    resolve: &mut ImageResolver<'_>,
) -> Result<HtmlImport, String> {
    let (document, media) = import_html_within(bytes, &Budget::standard(), Some(resolve))?;
    Ok(HtmlImport { document, media })
}

/// [`import_html`] against `budget`, with images when `images` gives a
/// resolver. Decoding, the `<style>` scan and the tokenizer are each one
/// forward pass over the input (charged once here); the style sheet is
/// charged as it is parsed and matched; the element stacks are at most
/// [`MAX_DEPTH`] deep, so a closing tag's search is bounded; the document is
/// charged as it is built, and images as [`html_image`] says.
pub(crate) fn import_html_within(
    bytes: &[u8],
    budget: &Budget,
    images: Option<&mut ImageResolver<'_>>,
) -> Result<(Document, Vec<ImportedMedia>), String> {
    // Three passes: decoding, the `<style>` scan, then the tokenizer.
    if !budget.work(3 * (bytes.len() / 16 + 1)) {
        return Err(TOO_BIG.into());
    }
    let text = decode(bytes);
    let sheet = StyleSheet::parse(&collect_styles(&text), budget);
    let mut h = Html::new(&text, budget, sheet);
    if let Some(resolve) = images {
        h.images = Some(Images::new(resolve));
    }
    h.run();
    // Cards left open at the end of the page end as `</div>` would end them
    // (#1927): an empty one is dropped.
    h.end_cards_from(0);
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    if h.out.is_empty() {
        return Err("the HTML file holds no text".into());
    }
    let media = h.images.take().map(Images::into_media).unwrap_or_default();
    let props = h.par_props();
    let doc = h.out.finish(props);
    if budget.exhausted() {
        return Err(TOO_BIG.into());
    }
    Ok((doc, media))
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
    /// On an `li`'s own entry (#1922): the item has ended a numbered
    /// paragraph, and a block image split its text, so its paragraphs past
    /// the numbered one are continuations. Per item, so a nested list's
    /// items leave their parent's alone.
    item_numbered: bool,
    item_split: bool,
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
    /// Where a block-level child is placed when its own margins do not say
    /// (#1922): `Center` or `Right` from an inherited `-webkit-center` /
    /// `-webkit-right` (`<center>`, `align` on a `div`, `p`, `caption` or
    /// table part), else `None`. A plain `text-align` clears it.
    block_align: Option<Align>,
    /// Inside a table cell (inherited): a fixed-width block's centring
    /// needs the body width, unknown there (#1922).
    in_cell: bool,
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
    /// A cell's or table part's own background (`RRGGBB`, not inherited):
    /// the cell shading (#1878).
    own_fill: Option<String>,
    /// A table's, cell's or image's own `width` (not inherited).
    own_width: Option<Width>,
    /// An image's own `height` (not inherited; only `img` reads one).
    own_height: Option<Width>,
    /// An image's own `border-radius` (not inherited, #1926): the crop
    /// shape of its picture. Only `img` reads one.
    own_radius: Option<html_image::Radius>,
    /// A table's or block image's own place in its line (not inherited,
    /// #1922): `Center`, `Right`, else `None` (the left), by its auto
    /// margins, else its parent's [`Look::block_align`].
    own_place: Option<Align>,
    /// An image is `display: block` (not inherited, #1922): a paragraph of
    /// its own, placed by `own_place`.
    own_block: bool,
    /// A `div` with its own background in a table cell is a card (not
    /// inherited, #1927): a one-cell table of its fill, padding, width and
    /// place ([`Html::begin_card`]).
    own_card: bool,
    /// A cell's own `padding`, in twips, top, right, bottom, left (not
    /// inherited): its `tcMar`.
    own_padding: [Option<i32>; 4],
    /// `valign` / `vertical-align` of a row group, row or cell, inherited
    /// by the cells below and reset at a `table`.
    valign: Option<VAlign>,
    /// The link the text is in (#1879): set by an `a` (none when its
    /// `href` is not a usable link), inherited. Shared, so a look's clone
    /// stays O(1) whatever the target's length.
    link: Option<Rc<Link>>,
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
    /// Whether it may be, is, or was a card (#1927): `Yes` while its
    /// table is open in the builder, ended once by [`Html::end_cards_from`].
    card: CardMode,
}

/// Whether an element may be a card (#1927), decided once, when it opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardMode {
    /// Not a candidate.
    No,
    /// A `div` opening in a cell: a card if its look has a background and
    /// is shown as a block.
    Maybe,
    /// Opened as a card, its table at this `table_depth` in the builder:
    /// a restyle keeps it one.
    Yes(usize),
    /// A candidate that is no card (or a card that has ended): `none`
    /// still clears its background, but it never becomes one.
    Declined,
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
    width: Option<String>,
    bgcolor: Option<String>,
    border: Option<String>,
    cellpadding: Option<String>,
    cellspacing: Option<String>,
    colspan: Option<String>,
    rowspan: Option<String>,
    valign: Option<String>,
    src: Option<String>,
    alt: Option<String>,
    height: Option<String>,
    href: Option<String>,
}

/// One element's style, being computed: its look so far, and the box
/// values kept until its font size is known.
struct Cascade {
    look: Look,
    kind: Kind,
    parent_pt: f64,
    /// The parent's run colour: what `color: inherit` gives back (#1879:
    /// over a link's user-agent blue).
    parent_color: Option<String>,
    margin: [Option<Len>; 4],
    padding: [Option<Len>; 4],
    text_indent: Option<Len>,
    line: Option<LineHeight>,
    background: Option<String>,
    /// The element is a `table` (not a row or row group).
    is_table: bool,
    /// The element is an `img`: what reads `width` and `height` as its size.
    is_img: bool,
    /// The `auto` sides of its margin, top, right, bottom, left (#1922).
    margin_auto: [bool; 4],
    /// The parent's [`Look::block_align`]: what places this element's box
    /// when it is block-level and its margins are not auto.
    parent_block_align: Option<Align>,
    /// A block's own CSS `width`, resolved in `finish` with its final font:
    /// absolute, the fixed-width centring (#1922); a percentage too, a
    /// card's table width (#1927).
    block_width: Option<Len>,
    /// An image's corner radii, top left, top right, bottom right, bottom
    /// left (#1926): kept until its font size is known, then one
    /// [`Look::own_radius`].
    radius: [Option<Len>; 4],
    /// Whether it may be, or is, a card (#1927).
    card: CardMode,
    /// Its own `display` is neither `block` nor unset (nor `none`): an
    /// inline or inline-block `div` is no card.
    not_block: bool,
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
        "right" | "end" | "-webkit-right" | "-moz-right" => Some(Align::Right),
        "justify" => Some(Align::Justify),
        "left" | "start" | "-webkit-left" | "-moz-left" => Some(Align::Left),
        _ => None,
    }
}

/// An `align` attribute's alignment: `text-align`'s values, and `middle`,
/// HTML's other spelling of `center` (#1922).
fn align_attr(v: &str) -> Option<Align> {
    if v.trim().eq_ignore_ascii_case("middle") {
        return Some(Align::Center);
    }
    align_value(v)
}

/// A list item paragraph's left indent at `level` (0-based) inside
/// enclosing boxes summing `boxes` twips: the level's own 720 per level,
/// as the Markdown package's numbering has it (#1872 9b, #1922).
fn item_indent(level: i32, boxes: i32) -> i32 {
    (720 * (level + 1) + boxes).max(0)
}

/// Whether a `text-align` value is a vendor one (`-webkit-center`,
/// `-moz-center`): it places block-level children too (#1922).
fn prefixed_align(v: &str) -> bool {
    let v = v.trim();
    ["-webkit-", "-moz-"]
        .iter()
        .any(|p| v.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)))
}

/// The [`Look::block_align`] a `text-align` of `a` sets: a `-webkit-`
/// centre or right places block children, anything else clears it.
fn webkit_block_align(a: Align, webkit: bool) -> Option<Align> {
    match a {
        Align::Center | Align::Right if webkit => Some(a),
        _ => None,
    }
}

/// Where a block-level box sits in its line (#1922), as a browser
/// resolves its inline margins: both `auto` centre it, a left `auto` alone
/// puts it right, a right `auto` alone left; with neither, the containing
/// block's `-webkit-` alignment (`parent`) places it. `None` is the left.
fn block_position(auto: [bool; 4], parent: Option<Align>) -> Option<Align> {
    match (auto[3], auto[1]) {
        (true, true) => Some(Align::Center),
        (true, false) => Some(Align::Right),
        (false, true) => None,
        (false, false) => parent.filter(|a| matches!(a, Align::Center | Align::Right)),
    }
}

/// A `valign` / cell `vertical-align` value: `baseline` and what a cell
/// treats as baseline (`super`, `sub`, `text-top`, …) are its top.
fn valign_value(v: &str) -> Option<VAlign> {
    match v.trim().to_ascii_lowercase().as_str() {
        "top" | "baseline" | "super" | "sub" | "text-top" | "text-bottom" => Some(VAlign::Top),
        "middle" | "center" => Some(VAlign::Center),
        "bottom" => Some(VAlign::Bottom),
        _ => None,
    }
}

/// A `background` that paints nothing (`none`, `transparent`).
fn clears_background(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "none" | "transparent"
    )
}

/// A `width` attribute: digits as px, `N%`, `Npx`; anything else none.
fn width_attr(v: &str) -> Option<Width> {
    let (n, pct) = attr_length(v)?;
    Some(if pct {
        Width::Pct(n)
    } else {
        Width::Twips((n * 15.0).round().min(f64::from(html_table::MAX_TWIPS)) as u32)
    })
}

/// An `<img>`'s `width` / `height` attribute in twips (digits as px,
/// `Npx`; a percentage is none), not capped at a page's width as a table's
/// is: the picture is scaled to the body as a whole box, so its aspect
/// must be the declared one (#1880 r2).
fn img_attr_twips(v: &str) -> Option<u32> {
    let (n, pct) = attr_length(v)?;
    (!pct).then(|| saturating_twips(n * 15.0))
}

/// Twips as a `u32`, rounded, saturating at its range.
fn saturating_twips(t: f64) -> u32 {
    t.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// A length attribute's number and whether it is a percentage: digits as
/// px, `N%`, `Npx`; anything else none.
fn attr_length(v: &str) -> Option<(f64, bool)> {
    let v = v.trim();
    let (num, pct) = match v.strip_suffix('%') {
        Some(n) => (n, true),
        None => (v.strip_suffix("px").unwrap_or(v), false),
    };
    let num = num.trim_end();
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let n: f64 = num.parse().ok().filter(|n: &f64| n.is_finite())?;
    Some((n, pct))
}

/// HTML's rules for a non-negative integer: leading white space, an
/// optional `+`, digits; what follows them is ignored.
fn html_int(v: &str) -> Option<u32> {
    let v = v.trim_start();
    let v = v.strip_prefix('+').unwrap_or(v);
    let end = v.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    Some(
        v[..end]
            .parse::<u64>()
            .map_or(u32::MAX, |n| n.min(u64::from(u32::MAX)) as u32),
    )
}

/// A `<table>`'s spec from its attributes and computed look.
fn table_spec(attrs: &Attrs, look: &Look) -> TableSpec {
    TableSpec {
        width: look.own_width,
        // A bare or unreadable `border` is 1 px.
        border: attrs
            .border
            .as_deref()
            .map_or(0, |b| html_int(b).unwrap_or(1).min(1000)),
        cellpadding: attrs
            .cellpadding
            .as_deref()
            .and_then(html_int)
            .map(|n| n.min(1000)),
        cellspacing: attrs
            .cellspacing
            .as_deref()
            .and_then(html_int)
            .map(|n| n.min(1000)),
        // `align=left|right` floats the table: it decides first (#1922).
        // Otherwise its margins, else its container's `-webkit-` alignment
        // (`<table align=center>` is an auto-margins hint).
        jc: match attrs
            .align
            .as_deref()
            .map(|a| a.trim().to_ascii_lowercase())
        {
            Some(a) if a == "right" => Some("right"),
            Some(a) if a == "left" => None,
            _ => match look.own_place {
                Some(Align::Center) => Some("center"),
                Some(Align::Right) => Some("right"),
                _ => None,
            },
        },
    }
}

impl Cascade {
    fn new(parent: Option<&Look>, kind: Kind, card: CardMode) -> Self {
        let mut look = parent.cloned().unwrap_or_default();
        look.before = None;
        look.after = None;
        look.own_fill = None;
        look.own_width = None;
        look.own_height = None;
        look.own_radius = None;
        look.own_padding = [None; 4];
        look.own_place = None;
        look.own_block = false;
        look.own_card = false;
        let parent_block_align = look.block_align;
        if kind == Kind::Cell {
            look.in_cell = true;
            // A cell starts a fresh block look; what is inherited (the runs,
            // alignment, line height) still passes in.
            look.indent = 0;
            look.indent_right = 0;
            look.first_line = 0;
            look.shade = None;
        }
        let parent_pt = font_pt(&look.run);
        let parent_color = look.run.color.clone();
        Cascade {
            look,
            kind,
            parent_pt,
            parent_color,
            margin: [None; 4],
            padding: [None; 4],
            text_indent: None,
            line: None,
            background: None,
            is_table: false,
            is_img: false,
            margin_auto: [false; 4],
            parent_block_align,
            block_width: None,
            radius: [None; 4],
            card,
            not_block: false,
        }
    }

    /// A cell, row or row group: what `valign`, row `align` and cell
    /// `vertical-align` apply to.
    fn table_part(&self) -> bool {
        self.kind == Kind::Cell || (self.kind == Kind::Table && !self.is_table)
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
        if tag == "table" {
            // A table's cells align by their own rows (#1878).
            self.is_table = true;
            self.look.valign = None;
        }
        self.is_img = tag == "img";
        let run = &mut self.look.run;
        match tag {
            "b" | "strong" => self.set_bold(true),
            "i" | "em" | "cite" | "dfn" | "var" => run.italic = true,
            "u" | "ins" => run.underline = true,
            "s" | "strike" | "del" => run.strike = true,
            "sup" => run.vert_align = VertAlign::Superscript,
            "sub" => run.vert_align = VertAlign::Subscript,
            "center" => {
                self.look.align = Some(Align::Center);
                self.look.block_align = Some(Align::Center);
            }
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

    /// Presentational attributes: `align` on a block, cell, row or row
    /// group; `bgcolor` and `valign` on a table part; `width` on a table or
    /// cell; `<font color face size>`; an `a`'s `href` (#1879).
    fn presentational(&mut self, tag: &str, attrs: &Attrs) {
        if tag == "a" {
            // A link (#1879), with a browser's `a:link` look; the page's
            // CSS on or inside it overrides that. An `a` without a usable
            // `href` (`<a name>`, `javascript:`) is no link.
            self.look.link = attrs.href.as_deref().and_then(link_of).map(Rc::new);
            if self.look.link.is_some() {
                self.look.run.underline = true;
                self.look.run.color = Some(LINK_COLOR.to_string());
            }
        }
        if matches!(self.kind, Kind::Block | Kind::Cell) || self.table_part() {
            if let Some(a) = attrs.align.as_deref().and_then(align_attr) {
                self.look.align = Some(a);
                // `align` on these is `text-align: -webkit-*` (it also
                // places block-level children, #1922); on a heading, `li`
                // and the rest it is plain, which clears an inherited one.
                let webkit = matches!(tag, "div" | "p" | "caption") || self.kind == Kind::Cell;
                self.look.block_align = webkit_block_align(a, webkit || self.table_part());
            }
        }
        if self.is_table {
            // `<table align=center>` is the hint `margin-left: auto;
            // margin-right: auto` (#1922): the page's margins override it.
            // `left` and `right` are floats, `table_spec`'s.
            let center = attrs
                .align
                .as_deref()
                .is_some_and(|a| a.trim().eq_ignore_ascii_case("center"));
            if center {
                self.margin_auto[1] = true;
                self.margin_auto[3] = true;
            }
        }
        if self.kind.table() {
            if let Some(c) = attrs.bgcolor.as_deref().and_then(css::attr_color) {
                self.background = Some(c);
            }
        }
        if self.table_part() {
            if let Some(v) = attrs.valign.as_deref().and_then(valign_value) {
                self.look.valign = Some(v);
            }
        }
        if matches!(tag, "table" | "td" | "th") {
            if let Some(w) = attrs.width.as_deref().and_then(width_attr) {
                self.look.own_width = Some(w);
            }
        }
        if tag == "img" {
            // An image's box is in pixels; a percentage has no container
            // width to resolve against.
            let px = |v: Option<&str>| v.and_then(img_attr_twips).map(Width::Twips);
            self.look.own_width = px(attrs.width.as_deref());
            self.look.own_height = px(attrs.height.as_deref());
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
        let (table_part, is_table) = (self.table_part(), self.is_table);
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
                // The parent's, over a user-agent or presentational colour
                // set on this element (#1879).
                Some(Color::Keep) => run.color.clone_from(&self.parent_color),
                None => {}
            },
            "background" | "background-color" => {
                if let Some(c) = css::background(v) {
                    self.background = Some(c);
                } else if (self.kind.table() || self.card != CardMode::No) && clears_background(v) {
                    // A cell's `none` shows its row's or table's (#1878); a
                    // card candidate's is no card (#1927).
                    self.background = None;
                }
            }
            "width" | "height" if self.is_img => {
                let w = v.trim();
                let pt = font_pt(&self.look.run);
                let side = if d.name == "width" {
                    &mut self.look.own_width
                } else {
                    &mut self.look.own_height
                };
                if w.eq_ignore_ascii_case("auto") {
                    *side = None;
                } else if let Some(len) = css::length(w) {
                    // A percentage leaves the attribute's size (an e-mail's
                    // `width="600" style="width:100%"`).
                    if len.unit != css::Unit::Percent {
                        // Not capped at a page's width (#1880 r2): the
                        // box is scaled to the body as a whole.
                        let twips = len.points(pt).map(|p| (p * 20.0).round());
                        if let Some(t) = twips.filter(|&t| t >= 0.0) {
                            *side = Some(Width::Twips(saturating_twips(t)));
                        }
                    }
                }
            }
            "width" if self.kind == Kind::Cell || is_table => {
                let w = v.trim();
                if w.eq_ignore_ascii_case("auto") {
                    self.look.own_width = None;
                } else if let Some(len) = css::length(w) {
                    let pt = font_pt(&self.look.run);
                    self.look.own_width = match len.unit {
                        css::Unit::Percent => (len.v >= 0.0).then_some(Width::Pct(len.v)),
                        _ => len
                            .twips(pt)
                            .filter(|&t| t >= 0)
                            .map(|t| Width::Twips(t as u32)),
                    };
                }
            }
            "width" if self.kind == Kind::Block => {
                // A non-negative width (#1922): `auto` leaves the block as
                // wide as its container; an invalid value (a negative one,
                // `600` without a unit) leaves the width as it was. Only a
                // card reads a percentage (#1927).
                let w = v.trim();
                if w.eq_ignore_ascii_case("auto") {
                    self.block_width = None;
                } else if let Some(len) = css::length(w).filter(|l| l.v >= 0.0) {
                    self.block_width = Some(len);
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
            // A table part aligns its cells; on a cell `super` / `sub` are
            // `baseline`, its top (#1878).
            "vertical-align" if table_part => {
                if let Some(a) = valign_value(v) {
                    self.look.valign = Some(a);
                }
            }
            "vertical-align" if is_table => {}
            "vertical-align" => match v.trim().to_ascii_lowercase().as_str() {
                "super" => run.vert_align = VertAlign::Superscript,
                "sub" => run.vert_align = VertAlign::Subscript,
                "baseline" => run.vert_align = VertAlign::Baseline,
                _ => {}
            },
            "text-align" => {
                if let Some(a) = align_value(v) {
                    self.look.align = Some(a);
                    self.look.block_align = webkit_block_align(a, prefixed_align(v));
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
                        self.margin_auto = css::box_autos(v).unwrap_or_default();
                    } else {
                        self.padding = sides;
                    }
                }
            }
            "border-radius" if self.is_img => {
                // A picture's crop (#1926); other elements' rounded boxes
                // are not drawn, and the value is never text.
                if let Some(corners) = css::radius_corners(v) {
                    self.radius = corners.map(Some);
                }
            }
            "border-top-left-radius"
            | "border-top-right-radius"
            | "border-bottom-right-radius"
            | "border-bottom-left-radius"
                if self.is_img =>
            {
                let corner = match d.name.as_str() {
                    "border-top-left-radius" => 0,
                    "border-top-right-radius" => 1,
                    "border-bottom-right-radius" => 2,
                    _ => 3,
                };
                if let Some(len) = css::radius_corner(v) {
                    self.radius[corner] = Some(len);
                }
            }
            "text-indent" => {
                if let Some(l) = css::length(v).filter(|l| l.unit != css::Unit::Percent) {
                    self.text_indent = Some(l);
                }
            }
            "display" => {
                let v = v.trim();
                if v.eq_ignore_ascii_case("none") {
                    run.vanish = true;
                } else if self.card != CardMode::No
                    && v.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
                {
                    // An inline or inline-block `div` is no card (#1927).
                    self.not_block = !v.eq_ignore_ascii_case("block");
                } else if self.is_img && v.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-') {
                    // A block image is a paragraph of its own (#1922); any
                    // other display (`inline`, `inline-block`) undoes it.
                    self.look.own_block = v.eq_ignore_ascii_case("block");
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
                            self.margin_auto[i] = v.trim().eq_ignore_ascii_case("auto");
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
        let card = self.kind == Kind::Block
            && match self.card {
                CardMode::Yes(_) => true,
                CardMode::Maybe => {
                    self.background.is_some() && !self.look.run.vanish && !self.not_block
                }
                CardMode::No | CardMode::Declined => false,
            };
        let look = &mut self.look;
        if card {
            // A card is a cell (#1927): a fresh block look, as a `td`
            // starts; a `text-indent` it declares still applies below.
            look.in_cell = true;
            look.indent = 0;
            look.indent_right = 0;
            look.first_line = 0;
            look.shade = None;
        }
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
            Kind::Block if card => {
                // Its box is its table's (#1927): the fill, the padding as
                // the cell's margins, the width and place as the table's.
                look.own_card = true;
                look.own_fill = self.background;
                for (side, p) in self.padding.iter().enumerate() {
                    look.own_padding[side] = tw(*p).map(|t| t.max(0));
                }
                look.own_width = self.block_width.and_then(|l| match l.unit {
                    css::Unit::Percent => Some(Width::Pct(l.v)),
                    _ => l.twips(pt).map(|t| Width::Twips(t.max(0) as u32)),
                });
                look.own_place = block_position(self.margin_auto, self.parent_block_align);
            }
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
                // A fixed-width box with both side margins auto is centred
                // in the body (#1922): its content box, so its own padding
                // (in the indents now) is outside the width.
                // A percentage leaves the block as wide as its container.
                let width = self
                    .block_width
                    .filter(|l| l.unit != css::Unit::Percent)
                    .and_then(|l| l.twips(pt));
                if let Some(width) = width.filter(|_| self.margin_auto[1] && self.margin_auto[3]) {
                    if !look.in_cell {
                        let room = i64::from(html_table::BODY_TWIPS)
                            - i64::from(look.indent)
                            - i64::from(look.indent_right)
                            - i64::from(width.max(0));
                        let extra = (room / 2).max(0);
                        look.indent = clamp_twips(i64::from(look.indent) + extra);
                        look.indent_right = clamp_twips(i64::from(look.indent_right) + extra);
                    }
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
            Kind::Cell | Kind::Table => {
                if self.is_table {
                    look.own_place = block_position(self.margin_auto, self.parent_block_align);
                }
                look.own_fill = self.background;
                if self.kind == Kind::Cell {
                    for (side, p) in self.padding.iter().enumerate() {
                        look.own_padding[side] = tw(*p).map(|t| t.max(0));
                    }
                }
            }
            Kind::List | Kind::Container | Kind::Other => {
                if self.is_img && look.own_block {
                    look.own_place = block_position(self.margin_auto, self.parent_block_align);
                }
                if self.is_img {
                    look.own_radius = image_radius(self.radius, pt);
                }
            }
        }
        self.look
    }
}

/// An image's one radius from its four corners (#1926), each resolved
/// against its font: `None` with none set or all of them zero, `NonUniform`
/// when they differ (a corner left unset is a square one). A zero length
/// and a zero percentage are the same square corner.
fn image_radius(corners: [Option<Len>; 4], font_pt: f64) -> Option<html_image::Radius> {
    use html_image::Radius;
    if corners.iter().all(Option::is_none) {
        return None;
    }
    let resolve = |c: Option<Len>| match c {
        Some(l) if l.unit == css::Unit::Percent && l.v > 0.0 => Radius::Pct(l.v),
        Some(l) => {
            let t = l.points(font_pt).map_or(0.0, |p| (p * 20.0).round());
            // `t` is finite and non-negative (the parser refuses negative
            // radii), so the cast saturates at most.
            Radius::Twips(t.max(0.0) as u64)
        }
        None => Radius::Twips(0),
    };
    let first = resolve(corners[0]);
    if corners[1..].iter().any(|&c| resolve(c) != first) {
        return Some(Radius::NonUniform);
    }
    match first {
        Radius::Twips(0) => None,
        r => Some(r),
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
fn look_of(
    ctx: Ctx,
    below: &[Elem],
    key: &ElemKey,
    kind: Kind,
    attrs: &Attrs,
    card: CardMode,
) -> Look {
    let mut c = Cascade::new(below.last().map(|e| &e.look), kind, card);
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
    /// properties and link.
    space: Option<(RunProps, Option<Rc<Link>>)>,
    /// Nothing visible yet in this paragraph (leading spaces are dropped).
    para_start: bool,
    /// Inside `<![if !supportLists]>`: text is the list marker.
    in_marker: bool,
    marker: Option<String>,
    /// Inside another `<![if …]>` block, dropped.
    in_downlevel: bool,
    /// That block is `<![if !vml]>`: Word's `<img>` for readers without
    /// VML, the one image of it read (#1880).
    in_not_vml: bool,
    /// The page's images, when the import converts them.
    images: Option<Images<'a>>,
    /// A block image ended the paragraph (#1922): a `</p>` then ends only
    /// one with content, so `<p><img style="display:block"></p>` makes no
    /// empty paragraph after the picture's.
    after_block_object: bool,
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
            in_not_vml: false,
            images: None,
            after_block_object: false,
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
            if let Some(condition) = what.strip_prefix("if") {
                if what.contains("supportlists") {
                    self.in_marker = true;
                    self.marker = Some(String::new());
                } else {
                    self.in_downlevel = true;
                    let cond: String = condition
                        .trim_end_matches("]>")
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect();
                    self.in_not_vml = cond == "!vml";
                }
            } else if what.starts_with("endif") {
                self.in_marker = false;
                self.in_downlevel = false;
                self.in_not_vml = false;
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
            "img" => {
                self.image(attrs_at);
                return;
            }
            // Void elements: nothing opens.
            "meta" | "link" | "input" | "wbr" | "col" | "area" | "base" | "source" | "param"
            | "embed" | "track" => return,
            _ => {}
        }
        let s = self.s;
        let attrs = attrs_of(&s[attrs_at.0..attrs_at.1]);
        let kind = Kind::of(name);
        // An `a` ends the open `a` first (#1879), as a browser's adoption
        // agency does: the formatting inside it is reopened under the new
        // one. Nothing when none is open, nor across a table or cell.
        if name == "a" {
            self.close_tag("a");
        }
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
                    if self.kept_table() {
                        self.out.row_group_boundary();
                    }
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
                let props = look_of(self.ctx(), &self.elems, &key, kind, &attrs, CardMode::No).run;
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
                self.after_block_object = false;
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
                    block.item_numbered = false;
                    block.item_split = false;
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
        let pushed = self.elems.len() < MAX_DEPTH && self.budget.work(1 + key.heap() / 16);
        if pushed {
            // A `div` in a cell may be a card (#1927): only with its blocks
            // entry on `blocks`, room for one more table, and a cell as the
            // innermost open table part (a `div` straight in a `table`,
            // row group or row, which a browser moves out, is none: the
            // builder's innermost table there has no cell open).
            let candidate = name == "div"
                && self.blocks.len() > blocks_at
                && self.visible()
                && self.kept_table()
                && self.table_depth < MAX_TABLE_NESTING
                && self
                    .elems
                    .iter()
                    .rev()
                    .find(|e| e.kind.table())
                    .is_some_and(|e| e.kind == Kind::Cell);
            let mode = if candidate {
                CardMode::Maybe
            } else {
                CardMode::No
            };
            let look = look_of(self.ctx(), &self.elems, &key, kind, &attrs, mode);
            // A card's table opens one level down (`begin_card`).
            let card = match mode {
                CardMode::Maybe if look.own_card => CardMode::Yes(self.table_depth + 1),
                CardMode::Maybe => CardMode::Declined,
                m => m,
            };
            self.elems.push(Elem {
                key,
                kind,
                look,
                attrs: attrs_at,
                blocks_at,
                lists_at,
                card,
            });
            if matches!(card, CardMode::Yes(_)) {
                self.begin_card();
            }
        }
        let kept = self.table_depth <= MAX_TABLE_NESTING;
        match name {
            "table" if kept => {
                let look = self.elems.last().filter(|_| pushed).map(|e| &e.look);
                let spec = table_spec(&attrs, look.unwrap_or(&Look::default()));
                self.out.begin_table(spec);
            }
            "td" | "th" if kept && self.table_depth > 0 => {
                let spec = self.cell_spec(&attrs, pushed);
                self.out.begin_cell(spec);
            }
            _ => {}
        }
    }

    /// The spec of the cell just opened (on top of `elems` when `pushed`):
    /// its spans and width, its fill or the nearest of its row's, row
    /// group's and table's, its inherited vertical alignment, its padding.
    fn cell_spec(&self, attrs: &Attrs, pushed: bool) -> CellSpec {
        let colspan = attrs
            .colspan
            .as_deref()
            .and_then(html_int)
            .filter(|&n| n > 0)
            .unwrap_or(1)
            .min(1000);
        let rowspan = attrs
            .rowspan
            .as_deref()
            .and_then(html_int)
            .unwrap_or(1)
            .min(65_534);
        let mut spec = CellSpec {
            colspan,
            rowspan,
            ..CellSpec::default()
        };
        let mut below = self.elems.len();
        if pushed {
            if let Some(e) = self.elems.last() {
                spec.width = e.look.own_width;
                spec.valign = e.look.valign.unwrap_or_default();
                spec.padding = e.look.own_padding;
                spec.fill = e.look.own_fill.clone();
                below -= 1;
            }
        }
        // Else the nearest fill of its row, row group or table.
        if spec.fill.is_none() {
            for e in self.elems[..below].iter().rev() {
                if e.kind == Kind::Cell {
                    break;
                }
                if e.kind != Kind::Table {
                    continue;
                }
                if let Some(f) = &e.look.own_fill {
                    spec.fill = Some(f.clone());
                    break;
                }
                if e.key.tag == "table" {
                    break;
                }
            }
        }
        spec
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
                e.card,
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
        // Every card still open in it ends first, the stacks intact:
        // `implicit_close` cuts them after this, `close_elem` has already
        // ended its own (before its cut), so here it finds none.
        self.end_cards_from(i);
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
                        card: CardMode::No,
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
        // A table part's close acts on the innermost real table: the cards
        // open in its innermost cell end first (#1927), as a browser's
        // `</td>` closes a `div` left open.
        if Kind::of(name).table() {
            let from = self
                .elems
                .iter()
                .rposition(|e| e.kind.table())
                .map_or(0, |t| t + 1);
            self.end_cards_from(from);
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
                // Rows after it are another group (a bare `tr` there opens an
                // implied `tbody` in a browser).
                let open = self
                    .elems
                    .iter()
                    .rev()
                    .take_while(|e| e.key.tag != "table")
                    .any(|e| e.key.tag == name);
                if open && self.kept_table() {
                    self.out.row_group_boundary();
                }
            }
            _ => {}
        }
        match name {
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "blockquote" | "pre"
            | "address" | "dt" | "dd" | "center" | "caption" => {
                if let Some(i) = self.blocks.iter().rposition(|(n, _)| n == name) {
                    // A `p` always makes a paragraph, empty or not: Word writes
                    // its empty paragraphs as `<p>&nbsp;</p>`. Not after a
                    // block image, whose paragraph it was (#1922).
                    if name == "p" && !self.after_block_object {
                        self.end_para();
                    } else {
                        self.end_para_if_any();
                    }
                    self.after_block_object = false;
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
                    if self.kept_table() {
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
                if self.kept_table() {
                    let props = self.par_props();
                    self.out.end_row(props);
                    self.reset_para();
                }
            }
            "table" => {
                if self.table_depth > 0 {
                    if self.kept_table() {
                        let props = self.par_props();
                        self.out.end_row(props);
                        self.out.end_table();
                        self.reset_para();
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
        self.end_cards_from(i);
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

    /// Inside a table the builder holds: one at most [`MAX_TABLE_NESTING`]
    /// deep (a deeper one's cells and rows are its enclosing cell's text).
    fn kept_table(&self) -> bool {
        (1..=MAX_TABLE_NESTING).contains(&self.table_depth)
    }

    fn visible(&self) -> bool {
        !self.in_downlevel && !self.in_marker
    }

    /// Open the card on top of `elems` (#1927): a one-cell table, as wide
    /// as its `width` (else its container), placed by its auto margins or
    /// its container's `-webkit-` alignment, no borders or cell spacing,
    /// and a cell of its fill and padding (a side not given is 0, as in a
    /// browser), its content at the top. What is in it starts a fresh
    /// block structure, as in a `td`.
    fn begin_card(&mut self) {
        let Some(e) = self.elems.last_mut() else {
            return;
        };
        if let Some((_, block)) = self.blocks.last_mut() {
            *block = Block::default();
        }
        self.table_depth += 1;
        let look = &e.look;
        let table = TableSpec {
            width: Some(look.own_width.unwrap_or(Width::Pct(100.0))),
            border: 0,
            cellpadding: Some(0),
            cellspacing: None,
            jc: match look.own_place {
                Some(Align::Center) => Some("center"),
                Some(Align::Right) => Some("right"),
                _ => None,
            },
        };
        let cell = CellSpec {
            fill: look.own_fill.clone(),
            padding: look.own_padding.map(|p| Some(p.unwrap_or(0))),
            valign: VAlign::Top,
            ..CellSpec::default()
        };
        self.out.begin_table(table);
        self.out.begin_cell(cell);
    }

    /// End every card still open in `elems[i..]`, innermost first (#1927):
    /// its open paragraph (the stacks as they still are), then its row and
    /// table, into the enclosing cell; a card with nothing in it is dropped
    /// (an e-mail's divider). A real table left open inside a card (a
    /// malformed page) ends with it, so the builder's innermost table is
    /// the card's.
    fn end_cards_from(&mut self, i: usize) {
        while let Some((j, depth)) = self.elems.get(i..).and_then(|above| {
            above
                .iter()
                .enumerate()
                .rev()
                .find_map(|(j, e)| match e.card {
                    CardMode::Yes(depth) => Some((i + j, depth)),
                    _ => None,
                })
        }) {
            self.elems[j].card = CardMode::Declined;
            if self.table_depth < depth {
                // Its table has already ended (a stray `</table>`).
                continue;
            }
            self.end_para_if_any();
            self.after_block_object = false;
            while self.table_depth > depth {
                // Only a table the builder holds (one past the nesting
                // limit is its cell's text).
                if self.kept_table() {
                    let props = self.par_props();
                    self.out.end_row(props);
                    self.out.end_table();
                    self.reset_para();
                }
                self.table_depth -= 1;
            }
            let props = self.par_props();
            self.out.end_card(props);
            self.reset_para();
            self.table_depth -= 1;
        }
    }

    /// An `<img>` (#1880), with images on: an inline picture, a sized
    /// placeholder, its `alt` text, or nothing (out of view, hidden, a side
    /// of 0, or unsized and unresolved with no `alt`).
    fn image(&mut self, attrs_at: (usize, usize)) {
        let in_view = !self.in_marker && (!self.in_downlevel || self.in_not_vml);
        if self.images.is_none() || !in_view {
            return;
        }
        let s = self.s;
        let attrs = attrs_of(&s[attrs_at.0..attrs_at.1]);
        let key = ElemKey::new("img", attrs.id.as_deref(), attrs.class.as_deref());
        if !self.budget.work(1 + key.heap() / 16) {
            return;
        }
        let look = look_of(
            self.ctx(),
            &self.elems,
            &key,
            Kind::Other,
            &attrs,
            CardMode::No,
        );
        if look.run.vanish {
            return;
        }
        let side = |w: Option<Width>| match w {
            Some(Width::Twips(t)) => Some(t),
            _ => None,
        };
        let (width, height) = (side(look.own_width), side(look.own_height));
        if width == Some(0) || height == Some(0) {
            return;
        }
        let src = attrs.src.as_deref().unwrap_or("").trim();
        let alt = html_image::alt_text(attrs.alt.as_deref().unwrap_or(""));
        let props = self.run_props();
        let budget = self.budget;
        let Some(images) = self.images.as_mut() else {
            return;
        };
        let (rid, pixels) = match images.source(src, budget) {
            Source::Media { rid, width, height } => (Some(rid), Some((width, height))),
            Source::Unresolved => (None, None),
        };
        // A block image (#1922) is a paragraph of its own, placed by its
        // margins or its container's `-webkit-` alignment.
        let block = look
            .own_block
            .then(|| look.own_place.unwrap_or(Align::Left));
        let declared = html_image::declared_box(width, height, pixels);
        let Some(box_twips) = declared.map(html_image::fit_to_body) else {
            if !alt.is_empty() {
                self.begin_block_object(block);
                // Alt text in a link is the link's text, as a browser
                // shows it (#1879).
                let link = self.run_link();
                self.pay_space();
                self.out.link_text(&alt, &props, link.as_deref());
                self.para_start = false;
                self.end_block_object(block);
            }
            return;
        };
        let outline = rid.is_none().then_some(html_image::PLACEHOLDER_OUTLINE);
        let Some(rid) = rid.or_else(|| images.placeholder_rid(budget)) else {
            return;
        };
        let id = images.next_id();
        let (cx, cy) = html_image::twips_emu(box_twips);
        let name = html_image::picture_name(src, id);
        // Its crop by its `border-radius` (#1926), measured in the box the
        // page declares (the fit to the body is uniform).
        let shape = declared.map_or(crate::image::PictureShape::Rect, |b| {
            html_image::shape(look.own_radius, b)
        });
        let raw =
            crate::image::inline_picture_run_shaped(&rid, cx, cy, id, &name, &alt, outline, shape);
        self.begin_block_object(block);
        self.pay_space();
        let heap = raw.len();
        self.out.object(Inline::Raw(raw), heap);
        self.para_start = false;
        self.end_block_object(block);
    }

    /// Before a block image's content (`block`: its place): end the text
    /// before it, which stays its own paragraph (#1922).
    fn begin_block_object(&mut self, block: Option<Align>) {
        if block.is_some() {
            self.end_para_if_any();
            self.space = None;
            if let Some(item) = self.open_item() {
                item.item_split = true;
            }
        }
    }

    /// The open list item's own entry on `blocks`, not across a cell.
    fn open_item(&mut self) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .rev()
            .take_while(|(n, _)| n != "td" && n != "th")
            .find(|(n, _)| n == "li")
            .map(|(_, b)| b)
    }

    /// After a block image's content: end its paragraph, aligned to its
    /// place; text after it starts the next one (#1922).
    fn end_block_object(&mut self, block: Option<Align>) {
        if let Some(align) = block {
            self.end_para_with(Some(align));
            self.after_block_object = true;
        }
    }

    /// Write the space owed before what comes next, in its own properties
    /// and link.
    fn pay_space(&mut self) {
        if let Some((sp, link)) = self.space.take() {
            self.out.link_text(" ", &sp, link.as_deref());
        }
    }

    /// The link the innermost element's text is in (#1879).
    fn run_link(&self) -> Option<Rc<Link>> {
        self.elems.last().and_then(|e| e.look.link.clone())
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
        let link = self.run_link();
        let mut out = String::new();
        for c in text.chars() {
            if self.pre > 0 {
                // A break or tab is written outside the link, which goes on
                // after it as a link of its own to the same place.
                match c {
                    '\n' => {
                        self.out.link_text(&out, &props, link.as_deref());
                        out.clear();
                        self.out.line_break(&props);
                    }
                    '\r' => {}
                    '\t' => {
                        self.out.link_text(&out, &props, link.as_deref());
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
                    self.space = Some((props.clone(), link.clone()));
                }
                continue;
            }
            // The owed space is in the formatting and link it was written
            // in.
            match self.space.take() {
                Some((sp, sl)) if sp == props && sl == link => out.push(' '),
                Some((sp, sl)) => self.out.link_text(" ", &sp, sl.as_deref()),
                None => {}
            }
            self.para_start = false;
            out.push(c);
        }
        self.out.link_text(&out, &props, link.as_deref());
    }

    /// The innermost paragraph-making element's look, else the innermost
    /// element's (text straight in `body`).
    fn para_look(&self) -> Option<&Look> {
        self.elems
            .iter()
            .rev()
            .find(|e| matches!(e.kind, Kind::Block | Kind::Cell))
            .or(self.elems.last())
            .map(|e| &e.look)
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
        let Some(look) = self.para_look() else {
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
                    p.indent = item_indent(level, look.indent);
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
        if !self.out.para_text().is_empty() || self.out.has_object() || self.marker.is_some() {
            self.end_para();
        }
    }

    fn end_para(&mut self) {
        self.end_para_with(None);
    }

    /// End the paragraph, its alignment `align` when given (a block
    /// image's place, #1922).
    fn end_para_with(&mut self, align: Option<Align>) {
        let block = self
            .blocks
            .last()
            .map(|(_, b)| b.clone())
            .unwrap_or_default();
        self.after_block_object = false;
        let mut props = self.par_props();
        if let Some(a) = align {
            props.align = a;
        }
        let mut marker = self.marker.take();
        if marker.is_none() && block.mso_list && block.heading.is_none() {
            marker = self.out.take_leading_marker();
        }
        if block.heading.is_none() {
            if let Some(m) = &marker {
                props.num_id = Some(if marker_is_numbered(m) { 2 } else { 1 });
            } else if let Some((numbered, level)) = block.item {
                let (done, split) = self
                    .open_item()
                    .map_or((false, false), |i| (i.item_numbered, i.item_split));
                if split && done && !block.mso_list {
                    // Past a block image, the item's text goes on in
                    // unnumbered paragraphs at its level's indent (#1922).
                    let boxes = self.para_look().map_or(0, |l| l.indent);
                    props.indent = item_indent(level, boxes);
                    props.first_line = 0;
                } else {
                    props.num_id = Some(if numbered { 2 } else { 1 });
                    props.ilvl = level;
                    if let Some(item) = self.open_item() {
                        item.item_numbered = true;
                    }
                }
            }
        }
        // Only spaces (Word's `&nbsp;` in an empty paragraph) is empty; a
        // picture is content (#1880).
        if !self.out.has_object() && self.out.para_text().chars().all(|c| c.is_whitespace()) {
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
            "width" => Some(&mut out.width),
            "bgcolor" => Some(&mut out.bgcolor),
            "border" => Some(&mut out.border),
            "cellpadding" => Some(&mut out.cellpadding),
            "cellspacing" => Some(&mut out.cellspacing),
            "colspan" => Some(&mut out.colspan),
            "rowspan" => Some(&mut out.rowspan),
            "valign" => Some(&mut out.valign),
            "src" => Some(&mut out.src),
            "alt" => Some(&mut out.alt),
            "height" => Some(&mut out.height),
            "href" => Some(&mut out.href),
            _ => None,
        };
        if let Some(slot) = slot.filter(|s| s.is_none()) {
            *slot = Some(attr_entities(value));
        }
        if i == start {
            i += 1;
        }
    }
    out
}

/// The link an `href` (its character references decoded) makes (#1879),
/// read as a browser's URL parser reads it: leading and trailing control
/// characters and spaces stripped, every tab and line break inside removed
/// (so `java\tscript:` is `javascript:`). `#name` is an anchor link to
/// `name`, percent-decoded. Otherwise only an absolute URL whose scheme is
/// in [`LINK_SCHEMES`] is a link, its scheme lowercased and space, `"`,
/// `<`, `>` and `` ` `` percent-encoded (a field's `HYPERLINK "…"` quotes
/// it). `None` (no link) for anything else: a relative or empty `href`, one
/// holding another control character, one longer than [`MAX_HREF`].
fn link_of(href: &str) -> Option<Link> {
    // Bounded by the limit, not by the attribute.
    if href.len() > 2 * MAX_HREF {
        return None;
    }
    let href: String = href
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    if href.chars().any(char::is_control) {
        return None;
    }
    if let Some(fragment) = href.strip_prefix('#') {
        let name = String::from_utf8_lossy(&html_image::percent_decode(fragment)).into_owned();
        let usable =
            !name.is_empty() && name.len() <= MAX_HREF && !name.chars().any(char::is_control);
        return usable.then_some(Link::Anchor(name));
    }
    let (scheme, rest) = href.split_once(':')?;
    let mut letters = scheme.chars();
    let is_scheme = letters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && letters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !is_scheme || rest.is_empty() || !LINK_SCHEMES.iter().any(|k| scheme.eq_ignore_ascii_case(k))
    {
        return None;
    }
    let mut url = scheme.to_ascii_lowercase();
    url.push(':');
    for c in rest.chars() {
        match c {
            ' ' => url.push_str("%20"),
            '"' => url.push_str("%22"),
            '<' => url.push_str("%3C"),
            '>' => url.push_str("%3E"),
            '`' => url.push_str("%60"),
            _ => url.push(c),
        }
    }
    (url.len() <= MAX_HREF).then_some(Link::Url(url))
}

/// `text` with character references decoded.
fn entities(text: &str) -> String {
    decode_references(text, false)
}

/// An attribute value with character references decoded, as HTML decodes
/// them there (#1879 r1): a named reference not ended by `;` and followed
/// by `=` or an ASCII letter or digit is kept as written, so
/// `href="?id=3&copy=2"` keeps its `&copy=2`.
fn attr_entities(value: &str) -> String {
    decode_references(value, true)
}

fn decode_references(text: &str, in_attribute: bool) -> String {
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
            let next = rest[end..].chars().next();
            let held = in_attribute && next.is_some_and(|c| c == '=' || c.is_ascii_alphanumeric());
            if held { None } else { named(name) }
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
            import_html_within(page.as_bytes(), &small, None).unwrap_err(),
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
        // A nested table without its `</td>` stays in the outer cell, as a
        // table of its own (#1878).
        let d = doc(
            "<table><tr><td>outer<table><tr><td>in1<td>in2</table>more</td><td>next</td></tr></table>",
        );
        let rows = table_texts(&d);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].len(), 2, "{rows:?}");
        assert_eq!(rows[0][0], "outer|more", "{rows:?}");
        assert_eq!(rows[0][1], "next");
        let inner = nested(table(&d), 0, 0);
        assert_eq!(inner.len(), 1);
        let texts: Vec<String> = inner[0].rows[0]
            .cells
            .iter()
            .map(|c| c.blocks.iter().map(|b| b.plain_text()).collect())
            .collect();
        assert_eq!(texts, ["in1", "in2"]);
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
            import_html_within(page.as_bytes(), &small, None).unwrap_err(),
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

    /// The first table of `d`.
    fn table(d: &Document) -> &crate::model::Table {
        match d.body.iter().find(|b| matches!(b, DocBlock::Table(_))) {
            Some(DocBlock::Table(t)) => t,
            _ => panic!("no table: {:?}", d.body),
        }
    }

    fn tcpr(t: &crate::model::Table, row: usize, cell: usize) -> &str {
        t.rows[row].cells[cell].raw_tcpr.as_deref().unwrap_or("")
    }

    fn tcw(t: &crate::model::Table, row: usize, cell: usize) -> u32 {
        let pr = tcpr(t, row, cell);
        let w = pr
            .split("<w:tcW w:w=\"")
            .nth(1)
            .unwrap_or_else(|| panic!("{pr}"));
        w[..w.find('"').unwrap()].parse().unwrap()
    }

    /// The nested tables of a cell.
    fn nested(t: &crate::model::Table, row: usize, cell: usize) -> Vec<&crate::model::Table> {
        t.rows[row].cells[cell]
            .blocks
            .iter()
            .filter_map(|b| match b {
                DocBlock::Table(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn colspan_and_rowspan_make_grid_spans_and_vertical_merges() {
        use crate::model::VMerge;
        let d = doc(
            "<table><tr><td colspan=2>head</td><td rowspan=2>side</td></tr>\
             <tr><td>a</td><td>b</td></tr><tr><td rowspan=0>all</td><td colspan=\"3\">wide</td></tr>\
             <tr><td>last</td></tr></table>",
        );
        let t = table(&d);
        // `wide` spans three columns after `all`: the grid grows to four.
        assert_eq!(t.grid.len(), 4);
        let shape: Vec<Vec<(u32, VMerge)>> = t
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| (c.grid_span, c.v_merge)).collect())
            .collect();
        use VMerge::*;
        assert_eq!(
            shape,
            [
                vec![(2, None), (1, Restart), (1, None)],
                vec![(1, None), (1, None), (1, Continue), (1, None)],
                // `rowspan=0` reaches the last row.
                vec![(1, Restart), (3, None)],
                vec![(1, Continue), (1, None), (1, None), (1, None)],
            ]
        );
        assert!(tcpr(t, 0, 0).contains("<w:gridSpan w:val=\"2\"/>"));
        assert!(tcpr(t, 2, 0).contains("<w:vMerge w:val=\"restart\"/>"));
        assert!(tcpr(t, 3, 0).contains("<w:vMerge/>"));
        // The continuation is an empty cell; `last` sits after it.
        assert_eq!(table_texts(&d)[3], ["", "last", "", ""]);
        // Every row covers the grid.
        for r in &t.rows {
            let cols: u32 = r.cells.iter().map(|c| c.grid_span).sum();
            assert_eq!(cols, 4);
        }
    }

    #[test]
    fn a_rowspan_ends_with_its_row_group() {
        use crate::model::VMerge::{self, *};
        let shape = |d: &Document| -> Vec<Vec<(u32, VMerge)>> {
            table(d)
                .rows
                .iter()
                .map(|r| r.cells.iter().map(|c| (c.grid_span, c.v_merge)).collect())
                .collect()
        };
        // A header cell's rowspan does not reach into the body.
        let d = doc(
            "<table><thead><tr><th rowspan=2>H</th><th>A</th></tr></thead>\
             <tbody><tr><td>x</td><td>y</td></tr></tbody></table>",
        );
        assert_eq!(table(&d).grid.len(), 2);
        assert_eq!(
            shape(&d),
            [vec![(1, None), (1, None)], vec![(1, None), (1, None)]]
        );
        assert_eq!(table_texts(&d), [["H", "A"], ["x", "y"]]);
        // `rowspan=0` reaches its group's last row, with implied group ends.
        let d = doc("<table><tbody><tr><td rowspan=0>a<td>b<tr><td>c\
             <tbody><tr><td>d<td>e</table>");
        assert_eq!(
            shape(&d),
            [
                vec![(1, Restart), (1, None)],
                vec![(1, Continue), (1, None)],
                vec![(1, None), (1, None)],
            ]
        );
        assert_eq!(table_texts(&d)[2], ["d", "e"]);
        // Bare rows after a closed group are a group of their own.
        let d = doc("<table><tbody><tr><td rowspan=3>a<td>b</tbody><tr><td>c<td>d</tr></table>");
        assert_eq!(table_texts(&d), [["a", "b"], ["c", "d"]]);
        assert_eq!(shape(&d)[0], [(1, None), (1, None)]);
        // Without row groups the rowspan reaches as far as the table.
        let d = doc("<table><tr><td rowspan=3>a<td>b<tr><td>c</table>");
        assert_eq!(shape(&d)[1], [(1, Continue), (1, None)]);
    }

    #[test]
    fn widths_from_attributes_and_css_build_the_grid() {
        // `width` attributes: px and a percentage of the table's.
        let d = doc(
            "<table width=600><tr><td width=200>a</td><td width='50%'>b</td><td>c</td></tr></table>",
        );
        let t = table(&d);
        assert_eq!(t.grid, [3000, 4500, 1500]);
        assert!(
            t.raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblW w:w=\"9000\" w:type=\"dxa\"/>")
        );
        assert_eq!(
            (tcw(t, 0, 0), tcw(t, 0, 1), tcw(t, 0, 2)),
            (3000, 4500, 1500)
        );
        // CSS over the attribute; a percent table.
        let d = doc("<style>.w{width:1in}</style><table style='width:50%'><tr>\
             <td class=w width=10>a</td><td style='width:auto' width=100>b</td></tr></table>");
        let t = table(&d);
        assert_eq!(t.grid, [1440, 3240]);
        assert!(
            t.raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblW w:w=\"2500\" w:type=\"pct\"/>")
        );
        // No widths: the 6.5" column shared, tblW auto; a garbage width
        // is none.
        let d = doc("<table width=wide><tr><td width=*>a</td><td>b</td></tr></table>");
        let t = table(&d);
        assert_eq!(t.grid, [4680, 4680]);
        assert!(
            t.raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblW w:w=\"0\" w:type=\"auto\"/>")
        );
        // Every column known and no table width: their sum.
        let d = doc("<table><tr><td width=100px>a</td><td width=50>b</td></tr></table>");
        assert_eq!(table(&d).grid, [1500, 750]);
    }

    #[test]
    fn over_full_declared_widths_widen_the_grid() {
        let d = doc("<table width=100><tr><td width=300>a</td><td>b</td></tr></table>");
        let t = table(&d);
        assert_eq!(t.grid, [4500, html_table::MIN_COL]);
        assert!(
            t.raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblW w:w=\"1500\" w:type=\"dxa\"/>")
        );
        let d = doc("<table width=600><tr><td width=70%>a</td><td width=70%>b</td></tr></table>");
        assert_eq!(table(&d).grid, [4500, 4500]);
    }

    #[test]
    fn cell_shading_falls_back_to_row_and_table() {
        let d = doc(
            "<style>.blue{background:#0000ff} tr.g{background-color:green}</style>\
             <table bgcolor=#ffff00><tbody bgcolor=silver><tr bgcolor=red>\
             <td bgcolor=white>own</td><td class=blue bgcolor=white>css</td><td>row</td></tr>\
             <tr><td>group</td><td style='background:none' bgcolor=white>cleared</td></tr></tbody>\
             <tr class=g><td>css row</td></tr><tr><td><p>table</p></td></tr></table>",
        );
        let t = table(&d);
        let fill = |r: usize, c: usize| {
            let pr = tcpr(t, r, c);
            pr.split("w:fill=\"").nth(1).map(|f| f[..6].to_string())
        };
        assert_eq!(fill(0, 0).as_deref(), Some("FFFFFF"));
        assert_eq!(fill(0, 1).as_deref(), Some("0000FF"), "CSS over bgcolor");
        assert_eq!(fill(0, 2).as_deref(), Some("FF0000"), "the row's");
        assert_eq!(fill(1, 0).as_deref(), Some("C0C0C0"), "the row group's");
        assert_eq!(
            fill(1, 1).as_deref(),
            Some("C0C0C0"),
            "none shows the group's"
        );
        assert_eq!(fill(2, 0).as_deref(), Some("008000"));
        assert_eq!(fill(3, 0).as_deref(), Some("FFFF00"), "the table's");
        // The cells' paragraphs are not shaded by it.
        for p in all_paras(&d) {
            assert!(p.props.raw_props.is_empty(), "{:?}", p.props.raw_props);
        }
        // A cell without any fill has no shd.
        let d = doc("<table><tr><td>x</td></tr></table>");
        assert!(!tcpr(table(&d), 0, 0).contains("w:shd"));
    }

    #[test]
    fn valign_from_attribute_css_and_row() {
        let d = doc(
            "<table><tbody valign=bottom><tr valign=top><td>a</td><td valign=middle>b</td>\
             <td style='vertical-align:bottom'>c</td></tr><tr><td>d</td></tr></tbody>\
             <tr><td>e</td><td style='vertical-align:super'>f</td></tr></table>",
        );
        let t = table(&d);
        let va = |r: usize, c: usize| {
            let pr = tcpr(t, r, c);
            let v = pr.split("<w:vAlign w:val=\"").nth(1).unwrap();
            v[..v.find('"').unwrap()].to_string()
        };
        assert_eq!(va(0, 0), "top", "the row's");
        assert_eq!(va(0, 1), "center");
        assert_eq!(va(0, 2), "bottom");
        assert_eq!(va(1, 0), "bottom", "the row group's, through its row");
        assert_eq!(va(2, 0), "center", "a browser's middle");
        assert_eq!(va(2, 1), "top", "super on a cell is its baseline");
        // ... and it makes no superscript text.
        assert_eq!(run_of(&d, "f").props.vert_align, VertAlign::Baseline);
        // A nested table does not inherit its cell's alignment.
        let d =
            doc("<table><tr valign=top><td><table><tr><td>in</td></tr></table></td></tr></table>");
        let inner = nested(table(&d), 0, 0)[0];
        assert!(tcpr(inner, 0, 0).contains("<w:vAlign w:val=\"center\"/>"));
    }

    #[test]
    fn border_cellpadding_cellspacing_and_table_align() {
        let d = doc(
            "<table border=2 cellpadding=4 cellspacing=6 align=center><tr align=right>\
             <td style='padding:2px 0'>a</td><td>b</td></tr></table>",
        );
        let t = table(&d);
        let pr = t.raw_tblpr.as_deref().unwrap();
        assert!(pr.contains("<w:jc w:val=\"center\"/>"), "{pr}");
        assert!(
            pr.contains("<w:tblCellSpacing w:w=\"45\" w:type=\"dxa\"/>"),
            "{pr}"
        );
        assert!(pr.contains("<w:top w:val=\"single\" w:sz=\"12\""), "{pr}");
        assert!(
            pr.contains("<w:insideH w:val=\"single\" w:sz=\"6\""),
            "{pr}"
        );
        assert!(
            pr.contains(
                "<w:tblCellMar><w:top w:w=\"60\" w:type=\"dxa\"/><w:left w:w=\"60\" \
                 w:type=\"dxa\"/><w:bottom w:w=\"60\" w:type=\"dxa\"/><w:right w:w=\"60\" \
                 w:type=\"dxa\"/></w:tblCellMar>"
            ),
            "{pr}"
        );
        // A cell's own padding is its tcMar.
        assert!(
            tcpr(t, 0, 0).contains(
                "<w:tcMar><w:top w:w=\"30\" w:type=\"dxa\"/><w:left w:w=\"0\" w:type=\"dxa\"/>\
                 <w:bottom w:w=\"30\" w:type=\"dxa\"/><w:right w:w=\"0\" w:type=\"dxa\"/></w:tcMar>"
            ),
            "{}",
            tcpr(t, 0, 0)
        );
        assert!(!tcpr(t, 0, 1).contains("w:tcMar"));
        // The table's align is the table's; the row's aligns the text.
        assert_eq!(para_of(&d, "a").props.align, Align::Right);
        let d = doc("<table align=center><tr><td>x</td></tr></table>");
        assert_eq!(para_of(&d, "x").props.align, Align::Left);
        // A bare border is 1px; border=0, a layout table, has none; an
        // explicit cellpadding=0 is four zero margins.
        let d = doc("<table border><tr><td>x</td></tr></table>");
        assert!(
            table(&d)
                .raw_tblpr
                .as_deref()
                .unwrap()
                .contains("w:sz=\"6\"")
        );
        let d = doc("<table border=0 cellpadding=0 cellspacing=0><tr><td>x</td></tr></table>");
        let pr = table(&d).raw_tblpr.as_deref().unwrap().to_string();
        assert!(
            !pr.contains("tblBorders") && !pr.contains("tblCellSpacing"),
            "{pr}"
        );
        assert!(
            pr.contains("<w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/>"),
            "{pr}"
        );
        assert_eq!(pr.matches("w:w=\"0\" w:type=\"dxa\"").count(), 4, "{pr}");
        assert!(!tcpr(table(&d), 0, 0).contains("Borders"));
    }

    #[test]
    fn nested_tables_stay_nested() {
        let d = doc(
            "<table><tr><td>outer<table><tr><td>in1</td><td>in2</td></tr></table></td>\
             <td><table><tr><td>last</td></tr></table></td></tr></table><p>after</p>",
        );
        let t = table(&d);
        assert_eq!(t.rows[0].cells.len(), 2);
        let first = &t.rows[0].cells[0].blocks;
        assert!(matches!(&first[0], DocBlock::Paragraph(p) if p.plain_text() == "outer"));
        let inner = nested(t, 0, 0);
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].rows[0].cells.len(), 2);
        // A cell ends in a paragraph after its table.
        for c in &t.rows[0].cells {
            assert!(
                matches!(c.blocks.last(), Some(DocBlock::Paragraph(_))),
                "{:?}",
                c.blocks
            );
        }
        assert_eq!(nested(t, 0, 1).len(), 1);
        assert_eq!(
            paragraph_texts(&d).last().map(String::as_str),
            Some("after")
        );
        // Past MAX_TABLE_NESTING a table is its cell's text.
        let depth = MAX_TABLE_NESTING + 3;
        let mut page = String::new();
        for k in 0..depth {
            page.push_str(&format!("<table><tr><td>t{k}"));
        }
        for _ in 0..depth {
            page.push_str("</td></tr></table>");
        }
        let d = doc(&page);
        let mut t = table(&d);
        let mut levels = 1;
        while let Some(n) = nested(t, 0, 0).first().copied() {
            t = n;
            levels += 1;
        }
        assert_eq!(levels, MAX_TABLE_NESTING);
        let deepest: Vec<String> = t.rows[0].cells[0]
            .blocks
            .iter()
            .map(|b| b.plain_text())
            .collect();
        assert!(deepest.contains(&format!("t{}", depth - 1)), "{deepest:?}");
        // 300 unclosed nested tables are bounded.
        let page = "<table><tr><td>x".repeat(300);
        let d = doc(&page);
        assert!(paragraph_texts(&d).len() >= 300);
    }

    #[test]
    fn a_nested_full_width_table_fills_its_cell_not_the_outer_table() {
        let d = doc(
            "<table width=600 cellpadding=0><tr><td>left<table width=100%><tr><td>in</td></tr></table></td>\
             <td>right</td></tr></table>",
        );
        let t = table(&d);
        assert_eq!(t.grid, [4500, 4500]);
        let inner = nested(t, 0, 0)[0];
        assert_eq!(inner.grid, [4500]);
        assert!(
            inner
                .raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblW w:w=\"5000\" w:type=\"pct\"/>")
        );
        // A cell's padding narrows the room its table has.
        let d = doc(
            "<table width=600><tr><td style='padding:0 10px'><table width=100%><tr><td>in</td></tr></table></td>\
             <td>right</td></tr></table>",
        );
        assert_eq!(nested(table(&d), 0, 0)[0].grid, [4200]);
    }

    #[test]
    fn an_unclosed_empty_spacer_cell_keeps_its_width_and_shading() {
        let d = doc("<table><tr><td>a<td width=20 bgcolor=red></tr></table>");
        let t = table(&d);
        assert_eq!(t.rows[0].cells.len(), 2);
        assert_eq!(tcw(t, 0, 1), 300);
        assert!(tcpr(t, 0, 1).contains("w:fill=\"FF0000\""));
        // Ended by the table, too.
        let d = doc("<table><tr><td>a<td bgcolor=red></table>");
        assert_eq!(table(&d).rows[0].cells.len(), 2);
    }

    #[test]
    fn an_imported_html_table_round_trips_through_docx() {
        let d = doc(
            "<table width=600 border=1><tr><td colspan=2 bgcolor=red>h</td><td rowspan=2>s</td></tr>\
             <tr><td>a<table><tr><td>in</td></tr></table></td><td>b</td></tr></table>",
        );
        let xml = crate::serialize::document_to_xml(&d);
        let back = crate::load::parse_document_xml(&xml, &crate::load::Relationships::default());
        let (a, b) = (table(&d), table(&back));
        assert_eq!(a.grid, b.grid);
        let shape =
            |t: &crate::model::Table| -> Vec<Vec<(u32, crate::model::VMerge, Option<String>)>> {
                t.rows
                    .iter()
                    .map(|r| {
                        r.cells
                            .iter()
                            .map(|c| (c.grid_span, c.v_merge, c.raw_tcpr.clone()))
                            .collect()
                    })
                    .collect()
            };
        assert_eq!(shape(a), shape(b));
        assert_eq!(a.raw_tblpr, b.raw_tblpr);
        assert_eq!(nested(b, 1, 0).len(), 1);
        assert_eq!(nested(a, 1, 0)[0].grid, nested(b, 1, 0)[0].grid);
        assert_eq!(paragraph_texts(&d), paragraph_texts(&back));
    }
}

#[cfg(test)]
mod image_tests {
    //! `<img>` with images on (#1880).
    use super::*;
    use crate::image::{inline_embed, inline_extent};
    use crate::import::paragraph_texts;
    use crate::model::Paragraph;

    /// A PNG header `image::sniff` reads as `w` x `h`.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        out.extend(w.to_be_bytes());
        out.extend(h.to_be_bytes());
        out.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
        out
    }

    /// `html` imported with a resolver giving a 40 x 20 PNG for any src
    /// but `missing…`, and the srcs it was asked for.
    fn with_images(html: &str) -> (Document, Vec<ImportedMedia>, Vec<String>) {
        let mut asked = Vec::new();
        let mut resolve = |src: &str, _: usize| {
            asked.push(src.to_string());
            (!src.starts_with("missing")).then(|| png(40, 20))
        };
        let (doc, media) =
            import_html_within(html.as_bytes(), &Budget::standard(), Some(&mut resolve)).unwrap();
        (doc, media, asked)
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

    fn pictures(p: &Paragraph) -> Vec<&str> {
        p.content
            .iter()
            .filter_map(|i| match i {
                Inline::Raw(r) if inline_embed(r).is_some() => Some(r.as_str()),
                _ => None,
            })
            .collect()
    }

    fn all_pictures(d: &Document) -> Vec<&str> {
        paras(d).into_iter().flat_map(pictures).collect()
    }

    fn sizes(d: &Document) -> Vec<(i64, i64)> {
        all_pictures(d)
            .iter()
            .map(|r| inline_extent(r).unwrap())
            .collect()
    }

    /// `bytes` in base64, by hand (the module's decoder is under test).
    fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = c.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - c.len()));
            for k in 0..=c.len() {
                out.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            }
            out.push_str(&"=".repeat(3 - c.len()));
        }
        out
    }

    const PX: i64 = 9525;

    #[test]
    fn words_not_vml_image_is_read_and_its_vml_twin_is_not() {
        let (d, media, asked) = with_images(
            "<p>a<!--[if gte vml 1]><v:shape><v:imagedata src=\"x_files/image001.png\"/></v:shape><![endif]--><![if !vml]><img width=96 height=48 src=\"x_files/image002.png\" v:shapes=\"_x0000_i1025\"><![endif]>b<![if !supportLineBreakNewLine]><br><![endif]>c</p>",
        );
        assert_eq!(asked, ["x_files/image002.png"]);
        assert_eq!(sizes(&d), [(96 * PX, 48 * PX)]);
        assert_eq!(media.len(), 1);
        // Everything else in a downlevel block stays out.
        assert_eq!(paragraph_texts(&d), ["abc"]);
        // `<![if !vml]>` only: another condition's image is not read.
        let (d, _, asked) = with_images("<p><![if !mso]><img src=a.png><![endif]>x</p>");
        assert!(asked.is_empty());
        assert!(all_pictures(&d).is_empty());
        let (d, _, _) = with_images("<p><![if ! VML ]><img src=a.png><![endif]>x</p>");
        assert_eq!(all_pictures(&d).len(), 1);
    }

    #[test]
    fn a_picture_bullet_is_no_picture_and_stays_a_bullet() {
        let (d, _, asked) = with_images(
            "<p class=MsoListParagraph><![if !supportLists]><img width=11 height=11 src=\"x_files/image003.gif\" alt=\"*\"><span>&nbsp; </span><![endif]>Apples</p>",
        );
        assert!(asked.is_empty());
        assert!(all_pictures(&d).is_empty());
        assert_eq!(paragraph_texts(&d), ["Apples"]);
        assert_eq!(paras(&d)[0].props.num_id, Some(1));
    }

    #[test]
    fn a_typed_marker_never_takes_a_picture_with_it() {
        // Filtered HTML's marker, then a picture.
        let (d, _, _) = with_images(
            "<p class=MsoListParagraph>\u{b7}&nbsp;<img src=a.png width=10 height=10>text</p>",
        );
        let p = paras(&d)[0];
        assert_eq!(pictures(p).len(), 1, "{:?}", p.content);
        assert_eq!(p.plain_text(), "text");
        assert_eq!(p.props.num_id, Some(1));
        // A picture before the marker: no marker taken.
        let (d, _, _) =
            with_images("<p class=MsoListParagraph><img src=a.png>\u{b7}&nbsp;text</p>");
        let p = paras(&d)[0];
        assert_eq!(pictures(p).len(), 1);
        assert_eq!(p.props.num_id, None);
        assert_eq!(p.plain_text(), "\u{b7}\u{a0}text");
    }

    #[test]
    fn a_picture_is_content_of_its_paragraph() {
        let (d, _, _) = with_images("<p><img src=a.png></p><p>&nbsp;</p>");
        assert_eq!(paras(&d).len(), 2);
        assert_eq!(pictures(paras(&d)[0]).len(), 1);
        assert!(paras(&d)[1].content.is_empty());
        let (d, _, _) = with_images("<div><img src=a.png></div><div>x</div>");
        let p = paras(&d);
        assert_eq!(p.len(), 2, "{:?}", d.body);
        assert_eq!(pictures(p[0]).len(), 1);
        assert_eq!(p[1].plain_text(), "x");
        // Spaces round a picture collapse as round a word.
        let (d, _, _) = with_images("<p>  a <img src=a.png>  b </p>");
        assert_eq!(paragraph_texts(&d), ["a  b"]);
        let (d, _, _) = with_images("<p><img src=a.png> b</p>");
        assert_eq!(paragraph_texts(&d), [" b"]);
    }

    #[test]
    fn css_lengths_beat_the_attributes_and_auto_and_percent_do_not() {
        let (d, _, _) = with_images(
            "<style>img.w { width: 80px }</style>\
             <p><img src=a.png width=200 style='width:100px'>\
             <img src=a.png width=200 height=50 style='height:auto'>\
             <img src=a.png width=200 style='width:auto'>\
             <img src=a.png width=200 style='width:50%'>\
             <img src=a.png width=50%>\
             <img class=w src=a.png height=10>\
             <img src=a.png style='width:1in;height:0.5in'></p>",
        );
        assert_eq!(
            sizes(&d),
            [
                (100 * PX, 50 * PX),
                (200 * PX, 100 * PX),
                // `auto` clears the attribute: the pixels.
                (40 * PX, 20 * PX),
                (200 * PX, 100 * PX),
                (40 * PX, 20 * PX),
                // The rule's width beats the attribute's height's aspect.
                (80 * PX, 10 * PX),
                (96 * PX, 48 * PX),
            ]
        );
    }

    fn shapes(d: &Document) -> Vec<crate::image::PictureShape> {
        all_pictures(d)
            .iter()
            .map(|r| crate::image::inline_shape(r))
            .collect()
    }

    #[test]
    fn border_radius_crops_a_picture_through_the_cascade() {
        use crate::image::PictureShape::{Ellipse, Rect, RoundRect};
        let (d, _, _) = with_images(
            "<style>img.avatar { border-radius: 50% } img.later { border-radius: 4px }</style>\
             <p><img src=a.png width=32 height=32 style='border-radius:50%'>\
             <img class=avatar src=a.png width=32 height=32>\
             <img src=a.png width=32 height=32 style='border-radius:16px'>\
             <img src=a.png width=120 height=40 style='border-radius:8px'>\
             <img src=a.png width=120 height=40 style='border-radius:10%'>\
             <img src=a.png width=120 height=40>\
             <img class=later src=a.png width=120 height=40 style='border-radius:8px'>\
             <img src=a.png width=32 height=32 style='border-radius:50%;border-radius:-2px'>\
             <img src=a.png width=32 height=32 style='border-radius:50%;border-radius:red'>\
             <img src=a.png width=32 height=32 style='border-radius:0'></p>",
        );
        assert_eq!(
            shapes(&d),
            [
                Ellipse,
                Ellipse,
                Ellipse,
                RoundRect { adj: 20_000 },
                RoundRect { adj: 10_000 },
                Rect,
                // The inline style beats the rule.
                RoundRect { adj: 20_000 },
                // An invalid value leaves the one before it.
                Ellipse,
                Ellipse,
                Rect,
            ]
        );
        // No radius: the current rectangle writer's run (pinned to the
        // pre-#1926 bytes in `image::tests`).
        let plain = all_pictures(&d)[5];
        let (id, rid) = (6, inline_embed(plain).unwrap());
        assert_eq!(
            plain,
            crate::image::inline_picture_run_described(
                &rid,
                120 * PX,
                40 * PX,
                id,
                "a.png",
                "",
                None
            )
        );
    }

    #[test]
    fn border_radius_longhands_count_when_all_four_agree() {
        use crate::image::PictureShape::{Ellipse, Rect, RoundRect};
        let all = "border-top-left-radius:8px;border-top-right-radius:8px;\
                   border-bottom-right-radius:8px;border-bottom-left-radius:8px 2px";
        let (d, _, _) = with_images(&format!(
            "<p><img src=a.png width=120 height=40 style='{all}'>\
             <img src=a.png width=120 height=40 style='border-top-left-radius:8px'>\
             <img src=a.png width=32 height=32 style='border-radius:50%;border-top-left-radius:0'>\
             <img src=a.png width=32 height=32 style='border-radius:8px 8px 0 0'>\
             <img src=a.png width=32 height=32 style='border-radius:6pt 8px'>\
             <img src=a.png width=32 height=32 style='border-radius:50% / 10%'>\
             <img src=a.png width=32 height=32 style='border-radius:0 0%'></p>"
        ));
        assert_eq!(
            shapes(&d),
            [
                RoundRect { adj: 20_000 },
                // One corner rounded alone: Word has no such crop here.
                Rect,
                Rect,
                Rect,
                // 6pt is 8px: the same corner.
                RoundRect { adj: 25_000 },
                // The vertical radii are dropped.
                Ellipse,
                Rect,
            ]
        );
    }

    #[test]
    fn border_radius_is_the_images_own_and_em_takes_its_final_font() {
        use crate::image::PictureShape::{Rect, RoundRect};
        let (d, _, _) = with_images(
            "<div style='border-radius:50%;background:#eee;padding:8px'>\
             <p style='border-radius:50%'><img src=a.png width=32 height=32></p>\
             <span style='border-radius:8px'><img src=a.png width=32 height=32></span>\
             <img src=a.png width=32 height=32 style='border-radius:1em;font-size:8px'>\
             <img src=a.png width=32 height=32 style='font-size:4px;border-radius:2em'>\
             </div>",
        );
        // `1em` declared before `font-size:8px` is 8px (the default 16px
        // font would make it a circle).
        assert_eq!(
            shapes(&d),
            [
                Rect,
                Rect,
                RoundRect { adj: 25_000 },
                RoundRect { adj: 25_000 }
            ]
        );
        // Never text.
        for t in paragraph_texts(&d) {
            assert!(!t.contains('%') && !t.contains("px"), "{t}");
        }
    }

    #[test]
    fn border_radius_crops_a_placeholder_and_a_picture_fitted_to_the_body() {
        use crate::image::PictureShape::{Ellipse, RoundRect};
        let (d, _, _) = with_images(
            "<p><img src=missing.png width=32 height=32 alt=Ann style='border-radius:50%'>\
             <img src=a.png width=1248 height=416 style='border-radius:8px'>\
             <img src=a.png style='border-radius:4px'></p>",
        );
        assert_eq!(
            shapes(&d),
            // 8 of 416 px, though drawn at 624 x 208; 4 of the pixels' 20.
            [Ellipse, RoundRect { adj: 1923 }, RoundRect { adj: 20_000 }]
        );
        let placeholder = all_pictures(&d)[0];
        assert!(
            placeholder
                .contains(r#"<a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom><a:ln w="9525">"#)
        );
        assert!(placeholder.contains(r#"descr="Ann""#));
    }

    #[test]
    fn a_wide_picture_is_scaled_to_the_body() {
        let (d, _, _) = with_images("<p><img src=a.png width=1248 height=100></p>");
        assert_eq!(sizes(&d), [(624 * PX, 50 * PX)]);
        // FIX r2 m1: sides past a page's 2112 px keep their aspect.
        let (d, _, _) = with_images(
            "<p><img src=a.png width=3000 height=1000>\
             <img src=a.png style='width:2400px;height:600px'>\
             <img src=a.png width=4000>\
             <img src=a.png style='height:3000px'>\
             <img src=missing.png width=99999999999 height=1></p>",
        );
        assert_eq!(
            sizes(&d),
            [
                (624 * PX, 208 * PX),
                (624 * PX, 156 * PX),
                // One side: the 40 x 20 pixels' aspect.
                (624 * PX, 312 * PX),
                (624 * PX, 312 * PX),
                // Saturating, then scaled: a height under a twip is one EMU.
                (624 * PX, 1),
            ]
        );
    }

    /// FIX r2b: an uncapped side over an extreme aspect overflowed u64 in
    /// `picture_twips` (now `declared_box` and `fit_to_body`).
    #[test]
    fn an_extreme_aspect_never_overflows() {
        let page = format!(
            "<p><img width=99999999999 src=\"data:image/png;base64,{}\">\
             <img height=99999999999 src=\"data:image/png;base64,{}\"></p>",
            b64(&png(1, 1_000_000)),
            b64(&png(1_000_000, 1))
        );
        let mut none = |_: &str, _: usize| None;
        let (d, _) =
            import_html_within(page.as_bytes(), &Budget::standard(), Some(&mut none)).unwrap();
        let got = sizes(&d);
        assert_eq!(got.len(), 2);
        for (cx, cy) in got {
            for side in [cx, cy] {
                assert!((1..=crate::image::MAX_EMU).contains(&side), "{side}");
            }
            assert_eq!(cx, 624 * PX, "scaled to the body");
        }
    }

    #[test]
    fn height_still_means_nothing_on_a_table() {
        let page =
            "<table height=40><tr height=30><td height=20 style='height:9px'>x</td></tr></table>";
        let (with, _, _) = with_images(page);
        let plain = import_html(page.replace("height", "data-h").as_bytes()).unwrap();
        assert_eq!(with, plain);
    }

    #[test]
    fn hidden_zero_sized_and_unresolved_images() {
        let (d, media, asked) = with_images(
            "<p>a<img src=hid.png style='display:none'><span style='mso-hide:all'><img src=hid2.png></span>\
             <img src=zero.png width=0 height=1><img src=zero2.png style='height:0'>b</p>\
             <p><img src=missing.png width=30 height=20 alt='A &amp; B'><img src=missing.png width=30>\
             <img src=missing.png alt=' Shop '><img src=missing.png></p>",
        );
        assert_eq!(asked, ["missing.png"]);
        let p = paras(&d);
        assert_eq!(p[0].plain_text(), "ab");
        assert!(pictures(p[0]).is_empty());
        let pics = pictures(p[1]);
        assert_eq!(
            pics.iter()
                .map(|r| inline_extent(r).unwrap())
                .collect::<Vec<_>>(),
            [(30 * PX, 20 * PX), (30 * PX, 30 * PX)]
        );
        assert!(pics[0].contains("descr=\"A &amp; B\""));
        assert!(pics[0].contains("<a:ln w=\"9525\">"));
        assert!(!pics[1].contains("descr="));
        assert_eq!(p[1].plain_text(), "Shop");
        // One placeholder medium for both.
        assert_eq!(inline_embed(pics[0]), inline_embed(pics[1]));
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].bytes, html_image::PLACEHOLDER_PNG);
    }

    #[test]
    fn alt_text_is_capped() {
        let alt = "x".repeat(5000);
        let (d, _, _) = with_images(&format!(
            "<p><img src=missing.png width=1 height=1 alt='{alt}'></p>"
        ));
        let raw = all_pictures(&d)[0];
        assert!(raw.contains(&format!("descr=\"{}\"", "x".repeat(html_image::MAX_ALT))));
        assert!(!raw.contains(&"x".repeat(html_image::MAX_ALT + 1)));
    }

    #[test]
    fn a_data_image_over_the_room_is_a_placeholder_and_the_import_goes_on() {
        let png = png(4, 2);
        let page = format!(
            "<p>x<img src=\"data:image/png;base64,{}\" width=10 height=5 alt=big></p>",
            b64(&png)
        );
        let mut none = |_: &str, _: usize| None;
        let roomy = Budget::standard();
        let (d, media) = import_html_within(page.as_bytes(), &roomy, Some(&mut none)).unwrap();
        assert_eq!(media[0].bytes, png);
        assert!(!all_pictures(&d)[0].contains("<a:ln"));
        // Room for less than the image: a placeholder, not a failure.
        let tight = Budget::new(png.len() - 1, crate::import::MAX_WORK);
        let (d, media) = import_html_within(page.as_bytes(), &tight, Some(&mut none)).unwrap();
        assert_eq!(media[0].bytes, html_image::PLACEHOLDER_PNG);
        assert!(all_pictures(&d)[0].contains("descr=\"big\""));
        assert_eq!(paragraph_texts(&d), ["x"]);
    }

    /// FIX r1 m2: a work budget that covers the up-front passes and runs out
    /// among the images fails the import, though the same page and budget
    /// with images off fit: decoding is charged, and running out is not
    /// swallowed by the image path.
    #[test]
    fn images_spend_the_imports_work_and_running_out_fails_it() {
        // Distinct lengths: no two media are compared byte for byte.
        let page: String = (0..200)
            .map(|i| {
                let mut b = png(4, 2);
                b.resize(4000 + i, 0);
                format!(
                    "<p>{i}<img src=\"data:image/png;base64,{}\" width=10 height=5></p>",
                    b64(&b)
                )
            })
            .collect();
        let mut none = |_: &str, _: usize| None;
        let full = Budget::standard();
        let (_, media) = import_html_within(page.as_bytes(), &full, Some(&mut none)).unwrap();
        assert_eq!(media.len(), 200);
        let upfront = 3 * (page.len() / 16 + 1);
        let spent = crate::import::MAX_WORK - full.work.get();
        let half = || Budget::new(crate::import::MAX_DECODED, upfront + (spent - upfront) / 2);
        assert!(import_html_within(page.as_bytes(), &half(), None).is_ok());
        assert_eq!(
            import_html_within(page.as_bytes(), &half(), Some(&mut none)).unwrap_err(),
            TOO_BIG
        );
    }

    #[test]
    fn the_resolver_is_told_the_room() {
        let mut limits = Vec::new();
        let mut resolve = |_: &str, limit: usize| {
            limits.push(limit);
            None
        };
        let b = Budget::new(1000, crate::import::MAX_WORK);
        import_html_within(b"<p>x<img src=a.png></p>", &b, Some(&mut resolve)).unwrap();
        assert_eq!(limits, [1000]);
    }
}

#[cfg(test)]
mod link_tests {
    //! `<a href>` links (#1879).
    use super::*;
    use crate::import::paragraph_texts;
    use crate::model::{Block as DocBlock, Hyperlink, Paragraph, Run};

    const U: &str = "https://x.test/";

    fn doc(html: &str) -> Document {
        import_html(html.as_bytes()).unwrap()
    }

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

    fn links(d: &Document) -> Vec<Hyperlink> {
        all_paras(d)
            .iter()
            .flat_map(|p| p.content.clone())
            .filter_map(|i| match i {
                Inline::Hyperlink(h) => Some(h),
                _ => None,
            })
            .collect()
    }

    /// One paragraph's inlines as a sketch: `Run "x"`, `Link <target or
    /// #anchor> ["a", "b"]`, `Break`, `Tab`, `Object`.
    fn sketch(p: &Paragraph) -> Vec<String> {
        p.content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => format!("Run {:?}", r.text),
                Inline::Hyperlink(h) => {
                    let to = match (&h.target, &h.anchor) {
                        (Some(t), None) => t.clone(),
                        (None, Some(a)) => format!("#{a}"),
                        other => format!("{other:?}"),
                    };
                    let runs: Vec<&str> = h.runs.iter().map(|r| r.text.as_str()).collect();
                    format!("Link {to} {runs:?}")
                }
                Inline::Break(..) => "Break".into(),
                Inline::Tab(_) => "Tab".into(),
                _ => "Object".into(),
            })
            .collect()
    }

    fn first(d: &Document) -> Vec<String> {
        sketch(&all_paras(d)[0])
    }

    fn link_run<'a>(h: &'a Hyperlink, text: &str) -> &'a Run {
        h.runs
            .iter()
            .find(|r| r.text == text)
            .unwrap_or_else(|| panic!("no run {text:?} in {h:?}"))
    }

    #[test]
    fn a_href_becomes_a_hyperlink_with_its_formatting() {
        let d = doc(r#"<p>x <a href="https://example.com/a?x=1&amp;y=2">see <b>this</b></a></p>"#);
        assert_eq!(
            first(&d),
            [
                r#"Run "x ""#,
                r#"Link https://example.com/a?x=1&y=2 ["see ", "this"]"#
            ]
        );
        let h = &links(&d)[0];
        assert_eq!(
            (&h.rel_id, &h.raw, h.content.is_empty()),
            (&None, &None, true)
        );
        // The user-agent link look, under the formatting inside.
        let see = link_run(h, "see ");
        assert!(see.props.underline && !see.props.bold);
        assert_eq!(see.props.color.as_deref(), Some(LINK_COLOR));
        let this = link_run(h, "this");
        assert!(this.props.underline && this.props.bold);
        assert_eq!(this.props.color.as_deref(), Some(LINK_COLOR));
        // Text outside has none of it.
        let Inline::Run(x) = &all_paras(&d)[0].content[0] else {
            panic!()
        };
        assert!(!x.props.underline && x.props.color.is_none());
    }

    #[test]
    fn the_page_s_css_overrides_the_link_look() {
        let plain = |d: &Document, color: Option<&str>| {
            let h = &links(d)[0];
            assert!(!h.runs[0].props.underline, "{h:?}");
            assert_eq!(h.runs[0].props.color.as_deref(), color, "{h:?}");
        };
        plain(
            &doc(
                r#"<p><a href="https://x.test/" style="text-decoration:none;color:#f00">r</a></p>"#,
            ),
            Some("FF0000"),
        );
        // Gmail's reset: the parent's colour, not the user agent's blue.
        plain(
            &doc(r#"<style>a{color:inherit;text-decoration:none}</style>
                   <p style="color:red"><a href="https://x.test/">r</a></p>"#),
            Some("FF0000"),
        );
        plain(
            &doc(
                r#"<style>a{color:inherit;text-decoration:none}</style><p><a href="https://x.test/">r</a></p>"#,
            ),
            None,
        );
        // A span inside a link keeps its own colour.
        let d =
            doc(r#"<p><a href="https://x.test/">a <span style="color:#00ff00">g</span></a></p>"#);
        let ls = links(&d);
        let g = link_run(&ls[0], "g");
        assert_eq!(g.props.color.as_deref(), Some("00FF00"));
        assert!(g.props.underline);
        // `inherit` is the parent's colour over a presentational one too.
        let d = doc(r#"<p style="color:blue"><font color=red style="color:inherit">f</font></p>"#);
        let Inline::Run(f) = &all_paras(&d)[0].content[0] else {
            panic!()
        };
        assert_eq!(f.props.color.as_deref(), Some("0000FF"));
    }

    #[test]
    fn an_a_that_is_not_a_link_has_no_link_look() {
        for page in [
            "<p><a name=top>n</a></p>",
            "<p><a>n</a></p>",
            "<p><a href=\"javascript:alert(1)\">n</a></p>",
        ] {
            let d = doc(page);
            assert_eq!(first(&d), [r#"Run "n""#], "{page}");
            let Inline::Run(r) = &all_paras(&d)[0].content[0] else {
                panic!()
            };
            assert!(!r.props.underline && r.props.color.is_none(), "{page}");
        }
    }

    #[test]
    fn adjacent_text_of_one_link_joins_and_other_links_do_not() {
        let d = doc(&format!(
            "<p><a href={U}>a</a><a href={U}><i>b</i></a><a href=https://y.test/>c</a></p>"
        ));
        assert_eq!(
            first(&d),
            [
                format!(r#"Link {U} ["a", "b"]"#),
                r#"Link https://y.test/ ["c"]"#.to_string()
            ]
        );
        // A link's own text in the same formatting is one run.
        let d = doc(&format!(
            "<p><a href={U}>one <span>two</span> three</a></p>"
        ));
        assert_eq!(first(&d), [format!(r#"Link {U} ["one two three"]"#)]);
    }

    #[test]
    fn a_fragment_href_is_an_anchor_link() {
        let d = doc("<p><a href=#intro>i</a> <a href='#caf%C3%A9'>c</a> <a href=#>n</a></p>");
        assert_eq!(
            first(&d),
            [
                r#"Link #intro ["i"]"#,
                r#"Run " ""#,
                r#"Link #café ["c"]"#,
                r#"Run " n""#
            ]
        );
        let h = &links(&d)[0];
        assert_eq!(
            (h.target.as_deref(), h.anchor.as_deref()),
            (None, Some("intro"))
        );
    }

    #[test]
    fn unsafe_and_relative_hrefs_are_not_links() {
        for href in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "java&#9;script:alert(1)",
            "java&#10;script:alert(1)",
            " javascript:alert(1)",
            "&#106;avascript:alert(1)",
            "&#x01;javascript:alert(1)",
            "vbscript:msgbox",
            "data:text/html,<b>x</b>",
            "file:///etc/passwd",
            "about:blank",
            "chrome://settings",
            "C:\\x.html",
            "page.html",
            "/x",
            "//host/x",
            "?q=1",
            "a/b:c",
            "",
            "   ",
            "mailto:",
            "https:",
        ] {
            let d = doc(&format!("<p>a <a href=\"{href}\">t</a> b</p>"));
            assert!(links(&d).is_empty(), "{href:?}: {:?}", first(&d));
            assert_eq!(paragraph_texts(&d), ["a t b"], "{href:?}");
        }
        let d = doc("<p><a name=x>t</a> <a id=y>u</a></p>");
        assert!(links(&d).is_empty());
    }

    #[test]
    fn allowed_schemes_in_any_case_are_links() {
        for (href, target) in [
            ("https://x.test/", "https://x.test/"),
            ("HTTPS://x.test/A", "https://x.test/A"),
            ("http://x.test/", "http://x.test/"),
            ("ftp://x.test/f", "ftp://x.test/f"),
            ("MailTo:sam@example.com", "mailto:sam@example.com"),
            (" \t https://x.test/ \n", "https://x.test/"),
            ("ht&#9;tps://x.test/a&#10;b", "https://x.test/ab"),
        ] {
            let d = doc(&format!("<p><a href=\"{href}\">t</a></p>"));
            assert_eq!(first(&d), [format!(r#"Link {target} ["t"]"#)], "{href:?}");
        }
    }

    #[test]
    fn an_href_keeps_a_named_reference_before_an_equals_sign() {
        // r1: HTML keeps `&name=` / `&nameX` in an attribute as written.
        for (href, target) in [
            (
                "https://shop.test/?id=3&copy=2",
                "https://shop.test/?id=3&copy=2",
            ),
            (
                "https://shop.test/?a=1&para=1",
                "https://shop.test/?a=1&para=1",
            ),
            ("https://shop.test/?r=&reg2", "https://shop.test/?r=&reg2"),
            (
                "https://shop.test/?a=1&amp;b=2",
                "https://shop.test/?a=1&b=2",
            ),
            ("https://shop.test/&copy;", "https://shop.test/\u{a9}"),
            ("https://shop.test/&copy x", "https://shop.test/\u{a9}%20x"),
            ("https://shop.test/&#169=", "https://shop.test/\u{a9}="),
        ] {
            let d = doc(&format!("<p><a href=\"{href}\">t</a></p>"));
            assert_eq!(links(&d)[0].target.as_deref(), Some(target), "{href:?}");
        }
        // Text is decoded as before.
        assert_eq!(paragraph_texts(&doc("<p>a&copy=2</p>")), ["a\u{a9}=2"]);
    }

    #[test]
    fn link_href_limits() {
        // Normalized length: at the limit a link, past it none.
        let at = format!("{U}{}", "a".repeat(MAX_HREF - U.len()));
        assert_eq!(
            links(&doc(&format!("<p><a href={at}>t</a></p>")))[0].target,
            Some(at)
        );
        let over = format!("{U}{}", "a".repeat(MAX_HREF - U.len() + 1));
        assert!(links(&doc(&format!("<p><a href={over}>t</a></p>"))).is_empty());
        // Encoding counts: an href within the limit whose spaces make it
        // longer than it is not a link.
        let spaced = format!("{U}{}", " a".repeat((MAX_HREF - U.len()) / 2));
        assert!(spaced.len() <= MAX_HREF);
        assert!(links(&doc(&format!("<p><a href=\"{spaced}\">t</a></p>"))).is_empty());
        // A huge attribute is refused before any work on it.
        let huge = format!("{U}{}", "\t".repeat(3 * MAX_HREF));
        assert!(link_of(&huge).is_none());
        // A control character other than a tab or line break.
        for bad in [
            "https://x.test/a&#1;b",
            "https://x.test/a&#127;b",
            "#a&#1;b",
            "#a%01b",
        ] {
            let d = doc(&format!("<p><a href=\"{bad}\">t</a></p>"));
            assert!(links(&d).is_empty(), "{bad:?}");
        }
        // Space, `"`, `<`, `>` and backtick are percent-encoded.
        let d = doc("<p><a href='https://x.test/a&quot;b&lt;c&gt; d`e'>t</a></p>");
        assert_eq!(
            links(&d)[0].target.as_deref(),
            Some("https://x.test/a%22b%3Cc%3E%20d%60e")
        );
    }

    #[test]
    fn spaces_around_and_inside_a_link() {
        let d = doc(&format!("<p>x <a href={U}>foo  bar</a> y</p>"));
        assert_eq!(
            first(&d),
            [
                r#"Run "x ""#.to_string(),
                format!(r#"Link {U} ["foo bar"]"#),
                r#"Run " y""#.to_string()
            ]
        );
        let d = doc(&format!("<p><a href={U}>x </a>y</p>"));
        assert_eq!(
            first(&d),
            [format!(r#"Link {U} ["x "]"#), r#"Run "y""#.to_string()]
        );
        // A space owed at the paragraph's end is never written.
        let d = doc(&format!("<p><a href={U}> x </a></p>"));
        assert_eq!(first(&d), [format!(r#"Link {U} ["x"]"#)]);
    }

    #[test]
    fn a_link_across_paragraphs_and_breaks() {
        let d = doc(&format!("<p><a href={U}>one</p><p>two</a> three</p>"));
        let ps = all_paras(&d);
        assert_eq!(sketch(&ps[0]), [format!(r#"Link {U} ["one"]"#)]);
        assert_eq!(
            sketch(&ps[1]),
            [
                format!(r#"Link {U} ["two"]"#),
                r#"Run " three""#.to_string()
            ]
        );
        let split = |inner: &str| {
            vec![
                format!(r#"Link {U} ["a"]"#),
                inner.to_string(),
                format!(r#"Link {U} ["b"]"#),
            ]
        };
        let d = doc(&format!("<p><a href={U}>a<br>b</a></p>"));
        assert_eq!(first(&d), split("Break"));
        let d = doc(&format!(
            "<p><a href={U}>a<span style='mso-tab-count:1'>&nbsp; </span>b</a></p>"
        ));
        assert_eq!(first(&d), split("Tab"));
        let d = doc(&format!("<pre><a href={U}>a\nb</a></pre>"));
        assert_eq!(first(&d), split("Break"));
    }

    #[test]
    fn a_picture_in_a_link_stays_outside_it_and_its_alt_text_is_in_it() {
        let mut resolve = |src: &str, _: usize| {
            (src == "logo.png").then(|| {
                // A 1 x 1 PNG's header is all `sniff` and the size read.
                let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
                png.extend(1u32.to_be_bytes());
                png.extend(1u32.to_be_bytes());
                png.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
                png
            })
        };
        let page = format!(
            "<p><a href={U}>x<img src=logo.png width=10 height=10>y</a></p>\
             <p><a href={U}><img src=missing.png alt=Logo></a></p>"
        );
        let (d, _) =
            import_html_within(page.as_bytes(), &Budget::standard(), Some(&mut resolve)).unwrap();
        let ps = all_paras(&d);
        assert_eq!(
            sketch(&ps[0]),
            [
                format!(r#"Link {U} ["x"]"#),
                "Object".into(),
                format!(r#"Link {U} ["y"]"#)
            ]
        );
        assert_eq!(sketch(&ps[1]), [format!(r#"Link {U} ["Logo"]"#)]);
    }

    #[test]
    fn a_link_of_white_space_alone_is_an_empty_paragraph() {
        let linked = doc(&format!("<p><a href={U}>&nbsp;</a></p><p>x</p>"));
        let plain = doc("<p>&nbsp;</p><p>x</p>");
        assert_eq!(linked.body, plain.body);
    }

    #[test]
    fn a_link_is_never_taken_for_a_typed_list_marker() {
        // Filtered HTML's list paragraph with no marker text: a leading
        // short word in a link stays, link and all.
        let d = doc(&format!(
            "<p class=MsoListParagraph><a href={U}>see</a> it</p>"
        ));
        assert_eq!(
            first(&d),
            [format!(r#"Link {U} ["see"]"#), r#"Run " it""#.to_string()]
        );
        assert_eq!(all_paras(&d)[0].props.num_id, None);
    }

    #[test]
    fn an_a_inside_an_a_closes_the_outer_one() {
        let d = doc("<p><a href=https://one.test/>x<a href=https://two.test/>y</a>z</a></p>");
        assert_eq!(
            first(&d),
            [
                r#"Link https://one.test/ ["x"]"#,
                r#"Link https://two.test/ ["y"]"#,
                r#"Run "z""#
            ]
        );
        let Inline::Run(z) = &all_paras(&d)[0].content[2] else {
            panic!()
        };
        assert!(!z.props.underline && z.props.color.is_none());
        // The formatting open inside the outer link goes on in the new one.
        let d = doc("<p><a href=https://one.test/><b>x<a href=https://two.test/>y</a></b></p>");
        let ls = links(&d);
        assert_eq!(ls.len(), 2);
        assert!(link_run(&ls[0], "x").props.bold);
        assert!(link_run(&ls[1], "y").props.bold);
        // Not across a table: the inner link is the cell's, the outer one
        // goes on after the table.
        let d = doc(
            "<a href=https://one.test/>a<table><tr><td><a href=https://two.test/>c</a></td></tr></table>b</a>",
        );
        let targets: Vec<_> = links(&d)
            .iter()
            .map(|h| h.target.clone().unwrap())
            .collect();
        assert_eq!(
            targets,
            [
                "https://one.test/",
                "https://two.test/",
                "https://one.test/"
            ]
        );
    }

    #[test]
    fn a_link_in_a_table_cell() {
        let d = doc(&format!(
            "<table><tr><td>a <a href={U}>b</a></td><td>c</td></tr></table>"
        ));
        let DocBlock::Table(t) = &d.body[0] else {
            panic!("{:?}", d.body)
        };
        let DocBlock::Paragraph(p) = &t.rows[0].cells[0].blocks[0] else {
            panic!()
        };
        assert_eq!(
            sketch(p),
            [r#"Run "a ""#.to_string(), format!(r#"Link {U} ["b"]"#)]
        );
    }
}

#[cfg(test)]
mod centering_tests {
    //! Centring (#1922): tables and block images placed by auto margins
    //! and a container's `-webkit-` alignment; fixed-width blocks.
    use super::*;
    use crate::image::inline_embed;
    use crate::model::{Block as DocBlock, Paragraph, Table};

    fn doc(html: &str) -> Document {
        import_html(html.as_bytes()).unwrap()
    }

    /// `html` imported with every image resolving to a 40 x 20 PNG.
    fn with_images(html: &str) -> Document {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(40u32.to_be_bytes());
        png.extend(20u32.to_be_bytes());
        png.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
        let mut resolve = |_: &str, _: usize| Some(png.clone());
        import_html_within(html.as_bytes(), &Budget::standard(), Some(&mut resolve))
            .unwrap()
            .0
    }

    /// Every table of `d`, top-level and nested, in document order.
    fn tables(d: &Document) -> Vec<&Table> {
        fn walk<'d>(blocks: &'d [DocBlock], out: &mut Vec<&'d Table>) {
            for b in blocks {
                if let DocBlock::Table(t) = b {
                    out.push(t);
                    for row in &t.rows {
                        for cell in &row.cells {
                            walk(&cell.blocks, out);
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&d.body, &mut out);
        out
    }

    /// A table's `w:jc`, if any.
    fn jc(t: &Table) -> Option<String> {
        let pr = t.raw_tblpr.as_deref().unwrap_or("");
        let v = pr.split("<w:jc w:val=\"").nth(1)?;
        Some(v[..v.find('"').unwrap()].to_string())
    }

    /// The `w:jc` of every table of `html`, in document order.
    fn jcs(html: &str) -> Vec<Option<String>> {
        tables(&doc(html)).into_iter().map(jc).collect()
    }

    fn some(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    const T: &str = "<table width=200><tr><td>t</td></tr></table>";

    /// Body paragraphs and the paragraphs of cells, in document order.
    fn all_paras(d: &Document) -> Vec<&Paragraph> {
        fn walk<'d>(blocks: &'d [DocBlock], out: &mut Vec<&'d Paragraph>) {
            for b in blocks {
                match b {
                    DocBlock::Paragraph(p) => out.push(p),
                    DocBlock::Table(t) => {
                        for row in &t.rows {
                            for cell in &row.cells {
                                walk(&cell.blocks, out);
                            }
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

    fn has_picture(p: &Paragraph) -> bool {
        p.content
            .iter()
            .any(|i| matches!(i, Inline::Raw(r) if inline_embed(r).is_some()))
    }

    /// Each paragraph as (its text, whether it holds a picture, its
    /// alignment).
    fn sketch(d: &Document) -> Vec<(String, bool, Align)> {
        all_paras(d)
            .into_iter()
            .map(|p| (p.plain_text(), has_picture(p), p.props.align))
            .collect()
    }

    #[test]
    fn margin_auto_centres_a_table() {
        for style in [
            "margin:0 auto",
            "margin-left:auto;margin-right:auto",
            "margin:10px auto 0",
            "MARGIN: 0 Auto !important",
        ] {
            let html = format!("<table width=600 style='{style}'><tr><td>x</td></tr></table>");
            assert_eq!(jcs(&html), [some("center")], "{style}");
        }
        assert_eq!(
            jcs("<table style='margin-left:auto'><tr><td>x</td></tr></table>"),
            [some("right")]
        );
        assert_eq!(
            jcs("<table style='margin:0 0 0 auto'><tr><td>x</td></tr></table>"),
            [some("right")]
        );
        assert_eq!(
            jcs("<table style='margin-right:auto'><tr><td>x</td></tr></table>"),
            [None]
        );
        // A later declaration replaces the side.
        assert_eq!(
            jcs("<table style='margin:0 auto;margin-left:0'><tr><td>x</td></tr></table>"),
            [None]
        );
        // From a style sheet too.
        assert_eq!(
            jcs("<style>.c{margin:0 auto}</style><table class=c><tr><td>x</td></tr></table>"),
            [some("center")]
        );
        // An invalid margin changes nothing.
        assert_eq!(
            jcs("<table style='margin:0 auto;margin:0 red'><tr><td>x</td></tr></table>"),
            [some("center")]
        );
    }

    #[test]
    fn table_align_center_yields_to_css_margin() {
        assert_eq!(
            jcs("<table align=center><tr><td>x</td></tr></table>"),
            [some("center")]
        );
        assert_eq!(
            jcs("<table align=center style='margin:0'><tr><td>x</td></tr></table>"),
            [None]
        );
        assert_eq!(
            jcs("<table align=center style='margin-left:0'><tr><td>x</td></tr></table>"),
            [None]
        );
    }

    #[test]
    fn webkit_center_ancestors_centre_tables() {
        for wrap in [
            "<center>{T}</center>",
            "<center><div><div>{T}</div></div></center>",
            "<div align=center>{T}</div>",
            "<div align=middle>{T}</div>",
            "<div style='text-align:-webkit-center'>{T}</div>",
            "<div style='text-align:-moz-center'>{T}</div>",
            "<table><tr><td align=center>{T}</td></tr></table>",
            "<table><tr align=center><td>{T}</td></tr></table>",
            "<table><tbody align=center><tr><td>{T}</td></tr></tbody></table>",
            "<table><tr><th align=center>{T}</th></tr></table>",
        ] {
            let html = wrap.replace("{T}", T);
            let got = jcs(&html);
            assert_eq!(got.last(), Some(&some("center")), "{html}");
        }
        for wrap in [
            "<div align=right>{T}</div>",
            "<div style='text-align:-webkit-right'>{T}</div>",
            "<div style='text-align:-moz-right'>{T}</div>",
            "<table><tr><td align=right>{T}</td></tr></table>",
        ] {
            let html = wrap.replace("{T}", T);
            assert_eq!(jcs(&html).last(), Some(&some("right")), "{html}");
        }
        // Through an outer table and its cell: the nested one is centred
        // too, as a browser's standards mode inherits `text-align`.
        assert_eq!(
            jcs(
                "<center><table><tr><td><table><tr><td>x</td></tr></table></td></tr></table></center>"
            ),
            [some("center"), some("center")]
        );
    }

    #[test]
    fn plain_text_align_does_not_centre_tables() {
        for wrap in [
            "<div style='text-align:center'>{T}</div>",
            "<table><tr><td style='text-align:center'>{T}</td></tr></table>",
            "<div align=center><div style='text-align:left'>{T}</div></div>",
            "<center><div style='text-align:center'>{T}</div></center>",
            // A table closes the open `p`: it is not inside it.
            "<p align=center>{T}",
            // A heading's `align` is plain, and clears an inherited one.
            "<center><h2 align=center>{T}</h2></center>",
            "<div align=center><div align=justify>{T}</div></div>",
        ] {
            let html = wrap.replace("{T}", T);
            assert_eq!(jcs(&html).last(), Some(&None), "{html}");
        }
    }

    #[test]
    fn own_margins_beat_ancestor_alignment() {
        assert_eq!(
            jcs(
                "<center><table style='margin-left:auto;margin-right:0'><tr><td>x</td></tr></table></center>"
            ),
            [some("right")]
        );
        assert_eq!(
            jcs("<div align=right><table style='margin:0 auto'><tr><td>x</td></tr></table></div>"),
            [some("center")]
        );
        assert_eq!(
            jcs("<center><table style='margin-right:auto'><tr><td>x</td></tr></table></center>"),
            [None]
        );
        // A table's own text-align is its cells', not its place.
        assert_eq!(
            jcs("<center><table style='text-align:left'><tr><td>x</td></tr></table></center>"),
            [some("center")]
        );
    }

    #[test]
    fn table_float_align_beats_margins_and_ancestors() {
        assert_eq!(
            jcs("<center><table align=right><tr><td>x</td></tr></table></center>"),
            [some("right")]
        );
        assert_eq!(
            jcs(&format!(
                "<table><tr><td align=center><table align=left><tr><td>x</td></tr></table>{T}</td></tr></table>"
            )),
            [None, None, some("center")]
        );
        assert_eq!(
            jcs("<table align=left style='margin:0 auto'><tr><td>x</td></tr></table>"),
            [None]
        );
    }

    #[test]
    fn a_centred_table_keeps_its_cell_text_alignment() {
        let d = doc("<center><table><tr><td>in</td></tr></table></center>\
             <table style='margin:0 auto'><tr><td>own</td></tr></table>\
             <div align=center><table align=center><tr><td style='text-align:left'>left</td></tr></table></div>");
        assert_eq!(
            tables(&d).into_iter().map(jc).collect::<Vec<_>>(),
            [some("center"), some("center"), some("center")]
        );
        let align = |text: &str| {
            all_paras(&d)
                .into_iter()
                .find(|p| p.plain_text() == text)
                .unwrap()
                .props
                .align
        };
        // The ancestors' alignment still reaches the text; the table's own
        // margin never does.
        assert_eq!(align("in"), Align::Center);
        assert_eq!(align("own"), Align::Left);
        assert_eq!(align("left"), Align::Left);
    }

    #[test]
    fn block_image_with_auto_margins_is_its_own_centred_paragraph() {
        let d = with_images(
            "<p style='text-align:justify;margin-left:20px'>before<img src=a.png \
             style='display:block;margin:0 auto'>after</p>",
        );
        assert_eq!(
            sketch(&d),
            [
                ("before".to_string(), false, Align::Justify),
                (String::new(), true, Align::Center),
                ("after".to_string(), false, Align::Justify),
            ]
        );
        // Each keeps the enclosing block's indent.
        assert!(all_paras(&d).iter().all(|p| p.props.indent == 300));
        let d = with_images("<div><img src=a.png style='display:block;margin-left:auto'></div>");
        assert_eq!(sketch(&d), [(String::new(), true, Align::Right)]);
        let d = with_images("<div><img src=a.png style='display:block;margin-right:auto'></div>");
        assert_eq!(sketch(&d), [(String::new(), true, Align::Left)]);
        // An inline image is a run of its paragraph, as before.
        let d = with_images("<p align=center>a <img src=a.png style='margin:0 auto'> b</p>");
        assert_eq!(sketch(&d), [("a  b".to_string(), true, Align::Center)]);
    }

    #[test]
    fn a_block_image_is_placed_by_its_container() {
        let img = "<img src=a.png style='display:block'>";
        for (wrap, want) in [
            (
                "<table><tr><td align=center>{I}</td></tr></table>",
                Align::Center,
            ),
            ("<p align=center>{I}</p>", Align::Center),
            ("<center>{I}</center>", Align::Center),
            ("<div align=right>{I}</div>", Align::Right),
            ("<div style='text-align:center'>{I}</div>", Align::Left),
            ("<h2 align=center>{I}</h2>", Align::Left),
            (
                "<table><tr><td style='text-align:center'>{I}</td></tr></table>",
                Align::Left,
            ),
        ] {
            let html = wrap.replace("{I}", img);
            let got: Vec<Align> = sketch(&with_images(&html))
                .into_iter()
                .filter(|(_, pic, _)| *pic)
                .map(|(_, _, a)| a)
                .collect();
            assert_eq!(got, [want], "{html}");
        }
    }

    #[test]
    fn a_style_sheets_block_image_and_a_display_reset() {
        // An e-mail's reset sheet: every image is a block; one in a link in
        // a centred cell is its own centred paragraph, outside the link.
        let d = with_images(
            "<style>img{display:block;border:0}</style><table><tr><td align=center>\
             <a href='https://example.com/'><img src=logo.png></a>Caption</td></tr></table>",
        );
        assert_eq!(
            sketch(&d),
            [
                (String::new(), true, Align::Center),
                ("Caption".to_string(), false, Align::Center),
            ]
        );
        // A later display undoes it.
        for reset in ["display:inline", "display:inline-block"] {
            let d = with_images(&format!(
                "<style>img{{display:block}}</style><p align=center>a<img src=a.png style='{reset}'>b</p>"
            ));
            assert_eq!(
                sketch(&d),
                [("ab".to_string(), true, Align::Center)],
                "{reset}"
            );
        }
        // `display:none` stays hidden.
        let d = with_images("<p>a<img src=a.png style='display:block;display:none'>b</p>");
        assert_eq!(sketch(&d), [("ab".to_string(), false, Align::Left)]);
    }

    #[test]
    fn block_image_in_a_list_item_is_an_unnumbered_continuation() {
        let num = |d: &Document| -> Vec<(String, Option<i32>, i32, i32)> {
            all_paras(d)
                .into_iter()
                .map(|p| {
                    (
                        p.plain_text(),
                        p.props.num_id,
                        p.props.indent,
                        p.props.first_line,
                    )
                })
                .collect()
        };
        let img = "<img src=a.png style='display:block;margin:0 auto'>";
        let d = with_images(&format!("<ul><li>a{img}b</li><li>c</li></ul>"));
        assert_eq!(
            num(&d),
            [
                ("a".to_string(), Some(1), 0, 0),
                (String::new(), None, 720, 0),
                ("b".to_string(), None, 720, 0),
                ("c".to_string(), Some(1), 0, 0),
            ]
        );
        // The picture opening its item carries the item's number.
        let d = with_images(&format!("<ol><li>{img}c</li><li>d</li></ol>"));
        assert_eq!(
            num(&d),
            [
                (String::new(), Some(2), 0, 0),
                ("c".to_string(), None, 720, 0),
                ("d".to_string(), Some(2), 0, 0),
            ]
        );
    }

    #[test]
    fn a_block_image_ending_a_p_leaves_no_empty_paragraph() {
        let img = "<img src=a.png style='display:block;margin:0 auto'>";
        let pic = (String::new(), true, Align::Center);
        for (html, want) in [
            (format!("<p>{img}</p><p>z</p>"), vec![pic.clone()]),
            (
                format!("<p>t{img}</p><p>z</p>"),
                vec![("t".to_string(), false, Align::Left), pic.clone()],
            ),
            (format!("<p>{img} </p><p>z</p>"), vec![pic.clone()]),
            (
                format!("<p>{img}u</p><p>z</p>"),
                vec![pic.clone(), ("u".to_string(), false, Align::Left)],
            ),
        ] {
            let mut want = want;
            want.push(("z".to_string(), false, Align::Left));
            assert_eq!(sketch(&with_images(&html)), want, "{html}");
            // In a cell too.
            let cell = format!("<table><tr><td>{html}</td></tr></table>");
            assert_eq!(sketch(&with_images(&cell)), want, "{cell}");
        }
        // An empty `p` is still an empty paragraph, after a block image's
        // paragraph too.
        let d = with_images(&format!(
            "<p></p><p>{img}</p><p></p><div>{img}</div><p></p>"
        ));
        assert_eq!(
            sketch(&d),
            [
                (String::new(), false, Align::Left),
                pic.clone(),
                (String::new(), false, Align::Left),
                pic.clone(),
                (String::new(), false, Align::Left),
            ]
        );
    }

    #[test]
    fn a_nested_list_keeps_its_parent_items_numbering() {
        let num = |d: &Document| -> Vec<(String, Option<i32>, i32)> {
            all_paras(d)
                .into_iter()
                .map(|p| (p.plain_text(), p.props.num_id, p.props.ilvl))
                .collect()
        };
        let img = "<img src=a.png style='display:block'>";
        // A nested item does not take the outer item's number: the picture
        // is the outer item's first paragraph.
        let d = with_images(&format!(
            "<ol><li><ul><li>b</li></ul>{img}c</li><li>d</li></ol>"
        ));
        assert_eq!(
            num(&d),
            [
                ("b".to_string(), Some(1), 1),
                (String::new(), Some(2), 0),
                ("c".to_string(), None, 0),
                ("d".to_string(), Some(2), 0),
            ]
        );
        // Nor does it clear the outer item's split: `d` goes on unnumbered.
        let d = with_images(&format!("<ul><li>a{img}b<ul><li>c</li></ul>d</li></ul>"));
        assert_eq!(
            num(&d),
            [
                ("a".to_string(), Some(1), 0),
                (String::new(), None, 0),
                ("b".to_string(), None, 0),
                ("c".to_string(), Some(1), 1),
                ("d".to_string(), None, 0),
            ]
        );
    }

    #[test]
    fn fixed_width_block_with_auto_margins_is_indented_evenly() {
        let indents = |html: &str| -> Vec<(String, i32, i32)> {
            all_paras(&doc(html))
                .into_iter()
                .map(|p| (p.plain_text(), p.props.indent, p.props.indent_right))
                .collect()
        };
        // 300 px = 4500 twips in a 9360 body: 2430 each side.
        assert_eq!(
            indents("<div style='width:300px;margin:0 auto'>x</div><p>y</p>"),
            [("x".into(), 2430, 2430), ("y".into(), 0, 0)]
        );
        // Content box: 20 px (300 twips) padding is outside the width.
        assert_eq!(
            indents(
                "<div style='width:300px;padding:0 20px;margin:0 auto;text-align:left'>x</div>"
            ),
            [("x".into(), 2430, 2430)]
        );
        // Centred in what an enclosing indent leaves (9360 - 1440 - 2880
        // = 5040 over two); the text's alignment is untouched.
        let d = doc("<div style='margin-left:1in'><p style='width:2in;margin:0 auto'>x</p></div>");
        let p = all_paras(&d)[0];
        assert_eq!((p.props.indent, p.props.indent_right), (1440 + 2520, 2520));
        assert_eq!(p.props.align, Align::Left);
        // Wider than the room: no extra indent, never negative.
        assert_eq!(
            indents("<div style='width:9in;margin:0 auto'>x</div>"),
            [("x".into(), 0, 0)]
        );
        // An invalid width (negative, no unit) leaves the valid one before
        // it; `auto` clears it.
        for w in ["-1px", "600", "red"] {
            let html = format!("<div style='width:300px;width:{w};margin:0 auto'>x</div>");
            assert_eq!(indents(&html), [("x".into(), 2430, 2430)], "{html}");
            let html = format!("<div style='width:{w};margin:0 auto'>x</div>");
            assert_eq!(indents(&html), [("x".into(), 0, 0)], "{html}");
        }
        assert_eq!(
            indents("<div style='width:300px;width:auto;margin:0 auto'>x</div>"),
            [("x".into(), 0, 0)]
        );
        // Not without a width, a percentage, both auto sides, or in a cell.
        for html in [
            "<div style='margin:0 auto'>x</div>",
            "<div style='width:50%;margin:0 auto'>x</div>",
            "<div style='width:300px;margin-left:auto'>x</div>",
            "<table><tr><td><div style='width:100px;margin:0 auto'>x</div></td></tr></table>",
        ] {
            assert_eq!(indents(html), [("x".into(), 0, 0)], "{html}");
        }
    }
}

/// Nested cards (#1927): a `div` with its own background in a table cell is
/// a one-cell table of its fill and padding.
#[cfg(test)]
mod card_tests {
    use super::*;
    use crate::model::{Block as DocBlock, Paragraph, Table};

    fn doc(html: &str) -> Document {
        import_html(html.as_bytes()).unwrap()
    }

    fn first_table(blocks: &[DocBlock]) -> &Table {
        blocks
            .iter()
            .find_map(|b| match b {
                DocBlock::Table(t) => Some(t),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no table: {blocks:?}"))
    }

    fn tables(blocks: &[DocBlock]) -> Vec<&Table> {
        blocks
            .iter()
            .filter_map(|b| match b {
                DocBlock::Table(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    fn cell(t: &Table, row: usize, col: usize) -> &[DocBlock] {
        &t.rows[row].cells[col].blocks
    }

    fn tcpr(t: &Table) -> &str {
        t.rows[0].cells[0].raw_tcpr.as_deref().unwrap_or("")
    }

    fn tblpr(t: &Table) -> &str {
        t.raw_tblpr.as_deref().unwrap_or("")
    }

    /// The outer table's first cell.
    fn outer_cell(d: &Document) -> &[DocBlock] {
        cell(first_table(&d.body), 0, 0)
    }

    /// The card (first table) in the outer table's first cell.
    fn card(d: &Document) -> &Table {
        first_table(outer_cell(d))
    }

    /// Blocks as text: a paragraph's text, a table as `[cell|cell/row…]`.
    fn sketch(blocks: &[DocBlock]) -> Vec<String> {
        blocks
            .iter()
            .filter_map(|b| match b {
                DocBlock::Paragraph(p) => Some(p.plain_text()),
                DocBlock::Table(t) => Some(format!(
                    "[{}]",
                    t.rows
                        .iter()
                        .map(|r| r
                            .cells
                            .iter()
                            .map(|c| sketch(&c.blocks).join(","))
                            .collect::<Vec<_>>()
                            .join("|"))
                        .collect::<Vec<_>>()
                        .join("/")
                )),
                _ => None,
            })
            .collect()
    }

    fn all_paras(blocks: &[DocBlock]) -> Vec<&Paragraph> {
        let mut out = Vec::new();
        for b in blocks {
            match b {
                DocBlock::Paragraph(p) => out.push(p),
                DocBlock::Table(t) => {
                    for r in &t.rows {
                        for c in &r.cells {
                            out.extend(all_paras(&c.blocks));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn para<'d>(d: &'d Document, text: &str) -> &'d Paragraph {
        all_paras(&d.body)
            .into_iter()
            .find(|p| p.plain_text() == text)
            .unwrap_or_else(|| panic!("no paragraph {text:?}: {:?}", d.body))
    }

    fn shaded(p: &Paragraph) -> bool {
        p.props.raw_props.iter().any(|r| r.starts_with("<w:shd"))
    }

    fn table_count(blocks: &[DocBlock]) -> usize {
        tables(blocks)
            .iter()
            .map(|t| {
                1 + t
                    .rows
                    .iter()
                    .flat_map(|r| r.cells.iter())
                    .map(|c| table_count(&c.blocks))
                    .sum::<usize>()
            })
            .sum()
    }

    #[test]
    fn a_background_div_in_a_cell_is_a_one_cell_card() {
        let d = doc(
            "<table><tr><td>before<div style='background:#eee;padding:8px 12px'>\
             <p>a</p><p>b</p></div>after</td></tr></table>",
        );
        assert_eq!(sketch(outer_cell(&d)), ["before", "[a,b]", "after"]);
        let c = card(&d);
        assert_eq!((c.rows.len(), c.rows[0].cells.len()), (1, 1));
        let pr = tcpr(c);
        assert!(pr.contains(r#"w:fill="EEEEEE""#), "{pr}");
        assert!(
            pr.contains(
                "<w:tcMar><w:top w:w=\"120\" w:type=\"dxa\"/><w:left w:w=\"180\" w:type=\"dxa\"/>\
                 <w:bottom w:w=\"120\" w:type=\"dxa\"/><w:right w:w=\"180\" w:type=\"dxa\"/></w:tcMar>"
            ),
            "{pr}"
        );
        assert!(pr.contains(r#"<w:vAlign w:val="top"/>"#), "{pr}");
        let tp = tblpr(c);
        assert!(tp.contains(r#"<w:tblW w:w="5000" w:type="pct"/>"#), "{tp}");
        assert!(tp.contains(
            "<w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"0\" w:type=\"dxa\"/>\
             <w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"0\" w:type=\"dxa\"/></w:tblCellMar>"
        ));
        assert!(!tp.contains("tblBorders") && !tp.contains("<w:jc"), "{tp}");
        for text in ["a", "b"] {
            let p = para(&d, text);
            assert!(!shaded(p), "{:?}", p.props);
            assert_eq!((p.props.indent, p.props.indent_right), (0, 0));
        }
        // A side not given is 0.
        let d = doc(
            "<table><tr><td><div style='background:#eee;padding-left:4px'>x</div>\
             </td></tr></table>",
        );
        assert!(tcpr(card(&d)).contains(
            "<w:tcMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"60\" w:type=\"dxa\"/>\
             <w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"0\" w:type=\"dxa\"/></w:tcMar>"
        ));
        // A `bgcolor`-less style-sheet background, through a class.
        let d = doc(
            "<style>.card{background-color:#F4F6F8;padding:16px}</style>\
             <table><tr><td><div class=card>x</div></td></tr></table>",
        );
        assert!(tcpr(card(&d)).contains(r#"w:fill="F4F6F8""#));
    }

    #[test]
    fn cards_nest_with_their_own_fills() {
        let d = doc("<table width=600><tr><td>\
             <div style='background:#aaaaaa;padding:16px'>one\
             <div style='background:#bbbbbb;padding:8px'>two\
             <div style='background:#cccccc'>three</div></div></div></td></tr></table>");
        assert_eq!(sketch(outer_cell(&d)), ["[one,[two,[three],],]", ""]);
        let one = card(&d);
        assert!(tcpr(one).contains(r#"w:fill="AAAAAA""#));
        let two = first_table(cell(one, 0, 0));
        assert!(tcpr(two).contains(r#"w:fill="BBBBBB""#));
        let three = first_table(cell(two, 0, 0));
        assert!(tcpr(three).contains(r#"w:fill="CCCCCC""#));
        // Each laid out in its card's width less its padding: 600 px is
        // 9000 twips, less 2 x 16 px, less 2 x 8 px.
        assert_eq!(one.grid.iter().sum::<u32>(), 9000);
        assert_eq!(two.grid.iter().sum::<u32>(), 9000 - 480);
        assert_eq!(three.grid.iter().sum::<u32>(), 9000 - 480 - 240);

        // A table in a card is in the card's cell; a fill-less cell of it
        // has no shading of its own (the card's shows through).
        let d = doc(
            "<table width=600><tr><td><div style='background:#dddddd;padding:16px'>\
             <table width=100%><tr><td>plain</td></tr></table></div></td></tr></table>",
        );
        let c = card(&d);
        let inner = first_table(cell(c, 0, 0));
        assert_eq!(inner.grid.iter().sum::<u32>(), 9000 - 480);
        assert!(!tcpr(inner).contains("<w:shd"), "{}", tcpr(inner));
        assert_eq!(sketch(outer_cell(&d)), ["[[plain],]", ""]);
    }

    #[test]
    fn a_card_ends_where_its_div_does() {
        let bg = "style='background:#eee'";
        let cases: [(&str, Vec<&str>); 6] = [
            (
                "<table><tr><td><div BG>in</div>after</td></tr></table>",
                vec!["[in]", "after"],
            ),
            (
                "<table><tr><td><div BG>in</td><td>next</td></tr></table>",
                vec!["[in]", ""],
            ),
            ("<table><tr><td>x<div BG>in", vec!["x", "[in]", ""]),
            (
                "<table><tr><td><ul><li>one<div BG>in<li>two</ul></td></tr></table>",
                vec!["one", "[in]", "two"],
            ),
            (
                "<table><tr><td><div BG><h2>x</td></tr></table>",
                vec!["[x]", ""],
            ),
            (
                "<table><tr><td><h2><div BG>x</h2>tail</td></tr></table>",
                vec!["[x]", "tail"],
            ),
        ];
        for (html, want) in cases {
            let html = html.replace("BG", bg);
            let d = doc(&html);
            assert_eq!(sketch(outer_cell(&d)), want, "{html}");
        }
        // Implied and explicit table-part ends: the outer table's shape.
        let shapes = [
            (
                "<table><tr><td><div BG>in<td>next</table>",
                vec!["[[in],|next]"],
            ),
            (
                "<table><tr><td><div BG>in</tr><tr><td>next</table>",
                vec!["[[in],/next]"],
            ),
            (
                "<table><tr><td><div BG>in<tr><td>next</table>",
                vec!["[[in],/next]"],
            ),
            (
                "<table><tbody><tr><td><div BG>in</tbody><tr><td>next</table>",
                vec!["[[in],/next]"],
            ),
            (
                "<table><tr><td><div BG>in</table>after",
                vec!["[[in],]", "after"],
            ),
            // The page ends inside a table in the card.
            (
                "<table><tr><td><div BG><table><tr><td>in",
                vec!["[[[in],],]"],
            ),
        ];
        for (html, want) in shapes {
            let html = html.replace("BG", bg);
            let d = doc(&html);
            assert_eq!(sketch(&d.body), want, "{html}");
        }
    }

    #[test]
    fn a_page_ending_deep_inside_a_card_keeps_the_builder_in_step() {
        // A card one level under the limit, then tables past it, all left
        // open: the ones past the limit are text, and nothing else ends.
        let open = "<table><tr><td>".repeat(MAX_TABLE_NESTING - 1);
        let deeper = "<table><tr><td>".repeat(3);
        let d = doc(&format!("{open}<div style='background:#eee'>{deeper}x"));
        assert_eq!(table_count(&d.body), MAX_TABLE_NESTING);
        assert_eq!(d.body.len(), 1, "{:?}", d.body);
        para(&d, "x");
        // With nothing in them, the card is dropped as empty: the tables past
        // the limit were never the builder's to end.
        let d = doc(&format!("{open}y<div style='background:#eee'>{deeper}"));
        assert_eq!(table_count(&d.body), MAX_TABLE_NESTING - 1);
        para(&d, "y");
    }

    #[test]
    fn every_prefix_of_the_card_digest_imports() {
        let page = include_bytes!("../../tests/fixtures/html/digest-cards.html");
        for end in 0..=page.len() {
            let _ = import_html(&page[..end]);
        }
    }

    #[test]
    fn a_div_straight_in_a_table_part_is_no_card() {
        // A `div` in a nested `table`, row group or row (which a browser
        // moves out of the table) has no cell to be a card in: its text
        // stays where it went before, and the outer table ends where it
        // did, `tail` in the body.
        for html in [
            "<table><tr><td><table><div style='background:#eee'><tr><td>y</table>after\
             </td></tr></table>tail",
            "<table><tr><td><table><div style='background:#eee'><tbody><tr><td>y</td></tr>\
             </table>after</td></tr></table>tail",
            "<table><tr><td><table><tbody><div style='background:#eee'><tr><td>y</td></tr>\
             </tbody></table>after</td></tr></table>tail",
        ] {
            let d = doc(html);
            assert_eq!(table_count(&d.body), 2, "{html}: {:?}", d.body);
            assert_eq!(d.body.len(), 2, "{html}: {:?}", d.body);
            assert_eq!(sketch(&d.body[1..]), ["tail"], "{html}");
            let outer = outer_cell(&d);
            assert_eq!(sketch(outer).last().map(String::as_str), Some("after"));
            assert_eq!(sketch(cell(first_table(outer), 0, 0)), ["y"], "{html}");
        }
        // In a nested row: `x` keeps its paragraph shading beside `y`, in
        // the nested table's cell.
        let d = doc(
            "<table><tr><td><table><tr><div style='background:#eee'>x<td>y</td></tr>\
             </table></td></tr></table>",
        );
        assert_eq!(table_count(&d.body), 2, "{:?}", d.body);
        let nested = first_table(outer_cell(&d));
        assert_eq!(sketch(cell(nested, 0, 0)), ["x", "y"]);
        assert!(shaded(para(&d, "x")));
        assert!(!shaded(para(&d, "y")));
    }

    #[test]
    fn a_restyle_keeps_a_card_a_card() {
        // `</b>` over the card takes the `b` out alone and restyles the card:
        // it stays one even when the rule that made it no longer matches.
        let d = doc("<style>b div{background:#eee}</style>\
             <table><tr><td><b><div>x</b>y</div>after</td></tr></table>");
        assert_eq!(sketch(outer_cell(&d)), ["[xy]", "after"]);
        // And a div that is no card stays none: its `none` still clears.
        let d = doc("<style>div{background:#eee}</style>\
             <table><tr><td><b><div style='background:none'>x</b>y</div></td></tr></table>");
        assert_eq!(table_count(&d.body), 1);
        assert!(!shaded(para(&d, "xy")));
    }

    #[test]
    fn a_card_takes_its_width_and_place() {
        let jc_w = |html: &str| {
            let d = doc(html);
            let tp = tblpr(card(&d)).to_string();
            let jc = tp
                .split("<w:jc w:val=\"")
                .nth(1)
                .map(|j| j[..j.find('"').unwrap()].to_string());
            let w = tp
                .split("<w:tblW ")
                .nth(1)
                .map(|w| w[..w.find("/>").unwrap()].to_string())
                .unwrap();
            (jc, w)
        };
        let center = Some("center".to_string());
        assert_eq!(
            jc_w(
                "<table width=600><tr><td><div style='width:300px;margin:0 auto;\
                  background:#eee'>x</div></td></tr></table>"
            ),
            (center.clone(), r#"w:w="4500" w:type="dxa""#.into())
        );
        // An `em` width takes the card's final font size: 20 x 20 px.
        assert_eq!(
            jc_w(
                "<table><tr><td><div style='width:20em;font-size:20px;background:#eee'>x\
                  </div></td></tr></table>"
            )
            .1,
            r#"w:w="6000" w:type="dxa""#
        );
        assert_eq!(
            jc_w("<table><tr><td><div style='width:50%;background:#eee'>x</div></td></tr></table>"),
            (None, r#"w:w="2500" w:type="pct""#.into())
        );
        assert_eq!(
            jc_w(
                "<table><tr><td align=center><div style='width:300px;background:#eee'>x</div>\
                  </td></tr></table>"
            )
            .0,
            center
        );
        // `align=center` on the card centres what is in it, not the card
        // (`-webkit-center`, #1922): a browser leaves a 300 px div at the
        // left.
        assert_eq!(
            jc_w(
                "<table><tr><td><div align=center style='width:300px;background:#eee'>x</div>\
                  </td></tr></table>"
            )
            .0,
            None
        );
        assert_eq!(
            jc_w(
                "<table><tr><td><div style='width:300px;margin-left:auto;background:#eee'>x\
                  </div></td></tr></table>"
            )
            .0,
            Some("right".into())
        );
    }

    #[test]
    fn stacked_cards_are_kept_apart() {
        for html in [
            "<table><tr><td><div BG>a</div><div BG>b</div></td></tr></table>",
            "<table><tr><td><div BG>a</div>\n  <div BG>b</div>\n</td></tr></table>",
            "<table><tr><td><div BG>a</div><table><tr><td>b</td></tr></table></td></tr></table>",
            "<table><tr><td><table><tr><td>a</td></tr></table><div BG>b</div></td></tr></table>",
            // Two plain nested tables back to back are kept apart too.
            "<table><tr><td><table><tr><td>a</td></tr></table>\
             <table><tr><td>b</td></tr></table></td></tr></table>",
        ] {
            let html = html.replace("BG", "style='background:#eee'");
            let d = doc(&html);
            assert_eq!(sketch(outer_cell(&d)), ["[a]", "", "[b]", ""], "{html}");
        }
    }

    #[test]
    fn an_empty_background_div_is_no_card() {
        for div in [
            "<div style='background:#eee'></div>",
            "<div style='background:#eee;height:8px'> </div>",
            "<div style='height:1px;line-height:1px;font-size:1px;background:#e0e0e0'>&nbsp;</div>",
            "<div style='background:#eee'><p>&nbsp;</p></div>",
            "<div style='background:#eee'><div style='background:#ddd'></div></div>",
        ] {
            let d = doc(&format!("<table><tr><td>x{div}y</td></tr></table>"));
            assert_eq!(sketch(outer_cell(&d)), ["x", "y"], "{div}");
        }
        // Between two cards it leaves them apart by one paragraph only.
        let d = doc("<table><tr><td><div style='background:#eee'>a</div>\
             <div style='background:#ccc'></div><div style='background:#eee'>b</div>\
             </td></tr></table>");
        assert_eq!(sketch(outer_cell(&d)), ["[a]", "", "[b]", ""]);
    }

    #[test]
    fn divs_that_are_not_cards() {
        // In a cell, no table: padding alone, `none`, `none` over a sheet's
        // background, hidden, under a hidden ancestor, inline-block, a `p`.
        let in_cell = [
            ("", "<div style='padding:8px'>x</div>", None),
            ("", "<div style='background:none;padding:8px'>x</div>", None),
            ("", "<div style='background:transparent'>x</div>", None),
            (
                "<style>div{background:#eee}</style>",
                "<div style='background:none'>x</div>",
                None,
            ),
            // Hidden: paragraph shading on vanished text, as before.
            (
                "",
                "<div style='display:none;background:#eee'>x</div>",
                Some("EEEEEE"),
            ),
            (
                "",
                "<div style='mso-hide:all;background:#eee'>x</div>",
                Some("EEEEEE"),
            ),
            (
                "",
                "<span style='display:none'><div style='background:#eee'>x</div></span>",
                Some("EEEEEE"),
            ),
            (
                "",
                "<div style='display:inline-block;background:#eee'>x</div>",
                Some("EEEEEE"),
            ),
            ("", "<p style='background:#eee'>x</p>", Some("EEEEEE")),
            (
                "",
                "<blockquote style='background:#eee'>x</blockquote>",
                Some("EEEEEE"),
            ),
        ];
        for (head, body, shade) in in_cell {
            let d = doc(&format!("{head}<table><tr><td>{body}</td></tr></table>"));
            assert_eq!(table_count(&d.body), 1, "{body}");
            let p = para(&d, "x");
            assert_eq!(
                shaded(p),
                shade.is_some(),
                "{body}: {:?}",
                p.props.raw_props
            );
            if let Some(fill) = shade {
                assert!(p.props.raw_props.iter().any(|r| r.contains(fill)));
            }
        }
        // Padding alone is still indent, as before.
        let d = doc("<table><tr><td><div style='padding:8px'>x</div></td></tr></table>");
        assert_eq!(para(&d, "x").props.indent, 120);
        // Outside any table: paragraph shading, as before.
        let d = doc("<div style='background:#eee;padding:8px'>x</div>");
        assert_eq!(table_count(&d.body), 0);
        assert!(shaded(para(&d, "x")));
        // At the nesting limit: paragraph shading in the innermost cell.
        let open = "<table><tr><td>".repeat(MAX_TABLE_NESTING);
        let d = doc(&format!("{open}<div style='background:#eee'>x</div>"));
        assert_eq!(table_count(&d.body), MAX_TABLE_NESTING);
        assert!(shaded(para(&d, "x")));
        // One level under it, a card still fits.
        let open = "<table><tr><td>".repeat(MAX_TABLE_NESTING - 1);
        let d = doc(&format!("{open}<div style='background:#eee'>x</div>"));
        assert_eq!(table_count(&d.body), MAX_TABLE_NESTING);
        assert!(!shaded(para(&d, "x")));
        // Past MAX_DEPTH open elements: no card, and nothing breaks.
        let spans = "<span>".repeat(MAX_DEPTH);
        let d = doc(&format!(
            "<table><tr><td>{spans}<div style='background:#eee'>x</div>y</td></tr></table>"
        ));
        assert_eq!(table_count(&d.body), 1);
    }
}
