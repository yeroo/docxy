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

pub(crate) fn gantt_viewport(body: Bounds<Pixels>, table_w: f32) -> Option<Bounds<Pixels>> {
    let chart_x = table_w + GANTT_INSET;
    intersect(
        body,
        Bounds {
            origin: point(body.origin.x + px(chart_x), body.origin.y),
            size: size(
                (body.size.width - px(chart_x)).max(px(0.)),
                body.size.height,
            ),
        },
    )
}

/// Weekend days in the visible range; the header and the body backdrop shade these.
pub(crate) fn shaded_days(scale: GanttScale, offset: f32, width: f32) -> Vec<i64> {
    scale
        .visible(offset, width)
        .filter(|d| scale.is_weekend(*d))
        .collect()
}

/// The left edge of each visible day, relative to the chart viewport.
pub(crate) fn day_lines(scale: GanttScale, offset: f32, width: f32) -> Vec<f32> {
    scale
        .visible(offset, width)
        .map(|day| day as f32 * DAY_W - offset)
        .filter(|x| (0. ..width).contains(x))
        .collect()
}

/// The bottom rule of each row in view, in body pixels. `scroll_y` grows downward, so the
/// rules follow the list's scroll phase whether a row holds a task or not.
pub(crate) fn row_rules(body_h: f32, scroll_y: f32) -> Vec<f32> {
    let scroll_y = scroll_y.max(0.);
    ((scroll_y / ROW_H).floor() as i64..)
        .map(|k| (k + 1) as f32 * ROW_H - 1. - scroll_y)
        .skip_while(|y| *y < 0.)
        .take_while(|y| *y < body_h)
        .collect()
}

/// Ruled empty rows visible below the last task, counting a partly visible one.
pub(crate) fn filler_rows(body_h: f32, scroll_y: f32, count: usize) -> usize {
    let used = count as f32 * ROW_H - scroll_y.max(0.);
    ((body_h - used).max(0.) / ROW_H).ceil() as usize
}

fn weekend_fill(pal: Pal) -> Hsla {
    Hsla { a: 0.12, ..pal.dim }
}

fn backdrop(scale: GanttScale, offset: f32, width: f32, pal: Pal) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for day in shaded_days(scale, offset, width) {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(bounds.origin.x + px(day as f32 * DAY_W), bounds.origin.y),
                        size: size(px(DAY_W), bounds.size.height),
                    },
                    weekend_fill(pal),
                ));
            }
        },
    )
    .absolute()
    .size_full()
}

/// The grid behind every row of the body, task or empty: table rules and column dividers,
/// chart shading, day lines and rules. Painted once at full height so it fills the pane.
pub(crate) fn body_grid(
    view: &ProjectView,
    scroll: UniformListScrollHandle,
    pal: Pal,
) -> impl IntoElement {
    let (table_w, table_x, gantt_x, scale) = (view.table_w, view.table_x, view.gantt_x, view.scale);
    let rule = Hsla {
        a: pal.border.a * 0.6,
        ..pal.border
    };
    let day_rule = Hsla {
        a: pal.border.a * 0.35,
        ..pal.border
    };
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            // Read at paint: the list's prepaint, which runs after this canvas's, settles the offset.
            let scroll_y = -f32::from(scroll.0.borrow().base_handle.offset().y);
            let body_h = f32::from(bounds.size.height);
            let rules = row_rules(body_h, scroll_y);
            let hline = |window: &mut Window, x: Pixels, w: Pixels, y: f32| {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(x, bounds.origin.y + px(y)),
                        size: size(w, px(1.)),
                    },
                    rule,
                ));
            };
            let vline = |window: &mut Window, x: Pixels, color: Hsla| {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(x, bounds.origin.y),
                        size: size(px(1.), bounds.size.height),
                    },
                    color,
                ));
            };
            let table = Bounds {
                origin: bounds.origin,
                size: size(px(table_w).min(bounds.size.width), bounds.size.height),
            };
            window.with_content_mask(Some(ContentMask { bounds: table }), |window| {
                let mut edge = 0.;
                for w in WIDTHS {
                    edge += w;
                    vline(window, bounds.origin.x + px(edge - table_x - 1.), rule);
                }
                for y in &rules {
                    hline(window, bounds.origin.x, table.size.width, *y);
                }
            });
            // The rows' inset divider, continued below the last task.
            vline(window, bounds.origin.x + px(table_w), pal.border);
            let Some(chart) = gantt_viewport(bounds, table_w) else {
                return;
            };
            let width = f32::from(chart.size.width);
            window.with_content_mask(Some(ContentMask { bounds: chart }), |window| {
                for day in shaded_days(scale, gantt_x, width) {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(
                                chart.origin.x + px(day as f32 * DAY_W - gantt_x),
                                chart.origin.y,
                            ),
                            size: size(px(DAY_W), chart.size.height),
                        },
                        weekend_fill(pal),
                    ));
                }
                for x in day_lines(scale, gantt_x, width) {
                    vline(window, chart.origin.x + px(x), day_rule);
                }
                for y in &rules {
                    hline(window, chart.origin.x, chart.size.width, *y);
                }
            });
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
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
) -> impl IntoElement {
    let strip = div().relative().w(px(scale.width())).h(px(ROW_H));
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
