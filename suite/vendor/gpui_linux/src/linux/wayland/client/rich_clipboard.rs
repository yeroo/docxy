// Added for docxy (#1966): RTF/HTML beside a copy's text on the Wayland
// selection, through gpui's own data device.

//! The Wayland selection's extra formats, for an app on top of gpui.
//!
//! gpui offers and reads text (and images) only. A compositor keeps one data
//! device per client (mutter replaces an older one), and a second connection
//! has neither the selection's events nor an input serial, so the formats go
//! through gpui's own device, serial and event loop: [`add_formats`] sets the
//! selection again right after gpui's copy, with a source that offers its
//! text, the formats and gpui's `pid/<pid>` mime (so gpui still reads the
//! copy as its own), and gpui serves it. [`mime_types`] and [`read`] look at
//! the selection as gpui's clipboard sees it.
//!
//! Every call is on gpui's main thread, outside gpui's own event handling;
//! off it, on X11 or headless, or while gpui's state is borrowed, they do
//! nothing (`false` / `None`). None panics.

use std::cell::RefCell;

use super::{WaylandClientState, WaylandClientStatePtr};

thread_local! {
    /// gpui's Wayland client on this (the main) thread, never kept alive
    /// by this.
    static CLIENT: RefCell<Option<WaylandClientStatePtr>> = const { RefCell::new(None) };
}

/// Called once gpui's Wayland client exists.
pub(super) fn set_client(client: WaylandClientStatePtr) {
    CLIENT.with(|held| *held.borrow_mut() = Some(client));
}

/// `f` over gpui's client state, if it is on this thread and not borrowed.
fn with_state<T>(f: impl FnOnce(&mut WaylandClientState) -> T) -> Option<T> {
    let client = CLIENT.with(|held| held.borrow().as_ref()?.0.upgrade())?;
    let mut state = client.try_borrow_mut().ok()?;
    Some(f(&mut state))
}

/// Add `formats` (mime, bytes) beside gpui's latest copy, when it is still
/// the selection and its text is `text` (line ends compared loosely): the
/// selection is set again, to a source offering the text, every format and
/// gpui's own mime. Whether it was. Call it right after gpui's copy, before
/// anything flushes the connection, so the compositor gets both in one write
/// and a clipboard manager never sees the text-only source alive.
pub fn add_formats(text: &str, formats: Vec<(String, Vec<u8>)>) -> bool {
    with_state(|state| {
        if !state.clipboard.own_copy_is(text) {
            return false;
        }
        let (Some(manager), Some(device)) = (
            state.globals.data_device_manager.clone(),
            state.data_device.clone(),
        ) else {
            return false;
        };
        let Some(serial) = state.serial_tracker.selection_serial() else {
            return false;
        };
        state.clipboard.merge_formats(formats);
        let source = manager.create_data_source(&state.globals.qh, ());
        for mime in state.clipboard.own_mime_types() {
            source.offer(mime);
        }
        device.set_selection(Some(&source), serial.as_raw());
        state.clipboard.set_source(source);
        // The read-back between gpui's write and this one never flushes, so
        // both reach the compositor here.
        if let Err(err) = state.clipboard.flush() {
            log::warn!("Wayland clipboard flush failed: {err}");
        }
        true
    })
    .unwrap_or(false)
}

/// The selection's mime types: gpui's own copy's while it is the selection,
/// else the current offer's. `None` with no selection.
pub fn mime_types() -> Option<Vec<String>> {
    with_state(|state| state.clipboard.mime_types()).flatten()
}

/// The selection's `mime`: gpui's own copy's formats from memory, another
/// app's through a pipe (waiting as long as gpui's text paste does). `None`
/// when it is not offered, is empty, is past `max` bytes, or on an error.
pub fn read(mime: &str, max: usize) -> Option<Vec<u8>> {
    with_state(|state| state.clipboard.read_format(mime, max)).flatten()
}
