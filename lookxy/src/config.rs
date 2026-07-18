//! `Config` — user-configurable settings, with defaults baked in.
//!
//! Nothing here is required for lookxy to run: the whole file is optional,
//! and every field has a sane built-in default. Precedence, highest wins:
//!
//! 1. environment variables (`LOOKXY_CLIENT_ID`, `LOOKXY_BACKFILL_DAYS`,
//!    `LOOKXY_REFRESH_SECS`, `LOOKXY_THREADED`)
//! 2. `%APPDATA%\lookxy\config.json` (if present and parseable)
//! 3. the built-in defaults below
//!
//! A missing or unparsable config file, or an unparsable env var, is
//! silently skipped in favor of whatever precedes it in that order — a mail
//! client shouldn't refuse to start over a malformed settings file.

use std::path::{Path, PathBuf};

/// User-configurable settings: which app registration to authenticate as,
/// how many days of history the sync engine backfills on first run, and how
/// often it ticks.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub client_id: String,
    pub backfill_days: i64,
    pub refresh_secs: u64,
    /// Whether the folder message-list is grouped into conversations. Toggled
    /// at runtime with `t` (persisted via `persist_threaded`).
    pub threaded: bool,
}

impl Default for Config {
    /// The same defaults `AuthConfig::default()`'s client id and the old
    /// `main::BACKFILL_DAYS` constant used before `Config` existed: the
    /// validated Microsoft Graph CLI client id (a public client
    /// preauthorized for Graph) and generous backfill/refresh intervals.
    fn default() -> Config {
        Config {
            client_id: "14d82eec-204b-4c2f-b7e8-296a70dab67e".to_string(),
            backfill_days: 180,
            refresh_secs: 60,
            threaded: true,
        }
    }
}

impl Config {
    /// Builds the effective config: [`Config::default`], overlaid with
    /// `%APPDATA%\lookxy\config.json` (or, if `path` is `Some`, that file
    /// instead — the seam tests use to point at a fixture without touching
    /// the real `%APPDATA%`), overlaid with env vars. `path: None` is the
    /// real entry point `main` uses; a missing file (the common case — the
    /// file is entirely optional) just leaves the defaults/env overlay in
    /// place.
    pub fn load_from(path: Option<&Path>) -> Config {
        let mut cfg = Config::default();

        let file_path = match path {
            Some(p) => Some(p.to_path_buf()),
            None => config_file_path(),
        };
        if let Some(p) = file_path
            && let Ok(text) = std::fs::read_to_string(&p)
        {
            cfg.overlay_json(&text);
        }

        cfg.overlay_env();
        cfg
    }

    /// Overlays whichever of `client_id`/`backfill_days`/`refresh_secs` are
    /// present with the right shape (and pass [`is_sane_backfill_days`]/
    /// [`is_sane_refresh_secs`]) in the parsed JSON object; anything else in
    /// the file (unknown keys, wrong types, out-of-range numbers, or the
    /// file failing to parse as JSON at all) is silently ignored rather than
    /// treated as an error — the current (lower-precedence) value is kept.
    fn overlay_json(&mut self, text: &str) {
        let Ok(value) = mailcore::json::parse(text) else {
            return;
        };
        if let Some(s) = value.get("client_id").and_then(|v| v.as_str()) {
            self.client_id = s.to_string();
        }
        if let Some(n) = value
            .get("backfill_days")
            .and_then(|v| v.as_i64())
            .filter(|n| is_sane_backfill_days(*n))
        {
            self.backfill_days = n;
        }
        if let Some(n) = value
            .get("refresh_secs")
            .and_then(|v| v.as_i64())
            .filter(|n| is_sane_refresh_secs_i64(*n))
        {
            self.refresh_secs = n as u64;
        }
        if let Some(b) = value.get("threaded").and_then(|v| v.as_bool()) {
            self.threaded = b;
        }
    }

    /// Overlays `LOOKXY_CLIENT_ID`/`LOOKXY_BACKFILL_DAYS`/`LOOKXY_REFRESH_SECS`
    /// if set, parseable, and (for the numeric ones) sane per
    /// [`is_sane_backfill_days`]/[`is_sane_refresh_secs`]; an empty,
    /// unparsable, or out-of-range value leaves whatever the file/default
    /// overlay already set.
    fn overlay_env(&mut self) {
        if let Ok(v) = std::env::var("LOOKXY_CLIENT_ID")
            && !v.is_empty()
        {
            self.client_id = v;
        }
        if let Ok(v) = std::env::var("LOOKXY_BACKFILL_DAYS")
            && let Ok(n) = v.parse::<i64>()
            && is_sane_backfill_days(n)
        {
            self.backfill_days = n;
        }
        if let Ok(v) = std::env::var("LOOKXY_REFRESH_SECS")
            && let Ok(n) = v.parse::<u64>()
            && is_sane_refresh_secs(n)
        {
            self.refresh_secs = n;
        }
        if let Ok(v) = std::env::var("LOOKXY_THREADED") {
            let v = v.trim();
            if v.eq_ignore_ascii_case("true") || v == "1" {
                self.threaded = true;
            } else if v.eq_ignore_ascii_case("false") || v == "0" {
                self.threaded = false;
            }
        }
    }
}

