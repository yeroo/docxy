//! Pictures a PDF can carry as they are (#797): a JPEG goes in under
//! `/DCTDecode` and a PNG's concatenated `IDAT` under `/FlateDecode` with a PNG
//! predictor, so nothing here decodes pixels (`docxcore` has no inflater).
//! Only what PDF can take unchanged is read: baseline/progressive JPEG with
//! one or three components, and non-interlaced PNG of grey or RGB at 8 bits, or
//! palette at 1 to 8, whose `tRNS` is ignored. Anything else is `None`, and the
//! caller draws nothing.

/// A picture ready to be written as an image XObject.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfImage {
    pub width: u32,
    pub height: u32,
    /// The XObject dictionary's entries after `/Width` and `/Height`
    /// (colour space, bits, filter, decode parameters).
    entries: String,
    /// The stream's bytes as stored (already filtered).
    data: Vec<u8>,
}

impl PdfImage {
    /// A JPEG or PNG, sniffed by its signature.
    pub fn parse(bytes: &[u8]) -> Option<PdfImage> {
        if bytes.starts_with(&[0xFF, 0xD8]) {
            jpeg(bytes)
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            png(bytes)
        } else {
            None
        }
    }

    /// The XObject object body: dictionary, stream.
    pub fn object(&self) -> Vec<u8> {
        let mut out = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} {} /Length {} >>\nstream\n",
            self.width,
            self.height,
            self.entries,
            self.data.len()
        )
        .into_bytes();
        out.extend(&self.data);
        out.extend(b"\nendstream");
        out
    }
}

fn jpeg(b: &[u8]) -> Option<PdfImage> {
    let frame = crate::image::jpeg_frame(b)?;
    // PDF's DCTDecode reads baseline, extended and progressive frames only.
    let space = match frame.components {
        1 => "/DeviceGray",
        3 => "/DeviceRGB",
        _ => return None,
    };
    if !matches!(frame.marker, 0xc0..=0xc2)
        || frame.precision != 8
        || frame.width == 0
        || frame.height == 0
    {
        return None;
    }
    Some(PdfImage {
        width: frame.width,
        height: frame.height,
        entries: format!("/ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode"),
        data: b.to_vec(),
    })
}

