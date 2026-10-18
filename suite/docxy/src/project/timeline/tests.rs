use super::*;
use core::prelude::v1::test;

fn day(y: i64, m: u32, d: u32) -> DateTime {
    DateTime::from_ymd_hm(y, m, d, 8, 0)
}

#[test]
fn timeline_shading_merges_off_days_and_respects_exceptions_and_density() {
    use projcore::{Calendar, CalendarException, DayWorking};
    let span = TimelineSpan::new(day(2026, 3, 2), day(2026, 3, 15));
    let mut cal = Calendar::standard(1);
    let wed = day(2026, 3, 4);
    let sat = day(2026, 3, 7);
    cal.exceptions.push(CalendarException::date_range(
        wed,
        wed,
        DayWorking::default(),
    ));
    cal.exceptions.push(CalendarException::date_range(
        sat,
        sat,
        cal.week[1].clone().unwrap(),
    ));
    let mut p = untitled_project();
    p.calendars = vec![cal];
    let runs = timeline_shading(span, p.project_shading_calendar().as_ref(), 280.);
    assert_eq!(runs, [(40., 20.), (120., 20.), (240., 40.)]);
    assert!(timeline_shading(span, p.project_shading_calendar().as_ref(), 20.).is_empty());
    assert!(
        runs.iter()
            .all(|(left, width)| *left >= 0. && *left + *width <= 280.)
    );
}

#[test]
fn timeline_state_exposes_current_holiday_shading() {
    use ctlcore::json::Json;
    use projcore::{Calendar, CalendarException, DayWorking};
    let mut p = untitled_project();
    p.start_date = Some(day(2026, 3, 2));
    p.tasks = vec![Task {
        uid: 1,
        id: 1,
        duration_min: 5 * 480,
        ..Task::default()
    }];
    let wed = day(2026, 3, 4);
    let mut cal = Calendar::standard(p.default_calendar_uid);
    cal.exceptions.push(CalendarException::date_range(
        wed,
        wed,
        DayWorking::default(),
    ));
    p.calendars = vec![cal];
    let mut v = ProjectView::new(p, false);
    v.width = 800.;
    let state = timeline_state(&v);
    let shaded = state.iter().find(|(key, _)| key == "nonworking").unwrap();
    assert!(
        matches!(&shaded.1, Json::Arr(runs) if runs.contains(&Json::Arr(vec![Json::Str("Wed 3/4/26".into()), Json::Str("Wed 3/4/26".into())])))
    );
}

#[test]
fn closed_project_week_has_no_timeline_shading() {
    use projcore::{Calendar, DayWorking};
    let span = TimelineSpan::new(day(2026, 3, 2), day(2026, 3, 15));
    let mut p = untitled_project();
    p.calendars = vec![Calendar::base(
        p.default_calendar_uid,
        "Closed",
        std::array::from_fn(|_| DayWorking::default()),
    )];
    assert!(timeline_shading(span, p.project_shading_calendar().as_ref(), 280.).is_empty());
}

fn corpus_editor(name: &str) -> ProjectEditor {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/mspdi")
        .join(name);
    let xml = std::fs::read_to_string(path).unwrap();
    ProjectEditor::new(projcore::mspdi::read_mspdi(&xml).unwrap())
}

/// The contract every ruler keeps, whatever its span and width.
fn assert_well_formed(r: &TimelineRuler, width: f32) {
    for (at, label) in &r.ticks {
        assert!(at.is_finite() && (0. ..1.).contains(at), "{at} {label}");
    }
    for w in r.ticks.windows(2) {
        assert!(w[0].0 < w[1].0, "strictly increasing: {w:?}");
        let gap = (w[1].0 - w[0].0) * width;
        assert!(
            gap >= r.unit.unwrap().min_gap() - 1e-3,
            "{gap}px < budget: {w:?}"
        );
    }
    assert!(!r.start.is_empty() && !r.finish.is_empty());
}

#[test]
fn dates_read_as_project_writes_them() {
    assert_eq!(project_date(day(2026, 3, 2)), "Mon 3/2/26");
    assert_eq!(project_date(day(2026, 3, 5)), "Thu 3/5/26");
    assert_eq!(project_date(day(2030, 12, 31)), "Tue 12/31/30");
    assert_eq!(project_date(day(2005, 1, 9)), "Sun 1/9/05");
}

