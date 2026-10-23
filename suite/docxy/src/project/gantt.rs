//! Day-scale geometry shared by the renderer and harness.
use super::barstyle::{BarStyled, styled_children};
use super::rolled::RolledChild;
use super::*;
use projcore::formatting::gridlines::{Gridline, Gridlines, Line, LineType};

/// The pixels one day is wide at the default zoom (the Days level); a view's live
/// value is [`GanttScale::day_w`].
pub(crate) const DEFAULT_DAY_W: f32 = 22.;

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
    pub inactive: bool,
    pub start: i64,
    pub end: i64,
    pub baseline: Option<(i64, i64)>,
    pub delay: Option<(i64, i64)>,
    /// A manual summary's rolled-up span, drawn beside its own dates.
    pub rollup: Option<(i64, i64)>,
    /// Project's warning on a manual summary: its subtasks finish after its
    /// own finish, or it finishes after its manual parent summary's.
    pub warning: bool,
    /// Layout's bar height and round-to-days settings (#1695).
    pub geom: BarGeom,
}

/// The Layout dialog's drawing settings that shape one bar (#1695). The
/// default is the built-in 14 px bar in whole days.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct BarGeom {
    /// The bar's height in px (`bar_height` points at 96/72); `None` is the
    /// built-in 14 px, which is not 12 pt.
    pub height: Option<f32>,
    /// With round bars off: the bar's start and finish as fractional days
    /// from the scale's origin (a milestone's both its start).
    pub exact: Option<(f32, f32)>,
}

/// The built-in bar height, px.
pub(crate) const BAR_PX: f32 = 14.;

/// One bar's boxes within its row (`y`, `h`), from the bar height: a leaf
/// bar centred on y 12, a summary and a thin bar on y 14, a milestone's
/// diamond on y 12. The default height gives the original 5/14, 9/10,
/// 11/6 and 5/14.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BarBoxes {
    pub leaf: (f32, f32),
    pub thin: (f32, f32),
    pub summary: (f32, f32),
    /// The summary's top bar height.
    pub summary_top: f32,
    /// The summary's end posts' width.
    pub post: f32,
    /// The diamond's square box: top and side.
    pub diamond: (f32, f32),
}

/// `BarBoxes` for a bar `height` px tall (`None` is the built-in), capped at
/// the row and kept inside it.
pub(crate) fn bar_boxes(height: Option<f32>) -> BarBoxes {
    let h = height.unwrap_or(BAR_PX).clamp(1., ROW_H);
    let s = h / BAR_PX;
    let boxed = |centre: f32, size: f32| {
        let size = size.min(ROW_H);
        ((centre - size / 2.).clamp(0., ROW_H - size), size)
    };
    let summary_h = (10. * s).round().max(1.);
    BarBoxes {
        leaf: boxed(12., h),
        thin: boxed(14., (6. * s).round().max(1.)),
        summary: boxed(14., summary_h),
        summary_top: (6. * s).round().max(1.).min(summary_h),
        post: 3.,
        diamond: (boxed(12., h).0, h),
    }
}

/// A bar kind's name as the UI plugins know it (#1532).
pub(crate) fn bar_kind_name(kind: BarKind) -> &'static str {
    match kind {
        BarKind::Critical => "critical",
        BarKind::OnTrack => "on-track",
        BarKind::Summary => "summary",
        BarKind::Milestone => "milestone",
    }
}

/// The left of `bar`'s milestone diamond: centred in its day, or on its
/// start instant with round bars off.
pub(crate) fn milestone_left(bar: &GanttBar, day_w: f32) -> f32 {
    let half = bar_boxes(bar.geom.height).diamond.1 / 2.;
    match bar.geom.exact {
        Some((s, _)) => s * day_w - half,
        None => bar.start as f32 * day_w + day_w / 2. - half,
    }
}

/// The left and width of `bar`'s span: whole days, or its instants.
pub(crate) fn span_x(bar: &GanttBar, day_w: f32) -> (f32, f32) {
    match bar.geom.exact {
        Some((s, e)) => (s * day_w, ((e - s) * day_w).max(1.)),
        None => (
            bar.start as f32 * day_w,
            (bar.end - bar.start + 1).max(1) as f32 * day_w,
        ),
    }
}

/// The rectangle `gantt_strip` draws `bar` in, `(x, y, w, h)` within its
/// row: a leaf bar, a summary's thinner bar, or a milestone's diamond box.
pub(crate) fn bar_rect(bar: &GanttBar, day_w: f32) -> (f32, f32, f32, f32) {
    let boxes = bar_boxes(bar.geom.height);
    let (x, w) = span_x(bar, day_w);
    match bar.kind {
        BarKind::Summary => (x, boxes.summary.0, w, boxes.summary.1),
        BarKind::Milestone => (
            milestone_left(bar, day_w),
            boxes.diamond.0,
            boxes.diamond.1,
            boxes.diamond.1,
        ),
        BarKind::Critical | BarKind::OnTrack => (x, boxes.leaf.0, w, boxes.leaf.1),
    }
}

