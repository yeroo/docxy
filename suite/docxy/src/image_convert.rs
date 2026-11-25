//! An HTML import's SVG and WebP pictures as PNG (#1898): the
//! [`docxcore::import::ImageConverter`] the converting child
//! ([`crate::convert_child`]) gives the import, so a bad picture costs that
//! child only.
//!
//! - WebP through `image`, its decoder bounded by [`image::Limits`]: at
//!   most [`MAX_DECODE_SIDE`] pixels a side and [`MAX_DECODE_ALLOC`] bytes,
//!   so a few header bytes cannot ask for gigabytes. Its CSS size is its
//!   pixels.
//! - SVG through `resvg`, at twice its intrinsic size (its `width` /
//!   `height`, else its `viewBox`, else, as usvg sizes one with neither,
//!   the box its content takes from the origin), the long side at most
//!   [`MAX_RASTER_SIDE`]; its CSS size is the intrinsic one. An `<image>` in it is drawn only from a `data:` URL:
//!   any other href (a file path, absolute or relative, a URL) is never
//!   read, so converting a page's SVG never reaches the disk or the
//!   network (#1880's rule for the page itself). Its text is drawn in the
//!   system's fonts, loaded once, on the first SVG.

use docxcore::import::{ConvertKind, ConvertedImage};
use resvg::{tiny_skia, usvg};
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

/// The longest side, in pixels, an SVG is rasterised at.
pub(crate) const MAX_RASTER_SIDE: u32 = 2048;
/// The longest side, in pixels, a WebP may declare.
pub(crate) const MAX_DECODE_SIDE: u32 = 16384;
/// The most bytes a WebP's decoder may allocate.
pub(crate) const MAX_DECODE_ALLOC: u64 = 256 << 20;

/// `bytes` (a `kind`) as PNG and its CSS size; `None` when it does not
/// decode, or its PNG is longer than `limit`.
pub(crate) fn convert(kind: ConvertKind, bytes: &[u8], limit: usize) -> Option<ConvertedImage> {
    let (png, width, height) = match kind {
        ConvertKind::Svg => svg_to_png(bytes)?,
        ConvertKind::Webp => webp_to_png(bytes)?,
    };
    (png.len() <= limit).then_some(ConvertedImage { png, width, height })
}

/// `bytes` decoded as a WebP within the converter's limits
/// ([`MAX_DECODE_SIDE`] a side, [`MAX_DECODE_ALLOC`] bytes).
fn decode_webp(bytes: &[u8]) -> image::ImageResult<image::DynamicImage> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::WebP);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    reader.decode()
}

fn webp_to_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = decode_webp(bytes).ok()?;
    let (w, h) = (img.width(), img.height());
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some((png, w, h))
}

/// The system's fonts, loaded on first use.
fn system_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// The options an SVG is read with: no resources folder, `<image>` hrefs
/// from `data:` URLs only, the system fonts.
fn svg_options() -> usvg::Options<'static> {
    usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: system_fonts(),
        ..usvg::Options::default()
    }
}

