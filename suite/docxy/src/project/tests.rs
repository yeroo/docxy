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
    let paths: Vec<_> = std::fs::read_dir(corpus(""))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| ext_is(p, "xml"))
        .collect();
    assert_eq!(paths.len(), 17);
    for path in paths {
        let tab = tab_from_path(&path);
        assert!(tab.status.starts_with("loaded"), "{}", tab.status);
        let ed = &view(&tab).ed;
        for task in &ed.project().tasks {
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
    view_mut(&mut tabs[2]).col = 2;
    view_mut(&mut tabs[2]).open_cell(Some("invalid")).unwrap();
    let invalid_model = view(&tabs[2]).ed.project().clone();
    commit_project_cells_for_exit(&mut tabs);
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
        assert_eq!(row[0], "4");
        assert_eq!(row[1], "Child");
        assert_eq!(row[2], "1d");
        assert_eq!(row[5], expected);
        assert_eq!(row[6], "Alice");
        assert_eq!(row[3].len(), 10);
        ed.remove_predecessor(9, 7).unwrap();
    }
    ed.toggle_milestone(9).unwrap();
    assert_eq!(project_row(&ed, ed.project().task(9).unwrap())[2], "—");
    let mut p = ed.project().clone();
    p.tasks[1].predecessors.push(projcore::Predecessor {
        uid: 999,
        link: LinkType::FinishStart,
        lag_min: 0,
    });
    let ed = ProjectEditor::new(p);
    assert_eq!(project_row(&ed, ed.project().task(9).unwrap())[5], "?999");
}

#[test]
fn navigation_clamps_and_preserves_dirty_state_even_on_an_empty_project() {
    let mut t = new_project_tab();
    for key in ["up", "down", "home", "end"] {
        assert!(view_mut(&mut t).key(key, false));
        assert_eq!(view(&t).ed.sel(), 0);
    }
    for i in 0..100 {
        view_mut(&mut t)
            .ed
            .add_task(None, &format!("Task {i}"), 480)
            .unwrap();
    }
    for (key, selected) in [
        ("end", 99),
        ("down", 99),
        ("up", 98),
        ("home", 0),
        ("up", 0),
    ] {
        assert!(view_mut(&mut t).key(key, false));
        assert_eq!(view(&t).ed.sel(), selected);
    }
    assert!(view(&t).ed.dirty());
    assert!(!view_mut(&mut t).key("d", false));
}
