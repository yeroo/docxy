//! The optional formatting symbols (`yppx_plugin_read_ex` and
//! `yppx_plugin_write_ex`): the plan's formatting rides in a trailer comment
//! after the MSPDI, hex-encoded so it can hold anything.

use crate::{MALFORMED_INPUT, OK, UNSUPPORTED_EXTENSION, YppxBuf, bytes, fail, give, is_ours};
use std::os::raw::c_char;

const OPEN: &[u8] = b"<!-- fixture-formatting: ";
const CLOSE: &[u8] = b" -->\n";
const NON_UTF8: &[u8] = b"<!-- fixture-formatting-nonutf8 -->";
const MALFORMED: &[u8] = b"<!-- fixture-formatting-malformed -->";

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// # Safety
/// The pointers follow the ABI: `ext` NUL-terminated, `file` `len` bytes,
/// both buffers valid buffer pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_read_ex(
    ext: *const c_char,
    file: *const u8,
    len: usize,
    mspdi_out: *mut YppxBuf,
    formatting_out: *mut YppxBuf,
) -> i32 {
    // SAFETY: the ABI's contract.
    unsafe {
        if !is_ours(ext) {
            return fail(UNSUPPORTED_EXTENSION, "only .fakeproj");
        }
        let file = bytes(file, len);
        if file.is_empty() {
            return fail(MALFORMED_INPUT, "empty .fakeproj file");
        }
        if find(file, NON_UTF8).is_some() {
            give(formatting_out, vec![0xff, 0xfe]);
        } else if find(file, MALFORMED).is_some() {
            give(formatting_out, b"<formatting><text".to_vec());
        } else if let Some(start) = find(file, OPEN) {
            let body = &file[start + OPEN.len()..];
            let Some(end) = find(body, CLOSE) else {
                return fail(MALFORMED_INPUT, "unterminated formatting trailer");
            };
            let Some(formatting) = unhex(&body[..end]) else {
                return fail(MALFORMED_INPUT, "formatting trailer is not hex");
            };
            give(formatting_out, formatting);
            // The MSPDI is what stands before the trailer; the source note
            // (if any) after it goes with it.
            let mut rest = file[..start].to_vec();
            rest.extend_from_slice(&body[end + CLOSE.len()..]);
            give(mspdi_out, rest);
            return OK;
        }
        give(mspdi_out, file.to_vec());
    }
    OK
}

/// # Safety
/// The pointers follow the ABI: `ext` NUL-terminated, `mspdi` `len` bytes,
/// `formatting_xml` NULL or `formatting_len` bytes, `source_file` NULL or
/// `source_len` bytes, `file_out` a valid buffer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_write_ex(
    ext: *const c_char,
    mspdi: *const u8,
    len: usize,
    formatting_xml: *const u8,
    formatting_len: usize,
    source_file: *const u8,
    source_len: usize,
    file_out: *mut YppxBuf,
) -> i32 {
    // SAFETY: the ABI's contract.
    unsafe {
        if !is_ours(ext) {
            return fail(UNSUPPORTED_EXTENSION, "only .fakeproj");
        }
        let mut file = bytes(mspdi, len).to_vec();
        if !formatting_xml.is_null() {
            file.extend_from_slice(OPEN);
            file.extend_from_slice(hex(bytes(formatting_xml, formatting_len)).as_bytes());
            file.extend_from_slice(CLOSE);
        }
        if !source_file.is_null() {
            let note = format!("<!-- fixture: source {source_len} bytes -->\n");
            file.extend_from_slice(note.as_bytes());
        }
        give(file_out, file);
    }
    OK
}
