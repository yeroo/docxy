//! The PDF view (#2245): a PDF opens as a reader shows it, a column of
//! drawn pages with zoom, page navigation, the outline, and text that
//! selects, copies and searches in reading order; links, comments' pop-ups
//! and form filling (#2255, `form.rs`). This is its state, without gpui
//! drawing: the page layout at a zoom, which page to ask the worker for
//! next (form operations first), the raster cache and its budget, each
//! page's revision, the selection, Find, and the `pdf-read` JSON.
//! `worker.rs` reads, fills and saves the file (in a child process),
//! `render.rs` draws, `host.rs` wires keys, presses and Save.

use ctlcore::json::Json;
use gpui::{RenderImage, ScrollHandle};
use pdfcraft_render::PageText;
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use worker::{
    Answer, Bookmark, DocMeta, Done, Engine, FieldMeta, FormState, LinkMeta, NoteMeta, Op,
    PageMeta, Secret,
};

pub(crate) mod convert;
pub(crate) mod form;
pub(crate) mod host;
pub(crate) mod render;
pub(crate) mod worker;

/// Logical pixels per point at 100 %: a page shows at its printed size.
pub(crate) const PX_PER_PT: f32 = 96. / 72.;
/// The space around and between pages (logical px).
pub(crate) const GAP: f32 = 12.;
pub(crate) const ZOOM_MIN: f32 = 0.1;
pub(crate) const ZOOM_MAX: f32 = 8.;
/// What Ctrl+= and Ctrl+- step through.
pub(crate) const ZOOM_STEPS: [f32; 13] =
    [0.1, 0.25, 0.5, 0.75, 1., 1.25, 1.5, 2., 3., 4., 5., 6., 8.];
/// A raster's largest side and pixel count: past them a page is drawn at a
/// lower scale and stretched (tiles are a later slice). 800 % on Letter at
/// a 2× display (8 × 96/72 × 2 px a point) would be 13056 × 16896 px, about
/// 880 MB of BGRA.
pub(crate) const MAX_SIDE: f32 = 4096.;
pub(crate) const MAX_PIXELS: f32 = 16.0e6;
/// The raster bytes one view keeps; past it the least recently shown page
/// not on screen goes (its GPU texture freed with it).
pub(crate) const RASTER_BUDGET: usize = 256 << 20;
/// Pages asked for past the last one showing.
const AHEAD: usize = 1;
/// A line's scroll (arrows), logical px.
const LINE: f32 = 40.;

/// A box in content coordinates (logical px; the first page's top is
/// `GAP`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Where every page sits at a zoom: one column, centred in the view or in
/// the widest page plus its gaps.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub pages: Vec<Rect>,
    pub width: f32,
    pub height: f32,
}

pub(crate) fn layout(pages: &[PageMeta], zoom: f32, view_w: f32) -> Layout {
    let k = zoom * PX_PER_PT;
    let widest = pages.iter().map(|p| p.w * k).fold(0., f32::max);
    let width = view_w.max(widest + 2. * GAP);
    let mut y = GAP;
    let mut rects = Vec::with_capacity(pages.len());
    for p in pages {
        let (w, h) = (p.w * k, p.h * k);
        rects.push(Rect {
            x: ((width - w) / 2.).max(GAP).floor(),
            y,
            w,
            h,
        });
        y += h + GAP;
    }
    Layout {
        pages: rects,
        width,
        height: y,
    }
}

/// The pages a view `[top, top + h)` shows.
pub(crate) fn visible(l: &Layout, top: f32, h: f32) -> Range<usize> {
    let first = l.pages.partition_point(|r| r.bottom() <= top);
    let end = l.pages.partition_point(|r| r.y < top + h.max(1.));
    first..end.max(first)
}

/// The page a view `[top, top + h)` is on: the one under its middle, or the
/// next one when the middle is in a gap.
pub(crate) fn current_page(l: &Layout, top: f32, h: f32) -> usize {
    let mid = top + h / 2.;
    l.pages
        .partition_point(|r| r.bottom() <= mid)
        .min(l.pages.len().saturating_sub(1))
}

/// The scale (device px per point) page `p` is drawn at: the zoom on this
/// display, lowered to keep the raster within [`MAX_SIDE`] and
/// [`MAX_PIXELS`].
pub(crate) fn raster_scale(p: &PageMeta, zoom: f32, scale_factor: f32) -> f32 {
    let s = zoom * PX_PER_PT * scale_factor.max(0.5);
    let side = MAX_SIDE / p.w.max(p.h).max(1.);
    let area = (MAX_PIXELS / (p.w * p.h).max(1.)).sqrt();
    s.min(side).min(area).max(0.01)
}

/// Whether two scales draw the same raster.
fn same_scale(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-3 * a.abs().max(b.abs())
}

/// The next step of [`ZOOM_STEPS`] up or down from `z`.
pub(crate) fn zoom_step(z: f32, up: bool) -> f32 {
    if up {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|s| *s > z + 1e-4)
            .unwrap_or(ZOOM_MAX)
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < z - 1e-4)
            .unwrap_or(ZOOM_MIN)
    }
}

pub(crate) fn clamp_zoom(z: f32) -> f32 {
    if z.is_finite() {
        z.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        1.
    }
}

/// The zoom that fits the widest page in `view_w` (its gaps and a
/// scrollbar's room left).
pub(crate) fn fit_width(pages: &[PageMeta], view_w: f32) -> f32 {
    let widest = pages.iter().map(|p| p.w).fold(0., f32::max).max(1.);
    clamp_zoom((view_w - 2. * GAP - 16.) / (widest * PX_PER_PT))
}

/// The page box's text as a page: "7", or "7 / 20" as it shows; `None` for
/// anything else or a page past the end.
pub(crate) fn parse_page_box(text: &str, count: usize) -> Option<usize> {
    let first = text.split('/').next()?.trim();
    let n: usize = first.parse().ok()?;
    (1..=count).contains(&n).then(|| n - 1)
}

/// A drawn page kept for drawing.
pub(crate) struct Cached {
    pub image: Arc<RenderImage>,
    pub scale: f32,
    /// The page's revision it was drawn at (#2255): an older one is shown
    /// until the page drawn again comes in.
    pub rev: u64,
    bytes: usize,
    used: u64,
}

/// A save on its way (#2255): what runs once it is over, and how it went.
pub(crate) struct PendingSave {
    /// Run last, once the save is over (#2194).
    pub after: Option<crate::AfterSave>,
    /// The written file's path, or why not; `None` while the worker writes.
    pub outcome: Option<Result<PathBuf, String>>,
}

