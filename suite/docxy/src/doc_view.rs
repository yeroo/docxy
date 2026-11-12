//! A document view an extension tab can embed (#1873): a
//! [`docxcore::model::Document`] drawn by the suite's own document renderer
//! in its flow (web layout) style, wrapped to the view's width, with inline
//! pictures from the package it came with, or in Print Layout's page sheets
//! ([`DocView::set_layout`], #1888). It selects (press, drag,
//! Shift+press, Ctrl+A) and copies (Ctrl+C: text and RTF, as a document copy
//! does), scrolls (wheel, Up/Down, Page Up/Down, Home/End) and zooms
//! (Ctrl+wheel, Ctrl+=, Ctrl+-, Ctrl+0). Read-only by default: it then never
//! edits and draws no caret. Its editing mode ([`DocView::set_editable`],
//! #1947) types, deletes, moves the caret, undoes and takes a host ribbon's
//! formatting commands ([`DocView::format`]) and its Insert, Copy and Cut
//! ([`DocView::insert_text`], [`DocView::copy`], [`DocView::cut`], #1977);
//! its decisions are [`edit`]'s. A hyperlink press goes to the extension's callback
//! ([`DocView::on_link`], [`DocView::on_link_with_text`]), never opened here; the
//! link under the pointer goes to [`DocView::on_link_hover`] and, read-only,
//! the pointer is a hand over it (#1916). The chrome (scroll bar, selection) follows the suite's
//! theme; the document is a white page in its authored colours unless the
//! extension turns on dark mode for content ([`DocView::set_content_dark`]),
//! whose dark page the host may paint in its own colour
//! ([`DocView::set_page_background`], #2003). The padding around the
//! document is the host's to set too ([`DocView::set_page_inset`]). Ctrl+F
//! (⌘F on macOS) opens the view's own Find box (#1887, [`find`]): every
//! match highlighted, Enter / Shift+Enter and F3 / Shift+F3 step through them
//! and scroll each into view, Escape closes it.
//!
//! The renderer is the suite's (`block_el`): only its click target differs
//! ([`crate::DocTarget::Embed`]), so a document reads here exactly as in a
//! document tab's Web Layout.

mod edit;
mod find;

pub use edit::{CaretFormat, DocFormat, DocLink};
pub(crate) use find::FindState;

use super::*;
use docxcore::editor::{Caret, Clip, Editor};
use docxcore::package::Package;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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

/// A hyperlink as the view tells a host about it (#1916): under the pointer
/// ([`DocView::on_link_hover`]) or followed ([`DocView::on_link_with_text`]).
/// A host compares the two to warn about a link whose text looks like a
/// different target.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinkHover {
    /// Its target as [`DocView::on_link`] gives it: the URL, `#name` for a
    /// bookmark link, or `target#anchor` for a link with both (a URL and its
    /// fragment, as Word stores it).
    pub url: String,
    /// The whole link's visible text, every run of it (not just the word
    /// under the pointer), read as [`CaretFormat`]'s `link` reads it: a
    /// field's result in place of the field, and a tab or line break in the
    /// link kept as `\t` / `\u{0B}`.
    pub text: String,
}

/// How the view lays the document out (#1888).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocLayout {
    /// The flow (Web Layout) style: the text wraps to the view's width,
    /// with no pages. The default.
    #[default]
    Web,
    /// Print Layout: page sheets on a grey canvas, each with its section's
    /// page size, margins, columns, header and footer, split as a document
    /// tab's Print Layout splits them.
    Print,
}

/// One sheet Print Layout drew in the last frame, for the harness: the
/// header and footer parts it shows, where it painted, and its framed
/// header/footer paragraphs' boxes (`true`: the header's).
#[derive(Default)]
pub(crate) struct SheetProbe {
    header: Option<String>,
    footer: Option<String>,
    bounds: Option<Bounds<Pixels>>,
    frames: Vec<(bool, Bounds<Pixels>)>,
}

type Sheets = Rc<RefCell<Vec<SheetProbe>>>;

type LinkFn = Rc<dyn Fn(&str, &mut Window, &mut App)>;
type LinkTextFn = Rc<dyn Fn(&LinkHover, &mut Window, &mut App)>;
type HoverFn = Rc<dyn Fn(Option<&LinkHover>, &mut Window, &mut App)>;

/// A hovered link's identity (#1916): its paragraph's path and its index in
/// that paragraph's content. Two links to one target are two hovers.
type HoverKey = (Vec<usize>, usize);

/// The link under the pointer (#1916), shared by the view, its overlay
/// canvas and the window's mouse listeners that canvas registers (they run
/// outside the view's update, and the view may be mid-render).
struct LinkHoverState {
    /// The last frame's link pieces: a hitbox per painted piece of a link,
    /// clipped to the view clear of the bars and the Find box.
    boxes: RefCell<Vec<(Hitbox, HoverKey, LinkHover)>>,
    /// The link the host was last told about.
    current: RefCell<Option<(HoverKey, LinkHover)>>,
    /// The pointer is out of the window as far as this view knows: it left
    /// (`MouseExited`) and has not moved in since, or no move has reached
    /// the view yet. gpui keeps its last position after the pointer leaves,
    /// so without this every repaint would find the link it left hovered
    /// again. A new view starts out, as #1869's `pointer_in_window` does: it
    /// cannot know whether the pointer left while it did not exist or was
    /// hidden (r1).
    pointer_out: Cell<bool>,
    on_hover: RefCell<Option<HoverFn>>,
}

impl Default for LinkHoverState {
    fn default() -> Self {
        LinkHoverState {
            boxes: RefCell::default(),
            current: RefCell::default(),
            pointer_out: Cell::new(true),
            on_hover: RefCell::default(),
        }
    }
}

impl LinkHoverState {
    /// The link under the pointer in the last frame's pieces. Not
    /// `Hitbox::is_hovered`, which is false after any keyboard input: a key
    /// would report no link under a still pointer. `should_handle_scroll` is
    /// gpui's hit test without that: it still stops at a `BlockMouse`
    /// overlay (a menu, a dialog), and the one `BlockMouseExceptScroll` over
    /// the view, the Find box, is clipped out of the pieces already.
    fn under_pointer(&self, window: &Window) -> Option<(HoverKey, LinkHover)> {
        if self.pointer_out.get() {
            return None;
        }
        // gpui on Windows sends no `MouseExited` (as `help_pointer`, #1869).
        if cfg!(target_os = "windows") && !harness() && !window.is_window_hovered() {
            return None;
        }
        self.boxes
            .borrow()
            .iter()
            .find(|(b, ..)| b.should_handle_scroll(window))
            .map(|(_, key, link)| (key.clone(), link.clone()))
    }

