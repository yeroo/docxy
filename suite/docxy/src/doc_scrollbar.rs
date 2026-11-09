//! The document views' scrollbars (#2080): Print Layout, Web Layout / Draft
//! and the embedded [`crate::doc_view::DocView`] draw these instead of
//! gpui-component's fading bars, as Word does. A bar is an arrow square at each end
//! with the track between. The thumb's length is the visible fraction of
//! the content. A press on the track pages toward the press, an arrow press
//! scrolls one line, and both repeat while held. Dragging the thumb maps the
//! pointer's travel back to the offset. In Print Layout a tip beside the
//! vertical thumb reads `Page N of M` while it is dragged.
//!
//! The geometry is plain `f32` arithmetic and unit-tested here. The element
//! is a canvas that reads the [`ScrollHandle`] while it prepaints and paints.
//! Laid out after the scroll area it serves, it sees this frame's offset and
//! extent, so a wheel scroll moves the thumb without a render of its own.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Bounds, CursorStyle, Hitbox, HitboxBehavior, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, ScrollHandle, ScrollWheelEvent,
    SharedString, Task, TextAlign, TextRun, Window, canvas, fill, point, px, size,
};

/// A bar's thickness, px.
pub(crate) const BAR_PX: f32 = 12.;
/// One arrow press (and one step of a held one), px.
pub(crate) const LINE: f32 = 40.;
/// The shortest thumb, px (shorter only when the track is).
pub(crate) const MIN_THUMB: f32 = 20.;
/// A held press first repeats after this long, then every `REPEAT_EVERY`.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(50);

/// One axis of a scroll area: the view's length, the content's, and how far
/// it is scrolled (`>= 0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Axis {
    pub(crate) view: f32,
    pub(crate) content: f32,
    pub(crate) offset: f32,
}

impl Axis {
    /// How far the content can scroll.
    pub(crate) fn max_offset(&self) -> f32 {
        (self.content - self.view).max(0.)
    }

    /// Whether the content is longer than the view: the bar shows. Half a
    /// pixel of slack for layout rounding.
    pub(crate) fn overflows(&self) -> bool {
        self.max_offset() > 0.5
    }

    fn clamp(&self, offset: f32) -> f32 {
        offset.clamp(0., self.max_offset())
    }
}

/// The thumb on a `track` px long track: its start from the track's start,
/// and its length (the visible fraction of the track, at least
/// [`MIN_THUMB`]). The whole track when nothing scrolls.
pub(crate) fn thumb(axis: Axis, track: f32) -> (f32, f32) {
    let track = track.max(0.);
    if axis.content <= 0. || axis.max_offset() <= 0. {
        return (0., track);
    }
    let len = (track * axis.view / axis.content).clamp(MIN_THUMB.min(track), track);
    let start = (track - len) * axis.clamp(axis.offset) / axis.max_offset();
    (start, len)
}

/// The offset that puts the thumb's start at `start` (from the track's
/// start): [`thumb`]'s inverse, clamped to the content.
pub(crate) fn offset_for_thumb(axis: Axis, track: f32, start: f32) -> f32 {
    let (_, len) = thumb(axis, track);
    let room = track - len;
    if room <= 0. {
        // The thumb fills the track: no travel maps to an offset, so it
        // stays where it is.
        return axis.clamp(axis.offset);
    }
    (start / room).clamp(0., 1.) * axis.max_offset()
}

/// The offset a thumb drag reaches when it began at `start_offset` and the
/// pointer has since moved `delta` px along the track.
pub(crate) fn drag_offset(axis: Axis, track: f32, start_offset: f32, delta: f32) -> f32 {
    let at_start = Axis {
        offset: start_offset,
        ..axis
    };
    let (start, _) = thumb(at_start, track);
    offset_for_thumb(axis, track, start + delta)
}

/// One page of a track press: the view less one line, as Word keeps a line
/// of context.
pub(crate) fn page_step(view: f32) -> f32 {
    (view - LINE).max(LINE)
}

/// The offset a press at `at` (px from the track's start) on the track
/// scrolls to: one page toward the press. `None` when the press is on the
/// thumb or the content is already at that end. A held press repeats this
/// step until it answers `None`, that is until the thumb is under the
/// pointer.
pub(crate) fn track_press(axis: Axis, track: f32, at: f32) -> Option<f32> {
    let (start, len) = thumb(axis, track);
    let to = if at < start {
        axis.offset - page_step(axis.view)
    } else if at >= start + len {
        axis.offset + page_step(axis.view)
    } else {
        return None;
    };
    moved(axis, axis.clamp(to))
}

/// The offset an arrow press scrolls to: one [`LINE`] back (`forward`
/// false) or on. `None` at that end.
pub(crate) fn arrow_press(axis: Axis, forward: bool) -> Option<f32> {
    let step = if forward { LINE } else { -LINE };
    moved(axis, axis.clamp(axis.offset + step))
}