#[test]
fn the_corpus_plan_spans_monday_to_thursday() {
    let ed = corpus_editor("02-link-fs.xml");
    let r = timeline_ruler(&ed, 1200.);
    assert_eq!(
        (r.start.as_str(), r.finish.as_str()),
        ("Mon 3/2/26", "Thu 3/5/26")
    );
    assert_eq!(r.unit, Some(TickUnit::Day));
    assert_eq!(
        r.ticks,
        vec![
            (0., "Mon 3/2".to_string()),
            (0.25, "Tue 3/3".to_string()),
            (0.5, "Wed 3/4".to_string()),
            (0.75, "Thu 3/5".to_string()),
        ]
    );
    assert_well_formed(&r, 1200.);
}

#[test]
fn the_unit_coarsens_as_the_span_grows_or_the_pane_narrows() {
    let start = day(2026, 3, 2);
    for (finish, width, unit) in [
        (start, 1200., TickUnit::Day),
        (start, 300., TickUnit::Day),
        (day(2026, 3, 5), 300., TickUnit::Day),
        (day(2026, 3, 5), 150., TickUnit::Week),
        (day(2026, 8, 31), 1200., TickUnit::Week),
        (day(2026, 8, 31), 600., TickUnit::Month),
        (day(2026, 8, 31), 300., TickUnit::Quarter),
        (day(2036, 3, 2), 300., TickUnit::Year),
    ] {
        let r = ruler(start, finish, width);
        assert_eq!(r.unit, Some(unit), "{} at {width}px", r.finish);
        assert!(!r.ticks.is_empty(), "{} at {width}px", r.finish);
        assert_well_formed(&r, width);
    }
    let r = ruler(start, day(2026, 8, 31), 300.);
    assert_eq!(
        r.ticks.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(),
        ["Q2 '26", "Q3 '26"]
    );
    let r = ruler(start, day(2026, 8, 31), 600.);
    assert_eq!(r.ticks[0].1, "Apr '26");
    let r = ruler(start, day(2026, 8, 31), 1200.);
    assert_eq!(r.ticks[0], (0., "3/2".to_string()));
}

#[test]
fn years_thin_out_on_a_long_plan_in_a_narrow_pane() {
    let r = ruler(day(2026, 1, 1), day(2075, 12, 31), 300.);
    assert_eq!(r.unit, Some(TickUnit::Year));
    assert!(r.ticks.len() < 50 && r.ticks.len() > 1, "{:?}", r.ticks);
    assert_well_formed(&r, 300.);
}

#[test]
fn a_degenerate_span_or_width_still_labels_both_ends() {
    let ed = ProjectEditor::new(untitled_project());
    assert_eq!(
        ed.schedule().project_start.day_number(),
        ed.disp_project_finish().day_number()
    );
    for width in [0., -5., 1., 1200.] {
        let r = timeline_ruler(&ed, width);
        assert_eq!(r.start, r.finish);
        assert_well_formed(&r, width);
    }
    assert!(timeline_ruler(&ed, 0.).ticks.is_empty());
    // A finish before the start (never scheduled, but cheap to survive).
    let r = ruler(day(2026, 3, 5), day(2026, 3, 2), 400.);
    assert_eq!(r.ticks, vec![(0., "Thu 3/5".to_string())]);
}

// The view box. A 40-day span on a chart whose scale starts on the span's first
// day and runs 7 days past it; a ruler 400 px wide is 10 px a day.
const SPAN: TimelineSpan = TimelineSpan {
    first: 100,
    days: 40,
};
const SCALE: GanttScale = GanttScale {
    origin_day: 100,
    days: 47,
    day_w: DEFAULT_DAY_W,
};
/// A baseline that starts 5 days before the displayed plan widens the scale.
const LEAD_SCALE: GanttScale = GanttScale {
    origin_day: 95,
    days: 52,
    day_w: DEFAULT_DAY_W,
};
const VIEW_W: f32 = 10. * DEFAULT_DAY_W;
const RULER_W: f32 = 400.;

