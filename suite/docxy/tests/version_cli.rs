//! `suite --version` prints the build block and exits without a window (#1023).

use std::process::Command;

#[test]
fn version_prints_the_build_block_and_exits() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_suite"))
            .arg(flag)
            // Nothing here may reach a display or the real config.
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .expect("run suite");
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next().unwrap_or("");
        assert_eq!(
            first,
            concat!("docxy suite ", env!("CARGO_PKG_VERSION")),
            "{text}"
        );
        for key in ["commit:", "last PR:", "kind:", "dirty:", "built:"] {
            assert!(text.contains(key), "{flag}: missing {key} in {text}");
        }
        let kind = text.lines().find(|l| l.starts_with("kind:")).unwrap();
        assert!(
            ["release", "ci", "local"].iter().any(|k| kind.ends_with(k)),
            "{kind}"
        );
        if kind.ends_with("local") {
            assert!(text.lines().any(|l| l == "manual build"), "{text}");
        }
    }
}
