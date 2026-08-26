//! Pixels, and the arithmetic of cutting a region out of them.
//!
//! Everything here is pure: an [`Image`] is a flat RGBA buffer, and the only
//! decisions are how a rectangle intersects one. That matters because the
//! rectangles come from another process — the app reports where it laid a
//! region out, in desktop coordinates — and by the time they arrive the window
//! may have moved, been resized, or slid partly off the screen. A crop that
//! panicked on any of those would take the harness down instead of failing the
//! test it was taking evidence for.

/// A rectangle in whole pixels. Signed origin because a window on a monitor to
/// the left of the primary one lives at negative desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectPx {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl RectPx {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> RectPx {
        RectPx { x, y, w, h }
    }

    /// The exclusive right edge.
    pub fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }

    /// The exclusive bottom edge.
    pub fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// The same rectangle measured from `origin` instead of from the desktop —
    /// what a screen rectangle becomes once the capture it will be cut out of
    /// is known.
    pub fn relative_to(&self, origin: (i32, i32)) -> RectPx {
        RectPx {
            x: self.x - origin.0,
            y: self.y - origin.1,
            ..*self
        }
    }
}

/// A rectangle clipped to a `w` x `h` image, or `None` when it lies entirely
/// outside one.
///
/// Clamping rather than refusing is deliberate for the partly-outside case: a
/// region that runs off the bottom of the window (a chart card scrolled half
/// out of view, say) still has pixels worth looking at, and the whole point of
/// the harness is that a failing test leaves evidence. Wholly outside is a
/// different thing — there is nothing to look at — so that one is `None` and
/// the caller reports it.
pub fn clamp_rect(r: RectPx, w: u32, h: u32) -> Option<RectPx> {
    let (iw, ih) = (w as i64, h as i64);
    let left = (r.x as i64).max(0);
    let top = (r.y as i64).max(0);
    let right = r.right().min(iw);
    let bottom = r.bottom().min(ih);
    if right <= left || bottom <= top {
        return None;
    }
    Some(RectPx {
        x: left as i32,
        y: top as i32,
        w: (right - left) as u32,
        h: (bottom - top) as u32,
    })
}

/// An 8-bit RGBA image, row-major, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    /// `w * h * 4` bytes, R, G, B, A.
    pub px: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{})", self.w, self.h)
    }
}

impl Image {
    /// A transparent image of the given size.
    pub fn new(w: u32, h: u32) -> Image {
        Image {
            w,
            h,
            px: vec![0; w as usize * h as usize * 4],
        }
    }

    /// An image over an existing buffer, or `None` if the buffer is not exactly
    /// `w * h * 4` bytes.
    pub fn from_rgba(w: u32, h: u32, px: Vec<u8>) -> Option<Image> {
        (px.len() == w as usize * h as usize * 4).then_some(Image { w, h, px })
    }

