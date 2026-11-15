// Modified for docxy (#1966): a copy's extra formats (RTF/HTML) beside its text.

use std::{
    fs::File,
    io::{ErrorKind, Write},
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
};

use calloop::{LoopHandle, PostAction};
use filedescriptor::Pipe;
use strum::IntoEnumIterator;
use wayland_client::{
    Connection,
    protocol::{wl_data_offer::WlDataOffer, wl_data_source::WlDataSource},
};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1;

use crate::linux::{
    WaylandClientStatePtr,
    platform::{PIPE_READ_TIMEOUT, read_fd_with_timeout},
};
use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat, hash};

mod formats;
pub(crate) use formats::{OwnCopy, Whose, same_text, whose};

/// Text mime types that we'll offer to other programs.
pub(crate) const TEXT_MIME_TYPES: [&str; 3] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
pub(crate) const FILE_LIST_MIME_TYPE: &str = "text/uri-list";

/// Text mime types that we'll accept from other programs.
pub(crate) const ALLOWED_TEXT_MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "UTF8_STRING"];

pub(crate) struct Clipboard {
    connection: Connection,
    loop_handle: LoopHandle<'static, WaylandClientStatePtr>,
    self_mime: String,

    // Internal clipboard
    contents: Option<ClipboardItem>,
    primary_contents: Option<ClipboardItem>,

    // External clipboard
    cached_read: Option<ClipboardItem>,
    current_offer: Option<DataOffer<WlDataOffer>>,
    cached_primary_read: Option<ClipboardItem>,
    current_primary_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>,

    // gpui's latest copy as the selection, and its formats beside the text
    // (#1966).
    own: OwnCopy<WlDataSource>,
}

pub(crate) trait ReceiveData {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>);
}

impl ReceiveData for WlDataOffer {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

impl ReceiveData for ZwpPrimarySelectionOfferV1 {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

#[derive(Clone, Debug)]
/// Wrapper for `WlDataOffer` and `ZwpPrimarySelectionOfferV1`, used to help track mime types.
pub(crate) struct DataOffer<T: ReceiveData> {
    pub inner: T,
    mime_types: Vec<String>,
}

impl<T: ReceiveData> DataOffer<T> {
    pub fn new(offer: T) -> Self {
        Self {
            inner: offer,
            mime_types: Vec::new(),
        }
    }

    pub fn add_mime_type(&mut self, mime_type: String) {
        self.mime_types.push(mime_type)
    }

    fn has_mime_type(&self, mime_type: &str) -> bool {
        self.mime_types.iter().any(|t| t == mime_type)
    }

