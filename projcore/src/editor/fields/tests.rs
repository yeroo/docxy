use super::*;
use crate::model::{Baseline, ExtendedAttributeValue, XmlElement};

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

/// Read one field of task `uid` by name.
fn read_field(ed: &Editor, uid: i32, name: &str) -> Result<FieldRead, String> {
    let field = Field::parse(name)?;
    let task = ed
        .project()
        .task(uid)
        .ok_or(format!("no task with uid {uid}"))?;
    Ok(FieldReader::new(ed).read(task, field))
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
fn hyperlink_fields_read_the_corpus_values() {
    let proj =
        crate::mspdi::read_mspdi(include_str!("../../../../corpus/mspdi/20-task-fields.xml"))
            .unwrap();
    let ed = Editor::new(proj);
    let text = |t: &str| (s(t), FieldValue::Text(s(t)));
    // Pour (uid 4) stores all three parts, read as Project names them.
    assert_eq!(tv(&ed, 4, "Hyperlink"), text("Pour instructions"));
    assert_eq!(
        tv(&ed, 4, "Hyperlink Address"),
        text("https://example.com/a?x=1&y=2")
    );
    assert_eq!(tv(&ed, 4, "Hyperlink SubAddress"), text("Gantt Chart!4"));
    // The names parse loosely, like every other field's.
    assert_eq!(Field::parse("Hyperlink"), Ok(Field::Hyperlink));
    assert_eq!(
        Field::parse("hyperlink address"),
        Ok(Field::HyperlinkAddress)
    );
    assert_eq!(
        Field::parse("  HYPERLINK subaddress "),
        Ok(Field::HyperlinkSubAddress)
    );
    // A task without a link reads empty text and Null; so does the blank row.
    for uid in [2, 3] {
        for name in ["Hyperlink", "Hyperlink Address", "Hyperlink SubAddress"] {
            assert_eq!(
                tv(&ed, uid, name),
                (s(""), FieldValue::Null),
                "uid {uid} {name}"
            );
        }
    }
    // The registry lists the three names, directly after Notes.
    let names = field_names();
    let notes = names.iter().position(|n| n == "Notes").unwrap();
    assert_eq!(
        names[notes + 1..notes + 4],
        [
            s("Hyperlink"),
            s("Hyperlink Address"),
            s("Hyperlink SubAddress")
        ]
    );
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
    // Its stored duration is 0, but a summary is not a milestone by length.
    assert_eq!(read(&ed, 10, "Milestone").text, "No");
    assert_eq!(read(&ed, 10, "Duration").text, "2d");
    assert_eq!(read(&ed, 2, "Milestone").text, "Yes");
    assert_eq!(read(&ed, 2, "Duration").text, "0d");
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
fn milestone_duration_reads_zero_in_its_unit_and_toggles_back_to_a_day() {
    let mut weekly = task(1, "Weekly", 2400);
    weekly.duration_format = Some(9);
    let mut ed = editor(vec![weekly, task(2, "Daily", 480)]);
    ed.toggle_milestone(1).unwrap();
    assert_eq!(read(&ed, 1, "Milestone").text, "Yes");
    assert_eq!(read(&ed, 1, "Duration").text, "0w");
    ed.toggle_milestone(2).unwrap();
    assert_eq!(read(&ed, 2, "Duration").text, "0d");
    ed.toggle_milestone(2).unwrap();
    assert_eq!(read(&ed, 2, "Milestone").text, "No");
    assert_eq!(read(&ed, 2, "Duration").text, "1d");
}

#[test]
fn an_estimated_milestone_shows_no_estimate_suffix() {
    let mut t = task(1, "Guess", 480);
    t.estimated = Some(true);
    let mut ed = editor(vec![t]);
    assert_eq!(read(&ed, 1, "Duration").text, "1d?");
    ed.toggle_milestone(1).unwrap();
    assert_eq!(read(&ed, 1, "Milestone").text, "Yes");
    // `duration_suffix` shows no `?` for a milestone.
    assert_eq!(read(&ed, 1, "Duration").text, "0d");
}

#[test]
fn names_list_the_families_and_match_loosely() {
    let names = field_names();
    assert_eq!(names.len(), 23 + 11 * 5 + 34 + 120);
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
    assert_eq!(Field::parse("Bogus"), Err(s("unknown task field 'Bogus'")));
    assert_eq!(Field::parse(" status "), Ok(Field::Status));
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
    let mut elapsed = Task {
        duration_format: Some(8),
        remaining_duration_min: Some(2880),
        ..task(1, "Cure", 2880)
    };
    elapsed.set_baseline_slot(Baseline {
        number: 0,
        start: Some(at(6, 8)),
        ..Baseline::default()
    });
    let ed = editor(vec![elapsed]);
    assert_eq!(read(&ed, 1, "Remaining Duration").text, "2 edays");
    // Slack and date variances are working time, shown in the working form
    // of the unit.
    assert_eq!(read(&ed, 1, "Total Slack").text, "0 days");
    assert_eq!(
        tv(&ed, 1, "Start Variance"),
        (s("-1 day"), FieldValue::Minutes(-480))
    );
}

#[test]
fn a_baseline_duration_shows_in_its_own_format() {
    let mut t = task(1, "A", 480);
    t.set_baseline_slot(Baseline {
        number: 0,
        duration_min: Some(4800),
        duration_format: Some(9),
        ..Baseline::default()
    });
    t.set_baseline_slot(Baseline {
        number: 2,
        duration_min: Some(1440),
        duration_format: Some(39),
        ..Baseline::default()
    });
    t.set_baseline_slot(Baseline {
        number: 4,
        duration_min: Some(0),
        duration_format: Some(19),
        ..Baseline::default()
    });
    let ed = editor(vec![Task {
        duration_format: Some(5),
        ..t
    }]);
    assert_eq!(
        tv(&ed, 1, "Baseline Duration"),
        (s("2 wks"), FieldValue::Minutes(4800))
    );
    assert_eq!(
        tv(&ed, 1, "Baseline2 Duration"),
        (s("3 days?"), FieldValue::Minutes(1440))
    );
    // A format with no duration unit falls back to the task's.
    assert_eq!(read(&ed, 1, "Baseline4 Duration").text, "0 hrs");
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
        ("Milestone", "No"),
    ] {
        assert_eq!(read(&ed, 0, name).text, text, "{name}");
    }
}

#[test]
fn wbs_is_the_stored_code_else_the_live_outline_number() {
    let tasks = vec![
        task(1, "A", 480),
        Task {
            wbs: Some("2".into()),
            ..task(2, "B", 480)
        },
        task(3, "C", 480),
        Task {
            wbs: Some("ABC.42".into()),
            ..task(4, "D", 480)
        },
    ];
    let mut ed = editor(tasks);
    assert_eq!(read(&ed, 1, "WBS").text, "1");
    assert_eq!(read(&ed, 2, "WBS").text, "2");
    assert_eq!(read(&ed, 3, "WBS").text, "3");
    assert_eq!(read(&ed, 4, "WBS").text, "ABC.42");
    // Indents move the outline numbers. A stored code that is the task's
    // outline number renumbers with it; a task without one reads its live
    // number; an explicit override stays as the plan holds it (a save
    // writes it unchanged).
    ed.indent(2, 1).unwrap();
    ed.indent(3, 1).unwrap();
    assert_eq!(read(&ed, 1, "WBS").text, "1");
    assert_eq!(read(&ed, 2, "WBS").text, "1.1");
    assert_eq!(read(&ed, 3, "WBS").text, "1.2");
    assert_eq!(read(&ed, 4, "WBS").text, "ABC.42");
}

#[test]
fn delete_renumbers_generated_wbs_and_undo_restores_it() {
    let tasks = vec![
        task(1, "A", 480),
        task(2, "B", 480),
        Task {
            wbs: Some("3".into()),
            ..task(3, "C", 480)
        },
        task(4, "D", 480),
    ];
    let mut ed = editor(tasks);
    ed.delete_task(2).unwrap();
    // The delete moved C from outline 3 to outline 2, and its stored
    // generated code followed it.
    assert_eq!(read(&ed, 3, "WBS").text, "2");
    assert_eq!(read(&ed, 4, "WBS").text, "3");
    // Undo is a whole-project snapshot: every stored code comes back exactly.
    assert!(ed.undo());
    assert_eq!(read(&ed, 2, "WBS").text, "2");
    assert_eq!(read(&ed, 3, "WBS").text, "3");
    assert_eq!(read(&ed, 4, "WBS").text, "4");
}

#[test]
fn added_and_inserted_tasks_never_collide_with_a_generated_wbs() {
    let tasks = vec![
        task(1, "A", 480),
        Task {
            wbs: Some("2".into()),
            ..task(2, "B", 480)
        },
    ];
    let mut ed = editor(tasks);
    // A task added above a stored-code row takes its outline number; the
    // stored code renumbers after it.
    let at = ed.add_task(Some(1), "Added", 480, false).unwrap();
    let added = ed.project().tasks[at].uid;
    ed.insert_blank_row(Some(2)).unwrap();
    // Typing into the blank row gives it the level of the row above and a
    // fresh outline number.
    let blank = ed.project().tasks[2].uid;
    ed.rename(blank, "Typed").unwrap();
    assert_eq!(read(&ed, added, "WBS").text, "2");
    assert_eq!(read(&ed, blank, "WBS").text, "3");
    assert_eq!(read(&ed, 2, "WBS").text, "4");
    let wbs: Vec<String> = ed
        .project()
        .tasks
        .iter()
        .filter(|t| !t.is_null)
        .map(|t| read(&ed, t.uid, "WBS").text)
        .collect();
    let mut distinct = wbs.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct, wbs, "a WBS collides with another row's");
}

