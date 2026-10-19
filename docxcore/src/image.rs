//! What an image file is, from its bytes: its kind and its size in pixels
//! (#768), for a picture inserted from a file. PNG, JPEG, GIF and BMP, the
//! kinds Insert Picture offers; anything else, or a truncated header, is
//! `None`, never a panic.

/// A recognised image: the extension and content type its media part gets,
/// and its size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub ext: &'static str,
    pub content_type: &'static str,
    pub width: u32,
    pub height: u32,
}

/// The kind and pixel size of `bytes`, by their signature (not the file's
/// name, which may lie). A zero width or height is not an image.
pub fn sniff(bytes: &[u8]) -> Option<ImageInfo> {
    let info = |ext, content_type, (width, height): (u32, u32)| ImageInfo {
        ext,
        content_type,
        width,
        height,
    };
    let found = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        info("png", "image/png", png_size(bytes)?)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        info("jpeg", "image/jpeg", jpeg_size(bytes)?)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        info("gif", "image/gif", (le16(bytes, 6)?, le16(bytes, 8)?))
    } else if bytes.starts_with(b"BM") {
        info("bmp", "image/bmp", bmp_size(bytes)?)
    } else {
        return None;
    };
    (found.width > 0 && found.height > 0).then_some(found)
}

/// EMUs per pixel at 96 dpi.
pub const EMU_PER_PX: i64 = 9525;

/// The largest extent DrawingML allows (`ST_PositiveCoordinate`), in EMUs.
pub const MAX_EMU: i64 = 27_273_042_316_900;

/// A picture's inline size in EMUs: its pixels at 96 dpi, scaled down to
/// `max_cx` (the text width) when wider, keeping its aspect, each side
/// within 1..=[`MAX_EMU`] whatever size the header claims.
pub fn extent_emu(info: &ImageInfo, max_cx: Option<i64>) -> (i64, i64) {
    let cx = i128::from(info.width) * i128::from(EMU_PER_PX);
    let cy = i128::from(info.height) * i128::from(EMU_PER_PX);
    let (cx, cy) = match max_cx {
        Some(max) if max > 0 && cx > i128::from(max) => {
            (i128::from(max), cy * i128::from(max) / cx)
        }
        _ => (cx, cy),
    };
    let clamp = |v: i128| v.clamp(1, i128::from(MAX_EMU)) as i64;
    (clamp(cx), clamp(cy))
}

/// The next free drawing-object id (`wp:docPr`'s `id`, an unsigned int)
/// over every story's XML in `xmls`: one past the largest, as Word keeps
/// them unique across a document and its headers and footers; past
/// `u32::MAX`, the smallest id not in use.
pub fn next_drawing_id<'a>(xmls: impl IntoIterator<Item = &'a str>) -> u32 {
    let mut used = std::collections::BTreeSet::new();
    for xml in xmls {
        let mut from = 0;
        while let Some(at) = xml[from..].find("docPr ").map(|i| from + i) {
            from = at + 6;
            let end = xml[at..].find('>').map_or(xml.len(), |e| at + e);
            if let Some(id) = crate::load::xml_attr_value(&xml[at..end], "id") {
                used.insert(id.trim().parse::<u32>().unwrap_or(0));
            }
        }
    }
    match used.last().copied().unwrap_or(0).checked_add(1) {
        Some(next) => next,
        None => (1..).find(|i| !used.contains(i)).unwrap_or(1),
    }
}

/// A run holding an inline picture: the media part `rid` names, `cx` x `cy`
/// EMUs, drawing object `id`. Every DrawingML namespace it uses is declared
/// on it, so it reads in a part whose root declares only `w` and `r`.
pub fn inline_picture_run(rid: &str, cx: i64, cy: i64, id: u32, file_name: &str) -> String {
    let name = crate::mermaid::xml_escape_attr(file_name);
    format!(
        "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\">\
         <wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
         <wp:docPr id=\"{id}\" name=\"Picture {id}\"/>\
         <wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
         <a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
         <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:nvPicPr><pic:cNvPr id=\"0\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
         </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
    )
}

/// An inline picture run's size in EMUs, from its `wp:inline`'s
/// `wp:extent`; `None` for any other XML (an anchored or floating drawing
/// included, even beside an inline one's fallback).
pub fn inline_extent(raw: &str) -> Option<(i64, i64)> {
    if !raw.starts_with("<w:r") || !raw.contains("<w:drawing") || raw.contains("<wp:anchor") {
        return None;
    }
    let inline = raw.find("<wp:inline")?;
    let at = inline + raw[inline..].find("<wp:extent")?;
    let end = at + raw[at..].find('>')?;
    let el = &raw[at..end];
    let cx = crate::load::xml_attr_value(el, "cx")?.parse().ok()?;
    let cy = crate::load::xml_attr_value(el, "cy")?.parse().ok()?;
    Some((cx, cy))
}

