use super::*;
use crate::mspdi::{read_mspdi, write_mspdi};

fn editor() -> Editor {
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![
            Task {
                uid: 1,
                id: 1,
                name: "S".into(),
                outline_level: 1,
                ..Task::default()
            },
            Task {
                uid: 2,
                id: 2,
                name: "A".into(),
                outline_level: 2,
                duration_min: 480,
                ..Task::default()
            },
        ],
        ..Project::default()
    })
}

fn round_trip(ed: &Editor) -> Project {
    read_mspdi(&write_mspdi(ed.project())).unwrap()
}

#[test]
fn percent_complete_range_unchanged_undo_and_round_trip() {
    let mut ed = editor();
    assert_eq!(ed.set_percent_complete(2, 0), Ok(false));
    assert!(!ed.dirty() && ed.undo_depth() == 0);
    assert!(ed.set_percent_complete(2, 101).is_err());
    assert!(ed.set_percent_complete(2, -1).is_err());
    assert!(
        ed.set_percent_complete(1, 10).is_err(),
        "a summary's rolls up"
    );
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_percent_complete(2, 40), Ok(true));
    assert_eq!(ed.set_percent_complete(2, 40), Ok(false));
    assert_eq!(ed.undo_depth(), 1);
    assert_eq!(round_trip(&ed).task(2).unwrap().percent_complete, Some(40));
    assert!(ed.undo());
    assert_eq!(ed.project().task(2).unwrap().percent_complete, None);
}

#[test]
fn percent_complete_cannot_contradict_the_actuals() {
    let day = DateTime::from_ymd_hm(2026, 1, 5, 8, 0);
    let with = |start, finish| {
        let mut proj = editor().project().clone();
        let task = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
        (task.actual_start, task.actual_finish) = (start, finish);
        Editor::new(proj)
    };
    let mut started = with(Some(day), None);
    assert!(
        started.set_percent_complete(2, 0).is_err(),
        "started, not 0%"
    );
    assert_eq!(started.set_percent_complete(2, 10), Ok(true));
    let mut finished = with(Some(day), Some(day));
    assert!(
        finished.set_percent_complete(2, 50).is_err(),
        "finished: 100%"
    );
    assert_eq!(finished.undo_depth(), 0);
    assert_eq!(finished.set_percent_complete(2, 100), Ok(true));
}

#[test]
fn priority_range_default_and_round_trip() {
    let mut ed = editor();
    assert_eq!(ed.set_priority(2, PRIORITY_DEFAULT), Ok(false));
    assert!(ed.set_priority(2, 1001).is_err());
    assert!(ed.set_priority(2, -1).is_err());
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_priority(2, 1000), Ok(true));
    assert_eq!(round_trip(&ed).task(2).unwrap().priority, Some(1000));
    assert_eq!(ed.set_priority(2, 0), Ok(true));
    assert_eq!(ed.undo_depth(), 2);
    assert!(ed.undo());
    assert_eq!(ed.project().task(2).unwrap().priority, Some(1000));
}

#[test]
fn deadline_sets_clears_and_moves_the_miss_indicator() {
    let mut ed = editor();
    let finish = ed.disp_finish(2).unwrap();
    let early = DateTime::from_ymd_hm(2026, 1, 5, 9, 0);
    assert_eq!(ed.set_deadline(2, None), Ok(false));
    assert_eq!(ed.set_deadline(2, Some(early)), Ok(true));
    let task = ed.project().task(2).unwrap();
    assert!(
        task.misses_deadline(finish),
        "the deadline reaches the model"
    );
    assert_eq!(round_trip(&ed).task(2).unwrap().deadline, Some(early));
    assert_eq!(ed.set_deadline(2, None), Ok(true));
    assert_eq!(ed.project().task(2).unwrap().deadline, None);
    assert_eq!(round_trip(&ed).task(2).unwrap().deadline, None);
    assert!(ed.undo());
    assert_eq!(ed.project().task(2).unwrap().deadline, Some(early));
}

