//! The RTF beside a copy's text on the OS clipboard (#1074).
//!
//! gpui's clipboard carries text (and images) only, so a document copy adds
//! its RTF here, right after gpui wrote the text: macOS as `public.rtf` on
//! the general pasteboard, Windows as the registered "Rich Text Format".
//! Neither write clears the clipboard, so the text stays; gpui's next write
//! of anything clears both. Elsewhere (Linux: gpui has no multi-format
//! clipboard) both calls do nothing and copy and paste stay plain text.
//!
//! The RTF goes on only while the clipboard still holds the copy's own
//! text, checked again just before the write: another app may have copied
//! since gpui wrote it, and our RTF beside their text would paste our
//! formatting over their words.
//!
//! A failed write leaves the copy as plain text; a failed read pastes the
//! plain text, and so does an RTF past [`MAX_RTF`], read no further.

/// The largest RTF a paste reads off the clipboard. Word's RTF carries its
/// pictures as hex, so a page or two of them is megabytes; past this the
/// paste takes the plain text rather than copying it all to find out.
const MAX_RTF: usize = 64 << 20;

/// `bytes` copied into a new vector, or `None` past [`MAX_RTF`] or when the
/// memory cannot be had.
#[cfg_attr(not(windows), allow(dead_code))]
fn bounded_copy(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > MAX_RTF {
        return None;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len()).ok()?;
    out.extend_from_slice(bytes);
    Some(out)
}

/// Add `rtf` to the copy on the clipboard, if it still holds `text`.
pub(crate) fn write_rtf(rtf: String, text: &str) {
    imp::write_rtf(&rtf, text);
}

/// Whether the clipboard's text `held` is the copy's `text`, line endings
/// compared loosely (the OS may hand CRLF back).
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn same_text(held: &str, text: &str) -> bool {
    held.replace("\r\n", "\n") == text.replace("\r\n", "\n")
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

    pub(super) fn write_rtf(rtf: &str, text: &str) {
        let board = NSPasteboard::generalPasteboard();
        let held = board.stringForType(&NSString::from_str("public.utf8-plain-text"));
        if !held.is_some_and(|held| super::same_text(&held.to_string(), text)) {
            return;
        }
        let kind = NSString::from_str(RTF);
        // SAFETY: no owner object; the type is a plain NSString.
        unsafe { board.addTypes_owner(&NSArray::from_slice(&[&*kind]), None) };
        board.setData_forType(Some(&NSData::with_bytes(rtf.as_bytes())), &kind);
    }

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        let board = NSPasteboard::generalPasteboard();
        let data = board.dataForType(&NSString::from_str(RTF))?;
        // Measured before it is copied.
        let len = data.len();
        (len > 0 && len <= super::MAX_RTF).then(|| data.to_vec())
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
    const CF_UNICODETEXT: u32 = 13;

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

    /// The clipboard's Unicode text; the clipboard must be open.
    fn held_text() -> Option<String> {
        // SAFETY: the handle belongs to the open clipboard; it is read only
        // while locked, within the size the allocation reports.
        unsafe {
            let mem = GetClipboardData(CF_UNICODETEXT);
            if mem.is_null() {
                return None;
            }
            let ptr = GlobalLock(mem).cast::<u16>();
            if ptr.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(ptr, GlobalSize(mem) / 2);
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            let text = String::from_utf16_lossy(&units[..end]);
            GlobalUnlock(mem);
            Some(text)
        }
    }

    pub(super) fn write_rtf(rtf: &str, text: &str) {
        let Some(format) = format() else {
            return;
        };
        let Some(_open) = Open::new() else {
            return;
        };
        if !held_text().is_some_and(|held| super::same_text(&held, text)) {
            return;
        }
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
        unsafe {
            let mem = GetClipboardData(format);
            if mem.is_null() {
                return None;
            }
            let ptr = GlobalLock(mem).cast::<u8>();
            if ptr.is_null() {
                return None;
            }
            let block = std::slice::from_raw_parts(ptr, GlobalSize(mem));
            // NUL-terminated, and may be padded past it; the RTF ends at the
            // NUL (or, without one, where MAX_RTF stops the copy).
            let end = block
                .iter()
                .take(super::MAX_RTF + 1)
                .position(|&b| b == 0)
                .unwrap_or(block.len());
            let bytes = super::bounded_copy(&block[..end]);
            GlobalUnlock(mem);
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RTF, bounded_copy, same_text};

    #[test]
    fn the_rtf_goes_only_beside_its_own_text() {
        assert!(same_text("a\r\nb", "a\nb"));
        assert!(same_text("bold", "bold"));
        assert!(!same_text("theirs", "bold"));
    }

    #[test]
    fn a_read_is_bounded() {
        assert_eq!(
            bounded_copy(b"{\\rtf1 x}").as_deref(),
            Some(&b"{\\rtf1 x}"[..])
        );
        assert_eq!(bounded_copy(b""), None);
        assert_eq!(bounded_copy(&vec![b' '; MAX_RTF + 1]), None);
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub(super) fn write_rtf(_rtf: &str, _text: &str) {}

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        None
    }
}