/// A press on a link, followed on its release unless a drag starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LinkPress {
    pub page: usize,
    pub link: usize,
    /// Content coordinates of the press.
    pub at: (f32, f32),
}

/// A comment's pop-up: shown while the pointer is on it, or pinned open by
/// a click until another click or Esc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotePopup {
    pub page: usize,
    pub note: usize,
    pub pinned: bool,
}

/// What is under the pointer (#2255): what its cursor and tooltip show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Under {
    #[default]
    Nothing,
    /// A form field's widget: `(field, widget)`.
    Widget(usize, usize),
    /// Comment `note` of `page`'s notes.
    Note(usize, usize),
    /// Link `link` of `page`'s links.
    Link(usize, usize),
}

/// How far a press on a link may move before it is a drag (content px).
const DRAG_SLOP: f32 = 4.;

/// A glyph of a page's text: `glyph` indexes its `PageText`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub page: usize,
    pub glyph: usize,
}

/// The selection, both ends inclusive, in reading order once ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sel {
    pub anchor: Pos,
    pub head: Pos,
}

impl Sel {
    pub(crate) fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The glyphs selected on `page`, of `len`.
    pub(crate) fn on_page(&self, page: usize, len: usize) -> Option<Range<usize>> {
        let (a, b) = self.ordered();
        if page < a.page || page > b.page || len == 0 {
            return None;
        }
        let start = if page == a.page { a.glyph.min(len) } else { 0 };
        let end = if page == b.page {
            b.glyph.saturating_add(1).min(len)
        } else {
            len
        };
        (start < end).then_some(start..end)
    }
}

/// What a drag extends by: glyphs, or the words or lines a double or
/// triple press started on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Glyph,
    Word,
    Line,
}

/// A selecting drag: its unit and the unit it started on (first and last
/// glyph).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Drag {
    pub unit: Unit,
    pub first: Pos,
    pub last: Pos,
}

/// A Find hit: glyphs of a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub page: usize,
    pub glyphs: Range<usize>,
}

/// A one-line field: Find's query and the page box.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Field {
    pub text: String,
    pub caret: usize,
    pub sel: bool,
    pub focused: bool,
}

impl Field {
    pub(crate) fn edit(&mut self, op: crate::field_edit::Op) {
        crate::field_edit::apply(&mut self.text, &mut self.caret, &mut self.sel, op);
    }

    /// Focused with its whole text selected, as a field Tab enters.
    pub(crate) fn focus_all(&mut self) {
        self.focused = true;
        self.edit(crate::field_edit::Op::SelectAll);
    }
}

/// The view's Find box.
#[derive(Clone, Debug, Default)]
pub(crate) struct Find {
    pub open: bool,
    pub field: Field,
    pub case: bool,
    pub whole: bool,
    pub hits: Vec<Hit>,
    pub current: Option<usize>,
    /// Pages searched so far (the tests' check that a page's text coming
    /// in searches that page alone).
    #[cfg(test)]
    pub searched: usize,
}

/// An encrypted PDF waiting for its password (#2256): the view shows the
/// prompt instead of pages, and asks its worker itself, one try at a time.
pub(crate) struct PdfLock {
    /// Taken when the file opens, to read its pages.
    worker: Option<worker::Worker>,
    /// The password being typed: emptied by each try.
    pub field: Field,
    /// The last try's password did not open it.
    pub wrong: bool,
}

impl Drop for PdfLock {
    /// The password being typed is cleared, not only freed (#2256).
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.field.text);
    }
}

/// What a try of the typed password did (#2256).
#[derive(Debug, PartialEq)]
pub(crate) enum Unlocked {
    /// It opened: the view shows its pages now.
    Opened,
    /// Not the password: the prompt asks again.
    Wrong,
    /// The file could not be read after all.
    Failed(String),
}

/// One PDF, open in a tab.
pub(crate) struct PdfView {
    pub path: PathBuf,
    pub meta: DocMeta,
    engine: Option<Engine>,
    /// The question the worker is answering.
    in_flight: Option<Op>,
    pub zoom: f32,
    pub scroll: ScrollHandle,
    /// Where the next frame scrolls the body to (content x, y).
    pub scroll_to: Cell<Option<(f32, f32)>>,
    /// The body's size at the last frame (logical px).
    pub viewport: Cell<(f32, f32)>,
    /// The window's scale factor at the last frame.
    pub scale_factor: f32,
    /// Pages to draw, showing first, then the ones after.
    pub wanted: Vec<usize>,
    /// The pages the last frame showed.
    pub showing: Range<usize>,
    pub rasters: Vec<Option<Cached>>,
    /// Pages that could not be drawn, and why. Never asked for again.
    pub failed: HashMap<usize, String>,
    pub texts: Vec<Option<Arc<PageText>>>,
    pub text_failed: HashMap<usize, String>,
    /// GPU images let go of: the host frees them with `drop_image`.
    pub released: Vec<Arc<RenderImage>>,
    clock: u64,
    pub sel: Option<Sel>,
    pub drag: Option<Drag>,
    pub find: Find,
    pub page_box: Field,
    /// The outline pane shows.
    pub outline_shown: bool,
    /// The page a restored session left the view on, gone to once the
    /// first frame knows the body's size.
    pub restore_page: Option<usize>,
    /// Ctrl+A or Find needs every page's text.
    pub want_all_text: bool,
    /// The selection a Ctrl+C waits on for a page's text. A change of the
    /// selection (Escape, a press, Find, Ctrl+A) drops the copy.
    pub copy_pending: Option<Sel>,
    /// Each page's links and comments with text (#2255), in view space.
    pub links: Vec<Vec<LinkMeta>>,
    pub notes: Vec<Vec<NoteMeta>>,
    /// The form's fields, and each page's widgets as `(field, widget)`.
    pub form: worker::FormMeta,
    pub widgets: Vec<Vec<(usize, usize)>>,
    /// Form operations waiting: sent before any page, in order.
    pub form_ops: VecDeque<Op>,
    /// Form values accepted so far, and how many of them the file holds:
    /// the tab is unsaved while they differ.
    pub form_rev: u64,
    pub saved_rev: u64,
    /// Each page's revision: bumped when a value shown on it changes, so it
    /// is drawn and read again.
    page_rev: Vec<u64>,
    text_rev: Vec<u64>,
    /// The field that has the keys.
    pub focus: Option<form::FormFocus>,
    pub popup: Option<NotePopup>,
    /// What the pointer is over, and where (content px).
    pub under: Under,
    pub pointer: (f32, f32),
    pub link_press: Option<LinkPress>,
    pub pending_save: Option<PendingSave>,
    /// A save of a signed PDF waiting on its question: where, and what
    /// runs once it is over.
    pub ask_signed: Option<(PathBuf, crate::AfterSave)>,
    /// For the host: a status line to show, a web or mail address to open,
    /// and that the unsaved values were lost (a new reader could not take
    /// them).
    pub notice: Option<String>,
    pub open_url: Option<String>,
    pub values_lost: bool,
    /// An encrypted file's password prompt (#2256): until it opens, the view
    /// has no pages and no engine.
    pub lock: Option<Box<PdfLock>>,
    /// The questions sent so far (the scheduler's tests).
    #[cfg(test)]
    pub sent: Vec<Op>,
}

