//! Opening what Word opens besides `.docx` (#633): RTF, a Web Page, a PDF,
//! and the text of a damaged file (Word's Recover Text from Any File).
//!
//! Each importer turns bytes into a plain [`Document`]: paragraphs of runs
//! with bold / italic / underline, heading styles (`Heading1`..`Heading6`),
//! list membership (numbering ids 1 = bullets and 2 = decimal, the two that
//! [`crate::package::new_markdown_package`] defines) and simple tables. The
//! result has no package of its own; it is saved through a fresh Markdown
//! package, which defines every style and list it uses. Fields, notes,
//! comments and section layout are not converted (an HTML import gets one
//! fixed page, Letter with Outlook-like 15 px margins, #1928); images only
//! by [`html::import_html_with`] (#1880), whose [`html::HtmlImport`] carries
//! their media into the package it builds.
//!
//! Format is decided by content first ([`sniff`]), so an RTF or HTML file
//! named `.doc` or `.docx` still opens as what it is.
//!
//! Every import runs against one `Budget` (#633 r2): the input it scans,
//! the bytes it decodes and the document it builds are all charged, so a
//! hostile file fails with a load error ([`TOO_BIG`]) instead of aborting the
//! process or hanging the UI thread. A cost that is not charged directly is
//! O(1) per charged unit, and says so where that is not obvious.
//!
//! # Every input-driven loop and allocation, and what bounds it
//!
//! "work" is `Budget::work` (time), "built" is `Budget::alloc` (memory
//! kept), "decoded" is `Budget::take_bytes`. "Input" is the bytes handed
//! to the importer, which the caller already holds.
//!
//! | Where | Loop or allocation | Bound / charged to |
//! |---|---|---|
//! | `sniff` | first 1 KB, first 4 KB | constant |
//! | `Builder::text` | a run's text appended | work (len/16) + built (len); a new run built (`size_of::<Inline>` + its props' strings) |
//! | `Builder::link_text` (#1879) | text in a link | as `Builder::text`; a new hyperlink built (`size_of::<Inline>` + a run + its props' strings + the target), an extra run in it built (`size_of::<Run>` + its props' strings) |
//! | `Builder::tab`, `line_break`, `page_break` | one inline | work + built (with its props' strings) |
//! | `Builder::end_para` / `end_cell` / `end_row` | a paragraph / cell / row | work + built (`size_of` each) |
//! | `Builder::end_cell`, `end_table` | cells past `MAX_COLUMNS` | join the last cell (no new cell) |
//! | `Builder::end_table` | padding a short row | ≤ 62 cells a row, each work + built |
//! | `Builder` tables | open tables | HTML: ≤ `MAX_TABLE_NESTING` (8), deeper ones are their cell's text; RTF: one |
//! | `html_table::layout` | the slot grid, cells emitted | rows × ≤ `MAX_COLUMNS` slots; each cell emitted (placed, continuation or pad) work + built; a cell with no free slot joins a neighbour |
//! | `html_table::layout` | nested tables | each laid out once, inside its cell, depth ≤ `MAX_TABLE_NESTING` |
//! | `Builder::take_leading_marker` | the paragraph's text, the inlines consumed | O(paragraph), once per paragraph; one pass rebuilding it |
//! | `Builder::bookmark` (#1912) | a point bookmark waiting, then two inlines | ≤ `MAX_BOOKMARKS` (10 000) a document, each built (`size_of::<Inline>` + 3 × name + 96); names ≤ 40 characters; a name made once (a set) |
//! | `Builder::flush_marks` (#1912 r2) | the waiting bookmarks written | only before content that keeps the paragraph (a white-space-only one, which HTML drops, takes none); work (marks) |
//! | `Builder::clear_para` | its bookmarks put back (only a kept paragraph has any: a typed list marker taken off can leave it empty) | work (marks), no parse |
//! | `Builder::para_text` | the paragraph in progress | O(paragraph), once per paragraph end |
//! | run props cloned per inline | `RunProps` | flags, and strings of ≤ 64 bytes (font, colour, highlight) plus ≤ 2 kept elements (HTML's shading, bold off): O(1), charged as built |
//! | `inflate_within` | inflate output | decoded, capped inside each block at the room; room 0 fails |
//! | `rtf::import_rtf` | the input, once | work (input/16), up front |
//! | rtf groups | the group stack | ≤ `MAX_DEPTH`; deeper ones counted, not kept |
//! | rtf control word | its letters copied | O(word), within the one pass |
//! | rtf `\listtext`, stylesheet names | text kept | ≤ 64 bytes each (`MAX_NAME`) |
//! | rtf font table, heading styles | map entries | built per new entry |
//! | `html::import_html` | decoding, the `<style>` scan, tokenizing | work (3 × input/16), up front |
//! | html `prescan` | the `<style>` texts joined; each `<a>` tag's attributes read (#1912) | ≤ input; forward only (comments and raw-text elements skipped by one search each, an `<a>` tag passed over whole; an `<a` that never closes read to the end once, and no `<a` after it, r1); anchor names ≤ `MAX_BOOKMARKS` |
//! | html `open_void` (#1912 r1) | a void or raw-text element's attributes, for a linked `id` | only when the page has an anchor link; scanned (attrs/16) |
//! | `css::StyleSheet::parse` | the CSS, once; rules, declaration blocks | work (css/16); each rule and block built; ≤ `MAX_RULES` (10 000) rules, more ignored; brace nesting counted, never recursed |
//! | css selector | compounds | ≤ `MAX_COMPOUNDS` (32), longer dropped; names ≤ 64 bytes |
//! | `css::StyleSheet::matching` | rules examined per element opened | work per rule examined (only those indexed by the element's id, classes, tag, or universal) |
//! | `css::Selector::matches` | reach sets, one per compound | work (depth/4 + 1) a compound: O(compounds × depth), no backtracking |
//! | html element look | parent's look cloned, the cascade | per open: work (1 + key/16) + the matching; inline `style` parsed once |
//! | html implicit close | the element stack, searched once | work (depth/16 + 1) per block open |
//! | html omitted table end (`close_open`) | the element stack, searched once | work (depth/16 + 1), ≤ 4 searches per `td th tr thead tbody tfoot` open |
//! | html `restyle` | looks above a closed element computed again | per element: its attributes scanned again (work attrs/16 + 1) + the matching; ≤ depth elements a close |
//! | html formatting reopened after a truncation | elements kept, looks restyled | ≤ depth, at most three alike counting those open below (work below/16 + 1 each); as `restyle` |
//! | html `out_of_scope` | the element stack, searched once | per close tag, ≤ 256 |
//! | html `decode` | the page as UTF-8 | ≤ 3 × input (one pass) |
//! | html element stacks | elems / blocks / lists | ≤ `MAX_DEPTH` each; a close searches ≤ 256; an element keeps ≤ 32 classes of ≤ 64 bytes, an id ≤ 64, its look's strings ≤ 64 each, and its link (#1879) shared (`Rc`), ≤ `MAX_HREF` (4096) bytes, so a look's clone copies none of it |
//! | html tag name | copied, matched | ≤ 32 bytes (`MAX_TAG`) |
//! | html attributes | one scan a tag | the ones `Attrs` holds copied, first of a name only |
//! | html `link_of` (#1879) | an `a`'s `href`, once per look computed | O(href), refused unread past 2 × `MAX_HREF`; within the attribute scan's cost |
//! | html `<img>` (#1880) | its look; the picture run | as an element's look, without being pushed; the run's XML built (alt ≤ `MAX_ALT` 1024 chars) |
//! | html `<img>` in a link (#1999) | its URL, once per distinct URL | ≤ `MAX_PICTURE_LINKS` (1000) URLs, a later one's picture unlinked; each built (twice the URL: the list and its index); their relationships added in one pass |
//! | `html_image::decode_data_uri` | a `data:` src: counted, then percent- and base64-decoded | work (src/16), up front; refused unless its counted size fits the image's limit; the bytes decoded |
//! | `ImageResolver` answer | the bytes another src resolves to | the resolver is told the limit (room, `MAX_IMAGE_BYTES` 64 MiB, what `MAX_MEDIA_BYTES` 128 MiB leaves); a longer answer dropped; decoded |
//! | resolver calls, src cache | one call per distinct src | ≤ `MAX_IMAGES` (1000) calls; each cached src built (its length) |
//! | html `<base href>` (#1898) | found by the `<style>` scan; each src resolved against it | ≤ `MAX_BASE` (4096) bytes, else none; a src's join O(base + src), within its resolver call |
//! | `<img srcset>`, `<picture>` sources (#1898) | candidates parsed, each tried as a src | a source's list parsed once when kept (scanned, each candidate built), ≤ `MAX_SOURCES` (16) sources a picture; an `<img>`'s own list only once its `src` gave nothing (scanned); every parse stops at `MAX_CANDIDATES` (16), and so do an image's tries, each a resolver call within `MAX_IMAGES` |
//! | `mhtml::parse` (#1898) | the header blocks, the boundary scan, the parts | headers ≤ `MAX_HEADER` (16 KiB) each; the body scanned once (charged as scanned); ≤ `MAX_PARTS` (4096) parts, each built with its keys (its location as written and percent-decoded, its Content-ID: decoded once, charged as built); the root part decoded |
//! | `Mhtml::part_for` (#1898) | the part a src names, decoded | map lookups and one percent-decode of the src per resolver call; refused unread unless its decoded bound fits the image's limit; decoded as a resolver answer |
//! | `Images::add` | a medium equal to one kept | work (len/16) per kept medium of the same length; kept media ≤ `MAX_MEDIA_BYTES` in all, so a converting child holds ≤ 3 × 128 MiB of them (import, package, the copy a save clones) |
//! | html `entities` | each `&` and the name after it | O(name), within the one pass |
//! | html list marker | text kept | ≤ 32 bytes |
//! | `recover::document_xml` | the header scan | work (input/16), up front; forward only |
//! | recover header name | copied, lowercased | only names ≤ 260 bytes under `word/` (`MAX_PART_NAME`) |
//! | recover part decoded | stored copy / inflate | decoded; once a fallback is held, others are not decoded |
//! | recover `paragraphs_of` | the part's XML, once | covered by decoded bytes; paragraphs via `Builder` |
//! | recover `unescape` | each `&` | looks at ≤ 11 bytes |
//! | `recover_any_text` | 8-bit and UTF-16 runs | work (3 × input/16); runs ≤ 2 × input; paragraphs via `Builder` |
//! | pdf `Lexer` | every byte advanced over, strings, numbers, keywords | work (bytes/16) on every pass, re-lexing included |
//! | pdf kept tokens (`File::scan`, object streams, CMaps) | an object per token | built (`size_of::<Obj>` + its bytes) |
//! | pdf name | bytes kept | ≤ 127 (`MAX_NAME`) |
//! | pdf stream body | raw bytes copied once into its object | ≤ input |
//! | pdf object stream | header pairs, entries | ≤ 100 000 pairs; each entry parsed only within its own span, repeated offsets once |
//! | pdf object streams listed | dict and raw copied for decoding | ≤ input |
//! | pdf `decode_stream` | raw data, stream dictionary | work (raw/16 + entries) |
//! | pdf filters / decode parms | read | ≤ `MAX_FILTERS`, borrowed, parms ≤ filters |
//! | pdf filter stages | inflate, predictor, ASCIIHex, ASCII85 output | decoded, each stage; ASCII85 capped as made |
//! | pdf predictor row | sized from the file | checked multiply; a row longer than the data is refused |
//! | pdf `walk_pages` | every call | work; each node and each `/Kids` array once, by identity; ≤ 100 000 pages, depth ≤ 64 |
//! | pdf page refs | per page | built; borrow the page dict and resources |
//! | pdf `/Contents` parts joined | bytes copied | decoded |
//! | pdf stream decode cache | by object | each referenced stream decoded once |
//! | pdf `ResIndex::new` | resources dict, `/Font`, `/XObject` | work (entries), once per content run |
//! | pdf content operators | `ops` operand stack | ≤ 10 000 objects (then cleared); tokens charged by the lexer |
//! | pdf `q` stack | graphics states | ≤ 64; a state holds an `Rc<Font>` |
//! | pdf `Do` | form dictionary read, form content re-lexed | work (dict entries) + lexer, every draw; depth ≤ 8, no form inside itself |
//! | pdf inline image | data skipped | work (bytes/16); `ID` found once |
//! | pdf `Font::load` | font dict, 256-entry table, descendant, encoding | work (entries + 256); once per font (object or inline address) |
//! | pdf `/Widths`, `/W` | entries read, widths kept | work per entry read; ≤ `MAX_CMAP_ENTRIES` widths, saturating codes |
//! | pdf `/Differences` | entries | work per entry |
//! | pdf ToUnicode `parse_cmap` | tokens, mappings | lexer + built per token; ≤ 1M mappings, each work; destination ≤ 32 UTF-16 units |
//! | pdf `show` | glyphs | work + built each (`size_of::<Glyph>` + text ≤ 32 chars); ≤ 2M glyphs |
//! | pdf `make_lines` | per glyph, trailing trim | O(glyphs) |
//! | pdf `layout` | per line, per piece, tabs split once | O(glyphs); paragraphs via `Builder` |