fn assert_near(got: (f32, f32), want: (f32, f32)) {
    assert!(
        (got.0 - want.0).abs() < 1e-4 && (got.1 - want.1).abs() < 1e-4,
        "{got:?} != {want:?}"
    );
}

#[test]
fn the_view_box_is_the_charts_visible_days_on_the_span() {
    // The chart shows the whole plan.
    assert_eq!(view_box(SPAN, SCALE, 0., 50. * DEFAULT_DAY_W), (0., 1.));
    // Scrolled mid-plan: days 10..20 of 40.
    assert_near(
        view_box(SPAN, SCALE, 10. * DEFAULT_DAY_W, VIEW_W),
        (0.25, 0.5),
    );
    // Scrolled to the end of the scale, into the padding past the finish.
    let end = SCALE.width() - VIEW_W;
    assert_near(view_box(SPAN, SCALE, end, VIEW_W), (37. / 40., 1.));
    // The scale starts before the span: its first 5 days are off the Timeline.
    assert_near(view_box(SPAN, LEAD_SCALE, 0., VIEW_W), (0., 0.125));
    assert_near(view_box(SPAN, LEAD_SCALE, VIEW_W, VIEW_W), (0.125, 0.375));
}

#[test]
fn the_view_box_keeps_a_grabbable_width_inside_the_ruler() {
    assert_eq!(box_px(0.25, 0.5, RULER_W), (100., 100.));
    assert_eq!(box_px(1., 1., RULER_W), (RULER_W - MIN_BOX_W, MIN_BOX_W));
    assert_eq!(box_px(0., 0., RULER_W), (0., MIN_BOX_W));
    assert_eq!(box_px(0.99, 1., RULER_W), (RULER_W - MIN_BOX_W, MIN_BOX_W));
    assert_eq!(box_px(0., 1., RULER_W), (0., RULER_W));
    assert_eq!(box_px(0., 1., 0.), (0., 0.));
}

#[test]
fn the_view_days_are_the_first_and_last_day_the_chart_shows() {
    assert_eq!(
        view_days(SPAN, SCALE, 10. * DEFAULT_DAY_W, VIEW_W),
        (110, 119)
    );
    // A day partly in view counts at either end.
    assert_eq!(
        view_days(SPAN, SCALE, 10.5 * DEFAULT_DAY_W, VIEW_W),
        (110, 120)
    );
    // Clamped to the span: the padding past the finish and the baseline lead-in.
    assert_eq!(
        view_days(SPAN, SCALE, SCALE.width() - VIEW_W, VIEW_W),
        (137, 139)
    );
    assert_eq!(view_days(SPAN, LEAD_SCALE, 0., VIEW_W), (100, 104));
}

#[test]
fn dragging_the_box_scrolls_the_chart_by_the_days_it_covers() {
    let drag = |start_x, dx| drag_gantt_x(SPAN, SCALE, VIEW_W, RULER_W, start_x, dx);
    let mid = 10. * DEFAULT_DAY_W;
    assert_eq!(drag(mid, 50.), 15. * DEFAULT_DAY_W);
    assert_eq!(drag(mid, -30.), 7. * DEFAULT_DAY_W);
    assert_eq!(drag(mid, 0.), mid);
    // The box stops at either end of the Timeline.
    assert_eq!(drag(mid, 1000.), 30. * DEFAULT_DAY_W);
    assert_eq!(drag(mid, -1000.), 0.);
    // No ruler, nothing to drag.
    assert_eq!(drag_gantt_x(SPAN, SCALE, VIEW_W, 0., mid, 50.), mid);
}

#[test]
fn a_chart_wider_than_the_plan_has_nothing_to_drag() {
    // The padded scale of a 4-day plan still lets the chart scroll 10 days,
    // but the box already covers the whole Timeline.
    let span = TimelineSpan {
        first: 100,
        days: 4,
    };
    let scale = GanttScale {
        origin_day: 100,
        days: 30,
        day_w: DEFAULT_DAY_W,
    };
    for dx in [-100., 30., 100.] {
        assert_eq!(
            drag_gantt_x(
                span,
                scale,
                20. * DEFAULT_DAY_W,
                RULER_W,
                5. * DEFAULT_DAY_W,
                dx
            ),
            5. * DEFAULT_DAY_W
        );
    }
}

