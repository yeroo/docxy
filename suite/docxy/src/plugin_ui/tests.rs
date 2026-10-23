use super::*;
use crate::dialog::{ControlKind, Value, grid_address};
use crate::menu::MenuItem;
use crate::ribbon_fit;
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

#[test]
fn the_view_json_carries_strikethrough_and_pattern() {
    let mut tab = tab();
    let Surface::Project(v) = &mut tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    v.ed.edit_formatting(|f| {
        f.set_category(
            "Summary Tasks",
            TextStyle {
                strikethrough: Some(true),
                ..Default::default()
            },
        );
        f.set_cell(
            uid,
            "Name",
            TextStyle {
                pattern: Some("dark-dither".into()),
                strikethrough: Some(false),
                ..Default::default()
            },
        );
    });
    v.entry = false;
    v.ed.select(0);
    v.col = crate::COLUMNS.iter().position(|c| *c == "Name").unwrap();
    let view = view_json(v);
    let category = view
        .get("text_styles")
        .unwrap()
        .get("Summary Tasks")
        .unwrap();
    assert_eq!(category.get("strikethrough"), Some(&Json::Bool(true)));
    let cells = view.list("cells");
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].get_str("pattern"), Some("dark-dither"));
    assert_eq!(cells[0].get("strikethrough"), Some(&Json::Bool(false)));
}

#[test]
fn a_pattern_is_drawn_by_its_family() {
    use PatternLook::*;
    assert_eq!(pattern_look(Some("hollow")), Hollow);
    for flat in [Some("solid"), Some("no-such-pattern"), Some(""), None] {
        assert_eq!(pattern_look(flat), Flat, "{flat:?}");
    }
    assert_eq!(pattern_look(Some("light-fill")), Blend(25));
    assert_eq!(pattern_look(Some("medium-fill")), Blend(50));
    assert_eq!(pattern_look(Some("dark-fill")), Blend(75));
    // Every other name of Project's vocabulary is a layer, none unknown.
    for name in projcore::formatting::PATTERNS {
        let hatched = matches!(pattern_look(Some(name)), Slash { .. } | Dots { .. });
        let expected = !matches!(
            name,
            "hollow" | "solid" | "light-fill" | "medium-fill" | "dark-fill"
        );
        assert_eq!(hatched, expected, "{name}");
    }
    // The dark slants are thicker than the thin ones.
    let (Slash { width: thin, .. }, Slash { width: dark, .. }) = (
        pattern_look(Some("diagonal-left")),
        pattern_look(Some("dark-diagonal-left")),
    ) else {
        panic!("slants are slashes");
    };
    assert!(dark > thin);
    // The dithers grow with their weight.
    let size = |n| match pattern_look(Some(n)) {
        Dots { size } => size,
        other => panic!("{n}: {other:?}"),
    };
    assert!(size("light-dither") < size("medium-dither"));
    assert!(size("medium-dither") < size("dark-dither"));
}

#[test]
fn the_base_fill_follows_the_pattern() {
    let style = |background, pattern: Option<&str>| TextStyle {
        background,
        pattern: pattern.map(str::to_string),
        color: Some(0x000000),
        ..Default::default()
    };
    // Hollow is no fill even over a colour; solid and unknown are today's.
    assert_eq!(base_fill(&style(Some(0xFF0000), Some("hollow"))), None);
    assert_eq!(
        base_fill(&style(Some(0xFF0000), Some("solid"))),
        Some(0xFF0000)
    );
    assert_eq!(
        base_fill(&style(Some(0xFF0000), Some("odd"))),
        Some(0xFF0000)
    );
    assert_eq!(base_fill(&style(None, None)), None);
    // A hatch keeps the background beneath it; the layer adds the white.
    assert_eq!(
        base_fill(&style(Some(0xFF0000), Some("dark-dither"))),
        Some(0xFF0000)
    );
    assert_eq!(base_fill(&style(None, Some("dark-dither"))), None);
    // A fill moves the background towards the text colour; white without one.
    assert_eq!(base_fill(&style(None, Some("medium-fill"))), Some(0x808080));
    assert_eq!(
        base_fill(&style(Some(0xFF0000), Some("dark-fill"))),
        Some(0x400000)
    );
    assert!(pattern_layer(&style(None, Some("dark-dither"))).is_some());
    assert!(pattern_layer(&style(None, Some("dark-fill"))).is_none());
    assert!(pattern_layer(&style(None, Some("solid"))).is_none());
}