impl PdfView {
    /// A view of `path` whose `info` is `meta`, its pages asked of `engine`.
    pub(crate) fn new(path: &Path, meta: DocMeta, engine: Option<Engine>) -> PdfView {
        let n = meta.pages.len();
        let mut v = PdfView {
            path: path.to_path_buf(),
            engine,
            in_flight: None,
            zoom: 1.,
            scroll: ScrollHandle::new(),
            scroll_to: Cell::new(None),
            viewport: Cell::new((800., 600.)),
            scale_factor: 1.,
            wanted: Vec::new(),
            showing: 0..0,
            rasters: (0..n).map(|_| None).collect(),
            failed: HashMap::new(),
            texts: vec![None; n],
            text_failed: HashMap::new(),
            released: Vec::new(),
            clock: 0,
            sel: None,
            drag: None,
            find: Find::default(),
            page_box: Field::default(),
            outline_shown: !meta.outline.is_empty(),
            restore_page: None,
            want_all_text: false,
            copy_pending: None,
            links: per_page(n, &meta.links, |l| l.page),
            notes: per_page(n, &meta.notes, |l| l.page),
            form: meta.form.clone(),
            widgets: Vec::new(),
            form_ops: VecDeque::new(),
            form_rev: 0,
            saved_rev: 0,
            page_rev: vec![0; n],
            text_rev: vec![0; n],
            focus: None,
            popup: None,
            under: Under::Nothing,
            pointer: (0., 0.),
            link_press: None,
            pending_save: None,
            ask_signed: None,
            notice: None,
            open_url: None,
            values_lost: false,
            lock: None,
            #[cfg(test)]
            sent: Vec::new(),
            meta,
        };
        v.index_widgets();
        v
    }

    /// Each page's widgets, from the form's fields.
    pub(crate) fn index_widgets(&mut self) {
        let mut widgets = vec![Vec::new(); self.page_count()];
        for (f, field) in self.form.fields.iter().enumerate() {
            for (w, widget) in field.widgets.iter().enumerate() {
                if let Some(p) = widget.page.filter(|p| *p < widgets.len()) {
                    widgets[p].push((f, w));
                }
            }
        }
        self.widgets = widgets;
    }

    /// Whether the tab has values the file does not (#2255).
    pub(crate) fn unsaved(&self) -> bool {
        self.form_rev != self.saved_rev
    }

    /// Open `path`: start a worker and ask its `info`, waiting at most the
    /// worker's timeout (as a conversion waits). `Err` says why the file is
    /// no PDF the view can show. An encrypted file that needs a password is
    /// a view with its prompt ([`PdfView::lock`], #2256).
    pub(crate) fn open(path: &Path, spawn: worker::Spawn) -> Result<PdfView, String> {
        let mut w = worker::Worker::new(path, spawn, worker::TIMEOUT);
        match w.ask(&Op::Info) {
            Answer::Info(meta) => Ok(PdfView::new(path, meta, Some(Engine::new(w)))),
            Answer::Locked { .. } => {
                let mut v = PdfView::new(path, DocMeta::default(), None);
                v.lock = Some(Box::new(PdfLock {
                    worker: Some(w),
                    field: Field {
                        focused: true,
                        ..Field::default()
                    },
                    wrong: false,
                }));
                Ok(v)
            }
            Answer::Failed(why) => Err(why),
            _ => Err(worker::DIED.into()),
        }
    }

    /// Try the password typed into the prompt (#2256), waiting for the
    /// worker as the open did. The field is emptied whatever the outcome;
    /// a password that opens the file stays with the view's worker (a new
    /// reader is unlocked with it), and the view shows the pages, where the
    /// session left it.
    pub(crate) fn try_unlock(&mut self) -> Unlocked {
        let Some(lock) = self.lock.as_mut() else {
            return Unlocked::Opened;
        };
        // The typed text moves into the secret, which clears it when dropped.
        let password = Secret::new(std::mem::take(&mut lock.field.text));
        lock.field = Field {
            focused: true,
            ..Field::default()
        };
        let Some(w) = lock.worker.as_mut() else {
            return Unlocked::Failed(worker::DIED.into());
        };
        match w.unlock(password) {
            Answer::Info(meta) => {
                let engine = lock.worker.take().map(Engine::new);
                let mut v = PdfView::new(&self.path, meta, engine);
                v.zoom = self.zoom;
                v.restore_page = self.restore_page.take();
                *self = v;
                Unlocked::Opened
            }
            Answer::Locked { wrong } => {
                lock.wrong = wrong;
                Unlocked::Wrong
            }
            Answer::Failed(why) => Unlocked::Failed(why),
            _ => Unlocked::Failed(worker::DIED.into()),
        }
    }

    /// The status line's word on what the file withholds (#2256):
    /// `; restricted: printing, copying` or nothing.
    pub(crate) fn restricted_note(&self) -> String {
        let r = self
            .meta
            .security
            .as_ref()
            .map(worker::SecurityMeta::restrictions)
            .unwrap_or_default();
        if r.is_empty() {
            String::new()
        } else {
            format!("; restricted: {}", r.join(", "))
        }
    }

    pub(crate) fn page_count(&self) -> usize {
        self.meta.pages.len()
    }

    pub(crate) fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub(crate) fn layout(&self) -> Layout {
        layout(&self.meta.pages, self.zoom, self.viewport.get().0)
    }

    /// The body's scroll offset as content coordinates of its top-left.
    pub(crate) fn top_left(&self) -> (f32, f32) {
        let o = self.scroll.offset();
        (-f32::from(o.x), -f32::from(o.y))
    }

    /// The page the view is on.
    pub(crate) fn current(&self) -> usize {
        let (_, top) = self.pending_or_top();
        current_page(&self.layout(), top, self.viewport.get().1)
    }

    /// The top the next frame shows: a scroll asked for, else where the
    /// body is.
    fn pending_or_top(&self) -> (f32, f32) {
        self.scroll_to.get().unwrap_or_else(|| self.top_left())
    }

    // ---- the frame -----------------------------------------------------------