#[test]
fn a_rename_leaves_stored_wbs_alone() {
    let tasks = vec![
        Task {
            wbs: Some("ABC.42".into()),
            ..task(1, "A", 480)
        },
        Task {
            wbs: Some("2".into()),
            ..task(2, "B", 480)
        },
    ];
    let mut ed = editor(tasks);
    // A non-structural edit renumbers nothing: the override and the stored
    // generated code read exactly as before.
    ed.rename(1, "Renamed").unwrap();
    ed.set_duration(2, "2d").unwrap();
    assert_eq!(read(&ed, 1, "WBS").text, "ABC.42");
    assert_eq!(read(&ed, 2, "WBS").text, "2");
}

#[test]
fn a_masked_code_is_never_renumbered() {
    // A code from a custom mask never equals an outline number, so a
    // structural edit leaves it exactly as read.
    let tasks = vec![
        task(1, "A", 480),
        Task {
            wbs: Some("PRJ-01.02".into()),
            ..task(2, "B", 480)
        },
        task(3, "C", 480),
    ];
    let mut ed = editor(tasks);
    ed.indent(2, 1).unwrap();
    ed.indent(3, 1).unwrap();
    assert_eq!(read(&ed, 2, "WBS").text, "PRJ-01.02");
    assert!(ed.undo());
    assert_eq!(read(&ed, 2, "WBS").text, "PRJ-01.02");
}

