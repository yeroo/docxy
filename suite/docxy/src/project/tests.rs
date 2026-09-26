use super::*;
use core::prelude::v1::test;
use std::sync::atomic::{AtomicUsize, Ordering};

// No runtime environment reads: the suite's config-override test mutates env.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/project-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/mspdi")
        .join(name)
}
fn view(tab: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &tab.surface else {
        panic!("{}", tab.status)
    };
    v
}
fn view_mut(tab: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut tab.surface else {
        panic!("expected project")
    };
    v
}
fn edited_tab() -> DocTab {
    let mut t = project_tab_from_path(&corpus("11-resource-assignment.xml"));
    view_mut(&mut t).ed.rename(1, "Unsaved rename").unwrap();
    t.dirty = view(&t).ed.dirty();
    t
}
fn original(dir: &Scratch) -> PathBuf {
    let path = dir.path("original.xml");
    std::fs::copy(corpus("11-resource-assignment.xml"), &path).unwrap();
    path
}
fn mpp_stub(dir: &Scratch) -> PathBuf {
    let path = dir.path("imported.MPP");
    std::fs::write(&path, mppread::write_cfb(&[("Props", vec![0; 12])])).unwrap();
    path
}
fn persisted(path: Option<&Path>, hot: Option<&Path>, dirty: bool) -> PersistTab {
    PersistTab {
        kind: Kind::Project,
        title: "Schedule".into(),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        hot: hot.map(|p| p.to_string_lossy().into_owned()),
        dirty,
        markdown: false,
        load_failed: None,
    }
}

#[test]
fn new_project_is_empty_and_clean() {
    let tab = new_project_tab();
    assert!(tab.kind == Kind::Project);
    assert_eq!(tab.title.as_ref(), "Untitled.yppx");
    assert!(tab.path.is_none());
    assert!(!tab.dirty && !view(&tab).ed.dirty());
    assert!(view(&tab).ed.project().tasks.is_empty());
}

#[test]
fn extensions_route_to_project_and_mpp_is_imported() {
    let dir = Scratch::new();
    let ed = ProjectEditor::new(untitled_project());
    for name in ["x.YPPX", "x.XML"] {
        let path = dir.path(name);
        write_project(&ed, &path).unwrap();
        let tab = tab_from_path(&path);
        assert!(tab.kind == Kind::Project);
        assert!(tab.status.starts_with("loaded"));
        assert!(!is_imported(&tab));
    }
    let path = mpp_stub(&dir);
    let tab = tab_from_path(&path);
    assert!(tab.kind == Kind::Project);
    assert!(is_imported(&tab));
    assert_eq!(
        tab.status.as_ref(),
        "loaded — 0 tasks, imported from .mpp; Save As .yppx or MSPDI to keep edits"
    );
    assert!(view(&tab).ed.project().tasks.is_empty());
    assert!(tab_from_path(&dir.path("x.docx")).kind == Kind::Docx);
}

#[test]
fn every_mspdi_fixture_opens_and_matches_its_date_oracle() {
    // The corpus generator keeps manifest.json in step with the fixtures, so the
    // expected set comes from there instead of a count that each new fixture breaks.
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(corpus("manifest.json")).unwrap()).unwrap();
    let mut listed: Vec<String> = manifest["files"]
        .as_array()
        .expect("manifest.json has a files array")
        .iter()
        .map(|f| f["file"].as_str().expect("files[].file").to_owned())
        .collect();
    listed.sort();
    let mut paths: Vec<_> = std::fs::read_dir(corpus(""))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| ext_is(p, "xml"))
        .collect();
    paths.sort();
    let on_disk: Vec<String> = paths.iter().map(|p| file_name(p)).collect();
    assert!(!listed.is_empty(), "manifest.json lists no fixtures");
    assert_eq!(
        on_disk, listed,
        "corpus/mspdi fixtures differ from manifest.json"
    );
    for path in paths {
        let tab = tab_from_path(&path);
        assert!(tab.status.starts_with("loaded"), "{}", tab.status);
        let ed = &view(&tab).ed;
        // Blank rows (#80) are not scheduled.
        for task in ed.project().tasks.iter().filter(|t| !t.is_null) {
            let actual = ed.schedule().get(task.uid).unwrap();
            if let Some(expected) = task.stored_start {
                assert_eq!(
                    actual.early_start,
                    expected,
                    "{} task {}",
                    path.display(),
                    task.uid
                );
            }
            if let Some(expected) = task.stored_finish {
                assert_eq!(
                    actual.early_finish,
                    expected,
                    "{} task {}",
                    path.display(),
                    task.uid
                );
            }
        }
    }
}

