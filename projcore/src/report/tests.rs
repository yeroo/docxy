use super::*;
use crate::datetime::DateTime;
use crate::editor::untitled_project;
use crate::model::{Assignment, AvailabilityPeriod, Baseline, Rate};

fn task(uid: i32, name: &str, duration_min: i64) -> Task {
    Task {
        uid,
        id: uid,
        name: name.into(),
        outline_level: 1,
        duration_min,
        ..Task::default()
    }
}

fn money(text: &str) -> Option<Rate> {
    Rate::parse(text)
}

fn resource(uid: i32, name: &str, kind: ResourceType) -> Resource {
    Resource {
        uid,
        id: uid,
        name: name.into(),
        kind,
        max_units: 1.0,
        ..Resource::default()
    }
}

fn assign(uid: i32, task_uid: i32, resource_uid: i32, units: f64) -> Assignment {
    Assignment {
        uid,
        task_uid,
        resource_uid,
        units,
        ..Assignment::default()
    }
}

fn project(tasks: Vec<Task>) -> Project {
    let mut p = untitled_project();
    p.name = "Demo".into();
    p.start_date = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
    p.tasks = tasks;
    p
}

/// A plan with a project summary row, a phase of two tasks, a milestone and
/// a fixed-cost task. The stored costs are the editor's (a hand-entered cost
/// on a cost resource, a fixed cost), none of them what repricing the
/// assignments would give.
fn costed() -> Project {
    let mut root = task(0, "Demo", 0);
    root.outline_level = 0;
    // Not the sum of the rows below (Phase, Sign-off, Permit: $1,500.00 and
    // 80 hrs), so a total read from this row is told apart from a sum.
    root.cost = money("155000");
    root.work_min = Some(5400);
    root.percent_complete = Some(25);
    let mut phase = task(1, "Phase", 0);
    phase.cost = money("100000");
    phase.work_min = Some(4800);
    let mut a = task(2, "Design", 2400);
    a.outline_level = 2;
    a.cost = money("60000");
    a.work_min = Some(2400);
    a.percent_complete = Some(50);
    let mut b = task(3, "Build", 2400);
    b.outline_level = 2;
    b.cost = money("40000");
    b.work_min = Some(2400);
    b.predecessors = vec![crate::model::Predecessor::fs(2)];
    let mut ms = task(4, "Sign-off", 0);
    ms.milestone = true;
    ms.predecessors = vec![crate::model::Predecessor::fs(3)];
    let mut permit = task(5, "Permit", 480);
    permit.fixed_cost = money("50000");
    permit.cost = money("50000");
    let mut p = project(vec![root, phase, a, b, ms, permit]);
    let mut dev = resource(1, "Dev", ResourceType::Work);
    dev.standard_rate = money("10");
    dev.cost = money("100000");
    dev.work_min = Some(4800);
    let mut fee = resource(2, "Fee", ResourceType::Cost);
    fee.cost = money("12345");
    p.resources = vec![dev, fee];
    p.assignments = vec![assign(1, 2, 1, 1.0), assign(2, 3, 1, 1.0)];
    p
}

fn render_of(p: Project, kind: ReportKind) -> String {
    render(&Editor::new(p), kind)
}

/// The rows of the first table under `heading` (or the first table when
/// `heading` is empty), header and divider skipped, split into cells.
fn rows(md: &str, heading: &str) -> Vec<Vec<String>> {
    let from = if heading.is_empty() {
        0
    } else {
        md.find(heading).expect(heading)
    };
    md[from..]
        .lines()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .skip(2)
        .map(|l| {
            l.trim_matches('|')
                .split(" | ")
                .map(|c| c.trim().to_string())
                .collect()
        })
        .collect()
}