/// How a bar is drawn: its colour and shape, from the plan's palette (a UI
/// plugin's, #1532) when it names the kind, else the built-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarShape {
    /// A filled bar.
    Bar,
    /// An outlined bar.
    Hollow,
    /// A thin bar in the middle of the row.
    Thin,
    /// A summary's bar with its end posts.
    Summary,
    /// A milestone diamond.
    Diamond,
}

/// `kind`'s colour and shape under `palette` (`None` is the built-in look).
pub(crate) fn bar_look(
    kind: BarKind,
    palette: Option<&projcore::uiplugin::Palette>,
) -> (u32, BarShape) {
    let builtin_shape = match kind {
        BarKind::Summary => BarShape::Summary,
        BarKind::Milestone => BarShape::Diamond,
        BarKind::Critical | BarKind::OnTrack => BarShape::Bar,
    };
    let Some(style) = palette.and_then(|p| p.bars.iter().find(|b| b.kind == bar_kind_name(kind)))
    else {
        return (bar_kind_color(kind), builtin_shape);
    };
    let shape = match style.shape.as_str() {
        "hollow" => BarShape::Hollow,
        "thin" => BarShape::Thin,
        "summary" => BarShape::Summary,
        "diamond" => BarShape::Diamond,
        "bar" => BarShape::Bar,
        _ => builtin_shape,
    };
    (style.color, shape)
}

impl GanttBar {
    pub fn state(self) -> String {
        let kind = match self.kind {
            BarKind::Critical => "critical",
            BarKind::OnTrack => "on-track",
            BarKind::Summary => "summary",
            BarKind::Milestone => "milestone",
        };
        let mut state = format!("{kind} {}-{}", self.start, self.end);
        if let Some((s, e)) = self.rollup {
            state += &format!(" rollup {s}-{e}");
        }
        if self.warning {
            state += " warning";
        }
        if self.inactive {
            state += " inactive";
        }
        state
    }