#[test]
fn a_chart_outside_the_span_moves_towards_it_and_never_snaps() {
    // The chart shows the baseline lead-in, so the box is clamped to the left
    // edge: holding the drag still leaves the chart where it was.
    let lead = |dx| drag_gantt_x(SPAN, LEAD_SCALE, VIEW_W, RULER_W, 0., dx);
    assert_eq!(lead(0.), 0.);
    assert_eq!(lead(-10.), 0.);
    assert_eq!(lead(10.), DEFAULT_DAY_W);
    // Scrolled into the padding past the finish: right is blocked, left moves.
    let end = SCALE.width() - VIEW_W;
    let pad = |dx| drag_gantt_x(SPAN, SCALE, VIEW_W, RULER_W, end, dx);
    assert_eq!(pad(0.), end);
    assert_eq!(pad(1.), end);
    assert_eq!(pad(-10.), end - DEFAULT_DAY_W);
}

// The view: a 60-working-day plan whose last task starts 40 days in, in a
// window whose chart shows only part of it.
fn long_tab() -> DocTab {
    let mut ed = ProjectEditor::new(untitled_project());
    ed.add_task(None, "Build", 40 * 480, false).unwrap();
    ed.add_task(None, "Late", 20 * 480, false).unwrap();
    ed.add_predecessor(2, 1, projcore::LinkType::FinishStart, 0)
        .unwrap();
    let mut t = new_project_tab("Project1");
    t.surface = Surface::Project(ProjectView::new(ed.project().clone(), false));
    vm(&mut t).layout(900.);
    t
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!("project")
    };
    v
}

#[test]
fn dragging_the_box_moves_only_the_chart() {
    let mut t = long_tab();
    let v = vm(&mut t);
    let span = TimelineSpan::of(&v.ed);
    assert!(
        v.gantt_w < span.days as f32 * DEFAULT_DAY_W,
        "the chart shows part of the plan"
    );
    let (f0, f1) = v.timeline_box();
    assert_eq!(f0, 0.);
    assert!(f1 < 1.);
    let px_per_day = ruler_width(v.width) / span.days as f32;
    // The pointer's window x is far from 0: only its distance from the press counts.
    let press = 500.;
    v.press_timeline(press);
    v.drag_timeline(press);
    assert_eq!(v.gantt_x.get(), 0., "held still, the chart stays");
    v.drag_timeline(press + 5. * px_per_day);
    assert!(
        (v.gantt_x.get() - 5. * DEFAULT_DAY_W).abs() < 1e-3,
        "{}",
        v.gantt_x.get()
    );
    assert!(v.timeline_box().0 > 0.);
    // Absolute from the press: a second move event to the same x adds nothing.
    v.drag_timeline(press + 5. * px_per_day);
    assert!((v.gantt_x.get() - 5. * DEFAULT_DAY_W).abs() < 1e-3);
    v.drag_timeline(press + 1e6);
    let (_, f1) = v.timeline_box();
    assert!((f1 - 1.).abs() < 1e-4, "stops at the finish: {f1}");
    // Back to where it was 5 days in: measured from the press, the clamp at
    // the finish is forgotten; stepping from the last move would not be.
    v.drag_timeline(press + 5. * px_per_day);
    assert!(
        (v.gantt_x.get() - 5. * DEFAULT_DAY_W).abs() < 1e-3,
        "{}",
        v.gantt_x.get()
    );
    v.drag_timeline(press + 1e6);
    // A new press starts from where the chart is now.
    let end = v.gantt_x.get();
    v.press_timeline(100.);
    v.drag_timeline(100. - 2. * px_per_day);
    assert!((v.gantt_x.get() - (end - 2. * DEFAULT_DAY_W)).abs() < 1e-3);
    v.drag_timeline(-1e6);
    assert_eq!(v.gantt_x.get(), 0.);
    assert!(!v.ed.dirty());
    assert_eq!(v.ed.undo_depth(), 0);
}

