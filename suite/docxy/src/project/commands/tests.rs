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
fn tab() -> DocTab {
    let mut t = new_project_tab();
    for (name, duration) in [("First", 480), ("Second", 960)] {
        vm(&mut t).ed.add_task(None, name, duration).unwrap();
    }
    let p = v(&t).ed.project().clone();
    vm(&mut t).ed = ProjectEditor::new(p);
    vm(&mut t).ed.select(1);
    t
}
fn geometry(t: &DocTab) -> Vec<(i32, String)> {
    let ed = &v(t).ed;
    let scale = gantt_scale(ed);
    ed.project()
        .tasks
        .iter()
        .map(|t| (t.id, gantt_bar(ed, t, scale).unwrap().state()))
        .collect()
}
fn commit(t: &mut DocTab, act: ProjectAct, text: &str) {
    apply_project_act(t, act);
    let mut p = vm(t).prompt.take().unwrap();
    p.buf = text.into();
    commit_prompt(t, p);
}

fn take_reveal(t: &DocTab) -> Option<usize> {
    v(t).scroll
        .0
        .borrow_mut()
        .deferred_scroll_to_item
        .take()
        .map(|request| request.item_index)
}

#[test]
fn completion_reveals_selection_changes_without_disturbing_other_inputs() {
    use ProjectAct::*;
    let mut t = tab();
    for act in ProjectAct::RIBBON
        .iter()
        .copied()
        .filter(|a| !matches!(a, AddTask | DeleteTask))
    {
        apply_project_act(&mut t, act);
        assert_eq!(take_reveal(&t), None, "{act:?}");
    }
    vm(&mut t).cancel_prompt();
    for (key, modifiers) in [
        ("right", Modifiers::default()),
        (
            "right",
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        ),
        ("q", Modifiers::default()),
        (
            "down",
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
    ] {
        project_input(&mut t, key, None, modifiers);
        assert_eq!(take_reveal(&t), None, "{key}");
    }
    project_input(&mut t, "home", None, Modifiers::default());
    assert_eq!(take_reveal(&t), Some(0));
    let old_uid = v(&t).ed.selected_uid();
    apply_project_act(&mut t, DeleteTask);
    assert_eq!(v(&t).ed.sel(), 0);
    assert_ne!(v(&t).ed.selected_uid(), old_uid);
    assert_eq!(take_reveal(&t), Some(0));
    for act in [Undo, Redo, AddTask] {
        apply_project_act(&mut t, act);
        assert_eq!(take_reveal(&t), Some(v(&t).ed.sel()), "{act:?}");
    }
    apply_project_act(&mut t, Find);
    project_input(&mut t, "n", Some("New task"), Modifiers::default());
    assert_eq!(take_reveal(&t), None);
    project_input(&mut t, "enter", None, Modifiers::default());
    assert_eq!(take_reveal(&t), Some(v(&t).ed.sel()));
    apply_project_act(&mut t, FindNext);
    assert_eq!(
        take_reveal(&t),
        Some(v(&t).ed.sel()),
        "repeat search reveals even the same row"
    );
}

#[test]
fn baseline_on_an_empty_project_preserves_status_dirtiness_and_history() {
    let mut t = new_project_tab();
    let before = v(&t).ed.project().clone();
    let status = t.status.clone();
    apply_project_act(&mut t, ProjectAct::Baseline);
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(t.status, status);
    assert!(!t.dirty && !v(&t).ed.dirty());
    assert_eq!((v(&t).ed.undo_depth(), v(&t).ed.redo_depth()), (0, 0));
    assert_eq!(take_reveal(&t), None);
}

#[test]
fn ribbon_inventory_keys_tips_and_assets_are_complete() {
    let r = project_ribbon();
    assert_eq!(
        r.tabs.iter().map(|t| t.name).collect::<Vec<_>>(),
        ["Task", "Resource", "Report", "Project", "View"]
    );
    let mut acts = vec![];
    for t in &r.tabs {
        let mut keys = vec![];
        for g in &t.groups {
            for control in &g.items {
                let cmds: Vec<_> = match control {
                    Control::Large(c) | Control::Toggle(c) => vec![c],
                    Control::Column(c) => c.iter().collect(),
                    _ => panic!("unexpected control"),
                };
                for c in cmds {
                    let Act::Project(act) = c.act else {
                        panic!("wrong action")
                    };
                    acts.push(act);
                    assert!(!c.key_tip.is_empty() && !keys.contains(&c.key_tip));
                    keys.push(c.key_tip);
                    assert!(!c.tip.title.is_empty() && !c.tip.shortcut.is_empty());
                    assert!(
                        cmd_tip_text(c).ends_with(c.tip.shortcut),
                        "{} hover shows its shortcut",
                        c.label
                    );
                    assert!(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("assets/icons")
                            .join(format!("{}.svg", c.icon.0))
                            .exists(),
                        "{}",
                        c.icon.0
                    );
                }
            }
        }
    }
    assert_eq!(acts.len(), ProjectAct::RIBBON.len());
    for a in ProjectAct::RIBBON {
        assert_eq!(acts.iter().filter(|x| *x == a).count(), 1, "{a:?}");
    }
}

#[test]
fn ribbon_context_survives_valid_switches_only() {
    for kind in [Kind::Project, Kind::Docx, Kind::Xlsx] {
        assert_eq!(
            ribbon_for(kind)
                .tabs
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>(),
            ribbon_tab_set(kind)
                .iter()
                .skip(1)
                .map(|(_, name, _)| *name)
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        ribbon_tab_set(Kind::Project)
            .iter()
            .map(|x| x.1)
            .collect::<Vec<_>>(),
        ["File", "Task", "Resource", "Report", "Project", "View"]
    );
    assert_eq!(
        ribbon_tab_set(Kind::Project)
            .iter()
            .map(|x| x.2)
            .collect::<Vec<_>>(),
        ["F", "T", "U", "R", "P", "W"]
    );
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::View, false) == RibbonTab::View);
    assert!(valid_ribbon_tab(Kind::Xlsx, RibbonTab::Task, false) == RibbonTab::Home);
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::Table, true) == RibbonTab::Table);
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::Table, false) == RibbonTab::Home);
    assert!(valid_ribbon_tab(Kind::Project, RibbonTab::Table, true) == RibbonTab::Task);
    for tab in [RibbonTab::Resource, RibbonTab::Report, RibbonTab::Project] {
        assert!(valid_ribbon_tab(Kind::Project, tab, false) == tab);
        assert!(valid_ribbon_tab(Kind::Docx, tab, false) == RibbonTab::Home);
        assert!(valid_ribbon_tab(Kind::Xlsx, tab, false) == RibbonTab::Home);
    }
    assert!(valid_ribbon_tab(Kind::Project, RibbonTab::Home, false) == RibbonTab::Task);
    assert_eq!(ribbon_tab_index(RibbonTab::View, Kind::Project), 4);
    assert_eq!(ribbon_tab_index(RibbonTab::Project, Kind::Project), 3);
    assert_eq!(ribbon_tab_name(RibbonTab::Project), "Project");
    assert_eq!(ribbon_tab_index(RibbonTab::View, Kind::Docx), 3);
}

