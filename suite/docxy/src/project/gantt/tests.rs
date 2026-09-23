use super::*;
use core::prelude::v1::test;

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
    p.tasks[0].baseline_start = Some(projcore::DateTime::from_minutes(
        (normal.origin_day - 10) * 1440,
    ));
    let incomplete = ProjectEditor::new(p.clone());
    assert!(bar(&incomplete, 1).baseline.is_none());
    p.tasks[0].baseline_finish = Some(projcore::DateTime::from_minutes(
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
fn date_ticks_are_mondays_and_weekend_columns_are_saturday_sunday() {
    let scale = gantt_scale(&editor(vec![]));
    assert_eq!(
        scale.ticks(0..15),
        vec![(0, "1/5".into()), (7, "1/12".into()), (14, "1/19".into())]
    );
    for day in 0..14 {
        assert_eq!(scale.is_weekend(day), matches!(day % 7, 5 | 6));
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
    v.layout(1180.);
    assert_eq!(v.table_w, 590.);
    assert!(v.key("right", false, false));
    assert_eq!(v.gantt_x, DAY_W);
    assert!(v.key("right", false, true));
    assert_eq!(v.table_x, 80.);
    for _ in 0..500 {
        v.key("right", false, false);
        v.key("right", false, true);
    }
    assert_eq!(v.table_x, TABLE_W - 590.);
    assert_eq!(v.gantt_w, 584.);
    assert_eq!(v.gantt_x, v.scale.width() - 584.);
    v.ed.set_duration(1, "1d").unwrap();
    v.layout(1180.);
    assert_eq!(v.gantt_x, 76.);
    v.layout(2000.);
    assert_eq!((v.table_x, v.gantt_x), (0., 0.));
    for _ in 0..10 {
        v.key("left", false, true);
        v.key("left", false, false);
    }
    assert_eq!((v.table_x, v.gantt_x), (0., 0.));
    for (width, table) in [(460., 320.), (800., 400.), (1180., 590.), (2000., 908.)] {
        assert_eq!(table_pane_width(width), table);
        assert!(width - table >= 140.);
    }
}

#[test]
fn indent_and_outdent_change_geometry_and_undo_redo_restore_it() {
    let mut t = new_project_tab();
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
    v.ed.select(1);
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
        project_region(&v, &probes, harness::Region::Gantt).unwrap(),
        rect(416., 80., 394., 400.)
    );
    assert!(
        project_region(&v, &probes, harness::Region::Bar(1))
            .unwrap_err()
            .contains("row is not rendered")
    );
    assert!(
        project_region(&v, &probes, harness::Region::Bar(99))
            .unwrap_err()
            .contains("no task")
    );
    probes
        .last
        .push(("bar:1".into(), rect(400., 75., 40., 14.)));
    assert_eq!(
        project_region(&v, &probes, harness::Region::Bar(1)).unwrap(),
        rect(416., 80., 24., 9.)
    );
    for r in [
        rect(350., 100., 40., 14.),
        rect(410., 100., 6., 14.), // divider/inset is outside the chart viewport
        rect(420., 60., 40., 14.),
        rect(420., 480., 40., 14.),
        rect(810., 100., 40., 14.),
    ] {
        probes.last[1].1 = r;
        assert!(
            project_region(&v, &probes, harness::Region::Bar(1))
                .unwrap_err()
                .contains("outside")
        );
    }
}

#[test]
fn harness_state_uses_displayed_ids_and_includes_tasks_outside_the_view() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/mspdi/10-summary.xml");
    let t = project_tab_from_path(&path);
    let Surface::Project(v) = t.surface else {
        unreachable!()
    };
    let state = project_state(&v);
    use ctlcore::json::Json;
    assert!(state.contains(&("bar_1".into(), Json::Str("summary 0-1".into()))));
    assert!(state.contains(&("bar_2".into(), Json::Str("critical 0-0".into()))));
    assert!(state.contains(&("bar_3".into(), Json::Str("critical 1-1".into()))));
    let mut p = untitled_project();
    p.tasks = (1..=100).map(|id| task(id, 1, 1)).collect();
    p.tasks[0].id = 200;
    let v = ProjectView::new(p, false);
    let state = project_state(&v);
    assert!(state.contains(&("bar_200".into(), Json::Str("critical 0-0".into()))));
    assert!(state.contains(&("bar_100".into(), Json::Str("critical 0-0".into()))));
}
