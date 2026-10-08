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
    let mut t = new_project_tab("Project1");
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
    // A prompt keeps the chords; an open cell editor passes them to the host
    // (#561), which edits the buffer itself — nothing commits here.
    vm(&mut t).open_prompt(PromptKind::Move);
    assert_eq!(project_input(&mut t, "c", None, ctrl(false, false)), None);
    vm(&mut t).cancel_prompt();
    vm(&mut t).open_cell(None).unwrap();
    assert_eq!(
        project_input(&mut t, "v", None, ctrl(false, false)),
        Some(Paste)
    );
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
    // New tasks are not estimated, so Ctrl+Delete below keeps the estimate:
    // only the reset itself can replace the elapsed format.
    p.new_tasks_estimated = Some(false);
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

// ---- Range copy/cut/paste (#560) --------------------------------------------

fn shift_key(t: &mut DocTab, key_name: &str) {
    let m = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(project_input(t, key_name, None, m), None);
}

/// Select the rectangle from (row 0, `col`) to (`rows`, `to_col`) with
/// Shift+arrows, the cursor landing on the bottom-right cell. Resets the
/// anchor first: `at` alone does not, and a stale one would move the box.
fn select_range(t: &mut DocTab, col: usize, rows: usize, to_col: usize) {
    at(t, 0, col);
    vm(t).anchor = None;
    for _ in 0..rows {
        shift_key(t, "down");
    }
    for _ in col..to_col {
        shift_key(t, "right");
    }
}

#[test]
fn ctrl_c_with_shift_arrows_maps_to_the_range_acts() {
    // The clipboard chords are unchanged and Shift+arrows are not acts; they
    // extend the selection (see ctrl_c_x_v_map_to_the_clipboard_acts for the
    // chord mapping itself).
    let m = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    for key in ["up", "down", "left", "right"] {
        assert_ne!(key_act(key, m), Some(ProjectAct::Copy), "{key}");
        assert_ne!(key_act(key, m), Some(ProjectAct::Cut), "{key}");
        assert_ne!(key_act(key, m), Some(ProjectAct::Paste), "{key}");
    }
    let mut t = tab();
    at(&mut t, 0, COL_NAME);
    shift_key(&mut t, "down");
    shift_key(&mut t, "right");
    assert_eq!(v(&t).selection().unwrap().count(), 4);
}

#[test]
fn copy_a_range_is_tsv_of_edit_texts() {
    let mut t = tab();
    select_range(&mut t, COL_NAME, 1, COL_DURATION);
    assert_eq!(project_copy_text(v(&t)), "A\t2d\nB\t2d\n");
    // One row, several columns: tabs between fields, and the line still
    // ends with \n.
    select_range(&mut t, COL_MODE, 0, COL_DURATION);
    assert_eq!(
        project_copy_text(v(&t)),
        "Auto Scheduled\tA\t2d\n",
        "the cursor's row copies top to bottom, left to right"
    );
}

#[test]
fn copy_a_range_replaces_tabs_and_newlines_inside_a_field() {
    let mut t = tab();
    vm(&mut t).ed.rename(1, "a\tb\rc\nd").unwrap();
    select_range(&mut t, COL_NAME, 0, COL_DURATION);
    assert_eq!(project_copy_text(v(&t)), "a b c d\t2d\n");
    // The single-cell path does not replace them (range-only rule).
    at(&mut t, 0, COL_NAME);
    assert_eq!(project_copy_text(v(&t)), "a\tb\rc\nd");
}

#[test]
fn copy_then_paste_a_range_round_trips() {
    let mut t = tab();
    select_range(&mut t, COL_NAME, 1, COL_DURATION);
    let text = project_copy_text(v(&t));
    assert_eq!(text, "A\t2d\nB\t2d\n");
    at(&mut t, 2, COL_NAME);
    let depth = v(&t).ed.undo_depth();
    paste_project_text(&mut t, &text);
    assert_eq!(
        tasks(&t),
        [
            (1, "A".into(), 960),
            (2, "B".into(), 960),
            (3, "A".into(), 960),
            (4, "B".into(), 960),
        ],
        "the copied values land on the appended tasks"
    );
    assert_eq!(v(&t).ed.undo_depth(), depth + 1, "one undo step");
}