pub(crate) mod css;
pub mod html;
pub mod html_image;
pub(crate) mod html_table;
pub(crate) mod mhtml;
pub mod pdf;
pub mod recover;
pub mod rtf;

pub use html::{HtmlImport, import_html, import_html_with, import_html_with_converter};
pub use html_image::{
    ConvertKind, ConvertedImage, ImageConverter, ImageResolver, ImportedLink, ImportedMedia,
};
pub use pdf::import_pdf;
pub use recover::{paragraph_count, recover_any_text, recover_docx_text};
pub use rtf::import_rtf;

use crate::model::{
    Block, BreakKind, Cell, Document, Hyperlink, Inline, ParProps, Paragraph, Row, Run, RunProps,
    Table,
};
use std::cell::Cell as Counter;

/// What any import says when its budget runs out (the file would cost more
/// to convert than an import may spend).
pub const TOO_BIG: &str = "the file is too large or too complex to convert";

/// The most stream data one import may decode: every filter stage's output
/// (inflate, predictor, ASCIIHex, ASCII85), stored ZIP parts, and content
/// streams joined.
pub(crate) const MAX_DECODED: usize = 256 << 20;
/// The most work one import may do, in units: a token, a glyph, a run, a
/// paragraph, a cell, a map entry, or 16 bytes of input scanned. A unit is
/// O(1) time and holds no memory of its own (memory is [`MAX_BUILT`]'s), so
/// this bounds time: 32M units at well under a microsecond each.
pub(crate) const MAX_WORK: usize = 32_000_000;
/// The most memory an import may build, in bytes, estimated as it is built:
/// `size_of` each kept element (an object, a token kept for a CMap, a glyph,
/// an inline, a paragraph, a cell, a row, a map entry) plus its heap text.
///
/// Peak memory of an import is therefore bounded: the decoded data
/// ([`MAX_DECODED`], 256 MiB) plus what is built (this, 192 MiB, at most
/// doubled by a vector's growth, so 384 MiB) plus the input itself; about
/// 640 MiB beyond the input, well under 1 GiB. The suite also runs every
/// conversion in a child process with a memory limit, so even a bound that
/// is wrong costs only that child.
pub(crate) const MAX_BUILT: usize = 192 << 20;
/// Word's widest table; cells past it in a row join its last cell.
pub(crate) const MAX_COLUMNS: usize = 63;
/// The deepest nesting an importer keeps (HTML elements, RTF groups);
/// opens past it are ignored.
pub(crate) const MAX_DEPTH: usize = 256;

/// What an import may still spend: decoded bytes, units of work and bytes
/// built. Once any runs out it stays out, and the import fails with
/// [`TOO_BIG`].
pub(crate) struct Budget {
    bytes: Counter<usize>,
    work: Counter<usize>,
    built: Counter<usize>,
    out: Counter<bool>,
}

impl Budget {
    pub(crate) fn new(bytes: usize, work: usize) -> Self {
        Self::with_built(bytes, work, MAX_BUILT)
    }

    pub(crate) fn with_built(bytes: usize, work: usize, built: usize) -> Self {
        Budget {
            bytes: Counter::new(bytes),
            work: Counter::new(work),
            built: Counter::new(built),
            out: Counter::new(false),
        }
    }

    /// The budget every import gets.
    pub(crate) fn standard() -> Self {
        Self::new(MAX_DECODED, MAX_WORK)
    }

    /// Decoded bytes that may still be made.
    pub(crate) fn room(&self) -> usize {
        if self.out.get() { 0 } else { self.bytes.get() }
    }

    /// Spend `n` decoded bytes; `false` (and out for good) past the budget.
    pub(crate) fn take_bytes(&self, n: usize) -> bool {
        match self.bytes.get().checked_sub(n) {
            Some(left) if !self.out.get() => {
                self.bytes.set(left);
                true
            }
            _ => self.fail(),
        }
    }

    /// Spend `n` units of work; `false` (and out for good) past the budget.
    pub(crate) fn work(&self, n: usize) -> bool {
        match self.work.get().checked_sub(n) {
            Some(left) if !self.out.get() => {
                self.work.set(left);
                true
            }
            _ => self.fail(),
        }
    }

    /// Spend one unit of work.
    pub(crate) fn op(&self) -> bool {
        self.work(1)
    }

    /// Spend `n` bytes of memory built (kept until the import ends).
    pub(crate) fn alloc(&self, n: usize) -> bool {
        match self.built.get().checked_sub(n) {
            Some(left) if !self.out.get() => {
                self.built.set(left);
                true
            }
            _ => self.fail(),
        }
    }

    /// One kept element of type `T` with `heap` bytes of its own: a unit
    /// of work and its memory.
    pub(crate) fn keep<T>(&self, heap: usize) -> bool {
        self.op() && self.alloc(std::mem::size_of::<T>() + heap)
    }

    /// Spend the work of scanning `n` bytes of input once.
    pub(crate) fn scanned(&self, n: usize) -> bool {
        self.work(n / 16 + 1)
    }

    /// Run out now.
    pub(crate) fn fail(&self) -> bool {
        self.out.set(true);
        false
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.out.get()
    }
}

/// Inflate a raw DEFLATE `body`, spending decoded bytes from `budget` and
/// capped inside each block at what is left. Past it, or with no room left
/// at all (where `inflate_partial` would read a cap of 0 as none): `Err`
/// with what was decoded, never more than the room, and the budget out.
/// The one inflate both the PDF importer and recovery use.
pub(crate) fn inflate_within(body: &[u8], budget: &Budget) -> Result<Vec<u8>, Vec<u8>> {
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

/// What a file holds, by its first bytes (and its extension when they do
/// not say).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A ZIP container: a Word package, or what is left of one.
    Docx,
    Rtf,
    Html,
    Pdf,
    /// An OLE2 compound file: a Word 97-2003 `.doc`, or an encrypted
    /// (password-protected) `.docx`. Neither is read.
    Cfb,
    /// Nothing recognised.
    Unknown,
}