    /// The days a warned bar draws in the warning colour: its rollup's days
    /// past its own finish, else its own finish day. That covers subtasks
    /// finishing later on that same day, and a summary that warns because it
    /// finishes after its manual parent while its rollup stays inside it.
    pub fn warning_days(self) -> Option<(i64, i64)> {
        if !self.warning {
            return None;
        }
        Some(match self.rollup {
            Some((s, e)) if e > self.end => (s.max(self.end + 1), e),
            _ => (self.end, self.end),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GanttScale {
    pub origin_day: i64,
    pub days: i64,
    /// Pixels per day: [`DEFAULT_DAY_W`] until the view is zoomed.
    pub day_w: f32,
}

impl GanttScale {
    pub fn width(self) -> f32 {
        self.days as f32 * self.day_w
    }

    /// The chart offset at which day number `day` starts.
    pub fn x(self, day: i64) -> f32 {
        (day - self.origin_day) as f32 * self.day_w
    }

    #[cfg(test)]
    fn date(self, day: i64) -> projcore::DateTime {
        projcore::DateTime::from_minutes((self.origin_day + day) * 1440)
    }

    pub(super) fn visible(self, offset: f32, width: f32) -> std::ops::Range<i64> {
        (offset / self.day_w).floor().max(0.) as i64
            ..(((offset + width) / self.day_w).ceil() as i64).min(self.days)
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
        day_w: DEFAULT_DAY_W,
    }
}

pub(crate) fn gantt_bar(ed: &ProjectEditor, task: &Task, scale: GanttScale) -> Option<GanttBar> {
    let result = ed.schedule().get(task.uid)?;
    let start = ed.disp_start(task.uid)?;
    let finish = ed.disp_finish(task.uid)?;
    let day = |dt: projcore::DateTime| dt.day_number() - scale.origin_day;
    let layout = &ed.project().formatting.layout;
    let edge = |dt: projcore::DateTime| {
        (dt.minutes() - scale.origin_day * 1440) as f32 / 1440.
    };
    let milestone = !task.summary && task.is_milestone();
    let inactive = ed
        .project()
        .tasks
        .iter()
        .position(|t| t.uid == task.uid)
        .is_some_and(|i| inactive_row(ed.project(), i));
    Some(GanttBar {
        inactive,
        kind: if task.summary {
            BarKind::Summary
        } else if task.is_milestone() {
            BarKind::Milestone
        } else if result.critical && !inactive {
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
        rollup: task
            .manual_summary_dates()
            .and(ed.disp_rollup(task.uid))
            .map(|(s, e)| (day(s), day(e))),
        // Leaf rows do not show the warning yet, as in yppxy.
        warning: task.summary && ed.summary_warning(task.uid),
        geom: BarGeom {
            height: layout.bar_height_px(),
            exact: (!layout.rounds_bars()).then(|| {
                let s = edge(start);
                (s, if milestone { s } else { edge(finish) })
            }),
        },
    })
}

/// `bar` as drawn under Gantt Chart Format › Bar Styles: with Critical Tasks
/// off a critical bar takes the normal bar colour, and with Baseline off no
/// baseline is drawn. Summaries, milestones and the leveling delay are kept.
pub(crate) fn styled_bar(bar: GanttBar, show_critical: bool, show_baseline: bool) -> GanttBar {
    GanttBar {
        kind: if bar.kind == BarKind::Critical && !show_critical {
            BarKind::OnTrack
        } else {
            bar.kind
        },
        baseline: bar.baseline.filter(|_| show_baseline),
        ..bar
    }
}

#[cfg(test)]
pub(crate) fn table_pane_width(width: f32) -> f32 {
    table_pane_width_for(width, TABLE_W)
}

/// The share of the window the table pane takes by default (#1534): the Entry
/// table's default columns fit it at the default 1180 px window.
pub(crate) const DEFAULT_TABLE_SHARE: f32 = 0.75;

/// [`table_pane_width`] for columns `total` px wide in all: the pane follows
/// them, so narrowing columns brings the right-hand ones into view.
pub(crate) fn table_pane_width_for(width: f32, total: f32) -> f32 {
    (width * DEFAULT_TABLE_SHARE)
        .max(320.)
        .min(total)
        .min(width.max(0.))
}

/// The split bar never narrows the table below the ID column's default width
/// (the live width, `ProjectView::widths()[0]`, in production), and leaves the
/// chart some days.
#[cfg(test)]
pub(crate) const MIN_TABLE_W: f32 = DEFAULT_WIDTHS[0];
pub(crate) const MIN_CHART_W: f32 = 48.;

/// The table pane's width for a user split, or the default without one. When the
/// window is too narrow for both minimums the chart's yields first.
#[cfg(test)]
pub(crate) fn split_table_width(width: f32, split: Option<f32>) -> f32 {
    split_table_width_for(width, split, TABLE_W, MIN_TABLE_W)
}

/// [`split_table_width`] for columns `total` px wide in all, whose ID column
/// is `min_table` wide: the split bar never hides it.
pub(crate) fn split_table_width_for(
    width: f32,
    split: Option<f32>,
    total: f32,
    min_table: f32,
) -> f32 {
    let Some(w) = split else {
        return table_pane_width_for(width, total);
    };
    w.min(total)
        .min(width - GANTT_INSET - SCROLLBAR_W - MIN_CHART_W)
        .max(min_table)
        .min(width.max(0.))
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

/// Non-working project-calendar days in the visible range.
pub(crate) fn shaded_days(
    scale: GanttScale,
    cal: Option<&projcore::WorkCalendar>,
    offset: f32,
    width: f32,
) -> Vec<i64> {
    let Some(cal) = cal else {
        return Vec::new();
    };
    scale
        .visible(offset, width)
        .filter(|d| cal.day(scale.origin_day + *d).is_empty())
        .collect()
}

/// Below this many pixels per day the day lines are left out: they would
/// be a wash of rules, and the header's tiers say where the days are.
pub(crate) const MIN_LINE_DAY_W: f32 = 8.;
/// Below this many pixels per day non-working days are not shaded one by one.
pub(crate) const MIN_SHADE_DAY_W: f32 = 3.;

/// The runs `(first, last)` of consecutive non-working days in view, one
/// rectangle each; none when a day is too narrow to tell apart.
pub(crate) fn shaded_runs(
    scale: GanttScale,
    cal: Option<&projcore::WorkCalendar>,
    offset: f32,
    width: f32,
) -> Vec<(i64, i64)> {
    if scale.day_w < MIN_SHADE_DAY_W {
        return Vec::new();
    }
    let mut runs: Vec<(i64, i64)> = Vec::new();
    for day in shaded_days(scale, cal, offset, width) {
        match runs.last_mut() {
            Some(run) if run.1 + 1 == day => run.1 = day,
            _ => runs.push((day, day)),
        }
    }
    runs
}

/// The bottom rule of each row in view, in body pixels, with its row (0 for
/// the first row of the body) for a gridline's interval (#1575). `scroll_y`
/// grows downward, so the rules follow the list's scroll phase whether a row
/// holds a task or not.
/// `row_h` is `ProjectView::row_h` (#1696).
pub(crate) fn ruled_rows_of(body_h: f32, scroll_y: f32, row_h: f32) -> Vec<(usize, f32)> {
    let scroll_y = scroll_y.max(0.);
    ((scroll_y / row_h).floor().max(0.) as usize..)
        .map(|k| (k, (k + 1) as f32 * row_h - 1. - scroll_y))
        .skip_while(|(_, y)| *y < 0.)
        .take_while(|(_, y)| *y < body_h)
        .collect()
}

/// A gridline's colour: its own, else the host's `automatic` for that line.
pub(crate) fn gridline_color(color: Option<u32>, automatic: Hsla) -> Hsla {
    color.map_or(automatic, crate::hsla_u)
}

/// Paint one rule of `line_type` from `origin`, `len` px long, 1 px thick,
/// horizontal or vertical: the type's dash pattern as a run of quads
/// (`LineType::segments`), a solid one as one. `None` paints nothing.
pub(crate) fn paint_rule(
    window: &mut Window,
    origin: Point<Pixels>,
    len: f32,
    vertical: bool,
    line_type: LineType,
    color: Hsla,
) {
    for (start, seg) in line_type.segments(len) {
        let (o, s) = if vertical {
            (point(origin.x, origin.y + px(start)), size(px(1.), px(seg)))
        } else {
            (point(origin.x + px(start), origin.y), size(px(seg), px(1.)))
        };
        window.paint_quad(fill(Bounds { origin: o, size: s }, color));
    }
}

/// The left edge of each visible cell of a timescale tier, relative to the
/// chart viewport, with the cell's ordinal from the scale's origin (#1575):
/// the Middle and Bottom Tier Column lines. The chart's header draws two
/// tiers (`zoom::tiers`, Project's middle and bottom), so the middle tier is
/// the major unit and the bottom the minor; the Top Tier Column has no tier
/// to draw on and is kept in the model undrawn, like Bar Rows.
pub(crate) fn tier_lines(
    scale: GanttScale,
    offset: f32,
    width: f32,
    minor: bool,
) -> Vec<(usize, f32)> {
    let level = &zoom::LEVELS[zoom::nearest(scale.day_w)];
    let unit = if minor { level.minor } else { level.major };
    // Within the viewport, and never past the scale's last day. The cells
    // are consecutive unit starts, so the walk from the origin happens once,
    // for the first, and the rest count up from it.
    let cells: Vec<zoom::Cell> = zoom::tier(scale, unit, offset, width)
        .into_iter()
        .filter(|c| c.x < scale.width())
        .collect();
    let Some(first) = cells.first() else {
        return Vec::new();
    };
    let base = zoom::tier_index(scale, unit, first.x);
    cells
        .into_iter()
        .enumerate()
        .map(|(k, c)| (base + k, c.x - offset))
        .filter(|(_, x)| (0. ..width).contains(x))
        .collect()
}

/// The Bottom Tier Column's lines (#1575): the bottom tier's cells, so a
/// line per day at the Days level and one per week at Weeks, as Project
/// draws them. A bottom tier left at its default keeps the rule from before
/// the model: nothing below [`MIN_LINE_DAY_W`] px per day, where day lines
/// would be a wash.
pub(crate) fn bottom_tier_lines(
    scale: GanttScale,
    offset: f32,
    width: f32,
    style: &Gridline,
) -> Vec<(usize, f32)> {
    let default = *style == Line::BottomTierColumn.default_style();
    if !style.draws() || (default && scale.day_w < MIN_LINE_DAY_W) {
        return Vec::new();
    }
    tier_lines(scale, offset, width, true)
}

/// The chart x of an instant, within its day, relative to the viewport.
pub(crate) fn instant_x(scale: GanttScale, offset: f32, at: projcore::DateTime) -> f32 {
    (at.minutes() - scale.origin_day * 1440) as f32 / 1440. * scale.day_w - offset
}

/// The chart's date lines (#1575): each line that draws, with its x in the
/// viewport, in list order. The current date is Project's `CurrentDate`
/// option, the status date the plan's `StatusDate` option (no fallback to
/// the current date: a plan without a status date draws no Status Date
/// line), the project start and finish the schedule's.
pub(crate) fn date_lines(
    ed: &projcore::editor::Editor,
    grid: &Gridlines,
    scale: GanttScale,
    offset: f32,
    width: f32,
) -> Vec<(Line, Gridline, f32)> {
    let proj = ed.project();
    let sched = ed.schedule();
    let date_option = |name: &str| proj.option(name).and_then(projcore::DateTime::parse_mspdi);
    [
        (Line::CurrentDate, date_option("CurrentDate")),
        (Line::ProjectStart, Some(sched.project_start)),
        (Line::ProjectFinish, Some(sched.project_finish)),
        (Line::StatusDate, date_option("StatusDate")),
    ]
    .into_iter()
    .filter_map(|(line, at)| {
        let style = grid.get(line);
        let x = instant_x(scale, offset, at?).floor();
        (style.line_type != LineType::None && (0. ..width).contains(&x)).then_some((line, style, x))
    })
    .collect()
}

/// Ruled empty rows visible below the last task, counting a partly visible one.
/// `row_h` is `ProjectView::row_h`.
pub(crate) fn filler_rows_of(body_h: f32, scroll_y: f32, count: usize, row_h: f32) -> usize {
    let used = count as f32 * row_h - scroll_y.max(0.);
    ((body_h - used).max(0.) / row_h).ceil() as usize
}

pub(crate) fn nonworking_fill(pal: Pal) -> Hsla {
    Hsla { a: 0.12, ..pal.dim }
}

fn backdrop(
    scale: GanttScale,
    cal: Option<projcore::WorkCalendar>,
    offset: f32,
    width: f32,
    pal: Pal,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for (first, last) in shaded_runs(scale, cal.as_ref(), offset, width) {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(
                            bounds.origin.x + px(first as f32 * scale.day_w),
                            bounds.origin.y,
                        ),
                        size: size(
                            px((last - first + 1) as f32 * scale.day_w),
                            bounds.size.height,
                        ),
                    },
                    nonworking_fill(pal),
                ));
            }
        },
    )
    .absolute()
    .size_full()
}

/// The grid behind every row of the body, task or empty: table rules and column dividers,
/// chart shading, day lines and rules. Painted once at full height so it fills the pane.
pub(crate) fn body_grid(view: &ProjectView, pal: Pal) -> impl IntoElement {
    let cal = view.ed.project().project_shading_calendar();
    // The plan's gridlines (#1575): each line's type, colour and interval;
    // the defaults are the rules drawn before the model.
    let grid = view.ed.project().formatting.gridlines.clone();
    let dates = date_lines(
        &view.ed,
        &grid,
        view.chart_scale(),
        view.gantt_x.get(),
        view.gantt_w,
    );
    // The handle the list tracks, so the rules cannot drift from the rows.
    let scroll = view.scroll.clone();
    // The same widths and offsets the rows are drawn at this render.
    let (table_w, gantt_w, table_x, gantt_x, scale) = (
        view.table_w,
        view.gantt_w,
        view.table_x.get(),
        view.gantt_x.get(),
        view.chart_scale(),
    );
    let widths = view.widths();
    let row_h = view.row_h();
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
            let rules = ruled_rows_of(body_h, scroll_y, row_h);
            // A row rule of `line` across `w` from `x`: the row's interval
            // style when the interval lands on it, else the normal one, in
            // the line's colour or the host's `automatic`.
            let hlines =
                |window: &mut Window, x: Pixels, w: Pixels, line: Line, automatic: Hsla| {
                    let style = grid.get(line);
                    for &(row, y) in &rules {
                        let (t, c) = style.style_at(row);
                        paint_rule(
                            window,
                            point(x, bounds.origin.y + px(y)),
                            f32::from(w),
                            false,
                            t,
                            gridline_color(c, automatic),
                        );
                    }
                };
            let vline = |window: &mut Window, x: Pixels, t: LineType, color: Hsla| {
                paint_rule(
                    window,
                    point(x, bounds.origin.y),
                    f32::from(bounds.size.height),
                    true,
                    t,
                    color,
                );
            };
            let table = Bounds {
                origin: bounds.origin,
                size: size(px(table_w).min(bounds.size.width), bounds.size.height),
            };
            window.with_content_mask(Some(ContentMask { bounds: table }), |window| {
                let columns = grid.get(Line::SheetColumns);
                let mut edge = 0.;
                for (k, w) in widths.into_iter().enumerate() {
                    edge += w;
                    let (t, c) = columns.style_at(k);
                    vline(
                        window,
                        bounds.origin.x + px(edge - table_x - 1.),
                        t,
                        gridline_color(c, rule),
                    );
                }
                hlines(
                    window,
                    bounds.origin.x,
                    table.size.width,
                    Line::SheetRows,
                    rule,
                );
            });
            // The rows' inset divider, continued below the last task.
            vline(
                window,
                bounds.origin.x + px(table_w),
                LineType::Solid,
                pal.border,
            );
            let Some(chart) = gantt_viewport(bounds, table_w, gantt_w) else {
                return;
            };
            let width = f32::from(chart.size.width);
            window.with_content_mask(Some(ContentMask { bounds: chart }), |window| {
                for (first, last) in shaded_runs(scale, cal.as_ref(), gantt_x, width) {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(
                                chart.origin.x + px(first as f32 * scale.day_w - gantt_x),
                                chart.origin.y,
                            ),
                            size: size(
                                px((last - first + 1) as f32 * scale.day_w),
                                chart.size.height,
                            ),
                        },
                        nonworking_fill(pal),
                    ));
                }
                // The tier columns (#1575): each line's interval index counts
                // from the scale's origin, so the pattern stays put as the
                // chart scrolls.
                let bottom = grid.get(Line::BottomTierColumn);
                for (k, x) in bottom_tier_lines(scale, gantt_x, width, &bottom) {
                    let (t, c) = bottom.style_at(k);
                    vline(
                        window,
                        chart.origin.x + px(x),
                        t,
                        gridline_color(c, day_rule),
                    );
                }
                // The major tier is the header's upper tier, Project's
                // middle; the Top Tier Column stays undrawn until a header
                // with three tiers exists.
                let middle = grid.get(Line::MiddleTierColumn);
                if middle.draws() {
                    for (k, x) in tier_lines(scale, gantt_x, width, false) {
                        let (t, c) = middle.style_at(k);
                        vline(window, chart.origin.x + px(x), t, gridline_color(c, rule));
                    }
                }
                hlines(
                    window,
                    chart.origin.x,
                    chart.size.width,
                    Line::GanttRows,
                    rule,
                );
                // The date lines (#1575): current and status date, project
                // start and finish, over the rules.
                for (_, style, x) in &dates {
                    vline(
                        window,
                        chart.origin.x + px(*x),
                        style.line_type,
                        gridline_color(style.color, pal.dim),
                    );
                }
            });
        },
    )
    .absolute()
    .size_full()
}

