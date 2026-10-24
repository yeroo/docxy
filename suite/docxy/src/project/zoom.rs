//! The Gantt's timescale (#1535): Project's zoom levels, the two header tiers
//! each draws, and the view's zoom commands. The maths is pure (`Level`,
//! `nearest`, `step`, `fit`, `tiers`) so it is tested without a
//! window; [`ProjectView::set_zoom`] is the one place a zoom reaches the view.
use super::*;
use projcore::DateTime;

/// The narrowest and widest a day is drawn.
pub(crate) const MIN_DAY_W: f32 = 0.25;
pub(crate) const MAX_DAY_W: f32 = 96.;

/// What a Ctrl+wheel event did ([`ProjectView::wheel_zoom`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WheelZoom {
    /// Not over the chart: the wheel is not the zoom's.
    NotChart,
    /// Over the chart and counted towards a step that has not come yet.
    Absorbed,
    /// A level was stepped; the status says which.
    Stepped(String),
}

/// Whether a wheel delta is a notch: a whole line or more. Fractional lines (a
/// hi-res wheel, a touchpad) and pixel deltas are summed instead.
pub(crate) fn is_notch(delta: &ScrollDelta) -> bool {
    matches!(delta, ScrollDelta::Lines(l) if l.y.abs() >= 1.)
}

/// The pixel deltas of a smooth wheel that make one zoom step.
pub(crate) const WHEEL_STEP: f32 = 40.;

/// A header tier's unit: where one starts and what it is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Year,
    HalfYear,
    Quarter,
    Month,
    /// A third of a month: days 1 to 10, 11 to 20, and 21 to the end.
    Third,
    Week,
    Day,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

impl Unit {
    pub fn name(self) -> &'static str {
        match self {
            Self::Year => "Years",
            Self::HalfYear => "Half Years",
            Self::Quarter => "Quarters",
            Self::Month => "Months",
            Self::Third => "Thirds of Months",
            Self::Week => "Weeks",
            Self::Day => "Days",
        }
    }

    fn date(day: i64) -> DateTime {
        DateTime::from_minutes(day * 1440)
    }

    /// Whether day number `day` starts a unit.
    pub fn starts(self, day: i64) -> bool {
        let dt = Self::date(day);
        let p = dt.parts();
        match self {
            Self::Year => p.day == 1 && p.month == 1,
            Self::HalfYear => p.day == 1 && p.month % 6 == 1,
            Self::Quarter => p.day == 1 && p.month % 3 == 1,
            Self::Month => p.day == 1,
            Self::Third => matches!(p.day, 1 | 11 | 21),
            Self::Week => dt.weekday() == 1,
            Self::Day => true,
        }
    }

    /// The last day number at or before `day` that starts a unit.
    pub fn start_at_or_before(self, day: i64) -> i64 {
        (0..=400)
            .map(|back| day - back)
            .find(|d| self.starts(*d))
            .unwrap_or(day)
    }

    /// The first day number after `day` that starts a unit.
    pub fn next_start(self, day: i64) -> i64 {
        (day + 1..=day + 400)
            .find(|d| self.starts(*d))
            .unwrap_or(day + 1)
    }

    /// The unit starting on `day`, named from longest to shortest; the header
    /// takes the longest that fits.
    pub fn labels(self, day: i64) -> Vec<String> {
        let dt = Self::date(day);
        let p = dt.parts();
        let yy = p.year.rem_euclid(100);
        let month = MONTHS[p.month as usize - 1];
        match self {
            Self::Year => vec![p.year.to_string(), format!("'{yy:02}")],
            Self::HalfYear => vec![
                format!("H{} '{yy:02}", (p.month - 1) / 6 + 1),
                format!("H{}", (p.month - 1) / 6 + 1),
            ],
            Self::Quarter => vec![
                format!("Qtr {}, {}", (p.month - 1) / 3 + 1, p.year),
                format!("Q{} '{yy:02}", (p.month - 1) / 3 + 1),
                format!("Q{}", (p.month - 1) / 3 + 1),
            ],
            Self::Month => vec![
                format!("{month} {}", p.year),
                format!("{month} '{yy:02}"),
                month.to_string(),
                month[..1].to_string(),
            ],
            Self::Third => vec![
                ["Beg", "Mid", "End"][(p.day / 10).min(2) as usize].to_string(),
                ["B", "M", "E"][(p.day / 10).min(2) as usize].to_string(),
            ],
            Self::Week => vec![
                format!("{month} {}, {}", p.day, p.year),
                format!("{month} {} '{yy:02}", p.day),
                format!("{}/{}", p.month, p.day),
            ],
            Self::Day => {
                let name = WEEKDAYS[dt.weekday() as usize];
                vec![
                    format!("{name} {}", p.day),
                    name.to_string(),
                    name[..1].to_string(),
                ]
            }
        }
    }
}

