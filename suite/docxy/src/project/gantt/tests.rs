use super::*;
use core::prelude::v1::test;

/// A window narrow enough that the default columns need the table scrollbar
/// (the table pane is `NARROW * DEFAULT_TABLE_SHARE` = 750 px).
const NARROW: f32 = 1000.;

fn task(uid: i32, days: i64, level: u32) -> Task {
    Task {
        uid,
        id: uid,
        name: format!("Task {uid}"),
        duration_min: days * 480,
        outline_level: level,
        ..Task::default()
    }
}

fn editor(tasks: Vec<Task>) -> ProjectEditor {
    let mut p = untitled_project();
    p.tasks = tasks;
    ProjectEditor::new(p)
}

fn bar(ed: &ProjectEditor, uid: i32) -> GanttBar {
    gantt_bar(ed, ed.project().task(uid).unwrap(), gantt_scale(ed)).unwrap()
}

#[test]
fn inactive_bars_keep_their_kind_but_never_critical() {
    let mut summary = task(1, 0, 1);
    summary.active = Some(false);
    let mut leaf = task(2, 4, 2);
    leaf.active = Some(true);
    let mut milestone = task(3, 0, 2);
    milestone.active = Some(false);
    let ed = editor(vec![summary, leaf, milestone]);
    for uid in [1, 2, 3] {
        let b = bar(&ed, uid);
        assert!(b.inactive);
        assert_ne!(b.kind, BarKind::Critical);
        assert!(b.state().ends_with(" inactive"));
    }
    assert_eq!(bar(&ed, 1).kind, BarKind::Summary);
    assert_eq!(bar(&ed, 3).kind, BarKind::Milestone);
    assert_eq!(bar_kind_color(BarKind::Summary), GANTT_SUMMARY);
    assert_eq!(bar_kind_color(BarKind::Milestone), GANTT_MILESTONE);
}

#[test]
fn critical_parallel_summary_milestone_and_missing_results() {
    let mut ed = editor(vec![
        task(1, 0, 1),
        task(2, 1, 2),
        task(3, 4, 2),
        task(4, 0, 2),
    ]);
    ed.add_predecessor(4, 3, LinkType::FinishStart, 0).unwrap();
    assert_eq!(bar(&ed, 2).kind, BarKind::OnTrack);
    assert_eq!(bar(&ed, 3).kind, BarKind::Critical);
    let summary = bar(&ed, 1);
    assert_eq!(summary.kind, BarKind::Summary);
    assert_eq!(summary.start, bar(&ed, 2).start.min(bar(&ed, 3).start));
    assert_eq!(summary.end, bar(&ed, 3).end.max(bar(&ed, 4).end));
    let milestone = bar(&ed, 4);
    assert_eq!(milestone.kind, BarKind::Milestone);
    assert_eq!(milestone.start, milestone.end);
    assert!(gantt_bar(&ed, &task(99, 1, 1), gantt_scale(&ed)).is_none());
}

#[test]
fn baseline_extends_both_scale_ends_and_requires_both_dates_for_a_bar() {
    let ed = editor(vec![task(1, 1, 1)]);
    let normal = gantt_scale(&ed);
    assert_eq!(normal.origin_day, ed.schedule().project_start.day_number());
    assert_eq!(normal.days, 30);
    let mut p = ed.project().clone();
    p.tasks[0].set_baseline_slot(projcore::Baseline {
        start: Some(projcore::DateTime::from_minutes(
            (normal.origin_day - 10) * 1440,
        )),
        ..projcore::Baseline::default()
    });
    let incomplete = ProjectEditor::new(p.clone());
    assert!(bar(&incomplete, 1).baseline.is_none());
    p.tasks[0].baselines[0].finish = Some(projcore::DateTime::from_minutes(
        (normal.origin_day + 60) * 1440,
    ));
    let ed = ProjectEditor::new(p);
    let scale = gantt_scale(&ed);
    assert_eq!(scale.origin_day, normal.origin_day - 10);
    assert_eq!(scale.days, 78);
    assert_eq!(bar(&ed, 1).baseline, Some((0, 70)));
    assert_eq!(bar(&ed, 1).start, 10);
    assert_eq!(bar(&ed, 1).end, 10);
}

#[test]
fn slot_one_alone_does_not_show_a_bar_or_extend_the_scale() {
    let ed = editor(vec![task(1, 1, 1)]);
    let normal = gantt_scale(&ed);
    let mut p = ed.project().clone();
    p.tasks[0].set_baseline_slot(projcore::Baseline {
        number: 1,
        start: Some(projcore::DateTime::from_minutes(
            (normal.origin_day - 10) * 1440,
        )),
        finish: Some(projcore::DateTime::from_minutes(
            (normal.origin_day + 60) * 1440,
        )),
        duration_min: Some(2400),
        ..projcore::Baseline::default()
    });
    let ed = ProjectEditor::new(p);
    assert_eq!(gantt_scale(&ed), normal);
    assert_eq!(bar(&ed, 1).baseline, None);
}

#[test]
fn week_cells_start_on_mondays_and_weekend_columns_are_saturday_sunday() {
    let scale = gantt_scale(&editor(vec![]));
    let weeks = zoom::tier(
        scale,
        zoom::Unit::Week,
        0.,
        15. * DEFAULT_DAY_W,
        zoom::CHAR_W,
    );
    assert_eq!(
        weeks.iter().map(|c| c.x).collect::<Vec<_>>(),
        [0., 7. * 22., 14. * 22.]
    );
    assert_eq!(zoom::Unit::Week.labels(scale.origin_day)[2], "1/5");
    assert_eq!(zoom::Unit::Week.labels(scale.origin_day + 14)[2], "1/19");
    for day in 0..14 {
        assert_eq!(
            editor(vec![])
                .project()
                .project_calendar()
                .day(scale.origin_day + day)
                .is_empty(),
            matches!(day % 7, 5 | 6)
        );
    }
}

#[test]
fn leveling_delays_leaves_and_rolls_up_the_summary() {
    let mut ed = editor(vec![task(1, 0, 1), task(2, 3, 2), task(3, 3, 2)]);
    ed.assign_resource(2, "Alice").unwrap();
    ed.assign_resource(3, "Alice").unwrap();
    assert!(bar(&ed, 3).delay.is_none());
    ed.toggle_level();
    let delayed = [bar(&ed, 2), bar(&ed, 3)]
        .into_iter()
        .find(|b| b.delay.is_some())
        .unwrap();
    assert_eq!(delayed.delay, Some((0, delayed.start)));
    let summary = bar(&ed, 1);
    assert_eq!(summary.start, bar(&ed, 2).start.min(bar(&ed, 3).start));
    assert_eq!(summary.end, bar(&ed, 2).end.max(bar(&ed, 3).end));
    ed.toggle_level();
    assert!(bar(&ed, 2).delay.is_none() && bar(&ed, 3).delay.is_none());
}