#[test]
fn a_save_holds_the_renumbered_wbs() {
    let tasks = vec![
        task(1, "A", 480),
        Task {
            wbs: Some("2".into()),
            ..task(2, "B", 480)
        },
    ];
    let mut ed = editor(tasks);
    ed.indent(2, 1).unwrap();
    let xml = crate::mspdi::write_mspdi(ed.project());
    assert!(
        xml.contains("<WBS>1.1</WBS>"),
        "the save holds the renumbered code: {xml}"
    );
    // Reopened, the renumbered code is the stored one and reads as itself.
    let reopened = Editor::new(crate::mspdi::read_mspdi(&xml).unwrap());
    assert_eq!(read(&reopened, 2, "WBS").text, "1.1");
}

#[test]
fn a_baseline_set_while_leveled_records_the_saved_schedule() {
    // Under a summary, whose Duration is its span: leveling stretches the
    // shown span, but the variance measures the scheduled one.
    let phase = Task {
        summary: true,
        id: 3,
        ..task(10, "Phase", 0)
    };
    let under = |t: Task| Task {
        outline_level: 2,
        ..t
    };
    let mut ed = editor(vec![
        phase,
        under(task(1, "A", 960)),
        under(task(2, "B", 480)),
    ]);
    assert_eq!(read(&ed, 10, "Summary").text, "Yes");
    ed.assign_resource(1, "Alice").unwrap();
    ed.assign_resource(2, "Alice").unwrap();
    ed.toggle_level();
    let early = |ed: &Editor, uid| ed.schedule().get(uid).unwrap().early_start;
    let delayed = [1, 2]
        .into_iter()
        .find(|&uid| ed.disp_start(uid) != Some(early(&ed, uid)))
        .expect("leveling delays one of the two");
    assert_ne!(
        ed.disp_duration_min(10),
        crate::schedule::task_duration_min(ed.project(), ed.schedule(), &ed.project().tasks[0]),
        "leveling stretches the summary's shown span"
    );
    ed.set_baseline();
    // Leveling is a view: the baseline is the CPM schedule a save writes,
    // and the variances measure that schedule too.
    assert_eq!(
        read(&ed, delayed, "Baseline Start").value,
        FieldValue::Date(early(&ed, delayed))
    );
    assert_ne!(
        read(&ed, delayed, "Start").value,
        read(&ed, delayed, "Baseline Start").value
    );
    let variances = |ed: &Editor| {
        for uid in [10, 1, 2] {
            for name in ["Start Variance", "Finish Variance", "Duration Variance"] {
                assert_eq!(
                    tv(ed, uid, name),
                    (s("0 days"), FieldValue::Minutes(0)),
                    "uid {uid} {name}"
                );
            }
        }
    };
    variances(&ed);
    // Saved and reopened, the plan keeps that baseline and still reads no
    // variance, with or without leveling.
    let baselines = |ed: &Editor| -> Vec<_> {
        ed.project()
            .tasks
            .iter()
            .map(|t| t.baseline(0).map(|b| (b.start, b.finish, b.duration_min)))
            .collect()
    };
    let xml = crate::mspdi::write_mspdi(ed.project());
    let mut reopened = Editor::new(crate::mspdi::read_mspdi(&xml).unwrap());
    assert_eq!(baselines(&reopened), baselines(&ed));
    variances(&reopened);
    reopened.toggle_level();
    variances(&reopened);
}

/// A March 2026 instant, the month the x-status oracle uses.
fn dt(day: u32, hour: u32, minute: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 3, day, hour, minute)
}

/// `YYYY-MM-DD HH:MM`, for assertion messages.
fn show(d: DateTime) -> String {
    let p = d.parts();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        p.year, p.month, p.day, p.hour, p.minute
    )
}

