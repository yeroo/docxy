use super::*;
use core::prelude::v1::test;

fn v(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!("project")
    };
    v
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!("project")
    };
    v
}
/// The issue's plan: unlinked tasks A and B, 2 days each.
fn tab() -> DocTab {
    let mut t = new_project_tab();
    for name in ["A", "B"] {
        vm(&mut t).ed.add_task(None, name, 960, false).unwrap();
    }
    let p = v(&t).ed.project().clone();
    t.surface = Surface::Project(ProjectView::new(p, false));
    t
}
/// Put the cursor on row `row` (the entry row past the last), column `col`.
fn at(t: &mut DocTab, row: usize, col: usize) {
    let v = vm(t);
    if row >= v.ed.project().tasks.len() {
        v.enter_entry_row();
    } else {
        v.entry = false;
        v.ed.select(row);
    }
    v.col = col;
}
fn tasks(t: &DocTab) -> Vec<(i32, String, i64)> {
    v(t).ed
        .project()
        .tasks
        .iter()
        .map(|t| (t.id, t.name.clone(), t.duration_min))
        .collect()
}
fn ctrl(shift: bool, alt: bool) -> Modifiers {
    Modifiers {
        control: true,
        shift,
        alt,
        ..Modifiers::default()
    }
}
fn undo(t: &mut DocTab) {
    apply_project_act(t, ProjectAct::Undo);
}

#[test]
fn ctrl_c_x_v_map_to_the_clipboard_acts() {
    use ProjectAct::*;
    let mut t = tab();
    at(&mut t, 0, COL_NAME);
    for (key, act) in [("c", Copy), ("x", Cut), ("v", Paste)] {
        assert_eq!(key_act(key, ctrl(false, false)), Some(act), "{key}");
        assert_eq!(
            project_input(&mut t, key, None, ctrl(false, false)),
            Some(act)
        );
        for m in [ctrl(true, false), ctrl(false, true), Modifiers::default()] {
            assert_ne!(key_act(key, m), Some(act), "{key} {m:?}");
        }
    }
    // A prompt or an open cell editor keeps the chords.
    vm(&mut t).open_prompt(PromptKind::Move);
    assert_eq!(project_input(&mut t, "c", None, ctrl(false, false)), None);
    vm(&mut t).cancel_prompt();
    vm(&mut t).open_cell(None).unwrap();
    assert_eq!(project_input(&mut t, "v", None, ctrl(false, false)), None);
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "A");
}

#[test]
fn pasting_a_duration_back_keeps_its_unit_and_is_no_edit() {
    let mut t = tab();
    // A manual summary over A, then B, C, D as leaves.
    vm(&mut t).ed.add_task(None, "C", 800, false).unwrap();
    vm(&mut t).ed.add_task(None, "D", 480, false).unwrap();
    vm(&mut t).ed.add_task(None, "E", 0, false).unwrap();
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.set_manual(1, true).unwrap();
    let mut p = v(&t).ed.project().clone();
    for (uid, format) in [(1, 9), (2, 9), (3, 9), (4, 8), (5, 9)] {
        p.tasks
            .iter_mut()
            .find(|t| t.uid == uid)
            .unwrap()
            .duration_format = Some(format);
    }
    t.surface = Surface::Project(ProjectView::new(p, false));
    t.dirty = false;
    let before = v(&t).ed.project().clone();
    // Summary (days text), a weeks leaf, a weeks leaf of 800 min
    // (`0.3333w`), an elapsed-days leaf (days text), a weeks milestone.
    for (row, text) in [(0, "2d"), (1, "0.4w"), (2, "0.3333w"), (3, "1d"), (4, "0w")] {
        at(&mut t, row, COL_DURATION);
        let copied = project_copy_text(v(&t));
        assert_eq!(copied, text, "row {row}");
        paste_project_text(&mut t, &copied);
        assert_eq!(v(&t).ed.project(), &before, "row {row}: {}", t.status);
        assert_eq!(v(&t).ed.undo_depth(), 0, "row {row}");
        assert!(!t.dirty, "row {row}");
    }
    // Ctrl+Delete gives the elapsed leaf a new task's day, in days.
    reset_cell(&mut vm(&mut t).ed, 4, COL_DURATION).unwrap();
    let d = v(&t).ed.project().task(4).unwrap();
    assert_eq!((d.duration_min, d.duration_format), (480, None));
}

#[test]
fn copy_takes_the_cells_edit_text() {
    let mut t = tab();
    vm(&mut t).ed.add_task(None, "Guess", 480, true).unwrap();
    for (row, col, text) in [
        (0, COL_ID, "1"),
        (0, COL_MODE, "Auto Scheduled"),
        (0, COL_NAME, "A"),
        // Exact and re-parseable, not the shown `2 days`.
        (0, COL_DURATION, "2d"),
        (2, COL_DURATION, "1d?"),
        (3, COL_NAME, ""),
    ] {
        at(&mut t, row, col);
        assert_eq!(project_copy_text(v(&t)), text, "row {row} col {col}");
    }
    // An auto summary's rolled-up duration copies as shown.
    vm(&mut t).ed.indent(2, 1).unwrap();
    at(&mut t, 0, COL_DURATION);
    assert_eq!(project_copy_text(v(&t)), "2d");
    // A blank row copies empty but for its ID.
    let row = vm(&mut t).ed.insert_blank_row(None).unwrap();
    for (col, text) in [(COL_ID, "4"), (COL_NAME, ""), (COL_DURATION, "")] {
        at(&mut t, row, col);
        assert_eq!(project_copy_text(v(&t)), text);
    }
}