#[test]
fn horizontal_offsets_clamp_on_keys_resize_and_schedule_changes() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(NARROW);
    assert_eq!(v.table_w, 750.);
    v.pan_gantt(true);
    assert_eq!(v.gantt_x.get(), DEFAULT_DAY_W);
    assert!(v.key("right", false));
    assert_eq!(v.col, COL_DURATION);
    assert_eq!(v.table_x.get(), 0.);
    for _ in 0..500 {
        v.pan_gantt(true);
        v.key("right", false);
    }
    assert_eq!(v.table_x.get(), TABLE_W - 750.);
    assert_eq!(v.gantt_w, 228.);
    assert_eq!(v.gantt_x.get(), v.scale.width() - 228.);
    v.ed.set_duration(1, "1d").unwrap();
    v.layout(NARROW);
    assert_eq!(v.gantt_x.get(), 432.);
    // Wide enough for the whole table: nothing left to scroll.
    v.layout(2. * TABLE_W + 400.);
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (0., 0.));
    for _ in 0..10 {
        v.pan_gantt(false);
        v.key("left", false);
    }
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (0., 0.));
    for (width, table) in [
        (600., 450.),
        (800., 600.),
        (1180., TABLE_W),
        (1000., 750.),
        (2000., TABLE_W),
        (2400., TABLE_W),
    ] {
        assert_eq!(table_pane_width(width), table);
        assert!(width - table >= 140.);
    }
}

#[test]
fn layout_reserves_the_vertical_scrollbar_right_of_the_chart() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    for width in [460., 800., 1180., 2000.] {
        v.layout(width);
        assert_eq!(v.table_w + GANTT_INSET + v.gantt_w + SCROLLBAR_W, width);
    }
    v.layout(100.);
    assert_eq!(v.gantt_w, 0.);
}

#[test]
fn split_table_width_clamps_to_the_table_and_chart_minimums() {
    for width in [0., 100., 400., 1180., 2000.] {
        assert_eq!(split_table_width(width, None), table_pane_width(width));
    }
    for (width, split, table) in [
        (1180., 690., 690.),
        (1180., 5000., TABLE_W),
        (1180., -500., MIN_TABLE_W),
        // The chart keeps MIN_CHART_W: 400 - 6 - 16 - 48.
        (400., 900., 330.),
        (400., 10., MIN_TABLE_W),
        // Too narrow for both minimums: the chart's yields, then the window's width wins.
        (100., 500., MIN_TABLE_W),
        (30., 20., 30.),
        (0., 500., 0.),
    ] {
        assert_eq!(
            split_table_width(width, Some(split)),
            table,
            "{width} {split}"
        );
    }
}

#[test]
fn set_split_moves_the_divider_and_keeps_the_width_invariant() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(1180.);
    assert_eq!((v.split, v.table_w), (None, TABLE_W));
    for (split, table) in [(690., 690.), (-500., MIN_TABLE_W), (5000., TABLE_W)] {
        v.set_split(split);
        assert_eq!((v.split, v.table_w), (Some(table), table));
        assert_eq!(v.table_w + GANTT_INSET + v.gantt_w + SCROLLBAR_W, 1180.);
    }
    // A narrow window squeezes the table to leave the chart its minimum...
    v.layout(400.);
    assert_eq!((v.table_w, v.gantt_w), (330., MIN_CHART_W));
    // ...and a window too narrow for both keeps the ID column.
    v.layout(100.);
    assert_eq!((v.table_w, v.gantt_w), (MIN_TABLE_W, 32.));
    v.layout(1180.);
    assert_eq!(v.table_w, TABLE_W);
}

#[test]
fn a_split_drag_does_not_reveal_the_selected_column() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(900.);
    // At this width only the ID column (0..46) can scroll wholly out of view.
    v.col = COL_ID;
    // Scroll the ID column out of view, then drag the split.
    v.table_x.set(TABLE_W - v.table_w);
    v.set_split(700.);
    // Only clamped to the wider pane's range, not brought back to 0.
    assert_eq!(v.table_x.get(), TABLE_W - 700.);
    v.layout(1180.);
    assert_eq!(v.table_x.get(), TABLE_W - 700.);
    // A window resize that leaves the table pane alone does not reveal either.
    v.layout(1300.);
    assert_eq!((v.table_w, v.table_x.get()), (700., TABLE_W - 700.));
    // One that narrows the table pane does.
    v.layout(700.);
    assert_eq!(v.table_w, 630.);
    assert_eq!(v.table_x.get(), 0.);
}

#[test]
fn a_split_survives_frames_and_resizes_and_double_click_resets_it() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(1180.);
    v.set_split(700.);
    for width in [1180., 1180., 2000., 1180.] {
        v.layout(width);
        assert_eq!(v.table_w, 700.);
        assert_eq!(v.table_w + GANTT_INSET + v.gantt_w + SCROLLBAR_W, width);
    }
    let state = project_state(&v, None);
    assert!(state.contains(&("table_w".into(), ctlcore::json::Json::Num(700.))));
    assert!(state.contains(&("gantt_w".into(), ctlcore::json::Json::Num(458.))));
    v.reset_split();
    assert_eq!((v.split, v.table_w), (None, TABLE_W));
    v.layout(2000.);
    assert_eq!(v.table_w, table_pane_width(2000.));
    // Window view state: nothing to undo, nothing to save.
    assert_eq!(v.ed.undo_depth(), 0);
    assert!(!v.ed.dirty());
}

#[test]
fn pane_scroll_maps_gpui_offsets_and_clamps_both_ends() {
    let offset = PaneOffset::default();
    let bar = PaneScroll {
        offset: offset.clone(),
        content: TABLE_W,
        viewport: 400.,
    };
    assert_eq!(bar.content_size(), size(px(TABLE_W), px(SCROLLBAR_W)));
    // gpui offsets are negative as content moves left.
    bar.set_offset(point(px(-120.), px(0.)));
    assert_eq!(offset.get(), 120.);
    assert_eq!(bar.offset(), point(px(-120.), px(0.)));
    bar.set_offset(point(px(-10_000.), px(0.)));
    assert_eq!(offset.get(), TABLE_W - 400.);
    bar.set_offset(point(px(50.), px(0.)));
    assert_eq!(offset.get(), 0.);
    // Content that fits has nowhere to scroll.
    let fits = PaneScroll {
        offset: offset.clone(),
        content: 300.,
        viewport: 400.,
    };
    fits.set_offset(point(px(-50.), px(0.)));
    assert_eq!(offset.get(), 0.);
}

#[test]
fn table_and_chart_scrollbars_move_only_their_own_pane() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(NARROW);
    // The handles project_el renders.
    let (table, chart) = v.pane_scrolls();
    assert_eq!(
        (table.content, table.viewport),
        (TABLE_W, v.table_w),
        "the table bar spans the table"
    );
    assert_eq!(
        (chart.content, chart.viewport),
        (v.scale.width(), v.gantt_w),
        "the chart bar spans the timescale"
    );
    table.set_offset(point(px(-90.), px(0.)));
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (90., 0.));
    chart.set_offset(point(px(-200.), px(0.)));
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (90., 200.));
    // Offsets changed by keys show on the bars.
    v.pan_gantt(true);
    assert_eq!(chart.offset(), point(px(-(200. + DEFAULT_DAY_W)), px(0.)));
    assert_eq!(table.offset(), point(px(-90.), px(0.)));
    // The next frame's layout keeps both where they were put.
    v.layout(NARROW);
    assert_eq!(
        (v.table_x.get(), v.gantt_x.get()),
        (90., 200. + DEFAULT_DAY_W)
    );
}

#[test]
fn a_table_scrollbar_drag_is_not_undone_by_the_next_frame() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(600.);
    // The ID column (0..46) is the one a scrolled table hides.
    v.col = COL_ID;
    v.table_x.set(TABLE_W - v.table_w);
    v.layout(600.);
    assert_eq!(v.table_x.get(), TABLE_W - v.table_w);
    // A resize still brings the selected column back: clamping alone would
    // leave the table at TABLE_W - 750, ID still hidden.
    v.layout(NARROW);
    assert_eq!(v.table_x.get(), 0.);
}

