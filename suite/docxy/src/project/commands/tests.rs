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
    // A view over the finished plan, as opening it builds one.
    let p = v(&t).ed.project().clone();
    t.surface = Surface::Project(ProjectView::new(p, false));
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
    // The contextual Gantt Chart Format tab holds the rest of the inventory.
    let fmt = gantt_format_tab();
    let mut acts = vec![];
    for t in r.tabs.iter().chain([&fmt]) {
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
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::View, false, false) == RibbonTab::View);
    assert!(valid_ribbon_tab(Kind::Xlsx, RibbonTab::Task, false, false) == RibbonTab::Home);
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::Table, true, false) == RibbonTab::Table);
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::Table, false, false) == RibbonTab::Home);
    assert!(valid_ribbon_tab(Kind::Project, RibbonTab::Table, true, true) == RibbonTab::Task);
    for tab in [RibbonTab::Resource, RibbonTab::Report, RibbonTab::Project] {
        assert!(valid_ribbon_tab(Kind::Project, tab, false, false) == tab);
        assert!(valid_ribbon_tab(Kind::Docx, tab, false, false) == RibbonTab::Home);
        assert!(valid_ribbon_tab(Kind::Xlsx, tab, false, false) == RibbonTab::Home);
    }
    assert!(valid_ribbon_tab(Kind::Project, RibbonTab::Home, false, false) == RibbonTab::Task);
    // The contextual Gantt Chart Format tab: only a Project with its Gantt showing.
    let fmt = RibbonTab::GanttFormat;
    assert!(valid_ribbon_tab(Kind::Project, fmt, false, true) == fmt);
    assert!(valid_ribbon_tab(Kind::Project, fmt, false, false) == RibbonTab::Task);
    assert!(valid_ribbon_tab(Kind::Docx, fmt, false, true) == RibbonTab::Home);
    assert!(valid_ribbon_tab(Kind::Xlsx, fmt, false, true) == RibbonTab::Home);
    assert_eq!(ribbon_tab_name(fmt), "Gantt Chart Format");
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
    let fmt = gantt_format_tab();
    let tabs: Vec<_> = r.tabs.iter().chain([&fmt]).collect();
    let mut paths = vec![];
    for t in &tabs {
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
        ("Task", "Schedule", "Unlink Tasks", UnlinkTasks),
        ("Task", "Tasks", "Manually Schedule", ManuallySchedule),
        ("Task", "Tasks", "Auto Schedule", AutoSchedule),
        ("Task", "Tasks", "Move", MoveTask),
        ("Task", "Insert", "Task", AddTask),
        ("Task", "Insert", "Milestone", Milestone),
        ("Task", "Properties", "Information", Constraint),
        ("Task", "Editing", "Find", Find),
        ("Task", "Editing", "Scroll to Task", ScrollToTask),
        ("Resource", "Assignments", "Assign Resources", Assign),
        ("Resource", "Level", "Level All", LevelAll),
        ("Resource", "Level", "Clear Leveling", ClearLeveling),
        ("Report", "Export", "Export Gantt", ExportGantt),
        ("Project", "Schedule", "Calculate Project", Recalc),
        ("Project", "Schedule", "Set Baseline", Baseline),
        ("Project", "Schedule", "Clear Baseline", ClearBaseline),
        ("View", "Split View", "Timeline", Timeline),
        (
            "Gantt Chart Format",
            "Bar Styles",
            "Critical Tasks",
            CriticalTasks,
        ),
        ("Gantt Chart Format", "Bar Styles", "Baseline", BaselineBars),
    ] {
        let path = format!("{tab} > {group} > {label}");
        let Some((_, _, _, found, key)) = paths
            .iter()
            .find(|p| (p.0, p.1, p.2) == (tab, group, label))
        else {
            panic!("missing ribbon path {path}");
        };
        assert!(matches!(found, Act::Project(a) if *a == act), "{path}");
        let t = tabs.iter().find(|t| t.name == tab).unwrap();
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
fn bar_style_toggles_change_the_drawn_bars_as_view_state_only() {
    use ctlcore::json::Json;
    let mut t = tab();
    // On a clean plan the toggles dirty nothing and add no undo step.
    for act in [ProjectAct::CriticalTasks, ProjectAct::BaselineBars].repeat(2) {
        apply_project_act(&mut t, act);
        assert!(!t.dirty && !v(&t).ed.dirty(), "{act:?}");
        assert_eq!(v(&t).ed.undo_depth(), 0, "{act:?}");
    }
    // The longer of two parallel tasks is critical; baseline the plan so
    // there is a baseline bar to hide.
    apply_project_act(&mut t, ProjectAct::Baseline);
    let get = |t: &DocTab, key: &str| {
        project_state(v(t), None)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, j)| j)
            .unwrap()
    };
    let (bar, baseline) = (get(&t, "bar_2"), get(&t, "baseline_2"));
    let Json::Str(bar) = bar else { panic!() };
    assert!(bar.starts_with("critical "), "{bar}");
    let span = bar.trim_start_matches("critical ").to_string();
    assert_eq!(baseline, Json::Str(span.clone()));
    let before = (
        t.dirty,
        v(&t).ed.undo_depth(),
        v(&t).ed.sel(),
        v(&t).ed.project().clone(),
    );
    apply_project_act(&mut t, ProjectAct::CriticalTasks);
    assert!(!v(&t).show_critical);
    assert_eq!(get(&t, "bar_2"), Json::Str(format!("on-track {span}")));
    apply_project_act(&mut t, ProjectAct::BaselineBars);
    assert!(!v(&t).show_baseline);
    assert_eq!(get(&t, "baseline_2"), Json::Str("none".into()));
    for act in [ProjectAct::CriticalTasks, ProjectAct::BaselineBars] {
        apply_project_act(&mut t, act);
    }
    assert_eq!(get(&t, "bar_2"), Json::Str(format!("critical {span}")));
    assert_eq!(get(&t, "baseline_2"), Json::Str(span));
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
    let state = project_state(v(&t), None);
    assert!(state.contains(&("timeline".into(), ctlcore::json::Json::Str("hidden".into()))));
}