#[test]
fn status_rule_matches_the_oracle_transitions() {
    // The x-status oracle's tasks, resume points and transition instants,
    // transcribed from corpus/mpp/snapshots/extras.json (key x-status) and
    // corpus/mpp/snapshots/x-status-sweep.json (key transitions), measured by
    // Project 2024 over COM (yeroo/mpp-corpus 644e3a9). Resume points: a
    // started task's stored resume snapped to the next working start (S:
    // Resume 3/10 17:00 -> 3/11 08:00); a 0% task's raw Start (M, at 17:00,
    // never snapped).
    let feb27 = DateTime::from_ymd_hm(2026, 2, 27, 0, 0);
    let sweep_end = dt(23, 23, 59);
    type OracleTask<'a> = (
        &'a str,
        Option<u8>,
        DateTime,
        DateTime,
        &'a [(DateTime, &'a str)],
    );
    let tasks: [OracleTask; 11] = [
        (
            "C",
            Some(100),
            dt(2, 8, 0),
            dt(4, 17, 0),
            &[(feb27, "Complete")],
        ),
        (
            "H",
            Some(50),
            dt(9, 8, 0),
            dt(11, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(9, 8, 0), "On Schedule"),
                (dt(12, 0, 0), "Late"),
            ],
        ),
        (
            "Q",
            Some(25),
            dt(9, 8, 0),
            dt(10, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(9, 8, 0), "On Schedule"),
                (dt(11, 0, 0), "Late"),
            ],
        ),
        (
            "Z",
            Some(0),
            dt(2, 8, 0),
            dt(2, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(2, 8, 0), "On Schedule"),
                (dt(3, 0, 0), "Late"),
            ],
        ),
        (
            "F",
            Some(0),
            dt(16, 8, 0),
            dt(16, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(16, 8, 0), "On Schedule"),
                (dt(17, 0, 0), "Late"),
            ],
        ),
        (
            "M",
            Some(0),
            dt(11, 17, 0),
            dt(11, 17, 0),
            &[
                (feb27, "Future Task"),
                (dt(11, 17, 0), "On Schedule"),
                (dt(12, 0, 0), "Late"),
            ],
        ),
        (
            "S",
            Some(50),
            dt(9, 8, 0),
            dt(11, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(9, 8, 0), "On Schedule"),
                (dt(12, 0, 0), "Late"),
            ],
        ),
        (
            "S1",
            Some(100),
            dt(9, 8, 0),
            dt(10, 17, 0),
            &[(feb27, "Complete")],
        ),
        (
            "S2",
            Some(0),
            dt(11, 8, 0),
            dt(11, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(11, 8, 0), "On Schedule"),
                (dt(12, 0, 0), "Late"),
            ],
        ),
        (
            "P",
            Some(25),
            dt(9, 8, 0),
            dt(13, 8, 0),
            &[
                (feb27, "Future Task"),
                (dt(9, 8, 0), "On Schedule"),
                (dt(14, 0, 0), "Late"),
            ],
        ),
        (
            "U",
            Some(30),
            dt(9, 8, 0),
            dt(10, 9, 36),
            &[
                (feb27, "Future Task"),
                (dt(9, 8, 0), "On Schedule"),
                (dt(11, 0, 0), "Late"),
            ],
        ),
    ];
    for (name, percent, start, resume_point, transitions) in tasks {
        for (i, (at, want)) in transitions.iter().enumerate() {
            let got = task_status(percent, start, resume_point, *at);
            assert_eq!(got, *want, "{name} at {}", show(*at));
            if i > 0 {
                let before = at.add_minutes(-1);
                let got = task_status(percent, start, resume_point, before);
                assert_eq!(got, transitions[i - 1].1, "{name} at {}", show(before));
            }
        }
        let got = task_status(percent, start, resume_point, sweep_end);
        assert_eq!(
            got,
            transitions.last().unwrap().1,
            "{name} at {}",
            show(sweep_end)
        );
    }
}

#[test]
fn status_resume_point_snaps_to_the_next_working_start() {
    // The oracle's S (Resume 3/10 17:00) resumes at the next working start,
    // 3/11 08:00 on the Standard calendar, so it stays On Schedule through
    // 3/11; a 0% task compares against its raw Start, so M (Start 3/11 17:00)
    // is Late from 3/12 00:00 rather than 3/13. extras.json key x-status.rule.
    let half = || Task {
        percent_complete: Some(50),
        stop: Some(dt(10, 17, 0)),
        resume: Some(dt(10, 17, 0)),
        constraint: ConstraintType::StartNoEarlierThan,
        constraint_date: Some(dt(9, 8, 0)),
        ..task(1, "Half", 1920)
    };
    let zero = || Task {
        constraint: ConstraintType::StartNoEarlierThan,
        constraint_date: Some(dt(11, 17, 0)),
        ..task(2, "Zero", 0)
    };
    for (status_date, half_want, zero_want) in [
        ("2026-03-11T23:59", "On Schedule", "On Schedule"),
        ("2026-03-12T00:00", "Late", "Late"),
    ] {
        let mut p = untitled_project();
        p.options.push(("StatusDate".into(), status_date.into()));
        p.tasks = vec![half(), zero()];
        let ed = Editor::new(p);
        assert_eq!(
            tv(&ed, 1, "Status").0,
            s(half_want),
            "StatusDate {status_date}"
        );
        assert_eq!(
            tv(&ed, 2, "Status").0,
            s(zero_want),
            "StatusDate {status_date}"
        );
    }
}