/// A backfill window only makes sense as at least one day; zero or negative
/// is meaningless (and would otherwise silently disable backfill or, worse,
/// underflow downstream date arithmetic).
fn is_sane_backfill_days(n: i64) -> bool {
    n >= 1
}

/// A refresh interval of zero (or negative) isn't a faster poll — it would
/// either busy-loop or, cast to `u64` from a negative `i64`, wrap around to
/// an astronomically large value that effectively never refreshes. Only
/// strictly-positive values are accepted.
fn is_sane_refresh_secs(n: u64) -> bool {
    n > 0
}

/// [`is_sane_refresh_secs`], but for the raw `i64` the JSON overlay reads
/// (checked *before* the `as u64` cast, so a negative value is rejected
/// outright instead of wrapping first).
fn is_sane_refresh_secs_i64(n: i64) -> bool {
    n > 0
}

/// `%APPDATA%\lookxy\config.json` (or, off Windows,
/// `$HOME/.config/lookxy/config.json`) — `None` if the base directory
/// variable isn't set, in which case `load_from` just skips the file
/// overlay.
pub fn config_file_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|base| PathBuf::from(base).join("lookxy").join("config.json"))
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("lookxy")
                .join("config.json")
        })
    }
}

/// Best-effort persistence of the `threaded` toggle to the real config file.
/// Silently does nothing if the config path can't be determined or the write
/// fails — a UI toggle must never crash or block on a settings-file problem.
///
/// Not called from production code — `App::toggle_threaded` (the `t`
/// keybinding) calls `persist_threaded_to` directly instead, since it already
/// has the resolved path cached in `config_path` and would otherwise pay to
/// re-resolve it on every toggle. Kept for callers without a cached path
/// (only this module's tests, currently); silences `dead_code`.
#[allow(dead_code)]
pub fn persist_threaded(value: bool) {
    if let Some(path) = config_file_path() {
        let _ = persist_threaded_to(&path, value);
    }
}

