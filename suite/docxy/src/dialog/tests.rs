use super::catalog;
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
        id: catalog::TEST_FORM,
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
        modeless: false,
        active: true,
        stale: false,
        seen: None,
        committed: None,
        calendar: None,
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
        note = Some(d.id.as_str());
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
        catalog::DELETE_SUMMARY,
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

/// Tab and Shift+Tab walk the editable widgets on the current page, lists
/// included (#988), skipping labels, grids, and disabled or hidden controls
/// (#649).
#[test]
fn tab_steps_through_the_editable_widgets_and_wraps() {
    let mut s = stack();
    assert_eq!(focused_name(&s), None);
    let mut order = Vec::new();
    for _ in 0..9 {
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
            "fields",
            "name"
        ]
    );
    s.top_dialog_mut().unwrap().focus_step(true);
    assert_eq!(focused_name(&s), Some("fields"));
    // Another tab has none of these: the focus goes with the page.
    s.select_tab("Predecessors").unwrap();
    assert_eq!(focused_name(&s), None);
}

/// A list is picked from (#988): a click chooses the item it lands on and
/// takes the focus; Up/Down step from the chosen item, Home/End go to the
/// first and last; each change goes through `set`. Home and End on a text
/// field still move its caret.
#[test]
fn a_list_item_is_chosen_by_click_and_by_keys() {
    let mut s = stack();
    let fields = index(&s, "fields");
    let choice = |s: &DialogStack| s.top().unwrap().controls[fields].value.clone();
    let d = s.top_dialog_mut().unwrap();
    d.controls[fields].items = vec!["Name".into(), "Start".into(), "Start".into()];
    d.click_control(fields, None).unwrap();
    assert_eq!(focused_name(&s), Some("fields"));
    assert_eq!(
        choice(&s),
        Value::Choice(None),
        "a click off the items picks none"
    );
    let d = s.top_dialog_mut().unwrap();
    d.click_control(fields, Some(2)).unwrap();
    assert_eq!(
        choice(&s),
        Value::Choice(Some(2)),
        "the item clicked, though another reads the same"
    );
    let d = s.top_dialog_mut().unwrap();
    d.step_focused(false).unwrap();
    assert_eq!(choice(&s), Value::Choice(Some(1)));
    let d = s.top_dialog_mut().unwrap();
    assert!(d.list_edge(false).unwrap());
    assert_eq!(choice(&s), Value::Choice(Some(0)));
    let d = s.top_dialog_mut().unwrap();
    d.step_focused(false).unwrap();
    assert_eq!(choice(&s), Value::Choice(Some(0)), "Up stops at the top");
    let d = s.top_dialog_mut().unwrap();
    assert!(d.list_edge(true).unwrap());
    d.step_focused(true).unwrap();
    assert_eq!(
        choice(&s),
        Value::Choice(Some(2)),
        "End, and Down stops there"
    );
    assert_eq!(
        s.top_dialog_mut()
            .unwrap()
            .click_control(fields, Some(3))
            .unwrap_err(),
        "no such item"
    );
    // On a text field Home/End are the caret's.
    let d = s.top_dialog_mut().unwrap();
    d.focus = Some(0);
    assert!(!d.list_edge(true).unwrap());
    assert_eq!(choice(&s), Value::Choice(Some(2)));
    // dialog-set takes an index too.
    s.set("fields", &args(r#"{"index":1}"#)).unwrap();
    assert_eq!(choice(&s), Value::Choice(Some(1)));
    assert!(s.set("fields", &args(r#"{"index":7}"#)).is_err());
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

/// Backspace removes a whole grapheme cluster, not one scalar: an NFD name
/// keeps no dangling letter, and a skin-tone emoji and a ZWJ family each go
/// at once (#1035).
#[test]
fn backspace_removes_a_whole_grapheme_cluster() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("Jose\u{301}").unwrap();
    d.backspace().unwrap();
    assert_eq!(text(&s, "name"), "Jos", "the accent stays with its letter");
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("a\u{1F44D}\u{1F3FD}").unwrap();
    d.backspace().unwrap();
    assert_eq!(text(&s, "name"), "a", "the skin tone stays with the hand");
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("a\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}")
        .unwrap();
    d.backspace().unwrap();
    assert_eq!(text(&s, "name"), "a", "the whole family goes at once");
}

/// Left/Right and Delete step over a grapheme cluster: a flag is two scalars
/// but one cluster, so the caret lands on 1 and 3, never inside it (#1035).
#[test]
fn delete_and_arrows_step_over_a_flag() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("a\u{1F1EB}\u{1F1F7}b").unwrap();
    let d = s.top_dialog_mut().unwrap();
    d.move_caret_edge(false, false);
    assert_eq!(d.caret_at(), 0);
    d.move_caret(true, false);
    assert_eq!(d.caret_at(), 1);
    d.move_caret(true, false);
    assert_eq!(d.caret_at(), 3, "over the flag, not into it");
    d.move_caret(false, false);
    assert_eq!(d.caret_at(), 1);
    d.delete().unwrap();
    assert_eq!(text(&s, "name"), "ab");
}

/// Shift+arrows extend the selection by whole clusters: the accented "e" of
/// "Jose\u{301}" selects as one unit (#1035).
#[test]
fn shift_arrows_select_whole_clusters() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("Jose\u{301}").unwrap();
    let d = s.top_dialog_mut().unwrap();
    d.move_caret_edge(true, false);
    d.move_caret(false, true);
    assert_eq!(d.selection(), Some((3, 5)));
    assert_eq!(d.selected_text().as_deref(), Some("e\u{301}"));
}

/// Ctrl+C copies the selected characters of the focused field, and nothing
/// without a selection (#1029).
#[test]
fn the_selection_is_what_a_copy_takes() {
    let mut s = focused_on_name();
    let d = s.top_dialog_mut().unwrap();
    d.select_all();
    d.insert_text("a\u{1F600}cd").unwrap();
    assert_eq!(d.selected_text(), None, "no selection after typing");
    d.move_caret(false, false);
    d.move_caret(false, true);
    d.move_caret(false, true);
    assert_eq!(d.selected_text().as_deref(), Some("\u{1F600}c"));
    d.select_all();
    assert_eq!(d.selected_text().as_deref(), Some("a\u{1F600}cd"));
}

/// Tab counts are what `real-key {"to-field"}` presses: from no focus the
/// first stop is one press, and a field that already has it takes a whole
/// round (#1029).
#[test]
fn tabs_to_counts_the_presses_focus_step_takes() {
    let mut s = stack();
    let d = s.top_dialog_mut().unwrap();
    d.focus = None;
    let order: Vec<&str> = d.focusable().iter().map(|&i| d.controls[i].name).collect();
    assert!(order.len() > 1, "the test form has several stops");
    for (k, name) in order.iter().enumerate() {
        let mut d = d.clone();
        let n = d.tabs_to(name).unwrap();
        assert_eq!(n, k + 1, "{name}");
        for _ in 0..n {
            d.focus_step(false);
        }
        assert_eq!(d.focused().map(|c| c.name), Some(*name));
        assert_eq!(d.tabs_to(name).unwrap(), order.len(), "a whole round");
    }
    assert!(d.tabs_to("no-such").is_err());
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

/// A grid takes the focus, selects rows and edits cells by key and click
/// (#1602); buttons carry ids `dialog-click` takes.
#[test]
fn a_grid_selects_rows_and_edits_cells_by_key_and_click() {
    let mut s = stack();
    s.select_tab("Predecessors").unwrap();
    let d = s.top_dialog_mut().unwrap();
    // A built-in grid is read-only; a plugin's is interactive (#1602).
    assert!(d.tabs_to("predecessors").is_err());
    assert!(d.select_row("predecessors", 0).is_err());
    let gi = d
        .controls
        .iter()
        .position(|c| c.name == "predecessors")
        .unwrap();
    d.controls[gi].plugin = true;
    assert!(d.tabs_to("predecessors").is_ok());
    d.select_row("predecessors", 0).unwrap();
    assert_eq!(
        d.select_row("predecessors", 1).unwrap_err(),
        "'Predecessors' has no row 1; rows 0..0"
    );
    assert!(d.select_row("name", 0).is_err());
    let i = d.focus.unwrap();
    assert_eq!(d.controls[i].row, Some(0));
    // Right moves to the Type column; typing edits it; Enter commits the
    // text through the input handler.
    assert!(d.grid_key("right", None).unwrap());
    assert_eq!(d.controls[i].col, 1);
    assert!(d.grid_key("s", Some("S")).unwrap());
    assert!(d.grid_key("s", Some("S")).unwrap());
    assert_eq!(d.controls[i].edit.as_deref(), Some("SS"));
    // A grid whose commits send the command records the cell (#1602).
    d.controls[i].commit_command = true;
    assert!(d.grid_key("enter", None).unwrap());
    assert_eq!(d.controls[i].edit, None);
    assert_eq!(d.committed.take(), Some((i, 0, 1, "SS".to_string())));
    d.controls[i].commit_command = false;
    assert_eq!(
        d.value("predecessors"),
        Some(&Value::Rows(vec![vec!["1".into(), "SS".into()]]))
    );
    // Backspace shortens the cell, Escape drops the edit; with no edit
    // open Enter and Escape are the dialog's.
    assert!(d.grid_key("backspace", None).unwrap());
    assert_eq!(d.controls[i].edit.as_deref(), Some("S"));
    assert!(d.grid_key("escape", None).unwrap());
    assert_eq!(d.controls[i].edit, None);
    assert!(!d.grid_key("escape", None).unwrap());
    assert!(!d.grid_key("enter", None).unwrap());
    assert!(!d.grid_key("tab", None).unwrap());
    // Delete clears the cell; Up and Down stay in the rows.
    assert!(d.grid_key("delete", None).unwrap());
    assert_eq!(
        d.value("predecessors"),
        Some(&Value::Rows(vec![vec!["1".into(), String::new()]]))
    );
    assert!(d.grid_key("down", None).unwrap());
    assert_eq!(d.controls[i].row, Some(0));
    // A click on a cell selects its row and column; off the rows it is
    // refused.
    d.click_control(i, Some(0)).unwrap();
    assert_eq!((d.controls[i].row, d.controls[i].col), (Some(0), 0));
    assert!(d.click_control(i, Some(5 * 256)).is_err());
    // `dialog-read` says so; buttons carry ids, which `click` takes too.
    let json = s.to_json();
    let grid = &json.get("controls").unwrap().as_array().unwrap()[0];
    assert_eq!(grid.get("selected_row"), Some(&Json::Num(0.0)));
    assert_eq!(grid.get("column"), Some(&Json::Num(0.0)));
    assert_eq!(grid.get("editing"), Some(&Json::Null));
    assert_eq!(json.get("modeless"), Some(&Json::Bool(false)));
    assert_eq!(json.get("active"), Some(&Json::Bool(true)));
    let buttons = json.get("buttons").unwrap().as_array().unwrap();
    assert_eq!(buttons[0].get_str("id"), Some("ok"));
    assert_eq!(buttons[1].get_str("id"), Some("cancel"));
    assert!(s.click("ok", |_| Ok(())).is_ok());
    assert!(!s.is_open());
}

/// A modeless dialog lets the keys through while it is not active, and a
/// mutating edit under it keeps it (#1602).
#[test]
fn a_modeless_dialog_keeps_the_keys_only_while_active_and_survives_clear_modal() {
    let mut s = stack();
    assert!(s.modal_open());
    assert!(s.top_takes_keys());
    s.top_dialog_mut().unwrap().modeless = true;
    assert!(!s.modal_open());
    assert!(s.top_takes_keys());
    s.activate(false);
    assert!(!s.top_takes_keys());
    s.activate(true);
    assert!(s.top_takes_keys());
    s.clear_modal();
    assert!(s.is_open());
    assert!(s.top().unwrap().stale);
    s.top_dialog_mut().unwrap().modeless = false;
    s.clear_modal();
    assert!(!s.is_open());
}

/// A grid the person edits (#1536): a combo column offering items, a
/// dropdown column, and a free one.
fn link_grid() -> Dialog {
    let mut d = form();
    let mut g = Control::new(
        "links",
        "Links",
        ControlKind::Grid,
        Value::Rows(vec![
            vec!["2".into(), "Design".into(), "Finish-to-Start (FS)".into()],
            vec![String::new(); 3],
        ]),
    );
    g.page = Some(1);
    g.columns = vec!["ID".into(), "Task Name".into(), "Type".into()];
    g.column_kinds = vec![ControlKind::Text, ControlKind::Combo, ControlKind::Dropdown];
    g.column_items = vec![
        Vec::new(),
        vec!["Design".into(), "Build".into(), "Deploy".into()],
        vec![
            "Finish-to-Start (FS)".into(),
            "Start-to-Start (SS)".into(),
            "Finish-to-Finish (FF)".into(),
        ],
    ];
    g.editable_grid = true;
    d.controls.push(g);
    d.tab = 1;
    d.mark_opened();
    d
}

fn rows(d: &Dialog) -> Vec<Vec<String>> {
    match d.value("links") {
        Some(Value::Rows(r)) => r.clone(),
        other => panic!("not rows: {other:?}"),
    }
}

#[test]
fn an_editable_grid_is_reached_by_tab_and_keeps_one_blank_row() {
    let mut d = link_grid();
    assert_eq!(d.tabs_to("links"), Ok(1));
    assert!(!d.changed("links"));
    d.select_row("links", 1).unwrap();
    // Typing a cell of the blank row makes the next blank row.
    d.set("links[1].id", &args(r#"{"value":"3"}"#)).unwrap();
    assert_eq!(rows(&d).len(), 3);
    assert!(rows(&d)[2].iter().all(String::is_empty));
    assert!(d.changed("links"));
    // Clearing it again leaves a single blank row, as opened.
    d.set("links[1].id", &args(r#"{"value":""}"#)).unwrap();
    assert_eq!(rows(&d).len(), 2);
    assert!(!d.changed("links"));
}

#[test]
fn a_dropdown_column_takes_an_item_or_the_only_match() {
    let mut d = link_grid();
    d.set("links[1].type", &args(r#"{"value":"ss"}"#)).unwrap();
    assert_eq!(rows(&d)[1][2], "Start-to-Start (SS)");
    d.set(
        "links[1].type",
        &args(r#"{"value":"FINISH-TO-FINISH (ff)"}"#),
    )
    .unwrap();
    assert_eq!(rows(&d)[1][2], "Finish-to-Finish (FF)");
    let both = d
        .set("links[1].type", &args(r#"{"value":"finish"}"#))
        .unwrap_err();
    assert!(both.contains("more than one"), "{both}");
    let none = d
        .set("links[1].type", &args(r#"{"value":"zz"}"#))
        .unwrap_err();
    assert!(none.contains("choose Finish-to-Start (FS)"), "{none}");
    // Blank clears it.
    d.set("links[1].type", &args(r#"{"value":""}"#)).unwrap();
}

#[test]
fn a_combo_cell_narrows_its_list_while_typed_into_and_takes_the_pick() {
    let mut d = link_grid();
    d.select_row("links", 1).unwrap();
    let i = d.focus.unwrap();
    d.controls[i].col = 1;
    assert!(d.controls[i].suggestions().is_empty(), "nothing typed yet");
    d.grid_key("d", Some("d")).unwrap();
    assert_eq!(d.controls[i].suggestions(), vec![0, 1, 2], "each has a d");
    d.grid_key("e", Some("e")).unwrap();
    assert_eq!(d.controls[i].suggestions(), vec![0, 2], "Design, Deploy");
    d.grid_key("p", Some("p")).unwrap();
    assert_eq!(d.controls[i].suggestions(), vec![2]);
    // The arrows reach into the list, not the rows; Enter takes the pick.
    assert!(d.grid_key("down", None).unwrap());
    assert_eq!((d.controls[i].row, d.controls[i].pick), (Some(1), Some(0)));
    assert!(d.grid_key("enter", None).unwrap());
    assert_eq!(rows(&d)[1][1], "Deploy");
    assert_eq!(d.controls[i].edit, None);
    // A click on a listed item commits it as well.
    d.grid_key("b", Some("b")).unwrap();
    d.click_control(i, Some(SUGGESTION_BASE)).unwrap();
    assert_eq!(rows(&d)[1][1], "Build");
    // A name that is no item stays as typed (the owner resolves it on OK).
    d.set("links[1].task name", &args(r#"{"value":"Launch"}"#))
        .unwrap();
    assert_eq!(rows(&d)[1][1], "Launch");
}

#[test]
fn space_and_a_click_step_a_dropdown_cell_and_delete_removes_the_row() {
    let mut d = link_grid();
    d.select_row("links", 0).unwrap();
    let i = d.focus.unwrap();
    d.controls[i].col = 2;
    d.grid_key("space", Some(" ")).unwrap();
    assert_eq!(rows(&d)[0][2], "Start-to-Start (SS)");
    d.click_control(i, Some(2)).unwrap();
    assert_eq!(rows(&d)[0][2], "Finish-to-Finish (FF)");
    d.grid_key("space", Some(" ")).unwrap();
    assert_eq!(rows(&d)[0][2], "Finish-to-Start (FS)", "wraps around");
    // Delete clears the whole row, leaving the blank one.
    assert!(d.grid_key("delete", None).unwrap());
    assert_eq!(rows(&d), vec![vec![String::new(); 3]]);
    assert!(d.changed("links"));
}

#[test]
fn a_date_combo_types_like_a_date_and_drops_a_calendar_down() {
    let mut d = form();
    let mut c = Control::new(
        "finish",
        "Finish:",
        ControlKind::DateCombo,
        Value::Text("2026-10-18T17:00".into()),
    );
    c.page = Some(0);
    d.controls.push(c);
    let i = d.controls.len() - 1;
    let today = projcore::DateTime::from_ymd_hm(2026, 3, 9, 0, 0);
    assert!(ControlKind::DateCombo.is_text());
    d.set("finish", &args(r#"{"value":"2026-10-20"}"#)).unwrap();
    // Opening it focuses the field and shows the field's month.
    d.toggle_calendar_on(i, today).unwrap();
    assert_eq!(d.focus, Some(i));
    let k = d.calendar.unwrap();
    assert_eq!((k.year(), k.month(), k.day()), (2026, 10, 20));
    // The arrows move the cursor, Page Down a month; a key it leaves
    // alone stays the calendar's.
    d.calendar_key("right").unwrap();
    d.calendar_key("up").unwrap();
    assert_eq!(d.calendar.unwrap().day(), 14);
    d.calendar_key("pagedown").unwrap();
    assert_eq!(d.calendar.unwrap().text(), "2026-11-14");
    // Escape closes it without a change; reopened, Enter picks.
    assert!(d.calendar_key("escape").unwrap());
    assert!(d.calendar.is_none());
    assert_eq!(text_of_control(&d, "finish"), "2026-10-20");
    d.toggle_calendar_on(i, today).unwrap();
    d.calendar_key("down").unwrap();
    d.calendar_key("enter").unwrap();
    assert!(d.calendar.is_none());
    assert_eq!(text_of_control(&d, "finish"), "2026-10-27");
    // A click on a day picks it; the button toggles; a blank field opens on today.
    d.toggle_calendar_on(i, today).unwrap();
    d.calendar_move(-1, true).unwrap();
    d.calendar_day(3, true).unwrap();
    assert_eq!(text_of_control(&d, "finish"), "2026-09-03");
    assert!(d.calendar_day(3, true).is_err(), "no calendar open");
    d.set("finish", &args(r#"{"value":""}"#)).unwrap();
    d.toggle_calendar_on(i, today).unwrap();
    assert_eq!(d.calendar.unwrap().text(), "2026-03-09");
    d.toggle_calendar_on(i, today).unwrap();
    assert!(d.calendar.is_none());
    // Not a date combo: no calendar. Tab closes it and moves on.
    assert!(d.toggle_calendar_on(0, today).is_err());
    d.toggle_calendar_on(i, today).unwrap();
    assert!(!d.calendar_key("tab").unwrap());
    assert!(d.calendar.is_none());
}

#[test]
fn an_open_calendar_keeps_enter_and_escape_from_the_buttons() {
    let mut s = stack();
    {
        let d = s.top_dialog_mut().unwrap();
        let mut c = Control::new(
            "finish",
            "Finish:",
            ControlKind::DateCombo,
            Value::Text(String::new()),
        );
        c.page = Some(0);
        d.controls.push(c);
        let i = d.controls.len() - 1;
        d.toggle_calendar(i).unwrap();
    }
    assert_eq!(s.key_button("enter", true), None);
    assert_eq!(s.key_button("escape", true), None);
    let json = s.to_json();
    assert_eq!(json.get("calendar_open"), Some(&Json::Bool(true)));
    assert_eq!(
        json.get("calendar").unwrap().get_str("control"),
        Some("finish")
    );
    s.top_dialog_mut().unwrap().calendar_key("escape").unwrap();
    assert_eq!(s.key_button("enter", true), Some("OK".to_string()));
    assert_eq!(s.to_json().get("calendar_open"), Some(&Json::Bool(false)));
}

fn text_of_control(d: &Dialog, name: &str) -> String {
    d.controls
        .iter()
        .find(|c| c.name == name)
        .map(Control::text)
        .unwrap_or_default()
}

// ---- FIX r1 (#1536) ----

/// A plugin's grid (no `editable_grid`) with a dropdown and a label column
/// behaves as it did before editable grids: nothing is resolved against
/// items it never had, Space types, and a label cell takes `dialog-set`.
#[test]
fn a_plugin_grid_with_dropdown_and_label_columns_is_unchanged() {
    let mut d = link_grid();
    let i = d.controls.iter().position(|c| c.name == "links").unwrap();
    d.controls[i].editable_grid = false;
    d.controls[i].plugin = true;
    d.controls[i].column_items = Vec::new();
    d.controls[i].column_kinds = vec![ControlKind::Text, ControlKind::Label, ControlKind::Dropdown];
    d.set("links[0].type", &args(r#"{"value":"anything"}"#))
        .unwrap();
    d.set("links[0].task name", &args(r#"{"value":"shown"}"#))
        .unwrap();
    assert_eq!(rows(&d)[0][1..], ["shown", "anything"]);
    assert_eq!(rows(&d).len(), 2, "no blank row is kept for it");
    d.select_row("links", 0).unwrap();
    d.controls[i].col = 2;
    d.grid_key("space", Some(" ")).unwrap();
    assert_eq!(d.controls[i].edit.as_deref(), Some(" "), "Space types");
    d.grid_key("escape", None).unwrap();
    d.controls[i].col = 1;
    d.grid_key("x", Some("x")).unwrap();
    assert_eq!(
        d.controls[i].edit.as_deref(),
        Some("x"),
        "a label cell types"
    );
    // Delete clears the cell, and a pending edit is not committed by Down.
    d.grid_key("escape", None).unwrap();
    d.grid_key("delete", None).unwrap();
    assert_eq!(rows(&d)[0][1], "");
    d.grid_key("y", Some("y")).unwrap();
    d.grid_key("down", None).unwrap();
    assert_eq!(rows(&d)[0][1], "");
}

#[test]
fn a_calendar_closes_when_the_focus_moves_and_leaves_other_keys_to_the_field() {
    let mut d = form();
    let mut c = Control::new(
        "finish",
        "Finish:",
        ControlKind::DateCombo,
        Value::Text("2026-10-20".into()),
    );
    c.page = Some(0);
    d.controls.push(c);
    let i = d.controls.len() - 1;
    d.toggle_calendar(i).unwrap();
    // A key it does not handle goes on to the field.
    assert!(!d.calendar_key("a").unwrap());
    assert!(d.calendar.is_none(), "a key it does not handle closes it");
    d.toggle_calendar(i).unwrap();
    assert!(!d.calendar_key("backspace").unwrap());
    d.toggle_calendar(i).unwrap();
    // A click on another control closes it.
    d.click_control(0, None).unwrap();
    assert!(d.calendar.is_none());
    assert!(!d.calendar_key("enter").unwrap());
    assert_eq!(text_of_control(&d, "finish"), "2026-10-20");
    // So does selecting a grid row.
    let mut g = link_grid();
    g.controls.push(Control {
        page: Some(1),
        ..Control::new(
            "when",
            "When:",
            ControlKind::DateCombo,
            Value::Text(String::new()),
        )
    });
    let w = g.controls.len() - 1;
    g.toggle_calendar(w).unwrap();
    g.select_row("links", 0).unwrap();
    assert!(g.calendar.is_none());
}

#[test]
fn a_combo_keeps_the_typed_name_unless_it_is_an_item_and_a_dropdown_completes() {
    let mut d = link_grid();
    d.set("links[1].task name", &args(r#"{"value":"Des"}"#))
        .unwrap();
    assert_eq!(rows(&d)[1][1], "Des", "a part of Design stays as typed");
    d.set("links[1].task name", &args(r#"{"value":"DESIGN"}"#))
        .unwrap();
    assert_eq!(rows(&d)[1][1], "Design", "an exact name, case aside");
    d.set("links[1].type", &args(r#"{"value":"ss"}"#)).unwrap();
    assert_eq!(rows(&d)[1][2], "Start-to-Start (SS)");
}

#[test]
fn a_pending_cell_edit_is_committed_by_the_arrows_a_click_tab_and_ok() {
    let mut s = DialogStack::default();
    s.push(link_grid());
    let d = s.top_dialog_mut().unwrap();
    d.select_row("links", 1).unwrap();
    let i = d.focus.unwrap();
    d.controls[i].col = 0;
    d.grid_key("7", Some("7")).unwrap();
    d.grid_key("up", None).unwrap();
    assert_eq!(rows(d)[1][0], "7", "Up commits");
    assert_eq!(d.controls[i].edit, None);
    d.select_row("links", 1).unwrap();
    d.grid_key("down", None).unwrap();
    d.controls[i].col = 0;
    d.select_row("links", 2).unwrap();
    d.grid_key("8", Some("8")).unwrap();
    d.click_control(i, Some(0)).unwrap();
    assert_eq!(rows(d)[2][0], "8", "a click on another cell commits");
    d.grid_key("9", Some("9")).unwrap();
    let before = d.controls[i].edit.clone();
    assert_eq!(before.as_deref(), Some("9"));
    // OK commits it, and a refused one keeps the dialog open.
    let mut seen = None;
    s.click("ok", |d| {
        seen = Some(rows(d)[0][0].clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(seen.as_deref(), Some("9"));
    let mut s = DialogStack::default();
    let mut d = link_grid();
    d.select_row("links", 1).unwrap();
    let i = d.focus.unwrap();
    d.controls[i].col = 2;
    d.grid_key("z", Some("z")).unwrap();
    s.push(d);
    assert!(s.click("ok", |_| Ok(())).is_err());
    assert!(s.is_open());
    assert_eq!(s.top().unwrap().controls[i].edit.as_deref(), Some("z"));
}

#[test]
fn the_combo_pick_stays_within_the_drawn_items() {
    let mut d = link_grid();
    let i = d.controls.iter().position(|c| c.name == "links").unwrap();
    d.controls[i].column_items[1] = (0..9).map(|n| format!("Task {n}")).collect();
    d.select_row("links", 1).unwrap();
    d.controls[i].col = 1;
    d.grid_key("t", Some("t")).unwrap();
    d.grid_key("up", None).unwrap();
    assert_eq!(d.controls[i].pick, Some(5));
    for _ in 0..9 {
        d.grid_key("down", None).unwrap();
    }
    assert_eq!(d.controls[i].pick, Some(5));
}

// ---- FIX r2 (#1536) ----

#[test]
fn down_after_a_commit_that_grows_or_shrinks_the_rows_uses_the_new_count() {
    let mut d = link_grid();
    d.select_row("links", 1).unwrap();
    let i = d.focus.unwrap();
    d.controls[i].col = 0;
    // Typing in the blank last row, then Down: the row it made stays, and
    // the cursor moves to the new blank one.
    d.grid_key("5", Some("5")).unwrap();
    d.grid_key("down", None).unwrap();
    assert_eq!(rows(&d)[1][0], "5");
    assert_eq!(d.controls[i].row, Some(2));
    d.grid_key("6", Some("6")).unwrap();
    d.grid_key("down", None).unwrap();
    assert_eq!(
        (rows(&d)[1][0].as_str(), rows(&d)[2][0].as_str()),
        ("5", "6")
    );
    // A commit that shrinks the grid does not carry Down past its end.
    d.select_row("links", 2).unwrap();
    d.controls[i].col = 0;
    d.grid_key("7", Some("7")).unwrap();
    d.grid_key("backspace", None).unwrap();
    d.grid_key("down", None).unwrap();
    assert!(d.controls[i].row.unwrap() < rows(&d).len());
    // select_row counts the rows after the commit too: typing in the blank
    // last row makes one more, which can then be selected.
    d.select_row("links", rows(&d).len() - 1).unwrap();
    d.controls[i].col = 0;
    d.grid_key("9", Some("9")).unwrap();
    let n = rows(&d).len();
    d.select_row("links", n).unwrap();
    assert_eq!(d.controls[i].row, Some(n));
}

#[test]
fn a_typed_date_is_not_overwritten_by_an_open_calendar() {
    let mut d = form();
    let mut c = Control::new(
        "finish",
        "Finish:",
        ControlKind::DateCombo,
        Value::Text("2026-10-20".into()),
    );
    c.page = Some(0);
    d.controls.push(c);
    let i = d.controls.len() - 1;
    d.toggle_calendar(i).unwrap();
    assert!(!d.calendar_key("1").unwrap());
    assert!(d.calendar.is_none(), "typing closes the calendar");
    d.select_all();
    d.type_char('x').unwrap();
    assert!(!d.calendar_key("enter").unwrap());
    assert_eq!(text_of_control(&d, "finish"), "x");
}