#[test]
fn project_act_active_checks_level_all_timeline_and_the_task_mode_only() {
    use ProjectAct::*;
    let mut t = tab();
    assert!(!project_act_active(v(&t), LevelAll));
    apply_project_act(&mut t, LevelAll);
    assert!(project_act_active(v(&t), LevelAll));
    assert!(project_act_active(v(&t), AutoSchedule));
    for act in ProjectAct::RIBBON.iter().copied().filter(|a| {
        !matches!(
            a,
            LevelAll | Timeline | AutoSchedule | CriticalTasks | BaselineBars
        )
    }) {
        assert!(!project_act_active(v(&t), act), "{act:?}");
    }
    // The Gantt Chart Format toggles start on (today's drawing) and flip.
    for act in [CriticalTasks, BaselineBars] {
        assert!(project_act_active(v(&t), act), "{act:?}");
        apply_project_act(&mut t, act);
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
    // An empty plan has only the entry row, where task prompts do not open.
    let mut empty = new_project_tab();
    let status = empty.status.clone();
    apply_project_act(&mut empty, ProjectAct::Rename);
    assert!(v(&empty).prompt.is_none());
    assert_eq!(empty.status, status);
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
        (ClearBaseline, None, false),
        (Milestone, None, true),
        (UnlinkTasks, None, true),
        (MoveTask, Some("1d"), true),
    ] {
        let mut t = tab();
        if act == Outdent {
            vm(&mut t).ed.indent(2, 1).unwrap();
        }
        if act == ClearBaseline {
            vm(&mut t).ed.set_baseline();
        }
        if act == UnlinkTasks {
            vm(&mut t)
                .ed
                .add_predecessor(2, 1, LinkType::FinishStart, 0)
                .unwrap();
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
            ClearBaseline => assert!(after.tasks.iter().all(|t| t.baseline(0).is_none())),
            UnlinkTasks => assert!(after.tasks.iter().all(|t| t.predecessors.is_empty())),
            MoveTask => assert_eq!(
                after.task(2).unwrap().constraint,
                projcore::ConstraintType::StartNoEarlierThan
            ),
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

/// `First` as a summary over `Second`, with the summary selected.
fn summary_tab() -> DocTab {
    let mut t = tab();
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.select(0);
    vm(&mut t).ed.mark_saved();
    t
}

#[test]
fn deleting_a_summary_asks_first_and_escape_changes_nothing() {
    let mut t = summary_tab();
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    let prompt = v(&t).prompt.clone().expect("a summary delete asks first");
    assert_eq!(prompt.kind, PromptKind::ConfirmDelete);
    assert_eq!(prompt.kind.name(), "delete");
    assert_eq!(
        prompt_label(&prompt, &v(&t).ed).as_ref(),
        "Delete 'First' and its 1 subtask? Enter = delete, Esc = cancel"
    );
    assert_eq!(v(&t).ed.project(), &before);

    // Typing and backspace do not edit a yes/no prompt's buffer.
    project_input(&mut t, "d", Some("d"), Modifiers::default());
    assert_eq!(v(&t).prompt.as_ref().unwrap().buf, "");
    vm(&mut t).prompt.as_mut().unwrap().buf = "x".into();
    project_input(&mut t, "backspace", None, Modifiers::default());
    assert_eq!(v(&t).prompt.as_ref().unwrap().buf, "x");

    project_input(&mut t, "escape", None, Modifiers::default());
    assert!(v(&t).prompt.is_none());
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert!(!t.dirty);
}

#[test]
fn confirming_a_summary_delete_removes_its_subtree_in_one_undo_step() {
    let mut t = summary_tab();
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    project_input(&mut t, "enter", None, Modifiers::default());
    assert!(v(&t).prompt.is_none());
    assert!(v(&t).ed.project().tasks.is_empty());
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    assert!(t.dirty);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project(), &before);
    // Emptying the plan latched the entry row, so select the summary again.
    assert!(v(&t).on_entry_row());
    project_cell_click(&mut t, 0, None, false);

    // The prompt bar's Delete button commits through the same path as Enter.
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    let prompt = vm(&mut t).prompt.take().unwrap();
    commit_prompt(&mut t, prompt);
    assert!(v(&t).ed.project().tasks.is_empty());
}

#[test]
fn deleting_a_leaf_needs_no_confirmation() {
    let mut t = summary_tab();
    vm(&mut t).ed.select(1);
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    assert!(v(&t).prompt.is_none());
    assert_eq!(v(&t).ed.project().tasks.len(), 1);
    assert!(!v(&t).ed.project().tasks[0].summary);
}

// ---- the entry row below the last task (#145) ----

#[test]
fn task_commands_do_nothing_on_the_entry_row() {
    use ProjectAct::*;
    let mut t = tab();
    // Give the last task something every command would change.
    vm(&mut t).ed.assign_resource(2, "Bob").unwrap();
    vm(&mut t)
        .ed
        .add_predecessor(2, 1, LinkType::FinishStart, 0)
        .unwrap();
    let p = v(&t).ed.project().clone();
    vm(&mut t).ed = ProjectEditor::new(p);
    project_entry_click(&mut t, None, false);
    assert!(v(&t).on_entry_row());
    let before = v(&t).ed.project().clone();
    let status = t.status.clone();
    for act in [
        DeleteTask,
        Milestone,
        Indent,
        Outdent,
        ClearResources,
        Rename,
        Duration,
        AddLink,
        Constraint,
        Assign,
        UnlinkTasks,
        MoveTask,
        ScrollToTask,
    ] {
        vm(&mut t).gantt_x.set(44.);
        apply_project_act(&mut t, act);
        assert_eq!(v(&t).gantt_x.get(), 44., "{act:?}");
        assert!(v(&t).prompt.is_none(), "{act:?} opened a prompt");
        assert_eq!(v(&t).ed.project(), &before, "{act:?}");
        assert_eq!(v(&t).ed.undo_depth(), 0, "{act:?}");
        assert_eq!(t.status, status, "{act:?}");
        assert!(!t.dirty, "{act:?}");
        assert!(v(&t).on_entry_row(), "{act:?}");
    }
    // The same commands still act on a task row.
    project_cell_click(&mut t, 1, None, false);
    apply_project_act(&mut t, Milestone);
    assert_eq!(v(&t).ed.undo_depth(), 1);
}

#[test]
fn insert_on_the_entry_row_appends_and_selects_a_new_task() {
    let mut t = tab();
    project_entry_click(&mut t, None, false);
    apply_project_act(&mut t, ProjectAct::AddTask);
    let tasks = &v(&t).ed.project().tasks;
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[2].name, "New task");
    assert!(!v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 2);
    assert_eq!(take_reveal(&t), Some(2));
}

#[test]
fn find_from_the_entry_row_starts_at_the_first_task_and_leaves_it() {
    let mut t = tab();
    // The cursor was on the first task; both tasks match "s".
    project_cell_click(&mut t, 0, None, false);
    project_entry_click(&mut t, None, false);
    commit(&mut t, ProjectAct::Find, "s");
    assert_eq!(v(&t).ed.sel(), 0, "{}", t.status);
    assert!(!v(&t).on_entry_row(), "a hit leaves the entry row");
    // A miss stays on the entry row.
    project_entry_click(&mut t, None, false);
    commit(&mut t, ProjectAct::Find, "nothing like it");
    assert!(v(&t).on_entry_row());
    assert_eq!(t.status.as_ref(), "No task matching 'nothing like it'");
}

#[test]
fn f3_from_the_entry_row_starts_at_the_first_task_after_undo_and_redo() {
    let mut t = tab();
    // "ir" matches First and Third, not Second.
    project_entry_click(&mut t, None, false);
    commit(&mut t, ProjectAct::Find, "ir");
    assert_eq!(v(&t).ed.sel(), 0);
    project_entry_click(&mut t, Some(COL_NAME), false);
    project_input(&mut t, "t", Some("Third"), Modifiers::default());
    project_input(&mut t, "enter", None, Modifiers::default());
    assert_eq!(v(&t).ed.project().tasks[2].name, "Third");
    assert!(v(&t).on_entry_row());
    // Undo, then Redo, leave the selection short of the last task.
    apply_project_act(&mut t, ProjectAct::Undo);
    apply_project_act(&mut t, ProjectAct::Redo);
    assert_eq!(v(&t).ed.project().tasks.len(), 3);
    assert!(v(&t).on_entry_row());
    assert_ne!(v(&t).ed.sel(), 2);
    apply_project_act(&mut t, ProjectAct::FindNext);
    assert_eq!(v(&t).ed.sel(), 0, "F3 starts at the first task");
    assert!(!v(&t).on_entry_row());
}

#[test]
fn deleting_every_task_latches_the_entry_row_through_undo() {
    let mut t = tab();
    project_cell_click(&mut t, 0, None, false);
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    assert!(v(&t).ed.project().tasks.is_empty());
    assert!(v(&t).entry, "an emptied plan latches the entry row");
    // Undo brings a task back above the cursor, which stays on the entry row.
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project().tasks.len(), 1);
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 1);
}

