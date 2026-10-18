//! Project's Timeline pane: a fit-to-width overview of the whole plan above the
//! Gantt view. Unlike the chart header it never scrolls; the plan's span is
//! mapped onto the pane's width.
use super::*;
use projcore::DateTime;

/// The Timeline's default height.
pub(crate) const TIMELINE_H: f32 = 84.;
/// The ruler and the bar row, the Timeline's content.
pub(crate) const BAND_H: f32 = RULER_H + BAR_H;
const PAD_V: f32 = 7.;
/// The shortest the grip leaves the Timeline: one band and its padding.
pub(crate) const MIN_TIMELINE_H: f32 = BAND_H + 2. * PAD_V;
/// The grip under the pane: its height, and the edge strips of the view box.
const GRIP_H: f32 = 6.;
const BOX_EDGE_W: f32 = 6.;
/// The column that carries `TIMELINE` down the pane's left edge.
const LABEL_W: f32 = 22.;
const PAD: f32 = 10.;
const RULER_H: f32 = 18.;
const BAR_H: f32 = 28.;
pub(crate) const PLACEHOLDER: &str = "Add tasks with dates to the timeline";
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Project's default date form: `Mon 3/2/26`. Defined in projcore so the
/// field registry and the timeline cannot drift.
pub(crate) fn project_date(dt: DateTime) -> String {
    projcore::editor::format_project_date(dt)
}