#[test]
fn indent_and_outdent_change_geometry_and_undo_redo_restore_it() {
    let mut t = new_project_tab("Project1");
    let status = t.status.clone();
    indent_project(&mut t, 1);
    indent_project(&mut t, -1);
    assert_eq!(t.status, status);
    assert!(!t.dirty);
    let Surface::Project(v) = &mut t.surface else {
        unreachable!()
    };
    assert_eq!(v.ed.undo_depth(), 0);
    v.ed = editor(vec![task(1, 1, 1), task(2, 4, 1)]);
    // Off the empty plan's entry row, onto task 2.
    v.select_row(1);
    indent_project(&mut t, -1);
    let Surface::Project(v) = &t.surface else {
        unreachable!()
    };
    assert!(!t.dirty);
    assert_eq!(v.ed.undo_depth(), 0);
    indent_project(&mut t, 1);
    let Surface::Project(v) = &mut t.surface else {
        unreachable!()
    };
    assert!(t.dirty);
    assert_eq!(bar(&v.ed, 1).state(), "summary 0-3");
    v.ed.undo();
    assert_eq!(bar(&v.ed, 1).state(), "on-track 0-0");
    v.ed.redo();
    assert_eq!(bar(&v.ed, 1).state(), "summary 0-3");
    indent_project(&mut t, -1);
    let Surface::Project(v) = &mut t.surface else {
        unreachable!()
    };
    assert_eq!(bar(&v.ed, 1).state(), "on-track 0-0");
    v.ed.indent(2, 30).unwrap();
    v.ed.mark_saved();
    t.dirty = false;
    let depth = v.ed.undo_depth();
    indent_project(&mut t, 1);
    let Surface::Project(v) = &t.surface else {
        unreachable!()
    };
    assert_eq!(v.ed.undo_depth(), depth);
    assert!(!t.dirty && !v.ed.dirty());
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(x), px(y)),
        size: size(px(w), px(h)),
    }
}

#[test]
fn regions_use_body_origin_clip_both_axes_and_reject_hidden_bars() {
    let mut v = ProjectView::new(editor(vec![task(1, 2, 1)]).project().clone(), false);
    v.layout(800.);
    let mut probes = Probes {
        last: vec![("project-body".into(), rect(10., 80., 800., 400.))],
        ..Probes::default()
    };
    assert_eq!(
        project_region(&v, &probes, harness::Region::Gantt, Probes::get).unwrap(),
        rect(616., 80., 178., 400.)
    );
    assert!(
        project_region(&v, &probes, harness::Region::Bar(1), Probes::get)
            .unwrap_err()
            .contains("row is not rendered")
    );
    assert!(
        project_region(&v, &probes, harness::Region::Bar(99), Probes::get)
            .unwrap_err()
            .contains("no task")
    );
    probes
        .last
        .push(("bar:1".into(), rect(600., 75., 40., 14.)));
    assert_eq!(
        project_region(&v, &probes, harness::Region::Bar(1), Probes::get).unwrap(),
        rect(616., 80., 24., 9.)
    );
    for r in [
        rect(550., 100., 40., 14.),
        rect(610., 100., 6., 14.), // divider/inset is outside the chart viewport
        rect(620., 60., 40., 14.),
        rect(620., 480., 40., 14.),
        rect(796., 100., 10., 14.), // under the vertical scrollbar, right of the chart
        rect(810., 100., 40., 14.),
    ] {
        probes.last[1].1 = r;
        assert!(
            project_region(&v, &probes, harness::Region::Bar(1), Probes::get)
                .unwrap_err()
                .contains("outside")
        );
    }
}

/// #1208: a heading is its probe, clipped to the table pane's columns; the
/// heading row is above the body, so it is not clipped to the body's rows.
#[test]
fn a_heading_region_clips_to_the_table_pane_only_across() {
    let mut v = ProjectView::new(editor(vec![task(1, 2, 1)]).project().clone(), false);
    v.layout(800.);
    let table_w = v.table_w;
    let mut probes = Probes {
        last: vec![("project-body".into(), rect(10., 80., 800., 400.))],
        ..Probes::default()
    };
    let heading = |probes: &Probes| {
        project_region(&v, probes, harness::Region::ProjectHeading(3), Probes::get)
    };
    assert!(heading(&probes).unwrap_err().contains("not been laid out"));
    probes
        .last
        .push(("project-heading:3".into(), rect(200., 58., 70., 22.)));
    assert_eq!(heading(&probes).unwrap(), rect(200., 58., 70., 22.));
    // Part scrolled under the chart: the visible part.
    probes.last[1].1 = rect(10. + table_w - 20., 58., 70., 22.);
    assert_eq!(
        heading(&probes).unwrap(),
        rect(10. + table_w - 20., 58., 20., 22.)
    );
    // Wholly scrolled out, either side.
    for r in [
        rect(10. + table_w, 58., 70., 22.),
        rect(-70., 58., 70., 22.),
    ] {
        probes.last[1].1 = r;
        assert!(
            heading(&probes)
                .unwrap_err()
                .contains("the Duration heading is outside the table viewport")
        );
    }
}

#[test]
fn scrollbar_regions_are_their_probed_strips() {
    let mut v = ProjectView::new(editor(vec![task(1, 2, 1)]).project().clone(), false);
    v.layout(800.);
    let mut probes = Probes::default();
    for region in [
        harness::Region::ProjectHbarTable,
        harness::Region::ProjectHbarChart,
        harness::Region::ProjectVbar,
    ] {
        assert!(
            project_region(&v, &probes, region, Probes::get)
                .unwrap_err()
                .contains("has not been laid out")
        );
    }
    probes.last = vec![
        ("project-hbar-table".into(), rect(10., 480., 400., 16.)),
        ("project-hbar-chart".into(), rect(416., 480., 378., 16.)),
        ("project-vbar".into(), rect(794., 80., 16., 400.)),
    ];
    assert_eq!(
        project_region(&v, &probes, harness::Region::ProjectHbarTable, Probes::get).unwrap(),
        rect(10., 480., 400., 16.)
    );
    assert_eq!(
        project_region(&v, &probes, harness::Region::ProjectHbarChart, Probes::get).unwrap(),
        rect(416., 480., 378., 16.)
    );
    assert_eq!(
        project_region(&v, &probes, harness::Region::ProjectVbar, Probes::get).unwrap(),
        rect(794., 80., 16., 400.)
    );
}

#[test]
fn the_timeline_region_is_its_probe_while_shown() {
    let mut v = ProjectView::new(editor(vec![task(1, 2, 1)]).project().clone(), false);
    v.layout(800.);
    let mut probes = Probes::default();
    let region = harness::Region::ProjectTimeline;
    assert!(
        project_region(&v, &probes, region, Probes::get)
            .unwrap_err()
            .contains("has not been laid out")
    );
    probes.last = vec![("project-timeline".into(), rect(0., 60., 800., 84.))];
    assert_eq!(
        project_region(&v, &probes, region, Probes::get).unwrap(),
        rect(0., 60., 800., 84.)
    );
    // Hidden, a probe left over from the last shown frame must not answer.
    v.timeline = false;
    assert!(
        project_region(&v, &probes, region, Probes::get)
            .unwrap_err()
            .contains("the Timeline is hidden")
    );
}