/// One of Project's timescale levels: a fixed pixel width per day and the
/// units of its two header tiers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Level {
    pub name: &'static str,
    pub day_w: f32,
    pub major: Unit,
    pub minor: Unit,
}

/// The levels the Gantt can draw, widest span first (the pixels per day grow).
pub(crate) const LEVELS: [Level; 7] = [
    Level {
        name: "Years",
        day_w: 0.25,
        major: Unit::Year,
        minor: Unit::Quarter,
    },
    Level {
        name: "Half Years",
        day_w: 0.5,
        major: Unit::Year,
        minor: Unit::HalfYear,
    },
    Level {
        name: "Quarters",
        day_w: 1.,
        major: Unit::Year,
        minor: Unit::Quarter,
    },
    Level {
        name: "Months",
        day_w: 3.,
        major: Unit::Quarter,
        minor: Unit::Month,
    },
    Level {
        name: "Thirds of Months",
        day_w: 6.,
        major: Unit::Month,
        minor: Unit::Third,
    },
    Level {
        name: "Weeks",
        day_w: 12.,
        major: Unit::Month,
        minor: Unit::Week,
    },
    Level {
        name: "Days",
        day_w: DEFAULT_DAY_W,
        major: Unit::Week,
        minor: Unit::Day,
    },
];

/// The Timescale combo's entries, in Project's order, with whether each can be
/// chosen. Hours and Minutes need sub-day bars, which the Gantt does not draw.
pub(crate) const COMBO: [(&str, bool); 9] = [
    ("Years", true),
    ("Half Years", true),
    ("Quarters", true),
    ("Months", true),
    ("Thirds of Months", true),
    ("Weeks", true),
    ("Days", true),
    ("Hours", false),
    ("Minutes", false),
];

/// Whether combo entry `i` can be chosen: every level, not Hours or Minutes.
pub(crate) fn available(i: usize) -> bool {
    COMBO.get(i).is_some_and(|(_, on)| *on)
}

/// The level whose pixels per day are nearest `day_w` on a log scale.
pub(crate) fn nearest(day_w: f32) -> usize {
    let d = day_w.max(MIN_DAY_W).ln();
    (0..LEVELS.len())
        .min_by(|a, b| {
            (LEVELS[*a].day_w.ln() - d)
                .abs()
                .total_cmp(&(LEVELS[*b].day_w.ln() - d).abs())
        })
        .unwrap_or(0)
}

/// Whether `day_w` is a level's own width, not one a view box or a fit made.
pub(crate) fn is_preset(day_w: f32) -> bool {
    (LEVELS[nearest(day_w)].day_w - day_w).abs() < 1e-3
}

/// The combo's text for `day_w`: the level, or the nearest with `Custom`.
pub(crate) fn label(day_w: f32) -> String {
    let level = &LEVELS[nearest(day_w)];
    if is_preset(day_w) {
        level.name.to_string()
    } else {
        format!("Custom ({})", level.name)
    }
}

/// The width one step in (`dir` > 0) or out (`dir` < 0) from `day_w`: the next
/// level beyond it. `None` at the last level.
pub(crate) fn step(day_w: f32, dir: i32) -> Option<f32> {
    let eps = 1e-3;
    if dir > 0 {
        LEVELS.iter().map(|l| l.day_w).find(|w| *w > day_w + eps)
    } else {
        LEVELS
            .iter()
            .rev()
            .map(|l| l.day_w)
            .find(|w| *w < day_w - eps)
    }
}

/// The width that makes `days` fill `width` px.
pub(crate) fn fit(days: f32, width: f32) -> f32 {
    (width / days.max(1.)).clamp(MIN_DAY_W, MAX_DAY_W)
}

