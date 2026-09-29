use super::*;
use crate::model::Baseline;

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 1, day, hour, 0)
}

fn rate(text: &str) -> Option<Rate> {
    Rate::parse(text)
}

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

fn editor(tasks: Vec<Task>) -> Editor {
    let mut p = untitled_project();
    p.tasks = tasks;
    Editor::new(p)
}

fn read(ed: &Editor, uid: i32, name: &str) -> FieldRead {
    read_field(ed, uid, name).unwrap()
}

/// `(text, value)` of a field, for compact assertions.
fn tv(ed: &Editor, uid: i32, name: &str) -> (String, FieldValue) {
    let r = read(ed, uid, name);
    (r.text, r.value)
}

fn s(text: &str) -> String {
    text.to_string()
}

/// A leaf carrying a value in every stored field the registry reads.
fn rich() -> Task {
    let mut t = Task {
        percent_complete: Some(50),
        percent_work_complete: Some(25),
        physical_percent_complete: Some(10),
        actual_start: Some(at(5, 8)),
        actual_duration_min: Some(480),
        remaining_duration_min: Some(480),
        work_min: Some(960),
        actual_work_min: Some(480),
        remaining_work_min: Some(480),
        cost: rate("140000"),
        actual_cost: rate("70000"),
        remaining_cost: rate("70000"),
        fixed_cost: rate("10000"),
        fixed_cost_accrual: Some(AccrueAt::Start),
        constraint: ConstraintType::StartNoEarlierThan,
        constraint_date: Some(at(5, 8)),
        deadline: Some(at(9, 17)),
        wbs: Some("A.1".into()),
        leveling_delay: Some(14400),
        task_type: Some(TaskType::FixedWork),
        effort_driven: Some(true),
        priority: Some(700),
        notes: Some("Pour on a dry day".into()),
        ..task(1, "Pour", 960)
    };
    t.set_baseline_slot(Baseline {
        number: 0,
        start: Some(at(5, 8)),
        finish: Some(at(5, 17)),
        duration_min: Some(480),
        work_min: Some(480),
        cost: rate("4000000"),
        ..Baseline::default()
    });
    t.set_baseline_slot(Baseline {
        number: 3,
        finish: Some(at(8, 17)),
        ..Baseline::default()
    });
    t
}

