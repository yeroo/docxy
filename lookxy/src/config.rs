//! `Config` — user-configurable settings, with defaults baked in.
//!
//! Nothing here is required for lookxy to run: the whole file is optional,
//! and every field has a sane built-in default. Precedence, highest wins:
//!
//! 1. environment variables (`LOOKXY_CLIENT_ID`, `LOOKXY_BACKFILL_DAYS`,
//!    `LOOKXY_REFRESH_SECS`)
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
    /// present with the right shape in the parsed JSON object; anything
    /// else in the file (unknown keys, wrong types, or the file failing to
    /// parse as JSON at all) is silently ignored rather than treated as an
    /// error.
    fn overlay_json(&mut self, text: &str) {
        let Ok(value) = mailcore::json::parse(text) else {
            return;
        };
        if let Some(s) = value.get("client_id").and_then(|v| v.as_str()) {
            self.client_id = s.to_string();
        }
        if let Some(n) = value.get("backfill_days").and_then(|v| v.as_i64()) {
            self.backfill_days = n;
        }
        if let Some(n) = value.get("refresh_secs").and_then(|v| v.as_i64()) {
            self.refresh_secs = n as u64;
        }
    }

    /// Overlays `LOOKXY_CLIENT_ID`/`LOOKXY_BACKFILL_DAYS`/`LOOKXY_REFRESH_SECS`
    /// if set and (for the numeric ones) parseable; an empty/unparsable
    /// value leaves whatever the file/default overlay already set.
    fn overlay_env(&mut self) {
        if let Ok(v) = std::env::var("LOOKXY_CLIENT_ID")
            && !v.is_empty()
        {
            self.client_id = v;
        }
        if let Ok(v) = std::env::var("LOOKXY_BACKFILL_DAYS")
            && let Ok(n) = v.parse::<i64>()
        {
            self.backfill_days = n;
        }
        if let Ok(v) = std::env::var("LOOKXY_REFRESH_SECS")
            && let Ok(n) = v.parse::<u64>()
        {
            self.refresh_secs = n;
        }
    }
}

/// `%APPDATA%\lookxy\config.json` (or, off Windows,
/// `$HOME/.config/lookxy/config.json`) — `None` if the base directory
/// variable isn't set, in which case `load_from` just skips the file
/// overlay.
fn config_file_path() -> Option<PathBuf> {
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
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        unsafe {
            std::env::remove_var("LOOKXY_CLIENT_ID");
            std::env::remove_var("LOOKXY_BACKFILL_DAYS");
            std::env::remove_var("LOOKXY_REFRESH_SECS");
        }
    }

    #[test]
    fn defaults_when_no_file_and_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let path = std::env::temp_dir().join("lookxy-config-test-does-not-exist.json");
        let _ = std::fs::remove_file(&path);

        let c = Config::load_from(Some(&path));

        assert_eq!(c, Config::default());
    }

    #[test]
    fn env_overlay_wins_over_file() {
        let _guard = ENV_LOCK.lock().unwrap();
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
}