/// Decide a file's format from its content, then from `ext` (the extension,
/// any case, without the dot).
pub fn sniff(bytes: &[u8], ext: &str) -> Format {
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Format::Docx;
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Format::Cfb;
    }
    // The signatures that must start the file come first, so a page or an
    // RTF that only mentions `%PDF-` near its top is still what it is.
    let text = trim_text_start(bytes);
    if text.starts_with(b"{\\rtf") {
        return Format::Rtf;
    }
    // An MHTML Web Page (#1898): read as HTML, its root part the page.
    if looks_like_html(text) || mhtml::looks_like(bytes) {
        return Format::Html;
    }
    // A PDF reader accepts junk before the header, within the first 1 KB.
    if find(&bytes[..bytes.len().min(1024)], b"%PDF-").is_some() {
        return Format::Pdf;
    }
    match ext.to_ascii_lowercase().as_str() {
        "rtf" => Format::Rtf,
        "htm" | "html" | "xhtml" | "mht" | "mhtml" => Format::Html,
        "pdf" => Format::Pdf,
        "docx" | "docm" | "dotx" | "dotm" => Format::Docx,
        _ => Format::Unknown,
    }
}

/// `bytes` without a UTF-8 byte-order mark and leading whitespace.
fn trim_text_start(bytes: &[u8]) -> &[u8] {
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    &b[start..]
}

fn looks_like_html(text: &[u8]) -> bool {
    if !text.starts_with(b"<") {
        return false;
    }
    let head: Vec<u8> = text[..text.len().min(4096)].to_ascii_lowercase();
    [b"<html".as_slice(), b"<!doctype html", b"<head", b"<body"]
        .iter()
        .any(|m| find(&head, m).is_some())
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The paragraph properties of a heading at `level` (1-based; deeper than 6
/// is styled as 6, the deepest the Markdown package defines).
pub(crate) fn heading_props(level: u8) -> ParProps {
    let level = level.clamp(1, 6);
    ParProps {
        heading_level: Some(level),
        style_id: Some(format!("Heading{level}")),
        ..ParProps::default()
    }
}

/// Whether a list marker as Word writes it (`1.`, `a)`, `iv.`, and a
/// multilevel list's `1.2.`) is a numbered one; anything else (`·`, `o`,
/// `§`, `•`, U+F0B7) is a bullet.
pub(crate) fn marker_is_numbered(marker: &str) -> bool {
    let m = marker.trim();
    let Some(body) = m.strip_suffix('.').or_else(|| m.strip_suffix(')')) else {
        return false;
    };
    let body = body.strip_prefix('(').unwrap_or(body);
    body.split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric()))
}

/// Whether a paragraph of `content` shows anything (#1883 r4, r5): as
/// Word and the suite collapse one, it is hidden only when its mark is
/// hidden and all its content too (`vanish`); a picture or other object
/// always shows.
pub(crate) fn shows(content: &[Inline], mark_vanish: bool) -> bool {
    !mark_vanish
        || content.iter().any(|i| match i {
            Inline::Run(r) => !r.props.vanish,
            Inline::Hyperlink(h) => h.runs.iter().any(|r| !r.props.vanish),
            Inline::Break(_, p) | Inline::Tab(p) => !p.vanish,
            i => !is_mark(i),
        })
}

/// Whether a paragraph's mark (its `w:rPr` in `raw_props`, as the HTML
/// import writes it) is hidden.
pub(crate) fn mark_vanish(props: &ParProps) -> bool {
    props
        .raw_props
        .iter()
        .any(|r| r.starts_with("<w:rPr") && r.contains("<w:vanish/>"))
}

/// An inline that is neither text nor a break nor a link: an object,
/// content of its own. A link (#1879) counts by its text, and a bookmark
/// (#1912) is no content.
fn is_object(inline: &Inline) -> bool {
    !matches!(
        inline,
        Inline::Run(_) | Inline::Tab(_) | Inline::Break(..) | Inline::Hyperlink(_)
    ) && !is_mark(inline)
}

/// Whether `inline` is half of a bookmark an importer made
/// ([`Builder::bookmark`], #1912): a zero-width mark, never content.
fn is_mark(inline: &Inline) -> bool {
    matches!(inline, Inline::Raw(r) if r.starts_with("<w:bookmarkStart ") || r.starts_with("<w:bookmarkEnd "))
}

/// The most bookmarks one import makes (#1912); later names make none.
pub(crate) const MAX_BOOKMARKS: usize = 10_000;

/// Where an imported link (#1879) goes, and the `title` and `target` it
/// keeps (#1912) as Word's `w:tooltip` and `w:tgtFrame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) to: LinkTo,
    pub(crate) tooltip: Option<String>,
    pub(crate) frame: Option<String>,
    /// Its [`Link::opener_of`], made once (#1912 r2).
    opener: Option<String>,
}

/// An external URL, or an anchor (a bookmark name) in the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkTo {
    Url(String),
    Anchor(String),
}

impl Link {
    pub(crate) fn new(to: LinkTo, tooltip: Option<String>, frame: Option<String>) -> Self {
        let mut link = Link {
            to,
            tooltip,
            frame,
            opener: None,
        };
        link.opener = link.opener_of();
        link
    }

    fn heap(&self) -> usize {
        let to = match &self.to {
            LinkTo::Url(s) | LinkTo::Anchor(s) => s.len(),
        };
        let kept = self.tooltip.as_ref().map_or(0, String::len)
            + self.frame.as_ref().map_or(0, String::len);
        // The opener holds them again, escaped (at most 6 bytes a char).
        to + 7 * kept + if kept > 0 { 64 + 6 * to } else { 0 }
    }

    /// The `w:hyperlink` start tag a link with a title or target keeps as
    /// its `raw` (#1912), self-closing: save writes it, then the runs, and
    /// a save's minted `r:id` goes into it (`with_opener_rel_id`). An
    /// anchor link's `w:anchor` is in it too, as save writes no other.
    /// `None` for a link with neither: it is written from its fields.
    fn opener_of(&self) -> Option<String> {
        if self.tooltip.is_none() && self.frame.is_none() {
            return None;
        }
        let mut out = String::from("<w:hyperlink");
        let mut attr = |name: &str, value: &str| {
            out.push(' ');
            out.push_str(name);
            out.push_str("=\"");
            crate::serialize::esc_attr(value, &mut out);
            out.push('"');
        };
        if let LinkTo::Anchor(a) = &self.to {
            attr("w:anchor", a);
        }
        if let Some(t) = &self.tooltip {
            attr("w:tooltip", t);
        }
        if let Some(f) = &self.frame {
            attr("w:tgtFrame", f);
        }
        out.push_str("/>");
        Some(out)
    }

    /// Whether `h` is a link the importer made to this place, with this
    /// title and target, so text written in this link joins it.
    fn made(&self, h: &Hyperlink) -> bool {
        h.content.is_empty()
            && h.raw.as_deref() == self.opener.as_deref()
            && match &self.to {
                LinkTo::Url(u) => h.target.as_deref() == Some(u.as_str()) && h.anchor.is_none(),
                LinkTo::Anchor(a) => h.anchor.as_deref() == Some(a.as_str()) && h.target.is_none(),
            }
    }
}

/// The character style imported link text takes (#1912).
pub(crate) const HYPERLINK_STYLE: &str = "Hyperlink";

/// Whether link text in `props` has the look Word's `Hyperlink` style gives
/// (#1912): underlined, in a colour, no character style of its own.
fn link_look(props: &RunProps) -> bool {
    props.underline
        && props.style_id.is_none()
        && props
            .color
            .as_deref()
            .is_some_and(|c| !c.eq_ignore_ascii_case("auto"))
}

/// The two inlines of the point bookmark `w:id` `id` named `name` (#1912).
fn mark_inlines(id: usize, name: &str) -> [Inline; 2] {
    let mut start = format!("<w:bookmarkStart w:id=\"{id}\" w:name=\"");
    crate::serialize::esc_attr(name, &mut start);
    start.push_str("\"/>");
    [
        Inline::Raw(start),
        Inline::Raw(format!("<w:bookmarkEnd w:id=\"{id}\"/>")),
    ]
}

/// Whether link text in `props`, with the `Hyperlink` style when `style`,
/// has the properties `run` has, so it joins that run: compared in place
/// (the run's style set aside a moment), no copy made.
fn same_link_props(run: &mut RunProps, props: &RunProps, style: bool) -> bool {
    if !style {
        return *run == *props;
    }
    if run.style_id.as_deref() != Some(HYPERLINK_STYLE) {
        return false;
    }
    let kept = run.style_id.take();
    let same = *run == *props;
    run.style_id = kept;
    same
}

/// The `w:id` and name of a bookmark start [`mark_inlines`] wrote.
#[cfg(test)]
fn mark_of(raw: &str) -> Option<(usize, String)> {
    let rest = raw.strip_prefix("<w:bookmarkStart w:id=\"")?;
    let (id, rest) = rest.split_once('"')?;
    let name = rest.strip_prefix(" w:name=\"")?.strip_suffix("\"/>")?;
    Some((id.parse().ok()?, crate::load::decode_attr(name)))
}

/// The heap bytes of the run properties an importer sets: font, colour,
/// highlight and style names, kept elements.
fn props_heap(p: &RunProps) -> usize {
    p.font.as_ref().map_or(0, String::len)
        + p.color.as_ref().map_or(0, String::len)
        + p.highlight.as_ref().map_or(0, String::len)
        + p.style_id.as_ref().map_or(0, String::len)
        + p.raw_props.iter().map(String::len).sum::<usize>()
}

