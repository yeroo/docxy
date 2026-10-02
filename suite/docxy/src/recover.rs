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

/// What one attempt by the timer to reach the app came to.
#[derive(Debug, PartialEq)]
pub(crate) enum Reach<T> {
    /// It ran, with this result.
    Done(T),
    /// The app was borrowed (a native modal dialog is pumping messages, or
    /// the app is quitting) but the view still exists: try again later.
    Busy,
    /// The view is gone: stop the timer.
    Gone,
}

/// Classify an attempt: `attempt` is its result if it ran; `alive` is asked
/// only when it did not, and says whether the view still exists.
pub(crate) fn reach<T>(attempt: Option<T>, alive: impl FnOnce() -> bool) -> Reach<T> {
    match attempt {
        Some(v) => Reach::Done(v),
        None if alive() => Reach::Busy,
        None => Reach::Gone,
    }
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

/// Excel keeps an unsaved workbook's draft for four days.
pub(crate) const DRAFT_RETENTION: Duration = Duration::from_secs(4 * 86_400);

/// Where workbooks closed with Don't Save keep their last AutoRecover copy (#613).
pub(crate) fn drafts_dir(root: &Path) -> PathBuf {
    root.join("docxy").join("drafts")
}

/// One kept draft, as Recover Unsaved Workbooks lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Draft {
    /// The file stem: the workbook's title and Excel's `((Unsaved-…))` stamp.
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    /// When it was kept (the file's modification time).
    pub(crate) saved: SystemTime,
}

/// The draft file name for a workbook titled `title`, kept at `secs` since the
/// epoch: `Book1 ((Unsaved-1759363200)).xlsx`, as Excel names its drafts. The
/// title loses its extension and any character a file name cannot hold.
pub(crate) fn draft_file_name(title: &str, secs: u64) -> String {
    // Sanitized before taking the stem: on Windows `a:b` would parse as a
    // drive prefix and lose the `a`.
    let safe: String = title
        .chars()
        .map(|c| {
            if c.is_control() || r#"\/:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = Path::new(&safe)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem.trim();
    let stem = if stem.is_empty() { "Book" } else { stem };
    format!("{stem} ((Unsaved-{secs})).xlsx")
}

/// Keep a copy of `sidecar` (a tab's last AutoRecover copy) as the draft of
/// the workbook titled `title`, and return where it went. The sidecar is
/// copied, never moved: the next persist rewrites it for whichever tab then
/// has its index. A name already taken gets `-2`, `-3`, … before `.xlsx`.
pub(crate) fn keep_draft(
    root: &Path,
    title: &str,
    sidecar: &Path,
    now: SystemTime,
) -> Result<PathBuf, String> {
    let bytes = std::fs::read(sidecar).map_err(|e| format!("draft not kept: {e}"))?;
    let dir = drafts_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("draft not kept: {e}"))?;
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = draft_file_name(title, secs);
    let base = name.trim_end_matches(".xlsx");
    let path = (1..)
        .map(|n| match n {
            1 => dir.join(&name),
            n => dir.join(format!("{base}-{n}.xlsx")),
        })
        .find(|p| !p.exists())
        .expect("an unbounded range always finds a free name");
    // Atomic: a half-written draft would be listed and fail to open.
    opccore::fsio::write_atomic(&path, &bytes).map_err(|e| format!("draft not kept: {e}"))?;
    Ok(path)
}

/// The drafts under `root`, newest first. Deletes drafts older than
/// [`DRAFT_RETENTION`] unless one of `open` (the tabs' paths) names it, so
/// call it on an explicit listing, not on every frame. A missing or
/// unreadable directory is an empty list.
pub(crate) fn list_drafts(root: &Path, now: SystemTime, open: &[PathBuf]) -> Vec<Draft> {
    let Ok(entries) = std::fs::read_dir(drafts_dir(root)) else {
        return Vec::new();
    };
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let open: Vec<PathBuf> = open.iter().map(|p| canonical(p)).collect();
    let mut drafts: Vec<Draft> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
        })
        .filter_map(|path| {
            let saved = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            let old = now
                .duration_since(saved)
                .is_ok_and(|age| age > DRAFT_RETENTION);
            if old && !open.contains(&canonical(&path)) {
                let _ = std::fs::remove_file(&path);
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(Draft { name, path, saved })
        })
        .collect();
    drafts.sort_by(|a, b| b.saved.cmp(&a.saved).then_with(|| a.name.cmp(&b.name)));
    drafts
}

