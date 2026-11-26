// Added for docxy (#1966): the formats a copy offers beside gpui's text, and
// its images (#2282).

//! What gpui's own latest copy is on the Wayland selection, beside its text.
//!
//! gpui's clipboard offers text only. The suite adds RTF or HTML to a copy by
//! setting the selection again, with a source that offers the text, those
//! formats and gpui's `pid/<pid>` mime ([`super::super::client::rich_clipboard`]).
//! This keeps, without a connection (so it is testable), which source is
//! gpui's live copy and which bytes each extra mime serves.

use std::{
    io::{ErrorKind, Read},
    os::fd::AsRawFd,
    time::Duration,
};

use gpui::{ClipboardEntry, ClipboardItem};

/// gpui's own latest copy: the `wl_data_source` that is the selection (until
/// the compositor cancels it), and the formats it serves beside the text.
pub(crate) struct OwnCopy<S> {
    source: Option<S>,
    formats: Vec<(String, Vec<u8>)>,
}

impl<S: PartialEq> OwnCopy<S> {
    pub(crate) fn new() -> Self {
        Self {
            source: None,
            formats: Vec::new(),
        }
    }

    /// gpui copied something new: no source is known to carry it yet (the
    /// write can still be skipped for want of a serial), and the old formats
    /// belong to the old copy.
    pub(crate) fn copied(&mut self) {
        self.source = None;
        self.formats.clear();
    }

    /// `source` was just set as the selection.
    pub(crate) fn set_source(&mut self, source: S) {
        self.source = Some(source);
    }

    /// The compositor cancelled `source`: forgotten when it is the tracked
    /// one. Replacing gpui's source with the suite's cancels the old one,
    /// which must not forget the new.
    pub(crate) fn cancelled(&mut self, source: &S) {
        if self.source.as_ref() == Some(source) {
            self.source = None;
        }
    }

    /// The selection changed to an offer that is gpui's own (`Some(true)`,
    /// it carries `pid/<pid>`), another app's, or none. Anything but gpui's
    /// own ends the copy, also when the compositor refused gpui's write and
    /// sent no `cancelled` (mutter refuses an older serial so). The formats
    /// stay: an own offer that comes after (the echo of the suite's source,
    /// behind a stale one) serves them through the `pid/` path.
    pub(crate) fn selection_changed(&mut self, offer_is_ours: Option<bool>) {
        if offer_is_ours != Some(true) {
            self.source = None;
        }
    }

    /// Whether gpui's latest copy is still the selection, as far as any
    /// event so far says.
    pub(crate) fn alive(&self) -> bool {
        self.source.is_some()
    }

    /// Add `formats`, a mime already held replaced.
    pub(crate) fn merge(&mut self, formats: Vec<(String, Vec<u8>)>) {
        for (mime, bytes) in formats {
            match self.formats.iter_mut().find(|(held, _)| *held == mime) {
                Some(held) => held.1 = bytes,
                None => self.formats.push((mime, bytes)),
            }
        }
    }

