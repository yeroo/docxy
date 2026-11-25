//! `<img>` in an HTML import (#1880): where a picture's bytes come from
//! (a `data:` URI decoded here, any other `src` through the caller's
//! [`ImageResolver`]), the media an import keeps, a picture's size and the
//! sized placeholder of one that does not resolve.
//!
//! Everything decoded or resolved is charged to the import's `Budget` as
//! decoded bytes, within [`MAX_IMAGE_BYTES`] an image and
//! [`MAX_MEDIA_BYTES`] in all; past either an image is a placeholder and
//! the import goes on. The resolver is called at most [`MAX_IMAGES`] times,
//! once per distinct `src`, and is told how many bytes it may return.

use super::Budget;
use super::html_table::BODY_TWIPS;
use crate::image::{self, ImageInfo};
use std::collections::HashMap;

/// What an `<img src>` other than `data:` resolves to, given the src as the
/// page writes it (character references decoded, trimmed) and the most
/// bytes the import will take: the image's bytes, or `None` (refused,
/// missing) for a placeholder. Called once per distinct src. A longer
/// answer is dropped, as is one whose bytes are not PNG, JPEG, GIF or BMP.
pub type ImageResolver<'r> = dyn FnMut(&str, usize) -> Option<Vec<u8>> + 'r;

/// What a caller's [`ImageConverter`] is asked to turn into PNG (#1898).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvertKind {
    /// An SVG document (`<svg` after an optional XML prolog, comments and
    /// doctype).
    Svg,
    /// A WebP image (`RIFF` … `WEBP`).
    Webp,
}

/// A picture a caller's [`ImageConverter`] made (#1898): PNG bytes and the
/// size in CSS pixels the picture takes when the page gives none (for an
/// SVG, its intrinsic size, whatever the PNG's pixels).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Turns an image the import cannot embed as it is (SVG, WebP) into PNG
/// (#1898), given its kind, its bytes and the most PNG bytes the import
/// will take; `None` leaves it a placeholder. A longer answer, or one that
/// is not a PNG of a nonzero size, is dropped. Called once per distinct
/// image.
pub type ImageConverter<'r> = dyn FnMut(ConvertKind, &[u8], usize) -> Option<ConvertedImage> + 'r;

/// The most distinct srcs an import asks its resolver for; later ones are
/// placeholders.
pub const MAX_IMAGES: usize = 1000;
/// The most distinct URLs an import's pictures link to (#1999); a picture
/// linked to a later one is a plain picture.
pub const MAX_PICTURE_LINKS: usize = 1000;
/// The most bytes one image may hold.
pub const MAX_IMAGE_BYTES: usize = 64 << 20;
/// The most media bytes one import keeps (each distinct image once).
pub const MAX_MEDIA_BYTES: usize = 128 << 20;
/// The longest Alt Text kept, in characters.
pub(crate) const MAX_ALT: usize = 1024;
/// The longest picture name kept (the src's last piece), in characters.
const MAX_NAME: usize = 64;

/// The placeholder's colours: a light grey fill (the PNG) and a grey line.
/// By estimate: there is no Word oracle for a missing HTML image.
pub(crate) const PLACEHOLDER_OUTLINE: &str = "A6A6A6";

/// A 1 x 1 RGB PNG of `#F2F2F2`, stretched over a placeholder's box.
pub(crate) const PLACEHOLDER_PNG: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xf4, 0xe9, 0x13,
    0x00, 0x05, 0xb0, 0x02, 0xd7, 0x22, 0xae, 0xdd, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// One medium an import made: the import-local rid its pictures name (see
/// [`crate::import::HtmlImport::package`]), its bytes and what they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedMedia {
    pub rid: String,
    pub bytes: Vec<u8>,
    pub info: ImageInfo,
}

/// The import-local rid of medium `index`.
fn local_rid(index: usize) -> String {
    format!("rIdHtmlImage{}", index + 1)
}

/// One picture link an import made (#1999): the import-local rid a linked
/// picture's `a:hlinkClick` names (see
/// [`crate::import::HtmlImport::package`]) and the URL it goes to. One per
/// distinct URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedLink {
    pub rid: String,
    pub url: String,
}

/// What one `<img>` came to.
pub(crate) enum Source {
    /// A medium of the import: its rid, its size in CSS pixels, and the rid
    /// of the SVG it was converted from (#1898), if it was.
    Media {
        rid: String,
        width: u32,
        height: u32,
        svg: Option<String>,
    },
    /// Nothing usable: a placeholder, or the alt text.
    Unresolved,
}

/// The images of one import: the resolver, what each src gave, the media.
pub(crate) struct Images<'a> {
    resolve: &'a mut ImageResolver<'a>,
    /// A resolved src's medium index, `None` for one that gave nothing.
    cache: HashMap<String, Option<usize>>,
    calls: usize,
    media: Vec<ImportedMedia>,
    media_bytes: usize,
    placeholder: Option<usize>,
    /// The last `wp:docPr` id given.
    last_id: u32,
    /// The pictures' click links (#1999), each URL once.
    links: Vec<ImportedLink>,
    link_index: HashMap<String, usize>,
    /// The page's base URL (#1898): its `<base href>`, which every src
    /// but a `data:` one is resolved against ([`resolve_ref`]).
    base: Option<String>,
    /// The caller's converter of SVG and WebP pictures (#1898), if any.
    convert: Option<&'a mut ImageConverter<'a>>,
    /// Each medium's size in CSS pixels (its pixels, or a conversion's
    /// size) and the index of the SVG medium it was made from (#1898).
    css: Vec<(u32, u32)>,
    svg_of: Vec<Option<usize>>,
    /// Each image converted so far, its source bytes and the PNG medium it
    /// made (`None`: refused), so equal images convert once (#1898).
    converted: Vec<(Vec<u8>, Option<usize>)>,
}