/// A ruler unit: which days start one, how a tick is labelled, and the pixels a
/// label is given. The budget is a fixed allowance per unit, not a measured
/// text width, so the choice of unit does not depend on the font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickUnit {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl TickUnit {
    const ALL: [Self; 5] = [
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Quarter,
        Self::Year,
    ];

    pub fn min_gap(self) -> f32 {
        match self {
            Self::Day => 56.,
            Self::Week => 40.,
            Self::Month | Self::Quarter => 56.,
            Self::Year => 44.,
        }
    }

    fn starts(self, dt: DateTime) -> bool {
        let p = dt.parts();
        match self {
            Self::Day => true,
            Self::Week => dt.weekday() == 1,
            Self::Month => p.day == 1,
            Self::Quarter => p.day == 1 && p.month % 3 == 1,
            Self::Year => p.day == 1 && p.month == 1,
        }
    }

    fn label(self, dt: DateTime) -> String {
        let p = dt.parts();
        let yy = p.year.rem_euclid(100);
        match self {
            Self::Day => format!("{} {}/{}", WEEKDAYS[dt.weekday() as usize], p.month, p.day),
            Self::Week => format!("{}/{}", p.month, p.day),
            Self::Month => format!("{} '{yy:02}", MONTHS[p.month as usize - 1]),
            Self::Quarter => format!("Q{} '{yy:02}", (p.month - 1) / 3 + 1),
            Self::Year => p.year.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineRuler {
    pub start: String,
    pub finish: String,
    pub unit: Option<TickUnit>,
    /// `(fraction of the width in [0, 1), label)`, strictly increasing.
    pub ticks: Vec<(f32, String)>,
}

/// The ruler for the plan as displayed: from its earliest displayed start to its
/// latest displayed finish (leveled when leveling is on). Like [`TimelineSpan`]
/// it leaves out what only the Gantt scale adds: baseline dates outside the
/// plan and the padding past the finish.
pub(crate) fn timeline_ruler(ed: &ProjectEditor, width: f32) -> TimelineRuler {
    ruler(ed.disp_project_start(), ed.disp_project_finish(), width)
}

/// Days `[start_day, finish_day]` spread over `width` px, ticked in the finest
/// unit whose neighbouring ticks keep that unit's `min_gap` apart. Years thin
/// out to every n-th year when even one per year is too dense.
pub(crate) fn ruler(start: DateTime, finish: DateTime, width: f32) -> TimelineRuler {
    let TimelineSpan { first, days } = TimelineSpan::new(start, finish);
    let px_per_day = width.max(0.) / days as f32;
    let boundaries = |unit: TickUnit| -> Vec<i64> {
        (0..days)
            .filter(|&d| unit.starts(DateTime::from_minutes((first + d) * 1440)))
            .collect()
    };
    let fits = |offsets: &[i64], unit: TickUnit| {
        offsets
            .windows(2)
            .all(|w| (w[1] - w[0]) as f32 * px_per_day >= unit.min_gap())
    };
    let mut chosen = None;
    if width > 0. {
        for unit in TickUnit::ALL {
            let offsets = boundaries(unit);
            if unit == TickUnit::Year {
                let stride = (1..=offsets.len().max(1))
                    .find(|&n| {
                        fits(
                            &offsets.iter().copied().step_by(n).collect::<Vec<_>>(),
                            unit,
                        )
                    })
                    .unwrap_or(1);
                chosen = Some((unit, offsets.into_iter().step_by(stride).collect()));
            } else if fits(&offsets, unit) {
                chosen = Some((unit, offsets));
                break;
            }
        }
    }
    let (unit, offsets): (Option<TickUnit>, Vec<i64>) = match chosen {
        Some((unit, offsets)) => (Some(unit), offsets),
        None => (None, Vec::new()),
    };
    TimelineRuler {
        start: project_date(start),
        finish: project_date(finish),
        unit,
        ticks: offsets
            .into_iter()
            .map(|d| {
                let label = unit
                    .expect("ticks imply a unit")
                    .label(DateTime::from_minutes((first + d) * 1440));
                (d as f32 / days as f32, label)
            })
            .collect(),
    }
}

/// The days the Timeline spans, `[first, first + days)`: the plan as displayed,
/// without the Gantt scale's baseline lead-in or its padding past the finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimelineSpan {
    pub first: i64,
    pub days: i64,
}

impl TimelineSpan {
    pub fn new(start: DateTime, finish: DateTime) -> Self {
        let first = start.day_number();
        Self {
            first,
            days: (finish.day_number() - first + 1).max(1),
        }
    }

    pub fn of(ed: &ProjectEditor) -> Self {
        Self::new(ed.disp_project_start(), ed.disp_project_finish())
    }

    /// Days since the span's first, where chart offset `x` falls.
    fn day_at(self, scale: GanttScale, x: f32) -> f32 {
        (scale.origin_day - self.first) as f32 + x / scale.day_w
    }
}

/// Inclusive non-working runs in the plan span. Skip shading when individual
/// days would be too narrow to distinguish in the fit-to-width overview.
fn timeline_runs(
    span: TimelineSpan,
    cal: Option<&projcore::WorkCalendar>,
    width: f32,
) -> Vec<(i64, i64)> {
    let Some(cal) = cal else {
        return Vec::new();
    };
    if width / (span.days as f32) < 2. {
        return Vec::new();
    }
    let mut runs: Vec<(i64, i64)> = Vec::new();
    for day in span.first..span.first + span.days {
        if cal.day(day).is_empty() {
            if let Some(last) = runs.last_mut()
                && last.1 == day - 1
            {
                last.1 = day;
                continue;
            }
            runs.push((day, day));
        }
    }
    runs
}

/// Clipped `(left, width)` pixel runs for the Timeline's visible non-working days.
pub(crate) fn timeline_shading(
    span: TimelineSpan,
    cal: Option<&projcore::WorkCalendar>,
    width: f32,
) -> Vec<(f32, f32)> {
    timeline_runs(span, cal, width)
        .into_iter()
        .map(|(first, last)| {
            let left = ((first - span.first) as f32 / span.days as f32 * width).clamp(0., width);
            let right =
                ((last + 1 - span.first) as f32 / span.days as f32 * width).clamp(0., width);
            (left, right - left)
        })
        .collect()
}

/// The narrowest the view box is drawn, so a box squeezed against either end
/// of the Timeline stays visible and grabbable.
pub(crate) const MIN_BOX_W: f32 = 6.;

/// The chart's visible days `[gantt_x, gantt_x + gantt_w)` as fractions
/// `(f0, f1)` of the Timeline, clamped to `0 <= f0 <= f1 <= 1`.
pub(crate) fn view_box(
    span: TimelineSpan,
    scale: GanttScale,
    gantt_x: f32,
    gantt_w: f32,
) -> (f32, f32) {
    let frac = |x: f32| (span.day_at(scale, x) / span.days as f32).clamp(0., 1.);
    (frac(gantt_x), frac(gantt_x + gantt_w))
}

/// The view box `(left, width)` in a ruler `w` px wide: at least `MIN_BOX_W`
/// wide, and pushed back inside the ruler when that widening overflows it.
pub(crate) fn box_px(f0: f32, f1: f32, w: f32) -> (f32, f32) {
    let w = w.max(0.);
    let width = ((f1 - f0) * w).max(MIN_BOX_W).min(w);
    ((f0 * w).min(w - width).max(0.), width)
}

/// The first and last day numbers the chart shows, clamped to the span.
pub(crate) fn view_days(
    span: TimelineSpan,
    scale: GanttScale,
    gantt_x: f32,
    gantt_w: f32,
) -> (i64, i64) {
    let last = span.first + span.days - 1;
    let clamp = |d: i64| d.clamp(span.first, last);
    (
        clamp(scale.origin_day + (gantt_x / scale.day_w).floor() as i64),
        clamp(scale.origin_day + ((gantt_x + gantt_w) / scale.day_w).ceil() as i64 - 1),
    )
}

/// The chart offset after dragging the view box `dx` px along a ruler `w` px
/// wide, from where the drag began (`start_x`). The drag keeps the box on the
/// Timeline: it may not scroll the chart before the span's first day or past
/// its last, nor beyond the chart's own scroll range. A chart already outside
/// that range (the scrollbar reaches the padding past the finish) may move
/// back towards it but not further out, so no drag snaps it; and a chart wider
/// than the span has nothing to drag.
pub(crate) fn drag_gantt_x(
    span: TimelineSpan,
    scale: GanttScale,
    gantt_w: f32,
    w: f32,
    start_x: f32,
    dx: f32,
) -> f32 {
    let lo = scale.x(span.first).max(0.);
    let hi = (scale.x(span.first + span.days) - gantt_w).min(scale.width() - gantt_w);
    if w <= 0. || lo > hi {
        return start_x;
    }
    let x = start_x + dx * span.days as f32 * scale.day_w / w;
    x.clamp(lo.min(start_x), hi.max(start_x))
}

/// The view box's edge a drag holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoxEdge {
    Left,
    Right,
}

