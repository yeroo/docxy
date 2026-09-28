use super::*;

fn task(uid: i32, name: &str, level: u32) -> Task {
    Task {
        uid,
        id: uid,
        name: name.into(),
        outline_level: level,
        duration_min: 960,
        ..Task::default()
    }
}

/// Summary S (1) over A (2), then B (3) and C (4).
fn editor() -> Editor {
    Editor::new(Project {
        start_date: Some(default_anchor()),
        tasks: vec![
            task(1, "S", 1),
            task(2, "A", 2),
            task(3, "B", 1),
            task(4, "C", 1),
        ],
        ..Project::default()
    })
}

fn names(ed: &Editor) -> Vec<&str> {
    ed.project().tasks.iter().map(|t| &*t.name).collect()
}

fn dates(ed: &Editor) -> Vec<(i32, Option<DateTime>, Option<DateTime>)> {
    ed.project()
        .tasks
        .iter()
        .map(|t| (t.uid, ed.disp_start(t.uid), ed.disp_finish(t.uid)))
        .collect()
}

fn results(sched: &Schedule) -> Vec<crate::schedule::TaskResult> {
    let mut results: Vec<_> = sched.results().copied().collect();
    results.sort_by_key(|r| r.uid);
    results
}

#[test]
fn a_batch_of_setters_is_one_undo_step() {
    let mut ed = editor();
    ed.rename(4, "Seed").unwrap();
    let before = ed.project().clone();
    ed.batch(|ed| {
        ed.rename(3, "Build")?;
        ed.set_duration(3, "3d")?;
        ed.rename(4, "Ship")
    })
    .unwrap();
    let after = ed.project().clone();
    assert_eq!(names(&ed), ["S", "A", "Build", "Ship"]);
    assert_eq!(ed.project().tasks[2].duration_min, 1440);
    assert_eq!((ed.undo_depth(), ed.redo_depth()), (2, 0));
    assert!(ed.dirty());
    assert_eq!(ed.pushes, 2, "the batch counts as one snapshot");
    assert!(ed.undo());
    assert_eq!(ed.project(), &before);
    assert_eq!((ed.undo_depth(), ed.redo_depth()), (1, 1));
    assert!(ed.redo());
    assert_eq!(ed.project(), &after);
    assert!(ed.pending.is_none(), "redo restores exactly");
}

#[test]
fn a_batch_pairs_the_refresh_with_the_state_before_it() {
    let mut ed = editor();
    let sched = ed.schedule().clone();
    ed.batch(|ed| {
        ed.set_duration(3, "3d")?;
        ed.set_duration(4, "4d")
    })
    .unwrap();
    let pending = ed.pending.as_ref().expect("an edit opened a refresh");
    assert_eq!(
        pending.get(3).unwrap().early_finish,
        sched.get(3).unwrap().early_finish
    );
    assert_eq!(
        pending.get(4).unwrap().early_finish,
        sched.get(4).unwrap().early_finish
    );
}

#[test]
fn a_failed_batch_restores_every_piece_of_state() {
    let mut ed = editor();
    ed.toggle_level();
    ed.rename(4, "Seed").unwrap();
    assert!(ed.undo());
    assert_eq!(ed.set_collapsed(1, true), Ok(true));
    ed.select(2);
    let proj = ed.project().clone();
    let (undo, redo) = (ed.undo.clone(), ed.redo.clone());
    let state = (ed.sel, ed.sel_uid, ed.dirty, ed.pushes, ed.leveled);
    let shown = dates(&ed);
    let err = ed.batch(|ed| {
        ed.rename(3, "Build")?;
        // Indenting the selected row under the collapsed S reveals it.
        ed.indent(3, 1)?;
        assert!(!ed.is_collapsed(1));
        ed.set_duration(4, "soon")
    });
    assert!(err.is_err());
    assert_eq!(ed.project(), &proj);
    assert_eq!((&ed.undo, &ed.redo), (&undo, &redo));
    assert_eq!((ed.sel, ed.sel_uid, ed.dirty, ed.pushes, ed.leveled), state);
    assert!(ed.is_collapsed(1));
    assert_eq!(results(ed.schedule()), results(&schedule(&proj)));
    assert_eq!(dates(&ed), shown);
    assert!(!ed.batching);
}

#[test]
fn a_batch_that_changes_nothing_records_nothing_and_keeps_redo() {
    let mut ed = editor();
    ed.rename(4, "Seed").unwrap();
    assert!(ed.undo());
    ed.mark_saved();
    let proj = ed.project().clone();
    for edit in [
        (|_: &mut Editor| Ok(())) as fn(&mut Editor) -> Result<(), String>,
        |ed| ed.rename(3, "B"),
        |ed| {
            ed.rename(3, "Other")?;
            ed.rename(3, "B")
        },
    ] {
        ed.batch(edit).unwrap();
        assert_eq!(ed.project(), &proj);
        assert_eq!((ed.undo_depth(), ed.redo_depth()), (0, 1));
        assert!(!ed.dirty());
    }
}

#[test]
fn rows_appended_in_a_batch_undo_with_it() {
    let mut ed = editor();
    let before = ed.project().clone();
    ed.batch(|ed| {
        ed.rename(3, "Build")?;
        for name in ["D", "E"] {
            let (_, ()) = ed
                .append_row(|ed, uid| ed.rename(uid, name))?
                .expect("a named row is a task");
            let uid = ed.project().tasks.last().unwrap().uid;
            // A second field on the new task, outside `append_row`.
            ed.set_duration(uid, "3d")?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(names(&ed), ["S", "A", "Build", "C", "D", "E"]);
    assert!(
        ed.project().tasks[4..]
            .iter()
            .all(|t| t.duration_min == 1440)
    );
    assert_eq!(ed.undo_depth(), 1);
    assert!(ed.undo());
    assert_eq!(ed.project(), &before);
}

#[test]
fn a_batch_at_the_undo_cap_keeps_the_cap_and_the_oldest_steps() {
    let mut ed = editor();
    for i in 0..UNDO_CAP {
        ed.rename(4, &format!("C{i}")).unwrap();
    }
    let second = ed.undo[1].clone();
    ed.batch(|ed| {
        for i in 0..UNDO_CAP + 5 {
            ed.rename(3, &format!("B{i}"))?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(ed.undo_depth(), UNDO_CAP);
    assert_eq!(ed.undo[0], second, "one step pushed out the oldest");
    assert!(ed.undo());
    assert_eq!(names(&ed)[2], "B");
}
