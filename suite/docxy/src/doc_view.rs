//! A document view an extension tab can embed (#1873): a
//! [`docxcore::model::Document`] drawn by the suite's own document renderer
//! in its flow (web layout) style, wrapped to the view's width, with inline
//! pictures from the package it came with. It selects (press, drag,
//! Shift+press, Ctrl+A) and copies (Ctrl+C: text and RTF, as a document copy
//! does), scrolls (wheel, Up/Down, Page Up/Down, Home/End) and zooms
//! (Ctrl+wheel, Ctrl+=, Ctrl+-, Ctrl+0). Read-only by default: it then never
//! edits and draws no caret. Its editing mode ([`DocView::set_editable`],
//! #1947) types, deletes, moves the caret, undoes and takes a host ribbon's
//! formatting commands ([`DocView::format`]) and its Insert, Copy and Cut
//! ([`DocView::insert_text`], [`DocView::copy`], [`DocView::cut`], #1977);
//! its decisions are [`edit`]'s. A hyperlink press goes to the extension's callback
//! ([`DocView::on_link`]), never opened here. The chrome (scroll bar, selection) follows the suite's
//! theme; the document is a white page in its authored colours unless the
//! extension turns on dark mode for content ([`DocView::set_content_dark`]),
//! whose dark page the host may paint in its own colour
//! ([`DocView::set_page_background`], #2003). The padding around the
//! document is the host's to set too ([`DocView::set_page_inset`]).
//!
//! The renderer is the suite's (`block_el`): only its click target differs
//! ([`crate::DocTarget::Embed`]), so a document reads here exactly as in a
//! document tab's Web Layout.

mod edit;

pub use edit::{CaretFormat, DocFormat};

use super::*;
use docxcore::editor::{Caret, Clip, Editor};
use docxcore::package::Package;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The painted pieces of each paragraph the view drew last, by path: what
/// its harness regions and link spans are read from.
pub(crate) type Words = Rc<RefCell<Vec<(Vec<usize>, WordSpans)>>>;

/// The story part the view's pictures resolve through.
const BODY_PART: &str = "word/document.xml";

/// The zoom range and step, as the document tabs' (Ctrl+= / Ctrl+-).
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 3.0;

/// A line's scroll by the arrow keys, at zoom 1.
const LINE_PX: f32 = 24.;

/// The padding around the document inside the scroll area when the host
/// sets none (#2003): 48 px beside it, 28 px above and below.
pub(crate) const DEFAULT_PAGE_INSET: Edges<Pixels> = Edges {
    top: px(28.),
    right: px(48.),
    bottom: px(28.),
    left: px(48.),
};

static HARNESS: AtomicBool = AtomicBool::new(false);

/// Whether this process is a harness instance: a view's copy then goes to
/// the run's private clipboard, as every copy in a harness does (#699).
pub(crate) fn set_harness(on: bool) {
    HARNESS.store(on, Ordering::Relaxed);
}

pub(crate) fn harness() -> bool {
    HARNESS.load(Ordering::Relaxed)
}

/// Which press follows a hyperlink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkClick {
    /// Ctrl+click (⌘-click on macOS) follows it; a plain click selects, as in
    /// Word's default. The default.
    #[default]
    CtrlClick,
    /// A plain click (press and release without a drag) follows it; Ctrl+click
    /// does too.
    Click,
}

type LinkFn = Rc<dyn Fn(&str, &mut Window, &mut App)>;
type ChangeFn = Rc<dyn Fn(&mut App)>;

/// An embeddable view of a document, read-only unless made editable. Make
/// it with `cx.new(|cx| DocView::new(doc, Some(pkg), cx))` and place it
/// anywhere in the extension's view: it fills its parent. Give its
/// [`DocView::focus_handle`] as the tab's [`crate::ext::TabSurface::focus_handle`]
/// when it should have the keys; the keys it does not use bubble on.
pub struct DocView {
    editor: Editor,
    pkg: Option<Package>,
    pictures: Rc<RefCell<pictures::Cache>>,
    run_styles: RefCell<run_look::Cache>,
    zoom: f32,
    scroll: ScrollHandle,
    focus: FocusHandle,
    /// A press started a drag-selection that has not been released.
    selecting: bool,
    /// The hyperlink a plain press landed on (followed on its release in
    /// [`LinkClick::Click`] mode when nothing was dragged).
    press_link: Option<String>,
    link_click: LinkClick,
    on_link: Option<LinkFn>,
    content_dark: bool,
    /// The host's padding around the document (#2003); `None`:
    /// [`DEFAULT_PAGE_INSET`].
    page_inset: Option<Edges<Pixels>>,
    /// The host's colour for the dark page (#2003); `None`: the theme's
    /// background.
    page_bg: Option<Hsla>,
    /// The page colour the last frame painted, for the harness.
    painted_bg: Option<Hsla>,
    words: Words,
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The text width (px) as last painted, which tables fit to (#1895).
    text_w: Rc<Cell<Option<f32>>>,
    /// Each top-level table's painted bounds in the last frame (#1895).
    tables: PaintedTables,
    /// Each top-level paragraph's painted layout in the last frame, in a
    /// harness instance (#1986).
    paras: PaintedParas,
    /// The text column's bounds as last painted (#1986).
    column: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Bumped by [`DocView::set_document`]; `painted_gen` is the one the
    /// last painted frame drew, so the harness never reads an older
    /// document's layout (#1986).
    doc_gen: u64,
    painted_gen: Rc<Cell<Option<u64>>>,
    /// The scroll offset of the last render: a Ctrl+wheel puts it back, as
    /// the scroll area has already moved by then.
    held_scroll: Point<Pixels>,
    wheel_acc: f32,
    clipboard: ClipboardStore,
    /// The editing mode (#1947): off, the view is read-only.
    editable: bool,
    on_change: Option<ChangeFn>,
    /// A key or host command moved the caret or edited: the next frame
    /// scrolls the caret into view, once it is painted.
    reveal: bool,
    /// The view's scrollbars (#2080), the suite's document bars.
    vbar: crate::doc_scrollbar::Bar,
    hbar: crate::doc_scrollbar::Bar,
}

impl DocView {
    /// A view of `doc`, its pictures read from `pkg` (`None`: each picture
    /// draws as its frame).
    pub fn new(doc: Document, pkg: Option<Package>, cx: &mut Context<Self>) -> Self {
        // The host may not run the suite's `run` (#1900).
        crate::font_subst::init(cx);
        let pictures = Rc::new(RefCell::new(pictures::Cache::default()));
        pictures::register_extra(&pictures);
        cx.on_release(|this: &mut Self, _cx| {
            pictures::queue_release(this.pictures.borrow_mut().take_released(true));
        })
        .detach();
        DocView {
            editor: Editor::new(doc),
            pkg,
            pictures,
            run_styles: RefCell::default(),
            zoom: 1.0,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            selecting: false,
            press_link: None,
            link_click: LinkClick::default(),
            on_link: None,
            content_dark: false,
            page_inset: None,
            page_bg: None,
            painted_bg: None,
            words: Words::default(),
            viewport: Rc::default(),
            text_w: Rc::default(),
            tables: PaintedTables::default(),
            paras: PaintedParas::default(),
            column: Rc::default(),
            doc_gen: 0,
            painted_gen: Rc::default(),
            held_scroll: Point::default(),
            wheel_acc: 0.,
            clipboard: ClipboardStore::shared(windows::shared_clipboard_text(cx)),
            editable: false,
            on_change: None,
            reveal: false,
            vbar: Default::default(),
            hbar: Default::default(),
        }
    }

    /// Show `doc` (and `pkg`'s pictures) instead: the selection is cleared,
    /// the view goes back to the top, the zoom stays.
    pub fn set_document(&mut self, doc: Document, pkg: Option<Package>, cx: &mut Context<Self>) {
        let mut cache = self.pictures.borrow_mut();
        pictures::queue_release(cache.take_released(true));
        *cache = pictures::Cache::default();
        drop(cache);
        self.editor = Editor::new(doc);
        self.pkg = pkg;
        self.doc_gen += 1;
        self.run_styles = RefCell::default();
        self.selecting = false;
        self.press_link = None;
        self.reveal = false;
        self.scroll.set_offset(Point::default());
        if self.editable {
            self.seed_styles();
        }
        cx.notify();
    }

    /// Turn the editing mode on or off (#1947). Editable, the view draws a
    /// caret while focused, types and edits by the keys, and takes the host's
    /// commands ([`DocView::format`], [`DocView::undo`], ...); a hyperlink is
    /// followed only by Ctrl+click (⌘ on macOS), whatever
    /// [`DocView::set_link_click`] says, so a plain press places the caret.
    /// Off (the default) it is the read-only view. The document, its undo
    /// history and the selection stay either way.
    pub fn set_editable(&mut self, on: bool, cx: &mut Context<Self>) {
        self.editable = on;
        if on {
            self.seed_styles();
        }
        cx.notify();
    }