#[test]
fn every_listed_field_reads_a_representative_value() {
    let ed = editor(vec![rich()]);
    let d = |day| FieldValue::Date(at(day, 8));
    let m = FieldValue::Minutes;
    let expected: Vec<(&str, &str, FieldValue)> = vec![
        ("ID", "1", FieldValue::Int(1)),
        (
            "Task Mode",
            "Auto Scheduled",
            FieldValue::Text(s("Auto Scheduled")),
        ),
        ("Name", "Pour", FieldValue::Text(s("Pour"))),
        ("Duration", "2d", m(960)),
        ("Start", "2026-01-05", d(5)),
        ("Finish", "2026-01-06", FieldValue::Date(at(6, 17))),
        ("Predecessors", "", FieldValue::Text(s(""))),
        ("Resource Names", "", FieldValue::Text(s(""))),
        ("% Complete", "50%", FieldValue::Int(50)),
        ("% Work Complete", "25%", FieldValue::Int(25)),
        ("Physical % Complete", "10%", FieldValue::Int(10)),
        ("Actual Start", "2026-01-05", d(5)),
        ("Actual Finish", "NA", FieldValue::Null),
        ("Actual Duration", "1 day", m(480)),
        ("Remaining Duration", "1 day", m(480)),
        ("Work", "16 hrs", m(960)),
        ("Actual Work", "8 hrs", m(480)),
        ("Remaining Work", "8 hrs", m(480)),
        ("Cost", "$1,400.00", FieldValue::Money(1400.0)),
        ("Actual Cost", "$700.00", FieldValue::Money(700.0)),
        ("Remaining Cost", "$700.00", FieldValue::Money(700.0)),
        ("Fixed Cost", "$100.00", FieldValue::Money(100.0)),
        ("Fixed Cost Accrual", "Start", FieldValue::Text(s("Start"))),
        ("Baseline Start", "2026-01-05", d(5)),
        ("Baseline Finish", "2026-01-05", FieldValue::Date(at(5, 17))),
        ("Baseline Duration", "1 day", m(480)),
        ("Baseline Work", "8 hrs", m(480)),
        ("Baseline Cost", "$40,000.00", FieldValue::Money(40000.0)),
        (
            "Baseline3 Finish",
            "2026-01-08",
            FieldValue::Date(at(8, 17)),
        ),
        ("Baseline3 Start", "NA", FieldValue::Null),
        ("Baseline3 Duration", "0 days", FieldValue::Null),
        ("Baseline3 Work", "0 hrs", FieldValue::Null),
        ("Baseline3 Cost", "$0.00", FieldValue::Null),
        ("Start Variance", "0 days", m(0)),
        ("Finish Variance", "1 day", m(480)),
        ("Duration Variance", "1 day", m(480)),
        ("Work Variance", "8 hrs", m(480)),
        ("Cost Variance", "($38,600.00)", FieldValue::Money(-38600.0)),
        ("Total Slack", "0 days", m(0)),
        ("Free Slack", "0 days", m(0)),
        ("Start Slack", "0 days", m(0)),
        ("Finish Slack", "0 days", m(0)),
        ("Early Start", "2026-01-05", d(5)),
        ("Early Finish", "2026-01-06", FieldValue::Date(at(6, 17))),
        ("Late Start", "2026-01-05", d(5)),
        ("Late Finish", "2026-01-06", FieldValue::Date(at(6, 17))),
        ("Critical", "Yes", FieldValue::Bool(true)),
        (
            "Constraint Type",
            "Start No Earlier Than",
            FieldValue::Text(s("Start No Earlier Than")),
        ),
        ("Constraint Date", "2026-01-05", d(5)),
        ("Deadline", "2026-01-09", FieldValue::Date(at(9, 17))),
        ("Active", "Yes", FieldValue::Bool(true)),
        ("Outline Number", "1", FieldValue::Text(s("1"))),
        ("Outline Level", "1", FieldValue::Int(1)),
        ("WBS", "A.1", FieldValue::Text(s("A.1"))),
        ("Leveling Delay", "1 eday", m(1440)),
        ("Type", "Fixed Work", FieldValue::Text(s("Fixed Work"))),
        ("Effort Driven", "Yes", FieldValue::Bool(true)),
        ("Priority", "700", FieldValue::Int(700)),
        (
            "Notes",
            "Pour on a dry day",
            FieldValue::Text(s("Pour on a dry day")),
        ),
        ("Milestone", "No", FieldValue::Bool(false)),
        ("Summary", "No", FieldValue::Bool(false)),
        ("Estimated", "No", FieldValue::Bool(false)),
        ("Unique ID", "1", FieldValue::Int(1)),
    ];
    for (name, text, value) in expected {
        assert_eq!(tv(&ed, 1, name), (s(text), value), "{name}");
    }
}

#[test]
fn unset_fields_read_null_but_show_what_project_shows() {
    let ed = editor(vec![task(1, "Bare", 480)]);
    for (name, text) in [
        ("% Complete", "0%"),
        ("% Work Complete", "0%"),
        ("Physical % Complete", "0%"),
        ("Actual Start", "NA"),
        ("Actual Finish", "NA"),
        ("Actual Duration", "0 days"),
        ("Remaining Duration", "0 days"),
        ("Work", "0 hrs"),
        ("Actual Work", "0 hrs"),
        ("Remaining Work", "0 hrs"),
        ("Cost", "$0.00"),
        ("Actual Cost", "$0.00"),
        ("Remaining Cost", "$0.00"),
        ("Fixed Cost", "$0.00"),
        ("Baseline Start", "NA"),
        ("Baseline Finish", "NA"),
        ("Baseline Duration", "0 days"),
        ("Baseline Work", "0 hrs"),
        ("Baseline Cost", "$0.00"),
        ("Baseline10 Finish", "NA"),
        ("Constraint Date", "NA"),
        ("Deadline", "NA"),
        ("Notes", ""),
    ] {
        assert_eq!(tv(&ed, 1, name), (s(text), FieldValue::Null), "{name}");
    }
    // Zero stored is a value, not an unset field.
    let zero = Task {
        percent_complete: Some(0),
        cost: rate("0"),
        work_min: Some(0),
        ..task(1, "Zero", 480)
    };
    let ed0 = editor(vec![zero]);
    assert_eq!(tv(&ed0, 1, "% Complete"), (s("0%"), FieldValue::Int(0)));
    assert_eq!(tv(&ed0, 1, "Cost"), (s("$0.00"), FieldValue::Money(0.0)));
    assert_eq!(tv(&ed0, 1, "Work"), (s("0 hrs"), FieldValue::Minutes(0)));
}

