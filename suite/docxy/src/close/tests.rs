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
        assert_eq!(close_step(&mut t, None), CloseStep::Remove);
        assert_eq!(t.status, status);
        for (answer, expected) in [
            (CloseAnswer::Save, CloseStep::Save),
            (CloseAnswer::Discard, CloseStep::Discard),
            (CloseAnswer::Cancel, CloseStep::Keep),
        ] {
            let mut t = tab(kind);
            t.dirty = true;
            let status = t.status.clone();
            assert_eq!(close_step(&mut t, Some(answer)), expected);
            assert_eq!(t.status, status);
            assert!(t.dirty); // Deciding to save is not a successful save.
        }
        t.dirty = true;
        // Not answered yet: the close prompt opens.
        assert_eq!(close_step(&mut t, None), CloseStep::Ask);
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
    v.begin_cell_edit(Some("12345".into()));
    let step = close_step(&mut t, Some(CloseAnswer::Cancel));
    // What it asked about: the pending edit, committed.
    {
        assert!(t.dirty);
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        assert!(v.editing.is_none());
        assert_eq!(v.edit_string(0, 0), "12345");
    }
    assert_eq!(step, CloseStep::Keep);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    assert_eq!(v.undo.len(), 1);
    assert!(v.redo.is_empty());
    assert_eq!(v.anchor, v.sel);
    let snap = v.undo.pop().unwrap();
    v.restore(snap);
    assert_eq!(v.edit_string(0, 0), before);
    assert_eq!(v.anchor, (2, 2));
    assert!(!v.commit_edit());
    assert!(v.undo.is_empty());
}

#[test]
fn valid_project_buffer_commits_before_asking() {
    let mut t = tab(Kind::Project);
    project_cell_click(&mut t, 1, Some(COL_NAME), false);
    project_input(&mut t, "text", Some("Pending name"), Modifiers::default());
    assert!(!t.dirty);
    let step = close_step(&mut t, Some(CloseAnswer::Cancel));
    // What it asked about: the pending edit, committed.
    {
        let Surface::Project(v) = &t.surface else {
            panic!()
        };
        assert!(t.dirty);
        assert!(v.cell.is_none());
        assert_eq!(v.ed.project().tasks[1].name, "Pending name");
    }
    assert_eq!(step, CloseStep::Keep);
}