#[test]
fn cut_a_range_clears_only_name_predecessors_resources_in_one_undo_step() {
    let mut t = tab();
    apply_cell(&mut vm(&mut t).ed, 2, COL_PREDECESSORS, "1").unwrap();
    vm(&mut t).ed.assign_resource(2, "Alice").unwrap();
    select_range(&mut t, COL_NAME, 1, COL_RESOURCES);
    let depth = v(&t).ed.undo_depth();
    project_cut(&mut t);
    assert_eq!(
        v(&t).ed.undo_depth(),
        depth + 1,
        "the whole cut is one step"
    );
    let names: Vec<_> = tasks(&t).into_iter().map(|t| t.1).collect();
    assert_eq!(names, ["", ""], "both names cleared");
    for uid in [1, 2] {
        let task = v(&t).ed.project().task(uid).unwrap();
        assert!(task.predecessors.is_empty(), "{uid}: predecessors cleared");
        assert!(
            project_row(&v(&t).ed, task)[COL_RESOURCES].is_empty(),
            "{uid}: resource names cleared"
        );
    }
    // Durations in the range survived.
    assert_eq!(tasks(&t)[0].2, 960);
    apply_project_act(&mut t, ProjectAct::Undo);
    let names: Vec<_> = tasks(&t).into_iter().map(|t| t.1).collect();
    assert_eq!(names, ["A", "B"], "one undo restores every cleared cell");
    assert!(!v(&t).ed.project().task(2).unwrap().predecessors.is_empty());
}

#[test]
fn cut_a_range_with_no_cutable_column_only_copies() {
    let mut t = tab();
    select_range(&mut t, COL_ID, 1, COL_ID);
    let depth = v(&t).ed.undo_depth();
    project_cut(&mut t);
    assert_eq!(
        t.status.as_ref(),
        "Selection copied; its columns can't be cut"
    );
    assert_eq!(v(&t).ed.undo_depth(), depth, "nothing was cleared");
    assert_eq!(tasks(&t)[0].1, "A");
    assert_eq!(sel_count(&t), None, "the cut consumed the range");
}

#[test]
fn cut_a_range_never_deletes_a_task_even_across_the_id_column() {
    let mut t = tab();
    // Whole rows: an ID-press drag over both tasks.
    crate::project_cell_press(&mut t, 0, COL_ID, false);
    crate::project_cell_drag_over(&mut t, 1, COL_ID);
    crate::project_cell_release(&mut t);
    let depth = v(&t).ed.undo_depth();
    project_cut(&mut t);
    assert_eq!(tasks(&t).len(), 2, "both tasks survive their ID cells");
    assert_eq!(tasks(&t)[0].1, "", "Name cleared");
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
}

#[test]
fn cut_a_range_skips_a_blank_row_and_clears_the_rest() {
    let mut t = tab();
    let row = vm(&mut t).ed.insert_blank_row(None).unwrap();
    assert_eq!(row, 2);
    select_range(&mut t, COL_NAME, 2, COL_NAME);
    project_cut(&mut t);
    let task = &v(&t).ed.project().tasks[2];
    assert!(task.is_null, "a blank row stays blank");
    assert_eq!(tasks(&t)[0].1, "", "the named rows cleared");
}

#[test]
fn paste_with_a_range_goes_to_its_top_left_and_clears_it() {
    let mut t = tab();
    // The cursor on the bottom-right: the range's top-left is (0, Name).
    select_range(&mut t, COL_NAME, 1, COL_DURATION);
    paste_project_text(&mut t, "X\t3d");
    assert_eq!(
        tasks(&t),
        [(1, "X".into(), 1440), (2, "B".into(), 960)],
        "one pasted line reached only the top-left cell's row"
    );
    assert_eq!(sel_count(&t), None);
    assert_eq!((v(&t).ed.sel(), v(&t).col), (0, COL_NAME));
}