/// The press on a box edge: the pointer's x, and the chart's visible span
/// then as days from the scale's origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EdgePress {
    pub edge: BoxEdge,
    pub x: f32,
    pub left: f32,
    pub right: f32,
}

/// The zoom after dragging `edge` of the visible span `[left, right)` (days
/// from the scale's origin) by `dd` days: `(day_w, left)`. The span the box
/// then covers fills `gantt_w`; the far edge stays put, also when the width
/// clamps. The moved edge stays inside `[lo, hi]` (the days the Timeline draws,
/// so the box edge follows the pointer) and the span never shrinks below the
/// day count the widest zoom shows, or the Timeline's own length if less.
/// `end` is the scale's last day, the end of the chart's scroll range
/// (`scale.days`). A Left drag holds the far edge at `right.min(end)`: when the
/// chart showed days past `end`, its real right edge moves to `end` (the scroll
/// clamp could not keep it elsewhere) while the drawn box edge, already clamped
/// to the Timeline, stays put.
pub(crate) fn drag_edge(
    edge: BoxEdge,
    left: f32,
    right: f32,
    dd: f32,
    gantt_w: f32,
    (lo, hi): (f32, f32),
    end: f32,
) -> (f32, f32) {
    // The span cannot be asked to shrink below what the Timeline holds.
    let least = (gantt_w.max(1.) / zoom::MAX_DAY_W).min((hi - lo).max(1.));
    // `left` and `right` are the chart's real edges; the box draws them clamped
    // to `[lo, hi]`, and the pointer moves that drawn edge. An edge the pointer
    // has not moved from where it is drawn stays at its real place, so a press
    // (or an outward drag at the Timeline's end) never changes the zoom.
    let moved = |real: f32| {
        let drawn = real.clamp(lo, hi);
        let target = (drawn + dd).clamp(lo, hi);
        ((target - drawn).abs() >= 1e-6).then_some(target)
    };
    match edge {
        BoxEdge::Left => {
            // The far edge is held where the scroll range can: a chart showing
            // days past the scale's `end` cannot keep its right edge there once
            // the left one moves, so it is held at `end`.
            let (l, right) = match moved(left) {
                Some(target) => (target, right.min(end)),
                None => (left, right),
            };
            let l = l.min(right - least);
            let day_w = zoom::fit(right - l, gantt_w);
            (day_w, right - gantt_w / day_w)
        }
        BoxEdge::Right => {
            let r = moved(right).unwrap_or(right).max(left + least);
            (zoom::fit(r - left, gantt_w), left)
        }
    }
}