#[test]
fn task_type_and_effort_driven_round_trip_and_undo() {
    let mut ed = editor();
    assert_eq!(ed.set_task_type(2, TaskType::FixedUnits), Ok(false));
    assert_eq!(ed.set_effort_driven(2, false), Ok(false));
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_task_type(2, TaskType::FixedWork), Ok(true));
    assert_eq!(ed.set_effort_driven(2, true), Ok(true));
    let back = round_trip(&ed);
    assert_eq!(back.task(2).unwrap().task_type, Some(TaskType::FixedWork));
    assert_eq!(back.task(2).unwrap().effort_driven, Some(true));
    assert_eq!(ed.undo_depth(), 2);
    assert!(ed.undo() && ed.undo());
    let task = ed.project().task(2).unwrap();
    assert_eq!((task.task_type, task.effort_driven), (None, None));
}

#[test]
fn bar_flags_set_undo_and_ignore_an_unchanged_value() {
    let mut ed = editor();
    assert_eq!(ed.set_hide_bar(2, false), Ok(false));
    assert_eq!(ed.set_rollup(2, false), Ok(false));
    assert_eq!(ed.set_display_on_timeline(2, false), Ok(false));
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_hide_bar(2, true), Ok(true));
    assert_eq!(ed.set_rollup(2, true), Ok(true));
    assert_eq!(ed.set_display_on_timeline(2, true), Ok(true));
    assert_eq!(ed.set_hide_bar(2, true), Ok(false));
    assert_eq!(ed.undo_depth(), 3);
    let task = ed.project().task(2).unwrap();
    assert_eq!(
        (task.hide_bar, task.rollup, task.display_on_timeline),
        (Some(true), Some(true), Some(true))
    );
    let back = round_trip(&ed);
    assert_eq!(back.task(2).unwrap().hide_bar, Some(true));
    assert_eq!(back.task(2).unwrap().rollup, Some(true));
    assert!(ed.undo() && ed.undo() && ed.undo());
    let task = ed.project().task(2).unwrap();
    assert_eq!(
        (task.hide_bar, task.rollup, task.display_on_timeline),
        (None, None, None)
    );
    assert!(ed.set_hide_bar(99, true).is_err());
}

#[test]
fn a_batch_of_property_edits_is_one_undo_step_and_an_error_applies_none() {
    let mut ed = editor();
    ed.batch(|ed| {
        ed.set_percent_complete(2, 25)?;
        ed.set_priority(2, 900)?;
        ed.set_task_type(2, TaskType::FixedDuration)?;
        ed.set_effort_driven(2, true)?;
        ed.set_deadline(2, Some(DateTime::from_ymd_hm(2026, 3, 2, 17, 0)))?;
        Ok(())
    })
    .unwrap();
    assert_eq!(ed.undo_depth(), 1);
    assert!(ed.undo());
    assert_eq!(ed.project().task(2), editor().project().task(2));
    assert!(!ed.undo());

    let mut ed = editor();
    let before = ed.project().clone();
    let result = ed.batch(|ed| {
        ed.set_priority(2, 900)?;
        ed.set_percent_complete(2, 500)
    });
    assert!(result.is_err());
    assert_eq!(*ed.project(), before);
    assert_eq!((ed.undo_depth(), ed.dirty()), (0, false));
}

#[test]
fn a_blank_row_takes_no_properties() {
    let mut ed = editor();
    ed.append_row(|ed, uid| {
        let err = ed.set_priority(uid, 10).unwrap_err();
        assert!(err.contains("blank"));
        Ok(())
    })
    .unwrap();
}

