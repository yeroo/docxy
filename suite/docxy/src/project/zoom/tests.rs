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
    // The diamond is centred in its day at any zoom.
    assert_eq!(bar_rect(&milestone, 6.).0, 10. * 6. + 3. - 7.);
    assert_eq!(bar_rect(&milestone, 22.).0, 10. * 22. + 11. - 7.);
    assert_eq!(bar_rect(&milestone, 22.).0, 224.);
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
    v.zoom_selected().unwrap();
    let days = (bar.end - bar.start + 1) as f32;
    assert!((v.day_w() * days - v.gantt_w).abs() < 1.0 || v.day_w() == MAX_DAY_W);
    assert!((v.gantt_x.get() / v.day_w() - bar.start as f32).abs() < 0.6);
    // No selected task: nothing changes and the status says why.
    let mut empty = view(vec![]);
    let before = empty.day_w();
    assert_eq!(
        empty.zoom_selected(),
        Err("Select one or more tasks to zoom to".into())
    );
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
    assert_eq!(LEVELS[nearest(v.day_w())].name, "Weeks");
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

const MOVED: TouchPhase = TouchPhase::Moved;

fn stepped(w: WheelZoom) -> bool {
    matches!(w, WheelZoom::Stepped(_))
}

#[test]
fn ctrl_wheel_zooms_around_the_pointer_over_the_chart_only() {
    let mut v = view(vec![task(1, 400)]);
    v.gantt_x.set(2000.);
    let chart_left = v.table_w + GANTT_INSET;
    // Over the table: not the chart's wheel.
    assert_eq!(v.wheel_zoom(10., 5., true, MOVED), WheelZoom::NotChart);
    assert_eq!(v.day_w(), DEFAULT_DAY_W);
    // Wheel up zooms in, down out; the day under the pointer stays.
    let pointer = 150.;
    let before = (v.gantt_x.get() + pointer) / v.day_w();
    assert_eq!(
        v.wheel_zoom(chart_left + pointer, -5., true, MOVED),
        WheelZoom::Stepped("Timescale: Weeks".into())
    );
    assert_eq!(v.day_w(), 12.);
    assert!(((v.gantt_x.get() + pointer) / v.day_w() - before).abs() < 1e-3);
    assert_eq!(
        v.wheel_zoom(chart_left + pointer, 0., true, MOVED),
        WheelZoom::NotChart
    );
    assert!(stepped(v.wheel_zoom(chart_left + pointer, 5., true, MOVED)));
    assert_eq!(v.day_w(), DEFAULT_DAY_W);
}

#[test]
fn the_state_reports_the_zoom() {
    let mut v = view(vec![task(1, 30)]);
    let find = |v: &ProjectView, key: &str| {
        project_state(v, None)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, j)| j)
    };
    use ctlcore::json::Json;
    assert_eq!(find(&v, "gantt_day_w"), Some(Json::Num(22.)));
    assert_eq!(find(&v, "gantt_zoom"), Some(Json::Str("Days".into())));
    v.set_zoom(14., 0., 0.);
    assert_eq!(find(&v, "gantt_day_w"), Some(Json::Num(14.)));
    assert_eq!(
        find(&v, "gantt_zoom"),
        Some(Json::Str("Custom (Weeks)".into()))
    );
    assert_eq!(
        find(&v, "timeline_h"),
        Some(Json::Num(f64::from(timeline::TIMELINE_H)))
    );
}