    /// Whether the view is in its editing mode.
    pub fn editable(&self) -> bool {
        self.editable
    }

    /// Called after every change to the document: typing, a delete, a cut,
    /// an insert, a formatting command, Undo and Redo, whether by the keys
    /// or the host.
    /// Not for what changes no document: a move, a selection, a copy, a scroll, a
    /// zoom, a format toggled at a collapsed caret for what is typed next,
    /// [`DocView::set_document`] or [`DocView::set_editable`]. It runs
    /// deferred, outside the view's update, so it may read or update the
    /// view (to autosave its [`DocView::document`], say).
    pub fn on_change(&mut self, f: impl Fn(&mut App) + 'static) {
        self.on_change = Some(Rc::new(f));
    }

    /// Apply a formatting command from the host's ribbon to the selection
    /// (or, for a toggle at a collapsed caret, to what is typed next), as
    /// the document tab's ribbon applies it: one undo step. True when it ran;
    /// false, changing nothing, on a read-only view or for a command it
    /// cannot run (an empty font name, a size outside 1..1638 pt, a colour
    /// that is not `RRGGBB`, a highlight that is not Word's). Pressing the
    /// host's button may have taken the focus: give it back
    /// ([`DocView::focus_handle`]), as the caret draws only while the view
    /// is focused.
    pub fn format(&mut self, f: DocFormat, cx: &mut Context<Self>) -> bool {
        if !self.editable {
            return false;
        }
        self.seed_styles();
        let done = edit::apply_format(&mut self.editor, self.pkg.as_mut(), &f);
        if done.ran {
            self.after_edit(done.changed, cx);
        }
        done.ran
    }

    /// Insert `text` at the caret, replacing the selection, as a host's
    /// Insert Signature or Quick Part does (#1977): newlines split
    /// paragraphs, a tab is a tab, `\u{0B}` (Word's plain-text manual line
    /// break) is a line break, and it all takes the formatting typing at the
    /// caret would give. One undo step of its own; the caret ends after it
    /// and [`DocView::on_change`] hears of it. Characters a document cannot
    /// hold (other control characters, U+FFFE, U+FFFF) are left out, and so
    /// is U+FFFC, the editor's stand-in for a field. True
    /// when it ran; false, changing nothing, on a read-only view or for a
    /// text with nothing left to insert. Give the focus back after a host's
    /// button, as for [`DocView::format`].
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if !self.editable {
            return false;
        }
        self.seed_styles();
        let done = edit::insert_text(&mut self.editor, text);
        if done.ran {
            self.after_edit(done.changed, cx);
        }
        done.ran
    }

