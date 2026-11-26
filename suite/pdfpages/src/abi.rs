//! The C ABI `pdfpages.wasm` exports to the VS Code webview (#2259), in
//! docxwasm's style: no wasm-bindgen, raw pointers into linear memory.
//!
//! - `pdf_alloc(len) -> ptr` / `pdf_free(ptr, len)`: the host writes the
//!   file's bytes into a buffer it allocated, passes it to `pdf_open`, then
//!   frees it.
//! - Every answer is a **length-prefixed buffer**: a little-endian `u32`
//!   count, then that many bytes. The host copies it out and frees it with
//!   `pdf_free(ptr, 4 + count)`.
//! - `pdf_open(ptr, len) -> handle` (0 only for no bytes), `pdf_close`.
//! - `pdf_info(handle)` and `pdf_text(handle, page)`: UTF-8 JSON
//!   ([`crate::Doc::info_json`], [`crate::Doc::text_json`]).
//! - `pdf_render(handle, page, scale)`: `[u32 width][u32 height][RGBA]`, or
//!   width and height 0 followed by why, in UTF-8.
//!
//! The whole module is built on every target, so native tests and clippy
//! cover it. In the browser a panic, an allocation past the memory cap
//! (`build.rs`) or a stack overflow is a trap: the instance is unusable
//! after one, and the host starts another.

use crate::Doc;
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static DOCS: RefCell<HashMap<u32, Doc>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u32> = const { RefCell::new(1) };
}

/// Allocate `len` bytes for the host to write into; freed by [`pdf_free`].
#[unsafe(no_mangle)]
pub extern "C" fn pdf_alloc(len: usize) -> *mut u8 {
    let mut buf = vec![0u8; len];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a buffer from [`pdf_alloc`] (its `len`) or an answer (`4 +` its
/// count).
///
/// # Safety
/// `ptr` and `len` must be exactly a live allocation of this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pdf_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len != 0 {
        // SAFETY: rebuilds the exact Vec `pdf_alloc` or `answer` leaked.
        drop(unsafe { Vec::from_raw_parts(ptr, len, len) });
    }
}

/// Open the PDF the host wrote at `ptr`/`len` (copied: the host may free
/// it). 0 for no bytes; a file that is no readable PDF still opens, and
/// [`pdf_info`] says why.
///
/// # Safety
/// `ptr`/`len` must describe a live allocation the host wrote.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pdf_open(ptr: *const u8, len: usize) -> u32 {
    if ptr.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the host guarantees the buffer for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    let handle = NEXT.with(|n| {
        let mut n = n.borrow_mut();
        let h = *n;
        *n = n.wrapping_add(1).max(1);
        h
    });
    DOCS.with(|d| d.borrow_mut().insert(handle, Doc::new(bytes)));
    handle
}

/// Close a document; an unknown handle is ignored.
#[unsafe(no_mangle)]
pub extern "C" fn pdf_close(handle: u32) {
    DOCS.with(|d| d.borrow_mut().remove(&handle));
}

/// The pages and outline as JSON.
#[unsafe(no_mangle)]
pub extern "C" fn pdf_info(handle: u32) -> *mut u8 {
    answer(
        &with_doc(handle, Doc::info_json)
            .unwrap_or_else(unknown)
            .into_bytes(),
    )
}

/// Page `page`'s text as JSON.
#[unsafe(no_mangle)]
pub extern "C" fn pdf_text(handle: u32, page: u32) -> *mut u8 {
    let json = with_doc(handle, |d| d.text_json(page as usize)).unwrap_or_else(unknown);
    answer(json.as_bytes())
}

/// Page `page` drawn at `scale` pixels a point (capped as
/// [`Doc::pixels`]): `[u32 width][u32 height][straight RGBA]`, or zeros and
/// why.
#[unsafe(no_mangle)]
pub extern "C" fn pdf_render(handle: u32, page: u32, scale: f32) -> *mut u8 {
    let drawn = with_doc(handle, |d| d.pixels(page as usize, scale))
        .unwrap_or_else(|| Err("no such document".into()));
    let mut out = Vec::new();
    match drawn {
        Ok(r) => {
            out.reserve(8 + r.rgba.len());
            out.extend_from_slice(&r.width.to_le_bytes());
            out.extend_from_slice(&r.height.to_le_bytes());
            out.extend_from_slice(&r.rgba);
        }
        Err(why) => {
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(why.as_bytes());
        }
    }
    answer(&out)
}

fn with_doc<T>(handle: u32, f: impl FnOnce(&mut Doc) -> T) -> Option<T> {
    DOCS.with(|d| d.borrow_mut().get_mut(&handle).map(f))
}

fn unknown() -> String {
    crate::failure_json("no such document", false)
}

/// `payload` as a leaked length-prefixed buffer.
fn answer(payload: &[u8]) -> *mut u8 {
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    // `from_raw_parts` in `pdf_free` needs capacity == length.
    let mut buf = buf.into_boxed_slice().into_vec();
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}