/// Builds a [`Document`] from a stream of text, breaks, paragraph ends and
/// table cells, the shape all the importers produce. Everything it makes is
/// charged to the import's [`Budget`]; once that is out it makes nothing
/// more.
pub(crate) struct Builder<'b> {
    budget: &'b Budget,
    body: Vec<Block>,
    para: Vec<Inline>,
    /// The open tables, innermost last: an RTF table (one, made on its
    /// first cell paragraph) or HTML's nested ones ([`Builder::begin_table`]).
    tables: Vec<TableAcc>,
    /// The body's [`TableAcc::tail`].
    body_tail: Tail,
    /// Bookmarks waiting for the next content written (#1912): each `w:id`
    /// and name ([`Builder::bookmark`]).
    marks: Vec<(usize, String)>,
    /// Every bookmark name made so far, and so the next `w:id`.
    marked: std::collections::HashSet<String>,
    /// The bookmarks written into the paragraph in progress, as they were
    /// waiting: what [`Builder::clear_para`] puts back, unparsed.
    para_marks: Vec<(usize, String)>,
    /// The paragraph in progress holds content that keeps it (#1912 r2):
    /// text that is not all white space, an object or a page break. Only
    /// then do the waiting bookmarks go in ([`Builder::flush_marks`]), so a
    /// paragraph of spaces, tabs or line breaks, which the HTML import
    /// drops (`clear_para`), takes none and gives none back.
    keeps: bool,
    /// Text in a link in its user-agent look takes Word's `Hyperlink`
    /// character style (#1912, [`Builder::set_link_style`]).
    link_style: bool,
    /// How many times content that keeps a paragraph has been written
    /// (#1936, [`Builder::shown`]).
    shown: u64,
    /// The page's body width, in twips ([`Builder::set_body`], #1944).
    body_twips: u32,
}

#[derive(Default)]
struct TableAcc {
    rows: Vec<Row>,
    cells: Vec<Cell>,
    blocks: Vec<Block>,
    /// A paragraph has been ended in the current cell since it began, so an
    /// empty cell end need not add one.
    cell_has_para: bool,
    /// An HTML table: laid out from specs ([`html_table::layout`]) instead
    /// of `rows` / `cells`.
    html: Option<HtmlAcc>,
    /// What the importer said of `blocks`' last paragraph, and how many
    /// blocks there were then ([`Builder::set_tail`]).
    tail: Tail,
}

/// A flow's [`FlowTail`] and how many blocks it held when it was said.
type Tail = Option<(usize, FlowTail)>;

/// What an importer remembers of the paragraph it ended last in a flow
/// (a cell's blocks or the body), kept with that flow so a dropped card or
/// a nested table never lends it to another (#1979 r2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FlowTail {
    /// Its bottom spacing so far, in twips, signed (its written `after` is
    /// this, at least 0): the bottom boxes of the elements it was the last
    /// paragraph of, applied as each closed (#2114).
    pub(crate) after: i32,
    /// The largest positive margin of the margin group open at its bottom,
    /// in twips, already in `after`: what the next paragraph's top margins
    /// collapse with, and what a closing element's bottom margin collapses
    /// into when no padding separates them.
    pub(crate) open: i32,
    /// In quirks mode, `after` and `open` as they would be without the
    /// quirky user-agent bottom margins folded in (#1979, #2114 r2): what
    /// `after` becomes if the paragraph turns out its flow's last. `None`
    /// while no quirky margin has been folded.
    pub(crate) bare: Option<(i32, i32)>,
}

/// An HTML table being read: its spec, its rows of raw cells, the cell
/// open now and the nested tables in its pending blocks.
struct HtmlAcc {
    spec: html_table::TableSpec,
    rows: Vec<Vec<html_table::RawCell>>,
    cells: Vec<html_table::RawCell>,
    /// The open cell's spec, from [`Builder::begin_cell`] to its end.
    cell: Option<html_table::CellSpec>,
    /// Nested tables at placeholder indexes of `TableAcc::blocks`.
    nested: Vec<(usize, html_table::RawTable)>,
    /// The open cell's paragraphs in centred boxes, at their indexes of
    /// `TableAcc::blocks` ([`Builder::mark_cell_boxes`], #1930).
    boxes: Vec<(usize, html_table::CellCentring)>,
    /// Row indexes where a row group starts ([`html_table::RawTable::groups`]).
    groups: Vec<usize>,
    /// Its columns' widths so far ([`html_table::RawTable::cols`], #1894).
    cols: Vec<Option<html_table::Width>>,
    /// The open `colgroup`: its span, its width and whether a `col` has
    /// been in it (one with none stands for its span of columns).
    colgroup: Option<(u32, Option<html_table::Width>, bool)>,
}

impl HtmlAcc {
    /// `n` columns of `width`, the total cut at [`MAX_COLUMNS`].
    fn push_cols(&mut self, n: u32, width: Option<html_table::Width>) {
        let room = MAX_COLUMNS.saturating_sub(self.cols.len());
        let n = (n as usize).min(room);
        self.cols.extend(std::iter::repeat_n(width, n));
    }

    /// End the open `colgroup`: one with no `col` is its span of columns.
    fn end_colgroup(&mut self) {
        if let Some((span, width, false)) = self.colgroup.take() {
            self.push_cols(span, width);
        }
    }
}

impl<'b> Builder<'b> {
    pub(crate) fn new(budget: &'b Budget) -> Self {
        Builder {
            budget,
            body: Vec::new(),
            para: Vec::new(),
            tables: Vec::new(),
            body_tail: None,
            marks: Vec::new(),
            marked: std::collections::HashSet::new(),
            para_marks: Vec::new(),
            keeps: false,
            link_style: false,
            shown: 0,
            body_twips: html_table::BODY_TWIPS,
        }
    }

    /// The page's body width (#1944): the room an outermost HTML table has
    /// (its own declared room aside). [`html_table::BODY_TWIPS`] until set.
    pub(crate) fn set_body(&mut self, twips: u32) {
        self.body_twips = twips;
    }

    /// A count that grows whenever content that keeps a paragraph is
    /// written (#1936): text that is not all white space, an object (a
    /// picture), a page break. The HTML import compares it with what it was
    /// when an element opened to tell whether anything showed in it since.
    pub(crate) fn shown(&self) -> u64 {
        self.shown
    }

    /// Give text in a link Word's `Hyperlink` character style (#1912) when
    /// its properties are underlined and coloured: they then hold all the
    /// style sets (`markdown_style_def("Hyperlink")`), so nothing drawn
    /// changes; a link the page restyled to no underline or no colour gets
    /// none. Only for an import saved through a package defining the style.
    pub(crate) fn set_link_style(&mut self, on: bool) {
        self.link_style = on;
    }

    /// A point bookmark `name` (#1912), placed right before the next
    /// content written in the flow once its paragraph holds content that
    /// keeps it ([`Builder::keeps`]: text not all white space, an object,
    /// a page break; then a tab or break too), so it never makes a
    /// paragraph of its own; one left at the end goes at
    /// the end of the body's last paragraph ([`Builder::finish`]). A name
    /// already made, an empty one or one past [`MAX_BOOKMARKS`] makes
    /// nothing. `w:id`s count up from 0.
    pub(crate) fn bookmark(&mut self, name: &str) {
        if name.is_empty()
            || self.marked.len() >= MAX_BOOKMARKS
            || self.marked.contains(name)
            || !self.budget.keep::<Inline>(3 * name.len() + 96)
        {
            return;
        }
        let id = self.marked.len();
        self.marked.insert(name.to_string());
        self.marks.push((id, name.to_string()));
    }

    /// Write the waiting bookmarks into the paragraph in progress, before
    /// content that `keeps` it, or any content once it is kept. Each mark
    /// written is charged: at most once a paragraph that keeps content (a
    /// typed list marker taken off can leave none, `clear_para`).
    fn flush_marks(&mut self, keeps: bool) {
        self.keeps |= keeps;
        if keeps {
            self.shown += 1;
        }
        if !self.keeps || self.marks.is_empty() {
            return;
        }
        if !self.budget.work(self.marks.len()) {
            self.marks.clear();
            return;
        }
        for (id, name) in &self.marks {
            self.para.extend(mark_inlines(*id, name));
        }
        self.para_marks.append(&mut self.marks);
    }

    /// The paragraph in progress is gone (ended or cleared): nothing in it
    /// keeps it, no mark is in it.
    fn para_gone(&mut self) {
        self.keeps = false;
        self.para_marks.clear();
    }

    /// Append `text` in `props`, joining the previous run when it has the
    /// same properties.
    pub(crate) fn text(&mut self, text: &str, props: &RunProps) {
        self.link_text(text, props, None);
    }

    /// Append `text` in `props`, in `link` when there is one (#1879): it
    /// joins the paragraph's last inline when that is a link the importer
    /// made to the same place (its last run when the properties are the
    /// same, else as a run of its own), else it starts a hyperlink. Without
    /// a link, as [`Builder::text`].
    pub(crate) fn link_text(&mut self, text: &str, props: &RunProps, link: Option<&Link>) {
        if text.is_empty() || !self.budget.scanned(text.len()) || !self.budget.alloc(text.len()) {
            return;
        }
        self.flush_marks(!text.chars().all(char::is_whitespace));
        if let Some(link) = link {
            // The `Hyperlink` style, added only where a run is made.
            let style = self.link_style && link_look(props);
            let made = || {
                let mut p = props.clone();
                if style {
                    p.style_id = Some(HYPERLINK_STYLE.to_string());
                }
                p
            };
            if let Some(Inline::Hyperlink(h)) = self.para.last_mut()
                && link.made(h)
            {
                if let Some(run) = h.runs.last_mut()
                    && same_link_props(&mut run.props, props, style)
                {
                    run.text.push_str(text);
                    return;
                }
                if self
                    .budget
                    .keep::<Run>(props_heap(props) + HYPERLINK_STYLE.len())
                {
                    h.runs.push(Run {
                        text: text.to_string(),
                        props: made(),
                    });
                }
                return;
            }
            let props = &made();
            let heap = std::mem::size_of::<Run>() + props_heap(props) + link.heap();
            if !self.budget.keep::<Inline>(heap) {
                return;
            }
            let (target, anchor) = match &link.to {
                LinkTo::Url(u) => (Some(u.clone()), None),
                LinkTo::Anchor(a) => (None, Some(a.clone())),
            };
            let raw = link.opener.clone();
            self.para.push(Inline::Hyperlink(Hyperlink {
                target,
                anchor,
                runs: vec![Run {
                    text: text.to_string(),
                    props: props.clone(),
                }],
                // A kept opener is rebuilt with the runs on save.
                content_changed: raw.is_some(),
                raw,
                ..Hyperlink::default()
            }));
            return;
        }
        if let Some(Inline::Run(run)) = self.para.last_mut() {
            if run.props == *props {
                run.text.push_str(text);
                return;
            }
        }
        if !self.budget.keep::<Inline>(props_heap(props)) {
            return;
        }
        self.para.push(Inline::Run(Run {
            text: text.to_string(),
            props: props.clone(),
        }));
    }

