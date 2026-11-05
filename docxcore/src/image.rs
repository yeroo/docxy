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
    inline_picture_run_described(rid, cx, cy, id, file_name, "", None)
}

/// [`inline_picture_run`] with Alt Text (`wp:docPr`'s `descr`, left out
/// when empty; line breaks and tabs kept as character references, other
/// control characters dropped) and, when `outline` names an `RRGGBB`
/// colour, a 1 px line of it round the picture (#1880's placeholder).
pub fn inline_picture_run_described(
    rid: &str,
    cx: i64,
    cy: i64,
    id: u32,
    file_name: &str,
    descr: &str,
    outline: Option<&str>,
) -> String {
    inline_picture_run_shaped(
        rid,
        cx,
        cy,
        id,
        file_name,
        descr,
        outline,
        PictureShape::Rect,
    )
}

/// The shape a picture is cropped to (#1926): its `pic:spPr`'s
/// `a:prstGeom`, as Word's Picture Format > Crop to Shape writes it.
/// `Rect` is an uncropped picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureShape {
    Rect,
    /// `prst="ellipse"`: the oval inscribed in the picture's box.
    Ellipse,
    /// `prst="roundRect"`: corners of radius `min(w, h) * adj / 100000`,
    /// `adj` within `0..=ROUND_RECT_MAX_ADJ`.
    RoundRect {
        adj: u32,
    },
}

/// The largest `roundRect` `adj` (ECMA-376 presetShapeDefinitions pins
/// it to `0..=50000`): corners of half the shorter side.
pub const ROUND_RECT_MAX_ADJ: u32 = 50_000;

/// A `roundRect`'s `adj` when it names none (the preset's default).
pub const ROUND_RECT_DEFAULT_ADJ: u32 = 16_667;

impl PictureShape {
    /// The `a:prstGeom` element of this shape.
    fn prst_geom(self) -> String {
        match self {
            PictureShape::Rect => "<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>".into(),
            PictureShape::Ellipse => "<a:prstGeom prst=\"ellipse\"><a:avLst/></a:prstGeom>".into(),
            PictureShape::RoundRect { adj } => format!(
                "<a:prstGeom prst=\"roundRect\"><a:avLst><a:gd name=\"adj\" fmla=\"val {}\"/></a:avLst></a:prstGeom>",
                adj.min(ROUND_RECT_MAX_ADJ)
            ),
        }
    }
}

/// [`inline_picture_run_described`] cropped to `shape` (#1926);
/// [`PictureShape::Rect`] writes exactly what that does.
#[allow(clippy::too_many_arguments)]
pub fn inline_picture_run_shaped(
    rid: &str,
    cx: i64,
    cy: i64,
    id: u32,
    file_name: &str,
    descr: &str,
    outline: Option<&str>,
    shape: PictureShape,
) -> String {
    let geom = shape.prst_geom();
    let name = crate::mermaid::xml_escape_attr(file_name);
    let descr = if descr.is_empty() {
        String::new()
    } else {
        format!(" descr=\"{}\"", escape_descr(descr))
    };
    let line = outline.map_or(String::new(), |c| {
        format!(
            "<a:ln w=\"9525\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></a:ln>",
            crate::mermaid::xml_escape_attr(c)
        )
    });
    format!(
        "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\">\
         <wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
         <wp:docPr id=\"{id}\" name=\"Picture {id}\"{descr}/>\
         <wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
         <a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
         <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:nvPicPr><pic:cNvPr id=\"0\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         {geom}{line}</pic:spPr>\
         </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
    )
}

/// Alt Text as an attribute value: XML's specials escaped, a line break or
/// tab as a character reference (an attribute's white space is otherwise
/// normalised to spaces on read), any other control character dropped (XML
/// 1.0 cannot hold it).
fn escape_descr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#9;"),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
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
    let (at, end) = picture_blip(raw)?;
    crate::load::xml_attr_value(&raw[at..end], "r:embed").filter(|rid| !rid.is_empty())
}