    /// Look again and tell the host when the link under the pointer changed,
    /// deferred, outside any update.
    fn update(&self, window: &mut Window, cx: &mut App) {
        let now = self.under_pointer(window);
        if *self.current.borrow() == now {
            return;
        }
        self.current.replace(now.clone());
        if let Some(f) = self.on_hover.borrow().clone() {
            window.defer(cx, move |window, cx| {
                f(now.as_ref().map(|(_, link)| link), window, cx)
            });
        }
    }
}
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
    press_link: Option<LinkHover>,
    link_click: LinkClick,
    on_link: Option<LinkFn>,
    on_link_with_text: Option<LinkTextFn>,
    /// The link under the pointer and the last frame's link pieces (#1916).
    hover: Rc<LinkHoverState>,
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
    /// A key or host command moved the caret or edited, or a Find step
    /// went to a match: the next frame scrolls it into view, once it is
    /// painted.
    reveal: Option<Reveal>,
    /// The Find box (#1887).
    find: find::FindState,
    /// The box's parts as last painted, for the harness: field, previous,
    /// next, close.
    find_rects: Rc<Cell<[Option<Bounds<Pixels>>; 4]>>,
    /// How many hit quads the last frame painted in view (the current match
    /// excluded: it is the selection).
    hits_painted: Rc<Cell<usize>>,
    /// The view's scrollbars (#2080), the suite's document bars.
    vbar: crate::doc_scrollbar::Bar,
    hbar: crate::doc_scrollbar::Bar,
    /// Web or Print Layout (#1888).
    layout: DocLayout,
    /// The sheets the last Print Layout frame drew (#1888).
    sheets: Sheets,
    /// The canvas colour the last Print Layout frame painted, for the
    /// harness; `None` in Web Layout.
    painted_canvas: Option<Hsla>,
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
            on_link_with_text: None,
            hover: Rc::default(),
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
            reveal: None,
            find: find::FindState::default(),
            find_rects: Rc::default(),
            hits_painted: Rc::default(),
            vbar: Default::default(),
            hbar: Default::default(),
            layout: DocLayout::default(),
            sheets: Sheets::default(),
            painted_canvas: None,
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
        self.reveal = None;
        // The box stays open with its query; its current match was the old
        // document's.
        self.find.cur = None;
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
    /// that is not `RRGGBB`, a highlight that is not Word's, a link's URL
    /// that is empty or holds a control character, Edit Link or Remove Link
    /// with no link at the caret: [`DocFormat::InsertLink`]). Pressing the
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
    /// see [`CaretFormat`]. Its `link` reads in either mode, as the
    /// read-only view places a caret and selects too; only an editable view
    /// runs the link commands.
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
        self.reveal = Some(Reveal::Caret);
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

    /// Called with a hyperlink's target (its URL, `#name` for a bookmark
    /// link, or `target#anchor` for a link with both, #1916) when the person
    /// follows it. It runs after the press is handled,
    /// outside the view's update, so it may update the view.
    pub fn on_link(&mut self, f: impl Fn(&str, &mut Window, &mut App) + 'static) {
        self.on_link = Some(Rc::new(f));
    }

    /// Called with a followed hyperlink's target and whole visible text
    /// (#1916), after [`DocView::on_link`] when both are set: a host may warn
    /// when the text looks like a different URL from the target.
    pub fn on_link_with_text(&mut self, f: impl Fn(&LinkHover, &mut Window, &mut App) + 'static) {
        self.on_link_with_text = Some(Rc::new(f));
    }

    /// Called when the hyperlink under the pointer changes (#1916): entering
    /// a link or moving onto another one (two links to one target are two),
    /// with the link; leaving every link, the view or the window, with
    /// `None`. Moving between words of one link calls nothing. Content that
    /// moves under a still pointer (a scroll, a zoom, a new document) is
    /// looked at again after its next frame; a key alone changes nothing.
    /// It runs outside the view's update, as `on_link` does. The view only
    /// reports: it draws no tooltip or status of its own. A view that stops
    /// being drawn while a link is under the pointer (taken out of the tree,
    /// or its tab hidden) is not told anything more, so a host clears its
    /// own status when it hides the view. A view reports nothing until the
    /// pointer first moves over the window after it is shown the first time,
    /// so a view that first appears under a still pointer reports that link
    /// at the first move. A view hidden, then shown again with no move in
    /// between, cannot tell whether the pointer left the window meanwhile,
    /// and may report a link at the pointer's last place. On Windows gpui
    /// sends no event when the pointer leaves the window, so leaving it
    /// straight from a link reports `None` at the next mouse event or frame.
    pub fn on_link_hover(
        &mut self,
        f: impl Fn(Option<&LinkHover>, &mut Window, &mut App) + 'static,
    ) {
        self.hover.on_hover.replace(Some(Rc::new(f)));
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

    /// Lay the document out as Web Layout (the default) or Print Layout
    /// (#1888): page sheets with each section's own page size, margins,
    /// columns and header/footer, as a document tab's Print Layout draws
    /// them. Selection, copy, zoom, Find, the editing mode and the link
    /// callback work the same in both; the headers and footers are drawn
    /// only (not selected, not followed). The selection and zoom stay, and
    /// the caret is scrolled into view. [`DocView::set_page_inset`] pads
    /// Web Layout only: Print Layout's sheets sit on its own canvas.
    pub fn set_layout(&mut self, layout: DocLayout, cx: &mut Context<Self>) {
        if set_if_changed(&mut self.layout, layout) {
            self.reveal = Some(Reveal::Caret);
            cx.notify();
        }
    }

    /// The layout [`DocView::set_layout`] set.
    pub fn layout(&self) -> DocLayout {
        self.layout
    }

    /// How many pages the document has in Print Layout, in either layout,
    /// so a host can show "N pages" whichever it draws: the same split of
    /// the live document (all its markup) the sheets use.
    pub fn page_count(&self) -> usize {
        print_sheet::page_count(&self.editor, self.pkg.as_ref())
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
        self.find.field_focused = false;
        // Only a press on a painted piece is on a link: beside a row the
        // offset is the row's nearest stop, which may be a link's first or
        // last character the person never pressed.
        let link = on_piece
            .then(|| docxcore::editor::resolve_para(&self.editor.doc.body, &path))
            .flatten()
            .and_then(|p| link_hover_at(p, off))
            .map(|(_, link)| link);
        // Ctrl+click (⌘ on macOS) follows a link at once, in either mode, and
        // selects nothing.
        if mods.secondary() && !mods.shift {
            if let Some(link) = link {
                self.selecting = false;
                self.press_link = None;
                self.follow(link, window, cx);
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
        self.find.field_focused = false;
        place_click_caret(&mut self.editor, Caret::at(path, off), extend, false);
        self.press_link = None;
        cx.notify();
    }

    fn follow(&self, link: LinkHover, window: &mut Window, cx: &mut App) {
        let (on_link, with_text) = (self.on_link.clone(), self.on_link_with_text.clone());
        if on_link.is_some() || with_text.is_some() {
            window.defer(cx, move |window, cx| {
                if let Some(f) = on_link {
                    f(&link.url, window, cx);
                }
                if let Some(f) = with_text {
                    f(&link, window, cx);
                }
            });
        }
    }

    /// The release that ends a press: in [`LinkClick::Click`] mode a plain
    /// press on a link that dragged nothing follows it.
    fn release(&mut self, mods: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Some(link) = self.press_link.take() {
            if self.link_click == LinkClick::Click
                && !mods.modified()
                && !self.editor.has_selection()
            {
                self.follow(link, window, cx);
            }
        }
    }

    // ---- keys, wheel ------------------------------------------------------

    fn key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Find first (#1887): while its field has the keys, no key reaches
        // the editing map.
        match find::find_key(
            &ev.keystroke,
            text_input::MACOS,
            self.find.open,
            self.find.field_focused,
        ) {
            find::FindKey::Pass => {}
            find::FindKey::Bubble => return,
            find::FindKey::View => {
                if self.view_key(ev, cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            k => {
                self.find_act(k, cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        if self.editable && self.edit_key(ev, cx) {
            cx.stop_propagation();
            return;
        }
        if self.view_key(ev, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The read-only view's keys: copy, select all, scroll and zoom.
    /// Whether it took the key.
    fn view_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        if m.secondary() && !m.alt && !m.function {
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
        }
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
        // The Find box's field has the keys (#1887): a Tab is no tab typed
        // into the document, in either mode.
        if self.find.open && self.find.field_focused {
            return;
        }
        if !self.editable {
            cx.propagate();
            return;
        }
        self.seed_styles();
        let out = edit::tab(&mut self.editor, self.pkg.as_mut(), back);
        self.after_edit(out.changed(), cx);
    }

    // ---- Find (#1887) -----------------------------------------------------

    /// A key [`find::find_key`] gave Find.
    fn find_act(&mut self, k: find::FindKey, cx: &mut Context<Self>) {
        use crate::field_edit::Op;
        use find::FindKey as K;
        match k {
            K::Open => self.open_find(),
            K::Close => self.close_find(),
            K::Step { back } => self.find_step(back, false),
            K::Field(op) => self.find_edit(op.op()),
            K::Type(text) => self.find_edit(Op::Insert(&text)),
            K::Copy | K::Cut => {
                if let Some(text) = self.find.selected_text().map(str::to_owned) {
                    self.clipboard.write(harness(), text, |text| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text))
                    });
                    if k == K::Cut {
                        self.find_edit(Op::Delete);
                    }
                }
            }
            K::Paste => {
                let read = self.clipboard.read(harness(), || {
                    cx.read_from_clipboard().map(|item| item.text())
                });
                if let ClipRead::Text(text) = read {
                    let line = crate::field_edit::one_line(&text);
                    if !line.is_empty() {
                        self.find_edit(Op::Insert(&line));
                    }
                }
            }
            K::Pass | K::View | K::Swallow | K::Bubble => {}
        }
    }

    /// One edit of the box's field: a changed query finds again from the
    /// top, as the document tab's find bar does while typing.
    fn find_edit(&mut self, op: crate::field_edit::Op) {
        if self.find.edit(op) {
            self.find_step(false, true);
        }
    }

    /// Ctrl+F: open the box, seeded with a one-line selection (as the
    /// document tab's Find), on the match the editor is on if it is on one
    /// (a reopen after Escape keeps it), else the next from the caret. With
    /// the box open its field takes the keys again. Either way the field's
    /// text is selected, so typing replaces it.
    fn open_find(&mut self) {
        if !self.find.open {
            self.find.open = true;
            self.find.cur = None;
            let sel = self.editor.selection_text();
            if !sel.is_empty() && !sel.contains('\n') {
                self.find.query = sel;
            }
            let matches = self.editor.find_visible(&self.find.query, false);
            match matches.iter().position(|m| self.editor.is_at_found(m)) {
                Some(i) => self.find.cur = Some((i, matches[i].clone())),
                None => self.find_step(false, false),
            }
        }
        self.find.focus_select_all();
    }

    /// Escape or the box's ×: the box closes; the match stays selected and
    /// the query is kept for the next Ctrl+F.
    fn close_find(&mut self) {
        self.find.open = false;
        self.find.field_focused = false;
    }

    /// Go to the next (`back`: previous) match, wrapping, and scroll it into
    /// view; `from_start` searches from the top (typing). The document tab's
    /// step ([`find_step_in`]). An empty query or one with no match changes
    /// nothing in the document.
    fn find_step(&mut self, back: bool, from_start: bool) {
        let q = self.find.query.clone();
        if q.is_empty() || self.editor.find_visible(&q, false).is_empty() {
            self.find.cur = None;
            return;
        }
        let cur = self.find.cur.take();
        self.find.cur = find_step_in(&mut self.editor, &q, false, cur, back, from_start);
        if let Some((_, m)) = &self.find.cur {
            self.reveal = Some(Reveal::Match(Caret::at(m.path.clone(), m.start)));
        }
    }

    /// The matches the open box shows (none while closed or empty).
    fn find_matches(&self) -> Vec<docxcore::editor::FoundMatch> {
        if !self.find.open || self.find.query.is_empty() {
            return Vec::new();
        }
        self.editor.find_visible(&self.find.query, false)
    }

    /// A press on the box: it never reaches the document. On the field it
    /// takes the keys with the caret at the end; on ↑ / ↓ it steps, on ×
    /// it closes.
    fn find_press(&mut self, part: FindPart, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.focus.focus(window, cx);
        match part {
            FindPart::Field => self.find.focus_at_end(),
            FindPart::Prev => self.find_step(true, false),
            FindPart::Next => self.find_step(false, false),
            FindPart::Close => self.close_find(),
            FindPart::Box => {}
        }
        cx.notify();
    }

    /// The box's state, for the harness's `field-read`.
    pub(crate) fn find_state(&self) -> &find::FindState {
        &self.find
    }

    /// A part of the open box as last painted: `field`, `prev`, `next` or
    /// `close` (the harness's `ext-doc-find:<part>`).
    pub(crate) fn find_region(&self, part: &str) -> Result<Bounds<Pixels>, String> {
        let i = match part {
            "field" => 0,
            "prev" => 1,
            "next" => 2,
            "close" => 3,
            _ => {
                return Err(format!(
                    "unknown Find box part '{part}': field, prev, next or close"
                ));
            }
        };
        if !self.find.open {
            return Err("the Find box is not open: press Ctrl+F in the view".into());
        }
        self.find_rects.get()[i].ok_or_else(|| "the Find box is not drawn yet".into())
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
            // The dump is the flow view's: Print Layout paints none (#1888).
            Some(col)
                if self.layout == DocLayout::Web
                    && self.painted_gen.get() == Some(self.doc_gen) =>
            {
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
        // The flow's text column is Web Layout's alone (#1888).
        let web = self.layout == DocLayout::Web;
        let text_w = self.text_w.get().filter(|_| web);
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
                text_w.map_or(Json::Null, |w| Json::Num(f64::from(w.round()))),
            ),
            (
                "table_widths",
                painted_tables_json(
                    &self.tables,
                    text_w,
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
            // The link under the pointer as the host was last told (#1916),
            // and whether the view asks for the hand there: gpui gives no
            // way to read the platform's cursor back, so this is the view's
            // own request (read-only, a link piece under the pointer).
            (
                "hovered",
                self.hover
                    .current
                    .borrow()
                    .as_ref()
                    .map_or(Json::Null, |(_, link)| link_hover_json(link)),
            ),
            (
                "link_hand",
                Json::Bool(!self.editable && self.hover.under_pointer(window).is_some()),
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
                match (self.column.get().filter(|_| web), self.viewport.get()) {
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
                    // Clear of the bars and the open Find box, as the
                    // reveal is.
                    (Some(piece), Some(view)) => {
                        Json::Bool(reveal_dy(self.find_view(view), piece) == px(0.))
                    }
                    _ => Json::Null,
                },
            ),
            ("caret_format", caret_format_json(&self.caret_format())),
            ("can_undo", Json::Bool(self.can_undo())),
            ("can_redo", Json::Bool(self.can_redo())),
            ("can_copy", Json::Bool(self.can_copy())),
            ("find", self.find_json()),
            (
                "layout",
                Json::Str(
                    match self.layout {
                        DocLayout::Print => "print",
                        _ => "web",
                    }
                    .into(),
                ),
            ),
            ("page_count", Json::Num(self.page_count() as f64)),
            ("pages", self.pages_json()),
            (
                "canvas_color",
                self.painted_canvas
                    .map_or(Json::Null, |c| Json::Str(ink_hex(c))),
            ),
        ])
    }

    /// The sheets the last frame painted (#1888), in view-relative px:
    /// `page` (1-based), `x`, `y`, `w`, `h`, the `header` and `footer`
    /// parts shown (`null`: none) and `frames`, each framed header/footer
    /// paragraph's box relative to its sheet (`header`: whether the
    /// header's; `right`: its right edge). Empty in Web Layout.
    fn pages_json(&self) -> ctlcore::json::Json {
        use ctlcore::json::Json;
        let Some(view) = self.viewport.get() else {
            return Json::Arr(Vec::new());
        };
        let n = |v: Pixels| Json::Num(f64::from(f32::from(v).round()));
        let name = |s: &Option<String>| s.clone().map_or(Json::Null, Json::Str);
        let sheets = self.sheets.borrow();
        let pages = sheets
            .iter()
            .enumerate()
            .filter_map(|(pi, s)| {
                let b = s.bounds?;
                let frames = s
                    .frames
                    .iter()
                    .map(|(is_h, f)| {
                        Json::obj(vec![
                            ("header", Json::Bool(*is_h)),
                            ("x", n(f.origin.x - b.origin.x)),
                            ("y", n(f.origin.y - b.origin.y)),
                            ("w", n(f.size.width)),
                            ("h", n(f.size.height)),
                            ("right", n(f.right() - b.origin.x)),
                        ])
                    })
                    .collect();
                Some(Json::obj(vec![
                    ("page", Json::Num((pi + 1) as f64)),
                    ("x", n(b.origin.x - view.origin.x)),
                    ("y", n(b.origin.y - view.origin.y)),
                    ("w", n(b.size.width)),
                    ("h", n(b.size.height)),
                    ("header", name(&s.header)),
                    ("footer", name(&s.footer)),
                    ("frames", Json::Arr(frames)),
                ]))
            })
            .collect();
        Json::Arr(pages)
    }

    /// The first painted word whose text, trimmed, is `text` (#1888): its
    /// box in view-relative px, and in Print Layout the sheet it is on
    /// (`page`, 1-based) and its box relative to that sheet (`page_x`,
    /// `page_y`); those are `null` in Web Layout.
    pub(crate) fn word_json(&self, text: &str) -> Result<ctlcore::json::Json, String> {
        use ctlcore::json::Json;
        let (b, _) = self
            .word_bounds(text)
            .ok_or_else(|| format!("no painted word '{text}'"))?;
        let view = self.viewport.get().ok_or("the view is not drawn yet")?;
        let n = |v: Pixels| Json::Num(f64::from(f32::from(v).round()));
        let sheets = self.sheets.borrow();
        let sheet = sheets.iter().enumerate().find_map(|(pi, s)| {
            s.bounds
                .filter(|sb| sb.contains(&b.center()))
                .map(|sb| (pi, sb))
        });
        let (page, page_x, page_y) = match sheet {
            Some((pi, sb)) => (
                Json::Num((pi + 1) as f64),
                n(b.origin.x - sb.origin.x),
                n(b.origin.y - sb.origin.y),
            ),
            None => (Json::Null, Json::Null, Json::Null),
        };
        Ok(Json::obj(vec![
            ("x", n(b.origin.x - view.origin.x)),
            ("y", n(b.origin.y - view.origin.y)),
            ("w", n(b.size.width)),
            ("h", n(b.size.height)),
            ("page", page),
            ("page_x", page_x),
            ("page_y", page_y),
        ]))
    }

    /// The part of the painted `view` a match or the caret is in view in
    /// ([`find::find_view`]).
    fn find_view(&self, view: Bounds<Pixels>) -> Bounds<Pixels> {
        find::find_view(view, self.hbar.shown(), self.vbar.shown(), self.find.open)
    }

    /// `ext-doc-read`'s `find` (#1887): the box, its query, how many
    /// matches and which is current (1-based), whether that one is in view
    /// in both axes, and how many hits the last frame painted in view.
    fn find_json(&self) -> ctlcore::json::Json {
        use ctlcore::json::Json;
        let matches = self.find_matches();
        let current = self.find.current(&self.editor, &matches);
        let m = current.map(|i| &matches[i]);
        let in_view = m.and_then(|m| {
            let piece = caret_piece(&self.words, &Caret::at(m.path.clone(), m.start))?;
            let view = self.find_view(self.viewport.get()?);
            Some(reveal_dy(view, piece) == px(0.) && find::reveal_dx(view, piece) == px(0.))
        });
        Json::obj(vec![
            ("open", Json::Bool(self.find.open)),
            ("query", Json::Str(self.find.query.clone())),
            ("field_focused", Json::Bool(self.find.field_focused)),
            ("count", Json::Num(matches.len() as f64)),
            (
                "current",
                current.map_or(Json::Null, |i| Json::Num((i + 1) as f64)),
            ),
            (
                "current_path",
                m.map_or(Json::Null, |m| {
                    Json::Arr(m.path.iter().map(|&i| Json::Num(i as f64)).collect())
                }),
            ),
            ("current_in_view", in_view.map_or(Json::Null, Json::Bool)),
            ("hits_painted", Json::Num(self.hits_painted.get() as f64)),
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
        // Editable, the caret draws while the view has the keys (#1947),
        // not while the Find box's field has them (#1887).
        let view_focused = self.focus.is_focused(window);
        let field_keys = self.find.open && self.find.field_focused;
        let caret: &[usize] = if self.editable && view_focused && !field_keys {
            &self.editor.caret.path
        } else {
            &[]
        };
        // A caret reveals in y only (#1895); a Find match sideways too.
        let reveal = self.reveal.take().map(|r| match r {
            Reveal::Caret => (self.editor.caret.clone(), false),
            Reveal::Match(at) => (at, true),
        });
        // The open box's matches: the current one is the selection, the
        // others paint over the text (#1887).
        let matches = self.find_matches();
        let current = self.find.current(&self.editor, &matches);
        let hits: Vec<(Vec<usize>, usize, usize)> = matches
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != current)
            .map(|(_, m)| (m.path.clone(), m.start, m.end))
            .collect();
        let find_box = self
            .find
            .open
            .then(|| self.find_box(matches.len(), current, view_focused, cx));
        if !self.find.open {
            self.find_rects.set([None; 4]);
        }
        let box_open = self.find.open;
        let hits_painted = self.hits_painted.clone();
        hits_painted.set(0);
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
        let scroll_area = match self.layout {
            DocLayout::Print => {
                // The sheets on the canvas, centred in a column at least as
                // wide as the view (a sheet wider than the view starts at
                // the left, #2080), as a document tab's Print Layout.
                let canvas = hsla_u(print_sheet::canvas_color(
                    self.content_dark && theme_dark,
                    theme_dark,
                ));
                self.painted_canvas = Some(canvas);
                let sheets = print_sheets(
                    &self.editor,
                    self.pkg.as_ref(),
                    &markers,
                    ctx,
                    bg,
                    pics.as_ref(),
                    &self.sheets,
                );
                v_flex()
                    .id("ext-doc-scroll")
                    .track_scroll(&self.scroll)
                    .size_full()
                    .overflow_scroll()
                    .bg(canvas)
                    .text_color(pal.fg)
                    .items_start()
                    .py(px(print_sheet::CANVAS_PAD))
                    .child(
                        v_flex()
                            .flex_none()
                            .min_w_full()
                            .items_center()
                            .gap(px(print_sheet::SHEET_GAP))
                            .children(sheets),
                    )
            }
            DocLayout::Web => {
                self.painted_canvas = None;
                self.sheets.borrow_mut().clear();
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
                let (used_w, text_w) = (self.text_w.get(), self.text_w.clone());
                let (column, painted_gen, doc_gen) =
                    (self.column.clone(), self.painted_gen.clone(), self.doc_gen);
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
                    )))
            }
        };
        let viewport = self.viewport.clone();
        let (words, scroll) = (self.words.clone(), self.scroll.clone());
        let (hbar, vbar) = (self.hbar.clone(), self.vbar.clone());
        // The links of each paragraph this frame draws (#1916): the overlay
        // canvas gives each painted piece of one a hitbox, for the hand and
        // the link under the pointer.
        let mut links: HashMap<Vec<usize>, ParaLinks> = HashMap::new();
        for (path, _) in self.words.borrow().iter() {
            if links.contains_key(path) {
                continue;
            }
            if let Some(p) = docxcore::editor::resolve_para(body, path) {
                let found = para_links(p);
                if !found.is_empty() {
                    links.insert(path.clone(), found);
                }
            }
        }
        let (hover, hover_boxes) = (self.hover.clone(), self.hover.clone());
        let words_p = self.words.clone();
        let (hbar_p, vbar_p) = (self.hbar.clone(), self.vbar.clone());
        let hand = !self.editable;
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
                    // A press in the document takes the keys from the Find
                    // box's field; the box stays (#1887).
                    if this.find.field_focused {
                        this.find.field_focused = false;
                        cx.notify();
                    }
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
            .child(scroll_area)
            .child(
                canvas(
                    move |b: Bounds<Pixels>, window: &mut Window, _a: &mut App| {
                        viewport.set(Some(b));
                        // `ext-doc-scroll` has prepainted, so every piece
                        // has its bounds (#1916).
                        let clip = find::find_view(b, hbar_p.shown(), vbar_p.shown(), box_open);
                        hover_boxes
                            .boxes
                            .replace(link_hitboxes(&words_p, &links, clip, window));
                    },
                    // It paints after `ext-doc-scroll`, whose pieces this
                    // frame has drawn by then: a paragraph an Enter just
                    // made among them.
                    move |b, _s, window, cx| {
                        // Read-only, the pointer is a hand over a link: asked
                        // after the words' I-beam and before the bars and the
                        // Find box, so it wins over the one and not the
                        // others (#1916). Editable, a press there places the
                        // caret, so no hand.
                        if hand {
                            for (hitbox, ..) in hover.boxes.borrow().iter() {
                                window.set_cursor_style(CursorStyle::PointingHand, hitbox);
                            }
                        }
                        link_hover_listeners(&hover, window);
                        // Content moved under a still pointer (a scroll, a
                        // zoom, a new document) shows here.
                        hover.update(window, cx);
                        // Clear of the scroll bars (#2080) and the open Find
                        // box (#1887).
                        let view = find::find_view(b, hbar.shown(), vbar.shown(), box_open);
                        if let Some((at, sideways)) = reveal {
                            if reveal_caret(&words, &scroll, view, &at, sideways) {
                                window.request_animation_frame();
                            }
                        }
                        if !hits.is_empty() {
                            // Hits paint over the whole view clear of the
                            // bars, the box's band included: the box is drawn
                            // after this canvas and covers what is under it.
                            let clip = find::find_view(b, hbar.shown(), vbar.shown(), false);
                            let quads = find::visible_hits(painted_hits(&words, &hits), clip);
                            hits_painted.set(quads.len());
                            window.with_content_mask(
                                Some(ContentMask { bounds: clip }),
                                |window| {
                                    for q in quads {
                                        window.paint_quad(fill(q, find::hit_color()));
                                    }
                                },
                            );
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
            // The Find box (#1887), over the bars at the top right.
            .children(find_box)
    }
}

impl DocView {
    /// The Find box: its field, the count, ↑ / ↓ and ×, in the theme's
    /// chrome (not the page's palette), at the view's top right. A press on
    /// it never reaches the document; each part records where it painted
    /// for the harness.
    fn find_box(
        &self,
        count: usize,
        current: Option<usize>,
        view_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pal = Pal::of(cx);
        let field_bg = cx.theme().background;
        let rects = self.find_rects.clone();
        let rec = move |i: usize| {
            let rects = rects.clone();
            canvas(
                move |b: Bounds<Pixels>, _w: &mut Window, _a: &mut App| {
                    let mut r = rects.get();
                    r[i] = Some(b);
                    rects.set(r);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        let press = |part: FindPart| {
            cx.listener(move |this: &mut Self, _: &MouseDownEvent, window, cx| {
                this.find_press(part, window, cx)
            })
        };
        let f = &self.find;
        let focused = f.field_focused && view_focused;
        let len = f.query.chars().count();
        let at = char_to_byte(&f.query, f.caret.min(len));
        let text = div()
            .flex()
            .items_center()
            .text_size(px(13.))
            .text_color(pal.fg)
            .when(f.sel && len > 0, |d| d.bg(hsla_u(BRAND).opacity(0.25)))
            .child(SharedString::from(f.query[..at].to_string()))
            .when(focused, |d| d.child(caret_bar()))
            .child(SharedString::from(f.query[at..].to_string()));
        let field = div()
            .id("ext-doc-find-field")
            .relative()
            .child(rec(0))
            .flex()
            .items_center()
            .h(px(22.))
            .w(px(180.))
            .px_1p5()
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(if focused { hsla_u(BRAND) } else { pal.border })
            .bg(field_bg)
            .cursor_text()
            .when(f.query.is_empty() && !focused, |d| {
                d.child(div().text_size(px(13.)).text_color(pal.dim).child("Find"))
            })
            .child(text)
            .on_mouse_down(MouseButton::Left, press(FindPart::Field));
        let button = |id: &'static str, glyph: &'static str, i: usize, part: FindPart| {
            div()
                .id(id)
                .relative()
                .child(rec(i))
                .flex()
                .items_center()
                .justify_center()
                .size(px(22.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(14.))
                .text_color(pal.fg)
                .hover(|d| d.bg(pal.hover))
                .child(glyph)
                .on_mouse_down(MouseButton::Left, press(part))
        };
        h_flex()
            .id("ext-doc-find")
            .absolute()
            .top(px(find::BOX_TOP))
            .right(px(find::BOX_RIGHT))
            .h(px(find::BOX_H))
            .items_center()
            .gap_1()
            .px_1()
            .rounded(px(6.))
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .shadow_md()
            // The document under it takes no press, but the wheel still
            // scrolls it.
            .block_mouse_except_scroll()
            .on_mouse_down(MouseButton::Left, press(FindPart::Box))
            .child(field)
            .child(
                div()
                    .min_w(px(68.))
                    .text_size(px(11.))
                    .text_color(pal.dim)
                    .child(SharedString::from(find::count_text(
                        &f.query, count, current,
                    ))),
            )
            .child(button("ext-doc-find-prev", "\u{2191}", 1, FindPart::Prev))
            .child(button("ext-doc-find-next", "\u{2193}", 2, FindPart::Next))
            .child(button("ext-doc-find-close", "\u{00d7}", 3, FindPart::Close))
            .into_any_element()
    }
}

/// Print Layout's sheets of `editor`'s document (#1888), read-only around
/// the body: each page a sheet of its section's size (`page_bg`, a 1 px
/// border), its header in the top margin at the header distance, its
/// footer in the bottom margin, the body's bands between with their own
/// section's left/right margins and columns (`print_sheet::band_el`, as a
/// document tab draws them) and the header and footer's framed paragraphs
/// at their frame. The body draws through `ctx` (the view's click target,
/// so selection, links and Find work on the pages); the headers and footers
/// are drawn only. Each sheet records into `probe` what it shows and where
/// it painted.
fn print_sheets(
    editor: &Editor,
    pkg: Option<&Package>,
    markers: &[Option<String>],
    ctx: RenderCtx,
    page_bg: Hsla,
    pics: Option<&pictures::Pictures>,
    probe: &Sheets,
) -> Vec<AnyElement> {
    let plan = print_sheet::page_plan(editor, pkg);
    let sections = editor.sections();
    let body = &editor.doc.body;
    let (zoom, pal, meas) = (ctx.zoom, ctx.pal, ctx.meas);
    let sheet = ctx.tbl.sheet.as_deref();
    let tw = move |t: i32| px(zoom * (t.max(0) as f32) / 15.0);
    // A page's column fits its tables and lays its paragraphs out as a
    // document tab's Print Layout does: no flow spacing, no fitted width.
    let band_ctx = RenderCtx {
        fit_width: None,
        flow: false,
        gap_below: false,
        paras: None,
        ..ctx
    };
    // The header/footer distance of each page's section.
    let dist = |pi: usize, is_h: bool| -> i32 {
        plan.slots
            .get(pi)
            .and_then(|s| sections.get(s.section))
            .map_or(720, |sect| hf::distance(sect, is_h))
    };
    *probe.borrow_mut() = (0..plan.flow.pages.len())
        .map(|pi| SheetProbe {
            header: plan.part(pi, true).map(str::to_string),
            footer: plan.part(pi, false).map(str::to_string),
            ..SheetProbe::default()
        })
        .collect();
    plan.flow
        .pages
        .iter()
        .enumerate()
        .map(|(pi, page)| {
            // The page's own section owns the sheet: its size, top/bottom
            // margins and header/footer text width; every band draws with
            // its own section's left/right margins and columns.
            let sb = &plan.flow.sections[page.bands[0].section];
            let hf_w = zoom * (sb.w - sb.left - sb.right).max(0) as f32 / 15.0;
            let vals = plan.page_vals.get(pi);
            let region = |is_h: bool| {
                crate::hf_els(
                    plan.blocks(pi, is_h),
                    pal,
                    meas,
                    hf_w,
                    sheet,
                    pictures::scope(pics, plan.part(pi, is_h)),
                    vals,
                )
            };
            let bands: Vec<AnyElement> = page
                .bands
                .iter()
                .map(|band| {
                    let bsect = &plan.flow.sections[band.section];
                    let inner = print_sheet::band_el(
                        band,
                        bsect,
                        body,
                        markers,
                        band_ctx,
                        zoom,
                        pal.dim,
                        &|_| None,
                    );
                    div()
                        .w_full()
                        .pl(tw(bsect.left))
                        .pr(tw(bsect.right))
                        .child(div().relative().w_full().child(inner))
                        .into_any_element()
                })
                .collect();
            let header = div()
                .relative()
                .min_h(tw(sb.top))
                .pt(tw(dist(pi, true)))
                .pl(tw(sb.left))
                .pr(tw(sb.right))
                .children(region(true));
            let footer = v_flex()
                .relative()
                .min_h(tw(sb.bottom))
                .justify_end()
                .pb(tw(dist(pi, false)))
                .pl(tw(sb.left))
                .pr(tw(sb.right))
                .children(region(false));
            let content = div().relative().w_full().flex_1().children(bands);
            let sheet_el = v_flex()
                .w(tw(sb.w))
                .min_h(tw(sb.h))
                .bg(page_bg)
                .text_color(pal.fg)
                .border_1()
                .border_color(hsla_u(print_sheet::SHEET_BORDER))
                .child(header)
                .child(v_flex().flex_1().child(content))
                .child(footer);
            // The header and footer's framed paragraphs (a page number in
            // the margin, #767) at their frame on the sheet.
            let mut frames: Vec<AnyElement> = Vec::new();
            for is_h in [true, false] {
                let blocks = plan.blocks(pi, is_h);
                let (flow, groups) = docxcore::frame::split_framed(blocks);
                if groups.is_empty() {
                    continue;
                }
                let geom = print_sheet::frame_geom(
                    sb,
                    is_h,
                    dist(pi, is_h),
                    &flow,
                    hf_w,
                    zoom,
                    page.number % 2 == 0,
                );
                for g in &groups {
                    let r = docxcore::frame::frame_rect(
                        g.frame,
                        &geom,
                        frame_content_w(g, meas, zoom, sheet, vals),
                        g.content_h(),
                    );
                    let width = zoom * r.w.max(0) as f32 / 15.0;
                    let paras = g.paras.iter().map(|&(_, p)| {
                        hf_para_el(
                            p,
                            pal,
                            meas,
                            width,
                            zoom,
                            sheet,
                            pictures::scope(pics, plan.part(pi, is_h)),
                            vals,
                        )
                    });
                    let exact = g.frame.h_rule.as_deref() == Some("exact");
                    let probe = probe.clone();
                    frames.push(
                        v_flex()
                            .absolute()
                            .left(px(zoom * r.x as f32 / 15.0))
                            .top(px(zoom * r.y as f32 / 15.0))
                            .w(px(width))
                            .when(exact, |d| d.h(tw(r.h)))
                            .when(!exact, |d| d.min_h(tw(r.h)))
                            .text_color(pal.fg)
                            .children(paras)
                            .child(
                                canvas(
                                    move |b: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                                        if let Some(s) = probe.borrow_mut().get_mut(pi) {
                                            s.frames.push((is_h, b));
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                            .into_any_element(),
                    );
                }
            }
            let probe = probe.clone();
            div()
                .relative()
                .w(tw(sb.w))
                .child(sheet_el)
                .children(frames)
                .child(
                    canvas(
                        move |b: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                            if let Some(s) = probe.borrow_mut().get_mut(pi) {
                                s.bounds = Some(b);
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .into_any_element()
        })
        .collect()
}

/// What a scroll-into-view after a render goes to: the caret (a key or a
/// host command), or a Find match's start (#1887), shown sideways too.
#[derive(Clone, Debug)]
enum Reveal {
    Caret,
    Match(Caret),
}

/// A part of the Find box a press landed on (#1887).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FindPart {
    Box,
    Field,
    Prev,
    Next,
    Close,
}

/// The quads highlighting `hits` (`path`, `start`, `end`) over this frame's
/// painted pieces ([`find::hit_quads`]); a hit whose paragraph painted
/// nothing has none.
fn painted_hits(words: &Words, hits: &[(Vec<usize>, usize, usize)]) -> Vec<Bounds<Pixels>> {
    let words = words.borrow();
    let mut out = Vec::new();
    for (path, start, end) in hits {
        let Some((_, spans)) = words.iter().find(|(p, _)| p == path) else {
            continue;
        };
        let spans = spans.borrow();
        let pieces: Vec<find::HitPiece> = spans
            .iter()
            .map(|w| find::HitPiece {
                off: w.off,
                len: w.len,
                bounds: w.bounds(),
                unit: matches!(w.shape, SpanShape::Unit(_)),
            })
            .collect();
        out.extend(find::hit_quads(
            &pieces,
            *start,
            *end,
            |i, c| match &spans[i].shape {
                SpanShape::Text(layout) => {
                    let text = layout.text();
                    let at = find::char_byte(&text, c).min(layout.len());
                    layout.position_for_index(at).map(|p| p.x)
                }
                SpanShape::Unit(_) => None,
            },
        ));
    }
    out
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
        (
            "link",
            f.link.as_ref().map_or(Json::Null, |l| {
                Json::obj(vec![
                    ("text", Json::Str(l.text.clone())),
                    ("url", Json::Str(l.url.clone())),
                ])
            }),
        ),
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
/// in y only (a wide table keeps its sideways scroll, #1895) unless
/// `sideways` (a Find match, #1887, shown in x too); a caret whose
/// paragraph painted nothing has its top-level block followed instead, as a
/// document tab's Web Layout follows it. Whether it moved (shown next frame).
fn reveal_caret(
    words: &Words,
    scroll: &ScrollHandle,
    view: Bounds<Pixels>,
    caret: &Caret,
    sideways: bool,
) -> bool {
    let Some(piece) = caret_piece(words, caret) else {
        return caret
            .path
            .first()
            .is_some_and(|&b| follow_item_y(scroll, b, view));
    };
    let dy = reveal_dy(view, piece);
    let dx = if sideways {
        find::reveal_dx(view, piece)
    } else {
        px(0.)
    };
    if dy == px(0.) && dx == px(0.) {
        return false;
    }
    let offset = scroll.offset();
    let max = scroll.max_offset().max(&point(px(0.), px(0.)));
    let to = point(
        (offset.x + dx).clamp(-max.x, px(0.)),
        (offset.y + dy).clamp(-max.y, px(0.)),
    );
    if to == offset {
        return false;
    }
    scroll.set_offset(to);
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
/// `#` and its bookmark, or `target#anchor` for a link with both (#1916);
/// `None` outside a link (or a link with neither).
pub(crate) fn link_at(p: &Paragraph, off: usize) -> Option<String> {
    link_hover_at(p, off).map(|(_, link)| link.url)
}

/// The top-level hyperlink holding caret offset `off` of `p` (#1916): its
/// index in `p.content` and what a host is told about it. `None` outside a
/// link, or for a link with neither a target nor a bookmark.
pub(crate) fn link_hover_at(p: &Paragraph, off: usize) -> Option<(usize, LinkHover)> {
    para_links(p)
        .into_iter()
        .find(|(range, ..)| range.contains(&off))
        .map(|(_, i, link)| (i, link))
}

/// A link as the harness reads it (#1916): `{url, text}`.
pub(crate) fn link_hover_json(link: &LinkHover) -> ctlcore::json::Json {
    use ctlcore::json::Json;
    Json::obj(vec![
        ("url", Json::Str(link.url.clone())),
        ("text", Json::Str(link.text.clone())),
    ])
}

/// A painted paragraph's links: caret offsets, index in its content, and
/// what a host is told.
type ParaLinks = Vec<(std::ops::Range<usize>, usize, LinkHover)>;

/// A hitbox for each painted piece (word or unit) of a link (#1916), clipped
/// to `clip` (the view clear of the bars and the Find box); a piece out of
/// view gets none. Per piece, not the link's merged span: a wrapped link's
/// union would cover the blank beside its rows. `Normal` hitboxes block
/// nothing, so the words' own press and move handling is unchanged. Only
/// in prepaint.
fn link_hitboxes(
    words: &Words,
    links: &HashMap<Vec<usize>, ParaLinks>,
    clip: Bounds<Pixels>,
    window: &mut Window,
) -> Vec<(Hitbox, HoverKey, LinkHover)> {
    let mut out = Vec::new();
    if links.is_empty() {
        return out;
    }
    for (path, spans) in words.borrow().iter() {
        let Some(found) = links.get(path) else {
            continue;
        };
        for w in spans.borrow().iter().filter(|w| w.len > 0) {
            let Some((_, i, link)) = found.iter().find(|(range, ..)| range.contains(&w.off)) else {
                continue;
            };
            let piece = w.bounds().intersect(&clip);
            if piece.size.width > px(0.) && piece.size.height > px(0.) {
                let hitbox = window.insert_hitbox(piece, HitboxBehavior::Normal);
                out.push((hitbox, (path.clone(), *i), link.clone()));
            }
        }
    }
    out
}

/// The window's mouse listeners that keep the link under the pointer
/// (#1916), registered each frame in paint. Capture phase: a word's own move
/// handler may stop a move's bubble during a drag. They never stop an event
/// themselves; `main.rs`'s gesture canvas registers earlier, so its capture
/// listeners run first and a table pen's could stop a move before it gets
/// here, which only delays the report to the next frame.
fn link_hover_listeners(hover: &Rc<LinkHoverState>, window: &mut Window) {
    let moved = hover.clone();
    window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            moved.pointer_out.set(false);
            moved.update(window, cx);
        }
    });
    let left = hover.clone();
    window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            left.pointer_out.set(true);
            left.update(window, cx);
        }
    });
}

/// Each top-level hyperlink of `p` with a target or a bookmark: its caret
/// offsets, its index in `p.content` and what a host is told about it
/// (docxcore's [`docxcore::editor::hyperlink_shown`], as the caret's link
/// reads, #1916).
fn para_links(p: &Paragraph) -> ParaLinks {
    let mut at = 0;
    let mut out = Vec::new();
    for (i, inline) in p.content.iter().enumerate() {
        let len = docxcore::editor::inline_len(inline);
        if let Some(shown) = docxcore::editor::hyperlink_shown(&p.content, i) {
            if !shown.url.is_empty() {
                let link = LinkHover {
                    url: shown.url,
                    text: shown.text,
                };
                out.push((at..at + len, i, link));
            }
        }
        at += len;
    }
    out
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

    fn hover(url: &str, text: &str) -> LinkHover {
        LinkHover {
            url: url.into(),
            text: text.into(),
        }
    }

    #[test]
    fn link_hover_at_gives_the_whole_link_text_1916() {
        // One link of three runs: "do", bold "cx", "y link".
        let mut runs = Vec::new();
        for (text, bold) in [("do", false), ("cx", true), ("y link", false)] {
            let mut r = Run {
                text: text.into(),
                ..Run::default()
            };
            r.props.bold = bold;
            runs.push(r);
        }
        let p = Paragraph {
            content: vec![
                run("see "),
                Inline::Hyperlink(Hyperlink {
                    target: Some("https://example.com/d".into()),
                    runs,
                    ..Hyperlink::default()
                }),
                run(" or "),
                link("top", None, Some("top")),
            ],
            ..Paragraph::default()
        };
        let whole = hover("https://example.com/d", "docxy link");
        // Every offset in the link, each run's, is the same link.
        for off in 4..14 {
            assert_eq!(link_hover_at(&p, off), Some((1, whole.clone())), "at {off}");
        }
        assert_eq!(link_hover_at(&p, 3), None);
        assert_eq!(link_hover_at(&p, 14), None);
        assert_eq!(link_hover_at(&p, 18), Some((3, hover("#top", "top"))));
    }

    #[test]
    fn two_links_to_one_target_split_by_a_break_are_two_hovers_1916() {
        let p = Paragraph {
            content: vec![
                link("a b", Some("https://u.example/"), None),
                Inline::Break(docxcore::model::BreakKind::Line, Default::default()),
                link("c", Some("https://u.example/"), None),
            ],
            ..Paragraph::default()
        };
        let first = link_hover_at(&p, 1).expect("the first link");
        let second = link_hover_at(&p, 4).expect("the second link");
        assert_eq!(first, (0, hover("https://u.example/", "a b")));
        assert_eq!(second, (2, hover("https://u.example/", "c")));
        // Its own key: the hover changes from one to the other.
        assert_ne!(first.0, second.0);
        // The break between them is neither.
        assert_eq!(link_hover_at(&p, 3), None);
    }

    #[test]
    fn a_link_with_target_and_anchor_keeps_its_fragment_1916() {
        let p = Paragraph {
            content: vec![link("page", Some("https://x.example/page"), Some("frag"))],
            ..Paragraph::default()
        };
        let url = "https://x.example/page#frag";
        assert_eq!(link_at(&p, 1).as_deref(), Some(url));
        assert_eq!(link_hover_at(&p, 1), Some((0, hover(url, "page"))));
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
        let inks = crate::dark_page::inks(0x262626);
        assert_eq!(p.fg, hsla_u(inks.fg));
        assert_eq!(p.link, hsla_u(inks.link));
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
    fn the_layout_is_web_until_set_and_notifies_only_on_a_change_1888() {
        let mut layout = DocLayout::default();
        assert_eq!(layout, DocLayout::Web);
        assert!(set_if_changed(&mut layout, DocLayout::Print));
        assert!(!set_if_changed(&mut layout, DocLayout::Print));
        assert!(set_if_changed(&mut layout, DocLayout::Web));
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
