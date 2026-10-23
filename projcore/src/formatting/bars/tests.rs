use super::*;
use crate::DateTime;
use crate::editor::{Editor, Field, FieldReader};
use crate::model::{Project, Task};

fn row(name: &str, show_for: &[&str], row: u8) -> BarRow {
    BarRow {
        name: name.into(),
        show_for: show_for.iter().map(|s| s.to_string()).collect(),
        row,
        ..BarRow::default()
    }
}

fn facts(f: impl FnOnce(&mut Facts)) -> Facts {
    let mut facts = Facts {
        active: true,
        ..Facts::default()
    };
    f(&mut facts);
    facts
}

fn sample() -> Formatting {
    let mut f = Formatting::default();
    f.set_bars(vec![
        BarRow {
            name: "Crit <\"&> task".into(),
            show_for: vec!["Critical".into(), "Not Summary".into()],
            row: 2,
            from: "Task Start".into(),
            to: "Baseline1 Finish".into(),
            start: Mark {
                shape: Some("diamond".into()),
                kind: Some("framed".into()),
                color: Some(0xFF0000),
            },
            middle: Middle {
                shape: Some("narrow-bottom".into()),
                pattern: Some("light-fill".into()),
                color: Some(0x1F3763),
            },
            end: Mark {
                shape: Some("arrow-down".into()),
                kind: Some("dashed".into()),
                color: None,
            },
            text: BarText {
                right: Some("Resource Names".into()),
                inside: Some("% Complete".into()),
                ..BarText::default()
            },
        },
        row("Plain", &[], 1),
    ]);
    f.set_task_bar(TaskBar {
        uid: 7,
        middle: Middle {
            color: Some(0x00AA00),
            ..Middle::default()
        },
        text: BarText {
            top: Some("Name & <more>".into()),
            ..BarText::default()
        },
        ..TaskBar::default()
    });
    f
}