fn moved(axis: Axis, to: f32) -> Option<f32> {
    ((to - axis.offset).abs() > 0.01).then_some(to)
}

/// Which of Print Layout's bar strips to lay out (vertical, horizontal),
/// decided together (#2080 r2) from the content's size and the view's
/// without any strip (`bare`). The two strips shrink each other's room, so
/// deciding each on its own from the other's last state could swap them
/// forever (content just under the bare view on both axes). The vertical
/// strip goes when the content is taller than the view, the horizontal one
/// when it is wider than the view beside that strip, and the vertical one
/// also when the content is taller than the view above that one. Half a
/// pixel of slack.
pub(crate) fn bars_needed(content: (f32, f32), bare: (f32, f32)) -> (bool, bool) {
    let over = |c: f32, view: f32| c > view + 0.5;
    let beside = |on: bool| if on { BAR_PX } else { 0. };
    let v = over(content.1, bare.1);
    let h = over(content.0, bare.0 - beside(v));
    let v = v || over(content.1, bare.1 - beside(h));
    (v, h)
}

/// The strips the next frame lays out, from what this frame measured: the
/// strips `shown`, the scroll area's `view` with them and how far it can
/// scroll (`max`). Content that overflows the view is known exactly (view
/// plus max); content that fits is known only to fit, and counts as none,
/// so a strip it might not need waits until a frame shows it is needed.
pub(crate) fn strips_next(shown: (bool, bool), view: (f32, f32), max: (f32, f32)) -> (bool, bool) {
    let with = |on: bool| if on { BAR_PX } else { 0. };
    let bare = (view.0 + with(shown.0), view.1 + with(shown.1));
    let known = |view: f32, max: f32| if max > 0.5 { view + max } else { 0. };
    bars_needed((known(view.0, max.0), known(view.1, max.1)), bare)
}

/// The part of a scroll area's `view` a caret follow keeps a line in: all
/// of it, less the overlaid horizontal bar along its bottom while that bar
/// is shown (#2080 r3). Print Layout's bars are beside its view.
pub(crate) fn clear_of_hbar(view: Bounds<Pixels>, hbar_shown: bool) -> Bounds<Pixels> {
    if !hbar_shown {
        return view;
    }
    let h = (view.size.height - px(BAR_PX)).max(px(0.));
    Bounds::new(view.origin, size(view.size.width, h))
}

/// Print Layout's strip decision (#2080 r2), drawn after its scroll area:
/// its prepaint applies [`strips_next`] to both bars' `shown` and asks for
/// a frame when that changed.
pub(crate) fn strip_decider(handle: &ScrollHandle, vbar: &Bar, hbar: &Bar) -> gpui::AnyElement {
    use gpui::{IntoElement, Styled};
    let (handle, vbar, hbar) = (handle.clone(), vbar.clone(), hbar.clone());
    canvas(
        move |_b, window: &mut Window, _cx: &mut App| {
            let (view, max) = (handle.bounds().size, handle.max_offset());
            let now = (vbar.shown(), hbar.shown());
            let next = strips_next(
                now,
                (f32::from(view.width), f32::from(view.height)),
                (f32::from(max.x).max(0.), f32::from(max.y).max(0.)),
            );
            if next != now {
                vbar.shown.set(next.0);
                hbar.shown.set(next.1);
                window.request_animation_frame();
            }
        },
        |_b, _s, _w, _a| {},
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// Where a bar is drawn.
#[derive(Clone)]
pub(crate) enum Place {
    /// Over its scroll area's padding (the flow view, DocView): it decides
    /// its own `shown` from its axis, as it never changes the layout.
    Overlay,
    /// In a strip of its own beside the scroll area (Print Layout), whose
    /// `shown` [`strip_decider`] keeps: a wheel over the strip scrolls the
    /// area, which is not under it. `pages`: the vertical bar's page tip.
    Strip { pages: Option<Pages> },
}

/// The page at the top of the view at `offset`: `pages` are each page's
/// top and bottom in content coordinates (the scroll area's unscrolled
/// top is 0), in order. The first page whose bottom is below the view's
/// top, so a gap between two pages counts for the next one. The last page
/// at the end of the content (`offset` at `max`). `None` with no pages.
pub(crate) fn page_at(pages: &[(f32, f32)], offset: f32, max: f32) -> Option<usize> {
    let last = pages.len().checked_sub(1)?;
    if max > 0. && offset >= max - 0.5 {
        return Some(last);
    }
    Some(
        pages
            .iter()
            .position(|&(_, bottom)| bottom > offset + 1.)
            .unwrap_or(last),
    )
}

/// The tip a dragged Print Layout thumb shows: `n` is 0-based.
pub(crate) fn page_tip(n: usize, m: usize) -> String {
    format!("Page {} of {}", n + 1, m)
}

/// Which way a bar runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dir {
    Vertical,
    Horizontal,
}

impl Dir {
    fn along(self, p: Point<Pixels>) -> f32 {
        f32::from(match self {
            Dir::Vertical => p.y,
            Dir::Horizontal => p.x,
        })
    }

    /// The handle's axis this bar scrolls.
    fn axis(self, handle: &ScrollHandle) -> Axis {
        let (view, max, offset) = match self {
            Dir::Vertical => (
                handle.bounds().size.height,
                handle.max_offset().y,
                handle.offset().y,
            ),
            Dir::Horizontal => (
                handle.bounds().size.width,
                handle.max_offset().x,
                handle.offset().x,
            ),
        };
        let (view, max) = (f32::from(view), f32::from(max).max(0.));
        Axis {
            view,
            content: view + max,
            // gpui clamps a wheel's offset only as the area prepaints.
            offset: (-f32::from(offset)).clamp(0., max),
        }
    }

    /// Scroll `handle` to `offset` on this axis, the other kept.
    fn set(self, handle: &ScrollHandle, offset: f32) {
        let now = handle.offset();
        handle.set_offset(match self {
            Dir::Vertical => point(now.x, px(-offset)),
            Dir::Horizontal => point(px(-offset), now.y),
        });
    }

    /// The names of the back and forward arrows' regions.
    fn arrows(self) -> (&'static str, &'static str) {
        match self {
            Dir::Vertical => ("up", "down"),
            Dir::Horizontal => ("left", "right"),
        }
    }
}

/// What a bar drew in its last frame, for the harness: the track's length
/// and the thumb on it, px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Painted {
    pub(crate) track: f32,
    pub(crate) thumb_start: f32,
    pub(crate) thumb_len: f32,
}

