//! Day-scale geometry shared by the renderer and harness.
use super::*;

pub(crate) const DAY_W: f32 = 22.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BarKind {
    Critical,
    OnTrack,
    Summary,
    Milestone,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GanttBar {
    pub kind: BarKind,
    pub start: i64,
    pub end: i64,
    pub baseline: Option<(i64, i64)>,
    pub delay: Option<(i64, i64)>,
}

impl GanttBar {
    pub fn state(self) -> String {
        let kind = match self.kind {
            BarKind::Critical => "critical",
            BarKind::OnTrack => "on-track",
            BarKind::Summary => "summary",
            BarKind::Milestone => "milestone",
        };
        format!("{kind} {}-{}", self.start, self.end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GanttScale {
    pub origin_day: i64,
    pub days: i64,
}

impl GanttScale {
    pub fn width(self) -> f32 {
        self.days as f32 * DAY_W
    }

    fn date(self, day: i64) -> projcore::DateTime {
        projcore::DateTime::from_minutes((self.origin_day + day) * 1440)
    }

    pub fn is_weekend(self, day: i64) -> bool {
        matches!(self.date(day).weekday(), 0 | 6)
    }

    pub fn ticks(self, visible: std::ops::Range<i64>) -> Vec<(i64, String)> {
        visible
            .filter_map(|day| {
                let date = self.date(day);
                let p = date.parts();
                (date.weekday() == 1).then(|| (day, format!("{}/{}", p.month, p.day)))
            })
            .collect()
    }

    fn visible(self, offset: f32, width: f32) -> std::ops::Range<i64> {
        (offset / DAY_W).floor().max(0.) as i64
            ..(((offset + width) / DAY_W).ceil() as i64).min(self.days)
    }
}

pub(crate) fn gantt_scale(ed: &ProjectEditor) -> GanttScale {
    let mut first = ed.schedule().project_start.day_number();
    let mut last = ed.schedule().project_finish.day_number();
    for t in &ed.project().tasks {
        for start in [ed.disp_start(t.uid), t.baseline(0).and_then(|b| b.start)]
            .into_iter()
            .flatten()
        {
            first = first.min(start.day_number());
        }
        for finish in [ed.disp_finish(t.uid), t.baseline(0).and_then(|b| b.finish)]
            .into_iter()
            .flatten()
        {
            last = last.max(finish.day_number());
        }
    }
    GanttScale {
        origin_day: first,
        days: (last - first + 1 + 7).max(30),
    }
}

pub(crate) fn gantt_bar(ed: &ProjectEditor, task: &Task, scale: GanttScale) -> Option<GanttBar> {
    let result = ed.schedule().get(task.uid)?;
    let start = ed.disp_start(task.uid)?;
    let finish = ed.disp_finish(task.uid)?;
    let day = |dt: projcore::DateTime| dt.day_number() - scale.origin_day;
    Some(GanttBar {
        kind: if task.summary {
            BarKind::Summary
        } else if task.is_milestone() {
            BarKind::Milestone
        } else if result.critical {
            BarKind::Critical
        } else {
            BarKind::OnTrack
        },
        start: day(start),
        end: day(finish),
        baseline: task
            .baseline(0)
            .and_then(|b| b.start.zip(b.finish))
            .map(|(s, e)| (day(s), day(e))),
        delay: (ed.leveled() && start > result.early_start)
            .then(|| (day(result.early_start), day(start))),
    })
}

pub(crate) fn table_pane_width(width: f32) -> f32 {
    (width * 0.5).clamp(320., TABLE_W).min(width.max(0.))
}

pub(crate) fn intersect(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let r = a.intersect(&b);
    (!r.is_empty()).then_some(r)
}

/// The chart is `gantt_w` wide, so the vertical scrollbar right of it is not chart.
pub(crate) fn gantt_viewport(
    body: Bounds<Pixels>,
    table_w: f32,
    gantt_w: f32,
) -> Option<Bounds<Pixels>> {
    let chart_x = table_w + GANTT_INSET;
    intersect(
        body,
        Bounds {
            origin: point(body.origin.x + px(chart_x), body.origin.y),
            size: size(px(gantt_w), body.size.height),
        },
    )
}

fn backdrop(scale: GanttScale, offset: f32, width: f32, pal: Pal) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for day in scale
                .visible(offset, width)
                .filter(|d| scale.is_weekend(*d))
            {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(bounds.origin.x + px(day as f32 * DAY_W), bounds.origin.y),
                        size: size(px(DAY_W), bounds.size.height),
                    },
                    Hsla { a: 0.12, ..pal.dim },
                ));
            }
        },
    )
    .absolute()
    .size_full()
}

