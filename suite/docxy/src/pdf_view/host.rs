//! The window's side of the PDF view (#2245): which files open as one and
//! how, its session row, Convert to editable document, its keys and the
//! presses its drawing routes here, and the frame tick that takes the
//! worker's answers in.

use super::worker::Spawn;
use super::{Field, PdfView};
use crate::*;
use std::path::Path;
use std::sync::Arc;

/// The status line after Ctrl+S on a PDF view.
pub(crate) const PDF_NO_SAVE: &str = "The PDF view is read-only and never writes the PDF; \
     Convert to editable document makes a copy you can save";
/// The status line after Save As on a PDF view.
pub(crate) const PDF_NO_SAVE_AS: &str = "Save As is not available for a PDF view; \
     Convert to editable document makes a copy you can save";
/// How often the frame looks for the worker's answers while one is due.
const POLL: std::time::Duration = std::time::Duration::from_millis(15);

/// The status bar's hint on a PDF view: the Mac's chords on macOS.
pub(crate) fn status_hint(macos: bool) -> &'static str {
    if macos {
        "⌘F find · ⌘= / ⌘- zoom · ⌘0 fit width · Page Up/Down · ⌘A ⌘C copy"
    } else {
        "Ctrl+F find · Ctrl+= / Ctrl+- zoom · Ctrl+0 fit width · PgUp/PgDn · Ctrl+A Ctrl+C copy"
    }
}

/// Whether `path` holds a PDF: its first bytes say so, as the importer's
/// sniff reads them (an RTF or page that only mentions `%PDF-` is not one).
pub(crate) fn is_pdf_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(4096);
    if f.by_ref().take(4096).read_to_end(&mut head).is_err() || head.is_empty() {
        return false;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    docxcore::import::sniff(&head, &ext) == docxcore::import::Format::Pdf
}

/// A PDF view's tab on `path`, at `page` and `zoom` when a session says so;
/// `None` when `path` is no PDF (it opens as a document). A PDF whose pages
/// the worker cannot read is a failed tab that says why.
pub(crate) fn pdf_view_tab(path: &Path, at: Option<PdfAt>) -> Option<DocTab> {
    if !is_pdf_file(path) {
        return None;
    }
    let mut tab = blank_docx_tab(&file_name(path));
    tab.kind = Kind::Pdf;
    tab.path = Some(path.to_path_buf());
    match PdfView::open(path, Spawn::default_for_process()) {
        Ok(mut v) => {
            tab.status = loaded_status(v.page_count()).into();
            if let Some(at) = at {
                v.zoom = super::clamp_zoom(at.zoom);
                v.restore_page = Some(at.page);
            }
            tab.surface = Surface::Pdf(Box::new(v));
        }
        Err(why) => {
            tab.status = format!("load error: {why}").into();
            tab.load_failed = true;
            tab.surface = Surface::Placeholder;
        }
    }
    Some(tab)
}

/// A PDF view's status once open: `loaded (PDF, N pages)`.
pub(crate) fn loaded_status(pages: usize) -> String {
    let s = if pages == 1 { "" } else { "s" };
    format!("loaded (PDF, {pages} page{s})")
}

/// Where a PDF view was, as the session keeps it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PdfAt {
    /// 0-based.
    pub page: usize,
    pub zoom: f32,
}

/// The session row of a PDF view: a document row (an older build reads
/// it, and converts the PDF), with where the view was.
pub(crate) fn pdf_at(tab: &DocTab) -> Option<PdfAt> {
    match &tab.surface {
        Surface::Pdf(v) => Some(PdfAt {
            page: v.restore_page.unwrap_or_else(|| v.current()),
            zoom: v.zoom,
        }),
        _ if tab.kind == Kind::Pdf => Some(PdfAt { page: 0, zoom: 1. }),
        _ => None,
    }
}