#[test]
fn an_opaque_cell_and_a_flat_cell_under_a_hatched_row_are_found() {
    let with = |background, pattern: Option<&str>| TextStyle {
        background,
        pattern: pattern.map(str::to_string),
        ..Default::default()
    };
    // Opaque: a colour, a fill, a hatch; not hollow, not an empty style.
    assert!(paints_opaque(&with(Some(0xFF0000), None)));
    assert!(paints_opaque(&with(None, Some("light-fill"))));
    assert!(paints_opaque(&with(None, Some("dark-dither"))));
    assert!(!paints_opaque(&with(None, None)));
    assert!(!paints_opaque(&with(Some(0xFF0000), Some("hollow"))));
    assert!(!paints_opaque(&with(None, Some("solid"))));
    // A plain cell under a hatched row gets its own base; others do not.
    let row = with(None, Some("dark-diagonal-left"));
    assert_eq!(
        cell_base(Some(&row), &with(None, Some("solid"))),
        Some(0xFFFFFF)
    );
    assert_eq!(
        cell_base(Some(&row), &with(Some(0x123456), Some("odd"))),
        Some(0x123456)
    );
    assert_eq!(
        cell_base(Some(&row), &with(None, Some("dark-dither"))),
        None
    );
    assert_eq!(cell_base(Some(&row), &with(None, Some("hollow"))), None);
    assert_eq!(
        cell_base(Some(&with(None, Some("solid"))), &with(None, Some("solid"))),
        None
    );
    assert_eq!(cell_base(None, &with(None, Some("solid"))), None);
    // A filled row (light/medium/dark-fill) is covered like a hatched one.
    let grey = with(None, Some("medium-fill"));
    assert_eq!(
        cell_base(Some(&grey), &with(None, Some("solid"))),
        Some(0xFFFFFF)
    );
    assert_eq!(cell_base(Some(&grey), &with(None, Some("hollow"))), None);
    // A row with only a colour is already what the cell inherits.
    assert_eq!(
        cell_base(Some(&with(Some(1), None)), &with(None, Some("solid"))),
        None
    );
}

#[test]
fn an_inactive_row_keeps_its_strike_over_a_plan_strike_off() {
    use gpui::{Styled, div};
    let off = TextStyle {
        strikethrough: Some(false),
        ..Default::default()
    };
    let thickness = |inactive| {
        let mut el = keep_inactive_strike(styled(div(), &off), inactive);
        el.style().text.strikethrough.as_ref().map(|s| s.thickness)
    };
    assert_eq!(thickness(true), Some(gpui::px(1.)));
    assert_eq!(thickness(false), Some(gpui::px(0.)));
}

#[test]
fn styled_strikes_through_with_underline() {
    use gpui::{Styled, div};
    let both = TextStyle {
        strikethrough: Some(true),
        underline: Some(true),
        ..Default::default()
    };
    let mut el = styled(div(), &both);
    assert!(el.style().text.underline.is_some());
    assert!(el.style().text.strikethrough.is_some());
    let mut plain = styled(div(), &TextStyle::default());
    assert!(plain.style().text.strikethrough.is_none());
    let mut off = styled(
        div(),
        &TextStyle {
            strikethrough: Some(false),
            ..Default::default()
        },
    );
    // An explicit off is a zero-thickness line, which cancels the row's.
    assert_eq!(
        off.style().text.strikethrough.as_ref().map(|s| s.thickness),
        Some(gpui::px(0.))
    );
    let mut hollow = styled(
        div(),
        &TextStyle {
            background: Some(0xFF0000),
            pattern: Some("hollow".into()),
            ..Default::default()
        },
    );
    assert!(hollow.style().background.is_none());
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

#[test]
fn the_overlay_is_asked_again_when_the_zoom_changes() {
    let mut tab = tab();
    let Surface::Project(v) = &mut tab.surface else {
        unreachable!()
    };
    v.layout(1180.);
    let before = overlay_key(v);
    assert_eq!(before, overlay_key(v));
    v.zoom_step(-1, 0.);
    let zoomed = overlay_key(v);
    assert_ne!(before, zoomed, "a zoom moves every shape");
    v.zoom_step(1, 0.);
    assert_eq!(before, overlay_key(v));
}

const WIDE: &str = r##"{
  "dialogs":[{"id":"task-information",
    "header":[{"name":"fx.head","label":"Head:","kind":"label","value":"on every page"}],
    "pages":[{"name":"Wide","columns":2,"controls":[
      {"name":"fx.note","label":"&Note:","kind":"multiline","value":""},
      {"name":"fx.pick","label":"Pick:","kind":"dropdown","items":["none"],"value":"none"}]}]}]
}"##;