    /// A frame lays the body out: note what it shows (and so what to draw
    /// next), and the display's scale.
    pub(crate) fn frame(&mut self, view_w: f32, view_h: f32, top: f32, scale_factor: f32) {
        self.viewport.set((view_w.max(1.), view_h.max(1.)));
        self.scale_factor = scale_factor;
        let l = self.layout();
        let showing = visible(&l, top, view_h);
        self.clock += 1;
        for p in showing.clone() {
            if let Some(c) = self.rasters.get_mut(p).and_then(Option::as_mut) {
                c.used = self.clock;
            }
        }
        let n = self.page_count();
        self.wanted = showing
            .clone()
            .chain(showing.end..(showing.end + AHEAD).min(n))
            .collect();
        self.showing = showing;
    }

    /// The question to send next, if any: the pages showing at this zoom,
    /// then their text (for selection), then the page after, then the text
    /// Find or Ctrl+A needs. A page no longer showing, or an old zoom, is
    /// never asked for; a page that failed is never asked for again.
    pub(crate) fn next_job(&self) -> Option<Op> {
        // The form first (#2255): a value set or a save goes ahead of pages.
        if let Some(op) = self.form_ops.front() {
            return Some(op.clone());
        }
        let showing = self.showing.clone();
        let pixels = |p: usize| -> Option<Op> {
            let meta = self.meta.pages.get(p)?;
            if self.failed.contains_key(&p) {
                return None;
            }
            let scale = raster_scale(meta, self.zoom, self.scale_factor);
            let have = self.rasters[p]
                .as_ref()
                .is_some_and(|c| same_scale(c.scale, scale) && c.rev == self.page_rev[p]);
            (!have).then_some(Op::Pixels { page: p, scale })
        };
        let text = |p: usize| -> Option<Op> { self.text_due(p).then_some(Op::Text { page: p }) };
        if let Some(op) = showing.clone().find_map(pixels) {
            return Some(op);
        }
        if let Some(op) = showing.clone().find_map(text) {
            return Some(op);
        }
        if let Some(op) = self
            .wanted
            .iter()
            .copied()
            .filter(|p| !showing.contains(p))
            .find_map(pixels)
        {
            return Some(op);
        }
        if self.want_all_text || !self.find.field.text.is_empty() {
            // From the page shown on, then the ones before it.
            let n = self.page_count();
            let from = self.showing.start.min(n);
            return (from..n).chain(0..from).find_map(text);
        }
        None
    }

    /// Whether the worker has a question out, or a form operation waits.
    pub(crate) fn busy(&self) -> bool {
        self.in_flight.is_some() || !self.form_ops.is_empty()
    }

    /// Whether an answer waits to be taken.
    pub(crate) fn has_answers(&self) -> bool {
        self.engine.as_ref().is_some_and(Engine::has_answers)
    }

    /// Take the worker's answers in, then send the next question if none is
    /// out. Called from the frame, which draws whatever changed.
    pub(crate) fn pump(&mut self) {
        let done = self.engine.as_ref().map(Engine::take).unwrap_or_default();
        for d in done {
            self.absorb(d);
        }
        if self.in_flight.is_none()
            && let Some(op) = self.next_job()
        {
            self.send(op);
        }
    }

    /// Take the answers in and send form operations only: a view behind
    /// another tab finishes its save (#2255) without asking for pages.
    pub(crate) fn pump_form(&mut self) {
        let done = self.engine.as_ref().map(Engine::take).unwrap_or_default();
        for d in done {
            self.absorb(d);
        }
        if self.in_flight.is_none()
            && let Some(op) = self.form_ops.front().cloned()
        {
            self.send(op);
        }
    }

    fn send(&mut self, op: Op) {
        if op.is_form() {
            self.form_ops.pop_front();
        }
        let sent = self.engine.as_ref().is_some_and(|e| e.send(op.clone()));
        if sent {
            #[cfg(test)]
            self.sent.push(op.clone());
            self.in_flight = Some(op);
        } else {
            // No worker (it could not start a thread): the page says so
            // rather than wait for ever.
            self.fail(&op, worker::DIED.into());
        }
    }

    /// An answer in.
    pub(crate) fn absorb(&mut self, d: Done) {
        if self.in_flight.as_ref() == Some(&d.op) {
            self.in_flight = None;
        }
        if d.lost {
            self.lose_values();
        }
        match (d.op, d.answer) {
            (Op::Pixels { page, scale }, Answer::Pixels(r)) if page < self.page_count() => {
                let bytes = r.bgra.len();
                let Some(image) = image_of(r) else {
                    self.failed.insert(page, worker::DIED.into());
                    return;
                };
                if let Some(old) = self.rasters[page].take() {
                    self.released.push(old.image);
                }
                self.clock += 1;
                self.rasters[page] = Some(Cached {
                    image,
                    scale,
                    rev: self.page_rev[page],
                    bytes,
                    used: self.clock,
                });
                self.evict(RASTER_BUDGET);
            }
            (Op::Text { page }, Answer::Text(t)) if page < self.page_count() => {
                // Read again after a value changed (#2255): a selection on
                // the page goes, and its Find hits are replaced.
                if self.texts[page].is_some()
                    && self
                        .sel
                        .is_some_and(|s| s.on_page(page, usize::MAX).is_some())
                {
                    self.sel = None;
                    self.drag = None;
                }
                self.texts[page] = Some(t);
                self.text_rev[page] = self.page_rev[page];
                self.find_on_page(page);
            }
            (Op::SetField { .. }, Answer::Form(st)) => {
                self.form_rev += 1;
                self.take_form(st);
            }
            (Op::Fields | Op::SetFields { .. }, Answer::Form(st)) => self.take_form(st),
            (Op::SaveTo { .. }, Answer::Saved(path)) => {
                // Every value accepted before the save went in it; one
                // accepted after keeps the tab unsaved.
                self.saved_rev = self.form_rev;
                self.path = path.clone();
                if let Some(s) = self.pending_save.as_mut() {
                    s.outcome = Some(Ok(path));
                }
            }
            (op, Answer::Failed(why)) => self.fail(&op, why),
            _ => {}
        }
    }

    /// The form as an answer has it: the pages whose values changed are
    /// drawn and read again.
    fn take_form(&mut self, st: FormState) {
        for p in st.pages {
            if let Some(r) = self.page_rev.get_mut(p) {
                *r += 1;
            }
        }
        let same_shape = st.fields.len() == self.form.fields.len();
        self.form.fields = st.fields;
        if !same_shape {
            self.focus = None;
        }
        self.index_widgets();
    }