/// The Timeline's height for a request `h` in a window `window_h` tall: one band
/// and its padding at least, half the window at most.
pub(crate) fn clamp_timeline_h(h: f32, window_h: f32) -> f32 {
    h.min(window_h / 2.).max(MIN_TIMELINE_H)
}

impl ProjectView {
    /// The Timeline's height now.
    pub fn timeline_height(&self) -> f32 {
        clamp_timeline_h(self.timeline_h.unwrap_or(TIMELINE_H), self.height)
    }

    /// Mouse-down on the grip: remember the pointer's `y` and the height.
    pub fn press_grip(&mut self, y: f32) {
        self.grip_press = Some((y, self.timeline_height()));
    }

    /// The pointer is at `y` in a drag of the grip. Without a press, nothing moves.
    pub fn drag_grip(&mut self, y: f32) {
        let Some((press, start_h)) = self.grip_press else {
            return;
        };
        self.timeline_h = Some(clamp_timeline_h(start_h + y - press, self.height));
    }

    /// Double-click on the grip: the default height.
    pub fn reset_timeline_h(&mut self) {
        self.timeline_h = None;
    }

    /// Mouse-down on a box edge at `x`.
    /// The chart's real edges are kept: it may show days outside the Timeline's
    /// span (a short plan in a wide chart), where the box is drawn clamped to
    /// the span; [`drag_edge`] maps the pointer through the drawn edge.
    pub fn press_timeline_edge(&mut self, edge: BoxEdge, x: f32) {
        let day_w = self.day_w();
        let left = self.gantt_x.get() / day_w;
        self.timeline_edge = Some(EdgePress {
            edge,
            x,
            left,
            right: left + self.gantt_w / day_w,
        });
    }

    /// The Timeline's span as days from the scale's origin.
    fn timeline_bounds(&self) -> (f32, f32) {
        let span = TimelineSpan::of(&self.ed);
        let lo = (span.first - self.scale.origin_day) as f32;
        (lo, lo + span.days as f32)
    }

    /// The pointer is at `x` in a drag of a box edge: the edge moves the days
    /// the pointer has covered since the press and the chart zooms so the box's
    /// span fills it. Absolute from the press, so nothing accumulates.
    pub fn drag_timeline_edge(&mut self, x: f32) {
        let Some(press) = self.timeline_edge else {
            return;
        };
        let span = TimelineSpan::of(&self.ed);
        let w = ruler_width(self.width);
        if w <= 0. {
            return;
        }
        let dd = (x - press.x) * span.days as f32 / w;
        let bounds = self.timeline_bounds();
        let (day_w, left) = drag_edge(
            press.edge,
            press.left,
            press.right,
            dd,
            self.gantt_w,
            bounds,
            self.scale.days as f32,
        );
        self.set_zoom(day_w, left, 0.);
    }

