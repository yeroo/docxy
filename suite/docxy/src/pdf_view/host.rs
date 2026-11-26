//! The window's side of the PDF view (#2245): which files open as one and
//! how, its session row, Convert to editable document, its keys and the
//! presses its drawing routes here, Save and Save As of its form values
//! (#2255), and the frame tick that takes the worker's answers in.

use super::worker::Spawn;
use super::{Field, PdfView, PendingSave};
use crate::*;
use std::path::Path;
use std::sync::Arc;

/// The status line after Ctrl+S on a PDF view with nothing filled in
/// since it was read or saved (#2255): the file is not written.
pub(crate) const PDF_NO_CHANGES: &str =
    "No changes to save: only filled-in form values are saved to a PDF";
/// The status line after Save on a PDF tab that is not open (failed, or
/// not opened yet).
pub(crate) const PDF_NOT_OPEN: &str = "This PDF is not open, so there is nothing to save";
/// The status line while a save waits for an earlier one.
pub(crate) const PDF_SAVING: &str = "Saving the PDF…";
/// Save As on a PDF tab in a harness, which never opens the native dialog.
pub(crate) const PDF_SAVE_AS_HARNESS: &str =
    "A harness instance cannot open the Save As dialog; use the harness save-as verb";
/// The question before a signed PDF is saved (#2255).
pub(crate) const SIGNED_QUESTION: &str =
    "This PDF is signed. Saving changes invalidates the signature.";
/// The question when the save cannot append to the file (a damaged file is
/// written anew): the signed revision is not kept at all.
pub(crate) const SIGNED_LOST_QUESTION: &str = "This PDF is signed. Saving changes invalidates the \
     signature, and as this file has to be written anew the signature will be lost.";
/// The status line of an encrypted PDF waiting for its password (#2256).
pub(crate) const PDF_LOCKED: &str = "password required: enter the PDF's password to open it";
/// What the prompt says after a password that does not open the file.
pub(crate) const PDF_WRONG_PASSWORD: &str = "The password is incorrect. Try again.";
/// What a failed PDF tab's body says above its reason (#2256).
pub(crate) const PDF_CANT_OPEN: &str = "This PDF can't be opened";
/// How often the frame looks for the worker's answers while one is due.
const POLL: std::time::Duration = std::time::Duration::from_millis(15);