#[test]
fn status_uses_status_date_then_current_date_then_empty() {
    // extras.json key x-status.no_status_date: with StatusDate = NA Project
    // measures Status at CurrentDate, never the wall clock; with neither (or
    // an unparsable StatusDate and no CurrentDate) the field reads ("", Null),
    // never an error.
    let build = |options: &[(&str, &str)]| {
        let mut p = untitled_project();
        p.options
            .extend(options.iter().map(|(n, v)| (n.to_string(), v.to_string())));
        p.tasks = vec![Task {
            constraint: ConstraintType::StartNoEarlierThan,
            constraint_date: Some(dt(9, 8, 0)),
            ..task(1, "T", 480)
        }];
        Editor::new(p)
    };
    // StatusDate wins over CurrentDate: on 3/9, the calendar day the 0% task
    // starting 3/9 08:00 reads On Schedule, where CurrentDate 3/13 would make
    // it Late.
    let on_schedule = (s("On Schedule"), FieldValue::Text(s("On Schedule")));
    let both = build(&[
        ("StatusDate", "2026-03-09T17:00"),
        ("CurrentDate", "2026-03-13T17:00"),
    ]);
    assert_eq!(tv(&both, 1, "Status"), on_schedule);
    let current_only = build(&[("CurrentDate", "2026-03-09T17:00")]);
    assert_eq!(tv(&current_only, 1, "Status"), on_schedule);
    let garbage = build(&[
        ("StatusDate", "garbage"),
        ("CurrentDate", "2026-03-09T17:00"),
    ]);
    assert_eq!(tv(&garbage, 1, "Status"), on_schedule);
    for options in [&[][..], &[("StatusDate", "NA")][..]] {
        let ed = build(options);
        assert_eq!(tv(&ed, 1, "Status"), (s(""), FieldValue::Null));
    }
}

#[test]
fn status_follows_an_edit_and_survives_a_save() {
    // extras.json x-status: F (0%, Start 3/16) is Future Task at StatusDate
    // 2026-03-10T17:00; moved to 3/9 it is Late, and a save keeps both the
    // Status and the StatusDate option.
    let mut p = untitled_project();
    p.options
        .push(("StatusDate".into(), "2026-03-10T17:00".into()));
    p.tasks = vec![Task {
        constraint: ConstraintType::StartNoEarlierThan,
        constraint_date: Some(dt(16, 8, 0)),
        ..task(1, "F", 480)
    }];
    let mut ed = Editor::new(p);
    assert_eq!(tv(&ed, 1, "Status").0, s("Future Task"));
    ed.set_start(1, dt(9, 8, 0)).unwrap();
    assert_eq!(tv(&ed, 1, "Status").0, s("Late"));
    let xml = crate::mspdi::write_mspdi(ed.project());
    let reread = crate::mspdi::read_mspdi(&xml).unwrap();
    assert_eq!(reread.option("StatusDate"), Some("2026-03-10T17:00"));
    let ed2 = Editor::new(reread);
    assert_eq!(tv(&ed2, 1, "Status").0, s("Late"));
}

#[test]
fn status_matches_project_on_the_x_status_corpus() {
    // Expected Status for x-status.xml (StatusDate 2026-03-10T17:00) and
    // x-status-nodate.xml (no StatusDate, CurrentDate 2026-03-13T17:00),
    // transcribed from corpus/mpp/snapshots/extras.json key x-status.status
    // (Project 2024 over COM, yeroo/mpp-corpus 644e3a9).
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/snapshots");
    if !dir.join("x-status.xml").exists() {
        eprintln!("SKIPPED: corpus/mpp/snapshots/x-status.xml absent");
        return;
    }
    let cases = [
        (
            "x-status.xml",
            [
                "Complete",
                "On Schedule",
                "On Schedule",
                "Late",
                "Future Task",
                "Future Task",
                "On Schedule",
                "Complete",
                "Future Task",
                "On Schedule",
                "On Schedule",
            ],
        ),
        (
            "x-status-nodate.xml",
            [
                "Complete",
                "Late",
                "Late",
                "Late",
                "Future Task",
                "Late",
                "Late",
                "Complete",
                "Late",
                "On Schedule",
                "Late",
            ],
        ),
    ];
    for (file, want) in cases {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        let ed = Editor::new(crate::mspdi::read_mspdi(&text).unwrap());
        for (i, want) in want.iter().enumerate() {
            let uid = i as i32 + 1;
            assert_eq!(tv(&ed, uid, "Status").0, s(want), "{file} uid {uid}");
        }
    }
}

