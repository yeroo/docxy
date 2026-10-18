use super::*;
use core::prelude::v1::test;

fn day(iso: &str) -> i64 {
    DateTime::parse_mspdi(&format!("{iso}T08:00:00"))
        .unwrap()
        .day_number()
}

fn scale_at(origin: &str, days: i64, day_w: f32) -> GanttScale {
    GanttScale {
        origin_day: day(origin),
        days,
        day_w,
    }
}

fn task(uid: i32, days: i64) -> Task {
    Task {
        uid,
        id: uid,
        name: format!("Task {uid}"),
        duration_min: days * 480,
        outline_level: 1,
        ..Task::default()
    }
}

fn view(tasks: Vec<Task>) -> ProjectView {
    let mut p = untitled_project();
    p.tasks = tasks;
    let mut v = ProjectView::new(p, false);
    v.layout(1180.);
    v
}

#[test]
fn levels_widen_in_order_and_stay_inside_the_clamp() {
    assert!(LEVELS.windows(2).all(|w| w[0].day_w < w[1].day_w));
    assert!(
        LEVELS
            .iter()
            .all(|l| (MIN_DAY_W..=MAX_DAY_W).contains(&l.day_w))
    );
    assert_eq!(LEVELS[LEVELS.len() - 1].day_w, DEFAULT_DAY_W);
    assert_eq!(LEVELS[LEVELS.len() - 1].name, "Days");
    // The combo lists every level, then Hours and Minutes, which cannot be chosen.
    assert!(COMBO.iter().take(7).all(|(_, on)| *on));
    assert_eq!(COMBO[7..], [("Hours", false), ("Minutes", false)]);
    assert!(
        LEVELS
            .iter()
            .zip(COMBO)
            .all(|(l, (name, _))| l.name == name)
    );
}

#[test]
fn nearest_and_label_name_the_level_and_mark_a_custom_width() {
    assert_eq!(nearest(22.), 6);
    assert_eq!(nearest(0.25), 0);
    assert_eq!(nearest(90.), 6);
    assert_eq!(nearest(4.), 3);
    assert_eq!(nearest(5.), 4);
    assert_eq!(label(12.), "Weeks");
    assert_eq!(label(14.), "Custom (Weeks)");
    assert!(is_preset(3.) && !is_preset(3.5));
}

#[test]
fn step_moves_to_the_next_level_and_stops_at_the_ends() {
    assert_eq!(step(22., -1), Some(12.));
    assert_eq!(step(12., 1), Some(22.));
    assert_eq!(step(22., 1), None);
    assert_eq!(step(0.25, -1), None);
    // From a free width the next level beyond it, not the nearest.
    assert_eq!(step(14., 1), Some(22.));
    assert_eq!(step(14., -1), Some(12.));
    assert_eq!(step(40., -1), Some(22.));
}

#[test]
fn fit_fills_the_width_and_clamps() {
    assert_eq!(fit(50., 1100.), 22.);
    assert_eq!(fit(0., 100.), 96.);
    assert_eq!(fit(1_000_000., 100.), MIN_DAY_W);
    assert_eq!(fit(1., 1000.), MAX_DAY_W);
}

#[test]
fn zoom_about_keeps_the_day_under_the_pointer() {
    let (x, p, old, new) = (440., 300., 22., 12.);
    let after = zoom_about(old, x, p, new);
    assert!(((x + p) / old - (after + p) / new).abs() < 1e-4);
    // Pointer at the left edge: the left-edge date stays.
    assert!((zoom_about(22., 220., 0., 12.) - 120.).abs() < 1e-4);
}

#[test]
fn tier_boundaries_follow_the_calendar() {
    assert!(Unit::Month.starts(day("2026-03-01")) && !Unit::Month.starts(day("2026-03-02")));
    assert!(Unit::Week.starts(day("2026-03-02")) && !Unit::Week.starts(day("2026-03-03")));
    assert!(Unit::Quarter.starts(day("2026-04-01")) && !Unit::Quarter.starts(day("2026-05-01")));
    assert!(Unit::HalfYear.starts(day("2026-07-01")) && !Unit::HalfYear.starts(day("2026-04-01")));
    assert!(Unit::Year.starts(day("2027-01-01")) && !Unit::Year.starts(day("2027-02-01")));
    assert!(Unit::Third.starts(day("2026-03-11")) && Unit::Third.starts(day("2026-03-21")));
    assert!(!Unit::Third.starts(day("2026-03-12")));
    assert_eq!(
        Unit::Month.start_at_or_before(day("2026-03-17")),
        day("2026-03-01")
    );
    assert_eq!(Unit::Month.next_start(day("2026-03-17")), day("2026-04-01"));
    assert_eq!(Unit::Year.next_start(day("2026-12-31")), day("2027-01-01"));
    assert_eq!(
        Unit::Day.start_at_or_before(day("2026-03-17")),
        day("2026-03-17")
    );
}

