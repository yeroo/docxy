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

use std::ffi::OsString;
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
/// panicked half way through must not leave a window on the desktop — and
/// for a desktop launch, must not leave one running on a desktop nobody can
/// see to close it.
pub struct Launched {
    proc_: Proc,
    sandbox: PathBuf,
    exe: PathBuf,
    /// Where the desktop launch sent the child's stdout and stderr, so
    /// [`Launched::exited`] can quote the tail — a refusal from the
    /// isolation gate is written there and is the one message a caller most
    /// needs to see. `None` on the plain path, which inherits stdio.
    output_log: Option<PathBuf>,
    /// Set by [`Launched::detach`]: the drop guard stops killing it.
    detached: bool,
}

/// The launched process, as either launcher leaves it: a std [`Child`], or
/// the raw handle `CreateProcessW` reports — the only API that can name a
/// desktop for the child. The child's thread handle is closed right after
/// the spawn, so only what these methods need is kept.
enum Proc {
    Std(Child),
    #[cfg(windows)]
    Raw {
        process: windows::Win32::Foundation::HANDLE,
        pid: u32,
    },
}

impl Proc {
    fn id(&self) -> u32 {
        match self {
            Proc::Std(child) => child.id(),
            #[cfg(windows)]
            Proc::Raw { pid, .. } => *pid,
        }
    }

    /// Whether it has gone, and a word about how. The string is what
    /// [`Launched::exited`] puts after the exe path.
    fn try_wait(&mut self) -> std::io::Result<Option<String>> {
        match self {
            Proc::Std(child) => Ok(child.try_wait()?.map(|s| s.to_string())),
            #[cfg(windows)]
            Proc::Raw { process, .. } => {
                use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
                use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

                // SAFETY: `process` is a live handle this variant owns until
                // `Drop`.
                let waited = unsafe { WaitForSingleObject(*process, 0) };
                if waited == WAIT_TIMEOUT {
                    return Ok(None);
                }
                if waited != WAIT_OBJECT_0 {
                    return Err(std::io::Error::other(format!(
                        "WaitForSingleObject on the suite process: {waited:?}"
                    )));
                }
                // SAFETY: the wait says the process has gone; `code` is
                // written on success.
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(*process, &mut code) }
                    .map_err(|e| std::io::Error::other(format!("GetExitCodeProcess: {e}")))?;
                Ok(Some(format!("exit code: {code}")))
            }
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Proc::Std(child) => child.kill(),
            #[cfg(windows)]
            Proc::Raw { process, .. } => {
                use windows::Win32::System::Threading::TerminateProcess;

                // SAFETY: `process` is a live handle this variant owns until
                // `Drop`; any exit code does, `1` says the kill did it.
                unsafe { TerminateProcess(*process, 1) }
                    .map_err(|e| std::io::Error::other(format!("TerminateProcess: {e}")))
            }
        }
    }

    /// Wait until it has gone, for the kill-on-drop and shutdown paths.
    fn wait(&mut self) -> std::io::Result<()> {
        match self {
            Proc::Std(child) => child.wait().map(|_| ()),
            #[cfg(windows)]
            Proc::Raw { process, .. } => {
                use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};

                // SAFETY: `process` is a live handle this variant owns until
                // `Drop`; INFINITE waits, so the return says it has gone
                // (barring a kill that itself failed, which `wait` reported).
                let _ = unsafe { WaitForSingleObject(*process, INFINITE) };
                Ok(())
            }
        }
    }
}

