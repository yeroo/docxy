//! The X11 clipboard against other X clients, each test on its own private
//! Xvfb: never the developer's `DISPLAY`, whose clipboard an owner would
//! take, and never one display for two tests, which run in parallel and
//! would race on one `CLIPBOARD`.

use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use x11rb::protocol::xproto::ChangeWindowAttributesAux;

/// A private X server, killed on drop.
struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    /// A fresh Xvfb on a free display it picks (`-displayfd`). `None`, with
    /// a note, where there is no Xvfb, except under CI, which always has it
    /// (its `xvfb-run` needs it): there, a missing one fails the test.
    fn start() -> Option<Self> {
        let spawned = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-nolisten",
                "tcp",
                "-screen",
                "0",
                "64x64x24",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "Xvfb is needed under CI: {e}"
                );
                eprintln!("skipped: no Xvfb ({e})");
                return None;
            }
        };
        let mut line = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        let n: u32 = line.trim().parse().expect("Xvfb printed its display");
        Some(Xvfb {
            child,
            display: format!(":{n}"),
        })
    }

    fn clip(&self) -> X11Clip {
        X11Clip::new(Some(&self.display)).expect("the private display opens")
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Another app: an X client owning `CLIPBOARD` with `offers` (target name,
/// bytes), serving each in `INCR` chunks of `chunk` bytes when it is past
/// that size, until dropped.
struct Peer {
    conn: Arc<RustConnection>,
    win: Window,
    serving: Option<JoinHandle<()>>,
}

impl Peer {
    fn own(display: &str, offers: &[(&str, &[u8])], chunk: usize) -> Peer {
        let (conn, screen) = x11rb::connect(Some(display)).unwrap();
        let root = conn.setup().roots[screen].root;
        let win = hidden_window(&conn, root, EventMask::STRUCTURE_NOTIFY).unwrap();
        let atom = |name: &str| {
            conn.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let atoms = Atoms::new(&conn).unwrap().reply().unwrap();
        let offers: Vec<(Atom, Vec<u8>)> =
            offers.iter().map(|(n, b)| (atom(n), b.to_vec())).collect();
        conn.set_selection_owner(win, atoms.CLIPBOARD, CURRENT_TIME)
            .unwrap();
        conn.flush().unwrap();
        let conn = Arc::new(conn);
        let serving = {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || peer_serve(&conn, win, &atoms, &offers, chunk))
        };
        // Owned before the test goes on.
        let owner = conn
            .get_selection_owner(atoms.CLIPBOARD)
            .unwrap()
            .reply()
            .unwrap()
            .owner;
        assert_eq!(owner, win);
        Peer {
            conn,
            win,
            serving: Some(serving),
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.win);
        let _ = self.conn.flush();
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// The peer's serving loop: `TARGETS`, its offers (in `INCR` chunks past
/// `chunk`), nothing else.
fn peer_serve(
    conn: &RustConnection,
    win: Window,
    atoms: &Atoms,
    offers: &[(Atom, Vec<u8>)],
    chunk: usize,
) {
    // A transfer in progress: requestor, property, target, bytes left.
    let mut incr: Option<(Window, Atom, Atom, Vec<u8>)> = None;
    while let Ok(event) = conn.wait_for_event() {
        match event {
            Event::DestroyNotify(e) if e.window == win => return,
            Event::SelectionRequest(e) => {
                let mut property = e.property;
                if e.target == atoms.TARGETS {
                    let mut targets = vec![atoms.TARGETS];
                    targets.extend(offers.iter().map(|(t, _)| *t));
                    conn.change_property32(
                        PropMode::REPLACE,
                        e.requestor,
                        e.property,
                        AtomEnum::ATOM,
                        &targets,
                    )
                    .unwrap();
                } else if let Some((t, bytes)) = offers.iter().find(|(t, _)| *t == e.target) {
                    if bytes.len() > chunk {
                        conn.change_window_attributes(
                            e.requestor,
                            &ChangeWindowAttributesAux::new()
                                .event_mask(EventMask::PROPERTY_CHANGE),
                        )
                        .unwrap();
                        conn.change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            e.property,
                            atoms.INCR,
                            &[bytes.len() as u32],
                        )
                        .unwrap();
                        incr = Some((e.requestor, e.property, *t, bytes.clone()));
                    } else {
                        conn.change_property8(
                            PropMode::REPLACE,
                            e.requestor,
                            e.property,
                            *t,
                            bytes,
                        )
                        .unwrap();
                    }
                } else {
                    property = NONE;
                }
                conn.send_event(
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
                        property,
                    },
                )
                .unwrap();
                conn.flush().unwrap();
            }
            // The requestor took the last chunk: the next one, or the empty
            // one that ends the transfer.
            Event::PropertyNotify(e) if e.state == Property::DELETE => {
                let Some((requestor, prop, target, left)) = incr.take() else {
                    continue;
                };
                if e.window != requestor || e.atom != prop {
                    incr = Some((requestor, prop, target, left));
                    continue;
                }
                let n = left.len().min(chunk);
                conn.change_property8(PropMode::REPLACE, requestor, prop, target, &left[..n])
                    .unwrap();
                conn.flush().unwrap();
                if n > 0 {
                    incr = Some((requestor, prop, target, left[n..].to_vec()));
                }
            }
            _ => {}
        }
    }
}

/// What another client reads off `CLIPBOARD` as `target`.
fn fetch(display: &str, clip: &X11Clip, target: &str) -> Option<Vec<u8>> {
    let reader = Reader::new(Some(display), &clip.owner.atoms)?;
    let atom = reader
        .conn
        .intern_atom(false, target.as_bytes())
        .ok()?
        .reply()
        .ok()?
        .atom;
    reader.convert(atom, 1 << 20)
}

/// The targets another client sees offered, by name.
fn offered(display: &str, clip: &X11Clip) -> Vec<String> {
    let reader = Reader::new(Some(display), &clip.owner.atoms).unwrap();
    reader
        .targets()
        .unwrap()
        .into_iter()
        .map(|t| {
            let name = reader.conn.get_atom_name(t).unwrap().reply().unwrap().name;
            String::from_utf8(name).unwrap()
        })
        .collect()
}

const RTF: &[u8] = b"{\\rtf1\\ansi Plain {\\b BOLD}}";
const HTML: &[u8] = b"<p>Plain <b>BOLD</b></p>";

/// Criterion 1: another app's RTF and HTML read, under the names apps use.
#[test]
fn another_apps_rtf_and_html_are_read() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    assert_eq!(clip.read_rtf(), None, "no owner");
    let peer = Peer::own(
        &x.display,
        &[
            ("UTF8_STRING", b"Plain BOLD"),
            ("text/rtf", RTF),
            ("text/html", HTML),
        ],
        1 << 20,
    );
    assert_eq!(clip.read_rtf().as_deref(), Some(RTF));
    assert_eq!(clip.read_html().as_deref(), Some(HTML));
    assert_eq!(clip.read_text().as_deref(), Some("Plain BOLD"));
    drop(peer);
    // LibreOffice's other RTF names, and an HTML target with a charset.
    let _peer = Peer::own(
        &x.display,
        &[
            ("text/richtext", b"rich"),
            ("application/rtf", b"app"),
            ("text/html;charset=utf-8", HTML),
        ],
        1 << 20,
    );
    assert_eq!(clip.read_rtf().as_deref(), Some(&b"app"[..]));
    assert_eq!(clip.read_html().as_deref(), Some(HTML));
}

#[test]
fn an_image_copy_has_no_rtf_or_html() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    let _peer = Peer::own(&x.display, &[("image/png", b"\x89PNG")], 1 << 20);
    assert_eq!(clip.read_rtf(), None);
    assert_eq!(clip.read_html(), None);
    // And a copy's RTF never goes beside it: nothing changes hands.
    assert!(!clip.offer("x", Extra::Rtf, RTF.to_vec()));
    assert!(!clip.owned());
}