fn png(b: &[u8]) -> Option<PdfImage> {
    let mut i = 8;
    let mut header = None;
    let mut palette: &[u8] = &[];
    let mut idat = Vec::new();
    while i + 8 <= b.len() {
        let len = u32::from_be_bytes(b[i..i + 4].try_into().ok()?) as usize;
        let kind = &b[i + 4..i + 8];
        let body = b.get(i + 8..i + 8 + len)?;
        match kind {
            b"IHDR" if len == 13 => header = Some(body),
            b"PLTE" => palette = body,
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    let h = header?;
    let width = u32::from_be_bytes(h[0..4].try_into().ok()?);
    let height = u32::from_be_bytes(h[4..8].try_into().ok()?);
    let (depth, color, compression, filter, interlace) = (h[8], h[9], h[10], h[11], h[12]);
    if width == 0 || height == 0 || compression != 0 || filter != 0 || interlace != 0 {
        return None;
    }
    if idat.is_empty() {
        return None;
    }
    let (space, colors) = match (color, depth) {
        (0, 8) => ("/DeviceGray".to_string(), 1),
        (2, 8) => ("/DeviceRGB".to_string(), 3),
        (3, 1 | 2 | 4 | 8) => {
            if palette.is_empty() || !palette.len().is_multiple_of(3) || palette.len() > 768 {
                return None;
            }
            let hex: String = palette.iter().map(|v| format!("{v:02x}")).collect();
            (
                format!("[/Indexed /DeviceRGB {} <{hex}>]", palette.len() / 3 - 1),
                1,
            )
        }
        _ => return None,
    };
    Some(PdfImage {
        width,
        height,
        entries: format!(
            "/ColorSpace {space} /BitsPerComponent {depth} /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors {colors} /BitsPerComponent {depth} /Columns {width} >>"
        ),
        data: idat,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A one-pixel-wide PNG of `rows` scanlines of `color`/`depth` (each
    /// scanline `filter 0` plus `row` bytes), with an optional palette.
    pub(crate) fn make_png(
        width: u32,
        color: u8,
        depth: u8,
        interlace: u8,
        row: &[u8],
        rows: u32,
        palette: Option<&[u8]>,
    ) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
            out.extend((body.len() as u32).to_be_bytes());
            out.extend(kind);
            out.extend(body);
            out.extend([0, 0, 0, 0]); // CRC: never checked
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend(width.to_be_bytes());
        ihdr.extend(rows.to_be_bytes());
        ihdr.extend([depth, color, 0, 0, interlace]);
        chunk(&mut out, b"IHDR", &ihdr);
        if let Some(p) = palette {
            chunk(&mut out, b"PLTE", p);
        }
        let mut raw = Vec::new();
        for _ in 0..rows {
            raw.push(0);
            raw.extend(row);
        }
        chunk(&mut out, b"IDAT", &crate::deflate::zlib(&raw));
        chunk(&mut out, b"IEND", &[]);
        out
    }

    pub(crate) fn make_jpeg(width: u16, height: u16, comps: u8) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0];
        b.extend([0xFF, 0xC0, 0, 8 + 3 * comps as u16 as u8, 8]);
        b.extend(height.to_be_bytes());
        b.extend(width.to_be_bytes());
        b.push(comps);
        for c in 0..comps {
            b.extend([c + 1, 0x11, 0]);
        }
        b.extend([0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn jpeg_passes_through_by_its_components() {
        let rgb = PdfImage::parse(&make_jpeg(40, 20, 3)).unwrap();
        assert_eq!((rgb.width, rgb.height), (40, 20));
        assert!(rgb.entries.contains("/DeviceRGB") && rgb.entries.contains("/DCTDecode"));
        let gray = PdfImage::parse(&make_jpeg(4, 2, 1)).unwrap();
        assert!(gray.entries.contains("/DeviceGray"));
        assert_eq!(PdfImage::parse(&make_jpeg(4, 2, 4)), None, "CMYK");
    }

    #[test]
    fn png_passes_through_with_a_predictor_and_refuses_the_rest() {
        let rgb = PdfImage::parse(&make_png(2, 2, 8, 0, &[1, 2, 3, 4, 5, 6], 3, None)).unwrap();
        assert_eq!((rgb.width, rgb.height), (2, 3));
        assert!(
            rgb.entries
                .contains("/Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns 2")
        );
        assert!(rgb.entries.contains("/FlateDecode"));
        let pal = PdfImage::parse(&make_png(
            2,
            3,
            4,
            0,
            &[0x01],
            1,
            Some(&[255, 0, 0, 0, 0, 255]),
        ))
        .unwrap();
        assert!(
            pal.entries
                .contains("[/Indexed /DeviceRGB 1 <ff00000000ff>]")
        );
        // Alpha, interlaced, 16-bit, palette without PLTE, garbage.
        assert_eq!(
            PdfImage::parse(&make_png(1, 6, 8, 0, &[0; 4], 1, None)),
            None
        );
        assert_eq!(
            PdfImage::parse(&make_png(1, 4, 8, 0, &[0; 2], 1, None)),
            None
        );
        assert_eq!(
            PdfImage::parse(&make_png(1, 2, 8, 1, &[0; 3], 1, None)),
            None
        );
        assert_eq!(
            PdfImage::parse(&make_png(1, 2, 16, 0, &[0; 6], 1, None)),
            None
        );
        assert_eq!(PdfImage::parse(&make_png(1, 3, 8, 0, &[0], 1, None)), None);
        assert_eq!(PdfImage::parse(b"GIF89a....."), None);
    }
}
