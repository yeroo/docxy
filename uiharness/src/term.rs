//! Driving a terminal editor (docxy, xlsxy, yppxy) the way a person does
//! (#1204).
//!
//! A script whose first line is `terminal <app> [args…]` runs here instead of
//! against the suite. Each `open` starts the editor afresh, on that file, in a
//! [`crate::pty`] pseudo-terminal of 120×40, with its config directories
//! inside the run's sandbox, and connects to the control server every editor
//! serves. Then:
//!
//! - `real-key` / `real-type` write each key's terminal bytes
//!   ([`crate::termkeys`]) into the terminal, one key at a time. After each,
//!   the step waits until the editor has *handled* one more key press
//!   (`field-read`'s `keys`) and then drawn a frame after that, so the next
//!   read sees what the screen shows. A key the editor never handles is an
//!   `Error` naming it; one that arrives as more than one press is too.
//! - `pointer-click {"input":id}` reads where the field was drawn
//!   (`field-read`'s `at`) and writes an SGR mouse press and release there,
//!   waiting for the click to be handled as for a key.
//! - every other `call` goes to the editor's own control server; `field-read
//!   {"input":id}` is one of its verbs (`ctlcore::field`).
//!
//! The terminal is the harness's: nothing else draws it and nothing else
//! answers it. docxy and xlsxy ask it, at start, what image protocol it
//! speaks (ratatui-image's `Picker::from_query_stdio`). That query's reader
//! thread reads stdin until it sees the answer to `ESC [ 5 n`, and when
//! nobody answers it stays behind after its 1-second timeout and swallows
//! typed keys. So the thread draining the terminal's output answers `ESC [ 5
//! n` (and `ESC [ c`) the moment they appear, in the order they appear. On
//! Windows conhost answers what it understands itself and only passes on
//! what it does not, so only what reaches the output is answered.
//!
//! The screen is never parsed beyond that: what a case asserts comes from
//! the editor's own verbs.

use crate::driver::Driver;
use crate::pty::{Pty, Spawn};
use crate::termkeys::{self, Key};
use ctlcore::json::Json;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The terminal editors a script may name.
pub const APPS: &[&str] = &["docxy", "xlsxy", "yppxy"];

/// The terminal's size. Wide enough for every editor's status line and
/// ribbon, so no prompt is clipped.
pub const COLS: u16 = 120;
pub const ROWS: u16 = 40;

/// How long an editor has to start and publish its control server.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one key, click or frame may take. Generous: a debug build on a
/// loaded CI runner redraws slowly, and a timeout is an `Error`, not a
/// flake to retry.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to watch for a stray extra key press after the last one sent.
const SETTLE: Duration = Duration::from_millis(60);

/// How much of the terminal's output to keep, for a failure to quote.
const SCREEN_TAIL: usize = 16 * 1024;

/// What a script's `terminal` line names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpec {
    pub app: String,
    /// Passed before the file, e.g. `--vim`.
    pub args: Vec<String>,
}

/// Where the terminal editors' binaries are: `dir`, or the directory this
/// harness runs from (the workspace's `target/<profile>/`, where `cargo
/// build -p docxy -p xlsxy -p yppxy` puts them).
pub fn find_editor(app: &str, dir: Option<&Path>) -> Result<PathBuf, String> {
    let dir = match dir {
        Some(d) => std::path::absolute(d).map_err(|e| format!("{}: {e}", d.display()))?,
        None => std::env::current_exe()
            .map_err(|e| format!("where this harness runs from: {e}"))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or("this harness runs from no directory")?,
    };
    let exe = dir.join(format!("{app}{}", std::env::consts::EXE_SUFFIX));
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(format!(
            "no {app} binary at {} — build the terminal editors next to uiharness \
             (`cargo build --release -p docxy -p xlsxy -p yppxy -p uiharness`), \
             or name their directory with --term-bin",
            exe.display()
        ))
    }
}

