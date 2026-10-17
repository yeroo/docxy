//! Format plugins end to end: the fixture plugin (`tests/plugin_fixture`) is
//! built, loaded from a plugin directory, registered, and a project
//! round-trips through it.
#![cfg(feature = "plugins")]

#[path = "support/fixture_plugin.rs"]
mod fixture_plugin;

use projcore::formats::Registry;
use projcore::formats::plugin::{load_plugin, load_plugins, load_plugins_with_errors};
use projcore::mspdi::{read_mspdi, write_mspdi};
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
