use super::*;
use core::prelude::v1::test;

fn day(y: i64, m: u32, d: u32) -> DateTime {
    DateTime::from_ymd_hm(y, m, d, 8, 0)
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
};
/// A baseline that starts 5 days before the displayed plan widens the scale.
const LEAD_SCALE: GanttScale = GanttScale {
    origin_day: 95,
    days: 52,
};
const VIEW_W: f32 = 10. * DAY_W;
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
    assert_eq!(view_box(SPAN, SCALE, 0., 50. * DAY_W), (0., 1.));
    // Scrolled mid-plan: days 10..20 of 40.
    assert_near(view_box(SPAN, SCALE, 10. * DAY_W, VIEW_W), (0.25, 0.5));
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
    assert_eq!(view_days(SPAN, SCALE, 10. * DAY_W, VIEW_W), (110, 119));
    // A day partly in view counts at either end.
    assert_eq!(view_days(SPAN, SCALE, 10.5 * DAY_W, VIEW_W), (110, 120));
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
    let mid = 10. * DAY_W;
    assert_eq!(drag(mid, 50.), 15. * DAY_W);
    assert_eq!(drag(mid, -30.), 7. * DAY_W);
    assert_eq!(drag(mid, 0.), mid);
    // The box stops at either end of the Timeline.
    assert_eq!(drag(mid, 1000.), 30. * DAY_W);
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
    };
    for dx in [-100., 30., 100.] {
        assert_eq!(
            drag_gantt_x(span, scale, 20. * DAY_W, RULER_W, 5. * DAY_W, dx),
            5. * DAY_W
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
    assert_eq!(lead(10.), DAY_W);
    // Scrolled into the padding past the finish: right is blocked, left moves.
    let end = SCALE.width() - VIEW_W;
    let pad = |dx| drag_gantt_x(SPAN, SCALE, VIEW_W, RULER_W, end, dx);
    assert_eq!(pad(0.), end);
    assert_eq!(pad(1.), end);
    assert_eq!(pad(-10.), end - DAY_W);
}

// The view: a 60-working-day plan whose last task starts 40 days in, in a
// window whose chart shows only part of it.
fn long_tab() -> DocTab {
    let mut ed = ProjectEditor::new(untitled_project());
    ed.add_task(None, "Build", 40 * 480).unwrap();
    ed.add_task(None, "Late", 20 * 480).unwrap();
    ed.add_predecessor(2, 1, projcore::LinkType::FinishStart, 0)
        .unwrap();
    let mut t = new_project_tab();
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
        v.gantt_w < span.days as f32 * DAY_W,
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
        (v.gantt_x.get() - 5. * DAY_W).abs() < 1e-3,
        "{}",
        v.gantt_x.get()
    );
    assert!(v.timeline_box().0 > 0.);
    // Absolute from the press: a second move event to the same x adds nothing.
    v.drag_timeline(press + 5. * px_per_day);
    assert!((v.gantt_x.get() - 5. * DAY_W).abs() < 1e-3);
    v.drag_timeline(press + 1e6);
    let (_, f1) = v.timeline_box();
    assert!((f1 - 1.).abs() < 1e-4, "stops at the finish: {f1}");
    // A new press starts from where the chart is now.
    let end = v.gantt_x.get();
    v.press_timeline(100.);
    v.drag_timeline(100. - 2. * px_per_day);
    assert!((v.gantt_x.get() - (end - 2. * DAY_W)).abs() < 1e-3);
    v.drag_timeline(-1e6);
    assert_eq!(v.gantt_x.get(), 0.);
    assert!(!v.ed.dirty());
    assert_eq!(v.ed.undo_depth(), 0);
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
    assert!(v.wheel(chart, -3. * DAY_W));
    assert_ne!(v.timeline_box(), at_start, "Shift+wheel");
}