#[test]
fn invalid_project_buffer_refuses_even_discard_and_correction_clears_status() {
    let mut t = tab(Kind::Project);
    project_cell_click(&mut t, 1, Some(COL_DURATION), false);
    project_input(&mut t, "text", Some("banana"), Modifiers::default());
    let step = close_step(&mut t, None);
    assert_eq!(
        step,
        CloseStep::Refuse("Invalid duration (try 3d, 4h, 2w, 1mo)".into())
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

/// A workbook tab saved at `path` whose A1 editor holds `buffer`, typed.
fn sheet_typing(path: &std::path::Path, buffer: &str) -> DocTab {
    let mut t = tab(Kind::Xlsx);
    t.path = Some(path.to_path_buf());
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.anchor = (0, 0);
    v.begin_cell_edit(Some(buffer.into()));
    t
}

#[test]
fn an_unfinished_formula_refuses_close_and_save_until_corrected() {
    let dir = close_test_dir("unfinished-formula");
    let path = dir.join("book.xlsx");
    let mut t = sheet_typing(&path, "=SUM(A1");
    // Close refuses, with Discard as the answer too: the editor keeps it.
    for answer in [CloseAnswer::Discard, CloseAnswer::Save] {
        let step = close_step(&mut t, Some(answer));
        let CloseStep::Refuse(message) = step else {
            panic!("{answer:?}: {step:?}")
        };
        assert!(message.starts_with("formula error"), "{message}");
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        assert_eq!(v.editing.as_deref(), Some("=SUM(A1"));
    }
    // Save refuses before writing: no file, the tab as it was.
    assert!(!save_sheet_tab(
        &mut t,
        false,
        false,
        |_| panic!("asked"),
        no_macros
    ));
    assert!(!save_sheet_to(&mut t, &path));
    assert!(!path.exists());
    assert!(!t.dirty);
    assert!(t.status.starts_with("formula error"), "{}", t.status);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("=SUM(A1"));
    assert!(v.undo.is_empty());
    // Corrected, it saves; the saved (clean) tab then closes without asking.
    v.editing = Some("=SUM(A1)".into());
    assert!(save_sheet_tab(
        &mut t,
        false,
        false,
        |_| panic!("asked"),
        no_macros
    ));
    assert!(path.is_file(), "{}", t.status);
    assert!(!t.dirty);
    assert_eq!(close_step(&mut t, None), CloseStep::Remove);
    // Corrected in the editor and closed: the close commits it, so the tab
    // is dirty with the formula in A1 when it asks; Discard removes it.
    let mut t = sheet_typing(&path, "=SUM(A1");
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.editing = Some("=SUM(A1)".into());
    let step = close_step(&mut t, Some(CloseAnswer::Discard));
    // What it asked about: the pending edit, committed.
    {
        assert!(t.dirty);
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        assert!(v.editing.is_none());
        let a1 = v.sheet().cell(0, 0).and_then(|c| c.formula.clone());
        assert_eq!(a1.as_deref(), Some("SUM(A1)"));
    }
    assert_eq!(step, CloseStep::Discard);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn header_and_footer_buffers_are_flushed_before_asking() {
    for is_header in [true, false] {
        let mut t = tab(Kind::Docx);
        let part_name = create_hf(&mut t, is_header);
        let mut editor = Editor::new(empty_doc());
        editor.insert_str("Pending margin text");
        t.hf_edit = Some(HfEdit {
            editor,
            part_name: part_name.clone(),
            is_header,
            section: 0,
            variant: HeaderVariant::Default,
            show_text: true,
        });
        t.status = "Editing header — press Esc to return to the document".into();
        let step = close_step(&mut t, Some(CloseAnswer::Cancel));
        // What it asked about: the pending edit, committed.
        {
            assert!(t.dirty);
            assert!(t.hf_edit.is_none());
            assert_eq!(t.status.as_ref(), "Closed header/footer");
            let xml =
                std::str::from_utf8(t.pkg.as_ref().unwrap().part(&part_name).unwrap()).unwrap();
            assert!(xml.contains("Pending margin text"));
        }
        assert_eq!(step, CloseStep::Keep);
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

/// Commit for window close, write every tab's hot-exit sidecar, then restore
/// them as the next launch would.
fn exit_and_restore(tabs: &mut [DocTab], name: &str) -> Vec<DocTab> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/close-tests")
        .join(format!("{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    commit_pending_for_exit(tabs);
    let restored = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| restore_tab(&persist_tab(&dir, i, t)))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    restored
}

fn pending_sheet(text: &str) -> DocTab {
    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.begin_cell_edit(Some(text.into()));
    assert!(!t.dirty);
    t
}

fn sheet_a1(t: &DocTab) -> String {
    let Surface::Sheet(v) = &t.surface else {
        panic!("{}", t.status)
    };
    assert!(v.editing.is_none());
    v.edit_string(0, 0)
}

/// An open header/footer editor holding `text`; returns its part name.
/// Create a default header (or footer) part the way `enter_hf` does: through
/// the body editor, whose final section (with the new reference) Save and the
/// hot-exit sidecar write.
fn create_hf(t: &mut DocTab, is_header: bool) -> String {
    edit_final_sect_pr(t, true, |pkg| pkg.create_hf(is_header, "default"))
        .flatten()
        .unwrap()
}

/// The part a tab's final section references for its default header (or
/// footer), if any.
fn referenced_hf(t: &DocTab, is_header: bool) -> Option<String> {
    hf_part_name_typed(t.pkg.as_ref()?, final_sect_pr(t)?, is_header, "default")
}

fn open_hf(t: &mut DocTab, is_header: bool, text: &str) -> String {
    let part_name = create_hf(t, is_header);
    let mut editor = Editor::new(empty_doc());
    editor.insert_str(text);
    t.hf_edit = Some(HfEdit {
        editor,
        part_name: part_name.clone(),
        is_header,
        section: 0,
        variant: HeaderVariant::Default,
        show_text: true,
    });
    // As the app leaves it: creating the part and typing both mark the tab dirty.
    t.dirty = true;
    part_name
}

fn part_text(t: &DocTab, part_name: &str) -> String {
    String::from_utf8_lossy(t.pkg.as_ref().unwrap().part(part_name).unwrap()).into_owned()
}

#[test]
fn window_close_commits_pending_sheet_edits_on_every_tab() {
    let mut tabs = vec![
        tab(Kind::Docx),
        pending_sheet("Pending active"),
        pending_sheet("Pending inactive"),
    ];
    let restored = exit_and_restore(&mut tabs, "sheet");
    for (i, text) in [(1, "Pending active"), (2, "Pending inactive")] {
        // Committed in the live tab, so the dirty check and a cancelled close see it.
        assert!(tabs[i].dirty);
        assert_eq!(sheet_a1(&tabs[i]), text);
        assert!(restored[i].dirty);
        assert_eq!(sheet_a1(&restored[i]), text);
    }
    assert!(!restored[0].dirty);
}

#[test]
fn window_close_flushes_open_header_and_footer_on_every_tab() {
    let mut tabs = vec![tab(Kind::Xlsx), tab(Kind::Docx), tab(Kind::Docx)];
    let header = open_hf(&mut tabs[1], true, "Pending header");
    let footer = open_hf(&mut tabs[2], false, "Pending footer");
    let restored = exit_and_restore(&mut tabs, "hf");
    for (i, part, text) in [
        (1, &header, "Pending header"),
        (2, &footer, "Pending footer"),
    ] {
        assert!(tabs[i].dirty);
        // Flushed, not exited: a cancelled close stays in header/footer mode.
        assert!(tabs[i].hf_edit.is_some());
        assert!(part_text(&tabs[i], part).contains(text));
        assert!(restored[i].dirty);
        assert!(restored[i].hf_edit.is_none());
        assert!(part_text(&restored[i], part).contains(text), "{i}");
        assert_eq!(
            referenced_hf(&restored[i], i == 1).as_ref(),
            Some(part),
            "{i}"
        );
    }
    assert!(!restored[0].dirty);
}

#[test]
fn window_close_does_not_stop_at_an_invalid_project_buffer() {
    let mut project = tab(Kind::Project);
    project_cell_click(&mut project, 1, Some(COL_DURATION), false);
    project_input(&mut project, "text", Some("banana"), Modifiers::default());
    let mut doc = tab(Kind::Docx);
    let header = open_hf(&mut doc, true, "After invalid");
    let mut tabs = vec![project, pending_sheet("After invalid"), doc];
    let restored = exit_and_restore(&mut tabs, "invalid");
    let Surface::Project(v) = &tabs[0].surface else {
        panic!()
    };
    assert_eq!(v.cell.as_ref().unwrap().buf, "banana");
    assert!(!tabs[0].dirty);
    assert_eq!(sheet_a1(&restored[1]), "After invalid");
    assert!(restored[1].dirty);
    assert!(part_text(&restored[2], &header).contains("After invalid"));
    assert_eq!(referenced_hf(&restored[2], true), Some(header));
    assert!(restored[2].dirty);
}

/// A saved .docx whose header (or footer) was written by another tool (extra
/// namespace, its own whitespace), loaded clean, with the header/footer editor
/// opened on it the way `enter_hf` opens it and nothing typed.
fn untouched_existing_header(name: &str, is_header: bool) -> (DocTab, String, Vec<u8>) {
    let mut source = tab(Kind::Docx);
    // Through the body editor, whose final section Save writes.
    let part_name = edit_final_sect_pr(&mut source, false, |pkg| {
        pkg.create_hf(is_header, "default")
    })
    .flatten()
    .unwrap();
    let pkg = source.pkg.as_mut().unwrap();
    let word_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
        <w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
        xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">\r\n  \
        <w:p><w:r><w:t>Existing header</w:t></w:r></w:p>\r\n</w:hdr>"
        .replace("w:hdr", if is_header { "w:hdr" } else { "w:ftr" });
    assert!(pkg.set_part(&part_name, word_xml.as_bytes().to_vec()));
    let Surface::Doc(ed) = &source.surface else {
        panic!()
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/close-tests")
        .join(format!("{}-{name}-source", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("with-header.docx");
    std::fs::write(
        &path,
        doc_to_docx(&ed.doc, &source.comments, source.pkg.as_ref()),
    )
    .unwrap();
    let mut t = tab_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!t.dirty, "{}", t.status);
    let pkg = t.pkg.as_ref().unwrap();
    let part_name = hf_part_name_typed(pkg, pkg.sect_pr(), is_header, "default").unwrap();
    let before = pkg.part(&part_name).unwrap().to_vec();
    assert_eq!(before, word_xml.as_bytes());
    let body = parse_hf_part(pkg, &part_name);
    t.hf_edit = Some(HfEdit {
        editor: Editor::new(docxcore::model::Document { body }),
        part_name: part_name.clone(),
        is_header,
        section: 0,
        variant: HeaderVariant::Default,
        show_text: true,
    });
    (t, part_name, before)
}

#[test]
fn window_close_leaves_an_untouched_header_editor_clean_and_its_part_intact() {
    let (first, part_name, before) = untouched_existing_header("untouched-0", true);
    let (last, _, _) = untouched_existing_header("untouched-2", true);
    let mut tabs = vec![first, pending_sheet("Beside"), last];
    let restored = exit_and_restore(&mut tabs, "untouched");
    for i in [0, 2] {
        assert!(!tabs[i].dirty, "{i}");
        assert!(tabs[i].hf_edit.is_some());
        assert_eq!(
            tabs[i].pkg.as_ref().unwrap().part(&part_name).unwrap(),
            &before[..]
        );
        assert!(!restored[i].dirty, "{i}");
        assert_eq!(
            restored[i].pkg.as_ref().unwrap().part(&part_name).unwrap(),
            &before[..],
            "{i}"
        );
    }
    assert!(restored[1].dirty);
}

/// A sheet whose A1 holds the text "007", with the cell editor opened on it
/// the way double-click or F2 opens it, and nothing typed.
fn untouched_text_cell() -> DocTab {
    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    let active = v.active;
    v.engine.set_cell(
        &mut v.pkg.workbook,
        (active, 0, 0),
        gridcore::sheet::Cell::text("007"),
    );
    v.begin_cell_edit(None);
    assert_eq!(v.editing.as_deref(), Some("007"));
    t
}

fn a1_is_text_007(t: &DocTab) {
    let Surface::Sheet(v) = &t.surface else {
        panic!("{}", t.status)
    };
    assert_eq!(
        v.sheet().cell(0, 0).map(|c| &c.value),
        Some(&gridcore::sheet::CellValue::Text("007".into()))
    );
}

#[test]
fn untouched_commit_keeps_every_cell_kind() {
    use gridcore::sheet::{Cell, CellValue};

    let cells = [
        Cell::text("007"),
        Cell::text("TRUE"),
        Cell::text(" 5 "),
        Cell::text("=x"),
        Cell {
            value: CellValue::Error("#DIV/0!".into()),
            ..Cell::default()
        },
        Cell::formula("1+1"),
        Cell::number(7.0),
        Cell {
            value: CellValue::Bool(true),
            ..Cell::default()
        },
        Cell::default(),
    ];
    for cell in cells {
        let mut t = tab(Kind::Xlsx);
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (0, 0);
        v.engine
            .set_cell(&mut v.pkg.workbook, (v.active, 0, 0), cell.clone());
        let before = v.sheet().cell(0, 0).cloned();
        v.begin_cell_edit(None);
        v.redo.push(v.snapshot());

        assert!(!v.commit_edit(), "{cell:?}");
        assert!(v.editing.is_none(), "{cell:?}");
        assert!(v.undo.is_empty(), "{cell:?}");
        assert_eq!(v.redo.len(), 1, "{cell:?}");
        assert_eq!(v.sheet().cell(0, 0), before.as_ref(), "{cell:?}");
    }
}

#[test]
fn changed_commit_still_parses_and_records_undo() {
    use gridcore::sheet::{Cell, CellValue};

    for (buffer, expected) in [
        ("0070", CellValue::Number(70.0)),
        ("007 ", CellValue::Number(7.0)),
    ] {
        let mut t = untouched_text_cell();
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.redo.push(v.snapshot());
        v.editing = Some(buffer.into());

        assert!(v.commit_edit(), "{buffer:?}");
        assert!(v.editing.is_none());
        assert_eq!(v.undo.len(), 1);
        assert!(v.redo.is_empty());
        assert_eq!(v.sheet().cell(0, 0).map(|c| &c.value), Some(&expected));
        assert_eq!(
            v.undo[0].workbook().sheets[v.active].cell(0, 0),
            Some(&Cell::text("007"))
        );
    }
}

#[test]
fn fresh_entry_equal_to_stored_text_still_commits() {
    use gridcore::sheet::{Cell, CellValue};

    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.engine
        .set_cell(&mut v.pkg.workbook, (v.active, 0, 0), Cell::text("5"));
    v.begin_cell_edit(Some(String::new()));
    v.editing = Some("5".into());

    assert!(v.commit_edit());
    assert_eq!(v.undo.len(), 1);
    assert_eq!(
        v.sheet().cell(0, 0).map(|c| &c.value),
        Some(&CellValue::Number(5.0))
    );
}

#[test]
fn close_commits_fresh_entry_equal_to_stored_text() {
    use gridcore::sheet::{Cell, CellValue};

    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.engine
        .set_cell(&mut v.pkg.workbook, (v.active, 0, 0), Cell::text("5"));
    v.begin_cell_edit(Some(String::new()));
    v.editing = Some("5".into());

    let step = close_step(&mut t, Some(CloseAnswer::Cancel));
    // What it asked about: the pending edit, committed.
    {
        assert!(t.dirty);
    }
    assert_eq!(step, CloseStep::Keep);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.undo.len(), 1);
    assert_eq!(
        v.sheet().cell(0, 0).map(|c| &c.value),
        Some(&CellValue::Number(5.0))
    );
}

#[test]
fn changed_editor_commits_to_its_origin_after_selection_moves() {
    use gridcore::sheet::{Cell, CellValue};

    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.engine
        .set_cell(&mut v.pkg.workbook, (v.active, 0, 0), Cell::text("007"));
    let b2_before = v.sheet().cell(1, 1).cloned();
    v.begin_cell_edit(None);
    v.editing = Some("008".into());
    v.sel = (1, 1);

    assert!(v.commit_edit());
    assert_eq!(
        v.sheet().cell(0, 0).map(|c| &c.value),
        Some(&CellValue::Number(8.0))
    );
    assert_eq!(v.sheet().cell(1, 1), b2_before.as_ref());
}

#[test]
fn untouched_editor_stays_unchanged_after_selection_moves() {
    let mut t = untouched_text_cell();
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    let a1_before = v.sheet().cell(0, 0).cloned();
    let b2_before = v.sheet().cell(1, 1).cloned();
    v.sel = (1, 1);

    assert!(!v.commit_edit());
    assert!(v.undo.is_empty());
    assert_eq!(v.sheet().cell(0, 0), a1_before.as_ref());
    assert_eq!(v.sheet().cell(1, 1), b2_before.as_ref());
}

#[test]
fn ending_editor_clears_its_seed_and_origin() {
    let mut t = untouched_text_cell();
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    assert!(v.edit_seed.is_some());
    assert!(v.edit_origin.is_some());
    v.end_cell_edit();
    assert!(v.editing.is_none());
    assert!(v.edit_seed.is_none());
    assert!(v.edit_origin.is_none());
    v.begin_cell_edit(Some(String::new()));
    assert!(v.edit_seed.is_none());
    assert_eq!(v.edit_origin, Some((v.active, 0, 0)));
}

#[test]
fn structural_edit_commits_the_open_editor_before_shifting_cells() {
    use gridcore::sheet::CellValue;

    for (op, moved_to) in [(StructOp::InsertRow, (2, 1)), (StructOp::InsertCol, (1, 2))] {
        let mut t = tab(Kind::Xlsx);
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (1, 1); // B2, which holds the number 10 in basic.xlsx.
        v.begin_cell_edit(None);
        v.editing = Some("42".into());

        v.structural_edit(op);
        assert!(v.editing.is_none());
        assert_eq!(v.undo.len(), 2);
        assert_eq!(
            v.sheet().cell(moved_to.0, moved_to.1).map(|c| &c.value),
            Some(&CellValue::Number(42.0))
        );
        assert!(!v.commit_edit());
        assert_ne!(
            v.sheet().cell(1, 1).map(|c| &c.value),
            Some(&CellValue::Number(42.0))
        );
    }
}

#[test]
fn sort_commits_an_open_editor_before_moving_its_row() {
    use gridcore::edit::{SortLevel, SortOn, SortOptions};
    use gridcore::sheet::CellValue;

    // Sort A to Z from the cursor (the list around it), and a sort of the
    // range A2:B5 given outright.
    for field in [None, Some((1, 0, 4, 1))] {
        let mut t = tab(Kind::Xlsx);
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (2, 0); // A3 is South; B3 is 20.
        v.anchor = v.sel;
        v.begin_cell_edit(None);
        v.editing = Some("Zzz".into());
        match field {
            None => crate::sheet_sort::quick(&mut t, true).unwrap(),
            Some(area) => {
                let levels = [SortLevel {
                    key: 0,
                    on: SortOn::Value {
                        asc: true,
                        list: None,
                    },
                }];
                crate::sheet_sort::run(&mut t, area, &levels, &SortOptions::default()).unwrap()
            }
        }
        assert!(t.dirty);
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        assert!(v.editing.is_none());
        assert_eq!(v.undo.len(), 2);
        for (row, name, number) in [
            (1, "East", 30.0),
            (2, "North", 10.0),
            (3, "West", 40.0),
            (4, "Zzz", 20.0),
        ] {
            assert_eq!(
                v.sheet().cell(row, 0).map(|c| &c.value),
                Some(&CellValue::Text(name.into())),
                "field={field:?}, row={row}"
            );
            assert_eq!(
                v.sheet().cell(row, 1).map(|c| &c.value),
                Some(&CellValue::Number(number)),
                "field={field:?}, row={row}"
            );
        }
    }
}

#[test]
fn unsortable_region_still_closes_an_untouched_editor() {
    let mut t = tab(Kind::Xlsx);
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (98, 25); // Z99 is empty, outside the used region.
    v.begin_cell_edit(None);

    assert!(crate::sheet_sort::quick(&mut t, true).is_err());
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert!(v.editing.is_none());
    assert!(v.undo.is_empty());
    assert!(!t.dirty);
}

#[test]
fn window_close_leaves_an_untouched_cell_editor_open_and_the_cell_intact() {
    let mut tabs = vec![
        untouched_text_cell(),
        tab(Kind::Docx),
        untouched_text_cell(),
    ];
    let restored = exit_and_restore(&mut tabs, "untouched-cell");
    for i in [0, 2] {
        assert!(!tabs[i].dirty, "{i}");
        let Surface::Sheet(v) = &tabs[i].surface else {
            panic!()
        };
        assert_eq!(v.editing.as_deref(), Some("007"));
        assert!(v.undo.is_empty());
        a1_is_text_007(&tabs[i]);
        assert!(!restored[i].dirty, "{i}");
        a1_is_text_007(&restored[i]);
    }
}

#[test]
fn single_close_of_an_untouched_header_editor_removes_without_asking_and_keeps_the_part() {
    for is_header in [true, false] {
        let (mut t, part_name, before) =
            untouched_existing_header(&format!("single-{is_header}"), is_header);
        assert_eq!(close_step(&mut t, None), CloseStep::Remove);
        assert!(!t.dirty, "{is_header}");
        assert!(t.hf_edit.is_none());
        assert_eq!(
            t.pkg.as_ref().unwrap().part(&part_name).unwrap(),
            &before[..],
            "{is_header}"
        );
    }
}

#[test]
fn exiting_or_saving_an_untouched_header_editor_keeps_the_tab_clean_and_the_part_intact() {
    for is_header in [true, false] {
        let (mut t, part_name, before) =
            untouched_existing_header(&format!("exit-{is_header}"), is_header);
        // The pre-save flush (Save, switching regions) leaves the session open.
        flush_hf_tab(&mut t);
        assert!(!t.dirty, "{is_header}");
        assert!(t.hf_edit.is_some());
        assert_eq!(
            t.pkg.as_ref().unwrap().part(&part_name).unwrap(),
            &before[..]
        );
        // Esc / Close Header and Footer.
        exit_hf_tab(&mut t);
        assert!(!t.dirty, "{is_header}");
        assert!(t.hf_edit.is_none());
        assert_eq!(t.status.as_ref(), "Closed header/footer");
        assert_eq!(
            t.pkg.as_ref().unwrap().part(&part_name).unwrap(),
            &before[..]
        );
    }
}

#[test]
fn single_close_of_an_untouched_cell_editor_removes_without_asking_and_keeps_the_cell() {
    let mut t = untouched_text_cell();
    assert_eq!(close_step(&mut t, None), CloseStep::Remove);
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert!(v.undo.is_empty());
    a1_is_text_007(&t);
}

#[test]
fn a_cancelled_single_close_keeps_an_untouched_cell_editor_open_as_seeded() {
    let mut t = untouched_text_cell();
    // Dirty for another reason, so the close asks.
    t.dirty = true;
    assert_eq!(
        close_step(&mut t, Some(CloseAnswer::Cancel)),
        CloseStep::Keep
    );
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("007"));
    assert!(v.undo.is_empty());
    a1_is_text_007(&t);
}

#[test]
fn close_dialog_save_preserves_an_untouched_text_cell_on_disk() {
    let dir = close_test_dir("untouched-cell-close-save");
    let path = dir.join("saved.xlsx");
    let mut t = untouched_text_cell();
    t.path = Some(path.clone());
    t.dirty = true; // Another change caused the close dialog to ask.
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.anchor = (2, 2);
    assert_eq!(close_step(&mut t, Some(CloseAnswer::Save)), CloseStep::Save);
    assert!(save_sheet_tab(
        &mut t,
        false,
        false,
        |_| panic!("in-place save asked"),
        no_macros
    ));
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("007"));
    assert!(v.undo.is_empty());
    assert_eq!(v.anchor, (2, 2));
    assert!(!t.dirty, "{}", t.status);
    assert!(path.is_file(), "{}", t.status);
    a1_is_text_007(&tab_from_path(&path));
}

#[test]
fn close_dialog_save_collapses_a_range_when_it_commits_a_changed_cell() {
    let mut t = untouched_text_cell();
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.anchor = (2, 2);
    v.begin_cell_edit(Some("abc".into()));
    assert_eq!(close_step(&mut t, Some(CloseAnswer::Save)), CloseStep::Save);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.anchor, v.sel);
    assert!(v.editing.is_none());
    assert_eq!(v.undo.len(), 1);
}

#[test]
fn save_preserves_an_untouched_cell_editor_and_clean_tab() {
    let dir = close_test_dir("untouched-cell-save");
    let path = dir.join("saved.xlsx");
    let mut t = untouched_text_cell();
    t.path = Some(path.clone());
    assert!(save_sheet_tab(
        &mut t,
        false,
        false,
        |_| panic!("in-place save asked"),
        no_macros
    ));
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("007"));
    assert!(v.undo.is_empty());
    a1_is_text_007(&t);
    assert!(path.is_file(), "{}", t.status);
    a1_is_text_007(&tab_from_path(&path));
}

#[test]
fn cancelled_save_as_keeps_an_untouched_cell_editor_open() {
    let mut t = untouched_text_cell();
    assert!(save_sheet_tab(&mut t, false, true, |_| None, no_macros));
    assert_eq!(t.status.as_ref(), "save cancelled");
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("007"));
    assert!(v.undo.is_empty());
    a1_is_text_007(&t);
}