#[test]
fn the_issue_copy_a_paste_on_b_renames_b_and_keeps_its_duration() {
    let mut t = tab();
    at(&mut t, 0, COL_NAME);
    let text = project_copy_text(v(&t));
    at(&mut t, 1, COL_NAME);
    paste_project_text(&mut t, &text);
    assert_eq!(tasks(&t), [(1, "A".into(), 960), (2, "A".into(), 960)]);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert!(t.dirty);
    assert_eq!(
        (v(&t).ed.sel(), v(&t).col, v(&t).entry),
        (1, COL_NAME, false)
    );
    undo(&mut t);
    assert_eq!(tasks(&t)[1].1, "B");
}

#[test]
fn pasting_on_the_entry_row_makes_a_task_as_typing_does() {
    let mut typed = tab();
    at(&mut typed, 2, COL_NAME);
    project_input(&mut typed, "a", Some("A"), Modifiers::default());
    project_input(&mut typed, "enter", None, Modifiers::default());

    let mut t = tab();
    at(&mut t, 2, COL_NAME);
    paste_project_text(&mut t, "A\r\n");
    assert_eq!(v(&t).ed.project().tasks, v(&typed).ed.project().tasks);
    assert_eq!(tasks(&t)[1], (2, "B".into(), 960));
    assert_eq!(tasks(&t)[2].1, "A");
    // The cursor goes to the new task, not a second entry row.
    assert_eq!((v(&t).entry, v(&t).ed.sel()), (false, 2));
    assert_eq!(v(&t).ed.undo_depth(), 1);
    undo(&mut t);
    assert_eq!(tasks(&t).len(), 2);
}

#[test]
fn a_block_overwrites_from_the_cursor_and_appends_past_the_last_task_as_one_step() {
    let mut t = tab();
    let before = v(&t).ed.project().clone();
    at(&mut t, 0, COL_NAME);
    // Fields past Resource Names are dropped.
    paste_project_text(&mut t, "X\t3d\nY\t4d\nZ\t5d\nW\n\t\t\t\t\t\tdropped");
    assert_eq!(
        tasks(&t),
        [
            (1, "X".into(), 1440),
            (2, "Y".into(), 1920),
            (3, "Z".into(), 2400),
            (4, "W".into(), tasks(&t)[3].2),
        ]
    );
    assert_eq!(v(&t).ed.sel(), 0, "the cursor stays on the target cell");
    let after = v(&t).ed.project().clone();
    assert_eq!(v(&t).ed.undo_depth(), 1);
    undo(&mut t);
    assert_eq!(v(&t).ed.project(), &before);
    apply_project_act(&mut t, ProjectAct::Redo);
    assert_eq!(v(&t).ed.project(), &after);
}

#[test]
fn a_new_task_takes_its_name_first_then_the_lines_other_fields() {
    let mut t = tab();
    at(&mut t, 2, COL_MODE);
    paste_project_text(&mut t, "Manually Scheduled\tC\t3d");
    let task = &v(&t).ed.project().tasks[2];
    assert_eq!(
        (&*task.name, task.manual, task.duration_min),
        ("C", true, 1440)
    );
    assert_eq!(v(&t).ed.undo_depth(), 1);
    undo(&mut t);
    assert_eq!(tasks(&t).len(), 2);
    // Without a name the first field makes it.
    at(&mut t, 2, COL_DURATION);
    paste_project_text(&mut t, "3d");
    assert_eq!(tasks(&t)[2], (3, String::new(), 1440));
}

#[test]
fn paste_skips_rows_a_collapsed_summary_hides() {
    let mut t = tab();
    vm(&mut t).ed.add_task(Some(1), "Sub", 480, false).unwrap();
    vm(&mut t).ed.indent(3, 1).unwrap();
    // A over Sub, then B.
    assert_eq!(vm(&mut t).ed.set_collapsed(1, true), Ok(true));
    at(&mut t, 0, COL_NAME);
    paste_project_text(&mut t, "X\nY");
    let names: Vec<_> = tasks(&t).into_iter().map(|t| t.1).collect();
    assert_eq!(names, ["X", "Sub", "Y"]);
    assert!(v(&t).ed.is_collapsed(1));
}