/// The shape an inline picture run is cropped to (#1926): the `prst` of
/// the `a:prstGeom` in its picture's `pic:spPr`, under [`inline_embed`]'s
/// guards. Lenient, never an error: anything but an inline picture, no
/// `pic:spPr`, a `custGeom`, or a preset other than `ellipse` or
/// `roundRect` is [`PictureShape::Rect`]. A `roundRect`'s `adj` guide is
/// [`ROUND_RECT_DEFAULT_ADJ`] when absent or unreadable and clamped to
/// `0..=ROUND_RECT_MAX_ADJ`.
pub fn inline_shape(raw: &str) -> PictureShape {
    picture_shape(raw).unwrap_or(PictureShape::Rect)
}

fn picture_shape(raw: &str) -> Option<PictureShape> {
    let (_, blip_end) = picture_blip(raw)?;
    let sp = blip_end + start_tag(&raw[blip_end..], "<pic:spPr")?;
    let sp_end = sp + raw[sp..].find("</pic:spPr>")?;
    let props = &raw[sp..sp_end];
    let geom = start_tag(props, "<a:prstGeom")?;
    let geom_end = geom + props[geom..].find('>')?;
    let prst = crate::load::xml_attr_value(&props[geom..geom_end], "prst")?;
    match prst.as_str() {
        "ellipse" => Some(PictureShape::Ellipse),
        "roundRect" => {
            let close = props[geom..]
                .find("</a:prstGeom>")
                .map_or(props.len(), |at| geom + at);
            let guides = &props[geom_end..close];
            let adj = round_rect_adj(guides).unwrap_or(ROUND_RECT_DEFAULT_ADJ);
            Some(PictureShape::RoundRect { adj })
        }
        _ => None,
    }
}

/// The `adj` guide's value among a `roundRect`'s guides (`fmla="val N"`),
/// clamped; `None` when there is none or it does not read.
fn round_rect_adj(guides: &str) -> Option<u32> {
    let mut from = 0;
    while let Some(at) = start_tag(&guides[from..], "<a:gd") {
        let at = from + at;
        let end = at + guides[at..].find('>')?;
        let gd = &guides[at..end];
        if crate::load::xml_attr_value(gd, "name").as_deref() == Some("adj") {
            let fmla = crate::load::xml_attr_value(gd, "fmla")?;
            let n: i64 = fmla.trim().strip_prefix("val")?.trim().parse().ok()?;
            return Some(n.clamp(0, i64::from(ROUND_RECT_MAX_ADJ)) as u32);
        }
        from = end;
    }
    None
}

/// Where the first `tag` start tag (`<a:gd`, not `<a:gdLst`) begins in
/// `s`: the name followed by white space, `/` or `>`.
fn start_tag(s: &str, tag: &str) -> Option<usize> {
    let mut from = 0;
    loop {
        let at = from + s[from..].find(tag)?;
        match s.as_bytes().get(at + tag.len()) {
            Some(b' ' | b'/' | b'>' | b'\t' | b'\r' | b'\n') => return Some(at),
            _ => from = at + 1,
        }
    }
}

/// Where the `<a:blip ...>` start tag of an inline picture run sits
/// (`raw[at..end]`, `end` at its `>`), with [`inline_embed`]'s guards.
fn picture_blip(raw: &str) -> Option<(usize, usize)> {
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
    Some((at, at + raw[at..].find('>')?))
}

