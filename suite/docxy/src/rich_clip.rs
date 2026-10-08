//! The RTF beside a copy's text on the OS clipboard (#1074).
//!
//! gpui's clipboard carries text (and images) only, so a document copy adds
//! its RTF here, right after gpui wrote the text: macOS as `public.rtf` on
//! the general pasteboard, Windows as the registered "Rich Text Format".
//! Neither write clears the clipboard, so the text stays; gpui's next write
//! of anything clears both. Elsewhere (Linux: gpui has no multi-format
//! clipboard) both calls do nothing and copy and paste stay plain text.
//!
//! A failed write leaves the copy as plain text; a failed read pastes the
//! plain text.

/// Add `rtf` to the copy on the clipboard.
pub(crate) fn write_rtf(rtf: String) {
    imp::write_rtf(&rtf);
}

/// The RTF on the clipboard, if any.
pub(crate) fn read_rtf() -> Option<Vec<u8>> {
    imp::read_rtf()
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSData, NSString};

    /// `NSPasteboardTypeRTF`.
    const RTF: &str = "public.rtf";

    pub(super) fn write_rtf(rtf: &str) {
        let board = NSPasteboard::generalPasteboard();
        let kind = NSString::from_str(RTF);
        // SAFETY: no owner object; the type is a plain NSString.
        unsafe { board.addTypes_owner(&NSArray::from_slice(&[&*kind]), None) };
        board.setData_forType(Some(&NSData::with_bytes(rtf.as_bytes())), &kind);
    }

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        let board = NSPasteboard::generalPasteboard();
        let data = board.dataForType(&NSString::from_str(RTF))?;
        Some(data.to_vec()).filter(|bytes| !bytes.is_empty())
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
        fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
        fn GetClipboardData(format: u32) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalFree(mem: *mut c_void) -> *mut c_void;
        fn GlobalLock(mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(mem: *mut c_void) -> i32;
        fn GlobalSize(mem: *mut c_void) -> usize;
    }

    const GMEM_MOVEABLE: u32 = 0x0002;

    /// The registered "Rich Text Format" clipboard format, `None` if
    /// registering failed.
    fn format() -> Option<u32> {
        let name: Vec<u16> = "Rich Text Format".encode_utf16().chain([0]).collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
        let id = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        (id != 0).then_some(id)
    }

    /// The clipboard, open until dropped. Another process (or gpui's own
    /// read a moment ago) may hold it, so an open is tried a few times.
    struct Open;

    impl Open {
        fn new() -> Option<Self> {
            for attempt in 0..5 {
                // SAFETY: no owner window; the clipboard is closed on drop.
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Some(Open);
                }
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            None
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: opened by `Open::new`.
            unsafe { CloseClipboard() };
        }
    }

    pub(super) fn write_rtf(rtf: &str) {
        let Some(format) = format() else {
            return;
        };
        let Some(_open) = Open::new() else {
            return;
        };
        let bytes = rtf.as_bytes();
        // SAFETY: a moveable block one byte longer than the RTF, filled and
        // NUL-terminated while locked; the clipboard owns it once
        // SetClipboardData takes it, else it is freed here.
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len() + 1);
            if mem.is_null() {
                return;
            }
            let ptr = GlobalLock(mem).cast::<u8>();
            if ptr.is_null() {
                GlobalFree(mem);
                return;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            *ptr.add(bytes.len()) = 0;
            GlobalUnlock(mem);
            if SetClipboardData(format, mem).is_null() {
                GlobalFree(mem);
            }
        }
    }

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        let format = format()?;
        let _open = Open::new()?;
        // SAFETY: the handle belongs to the open clipboard; it is read only
        // while locked, within the size the allocation reports.
        let mut bytes = unsafe {
            let mem = GetClipboardData(format);
            if mem.is_null() {
                return None;
            }
            let ptr = GlobalLock(mem).cast::<u8>();
            if ptr.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(ptr, GlobalSize(mem)).to_vec();
            GlobalUnlock(mem);
            bytes
        };
        // The block is NUL-terminated and may be padded past it.
        if let Some(end) = bytes.iter().position(|&b| b == 0) {
            bytes.truncate(end);
        }
        Some(bytes).filter(|bytes| !bytes.is_empty())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub(super) fn write_rtf(_rtf: &str) {}

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        None
    }
}