    /// A new reader could not take the unsaved values (#2255): the form is
    /// read again from the file, every page with a field is drawn again, and
    /// the tab holds nothing unsaved.
    fn lose_values(&mut self) {
        self.saved_rev = self.form_rev;
        self.focus = None;
        self.form_ops.push_front(Op::Fields);
        for (p, w) in self.widgets.iter().enumerate() {
            if !w.is_empty() {
                self.page_rev[p] += 1;
            }
        }
        self.values_lost = true;
    }

    fn fail(&mut self, op: &Op, why: String) {
        match op {
            Op::Pixels { page, .. } => {
                self.failed.insert(*page, why);
            }
            Op::Text { page } => {
                self.text_failed.insert(*page, why);
            }
            Op::SetField { name, .. } => {
                self.notice = Some(format!("{name} was not filled in: {why}"));
            }
            Op::SaveTo { .. } => {
                if let Some(s) = self.pending_save.as_mut() {
                    s.outcome = Some(Err(why));
                }
            }
            Op::Info
            | Op::Fields
            | Op::SetFields { .. }
            | Op::Save
            | Op::Saved
            | Op::Unlock { .. } => {}
        }
    }

    /// Let rasters go, least recently shown first and never one wanted,
    /// until they fit `budget` bytes. Their images go to `released`.
    pub(crate) fn evict(&mut self, budget: usize) {
        let mut total: usize = self.rasters.iter().flatten().map(|c| c.bytes).sum();
        while total > budget {
            let victim = self
                .rasters
                .iter()
                .enumerate()
                .filter(|(p, c)| c.is_some() && !self.wanted.contains(p))
                .min_by_key(|(_, c)| c.as_ref().map_or(0, |c| c.used))
                .map(|(p, _)| p);
            let Some(p) = victim else { break };
            if let Some(c) = self.rasters[p].take() {
                total -= c.bytes;
                self.released.push(c.image);
            }
        }
    }

    /// Every image the view holds, for the host to free when the tab goes.
    pub(crate) fn all_images(&self) -> Vec<Arc<RenderImage>> {
        self.rasters
            .iter()
            .flatten()
            .map(|c| c.image.clone())
            .chain(self.released.iter().cloned())
            .collect()
    }

    /// The state of page `p` as `pdf-read` reports it.
    pub(crate) fn page_state(&self, p: usize) -> String {
        if let Some(why) = self.failed.get(&p) {
            format!("failed: {why}")
        } else if self.rasters.get(p).is_some_and(Option::is_some) {
            "drawn".into()
        } else {
            "pending".into()
        }
    }

    /// What a page that could not be drawn says on its placeholder.
    pub(crate) fn placeholder(&self, p: usize) -> Option<String> {
        self.failed
            .get(&p)
            .map(|why| format!("this page could not be drawn: {why}"))
    }

    // ---- moving about ----------------------------------------------------------

    /// Scroll to content `(x, y)` on the next frame, within the content.
    pub(crate) fn scroll_to_xy(&self, x: f32, y: f32) {
        let l = self.layout();
        let (vw, vh) = self.viewport.get();
        let x = x.clamp(0., (l.width - vw).max(0.));
        let y = y.clamp(0., (l.height - vh).max(0.));
        self.scroll_to.set(Some((x, y)));
    }

    pub(crate) fn go_to(&mut self, page: usize) {
        let l = self.layout();
        if let Some(r) = l.pages.get(page) {
            let (x, _) = self.pending_or_top();
            self.scroll_to_xy(x, r.y - GAP / 2.);
        }
    }

    /// A navigation key on the body: `true` when it was one.
    pub(crate) fn nav_key(&mut self, key: &str, ctrl: bool) -> bool {
        let (x, y) = self.pending_or_top();
        let (_, vh) = self.viewport.get();
        let page = self.current();
        let n = self.page_count();
        match key {
            "pagedown" => self.scroll_to_xy(x, y + vh * 0.9),
            "pageup" => self.scroll_to_xy(x, y - vh * 0.9),
            "down" => self.scroll_to_xy(x, y + LINE),
            "up" => self.scroll_to_xy(x, y - LINE),
            "right" if ctrl => self.go_to((page + 1).min(n.saturating_sub(1))),
            "left" if ctrl => self.go_to(page.saturating_sub(1)),
            "right" => self.scroll_to_xy(x + LINE, y),
            "left" => self.scroll_to_xy(x - LINE, y),
            "home" => self.scroll_to_xy(0., 0.),
            "end" => self.scroll_to_xy(0., f32::MAX),
            _ => return false,
        }
        true
    }

    /// Zoom to `z`, keeping the view on the same place of the page it shows.
    pub(crate) fn set_zoom(&mut self, z: f32) {
        let z = clamp_zoom(z);
        if same_scale(z, self.zoom) {
            return;
        }
        let before = self.layout();
        let (x, y) = self.pending_or_top();
        let page = self.current();
        let ratio = z / self.zoom;
        self.zoom = z;
        let after = self.layout();
        let (Some(b), Some(a)) = (before.pages.get(page), after.pages.get(page)) else {
            return;
        };
        let (vw, _) = self.viewport.get();
        let ny = a.y + (y - b.y) * ratio;
        let nx = (x + vw / 2.) * after.width / before.width.max(1.) - vw / 2.;
        self.scroll_to_xy(nx, ny);
    }

    pub(crate) fn zoom_in(&mut self, up: bool) {
        self.set_zoom(zoom_step(self.zoom, up));
    }

    pub(crate) fn fit_width(&mut self) {
        let z = fit_width(&self.meta.pages, self.viewport.get().0);
        self.set_zoom(z);
    }

    /// The page box as it shows when not typed in: "n / N".
    pub(crate) fn page_box_text(&self) -> String {
        format!("{} / {}", self.current() + 1, self.page_count())
    }

    // ---- hit testing and selection --------------------------------------------

    /// The page and point (points, page view space) under content `(x,
    /// y)`: the page it is on, else the nearest one above or below.
    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<(usize, f32, f32)> {
        let l = self.layout();
        if l.pages.is_empty() {
            return None;
        }
        let p = l
            .pages
            .partition_point(|r| r.bottom() < y)
            .min(l.pages.len() - 1);
        let r = l.pages[p];
        let k = self.zoom * PX_PER_PT;
        Some((p, (x - r.x) / k, (y - r.y) / k))
    }

    /// The glyph nearest content `(x, y)`, on a page whose text is in.
    pub(crate) fn glyph_at(&self, x: f32, y: f32) -> Option<Pos> {
        let (page, px, py) = self.hit(x, y)?;
        let t = self.texts.get(page)?.as_ref()?;
        let glyph = t.nearest(px, py)?;
        Some(Pos { page, glyph })
    }