#[test]
fn every_report_has_a_slug_title_menu_and_file_name() {
    let mut slugs: Vec<_> = ReportKind::ALL.iter().map(|k| k.slug()).collect();
    slugs.dedup();
    assert_eq!(slugs.len(), 8);
    for k in ReportKind::ALL {
        assert_eq!(ReportKind::parse(k.slug()), Some(k));
        assert!(k.slug().chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }
    assert_eq!(ReportKind::parse("burndown"), None);
    assert_eq!(
        ReportKind::LateTasks.file_name("plan"),
        "plan-late-tasks.md"
    );
    assert_eq!(ReportKind::ProjectOverview.menu(), "Dashboards");
    assert_eq!(ReportKind::OverallocatedResources.menu(), "Resources");
    assert_eq!(ReportKind::ResourceCostOverview.menu(), "Costs");
    assert_eq!(ReportKind::MilestoneReport.menu(), "In Progress");
    assert!(ReportKind::slugs().starts_with("project-overview, resource-overview"));
}

#[test]
fn an_empty_plan_renders_every_report() {
    for k in ReportKind::ALL {
        let md = render_of(project(Vec::new()), k);
        assert!(
            md.starts_with(&format!("# Demo: {}\n\n", k.title())),
            "{md}"
        );
        let empty = if k.menu() == "Resources" || k == ReportKind::ResourceCostOverview {
            "No resources."
        } else {
            "No tasks."
        };
        assert!(md.contains(empty), "{k:?}: {md}");
        assert!(
            md.ends_with(".\n") && !md.ends_with("\n\n"),
            "{k:?}: {md:?}"
        );
    }
}

#[test]
fn task_costs_are_the_stored_ones_with_the_summary_rows_total() {
    let md = render_of(costed(), ReportKind::TaskCostOverview);
    let rows = rows(&md, "");
    let names: Vec<_> = rows.iter().map(|r| r[0].as_str()).collect();
    // The project summary row is the total, not a row of its own.
    assert_eq!(
        names,
        [
            "**Phase**",
            "Design",
            "Build",
            "Sign-off",
            "Permit",
            "**Total**"
        ]
    );
    assert_eq!(rows[1][4], "$600.00", "Design's stored Cost");
    assert_eq!(rows[4][1], "$500.00", "Permit's Fixed Cost");
    assert_eq!(rows[5][4], "$1,550.00", "the project summary row's Cost");
    // Fixed Cost does not roll up: its total is every row's own.
    assert_eq!(rows[5][1], "$500.00");
    let mut p = costed();
    p.tasks[2].fixed_cost = money("5000");
    let md = render_of(p, ReportKind::TaskCostOverview);
    assert_eq!(rows_of(&md)[5][1], "$550.00", "{md}");
}

fn rows_of(md: &str) -> Vec<Vec<String>> {
    rows(md, "")
}

#[test]
fn without_a_summary_row_totals_sum_only_the_top_level() {
    let mut p = costed();
    p.tasks.remove(0);
    for t in &mut p.tasks {
        t.outline_level = t.outline_level.max(1);
    }
    p.tasks[1].fixed_cost = money("5000");
    let md = render_of(p, ReportKind::TaskCostOverview);
    let total = rows(&md, "").pop().unwrap();
    // Phase ($1,000) + Sign-off + Permit ($500): its children are in Phase.
    assert_eq!(total[4], "$1,500.00");
    // Design's fixed cost, nested under Phase, is in the Fixed Cost total.
    assert_eq!(total[1], "$550.00");
    let md = render_of(costed_without_root(), ReportKind::ProjectOverview);
    let overview = &rows(&md, "")[0];
    assert_eq!(overview[3], "80 hrs");
    assert_eq!(overview[4], "$1,500.00");
}

fn costed_without_root() -> Project {
    let mut p = costed();
    p.tasks.remove(0);
    p
}

#[test]
fn inactive_tasks_are_left_out() {
    let mut p = costed_without_root();
    p.options
        .push(("StatusDate".into(), "2026-03-31T17:00:00".into()));
    p.tasks.iter_mut().find(|t| t.uid == 5).unwrap().active = Some(false);
    let costs = render_of(p.clone(), ReportKind::TaskCostOverview);
    assert!(!costs.contains("Permit"));
    assert_eq!(rows(&costs, "").pop().unwrap()[4], "$1,000.00");
    let late = render_of(p, ReportKind::LateTasks);
    assert!(
        late.contains("Design") && !late.contains("Permit"),
        "{late}"
    );
}

#[test]
fn late_tasks_need_a_status_date() {
    let md = render_of(costed(), ReportKind::LateTasks);
    assert!(
        md.contains("No status date: lateness not computed."),
        "{md}"
    );
    assert!(!md.contains('|'), "{md}");
    let overview = render_of(costed(), ReportKind::ProjectOverview);
    let late = &overview[overview.find("## Late Tasks").unwrap()..];
    assert!(late.contains("No status date: lateness not computed."));

    let mut p = costed();
    // By 3/9 Design (50%) and Permit (0%) should be done; Build starts that
    // day, which Project's Status does not count as late.
    p.options
        .push(("StatusDate".into(), "2026-03-09T17:00:00".into()));
    let md = render_of(p.clone(), ReportKind::LateTasks);
    assert!(md.contains("Status date: Mon 3/9/26"), "{md}");
    let late: Vec<_> = rows(&md, "").into_iter().map(|r| r[0].clone()).collect();
    assert_eq!(late, ["Design", "Permit"], "{md}");
    let overview = render_of(p, ReportKind::ProjectOverview);
    assert_eq!(rows(&overview, "## Late Tasks").len(), 2, "{overview}");
}

#[test]
fn the_project_overview_lists_top_level_progress_and_milestones_due() {
    let md = render_of(costed(), ReportKind::ProjectOverview);
    let head = &rows(&md, "")[0];
    assert_eq!(head[0], "Mon 3/2/26");
    assert_eq!(head[2], "25%", "the project summary row's % Complete");
    assert_eq!(head[3], "90 hrs", "the project summary row's Work");
    assert_eq!(head[4], "$1,550.00", "the project summary row's Cost");
    let top: Vec<_> = rows(&md, "## % Complete")
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(top, ["**Phase**", "Sign-off", "Permit"]);
    let due = rows(&md, "## Milestones Due");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0][0], "Sign-off");
}

