use super::*;
use core::prelude::v1::test;

fn tab() -> DocTab {
    let p = Project {
        start_date: Some(projcore::editor::default_anchor()),
        tasks: (1..=3)
            .map(|id| Task {
                uid: id * 10,
                id,
                name: format!("Task {id}"),
                duration_min: 480,
                ..Task::default()
            })
            .collect(),
        ..Project::default()
    };
    project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    )
}
fn v(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!()
    };
    v
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!()
    };
    v
}
fn key(t: &mut DocTab, key: &str) {
    if let Some(act) = project_input(t, key, None, Modifiers::default()) {
        apply_project_act(t, act);
    }
}
fn edit(t: &mut DocTab, col: usize, text: &str) {
    vm(t).col = col;
    project_input(t, "text", Some(text), Modifiers::default());
}

#[test]
fn navigation_scrolls_and_all_printable_shortcuts_start_an_edit() {
    let mut t = tab();
    assert_eq!(v(&t).col, 1);
    for c in ["n", "x", "d", "p", "c", "a", "b", "L", "λ"] {
        edit(&mut t, 1, c);
        assert_eq!(v(&t).cell.as_ref().unwrap().buf, c);
        key(&mut t, "escape");
        assert!(!t.dirty);
    }
    key(&mut t, "end");
    assert_eq!(v(&t).ed.sel(), 2);
    key(&mut t, "home");
    assert_eq!(v(&t).ed.sel(), 0);
    for _ in 0..10 {
        key(&mut t, "right");
    }
    assert_eq!(v(&t).col, 6);
    assert!(v(&t).table_x > 0.);
    assert_eq!(v(&t).gantt_x, 0.);
    for _ in 0..10 {
        key(&mut t, "left");
    }
    assert_eq!(v(&t).col, 0);
    assert_eq!(v(&t).table_x, 0.);
    key(&mut t, "f2");
    assert!(v(&t).cell.is_none());
    assert!(t.status.contains("read-only"));
}

#[test]
fn all_columns_commit_as_one_step_and_undo_restores_schedule() {
    for (col, text) in [
        (1, "Renamed"),
        (2, "2d"),
        (3, "2026-01-08"),
        (4, "2026-01-09"),
        (5, "2SS+2h, 3FF-7m"),
        (6, "Alice, Bob"),
    ] {
        let mut t = tab();
        let before = v(&t).ed.project().clone();
        let finish = v(&t).ed.disp_finish(10);
        edit(&mut t, col, text);
        key(&mut t, "enter");
        assert!(v(&t).cell.is_none(), "{col}: {}", t.status);
        assert_eq!(v(&t).ed.sel(), 1);
        assert_eq!(v(&t).ed.undo_depth(), 1);
        assert!(t.dirty);
        let after = v(&t).ed.project().clone();
        let after_finish = v(&t).ed.disp_finish(10);
        apply_project_act(&mut t, ProjectAct::Undo);
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.disp_finish(10), finish);
        apply_project_act(&mut t, ProjectAct::Redo);
        assert_eq!(v(&t).ed.project(), &after);
        assert_eq!(v(&t).ed.disp_finish(10), after_finish);
    }
}

#[test]
fn no_op_date_duration_and_milestone_keep_history_and_redo() {
    let mut t = tab();
    vm(&mut t).ed.set_constraint(10, "MSO 2026-01-08").unwrap();
    vm(&mut t).ed.rename(10, "temporary").unwrap();
    vm(&mut t).ed.undo();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let before = v(&t).ed.project().clone();
    for col in 1..7 {
        vm(&mut t).ed.select(0);
        vm(&mut t).col = col;
        key(&mut t, "f2");
        key(&mut t, "enter");
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!((v(&t).ed.undo_depth(), v(&t).ed.redo_depth()), (1, 1));
        assert!(!t.dirty);
    }
    vm(&mut t).ed.select(0);
    edit(&mut t, 2, "8h");
    key(&mut t, "enter");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    vm(&mut t).ed.select(0);
    edit(&mut t, 2, "0");
    key(&mut t, "enter");
    vm(&mut t).ed.select(0);
    key(&mut t, "f2");
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "0");
}