fn wide_manifest() -> Manifest {
    Manifest::parse(WIDE).unwrap()
}

#[test]
fn task_information_top_row_is_a_whole_header_band_over_a_two_column_page() {
    let m = wide_manifest();
    let spec = m.dialog("task-information").unwrap();
    let tab = tab();
    let Surface::Project(v) = &tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    let mut d = crate::task_info_dialog(&v.ed, uid, crate::InfoPage::General).unwrap();
    extend_task_info_with(&mut d, spec, &Prefill::default());
    d.mark_opened();
    d.select_tab("Wide").unwrap();
    assert_eq!(d.columns(), 2);
    let name_of = |d: &Dialog, i: usize| d.controls[i].name;
    let bands = d.layout_rows();
    let header = &bands[0];
    assert!(header.header);
    // The plugin's header label is one column, so the band is one wide:
    // Name alone, Duration with Estimated beside it, then the plugin's.
    assert_eq!(header.columns, 1);
    let rows: Vec<Vec<&str>> = header
        .rows
        .iter()
        .map(|r| r.iter().map(|&(i, _)| name_of(&d, i)).collect())
        .collect();
    assert_eq!(
        rows,
        [vec!["name"], vec!["duration", "estimated"], vec!["fx.head"]]
    );
    // The page keeps its two columns.
    assert!(bands.iter().skip(1).any(|b| b.columns == 2));
    for name in ["name", "duration", "estimated"] {
        assert!(d.controls.iter().find(|c| c.name == name).unwrap().header);
    }
}

#[test]
fn task_information_top_row_spans_a_wide_plugin_header_and_the_size_is_fixed() {
    let m = wide_manifest();
    let spec = m.dialog("task-information").unwrap();
    let tab = tab();
    let Surface::Project(v) = &tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    let mut plain = crate::task_info_dialog(&v.ed, uid, crate::InfoPage::General).unwrap();
    let mut d = crate::task_info_dialog(&v.ed, uid, crate::InfoPage::General).unwrap();
    extend_task_info_with(&mut d, spec, &Prefill::default());
    d.controls
        .iter_mut()
        .find(|c| c.name == "fx.head")
        .unwrap()
        .span = 2;
    assert_eq!(d.header_columns(), 2);
    let bands = d.layout_rows();
    let first = &bands[0].rows[0];
    assert_eq!(first.len(), 1, "Name is alone in the band's first row");
    assert_eq!(d.controls[first[0].0].name, "name");
    // No plugin: one column, and the same order as before.
    assert_eq!(plain.header_columns(), 1);
    let order: Vec<&str> = plain.layout_rows()[0]
        .rows
        .iter()
        .flatten()
        .map(|&(i, _)| plain.controls[i].name)
        .collect();
    assert_eq!(order, ["name", "duration", "estimated"]);
    // One size on every page, with and without a plugin page.
    for dialog in [&mut plain, &mut d] {
        assert_eq!(dialog.size, Some(crate::TASK_INFO_SIZE));
        for t in dialog.tabs.clone() {
            dialog.select_tab(&t).unwrap();
            assert_eq!(dialog.size, Some(crate::TASK_INFO_SIZE), "{t}");
        }
    }
    // `fills` holds for a `fill` multiline once Task Information is sized
    // (the flag only; the layout is not covered here).
    d.select_tab("Wide").unwrap();
    let note = d.controls.iter_mut().find(|c| c.name == "fx.note").unwrap();
    note.fill = true;
    let note = note.clone();
    assert!(d.fills(&note));
}

