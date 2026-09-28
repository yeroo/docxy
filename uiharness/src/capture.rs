//! Taking the pixels. On Windows: find the test instance's window from its
//! process id, and read that window's contents into an [`Image`]. On macOS: ask
//! the app to render itself offscreen, and read what it wrote
//! ([`offscreen_capture`]).
//!
//! ## Why this lives on the harness side, except on macOS
//!
//! gpui has no window-to-image readback in a shipping build.
//! `App::screen_capture_sources` is the screen-*sharing* source list (whole
//! displays), `to_image_data` renders SVG, and `Window::render_to_image` — the
//! one that sounds right — is behind `cfg(any(test, feature = "test-support"))`
//! and re-renders the scene to an offscreen texture rather than reading what
//! the compositor actually put on screen. So on Windows the app reports
//! geometry and the harness takes the picture.
//!
//! macOS is the exception, because there the harness *cannot* take it: the
//! window is never on screen, and reading another process's pixels needs Screen
//! Recording permission. A suite built with its `harness-capture` feature
//! renders offscreen on request instead — see `docs/ui-test-harness.md`.
//!
//! ## Why `PrintWindow`
//!
//! `PrintWindow` with `PW_RENDERFULLCONTENT` asks the window to draw itself
//! into a device context, which captures that window alone even when another
//! one is in front of it — a test can therefore run without owning the desktop.
//! Its weakness is the mirror image: a window drawn by the GPU (gpui uses
//! DirectX) sometimes comes back blank, because the flag only reaches
//! DWM-redirected content. When that happens [`capture_window`] falls back to
//! copying the same rectangle off the screen, which needs the window to be
//! visible and unobstructed but always shows exactly what is there. Which route
//! a capture took is on the [`Capture`] it returns, so a test can say so.

use crate::image::Image;

/// A window's pixels, and where its top-left corner sits on the desktop —
/// which is what turns a screen rectangle into a crop.
pub struct Capture {
    pub image: Image,
    /// The window rectangle's top-left, in physical desktop pixels.
    pub origin: (i32, i32),
    /// How the pixels were obtained, for a test to report.
    pub how: How,
}

/// Which route [`capture_window`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// `PrintWindow`: the window alone, occluded or not.
    PrintWindow,
    /// A copy off the screen, because `PrintWindow` came back blank. Whatever
    /// is in front of the window is in the picture.
    Screen,
    /// macOS: the app rendered its last drawn frame to an offscreen texture
    /// (the `capture` verb). Not what the compositor showed, and never on screen.
    Offscreen,
}

impl std::fmt::Display for How {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            How::PrintWindow => write!(f, "PrintWindow"),
            How::Screen => write!(f, "screen copy (PrintWindow came back blank)"),
            How::Offscreen => write!(f, "offscreen render"),
        }
    }
}

/// Whether an image is a single flat colour — the signature of a `PrintWindow`
/// that captured nothing. A real window has a title bar, a ribbon and a grid in
/// it, so this cannot be a false positive on anything the harness drives.
pub fn is_blank(img: &Image) -> bool {
    if img.px.len() < 4 {
        return true;
    }
    let first = &img.px[0..4];
    img.px.as_chunks::<4>().0.iter().all(|p| &p[..] == first)
}