impl<'a> Images<'a> {
    pub(crate) fn new(resolve: &'a mut ImageResolver<'a>) -> Self {
        Images {
            resolve,
            cache: HashMap::new(),
            calls: 0,
            media: Vec::new(),
            media_bytes: 0,
            placeholder: None,
            last_id: 0,
            links: Vec::new(),
            link_index: HashMap::new(),
            base: None,
            convert: None,
            css: Vec::new(),
            svg_of: Vec::new(),
            converted: Vec::new(),
        }
    }

    /// Turn SVG and WebP pictures into PNG through `convert` (#1898).
    pub(crate) fn set_converter(&mut self, convert: &'a mut ImageConverter<'a>) {
        self.convert = Some(convert);
    }

    /// Resolve every later src against `base` (#1898).
    pub(crate) fn set_base(&mut self, base: Option<String>) {
        self.base = base;
    }

    /// The media and the picture links the import made.
    pub(crate) fn into_parts(self) -> (Vec<ImportedMedia>, Vec<ImportedLink>) {
        (self.media, self.links)
    }

    /// The import-local rid of a picture link to `url` (#1999): one per
    /// distinct URL, made on first use (kept twice: the list and its
    /// index). `None` past [`MAX_PICTURE_LINKS`] URLs or when the budget has
    /// no room.
    pub(crate) fn link_rid(&mut self, url: &str, budget: &Budget) -> Option<String> {
        if let Some(&i) = self.link_index.get(url) {
            return Some(self.links[i].rid.clone());
        }
        if self.links.len() >= MAX_PICTURE_LINKS || !budget.keep::<ImportedLink>(2 * url.len() + 32)
        {
            return None;
        }
        let index = self.links.len();
        let rid = format!("rIdHtmlLink{}", index + 1);
        self.links.push(ImportedLink {
            rid: rid.clone(),
            url: url.to_string(),
        });
        self.link_index.insert(url.to_string(), index);
        Some(rid)
    }

    /// The next drawing object id (1, 2, …).
    pub(crate) fn next_id(&mut self) -> u32 {
        self.last_id = self.last_id.saturating_add(1);
        self.last_id
    }

    /// The most bytes the next image may hold.
    fn limit(&self, budget: &Budget) -> usize {
        budget
            .room()
            .min(MAX_IMAGE_BYTES)
            .min(MAX_MEDIA_BYTES.saturating_sub(self.media_bytes))
    }

    /// Where `src` (trimmed, references decoded) takes its picture from.
    pub(crate) fn source(&mut self, src: &str, budget: &Budget) -> Source {
        if src.is_empty() {
            return Source::Unresolved;
        }
        let resolved;
        let src = match self.base.as_deref() {
            Some(base) if !has_data_scheme(src) => {
                resolved = resolve_ref(base, src);
                resolved.as_str()
            }
            _ => src,
        };
        let index = if has_data_scheme(src) {
            let limit = self.limit(budget);
            decode_data_uri(src, limit, budget).and_then(|bytes| self.add(bytes, budget))
        } else if let Some(&hit) = self.cache.get(src) {
            hit
        } else {
            let limit = self.limit(budget);
            let index = if limit == 0 || self.calls >= MAX_IMAGES {
                None
            } else {
                self.calls += 1;
                (self.resolve)(src, limit)
                    .filter(|b| b.len() <= limit && budget.take_bytes(b.len()))
                    .and_then(|bytes| self.add(bytes, budget))
            };
            if budget.alloc(src.len() + std::mem::size_of::<Option<usize>>()) {
                self.cache.insert(src.to_string(), index);
            }
            index
        };
        match index {
            Some(i) => {
                let (width, height) = self.css[i];
                Source::Media {
                    rid: self.media[i].rid.clone(),
                    width,
                    height,
                    svg: self.svg_of[i].map(|s| self.media[s].rid.clone()),
                }
            }
            None => Source::Unresolved,
        }
    }

    /// Push a medium (its bytes already charged as decoded and within
    /// [`MAX_MEDIA_BYTES`]): its index.
    fn push(&mut self, bytes: Vec<u8>, info: ImageInfo, css: (u32, u32)) -> usize {
        let index = self.media.len();
        self.media_bytes += bytes.len();
        self.media.push(ImportedMedia {
            rid: local_rid(index),
            bytes,
            info,
        });
        self.css.push(css);
        self.svg_of.push(None);
        index
    }

