use super::*;
use crate::dialog_host::{dialog_click, dialog_key};
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
        vm(&mut t).ed.add_task(None, name, duration, false).unwrap();
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

/// Delete the selected task by Project's route: Delete on the ID column.
/// The cursor's column is put back afterwards.
fn delete_task(t: &mut DocTab) {
    let col = std::mem::replace(&mut vm(t).col, COL_ID);
    apply_project_act(t, ProjectAct::ClearCell);
    vm(t).col = col;
}

fn press(t: &mut DocTab, key: &str) {
    if let Some(act) = project_input(t, key, None, Modifiers::default()) {
        apply_project_act(t, act);
    }
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
        .filter(|a| !matches!(a, AddTask | InsertBlankRow))
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
        // Home and End stay on the row.
        ("home", Modifiers::default()),
        ("end", Modifiers::default()),
    ] {
        project_input(&mut t, key, None, modifiers);
        assert_eq!(take_reveal(&t), None, "{key}");
    }
    project_input(&mut t, "home", None, ctrl());
    assert_eq!(take_reveal(&t), Some(0));
    let old_uid = v(&t).ed.selected_uid();
    delete_task(&mut t);
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
                    Control::Large(c) | Control::Toggle(c) => vec![(c, false)],
                    Control::Column(c) => c.iter().map(|c| (c, false)).collect(),
                    // A split's menu may repeat its primary (Set Baseline...):
                    // the same command under the same letter, not a second one.
                    Control::Split { primary, menu } => [(primary, false)]
                        .into_iter()
                        .chain(menu.iter().map(|c| {
                            let again = matches!(
                                (c.act, primary.act),
                                (Act::Project(a), Act::Project(b)) if a == b
                            );
                            assert!(!again || c.key_tip == primary.key_tip, "{}", c.id);
                            (c, again)
                        }))
                        .collect(),
                    _ => panic!("unexpected control"),
                };
                for (c, again) in cmds {
                    let Act::Project(act) = c.act else {
                        panic!("wrong action")
                    };
                    if !again {
                        acts.push(act);
                        assert!(!c.key_tip.is_empty() && !keys.contains(&c.key_tip));
                        keys.push(c.key_tip);
                    }
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
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::View, false, false, false) == RibbonTab::View);
    assert!(valid_ribbon_tab(Kind::Xlsx, RibbonTab::Task, false, false, false) == RibbonTab::Home);
    for tab in [RibbonTab::TableDesign, RibbonTab::TableLayout] {
        assert!(valid_ribbon_tab(Kind::Docx, tab, true, false, false) == tab);
        assert!(valid_ribbon_tab(Kind::Docx, tab, false, false, false) == RibbonTab::Home);
        assert!(valid_ribbon_tab(Kind::Project, tab, true, true, false) == RibbonTab::Task);
    }
    for tab in [RibbonTab::Resource, RibbonTab::Report, RibbonTab::Project] {
        assert!(valid_ribbon_tab(Kind::Project, tab, false, false, false) == tab);
        assert!(valid_ribbon_tab(Kind::Docx, tab, false, false, false) == RibbonTab::Home);
        assert!(valid_ribbon_tab(Kind::Xlsx, tab, false, false, false) == RibbonTab::Home);
    }
    assert!(
        valid_ribbon_tab(Kind::Project, RibbonTab::Home, false, false, false) == RibbonTab::Task
    );
    // The contextual Gantt Chart Format tab: only a Project with its Gantt showing.
    let fmt = RibbonTab::GanttFormat;
    assert!(valid_ribbon_tab(Kind::Project, fmt, false, true, false) == fmt);
    assert!(valid_ribbon_tab(Kind::Project, fmt, false, false, false) == RibbonTab::Task);
    assert!(valid_ribbon_tab(Kind::Docx, fmt, false, true, false) == RibbonTab::Home);
    assert!(valid_ribbon_tab(Kind::Xlsx, fmt, false, true, false) == RibbonTab::Home);
    assert_eq!(ribbon_tab_name(fmt), "Gantt Chart Format");
    assert_eq!(ribbon_tab_index(RibbonTab::View, Kind::Project), 4);
    assert_eq!(ribbon_tab_index(RibbonTab::Project, Kind::Project), 3);
    assert_eq!(ribbon_tab_name(RibbonTab::Project), "Project");
    // Documents have the Design (#651), page Layout (#649) and Mailings
    // (#628) tabs before Review and View; workbooks have none of them.
    assert_eq!(ribbon_tab_index(RibbonTab::View, Kind::Docx), 6);
    assert_eq!(ribbon_tab_index(RibbonTab::Design, Kind::Docx), 2);
    assert_eq!(ribbon_tab_index(RibbonTab::Mailings, Kind::Docx), 4);
    assert!(
        valid_ribbon_tab(Kind::Docx, RibbonTab::Mailings, false, false, false)
            == RibbonTab::Mailings
    );
    assert!(
        valid_ribbon_tab(Kind::Xlsx, RibbonTab::Mailings, false, false, false) == RibbonTab::Home
    );
    // Workbooks have Excel's Data tab (#693) between Insert and Review;
    // documents do not.
    assert_eq!(ribbon_tab_index(RibbonTab::View, Kind::Xlsx), 4);
    assert_eq!(ribbon_tab_index(RibbonTab::Data, Kind::Xlsx), 2);
    assert!(valid_ribbon_tab(Kind::Xlsx, RibbonTab::Data, false, false, false) == RibbonTab::Data);
    assert!(valid_ribbon_tab(Kind::Docx, RibbonTab::Data, false, false, false) == RibbonTab::Home);
    assert!(
        valid_ribbon_tab(Kind::Docx, RibbonTab::Layout, false, false, false) == RibbonTab::Layout
    );
    assert!(
        valid_ribbon_tab(Kind::Xlsx, RibbonTab::Layout, false, false, false) == RibbonTab::Home
    );
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
                    Control::Split { primary, menu } => [primary].into_iter().chain(menu).collect(),
                    _ => panic!("unexpected control"),
                };
                for c in cmds {
                    assert_eq!(c.tip.body, "", "{}: the screentip has a title only", c.id);
                    paths.push((t.name, g.title, c.label, c.tip.title, c.act, c.key_tip));
                }
            }
        }
    }
    // (tab, group, Project's label, Project's screentip, act) — Project 2024.
    let want = [
        ("Task", "Schedule", "Indent", "Indent Task", Indent),
        ("Task", "Schedule", "Outdent", "Outdent Task", Outdent),
        (
            "Task",
            "Schedule",
            "Unlink Tasks",
            "Unlink Tasks",
            UnlinkTasks,
        ),
        (
            "Task",
            "Schedule",
            "Link Tasks",
            "Link the Selected Tasks",
            AddLink,
        ),
        ("Task", "Schedule", "Inactivate", "Inactivate", Inactivate),
        (
            "Task",
            "Tasks",
            "Manually Schedule",
            "Manually Schedule",
            ManuallySchedule,
        ),
        (
            "Task",
            "Tasks",
            "Auto Schedule",
            "Auto Schedule",
            AutoSchedule,
        ),
        ("Task", "Tasks", "Move", "Move Task", MoveTask),
        ("Task", "Insert", "Task", "Task", AddTask),
        ("Task", "Insert", "Milestone", "Insert Milestone", Milestone),
        (
            "Task",
            "Insert",
            "Blank Row",
            "Insert Blank Row",
            InsertBlankRow,
        ),
        (
            "Task",
            "Properties",
            "Information...",
            "View Task Information",
            Constraint,
        ),
        ("Task", "Editing", "Find...", "Find...", Find),
        (
            "Task",
            "Editing",
            "Scroll to Task",
            "Scroll to Task",
            ScrollToTask,
        ),
        (
            "Resource",
            "Assignments",
            "Assign Resources...",
            "Assign Resources...",
            Assign,
        ),
        ("Resource", "Level", "Level All", "Level All", LevelAll),
        (
            "Resource",
            "Level",
            "Clear Leveling",
            "Clear Leveling",
            ClearLeveling,
        ),
        (
            "Project",
            "Schedule",
            "Calculate Project",
            "Calculate Project",
            Recalc,
        ),
        (
            "Project",
            "Schedule",
            "Set Baseline",
            "Set Baseline",
            Baseline,
        ),
        // Set Baseline's menu (#397); Project 2024's labels, with the
        // screentips naming the commands, so `ribbon-click "Clear Baseline"`
        // still finds it.
        (
            "Project",
            "Schedule",
            "Set Baseline...",
            "Set Baseline",
            Baseline,
        ),
        (
            "Project",
            "Schedule",
            "Clear Baseline...",
            "Clear Baseline",
            ClearBaseline,
        ),
        (
            "View",
            "Data",
            "Show Subtasks",
            "Show Subtasks",
            ShowSubtasks,
        ),
        (
            "View",
            "Data",
            "Hide Subtasks",
            "Hide Subtasks",
            HideSubtasks,
        ),
        ("View", "Split View", "Timeline", "Timeline View", Timeline),
        (
            "Gantt Chart Format",
            "Bar Styles",
            "Critical Tasks",
            "Critical Tasks",
            CriticalTasks,
        ),
        (
            "Gantt Chart Format",
            "Bar Styles",
            "Baseline",
            "Baseline",
            BaselineBars,
        ),
    ];
    for (tab, group, label, tip, act) in want {
        let path = format!("{tab} > {group} > {label}");
        let Some((_, _, _, found_tip, found, key)) = paths
            .iter()
            .find(|p| (p.0, p.1, p.2) == (tab, group, label))
        else {
            panic!("missing ribbon path {path}");
        };
        assert_eq!(*found_tip, tip, "{path} screentip");
        assert!(matches!(found, Act::Project(a) if *a == act), "{path}");
        let t = tabs.iter().find(|t| t.name == tab).unwrap();
        assert!(
            matches!(tab_keytip_cmd(t, key), Some(Act::Project(a)) if a == act),
            "{path} KeyTip {key}"
        );
    }
    // Only Project's commands: nothing on the ribbon beyond the list above.
    assert_eq!(paths.len(), want.len(), "commands on the Project ribbon");
    // Project's Report groups are not implemented; the tab stays (#72).
    let report = r.tabs.iter().find(|t| t.name == "Report").unwrap();
    assert!(report.groups.is_empty());
}

