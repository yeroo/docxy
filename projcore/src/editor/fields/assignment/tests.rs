use super::*;
use crate::editor::ResourceRef;

/// Task 1 (5 days) with no assignments; Ann (1) at $50/h, Steel (2) a
/// material labelled `tons`, Fee (3) a cost resource.
fn editor() -> Editor {
    let resource = |uid, name: &str, kind| Resource {
        uid,
        id: uid,
        name: name.into(),
        kind,
        max_units: 1.0,
        ..Resource::default()
    };
    let mut ann = resource(1, "Ann", ResourceType::Work);
    ann.standard_rate = Rate::parse("50");
    let mut steel = resource(2, "Steel", ResourceType::Material);
    steel.material_label = Some("tons".into());
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![Task {
            uid: 1,
            id: 1,
            name: "Build".into(),
            outline_level: 1,
            duration_min: 5 * 480,
            ..Task::default()
        }],
        resources: vec![ann, steel, resource(3, "Fee", ResourceType::Cost)],
        ..Project::default()
    })
}

fn read(ed: &Editor, uid: i32, name: &str) -> (String, FieldValue) {
    let a = ed
        .project()
        .assignments
        .iter()
        .find(|a| a.uid == uid)
        .unwrap();
    let f = read_assignment_field(ed, a, AssignmentField::parse(name).unwrap());
    (f.text, f.value)
}

fn shows(text: &str, value: FieldValue) -> (String, FieldValue) {
    (text.to_string(), value)
}

#[test]
fn names_parse_ignoring_case_and_space_and_round_trip() {
    let names = assignment_field_names();
    assert_eq!(names.len(), AssignmentField::all().len());
    for (field, name) in AssignmentField::all().into_iter().zip(&names) {
        assert_eq!(AssignmentField::parse(name), Ok(field));
        assert_eq!(field.name(), name);
    }
    assert_eq!(
        AssignmentField::parse("  % work COMPLETE "),
        Ok(AssignmentField::PercentWorkComplete)
    );
    assert_eq!(
        AssignmentField::parse("Duration"),
        Err("unknown assignment field 'Duration'".into())
    );
}

#[test]
fn work_cost_and_units_read_as_project_shows_them() {
    let mut ed = editor();
    let uid = ed
        .add_assignment(1, ResourceRef::Uid(1), None, None)
        .unwrap();
    assert_eq!(
        read(&ed, uid, "Work"),
        shows("40 hrs", FieldValue::Minutes(2400))
    );
    assert_eq!(
        read(&ed, uid, "Cost"),
        shows("$2,000.00", FieldValue::Money(2000.0))
    );
    assert_eq!(
        read(&ed, uid, "Units"),
        shows("100%", FieldValue::Number(1.0))
    );
    assert_eq!(
        read(&ed, uid, "Regular Work"),
        shows("40 hrs", FieldValue::Minutes(2400))
    );
    assert_eq!(
        read(&ed, uid, "Remaining Cost"),
        shows("$2,000.00", FieldValue::Money(2000.0))
    );
    assert_eq!(read(&ed, uid, "Unique ID"), shows("1", FieldValue::Int(1)));
    assert_eq!(read(&ed, uid, "Task ID"), shows("1", FieldValue::Int(1)));
    assert_eq!(
        read(&ed, uid, "Task Name"),
        shows("Build", FieldValue::Text("Build".into()))
    );
    assert_eq!(
        read(&ed, uid, "Resource Name"),
        shows("Ann", FieldValue::Text("Ann".into()))
    );
    assert_eq!(
        read(&ed, uid, "Cost Rate Table"),
        shows("A", FieldValue::Text("A".into()))
    );
    assert_eq!(
        read(&ed, uid, "Work Contour"),
        shows("Flat", FieldValue::Text("Flat".into()))
    );
    assert_eq!(
        read(&ed, uid, "Delay"),
        shows("0 days", FieldValue::Minutes(0))
    );
    let start = DateTime::from_ymd_hm(2026, 1, 5, 8, 0);
    assert_eq!(
        read(&ed, uid, "Start"),
        shows("2026-01-05", FieldValue::Date(start))
    );
    let finish = ed.project().assignments[0].finish.unwrap();
    assert_eq!(
        read(&ed, uid, "Finish"),
        shows("2026-01-09", FieldValue::Date(finish))
    );
}

#[test]
fn absent_stored_values_read_null_with_project_s_text() {
    let mut ed = editor();
    let uid = ed
        .add_assignment(1, ResourceRef::Uid(1), None, None)
        .unwrap();
    for (name, text) in [
        ("Overtime Work", "0 hrs"),
        ("Actual Work", "0 hrs"),
        ("Baseline Work", "0 hrs"),
        ("Budget Work", "0 hrs"),
        ("Actual Cost", "$0.00"),
        ("Baseline Cost", "$0.00"),
        ("Budget Cost", "$0.00"),
        ("% Work Complete", "0%"),
        ("Peak", "0%"),
    ] {
        assert_eq!(
            read(&ed, uid, name),
            shows(text, FieldValue::Null),
            "{name}"
        );
    }
}

#[test]
fn materials_show_their_quantity_and_cost_resources_no_units() {
    let mut ed = editor();
    let steel = ed
        .add_assignment(1, ResourceRef::Uid(2), Some(5.0), None)
        .unwrap();
    assert_eq!(
        read(&ed, steel, "Units"),
        shows("5 tons", FieldValue::Number(5.0))
    );
    assert_eq!(
        read(&ed, steel, "Work"),
        shows("5 tons", FieldValue::Minutes(300))
    );
    let fee = ed
        .add_assignment(1, ResourceRef::Uid(3), None, None)
        .unwrap();
    assert_eq!(read(&ed, fee, "Units"), shows("", FieldValue::Null));
}

#[test]
fn stored_rate_table_contour_delay_and_dates_read_back() {
    let mut ed = editor();
    let uid = ed
        .add_assignment(1, ResourceRef::Uid(1), None, None)
        .unwrap();
    let mut a = ed.project().assignments[0].clone();
    a.cost_rate_table = Some(1);
    a.work_contour = Some(6);
    a.delay = Some(4800);
    a.peak_units = Rate::parse("1.5");
    let f = |a: &Assignment, field| read_assignment_field(&ed, a, field);
    assert_eq!(f(&a, AssignmentField::CostRateTable).text, "B");
    assert_eq!(f(&a, AssignmentField::WorkContour).text, "Bell");
    assert_eq!(
        (
            f(&a, AssignmentField::Delay).text,
            f(&a, AssignmentField::Delay).value
        ),
        ("1 day".to_string(), FieldValue::Minutes(480))
    );
    assert_eq!(f(&a, AssignmentField::Peak).value, FieldValue::Number(1.5));
    // Without stored dates, its span on the schedule.
    a.start = None;
    a.finish = None;
    let span = crate::assign::assignment_span(ed.project(), ed.schedule(), &a).unwrap();
    assert_eq!(assignment_dates(&ed, &a), Some(span));
    assert_eq!(uid, 1);
}