pub(crate) fn gantt_header(
    scale: GanttScale,
    cal: Option<projcore::WorkCalendar>,
    offset: f32,
    width: f32,
    pal: Pal,
    grid: &Gridlines,
) -> impl IntoElement {
    div()
        .relative()
        .w(px(scale.width()))
        .h(px(ROW_H))
        .child(backdrop(scale, cal, offset, width, pal))
        .children(header_cells(scale, offset, width, pal, grid))
}

/// The header's two tiers, 14 px each: the major unit over the minor, each
/// cell ruled on its left (the Title Vertical gridline) and labelled when
/// the label fits; the minor tier ruled on its top (Title Horizontal). A
/// title line draws solid whatever its type, in its colour (#1575).
fn header_cells(
    scale: GanttScale,
    offset: f32,
    width: f32,
    pal: Pal,
    grid: &Gridlines,
) -> Vec<Div> {
    let level = &zoom::LEVELS[zoom::nearest(scale.day_w)];
    let (major, minor) = zoom::tiers(scale, level, offset, width);
    let half = ROW_H / 2.;
    let rule = Hsla {
        a: pal.border.a * 0.6,
        ..pal.border
    };
    let vertical = grid.get(Line::TitleVertical);
    let horizontal = grid.get(Line::TitleHorizontal);
    let cell = |c: zoom::Cell, top: f32, bold: bool| {
        div()
            .absolute()
            .left(px(c.x))
            .top(px(top))
            .w(px(c.w))
            .h(px(half))
            .when(vertical.line_type != LineType::None, |d| {
                d.border_l_1()
                    .border_color(gridline_color(vertical.color, rule))
            })
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(px(10.))
            .line_height(px(half))
            .when(bold, |d| d.font_weight(FontWeight::BOLD))
            .child(div().absolute().left(px(c.at - c.x + 2.)).child(c.label))
    };
    let mut cells: Vec<Div> = major.into_iter().map(|c| cell(c, 0., true)).collect();
    // The Title Horizontal rule is a child of its own: gpui's border colour
    // is one value for all four sides, so a top border would recolour the
    // cell's Title Vertical left rule.
    cells.extend(minor.into_iter().map(|c| {
        cell(c, half, false).when(horizontal.line_type != LineType::None, |d| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(1.))
                    .bg(gridline_color(horizontal.color, rule)),
            )
        })
    }));
    cells
}

