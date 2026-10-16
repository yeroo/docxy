//! Build the fixture format plugin (`projcore/tests/plugin_fixture`) and put
//! it in a plugin directory under the name the loader takes. Shared, through
//! `#[path]`, by projcore's and yppxy's plugin tests.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The built fixture library. It is built once per test binary, into a
/// target directory of its own under `CARGO_TARGET_TMPDIR`, so the build
/// does not wait on the outer `cargo test`'s lock; it has no dependencies,
/// so that takes a second.
pub fn built_fixture() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugin-fixture");
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../projcore/tests/plugin_fixture/Cargo.toml");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target)
            .status()
            .expect("run cargo to build the fixture plugin");
        assert!(status.success(), "building the fixture plugin failed");
        let built = target
            .join("debug")
            .join(format!("{DLL_PREFIX}yppx_fixture{DLL_SUFFIX}"));
        assert!(built.is_file(), "no {}", built.display());
        built
    })
}

/// A fresh directory (`<temp>/<name>-<pid>`) holding the fixture as
/// `yppx_fixture.<dll|so|dylib>`, the only file in it.
pub fn plugin_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        built_fixture(),
        dir.join(format!("yppx_fixture{DLL_SUFFIX}")),
    )
    .unwrap();
    dir
}