#[cfg(windows)]
mod win {
    use super::{Capture, How, Image, is_blank};
    use windows::Win32::Foundation::{BOOL, HANDLE, HWND, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, ReleaseDC, SRCCOPY, SelectObject,
        StretchBlt,
    };
    use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };

    /// `PW_RENDERFULLCONTENT` — include content the window draws itself rather
    /// than only what it hands to GDI. The crate has `PW_CLIENTONLY` but not
    /// this one.
    const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

    /// The null window handle, which is how the Win32 calls here spell "the
    /// desktop".
    fn no_window() -> HWND {
        HWND(std::ptr::null_mut())
    }

    /// Tell Windows this process reads real pixels. Without it `GetWindowRect`
    /// hands back coordinates scaled for a 96-DPI fiction, and every crop on a
    /// 125%/150% display would be off by that ratio. Safe to call more than
    /// once; the second call simply fails.
    pub fn become_dpi_aware() {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    struct Find {
        pid: u32,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let find = unsafe { &mut *(lparam.0 as *mut Find) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // Top-level and visible only: tooltips and menus belong to the same
        // process and are owned windows, and one of them would otherwise be
        // picked ahead of the app.
        let owned = unsafe { GetWindow(hwnd, GW_OWNER) }
            .map(|h| !h.0.is_null())
            .unwrap_or(false);
        if pid == find.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() && !owned {
            let mut r = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok()
                && r.right - r.left > 1
                && r.bottom - r.top > 1
            {
                find.found.push(hwnd);
            }
        }
        TRUE
    }

    /// The visible top-level window belonging to `pid`.
    pub fn window_of_pid(pid: u32) -> Result<HWND, String> {
        let mut find = Find {
            pid,
            found: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut find as *mut Find as isize));
        }
        if find.found.is_empty() {
            return Err(format!(
                "process {pid} has no visible top-level window \
                 (is the harness instance still starting, or already gone?)"
            ));
        }
        // More than one is possible in principle; the largest is the app.
        let mut best = (find.found[0], -1i64);
        for &h in &find.found {
            let mut r = RECT::default();
            if unsafe { GetWindowRect(h, &mut r) }.is_ok() {
                let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                if area > best.1 {
                    best = (h, area);
                }
            }
        }
        Ok(best.0)
    }

    /// A GDI object freed when it goes out of scope, so an early `?` cannot
    /// leak a device context or a bitmap.
    struct Dc(HDC);
    impl Drop for Dc {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
    struct Bmp(HBITMAP);
    impl Drop for Bmp {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
    /// The desktop DC, which is released rather than deleted.
    struct ScreenDc(HDC);
    impl Drop for ScreenDc {
        fn drop(&mut self) {
            unsafe {
                ReleaseDC(no_window(), self.0);
            }
        }
    }

    /// Capture `hwnd` — the whole window, frame included, so the returned
    /// origin is `GetWindowRect`'s and a screen rectangle needs only a
    /// subtraction to become a crop.
    pub fn capture_window(hwnd: HWND) -> Result<Capture, String> {
        become_dpi_aware();
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|e| format!("GetWindowRect: {e}"))?;
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return Err(format!(
                "the window has no area ({w}x{h}); is it minimized?"
            ));
        }

        unsafe {
            let screen = ScreenDc(GetDC(no_window()));
            if screen.0.is_invalid() {
                return Err("GetDC failed: no desktop device context".to_string());
            }
            let mem = Dc(CreateCompatibleDC(screen.0));
            if mem.0.is_invalid() {
                return Err("CreateCompatibleDC failed".to_string());
            }
            let mut info = BITMAPINFO::default();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            // Negative height asks for a top-down bitmap, so row 0 is the top
            // one and no flip is needed on the way out.
            info.bmiHeader.biHeight = -h;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB.0;
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bmp = Bmp(CreateDIBSection(
                mem.0,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                HANDLE(std::ptr::null_mut()),
                0,
            )
            .map_err(|e| format!("CreateDIBSection: {e}"))?);
            if bits.is_null() {
                return Err("CreateDIBSection returned no pixels".to_string());
            }
            let old = SelectObject(mem.0, bmp.0);

            let printed = PrintWindow(hwnd, mem.0, PW_RENDERFULLCONTENT).as_bool();
            let mut how = How::PrintWindow;
            // ⚠️ GDI batches drawing into a DIB section, and reading the bits
            // through the pointer goes behind the batch's back. Without the
            // flush the buffer can still be its initial zeros, `is_blank` fires
            // on a capture that did work, and the run silently falls through to
            // the screen copy — which shows whatever window is in front. Machine
            // -dependent and intermittent, the worst shape for a test harness.
            let _ = GdiFlush();
            let mut image = read_bgra(bits, w, h);
            if !printed || is_blank(&image) {
                // The GPU-rendered fallback: copy the same rectangle off the
                // screen. StretchBlt with matching sizes is a plain copy.
                let ok = StretchBlt(
                    mem.0, 0, 0, w, h, screen.0, rect.left, rect.top, w, h, SRCCOPY,
                )
                .as_bool();
                if ok {
                    let _ = GdiFlush();
                    let from_screen = read_bgra(bits, w, h);
                    if !is_blank(&from_screen) {
                        image = from_screen;
                        how = How::Screen;
                    }
                }
            }
            SelectObject(mem.0, old);

            if is_blank(&image) {
                return Err(
                    "the capture came back blank from both PrintWindow and the screen; \
                     is the window minimized or on another desktop?"
                        .to_string(),
                );
            }
            Ok(Capture {
                image,
                origin: (rect.left, rect.top),
                how,
            })
        }
    }

    /// Copy a top-down 32-bit BGRA DIB into an RGBA [`Image`]. GDI leaves the
    /// alpha byte as whatever was drawn there, which for a window capture is
    /// usually zero — a PNG written with it would be invisible, so it is forced
    /// opaque.
    unsafe fn read_bgra(bits: *const core::ffi::c_void, w: i32, h: i32) -> Image {
        let n = w as usize * h as usize * 4;
        let src = unsafe { std::slice::from_raw_parts(bits as *const u8, n) };
        let mut px = Vec::with_capacity(n);
        for p in src.as_chunks::<4>().0 {
            px.extend_from_slice(&[p[2], p[1], p[0], 0xff]);
        }
        Image::from_rgba(w as u32, h as u32, px).expect("the DIB is exactly w*h*4 bytes")
    }

    /// Capture the window belonging to `pid`.
    pub fn capture_pid(pid: u32) -> Result<Capture, String> {
        become_dpi_aware();
        capture_window(window_of_pid(pid)?)
    }
}