/// docxy's own commands are off Project's ribbon (#370); each keeps its
/// keyboard, cell or backstage route.
#[test]
fn docxy_only_commands_are_off_the_ribbon() {
    use ProjectAct::*;
    let r = project_ribbon();
    let fmt = gantt_format_tab();
    let cmds: Vec<_> = r
        .tabs
        .iter()
        .chain([&fmt])
        .flat_map(|t| &t.groups)
        .flat_map(|g| &g.items)
        .flat_map(|item| match item {
            Control::Large(c) | Control::Toggle(c) => vec![c],
            Control::Column(commands) => commands.iter().collect(),
            Control::Split { primary, menu } => [primary].into_iter().chain(menu).collect(),
            _ => vec![],
        })
        .collect();
    for id in [
        "pr-rename",
        "pr-duration",
        "pr-delete",
        "pr-clear",
        "pr-export",
        "pr-left",
        "pr-right",
        "pr-start",
    ] {
        assert!(!cmds.iter().any(|c| c.id == id), "{id} is on the ribbon");
    }
    for gone in [
        Save,
        Level,
        ExportGantt,
        ScrollLeft,
        ScrollRight,
        GoToStart,
        DeleteTask,
    ] {
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c.act, Act::Project(a) if a == gone)),
            "{gone:?} is not on Project's ribbon"
        );
    }
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    let ctrl = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    assert_eq!(key_act("e", ctrl), Some(ExportGantt));
    assert_eq!(key_act("left", alt), Some(ScrollLeft));
    assert_eq!(key_act("right", alt), Some(ScrollRight));
    assert_eq!(key_act("home", alt), Some(GoToStart));
}

#[test]
fn inactivate_ribbon_toggles_a_summary_and_reports_the_count() {
    let mut t = tab();
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.select(0);
    apply_project_act(&mut t, ProjectAct::Inactivate);
    assert_eq!(t.status.as_ref(), "Inactivated 2 tasks");
    assert!(project_act_active(v(&t), ProjectAct::Inactivate));
    assert!(
        v(&t)
            .ed
            .project()
            .tasks
            .iter()
            .all(|task| !task.is_active())
    );
    apply_project_act(&mut t, ProjectAct::Inactivate);
    assert_eq!(t.status.as_ref(), "Activated 2 tasks");
    assert!(!project_act_active(v(&t), ProjectAct::Inactivate));
}