#[test]
fn the_zoom_dialog_applies_a_level_a_fit_or_a_custom_span() {
    use crate::dialog::Value;
    let mut v = view(vec![task(1, 60), task(2, 5)]);
    let mut d = zoom_dialogs::zoom_dialog(&v);
    // It opens on the level in force; Custom is off.
    assert_eq!(crate::page_setup::chosen(&d, "zoom"), Some(6));
    assert!(
        !d.controls
            .iter()
            .find(|c| c.name == "custom")
            .unwrap()
            .enabled
    );
    let set = |d: &mut crate::dialog::Dialog, name: &str, value: Value| {
        d.controls
            .iter_mut()
            .find(|c| c.name == name)
            .unwrap()
            .value = value;
    };
    set(&mut d, "zoom", Value::Choice(Some(3)));
    zoom_dialogs::apply_zoom(&mut v, &d).unwrap();
    assert_eq!(v.day_w(), 3.);
    set(&mut d, "zoom", Value::Choice(Some(7)));
    zoom_dialogs::apply_zoom(&mut v, &d).unwrap();
    let span = timeline::TimelineSpan::of(&v.ed);
    assert!((v.day_w() * span.days as f32 - v.gantt_w).abs() < 0.5 || v.day_w() == MAX_DAY_W);
    // Custom: this many days across the chart.
    set(&mut d, "zoom", Value::Choice(Some(9)));
    set(&mut d, "custom", Value::Text("40".into()));
    zoom_dialogs::apply_zoom(&mut v, &d).unwrap();
    assert!((v.visible_days() - 40.).abs() < 0.5);
    // A refused Custom changes nothing.
    let held = v.day_w();
    set(&mut d, "custom", Value::Text("0".into()));
    assert!(zoom_dialogs::apply_zoom(&mut v, &d).is_err());
    set(&mut d, "custom", Value::Text("x".into()));
    assert!(zoom_dialogs::apply_zoom(&mut v, &d).is_err());
    assert_eq!(v.day_w(), held);
    // The dialog opened on a free width starts on Custom, with the span shown.
    let d = zoom_dialogs::zoom_dialog(&v);
    assert_eq!(crate::page_setup::chosen(&d, "zoom"), Some(9));
    assert!(
        d.controls
            .iter()
            .find(|c| c.name == "custom")
            .unwrap()
            .enabled
    );
}

#[test]
fn the_timescale_dialog_picks_a_level_and_shows_its_tiers() {
    use crate::dialog::Value;
    let mut v = view(vec![task(1, 60)]);
    let mut d = timescale_dialog_for(&v);
    let text = |d: &crate::dialog::Dialog, name: &str| crate::page_setup::text_of(d, name);
    assert_eq!(
        (text(&d, "major").as_str(), text(&d, "minor").as_str()),
        ("Weeks", "Days")
    );
    assert_eq!(crate::page_setup::chosen(&d, "level"), Some(6));
    // Choosing a level moves the read-only tier rows with it.
    let i = d.controls.iter().position(|c| c.name == "level").unwrap();
    d.controls[i].value = Value::Choice(Some(3));
    (d.react.unwrap().0)(&mut d, i, &Value::Choice(Some(6)));
    assert_eq!(
        (text(&d, "major").as_str(), text(&d, "minor").as_str()),
        ("Quarters", "Months")
    );
    assert_eq!(text(&d, "minor-label"), "Mar 2026");
    zoom_dialogs::apply_timescale(&mut v, &d).unwrap();
    assert_eq!(v.day_w(), 3.);
    for name in ["major", "major-label", "minor", "minor-label", "count"] {
        assert!(
            !d.controls.iter().find(|c| c.name == name).unwrap().enabled,
            "{name}"
        );
    }
}

fn timescale_dialog_for(v: &ProjectView) -> crate::dialog::Dialog {
    zoom_dialogs::timescale_dialog(v)
}

#[test]
fn the_chart_menu_opens_the_timescale_and_zoom_dialogs() {
    let v = view(vec![task(1, 5)]);
    let items = project_chart_menu(&v);
    let find = |label: &str| {
        items.iter().find_map(|i| match i {
            crate::menu::MenuItem::Item(e) if e.label == label => Some(e),
            _ => None,
        })
    };
    let acts = ["Timescale...", "Zoom..."].map(|l| find(l).unwrap_or_else(|| panic!("{l}")));
    assert!(acts.iter().all(|e| e.enabled));
    assert!(matches!(
        acts[0].act,
        Some(Act::Project(ProjectAct::TimescaleDialog))
    ));
    assert!(matches!(
        acts[1].act,
        Some(Act::Project(ProjectAct::ZoomDialog))
    ));
    // Project's order: the two dialogs lead; the rest is still not built.
    assert!(!find("Change Working Time...").unwrap().enabled);
    assert!(!find("Show/Hide Bar Styles").unwrap().enabled);
    assert!(!find("Gridlines...").unwrap().enabled);
}