/// A bar's state across frames. The tab views keep theirs on `Docxy`, the
/// embedded view on its `DocView`.
#[derive(Default)]
pub(crate) struct BarState {
    /// Whether the bar is shown. An overlay bar sets it each frame from
    /// its own axis; in Print Layout [`strip_decider`] sets both bars'
    /// together for the next frame, and the strip is laid out only while
    /// it is set.
    shown: Cell<bool>,
    /// A thumb drag: the offset and the pointer (along the bar) at the press.
    drag: Cell<Option<(f32, f32)>>,
    /// The pointer is over the thumb.
    hover: Cell<bool>,
    /// The repeat of a held arrow or track press; dropping it stops it.
    repeat: RefCell<Option<Task<()>>>,
    painted: Cell<Option<Painted>>,
    /// The bar's regions as last painted (the strip `""`, `thumb`, and the
    /// arrows), in window coordinates, while it is shown.
    regions: RefCell<Vec<(&'static str, Bounds<Pixels>)>>,
    /// The tip shown beside a dragged Print Layout thumb.
    tip: RefCell<Option<String>>,
}

impl BarState {
    pub(crate) fn shown(&self) -> bool {
        self.shown.get()
    }

    pub(crate) fn painted(&self) -> Option<Painted> {
        self.painted.get()
    }

    pub(crate) fn tip(&self) -> Option<String> {
        self.tip.borrow().clone()
    }

