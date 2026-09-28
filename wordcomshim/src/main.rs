//! Binary entry point for the wordcomshim COM LocalServer32. The COM
//! implementation lives in the `wordcomshim` library, which is also built as a
//! cdylib (InprocServer32) so the same objects serve both activation styles.
//!
//! ⚠️ Built for the Windows GUI subsystem: COM starts this exe for every
//! out-of-process client, and a console-subsystem exe would open a visible
//! console window on the user's screen for as long as the server runs. The
//! server prints nothing; logging goes to `%TEMP%` (see `comshimcore`). The
//! typelib tools in `src/bin/` stay console programs.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("wordcomshim is a Windows COM server and only runs on Windows.");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    wordcomshim::run()
}
