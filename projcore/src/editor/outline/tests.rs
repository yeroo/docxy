use super::*;

/// Tasks `(uid, name, level)`; level 0 makes a blank row.
fn outline(rows: &[(i32, &str, u32)]) -> Editor {
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: rows
            .iter()
            .map(|&(uid, name, outline_level)| Task {
                uid,
                id: uid,
                name: name.into(),
                outline_level,
                duration_min: 480,
                is_null: outline_level == 0,
                ..Task::default()
            })
            .collect(),
        ..Project::default()
    })
}

fn hidden(ed: &Editor, index: usize) -> bool {
    !ed.visible_rows().contains(&index)
}

/// A, then S { S1, T { T1 }, blank, S2 }, then B.
fn plan() -> Editor {
    outline(&[
        (1, "A", 1),
        (2, "S", 1),
        (3, "S1", 2),
        (4, "T", 2),
        (5, "T1", 3),
        (6, "", 0),
        (7, "S2", 2),
        (8, "B", 1),
    ])
}

#[test]
fn visible_rows_skip_collapsed_subtrees() {
    let mut ed = plan();
    assert_eq!(ed.visible_rows(), (0..8).collect::<Vec<_>>(), "flat");

    // The blank row after T's last descendant is S's, not T's.
    assert_eq!(ed.set_collapsed(4, true), Ok(true));
    assert_eq!(ed.visible_rows(), [0, 1, 2, 3, 5, 6, 7]);
    assert!(hidden(&ed, 4) && !hidden(&ed, 5));

    // S hides everything under it, the blank row between its children too.
    assert_eq!(ed.set_collapsed(2, true), Ok(true));
    assert_eq!(ed.visible_rows(), [0, 1, 7]);

    // Nested states compose: T is still collapsed inside the expanded S.
    assert_eq!(ed.set_collapsed(2, false), Ok(true));
    assert!(ed.is_collapsed(4));
    assert_eq!(ed.visible_rows(), [0, 1, 2, 3, 5, 6, 7]);
}

#[test]
fn a_trailing_subtree_hides_to_the_end() {
    let mut ed = outline(&[(1, "A", 1), (2, "S", 1), (3, "S1", 2), (4, "S2", 2)]);
    ed.set_collapsed(2, true).unwrap();
    assert_eq!(ed.visible_rows(), [0, 1]);
    assert_eq!(ed.visible_step(1, 1), 1, "the last visible row is the end");
    assert_eq!(ed.visible_step(0, 5), 1);
    assert_eq!(ed.visible_step(1, -1), 0);
    ed.select_last_visible();
    assert_eq!(ed.sel(), 1);
    assert!(
        ed.is_collapsed(2),
        "the entry row's selection keeps it collapsed"
    );
}

#[test]
fn visible_step_moves_over_hidden_rows() {
    let mut ed = plan();
    ed.set_collapsed(2, true).unwrap();
    assert_eq!(ed.visible_step(1, 1), 7);
    assert_eq!(ed.visible_step(7, -1), 1);
    assert_eq!(ed.visible_step(0, -1), 0);
    assert_eq!(outline(&[]).visible_step(0, 1), 0);
}

#[test]
fn only_a_summary_collapses() {
    let mut ed = plan();
    for uid in [1, 3, 6, 99] {
        assert!(ed.set_collapsed(uid, true).is_err(), "uid {uid}");
        assert!(ed.set_collapsed(uid, false).is_err(), "uid {uid}");
        assert!(ed.toggle_collapsed(uid).is_err(), "uid {uid}");
    }
    assert_eq!(ed.visible_rows().len(), 8);
    assert_eq!(ed.set_collapsed(2, false), Ok(false), "already expanded");
    assert_eq!(ed.toggle_collapsed(2), Ok(true));
    assert_eq!(ed.toggle_collapsed(2), Ok(false));
}

#[test]
fn a_task_that_stops_being_a_summary_is_forgotten() {
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "B", 1)]);
    ed.set_collapsed(1, true).unwrap();
    ed.indent(2, -1).unwrap();
    assert!(!ed.is_collapsed(1));
    // A summary again, it starts expanded rather than re-hiding its child.
    ed.indent(2, 1).unwrap();
    assert!(!ed.is_collapsed(1));
    assert_eq!(ed.visible_rows(), [0, 1, 2]);
}