    /// Copy the selection, as Ctrl+C does: its text and RTF on the
    /// clipboard. In either mode, as Ctrl+C copies in the read-only view
    /// too. Whether there was a selection; the document never changes.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        self.write_copy(cx)
    }

    /// Cut the selection, as Ctrl+X does: the clipboard gets what a copy
    /// would, and the selection is deleted as one undo step, which
    /// [`DocView::on_change`] hears of. A selection from body text into a
    /// table cell is copied but not deleted (the caret collapses, as in a
    /// document tab), so the host hears of no change. True when there was
    /// something to cut; false, changing nothing, on a read-only view or
    /// with nothing to cut.
    pub fn cut(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.editable {
            return false;
        }
        self.seed_styles();
        let Some((clip, changed)) = edit::cut(&mut self.editor) else {
            return false;
        };
        self.write_clip(&clip, cx);
        self.after_edit(changed, cx);
        true
    }

    /// Whether there is a selection, so [`DocView::copy`] would copy: for a
    /// host's Copy button, in either mode; its Cut button is
    /// `can_copy() && editable()`. Cheap enough to ask on every render.
    pub fn can_copy(&self) -> bool {
        self.editor.has_selection()
    }

    /// Undo the newest step, as Ctrl+Z does. False, with nothing changed, on
    /// a read-only view or with nothing to undo.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        self.history(false, cx)
    }

    /// Redo the newest undone step, as Ctrl+Y does.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        self.history(true, cx)
    }

    /// [`DocView::undo`] / [`DocView::redo`]: whether a step was taken;
    /// the host hears of a change only when the document differs after it,
    /// as for Ctrl+Z (a step may have changed nothing).
    fn history(&mut self, redo: bool, cx: &mut Context<Self>) -> bool {
        if !self.editable {
            return false;
        }
        let done = edit::history(&mut self.editor, redo);
        if done.ran {
            self.after_edit(done.changed, cx);
        }
        done.ran
    }

    /// Whether [`DocView::undo`] would undo something.
    pub fn can_undo(&self) -> bool {
        self.editable && self.editor.undo_serial().is_some()
    }

    /// Whether [`DocView::redo`] would redo something.
    pub fn can_redo(&self) -> bool {
        self.editable && self.editor.can_redo()
    }

    /// The formatting at the caret, for the host ribbon's pressed states:
    /// see [`CaretFormat`].
    pub fn caret_format(&self) -> CaretFormat {
        edit::caret_format(&self.editor)
    }

    /// The document as it is now, edits included: what the host exports.
    pub fn document(&self) -> Document {
        self.editor.export_doc().into_owned()
    }

    /// The package the view draws from, with what editing added to it (the
    /// numbering part a first list needs); `None` when it was given none.
    pub fn package(&self) -> Option<Package> {
        self.pkg.clone()
    }

    /// Bold and Italic toggle over what the package's styles give, and the
    /// caret's formatting reads them (#1427), as a document tab's editor
    /// is given them. Free while the styles part is unchanged.
    fn seed_styles(&mut self) {
        let xml = self.run_styles.borrow_mut().current(self.pkg.as_ref()).0;
        self.editor.set_styles_xml(xml);
    }

    /// After a key or host command: the caret goes into view, and a change
    /// tells the host.
    fn after_edit(&mut self, changed: bool, cx: &mut Context<Self>) {
        self.reveal = true;
        if changed {
            if let Some(f) = self.on_change.clone() {
                cx.defer(move |cx| f(cx));
            }
        }
        cx.notify();
    }

    /// Which press follows a hyperlink.
    pub fn set_link_click(&mut self, mode: LinkClick, cx: &mut Context<Self>) {
        self.link_click = mode;
        cx.notify();
    }

    /// Called with a hyperlink's target (its URL, or `#name` for a bookmark
    /// link) when the person follows it. It runs after the press is handled,
    /// outside the view's update, so it may update the view.
    pub fn on_link(&mut self, f: impl Fn(&str, &mut Window, &mut App) + 'static) {
        self.on_link = Some(Rc::new(f));
    }

    /// Dark mode for content: on a dark theme the document draws in the
    /// theme's colours instead of a white page (on a light theme it changes
    /// nothing, as Word's).
    pub fn set_content_dark(&mut self, on: bool, cx: &mut Context<Self>) {
        self.content_dark = on;
        cx.notify();
    }

    /// The padding around the document inside the scroll area, all four
    /// edges, in logical pixels (#2003): a host lines the text up with its
    /// own chrome this way. `None` keeps the default, 48 px left and right
    /// and 28 px top and bottom. It does not scale with the zoom, as the
    /// default does not; a negative edge counts as 0. The scroll bars (12 px
    /// strips along the right and bottom edges) draw over the padding, so
    /// an edge under 12 px lets them cover the text's edge.
    pub fn set_page_inset(&mut self, inset: Option<Edges<Pixels>>, cx: &mut Context<Self>) {
        if set_if_changed(&mut self.page_inset, inset) {
            cx.notify();
        }
    }

    /// The padding the host set with [`DocView::set_page_inset`] (`None`:
    /// the default).
    pub fn page_inset(&self) -> Option<Edges<Pixels>> {
        self.page_inset
    }

    /// The colour of the dark page, the one dark mode for content draws on
    /// a dark theme (#2003): a host whose chrome around the view is in
    /// another colour paints the page to match. `None` keeps the theme's
    /// background. The light page stays white, and the document's colours
    /// do not change: a host that wants other text colours for its page
    /// sets them in the document.
    pub fn set_page_background(&mut self, bg: Option<Hsla>, cx: &mut Context<Self>) {
        if set_if_changed(&mut self.page_bg, bg) {
            cx.notify();
        }
    }

    /// The dark page's colour the host set with
    /// [`DocView::set_page_background`] (`None`: the theme's background).
    pub fn page_background(&self) -> Option<Hsla> {
        self.page_bg
    }

    /// The zoom, 1.0 for 100%.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Set the zoom, kept within 50% to 300%.
    pub fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        cx.notify();
    }

    /// The selected text, as a copy would put it on the clipboard (empty
    /// without a selection).
    pub fn selected_text(&self) -> String {
        // The text alone: the RTF a copy makes beside it is not needed here.
        self.editor
            .copy()
            .map(|clip| doc_copy_text(&clip))
            .unwrap_or_default()
    }

    /// The view's focus: while focused it takes the keys it handles.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    // ---- the renderer's click target (`DocTarget::Embed`) -----------------

    pub(crate) fn begin_select(
        &mut self,
        path: Vec<usize>,
        off: usize,
        mods: Modifiers,
        on_piece: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        // Only a press on a painted piece is on a link: beside a row the
        // offset is the row's nearest stop, which may be a link's first or
        // last character the person never pressed.
        let link = on_piece
            .then(|| docxcore::editor::resolve_para(&self.editor.doc.body, &path))
            .flatten()
            .and_then(|p| link_at(p, off));
        // Ctrl+click (⌘ on macOS) follows a link at once, in either mode, and
        // selects nothing.
        if mods.secondary() && !mods.shift {
            if let Some(url) = link {
                self.selecting = false;
                self.press_link = None;
                self.follow(url, window, cx);
                return;
            }
        }
        place_click_caret(&mut self.editor, Caret::at(path, off), mods.shift, true);
        self.selecting = true;
        // Editable, a plain press places the caret on a link and never
        // follows it (Word's editing behaviour).
        self.press_link = link.filter(|_| !mods.modified() && !self.editable);
        cx.notify();
    }

    pub(crate) fn extend_select(
        &mut self,
        path: Vec<usize>,
        off: Option<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        if let (true, Some(off)) = (self.selecting, off) {
            let caret = Caret::at(path, off);
            if self.editor.caret != caret {
                self.editor.caret = caret;
                self.editor.clamp();
                cx.notify();
            }
        }
        self.selecting
    }

    pub(crate) fn set_caret(
        &mut self,
        path: Vec<usize>,
        off: usize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        place_click_caret(&mut self.editor, Caret::at(path, off), extend, false);
        self.press_link = None;
        cx.notify();
    }

    fn follow(&self, url: String, window: &mut Window, cx: &mut App) {
        if let Some(f) = self.on_link.clone() {
            window.defer(cx, move |window, cx| f(&url, window, cx));
        }
    }

    /// The release that ends a press: in [`LinkClick::Click`] mode a plain
    /// press on a link that dragged nothing follows it.
    fn release(&mut self, mods: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Some(url) = self.press_link.take() {
            if self.link_click == LinkClick::Click
                && !mods.modified()
                && !self.editor.has_selection()
            {
                self.follow(url, window, cx);
            }
        }
    }

    // ---- keys, wheel ------------------------------------------------------

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.editable && self.edit_key(ev, cx) {
            cx.stop_propagation();
            return;
        }
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let handled = if m.secondary() && !m.alt && !m.function {
            match key {
                "c" if !m.shift => {
                    self.write_copy(cx);
                    true
                }
                "a" if !m.shift => {
                    self.editor.select_all();
                    true
                }
                "=" | "+" => self.zoom_step(1),
                "-" if !m.shift => self.zoom_step(-1),
                "0" if !m.shift => self.zoom_step(0),
                "home" if !m.shift => self.scroll_to(0.),
                "end" if !m.shift => self.scroll_to(f32::INFINITY),
                _ => false,
            }
        } else if !m.modified() {
            let page = self
                .viewport
                .get()
                .map_or(400., |b| f32::from(b.size.height))
                - LINE_PX * self.zoom;
            match key {
                "up" => self.scroll_by(-LINE_PX * self.zoom),
                "down" => self.scroll_by(LINE_PX * self.zoom),
                "pageup" => self.scroll_by(-page.max(LINE_PX)),
                "pagedown" => self.scroll_by(page.max(LINE_PX)),
                "home" => self.scroll_to(0.),
                "end" => self.scroll_to(f32::INFINITY),
                _ => false,
            }
        } else {
            false
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
        let _ = window;
    }

    /// A key in the editing mode: whether the editing map took it. A key it
    /// leaves is the read-only map's (Page Up/Down, zoom) or bubbles.
    fn edit_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        self.seed_styles();
        let out = edit::edit_key(
            &mut self.editor,
            self.pkg.as_mut(),
            &ev.keystroke,
            text_input::MACOS,
        );
        let changed = out.changed();
        match out {
            edit::KeyOutcome::Ignored => return false,
            edit::KeyOutcome::Copy => {
                self.write_copy(cx);
            }
            edit::KeyOutcome::Cut(clip, _) => self.write_clip(&clip, cx),
            edit::KeyOutcome::Moved | edit::KeyOutcome::Changed => {}
        }
        // Ctrl+Home / Ctrl+End go to the very top and bottom, as read-only.
        let m = ev.keystroke.modifiers;
        if (m.control || m.platform) && !m.alt {
            match ev.keystroke.key.as_str() {
                "home" => {
                    self.scroll_to(0.);
                }
                "end" => {
                    self.scroll_to(f32::INFINITY);
                }
                _ => {}
            }
        }
        self.after_edit(changed, cx);
        true
    }

    /// Tab and Shift+Tab (action-bound, they never arrive as keys) in the
    /// editing mode: [`edit::tab`]. Read-only, they pass on as before.
    fn tab_action(&mut self, back: bool, cx: &mut Context<Self>) {
        if !self.editable {
            cx.propagate();
            return;
        }
        self.seed_styles();
        let out = edit::tab(&mut self.editor, self.pkg.as_mut(), back);
        self.after_edit(out.changed(), cx);
    }

    fn zoom_step(&mut self, dir: i32) -> bool {
        self.zoom = step_zoom(self.zoom, dir);
        true
    }

    fn scroll_y(&self) -> f32 {
        -f32::from(self.scroll.offset().y)
    }

    fn max_scroll_y(&self) -> f32 {
        f32::from(self.scroll.max_offset().y).max(0.)
    }

    /// The horizontal scroll, within what the last frame allowed: a wheel
    /// turn moves the offset at once, and gpui clamps it only as it paints.
    fn scroll_x(&self) -> f32 {
        (-f32::from(self.scroll.offset().x)).clamp(0., self.max_scroll_x())
    }

    fn max_scroll_x(&self) -> f32 {
        f32::from(self.scroll.max_offset().x).max(0.)
    }

    fn scroll_to(&mut self, y: f32) -> bool {
        let y = y.clamp(0., self.max_scroll_y());
        let x = self.scroll.offset().x;
        self.scroll.set_offset(point(x, px(-y)));
        true
    }

    fn scroll_by(&mut self, dy: f32) -> bool {
        self.scroll_to(self.scroll_y() + dy)
    }

    /// Ctrl+wheel zooms one step per notch, or per 40 px of a smooth wheel
    /// (the Project chart's rule, [`project::wheel_step`]; a fractional line
    /// counts as its pixels); the scroll area has already scrolled by the
    /// same turn, so the offset is put back.
    fn wheel(&mut self, ev: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        if !ev.modifiers.control {
            return;
        }
        cx.stop_propagation();
        self.scroll.set_offset(self.held_scroll);
        let (dy, notch) = project::wheel_px(&ev.delta, window.line_height());
        if ev.touch_phase == TouchPhase::Started {
            self.wheel_acc = 0.;
        }
        if let Some(dir) = project::wheel_step(&mut self.wheel_acc, dy, notch) {
            self.zoom_step(dir);
        }
        if ev.touch_phase == TouchPhase::Ended {
            self.wheel_acc = 0.;
        }
        cx.notify();
    }

    /// Ctrl+C: the selection as text and RTF, on the clipboard (the run's
    /// private one in a harness), as a document tab's copy writes it.
    /// Whether there was a selection to write.
    fn write_copy(&mut self, cx: &mut App) -> bool {
        let Some((text, rtf)) = copy_payload(&self.editor, self.pkg.as_ref()) else {
            return false;
        };
        self.clipboard.write_rich(harness(), text, rtf, cx);
        true
    }

    /// What a cut took, on the clipboard as [`DocView::write_copy`] writes a
    /// copy.
    fn write_clip(&mut self, clip: &Clip, cx: &mut App) {
        let (text, rtf) = clip_payload(clip, self.pkg.as_ref());
        self.clipboard.write_rich(harness(), text, rtf, cx);
    }

    // ---- what the harness reads -------------------------------------------

    /// The harness's `ext-doc-layout` (#1986): [`painted_layout_json`] of
    /// the last frame, or `painted: false` until a frame of the document
    /// last set has been painted.
    pub(crate) fn layout_json(&self) -> ctlcore::json::Json {
        use ctlcore::json::Json;
        match self.column.get() {
            Some(col) if self.painted_gen.get() == Some(self.doc_gen) => {
                painted_layout_json(&self.paras.borrow(), f32::from(col.origin.x))
            }
            _ => Json::obj(vec![
                ("painted", Json::Bool(false)),
                ("paragraphs", Json::Arr(Vec::new())),
            ]),
        }
    }

    /// The view's state for the harness's `ext-doc-read`.
    pub(crate) fn harness_json(&self, window: &Window) -> ctlcore::json::Json {
        use ctlcore::json::Json;
        let body = &self.editor.doc.body;
        let tables = body.iter().filter(|b| matches!(b, Block::Table(_))).count();
        let links = self
            .link_spans()
            .into_iter()
            .enumerate()
            .map(|(i, (url, _))| {
                Json::obj(vec![
                    ("index", Json::Num(i as f64)),
                    ("url", Json::Str(url)),
                ])
            })
            .collect();
        Json::obj(vec![
            ("blocks", Json::Num(body.len() as f64)),
            ("tables", Json::Num(tables as f64)),
            (
                "pictures",
                Json::Num(self.pictures.borrow().live_ids().count() as f64),
            ),
            ("links", Json::Arr(links)),
            ("selection", Json::Str(self.selected_text())),
            (
                "selected_paragraphs",
                Json::Num(self.editor.selection_spans().len() as f64),
            ),
            (
                "zoom",
                Json::Num((f64::from(self.zoom) * 100.).round() / 100.),
            ),
            ("scroll_y", Json::Num(f64::from(self.scroll_y().round()))),
            (
                "max_scroll_y",
                Json::Num(f64::from(self.max_scroll_y().round())),
            ),
            ("scroll_x", Json::Num(f64::from(self.scroll_x().round()))),
            (
                "max_scroll_x",
                Json::Num(f64::from(self.max_scroll_x().round())),
            ),
            // The scrollbars as last painted (#2080), `null` while hidden.
            ("vbar", crate::doc_scrollbar::bar_json(&self.vbar)),
            ("hbar", crate::doc_scrollbar::bar_json(&self.hbar)),
            (
                "text_width",
                self.text_w
                    .get()
                    .map_or(Json::Null, |w| Json::Num(f64::from(w.round()))),
            ),
            (
                "table_widths",
                painted_tables_json(
                    &self.tables,
                    self.text_w.get(),
                    self.viewport
                        .get()
                        .map(|v| (f32::from(v.origin.x), f32::from(v.size.width))),
                    None,
                ),
            ),
            (
                "link_click",
                Json::Str(
                    match self.link_click {
                        LinkClick::CtrlClick => "ctrl-click",
                        LinkClick::Click => "click",
                    }
                    .into(),
                ),
            ),
            ("content_dark", Json::Bool(self.content_dark)),
            ("page_inset", {
                let e = resolve_page_inset(self.page_inset);
                let n = |p: Pixels| Json::Num(f64::from(f32::from(p).round()));
                Json::obj(vec![
                    ("top", n(e.top)),
                    ("right", n(e.right)),
                    ("bottom", n(e.bottom)),
                    ("left", n(e.left)),
                ])
            }),
            (
                "page_background",
                self.page_bg.map_or(Json::Null, |c| Json::Str(ink_hex(c))),
            ),
            (
                "page_color",
                self.painted_bg
                    .map_or(Json::Null, |c| Json::Str(ink_hex(c))),
            ),
            (
                "column_left",
                match (self.column.get(), self.viewport.get()) {
                    (Some(col), Some(view)) => {
                        Json::Num(f64::from(f32::from(col.origin.x - view.origin.x).round()))
                    }
                    _ => Json::Null,
                },
            ),
            ("focused", Json::Bool(self.focus.is_focused(window))),
            ("editable", Json::Bool(self.editable)),
            (
                "caret",
                Json::obj(vec![
                    (
                        "path",
                        Json::Arr(
                            self.editor
                                .caret
                                .path
                                .iter()
                                .map(|&i| Json::Num(i as f64))
                                .collect(),
                        ),
                    ),
                    ("offset", Json::Num(self.editor.caret.offset as f64)),
                ]),
            ),
            (
                "caret_in_view",
                match (
                    caret_piece(&self.words, &self.editor.caret),
                    self.viewport.get(),
                ) {
                    // Clear of the horizontal bar, as the reveal is.
                    (Some(piece), Some(view)) => Json::Bool(
                        reveal_dy(
                            crate::doc_scrollbar::clear_of_hbar(view, self.hbar.shown()),
                            piece,
                        ) == px(0.),
                    ),
                    _ => Json::Null,
                },
            ),
            ("caret_format", caret_format_json(&self.caret_format())),
            ("can_undo", Json::Bool(self.can_undo())),
            ("can_redo", Json::Bool(self.can_redo())),
            ("can_copy", Json::Bool(self.can_copy())),
        ])
    }

    /// Each hyperlink as painted, in order: its target and the union of its
    /// painted words' bounds (consecutive words of one link in a paragraph
    /// are one span).
    pub(crate) fn link_spans(&self) -> Vec<(String, Bounds<Pixels>)> {
        let words = self.words.borrow();
        let pieces = words.iter().flat_map(|(path, spans)| {
            let para = docxcore::editor::resolve_para(&self.editor.doc.body, path);
            spans
                .borrow()
                .iter()
                .filter(|w| w.len > 0)
                .map(|w| {
                    (
                        path.clone(),
                        para.and_then(|p| link_at(p, w.off)),
                        w.bounds(),
                    )
                })
                .collect::<Vec<_>>()
        });
        merge_link_spans(pieces)
    }

    /// The first painted word whose text, trimmed, is `text`: its bounds,
    /// and the x just after its last character (before any trailing space).
    pub(crate) fn word_bounds(&self, text: &str) -> Option<(Bounds<Pixels>, Pixels)> {
        let words = self.words.borrow();
        words.iter().find_map(|(path, spans)| {
            let para = docxcore::editor::resolve_para(&self.editor.doc.body, path)?;
            let chars = caret_chars(para);
            spans.borrow().iter().find_map(|w| {
                let end = (w.off + w.len).min(chars.len());
                let word: String = chars.get(w.off..end)?.iter().collect();
                if w.len == 0 || word.trim() != text {
                    return None;
                }
                let bounds = w.bounds();
                let text_end = match &w.shape {
                    SpanShape::Text(layout) => layout
                        .position_for_index(word.trim_end().len())
                        .map_or(bounds.right(), |p| p.x),
                    SpanShape::Unit(_) => bounds.right(),
                };
                Some((bounds, text_end))
            })
        })
    }

    /// A scrollbar region (#2080): `vbar`, `hbar-thumb`, `vbar-down`, ...
    /// ([`crate::doc_scrollbar::bar_region`]).
    pub(crate) fn bar_region(&self, part: &str) -> Result<Bounds<Pixels>, String> {
        crate::doc_scrollbar::bar_region(&self.vbar, &self.hbar, part)
    }

    /// The scrolling area's bounds as last painted.
    pub(crate) fn viewport(&self) -> Option<Bounds<Pixels>> {
        self.viewport.get()
    }
}

