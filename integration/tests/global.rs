//! `host::global` and `host::start` in a process of their own: the test
//! binary holds one test, so nothing else starts the plugin.
#![cfg(feature = "host")]

use docxy_integration::host::{self, Deadlines, HostConfig, Source, Status};
use docxy_integration::protocol::App;

#[test]
fn a_read_before_start_leaves_start_to_load() {
    assert_eq!(host::global().status(), Status::NotConfigured);
    let dir = std::env::temp_dir().join(format!("dxi-global-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let var = format!("DXI_GLOBAL_TEST_DIR_{}", std::process::id());
    // SAFETY: this binary's one test, before any other thread reads the
    // environment.
    unsafe { std::env::set_var(&var, &dir) };
    let started = host::start(HostConfig {
        app: App::Suite,
        app_version: "1.2.3".into(),
        marker_dir: None,
        source: Source::Env(var),
        deadlines: Deadlines::default(),
    });
    // The folder holds no library: refused, so `start` did read the config.
    assert!(
        matches!(started.status(), Status::Refused { .. }),
        "{:?}",
        started.status()
    );
    assert!(std::ptr::eq(host::global(), started));
    host::shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