    /// The `unit` around `pos`: its first and last glyph.
    fn unit_at(&self, pos: Pos, unit: Unit) -> (Pos, Pos) {
        let t = self.texts.get(pos.page).and_then(Option::as_ref);
        let span = t.and_then(|t| match unit {
            Unit::Glyph => Some((pos.glyph, pos.glyph)),
            Unit::Word => t.word_at(pos.glyph),
            Unit::Line => t.line_at(pos.glyph),
        });
        let (a, b) = span.unwrap_or((pos.glyph, pos.glyph));
        (
            Pos {
                page: pos.page,
                glyph: a,
            },
            Pos {
                page: pos.page,
                glyph: b,
            },
        )
    }

    /// A press at content `(x, y)`: `clicks` 2 takes a word, 3 a line;
    /// Shift extends the selection. A press off any text clears it.
    pub(crate) fn press(&mut self, x: f32, y: f32, clicks: usize, shift: bool) {
        let Some(pos) = self.glyph_at(x, y) else {
            self.sel = None;
            self.drag = None;
            return;
        };
        if shift && let Some(sel) = self.sel {
            self.sel = Some(Sel {
                anchor: sel.anchor,
                head: pos,
            });
            self.drag = Some(Drag {
                unit: Unit::Glyph,
                first: sel.anchor,
                last: sel.anchor,
            });
            return;
        }
        let unit = match clicks {
            2 => Unit::Word,
            n if n >= 3 => Unit::Line,
            _ => Unit::Glyph,
        };
        let (first, last) = self.unit_at(pos, unit);
        self.drag = Some(Drag { unit, first, last });
        // A plain press only places the anchor: a drag selects.
        self.sel = (unit != Unit::Glyph).then_some(Sel {
            anchor: first,
            head: last,
        });
    }

    /// The pointer moved to content `(x, y)` with the button down.
    pub(crate) fn drag_to(&mut self, x: f32, y: f32) {
        let Some(d) = self.drag else { return };
        let Some(pos) = self.glyph_at(x, y) else {
            return;
        };
        let (hf, hl) = self.unit_at(pos, d.unit);
        self.sel = Some(if pos >= d.first {
            Sel {
                anchor: d.first,
                head: hl.max(d.last),
            }
        } else {
            Sel {
                anchor: d.last,
                head: hf,
            }
        });
    }

    pub(crate) fn release(&mut self) {
        self.drag = None;
    }

    /// Ctrl+A: every page's text (read as it comes in).
    pub(crate) fn select_all(&mut self) {
        let n = self.page_count();
        if n == 0 {
            return;
        }
        self.want_all_text = true;
        self.sel = Some(Sel {
            anchor: Pos { page: 0, glyph: 0 },
            head: Pos {
                page: n - 1,
                glyph: usize::MAX,
            },
        });
    }

    /// The selection's text, pages joined by a line break; `None` while a
    /// selected page's text is still being read. A page whose text could not
    /// be read adds nothing.
    pub(crate) fn selection_text(&self) -> Option<String> {
        let sel = self.sel?;
        let (a, b) = sel.ordered();
        let mut parts = Vec::new();
        for p in a.page..=b.page.min(self.page_count().saturating_sub(1)) {
            match &self.texts[p] {
                Some(t) => {
                    if let Some(r) = sel.on_page(p, t.glyphs.len()) {
                        parts.push(t.text_of(r));
                    }
                }
                None if self.text_owed(p) => return None,
                None => {}
            }
        }
        Some(parts.join("\n"))
    }

    /// Ctrl+C: the selection's text now, or `None` while a selected page's
    /// text is still being read (the copy then waits on this selection).
    pub(crate) fn copy(&mut self) -> Option<String> {
        let sel = self.sel?;
        match self.selection_text() {
            Some(text) => {
                self.copy_pending = None;
                Some(text)
            }
            None => {
                self.copy_pending = Some(sel);
                self.want_all_text = true;
                None
            }
        }
    }

    /// Whether a Ctrl+C still waits: on the selection it was pressed on.
    pub(crate) fn copy_waits(&self) -> bool {
        self.copy_pending.is_some() && self.copy_pending == self.sel
    }

    /// A copy that waited: its text once it is in. A selection changed
    /// since drops it: nothing is copied that Ctrl+C was not pressed on.
    pub(crate) fn take_pending_copy(&mut self) -> Option<String> {
        if !self.copy_waits() {
            self.copy_pending = None;
            return None;
        }
        let text = self.selection_text()?;
        self.copy_pending = None;
        Some(text)
    }

    /// Whether a selected page's text is not in yet (a page whose text
    /// failed counts as read, as in [`Self::selection_text`]). Asked every
    /// frame, so it builds no text.
    pub(crate) fn selection_waits(&self) -> bool {
        let Some(sel) = self.sel else {
            return false;
        };
        let (a, b) = sel.ordered();
        let n = self.page_count();
        (a.page..=b.page.min(n.saturating_sub(1)))
            .take_while(|p| *p < n)
            .any(|p| self.text_owed(p))
    }

    /// Whether page `p`'s text is still to come: not read, and not failed
    /// (a failed page counts as read everywhere). `false` past the end.
    pub(crate) fn text_owed(&self, p: usize) -> bool {
        self.texts.get(p).is_some_and(Option::is_none) && !self.text_failed.contains_key(&p)
    }

    /// Whether page `p`'s text is to be asked for: never read, or read
    /// before a value on it changed (#2255).
    pub(crate) fn text_due(&self, p: usize) -> bool {
        self.text_owed(p)
            || (self.texts.get(p).is_some_and(Option::is_some)
                && self.text_rev[p] != self.page_rev[p]
                && !self.text_failed.contains_key(&p))
    }

    /// Text is still needed for a selection: ask for it.
    pub(crate) fn note_selection_needs(&mut self) {
        if self.selection_waits() {
            self.want_all_text = true;
        }
    }

    // ---- Find -------------------------------------------------------------------

    /// The query or an option changed: find again now, so what the box
    /// says is never a step behind.
    pub(crate) fn find_changed(&mut self) {
        self.find.current = None;
        self.refind();
    }

    /// The query's hits on `page`, whose text is in.
    fn hits_on(&mut self, page: usize) -> Vec<Hit> {
        let q = &self.find.field.text;
        let Some(t) = self.texts.get(page).and_then(Option::as_ref) else {
            return Vec::new();
        };
        if q.trim().is_empty() {
            return Vec::new();
        }
        #[cfg(test)]
        {
            self.find.searched += 1;
        }
        t.find_opts(q, self.find.case, self.find.whole)
            .into_iter()
            .map(|glyphs| Hit { page, glyphs })
            .collect()
    }