/// The leveling delay and the baseline under a bar: the palette's marks.
fn delay_and_baseline(mut strip: Div, bar: GanttBar, dw: f32, pal: Pal) -> Div {
    let span = |s: i64, e: i64, y: f32, h: f32, color: Hsla| {
        div()
            .absolute()
            .left(px(s as f32 * dw))
            .top(px(y))
            .w(px((e - s + 1).max(1) as f32 * dw))
            .h(px(h))
            .bg(color)
    };
    if let Some((s, e)) = bar.delay {
        strip = strip.child(
            div()
                .absolute()
                .left(px(s as f32 * dw))
                .top(px(8.))
                .w(px((e - s).max(0) as f32 * dw))
                .h(px(8.))
                .bg(Hsla { a: 0.4, ..pal.dim }),
        );
    }
    if let Some((s, e)) = bar.baseline {
        strip = strip.child(span(s, e, 22., 4., pal.dim));
    }
    strip
}

/// One row's Gantt strip. `on_right` is the bar's own right-press (#1207):
/// it is attached to the drawn bar (or milestone diamond) only, so a press
/// on the chart beside it is not the bar's.
pub(crate) fn gantt_strip(
    bar: Option<GanttBar>,
    id: i32,
    scale: GanttScale,
    pal: Pal,
    palette: Option<&projcore::uiplugin::Palette>,
    probes: &std::rc::Rc<std::cell::RefCell<Probes>>,
    row_h: f32,
    styled: Option<BarStyled>,
    rolled: Option<&[RolledChild]>,
    on_right: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    on_chart_right: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // The strip itself takes a right-press beside the bar: the chart's own
    // menu (#1532). The bar's press stops before it.
    let strip = div()
        .relative()
        .w(px(scale.width()))
        .h(px(row_h))
        .on_mouse_down(MouseButton::Right, on_chart_right);
    // The plan's bar style rows replace the palette drawing of a task they
    // cover (#1696); an override over a plain leaf bar keeps its baseline
    // and leveling-delay marks.
    if let Some(styled) = styled {
        let strip = match bar.filter(|_| styled.marks) {
            Some(bar) => delay_and_baseline(strip, bar, scale.day_w, pal),
            None => strip,
        };
        return strip.children(styled_children(
            styled.parts,
            id,
            pal,
            probes,
            std::rc::Rc::new(on_right),
        ));
    }
    let Some(bar) = bar else {
        return strip;
    };
    let (color, shape) = bar_look(bar.kind, palette);
    let dw = scale.day_w;
    let boxes = bar_boxes(bar.geom.height);
    let (bx, bw) = span_x(&bar, dw);
    let at = |x: f32, w: f32, y: f32, h: f32, color: Hsla, hollow: bool| {
        div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .when(hollow, |d| d.border_1().border_color(color))
            .when(!hollow, |d| d.bg(color))
    };
    let span = |s: i64, e: i64, y: f32, h: f32, color: Hsla, hollow: bool| {
        at(
            s as f32 * dw,
            (e - s + 1).max(1) as f32 * dw,
            y,
            h,
            color,
            hollow,
        )
    };
    let strip = delay_and_baseline(strip, bar, dw, pal);
    let mut strip = strip;
    // With Layout's roll-up on, the summary's children are drawn instead of
    // a manual summary's own rolled-up strip (#1695).
    let own_rollup = bar.rollup.filter(|_| rolled.is_none());
    if bar.inactive {
        if let Some((s, e)) = own_rollup {
            strip = strip.child(span(s, e, 3., 4., hsla_u(color), true));
        }
        if let Some((s, e)) = bar.warning_days() {
            strip = strip.child(span(s, e, 3., 4., hsla_u(GANTT_WARNING), true));
        }
        let marker = probe(probes, format!("bar:{id}"));
        let outline = if shape == BarShape::Diamond {
            let (y, size) = boxes.diamond;
            div()
                .absolute()
                .left(px(milestone_left(&bar, dw)))
                .top(px(y))
                .size(px(size))
                .child(milestone_diamond(color, true))
                .child(marker)
        } else {
            let (y, h) = if shape == BarShape::Summary {
                boxes.summary
            } else {
                boxes.leaf
            };
            at(bx, bw, y, h, hsla_u(color), true).child(marker)
        };
        return strip
            .child(outline.on_mouse_down(MouseButton::Right, on_right))
            .children(rolled_children(rolled, bar, palette, pal));
    }
    // A manual summary's rollup: a thin bar above its own. Its warning marks
    // the part past its finish, else its own finish day (an overrun within
    // that day, or a finish past its manual parent), in the warning colour.
    if let Some((s, e)) = own_rollup {
        strip = strip.child(span(
            s,
            e,
            3.,
            3.,
            Hsla {
                a: 0.6,
                ..hsla_u(GANTT_SUMMARY)
            },
            false,
        ));
    }
    if let Some((s, e)) = bar.warning_days() {
        strip = strip.child(span(s, e, 3., 3., hsla_u(GANTT_WARNING), false));
    }
    let marker = probe(probes, format!("bar:{id}"));
    let element = match shape {
        BarShape::Bar => at(bx, bw, boxes.leaf.0, boxes.leaf.1, hsla_u(color), false).child(marker),
        BarShape::Hollow => {
            at(bx, bw, boxes.leaf.0, boxes.leaf.1, hsla_u(color), true).child(marker)
        }
        BarShape::Thin => at(bx, bw, boxes.thin.0, boxes.thin.1, hsla_u(color), false).child(marker),
        BarShape::Summary => div()
            .absolute()
            .left(px(bx))
            .top(px(boxes.summary.0))
            .w(px(bw))
            .h(px(boxes.summary.1))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .w_full()
                    .h(px(boxes.summary_top))
                    .bg(hsla_u(color)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .w(px(boxes.post))
                    .h_full()
                    .bg(hsla_u(color)),
            )
            .child(
                div()
                    .absolute()
                    .right_0()
                    .w(px(boxes.post))
                    .h_full()
                    .bg(hsla_u(color)),
            )
            .child(marker),
        BarShape::Diamond => div()
            .absolute()
            .left(px(milestone_left(&bar, dw)))
            .top(px(boxes.diamond.0))
            .size(px(boxes.diamond.1))
            .child(milestone_diamond(color, false))
            .child(marker),
    };
    // The children are painted after the summary, over it.
    strip
        .child(element.on_mouse_down(MouseButton::Right, on_right))
        .children(rolled_children(rolled, bar, palette, pal))
}

