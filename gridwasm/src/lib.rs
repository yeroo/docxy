//! `gridwasm` — WebAssembly bridge over [`gridcore`] (the Offxy VS Code
//! extension's spreadsheet engine).
//!
//! Std-only, like the rest of the core stack, so it compiles straight to
//! `wasm32-unknown-unknown` and runs inside a browser/webview. This crate adds
//! the thin C-ABI seam a JavaScript host talks to — deliberately without
//! `wasm-bindgen`, matching the project's from-scratch ethos and keeping the
//! artifact tiny and auditable. Mirrors `docxwasm`'s ABI shape.
//!
//! ## ABI
//!
//! Memory is shared by handing raw pointers across the boundary:
//!
//! - `grid_alloc(len) -> ptr` / `grid_free(ptr, len)` — the host allocates a
//!   buffer in wasm memory, writes input bytes into it, and frees it later.
//! - Every result-returning export returns a pointer to a **length-prefixed
//!   buffer**: a little-endian `u32` byte count followed by that many bytes. The
//!   host reads the count, copies the payload, then calls `grid_free(ptr,
//!   4 + len)`. (Avoids 64-bit return values / BigInt on the JS side.)
//!
//! Workbooks are addressed by an opaque `u32` **handle** from `grid_open`;
//! `0` means failure. Sessions live in a thread-local registry (wasm is
//! single-threaded).
//!
//! The interesting logic lives in [`bridge`] as plain, natively-testable Rust;
//! this module is just marshalling.

pub mod bridge;
mod json;

use std::cell::RefCell;
use std::collections::HashMap;

use bridge::Session;

thread_local! {
    static SESSIONS: RefCell<HashMap<u32, Session>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(1) };
}

// ---- memory management -----------------------------------------------------

/// Allocate `len` bytes in wasm memory and return the pointer. The host writes
/// input (e.g. the `.xlsx` bytes, or a command string) here, then passes the
/// pointer to an export. Paired with [`grid_free`].
#[unsafe(no_mangle)]
pub extern "C" fn grid_alloc(len: usize) -> *mut u8 {
    // Exact-size allocation so `grid_free` can reconstruct the Vec precisely.
    let mut buf = vec![0u8; len];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a buffer previously returned by [`grid_alloc`] or by a result-returning
/// export. For result buffers, `len` must be `4 + payload_len` (the full
/// length-prefixed buffer).
///
/// # Safety
/// `ptr`/`len` must exactly match a live allocation from this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn grid_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len != 0 {
        // SAFETY: reconstructs the exact Vec `grid_alloc`/`ret_bytes` leaked.
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

/// Open a `.xlsx` from bytes the host wrote at `ptr`/`len`. Returns an opaque
/// handle, or `0` if the workbook could not be parsed.
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation of the file bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn grid_open(ptr: *const u8, len: usize) -> u32 {
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

/// Close a session and free its workbook. Safe to call with an unknown handle.
#[unsafe(no_mangle)]
pub extern "C" fn grid_close(handle: u32) {
    SESSIONS.with(|s| s.borrow_mut().remove(&handle));
}

// ---- render / command / save -----------------------------------------------

/// Apply one tab-delimited command (see [`bridge::Session::dispatch`]) and
/// return the fresh viewport JSON. If the command produced clipboard text
/// (copy / cut), the JSON carries it in a `"copied"` field.
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation of the command string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn grid_cmd(handle: u32, ptr: *const u8, len: usize) -> *mut u8 {
    let cmd = String::from_utf8_lossy(unsafe { input(ptr, len) }).into_owned();
    with_session(handle, |s| {
        let copied = s.dispatch(&cmd);
        s.view_json(copied.as_deref()).into_bytes()
    })
}

/// Serialize the workbook back to `.xlsx` bytes, losslessly. Returns a
/// length-prefixed buffer (empty for an unknown handle).
#[unsafe(no_mangle)]
pub extern "C" fn grid_save(handle: u32) -> *mut u8 {
    with_session(handle, |s| s.save())
}

/// Bytes of a fresh empty workbook (the host's empty-file create flow).
/// Stateless — no handle needed. Returns a length-prefixed buffer.
#[unsafe(no_mangle)]
pub extern "C" fn grid_new() -> *mut u8 {
    ret_bytes(&bridge::new_workbook())
}

// ---- agent control surface -------------------------------------------------

/// Route one agent control request (JSON `{"verb":…,"args":{…}}`, see
/// [`bridge::Session::ctl`]) against the session for `handle` and return the
/// JSON reply. For an unknown handle this returns the same failure envelope
/// `ctl` uses for any other error, so the caller never has to special-case
/// handle lookup.
///
/// # Safety
/// `ptr`/`len` must describe a live host allocation of the request JSON.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn grid_ctl(handle: u32, ptr: *const u8, len: usize) -> *mut u8 {
    let req = String::from_utf8_lossy(unsafe { input(ptr, len) }).into_owned();
    SESSIONS.with(|s| match s.borrow_mut().get_mut(&handle) {
        Some(session) => ret_bytes(session.ctl(&req).as_bytes()),
        None => ret_bytes(br#"{"ok":false,"error":"unknown session handle"}"#),
    })
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
            grid_free(ptr, 4 + len);
        }
    }

    /// Copy `bytes` into a fresh wasm allocation and return its pointer (the
    /// host-write step every export's SAFETY contract assumes).
    fn write_into_wasm(bytes: &[u8]) -> *mut u8 {
        let ptr = grid_alloc(bytes.len());
        // SAFETY: `ptr` is a fresh allocation exactly `bytes.len()` long.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
        ptr
    }

    /// Read back a length-prefixed result buffer as a `String` and free it.
    fn read_result(ptr: *mut u8) -> String {
        // SAFETY: `ptr` is a live length-prefixed buffer from this module.
        unsafe {
            let len = u32::from_le_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]) as usize;
            let data = std::slice::from_raw_parts(ptr.add(4), len);
            let s = String::from_utf8_lossy(data).into_owned();
            grid_free(ptr, 4 + len);
            s
        }
    }

    #[test]
    fn grid_ctl_marshals_a_request_and_reply() {
        let bytes = bridge::new_workbook();
        let doc_ptr = write_into_wasm(&bytes);
        // SAFETY: `doc_ptr`/`bytes.len()` is the allocation just written.
        let handle = unsafe { grid_open(doc_ptr, bytes.len()) };
        unsafe { grid_free(doc_ptr, bytes.len()) };
        assert_ne!(handle, 0, "expected a live session handle");

        let req = br#"{"verb":"cell.set","args":{"ref":"A1","text":"hi"}}"#;
        let req_ptr = write_into_wasm(req);
        // SAFETY: `req_ptr`/`req.len()` is the allocation just written.
        let out = read_result(unsafe { grid_ctl(handle, req_ptr, req.len()) });
        unsafe { grid_free(req_ptr, req.len()) };
        assert!(
            out.contains("\"ok\":true") && out.contains("\"text\":\"hi\""),
            "{out}"
        );

        grid_close(handle);
    }

    #[test]
    fn grid_ctl_reports_unknown_handle_instead_of_panicking() {
        // SAFETY: null/0 is the documented empty-input case for `input()`.
        let out = read_result(unsafe { grid_ctl(999_999, std::ptr::null(), 0) });
        assert!(out.contains("\"ok\":false"), "{out}");
    }
}