    /// `bytes`, an SVG or WebP, as a PNG medium made by the caller's
    /// converter (#1898), an SVG keeping its own medium beside it: the PNG
    /// medium's index. `None` without a converter, or when it refuses.
    fn add_converted(&mut self, bytes: Vec<u8>, budget: &Budget) -> Option<usize> {
        let kind = convertible(&bytes)?;
        for (src, made) in &self.converted {
            if src.len() == bytes.len() {
                if !budget.scanned(bytes.len()) {
                    return None;
                }
                if *src == bytes {
                    return *made;
                }
            }
        }
        let limit = self.limit(budget);
        let convert = self.convert.as_mut()?;
        let made = convert(kind, &bytes, limit)
            .filter(|c| c.width > 0 && c.height > 0 && c.png.len() <= limit)
            .and_then(|c| {
                let info = image::sniff(&c.png).filter(|i| i.ext == "png")?;
                Some((c, info))
            });
        let made = made.and_then(|(c, info)| {
            let svg = kind == ConvertKind::Svg;
            let kept = c.png.len() + if svg { bytes.len() } else { 0 };
            if kept > MAX_MEDIA_BYTES.saturating_sub(self.media_bytes)
                || !budget.take_bytes(c.png.len())
                || !budget.keep::<ImportedMedia>(32)
            {
                return None;
            }
            let css = (c.width, c.height);
            let png = self.push(c.png, info, css);
            if svg {
                let svg_info = ImageInfo {
                    ext: "svg",
                    content_type: "image/svg+xml",
                    width: c.width,
                    height: c.height,
                };
                let at = self.push(bytes.clone(), svg_info, css);
                self.svg_of[png] = Some(at);
            }
            Some(png)
        });
        if budget.alloc(bytes.len() + 32) {
            self.converted.push((bytes, made));
        }
        made
    }

    /// Keep `bytes` (already charged as decoded) when they are an image:
    /// the medium holding the same bytes when there is one.
    fn add(&mut self, bytes: Vec<u8>, budget: &Budget) -> Option<usize> {
        let Some(info) = image::sniff(&bytes) else {
            return self.add_converted(bytes, budget);
        };
        for (i, m) in self.media.iter().enumerate() {
            if m.bytes.len() == bytes.len() {
                if !budget.scanned(bytes.len()) {
                    return None;
                }
                if m.bytes == bytes {
                    return Some(i);
                }
            }
        }
        if bytes.len() > MAX_MEDIA_BYTES.saturating_sub(self.media_bytes)
            || !budget.keep::<ImportedMedia>(16)
        {
            return None;
        }
        Some(self.push(bytes, info, (info.width, info.height)))
    }

    /// The rid of the shared placeholder medium, made on first use (its
    /// bytes counted once).
    pub(crate) fn placeholder_rid(&mut self, budget: &Budget) -> Option<String> {
        if let Some(i) = self.placeholder {
            return Some(self.media[i].rid.clone());
        }
        let info = image::sniff(&PLACEHOLDER_PNG)?;
        if !budget.keep::<ImportedMedia>(PLACEHOLDER_PNG.len() + 16) {
            return None;
        }
        let index = self.push(PLACEHOLDER_PNG.to_vec(), info, (info.width, info.height));
        self.placeholder = Some(index);
        Some(self.media[index].rid.clone())
    }
}

/// The most `<source>`s of one `<picture>` kept (#1898).
pub(crate) const MAX_SOURCES: usize = 16;
/// The most `srcset` candidates one `<img>` tries (#1898).
pub(crate) const MAX_CANDIDATES: usize = 16;

/// Whether a `<source type>` names an image the import may use (#1898):
/// PNG, JPEG, GIF, BMP, or (through a caller's converter) WebP and SVG;
/// parameters and case ignored.
pub(crate) fn usable_type(mime: &str) -> bool {
    let t = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        t.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/gif"
            | "image/bmp"
            | "image/webp"
            | "image/svg+xml"
    )
}

/// A `srcset`'s candidates (#1898), each URL with its density, in the order
/// an import tries them: those with no descriptor or `1x` first, then the
/// rest as written. A density descriptor `Nx` (N > 0) is N; a width one
/// (`Nw`) is 1, as the `sizes` it needs is not read; a candidate with a
/// descriptor that is neither is dropped. Parsed as the HTML standard's
/// srcset parser splits it: a URL runs to white space (so a `data:` URI's
/// commas are its own), a trailing comma ends a candidate with no
/// descriptors, and descriptors run to a comma outside parentheses.
pub(crate) fn srcset_candidates(srcset: &str) -> Vec<(String, f64)> {
    let mut first = Vec::new();
    let mut rest = Vec::new();
    let mut s = srcset;
    loop {
        s = s.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if s.is_empty() {
            break;
        }
        let end = s.find(|c: char| c.is_ascii_whitespace()).unwrap_or(s.len());
        let raw_url = &s[..end];
        s = &s[end..];
        let url = raw_url.trim_end_matches(',');
        let descriptors = if url.len() < raw_url.len() {
            ""
        } else {
            // To the first comma outside parentheses.
            let mut depth = 0usize;
            let mut stop = s.len();
            for (i, c) in s.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => {
                        stop = i;
                        break;
                    }
                    _ => {}
                }
            }
            let d = &s[..stop];
            s = &s[stop..];
            d
        };
        let words: Vec<&str> = descriptors.split_ascii_whitespace().collect();
        let density = match words.as_slice() {
            [] => Some(1.0),
            [d] => {
                let d = d.to_ascii_lowercase();
                if let Some(x) = d.strip_suffix('x') {
                    x.parse::<f64>().ok().filter(|x| x.is_finite() && *x > 0.0)
                } else if let Some(w) = d.strip_suffix('w') {
                    w.parse::<u32>().ok().filter(|&w| w > 0).map(|_| 1.0)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(density) = density.filter(|_| !url.is_empty()) {
            let one = descriptors.trim().is_empty()
                || (density == 1.0 && !descriptors.contains(['w', 'W']));
            if one {
                first.push((url.to_string(), density));
            } else {
                rest.push((url.to_string(), density));
            }
        }
    }
    first.extend(rest);
    first
}

/// The longest `<base href>` kept (#1898), in bytes: a longer one is none.
const MAX_BASE: usize = 4096;

/// A `<base href>` as the page's base URL (#1898): trimmed, references
/// already decoded; none when empty, too long, or a `data:` or
/// `javascript:` URL (a browser takes neither as a base).
pub(crate) fn page_base(href: &str) -> Option<String> {
    let href = href.trim();
    let scheme = scheme_of(href).map(str::to_ascii_lowercase);
    (!href.is_empty()
        && href.len() <= MAX_BASE
        && !matches!(scheme.as_deref(), Some("data" | "javascript")))
    .then(|| href.to_string())
}

/// `s`'s URI scheme (RFC 3986: a letter, then letters, digits, `+`, `-`,
/// `.`, before a `:`), if it has one.
pub(crate) fn scheme_of(s: &str) -> Option<&str> {
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    let mut bytes = scheme.bytes();
    let first = bytes.next()?;
    (first.is_ascii_alphabetic()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))
    .then_some(scheme)
}

