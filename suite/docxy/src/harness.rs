//! The suite's **opt-in** UI test harness surface: a [`ctlcore`] control server
//! that lets a test script drive this instance by verbs instead of by synthetic
//! desktop input.
//!
//! Wiring follows `docxy/src/control.rs` — the same `ctlcore::serve` listener,
//! the same token check (inside ctlcore), the same `reply_ok`/`reply_err`
//! shapes — so there is one style of control surface in the repo rather than
//! two. What differs is the pump: the terminal editor owns its event loop and
//! can select over requests, while here the app's thread belongs to gpui, so
//! requests are drained on the window's own foreground task (see [`attach`]).
//!
//! ## Two things this module refuses to do
//!
//! 1. **Start without isolation.** `--harness` is only honoured when
//!    `DOCXY_CONFIG_DIR` names a directory that is not the real config root.
//!    Task 1 measured why an `APPDATA` override cannot stand in for it:
//!    `dirs::config_dir()` asks the Windows known-folder API and ignores
//!    `APPDATA` entirely, so a test instance relying on it would keep writing
//!    `session.json` and the hot sidecars into the user's own profile — over
//!    the documents they have open. See [`gate`].
//! 2. **Publish its socket somewhere else.** The discovery file goes under
//!    [`control_dir`] — derived from the sandbox root — rather than through
//!    `ctlcore::config_ctl_dir`, which does its own `APPDATA` lookup and could
//!    put the socket in a different sandbox from the session state.
//!
//! Without the flag none of this runs: no listener, no discovery file, and the
//! real config root, exactly as before.

use crate::CONFIG_DIR_ENV;
use ctlcore::json::Json;
use gpui::{App, AsyncWindowContext, Context, Entity, Task, Window};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// The command-line flag that turns the harness on.
pub const HARNESS_FLAG: &str = "--harness";

/// The environment variable that turns the harness on, for launchers that
/// cannot add an argument (equivalent to passing [`HARNESS_FLAG`]).
pub const HARNESS_ENV: &str = "DOCXY_HARNESS";

/// The app name the control surface publishes itself under. Not `"docxy"`: the
/// terminal editor already owns that, and a harness instance is a different
/// thing to address even though it is the same product.
const CTL_APP: &str = "suite";

/// How often the pump looks for a queued request. Requests arrive on ctlcore's
/// own threads, but they may only be *applied* on the app thread, so the pump
/// polls rather than blocks. 8ms is under a frame at 60Hz — invisible to a test
/// — and only runs at all in harness mode.
const POLL: Duration = Duration::from_millis(8);

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

/// The command line, parsed. Everything that is not a flag is a candidate file
/// to open; whether it exists is the caller's business, so this stays pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// `--harness` was passed.
    pub harness: bool,
    /// Positional arguments, in order.
    pub files: Vec<PathBuf>,
    /// `--`-prefixed arguments that are not ours. Kept rather than silently
    /// dropped so a mistyped `--harnes` is reported instead of being treated as
    /// a file that does not exist and quietly ignored.
    pub unknown_flags: Vec<String>,
}

/// Parse the arguments *after* the executable name.
///
/// A literal `--` ends flag parsing, so a file genuinely named `--harness` can
/// still be opened.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Cli {
    let mut cli = Cli::default();
    let mut positional_only = false;
    for arg in args {
        if !positional_only {
            if arg == "--" {
                positional_only = true;
                continue;
            }
            if arg == HARNESS_FLAG {
                cli.harness = true;
                continue;
            }
            if let Some(s) = arg.to_str()
                && s.starts_with("--")
            {
                cli.unknown_flags.push(s.to_string());
                continue;
            }
        }
        cli.files.push(PathBuf::from(arg));
    }
    cli
}

