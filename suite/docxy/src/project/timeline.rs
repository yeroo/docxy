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

/// The ruler for the plan as displayed: the project start to the displayed
/// (leveled when leveling is on) finish.
pub(crate) fn timeline_ruler(ed: &ProjectEditor, width: f32) -> TimelineRuler {
    ruler(ed.schedule().project_start, ed.disp_project_finish(), width)
}

/// Days `[start_day, finish_day]` spread over `width` px, ticked in the finest
/// unit whose neighbouring ticks keep that unit's `min_gap` apart. Years thin
/// out to every n-th year when even one per year is too dense.
pub(crate) fn ruler(start: DateTime, finish: DateTime, width: f32) -> TimelineRuler {
    let first = start.day_number();
    let days = (finish.day_number() - first + 1).max(1);
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

/// The ruler's drawable width inside a Timeline pane `pane_w` wide.
pub(crate) fn ruler_width(pane_w: f32) -> f32 {
    (pane_w - LABEL_W - 2. * PAD).max(0.)
}

pub(crate) fn timeline_state(v: &ProjectView) -> Vec<(String, ctlcore::json::Json)> {
    use ctlcore::json::Json;
    let r = timeline_ruler(&v.ed, ruler_width(v.width));
    vec![
        (
            "timeline".into(),
            Json::Str(if v.timeline { "shown" } else { "hidden" }.into()),
        ),
        ("timeline_start".into(), Json::Str(r.start)),
        ("timeline_finish".into(), Json::Str(r.finish)),
    ]
}

pub(crate) fn timeline_el(
    view: &ProjectView,
    pal: Pal,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
) -> impl IntoElement {
    let w = ruler_width(view.width);
    let r = timeline_ruler(&view.ed, w);
    let caption = |top: &'static str, date: String, end: bool| {
        v_flex()
            .when(end, |d| d.items_end())
            .text_size(px(10.))
            .line_height(px(12.))
            .child(div().text_color(pal.dim).child(top))
            .child(div().font_weight(FontWeight::BOLD).child(date))
    };
    h_flex()
        .relative()
        .h(px(TIMELINE_H))
        .flex_none()
        .bg(pal.panel)
        .border_b_1()
        .border_color(pal.border)
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
                .text_size(px(9.))
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
                        })),
                )
                .child(
                    h_flex()
                        .w(px(w))
                        .h(px(BAR_H))
                        .flex_none()
                        .px(px(6.))
                        .justify_between()
                        .items_center()
                        .rounded(px(3.))
                        .bg(pal.sel)
                        .child(caption("Start", r.start, false))
                        .child(caption("Finish", r.finish, true)),
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