/// How old a draft is, for the list: relative, like [`recovered_status`].
pub(crate) fn draft_age_label(saved: SystemTime, now: SystemTime) -> String {
    match now.duration_since(saved) {
        Ok(age) if age < Duration::from_secs(60) => "less than a minute ago".into(),
        Ok(age) if age < Duration::from_secs(2 * 3600) => {
            format!("{} min ago", age.as_secs() / 60)
        }
        Ok(age) if age < Duration::from_secs(2 * 86_400) => {
            format!("{} h ago", age.as_secs() / 3600)
        }
        Ok(age) => format!("{} days ago", age.as_secs() / 86_400),
        Err(_) => "just now".into(),
    }
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
    fn a_busy_app_is_retried_and_only_a_gone_view_stops_the_timer() {
        assert_eq!(
            reach(Some(3), || panic!("not asked on success")),
            Reach::Done(3)
        );
        assert_eq!(reach(None::<u8>, || true), Reach::Busy, "a modal dialog");
        assert_eq!(reach(None::<u8>, || false), Reach::Gone);
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

    fn set_age(path: &Path, now: SystemTime, age: Duration) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(now - age)
            .unwrap();
    }

    #[test]
    fn a_draft_name_is_the_title_without_extension_and_unsafe_characters() {
        assert_eq!(draft_file_name("Book1", 7), "Book1 ((Unsaved-7)).xlsx");
        assert_eq!(
            draft_file_name("Budget.xlsx", 7),
            "Budget ((Unsaved-7)).xlsx"
        );
        assert_eq!(
            draft_file_name("a:b*c?\"<x>|\u{1}.xlsx", 7),
            "a_b_c___x___ ((Unsaved-7)).xlsx"
        );
        assert_eq!(draft_file_name("", 7), "Book ((Unsaved-7)).xlsx");
        assert_eq!(draft_file_name("  .xlsx", 7), "Book ((Unsaved-7)).xlsx");
    }

    #[test]
    fn keeping_a_draft_copies_the_sidecar_and_never_overwrites_one() {
        let root = Scratch::new();
        let sidecar = root.0.join("tab-0.xlsx");
        std::fs::write(&sidecar, b"first").unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let a = keep_draft(&root.0, "Book1", &sidecar, now).unwrap();
        assert_eq!(a, drafts_dir(&root.0).join("Book1 ((Unsaved-1000)).xlsx"));
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert!(sidecar.exists(), "copied, not moved");
        std::fs::write(&sidecar, b"second").unwrap();
        let b = keep_draft(&root.0, "Book1", &sidecar, now).unwrap();
        assert_eq!(b, drafts_dir(&root.0).join("Book1 ((Unsaved-1000))-2.xlsx"));
        assert_eq!(std::fs::read(&a).unwrap(), b"first", "the first is kept");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");
        let gone = root.0.join("missing.xlsx");
        assert!(keep_draft(&root.0, "Book1", &gone, now).is_err());
    }

    #[test]
    fn drafts_list_newest_first_and_only_workbooks() {
        let root = Scratch::new();
        assert_eq!(list_drafts(&root.0, SystemTime::now(), &[]), vec![]);
        let dir = drafts_dir(&root.0);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for (name, age) in [("old.xlsx", 3600), ("new.xlsx", 60), ("note.txt", 0)] {
            let p = dir.join(name);
            std::fs::write(&p, b"x").unwrap();
            set_age(&p, now, Duration::from_secs(age));
        }
        std::fs::create_dir_all(dir.join("folder.xlsx")).unwrap();
        let names: Vec<_> = list_drafts(&root.0, now, &[])
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["new", "old"]);
    }

    #[test]
    fn drafts_older_than_four_days_are_deleted_unless_open() {
        let root = Scratch::new();
        let dir = drafts_dir(&root.0);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let day = Duration::from_secs(86_400);
        let stale = dir.join("stale.xlsx");
        let open = dir.join("open.xlsx");
        let fresh = dir.join("fresh.xlsx");
        for (p, age) in [(&stale, day * 5), (&open, day * 5), (&fresh, day * 3)] {
            std::fs::write(p, b"x").unwrap();
            set_age(p, now, age);
        }
        let names: Vec<_> = list_drafts(&root.0, now, std::slice::from_ref(&open))
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["fresh", "open"]);
        assert!(!stale.exists(), "pruned");
        assert!(open.exists(), "a draft open in a tab is never pruned");
    }

    #[test]
    fn a_draft_age_is_relative() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |s| draft_age_label(now - Duration::from_secs(s), now);
        assert_eq!(ago(5), "less than a minute ago");
        assert_eq!(ago(12 * 60), "12 min ago");
        assert_eq!(ago(5 * 3600), "5 h ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(
            draft_age_label(now + Duration::from_secs(5), now),
            "just now"
        );
    }
}
