//! The RTF and HTML beside a copy's text on the X11 `CLIPBOARD` selection
//! (#1950), on the suite's own connection: gpui's X11 clipboard is its own,
//! unreachable from here, and offers text and images only.
//!
//! **Owning.** A copy's extra (`text/rtf`, `text/html`) goes on only while
//! the selection still holds the copy's text, read back just before. The
//! suite then takes `CLIPBOARD` over from gpui (which drops its item) and
//! serves the same text on every target gpui does, beside the extra. gpui's
//! next copy takes it back, and the suite's data goes with its ownership.
//! A payload one property cannot carry (past the server's request size) is
//! not offered: no `INCR` sending, so the copy stays plain.
//!
//! **Two connections.** The owner's connection only owns and serves; its
//! thread is the only one that waits on its events. Every read opens its
//! own connection and window, as gpui's does, so a read's `SelectionNotify`
//! and `PropertyNotify` never meet a `SelectionRequest` in one queue.
//!
//! **Ownership is the server's word.** Whether the suite owns `CLIPBOARD` is
//! asked of the server (`GetSelectionOwner`), never a flag: a
//! `SelectionClear` comes in late, and a read just after another app's copy
//! must not answer with our old RTF.
//!
//! **Reading.** `TARGETS` first, then the first target the owner offers in
//! our order (`text/rtf`, `application/rtf`, `text/richtext`; `text/html`
//! or `text/html;…`), following `INCR`, bounded by [`super::MAX_RTF`] and a
//! wait of [`WAIT`] for each answer.
//!
//! **At quit** the suite hands its data to the clipboard manager
//! (`SAVE_TARGETS`) as gpui does for its own, so a copy outlives the app
//! where a manager runs.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

x11rb::atom_manager! {
    pub(super) Atoms: AtomCookies {
        CLIPBOARD,
        CLIPBOARD_MANAGER,
        SAVE_TARGETS,
        TARGETS,
        ATOM,
        INCR,
        UTF8_STRING,
        UTF8_MIME_0: b"text/plain;charset=utf-8",
        UTF8_MIME_1: b"text/plain;charset=UTF-8",
        STRING,
        TEXT,
        TEXT_MIME_UNKNOWN: b"text/plain",
        RTF: b"text/rtf",
        APPLICATION_RTF: b"application/rtf",
        RICHTEXT: b"text/richtext",
        HTML: b"text/html",
        // The property a read asks the owner to put its answer in.
        DOCXY_CLIP,
    }
}

/// How long a read waits for the owner's answer, and for each `INCR` chunk
/// after it.
const WAIT: Duration = Duration::from_secs(2);

/// How long the quit handover serves the clipboard manager, at most, as
/// gpui's own does.
const HANDOVER: Duration = Duration::from_millis(100);

/// Room left in a request for its header and the property's fields.
const REQUEST_HEADROOM: usize = 1024;

/// The extras a copy adds beside its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extra {
    Rtf,
    Html,
}

/// What the suite serves while it owns `CLIPBOARD`: the copy's text and its
/// extras, each under its target.
struct Held {
    text: String,
    extras: Vec<(Atom, Vec<u8>)>,
}

/// The quit handover's progress (gpui's `ManagerHandoverState`): the
/// manager may answer `SAVE_TARGETS` before or after it reads the data.
#[derive(Default)]
struct Handover {
    asked: bool,
    notified: bool,
    written: bool,
}

/// The owner's connection, its window and what it serves.
struct Owner {
    conn: RustConnection,
    win: Window,
    atoms: Atoms,
    /// Held across a check of the owner and the change it decides, on both
    /// threads, so a late `SelectionClear` never drops a newer copy's data.
    held: Mutex<Option<Held>>,
    /// The largest payload one `ChangeProperty` carries.
    max_payload: usize,
    handover: Mutex<Handover>,
    handed: Condvar,
}

/// The suite's X11 clipboard: an owner (and its serving thread) on the
/// display it was opened on, and reads on their own connections.
pub(crate) struct X11Clip {
    display: Option<String>,
    owner: Arc<Owner>,
    serving: Option<JoinHandle<()>>,
}