    /// The bytes a format `mime` serves, if the copy has it.
    pub(crate) fn bytes(&self, mime: &str) -> Option<&[u8]> {
        self.formats
            .iter()
            .find(|(held, _)| held == mime)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// What a source for this copy offers: the text mimes, each format's
    /// mime, and gpui's own `self_mime`.
    pub(crate) fn mime_types(&self, text: &[&str], self_mime: &str) -> Vec<String> {
        text.iter()
            .map(|mime| mime.to_string())
            .chain(self.formats.iter().map(|(mime, _)| mime.clone()))
            .chain(std::iter::once(self_mime.to_string()))
            .collect()
    }

    /// What a `send` of `mime` writes: a format's bytes, else `text` (gpui's
    /// own answer to every mime, as before).
    pub(crate) fn send_bytes(&self, mime: &str, text: Option<&str>) -> Option<Vec<u8>> {
        self.bytes(mime)
            .map(<[u8]>::to_vec)
            .or_else(|| text.map(|text| text.as_bytes().to_vec()))
    }
}

/// `item`'s images as formats (mime, bytes) (#2282): entry order, the first
/// of each mime.
pub(crate) fn image_formats(item: &ClipboardItem) -> Vec<(String, Vec<u8>)> {
    let mut formats: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in item.entries() {
        if let ClipboardEntry::Image(image) = entry {
            let mime = image.format.mime_type();
            if !formats.iter().any(|(held, _)| held == mime) {
                formats.push((mime.to_owned(), image.bytes.clone()));
            }
        }
    }
    formats
}

/// Whether a copy of `item` offers the text mimes (#2282): when it has text,
/// or no image (gpui's empty answer, as before). A copy of images alone
/// offers only theirs.
pub(crate) fn offers_text(item: &ClipboardItem) -> bool {
    item.text().is_some()
        || !item
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
}

/// Who answers a read of the selection.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Whose {
    /// gpui's own copy, from memory: its source is alive, or the offer is
    /// its own (`pid/<pid>`). Never through a pipe: gpui serves its pipes on
    /// the thread that would be waiting on it.
    Own,
    /// Another app's, through the offer.
    Offer,
    /// No selection.
    Nothing,
}

/// Who answers a read, given whether gpui's own copy is `alive` and, when
/// there is an offer, whether it carries gpui's `pid/<pid>` mime.
pub(crate) fn whose(alive: bool, offer_is_ours: Option<bool>) -> Whose {
    match (alive, offer_is_ours) {
        (true, _) | (false, Some(true)) => Whose::Own,
        (false, Some(false)) => Whose::Offer,
        (false, None) => Whose::Nothing,
    }
}

/// Whether gpui's copy of `held` is the suite's copy of `text`, line ends
/// compared loosely.
pub(crate) fn same_text(held: &str, text: &str) -> bool {
    held.replace("\r\n", "\n") == text.replace("\r\n", "\n")
}