#[test]
fn defaults_read_their_effective_value() {
    let mut p = untitled_project();
    p.default_task_type = Some(TaskType::FixedDuration);
    p.new_tasks_effort_driven = Some(true);
    p.tasks = vec![task(1, "Bare", 480)];
    let ed = Editor::new(p);
    let text = |t: &str| FieldValue::Text(s(t));
    for (name, shown, value) in [
        ("Active", "Yes", FieldValue::Bool(true)),
        ("Priority", "500", FieldValue::Int(500)),
        ("Effort Driven", "Yes", FieldValue::Bool(true)),
        ("Type", "Fixed Duration", text("Fixed Duration")),
        ("Fixed Cost Accrual", "Prorated", text("Prorated")),
        (
            "Constraint Type",
            "As Soon As Possible",
            text("As Soon As Possible"),
        ),
        ("Leveling Delay", "0 edays", FieldValue::Minutes(0)),
        ("WBS", "1", text("1")),
        // No baseline: Start and Finish Variance are zero, and the other
        // variances are the current value less nothing.
        ("Start Variance", "0 days", FieldValue::Minutes(0)),
        ("Finish Variance", "0 days", FieldValue::Minutes(0)),
        ("Duration Variance", "1 day", FieldValue::Minutes(480)),
        ("Work Variance", "0 hrs", FieldValue::Minutes(0)),
        ("Cost Variance", "$0.00", FieldValue::Money(0.0)),
    ] {
        assert_eq!(tv(&ed, 1, name), (s(shown), value), "{name}");
    }
    let inactive = Task {
        active: Some(false),
        fixed_cost_accrual: Some(AccrueAt::Invalid),
        ..task(1, "Off", 480)
    };
    let ed = editor(vec![inactive]);
    assert_eq!(tv(&ed, 1, "Active"), (s("No"), FieldValue::Bool(false)));
    assert_eq!(read(&ed, 1, "Fixed Cost Accrual").text, "Prorated");
}

#[test]
fn every_name_reads_on_a_leaf_a_summary_a_milestone_and_a_blank_row() {
    let tasks = vec![
        Task {
            summary: true,
            id: 1,
            ..task(10, "Phase", 0)
        },
        Task {
            outline_level: 2,
            ..rich()
        },
        Task {
            uid: 2,
            id: 2,
            outline_level: 2,
            milestone: true,
            ..task(2, "Done", 0)
        },
        Task {
            is_null: true,
            ..task(3, "", 0)
        },
    ];
    let ed = editor(tasks);
    let names = field_names();
    for uid in [10, 1, 2] {
        for name in &names {
            read(&ed, uid, name);
        }
    }
    assert_eq!(read(&ed, 10, "Summary").text, "Yes");
    assert_eq!(read(&ed, 10, "Duration").text, "2d");
    assert_eq!(read(&ed, 2, "Milestone").text, "Yes");
    assert_eq!(read(&ed, 2, "Duration").text, "—");
    assert_eq!(read(&ed, 10, "Outline Number").text, "1");
    assert_eq!(read(&ed, 1, "Outline Number").text, "1.1");
    assert_eq!(read(&ed, 2, "Outline Number").text, "1.2");
    for name in &names {
        let r = read(&ed, 3, name);
        match name.as_str() {
            "ID" | "Unique ID" => assert_eq!(r.text, "3"),
            _ => assert_eq!((r.text, r.value), (s(""), FieldValue::Null), "{name}"),
        }
    }
}