impl X11Clip {
    /// The clipboard on `display` (`None`: the `DISPLAY` variable's), or
    /// `None` when it cannot be opened.
    pub(crate) fn new(display: Option<&str>) -> Option<Self> {
        let (conn, screen) = x11rb::connect(display).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        let win = hidden_window(&conn, root, EventMask::STRUCTURE_NOTIFY)?;
        let max_payload = conn
            .maximum_request_bytes()
            .saturating_sub(REQUEST_HEADROOM);
        let owner = Arc::new(Owner {
            conn,
            win,
            atoms,
            held: Mutex::new(None),
            max_payload,
            handover: Mutex::new(Handover::default()),
            handed: Condvar::new(),
        });
        let serving = {
            let owner = Arc::clone(&owner);
            std::thread::Builder::new()
                .name("x11-clipboard".into())
                .spawn(move || owner.serve())
                .ok()?
        };
        Some(X11Clip {
            display: display.map(str::to_string),
            owner,
            serving: Some(serving),
        })
    }

    /// Whether the suite owns `CLIPBOARD`, as the server says.
    fn owned(&self) -> bool {
        self.owner.owned()
    }

    /// A fresh connection for one read.
    fn reader(&self) -> Option<Reader> {
        Reader::new(self.display.as_deref(), &self.owner.atoms)
    }

    /// Add `extra`'s `bytes` to the copy of `text` on `CLIPBOARD`, if it
    /// still holds `text`: the suite then owns the selection and serves both.
    /// Whether it does.
    pub(crate) fn offer(&self, text: &str, extra: Extra, bytes: Vec<u8>) -> bool {
        let a = &self.owner.atoms;
        let target = match extra {
            Extra::Rtf => a.RTF,
            Extra::Html => a.HTML,
        };
        if bytes.is_empty() || bytes.len() > self.owner.max_payload {
            return false;
        }
        // What the selection holds now: ours, or the owner's (gpui's serving
        // thread answers, so this thread waits on nothing of its own).
        let held_now = if self.owned() {
            self.owner.lock().as_ref().map(|h| h.text.clone())
        } else {
            self.reader()
                .and_then(|r| r.convert(a.UTF8_STRING, self.owner.max_payload))
                .and_then(|bytes| String::from_utf8(bytes).ok())
        };
        let Some(held_now) = held_now.filter(|held| super::same_text(held, text)) else {
            return false;
        };
        if held_now.len() > self.owner.max_payload {
            return false;
        }
        let mut held = self.owner.lock();
        let ours = self.owned();
        match held.as_mut() {
            Some(h) if ours && h.text == held_now => {
                h.extras.retain(|(t, _)| *t != target);
                h.extras.push((target, bytes));
            }
            _ => {
                *held = Some(Held {
                    text: held_now,
                    extras: vec![(target, bytes)],
                });
            }
        }
        if ours {
            return true;
        }
        let conn = &self.owner.conn;
        let took = conn
            .set_selection_owner(self.owner.win, a.CLIPBOARD, CURRENT_TIME)
            .is_ok()
            && conn.flush().is_ok()
            && self.owned();
        if !took {
            *held = None;
        }
        took
    }

    /// The copy's text on `CLIPBOARD`, as `UTF8_STRING`.
    #[cfg(test)]
    pub(crate) fn read_text(&self) -> Option<String> {
        if self.owned() {
            return self.owner.lock().as_ref().map(|h| h.text.clone());
        }
        let bytes = self
            .reader()?
            .convert(self.owner.atoms.UTF8_STRING, super::MAX_RTF)?;
        String::from_utf8(bytes).ok()
    }

    /// The RTF on `CLIPBOARD`: `text/rtf`, else `application/rtf`, else
    /// `text/richtext`, whichever the owner offers first. `None` when it
    /// offers none, no one owns it, or it is past [`super::MAX_RTF`].
    pub(crate) fn read_rtf(&self) -> Option<Vec<u8>> {
        let a = &self.owner.atoms;
        self.read(&[a.RTF, a.APPLICATION_RTF, a.RICHTEXT], None)
    }

