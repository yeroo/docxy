use super::*;
use core::prelude::v1::test;
use projcore::LinkType;
use std::sync::atomic::{AtomicUsize, Ordering};

fn args(text: &str) -> Json {
    Json::parse(text).unwrap()
}
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/gantt-indent.xml")
}
fn tab() -> DocTab {
    project_tab_from_path(&fixture())
}
fn view(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!("Project expected")
    };
    v
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!("Project expected")
    };
    v
}
fn word() -> DocTab {
    sample_doc().into_tab(Kind::Docx, "Word.docx".into(), None, false)
}
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/control-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn copied(dir: &Path) -> PathBuf {
    let p = dir.join("schedule.xml");
    std::fs::copy(fixture(), &p).unwrap();
    p
}
fn path_args(path: &Path) -> Json {
    Json::obj(vec![(
        "path",
        Json::Str(path.to_string_lossy().into_owned()),
    )])
}
fn call(
    tabs: &mut Vec<DocTab>,
    active: usize,
    verb: &str,
    a: Json,
) -> Result<(Json, Effect), String> {
    project_verb(tabs, active, verb, &a).unwrap_or_else(|| Err(format!("unknown verb '{verb}'")))
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    project: projcore::Project,
    dirty: (bool, bool),
    history: (usize, usize),
    selected: usize,
    prompt: String,
    query: String,
    leveled: bool,
    scroll: Point<Pixels>,
    offsets: (f32, f32),
    reveal: Option<usize>,
    path: Option<PathBuf>,
    title: String,
    status: String,
}
fn snapshot(t: &DocTab) -> Snapshot {
    let v = view(t);
    Snapshot {
        project: v.ed.project().clone(),
        dirty: (t.dirty, v.ed.dirty()),
        history: (v.ed.undo_depth(), v.ed.redo_depth()),
        selected: v.ed.sel(),
        prompt: format!("{:?}", v.prompt),
        query: v.ed.find_query().into(),
        leveled: v.ed.leveled(),
        scroll: v.scroll.0.borrow().base_handle.offset(),
        offsets: (v.table_x, v.gantt_x),
        reveal: v
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .map(|r| r.item_index),
        path: t.path.clone(),
        title: t.title.to_string(),
        status: t.status.to_string(),
    }
}

#[test]
fn selector_distinguishes_indices_paths_ambiguity_and_failed_projects() {
    let mut tabs = vec![word(), tab(), tab(), new_project_tab()];
    tabs[1].path = Some(PathBuf::from("alpha/first.xml"));
    tabs[1].title = "Same.xml".into();
    tabs[2].path = Some(PathBuf::from("beta/second.xml"));
    tabs[2].title = "Same.xml".into();
    tabs[3].surface = Surface::Placeholder;
    tabs[3].title = "Broken.xml".into();
    assert_eq!(resolve_project_tab(&tabs, 1, None), Ok(1));
    assert_eq!(
        resolve_project_tab(&tabs, 0, None).unwrap_err(),
        "the active tab is not a Project"
    );
    assert_eq!(resolve_project_tab(&tabs, 0, Some(&args("1"))), Ok(1));
    assert_eq!(
        resolve_project_tab(&tabs, 0, Some(&args("\"ALPHA\""))),
        Ok(1)
    );
    assert_eq!(
        resolve_project_tab(&tabs, 0, Some(&args("\"second\""))),
        Ok(2)
    );
    assert_eq!(
        resolve_project_tab(&tabs, 0, Some(&args("\"same\""))).unwrap_err(),
        "several Project tabs match 'same' (1, 2)"
    );
    assert_eq!(
        resolve_project_tab(&tabs, 0, Some(&args("\"absent\""))).unwrap_err(),
        "no Project tab matches 'absent'"
    );
    for a in [args("3"), args("\"broken\"")] {
        assert_eq!(
            resolve_project_tab(&tabs, 0, Some(&a)).unwrap_err(),
            "that tab could not be loaded"
        );
    }
    assert_eq!(
        resolve_project_tab(&tabs, 0, Some(&args("0"))).unwrap_err(),
        "tab 0 is not a Project"
    );
    assert_eq!(
        resolve_project_tab(&tabs, 0, Some(&args("9"))).unwrap_err(),
        "no tab at index 9"
    );
    for text in [
        "-1",
        "1.5",
        "1e100",
        "18446744073709551616",
        "null",
        "true",
        "[]",
        "{}",
        "\"\"",
    ] {
        assert_eq!(
            resolve_project_tab(&tabs, 0, Some(&args(text))).unwrap_err(),
            "'tab' must be a tab index or a title/path substring",
            "{text}"
        );
    }
    assert!(resolve_project_tab(&[], 0, None).is_err());
}