#[test]
fn status_matches_project_over_the_x_status_sweep() {
    // x-status-sweep.json holds Project 2024's Status for the x-status plan at
    // 181 StatusDate rows plus 4 StatusDate=NA rows keyed by CurrentDate.
    // Line-scanned, since projcore keeps no JSON dependency. The sweep date
    // values are minute-precise and parse as MSPDI datetimes as-is.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/snapshots");
    let Ok(text) = std::fs::read_to_string(dir.join("x-status-sweep.json")) else {
        eprintln!("SKIPPED: corpus/mpp/snapshots/x-status-sweep.json absent");
        return;
    };
    let Ok(base_text) = std::fs::read_to_string(dir.join("x-status.xml")) else {
        eprintln!("SKIPPED: corpus/mpp/snapshots/x-status.xml absent");
        return;
    };
    let base = crate::mspdi::read_mspdi(&base_text).unwrap();
    // (date, status_date_is_na, wants)
    type Row = (String, bool, Vec<(i32, String)>);
    let mut rows: Vec<Row> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.split("\"status_date\": \"").nth(1) {
            rows.push((
                rest.split('"').next().unwrap().to_string(),
                false,
                Vec::new(),
            ));
        } else if let Some(rest) = line.split("\"current_date\": \"").nth(1) {
            rows.push((
                rest.split('"').next().unwrap().to_string(),
                true,
                Vec::new(),
            ));
        } else if let Some(row) = rows.last_mut() {
            let Some(rest) = line.trim().strip_prefix('"') else {
                continue;
            };
            let Some((uid, rest)) = rest.split_once("\": \"") else {
                continue;
            };
            let Ok(uid) = uid.parse::<i32>() else {
                continue;
            };
            row.2
                .push((uid, rest.trim_end_matches([',', '"']).to_string()));
        }
    }
    assert_eq!(rows.len(), 181 + 4, "sweep row count");
    for (date, is_na, wants) in &rows {
        assert_eq!(wants.len(), 11, "row {date} expectations");
        let mut proj = base.clone();
        proj.options
            .retain(|(n, _)| n != "StatusDate" && n != "CurrentDate");
        if *is_na {
            proj.options.push(("CurrentDate".into(), date.clone()));
        } else {
            proj.options.push(("StatusDate".into(), date.clone()));
        }
        let ed = Editor::new(proj);
        for (uid, want) in wants {
            assert_eq!(
                &tv(&ed, *uid, "Status").0,
                want,
                "status date {date} uid {uid}"
            );
        }
    }
}

// ---- custom fields (#577) ----

/// An `<ExtendedAttribute>` definition element the way `read_mspdi` keeps it.
fn custom_def(field_id: &str, field_name: &str) -> XmlElement {
    node(
        "ExtendedAttribute",
        vec![leaf("FieldID", field_id), leaf("FieldName", field_name)],
    )
}

fn leaf(name: &str, text: &str) -> XmlElement {
    XmlElement {
        name: name.into(),
        text: text.into(),
        children: Vec::new(),
    }
}

fn node(name: &str, children: Vec<XmlElement>) -> XmlElement {
    XmlElement {
        name: name.into(),
        text: String::new(),
        children,
    }
}

fn custom_value(field_id: &str, value: &str) -> ExtendedAttributeValue {
    ExtendedAttributeValue {
        field_id: field_id.into(),
        value: Some(value.into()),
        ..ExtendedAttributeValue::default()
    }
}

#[test]
fn custom_field_names_parse_and_list() {
    for (given, canonical) in [
        ("Text1", "Text1"),
        ("text30", "Text30"),
        ("Number20", "Number20"),
        ("Cost10", "Cost10"),
        ("Flag20", "Flag20"),
        ("Date10", "Date10"),
        ("Start10", "Start10"),
        ("Finish10", "Finish10"),
        ("Duration10", "Duration10"),
        ("  flag1 ", "Flag1"),
    ] {
        let field = Field::parse(given).unwrap_or_else(|e| panic!("{given}: {e}"));
        assert_eq!(field.name(), canonical, "{given}");
    }
    for bad in [
        "Text31",
        "Text0",
        "Text01",
        "Number21",
        "Cost11",
        "Flag21",
        "Date11",
        "Start11",
        "Finish11",
        "Duration11",
    ] {
        assert_eq!(
            Field::parse(bad),
            Err(format!("unknown task field '{bad}'"))
        );
    }
    // The families append after "Unique ID", Text through Duration, and
    // `Field::all` agrees with the listing.
    let names = field_names();
    let tail: Vec<String> = [
        ("Text", 30),
        ("Number", 20),
        ("Cost", 10),
        ("Flag", 20),
        ("Date", 10),
        ("Start", 10),
        ("Finish", 10),
        ("Duration", 10),
    ]
    .iter()
    .flat_map(|&(prefix, max)| (1..=max).map(move |n| format!("{prefix}{n}")))
    .collect();
    let unique_id = names.iter().position(|n| n == "Unique ID").unwrap();
    assert_eq!(names[unique_id + 1..], tail[..]);
    assert_eq!(names.len(), 23 + 11 * 5 + 34 + 120);
    assert_eq!(Field::all().len(), names.len());
    for name in &names[unique_id + 1..] {
        assert_eq!(Field::parse(name).unwrap().name(), *name);
    }
}

#[test]
fn custom_text_reads_through_the_definition() {
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![custom_def("188743731", "Text1")];
    let mut t = task(1, "Pour", 480);
    t.extended_attributes = vec![custom_value("188743731", "M&E")];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    assert_eq!(tv(&ed, 1, "Text1"), (s("M&E"), FieldValue::Text(s("M&E"))));

    // The lookup follows the definition's FieldName, not a fixed id: renaming
    // the definition moves the value to Text2 and leaves Text1 unset.
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![custom_def("188743731", "Text2")];
    let mut t = task(1, "Pour", 480);
    t.extended_attributes = vec![custom_value("188743731", "M&E")];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    assert_eq!(tv(&ed, 1, "Text1"), (s(""), FieldValue::Null));
    assert_eq!(tv(&ed, 1, "Text2"), (s("M&E"), FieldValue::Text(s("M&E"))));
}