pub(crate) fn gantt_header(
    scale: GanttScale,
    offset: f32,
    width: f32,
    pal: Pal,
) -> impl IntoElement {
    div()
        .relative()
        .w(px(scale.width()))
        .h(px(ROW_H))
        .child(backdrop(scale, offset, width, pal))
        .children(
            scale
                .ticks(scale.visible((offset - DAY_W * 2.).max(0.), width + DAY_W * 2.))
                .into_iter()
                .map(|(day, label)| {
                    div()
                        .absolute()
                        .left(px(day as f32 * DAY_W))
                        .top(px(5.))
                        .child(label)
                }),
        )
}

pub(crate) fn gantt_strip(
    bar: Option<GanttBar>,
    id: i32,
    scale: GanttScale,
    offset: f32,
    width: f32,
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
) -> impl IntoElement {
    let strip = div()
        .relative()
        .w(px(scale.width()))
        .h(px(ROW_H))
        .child(backdrop(scale, offset, width, pal));
    let Some(bar) = bar else {
        return strip;
    };
    let span = |s: i64, e: i64, y: f32, h: f32, color: Hsla| {
        div()
            .absolute()
            .left(px(s as f32 * DAY_W))
            .top(px(y))
            .w(px((e - s + 1).max(1) as f32 * DAY_W))
            .h(px(h))
            .bg(color)
    };
    let mut strip = strip;
    if let Some((s, e)) = bar.delay {
        strip = strip.child(
            div()
                .absolute()
                .left(px(s as f32 * DAY_W))
                .top(px(8.))
                .w(px((e - s).max(0) as f32 * DAY_W))
                .h(px(8.))
                .bg(Hsla { a: 0.4, ..pal.dim }),
        );
    }
    if let Some((s, e)) = bar.baseline {
        strip = strip.child(span(s, e, 22., 4., pal.dim));
    }
    let marker = probe(probes, format!("bar:{id}"));
    let element = match bar.kind {
        BarKind::Critical | BarKind::OnTrack => span(
            bar.start,
            bar.end,
            5.,
            14.,
            hsla_u(if bar.kind == BarKind::Critical {
                GANTT_CRIT
            } else {
                BRAND
            }),
        )
        .child(marker)
        .into_any_element(),
        BarKind::Summary => div()
            .absolute()
            .left(px(bar.start as f32 * DAY_W))
            .top(px(9.))
            .w(px((bar.end - bar.start + 1).max(1) as f32 * DAY_W))
            .h(px(10.))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .w_full()
                    .h(px(6.))
                    .bg(hsla_u(GANTT_SUMMARY)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .w(px(3.))
                    .h_full()
                    .bg(hsla_u(GANTT_SUMMARY)),
            )
            .child(
                div()
                    .absolute()
                    .right_0()
                    .w(px(3.))
                    .h_full()
                    .bg(hsla_u(GANTT_SUMMARY)),
            )
            .child(marker)
            .into_any_element(),
        BarKind::Milestone => div()
            .absolute()
            .left(px(bar.start as f32 * DAY_W + 4.))
            .top(px(5.))
            .size(px(14.))
            .child(
                canvas(
                    |_, _, _| (),
                    |b, _, window, _| {
                        let mut path = PathBuilder::fill();
                        let x = b.origin.x;
                        let y = b.origin.y;
                        path.move_to(point(x + px(7.), y));
                        path.line_to(point(x + px(14.), y + px(7.)));
                        path.line_to(point(x + px(7.), y + px(14.)));
                        path.line_to(point(x, y + px(7.)));
                        path.close();
                        if let Ok(path) = path.build() {
                            window.paint_path(path, rgb(GANTT_MILESTONE));
                        }
                    },
                )
                .size_full(),
            )
            .child(marker)
            .into_any_element(),
    };
    strip.child(element)
}

#[cfg(test)]
mod tests;