/// A payload past one property chunk comes in `INCR` pieces, and one past
/// the read's bound is not read at all.
#[test]
fn a_large_payload_is_read_in_incr_chunks() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    let big: Vec<u8> = (0..20_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let _peer = Peer::own(&x.display, &[("text/rtf", &big)], 4096);
    assert_eq!(clip.read_rtf().as_deref(), Some(&big[..]));
    let reader = clip.reader().unwrap();
    assert_eq!(reader.convert(clip.owner.atoms.RTF, 10_000), None);
}

/// A UTF-16 page (an older Firefox's) reads as UTF-8.
#[test]
fn a_utf16_html_page_reads_as_utf8() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    let mut page = vec![0xFF, 0xFE];
    for unit in "<b>caf\u{e9}</b>".encode_utf16() {
        page.extend_from_slice(&unit.to_le_bytes());
    }
    let _peer = Peer::own(&x.display, &[("text/html", &page)], 1 << 20);
    assert_eq!(
        clip.read_html().as_deref(),
        Some("<b>caf\u{e9}</b>".as_bytes())
    );
}

/// Criterion 2: the suite takes the copy over only while it holds the
/// copy's text, then serves that text on every text target, the RTF and
/// the HTML beside it.
#[test]
fn a_copy_of_its_own_text_is_served_with_its_rtf_and_html() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    let text = "caf\u{e9} \u{2603}";
    // gpui's copy of the text.
    let gpui = Peer::own(&x.display, &[("UTF8_STRING", text.as_bytes())], 1 << 20);
    // Another app's text there: nothing changes hands.
    assert!(!clip.offer("other", Extra::Rtf, RTF.to_vec()));
    assert!(!clip.owned());
    assert!(clip.offer(text, Extra::Rtf, RTF.to_vec()));
    assert!(clip.owned());
    assert!(clip.offer(text, Extra::Html, HTML.to_vec()));
    drop(gpui);
    for target in [
        "UTF8_STRING",
        "text/plain;charset=utf-8",
        "text/plain;charset=UTF-8",
        "text/plain",
        "TEXT",
    ] {
        assert_eq!(
            fetch(&x.display, &clip, target).as_deref(),
            Some(text.as_bytes()),
            "{target}"
        );
    }
    // STRING is Latin-1: the snowman is past it.
    assert_eq!(
        fetch(&x.display, &clip, "STRING").as_deref(),
        Some(&b"caf\xe9 ?"[..])
    );
    assert_eq!(fetch(&x.display, &clip, "text/rtf").as_deref(), Some(RTF));
    assert_eq!(fetch(&x.display, &clip, "text/html").as_deref(), Some(HTML));
    assert_eq!(fetch(&x.display, &clip, "image/png"), None);
    let targets = offered(&x.display, &clip);
    for name in [
        "TARGETS",
        "SAVE_TARGETS",
        "UTF8_STRING",
        "STRING",
        "text/rtf",
        "text/html",
    ] {
        assert!(targets.iter().any(|t| t == name), "{name} in {targets:?}");
    }
    // The suite's own reads answer from what it holds.
    assert_eq!(clip.read_rtf().as_deref(), Some(RTF));
    assert_eq!(clip.read_html().as_deref(), Some(HTML));
    assert_eq!(clip.read_text().as_deref(), Some(text));
}

