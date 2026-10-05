//! `yppxy --version` prints the build block and the MCP `version` carries the long
//! string (#1023).

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn version_prints_the_build_block_and_exits() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_yppxy"))
            .arg(flag)
            .output()
            .expect("run yppxy");
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next().unwrap_or("");
        assert_eq!(
            first,
            concat!("yppxy ", env!("CARGO_PKG_VERSION")),
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
        // A local build is a manual build, and says so.
        if kind.ends_with("local") {
            assert!(text.lines().any(|l| l == "manual build"), "{text}");
        }
    }
}

#[test]
fn mcp_initialize_reports_the_long_version() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_yppxy"))
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run yppxy --mcp");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let want = format!("\"version\":\"{} (", env!("CARGO_PKG_VERSION"));
    assert!(text.contains(&want), "{text}");
    assert!(
        text.contains("local") || text.contains("ci") || text.contains("release"),
        "{text}"
    );
}