// The images PDF views let go of, freed from the GPU's atlas by the next
// frame (`drop_image` needs a window). A view going away queues its own.
thread_local! {
    static RELEASED: std::cell::RefCell<Vec<Arc<gpui::RenderImage>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn queue_release(images: Vec<Arc<gpui::RenderImage>>) {
    RELEASED.with(|r| r.borrow_mut().extend(images));
}

fn take_released() -> Vec<Arc<gpui::RenderImage>> {
    RELEASED.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

impl Drop for PdfView {
    fn drop(&mut self) {
        queue_release(self.all_images());
    }
}

impl Docxy {
    pub(crate) fn active_is_pdf(&self) -> bool {
        matches!(
            self.tabs.get(self.active).map(|t| &t.surface),
            Some(Surface::Pdf(_))
        )
    }

    pub(crate) fn active_pdf(&self) -> Option<&PdfView> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Pdf(v)) => Some(v),
            _ => None,
        }
    }

    fn active_pdf_mut(&mut self) -> Option<&mut PdfView> {
        match self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            Some(Surface::Pdf(v)) => Some(v),
            _ => None,
        }
    }

    /// Change the active PDF view, then redraw.
    pub(crate) fn with_pdf(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut PdfView)) {
        if let Some(v) = self.active_pdf_mut() {
            f(v);
            cx.notify();
        }
    }

    /// Every frame: free the images views let go of, lay the active view
    /// out from the body's last bounds, take the worker's answers in, send
    /// the next question, finish a Copy that waited for text, and keep a
    /// poll running while an answer is due. On whichever window holds the
    /// tab: a torn-off tab's new window draws and polls it from then on.
    pub(crate) fn pdf_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for img in take_released() {
            cx.drop_image(img, Some(window));
        }
        let scale_factor = window.scale_factor();
        let Some(v) = self.active_pdf_mut() else {
            return;
        };
        let b = v.scroll.bounds();
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        let (w, h) = if w >= 1. && h >= 1. {
            (w, h)
        } else {
            v.viewport.get()
        };
        if let Some(page) = v.restore_page.take() {
            v.viewport.set((w, h));
            v.go_to(page);
        }
        let (_, top) = v.scroll_to.get().unwrap_or_else(|| v.top_left());
        v.frame(w, h, top, scale_factor);
        v.note_selection_needs();
        v.pump();
        for img in std::mem::take(&mut v.released) {
            cx.drop_image(img, Some(window));
        }
        let copy = v.take_pending_copy();
        let due = v.busy() || v.copy_waits();
        if let Some(text) = copy {
            self.pdf_copied(text, cx);
        }
        if due && !self.pdf_polling {
            self.pdf_polling = true;
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(POLL).await;
                    let more = this.update(cx, |this, cx| {
                        let Some(v) = this.active_pdf() else {
                            return false;
                        };
                        if v.has_answers() || !v.busy() {
                            cx.notify();
                        }
                        v.busy() || v.copy_waits()
                    });
                    // Gone, or nothing due: the next frame that sends a
                    // question starts a poll again.
                    if !more.unwrap_or(false) {
                        let _ = this.update(cx, |this, _| this.pdf_polling = false);
                        break;
                    }
                }
            })
            .detach();
        }
    }

    fn pdf_copied(&mut self, text: String, cx: &mut Context<Self>) {
        let n = text.chars().count();
        self.clipboard_write(text.clone(), cx);
        self.office_clip.push(&text);
        self.set_status(format!("Copied {n} characters"));
    }

    /// Ctrl+C: the selection's text, now or once its pages' text is in.
    pub(crate) fn pdf_copy(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.active_pdf_mut() else {
            return;
        };
        if v.sel.is_none() {
            return;
        }
        match v.copy() {
            Some(text) => self.pdf_copied(text, cx),
            None => self.set_status("Copying: reading the selected pages' text…"),
        }
        cx.notify();
    }

    /// A key on the active PDF view: its fields, zoom, navigation, Find,
    /// selection and copy. Nothing edits.
    pub(crate) fn pdf_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let ctrl = m.control || m.platform;
        if crate::field_edit::is_save_chord(ctrl, m.alt, key) {
            return self.save_active(window, cx);
        }
        // Ctrl+F (⌘F) opens Find, its query selected.
        if ctrl && !m.alt && key == "f" {
            return self.with_pdf(cx, |v| {
                v.find.open = true;
                v.page_box.focused = false;
                v.find.field.focus_all();
            });
        }
        // F3 and Shift+F3: the next and previous hit, from anywhere.
        if key == "f3" {
            return self.with_pdf(cx, |v| v.find_step(!m.shift));
        }
        let Some(v) = self.active_pdf() else { return };
        if v.find.field.focused {
            return self.pdf_find_key(ev, cx);
        }
        if v.page_box.focused {
            return self.pdf_page_box_key(ev, cx);
        }
        if ctrl && !m.alt {
            match key {
                "=" | "+" => return self.with_pdf(cx, |v| v.zoom_in(true)),
                "-" => return self.with_pdf(cx, |v| v.zoom_in(false)),
                "0" => return self.with_pdf(cx, PdfView::fit_width),
                "a" => return self.with_pdf(cx, PdfView::select_all),
                "c" => return self.pdf_copy(cx),
                "home" => return self.with_pdf(cx, |v| v.go_to(0)),
                "end" => {
                    return self.with_pdf(cx, |v| {
                        let last = v.page_count().saturating_sub(1);
                        v.go_to(last)
                    });
                }
                _ => {}
            }
        }
        if key == "escape" {
            return self.with_pdf(cx, |v| {
                if v.find.open {
                    v.find.open = false;
                } else {
                    v.sel = None;
                }
            });
        }
        if !m.alt {
            self.with_pdf(cx, |v| {
                v.nav_key(key, ctrl);
            });
        }
    }

    /// A key in the Find box: the shared one-line keys (#2116), then Enter
    /// and Shift+Enter for the next and previous hit, Escape to close.
    fn pdf_find_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        use crate::field_edit::FieldKey;
        let shift = ev.keystroke.modifiers.shift;
        match field_key_of(ev) {
            FieldKey::Enter => self.with_pdf(cx, |v| v.find_step(!shift)),
            FieldKey::Escape => self.with_pdf(cx, |v| {
                v.find.field.focused = false;
                v.find.open = false;
            }),
            FieldKey::Edit(op) => self.with_pdf(cx, |v| {
                let before = v.find.field.text.clone();
                v.find.field.edit(op);
                if v.find.field.text != before {
                    v.find_changed();
                }
            }),
            k @ (FieldKey::Copy | FieldKey::Cut) => {
                let Some(v) = self.active_pdf() else { return };
                let (text, sel) = (v.find.field.text.clone(), v.find.field.sel);
                if sel && !text.is_empty() {
                    self.field_copy(text, cx);
                    if k == FieldKey::Cut {
                        self.with_pdf(cx, |v| {
                            v.find.field.edit(crate::field_edit::Op::Delete);
                            v.find_changed();
                        });
                    }
                }
            }
            FieldKey::Paste => {
                if let Some(line) = self.field_paste(true, cx) {
                    self.with_pdf(cx, |v| {
                        v.find.field.edit(crate::field_edit::Op::Insert(&line));
                        v.find_changed();
                    });
                }
            }
            FieldKey::Ignore => {}
        }
    }

    /// A key in the page box: the shared one-line keys; Enter goes to the
    /// page typed, Escape gives the keys back to the pages.
    fn pdf_page_box_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        use crate::field_edit::FieldKey;
        match field_key_of(ev) {
            FieldKey::Enter => self.with_pdf(cx, |v| {
                match super::parse_page_box(&v.page_box.text, v.page_count()) {
                    Some(p) => {
                        v.go_to(p);
                        v.page_box = Field::default();
                    }
                    None => v.page_box.focus_all(),
                }
            }),
            FieldKey::Escape => self.with_pdf(cx, |v| v.page_box = Field::default()),
            FieldKey::Edit(op) => self.with_pdf(cx, |v| v.page_box.edit(op)),
            k @ (FieldKey::Copy | FieldKey::Cut) => {
                let Some(v) = self.active_pdf() else { return };
                if v.page_box.sel && !v.page_box.text.is_empty() {
                    let text = v.page_box.text.clone();
                    self.field_copy(text, cx);
                    if k == FieldKey::Cut {
                        self.with_pdf(cx, |v| v.page_box.edit(crate::field_edit::Op::Delete));
                    }
                }
            }
            FieldKey::Paste => {
                if let Some(line) = self.field_paste(false, cx) {
                    self.with_pdf(cx, |v| {
                        v.page_box.edit(crate::field_edit::Op::Insert(&line))
                    });
                }
            }
            FieldKey::Ignore => {}
        }
    }

    /// Tab (Shift+Tab) on a PDF view: between the Find box (when open) and
    /// the page box, then back to the pages.
    pub(crate) fn pdf_tab(&mut self, back: bool, cx: &mut Context<Self>) {
        self.with_pdf(cx, |v| {
            let order = [v.find.open, true];
            let at = if v.find.field.focused {
                Some(0)
            } else if v.page_box.focused {
                Some(1)
            } else {
                None
            };
            let next = match (at, back) {
                (None, false) => order.iter().position(|o| *o),
                (None, true) => Some(1),
                (Some(i), false) => (i + 1..2).find(|j| order[*j]),
                (Some(i), true) => (0..i).rev().find(|j| order[*j]),
            };
            v.find.field.focused = false;
            v.page_box = Field::default();
            match next {
                Some(0) => v.find.field.focus_all(),
                Some(1) => {
                    v.page_box.text = v.page_box_text();
                    v.page_box.focus_all();
                }
                _ => {}
            }
        });
    }

    /// A press on the Find box's field.
    pub(crate) fn pdf_find_press(&mut self, cx: &mut Context<Self>) {
        self.with_pdf(cx, |v| {
            v.page_box = Field::default();
            v.find.field.focused = true;
            v.find.field.sel = false;
            v.find.field.caret = v.find.field.text.chars().count();
        });
    }

    /// A press on the page box: it takes the keys, its "n / N" selected.
    pub(crate) fn pdf_page_box_press(&mut self, cx: &mut Context<Self>) {
        self.with_pdf(cx, |v| {
            v.find.field.focused = false;
            v.page_box.text = v.page_box_text();
            v.page_box.focus_all();
        });
    }

    /// A press on the pages: the fields give the keys back, and the press
    /// starts a selection at content `(x, y)`.
    pub(crate) fn pdf_page_press(
        &mut self,
        x: f32,
        y: f32,
        clicks: usize,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        self.with_pdf(cx, |v| {
            v.find.field.focused = false;
            v.page_box = Field::default();
            v.press(x, y, clicks, shift);
        });
    }

    /// Convert to editable document: today's conversion of the PDF
    /// (#633), as a new document tab after the view. A view from a
    /// protected zone gives a Protected View tab, as opening the file would.
    pub(crate) fn pdf_convert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let (Some(path), true) = (tab.path.clone(), matches!(tab.surface, Surface::Pdf(_))) else {
            return;
        };
        match converted_pdf_tab(&path, &trusted::TrustStore::load(&config_root())) {
            Ok(new) => {
                let i = self.active + 1;
                self.tabs.insert(i, new);
                self.select_tab(i, window, cx);
                self.persist(cx);
            }
            Err(e) => self.set_status(e),
        }
        cx.notify();
    }

    /// The outline pane shown or hidden.
    pub(crate) fn pdf_toggle_outline(&mut self, cx: &mut Context<Self>) {
        self.with_pdf(cx, |v| v.outline_shown = !v.outline_shown);
    }
}

/// The document Convert to editable document opens: the PDF converted as
/// an open converted it before the PDF view (`Converted::Pdf`).
pub(crate) fn converted_pdf_tab(
    path: &Path,
    trusted: &trusted::TrustStore,
) -> Result<DocTab, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read error: {e}"))?;
    let loaded = convert_doc(path, &bytes, convert_child::What::Pdf)
        .unwrap_or_else(|| Loaded::empty("load error: the PDF has no text to convert"));
    let mut tab = loaded.into_tab(
        Kind::Docx,
        file_name(path).into(),
        Some(path.to_path_buf()),
        false,
    );
    tab.access.protected = open_mode::is_protected_zone(open_mode::zone_id(path))
        && !trusted.is_trusted(path, trusted::Stamp::of(path));
    tab.access.stamp = tab
        .access
        .protected
        .then(|| trusted::Stamp::of(path))
        .flatten();
    Ok(tab)
}