/// `reference` resolved against `base` (#1898), as RFC 3986 section 5.2
/// does: one with a scheme is itself; `//host…` takes the base's scheme;
/// `/path` the base's scheme and authority; `?query` and `#fragment` the
/// base's path; any other path is merged with the base's directory, its
/// `.` and `..` segments removed. A relative base (no scheme, such as
/// `img/`) gives a relative result, which keeps a leading `..` it cannot
/// remove (the caller decides whether that leaves its folder).
pub(crate) fn resolve_ref(base: &str, reference: &str) -> String {
    if scheme_of(reference).is_some() {
        return reference.to_string();
    }
    let base = base.split('#').next().unwrap_or("");
    let (scheme, rest) = match scheme_of(base) {
        Some(s) => (Some(s), &base[s.len() + 1..]),
        None => (None, base),
    };
    let (authority, rest) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?']).unwrap_or(r.len());
            (Some(&r[..end]), &r[end..])
        }
        None => (None, rest),
    };
    let (base_path, base_query) = match rest.find('?') {
        Some(q) => (&rest[..q], Some(&rest[q..])),
        None => (rest, None),
    };
    let mut out = String::new();
    if let Some(s) = scheme {
        out.push_str(s);
        out.push(':');
    }
    if reference.starts_with("//") {
        out.push_str(reference);
        return out;
    }
    if let Some(a) = authority {
        out.push_str("//");
        out.push_str(a);
    }
    if reference.is_empty() || reference.starts_with('#') {
        out.push_str(base_path);
        out.push_str(base_query.unwrap_or(""));
        out.push_str(reference);
        return out;
    }
    if reference.starts_with('?') {
        out.push_str(base_path);
        out.push_str(reference);
        return out;
    }
    let path_end = reference.find(['?', '#']).unwrap_or(reference.len());
    let (path, tail) = reference.split_at(path_end);
    let merged = if path.starts_with('/') {
        path.to_string()
    } else if authority.is_some() && base_path.is_empty() {
        format!("/{path}")
    } else {
        let dir = base_path.rfind('/').map_or("", |i| &base_path[..=i]);
        format!("{dir}{path}")
    };
    out.push_str(&remove_dot_segments(&merged));
    out.push_str(tail);
    out
}

