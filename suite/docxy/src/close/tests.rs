use super::*;
use core::prelude::v1::test;

fn tab(kind: Kind) -> DocTab {
    let name = match kind {
        Kind::Docx => "basic.docx",
        Kind::Xlsx => "basic.xlsx",
        Kind::Project => "gantt-summary.xml",
        _ => unreachable!(),
    };
    tab_from_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../uiharness/fixtures")
            .join(name),
    )
}

#[test]
fn clean_tabs_never_ask_and_dirty_tabs_honor_each_answer() {
    for kind in [Kind::Docx, Kind::Xlsx, Kind::Project] {
        let mut t = tab(kind);
        assert!(!matches!(t.surface, Surface::Placeholder), "{}", t.status);
        let status = t.status.clone();
        assert_eq!(
            close_step(&mut t, |_| panic!("clean tab asked")),
            CloseStep::Remove
        );
        assert_eq!(t.status, status);
        for (answer, expected) in [
            (CloseAnswer::Save, CloseStep::Save),
            (CloseAnswer::Discard, CloseStep::Remove),
            (CloseAnswer::Cancel, CloseStep::Keep),
        ] {
            let mut t = tab(kind);
            t.dirty = true;
            let status = t.status.clone();
            let mut asked = false;
            assert_eq!(
                close_step(&mut t, |_| {
                    asked = true;
                    Ok(answer)
                }),
                expected
            );
            assert!(asked);
            assert_eq!(t.status, status);
            assert!(t.dirty); // Deciding to save is not a successful save.
        }
        t.dirty = true;
        assert_eq!(
            close_step(&mut t, |_| Err("no dialog".into())),
            CloseStep::Refuse("no dialog".into())
        );
    }
}

#[test]
fn sheet_pending_edit_commits_before_asking_and_is_undoable() {
    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.anchor = (2, 2);
    let before = v.edit_string(0, 0);
    v.redo.push(v.snapshot());
    v.editing = Some("12345".into());
    assert_eq!(
        close_step(&mut t, |t| {
            assert!(t.dirty);
            let Surface::Sheet(v) = &t.surface else {
                panic!()
            };
            assert!(v.editing.is_none());
            assert_eq!(v.edit_string(0, 0), "12345");
            Ok(CloseAnswer::Cancel)
        }),
        CloseStep::Keep
    );
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    assert_eq!(v.undo.len(), 1);
    assert!(v.redo.is_empty());
    assert_eq!(v.anchor, (2, 2));
    let snap = v.undo.pop().unwrap();
    v.restore(snap);
    assert_eq!(v.edit_string(0, 0), before);
    assert!(!v.commit_edit());
    assert!(v.undo.is_empty());
}

#[test]
fn valid_project_buffer_commits_before_asking() {
    let mut t = tab(Kind::Project);
    project_cell_click(&mut t, 1, Some(1), false);
    project_input(&mut t, "text", Some("Pending name"), Modifiers::default());
    assert!(!t.dirty);
    assert_eq!(
        close_step(&mut t, |t| {
            let Surface::Project(v) = &t.surface else {
                panic!()
            };
            assert!(t.dirty);
            assert!(v.cell.is_none());
            assert_eq!(v.ed.project().tasks[1].name, "Pending name");
            Ok(CloseAnswer::Cancel)
        }),
        CloseStep::Keep
    );
}

#[test]
fn invalid_project_buffer_refuses_even_discard_and_correction_clears_status() {
    let mut t = tab(Kind::Project);
    project_cell_click(&mut t, 1, Some(2), false);
    project_input(&mut t, "text", Some("banana"), Modifiers::default());
    let step = close_step(&mut t, |_| panic!("invalid buffer asked"));
    assert_eq!(
        step,
        CloseStep::Refuse("Invalid duration (try 3d, 4h, 2w)".into())
    );
    let Surface::Project(v) = &mut t.surface else {
        panic!()
    };
    let cell = v.cell.as_mut().unwrap();
    assert_eq!(cell.buf, "banana");
    assert_eq!(cell.last_error.as_deref(), Some(t.status.as_ref()));
    assert!(!t.dirty);
    cell.buf = "2d".into();
    assert!(commit_project_cell(&mut t));
    assert_eq!(t.status.as_ref(), "Ready");
    assert!(t.dirty);
}

#[test]
fn header_and_footer_buffers_are_flushed_before_asking() {
    for is_header in [true, false] {
        let mut t = tab(Kind::Docx);
        let part_name = t
            .pkg
            .as_mut()
            .unwrap()
            .create_hf(is_header, "default")
            .unwrap();
        let mut editor = Editor::new(empty_doc());
        editor.insert_str("Pending margin text");
        t.hf_edit = Some(HfEdit {
            editor,
            part_name: part_name.clone(),
            is_header,
            variant: "default",
        });
        t.status = "Editing header — press Esc to return to the document".into();
        assert_eq!(
            close_step(&mut t, |t| {
                assert!(t.dirty);
                assert!(t.hf_edit.is_none());
                assert_eq!(t.status.as_ref(), "Closed header/footer");
                let xml =
                    std::str::from_utf8(t.pkg.as_ref().unwrap().part(&part_name).unwrap()).unwrap();
                assert!(xml.contains("Pending margin text"));
                Ok(CloseAnswer::Cancel)
            }),
            CloseStep::Keep
        );
    }
}

#[test]
fn removal_preserves_existing_active_index_rules_including_last_tab() {
    for (active, remove, expected) in [(1, 0, 0), (1, 1, 1), (1, 2, 1), (2, 2, 1)] {
        let mut tabs = vec![tab(Kind::Docx), tab(Kind::Xlsx), tab(Kind::Project)];
        let mut active = active;
        remove_tab(&mut tabs, &mut active, remove);
        assert_eq!(active, expected);
        assert_eq!(tabs.len(), 2);
    }
    let mut tabs = vec![tab(Kind::Docx)];
    let mut active = 0;
    remove_tab(&mut tabs, &mut active, 0);
    assert_eq!(active, 0);
    assert!(tabs.is_empty());
}