#[test]
fn paste_with_a_range_pastes_down_and_right_from_its_top_left() {
    let mut t = tab();
    select_range(&mut t, COL_DURATION, 1, COL_DURATION);
    paste_project_text(&mut t, "3d\n4d");
    assert_eq!(tasks(&t), [(1, "A".into(), 1440), (2, "B".into(), 1920)]);
}

fn sel_count(t: &DocTab) -> Option<usize> {
    v(t).selection().map(|s| s.count())
}

#[test]
fn whole_rows_copy_and_paste_back_from_the_id_column() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../uiharness/fixtures/project-clipboard.xml");
    let mut t = crate::project_tab_from_path(&path);
    // Whole rows via an ID press + drag, as the uit does; the copy starts at
    // the ID column, so the paste has to start there too (the ID field is
    // ignored, as on any paste).
    crate::project_cell_press(&mut t, 0, COL_ID, false);
    crate::project_cell_drag_over(&mut t, 1, COL_ID);
    crate::project_cell_release(&mut t);
    let text = {
        let Surface::Project(v) = &t.surface else {
            panic!()
        };
        project_copy_text(v)
    };
    crate::project_cell_press(&mut t, 2, COL_ID, false);
    crate::project_cell_release(&mut t);
    paste_project_text(&mut t, &text);
    assert_eq!(
        tasks(&t),
        [
            (1, "A".into(), 960),
            (2, "B".into(), 960),
            (3, "A".into(), 960),
            (4, "B".into(), 960),
        ],
        "{}",
        t.status
    );
}

// ---- Pasting whole rows inserts tasks (#1100) ----

/// What Ctrl+C on the selection does to the app: the TSV the clipboard gets
/// and the whole-rows clip beside it.
fn copy_rows(t: &DocTab) -> (String, Option<ProjectRowsClip>) {
    let text = project_copy_text(v(t));
    let clip = project_rows_clip(v(t), &text);
    (text, clip)
}
/// Select task rows `from..=to` as dragging down the ID cells does.
fn select_rows(t: &mut DocTab, from: usize, to: usize) {
    project_cell_press(t, from, COL_ID, false);
    project_cell_drag_over(t, to, COL_ID);
}
/// A click elsewhere: the range is gone and the cursor is on the cell.
fn unselect_at(t: &mut DocTab, row: usize, col: usize) {
    vm(t).anchor = None;
    at(t, row, col);
}
fn names(t: &DocTab) -> Vec<String> {
    tasks(t).into_iter().map(|t| t.1).collect()
}

#[test]
fn whole_rows_paste_above_the_cursor_row_as_one_undo_step() {
    let mut t = tab();
    vm(&mut t).ed.assign_resource(1, "Ann").unwrap();
    project_cell_press(&mut t, 0, COL_ID, false);
    let (text, clip) = copy_rows(&t);
    let clip = clip.expect("a row selected by its ID cell is a whole row");
    assert!(clip.live(&text) && clip.live(&text.replace('\n', "\r\n")));
    assert!(!clip.live("something else"));
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    unselect_at(&mut t, 1, COL_DURATION);
    paste_project_rows(&mut t, &clip);
    // Above B: nothing is overwritten.
    assert_eq!(names(&t), ["A", "A", "B"]);
    assert_eq!(v(&t).ed.project().tasks[1].duration_min, 960);
    assert_eq!(v(&t).ed.project().tasks[1].uid, 3);
    assert_eq!(t.status.to_string(), "Pasted 1 row");
    assert_eq!(v(&t).ed.project().assignments.len(), 2);
    assert_eq!(
        (v(&t).ed.sel(), v(&t).entry, v(&t).selection()),
        (1, false, None)
    );
    assert!(t.dirty);
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    undo(&mut t);
    assert_eq!(v(&t).ed.project(), &before);
}