#[test]
fn a_tier_covers_the_view_edge_to_edge_with_labels_that_fit() {
    let scale = scale_at("2026-03-02", 200, 12.);
    let level = &LEVELS[nearest(12.)];
    assert_eq!((level.major, level.minor), (Unit::Month, Unit::Week));
    let (major, minor) = tiers(scale, level, 0., 500.);
    // March 2026 is 31 days, entered from its Monday the 2nd.
    assert_eq!(major[0].label, "Mar 2026");
    assert_eq!(
        major[0].x,
        -scale.x(day("2026-03-02")) + (day("2026-03-01") - scale.origin_day) as f32 * 12.
    );
    assert!(
        major
            .windows(2)
            .all(|w| (w[0].x + w[0].w - w[1].x).abs() < 1e-3)
    );
    assert!(
        minor
            .windows(2)
            .all(|w| (w[0].x + w[0].w - w[1].x).abs() < 1e-3)
    );
    assert_eq!(minor[0].x, 0.);
    assert_eq!(minor[0].w, 7. * 12.);
    assert_eq!(minor[0].label, "Mar 2, 2026");
    assert!(
        minor[0].x + minor[0].w >= 0. && minor.last().unwrap().x + minor.last().unwrap().w >= 500.
    );
    // Every label fits its cell, or is empty.
    for c in major.iter().chain(&minor) {
        assert!(c.label.is_empty() || c.label.chars().count() as f32 * CHAR_W + CELL_PAD <= c.w);
    }
}

#[test]
fn a_cell_cut_by_the_left_edge_keeps_its_label_in_view() {
    let scale = scale_at("2026-01-01", 800, 3.);
    let level = &LEVELS[nearest(3.)];
    // 20 days in: January began 20 days (60 px) before the view's left edge.
    let (_, minor) = tiers(scale, level, 60., 400.);
    assert_eq!(minor[0].x, 0.);
    assert_eq!(minor[0].at, 60.);
    assert!(!minor[0].label.is_empty());
}

#[test]
fn narrow_cells_drop_to_short_labels_then_none() {
    let scale = scale_at("2026-03-02", 60, 22.);
    let (_, days) = tiers(scale, &LEVELS[6], 0., 300.);
    // 22 px holds one character: the weekday's initial.
    assert_eq!(days[0].label, "M");
    let scale = scale_at("2026-01-01", 4000, 0.25);
    let (_, quarters) = tiers(scale, &LEVELS[0], 0., 1000.);
    assert!(
        quarters
            .iter()
            .all(|c| c.label.is_empty() || c.label.starts_with('Q'))
    );
}

#[test]
fn geometry_follows_the_scales_own_width() {
    let scale = scale_at("2026-03-02", 100, 6.);
    assert_eq!(scale.width(), 600.);
    assert_eq!(scale.x(scale.origin_day + 10), 60.);
    assert_eq!(scale.visible(60., 30.), 10..15);
    let bar = GanttBar {
        kind: BarKind::OnTrack,
        inactive: false,
        start: 10,
        end: 14,
        baseline: None,
        delay: None,
        rollup: None,
        warning: false,
    };
    assert_eq!(bar_rect(&bar, 6.), (60., 5., 30., 14.));
    assert_eq!(bar_rect(&bar, 22.), (220., 5., 110., 14.));
    let milestone = GanttBar {
        kind: BarKind::Milestone,
        ..bar
    };
    assert_eq!(bar_rect(&milestone, 6.).0, 64.);
}

#[test]
fn day_lines_and_shading_thin_out_when_days_get_narrow() {
    let wide = scale_at("2026-03-02", 100, 22.);
    assert_eq!(day_lines(wide, 0., 100.).len(), 5);
    let mid = scale_at("2026-03-02", 100, 6.);
    assert!(day_lines(mid, 0., 100.).is_empty());
    let p = untitled_project();
    let cal = p.project_shading_calendar();
    // Saturday and Sunday are one run, not two rectangles, where days are wide enough.
    let runs = shaded_runs(wide, cal.as_ref(), 0., 14. * 22.);
    assert!(runs.iter().all(|(a, b)| b >= a));
    assert!(runs.iter().any(|(a, b)| b - a == 1) || cal.is_none());
    let narrow = scale_at("2026-03-02", 100, 2.);
    assert!(shaded_runs(narrow, cal.as_ref(), 0., 200.).is_empty());
}

