//! The first key typed into docxy in a terminal reaches it (#1204).
//!
//! At start docxy asks the terminal which image protocol it speaks
//! (ratatui-image's `Picker::from_query_stdio`), and that query's reader
//! thread reads stdin until it sees the answer to `ESC [ 5 n`. Unanswered,
//! it outlives its timeout and swallows the keys a person types next. The UI
//! harness's terminal answers it; this runs docxy in that terminal and types
//! one key straight after start, which the editor must handle exactly once.

use ctlcore::json::Json;
use std::path::PathBuf;
use uiharness::term::{Terminal, TerminalSpec};

#[test]
fn the_first_key_typed_after_start_is_handled() {
    let sandbox = std::env::temp_dir().join(format!(
        "docxy-pty-first-key-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&sandbox).unwrap();
    let file = sandbox.join("basic.docx");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../uiharness/fixtures/basic.docx"),
        &file,
    )
    .unwrap();
    let mut term = Terminal::new(
        TerminalSpec {
            app: "docxy".into(),
            args: Vec::new(),
        },
        PathBuf::from(env!("CARGO_BIN_EXE_docxy")),
        sandbox.clone(),
    );
    term.open(&file).expect("start docxy in a pseudo-terminal");
    let before = term.call("field-read", Json::obj(vec![])).unwrap();
    assert_eq!(before.get_usize("keys"), Some(0), "{before}");
    // `real-key` waits for the press to be handled and drawn, and errs when
    // it never is or arrives as more than one.
    term.call(
        "real-key",
        Json::obj(vec![("key", Json::Str("right".into()))]),
    )
    .expect("the first key reaches docxy");
    let after = term.call("field-read", Json::obj(vec![])).unwrap();
    assert_eq!(after.get_usize("keys"), Some(1), "{after}");
    term.stop();
    let _ = std::fs::remove_dir_all(&sandbox);
}