#[test]
fn timeline_state_names_the_span_and_follows_leveling() {
    use ctlcore::json::Json;
    let get = |v: &ProjectView, key: &str| {
        project_state(v, None)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, j)| j)
            .unwrap()
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/mspdi/02-link-fs.xml");
    let Surface::Project(v) = project_tab_from_path(&path).surface else {
        unreachable!()
    };
    assert_eq!(get(&v, "timeline"), Json::Str("shown".into()));
    assert_eq!(get(&v, "timeline_start"), Json::Str("Mon 3/2/26".into()));
    assert_eq!(get(&v, "timeline_finish"), Json::Str("Thu 3/5/26".into()));

    // Two unlinked 1d tasks on one resource: leveling serialises them, so the
    // finish the bars reach moves a working day later.
    let mut v = ProjectView::new(
        editor(vec![task(1, 1, 1), task(2, 1, 1)]).project().clone(),
        false,
    );
    v.ed.assign_resource(1, "Alice").unwrap();
    v.ed.assign_resource(2, "Alice").unwrap();
    let unleveled = get(&v, "timeline_finish");
    v.ed.toggle_level();
    let leveled = get(&v, "timeline_finish");
    assert_ne!(leveled, unleveled);
    assert_eq!(leveled, Json::Str(project_date(v.ed.disp_project_finish())));
    v.ed.toggle_level();
    assert_eq!(get(&v, "timeline_finish"), unleveled);

    // A manual task pinned before the project start is drawn there, so the
    // Timeline starts with it rather than at the scheduler's anchor.
    let mut early = task(1, 1, 1);
    early.manual = true;
    early.manual_start = Some(projcore::DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
    let mut p = untitled_project();
    p.start_date = Some(projcore::DateTime::from_ymd_hm(2026, 3, 9, 8, 0));
    p.tasks = vec![early, task(2, 1, 1)];
    let v = ProjectView::new(p, false);
    assert_eq!(project_date(v.ed.schedule().project_start), "Mon 3/9/26");
    assert_eq!(get(&v, "timeline_start"), Json::Str("Mon 3/2/26".into()));
    assert_eq!(get(&v, "timeline_finish"), Json::Str("Mon 3/9/26".into()));
}

#[test]
fn harness_state_uses_displayed_ids_and_includes_tasks_outside_the_view() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/mspdi/10-summary.xml");
    let t = project_tab_from_path(&path);
    let Surface::Project(v) = t.surface else {
        unreachable!()
    };
    let state = project_state(&v, None);
    use ctlcore::json::Json;
    assert!(state.contains(&("bar_1".into(), Json::Str("summary 0-1".into()))));
    assert!(state.contains(&("bar_2".into(), Json::Str("critical 0-0".into()))));
    assert!(state.contains(&("bar_3".into(), Json::Str("critical 1-1".into()))));
    let mut p = untitled_project();
    p.tasks = (1..=100).map(|id| task(id, 1, 1)).collect();
    p.tasks[0].id = 200;
    let v = ProjectView::new(p, false);
    let state = project_state(&v, None);
    assert!(state.contains(&("bar_200".into(), Json::Str("critical 0-0".into()))));
    assert!(state.contains(&("bar_100".into(), Json::Str("critical 0-0".into()))));
}

fn monday_scale(days: i64) -> GanttScale {
    let origin_day = (0..7)
        .find(|d| {
            GanttScale {
                origin_day: *d,
                days: 1,
                day_w: DEFAULT_DAY_W,
            }
            .date(0)
            .weekday()
                == 1
        })
        .unwrap();
    GanttScale {
        origin_day,
        days,
        day_w: DEFAULT_DAY_W,
    }
}

#[test]
fn row_rules_fill_the_body_and_follow_the_scroll_phase() {
    let rules = |body_h: f32, scroll_y: f32| -> Vec<f32> {
        ruled_rows_of(body_h, scroll_y, ROW_H)
            .into_iter()
            .map(|(_, y)| y)
            .collect()
    };
    assert_eq!(rules(100., 0.), [27., 55., 83.]);
    assert_eq!(rules(84., 0.), [27., 55., 83.]);
    assert_eq!(rules(83., 0.), [27., 55.]);
    assert!(rules(0., 0.).is_empty());
    // A scrolled list moves the rules up by the offset modulo a row, never off the grid.
    assert_eq!(rules(100., 10.), [17., 45., 73.]);
    assert_eq!(rules(100., 10. + ROW_H * 7.), rules(100., 10.));
    assert_eq!(rules(100., 27.), [0., 28., 56., 84.]);
    assert_eq!(rules(100., 28.), rules(100., 0.));
}

/// #1575: each rule knows its row, so a gridline's interval lands on every
/// Nth row however the body is scrolled; the bottom tier's lines at the Days
/// level are the day lines, and the major tier's start at the weeks.
#[test]
fn ruled_rows_carry_their_row_and_tier_lines_follow_the_timescale() {
    assert_eq!(
        ruled_rows_of(84., 0., ROW_H),
        [(0, 27.), (1, 55.), (2, 83.)]
    );
    assert_eq!(
        ruled_rows_of(60., ROW_H * 3. + 10., ROW_H),
        [(3, 17.), (4, 45.)]
    );
    let two = projcore::formatting::Gridline {
        interval: 2,
        ..projcore::formatting::Gridline::solid()
    };
    let at: Vec<usize> = ruled_rows_of(84., 0., ROW_H)
        .into_iter()
        .filter(|(row, _)| two.at_interval(*row))
        .map(|(row, _)| row)
        .collect();
    assert_eq!(at, [1], "the second row of the body");
    let scale = GanttScale {
        origin_day: 0,
        days: 60,
        day_w: 22.,
    };
    // The minor tier at the Days level is the days, each line carrying its
    // day's index from the scale's origin, so a scroll of 11 px shows the
    // same lines under the same indexes (#1575 r1).
    assert_eq!(
        tier_lines(scale, 0., 110., true),
        [(0, 0.), (1, 22.), (2, 44.), (3, 66.), (4, 88.)]
    );
    assert_eq!(
        tier_lines(scale, 11., 110., true),
        [(1, 11.), (2, 33.), (3, 55.), (4, 77.), (5, 99.)]
    );
    let weeks: Vec<f32> = tier_lines(scale, 0., 22. * 21., false)
        .into_iter()
        .map(|(_, x)| x)
        .collect();
    assert!(
        weeks.len() <= 4 && weeks.windows(2).all(|w| w[1] - w[0] == 22. * 7.),
        "{weeks:?}"
    );
    assert_eq!(
        instant_x(scale, 0., projcore::DateTime::from_minutes(1440 + 720)),
        33.
    );
    assert_eq!(
        instant_x(scale, 10., projcore::DateTime::from_minutes(1440 + 720)),
        23.
    );
}