#[test]
fn inactivate_refuses_blank_and_inherited_inactivity() {
    let mut t = tab();
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.select(0);
    apply_project_act(&mut t, ProjectAct::Inactivate);
    let depth = v(&t).ed.undo_depth();
    vm(&mut t).ed.select(1);
    // This project has an inactive child because the summary was toggled.
    // Restore only the child's own flag through a file-style project.
    let mut proj = v(&t).ed.project().clone();
    proj.tasks[1].active = None;
    t.surface = Surface::Project(ProjectView::new(proj, false));
    vm(&mut t).ed.select(1);
    apply_project_act(&mut t, ProjectAct::Inactivate);
    assert_eq!(t.status.as_ref(), "Its summary task is inactive");
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(project_act_active(v(&t), ProjectAct::Inactivate));
    assert!(depth > 0);

    let blank = v(&t).ed.project().tasks.len();
    vm(&mut t).ed.insert_blank_row(None).unwrap();
    vm(&mut t).ed.select(blank);
    let before = v(&t).ed.undo_depth();
    apply_project_act(&mut t, ProjectAct::Inactivate);
    assert!(t.status.contains("blank row"));
    assert_eq!(v(&t).ed.undo_depth(), before);
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
fn set_baseline_status_says_whether_baseline_bars_show() {
    let mut t = tab();
    apply_project_act(&mut t, ProjectAct::Baseline);
    assert_eq!(
        t.status.as_ref(),
        "Baseline set — baseline bars now show under the current bars"
    );
    apply_project_act(&mut t, ProjectAct::BaselineBars);
    apply_project_act(&mut t, ProjectAct::Baseline);
    assert_eq!(
        t.status.as_ref(),
        "Baseline set — baseline bars are hidden (Gantt Chart Format › Baseline)"
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
fn project_act_active_checks_level_all_timeline_bar_styles_and_the_task_mode_only() {
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
        ("insert", InsertBlankRow),
        ("delete", ClearCell),
        ("f3", FindNext),
        ("f11", NewProject),
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
    assert_eq!(key_act("delete", ctrl()), Some(ResetCell));
    for m in [
        ctrl_shift(),
        ctrl_alt(),
        Modifiers {
            platform: true,
            ..ctrl()
        },
    ] {
        assert_eq!(key_act("delete", m), None, "{m:?}");
    }
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
    apply_project_act(&mut t, Constraint);
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
    apply_project_act(&mut t, ProjectAct::MoveTask);
    let p = v(&t).prompt.clone().unwrap();
    assert_eq!(p.uid, Some(2));
    vm(&mut t).ed.select(0);
    commit_prompt(
        &mut t,
        ProjectPrompt {
            buf: "1d".into(),
            ..p
        },
    );
    let moved = |t: &DocTab, i: usize| {
        v(t).ed.project().tasks[i].constraint == projcore::ConstraintType::StartNoEarlierThan
    };
    assert!(!moved(&t, 0));
    assert!(moved(&t, 1), "the prompt edits the task it opened on");
    apply_project_act(&mut t, ProjectAct::MoveTask);
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
    apply_project_act(&mut t, ProjectAct::Constraint);
    vm(&mut t).select_row(1);
    assert!(v(&t).prompt.is_none());
    apply_project_act(&mut t, ProjectAct::Constraint);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert!(v(&t).prompt.is_none());
    // An empty plan has only the entry row, where task prompts do not open.
    let mut empty = new_project_tab();
    let status = empty.status.clone();
    apply_project_act(&mut empty, ProjectAct::Constraint);
    assert!(v(&empty).prompt.is_none());
    assert_eq!(empty.status, status);
    assert!(!empty.dirty);
}

#[test]
fn rejection_preserves_model_geometry_and_both_history_stacks() {
    use ProjectAct::*;
    for (act, text, message) in [
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
        (Indent, None, true),
        (Outdent, None, true),
        (AddLink, Some("1"), true),
        (Constraint, Some("SNET 2026-01-08"), true),
        (Assign, Some("Alice"), false),
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
            Indent => assert_eq!(after.task(2).unwrap().outline_level, 2),
            Outdent => assert_eq!(after.task(2).unwrap().outline_level, 1),
            AddLink => assert_eq!(after.task(2).unwrap().predecessors[0].uid, 1),
            Constraint => assert_ne!(
                before.task(2).unwrap().constraint,
                after.task(2).unwrap().constraint
            ),
            Assign => {
                assert_eq!(after.resources.len(), 1);
                assert_eq!(after.assignments.len(), 1);
            }
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

/// The removed docxy-only buttons' routes (#370): Rename and Duration are cell
/// edits, Delete Task is Delete on the ID column and Clear Resources is Delete
/// on the Resource Names cell; each undoes and redoes as one step.
#[test]
fn cell_routes_for_removed_ribbon_commands_undo_and_redo() {
    for (route, col, text, changes_geometry) in [
        ("delete task", COL_ID, None, true),
        ("rename", COL_NAME, Some("Renamed"), false),
        ("duration", COL_DURATION, Some("3d"), true),
        ("clear resources", COL_RESOURCES, None, false),
    ] {
        let mut t = tab();
        if col == COL_RESOURCES {
            vm(&mut t).ed.assign_resource(2, "Alice").unwrap();
        }
        let before = v(&t).ed.project().clone();
        let before_geom = geometry(&t);
        vm(&mut t).col = col;
        match text {
            Some(text) => {
                project_input(&mut t, "text", Some(text), Modifiers::default());
                press(&mut t, "enter");
                assert!(v(&t).cell.is_none(), "{route}");
            }
            None => press(&mut t, "delete"),
        }
        let after = v(&t).ed.project().clone();
        assert_ne!(before, after, "{route}");
        assert_eq!(geometry(&t) != before_geom, changes_geometry, "{route}");
        match route {
            "delete task" => assert_eq!(after.tasks.len(), 1),
            "rename" => assert_eq!(after.task(2).unwrap().name, "Renamed"),
            "duration" => assert_eq!(after.task(2).unwrap().duration_min, 3 * 480),
            _ => assert!(after.assignments.is_empty()),
        }
        apply_project_act(&mut t, ProjectAct::Undo);
        assert_eq!(v(&t).ed.project(), &before, "{route}");
        apply_project_act(&mut t, ProjectAct::Redo);
        assert_eq!(v(&t).ed.project(), &after, "{route}");
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

/// Project's Alt+Home moves the timescale to the project start; it replaces
/// the ribbon's Go to Start (#370). Plain Home still moves the cell cursor.
#[test]
fn alt_home_goes_to_start() {
    let mut t = tab();
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    vm(&mut t).col = COL_DURATION;
    vm(&mut t).pan_gantt(true);
    vm(&mut t).pan_gantt(true);
    assert!(v(&t).gantt_x.get() > 0.);
    let (sel, col) = (v(&t).ed.sel(), v(&t).col);
    let act = project_input(&mut t, "home", None, alt);
    assert_eq!(act, Some(ProjectAct::GoToStart));
    apply_project_act(&mut t, act.unwrap());
    assert_eq!(v(&t).gantt_x.get(), 0.);
    assert_eq!((v(&t).ed.sel(), v(&t).col), (sel, col), "the cursor stays");
    vm(&mut t).pan_gantt(true);
    press(&mut t, "home");
    assert_eq!(v(&t).col, COL_ID, "plain Home goes to the row's first cell");
    assert!(v(&t).gantt_x.get() > 0., "plain Home leaves the timescale");
}

/// Project's Alt keys get past the KeyTips overlay (pressing Alt starts it),
/// so Alt+Home works after F10 as Alt+Left/Right do; KeyTip letters and
/// digits never do.
#[test]
fn project_alt_keys_bypass_keytips_but_letters_do_not() {
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    let alt_shift = Modifiers { shift: true, ..alt };
    for (key, m) in [
        ("left", alt),
        ("right", alt),
        ("home", alt),
        ("right", alt_shift),
        ("left", alt_shift),
        ("-", alt_shift),
        ("=", alt_shift),
    ] {
        assert!(project_alt_key(key, m), "{key} {m:?}");
    }
    let ctrl_alt = Modifiers {
        control: true,
        ..alt
    };
    assert!(!project_alt_key("home", ctrl_alt));
    assert!(!project_alt_key("home", Modifiers::default()));
    assert!(!project_alt_key("alt", alt));
    for c in ('a'..='z').chain('0'..='9') {
        let key = c.to_string();
        for m in [alt, alt_shift] {
            assert!(!project_alt_key(&key, m), "KeyTip {key} {m:?}");
        }
    }
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

/// A key as the window delivers it: an open dialog takes it first, as
/// `on_key` does, and only then does the Project see it.
fn key(t: &mut DocTab, key: &str, text: Option<&str>, m: Modifiers) {
    if dialog_key(t, key, text, m) {
        return;
    }
    if let Some(act) = project_input(t, key, text, m) {
        apply_project_act(t, act);
    }
}

#[test]
fn deleting_a_summary_asks_first_and_escape_changes_nothing() {
    let mut t = summary_tab();
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    delete_task(&mut t);
    assert!(v(&t).prompt.is_none(), "a message box, not the prompt bar");
    let d = t.dialogs.top().expect("a summary delete asks first");
    assert_eq!(d.id, "delete-summary");
    assert_eq!(d.text.as_deref(), Some("Delete 'First' and its 1 subtask?"));
    let labels: Vec<_> = d
        .buttons
        .iter()
        .map(|b| (b.label.as_str(), b.default))
        .collect();
    assert_eq!(labels, [("Yes", true), ("No", false)]);
    assert_eq!(tab_app_state(&t), Some(AppState::Edit));
    assert_eq!(v(&t).ed.project(), &before);

    // Typing, chords and Tab reach neither the dialog nor the plan under it.
    for (k, text, m) in [
        ("d", Some("d"), Modifiers::default()),
        ("backspace", None, Modifiers::default()),
        ("delete", None, Modifiers::default()),
        ("tab", None, Modifiers::default()),
        ("z", None, ctrl()),
        ("enter", None, ctrl()),
        ("alt", None, Modifiers::default()),
    ] {
        key(&mut t, k, text, m);
        assert_eq!(t.dialogs.top_id(), "delete-summary", "{k}");
    }
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert!(v(&t).cell.is_none() && v(&t).prompt.is_none());

    key(&mut t, "escape", None, Modifiers::default());
    assert!(!t.dialogs.is_open());
    assert_eq!(tab_app_state(&t), Some(AppState::Ready));
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert!(!t.dirty);

    // No is the same as Escape.
    delete_task(&mut t);
    dialog_click(&mut t, "No").unwrap();
    assert!(!t.dialogs.is_open());
    assert_eq!(v(&t).ed.project(), &before);
}

#[test]
fn confirming_a_summary_delete_removes_its_subtree_in_one_undo_step() {
    let mut t = summary_tab();
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    delete_task(&mut t);
    key(&mut t, "enter", None, Modifiers::default());
    assert!(!t.dialogs.is_open());
    assert!(v(&t).ed.project().tasks.is_empty());
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    assert!(t.dirty);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project(), &before);
    // Emptying the plan latched the entry row, so select the summary again.
    assert!(v(&t).on_entry_row());
    project_cell_click(&mut t, 0, None, false);

    // The drawn Yes button and `dialog-click` press it through the same path
    // as Enter; a label is matched without regard to case.
    delete_task(&mut t);
    dialog_click(&mut t, "yes").unwrap();
    assert!(v(&t).ed.project().tasks.is_empty());
    assert_eq!(
        dialog_click(&mut t, "Yes").unwrap_err(),
        crate::dialog::NONE_OPEN
    );
}

#[test]
fn the_delete_message_box_counts_the_subtasks() {
    let mut t = summary_tab();
    let uid = v(&t).selected_uid().unwrap();
    let d = delete_summary_dialog(&v(&t).ed, uid, 2);
    assert_eq!(
        d.text.as_deref(),
        Some("Delete 'First' and its 2 subtasks?")
    );
    assert_eq!(d.title, "Delete Task");
    vm(&mut t).ed.select(1);
    delete_task(&mut t);
    assert!(!t.dialogs.is_open(), "a leaf is deleted without asking");
}

#[test]
fn deleting_a_leaf_needs_no_confirmation() {
    let mut t = summary_tab();
    vm(&mut t).ed.select(1);
    delete_task(&mut t);
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
        Milestone,
        Indent,
        Outdent,
        AddLink,
        Constraint,
        Assign,
        Hyperlink,
        UnlinkTasks,
        MoveTask,
        ScrollToTask,
        ShowSubtasks,
        HideSubtasks,
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
fn add_task_on_the_entry_row_appends_and_selects_a_new_task() {
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
fn insert_task_follows_the_plans_new_tasks_estimated() {
    for (stated, estimated) in [(None, Some(true)), (Some(false), None)] {
        let mut t = tab();
        let mut p = v(&t).ed.project().clone();
        p.new_tasks_estimated = stated;
        t.surface = Surface::Project(ProjectView::new(p, false));
        vm(&mut t).ed.select(0);
        apply_project_act(&mut t, ProjectAct::AddTask);
        let task = &v(&t).ed.project().tasks[1];
        assert_eq!(task.name, "New task");
        assert_eq!(task.estimated, estimated, "{stated:?}");
    }
}

#[test]
fn blank_row_goes_above_the_selected_row_or_the_entry_row_and_is_selected() {
    let mut t = tab();
    t.dirty = false;
    // On "Second": the blank row goes above it and takes the cursor.
    project_cell_click(&mut t, 1, None, false);
    apply_project_act(&mut t, ProjectAct::InsertBlankRow);
    let rows = |t: &DocTab| -> Vec<(String, bool)> {
        v(t).ed
            .project()
            .tasks
            .iter()
            .map(|t| (t.name.clone(), t.is_null))
            .collect()
    };
    let row = |name: &str, blank| (name.to_string(), blank);
    assert_eq!(
        rows(&t),
        [row("First", false), row("", true), row("Second", false)]
    );
    assert_eq!(v(&t).cursor_row(), 1);
    assert_eq!(take_reveal(&t), Some(1));
    assert!(t.dirty);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    // On the entry row it appends: just above the entry row.
    project_entry_click(&mut t, None, false);
    apply_project_act(&mut t, ProjectAct::InsertBlankRow);
    assert_eq!(rows(&t).len(), 4);
    assert!(v(&t).ed.project().tasks[3].is_null);
    assert!(!v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 3);
    assert_eq!(take_reveal(&t), Some(3));
    // One Undo each.
    apply_project_act(&mut t, ProjectAct::Undo);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(rows(&t), [row("First", false), row("Second", false)]);
}

#[test]
fn insert_key_puts_blank_rows_above_the_cursor_in_the_same_column() {
    // As the host's key route does: project_input maps, then the act applies.
    fn press_insert(t: &mut DocTab) {
        let act = project_input(t, "insert", None, Modifiers::default());
        assert_eq!(act, Some(ProjectAct::InsertBlankRow));
        apply_project_act(t, act.unwrap());
    }
    let rows = |t: &DocTab| -> Vec<(String, bool)> {
        v(t).ed
            .project()
            .tasks
            .iter()
            .map(|t| (t.name.clone(), t.is_null))
            .collect()
    };
    let row = |name: &str, blank| (name.to_string(), blank);
    let mut t = tab();
    project_cell_click(&mut t, 1, Some(COL_NAME), false);
    press_insert(&mut t);
    assert_eq!(
        rows(&t),
        [row("First", false), row("", true), row("Second", false)]
    );
    assert_eq!(v(&t).cursor_row(), 1);
    assert_eq!(v(&t).col, COL_NAME, "the cursor keeps its column");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    // Again: a second blank row, above the first, still above "Second".
    press_insert(&mut t);
    assert_eq!(
        rows(&t),
        [
            row("First", false),
            row("", true),
            row("", true),
            row("Second", false)
        ]
    );
    assert_eq!(v(&t).cursor_row(), 1);
    assert_eq!(v(&t).col, COL_NAME);
    assert_eq!(v(&t).ed.undo_depth(), 2);
    apply_project_act(&mut t, ProjectAct::Undo);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(rows(&t), [row("First", false), row("Second", false)]);
    // On the entry row: a blank row just above it, not a named task.
    project_entry_click(&mut t, Some(COL_DURATION), false);
    press_insert(&mut t);
    assert_eq!(
        rows(&t),
        [row("First", false), row("Second", false), row("", true)]
    );
    assert!(!v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 2);
    assert_eq!(v(&t).col, COL_DURATION);
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
    delete_task(&mut t);
    delete_task(&mut t);
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
        prompt_label(&prompt).as_ref(),
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
    vm(&mut t).ed.set_duration_min(2, 0, false).unwrap();
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

// ---- View > Data > Outline: Show/Hide Subtasks (#155) ----

/// Phase over X and Y, then Z; saved, with Phase selected.
fn outline_tab() -> DocTab {
    let mut t = new_project_tab();
    for name in ["Phase", "X", "Y", "Z"] {
        vm(&mut t).ed.add_task(None, name, 480, false).unwrap();
    }
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.indent(3, 1).unwrap();
    let p = v(&t).ed.project().clone();
    t.surface = Surface::Project(ProjectView::new(p, false));
    vm(&mut t).ed.select(0);
    t
}

fn alt_shift() -> Modifiers {
    Modifiers {
        alt: true,
        shift: true,
        ..Modifiers::default()
    }
}

#[test]
fn alt_shift_minus_and_plus_hide_and_show_subtasks() {
    use ProjectAct::*;
    for (key, act) in [
        ("-", HideSubtasks),
        ("_", HideSubtasks),
        ("=", ShowSubtasks),
        ("+", ShowSubtasks),
    ] {
        assert_eq!(key_act(key, alt_shift()), Some(act), "{key}");
    }
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    assert_eq!(key_act("-", alt), None);
    assert_eq!(key_act("=", alt), None);
    assert_eq!(key_act("-", Modifiers::default()), None);
    let mut t = outline_tab();
    assert_eq!(
        project_input(&mut t, "-", Some("_"), alt_shift()),
        Some(HideSubtasks),
        "routed to the host, not typed into a cell"
    );
    assert!(v(&t).cell.is_none());
}

#[test]
fn hide_and_show_subtasks_act_on_the_selected_summary_as_view_state() {
    use ProjectAct::*;
    let mut t = outline_tab();
    let before = v(&t).ed.project().clone();
    apply_project_act(&mut t, HideSubtasks);
    assert!(v(&t).ed.is_collapsed(1));
    assert_eq!(v(&t).ed.visible_rows(), [0, 3]);
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(!t.dirty);
    apply_project_act(&mut t, ShowSubtasks);
    assert!(!v(&t).ed.is_collapsed(1));
    assert_eq!(v(&t).ed.visible_rows(), [0, 1, 2, 3]);

    // On a subtask, Hide collapses its summary, which takes the cursor
    // (and the list scrolls to it).
    project_cell_click(&mut t, 2, None, false);
    take_reveal(&t);
    apply_project_act(&mut t, HideSubtasks);
    assert!(v(&t).ed.is_collapsed(1));
    assert_eq!((v(&t).cursor_row(), v(&t).display_row()), (0, 0));
    assert_eq!(take_reveal(&t), Some(0));

    // Nothing to show or hide on a top-level task.
    project_cell_click(&mut t, 3, None, false);
    apply_project_act(&mut t, HideSubtasks);
    assert_eq!(t.status.as_ref(), "The task has no subtasks to hide");
    apply_project_act(&mut t, ShowSubtasks);
    assert_eq!(
        t.status.as_ref(),
        "Only a summary task has subtasks to show or hide"
    );
    assert!(v(&t).ed.is_collapsed(1));
    assert_eq!(v(&t).cursor_row(), 3);
}

#[test]
fn arrow_keys_and_ctrl_up_and_down_move_over_visible_rows() {
    let mut t = outline_tab();
    apply_project_act(&mut t, ProjectAct::HideSubtasks);
    let key = |t: &mut DocTab, k, m| {
        project_input(t, k, None, m);
        (v(t).cursor_row(), v(t).display_row())
    };
    let plain = Modifiers::default();
    // Row indexes stay task indexes; the display row counts shown rows.
    assert_eq!(key(&mut t, "down", plain), (3, 1));
    assert_eq!(key(&mut t, "down", plain), (4, 2), "then the entry row");
    assert!(v(&t).on_entry_row());
    assert_eq!(key(&mut t, "up", plain), (3, 1));
    assert_eq!(key(&mut t, "up", plain), (0, 0));
    assert_eq!(key(&mut t, "down", ctrl()), (3, 1));
    assert_eq!(key(&mut t, "up", ctrl()), (0, 0));
    assert!(v(&t).ed.is_collapsed(1), "moving never expands");
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

/// (cursor row, column, on the entry row, a cell editor open, dirty)
fn cursor(t: &DocTab) -> (usize, usize, bool, bool, bool) {
    let v = v(t);
    (
        v.cursor_row(),
        v.col,
        v.on_entry_row(),
        v.cell.is_some(),
        t.dirty || v.ed.dirty(),
    )
}

#[test]
fn home_and_end_move_to_the_rows_first_and_last_field() {
    let mut t = tab();
    vm(&mut t).col = COL_NAME;
    // Ctrl+Left/Right are Project's aliases for Home/End.
    for (key, m, col) in [
        ("end", Modifiers::default(), COLUMN_COUNT - 1),
        ("home", Modifiers::default(), 0),
        ("right", ctrl(), COLUMN_COUNT - 1),
        ("left", ctrl(), 0),
    ] {
        assert_eq!(project_input(&mut t, key, None, m), None, "{key}");
        assert_eq!(cursor(&t), (1, col, false, false, false), "{key}");
        assert_eq!(v(&t).ed.sel(), 1, "{key}");
        assert_eq!(take_reveal(&t), None, "{key}");
    }
    // End reveals the Resource Names column, Home scrolls back to ID.
    vm(&mut t).table_w = 200.;
    press(&mut t, "end");
    assert!(v(&t).table_x.get() > 0.);
    press(&mut t, "home");
    assert_eq!(v(&t).table_x.get(), 0.);
    // On the entry row they move the column and stay there.
    vm(&mut t).enter_entry_row();
    press(&mut t, "end");
    assert_eq!(cursor(&t), (2, COLUMN_COUNT - 1, true, false, false));
    press(&mut t, "home");
    assert_eq!(cursor(&t), (2, 0, true, false, false));
}

#[test]
fn ctrl_home_and_end_go_to_the_first_and_last_task_and_field() {
    let mut t = outline_tab();
    apply_project_act(&mut t, ProjectAct::HideSubtasks);
    vm(&mut t).enter_entry_row();
    take_reveal(&t);
    vm(&mut t).col = COL_NAME;
    // Ctrl+Up/Down keep the column; Shift is ignored.
    let shift = Modifiers {
        shift: true,
        ..ctrl()
    };
    for (key, m, row, col) in [
        ("up", ctrl(), 0, COL_NAME),
        ("down", shift, 3, COL_NAME),
        ("up", shift, 0, COL_NAME),
        ("end", ctrl(), 3, COLUMN_COUNT - 1),
        ("home", ctrl(), 0, 0),
    ] {
        assert_eq!(project_input(&mut t, key, None, m), None, "{key}");
        assert_eq!(cursor(&t), (row, col, false, false, false), "{key}");
        assert_eq!(take_reveal(&t), Some(v(&t).display_row()), "{key}");
    }
    // Already there: nothing to reveal.
    project_input(&mut t, "up", None, ctrl());
    assert_eq!(take_reveal(&t), None);
    assert!(v(&t).ed.is_collapsed(1), "moving never expands");
}

#[test]
fn ctrl_row_jumps_on_an_empty_plan_move_only_the_column() {
    let mut t = new_project_tab();
    vm(&mut t).col = COL_NAME;
    for (key, col) in [
        ("up", COL_NAME),
        ("down", COL_NAME),
        ("end", COLUMN_COUNT - 1),
        ("home", 0),
    ] {
        project_input(&mut t, key, None, ctrl());
        assert_eq!(cursor(&t), (0, col, true, false, false), "{key}");
    }
}

#[test]
fn ctrl_navigation_leaves_ctrl_shortcuts_the_cell_editor_and_prompts_alone() {
    use ProjectAct::*;
    let mut t = tab();
    let shift = Modifiers {
        shift: true,
        ..ctrl()
    };
    assert_eq!(project_input(&mut t, "l", None, shift), Some(Level));
    assert_eq!(project_input(&mut t, "f", None, ctrl()), Some(Find));
    assert_eq!(project_input(&mut t, "home", None, ctrl_alt()), None);
    assert_eq!(
        cursor(&t),
        (1, COL_NAME, false, false, false),
        "Ctrl+Alt+Home"
    );
    // An open cell editor keeps its row, and Home/End move its caret.
    press(&mut t, "f2");
    project_input(&mut t, "home", None, ctrl());
    assert_eq!(cursor(&t), (1, COL_NAME, false, true, false));
    press(&mut t, "home");
    assert_eq!(v(&t).cell.as_ref().unwrap().caret, 0);
    press(&mut t, "end");
    let cell = v(&t).cell.as_ref().unwrap();
    assert_eq!((cell.caret, cell.buf.as_str()), ("Second".len(), "Second"));
    press(&mut t, "escape");
    // So does an open prompt.
    apply_project_act(&mut t, Find);
    project_input(&mut t, "up", None, ctrl());
    assert!(v(&t).prompt.is_some());
    assert_eq!(v(&t).cursor_row(), 1);
}

fn ctrl_alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..ctrl()
    }
}

#[test]
fn the_entry_row_below_a_collapsed_last_summary_keeps_it_collapsed() {
    let mut t = outline_tab();
    vm(&mut t).ed.delete_task(4).unwrap();
    apply_project_act(&mut t, ProjectAct::HideSubtasks);
    project_input(&mut t, "down", None, Modifiers::default());
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).display_row(), 1);
    assert!(v(&t).ed.is_collapsed(1));
    // Find from there still starts at the first task.
    commit(&mut t, ProjectAct::Find, "x");
    assert_eq!(v(&t).cursor_row(), 1, "{}", t.status);
    assert!(
        !v(&t).ed.is_collapsed(1),
        "a hidden hit shows its summary's subtasks"
    );
}

#[test]
fn list_rows_map_to_task_indexes_for_clicks() {
    let mut t = outline_tab();
    apply_project_act(&mut t, ProjectAct::HideSubtasks);
    let rows = v(&t).ed.visible_rows();
    let n = v(&t).ed.project().tasks.len();
    assert_eq!(
        (0..3).map(|r| list_task(&rows, n, r)).collect::<Vec<_>>(),
        [0, 3, n],
        "Phase, Z, then the entry row"
    );
    // A click on the second list row lands on Z, not the hidden X.
    project_cell_click(&mut t, list_task(&rows, n, 1), None, false);
    assert_eq!(v(&t).ed.selected_uid(), Some(4));
    assert!(v(&t).ed.is_collapsed(1));
}

#[test]
fn the_outline_glyph_toggles_without_moving_the_cursor() {
    let mut t = outline_tab();
    project_cell_click(&mut t, 3, None, false);
    take_reveal(&t);
    toggle_project_collapse(&mut t, 1);
    assert!(v(&t).ed.is_collapsed(1));
    assert_eq!(v(&t).ed.selected_uid(), Some(4));
    assert!(v(&t).cell.is_none(), "no cell edit starts");
    assert_eq!(take_reveal(&t), None, "the cursor did not move");
    toggle_project_collapse(&mut t, 1);
    assert!(!v(&t).ed.is_collapsed(1));
    assert!(!t.dirty);
    // On a task that is not a summary it only reports.
    toggle_project_collapse(&mut t, 4);
    assert_eq!(
        t.status.as_ref(),
        "Only a summary task has subtasks to show or hide"
    );
}

#[test]
fn the_outline_glyph_commits_an_open_cell_edit_first() {
    let mut t = outline_tab();
    project_cell_click(&mut t, 2, Some(COL_NAME), false);
    vm(&mut t).open_cell(Some("Renamed")).unwrap();
    toggle_project_collapse(&mut t, 1);
    assert_eq!(v(&t).ed.project().tasks[2].name, "Renamed");
    assert!(v(&t).cell.is_none());
    assert!(v(&t).ed.is_collapsed(1));
    assert_eq!(
        v(&t).ed.selected_uid(),
        Some(1),
        "the hidden cursor moves up"
    );

    // A commit that fails keeps the edit and the outline.
    toggle_project_collapse(&mut t, 1);
    project_cell_click(&mut t, 2, Some(COL_DURATION), false);
    vm(&mut t).open_cell(Some("soon")).unwrap();
    toggle_project_collapse(&mut t, 1);
    assert!(v(&t).cell.is_some());
    assert!(!v(&t).ed.is_collapsed(1));
    assert_eq!(v(&t).ed.selected_uid(), Some(3));
}

#[test]
fn a_reveal_scrolls_to_the_row_as_shown() {
    let mut t = outline_tab();
    apply_project_act(&mut t, ProjectAct::HideSubtasks);
    take_reveal(&t);
    // Z is task 3, but the second row shown: X and Y are hidden.
    commit(&mut t, ProjectAct::Find, "z");
    assert_eq!((v(&t).cursor_row(), v(&t).display_row()), (3, 1));
    assert_eq!(take_reveal(&t), Some(1));
    assert!(v(&t).ed.is_collapsed(1));
}

#[test]
fn indenting_under_a_collapsed_summary_scrolls_to_the_task() {
    use ProjectAct::*;
    let mut t = outline_tab();
    // Phase is collapsed; Z sits right below it, at its level.
    apply_project_act(&mut t, HideSubtasks);
    project_cell_click(&mut t, 3, None, false);
    take_reveal(&t);
    assert_eq!((v(&t).cursor_row(), v(&t).display_row()), (3, 1));
    apply_project_act(&mut t, Indent);
    // Z joined Phase, which shows its subtasks: Z is shown below X and Y.
    assert!(!v(&t).ed.is_collapsed(1));
    assert_eq!((v(&t).cursor_row(), v(&t).display_row()), (3, 3));
    assert_eq!(take_reveal(&t), Some(3));
    // The keyboard route dispatches the same act.
    apply_project_act(&mut t, HideSubtasks);
    take_reveal(&t);
    assert_eq!(
        project_input(
            &mut t,
            "right",
            None,
            Modifiers {
                alt: true,
                shift: true,
                ..Modifiers::default()
            }
        ),
        Some(Indent)
    );
}

#[test]
fn delete_on_name_clears_the_name_and_keeps_the_task() {
    let mut t = tab();
    vm(&mut t).ed.set_duration_min(1, 960, false).unwrap();
    vm(&mut t)
        .ed
        .add_predecessor(2, 1, LinkType::FinishStart, 0)
        .unwrap();
    vm(&mut t).ed.assign_resource(1, "Alice").unwrap();
    let before = v(&t).ed.project().clone();
    vm(&mut t).ed.mark_saved();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    let depth = v(&t).ed.undo_depth();
    press(&mut t, "delete");
    let after = v(&t).ed.project().clone();
    assert_eq!(after.tasks.len(), 2);
    assert_eq!(after.tasks[0].name, "");
    assert_eq!(after.tasks[0].duration_min, 960);
    assert_eq!(after.tasks[1], before.tasks[1]);
    assert_eq!(after.assignments, before.assignments);
    assert_eq!(after.resources, before.resources);
    assert_eq!(after.tasks[0].predecessors, before.tasks[0].predecessors);
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    assert!(t.dirty);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project(), &before);
    apply_project_act(&mut t, ProjectAct::Redo);
    assert_eq!(v(&t).ed.project(), &after);
}

#[test]
fn delete_on_predecessors_and_resources_clears_only_that_field() {
    for col in [COL_PREDECESSORS, COL_RESOURCES] {
        let mut t = tab();
        vm(&mut t)
            .ed
            .add_predecessor(2, 1, LinkType::FinishStart, 0)
            .unwrap();
        vm(&mut t).ed.assign_resource(2, "Alice").unwrap();
        vm(&mut t).ed.mark_saved();
        vm(&mut t).col = col;
        let before = v(&t).ed.project().clone();
        let depth = v(&t).ed.undo_depth();
        press(&mut t, "delete");
        let after = v(&t).ed.project();
        assert_eq!(after.tasks.len(), 2);
        assert_eq!(after.tasks[0], before.tasks[0]);
        assert_eq!(after.tasks[1].name, before.tasks[1].name);
        assert_eq!(after.tasks[1].duration_min, before.tasks[1].duration_min);
        if col == COL_PREDECESSORS {
            assert!(after.tasks[1].predecessors.is_empty());
            assert_eq!(after.assignments.len(), before.assignments.len());
            for (actual, original) in after.assignments.iter().zip(&before.assignments) {
                assert_eq!(
                    (
                        actual.uid,
                        actual.task_uid,
                        actual.resource_uid,
                        actual.units
                    ),
                    (
                        original.uid,
                        original.task_uid,
                        original.resource_uid,
                        original.units
                    )
                );
            }
        } else {
            assert_eq!(after.tasks[1].predecessors, before.tasks[1].predecessors);
            assert!(after.assignments.is_empty());
        }
        assert_eq!(v(&t).ed.undo_depth(), depth + 1);
        apply_project_act(&mut t, ProjectAct::Undo);
        assert_eq!(v(&t).ed.project(), &before);
    }
}

#[test]
fn delete_on_resources_clears_an_assignment_with_no_displayed_name() {
    let mut t = tab();
    vm(&mut t).ed.assign_resource(2, "Alice").unwrap();
    let mut project = v(&t).ed.project().clone();
    project.resources.clear();
    vm(&mut t).ed = ProjectEditor::new(project);
    vm(&mut t).ed.select(1);
    vm(&mut t).col = COL_RESOURCES;
    let before = v(&t).ed.project().clone();
    assert!(project_row(&v(&t).ed, &before.tasks[1])[COL_RESOURCES].is_empty());
    assert_eq!(before.assignments.len(), 1);
    press(&mut t, "delete");
    assert!(v(&t).ed.project().assignments.is_empty());
    assert_eq!(v(&t).ed.undo_depth(), 1);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project(), &before);
}

#[test]
fn delete_on_id_deletes_the_task_and_a_summary_asks_first() {
    let mut t = tab();
    vm(&mut t).col = COL_ID;
    press(&mut t, "delete");
    assert_eq!(v(&t).ed.project().tasks.len(), 1);
    assert_eq!(v(&t).ed.project().tasks[0].name, "First");

    let mut t = summary_tab();
    vm(&mut t).col = COL_ID;
    press(&mut t, "delete");
    assert_eq!(t.dialogs.top_id(), "delete-summary");
    assert!(v(&t).prompt.is_none());
    assert_eq!(v(&t).ed.project().tasks.len(), 2);
}

#[test]
fn delete_on_unclearable_columns_changes_nothing() {
    for col in [COL_MODE, COL_DURATION, COL_START, COL_FINISH] {
        let mut t = tab();
        vm(&mut t).col = col;
        let before = v(&t).ed.project().clone();
        let depth = v(&t).ed.undo_depth();
        press(&mut t, "delete");
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.undo_depth(), depth);
        assert!(!t.dirty);
        assert_eq!(
            t.status.as_ref(),
            format!("{} can't be cleared", COLUMNS[col])
        );
    }
}

#[test]
fn delete_on_an_empty_field_blank_row_or_entry_row_is_a_no_op() {
    let mut t = tab();
    vm(&mut t).ed.rename(2, "").unwrap();
    vm(&mut t).ed.mark_saved();
    for col in [COL_NAME, COL_PREDECESSORS, COL_RESOURCES] {
        vm(&mut t).col = col;
        let before = v(&t).ed.project().clone();
        let depth = v(&t).ed.undo_depth();
        press(&mut t, "delete");
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.undo_depth(), depth);
        assert!(!t.dirty);
    }

    vm(&mut t).ed.insert_blank_row(Some(2)).unwrap();
    vm(&mut t).ed.mark_saved();
    vm(&mut t).ed.select(1);
    for col in [COL_NAME, COL_PREDECESSORS, COL_RESOURCES] {
        vm(&mut t).col = col;
        let before = v(&t).ed.project().clone();
        let depth = v(&t).ed.undo_depth();
        press(&mut t, "delete");
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.undo_depth(), depth);
        assert!(!t.dirty);
    }
    vm(&mut t).col = COL_ID;
    press(&mut t, "delete");
    assert_eq!(v(&t).ed.project().tasks.len(), 2);

    project_entry_click(&mut t, Some(COL_NAME), false);
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    press(&mut t, "delete");
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
}

#[test]
fn delete_inside_an_open_cell_edit_edits_the_buffer() {
    let mut t = tab();
    vm(&mut t).col = COL_NAME;
    let before = v(&t).ed.project().clone();
    vm(&mut t).open_cell(None).unwrap();
    vm(&mut t).cell.as_mut().unwrap().caret = 0;
    press(&mut t, "delete");
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "econd");
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 0);
}