/// The owner's bar for #72: a Microsoft Project instruction written as a ribbon
/// path (tab > group > command) must be followable as written, by mouse and by
/// KeyTips.
#[test]
fn project_instruction_paths_exist() {
    use ProjectAct::*;
    let r = project_ribbon();
    let mut paths = vec![];
    for t in &r.tabs {
        for g in &t.groups {
            for control in &g.items {
                let cmds: Vec<_> = match control {
                    Control::Large(c) | Control::Toggle(c) => vec![c],
                    Control::Column(c) => c.iter().collect(),
                    _ => panic!("unexpected control"),
                };
                for c in cmds {
                    paths.push((t.name, g.title, c.label, c.act, c.key_tip));
                }
            }
        }
    }
    for (tab, group, label, act) in [
        ("Task", "Schedule", "Indent Task", Indent),
        ("Task", "Schedule", "Outdent Task", Outdent),
        ("Task", "Schedule", "Link the Selected Tasks", AddLink),
        ("Task", "Insert", "Task", AddTask),
        ("Task", "Insert", "Milestone", Milestone),
        ("Task", "Properties", "Information", Constraint),
        ("Task", "Editing", "Find", Find),
        ("Resource", "Assignments", "Assign Resources", Assign),
        ("Resource", "Level", "Level All", LevelAll),
        ("Resource", "Level", "Clear Leveling", ClearLeveling),
        ("Report", "Export", "Export Gantt", ExportGantt),
        ("Project", "Schedule", "Calculate Project", Recalc),
        ("Project", "Schedule", "Set Baseline", Baseline),
        ("View", "Split View", "Timeline", Timeline),
    ] {
        let path = format!("{tab} > {group} > {label}");
        let Some((_, _, _, found, key)) = paths
            .iter()
            .find(|p| (p.0, p.1, p.2) == (tab, group, label))
        else {
            panic!("missing ribbon path {path}");
        };
        assert!(matches!(found, Act::Project(a) if *a == act), "{path}");
        let t = r.tabs.iter().find(|t| t.name == tab).unwrap();
        assert!(
            matches!(tab_keytip_cmd(t, key), Some(Act::Project(a)) if a == act),
            "{path} KeyTip {key}"
        );
    }
    for gone in [Save, Level] {
        assert!(
            !paths
                .iter()
                .any(|p| matches!(p.3, Act::Project(a) if a == gone)),
            "{gone:?} is not on Project's ribbon"
        );
    }
}

