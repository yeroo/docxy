//! PNG encoding (RFC 2083): the four chunks a viewer needs, over the DEFLATE
//! stream in [`crate::deflate`].
//!
//! PNG rather than a raw bitmap because the point of a capture is that a person
//! — or an agent that can read images — opens it and looks at what rendered.
//! Rows are filtered before compression with the usual heuristic, which is what
//! turns a screenshot's flat areas into the long runs the matcher can code.

use crate::image::Image;
use opccore::zipwrite::crc32;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The per-row filters worth trying. Average (3) is left out: it needs the same
/// work as Paeth for less benefit on the flat, axis-aligned content a UI
/// screenshot is made of.
const FILTERS: [u8; 4] = [0, 1, 2, 4];

/// Paeth's predictor (RFC 2083 6.6): whichever of the three neighbours the
/// linear estimate `a + b - c` is closest to.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Apply filter `kind` to `row`, given the row above it, writing into `out`.
/// `bpp` is bytes per pixel — the distance back to the pixel on the left.
pub fn filter_row(kind: u8, row: &[u8], up: &[u8], bpp: usize, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(row.len());
    for i in 0..row.len() {
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = up.get(i).copied().unwrap_or(0);
        let c = if i >= bpp {
            up.get(i - bpp).copied().unwrap_or(0)
        } else {
            0
        };
        let v = match kind {
            1 => row[i].wrapping_sub(a),
            2 => row[i].wrapping_sub(b),
            3 => row[i].wrapping_sub((((a as u16) + (b as u16)) / 2) as u8),
            4 => row[i].wrapping_sub(paeth(a, b, c)),
            _ => row[i],
        };
        out.push(v);
    }
}