#[test]
fn json_and_xml_round_trip_losslessly() {
    let f = sample();
    let xml = f.to_xml();
    assert!(
        xml.contains(r##"<bar name="Crit &lt;&quot;&amp;&gt; task" show-for="Critical,Not Summary" row="2" from="Task Start" to="Baseline1 Finish" start-shape="diamond" start-type="framed" start-color="#FF0000" middle-shape="narrow-bottom" middle-pattern="light-fill" middle-color="#1F3763" end-shape="arrow-down" end-type="dashed" text-right="Resource Names" text-inside="% Complete"/>"##),
        "{xml}"
    );
    assert!(
        xml.contains(
            r##"<task-bar uid="7" middle-color="#00AA00" text-top="Name &amp; &lt;more&gt;"/>"##
        ),
        "{xml}"
    );
    assert_eq!(Formatting::from_xml(&xml).unwrap(), f);

    // The view JSON is what apply_view reads back.
    let view = Json::obj(vec![
        ("bars", f.bars_json()),
        ("task_bars", f.task_bars_json()),
    ]);
    let mut g = Formatting::default();
    assert!(g.apply_view(&view).unwrap());
    assert_eq!(g, f);
    // Through the text form too.
    let reparsed = Json::parse(&view.to_string()).unwrap();
    let mut h = Formatting::default();
    h.apply_view(&reparsed).unwrap();
    assert_eq!(h, f);
}

#[test]
fn an_empty_model_saves_as_before() {
    let f = Formatting::default();
    assert_eq!(
        f.to_xml(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<formatting>\n</formatting>\n"
    );
    assert!(f.is_empty());
    assert!(!sample().is_empty());
    let mut only_tasks = Formatting::default();
    only_tasks.set_task_bar(TaskBar {
        uid: 1,
        end: Mark {
            color: Some(1),
            ..Mark::default()
        },
        ..TaskBar::default()
    });
    assert!(!only_tasks.is_empty());
    // A file written before this model loads unchanged.
    let old = r#"<formatting palette="p"><text category="All" bold="1"/></formatting>"#;
    let got = Formatting::from_xml(old).unwrap();
    assert!(got.bars.is_empty() && got.task_bars.is_empty());
    // Unknown attributes and elements beside the bars are ignored.
    let later = r#"<formatting><bar name="A" row="3" glow="1"><x/></bar><task-bar/><task-bar uid="x"/></formatting>"#;
    let got = Formatting::from_xml(later).unwrap();
    assert_eq!(got.bars.len(), 1);
    assert_eq!(got.bars[0].row, 3);
    assert!(got.task_bars.is_empty());
}

#[test]
fn show_for_evaluates_categories() {
    let crit = facts(|f| f.critical = true);
    let plain = facts(|_| {});
    let summary = facts(|f| f.summary = true);
    let milestone = facts(|f| f.milestone = true);
    // An empty list shows for every task.
    assert!(row("a", &[], 1).applies(&plain));
    assert!(row("a", &[], 1).applies(&summary));
    // Normal is neither summary nor milestone.
    let normal = row("a", &["Normal"], 1);
    assert!(normal.applies(&plain) && !normal.applies(&summary) && !normal.applies(&milestone));
    // Critical / Noncritical.
    assert!(row("a", &["Critical"], 1).applies(&crit));
    assert!(!row("a", &["Critical"], 1).applies(&plain));
    assert!(row("a", &["noncritical"], 1).applies(&plain));
    assert!(!row("a", &["Noncritical"], 1).applies(&crit));
    // Every listed category must match (Project's list is a conjunction);
    // Not removes.
    let r = row("a", &["Milestone", "Critical"], 1);
    let crit_milestone = facts(|f| {
        f.critical = true;
        f.milestone = true;
    });
    assert!(r.applies(&crit_milestone));
    assert!(!r.applies(&crit) && !r.applies(&milestone) && !r.applies(&plain));
    // Project's default Task row: Normal, Active, Not Manually Scheduled.
    let task_row = row("Task", &["Normal", "Active", "Not Manual"], 1);
    assert!(task_row.applies(&plain));
    assert!(!task_row.applies(&summary));
    assert!(!task_row.applies(&facts(|f| f.manual = true)));
    assert!(!task_row.applies(&Facts::default()));
    let r = row("a", &["Critical", "Not Summary"], 1);
    assert!(r.applies(&crit));
    assert!(!r.applies(&facts(|f| {
        f.critical = true;
        f.summary = true;
    })));
    assert!(!r.applies(&plain));
    // Only negated entries: every task but those.
    let r = row("a", &["Not Summary"], 1);
    assert!(r.applies(&plain) && !r.applies(&summary));
    assert!(row("a", &["  not   critical "], 1).applies(&plain));
    // Active / Inactive / Manual / Auto / External / Late.
    let inactive = Facts::default();
    assert!(row("a", &["Inactive"], 1).applies(&inactive));
    assert!(!row("a", &["Active"], 1).applies(&inactive));
    let manual = facts(|f| f.manual = true);
    assert!(row("a", &["Manual"], 1).applies(&manual));
    assert!(row("a", &["Auto"], 1).applies(&plain));
    assert!(row("a", &["External"], 1).applies(&facts(|f| f.external = true)));
    assert!(!row("a", &["External"], 1).applies(&plain));
    assert!(row("a", &["Late"], 1).applies(&facts(|f| f.late = true)));
    // Flag1..20, bit n-1.
    let flagged = facts(|f| f.flags = 1 | 1 << 19);
    assert!(row("a", &["Flag1"], 1).applies(&flagged));
    assert!(row("a", &["flag20"], 1).applies(&flagged));
    assert!(!row("a", &["Flag2"], 1).applies(&flagged));
    assert!(!row("a", &["Flag21"], 1).applies(&flagged));
    assert!(!row("a", &["Flag0"], 1).applies(&flagged));
    assert!(row("a", &["Not Flag2"], 1).applies(&flagged));
    // Categories the plan holds no data for never match.
    for unknown in ["Marked", "Driving", "Nonsense", ""] {
        assert!(
            !row("a", &[unknown, "Critical"], 1).applies(&plain),
            "{unknown}"
        );
        assert!(!row("a", &[unknown, "Critical"], 1).applies(&facts(|_| {})));
    }
    assert!(!row("a", &["Marked"], 1).applies(&crit));
    // bars_for keeps table order.
    let mut f = Formatting::default();
    f.set_bars(vec![
        row("first", &["Critical"], 2),
        row("second", &[], 1),
        row("third", &["Summary"], 3),
    ]);
    let names = |facts: &Facts| -> Vec<String> {
        f.bars_for(facts).iter().map(|r| r.name.clone()).collect()
    };
    assert_eq!(names(&crit), ["first", "second"]);
    assert_eq!(names(&summary), ["second", "third"]);
    assert_eq!(f.max_bar_row(), 3);
    assert_eq!(Formatting::default().max_bar_row(), 0);
    assert!(!f.bars_use_flags());
    f.bars[1].show_for = vec!["Not Flag3".into()];
    assert!(f.bars_use_flags());
}

#[test]
fn apply_view_keeps_replaces_clears_and_clamps() {
    let mut f = sample();
    let before = f.clone();
    // No bars key (or any other key) keeps the table and the overrides.
    assert!(
        !f.apply_view(&Json::parse(r#"{"switches":{}}"#).unwrap())
            .unwrap()
    );
    assert_eq!(f, before);
    // The row clamps to 1..=4.
    let view =
        Json::parse(r#"{"bars":[{"name":"z","row":0},{"name":"n","row":9},{"name":"f","row":2.0},{"name":"s","row":"3"}]}"#)
            .unwrap();
    f.apply_view(&view).unwrap();
    let rows: Vec<u8> = f.bars.iter().map(|r| r.row).collect();
    assert_eq!(rows, [1, 4, 2, 1]);
    // The overrides stayed.
    assert_eq!(f.task_bars, before.task_bars);
    // null clears the table only.
    assert!(
        f.apply_view(&Json::parse(r#"{"bars":null}"#).unwrap())
            .unwrap()
    );
    assert!(f.bars.is_empty());
    assert_eq!(f.task_bars.len(), 1);
    // null clears every override.
    f.apply_view(&Json::parse(r#"{"task_bars":null}"#).unwrap())
        .unwrap();
    assert!(f.task_bars.is_empty() && f.is_empty());
    // A show_for comma string splits.
    f.apply_view(
        &Json::parse(r#"{"bars":[{"name":"a","show_for":"Critical, Not Summary"}]}"#).unwrap(),
    )
    .unwrap();
    assert_eq!(f.bars[0].show_for, ["Critical", "Not Summary"]);
    // Wrong types inside a part are ignored per field.
    f.apply_view(
        &Json::parse(r##"{"bars":[{"name":5,"start":{"shape":3,"color":"#00FF00"},"text":{"left":1,"right":"Name"}}]}"##)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(f.bars[0].name, "");
    assert_eq!(f.bars[0].start.shape, None);
    assert_eq!(f.bars[0].start.color, Some(0x00FF00));
    assert_eq!(f.bars[0].text.left, None);
    assert_eq!(f.bars[0].text.right.as_deref(), Some("Name"));
    // A word outside the vocabulary is kept as typed.
    f.apply_view(&Json::parse(r#"{"bars":[{"name":"a","middle":{"shape":"zig"}}]}"#).unwrap())
        .unwrap();
    assert_eq!(f.bars[0].middle.shape.as_deref(), Some("zig"));
    // Errors name the entry.
    let e = f.apply_view(&Json::parse(r#"{"bars":[{"name":"a"},3]}"#).unwrap());
    assert_eq!(e.unwrap_err(), "view.bars[1] is not an object");
    let e = f.apply_view(&Json::parse(r#"{"task_bars":[{"start":{}}]}"#).unwrap());
    assert_eq!(e.unwrap_err(), "view.task_bars[0] has no uid");
    assert!(
        f.apply_view(&Json::parse(r#"{"bars":4}"#).unwrap())
            .is_err()
    );
    assert!(
        f.apply_view(&Json::parse(r#"{"task_bars":"x"}"#).unwrap())
            .is_err()
    );
}

#[test]
fn reset_removes_the_override() {
    let mut f = sample();
    // An entry with no parts removes it.
    assert!(
        f.apply_view(&Json::parse(r#"{"task_bars":[{"uid":7}]}"#).unwrap())
            .unwrap()
    );
    assert!(f.task_bar(7).is_none());
    // So does "reset": true, whatever else it carries.
    let mut f = sample();
    f.apply_view(
        &Json::parse(r##"{"task_bars":[{"uid":7,"reset":true,"start":{"color":"#FFFFFF"}}]}"##)
            .unwrap(),
    )
    .unwrap();
    assert!(f.task_bar(7).is_none());
    // Resetting a task with no override changes nothing.
    assert!(
        !f.apply_view(&Json::parse(r#"{"task_bars":[{"uid":99,"reset":true}]}"#).unwrap())
            .unwrap()
    );
    // A new entry replaces the task's whole override.
    f.apply_view(&Json::parse(r##"{"task_bars":[{"uid":7,"end":{"shape":"circle"}},{"uid":7,"start":{"shape":"square"}}]}"##).unwrap())
        .unwrap();
    let bar = f.task_bar(7).unwrap();
    assert_eq!(bar.start.shape.as_deref(), Some("square"));
    assert!(bar.end.is_empty());
    assert_eq!(f.task_bars.len(), 1);
}

#[test]
fn overrides_merge_over_a_row_per_part() {
    let row_mark = Mark {
        shape: Some("square".into()),
        kind: Some("solid".into()),
        color: Some(0x111111),
    };
    let over = Mark {
        color: Some(0x222222),
        ..Mark::default()
    };
    let m = row_mark.over(&over);
    assert_eq!(
        (m.shape.as_deref(), m.kind.as_deref(), m.color),
        (Some("square"), Some("solid"), Some(0x222222))
    );
    let t = BarText {
        left: Some("Name".into()),
        right: Some("Start".into()),
        ..BarText::default()
    }
    .over(&BarText {
        right: Some("Finish".into()),
        ..BarText::default()
    });
    assert_eq!(
        (t.left.as_deref(), t.right.as_deref()),
        (Some("Name"), Some("Finish"))
    );
    assert!(t.names("name") && !t.names("Cost"));
}

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 1, day, hour, 0)
}

fn editor() -> Editor {
    editor_with(|_| {})
}

/// Task 1 (two days) is touched by `touch`; task 2 is a milestone.
fn editor_with(touch: impl FnOnce(&mut Task)) -> Editor {
    let task = |uid: i32, days: i64| Task {
        uid,
        id: uid,
        name: format!("Task {uid}"),
        outline_level: 1,
        duration_min: days * 480,
        ..Task::default()
    };
    let mut first = task(1, 2);
    touch(&mut first);
    Editor::new(Project {
        start_date: Some(at(5, 8)),
        tasks: vec![first, task(2, 0), task(3, 4)],
        ..Project::default()
    })
}

#[test]
fn an_editor_reads_facts_dates_and_text() {
    let ed = editor();
    let reader = FieldReader::new(&ed);
    let tasks = ed.project().tasks.clone();
    let f1 = ed.bar_facts(&reader, &tasks[0], false);
    assert!(f1.active && !f1.milestone && !f1.summary && !f1.manual && !f1.external);
    assert!(ed.bar_facts(&reader, &tasks[1], false).milestone);
    // Start / Finish and the aliases; an empty name is the default field.
    let start = ed.bar_date(&reader, &tasks[0], "", Field::Start);
    assert_eq!(start, Some(at(5, 8)));
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "Task Start", Field::Finish),
        start
    );
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], " start ", Field::Finish),
        start
    );
    let finish = ed
        .bar_date(&reader, &tasks[0], "Task Finish", Field::Start)
        .unwrap();
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "", Field::Finish),
        Some(finish)
    );
    assert!(finish > start.unwrap());
    // A date field that is not set, or not a field, draws nothing.
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "Actual Start", Field::Start),
        None
    );
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "Deadline", Field::Start),
        None
    );
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "Baseline Start", Field::Start),
        None
    );
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "Start1", Field::Start),
        None
    );
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "No Such Field", Field::Start),
        None
    );
    // A name column is not a date.
    assert_eq!(ed.bar_date(&reader, &tasks[0], "Name", Field::Start), None);
    // CompleteThrough: start with no progress, then the percent of the span.
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "CompleteThrough", Field::Start),
        start
    );
    assert_eq!(
        ed.bar_date(&reader, &tasks[0], "Complete Through", Field::Start),
        start
    );
    assert_eq!(ed.bar_text(&reader, &tasks[0], "Name"), "Task 1");
    assert_eq!(ed.bar_text(&reader, &tasks[0], "Nope"), "");
    let ed = editor_with(|t| t.percent_complete = Some(50));
    let reader = FieldReader::new(&ed);
    let task = ed.project().tasks[0].clone();
    let half = ed
        .bar_date(&reader, &task, "CompleteThrough", Field::Start)
        .unwrap();
    assert!(half > start.unwrap() && half < finish);
    let ed = editor_with(|t| t.stop = Some(at(6, 12)));
    let reader = FieldReader::new(&ed);
    let task = ed.project().tasks[0].clone();
    assert_eq!(
        ed.bar_date(&reader, &task, "CompleteThrough", Field::Start),
        Some(at(6, 12))
    );
}

#[test]
fn bars_and_overrides_in_one_reply_are_one_undo_step() {
    let mut ed = editor();
    let depth = ed.undo_depth();
    let view = Json::parse(
        r#"{"bars":[{"name":"a","row":2}],"task_bars":[{"uid":1,"middle":{"shape":"full"}}]}"#,
    )
    .unwrap();
    ed.edit_formatting(|f| {
        f.apply_view(&view).unwrap();
    });
    assert_eq!(ed.undo_depth(), depth + 1);
    assert_eq!(ed.project().formatting.bars.len(), 1);
    assert!(ed.project().formatting.task_bar(1).is_some());
    // The same reply again changes nothing: no step.
    ed.edit_formatting(|f| {
        assert!(!f.apply_view(&view).unwrap());
    });
    assert_eq!(ed.undo_depth(), depth + 1);
    // Reset is a step; one Undo brings the override back, another the table.
    let reset = Json::parse(r#"{"task_bars":[{"uid":1,"reset":true}]}"#).unwrap();
    ed.edit_formatting(|f| {
        f.apply_view(&reset).unwrap();
    });
    assert_eq!(ed.undo_depth(), depth + 2);
    assert!(ed.project().formatting.task_bar(1).is_none());
    assert!(ed.undo());
    assert!(ed.project().formatting.task_bar(1).is_some());
    assert!(ed.undo());
    assert!(ed.project().formatting.is_empty());
    assert!(ed.redo());
    assert_eq!(ed.project().formatting.bars.len(), 1);
}

#[test]
fn set_bars_clamps_the_row() {
    let mut f = Formatting::default();
    f.set_bars(vec![row("a", &[], 0), row("b", &[], 9)]);
    assert_eq!(f.bars.iter().map(|r| r.row).collect::<Vec<_>>(), [1, 4]);
    assert_eq!(f.max_bar_row(), 4);
}

#[test]
fn a_comma_list_inside_a_json_entry_splits_like_the_xml_form() {
    let mut f = Formatting::default();
    f.apply_view(
        &Json::parse(r#"{"bars":[{"name":"a","show_for":["Normal, Active","Not Manual"]}]}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(f.bars[0].show_for, ["Normal", "Active", "Not Manual"]);
    let xml = f.to_xml();
    assert!(
        xml.contains(r#"show-for="Normal,Active,Not Manual""#),
        "{xml}"
    );
    assert_eq!(Formatting::from_xml(&xml).unwrap(), f);
    let mut g = Formatting::default();
    g.apply_view(&Json::obj(vec![("bars", f.bars_json())]))
        .unwrap();
    assert_eq!(g, f);
}

#[test]
fn date_names_land_on_the_day_scale_by_kind() {
    use crate::editor::BarDateKind::{Finish, Instant, Start, Through};
    let kind = |name: &str, default| crate::editor::BarDateKind::of(name, default);
    assert_eq!(kind("", Field::Start), Start);
    assert_eq!(kind("", Field::Finish), Finish);
    for name in [
        "Start",
        "Task Start",
        " actual start ",
        "Baseline Start",
        "Baseline3 Start",
        "Start4",
        "Early Start",
    ] {
        assert_eq!(kind(name, Field::Finish), Start, "{name}");
    }
    for name in [
        "Finish",
        "Task Finish",
        "Actual Finish",
        "Baseline Finish",
        "Finish10",
        "Late Finish",
    ] {
        assert_eq!(kind(name, Field::Start), Finish, "{name}");
    }
    for name in ["CompleteThrough", "Complete Through"] {
        assert_eq!(kind(name, Field::Start), Through, "{name}");
    }
    for name in ["Deadline", "Date1", "Nonsense"] {
        assert_eq!(kind(name, Field::Start), Instant, "{name}");
    }
}

#[test]
fn bar_through_is_the_progress_fraction_of_the_bar() {
    let through = |touch: fn(&mut Task)| {
        let ed = editor_with(touch);
        let reader = FieldReader::new(&ed);
        ed.bar_through(&reader, &ed.project().tasks[0])
    };
    assert_eq!(through(|_| {}), Some(0.));
    assert_eq!(through(|t| t.percent_complete = Some(50)), Some(0.5));
    assert_eq!(through(|t| t.percent_complete = Some(100)), Some(1.));
    assert_eq!(through(|t| t.percent_complete = Some(250)), Some(1.));
    // A stop wins over the percent: the start is Monday 8:00, the two-day
    // task ends Tuesday 17:00 (18 working hours over 33 clock hours).
    let stop = through(|t| {
        t.percent_complete = Some(10);
        t.stop = Some(at(5, 8).add_minutes(33 * 60 / 2));
    })
    .unwrap();
    assert!((stop - 0.5).abs() < 1e-9, "{stop}");
    assert_eq!(through(|t| t.stop = Some(at(1, 8))), Some(0.));
}