#[test]
fn large_and_small_buttons_show_the_same_tooltip() {
    let r = project_ribbon();
    let level_all = r.tabs[1].groups[1].items.iter().find_map(|c| match c {
        Control::Large(c) if c.label == "Level All" => Some(c),
        _ => None,
    });
    assert_eq!(
        cmd_tip_text(level_all.expect("Level All is a large button")).as_ref(),
        "Level All  ·  Alt, U, L  (Ctrl+Shift+L toggles)"
    );
    let bare = cmdt("x", "find", "Bare", Act::Project(ProjectAct::Find), "");
    assert_eq!(cmd_tip_text(&bare).as_ref(), "Bare");
}

#[test]
fn timeline_toggles_the_pane_as_view_state_only() {
    let mut t = tab();
    assert!(v(&t).timeline, "a Project opens with its Timeline shown");
    assert!(project_act_active(v(&t), ProjectAct::Timeline));
    let before = (
        t.dirty,
        v(&t).ed.undo_depth(),
        v(&t).ed.sel(),
        v(&t).ed.project().clone(),
    );
    for shown in [false, true, false] {
        apply_project_act(&mut t, ProjectAct::Timeline);
        assert_eq!(v(&t).timeline, shown);
        assert_eq!(project_act_active(v(&t), ProjectAct::Timeline), shown);
        assert_eq!(take_reveal(&t), None);
        assert_eq!(
            before,
            (
                t.dirty,
                v(&t).ed.undo_depth(),
                v(&t).ed.sel(),
                v(&t).ed.project().clone(),
            )
        );
    }
    let state = project_state(v(&t));
    assert!(state.contains(&("timeline".into(), ctlcore::json::Json::Str("hidden".into()))));
}

#[test]
fn project_act_active_checks_level_all_and_timeline_only() {
    let mut t = tab();
    assert!(!project_act_active(v(&t), ProjectAct::LevelAll));
    apply_project_act(&mut t, ProjectAct::LevelAll);
    assert!(project_act_active(v(&t), ProjectAct::LevelAll));
    for act in ProjectAct::RIBBON
        .iter()
        .copied()
        .filter(|a| !matches!(a, ProjectAct::LevelAll | ProjectAct::Timeline))
    {
        assert!(!project_act_active(v(&t), act), "{act:?}");
    }
}

#[test]
fn level_all_and_clear_leveling_are_idempotent() {
    let mut t = tab();
    let on = "Resource leveling ON — bars delayed to fit resource capacity";
    for (act, leveled, status) in [
        (ProjectAct::LevelAll, true, on),
        (ProjectAct::LevelAll, true, on),
        (ProjectAct::ClearLeveling, false, "Resource leveling OFF"),
        (ProjectAct::ClearLeveling, false, "Resource leveling OFF"),
    ] {
        apply_project_act(&mut t, act);
        assert_eq!(v(&t).ed.leveled(), leveled, "{act:?}");
        assert_eq!(t.status.as_ref(), status, "{act:?}");
    }
    apply_project_act(&mut t, ProjectAct::Level);
    assert!(v(&t).ed.leveled(), "Ctrl+Shift+L still toggles");
}