    /// The HTML on `CLIPBOARD`: a target named `text/html` (or
    /// `text/html;…`, any case). A UTF-16 page (an older Firefox's) comes
    /// back as UTF-8.
    pub(crate) fn read_html(&self) -> Option<Vec<u8>> {
        let html = |name: &str| {
            let name = name.to_ascii_lowercase();
            name == "text/html" || name.starts_with("text/html;")
        };
        let bytes = self.read(&[self.owner.atoms.HTML], Some(&html))?;
        Some(utf8_page(bytes))
    }

    /// The first of `order` the owner offers, else (another owner's) the
    /// first of its targets whose name `named` takes, read. The suite's own
    /// extras are always one of `order`.
    fn read(&self, order: &[Atom], named: Option<&dyn Fn(&str) -> bool>) -> Option<Vec<u8>> {
        if self.owned() {
            let held = self.owner.lock();
            let extras = &held.as_ref()?.extras;
            return order.iter().find_map(|target| {
                extras
                    .iter()
                    .find(|(t, _)| t == target)
                    .map(|(_, bytes)| bytes.clone())
            });
        }
        let reader = self.reader()?;
        let targets = reader.targets()?;
        let target = order
            .iter()
            .copied()
            .find(|t| targets.contains(t))
            .or_else(|| named.and_then(|named| reader.find_named(&targets, named)))?;
        reader.convert(target, super::MAX_RTF)
    }

    /// Hand what the suite serves to the clipboard manager, if it owns
    /// `CLIPBOARD` and a manager runs: `SAVE_TARGETS`, then serve its reads
    /// for at most [`HANDOVER`], as gpui does for its own at quit. Whether
    /// the manager said it is done.
    pub(crate) fn hand_over(&self) -> bool {
        let a = &self.owner.atoms;
        if !self.owned() || self.owner.lock().is_none() {
            return false;
        }
        let conn = &self.owner.conn;
        let has_manager = conn
            .get_selection_owner(a.CLIPBOARD_MANAGER)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner != NONE);
        if !has_manager {
            return false;
        }
        let mut state = self
            .owner
            .handover
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *state = Handover {
            asked: true,
            ..Handover::default()
        };
        let asked = conn
            .convert_selection(
                self.owner.win,
                a.CLIPBOARD_MANAGER,
                a.SAVE_TARGETS,
                a.DOCXY_CLIP,
                CURRENT_TIME,
            )
            .is_ok()
            && conn.flush().is_ok();
        if !asked {
            return false;
        }
        let (state, _) = self
            .owner
            .handed
            .wait_timeout_while(state, HANDOVER, |s| !(s.notified && s.written))
            .unwrap_or_else(|e| e.into_inner());
        state.notified && state.written
    }
}

impl Drop for X11Clip {
    fn drop(&mut self) {
        // The serving thread ends on its window's DestroyNotify.
        let conn = &self.owner.conn;
        if conn.destroy_window(self.owner.win).is_ok() && conn.flush().is_ok() {
            if let Some(serving) = self.serving.take() {
                let _ = serving.join();
            }
        }
    }
}

