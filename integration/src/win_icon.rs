//! A window's own icon from a plugin's `.ico` bytes. On Windows a window
//! takes its class's icon, the exe's first icon resource, until it is sent
//! `WM_SETICON`; Alt-Tab and the taskbar button follow that message.

/// The image in an `.ico` file that best fits `size` pixels: the smallest at
/// least that wide, else the widest, the deeper colour on a tie. `None` when
/// the bytes are not an icon file or an entry points outside them.
pub fn pick_image(ico: &[u8], size: u32) -> Option<&[u8]> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(ico.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(ico.get(at..at + 4)?.try_into().ok()?))
    };
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = usize::from(u16_at(4)?);
    let mut images = Vec::with_capacity(count);
    for i in 0..count {
        let entry = 6 + 16 * i;
        let width = match *ico.get(entry)? {
            0 => 256,
            w => u32::from(w),
        };
        let bits = u16_at(entry + 6)?;
        let len = usize::try_from(u32_at(entry + 8)?).ok()?;
        let offset = usize::try_from(u32_at(entry + 12)?).ok()?;
        let bytes = ico.get(offset..offset.checked_add(len)?)?;
        if bytes.is_empty() {
            return None;
        }
        images.push((width, bits, bytes));
    }
    let fits = images
        .iter()
        .filter(|(width, _, _)| *width >= size)
        .min_by_key(|(width, bits, _)| (*width, std::cmp::Reverse(*bits)));
    let best = fits.or_else(|| images.iter().max_by_key(|(width, bits, _)| (*width, *bits)))?;
    Some(best.2)
}

/// Give the window `hwnd` (a Win32 `HWND`, as `raw-window-handle` gives it)
/// the big and small images of `ico`.
#[cfg(windows)]
pub fn set_window_icon(hwnd: isize, ico: &[u8]) -> Result<(), String> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, GetSystemMetrics, ICON_BIG, ICON_SMALL, LR_DEFAULTCOLOR,
        SM_CXICON, SM_CXSMICON, SendMessageW, WM_SETICON,
    };
    // The version `CreateIconFromResourceEx` takes for icon images.
    const ICON_FORMAT: u32 = 0x0003_0000;
    for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
        // SAFETY: reads a system metric.
        let size = unsafe { GetSystemMetrics(metric) }.max(16);
        let image = pick_image(ico, size as u32).ok_or("the icon is not an .ico file")?;
        let len = u32::try_from(image.len()).map_err(|_| "the icon image is too large")?;
        // SAFETY: `image` is `len` readable bytes of one icon image.
        let icon = unsafe {
            CreateIconFromResourceEx(
                image.as_ptr(),
                len,
                1,
                ICON_FORMAT,
                size,
                size,
                LR_DEFAULTCOLOR,
            )
        };
        if icon.is_null() {
            return Err(format!(
                "Windows refused the {size} px icon image: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: a message with an icon handle; an `hwnd` that is no window
        // makes the call fail, nothing more.
        unsafe { SendMessageW(hwnd as HWND, WM_SETICON, kind as usize, icon as isize) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `.ico` file of one `size`-pixel, 32-bit image per entry of `sizes`.
    fn ico(sizes: &[u32]) -> Vec<u8> {
        let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bitmap(s)).collect();
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len();
        for (size, image) in sizes.iter().zip(&images) {
            let side = if *size >= 256 { 0 } else { *size as u8 };
            out.extend_from_slice(&[side, side, 0, 0]);
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&(image.len() as u32).to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += image.len();
        }
        for image in images {
            out.extend_from_slice(&image);
        }
        out
    }

    /// A 32-bit icon image: its header, the colour rows, the mask rows.
    fn bitmap(size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for field in [40u32, size, size * 2] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        let pixels = (size * size) as usize;
        out.extend(std::iter::repeat_n([0x40, 0x80, 0xC0, 0xFF], pixels).flatten());
        let mask_row = size.div_ceil(32) as usize * 4;
        out.extend(std::iter::repeat_n(0u8, mask_row * size as usize));
        out
    }

    #[test]
    fn the_smallest_image_that_fits_is_picked() {
        let file = ico(&[16, 48, 32, 256]);
        let width = |image: &[u8]| u32::from_le_bytes(image[4..8].try_into().unwrap());
        assert_eq!(pick_image(&file, 16).map(width), Some(16));
        assert_eq!(pick_image(&file, 20).map(width), Some(32));
        assert_eq!(pick_image(&file, 32).map(width), Some(32));
        assert_eq!(pick_image(&file, 300).map(width), Some(256));
        assert_eq!(pick_image(&ico(&[16]), 32).map(width), Some(16));
    }

    #[test]
    fn a_damaged_file_gives_no_image() {
        let good = ico(&[16]);
        assert!(pick_image(&good, 16).is_some());
        assert_eq!(pick_image(&[], 16), None);
        let mut cursor = good.clone();
        cursor[2] = 2;
        assert_eq!(pick_image(&cursor, 16), None);
        let mut outside = good.clone();
        outside[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(pick_image(&outside, 16), None);
        assert_eq!(pick_image(&good[..good.len() - 1], 16), None);
        assert_eq!(pick_image(&good[..20], 16), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_window_takes_the_icon() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, ICON_BIG, ICON_SMALL, SendMessageW, WM_GETICON,
            WS_OVERLAPPED,
        };
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: a hidden window of a system class, destroyed below.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "{}", std::io::Error::last_os_error());
        let icon = |kind: u32| {
            // SAFETY: asks the window for its icon handle.
            unsafe { SendMessageW(hwnd, WM_GETICON, kind as usize, 0) }
        };
        assert_eq!(icon(ICON_BIG), 0);
        let result = set_window_icon(hwnd as isize, &ico(&[16, 32, 48]));
        let (big, small) = (icon(ICON_BIG), icon(ICON_SMALL));
        // SAFETY: the window made above.
        unsafe { DestroyWindow(hwnd) };
        assert_eq!(result, Ok(()));
        assert_ne!(big, 0);
        assert_ne!(small, 0);
        assert_eq!(
            set_window_icon(0, b"not an icon"),
            Err("the icon is not an .ico file".into())
        );
    }
}