/// Criterion 3: another owner's copy (gpui's next) ends the suite's: its
/// reads then answer for that owner, never with the old RTF.
#[test]
fn the_next_copy_ends_ours() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    let gpui = Peer::own(&x.display, &[("UTF8_STRING", b"mine")], 1 << 20);
    assert!(clip.offer("mine", Extra::Rtf, RTF.to_vec()));
    drop(gpui);
    let _next = Peer::own(&x.display, &[("UTF8_STRING", b"plain")], 1 << 20);
    // Right away, whether or not our SelectionClear has been seen yet.
    assert!(!clip.owned());
    assert_eq!(clip.read_rtf(), None);
    assert_eq!(clip.read_html(), None);
    assert_eq!(clip.read_text().as_deref(), Some("plain"));
    // And a later copy of ours takes it again, its data intact.
    std::thread::sleep(Duration::from_millis(50));
    assert!(clip.offer("plain", Extra::Rtf, RTF.to_vec()));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fetch(&x.display, &clip, "text/rtf").as_deref(), Some(RTF));
}

/// A payload one property cannot carry is not offered: the copy stays
/// plain, its owner as it was.
#[test]
fn a_payload_past_one_request_is_not_offered() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    let _gpui = Peer::own(&x.display, &[("UTF8_STRING", b"t")], 1 << 20);
    let huge = vec![b' '; clip.owner.max_payload + 1];
    assert!(!clip.offer("t", Extra::Rtf, huge));
    assert!(!clip.owned());
}

