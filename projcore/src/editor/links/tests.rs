use super::*;

/// Tasks `(uid, name, level)`; level 0 makes a blank row.
fn project(rows: &[(i32, &str, u32)]) -> Project {
    Project {
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
    }
}

/// `succ` has predecessor `pred`, as read from a file.
fn link(proj: &mut Project, succ: i32, pred: i32) {
    let task = proj.tasks.iter_mut().find(|t| t.uid == succ).unwrap();
    task.predecessors.push(Predecessor::fs(pred));
}

fn preds(ed: &Editor, uid: i32) -> Vec<i32> {
    let task = ed.project().task(uid).unwrap();
    task.predecessors.iter().map(|p| p.uid).collect()
}

fn level(ed: &Editor, uid: i32) -> u32 {
    ed.project().task(uid).unwrap().outline_level
}

fn state(ed: &Editor) -> (Project, usize, usize, bool, usize) {
    (
        ed.project().clone(),
        ed.undo_depth(),
        ed.redo_depth(),
        ed.dirty(),
        ed.sel(),
    )
}

/// Applies `edit`, which must drop `succ`'s link to `pred` while moving
/// `moved` to `level`; one undo brings both back.
fn assert_drops(
    mut proj: Project,
    succ: i32,
    pred: i32,
    moved: i32,
    to: u32,
    edit: impl FnOnce(&mut Editor) -> Result<(), String>,
) {
    link(&mut proj, succ, pred);
    let mut ed = Editor::new(proj);
    let from = level(&ed, moved);
    edit(&mut ed).unwrap();
    assert_eq!(level(&ed, moved), to);
    assert_eq!(preds(&ed, succ), Vec::<i32>::new());
    assert_eq!(ed.undo_depth(), 1);
    assert!(ed.undo());
    assert_eq!((level(&ed, moved), preds(&ed, succ)), (from, vec![pred]));
}

#[test]
fn indent_under_predecessor_drops_the_link() {
    let rows = [(1, "A", 1), (2, "B", 1), (3, "C", 1)];
    assert_drops(project(&rows), 2, 1, 2, 2, |ed| ed.indent(2, 1));
}

#[test]
fn level_patch_under_predecessor_drops_the_link() {
    let rows = [(1, "A", 1), (2, "B", 1)];
    assert_drops(project(&rows), 2, 1, 2, 2, |ed| {
        ed.update_task(
            2,
            TaskPatch {
                level: Some(2),
                ..TaskPatch::default()
            },
        )
    });
}

#[test]
fn indent_under_successor_drops_the_link() {
    // A's predecessor is B, the row below it.
    let rows = [(1, "A", 1), (2, "B", 1)];
    assert_drops(project(&rows), 1, 2, 2, 2, |ed| ed.indent(2, 1));
}

#[test]
fn outdent_that_makes_a_summary_link_drops_it() {
    // S { X, Y }: outdenting X makes Y its subtask.
    let rows = [(1, "S", 1), (2, "X", 2), (3, "Y", 2)];
    assert_drops(project(&rows), 3, 2, 2, 1, |ed| ed.indent(2, -1));
    assert_drops(project(&rows), 2, 3, 2, 1, |ed| ed.indent(2, -1));
}

#[test]
fn outline_edit_that_closes_a_cycle_is_refused() {
    // A -> X -> S: indenting A under S makes X's link to S reach A.
    let mut proj = project(&[(1, "S", 1), (2, "A", 1), (3, "X", 1)]);
    link(&mut proj, 3, 2);
    link(&mut proj, 1, 3);
    let mut ed = Editor::new(proj);
    ed.rename(3, "X'").unwrap();
    ed.select(2);
    let before = state(&ed);
    let err = ed.indent(2, 1).unwrap_err();
    assert!(err.contains("circular relationship"), "{err}");
    assert_eq!(state(&ed), before);
    let err = ed
        .update_task(
            2,
            TaskPatch {
                level: Some(2),
                ..TaskPatch::default()
            },
        )
        .unwrap_err();
    assert!(err.contains("circular relationship"), "{err}");
    assert_eq!(state(&ed), before);
    assert!(ed.undo(), "the earlier rename is still the last step");
}

#[test]
fn a_blank_row_that_joins_the_outline_drops_its_summary_links() {
    // A, then a blank row stored at level 2 (under A once typed into), then C.
    let blank_under_a = || {
        let mut proj = project(&[(1, "A", 1), (2, "", 0), (3, "C", 1)]);
        proj.tasks[1].outline_level = 2;
        proj
    };
    // A's predecessor is the blank row, which #310 keeps.
    assert_drops(blank_under_a(), 1, 2, 2, 2, |ed| ed.rename(2, "X"));
    // The blank row itself stores predecessor A.
    assert_drops(blank_under_a(), 2, 1, 2, 2, |ed| ed.rename(2, "X"));
}

#[test]
fn loaded_summary_links_survive_unrelated_outline_edits() {
    let mut proj = project(&[(1, "S", 1), (2, "A", 2), (3, "T", 1), (4, "U", 1)]);
    link(&mut proj, 2, 1);
    link(&mut proj, 1, 4);
    let mut ed = Editor::new(proj);
    ed.indent(4, 1).unwrap();
    assert_eq!(level(&ed, 4), 2);
    assert_eq!(preds(&ed, 2), [1]);
    assert_eq!(
        preds(&ed, 1),
        [4],
        "a link that was not made a summary link stays"
    );
}