fn svg_to_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let tree = usvg::Tree::from_data(bytes, &svg_options()).ok()?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let side = |v: f32| (v.round().min(u32::MAX as f32) as u32).max(1);
    let scale = 2.0f32.min(MAX_RASTER_SIDE as f32 / w.max(h));
    let px = |v: f32| side(v * scale).min(MAX_RASTER_SIDE);
    let (pw, ph) = (px(w), px(h));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    let transform = tiny_skia::Transform::from_scale(pw as f32 / w, ph as f32 / h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let png = pixmap.encode_png().ok()?;
    Some((png, side(w), side(h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8()
    }

    fn red_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn webp(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 255, 255]));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::WebP)
            .unwrap();
        out
    }

    fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = c.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - c.len()));
            for k in 0..=c.len() {
                out.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            }
            for _ in c.len()..3 {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn an_svg_is_drawn_at_twice_its_size_and_takes_its_own_1898() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="10"><rect width="30" height="10" fill="red"/></svg>"#;
        let c = convert(ConvertKind::Svg, svg, usize::MAX).unwrap();
        assert_eq!((c.width, c.height), (30, 10));
        let img = decode(&c.png);
        assert_eq!(img.dimensions(), (60, 20));
        assert_eq!(img.get_pixel(30, 10).0, [255, 0, 0, 255]);
    }

    #[test]
    fn an_svgs_size_comes_from_its_view_box_else_its_content_1898() {
        let vb = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20"/>"#;
        let c = convert(ConvertKind::Svg, vb, usize::MAX).unwrap();
        assert_eq!((c.width, c.height), (40, 20));
        let none = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect x="10" width="40" height="20"/></svg>"#;
        let c = convert(ConvertKind::Svg, none, usize::MAX).unwrap();
        assert_eq!((c.width, c.height), (50, 20));
    }

    #[test]
    fn a_big_svg_is_drawn_with_its_long_side_capped_1898() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="5000" height="1000"/>"#;
        let c = convert(ConvertKind::Svg, svg, usize::MAX).unwrap();
        assert_eq!((c.width, c.height), (5000, 1000));
        assert_eq!(decode(&c.png).dimensions(), (MAX_RASTER_SIDE, 410));
    }

    /// An `<image>` naming a real file by its absolute path draws nothing
    /// (the file is never read); the same picture as a `data:` URL draws.
    #[test]
    fn an_svg_image_is_read_from_a_data_url_only_1898() {
        let dir = std::env::temp_dir().join(format!("docxy-1898-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("red.png");
        std::fs::write(&file, red_png(4, 4)).unwrap();
        let svg = |href: &str| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><image width="4" height="4" xlink:href="{href}"/></svg>"#
            )
        };
        let opaque = |c: &ConvertedImage| decode(&c.png).pixels().any(|p| p.0[3] > 0);
        let path = file.to_string_lossy().into_owned();
        for href in [path.as_str(), "red.png", "file:///etc/hostname"] {
            let c = convert(ConvertKind::Svg, svg(href).as_bytes(), usize::MAX).unwrap();
            assert!(!opaque(&c), "{href} was read");
        }
        let data = format!("data:image/png;base64,{}", b64(&red_png(4, 4)));
        let c = convert(ConvertKind::Svg, svg(&data).as_bytes(), usize::MAX).unwrap();
        assert!(opaque(&c), "a data: picture draws");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_webp_is_a_png_of_its_pixels_1898() {
        let c = convert(ConvertKind::Webp, &webp(6, 3), usize::MAX).unwrap();
        assert_eq!((c.width, c.height), (6, 3));
        let img = decode(&c.png);
        assert_eq!(img.dimensions(), (6, 3));
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 255, 255]);
    }

    #[test]
    fn a_bad_or_too_long_picture_converts_to_nothing_1898() {
        assert_eq!(convert(ConvertKind::Svg, b"<svg", usize::MAX), None);
        assert_eq!(
            convert(ConvertKind::Webp, b"RIFF\0\0\0\0WEBPjunk", usize::MAX),
            None
        );
        assert_eq!(convert(ConvertKind::Webp, &webp(6, 3), 10), None);
        // A VP8X header with no image chunk after it: malformed, refused
        // before any size is looked at.
        let mut vp8x = b"RIFF\x16\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00".to_vec();
        vp8x.extend([0xff, 0x3f, 0x00, 0xff, 0x3f, 0x00]);
        assert_eq!(convert(ConvertKind::Webp, &vp8x, usize::MAX), None);
    }

    /// A lossless (VP8L) WebP header of `w` x `h`, its bit stream cut
    /// short: well formed up to where the decoder sizes its buffer.
    fn vp8l_header(w: u32, h: u32) -> Vec<u8> {
        // Width - 1 and height - 1 in 14 bits each, then alpha and a
        // version of 0, little endian.
        let bits = (w - 1) | ((h - 1) << 14);
        let mut chunk = vec![0x2f];
        chunk.extend(bits.to_le_bytes());
        chunk.extend([0u8; 16]);
        let mut out = b"RIFF".to_vec();
        out.extend((4 + 8 + chunk.len() as u32).to_le_bytes());
        out.extend(b"WEBPVP8L");
        out.extend((chunk.len() as u32).to_le_bytes());
        out.extend(chunk);
        out
    }

    /// 10000 x 10000 RGBA is 400 MB: under `image`'s default 512 MiB, so
    /// only the converter's own [`MAX_DECODE_ALLOC`] (256 MiB) refuses it,
    /// before any pixel is decoded (r2).
    #[test]
    fn a_webp_past_the_allocation_limit_is_refused_by_it_1898() {
        assert!(10_000u64 * 10_000 * 4 > MAX_DECODE_ALLOC);
        match decode_webp(&vp8l_header(10_000, 10_000)) {
            Err(image::ImageError::Limits(_)) => {}
            other => panic!("not refused by the limits: {other:?}"),
        }
        // The same header small enough is read past the limits (and fails
        // on its cut-short bit stream instead).
        assert!(!matches!(
            decode_webp(&vp8l_header(100, 100)),
            Err(image::ImageError::Limits(_))
        ));
    }
}