#[test]
fn ribbon_insert_shortcut_is_on_blank_row_not_task() {
    let ribbon = project_ribbon();
    let shortcut = |id: &str| {
        ribbon
            .tabs
            .iter()
            .flat_map(|t| &t.groups)
            .flat_map(|g| &g.items)
            .flat_map(|item| match item {
                Control::Large(c) | Control::Toggle(c) => vec![c],
                Control::Column(commands) => commands.iter().collect(),
                _ => vec![],
            })
            .find(|c| c.id == id)
            .unwrap()
            .tip
            .shortcut
    };
    assert_eq!(shortcut("pr-blank-row"), "Insert");
    assert_eq!(shortcut("pr-add"), "Alt, T, N");
}

fn ctrl_shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..ctrl()
    }
}

/// A chord through the real key route, run the way the host runs its act.
fn chord(t: &mut DocTab, key: &str, m: Modifiers) {
    if let Some(act) = project_input(t, key, None, m) {
        apply_project_act(t, act);
    }
}

fn predecessors(t: &DocTab, uid: i32) -> usize {
    v(t).ed.project().task(uid).unwrap().predecessors.len()
}

#[test]
fn ctrl_f2_links_and_ctrl_shift_f2_unlinks() {
    use ProjectAct::*;
    assert_eq!(key_act("f2", ctrl()), Some(AddLink));
    assert_eq!(key_act("f2", ctrl_shift()), Some(UnlinkTasks));
    for m in [ctrl(), ctrl_shift()] {
        for gated in [
            Modifiers { alt: true, ..m },
            Modifiers {
                platform: true,
                ..m
            },
        ] {
            assert_eq!(key_act("f2", gated), None, "{gated:?}");
        }
    }
}

