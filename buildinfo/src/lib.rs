//! Build info stamped at compile time: which commit, which merged PR, which kind
//! of build, and whether it is a "manual build". Dependency-free so every host
//! (the suite, docxy, xlsxy, yppxy, lookxy) can share it. Hosts that need JSON
//! (`app-info`) take [`BuildInfo::json`] and parse it with their own JSON type.
//!
//! The dirty bit and build time are as of the last time `build.rs` ran. It reruns
//! when one of these changes, and only these: `.git/HEAD`, the ref HEAD points to
//! (the ref's directory once `git pack-refs` has removed the loose file),
//! `packed-refs` and the index (each only while it exists), every tracked file that
//! was already modified when it last ran, `build.rs` itself, and the environment
//! variables `DOCXY_BUILD_KIND`, `SOURCE_DATE_EPOCH`, `GITHUB_HEAD_REF` and
//! `GITHUB_REF_NAME`. It does not rerun on every source edit, so a first edit to a
//! clean, unstaged tree is not noticed until `git add`, a commit, or
//! `touch buildinfo/build.rs`. The dirty bit counts tracked files only: untracked
//! scratch files do not turn a release build into a manual one. The repository
//! counts only if it tracks this crate (see `collect.rs`).

#[cfg(test)]
mod collect;
mod parse;

pub use parse::Kind;
use parse::{is_manual, kind_from_env};

use std::sync::OnceLock;

/// What `build.rs` recorded.
pub(crate) struct Raw {
    pub commit: &'static str,
    pub branch: &'static str,
    pub commit_date: &'static str,
    pub dirty: bool,
    pub last_pr: Option<u32>,
    pub last_pr_title: Option<&'static str>,
    pub issue: Option<u32>,
    pub ahead: Option<u32>,
    pub built_at: &'static str,
    pub profile: &'static str,
    pub target: &'static str,
    pub host: &'static str,
    pub kind: &'static str,
}

const RAW: Raw = include!(concat!(env!("OUT_DIR"), "/buildinfo.rs"));

/// A JSON-shaped value: what [`BuildInfo::json`] serializes. Private; hosts take
/// the JSON text.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    Bool(bool),
    Num(u64),
    Null,
}

#[derive(Clone, Debug)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub branch: String,
    pub commit_date: String,
    pub dirty: bool,
    pub last_pr: Option<u32>,
    pub last_pr_title: Option<String>,
    pub issue: Option<u32>,
    pub ahead: Option<u32>,
    pub built_at: String,
    pub profile: String,
    pub target: String,
    pub host: String,
    pub kind: Kind,
}

impl BuildInfo {
    /// The recorded build info for a host whose own version is `version`
    /// (`env!("CARGO_PKG_VERSION")` at the call site: the suite is 0.1.0, the
    /// terminal editors 0.5.0).
    pub fn new(version: &str) -> BuildInfo {
        BuildInfo::from_raw(&RAW, version)
    }

    fn from_raw(raw: &Raw, version: &str) -> BuildInfo {
        BuildInfo {
            version: version.to_string(),
            commit: raw.commit.to_string(),
            branch: raw.branch.to_string(),
            commit_date: raw.commit_date.to_string(),
            dirty: raw.dirty,
            last_pr: raw.last_pr,
            last_pr_title: raw.last_pr_title.map(str::to_string),
            issue: raw.issue,
            ahead: raw.ahead,
            built_at: raw.built_at.to_string(),
            profile: raw.profile.to_string(),
            target: raw.target.to_string(),
            host: raw.host.to_string(),
            kind: kind_from_env(Some(raw.kind)),
        }
    }

    /// Built by hand or from a dirty tree.
    pub fn manual(&self) -> bool {
        is_manual(self.kind, self.dirty)
    }

    /// The first 7 characters of the commit (or `unknown`).
    pub fn short_commit(&self) -> &str {
        self.commit.get(..7).unwrap_or(&self.commit)
    }

    fn pr_text(&self) -> String {
        match self.last_pr {
            Some(n) => format!("after #{n}"),
            None => "no merged PR".to_string(),
        }
    }

    /// `v0.5.0 · abc1234 · after #1015 · release`, with ` · manual build` appended
    /// when manual.
    pub fn short_line(&self) -> String {
        let mut s = format!(
            "v{} · {} · {} · {}",
            self.version,
            self.short_commit(),
            self.pr_text(),
            self.kind.as_str()
        );
        if self.manual() {
            s.push_str(" · manual build");
        }
        s
    }

