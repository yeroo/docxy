//! The RTF (and HTML) beside a copy's text on the OS clipboard (#1074, #1248).
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
//! A sheet copy adds an HTML table the same way (#1248): macOS `public.html`,
//! Windows the registered "HTML Format" (CF_HTML: a `Version:0.9` header of
//! byte offsets, then the page around the table), checked and bounded like
//! the RTF.
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

/// Add `html` (a fragment: the table) to the copy on the clipboard, if it
/// still holds `text` (#1248).
pub(crate) fn write_html(html: String, text: &str) {
    imp::write_html(&html, text);
}

/// The HTML table fragment on the clipboard, if any.
pub(crate) fn read_html() -> Option<String> {
    let bytes = imp::read_html()?;
    html_fragment(&bytes)
}

/// The meta that tells a reader the page is UTF-8: a clipboard HTML has no
/// other way to say so, and readers assume a Windows code page without it.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
const CHARSET: &str = "<meta charset=\"utf-8\">";

/// `fragment` as a CF_HTML clipboard payload (Windows' "HTML Format"): the
/// `Version:0.9` header with the byte offsets (UTF-8, ten digits, so the
/// header's length does not depend on them) of the page, and of the fragment
/// between its `<!--StartFragment-->` markers.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn cf_html(fragment: &str) -> String {
    const HEADER: usize = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
    let before = format!("<html><head>{CHARSET}</head><body>\r\n<!--StartFragment-->");
    let after = "<!--EndFragment-->\r\n</body>\r\n</html>";
    let start_fragment = HEADER + before.len();
    let end_fragment = start_fragment + fragment.len();
    let end = end_fragment + after.len();
    format!(
        "Version:0.9\r\nStartHTML:{HEADER:010}\r\nEndHTML:{end:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{before}{fragment}{after}"
    )
}

/// `fragment` as the macOS `public.html` page.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn html_page(fragment: &str) -> String {
    format!("<html><head>{CHARSET}</head><body>{fragment}</body></html>")
}