    fn read_bytes(&self, connection: &Connection, mime_type: &str) -> Option<Vec<u8>> {
        let pipe = Pipe::new().unwrap();
        self.inner.receive_data(mime_type.to_string(), unsafe {
            BorrowedFd::borrow_raw(pipe.write.as_raw_fd())
        });
        let fd = pipe.read;
        drop(pipe.write);

        connection.flush().unwrap();

        match read_fd_with_timeout(fd, PIPE_READ_TIMEOUT) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    /// `mime` read, `None` past `max` bytes, empty, or on error (#1966).
    fn read_bytes_bounded(
        &self,
        connection: &Connection,
        mime_type: &str,
        max: usize,
    ) -> Option<Vec<u8>> {
        let pipe = Pipe::new().ok()?;
        self.inner.receive_data(mime_type.to_string(), unsafe {
            BorrowedFd::borrow_raw(pipe.write.as_raw_fd())
        });
        let fd = pipe.read;
        drop(pipe.write);
        connection.flush().ok()?;
        match formats::read_fd_bounded(fd, PIPE_READ_TIMEOUT, max) {
            Ok(bytes) => bytes.filter(|bytes| !bytes.is_empty()),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    fn read_text(&self, connection: &Connection) -> Option<ClipboardItem> {
        let mime_type = self.mime_types.iter().find(|&mime_type| {
            ALLOWED_TEXT_MIME_TYPES
                .iter()
                .any(|&allowed| allowed == mime_type)
        })?;
        let bytes = self.read_bytes(connection, mime_type)?;
        let text_content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(e) => {
                log::error!("Failed to convert clipboard content to UTF-8: {}", e);
                return None;
            }
        };

        // Normalize the text to unix line endings, otherwise
        // copying from eg: firefox inserts a lot of blank
        // lines, and that is super annoying.
        let result = text_content.replace("\r\n", "\n");
        Some(ClipboardItem::new_string(result))
    }

    fn read_image(&self, connection: &Connection) -> Option<ClipboardItem> {
        for format in ImageFormat::iter() {
            let mime_type = format.mime_type();
            if !self.has_mime_type(mime_type) {
                continue;
            }

            if let Some(bytes) = self.read_bytes(connection, mime_type) {
                let id = hash(&bytes);
                return Some(ClipboardItem {
                    entries: vec![ClipboardEntry::Image(Image { format, bytes, id })],
                });
            }
        }
        None
    }
}

impl Clipboard {
    pub fn new(
        connection: Connection,
        loop_handle: LoopHandle<'static, WaylandClientStatePtr>,
    ) -> Self {
        Self {
            connection,
            loop_handle,
            self_mime: format!("pid/{}", std::process::id()),

            contents: None,
            primary_contents: None,

            cached_read: None,
            current_offer: None,
            cached_primary_read: None,
            current_primary_offer: None,

            own: OwnCopy::new(),
        }
    }

    pub fn set(&mut self, item: ClipboardItem) {
        self.contents = Some(item);
        self.own.copied();
    }

    /// `source` was just set as the selection for `contents` (#1966).
    pub fn set_source(&mut self, source: WlDataSource) {
        self.own.set_source(source);
    }

    /// The compositor cancelled `source` (#1966).
    pub fn source_cancelled(&mut self, source: &WlDataSource) {
        self.own.cancelled(source);
    }

    /// Whether gpui's latest copy is still the selection and its text is
    /// `text`: the copy the suite adds formats to (#1966).
    pub fn own_copy_is(&self, text: &str) -> bool {
        self.own.alive()
            && self
                .contents
                .as_ref()
                .and_then(|contents| contents.text())
                .is_some_and(|held| same_text(&held, text))
    }

    /// Add `formats` beside the text of gpui's copy (#1966).
    pub fn merge_formats(&mut self, formats: Vec<(String, Vec<u8>)>) {
        self.own.merge(formats);
    }

    /// What a source for gpui's copy offers: text, formats, `pid/<pid>` (#1966).
    pub fn own_mime_types(&self) -> Vec<String> {
        self.own.mime_types(&TEXT_MIME_TYPES, &self.self_mime)
    }

    /// Send what is queued to the compositor (#1966).
    pub fn flush(&self) -> Result<(), wayland_client::backend::WaylandError> {
        self.connection.flush()
    }

    fn whose(&self) -> Whose {
        whose(
            self.own.alive(),
            self.current_offer
                .as_ref()
                .map(|offer| offer.has_mime_type(&self.self_mime)),
        )
    }

    /// The selection's mime types (#1966).
    pub fn mime_types(&self) -> Option<Vec<String>> {
        match self.whose() {
            Whose::Own => Some(self.own_mime_types()),
            Whose::Offer => self
                .current_offer
                .as_ref()
                .map(|offer| offer.mime_types.clone()),
            Whose::Nothing => None,
        }
    }

    /// The selection's `mime` read, `None` past `max` bytes, empty, or on
    /// error (#1966). gpui's own copy from memory; another app's through
    /// the pipe, waiting up to `PIPE_READ_TIMEOUT` as a text paste does.
    pub fn read_format(&self, mime: &str, max: usize) -> Option<Vec<u8>> {
        match self.whose() {
            Whose::Own => self
                .own
                .bytes(mime)
                .filter(|bytes| !bytes.is_empty() && bytes.len() <= max)
                .map(<[u8]>::to_vec),
            Whose::Offer => {
                let offer = self.current_offer.as_ref()?;
                if !offer.has_mime_type(mime) {
                    return None;
                }
                offer.read_bytes_bounded(&self.connection, mime, max)
            }
            Whose::Nothing => None,
        }
    }

    pub fn set_primary(&mut self, item: ClipboardItem) {
        self.primary_contents = Some(item);
    }

    pub fn set_offer(&mut self, data_offer: Option<DataOffer<WlDataOffer>>) {
        self.cached_read = None;
        self.current_offer = data_offer;
    }

    pub fn set_primary_offer(&mut self, data_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>) {
        self.cached_primary_read = None;
        self.current_primary_offer = data_offer;
    }

    pub fn self_mime(&self) -> String {
        self.self_mime.clone()
    }

    pub fn send(&self, mime_type: String, fd: OwnedFd) {
        let text = self.contents.as_ref().and_then(|contents| contents.text());
        if let Some(bytes) = self.own.send_bytes(&mime_type, text.as_deref()) {
            self.send_internal(fd, bytes);
        }
    }

    pub fn send_primary(&self, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self
            .primary_contents
            .as_ref()
            .and_then(|contents| contents.text())
        {
            self.send_internal(fd, text.as_bytes().to_owned());
        }
    }

    pub fn read(&mut self) -> Option<ClipboardItem> {
        // gpui's own copy while its source is alive: right after a write the
        // compositor's `selection` echo is not dispatched yet, and the offer
        // is still the previous owner's (#1966).
        if self.own.alive() {
            return self.contents.clone();
        }
        let offer = self.current_offer.as_ref()?;
        if let Some(cached) = self.cached_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_read = Some(item.clone());
        Some(item)
    }

    pub fn read_primary(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_primary_offer.as_ref()?;
        if let Some(cached) = self.cached_primary_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.primary_contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_primary_read = Some(item.clone());
        Some(item)
    }

    fn send_internal(&self, fd: OwnedFd, bytes: Vec<u8>) {
        let mut written = 0;
        self.loop_handle
            .insert_source(
                calloop::generic::Generic::new(
                    File::from(fd),
                    calloop::Interest::WRITE,
                    calloop::Mode::Level,
                ),
                move |_, file, _| {
                    let file = unsafe { file.get_mut() };
                    loop {
                        match file.write(&bytes[written..]) {
                            Ok(n) if written + n == bytes.len() => {
                                written += n;
                                break Ok(PostAction::Remove);
                            }
                            Ok(n) => written += n,
                            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                                break Ok(PostAction::Continue);
                            }
                            Err(_) => break Ok(PostAction::Remove),
                        }
                    }
                },
            )
            .unwrap();
    }
}