#[test]
fn several_whole_rows_paste_from_the_entry_row_as_appended_tasks() {
    let mut t = tab();
    select_rows(&mut t, 0, 1);
    let (_, clip) = copy_rows(&t);
    unselect_at(&mut t, 2, COL_NAME);
    assert!(v(&t).on_entry_row());
    paste_project_rows(&mut t, &clip.unwrap());
    assert_eq!(names(&t), ["A", "B", "A", "B"]);
    assert_eq!(t.status.to_string(), "Pasted 2 rows");
    assert_eq!((v(&t).entry, v(&t).ed.sel()), (false, 2));
    assert_eq!(v(&t).ed.undo_depth(), 1);
    undo(&mut t);
    assert_eq!(names(&t), ["A", "B"]);
    apply_project_act(&mut t, ProjectAct::Redo);
    assert_eq!(names(&t), ["A", "B", "A", "B"]);
}

#[test]
fn only_a_range_across_every_column_is_a_whole_row_copy() {
    let mut t = tab();
    // A cell, a partial range, and the range up to the last column only.
    unselect_at(&mut t, 0, COL_NAME);
    assert!(copy_rows(&t).1.is_none());
    vm(&mut t).extend_selection("right");
    assert!(v(&t).selection().is_some());
    assert!(copy_rows(&t).1.is_none());
    at(&mut t, 0, COL_ID);
    vm(&mut t).anchor = Some((v(&t).ed.project().tasks[0].uid, COL_ID));
    vm(&mut t).col = COLUMN_COUNT - 1;
    vm(&mut t).ed.select(1);
    assert!(copy_rows(&t).1.is_some(), "ID..last column is whole rows");
    vm(&mut t).col = COLUMN_COUNT - 2;
    assert!(copy_rows(&t).1.is_none());
}

