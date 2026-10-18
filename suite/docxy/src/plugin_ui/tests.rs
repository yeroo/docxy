use super::*;
use crate::dialog::{ControlKind, Value, grid_address};
use crate::menu::MenuItem;
use projcore::uiplugin::Manifest;

const MANIFEST: &str = r##"{
  "ribbon":[{"tab":"Task","groups":[
    {"title":"Fixture","controls":[
      {"id":"fx.hello","kind":"button","label":"Hello","tip":"Say hello","keytip":"XH","icon":"comment","large":true},
      {"id":"fx.a","kind":"button","label":"A"},
      {"id":"fx.b","kind":"button","label":"B"},
      {"id":"fx.c","kind":"button","label":"C"},
      {"id":"fx.d","kind":"button","label":"D"},
      {"id":"fx.flag","kind":"toggle","label":"Flag"},
      {"id":"fx.palette","kind":"gallery","label":"Palette","swatches":[
        {"id":"fx.blue","label":"Blue","color":"#3366CC"},{"id":"fx.red","label":"Red","color":"#CC3333"}]}]},
    {"title":"Schedule","controls":[{"id":"fx.in-schedule","kind":"button","label":"Extra"}]},
    {"title":"Anywhere","controls":[{"id":"pr-indent","kind":"button","label":"Indent (plugin)","tip":"Replaced","keytip":"ZZ"}]},
    {"title":"Placed","before":"Schedule","controls":[{"id":"fx.placed","kind":"button","label":"Placed"}]},
    {"title":"Trailing","after":"Clipboard","controls":[{"id":"fx.trailing","kind":"button","label":"Trailing"}]}]}],
  "menus":[{"menu":"row","items":[
    {"id":"fx.rename","label":"Rename (fixture)","after":"Notes..."},
    {"id":"rm-hyperlink","label":"Hyperlink (plugin)"},
    {"id":"fx.tail","label":"Tail"}]}],
  "dialogs":[{"id":"task-information","pages":[
    {"name":"Fixture","controls":[
      {"name":"fx.note","label":"&Note:","kind":"multiline","value":"default"},
      {"name":"fx.grid","label":"Items","kind":"grid","columns":[{"name":"Name","kind":"text"},{"name":"Done","kind":"checkbox"}],"value":[["a","false"]]}]},
    {"name":"Notes","controls":[{"name":"fx.notes2","label":"Notes:","kind":"text"}]}]}],
  "palettes":[{"id":"fx.blue","label":"Blue","bars":[{"kind":"on-track","color":"#3366CC","shape":"bar"}]}]
}"##;

fn manifest() -> Manifest {
    Manifest::parse(MANIFEST).unwrap()
}

/// A Project tab with two tasks, the second selected.
fn tab() -> DocTab {
    let mut t = crate::new_project_tab("Project1");
    let Surface::Project(v) = &mut t.surface else {
        unreachable!()
    };
    for (name, duration) in [("First", 480), ("Second", 960)] {
        v.ed.add_task(None, name, duration, false).unwrap();
    }
    v.ed.select(1);
    t
}

#[test]
fn the_view_json_carries_the_text_styles_and_the_selected_cells_styles() {
    let mut tab = tab();
    let Surface::Project(v) = &mut tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    let other = v.ed.project().tasks[1].uid;
    v.ed.edit_formatting(|f| {
        f.set_category(
            "Critical Tasks",
            projcore::formatting::TextStyle {
                bold: Some(true),
                color: Some(0xCC0000),
                ..Default::default()
            },
        );
        for u in [uid, other] {
            f.set_cell(
                u,
                "Name",
                projcore::formatting::TextStyle {
                    italic: Some(true),
                    ..Default::default()
                },
            );
        }
    });
    // The cursor is on the first task's Name cell: its style is sent, the
    // other task's is not; every category is.
    v.entry = false;
    v.ed.select(0);
    v.col = crate::COLUMNS.iter().position(|c| *c == "Name").unwrap();
    let view = view_json(v);
    let styles = view.get("text_styles").unwrap();
    assert_eq!(
        styles.get("Critical Tasks").unwrap().get("bold"),
        Some(&Json::Bool(true))
    );
    assert_eq!(
        styles.get("Critical Tasks").unwrap().get_str("color"),
        Some("#CC0000")
    );
    let cells = view.list("cells");
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].int("uid"), Some(i64::from(uid)));
    assert_eq!(cells[0].get_str("column"), Some("Name"));
    assert_eq!(cells[0].get("italic"), Some(&Json::Bool(true)));
    // Another column: no cell style there.
    v.col = crate::COLUMNS
        .iter()
        .position(|c| *c == "Duration")
        .unwrap();
    assert!(view_json(v).list("cells").is_empty());
}