#[test]
fn failed_loads_are_unsaveable_and_do_not_overwrite_the_input() {
    let dir = Scratch::new();
    for (name, bytes) in [
        ("other.xml", b"<foo/>".as_slice()),
        ("empty.xml", b""),
        ("text.xml", b"hello"),
        ("bad.yppx", b"bad"),
    ] {
        let path = dir.path(name);
        std::fs::write(&path, bytes).unwrap();
        let mut tab = tab_from_path(&path);
        assert!(matches!(tab.surface, Surface::Placeholder));
        assert!(!tab.status.starts_with("loaded"));
        assert_eq!(save_decision(&tab, false, false), SaveDecision::Unsaveable);
        assert_eq!(save_decision(&tab, false, true), SaveDecision::Unsaveable);
        assert!(apply_save(&mut tab, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    let tab = tab_from_path(&dir.path("missing.yppx"));
    assert!(matches!(tab.surface, Surface::Placeholder));
    assert!(!tab.status.starts_with("loaded"));
}

#[test]
fn save_target_validates_extension_before_writing() {
    for name in ["a.yppx", "a.XML"] {
        assert_eq!(save_target(Path::new(name)).unwrap(), Path::new(name));
    }
    assert_eq!(save_target(Path::new("a")).unwrap(), Path::new("a.yppx"));
    for name in ["a.mpp", "a.MPP", "a.docx"] {
        assert!(save_target(Path::new(name)).is_err());
    }
    let dir = Scratch::new();
    let source = mpp_stub(&dir);
    let before = std::fs::read(&source).unwrap();
    assert!(write_project(&view(&new_project_tab()).ed, &source).is_err());
    assert_eq!(std::fs::read(source).unwrap(), before);
}

#[test]
fn saved_formats_round_trip_the_complete_project() {
    let dir = Scratch::new();
    let tab = project_tab_from_path(&corpus("11-resource-assignment.xml"));
    for name in ["saved.yppx", "saved.XML", "extensionless"] {
        let (path, n) = write_project(&view(&tab).ed, &dir.path(name)).unwrap();
        assert!(n > 0);
        assert_eq!(&project_from_path(&path).unwrap(), view(&tab).ed.project());
    }
}

#[test]
fn save_decisions_cover_in_place_dialog_and_harness_refusal() {
    let mut t = new_project_tab();
    for name in [None, Some("a.mpp"), Some("a.yppx"), Some("a.XML")] {
        t.path = name.map(PathBuf::from);
        for explicit in [false, true] {
            let dialog = explicit || name.is_none() || name == Some("a.mpp");
            assert_eq!(
                matches!(
                    save_decision(&t, false, explicit),
                    SaveDecision::Dialog { .. }
                ),
                dialog
            );
            assert_eq!(
                matches!(
                    save_decision(&t, true, explicit),
                    SaveDecision::RefuseHarness(_)
                ),
                dialog
            );
            if !dialog {
                assert_eq!(
                    save_decision(&t, true, explicit),
                    SaveDecision::InPlace(PathBuf::from(name.unwrap()))
                );
            }
        }
    }
}

#[test]
fn cancelled_rejected_and_failed_save_preserve_the_import_binding_and_dirty_flags() {
    let dir = Scratch::new();
    let path = mpp_stub(&dir);
    let bytes = std::fs::read(&path).unwrap();
    let mut tab = edited_tab();
    tab.path = Some(path.clone());
    tab.title = "imported.MPP".into();
    let model = view(&tab).ed.project().clone();
    for target in [
        None,
        Some(path.clone()),
        Some(dir.path("missing-dir/file.yppx")),
    ] {
        finish_project_save(&mut tab, target.as_deref());
        assert_eq!(tab.path.as_ref(), Some(&path));
        assert_eq!(tab.title.as_ref(), "imported.MPP");
        assert!(tab.dirty && view(&tab).ed.dirty() && is_imported(&tab));
        assert_eq!(view(&tab).ed.project(), &model);
        assert!(!tab.status.starts_with("saved"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn successful_save_publishes_actual_binding_and_clears_both_dirty_flags() {
    let dir = Scratch::new();
    let path = mpp_stub(&dir);
    let bytes = std::fs::read(&path).unwrap();
    let mut tab = edited_tab();
    tab.path = Some(path.clone());
    apply_save(&mut tab, &dir.path("converted")).unwrap();
    assert_eq!(tab.path, Some(dir.path("converted.yppx")));
    assert_eq!(tab.title.as_ref(), "converted.yppx");
    assert!(!tab.dirty && !view(&tab).ed.dirty() && !is_imported(&tab));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        project_from_path(tab.path.as_ref().unwrap()).unwrap(),
        *view(&tab).ed.project()
    );
    let path = original(&dir);
    tab.path = Some(path.clone());
    view_mut(&mut tab).ed.rename(1, "XML in place").unwrap();
    tab.dirty = true;
    apply_save(&mut tab, &path).unwrap();
    assert_eq!(tab.path.as_ref(), Some(&path));
    assert!(!tab.dirty && !view(&tab).ed.dirty());
    assert_eq!(
        project_from_path(&path).unwrap().tasks[0].name,
        "XML in place"
    );
}

#[test]
fn hot_exit_round_trips_dirty_clean_untitled_and_imported_sessions() {
    let dir = Scratch::new();
    for (i, (dirty, path)) in [
        (true, Some(original(&dir))),
        (false, Some(original(&dir))),
        (true, None),
        (true, Some(mpp_stub(&dir))),
    ]
    .into_iter()
    .enumerate()
    {
        let mut tab = edited_tab();
        tab.path = path.clone();
        if !dirty {
            view_mut(&mut tab).ed.mark_saved();
            tab.dirty = false;
        }
        let pt = persist_tab(&dir.0, i, &tab);
        assert!(pt.hot.as_deref().unwrap().ends_with(".yppx"));
        let json = serde_json::to_vec(&Session {
            tabs: vec![pt],
            ..Session::default()
        })
        .unwrap();
        let session: Session = serde_json::from_slice(&json).unwrap();
        assert!(session.tabs[0].kind == Kind::Project);
        let mut restored = restore_tab(&session.tabs[0]);
        assert_eq!(restored.path, path);
        assert_eq!(restored.dirty, dirty);
        assert_eq!(view(&restored).ed.dirty(), dirty);
        assert_eq!(view(&restored).ed.project(), view(&tab).ed.project());
        assert_eq!(view(&restored).ed.undo_depth(), 0);
        let restored_status = restored.status.clone();
        for act in [ProjectAct::Undo, ProjectAct::Redo] {
            apply_project_act(&mut restored, act);
            assert_eq!(restored.dirty, dirty);
            assert_eq!(view(&restored).ed.dirty(), dirty);
        }
        if is_imported(&tab) {
            assert!(is_imported(&restored));
            assert!(restored_status.contains("imported from .mpp; Save As"));
        }
    }
}

#[test]
fn hot_exit_commits_all_valid_buffers_and_preserves_models_on_invalid_input() {
    let dir = Scratch::new();
    let mut tabs: Vec<_> = (0..3)
        .map(|_| project_tab_from_path(&corpus("11-resource-assignment.xml")))
        .collect();
    for (tab, name) in tabs[..2]
        .iter_mut()
        .zip(["Pending active", "Pending inactive"])
    {
        view_mut(tab).open_cell(Some(name)).unwrap();
    }
    view_mut(&mut tabs[2]).col = COL_DURATION;
    view_mut(&mut tabs[2]).open_cell(Some("invalid")).unwrap();
    let invalid_model = view(&tabs[2]).ed.project().clone();
    crate::close::commit_pending_for_exit(&mut tabs);
    for (i, tab) in tabs.iter().enumerate() {
        let saved = persist_tab(&dir.0, i, tab);
        let restored = restore_project_tab(&saved);
        assert_eq!(view(&restored).ed.project(), view(tab).ed.project());
        if i < 2 {
            assert!(view(tab).cell.is_none());
            assert!(restored.dirty);
            assert_eq!(
                view(&restored).ed.project().tasks[0].name,
                if i == 0 {
                    "Pending active"
                } else {
                    "Pending inactive"
                }
            );
        } else {
            assert_eq!(view(&restored).ed.project(), &invalid_model);
            assert_eq!(view(tab).cell.as_ref().unwrap().buf, "invalid");
        }
    }
}

#[test]
fn recovery_reports_lost_content_for_every_unavailable_sidecar_case() {
    let dir = Scratch::new();
    let orig = original(&dir);
    let missing_orig = dir.path("missing-original.xml");
    let missing = dir.path("missing.yppx");
    let corrupt = dir.path("corrupt.yppx");
    std::fs::write(&corrupt, b"not a package").unwrap();
    for (hot, reason) in [
        (None, "no sidecar recorded"),
        (Some(missing.as_path()), "sidecar missing"),
        (Some(corrupt.as_path()), "sidecar unreadable"),
    ] {
        for path in [Some(orig.as_path()), Some(missing_orig.as_path()), None] {
            let clean = restore_tab(&persisted(path, hot, false));
            assert!(!clean.dirty);
            assert!(!clean.status.contains("unsaved edits lost"));
            assert_eq!(clean.path.as_deref(), path);
            match path {
                Some(p) if p == orig => {
                    assert!(!view(&clean).ed.dirty());
                    assert!(clean.status.starts_with("loaded"));
                    assert_eq!(view(&clean).ed.project().tasks[0].name, "Build");
                }
                Some(_) => {
                    assert!(matches!(clean.surface, Surface::Placeholder));
                    assert!(clean.status.starts_with("project load error:"));
                    assert_eq!(
                        save_decision(&clean, false, false),
                        SaveDecision::Unsaveable
                    );
                }
                None => {
                    assert!(!view(&clean).ed.dirty());
                    assert!(view(&clean).ed.project().tasks.is_empty());
                    assert_eq!(clean.title.as_ref(), "Untitled.yppx");
                    assert_eq!(clean.status.as_ref(), "new project");
                }
            }
            let tab = restore_tab(&persisted(path, hot, true));
            assert!(!tab.dirty);
            assert!(tab.status.starts_with(reason), "{}", tab.status);
            assert!(tab.status.contains("unsaved edits lost"));
            if path == Some(orig.as_path()) {
                assert!(!view(&tab).ed.dirty());
                assert!(tab.status.contains("reloaded"));
                assert_eq!(view(&tab).ed.project().tasks[0].name, "Build");
            } else {
                assert!(matches!(tab.surface, Surface::Placeholder));
                assert!(!tab.status.contains("reloaded"));
                assert_eq!(save_decision(&tab, false, false), SaveDecision::Unsaveable);
            }
        }
    }
}

#[test]
fn failed_sidecar_write_enters_recovery_instead_of_claiming_dirty_content() {
    let dir = Scratch::new();
    let blocker = dir.path("regular-file");
    std::fs::write(&blocker, b"block directory creation").unwrap();
    let orig = original(&dir);
    for path in [Some(orig), Some(dir.path("missing.xml")), None] {
        let mut t = edited_tab();
        t.path = path;
        let saved = persist_tab(&blocker.join("hot"), 0, &t);
        assert!(saved.hot.is_none() && saved.dirty);
        let restored = restore_tab(&saved);
        assert!(!restored.dirty);
        assert!(restored.status.starts_with("no sidecar recorded"));
        match restored.surface {
            Surface::Project(v) => {
                assert!(!v.ed.dirty());
                assert_eq!(v.ed.project().tasks[0].name, "Build");
            }
            Surface::Placeholder => {}
            _ => panic!("wrong kind"),
        }
    }
}

#[test]
fn clean_sessions_without_a_sidecar_load_original_or_start_empty() {
    let dir = Scratch::new();
    let tab = restore_tab(&persisted(Some(&original(&dir)), None, false));
    assert!(!tab.dirty && !view(&tab).ed.dirty());
    assert_eq!(view(&tab).ed.project().tasks[0].name, "Build");
    let tab = restore_tab(&persisted(None, None, false));
    assert!(tab.path.is_none());
    assert!(view(&tab).ed.project().tasks.is_empty());
}

#[test]
fn rows_resolve_ids_format_links_milestones_and_resources() {
    let mut p = untitled_project();
    p.tasks = vec![
        Task {
            uid: 7,
            id: 3,
            name: "Parent".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        },
        Task {
            uid: 9,
            id: 4,
            name: "Child".into(),
            outline_level: 2,
            duration_min: 480,
            ..Task::default()
        },
    ];
    let mut ed = ProjectEditor::new(p);
    ed.assign_resource(9, "Alice").unwrap();
    assert!(ed.project().tasks[0].summary);
    for (link, lag, expected) in [
        (LinkType::FinishStart, 0, "3"),
        (LinkType::StartStart, 0, "3SS"),
        (LinkType::FinishStart, 960, "3FS+2d"),
        (LinkType::FinishStart, -480, "3FS-1d"),
        (LinkType::FinishFinish, 0, "3FF"),
        (LinkType::StartFinish, 0, "3SF"),
    ] {
        ed.add_predecessor(9, 7, link, lag).unwrap();
        let row = project_row(&ed, ed.project().task(9).unwrap());
        assert_eq!(row[COL_ID], "4");
        assert_eq!(row[COL_NAME], "Child");
        assert_eq!(row[COL_DURATION], "1d");
        assert_eq!(row[COL_PREDECESSORS], expected);
        assert_eq!(row[COL_RESOURCES], "Alice");
        assert_eq!(row[COL_START].len(), 10);
        ed.remove_predecessor(9, 7).unwrap();
    }
    ed.toggle_milestone(9).unwrap();
    assert_eq!(
        project_row(&ed, ed.project().task(9).unwrap())[COL_DURATION],
        "—"
    );
    let mut p = ed.project().clone();
    p.tasks[1].predecessors.push(projcore::Predecessor::fs(999));
    let ed = ProjectEditor::new(p);
    assert_eq!(
        project_row(&ed, ed.project().task(9).unwrap())[COL_PREDECESSORS],
        "?999"
    );
}

#[test]
fn a_blank_row_shows_only_its_id() {
    let xml = std::fs::read_to_string(corpus("20-task-fields.xml")).unwrap();
    let ed = ProjectEditor::new(mspdi::read_mspdi(&xml).unwrap());
    let blank = ed.project().task(3).unwrap();
    assert!(blank.is_null);
    assert_eq!(project_row(&ed, blank), ["3", "", "", "", "", "", "", ""]);
    let pour = project_row(&ed, ed.project().task(4).unwrap());
    // Pour's link from the blank row is kept but does not drive it.
    assert_eq!(
        (pour[COL_NAME].as_str(), pour[COL_PREDECESSORS].as_str()),
        ("Pour", "2, 3")
    );
    assert_eq!(pour[COL_START], "2026-03-04");
}

fn summary_fixture(stored_summary_min: Option<i64>) -> ProjectEditor {
    let xml = std::fs::read_to_string(corpus("10-summary.xml")).unwrap();
    let mut p = mspdi::read_mspdi(&xml).unwrap();
    assert!(p.tasks[0].summary);
    if let Some(min) = stored_summary_min {
        p.tasks[0].duration_min = min;
    }
    ProjectEditor::new(p)
}

#[test]
fn summary_duration_follows_child_edits_and_matches_the_gantt_export() {
    let mut ed = summary_fixture(None);
    let phase = ed.project().tasks[0].uid;
    let b = ed.project().tasks[2].uid;
    assert_eq!(ed.project().tasks[2].name, "B");
    assert_eq!(
        project_row(&ed, ed.project().task(phase).unwrap())[COL_DURATION],
        "2d"
    );
    ed.set_duration_min(b, 1440).unwrap();
    let row = project_row(&ed, ed.project().task(phase).unwrap());
    assert_eq!(row[COL_DURATION], "4d");
    let md = projcore::gantt::to_markdown(ed.project(), ed.schedule());
    let exported = md
        .lines()
        .find(|l| l.starts_with("| **Phase** |"))
        .unwrap()
        .split('|')
        .map(str::trim)
        .nth(4)
        .unwrap();
    assert_eq!(row[COL_DURATION], exported);
}

#[test]
fn summary_with_zero_stored_duration_is_not_shown_as_a_milestone() {
    let ed = summary_fixture(Some(0));
    let phase = ed.project().task(ed.project().tasks[0].uid).unwrap();
    assert!(phase.is_milestone());
    assert_eq!(project_row(&ed, phase)[COL_DURATION], "2d");
}

#[test]
fn navigation_clamps_and_preserves_dirty_state_even_on_an_empty_project() {
    let mut t = new_project_tab();
    for key in ["up", "down", "home", "end"] {
        assert!(view_mut(&mut t).key(key, false));
        assert_eq!(view(&t).ed.sel(), 0);
        // An empty plan has only the entry row.
        assert!(view(&t).on_entry_row());
        assert_eq!(view(&t).cursor_row(), 0);
    }
    for i in 0..100 {
        view_mut(&mut t)
            .ed
            .add_task(None, &format!("Task {i}"), 480)
            .unwrap();
    }
    // Down from the last task goes to the entry row (100), as in Project;
    // Up from there goes back to the last task.
    for (key, cursor, selected) in [
        ("end", 99, 99),
        ("down", 100, 99),
        ("down", 100, 99),
        ("up", 99, 99),
        ("up", 98, 98),
        ("down", 99, 99),
        ("down", 100, 99),
        ("end", 99, 99),
        ("down", 100, 99),
        ("home", 0, 0),
        ("up", 0, 0),
    ] {
        assert!(view_mut(&mut t).key(key, false));
        assert_eq!(
            (view(&t).cursor_row(), view(&t).ed.sel()),
            (cursor, selected),
            "{key}"
        );
    }
    assert!(view(&t).ed.dirty());
    assert!(!view_mut(&mut t).key("d", false));
}

#[test]
fn the_task_mode_column_names_each_tasks_mode_and_is_blank_on_a_blank_row() {
    let xml = std::fs::read_to_string(corpus("20-task-fields.xml")).unwrap();
    let mut ed = ProjectEditor::new(mspdi::read_mspdi(&xml).unwrap());
    let pour = ed.project().task(4).unwrap().uid;
    assert_eq!(
        project_row(&ed, ed.project().task(pour).unwrap())[COL_MODE],
        "Auto Scheduled"
    );
    ed.set_manual(pour, true).unwrap();
    assert_eq!(
        project_row(&ed, ed.project().task(pour).unwrap())[COL_MODE],
        "Manually Scheduled"
    );
    assert_eq!(
        project_row(&ed, ed.project().task(3).unwrap())[COL_MODE],
        ""
    );
}