    pub(crate) fn tab(&mut self, props: &RunProps) {
        self.flush_marks(false);
        if self.budget.keep::<Inline>(props_heap(props)) {
            self.para.push(Inline::Tab(props.clone()));
        }
    }

    pub(crate) fn line_break(&mut self, props: &RunProps) {
        self.flush_marks(false);
        if self.budget.keep::<Inline>(props_heap(props)) {
            self.para
                .push(Inline::Break(BreakKind::Line, props.clone()));
        }
    }

    pub(crate) fn page_break(&mut self, props: &RunProps) {
        self.flush_marks(true);
        if self.budget.keep::<Inline>(props_heap(props)) {
            self.para
                .push(Inline::Break(BreakKind::Page, props.clone()));
        }
    }

    /// Drop the line break that ends the paragraph in progress, when
    /// something comes before it (#2114): a browser's `<br>` that ends a
    /// block adds no line, while a lone one (`<div><br></div>`) is the
    /// block's one empty line.
    pub(crate) fn drop_trailing_break(&mut self) {
        // Bookmarks (#1912) are no content: `<div id=x><br></div>` keeps
        // its one line whether `x` is a bookmark or not.
        let content = self.para.iter().filter(|i| !is_mark(i)).count();
        if content > 1 && matches!(self.para.last(), Some(Inline::Break(BreakKind::Line, _))) {
            self.para.pop();
        }
    }

    /// Whether the paragraph in progress shows anything, its mark hidden
    /// or not ([`shows`], #1883).
    pub(crate) fn para_shows(&self, mark_vanish: bool) -> bool {
        shows(&self.para, mark_vanish)
    }

    /// The properties of the last paragraph that shows anything among the
    /// flow's blocks from `from` on (#1883 r4): a table or a hidden
    /// paragraph after it is passed over. A unit of work per block looked
    /// at.
    pub(crate) fn last_shown_para_mut(
        &mut self,
        in_table: bool,
        from: usize,
    ) -> Option<&mut ParProps> {
        let budget = self.budget;
        let (blocks, _) = self.flow_mut(in_table)?;
        for b in blocks.get_mut(from..)?.iter_mut().rev() {
            if !budget.op() {
                return None;
            }
            if let Block::Paragraph(p) = b {
                if shows(&p.content, mark_vanish(&p.props)) {
                    return Some(&mut p.props);
                }
            }
        }
        None
    }

    /// Whether the paragraph in progress holds a page break (#1883): kept
    /// though it has no text.
    pub(crate) fn has_page_break(&self) -> bool {
        self.para
            .iter()
            .any(|i| matches!(i, Inline::Break(BreakKind::Page, _)))
    }

    /// The text of the paragraph in progress.
    pub(crate) fn para_text(&self) -> String {
        self.para.iter().map(|i| i.text()).collect()
    }

    /// Append an object (an imported picture's run, #1880) to the paragraph
    /// in progress, charged with its XML.
    pub(crate) fn object(&mut self, inline: Inline, heap: usize) {
        self.flush_marks(true);
        if self.budget.keep::<Inline>(heap) {
            self.para.push(inline);
        }
    }

    /// Whether the paragraph in progress holds an object: content though it
    /// has no text.
    pub(crate) fn has_object(&self) -> bool {
        self.para.iter().any(is_object)
    }

    /// Drop everything in the paragraph in progress (text only made of
    /// spaces that stands for an empty paragraph). Its bookmarks (#1912)
    /// wait again, for the next content, ahead of the ones waiting.
    pub(crate) fn clear_para(&mut self) {
        self.para.clear();
        // Only a kept paragraph has marks in it (`flush_marks`), so this is
        // rare: a typed list marker was all that kept it.
        if !self.para_marks.is_empty() && self.budget.work(self.para_marks.len()) {
            self.para_marks.append(&mut self.marks);
            self.marks = std::mem::take(&mut self.para_marks);
        }
        self.para_gone();
    }

    /// Remove a list marker typed at the start of the paragraph in progress
    /// (`·`, `1.`, then spaces or a tab) and return it; `None`, changing
    /// nothing, when the paragraph does not start with a short word followed
    /// by white space.
    pub(crate) fn take_leading_marker(&mut self) -> Option<String> {
        let text = self.para_text();
        let marker: String = text.chars().take_while(|c| !c.is_whitespace()).collect();
        let n = marker.chars().count();
        if n == 0 || n > 6 {
            return None;
        }
        let rest = text.chars().skip(n);
        let gap = rest.take_while(|c| c.is_whitespace()).count();
        if gap == 0 {
            return None;
        }
        // An object (an imported picture, #1880) or a link (#1879) before
        // the marker's end: the paragraph does not start with a typed
        // marker (only a leading run can be cut). A bookmark (#1912) is
        // passed over and stays.
        let mut left = n + gap;
        for inline in &self.para {
            if left == 0 {
                break;
            }
            if is_mark(inline) {
                continue;
            }
            if is_object(inline) || matches!(inline, Inline::Hyperlink(_)) {
                return None;
            }
            let len = inline.text().chars().count();
            if len > left {
                break;
            }
            left -= len;
        }
        // Drop `n + gap` characters from the front, in one pass: the
        // inlines consumed whole, then the front of the run the marker
        // ends in; the bookmarks stay where they were.
        let mut drop = n + gap;
        let mut kept = Vec::with_capacity(self.para.len());
        for mut inline in self.para.drain(..) {
            if drop > 0 && !is_mark(&inline) {
                let len = inline.text().chars().count();
                if len <= drop {
                    drop -= len;
                    continue;
                }
                // Only a run is cut; any other inline ends the marker.
                if let Inline::Run(run) = &mut inline {
                    run.text = run.text.chars().skip(drop).collect();
                }
                drop = 0;
            }
            kept.push(inline);
        }
        self.para = kept;
        // What is left may be only white space now: the paragraph is kept
        // only by what still keeps it.
        self.keeps = self.para.iter().any(|i| match i {
            Inline::Run(r) => !r.text.chars().all(char::is_whitespace),
            Inline::Hyperlink(h) => h
                .runs
                .iter()
                .any(|r| !r.text.chars().all(char::is_whitespace)),
            Inline::Break(BreakKind::Page, _) => true,
            i => is_object(i),
        });
        Some(marker)
    }

    /// End the paragraph in progress, with `props`, in the open table cell
    /// when `in_table`, else in the body (closing a table left open).
    pub(crate) fn end_para(&mut self, props: ParProps, in_table: bool) {
        self.para_gone();
        if !self.budget.keep::<Block>(0) {
            self.para.clear();
            return;
        }
        let p = Block::Paragraph(Paragraph {
            props,
            content: std::mem::take(&mut self.para),
        });
        if in_table {
            let t = self.top();
            t.blocks.push(p);
            t.cell_has_para = true;
        } else {
            self.end_tables();
            self.body.push(p);
        }
    }

    /// The blocks a paragraph ended now goes after: the open cell's (the
    /// innermost table's) when `in_table`, else the body's, `None` when
    /// tables are left open there (a body paragraph closes them first).
    fn flow(&self, in_table: bool) -> Option<&Vec<Block>> {
        match self.tables.last() {
            Some(t) if in_table => Some(&t.blocks),
            None if !in_table => Some(&self.body),
            _ => None,
        }
    }

    /// Whether a paragraph ended now would be the first block of its flow
    /// ([`Builder::flow`]): its cell's or the body's (#1979).
    pub(crate) fn flow_is_empty(&self, in_table: bool) -> bool {
        match self.flow(in_table) {
            Some(blocks) => blocks.is_empty(),
            // A body paragraph after tables left open follows them.
            None => in_table,
        }
    }

    /// [`Builder::flow`], with its tail, to change.
    fn flow_mut(&mut self, in_table: bool) -> Option<(&mut Vec<Block>, &mut Tail)> {
        match self.tables.last_mut() {
            Some(t) if in_table => Some((&mut t.blocks, &mut t.tail)),
            None if !in_table => Some((&mut self.body, &mut self.body_tail)),
            _ => None,
        }
    }

    /// How many blocks the flow holds (#1979): with [`Builder::para_mut`],
    /// where a paragraph ended now is found again.
    pub(crate) fn flow_len(&self, in_table: bool) -> Option<usize> {
        self.flow(in_table).map(Vec::len)
    }

    /// The properties of the paragraph at `index` of the flow, if that
    /// block is one.
    pub(crate) fn para_mut(&mut self, in_table: bool, index: usize) -> Option<&mut ParProps> {
        match self.flow_mut(in_table)?.0.get_mut(index)? {
            Block::Paragraph(p) => Some(&mut p.props),
            _ => None,
        }
    }

    /// Remember `tail` of the flow's last block, a paragraph just ended
    /// (#1979 r2).
    pub(crate) fn set_tail(&mut self, in_table: bool, tail: FlowTail) {
        if let Some((blocks, slot)) = self.flow_mut(in_table) {
            let ends_in_para = matches!(blocks.last(), Some(Block::Paragraph(_)));
            *slot = ends_in_para.then_some((blocks.len(), tail));
        }
    }