#[test]
fn smooth_wheel_deltas_add_up_to_one_step_per_threshold() {
    let mut v = view(vec![task(1, 400)]);
    let x = v.table_w + GANTT_INSET + 100.;
    // Many small same-direction deltas are taken by the chart, without a step,
    // until the sum crosses the threshold.
    for _ in 0..7 {
        assert_eq!(v.wheel_zoom(x, -5., false, MOVED), WheelZoom::Absorbed);
    }
    assert_eq!(v.day_w(), DEFAULT_DAY_W);
    assert!(stepped(v.wheel_zoom(x, -5., false, MOVED)));
    assert_eq!(v.day_w(), 12., "one level for the first 40 px");
    // The sum starts afresh: another 40 px is one more step, not a swipe's worth.
    for _ in 0..7 {
        v.wheel_zoom(x, -5., false, MOVED);
    }
    assert_eq!(v.day_w(), 12.);
    v.wheel_zoom(x, -5., false, MOVED);
    assert_eq!(v.day_w(), 6.);
    // A change of direction drops what was summed.
    v.wheel_zoom(x, -30., false, MOVED);
    assert_eq!(v.wheel_zoom(x, 30., false, MOVED), WheelZoom::Absorbed);
    assert_eq!(v.day_w(), 6.);
    assert_eq!(v.wheel_zoom(x, 5., false, MOVED), WheelZoom::Absorbed);
    assert!(stepped(v.wheel_zoom(x, 30., false, MOVED)));
    assert_eq!(v.day_w(), 12.);
    // One whole-line notch is one step whatever its size, and clears the sum.
    v.wheel_zoom(x, 20., false, MOVED);
    assert!(stepped(v.wheel_zoom(x, 1., true, MOVED)));
    assert_eq!(v.day_w(), 22.);
    assert_eq!(v.wheel_acc, 0.);
    // A large pixel delta (the harness's) is one step, not several.
    assert!(stepped(v.wheel_zoom(x, -120., false, MOVED)));
    assert_eq!(v.day_w(), 12.);
}

#[test]
fn fractional_lines_accumulate_and_a_touch_phase_clears_the_sum() {
    let mut v = view(vec![task(1, 400)]);
    let x = v.table_w + GANTT_INSET + 100.;
    // A hi-res wheel's tenth of a line (2 px at a 20 px line) is not a notch.
    let tenth = 2.;
    for _ in 0..19 {
        assert_eq!(v.wheel_zoom(x, -tenth, false, MOVED), WheelZoom::Absorbed);
    }
    assert_eq!(v.day_w(), DEFAULT_DAY_W);
    assert!(stepped(v.wheel_zoom(x, -tenth, false, MOVED)));
    assert_eq!(v.day_w(), 12.);
    // A new touch clears what a lifted one left; so does the end of a swipe.
    v.wheel_zoom(x, -30., false, MOVED);
    assert_eq!(
        v.wheel_zoom(x, -30., false, TouchPhase::Started),
        WheelZoom::Absorbed
    );
    assert_eq!(v.wheel_acc, -30.);
    // The end of a swipe is a zero delta: it still clears the sum.
    v.wheel_zoom(x, 0., false, TouchPhase::Ended);
    assert_eq!(v.wheel_acc, 0.);
    assert_eq!(v.day_w(), 12.);
}

#[test]
fn only_the_listed_timescales_can_be_chosen() {
    assert!((0..7).all(available));
    assert!(!available(7) && !available(8) && !available(9));
}

#[test]
fn only_a_whole_line_is_a_notch() {
    let lines = |y: f32| ScrollDelta::Lines(point(0., y));
    assert!(!is_notch(&lines(0.25)));
    assert!(!is_notch(&lines(-0.99)));
    assert!(is_notch(&lines(1.)));
    assert!(is_notch(&lines(-3.)));
    assert!(!is_notch(&ScrollDelta::Pixels(point(px(0.), px(120.)))));
}