/// #1575 r2: the Status Date line is the plan's `StatusDate` option alone;
/// a plan with a current date and no status date draws the Current Date line
/// and no Status Date line, however the latter is styled.
#[test]
fn a_status_date_line_needs_a_status_date() {
    use projcore::formatting::gridlines::{Gridline, Gridlines, Line};
    // Two days after the plan's start, inside a 2000 px chart.
    let p = untitled_project();
    let start = p.start_date.unwrap();
    let day = |n: i64| projcore::DateTime::from_minutes(start.minutes() + n * 1440).to_mspdi();
    let view = |status: Option<i64>| {
        let mut p = untitled_project();
        p.tasks = vec![task(1, 1, 1)];
        p.set_option("CurrentDate", Some(&day(2)));
        if let Some(n) = status {
            p.set_option("StatusDate", Some(&day(n)));
        }
        let mut v = ProjectView::new(p, false);
        v.layout(2000.);
        v
    };
    let v = view(None);
    let scale = v.chart_scale();
    let mut grid = Gridlines::default();
    grid.set(Line::CurrentDate, Gridline::solid());
    grid.set(Line::StatusDate, Gridline::solid());
    let lines: Vec<Line> = date_lines(&v.ed, &grid, scale, 0., scale.width())
        .into_iter()
        .map(|(line, _, _)| line)
        .collect();
    assert!(lines.contains(&Line::CurrentDate), "{lines:?}");
    assert!(!lines.contains(&Line::StatusDate), "{lines:?}");
    // With a status date of its own the line draws there.
    let v = view(Some(1));
    let xs: Vec<(Line, f32)> = date_lines(&v.ed, &grid, scale, 0., scale.width())
        .into_iter()
        .map(|(line, _, x)| (line, x))
        .collect();
    let at = |l: Line| xs.iter().find(|(line, _)| *line == l).map(|(_, x)| *x);
    assert!(at(Line::StatusDate).is_some());
    assert!(at(Line::StatusDate) < at(Line::CurrentDate), "{xs:?}");
}

#[test]
fn filler_rows_count_the_ruled_space_below_the_last_task() {
    assert_eq!(filler_rows_of(280., 0., 0, ROW_H), 10);
    assert_eq!(filler_rows_of(280., 0., 3, ROW_H), 7);
    assert_eq!(
        filler_rows_of(290., 0., 3, ROW_H),
        8,
        "a partly visible row counts"
    );
    assert_eq!(filler_rows_of(280., 0., 10, ROW_H), 0);
    assert_eq!(
        filler_rows_of(280., 0., 20, ROW_H),
        0,
        "more tasks than fit"
    );
    assert_eq!(
        filler_rows_of(280., 20. * ROW_H - 280., 20, ROW_H),
        0,
        "scrolled to the end"
    );
    assert_eq!(
        filler_rows_of(280., 10., 3, ROW_H),
        8,
        "scrolling uncovers more empty rows"
    );
}

/// The default Bottom Tier Column at the Days level: the day lines (#1575).
fn day_lines(scale: GanttScale, offset: f32, width: f32) -> Vec<f32> {
    let default = projcore::formatting::gridlines::Line::BottomTierColumn.default_style();
    bottom_tier_lines(scale, offset, width, &default)
        .into_iter()
        .map(|(_, x)| x)
        .collect()
}

#[test]
fn day_lines_follow_horizontal_scroll_and_stay_in_the_viewport() {
    let scale = monday_scale(30);
    assert_eq!(day_lines(scale, 0., 110.), [0., 22., 44., 66., 88.]);
    assert_eq!(day_lines(scale, 11., 110.), [11., 33., 55., 77., 99.]);
    assert_eq!(
        day_lines(scale, DEFAULT_DAY_W, 2. * DEFAULT_DAY_W),
        [0., 22.]
    );
    // No lines past the scale's last day.
    assert_eq!(
        day_lines(scale, 28. * DEFAULT_DAY_W, 10. * DEFAULT_DAY_W),
        [0., 22.]
    );
    assert!(day_lines(scale, 0., 0.).is_empty());
}

#[test]
fn shaded_days_are_the_visible_weekends() {
    let scale = monday_scale(30);
    let cal = untitled_project().project_calendar();
    assert_eq!(
        shaded_days(scale, Some(&cal), 0., 14. * DEFAULT_DAY_W),
        [5, 6, 12, 13]
    );
    assert_eq!(
        shaded_days(scale, Some(&cal), 6. * DEFAULT_DAY_W, 2. * DEFAULT_DAY_W),
        [6]
    );
    assert_eq!(
        shaded_days(scale, Some(&cal), 5.5 * DEFAULT_DAY_W, DEFAULT_DAY_W),
        [5, 6]
    );
    assert_eq!(
        shaded_days(scale, Some(&cal), 26. * DEFAULT_DAY_W, 10. * DEFAULT_DAY_W),
        [26, 27]
    );
}

#[test]
fn shaded_days_follow_project_calendar_exceptions_and_week() {
    use projcore::{Calendar, CalendarException, DateTime, DayWorking};
    let scale = monday_scale(14);
    let wed = DateTime::from_minutes((scale.origin_day + 2) * 1440);
    let sat = DateTime::from_minutes((scale.origin_day + 5) * 1440);
    let mut p = untitled_project();
    let mut cal = Calendar::standard(p.default_calendar_uid);
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
    p.calendars = vec![cal];
    assert_eq!(
        shaded_days(
            scale,
            p.project_shading_calendar().as_ref(),
            0.,
            7. * DEFAULT_DAY_W
        ),
        [2, 6]
    );

    p.calendars[0].exceptions.clear();
    p.calendars[0].week[0] = p.calendars[0].week[1].clone();
    p.calendars[0].week[5] = Some(DayWorking::default());
    assert_eq!(
        shaded_days(
            scale,
            p.project_shading_calendar().as_ref(),
            0.,
            7. * DEFAULT_DAY_W
        ),
        [4, 5]
    );

    p.default_calendar_uid = 99;
    assert_eq!(
        shaded_days(
            scale,
            p.project_shading_calendar().as_ref(),
            0.,
            7. * DEFAULT_DAY_W
        ),
        [5, 6]
    );
}

#[test]
fn shaded_days_recompute_after_project_replacement() {
    use projcore::{Calendar, CalendarException, DateTime, DayWorking};
    let mut p = untitled_project();
    p.tasks = vec![task(1, 5, 1)];
    let mut v = ProjectView::new(p.clone(), false);
    let scale = gantt_scale(&v.ed);
    let wed = DateTime::from_minutes((scale.origin_day + 2) * 1440);
    assert!(
        !shaded_days(
            scale,
            v.ed.project().project_shading_calendar().as_ref(),
            0.,
            7. * DEFAULT_DAY_W
        )
        .contains(&2)
    );
    let mut cal = Calendar::standard(p.default_calendar_uid);
    cal.exceptions.push(CalendarException::date_range(
        wed,
        wed,
        DayWorking::default(),
    ));
    p.calendars = vec![cal];
    v.ed.replace_project(p);
    assert!(
        shaded_days(
            scale,
            v.ed.project().project_shading_calendar().as_ref(),
            0.,
            7. * DEFAULT_DAY_W
        )
        .contains(&2)
    );
}

#[test]
fn closed_project_week_has_no_global_shading() {
    use projcore::{Calendar, DayWorking};
    let mut p = untitled_project();
    p.calendars = vec![
        Calendar::base(
            p.default_calendar_uid,
            "Closed",
            std::array::from_fn(|_| DayWorking::default()),
        ),
        Calendar::standard(3),
    ];
    let mut leaf = task(1, 3, 1);
    leaf.calendar_uid = Some(3);
    p.tasks = vec![leaf];
    let scale = gantt_scale(&ProjectEditor::new(p.clone()));
    assert!(
        shaded_days(
            scale,
            p.project_shading_calendar().as_ref(),
            0.,
            7. * DEFAULT_DAY_W
        )
        .is_empty()
    );
}