    /// The region `part` (`""` for the whole strip, `thumb`, `up` / `down`
    /// or `left` / `right`) as last painted.
    pub(crate) fn region(&self, part: &str) -> Option<Bounds<Pixels>> {
        self.regions
            .borrow()
            .iter()
            .find(|(p, _)| *p == part)
            .map(|(_, b)| *b)
    }
}

pub(crate) type Bar = Rc<BarState>;

/// A bar for the harness (#2080): `null` while it is not shown, else its
/// `track` length and the thumb on it (`thumb_start` from the track's
/// start, `thumb_len`), px.
pub(crate) fn bar_json(bar: &BarState) -> ctlcore::json::Json {
    use ctlcore::json::Json;
    let num = |v: f32| Json::Num(f64::from(v.round()));
    bar.painted().map_or(Json::Null, |p| {
        Json::obj(vec![
            ("track", num(p.track)),
            ("thumb_start", num(p.thumb_start)),
            ("thumb_len", num(p.thumb_len)),
        ])
    })
}

/// The bar region `part` names among a view's two bars: `vbar` / `hbar`,
/// alone (the strip) or with `-thumb`, `-up` / `-down` or `-left` /
/// `-right`, as last painted.
pub(crate) fn bar_region(
    vbar: &BarState,
    hbar: &BarState,
    part: &str,
) -> Result<Bounds<Pixels>, String> {
    let (bar, rest, arrows) = if let Some(rest) = part.strip_prefix("vbar") {
        (vbar, rest, ["up", "down"])
    } else if let Some(rest) = part.strip_prefix("hbar") {
        (hbar, rest, ["left", "right"])
    } else {
        return Err(format!("no scrollbar part '{part}'"));
    };
    let piece = match rest.strip_prefix('-') {
        None if rest.is_empty() => "",
        Some(p) if p == "thumb" || arrows.contains(&p) => p,
        _ => {
            return Err(format!(
                "no scrollbar part '{part}' (the bar, -thumb, -{}, -{})",
                arrows[0], arrows[1]
            ));
        }
    };
    bar.region(piece)
        .ok_or_else(|| format!("the scrollbar is not shown: its axis fits the view ({part})"))
}

/// The colours a bar paints in, from the theme.
#[derive(Clone, Copy)]
pub(crate) struct BarColors {
    track: Hsla,
    thumb: Hsla,
    thumb_hover: Hsla,
    arrow: Hsla,
    tip_bg: Hsla,
    tip_fg: Hsla,
    tip_border: Hsla,
}

impl BarColors {
    pub(crate) fn of(cx: &App) -> Self {
        use gpui_component::ActiveTheme;
        let t = cx.theme();
        BarColors {
            track: t.scrollbar,
            thumb: t.scrollbar_thumb,
            thumb_hover: t.scrollbar_thumb_hover,
            arrow: t.muted_foreground,
            tip_bg: t.popover,
            tip_fg: t.popover_foreground,
            tip_border: t.border,
        }
    }
}

impl BarColors {
    /// The track's colour, which the corner square between two bars takes.
    pub(crate) fn track(&self) -> Hsla {
        self.track
    }
}

/// A flow view's bars (#2080): laid over the right and bottom edges of the
/// scroll area's parent, which must be `relative`, so they take no text
/// width. They cover the view's edges, which are padding only at the ends
/// of the scroll: a caret follow keeps clear of the horizontal one
/// ([`clear_of_hbar`]). Each paints only while its axis overflows; the
/// vertical one stops above the horizontal one while both show.
pub(crate) fn overlay_bars(
    handle: &ScrollHandle,
    vbar: &Bar,
    hbar: &Bar,
    colors: BarColors,
) -> [gpui::AnyElement; 2] {
    use gpui::{IntoElement, ParentElement, Styled, div};
    let (v, h) = (vbar.shown(), hbar.shown());
    let end = |other: bool| px(if other { BAR_PX } else { 0. });
    [
        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom(end(h))
            .w(px(BAR_PX))
            .child(doc_scrollbar(
                handle,
                Dir::Vertical,
                vbar,
                Place::Overlay,
                colors,
            ))
            .into_any_element(),
        div()
            .absolute()
            .left_0()
            .bottom_0()
            .right(end(v))
            .h(px(BAR_PX))
            .child(doc_scrollbar(
                handle,
                Dir::Horizontal,
                hbar,
                Place::Overlay,
                colors,
            ))
            .into_any_element(),
    ]
}

/// Each page's top and bottom in a scroll area's content coordinates.
pub(crate) type PageSpans = Rc<dyn Fn(&ScrollHandle) -> Vec<(f32, f32)>>;

/// One step of a held press: the offset it scrolls to, `None` once it stops.
type Step = Rc<dyn Fn(&ScrollHandle) -> Option<f32>>;

/// The page tip's source: each page's top and bottom in the scroll
/// area's content coordinates (its unscrolled top is 0), as last laid out.
#[derive(Clone)]
pub(crate) struct Pages {
    pub(crate) spans: PageSpans,
}

impl Pages {
    /// The page at the top of `handle`'s view now, 0-based, and the count.
    pub(crate) fn top(&self, handle: &ScrollHandle) -> Option<(usize, usize)> {
        let spans = (self.spans)(handle);
        let axis = Dir::Vertical.axis(handle);
        page_at(&spans, axis.offset, axis.max_offset()).map(|n| (n, spans.len()))
    }
}

/// The geometry of one frame of a bar.
#[derive(Clone, Copy)]
struct Frame {
    strip: Bounds<Pixels>,
    back: Bounds<Pixels>,
    forward: Bounds<Pixels>,
    /// The track's start along the bar (window coordinates) and length.
    track_at: f32,
    track: f32,
    thumb: Bounds<Pixels>,
}

fn frame(dir: Dir, strip: Bounds<Pixels>, axis: Axis) -> Frame {
    let o = strip.origin;
    let (w, h) = (f32::from(strip.size.width), f32::from(strip.size.height));
    let long = if dir == Dir::Vertical { h } else { w };
    // Arrows are squares, shrunk when the strip is too short for both.
    let arrow = BAR_PX.min(long / 2.);
    let track = (long - 2. * arrow).max(0.);
    let (start, len) = thumb(axis, track);
    let along = |at: f32, len: f32| match dir {
        Dir::Vertical => Bounds::new(point(o.x, o.y + px(at)), size(strip.size.width, px(len))),
        Dir::Horizontal => Bounds::new(point(o.x + px(at), o.y), size(px(len), strip.size.height)),
    };
    Frame {
        strip,
        back: along(0., arrow),
        forward: along(long - arrow, arrow),
        track_at: dir.along(o) + arrow,
        track,
        thumb: along(arrow + start, len),
    }
}

/// A bar for `handle`'s `dir` axis, filling its parent. It paints nothing
/// and takes no input while that axis fits.
pub(crate) fn doc_scrollbar(
    handle: &ScrollHandle,
    dir: Dir,
    state: &Bar,
    place: Place,
    colors: BarColors,
) -> gpui::AnyElement {
    use gpui::{IntoElement, Styled};
    let (prep_handle, prep_state, prep_place) = (handle.clone(), state.clone(), place.clone());
    let (handle, state) = (handle.clone(), state.clone());
    canvas(
        move |strip: Bounds<Pixels>, window: &mut Window, _cx: &mut App| {
            let axis = dir.axis(&prep_handle);
            let shows = axis.overflows();
            // An overlay keeps its own `shown`. `overlay_bars` reads it to
            // stop the other bar short of the corner, the caret follow to
            // keep clear of the horizontal bar, and `print_bars` as its
            // starting point after a view switch (the pair is shared). The
            // harness reads `painted` / `regions`, not `shown`. A frame
            // shows the change.
            if matches!(prep_place, Place::Overlay) && prep_state.shown.replace(shows) != shows {
                window.request_animation_frame();
            }
            if !shows || strip.size.width <= px(0.) || strip.size.height <= px(0.) {
                prep_state.painted.set(None);
                prep_state.regions.borrow_mut().clear();
                if prep_state.drag.take().is_some() {
                    prep_state.tip.replace(None);
                }
                return None;
            }
            let f = frame(dir, strip, axis);
            prep_state.painted.set(Some(Painted {
                track: f.track,
                thumb_start: dir.along(f.thumb.origin) - f.track_at,
                thumb_len: f32::from(match dir {
                    Dir::Vertical => f.thumb.size.height,
                    Dir::Horizontal => f.thumb.size.width,
                }),
            }));
            let (b, fw) = dir.arrows();
            *prep_state.regions.borrow_mut() = vec![
                ("", strip),
                ("thumb", f.thumb),
                (b, f.back),
                (fw, f.forward),
            ];
            Some((f, window.insert_hitbox(strip, HitboxBehavior::Normal)))
        },
        move |_b, prep: Option<(Frame, Hitbox)>, window: &mut Window, cx: &mut App| {
            let Some((f, hitbox)) = prep else { return };
            paint_bar(dir, &f, &state, colors, window);
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);
            if let Place::Strip {
                pages: Some(pages), ..
            } = &place
            {
                paint_tip(&f, &handle, &state, pages, colors, window, cx);
            }
            let wheel = matches!(place, Place::Strip { .. });
            listen(dir, f, hitbox, &handle, &state, wheel, window);
        },
    )
    .size_full()
    .into_any_element()
}