    /// The pixel at `(x, y)`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        Some([self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]])
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        self.px[i..i + 4].copy_from_slice(&rgba);
    }

    /// One row of the image, as RGBA bytes.
    pub fn row(&self, y: u32) -> &[u8] {
        let stride = self.w as usize * 4;
        let start = y as usize * stride;
        &self.px[start..start + stride]
    }

    /// The part of `r` that is inside this image, as a new image. `Err` with a
    /// message naming both rectangles when `r` misses the image entirely —
    /// which is the answer a test needs to read, not a panic.
    pub fn crop(&self, r: RectPx) -> Result<Image, String> {
        let c = clamp_rect(r, self.w, self.h).ok_or_else(|| {
            format!(
                "the region {}x{} at ({},{}) is entirely outside the {}x{} capture",
                r.w, r.h, r.x, r.y, self.w, self.h
            )
        })?;
        let mut out = Image::new(c.w, c.h);
        let stride = self.w as usize * 4;
        for row in 0..c.h as usize {
            let src = (c.y as usize + row) * stride + c.x as usize * 4;
            let dst = row * c.w as usize * 4;
            out.px[dst..dst + c.w as usize * 4]
                .copy_from_slice(&self.px[src..src + c.w as usize * 4]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small image whose every pixel encodes its own coordinates, so a crop
    /// that took the right SIZE from the wrong PLACE still fails.
    fn ramp(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, [x as u8, y as u8, 0, 255]);
            }
        }
        img
    }

    #[test]
    fn a_rect_inside_the_image_crops_exactly() {
        let img = ramp(20, 10);
        let c = img.crop(RectPx::new(4, 3, 5, 2)).unwrap();
        assert_eq!((c.w, c.h), (5, 2));
        assert_eq!(c.pixel(0, 0), Some([4, 3, 0, 255]));
        assert_eq!(c.pixel(4, 1), Some([8, 4, 0, 255]));
        assert_eq!(c.pixel(5, 0), None, "past the crop's own edge");
    }

    #[test]
    fn the_whole_image_crops_to_itself() {
        let img = ramp(7, 5);
        assert_eq!(img.crop(RectPx::new(0, 0, 7, 5)).unwrap(), img);
    }

    /// The case the plan calls out: a region running off an edge clamps to what
    /// is there rather than panicking or reading past the buffer.
    #[test]
    fn a_rect_partly_outside_clamps_to_what_is_there() {
        let img = ramp(20, 10);
        // Off the bottom-right corner.
        let c = img.crop(RectPx::new(16, 8, 10, 10)).unwrap();
        assert_eq!((c.w, c.h), (4, 2));
        assert_eq!(c.pixel(0, 0), Some([16, 8, 0, 255]));
        // Off the top-left, with a negative origin.
        let c = img.crop(RectPx::new(-3, -4, 10, 10)).unwrap();
        assert_eq!((c.w, c.h), (7, 6));
        assert_eq!(c.pixel(0, 0), Some([0, 0, 0, 255]));
        // Wider and taller than the image on every side.
        let c = img.crop(RectPx::new(-100, -100, 1000, 1000)).unwrap();
        assert_eq!((c.w, c.h), (20, 10));
    }

    #[test]
    fn a_rect_entirely_outside_is_reported_not_cropped() {
        let img = ramp(20, 10);
        for r in [
            RectPx::new(20, 0, 5, 5), // past the right edge
            RectPx::new(0, 10, 5, 5), // past the bottom
            RectPx::new(-5, 0, 5, 5), // wholly left
            RectPx::new(0, -5, 5, 5), // wholly above
            RectPx::new(2, 2, 0, 4),  // zero width
            RectPx::new(2, 2, 4, 0),  // zero height
        ] {
            let e = img.crop(r).unwrap_err();
            assert!(e.contains("20x10"), "{e}");
            assert!(e.contains(&format!("({},{})", r.x, r.y)), "{e}");
        }
    }

    /// An extreme rectangle must not overflow into a wrapped-around one.
    #[test]
    fn an_absurd_rect_does_not_overflow() {
        let img = ramp(4, 4);
        // i32::MIN + u32::MAX overflows i32 but not the i64 the edges are
        // computed in, and lands just past zero — so this covers the image
        // rather than wrapping round to an empty or enormous rectangle.
        let c = img
            .crop(RectPx::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX))
            .unwrap();
        assert_eq!((c.w, c.h), (4, 4));
        let c = img.crop(RectPx::new(-1, -1, u32::MAX, u32::MAX)).unwrap();
        assert_eq!((c.w, c.h), (4, 4));
        // One further left and the whole rectangle really is off the image.
        assert!(img.crop(RectPx::new(i32::MIN, i32::MIN, 1, 1)).is_err());
        assert!(img.crop(RectPx::new(i32::MAX, 0, u32::MAX, 4)).is_err());
    }

    #[test]
    fn a_screen_rect_becomes_a_capture_rect_by_subtracting_the_origin() {
        let r = RectPx::new(1930, 250, 100, 40);
        assert_eq!(
            r.relative_to((1920, 200)),
            RectPx::new(10, 50, 100, 40),
            "the window's own top-left is the capture's (0,0)"
        );
        // A window off the left of the primary monitor.
        assert_eq!(
            RectPx::new(-1900, 30, 10, 10).relative_to((-1920, 0)),
            RectPx::new(20, 30, 10, 10)
        );
    }

    #[test]
    fn from_rgba_rejects_a_buffer_of_the_wrong_length() {
        assert!(Image::from_rgba(2, 2, vec![0; 16]).is_some());
        assert!(Image::from_rgba(2, 2, vec![0; 15]).is_none());
        assert!(Image::from_rgba(2, 2, vec![0; 17]).is_none());
    }
}
