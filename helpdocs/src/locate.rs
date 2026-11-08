//! Where a terminal editor finds its manual (#1856): the one installed under
//! `<config root>/docxy/manual/<editor>/`, where the suite reads it too (#1855),
//! else the one built into the binary ([`crate::embed`]), else none.
//!
//! The config root is `DOCXY_CONFIG_DIR` when set and not empty, else the
//! platform's config directory as the suite's `dirs::config_dir` gives it,
//! read here from the environment: `%APPDATA%` on Windows, `$HOME/Library/
//! Application Support` on macOS, else `$XDG_CONFIG_HOME` when absolute, else
//! `$HOME/.config`. The environment and not the known-folder API, because a
//! terminal harness case points `APPDATA` into its own home, and must not
//! read the real user's installed manual.

use crate::Manual;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The config root override, the suite's too.
pub const CONFIG_DIR_ENV: &str = "DOCXY_CONFIG_DIR";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    /// Linux and the other Unixes (XDG).
    Other,
}

impl Os {
    pub const fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Other
        }
    }
}

/// The variables [`config_root_from`] reads.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `DOCXY_CONFIG_DIR`.
    pub over: Option<OsString>,
    pub appdata: Option<OsString>,
    pub home: Option<OsString>,
    pub xdg_config_home: Option<OsString>,
}

impl Env {
    pub fn read() -> Env {
        let var = std::env::var_os;
        Env {
            over: var(CONFIG_DIR_ENV),
            appdata: var("APPDATA"),
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
        }
    }
}

/// The config root `env` gives on `os`; `None` when there is none (no
/// override and the platform's variable unset or empty). An empty value is
/// unset: an exported-but-blank variable is a shell accident.
pub fn config_root_from(env: &Env, os: Os) -> Option<PathBuf> {
    let set = |v: &Option<OsString>| v.as_ref().filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(over) = set(&env.over) {
        return Some(over);
    }
    match os {
        Os::Windows => set(&env.appdata),
        Os::MacOs => set(&env.home).map(|h| h.join("Library").join("Application Support")),
        Os::Other => set(&env.xdg_config_home)
            .filter(|p| p.is_absolute())
            .or_else(|| set(&env.home).map(|h| h.join(".config"))),
    }
}

/// This process's config root ([`config_root_from`] on its environment).
pub fn config_root() -> Option<PathBuf> {
    config_root_from(&Env::read(), Os::current())
}

/// Where the config root `root` holds `editor`'s installed manual (`word`,
/// `excel` or `project`).
pub fn installed_dir(root: &Path, editor: &str) -> PathBuf {
    root.join("docxy").join("manual").join(editor)
}

/// The decision behind [`load`]: the manual installed under `root`, else the
/// `embedded` one; a bundle without pages counts as none. An installed one is
/// read through [`crate::bundle::files`], which follows no link.
pub fn find(root: Option<&Path>, editor: &str, embedded: &[(&str, &[u8])]) -> Option<Manual> {
    let has_pages = |m: Manual| (!m.pages.is_empty()).then_some(m);
    if let Some(root) = root {
        if let Ok(m) = Manual::load(&installed_dir(root, editor)) {
            if let Some(m) = has_pages(m) {
                return Some(m);
            }
        }
    }
    has_pages(Manual::from_files(embedded.iter().copied()))
}

/// `editor`'s manual: installed, else `embedded`, else none.
pub fn load(editor: &str, embedded: &[(&str, &[u8])]) -> Option<Manual> {
    find(config_root().as_deref(), editor, embedded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(over: &str, appdata: &str, home: &str, xdg: &str) -> Env {
        let v = |s: &str| (s != "-").then(|| OsString::from(s));
        Env {
            over: v(over),
            appdata: v(appdata),
            home: v(home),
            xdg_config_home: v(xdg),
        }
    }

    fn root(e: &Env, os: Os) -> Option<String> {
        config_root_from(e, os).map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    #[test]
    fn the_override_wins_unless_empty() {
        let e = env("/o", "/a", "/h", "/x");
        for os in [Os::Windows, Os::MacOs, Os::Other] {
            assert_eq!(root(&e, os).as_deref(), Some("/o"));
        }
        // Relative is honoured as typed.
        assert_eq!(
            root(&env("rel", "-", "-", "-"), Os::Other).as_deref(),
            Some("rel")
        );
        let e = env("", "/a", "/h", "/x");
        assert_eq!(root(&e, Os::Windows).as_deref(), Some("/a"));
    }

    #[test]
    fn each_platform_has_its_config_dir() {
        let e = env("-", "/a", "/h", "/x");
        assert_eq!(root(&e, Os::Windows).as_deref(), Some("/a"));
        assert_eq!(
            root(&e, Os::MacOs).as_deref(),
            Some("/h/Library/Application Support")
        );
        assert_eq!(root(&e, Os::Other).as_deref(), Some("/x"));
        // XDG must be absolute, as `dirs` has it; else HOME/.config.
        assert_eq!(
            root(&env("-", "-", "/h", "x"), Os::Other).as_deref(),
            Some("/h/.config")
        );
        assert_eq!(
            root(&env("-", "-", "/h", ""), Os::Other).as_deref(),
            Some("/h/.config")
        );
        // Nothing set: no root.
        let none = env("-", "", "-", "-");
        for os in [Os::Windows, Os::MacOs, Os::Other] {
            assert_eq!(root(&none, os), None);
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("helpdocs-locate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const EMBEDDED: &[(&str, &[u8])] = &[
        ("contents.txt", b"built\n"),
        ("built.md", b"---\ntitle: Built in\n---\nHi."),
    ];

    #[test]
    fn installed_beats_embedded_and_no_pages_is_none() {
        let root = temp("find");
        // Nothing installed: the embedded one.
        let m = find(Some(&root), "word", EMBEDDED).unwrap();
        assert_eq!(m.pages[0].id, "built");
        assert_eq!(find(None, "word", EMBEDDED).unwrap().pages[0].id, "built");
        // Installed but empty (no page): still the embedded one.
        let dir = installed_dir(&root, "word");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("contents.txt"), "").unwrap();
        assert_eq!(
            find(Some(&root), "word", EMBEDDED).unwrap().pages[0].id,
            "built"
        );
        // Installed with a page wins.
        std::fs::write(dir.join("contents.txt"), "mine\n").unwrap();
        std::fs::write(dir.join("mine.md"), "Mine.").unwrap();
        assert_eq!(
            find(Some(&root), "word", EMBEDDED).unwrap().pages[0].id,
            "mine"
        );
        // Another editor's directory is not this one's.
        assert_eq!(find(Some(&root), "excel", &[]), None);
        // Nothing anywhere: none.
        assert_eq!(find(None, "word", &[]), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