fn paint_bar(dir: Dir, f: &Frame, state: &BarState, colors: BarColors, window: &mut Window) {
    window.paint_quad(fill(f.strip, colors.track));
    let thumb = if state.hover.get() || state.drag.get().is_some() {
        colors.thumb_hover
    } else {
        colors.thumb
    };
    // Inset across the bar so the thumb reads as a bar in the track.
    let inset = px(2.);
    let t = f.thumb;
    let thumb_box = match dir {
        Dir::Vertical => Bounds::new(
            point(t.origin.x + inset, t.origin.y),
            size(t.size.width - inset * 2., t.size.height),
        ),
        Dir::Horizontal => Bounds::new(
            point(t.origin.x, t.origin.y + inset),
            size(t.size.width, t.size.height - inset * 2.),
        ),
    };
    window.paint_quad(fill(thumb_box, thumb).corner_radii(px(3.)));
    paint_arrow(dir, f.back, false, colors.arrow, window);
    paint_arrow(dir, f.forward, true, colors.arrow, window);
}

/// A small filled triangle in `b`, pointing back (up / left) or forward.
fn paint_arrow(dir: Dir, b: Bounds<Pixels>, forward: bool, color: Hsla, window: &mut Window) {
    let c = b.center();
    let r = px((f32::from(b.size.width.min(b.size.height)) / 4.).max(1.));
    let s = if forward { r } else { -r };
    let pts = match dir {
        Dir::Vertical => [
            point(c.x - r, c.y - s / 2.),
            point(c.x + r, c.y - s / 2.),
            point(c.x, c.y + s / 2.),
        ],
        Dir::Horizontal => [
            point(c.x - s / 2., c.y - r),
            point(c.x - s / 2., c.y + r),
            point(c.x + s / 2., c.y),
        ],
    };
    let mut path = PathBuilder::fill();
    path.move_to(pts[0]);
    path.line_to(pts[1]);
    path.line_to(pts[2]);
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// The `Page N of M` tip left of the thumb while it is dragged.
fn paint_tip(
    f: &Frame,
    handle: &ScrollHandle,
    state: &BarState,
    pages: &Pages,
    colors: BarColors,
    window: &mut Window,
    cx: &mut App,
) {
    let tip = match state.drag.get() {
        Some(_) => pages.top(handle).map(|(n, m)| page_tip(n, m)),
        None => None,
    };
    state.tip.replace(tip.clone());
    let Some(tip) = tip else { return };
    let style = window.text_style();
    let font_size = px(12.);
    let run = TextRun {
        len: tip.len(),
        font: style.font(),
        color: colors.tip_fg,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(tip), font_size, &[run], None);
    let (pad, line_h) = (px(6.), px(18.));
    let w = line.width + pad * 2.;
    let h = line_h + px(4.);
    let mid = f.thumb.center().y;
    let origin = point(f.strip.left() - px(4.) - w, mid - h / 2.);
    let b = Bounds::new(origin, size(w, h));
    window.paint_quad(
        fill(b, colors.tip_bg)
            .border_color(colors.tip_border)
            .border_widths(px(1.))
            .corner_radii(px(3.)),
    );
    let _ = line.paint(
        point(origin.x + pad, origin.y + px(2.)),
        line_h,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// The bar's input: presses on its parts, a thumb drag that follows the
/// pointer anywhere until the button goes up, and the thumb's hover.
fn listen(
    dir: Dir,
    f: Frame,
    hitbox: Hitbox,
    handle: &ScrollHandle,
    state: &Bar,
    wheel: bool,
    window: &mut Window,
) {
    let view = window.current_view();
    if wheel {
        // A strip lies beside its scroll area, outside it: a wheel over it
        // scrolls the area as a wheel over the pages does (an overlay sits
        // on the area, which scrolls already).
        let (handle, hitbox) = (handle.clone(), hitbox.clone());
        window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
            if !phase.bubble() || !hitbox.should_handle_scroll(window) {
                return;
            }
            let delta = ev.delta.pixel_delta(window.line_height());
            let (now, max) = (handle.offset(), handle.max_offset());
            let to = point(
                (now.x + delta.x).clamp(-max.x.max(px(0.)), px(0.)),
                (now.y + delta.y).clamp(-max.y.max(px(0.)), px(0.)),
            );
            if to != now {
                handle.set_offset(to);
                cx.notify(view);
            }
            cx.stop_propagation();
        });
    }
    {
        let (handle, state, hitbox) = (handle.clone(), state.clone(), hitbox.clone());
        window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
            if !phase.bubble() || ev.button != MouseButton::Left || !hitbox.is_hovered(window) {
                return;
            }
            cx.stop_propagation();
            let axis = dir.axis(&handle);
            let at = dir.along(ev.position);
            if f.thumb.contains(&ev.position) {
                state.drag.set(Some((axis.offset, at)));
            } else {
                // An arrow scrolls a line, the track a page toward the
                // press (from where the track starts); both repeat while
                // held, the track's until the thumb reaches the press.
                let step: Step = if f.back.contains(&ev.position) {
                    Rc::new(move |h| arrow_press(dir.axis(h), false))
                } else if f.forward.contains(&ev.position) {
                    Rc::new(move |h| arrow_press(dir.axis(h), true))
                } else {
                    let (track, from) = (f.track, at - f.track_at);
                    Rc::new(move |h| track_press(dir.axis(h), track, from))
                };
                if let Some(to) = step(&handle) {
                    dir.set(&handle, to);
                }
                let handle = handle.clone();
                let task = window.spawn(cx, async move |cx| {
                    let mut wait = REPEAT_DELAY;
                    loop {
                        cx.background_executor().timer(wait).await;
                        wait = REPEAT_EVERY;
                        let Some(to) = step(&handle) else { break };
                        dir.set(&handle, to);
                        if cx.update(|_, cx| cx.notify(view)).is_err() {
                            break;
                        }
                    }
                });
                state.repeat.replace(Some(task));
            }
            cx.notify(view);
        });
    }
    {
        let (handle, state) = (handle.clone(), state.clone());
        window.on_mouse_event(move |ev: &MouseMoveEvent, _phase, _window, cx| {
            if let Some((from_offset, from)) = state.drag.get() {
                if ev.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                cx.stop_propagation();
                let axis = dir.axis(&handle);
                let to = drag_offset(axis, f.track, from_offset, dir.along(ev.position) - from);
                if (to - axis.offset).abs() > 0.01 {
                    dir.set(&handle, to);
                }
                cx.notify(view);
                return;
            }
            let over = f.thumb.contains(&ev.position);
            if state.hover.replace(over) != over {
                cx.notify(view);
            }
        });
    }
    {
        let state = state.clone();
        window.on_mouse_event(move |ev: &MouseUpEvent, _phase, _window, cx| {
            if ev.button != MouseButton::Left {
                return;
            }
            let dragged = state.drag.take().is_some();
            let held = state.repeat.borrow_mut().take().is_some();
            if dragged || held {
                state.tip.replace(None);
                cx.notify(view);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn axis(view: f32, content: f32, offset: f32) -> Axis {
        Axis {
            view,
            content,
            offset,
        }
    }

    /// #2080: the thumb is the visible fraction of the track, placed by
    /// the offset's share of the scrollable length.
    #[test]
    fn thumb_is_the_visible_fraction() {
        assert_eq!(thumb(axis(500., 2000., 0.), 400.), (0., 100.));
        assert_eq!(thumb(axis(500., 2000., 750.), 400.), (150., 100.));
        assert_eq!(thumb(axis(500., 2000., 1500.), 400.), (300., 100.));
        // Past the end (a wheel not yet clamped): at the end.
        assert_eq!(thumb(axis(500., 2000., 9000.), 400.), (300., 100.));
    }

    #[test]
    fn thumb_has_a_minimum() {
        // 400 * 500 / 100000 = 2 px: raised to the minimum.
        let (start, len) = thumb(axis(500., 100_000., 99_500.), 400.);
        assert_eq!(len, MIN_THUMB);
        assert_eq!(start, 400. - MIN_THUMB);
        // A track shorter than the minimum: the whole track.
        assert_eq!(thumb(axis(500., 100_000., 0.), 10.), (0., 10.));
    }

    #[test]
    fn no_overflow_fills_the_track() {
        let a = axis(500., 500., 0.);
        assert!(!a.overflows());
        assert_eq!(thumb(a, 400.), (0., 400.));
        assert!(!axis(500., 500.4, 0.).overflows());
        assert!(axis(500., 501., 0.).overflows());
        assert_eq!(offset_for_thumb(a, 400., 50.), 0.);
    }

    #[test]
    fn thumb_drag_maps_back_to_offset() {
        let a = axis(500., 2000., 0.);
        // Each pixel of thumb travel is 1500 / 300 = 5 px of content.
        assert_eq!(drag_offset(a, 400., 0., 30.), 150.);
        assert_eq!(drag_offset(a, 400., 750., -30.), 600.);
        // Round trip: the offset a thumb position gives puts it back there.
        for off in [0., 123., 750., 1500.] {
            let (start, _) = thumb(axis(500., 2000., off), 400.);
            assert!((offset_for_thumb(a, 400., start) - off).abs() < 0.01);
        }
        // Clamped at both ends.
        assert_eq!(drag_offset(a, 400., 0., -100.), 0.);
        assert_eq!(drag_offset(a, 400., 1400., 1000.), 1500.);
    }

    /// #2080 r3: a caret follow keeps a line clear of the horizontal
    /// overlay bar while it is shown, and uses the whole view otherwise.
    #[test]
    fn a_follow_keeps_clear_of_the_horizontal_bar() {
        let view = Bounds::new(point(px(10.), px(100.)), size(px(600.), px(400.)));
        assert_eq!(clear_of_hbar(view, false), view);
        let clear = clear_of_hbar(view, true);
        assert_eq!(clear.origin, view.origin);
        assert_eq!(clear.size, size(px(600.), px(400. - BAR_PX)));
        assert_eq!(clear.bottom(), px(500. - BAR_PX));
        // A view shorter than the bar: nothing left, never negative.
        let tiny = Bounds::new(point(px(0.), px(0.)), size(px(600.), px(5.)));
        assert_eq!(clear_of_hbar(tiny, true).size.height, px(0.));
    }

    /// #2080 r2: a thumb that fills a track too short for it does not
    /// jump to the start when dragged; it stays.
    #[test]
    fn a_thumb_with_no_room_keeps_the_offset() {
        let a = axis(500., 2000., 750.);
        // A 15 px track: the thumb is the whole track.
        assert_eq!(thumb(a, 15.), (0., 15.));
        assert_eq!(offset_for_thumb(a, 15., 0.), 750.);
        assert_eq!(drag_offset(a, 15., 750., 5.), 750.);
        assert_eq!(drag_offset(a, 15., 750., -5.), 750.);
        // A wheel past the end, not yet clamped: clamped.
        assert_eq!(offset_for_thumb(axis(500., 2000., 9000.), 15., 0.), 1500.);
    }

    /// #2080 r2: Print Layout's two strips are decided together from the
    /// view without strips.
    #[test]
    fn strips_are_decided_together() {
        let (w, h) = (600., 400.);
        // Neither: the content fits the bare view.
        assert_eq!(bars_needed((500., 300.), (w, h)), (false, false));
        assert_eq!(bars_needed((w, h), (w, h)), (false, false));
        // Just under the bare view on both axes (the swap r2 traced):
        // neither strip, as neither axis overflows without them.
        assert_eq!(bars_needed((w - 5., h - 5.), (w, h)), (false, false));
        // Taller only, and narrow enough beside the vertical strip.
        assert_eq!(bars_needed((500., 2000.), (w, h)), (true, false));
        // Taller, and wider than the room beside the vertical strip: both.
        assert_eq!(bars_needed((w - 5., 2000.), (w, h)), (true, true));
        // Wider only, and short enough above the horizontal strip.
        assert_eq!(bars_needed((900., 300.), (w, h)), (false, true));
        // Wider, and taller than the room above the horizontal strip: both.
        assert_eq!(bars_needed((900., h - 5.), (w, h)), (true, true));
        assert_eq!(bars_needed((900., 2000.), (w, h)), (true, true));
    }

    /// #2080 r2: frame after frame, measuring the scroll area as the
    /// strips leave it, the decision settles within three frames on
    /// [`bars_needed`]'s answer and stays there, from every start and for
    /// content around every edge (no endless swap).
    #[test]
    fn the_strips_settle_from_any_start() {
        let (w, h) = (600., 400.);
        let b = BAR_PX;
        let sizes = |bare: f32| {
            [
                bare / 2.,
                bare - b - 3.,
                bare - b,
                bare - b + 3.,
                bare - 3.,
                bare,
                bare + 3.,
                bare * 3.,
            ]
        };
        let starts = [(false, false), (true, false), (false, true), (true, true)];
        for cw in sizes(w) {
            for ch in sizes(h) {
                let want = bars_needed((cw, ch), (w, h));
                for start in starts {
                    let mut shown = start;
                    let mut frames = 0;
                    loop {
                        let view = (
                            w - if shown.0 { b } else { 0. },
                            h - if shown.1 { b } else { 0. },
                        );
                        // The centring column is at least the view's width.
                        let max = ((cw - view.0).max(0.), (ch - view.1).max(0.));
                        let next = strips_next(shown, view, max);
                        if next == shown {
                            break;
                        }
                        shown = next;
                        frames += 1;
                        assert!(frames <= 3, "{cw}x{ch} from {start:?}: no settling");
                    }
                    assert_eq!(shown, want, "{cw}x{ch} from {start:?}");
                }
            }
        }
    }

    #[test]
    fn track_press_pages_toward_the_press() {
        // Thumb at 150..250 on a 400 px track; a page is 500 - 40.
        let a = axis(500., 2000., 750.);
        assert_eq!(page_step(500.), 460.);
        assert_eq!(track_press(a, 400., 20.), Some(290.));
        assert_eq!(track_press(a, 400., 300.), Some(1210.));
        // On the thumb: nothing.
        assert_eq!(track_press(a, 400., 150.), None);
        assert_eq!(track_press(a, 400., 249.), None);
        // Clamped at the ends.
        assert_eq!(track_press(axis(500., 2000., 100.), 400., 0.), Some(0.));
        assert_eq!(
            track_press(axis(500., 2000., 1400.), 400., 399.),
            Some(1500.)
        );
        // A tiny view still steps a line.
        assert_eq!(page_step(10.), LINE);
    }

    /// A held track press repeats [`track_press`] until the thumb is under
    /// the pointer, and then stops.
    #[test]
    fn a_held_track_press_stops_under_the_pointer() {
        let mut a = axis(500., 5000., 0.);
        let (track, at) = (400., 300.);
        let mut steps = 0;
        while let Some(to) = track_press(a, track, at) {
            a.offset = to;
            steps += 1;
            assert!(steps < 100);
        }
        let (start, len) = thumb(a, track);
        assert!(start <= at && at < start + len, "{start} {len}");
        assert!(steps > 1);
        // A held press at the end of the content stops there.
        let end = axis(500., 2000., 1500.);
        assert_eq!(track_press(end, 400., 399.), None);
    }

    #[test]
    fn arrows_scroll_a_line() {
        let a = axis(500., 2000., 100.);
        assert_eq!(arrow_press(a, true), Some(140.));
        assert_eq!(arrow_press(a, false), Some(60.));
        assert_eq!(arrow_press(axis(500., 2000., 20.), false), Some(0.));
        assert_eq!(arrow_press(axis(500., 2000., 0.), false), None);
        assert_eq!(arrow_press(axis(500., 2000., 1500.), true), None);
    }

    #[test]
    fn page_at_offset() {
        // Three pages, 24 px from the top, 18 px apart.
        let pages = [(24., 1080.), (1098., 2154.), (2172., 3228.)];
        let max = 3252. - 500.;
        assert_eq!(page_at(&pages, 0., max), Some(0));
        assert_eq!(page_at(&pages, 1000., max), Some(0));
        // In the gap below page 1: the next page.
        assert_eq!(page_at(&pages, 1085., max), Some(1));
        assert_eq!(page_at(&pages, 2000., max), Some(1));
        assert_eq!(page_at(&pages, 2200., max), Some(2));
        // At the end: the last page, wherever its top is.
        assert_eq!(page_at(&pages, max, max), Some(2));
        assert_eq!(page_at(&[], 0., 0.), None);
        // Nothing scrolls: the first page.
        assert_eq!(page_at(&pages[..1], 0., 0.), Some(0));
    }

    #[test]
    fn page_tip_text() {
        assert_eq!(page_tip(2, 14), "Page 3 of 14");
        assert_eq!(page_tip(13, 14), "Page 14 of 14");
    }
}