#[test]
fn a_two_column_plugin_header_starts_a_new_row_after_the_estimated_box() {
    let m = wide_manifest();
    let spec = m.dialog("task-information").unwrap();
    let tab = tab();
    let Surface::Project(v) = &tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    let mut d = crate::task_info_dialog(&v.ed, uid, crate::InfoPage::General).unwrap();
    extend_task_info_with(&mut d, spec, &Prefill::default());
    let mut second = d
        .controls
        .iter()
        .find(|c| c.name == "fx.head")
        .unwrap()
        .clone();
    second.name = "fx.head2";
    d.controls.push(second);
    assert_eq!(d.header_columns(), 2);
    let rows: Vec<Vec<&str>> = d.layout_rows()[0]
        .rows
        .iter()
        .map(|r| r.iter().map(|&(i, _)| d.controls[i].name).collect())
        .collect();
    assert_eq!(
        rows,
        [
            vec!["name"],
            vec!["duration", "estimated"],
            vec!["fx.head", "fx.head2"]
        ]
    );
}

#[test]
fn the_built_in_grids_scroll_in_the_fixed_size() {
    let tab = tab();
    let Surface::Project(v) = &tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    let d = crate::task_info_dialog(&v.ed, uid, crate::InfoPage::General).unwrap();
    let grids: Vec<_> = d
        .controls
        .iter()
        .filter(|c| c.kind == ControlKind::Grid)
        .collect();
    assert_eq!(grids.len(), 2);
    assert!(grids.iter().all(|c| c.min_rows.is_some()));
}

fn extended(json: &str) -> rs::Tab<Act> {
    let m = Manifest::parse(json).unwrap();
    let mut ribbon = crate::project_ribbon();
    let mut task = ribbon.tabs.remove(
        ribbon
            .tabs
            .iter()
            .position(|t| t.name == "Task")
            .expect("Project has a Task tab"),
    );
    extend_tab_with(&mut task, &[&m]);
    task
}

fn priority_of(tab: &rs::Tab<Act>, title: &str) -> u8 {
    tab.groups
        .iter()
        .find(|g| g.title == title)
        .unwrap_or_else(|| panic!("no group {title}"))
        .priority
}

#[test]
fn a_contributed_group_takes_a_declared_priority_else_its_neighbours_else_50() {
    let task = extended(
        r##"{"ribbon":[{"tab":"Task","groups":[
          {"title":"Declared","priority":7,"after":"Tasks","controls":[{"id":"d.a","kind":"button","label":"A"}]},
          {"title":"AfterTasks","after":"Tasks","controls":[{"id":"a.a","kind":"button","label":"A"}]},
          {"title":"BeforeSchedule","before":"Schedule","controls":[{"id":"b.a","kind":"button","label":"A"}]},
          {"title":"NoSuchTitle","after":"Nope","controls":[{"id":"n.a","kind":"button","label":"A"}]},
          {"title":"Appended","controls":[{"id":"p.a","kind":"button","label":"A"}]}]}]}"##,
    );
    let built_in = |t: &str| {
        let plain = crate::project_ribbon();
        let tab = plain.tabs.iter().find(|t| t.name == "Task").unwrap();
        priority_of(tab, t)
    };
    assert_eq!(priority_of(&task, "Declared"), 7);
    assert_eq!(priority_of(&task, "AfterTasks"), built_in("Tasks"));
    assert_eq!(priority_of(&task, "BeforeSchedule"), built_in("Schedule"));
    assert_eq!(priority_of(&task, "NoSuchTitle"), 50);
    assert_eq!(priority_of(&task, "Appended"), 50);
    // Joining a built-in group changes nothing.
    let joined = extended(
        r##"{"ribbon":[{"tab":"Task","groups":[
          {"title":"Schedule","priority":1,"controls":[{"id":"j.a","kind":"button","label":"A"}]}]}]}"##,
    );
    assert_eq!(priority_of(&joined, "Schedule"), built_in("Schedule"));
}

