use super::*;
use core::prelude::v1::test;

fn args(text: &str) -> Json {
    Json::parse(text).unwrap()
}

/// A two-page form with one control of every kind, a disabled field and
/// button, and a button that opens a nested dialog.
fn form() -> Dialog {
    let mut disabled = Control::new(
        "locked",
        "Locked:",
        ControlKind::Text,
        Value::Text("x".into()),
    );
    disabled.enabled = false;
    let mut hidden = Control::new(
        "secret",
        "Secret:",
        ControlKind::Text,
        Value::Text(String::new()),
    );
    hidden.visible = false;
    let mut calendar = Control::new(
        "calendar",
        "&Calendar:",
        ControlKind::Dropdown,
        Value::Choice(Some(0)),
    );
    calendar.items = vec!["Standard".into(), "24 Hours".into(), "Night Shift".into()];
    let mut kind = Control::new("kind", "Type", ControlKind::Radio, Value::Choice(None));
    kind.items = vec!["Fixed Units".into(), "Fixed Work".into()];
    let mut list = Control::new("fields", "Fields", ControlKind::List, Value::Choice(None));
    list.items = vec!["Name".into(), "Start".into()];
    let mut preds = Control::new(
        "predecessors",
        "Predecessors",
        ControlKind::Grid,
        Value::Rows(vec![vec!["1".into(), "FS".into()]]),
    );
    preds.columns = vec!["ID".into(), "Type".into()];
    let general = |mut c: Control| {
        c.page = Some(0);
        c
    };
    let mut d = Dialog {
        id: "form",
        title: "Task Information".into(),
        text: None,
        tabs: vec!["General".into(), "Predecessors".into()],
        tab: 0,
        controls: vec![
            Control::new(
                "name",
                "&Name:",
                ControlKind::Text,
                Value::Text("Design".into()),
            ),
            Control::new(
                "percent",
                "Percent complete:",
                ControlKind::Number,
                Value::Text("0".into()),
            ),
            Control::new(
                "start",
                "Start:",
                ControlKind::Date,
                Value::Text("2026-03-02".into()),
            ),
            Control::new(
                "duration",
                "Duration:",
                ControlKind::Duration,
                Value::Text("1d".into()),
            ),
            Control::new(
                "milestone",
                "Mark as milestone",
                ControlKind::Checkbox,
                Value::Bool(false),
            ),
            Control::new(
                "hint",
                "Hint",
                ControlKind::Label,
                Value::Text("Read me".into()),
            ),
            calendar,
            kind,
            list,
            disabled,
            hidden,
        ],
        buttons: vec![
            Button {
                default: true,
                ..Button::new("OK", ButtonRole::Accept)
            },
            Button::new("Cancel", ButtonRole::Cancel),
            Button::new("Details...", ButtonRole::Open(ChildDialog::Test)),
            Button {
                enabled: false,
                ..Button::new("Help", ButtonRole::Apply)
            },
        ],
        owner: DialogOwner::Test,
        focus: None,
        caret: None,
        anchor: None,
        caret_for: None,
        opened: Vec::new(),
        react: None,
    };
    d.controls = d.controls.into_iter().map(general).collect();
    preds.page = Some(1);
    d.controls.push(preds);
    d
}

fn stack() -> DialogStack {
    let mut s = DialogStack::default();
    s.push(form());
    s
}