    /// The tail remembered of the flow's last block, while that is still
    /// the paragraph it was said of: what a paragraph ended now follows.
    pub(crate) fn tail(&self, in_table: bool) -> Option<FlowTail> {
        let (blocks, slot) = match self.tables.last() {
            Some(t) if in_table => (&t.blocks, t.tail),
            None if !in_table => (&self.body, self.body_tail),
            _ => return None,
        };
        slot.filter(|(len, _)| *len == blocks.len())
            .filter(|_| matches!(blocks.last(), Some(Block::Paragraph(_))))
            .map(|(_, tail)| tail)
    }

    /// The flow's last block's properties, if it is a paragraph.
    pub(crate) fn flow_last_mut(&mut self, in_table: bool) -> Option<&mut ParProps> {
        let index = self.flow_len(in_table)?.checked_sub(1)?;
        self.para_mut(in_table, index)
    }

    /// The innermost HTML table's flow ended its last block, a paragraph,
    /// inside the centred boxes `c` names: its cell's layout centres it
    /// there (#1930).
    pub(crate) fn mark_cell_boxes(&mut self, c: html_table::CellCentring) {
        if let Some(t) = self.tables.last_mut() {
            let at = t.blocks.len().checked_sub(1);
            if let (Some(h), Some(at)) = (t.html.as_mut(), at) {
                h.boxes.push((at, c));
            }
        }
    }

    /// End the paragraph in progress only when it has content.
    pub(crate) fn end_para_if_any(&mut self, props: ParProps, in_table: bool) {
        if !self.para.is_empty() {
            self.end_para(props, in_table);
        }
    }

    /// The innermost open table, made when there is none (an RTF table's
    /// first cell paragraph).
    fn top(&mut self) -> &mut TableAcc {
        if self.tables.is_empty() {
            self.tables.push(TableAcc::default());
        }
        self.tables.last_mut().expect("just pushed")
    }

    /// Open an HTML table (#1878), inside the open one's cell when there is
    /// one: it is laid out from `spec` and its cells' specs when the
    /// outermost table ends.
    pub(crate) fn begin_table(&mut self, spec: html_table::TableSpec) {
        self.tables.push(TableAcc {
            html: Some(HtmlAcc {
                spec,
                rows: Vec::new(),
                cells: Vec::new(),
                cell: None,
                nested: Vec::new(),
                boxes: Vec::new(),
                groups: Vec::new(),
                cols: Vec::new(),
                colgroup: None,
            }),
            ..TableAcc::default()
        });
    }

    /// The innermost HTML table being read.
    fn html_acc(&mut self) -> Option<&mut HtmlAcc> {
        self.tables.last_mut().and_then(|t| t.html.as_mut())
    }

    /// A `colgroup` of the innermost HTML table opens (#1894): `span`
    /// columns of `width` unless `col`s in it say otherwise.
    pub(crate) fn begin_colgroup(&mut self, span: u32, width: Option<html_table::Width>) {
        if let Some(h) = self.html_acc() {
            h.end_colgroup();
            h.colgroup = Some((span, width, false));
        }
    }

    /// A `colgroup` of the innermost HTML table ends (#1894).
    pub(crate) fn end_colgroup(&mut self) {
        if let Some(h) = self.html_acc() {
            h.end_colgroup();
        }
    }

    /// A `col` of the innermost HTML table (#1894): `span` columns of its
    /// width, else its `colgroup`'s.
    pub(crate) fn table_col(&mut self, span: u32, width: Option<html_table::Width>) {
        if let Some(h) = self.html_acc() {
            let width = match &mut h.colgroup {
                Some((_, group, had)) => {
                    *had = true;
                    width.or(*group)
                }
                None => width,
            };
            h.push_cols(span, width);
        }
    }

    /// Open a cell of the innermost HTML table with `spec`: its end, or the
    /// end of its row or table, makes it even when it holds nothing.
    pub(crate) fn begin_cell(&mut self, spec: html_table::CellSpec) {
        if let Some(h) = self.html_acc() {
            h.end_colgroup();
            h.cell = Some(spec);
        }
    }

    /// A row group of the innermost HTML table starts or ends here: the
    /// rows after it are another group, which no `rowspan` reaches into.
    pub(crate) fn row_group_boundary(&mut self) {
        if let Some(h) = self.html_acc() {
            h.end_colgroup();
            let at = h.rows.len();
            if at > 0 && h.groups.last() != Some(&at) {
                h.groups.push(at);
            }
        }
    }

    /// Whether the innermost table has a begun cell open.
    fn cell_open(&self) -> bool {
        self.tables
            .last()
            .and_then(|t| t.html.as_ref())
            .is_some_and(|h| h.cell.is_some())
    }

    /// End a table cell: text since the last paragraph end is its last
    /// paragraph, and a cell always has at least one. A row keeps at most
    /// [`MAX_COLUMNS`] cells: past that, a cell's paragraphs join the last.
    pub(crate) fn end_cell(&mut self, props: ParProps) {
        let has_para = self.tables.last().is_some_and(|t| t.cell_has_para);
        if !self.para.is_empty() || !has_para {
            self.end_para(props, true);
        }
        if !self.budget.keep::<Cell>(0) {
            return;
        }
        let t = self.top();
        let blocks = std::mem::take(&mut t.blocks);
        t.tail = None;
        t.cell_has_para = false;
        if let Some(h) = t.html.as_mut() {
            let cell = html_table::RawCell {
                spec: h.cell.take().unwrap_or_default(),
                blocks,
                nested: std::mem::take(&mut h.nested),
                boxes: std::mem::take(&mut h.boxes),
            };
            // Past MAX_COLUMNS the cell (and its spec) joins the last.
            let full = h.cells.len() >= MAX_COLUMNS;
            match h.cells.last_mut() {
                Some(last) if full => last.absorb(cell),
                _ => h.cells.push(cell),
            }
            return;
        }
        let full = t.cells.len() >= MAX_COLUMNS;
        match t.cells.last_mut() {
            Some(last) if full => last.blocks.extend(blocks),
            _ => t.cells.push(Cell {
                blocks,
                ..Cell::default()
            }),
        }
    }

    /// End a table row. A row with text but no cell end closes that cell,
    /// as does one with a begun cell open.
    pub(crate) fn end_row(&mut self, props: ParProps) {
        let open_text = !self.para.is_empty()
            || self.tables.last().is_some_and(|t| !t.blocks.is_empty())
            || self.cell_open();
        if open_text {
            self.end_cell(props);
        }
        if !self.budget.keep::<Row>(0) {
            return;
        }
        if let Some(t) = self.tables.last_mut() {
            if let Some(h) = t.html.as_mut() {
                if !h.cells.is_empty() {
                    h.rows.push(std::mem::take(&mut h.cells));
                }
            } else if !t.cells.is_empty() {
                t.rows.push(Row {
                    cells: std::mem::take(&mut t.cells),
                    ..Row::default()
                });
            }
        }
    }

    /// End the innermost table, an HTML card (#1927): its row and table as
    /// `</table>` ends them, or, with nothing in it (no text, object or
    /// table; only empty paragraphs), nothing at all: an e-mail's coloured
    /// divider `div` leaves no empty shaded row behind.
    pub(crate) fn end_card(&mut self, props: ParProps) {
        let empty = self.para.is_empty()
            && self.tables.last().is_some_and(|t| {
                t.html
                    .as_ref()
                    .is_some_and(|h| h.rows.is_empty() && h.cells.is_empty() && h.nested.is_empty())
                    && t.blocks
                        .iter()
                        .all(|b| matches!(b, Block::Paragraph(p) if p.content.is_empty()))
            });
        if empty {
            self.tables.pop();
            return;
        }
        self.end_row(props);
        self.end_table();
    }

    /// Close every open table, innermost first.
    fn end_tables(&mut self) {
        while !self.tables.is_empty() {
            self.end_table();
        }
    }