#[test]
fn a_declared_priority_moves_the_group_in_the_collapse_order() {
    let task = extended(
        r##"{"ribbon":[{"tab":"Task","groups":[
          {"title":"Early","priority":0,"before":"Clipboard","controls":[{"id":"e.a","kind":"button","label":"A"}]},
          {"title":"Late","priority":255,"controls":[{"id":"l.a","kind":"button","label":"A"}]}]}]}"##,
    );
    let specs: Vec<ribbon_fit::Spec> = task
        .groups
        .iter()
        .map(|g| ribbon_fit::model_spec(g, &ribbon_fit::approx_tw))
        .collect();
    let at = |t: &str| task.groups.iter().position(|g| g.title == t).unwrap();
    let (early, late) = (at("Early"), at("Late"));
    assert_eq!((early, late), (0, task.groups.len() - 1));
    // Squeezed just below the full width, the lowest priority goes first
    // (the leftmost group), though the appended one would, at the default 50
    // and last place, and the highest stays full.
    let full: f32 = specs.iter().map(|s| s.full).sum();
    let fit = ribbon_fit::fit_with(
        &specs,
        full + ribbon_fit::GUTTER - 1.0,
        ribbon_fit::Policy::WholeGroups,
    );
    assert_eq!(fit.states[early], ribbon_fit::State::Collapsed);
    assert_eq!(fit.states[late], ribbon_fit::State::Full);
    // Wide again: every group is full, nothing stays collapsed (#1685).
    let fit = ribbon_fit::fit_with(
        &specs,
        full + ribbon_fit::GUTTER,
        ribbon_fit::Policy::WholeGroups,
    );
    assert!(fit.states.iter().all(|s| *s == ribbon_fit::State::Full));
}

#[test]
fn small_buttons_stack_three_deep_and_a_long_label_is_cut() {
    let long = "A very long contributed label that would stretch its column";
    let json = format!(
        r##"{{"ribbon":[{{"tab":"Task","groups":[{{"title":"Percent","controls":[
          {{"id":"p.0","kind":"button","label":"0%"}},
          {{"id":"p.25","kind":"button","label":"25%"}},
          {{"id":"p.50","kind":"button","label":"50%"}},
          {{"id":"p.75","kind":"button","label":"75%"}},
          {{"id":"p.100","kind":"button","label":"{long}","tip":""}}]}}]}}]}}"##
    );
    let task = extended(&json);
    let group = task.groups.last().unwrap();
    let cols: Vec<&Vec<rs::Cmd<Act>>> = group
        .items
        .iter()
        .map(|c| match c {
            Control::Column(cs) => cs,
            _ => panic!("small buttons stack in columns"),
        })
        .collect();
    assert_eq!(cols.iter().map(|c| c.len()).collect::<Vec<_>>(), [3, 2]);
    let cut = &cols[1][1];
    assert_eq!(cut.label.chars().count(), CONTRIB_LABEL_MAX);
    assert!(cut.label.ends_with('\u{2026}'));
    assert_eq!(cut.tip.title, long);
    // A short label is untouched, and a multibyte one is cut on characters.
    assert_eq!(capped_label("0%"), "0%");
    let wide = "é".repeat(CONTRIB_LABEL_MAX + 3);
    assert_eq!(capped_label(&wide).chars().count(), CONTRIB_LABEL_MAX);
    // The column is as wide as its capped widest label, not the long one.
    let tw = ribbon_fit::approx_tw;
    let w = ribbon_fit::model_control_w(&group.items[1], false, None, &tw);
    assert!(w <= 18.0 + tw(&"W".repeat(CONTRIB_LABEL_MAX), 12.0), "{w}");
}