#[test]
fn collapsing_around_the_selection_selects_the_summary() {
    let mut ed = plan();
    ed.select(4);
    ed.set_collapsed(2, true).unwrap();
    assert_eq!((ed.sel(), ed.selected_uid()), (1, Some(2)));
    // A selection outside the subtree stays.
    let mut ed = plan();
    ed.select(7);
    ed.set_collapsed(2, true).unwrap();
    assert_eq!(ed.sel(), 7);
}

#[test]
fn selecting_or_finding_a_hidden_row_expands_its_summaries() {
    let mut ed = plan();
    ed.set_collapsed(4, true).unwrap();
    ed.set_collapsed(2, true).unwrap();
    assert_eq!(ed.find("t1"), FindOutcome::Found(4));
    assert!(!ed.is_collapsed(2) && !ed.is_collapsed(4));
    assert!(!hidden(&ed, 4));

    let mut ed = plan();
    ed.set_collapsed(2, true).unwrap();
    ed.select(2);
    assert_eq!(ed.sel(), 2);
    assert!(!ed.is_collapsed(2));
}

#[test]
fn find_from_top_starts_at_the_first_task() {
    let mut ed = outline(&[(1, "Dig", 1), (2, "Pour", 1), (3, "Dig more", 1)]);
    ed.select(0);
    assert_eq!(ed.find("dig"), FindOutcome::Found(2));
    assert_eq!(ed.find_from_top("dig"), FindOutcome::Found(0));
}

#[test]
fn indenting_the_selected_task_under_a_collapsed_summary_expands_it() {
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "B", 1)]);
    ed.set_collapsed(1, true).unwrap();
    ed.select(2);
    ed.indent(3, 1).unwrap();
    assert_eq!(ed.selected_uid(), Some(3));
    assert!(!ed.is_collapsed(1));
    assert!(!hidden(&ed, 2));
}

#[test]
fn a_clamped_selection_moves_to_its_collapsed_summary() {
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "S2", 2), (4, "C", 1)]);
    ed.set_collapsed(1, true).unwrap();
    ed.select(3);
    ed.delete_task(4).unwrap();
    assert_eq!(ed.selected_uid(), Some(1));
    assert!(ed.is_collapsed(1), "the clamp does not expand the summary");
}

#[test]
fn undo_leaves_a_stale_selection_on_its_collapsed_summary() {
    let mut ed = outline(&[(1, "A", 1), (2, "S", 1), (3, "S1", 2), (4, "S2", 2)]);
    ed.add_task(Some(1), "N", 480, false).unwrap();
    ed.select(2);
    assert_eq!(ed.selected_uid(), Some(2));
    ed.set_collapsed(2, true).unwrap();
    // Undo removes N: index 2 is now S1, hidden under S.
    assert!(ed.undo());
    assert_eq!(ed.selected_uid(), Some(2));
    assert!(ed.is_collapsed(2));
    assert_eq!(ed.visible_rows(), [0, 1]);
}

#[test]
fn collapse_state_is_not_an_edit() {
    let mut ed = plan();
    let before = crate::mspdi::write_mspdi(ed.project());
    ed.set_collapsed(2, true).unwrap();
    assert!(!ed.dirty());
    assert_eq!((ed.undo_depth(), ed.redo_depth()), (0, 0));
    assert_eq!(crate::mspdi::write_mspdi(ed.project()), before);

    // It survives undo and redo of an edit elsewhere.
    ed.rename(1, "Alpha").unwrap();
    assert!(ed.undo());
    assert!(ed.is_collapsed(2));
    assert!(ed.redo());
    assert!(ed.is_collapsed(2));

    ed.replace_project(plan().project().clone());
    assert!(!ed.is_collapsed(2));
    assert_eq!(ed.visible_rows().len(), 8);
}

#[test]
fn typing_below_a_trailing_collapsed_summary_makes_its_sibling() {
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "T", 2), (4, "T1", 3)]);
    ed.set_collapsed(1, true).unwrap();
    let (row, ()) = ed
        .append_row(|ed, uid| ed.rename(uid, "New"))
        .unwrap()
        .unwrap();
    assert_eq!(ed.project().tasks[row].outline_level, 1);
    assert!(ed.is_collapsed(1));
    assert_eq!(ed.visible_rows(), [0, row]);

    // Expanded, the same append joins the last task's level, as before.
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "T", 2), (4, "T1", 3)]);
    let (row, ()) = ed
        .append_row(|ed, uid| ed.rename(uid, "New"))
        .unwrap()
        .unwrap();
    assert_eq!(ed.project().tasks[row].outline_level, 3);
}