/// The content type of a picture part by its extension (any letter case),
/// for media whose package declares none (#1639).
pub fn media_content_type(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "emf" => "image/x-emf",
        "wmf" => "image/x-wmf",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// Every `r:embed` an inline picture run names (#1639): its blip's
/// ([`inline_embed`]), then each one inside that blip: `asvg:svgBlip`
/// (Word's SVG picture keeps a PNG fallback in the blip and the SVG in the
/// extension), `a14:imgLayer` (an artistic effect's original, `.wdp`), and
/// any other extension's, each once. `a:hlinkClick`'s `r:id` is a hyperlink, not media: not listed.
pub fn inline_embeds(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (at, end) in embed_tags(raw) {
        if let Some(rid) =
            crate::load::xml_attr_value(&raw[at..end], "r:embed").filter(|r| !r.is_empty())
        {
            if !out.contains(&rid) {
                out.push(rid);
            }
        }
    }
    out
}

/// `raw` with each `r:embed` [`inline_embeds`] lists renamed by `map` (a
/// rid not in it is kept), every other attribute, `r:link` included, as it
/// was. `None` when `raw` is not an inline picture.
pub fn replace_inline_embeds(
    raw: &str,
    map: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let tags = embed_tags(raw);
    if tags.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut copied = 0;
    for (at, end) in tags {
        let tag = &raw[at..end];
        let Some(old) = crate::load::xml_attr_value(tag, "r:embed") else {
            continue;
        };
        let Some(new) = map.get(&old) else {
            continue;
        };
        for quote in ['"', '\''] {
            let needle = format!("r:embed={quote}{old}{quote}");
            if let Some(i) = tag
                .match_indices(&needle)
                .map(|(i, _)| i)
                .find(|&i| i > 0 && tag.as_bytes()[i - 1].is_ascii_whitespace())
            {
                out.push_str(&raw[copied..at + i]);
                out.push_str(&format!("r:embed={quote}{new}{quote}"));
                copied = at + i + needle.len();
                break;
            }
        }
    }
    out.push_str(&raw[copied..]);
    Some(out)
}

/// The start tags holding an inline picture's media rids: its blip and the
/// tags with an `r:embed` inside it, in order (`raw[at..end]`, `end` at `>`).
fn embed_tags(raw: &str) -> Vec<(usize, usize)> {
    let Some((at, end)) = picture_blip(raw) else {
        return Vec::new();
    };
    let mut tags = vec![(at, end)];
    if raw[..end].ends_with('/') {
        return tags;
    }
    let close = raw[end..].find("</a:blip>").map_or(raw.len(), |i| end + i);
    // Every start tag inside the blip naming media: `asvg:svgBlip`,
    // `a14:imgLayer` (Word's artistic effects), and any such extension.
    let mut from = end;
    while let Some(i) = raw[from..close].find('<') {
        let s = from + i;
        let Some(e) = raw[s..close].find('>').map(|j| s + j) else {
            break;
        };
        let tag = &raw[s..e];
        if !tag.starts_with("</") && crate::load::xml_attr_value(tag, "r:embed").is_some() {
            tags.push((s, e));
        }
        from = e;
    }
    tags
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

/// A JPEG's first start-of-frame segment.
pub(crate) struct JpegFrame {
    /// The marker: `0xc0` baseline, `0xc1` extended, `0xc2` progressive, ….
    pub marker: u8,
    pub precision: u8,
    pub width: u32,
    pub height: u32,
    pub components: u8,
}

fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    jpeg_frame(b).map(|f| (f.width, f.height))
}

/// The first start-of-frame segment, walking the segments after SOI.
pub(crate) fn jpeg_frame(b: &[u8]) -> Option<JpegFrame> {
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
                // Length, precision, height, width, component count.
                return Some(JpegFrame {
                    marker,
                    precision: *b.get(i + 2)?,
                    height: be16(b, i + 3)?,
                    width: be16(b, i + 5)?,
                    components: *b.get(i + 7)?,
                });
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
    fn a_described_picture_run_carries_its_alt_text_and_outline() {
        // The plain run is byte-for-byte what it was.
        let plain = inline_picture_run("rId5", 10, 20, 3, "a.png");
        assert_eq!(
            plain,
            inline_picture_run_described("rId5", 10, 20, 3, "a.png", "", None)
        );
        assert!(!plain.contains("descr="));
        assert!(!plain.contains("<a:ln"));
        let run = inline_picture_run_described(
            "rId5",
            10,
            20,
            3,
            "a.png",
            "Logo \"A&B\" <x>\nline\ttab\u{1}\r",
            Some("A6A6A6"),
        );
        assert!(
            run.contains(
                r#"<wp:docPr id="3" name="Picture 3" descr="Logo &quot;A&amp;B&quot; &lt;x&gt;&#10;line&#9;tab"/>"#
            ),
            "{run}"
        );
        assert!(run.contains(
            r#"<a:avLst/></a:prstGeom><a:ln w="9525"><a:solidFill><a:srgbClr val="A6A6A6"/></a:solidFill></a:ln></pic:spPr>"#
        ));
        assert_eq!(inline_extent(&run), Some((10, 20)));
        assert_eq!(inline_embed(&run).as_deref(), Some("rId5"));
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
    fn replace_inline_embeds_rewrites_only_the_blip_embeds() {
        let map = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let run = inline_picture_run("rId7", 952_500, 476_250, 3, "a.png");
        assert_eq!(inline_embeds(&run), vec!["rId7".to_string()]);
        let out = replace_inline_embeds(&run, &map(&[("rId7", "rId12")])).expect("a picture");
        assert_eq!(inline_embed(&out).as_deref(), Some("rId12"));
        assert_eq!(out.replace("rId12", "rId7"), run);
        // r:link (and a longer attribute ending in r:embed) stay as they are.
        let linked = run.replace(
            "r:embed=\"rId7\"",
            "r:link=\"rId7\" xr:embed=\"rId7\" r:embed=\"rId7\"",
        );
        assert_ne!(linked, run);
        let out = replace_inline_embeds(&linked, &map(&[("rId7", "rId9")])).expect("a picture");
        assert!(
            out.contains("r:link=\"rId7\" xr:embed=\"rId7\" r:embed=\"rId9\""),
            "{out}"
        );
        assert_eq!(
            replace_inline_embeds("<w:r><w:ptab/></w:r>", &map(&[])),
            None
        );
        // Word's SVG picture: the PNG blip and the svgBlip inside it.
        let svg = run.replace(
            "<a:blip r:embed=\"rId7\"/>",
            "<a:blip r:embed=\"rId7\"><a:extLst><a:ext uri=\"{96DAC541-7B7A-43D3-8B79-37D633B846F1}\"><asvg:svgBlip xmlns:asvg=\"http://schemas.microsoft.com/office/drawing/2016/SVG/main\" r:embed=\"rId8\"/></a:ext></a:extLst></a:blip>",
        );
        assert_ne!(svg, run);
        assert_eq!(
            inline_embeds(&svg),
            vec!["rId7".to_string(), "rId8".to_string()]
        );
        let out =
            replace_inline_embeds(&svg, &map(&[("rId7", "rId20"), ("rId8", "rId21")])).unwrap();
        assert_eq!(
            inline_embeds(&out),
            vec!["rId20".to_string(), "rId21".to_string()]
        );
        assert_eq!(out.replace("rId20", "rId7").replace("rId21", "rId8"), svg);
        // An artistic effect keeps the original in an a14:imgLayer.
        let layer = run.replace(
            "<a:blip r:embed=\"rId7\"/>",
            "<a:blip r:embed=\"rId7\"><a:extLst><a:ext uri=\"{BEBA8EAE-BF5A-486C-A8C5-ECC9F3942E4B}\"><a14:imgProps xmlns:a14=\"http://schemas.microsoft.com/office/drawing/2010/main\"><a14:imgLayer r:embed=\"rId9\"><a14:imgEffect><a14:artisticPencilSketch/></a14:imgEffect></a14:imgLayer></a14:imgProps></a:ext></a:extLst></a:blip>",
        );
        assert_ne!(layer, run);
        assert_eq!(
            inline_embeds(&layer),
            vec!["rId7".to_string(), "rId9".to_string()]
        );
        let out = replace_inline_embeds(&layer, &map(&[("rId9", "rId30")])).unwrap();
        assert_eq!(out, layer.replace("rId9", "rId30"));
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

    #[test]
    fn a_rect_shaped_run_is_todays_run_and_shapes_write_their_preset() {
        let today = inline_picture_run_described("rId5", 10, 20, 3, "a.png", "x", Some("A6A6A6"));
        let rect = inline_picture_run_shaped(
            "rId5",
            10,
            20,
            3,
            "a.png",
            "x",
            Some("A6A6A6"),
            PictureShape::Rect,
        );
        assert_eq!(rect, today);
        assert!(today.contains(r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#));
        let shaped = |shape| inline_picture_run_shaped("rId5", 10, 20, 3, "a.png", "", None, shape);
        let oval = shaped(PictureShape::Ellipse);
        assert!(oval.contains(r#"<a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom></pic:spPr>"#));
        assert!(!oval.contains(r#"prst="rect""#));
        let round = shaped(PictureShape::RoundRect { adj: 20_000 });
        assert!(round.contains(
            r#"<a:prstGeom prst="roundRect"><a:avLst><a:gd name="adj" fmla="val 20000"/></a:avLst></a:prstGeom>"#
        ));
        // Written clamped.
        assert!(shaped(PictureShape::RoundRect { adj: 90_000 }).contains(r#"fmla="val 50000""#));
        for shape in [
            PictureShape::Rect,
            PictureShape::Ellipse,
            PictureShape::RoundRect { adj: 20_000 },
            PictureShape::RoundRect { adj: 0 },
        ] {
            let run = shaped(shape);
            assert_eq!(inline_shape(&run), shape, "{run}");
            assert_eq!(inline_embed(&run).as_deref(), Some("rId5"));
            assert_eq!(inline_extent(&run), Some((10, 20)));
        }
    }

    /// Word's own Crop to Shape picture: `bwMode` on `pic:spPr`, other
    /// prefixes' attributes, the geometry after the transform.
    fn word_picture(geom: &str) -> String {
        format!(
            r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="Picture 1"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="1" name="a.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId4"><a:extLst/></a:blip><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr bwMode="auto"><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm>{geom}<a:noFill/></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#
        )
    }

    #[test]
    fn the_shape_reads_from_words_own_crop_and_is_lenient() {
        let read = |geom: &str| inline_shape(&word_picture(geom));
        assert_eq!(
            read(r#"<a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom>"#),
            PictureShape::Ellipse
        );
        assert_eq!(
            read(r#"<a:prstGeom prst="roundRect"><a:avLst/></a:prstGeom>"#),
            PictureShape::RoundRect {
                adj: ROUND_RECT_DEFAULT_ADJ
            }
        );
        assert_eq!(
            read(r#"<a:prstGeom prst="roundRect"/>"#),
            PictureShape::RoundRect {
                adj: ROUND_RECT_DEFAULT_ADJ
            }
        );
        assert_eq!(
            read(
                r#"<a:prstGeom prst="roundRect"><a:avLst><a:gd name="adj" fmla="val 8000"/></a:avLst></a:prstGeom>"#
            ),
            PictureShape::RoundRect { adj: 8000 }
        );
        // Out of range: clamped; junk: the default.
        for (fmla, adj) in [
            ("val 70000", 50_000),
            ("val -5", 0),
            ("val x", ROUND_RECT_DEFAULT_ADJ),
            ("*/ w 1 2", ROUND_RECT_DEFAULT_ADJ),
        ] {
            let geom = format!(
                r#"<a:prstGeom prst="roundRect"><a:avLst><a:gd name="adj" fmla="{fmla}"/></a:avLst></a:prstGeom>"#
            );
            assert_eq!(read(&geom), PictureShape::RoundRect { adj }, "{fmla}");
        }
        // Other presets, a custom geometry, none at all: a rectangle.
        for geom in [
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#,
            r#"<a:prstGeom prst="snip1Rect"><a:avLst/></a:prstGeom>"#,
            r#"<a:custGeom><a:pathLst/></a:custGeom>"#,
            "",
        ] {
            assert_eq!(read(geom), PictureShape::Rect, "{geom}");
        }
        // Not an inline picture: an anchored one, a shape, plain text.
        let oval = r#"<a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom>"#;
        let anchored = word_picture(oval)
            .replace("<wp:inline", "<wp:anchor")
            .replace("</wp:inline>", "</wp:anchor>");
        assert_eq!(inline_shape(&anchored), PictureShape::Rect);
        let shape = word_picture(oval).replace(
            "drawingml/2006/picture\"><pic:pic",
            "drawingml/2006/wordprocessingShape\"><pic:pic",
        );
        assert_eq!(inline_shape(&shape), PictureShape::Rect);
        assert_eq!(inline_shape("<w:r><w:t>x</w:t></w:r>"), PictureShape::Rect);
        // An ellipse outside the picture's own `pic:spPr` is not its crop.
        let elsewhere =
            word_picture("").replace("</pic:pic>", r#"</pic:pic><a:prstGeom prst="ellipse"/>"#);
        assert_eq!(inline_shape(&elsewhere), PictureShape::Rect);
    }
}