#[test]
fn latin1_replaces_what_it_cannot_say() {
    assert_eq!(latin1("a\u{e9}\u{20ac}"), b"a\xe9?");
}

/// A clipboard manager: owns `CLIPBOARD_MANAGER`, and on `SAVE_TARGETS`
/// reads the app's text and RTF, as a manager does, then says it is done.
/// What it saved comes back through the join handle.
fn manager(display: &str) -> JoinHandle<Vec<(String, Vec<u8>)>> {
    let (conn, screen) = x11rb::connect(Some(display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let win = hidden_window(&conn, root, EventMask::NO_EVENT).unwrap();
    let atoms = Atoms::new(&conn).unwrap().reply().unwrap();
    conn.set_selection_owner(win, atoms.CLIPBOARD_MANAGER, CURRENT_TIME)
        .unwrap();
    conn.flush().unwrap();
    let display = display.to_string();
    std::thread::spawn(move || {
        loop {
            let Event::SelectionRequest(e) = conn.wait_for_event().unwrap() else {
                continue;
            };
            if e.target != atoms.SAVE_TARGETS {
                continue;
            }
            let reader = Reader::new(Some(&display), &atoms).unwrap();
            let saved = [("UTF8_STRING", atoms.UTF8_STRING), ("text/rtf", atoms.RTF)]
                .into_iter()
                .filter_map(|(name, t)| Some((name.to_string(), reader.convert(t, 1 << 20)?)))
                .collect();
            conn.send_event(
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
                    property: e.property,
                },
            )
            .unwrap();
            conn.flush().unwrap();
            return saved;
        }
    })
}

/// At quit the suite's copy goes to the clipboard manager, its RTF too;
/// with no copy of its own, or no manager, there is nothing to hand over.
#[test]
fn at_quit_our_copy_goes_to_the_clipboard_manager() {
    let Some(x) = Xvfb::start() else { return };
    let clip = x.clip();
    assert!(!clip.hand_over(), "nothing of ours");
    let gpui = Peer::own(&x.display, &[("UTF8_STRING", b"keep me")], 1 << 20);
    assert!(clip.offer("keep me", Extra::Rtf, RTF.to_vec()));
    drop(gpui);
    assert!(!clip.hand_over(), "no manager runs");
    let saved = manager(&x.display);
    // The manager reads on its own connection: under a loaded test run it
    // may take longer than the app's 100 ms bound.
    assert!(clip.hand_over_within(Duration::from_secs(10)));
    let saved = saved.join().unwrap();
    assert_eq!(
        saved,
        [
            ("UTF8_STRING".to_string(), b"keep me".to_vec()),
            ("text/rtf".to_string(), RTF.to_vec()),
        ]
    );
}

/// A UTF-16 page (either byte order, with its BOM) becomes UTF-8; anything
/// else is left as it is (the document paste never sniffs it again).
#[test]
fn a_utf16_page_is_converted_where_it_is_read() {
    let page = "<b>caf\u{e9}</b>";
    let le: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(page.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let be: Vec<u8> = [0xFE, 0xFF]
        .into_iter()
        .chain(page.encode_utf16().flat_map(u16::to_be_bytes))
        .collect();
    assert_eq!(utf8_page(le), page.as_bytes());
    assert_eq!(utf8_page(be), page.as_bytes());
    assert_eq!(utf8_page(page.as_bytes().to_vec()), page.as_bytes());
}
