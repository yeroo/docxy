//! Project's Timeline pane: a fit-to-width overview of the whole plan above the
//! Gantt view. Unlike the chart header it never scrolls; the plan's span is
//! mapped onto the pane's width.
use super::*;
use projcore::DateTime;

pub(crate) const TIMELINE_H: f32 = 84.;
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

/// Project's default date form: `Mon 3/2/26`.
pub(crate) fn project_date(dt: DateTime) -> String {
    let p = dt.parts();
    format!(
        "{} {}/{}/{:02}",
        WEEKDAYS[dt.weekday() as usize],
        p.month,
        p.day,
        p.year.rem_euclid(100)
    )
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

/// The ruler for the plan as displayed: from the earliest date the Gantt draws
/// to the latest (leveled when leveling is on).
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

    /// The chart offset at which `day` starts.
    fn x(self, scale: GanttScale, day: i64) -> f32 {
        (day - scale.origin_day) as f32 * DAY_W
    }

    /// Days since the span's first, where chart offset `x` falls.
    fn day_at(self, scale: GanttScale, x: f32) -> f32 {
        (scale.origin_day - self.first) as f32 + x / DAY_W
    }
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
        clamp(scale.origin_day + (gantt_x / DAY_W).floor() as i64),
        clamp(scale.origin_day + ((gantt_x + gantt_w) / DAY_W).ceil() as i64 - 1),
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
    let lo = span.x(scale, span.first).max(0.);
    let hi = (span.x(scale, span.first + span.days) - gantt_w).min(scale.width() - gantt_w);
    if w <= 0. || lo > hi {
        return start_x;
    }
    let x = start_x + dx * span.days as f32 * DAY_W / w;
    x.clamp(lo.min(start_x), hi.max(start_x))
}

impl ProjectView {
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
    pub fn press_timeline(&self, x: f32) {
        self.timeline_press.set((x, self.gantt_x.get()));
    }

    /// The pointer is at `x` in a drag of the view box: move the chart by the
    /// days the box has covered since the press. Absolute from the press, so
    /// nothing accumulates and nothing drifts. View state only.
    pub fn drag_timeline(&mut self, x: f32) {
        let (press, start_x) = self.timeline_press.get();
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
    vec![
        (
            "timeline".into(),
            Json::Str(if v.timeline { "shown" } else { "hidden" }.into()),
        ),
        ("timeline_start".into(), Json::Str(r.start)),
        ("timeline_finish".into(), Json::Str(r.finish)),
        ("timeline_view_start".into(), date(first)),
        ("timeline_view_finish".into(), date(last)),
    ]
}

/// The drag payload for the Timeline's view box. It carries nothing: the press
/// it is measured from is [`ProjectView::timeline_press`], because the frame
/// that takes the mouse-down need not be the one whose payload the drag uses.
struct TimelineDrag;

impl Render for TimelineDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
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
        .w(px(w))
        .h(px(BAR_H))
        .flex_none()
        .px(px(6.))
        .justify_between()
        .items_center()
        .rounded(px(3.))
        .bg(pal.sel)
        .child(caption("Start", r.start, false))
        .child(caption("Finish", r.finish, true));
    h_flex()
        .id(("project-timeline", index))
        .relative()
        .h(px(TIMELINE_H))
        .flex_none()
        .bg(pal.panel)
        .border_t_1()
        .border_b_1()
        .border_color(pal.border)
        .on_drag_move::<TimelineDrag>(cx.listener(
            move |this, e: &DragMoveEvent<TimelineDrag>, window, cx| {
                cx.set_active_drag_cursor_style(CursorStyle::ClosedHand, window);
                if let Some(Surface::Project(v)) = this.tabs.get_mut(index).map(|t| &mut t.surface)
                {
                    v.drag_timeline(f32::from(e.event.position.x));
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
                                            this.tabs.get(index).map(|t| &t.surface)
                                        {
                                            v.press_timeline(f32::from(e.position.x));
                                        }
                                    }),
                                )
                                .on_drag(TimelineDrag, |_, _, _, cx| cx.new(|_| TimelineDrag)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_color(pal.dim)
                        .italic()
                        .child(PLACEHOLDER),
                ),
        )
}

#[cfg(test)]
mod tests;