impl Owner {
    fn lock(&self) -> MutexGuard<'_, Option<Held>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn owned(&self) -> bool {
        self.conn
            .get_selection_owner(self.atoms.CLIPBOARD)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner == self.win)
    }

    /// The serving thread: answers every request for `CLIPBOARD` until its
    /// window goes, or the connection does.
    fn serve(&self) {
        while let Ok(event) = self.conn.wait_for_event() {
            match event {
                Event::DestroyNotify(e) if e.window == self.win => return,
                Event::SelectionClear(e) if e.selection == self.atoms.CLIPBOARD => {
                    // Only while another owner holds it: a clear that comes
                    // in after a newer copy of ours took it back is stale.
                    let mut held = self.lock();
                    if !self.owned() {
                        *held = None;
                    }
                }
                Event::SelectionRequest(e) => {
                    let written = self.answer(e).unwrap_or(false);
                    if written && e.target != self.atoms.TARGETS {
                        self.handover_progress(|s| s.written = true);
                    }
                }
                Event::SelectionNotify(e) if e.selection == self.atoms.CLIPBOARD_MANAGER => {
                    self.handover_progress(|s| s.notified = true);
                }
                _ => {}
            }
        }
    }

    fn handover_progress(&self, mark: impl FnOnce(&mut Handover)) {
        let mut state = self.handover.lock().unwrap_or_else(|e| e.into_inner());
        if state.asked {
            mark(&mut state);
            self.handed.notify_all();
        }
    }

    /// Answer `e` with a `SelectionNotify`: its property set when the
    /// target could be converted, `NONE` when not. Whether it was.
    fn answer(&self, e: SelectionRequestEvent) -> Result<bool, x11rb::errors::ConnectionError> {
        // An obsolete requestor names no property: the target is its name.
        let property = if e.property == NONE {
            e.target
        } else {
            e.property
        };
        let done = e.selection == self.atoms.CLIPBOARD && self.put(&e, property)?;
        self.conn.send_event(
            false,
            e.requestor,
            EventMask::NO_EVENT,
            SelectionNotifyEvent {
                response_type: SELECTION_NOTIFY_EVENT,
                sequence: 0,
                time: e.time,
                requestor: e.requestor,
                selection: e.selection,
                target: e.target,
                property: if done { property } else { NONE },
            },
        )?;
        self.conn.flush()?;
        Ok(done)
    }

    /// Put `e.target`'s conversion of what is held on the requestor's
    /// `property`. Whether there was one.
    fn put(
        &self,
        e: &SelectionRequestEvent,
        property: Atom,
    ) -> Result<bool, x11rb::errors::ConnectionError> {
        let a = &self.atoms;
        let held = self.lock();
        let Some(held) = held.as_ref() else {
            return Ok(false);
        };
        let utf8 = [
            a.UTF8_STRING,
            a.UTF8_MIME_0,
            a.UTF8_MIME_1,
            a.TEXT_MIME_UNKNOWN,
            a.TEXT,
        ];
        let put8 = |kind: Atom, bytes: &[u8]| {
            self.conn
                .change_property8(PropMode::REPLACE, e.requestor, property, kind, bytes)
                .map(|_| true)
        };
        if e.target == a.TARGETS {
            let mut targets = vec![a.TARGETS, a.SAVE_TARGETS];
            targets.extend(utf8);
            targets.push(a.STRING);
            targets.extend(held.extras.iter().map(|(t, _)| *t));
            self.conn.change_property32(
                PropMode::REPLACE,
                e.requestor,
                property,
                AtomEnum::ATOM,
                &targets,
            )?;
            return Ok(true);
        }
        if utf8.contains(&e.target) {
            // TEXT names no encoding of its own: it answers as UTF-8.
            let kind = if e.target == a.TEXT {
                a.UTF8_STRING
            } else {
                e.target
            };
            return put8(kind, held.text.as_bytes());
        }
        if e.target == a.STRING {
            return put8(a.STRING, &latin1(&held.text));
        }
        match held.extras.iter().find(|(t, _)| *t == e.target) {
            Some((t, bytes)) => put8(*t, bytes),
            None => Ok(false),
        }
    }
}

/// `text` as ISO Latin-1 (`STRING`), a character past it as `?`.
fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