#[test]
fn a_plan_without_a_summary_row_weights_percent_complete_by_duration() {
    let md = render_of(costed_without_root(), ReportKind::ProjectOverview);
    // Design is half of 5 of the 11 leaf days (5 + 5 + 0 + 1).
    assert_eq!(rows(&md, "")[0][2], "22%");
}

#[test]
fn names_are_escaped_so_tables_keep_their_cells() {
    let mut p = costed();
    p.tasks[2].name = "a|b *c*".into();
    p.resources[0].name = "Ann|Bo".into();
    let md = render_of(p.clone(), ReportKind::TaskCostOverview);
    let row = md.lines().find(|l| l.contains("a\\|b")).unwrap();
    assert!(row.starts_with("| a\\|b \\*c\\* |"), "{row}");
    assert_eq!(rows(&md, "")[1].len(), 7, "{md}");
    let md = render_of(p, ReportKind::ResourceOverview);
    assert!(md.contains("| Ann\\|Bo | Work |"), "{md}");
}

#[test]
fn critical_tasks_leave_out_complete_ones() {
    let md = render_of(costed(), ReportKind::CriticalTasks);
    let names: Vec<_> = rows(&md, "").into_iter().map(|r| r[0].clone()).collect();
    assert_eq!(names, ["Design", "Build", "Sign-off"], "{md}");
    let mut p = costed();
    p.tasks[2].percent_complete = Some(100);
    let md = render_of(p, ReportKind::CriticalTasks);
    assert!(!md.contains("Design"), "{md}");
}

#[test]
fn the_milestone_report_lists_milestones_with_their_status() {
    let mut p = costed();
    p.options
        .push(("StatusDate".into(), "2026-03-02T09:00:00".into()));
    let md = render_of(p, ReportKind::MilestoneReport);
    let rows = rows(&md, "");
    assert_eq!(rows.len(), 1, "{md}");
    assert_eq!(rows[0][0], "Sign-off");
    assert_eq!(rows[0][3], "Future Task");
}

#[test]
fn resource_reports_read_stored_totals() {
    let mut p = costed();
    let mut brick = resource(3, "Brick", ResourceType::Material);
    brick.material_label = Some("ton".into());
    brick.work_min = Some(150);
    p.resources.push(brick);
    let md = render_of(p.clone(), ReportKind::ResourceOverview);
    let rows_ = rows(&md, "");
    assert_eq!(rows_[0][..3], ["Dev", "Work", "80 hrs"]);
    assert_eq!(rows_[1][..3], ["Fee", "Cost", ""]);
    assert_eq!(rows_[2][..3], ["Brick", "Material", "2.5 ton"]);
    let md = render_of(p, ReportKind::ResourceCostOverview);
    let rows_ = rows(&md, "");
    // Fee's hand-entered cost, which no rate would give.
    assert_eq!(rows_[1][4], "$123.45");
    assert_eq!(rows_[3], ["**Total**", "", "$0.00", "$0.00", "$1,123.45"]);
}

#[test]
fn overallocation_is_measured_from_the_shown_schedule() {
    // Design and Build run one after the other: Dev is never booked twice.
    let md = render_of(costed(), ReportKind::OverallocatedResources);
    assert!(md.contains("No overallocated resources."), "{md}");

    // In parallel at 100% each, Dev is booked at 200%.
    let mut p = costed();
    p.tasks[3].predecessors.clear();
    let md = render_of(p.clone(), ReportKind::OverallocatedResources);
    assert_eq!(rows(&md, ""), [["Dev", "100%", "200%", "80 hrs"]], "{md}");

    // Twice as much capacity in an availability period that covers them.
    p.resources[0].availability_periods = vec![AvailabilityPeriod {
        available_from: Some(DateTime::from_ymd_hm(2026, 3, 1, 0, 0)),
        available_to: Some(DateTime::from_ymd_hm(2026, 4, 1, 0, 0)),
        available_units: Rate::parse("2"),
    }];
    let md = render_of(p.clone(), ReportKind::OverallocatedResources);
    assert!(md.contains("No overallocated resources."), "{md}");

    // An inactive task books nothing.
    p.resources[0].availability_periods.clear();
    p.tasks[3].active = Some(false);
    let md = render_of(p, ReportKind::OverallocatedResources);
    assert!(md.contains("No overallocated resources."), "{md}");
}

