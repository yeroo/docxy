//! A read-only document view an extension tab can embed (#1873): a
//! [`docxcore::model::Document`] drawn by the suite's own document renderer
//! in its flow (web layout) style, wrapped to the view's width, with inline
//! pictures from the package it came with. It selects (press, drag,
//! Shift+press, Ctrl+A) and copies (Ctrl+C: text and RTF, as a document copy
//! does), scrolls (wheel, Up/Down, Page Up/Down, Home/End) and zooms
//! (Ctrl+wheel, Ctrl+=, Ctrl+-, Ctrl+0). It never edits and draws no caret.
//! A hyperlink press goes to the extension's callback ([`DocView::on_link`]),
//! never opened here. The chrome (scroll bar, selection) follows the suite's
//! theme; the document is a white page in its authored colours unless the
//! extension turns on dark mode for content ([`DocView::set_content_dark`]).
//!
//! The renderer is the suite's (`block_el`): only its click target differs
//! ([`crate::DocTarget::Embed`]), so a document reads here exactly as in a
//! document tab's Web Layout.

use super::*;
use docxcore::editor::{Caret, Editor};
use docxcore::package::Package;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
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

static HARNESS: AtomicBool = AtomicBool::new(false);

/// Whether this process is a harness instance: a view's copy then goes to
/// the run's private clipboard, as every copy in a harness does (#699).
pub(crate) fn set_harness(on: bool) {
    HARNESS.store(on, Ordering::Relaxed);
}

fn harness() -> bool {
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

/// An embeddable read-only view of a document. Make it with
/// `cx.new(|cx| DocView::new(doc, Some(pkg), cx))` and place it anywhere in
/// the extension's view: it fills its parent. Give its
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
    words: Words,
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The scroll offset of the last render: a Ctrl+wheel puts it back, as
    /// the scroll area has already moved by then.
    held_scroll: Point<Pixels>,
    wheel_acc: f32,
    clipboard: ClipboardStore,
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
            words: Words::default(),
            viewport: Rc::default(),
            held_scroll: Point::default(),
            wheel_acc: 0.,
            clipboard: ClipboardStore::shared(windows::shared_clipboard_text(cx)),
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
        self.run_styles = RefCell::default();
        self.selecting = false;
        self.press_link = None;
        self.scroll.set_offset(Point::default());
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
        self.press_link = link.filter(|_| !mods.modified());
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
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let handled = if m.secondary() && !m.alt && !m.function {
            match key {
                "c" if !m.shift => {
                    self.copy(cx);
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
    fn copy(&mut self, cx: &mut App) {
        let Some((text, rtf)) = copy_payload(&self.editor, self.pkg.as_ref()) else {
            return;
        };
        self.clipboard.write_rich(harness(), text, rtf, cx);
    }

    // ---- what the harness reads -------------------------------------------

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
            ("focused", Json::Bool(self.focus.is_focused(window))),
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
        self.held_scroll = self.scroll.offset();
        let theme_dark = cx.theme().mode.is_dark();
        let (bg, pal) = content_look(
            Pal::of(cx),
            cx.theme().background,
            theme_dark,
            self.content_dark,
        );
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
        let ctx = RenderCtx {
            caret_path: &[],
            caret_off: 0,
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
        };
        let blocks: Vec<AnyElement> = body
            .iter()
            .enumerate()
            .map(|(i, b)| block_el(b, vec![i], markers[i].as_deref(), ctx))
            .collect();
        let viewport = self.viewport.clone();
        // The document's inherited font: the host's theme font, made loadable
        // without touching the host's theme (#1900).
        let ui_font = crate::font_subst::ui_family(&cx.theme().font_family);
        div()
            .id("ext-doc-view")
            .font_family(ui_font)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .min_h(px(0.))
            .bg(bg)
            .on_key_down(cx.listener(Self::key))
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
                    .overflow_y_scroll()
                    .text_color(pal.fg)
                    .px(px(48.))
                    .py(px(28.))
                    .gap_1()
                    .children(blocks),
            )
            .child(
                canvas(
                    move |b: Bounds<Pixels>, _w: &mut Window, _a: &mut App| viewport.set(Some(b)),
                    |_b, _s, _w, _a| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(12.))
                    .child(
                        Scrollbar::vertical(&self.scroll)
                            .id("ext-doc-scrollbar")
                            .scrollbar_show(ScrollbarShow::Scrolling),
                    ),
            )
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

/// The page the document draws on and its palette. A light page (white, in
/// the ink Print Layout's sheet uses) unless dark mode for content is on and
/// the theme is dark: then the theme's own background and colours. The
/// selection is the theme's either way.
pub(crate) fn content_look(
    theme: Pal,
    theme_bg: Hsla,
    theme_dark: bool,
    content_dark: bool,
) -> (Hsla, Pal) {
    if content_dark && theme_dark {
        return (theme_bg, theme);
    }
    (hsla_u(0xffffff), page_pal(0xffffff, theme.sel))
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
        }
    }

    #[test]
    fn content_is_a_white_page_unless_dark_mode_for_content_on_a_dark_theme() {
        let theme = pal(0xeeeeee);
        let theme_bg = hsla_u(0x1e1e1e);
        let ink = hsla_u(design_tab::page_ink(0xffffff).0);
        // Light content on a dark theme: the white page, its ink, the
        // theme's selection.
        let (bg, p) = content_look(theme, theme_bg, true, false);
        assert_eq!(bg, hsla_u(0xffffff));
        assert_eq!(p.fg, ink);
        assert_eq!(p.sel, hsla_u(0x123456));
        // Dark mode for content on a dark theme: the theme's own.
        let (bg, p) = content_look(theme, theme_bg, true, true);
        assert_eq!(bg, theme_bg);
        assert_eq!(p.fg, hsla_u(0xeeeeee));
        // On a light theme it changes nothing.
        let (bg, p) = content_look(theme, theme_bg, false, true);
        assert_eq!(bg, hsla_u(0xffffff));
        assert_eq!(p.fg, ink);
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