#[test]
fn manually_and_auto_schedule_switch_the_selected_task_as_one_step() {
    use ProjectAct::*;
    let mut t = tab();
    let uid = v(&t).selected_uid().unwrap();
    let start = v(&t).ed.disp_start(uid);
    apply_project_act(&mut t, ManuallySchedule);
    let task = v(&t).ed.project().task(uid).unwrap();
    assert!(task.manual);
    assert_eq!(task.manual_start, start);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert!(t.dirty);
    assert!(project_act_active(v(&t), ManuallySchedule));
    assert!(!project_act_active(v(&t), AutoSchedule));
    // Again: the task already has that mode.
    apply_project_act(&mut t, ManuallySchedule);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    apply_project_act(&mut t, AutoSchedule);
    assert!(!v(&t).ed.project().task(uid).unwrap().manual);
    assert!(project_act_active(v(&t), AutoSchedule));
    apply_project_act(&mut t, Undo);
    assert!(v(&t).ed.project().task(uid).unwrap().manual);
    // The entry row has no task: neither is active, and neither changes anything.
    project_entry_click(&mut t, None, false);
    let before = v(&t).ed.project().clone();
    for act in [ManuallySchedule, AutoSchedule] {
        assert!(!project_act_active(v(&t), act), "{act:?}");
        apply_project_act(&mut t, act);
        assert_eq!(v(&t).ed.project(), &before, "{act:?}");
    }
}

