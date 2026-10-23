use super::super::*;
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

fn set_layout(ed: &mut ProjectEditor, json: &str) {
    let view = projcore::json::Json::parse(&format!(r#"{{"layout":{json}}}"#)).unwrap();
    ed.edit_formatting(|f| {
        f.apply_view(&view).unwrap();
    });
}

fn rolled(ed: &ProjectEditor, uid: i32) -> Option<Vec<i32>> {
    let t = ed.project().task(uid).unwrap();
    rolled_parts(ed, t, gantt_scale(ed), |b| b).map(|v| v.into_iter().map(|c| c.uid).collect())
}

#[test]
fn rolled_children_follow_rollup_collapse_and_hide_expanded() {
    let mut ed = editor(vec![
        task(1, 0, 1),
        task(2, 2, 2),
        task(3, 0, 2),
        task(4, 0, 2),
        task(5, 3, 3),
        task(6, 1, 1),
    ]);
    // Nested summary 4 holds leaf 5; leaf 6 is outside summary 1.
    assert_eq!(rolled(&ed, 1), None);
    set_layout(&mut ed, r#"{"rollup":true}"#);
    // Expanded, roll-up on: leaves and milestones, nested summary itself
    // never drawn; its leaf is.
    assert_eq!(rolled(&ed, 1), Some(vec![2, 3, 5]));
    assert_eq!(rolled(&ed, 2), None, "a leaf rolls nothing up");
    set_layout(&mut ed, r#"{"hide_rollup_expanded":true}"#);
    assert_eq!(rolled(&ed, 1), None);
    ed.set_collapsed(1, true).unwrap();
    assert_eq!(rolled(&ed, 1), Some(vec![2, 3, 5]));
    ed.set_collapsed(1, false).unwrap();
    set_layout(&mut ed, r#"{"rollup":false}"#);
    assert_eq!(rolled(&ed, 1), None);
    set_layout(&mut ed, r#"{"rollup":null,"hide_rollup_expanded":null}"#);
    assert_eq!(rolled(&ed, 1), None);
}

#[test]
fn rolled_children_skip_inactive_tasks_and_honour_round_bars() {
    let mut inactive = task(3, 2, 2);
    inactive.active = Some(false);
    let mut ed = editor(vec![task(1, 0, 1), task(2, 2, 2), inactive, task(4, 0, 2)]);
    set_layout(&mut ed, r#"{"rollup":true}"#);
    assert_eq!(rolled(&ed, 1), Some(vec![2, 4]));
    let scale = gantt_scale(&ed);
    let parts =
        |ed: &ProjectEditor| rolled_parts(ed, ed.project().task(1).unwrap(), scale, |b| b).unwrap();
    let whole = parts(&ed);
    assert_eq!(whole[0].kind, bar(&ed, 2).kind);
    assert_eq!(whole[1].kind, BarKind::Milestone);
    assert_eq!(whole[0].x, bar_rect(&bar(&ed, 2), scale.day_w).0);
    set_layout(&mut ed, r#"{"round_bars":false}"#);
    let exact = parts(&ed);
    assert!(exact[0].w < whole[0].w);
    // The view's critical toggle recolours a rolled critical child.
    assert_eq!(whole[0].kind, BarKind::Critical);
    let off = rolled_parts(&ed, ed.project().task(1).unwrap(), scale, |b| {
        styled_bar(b, false, true)
    })
    .unwrap();
    assert_eq!(off[0].kind, BarKind::OnTrack);
}

#[test]
fn rolled_children_walk_past_a_blank_row_and_stop_at_the_next_sibling() {
    let blank = Task {
        is_null: true,
        outline_level: 0,
        ..task(5, 0, 0)
    };
    let trailing = Task {
        is_null: true,
        outline_level: 0,
        ..task(8, 0, 0)
    };
    let mut ed = editor(vec![
        task(1, 0, 1),
        task(2, 0, 2),
        task(3, 2, 3),
        task(4, 1, 2),
        blank,
        task(6, 0, 2),
        task(7, 3, 3),
        trailing,
        task(9, 1, 1),
    ]);
    set_layout(&mut ed, r#"{"rollup":true}"#);
    ed.set_collapsed(1, true).unwrap();
    // The blank row between two descendants goes with the subtree; a blank
    // one after the last, and the next level-1 task, do not.
    assert_eq!(rolled(&ed, 1), Some(vec![3, 4, 7]));
}