fn text(s: &DialogStack, name: &str) -> String {
    match s.top().unwrap().value(name) {
        Some(Value::Text(t)) => t.clone(),
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn nothing_open_reads_closed_and_refuses_input() {
    let mut s = DialogStack::default();
    assert_eq!(s.to_json(), Json::obj(vec![("open", Json::Bool(false))]));
    assert_eq!(s.top_id(), "none");
    assert_eq!(
        s.set("Name", &args(r#"{"value":"x"}"#)),
        Err(NONE_OPEN.into())
    );
    assert_eq!(s.select_tab("General"), Err(NONE_OPEN.into()));
    assert_eq!(s.click("OK", |_| Ok(())), Err(NONE_OPEN.into()));
    assert_eq!(s.key_button("enter", true), None);
}

#[test]
fn read_lists_the_current_pages_controls_and_the_buttons() {
    let s = stack();
    let json = s.to_json();
    assert_eq!(json.get("open"), Some(&Json::Bool(true)));
    assert_eq!(json.get_str("id"), Some("form"));
    assert_eq!(json.get_usize("depth"), Some(1));
    assert_eq!(json.get_str("tab"), Some("General"));
    assert_eq!(json.get("text"), Some(&Json::Null));
    let controls = json.get("controls").unwrap().as_array().unwrap();
    let names: Vec<&str> = controls
        .iter()
        .map(|c| c.get_str("name").unwrap())
        .collect();
    // The grid sits on the Predecessors page; the hidden field is listed with
    // visible:false, because it is on this page.
    assert!(!names.contains(&"predecessors"));
    assert!(names.contains(&"secret"));
    let by = |n: &str| {
        controls
            .iter()
            .find(|c| c.get_str("name") == Some(n))
            .unwrap()
    };
    assert_eq!(by("secret").get("visible"), Some(&Json::Bool(false)));
    assert_eq!(by("locked").get("enabled"), Some(&Json::Bool(false)));
    assert_eq!(by("percent").get("value"), Some(&Json::Num(0.0)));
    assert_eq!(by("milestone").get("value"), Some(&Json::Bool(false)));
    assert_eq!(by("milestone").get_str("kind"), Some("checkbox"));
    assert_eq!(by("calendar").get_str("value"), Some("Standard"));
    assert_eq!(by("calendar").get_usize("selected"), Some(0));
    assert_eq!(
        by("calendar")
            .get("items")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(by("kind").get("value"), Some(&Json::Null));
    let buttons = json.get("buttons").unwrap().as_array().unwrap();
    assert_eq!(buttons[0].get_str("label"), Some("OK"));
    assert_eq!(buttons[0].get("default"), Some(&Json::Bool(true)));
    assert_eq!(buttons[3].get("enabled"), Some(&Json::Bool(false)));
}

#[test]
fn controls_answer_to_their_label_without_accelerators_or_colon_then_their_name() {
    let mut s = stack();
    s.set("name", &args(r#"{"value":"By label"}"#)).unwrap();
    assert_eq!(text(&s, "name"), "By label");
    s.set("NAME:", &args(r#"{"value":"Folded"}"#)).unwrap();
    assert_eq!(text(&s, "name"), "Folded");
    // "percent" is not a label, so it falls back to the name.
    s.set("percent", &args(r#"{"value":"50"}"#)).unwrap();
    assert_eq!(text(&s, "percent"), "50");
}

#[test]
fn an_ambiguous_label_is_refused_naming_the_candidates() {
    let mut d = form();
    d.controls.push(Control::new(
        "alias",
        "Name",
        ControlKind::Text,
        Value::Text(String::new()),
    ));
    let e = d.set("Name", &args(r#"{"value":"x"}"#)).unwrap_err();
    assert_eq!(e, "'Name' names 2 controls; use a name: name, alias");
    d.set("alias", &args(r#"{"value":"x"}"#)).unwrap();
}

#[test]
fn unknown_control_button_and_tab_are_refused_listing_what_exists() {
    let mut s = stack();
    let e = s.set("Colour", &args(r#"{"value":"x"}"#)).unwrap_err();
    assert!(
        e.starts_with("no control 'Colour'; controls: Name:, Percent complete:"),
        "{e}"
    );
    assert!(
        !e.contains("Secret"),
        "hidden controls are not offered: {e}"
    );
    assert_eq!(
        s.click("Apply", |_| Ok(())).unwrap_err(),
        "no button 'Apply'; buttons: OK, Cancel, Details..., Help"
    );
    assert_eq!(
        s.select_tab("Notes").unwrap_err(),
        "no tab 'Notes'; tabs: General, Predecessors"
    );
}

#[test]
fn disabled_or_hidden_controls_and_disabled_buttons_refuse() {
    let mut s = stack();
    assert_eq!(
        s.set("Locked", &args(r#"{"value":"y"}"#)).unwrap_err(),
        "'Locked:' is disabled"
    );
    assert_eq!(text(&s, "locked"), "x");
    assert_eq!(
        s.set("secret", &args(r#"{"value":"y"}"#)).unwrap_err(),
        "'Secret:' is hidden"
    );
    let mut applied = false;
    assert_eq!(
        s.click("help", |_| {
            applied = true;
            Ok(())
        })
        .unwrap_err(),
        "'Help' is disabled"
    );
    assert!(!applied);
    assert_eq!(
        s.set("Hint", &args(r#"{"value":"y"}"#)).unwrap_err(),
        "'Hint' is a label; it cannot be set"
    );
}

#[test]
fn values_are_checked_against_the_controls_kind() {
    let mut s = stack();
    let set = |s: &mut DialogStack, c: &str, v: &str| s.set(c, &args(v));
    assert_eq!(
        set(&mut s, "Mark as milestone", r#"{"value":"yes"}"#).unwrap_err(),
        "'Mark as milestone' takes true or false"
    );
    set(&mut s, "Mark as milestone", r#"{"value":true}"#).unwrap();
    assert_eq!(
        s.top().unwrap().value("milestone"),
        Some(&Value::Bool(true))
    );

    assert_eq!(
        set(&mut s, "Percent complete", r#"{"value":"half"}"#).unwrap_err(),
        "'Percent complete:' takes a number"
    );
    set(&mut s, "Percent complete", r#"{"value":25}"#).unwrap();
    assert_eq!(text(&s, "percent"), "25");
    // Rust's float parser takes these; a number field must not.
    for bad in ["NaN", "inf", "-infinity", "1e999"] {
        let v = format!(r#"{{"value":"{bad}"}}"#);
        assert_eq!(
            set(&mut s, "Percent complete", &v).unwrap_err(),
            "'Percent complete:' takes a number",
            "{bad}"
        );
    }
    assert_eq!(text(&s, "percent"), "25");

    // Dropdown, radio and list take an item's label, which must exist.
    let e = set(&mut s, "Calendar", r#"{"value":"Weekend"}"#).unwrap_err();
    assert_eq!(
        e,
        "'Calendar:' has no item 'Weekend'; items: Standard, 24 Hours, Night Shift"
    );
    set(&mut s, "Calendar", r#"{"value":"night shift"}"#).unwrap();
    assert_eq!(
        s.top().unwrap().value("calendar"),
        Some(&Value::Choice(Some(2)))
    );
    set(&mut s, "Type", r#"{"value":"Fixed Work"}"#).unwrap();
    assert!(set(&mut s, "Fields", r#"{"value":"Finish"}"#).is_err());
    set(&mut s, "Fields", r#"{"value":"Start"}"#).unwrap();
    assert_eq!(
        s.top().unwrap().value("fields"),
        Some(&Value::Choice(Some(1)))
    );

    // Dates and durations are staged as text; the owner parses them on OK.
    set(&mut s, "Start", r#"{"value":"not a date"}"#).unwrap();
    assert_eq!(text(&s, "start"), "not a date");
    set(&mut s, "Duration", r#"{"value":"3d"}"#).unwrap();
    assert_eq!(text(&s, "duration"), "3d");
    assert_eq!(
        set(&mut s, "Duration", r#"{"value":3}"#).unwrap_err(),
        "'Duration:' takes text"
    );
    assert_eq!(
        set(&mut s, "Name", "{}").unwrap_err(),
        "missing argument 'value'"
    );
}

#[test]
fn a_control_on_another_tab_needs_dialog_tab_first() {
    let mut s = stack();
    s.set("Name", &args(r#"{"value":"Staged"}"#)).unwrap();
    let cell = r#"{"row":0,"column":"Type","value":"SS"}"#;
    assert_eq!(
        s.set("Predecessors", &args(cell)).unwrap_err(),
        "'Predecessors' is on the 'Predecessors' tab; switch with dialog-tab first"
    );
    s.select_tab("predecessors").unwrap();
    let json = s.to_json();
    assert_eq!(json.get_str("tab"), Some("Predecessors"));
    let controls = json.get("controls").unwrap().as_array().unwrap();
    assert_eq!(controls.len(), 1);
    assert_eq!(controls[0].get_str("kind"), Some("grid"));
    s.set("Predecessors", &args(cell)).unwrap();
    assert_eq!(
        s.set("Name", &args(r#"{"value":"x"}"#)).unwrap_err(),
        "'Name:' is on the 'General' tab; switch with dialog-tab first"
    );
    // Each page's staged values survive switching away and back.
    s.select_tab("General").unwrap();
    assert_eq!(text(&s, "name"), "Staged");
    s.select_tab("Predecessors").unwrap();
    assert_eq!(
        s.top().unwrap().value("predecessors"),
        Some(&Value::Rows(vec![vec!["1".into(), "SS".into()]]))
    );
}

#[test]
fn grid_cells_rows_and_bounds() {
    let mut s = stack();
    s.select_tab("Predecessors").unwrap();
    let rows = |s: &DialogStack| match s.top().unwrap().value("predecessors") {
        Some(Value::Rows(r)) => r.clone(),
        other => panic!("{other:?}"),
    };
    let set = |s: &mut DialogStack, v: &str| s.set("Predecessors", &args(v));
    set(&mut s, r#"{"row":0,"column":1,"value":"SS"}"#).unwrap();
    assert_eq!(rows(&s), vec![vec!["1".to_string(), "SS".into()]]);
    set(&mut s, r#"{"insert_row":1}"#).unwrap();
    set(&mut s, r#"{"row":1,"column":"id","value":"4"}"#).unwrap();
    assert_eq!(rows(&s)[1], vec!["4".to_string(), String::new()]);
    assert_eq!(
        set(&mut s, r#"{"row":2,"column":0,"value":"9"}"#).unwrap_err(),
        "'Predecessors' has no row 2; rows 0..1"
    );
    assert_eq!(
        set(&mut s, r#"{"row":0,"column":2,"value":"9"}"#).unwrap_err(),
        "'column' must be a column name or 0..2"
    );
    assert_eq!(
        set(&mut s, r#"{"row":0,"column":"Lag","value":"9"}"#).unwrap_err(),
        "'Predecessors' has no column 'Lag'; columns: ID, Type"
    );
    assert!(set(&mut s, r#"{"insert_row":3}"#).is_err());
    set(&mut s, r#"{"delete_row":0}"#).unwrap();
    assert_eq!(rows(&s), vec![vec!["4".to_string(), String::new()]]);
    assert!(set(&mut s, r#"{"delete_row":1}"#).is_err());
    let json = s.to_json();
    let grid = &json.get("controls").unwrap().as_array().unwrap()[0];
    assert_eq!(grid.get_str("text"), Some("1 row"));
    assert_eq!(grid.get("columns").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(grid.get("rows").unwrap().as_array().unwrap().len(), 1);
}

#[test]
fn cancel_discards_staged_values_and_ok_hands_them_to_the_owner() {
    let mut s = stack();
    s.set("Name", &args(r#"{"value":"Staged"}"#)).unwrap();
    // Staged values read back before OK.
    assert_eq!(text(&s, "name"), "Staged");
    let mut applied = false;
    s.click("Cancel", |_| {
        applied = true;
        Ok(())
    })
    .unwrap();
    assert!(!applied, "Cancel never reaches the owner");
    assert!(!s.is_open());

    let mut s = stack();
    s.set("Name", &args(r#"{"value":"Kept"}"#)).unwrap();
    let mut seen = None;
    s.click("ok", |d| {
        seen = d.value("name").cloned();
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, Some(Value::Text("Kept".into())));
    assert!(!s.is_open());
}

#[test]
fn an_owner_refusing_ok_keeps_the_dialog_and_its_staged_values() {
    let mut s = stack();
    s.set("Start", &args(r#"{"value":"soon"}"#)).unwrap();
    let e = s.click("OK", |_| Err("'soon' is not a date".into()));
    assert_eq!(e, Err("'soon' is not a date".into()));
    assert_eq!(s.top_id(), "form");
    assert_eq!(text(&s, "start"), "soon");
}

#[test]
fn apply_hands_the_values_over_and_stays_open() {
    let mut d = form();
    d.buttons[3].enabled = true;
    let mut s = DialogStack::default();
    s.push(d);
    let mut applied = 0;
    s.click("Help", |_| {
        applied += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(applied, 1);
    assert_eq!(s.top_id(), "form");
}

#[test]
fn a_nested_dialog_stacks_and_its_cancel_returns_to_the_parent_intact() {
    let mut s = stack();
    s.set("Name", &args(r#"{"value":"Parent staged"}"#))
        .unwrap();
    s.click("Details...", |_| panic!("opening a child is not an apply"))
        .unwrap();
    let json = s.to_json();
    assert_eq!(json.get_str("id"), Some("child"));
    assert_eq!(json.get_usize("depth"), Some(2));
    assert_eq!(json.get_str("title"), Some("Task Information › Details"));
    assert_eq!(s.top_id(), "child");

    // Only the top dialog takes input.
    assert!(s.set("Name", &args(r#"{"value":"x"}"#)).is_err());
    s.set("Note", &args(r#"{"value":"child only"}"#)).unwrap();
    s.click("Cancel", |_| Ok(())).unwrap();
    assert_eq!(s.depth(), 1);
    assert_eq!(s.top_id(), "form");
    assert_eq!(text(&s, "name"), "Parent staged");

    // The child's OK goes to its owner and returns to the parent.
    s.click("Details...", |_| Ok(())).unwrap();
    let mut note = None;
    s.click("OK", |d| {
        note = Some(d.id);
        Ok(())
    })
    .unwrap();
    assert_eq!(note, Some("child"));
    assert_eq!(s.top_id(), "form");
}

#[test]
fn enter_is_the_default_button_escape_the_cancel_one_and_other_keys_nothing() {
    let s = stack();
    assert_eq!(s.key_button("enter", true).as_deref(), Some("OK"));
    assert_eq!(s.key_button("escape", true).as_deref(), Some("Cancel"));
    assert_eq!(s.key_button("enter", false), None, "a chord is not Enter");
    assert_eq!(s.key_button("a", true), None);
    assert_eq!(s.key_button("tab", true), None);

    let mut d = form();
    d.buttons[0].enabled = false;
    let mut s = DialogStack::default();
    s.push(d);
    assert_eq!(
        s.key_button("enter", true),
        None,
        "a disabled default does nothing"
    );
}

#[test]
fn a_message_box_defaults_to_its_first_button() {
    let d = Dialog::message(
        "delete-summary",
        "Delete",
        "Delete 'A' and its 1 subtask?".into(),
        &[("Yes", ButtonRole::Accept), ("No", ButtonRole::Cancel)],
        DialogOwner::Test,
    );
    let mut s = DialogStack::default();
    s.push(d);
    let json = s.to_json();
    assert_eq!(json.get_str("text"), Some("Delete 'A' and its 1 subtask?"));
    assert_eq!(json.get("tab"), Some(&Json::Null));
    assert_eq!(json.get("tabs").unwrap().as_array().unwrap().len(), 0);
    assert_eq!(s.key_button("enter", true).as_deref(), Some("Yes"));
    assert_eq!(s.key_button("escape", true).as_deref(), Some("No"));
    s.clear();
    assert!(!s.is_open());
}

fn focused_name(s: &DialogStack) -> Option<&'static str> {
    s.top().unwrap().focused().map(|c| c.name)
}

fn index(s: &DialogStack, name: &str) -> usize {
    s.top()
        .unwrap()
        .controls
        .iter()
        .position(|c| c.name == name)
        .unwrap()
}

/// Tab and Shift+Tab walk the editable widgets on the current page, skipping
/// labels, lists, grids, and disabled or hidden controls (#649).
#[test]
fn tab_steps_through_the_editable_widgets_and_wraps() {
    let mut s = stack();
    assert_eq!(focused_name(&s), None);
    let mut order = Vec::new();
    for _ in 0..8 {
        s.top_dialog_mut().unwrap().focus_step(false);
        order.push(focused_name(&s).unwrap());
    }
    assert_eq!(
        order,
        [
            "name",
            "percent",
            "start",
            "duration",
            "milestone",
            "calendar",
            "kind",
            "name"
        ]
    );
    s.top_dialog_mut().unwrap().focus_step(true);
    assert_eq!(focused_name(&s), Some("kind"));
    // Another tab has none of these: the focus goes with the page.
    s.select_tab("Predecessors").unwrap();
    assert_eq!(focused_name(&s), None);
}

/// Typing edits the focused field through `Control::set`: a number field
/// takes the start of a number and refuses a letter, changing nothing.
#[test]
fn typing_and_backspace_edit_the_focused_field() {
    let mut s = stack();
    let d = s.top_dialog_mut().unwrap();
    d.type_char('x').unwrap();
    assert_eq!(text(&s, "name"), "Design", "nothing focused, nothing typed");
    let d = s.top_dialog_mut().unwrap();
    d.focus = Some(1); // Percent complete
    d.backspace().unwrap();
    assert_eq!(text(&s, "percent"), "", "a number field can be emptied");
    let d = s.top_dialog_mut().unwrap();
    for c in "-1.5".chars() {
        d.type_char(c).unwrap();
    }
    assert_eq!(text(&s, "percent"), "-1.5");
    let d = s.top_dialog_mut().unwrap();
    assert_eq!(
        d.type_char('e').unwrap_err(),
        "'Percent complete:' takes a number"
    );
    assert_eq!(
        d.type_char('.').unwrap_err(),
        "'Percent complete:' takes a number"
    );
    assert_eq!(text(&s, "percent"), "-1.5");
    let d = s.top_dialog_mut().unwrap();
    d.focus = Some(0);
    d.backspace().unwrap();
    d.space().unwrap();
    d.type_char('!').unwrap();
    assert_eq!(text(&s, "name"), "Desig !");
}

/// A click on a widget goes through the same `set`: a checkbox toggles, a
/// radio picks the item clicked, a dropdown steps to its next item; a
/// disabled field refuses and takes no focus.
#[test]
fn clicks_toggle_pick_and_step_through_set() {
    let mut s = stack();
    let (milestone, kind, calendar, locked) = (
        index(&s, "milestone"),
        index(&s, "kind"),
        index(&s, "calendar"),
        index(&s, "locked"),
    );
    let d = s.top_dialog_mut().unwrap();
    d.click_control(milestone, None).unwrap();
    assert_eq!(d.value("milestone"), Some(&Value::Bool(true)));
    assert_eq!(focused_name(&s), Some("milestone"));
    let d = s.top_dialog_mut().unwrap();
    d.space().unwrap();
    assert_eq!(d.value("milestone"), Some(&Value::Bool(false)));
    d.click_control(kind, Some(1)).unwrap();
    assert_eq!(d.value("kind"), Some(&Value::Choice(Some(1))));
    d.step_focused(false).unwrap();
    assert_eq!(d.value("kind"), Some(&Value::Choice(Some(0))));
    d.click_control(calendar, None).unwrap();
    assert_eq!(d.value("calendar"), Some(&Value::Choice(Some(1))));
    d.click_control(calendar, None).unwrap();
    d.click_control(calendar, None).unwrap();
    assert_eq!(
        d.value("calendar"),
        Some(&Value::Choice(Some(0))),
        "it wraps"
    );
    d.step_focused(true).unwrap();
    assert_eq!(d.value("calendar"), Some(&Value::Choice(Some(1))));
    assert_eq!(
        d.click_control(locked, None).unwrap_err(),
        "'Locked:' cannot be edited"
    );
    assert_eq!(focused_name(&s), Some("calendar"));
}

#[test]
fn a_write_back_child_hands_its_values_to_the_open_parent() {
    // The form's Details... opens a write-back child here.
    let mut parent = form();
    for b in &mut parent.buttons {
        if b.label == "Details..." {
            b.role = ButtonRole::Open(ChildDialog::TestWriteBack);
        }
    }
    let mut s = DialogStack::default();
    s.push(parent);
    s.click("Details...", |_| panic!("opening a child is not an apply"))
        .unwrap();
    s.set("Note", &args(r#"{"value":"from the child"}"#))
        .unwrap();
    // OK never reaches an owner: the parent takes the values and stays open.
    s.click("OK", |_| panic!("a write-back child applies nothing"))
        .unwrap();
    assert_eq!(s.depth(), 1);
    assert_eq!(s.top_id(), "form");
    assert_eq!(text(&s, "name"), "from the child");
    // Cancel on the child changes nothing.
    s.click("Details...", |_| Ok(())).unwrap();
    s.set("Note", &args(r#"{"value":"dropped"}"#)).unwrap();
    s.click("Cancel", |_| Ok(())).unwrap();
    assert_eq!(text(&s, "name"), "from the child");
}

/// A stack whose `name` field (text "Design") has the focus.
fn focused_on_name() -> DialogStack {
    let mut s = stack();
    s.top_dialog_mut().unwrap().focus_step(false);
    assert_eq!(focused_name(&s), Some("name"));
    s
}

/// A caret edits in the middle of a field, not just at its end (#1027).
#[test]
fn the_caret_edits_inside_a_field() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    assert_eq!(d.caret_at(), 6, "after the opening text");
    d.move_caret_edge(false, false);
    d.type_char('X').unwrap();
    assert_eq!(text(&s, "name"), "XDesign");
    let d = s.top_dialog_mut().unwrap();
    d.move_caret(true, false);
    d.delete().unwrap();
    assert_eq!(text(&s, "name"), "XDsign");
    let d = s.top_dialog_mut().unwrap();
    d.backspace().unwrap();
    assert_eq!(text(&s, "name"), "Xsign");
    let d = s.top_dialog_mut().unwrap();
    d.backspace().unwrap();
    assert_eq!(d.caret_at(), 0);
    d.backspace().unwrap();
    d.move_caret(false, false);
    assert_eq!(
        text(&s, "name"),
        "sign",
        "nothing before the first character"
    );
    let d = s.top_dialog_mut().unwrap();
    d.move_caret_edge(true, false);
    d.delete().unwrap();
    d.type_char('!').unwrap();
    assert_eq!(text(&s, "name"), "sign!");
}

/// Select-all then typing replaces the text; Shift+arrows extend a selection;
/// a plain arrow drops it at its edge.
#[test]
fn a_selection_is_replaced_by_typing_and_dropped_by_an_arrow() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    assert_eq!(d.selection(), Some((0, 6)));
    assert_eq!(d.selected_text().as_deref(), Some("Design"));
    d.type_char('J').unwrap();
    assert_eq!(text(&s, "name"), "J");
    let d = s.top_dialog_mut().unwrap();
    d.insert_text("ane Doe").unwrap();
    d.select_all();
    d.backspace().unwrap();
    assert_eq!(text(&s, "name"), "");
    let d = s.top_dialog_mut().unwrap();
    d.insert_text("abcd").unwrap();
    d.move_caret_edge(false, false);
    d.move_caret(true, true);
    d.move_caret(true, true);
    assert_eq!(d.selection(), Some((0, 2)));
    d.move_caret(true, false);
    assert_eq!(d.selection(), None);
    assert_eq!(d.caret_at(), 2, "the selection's right edge");
    d.move_caret(false, true);
    d.delete().unwrap();
    assert_eq!(text(&s, "name"), "acd");
}

/// A paste replaces the selection and keeps the caret after it; characters
/// beyond the BMP count as one.
#[test]
fn paste_inserts_at_the_caret_by_characters() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("a\u{1F600}c").unwrap();
    d.move_caret(false, false);
    d.insert_text("-").unwrap();
    assert_eq!(text(&s, "name"), "a\u{1F600}-c");
    assert_eq!(s.top().unwrap().caret_at(), 3);
}

/// A field that refuses a character keeps its text and its caret, and a value
/// set from outside (`dialog-set`) puts the caret at the end.
#[test]
fn a_refused_character_changes_nothing_and_a_set_value_moves_the_caret_to_the_end() {
    let mut s = stack();
    let n = index(&s, "percent");
    s.top_dialog_mut().unwrap().click_control(n, None).unwrap();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.type_char('4').unwrap();
    d.type_char('2').unwrap();
    d.move_caret(false, false);
    assert!(d.type_char('x').is_err());
    assert_eq!(text(&s, "percent"), "42");
    assert_eq!(s.top().unwrap().caret_at(), 1);
    s.set("percent", &args(r#"{"value":"7"}"#)).unwrap();
    assert_eq!(s.top().unwrap().caret_at(), 1, "the end of '7'");
    s.set("percent", &args(r#"{"value":"1234"}"#)).unwrap();
    assert_eq!(s.top().unwrap().caret_at(), 4);
}

/// Moving the focus puts the caret at the end of the field it lands on.
#[test]
fn the_focus_moving_resets_the_caret() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.focus_step(false);
    assert_eq!(focused_name(&s), Some("percent"));
    let d = s.top().unwrap();
    assert_eq!(d.selection(), None);
    assert_eq!(d.caret_at(), 1, "the end of '0'");
}