#[test]
fn a_swatch_gallerys_estimate_is_the_width_it_draws() {
    let swatches: Vec<String> = (0..12)
        .map(|i| format!(r##"{{"id":"g.{i}","label":"S{i}","color":"#3366CC"}}"##))
        .collect();
    let json = format!(
        r##"{{"ribbon":[{{"tab":"Task","groups":[{{"title":"Palette","controls":[
          {{"id":"g.gallery","kind":"gallery","label":"Palette","swatches":[{}]}}]}}]}}]}}"##,
        swatches.join(",")
    );
    let task = extended(&json);
    let Control::Gallery(gal) = &task.groups.last().unwrap().items[0] else {
        panic!("a gallery");
    };
    assert!(is_swatch_gallery(gal));
    let t = crate::style_gallery::TILE;
    let n = gal.items.len() as f32;
    // `Docxy::swatch_gallery` draws `per_row * w + (per_row - 1) * gap`.
    let drawn = n * t.w + (n - 1.0) * t.gap;
    let tw = ribbon_fit::approx_tw;
    let control = &task.groups.last().unwrap().items[0];
    assert_eq!(
        ribbon_fit::model_control_w(control, false, None, &tw),
        drawn
    );
    // Wrapped to a flyout width, the same rule.
    let (k, w) =
        ribbon_fit::gallery_row(ribbon_fit::gallery_geom(gal), gal.items.len(), Some(300.0));
    assert_eq!(w, k as f32 * t.w + (k as f32 - 1.0) * t.gap);
}

#[test]
fn a_cut_label_keeps_its_full_text_and_the_plugins_tip() {
    let long = "A very long contributed label that would stretch its column";
    let json = format!(
        r##"{{"ribbon":[{{"tab":"Task","groups":[{{"title":"Cut","controls":[
          {{"id":"c.a","kind":"button","label":"{long}","tip":"What it does"}}]}}]}}]}}"##
    );
    let task = extended(&json);
    let Control::Column(cs) = &task.groups.last().unwrap().items[0] else {
        panic!("a column");
    };
    assert_eq!(cs[0].label.chars().count(), CONTRIB_LABEL_MAX);
    assert_eq!((cs[0].tip.title, cs[0].tip.body), (long, "What it does"));
}

#[test]
fn a_replacement_in_a_column_is_capped_but_a_large_one_keeps_its_label() {
    let long = "A very long contributed label that would stretch its column";
    let json = format!(
        r##"{{"ribbon":[{{"tab":"Task","groups":[{{"title":"Anywhere","controls":[
          {{"id":"pr-indent","kind":"button","label":"{long}"}},
          {{"id":"pr-paste","kind":"button","label":"{long}","large":true}}]}}]}}]}}"##
    );
    let task = extended(&json);
    let find = |id: &str| {
        task.groups
            .iter()
            .flat_map(|g| g.items.iter())
            .find_map(|c| match c {
                Control::Column(cs) => cs.iter().find(|c| c.id == id),
                Control::Large(c) if c.id == id => Some(c),
                _ => None,
            })
            .unwrap()
    };
    let indent = find("pr-indent");
    assert_eq!(indent.label.chars().count(), CONTRIB_LABEL_MAX);
    assert_eq!(indent.tip.title, long);
    assert_eq!(find("pr-paste").label, long);
}

#[test]
fn a_plugin_gallery_named_like_table_styles_is_not_a_swatch_gallery() {
    let task = extended(
        r##"{"ribbon":[{"tab":"Task","groups":[{"title":"T","controls":[
          {"id":"tablestyles","kind":"gallery","label":"T","swatches":[{"id":"t.a","label":"A","color":"#3366CC"}]}]}]}]}"##,
    );
    let Control::Gallery(gal) = &task.groups.last().unwrap().items[0] else {
        panic!("a gallery");
    };
    assert_eq!(gal.id, "tablestyles");
    assert!(!is_swatch_gallery(gal));
    assert_eq!(ribbon_fit::gallery_geom(gal).frame, 6.0);
}