/// An HTML payload as UTF-8: a UTF-16 one (with its BOM) decoded, anything
/// else as it is.
fn utf8_page(bytes: Vec<u8>) -> Vec<u8> {
    let utf16 = |rest: &[u8], big: bool| {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|p| {
                if big {
                    u16::from_be_bytes([p[0], p[1]])
                } else {
                    u16::from_le_bytes([p[0], p[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units).into_bytes()
    };
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        _ => bytes,
    }
}

/// An unmapped 1×1 window under `root` that hears `events`.
fn hidden_window(conn: &RustConnection, root: Window, events: EventMask) -> Option<Window> {
    let win = conn.generate_id().ok()?;
    conn.create_window(
        0,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(events),
    )
    .ok()?;
    conn.flush().ok()?;
    Some(win)
}

/// One read's own connection and window.
struct Reader {
    conn: RustConnection,
    win: Window,
    atoms: Atoms,
}

impl Reader {
    /// A connection to `display` with a window that hears its property's
    /// changes (for `INCR`). `atoms` are the owner's: atoms are the
    /// server's, the same on every connection.
    fn new(display: Option<&str>, atoms: &Atoms) -> Option<Self> {
        let (conn, screen) = x11rb::connect(display).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let win = hidden_window(&conn, root, EventMask::PROPERTY_CHANGE)?;
        Some(Reader {
            conn,
            win,
            atoms: *atoms,
        })
    }

    /// The targets the owner offers.
    fn targets(&self) -> Option<Vec<Atom>> {
        let bytes = self.convert(self.atoms.TARGETS, 1 << 20)?;
        Some(
            bytes
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )
    }

    /// The first of `targets` whose name `wanted` takes.
    fn find_named(&self, targets: &[Atom], wanted: &dyn Fn(&str) -> bool) -> Option<Atom> {
        // Every name asked at once, then the replies read in order.
        let cookies: Vec<_> = targets
            .iter()
            .map(|&t| (t, self.conn.get_atom_name(t).ok()))
            .collect();
        let mut found = None;
        for (t, cookie) in cookies {
            let name = cookie.and_then(|c| c.reply().ok());
            if found.is_none() && name.is_some_and(|n| wanted(&String::from_utf8_lossy(&n.name))) {
                found = Some(t);
            }
        }
        found
    }

    /// Ask the owner for `target` and read its answer, following `INCR`.
    /// `None` when it refuses, does not answer within [`WAIT`], or sends
    /// more than `limit` bytes.
    fn convert(&self, target: Atom, limit: usize) -> Option<Vec<u8>> {
        let conn = &self.conn;
        let (clipboard, prop, incr) =
            (self.atoms.CLIPBOARD, self.atoms.DOCXY_CLIP, self.atoms.INCR);
        conn.delete_property(self.win, prop).ok()?;
        conn.convert_selection(self.win, clipboard, target, prop, CURRENT_TIME)
            .ok()?;
        conn.flush().ok()?;
        let mut deadline = Instant::now() + WAIT;
        let notify = loop {
            match conn.poll_for_event().ok()? {
                Some(Event::SelectionNotify(e)) if e.requestor == self.win => break e,
                Some(_) => {}
                None if Instant::now() > deadline => return None,
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        if notify.property == NONE {
            return None;
        }
        // Its size first, so a payload past `limit` is never copied.
        let head = conn
            .get_property(false, self.win, prop, AtomEnum::ANY, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        if head.type_ != incr {
            let len = head.bytes_after as usize;
            if len > limit {
                let _ = conn.delete_property(self.win, prop);
                let _ = conn.flush();
                return None;
            }
            let words = u32::try_from(len.div_ceil(4)).ok()?;
            let reply = conn
                .get_property(true, self.win, prop, AtomEnum::ANY, 0, words)
                .ok()?
                .reply()
                .ok()?;
            conn.flush().ok()?;
            return Some(reply.value);
        }
        // INCR: deleting the property asks for the first chunk; each chunk
        // is a new value, and an empty one ends it.
        conn.delete_property(self.win, prop).ok()?;
        conn.flush().ok()?;
        let mut out = Vec::new();
        deadline = Instant::now() + WAIT;
        loop {
            match conn.poll_for_event().ok()? {
                Some(Event::PropertyNotify(e))
                    if e.window == self.win && e.atom == prop && e.state == Property::NEW_VALUE =>
                {
                    let chunk = conn
                        .get_property(true, self.win, prop, AtomEnum::ANY, 0, u32::MAX / 4)
                        .ok()?
                        .reply()
                        .ok()?;
                    conn.flush().ok()?;
                    if chunk.value.is_empty() {
                        return Some(out);
                    }
                    if out.len() + chunk.value.len() > limit {
                        return None;
                    }
                    out.extend_from_slice(&chunk.value);
                    deadline = Instant::now() + WAIT;
                }
                Some(_) => {}
                None if Instant::now() > deadline => return None,
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