    pub fn timeline_box(&self) -> (f32, f32) {
        view_box(
            TimelineSpan::of(&self.ed),
            self.scale,
            self.gantt_x.get(),
            self.gantt_w,
        )
    }

    /// Mouse-down on the view box: remember the pointer's `x` and the chart
    /// offset, which every move of a drag that follows is measured from.
    pub fn press_timeline(&mut self, x: f32) {
        self.timeline_press = Some((x, self.gantt_x.get()));
    }

    /// The button came up: the press, and any drag, is over.
    pub fn release_timeline(&mut self) {
        self.timeline_press = None;
        self.timeline_edge = None;
        self.grip_press = None;
    }

    /// The pointer is at `x` in a drag of the view box: move the chart by the
    /// days the box has covered since the press. Absolute from the press, so
    /// nothing accumulates and nothing drifts. Without a press, nothing moves.
    /// View state only.
    pub fn drag_timeline(&mut self, x: f32) {
        let Some((press, start_x)) = self.timeline_press else {
            return;
        };
        self.gantt_x.set(drag_gantt_x(
            TimelineSpan::of(&self.ed),
            self.scale,
            self.gantt_w,
            ruler_width(self.width),
            start_x,
            x - press,
        ));
        self.clamp_offsets();
    }
}

/// The ruler's drawable width inside a Timeline pane `pane_w` wide.
pub(crate) fn ruler_width(pane_w: f32) -> f32 {
    (pane_w - LABEL_W - 2. * PAD).max(0.)
}

pub(crate) fn timeline_state(v: &ProjectView) -> Vec<(String, ctlcore::json::Json)> {
    use ctlcore::json::Json;
    let r = timeline_ruler(&v.ed, ruler_width(v.width));
    let (first, last) = view_days(TimelineSpan::of(&v.ed), v.scale, v.gantt_x.get(), v.gantt_w);
    let date = |d: i64| Json::Str(project_date(DateTime::from_minutes(d * 1440)));
    let nonworking = timeline_runs(
        TimelineSpan::of(&v.ed),
        v.ed.project().project_shading_calendar().as_ref(),
        ruler_width(v.width),
    )
    .into_iter()
    .map(|(start, finish)| Json::Arr(vec![date(start), date(finish)]))
    .collect();
    vec![
        (
            "timeline".into(),
            Json::Str(if v.timeline { "shown" } else { "hidden" }.into()),
        ),
        ("timeline_start".into(), Json::Str(r.start)),
        ("timeline_finish".into(), Json::Str(r.finish)),
        ("timeline_view_start".into(), date(first)),
        ("timeline_view_finish".into(), date(last)),
        ("nonworking".into(), Json::Arr(nonworking)),
        (
            "timeline_h".into(),
            Json::Num(f64::from(v.timeline_height())),
        ),
    ]
}

/// The drag payload for the Timeline's view box: the index of the tab whose box
/// it is, so a drag begun on one tab never scrolls another's chart. The press it
/// is measured from is [`ProjectView::timeline_press`], not a field here, because
/// the frame that takes the mouse-down need not be the one whose payload the
/// drag uses.
#[derive(Clone, Copy)]
struct TimelineDrag(usize, Option<BoxEdge>);

/// The drag payload for the Timeline's grip.
#[derive(Clone, Copy)]
struct GripDrag(usize);

impl Render for GripDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl Render for TimelineDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

fn release_timeline(
    index: usize,
    cx: &mut Context<Docxy>,
) -> impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this, _: &MouseUpEvent, _, _| {
        if let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface) {
            v.release_timeline();
        }
    })
}

fn tint_run((left, width): (f32, f32), fill: Hsla) -> impl IntoElement {
    div()
        .absolute()
        .left(px(left))
        .top_0()
        .h_full()
        .w(px(width))
        .bg(fill)
}