    /// One line for the MCP `version` field: `0.5.0 (abc1234, after #1015, release)`.
    pub fn long_version(&self) -> String {
        let mut s = format!(
            "{} ({}, {}, {}",
            self.version,
            self.short_commit(),
            self.pr_text(),
            self.kind.as_str()
        );
        if self.manual() {
            s.push_str(", manual build");
        }
        s.push(')');
        s
    }

    /// Every field, in display order.
    fn fields(&self) -> Vec<(&'static str, Value)> {
        let s = |v: &str| Value::Str(v.to_string());
        let num = |v: Option<u32>| v.map_or(Value::Null, |n| Value::Num(u64::from(n)));
        vec![
            ("version", s(&self.version)),
            ("commit", s(&self.commit)),
            ("short_commit", s(self.short_commit())),
            ("commit_len", Value::Num(self.commit.len() as u64)),
            (
                "commit_hex",
                Value::Bool(
                    !self.commit.is_empty() && self.commit.bytes().all(|b| b.is_ascii_hexdigit()),
                ),
            ),
            ("branch", s(&self.branch)),
            ("commit_date", s(&self.commit_date)),
            ("dirty", Value::Bool(self.dirty)),
            ("last_pr", num(self.last_pr)),
            (
                "last_pr_title",
                self.last_pr_title.as_deref().map_or(Value::Null, s),
            ),
            ("issue", num(self.issue)),
            ("ahead", num(self.ahead)),
            ("built_at", s(&self.built_at)),
            ("profile", s(&self.profile)),
            ("target", s(&self.target)),
            ("host", s(&self.host)),
            ("kind", s(self.kind.as_str())),
            ("manual", Value::Bool(self.manual())),
            ("summary", s(&self.short_line())),
        ]
    }

    /// Every field of the build (version, commit, branch, last PR, kind, `manual`,
    /// `summary`, ...) as one JSON object (`{"version":"0.5.0",...}`), for
    /// hosts that parse it into their own JSON type.
    pub fn json(&self) -> String {
        let mut s = String::from("{");
        for (i, (k, v)) in self.fields().iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("\"{k}\":"));
            match v {
                Value::Str(t) => json_str(&mut s, t),
                Value::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
                Value::Num(n) => s.push_str(&n.to_string()),
                Value::Null => s.push_str("null"),
            }
        }
        s.push('}');
        s
    }

    /// The labelled rows of the build, in display order: what the About dialog
    /// lists and what [`BuildInfo::version_block`] prints after its title line.
    /// The "manual build" marker is not a row: see [`BuildInfo::manual`].
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let last_pr = match (self.last_pr, &self.last_pr_title) {
            (Some(n), Some(t)) if !t.is_empty() => format!("#{n} {t}"),
            (Some(n), _) => format!("#{n}"),
            (None, _) => "none".to_string(),
        };
        let mut rows = vec![
            ("commit", self.commit.clone()),
            ("branch", self.branch.clone()),
            ("commit date", self.commit_date.clone()),
            ("last PR", last_pr),
        ];
        if let Some(issue) = self.issue {
            rows.push((
                "issue",
                match self.ahead {
                    Some(a) => format!("#{issue} ({a} ahead of origin/main)"),
                    None => format!("#{issue}"),
                },
            ));
        }
        rows.push(("dirty", if self.dirty { "yes" } else { "no" }.to_string()));
        rows.push(("built", self.built_at.clone()));
        rows.push(("profile", self.profile.clone()));
        rows.push(("target", self.target.clone()));
        rows.push(("host", self.host.clone()));
        rows.push(("kind", self.kind.as_str().to_string()));
        rows
    }

    /// The multi-line block `--version`, the About dialog's Copy and the crash log print.
    /// `name` is the program (`docxy`, `suite`).
    pub fn version_block(&self, name: &str) -> String {
        let mut s = format!("{name} {}\n", self.version);
        for (label, value) in self.rows() {
            s.push_str(&format!("{:<12} {value}\n", format!("{label}:")));
        }
        if self.manual() {
            s.push_str("manual build\n");
        }
        s
    }
}