/// Read-modify-write `path`, replacing only the `threaded` key and preserving
/// every other key already in the file (client_id, backfill_days, unknown
/// keys, …). Creates the file (and parent dir) if absent.
///
/// Called directly by `App::toggle_threaded` (the `t` keybinding), bypassing
/// `persist_threaded` above since `App` already has the resolved path in
/// `config_path`.
pub fn persist_threaded_to(path: &Path, value: bool) -> std::io::Result<()> {
    use mailcore::json::Value;

    // Start from the file's existing object (or an empty one).
    let mut entries: Vec<(String, Value)> = match std::fs::read_to_string(path) {
        Ok(text) => match mailcore::json::parse(&text) {
            Ok(Value::Object(e)) => e,
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    entries.retain(|(k, _)| k != "threaded");
    entries.push(("threaded".to_string(), Value::Bool(value)));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, Value::Object(entries).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes every test in this module — env vars are process-global
    /// and shared across every test in this binary (which run in parallel
    /// by default), and `Config::load_from` *always* reads them as its last
    /// overlay step, so even a test that never sets a `LOOKXY_*` var itself
    /// can observe one left set by another test running concurrently.
    /// Taking this lock up front and clearing all three vars (see
    /// `clear_env`) makes every test below see a clean, private view.
    ///
    /// `unwrap_or_else(|e| e.into_inner())` rather than a plain `.unwrap()`:
    /// if some future test panics while holding the lock, the mutex is
    /// "poisoned" and a bare `.unwrap()` on every later test's `lock()` call
    /// would panic too, cascading one failure into every other env test in
    /// the suite. Recovering the guard instead lets the rest of the module
    /// run (and fail independently, if they do) rather than all going red
    /// together.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn clear_env() {
        unsafe {
            std::env::remove_var("LOOKXY_CLIENT_ID");
            std::env::remove_var("LOOKXY_BACKFILL_DAYS");
            std::env::remove_var("LOOKXY_REFRESH_SECS");
            std::env::remove_var("LOOKXY_THREADED");
        }
    }

    #[test]
    fn defaults_when_no_file_and_env_overrides() {
        let _guard = lock_env();
        clear_env();
        let c = Config::load_from(None);
        assert_eq!(c.backfill_days, 180);
        unsafe {
            std::env::set_var("LOOKXY_BACKFILL_DAYS", "30");
        }
        let c2 = Config::load_from(None);
        assert_eq!(c2.backfill_days, 30);
        unsafe {
            std::env::remove_var("LOOKXY_BACKFILL_DAYS");
        }
    }

    #[test]
    fn defaults_are_the_documented_baked_in_values() {
        let c = Config::default();
        assert_eq!(c.client_id, "14d82eec-204b-4c2f-b7e8-296a70dab67e");
        assert_eq!(c.backfill_days, 180);
        assert_eq!(c.refresh_secs, 60);
    }

    #[test]
    fn file_overlay_wins_over_defaults() {
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-file-overlay-wins-over-defaults",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"client_id":"custom-client","backfill_days":7,"refresh_secs":15}"#,
        )
        .expect("write fixture config");

        let c = Config::load_from(Some(&path));

        assert_eq!(c.client_id, "custom-client");
        assert_eq!(c.backfill_days, 7);
        assert_eq!(c.refresh_secs, 15);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let _guard = lock_env();
        clear_env();
        let path = std::env::temp_dir().join("lookxy-config-test-does-not-exist.json");
        let _ = std::fs::remove_file(&path);

        let c = Config::load_from(Some(&path));

        assert_eq!(c, Config::default());
    }

    #[test]
    fn env_overlay_wins_over_file() {
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-env-overlay-wins-over-file",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"backfill_days":7}"#).expect("write fixture config");

        unsafe {
            std::env::set_var("LOOKXY_BACKFILL_DAYS", "99");
        }
        let c = Config::load_from(Some(&path));
        unsafe {
            std::env::remove_var("LOOKXY_BACKFILL_DAYS");
        }

        assert_eq!(c.backfill_days, 99);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn negative_refresh_secs_in_file_falls_back_to_default() {
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-negative-refresh-secs-in-file",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("config.json");
        // `-5` would wrap to a huge `u64` if cast unchecked — must be
        // rejected outright and fall back to the default (60), not wrap.
        std::fs::write(&path, r#"{"refresh_secs":-5}"#).expect("write fixture config");

        let c = Config::load_from(Some(&path));

        assert_eq!(c.refresh_secs, 60);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_backfill_days_in_file_falls_back_to_default() {
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-zero-backfill-days-in-file",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"backfill_days":0}"#).expect("write fixture config");

        let c = Config::load_from(Some(&path));

        assert_eq!(c.backfill_days, 180);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_refresh_secs_env_is_rejected() {
        let _guard = lock_env();
        clear_env();
        unsafe {
            std::env::set_var("LOOKXY_REFRESH_SECS", "0");
        }
        let c = Config::load_from(Some(Path::new(
            "lookxy-config-test-does-not-exist-either.json",
        )));
        unsafe {
            std::env::remove_var("LOOKXY_REFRESH_SECS");
        }

        assert_eq!(c.refresh_secs, 60);
    }

    #[test]
    fn negative_backfill_days_env_is_rejected() {
        let _guard = lock_env();
        clear_env();
        unsafe {
            std::env::set_var("LOOKXY_BACKFILL_DAYS", "-1");
        }
        let c = Config::load_from(Some(Path::new(
            "lookxy-config-test-does-not-exist-either.json",
        )));
        unsafe {
            std::env::remove_var("LOOKXY_BACKFILL_DAYS");
        }

        assert_eq!(c.backfill_days, 180);
    }

    #[test]
    fn zero_backfill_days_env_is_rejected() {
        let _guard = lock_env();
        clear_env();
        unsafe {
            std::env::set_var("LOOKXY_BACKFILL_DAYS", "0");
        }
        let c = Config::load_from(Some(Path::new(
            "lookxy-config-test-does-not-exist-either.json",
        )));
        unsafe {
            std::env::remove_var("LOOKXY_BACKFILL_DAYS");
        }

        assert_eq!(c.backfill_days, 180);
    }

    #[test]
    fn threaded_defaults_true_and_file_overlay_can_disable() {
        assert!(Config::default().threaded);
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-threaded-overlay",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"threaded":false}"#).unwrap();
        let c = Config::load_from(Some(&path));
        assert!(!c.threaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_threaded_roundtrips_and_preserves_other_keys() {
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-persist-threaded",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"client_id":"keep-me","backfill_days":7}"#).unwrap();

        persist_threaded_to(&path, false).unwrap();

        let c = Config::load_from(Some(&path));
        assert!(!c.threaded); // the toggle was written
        assert_eq!(c.client_id, "keep-me"); // other keys preserved
        assert_eq!(c.backfill_days, 7);

        persist_threaded_to(&path, true).unwrap();
        assert!(Config::load_from(Some(&path)).threaded);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
