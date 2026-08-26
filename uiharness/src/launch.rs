//! Starting a sandboxed instance of the suite, and stopping it again.
//!
//! A script says `open fixtures/basic.xlsx`, not "attach to whatever is
//! running": a case that ran against an instance somebody had already been
//! clicking in would be testing that instance's history, not the app. So the
//! runner starts its own, in a config root that exists for this run only.
//!
//! ## The isolation is the app's to enforce, not this module's
//!
//! [`launch`] sets `DOCXY_CONFIG_DIR` to a throwaway directory and passes
//! `--harness`; `suite/docxy/src/harness.rs`'s `gate` is what refuses to start
//! if those two disagree, and it refuses in the process that would do the
//! damage. This side sets the variable and then trusts the refusal — a check
//! here as well would be a second opinion that can drift from the first.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The environment variable the suite reads its sandbox config root from.
pub const CONFIG_DIR_ENV: &str = "DOCXY_CONFIG_DIR";

/// The flag that turns the control server on.
pub const HARNESS_FLAG: &str = "--harness";

/// The binary's name on this platform.
pub fn exe_name() -> &'static str {
    if cfg!(windows) { "suite.exe" } else { "suite" }
}

/// Where a built `suite` might be, given the repository roots to look under.
///
/// Release first: the suite's dev profile leaves gpui at `opt-level = 1` and
/// everything else unoptimized, and a harness that waits on frames is slow
/// enough there to be worth avoiding when both exist. Pure, so the search order
/// is a unit test rather than something you discover by not having a build.
pub fn candidate_exes(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        for profile in ["release", "debug"] {
            out.push(
                root.join("suite")
                    .join("target")
                    .join(profile)
                    .join(exe_name()),
            );
            out.push(root.join("target").join(profile).join(exe_name()));
        }
    }
    out
}

/// The roots [`find_suite`] looks under: where this crate was built from (so a
/// `cargo run -p uiharness` finds the sibling workspace's build), and the
/// working directory.
fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo) = manifest.parent() {
        roots.push(repo.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        if !roots.contains(&cwd) {
            roots.push(cwd);
        }
    }
    roots
}

/// Find a built `suite`: what the caller named, else `UIHARNESS_SUITE`, else
/// the first of [`candidate_exes`] that is there.
pub fn find_suite(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("{}: not a file", p.display()))
        };
    }
    if let Some(v) = std::env::var_os("UIHARNESS_SUITE") {
        let p = PathBuf::from(v);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("UIHARNESS_SUITE={}: not a file", p.display()))
        };
    }
    let tried = candidate_exes(&default_roots());
    for p in &tried {
        if p.is_file() {
            return Ok(p.clone());
        }
    }
    Err(format!(
        "no built {} found; build it with `cargo build --release --manifest-path \
         suite/Cargo.toml`, or point --suite at one. Looked in:\n{}",
        exe_name(),
        tried
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// A running sandboxed instance. Dropping it kills the process: a case that
/// panicked half way through must not leave a window on the desktop.
pub struct Launched {
    child: Child,
    sandbox: PathBuf,
    exe: PathBuf,
    /// Set by [`Launched::detach`]: the drop guard stops killing it.
    detached: bool,
}

impl Launched {
    /// The throwaway config root this instance was given.
    pub fn sandbox(&self) -> &Path {
        &self.sandbox
    }

    /// Where it publishes its control socket.
    pub fn ctl_dir(&self) -> PathBuf {
        crate::driver::control_dir(&self.sandbox)
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Whether it has already exited, and with what — the answer to "why did
    /// the connect time out".
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(format!("{} exited: {status}", self.exe.display())),
            Ok(None) => None,
            Err(e) => Some(format!("{}: {e}", self.exe.display())),
        }
    }

    /// Leave it running: `--keep`, for working out by hand why a case failed
    /// against the window it failed on.
    pub fn detach(mut self) {
        self.detached = true;
    }

    /// Ask it to quit, and wait a little for it to go. Falls back to killing
    /// it: a hung instance must not hold the run open.
    pub fn shutdown(mut self, driver: Option<&crate::Driver>) {
        if let Some(d) = driver {
            let _ = d.call("quit", ctlcore::json::Json::Obj(Vec::new()));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        if !self.detached && matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Start `exe` with the harness on and its config root in `sandbox`.
///
/// The sandbox is created if it is not there. `open` is left to the script:
/// passing the file on the command line would work too, but then half a case's
/// setup would live in the runner and half in the script.
pub fn launch(exe: &Path, sandbox: &Path) -> Result<Launched, String> {
    std::fs::create_dir_all(sandbox).map_err(|e| format!("{}: {e}", sandbox.display()))?;
    let child = Command::new(exe)
        .arg(HARNESS_FLAG)
        .env(CONFIG_DIR_ENV, sandbox)
        // Inherited, so a refusal from the isolation gate is visible rather
        // than swallowed — it is written to stderr and is the one message a
        // caller most needs to see.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(Launched {
        child,
        sandbox: sandbox.to_path_buf(),
        exe: exe.to_path_buf(),
        detached: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_build_is_preferred_to_a_debug_one() {
        let root = PathBuf::from("R");
        let c = candidate_exes(std::slice::from_ref(&root));
        let release = c
            .iter()
            .position(|p| p.to_string_lossy().contains("release"));
        let debug = c.iter().position(|p| p.to_string_lossy().contains("debug"));
        assert!(release < debug, "{c:?}");
    }

    /// The suite is its own workspace, so its build lands under `suite/target`
    /// — looking only in the root workspace's `target` would never find it.
    #[test]
    fn the_sibling_workspaces_target_directory_is_searched() {
        let c = candidate_exes(&[PathBuf::from("R")]);
        assert!(
            c.contains(
                &PathBuf::from("R")
                    .join("suite")
                    .join("target")
                    .join("release")
                    .join(exe_name())
            ),
            "{c:?}"
        );
        assert!(
            c.contains(
                &PathBuf::from("R")
                    .join("target")
                    .join("debug")
                    .join(exe_name())
            ),
            "{c:?}"
        );
    }

    #[test]
    fn a_named_binary_that_is_not_there_is_reported_rather_than_searched_past() {
        let e = find_suite(Some(Path::new("no-such-suite.exe"))).unwrap_err();
        assert!(e.contains("no-such-suite.exe"), "{e}");
    }
}