    /// A page's text came in (or was read again after a value on it changed,
    /// #2255): its hits replace that page's old ones, in page order; the
    /// current hit stays the same hit, unless it was on that page, when
    /// there is none. Only that page is searched.
    fn find_on_page(&mut self, page: usize) {
        let new = self.hits_on(page);
        let at = self.find.hits.partition_point(|h| h.page < page);
        let end = self.find.hits.partition_point(|h| h.page <= page);
        let n = new.len();
        let removed = end - at;
        self.find.hits.splice(at..end, new);
        if let Some(k) = self.find.current {
            self.find.current = if k >= end {
                Some(k - removed + n)
            } else if k >= at {
                None
            } else {
                Some(k)
            };
        }
    }

    /// Find again over every page whose text is in.
    pub(crate) fn refind(&mut self) {
        let q = self.find.field.text.clone();
        let keep = self
            .find
            .current
            .and_then(|i| self.find.hits.get(i).cloned());
        self.find.hits.clear();
        if q.trim().is_empty() {
            self.find.current = None;
            return;
        }
        for p in 0..self.page_count() {
            let hits = self.hits_on(p);
            self.find.hits.extend(hits);
        }
        self.find.current = keep.and_then(|h| self.find.hits.iter().position(|x| *x == h));
    }

    /// Pages Find has not read yet.
    pub(crate) fn unread_pages(&self) -> usize {
        (0..self.page_count())
            .filter(|p| self.text_owed(*p))
            .count()
    }

    /// The Find box's count: "k of M", "No matches", with what is left to
    /// read and which pages' text is incomplete.
    pub(crate) fn find_status(&self) -> String {
        if self.find.field.text.trim().is_empty() {
            return String::new();
        }
        let m = self.find.hits.len();
        let mut s = match (m, self.find.current) {
            (0, _) => "No matches".to_string(),
            (m, Some(k)) => format!("{} of {m}", k + 1),
            (m, None) => format!("{m} found"),
        };
        let unread = self.unread_pages();
        if unread > 0 {
            s.push_str(&format!(
                " · reading {} of {} pages",
                self.page_count() - unread,
                self.page_count()
            ));
        }
        let mut short: Vec<usize> = (0..self.page_count())
            .filter(|p| {
                self.text_failed.contains_key(p)
                    || self.texts[*p].as_ref().is_some_and(|t| t.truncated)
            })
            .collect();
        short.sort_unstable();
        if let Some(first) = short.first() {
            let more = short.len() - 1;
            s.push_str(&format!(" · page {}'s text is incomplete", first + 1));
            if more > 0 {
                s.push_str(&format!(" (and {more} more)"));
            }
        }
        s
    }

    /// Go to the next hit (`forward`) or the previous one, wrapping; with
    /// none current, the first one from the page shown on. It is selected.
    pub(crate) fn find_step(&mut self, forward: bool) {
        let m = self.find.hits.len();
        if m == 0 {
            return;
        }
        let page = self.current();
        let next = match self.find.current {
            Some(k) if forward => (k + 1) % m,
            Some(k) => (k + m - 1) % m,
            None if forward => self
                .find
                .hits
                .iter()
                .position(|h| h.page >= page)
                .unwrap_or(0),
            None => self
                .find
                .hits
                .iter()
                .rposition(|h| h.page <= page)
                .unwrap_or(m - 1),
        };
        self.find.current = Some(next);
        let h = self.find.hits[next].clone();
        self.sel = Some(Sel {
            anchor: Pos {
                page: h.page,
                glyph: h.glyphs.start,
            },
            head: Pos {
                page: h.page,
                glyph: h.glyphs.end.saturating_sub(1),
            },
        });
        self.scroll_to_glyphs(h.page, h.glyphs);
    }

    /// Bring glyphs of `page` into view, a third down from the top.
    fn scroll_to_glyphs(&mut self, page: usize, glyphs: Range<usize>) {
        let Some(t) = self.texts.get(page).and_then(Option::as_ref) else {
            return self.go_to(page);
        };
        let Some(r) = t.line_rects(glyphs).first().copied() else {
            return self.go_to(page);
        };
        let l = self.layout();
        let pr = l.pages[page];
        let k = self.zoom * PX_PER_PT;
        let (x, y) = self.pending_or_top();
        let (vw, vh) = self.viewport.get();
        let (gx0, gy0, gx1, gy1) = (
            pr.x + r[0] * k,
            pr.y + r[1] * k,
            pr.x + r[2] * k,
            pr.y + r[3] * k,
        );
        let ny = if gy0 >= y && gy1 <= y + vh {
            y
        } else {
            gy0 - vh / 3.
        };
        let nx = if gx0 >= x && gx1 <= x + vw {
            x
        } else {
            gx0 - GAP
        };
        self.scroll_to_xy(nx, ny);
    }

    // ---- the harness ---------------------------------------------------------------

