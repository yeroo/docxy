use super::*;
use crate::editor::ResourceRef;
use crate::model::{ExtendedAttributeValue, XmlElement};

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
        assert_eq!(&field.name(), name);
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
        shows("Mon 1/5/26", FieldValue::Date(start))
    );
    let finish = ed.project().assignments[0].finish.unwrap();
    assert_eq!(
        read(&ed, uid, "Finish"),
        shows("Fri 1/9/26", FieldValue::Date(finish))
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

// ---- custom fields (#1178) ----

/// Task Text1's id and the assignment id its `SecondaryPID` names.
const TASK_TEXT1: &str = "188743731";
const ASSN_TEXT1: &str = "255868938";

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

/// An `<ExtendedAttribute>` definition: its FieldID, FieldName and, when
/// given, SecondaryPID.
fn def(field_id: &str, name: &str, secondary: Option<&str>) -> XmlElement {
    let mut children = vec![leaf("FieldID", field_id), leaf("FieldName", name)];
    children.extend(secondary.map(|pid| leaf("SecondaryPID", pid)));
    node("ExtendedAttribute", children)
}

/// A `ValueList` of `(value, guid)` entries.
fn value_list(entries: &[(&str, &str)]) -> XmlElement {
    node(
        "ValueList",
        entries
            .iter()
            .map(|&(value, guid)| {
                node("Value", vec![leaf("Value", value), leaf("FieldGUID", guid)])
            })
            .collect(),
    )
}

fn value(field_id: &str, text: &str) -> ExtendedAttributeValue {
    ExtendedAttributeValue {
        field_id: field_id.into(),
        value: Some(text.into()),
        ..ExtendedAttributeValue::default()
    }
}

fn by_guid(field_id: &str, guid: &str) -> ExtendedAttributeValue {
    ExtendedAttributeValue {
        field_id: field_id.into(),
        value_guid: Some(guid.into()),
        ..ExtendedAttributeValue::default()
    }
}

/// Task 1 holding `task_values`, assigned to Ann (assignment 1) with
/// `assn_values`, under these definitions.
fn custom_editor(
    defs: Vec<XmlElement>,
    task_values: Vec<ExtendedAttributeValue>,
    assn_values: Vec<ExtendedAttributeValue>,
) -> Editor {
    let mut p = editor().project().clone();
    p.extended_attribute_definitions = defs;
    p.tasks[0].extended_attributes = task_values;
    p.assignments = vec![Assignment {
        uid: 1,
        task_uid: 1,
        resource_uid: 1,
        units: 1.0,
        work_min: 2400,
        extended_attributes: assn_values,
        ..Assignment::default()
    }];
    Editor::new(p)
}

/// Task 1's `(text, value)` for a task field.
fn task_read(ed: &Editor, name: &str) -> (String, FieldValue) {
    let task = ed.project().task(1).unwrap();
    let f = FieldReader::new(ed).read(task, Field::parse(name).unwrap());
    (f.text, f.value)
}

fn txt(text: &str) -> (String, FieldValue) {
    shows(text, FieldValue::Text(text.into()))
}

#[test]
fn custom_names_parse_and_list_after_the_builtins() {
    let names = assignment_field_names();
    assert_eq!(names.len(), NAMES.len() + 120);
    assert_eq!(names[NAMES.len()], "Text1");
    assert_eq!(names.last().unwrap(), "Duration10");
    // The custom names follow the built-ins in the task registry's order.
    let task_customs: Vec<String> = Field::all()
        .into_iter()
        .filter(|f| matches!(f, Field::Custom(..)))
        .map(Field::name)
        .collect();
    assert_eq!(names[NAMES.len()..], task_customs[..]);
    for (given, field) in [
        ("Text1", AssignmentField::Custom(CustomKind::Text, 1)),
        ("text30", AssignmentField::Custom(CustomKind::Text, 30)),
        ("number20", AssignmentField::Custom(CustomKind::Number, 20)),
        (
            " Duration10 ",
            AssignmentField::Custom(CustomKind::Duration, 10),
        ),
        ("Cost1", AssignmentField::Custom(CustomKind::Cost, 1)),
        ("Cost", AssignmentField::Cost),
        ("Start", AssignmentField::Start),
        ("start10", AssignmentField::Custom(CustomKind::Start, 10)),
        ("Flag20", AssignmentField::Custom(CustomKind::Flag, 20)),
    ] {
        assert_eq!(AssignmentField::parse(given), Ok(field), "{given}");
    }
    for bad in [
        "Text31",
        "Number21",
        "Text0",
        "Flag21",
        "Duration11",
        "Cost11",
    ] {
        assert_eq!(
            AssignmentField::parse(bad),
            Err(format!("unknown assignment field '{bad}'"))
        );
    }
}

#[test]
fn custom_fields_resolve_through_the_definition_field_id() {
    let ed = custom_editor(
        vec![
            def("255869001", "Text1", None),
            def("255869002", "Number1", None),
            def("255869003", "Cost1", None),
            def("255869004", "Flag1", None),
            def("255869005", "Date1", None),
            def("255869006", "Start1", None),
            def("255869007", "Finish1", None),
            def("255869008", "Duration1", None),
            def("255869009", "Duration2", None),
        ],
        Vec::new(),
        vec![
            value("255869001", "M&E"),
            value("255869002", "12.50"),
            value("255869003", "12550"),
            value("255869004", "1"),
            value("255869005", "2026-03-04T17:00:00"),
            value("255869006", "2026-03-02T08:00:00"),
            value("255869007", "2026-03-05T17:00:00"),
            ExtendedAttributeValue {
                duration_format: Some(5),
                ..value("255869008", "PT16H0M0S")
            },
            value("255869009", "PT16H0M0S"),
        ],
    );
    let date = |text: &str| FieldValue::Date(DateTime::parse_mspdi(text).unwrap());
    for (name, text, want) in [
        ("Text1", "M&E", FieldValue::Text("M&E".into())),
        ("Number1", "12.5", FieldValue::Number(12.5)),
        ("Cost1", "$125.50", FieldValue::Money(125.5)),
        ("Flag1", "Yes", FieldValue::Bool(true)),
        ("Date1", "Wed 3/4/26", date("2026-03-04T17:00:00")),
        ("Start1", "Mon 3/2/26", date("2026-03-02T08:00:00")),
        ("Finish1", "Thu 3/5/26", date("2026-03-05T17:00:00")),
        // The value's own DurationFormat (5 = hours), else days.
        ("Duration1", "16 hrs", FieldValue::Minutes(960)),
        ("Duration2", "2 days", FieldValue::Minutes(960)),
    ] {
        assert_eq!(read(&ed, 1, name), shows(text, want), "{name}");
    }
}

#[test]
fn custom_fields_resolve_through_a_task_definition_s_secondary_pid() {
    let guid = "7F2B5E61-7C21-4E0A-9B55-3C8D12A4E102";
    let mut text1 = def(TASK_TEXT1, "Text1", Some(ASSN_TEXT1));
    text1.children.push(value_list(&[
        ("Civil", "7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101"),
        ("M&E", guid),
    ]));
    // A lookup entry named by its guid, case-insensitively.
    let ed = custom_editor(
        vec![text1.clone()],
        Vec::new(),
        vec![by_guid(ASSN_TEXT1, &guid.to_lowercase())],
    );
    assert_eq!(read(&ed, 1, "Text1"), txt("M&E"));
    // A plain stored value.
    let ed = custom_editor(vec![text1], Vec::new(), vec![value(ASSN_TEXT1, "Steel")]);
    assert_eq!(read(&ed, 1, "Text1"), txt("Steel"));
    // Other kinds pair the same way.
    let ed = custom_editor(
        vec![def("188743783", "Duration1", Some("255868966"))],
        Vec::new(),
        vec![value("255868966", "PT8H0M0S")],
    );
    assert_eq!(
        read(&ed, 1, "Duration1"),
        shows("1 day", FieldValue::Minutes(480))
    );
}

#[test]
fn custom_fields_do_not_cross_owners() {
    let defs = || vec![def(TASK_TEXT1, "Text1", Some(ASSN_TEXT1))];
    // Each owner reads its own value.
    let ed = custom_editor(
        defs(),
        vec![value(TASK_TEXT1, "task")],
        vec![value(ASSN_TEXT1, "assignment")],
    );
    assert_eq!(task_read(&ed, "Text1"), txt("task"));
    assert_eq!(read(&ed, 1, "Text1"), txt("assignment"));
    // The task's value never reads as the assignment's.
    let ed = custom_editor(
        defs(),
        vec![value(TASK_TEXT1, "task")],
        vec![value("188743999", "other")],
    );
    assert_eq!(read(&ed, 1, "Text1"), shows("", FieldValue::Null));
    // The task resolver ignores SecondaryPID, even for a task value under
    // that id.
    let ed = custom_editor(
        defs(),
        vec![value(ASSN_TEXT1, "stray")],
        vec![value(ASSN_TEXT1, "assignment")],
    );
    assert_eq!(task_read(&ed, "Text1"), shows("", FieldValue::Null));
}

#[test]
fn exact_field_id_beats_an_earlier_secondary_pid() {
    let ed = custom_editor(
        vec![
            def(TASK_TEXT1, "Text1", Some(ASSN_TEXT1)),
            def("255869001", "Text1", None),
        ],
        Vec::new(),
        vec![value(ASSN_TEXT1, "paired"), value("255869001", "exact")],
    );
    assert_eq!(read(&ed, 1, "Text1"), txt("exact"));
}

#[test]
fn shared_secondary_pid_takes_the_table_holding_the_guid() {
    let mut task_def = def(TASK_TEXT1, "Text1", Some(ASSN_TEXT1));
    task_def.children.push(value_list(&[("Civil", "AAAA-1")]));
    let mut resource_def = def("205520904", "Text1", Some(ASSN_TEXT1));
    resource_def
        .children
        .push(value_list(&[("Crane", "BBBB-1")]));
    for defs in [
        vec![task_def.clone(), resource_def.clone()],
        vec![resource_def.clone(), task_def.clone()],
    ] {
        let ed = custom_editor(
            defs.clone(),
            Vec::new(),
            vec![by_guid(ASSN_TEXT1, "BBBB-1")],
        );
        assert_eq!(read(&ed, 1, "Text1"), txt("Crane"));
        let ed = custom_editor(defs, Vec::new(), vec![by_guid(ASSN_TEXT1, "AAAA-1")]);
        assert_eq!(read(&ed, 1, "Text1"), txt("Civil"));
    }
}

#[test]
fn unset_and_unparseable_custom_values() {
    // No definitions: an assignment value under any id reads unset.
    let ed = custom_editor(Vec::new(), Vec::new(), vec![value(ASSN_TEXT1, "orphan")]);
    for (name, text, want) in [
        ("Text1", "", FieldValue::Null),
        ("Number1", "0", FieldValue::Null),
        ("Cost1", "$0.00", FieldValue::Null),
        ("Flag1", "No", FieldValue::Bool(false)),
        ("Date1", "NA", FieldValue::Null),
        ("Start1", "NA", FieldValue::Null),
        ("Finish1", "NA", FieldValue::Null),
        ("Duration1", "0 days", FieldValue::Null),
    ] {
        assert_eq!(read(&ed, 1, name), shows(text, want), "{name}");
    }
    // A stored value that does not parse reads as its raw text and Null.
    let ed = custom_editor(
        vec![
            def("255869002", "Number1", None),
            def("188743000", "Flag1", Some("255869004")),
        ],
        Vec::new(),
        vec![value("255869002", "abc"), value("255869004", "maybe")],
    );
    assert_eq!(read(&ed, 1, "Number1"), shows("abc", FieldValue::Null));
    assert_eq!(read(&ed, 1, "Flag1"), shows("maybe", FieldValue::Null));
}

#[test]
fn custom_values_read_from_mspdi_and_survive_a_save() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Name>assignment-custom</Name>
  <StartDate>2026-03-02T08:00:00</StartDate>
  <ExtendedAttributes>
    <ExtendedAttribute>
      <FieldID>188743731</FieldID><FieldName>Text1</FieldName><Alias>Trade</Alias>
      <SecondaryPID>255868938</SecondaryPID>
      <ValueList>
        <Value><ID>1</ID><Value>Civil</Value><FieldGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101</FieldGUID></Value>
      </ValueList>
    </ExtendedAttribute>
    <ExtendedAttribute>
      <FieldID>255869002</FieldID><FieldName>Number1</FieldName>
    </ExtendedAttribute>
  </ExtendedAttributes>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>Build</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT16H0M0S</Duration></Task>
  </Tasks>
  <Resources>
    <Resource><UID>1</UID><ID>1</ID><Name>Alice</Name><Type>1</Type></Resource>
  </Resources>
  <Assignments>
    <Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID><Units>1</Units>
      <Work>PT16H0M0S</Work>
      <ExtendedAttribute><FieldID>255868938</FieldID><ValueGUID>7F2B5E61-7C21-4E0A-9B55-3C8D12A4E101</ValueGUID></ExtendedAttribute>
      <ExtendedAttribute><FieldID>255869002</FieldID><Value>7</Value></ExtendedAttribute>
    </Assignment>
  </Assignments>
</Project>"#;
    let check = |ed: &Editor| {
        assert_eq!(read(ed, 1, "Text1"), txt("Civil"));
        assert_eq!(read(ed, 1, "Number1"), shows("7", FieldValue::Number(7.0)));
    };
    let ed = Editor::new(crate::mspdi::read_mspdi(xml).unwrap());
    check(&ed);
    let saved = crate::mspdi::write_mspdi(ed.project());
    check(&Editor::new(crate::mspdi::read_mspdi(&saved).unwrap()));
}