/// The environment an editor starts with: this process's, without the
/// agwinterm variables (#697) and with every config root it reads pointed
/// into `home`, so its preferences, recent files and control discovery file
/// are the case's own.
pub fn editor_env(
    home: &Path,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const OURS: &[&str] = &[
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "USERPROFILE",
        "TERM",
        "COLORTERM",
        "TMUX",
        "TERM_PROGRAM",
        "DOCXY_CONFIG_DIR",
    ];
    let mut env: Vec<(OsString, OsString)> = parent
        .into_iter()
        .filter(|(k, _)| !crate::launch::is_terminal_integration_var(k))
        .filter(|(k, _)| {
            !OURS
                .iter()
                .any(|o| k.to_string_lossy().eq_ignore_ascii_case(o))
        })
        .collect();
    let p = |sub: &str| home.join(sub).into_os_string();
    env.push(("APPDATA".into(), p("appdata")));
    env.push(("LOCALAPPDATA".into(), p("localappdata")));
    env.push(("XDG_CONFIG_HOME".into(), p("config")));
    env.push(("XDG_DATA_HOME".into(), p("data")));
    env.push(("XDG_STATE_HOME".into(), p("state")));
    env.push(("XDG_CACHE_HOME".into(), p("cache")));
    env.push(("HOME".into(), home.as_os_str().to_os_string()));
    env.push(("USERPROFILE".into(), home.as_os_str().to_os_string()));
    // An xterm, and nothing that would make ratatui-image guess at tmux or
    // iTerm2.
    env.push(("TERM".into(), "xterm-256color".into()));
    env
}

/// Where an editor started with [`editor_env`] publishes its control server.
pub fn editor_ctl_dir(home: &Path, app: &str) -> PathBuf {
    let base = if cfg!(windows) {
        home.join("appdata")
    } else {
        home.join("config")
    };
    base.join(app).join("ctl")
}

/// The answers a terminal owes for the queries in `out`, in the order asked.
/// `carry` holds the end of the previous chunk, so a query split across two
/// reads is still seen once; it is updated for the next call.
pub fn answers(carry: &mut Vec<u8>, out: &[u8]) -> Vec<u8> {
    const QUERIES: &[(&[u8], &[u8])] = &[
        // Device Status Report: "OK". ratatui-image reads until this one.
        (b"\x1b[5n", b"\x1b[0n"),
        // Primary Device Attributes: a VT220, no sixel.
        (b"\x1b[c", b"\x1b[?62c"),
        (b"\x1b[0c", b"\x1b[?62c"),
    ];
    let mut buf = std::mem::take(carry);
    buf.extend_from_slice(out);
    let mut reply = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if let Some((q, a)) = QUERIES.iter().find(|(q, _)| buf[i..].starts_with(q)) {
            // The carry never holds a whole query (below), so this one has
            // not been answered yet.
            reply.extend_from_slice(a);
            i += q.len();
        } else {
            i += 1;
        }
    }
    // Keep enough to see a query that starts in this chunk and ends in the
    // next, but never a whole one: it would be answered twice.
    let longest = QUERIES.iter().map(|(q, _)| q.len()).max().unwrap_or(0);
    let mut start = buf.len().saturating_sub(longest - 1);
    while start < buf.len() && QUERIES.iter().any(|(q, _)| buf[start..].starts_with(q)) {
        start += 1;
    }
    *carry = buf[start..].to_vec();
    reply
}

/// The counters `field-read` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    keys: u64,
    clicks: u64,
    frames: u64,
}

fn counts_of(j: &Json) -> Result<Counts, String> {
    let n = |k: &str| {
        j.get(k)
            .and_then(Json::as_f64)
            .map(|v| v as u64)
            .ok_or_else(|| format!("field-read has no '{k}': {j}"))
    };
    Ok(Counts {
        keys: n("keys")?,
        clicks: n("clicks")?,
        frames: n("frames")?,
    })
}

/// One running editor.
struct Session {
    pty: Pty,
    input: Box<dyn Write + Send>,
    driver: Driver,
    screen: Arc<Mutex<Vec<u8>>>,
}