#[test]
fn the_status_bar_item_switches_the_mode_for_new_tasks() {
    let mut t = tab();
    t.dirty = false;
    vm(&mut t).ed.mark_saved();
    apply_project_act(&mut t, ProjectAct::NewTasksMode);
    assert!(v(&t).ed.project().new_tasks_are_manual);
    assert_eq!(t.status.as_ref(), "New tasks: Manually Scheduled");
    assert!(t.dirty);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    apply_project_act(&mut t, ProjectAct::AddTask);
    assert!(v(&t).ed.project().tasks[v(&t).ed.sel()].manual);
    apply_project_act(&mut t, ProjectAct::NewTasksMode);
    assert!(!v(&t).ed.project().new_tasks_are_manual);
    assert_eq!(t.status.as_ref(), "New tasks: Auto Scheduled");
    apply_project_act(&mut t, ProjectAct::Undo);
    assert!(v(&t).ed.project().new_tasks_are_manual);
}

// ---- Project commands added for #118 ----

#[test]
fn unlink_tasks_reports_what_it_removed() {
    let mut t = tab();
    vm(&mut t)
        .ed
        .add_predecessor(2, 1, LinkType::FinishStart, 0)
        .unwrap();
    let depth = v(&t).ed.undo_depth();
    // The first task: its one successor link goes.
    vm(&mut t).ed.select(0);
    apply_project_act(&mut t, ProjectAct::UnlinkTasks);
    assert_eq!(t.status.as_ref(), "Removed 1 link");
    assert!(v(&t).ed.project().task(2).unwrap().predecessors.is_empty());
    assert!(t.dirty);
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    vm(&mut t).ed.mark_saved();
    apply_project_act(&mut t, ProjectAct::UnlinkTasks);
    assert_eq!(t.status.as_ref(), "No links to remove");
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    assert!(!v(&t).ed.dirty());
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project().task(2).unwrap().predecessors.len(), 1);
}