#[test]
fn reads_and_rejected_edits_preserve_prompt_selection_history_and_scroll() {
    let mut tabs = vec![tab()];
    vm(&mut tabs[0]).ed.rename(1, "Change").unwrap();
    vm(&mut tabs[0]).ed.undo();
    vm(&mut tabs[0]).ed.mark_saved();
    vm(&mut tabs[0]).ed.select(1);
    vm(&mut tabs[0]).gantt_x = 20.;
    vm(&mut tabs[0]).open_prompt(PromptKind::Rename);
    let before = snapshot(&tabs[0]);
    for (verb, a) in [
        ("proj.path", Json::Null),
        ("task.list", Json::Null),
        ("task.get", args(r#"{"uid":1}"#)),
        ("find", args(r#"{"query":"Task"}"#)),
    ] {
        let (_, effect) = call(&mut tabs, 0, verb, a).unwrap();
        assert_eq!(effect, Effect::default());
        assert_eq!(snapshot(&tabs[0]), before);
    }
    for (verb, a) in [
        (
            "task.set",
            args(r#"{"uid":1,"name":"bad","duration":"banana"}"#),
        ),
        ("task.del", args(r#"{"uid":999}"#)),
        ("link.add", args(r#"{"uid":1,"pred":999}"#)),
    ] {
        assert!(call(&mut tabs, 0, verb, a).is_err());
        assert_eq!(snapshot(&tabs[0]), before);
    }
    let (info, _) = call(&mut tabs, 0, "proj.path", Json::Null).unwrap();
    let Json::Obj(fields) = info else { panic!() };
    assert_eq!(
        fields.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        [
            "path", "modified", "name", "tasks", "start", "finish", "tab", "imported"
        ]
    );
}

#[test]
fn each_agent_edit_is_undoable_and_changes_only_its_inactive_target() {
    for (verb, text) in [
        ("task.add", r#"{"tab":1,"name":"Agent","duration":"3d"}"#),
        (
            "task.set",
            r#"{"tab":1,"uid":1,"name":"Changed","duration":"3d"}"#,
        ),
        ("task.del", r#"{"tab":1,"uid":1}"#),
        ("link.add", r#"{"tab":1,"uid":2,"pred":1}"#),
        ("link.del", r#"{"tab":1,"uid":2,"pred":1}"#),
    ] {
        let mut tabs = vec![tab(), tab()];
        if verb == "link.del" {
            vm(&mut tabs[1])
                .ed
                .add_predecessor(2, 1, LinkType::FinishStart, 0)
                .unwrap();
        }
        vm(&mut tabs[0]).open_prompt(PromptKind::Rename);
        vm(&mut tabs[1]).open_prompt(PromptKind::Rename);
        let untouched = snapshot(&tabs[0]);
        let before = view(&tabs[1]).ed.project().clone();
        let depth = view(&tabs[1]).ed.undo_depth();
        let (_, effect) = call(&mut tabs, 0, verb, args(text)).unwrap();
        assert_eq!(
            effect,
            Effect {
                repaint: true,
                activity: true,
                focus: None
            }
        );
        assert_eq!(snapshot(&tabs[0]), untouched);
        assert!(tabs[1].dirty && view(&tabs[1]).ed.dirty());
        assert!(view(&tabs[1]).prompt.is_none());
        assert_eq!(view(&tabs[1]).ed.undo_depth(), depth + 1);
        assert_ne!(view(&tabs[1]).ed.project(), &before);
        apply_project_act(&mut tabs[1], ProjectAct::Undo);
        assert_eq!(view(&tabs[1]).ed.project(), &before);
    }
}

#[test]
fn normal_surface_recognizes_no_harness_verbs_even_without_a_project() {
    let mut tabs = vec![word()];
    for verb in [
        "open",
        "key",
        "type",
        "quit",
        "selection",
        "rect",
        "proj.unknown",
    ] {
        assert!(project_verb(&mut tabs, 0, verb, &Json::Null).is_none());
        assert_eq!(
            call(&mut tabs, 0, verb, Json::Null).unwrap_err(),
            format!("unknown verb '{verb}'")
        );
    }
}

#[test]
fn save_policy_preserves_binding_history_and_source_on_refusal() {
    let dir = scratch();
    let source = copied(&dir);
    let bytes = std::fs::read(&source).unwrap();
    let mut tabs = vec![project_tab_from_path(&source)];
    apply_project_act(&mut tabs[0], ProjectAct::Baseline);
    vm(&mut tabs[0]).open_prompt(PromptKind::Rename);
    let before = snapshot(&tabs[0]);
    for target in [dir.join("bad.mpp"), source.join("bad.xml")] {
        assert!(call(&mut tabs, 0, "proj.save", path_args(&target)).is_err());
        assert_eq!(snapshot(&tabs[0]), before);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }
    let (_, effect) = call(&mut tabs, 0, "proj.save", Json::Null).unwrap();
    assert!(effect.repaint && !effect.activity && effect.focus.is_none());
    assert!(!tabs[0].dirty && !view(&tabs[0]).ed.dirty());
    assert_eq!(view(&tabs[0]).ed.undo_depth(), before.history.0);
    assert!(view(&tabs[0]).prompt.is_none());
    assert!(!project_tab_from_path(&source).dirty);
    let target = dir.join("converted");
    call(&mut tabs, 0, "proj.save", path_args(&target)).unwrap();
    assert_eq!(tabs[0].path, Some(target.with_extension("yppx")));
    assert_eq!(
        view(&project_tab_from_path(tabs[0].path.as_ref().unwrap()))
            .ed
            .project(),
        view(&tabs[0]).ed.project()
    );
    for path in [None, Some(dir.join("original.mpp"))] {
        if let Some(p) = &path {
            std::fs::write(p, b"original imported bytes").unwrap();
        }
        tabs[0].path = path;
        apply_project_act(&mut tabs[0], ProjectAct::Baseline);
        let before = snapshot(&tabs[0]);
        assert_eq!(
            call(&mut tabs, 0, "proj.save", Json::Null).unwrap_err(),
            "pass \"path\" to save this project (.yppx or .xml)"
        );
        assert_eq!(snapshot(&tabs[0]), before);
        let (info, _) = call(&mut tabs, 0, "proj.path", Json::Null).unwrap();
        assert_eq!(
            info.get("imported").and_then(Json::as_bool),
            Some(tabs[0].path.is_some())
        );
        if let Some(p) = &tabs[0].path {
            assert_eq!(std::fs::read(p).unwrap(), b"original imported bytes");
        }
    }
}

#[test]
fn reload_commits_only_a_successful_load() {
    let dir = scratch();
    let source = copied(&dir);
    let original = std::fs::read(&source).unwrap();
    let mut tabs = vec![project_tab_from_path(&source)];
    vm(&mut tabs[0]).ed.rename(1, "unsaved").unwrap();
    vm(&mut tabs[0]).ed.toggle_level();
    vm(&mut tabs[0]).ed.find("unsaved");
    vm(&mut tabs[0]).layout(400.);
    vm(&mut tabs[0]).table_x = 20.;
    vm(&mut tabs[0]).gantt_x = 10.;
    let scroll = view(&tabs[0]).scroll.clone();
    scroll
        .0
        .borrow()
        .base_handle
        .set_offset(point(px(0.), px(-24.)));
    tabs[0].dirty = true;
    vm(&mut tabs[0]).open_prompt(PromptKind::Rename);
    let before = snapshot(&tabs[0]);
    std::fs::remove_file(&source).unwrap();
    assert!(call(&mut tabs, 0, "proj.reload", Json::Null).is_err());
    assert_eq!(snapshot(&tabs[0]), before);
    std::fs::write(&source, "bad XML").unwrap();
    assert!(call(&mut tabs, 0, "proj.reload", Json::Null).is_err());
    assert_eq!(snapshot(&tabs[0]), before);
    std::fs::write(&source, original).unwrap();
    call(&mut tabs, 0, "proj.reload", Json::Null).unwrap();
    assert!(!tabs[0].dirty);
    assert_eq!(view(&tabs[0]).ed.undo_depth(), 0);
    assert!(view(&tabs[0]).prompt.is_none());
    assert_ne!(view(&tabs[0]).ed.project(), &before.project);
    let after = snapshot(&tabs[0]);
    assert!(after.leveled);
    assert_eq!(after.query, before.query);
    assert_eq!(after.scroll, before.scroll);
    assert_eq!(after.offsets, before.offsets);
    assert!(std::rc::Rc::ptr_eq(&scroll.0, &view(&tabs[0]).scroll.0));
    assert_eq!(
        after.status,
        project_tab_from_path(&source).status.to_string()
    );
    // A shorter schedule or narrower content clamps stale offsets without replacing the view.
    vm(&mut tabs[0]).table_x = f32::MAX;
    vm(&mut tabs[0]).gantt_x = f32::MAX;
    call(&mut tabs, 0, "proj.reload", Json::Null).unwrap();
    let v = view(&tabs[0]);
    assert!(v.table_x > 0. && v.table_x < f32::MAX);
    assert_eq!(v.gantt_x, (v.scale.width() - v.gantt_w).max(0.));
    let offsets = (v.table_x, v.gantt_x);
    vm(&mut tabs[0]).key("right", true);
    vm(&mut tabs[0]).key("right", false);
    assert_eq!((view(&tabs[0]).table_x, view(&tabs[0]).gantt_x), offsets);
    tabs[0].path = None;
    let before = snapshot(&tabs[0]);
    assert!(call(&mut tabs, 0, "proj.reload", Json::Null).is_err());
    assert_eq!(snapshot(&tabs[0]), before);
}

#[test]
fn request_bridge_stays_asleep_until_arrival_and_reports_disconnect() {
    use std::future::Future;
    use std::sync::{Arc, mpsc};
    use std::task::{Context as TaskContext, Poll, Wake, Waker};

    struct WakeSignal(mpsc::Sender<()>);
    impl Wake for WakeSignal {
        fn wake(self: Arc<Self>) {
            let _ = self.0.send(());
        }
    }
    let (tx, rx) = mpsc::channel();
    let pending = async_requests(rx);
    let (wake_tx, wakes) = mpsc::channel();
    let waker = Waker::from(Arc::new(WakeSignal(wake_tx)));
    let mut cx = TaskContext::from_waker(&waker);
    let mut receive = std::pin::pin!(pending.recv_async());
    assert!(receive.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        wakes.recv_timeout(Duration::from_secs(2)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    tx.send(42).unwrap();
    wakes.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(receive.as_mut().poll(&mut cx), Poll::Ready(Ok(42)));
    let mut receive = std::pin::pin!(pending.recv_async());
    assert!(receive.as_mut().poll(&mut cx).is_pending());
    drop(tx);
    wakes.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        receive.as_mut().poll(&mut cx),
        Poll::Ready(Err(flume::RecvError::Disconnected))
    );
}

#[test]
fn open_focuses_loaded_duplicates_recovers_placeholders_and_preserves_failed_state() {
    let dir = scratch();
    let source = copied(&dir);
    let bytes = std::fs::read(&source).unwrap();
    let mut tabs = vec![word()];
    let (_, effect) = call(&mut tabs, 0, "proj.open", path_args(&source)).unwrap();
    assert_eq!(effect.focus, Some(1));
    assert_eq!(tabs.len(), 2);
    vm(&mut tabs[1]).ed.rename(1, "unsaved").unwrap();
    tabs[1].dirty = true;
    vm(&mut tabs[1]).open_prompt(PromptKind::Rename);
    let before = snapshot(&tabs[1]);
    let (_, effect) = call(
        &mut tabs,
        0,
        "proj.open",
        path_args(&dir.join("./schedule.xml")),
    )
    .unwrap();
    assert_eq!(effect.focus, Some(1));
    assert_eq!(tabs.len(), 2);
    assert_eq!(snapshot(&tabs[1]), before);
    assert!(
        call(
            &mut tabs,
            0,
            "proj.open",
            args(r#"{"path":"anything","tab":1}"#)
        )
        .unwrap_err()
        .contains("does not take")
    );
    for path in [
        dir.join("missing.xml"),
        dir.join("unsupported.txt"),
        dir.join("corrupt.xml"),
    ] {
        if path.file_name().unwrap() != "missing.xml" {
            std::fs::write(&path, "invalid").unwrap();
        }
        assert!(call(&mut tabs, 0, "proj.open", path_args(&path)).is_err());
        assert_eq!(tabs.len(), 2);
        assert_eq!(snapshot(&tabs[1]), before);
    }
    let broken = dir.join("broken.xml");
    std::fs::write(&broken, "invalid").unwrap();
    tabs.push(project_tab_from_path(&broken));
    let status = tabs[2].status.clone();
    assert!(call(&mut tabs, 0, "proj.open", path_args(&broken)).is_err());
    assert_eq!(tabs.len(), 3);
    assert_eq!(tabs[2].status, status);
    assert!(matches!(tabs[2].surface, Surface::Placeholder));
    std::fs::write(&broken, bytes).unwrap();
    let (_, effect) = call(&mut tabs, 0, "proj.open", path_args(&broken)).unwrap();
    assert_eq!(effect.focus, Some(2));
    assert_eq!(tabs.len(), 3);
    assert!(matches!(tabs[2].surface, Surface::Project(_)));
}