impl Session {
    fn screen_tail(&self) -> String {
        let s = self.screen.lock().unwrap_or_else(|e| e.into_inner());
        let start = s.len().saturating_sub(600);
        String::from_utf8_lossy(&s[start..])
            .chars()
            .map(|c| if c.is_control() && c != '\n' { '·' } else { c })
            .collect()
    }
}

/// A terminal editor, (re)started by each `open`.
pub struct Terminal {
    spec: TerminalSpec,
    exe: PathBuf,
    sandbox: PathBuf,
    session: Option<Session>,
    starts: usize,
}

impl Terminal {
    pub fn new(spec: TerminalSpec, exe: PathBuf, sandbox: PathBuf) -> Terminal {
        Terminal {
            spec,
            exe,
            sandbox,
            session: None,
            starts: 0,
        }
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Stop the running editor, if one is.
    pub fn stop(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.pty.kill();
        }
    }

    /// Start the editor on `file`, replacing the one running.
    pub fn open(&mut self, file: &Path) -> Result<(), String> {
        self.stop();
        self.starts += 1;
        let home = self.sandbox.join(format!("term-{}", self.starts));
        std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
        let home = std::path::absolute(&home).map_err(|e| format!("{}: {e}", home.display()))?;
        for sub in [
            "appdata",
            "localappdata",
            "config",
            "data",
            "state",
            "cache",
        ] {
            let _ = std::fs::create_dir_all(home.join(sub));
        }
        let mut args: Vec<OsString> = self.spec.args.iter().map(OsString::from).collect();
        args.push(file.as_os_str().to_os_string());
        let cwd = file
            .parent()
            .filter(|d| d.is_dir())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.clone());
        let spawn = Spawn {
            exe: self.exe.clone(),
            args,
            env: editor_env(&home, std::env::vars_os()),
            cwd,
            cols: COLS,
            rows: ROWS,
        };
        let (mut pty, output) = Pty::spawn(&spawn)?;
        let screen = Arc::new(Mutex::new(Vec::new()));
        drain(output, pty.writer()?, screen.clone());
        let input = pty.writer()?;

        let ctl = editor_ctl_dir(&home, &self.spec.app);
        let name = format!("{}-{}", self.spec.app, pty.pid());
        let deadline = Instant::now() + START_TIMEOUT;
        let driver = loop {
            if let Some(how) = pty.exited() {
                let tail =
                    String::from_utf8_lossy(&screen.lock().unwrap_or_else(|e| e.into_inner()))
                        .into_owned();
                return Err(format!(
                    "{} exited before it was ready ({how}); the terminal showed: {tail:?}",
                    self.exe.display()
                ));
            }
            if let Ok(d) = Driver::connect_within(&ctl, Some(&name), Duration::ZERO) {
                break d;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} published no control server '{name}' in {} within {START_TIMEOUT:?}",
                    self.exe.display(),
                    ctl.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let session = Session {
            pty,
            input,
            driver,
            screen,
        };
        self.session = Some(session);
        // Ready once it answers and has drawn: the start-up query is behind
        // it, since the control server comes up after it.
        self.wait("its first frame", |c| c.frames >= 1)?;
        Ok(())
    }

    fn session(&mut self) -> Result<&mut Session, String> {
        self.session
            .as_mut()
            .ok_or_else(|| "no editor is running: start one with 'open'".to_string())
    }

    fn counts(&mut self) -> Result<Counts, String> {
        let s = self.session()?;
        counts_of(&s.driver.call("field-read", Json::obj(vec![]))?)
    }