#[test]
fn keys_and_whole_route_enforce_modifiers() {
    use ProjectAct::*;
    for (key, act) in [
        ("insert", AddTask),
        ("delete", DeleteTask),
        ("f3", FindNext),
    ] {
        assert_eq!(key_act(key, Modifiers::default()), Some(act));
    }
    for (key, act) in [
        ("f", Find),
        ("z", Undo),
        ("y", Redo),
        ("s", Save),
        ("e", ExportGantt),
    ] {
        assert_eq!(
            key_act(
                key,
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                }
            ),
            Some(act)
        );
    }
    assert_eq!(
        key_act(
            "l",
            Modifiers {
                shift: true,
                control: true,
                ..Modifiers::default()
            }
        ),
        Some(Level)
    );
    let mut t = tab();
    vm(&mut t).ed.rename(1, "Changed").unwrap();
    let before = v(&t).ed.project().clone();
    let sel = v(&t).ed.sel();
    for (key, m) in [
        (
            "z",
            Modifiers {
                control: true,
                alt: true,
                ..Modifiers::default()
            },
        ),
        (
            "down",
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
        (
            "s",
            Modifiers {
                platform: true,
                ..Modifiers::default()
            },
        ),
        (
            "n",
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
        ),
    ] {
        assert!(project_input(&mut t, key, Some(key), m).is_none());
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.sel(), sel);
    }
    apply_project_act(&mut t, Rename);
    let buf = v(&t).prompt.as_ref().unwrap().buf.clone();
    for key in ["enter", "escape", "backspace", "n"] {
        for m in [
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
            Modifiers {
                control: true,
                alt: true,
                ..Modifiers::default()
            },
        ] {
            project_input(&mut t, key, Some("€"), m);
            assert_eq!(v(&t).prompt.as_ref().unwrap().buf, buf);
        }
    }
}

#[test]
fn prompts_bind_uid_cancel_and_edit_unicode_without_dispatching_commands() {
    let mut t = tab();
    apply_project_act(&mut t, ProjectAct::Rename);
    let p = v(&t).prompt.clone().unwrap();
    assert_eq!(p.buf, "Second");
    vm(&mut t).ed.select(0);
    commit_prompt(
        &mut t,
        ProjectPrompt {
            buf: "Bound".into(),
            ..p
        },
    );
    assert_eq!(v(&t).ed.project().tasks[0].name, "First");
    assert_eq!(v(&t).ed.project().tasks[1].name, "Bound");
    apply_project_act(&mut t, ProjectAct::Duration);
    project_input(&mut t, "n", Some("n"), Modifiers::default());
    project_input(&mut t, "x", Some("λ"), Modifiers::default());
    project_input(&mut t, "backspace", None, Modifiers::default());
    assert_eq!(v(&t).prompt.as_ref().unwrap().buf, "n");
    assert_eq!(v(&t).ed.project().tasks.len(), 2);
    project_input(&mut t, "tab", Some("\t"), Modifiers::default());
    assert!(v(&t).prompt.is_some());
    let status = t.status.clone();
    project_input(&mut t, "escape", None, Modifiers::default());
    assert!(v(&t).prompt.is_none());
    assert_eq!(t.status, status);
    apply_project_act(&mut t, ProjectAct::Rename);
    vm(&mut t).select_row(1);
    assert!(v(&t).prompt.is_none());
    apply_project_act(&mut t, ProjectAct::Rename);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert!(v(&t).prompt.is_none());
    let mut empty = new_project_tab();
    commit(&mut empty, ProjectAct::Rename, "Empty");
    assert_eq!(empty.status.as_ref(), "No task selected");
    assert!(!empty.dirty);
}

#[test]
fn rejection_preserves_model_geometry_and_both_history_stacks() {
    use ProjectAct::*;
    for (act, text, message) in [
        (
            Duration,
            "banana",
            "Couldn't read duration 'banana' (try 3d, 4h, 2w)",
        ),
        (AddLink, "abc", "Predecessor must be a task ID (number)"),
        (AddLink, "999", "No other task with ID 999"),
        (AddLink, "2", "No other task with ID 2"),
        (Constraint, "mso", "MSO needs a date, e.g. mso 2026-03-05"),
    ] {
        let mut t = tab();
        vm(&mut t).ed.rename(1, "Temporary").unwrap();
        vm(&mut t).ed.undo();
        vm(&mut t).ed.mark_saved();
        let before = v(&t).ed.project().clone();
        let geom = geometry(&t);
        let depths = (v(&t).ed.undo_depth(), v(&t).ed.redo_depth());
        commit(&mut t, act, text);
        assert_eq!(t.status.as_ref(), message);
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(geometry(&t), geom);
        assert_eq!((v(&t).ed.undo_depth(), v(&t).ed.redo_depth()), depths);
        assert!(!t.dirty);
        assert!(v(&t).prompt.is_none());
    }
}