#[cfg(windows)]
pub use win::{become_dpi_aware, capture_pid, capture_window, window_of_pid};

/// Off Windows there is nothing to capture with yet: `PrintWindow` is a Win32
/// call, and the plan defers a portable route until there is a second platform
/// to run the suite's tests on.
#[cfg(not(windows))]
pub fn capture_pid(_pid: u32) -> Result<Capture, String> {
    Err("window capture is implemented on Windows only".to_string())
}

#[cfg(not(windows))]
pub fn become_dpi_aware() {}

/// Turn the app's reply to the `capture` verb into a [`Capture`], reading the
/// pixel file it names through `read`.
///
/// This is how a macOS harness gets its pixels. There, nothing outside the app
/// can photograph a harness window without Screen Recording permission, and the
/// window is never on screen to be photographed anyway. So the app renders its
/// last drawn frame to an offscreen texture and writes the raw RGBA into its
/// own sandbox, and this side reads it back. From here on it is an ordinary
/// [`Capture`]: `content_origin` is the content's top-left in the same physical
/// desktop pixels `rect` answers in, so the existing crop — a region's rect
/// minus the capture's origin — lands on the right pixels unchanged.
///
/// ⚠️ The file travels beside the control channel rather than through it: a
/// full window is several megabytes of RGBA, which is no size for a JSON reply.
pub fn offscreen_capture<R>(reply: &ctlcore::json::Json, read: R) -> Result<Capture, String>
where
    R: FnOnce(&std::path::Path) -> std::io::Result<Vec<u8>>,
{
    use ctlcore::json::Json;
    let field = |j: &Json, k: &str| -> Result<f64, String> {
        j.get(k)
            .and_then(Json::as_f64)
            .ok_or_else(|| format!("the capture reply has no numeric '{k}': {reply}"))
    };
    let path = reply
        .get_str("path")
        .ok_or_else(|| format!("the capture reply has no 'path': {reply}"))?;
    let (w, h) = (
        field(reply, "width")? as u32,
        field(reply, "height")? as u32,
    );
    let origin = reply
        .get("content_origin")
        .ok_or_else(|| format!("the capture reply has no 'content_origin': {reply}"))?;
    let origin = (
        field(origin, "x")?.round() as i32,
        field(origin, "y")?.round() as i32,
    );
    let path = std::path::Path::new(path);
    let px = read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let got = px.len();
    let image = Image::from_rgba(w, h, px).ok_or_else(|| {
        format!(
            "{}: {got} bytes is not a {w}x{h} RGBA image; a torn or stale write \
             would shear every row of the crop",
            path.display()
        )
    })?;
    if is_blank(&image) {
        return Err(format!(
            "the offscreen render came back blank ({w}x{h}, one flat colour): the \
             drawable never received the scene, so there is nothing to assert on"
        ));
    }
    Ok(Capture {
        image,
        origin,
        how: How::Offscreen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_image_reads_as_blank() {
        assert!(is_blank(&Image::new(4, 4)), "all-zero is blank");
        let mut solid = Image::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                solid.set_pixel(x, y, [0, 0, 0, 255]);
            }
        }
        assert!(is_blank(&solid), "one flat colour is blank");
        assert!(is_blank(&Image::new(0, 0)), "nothing at all is blank");
    }

    #[test]
    fn an_image_with_any_variation_is_not_blank() {
        let mut img = Image::new(4, 4);
        img.set_pixel(2, 1, [1, 0, 0, 0]);
        assert!(!is_blank(&img), "one differing pixel is enough");
    }

    use ctlcore::json::Json;

    /// What the app answers for a 2x1 capture whose content starts at (10,20).
    fn reply(w: u32, h: u32) -> Json {
        Json::obj(vec![
            ("path", Json::Str("/sandbox/suite/capture/last.rgba".into())),
            ("width", Json::Num(w as f64)),
            ("height", Json::Num(h as f64)),
            ("scale", Json::Num(2.0)),
            ("frame", Json::Num(7.0)),
            (
                "content_origin",
                Json::obj(vec![("x", Json::Num(10.0)), ("y", Json::Num(20.0))]),
            ),
        ])
    }

    /// Two different pixels, so the image is not flat.
    fn two_px() -> Vec<u8> {
        vec![255, 0, 0, 255, 0, 0, 255, 255]
    }

    #[test]
    fn an_offscreen_reply_becomes_a_capture_at_the_content_origin() {
        let cap = offscreen_capture(&reply(2, 1), |p| {
            assert_eq!(p, std::path::Path::new("/sandbox/suite/capture/last.rgba"));
            Ok(two_px())
        })
        .unwrap();
        assert_eq!((cap.image.w, cap.image.h), (2, 1));
        assert_eq!(cap.image.px, two_px());
        // The origin is what turns a desktop rect into a crop of this image.
        assert_eq!(cap.origin, (10, 20));
        assert_eq!(cap.how, How::Offscreen);
    }

    /// A file of the wrong size is a torn or stale write, never a picture to
    /// crop: cropping it would silently shear every row.
    #[test]
    fn pixels_that_do_not_fill_the_stated_size_are_refused() {
        let e = offscreen_capture(&reply(2, 2), |_| Ok(two_px()))
            .err()
            .unwrap();
        assert!(e.contains("8 bytes") && e.contains("2x2"), "{e}");
    }

    /// The offscreen render's own failure mode: a drawable that never got the
    /// scene. A flat image must stop the step, not reach a probe that would
    /// then read "no border" off an empty picture and pass.
    #[test]
    fn a_blank_offscreen_render_is_refused() {
        let e = offscreen_capture(&reply(2, 1), |_| Ok(vec![0; 8]))
            .err()
            .unwrap();
        assert!(e.contains("blank"), "{e}");
    }

    #[test]
    fn a_reply_missing_a_field_names_the_field() {
        let mut r = reply(2, 1);
        if let Json::Obj(fields) = &mut r {
            fields.retain(|(k, _)| k != "content_origin");
        }
        let e = offscreen_capture(&r, |_| Ok(two_px())).err().unwrap();
        assert!(e.contains("content_origin"), "{e}");
    }

    #[test]
    fn a_pixel_file_that_cannot_be_read_says_which_file() {
        let e = offscreen_capture(&reply(2, 1), |_| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"))
        })
        .err()
        .unwrap();
        assert!(e.contains("last.rgba") && e.contains("gone"), "{e}");
    }

    #[test]
    fn how_says_which_route_a_capture_took() {
        assert_eq!(How::PrintWindow.to_string(), "PrintWindow");
        assert!(
            How::Screen
                .to_string()
                .contains("PrintWindow came back blank")
        );
    }
}