#[test]
fn ctrl_shift_f2_removes_the_selected_tasks_links_as_one_undo_step() {
    let mut t = tab();
    vm(&mut t)
        .ed
        .add_predecessor(2, 1, LinkType::FinishStart, 0)
        .unwrap();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let depth = v(&t).ed.undo_depth();
    // The cursor is on the second task, the link's successor.
    chord(&mut t, "f2", ctrl_shift());
    assert_eq!(t.status.as_ref(), "Removed 1 link");
    assert_eq!(predecessors(&t, 2), 0);
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    assert!(t.dirty);
    chord(&mut t, "z", ctrl());
    assert_eq!(predecessors(&t, 2), 1);
}

#[test]
fn ctrl_shift_f2_on_an_unlinked_task_changes_nothing() {
    let mut t = tab();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let depth = v(&t).ed.undo_depth();
    chord(&mut t, "f2", ctrl_shift());
    assert_eq!(t.status.as_ref(), "No links to remove");
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert!(!t.dirty && !v(&t).ed.dirty());
}

#[test]
fn ctrl_f2_asks_for_the_predecessor_and_links_in_one_undo_step() {
    let mut t = tab();
    let depth = v(&t).ed.undo_depth();
    chord(&mut t, "f2", ctrl());
    assert_eq!(
        v(&t).prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::Predecessor)
    );
    project_input(&mut t, "1", Some("1"), Modifiers::default());
    press(&mut t, "enter");
    assert!(v(&t).prompt.is_none());
    let preds = &v(&t).ed.project().task(2).unwrap().predecessors;
    assert_eq!(preds.len(), 1);
    assert_eq!((preds[0].uid, preds[0].link), (1, LinkType::FinishStart));
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
}

