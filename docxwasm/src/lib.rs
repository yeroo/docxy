//! `docxwasm` — a hand-written WebAssembly bridge over [`docxcore`].
//!
//! The whole DOCX engine (parse → model → render → edit → **lossless** save) is
//! pure `std` with no third-party crates, so it compiles straight to
//! `wasm32-unknown-unknown` and runs inside a browser/webview. This crate adds
//! the thin C-ABI seam a JavaScript host talks to — deliberately without
//! `wasm-bindgen`, matching the project's from-scratch ethos (its own ZIP,
//! DEFLATE, XML, and PDF) and keeping the artifact tiny and auditable.
//!
//! ## ABI
//!
//! Memory is shared by handing raw pointers across the boundary:
//!
//! - `docx_alloc(len) -> ptr` / `docx_free(ptr, len)` — the host allocates a
//!   buffer in wasm memory, writes input bytes into it, and frees it later.
//! - Every result-returning export returns a pointer to a **length-prefixed
//!   buffer**: a little-endian `u32` byte count followed by that many bytes. The
//!   host reads the count, copies the payload, then calls `docx_free(ptr,
//!   4 + len)`. (Avoids 64-bit return values / BigInt on the JS side.)
//!
//! Documents are addressed by an opaque `u32` **handle** from `docx_open`;
//! `0` means failure. Sessions live in a thread-local registry (wasm is
//! single-threaded).
//!
//! The interesting logic lives in [`bridge`] as plain, natively-testable Rust;
//! this module is just marshalling.

pub mod bridge;
pub mod json;

use std::cell::RefCell;
use std::collections::HashMap;

use bridge::Session;

thread_local! {
    static SESSIONS: RefCell<HashMap<u32, Session>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(1) };
}

// ---- memory management -----------------------------------------------------

/// Allocate `len` bytes in wasm memory and return the pointer. The host writes
/// input (e.g. the `.docx` bytes, or a command string) here, then passes the
/// pointer to an export. Paired with [`docx_free`].
#[unsafe(no_mangle)]
pub extern "C" fn docx_alloc(len: usize) -> *mut u8 {
    // Exact-size allocation so `docx_free` can reconstruct the Vec precisely.
    let mut buf = vec![0u8; len];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a buffer previously returned by [`docx_alloc`] or by a result-returning
/// export. For result buffers, `len` must be `4 + payload_len` (the full
/// length-prefixed buffer).
///
/// # Safety
/// `ptr`/`len` must exactly match a live allocation from this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn docx_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len != 0 {
        // SAFETY: reconstructs the exact Vec `docx_alloc`/`ret_bytes` leaked.
        drop(unsafe { Vec::from_raw_parts(ptr, len, len) });
    }
}

/// Leak a length-prefixed result buffer (`[u32 len][payload]`) and return its
/// pointer for the host to read then free.
fn ret_bytes(payload: &[u8]) -> *mut u8 {
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Borrow the input bytes a host wrote at `ptr`/`len` (does not take ownership;
/// the host frees them separately).
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation.
unsafe fn input(ptr: *const u8, len: usize) -> &'static [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the host guarantees the buffer is valid for the call.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

// ---- session lifecycle -----------------------------------------------------

/// Open a `.docx` from bytes the host wrote at `ptr`/`len`. Returns an opaque
/// handle, or `0` if the document could not be parsed.
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation of the file bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn docx_open(ptr: *const u8, len: usize) -> u32 {
    let bytes = unsafe { input(ptr, len) };
    let Some(session) = Session::open(bytes) else {
        return 0;
    };
    let handle = NEXT_HANDLE.with(|n| {
        let mut n = n.borrow_mut();
        let h = *n;
        *n = n.wrapping_add(1).max(1);
        h
    });
    SESSIONS.with(|s| s.borrow_mut().insert(handle, session));
    handle
}

/// Close a session and free its document. Safe to call with an unknown handle.
#[unsafe(no_mangle)]
pub extern "C" fn docx_close(handle: u32) {
    SESSIONS.with(|s| s.borrow_mut().remove(&handle));
}

// ---- render / command / save ----------------------------------------------

/// Render the document to the JSON view (see [`bridge::Session::view_json`]).
/// Returns a length-prefixed UTF-8 buffer, or an empty buffer for an unknown
/// handle.
#[unsafe(no_mangle)]
pub extern "C" fn docx_render(handle: u32) -> *mut u8 {
    with_session(handle, |s| s.view_json(None).into_bytes())
}

/// Apply one tab-delimited command (see [`bridge::Session::dispatch`]) and
/// return the fresh JSON view. If the command produced clipboard text (copy /
/// cut), the view carries it in a `"copied"` field.
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation of the command string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn docx_cmd(handle: u32, ptr: *const u8, len: usize) -> *mut u8 {
    let cmd = String::from_utf8_lossy(unsafe { input(ptr, len) }).into_owned();
    with_session(handle, |s| {
        let copied = s.dispatch(&cmd);
        s.view_json(copied.as_deref()).into_bytes()
    })
}

/// Serialize the document back to `.docx` bytes, losslessly. Returns a
/// length-prefixed buffer (empty for an unknown handle).
#[unsafe(no_mangle)]
pub extern "C" fn docx_save(handle: u32) -> *mut u8 {
    with_session(handle, |s| s.save())
}

/// Return the raw bytes of the embedded media referenced by the relationship id
/// at `ptr`/`len` (an `rId…` string from a rendered image box). Empty buffer if
/// the handle or the media is unknown. The webview fetches each rid once and
/// caches the resulting data URI.
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation of the rid string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn docx_media(handle: u32, ptr: *const u8, len: usize) -> *mut u8 {
    let rid = String::from_utf8_lossy(unsafe { input(ptr, len) }).into_owned();
    with_session(handle, |s| s.media(&rid).unwrap_or_default())
}

/// Run `f` against the session for `handle`, returning its bytes as a
/// length-prefixed result buffer (empty payload if the handle is unknown).
fn with_session(handle: u32, f: impl FnOnce(&mut Session) -> Vec<u8>) -> *mut u8 {
    SESSIONS.with(|s| match s.borrow_mut().get_mut(&handle) {
        Some(session) => ret_bytes(&f(session)),
        None => ret_bytes(&[]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ret_bytes_is_length_prefixed() {
        let payload = b"hello";
        let ptr = ret_bytes(payload);
        // SAFETY: reading back the buffer we just produced.
        unsafe {
            let len = u32::from_le_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]) as usize;
            assert_eq!(len, 5);
            let data = std::slice::from_raw_parts(ptr.add(4), len);
            assert_eq!(data, b"hello");
            docx_free(ptr, 4 + len);
        }
    }
}