#[test]
fn custom_text_lookup_by_guid() {
    let list = node(
        "ValueList",
        vec![
            node(
                "Value",
                vec![
                    leaf("ID", "1"),
                    leaf("Value", "Civil"),
                    leaf("FieldGUID", "7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101"),
                ],
            ),
            node(
                "Value",
                vec![
                    leaf("ID", "2"),
                    leaf("Value", "M&E"),
                    leaf("FieldGUID", "7F2B5E61-7C21-4E0A-9B55-3C8D12A4E102"),
                ],
            ),
        ],
    );
    let mut def = custom_def("188743731", "Text1");
    def.children.push(list);
    // No value: the guid names the ValueList entry, case-insensitively; an
    // empty value falls back to the guid as well. A guid no entry matches,
    // or no guid at all, reads unset.
    let build = |value: Option<&str>, guid: Option<&str>| {
        let mut p = untitled_project();
        p.extended_attribute_definitions = vec![def.clone()];
        let mut t = task(1, "Pour", 480);
        t.extended_attributes = vec![ExtendedAttributeValue {
            field_id: "188743731".into(),
            value: value.map(String::from),
            value_guid: guid.map(String::from),
            duration_format: None,
        }];
        p.tasks = vec![t];
        Editor::new(p)
    };
    assert_eq!(
        tv(
            &build(None, Some("7f2b5e61-7c21-4e0a-9b55-3c8d12a4e102")),
            1,
            "Text1"
        ),
        (s("M&E"), FieldValue::Text(s("M&E")))
    );
    assert_eq!(
        tv(
            &build(Some(""), Some("7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101")),
            1,
            "Text1"
        ),
        (s("Civil"), FieldValue::Text(s("Civil")))
    );
    // A stored value wins over a guid pointing at a different entry.
    assert_eq!(
        tv(
            &build(Some("Civil"), Some("7F2B5E61-7C21-4E0A-9B55-3C8D12A4E102")),
            1,
            "Text1"
        ),
        (s("Civil"), FieldValue::Text(s("Civil")))
    );
    for guid in [Some("no-such-guid"), None] {
        assert_eq!(
            tv(&build(None, guid), 1, "Text1"),
            (s(""), FieldValue::Null),
            "{guid:?}"
        );
    }
}

#[test]
fn custom_field_reads_past_shadowing_definitions() {
    // The definitions block is shared with resource and assignment custom
    // fields, whose definitions repeat the task field names (resource Text1
    // is FieldID 205520904, corpus/mspdi/13-resource-fields.xml). One that
    // names no task value — or has no FieldID child at all — must not
    // shadow the task's definition.
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![
        custom_def("205520904", "Text1"),
        node("ExtendedAttribute", vec![leaf("FieldName", "Text1")]),
        custom_def("188743731", "Text1"),
    ];
    let mut t = task(1, "Pour", 480);
    t.extended_attributes = vec![custom_value("188743731", "M&E")];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    assert_eq!(tv(&ed, 1, "Text1"), (s("M&E"), FieldValue::Text(s("M&E"))));
}

#[test]
fn custom_number_cost_flag_date_duration() {
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![
        custom_def("555000001", "Number1"),
        custom_def("555000002", "Number2"),
        custom_def("555000003", "Cost1"),
        custom_def("555000004", "Flag1"),
        custom_def("555000005", "Flag2"),
        custom_def("555000006", "Date1"),
        custom_def("555000007", "Start1"),
        custom_def("555000008", "Finish1"),
        custom_def("555000009", "Duration1"),
        custom_def("555000010", "Duration2"),
    ];
    let mut t = task(1, "Pour", 480);
    t.extended_attributes = vec![
        custom_value("555000001", "12.50"),
        custom_value("555000002", "-0.25"),
        custom_value("555000003", "12550"),
        custom_value("555000004", "1"),
        custom_value("555000005", "FALSE"),
        custom_value("555000006", "2026-03-04T17:00:00"),
        custom_value("555000007", "2026-03-02T08:00:00"),
        custom_value("555000008", "2026-03-05T17:00:00"),
        ExtendedAttributeValue {
            field_id: "555000009".into(),
            value: Some("PT16H0M0S".into()),
            value_guid: None,
            duration_format: Some(5),
        },
        custom_value("555000010", "PT16H0M0S"),
    ];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    let date = |text: &str| FieldValue::Date(DateTime::parse_mspdi(text).unwrap());
    for (name, text, value) in [
        ("Number1", "12.5", FieldValue::Number(12.5)),
        ("Number2", "-0.25", FieldValue::Number(-0.25)),
        ("Cost1", "$125.50", FieldValue::Money(125.5)),
        ("Flag1", "Yes", FieldValue::Bool(true)),
        ("Flag2", "No", FieldValue::Bool(false)),
        ("Date1", "2026-03-04", date("2026-03-04T17:00:00")),
        ("Start1", "2026-03-02", date("2026-03-02T08:00:00")),
        ("Finish1", "2026-03-05", date("2026-03-05T17:00:00")),
        // The unit is the value's own DurationFormat (5 = hours).
        ("Duration1", "16 hrs", FieldValue::Minutes(960)),
        // Without one it is days.
        ("Duration2", "2 days", FieldValue::Minutes(960)),
    ] {
        assert_eq!(tv(&ed, 1, name), (s(text), value), "{name}");
    }
}