#[test]
fn link_chords_leave_an_open_cell_editor_and_prompt_alone() {
    let mut t = tab();
    vm(&mut t)
        .ed
        .add_predecessor(2, 1, LinkType::FinishStart, 0)
        .unwrap();
    let depth = v(&t).ed.undo_depth();
    press(&mut t, "f2");
    for m in [ctrl(), ctrl_shift()] {
        chord(&mut t, "f2", m);
        assert!(v(&t).cell.is_some(), "{m:?}");
        assert!(v(&t).prompt.is_none(), "{m:?}");
    }
    press(&mut t, "escape");
    apply_project_act(&mut t, ProjectAct::Find);
    for m in [ctrl(), ctrl_shift()] {
        chord(&mut t, "f2", m);
        assert_eq!(
            v(&t).prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::Find),
            "{m:?}"
        );
    }
    assert_eq!(predecessors(&t, 2), 1);
    assert_eq!(v(&t).ed.undo_depth(), depth);
}

#[test]
fn ribbon_link_hints_name_the_chords_the_keys_run() {
    use ProjectAct::*;
    let ribbon = project_ribbon();
    let command = |id: &str| {
        ribbon
            .tabs
            .iter()
            .flat_map(|t| &t.groups)
            .flat_map(|g| &g.items)
            .flat_map(|item| match item {
                Control::Large(c) | Control::Toggle(c) => vec![c],
                Control::Column(commands) => commands.iter().collect(),
                _ => vec![],
            })
            .find(|c| c.id == id)
            .unwrap()
    };
    for (id, act, hint, m) in [
        ("pr-link", AddLink, "Alt, T, P  (Ctrl+F2)", ctrl()),
        (
            "pr-unlink",
            UnlinkTasks,
            "Alt, T, U  (Ctrl+Shift+F2)",
            ctrl_shift(),
        ),
    ] {
        let c = command(id);
        assert!(matches!(c.act, Act::Project(a) if a == act), "{id}");
        assert_eq!(c.tip.shortcut, hint);
        assert_eq!(key_act("f2", m), Some(act), "{id}");
    }
}

#[test]
fn f11_is_new_project_only_without_modifiers() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    let platform = Modifiers {
        platform: true,
        ..Modifiers::default()
    };
    // Shift+F11 is Project's New Window, which docxy does not have.
    for m in [shift, ctrl(), alt, platform, ctrl_shift(), ctrl_alt()] {
        assert_eq!(key_act("f11", m), None, "{m:?}");
        let mut t = tab();
        assert_eq!(project_input(&mut t, "f11", None, m), None, "{m:?}");
    }
    let mut t = tab();
    assert_eq!(
        project_input(&mut t, "f11", None, Modifiers::default()),
        Some(ProjectAct::NewProject)
    );
    // The new tab is the host's; the plan itself is untouched.
    let before = v(&t).ed.project().clone();
    apply_project_act(&mut t, ProjectAct::NewProject);
    assert_eq!(v(&t).ed.project(), &before);
    assert!(!t.dirty);
}

#[test]
fn an_open_prompt_swallows_f11() {
    let mut t = tab();
    apply_project_act(&mut t, ProjectAct::Find);
    assert_eq!(
        project_input(&mut t, "f11", None, Modifiers::default()),
        None
    );
    assert!(v(&t).prompt.is_some());
}

#[test]
fn ctrl_k_opens_the_hyperlink_prompt_prefilled() {
    use ProjectAct::*;
    assert_eq!(key_act("k", ctrl()), Some(Hyperlink));
    for gated in [
        Modifiers {
            alt: true,
            ..ctrl()
        },
        Modifiers {
            platform: true,
            ..ctrl()
        },
        Modifiers::default(),
    ] {
        assert_eq!(key_act("k", gated), None, "{gated:?}");
    }
    let mut t = tab();
    vm(&mut t)
        .ed
        .set_hyperlink(
            2,
            "Pour instructions",
            "https://example.com/a?x=1&y=2",
            "Gantt Chart!4",
        )
        .unwrap();
    chord(&mut t, "k", ctrl());
    let p = v(&t).prompt.as_ref().unwrap();
    assert_eq!(p.kind, PromptKind::Hyperlink);
    assert_eq!(p.uid, Some(2));
    assert_eq!(
        p.buf,
        "https://example.com/a?x=1&y=2#Gantt Chart!4 | Pour instructions"
    );
}

/// The three hyperlink parts a commit left on the selected task (uid 2).
fn hyperlink_parts(t: &DocTab) -> (Option<String>, Option<String>, Option<String>) {
    let task = v(t).ed.project().task(2).unwrap();
    (
        task.hyperlink.clone(),
        task.hyperlink_address.clone(),
        task.hyperlink_sub_address.clone(),
    )
}

#[test]
fn hyperlink_prompt_commit_sets_the_task_and_undo_restores() {
    let mut t = tab();
    // An address alone: the display text defaults to it, as Project's
    // Text to display does.
    commit(&mut t, ProjectAct::Hyperlink, "https://example.com");
    assert_eq!(
        hyperlink_parts(&t),
        (
            Some("https://example.com".into()),
            Some("https://example.com".into()),
            None
        )
    );
    assert_eq!(t.status.as_ref(), "Hyperlink set: https://example.com");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    // All three parts in the buffer's grammar, still one undo step.
    commit(
        &mut t,
        ProjectAct::Hyperlink,
        "https://x#Gantt Chart!4 | Runbook",
    );
    assert_eq!(
        hyperlink_parts(&t),
        (
            Some("Runbook".into()),
            Some("https://x".into()),
            Some("Gantt Chart!4".into())
        )
    );
    assert_eq!(t.status.as_ref(), "Hyperlink set: Runbook");
    assert_eq!(v(&t).ed.undo_depth(), 2);
    // Undo walks back through both steps, redo replays them.
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(
        hyperlink_parts(&t),
        (
            Some("https://example.com".into()),
            Some("https://example.com".into()),
            None
        )
    );
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(hyperlink_parts(&t), (None, None, None));
    apply_project_act(&mut t, ProjectAct::Redo);
    assert_eq!(
        hyperlink_parts(&t),
        (
            Some("https://example.com".into()),
            Some("https://example.com".into()),
            None
        )
    );
}

#[test]
fn hyperlink_prompt_empty_buffer_removes_and_reports() {
    let mut t = tab();
    // Nothing to remove: no status, no history.
    let status = t.status.clone();
    commit(&mut t, ProjectAct::Hyperlink, "");
    assert_eq!(hyperlink_parts(&t), (None, None, None));
    assert_eq!(t.status, status);
    assert_eq!(v(&t).ed.undo_depth(), 0);
    // A removed link says so, as one undo step holding the previous link.
    vm(&mut t)
        .ed
        .set_hyperlink(2, "docs", "https://x", "Gantt Chart!4")
        .unwrap();
    commit(&mut t, ProjectAct::Hyperlink, "");
    assert_eq!(hyperlink_parts(&t), (None, None, None));
    assert_eq!(t.status.as_ref(), "Hyperlink removed");
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(
        hyperlink_parts(&t),
        (
            Some("docs".into()),
            Some("https://x".into()),
            Some("Gantt Chart!4".into())
        )
    );
}

#[test]
fn hyperlink_prompt_parse_cases() {
    let opt = |s: Option<&str>| s.map(str::to_string);
    for (buf, address, location, text) in [
        (
            "https://example.com",
            Some("https://example.com"),
            None,
            None,
        ),
        (
            "https://x#Gantt Chart!4",
            Some("https://x"),
            Some("Gantt Chart!4"),
            None,
        ),
        // An address-less `#Gantt Chart!4` is a location-only link.
        ("#Gantt Chart!4", None, Some("Gantt Chart!4"), None),
        (
            "https://x | Runbook",
            Some("https://x"),
            None,
            Some("Runbook"),
        ),
        (
            "https://x#loc | Runbook",
            Some("https://x"),
            Some("loc"),
            Some("Runbook"),
        ),
        (" | text only", None, None, Some("text only")),
        // The first ` | ` splits; a later one stays in the text.
        (
            "https://x | one | two",
            Some("https://x"),
            None,
            Some("one | two"),
        ),
        // Ends are trimmed, inner whitespace kept.
        (
            "  https://x  |  Run book  ",
            Some("https://x"),
            None,
            Some("Run book"),
        ),
        ("", None, None, None),
    ] {
        assert_eq!(
            parse_hyperlink(buf),
            (opt(address), opt(location), opt(text)),
            "{buf}"
        );
    }
}

#[test]
fn hyperlink_prompt_does_not_open_on_the_entry_row() {
    let mut t = tab();
    vm(&mut t).enter_entry_row();
    let status = t.status.clone();
    chord(&mut t, "k", ctrl());
    assert!(v(&t).prompt.is_none());
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert_eq!(t.status, status);
    assert!(!t.dirty);
}

/// The Duration cell as the table shows it.
fn duration_text(t: &DocTab, i: usize) -> String {
    let ed = &v(t).ed;
    project_row(ed, &ed.project().tasks[i])[COL_DURATION].clone()
}

#[test]
fn ctrl_delete_on_duration_resets_it_to_a_new_tasks_day_as_one_undo_step() {
    let mut t = tab();
    vm(&mut t).ed.mark_saved();
    vm(&mut t).col = COL_DURATION;
    let before = v(&t).ed.project().clone();
    assert_eq!(duration_text(&t, 1), "2d");
    chord(&mut t, "delete", ctrl());
    let after = v(&t).ed.project().clone();
    assert_eq!(after.tasks.len(), 2);
    assert_eq!(after.tasks[1].duration_min, 480);
    assert_eq!(duration_text(&t, 1), "1d?");
    assert_eq!(after.tasks[1].name, before.tasks[1].name);
    assert_eq!(after.tasks[0], before.tasks[0]);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert!(t.dirty);
    // Already the default: no second undo step.
    chord(&mut t, "delete", ctrl());
    assert_eq!(v(&t).ed.project(), &after);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    chord(&mut t, "z", ctrl());
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(duration_text(&t, 1), "2d");
}