/// The HTML an `HTML Format` / `public.html` payload carries: the fragment
/// its CF_HTML header names (when the offsets fit the payload, cut it on
/// character boundaries and it holds the `<table`), else the whole page, from
/// its first tag on (Excel's fragment starts inside the table, after its
/// `<table>` tag, so the page is the one that has it). `None` for an empty
/// payload.
fn html_fragment(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let offset = |key: &str| -> Option<usize> {
        let rest = &head[head.find(key)? + key.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    if let (Some(start), Some(end)) = (offset("StartFragment:"), offset("EndFragment:")) {
        if start <= end && end <= bytes.len() {
            if let Ok(fragment) = std::str::from_utf8(&bytes[start..end]) {
                if fragment.to_ascii_lowercase().contains("<table") {
                    return Some(fragment.to_string());
                }
            }
        }
    }
    let page = String::from_utf8_lossy(bytes);
    Some(page[page.find('<').unwrap_or(0)..].to_string())
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSData, NSString};

    /// `NSPasteboardTypeRTF`.
    const RTF: &str = "public.rtf";

    /// `NSPasteboardTypeHTML`.
    const HTML: &str = "public.html";

    fn write(kind: &str, bytes: &[u8], text: &str) {
        let board = NSPasteboard::generalPasteboard();
        let held = board.stringForType(&NSString::from_str("public.utf8-plain-text"));
        if !held.is_some_and(|held| super::same_text(&held.to_string(), text)) {
            return;
        }
        let kind = NSString::from_str(kind);
        // SAFETY: no owner object; the type is a plain NSString.
        unsafe { board.addTypes_owner(&NSArray::from_slice(&[&*kind]), None) };
        board.setData_forType(Some(&NSData::with_bytes(bytes)), &kind);
    }

    fn read(kind: &str) -> Option<Vec<u8>> {
        let board = NSPasteboard::generalPasteboard();
        let data = board.dataForType(&NSString::from_str(kind))?;
        // Measured before it is copied.
        let len = data.len();
        (len > 0 && len <= super::MAX_RTF).then(|| data.to_vec())
    }

    pub(super) fn write_rtf(rtf: &str, text: &str) {
        write(RTF, rtf.as_bytes(), text);
    }

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        read(RTF)
    }

    pub(super) fn write_html(html: &str, text: &str) {
        write(HTML, super::html_page(html).as_bytes(), text);
    }

    pub(super) fn read_html() -> Option<Vec<u8>> {
        read(HTML)
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

    /// The registered clipboard format `name`, `None` if registering failed.
    fn format(name: &str) -> Option<u32> {
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
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
        write("Rich Text Format", rtf.as_bytes(), text);
    }

    pub(super) fn read_rtf() -> Option<Vec<u8>> {
        read("Rich Text Format")
    }

    pub(super) fn write_html(html: &str, text: &str) {
        write("HTML Format", super::cf_html(html).as_bytes(), text);
    }

    pub(super) fn read_html() -> Option<Vec<u8>> {
        read("HTML Format")
    }

    fn write(name: &str, bytes: &[u8], text: &str) {
        let Some(format) = format(name) else {
            return;
        };
        let Some(_open) = Open::new() else {
            return;
        };
        if !held_text().is_some_and(|held| super::same_text(&held, text)) {
            return;
        }
        // SAFETY: a moveable block one byte longer than the payload, filled and
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

    fn read(name: &str) -> Option<Vec<u8>> {
        let format = format(name)?;
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
            // NUL-terminated, and may be padded past it; the payload ends at the
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
    use super::{MAX_RTF, bounded_copy, cf_html, html_fragment, html_page, same_text};

    #[test]
    fn the_rtf_goes_only_beside_its_own_text() {
        assert!(same_text("a\r\nb", "a\nb"));
        assert!(same_text("bold", "bold"));
        assert!(!same_text("theirs", "bold"));
    }

    #[test]
    fn cf_html_offsets_cut_the_fragment_out() {
        let fragment = "<table><tr><td>caf\u{e9} \u{20ac}</td></tr></table>";
        let payload = cf_html(fragment);
        let bytes = payload.as_bytes();
        let field = |key: &str| -> usize {
            let at = payload.find(key).unwrap() + key.len();
            payload[at..at + 10].parse().unwrap()
        };
        assert!(payload.starts_with("Version:0.9\r\nStartHTML:"));
        assert_eq!(field("EndHTML:"), bytes.len());
        assert_eq!(
            &payload[field("StartHTML:")..field("StartHTML:") + 6],
            "<html>"
        );
        assert_eq!(
            &bytes[field("StartFragment:")..field("EndFragment:")],
            fragment.as_bytes()
        );
        assert_eq!(html_fragment(bytes).as_deref(), Some(fragment));
    }

    #[test]
    fn a_foreign_payload_is_read_whole_and_a_bad_header_falls_back() {
        // No header (macOS `public.html`, a browser's page).
        let page = html_page("<table><tr><td>x</td></tr></table>");
        assert_eq!(
            html_fragment(page.as_bytes()).as_deref(),
            Some(page.as_str())
        );
        // Offsets past the payload, or inside a character.
        let bad = "Version:0.9\r\nStartFragment:0000000010\r\nEndFragment:0000009999\r\n<table>";
        assert_eq!(html_fragment(bad.as_bytes()).as_deref(), Some("<table>"));
        let split = "StartFragment:0000000051\r\nEndFragment:0000000052\r\n<p>\u{e9}";
        assert_eq!(
            html_fragment(split.as_bytes()).as_deref(),
            Some("<p>\u{e9}")
        );
        assert_eq!(html_fragment(b""), None);
    }

    /// Excel's CF_HTML fragment starts after the opening `<table>` tag, so the
    /// table's attributes and the page around it are what the paste needs.
    #[test]
    fn a_fragment_inside_the_table_reads_the_whole_page() {
        let page = "<html><body><table x=1>\r\n<!--StartFragment--><tr><td>a</td></tr><!--EndFragment--></table></body></html>";
        let start = page.find("<tr>").unwrap();
        let end = page.find("<!--EndFragment").unwrap();
        let payload = format!(
            "Version:0.9\r\nStartHTML:0000000105\r\nEndHTML:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n{page}",
            105 + page.len(),
            105 + start,
            105 + end
        );
        let got = html_fragment(payload.as_bytes()).unwrap();
        assert!(
            got.starts_with("<html>") && got.contains("<table x=1>"),
            "{got}"
        );
    }

    /// The pages say they are UTF-8, so a non-ASCII cell reads right in an
    /// app that would otherwise assume a code page, and the table inside
    /// survives both wrappers.
    #[test]
    fn the_pages_say_they_are_utf8_around_a_marked_table() {
        let fragment = "<table data-source=docxy><tr><td>M\u{fc}ller \u{4e2d}</td></tr></table>";
        let page = html_page(fragment);
        assert!(page.contains("<meta charset=\"utf-8\">") && page.contains(fragment));
        let payload = cf_html(fragment);
        assert!(payload.contains("<meta charset=\"utf-8\">"));
        assert_eq!(html_fragment(payload.as_bytes()).as_deref(), Some(fragment));
        assert!(gridcore::clip_html::is_own_table(
            &html_fragment(page.as_bytes()).unwrap()
        ));
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

    pub(super) fn write_html(_html: &str, _text: &str) {}

    pub(super) fn read_html() -> Option<Vec<u8>> {
        None
    }
}