    /// Close the innermost table in progress, if any: into the open cell of
    /// the table around it (an HTML table), else into the body.
    pub(crate) fn end_table(&mut self) {
        let Some(mut t) = self.tables.pop() else {
            return;
        };
        if let Some(h) = t.html.take() {
            return self.end_html_table(t, h);
        }
        if !t.cells.is_empty() || !t.blocks.is_empty() {
            // Cells with no row end: keep them as a last row (a full row's
            // pending paragraphs join its last cell, as in end_cell).
            if !t.blocks.is_empty() {
                let blocks = std::mem::take(&mut t.blocks);
                let full = t.cells.len() >= MAX_COLUMNS;
                match t.cells.last_mut() {
                    Some(last) if full => last.blocks.extend(blocks),
                    _ => t.cells.push(Cell {
                        blocks,
                        ..Cell::default()
                    }),
                }
            }
            t.rows.push(Row {
                cells: std::mem::take(&mut t.cells),
                ..Row::default()
            });
        }
        if t.rows.is_empty() {
            return;
        }
        // At most MAX_COLUMNS (end_cell keeps rows to that), so the padding
        // below is at most that many cells a row, each charged.
        let ncols = t
            .rows
            .iter()
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1)
            .clamp(1, MAX_COLUMNS);
        // Rows shorter than the widest get empty cells, so the grid is square.
        for row in &mut t.rows {
            while row.cells.len() < ncols && self.budget.keep::<Cell>(0) {
                row.cells.push(Cell {
                    blocks: vec![Block::Paragraph(Paragraph::default())],
                    ..Cell::default()
                });
            }
        }
        // A 6.5" text column shared evenly.
        let w = (9360 / ncols) as u32;
        self.body.push(Block::Table(Table {
            grid: vec![w; ncols],
            rows: t.rows,
            ..Table::default()
        }));
    }

    /// The end of HTML table `t`: what is pending is its last cell and row;
    /// nested, it waits in its enclosing cell as a [`html_table::RawTable`]
    /// at a placeholder; outermost, it is laid out in the body's width, or
    /// in the room of the centred block it is in (#2114) or a body card's
    /// (#1940), after an empty paragraph when it follows another table.
    fn end_html_table(&mut self, mut t: TableAcc, mut h: HtmlAcc) {
        if !t.blocks.is_empty() || h.cell.is_some() {
            let cell = html_table::RawCell {
                spec: h.cell.take().unwrap_or_default(),
                blocks: std::mem::take(&mut t.blocks),
                nested: std::mem::take(&mut h.nested),
                boxes: std::mem::take(&mut h.boxes),
            };
            if self.budget.keep::<Cell>(0) {
                let full = h.cells.len() >= MAX_COLUMNS;
                match h.cells.last_mut() {
                    Some(last) if full => last.absorb(cell),
                    _ => h.cells.push(cell),
                }
            }
        }
        if !h.cells.is_empty() && self.budget.keep::<Row>(0) {
            h.rows.push(std::mem::take(&mut h.cells));
        }
        if h.rows.is_empty() {
            return;
        }
        h.end_colgroup();
        let mut raw = html_table::RawTable {
            spec: h.spec,
            rows: h.rows,
            groups: h.groups,
            cols: h.cols,
        };
        match self.tables.last_mut() {
            Some(outer) if outer.html.is_some() => {
                // Two tables back to back in a cell are kept apart by an
                // empty paragraph, or Word joins them (#1927).
                if outer.blocks.last().is_some_and(html_table::is_placeholder)
                    && self.budget.keep::<Block>(0)
                {
                    outer.blocks.push(Block::Paragraph(Paragraph::default()));
                }
                // A nested table takes its cell's width: a room (#2114),
                // which only a table outside every cell gets, can still
                // come with one a malformed page leaves in an open table
                // but outside its cells, and does not apply there.
                raw.spec.room = None;
                let at = outer.blocks.len();
                outer.blocks.push(html_table::placeholder());
                outer.cell_has_para = true;
                if let Some(oh) = outer.html.as_mut() {
                    oh.nested.push((at, raw));
                }
            }
            _ => {
                // And in the body (#1940): a table or card right after
                // another would be joined to it.
                if matches!(self.body.last(), Some(Block::Table(_))) && self.budget.keep::<Block>(0)
                {
                    self.body.push(Block::Paragraph(Paragraph::default()));
                }
                let avail = raw.spec.room.map_or(self.body_twips, |r| r.width);
                let table = html_table::layout(raw, avail, self.budget);
                self.body.push(Block::Table(table));
            }
        }
    }

    pub(crate) fn finish(mut self, props: ParProps) -> Document {
        let in_table = !self.tables.is_empty() && !self.para.is_empty();
        if in_table {
            self.end_cell(props.clone());
        } else {
            self.end_para_if_any(props, false);
        }
        self.end_tables();
        // Bookmarks no content came after (#1912) end the body's last
        // paragraph; after a table they are dropped (no new paragraph).
        if let Some(Block::Paragraph(p)) = self.body.last_mut() {
            for (id, name) in self.marks.drain(..) {
                p.content.extend(mark_inlines(id, &name));
            }
        }
        Document { body: self.body }
    }

    /// Whether nothing at all has been produced.
    pub(crate) fn is_empty(&self) -> bool {
        self.body.is_empty() && self.para.is_empty() && self.tables.is_empty()
    }
}

