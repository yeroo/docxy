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
    v.editing = Some(text.into());
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
fn open_hf(t: &mut DocTab, is_header: bool, text: &str) -> String {
    let part_name = t
        .pkg
        .as_mut()
        .unwrap()
        .create_hf(is_header, "default")
        .unwrap();
    let mut editor = Editor::new(empty_doc());
    editor.insert_str(text);
    t.hf_edit = Some(HfEdit {
        editor,
        part_name: part_name.clone(),
        is_header,
        variant: "default",
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
    }
    assert!(!restored[0].dirty);
}

#[test]
fn window_close_does_not_stop_at_an_invalid_project_buffer() {
    let mut project = tab(Kind::Project);
    project_cell_click(&mut project, 1, Some(2), false);
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
    assert!(restored[2].dirty);
}

/// A saved .docx whose header was written by another tool (extra namespace,
/// its own whitespace), loaded clean, with the header editor opened on it the
/// way `enter_hf` opens it and nothing typed.
fn untouched_existing_header(name: &str) -> (DocTab, String, Vec<u8>) {
    let mut source = tab(Kind::Docx);
    let pkg = source.pkg.as_mut().unwrap();
    let part_name = pkg.create_hf(true, "default").unwrap();
    let word_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
        <w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
        xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">\r\n  \
        <w:p><w:r><w:t>Existing header</w:t></w:r></w:p>\r\n</w:hdr>";
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
    let part_name = hf_part_name_typed(pkg, true, "default").unwrap();
    let before = pkg.part(&part_name).unwrap().to_vec();
    assert_eq!(before, word_xml.as_bytes());
    let body = parse_hf_part(pkg, &part_name);
    t.hf_edit = Some(HfEdit {
        editor: Editor::new(docxcore::model::Document { body }),
        part_name: part_name.clone(),
        is_header: true,
        variant: "default",
    });
    (t, part_name, before)
}

#[test]
fn window_close_leaves_an_untouched_header_editor_clean_and_its_part_intact() {
    let (first, part_name, before) = untouched_existing_header("untouched-0");
    let (last, _, _) = untouched_existing_header("untouched-2");
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
    v.editing = Some(v.edit_string(0, 0));
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
        markdown: false,
        load_failed: Some(true),
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
