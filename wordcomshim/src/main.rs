//! Binary entry point for the wordcomshim COM LocalServer32. The COM
//! implementation lives in the `wordcomshim` library, which is also built as a
//! cdylib (InprocServer32) so the same objects serve both activation styles.

#[cfg(not(windows))]
fn main() {
    eprintln!("wordcomshim is a Windows COM server and only runs on Windows.");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    wordcomshim::run()
}