#[test]
fn a_loaded_summary_link_does_not_switch_off_the_cycle_check() {
    // S -> A is a loaded summary link; P, Q, R is the cycle-closing shape.
    let mut proj = project(&[
        (1, "S", 1),
        (2, "A", 2),
        (3, "P", 1),
        (4, "Q", 1),
        (5, "R", 1),
    ]);
    link(&mut proj, 2, 1);
    link(&mut proj, 5, 4);
    link(&mut proj, 3, 5);
    let mut ed = Editor::new(proj);
    let before = state(&ed);
    let err = ed.indent(4, 1).unwrap_err();
    assert!(err.contains("circular relationship"), "{err}");
    assert_eq!(state(&ed), before);
}

#[test]
fn an_already_cyclic_plan_allows_outline_edits() {
    let mut proj = project(&[(1, "A", 1), (2, "B", 1), (3, "C", 1), (4, "D", 1)]);
    link(&mut proj, 1, 2);
    link(&mut proj, 2, 1);
    let mut ed = Editor::new(proj);
    ed.indent(4, 1).unwrap();
    assert_eq!(level(&ed, 4), 2);
    assert_eq!((preds(&ed, 1), preds(&ed, 2)), (vec![2], vec![1]));
}

#[test]
fn outline_cycle_check_is_linear_on_large_summaries() {
    // P { 250 leaves }, A, S { 250 leaves }, Y, Z with P -> S -> A: the
    // summary link P -> S joins 250x250 leaf pairs, which a per-link leaf
    // expansion would walk. Indenting A under P makes it one of P's leaves,
    // so A -> S -> A closes a cycle through that link.
    let mut rows = vec![(1, "P", 1)];
    rows.extend((2..=251).map(|uid| (uid, "P leaf", 2)));
    rows.push((252, "A", 1));
    rows.push((253, "S", 1));
    rows.extend((254..=503).map(|uid| (uid, "S leaf", 2)));
    rows.extend([(505, "Y", 1), (506, "Z", 1)]);
    let mut proj = project(&rows);
    link(&mut proj, 253, 1);
    link(&mut proj, 252, 253);
    assert!(!has_link_cycle(&proj.tasks));
    let mut ed = Editor::new(proj);
    ed.indent(506, 1).unwrap();
    assert_eq!(level(&ed, 506), 2);
    let before = state(&ed);
    let err = ed.indent(252, 1).unwrap_err();
    assert!(err.contains("circular relationship"), "{err}");
    assert_eq!(state(&ed), before);
}

#[test]
fn a_rename_keeps_a_loaded_summary_link() {
    // A loaded cycle-free plan with a link the outline makes a summary link:
    // an edit that leaves the outline as it is does not drop it.
    let mut proj = project(&[(1, "S", 1), (2, "A", 2)]);
    link(&mut proj, 2, 1);
    let mut ed = Editor::new(proj);
    ed.rename(2, "A'").unwrap();
    assert_eq!(preds(&ed, 2), [1]);
}

#[test]
fn cycles_expand_summaries_and_ignore_summary_links_and_blank_rows() {
    let cyclic = |rows: &[(i32, &str, u32)], links: &[(i32, i32)]| {
        let mut proj = project(rows);
        for &(succ, pred) in links {
            link(&mut proj, succ, pred);
        }
        has_link_cycle(&proj.tasks)
    };
    let flat = [(1, "A", 1), (2, "B", 1), (3, "C", 1)];
    assert!(!cyclic(&flat, &[(2, 1), (3, 2)]));
    assert!(cyclic(&flat, &[(2, 1), (3, 2), (1, 3)]));
    // S { A, B } -> C -> A: C follows every leaf of S, A among them.
    let summary = [(1, "S", 1), (2, "A", 2), (3, "B", 2), (4, "C", 1)];
    assert!(cyclic(&summary, &[(4, 1), (2, 4)]));
    assert!(!cyclic(&summary, &[(4, 1), (2, 3)]));
    // A link between a summary and its own subtask is #310's other rule.
    assert!(!cyclic(&summary, &[(2, 1), (1, 3)]));
    // So is a task linked to itself, and a link naming a blank row.
    assert!(!cyclic(&flat, &[(1, 1)]));
    let blank = [(1, "A", 1), (2, "", 0), (3, "C", 1)];
    assert!(!cyclic(&blank, &[(1, 3), (2, 1), (3, 2)]));
}

#[test]
fn outline_extents_match_subtree_end() {
    let proj = project(&[
        (1, "S", 1),
        (2, "", 0),
        (3, "T", 2),
        (4, "T1", 3),
        (5, "", 0),
        (6, "S2", 2),
        (7, "", 0),
        (8, "B", 1),
        (9, "", 0),
    ]);
    let outline = Outline::new(&proj.tasks);
    for i in 0..proj.tasks.len() {
        assert_eq!(
            outline.ends[i],
            crate::editor::outline::subtree_end_in(&proj.tasks, i),
            "row {i}"
        );
    }
    assert_eq!(
        outline.parents,
        [
            None,
            None,
            Some(0),
            Some(2),
            None,
            Some(0),
            None,
            None,
            None
        ]
    );
}