fn json_str(out: &mut String, t: &str) {
    out.push('"');
    for c in t.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The process-wide build info; the first caller's `version` wins (every host
/// passes its own `CARGO_PKG_VERSION`).
pub fn get(version: &'static str) -> &'static BuildInfo {
    static INFO: OnceLock<BuildInfo> = OnceLock::new();
    INFO.get_or_init(|| BuildInfo::new(version))
}

/// [`BuildInfo::long_version`] as a `&'static str` (for APIs that want one).
pub fn long_version(version: &'static str) -> &'static str {
    static LONG: OnceLock<String> = OnceLock::new();
    LONG.get_or_init(|| get(version).long_version())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: &'static str, dirty: bool) -> Raw {
        Raw {
            commit: "0123456789abcdef0123456789abcdef01234567",
            branch: "main",
            commit_date: "2026-10-05T10:00:00+00:00",
            dirty,
            last_pr: Some(1015),
            last_pr_title: Some("Batch Word repeat"),
            issue: None,
            ahead: None,
            built_at: "2026-10-05T10:15:17Z",
            profile: "release",
            target: "x86_64-unknown-linux-gnu",
            host: "box",
            kind,
        }
    }

    #[test]
    fn release_clean_is_not_manual() {
        let b = BuildInfo::from_raw(&raw("release", false), "0.5.0");
        assert!(!b.manual());
        assert_eq!(b.short_line(), "v0.5.0 · 0123456 · after #1015 · release");
        assert!(!b.version_block("docxy").contains("manual build"));
        assert_eq!(b.long_version(), "0.5.0 (0123456, after #1015, release)");
    }

    #[test]
    fn local_and_dirty_are_manual() {
        let local = BuildInfo::from_raw(&raw("local", false), "0.5.0");
        assert!(local.manual());
        assert!(local.short_line().ends_with(" · manual build"));
        assert!(local.version_block("docxy").contains("manual build"));
        let dirty = BuildInfo::from_raw(&raw("ci", true), "0.5.0");
        assert!(dirty.manual());
        assert!(dirty.long_version().contains("manual build"));
    }

    #[test]
    fn fields_carry_the_documented_keys() {
        let b = BuildInfo::from_raw(&raw("local", true), "0.1.0");
        let f = b.fields();
        let get = |k: &str| f.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        for k in [
            "version",
            "commit",
            "short_commit",
            "branch",
            "commit_date",
            "dirty",
            "last_pr",
            "issue",
            "ahead",
            "built_at",
            "profile",
            "target",
            "host",
            "kind",
            "manual",
            "summary",
            "commit_len",
            "commit_hex",
            "last_pr_title",
        ] {
            assert!(get(k).is_some(), "missing {k}");
        }
        assert_eq!(get("version"), Some(Value::Str("0.1.0".into())));
        assert_eq!(get("commit_len"), Some(Value::Num(40)));
        assert_eq!(get("commit_hex"), Some(Value::Bool(true)));
        assert_eq!(get("manual"), Some(Value::Bool(true)));
        assert_eq!(get("last_pr"), Some(Value::Num(1015)));
        assert_eq!(get("issue"), Some(Value::Null));
    }

    #[test]
    fn json_is_one_escaped_object() {
        let mut r = raw("local", false);
        r.last_pr_title = Some("say \"hi\"\\");
        let j = BuildInfo::from_raw(&r, "0.5.0").json();
        assert!(j.starts_with("{\"version\":\"0.5.0\","), "{j}");
        assert!(
            j.contains("\"last_pr_title\":\"say \\\"hi\\\"\\\\\""),
            "{j}"
        );
        assert!(j.contains("\"issue\":null"), "{j}");
        assert!(j.contains("\"manual\":true"), "{j}");
        assert!(j.ends_with('}'));
    }

    #[test]
    fn unknown_commit_is_not_hex() {
        let mut r = raw("local", false);
        r.commit = "unknown";
        r.last_pr = None;
        r.last_pr_title = None;
        let b = BuildInfo::from_raw(&r, "0.5.0");
        assert_eq!(b.short_commit(), "unknown");
        assert!(b.short_line().contains("no merged PR"));
        assert!(b.fields().contains(&("commit_hex", Value::Bool(false))));
    }

    #[test]
    fn the_stamped_build_is_consistent() {
        let b = BuildInfo::new("9.9.9");
        assert_eq!(b.version, "9.9.9");
        assert!(["release", "ci", "local"].contains(&b.kind.as_str()));
    }
}
