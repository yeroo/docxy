//! Taking the pixels: find the test instance's window from its process id, and
//! read that window's contents into an [`Image`].
//!
//! ## Why this lives on the harness side
//!
//! gpui has no window-to-image readback in a shipping build.
//! `App::screen_capture_sources` is the screen-*sharing* source list (whole
//! displays), `to_image_data` renders SVG, and `Window::render_to_image` — the
//! one that sounds right — is behind `cfg(any(test, feature = "test-support"))`
//! and re-renders the scene to an offscreen texture rather than reading what
//! the compositor actually put on screen. So the app reports geometry and the
//! harness takes the picture.
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
}

impl std::fmt::Display for How {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            How::PrintWindow => write!(f, "PrintWindow"),
            How::Screen => write!(f, "screen copy (PrintWindow came back blank)"),
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
    img.px.chunks_exact(4).all(|p| p == first)
}

#[cfg(windows)]
mod win {
    use super::{Capture, How, Image, is_blank};
    use windows::Win32::Foundation::{BOOL, HANDLE, HWND, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, ReleaseDC, SRCCOPY, SelectObject, StretchBlt,
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
            let mut image = read_bgra(bits, w, h);
            if !printed || is_blank(&image) {
                // The GPU-rendered fallback: copy the same rectangle off the
                // screen. StretchBlt with matching sizes is a plain copy.
                let ok = StretchBlt(
                    mem.0, 0, 0, w, h, screen.0, rect.left, rect.top, w, h, SRCCOPY,
                )
                .as_bool();
                if ok {
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
        for p in src.chunks_exact(4) {
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
