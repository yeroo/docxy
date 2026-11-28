//! Windows: a plugin's own dependencies come from its folder, never from the
//! current directory. A test binary of its own, as it changes the current
//! directory.
#![cfg(all(windows, feature = "host"))]

use docxy_integration::host::{Deadlines, HostConfig, Source, Status, load_from};
use docxy_integration::protocol::App;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build `manifest` (relative to this package) into a target folder of its
/// own and return its `debug/<file>`.
fn build(manifest: &str, features: &[&str], target: &str, file: &str) -> PathBuf {
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join(target);
    let mut cargo = Command::new(env!("CARGO"));
    cargo
        .args(["build", "--quiet", "--manifest-path"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest))
        .arg("--target-dir")
        .arg(&target);
    for feature in features {
        cargo.args(["--features", feature]);
    }
    assert!(
        cargo.status().expect("run cargo").success(),
        "building {manifest} failed"
    );
    let built = target.join("debug").join(file);
    assert!(built.is_file(), "no {}", built.display());
    built
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dxi-flags-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_dependency_in_the_current_directory_is_not_loaded() {
    let plugin = build(
        "tests/integration_fixture/Cargo.toml",
        &["needs-dependency"],
        "integration-fixture-needs-dependency",
        "dxi_fixture.dll",
    );
    let needed = build(
        "tests/dependency_fixture/Cargo.toml",
        &[],
        "integration-dependency-fixture",
        "integration_dependency_fixture.dll",
    );
    let config = HostConfig {
        app: App::Suite,
        app_version: "1.2.3".into(),
        marker_dir: None,
        source: Source::Env(String::new()),
        deadlines: Deadlines::default(),
    };

    // The dependency only in the current directory: the load fails. This
    // case comes first, before any copy of the dependency is loaded, since
    // Windows takes a loaded module of the same name for a later import.
    let alone = scratch("alone");
    std::fs::copy(&plugin, alone.join("dxi_fixture.dll")).unwrap();
    let cwd = scratch("cwd");
    std::fs::copy(&needed, cwd.join("fixture_needed.dll")).unwrap();
    std::env::set_current_dir(&cwd).unwrap();
    let status = load_from(&alone, &config).status();
    assert!(
        matches!(&status, Status::Refused { why, .. } if why.contains("LoadLibraryExW")),
        "{status:?}"
    );

    // Beside the plugin, the same dependency loads.
    let beside = scratch("beside");
    std::fs::copy(&plugin, beside.join("dxi_fixture.dll")).unwrap();
    std::fs::copy(&needed, beside.join("fixture_needed.dll")).unwrap();
    let status = load_from(&beside, &config).status();
    assert!(matches!(status, Status::Loaded { .. }), "{status:?}");
}
