//! A format plugin for tests: it declares one type, `.fakeproj`, whose files
//! are MSPDI XML passed through unchanged, over the C ABI that
//! `projcore::plugin_abi` documents. It is dependency-free on purpose, so it
//! exercises the ABI as a plugin in any language would.
//!
//! Two behaviours exist to test the host's plumbing:
//! - reading an empty file fails with code 2 and the message "empty
//!   .fakeproj file", which the host reads through `yppx_plugin_last_error`;
//! - writing with source bytes appends `<!-- fixture: source N bytes -->`
//!   after the MSPDI, so a test can see the source arrive.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Mutex;

/// `{uint8_t* ptr; size_t len;}`, as the ABI lays it out.
#[repr(C)]
pub struct YppxBuf {
    ptr: *mut u8,
    len: usize,
}

const OK: i32 = 0;
const UNSUPPORTED_EXTENSION: i32 = 1;
const MALFORMED_INPUT: i32 = 2;

const FORMATS: &str = r#"[{"ext":"fakeproj","label":"Fake Project","read":true,"write":true}]"#;

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

fn fail(code: i32, message: &str) -> i32 {
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = message.to_string();
    code
}

/// Hand `bytes` to the host in `out`.
///
/// # Safety
/// `out` is a valid buffer pointer.
unsafe fn give(out: *mut YppxBuf, bytes: Vec<u8>) {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    // SAFETY: the caller passes a valid `out`.
    unsafe { *out = YppxBuf { ptr, len } };
}

/// # Safety
/// `ptr` points at `len` readable bytes, or `len` is 0.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// # Safety
/// `ext` is NULL or a NUL-terminated string.
unsafe fn is_ours(ext: *const c_char) -> bool {
    // SAFETY: the caller's contract.
    !ext.is_null() && unsafe { CStr::from_ptr(ext) }.to_bytes() == b"fakeproj"
}

#[unsafe(no_mangle)]
pub extern "C" fn yppx_plugin_abi() -> u32 {
    1
}

/// # Safety
/// `out` is a valid buffer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_formats(out: *mut YppxBuf) -> i32 {
    // SAFETY: the ABI's contract.
    unsafe { give(out, FORMATS.as_bytes().to_vec()) };
    OK
}

/// # Safety
/// The pointers follow the ABI: `ext` NUL-terminated, `file` `len` bytes,
/// `mspdi_out` a valid buffer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_read(
    ext: *const c_char,
    file: *const u8,
    len: usize,
    mspdi_out: *mut YppxBuf,
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
        give(mspdi_out, file.to_vec());
    }
    OK
}

/// # Safety
/// The pointers follow the ABI: `ext` NUL-terminated, `mspdi` `len` bytes,
/// `source_file` NULL or `source_len` bytes, `file_out` a valid buffer
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_write(
    ext: *const c_char,
    mspdi: *const u8,
    len: usize,
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
        if !source_file.is_null() {
            let note = format!("<!-- fixture: source {source_len} bytes -->\n");
            file.extend_from_slice(note.as_bytes());
        }
        give(file_out, file);
    }
    OK
}

/// # Safety
/// `buf` is NULL or a buffer this library filled, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_free(buf: *mut YppxBuf) {
    // SAFETY: the ABI's contract: `ptr`/`len` came from `give`.
    unsafe {
        if buf.is_null() || (*buf).ptr.is_null() {
            return;
        }
        let slice = std::ptr::slice_from_raw_parts_mut((*buf).ptr, (*buf).len);
        drop(Box::from_raw(slice));
        *buf = YppxBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
    }
}

/// # Safety
/// `utf8_out` is a valid buffer pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn yppx_plugin_last_error(utf8_out: *mut YppxBuf) -> i32 {
    let message = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if message.is_empty() {
        return 1;
    }
    // SAFETY: the ABI's contract.
    unsafe { give(utf8_out, message.into_bytes()) };
    OK
}