    /// `pdf-read`: the pages, the outline, where the view is and what the
    /// selection and Find hold.
    pub(crate) fn read_json(&self) -> Json {
        let s = |t: &str| Json::Str(t.to_string());
        let n = |v: usize| Json::Num(v as f64);
        let f = |v: f32| Json::Num(((v * 100.).round() / 100.) as f64);
        let pages = self
            .meta
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                Json::obj(vec![
                    ("width", f(p.w)),
                    ("height", f(p.h)),
                    ("rotation", n(p.rotation as usize)),
                    ("label", s(&p.label)),
                    ("state", s(&self.page_state(i))),
                    (
                        "text",
                        match (&self.texts[i], self.text_failed.get(&i)) {
                            (Some(t), _) if t.truncated => s("incomplete"),
                            (Some(_), _) => s("read"),
                            (None, Some(why)) => s(&format!("failed: {why}")),
                            (None, None) => s("pending"),
                        },
                    ),
                ])
            })
            .collect();
        let outline = self
            .meta
            .outline
            .iter()
            .map(|b: &Bookmark| {
                Json::obj(vec![
                    ("title", s(&b.title)),
                    ("page", b.page.map_or(Json::Null, |p| n(p + 1))),
                    ("depth", n(b.depth)),
                ])
            })
            .collect();
        let drawn = (0..self.page_count())
            .filter(|p| self.rasters[*p].is_some())
            .map(|p| n(p + 1))
            .collect();
        let hits = self
            .find
            .hits
            .iter()
            .map(|h| {
                let text = self.texts[h.page]
                    .as_ref()
                    .map(|t| t.text_of(h.glyphs.clone()))
                    .unwrap_or_default();
                Json::obj(vec![("page", n(h.page + 1)), ("text", s(&text))])
            })
            .collect();
        let links = self
            .links
            .iter()
            .flatten()
            .map(|l| {
                Json::obj(vec![
                    ("page", n(l.page + 1)),
                    ("go", s(&form::link_tip(&l.go, &self.meta.pages))),
                ])
            })
            .collect();
        let notes = self
            .notes
            .iter()
            .flatten()
            .map(|x| {
                Json::obj(vec![
                    ("page", n(x.page + 1)),
                    ("subtype", s(&x.subtype)),
                    ("author", x.author.as_deref().map_or(Json::Null, s)),
                    ("date", x.date.as_deref().map_or(Json::Null, s)),
                    ("contents", s(&x.contents)),
                ])
            })
            .collect();
        let fields = self
            .form
            .fields
            .iter()
            .map(|f| {
                Json::obj(vec![
                    ("name", s(&f.name)),
                    ("kind", s(field_kind_name(f.kind))),
                    ("value", s(&field_value_text(f))),
                    (
                        "on",
                        Json::Arr(f.widgets.iter().map(|w| Json::Bool(w.on)).collect()),
                    ),
                    ("read_only", Json::Bool(f.read_only)),
                    ("required", Json::Bool(f.required)),
                ])
            })
            .collect();
        let values = self
            .form
            .fields
            .iter()
            .map(|f| (f.name.clone(), s(&field_value_text(f))))
            .collect();
        Json::obj(vec![
            ("open", Json::Bool(true)),
            ("file", s(&self.file_name())),
            ("page_count", n(self.page_count())),
            ("page", n(self.current() + 1)),
            ("page_box", s(&self.page_box_text())),
            ("zoom", n((self.zoom * 100.).round() as usize)),
            ("pages", Json::Arr(pages)),
            ("drawn", Json::Arr(drawn)),
            ("outline", Json::Arr(outline)),
            ("outline_shown", Json::Bool(self.outline_shown)),
            (
                "selection",
                self.selection_text().map_or(Json::Null, |t| s(&t)),
            ),
            ("find_open", Json::Bool(self.find.open)),
            ("find_focused", Json::Bool(self.find.field.focused)),
            ("find_query", s(&self.find.field.text)),
            ("find_status", s(&self.find_status())),
            ("find_case", Json::Bool(self.find.case)),
            ("find_whole", Json::Bool(self.find.whole)),
            ("hits", Json::Arr(hits)),
            (
                "current_hit",
                self.find.current.map_or(Json::Null, |k| n(k + 1)),
            ),
            ("copy_pending", Json::Bool(self.copy_waits())),
            ("busy", Json::Bool(self.busy())),
            // Links, comments and the form (#2255).
            ("links", Json::Arr(links)),
            ("notes", Json::Arr(notes)),
            (
                "popup",
                self.popup
                    .and_then(|p| self.notes.get(p.page).and_then(|n| n.get(p.note)))
                    .map_or(Json::Null, |n| s(&n.contents)),
            ),
            (
                "popup_pinned",
                Json::Bool(self.popup.is_some_and(|p| p.pinned)),
            ),
            ("tooltip", self.tooltip().map_or(Json::Null, |t| s(&t))),
            ("fields", Json::Arr(fields)),
            ("values", Json::Obj(values)),
            (
                "focus",
                self.focus
                    .as_ref()
                    .and_then(|f| self.field(f.field))
                    .map_or(Json::Null, |f| s(&f.name)),
            ),
            (
                "focus_open",
                Json::Bool(self.focus.as_ref().is_some_and(|f| f.open)),
            ),
            (
                "unsaved",
                Json::Bool(self.unsaved() || self.fills_waiting()),
            ),
            ("saving", Json::Bool(self.pending_save.is_some())),
            ("signed", Json::Bool(self.form.signed)),
            ("incremental", Json::Bool(self.form.incremental)),
            ("title", self.meta.title.as_deref().map_or(Json::Null, s)),
            ("author", self.meta.author.as_deref().map_or(Json::Null, s)),
            // The password prompt (#2256): never the password typed, nor
            // its length.
            ("locked", Json::Bool(self.lock.is_some())),
            (
                "wrong_password",
                Json::Bool(self.lock.as_ref().is_some_and(|l| l.wrong)),
            ),
            (
                "password_focused",
                Json::Bool(self.lock.as_ref().is_some_and(|l| l.field.focused)),
            ),
            (
                "permissions",
                self.meta.security.as_ref().map_or(Json::Null, |m| {
                    Json::obj(vec![
                        ("method", s(&m.method)),
                        ("owner", Json::Bool(m.owner)),
                        ("print", Json::Bool(m.print)),
                        ("copy", Json::Bool(m.copy)),
                        ("modify", Json::Bool(m.modify)),
                        ("annotate", Json::Bool(m.annotate)),
                        ("fill_forms", Json::Bool(m.fill_forms)),
                        (
                            "restricted",
                            Json::Arr(m.restrictions().into_iter().map(s).collect()),
                        ),
                    ])
                }),
            ),
        ])
    }
}

/// A field's kind as `pdf-read` names it.
fn field_kind_name(k: worker::FieldKind) -> &'static str {
    use worker::FieldKind as K;
    match k {
        K::Text => "text",
        K::Check => "check",
        K::Radio => "radio",
        K::Button => "button",
        K::Combo => "combo",
        K::List => "list",
        K::Signature => "signature",
    }
}

/// A field's value as `pdf-read` reports it: a text field's text, the on
/// state of a check box or radio group ("Off" when none), a choice's picked
/// options' texts joined by ", ".
pub(crate) fn field_value_text(f: &FieldMeta) -> String {
    use worker::FieldKind as K;
    match f.kind {
        K::Check | K::Radio => f
            .widgets
            .iter()
            .find(|w| w.on)
            .and_then(|w| w.on_state.clone())
            .unwrap_or_else(|| "Off".into()),
        K::Combo | K::List if !f.picked.is_empty() => f
            .picked
            .iter()
            .filter_map(|i| f.options.get(*i).map(|o| o.1.clone()))
            .collect::<Vec<_>>()
            .join(", "),
        _ => f.value.join(", "),
    }
}

/// `items` by page, `n` pages (an item on a page past the end is dropped).
fn per_page<T: Clone>(n: usize, items: &[T], page: impl Fn(&T) -> usize) -> Vec<Vec<T>> {
    let mut out = vec![Vec::new(); n];
    for it in items {
        if let Some(v) = out.get_mut(page(it)) {
            v.push(it.clone());
        }
    }
    out
}

/// A raster as gpui's image: one frame of BGRA rows.
pub(crate) fn image_of(r: worker::Raster) -> Option<Arc<RenderImage>> {
    let buf = image::RgbaImage::from_raw(r.w, r.h, r.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

#[cfg(test)]
pub(crate) mod form_tests;
#[cfg(test)]
mod tests;