#[test]
fn ctrl_delete_on_duration_follows_the_plans_day_and_estimate() {
    let mut t = tab();
    let mut p = v(&t).ed.project().clone();
    p.hours_per_day = 7.0;
    p.new_tasks_estimated = Some(false);
    t.surface = Surface::Project(ProjectView::new(p, false));
    vm(&mut t).ed.set_duration(2, "2d?").unwrap();
    vm(&mut t).ed.select(1);
    vm(&mut t).col = COL_DURATION;
    assert_eq!(duration_text(&t, 1), "2d?");
    chord(&mut t, "delete", ctrl());
    assert_eq!(v(&t).ed.project().tasks[1].duration_min, 420);
    assert_eq!(duration_text(&t, 1), "1d");
}

#[test]
fn ctrl_delete_on_a_milestones_duration_makes_it_a_one_day_task() {
    let mut t = tab();
    vm(&mut t).ed.toggle_milestone(2).unwrap();
    assert!(v(&t).ed.project().tasks[1].milestone);
    vm(&mut t).col = COL_DURATION;
    chord(&mut t, "delete", ctrl());
    let task = &v(&t).ed.project().tasks[1];
    assert_eq!(task.duration_min, 480);
    assert!(!task.milestone);
    assert_eq!(duration_text(&t, 1), "1d?");
}

#[test]
fn ctrl_delete_on_a_summarys_duration_refuses_unless_it_is_manual() {
    let mut t = summary_tab();
    vm(&mut t).col = COL_DURATION;
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    chord(&mut t, "delete", ctrl());
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert_eq!(
        t.status.as_ref(),
        "Summary dates and duration are read-only"
    );

    vm(&mut t).ed.set_manual(1, true).unwrap();
    vm(&mut t).ed.set_duration_min(1, 1440, false).unwrap();
    let depth = v(&t).ed.undo_depth();
    chord(&mut t, "delete", ctrl());
    assert_eq!(v(&t).ed.disp_duration_min(1), Some(480));
    // A summary takes no estimate.
    assert_eq!(duration_text(&t, 0), "1d");
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
}

#[test]
fn ctrl_delete_on_name_predecessors_and_resources_clears_as_delete_does() {
    for col in [COL_NAME, COL_PREDECESSORS, COL_RESOURCES] {
        let setup = || {
            let mut t = tab();
            vm(&mut t)
                .ed
                .add_predecessor(2, 1, LinkType::FinishStart, 0)
                .unwrap();
            vm(&mut t).ed.assign_resource(2, "Alice").unwrap();
            vm(&mut t).ed.mark_saved();
            vm(&mut t).col = col;
            t
        };
        let mut cleared = setup();
        press(&mut cleared, "delete");
        let mut t = setup();
        let before = v(&t).ed.project().clone();
        let depth = v(&t).ed.undo_depth();
        chord(&mut t, "delete", ctrl());
        assert_eq!(v(&t).ed.project(), v(&cleared).ed.project(), "{col}");
        assert_ne!(v(&t).ed.project(), &before, "{col}");
        assert_eq!(v(&t).ed.undo_depth(), depth + 1, "{col}");
        chord(&mut t, "z", ctrl());
        assert_eq!(v(&t).ed.project(), &before, "{col}");
    }
}

#[test]
fn ctrl_delete_on_task_mode_resets_it_to_the_mode_for_new_tasks() {
    for new_manual in [false, true] {
        let mut t = tab();
        vm(&mut t).ed.set_new_tasks_manual(new_manual);
        vm(&mut t).ed.set_manual(2, !new_manual).unwrap();
        vm(&mut t).col = COL_MODE;
        let before = v(&t).ed.project().clone();
        let depth = v(&t).ed.undo_depth();
        chord(&mut t, "delete", ctrl());
        let after = v(&t).ed.project().clone();
        assert_eq!(after.tasks[1].manual, new_manual);
        assert_eq!(after.tasks[0], before.tasks[0]);
        assert_eq!(v(&t).ed.undo_depth(), depth + 1);
        // Already the default: no second undo step.
        chord(&mut t, "delete", ctrl());
        assert_eq!(v(&t).ed.project(), &after);
        assert_eq!(v(&t).ed.undo_depth(), depth + 1);
        chord(&mut t, "z", ctrl());
        assert_eq!(v(&t).ed.project(), &before);
    }
}

#[test]
fn ctrl_delete_on_id_start_and_finish_changes_nothing_and_keeps_the_task() {
    for col in [COL_ID, COL_START, COL_FINISH] {
        let mut t = tab();
        vm(&mut t).ed.mark_saved();
        vm(&mut t).col = col;
        let before = v(&t).ed.project().clone();
        chord(&mut t, "delete", ctrl());
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.undo_depth(), 0);
        assert!(v(&t).prompt.is_none());
        assert!(!t.dirty);
        assert_eq!(
            t.status.as_ref(),
            format!("{} can't be cleared", COLUMNS[col])
        );
    }
    let mut t = summary_tab();
    vm(&mut t).col = COL_ID;
    chord(&mut t, "delete", ctrl());
    assert!(
        v(&t).prompt.is_none(),
        "a summary is not offered for deletion"
    );
    assert_eq!(v(&t).ed.project().tasks.len(), 2);
}

#[test]
fn ctrl_delete_on_a_blank_row_or_the_entry_row_is_a_no_op() {
    let mut t = tab();
    vm(&mut t).ed.insert_blank_row(Some(2)).unwrap();
    vm(&mut t).ed.mark_saved();
    vm(&mut t).ed.select(1);
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    for col in 0..COLUMN_COUNT {
        vm(&mut t).col = col;
        chord(&mut t, "delete", ctrl());
        assert_eq!(v(&t).ed.project(), &before, "{col}");
        assert!(v(&t).ed.project().tasks[1].is_null, "{col}");
        assert_eq!(v(&t).ed.undo_depth(), depth, "{col}");
        assert!(!t.dirty, "{col}");
    }
    for col in [COL_NAME, COL_DURATION, COL_MODE, COL_ID] {
        project_entry_click(&mut t, Some(col), false);
        chord(&mut t, "delete", ctrl());
        assert_eq!(v(&t).ed.project(), &before, "{col}");
        assert_eq!(v(&t).ed.undo_depth(), depth, "{col}");
    }
}

#[test]
fn ctrl_delete_inside_an_open_cell_edit_stays_in_the_editor() {
    let mut t = tab();
    vm(&mut t).col = COL_DURATION;
    let before = v(&t).ed.project().clone();
    vm(&mut t).open_cell(None).unwrap();
    chord(&mut t, "delete", ctrl());
    assert!(v(&t).cell.is_some());
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 0);
}

#[test]
fn app_state_is_busy_over_edit_over_ready() {
    let mut t = tab();
    assert_eq!(project_app_state(v(&t)), AppState::Ready);
    press(&mut t, "f2");
    assert_eq!(project_app_state(v(&t)), AppState::Edit, "cell editor");
    press(&mut t, "escape");
    assert_eq!(project_app_state(v(&t)), AppState::Ready);
    apply_project_act(&mut t, ProjectAct::Find);
    assert_eq!(project_app_state(v(&t)), AppState::Edit, "prompt");
    press(&mut t, "escape");
    assert_eq!(project_app_state(v(&t)), AppState::Ready);
    // Not reachable through the UI (asking for a pass commits the cell, and
    // input runs a pending pass before it opens one), but defined.
    press(&mut t, "f2");
    vm(&mut t).busy = Some((0, true));
    assert_eq!(project_app_state(v(&t)), AppState::Busy);
    assert_eq!(
        [AppState::Ready, AppState::Edit, AppState::Busy].map(AppState::label),
        ["Ready", "Edit", "Busy"]
    );
}

#[test]
fn a_level_pass_is_asked_for_then_run_by_a_flush() {
    let mut t = tab();
    t.status = "before".into();
    request_level_pass(&mut t, ProjectAct::LevelAll).unwrap();
    // Asked for, not run: the plan and the message are as they were.
    assert_eq!(project_app_state(v(&t)), AppState::Busy);
    assert!(!v(&t).ed.leveled());
    assert_eq!(t.status.as_ref(), "before");
    assert!(flush_level_pass(&mut t));
    assert_eq!(project_app_state(v(&t)), AppState::Ready);
    assert!(v(&t).ed.leveled());
    assert_eq!(
        t.status.as_ref(),
        "Resource leveling ON — bars delayed to fit resource capacity"
    );
    assert!(!flush_level_pass(&mut t), "nothing left to run");
    request_level_pass(&mut t, ProjectAct::ClearLeveling).unwrap();
    assert!(flush_level_passes(std::slice::from_mut(&mut t)));
    assert!(!v(&t).ed.leveled());
    assert_eq!(t.status.as_ref(), "Resource leveling OFF");
}

#[test]
fn level_twice_is_a_round_trip() {
    let mut t = tab();
    for leveled in [true, false] {
        request_level_pass(&mut t, ProjectAct::Level).unwrap();
        flush_level_pass(&mut t);
        assert_eq!(v(&t).ed.leveled(), leveled);
    }
    // A second request runs the first before it resolves its own target.
    request_level_pass(&mut t, ProjectAct::LevelAll).unwrap();
    request_level_pass(&mut t, ProjectAct::Level).unwrap();
    assert!(v(&t).ed.leveled(), "the Level All ran first");
    flush_level_pass(&mut t);
    assert!(!v(&t).ed.leveled(), "then Level turned it off");
}

#[test]
fn keys_clicks_and_prompts_run_a_pending_pass_before_they_reach_the_plan() {
    let busy = || {
        let mut t = tab();
        request_level_pass(&mut t, ProjectAct::LevelAll).unwrap();
        t
    };
    let settled = |t: &DocTab, how: &str| {
        assert!(v(t).busy.is_none(), "{how}");
        assert!(v(t).ed.leveled(), "{how}");
    };
    // A key that opens the cell editor: the pass runs first, so the editor
    // is open over a settled plan (Edit, never Busy with an editor).
    let mut t = busy();
    assert_eq!(
        project_input(&mut t, "f2", None, Modifiers::default()),
        None
    );
    settled(&t, "F2");
    assert_eq!(project_app_state(v(&t)), AppState::Edit);
    // A typed edit lands after the pass, so Undo takes back the edit and
    // leaves the levelling the user asked for first.
    let mut t = busy();
    project_input(&mut t, "x", Some("x"), Modifiers::default());
    settled(&t, "typing");
    project_input(&mut t, "enter", None, Modifiers::default());
    apply_project_act(&mut t, ProjectAct::Undo);
    assert!(
        v(&t).ed.leveled(),
        "Undo reverts the edit, not the levelling"
    );
    let mut t = busy();
    project_cell_click(&mut t, 0, Some(COL_NAME), true);
    settled(&t, "double-click");
    let mut t = busy();
    project_entry_click(&mut t, Some(COL_NAME), false);
    settled(&t, "entry row click");
    let mut t = busy();
    project_below_click(&mut t);
    settled(&t, "click below the rows");
    let mut t = busy();
    toggle_project_collapse(&mut t, 1);
    settled(&t, "outline toggle");
    let mut t = busy();
    vm(&mut t).open_prompt(PromptKind::Find);
    let p = vm(&mut t).prompt.take().unwrap();
    commit_prompt(&mut t, p);
    settled(&t, "prompt commit");
}