#[test]
fn names_list_the_families_and_match_loosely() {
    let names = field_names();
    assert_eq!(names.len(), 23 + 11 * 5 + 30);
    assert_eq!(names[..3], [s("ID"), s("Task Mode"), s("Name")]);
    for name in [
        "Baseline Start",
        "Baseline1 Finish",
        "Baseline10 Cost",
        "WBS",
    ] {
        assert!(names.iter().any(|n| n == name), "{name}");
    }
    for name in &names {
        assert_eq!(Field::parse(name).map(Field::name).as_ref(), Ok(name));
    }
    assert_eq!(Field::parse("  % complete "), Ok(Field::PercentComplete));
    assert_eq!(
        Field::parse("baseline7 FINISH"),
        Ok(Field::Baseline(7, BaselinePart::Finish))
    );
    assert_eq!(
        Field::parse("Baseline11 Start"),
        Err(s("unknown task field 'Baseline11 Start'"))
    );
    assert_eq!(
        Field::parse("Status"),
        Err(s("unknown task field 'Status'"))
    );
    let ed = editor(vec![task(1, "A", 480)]);
    assert_eq!(read_field(&ed, 9, "Name"), Err(s("no task with uid 9")));
}

#[test]
fn durations_use_project_spellings() {
    let p = untitled_project();
    let unit = |code| DurationUnit::of_code(code).unwrap();
    for (min, code, estimated, text) in [
        (0, 7, false, "0 days"),
        (480, 7, false, "1 day"),
        (-480, 7, false, "-1 day"),
        (600, 7, false, "1.25 days"),
        (5760, 7, true, "12 days?"),
        (4800, 9, false, "2 wks"),
        (2400, 9, false, "1 wk"),
        (240, 5, false, "4 hrs"),
        (60, 5, false, "1 hr"),
        (3, 3, false, "3 mins"),
        (9600, 11, false, "1 mon"),
        (2880, 8, false, "2 edays"),
        (1440, 8, false, "1 eday"),
        (120, 6, false, "2 ehrs"),
        (10080, 10, false, "1 ewk"),
        (160, 7, false, "0.33 days"),
    ] {
        assert_eq!(
            format_duration_field(&p, min, unit(code), estimated),
            text,
            "{min} {code}"
        );
    }
    assert_eq!(DurationUnit::of_code(19), None);
    assert_eq!(format_work(0), "0 hrs");
    assert_eq!(format_work(60), "1 hr");
    assert_eq!(format_work(240), "4 hrs");
    assert_eq!(format_work(90), "1.5 hrs");
    assert_eq!(format_work(-480), "-8 hrs");
}

#[test]
fn money_percent_and_dates_use_project_spellings() {
    assert_eq!(format_money(1400.0), "$1,400.00");
    assert_eq!(format_money(-40000.0), "($40,000.00)");
    assert_eq!(format_money(0.0), "$0.00");
    assert_eq!(format_money(1234567.891), "$1,234,567.89");
    assert_eq!(format_money(0.5), "$0.50");
    assert_eq!(format_date_field(Some(at(5, 8))), "2026-01-05");
    assert_eq!(format_date_field(None), "NA");
}

#[test]
fn a_task_shows_its_durations_and_slack_in_its_own_unit() {
    let hours = Task {
        duration_format: Some(5),
        actual_duration_min: Some(120),
        ..task(1, "Short", 240)
    };
    let ed = editor(vec![hours]);
    assert_eq!(read(&ed, 1, "Duration").text, "4h");
    assert_eq!(read(&ed, 1, "Actual Duration").text, "2 hrs");
    assert_eq!(read(&ed, 1, "Total Slack").text, "0 hrs");
    let elapsed = Task {
        duration_format: Some(8),
        remaining_duration_min: Some(2880),
        ..task(1, "Cure", 2880)
    };
    let ed = editor(vec![elapsed]);
    assert_eq!(read(&ed, 1, "Remaining Duration").text, "2 edays");
    // Slack is working time, shown in the working form of the unit.
    assert!(read(&ed, 1, "Total Slack").text.ends_with("days"));
}