#[test]
fn without_a_held_press_a_drag_moves_nothing() {
    let mut t = long_tab();
    let v = vm(&mut t);
    // A drag reaching a tab that never took the press (the user switched tabs
    // with the button held) has nothing to measure from.
    v.drag_timeline(500.);
    assert_eq!(v.gantt_x.get(), 0.);
    v.press_timeline(100.);
    v.drag_timeline(150.);
    let moved = v.gantt_x.get();
    assert!(moved > 0.);
    v.release_timeline();
    v.drag_timeline(900.);
    assert_eq!(v.gantt_x.get(), moved, "released");
}

#[test]
fn the_box_follows_every_way_the_chart_scrolls() {
    let mut t = long_tab();
    let at_start = vm(&mut t).timeline_box();
    vm(&mut t).pan_gantt(true);
    assert_ne!(vm(&mut t).timeline_box(), at_start, "pan");
    apply_project_act(&mut t, ProjectAct::GoToStart);
    assert_eq!(vm(&mut t).timeline_box(), at_start, "Go to Start");
    vm(&mut t).ed.select(1);
    apply_project_act(&mut t, ProjectAct::ScrollToTask);
    assert!(vm(&mut t).timeline_box().0 > at_start.0, "Scroll to Task");
    apply_project_act(&mut t, ProjectAct::GoToStart);
    let v = vm(&mut t);
    let chart = v.table_w + GANTT_INSET + 10.;
    assert!(v.wheel(chart, -3. * DEFAULT_DAY_W));
    assert_ne!(v.timeline_box(), at_start, "Shift+wheel");
}

/// `uiharness/cases/project-timeline-view.uit` asserts these at the harness
/// window (1180 px): the table takes its 847 px, so the chart shows 15 days.
#[test]
fn the_view_box_of_the_timeline_view_fixture_at_the_default_window() {
    let xml = include_str!("../../../../../uiharness/fixtures/project-timeline-view.xml");
    let project = projcore::mspdi::read_mspdi(xml).unwrap();
    let mut t = new_project_tab("Project1");
    t.surface = Surface::Project(ProjectView::new(project, false));
    vm(&mut t).layout(1180.);
    let view = |t: &mut DocTab| {
        let v = vm(t);
        let (first, last) = view_days(TimelineSpan::of(&v.ed), v.scale, v.gantt_x.get(), v.gantt_w);
        let date = |d: i64| project_date(DateTime::from_minutes(d * 1440));
        (date(first), date(last))
    };
    assert_eq!(vm(&mut t).gantt_w, 311.);
    assert_eq!(view(&mut t), ("Mon 1/5/26".into(), "Mon 1/19/26".into()));
    let last = vm(&mut t).ed.project().tasks.len() - 1;
    vm(&mut t).ed.select(last);
    apply_project_act(&mut t, ProjectAct::ScrollToTask);
    assert_eq!(vm(&mut t).ed.project().tasks[last].name, "Late");
    assert_eq!(view(&mut t), ("Sun 3/1/26".into(), "Sun 3/15/26".into()));
}

const ALL: (f32, f32) = (0., 1000.);

#[test]
fn dragging_the_left_edge_keeps_the_right_date_and_fills_the_chart() {
    // A chart 440 px wide at 22 px/day shows days 10 to 30.
    let (day_w, left) = drag_edge(BoxEdge::Left, 10., 30., 5., 440., ALL);
    // The box now covers days 15 to 30: 15 days fill 440 px.
    assert!((day_w - 440. / 15.).abs() < 1e-3);
    assert!((left - 15.).abs() < 1e-3);
    // Dragging out widens the span.
    let (day_w, left) = drag_edge(BoxEdge::Left, 10., 30., -10., 440., ALL);
    assert!((day_w - 440. / 30.).abs() < 1e-3 && (left - 0.).abs() < 1e-3);
}

#[test]
fn dragging_the_right_edge_keeps_the_left_date() {
    let (day_w, left) = drag_edge(BoxEdge::Right, 10., 30., 20., 440., ALL);
    assert!((day_w - 440. / 40.).abs() < 1e-3);
    assert_eq!(left, 10.);
    let (day_w, left) = drag_edge(BoxEdge::Right, 10., 30., -10., 440., ALL);
    assert!((day_w - 440. / 10.).abs() < 1e-3);
    assert_eq!(left, 10.);
}