/// A summary's rolled-up children (#1695): thin bars and diamonds centred on
/// the summary bar's centre line, each with a 1 px outline in the panel colour
/// so it reads over the summary's own colour. They take no press.
fn rolled_children(
    rolled: Option<&[RolledChild]>,
    bar: GanttBar,
    palette: Option<&projcore::uiplugin::Palette>,
    pal: Pal,
) -> Vec<Div> {
    let s = bar.geom.height.unwrap_or(BAR_PX).clamp(1., ROW_H) / BAR_PX;
    rolled
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let (color, _) = bar_look(c.kind, palette);
            let (h, w) = match c.kind {
                BarKind::Milestone => ((8. * s).round().max(2.), (8. * s).round().max(2.)),
                _ => ((4. * s).round().max(1.), c.w),
            };
            let (x, y) = match c.kind {
                BarKind::Milestone => (c.x - w / 2., 14. - h / 2.),
                _ => (c.x, 14. - h / 2.),
            };
            let el = div().absolute().left(px(x)).top(px(y)).w(px(w)).h(px(h));
            match c.kind {
                // A panel-coloured diamond one px larger behind it.
                BarKind::Milestone => {
                    let panel: Rgba = pal.panel.into();
                    let channel = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
                    let panel = channel(panel.r) << 16 | channel(panel.g) << 8 | channel(panel.b);
                    div()
                        .absolute()
                        .left(px(x - 1.))
                        .top(px(y - 1.))
                        .size(px(w + 2.))
                        .child(milestone_diamond(panel, false))
                        .child(el.left(px(1.)).top(px(1.)).child(milestone_diamond(color, false)))
                }
                _ => el
                    .bg(hsla_u(color))
                    .border_1()
                    .border_color(pal.panel),
            }
        })
        .collect()
}