#[test]
fn clear_baseline_reports_whether_there_was_one() {
    let mut t = tab();
    apply_project_act(&mut t, ProjectAct::ClearBaseline);
    assert_eq!(t.status.as_ref(), "No baseline to clear");
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(!t.dirty);
    apply_project_act(&mut t, ProjectAct::Baseline);
    apply_project_act(&mut t, ProjectAct::ClearBaseline);
    assert_eq!(t.status.as_ref(), "Baseline cleared");
    assert!(
        v(&t)
            .ed
            .project()
            .tasks
            .iter()
            .all(|t| t.baseline(0).is_none())
    );
    assert_eq!(v(&t).ed.undo_depth(), 2);
    assert!(t.dirty);
}

#[test]
fn move_prompts_for_an_amount_and_reports_the_new_start() {
    let mut t = tab();
    apply_project_act(&mut t, ProjectAct::MoveTask);
    let prompt = v(&t).prompt.clone().expect("Move asks how far");
    assert_eq!((prompt.kind, prompt.uid), (PromptKind::Move, Some(2)));
    assert_eq!(prompt.kind.name(), "move");
    assert_eq!(
        prompt_label(&prompt, &v(&t).ed).as_ref(),
        "Move task by (1d / 1w / 4w; -1d back)"
    );
    for c in ["1", "w"] {
        project_input(&mut t, c, Some(c), Modifiers::default());
    }
    project_input(&mut t, "enter", None, Modifiers::default());
    assert_eq!(t.status.as_ref(), "Moved to 2026-01-12");
    assert_eq!(
        v(&t).ed.schedule().get(2).unwrap().early_start,
        projcore::DateTime::from_ymd_hm(2026, 1, 12, 8, 0)
    );
    assert!(t.dirty);
    // A refused amount says why and changes nothing.
    let before = v(&t).ed.project().clone();
    commit(&mut t, ProjectAct::MoveTask, "soon");
    assert_eq!(
        t.status.as_ref(),
        "Couldn't read 'soon' (try 1d, 1w, 4w, -1d)"
    );
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    // The status names the date the task is scheduled on: a milestone at
    // its predecessor's 17:00 finish moves to the next day, not two.
    let mut t = tab();
    vm(&mut t).ed.set_duration_min(2, 0).unwrap();
    vm(&mut t)
        .ed
        .add_predecessor(2, 1, LinkType::FinishStart, 0)
        .unwrap();
    commit(&mut t, ProjectAct::MoveTask, "1d");
    assert_eq!(t.status.as_ref(), "Moved to 2026-01-06");
    assert_eq!(
        v(&t).ed.disp_start(2).unwrap().day_number(),
        projcore::DateTime::from_ymd_hm(2026, 1, 6, 0, 0).day_number()
    );
    // So does a summary.
    let mut t = summary_tab();
    let before = v(&t).ed.project().clone();
    commit(&mut t, ProjectAct::MoveTask, "1d");
    assert_eq!(t.status.as_ref(), "Move a subtask, not a summary");
    assert_eq!(v(&t).ed.project(), &before);
    assert!(!t.dirty);
}

#[test]
fn scroll_to_task_puts_the_bar_a_day_in_from_the_left_edge() {
    let mut t = tab();
    // A narrow chart, so the pan's clamp does not decide where it lands.
    vm(&mut t).gantt_w = 100.;
    vm(&mut t).refresh_schedule_layout();
    let days = v(&t).scale.days;
    // Eight weeks on is past the scale the view last laid out: a stale
    // scale would clamp the pan short of the bar.
    vm(&mut t).ed.move_task(2, "8w").unwrap();
    assert!(
        v(&t).scale.days < 56,
        "the view's scale is stale ({days} days)"
    );
    vm(&mut t).ed.mark_saved();
    let before = v(&t).ed.project().clone();
    let status = t.status.clone();
    apply_project_act(&mut t, ProjectAct::ScrollToTask);
    // Monday 2026-01-05 + 56 days, less the one-day margin.
    assert_eq!(v(&t).gantt_x.get(), 55. * DAY_W);
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert_eq!(t.status, status);
    assert!(!t.dirty);
    assert_eq!(take_reveal(&t), None);
    // A bar at the chart's origin scrolls to the very start.
    vm(&mut t).ed.select(0);
    apply_project_act(&mut t, ProjectAct::ScrollToTask);
    assert_eq!(v(&t).gantt_x.get(), 0.);
}