#[test]
fn save_as_uses_the_picker_and_preserves_an_untouched_text_cell() {
    let dir = close_test_dir("untouched-cell-save-as");
    let path = dir.join("picked.xlsx");
    let mut t = untouched_text_cell();
    assert!(save_sheet_tab(
        &mut t,
        false,
        true,
        |suggested| {
            assert_eq!(suggested, "basic.xlsx");
            Some(path.clone())
        },
        no_macros
    ));
    assert_eq!(t.path.as_deref(), Some(path.as_path()));
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("007"));
    assert!(v.undo.is_empty());
    a1_is_text_007(&tab_from_path(&path));
}

#[test]
fn save_commits_a_changed_cell_editor_before_writing() {
    let dir = close_test_dir("changed-cell-save");
    for (buffer, expected) in [
        ("abc", gridcore::sheet::CellValue::Text("abc".into())),
        ("008", gridcore::sheet::CellValue::Number(8.0)),
    ] {
        let path = dir.join(format!("{buffer}.xlsx"));
        let mut t = untouched_text_cell();
        t.path = Some(path.clone());
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.anchor = (2, 2);
        v.begin_cell_edit(Some(buffer.into()));
        assert!(save_sheet_tab(
            &mut t,
            false,
            false,
            |_| panic!("in-place save asked"),
            no_macros
        ));
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        assert!(v.editing.is_none(), "{buffer}");
        assert_eq!(v.undo.len(), 1, "{buffer}");
        assert_eq!(v.anchor, v.sel, "{buffer}");
        assert!(!t.dirty, "{}", t.status);
        assert!(path.is_file(), "{}", t.status);
        let reloaded = tab_from_path(&path);
        let Surface::Sheet(v) = &reloaded.surface else {
            panic!("{}", reloaded.status)
        };
        assert_eq!(
            v.sheet().cell(0, 0).map(|c| &c.value),
            Some(&expected),
            "{buffer}"
        );
    }
}

#[test]
fn preparing_a_changed_cell_for_save_marks_dirty_and_collapses_selection() {
    let mut t = untouched_text_cell();
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.anchor = (2, 2);
    v.begin_cell_edit(Some("abc".into()));
    prepare_sheet_save(&mut t).unwrap();
    assert!(t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.undo.len(), 1);
    assert!(v.editing.is_none());
    assert_eq!(v.anchor, v.sel);
}