/// The UI plugins' overlay (#1532): their shapes, in chart pixels (x from
/// the scale's origin, y from the top of the first row), painted over the
/// rows after the bars, scrolled with them and clipped to the chart. The
/// shapes are asked for once per plan revision, scale and view switches
/// (`plugin_ui::overlay_shapes`); nothing is drawn without a UI plugin.
pub(crate) fn gantt_overlay(view: &ProjectView, pal: Pal) -> Option<impl IntoElement> {
    let shapes = crate::plugin_ui::overlay_shapes(view);
    if shapes.is_empty() {
        return None;
    }
    let scroll = view.scroll.clone();
    let (table_w, gantt_w, gantt_x) = (view.table_w, view.gantt_w, view.gantt_x.get());
    let font = Font {
        family: "Segoe UI".into(),
        ..Default::default()
    };
    let _ = pal;
    Some(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let scroll_y = -f32::from(scroll.0.borrow().base_handle.offset().y);
                let Some(chart) = gantt_viewport(bounds, table_w, gantt_w) else {
                    return;
                };
                // Chart pixels to window pixels.
                let ox = f32::from(chart.origin.x) - gantt_x;
                let oy = f32::from(bounds.origin.y) - scroll_y;
                let at = |(x, y): (f32, f32)| point(px(ox + x), px(oy + y));
                window.with_content_mask(Some(ContentMask { bounds: chart }), |window| {
                    for shape in shapes.iter() {
                        paint_shape(window, cx, shape, &at, &font);
                    }
                });
            },
        )
        .absolute()
        .size_full(),
    )
}