#[test]
fn negative_total_slack_reads_negative() {
    let late = Task {
        deadline: Some(at(5, 17)),
        ..task(1, "Late", 1440)
    };
    let ed = editor(vec![late]);
    assert_eq!(
        tv(&ed, 1, "Total Slack"),
        (s("-2 days"), FieldValue::Minutes(-960))
    );
    assert_eq!(read(&ed, 1, "Critical").text, "Yes");
    // Free slack is floored at zero.
    assert_eq!(read(&ed, 1, "Free Slack").value, FieldValue::Minutes(0));
}

#[test]
fn variances_follow_the_live_schedule_after_set_baseline() {
    let mut ed = editor(vec![task(1, "A", 960), {
        let mut b = task(2, "B", 480);
        b.predecessors.push(crate::model::Predecessor::fs(1));
        b
    }]);
    ed.set_baseline();
    for name in ["Start Variance", "Finish Variance", "Duration Variance"] {
        assert_eq!(
            tv(&ed, 2, name),
            (s("0 days"), FieldValue::Minutes(0)),
            "{name}"
        );
    }
    let finish = read(&ed, 2, "Baseline Finish");
    ed.set_duration_min(1, 1920, false).unwrap();
    assert_eq!(read(&ed, 2, "Baseline Finish"), finish);
    assert_eq!(
        tv(&ed, 2, "Finish Variance"),
        (s("2 days"), FieldValue::Minutes(960))
    );
    assert_eq!(tv(&ed, 2, "Start Variance").1, FieldValue::Minutes(960));
    assert_eq!(tv(&ed, 1, "Duration Variance").1, FieldValue::Minutes(960));
    // Finishing earlier than the baseline is negative.
    ed.set_duration_min(1, 480, false).unwrap();
    assert_eq!(
        tv(&ed, 2, "Finish Variance"),
        (s("-1 day"), FieldValue::Minutes(-480))
    );
}

#[test]
fn the_progress_fixture_reads_its_tracking_fields() {
    let proj =
        crate::mspdi::read_mspdi(include_str!("../../../../corpus/mspdi/22-progress.xml")).unwrap();
    let ed = Editor::new(proj);
    let pour = 2;
    assert_eq!(tv(&ed, pour, "% Complete"), (s("50%"), FieldValue::Int(50)));
    assert_eq!(
        tv(&ed, pour, "Actual Start"),
        (
            s("2026-03-04"),
            FieldValue::Date(DateTime::from_ymd_hm(2026, 3, 4, 8, 0))
        )
    );
    assert_eq!(
        tv(&ed, pour, "Actual Duration"),
        (s("2 days"), FieldValue::Minutes(960))
    );
    assert_eq!(
        tv(&ed, pour, "Remaining Duration"),
        (s("2 days"), FieldValue::Minutes(960))
    );
    assert_eq!(read(&ed, pour, "Actual Cost").value, FieldValue::Money(8.0));
}

#[test]
fn the_project_summary_row_reads_its_rollup_and_stored_values() {
    let mut summary = Task {
        uid: 0,
        id: 0,
        name: "Plan".into(),
        outline_level: 0,
        summary: true,
        percent_complete: Some(25),
        cost: rate("140000"),
        notes: Some("Kickoff".into()),
        ..Task::default()
    };
    summary.duration_min = 0;
    let ed = editor(vec![summary, task(1, "A", 960), task(2, "B", 480)]);
    for (name, text) in [
        ("Duration", "2d"),
        ("Start", "2026-01-05"),
        ("Finish", "2026-01-06"),
        ("% Complete", "25%"),
        ("Cost", "$1,400.00"),
        ("Notes", "Kickoff"),
        ("Outline Number", "0"),
    ] {
        assert_eq!(read(&ed, 0, name).text, text, "{name}");
    }
}
