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

/// `--command <id>` runs a UI plugin's command headless (#1532): the
/// fixture's `fixture.rename` renames the first task through a `task.set`
/// op, `--save` writes the result, and the status is printed. A command no
/// plugin owns, or a plugin built for another host version, fails.
#[test]
fn a_ui_plugin_command_runs_headless() {
    let plugins = fixture_plugin::plugin_dir("yppxy-plugin-ui");
    let dir = work_dir("yppxy-plugin-command");
    let input = dir.join("plan.xml");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/10-summary.xml"),
        &input,
    )
    .unwrap();
    let out = dir.join("renamed.xml");
    let output = yppxy(
        Some(&plugins),
        &[
            &input,
            Path::new("--command"),
            Path::new("fixture.rename"),
            Path::new("--save"),
            &out,
        ],
    );
    ok(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("Fixture: renamed task "), "{stdout}");
    let plan = projcore::mspdi::read_mspdi(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(plan.tasks[0].name, "Renamed by fixture");
    assert!(
        plan.tasks[1..]
            .iter()
            .all(|t| t.name != "Renamed by fixture")
    );

    // #1575 r1: the request's `view.gridlines` carries the plan's lines, so
    // the fixture's `fixture.format` sets them on a plain plan and, seeing
    // them set in a saved `.yppx`, clears them again.
    let set = dir.join("set.yppx");
    let output = yppxy(
        Some(&plugins),
        &[
            &input,
            Path::new("--command"),
            Path::new("fixture.format"),
            Path::new("--save"),
            &set,
        ],
    );
    ok(&output);
    let plan = projcore::yppx::read_yppx(&std::fs::read(&set).unwrap()).unwrap();
    assert_eq!(
        plan.formatting
            .gridlines
            .get(projcore::formatting::gridlines::Line::GanttRows)
            .line_type,
        projcore::formatting::LineType::Dotted
    );
    let cleared = dir.join("cleared.yppx");
    let output = yppxy(
        Some(&plugins),
        &[
            &set,
            Path::new("--command"),
            Path::new("fixture.format"),
            Path::new("--save"),
            &cleared,
        ],
    );
    ok(&output);
    let plan = projcore::yppx::read_yppx(&std::fs::read(&cleared).unwrap()).unwrap();
    assert_eq!(plan.formatting.gridlines.set_lines().count(), 0);

    // #1695: the request's `view.layout` carries the plan's Layout, so
    // `fixture.layout` sets it and, seeing a bar height in a saved `.yppx`,
    // clears it again.
    let laid = dir.join("laid.yppx");
    let output = yppxy(
        Some(&plugins),
        &[
            &input,
            Path::new("--command"),
            Path::new("fixture.layout"),
            Path::new("--save"),
            &laid,
        ],
    );
    ok(&output);
    let plan = projcore::yppx::read_yppx(&std::fs::read(&laid).unwrap()).unwrap();
    assert_eq!(plan.formatting.layout.bar_height, Some(12));
    assert_eq!(plan.formatting.layout.rollup, Some(true));
    let unlaid = dir.join("unlaid.yppx");
    let output = yppxy(
        Some(&plugins),
        &[
            &laid,
            Path::new("--command"),
            Path::new("fixture.layout"),
            Path::new("--save"),
            &unlaid,
        ],
    );
    ok(&output);
    let plan = projcore::yppx::read_yppx(&std::fs::read(&unlaid).unwrap()).unwrap();
    assert!(plan.formatting.layout.is_empty());

    // A status-only command saves nothing and says what the plugin said.
    let output = yppxy(
        Some(&plugins),
        &[&input, Path::new("--command"), Path::new("fixture.flag")],
    );
    ok(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Fixture: flag on"
    );

    // No plugin owns the id.
    let output = yppxy(
        Some(&plugins),
        &[&input, Path::new("--command"), Path::new("nobody.home")],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no plugin runs the command 'nobody.home'"),
        "{stderr}"
    );

    // Without the plugin directory there is no UI plugin at all.
    let output = yppxy(
        None,
        &[&input, Path::new("--command"), Path::new("fixture.rename")],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("(0 UI plugins loaded)"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Rich task notes (#1801), the downstream plugin's loop: a command reads a
/// task's markup from the request's `view.rich_notes` (MSPDI has the plain
/// text only) and writes it back through `task.set rich_notes`; a `.yppx`
/// save keeps it, and the next press reads the stored markup back.
#[test]
fn a_ui_plugin_reads_and_writes_rich_notes() {
    let plugins = fixture_plugin::plugin_dir("yppxy-plugin-rich-notes");
    let dir = work_dir("yppxy-rich-notes-work");
    let input = dir.join("plan.xml");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi/10-summary.xml"),
        &input,
    )
    .unwrap();
    let press = |from: &Path, to: &Path| {
        let output = yppxy(
            Some(&plugins),
            &[
                from,
                Path::new("--command"),
                Path::new("fixture.richnote"),
                Path::new("--save"),
                to,
            ],
        );
        ok(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with("Fixture: rich note on task "),
            "{stdout}"
        );
        let plan = projcore::yppx::read_yppx(&std::fs::read(to).unwrap()).unwrap();
        let noted: Vec<_> = plan
            .tasks
            .iter()
            .filter(|t| t.rich_notes.is_some())
            .map(|t| (t.uid, t.notes.clone(), t.rich_notes.clone()))
            .collect();
        assert_eq!(noted.len(), 1, "{noted:?}");
        noted.into_iter().next().unwrap()
    };
    // No rich notes yet: the plugin bolds the plain notes it reads from MSPDI.
    let once = dir.join("once.yppx");
    let (uid, notes, rich) = press(&input, &once);
    let plain = notes.clone().unwrap();
    let bold = plain
        .split('\n')
        .map(|l| format!("<p><b>{l}</b></p>"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(rich.as_deref(), Some(bold.as_str()));
    // Reopened, the view hands the stored markup back, which the plugin
    // extends: the bold survives, so it read the markup, not the plain text.
    let twice = dir.join("twice.yppx");
    let (uid2, notes, rich) = press(&once, &twice);
    assert_eq!(uid2, uid);
    assert_eq!(notes, Some(format!("{plain}\nagain")));
    assert_eq!(rich, Some(format!("{bold}\n<li><i>again</i></li>")));
    let _ = std::fs::remove_dir_all(dir);
}