pub(crate) fn timeline_el(
    view: &ProjectView,
    index: usize,
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    let w = ruler_width(view.width);
    let r = timeline_ruler(&view.ed, w);
    let shading = timeline_shading(
        TimelineSpan::of(&view.ed),
        view.ed.project().project_shading_calendar().as_ref(),
        w,
    );
    let (f0, f1) = view.timeline_box();
    let (box_left, box_w) = box_px(f0, f1, w);
    let caption = |top: &'static str, date: String, end: bool| {
        v_flex()
            .when(end, |d| d.items_end())
            .text_size(px(10.))
            .line_height(px(12.))
            .child(div().text_color(pal.dim).child(top))
            .child(div().font_weight(FontWeight::BOLD).child(date))
    };
    let ruler_row = div()
        .relative()
        .w(px(w))
        .h(px(RULER_H))
        .flex_none()
        .text_size(px(10.))
        .text_color(pal.dim)
        .children(
            shading
                .iter()
                .copied()
                .map(|run| tint_run(run, nonworking_fill(pal))),
        )
        .children(r.ticks.into_iter().map(|(at, label)| {
            div()
                .absolute()
                .left(px(at * w))
                .top(px(3.))
                .h(px(RULER_H - 3.))
                .pl(px(3.))
                .border_l_1()
                .border_color(pal.border)
                .whitespace_nowrap()
                .child(label)
        }));
    let bar_row = h_flex()
        .relative()
        .w(px(w))
        .h(px(BAR_H))
        .flex_none()
        .px(px(6.))
        .justify_between()
        .items_center()
        .rounded(px(3.))
        .bg(pal.sel)
        .children(
            shading
                .iter()
                .copied()
                .map(|run| tint_run(run, nonworking_fill(pal))),
        )
        .child(caption("Start", r.start, false))
        .child(caption("Finish", r.finish, true));
    h_flex()
        .id(("project-timeline", index))
        .relative()
        .h(px(view.timeline_height()))
        .flex_none()
        .bg(pal.panel)
        .border_t_1()
        .border_b_1()
        .border_color(pal.border)
        .on_drag_move::<TimelineDrag>(cx.listener(
            move |this, e: &DragMoveEvent<TimelineDrag>, window, cx| {
                cx.set_active_drag_cursor_style(CursorStyle::ClosedHand, window);
                if e.drag(cx).0 != index {
                    return;
                }
                if let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface)
                {
                    match e.drag(cx).1 {
                        Some(_) => v.drag_timeline_edge(f32::from(e.event.position.x)),
                        None => v.drag_timeline(f32::from(e.event.position.x)),
                    }
                    cx.notify();
                }
            },
        ))
        .on_drag_move::<GripDrag>(cx.listener(
            move |this, e: &DragMoveEvent<GripDrag>, window, cx| {
                cx.set_active_drag_cursor_style(CursorStyle::ResizeUpDown, window);
                if e.drag(cx).0 != index {
                    return;
                }
                if let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface)
                {
                    v.drag_grip(f32::from(e.event.position.y));
                    cx.notify();
                }
            },
        ))
        .child(probe(
            probes,
            harness::region_name(harness::Region::ProjectTimeline),
        ))
        // GPUI draws no rotated text; stacked letters read down the edge instead.
        .child(
            v_flex()
                .w(px(LABEL_W))
                .h_full()
                .flex_none()
                .items_center()
                .justify_center()
                .border_r_1()
                .border_color(pal.border)
                .text_size(px(8.))
                .line_height(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(pal.dim)
                .children("TIMELINE".chars().map(|c| div().child(c.to_string()))),
        )
        .child(
            v_flex()
                .w(px(w + 2. * PAD))
                .h_full()
                .flex_none()
                .px(px(PAD))
                .overflow_hidden()
                .child(
                    div()
                        .relative()
                        .w(px(w))
                        .flex_none()
                        .child(ruler_row)
                        .child(bar_row)
                        .child(
                            div()
                                .id(("project-timeline-box", index))
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(box_left))
                                .w(px(box_w))
                                .border_1()
                                .rounded(px(3.))
                                .border_color(pal.dim)
                                .bg(Hsla { a: 0.1, ..pal.fg })
                                .cursor(CursorStyle::OpenHand)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, e: &MouseDownEvent, _, _| {
                                        if let Some(Surface::Project(v)) =
                                            this.tabs.get_mut(index).map(|t| &mut t.surface)
                                        {
                                            v.press_timeline(f32::from(e.position.x));
                                        }
                                    }),
                                )
                                // Wherever the button comes up, on the box or off it.
                                .on_mouse_up(MouseButton::Left, release_timeline(index, cx))
                                .on_mouse_up_out(MouseButton::Left, release_timeline(index, cx))
                                .on_drag(TimelineDrag(index, None), |d, _, _, cx| cx.new(|_| *d))
                                .child(box_edge(
                                    BoxEdge::Left,
                                    edge_strip_w(box_w),
                                    index,
                                    probes,
                                    cx,
                                ))
                                .child(box_edge(
                                    BoxEdge::Right,
                                    edge_strip_w(box_w),
                                    index,
                                    probes,
                                    cx,
                                )),
                        ),
                )
                .when(view.ed.project().tasks.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .text_color(pal.dim)
                            .italic()
                            .child(PLACEHOLDER),
                    )
                }),
        )
        .child(timeline_grip(index, probes, cx))
}

