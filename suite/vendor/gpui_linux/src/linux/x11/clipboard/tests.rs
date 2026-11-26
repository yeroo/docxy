// Added for docxy (#2282): a copy's images on the X11 clipboard.

//! gpui's X11 clipboard against another X client, each test on its own
//! private Xvfb: never the developer's `DISPLAY`, whose clipboard an owner
//! would take, and never one display for two tests, which run in parallel
//! and would race on one `CLIPBOARD`. The owner is a `Clipboard` on its own
//! `Inner`, never the global one.

use super::*;
use gpui::ClipboardString;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// A private X server, killed on drop.
struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    /// A fresh Xvfb on a free display it picks (`-displayfd`). `None`, with
    /// a note, where there is no Xvfb, except under CI, which always has it:
    /// there, a missing one fails the test.
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
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// gpui's clipboard on a private display, serving on its own thread. Only
/// the `Clipboard` and the thread hold its `Inner`, so `Clipboard::drop`
/// (which acts at three owners) does nothing.
struct Owner {
    clipboard: Clipboard,
    serving: Option<JoinHandle<()>>,
}

impl Owner {
    fn new(display: &str) -> Owner {
        let inner = Arc::new(Inner::new(Some(display)).expect("the private display opens"));
        let serving = std::thread::spawn({
            let inner = Arc::clone(&inner);
            move || {
                if let Err(error) = serve_requests(inner) {
                    eprintln!("serve_requests ended: {error}");
                }
            }
        });
        Owner {
            clipboard: Clipboard { inner },
            serving: Some(serving),
        }
    }

    fn inner(&self) -> &Inner {
        &self.clipboard.inner
    }

    /// `item` written to `CLIPBOARD`, its ownership through the server
    /// before another client asks.
    fn copy(&self, item: &ClipboardItem) {
        self.clipboard
            .set_item(item, ClipboardKind::Clipboard, WaitConfig::None)
            .expect("the write takes");
        assert!(self.clipboard.is_owner(ClipboardKind::Clipboard));
    }