/// Text of every paragraph of `doc`, tables row-major, one entry per
/// paragraph: what the tests compare.
pub fn paragraph_texts(doc: &Document) -> Vec<String> {
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => out.push(p.plain_text()),
                Block::Table(t) => {
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
    walk(&doc.body, &mut out);
    out
}

/// Pin the left and first-line indents of every list item the HTML importer
/// made (#2231): it computes where each item draws (its `<li>`'s padding and
/// margins, #1979, #2114), so a 0 is that value, not "inherit". Pinned,
/// neither the numbering level's indent nor a style's reaches it, in the
/// page view or in Word after a save. A nonzero value is direct already.
/// The RTF and PDF importers do not compute list indents (every item comes
/// at 0), so theirs stay unpinned and take their level's indent, as a save
/// and Word give them.
pub(crate) fn pin_list_indents(blocks: &mut [Block]) {
    for b in blocks {
        match b {
            Block::Paragraph(p) if p.props.in_list() => {
                if !p.props.has_direct_left_indent() {
                    p.props.pin_left_indent_zero();
                }
                if !p.props.has_direct_first_line() {
                    p.props.pin_first_line(0);
                }
            }
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        pin_list_indents(&mut cell.blocks);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paragraph in progress as a sketch: `Mark <name>` / `End` for a
    /// bookmark's halves, else the inline's text.
    fn para_sketch(b: &Builder) -> Vec<String> {
        b.para
            .iter()
            .map(|i| match i {
                Inline::Raw(r) if r.starts_with("<w:bookmarkStart ") => {
                    format!("Mark {}", mark_of(r).unwrap().1)
                }
                Inline::Raw(r) if r.starts_with("<w:bookmarkEnd ") => "End".into(),
                i => i.text(),
            })
            .collect()
    }

    #[test]
    fn a_bookmark_waits_for_content_and_is_none_itself_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let props = RunProps::default();
        b.bookmark("a");
        b.bookmark("a"); // the first wins
        b.bookmark("");
        assert!(b.para.is_empty(), "no content, no mark yet");
        b.text("1. Item", &props);
        assert_eq!(para_sketch(&b), ["Mark a", "End", "1. Item"]);
        // Not an object, and a paragraph shows only its content.
        assert!(!b.has_object());
        assert!(shows(&b.para, true));
        assert!(!shows(&b.para[..2], true));
        // A typed marker is still taken, the mark kept in front.
        assert_eq!(b.take_leading_marker().as_deref(), Some("1."));
        assert_eq!(para_sketch(&b), ["Mark a", "End", "Item"]);
        // The ids count up.
        b.bookmark("b");
        b.tab(&props);
        let raws: Vec<&String> = b
            .para
            .iter()
            .filter_map(|i| match i {
                Inline::Raw(r) => Some(r),
                _ => None,
            })
            .collect();
        assert_eq!(
            raws,
            [
                "<w:bookmarkStart w:id=\"0\" w:name=\"a\"/>",
                "<w:bookmarkEnd w:id=\"0\"/>",
                "<w:bookmarkStart w:id=\"1\" w:name=\"b\"/>",
                "<w:bookmarkEnd w:id=\"1\"/>",
            ]
        );
    }

    #[test]
    fn a_marker_split_by_a_bookmark_is_still_taken_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let props = RunProps::default();
        b.text("1.", &props);
        b.bookmark("x");
        b.text(
            " Item",
            &RunProps {
                bold: true,
                ..RunProps::default()
            },
        );
        assert_eq!(b.take_leading_marker().as_deref(), Some("1."));
        assert_eq!(para_sketch(&b), ["Mark x", "End", "Item"]);
    }

    #[test]
    fn a_trailing_break_after_a_bookmark_is_the_lines_one_break_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let props = RunProps::default();
        b.bookmark("x");
        b.line_break(&props);
        b.drop_trailing_break();
        // A break alone does not keep its paragraph (r2): the mark waits.
        assert_eq!(para_sketch(&b), ["\n"]);
        b.text("t", &props);
        b.line_break(&props);
        b.drop_trailing_break();
        assert_eq!(para_sketch(&b), ["\n", "Mark x", "End", "t"]);
        // Once kept, a break takes the waiting marks before it.
        b.bookmark("y");
        b.line_break(&props);
        assert_eq!(
            para_sketch(&b),
            ["\n", "Mark x", "End", "t", "Mark y", "End", "\n"]
        );
    }

    #[test]
    fn white_space_takes_no_bookmark_and_a_paragraph_a_marker_emptied_gives_them_back_r2_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let props = RunProps::default();
        b.bookmark("x");
        b.text("\u{a0} ", &props);
        b.tab(&props);
        assert_eq!(para_sketch(&b), ["\u{a0} ", "\t"]);
        b.clear_para();
        // A typed marker alone kept the paragraph; taken off, it is empty
        // and its marks wait again.
        b.text("1. ", &props);
        assert_eq!(para_sketch(&b), ["Mark x", "End", "1. "]);
        assert_eq!(b.take_leading_marker().as_deref(), Some("1."));
        b.text("\u{a0}", &props);
        assert_eq!(para_sketch(&b), ["Mark x", "End", "\u{a0}"]);
        b.clear_para();
        b.text("t", &props);
        assert_eq!(para_sketch(&b), ["Mark x", "End", "t"]);
    }

    #[test]
    fn a_cleared_paragraphs_bookmarks_wait_again_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let props = RunProps::default();
        b.bookmark("x");
        b.text("\u{a0}", &props);
        b.bookmark("y");
        b.clear_para();
        assert!(b.para.is_empty());
        b.text("t", &props);
        assert_eq!(para_sketch(&b), ["Mark x", "End", "Mark y", "End", "t"]);
    }

    #[test]
    fn bookmarks_left_at_the_end_go_into_the_last_paragraph_or_nowhere_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let props = RunProps::default();
        b.text("t", &props);
        b.end_para(ParProps::default(), false);
        b.bookmark("end");
        let d = b.finish(ParProps::default());
        assert_eq!(d.body.len(), 1);
        let Block::Paragraph(p) = &d.body[0] else {
            panic!()
        };
        assert_eq!(p.content.len(), 3, "{:?}", p.content);
        assert!(is_mark(&p.content[2]));
        // Nothing to put them in: dropped, no paragraph made.
        let mut b = Builder::new(&budget);
        b.bookmark("end");
        assert!(b.finish(ParProps::default()).body.is_empty());
    }

    #[test]
    fn bookmarks_past_the_cap_make_nothing_1912() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        for n in 0..MAX_BOOKMARKS + 3 {
            b.bookmark(&format!("b{n}"));
        }
        b.text("t", &RunProps::default());
        assert_eq!(b.para.len(), 2 * MAX_BOOKMARKS + 1);
    }

    /// #2231: the HTML importer's list items keep the indents it computed,
    /// a 0 included, as direct ones, so neither a numbering level's indent
    /// nor a style's reaches them. The RTF importer computes none (every
    /// item comes at 0), so its items stay unpinned and save with no `w:ind`:
    /// they take their level's indent, as Word gives a pasted bullet.
    #[test]
    fn html_list_items_pin_their_indents_and_rtf_items_do_not() {
        let items = |d: &Document| -> Vec<(i32, i32, bool, bool)> {
            d.body
                .iter()
                .filter_map(|b| match b {
                    Block::Paragraph(p) if p.props.in_list() => Some((
                        p.props.indent,
                        p.props.first_line,
                        p.props.has_direct_left_indent(),
                        p.props.has_direct_first_line(),
                    )),
                    _ => None,
                })
                .collect()
        };
        let html = html::import_html(
            br#"<ul style="padding-left:0;margin:0"><li>flat</li></ul><ol><li>padded</li></ol>"#,
        )
        .unwrap();
        let got = items(&html);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|&(_, _, l, f)| l && f), "{got:?}");
        assert_eq!((got[0].0, got[0].1), (0, 0));
        assert!(got[1].0 > 0, "the padded item keeps its indent: {got:?}");
        let rtf = rtf::import_rtf(
            br"{\rtf1{\*\listtable{\list x}}\pard\ls1\ilvl0\li720\fi-360{\listtext\f3\'b7\tab}Apples\par\pard\ls1{\listtext\f3\'b7\tab}Bananas\par}",
        )
        .unwrap();
        let got = items(&rtf);
        assert_eq!(got.len(), 2, "{:?}", rtf.body);
        assert!(got.iter().all(|&(_, _, l, f)| !l && !f), "{got:?}");
        let xml = crate::serialize::document_to_xml(&rtf);
        assert!(xml.contains("<w:numId w:val=\"1\"/>"), "{xml}");
        assert!(
            !xml.contains("<w:ind"),
            "an RTF bullet saves no w:ind: {xml}"
        );
        // A table cell's item too, and a paragraph outside a list is untouched.
        let item = |num_id| {
            Block::Paragraph(Paragraph {
                props: ParProps {
                    num_id,
                    ..ParProps::default()
                },
                content: Vec::new(),
            })
        };
        let mut t = crate::table::new_table(1, 1, 9000, crate::table::AutoFit::Fixed(None));
        t.rows[0].cells[0].blocks = vec![item(Some(1))];
        let mut body = vec![Block::Table(t), item(None)];
        pin_list_indents(&mut body);
        let (Block::Table(t), Block::Paragraph(plain)) = (&body[0], &body[1]) else {
            panic!("{body:?}")
        };
        let Block::Paragraph(cell_item) = &t.rows[0].cells[0].blocks[0] else {
            panic!("a paragraph")
        };
        assert!(cell_item.props.has_direct_first_line());
        assert!(cell_item.props.has_direct_left_indent());
        assert!(plain.props.shadow.is_empty());
    }

    #[test]
    fn sniff_reads_content_before_the_extension() {
        assert_eq!(sniff(b"{\\rtf1\\ansi hi}", "docx"), Format::Rtf);
        assert_eq!(sniff(b"\xEF\xBB\xBF  {\\rtf1}", ""), Format::Rtf);
        assert_eq!(sniff(b"%PDF-1.7\n", "docx"), Format::Pdf);
        assert_eq!(sniff(b"junk\r\n%PDF-1.4", ""), Format::Pdf);
        assert_eq!(sniff(b"PK\x03\x04rest", "rtf"), Format::Docx);
        assert_eq!(
            sniff(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0], "docx"),
            Format::Cfb
        );
        assert_eq!(sniff(b"<html><body>x</body></html>", "doc"), Format::Html);
        assert_eq!(sniff(b"<!DOCTYPE HTML PUBLIC>", ""), Format::Html);
        assert_eq!(sniff(b"plain words", "htm"), Format::Html);
        assert_eq!(sniff(b"plain words", "DOCX"), Format::Docx);
        assert_eq!(sniff(b"plain words", "txt"), Format::Unknown);
        assert_eq!(sniff(b"<note>xml</note>", "xml"), Format::Unknown);
        // Mentioning the PDF header does not make a page or an RTF a PDF.
        assert_eq!(
            sniff(
                b"<html><head><title>Notes on %PDF-1.7 headers</title>",
                "htm"
            ),
            Format::Html
        );
        assert_eq!(sniff(b"{\\rtf1 about %PDF-1.4}", "rtf"), Format::Rtf);
    }

    #[test]
    fn list_markers_say_numbered_or_bullet() {
        // A multilevel list's compound markers too (#635 FIX r1).
        for m in ["1.", "12.", "a)", "(iv)", "B.", "1.1.", "2.3.4.", "1.a)"] {
            assert!(marker_is_numbered(m), "{m}");
        }
        for m in [
            "\u{b7}",
            "o",
            "\u{a7}",
            "\u{2022}",
            "\u{f0b7}",
            "-",
            ".",
            "1..",
            ".1.",
            "1.\u{2022}.",
        ] {
            assert!(!marker_is_numbered(m), "{m}");
        }
    }

    #[test]
    fn builder_makes_paragraphs_and_a_square_table() {
        let plain = RunProps::default();
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        b.text("Hello ", &plain);
        b.text("big", &bold);
        b.text(" world", &plain);
        b.end_para(ParProps::default(), false);
        b.text("a", &plain);
        b.end_cell(ParProps::default());
        b.text("b", &plain);
        b.end_cell(ParProps::default());
        b.end_row(ParProps::default());
        b.text("c", &plain);
        b.end_cell(ParProps::default());
        b.end_row(ParProps::default());
        b.text("after", &plain);
        b.end_para(ParProps::default(), false);
        let doc = b.finish(ParProps::default());
        assert_eq!(
            paragraph_texts(&doc),
            ["Hello big world", "a", "b", "c", "", "after"]
        );
        let Block::Table(t) = &doc.body[1] else {
            panic!("{:?}", doc.body[1]);
        };
        assert_eq!(t.rows.len(), 2);
        assert!(t.rows.iter().all(|r| r.cells.len() == 2));
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        assert_eq!(p.content.len(), 3, "same-props text joins one run");
    }

    /// FIX r3: with no room left, inflate must not read a cap of 0 as none;
    /// and what it hands back past the room is no more than the room.
    #[test]
    fn inflate_within_never_runs_uncapped() {
        // A stored DEFLATE block of 2 MiB.
        let data = vec![b'a'; 2 << 20];
        let mut body = Vec::new();
        let chunks: Vec<&[u8]> = data.chunks(65535).collect();
        for (i, c) in chunks.iter().enumerate() {
            body.push(u8::from(i + 1 == chunks.len()));
            let len = c.len() as u16;
            body.extend(len.to_le_bytes());
            body.extend((!len).to_le_bytes());
            body.extend_from_slice(c);
        }
        let budget = Budget::new(0, MAX_WORK);
        assert!(inflate_within(&body, &budget).unwrap_err().is_empty());
        assert!(budget.exhausted());
        let budget = Budget::new(1 << 20, MAX_WORK);
        let over = inflate_within(&body, &budget).unwrap_err();
        assert!(
            over.len() <= 1 << 20 && over.capacity() <= 4 << 20,
            "{}",
            over.capacity()
        );
    }

    /// FIX r3: a list marker followed by many breaks is taken off in one
    /// drain, not one shift of the whole paragraph per break.
    #[test]
    fn a_marker_before_many_breaks_is_taken_in_one_pass() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let plain = RunProps::default();
        b.text("1.", &plain);
        for _ in 0..200_000 {
            b.line_break(&plain);
        }
        b.text("Item", &plain);
        assert_eq!(b.take_leading_marker().as_deref(), Some("1."));
        assert_eq!(b.para_text(), "Item");
    }

    /// FIX r3: a full row's pending paragraphs (no cell end before the
    /// table closes) join its last cell instead of vanishing.
    #[test]
    fn a_full_rows_pending_paragraphs_join_its_last_cell() {
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        let plain = RunProps::default();
        for _ in 0..MAX_COLUMNS {
            b.text("c", &plain);
            b.end_cell(ParProps::default());
        }
        b.text("tail", &plain);
        b.end_para(ParProps::default(), true);
        b.text("after", &plain);
        b.end_para(ParProps::default(), false);
        let doc = b.finish(ParProps::default());
        let texts = paragraph_texts(&doc);
        assert!(texts.contains(&"tail".to_string()), "{texts:?}");
        let Block::Table(t) = &doc.body[0] else {
            panic!()
        };
        assert!(t.rows.iter().all(|r| r.cells.len() == MAX_COLUMNS));
    }

    /// FIX r3: what an import builds is charged as memory, so many small
    /// pieces fail against the memory allowance even with work to spare.
    #[test]
    fn built_memory_is_charged() {
        let budget = Budget::with_built(MAX_DECODED, usize::MAX, 1 << 16);
        let mut b = Builder::new(&budget);
        let plain = RunProps::default();
        for _ in 0..10_000 {
            b.tab(&plain);
        }
        assert!(budget.exhausted());
    }
}