/// Read `fd` to its end, as `read_fd_with_timeout` does, but stop with
/// `Ok(None)` once more than `max` bytes came: a paste of a huge rich copy
/// takes its text rather than copying it all to find out.
pub(crate) fn read_fd_bounded(
    mut fd: filedescriptor::FileDescriptor,
    timeout: Duration,
    max: usize,
) -> anyhow::Result<Option<Vec<u8>>> {
    fd.set_non_blocking(true)?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let mut poll_fds = [filedescriptor::pollfd {
            fd: fd.as_raw_fd(),
            events: filedescriptor::POLLIN,
            revents: 0,
        }];
        let ready = match filedescriptor::poll(&mut poll_fds, Some(timeout)) {
            Ok(ready) => ready,
            Err(filedescriptor::Error::Poll(err)) if err.kind() == ErrorKind::Interrupted => {
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        if ready == 0 {
            anyhow::bail!("timed out waiting for data on pipe after {timeout:?}");
        }
        match fd.read(&mut chunk) {
            Ok(0) => return Ok(Some(buffer)),
            Ok(len) => {
                if buffer.len() + len > max {
                    return Ok(None);
                }
                buffer.extend_from_slice(&chunk[..len]);
            }
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn copy_with(formats: &[(&str, &str)]) -> OwnCopy<u32> {
        let mut copy = OwnCopy::new();
        copy.merge(
            formats
                .iter()
                .map(|(mime, bytes)| (mime.to_string(), bytes.as_bytes().to_vec()))
                .collect(),
        );
        copy
    }

    #[test]
    fn merging_replaces_a_mime_and_keeps_the_others() {
        let mut copy = copy_with(&[("text/rtf", "{\\rtf1 a}"), ("text/html", "<b>a</b>")]);
        copy.merge(vec![("text/rtf".into(), b"{\\rtf1 b}".to_vec())]);
        assert_eq!(copy.bytes("text/rtf"), Some(&b"{\\rtf1 b}"[..]));
        assert_eq!(copy.bytes("text/html"), Some(&b"<b>a</b>"[..]));
        assert_eq!(copy.bytes("image/png"), None);
    }

    #[test]
    fn a_new_copy_forgets_the_source_and_the_formats() {
        let mut copy = copy_with(&[("text/rtf", "{\\rtf1 a}")]);
        copy.set_source(1);
        assert!(copy.alive());
        copy.copied();
        assert!(!copy.alive());
        assert_eq!(copy.bytes("text/rtf"), None);
    }

    #[test]
    fn cancelling_the_replaced_source_keeps_the_new_one() {
        let mut copy = OwnCopy::new();
        copy.set_source(1);
        // The suite's source replaces gpui's; the compositor then cancels 1.
        copy.set_source(2);
        copy.cancelled(&1);
        assert!(copy.alive());
        copy.cancelled(&2);
        assert!(!copy.alive());
    }

    #[test]
    fn a_source_offers_text_formats_and_gpuis_own_mime() {
        let copy = copy_with(&[("text/rtf", "{\\rtf1 a}")]);
        assert_eq!(
            copy.mime_types(&["text/plain;charset=utf-8", "UTF8_STRING"], "pid/7"),
            [
                "text/plain;charset=utf-8",
                "UTF8_STRING",
                "text/rtf",
                "pid/7"
            ]
        );
    }

    #[test]
    fn a_send_serves_a_formats_bytes_and_else_the_text() {
        let copy = copy_with(&[("text/rtf", "{\\rtf1 a}")]);
        assert_eq!(
            copy.send_bytes("text/rtf", Some("a")).as_deref(),
            Some(&b"{\\rtf1 a}"[..])
        );
        assert_eq!(
            copy.send_bytes("UTF8_STRING", Some("a")).as_deref(),
            Some(&b"a"[..])
        );
        // gpui's old answer to any mime: its text.
        assert_eq!(
            copy.send_bytes("text/html", Some("a")).as_deref(),
            Some(&b"a"[..])
        );
        assert_eq!(copy.send_bytes("UTF8_STRING", None), None);
    }

    /// The live source answers before any offer: right after gpui's write the
    /// compositor's `selection` echo has not been dispatched, and the offer
    /// is still the previous owner's.
    #[test]
    fn a_live_own_copy_answers_before_a_stale_offer() {
        assert_eq!(whose(true, Some(false)), Whose::Own);
        assert_eq!(whose(true, None), Whose::Own);
        assert_eq!(whose(false, Some(true)), Whose::Own);
        assert_eq!(whose(false, Some(false)), Whose::Offer);
        assert_eq!(whose(false, None), Whose::Nothing);
    }

    /// A write skipped for want of a serial leaves no live source behind:
    /// the offer answers again, as before.
    #[test]
    fn a_copy_without_a_source_leaves_the_offer_to_answer() {
        let mut copy = OwnCopy::new();
        copy.set_source(1);
        copy.copied();
        assert_eq!(whose(copy.alive(), Some(false)), Whose::Offer);
        copy.set_source(2);
        assert_eq!(whose(copy.alive(), Some(false)), Whose::Own);
    }

    /// A selection that is not gpui's own ends its copy (a refused write
    /// gets no `cancelled`); its own echo keeps it, and the formats stay for
    /// the `pid/` path either way.
    #[test]
    fn a_selection_not_ours_ends_the_copy() {
        let mut copy = copy_with(&[("text/rtf", "{\\rtf1 a}")]);
        copy.set_source(1);
        copy.selection_changed(Some(true));
        assert!(copy.alive());
        copy.selection_changed(Some(false));
        assert!(!copy.alive());
        assert_eq!(whose(copy.alive(), Some(false)), Whose::Offer);
        assert_eq!(whose(copy.alive(), Some(true)), Whose::Own);
        assert_eq!(copy.bytes("text/rtf"), Some(&b"{\\rtf1 a}"[..]));
        copy.set_source(2);
        copy.selection_changed(None);
        assert!(!copy.alive());
        assert_eq!(whose(copy.alive(), None), Whose::Nothing);
    }

    #[test]
    fn text_compares_loosely_over_line_ends() {
        assert!(same_text("a\r\nb", "a\nb"));
        assert!(!same_text("a", "b"));
    }

    fn piped(bytes: &[u8]) -> filedescriptor::FileDescriptor {
        let mut pipe = filedescriptor::Pipe::new().unwrap();
        pipe.write.write_all(bytes).unwrap();
        drop(pipe.write);
        pipe.read
    }

    #[test]
    fn a_bounded_read_stops_past_max() {
        let timeout = Duration::from_secs(1);
        assert_eq!(
            read_fd_bounded(piped(b"12345"), timeout, 5).unwrap(),
            Some(b"12345".to_vec())
        );
        assert_eq!(read_fd_bounded(piped(b"123456"), timeout, 5).unwrap(), None);
        assert_eq!(
            read_fd_bounded(piped(b""), timeout, 5).unwrap(),
            Some(Vec::new())
        );
    }

    const TEXT: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

    fn image(format: gpui::ImageFormat, bytes: &[u8]) -> ClipboardEntry {
        ClipboardEntry::Image(gpui::Image::from_bytes(format, bytes.to_vec()))
    }

    fn text(text: &str) -> ClipboardEntry {
        ClipboardEntry::String(gpui::ClipboardString::new(text.to_owned()))
    }

    #[test]
    fn an_items_images_are_formats_in_order_the_first_of_a_mime() {
        use gpui::ImageFormat::{Gif, Jpeg, Png};
        let item = ClipboardItem {
            entries: vec![
                image(Jpeg, b"jpeg"),
                text("caption"),
                image(Png, b"png one"),
                image(Png, b"png two"),
                image(Gif, b"gif"),
            ],
        };
        assert_eq!(
            image_formats(&item),
            [
                ("image/jpeg".to_owned(), b"jpeg".to_vec()),
                ("image/png".to_owned(), b"png one".to_vec()),
                ("image/gif".to_owned(), b"gif".to_vec()),
            ]
        );
        assert!(image_formats(&ClipboardItem::new_string("t".into())).is_empty());
    }

    #[test]
    fn text_mimes_are_offered_with_text_or_without_an_image() {
        let png = || image(gpui::ImageFormat::Png, b"png");
        assert!(offers_text(&ClipboardItem::new_string("t".into())));
        assert!(offers_text(&ClipboardItem { entries: vec![] }));
        assert!(!offers_text(&ClipboardItem {
            entries: vec![png()]
        }));
        assert!(offers_text(&ClipboardItem {
            entries: vec![text("t"), png()]
        }));
    }

    /// A copy of an image offers its mime and gpui's own, no text; with text
    /// both. The suite's RTF merged after keeps the image beside it.
    #[test]
    fn an_images_copy_offers_and_sends_its_mime() {
        let png = image(gpui::ImageFormat::Png, b"png");
        let alone = ClipboardItem {
            entries: vec![png.clone()],
        };
        let mut copy: OwnCopy<u32> = OwnCopy::new();
        copy.merge(image_formats(&alone));
        assert!(!offers_text(&alone));
        assert_eq!(copy.mime_types(&[], "pid/1"), ["image/png", "pid/1"]);
        assert_eq!(copy.send_bytes("image/png", None), Some(b"png".to_vec()));

        let both = ClipboardItem {
            entries: vec![text("t"), png],
        };
        copy.copied();
        copy.merge(image_formats(&both));
        copy.merge(vec![("text/rtf".into(), b"{\\rtf1 t}".to_vec())]);
        assert_eq!(
            copy.mime_types(&TEXT, "pid/1"),
            [
                "text/plain;charset=utf-8",
                "UTF8_STRING",
                "text/plain",
                "image/png",
                "text/rtf",
                "pid/1",
            ]
        );
        assert_eq!(
            copy.send_bytes("image/png", Some("t")),
            Some(b"png".to_vec())
        );
        assert_eq!(
            copy.send_bytes("text/plain", Some("t")),
            Some(b"t".to_vec())
        );
    }
}