#[test]
fn a_frame_callback_runs_only_its_own_pass() {
    let mut t = tab();
    let stale = request_level_pass(&mut t, ProjectAct::LevelAll).unwrap();
    vm(&mut t).scheduled = Some(stale);
    // A harness verb flushed it, and a new pass was asked for before the
    // stale callback fired: that callback must not run the new pass early.
    flush_level_pass(&mut t);
    assert_eq!(v(&t).scheduled, None);
    let fresh = request_level_pass(&mut t, ProjectAct::ClearLeveling).unwrap();
    assert_ne!(stale, fresh);
    assert!(!finish_level_pass(&mut t, stale));
    assert_eq!(project_app_state(v(&t)), AppState::Busy);
    assert!(v(&t).ed.leveled());
    assert!(finish_level_pass(&mut t, fresh));
    assert!(!v(&t).ed.leveled());
    assert_eq!(project_app_state(v(&t)), AppState::Ready);
}

#[test]
fn asking_for_a_pass_commits_the_cell_and_closes_the_prompt() {
    let mut t = tab();
    press(&mut t, "f2");
    request_level_pass(&mut t, ProjectAct::LevelAll).unwrap();
    assert!(v(&t).cell.is_none());
    apply_project_act(&mut t, ProjectAct::Find);
    request_level_pass(&mut t, ProjectAct::Level).unwrap();
    assert!(v(&t).prompt.is_none());
}

#[test]
fn status_items_are_the_state_the_mode_and_the_message_in_order() {
    let mut t = tab();
    t.status = "loaded — 2 tasks".into();
    let items = |t: &DocTab| status_items(t);
    assert_eq!(
        items(&t),
        [
            ("state", "Ready".to_string()),
            ("new-tasks", "New Tasks: Auto Scheduled".to_string()),
            ("message", "loaded — 2 tasks".to_string()),
        ]
    );
    request_level_pass(&mut t, ProjectAct::LevelAll).unwrap();
    assert_eq!(items(&t)[0], ("state", "Busy".to_string()));
    // Any other surface has only the message.
    t.surface = Surface::Placeholder;
    t.status = "saved".into();
    assert_eq!(items(&t), [("message", "saved".to_string())]);
}

/// The row menu's items as (label, enabled, checked); separators as `-`.
fn row_menu(t: &DocTab) -> Vec<(String, bool, bool)> {
    use crate::menu::MenuItem;
    project_row_menu(v(t))
        .into_iter()
        .map(|item| match item {
            MenuItem::Item(e) => (e.label, e.enabled, e.checked),
            MenuItem::Separator => ("-".into(), false, false),
            MenuItem::Heading(h) => (format!("[{h}]"), false, false),
            MenuItem::TableGrid { .. } => ("[grid]".into(), false, false),
        })
        .collect()
}

fn enabled_items(t: &DocTab) -> Vec<String> {
    row_menu(t)
        .into_iter()
        .filter(|(_, enabled, _)| *enabled)
        .map(|(label, ..)| label)
        .collect()
}

/// #397: a task row's context menu is Project's, in Project's order.
#[test]
fn the_row_menu_lists_projects_items_in_order() {
    let t = tab();
    let labels: Vec<_> = row_menu(&t).into_iter().map(|(l, ..)| l).collect();
    assert_eq!(
        labels,
        [
            "Cut",
            "Copy",
            "Paste",
            "Paste Special...",
            "-",
            "Scroll to Task",
            "-",
            "Insert Task",
            "Delete Task",
            "Inactivate Task",
            "-",
            "Manually Schedule",
            "Auto Schedule",
            "-",
            "Assign Resources...",
            "-",
            "Text Styles...",
            "Font...",
            "-",
            "Fill Down",
            "Clear Contents",
            "-",
            "Information...",
            "Notes...",
            "Add to Timeline",
            "-",
            "Hyperlink...",
        ]
    );
    // Every drawn icon is one the window can load.
    for item in project_row_menu(v(&t)) {
        if let crate::menu::MenuItem::Item(e) = item {
            assert!(
                e.icon.is_empty()
                    || Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("assets/icons")
                        .join(format!("{}.svg", e.icon))
                        .exists(),
                "{}",
                e.icon
            );
            // An item with no command is never enabled.
            assert!(e.act.is_some() || !e.enabled, "{}", e.label);
        }
    }
}

/// #397: which items a row enables — a real task, a blank row, the entry row.
#[test]
fn the_row_menu_enables_by_the_row_under_the_cursor() {
    let mut t = tab();
    let all_task = [
        "Cut",
        "Copy",
        "Paste",
        "Scroll to Task",
        "Insert Task",
        "Delete Task",
        "Inactivate Task",
        "Manually Schedule",
        "Auto Schedule",
        "Assign Resources...",
        "Information...",
        "Hyperlink...",
    ];
    assert_eq!(enabled_items(&t), all_task);
    // A blank row can be inserted above and deleted, not scheduled.
    let blank = v(&t).ed.project().tasks.len();
    vm(&mut t).ed.insert_blank_row(None).unwrap();
    vm(&mut t).ed.select(blank);
    assert!(v(&t).selected_uid().is_some());
    assert_eq!(
        enabled_items(&t),
        ["Cut", "Copy", "Paste", "Insert Task", "Delete Task"]
    );
    // The entry row has no task to delete.
    vm(&mut t).enter_entry_row();
    assert_eq!(v(&t).selected_uid(), None);
    assert_eq!(enabled_items(&t), ["Cut", "Copy", "Paste", "Insert Task"]);
    // No tick without a task.
    assert!(row_menu(&t).iter().all(|(_, _, checked)| !checked));
}

/// #418: the row menu's Hyperlink... opens the prompt on a task row only,
/// like Information....
#[test]
fn row_menu_hyperlink_is_enabled_on_a_task_row() {
    let mut t = tab();
    let enabled = |t: &DocTab| enabled_items(t).contains(&"Hyperlink...".to_string());
    assert!(enabled(&t), "a task row links");
    // A blank row in the middle of the plan has no task to link.
    let i = vm(&mut t).ed.insert_blank_row(Some(2)).unwrap();
    vm(&mut t).ed.select(i);
    assert!(!enabled(&t), "a blank row does not");
    vm(&mut t).enter_entry_row();
    assert!(!enabled(&t), "the entry row does not");
}

/// #397: the ticks are the ribbon's (`project_act_active`).
#[test]
fn the_row_menu_ticks_the_task_mode_and_inactive_as_the_ribbon_does() {
    let mut t = tab();
    let ticked = |t: &DocTab| {
        row_menu(t)
            .into_iter()
            .filter(|(_, _, checked)| *checked)
            .map(|(label, ..)| label)
            .collect::<Vec<_>>()
    };
    assert_eq!(ticked(&t), ["Auto Schedule"]);
    apply_project_act(&mut t, ProjectAct::ManuallySchedule);
    apply_project_act(&mut t, ProjectAct::Inactivate);
    assert_eq!(ticked(&t), ["Inactivate Task", "Manually Schedule"]);
}

/// #397: the row menu's Delete Task deletes the selected task whatever
/// column the cursor is on; the Delete key does only on the ID column.
#[test]
fn delete_task_deletes_from_any_column_and_asks_for_a_summary() {
    let mut t = tab();
    assert_eq!(v(&t).col, COL_NAME);
    let uid = v(&t).selected_uid().unwrap();
    let count = v(&t).ed.project().tasks.len();
    apply_project_act(&mut t, ProjectAct::ClearCell);
    assert_eq!(
        v(&t).ed.project().tasks.len(),
        count,
        "Delete on Name clears"
    );
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    assert_eq!(v(&t).ed.project().tasks.len(), count - 1);
    assert!(v(&t).ed.project().task(uid).is_none());
    assert!(t.dialogs.top().is_none());
    // One undo step brings it back.
    apply_project_act(&mut t, ProjectAct::Undo);
    assert!(v(&t).ed.project().task(uid).is_some());

    // A summary asks first, as Delete on its ID does.
    let mut t = summary_tab();
    let before = v(&t).ed.project().clone();
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    assert_eq!(t.dialogs.top_id(), "delete-summary");
    assert_eq!(v(&t).ed.project(), &before);
    dialog_click(&mut t, "Yes").unwrap();
    assert!(v(&t).ed.project().tasks.is_empty());

    // On the entry row there is nothing to delete.
    let mut t = tab();
    vm(&mut t).enter_entry_row();
    let before = v(&t).ed.project().clone();
    apply_project_act(&mut t, ProjectAct::DeleteTask);
    assert_eq!(v(&t).ed.project(), &before);
}

/// #397: Set Baseline is a split button: the primary sets the baseline, its
/// menu offers Set Baseline... and Clear Baseline..., and the letters B and
/// L still reach both from the keyboard.
#[test]
fn set_baseline_is_a_split_with_clear_baseline_in_its_menu() {
    let r = project_ribbon();
    let project = r.tabs.iter().find(|t| t.name == "Project").unwrap();
    let schedule = &project.groups[0];
    let (primary, menu) = schedule
        .items
        .iter()
        .find_map(|c| match c {
            Control::Split { primary, menu } => Some((primary, menu)),
            _ => None,
        })
        .expect("Set Baseline is a split button");
    assert_eq!(primary.label, "Set Baseline");
    assert!(matches!(primary.act, Act::Project(ProjectAct::Baseline)));
    let items = crate::menu::split_menu(menu, |_| true, |_| false);
    let read: Vec<_> = items
        .iter()
        .map(|item| match item {
            crate::menu::MenuItem::Item(e) => (e.label.as_str(), e.key_tip.as_str(), e.enabled),
            _ => panic!("a split's menu is its commands"),
        })
        .collect();
    assert_eq!(
        read,
        [
            ("Set Baseline...", "B", true),
            ("Clear Baseline...", "L", true)
        ]
    );
    // Clear Baseline no longer sits beside it.
    assert_eq!(schedule.items.len(), 2, "Calculate Project and the split");
    assert!(matches!(
        tab_keytip_cmd(project, "B"),
        Some(Act::Project(ProjectAct::Baseline))
    ));
    assert!(matches!(
        tab_keytip_cmd(project, "L"),
        Some(Act::Project(ProjectAct::ClearBaseline))
    ));
}