#[test]
fn harness_state_reports_filler_rows_for_the_laid_out_body() {
    use ctlcore::json::Json;
    let mut p = untitled_project();
    p.tasks = (1..=3).map(|id| task(id, 1, 1)).collect();
    let v = ProjectView::new(p, false);
    let filler = |body_h| {
        project_state(&v, body_h)
            .into_iter()
            .find(|(k, _)| k == "filler_rows")
            .map(|(_, j)| j)
    };
    assert_eq!(filler(Some(280.)), Some(Json::Num(7.)));
    assert_eq!(filler(None), Some(Json::Num(0.)));
    let v = ProjectView::new(untitled_project(), false);
    assert!(project_state(&v, Some(280.)).contains(&("filler_rows".into(), Json::Num(10.))));
}

#[test]
fn a_short_plan_on_a_wide_window_has_days_across_the_whole_chart() {
    let mut p = untitled_project();
    p.tasks = vec![task(1, 1, 1)];
    let mut v = ProjectView::new(p, false);
    let before = bar(&v.ed, 1);
    v.layout(2000.);
    assert!(
        v.gantt_w > v.scale.width(),
        "the plan is shorter than the chart"
    );
    let chart = v.chart_scale();
    assert!(chart.width() >= v.gantt_w);
    let last = *day_lines(chart, 0., v.gantt_w).last().unwrap();
    assert!(
        v.gantt_w - last <= DEFAULT_DAY_W,
        "last day line {last} of {}",
        v.gantt_w
    );
    let weekend = shaded_days(
        chart,
        v.ed.project().project_shading_calendar().as_ref(),
        0.,
        v.gantt_w,
    );
    assert!(*weekend.last().unwrap() as f32 * DEFAULT_DAY_W >= v.gantt_w - 7. * DEFAULT_DAY_W);
    // Bars keep their days, and the widened days are not a place to scroll to.
    assert_eq!(bar(&v.ed, 1), before);
    assert_eq!(chart.origin_day, v.scale.origin_day);
    v.pan_gantt(true);
    assert_eq!(v.gantt_x.get(), 0.);
    // A plan wider than the chart draws and scrolls on its own scale.
    v.ed.set_duration(1, "60d").unwrap();
    v.layout(1180.);
    assert_eq!(v.chart_scale(), v.scale);
}

#[test]
fn a_manual_summary_bar_carries_its_rollup_and_warning() {
    let mut ed = editor(vec![task(1, 0, 1), task(2, 1, 2), task(3, 4, 2)]);
    assert_eq!(bar(&ed, 1).state(), "summary 0-3");
    ed.set_manual(1, true).unwrap();
    assert_eq!(bar(&ed, 1).state(), "summary 0-3 rollup 0-3");
    assert_eq!(bar(&ed, 1).warning_days(), None);
    // Shorter than its subtasks: they run past it, in the warning colour.
    ed.set_duration(1, "1d").unwrap();
    assert_eq!(bar(&ed, 1).state(), "summary 0-0 rollup 0-3 warning");
    assert_eq!(bar(&ed, 1).warning_days(), Some((1, 3)));
    // Longer: the rollup sits inside it and nothing warns.
    ed.set_duration(1, "10d").unwrap();
    assert_eq!(bar(&ed, 1).state(), "summary 0-11 rollup 0-3");
    // Made automatic again, it rolls up and draws no second bar.
    ed.set_manual(1, false).unwrap();
    assert_eq!(bar(&ed, 1).state(), "summary 0-3");
    // A finish later on the summary's own last day warns on that day.
    let same_day = GanttBar {
        kind: BarKind::Summary,
        inactive: false,
        start: 0,
        end: 3,
        baseline: None,
        delay: None,
        rollup: Some((0, 3)),
        warning: true,
        geom: BarGeom::default(),
    };
    assert_eq!(same_day.warning_days(), Some((3, 3)));
    // Without a rollup, a warned bar marks its own finish day.
    let bare = GanttBar {
        rollup: None,
        ..same_day
    };
    assert_eq!(bare.warning_days(), Some((3, 3)));
    assert_eq!(
        GanttBar {
            warning: false,
            ..bare
        }
        .warning_days(),
        None
    );
}

/// A manual summary at `level` from `start` to `finish` (days of Jan 2026).
fn manual_summary(uid: i32, level: u32, start: u32, finish: u32) -> Task {
    Task {
        summary: true,
        manual: true,
        manual_start: Some(projcore::DateTime::from_ymd_hm(2026, 1, start, 8, 0)),
        manual_finish: Some(projcore::DateTime::from_ymd_hm(2026, 1, finish, 17, 0)),
        ..task(uid, 0, level)
    }
}

#[test]
fn inactive_manual_summary_retains_its_secondary_rollup_outline() {
    let mut summary = manual_summary(1, 1, 5, 6);
    summary.active = Some(false);
    let ed = editor(vec![summary, task(2, 4, 2)]);
    let b = bar(&ed, 1);
    assert!(b.inactive);
    assert_eq!(b.kind, BarKind::Summary);
    assert!(b.rollup.is_some());
    assert!(b.state().contains("rollup"));
    assert!(b.warning);
    assert!(b.state().contains("warning"));
    assert!(b.warning_days().is_some());
}

#[test]
fn a_summary_past_its_manual_parent_warns_on_its_own_finish_day() {
    // O (1/5..1/16) > I (1/5..1/23) > A 1d: I's rollup is inside I, but I
    // finishes after O. Day 0 is Monday 1/5.
    let ed = editor(vec![
        manual_summary(1, 1, 5, 16),
        manual_summary(2, 2, 5, 23),
        task(3, 1, 3),
    ]);
    let inner = bar(&ed, 2);
    assert_eq!(inner.state(), "summary 0-18 rollup 0-0 warning");
    assert_eq!(inner.warning_days(), Some((18, 18)));
    // O's rollup (through I's own span) runs past O's finish.
    assert_eq!(bar(&ed, 1).state(), "summary 0-11 rollup 0-18 warning");
    assert_eq!(bar(&ed, 1).warning_days(), Some((12, 18)));
    // O (1/5..1/6) > I (1/12..1/16) > A 2d, floored to 1/12.
    let ed = editor(vec![
        manual_summary(1, 1, 5, 6),
        manual_summary(2, 2, 12, 16),
        task(3, 2, 3),
    ]);
    let inner = bar(&ed, 2);
    assert_eq!(inner.state(), "summary 7-11 rollup 7-8 warning");
    assert_eq!(inner.warning_days(), Some((11, 11)));
    // w1: a manual leaf past its manual summary warns in the editor, but
    // only the summary's bar shows it.
    let leaf = Task {
        manual: true,
        manual_start: Some(projcore::DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        manual_finish: Some(projcore::DateTime::from_ymd_hm(2026, 1, 9, 17, 0)),
        ..task(2, 5, 2)
    };
    let ed = editor(vec![manual_summary(1, 1, 5, 6), leaf]);
    assert!(ed.summary_warning(2));
    assert_eq!(bar(&ed, 2).state(), "critical 0-4");
    assert_eq!(bar(&ed, 2).warning_days(), None);
    assert_eq!(bar(&ed, 1).state(), "summary 0-1 rollup 0-4 warning");
}

#[test]
fn bar_styles_hide_critical_colour_and_baseline_only() {
    let base = GanttBar {
        inactive: false,
        kind: BarKind::Critical,
        start: 2,
        end: 5,
        baseline: Some((1, 4)),
        delay: Some((0, 2)),
        rollup: None,
        warning: false,
        geom: BarGeom::default(),
    };
    assert_eq!(styled_bar(base, true, true), base);
    assert_eq!(
        styled_bar(base, false, true),
        GanttBar {
            kind: BarKind::OnTrack,
            ..base
        }
    );
    assert_eq!(
        styled_bar(base, true, false),
        GanttBar {
            baseline: None,
            ..base
        }
    );
    // Summaries, milestones and the leveling delay are not bar styles here.
    for kind in [BarKind::Summary, BarKind::Milestone, BarKind::OnTrack] {
        let bar = GanttBar { kind, ..base };
        assert_eq!(styled_bar(bar, false, true), bar);
        assert_eq!(styled_bar(bar, false, false).kind, kind);
        assert_eq!(styled_bar(bar, false, false).delay, base.delay);
    }
}

#[test]
fn wheel_routes_to_the_pane_under_the_pointer_at_the_split_bar_middle() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(1180.);
    let mid = v.table_w + GANTT_INSET / 2.;
    assert_eq!(v.pane_at(0.), ProjectPane::Table);
    assert_eq!(v.pane_at(mid - 0.5), ProjectPane::Table);
    assert_eq!(v.pane_at(mid), ProjectPane::Chart);
    // The vertical scrollbar column, right of the chart.
    assert_eq!(v.pane_at(1180. - 1.), ProjectPane::Chart);
    v.set_split(400.);
    assert_eq!(v.pane_at(400. + GANTT_INSET / 2. - 0.5), ProjectPane::Table);
    assert_eq!(v.pane_at(400. + GANTT_INSET / 2.), ProjectPane::Chart);

    // Over the table only table_x moves; gpui's negative delta scrolls right.
    assert!(v.wheel(10., -30.));
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (30., 0.));
    // Over the chart only gantt_x moves.
    assert!(v.wheel(700., -50.));
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (30., 50.));
    // Positive delta scrolls back left.
    assert!(v.wheel(700., 20.));
    assert_eq!(v.gantt_x.get(), 30.);
    // A y-only delta reaches `wheel` as zero: nothing moves.
    assert!(!v.wheel(10., 0.));
    assert!(!v.wheel(700., 0.));
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (30., 30.));
}

