//! Reads the build info from git. Every failure (no git binary, no repository, a
//! shallow clone without `origin/main`) gives `unknown` / `None`; nothing here
//! panics. `build.rs` includes this file, so the tests here cover the build script.
//!
//! The repository counts only when it tracks this crate (`build.rs` is a tracked
//! file in `dir`): git walks up from wherever it runs, so a crate unpacked under
//! `~/.cargo/registry` inside some enclosing repository (a dotfiles repo in `$HOME`)
//! must not be stamped with that repository's commit.
//!
//! A shallow checkout sees only the commits it has: the last merged PR is found
//! only if a merge commit is among them. `release.yml` fetches the whole history.
//! A CI build of a pull request checks out a synthetic `Merge <sha> into <sha>`
//! commit with depth 1, so it reports no PR and no commits ahead of `origin/main`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// First-parent commits to scan for the last merged PR.
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

/// An environment lookup (a parameter so tests need not touch the process env).
pub type Env<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Run `git <args>` in `dir`; stdout (trimmed of the final newline) on success.
fn git(dir: &Path, env: &Env, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    // Where git must stop looking for a repository (the tests' git-less directory).
    if let Some(ceiling) = env("GIT_CEILING_DIRECTORIES") {
        cmd.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    let out = cmd
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

/// HEAD's commit when `dir` is inside a repository that tracks this crate.
fn head_of_our_repository(dir: &Path, env: &Env) -> Option<String> {
    let head = git(dir, env, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty())?;
    git(dir, env, &["ls-files", "--error-unmatch", "build.rs"])?;
    Some(head)
}

/// `env` reads an environment variable (a parameter so tests need not touch the process env).
pub fn collect(dir: &Path, env: &Env) -> Fields {
    let mut f = Fields::unknown();
    let Some(head) = head_of_our_repository(dir, env) else {
        return f;
    };
    f.commit = head;

    f.branch = match git(dir, env, &["rev-parse", "--abbrev-ref", "HEAD"]).as_deref() {
        Some(b) if !b.is_empty() && b != "HEAD" => b.to_string(),
        // Detached HEAD (a CI checkout): the workflow knows the branch.
        _ => env("GITHUB_HEAD_REF")
            .or_else(|| env("GITHUB_REF_NAME"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| UNKNOWN.into()),
    };

    if let Some(d) = git(dir, env, &["log", "-1", "--format=%cI"]).filter(|s| !s.is_empty()) {
        f.commit_date = d;
    }

    if let Some(s) = git(dir, env, &["status", "--porcelain", "--untracked-files=no"]) {
        f.dirty = !s.trim().is_empty();
    }

    let max = format!("--max-count={SCAN_LIMIT}");
    if let Some(log) = git(
        dir,
        env,
        &["log", "--first-parent", &max, "--format=%s%x1f%b%x1e"],
    ) {
        let recs: Vec<(u32, String, bool)> = log
            .split('\x1e')
            .filter_map(|rec| {
                let (subject, body) = rec.trim_start_matches(['\n', '\r']).split_once('\x1f')?;
                let (n, t) = crate::parse::last_merged_pr(subject, body)?;
                Some((n, t, subject.trim().starts_with("Merge pull request #")))
            })
            .collect();
        // Merge commits are the project's way of landing a PR; a `Title (#N)` subject
        // on a branch usually names an *issue*. So squash subjects only count when
        // no merge commit is in range, and never when N is the branch's own issue.
        let issue = crate::parse::issue_from_branch(&f.branch);
        f.last_pr = recs
            .iter()
            .find(|r| r.2)
            .or_else(|| recs.iter().find(|r| Some(r.0) != issue))
            .map(|r| (r.0, r.1.clone()));
    }

    f.issue = crate::parse::issue_from_branch(&f.branch);
    if f.issue.is_some() {
        f.ahead = git(dir, env, &["rev-list", "--count", "origin/main..HEAD"])
            .and_then(|s| s.trim().parse().ok());
    }
    f
}

/// Files whose change must rerun the build script: HEAD, the ref it points to
/// (its directory when the ref is packed and the loose file is gone), packed refs
/// and the index when they exist, and every tracked file that is modified right
/// now. Only paths that exist: cargo reruns a build script on every build for a
/// missing one. Empty outside a repository that tracks this crate. Paths resolve
/// against `dir` (cargo runs the build script in the package directory).
pub fn watch_paths(dir: &Path, env: &Env) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if head_of_our_repository(dir, env).is_none() {
        return out;
    }
    let git_path = |name: &str| {
        git(dir, env, &["rev-parse", "--git-path", name])
            .map(PathBuf::from)
            .filter(|p| dir.join(p).exists())
    };
    out.extend(git_path("HEAD"));
    if let Some(r) = git(dir, env, &["symbolic-ref", "-q", "HEAD"]) {
        match git_path(&r) {
            Some(p) => out.push(p),
            // `git pack-refs` / gc removed the loose file: its directory sees it come back.
            None => {
                let loose = git(dir, env, &["rev-parse", "--git-path", &r]).map(PathBuf::from);
                out.extend(
                    loose
                        .as_deref()
                        .and_then(Path::parent)
                        .filter(|p| dir.join(p).is_dir())
                        .map(Path::to_path_buf),
                );
            }
        }
    }
    out.extend(git_path("packed-refs"));
    out.extend(git_path("index"));
    if let Some(top) = git(dir, env, &["rev-parse", "--show-toplevel"]) {
        if let Some(list) = git(dir, env, &["diff", "--name-only", "--no-renames", "HEAD"]) {
            out.extend(
                list.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| Path::new(&top).join(l))
                    .filter(|p| p.exists()),
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
        let outer = temp_dir("nogit");
        // git ignores a ceiling equal to the working directory, so the directory
        // under test is a child of the ceiling: git may not look above it, and
        // cannot find a repository enclosing the temp dir.
        let d = outer.join("work");
        std::fs::create_dir_all(&d).unwrap();
        let ceiling = outer.display().to_string();
        let env = move |k: &str| (k == "GIT_CEILING_DIRECTORIES").then(|| ceiling.clone());
        let f = collect(&d, &env);
        assert_eq!(f, Fields::unknown());
        assert_eq!(f.commit, "unknown");
        assert!(watch_paths(&d, &env).is_empty());
        let _ = std::fs::remove_dir_all(&outer);
    }

    /// `git -C dir <args>` with a fixed identity; false when git is not installed.
    fn run(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn reads_a_scratch_repository() {
        let d = temp_dir("repo");
        if !run(&d, &["init", "-q", "-b", "main"]) {
            return; // no git on this machine
        }
        std::fs::write(d.join("a.txt"), "1").unwrap();
        std::fs::write(d.join("build.rs"), "fn main() {}").unwrap();
        assert!(run(&d, &["add", "."]));
        // With no merge commit in range, a `Title (#N)` subject counts as a squashed PR ...
        assert!(run(&d, &["commit", "-qm", "Fix the thing (#42)"]));
        let f = collect(&d, &no_env);
        assert_eq!(
            f.last_pr,
            Some((42, "Fix the thing".into())),
            "squash fallback"
        );
        // ... once a real merge commit is in range, that wins.
        assert!(run(
            &d,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "Merge pull request #7 from o/issue-5-x",
                "-m",
                "Land the thing"
            ],
        ));
        assert!(run(
            &d,
            &["commit", "-q", "--allow-empty", "-m", "Later work (#99)"]
        ));
        let f = collect(&d, &no_env);
        assert_eq!(f.last_pr, Some((7, "Land the thing".into())));
        assert_eq!(f.branch, "main");
        assert_eq!(f.commit.len(), 40);
        assert!(!f.dirty);
        // Untracked files do not make the tree dirty; a tracked edit does.
        std::fs::write(d.join("scratch.txt"), "x").unwrap();
        assert!(!collect(&d, &no_env).dirty);
        std::fs::write(d.join("a.txt"), "2").unwrap();
        assert!(collect(&d, &no_env).dirty);
        assert!(
            watch_paths(&d, &no_env)
                .iter()
                .any(|p| p.ends_with("a.txt"))
        );
        // The issue number comes from the branch name.
        assert!(run(&d, &["checkout", "-q", "-b", "issue-99-later"]));
        assert_eq!(collect(&d, &no_env).issue, Some(99));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_squash_subject_naming_the_branchs_own_issue_is_not_a_pr() {
        let outer = temp_dir("own-issue");
        let d = outer.join("work");
        std::fs::create_dir_all(&d).unwrap();
        if !run(&d, &["init", "-q", "-b", "issue-42-x"]) {
            return; // no git on this machine
        }
        std::fs::write(d.join("a.txt"), "1").unwrap();
        std::fs::write(d.join("build.rs"), "fn main() {}").unwrap();
        assert!(run(&d, &["add", "."]));
        assert!(run(&d, &["commit", "-qm", "Older squash (#7)"]));
        assert!(run(
            &d,
            &["commit", "-q", "--allow-empty", "-m", "Fix (#42)"]
        ));
        let ceiling = outer.display().to_string();
        let env = move |k: &str| (k == "GIT_CEILING_DIRECTORIES").then(|| ceiling.clone());
        // Newest first: "Fix (#42)" is the branch's own issue, so the older squash wins.
        let f = collect(&d, &env);
        assert_eq!(f.issue, Some(42));
        assert_eq!(f.last_pr, Some((7, "Older squash".into())));
        // With nothing else in range there is no PR at all.
        assert!(run(&d, &["checkout", "-q", "--orphan", "issue-42-y"]));
        assert!(run(
            &d,
            &["commit", "-q", "--allow-empty", "-m", "Fix (#42)"]
        ));
        let f = collect(&d, &env);
        assert_eq!(f.issue, Some(42));
        assert_eq!(f.last_pr, None);
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn an_enclosing_repository_that_does_not_track_the_crate_is_not_ours() {
        // A crate unpacked inside someone else's repository (`~/.cargo/registry`
        // under a dotfiles repo): git finds HEAD, but build.rs is not tracked.
        let repo = temp_dir("enclosing");
        if !run(&repo, &["init", "-q", "-b", "main"]) {
            return; // no git on this machine
        }
        std::fs::write(repo.join("notes.txt"), "x").unwrap();
        assert!(run(&repo, &["add", "."]));
        assert!(run(&repo, &["commit", "-qm", "Dotfiles (#3)"]));
        let krate = repo.join("registry").join("buildinfo-0.5.0");
        std::fs::create_dir_all(&krate).unwrap();
        std::fs::write(krate.join("build.rs"), "fn main() {}").unwrap(); // untracked
        assert_eq!(collect(&krate, &no_env), Fields::unknown());
        assert!(watch_paths(&krate, &no_env).is_empty());
        // Once the repository tracks it, it is ours.
        assert!(run(
            &repo,
            &["add", "-f", "registry/buildinfo-0.5.0/build.rs"]
        ));
        assert_ne!(collect(&krate, &no_env).commit, UNKNOWN);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn watch_paths_names_only_paths_that_exist() {
        let d = temp_dir("watch");
        if !run(&d, &["init", "-q", "-b", "main"]) {
            return; // no git on this machine
        }
        std::fs::write(d.join("build.rs"), "fn main() {}").unwrap();
        assert!(run(&d, &["add", "."]));
        assert!(run(&d, &["commit", "-qm", "one"]));
        let all_exist = |watched: &[PathBuf]| {
            assert!(!watched.is_empty());
            for p in watched {
                assert!(d.join(p).exists(), "{p:?} does not exist");
            }
        };
        let loose = watch_paths(&d, &no_env);
        all_exist(&loose);
        assert!(
            loose.iter().any(|p| p.ends_with("refs/heads/main")),
            "{loose:?}"
        );
        // After `git pack-refs` the loose ref is gone: watch its directory, and
        // packed-refs, instead of a path that does not exist.
        assert!(run(&d, &["pack-refs", "--all", "--prune"]));
        let packed = watch_paths(&d, &no_env);
        all_exist(&packed);
        assert!(
            !packed.iter().any(|p| p.ends_with("refs/heads/main")),
            "{packed:?}"
        );
        assert!(
            packed.iter().any(|p| p.ends_with("packed-refs")),
            "{packed:?}"
        );
        assert!(
            packed.iter().any(|p| p.ends_with("refs/heads")),
            "{packed:?}"
        );
        // A tracked file modified now is watched.
        std::fs::write(d.join("build.rs"), "fn main() { }").unwrap();
        assert!(
            watch_paths(&d, &no_env)
                .iter()
                .any(|p| p.ends_with("build.rs"))
        );
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
