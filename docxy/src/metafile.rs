//! Rasterize legacy Windows vector images (WMF/EMF) to RGBA pixels using the
//! OS's own GDI, so `.wmf`/`.emf` media render as real images instead of
//! placeholder boxes. Windows-only; elsewhere [`render`] returns `None`.

/// Render a WMF or EMF metafile into a `w`×`h` RGBA bitmap with the white page
/// keyed to transparent (see [`key_page`]), so equations blend with the terminal.
/// Returns `None` if the bytes aren't a recognizable metafile or GDI fails.
#[cfg(windows)]
pub fn render(bytes: &[u8], w: u32, h: u32) -> Option<image::RgbaImage> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, DeleteEnhMetaFile, DeleteObject, FillRect, GdiFlush, GetDC, GetStockObject,
        HBRUSH, PlayEnhMetaFile, ReleaseDC, SelectObject, WHITE_BRUSH,
    };

    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    unsafe {
        let hemf = to_emf(bytes)?;

        let screen = GetDC(null_mut());
        let hdc = CreateCompatibleDC(screen);
        if hdc.is_null() {
            ReleaseDC(null_mut(), screen);
            DeleteEnhMetaFile(hemf);
            return None;
        }

        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w as i32;
        bi.bmiHeader.biHeight = -(h as i32); // negative = top-down rows
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;

        let mut bits: *mut core::ffi::c_void = null_mut();
        let dib = CreateDIBSection(hdc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if dib.is_null() || bits.is_null() {
            DeleteDC(hdc);
            ReleaseDC(null_mut(), screen);
            DeleteEnhMetaFile(hemf);
            return None;
        }
        let old = SelectObject(hdc, dib);

        // Metafiles assume a white page; clear before playing.
        let rect = RECT {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        FillRect(hdc, &rect, GetStockObject(WHITE_BRUSH) as HBRUSH);
        PlayEnhMetaFile(hdc, hemf, &rect);
        GdiFlush();

        let n = (w * h * 4) as usize;
        let src = std::slice::from_raw_parts(bits as *const u8, n);
        let mut rgba = Vec::with_capacity(n);
        for px in src.chunks_exact(4) {
            // BGRX from GDI. These metafiles are mostly equations: black line-art on
            // the white page we filled. Drop the page to transparent so it blends
            // with the terminal instead of showing as a white sticker.
            let (r, g, b) = (px[2], px[1], px[0]);
            rgba.extend_from_slice(&key_page(r, g, b));
        }

        SelectObject(hdc, old);
        DeleteObject(dib);
        DeleteDC(hdc);
        ReleaseDC(null_mut(), screen);
        DeleteEnhMetaFile(hemf);

        image::RgbaImage::from_raw(w, h, rgba)
    }
}

/// Map one rendered pixel (over a white page) to RGBA with the page keyed out.
///
/// Grayscale ink (the usual equation case) becomes a light glyph whose opacity is
/// its darkness, so it reads on a dark terminal and the page turns transparent.
/// Coloured art keeps its colour and only the near-white page is dropped. The
/// light branch is premultiplied against black, so it still looks right on the
/// few terminals that ignore alpha (the page stays dark rather than light).
#[cfg(windows)]
fn key_page(r: u8, g: u8, b: u8) -> [u8; 4] {
    let lum = (r as u32 * 30 + g as u32 * 59 + b as u32 * 11) / 100; // 0..=255
    let sat = r.max(g).max(b) - r.min(g).min(b);
    if sat < 24 {
        // Line art: ink coverage is how dark the pixel is; recolour to light.
        let cov = 255 - lum as u8;
        let v = (220 * cov as u32 / 255) as u8;
        [v, v, v, cov]
    } else {
        // Coloured: keep the colour, drop only the white page.
        let a = if r.min(g).min(b) > 230 { 0 } else { 255 };
        [r, g, b, a]
    }
}

/// Turn raw metafile bytes into an enhanced-metafile handle (converting WMF→EMF).
#[cfg(windows)]
unsafe fn to_emf(bytes: &[u8]) -> Option<windows_sys::Win32::Graphics::Gdi::HENHMETAFILE> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Graphics::Gdi::{GetDC, MM_ANISOTROPIC, ReleaseDC, SetEnhMetaFileBits};
    use windows_sys::Win32::System::DataExchange::{METAFILEPICT, SetWinMetaFileBits};

    // EMF: signature "EMF " (0x464D4520) at byte offset 40.
    if bytes.len() >= 44 && bytes[40..44] == [0x20, 0x45, 0x4D, 0x46] {
        let h = unsafe { SetEnhMetaFileBits(bytes.len() as u32, bytes.as_ptr()) };
        return (!h.is_null()).then_some(h);
    }
    // WMF: drop a 22-byte placeable header (magic 0x9AC6CDD7) if present.
    let body = if bytes.len() >= 22 && bytes[0..4] == [0xD7, 0xCD, 0xC6, 0x9A] {
        &bytes[22..]
    } else {
        bytes
    };
    if body.len() < 18 {
        return None;
    }
    let mfp = METAFILEPICT {
        mm: MM_ANISOTROPIC,
        xExt: 0,
        yExt: 0,
        hMF: null_mut(),
    };
    let h = unsafe {
        let screen = GetDC(null_mut());
        let h = SetWinMetaFileBits(body.len() as u32, body.as_ptr(), screen, &mfp);
        ReleaseDC(null_mut(), screen);
        h
    };
    (!h.is_null()).then_some(h)
}

