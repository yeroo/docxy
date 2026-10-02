//! Crash log (#733): leave evidence when the suite dies or degrades at a point
//! where nobody can see stderr.
//!
//! The release build is a `windows_subsystem = "windows"` app with no console,
//! so a panic message, and every `eprintln!` before an exit, goes nowhere. A
//! relaunch after `taskkill /F` once exited silently and never did again; this
//! module exists so the next one says why. [`install`] chains a panic hook that
//! appends to `<config root>/docxy/crash.log`, and [`startup`] records the
//! start-up failures that `main` otherwise only prints.
//!
//! ⚠️ A panic hook sees panics only. A stack overflow, an access violation in
//! native or GPU code, an abort and `taskkill` all end the process without it;
//! the exit code is the only evidence of those.
//!
//! Everything here takes the config root as a parameter, like `recover`, and
//! nothing here may fail start-up: every I/O error is ignored.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Past this size the log is moved to `crash.log.1` before the next append, so
/// a crash loop cannot fill the disk. One generation is kept.
pub(crate) const MAX_BYTES: u64 = 256 * 1024;

pub(crate) fn log_path(root: &Path) -> PathBuf {
    root.join("docxy").join("crash.log")
}

fn rotated_path(root: &Path) -> PathBuf {
    root.join("docxy").join("crash.log.1")
}

/// `secs` since the Unix epoch as RFC 3339 UTC, e.g. `2026-09-30T19:05:15Z`.
pub(crate) fn rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day): Howard
/// Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// One log entry: a header line naming what, when, which process and thread,
/// then `body`, then a blank line.
pub(crate) fn format_entry(
    kind: &str,
    now_secs: u64,
    pid: u32,
    thread: &str,
    body: &str,
) -> String {
    format!(
        "=== {kind} {} pid {pid} docxy {} thread {thread}\n{}\n\n",
        rfc3339_utc(now_secs),
        env!("CARGO_PKG_VERSION"),
        body.trim_end()
    )
}

/// The body of a panic entry.
pub(crate) fn panic_body(message: &str, location: Option<&str>, backtrace: &str) -> String {
    format!(
        "panicked at {}:\n{message}\nbacktrace:\n{backtrace}",
        location.unwrap_or("<unknown location>")
    )
}

/// Append `entry` to the log under `root`, rotating it first when it has grown
/// past [`MAX_BYTES`]. The entry goes out in one write on an append handle, so
/// a second process logging to the same root cannot split it.
pub(crate) fn append(root: &Path, entry: &str) {
    let path = log_path(root);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        // `rename` replaces an existing `crash.log.1` on every platform.
        let _ = std::fs::rename(&path, rotated_path(root));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(entry.as_bytes());
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string()
}

/// Record a start-up failure or degradation that `main` would otherwise only
/// print to a console the release build does not have.
pub(crate) fn startup(root: &Path, message: &str) {
    append(
        root,
        &format_entry(
            "startup",
            now_secs(),
            std::process::id(),
            &thread_name(),
            message,
        ),
    );
}