    fn serving(&self) -> bool {
        !self.inner().serve_stopped.load(Ordering::Relaxed)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let inner = self.inner();
        let _ = inner.server.conn.destroy_window(inner.server.win_id);
        let _ = inner.server.conn.flush();
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Another app on the same display, reading `CLIPBOARD` through the server.
struct Reader {
    inner: Inner,
    display: String,
}

impl Reader {
    fn new(display: &str) -> Reader {
        Reader {
            inner: Inner::new(Some(display)).expect("the private display opens"),
            display: display.to_owned(),
        }
    }

    fn atom(&self, name: &str) -> Atom {
        self.inner
            .server
            .conn
            .intern_atom(false, name.as_bytes())
            .expect("intern_atom")
            .reply()
            .expect("an atom")
            .atom
    }

    fn context(&self) -> XContext {
        XContext::new(Some(&self.display)).expect("the private display opens")
    }

    /// The owner's `TARGETS`, by name.
    fn targets(&self) -> Vec<String> {
        let data = self
            .inner
            .read_single(
                &self.context(),
                ClipboardKind::Clipboard,
                self.inner.atoms.TARGETS,
            )
            .expect("TARGETS answered");
        assert_eq!(data.format, self.inner.atoms.ATOM);
        Inner::parse_formats(&data.bytes)
            .into_iter()
            .map(|atom| self.inner.query_atom_name(atom).expect("an atom name"))
            .collect()
    }

    /// The selection converted to `target`, `None` when refused.
    fn fetch(&self, target: &str) -> Option<Vec<u8>> {
        let target = self.atom(target);
        match self
            .inner
            .read_single(&self.context(), ClipboardKind::Clipboard, target)
        {
            Ok(data) => {
                assert_eq!(data.format, target);
                Some(data.bytes)
            }
            Err(Error::ContentNotAvailable) => None,
            Err(error) => panic!("the read failed: {error}"),
        }
    }
}

fn image(format: ImageFormat, bytes: &[u8]) -> Image {
    Image::from_bytes(format, bytes.to_vec())
}

fn item(text: Option<&str>, images: &[Image]) -> ClipboardItem {
    let mut entries = Vec::new();
    if let Some(text) = text {
        entries.push(ClipboardEntry::String(ClipboardString::new(
            text.to_owned(),
        )));
    }
    entries.extend(images.iter().cloned().map(ClipboardEntry::Image));
    ClipboardItem { entries }
}

const JPEG: &[u8] = b"\xff\xd8\xff\xe0 a jpeg";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n a png";

#[test]
fn an_image_item_is_served_as_its_own_mime() {
    let Some(x) = Xvfb::start() else { return };
    let owner = Owner::new(&x.display);
    let reader = Reader::new(&x.display);
    owner.copy(&ClipboardItem::new_image(&image(ImageFormat::Jpeg, JPEG)));

    let targets = reader.targets();
    assert_eq!(targets, ["TARGETS", "SAVE_TARGETS", "image/jpeg"]);
    assert_eq!(reader.fetch("image/jpeg").as_deref(), Some(JPEG));
    assert_eq!(reader.fetch("UTF8_STRING"), None);
}

#[test]
fn text_and_image_are_both_offered() {
    let Some(x) = Xvfb::start() else { return };
    let owner = Owner::new(&x.display);
    let reader = Reader::new(&x.display);
    owner.copy(&item(Some("caption"), &[image(ImageFormat::Png, PNG)]));

    let targets = reader.targets();
    assert_eq!(
        targets,
        [
            "TARGETS",
            "SAVE_TARGETS",
            "UTF8_STRING",
            "text/plain;charset=utf-8",
            "text/plain;charset=UTF-8",
            "image/png",
        ]
    );
    assert_eq!(
        reader.fetch("UTF8_STRING").as_deref(),
        Some(&b"caption"[..])
    );
    assert_eq!(reader.fetch("image/png").as_deref(), Some(PNG));
    // The two names `TARGETS` lists beside `UTF8_STRING` serve its text.
    for name in ["text/plain;charset=utf-8", "text/plain;charset=UTF-8"] {
        assert_eq!(
            reader.fetch(name).as_deref(),
            Some(&b"caption"[..]),
            "{name}"
        );
    }
}

/// A text item serves what gpui always wrote, and so does an item with
/// neither text nor image (an empty `UTF8_STRING`). Images keep their entry
/// order, the first of a format.
#[test]
fn item_data_is_the_text_then_each_format_once() {
    let Some(x) = Xvfb::start() else { return };
    let reader = Reader::new(&x.display);
    let atoms = &reader.inner.atoms;
    let summary = |item: &ClipboardItem| {
        item_data(atoms, item)
            .into_iter()
            .map(|data| {
                (
                    reader.inner.query_atom_name(data.format).unwrap(),
                    data.bytes,
                )
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(
        summary(&ClipboardItem::new_string("plain".into())),
        [("UTF8_STRING".to_owned(), b"plain".to_vec())]
    );
    assert_eq!(
        summary(&ClipboardItem { entries: vec![] }),
        [("UTF8_STRING".to_owned(), Vec::new())]
    );
    assert_eq!(
        summary(&item(
            None,
            &[
                image(ImageFormat::Gif, b"gif one"),
                image(ImageFormat::Png, PNG),
                image(ImageFormat::Gif, b"gif two"),
            ]
        )),
        [
            ("image/gif".to_owned(), b"gif one".to_vec()),
            ("image/png".to_owned(), PNG.to_vec()),
        ]
    );
}

#[test]
fn set_image_uses_the_images_format() {
    let Some(x) = Xvfb::start() else { return };
    let owner = Owner::new(&x.display);
    let reader = Reader::new(&x.display);
    owner
        .clipboard
        .set_image(
            image(ImageFormat::Gif, b"GIF89a"),
            ClipboardKind::Clipboard,
            WaitConfig::None,
        )
        .expect("the write takes");
    assert!(owner.clipboard.is_owner(ClipboardKind::Clipboard));

    assert_eq!(reader.targets(), ["TARGETS", "SAVE_TARGETS", "image/gif"]);
    assert_eq!(reader.fetch("image/gif").as_deref(), Some(&b"GIF89a"[..]));
    assert_eq!(reader.fetch("image/png"), None);
}

/// A payload one `ChangeProperty` cannot carry is neither offered nor sent:
/// sending it would fail and end `serve_requests`, and with it every later
/// copy of the process.
#[test]
fn a_payload_past_one_request_is_refused_not_fatal() {
    let Some(x) = Xvfb::start() else { return };
    let owner = Owner::new(&x.display);
    let reader = Reader::new(&x.display);
    let past = owner.inner().server.conn.maximum_request_bytes() + 1;
    owner.copy(&item(
        Some("small"),
        &[Image::from_bytes(ImageFormat::Png, vec![0; past])],
    ));

    assert!(!reader.targets().iter().any(|target| target == "image/png"));
    assert_eq!(reader.fetch("image/png"), None);
    assert_eq!(reader.fetch("UTF8_STRING").as_deref(), Some(&b"small"[..]));
    assert!(owner.serving());

    owner.copy(&ClipboardItem::new_string("next".into()));
    assert_eq!(reader.fetch("UTF8_STRING").as_deref(), Some(&b"next"[..]));
    assert!(owner.serving());
}