/// One cell of a header tier: `x` and `w` in chart pixels from the scale's
/// origin, the label already fitted to `w` (empty when none fits), and `at`
/// where the label starts (held at the visible edge while the cell runs past it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cell {
    pub x: f32,
    pub w: f32,
    pub at: f32,
    pub label: String,
}

/// Pixels a label character is given in the default 10 px tier text, and
/// the cell's padding.
pub(crate) const CHAR_W: f32 = 6.;
const CELL_PAD: f32 = 6.;

fn fitted(labels: Vec<String>, room: f32, char_w: f32) -> String {
    labels
        .into_iter()
        .find(|l| l.chars().count() as f32 * char_w + CELL_PAD <= room)
        .unwrap_or_default()
}

/// The cells of one tier over the chart offsets `[offset, offset + width)`,
/// each label given `char_w` px a character ([`CHAR_W`] for the default
/// text; a tier text style scales it, #1666).
pub(crate) fn tier(
    scale: GanttScale,
    unit: Unit,
    offset: f32,
    width: f32,
    char_w: f32,
) -> Vec<Cell> {
    let dw = scale.day_w;
    let first = scale.origin_day + (offset / dw).floor() as i64;
    let last = scale.origin_day + ((offset + width) / dw).ceil() as i64;
    let mut day = unit.start_at_or_before(first);
    let mut cells = Vec::new();
    while day < last && cells.len() < 4096 {
        let next = unit.next_start(day);
        let x = (day - scale.origin_day) as f32 * dw;
        let w = (next - day) as f32 * dw;
        // A cell cut by the left edge keeps its label in view when it fits there.
        let shown = x.max(offset);
        let room = x + w - shown;
        let label = fitted(unit.labels(day), room, char_w);
        let at = if label.is_empty() || x >= offset {
            x
        } else {
            shown
        };
        cells.push(Cell { x, w, at, label });
        day = next;
    }
    cells
}

/// The ordinal of the `unit` cell starting at chart `x` px, counted from
/// the cell at or before the scale's origin (0): a gridline's interval index
/// (#1575), the same wherever the chart is scrolled.
pub(crate) fn tier_index(scale: GanttScale, unit: Unit, x: f32) -> usize {
    let target = scale.origin_day + (x / scale.day_w).round() as i64;
    let mut day = unit.start_at_or_before(scale.origin_day);
    let mut k = 0;
    while day < target && k < 1 << 20 {
        day = unit.next_start(day);
        k += 1;
    }
    k
}

/// The major and minor tiers of `level` over the visible offsets, their
/// labels fitted at `char_w` (major, minor) px a character.
pub(crate) fn tiers(
    scale: GanttScale,
    level: &Level,
    offset: f32,
    width: f32,
    char_w: (f32, f32),
) -> (Vec<Cell>, Vec<Cell>) {
    (
        tier(scale, level.major, offset, width, char_w.0),
        tier(scale, level.minor, offset, width, char_w.1),
    )
}

impl ProjectView {
    /// The zoom in pixels per day now.
    pub fn day_w(&self) -> f32 {
        self.scale.day_w
    }

    /// Days the chart shows across its width, as the Zoom dialog's Custom field.
    pub fn visible_days(&self) -> f32 {
        self.gantt_w / self.day_w()
    }

    /// The one place a zoom reaches the view: `day_w` is clamped, and the chart
    /// is scrolled so day `anchor_day` (days from the scale's origin, whole or
    /// fractional) sits `anchor_px` from its left edge, within the scroll range.
    pub fn set_zoom(&mut self, day_w: f32, anchor_day: f32, anchor_px: f32) {
        let day_w = day_w.clamp(MIN_DAY_W, MAX_DAY_W);
        self.zoom_day_w = ((day_w - DEFAULT_DAY_W).abs() > 1e-4).then_some(day_w);
        self.refresh_schedule_layout();
        self.gantt_x.set(anchor_day * self.day_w() - anchor_px);
        self.clamp_offsets();
    }