/// Chain a panic hook that appends every panic, on any thread, to the log
/// under `root`, then runs the hook that was installed before it — so console
/// and debug behaviour is unchanged. `main` calls this first.
///
/// No re-entry guard: std aborts on a panic inside a panic hook, so the hook
/// cannot recurse, and a panic while writing the log ends the process without
/// running the previous hook.
pub(crate) fn install(root: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let message = info.payload_as_str().unwrap_or("<non-string payload>");
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let body = panic_body(message, location.as_deref(), &backtrace);
        append(
            &root,
            &format_entry(
                "panic",
                now_secs(),
                std::process::id(),
                &thread_name(),
                &body,
            ),
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/crashlog-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        // A leap day, and the second before the next day.
        assert_eq!(rfc3339_utc(951_868_799), "2000-02-29T23:59:59Z");
        assert_eq!(rfc3339_utc(951_868_800), "2000-03-01T00:00:00Z");
        // New Year's Eve of a leap year, the 366th day.
        assert_eq!(rfc3339_utc(1_735_689_599), "2024-12-31T23:59:59Z");
        assert_eq!(rfc3339_utc(1_790_795_115), "2026-09-30T19:05:15Z");
    }

    #[test]
    fn an_entry_has_its_header_fields_and_ends_in_a_blank_line() {
        let e = format_entry("startup", 0, 4242, "main", "control unavailable\n");
        assert_eq!(
            e,
            format!(
                "=== startup 1970-01-01T00:00:00Z pid 4242 docxy {} thread main\n\
                 control unavailable\n\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn a_panic_body_names_the_location_message_and_backtrace() {
        let b = panic_body("boom", Some("src/main.rs:1:2"), "0: frame");
        assert_eq!(
            b,
            "panicked at src/main.rs:1:2:\nboom\nbacktrace:\n0: frame"
        );
        assert!(panic_body("boom", None, "").contains("<unknown location>"));
    }

    #[test]
    fn append_creates_the_directory_and_keeps_earlier_entries() {
        let root = Scratch::new();
        append(&root.0, "one\n");
        append(&root.0, "two\n");
        assert_eq!(
            std::fs::read_to_string(log_path(&root.0)).unwrap(),
            "one\ntwo\n"
        );
        assert!(
            !rotated_path(&root.0).exists(),
            "a small log is not rotated"
        );
    }

    #[test]
    fn a_log_past_the_limit_is_rotated_before_the_append() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("docxy")).unwrap();
        std::fs::write(rotated_path(&root.0), "older generation").unwrap();
        let big = "x".repeat(MAX_BYTES as usize + 1);
        std::fs::write(log_path(&root.0), &big).unwrap();
        append(&root.0, "fresh\n");
        assert_eq!(
            std::fs::read_to_string(log_path(&root.0)).unwrap(),
            "fresh\n"
        );
        assert_eq!(
            std::fs::read_to_string(rotated_path(&root.0)).unwrap(),
            big,
            "the full log replaces the older generation"
        );
    }

    #[test]
    fn a_log_at_exactly_the_limit_is_still_appended_to() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("docxy")).unwrap();
        std::fs::write(log_path(&root.0), "x".repeat(MAX_BYTES as usize)).unwrap();
        append(&root.0, "y");
        assert_eq!(
            std::fs::metadata(log_path(&root.0)).unwrap().len(),
            MAX_BYTES + 1
        );
        assert!(!rotated_path(&root.0).exists());
    }

    #[test]
    fn a_startup_entry_is_written_without_a_backtrace() {
        let root = Scratch::new();
        startup(&root.0, "docxy: Project control unavailable: denied");
        let log = std::fs::read_to_string(log_path(&root.0)).unwrap();
        assert!(log.starts_with("=== startup "), "{log}");
        assert!(
            log.contains(&format!("pid {}", std::process::id())),
            "{log}"
        );
        assert!(log.contains("Project control unavailable: denied"), "{log}");
        assert!(!log.contains("backtrace:"), "{log}");
    }

    /// Set only in the child process [`a_panic_is_written_to_the_crash_log`]
    /// starts; without it [`crash_child`] does nothing.
    const CHILD_ENV: &str = "DOCXY_CRASHLOG_TEST_CHILD";

    /// The child half of [`a_panic_is_written_to_the_crash_log`]. It installs
    /// the hook exactly as `main` does — the hook is process-global, so this
    /// must never run in the parent — then panics twice on a named thread and
    /// once on its own thread. That last message carries its line so the
    /// parent can check the logged location.
    #[test]
    fn crash_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        install(crate::config_root());
        let probe = std::thread::Builder::new()
            .name("crash-probe".into())
            .spawn(|| {
                // Two panics on one thread, the second after the first hook
                // has returned: on Windows, the "cannot unwind" panic that
                // follows one raised in a window callback. Both are logged.
                let _ = std::panic::catch_unwind(|| panic!("#733 probe first"));
                panic!("#733 probe second")
            })
            .unwrap();
        assert!(probe.join().is_err());
        panic!("#733 test thread panic at line {}", line!());
    }

    /// A panic, on the main path or another thread, lands in
    /// `<DOCXY_CONFIG_DIR>/docxy/crash.log` with its pid, thread, message,
    /// location and a backtrace. The only way to see a real hook fire without
    /// replacing this test binary's own hook is to re-run the binary.
    #[test]
    fn a_panic_is_written_to_the_crash_log() {
        let root = Scratch::new();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crashlog::tests::crash_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env(crate::CONFIG_DIR_ENV, &root.0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "the child test must fail: {stderr}");
        // The chained default hook still printed.
        assert!(stderr.contains("#733 test thread panic"), "{stderr}");

        let log = std::fs::read_to_string(log_path(&root.0)).unwrap_or_default();
        let entries: Vec<&str> = log.split("=== ").filter(|e| !e.is_empty()).collect();
        assert_eq!(entries.len(), 3, "one entry per panic:\n{log}");

        for (probe, message) in entries[..2]
            .iter()
            .zip(["#733 probe first", "#733 probe second"])
        {
            assert!(probe.starts_with("panic "), "{probe}");
            assert!(probe.contains(&format!(" pid {pid} ")), "{probe}");
            assert!(probe.contains(" thread crash-probe\n"), "{probe}");
            assert!(probe.contains(message), "{probe}");
            assert!(
                probe.contains(&format!("panicked at {}:", file!())),
                "{probe}"
            );
            assert!(probe.contains("backtrace:\n"), "{probe}");
        }

        let main = entries[2];
        assert!(main.contains(&format!(" pid {pid} ")), "{main}");
        let line: u32 = main
            .split("#733 test thread panic at line ")
            .nth(1)
            .and_then(|s| s.lines().next())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or_else(|| panic!("no message line in:\n{main}"));
        assert!(
            main.contains(&format!("panicked at {}:{line}:", file!())),
            "{main}"
        );
        assert!(main.contains("backtrace:\n"), "{main}");
    }

    /// A release crash.log backtrace names the source line of each frame only
    /// when the release build carries line tables (#795), so the workspace
    /// profile must keep them.
    #[test]
    fn release_profile_keeps_line_tables() {
        let manifest = include_str!("../../Cargo.toml");
        let mut in_release = false;
        let mut has_line_tables = false;
        for line in manifest.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_release = trimmed == "[profile.release]";
                continue;
            }
            if in_release && trimmed == "debug = \"line-tables-only\"" {
                has_line_tables = true;
            }
        }
        assert!(
            has_line_tables,
            "suite/Cargo.toml [profile.release] must set debug = \"line-tables-only\""
        );
    }

    /// The field crash.log resolves file:line only when suite.pdb sits next
    /// to suite.exe (#795), so the installer must ship it into {app}.
    #[test]
    fn the_suite_installer_ships_the_pdb() {
        let iss = include_str!("../../../packaging/inno/suite.iss");
        assert!(
            iss.lines().any(|line| {
                let line = line.trim();
                line.starts_with("Source: \"{#SrcDir}\\suite.pdb\"")
                    && line.contains("DestDir: \"{app}\"")
            }),
            "suite.iss [Files] must install suite.pdb next to suite.exe"
        );
    }
}