/// The status bar's hint on a PDF view: the Mac's chords on macOS.
pub(crate) fn status_hint(macos: bool) -> &'static str {
    if macos {
        "⌘F find · ⌘= / ⌘- zoom · ⌘0 fit width · Page Up/Down · ⌘A ⌘C copy · Tab next field · ⌘S save"
    } else {
        "Ctrl+F find · Ctrl+= / Ctrl+- zoom · Ctrl+0 fit width · PgUp/PgDn · Ctrl+A Ctrl+C copy · Tab next field · Ctrl+S save"
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
/// `None` when `path` is no PDF, or cannot be read (the caller opens it as a
/// document, which says why). A PDF whose pages the worker cannot read is a
/// failed tab that says why.
pub(crate) fn pdf_view_tab(path: &Path, at: Option<PdfAt>) -> Option<DocTab> {
    pdf_view_tab_with(path, at, Spawn::default_for_process())
}

/// [`pdf_view_tab`] with its worker started by `spawn` (the tests' failing
/// one).
pub(crate) fn pdf_view_tab_with(path: &Path, at: Option<PdfAt>, spawn: Spawn) -> Option<DocTab> {
    if !is_pdf_file(path) {
        return None;
    }
    let mut tab = blank_docx_tab(&file_name(path));
    tab.kind = Kind::Pdf;
    tab.path = Some(path.to_path_buf());
    match PdfView::open(path, spawn) {
        Ok(mut v) => {
            tab.status = if v.lock.is_some() {
                PDF_LOCKED.into()
            } else {
                format!("{}{}", loaded_status(v.page_count()), v.restricted_note()).into()
            };
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

/// Try the password typed into `tab`'s prompt (#2256): the pages; the
/// prompt again; or, when the file could not be read after all, a failed
/// tab that keeps where the view was (pending, `load_failed`: its next
/// activation tries again, as a restored tab's does) and says why.
pub(crate) fn unlock_tab(tab: &mut DocTab) {
    let Surface::Pdf(v) = &mut tab.surface else {
        return;
    };
    if v.lock.is_none() {
        return;
    }
    let at = PdfAt {
        page: v.restore_page.unwrap_or(0),
        zoom: v.zoom,
    };
    match v.try_unlock() {
        super::Unlocked::Opened => {
            tab.status = format!("{}{}", loaded_status(v.page_count()), v.restricted_note()).into();
        }
        super::Unlocked::Wrong => tab.status = PDF_LOCKED.into(),
        super::Unlocked::Failed(why) => {
            tab.status = format!("load error: {why}").into();
            tab.load_failed = true;
            tab.surface = Surface::PdfPending(at);
        }
    }
}

/// A Save or Save As that writes nothing says why on the status line, but
/// on a PDF tab that could not be opened (#2256): its status is the reason
/// its body shows, which a refusal must not replace.
pub(crate) fn note_save_refusal(tab: &mut DocTab, why: &'static str) {
    if !(tab.kind == Kind::Pdf && tab.load_failed) {
        tab.status = why.into();
    }
}

/// Why a failed PDF tab could not open it, for its body (#2256): its
/// status line's reason, without the `load error: ` it starts with.
pub(crate) fn pdf_failed_reason(tab: &DocTab) -> Option<String> {
    if tab.kind != Kind::Pdf || !tab.load_failed {
        return None;
    }
    let status = tab.status.to_string();
    let why = ["load error: ", "read error: "]
        .iter()
        .find_map(|p| status.strip_prefix(p))
        .unwrap_or(&status);
    Some(why.to_string())
}

/// A PDF view's status once open: `loaded (PDF, N pages)`.
pub(crate) fn loaded_status(pages: usize) -> String {
    let s = if pages == 1 { "" } else { "s" };
    format!("loaded (PDF, {pages} page{s})")
}

/// What Save does on a PDF tab (#2255).
#[derive(Debug, PartialEq)]
pub(crate) enum PdfSaveStep {
    /// Nothing is written, and the status line says why.
    Nothing(&'static str),
    /// A signed PDF: this question first, then a save to `path`.
    Ask {
        path: std::path::PathBuf,
        question: &'static str,
    },
    /// Write to `path`.
    Go(std::path::PathBuf),
}

/// What Save (`target` `None`) or Save As (`Some`) does on `tab`: the text
/// being typed into a field goes in first; then nothing is written for a
/// tab that is not open, a save already on its way, or (Save) nothing filled
/// in since the file was read or saved; a signed PDF asks first.
pub(crate) fn pdf_save_step(tab: &mut DocTab, target: Option<std::path::PathBuf>) -> PdfSaveStep {
    let path = target.clone().or_else(|| tab.path.clone());
    let Surface::Pdf(v) = &mut tab.surface else {
        return PdfSaveStep::Nothing(PDF_NOT_OPEN);
    };
    // Waiting for its password (#2256): nothing is open to save.
    if v.lock.is_some() {
        return PdfSaveStep::Nothing(PDF_NOT_OPEN);
    }
    let Some(path) = path else {
        return PdfSaveStep::Nothing(PDF_NOT_OPEN);
    };
    v.commit_focus();
    if v.pending_save.is_some() || v.ask_signed.is_some() {
        return PdfSaveStep::Nothing(PDF_SAVING);
    }
    if target.is_none() && !v.unsaved() && !v.fills_waiting() {
        return PdfSaveStep::Nothing(PDF_NO_CHANGES);
    }
    if v.form.signed {
        let question = if v.form.incremental {
            SIGNED_QUESTION
        } else {
            SIGNED_LOST_QUESTION
        };
        return PdfSaveStep::Ask { path, question };
    }
    PdfSaveStep::Go(path)
}

/// What Save As on a PDF tab writes to, from the path its dialog gave
/// (#2255): a name without an extension gets `.pdf` (the dialog may not add
/// the filter's), a `.pdf` one is kept; any other extension is refused, as
/// a workbook's Save As refuses one, since the dialog confirmed replacing
/// that name and no other.
pub(crate) fn pdf_save_as_target(path: std::path::PathBuf) -> Result<std::path::PathBuf, String> {
    match path.extension() {
        None => {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(".pdf");
            Ok(path.with_file_name(name))
        }
        Some(e) if e.eq_ignore_ascii_case("pdf") => Ok(path),
        Some(e) => Err(format!(
            "A PDF is saved as a .pdf file, not .{}: choose a name that ends in .pdf",
            e.to_string_lossy()
        )),
    }
}

/// The signed PDF's question (#2255): Save goes on, Cancel writes nothing.
pub(crate) fn signed_dialog(question: &str) -> crate::dialog::Dialog {
    crate::dialog::Dialog::message(
        crate::dialog::catalog::PDF_SIGNED_SAVE,
        "docxy",
        question.into(),
        &[
            ("Save", crate::dialog::ButtonRole::Accept),
            ("Cancel", crate::dialog::ButtonRole::Cancel),
        ],
        crate::dialog::DialogOwner::PdfSignedSave,
    )
}

/// A PDF tab's save is over (#2255): written, its path and title follow a
/// Save As, and it is unsaved only for values filled in since; not written,
/// it keeps its values and says why.
pub(crate) fn finish_pdf_save(tab: &mut DocTab, outcome: Result<std::path::PathBuf, String>) {
    match outcome {
        Ok(path) => {
            if tab.path.as_deref() != Some(path.as_path()) {
                tab.title = file_name(&path).into();
                tab.path = Some(path.clone());
            }
            tab.dirty = matches!(&tab.surface, Surface::Pdf(v) if v.has_unsaved());
            tab.status = format!("saved {}", path.display()).into();
        }
        Err(why) => tab.status = format!("save error: {why}").into(),
    }
}

/// The first PDF tab with filled-in values not saved: hot exit cannot keep
/// them (#2255).
pub(crate) fn unsaved_pdf(tabs: &[DocTab]) -> Option<usize> {
    tabs.iter().position(|t| {
        t.kind == Kind::Pdf && (t.dirty || matches!(&t.surface, Surface::Pdf(v) if v.has_unsaved()))
    })
}

/// A PDF tab before a close or the window's close asks about it (#2255):
/// the text being typed into a field goes in, and the tab is unsaved
/// exactly when it has values the file does not, without waiting for the
/// next frame.
pub(crate) fn commit_pdf_tab(tab: &mut DocTab) {
    if let Surface::Pdf(v) = &mut tab.surface {
        v.commit_focus();
        tab.dirty = v.has_unsaved();
    }
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
        // Not opened since the session restored it: where it was then.
        Surface::PdfPending(at) => Some(*at),
        _ if tab.kind == Kind::Pdf => Some(PdfAt { page: 0, zoom: 1. }),
        _ => None,
    }
}

/// A PDF view's tab restored from the session, not opened yet: no worker
/// starts until it is first in front ([`finish_pending_pdf`]).
pub(crate) fn pending_pdf_tab(path: &Path, title: &str, at: PdfAt) -> DocTab {
    let mut tab = blank_docx_tab(title);
    tab.kind = Kind::Pdf;
    tab.path = Some(path.to_path_buf());
    tab.status = PENDING.into();
    tab.surface = Surface::PdfPending(at);
    tab
}

/// What a restored PDF tab says until it opens.
pub(crate) const PENDING: &str = "not opened yet: it opens when you open its tab";

/// Open a restored PDF tab now that it is in front, where it was.
///
/// A file that cannot be read (gone, a drive not mounted) keeps the tab
/// pending with `load_failed` and a `read error`: the session keeps where
/// it was, and the next time the tab comes in front it tries again (a
/// failed pending tab is not retried every frame). A file that reads but is
/// no PDF any more opens as a document would.
pub(crate) fn finish_pending_pdf(tab: &mut DocTab, at: PdfAt) {
    finish_pending_pdf_with(tab, at, Spawn::default_for_process());
}

/// [`finish_pending_pdf`] with its worker started by `spawn`. A worker that
/// cannot read the file keeps the tab pending where it was, as an
/// unreadable file does: the session keeps its place, and the next
/// activation tries again.
pub(crate) fn finish_pending_pdf_with(tab: &mut DocTab, at: PdfAt, spawn: Spawn) {
    let Some(path) = tab.path.clone() else {
        return;
    };
    if let Err(e) = std::fs::File::open(&path) {
        tab.surface = Surface::PdfPending(at);
        tab.load_failed = true;
        tab.status = format!("read error: {e}").into();
        return;
    }
    let title = tab.title.clone();
    let fresh = match pdf_view_tab_with(&path, Some(at), spawn) {
        Some(fresh) if matches!(fresh.surface, Surface::Placeholder) => {
            tab.surface = Surface::PdfPending(at);
            tab.load_failed = true;
            tab.status = fresh.status;
            return;
        }
        Some(fresh) => fresh,
        None => {
            load_doc_for_tab(&path, None).into_tab(Kind::Docx, title.clone(), Some(path), false)
        }
    };
    tab.replace_in_place(fresh);
    tab.title = title;
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

    /// A PDF tab, whatever its state: open, failed or not opened yet.
    pub(crate) fn active_kind_is_pdf(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.kind == Kind::Pdf)
    }

    pub(crate) fn active_pdf(&self) -> Option<&PdfView> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Pdf(v)) => Some(v),
            _ => None,
        }
    }

    pub(crate) fn active_pdf_mut(&mut self) -> Option<&mut PdfView> {
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
        // A restored PDF tab in front opens before this frame lays it out
        // (one whose file could not be read waits for its next activation).
        if let Some(t) = self.tabs.get_mut(self.active)
            && matches!(t.surface, Surface::PdfPending(_))
            && !t.load_failed
        {
            finish_pending_conversion(t);
        }
        self.pdf_pump_behind();
        let scale_factor = window.scale_factor();
        // A view waiting for its password has nothing to lay out or ask
        // (#2256): where the session left it waits for its pages.
        let Some(v) = self.active_pdf_mut().filter(|v| v.lock.is_none()) else {
            return self.pdf_settle_tabs(window, cx);
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
        let notice = v.notice.take();
        let url = v.open_url.take();
        if let Some(text) = copy {
            self.pdf_copied(text, cx);
        }
        if let Some(notice) = notice {
            self.set_status(notice);
        }
        if let Some(url) = url {
            cx.open_url(&url);
        }
        // After the active view took its answers in: a save it finished is
        // over in this frame.
        self.pdf_settle_tabs(window, cx);
        let due = self.pdf_due();
        if due && !self.pdf_polling {
            self.pdf_polling = true;
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(POLL).await;
                    let more = this.update(cx, |this, cx| {
                        let answers = this.tabs.iter().any(|t| match &t.surface {
                            Surface::Pdf(v) => v.has_answers(),
                            _ => false,
                        });
                        let due = this.pdf_due();
                        if answers || !due {
                            cx.notify();
                        }
                        due
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

    /// Whether a PDF view waits on its worker: the active one's question or
    /// Copy, or any tab's save (#2255).
    pub(crate) fn pdf_due(&self) -> bool {
        let active = self
            .active_pdf()
            .is_some_and(|v| v.busy() || v.copy_waits());
        active
            || self.tabs.iter().any(|t| match &t.surface {
                Surface::Pdf(v) => v.pending_save.is_some(),
                _ => false,
            })
    }

    /// Every PDF tab behind another, each frame (#2255): it takes its
    /// answers in and goes on with its form operations (a save).
    fn pdf_pump_behind(&mut self) {
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if let Surface::Pdf(v) = &mut t.surface
                && i != self.active
            {
                v.pump_form();
            }
        }
    }

    /// Every PDF tab, each frame (#2255): its unsaved mark follows its
    /// values; a save that is over updates the tab and runs its `after`,
    /// last; values a new reader could not take say so.
    fn pdf_settle_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut over: Vec<(usize, PendingSave)> = Vec::new();
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let Surface::Pdf(v) = &mut t.surface else {
                continue;
            };
            t.dirty = v.has_unsaved();
            if std::mem::take(&mut v.values_lost) {
                t.status = super::worker::VALUES_LOST.into();
            }
            if v.pending_save.as_ref().is_some_and(|s| s.outcome.is_some())
                && let Some(done) = v.pending_save.take()
            {
                over.push((i, done));
            }
        }
        for (i, done) in over {
            let Some(outcome) = done.outcome else {
                continue;
            };
            let prev = self.tabs[i].path.clone();
            let written = outcome.is_ok();
            finish_pdf_save(&mut self.tabs[i], outcome);
            if written {
                if i == self.active {
                    self.note_recent_if_moved(prev);
                }
                self.persist(cx);
            }
            // Last, written or not, as a document's save runs it (#2194);
            // after this frame's drawing, as it may close the tab or end a
            // quit.
            if let Some(after) = done.after {
                cx.defer_in(window, move |this, window, cx| {
                    after(this, window, cx);
                    cx.notify();
                });
            }
        }
    }

    /// Save the active PDF tab (#2255): to its own file, or to `target`
    /// (Save As). Nothing is written when nothing was filled in since the
    /// file was read or saved (but Save As writes the copy); a signed PDF
    /// asks first. The worker writes the file, so the save is over when its
    /// answer comes: `after` runs then (`None` here), or now when nothing
    /// is written (`Some`).
    #[must_use = "run the returned AfterSave: the save is over"]
    pub(crate) fn save_pdf_then(
        &mut self,
        target: Option<std::path::PathBuf>,
        after: AfterSave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AfterSave> {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return Some(after);
        };
        match pdf_save_step(tab, target) {
            PdfSaveStep::Nothing(why) => {
                note_save_refusal(tab, why);
                self.refocus(window, cx);
                Some(after)
            }
            PdfSaveStep::Ask { path, question } => {
                if let Surface::Pdf(v) = &mut tab.surface {
                    v.ask_signed = Some((path, after));
                }
                tab.dialogs.push(signed_dialog(question));
                cx.notify();
                None
            }
            PdfSaveStep::Go(path) => self.pdf_save_go(path, after, cx),
        }
    }

    /// Hand the save to the worker: written after the values queued before
    /// it, its `after` run once the answer comes.
    fn pdf_save_go(
        &mut self,
        path: std::path::PathBuf,
        after: AfterSave,
        cx: &mut Context<Self>,
    ) -> Option<AfterSave> {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return Some(after);
        };
        let Surface::Pdf(v) = &mut tab.surface else {
            return Some(after);
        };
        v.form_ops.push_back(super::worker::Op::SaveTo { path });
        v.pending_save = Some(PendingSave {
            after: Some(after),
            outcome: None,
        });
        tab.status = PDF_SAVING.into();
        cx.notify();
        None
    }

    /// A press on the signed PDF's question (#2255): Save goes on with the
    /// save, Cancel writes nothing; either way the save's `after` runs once
    /// it is over. `None` for any other dialog.
    pub(crate) fn pdf_signed_save_click(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        let top = tab.dialogs.top()?;
        if top.owner != crate::dialog::DialogOwner::PdfSignedSave {
            return None;
        }
        let role = top
            .buttons
            .iter()
            .find(|b| b.label.eq_ignore_ascii_case(button))
            .map(|b| b.role);
        let Some(role) = role else {
            return Some(Err(format!("the dialog has no '{button}' button")));
        };
        tab.dialogs.pop();
        let asked = match &mut tab.surface {
            Surface::Pdf(v) => v.ask_signed.take(),
            _ => None,
        };
        let Some((path, after)) = asked else {
            return Some(Ok(()));
        };
        let after = if role == crate::dialog::ButtonRole::Accept {
            self.pdf_save_go(path, after, cx)
        } else {
            self.set_status("Not saved: the signature is kept");
            Some(after)
        };
        self.refocus(window, cx);
        self.run_after(after, window, cx);
        Some(Ok(()))
    }

    /// Save As on a PDF tab (#2255): the native dialog with the PDF type
    /// only, then the save to the file picked.
    pub(crate) fn pdf_save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if !matches!(tab.surface, Surface::Pdf(_)) {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                note_save_refusal(tab, PDF_NOT_OPEN);
            }
            return self.refocus(window, cx);
        }
        if self.harness {
            self.set_status(PDF_SAVE_AS_HARNESS);
            return self.refocus(window, cx);
        }
        let id = self.active_tab_id();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(tab.title.to_string());
        if let Some(dir) = tab.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        self.native_dialog(
            dialog,
            |d| d.save_file(),
            move |this, target, window, cx| {
                // Not the tab that asked (the window is disabled while the
                // dialog is up, so this is defence): nothing to save.
                let Some(target) = target.filter(|_| this.active_tab_is(&id)) else {
                    return this.refocus(window, cx);
                };
                match pdf_save_as_target(target) {
                    Ok(target) => {
                        let after = this.save_pdf_then(Some(target), crate::no_after(), window, cx);
                        this.run_after(after, window, cx);
                    }
                    Err(why) => {
                        this.set_status(why);
                        this.refocus(window, cx);
                    }
                }
            },
            window,
            cx,
        );
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
    /// selection and copy. Only form fields edit (#2255).
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
        // The password prompt has every other key (#2256).
        if self.active_pdf().is_some_and(|v| v.lock.is_some()) {
            return self.pdf_password_key(ev, cx);
        }
        // Ctrl+F (⌘F) opens Find, its query selected.
        if ctrl && !m.alt && key == "f" {
            return self.with_pdf(cx, |v| {
                v.blur();
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
        // A form field with the keys takes them first (#2255).
        if v.focus.is_some() && self.pdf_field_key(ev, cx) {
            return;
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
                if v.popup.is_some() {
                    v.popup = None;
                } else if v.find.open {
                    v.find.open = false;
                } else {
                    v.sel = None;
                }
            });
        }
        if !m.alt
            && let Some(v) = self.active_pdf_mut()
            && v.nav_key(key, ctrl)
        {
            cx.notify();
        }
    }

    /// A key for the focused form field (#2255): its own keys (Space, the
    /// lists' Up/Down, F4), then a text field's one-line keys (#2116), Enter
    /// (a newline in a multi-line field) and Esc. `false` for a key the
    /// field does not take, which the view then has.
    fn pdf_field_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        use crate::field_edit::FieldKey;
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let ctrl = m.control || m.platform;
        let taken = self
            .active_pdf_mut()
            .is_some_and(|v| v.form_key(key, m.alt));
        if taken {
            cx.notify();
            return true;
        }
        let Some(v) = self.active_pdf() else {
            return false;
        };
        let editing = v.focus.as_ref().is_some_and(|f| f.edit.is_some());
        if !editing {
            return match key {
                "enter" => {
                    self.with_pdf(cx, |v| v.form_enter(ctrl));
                    true
                }
                "escape" => {
                    self.with_pdf(cx, PdfView::cancel_focus);
                    true
                }
                _ => false,
            };
        }
        match field_key_of(ev) {
            FieldKey::Enter => self.with_pdf(cx, |v| v.form_enter(ctrl)),
            FieldKey::Escape => self.with_pdf(cx, PdfView::cancel_focus),
            FieldKey::Edit(op) => self.with_pdf(cx, |v| v.edit_text(op)),
            k @ (FieldKey::Copy | FieldKey::Cut) => {
                let Some(e) = self
                    .active_pdf()
                    .and_then(|v| v.focus.as_ref())
                    .and_then(|f| f.edit.clone())
                else {
                    return true;
                };
                let password = self
                    .active_pdf()
                    .and_then(|v| v.focus.as_ref().and_then(|f| v.field(f.field)))
                    .is_some_and(|f| f.password);
                // A password is never copied out.
                if e.sel && !e.text.is_empty() && !password {
                    self.field_copy(e.text.clone(), cx);
                    if k == FieldKey::Cut {
                        self.with_pdf(cx, |v| v.edit_text(crate::field_edit::Op::Delete));
                    }
                }
            }
            FieldKey::Paste => {
                let multiline = self
                    .active_pdf()
                    .and_then(|v| v.focus.as_ref().and_then(|f| v.field(f.field)))
                    .is_some_and(|f| f.multiline);
                // A multi-line field keeps the pasted line breaks; a
                // one-line field takes the text as one line, as every other.
                let text = if multiline {
                    match self.clipboard_read(cx) {
                        ClipRead::Text(t) => Some(crate::field_edit::multi_line(&t)),
                        _ => None,
                    }
                } else {
                    self.field_paste(false, cx)
                };
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    self.with_pdf(cx, |v| v.edit_text(crate::field_edit::Op::Insert(&text)));
                }
            }
            FieldKey::Ignore => return false,
        }
        true
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

    /// A key in the password prompt (#2256): the shared one-line keys; Enter
    /// tries the password. Nothing typed is ever copied or cut out of it.
    fn pdf_password_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        use crate::field_edit::FieldKey;
        let with_lock = |this: &mut Self, cx: &mut Context<Self>, f: &dyn Fn(&mut Field)| {
            if let Some(lock) = this.active_pdf_mut().and_then(|v| v.lock.as_mut()) {
                lock.field.focused = true;
                f(&mut lock.field);
                cx.notify();
            }
        };
        match field_key_of(ev) {
            FieldKey::Enter => self.pdf_unlock(cx),
            FieldKey::Edit(op) => with_lock(self, cx, &|f| f.edit(op)),
            FieldKey::Paste => {
                if let Some(mut line) = self.field_paste(false, cx) {
                    with_lock(self, cx, &|f| f.edit(crate::field_edit::Op::Insert(&line)));
                    zeroize::Zeroize::zeroize(&mut line);
                }
            }
            FieldKey::Escape | FieldKey::Copy | FieldKey::Cut | FieldKey::Ignore => {}
        }
    }

    /// Try the password typed into the active tab's prompt (#2256): the
    /// pages, where the session left them; the prompt again, saying the
    /// password is wrong; or a failed tab that says why.
    pub(crate) fn pdf_unlock(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            unlock_tab(tab);
            cx.notify();
        }
    }

    /// A press on the password prompt's field (#2256).
    pub(crate) fn pdf_password_press(&mut self, cx: &mut Context<Self>) {
        if let Some(lock) = self.active_pdf_mut().and_then(|v| v.lock.as_mut()) {
            lock.field.focused = true;
            cx.notify();
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
            // The password prompt's one field keeps the keys (#2256).
            if let Some(lock) = v.lock.as_mut() {
                lock.field.focused = true;
                return;
            }
            // In a form, Tab goes from field to field in the document's tab
            // order (#2255); from the Find box or page box it goes on there.
            let boxes = v.find.field.focused || v.page_box.focused;
            if (v.focus.is_some() || !boxes) && v.tab_field(back) {
                return;
            }
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
            v.blur();
            v.page_box = Field::default();
            v.find.field.focused = true;
            v.find.field.sel = false;
            v.find.field.caret = v.find.field.text.chars().count();
        });
    }

    /// A press on the page box: it takes the keys, its "n / N" selected.
    pub(crate) fn pdf_page_box_press(&mut self, cx: &mut Context<Self>) {
        self.with_pdf(cx, |v| {
            v.blur();
            v.find.field.focused = false;
            v.page_box.text = v.page_box_text();
            v.page_box.focus_all();
        });
    }

    /// A press on the pages: the boxes give the keys back, and the press
    /// goes to a form field, a comment, a link or the text at content
    /// `(x, y)` (#2255).
    pub(crate) fn pdf_page_press(
        &mut self,
        x: f32,
        y: f32,
        clicks: usize,
        shift: bool,
        ctrl: bool,
        cx: &mut Context<Self>,
    ) {
        self.with_pdf(cx, |v| {
            v.find.field.focused = false;
            v.page_box = Field::default();
            v.press_at(x, y, clicks, shift, ctrl);
        });
    }

    /// Convert to editable document: the PDF converted with its layout
    /// (#2257, `pdf_view/convert.rs`; the text importer of #633 when that
    /// fails), as a new document tab after the view. A view from a
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