#[test]
fn a_field_that_cannot_apply_cancels_the_whole_paste() {
    for (row, text, status) in [
        (0, "X\nY\tsoon", "Task 2 Duration: Invalid duration"),
        (0, "X\nY\nZ\tsoon", "New row Duration: Invalid duration"),
    ] {
        let mut t = tab();
        at(&mut t, row, COL_NAME);
        let before = v(&t).ed.project().clone();
        paste_project_text(&mut t, text);
        assert_eq!(v(&t).ed.project(), &before, "{text}");
        assert_eq!(
            (v(&t).ed.undo_depth(), t.dirty, v(&t).ed.sel()),
            (0, false, 0)
        );
        assert!(t.status.starts_with(status), "{}", t.status);
        // The next paste that works clears the error.
        paste_project_text(&mut t, "X");
        assert_eq!(t.status.as_ref(), "Ready");
    }
    // A summary's rolled-up dates.
    let mut t = tab();
    vm(&mut t).ed.indent(2, 1).unwrap();
    at(&mut t, 0, COL_NAME);
    paste_project_text(&mut t, "X\t4d");
    assert!(t.status.starts_with("Task 1 Duration: "), "{}", t.status);
    assert_eq!(tasks(&t)[0].1, "A");
}

#[test]
fn id_fields_are_ignored_and_empty_fields_clear_only_what_delete_clears() {
    let mut t = tab();
    at(&mut t, 0, COL_ID);
    paste_project_text(&mut t, "7\t\tQ\t");
    assert_eq!(tasks(&t)[0], (1, "Q".into(), 960));
    at(&mut t, 0, COL_NAME);
    paste_project_text(&mut t, "\t");
    assert_eq!(
        tasks(&t)[0],
        (1, String::new(), 960),
        "Name clears, Duration stays"
    );

    // Empty lines make no task on the entry row, nor on a blank row.
    let depth = v(&t).ed.undo_depth();
    at(&mut t, 2, COL_NAME);
    paste_project_text(&mut t, "\t\t\n\n");
    assert_eq!((tasks(&t).len(), v(&t).ed.undo_depth()), (2, depth));
    let row = vm(&mut t).ed.insert_blank_row(None).unwrap();
    let depth = v(&t).ed.undo_depth();
    at(&mut t, row, COL_NAME);
    paste_project_text(&mut t, "\t");
    assert!(v(&t).ed.project().tasks[row].is_null);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    // `\t3d` makes the blank row a task by its duration alone.
    paste_project_text(&mut t, "\t3d");
    let task = &v(&t).ed.project().tasks[row];
    assert!(!task.is_null);
    assert_eq!((&*task.name, task.duration_min), ("", 1440));
}

#[test]
fn a_paste_that_changes_nothing_records_nothing() {
    let mut t = tab();
    at(&mut t, 1, COL_NAME);
    paste_project_text(&mut t, "B\t2d");
    assert_eq!((v(&t).ed.undo_depth(), t.dirty), (0, false));
}

#[test]
fn cut_clears_what_delete_clears_and_never_deletes_a_task() {
    let mut t = tab();
    at(&mut t, 0, COL_NAME);
    project_cut(&mut t);
    assert_eq!(tasks(&t)[0].1, "");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    undo(&mut t);
    assert_eq!(tasks(&t)[0].1, "A");
    for (col, status) in [
        (COL_ID, "ID can't be cut"),
        (COL_DURATION, "Duration can't be cut"),
    ] {
        at(&mut t, 0, col);
        project_cut(&mut t);
        assert_eq!(t.status.as_ref(), status);
        assert_eq!(tasks(&t).len(), 2);
        assert_eq!(v(&t).ed.undo_depth(), 0);
    }
}

#[test]
fn tsv_keeps_all_but_one_trailing_line_break() {
    // An empty copied cell is one empty field, not nothing.
    for empty in ["", "\n", "\r\n"] {
        assert_eq!(parse_tsv(empty), [[""]], "{empty:?}");
    }
    assert_eq!(parse_tsv("A\r\n"), [["A"]]);
    assert_eq!(parse_tsv("A\tB\r\nC"), [vec!["A", "B"], vec!["C"]]);
    assert_eq!(parse_tsv("A\n\n"), [["A"], [""]]);
}

#[test]
fn an_empty_copied_cell_pastes_as_an_emptied_cell() {
    for text in ["", "\r\n"] {
        let mut t = tab();
        apply_cell(&mut vm(&mut t).ed, 2, COL_PREDECESSORS, "1").unwrap();
        let depth = v(&t).ed.undo_depth();
        // Copied from A, which has no predecessors.
        at(&mut t, 0, COL_PREDECESSORS);
        assert_eq!(project_copy_text(v(&t)), "");
        at(&mut t, 1, COL_PREDECESSORS);
        t.status = "Earlier error".into();
        paste_project_text(&mut t, text);
        assert!(
            v(&t).ed.project().tasks[1].predecessors.is_empty(),
            "{text:?}"
        );
        assert_eq!(v(&t).ed.undo_depth(), depth + 1);
        assert_eq!(t.status.as_ref(), "Ready");
        // On the entry row it makes no task and records nothing.
        at(&mut t, 2, COL_NAME);
        paste_project_text(&mut t, text);
        assert_eq!((tasks(&t).len(), v(&t).ed.undo_depth()), (2, depth + 1));
    }
}
