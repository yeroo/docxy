//! Where a run's evidence goes.
//!
//! A failing UI test is only useful if the picture it failed on can be found
//! afterwards, so every capture lands under the run's own directory, in a
//! folder named for the test that took it:
//!
//! ```text
//! <root>/<run>/drag-select-does-not-fill/007-cell-a1-c5.png
//!                                       /012-grid.png
//! ```
//!
//! A capture taken by a script is named for its step's line as well as its
//! region, so two steps that look at the same region — an assertion and the
//! `shot` after it — leave two pictures rather than one overwriting the other.
//!
//! The run directory is also the isolation boundary's other half: a harness
//! instance writes its `session.json` and hot sidecars under `DOCXY_CONFIG_DIR`,
//! and its evidence here. Nothing either side writes touches the installed app.

use crate::image::Image;
use std::path::{Path, PathBuf};

/// A file-system-safe version of `name`: anything that is not a letter, digit,
/// dot, dash or underscore becomes a dash, runs of dashes collapse, and the
/// result is trimmed and capped. Windows-reserved device basenames and trailing
/// dots are rewritten too, because these slugs become path components.
///
/// Test names are prose (`drag-to-select does not fill`) and region names carry
/// colons (`cell:A1:C5`) — a colon in a Windows path is a stream separator, so
/// a capture named straight from one would silently write somewhere else.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s = out.trim_matches('-');
    let s: String = s.chars().take(80).collect();
    let mut s = s.trim_end_matches(['-', '.', ' ']).to_string();
    // `.` and `..` survive the filter above, and a slug is joined beneath the
    // run directory as one path component — so `test ..` would resolve a
    // capture to `<run>/../002-grid.png` and truncate a same-named PNG outside
    // the evidence directory. Nothing else is only dots, so refusing the whole
    // class costs no legitimate name.
    if s.is_empty() {
        return "unnamed".to_string();
    }

    // Win32 treats these basenames as devices even when an extension follows
    // (`con.png` is still CON). Prefixing keeps the spelling recognizable while
    // making it an ordinary directory name. Do this after trimming trailing
    // dots so `CON.` cannot bypass the check.
    let stem = s.split('.').next().unwrap_or_default();
    let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
        || stem
            .strip_prefix("com")
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
        || stem
            .strip_prefix("lpt")
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
    if reserved {
        s.insert(0, '_');
        s.truncate(80);
        let trimmed_len = s.trim_end_matches(['-', '.', ' ']).len();
        s.truncate(trimmed_len);
    }
    s
}

/// One run's output directory.
pub struct Run {
    dir: PathBuf,
}

impl Run {
    /// A run rooted at `dir`, created if it is not there. The caller names it —
    /// usually `<something>/runs/<stamp>` — so a run never has to guess where
    /// the user wants its output.
    pub fn create(dir: impl Into<PathBuf>) -> std::io::Result<Run> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Run { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where a capture of `region`, taken by `test`, belongs.
    pub fn shot_path(&self, test: &str, region: &str) -> PathBuf {
        self.dir
            .join(slug(test))
            .join(format!("{}.png", slug(region)))
    }

    /// Save a capture and return the path, so a failure message can name it.
    pub fn save(&self, test: &str, region: &str, img: &Image) -> std::io::Result<PathBuf> {
        let path = self.shot_path(test, region);
        crate::png::write(&path, img)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_what_is_safe_and_replaces_what_is_not() {
        assert_eq!(slug("grid"), "grid");
        assert_eq!(
            slug("drag-to-select does not fill"),
            "drag-to-select-does-not-fill"
        );
        assert_eq!(slug("Cell.A1"), "cell.a1");
        assert_eq!(slug("keep_this"), "keep_this");
    }

    /// A colon is the one that matters on Windows: `cell:A1:C5.png` names an
    /// alternate data stream on `cell`, so the PNG would vanish rather than
    /// fail.
    #[test]
    fn slug_removes_the_characters_a_windows_path_would_swallow() {
        assert_eq!(slug("cell:A1:C5"), "cell-a1-c5");
        assert_eq!(slug("chart:0"), "chart-0");
        assert_eq!(slug(r#"a\b/c*d?e"f<g>h|i"#), "a-b-c-d-e-f-g-h-i");
    }

    #[test]
    fn slug_never_returns_an_empty_or_edge_dashed_name() {
        assert_eq!(slug(""), "unnamed");
        assert_eq!(slug(":::"), "unnamed");
        // A slug is joined beneath the run directory as one component, so a
        // name that is only dots would step out of it: `test ..` would resolve
        // its capture to `<run>/../<region>.png` and truncate a same-named PNG
        // outside the evidence directory.
        assert_eq!(slug(".."), "unnamed");
        assert_eq!(slug("."), "unnamed");
        assert_eq!(slug("..."), "unnamed");
        assert_eq!(slug(" .. "), "unnamed");
        // A dot that is part of a name is still a dot.
        assert_eq!(slug("a..b"), "a..b");
        assert_eq!(slug("  spaced  "), "spaced");
        assert_eq!(slug("--x--"), "x");
    }

    #[test]
    fn slug_avoids_windows_device_names_and_trailing_dot_aliases() {
        for name in [
            "CON", "con.txt", "PRN", "AUX.log", "NUL", "COM1", "LPT9.csv",
        ] {
            assert!(slug(name).starts_with('_'), "{name}: {}", slug(name));
        }
        assert_eq!(slug("smoke."), "smoke");
        assert_eq!(slug("smoke..."), "smoke");
        assert_eq!(slug("ordinary.txt"), "ordinary.txt");

        // Prefixing a capped reserved name must not expose a trailing dot and
        // let Win32 alias it with a different accepted slug.
        let capped_reserved = format!("con.{}.x", "a".repeat(74));
        let ordinary_alias = format!("_con.{}", "a".repeat(74));
        assert_eq!(slug(&capped_reserved), slug(&ordinary_alias));
        assert!(!slug(&capped_reserved).ends_with('.'));
    }

    #[test]
    fn slug_caps_a_runaway_name_without_leaving_a_trailing_dash() {
        let long = "a b ".repeat(60);
        let s = slug(&long);
        assert!(s.len() <= 80, "{} chars", s.len());
        assert!(!s.ends_with('-'), "{s}");
    }

    #[test]
    fn a_capture_lands_under_its_run_and_its_test() {
        let base = std::env::temp_dir().join(format!("uiharness-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let run = Run::create(&base).unwrap();
        let img = Image::new(3, 2);
        let p = run
            .save("drag-select does not fill", "cell:A1:C5", &img)
            .unwrap();
        assert_eq!(
            p,
            base.join("drag-select-does-not-fill")
                .join("cell-a1-c5.png")
        );
        assert!(p.is_file(), "the PNG is written, not just named");
        assert_eq!(run.shot_path("drag-select does not fill", "cell:A1:C5"), p);
        let _ = std::fs::remove_dir_all(&base);
    }
}