    /// Poll the counters until `done` holds.
    fn wait(&mut self, what: &str, done: impl Fn(Counts) -> bool) -> Result<Counts, String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let c = self.counts()?;
            if done(c) {
                return Ok(c);
            }
            if let Some(how) = self.session()?.pty.exited() {
                return Err(format!("the editor exited waiting for {what} ({how})"));
            }
            if Instant::now() >= deadline {
                let tail = self.session()?.screen_tail();
                return Err(format!(
                    "waited {STEP_TIMEOUT:?} for {what}; the editor's counters are {c:?}\n\
                     the terminal's last output: {tail}"
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Write `bytes`, then wait until `took` holds of the counters and a
    /// frame has been drawn after that.
    fn send(
        &mut self,
        what: &str,
        bytes: &[u8],
        took: impl Fn(Counts, Counts) -> bool,
    ) -> Result<(), String> {
        let before = self.counts()?;
        {
            let s = self.session()?;
            s.input
                .write_all(bytes)
                .map_err(|e| format!("writing {what} to the terminal: {e}"))?;
        }
        let after = self.wait(&format!("the editor to take {what}"), |c| took(before, c))?;
        self.wait(&format!("a frame after {what}"), |c| {
            c.frames > after.frames
        })?;
        Ok(())
    }

    /// Press `keys` (each named `names[i]` in a message), one at a time.
    fn press(&mut self, keys: &[(String, Key)]) -> Result<Json, String> {
        let start = self.counts()?;
        for (name, key) in keys {
            let bytes = termkeys::key_bytes(key)?;
            self.send(&format!("the key '{name}'"), &bytes, |b, c| c.keys > b.keys)?;
        }
        std::thread::sleep(SETTLE);
        let end = self.counts()?;
        let sent = keys.len() as u64;
        if end.keys != start.keys + sent {
            return Err(format!(
                "{sent} key(s) sent but the editor handled {}: the terminal turned a key into \
                 more than one press",
                end.keys - start.keys
            ));
        }
        Ok(Json::obj(vec![("keys", Json::Num(sent as f64))]))
    }

    fn click(&mut self, args: &Json) -> Result<Json, String> {
        let id = args
            .get_str("input")
            .ok_or("a terminal's pointer-click takes {\"input\":id}")?
            .to_string();
        let s = self.session()?;
        let read = s.driver.call(
            "field-read",
            Json::obj(vec![("input", Json::Str(id.clone()))]),
        )?;
        let at = read
            .get("at")
            .filter(|a| **a != Json::Null)
            .ok_or_else(|| format!("'{id}' is not drawn, so there is nothing to click: {read}"))?;
        let cell = |k: &str| {
            at.get(k)
                .and_then(Json::as_f64)
                .map(|v| v as u16)
                .ok_or_else(|| format!("field-read's 'at' has no '{k}': {read}"))
        };
        let (col, row) = (cell("col")?, cell("row")?);
        self.send(
            &format!("a click on '{id}'"),
            &termkeys::sgr_click(col, row),
            |b, c| c.clicks > b.clicks,
        )?;
        Ok(Json::obj(vec![
            ("input", Json::Str(id)),
            ("col", Json::Num(f64::from(col))),
            ("row", Json::Num(f64::from(row))),
            ("clicks", Json::Num(1.0)),
        ]))
    }

    /// A step's verb: the terminal's own (`real-key`, `real-type`,
    /// `pointer-click`), else the editor's.
    pub fn call(&mut self, verb: &str, args: Json) -> Result<Json, String> {
        match verb {
            "real-key" => {
                let names: Vec<String> = match (args.get("key"), args.get("keys")) {
                    (Some(k), None) => vec![k.as_str().ok_or("'key' must be a string")?.into()],
                    (None, Some(Json::Arr(ks))) => ks
                        .iter()
                        .map(|k| k.as_str().map(String::from).ok_or("'keys' are strings"))
                        .collect::<Result<_, _>>()?,
                    _ => return Err("real-key takes 'key' or 'keys'".into()),
                };
                let times = match args.get("times") {
                    None => 1,
                    Some(v) => v
                        .as_f64()
                        .filter(|n| n.fract() == 0.0 && (1.0..=500.0).contains(n))
                        .ok_or("'times' must be a whole number from 1 to 500")?
                        as usize,
                };
                let mut keys = Vec::new();
                for _ in 0..times {
                    for n in &names {
                        keys.push((n.clone(), termkeys::parse_key(n)?));
                    }
                }
                self.press(&keys)
            }
            "real-type" => {
                let text = args.get_str("text").ok_or("real-type takes 'text'")?;
                let keys: Vec<(String, Key)> = text
                    .chars()
                    .zip(termkeys::typed(text))
                    .map(|(c, k)| (c.to_string(), k))
                    .collect();
                self.press(&keys)
            }
            "pointer-click" => self.click(&args),
            _ => self.session()?.driver.call(verb, args),
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Drain the terminal's output on a thread for as long as it has any,
/// keeping its tail and answering the queries it carries.
fn drain(
    mut output: Box<dyn Read + Send>,
    mut answer: Box<dyn Write + Send>,
    screen: Arc<Mutex<Vec<u8>>>,
) {
    let _ = std::thread::Builder::new()
        .name("uiharness-pty-drain".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            let mut carry = Vec::new();
            loop {
                let n = match output.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let reply = answers(&mut carry, &buf[..n]);
                if !reply.is_empty() {
                    let _ = answer.write_all(&reply);
                    let _ = answer.flush();
                }
                let mut s = screen.lock().unwrap_or_else(|e| e.into_inner());
                s.extend_from_slice(&buf[..n]);
                if s.len() > SCREEN_TAIL {
                    let cut = s.len() - SCREEN_TAIL;
                    s.drain(..cut);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_and_attribute_queries_are_answered_in_order() {
        let mut carry = Vec::new();
        let q = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[16t\x1b[1337n\x1b[5n";
        assert_eq!(answers(&mut carry, q), b"\x1b[?62c\x1b[0n");
        assert_eq!(answers(&mut carry, b"plain output"), b"");
    }

    #[test]
    fn a_query_split_across_reads_is_answered_once() {
        let mut carry = Vec::new();
        assert_eq!(answers(&mut carry, b"draw\x1b[5"), b"");
        assert_eq!(answers(&mut carry, b"n more"), b"\x1b[0n");
        // A whole query at a chunk's end is answered there and never again.
        let mut carry = Vec::new();
        assert_eq!(answers(&mut carry, b"x\x1b[5n"), b"\x1b[0n");
        assert_eq!(answers(&mut carry, b"y"), b"");
        let mut carry = Vec::new();
        assert_eq!(answers(&mut carry, b"\x1b["), b"");
        assert_eq!(answers(&mut carry, b"c"), b"\x1b[?62c");
    }

    #[test]
    fn the_editor_gets_its_config_roots_in_the_sandbox_and_no_agwinterm() {
        let home = Path::new("H");
        let env = editor_env(
            home,
            vec![
                ("AGWINTERM_SESSION_ID".into(), "7".into()),
                ("appdata".into(), "C:/real".into()),
                ("PATH".into(), "/bin".into()),
                ("TERM".into(), "screen".into()),
            ],
        );
        let get = |k: &str| {
            env.iter()
                .filter(|(n, _)| n.to_string_lossy().eq_ignore_ascii_case(k))
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert!(get("AGWINTERM_SESSION_ID").is_empty());
        assert_eq!(get("APPDATA"), [home.join("appdata").to_string_lossy()]);
        assert_eq!(
            get("XDG_CONFIG_HOME"),
            [home.join("config").to_string_lossy()]
        );
        assert_eq!(get("HOME"), ["H"]);
        assert_eq!(get("TERM"), ["xterm-256color"]);
        assert_eq!(get("PATH"), ["/bin"]);
        assert_eq!(
            editor_ctl_dir(home, "docxy"),
            if cfg!(windows) {
                home.join("appdata").join("docxy").join("ctl")
            } else {
                home.join("config").join("docxy").join("ctl")
            }
        );
    }

    #[test]
    fn a_missing_editor_binary_says_how_to_build_it() {
        let e = find_editor("docxy", Some(Path::new("no-such-dir"))).unwrap_err();
        assert!(e.contains("cargo build --release -p docxy"), "{e}");
    }
}