#[test]
fn predecessor_input_uses_displayed_ids_and_duplicate_error_does_too() {
    let mut t = tab();
    let mut p = v(&t).ed.project().clone();
    p.tasks[0].uid = 7;
    p.tasks[0].id = 3;
    p.tasks[1].uid = 9;
    p.tasks[1].id = 4;
    vm(&mut t).ed = ProjectEditor::new(p);
    vm(&mut t).ed.select(1);
    commit(&mut t, ProjectAct::AddLink, "3");
    assert_eq!(v(&t).ed.project().tasks[1].predecessors[0].uid, 7);
    let depth = v(&t).ed.undo_depth();
    commit(&mut t, ProjectAct::AddLink, "3");
    assert_eq!(t.status.as_ref(), "Already depends on 3");
    assert_eq!(v(&t).ed.undo_depth(), depth);
    commit(&mut t, ProjectAct::AddLink, "4");
    assert_eq!(t.status.as_ref(), "No other task with ID 4");
}

#[test]
fn every_edit_undoes_and_redoes_model_and_geometry_through_command_paths() {
    use ProjectAct::*;
    for (act, text, changes_geometry) in [
        (AddTask, None, true),
        (DeleteTask, None, true),
        (Indent, None, true),
        (Outdent, None, true),
        (Rename, Some("Renamed"), false),
        (Duration, Some("3d"), true),
        (AddLink, Some("1"), true),
        (Constraint, Some("SNET 2026-01-08"), true),
        (Assign, Some("Alice"), false),
        (ClearResources, None, false),
        (Baseline, None, false),
        (Milestone, None, true),
    ] {
        let mut t = tab();
        if act == Outdent {
            vm(&mut t).ed.indent(2, 1).unwrap();
        }
        if act == ClearResources {
            vm(&mut t).ed.assign_resource(2, "Alice").unwrap();
        }
        let before = v(&t).ed.project().clone();
        let before_geom = geometry(&t);
        if let Some(text) = text {
            commit(&mut t, act, text);
        } else {
            apply_project_act(&mut t, act);
        }
        let after = v(&t).ed.project().clone();
        let after_geom = geometry(&t);
        assert_ne!(before, after, "{act:?}");
        assert_eq!(before_geom != after_geom, changes_geometry, "{act:?}");
        match act {
            AddTask => {
                assert_eq!(after.tasks.len(), 3);
                assert_eq!(v(&t).ed.sel(), 2);
            }
            DeleteTask => assert_eq!(after.tasks.len(), 1),
            Indent => assert_eq!(after.task(2).unwrap().outline_level, 2),
            Outdent => assert_eq!(after.task(2).unwrap().outline_level, 1),
            Rename => assert_eq!(after.task(2).unwrap().name, "Renamed"),
            Duration => assert_eq!(after.task(2).unwrap().duration_min, 3 * 480),
            AddLink => assert_eq!(after.task(2).unwrap().predecessors[0].uid, 1),
            Constraint => assert_ne!(
                before.task(2).unwrap().constraint,
                after.task(2).unwrap().constraint
            ),
            Assign => {
                assert_eq!(after.resources.len(), 1);
                assert_eq!(after.assignments.len(), 1);
            }
            ClearResources => assert!(after.assignments.is_empty()),
            Milestone => assert!(after.task(2).unwrap().milestone),
            Baseline => assert!(after.tasks.iter().all(|t| {
                t.baseline(0)
                    .is_some_and(|b| b.start.is_some() && b.finish.is_some())
            })),
            _ => unreachable!(),
        }
        apply_project_act(&mut t, Undo);
        assert_eq!(v(&t).ed.project(), &before, "{act:?}");
        assert_eq!(geometry(&t), before_geom);
        assert_eq!(t.status.as_ref(), "Undo");
        apply_project_act(&mut t, Redo);
        assert_eq!(v(&t).ed.project(), &after, "{act:?}");
        assert_eq!(geometry(&t), after_geom);
        assert_eq!(t.status.as_ref(), "Redo");
        assert_eq!(t.dirty, v(&t).ed.dirty());
    }
}