#[test]
fn wheel_clamps_like_clamp_offsets_and_reports_no_change_at_an_edge() {
    let mut v = ProjectView::new(editor(vec![task(1, 60, 1)]).project().clone(), false);
    v.layout(NARROW);
    assert_eq!((v.table_w, v.gantt_w), (750., 228.));
    // At the left edge a leftward wheel is a no-op.
    assert!(!v.wheel(10., 40.));
    assert!(!v.wheel(800., 40.));
    assert_eq!((v.table_x.get(), v.gantt_x.get()), (0., 0.));
    // Far right clamps to content - viewport.
    assert!(v.wheel(10., -100_000.));
    assert!(v.wheel(800., -100_000.));
    assert_eq!(v.table_x.get(), TABLE_W - 750.);
    assert_eq!(v.gantt_x.get(), v.scale.width() - 228.);
    assert!(!v.wheel(10., -10.));
    assert!(!v.wheel(800., -10.));
    // Wide enough for the whole table: the table has nothing to scroll.
    v.layout(2. * TABLE_W + 400.);
    assert_eq!(v.table_x.get(), 0.);
    assert!(!v.wheel(10., -50.));
    assert_eq!(v.table_x.get(), 0.);
}

fn set_layout(ed: &mut ProjectEditor, json: &str) {
    let view = projcore::json::Json::parse(&format!(r#"{{"layout":{json}}}"#)).unwrap();
    ed.edit_formatting(|f| {
        f.apply_view(&view).unwrap();
    });
}

fn plain(kind: BarKind) -> GanttBar {
    GanttBar {
        kind,
        inactive: false,
        start: 3,
        end: 5,
        baseline: None,
        delay: None,
        rollup: None,
        warning: false,
        geom: BarGeom::default(),
    }
}

#[test]
fn the_default_geometry_is_the_original_boxes() {
    let b = bar_boxes(None);
    assert_eq!(
        (b.leaf, b.thin, b.summary, b.summary_top, b.post, b.diamond),
        ((5., 14.), (11., 6.), (9., 10.), 6., 3., (5., 14.))
    );
    assert_eq!(bar_boxes(Some(BAR_PX)), b);
    for kind in [
        BarKind::Critical,
        BarKind::OnTrack,
        BarKind::Summary,
        BarKind::Milestone,
    ] {
        let bar = plain(kind);
        let expected = match kind {
            BarKind::Summary => (66., 9., 66., 10.),
            BarKind::Milestone => (3. * 22. + 11. - 7., 5., 14., 14.),
            _ => (66., 5., 66., 14.),
        };
        assert_eq!(bar_rect(&bar, 22.), expected, "{kind:?}");
        assert_eq!(bar.geom, BarGeom::default());
    }
}

#[test]
fn bar_height_scales_each_kind_about_its_own_centre() {
    // 12 pt = 16 px.
    let b = bar_boxes(Some(16.));
    assert_eq!(b.leaf, (4., 16.));
    assert_eq!(b.diamond, (4., 16.));
    // round(10 * 16/14) = 11 px tall, centred on y 14.
    assert_eq!(b.summary, (14. - 5.5, 11.));
    assert_eq!(b.thin.0 + b.thin.1 / 2., 14.);
    // Every kind keeps its centre line for every height that fits the row.
    for px in [8., 10., 13.333_333, 16., 20., 24.] {
        let b = bar_boxes(Some(px));
        assert!((b.leaf.0 + b.leaf.1 / 2. - 12.).abs() < 1e-3, "{px}");
        assert!((b.diamond.0 + b.diamond.1 / 2. - 12.).abs() < 1e-3, "{px}");
        assert!((b.summary.0 + b.summary.1 / 2. - 14.).abs() < 1e-3, "{px}");
        assert!((b.thin.0 + b.thin.1 / 2. - 14.).abs() < 1e-3, "{px}");
    }
    // Capped at the row and kept inside it.
    let big = bar_boxes(Some(32.));
    assert_eq!(big.leaf, (0., ROW_H));
    assert!(big.summary.0 >= 0. && big.summary.0 + big.summary.1 <= ROW_H);
    assert!(big.summary_top <= big.summary.1);
}

#[test]
fn a_layout_bar_height_reaches_gantt_bar_and_the_overlay_rectangle() {
    let mut ed = editor(vec![task(1, 0, 1), task(2, 2, 2), task(3, 0, 2)]);
    let (leaf, sum, ms) = (bar(&ed, 2), bar(&ed, 1), bar(&ed, 3));
    assert_eq!(leaf.geom.height, None);
    set_layout(&mut ed, r#"{"bar_height":12}"#);
    let tall = bar(&ed, 2);
    assert_eq!(tall.geom.height, Some(16.));
    let (_, y, _, h) = bar_rect(&tall, 22.);
    assert_eq!((y, h), (4., 16.));
    assert_eq!(bar_rect(&bar(&ed, 3), 22.).3, 16.);
    assert_eq!(
        bar_rect(&bar(&ed, 1), 22.).1 + bar_rect(&bar(&ed, 1), 22.).3 / 2.,
        14.
    );
    // Unset is not Some(12): 14 px against 16 px.
    assert_ne!(bar_rect(&leaf, 22.).3, h);
    set_layout(&mut ed, r#"{"bar_height":null}"#);
    assert_eq!(bar(&ed, 2), leaf);
    assert_eq!(bar(&ed, 1), sum);
    assert_eq!(bar(&ed, 3), ms);
}

#[test]
fn round_bars_off_draws_start_and_finish_at_their_instants() {
    let mut ed = editor(vec![task(1, 0, 1), task(2, 1, 2), task(3, 0, 2)]);
    let scale = gantt_scale(&ed);
    let whole = bar(&ed, 2);
    set_layout(&mut ed, r#"{"round_bars":false}"#);
    let exact = bar(&ed, 2);
    let t = ed.project().task(2).unwrap();
    let (x, w) = span_x(&exact, scale.day_w);
    let want = |at| instant_x(scale, 0., at);
    assert!((x - want(ed.disp_start(2).unwrap())).abs() < 1e-3);
    assert!((x + w - want(ed.disp_finish(2).unwrap())).abs() < 1e-3);
    assert!(t.duration_min > 0);
    // A one-day task ends before the end of its day: narrower than whole days.
    assert!(w < span_x(&whole, scale.day_w).1);
    // A milestone is centred on its start instant.
    let m = bar(&ed, 3);
    let centre = milestone_left(&m, scale.day_w) + 7.;
    assert!((centre - want(ed.disp_start(3).unwrap())).abs() < 1e-3);
    // Round bars on (or unset) is the whole-day drawing.
    set_layout(&mut ed, r#"{"round_bars":true}"#);
    assert_eq!(bar(&ed, 2), whole);
    set_layout(&mut ed, r#"{"round_bars":null}"#);
    assert_eq!(bar(&ed, 2), whole);
}

#[test]
fn layout_drawn_lists_rects_with_the_row_offset_and_skips_table_rows_for_rollups() {
    let mut p = untitled_project();
    p.tasks = vec![task(1, 0, 1), task(2, 2, 2), task(3, 0, 2), task(4, 1, 1)];
    let mut view = ProjectView::new(p, false);
    let scale = view.scale;
    let set = projcore::json::Json::parse(r#"{"layout":{"bar_height":12,"rollup":true}}"#).unwrap();
    view.ed.edit_formatting(|f| {
        f.apply_view(&set).unwrap();
    });
    let (rects, rolls) = layout_drawn(&view, scale);
    assert_eq!(rects.len(), 4);
    let fields = |s: &str| s.split(':').map(str::to_string).collect::<Vec<_>>();
    // Task 2 is the second drawn row: y = 28 + 4 (a 16 px bar centred on 12).
    let second = fields(&rects[1]);
    assert_eq!(
        (second[0].as_str(), second[2].as_str(), second[4].as_str()),
        ("2", "32", "16")
    );
    // The summary: 16 px bar -> an 11 px summary centred on y 14.
    assert_eq!(
        (fields(&rects[0])[2].as_str(), fields(&rects[0])[4].as_str()),
        ("8.5", "11")
    );
    assert_eq!(rolls, ["1:2"]);
    // A Bar Styles table draws every row itself: no roll-up is reported.
    view.ed
        .edit_formatting(|f| f.set_bars(vec![projcore::formatting::BarRow::default()]));
    let (rects, rolls) = layout_drawn(&view, scale);
    assert_eq!(rects.len(), 4);
    assert!(rolls.is_empty());
}

#[test]
fn overlay_text_takes_the_shapes_text_style() {
    // A base that is neither bold nor italic shows whether a key moved it.
    let base = Font {
        family: "Segoe UI".into(),
        weight: FontWeight::LIGHT,
        style: FontStyle::Oblique,
        ..Default::default()
    };
    let len = "names Ann".len();
    let run = |family: Option<&str>, b, i, u, s| {
        overlay_text_run(&base, len, 0xFF0000, family, b, i, u, s)
    };
    let red = Hsla::from(rgb(0xFF0000));
    // Unset: the overlay's own font, plain, as before #1668.
    let plain = run(None, None, None, None, None);
    assert_eq!(plain.len, len);
    assert_eq!(plain.font, base);
    assert_eq!(plain.color, red);
    assert!(plain.underline.is_none() && plain.strikethrough.is_none());
    // Every key on.
    let on = run(
        Some("Arial"),
        Some(true),
        Some(true),
        Some(true),
        Some(true),
    );
    assert_eq!(on.font.family.as_ref(), "Arial");
    assert_eq!(on.font.weight, FontWeight::BOLD);
    assert_eq!(on.font.style, FontStyle::Italic);
    let under = on.underline.unwrap();
    assert_eq!(
        (under.thickness, under.color, under.wavy),
        (px(1.), Some(red), false)
    );
    let strike = on.strikethrough.unwrap();
    assert_eq!((strike.thickness, strike.color), (px(1.), Some(red)));
    // Each key acts alone: bold and strikethrough leave the slant and the
    // underline as they were, and the mirror.
    let bold_strike = run(None, Some(true), None, None, Some(true));
    assert_eq!(bold_strike.font.weight, FontWeight::BOLD);
    assert_eq!(bold_strike.font.style, base.style);
    assert!(bold_strike.underline.is_none() && bold_strike.strikethrough.is_some());
    let italic_under = run(None, None, Some(true), Some(true), None);
    assert_eq!(italic_under.font.weight, base.weight);
    assert_eq!(italic_under.font.style, FontStyle::Italic);
    assert!(italic_under.underline.is_some() && italic_under.strikethrough.is_none());
    // An explicit off is plain: nothing is inherited.
    let off = run(None, Some(false), Some(false), Some(false), Some(false));
    assert_eq!(off.font.weight, FontWeight::NORMAL);
    assert_eq!(off.font.style, FontStyle::Normal);
    assert_eq!(off.font.family, base.family);
    assert!(off.underline.is_none() && off.strikethrough.is_none());
}

#[test]
fn a_tier_label_fits_at_its_drawn_size_and_weight() {
    use crate::plugin_ui::tier_cell_style;
    let close = |w: f32, want: f32| assert!((w - want).abs() < 1e-4, "{w} != {want}");
    // No plan style: the default 6 px a character on both tiers, which the
    // header row draws bold (#1666).
    close(tier_char_w(&tier_cell_style(None, true)), zoom::CHAR_W);
    close(tier_char_w(&tier_cell_style(None, false)), zoom::CHAR_W);
    // A style that sets neither size nor weight fits as the plain header.
    let colour = PlanStyle {
        color: Some(0xC00000),
        italic: Some(true),
        pattern: Some("light-fill".into()),
        ..PlanStyle::default()
    };
    close(
        tier_char_w(&tier_cell_style(Some(&colour), true)),
        zoom::CHAR_W,
    );
    close(
        tier_char_w(&tier_cell_style(Some(&colour), false)),
        zoom::CHAR_W,
    );
    // 15 pt is 20 px, twice the default text, on either tier; the minor
    // tier's inherited bold fits as an explicit bold does.
    let big = PlanStyle {
        size: Some(15.),
        ..PlanStyle::default()
    };
    let big_bold = PlanStyle {
        bold: Some(true),
        ..big.clone()
    };
    close(
        tier_char_w(&tier_cell_style(Some(&big), true)),
        2. * zoom::CHAR_W,
    );
    close(
        tier_char_w(&tier_cell_style(Some(&big), false)),
        2. * zoom::CHAR_W,
    );
    close(
        tier_char_w(&tier_cell_style(Some(&big_bold), false)),
        2. * zoom::CHAR_W,
    );
    // Only an explicit regular weight is a tenth narrower.
    let thin = PlanStyle {
        bold: Some(false),
        ..PlanStyle::default()
    };
    close(
        tier_char_w(&tier_cell_style(Some(&thin), true)),
        zoom::CHAR_W / 1.1,
    );
}