/// An inline picture run's `r:embed` relationship id, from its `a:blip`,
/// under the guards of [`inline_extent`] (inline only, never `wp:anchor`).
/// Only a drawing whose `a:graphicData` is a picture has one: a group, canvas
/// or shape holds blips of members that are not the whole drawing.
pub fn inline_embed(raw: &str) -> Option<String> {
    const PICTURE_URI: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
    inline_extent(raw)?;
    let data = raw.find("<a:graphicData")?;
    let data_end = data + raw[data..].find('>')?;
    if crate::load::xml_attr_value(&raw[data..data_end], "uri")? != PICTURE_URI {
        return None;
    }
    let mut from = data_end;
    let at = loop {
        let at = from + raw[from..].find("<a:blip")?;
        match raw.as_bytes().get(at + "<a:blip".len()) {
            Some(b' ' | b'/' | b'>' | b'\t' | b'\r' | b'\n') => break at,
            _ => from = at + 1,
        }
    };
    let end = at + raw[at..].find('>')?;
    crate::load::xml_attr_value(&raw[at..end], "r:embed").filter(|rid| !rid.is_empty())
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The IHDR chunk, which the signature must be followed by.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(12..16)? == b"IHDR").then_some(())?;
    Some((be32(b, 16)?, be32(b, 20)?))
}

/// The first start-of-frame marker's size, walking the segments after SOI.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    loop {
        // Fill bytes (0xFF) may pad before a marker.
        while *b.get(i)? == 0xff && *b.get(i + 1)? == 0xff {
            i += 1;
        }
        if *b.get(i)? != 0xff {
            return None;
        }
        let marker = *b.get(i + 1)?;
        i += 2;
        match marker {
            // Markers with no segment: TEM and RST0..7.
            0x01 | 0xd0..=0xd7 => continue,
            // End of image, or scan data before any frame: no size.
            0xd9 | 0xda => return None,
            // SOF0..15, but DHT (C4), JPG (C8) and DAC (CC).
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                // Length, precision, then height and width.
                return Some((be16(b, i + 5)?, be16(b, i + 3)?));
            }
            _ => i += be16(b, i)? as usize,
        }
    }
}