#[test]
fn reports_ignore_blank_rows_and_baselines_show_variance() {
    let mut p = costed();
    let mut blank = task(9, "Ghost", 480);
    blank.is_null = true;
    p.tasks.insert(3, blank);
    p.tasks[2].baselines = vec![Baseline {
        number: 0,
        cost: money("50000"),
        ..Baseline::default()
    }];
    for k in ReportKind::ALL {
        assert!(!render_of(p.clone(), k).contains("Ghost"), "{k:?}");
    }
    let md = render_of(p, ReportKind::TaskCostOverview);
    let design = &rows(&md, "")[1];
    assert_eq!(design[5], "$500.00");
    assert_eq!(design[6], "$100.00");
}

#[test]
fn tasks_under_an_inactive_summary_are_left_out() {
    // Phase is inactive; Design and Build keep their own Active flag. Build
    // runs beside Design, and Dev also works on Permit beside them.
    let mut p = costed();
    p.tasks[1].active = Some(false);
    p.tasks[3].predecessors.clear();
    p.assignments.push(assign(3, 5, 1, 1.0));
    let costs = render_of(p.clone(), ReportKind::TaskCostOverview);
    assert!(
        !costs.contains("Design") && !costs.contains("Build"),
        "{costs}"
    );
    let over = render_of(p.clone(), ReportKind::OverallocatedResources);
    assert!(over.contains("No overallocated resources."), "{over}");
    // Active again, all three book Dev at once.
    p.tasks[1].active = None;
    let over = render_of(p, ReportKind::OverallocatedResources);
    assert_eq!(rows(&over, "")[0][2], "300%", "{over}");
}

#[test]
fn a_delayed_assignment_books_from_its_delay() {
    // Build runs ten days beside Design, but Dev starts on it after five,
    // when Design is done: never booked twice.
    let mut p = costed();
    p.tasks[3].predecessors.clear();
    p.tasks[3].duration_min = 4800;
    p.assignments[1].work_min = 2400;
    p.assignments[1].delay = Some(2400 * 10);
    let md = render_of(p.clone(), ReportKind::OverallocatedResources);
    assert!(md.contains("No overallocated resources."), "{md}");
    p.assignments[1].delay = None;
    let md = render_of(p, ReportKind::OverallocatedResources);
    assert_eq!(rows(&md, "")[0][2], "200%", "{md}");
}

#[test]
fn availability_periods_meeting_overnight_leave_no_gap() {
    // Design (Mar 2-6) at 100% across periods that end at 17:00 and start
    // again at 08:00, as MSPDI writes them: Dev is never short.
    let mut p = costed();
    let period = |from: (u32, u32), to: Option<(u32, u32)>| AvailabilityPeriod {
        available_from: Some(DateTime::from_ymd_hm(2026, 3, from.0, from.1, 0)),
        available_to: to.map(|(d, h)| DateTime::from_ymd_hm(2026, 3, d, h, 0)),
        available_units: Rate::parse("1"),
    };
    p.resources[0].availability_periods = vec![period((2, 8), Some((3, 17))), period((4, 8), None)];
    let md = render_of(p.clone(), ReportKind::OverallocatedResources);
    assert!(md.contains("No overallocated resources."), "{md}");
    // A real gap (no period on Mar 4) leaves Design's work there without
    // capacity.
    p.resources[0].availability_periods = vec![period((2, 8), Some((3, 17))), period((5, 8), None)];
    let md = render_of(p, ReportKind::OverallocatedResources);
    assert_eq!(rows(&md, "")[0][0], "Dev", "{md}");
}

#[test]
fn the_overview_spans_only_active_tasks() {
    // An inactive twenty-day task starting with the plan does not stretch
    // its finish.
    let mut p = costed();
    let finish = rows(&render_of(p.clone(), ReportKind::ProjectOverview), "")[0][1].clone();
    let mut long = task(6, "Someday", 20 * 480);
    long.active = Some(false);
    p.tasks.push(long);
    let md = render_of(p, ReportKind::ProjectOverview);
    assert_eq!(rows(&md, "")[0][1], finish, "{md}");
    assert_eq!(finish, "Fri 3/13/26");
}
