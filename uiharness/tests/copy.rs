use std::path::PathBuf;
use uiharness::{
    runner::copy_fixture,
    script::{Action, parse_script},
};

#[test]
fn copy_open_is_explicit_and_requires_a_path() {
    let script = parse_script("test copy\n open copy:../fixtures/gantt-summary.xml\n open \"copy:a file.xml\"\n open ordinary.xml\n").unwrap();
    assert_eq!(
        script.cases[0].steps[0].action,
        Action::OpenCopy("../fixtures/gantt-summary.xml".into())
    );
    assert_eq!(
        script.cases[0].steps[1].action,
        Action::OpenCopy("a file.xml".into())
    );
    assert_eq!(
        script.cases[0].steps[2].action,
        Action::Open("ordinary.xml".into())
    );
    assert!(parse_script("test bad\n open copy:\n").is_err());
}

#[test]
fn copied_fixture_keeps_its_name_and_isolated_sidecars_never_touch_the_original() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = root.join("fixtures/gantt-summary.xml");
    let original = std::fs::read(&source).unwrap();
    let sandbox = root.join("../target").join(format!(
        "copy-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let copy = copy_fixture(&source, &sandbox, "Export isolated").unwrap();
    assert_eq!(copy, sandbox.join("export-isolated/gantt-summary.xml"));
    assert_eq!(std::fs::read(&copy).unwrap(), original);
    assert!(copy_fixture(&source, &sandbox, "Export isolated").is_err());
    let output = copy.with_extension("md");
    std::fs::write(&output, "# Gantt export\n").unwrap();
    std::fs::write(&copy, "edited schedule").unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(output.file_name().unwrap(), "gantt-summary.md");
    assert!(output.starts_with(&sandbox));
}