#[test]
fn a_tab_that_failed_to_load_stays_unsaveable_after_a_restart() {
    let dir = close_test_dir("load-failed-source");
    let path = dir.join("broken.docx");
    std::fs::write(&path, b"not a zip").unwrap();
    for dirty in [false, true] {
        let mut t = tab_from_path(&path);
        assert!(t.load_failed, "{}", t.status);
        t.dirty = dirty;
        let mut restored = exit_and_restore(&mut [t], &format!("load-failed-{dirty}"));
        let r = &mut restored[0];
        assert!(r.load_failed, "dirty={dirty}: {}", r.status);
        assert!(r.status.starts_with("load error"), "{}", r.status);
        assert_eq!(r.dirty, dirty);
        r.dirty = true;
        assert!(!save_doc_tab(r, None));
        assert_eq!(r.status.as_ref(), DOC_LOAD_FAILED_SAVE);
        assert_eq!(std::fs::read(&path).unwrap(), b"not a zip");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_restore_without_a_sidecar_takes_the_fresh_load_s_mark() {
    let dir = close_test_dir("load-failed-reload");
    let good = dir.join("repaired.docx");
    basic_docx_at(&good);
    let broken = dir.join("broken.docx");
    std::fs::write(&broken, b"not a zip").unwrap();
    let persisted = |path: &std::path::Path| PersistTab {
        kind: Kind::Docx,
        title: file_name(path),
        path: Some(path.display().to_string()),
        dirty: false,
        hot: None,
        unreadable: Vec::new(),
        markdown: false,
        load_failed: Some(true),
        read_only: false,
        protected: false,
        repaired: false,
        stamp: None,
        converted: None,
        binary_source: false,
        compat: false,
    };
    // Repaired since the session was saved: a normal, saveable document.
    let mut t = restore_tab(&persisted(&good));
    assert!(!t.load_failed, "{}", t.status);
    t.dirty = true;
    assert!(save_doc_tab(&mut t, None), "{}", t.status);
    // Still broken: marked by the fresh load itself.
    assert!(restore_tab(&persisted(&broken)).load_failed);
    // A session written before the mark existed still reads, unmarked.
    let old: PersistTab = serde_json::from_str(&format!(
        r#"{{"kind":"Docx","title":"repaired.docx","path":{:?}}}"#,
        good.display().to_string()
    ))
    .unwrap();
    assert_eq!(old.load_failed, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_session_from_before_the_mark_asks_the_file_whether_it_loads() {
    let dir = close_test_dir("load-failed-old-session");
    let broken = dir.join("broken.docx");
    std::fs::write(&broken, b"not a zip").unwrap();
    let good = dir.join("good.docx");
    basic_docx_at(&good);
    for (i, (path, failed)) in [(&broken, true), (&good, false)].into_iter().enumerate() {
        let mut t = tab_from_path(path);
        t.dirty = true;
        // Written as a session from before #209: sidecar present, no mark.
        let mut p = persist_tab(&dir, i, &t);
        assert!(p.hot.is_some());
        p.load_failed = None;
        let mut r = restore_tab(&p);
        assert_eq!(r.load_failed, failed, "{}: {}", path.display(), r.status);
        assert_eq!(r.status.starts_with("load error"), failed, "{}", r.status);
        assert_eq!(save_doc_tab(&mut r, None), !failed, "{}", r.status);
    }
    assert_eq!(std::fs::read(&broken).unwrap(), b"not a zip");
    // A good tab whose file has since gone missing keeps its content and
    // saves it back; a missing file has nothing to lose.
    let gone = dir.join("gone.docx");
    basic_docx_at(&gone);
    let mut t = tab_from_path(&gone);
    t.dirty = true;
    let text = doc_text(&t);
    let mut p = persist_tab(&dir, 5, &t);
    p.load_failed = None;
    std::fs::remove_file(&gone).unwrap();
    let mut r = restore_tab(&p);
    assert!(!r.load_failed, "{}", r.status);
    assert_eq!(doc_text(&r), text);
    assert!(save_doc_tab(&mut r, None), "{}", r.status);
    assert!(!tab_from_path(&gone).load_failed);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_session_from_before_strict_markdown_decoding_cannot_overwrite_its_source() {
    let dir = close_test_dir("legacy-markdown-session");
    let path = dir.join("legacy.md");
    // The old decoder saved its replacement characters in the sidecar and
    // marked the tab as loaded. Recreate that persisted state.
    std::fs::write(&path, "caf\u{fffd}\n").unwrap();
    let mut tab = tab_from_path(&path);
    assert!(!tab.load_failed);
    tab.dirty = true;
    let persisted = persist_tab(&dir, 0, &tab);
    assert!(persisted.hot.is_some());
    assert_eq!(persisted.load_failed, Some(false));

    let original = b"caf\xE9\n";
    std::fs::write(&path, original).unwrap();
    let mut restored = restore_tab(&persisted);
    assert!(restored.load_failed, "{}", restored.status);
    assert!(
        restored.status.starts_with("load error"),
        "{}",
        restored.status
    );
    assert!(!save_doc_tab(&mut restored, None));
    assert_eq!(restored.status.as_ref(), DOC_LOAD_FAILED_SAVE);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let _ = std::fs::remove_dir_all(&dir);
}

fn close_test_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/close-tests")
        .join(format!("{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn basic_docx_at(path: &std::path::Path) {
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
        path,
    )
    .unwrap();
}

fn doc_text(t: &DocTab) -> String {
    let Surface::Doc(ed) = &t.surface else {
        panic!("not a document")
    };
    docxcore::markdown::to_markdown(&ed.doc)
}

#[test]
fn a_corrupt_sidecar_reopens_the_file_from_disk() {
    let dir = close_test_dir("load-failed-sidecar");
    let good = dir.join("good.docx");
    basic_docx_at(&good);
    let on_disk = doc_text(&tab_from_path(&good));
    // A truncated (0-byte) sidecar is as unreadable as a garbled one, even
    // though a 0-byte file of the user's own opens as a new document.
    for (dirty, corrupt) in [
        (false, &b"not a zip"[..]),
        (true, b"not a zip"),
        (true, b""),
    ] {
        let mut t = tab_from_path(&good);
        t.dirty = dirty;
        let p = persist_tab(&dir, 0, &t);
        std::fs::write(p.hot.as_ref().unwrap(), corrupt).unwrap();
        let mut r = restore_tab(&p);
        assert!(!r.load_failed, "dirty={dirty}: {}", r.status);
        assert!(!r.dirty);
        assert!(r.status.starts_with("loaded"), "{}", r.status);
        assert!(r.status.contains("restored copy"), "{}", r.status);
        assert_eq!(doc_text(&r), on_disk);
        // The next exit and restart is an ordinary one.
        let again = restore_tab(&persist_tab(&dir, 1, &r));
        assert!(!again.load_failed && again.status.starts_with("loaded"));
        r.dirty = true;
        assert!(save_doc_tab(&mut r, None), "{}", r.status);
        let saved = tab_from_path(&good);
        assert!(
            !saved.load_failed && saved.pkg.is_some(),
            "{}",
            saved.status
        );
        assert_eq!(doc_text(&saved), on_disk);
    }
    // A sidecar cut in half (FIX r1 m4) still has readable local entries,
    // which Recover Text would read; a sidecar is never recovered, so the
    // tab reopens its intact file instead of showing partial text.
    let long = dir.join("long.docx");
    let md: String = (1..=40)
        .map(|i| {
            format!(
                "Paragraph {i} of a long one.

"
            )
        })
        .collect();
    std::fs::write(
        &long,
        docxcore::package::save_package(&docxcore::package::new_package(
            docxcore::markdown::from_markdown(&md),
        )),
    )
    .unwrap();
    let long_text = doc_text(&tab_from_path(&long));
    let t = tab_from_path(&long);
    let p = persist_tab(&dir, 3, &t);
    let hot = std::fs::read(p.hot.as_ref().unwrap()).unwrap();
    // Cut inside word/document.xml's own data, past its local header.
    let name = b"word/document.xml";
    // The part's own local header (the name also appears in
    // [Content_Types].xml): 30 header bytes, then the name.
    let at = (30..hot.len() - name.len())
        .find(|&i| &hot[i..i + name.len()] == name && &hot[i - 30..i - 26] == b"PK\x03\x04")
        .unwrap();
    // Two thirds into its (stored) data, by the size the header gives.
    let size = u32::from_le_bytes(hot[at - 12..at - 8].try_into().unwrap()) as usize;
    let cut = at + name.len() + size * 2 / 3;
    std::fs::write(p.hot.as_ref().unwrap(), &hot[..cut]).unwrap();
    assert!(
        docxcore::import::recover_docx_text(&hot[..cut])
            .unwrap()
            .is_some(),
        "the cut sidecar has text Recover Text could read"
    );
    let r = restore_tab(&p);
    assert!(r.status.contains("restored copy"), "{}", r.status);
    assert_eq!(r.access.converted, None);
    assert_eq!(doc_text(&r), long_text);
    // A file that is itself broken stays marked, by the fresh load.
    let broken = dir.join("broken.docx");
    std::fs::write(&broken, b"not a zip").unwrap();
    let p = persist_tab(&dir, 2, &tab_from_path(&broken));
    std::fs::write(p.hot.as_ref().unwrap(), b"garbage").unwrap();
    let mut r = restore_tab(&p);
    assert!(r.load_failed);
    assert!(r.status.starts_with("load error"), "{}", r.status);
    r.dirty = true;
    assert!(!save_doc_tab(&mut r, None));
    assert_eq!(std::fs::read(&broken).unwrap(), b"not a zip");
    // A never-saved document has no file to protect: unmarked, Save asks.
    let mut t = tab_from_path(&good);
    t.path = None;
    let p = persist_tab(&dir, 3, &t);
    std::fs::write(p.hot.as_ref().unwrap(), b"garbage").unwrap();
    let r = restore_tab(&p);
    assert!(!r.load_failed && r.path.is_none());
    assert!(r.status.starts_with("load error"), "{}", r.status);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_persisted_mark_holds_over_a_file_that_loads_at_restart() {
    let dir = close_test_dir("load-failed-persisted");
    let path = dir.join("x.docx");
    let t = tab_from_path(&path);
    assert!(t.load_failed, "{}", t.status);
    let json = serde_json::to_string(&persist_tab(&dir, 0, &t)).unwrap();
    let p: PersistTab = serde_json::from_str(&json).unwrap();
    assert_eq!(p.load_failed, Some(true));
    // The file appears (or is repaired) before the restart; the sidecar
    // still holds only the placeholder.
    basic_docx_at(&path);
    let before = std::fs::read(&path).unwrap();
    let mut r = restore_tab(&p);
    assert!(r.load_failed, "{}", r.status);
    r.dirty = true;
    assert!(!save_doc_tab(&mut r, None));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- AutoRecover tick (#632) ----------------------------------------------

/// A config root for one AutoRecover test, passed to `write_session` directly:
/// no test here sets `DOCXY_CONFIG_DIR`.
struct Root(PathBuf);
impl Root {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/autorecover-tests")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn session(&self) -> Session {
        serde_json::from_slice(&std::fs::read(session_path_in(&self.0)).unwrap()).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn prefs() -> Prefs {
    Prefs {
        theme: ThemePref::default(),
        ask_on_close: false,
        autorecover_minutes: 10,
        keep_drafts: true,
        edit_opts: gridcore::options::EditOptions::default(),
        custom_lists: Vec::new(),
        autocorrect: String::new(),
        user_name: String::new(),
        user_initials: String::new(),
    }
}

#[test]
fn autorecover_skips_the_write_when_nothing_is_unsaved() {
    let mut tabs = vec![tab(Kind::Docx), tab(Kind::Xlsx), tab(Kind::Project)];
    assert!(
        !autorecover_prepare(&mut tabs),
        "a clean session writes nothing"
    );
    tabs[1].dirty = true;
    assert!(autorecover_prepare(&mut tabs), "one unsaved tab is enough");
}

#[test]
fn autorecover_writes_an_unsaved_edit_that_a_crash_restores_as_recovered() {
    let root = Root::new("doc");
    let original = root.0.join("original.docx");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
        &original,
    )
    .unwrap();
    let before = std::fs::read(&original).unwrap();
    let mut t = tab_from_path(&original);
    let Surface::Doc(ed) = &mut t.surface else {
        panic!("{}", t.status)
    };
    ed.insert_str("recover me");
    t.dirty = true; // as `with_editor` leaves it
    let mut tabs = vec![t];

    assert!(autorecover_prepare(&mut tabs));
    write_session(&root.0, &tabs, 0, prefs());
    let session = root.session();
    assert!(session.tabs[0].dirty);
    let hot = PathBuf::from(session.tabs[0].hot.as_deref().unwrap());
    assert!(hot.starts_with(hot_dir_in(&root.0)), "{}", hot.display());

    let now = std::time::SystemTime::now();
    let crashed = restore_session(&session, true, now, &crate::trusted::TrustStore::default());
    assert!(doc_text(&crashed[0]).contains("recover me"));
    assert!(crashed[0].dirty);
    assert_eq!(crashed[0].path.as_deref(), Some(original.as_path()));
    assert!(
        crashed[0].status.starts_with("recovered"),
        "{}",
        crashed[0].status
    );
    // Restore reads the copy and never writes the original.
    assert_eq!(std::fs::read(&original).unwrap(), before);

    let clean = restore_session(&session, false, now, &crate::trusted::TrustStore::default());
    assert_eq!(clean[0].status.as_ref(), "unsaved — restored");
    assert!(doc_text(&clean[0]).contains("recover me"));
}

#[test]
fn a_crash_does_not_label_clean_tabs_recovered() {
    let root = Root::new("clean");
    let mut tabs = vec![tab(Kind::Docx), tab(Kind::Xlsx)];
    tabs[1].dirty = true;
    write_session(&root.0, &tabs, 0, prefs());
    let restored = restore_session(
        &root.session(),
        true,
        std::time::SystemTime::now(),
        &crate::trusted::TrustStore::default(),
    );
    assert!(
        !restored[0].status.starts_with("recovered"),
        "{}",
        restored[0].status
    );
    assert!(
        restored[1].status.starts_with("recovered"),
        "{}",
        restored[1].status
    );
}

#[test]
fn autorecover_leaves_an_open_cell_edit_open() {
    let mut tabs = vec![pending_sheet("Still typing")];
    tabs.push(tab(Kind::Docx));
    tabs[1].dirty = true;
    assert!(autorecover_prepare(&mut tabs));
    let Surface::Sheet(v) = &tabs[0].surface else {
        panic!()
    };
    assert_eq!(v.editing.as_deref(), Some("Still typing"));
    assert!(!tabs[0].dirty, "an uncommitted cell edit is not committed");
}

#[test]
fn autorecover_captures_an_open_header_and_stays_in_header_mode() {
    let root = Root::new("hf");
    let mut tabs = vec![tab(Kind::Docx)];
    let part = open_hf(&mut tabs[0], true, "Recovered header");
    assert!(autorecover_prepare(&mut tabs));
    assert!(tabs[0].hf_edit.is_some(), "the header editor stays open");
    write_session(&root.0, &tabs, 0, prefs());
    let restored = restore_session(
        &root.session(),
        true,
        std::time::SystemTime::now(),
        &crate::trusted::TrustStore::default(),
    );
    assert!(part_text(&restored[0], &part).contains("Recovered header"));
    assert_eq!(referenced_hf(&restored[0], true), Some(part));
}

#[test]
fn autorecover_is_on_by_default_and_the_setting_round_trips() {
    assert_eq!(Session::default().autorecover_minutes, 10);
    let old: Session = serde_json::from_str(r#"{"tabs":[],"active":0}"#).unwrap();
    assert_eq!(
        old.autorecover_minutes, 10,
        "a session from before the setting"
    );
    let root = Root::new("setting");
    let prefs = Prefs {
        autorecover_minutes: 0,
        ..prefs()
    };
    write_session(&root.0, &[tab(Kind::Docx)], 0, prefs);
    assert_eq!(root.session().autorecover_minutes, 0, "off is kept");
}

fn persisted_tab(
    kind: Kind,
    path: Option<&std::path::Path>,
    hot: Option<&std::path::Path>,
) -> PersistTab {
    PersistTab {
        kind,
        title: "Recovered?".into(),
        path: path.map(|p| p.display().to_string()),
        dirty: true,
        hot: hot.map(|p| p.display().to_string()),
        unreadable: Vec::new(),
        markdown: false,
        load_failed: Some(false),
        read_only: false,
        protected: false,
        repaired: false,
        stamp: None,
        converted: None,
        binary_source: false,
        compat: false,
    }
}

fn crash_restore(tabs: Vec<PersistTab>) -> Vec<DocTab> {
    let session = Session {
        tabs,
        ..Session::default()
    };
    restore_session(
        &session,
        true,
        std::time::SystemTime::now(),
        &crate::trusted::TrustStore::default(),
    )
}

/// Only content that came from a readable sidecar is an AutoRecover copy: a
/// tab that fell back to its file or a placeholder lost its edits, and a load
/// error must stay a load error.
#[test]
fn a_crash_labels_only_tabs_whose_sidecar_was_read() {
    let root = Root::new("sources");
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures");
    let empty_docx = root.0.join("tab-0.docx");
    std::fs::write(&empty_docx, b"").unwrap();
    let broken_xlsx = root.0.join("tab-1.xlsx");
    std::fs::write(&broken_xlsx, b"not a workbook").unwrap();
    let gone = root.0.join("missing.docx");
    let original = root.0.join("original.docx");
    std::fs::copy(fixtures.join("basic.docx"), &original).unwrap();
    let sheet = root.0.join("original.xlsx");
    std::fs::copy(fixtures.join("basic.xlsx"), &sheet).unwrap();

    let restored = crash_restore(vec![
        // Never saved, 0-byte sidecar: a placeholder with a load error.
        persisted_tab(Kind::Docx, None, Some(&empty_docx)),
        // A file, no sidecar recorded: the file is reloaded.
        persisted_tab(Kind::Docx, Some(&original), None),
        // A file, the recorded sidecar is gone.
        persisted_tab(Kind::Docx, Some(&original), Some(&gone)),
        // A workbook whose sidecar cannot be read.
        persisted_tab(Kind::Xlsx, Some(&sheet), Some(&broken_xlsx)),
        // A workbook with no sidecar.
        persisted_tab(Kind::Xlsx, Some(&sheet), None),
    ]);
    assert!(
        restored[0].status.starts_with("load error"),
        "{}",
        restored[0].status
    );
    for t in &restored {
        assert!(!t.status.starts_with("recovered"), "{}", t.status);
    }
}

#[test]
fn a_crash_still_labels_sheet_and_project_sidecars_it_read() {
    let root = Root::new("sheet-project");
    let mut tabs = vec![tab(Kind::Xlsx), tab(Kind::Project)];
    for t in &mut tabs {
        t.dirty = true;
    }
    write_session(&root.0, &tabs, 0, prefs());
    let restored = restore_session(
        &root.session(),
        true,
        std::time::SystemTime::now(),
        &crate::trusted::TrustStore::default(),
    );
    for t in &restored {
        assert!(t.dirty);
        assert!(t.status.starts_with("recovered"), "{}", t.status);
    }
}

/// The macro question's answer for a workbook that has no macros to lose:
/// it is never asked.
fn no_macros(features: &[&'static str]) -> bool {
    panic!("asked about {features:?}")
}

// ---- Drafts kept by Don't Save (#613) --------------------------------------

#[test]
fn only_removals_leave_the_active_tab_alone() {
    assert!(CloseStep::Remove.removes());
    assert!(
        CloseStep::Discard.removes(),
        "Don't Save keeps previous_active"
    );
    assert!(!CloseStep::Save.removes());
    assert!(!CloseStep::Keep.removes());
    assert!(!CloseStep::Refuse("no".into()).removes());
}

#[test]
fn a_draft_is_kept_only_for_a_discarded_workbook_with_autorecover_and_a_write() {
    let hot = PathBuf::from("tab-0.xlsx");
    let hot = Some(hot.as_path());
    assert!(should_keep_draft(Kind::Xlsx, 1, true, true, hot));
    assert!(
        !should_keep_draft(Kind::Xlsx, 1, true, false, hot),
        "clean, Save or Cancel"
    );
    assert!(
        !should_keep_draft(Kind::Xlsx, 1, true, true, None),
        "no hot-exit write while unsaved"
    );
    assert!(
        !should_keep_draft(Kind::Xlsx, 0, true, true, hot),
        "AutoRecover off"
    );
    assert!(
        !should_keep_draft(Kind::Xlsx, 1, false, true, hot),
        "keep off"
    );
    assert!(
        !should_keep_draft(Kind::Docx, 1, true, true, hot),
        "a document"
    );
    assert!(
        !should_keep_draft(Kind::Project, 1, true, true, hot),
        "a plan"
    );
}

#[test]
fn keeping_drafts_is_on_by_default_and_the_setting_round_trips() {
    assert!(Session::default().keep_drafts);
    let old: Session = serde_json::from_str(r#"{"tabs":[],"active":0}"#).unwrap();
    assert!(old.keep_drafts, "a session from before the setting");
    let root = Root::new("keep-setting");
    let prefs = Prefs {
        keep_drafts: false,
        ..prefs()
    };
    write_session(&root.0, &[tab(Kind::Xlsx)], 0, prefs);
    assert!(!root.session().keep_drafts, "off is kept");
}

/// #672: Settings' Sheet editing options persist in `session.json`, and a
/// session from before them (or with a bad value) gets Excel's defaults.
#[test]
fn sheet_editing_options_round_trip_through_the_session() {
    use gridcore::options::{EditOptions, EnterMove};
    let old: Session = serde_json::from_str(r#"{"tabs":[],"active":0}"#).unwrap();
    assert_eq!(
        EditOptions::from_text(&old.sheet_editing),
        EditOptions::default()
    );
    let bad: Session = serde_json::from_str(
        r#"{"tabs":[],"active":0,"sheet_editing":"edit_move_direction=nowhere\n"}"#,
    )
    .unwrap();
    assert_eq!(
        EditOptions::from_text(&bad.sheet_editing).enter_move,
        EnterMove::Down
    );
    let root = Root::new("sheet-editing");
    let opts = EditOptions {
        fixed_decimal: true,
        places: -1,
        enter_move: EnterMove::Up,
        fill_handle: false,
        ..EditOptions::default()
    };
    let prefs = Prefs {
        edit_opts: opts,
        ..prefs()
    };
    write_session(&root.0, &[tab(Kind::Xlsx)], 0, prefs);
    assert_eq!(EditOptions::from_text(&root.session().sheet_editing), opts);
}

/// #667: AutoCorrect's changes persist in `sheet_editing` beside the
/// Editing options, and each reads its own keys back.
#[test]
fn autocorrect_persists_with_the_sheet_editing_options() {
    use gridcore::autocorrect::{AutoCorrect, ExceptionKind};
    use gridcore::options::EditOptions;
    let root = Root::new("autocorrect");
    let mut ac = AutoCorrect::default();
    ac.add("cdp", "Consolidated Data Processing").unwrap();
    ac.delete("adn");
    ac.add_exception(ExceptionKind::InitialCaps, "ABc").unwrap();
    ac.opts.hyperlinks = false;
    let opts = EditOptions {
        flash_fill_auto: false,
        ..EditOptions::default()
    };
    let prefs = Prefs {
        edit_opts: opts,
        autocorrect: ac.to_lines(),
        ..prefs()
    };
    write_session(&root.0, &[tab(Kind::Xlsx)], 0, prefs);
    let saved = root.session().sheet_editing;
    assert_eq!(AutoCorrect::from_text(&saved), ac);
    assert_eq!(EditOptions::from_text(&saved), opts);
}

/// #672: a sheet tab reached through the app gets the app's Editing options,
/// so one opened after they changed honours them on its first key.
#[test]
fn a_sheet_reached_through_the_app_has_its_editing_options() {
    use gridcore::options::EditOptions;
    let mut t = tab(Kind::Xlsx);
    let opts = EditOptions {
        autocomplete: false,
        ..EditOptions::default()
    };
    let v = sheet_with_opts(Some(&mut t), opts).expect("a sheet tab");
    assert_eq!(v.edit_opts, opts);
    let mut doc = tab(Kind::Docx);
    assert!(sheet_with_opts(Some(&mut doc), opts).is_none());
}

/// #672 r1: every sheet tab, however it was created, holds the app's
/// Editing options once they are stamped (restore, a change, each frame).
#[test]
fn every_sheet_tab_is_stamped_with_the_app_options() {
    use gridcore::options::EditOptions;
    let opts = EditOptions {
        fixed_decimal: true,
        ..EditOptions::default()
    };
    let mut tabs = vec![tab(Kind::Xlsx), tab(Kind::Docx), tab(Kind::Xlsx)];
    stamp_edit_opts(&mut tabs, opts);
    for t in &tabs {
        if let Surface::Sheet(v) = &t.surface {
            assert_eq!(v.edit_opts, opts);
        }
    }
    assert!(matches!(tabs[2].surface, Surface::Sheet(_)));
}

/// FIX r5 m4: the per-frame step retires a Flash Fill preview the selection
/// moved off, so moving back cannot revive it.
#[test]
fn the_frame_step_retires_a_preview_the_selection_left() {
    let mut tabs = vec![tab(Kind::Xlsx)];
    let Surface::Sheet(v) = &mut tabs[0].surface else {
        panic!("a sheet tab")
    };
    for (r, t) in [
        "Ada Lovelace",
        "Alan Turing",
        "Grace Hopper",
        "Edsger Dijkstra",
    ]
    .iter()
    .enumerate()
    {
        let s = v.active;
        let cell =
            gridcore::entry::entry_cell(&mut v.pkg.workbook, s, r as u32, 10, t, None).unwrap();
        v.engine
            .set_cell(&mut v.pkg.workbook, (s, r as u32, 10), cell);
    }
    for (r, t) in ["Ada", "Alan"].iter().enumerate() {
        v.sel = (r as u32, 11);
        v.begin_cell_edit(Some(t.to_string()));
        let origin = v.edit_origin.unwrap();
        assert_eq!(v.commit_and_move(1, 0), Some(true));
        v.flash_preview_after(origin);
    }
    assert!(
        v.flash_preview.is_some(),
        "a preview after the second example"
    );
    let at = v.sel;
    v.sel = (9, 9);
    crate::sheet_flashfill::retire_stale_previews(&mut tabs);
    let Surface::Sheet(v) = &mut tabs[0].surface else {
        unreachable!()
    };
    assert!(v.flash_preview.is_none(), "dropped by the frame step");
    v.sel = at;
    crate::sheet_flashfill::retire_stale_previews(&mut tabs);
    let Surface::Sheet(v) = &tabs[0].surface else {
        unreachable!()
    };
    assert!(v.live_preview().is_none(), "and not revived by moving back");
}

#[test]
fn a_persist_records_the_sidecar_only_while_the_tab_is_unsaved() {
    let root = Root::new("last-hot");
    let mut tabs = vec![tab(Kind::Docx), tab(Kind::Xlsx)];
    tabs[1].dirty = true;
    write_session(&root.0, &tabs, 0, prefs());
    assert_eq!(*tabs[0].last_hot.borrow(), None, "clean");
    let hot = tabs[1].last_hot.borrow().clone().unwrap();
    assert_eq!(hot, hot_dir_in(&root.0).join("tab-1.xlsx"));
    assert!(hot.exists());
    // Saved since: the next persist forgets it.
    tabs[1].dirty = false;
    write_session(&root.0, &tabs, 0, prefs());
    assert_eq!(*tabs[1].last_hot.borrow(), None);
    // A sibling closed: the path follows the tab to its new index.
    tabs[1].dirty = true;
    tabs.remove(0);
    write_session(&root.0, &tabs, 0, prefs());
    assert_eq!(
        tabs[0].last_hot.borrow().as_deref(),
        Some(hot_dir_in(&root.0).join("tab-0.xlsx").as_path())
    );
}

/// A1 committed to `text`, as Enter leaves it.
fn commit_a1(t: &mut DocTab, text: &str) {
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    v.sel = (0, 0);
    v.anchor = (0, 0);
    v.begin_cell_edit(Some(text.into()));
    commit_changed_cell(t).unwrap();
    assert!(t.dirty);
}

/// The issue's scenario below the window: type in a workbook and commit, an
/// AutoRecover write, more typing, Don't Save. The draft is the AutoRecover
/// write, not the content at close, and opens read-only.
#[test]
fn dont_save_keeps_the_last_autorecover_copy_as_a_read_only_draft() {
    let root = Root::new("keep-draft");
    let mut tabs = vec![tab(Kind::Xlsx)];
    commit_a1(&mut tabs[0], "Draft me");
    assert!(autorecover_prepare(&mut tabs));
    write_session(&root.0, &tabs, 0, prefs());
    commit_a1(&mut tabs[0], "After the tick");
    let step = close_step(&mut tabs[0], Some(CloseAnswer::Discard));
    assert_eq!(step, CloseStep::Discard);
    let now = std::time::SystemTime::now();
    let draft = keep_closed_draft(&root.0, &tabs[0], &step, 1, true, now)
        .unwrap()
        .unwrap();
    let drafts = recover::list_drafts(&root.0, now, &[]);
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    assert_eq!(drafts[0].path, draft);
    assert!(drafts[0].name.starts_with("basic ((Unsaved-"), "{drafts:?}");

    let opened = tab_from_path_mode(
        &draft,
        OpenMode::ReadOnly,
        &crate::trusted::TrustStore::default(),
    )
    .unwrap();
    assert_eq!(sheet_a1(&opened), "Draft me");
    assert!(opened.access.read_only);
}

#[test]
fn no_draft_without_a_write_while_unsaved_or_without_discard() {
    let root = Root::new("no-draft");
    let now = std::time::SystemTime::now();
    // Typed and committed, but closed before any persist.
    let mut t = tab(Kind::Xlsx);
    commit_a1(&mut t, "Never written");
    let step = close_step(&mut t, Some(CloseAnswer::Discard));
    assert_eq!(keep_closed_draft(&root.0, &t, &step, 1, true, now), None);
    // Written while unsaved, but the answer was Cancel.
    let tabs = vec![t];
    write_session(&root.0, &tabs, 0, prefs());
    let mut t = tabs.into_iter().next().unwrap();
    let step = close_step(&mut t, Some(CloseAnswer::Cancel));
    assert_eq!(keep_closed_draft(&root.0, &t, &step, 1, true, now), None);
    // A clean tab is removed without a draft.
    let mut clean = tab(Kind::Xlsx);
    let step = close_step(&mut clean, None);
    assert_eq!(
        keep_closed_draft(&root.0, &clean, &step, 1, true, now),
        None
    );
    assert_eq!(recover::list_drafts(&root.0, now, &[]), vec![]);
}

#[test]
fn a_draft_that_could_not_be_kept_is_reported_where_it_can_be_seen() {
    assert_eq!(draft_error_to(true, false), DraftErrorTo::Status);
    assert_eq!(draft_error_to(true, true), DraftErrorTo::Status);
    assert_eq!(
        draft_error_to(false, false),
        DraftErrorTo::Dialog,
        "the last tab closed: no status line is left"
    );
    assert_eq!(
        draft_error_to(false, true),
        DraftErrorTo::Reply,
        "no native modal under the harness"
    );
}

#[test]
fn only_a_draft_that_loads_is_labelled_recovered() {
    let root = Root::new("draft-loads");
    let trusted = crate::trusted::TrustStore::default();
    let good = root.0.join("good ((Unsaved-1)).xlsx");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.xlsx"),
        &good,
    )
    .unwrap();
    let bad = root.0.join("bad ((Unsaved-1)).xlsx");
    std::fs::write(&bad, b"not a workbook").unwrap();
    let opened = tab_from_path_mode(&good, OpenMode::ReadOnly, &trusted).unwrap();
    assert!(draft_loaded(&opened));
    let broken = tab_from_path_mode(&bad, OpenMode::ReadOnly, &trusted).unwrap();
    assert!(!draft_loaded(&broken), "{}", broken.status);
}

/// A never-saved document tab titled `title` holding `markdown`.
fn untitled(title: &str, markdown: &str) -> DocTab {
    let mut tab = sample_doc().into_tab(Kind::Docx, title.into(), None, false);
    tab.surface = Surface::Doc(Editor::new(docxcore::markdown::from_markdown(markdown)));
    tab
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures")
}

fn known() -> Vec<PathBuf> {
    vec![PathBuf::from("/docs"), PathBuf::from("/desk")]
}

#[test]
fn a_never_saved_document_proposes_its_first_words() {
    let tab = untitled("Document1", "Quarterly report for the north region\n");
    let name = prompt_name(&tab, &known());
    assert_eq!(name.stem, "Quarterly report for the north region");
    assert_eq!(name.ext, ".docx");
    assert_eq!(name.locations, known());
    // An empty one keeps its title.
    let empty = prompt_name(&untitled("Document3", ""), &known());
    assert_eq!(empty.stem, "Document3");
}

#[test]
fn a_saved_document_proposes_its_own_name_in_its_own_folder() {
    let tab = tab(Kind::Docx);
    let name = prompt_name(&tab, &known());
    assert_eq!(name.stem, "basic");
    assert_eq!(name.ext, ".docx");
    assert_eq!(name.locations[0], fixtures());
    assert_eq!(&name.locations[1..], known().as_slice());
    // Its own folder is not offered twice.
    let again = prompt_name(&tab, &[fixtures(), PathBuf::from("/docs")]);
    assert_eq!(again.locations, vec![fixtures(), PathBuf::from("/docs")]);
    // Unchanged, Save writes it in place.
    assert_eq!(
        prompt_target(
            &name.stem,
            &name.ext,
            &name.locations[0],
            tab.path.as_deref(),
            true
        ),
        Ok(PromptSave::InPlace)
    );
}

#[test]
fn a_markdown_document_keeps_its_extension() {
    let dir = crate::open_mode_tests::Scratch::new();
    let path = dir.path("notes.md");
    std::fs::write(&path, "# Notes\n").unwrap();
    let tab = tab_from_path(&path);
    assert!(tab.markdown);
    let name = prompt_name(&tab, &known());
    assert_eq!((name.stem.as_str(), name.ext.as_str()), ("notes", ".md"));
    assert_eq!(
        prompt_target(
            "notes",
            ".md",
            &name.locations[0],
            tab.path.as_deref(),
            true
        ),
        Ok(PromptSave::InPlace)
    );
    assert_eq!(
        prompt_target(
            "renamed",
            ".md",
            &name.locations[0],
            tab.path.as_deref(),
            true
        ),
        Ok(PromptSave::To(dir.path("renamed.md")))
    );
}

#[test]
fn a_tab_that_saves_as_never_saves_in_place_from_the_prompt() {
    // Read-only, repaired and converted tabs, and an imported .doc: Save is
    // Save As today, so the prompt never writes over the tab's own file.
    let mut cases = Vec::new();
    let mut read_only = tab(Kind::Docx);
    read_only.access.read_only = true;
    cases.push(read_only);
    let mut repaired = tab(Kind::Docx);
    repaired.access.repaired = true;
    cases.push(repaired);
    let mut converted = tab(Kind::Docx);
    converted.access.converted = Some(crate::open_mode::Converted::Rtf);
    cases.push(converted);
    for tab in cases {
        assert!(!saves_in_place(&tab));
        let name = prompt_name(&tab, &known());
        assert_eq!(name.ext, ".docx");
        assert_eq!(name.locations[0], fixtures());
        // The proposed name is a copy beside it, so Save (Enter) works.
        assert_eq!(name.stem, "basic (copy)", "{:?}", tab.access);
        assert_eq!(
            prompt_target(
                &name.stem,
                &name.ext,
                &name.locations[0],
                tab.path.as_deref(),
                false
            ),
            Ok(PromptSave::To(fixtures().join("basic (copy).docx"))),
            "{:?}",
            tab.access
        );
        // Typing its own name back is refused, never a silent overwrite.
        let own = prompt_target("basic", ".docx", &fixtures(), tab.path.as_deref(), false);
        assert!(
            own.as_ref()
                .unwrap_err()
                .contains("not opened for saving in place"),
            "{own:?}"
        );
    }
    let mut imported = tab(Kind::Docx);
    imported.path = Some(fixtures().join("letter.doc"));
    imported.import.binary_source = true;
    assert!(!saves_in_place(&imported));
    let name = prompt_name(&imported, &known());
    assert_eq!((name.stem.as_str(), name.ext.as_str()), ("letter", ".docx"));
    assert_eq!(name.locations[0], fixtures());
}

#[test]
fn the_prompts_default_save_leaves_a_read_only_original_alone() {
    let dir = crate::open_mode_tests::Scratch::new();
    let src = dir.path("report.docx");
    std::fs::copy(fixtures().join("basic.docx"), &src).unwrap();
    let before = std::fs::read(&src).unwrap();
    let mut t = tab_from_path(&src);
    t.access.read_only = true;
    let Surface::Doc(ed) = &mut t.surface else {
        panic!("{}", t.status)
    };
    ed.insert_str("Edited read-only");
    t.dirty = true;
    let name = prompt_name(&t, &known());
    let PromptSave::To(target) = prompt_target(
        &name.stem,
        &name.ext,
        &name.locations[0],
        t.path.as_deref(),
        saves_in_place(&t),
    )
    .unwrap() else {
        panic!("a read-only tab never saves in place")
    };
    assert_eq!(target, dir.path("report (copy).docx"));
    assert!(save_doc_tab(&mut t, Some(target.clone())), "{}", t.status);
    assert_eq!(
        std::fs::read(&src).unwrap(),
        before,
        "the original was written"
    );
    assert!(target.exists());
}

#[test]
fn the_prompt_target_refuses_what_save_cannot_write() {
    let dir = crate::open_mode_tests::Scratch::new();
    let own = dir.path("mine.docx");
    std::fs::write(&own, b"x").unwrap();
    std::fs::write(dir.path("other.docx"), b"x").unwrap();
    let at = |stem: &str| prompt_target(stem, ".docx", &dir.path(""), Some(&own), true);
    assert_eq!(at("mine"), Ok(PromptSave::InPlace));
    // A typed extension is the fixed one.
    assert_eq!(at("mine.DOCX"), Ok(PromptSave::InPlace));
    assert_eq!(at("  fresh "), Ok(PromptSave::To(dir.path("fresh.docx"))));
    assert!(
        at("other")
            .unwrap_err()
            .contains("other.docx already exists")
    );
    assert!(at("   ").unwrap_err().contains("Type a file name"));
    for bad in [
        "a/b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\\b",
    ] {
        assert!(at(bad).unwrap_err().contains("can't contain"), "{bad}");
    }
    // A never-saved document has no file of its own to write in place.
    assert!(
        prompt_target("other", ".docx", &dir.path(""), None, false)
            .unwrap_err()
            .contains("already exists")
    );
}

#[test]
fn the_document_prompt_is_words() {
    let name = prompt_name(
        &untitled("Document1", "Quarterly report for the north region\n"),
        &known(),
    );
    let d = doc_prompt(&name, false);
    assert_eq!(d.title, "Save your changes to this file?");
    assert_eq!(d.owner, DialogOwner::SaveOnClose { quit: false });
    let names: Vec<&str> = d.controls.iter().map(|c| c.name).collect();
    assert_eq!(names, ["file-name", "extension", "location"]);
    assert_eq!(d.controls[0].label, "File name:");
    assert_eq!(
        d.controls[0].text(),
        "Quarterly report for the north region"
    );
    assert_eq!(d.controls[1].kind, ControlKind::Label);
    assert_eq!(d.controls[1].text(), ".docx");
    assert_eq!(d.controls[2].label, "Choose a Location:");
    assert_eq!(d.controls[2].items, ["/docs", "/desk"]);
    assert_eq!(d.controls[2].text(), "/docs");
    let labels: Vec<&str> = d.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["Save", "Don't Save", "Cancel", "More options..."]);
    let mut stack = crate::dialog::DialogStack::default();
    stack.push(d);
    assert_eq!(stack.key_button("enter", true).as_deref(), Some("Save"));
    assert_eq!(stack.key_button("escape", true).as_deref(), Some("Cancel"));
    // Typing goes to the File name.
    stack.top_dialog_mut().unwrap().type_char('!').unwrap();
    assert_eq!(
        stack.top().unwrap().controls[0].text(),
        "Quarterly report for the north region!"
    );
}

#[test]
fn a_workbook_or_project_prompt_names_the_tab() {
    for kind in [Kind::Xlsx, Kind::Project] {
        let t = tab(kind);
        let d = close_prompt(&t, true, &known());
        assert_eq!(
            d.text.as_deref(),
            Some(format!("Save changes to {} before closing?", t.title).as_str())
        );
        assert_eq!(d.owner, DialogOwner::SaveOnClose { quit: true });
        let labels: Vec<&str> = d.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Save", "Don't Save", "Cancel"]);
        assert!(d.buttons[0].default);
    }
}

/// A file-backed document tab on a private copy of basic.docx, with `text`
/// typed into it (unsaved).
fn edited_copy(root: &Root, name: &str, text: &str) -> DocTab {
    let path = root.0.join(name);
    std::fs::copy(fixtures().join("basic.docx"), &path).unwrap();
    let mut t = tab_from_path(&path);
    let Surface::Doc(ed) = &mut t.surface else {
        panic!("{}", t.status)
    };
    ed.insert_str(text);
    t.dirty = true;
    t
}

#[test]
fn quitting_asks_each_unsaved_tab_once_in_order() {
    let mut tabs = vec![
        tab(Kind::Docx),
        tab(Kind::Xlsx),
        tab(Kind::Docx),
        tab(Kind::Project),
    ];
    tabs[1].dirty = true;
    tabs[3].dirty = true;
    assert_eq!(next_to_ask(&tabs, &[]), Some(1));
    // Answered Don't Save: the next one; a clean tab is never asked.
    assert_eq!(next_to_ask(&tabs, &[1]), Some(3));
    assert_eq!(next_to_ask(&tabs, &[1, 3]), None);
    // Answered Save: it is clean now.
    tabs[1].dirty = false;
    assert_eq!(next_to_ask(&tabs, &[]), Some(3));
}

#[test]
fn dont_save_on_quit_drops_a_never_saved_tab_and_forgets_a_files_edits() {
    let root = Root::new("quit-forget");
    let mut tabs = vec![
        untitled("Document1", "never saved\n"),
        edited_copy(&root, "kept.docx", "QuitKeptMarker"),
        edited_copy(&root, "forgotten.docx", "QuitForgottenMarker"),
        untitled("Document2", "also never saved\n"),
    ];
    let before = std::fs::read(root.0.join("forgotten.docx")).unwrap();
    let mut active = 2;
    let forget = forget_on_quit(&mut tabs, &mut active, vec![0, 2, 3]);
    let titles: Vec<&str> = tabs.iter().map(|t| t.title.as_ref()).collect();
    assert_eq!(titles, ["kept.docx", "forgotten.docx"]);
    assert_eq!(forget, [1]);
    assert_eq!(active, 1);

    write_session_forgetting(&root.0, &tabs, active, prefs(), &forget);
    let session = root.session();
    assert!(session.tabs[0].dirty);
    assert!(
        session.tabs[0].hot.is_some(),
        "an unanswered tab keeps its work"
    );
    assert!(!session.tabs[1].dirty);
    assert!(session.tabs[1].hot.is_none());
    assert!(tabs[1].last_hot.borrow().is_none());

    // The next launch reopens the file as it is on disk, and the file was
    // never written.
    let restored = restore_session(
        &session,
        false,
        std::time::SystemTime::now(),
        &crate::trusted::TrustStore::default(),
    );
    assert!(doc_text(&restored[0]).contains("QuitKeptMarker"));
    assert!(!restored[1].dirty);
    assert!(!doc_text(&restored[1]).contains("QuitForgottenMarker"));
    assert_eq!(
        std::fs::read(root.0.join("forgotten.docx")).unwrap(),
        before
    );
}

#[test]
fn a_quit_is_live_only_while_its_question_is_open() {
    let mut tabs = vec![tab(Kind::Docx), tab(Kind::Project)];
    assert!(!quit_prompt_live(&tabs));
    // A tab's own close prompt is not a quit.
    let p = close_prompt(&tabs[1], false, &known());
    tabs[1].dialogs.push(p);
    assert!(!quit_prompt_live(&tabs));
    tabs[1].dialogs.clear();
    let p = close_prompt(&tabs[1], true, &known());
    tabs[1].dialogs.push(p);
    assert!(quit_prompt_live(&tabs));
    // Cleared behind the app's back (a control verb, say): a later window
    // close starts a new quit instead of waiting forever.
    tabs[1].dialogs.clear();
    assert!(!quit_prompt_live(&tabs));
}

#[test]
fn a_quit_notices_its_tabs_changing() {
    let mut tabs = vec![tab(Kind::Docx), tab(Kind::Xlsx)];
    let at_start = tab_ids(&tabs);
    assert_eq!(tab_ids(&tabs), at_start);
    tabs.swap(0, 1);
    assert_ne!(tab_ids(&tabs), at_start, "a reorder");
    tabs.swap(0, 1);
    tabs.remove(1);
    assert_ne!(tab_ids(&tabs), at_start, "a close");
}

#[test]
fn project_verbs_that_would_drop_a_close_prompt_are_refused() {
    let mut tabs = vec![tab(Kind::Docx), tab(Kind::Project)];
    for verb in [
        "proj.open",
        "proj.save",
        "proj.reload",
        projctl::MUTATING[0],
        "proj.path",
    ] {
        assert_eq!(close_prompt_refusal(&tabs, verb), Ok(()), "{verb}");
    }
    let p = close_prompt(&tabs[1], true, &known());
    tabs[1].dialogs.push(p);
    for verb in [
        "proj.open",
        "proj.save",
        "proj.reload",
        projctl::MUTATING[0],
    ] {
        assert_eq!(
            close_prompt_refusal(&tabs, verb),
            Err("a dialog is open: docxy".into()),
            "{verb}"
        );
    }
    // Reads still answer.
    assert_eq!(close_prompt_refusal(&tabs, "proj.path"), Ok(()));
}

#[test]
fn a_quits_own_save_under_a_new_name_is_not_a_change_of_tabs() {
    let mut tabs = vec![untitled("Document1", "Quarterly report\n"), tab(Kind::Docx)];
    let mut ids = tab_ids(&tabs);
    // Save named the never-saved document, and saved the other as a copy.
    tabs[0].title = "Quarterly report.docx".into();
    tabs[0].path = Some(PathBuf::from("/docs/Quarterly report.docx"));
    refresh_tab_id(&mut ids, &tabs, 0);
    assert_eq!(tab_ids(&tabs), ids);
    tabs[1].title = "renamed.docx".into();
    tabs[1].path = Some(PathBuf::from("/docs/renamed.docx"));
    refresh_tab_id(&mut ids, &tabs, 1);
    assert_eq!(tab_ids(&tabs), ids);
    // Anything else still is.
    tabs.swap(0, 1);
    assert_ne!(tab_ids(&tabs), ids);
    // Out of range: nothing to refresh, nothing panics.
    refresh_tab_id(&mut ids, &tabs, 9);
}

#[test]
fn an_in_place_tab_keeps_whatever_extension_it_has() {
    let dir = crate::open_mode_tests::Scratch::new();
    for (name, stem, ext) in [("Letter.dotx", "Letter", ".dotx"), ("Letter", "Letter", "")] {
        let path = dir.path(name);
        std::fs::copy(fixtures().join("basic.docx"), &path).unwrap();
        let t = tab_from_path(&path);
        assert!(matches!(t.surface, Surface::Doc(_)), "{name}: {}", t.status);
        assert!(saves_in_place(&t), "{name}");
        let prompt = prompt_name(&t, &known());
        assert_eq!(
            (prompt.stem.as_str(), prompt.ext.as_str()),
            (stem, ext),
            "{name}"
        );
        assert_eq!(
            prompt_target(
                &prompt.stem,
                &prompt.ext,
                &prompt.locations[0],
                t.path.as_deref(),
                true
            ),
            Ok(PromptSave::InPlace),
            "{name}"
        );
    }
}

// ---- dialog buffers (#202) -------------------------------------------------
//
// Window close, harness `quit` and the active tab's own close fold the app's
// typed-but-uncommitted dialog buffers into the workbook before the hot-exit
// persist, through `commit_comment_buffer(_for_exit)` and
// `commit_rename_buffer` below; which buffers reach them is decided per
// buffer in `Docxy::commit_dialog_buffers_for_exit`. These tests pin the
// tab-level half: the commit itself, the dirty flag, and the round-trip
// through the persisted session.

/// The comment on the active sheet's selected cell, if any.
fn cell_comment(t: &DocTab) -> Option<String> {
    let Surface::Sheet(v) = &t.surface else {
        panic!("{}", t.status)
    };
    let (r, c) = v.sel;
    v.pkg
        .comments()
        .into_iter()
        .find(|cm| cm.sheet == v.active && cm.row == r && cm.col == c)
        .map(|cm| cm.text)
}

/// What the dropped rule bars (conditional formatting, data validation, row
/// height) would change: the exit commits must leave all of it alone.
fn fmt_state(t: &DocTab) -> (usize, usize, usize) {
    let Surface::Sheet(v) = &t.surface else {
        panic!("{}", t.status)
    };
    let s = &v.pkg.workbook.sheets[v.active];
    (s.cond_formats.len(), s.validations.len(), s.row_attrs.len())
}

#[test]
fn exit_commits_typed_comment_and_marks_tab_dirty() {
    let mut tabs = vec![tab(Kind::Xlsx)];
    {
        let Surface::Sheet(v) = &mut tabs[0].surface else {
            panic!()
        };
        v.sel = (0, 0);
    }
    assert!(!tabs[0].dirty);
    // The bar opened on A1, empty; the user typed and closed the window.
    assert!(commit_comment_buffer_for_exit(
        &mut tabs[0],
        "",
        "Jane Doe",
        "Check this"
    ));
    assert!(tabs[0].dirty);
    assert_eq!(cell_comment(&tabs[0]).as_deref(), Some("Check this"));
    // The persisted hot-exit session carries it, as the next launch sees it.
    let restored = exit_and_restore(&mut tabs, "exit-comment");
    assert!(restored[0].dirty);
    assert_eq!(cell_comment(&restored[0]).as_deref(), Some("Check this"));
}

#[test]
fn exit_commits_typed_sheet_rename_and_follows_chart_refs() {
    let mut tabs = vec![tab(Kind::Xlsx)];
    let old = {
        let Surface::Sheet(v) = &mut tabs[0].surface else {
            panic!()
        };
        let old = v.pkg.workbook.sheets[0].name.clone();
        // A chart this session authored, plotting the sheet about to be
        // renamed — the workbook-side refs `rename_sheet` follows are its
        // own; this one follows the `rename_sheet_in_chart` loop.
        v.charts.push(ChartView {
            sheet: 0,
            from: (0, 0),
            to: (4, 4),
            data: gridcore::sheet::ChartData {
                source: Some(gridcore::sheet::ChartSource {
                    sheet: old.clone(),
                    range: (0, 0, 4, 3),
                    cat_col: 0,
                }),
                ..Default::default()
            },
        });
        old
    };
    let new = format!("{old}X");
    assert!(commit_rename_buffer(&mut tabs[0], 0, &new));
    assert!(tabs[0].dirty);
    {
        let Surface::Sheet(v) = &tabs[0].surface else {
            panic!()
        };
        assert_eq!(v.pkg.workbook.sheets[0].name, new);
        let src = v.charts[0].data.source.as_ref().expect("chart source");
        assert_eq!(src.sheet, new);
    }
    // The persisted session round-trips under the new name.
    let restored = exit_and_restore(&mut tabs, "exit-rename");
    assert!(restored[0].dirty);
    let Surface::Sheet(v) = &restored[0].surface else {
        panic!()
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, new);
}

#[test]
fn tab_level_exit_commits_leave_cf_dv_and_rowh_alone() {
    // What this pins: the tab-level exit commits (`commit_comment_buffer_for_exit`,
    // `commit_rename_buffer`) never touch conditional formats, validations or
    // row heights. The other half of the decision — that the app drops the
    // cf/dv/rowh BUFFERS without committing them — lives in
    // `Docxy::commit_dialog_buffers_for_exit` over gpui state and has no
    // suite-level guard (same seam as the chart field, agreed in pin 4).
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        // A row height as the row-height bar would have set it; the cf/dv
        // lists stay as the file has them.
        v.pkg.workbook.sheets[v.active].set_row_height(2, Some(42.0));
    }
    let before = fmt_state(&t);
    // The exit commits take no rule/format buffer: running them changes none
    // of it, even while they commit what they do own.
    assert!(commit_comment_buffer_for_exit(
        &mut t,
        "",
        "Jane Doe",
        "Check this"
    ));
    let old = {
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        v.pkg.workbook.sheets[0].name.clone()
    };
    assert!(commit_rename_buffer(&mut t, 0, &format!("{old}X")));
    assert_eq!(fmt_state(&t), before);
    assert!(t.dirty);
}

#[test]
fn exit_with_empty_comment_buffer_does_not_dirty_clean_tab() {
    // A comment bar merely OPEN on a cell with no comment: the buffer is
    // empty, the commit could only delete a comment that isn't there, so the
    // tab stays clean — in the live tab and in the restored session.
    let mut tabs = vec![tab(Kind::Xlsx)];
    {
        let Surface::Sheet(v) = &mut tabs[0].surface else {
            panic!()
        };
        v.sel = (1, 1);
    }
    assert!(cell_comment(&tabs[0]).is_none());
    assert!(!commit_comment_buffer_for_exit(
        &mut tabs[0],
        "",
        "Jane Doe",
        ""
    ));
    assert!(!commit_comment_buffer_for_exit(
        &mut tabs[0],
        "",
        "Jane Doe",
        "   "
    ));
    assert!(!tabs[0].dirty);
    let restored = exit_and_restore(&mut tabs, "exit-empty-comment");
    assert!(!restored[0].dirty);

    // Complement: on a cell WITH a comment, an empty buffer is a real delete
    // and marks the tab dirty, exactly as Enter would.
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (2, 2);
    }
    assert!(commit_comment_buffer_for_exit(
        &mut t,
        "",
        "Jane Doe",
        "Check this"
    ));
    // The delete must be what dirties: reset, then a fresh bar on the
    // commented cell, its buffer cleared, commits the delete at exit.
    t.dirty = false;
    assert!(commit_comment_buffer_for_exit(
        &mut t,
        "Check this",
        "Jane Doe",
        ""
    ));
    assert!(t.dirty);
    assert!(cell_comment(&t).is_none());

    // Enter's own path keeps its long-standing quirk, unchanged: an empty
    // commit there marks even a clean tab dirty. The exit variant exists so
    // the bar having been open cannot.
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (3, 3);
    }
    assert!(commit_comment_buffer(&mut t, "Jane Doe", ""));
    assert!(t.dirty);
}

#[test]
fn exit_does_not_edit_a_protected_sheet() {
    // Protected View and marked-as-final both refuse (Access::locked).
    for marked_final in [false, true] {
        let mut t = tab(Kind::Xlsx);
        t.access.protected = !marked_final;
        t.access.marked_final = marked_final;
        let old = {
            let Surface::Sheet(v) = &t.surface else {
                panic!()
            };
            v.pkg.workbook.sheets[0].name.clone()
        };
        {
            let Surface::Sheet(v) = &mut t.surface else {
                panic!()
            };
            v.sel = (0, 0);
        }
        assert!(!commit_comment_buffer_for_exit(
            &mut t,
            "",
            "Jane Doe",
            "Check this"
        ));
        assert!(!commit_rename_buffer(&mut t, 0, &format!("{old}X")));
        assert!(!t.dirty);
        assert!(cell_comment(&t).is_none());
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        assert_eq!(v.pkg.workbook.sheets[0].name, old);
    }
}

#[test]
fn exit_with_unchanged_rename_buffer_leaves_clean_tab_clean() {
    let mut t = tab(Kind::Xlsx);
    let name = {
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        v.pkg.workbook.sheets[0].name.clone()
    };
    // The rename bar seeds the buffer with the sheet's name; a bar merely
    // OPEN at exit must not dirty the tab or rewrite the workbook — the
    // taken-name check looks at the OTHER sheets, so `rename_sheet` itself
    // would take the same-name rename.
    assert!(!commit_rename_buffer_for_exit(&mut t, 0, &name));
    assert!(!commit_rename_buffer_for_exit(
        &mut t,
        0,
        &format!(" {name} ")
    ));
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, name);

    // A case-only rename is not "unchanged" — the trims differ — so it still
    // commits and is taken (only names the other sheets hold decline).
    let case = name.to_lowercase();
    assert_ne!(case, name);
    assert!(commit_rename_buffer_for_exit(&mut t, 0, &case));
    assert!(t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, case);
}

#[test]
fn exit_with_unchanged_prefilled_comment_keeps_author_undo_and_clean_tab() {
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (0, 0);
    }
    // A colleague's comment, opened in the bar to read: the buffer is seeded
    // with the note's text. Exiting with it untouched must not restamp the
    // author, spend an undo step, or dirty the tab — committing would do all
    // three for nothing.
    assert!(commit_comment_buffer(&mut t, "Jane Doe", "Original note"));
    t.dirty = false;
    assert!(!commit_comment_buffer_for_exit(
        &mut t,
        "Original note",
        "John Smith",
        "Original note"
    ));
    assert!(!commit_comment_buffer_for_exit(
        &mut t,
        "Original note",
        "John Smith",
        "  Original note  "
    ));
    assert!(!t.dirty);
    {
        let Surface::Sheet(v) = &t.surface else {
            panic!()
        };
        let cm = v
            .pkg
            .comments()
            .into_iter()
            .find(|cm| cm.sheet == v.active && cm.row == 0 && cm.col == 0)
            .expect("the comment");
        assert_eq!(cm.text, "Original note");
        assert_eq!(cm.author, "Jane Doe");
    }
    // Undo holds exactly the one step Jane's commit took: the skipped commit
    // added none (the second undo finds nothing).
    let Surface::Sheet(v) = &mut t.surface else {
        panic!()
    };
    assert!(v.undo_step());
    assert!(!v.undo_step());
    assert!(cell_comment(&t).is_none());
}

#[test]
fn exit_with_unchanged_rename_buffer_leaves_trailing_space_sheet_clean() {
    let mut t = tab(Kind::Xlsx);
    // A sheet name as a file can hold it — `rename_sheet` would trim it, so
    // the name is set on the model directly.
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.pkg.workbook.sheets[0].name = "Data ".into();
    }
    // The rename bar seeds the buffer with the raw name; a bar merely OPEN
    // must not rename "Data " to "Data" (rewriting formulas, dirtying the
    // tab). The skip compares both sides trimmed, so padding skips too.
    assert!(!commit_rename_buffer_for_exit(&mut t, 0, "Data "));
    assert!(!commit_rename_buffer_for_exit(&mut t, 0, "  Data  "));
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, "Data ");

    // A case-only rename is not "unchanged" — the trims differ — so it still
    // commits and is taken (only names the OTHER sheets hold decline).
    assert!(commit_rename_buffer_for_exit(&mut t, 0, "data"));
    assert!(t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, "data");
}

#[test]
fn exit_with_untouched_buffer_keeps_a_whitespace_note_intact() {
    // A file-loaded note keeps its whitespace; the bar seeds with the raw
    // text (written here straight into the model, the way a load leaves it).
    // An untouched buffer must not rewrite the note (trim it, restamp the
    // author) — and a whitespace-only note must not be deleted by the
    // empty-looking buffer.
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        assert!(v.pkg.set_comment(v.active, 0, 0, "Jane Doe", "note\n"));
        assert!(v.pkg.set_comment(v.active, 1, 1, "Jane Doe", "\n"));
        v.sel = (1, 1);
    }
    assert!(!commit_comment_buffer_for_exit(
        &mut t,
        "note\n",
        "John Smith",
        "note\n"
    ));
    assert!(!commit_comment_buffer_for_exit(
        &mut t,
        "\n",
        "John Smith",
        ""
    ));
    // The whitespace-only note trims to the empty buffer, so the commit is
    // skipped; were it committed it would land on the selection — the
    // whitespace-note cell — as Enter does.
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    let at = |r: u32, c: u32| {
        v.pkg
            .comments()
            .into_iter()
            .find(|cm| cm.sheet == v.active && cm.row == r && cm.col == c)
    };
    let note = at(0, 0).expect("the note");
    assert_eq!(note.text, "note\n");
    assert_eq!(note.author, "Jane Doe");
    assert!(at(1, 1).is_some(), "the whitespace-only note survives");
}

#[test]
fn exit_comment_commit_follows_the_selection_like_enter() {
    // The bar's label reads off the live selection ("Comment on C5:"), and
    // Enter/Save write there — the exit commit agrees: a CHANGED buffer
    // lands on the selected cell even when the bar opened elsewhere. Only an
    // untouched buffer (the seed) is skipped after a click.
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        v.sel = (4, 4); // clicked here after opening the bar on A1
    }
    assert!(commit_comment_buffer_for_exit(
        &mut t,
        "",
        "Jane Doe",
        "Typed note"
    ));
    assert!(t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    let at = |r: u32, c: u32| {
        v.pkg
            .comments()
            .iter()
            .any(|cm| cm.sheet == v.active && cm.row == r && cm.col == c)
    };
    assert!(at(4, 4), "on the selected cell, where Enter would write");
    assert!(!at(0, 0), "not on the bar's cell");
}

#[test]
fn exit_comment_bar_untouched_survives_a_moved_selection() {
    // A colleague's note on B2, opened in the bar to read; a click moved the
    // selection to C3. The buffer is the seed, so the commit is skipped; were
    // it committed it would land on the selection (C3), as Enter does.
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        assert!(v.pkg.set_comment(v.active, 1, 1, "Jane Doe", "B2 note"));
        v.sel = (2, 2);
    }
    assert!(!commit_comment_buffer_for_exit(
        &mut t,
        "B2 note",
        "John Smith",
        "B2 note"
    ));
    assert!(!t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    let at = |r: u32, c: u32| {
        v.pkg
            .comments()
            .iter()
            .any(|cm| cm.sheet == v.active && cm.row == r && cm.col == c)
    };
    assert!(at(1, 1), "B2 keeps its note, author unstamped");
    assert!(!at(2, 2), "C3 gets no copy");
}

#[test]
fn exit_comment_edge_whitespace_commit_lands_on_the_selection() {
    // Open on commented A1 (seed "X"), click empty B2, and type edge
    // whitespace around the text. Enter would create "X" on B2 — the exit
    // commit agrees: the trim-equal skip looks at the TARGET cell, which has
    // no note, so this is a change and it lands on the selection.
    let mut t = tab(Kind::Xlsx);
    {
        let Surface::Sheet(v) = &mut t.surface else {
            panic!()
        };
        assert!(v.pkg.set_comment(v.active, 0, 0, "Jane Doe", "X"));
        v.sel = (1, 1);
    }
    assert!(commit_comment_buffer_for_exit(
        &mut t, "X", "Jane Doe", " X "
    ));
    assert!(t.dirty);
    let Surface::Sheet(v) = &t.surface else {
        panic!()
    };
    let at = |r: u32, c: u32| {
        v.pkg
            .comments()
            .iter()
            .filter(|cm| cm.sheet == v.active && cm.row == r && cm.col == c)
            .count()
    };
    assert_eq!(
        at(1, 1),
        1,
        "one note on the selection, as Enter would write"
    );
    assert_eq!(at(0, 0), 1, "A1 keeps its own note, untouched");
}