#[test]
fn the_view_json_carries_bars_and_the_overlay_bar_follows_the_first_row() {
    use projcore::formatting::{BarRow, Middle, TaskBar};
    let mut tab = tab();
    let Surface::Project(v) = &mut tab.surface else {
        unreachable!()
    };
    let uid = v.ed.project().tasks[0].uid;
    // No table: the view says so and the overlay row has the palette's
    // rectangle at the original row height.
    let view = view_json(v);
    assert!(view.list("bars").is_empty() && view.list("task_bars").is_empty());
    let plain = overlay_ctx(v);
    assert_eq!(plain.row_h, crate::ROW_H);
    let palette_bar = plain.rows[0].bar.expect("a scheduled leaf has a bar");
    assert_eq!(plain.rows[1].row_y, crate::ROW_H);
    // Two rows, the first in band 2: rows are two bands tall, the second
    // task's row starts a row later, and the bar is the first drawn row's
    // middle (a medium-bottom bar here), in that band.
    v.ed.edit_formatting(|f| {
        f.set_bars(vec![
            BarRow {
                name: "A".into(),
                row: 2,
                middle: Middle {
                    shape: Some("medium-bottom".into()),
                    ..Middle::default()
                },
                ..BarRow::default()
            },
            BarRow {
                name: "B".into(),
                row: 1,
                ..BarRow::default()
            },
        ]);
        f.set_task_bar(TaskBar {
            uid,
            middle: Middle {
                color: Some(0x00AA00),
                ..Middle::default()
            },
            ..TaskBar::default()
        });
    });
    let view = view_json(v);
    assert_eq!(view.list("bars").len(), 2);
    assert_eq!(view.list("bars")[0].get_str("name"), Some("A"));
    assert_eq!(view.list("bars")[0].int("row"), Some(2));
    assert_eq!(view.list("task_bars")[0].int("uid"), Some(i64::from(uid)));
    // The same JSON read back is the same table.
    let mut back = projcore::formatting::Formatting::default();
    back.apply_view(&Json::obj(vec![
        ("bars", view.get("bars").unwrap().clone()),
        ("task_bars", view.get("task_bars").unwrap().clone()),
    ]))
    .unwrap();
    assert_eq!(back.bars, v.ed.project().formatting.bars);
    assert_eq!(back.task_bars, v.ed.project().formatting.task_bars);
    let ctx = overlay_ctx(v);
    assert_eq!(ctx.row_h, 2. * crate::ROW_H);
    assert_eq!(ctx.rows[1].row_y, 2. * crate::ROW_H);
    let (x, y, w, h) = ctx.rows[0].bar.unwrap();
    assert_eq!(
        (x, y, w, h),
        (
            palette_bar.0,
            crate::ROW_H + crate::BAR_Y + 2.,
            palette_bar.2,
            8.
        )
    );
}

#[test]
fn the_view_json_carries_layout_and_the_overlay_bar_follows_the_bar_height() {
    let mut tab = tab();
    let Surface::Project(v) = &mut tab.surface else {
        unreachable!()
    };
    // Unset: seven nulls, and the overlay bar is the original 14 px box.
    let view = view_json(v);
    let Some(Json::Obj(pairs)) = view.get("layout") else {
        panic!("no layout in the view")
    };
    assert_eq!(pairs.len(), 7);
    assert!(pairs.iter().all(|(_, value)| value.is_null()));
    let plain = overlay_ctx(v).rows[0].bar.expect("a scheduled leaf has a bar");
    assert_eq!(plain.3, 14.);
    // 12 pt is 16 px, still centred on y 12; the layout reads back as sent.
    let set = Json::parse(r#"{"layout":{"bar_height":12,"rollup":true,"round_bars":false}}"#)
        .unwrap();
    v.ed.edit_formatting(|f| {
        f.apply_view(&set).unwrap();
    });
    let view = view_json(v);
    assert_eq!(view.get("layout").unwrap().int("bar_height"), Some(12));
    assert_eq!(view.get("layout").unwrap().get("round_bars"), Some(&Json::Bool(false)));
    let mut back = projcore::formatting::Formatting::default();
    back.apply_view(&Json::obj(vec![("layout", view.get("layout").unwrap().clone())]))
        .unwrap();
    assert_eq!(back.layout, v.ed.project().formatting.layout);
    let tall = overlay_ctx(v).rows[0].bar.unwrap();
    assert_eq!((tall.1, tall.3), (4., 16.));
    assert_eq!(overlay_ctx(v).flags.get("layout").unwrap().int("bar_height"), Some(12));
}