/// The width of a box edge strip: 6 px, but a third of a narrow box at most, so
/// the middle (which pans) is always there.
pub(crate) fn edge_strip_w(box_w: f32) -> f32 {
    BOX_EDGE_W.min(box_w / 3.)
}

/// One of the view box's edge strips, drawn last so it wins over the box. A
/// drag of it moves that edge of the span and zooms the chart to fit it.
fn box_edge(
    edge: BoxEdge,
    strip_w: f32,
    index: usize,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    let name = match edge {
        BoxEdge::Left => harness::Region::ProjectTimelineBoxLeft,
        BoxEdge::Right => harness::Region::ProjectTimelineBoxRight,
    };
    div()
        .id((
            match edge {
                BoxEdge::Left => "project-timeline-box-left",
                BoxEdge::Right => "project-timeline-box-right",
            },
            index,
        ))
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(strip_w))
        .when(edge == BoxEdge::Left, |d| d.left(px(-1.)))
        .when(edge == BoxEdge::Right, |d| d.right(px(-1.)))
        .cursor(CursorStyle::ResizeLeftRight)
        .child(probe(probes, harness::region_name(name)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                // The box's own press (a pan) must not also begin.
                cx.stop_propagation();
                if let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface)
                {
                    v.press_timeline_edge(edge, f32::from(e.position.x));
                }
            }),
        )
        .on_mouse_up(MouseButton::Left, release_timeline(index, cx))
        .on_mouse_up_out(MouseButton::Left, release_timeline(index, cx))
        .on_drag(TimelineDrag(index, Some(edge)), |d, _, _, cx| {
            cx.new(|_| *d)
        })
}

/// The strip along the Timeline's bottom edge: a drag sets the pane's height,
/// a double-click restores the default.
fn timeline_grip(
    index: usize,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    cx: &mut Context<Docxy>,
) -> impl IntoElement {
    div()
        .id(("project-timeline-grip", index))
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(-1.))
        .h(px(GRIP_H))
        .cursor(CursorStyle::ResizeUpDown)
        .child(probe(
            probes,
            harness::region_name(harness::Region::ProjectTimelineGrip),
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface)
                {
                    if e.click_count >= 2 {
                        v.reset_timeline_h();
                        cx.notify();
                    } else {
                        v.press_grip(f32::from(e.position.y));
                    }
                }
            }),
        )
        .on_mouse_up(MouseButton::Left, release_timeline(index, cx))
        .on_mouse_up_out(MouseButton::Left, release_timeline(index, cx))
        .on_drag(GripDrag(index), |d, _, _, cx| cx.new(|_| *d))
}

#[cfg(test)]
mod tests;