impl Focusable for DocView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DocView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.words.borrow_mut().clear();
        self.tables.borrow_mut().clear();
        self.paras.borrow_mut().clear();
        self.held_scroll = self.scroll.offset();
        let theme_dark = cx.theme().mode.is_dark();
        let (bg, pal) = content_look(
            Pal::of(cx),
            cx.theme().background,
            theme_dark,
            self.content_dark,
            self.page_bg,
        );
        self.painted_bg = Some(bg);
        let inset = resolve_page_inset(self.page_inset);
        // The document's inherited font: the host's (whatever its ancestors
        // set), made loadable without touching the host's theme (#1900). The
        // same family `Measurer::new` measures in, so measured = drawn.
        let ui_font = crate::font_subst::ui_family(&window.text_style().font().family);
        let measurer = Measurer::new(window);
        let sheet = self.run_styles.borrow_mut().current(self.pkg.as_ref()).1;
        let tbl = TableCtx::new(self.pkg.as_ref(), sheet, false);
        let pics = self
            .pkg
            .as_ref()
            .map(|pkg| pictures::Pictures::new(pkg, &self.pictures));
        let spans = self.editor.selection_spans();
        let body = &self.editor.doc.body;
        let markers = list_markers(body);
        let target = DocTarget::Embed(cx.entity().downgrade(), self.words.clone());
        // Editable, the caret draws while the view has the keys (#1947).
        let caret: &[usize] = if self.editable && self.focus.is_focused(window) {
            &self.editor.caret.path
        } else {
            &[]
        };
        let reveal = std::mem::take(&mut self.reveal).then(|| self.editor.caret.clone());
        let ctx = RenderCtx {
            caret_path: caret,
            caret_off: self.editor.caret.offset,
            spans: &spans,
            ent: &target,
            pal,
            marks: false,
            zoom: self.zoom,
            active: true,
            meas: &measurer,
            hf_width: None,
            line_width: None,
            tbl: &tbl,
            cell_range: None,
            merge_hl: false,
            field_codes: false,
            cell_run: None,
            pics: pictures::scope(pics.as_ref(), Some(BODY_PART)),
            page_vals: None,
            // Tables fit the text width as last painted (#1895).
            fit_width: self.text_w.get(),
            tables: Some(&self.tables),
            // A flow view (#1986): fully spaced paragraphs drawn exactly,
            // the column's gap per block, the layout kept for the harness.
            flow: true,
            gap_below: false,
            paras: harness().then_some(&self.paras),
            inks: None,
        };
        let blocks: Vec<AnyElement> = body
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let ctx = RenderCtx {
                    gap_below: flow_gap_below(body, i),
                    ..ctx
                };
                block_el(b, vec![i], markers[i].as_deref(), ctx)
            })
            .collect();
        let viewport = self.viewport.clone();
        let (words, scroll) = (self.words.clone(), self.scroll.clone());
        let hbar = self.hbar.clone();
        let (used_w, text_w) = (self.text_w.get(), self.text_w.clone());
        let (column, painted_gen, doc_gen) =
            (self.column.clone(), self.painted_gen.clone(), self.doc_gen);
        div()
            .id("ext-doc-view")
            .font_family(ui_font)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .min_h(px(0.))
            .bg(bg)
            .on_key_down(cx.listener(Self::key))
            .on_action(
                cx.listener(|this, _: &InsertTabAction, _window, cx| this.tab_action(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OutdentAction, _window, cx| this.tab_action(true, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|this, ev: &ScrollWheelEvent, window, cx| this.wheel(ev, window, cx)),
            )
            // A press beside the paragraphs (the padding) still gives the
            // view the keys.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                    this.release(ev.modifiers, window, cx)
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                    this.release(ev.modifiers, window, cx)
                }),
            )
            .child(
                v_flex()
                    .id("ext-doc-scroll")
                    .track_scroll(&self.scroll)
                    .size_full()
                    // Both axes: a table wider than the view scrolls
                    // sideways (#1895).
                    .overflow_scroll()
                    .text_color(pal.fg)
                    // The host's padding, or the default (#2003).
                    .pt(inset.top)
                    .pr(inset.right)
                    .pb(inset.bottom)
                    .pl(inset.left)
                    .children(blocks)
                    // The text width, inside the padding: a percent width
                    // resolves against the scroll area, not its extent.
                    .child(div().relative().w_full().h(px(0.)).child(fit_width_probe(
                        used_w,
                        move |b, _| {
                            text_w.set(Some(f32::from(b.size.width)));
                            // Prepainted after every block (#1986).
                            column.set(Some(b));
                            painted_gen.set(Some(doc_gen));
                        },
                    ))),
            )
            .child(
                canvas(
                    move |b: Bounds<Pixels>, _w: &mut Window, _a: &mut App| viewport.set(Some(b)),
                    // It paints after `ext-doc-scroll`, whose pieces this
                    // frame has drawn by then: a paragraph an Enter just
                    // made among them.
                    move |b, _s, window, _a| {
                        if let Some(caret) = reveal {
                            // Clear of the horizontal bar over the view's
                            // bottom (#2080).
                            let view = crate::doc_scrollbar::clear_of_hbar(b, hbar.shown());
                            if reveal_caret(&words, &scroll, view, &caret) {
                                window.request_animation_frame();
                            }
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            // The suite's document bars (#2080), over the host's padding
            // along the right and bottom edges.
            .children(crate::doc_scrollbar::overlay_bars(
                &self.scroll,
                &self.vbar,
                &self.hbar,
                crate::doc_scrollbar::BarColors::of(cx),
            ))
    }
}

/// The text and RTF a copy of `editor`'s selection writes (the RTF resolves
/// styles through `pkg`, as a document copy's does); `None` with no selection.
pub(crate) fn copy_payload(
    editor: &Editor,
    pkg: Option<&Package>,
) -> Option<(String, Option<String>)> {
    Some(clip_payload(&editor.copy()?, pkg))
}

/// A colour as `rrggbb`, with no `#`: a case's `#` starts a comment.
pub(crate) fn ink_hex(c: Hsla) -> String {
    let c = Rgba::from(c);
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
}

/// One painted line of a paragraph (#1986): its top and left, its right
/// edge, its text, and its first word's size and ink.
#[derive(Clone, Debug, PartialEq)]
struct LaidLine {
    y: f32,
    x: f32,
    right: f32,
    text: String,
    size: f32,
    ink: Hsla,
}

/// The lines `para` painted: its words in order, joined with the run
/// holding each by offset; a word left of the one before it starts a line,
/// as the row wrapped.
fn painted_lines(para: &PaintedPara) -> Vec<LaidLine> {
    let mut lines: Vec<LaidLine> = Vec::new();
    for w in para.words.borrow().iter() {
        let Some(run) = para
            .runs
            .iter()
            .find(|r| r.off <= w.off && w.off < r.off + r.len)
        else {
            continue;
        };
        let b = w.bounds();
        let (x, y, right) = (
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.origin.x + b.size.width),
        );
        let text: String = run.text.chars().skip(w.off - run.off).take(w.len).collect();
        match lines.last_mut() {
            Some(line) if x >= line.right - 0.5 => {
                line.right = right;
                line.y = line.y.min(y);
                line.text.push_str(&text);
            }
            _ => lines.push(LaidLine {
                y,
                x,
                right,
                text,
                size: run.size,
                ink: run.ink,
            }),
        }
    }
    for line in &mut lines {
        line.text.truncate(line.text.trim_end().len());
    }
    lines
}

/// The harness's `ext-doc-layout` and `doc`'s `flow.layout` (#1986) of
/// the paragraphs a frame painted (`paras`): per paragraph its block, top and height,
/// its text's left (`text_x`), its list number and its lines, px rounded,
/// x from the text column's left (`column_x`) and y from the first
/// paragraph's top; `marker_matches_text` (the number's ink is its first
/// line's) and `marker_fits` (it ends left of the text), null without a
/// number; and the least and greatest pitch between consecutive lines of
/// the document, an empty paragraph counting as one line at its top.
pub(crate) fn painted_layout_json(paras: &[PaintedPara], column_x: f32) -> ctlcore::json::Json {
    use ctlcore::json::Json;
    let num = |v: f32| Json::Num(f64::from(v.round()));
    let y0 = paras
        .first()
        .map_or(0., |p| f32::from(p.root.get().origin.y));
    let mut tops: Vec<f32> = Vec::new();
    let out = paras
        .iter()
        .map(|para| {
            let root = para.root.get();
            let lines = painted_lines(para);
            let top = f32::from(root.origin.y) - y0;
            if lines.is_empty() {
                tops.push(top.round());
            }
            tops.extend(lines.iter().map(|l| (l.y - y0).round()));
            let text_x = lines.first().map(|l| l.x - column_x);
            let marker = para.marker.as_ref().map(|m| {
                let at = m.at.get();
                let (x, right) = (
                    f32::from(at.origin.x) - column_x,
                    f32::from(at.origin.x + at.size.width) - column_x,
                );
                (m, x, right)
            });
            let matches = marker.as_ref().map_or(Json::Null, |(m, _, _)| {
                Json::Bool(
                    lines
                        .first()
                        .is_some_and(|l| ink_hex(l.ink) == ink_hex(m.ink)),
                )
            });
            let fits = marker.as_ref().map_or(Json::Null, |(_, _, right)| {
                Json::Bool(text_x.is_some_and(|t| right.round() <= t.round()))
            });
            Json::obj(vec![
                ("block", Json::Num(para.block as f64)),
                ("top", num(top)),
                ("height", num(f32::from(root.size.height))),
                ("text_x", text_x.map_or(Json::Null, num)),
                (
                    "marker",
                    marker.map_or(Json::Null, |(m, x, right)| {
                        Json::obj(vec![
                            ("text", Json::Str(m.text.clone())),
                            ("x", num(x)),
                            ("right", num(right)),
                            ("size", num(m.size)),
                            ("color", Json::Str(ink_hex(m.ink))),
                        ])
                    }),
                ),
                ("marker_matches_text", matches),
                ("marker_fits", fits),
                (
                    "lines",
                    Json::Arr(
                        lines
                            .iter()
                            .map(|l| {
                                Json::obj(vec![
                                    ("y", num(l.y - y0)),
                                    ("x", num(l.x - column_x)),
                                    ("text", Json::Str(l.text.clone())),
                                    ("size", num(l.size)),
                                    ("color", Json::Str(ink_hex(l.ink))),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    let pitches: Vec<f32> = tops.windows(2).map(|w| w[1] - w[0]).collect();
    let pitch = |f: fn(f32, f32) -> f32| pitches.iter().copied().reduce(f).map_or(Json::Null, num);
    Json::obj(vec![
        ("painted", Json::Bool(true)),
        ("paragraphs", Json::Arr(out)),
        ("pitch_min", pitch(f32::min)),
        ("pitch_max", pitch(f32::max)),
    ])
}

/// [`CaretFormat`] for the harness's `ext-doc-read`.
fn caret_format_json(f: &CaretFormat) -> ctlcore::json::Json {
    use ctlcore::json::Json;
    let text = |v: &Option<String>| v.clone().map_or(Json::Null, Json::Str);
    Json::obj(vec![
        ("bold", Json::Bool(f.bold)),
        ("italic", Json::Bool(f.italic)),
        ("underline", Json::Bool(f.underline)),
        ("strike", Json::Bool(f.strike)),
        ("font", text(&f.font)),
        (
            "size",
            f.size.map_or(Json::Null, |s| Json::Num(f64::from(s))),
        ),
        ("color", text(&f.color)),
        ("highlight", text(&f.highlight)),
        ("bullets", Json::Bool(f.bullets)),
        ("numbering", Json::Bool(f.numbering)),
        ("align", Json::Str(align_name(f.align).into())),
    ])
}

/// An alignment's name in the harness's replies and calls.
pub(crate) fn align_name(a: Align) -> &'static str {
    match a {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "justify",
    }
}

/// The painted bounds of the piece holding `caret` among `words` (the last
/// frame's): [`piece_at`] over its paragraph's pieces. `None` when its
/// paragraph painted none.
fn caret_piece(words: &Words, caret: &Caret) -> Option<Bounds<Pixels>> {
    let words = words.borrow();
    let (_, spans) = words.iter().find(|(path, _)| *path == caret.path)?;
    let pieces: Vec<_> = spans
        .borrow()
        .iter()
        .map(|w| (w.off, w.len, w.bounds()))
        .collect();
    piece_at(&pieces, caret.offset)
}

/// Of a paragraph's painted pieces `(offset, length, bounds)`, the bounds of
/// the one whose characters hold caret offset `off`, else the last one that
/// ends at or before it (the caret after the paragraph's last character, or
/// past a unit), else the first. A piece that drew nothing (a line break's
/// empty bounds) is never one.
fn piece_at(pieces: &[(usize, usize, Bounds<Pixels>)], off: usize) -> Option<Bounds<Pixels>> {
    let drawn = || pieces.iter().filter(|(_, _, b)| b.size.height > px(0.));
    drawn()
        .find(|(o, l, _)| *o <= off && off < o + l)
        .or_else(|| drawn().rfind(|(o, l, _)| o + l <= off))
        .or_else(|| drawn().next())
        .map(|(_, _, b)| *b)
}

/// The change of the scroll area's y offset that brings `piece` into `view`
/// (both painted bounds): down to a piece above it, up to one below, none
/// for one inside; a piece taller than the view shows its top.
fn reveal_dy(view: Bounds<Pixels>, piece: Bounds<Pixels>) -> Pixels {
    if piece.top() < view.top() || piece.size.height > view.size.height {
        view.top() - piece.top()
    } else if piece.bottom() > view.bottom() {
        view.bottom() - piece.bottom()
    } else {
        px(0.)
    }
}

/// Scroll the view so `caret` is in sight, from this frame's painted pieces,
/// in y only (a wide table keeps its sideways scroll, #1895); a caret whose
/// paragraph painted nothing has its top-level block followed instead, as a
/// document tab's Web Layout follows it. Whether it moved (shown next frame).
fn reveal_caret(words: &Words, scroll: &ScrollHandle, view: Bounds<Pixels>, caret: &Caret) -> bool {
    let Some(piece) = caret_piece(words, caret) else {
        return caret
            .path
            .first()
            .is_some_and(|&b| follow_item_y(scroll, b, view));
    };
    let dy = reveal_dy(view, piece);
    if dy == px(0.) {
        return false;
    }
    let offset = scroll.offset();
    let max = scroll.max_offset().y.max(px(0.));
    let y = (offset.y + dy).clamp(-max, px(0.));
    if y == offset.y {
        return false;
    }
    scroll.set_offset(point(offset.x, y));
    true
}

/// The page the document draws on and its palette. A light page (white, in
/// the ink Print Layout's sheet uses) unless dark mode for content is on and
/// the theme is dark: then the host's page colour (`page_bg`, #2003), else
/// the theme's background, with the document's colours mapped against it as
/// Word's dark mode maps them (`dark_page_pal`, #2079). The selection is the
/// theme's either way.
pub(crate) fn content_look(
    theme: Pal,
    theme_bg: Hsla,
    theme_dark: bool,
    content_dark: bool,
    page_bg: Option<Hsla>,
) -> (Hsla, Pal) {
    if content_dark && theme_dark {
        let bg = page_bg.unwrap_or(theme_bg);
        return (bg, crate::dark_page_pal(crate::rgb_u(bg), theme.sel));
    }
    (hsla_u(0xffffff), page_pal(0xffffff, theme.sel))
}

/// The padding the view draws around the document (#2003): the host's, each
/// edge at least 0, else [`DEFAULT_PAGE_INSET`].
pub(crate) fn resolve_page_inset(inset: Option<Edges<Pixels>>) -> Edges<Pixels> {
    let Some(e) = inset else {
        return DEFAULT_PAGE_INSET;
    };
    let edge = |p: Pixels| p.max(px(0.));
    Edges {
        top: edge(e.top),
        right: edge(e.right),
        bottom: edge(e.bottom),
        left: edge(e.left),
    }
}

/// Store `v` in `slot`; whether it differed from what was there, which is
/// when a setter notifies (#2003).
fn set_if_changed<T: PartialEq>(slot: &mut T, v: T) -> bool {
    if *slot == v {
        return false;
    }
    *slot = v;
    true
}

/// One zoom step from `zoom`: in (`dir` > 0), out (< 0) or back to 100%
/// (0), by a tenth within 50%..300%, landing on a tenth.
pub(crate) fn step_zoom(zoom: f32, dir: i32) -> f32 {
    if dir == 0 {
        return 1.0;
    }
    let tenths = (zoom * 10.).round() as i32 + dir.signum();
    (tenths as f32 / 10.).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// The target of the hyperlink holding caret offset `off` of `p`: its URL,
/// else `#` and its bookmark; `None` outside a link (or a link with neither).
pub(crate) fn link_at(p: &Paragraph, off: usize) -> Option<String> {
    let mut at = 0;
    for inline in &p.content {
        let len = docxcore::editor::inline_len(inline);
        if let Inline::Hyperlink(h) = inline {
            if (at..at + len).contains(&off) {
                return h
                    .target
                    .clone()
                    .or_else(|| h.anchor.as_ref().map(|a| format!("#{a}")));
            }
        }
        at += len;
    }
    None
}

/// A paragraph's characters by caret offset: run text as it is, every other
/// unit (tab, break, field, picture) one [`docxcore::editor::FIELD_CHAR`] per
/// offset it takes. Only for naming painted words.
fn caret_chars(p: &Paragraph) -> Vec<char> {
    fn push(inline: &Inline, out: &mut Vec<char>) {
        match inline {
            Inline::Run(r) => out.extend(r.text.chars()),
            Inline::Hyperlink(h) => {
                for r in &h.runs {
                    out.extend(r.text.chars());
                }
                for c in &h.content {
                    push(c, out);
                }
            }
            other => out.extend(std::iter::repeat_n(
                docxcore::editor::FIELD_CHAR,
                docxcore::editor::inline_len(other),
            )),
        }
    }
    let mut out = Vec::new();
    for inline in &p.content {
        push(inline, &mut out);
    }
    out
}

/// Painted pieces (paragraph path, the link holding the piece, its bounds)
/// in painted order into one span per run of consecutive pieces of one link
/// in one paragraph, its bounds their union.
fn merge_link_spans(
    pieces: impl IntoIterator<Item = (Vec<usize>, Option<String>, Bounds<Pixels>)>,
) -> Vec<(String, Bounds<Pixels>)> {
    let mut out: Vec<(String, Bounds<Pixels>)> = Vec::new();
    let mut last: Option<(Vec<usize>, String)> = None;
    for (path, link, bounds) in pieces {
        match link {
            Some(url) => {
                let joins = last.as_ref().is_some_and(|(p, u)| *p == path && *u == url);
                if joins {
                    let span = out.last_mut().expect("a span to join");
                    span.1 = span.1.union(&bounds);
                } else {
                    out.push((url.clone(), bounds));
                }
                last = Some((path, url));
            }
            None => last = None,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::model::{Hyperlink, Run};

    /// A painted piece at `x`..`x + w`, `y`, holding caret offsets
    /// `off..off + len`.
    fn piece(off: usize, len: usize, x: f32, y: f32, w: f32) -> WordSpan {
        let cell = UnitCell::default();
        cell.set(Bounds::new(point(px(x), px(y)), size(px(w), px(18.))));
        WordSpan {
            shape: SpanShape::Unit(cell),
            off,
            len,
            trailing_space: false,
        }
    }

    /// #1986: the layout dump joins painted words with their runs by
    /// offset, starts a line where the row wrapped, and reports positions
    /// from the text column and the first paragraph, pitches and whether
    /// a number is in its text's ink.
    #[test]
    fn layout_dump_reads_lines_markers_and_pitch() {
        use ctlcore::json::Json;
        let ink = hsla_u(0x112233);
        let at = |x: f32, y: f32, w: f32, h: f32| {
            let c = UnitCell::default();
            c.set(Bounds::new(point(px(x), px(y)), size(px(w), px(h))));
            c
        };
        let words = WordSpans::default();
        words.borrow_mut().extend([
            piece(0, 5, 140., 128., 30.),
            piece(5, 4, 170., 128., 26.),
            piece(9, 5, 140., 146., 40.),
        ]);
        let paras = vec![
            PaintedPara {
                block: 0,
                root: at(100., 128., 500., 36.),
                marker: Some(PaintedMarker {
                    text: "1.".into(),
                    size: 16.,
                    ink,
                    at: at(122., 128., 13., 18.),
                }),
                runs: vec![PaintedRun {
                    off: 0,
                    len: 14,
                    size: 16.,
                    ink,
                    text: "Item one wraps".into(),
                }],
                words,
            },
            PaintedPara {
                block: 1,
                root: at(100., 164., 500., 18.),
                marker: None,
                runs: Vec::new(),
                words: WordSpans::default(),
            },
        ];
        let j = painted_layout_json(&paras, 100.);
        let get = |path: &str| {
            path.split('.')
                .try_fold(&j, |v, k| match k.parse::<usize>() {
                    Ok(i) => v.as_array().and_then(|a| a.get(i)),
                    Err(_) => v.get(k),
                })
        };
        let num = |path: &str| get(path).and_then(Json::as_f64).unwrap();
        let text = |path: &str| get(path).and_then(Json::as_str).unwrap().to_string();
        assert_eq!(text("paragraphs.0.lines.0.text"), "Item one");
        assert_eq!(text("paragraphs.0.lines.1.text"), "wraps");
        assert_eq!(num("paragraphs.0.lines.1.y"), 18.);
        assert_eq!(num("paragraphs.0.text_x"), 40.);
        assert_eq!(num("paragraphs.0.marker.x"), 22.);
        assert_eq!(text("paragraphs.0.lines.0.color"), "112233");
        assert_eq!(
            get("paragraphs.0.marker_matches_text"),
            Some(&Json::Bool(true))
        );
        assert_eq!(get("paragraphs.0.marker_fits"), Some(&Json::Bool(true)));
        assert_eq!(get("paragraphs.1.marker_matches_text"), Some(&Json::Null));
        assert_eq!(num("paragraphs.1.top"), 36.);
        assert_eq!((num("pitch_min"), num("pitch_max")), (18., 18.));
    }

    fn run(text: &str) -> Inline {
        Inline::Run(Run {
            text: text.into(),
            ..Run::default()
        })
    }

    fn link(text: &str, target: Option<&str>, anchor: Option<&str>) -> Inline {
        Inline::Hyperlink(Hyperlink {
            target: target.map(str::to_string),
            anchor: anchor.map(str::to_string),
            runs: vec![Run {
                text: text.into(),
                ..Run::default()
            }],
            ..Hyperlink::default()
        })
    }

    #[test]
    fn link_at_finds_the_link_under_an_offset() {
        let p = Paragraph {
            content: vec![
                run("see "),
                link("docxy", Some("https://example.com/d"), None),
                run(" and "),
                link("here", None, Some("top")),
            ],
            ..Paragraph::default()
        };
        assert_eq!(link_at(&p, 0), None);
        assert_eq!(link_at(&p, 3), None);
        assert_eq!(link_at(&p, 4).as_deref(), Some("https://example.com/d"));
        assert_eq!(link_at(&p, 8).as_deref(), Some("https://example.com/d"));
        // One past the link's last character is the text after it.
        assert_eq!(link_at(&p, 9), None);
        assert_eq!(link_at(&p, 14).as_deref(), Some("#top"));
        assert_eq!(link_at(&p, 18), None);
        // A link with neither a target nor a bookmark follows nowhere.
        let bare = Paragraph {
            content: vec![link("x", None, None)],
            ..Paragraph::default()
        };
        assert_eq!(link_at(&bare, 0), None);
    }

    #[test]
    fn zoom_steps_by_a_tenth_and_clamps() {
        let mut z = 1.0;
        for _ in 0..5 {
            z = step_zoom(z, 1);
        }
        assert_eq!(z, 1.5);
        assert_eq!(step_zoom(1.5, -1), 1.4);
        assert_eq!(step_zoom(3.0, 1), 3.0);
        assert_eq!(step_zoom(0.5, -1), 0.5);
        assert_eq!(step_zoom(2.3, 0), 1.0);
        // An odd zoom (set by the extension) lands on a tenth.
        assert_eq!(step_zoom(1.234, 1), 1.3);
    }

    #[test]
    fn wheel_zoom_accumulates_pixels_and_steps_on_a_notch() {
        let step = project::wheel_step;
        let mut acc = 0.;
        assert_eq!(step(&mut acc, 1., true), Some(1));
        assert_eq!(step(&mut acc, -1., true), Some(-1));
        assert_eq!(step(&mut acc, 25., false), None);
        assert_eq!(step(&mut acc, 20., false), Some(1));
        assert_eq!(acc, 0.);
        // A change of direction starts the sum afresh.
        assert_eq!(step(&mut acc, 30., false), None);
        assert_eq!(step(&mut acc, -30., false), None);
        assert_eq!(step(&mut acc, -15., false), Some(-1));
        assert_eq!(step(&mut acc, 0., false), None);
    }

    #[test]
    fn a_fractional_line_counts_as_its_pixels_toward_a_step() {
        // A hi-res wheel's half line at a 20 px line: 10 px, not a notch, so
        // the fourth such turn makes the step (not the 80th, as raw lines
        // against 40 px would).
        let half = ScrollDelta::Lines(point(0., 0.5));
        let (dy, notch) = project::wheel_px(&half, px(20.));
        assert_eq!((dy, notch), (10., false));
        let mut acc = 0.;
        for _ in 0..3 {
            assert_eq!(project::wheel_step(&mut acc, dy, notch), None);
        }
        assert_eq!(project::wheel_step(&mut acc, dy, notch), Some(1));
        // A whole line is a notch and steps at once; pixels are never one.
        let line = ScrollDelta::Lines(point(0., -1.));
        assert_eq!(project::wheel_px(&line, px(20.)), (-20., true));
        let smooth = ScrollDelta::Pixels(point(px(0.), px(-60.)));
        assert_eq!(project::wheel_px(&smooth, px(20.)), (-60., false));
    }

    fn pal(fg: u32) -> Pal {
        let c = hsla_u(fg);
        Pal {
            fg: c,
            dim: c,
            border: c,
            panel: c,
            hover: c,
            sel: hsla_u(0x123456),
            link: hsla_u(0x0000ff),
            ground: 0x1e1e1e,
            dark: false,
        }
    }

    #[test]
    fn content_is_a_white_page_unless_dark_mode_for_content_on_a_dark_theme() {
        let theme = pal(0xeeeeee);
        let theme_bg = hsla_u(0x1e1e1e);
        let ink = hsla_u(design_tab::page_ink(0xffffff).0);
        // Light content on a dark theme: the white page, its ink, the
        // theme's selection.
        let (bg, p) = content_look(theme, theme_bg, true, false, None);
        assert_eq!(bg, hsla_u(0xffffff));
        assert_eq!(p.fg, ink);
        assert_eq!(p.sel, hsla_u(0x123456));
        // Dark mode for content on a dark theme: the theme's background,
        // the document's colours mapped against it (#2079).
        let (bg, p) = content_look(theme, theme_bg, true, true, None);
        assert_eq!(bg, theme_bg);
        assert!(p.dark);
        assert_eq!(p.ground, 0x1e1e1e);
        assert_eq!(p.fg, hsla_u(crate::dark_page::AUTO_DARK));
        assert_eq!(p.sel, hsla_u(0x123456));
        // On a light theme it changes nothing.
        let (bg, p) = content_look(theme, theme_bg, false, true, None);
        assert_eq!(bg, hsla_u(0xffffff));
        assert_eq!(p.fg, ink);
    }

    #[test]
    fn the_host_paints_the_dark_page_and_the_palette_maps_against_it_2003_2079() {
        let theme = pal(0xeeeeee);
        let theme_bg = hsla_u(0x0a0a0a);
        let host = hsla_u(0x262626);
        let ink = hsla_u(design_tab::page_ink(0xffffff).0);
        // Unset: the theme's background, as before.
        let (bg, p) = content_look(theme, theme_bg, true, true, None);
        assert_eq!(bg, theme_bg);
        assert_eq!(p.ground, 0x0a0a0a);
        // Set: the host's colour, and the dark mapping against it (#2079):
        // automatic text and an uncoloured link that read on it.
        let (bg, p) = content_look(theme, theme_bg, true, true, Some(host));
        assert_eq!(bg, host);
        assert!(p.dark);
        assert_eq!(p.ground, 0x262626);
        let chip = crate::dark_page::chip(0x262626);
        assert_eq!(p.fg, hsla_u(crate::dark_page::auto_text(chip)));
        assert_eq!(
            p.link,
            hsla_u(crate::dark_page::text(crate::LINK_DARK, chip))
        );
        assert_eq!(p.sel, hsla_u(0x123456));
        // A light host page under dark content takes dark ink.
        let (_, p) = content_look(theme, theme_bg, true, true, Some(hsla_u(0xffffff)));
        assert!(crate::dark_page::luminance(crate::rgb_u(p.fg)) < 0.05);
        // The light page stays white: light content, or a light theme.
        for (theme_dark, content_dark) in [(true, false), (false, true), (false, false)] {
            let (bg, p) = content_look(theme, theme_bg, theme_dark, content_dark, Some(host));
            assert_eq!(bg, hsla_u(0xffffff), "{theme_dark} {content_dark}");
            assert_eq!(p.fg, ink);
        }
        // Reset to `None`: the theme's background again.
        let (bg, _) = content_look(theme, theme_bg, true, true, None);
        assert_eq!(bg, theme_bg);
    }

    #[test]
    fn the_page_inset_is_the_hosts_or_48_by_28_and_never_negative_2003() {
        let e = |t: f32, r: f32, b: f32, l: f32| Edges {
            top: px(t),
            right: px(r),
            bottom: px(b),
            left: px(l),
        };
        assert_eq!(resolve_page_inset(None), e(28., 48., 28., 48.));
        assert_eq!(
            resolve_page_inset(Some(e(12., 22., 12., 22.))),
            e(12., 22., 12., 22.)
        );
        assert_eq!(
            resolve_page_inset(Some(e(-4., 0., 3., -1.))),
            e(0., 0., 3., 0.)
        );
    }

    #[test]
    fn a_page_setter_notifies_only_when_its_value_changes_2003() {
        let inset = Edges::all(px(10.));
        let mut slot: Option<Edges<Pixels>> = None;
        assert!(set_if_changed(&mut slot, Some(inset)));
        assert!(!set_if_changed(&mut slot, Some(inset)));
        assert_eq!(slot, Some(inset));
        assert!(set_if_changed(&mut slot, None));
        assert!(!set_if_changed(&mut slot, None));
        let mut bg: Option<Hsla> = None;
        assert!(set_if_changed(&mut bg, Some(hsla_u(0x262626))));
        assert!(!set_if_changed(&mut bg, Some(hsla_u(0x262626))));
        assert!(set_if_changed(&mut bg, None));
        assert!(!set_if_changed(&mut bg, None));
    }

    #[test]
    fn an_uncoloured_link_reads_on_the_light_and_the_dark_content_page_1906() {
        use crate::style_gallery::contrast;
        use gpui_component::ThemeColor;
        // The palettes `Pal::of` builds from gpui-component's stock themes,
        // and each theme's background.
        for theme_dark in [false, true] {
            let t = if theme_dark {
                ThemeColor::dark()
            } else {
                ThemeColor::light()
            };
            let theme = Pal::from_theme(&t, theme_dark);
            for content_dark in [false, true] {
                let (bg, p) = content_look(theme, t.background, theme_dark, content_dark, None);
                let r = contrast(p.link.to_rgb(), bg.to_rgb());
                assert!(
                    r >= 4.5,
                    "theme dark {theme_dark}, content dark {content_dark}: {r:.2}"
                );
                // The dark page draws the theme's light link blue, the white
                // page the usual one.
                let want = if theme_dark && content_dark {
                    crate::LINK_DARK
                } else {
                    crate::LINK
                };
                assert_eq!(p.link, hsla_u(want));
            }
        }
    }

    fn b(x: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(0.)),
            size: size(px(10.), px(10.)),
        }
    }

    #[test]
    fn consecutive_pieces_of_one_link_are_one_span() {
        let u = |s: &str| Some(s.to_string());
        let spans = merge_link_spans(vec![
            (vec![1], None, b(0.)),
            (vec![1], u("a"), b(10.)),
            (vec![1], u("a"), b(20.)),
            (vec![1], u("b"), b(30.)),
            (vec![2], u("b"), b(40.)),
            (vec![2], None, b(50.)),
            (vec![2], u("b"), b(60.)),
        ]);
        let got: Vec<(&str, f32, f32)> = spans
            .iter()
            .map(|(u, b)| (u.as_str(), f32::from(b.left()), f32::from(b.right())))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", 10., 30.),
                ("b", 30., 40.),
                ("b", 40., 50.),
                ("b", 60., 70.)
            ]
        );
    }

    fn rows(top: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.), px(top)),
            size: size(px(10.), px(h)),
        }
    }

    #[test]
    fn the_caret_piece_holds_its_offset_or_ends_before_it_1947() {
        let empty = Bounds::default();
        // "ab cd" as two words, then a line break (drawn as nothing), then
        // "ef" on the next row.
        let pieces = [
            (0, 3, rows(0., 20.)),
            (3, 2, rows(0., 20.)),
            (5, 1, empty),
            (6, 2, rows(20., 20.)),
        ];
        assert_eq!(piece_at(&pieces, 0), Some(rows(0., 20.)));
        assert_eq!(piece_at(&pieces, 4), Some(rows(0., 20.)));
        // On the break: the word before it, not its empty bounds.
        assert_eq!(piece_at(&pieces, 5), Some(rows(0., 20.)));
        assert_eq!(piece_at(&pieces, 7), Some(rows(20., 20.)));
        // After the last character: the last piece.
        assert_eq!(piece_at(&pieces, 8), Some(rows(20., 20.)));
        assert_eq!(piece_at(&[], 0), None);
        assert_eq!(piece_at(&[(0, 0, empty)], 0), None);
    }

    #[test]
    fn a_piece_out_of_view_is_scrolled_to_its_nearest_edge_1947() {
        let view = rows(100., 300.);
        // Inside: no scroll.
        assert_eq!(reveal_dy(view, rows(150., 20.)), px(0.));
        assert_eq!(reveal_dy(view, rows(100., 300.)), px(0.));
        // Above: the content moves down until the piece's top is the view's.
        assert_eq!(reveal_dy(view, rows(40., 20.)), px(60.));
        // Below: up until its bottom is the view's.
        assert_eq!(reveal_dy(view, rows(390., 20.)), px(-10.));
        assert_eq!(reveal_dy(view, rows(900., 20.)), px(-520.));
        // Taller than the view: its top.
        assert_eq!(reveal_dy(view, rows(120., 400.)), px(-20.));
        // #2080 r3: a line in the view's bottom 12 px, under the
        // horizontal bar while it is shown, comes up clear of it.
        let clear = crate::doc_scrollbar::clear_of_hbar(view, true);
        assert_eq!(reveal_dy(view, rows(385., 10.)), px(0.));
        assert_eq!(reveal_dy(clear, rows(385., 10.)), px(-7.));
    }

    #[test]
    fn caret_chars_count_units_as_the_editor_does() {
        let p = Paragraph {
            content: vec![
                run("ab"),
                Inline::Tab(Default::default()),
                link("cd", Some("u"), None),
            ],
            ..Paragraph::default()
        };
        let chars = caret_chars(&p);
        assert_eq!(chars.len(), docxcore::editor::para_text_len(&p));
        assert_eq!(chars[4], 'd');
    }
}