/// The Advanced page's other fields (#1550): milestone, calendar, ignore
/// resource calendar, WBS and earned value method, each one undo step,
/// each a no-op on its own value, each refusing what Project refuses.
#[test]
fn advanced_fields_set_once_and_refuse_bad_values() {
    // A new plan: it has the Standard calendar the task calendar can name.
    let fresh = || {
        let mut ed = Editor::new(crate::editor::untitled_project());
        ed.add_task(None, "A", 480, false).unwrap();
        ed
    };
    let mut ed = fresh();
    let uid = ed.project().tasks[0].uid;
    let depth0 = ed.undo_depth();
    assert!(ed.set_milestone(uid, true).unwrap());
    assert!(ed.project().task(uid).unwrap().is_milestone());
    assert!(!ed.set_milestone(uid, true).unwrap());
    assert!(ed.set_milestone(uid, false).unwrap());
    assert!(!ed.project().task(uid).unwrap().is_milestone());

    let base = ed.project().calendars.first().map(|c| c.uid).unwrap();
    assert!(ed.set_task_calendar(uid, Some(base)).unwrap());
    assert_eq!(ed.project().task(uid).unwrap().calendar_uid, Some(base));
    assert!(!ed.set_task_calendar(uid, Some(base)).unwrap());
    assert!(ed.set_task_calendar(uid, Some(999)).is_err());
    assert!(ed.set_ignore_resource_calendar(uid, true).unwrap());
    assert_eq!(
        ed.project().task(uid).unwrap().ignore_resource_calendar,
        Some(true)
    );
    assert!(ed.set_task_calendar(uid, None).unwrap());
    // Without a task calendar the flag cannot be turned on.
    ed.set_ignore_resource_calendar(uid, false).unwrap();
    assert!(ed.set_ignore_resource_calendar(uid, true).is_err());

    assert!(ed.set_wbs(uid, " 1.2 ").unwrap());
    assert_eq!(ed.project().task(uid).unwrap().wbs.as_deref(), Some("1.2"));
    assert!(!ed.set_wbs(uid, "1.2").unwrap());
    assert!(ed.set_wbs(uid, "").unwrap());
    assert_eq!(ed.project().task(uid).unwrap().wbs, None);

    assert!(
        ed.set_earned_value_method(uid, EV_PHYSICAL_PERCENT_COMPLETE)
            .unwrap()
    );
    assert!(!ed.set_earned_value_method(uid, 1).unwrap());
    assert!(ed.set_earned_value_method(uid, 2).is_err());
    // Every change above was one step; undoing them all restores the plan.
    let steps = ed.undo_depth() - depth0;
    for _ in 0..steps {
        assert!(ed.undo());
    }
    assert_eq!(ed.project().task(uid), fresh().project().task(uid));
}