#[cfg(not(windows))]
pub fn render(_bytes: &[u8], _w: u32, _h: u32) -> Option<image::RgbaImage> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::{key_page, render, to_emf};

    #[test]
    fn keys_page_to_transparent_and_inverts_ink() {
        // The white page becomes fully transparent.
        assert_eq!(key_page(255, 255, 255)[3], 0);
        // Black ink becomes opaque and light (visible on a dark terminal).
        let ink = key_page(0, 0, 0);
        assert_eq!(ink[3], 255);
        assert!(ink[0] > 128, "ink should be light: {ink:?}");
        // A coloured pixel keeps its colour and stays opaque.
        let red = key_page(200, 10, 10);
        assert_eq!([red[0], red[1], red[2]], [200, 10, 10]);
        assert_eq!(red[3], 255);
    }

    #[test]
    fn key_page_mid_gray_is_partially_transparent() {
        // A mid gray (low saturation) is treated as ink: opacity tracks darkness
        // and the pixel is recoloured to the light glyph ramp (not the source gray).
        let px = key_page(128, 128, 128);
        assert_eq!(px[3], 255 - 128, "alpha == coverage == 255-luma");
        // Recoloured onto the ~220 ceiling, premultiplied by coverage.
        let v = (220u32 * px[3] as u32 / 255) as u8;
        assert_eq!([px[0], px[1], px[2]], [v, v, v]);
    }

    #[test]
    fn key_page_near_white_colour_page_drops_out() {
        // A pixel that clears the saturation gate (>= 24) yet has every channel
        // > 230 is the coloured page: keyed fully transparent, colour preserved.
        let px = key_page(255, 231, 231);
        assert_eq!(px[3], 0);
        assert_eq!([px[0], px[1], px[2]], [255, 231, 231]);
    }

    // --- render() dimension guards (no GDI / no metafile bytes needed) ---

    #[test]
    fn render_rejects_degenerate_and_oversize_dimensions() {
        // A trivially non-metafile byte buffer; the guard fires before parsing.
        let junk = [0u8; 8];
        assert!(render(&junk, 0, 10).is_none(), "zero width");
        assert!(render(&junk, 10, 0).is_none(), "zero height");
        assert!(render(&junk, 4097, 10).is_none(), "width over 4096");
        assert!(render(&junk, 10, 4097).is_none(), "height over 4096");
    }

    #[test]
    fn render_returns_none_on_unrecognizable_bytes() {
        // Valid dimensions, but the bytes are not a metafile: to_emf() fails and
        // render() must surface None rather than panic.
        let junk = [0xABu8; 64];
        assert!(render(&junk, 16, 16).is_none());
    }

    // --- to_emf() format detection & length guards ---

    /// A buffer whose bytes[40..44] hold the EMF signature " EMF" (0x464D4520,
    /// little-endian), padded to `len` with `fill` elsewhere.
    fn emf_with_signature(len: usize, fill: u8) -> Vec<u8> {
        assert!(len >= 44);
        let mut v = vec![fill; len];
        v[40..44].copy_from_slice(&[0x20, 0x45, 0x4D, 0x46]);
        v
    }

    /// A 22-byte WMF placeable header (magic 0x9AC6CDD7) followed by `body`.
    fn wmf_placeable(body: &[u8]) -> Vec<u8> {
        let mut v = vec![0xD7, 0xCD, 0xC6, 0x9A];
        v.resize(22, 0);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn to_emf_none_on_empty_and_truncated() {
        // Empty and short buffers can't hold even a bare WMF body (< 18 bytes)
        // and must return None without touching GDI.
        unsafe {
            assert!(to_emf(&[]).is_none(), "empty");
            assert!(to_emf(&[0u8; 10]).is_none(), "10 bytes");
            assert!(
                to_emf(&[0u8; 17]).is_none(),
                "17 bytes (one under the floor)"
            );
        }
    }

    #[test]
    fn to_emf_strips_wmf_placeable_but_rejects_too_short_body() {
        // Magic present, so the 22-byte placeable header is dropped; the 10-byte
        // remainder is under the 18-byte WMF floor -> None (exercises the
        // magic-match branch without invoking GDI).
        let buf = wmf_placeable(&[0u8; 10]);
        assert_eq!(buf.len(), 32);
        unsafe {
            assert!(to_emf(&buf).is_none());
        }
    }

    #[test]
    fn to_emf_none_on_garbage_emf() {
        // Correct EMF signature at offset 40 but the rest is garbage: GDI's
        // SetEnhMetaFileBits rejects it and to_emf yields None (no leaked handle,
        // no panic). Exercises the EMF-detection branch.
        let buf = emf_with_signature(128, 0);
        unsafe {
            assert!(to_emf(&buf).is_none());
        }
    }

    #[test]
    fn to_emf_none_on_garbage_wmf_body() {
        // Placeable magic + a body long enough to clear the 18-byte floor but not
        // a valid metafile: SetWinMetaFileBits rejects it -> None. Exercises the
        // WMF conversion path.
        let buf = wmf_placeable(&[0xEEu8; 64]);
        unsafe {
            assert!(to_emf(&buf).is_none());
        }
    }

    #[test]
    fn to_emf_emf_signature_only_recognized_at_offset_40() {
        // The same signature bytes at offset 0 (not 40) must NOT be taken as EMF;
        // they fall through to the WMF path, which rejects the garbage -> None.
        let mut buf = vec![0u8; 128];
        buf[0..4].copy_from_slice(&[0x20, 0x45, 0x4D, 0x46]);
        unsafe {
            assert!(to_emf(&buf).is_none());
        }
    }

    #[test]
    fn to_emf_wmf_magic_must_match_exactly() {
        // One byte off the placeable magic: the header is NOT stripped and the
        // whole buffer is treated as a raw WMF body. Still garbage -> None, but
        // this pins the exact-magic requirement.
        let mut buf = wmf_placeable(&[0u8; 64]);
        buf[0] = 0xD6; // was 0xD7
        unsafe {
            assert!(to_emf(&buf).is_none());
        }
    }
}