#[test]
fn a_whole_row_copy_keeps_links_inside_the_block_only() {
    let mut t = tab();
    // A, B, C; B links to A, C to B.
    vm(&mut t).ed.add_task(None, "C", 480, false).unwrap();
    vm(&mut t)
        .ed
        .add_link(2, projcore::Predecessor::fs(1))
        .unwrap();
    vm(&mut t)
        .ed
        .add_link(3, projcore::Predecessor::fs(2))
        .unwrap();
    // B and C: B's predecessor A is outside the copy.
    select_rows(&mut t, 1, 2);
    let (_, clip) = copy_rows(&t);
    unselect_at(&mut t, 3, COL_NAME);
    paste_project_rows(&mut t, &clip.unwrap());
    let p = v(&t).ed.project();
    let pasted: Vec<_> = p.tasks[3..]
        .iter()
        .map(|t| {
            (
                t.name.as_str(),
                t.predecessors.iter().map(|p| p.uid).collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(pasted, [("B", vec![]), ("C", vec![p.tasks[3].uid])]);
}

#[test]
fn a_collapsed_summary_copies_with_its_hidden_subtree() {
    let mut t = tab();
    vm(&mut t).ed.add_task(Some(1), "Sub", 480, false).unwrap();
    vm(&mut t).ed.indent(3, 1).unwrap();
    // A over Sub, then B.
    assert_eq!(vm(&mut t).ed.set_collapsed(1, true), Ok(true));
    project_cell_press(&mut t, 0, COL_ID, false);
    let (_, clip) = copy_rows(&t);
    unselect_at(&mut t, 2, COL_NAME);
    paste_project_rows(&mut t, &clip.unwrap());
    assert_eq!(names(&t), ["A", "Sub", "A", "Sub", "B"].map(String::from));
    assert_eq!(t.status.to_string(), "Pasted 2 rows");
}

#[test]
fn another_plans_resources_map_by_name_and_unmatched_assignments_drop() {
    let mut src = tab();
    for name in ["Ann", "Bob"] {
        vm(&mut src).ed.assign_resource(1, name).unwrap();
    }
    project_cell_press(&mut src, 0, COL_ID, false);
    let clip = copy_rows(&src).1.unwrap();
    let mut dst = tab();
    // The target has Bob only, under a uid the source's Bob does not have.
    vm(&mut dst).ed.assign_resource(2, "Bob").unwrap();
    let bob = v(&dst).ed.project().resources[0].uid;
    unselect_at(&mut dst, 1, COL_NAME);
    paste_project_rows(&mut dst, &clip);
    assert_eq!(
        t_status(&dst),
        "Pasted 1 row; 1 resource assignment dropped"
    );
    let p = v(&dst).ed.project();
    let new = p.tasks[1].uid;
    let on_new: Vec<_> = p
        .assignments
        .iter()
        .filter(|a| a.task_uid == new)
        .map(|a| a.resource_uid)
        .collect();
    assert_eq!(on_new, [bob]);
    assert_eq!(p.resources.len(), 1, "no resource is added");
}
fn t_status(t: &DocTab) -> String {
    t.status.to_string()
}

#[test]
fn a_paste_that_cannot_apply_changes_nothing() {
    // A chain of three levels, copied into a plan whose last task is at 19.
    let mut src = tab();
    vm(&mut src).ed.add_task(None, "C", 480, false).unwrap();
    vm(&mut src).ed.indent(2, 1).unwrap();
    vm(&mut src).ed.indent(3, 2).unwrap();
    select_rows(&mut src, 0, 2);
    let clip = copy_rows(&src).1.unwrap();
    let mut dst = tab();
    let mut p = v(&dst).ed.project().clone();
    p.tasks = (1..=19)
        .map(|n| Task {
            uid: n,
            id: n,
            name: format!("T{n}"),
            outline_level: n as u32,
            duration_min: 480,
            ..Task::default()
        })
        .collect();
    dst.surface = Surface::Project(ProjectView::new(p, false));
    unselect_at(&mut dst, 19, COL_NAME);
    let before = v(&dst).ed.project().clone();
    paste_project_rows(&mut dst, &clip);
    assert_eq!(v(&dst).ed.project(), &before);
    assert_eq!(v(&dst).ed.undo_depth(), 0);
    assert_eq!(
        t_status(&dst),
        "Pasted rows would nest deeper than 20 levels"
    );
}

#[test]
fn a_task_calendar_follows_its_name_into_another_plan_never_its_uid() {
    let with_calendars = |mut t: DocTab, calendars: &[(i32, &str)], task_cal: Option<i32>| {
        let mut p = v(&t).ed.project().clone();
        p.calendars = calendars
            .iter()
            .map(|&(uid, name)| projcore::Calendar {
                name: name.into(),
                ..projcore::Calendar::standard(uid)
            })
            .collect();
        p.tasks[0].calendar_uid = task_cal;
        t.surface = Surface::Project(ProjectView::new(p, false));
        t
    };
    let mut src = with_calendars(tab(), &[(7, "Night")], Some(7));
    project_cell_press(&mut src, 0, COL_ID, false);
    let clip = copy_rows(&src).1.unwrap();
    let pasted = |dst: DocTab| {
        let mut dst = dst;
        unselect_at(&mut dst, 1, COL_NAME);
        paste_project_rows(&mut dst, &clip);
        v(&dst).ed.project().tasks[1].calendar_uid
    };
    // The same UID is another calendar here: the task takes the default.
    assert_eq!(pasted(with_calendars(tab(), &[(7, "Day")], None)), None);
    // The same name under another UID.
    assert_eq!(
        pasted(with_calendars(tab(), &[(3, "Day"), (9, "night")], None)),
        Some(9)
    );
    // The same plan keeps it.
    assert_eq!(
        pasted(with_calendars(tab(), &[(7, "Night")], Some(7))),
        Some(7)
    );
}

#[test]
fn an_unassigned_placeholder_assignment_goes_as_it_is() {
    let mut t = tab();
    let mut p = v(&t).ed.project().clone();
    p.assignments.push(projcore::Assignment {
        uid: 1,
        task_uid: p.tasks[0].uid,
        resource_uid: -65535,
        ..projcore::Assignment::default()
    });
    t.surface = Surface::Project(ProjectView::new(p, false));
    project_cell_press(&mut t, 0, COL_ID, false);
    let clip = copy_rows(&t).1.unwrap();
    unselect_at(&mut t, 1, COL_NAME);
    paste_project_rows(&mut t, &clip);
    assert_eq!(t_status(&t), "Pasted 1 row");
    let p = v(&t).ed.project();
    let new = p.tasks[1].uid;
    assert!(
        p.assignments
            .iter()
            .any(|a| a.task_uid == new && a.resource_uid == -65535)
    );
}