#[test]
fn assignment_find_leveling_recalc_and_navigation_statuses() {
    let mut t = tab();
    commit(&mut t, ProjectAct::Assign, "Alice");
    assert_eq!(t.status.as_ref(), "Assigned Alice");
    let depth = v(&t).ed.undo_depth();
    commit(&mut t, ProjectAct::Assign, "alice");
    assert_eq!(t.status.as_ref(), "alice is already assigned");
    assert_eq!(depth, v(&t).ed.undo_depth());
    commit(&mut t, ProjectAct::Assign, "");
    assert_eq!(t.status.as_ref(), "Cleared the task's resources");
    let status = t.status.clone();
    apply_project_act(&mut t, ProjectAct::FindNext);
    assert_eq!(t.status, status);
    commit(&mut t, ProjectAct::Find, "first");
    assert_eq!(v(&t).ed.sel(), 0);
    assert_eq!(t.status.as_ref(), "Found 'first'  (F3 next)");
    apply_project_act(&mut t, ProjectAct::FindNext);
    assert_eq!(v(&t).ed.sel(), 0);
    commit(&mut t, ProjectAct::Find, "absent");
    assert_eq!(t.status.as_ref(), "No task matching 'absent'");
    apply_project_act(&mut t, ProjectAct::Level);
    assert!(v(&t).ed.leveled());
    assert!(t.status.starts_with("Resource leveling ON"));
    apply_project_act(&mut t, ProjectAct::Level);
    assert_eq!(t.status.as_ref(), "Resource leveling OFF");
    apply_project_act(&mut t, ProjectAct::Recalc);
    assert_eq!(t.status.as_ref(), "Rescheduled (automatic on every edit)");
    apply_project_act(&mut t, ProjectAct::ScrollRight);
    assert_eq!(v(&t).gantt_x.get(), DAY_W);
    apply_project_act(&mut t, ProjectAct::ScrollLeft);
    assert_eq!(v(&t).gantt_x.get(), 0.);
    apply_project_act(&mut t, ProjectAct::ScrollRight);
    apply_project_act(&mut t, ProjectAct::GoToStart);
    assert_eq!(v(&t).gantt_x.get(), 0.);
}

#[test]
fn export_decisions_and_io_preserve_the_project_binding_and_history() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/project-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.xml");
    let mut t = tab();
    std::fs::write(&source, mspdi::write_mspdi(v(&t).ed.project())).unwrap();
    let source_bytes = std::fs::read(&source).unwrap();
    assert_eq!(export_decision(&t, true), ExportDecision::RefuseHarness);
    assert!(matches!(
        export_decision(&t, false),
        ExportDecision::Dialog { .. }
    ));
    t.path = Some(source.clone());
    t.title = "source.xml".into();
    assert_eq!(
        export_decision(&t, true),
        ExportDecision::InPlace(source.with_extension("md"))
    );
    apply_project_act(&mut t, ProjectAct::Baseline);
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    apply_export(&mut t, &dir.join("source.md")).unwrap();
    assert!(
        std::fs::read_to_string(dir.join("source.md"))
            .unwrap()
            .contains("First")
    );
    assert_eq!(v(&t).exported.as_deref(), Some("source.md"));
    assert!(apply_export(&mut t, &source).is_err());
    assert!(apply_export(&mut t, &dir.join("./source.xml")).is_err());
    let alias = dir.join("source-alias.md");
    std::fs::hard_link(&source, &alias).unwrap();
    assert!(apply_export(&mut t, &alias).is_err());
    assert_eq!(std::fs::read(&alias).unwrap(), source_bytes);
    std::fs::remove_file(alias).unwrap();
    assert!(apply_export(&mut t, &dir).is_err());
    assert!(t.status.contains("Export failed"));

    assert!(apply_export(&mut t, &source.join("bad.md")).is_err());
    finish_project_export(&mut t, None);
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert!(t.dirty && v(&t).ed.dirty());
    assert_eq!(t.path, Some(source.clone()));
    assert_eq!(t.title.as_ref(), "source.xml");
    assert_eq!(v(&t).exported.as_deref(), Some("source.md"));
    assert_eq!(std::fs::read(source).unwrap(), source_bytes);
    t.surface = Surface::Placeholder;
    assert_eq!(export_decision(&t, false), ExportDecision::Unsaveable);
    assert!(apply_export(&mut t, &dir.join("bad.md")).is_err());
}

#[test]
fn project_info_has_schedule_counts_and_outline_bullets() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/mspdi/10-summary.xml");
    let t = project_tab_from_path(&path);
    let info = project_info_lines(&v(&t).ed);
    assert_eq!(info[0], "summary");
    assert!(info[1].contains("2026-03-02") && info[1].contains("2026-03-03"));
    assert!(info[2].starts_with("3 tasks"));
    assert_eq!(info[3], "• Phase");
    assert_eq!(info[4], "  • A");
}