    /// Zoom In (`dir` > 0) or Out (`dir` < 0) one level around `pointer_x`
    /// (px from the chart's left edge; 0 keeps the left-edge date). The status
    /// says why nothing changed at the last level.
    pub fn zoom_step(&mut self, dir: i32, pointer_x: f32) -> String {
        let old = self.day_w();
        let Some(new) = step(old, dir) else {
            return format!(
                "Already at the {} timescale",
                if dir > 0 { "narrowest" } else { "widest" }
            );
        };
        let day = (self.gantt_x.get() + pointer_x) / old;
        self.set_zoom(new, day, pointer_x);
        format!("Timescale: {}", label(self.day_w()))
    }

    /// Ctrl+wheel over the split area at `x` (px from its left edge), `dy` px
    /// of it (gpui's sign: up is positive): zoom one level around the pointer,
    /// in for a wheel turned up. Every delta adds into `wheel_acc` and a level
    /// steps per [`WHEEL_STEP`] px, so a trackpad swipe or a hi-res wheel does
    /// not jump levels; only a whole-line `notch` steps at once. A change of
    /// direction starts the sum afresh, and the touch phases `Started` and
    /// `Ended` clear it (even on a zero delta, which is how a gesture ends). The chart takes every event over it ([`WheelZoom`]).
    pub fn wheel_zoom(&mut self, x: f32, dy: f32, notch: bool, phase: TouchPhase) -> WheelZoom {
        if phase == TouchPhase::Started {
            self.wheel_acc = 0.;
        }
        let out = if dy == 0. || self.pane_at(x) != ProjectPane::Chart {
            WheelZoom::NotChart
        } else {
            let dir = if dy > 0. { 1 } else { -1 };
            let step = if notch {
                self.wheel_acc = 0.;
                true
            } else {
                if self.wheel_acc * dy < 0. {
                    self.wheel_acc = 0.;
                }
                self.wheel_acc += dy;
                let step = self.wheel_acc.abs() >= WHEEL_STEP;
                if step {
                    self.wheel_acc = 0.;
                }
                step
            };
            if step {
                let pointer = (x - self.table_w - GANTT_INSET).clamp(0., self.gantt_w);
                WheelZoom::Stepped(self.zoom_step(dir, pointer))
            } else {
                WheelZoom::Absorbed
            }
        };
        if phase == TouchPhase::Ended {
            self.wheel_acc = 0.;
        }
        out
    }

    /// Entire Project: the plan's span fills the chart.
    pub fn zoom_entire(&mut self) -> String {
        self.refresh_schedule_layout();
        let span = timeline::TimelineSpan::of(&self.ed);
        let first = (span.first - self.scale.origin_day) as f32;
        self.set_zoom(fit(span.days as f32, self.gantt_w), first, 0.);
        format!("Timescale: {}", label(self.day_w()))
    }

    /// Selected Tasks: the selected tasks' span fills the chart.
    pub fn zoom_selected(&mut self) -> Result<String, String> {
        self.refresh_schedule_layout();
        let uids = self.look().uids;
        let span = uids
            .iter()
            .filter_map(|uid| self.ed.project().task(*uid))
            .filter_map(|task| gantt_bar(&self.ed, task, self.scale))
            .fold(None, |acc: Option<(i64, i64)>, bar| {
                Some(acc.map_or((bar.start, bar.end), |(s, e)| {
                    (s.min(bar.start), e.max(bar.end))
                }))
            });
        let Some((start, end)) = span else {
            return Err("Select one or more tasks to zoom to".into());
        };
        let days = (end - start + 1) as f32;
        self.set_zoom(fit(days, self.gantt_w), start as f32, 0.);
        Ok(format!("Timescale: {}", label(self.day_w())))
    }

    /// Zoom dialog's Custom: `days` days across the chart, the left-edge date kept.
    pub fn zoom_to_days(&mut self, days: f32) -> String {
        let day = self.gantt_x.get() / self.day_w();
        self.set_zoom(fit(days, self.gantt_w), day, 0.);
        format!("Timescale: {}", label(self.day_w()))
    }

    /// The Timescale combo: choose `level`.
    pub fn zoom_to_level(&mut self, level: usize) -> String {
        let Some(level) = LEVELS.get(level) else {
            return "That timescale is not available".into();
        };
        let day = self.gantt_x.get() / self.day_w();
        self.set_zoom(level.day_w, day, 0.);
        format!("Timescale: {}", level.name)
    }
}

#[cfg(test)]
mod tests;