#[cfg(windows)]
impl Drop for Proc {
    fn drop(&mut self) {
        if let Proc::Raw { process, .. } = self {
            use windows::Win32::Foundation::CloseHandle;

            // SAFETY: the handle is ours and `Drop` runs exactly once.
            unsafe {
                let _ = CloseHandle(*process);
            }
        }
    }
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
        self.proc_.id()
    }

    /// Whether it has already exited, and with what — the answer to "why did
    /// the connect time out". With a desktop launch, the output log's tail
    /// follows, so the cause stays visible although stdio no longer points
    /// at the caller's terminal.
    pub fn exited(&mut self) -> Option<String> {
        let mut msg = match self.proc_.try_wait() {
            Ok(Some(status)) => Some(format!("{} exited: {status}", self.exe.display())),
            Ok(None) => None,
            Err(e) => Some(format!("{}: {e}", self.exe.display())),
        };
        if let (Some(msg), Some(log)) = (msg.as_mut(), self.output_log.as_ref()) {
            if let Ok(text) = std::fs::read_to_string(log) {
                if !text.is_empty() {
                    let tail = text
                        .lines()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");
                    msg.push_str(&format!(
                        "\n--- {} (last 20 lines) ---\n{tail}",
                        log.display()
                    ));
                }
            }
        }
        msg
    }

    /// Take charge of a suite started some other way — a test that must spawn
    /// it with an environment [`launch`] would never pass — so it gets the same
    /// drop guard and [`Launched::shutdown`].
    pub fn adopt(child: Child, exe: &Path, sandbox: &Path) -> Launched {
        Launched {
            proc_: Proc::Std(child),
            sandbox: sandbox.to_path_buf(),
            exe: exe.to_path_buf(),
            output_log: None,
            detached: false,
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
            if matches!(self.proc_.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.proc_.kill();
        let _ = self.proc_.wait();
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        if !self.detached && matches!(self.proc_.try_wait(), Ok(None)) {
            let _ = self.proc_.kill();
            let _ = self.proc_.wait();
        }
    }
}

/// Start `exe` with the harness on and its config root in `sandbox`.
///
/// The sandbox is created if it is not there. `open` is left to the script:
/// passing the file on the command line would work too, but then half a case's
/// setup would live in the runner and half in the script.
pub fn launch(exe: &Path, sandbox: &Path) -> Result<Launched, String> {
    launch_with_env(exe, sandbox, std::env::vars_os())
}

/// [`launch`] with the environment the child is built from spelled out, for a
/// test that starts the suite as if from an agwinterm pane.
pub fn launch_with_env(
    exe: &Path,
    sandbox: &Path,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Launched, String> {
    std::fs::create_dir_all(sandbox).map_err(|e| format!("{}: {e}", sandbox.display()))?;
    // ⚠️ Absolute before it is handed over. The app resolves a relative
    // `DOCXY_CONFIG_DIR` against its own working directory, which is the
    // sandbox itself — so a relative `--run target/…` would send the child
    // looking for its config root *inside* the config root, and the control
    // socket would be published somewhere this side never looks. `absolute`
    // rather than `canonicalize`, which returns a `\\?\` verbatim path.
    let sandbox =
        std::path::absolute(sandbox).map_err(|e| format!("{}: {e}", sandbox.display()))?;
    let child = command(exe, &sandbox, parent)
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(Launched {
        proc_: Proc::Std(child),
        sandbox,
        exe: exe.to_path_buf(),
        output_log: None,
        detached: false,
    })
}

/// [`launch`]'s desktop mode (#722): start the suite on `desktop`, a Win32
/// desktop that is never switched in, so its window cannot take the user's
/// keyboard. A child inherits the *process's* desktop, not the calling
/// thread's, so this goes through `CreateProcessW` with
/// `STARTUPINFOW.lpDesktop` — `std::process::Command` cannot name one. The
/// instance's stdout and stderr go to `<sandbox>/suite-output.log`, so a
/// refusal from the isolation gate is filed rather than lost; the caller's
/// thread must attach to the desktop ([`crate::desktop::Desktop`]) for
/// captures to find the window.
#[cfg(windows)]
pub fn launch_on_desktop(
    exe: &Path,
    sandbox: &Path,
    desktop: &crate::desktop::Desktop,
) -> Result<Launched, String> {
    launch_on_desktop_with_env(exe, sandbox, desktop, std::env::vars_os())
}

/// [`launch_on_desktop`] with the environment the child is built from
/// spelled out, for a test that starts the suite as if from an agwinterm
/// pane. Off Windows a separate desktop is an error, not a silent fallback.
#[cfg(windows)]
pub fn launch_on_desktop_with_env(
    exe: &Path,
    sandbox: &Path,
    desktop: &crate::desktop::Desktop,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Launched, String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOW,
    };
    use windows::core::{PCWSTR, PWSTR};

    std::fs::create_dir_all(sandbox).map_err(|e| format!("{}: {e}", sandbox.display()))?;
    // ⚠️ Absolute before it is handed over, for the reason `launch_with_env`
    // gives — and because the environment block below carries it verbatim.
    let sandbox =
        std::path::absolute(sandbox).map_err(|e| format!("{}: {e}", sandbox.display()))?;
    let log_path = sandbox.join("suite-output.log");
    let log =
        std::fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    // The handle must survive the hand-over: `bInheritHandles = true` plus
    // STARTF_USESTDHANDLES is what retargets the child's stdio at the log.
    let log_handle = HANDLE(log.as_raw_handle());
    // SAFETY: the handle belongs to `log`, which outlives the call.
    unsafe { SetHandleInformation(log_handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
        .map_err(|e| format!("{}: {e}", log_path.display()))?;

    let wide =
        |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let exe_wide = wide(exe.as_os_str());
    let mut cmdline = wide(std::ffi::OsStr::new(&command_line(exe)));
    let cwd = wide(sandbox.as_os_str());
    let desktop_name = wide(std::ffi::OsStr::new(desktop.name()));
    let env_block = environment_block(&child_env(&sandbox, parent));

    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop_name.as_ptr() as *mut u16),
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: HANDLE::default(),
        hStdOutput: log_handle,
        hStdError: log_handle,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: every pointer is a live NUL-terminated buffer above; the
    // environment block is sorted and double-NUL-terminated as
    // CREATE_UNICODE_ENVIRONMENT requires; `bInheritHandles = true` exposes
    // the inheritable log handle (and only such handles) to the child, as
    // std's own spawn does for inherited stdio; `si` and `pi` are sized as
    // the API requires.
    unsafe {
        CreateProcessW(
            PCWSTR(exe_wide.as_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(env_block.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &si,
            &mut pi,
        )
    }
    .map_err(|e| format!("{}: {e}", exe.display()))?;
    // The child's thread is of no use; the process handle lives in `Proc`.
    // SAFETY: `pi.hThread` is a handle we own from the successful spawn.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
    }
    // The child holds its own inherited copy; this side is done with it.
    drop(log);
    Ok(Launched {
        proc_: Proc::Raw {
            process: pi.hProcess,
            pid: pi.dwProcessId,
        },
        sandbox,
        exe: exe.to_path_buf(),
        output_log: Some(log_path),
        detached: false,
    })
}

/// Off Windows, [`launch_on_desktop`] is the same error [`Desktop::create`]
/// gives: a separate desktop is Windows-only.
///
/// [`Desktop::create`]: crate::desktop::Desktop::create
#[cfg(not(windows))]
pub fn launch_on_desktop(
    exe: &Path,
    sandbox: &Path,
    desktop: &crate::desktop::Desktop,
) -> Result<Launched, String> {
    launch_on_desktop_with_env(exe, sandbox, desktop, std::env::vars_os())
}

/// Off Windows counterpart of the cfg'd twin above.
#[cfg(not(windows))]
pub fn launch_on_desktop_with_env(
    exe: &Path,
    sandbox: &Path,
    desktop: &crate::desktop::Desktop,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Launched, String> {
    let _ = (exe, sandbox, desktop, parent.into_iter());
    Err("--desktop: a separate desktop is Windows-only".to_string())
}

/// Whether `name` is one of the variables agwinterm sets in its panes
/// (`AGWINTERM_SESSION_ID`, `AGWINTERM_PIPE`, …, and bare `AGWINTERM`). A
/// prefix rather than a list: agwinterm keeps adding them. ASCII
/// case-insensitive, as Windows environment names are.
pub fn is_terminal_integration_var(name: &std::ffi::OsStr) -> bool {
    const PREFIX: &[u8] = b"AGWINTERM";
    let name = name.as_encoded_bytes();
    name.len() >= PREFIX.len() && name[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
}

/// The environment the desktop launch hands the child, as a list
/// [`environment_block`] can serialize: `parent` without the agwinterm
/// variables ([`is_terminal_integration_var`]) and without any existing
/// `DOCXY_CONFIG_DIR` — Windows names are case-insensitive, so the override
/// must replace whatever case the parent used, never add a second entry —
/// plus one [`CONFIG_DIR_ENV`] pointing at `sandbox`. Hidden `=C:`-style
/// entries pass through byte-faithfully, as the plain launch path keeps
/// them.
pub fn child_env(
    sandbox: &Path,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = parent
        .into_iter()
        .filter(|(name, _)| !is_terminal_integration_var(name))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(CONFIG_DIR_ENV))
        .collect();
    env.push((
        OsString::from(CONFIG_DIR_ENV),
        sandbox.as_os_str().to_os_string(),
    ));
    env
}

/// The ASCII case fold Windows sorts environment names by, applied to one
/// UTF-16 unit: only `a-z` fold; every other unit compares as-is.
#[cfg(windows)]
fn upper(unit: &u16) -> u16 {
    match unit {
        0x61..=0x7a => unit - 0x20, // 'a'..='z'
        _ => *unit,
    }
}

/// `env` as a `CreateProcessW` environment block: entries `name=value\0` in
/// the order Windows keeps them — sorted with the name ASCII-case-
/// insensitive on the UTF-16 units (its rule, and what std's own block
/// does), names that differ only in case ordered by their exact units — then
/// a double NUL. An empty env is two zero words.
#[cfg(windows)]
pub fn environment_block(env: &[(OsString, OsString)]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    let mut entries: Vec<(Vec<u16>, Vec<u16>)> = env
        .iter()
        .map(|(name, value)| {
            (
                name.as_os_str().encode_wide().collect(),
                value.as_os_str().encode_wide().collect(),
            )
        })
        .collect();
    entries.sort_by(|(a, _), (b, _)| {
        a.iter()
            .map(upper)
            .cmp(b.iter().map(upper))
            .then_with(|| a.cmp(b))
    });
    let mut block = Vec::new();
    for (name, value) in &entries {
        block.extend_from_slice(name);
        block.push(u16::from(b'='));
        block.extend_from_slice(value);
        block.push(0);
    }
    // One NUL beyond the last entry's own; with no entries at all the block
    // is two zero words, which is what an empty one is.
    block.push(0);
    if entries.is_empty() {
        block.push(0);
    }
    block
}

/// The `lpCommandLine` the desktop launch hands `CreateProcessW`, as one
/// writable string: the quoted executable plus the harness flag — the
/// oracle launcher's exact shape.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" {HARNESS_FLAG}", exe.display())
}

/// The command [`launch`] spawns. The child's environment is built from
/// `parent` (what [`launch`] passes is its own), not inherited, so what the
/// child gets is exactly what this function lets through, and the unit test
/// below can check it without depending on the terminal the tests ran from.
///
/// Everything is passed on except the agwinterm variables (#697): a harness
/// instance started from an agwinterm pane is not *in* that pane, and must not
/// name itself after it or talk to its control pipe. The suite still needs
/// `SystemRoot`, `PATH` and `TEMP`, so nothing else is dropped.
pub fn command(
    exe: &Path,
    sandbox: &Path,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Command {
    let mut cmd = Command::new(exe);
    cmd.env_clear().envs(
        parent
            .into_iter()
            .filter(|(name, _)| !is_terminal_integration_var(name)),
    );
    cmd.arg(HARNESS_FLAG)
        .env(CONFIG_DIR_ENV, sandbox)
        // The sandbox is also the child's working directory, as a defensive
        // default: no save path falls back to `current_dir()` any more (a
        // never-saved document or workbook refuses in a harness), but anything
        // that ever writes relative to the cwd lands here rather than in the
        // repository the harness was run from.
        .current_dir(sandbox)
        // Inherited, so a refusal from the isolation gate is visible rather
        // than swallowed — it is written to stderr and is the one message a
        // caller most needs to see.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd
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

    /// #697: the child gets `parent` without its agwinterm variables, in any
    /// case, and without dropping anything else.
    #[test]
    fn the_agwinterm_variables_are_removed_from_the_child_and_nothing_else() {
        let parent = [
            "AGWINTERM_PIPE",
            "agwinterm_session_id",
            "AGWINTERM",
            "AGWINTERMX",
            "PATH",
            "XAGWINTERM_Y",
            "AGWINTER",
        ]
        .map(|k| (OsString::from(k), OsString::from("v")));
        let cmd = command(Path::new("suite.exe"), Path::new("sandbox"), parent);
        let mut env: Vec<String> = cmd
            .get_envs()
            .map(|(k, v)| {
                assert!(v.is_some(), "{k:?} is removed rather than never passed");
                k.to_string_lossy().to_ascii_uppercase()
            })
            .collect();
        env.sort();
        assert_eq!(env, ["AGWINTER", CONFIG_DIR_ENV, "PATH", "XAGWINTERM_Y"]);
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), [HARNESS_FLAG]);
    }

    #[test]
    fn a_named_binary_that_is_not_there_is_reported_rather_than_searched_past() {
        let e = find_suite(Some(Path::new("no-such-suite.exe"))).unwrap_err();
        assert!(e.contains("no-such-suite.exe"), "{e}");
    }

    /// The desktop launch builds its environment block itself, so the rule
    /// `command()` gets from std — parent minus the agwinterm variables, one
    /// `DOCXY_CONFIG_DIR` whatever case the parent used it in — is pinned
    /// here as a pure function. A stale `docxy_config_dir` reaching the suite
    /// would point the isolation gate's check at the wrong directory.
    #[test]
    fn the_desktop_child_env_strips_agwinterm_and_sets_one_config_dir() {
        let parent = [
            ("AGWINTERM_PIPE", "x"),
            ("docxy_config_dir", "stale"),
            ("=C:", r"C:\w"),
            ("PATH", "p"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let env = child_env(Path::new("S"), parent);
        let mut names: Vec<String> = env
            .iter()
            .map(|(k, _)| k.to_string_lossy().to_ascii_uppercase())
            .collect();
        names.sort();
        assert_eq!(names, ["=C:", "DOCXY_CONFIG_DIR", "PATH"]);
        let config_dirs = env
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(CONFIG_DIR_ENV))
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();
        assert_eq!(config_dirs, [OsString::from("S")]);
    }

    /// Windows keeps an environment block sorted by name; std sorts its own
    /// block the same way. Ours must agree, and be NUL-separated, value-less
    /// entries carried byte-faithfully (the hidden `=C:` ones), and
    /// double-NUL-terminated.
    #[cfg(windows)]
    #[test]
    fn the_environment_block_is_sorted_nul_separated_and_double_terminated() {
        use std::os::windows::ffi::OsStrExt;
        let block = environment_block(&[
            (OsString::from("b"), OsString::from("2")),
            (OsString::from("A"), OsString::from("1")),
        ]);
        let want: Vec<u16> = std::ffi::OsStr::new("A=1\0b=2\0\0").encode_wide().collect();
        assert_eq!(block, want);
        assert_eq!(environment_block(&[]), [0, 0]);
    }

    /// `CreateProcessW` takes the command line as one writable string, the
    /// quoted exe and the flag together — the oracle launcher's exact shape.
    #[test]
    fn the_command_line_quotes_the_exe_and_passes_harness() {
        assert_eq!(
            command_line(Path::new(r"C:\a b\suite.exe")),
            r#""C:\a b\suite.exe" --harness"#
        );
    }

    /// A scratch directory per test: the suite's unit tests run on several
    /// threads in one process, so the process id alone is not unique.
    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("uiharness-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The stub the `exited` tests drive: this test binary again, with a
    /// flag that makes it exit at once with a failure status. Waiting for it
    /// here keeps `exited()`'s single poll deterministic.
    fn exited_stub() -> std::process::Child {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--definitely-not-a-real-test-flag")
            .spawn()
            .unwrap();
        child.wait().unwrap();
        child
    }

    /// #722, criterion 4: when the instance exits before connecting, the
    /// `run` error must carry the tail of the desktop launch's output log —
    /// a refusal from the isolation gate is written to stderr and is the one
    /// message a caller most needs to see. The log here holds more than the
    /// 20 lines reported, so the cut is checked: the first line is absent,
    /// the last present.
    #[test]
    fn the_exited_report_carries_the_output_log_tail() {
        let dir = temp_dir("log-tail");
        let log = dir.join("suite-output.log");
        let mut text = String::new();
        for i in 0..25 {
            text.push_str(&format!("log line {i:03}\n"));
        }
        std::fs::write(&log, &text).unwrap();
        let mut app = Launched {
            proc_: Proc::Std(exited_stub()),
            sandbox: dir,
            exe: PathBuf::from("suite.exe"),
            detached: false,
            output_log: Some(log.clone()),
        };
        let msg = app.exited().expect("the stub has exited");
        assert!(msg.contains("suite.exe exited: "), "{msg}");
        assert!(
            msg.contains(&format!("--- {} (last 20 lines) ---", log.display())),
            "{msg}"
        );
        assert!(msg.contains("log line 024"), "{msg}");
        assert!(!msg.contains("log line 000"), "{msg}");
        assert!(!msg.contains("log line 004"), "{msg}");
    }

    /// Without a log — the plain launch path — `exited()` stays the one line
    /// it always was, so a desktop run cannot change what a caller of the
    /// ordinary one sees (#722, criterion 2).
    #[test]
    fn the_exited_report_without_a_log_stays_one_line() {
        let mut app = Launched {
            proc_: Proc::Std(exited_stub()),
            sandbox: temp_dir("no-log"),
            exe: PathBuf::from("suite.exe"),
            detached: false,
            output_log: None,
        };
        let msg = app.exited().expect("the stub has exited");
        assert!(msg.contains("suite.exe exited: "), "{msg}");
        assert!(!msg.contains("last 20 lines"), "{msg}");
        assert!(!msg.contains('\n'), "{msg}");
    }
}
