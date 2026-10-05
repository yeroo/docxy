//! Reads the build info from git. Every failure (no git binary, no repository, a
//! shallow clone without `origin/main`) gives `unknown` / `None`; nothing here
//! panics. `build.rs` includes this file, so the tests here cover the build script.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Newest merges to scan for the last merged PR.
const SCAN_LIMIT: usize = 500;

pub const UNKNOWN: &str = "unknown";

/// What git says about the tree. `commit`, `branch` and `commit_date` are
/// `unknown` when git cannot tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    pub commit: String,
    pub branch: String,
    pub commit_date: String,
    /// Tracked files differ from HEAD (untracked files do not count).
    pub dirty: bool,
    pub last_pr: Option<(u32, String)>,
    pub issue: Option<u32>,
    pub ahead: Option<u32>,
}

impl Fields {
    pub fn unknown() -> Fields {
        Fields {
            commit: UNKNOWN.into(),
            branch: UNKNOWN.into(),
            commit_date: UNKNOWN.into(),
            dirty: false,
            last_pr: None,
            issue: None,
            ahead: None,
        }
    }
}

/// Run `git <args>` in `dir`; stdout (trimmed of the final newline) on success.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        // An outer `git` invocation (a hook, a worktree script) must not redirect us.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim_end_matches(['\n', '\r']).to_string())
}

/// `env` reads an environment variable (a parameter so tests need not touch the process env).
pub fn collect(dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> Fields {
    let mut f = Fields::unknown();
    let Some(head) = git(dir, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty()) else {
        return f;
    };
    f.commit = head;

    f.branch = match git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).as_deref() {
        Some(b) if !b.is_empty() && b != "HEAD" => b.to_string(),
        // Detached HEAD (a CI checkout): the workflow knows the branch.
        _ => env("GITHUB_HEAD_REF")
            .or_else(|| env("GITHUB_REF_NAME"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| UNKNOWN.into()),
    };

    if let Some(d) = git(dir, &["log", "-1", "--format=%cI"]).filter(|s| !s.is_empty()) {
        f.commit_date = d;
    }

    if let Some(s) = git(dir, &["status", "--porcelain", "--untracked-files=no"]) {
        f.dirty = !s.trim().is_empty();
    }

    let max = format!("--max-count={SCAN_LIMIT}");
    if let Some(log) = git(
        dir,
        &["log", "--first-parent", &max, "--format=%s%x1f%b%x1e"],
    ) {
        f.last_pr = log.split('\x1e').find_map(|rec| {
            let (subject, body) = rec.trim_start_matches(['\n', '\r']).split_once('\x1f')?;
            crate::parse::last_merged_pr(subject, body)
        });
    }

    f.issue = crate::parse::issue_from_branch(&f.branch);
    if f.issue.is_some() {
        f.ahead = git(dir, &["rev-list", "--count", "origin/main..HEAD"])
            .and_then(|s| s.trim().parse().ok());
    }
    f
}

/// Files whose change must rerun the build script: HEAD, the ref it points to,
/// the index, packed refs, and every tracked file that is modified right now.
/// Empty without a repository. Paths resolve against `dir` (cargo runs the build
/// script in the package directory).
pub fn watch_paths(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if git(dir, &["rev-parse", "HEAD"]).is_none() {
        return out;
    }
    let git_path = |name: &str| git(dir, &["rev-parse", "--git-path", name]).map(PathBuf::from);
    out.extend(git_path("HEAD"));
    if let Some(r) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        out.extend(git_path(&r));
    }
    out.extend(git_path("packed-refs"));
    out.extend(git_path("index"));
    if let Some(top) = git(dir, &["rev-parse", "--show-toplevel"]) {
        if let Some(list) = git(dir, &["diff", "--name-only", "--no-renames", "HEAD"]) {
            out.extend(
                list.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| Path::new(&top).join(l)),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("buildinfo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn without_git_everything_is_unknown() {
        let d = temp_dir("nogit");
        // Stop git walking up from the temp dir into some enclosing repository.
        // SAFETY: only this test sets the variable, before any thread it spawns reads it.
        unsafe { std::env::set_var("GIT_CEILING_DIRECTORIES", &d) };
        let f = collect(&d, &no_env);
        assert_eq!(f, Fields::unknown());
        assert_eq!(f.commit, "unknown");
        assert!(watch_paths(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_directory_is_unknown_too() {
        let d = std::env::temp_dir().join("buildinfo-does-not-exist-xyz");
        assert_eq!(collect(&d, &no_env), Fields::unknown());
    }

    #[test]
    fn this_repository_reports_a_full_sha() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let f = collect(here, &no_env);
        // A source tarball has no .git; then there is nothing to check.
        if f.commit != UNKNOWN {
            assert_eq!(f.commit.len(), 40, "{}", f.commit);
            assert!(f.commit.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_ne!(f.branch, "");
        }
    }
}