#[test]
fn inserting_after_a_collapsed_summary_adds_its_first_child() {
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "B", 1)]);
    ed.set_collapsed(1, true).unwrap();
    ed.select(0);
    let at = ed.add_task(Some(1), "N", 480, false).unwrap();
    assert_eq!((at, ed.project().tasks[at].outline_level), (1, 2));
    // The host selects the new row, which shows it.
    ed.select(at);
    assert!(!ed.is_collapsed(1));
}

#[test]
fn hide_subtasks_on_a_subtask_collapses_its_summary() {
    let mut ed = plan();
    ed.select(4);
    assert_eq!(ed.hide_subtasks(5), Ok(4));
    assert_eq!(ed.selected_uid(), Some(4));
    assert_eq!(ed.hide_subtasks(4), Ok(4), "a summary collapses itself");
    assert_eq!(ed.hide_subtasks(3), Ok(2));
    assert_eq!(ed.selected_uid(), Some(2));
    assert!(
        ed.hide_subtasks(1).is_err(),
        "a top-level task has no summary"
    );
    assert!(ed.hide_subtasks(6).is_err(), "a blank row");
}

#[test]
fn inserting_inside_a_hidden_subtree_keeps_the_outline() {
    // An agent can insert after a hidden task (`task.add --after`).
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "S2", 2), (4, "B", 1)]);
    ed.set_collapsed(1, true).unwrap();
    let at = ed.add_task(Some(2), "N", 480, false).unwrap();
    assert_eq!((at, ed.project().tasks[at].outline_level), (2, 2));
    let s2 = ed.project().tasks.iter().position(|t| t.uid == 3).unwrap();
    assert_eq!(ed.project().tasks[s2].outline_level, 2);
    assert_eq!(ed.subtree_len(1), Ok(3), "S keeps S1, N and S2");
    assert!(ed.is_collapsed(1));
}

#[test]
fn a_hidden_blank_row_becomes_a_task_at_its_expanded_level() {
    // The blank row between S1 and S2 is S's, hidden while S is collapsed.
    let rows = [(1, "S", 1), (2, "S1", 2), (3, "", 0), (4, "S2", 2)];
    let mut expanded = outline(&rows);
    expanded.rename(3, "Filled").unwrap();
    let mut ed = outline(&rows);
    ed.set_collapsed(1, true).unwrap();
    ed.rename(3, "Filled").unwrap();
    assert_eq!(ed.project().tasks[2].outline_level, 2);
    assert_eq!(ed.project(), expanded.project());
}

#[test]
fn inserting_after_the_last_hidden_subtask_keeps_the_outline() {
    let rows = [(1, "S", 1), (2, "S1", 2), (3, "S2", 2), (4, "B", 1)];
    let mut expanded = outline(&rows);
    expanded.add_task(Some(3), "N", 480, false).unwrap();
    let mut ed = outline(&rows);
    ed.set_collapsed(1, true).unwrap();
    let at = ed.add_task(Some(3), "N", 480, false).unwrap();
    assert_eq!((at, ed.project().tasks[at].outline_level), (3, 2));
    assert_eq!(ed.subtree_len(1), Ok(3), "S takes N");
    assert_eq!(ed.project().tasks, expanded.project().tasks);
    assert!(ed.is_collapsed(1));
}

#[test]
fn inserting_after_a_nested_hidden_subtask_keeps_the_outline() {
    // Only T is collapsed; T1 is its last and only child.
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "T", 2), (4, "T1", 3)]);
    ed.set_collapsed(3, true).unwrap();
    let at = ed.add_task(Some(4), "N", 480, false).unwrap();
    assert_eq!(ed.project().tasks[at].outline_level, 3);
    assert!(ed.is_collapsed(3));
}

#[test]
fn adding_after_a_shown_blank_row_below_a_collapsed_summary_makes_its_sibling() {
    // The blank row after S's last subtask is outside S, so it shows.
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "", 0)]);
    ed.set_collapsed(1, true).unwrap();
    assert_eq!(ed.visible_rows(), [0, 2]);
    let at = ed.add_task(Some(3), "N", 480, false).unwrap();
    assert_eq!(ed.project().tasks[at].outline_level, 1);
    // Filling it in makes a sibling too.
    let mut ed = outline(&[(1, "S", 1), (2, "S1", 2), (3, "", 0)]);
    ed.set_collapsed(1, true).unwrap();
    ed.rename(3, "Filled").unwrap();
    assert_eq!(ed.project().tasks[2].outline_level, 1);
    assert!(ed.is_collapsed(1));
}
