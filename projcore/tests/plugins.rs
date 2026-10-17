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
            ("fixture.palette", ControlKind::Gallery),
        ]
    );
    assert!(group.controls[0].large && group.controls[0].keytip == "XH");
    assert_eq!(group.controls[2].swatches[0].color, Some(0x3366CC));
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
    };
    let state = ui.state(&ctx(Vec::new(), None)).unwrap();
    assert_eq!(state.control("fixture.flag").checked, Some(false));
    assert_eq!(state.control("fixture.rename").enabled, Some(false));
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
    };
    let flipped = ui
        .command(&cmd("fixture.flag", vec![], None))
        .unwrap()
        .unwrap();
    assert_eq!(flipped.status.as_deref(), Some("Fixture: flag on"));
    assert!(flipped.ops.is_empty());
    assert_eq!(
        ui.state(&ctx(Vec::new(), None))
            .unwrap()
            .control("fixture.flag")
            .checked,
        Some(true)
    );
    ui.command(&cmd("fixture.flag", vec![], None)).unwrap();
    let renamed = ui
        .command(&cmd("fixture.rename", vec![3, 4], None))
        .unwrap()
        .unwrap();
    assert_eq!(renamed.ops.len(), 1);
    assert_eq!(renamed.ops[0].verb, "task.set");
    assert_eq!(renamed.ops[0].args.int("uid"), Some(3));
    assert_eq!(renamed.ops[0].args.text("name"), "Renamed by fixture");
    assert_eq!(renamed.status.as_deref(), Some("Fixture: renamed task 3"));
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