/// Whether [`HARNESS_ENV`]'s value means "on". Unset is off; so are the usual
/// written-out falsehoods, so `DOCXY_HARNESS=0` in a shell profile does not
/// silently enable it. Anything else set is on — including a value that is not
/// valid UTF-8, which cannot be one of the off-words.
pub fn env_flag(value: Option<OsString>) -> bool {
    match value {
        None => false,
        Some(v) => match v.to_str() {
            None => true,
            Some(s) => !matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// The isolation gate
// ---------------------------------------------------------------------------

/// Decide whether a harness may start, returning the sandbox root it must use.
///
/// `over` is `DOCXY_CONFIG_DIR`'s value and `os_config` is `dirs::config_dir()`.
/// Refuses when the override is missing or blank, and when it points at the
/// real config directory — the two ways a mistyped invocation would end up
/// driving, and overwriting, the installed app's own state.
pub fn gate(over: Option<&OsStr>, os_config: Option<&Path>) -> Result<PathBuf, String> {
    let root = match over {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            return Err(format!(
                "{HARNESS_FLAG} requires {CONFIG_DIR_ENV} to point at a throwaway directory. \
                 Without it this instance writes session.json and the hot sidecars into the \
                 installed app's config and overwrites whatever the user has open. \
                 (An APPDATA override is not isolation: dirs::config_dir() ignores it.)"
            ));
        }
    };
    if let Some(os) = os_config
        && same_dir(&root, os)
    {
        return Err(format!(
            "{CONFIG_DIR_ENV} points at the real config directory ({}); \
             the harness will not drive the installed app's own state",
            os.display()
        ));
    }
    Ok(root)
}

/// Whether two paths name the same directory, textually: separators and a
/// trailing one are noise, and Windows paths are case-insensitive. Deliberately
/// not `canonicalize` — the sandbox root usually does not exist yet when the
/// gate runs, and a comparison that fails open would defeat the check.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    }
    norm(a) == norm(b)
}

// ---------------------------------------------------------------------------
// The control server
// ---------------------------------------------------------------------------

/// Where this instance publishes its discovery file: `<root>/suite/ctl`.
/// Derived from the sandbox root rather than looked up, so the socket and the
/// session state can never land in two different places.
pub fn control_dir(root: &Path) -> PathBuf {
    root.join(CTL_APP).join("ctl")
}

/// This instance's control name — `suite-<AGWINTERM_SESSION_ID|pid>`, the same
/// convention the terminal editors use.
pub fn instance_name() -> String {
    ctlcore::instance_name(CTL_APP)
}

/// Start the control server for a sandbox rooted at `root`.
pub fn start(root: &Path) -> std::io::Result<(ctlcore::Server, Receiver<ctlcore::Request>)> {
    ctlcore::serve(&control_dir(root), &instance_name())
}

/// The live harness, parked on the app so it lives as long as the window.
/// Dropping either field ends something: the server's `Drop` removes the
/// discovery file, and dropping a gpui `Task` cancels the pump.
pub struct Harness {
    _server: ctlcore::Server,
    _pump: Task<()>,
}

/// Bring the harness up on `view`: drain requests on the window's foreground
/// task, apply each one to the app, and answer it.
pub fn attach(
    view: &Entity<crate::Docxy>,
    server: ctlcore::Server,
    rx: Receiver<ctlcore::Request>,
    window: &mut Window,
    cx: &mut App,
) {
    let target = view.clone();
    let pump = window.spawn(cx, async move |cx: &mut AsyncWindowContext| {
        loop {
            match rx.try_recv() {
                Ok(req) => {
                    let (verb, args) = (req.verb.clone(), req.args.clone());
                    match target.update_in(cx, |this, window, cx| {
                        dispatch(this, &verb, &args, window, cx)
                    }) {
                        Ok(Ok(result)) => req.reply_ok(result),
                        Ok(Err(e)) => req.reply_err(e),
                        // The window closed between the request arriving and it
                        // being applied; answer rather than leave a client hung.
                        Err(e) => req.reply_err(format!("the app is gone: {e}")),
                    }
                }
                Err(TryRecvError::Empty) => cx.background_executor().timer(POLL).await,
                // The server was dropped: nothing more will arrive.
                Err(TryRecvError::Disconnected) => break,
            }
        }
    });
    view.update(cx, |this, _| {
        this.harness = Some(Harness {
            _server: server,
            _pump: pump,
        })
    });
}

/// Route one harness verb against the live app.
///
/// Task 3 grows this into the verbs that drive the grid, each going through the
/// same entry point the real UI uses. Today there is only `ping`, which proves
/// the channel end to end — including that the reply was produced on the app
/// thread, since it reports the app's own state.
pub fn dispatch(
    app: &mut crate::Docxy,
    verb: &str,
    _args: &Json,
    _window: &mut Window,
    _cx: &mut Context<crate::Docxy>,
) -> Result<Json, String> {
    match verb {
        "ping" => Ok(Json::obj(vec![
            ("instance", Json::Str(instance_name())),
            ("pid", Json::Num(std::process::id() as f64)),
            (
                "config_root",
                Json::Str(crate::config_root().display().to_string()),
            ),
            ("tabs", Json::Num(app.tabs.len() as f64)),
        ])),
        other => Err(format!("unknown verb '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    // ---- flag parsing ----

    /// The ordinary launch: files only, and no harness.
    #[test]
    fn plain_files_do_not_enable_the_harness() {
        let cli = parse_args(args(&[r"C:\docs\a.docx", "b.xlsx"]));
        assert!(!cli.harness);
        assert_eq!(
            cli.files,
            vec![PathBuf::from(r"C:\docs\a.docx"), PathBuf::from("b.xlsx")]
        );
        assert!(cli.unknown_flags.is_empty());
    }

    /// The flag is recognized wherever it appears, and never counted as a file.
    #[test]
    fn harness_flag_is_recognized_and_is_not_a_file() {
        let cli = parse_args(args(&["a.xlsx", HARNESS_FLAG, "b.xlsx"]));
        assert!(cli.harness);
        assert_eq!(
            cli.files,
            vec![PathBuf::from("a.xlsx"), PathBuf::from("b.xlsx")]
        );
    }

    /// No arguments at all is the double-click case: nothing on, nothing to open.
    #[test]
    fn empty_command_line_is_a_plain_launch() {
        assert_eq!(parse_args(args(&[])), Cli::default());
    }

    /// A near-miss must be reported, not swallowed. If `--harnes` were treated
    /// as a file it would vanish (no such file) and the app would come up
    /// looking normal while the test waited forever for a socket.
    #[test]
    fn unknown_flags_are_reported_rather_than_treated_as_files() {
        let cli = parse_args(args(&["--harnes", "--verbose"]));
        assert!(!cli.harness);
        assert!(cli.files.is_empty());
        assert_eq!(cli.unknown_flags, vec!["--harnes", "--verbose"]);
    }

    /// After `--`, a file that happens to look like our flag is still a file.
    #[test]
    fn double_dash_ends_flag_parsing() {
        let cli = parse_args(args(&["--", HARNESS_FLAG]));
        assert!(!cli.harness);
        assert_eq!(cli.files, vec![PathBuf::from(HARNESS_FLAG)]);
    }

    // ---- the env alternative ----

    #[test]
    fn env_flag_reads_on_and_off_values() {
        assert!(!env_flag(None));
        assert!(!env_flag(Some(OsString::from(""))));
        assert!(!env_flag(Some(OsString::from("0"))));
        assert!(!env_flag(Some(OsString::from("false"))));
        assert!(!env_flag(Some(OsString::from(" OFF "))));
        assert!(env_flag(Some(OsString::from("1"))));
        assert!(env_flag(Some(OsString::from("true"))));
        assert!(env_flag(Some(OsString::from("yes"))));
    }

    // ---- the isolation gate ----

    /// The happy path: an override that is somewhere else entirely.
    #[test]
    fn gate_accepts_a_sandbox_root() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        let root = gate(Some(OsStr::new(r"D:\runs\harness-42")), Some(&os))
            .expect("a sandbox root is accepted");
        assert_eq!(root, PathBuf::from(r"D:\runs\harness-42"));
    }

    /// No override: refuse, and say why. This is the case the whole gate exists
    /// for — starting here would write into the user's real profile.
    #[test]
    fn gate_refuses_without_an_override() {
        let err = gate(None, Some(Path::new(r"C:\Users\someone\AppData\Roaming")))
            .expect_err("no override must be refused");
        assert!(
            err.contains(CONFIG_DIR_ENV),
            "message names the variable: {err}"
        );
    }

    /// An exported-but-blank variable is a shell accident, and `config_root`
    /// already treats it as unset — so the gate must refuse it too, rather than
    /// approve a sandbox that is really the config directory.
    #[test]
    fn gate_refuses_a_blank_override() {
        let err = gate(
            Some(OsStr::new("")),
            Some(Path::new(r"C:\Users\someone\AppData\Roaming")),
        )
        .expect_err("a blank override must be refused");
        assert!(err.contains(CONFIG_DIR_ENV));
    }

    /// Pointing the override at the real config directory is the mistyped
    /// invocation that would drive the user's own instance. Refuse it, and
    /// refuse the trailing-separator and wrong-case spellings of it too.
    #[test]
    fn gate_refuses_the_real_config_directory() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        for spelling in [
            r"C:\Users\someone\AppData\Roaming",
            r"C:\Users\someone\AppData\Roaming\",
            r"C:/Users/someone/AppData/Roaming",
        ] {
            let err = gate(Some(OsStr::new(spelling)), Some(&os))
                .expect_err(&format!("{spelling} must be refused"));
            assert!(
                err.contains("real config directory"),
                "message says what is wrong: {err}"
            );
        }
    }

    /// With no OS config directory to compare against (an odd profile), an
    /// override is still enough — there is nothing it could collide with.
    #[test]
    fn gate_accepts_when_the_os_config_dir_is_unknown() {
        assert_eq!(
            gate(Some(OsStr::new("target/harness-cfg")), None),
            Ok(PathBuf::from("target/harness-cfg"))
        );
    }

    #[test]
    fn same_dir_ignores_separator_style_trailing_slash_and_case() {
        assert!(same_dir(
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new(r"C:\Users\a\AppData\Roaming\")
        ));
        assert!(same_dir(
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new("C:/Users/a/AppData/Roaming")
        ));
        assert!(!same_dir(
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new(r"C:\Users\a\AppData\Roaming2")
        ));
        if cfg!(windows) {
            assert!(same_dir(
                Path::new(r"C:\Users\a\AppData\Roaming"),
                Path::new(r"c:\users\a\appdata\roaming")
            ));
        }
    }

    // ---- discovery placement ----

    /// The discovery file must sit under the sandbox, not wherever
    /// `ctlcore::config_ctl_dir` would put it — that reads APPDATA and could
    /// name a different sandbox from the one holding session.json.
    #[test]
    fn control_dir_is_under_the_sandbox_root() {
        let root = Path::new(r"D:\runs\harness-42");
        let dir = control_dir(root);
        assert_eq!(dir, root.join("suite").join("ctl"));
        assert!(dir.starts_with(root));
    }
}