#[test]
fn custom_unset_and_unparseable_read_leniently() {
    // No definitions at all: every kind reads its unset shape.
    let ed = editor(vec![task(1, "Bare", 480)]);
    for (name, text, value) in [
        ("Text1", "", FieldValue::Null),
        ("Number1", "0", FieldValue::Null),
        ("Cost1", "$0.00", FieldValue::Null),
        ("Flag1", "No", FieldValue::Bool(false)),
        ("Date1", "NA", FieldValue::Null),
        ("Start1", "NA", FieldValue::Null),
        ("Finish1", "NA", FieldValue::Null),
        ("Duration1", "0 days", FieldValue::Null),
    ] {
        assert_eq!(tv(&ed, 1, name), (s(text), value), "{name}");
    }

    // A definition with no FieldName never matches, and a task value whose id
    // no definition names reads unset too.
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![node(
        "ExtendedAttribute",
        vec![leaf("FieldID", "555000099")],
    )];
    let mut t = task(1, "Bare", 480);
    t.extended_attributes = vec![
        custom_value("555000099", "M&E"),
        custom_value("555000777", "orphan"),
    ];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    assert_eq!(tv(&ed, 1, "Text1"), (s(""), FieldValue::Null));

    // A stored value that does not parse reads as its raw text and Null,
    // never an error.
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![
        custom_def("555000001", "Number1"),
        custom_def("555000002", "Cost1"),
        custom_def("555000003", "Flag1"),
        custom_def("555000004", "Date1"),
        custom_def("555000005", "Duration1"),
        custom_def("555000006", "Text1"),
    ];
    let mut t = task(1, "Bare", 480);
    t.extended_attributes = vec![
        custom_value("555000001", "abc"),
        custom_value("555000002", "12x"),
        custom_value("555000003", "maybe"),
        custom_value("555000004", "not-a-date"),
        custom_value("555000005", "XYZ"),
        custom_value("555000006", ""),
    ];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    for (name, raw) in [
        ("Number1", "abc"),
        ("Cost1", "12x"),
        ("Flag1", "maybe"),
        ("Date1", "not-a-date"),
        ("Duration1", "XYZ"),
        ("Text1", ""),
    ] {
        assert_eq!(tv(&ed, 1, name), (s(raw), FieldValue::Null), "{name}");
    }

    // `parse` accepts NaN and the inf spellings; they are not values and
    // keep their raw text. Finite numbers keep their exact digits — no
    // rounding to hundredths, no overflow to inf in the display text.
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![
        custom_def("555000001", "Number1"),
        custom_def("555000002", "Number2"),
        custom_def("555000003", "Number3"),
        custom_def("555000004", "Number4"),
        custom_def("555000005", "Cost1"),
        custom_def("555000006", "Cost2"),
        custom_def("555000007", "Cost3"),
        custom_def("555000008", "Cost4"),
    ];
    let mut t = task(1, "Bare", 480);
    t.extended_attributes = vec![
        custom_value("555000001", "NaN"),
        custom_value("555000002", "inf"),
        custom_value("555000003", "0.001"),
        custom_value("555000004", "1e308"),
        custom_value("555000005", "NaN"),
        custom_value("555000006", "inf"),
        // A finite but huge cost saturates the money text; a normal large
        // one still reads.
        custom_value("555000007", "1e40"),
        custom_value("555000008", "123456789012"),
    ];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    for (name, text, value) in [
        ("Number1", "NaN", FieldValue::Null),
        ("Number2", "inf", FieldValue::Null),
        ("Number3", "0.001", FieldValue::Number(0.001)),
        ("Number4", &1e308.to_string(), FieldValue::Number(1e308)),
        ("Cost1", "NaN", FieldValue::Null),
        ("Cost2", "inf", FieldValue::Null),
        ("Cost3", "1e40", FieldValue::Null),
        (
            "Cost4",
            "$1,234,567,890.12",
            FieldValue::Money(1234567890.12),
        ),
    ] {
        assert_eq!(tv(&ed, 1, name), (s(text), value), "{name}");
    }
}

#[test]
fn custom_field_first_value_wins_and_blank_row_is_empty() {
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![custom_def("188743731", "Text1")];
    let mut t = task(1, "Pour", 480);
    t.extended_attributes = vec![
        custom_value("188743731", "first"),
        custom_value("188743731", "second"),
    ];
    p.tasks = vec![t];
    let ed = Editor::new(p);
    assert_eq!(
        tv(&ed, 1, "Text1"),
        (s("first"), FieldValue::Text(s("first")))
    );

    // The blank-row guard covers custom fields: empty text and Null even
    // with a stored value and a matching definition.
    let mut p = untitled_project();
    p.extended_attribute_definitions = vec![custom_def("188743731", "Text1")];
    let mut blank = task(2, "", 0);
    blank.is_null = true;
    blank.extended_attributes = vec![custom_value("188743731", "M&E")];
    p.tasks = vec![blank];
    let ed = Editor::new(p);
    assert_eq!(tv(&ed, 2, "Text1"), (s(""), FieldValue::Null));
}