#[test]
fn invalid_inputs_and_click_away_preserve_everything() {
    for (col, text) in [
        (2, "NaN"),
        (2, "-1d"),
        (2, "1e50d"),
        (3, "2026-02-31"),
        (4, "2026-01-10"),
        (5, "1"),
        (5, "99"),
        (5, "2,2"),
    ] {
        let mut t = tab();
        let before = v(&t).ed.project().clone();
        edit(&mut t, col, text);
        key(&mut t, "tab");
        assert_eq!(v(&t).cell.as_ref().unwrap().buf, text);
        assert_eq!(v(&t).col, col);
        project_cell_click(&mut t, 1, Some(1), true);
        project_cell_click(&mut t, 1, None, false);
        apply_project_act(&mut t, ProjectAct::Rename);
        assert!(!commit_project_cell(&mut t));
        assert!(v(&t).prompt.is_none());
        assert_eq!(v(&t).ed.sel(), 0);
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.undo_depth(), 0);
        assert!(!t.dirty);
        key(&mut t, "escape");
        assert!(v(&t).cell.is_none());
    }
}

#[test]
fn clicking_prompts_and_tab_share_commit_policy() {
    let mut t = tab();
    edit(&mut t, 1, "New");
    project_cell_click(&mut t, 1, Some(2), true);
    assert_eq!(v(&t).ed.project().tasks[0].name, "New");
    assert_eq!(v(&t).cell.as_ref().unwrap().col, 2);
    key(&mut t, "escape");
    edit(&mut t, 1, "Next");
    apply_project_act(&mut t, ProjectAct::Duration);
    assert!(v(&t).cell.is_none() && v(&t).prompt.is_some());
    project_cell_click(&mut t, 0, Some(1), false);
    assert!(v(&t).prompt.is_none());
    key(&mut t, "f2");
    project_input(
        &mut t,
        "tab",
        None,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(v(&t).col, 0);
    assert!(v(&t).cell.is_none());
    key(&mut t, "tab");
    assert_eq!(v(&t).col, 1);
    edit(&mut t, 1, "Saved");
    assert_eq!(
        project_input(
            &mut t,
            "s",
            None,
            Modifiers {
                control: true,
                ..Modifiers::default()
            }
        ),
        Some(ProjectAct::Save)
    );
    assert_eq!(v(&t).ed.project().tasks[0].name, "Saved");
}

#[test]
fn summary_cells_are_read_only() {
    let mut t = tab();
    vm(&mut t).ed.indent(20, 1).unwrap();
    for col in 2..=4 {
        edit(&mut t, col, "1");
        assert!(v(&t).cell.is_none());
        assert!(t.status.contains("read-only"));
    }
}

#[test]
fn save_commits_a_cell_and_invalid_input_never_reaches_disk() {
    let mut t = tab();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/cell-save-{}.yppx", std::process::id()));
    edit(&mut t, 1, "Saved from cell");
    apply_save(&mut t, &path).unwrap();
    assert!(v(&t).cell.is_none());
    assert!(!t.dirty);
    let bytes = std::fs::read(&path).unwrap();
    let saved = project_from_path(&path).unwrap();
    assert_eq!(saved.task(10).unwrap().name, "Saved from cell");
    edit(&mut t, 2, "invalid");
    assert!(apply_save(&mut t, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "invalid");
    assert!(!t.dirty);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn caret_edits_utf8_and_long_buffer_window_tracks_it() {
    let mut c = CellEdit {
        uid: 1,
        col: 1,
        initial: String::new(),
        buf: "aλ🙂z".into(),
        caret: 8,
    };
    c.key("left", None);
    c.key("backspace", None);
    assert_eq!(c.buf, "aλz");
    assert_eq!(c.caret, 3);
    c.key("left", None);
    c.key("delete", None);
    assert_eq!(c.buf, "az");
    c.key("home", None);
    c.key("text", Some("é"));
    assert_eq!(c.buf, "éaz");
    c.key("end", None);
    assert_eq!(c.caret, c.buf.len());
    c.key("text", Some("123456789"));
    let measure = |s: &str| s.chars().count() as f32 * 10.;
    assert_eq!(c.scroll_x(30., measure), 90.);
    c.key("left", None);
    assert_eq!(c.scroll_x(30., measure), 80.);
    c.key("home", None);
    assert_eq!(c.scroll_x(30., measure), 0.);
}