/// BITMAPCOREHEADER (12 bytes) has 16-bit sizes; every later header 32-bit
/// ones, a negative height meaning a top-down bitmap.
fn bmp_size(b: &[u8]) -> Option<(u32, u32)> {
    if le32(b, 14)? == 12 {
        return Some((le16(b, 18)?, le16(b, 20)?));
    }
    let w = le32(b, 18)?;
    let h = le32(b, 22)?;
    Some((u32::try_from(w).ok()?, h.unsigned_abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn png_size_from_ihdr() {
        let i = sniff(&png(640, 480)).unwrap();
        assert_eq!(
            (i.ext, i.content_type, i.width, i.height),
            ("png", "image/png", 640, 480)
        );
    }

    #[test]
    fn jpeg_size_from_the_frame_after_other_segments() {
        // SOI, an APP0 segment, a DHT one, then SOF2 (progressive): 300 x 200.
        let mut b = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2];
        b.extend_from_slice(&[0xff, 0xc4, 0, 3, 9]);
        b.extend_from_slice(&[0xff, 0xff, 0xc2, 0, 11, 8, 0, 200, 1, 44, 3]);
        let i = sniff(&b).unwrap();
        assert_eq!(
            (i.ext, i.content_type, i.width, i.height),
            ("jpeg", "image/jpeg", 300, 200)
        );
    }

    #[test]
    fn gif_and_bmp_sizes() {
        let gif = b"GIF89a\x20\x03\x58\x02rest";
        let i = sniff(gif).unwrap();
        assert_eq!((i.ext, i.width, i.height), ("gif", 800, 600));

        let mut bmp = b"BM".to_vec();
        bmp.resize(14, 0);
        bmp.extend_from_slice(&40i32.to_le_bytes());
        bmp.extend_from_slice(&120i32.to_le_bytes());
        bmp.extend_from_slice(&(-90i32).to_le_bytes());
        let i = sniff(&bmp).unwrap();
        assert_eq!(
            (i.ext, i.content_type, i.width, i.height),
            ("bmp", "image/bmp", 120, 90)
        );

        let mut core = b"BM".to_vec();
        core.resize(14, 0);
        core.extend_from_slice(&12i32.to_le_bytes());
        core.extend_from_slice(&[64, 0, 32, 0]);
        assert_eq!(sniff(&core).map(|i| (i.width, i.height)), Some((64, 32)));
    }

    #[test]
    fn extent_is_96_dpi_scaled_down_to_the_text_width() {
        let i = sniff(&png(200, 100)).unwrap();
        assert_eq!(extent_emu(&i, None), (200 * 9525, 100 * 9525));
        assert_eq!(extent_emu(&i, Some(10_000_000)), (200 * 9525, 100 * 9525));
        assert_eq!(extent_emu(&i, Some(952_500)), (952_500, 476_250));
        // FIX r1 M1: a header claiming a vast size neither overflows nor
        // writes an extent DrawingML refuses.
        let vast = ImageInfo {
            width: 10,
            height: u32::MAX,
            ..i
        };
        assert_eq!(extent_emu(&vast, Some(952_500)), (95_250, MAX_EMU));
        let wide = ImageInfo {
            width: u32::MAX,
            height: u32::MAX,
            ..i
        };
        assert_eq!(extent_emu(&wide, Some(5_943_600)), (5_943_600, 5_943_600));
        assert_eq!(extent_emu(&wide, None), (MAX_EMU, MAX_EMU));
        let thin = ImageInfo {
            width: u32::MAX,
            height: 1,
            ..i
        };
        assert_eq!(extent_emu(&thin, Some(100)), (100, 1));
    }

    #[test]
    fn drawing_ids_are_one_past_the_largest_in_any_story() {
        assert_eq!(next_drawing_id([]), 1);
        let body = r#"<w:p><wp:docPr id="7" name="Picture 7"/><wp:docPr name="x" id='12'/></w:p>"#;
        let header = inline_picture_run("rId1", 1, 1, 30, "a.png");
        assert_eq!(next_drawing_id([body, header.as_str()]), 31);
        // FIX r1 M1: the largest id in use is not followed by an overflow.
        let full = r#"<wp:docPr id="4294967295"/><wp:docPr id="1"/><wp:docPr id="2"/>"#;
        assert_eq!(next_drawing_id([full]), 3);
    }

    #[test]
    fn a_picture_run_declares_its_namespaces_and_reads_its_extent_back() {
        let run = inline_picture_run("rId5", 952_500, 476_250, 3, "logo <1>.png");
        for ns in ["xmlns:wp=", "xmlns:a=", "xmlns:pic="] {
            assert!(run.contains(ns), "{ns} in {run}");
        }
        assert!(run.contains(r#"<a:blip r:embed="rId5"/>"#));
        assert!(run.contains(r#"<wp:docPr id="3" name="Picture 3"/>"#));
        assert!(run.contains(r#"name="logo &lt;1&gt;.png""#));
        assert_eq!(inline_extent(&run), Some((952_500, 476_250)));
        assert_eq!(inline_extent("<w:r><w:ptab/></w:r>"), None);
        assert_eq!(
            inline_extent(
                "<w:r><w:drawing><wp:anchor><wp:extent cx=\"1\" cy=\"1\"/></wp:anchor></w:drawing></w:r>"
            ),
            None
        );
        // FIX r2 M1: an anchored drawing beside an inline fallback is not an
        // inline picture, and neither is a drawing outside a run.
        assert_eq!(
            inline_extent(
                "<w:r><mc:AlternateContent><mc:Choice><w:drawing><wp:anchor><wp:extent cx=\"1\" cy=\"1\"/></wp:anchor></w:drawing></mc:Choice>\
                 <mc:Fallback><w:drawing><wp:inline><wp:extent cx=\"2\" cy=\"2\"/></wp:inline></w:drawing></mc:Fallback></mc:AlternateContent></w:r>"
            ),
            None
        );
        assert_eq!(
            inline_extent(
                "<w:drawing><wp:inline><wp:extent cx=\"2\" cy=\"2\"/></wp:inline></w:drawing>"
            ),
            None
        );
    }

    #[test]
    fn inline_embed_reads_the_blip_rid_of_an_inline_picture_only() {
        let run = inline_picture_run("rId7", 952_500, 476_250, 3, "a.png");
        assert_eq!(inline_embed(&run).as_deref(), Some("rId7"));
        assert_eq!(inline_embed("<w:r><w:ptab/></w:r>"), None);
        assert_eq!(
            inline_embed(
                "<w:r><w:drawing><wp:anchor><wp:extent cx=\"1\" cy=\"1\"/><a:blip r:embed=\"rId1\"/></wp:anchor></w:drawing></w:r>"
            ),
            None
        );
        // A group holding a picture member is not itself a picture, and a
        // longer element name is not a blip.
        let group = run.replace(
            "http://schemas.openxmlformats.org/drawingml/2006/picture\"",
            "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup\"",
        );
        assert_ne!(group, run);
        assert_eq!(inline_embed(&group), None);
        let lookalike = run.replace("<a:blip ", "<a:blipFill ");
        assert_eq!(inline_embed(&lookalike), None);
        // An inline drawing with no blip (a shape) has no picture to draw.
        assert_eq!(
            inline_embed(
                "<w:r><w:drawing><wp:inline><wp:extent cx=\"2\" cy=\"2\"/></wp:inline></w:drawing></w:r>"
            ),
            None
        );
    }

    #[test]
    fn garbage_truncated_and_empty_images_are_none() {
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"hello, world"), None);
        assert_eq!(sniff(&png(640, 480)[..20]), None);
        assert_eq!(sniff(&png(0, 480)), None);
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0, 0, 200]), None);
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xd9]), None);
        assert_eq!(sniff(b"GIF89a\x01"), None);
        assert_eq!(sniff(b"BM\0\0"), None);
    }
}
