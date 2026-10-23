//! Format plugins end to end: the fixture plugin (`tests/plugin_fixture`) is
//! built, loaded from a plugin directory, registered, and a project
//! round-trips through it.
#![cfg(feature = "plugins")]

#[path = "support/fixture_plugin.rs"]
mod fixture_plugin;

use projcore::formats::Registry;
use projcore::formats::plugin::{
    Host, load_plugin, load_plugins, load_plugins_for, load_plugins_with_errors,
};
use projcore::json::Json;
use projcore::mspdi::{read_mspdi, write_mspdi};
use projcore::uiplugin::{
    Command, ControlKind, OverlayCtx, OverlayRow, Selection, Shape, StateCtx,
};
use std::path::Path;

fn corpus_plan() -> projcore::Project {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/10-summary.xml");
    read_mspdi(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_fixture_plugin_round_trips_a_project() {
    let dir = fixture_plugin::plugin_dir("projcore-plugin-roundtrip");
    // A missing directory is skipped; the same directory twice loads once.
    let (formats, errors) =
        load_plugins_with_errors(&[dir.join("missing"), dir.clone(), dir.clone()]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(formats.len(), 1);
    let format = &formats[0];
    assert_eq!(format.extensions(), ["fakeproj"]);
    assert_eq!(format.label(), "Fake Project");
    assert!(format.reads() && format.writes());
    // It declares itself the default save type.
    assert!(format.is_default());

    let mut registry = Registry::new();
    for format in formats {
        registry.register(format);
    }
    assert_eq!(registry.open_extensions(), ["yppx", "xml", "fakeproj"]);
    assert_eq!(registry.save_extensions(), ["yppx", "xml", "fakeproj"]);
    assert_eq!(registry.save_types()[2], ("fakeproj", "Fake Project"));
    // So a new plan's Save As proposes it, first; an opened .xml stays .xml.
    assert_eq!(registry.default_save_extension(None), "fakeproj");
    assert_eq!(registry.default_save_extension(Some("xml")), "xml");
    assert_eq!(
        registry.save_types_default_first(None),
        [
            ("fakeproj", "Fake Project"),
            ("yppx", "Project schedule"),
            ("xml", "MSPDI")
        ]
    );
    assert_eq!(
        registry.save_target(Path::new("plan.mpp")).unwrap_err(),
        "Project schedules can only be saved as .yppx, .xml (MSPDI) or .fakeproj"
    );
    let path = Path::new("plan.FakeProj");
    assert_eq!(registry.save_target(path).unwrap(), path);

    // The fixture's files are the MSPDI, passed through unchanged.
    let project = corpus_plan();
    assert!(!project.tasks.is_empty());
    let bytes = registry.write(&project, path, None).unwrap();
    assert_eq!(bytes, write_mspdi(&project).into_bytes());
    let back = registry.read(path, &bytes).unwrap();
    assert_eq!(back, read_mspdi(&write_mspdi(&project)).unwrap());
    assert_eq!(
        back.tasks.iter().map(|t| &t.name).collect::<Vec<_>>(),
        project.tasks.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // The source bytes reach the plugin (it notes their length).
    let with_source = registry.write(&project, path, Some(b"abc")).unwrap();
    let text = String::from_utf8(with_source.clone()).unwrap();
    assert!(
        text.ends_with("<!-- fixture: source 3 bytes -->\n"),
        "{text}"
    );
    assert_eq!(registry.read(path, &with_source).unwrap(), back);

    // A loaded library stays loaded (and, on Windows, its file locked).
    let _ = std::fs::remove_dir_all(dir);
}

fn formatted_plan() -> projcore::Project {
    use projcore::formatting::{Gridline, LineType, TextStyle, gridlines::Line};
    let mut project = corpus_plan();
    let f = &mut project.formatting;
    f.palette = Some("fx.blue".into());
    f.set_switch("fx.slack", Some(true));
    f.set_category(
        "Summary Tasks",
        TextStyle {
            bold: Some(true),
            color: Some(0x1F3763),
            ..TextStyle::default()
        },
    );
    f.gridlines.set(
        Line::GanttRows,
        Gridline {
            line_type: LineType::Dotted,
            color: Some(0xCC3333),
            interval: 2,
            interval_type: LineType::Solid,
            interval_color: None,
        },
    );
    project
}

#[test]
fn a_plugin_write_and_read_carry_the_formatting() {
    let dir = fixture_plugin::plugin_dir("projcore-plugin-formatting");
    let mut registry = Registry::new();
    for format in load_plugins(std::slice::from_ref(&dir)) {
        registry.register(format);
    }
    let path = Path::new("plan.fakeproj");

    // The formatting reaches the plugin beside the MSPDI, and comes back.
    let project = formatted_plan();
    assert!(!project.formatting.is_empty());
    let bytes = registry.write(&project, path, None).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("<!-- fixture-formatting: "), "{text}");
    let back = registry.read(path, &bytes).unwrap();
    assert_eq!(back.formatting, project.formatting);
    assert_eq!(
        back,
        read_mspdi(&write_mspdi(&project))
            .map(|mut p| {
                p.formatting = project.formatting.clone();
                p
            })
            .unwrap()
    );

    // With a source file the note follows the trailer; both still read.
    let with_source = registry.write(&project, path, Some(b"abc")).unwrap();
    assert!(with_source.ends_with(b"<!-- fixture: source 3 bytes -->\n"));
    assert_eq!(
        registry.read(path, &with_source).unwrap().formatting,
        project.formatting
    );

    // The plain read (what a host without the `_ex` symbols calls) parses
    // the MSPDI with the trailer after it.
    assert_eq!(read_mspdi(&text).unwrap().tasks, project.tasks);

    // No formatting: NULL and 0 reach the plugin (no trailer), and a read
    // leaves the formatting default.
    let plain = corpus_plan();
    let bytes = registry.write(&plain, path, None).unwrap();
    assert_eq!(bytes, write_mspdi(&plain).into_bytes());
    assert!(registry.read(path, &bytes).unwrap().formatting.is_empty());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn formatting_a_plugin_returns_that_cannot_be_read_fails_the_read() {
    let dir = fixture_plugin::plugin_dir("projcore-plugin-formatting-bad");
    let mut registry = Registry::new();
    for format in load_plugins(std::slice::from_ref(&dir)) {
        registry.register(format);
    }
    let path = Path::new("plan.fakeproj");
    let name = "yppx_fixture".to_string() + std::env::consts::DLL_SUFFIX;
    for (marker, why) in [
        ("<!-- fixture-formatting-nonutf8 -->", "not UTF-8"),
        (
            "<!-- fixture-formatting-malformed -->",
            "the formatting it returned",
        ),
    ] {
        let file = format!("{}{marker}\n", write_mspdi(&corpus_plan()));
        let error = registry.read(path, file.as_bytes()).unwrap_err();
        assert!(error.starts_with(&format!("{name}: ")), "{error}");
        assert!(error.contains(why), "{error}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn plugin_errors_carry_the_plugin_message() {
    let dir = fixture_plugin::plugin_dir("projcore-plugin-errors");
    let mut registry = Registry::new();
    for format in load_plugins(std::slice::from_ref(&dir)) {
        registry.register(format);
    }
    let error = registry.read(Path::new("empty.fakeproj"), b"").unwrap_err();
    assert_eq!(
        error,
        "yppx_fixture".to_string() + std::env::consts::DLL_SUFFIX + ": empty .fakeproj file"
    );
    // MSPDI the plugin passes back is still parsed by the host.
    let error = registry
        .read(Path::new("bad.fakeproj"), b"<Nope/>")
        .unwrap_err();
    assert!(error.contains("not an MSPDI document"), "{error}");
    // A loaded library stays loaded (and, on Windows, its file locked).
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_library_loads_whatever_its_name() {
    // `load_plugin` takes the file as cargo built it (`lib` prefix on Unix).
    let formats = load_plugin(fixture_plugin::built_fixture()).unwrap();
    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].extensions(), ["fakeproj"]);
}

/// The fixture as a UI plugin: its target is checked for the host, its
/// manifest read, and state, a command and the overlay round-trip.
#[test]
fn the_fixture_is_a_ui_plugin_for_a_matching_host() {
    let dir = fixture_plugin::plugin_dir("projcore-plugin-ui");
    let host = Host {
        name: "suite",
        version: "0.1.0",
    };
    let loaded = load_plugins_for(std::slice::from_ref(&dir), Some(host));
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(loaded.formats.len(), 1);
    assert_eq!(loaded.ui.len(), 1);
    assert_eq!(loaded.plugins.len(), 1);
    let info = &loaded.plugins[0];
    assert_eq!(
        info.file,
        format!("yppx_fixture{}", std::env::consts::DLL_SUFFIX)
    );
    assert_eq!(info.formats, ["fakeproj"]);
    assert!(info.ui);
    let target = info.target.as_ref().unwrap();
    assert_eq!(target.built_against, "fixture");
    assert_eq!(target.requirement("yppxy"), Some("0.1.*"));

    let ui = &loaded.ui[0];
    assert_eq!(ui.name(), info.file);
    let m = ui.manifest();
    assert_eq!(m.ribbon[0].tab, "Task");
    // A second tab's group asks to go before a built-in group (#1574).
    assert_eq!(m.ribbon[1].tab, "Gantt Chart Format");
    assert_eq!(m.ribbon[1].groups[0].before, "Bar Styles");
    // A group may declare its collapse priority (#1685); another does not.
    assert_eq!(m.ribbon[1].groups[0].priority, None);
    assert_eq!(m.ribbon[1].groups[1].title, "Fixture Percent");
    assert_eq!(m.ribbon[1].groups[1].after, "Fixture Format");
    assert_eq!(m.ribbon[1].groups[1].priority, Some(120));
    assert_eq!(m.ribbon[1].groups[1].controls.len(), 5);
    let group = &m.ribbon[0].groups[0];
    assert_eq!(group.title, "Fixture");
    assert_eq!(
        group
            .controls
            .iter()
            .map(|c| (c.id.as_str(), c.kind))
            .collect::<Vec<_>>(),
        [
            ("fixture.hello", ControlKind::Button),
            ("fixture.flag", ControlKind::Toggle),
            ("fixture.plain", ControlKind::Toggle),
            ("fixture.palette", ControlKind::Gallery),
            ("fixture.bold", ControlKind::Toggle),
            ("fixture.sheet", ControlKind::Button),
        ]
    );
    assert!(group.controls[0].large && group.controls[0].keytip == "XH");
    assert_eq!(group.controls[3].swatches[0].color, Some(0x3366CC));
    assert!(
        group.controls[2].icon.is_empty(),
        "the Plain toggle names no icon"
    );
    assert_eq!(m.menus[0].menu, "row");
    assert_eq!(m.menus[0].items[0].id, "fixture.rename");
    assert_eq!(m.menus[0].items[0].after, "Notes...");
    let page = &m.dialog("task-information").unwrap().pages[0];
    assert_eq!(page.name, "Fixture");
    assert_eq!(page.controls[0].kind, "multiline");
    assert_eq!(page.controls[1].columns[1].kind, "checkbox");
    assert_eq!(
        m.dialog("fixture.options").unwrap().title,
        "Fixture Options"
    );
    // A modeless dialog with buttons of its own and a check column (#1602).
    let sheet = m.dialog("fixture.sheet").unwrap();
    assert!(sheet.modeless);
    assert_eq!(
        sheet
            .buttons
            .iter()
            .map(|b| (b.id.as_str(), b.default, b.cancel))
            .collect::<Vec<_>>(),
        [
            ("apply", true, false),
            ("clear", false, false),
            ("close", false, true),
            ("help", false, false)
        ]
    );
    assert_eq!(sheet.pages[0].controls[0].columns[0].kind, "check");
    assert_eq!(m.palettes.len(), 2);
    assert_eq!(m.text_style_categories, ["Fixture Tasks"]);

    // State: the toggle off, the row item disabled without a selection,
    // the blue palette checked; a Task Information prefill by uid.
    let plan = write_mspdi(&corpus_plan());
    let ctx = |uids: Vec<i32>, dialog| StateCtx {
        plan: plan.clone(),
        selection: Selection {
            uids,
            cols: vec!["Name".into()],
        },
        view: Json::Obj(Vec::new()),
        dialog,
        dialog_values: None,
    };
    let state = ui.state(&ctx(Vec::new(), None)).unwrap();
    assert_eq!(state.control("fixture.flag").checked, Some(false));
    assert_eq!(state.control("fixture.rename").enabled, Some(false));
    assert_eq!(state.control("fixture.bold").enabled, Some(false));
    assert_eq!(
        state.checked_swatch("fixture.palette"),
        Some("fixture.blue")
    );
    assert!(state.values.is_empty());
    let state = ui
        .state(&ctx(vec![3], Some(("task-information".into(), Some(3)))))
        .unwrap();
    assert_eq!(state.control("fixture.rename").enabled, Some(true));
    assert_eq!(
        state.value("fixture.note").and_then(Json::as_str),
        Some("note for task 3")
    );
    assert_eq!(
        state
            .value("fixture.grid")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // The dialog's dropdown items and a greyed control come from the state
    // too (#1556).
    assert_eq!(
        state.items_of("fixture.pick"),
        Some(&["task".to_string(), "3".to_string()][..])
    );
    assert_eq!(
        state.value("fixture.pick").and_then(Json::as_str),
        Some("3")
    );
    assert_eq!(state.enabled_of("fixture.locked"), Some(false));
    // The Fixture Sheet (#1602): a row per task, Clear greyed until a row is
    // picked; asked again with the dialog's values, the picks stay by name.
    let sheet_ctx = ctx(Vec::new(), Some(("fixture.sheet".into(), None)));
    let state = ui.state(&sheet_ctx).unwrap();
    let rows = state
        .value("fixture.rows")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    assert!(!rows.is_empty());
    let first = rows[0].as_array().unwrap()[1].clone();
    assert_eq!(rows[0].as_array().unwrap()[0], Json::str("false"));
    assert_eq!(state.enabled_of("clear"), Some(false));
    let again = StateCtx {
        dialog_values: Some(Json::obj(vec![(
            "fixture.rows",
            Json::Arr(vec![Json::Arr(vec![
                Json::str("true"),
                first.clone(),
                Json::str("100"),
            ])]),
        )])),
        ..sheet_ctx
    };
    let state = ui.state(&again).unwrap();
    assert_eq!(state.enabled_of("clear"), Some(true));
    let rows = state
        .value("fixture.rows")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    assert_eq!(rows[0].as_array().unwrap()[0], Json::str("true"));
    assert_eq!(rows[0].as_array().unwrap()[1], first);

    // Commands: the toggle flips and the state follows; the rename returns
    // one task.set op; a swatch sets the view's palette; an unknown id is
    // not this plugin's; the dialog command opens its dialog, then takes
    // the values.
    let cmd = |id: &str, uids: Vec<i32>, values: Option<Json>| Command {
        id: id.into(),
        plan: plan.clone(),
        selection: Selection {
            uids,
            cols: Vec::new(),
        },
        view: Json::Obj(Vec::new()),
        values,
        button: None,
        edit: None,
    };
    // The sheet command: without a button it opens the dialog; Apply
    // reports the picks and the selected row and stays open; Clear closes
    // from the reply (#1602).
    let opened = ui
        .command(&cmd("fixture.sheet", vec![], None))
        .unwrap()
        .unwrap();
    assert_eq!(opened.dialog.as_deref(), Some("fixture.sheet"));
    assert!(!opened.close);
    let applied = ui
        .command(&Command {
            button: Some("apply".into()),
            ..cmd(
                "fixture.sheet",
                vec![],
                Some(Json::obj(vec![
                    (
                        "fixture.rows",
                        Json::Arr(vec![Json::strs(["true", "A", "50"])]),
                    ),
                    ("fixture.rows.selected_row", Json::Num(0.0)),
                ])),
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        applied.status.as_deref(),
        Some("Fixture: applied 1 picked, row 0")
    );
    assert!(!applied.close);
    let cleared = ui
        .command(&Command {
            button: Some("clear".into()),
            ..cmd("fixture.sheet", vec![], Some(Json::Obj(Vec::new())))
        })
        .unwrap()
        .unwrap();
    assert!(cleared.close);
    assert_eq!(cleared.status.as_deref(), Some("Fixture: cleared"));
    // A committed Name cell renames the row's task at once (#1602).
    let typed = ui
        .command(&Command {
            edit: Some(Json::obj(vec![
                ("control", Json::str("fixture.rows")),
                ("row", Json::Num(0.0)),
                ("col", Json::Num(1.0)),
                ("value", Json::str("Z")),
            ])),
            ..cmd("fixture.sheet", vec![], Some(Json::Obj(Vec::new())))
        })
        .unwrap()
        .unwrap();
    assert_eq!(typed.ops.len(), 1);
    assert_eq!(typed.ops[0].verb, "task.set");
    assert_eq!(typed.ops[0].args.text("name"), "Z");
    assert_eq!(typed.status.as_deref(), Some("Fixture: renamed row 0"));
    assert!(!typed.close);
    let flipped = ui
        .command(&cmd("fixture.flag", vec![], None))
        .unwrap()
        .unwrap();
    assert_eq!(flipped.status.as_deref(), Some("Fixture: flag on"));
    assert!(flipped.ops.is_empty());
    // The toggle is a view switch (#1553): the reply sets it, and the state
    // reads it back from the view the host sends.
    let view = flipped.view.clone().unwrap();
    assert_eq!(
        view.get("switches")
            .and_then(|s| s.get("fixture.flag"))
            .and_then(Json::as_bool),
        Some(true)
    );
    let on = StateCtx {
        view: view.clone(),
        ..ctx(Vec::new(), None)
    };
    assert_eq!(
        ui.state(&on).unwrap().control("fixture.flag").checked,
        Some(true)
    );
    let off = ui
        .command(&Command {
            view,
            ..cmd("fixture.flag", vec![], None)
        })
        .unwrap()
        .unwrap();
    assert_eq!(off.status.as_deref(), Some("Fixture: flag off"));
    // The Bold toggle reads the cursor cell's style from the view's cells
    // (#1572), and its command writes them for the selection.
    let bold_view = Json::parse(
        r#"{"cells":[{"uid":3,"column":"Name","bold":true},{"uid":4,"column":"Name","italic":true}]}"#,
    )
    .unwrap();
    let on = StateCtx {
        view: bold_view.clone(),
        ..ctx(vec![3], None)
    };
    assert_eq!(
        ui.state(&on).unwrap().control("fixture.bold").checked,
        Some(true)
    );
    let off = StateCtx {
        view: bold_view.clone(),
        ..ctx(vec![4], None)
    };
    assert_eq!(
        ui.state(&off).unwrap().control("fixture.bold").checked,
        Some(false)
    );
    let unbold = ui
        .command(&Command {
            view: bold_view,
            selection: Selection {
                uids: vec![3, 4],
                cols: vec!["Name".into()],
            },
            ..cmd("fixture.bold", vec![3, 4], None)
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        unbold.status.as_deref(),
        Some("Fixture: bold off"),
        "{:?}",
        unbold.view
    );
    let view = unbold.view.clone().unwrap();
    let cells = view.list("cells");
    assert_eq!(cells.len(), 2, "{view:?}");
    assert_eq!(cells[0].int("uid"), Some(3));
    assert_eq!(cells[0].get_str("column"), Some("Name"));
    assert_eq!(cells[0].get("bold"), Some(&Json::Bool(false)));
    // It carries strikethrough and pattern too (#1664), which the plan takes.
    assert_eq!(cells[0].get("strikethrough"), Some(&Json::Bool(false)));
    assert_eq!(cells[0].get_str("pattern"), Some("solid"));
    let mut plan = projcore::formatting::Formatting::default();
    plan.apply_view(&view).unwrap();
    let style = plan.cell(3, "Name").unwrap();
    assert_eq!(style.strikethrough, Some(false));
    assert_eq!(style.pattern.as_deref(), Some("solid"));
    let struck = ui
        .command(&Command {
            selection: Selection {
                uids: vec![3],
                cols: vec!["Name".into()],
            },
            ..cmd("fixture.bold", vec![3], None)
        })
        .unwrap()
        .unwrap();
    let mut plan = projcore::formatting::Formatting::default();
    plan.apply_view(&struck.view.unwrap()).unwrap();
    let style = plan.cell(3, "Name").unwrap();
    assert_eq!(style.bold, Some(true));
    assert_eq!(style.strikethrough, Some(true));
    assert_eq!(style.pattern.as_deref(), Some("dark-diagonal-left"));
    let renamed = ui
        .command(&cmd("fixture.rename", vec![3, 4], None))
        .unwrap()
        .unwrap();
    assert_eq!(renamed.ops.len(), 1);
    assert_eq!(renamed.ops[0].verb, "task.set");
    assert_eq!(renamed.ops[0].args.int("uid"), Some(3));
    assert_eq!(renamed.ops[0].args.text("name"), "Renamed by fixture");
    assert_eq!(
        renamed.status.as_deref(),
        Some("Fixture: renamed task 3 of 2 selected")
    );
    let none = ui
        .command(&cmd("fixture.rename", vec![], None))
        .unwrap()
        .unwrap();
    assert!(none.ops.is_empty());
    assert_eq!(none.status.as_deref(), Some("Fixture: select a task first"));
    let red = ui
        .command(&cmd("fixture.red", vec![], None))
        .unwrap()
        .unwrap();
    assert_eq!(red.view.as_ref().unwrap().text("palette"), "fixture.red");
    // The format command sets the gridlines through the view (#1575), and
    // clears them once the view shows them set; the model takes both.
    let format = ui
        .command(&cmd("fixture.format", vec![], None))
        .unwrap()
        .unwrap();
    assert_eq!(format.status.as_deref(), Some("Fixture: format"));
    let mut formatting = projcore::formatting::Formatting::default();
    assert!(
        formatting
            .apply_view(format.view.as_ref().unwrap())
            .unwrap()
    );
    let rows = formatting
        .gridlines
        .get(projcore::formatting::gridlines::Line::GanttRows);
    assert_eq!(rows.line_type, projcore::formatting::LineType::Dotted);
    assert_eq!(rows.interval, 2);
    assert_eq!(rows.interval_color, Some(0x3366CC));
    assert_eq!(formatting.gridlines.set_lines().count(), 4);
    assert_eq!(
        formatting
            .gridlines
            .get(projcore::formatting::gridlines::Line::SheetColumns)
            .color,
        Some(0x33AA33)
    );
    let mut seen = cmd("fixture.format", vec![], None);
    seen.view = Json::obj(vec![("gridlines", formatting.gridlines.to_json())]);
    let cleared = ui.command(&seen).unwrap().unwrap();
    assert!(
        formatting
            .apply_view(cleared.view.as_ref().unwrap())
            .unwrap()
    );
    assert!(formatting.gridlines.is_empty());
    assert_eq!(
        ui.state(&ctx(Vec::new(), None))
            .unwrap()
            .checked_swatch("fixture.palette"),
        Some("fixture.red")
    );
    assert_eq!(ui.command(&cmd("other.thing", vec![], None)).unwrap(), None);
    let asks = ui
        .command(&cmd("fixture.hello", vec![], None))
        .unwrap()
        .unwrap();
    assert_eq!(asks.dialog.as_deref(), Some("fixture.options"));
    let values = Json::obj(vec![("n", Json::Num(7.0))]);
    let said = ui
        .command(&cmd("fixture.hello", vec![], Some(values)))
        .unwrap()
        .unwrap();
    assert_eq!(said.status.as_deref(), Some("Fixture: hello 7"));
    assert_eq!(said.dialog, None);
    // The dialog's label follows the number and the selected task (#1610),
    // and Bump renames that task with the dialog kept open.
    let options = StateCtx {
        dialog_values: Some(Json::obj(vec![("n", Json::Num(7.0))])),
        ..ctx(vec![1], Some(("fixture.options".into(), None)))
    };
    let state = ui.state(&options).unwrap();
    assert_eq!(
        state.value("fixture.echo"),
        Some(&Json::str("Number 7 for Phase (?)"))
    );
    let bumped = ui
        .command(&Command {
            button: Some("bump".into()),
            ..cmd("fixture.hello", vec![1], Some(Json::Obj(Vec::new())))
        })
        .unwrap()
        .unwrap();
    assert_eq!(bumped.ops.len(), 1);
    assert_eq!(bumped.ops[0].verb, "task.set");
    assert_eq!(bumped.status.as_deref(), Some("Fixture: bumped task 1"));
    assert!(!bumped.close);
    // The layout hints (#1609) and Help.
    let opts = ui.manifest().dialog("fixture.options").unwrap();
    assert_eq!(opts.size, Some((460, 400)));
    assert_eq!(opts.pages[0].columns, 2);
    assert_eq!(opts.pages[0].controls[0].group, "Numbers");
    assert!(opts.pages[0].controls[4].fill);
    assert_eq!(
        ui.manifest().dialog("fixture.sheet").unwrap().placement,
        "right"
    );
    assert_eq!(
        ui.manifest().dialog("task-information").unwrap().header[0].name,
        "fixture.head"
    );
    assert_eq!(
        state.value("fixture.sample").and_then(|v| v.get("bold")),
        Some(&Json::Bool(true))
    );
    assert_eq!(opts.header[0].name, "fixture.title");
    assert_eq!(opts.header[0].span, 2);
    assert_eq!(opts.help, "");
    let sheet = ui.manifest().dialog("fixture.sheet").unwrap();
    assert_eq!(sheet.help, "https://example.test/fixture-help");
    assert_eq!(sheet.buttons.last().map(|b| b.id.as_str()), Some("help"));
    assert_eq!(
        ui.manifest().dialog("task-information").unwrap().pages[0].columns,
        2
    );
    // The header's declared value reaches the echo label.
    let with_title = StateCtx {
        dialog_values: Some(Json::obj(vec![
            ("n", Json::Num(7.0)),
            ("fixture.title", Json::str("Plan")),
        ])),
        ..ctx(vec![1], Some(("fixture.options".into(), None)))
    };
    assert_eq!(
        ui.state(&with_title).unwrap().value("fixture.echo"),
        Some(&Json::str("Number 7 for Phase (Plan)"))
    );

    // The overlay: a label at the first bar, a link from the first bar's
    // end to the second bar's start.
    let row = |uid, y, bar| OverlayRow {
        uid,
        row_y: y,
        bar,
        kind: "on-track".into(),
        ..OverlayRow::default()
    };
    let overlay = OverlayCtx {
        rows: vec![
            row(1, 0.0, Some((22.0, 5.0, 66.0, 14.0))),
            row(2, 28.0, None),
            row(3, 56.0, Some((110.0, 61.0, 44.0, 14.0))),
        ],
        origin_day: 20000,
        day_w: 22.0,
        row_h: 28.0,
        links: Vec::new(),
        status_date: None,
        current_date: None,
        flags: Json::Obj(Vec::new()),
        ..OverlayCtx::default()
    };
    let shapes = ui.overlay(&overlay).unwrap();
    assert_eq!(shapes.len(), 3);
    assert!(matches!(&shapes[0], Shape::Text { text, x, .. } if text == "fixture" && *x == 22.0));
    assert_eq!(
        shapes[1],
        Shape::Polyline {
            points: vec![(88.0, 12.0), (94.0, 12.0), (94.0, 68.0)],
            color: 0,
            width: 1.0
        }
    );
    assert_eq!(
        shapes[2],
        Shape::Arrow {
            from: (94.0, 68.0),
            to: (110.0, 68.0),
            color: 0,
            width: 1.0
        }
    );
    let one_bar = OverlayCtx {
        rows: overlay.rows[..2].to_vec(),
        ..overlay
    };
    assert_eq!(ui.overlay(&one_bar).unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// A host the target does not match skips the library before any other
/// symbol; a host that gives no name loads the formats without the UI.
#[test]
fn the_version_target_gates_the_library() {
    let dir = fixture_plugin::plugin_dir("projcore-plugin-target");
    let other = Host {
        name: "suite",
        version: "0.2.0",
    };
    let loaded = load_plugins_for(std::slice::from_ref(&dir), Some(other));
    assert!(loaded.formats.is_empty() && loaded.ui.is_empty() && loaded.plugins.is_empty());
    assert_eq!(loaded.errors.len(), 1);
    assert!(
        loaded.errors[0].ends_with("targets suite 0.1.*, this is 0.2.0"),
        "{}",
        loaded.errors[0]
    );
    let unknown = Host {
        name: "docxy",
        version: "0.5.0",
    };
    let loaded = load_plugins_for(std::slice::from_ref(&dir), Some(unknown));
    assert!(
        loaded.errors[0].contains("targets no docxy"),
        "{}",
        loaded.errors[0]
    );
    let nameless = load_plugins_for(std::slice::from_ref(&dir), None);
    assert!(nameless.errors.is_empty());
    assert_eq!(nameless.formats.len(), 1);
    assert!(nameless.ui.is_empty());
    assert!(!nameless.plugins[0].ui);
    assert!(nameless.plugins[0].target.is_some());
    let _ = std::fs::remove_dir_all(dir);
}