#[test]
fn custom_fields_set_from_text_by_the_plans_definitions() {
    use crate::editor::fields::{CustomKind, Field, FieldReader};
    let plan = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <StartDate>2026-03-02T08:00:00</StartDate>
  <ExtendedAttributes>
    <ExtendedAttribute><FieldID>205520904</FieldID><FieldName>Text1</FieldName><Alias>Crew</Alias></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743731</FieldID><FieldName>Text1</FieldName><Alias>Trade</Alias>
      <ValueList>
        <Value><ID>1</ID><Value>Civil</Value><FieldGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101</FieldGUID></Value>
        <Value><ID>2</ID><Value>M&amp;E</Value><FieldGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E102</FieldGUID></Value>
      </ValueList></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743732</FieldID><FieldName>Text2</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743767</FieldID><FieldName>Number1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743752</FieldID><FieldName>Flag1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743737</FieldID><FieldName>Date1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743783</FieldID><FieldName>Duration1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743786</FieldID><FieldName>Cost1</FieldName></ExtendedAttribute>
    <ExtendedAttribute><FieldID>188743784</FieldID><FieldName>Duration2</FieldName><Formula>[Duration]*2</Formula></ExtendedAttribute>
  </ExtendedAttributes>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>Dig</Name><OutlineLevel>1</OutlineLevel><Duration>PT8H0M0S</Duration>
      <ExtendedAttribute><FieldID>188743731</FieldID><Value>Civil</Value><ValueGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101</ValueGUID></ExtendedAttribute>
    </Task>
  </Tasks>
</Project>"#;
    let mut ed = Editor::new(crate::mspdi::read_mspdi(plan).unwrap());
    let read = |ed: &Editor, name: &str| {
        let task = ed.project().task(1).unwrap();
        FieldReader::new(ed)
            .read(task, Field::parse(name).unwrap())
            .text
    };
    let field = |name: &str| Field::parse(name).unwrap();
    let depth0 = ed.undo_depth();
    // The same lookup value again is no change; another entry (any case)
    // stores its text and guid; a text outside the table is refused.
    assert_eq!(ed.set_custom_field(1, field("Text1"), "Civil"), Ok(false));
    assert_eq!(ed.set_custom_field(1, field("Text1"), "m&e"), Ok(true));
    assert_eq!(read(&ed, "Text1"), "M&E");
    let stored = &ed.project().task(1).unwrap().extended_attributes[0];
    assert_eq!(
        stored.value_guid.as_deref(),
        Some("7F2B5E61-7C21-4E0A-9B55-3C8D12A4E102")
    );
    assert!(
        ed.set_custom_field(1, field("Text1"), "Steel")
            .unwrap_err()
            .contains("Civil, M&E")
    );
    // Free text, a number, a flag, a date, a duration, a cost.
    assert_eq!(
        ed.set_custom_field(1, field("Text2"), "  north wing "),
        Ok(true)
    );
    assert_eq!(read(&ed, "Text2"), "north wing");
    assert_eq!(
        ed.set_custom_field(1, field("Number1"), "1,250.5"),
        Ok(true)
    );
    assert_eq!(read(&ed, "Number1"), "1250.5");
    assert_eq!(ed.set_custom_field(1, field("Flag1"), "Yes"), Ok(true));
    assert_eq!(read(&ed, "Flag1"), "Yes");
    assert_eq!(ed.set_custom_field(1, field("Flag1"), "yes"), Ok(false));
    assert_eq!(
        ed.set_custom_field(1, field("Date1"), "2026-03-09"),
        Ok(true)
    );
    assert_eq!(
        ed.project()
            .task(1)
            .unwrap()
            .extended_attributes
            .last()
            .unwrap()
            .value
            .as_deref(),
        Some("2026-03-09T08:00:00")
    );
    assert_eq!(
        ed.set_custom_field(1, field("Duration1"), "2.5 wks"),
        Ok(true)
    );
    let stored = ed
        .project()
        .task(1)
        .unwrap()
        .extended_attributes
        .last()
        .unwrap();
    assert_eq!(stored.value.as_deref(), Some("PT100H0M0S"));
    assert_eq!(read(&ed, "Duration1"), "2.5 wks");
    assert_eq!(
        ed.set_custom_field(1, field("Cost1"), "$1,400.50"),
        Ok(true)
    );
    assert_eq!(
        ed.project()
            .task(1)
            .unwrap()
            .extended_attributes
            .last()
            .unwrap()
            .value
            .as_deref(),
        Some("140050")
    );
    assert_eq!(read(&ed, "Cost1"), "$1,400.50");
    assert_eq!(ed.undo_depth() - depth0, 7);
    // Clearing removes the value; clearing an unset field is no change.
    assert_eq!(ed.set_custom_field(1, field("Number1"), ""), Ok(true));
    assert_eq!(read(&ed, "Number1"), "0");
    assert_eq!(ed.set_custom_field(1, field("Number1"), " "), Ok(false));
    // Refusals: a bad value, a formula field, an undefined field, a built-in
    // field, an unknown task.
    assert!(ed.set_custom_field(1, field("Number1"), "many").is_err());
    assert!(ed.set_custom_field(1, field("Flag1"), "maybe").is_err());
    assert!(ed.set_custom_field(1, field("Date1"), "soon").is_err());
    assert!(
        ed.set_custom_field(1, field("Duration1"), "3 parsecs")
            .is_err()
    );
    assert!(ed.set_custom_field(1, field("Cost1"), "a lot").is_err());
    assert_eq!(
        ed.set_custom_field(1, field("Duration2"), "3d")
            .unwrap_err(),
        "Duration2 is computed by a formula"
    );
    assert!(
        ed.set_custom_field(1, field("Text3"), "x")
            .unwrap_err()
            .contains("no definition")
    );
    assert!(ed.set_custom_field(1, field("Name"), "x").is_err());
    assert!(ed.set_custom_field(9, field("Text2"), "x").is_err());
    assert_eq!(
        ed.set_custom_field(1, Field::Custom(CustomKind::Text, 2), "x"),
        Ok(true)
    );
    // Undo takes the last write back.
    assert!(ed.undo());
    assert_eq!(read(&ed, "Text2"), "north wing");
}