#[test]
fn an_edge_cannot_cross_the_other_or_leave_the_timeline() {
    // Past the opposite edge the span stays one clamp-wide, the far edge put.
    let (day_w, left) = drag_edge(BoxEdge::Left, 10., 30., 500., 440., ALL);
    assert!((day_w - zoom::MAX_DAY_W).abs() < 1e-3);
    assert!((left - (30. - 440. / zoom::MAX_DAY_W)).abs() < 1e-3);
    let (day_w, left) = drag_edge(BoxEdge::Right, 10., 30., -500., 440., ALL);
    assert!((day_w - zoom::MAX_DAY_W).abs() < 1e-3);
    assert_eq!(left, 10.);
    // The moved edge stops at the Timeline's ends: the left at day 0, the right
    // at the span's last, so the far edge never has to move to make room.
    let (day_w, left) = drag_edge(BoxEdge::Left, 10., 30., -4000., 440., (0., 60.));
    assert!((left - 0.).abs() < 1e-3);
    assert!((day_w - 440. / 30.).abs() < 1e-3);
    let (day_w, left) = drag_edge(BoxEdge::Right, 10., 30., 4000., 440., (0., 60.));
    assert_eq!(left, 10.);
    assert!((day_w - 440. / 50.).abs() < 1e-3);
}

#[test]
fn a_short_plan_in_a_wide_chart_has_edges_that_follow_the_pointer() {
    // 6 days of plan in a chart that shows 15 at 22 px/day.
    let mut v = ProjectView::new(project_with_days(6), false);
    v.layout(1180.);
    let span = TimelineSpan::of(&v.ed);
    assert!(v.gantt_w / v.day_w() > span.days as f32 + 5.);
    let w = ruler_width(v.width);
    let (f0, f1) = v.timeline_box();
    assert_eq!((f0, f1), (0., 1.), "the box fills the ruler");
    // Press the drawn right edge and drag it back across a quarter of the ruler.
    v.press_timeline_edge(BoxEdge::Right, w);
    v.drag_timeline_edge(w * 0.75);
    let (g0, g1) = v.timeline_box();
    assert!(
        (g1 - 0.75).abs() < 0.02,
        "the drawn edge is at the pointer: {g1}"
    );
    assert!(g0.abs() < 1e-3, "the left edge did not move");
    assert!((v.visible_days() - 0.75 * span.days as f32).abs() < 0.2);
    // Dragging the right edge past the end of the ruler stays at the end.
    v.drag_timeline_edge(w * 3.);
    let (h0, h1) = v.timeline_box();
    assert!(h0.abs() < 1e-3 && (h1 - 1.).abs() < 1e-3);
}

#[test]
fn an_overshoot_leaves_the_other_edge_fixed() {
    let mut v = ProjectView::new(project_with_days(60), false);
    v.layout(1180.);
    let w = ruler_width(v.width);
    let (f0, f1) = v.timeline_box();
    let (left, box_w) = box_px(f0, f1, w);
    // Drag the left edge far past the left end of the ruler.
    v.press_timeline_edge(BoxEdge::Left, left);
    let right_before = f1;
    v.drag_timeline_edge(left - w * 5.);
    let (g0, g1) = v.timeline_box();
    assert!(g0.abs() < 1e-3, "the edge stops at the first day");
    assert!((g1 - right_before).abs() < 0.01, "{right_before} -> {g1}");
    // And the right edge far past the right end.
    v.release_timeline();
    let (f0, f1) = v.timeline_box();
    let (left, box_w2) = box_px(f0, f1, w);
    let _ = box_w;
    v.press_timeline_edge(BoxEdge::Right, left + box_w2);
    v.drag_timeline_edge(left + box_w2 + w * 5.);
    let (h0, h1) = v.timeline_box();
    assert!((h0 - f0).abs() < 0.01, "{f0} -> {h0}");
    assert!((h1 - 1.).abs() < 0.01);
}

#[test]
fn the_edge_strips_leave_the_middle_of_a_narrow_box_to_pan() {
    assert_eq!(edge_strip_w(100.), BOX_EDGE_W);
    assert_eq!(edge_strip_w(MIN_BOX_W), MIN_BOX_W / 3.);
    for w in [MIN_BOX_W, 9., 18., 40.] {
        assert!(w - 2. * edge_strip_w(w) >= w / 3. - 1e-3, "{w}");
    }
}

