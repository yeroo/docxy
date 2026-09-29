//! AutoRecover (#632): the periodic hot-exit write while tabs are unsaved, and
//! the run marker that tells a relaunch whether the previous run crashed.
//!
//! Everything here takes the config root as a parameter, so tests pass a
//! scratch directory and never go through `DOCXY_CONFIG_DIR`.
//!
//! ⚠️ The marker is one file per config root, not per process. Two suite
//! instances sharing a root already share one `session.json`; the second to
//! start sees the first's marker and labels its dirty restored tabs as
//! recovered. That only changes a status line, so it is left as is.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Word's default: save AutoRecover information every 10 minutes.
pub(crate) const DEFAULT_MINUTES: u32 = 10;

/// The intervals the Settings button cycles through; 0 is off.
pub(crate) const CHOICES: [u32; 7] = [0, 1, 2, 5, 10, 15, 30];

/// The longest the timer sleeps between checks, so a changed interval takes
/// effect within this long rather than after the old interval runs out.
pub(crate) const POLL: Duration = Duration::from_secs(30);

fn marker_path(root: &Path) -> PathBuf {
    root.join("docxy").join("running")
}

/// Whether the previous run under `root` ended without a clean exit. Read
/// before [`mark_running`] writes this run's marker.
pub(crate) fn was_unclean(root: &Path) -> bool {
    marker_path(root).exists()
}

/// Record that a run is in progress; [`clear_running`] removes it on a clean exit.
pub(crate) fn mark_running(root: &Path) {
    let p = marker_path(root);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let started = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(
        p,
        format!("pid {}\nstarted {started}\n", std::process::id()),
    );
}

pub(crate) fn clear_running(root: &Path) {
    let _ = std::fs::remove_file(marker_path(root));
}

/// The setting after `minutes` in [`CHOICES`], wrapping; a value not in the
/// list (a hand-edited session) goes to the first choice above it.
pub(crate) fn next_choice(minutes: u32) -> u32 {
    CHOICES
        .iter()
        .copied()
        .find(|&m| m > minutes)
        .unwrap_or(CHOICES[0])
}

pub(crate) fn choice_label(minutes: u32) -> String {
    match minutes {
        0 => "Off".into(),
        1 => "every minute".into(),
        m => format!("every {m} minutes"),
    }
}

/// Whether a tick is due `since` the last hot-exit write.
pub(crate) fn due(minutes: u32, since: Duration) -> bool {
    minutes > 0 && since >= Duration::from_secs(u64::from(minutes) * 60)
}

/// How long the timer sleeps before checking again, `since` the last write:
/// until the interval is up, but never longer than [`POLL`] (so a changed
/// setting is picked up) and never less than a second (no busy loop).
pub(crate) fn wake_after(minutes: u32, since: Duration) -> Duration {
    if minutes == 0 {
        return POLL;
    }
    let left = Duration::from_secs(u64::from(minutes) * 60).saturating_sub(since);
    left.clamp(Duration::from_secs(1), POLL)
}

/// The status of a tab restored from an AutoRecover copy after a crash.
///
/// The age is relative ("12 min ago") rather than a clock time: the suite has
/// no local-time-zone conversion, and a UTC clock time would read as wrong.
pub(crate) fn recovered_status(saved: Option<SystemTime>, now: SystemTime) -> String {
    let when = match saved.and_then(|s| now.duration_since(s).ok()) {
        Some(age) if age < Duration::from_secs(60) => "from less than a minute ago".into(),
        Some(age) if age < Duration::from_secs(2 * 3600) => {
            format!("from {} min ago", age.as_secs() / 60)
        }
        Some(age) if age < Duration::from_secs(2 * 86_400) => {
            format!("from {} h ago", age.as_secs() / 3600)
        }
        Some(age) => format!("from {} days ago", age.as_secs() / 86_400),
        None => "from the last session".into(),
    };
    format!(
        "recovered — AutoRecover copy {when}; Save to keep it, or close without saving to discard"
    )
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
                .join("../target/recover-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
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
    fn a_marker_left_behind_means_the_last_run_crashed() {
        let root = Scratch::new();
        assert!(!was_unclean(&root.0), "a fresh root has no previous run");
        mark_running(&root.0);
        assert!(was_unclean(&root.0), "killed before clear_running");
        clear_running(&root.0);
        assert!(!was_unclean(&root.0), "a clean exit removes it");
        clear_running(&root.0); // twice is harmless
    }

    #[test]
    fn a_tick_is_due_only_when_on_and_a_whole_interval_has_passed() {
        assert!(!due(0, Duration::from_secs(86_400)), "0 is off");
        assert!(!due(10, Duration::from_secs(599)));
        assert!(due(10, Duration::from_secs(600)));
        assert!(due(1, Duration::from_secs(61)));
    }

    #[test]
    fn the_timer_wakes_when_the_interval_is_up_and_at_least_every_poll() {
        let secs = Duration::from_secs;
        assert_eq!(wake_after(0, secs(0)), POLL, "off still polls the setting");
        assert_eq!(wake_after(10, secs(0)), POLL);
        assert_eq!(
            wake_after(1, secs(45)),
            secs(15),
            "wakes as the interval ends"
        );
        assert_eq!(
            wake_after(1, secs(60)),
            secs(1),
            "due now: no zero-length spin"
        );
        assert_eq!(wake_after(1, secs(600)), secs(1));
    }

    #[test]
    fn the_setting_cycles_through_the_choices_and_wraps() {
        let mut m = 0;
        let mut seen = vec![m];
        for _ in 0..CHOICES.len() - 1 {
            m = next_choice(m);
            seen.push(m);
        }
        assert_eq!(seen, CHOICES);
        assert_eq!(next_choice(30), 0);
        assert_eq!(next_choice(7), 10, "an odd hand-edited value moves up");
        assert_eq!(choice_label(0), "Off");
        assert_eq!(choice_label(10), "every 10 minutes");
    }

    #[test]
    fn the_recovered_status_says_recovered_and_never_saved() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let s = recovered_status(Some(now - Duration::from_secs(12 * 60)), now);
        assert!(s.starts_with("recovered"), "{s}");
        assert!(s.contains("12 min ago"), "{s}");
        assert!(
            !s.contains("saved "),
            "must not claim the file was saved: {s}"
        );
        assert!(recovered_status(None, now).contains("from the last session"));
        assert!(recovered_status(Some(now), now).contains("less than a minute"));
    }
}
