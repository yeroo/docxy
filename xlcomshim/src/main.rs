//! Binary entry point for the xlcomshim COM LocalServer32. The COM
//! implementation lives in the `xlcomshim` library, which is also built as a
//! cdylib (InprocServer32) so the same objects serve both activation styles.

#[cfg(not(windows))]
fn main() {
    eprintln!("xlcomshim is a Windows COM server and only runs on Windows.");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    xlcomshim::run()
}