#[test]
fn an_edge_drag_zooms_the_chart_and_the_box_mirrors_the_visible_span() {
    let mut v = ProjectView::new(project_with_days(60), false);
    v.layout(1180.);
    let span = TimelineSpan::of(&v.ed);
    let w = ruler_width(v.width);
    let width_before = v.gantt_w;
    let (f0, f1) = v.timeline_box();
    let (left, box_w) = box_px(f0, f1, w);
    // Press the right edge and drag it a quarter of the ruler right.
    v.press_timeline_edge(BoxEdge::Right, left + box_w);
    v.drag_timeline_edge(left + box_w + w / 4.);
    assert!(v.day_w() < DEFAULT_DAY_W, "a wider span is a narrower day");
    assert_eq!(
        v.gantt_w, width_before,
        "the chart's width is not the zoom's"
    );
    // The box's right edge followed the pointer.
    let (g0, g1) = v.timeline_box();
    assert!(
        (g1 - (f1 + 0.25)).abs() < 0.03 || g1 >= 0.99,
        "{f1} -> {g1}"
    );
    assert!((g0 - f0).abs() < 0.03);
    // The span the chart shows is what the box says.
    let (first, last) = view_days(span, v.scale, v.gantt_x.get(), v.gantt_w);
    assert!(last > first);
    // Without a press a drag moves nothing; a release ends it.
    v.release_timeline();
    let held = v.day_w();
    v.drag_timeline_edge(0.);
    assert_eq!(v.day_w(), held);
}

#[test]
fn the_middle_of_the_box_still_pans_without_zooming() {
    let mut v = ProjectView::new(project_with_days(60), false);
    v.layout(1180.);
    v.zoom_step(-1, 0.);
    let day_w = v.day_w();
    v.press_timeline(100.);
    v.drag_timeline(140.);
    assert_eq!(v.day_w(), day_w);
    assert!(v.timeline_edge.is_none());
}

fn project_with_days(days: i64) -> Project {
    let mut p = untitled_project();
    p.tasks = vec![Task {
        uid: 1,
        id: 1,
        name: "Task".into(),
        duration_min: days * 480,
        outline_level: 1,
        ..Task::default()
    }];
    p
}

#[test]
fn the_pane_height_clamps_between_one_band_and_half_the_window() {
    assert_eq!(MIN_TIMELINE_H, BAND_H + 2. * PAD_V);
    const { assert!(MIN_TIMELINE_H <= TIMELINE_H) };
    const { assert!(BAND_H < TIMELINE_H) };
    assert_eq!(clamp_timeline_h(10., 800.), MIN_TIMELINE_H);
    assert_eq!(clamp_timeline_h(500., 800.), 400.);
    assert_eq!(clamp_timeline_h(120., 800.), 120.);
    // A window too short for the minimum still gets the minimum.
    assert_eq!(clamp_timeline_h(120., 40.), MIN_TIMELINE_H);
}

#[test]
fn the_grip_drags_the_height_and_a_double_click_restores_it() {
    let mut v = ProjectView::new(project_with_days(10), false);
    v.height = 800.;
    assert_eq!(v.timeline_height(), TIMELINE_H);
    // A drag is measured from the press, so it does not accumulate.
    v.press_grip(100.);
    v.drag_grip(140.);
    assert_eq!(v.timeline_height(), TIMELINE_H + 40.);
    v.drag_grip(150.);
    assert_eq!(v.timeline_height(), TIMELINE_H + 50.);
    v.drag_grip(-1000.);
    assert_eq!(v.timeline_height(), MIN_TIMELINE_H);
    v.drag_grip(5000.);
    assert_eq!(v.timeline_height(), 400.);
    v.release_timeline();
    // Without a press a drag moves nothing.
    v.drag_grip(120.);
    assert_eq!(v.timeline_height(), 400.);
    v.reset_timeline_h();
    assert_eq!(v.timeline_height(), TIMELINE_H);
    // The window shrinking pulls a tall pane back to its half.
    v.timeline_h = Some(500.);
    v.height = 600.;
    assert_eq!(v.timeline_height(), 300.);
}
