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

/// The most distinct srcs an import asks its resolver for; later ones are
/// placeholders.
pub const MAX_IMAGES: usize = 1000;
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

/// What one `<img>` came to.
pub(crate) enum Source {
    /// A medium of the import: its rid and pixel size.
    Media {
        rid: String,
        width: u32,
        height: u32,
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
        }
    }

    pub(crate) fn into_media(self) -> Vec<ImportedMedia> {
        self.media
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
                let m = &self.media[i];
                Source::Media {
                    rid: m.rid.clone(),
                    width: m.info.width,
                    height: m.info.height,
                }
            }
            None => Source::Unresolved,
        }
    }

    /// Keep `bytes` (already charged as decoded) when they are an image:
    /// the medium holding the same bytes when there is one.
    fn add(&mut self, bytes: Vec<u8>, budget: &Budget) -> Option<usize> {
        let info = image::sniff(&bytes)?;
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
        self.media_bytes += bytes.len();
        let index = self.media.len();
        self.media.push(ImportedMedia {
            rid: local_rid(index),
            bytes,
            info,
        });
        Some(index)
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
        let index = self.media.len();
        self.media_bytes += PLACEHOLDER_PNG.len();
        self.media.push(ImportedMedia {
            rid: local_rid(index),
            bytes: PLACEHOLDER_PNG.to_vec(),
            info,
        });
        self.placeholder = Some(index);
        Some(self.media[index].rid.clone())
    }
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
fn base64_decode(b: &[u8]) -> Option<Vec<u8>> {
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
/// none, its pixels (a placeholder: no box). Wider than the body, it is
/// scaled down to the body's width keeping its aspect.
pub(crate) fn picture_twips(
    width: Option<u32>,
    height: Option<u32>,
    pixels: Option<(u32, u32)>,
) -> Option<(u64, u64)> {
    // In u128: a side may be any u32 (an `<img>`'s is not capped at a
    // page) and the pixels any u32, so `w * ph` and then `h * body` can pass
    // u64 (#1880 r2b).
    let (w, h): (u128, u128) = match (width, height, pixels) {
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
    };
    let body = u128::from(BODY_TWIPS);
    let (w, h) = if w > body {
        (body, h * body / w)
    } else {
        (w, h)
    };
    let side = |t: u128| u64::try_from(t).unwrap_or(u64::MAX);
    Some((side(w), side(h)))
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
            picture_twips(None, None, Some((1248, 100))),
            Some((9360, 750))
        );
        assert_eq!(
            picture_twips(Some(18_720), Some(600), None),
            Some((9360, 300))
        );
        assert_eq!(twips_emu((15, 30)), (9525, 19_050));
        assert_eq!(twips_emu((0, 1)), (1, 635));
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
        let media = images.into_media();
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
        assert!(images.into_media().is_empty());
    }

    #[test]
    fn the_placeholder_medium_is_made_once() {
        let mut none = |_: &str, _: usize| None;
        let b = budget();
        let mut images = Images::new(&mut none);
        let a = images.placeholder_rid(&b).unwrap();
        let c = images.placeholder_rid(&b).unwrap();
        assert_eq!(a, c);
        assert_eq!(images.into_media().len(), 1);
    }
}
