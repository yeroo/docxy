//! `yppxy` with a format plugin: the fixture (`projcore/tests/plugin_fixture`,
//! a `.fakeproj` type whose files are MSPDI passed through) in
//! `$YPPX_PLUGIN_DIR` makes `--save out.fakeproj` write MSPDI bytes.

#[path = "../../projcore/tests/support/fixture_plugin.rs"]
mod fixture_plugin;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// yppxy with no config-dir plugins, and the plugins in `plugins` when given.
fn yppxy(plugins: Option<&Path>, args: &[&Path]) -> Output {
    let no_config = std::env::temp_dir().join("yppxy-tests-no-config");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_yppxy"));
    cmd.env("APPDATA", &no_config)
        .env("XDG_CONFIG_HOME", &no_config)
        .env_remove("YPPX_PLUGIN_DIR");
    if let Some(dir) = plugins {
        cmd.env("YPPX_PLUGIN_DIR", dir);
    }
    cmd.args(args).output().unwrap()
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn save_writes_mspdi_through_the_fixture_plugin() {
    let plugins = fixture_plugin::plugin_dir("yppxy-plugin-dir");
    let dir = work_dir("yppxy-plugin-save");
    let input = dir.join("plan.xml");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/10-summary.xml"),
        &input,
    )
    .unwrap();
    let save = Path::new("--save");

    // What the built-in MSPDI writer makes of the plan.
    let expected = dir.join("expected.xml");
    ok(&yppxy(None, &[&input, save, &expected]));
    let expected = std::fs::read(&expected).unwrap();

    // `yppxy plan.xml --save out.fakeproj` writes exactly those MSPDI bytes.
    let out = dir.join("out.fakeproj");
    ok(&yppxy(Some(&plugins), &[&input, save, &out]));
    assert_eq!(std::fs::read(&out).unwrap(), expected);

    // Opened from the plugin's type and saved under it again, the plan's
    // file goes back to the plugin as the source.
    let again = dir.join("again.fakeproj");
    ok(&yppxy(Some(&plugins), &[&out, save, &again]));
    let text = std::fs::read_to_string(&again).unwrap();
    let note = format!("<!-- fixture: source {} bytes -->\n", expected.len());
    assert!(text.ends_with(&note), "{text}");
    let back = projcore::mspdi::read_mspdi(&text).unwrap();
    let names = |p: &projcore::Project| p.tasks.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    let plan = projcore::mspdi::read_mspdi(&String::from_utf8(expected).unwrap()).unwrap();
    assert_eq!(names(&back), names(&plan));

    // The refusal names the plugin's type; without the plugin it is the
    // built-in one and the file is not created.
    let refused = yppxy(Some(&plugins), &[&input, save, &dir.join("out.mpp")]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("can only be saved as .yppx, .xml (MSPDI) or .fakeproj"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let unplugged = dir.join("unplugged.fakeproj");
    let refused = yppxy(None, &[&input, save, &unplugged]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("Project schedules can only be saved as .yppx or .xml (MSPDI)")
    );
    assert!(!unplugged.exists());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_plugin_that_does_not_load_is_reported_and_skipped() {
    let plugins = fixture_plugin::plugin_dir("yppxy-plugin-dir-broken");
    let broken = plugins.join(format!("yppx_broken{}", std::env::consts::DLL_SUFFIX));
    std::fs::write(&broken, b"not a library").unwrap();
    let dir = work_dir("yppxy-plugin-broken");
    let out = dir.join("new.fakeproj");
    let output = yppxy(Some(&plugins), &[Path::new("--save"), &out]);
    ok(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("yppxy: plugin not loaded: ") && stderr.contains("yppx_broken"),
        "{stderr}"
    );
    assert!(projcore::mspdi::read_mspdi(&std::fs::read_to_string(&out).unwrap()).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}