/// Paint one overlay shape, `at` mapping chart pixels to window pixels.
fn paint_shape(
    window: &mut Window,
    cx: &mut App,
    shape: &projcore::uiplugin::Shape,
    at: &dyn Fn((f32, f32)) -> Point<Pixels>,
    font: &Font,
) {
    use projcore::uiplugin::Shape;
    let stroke = |window: &mut Window, points: &[(f32, f32)], color: u32, width: f32| {
        if points.len() < 2 {
            return;
        }
        let mut path = PathBuilder::stroke(px(width.max(0.5)));
        path.move_to(at(points[0]));
        for p in &points[1..] {
            path.line_to(at(*p));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, rgb(color));
        }
    };
    match shape {
        Shape::Polyline {
            points,
            color,
            width,
        } => stroke(window, points, *color, *width),
        Shape::Dashed {
            points,
            color,
            width,
        } => {
            // Dashes of 4 px with 3 px gaps along each segment.
            for pair in points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = (dx * dx + dy * dy).sqrt();
                if len <= 0. {
                    continue;
                }
                let (ux, uy) = (dx / len, dy / len);
                let mut t = 0.;
                while t < len {
                    let end = (t + 4.).min(len);
                    stroke(
                        window,
                        &[
                            (a.0 + ux * t, a.1 + uy * t),
                            (a.0 + ux * end, a.1 + uy * end),
                        ],
                        *color,
                        *width,
                    );
                    t += 7.;
                }
            }
        }
        Shape::Arrow {
            from,
            to,
            color,
            width,
        } => {
            stroke(window, &[*from, *to], *color, *width);
            let (dx, dy) = (to.0 - from.0, to.1 - from.1);
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            let (ux, uy) = (dx / len, dy / len);
            let size = 6.;
            let base = (to.0 - ux * size, to.1 - uy * size);
            let (nx, ny) = (-uy * size / 2., ux * size / 2.);
            let mut path = PathBuilder::fill();
            path.move_to(at(*to));
            path.line_to(at((base.0 + nx, base.1 + ny)));
            path.line_to(at((base.0 - nx, base.1 - ny)));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(*color));
            }
        }
        Shape::Bracket {
            x0,
            x1,
            y,
            tick,
            color,
            width,
        } => stroke(
            window,
            &[(*x0, y + tick), (*x0, *y), (*x1, *y), (*x1, y + tick)],
            *color,
            *width,
        ),
        Shape::Rect {
            x,
            y,
            w,
            h,
            color,
            filled,
        } => {
            let bounds = Bounds {
                origin: at((*x, *y)),
                size: size(px(*w), px(*h)),
            };
            if *filled {
                window.paint_quad(fill(bounds, rgb(*color)));
            } else {
                stroke(
                    window,
                    &[(*x, *y), (x + w, *y), (x + w, y + h), (*x, y + h), (*x, *y)],
                    *color,
                    1.,
                );
            }
        }
        Shape::Text {
            x,
            y,
            text,
            color,
            size: text_size,
        } => {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: rgb(*color).into(),
                ..Default::default()
            };
            let line = window.text_system().shape_line(
                SharedString::from(text.clone()),
                px(*text_size),
                &[run],
                None,
            );
            let _ = line.paint(
                at((*x, *y)),
                px(text_size * 1.3),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

fn milestone_diamond(color: u32, hollow: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let mut path = if hollow {
                PathBuilder::stroke(px(1.))
            } else {
                PathBuilder::fill()
            };
            let x = b.origin.x;
            let y = b.origin.y;
            let (w, h) = (b.size.width, b.size.height);
            path.move_to(point(x + w / 2., y));
            path.line_to(point(x + w, y + h / 2.));
            path.line_to(point(x + w / 2., y + h));
            path.line_to(point(x, y + h / 2.));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
        },
    )
    .size_full()
}

fn bar_kind_color(kind: BarKind) -> u32 {
    match kind {
        BarKind::Critical => GANTT_CRIT,
        BarKind::OnTrack => BRAND,
        BarKind::Summary => GANTT_SUMMARY,
        BarKind::Milestone => GANTT_MILESTONE,
    }
}

#[cfg(test)]
mod tests;