fn ids(tab: &rs::Tab<Act>) -> Vec<(String, Vec<String>)> {
    tab.groups
        .iter()
        .map(|g| {
            let mut cmds = Vec::new();
            for c in &g.items {
                match c {
                    Control::Large(c) | Control::Toggle(c) => cmds.push(c.id.to_string()),
                    Control::Column(cs) => cmds.push(format!(
                        "[{}]",
                        cs.iter().map(|c| c.id).collect::<Vec<_>>().join(",")
                    )),
                    Control::Gallery(g) => cmds.push(format!("gallery:{}", g.id)),
                    Control::Split { primary, .. } => cmds.push(format!("split:{}", primary.id)),
                    Control::Dropdown { cmd, .. } => cmds.push(format!("drop:{}", cmd.id)),
                    Control::Rows(_) | Control::Separator => {}
                }
            }
            (g.title.to_string(), cmds)
        })
        .collect()
}

#[test]
fn contributions_append_merge_and_replace_in_place() {
    let m = manifest();
    let mut ribbon = crate::project_ribbon();
    let task = ribbon.tabs.iter_mut().find(|t| t.name == "Task").unwrap();
    let before = ids(task);
    extend_tab_with(task, &[&m]);
    let after = ids(task);
    // The built-in groups keep their order; "Fixture" is appended after
    // them; "Schedule" gained a control; no "Anywhere" group was made,
    // because its one control replaced a built-in in place; "Placed" goes
    // before Schedule and "Trailing" after Clipboard (#1574).
    assert_eq!(after.len(), before.len() + 3);
    assert_eq!(after.last().unwrap().0, "Fixture");
    let titles: Vec<&str> = after.iter().map(|(t, _)| t.as_str()).collect();
    let at = |t: &str| titles.iter().position(|x| *x == t).unwrap();
    assert_eq!(at("Placed") + 1, at("Schedule"));
    assert_eq!(at("Clipboard") + 1, at("Trailing"));
    assert_eq!(
        after.last().unwrap().1,
        [
            "fx.hello",
            "[fx.a,fx.b,fx.c]",
            "[fx.d]",
            "fx.flag",
            "gallery:fx.palette"
        ]
    );
    let schedule = after.iter().find(|(t, _)| t == "Schedule").unwrap();
    assert_eq!(schedule.1.last().unwrap(), "[fx.in-schedule]");
    assert!(!after.iter().any(|(t, _)| t == "Anywhere"));
    let indent = task
        .groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find_map(|c| match c {
            Control::Column(cs) => cs.iter().find(|c| c.id == "pr-indent"),
            _ => None,
        })
        .unwrap();
    assert_eq!(indent.label, "Indent (plugin)");
    assert_eq!(indent.tip.title, "Replaced");
    assert_eq!(indent.key_tip, "ZZ");
    assert!(is_plugin(indent.act));
    // The built-in Indent is still in its place, as the same column's row.
    let schedule_before = before.iter().find(|(t, _)| t == "Schedule").unwrap();
    assert_eq!(schedule.1.len(), schedule_before.1.len() + 1);
    // Acts, tips and the gallery's swatch colours.
    let hello = task
        .groups
        .last()
        .and_then(|g| match &g.items[0] {
            Control::Large(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(hello.act, act_of("fx.hello"));
    assert_eq!(
        (hello.tip.title, hello.key_tip, hello.icon.0),
        ("Say hello", "XH", "comment")
    );
    let gallery = task
        .groups
        .last()
        .and_then(|g| match g.items.last() {
            Some(Control::Gallery(g)) => Some(g),
            _ => None,
        })
        .unwrap();
    assert_eq!(gallery.items[0].preview, "#3366CC");
    assert_eq!(swatch_color(gallery.items[0].preview), Some(0x3366CC));
    assert_eq!(swatch_color("h1"), None);
    // Interning keeps one copy.
    assert!(std::ptr::eq(intern("fx.hello"), intern("fx.hello")));
}

#[test]
fn menu_items_go_after_their_hint_replace_by_id_or_append() {
    let m = manifest();
    let tab = tab();
    let Surface::Project(v) = &tab.surface else {
        unreachable!()
    };
    let mut items = crate::project_row_menu(v, v.selected_uid());
    // No plugin is loaded in a test: the built-in menu, unchanged.
    let built_in = items.len();
    extend_menu_with("row", &mut items, &[&m], |id| {
        (id != "fx.tail", id == "fx.rename")
    });
    let labels: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Item(e) => Some(e.label.clone()),
            MenuItem::Separator => Some("-".into()),
            _ => None,
        })
        .collect();
    let at = |label: &str| labels.iter().position(|l| l == label).unwrap();
    assert_eq!(at("Rename (fixture)"), at("Notes...") + 1);
    assert!(!labels.iter().any(|l| l == "Hyperlink..."));
    assert_eq!(labels.last().unwrap(), "Tail");
    assert_eq!(labels[labels.len() - 2], "-");
    // One inserted, one replaced, one appended with its separator.
    assert_eq!(items.len(), built_in + 3);
    let entry = |label: &str| {
        items
            .iter()
            .find_map(|i| match i {
                MenuItem::Item(e) if e.label == label => Some(e.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert!(entry("Rename (fixture)").plugin && entry("Rename (fixture)").checked);
    assert_eq!(entry("Hyperlink (plugin)").id, "rm-hyperlink");
    assert!(entry("Hyperlink (plugin)").plugin);
    assert!(!entry("Tail").enabled);
    assert_eq!(entry("Tail").act, Some(act_of("fx.tail")));
    assert!(!entry("Cut").plugin);
}

#[test]
fn grid_cells_are_addressed_as_name_row_column() {
    assert_eq!(
        grid_address("fx.grid[1].Done"),
        Some(("fx.grid", 1, "Done"))
    );
    assert_eq!(grid_address("g[0].2"), Some(("g", 0, "2")));
    assert_eq!(grid_address("fx.grid"), None);
    assert_eq!(grid_address("[1].x"), None);
    assert_eq!(grid_address("g[x].Done"), None);
    assert_eq!(grid_address("g[1]"), None);
    assert_eq!(grid_address("g[1]."), None);
}

#[test]
fn task_information_pages_replace_by_name_or_follow_and_take_the_prefill() {
    let m = manifest();
    let spec = m.dialog("task-information").unwrap();
    let tab = tab();
    let Surface::Project(v) = &tab.surface else {
        unreachable!()
    };
    let ed = &v.ed;
    let uid = ed.project().tasks[0].uid;
    let mut d = crate::task_info_dialog(ed, uid, crate::InfoPage::General).unwrap();
    let built_in_tabs = d.tabs.clone();
    let built_in_notes = d.controls.iter().filter(|c| c.page == Some(4)).count();
    assert_eq!(built_in_notes, 1);
    let values = Prefill {
        values: vec![("fx.note".to_string(), Json::str("from the plugin"))],
        ..Prefill::default()
    };
    extend_task_info_with(&mut d, spec, &values);
    d.mark_opened();
    // "Fixture" is a new tab after the built-in five; "Notes" replaced the
    // built-in Notes page's controls.
    assert_eq!(d.tabs.len(), built_in_tabs.len() + 1);
    assert_eq!(d.tabs[5], "Fixture");
    let notes: Vec<&str> = d
        .controls
        .iter()
        .filter(|c| c.page == Some(4))
        .map(|c| c.name)
        .collect();
    assert_eq!(notes, ["fx.notes2"]);
    let note = d.controls.iter().find(|c| c.name == "fx.note").unwrap();
    assert_eq!(note.kind, ControlKind::Multiline);
    assert_eq!(note.value, Value::Text("from the plugin".into()));
    assert!(note.plugin);
    assert_eq!(note.page, Some(5));
    let grid = d.controls.iter().find(|c| c.name == "fx.grid").unwrap();
    assert_eq!(grid.kind, ControlKind::Grid);
    assert_eq!(grid.columns, ["Name", "Done"]);
    assert_eq!(
        grid.column_kinds,
        [ControlKind::Text, ControlKind::Checkbox]
    );
    assert_eq!(
        grid.value,
        Value::Rows(vec![vec!["a".into(), "false".into()]])
    );
    // The catalogue check leaves the plugin's controls out; here it still
    // fails, because the plugin replaced the built-in Notes page, whose
    // `notes` field the catalogue lists (the fixture adds a page and
    // replaces none, so `project-plugin-ui.uit`'s check passes).
    let check = d.catalog_check().unwrap_err();
    assert!(
        check.contains("notes (text, Notes)") && !check.contains("fx."),
        "{check}"
    );
    // The grid's cells by address, with the column's kind checked.
    d.select_tab("Fixture").unwrap();
    d.set(
        "fx.grid[0].Done",
        &ctlcore::json::Json::obj(vec![("value", ctlcore::json::Json::Bool(true))]),
    )
    .unwrap();
    assert_eq!(
        d.value("fx.grid"),
        Some(&Value::Rows(vec![vec!["a".into(), "true".into()]]))
    );
    let refused = d
        .set(
            "fx.grid[0].Done",
            &ctlcore::json::Json::obj(vec![("value", ctlcore::json::Json::Str("maybe".into()))]),
        )
        .unwrap_err();
    assert!(refused.contains("takes true or false"), "{refused}");
    d.set(
        "fx.grid[0].1",
        &ctlcore::json::Json::obj(vec![("value", ctlcore::json::Json::Str("false".into()))]),
    )
    .unwrap();
    let refused = d
        .set("fx.note[0].x", &ctlcore::json::Json::obj(vec![]))
        .unwrap_err();
    assert!(refused.contains("is not a grid"), "{refused}");
    // The values the plugin gets back: the plugin's controls only.
    let values = values_of(&d);
    assert_eq!(values.text("fx.note"), "from the plugin");
    assert_eq!(
        values.get("fx.grid"),
        Some(&Json::Arr(vec![Json::strs(["a", "false"])]))
    );
    assert!(values.get("name").is_none());
}

#[test]
fn a_rows_categories_run_general_to_specific() {
    // A zero-duration task is a milestone: give this one a day.
    let mut task = projcore::Task {
        uid: 3,
        duration_min: 480,
        ..projcore::Task::default()
    };
    assert_eq!(
        row_categories(&task, true, false),
        ["All", "Critical Tasks"]
    );
    task.summary = true;
    task.manual = true;
    assert_eq!(
        row_categories(&task, false, true),
        [
            "All",
            "Summary Tasks",
            "Manually Scheduled Tasks",
            "Inactive Tasks"
        ]
    );
    task.uid = 0;
    task.manual = false;
    assert_eq!(
        row_categories(&task, false, false),
        ["All", "Project Summary Task"]
    );
    let mut milestone = projcore::Task::default();
    milestone.milestone = true;
    assert_eq!(
        row_categories(&milestone, true, false),
        ["All", "Milestone Tasks"]
    );
}

#[test]
fn the_palette_is_the_manifests_by_id() {
    let m = manifest();
    assert_eq!(
        palette_with("fx.blue", &[&m]).unwrap().bars[0].color,
        0x3366CC
    );
    assert!(palette_with("fx.green", &[&m]).is_none());
    assert!(palette_with("fx.blue", &[]).is_none());
}

#[test]
fn help_is_the_hosts_never_the_default_and_never_a_command() {
    // #1609: a spec's `help` button loses `default`, so Enter cannot name
    // it, and a press through the dialog path says the documentation is
    // coming without sending the plugin anything; a disabled one refuses.
    let spec = ui::DialogButton {
        id: "help".into(),
        label: "Help".into(),
        default: true,
        cancel: false,
    };
    let b = button_of_spec(&spec, dialog::ButtonRole::Apply);
    assert!(!b.default && b.id == "help");
    // Nor the cancel button: Escape names nothing when Help is the only
    // one a spec marked cancel.
    let cancelling = ui::DialogButton {
        cancel: true,
        default: false,
        ..spec.clone()
    };
    let c = button_of_spec(&cancelling, dialog::ButtonRole::Cancel);
    assert_eq!(c.role, dialog::ButtonRole::Apply);
    let ok = button_of_spec(&ui::DialogButton::named("OK"), dialog::ButtonRole::Accept);
    assert!(ok.default);
    let mut t = tab();
    let mut d = Dialog::message(
        catalog::PLUGIN,
        "Fixture",
        String::new(),
        &[],
        DialogOwner::Plugin {
            command: intern("fixture.hello"),
            spec: intern("fixture.options"),
        },
    );
    d.text = None;
    d.buttons = vec![
        dialog::Button::new("OK", dialog::ButtonRole::Accept),
        button_of_spec(&cancelling, dialog::ButtonRole::Cancel),
    ];
    t.dialogs.push(d);
    assert_eq!(
        t.dialogs.key_button("escape", true),
        None,
        "Escape names no Help, cancel on it or not"
    );
    assert_eq!(dialog_click(&mut t, "Help"), Some(Ok(())));
    assert_eq!(t.status.as_str(), crate::help_tab::DOCS_COMING);
    assert!(t.dialogs.is_open(), "the dialog stays");
    t.dialogs.top_dialog_mut().unwrap().buttons[1].enabled = false;
    assert_eq!(
        dialog_click(&mut t, "Help"),
        Some(Err("'Help' is disabled".into()))
    );
}