/// `path` without its `.` and `..` segments (RFC 3986 section 5.2.4); a
/// relative path keeps a leading `..` that has nothing to remove.
fn remove_dot_segments(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len() - 1;
    for (i, seg) in segments.iter().enumerate() {
        let dir_end = i == last;
        match *seg {
            "." => {
                if dir_end {
                    out.push("");
                }
            }
            ".." => {
                if out.last().is_some_and(|s| !s.is_empty() && *s != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
                if dir_end {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut joined = out.join("/");
    if absolute && !joined.starts_with('/') {
        joined.insert(0, '/');
    }
    joined
}

/// What a caller's converter could make of `bytes` (#1898): an SVG
/// document (`<svg` after an optional byte-order mark, white space, XML
/// declaration, processing instructions, comments and doctype, within the
/// first 4 KiB) or a WebP (`RIFF`, a size, `WEBP`).
pub(crate) fn convertible(bytes: &[u8]) -> Option<ConvertKind> {
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ConvertKind::Webp);
    }
    let head = &bytes[..bytes.len().min(4096)];
    let mut s = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    loop {
        s = s.trim_ascii_start();
        let skip = |s: &[u8], open: &[u8], close: &[u8]| -> Option<usize> {
            s.starts_with(open).then(|| {
                super::find(&s[open.len()..], close).map(|i| open.len() + i + close.len())
            })?
        };
        if let Some(n) = skip(s, b"<?", b"?>")
            .or_else(|| skip(s, b"<!--", b"-->"))
            .or_else(|| {
                (s.len() >= 9 && s[..9].eq_ignore_ascii_case(b"<!doctype"))
                    .then(|| doctype_end(s))
                    .flatten()
            })
        {
            s = &s[n..];
            continue;
        }
        break;
    }
    (s.starts_with(b"<svg")
        && s.get(4)
            .is_some_and(|&b| b.is_ascii_whitespace() || b == b'>' || b == b'/'))
    .then_some(ConvertKind::Svg)
}

/// The length of a `<!DOCTYPE …>` at the start of `s`, its internal subset
/// (`[…]`) included.
fn doctype_end(s: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in s.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => return Some(i + 1),
            _ => {}
        }
    }
    None
}

/// Whether `src` is a `data:` URI (scheme case-insensitive).
pub(crate) fn has_data_scheme(src: &str) -> bool {
    src.get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
}

/// The bytes of a `data:[<mediatype>][;base64],<data>` URI, at most `limit`
/// of them; `None` when it is malformed or larger. The data is
/// percent-decoded, then base64-decoded when the header ends in `;base64`
/// (ASCII white space between the characters is skipped, as mail wraps it).
/// Scanning is charged as work, the bytes as decoded.
pub(crate) fn decode_data_uri(src: &str, limit: usize, budget: &Budget) -> Option<Vec<u8>> {
    if !budget.scanned(src.len()) {
        return None;
    }
    let rest = src.get(5..)?;
    let comma = rest.find(',')?;
    let header = rest[..comma].trim();
    let data = &rest[comma + 1..];
    let base64 = header
        .rsplit(';')
        .next()
        .is_some_and(|p| p.trim().eq_ignore_ascii_case("base64"))
        && header.contains(';');
    if decoded_bound(data, base64) > limit {
        return None;
    }
    let raw = percent_decode(data);
    let bytes = if base64 { base64_decode(&raw)? } else { raw };
    (bytes.len() <= limit && budget.take_bytes(bytes.len())).then_some(bytes)
}

/// The most bytes `data` decodes to: its bytes once percent escapes are
/// decoded, base64's white space left out, three bytes made of four.
/// One counting pass, so a too large URI is refused before anything is
/// made.
fn decoded_bound(data: &str, base64: bool) -> usize {
    let b = data.as_bytes();
    let (mut n, mut i) = (0usize, 0);
    while i < b.len() {
        let c = match escape_at(b, i) {
            Some(c) => {
                i += 3;
                c
            }
            None => {
                i += 1;
                b[i - 1]
            }
        };
        if !(base64 && c.is_ascii_whitespace()) {
            n += 1;
        }
    }
    if base64 { (n * 3).div_ceil(4) } else { n }
}

/// The byte a `%XX` escape at `b[i]` stands for.
fn escape_at(b: &[u8], i: usize) -> Option<u8> {
    if b[i] != b'%' || i + 2 >= b.len() {
        return None;
    }
    let hex = |c: u8| (c as char).to_digit(16);
    Some((hex(b[i + 1])? * 16 + hex(b[i + 2])?) as u8)
}

/// `%XX` escapes decoded; a `%` not followed by two hex digits is kept.
pub(crate) fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match escape_at(b, i) {
            Some(c) => {
                out.push(c);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    out
}

/// Standard base64 (`+/`, also the URL-safe `-_`), ASCII white space
/// skipped, `=` padding only at the end; `None` for any other character.
pub(crate) fn base64_decode(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut padded = false;
    for &c in b {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => {
                padded = true;
                continue;
            }
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        if padded {
            return None;
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A picture's box in twips from its declared width and height (`None`
/// unset) and, for an image that resolved, its pixel size: both declared,
/// those; one, the other by the image's aspect (a placeholder: square);
/// none, its pixels (a placeholder: no box). This is the box the page
/// declares, before [`fit_to_body`], in which a CSS radius is measured
/// (#1926). In u128: a
/// side may be any u32 (an `<img>`'s is not capped at a page) and the
/// pixels any u32, so `w * ph` and then `h * body` can pass u64 (#1880
/// r2b).
pub(crate) fn declared_box(
    width: Option<u32>,
    height: Option<u32>,
    pixels: Option<(u32, u32)>,
) -> Option<(u128, u128)> {
    Some(match (width, height, pixels) {
        (Some(w), Some(h), _) => (w.into(), h.into()),
        (Some(w), None, Some((pw, ph))) => {
            (w.into(), u128::from(w) * u128::from(ph) / u128::from(pw))
        }
        (None, Some(h), Some((pw, ph))) => {
            (u128::from(h) * u128::from(pw) / u128::from(ph), h.into())
        }
        (Some(s), None, None) | (None, Some(s), None) => (s.into(), s.into()),
        (None, None, Some((pw, ph))) => (u128::from(pw) * 15, u128::from(ph) * 15),
        (None, None, None) => return None,
    })
}

/// An image's `max-width` and `max-height` in twips (#1898), resolved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MaxBox {
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
}

impl MaxBox {
    /// The declared box `(w, h)` within these bounds, as a browser
    /// clamps a replaced element: a side over its bound takes the bound,
    /// and the other side scales with it when it was not declared
    /// (`width` / `height`, the declared sides) but came from the aspect
    /// (or a placeholder's square); a declared side stays as it is.
    pub(crate) fn clamp(
        self,
        (mut w, mut h): (u128, u128),
        width: Option<u32>,
        height: Option<u32>,
    ) -> (u128, u128) {
        if let Some(m) = self.width.map(u128::from).filter(|&m| w > m) {
            if height.is_none() {
                h = h * m / w;
            }
            w = m;
        }
        if let Some(m) = self.height.map(u128::from).filter(|&m| h > m) {
            if width.is_none() {
                w = w * m / h;
            }
            h = m;
        }
        (w, h)
    }
}

/// A declared box wider than the body scaled down to the body's width,
/// keeping its aspect.
pub(crate) fn fit_to_body((w, h): (u128, u128)) -> (u64, u64) {
    let body = u128::from(BODY_TWIPS);
    let (w, h) = if w > body {
        (body, h * body / w)
    } else {
        (w, h)
    };
    let side = |t: u128| u64::try_from(t).unwrap_or(u64::MAX);
    (side(w), side(h))
}

/// An `<img>`'s computed `border-radius` (#1926): one radius for all four
/// corners (horizontal radii; a vertical part is dropped), or `NonUniform`
/// when the corners differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Radius {
    /// A percentage of the box's sides.
    Pct(f64),
    /// A length, in twips against the element's font.
    Twips(u64),
    NonUniform,
}

/// The crop shape a picture of declared box `(w, h)` (twips, before
/// [`fit_to_body`]: the fit is uniform, so the ratio holds) gets from its
/// `radius` (#1926), as Word's Crop to Shape writes it:
///
/// - a percentage of 50 or more: the box's ellipse; under 50, a
///   `roundRect` whose circular corner is the browser's elliptical corner's
///   short radius, `p% * min(w, h)` (`adj` is `p * 1000`);
/// - a length: a square box's circle once it reaches half the side (a
///   browser clamps it there), else a `roundRect` of `r / min(w, h)`,
///   clamped at half the shorter side (a browser's stadium);
/// - none, zero, corners that differ, or an empty box: a rectangle.
pub(crate) fn shape(radius: Option<Radius>, (w, h): (u128, u128)) -> image::PictureShape {
    use image::{PictureShape, ROUND_RECT_MAX_ADJ};
    let round = |adj: f64| {
        let adj = adj.round().min(f64::from(ROUND_RECT_MAX_ADJ));
        if adj >= 1.0 {
            PictureShape::RoundRect { adj: adj as u32 }
        } else {
            PictureShape::Rect
        }
    };
    let short = w.min(h);
    match radius {
        None | Some(Radius::NonUniform) => PictureShape::Rect,
        _ if short == 0 => PictureShape::Rect,
        Some(Radius::Pct(p)) if p >= 50.0 => PictureShape::Ellipse,
        Some(Radius::Pct(p)) if p > 0.0 => round(p * 1000.0),
        Some(Radius::Pct(_)) | Some(Radius::Twips(0)) => PictureShape::Rect,
        Some(Radius::Twips(r)) => {
            let r = u128::from(r);
            if w == h && 2 * r >= w {
                PictureShape::Ellipse
            } else {
                // `r / short` as a fraction of 100000, in f64 only once it
                // is at most a few times the clamp.
                round(100_000.0 * (r.min(short) as f64) / short as f64)
            }
        }
    }
}

/// A box in twips as DrawingML's EMUs (635 a twip), each side within
/// 1..=[`image::MAX_EMU`].
pub(crate) fn twips_emu((w, h): (u64, u64)) -> (i64, i64) {
    let emu = |t: u64| (i128::from(t) * 635).clamp(1, i128::from(image::MAX_EMU)) as i64;
    (emu(w), emu(h))
}

/// A picture's name: the last piece of its src's path (no query or
/// fragment, no `cid:` style scheme), at most [`MAX_NAME`] characters; a
/// `data:` src, or one that leaves nothing, is `Picture <id>`.
pub(crate) fn picture_name(src: &str, id: u32) -> String {
    let fallback = || format!("Picture {id}");
    if has_data_scheme(src) {
        return fallback();
    }
    let path = src.split(['?', '#']).next().unwrap_or("");
    let piece = path.rsplit(['/', '\\']).next().unwrap_or("");
    let piece = match piece.split_once(':') {
        Some((scheme, rest))
            if scheme.len() >= 2 && scheme.bytes().all(|b| b.is_ascii_alphabetic()) =>
        {
            rest
        }
        _ => piece,
    };
    let name: String = piece
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect();
    if name.trim().is_empty() {
        fallback()
    } else {
        name
    }
}

/// Alt Text as kept: at most [`MAX_ALT`] characters, trimmed.
pub(crate) fn alt_text(alt: &str) -> String {
    alt.trim().chars().take(MAX_ALT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget::standard()
    }

    #[test]
    fn base64_takes_padding_and_wrapping_and_refuses_junk() {
        assert_eq!(base64_decode(b"TWFu").unwrap(), b"Man");
        assert_eq!(base64_decode(b"TWE=").unwrap(), b"Ma");
        assert_eq!(base64_decode(b"TQ==").unwrap(), b"M");
        assert_eq!(base64_decode(b"TQ").unwrap(), b"M");
        assert_eq!(base64_decode(b"TW\r\n Fu\tTWE=").unwrap(), b"ManMa");
        assert_eq!(base64_decode(b"-_8=").unwrap(), [0xfb, 0xff]);
        assert!(base64_decode(b"TW*u").is_none());
        assert!(base64_decode(b"TQ==TQ==").is_none(), "data after padding");
    }

    #[test]
    fn data_uris_decode_base64_and_percent_escapes() {
        let b = budget();
        let d = |s: &str| decode_data_uri(s, 1 << 20, &b);
        assert_eq!(d("data:image/png;base64,TWFu").unwrap(), b"Man");
        assert_eq!(d("DATA:image/png;BASE64,TW%46u").unwrap(), b"Man");
        assert_eq!(d("data:;base64,TWFu").unwrap(), b"Man");
        assert_eq!(d("data:text/plain,a%20b%2").unwrap(), b"a b%2");
        assert_eq!(d("data:,x").unwrap(), b"x");
        // `base64` must be the header's last parameter.
        assert_eq!(d("data:base64,TWFu").unwrap(), b"TWFu");
        assert!(d("data:image/png;base64").is_none(), "no comma");
        assert!(d("data:image/png;base64,TW!u").is_none());
    }

    #[test]
    fn a_data_uri_over_its_limit_is_not_decoded() {
        let b = budget();
        assert!(decode_data_uri("data:;base64,TWFuTWFu", 5, &b).is_none());
        assert_eq!(
            decode_data_uri("data:;base64,TWFuTWFu", 6, &b).unwrap(),
            b"ManMan"
        );
        // Wrapped: the length bound is over the limit, the size is not.
        let wrapped = format!("data:;base64,{}", "TWFu\r\n".repeat(4));
        assert_eq!(decode_data_uri(&wrapped, 12, &b).unwrap(), b"ManManManMan");
        assert!(decode_data_uri("data:,abcdef", 5, &b).is_none());
        // Charged as decoded bytes.
        let small = Budget::new(3, 1000);
        assert!(decode_data_uri("data:;base64,TWFu", small.room(), &small).is_some());
        assert_eq!(small.room(), 0);
        assert!(!small.exhausted());
    }

    /// The picture's box as the import writes it.
    fn picture_twips(
        width: Option<u32>,
        height: Option<u32>,
        pixels: Option<(u32, u32)>,
    ) -> Option<(u64, u64)> {
        declared_box(width, height, pixels).map(fit_to_body)
    }

    #[test]
    fn sizes_follow_the_declared_sides_and_the_aspect() {
        let px = Some((200, 100));
        // Both declared win over the pixels.
        assert_eq!(
            picture_twips(Some(1500), Some(3000), px),
            Some((1500, 3000))
        );
        assert_eq!(picture_twips(Some(1500), None, px), Some((1500, 750)));
        assert_eq!(picture_twips(None, Some(750), px), Some((1500, 750)));
        assert_eq!(picture_twips(None, None, px), Some((3000, 1500)));
        // A placeholder: square of one side, no box without one.
        assert_eq!(picture_twips(Some(600), None, None), Some((600, 600)));
        assert_eq!(picture_twips(None, Some(600), None), Some((600, 600)));
        assert_eq!(picture_twips(None, None, None), None);
        // Capped at the body width, aspect kept, placeholders too.
        assert_eq!(
            picture_twips(None, None, Some((1572, 100))),
            Some((11_790, 750))
        );
        assert_eq!(
            picture_twips(Some(23_580), Some(600), None),
            Some((11_790, 300))
        );
        assert_eq!(twips_emu((15, 30)), (9525, 19_050));
        assert_eq!(twips_emu((0, 1)), (1, 635));
    }

    #[test]
    fn a_radius_maps_to_a_crop_shape_against_the_declared_box() {
        use crate::image::PictureShape::{Ellipse, Rect, RoundRect};
        let px = |n: u128| n * 15;
        let avatar = (px(32), px(32));
        let card = (px(120), px(40));
        // Percentages: 50 or more is the ellipse, square or not.
        assert_eq!(shape(Some(Radius::Pct(50.0)), avatar), Ellipse);
        assert_eq!(shape(Some(Radius::Pct(80.0)), card), Ellipse);
        // Under 50: the short radius, whatever the box.
        assert_eq!(
            shape(Some(Radius::Pct(10.0)), card),
            RoundRect { adj: 10_000 }
        );
        assert_eq!(
            shape(Some(Radius::Pct(25.0)), avatar),
            RoundRect { adj: 25_000 }
        );
        // Lengths, against the shorter side.
        assert_eq!(
            shape(Some(Radius::Twips(px(8) as u64)), card),
            RoundRect { adj: 20_000 }
        );
        assert_eq!(shape(Some(Radius::Twips(px(16) as u64)), avatar), Ellipse);
        assert_eq!(shape(Some(Radius::Twips(px(99) as u64)), avatar), Ellipse);
        assert_eq!(
            shape(Some(Radius::Twips(px(15) as u64)), avatar),
            RoundRect { adj: 46_875 }
        );
        // Past half a non-square box's short side: the stadium.
        assert_eq!(
            shape(Some(Radius::Twips(px(30) as u64)), card),
            RoundRect { adj: 50_000 }
        );
        assert_eq!(
            shape(Some(Radius::Twips(u64::MAX)), card),
            RoundRect { adj: 50_000 }
        );
        // The ratio is the box's, so a box later fitted to the body keeps
        // it: 8px on 1200x400 px is 2000 in 100000.
        assert_eq!(
            shape(Some(Radius::Twips(px(8) as u64)), (px(1200), px(400))),
            RoundRect { adj: 2000 }
        );
        // Nothing to round.
        for r in [
            None,
            Some(Radius::NonUniform),
            Some(Radius::Pct(0.0)),
            Some(Radius::Twips(0)),
            Some(Radius::Pct(0.0001)),
        ] {
            assert_eq!(shape(r, card), Rect, "{r:?}");
        }
        assert_eq!(shape(Some(Radius::Pct(50.0)), (0, px(4))), Rect);
    }

    #[test]
    fn names_come_from_the_srcs_last_piece() {
        assert_eq!(picture_name("page_files/image001.png", 1), "image001.png");
        assert_eq!(picture_name("a\\b\\c.gif?x=1#y", 1), "c.gif");
        assert_eq!(picture_name("cid:logo.png@01D9", 2), "logo.png@01D9");
        assert_eq!(picture_name("data:image/png;base64,AAAA", 3), "Picture 3");
        assert_eq!(picture_name("http://x/", 4), "Picture 4");
        assert_eq!(picture_name(&"n".repeat(100), 1).len(), 64);
    }

    #[test]
    fn the_placeholder_png_is_one_grey_pixel() {
        let info = image::sniff(&PLACEHOLDER_PNG).unwrap();
        assert_eq!((info.ext, info.width, info.height), ("png", 1, 1));
        // Every chunk's CRC checks.
        let mut at = 8;
        let mut idat = Vec::new();
        while at < PLACEHOLDER_PNG.len() {
            let len = u32::from_be_bytes(PLACEHOLDER_PNG[at..at + 4].try_into().unwrap()) as usize;
            let body = &PLACEHOLDER_PNG[at + 4..at + 8 + len];
            let crc = u32::from_be_bytes(
                PLACEHOLDER_PNG[at + 8 + len..at + 12 + len]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(opccore::zipwrite::crc32(body), crc);
            if &body[..4] == b"IDAT" {
                idat = body[4..].to_vec();
            }
            at += 12 + len;
        }
        assert_eq!(at, PLACEHOLDER_PNG.len());
        // A zlib stream: its deflate body is one filter byte and the RGB.
        assert_eq!(
            opccore::inflate::inflate_partial(&idat[2..], 16),
            [0x00, 0xf2, 0xf2, 0xf2]
        );
    }

    #[test]
    fn the_resolver_is_asked_once_per_src_and_at_most_max_images_times() {
        let png = PLACEHOLDER_PNG.to_vec();
        let mut asked: Vec<(String, usize)> = Vec::new();
        let mut resolve = |src: &str, limit: usize| {
            asked.push((src.to_string(), limit));
            (src != "no").then(|| png.clone())
        };
        let b = budget();
        let mut images = Images::new(&mut resolve);
        assert!(matches!(
            images.source("a", &b),
            Source::Media {
                width: 1,
                height: 1,
                ..
            }
        ));
        assert!(matches!(images.source("a", &b), Source::Media { .. }));
        assert!(matches!(images.source("no", &b), Source::Unresolved));
        assert!(matches!(images.source("no", &b), Source::Unresolved));
        // Other bytes alike: one medium.
        assert!(matches!(images.source("b", &b), Source::Media { .. }));
        assert!(matches!(images.source("", &b), Source::Unresolved));
        let (media, _) = images.into_parts();
        assert_eq!(media.len(), 1);
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[0], ("a".to_string(), MAX_IMAGE_BYTES));

        let mut calls = 0;
        let mut count = |_: &str, _: usize| {
            calls += 1;
            None
        };
        let mut images = Images::new(&mut count);
        for i in 0..MAX_IMAGES + 5 {
            images.source(&format!("cid:{i}"), &b);
        }
        drop(images);
        assert_eq!(calls, MAX_IMAGES);
    }

    #[test]
    fn a_too_long_answer_or_no_room_makes_nothing() {
        let mut resolve = |_: &str, limit: usize| Some(vec![0; limit + 1]);
        let b = budget();
        let mut images = Images::new(&mut resolve);
        assert!(matches!(images.source("x", &b), Source::Unresolved));
        assert!(!b.exhausted());
        // No room left: the resolver is not called.
        let mut called = false;
        let mut resolve = |_: &str, _: usize| {
            called = true;
            None
        };
        let empty = Budget::new(0, 1000);
        let mut images = Images::new(&mut resolve);
        assert!(matches!(images.source("x", &empty), Source::Unresolved));
        drop(images);
        assert!(!called);
        // Bytes that are no image (an SVG) are dropped.
        let mut svg = |_: &str, _: usize| Some(b"<svg/>".to_vec());
        let mut images = Images::new(&mut svg);
        assert!(matches!(images.source("x.svg", &b), Source::Unresolved));
        assert!(images.into_parts().0.is_empty());
    }

    #[test]
    fn the_placeholder_medium_is_made_once() {
        let mut none = |_: &str, _: usize| None;
        let b = budget();
        let mut images = Images::new(&mut none);
        let a = images.placeholder_rid(&b).unwrap();
        let c = images.placeholder_rid(&b).unwrap();
        assert_eq!(a, c);
        assert_eq!(images.into_parts().0.len(), 1);
    }
}