/// The standard heuristic: pick the filter whose output has the smallest sum of
/// absolute signed values, which is a decent proxy for "compresses best".
fn best_filter(row: &[u8], up: &[u8], bpp: usize) -> (u8, Vec<u8>) {
    let mut best: Option<(u64, u8, Vec<u8>)> = None;
    let mut buf = Vec::new();
    for &k in &FILTERS {
        filter_row(k, row, up, bpp, &mut buf);
        let score: u64 = buf.iter().map(|&b| (b as i8).unsigned_abs() as u64).sum();
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, k, buf.clone()));
        }
    }
    let (_, k, data) = best.expect("FILTERS is never empty");
    (k, data)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    // The CRC covers the type and the data, not the length.
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encode an RGBA image as a PNG.
pub fn encode(img: &Image) -> Vec<u8> {
    let mut raw = Vec::with_capacity(img.px.len() + img.h as usize);
    let mut prev = vec![0u8; img.w as usize * 4];
    for y in 0..img.h {
        let row = img.row(y);
        let (kind, data) = best_filter(row, &prev, 4);
        raw.push(kind);
        raw.extend_from_slice(&data);
        prev.copy_from_slice(row);
    }

    let mut out = Vec::from(SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.w.to_be_bytes());
    ihdr.extend_from_slice(&img.h.to_be_bytes());
    // 8 bits per sample, colour type 6 (truecolour with alpha), deflate,
    // adaptive filtering, no interlacing.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &crate::deflate::zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Encode `img` and write it to `path`, creating the parent directory.
pub fn write(path: &std::path::Path, img: &Image) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, encode(img))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Image;
    use opccore::inflate::inflate_raw;

    /// Undo `filter_row`, so the round trip below tests the encoder rather than
    /// two copies of the same mistake. Written straight from RFC 2083 6.6.
    fn unfilter(kind: u8, row: &mut [u8], up: &[u8], bpp: usize) {
        for i in 0..row.len() {
            let a = if i >= bpp { row[i - bpp] } else { 0 };
            let b = up.get(i).copied().unwrap_or(0);
            let c = if i >= bpp {
                up.get(i - bpp).copied().unwrap_or(0)
            } else {
                0
            };
            row[i] = match kind {
                1 => row[i].wrapping_add(a),
                2 => row[i].wrapping_add(b),
                3 => row[i].wrapping_add((((a as u16) + (b as u16)) / 2) as u8),
                4 => row[i].wrapping_add(paeth(a, b, c)),
                _ => row[i],
            };
        }
    }

    /// A minimal PNG reader: walk the chunks, check every CRC, inflate the
    /// IDATs, unfilter, and hand back the pixels. Anything the encoder got
    /// wrong shows up here as a mismatch rather than as a file no one opens
    /// until much later.
    fn decode(png: &[u8]) -> Image {
        assert_eq!(&png[0..8], &SIGNATURE, "signature");
        let mut at = 8usize;
        let (mut w, mut h) = (0u32, 0u32);
        let mut idat = Vec::new();
        let mut saw_iend = false;
        while at + 8 <= png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let body = &png[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[at + 4..at + 8 + len]), "CRC of a chunk");
            match kind {
                b"IHDR" => {
                    w = u32::from_be_bytes(body[0..4].try_into().unwrap());
                    h = u32::from_be_bytes(body[4..8].try_into().unwrap());
                    assert_eq!(&body[8..13], &[8, 6, 0, 0, 0], "8-bit RGBA, no interlace");
                }
                b"IDAT" => idat.extend_from_slice(body),
                b"IEND" => {
                    saw_iend = true;
                    assert_eq!(len, 0);
                }
                _ => {}
            }
            at += 12 + len;
        }
        assert!(saw_iend, "IEND");
        assert_eq!(at, png.len(), "no trailing bytes");
        // Strip the zlib header and checksum, then inflate.
        let stride = w as usize * 4;
        let expect = (stride + 1) * h as usize;
        let raw = inflate_raw(&idat[2..idat.len() - 4], expect).expect("IDAT inflates");
        assert_eq!(raw.len(), expect);
        assert_eq!(
            u32::from_be_bytes(idat[idat.len() - 4..].try_into().unwrap()),
            crate::deflate::adler32(&raw),
            "the zlib checksum covers the filtered rows"
        );
        let mut px = Vec::with_capacity(stride * h as usize);
        let mut prev = vec![0u8; stride];
        for y in 0..h as usize {
            let kind = raw[y * (stride + 1)];
            let mut row = raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)].to_vec();
            unfilter(kind, &mut row, &prev, 4);
            prev.clone_from(&row);
            px.extend_from_slice(&row);
        }
        Image::from_rgba(w, h, px).unwrap()
    }

    fn sample(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // A flat block, a gradient and a hard edge, so several filters
                // win on different rows.
                let rgba = if x < w / 3 {
                    [0x20, 0x40, 0x60, 0xff]
                } else if x < 2 * w / 3 {
                    [(x * 3) as u8, (y * 5) as u8, 0x80, 0xff]
                } else {
                    [0xff, 0xff, 0xff, (x + y) as u8]
                };
                img.set_pixel(x, y, rgba);
            }
        }
        img
    }

    #[test]
    fn a_png_round_trips_through_its_own_decoder() {
        let img = sample(37, 21);
        assert_eq!(decode(&encode(&img)), img);
    }

    #[test]
    fn a_one_pixel_image_round_trips() {
        let mut img = Image::new(1, 1);
        img.set_pixel(0, 0, [1, 2, 3, 4]);
        assert_eq!(decode(&encode(&img)), img);
    }

    #[test]
    fn a_single_row_and_a_single_column_round_trip() {
        assert_eq!(decode(&encode(&sample(64, 1))), sample(64, 1));
        assert_eq!(decode(&encode(&sample(1, 64))), sample(1, 64));
    }

    /// A capture-sized image: the case that actually runs, and the one where a
    /// wrong stride or an overflowed length would show.
    #[test]
    fn a_window_sized_image_round_trips_and_compresses() {
        let img = sample(600, 400);
        let png = encode(&img);
        assert_eq!(decode(&png), img);
        assert!(
            png.len() < img.px.len() / 2,
            "a screenshot-shaped image should compress: {} vs {}",
            png.len(),
            img.px.len()
        );
    }

    /// Every filter, applied and undone, over rows that make each of them the
    /// natural choice.
    #[test]
    fn each_filter_undoes_itself() {
        let row: Vec<u8> = (0..40u8)
            .map(|i| i.wrapping_mul(7).wrapping_add(3))
            .collect();
        let up: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(11)).collect();
        for kind in [0u8, 1, 2, 3, 4] {
            let mut buf = Vec::new();
            filter_row(kind, &row, &up, 4, &mut buf);
            let mut back = buf.clone();
            unfilter(kind, &mut back, &up, 4);
            assert_eq!(back, row, "filter {kind}");
        }
    }

    #[test]
    fn writing_a_png_creates_its_directory() {
        let dir = std::env::temp_dir().join(format!(
            "uiharness-png-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("shot.png");
        write(&path, &sample(8, 8)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..8], &SIGNATURE);
        assert_eq!(decode(&bytes), sample(8, 8));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