#[test]
fn zoom_in_and_out_keep_the_left_edge_date() {
    let mut v = view(vec![task(1, 90)]);
    v.gantt_x.set(10. * 22.);
    let left = v.gantt_x.get() / v.day_w();
    let status = v.zoom_step(-1, 0.);
    assert_eq!(v.day_w(), 12.);
    assert_eq!(status, "Timescale: Weeks");
    assert!((v.gantt_x.get() / v.day_w() - left).abs() < 1e-3);
    v.zoom_step(1, 0.);
    assert_eq!(v.day_w(), 22.);
    assert_eq!(v.zoom_day_w, None);
    assert!((v.gantt_x.get() / v.day_w() - left).abs() < 1e-3);
    assert_eq!(v.zoom_step(1, 0.), "Already at the narrowest timescale");
    assert_eq!(v.day_w(), 22.);
}

#[test]
fn a_pointer_zoom_keeps_the_day_under_the_pointer_when_the_range_allows() {
    let mut v = view(vec![task(1, 400)]);
    v.gantt_x.set(2000.);
    let at = |v: &ProjectView, p: f32| (v.gantt_x.get() + p) / v.day_w();
    let before = at(&v, 150.);
    v.zoom_step(-1, 150.);
    assert!((at(&v, 150.) - before).abs() < 1e-3);
    // Zoomed out far enough the plan is narrower than the chart: the offset clamps to 0.
    for _ in 0..6 {
        v.zoom_step(-1, 150.);
    }
    assert_eq!(v.day_w(), 0.25);
    assert!(v.scale.width() < v.gantt_w);
    assert_eq!(v.gantt_x.get(), 0.);
}

#[test]
fn entire_project_fills_the_chart_with_the_plan() {
    let mut v = view(vec![task(1, 60)]);
    let span = timeline::TimelineSpan::of(&v.ed);
    let status = v.zoom_entire();
    assert!((v.day_w() * span.days as f32 - v.gantt_w).abs() < 0.5 || v.day_w() == MAX_DAY_W);
    let (first, last) = timeline::view_days(span, v.scale, v.gantt_x.get(), v.gantt_w);
    assert_eq!(first, span.first);
    assert!(last >= span.first + span.days - 2);
    assert!(status.starts_with("Timescale"));
    // The chart's own width does not depend on the zoom.
    let w = v.gantt_w;
    v.zoom_step(1, 0.);
    assert_eq!(v.gantt_w, w);
}

#[test]
fn selected_tasks_fill_the_chart_and_need_a_selection() {
    let mut v = view(vec![task(1, 5), task(2, 30)]);
    v.select_row(1);
    let bar = gantt_bar(&v.ed, v.ed.project().task(2).unwrap(), v.scale).unwrap();
    v.zoom_selected();
    let days = (bar.end - bar.start + 1) as f32;
    assert!((v.day_w() * days - v.gantt_w).abs() < 1.0 || v.day_w() == MAX_DAY_W);
    assert!((v.gantt_x.get() / v.day_w() - bar.start as f32).abs() < 0.6);
    // No selected task: nothing changes and the status says why.
    let mut empty = view(vec![]);
    let before = empty.day_w();
    assert_eq!(empty.zoom_selected(), "Select one or more tasks to zoom to");
    assert_eq!(empty.day_w(), before);
}

#[test]
fn the_combo_choice_keeps_the_left_edge_and_a_free_width_is_custom() {
    let mut v = view(vec![task(1, 90)]);
    v.gantt_x.set(220.);
    assert_eq!(v.zoom_to_level(5), "Timescale: Weeks");
    assert_eq!(v.day_w(), 12.);
    assert_eq!(v.gantt_x.get(), 120.);
    v.set_zoom(14., 10., 0.);
    assert_eq!(label(v.day_w()), "Custom (Weeks)");
    assert_eq!(v.zoom_level().name, "Weeks");
    assert!((v.visible_days() - v.gantt_w / 14.).abs() < 1e-3);
}

#[test]
fn set_zoom_clamps_and_refreshes_the_scale() {
    let mut v = view(vec![task(1, 90)]);
    v.set_zoom(1000., 0., 0.);
    assert_eq!(v.day_w(), MAX_DAY_W);
    assert_eq!(v.scale.day_w, MAX_DAY_W);
    v.set_zoom(0.001, 0., 0.);
    assert_eq!(v.day_w(), MIN_DAY_W);
    v.set_zoom(DEFAULT_DAY_W, 0., 0.);
    assert_eq!(v.zoom_day_w, None);
}
