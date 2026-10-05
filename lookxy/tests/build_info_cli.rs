//! The MCP `version` carries the build info (#1023).

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn mcp_initialize_reports_the_long_version() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lookxy"))
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run lookxy --mcp");
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
