//! Desktop-only check that a harness instance started from an agwinterm pane
//! behaves as one started anywhere else (#697). Run after building suite:
//! cargo test -p uiharness --test agwinterm_env -- --ignored --nocapture
//!
//! ⚠️ Readiness alone is not the regression: with `AGWINTERM_PIPE` pointing at
//! a pipe that is not there, the suite before #697 came up and answered too.
//! What fails without the fix is the instance name (it was the pane id, not
//! `suite-<pid>`) and the `agwintermctl` child every edit spawned. The child
//! check is best-effort — a CLI that fails fast on a missing pipe may exit
//! between two polls — so the name is the deterministic assertion.
#![cfg(windows)]

use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uiharness::{Driver, Run, launch};

/// What an agwinterm pane puts in the environment, pointed nowhere.
const FAKES: [(&str, &str); 5] = [
    ("AGWINTERM_ENABLED", "1"),
    ("AGWINTERM_SESSION_ID", "697-fake-pane"),
    ("AGWINTERM_PANE_ID", "697-fake-pane"),
    ("AGWINTERM_WINDOW_ID", "697-fake-window"),
    ("AGWINTERM_PIPE", r"\\.\pipe\docxy-697-no-such-pipe"),
];

fn call(driver: &Driver, verb: &str, args: Vec<(&str, Json)>) -> Json {
    driver
        .call(verb, Json::obj(args))
        .unwrap_or_else(|e| panic!("{verb}: {e}"))
}

fn sandbox(tag: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let run = Run::create(
        root.join("../target/agwinterm-env-tests")
            .join(format!("{}-{stamp}", std::process::id())),
    )
    .unwrap();
    let sandbox = std::path::absolute(run.dir().join(tag)).unwrap();
    std::fs::create_dir_all(&sandbox).unwrap();
    sandbox
}

/// The executable names of `pid`'s live children.
fn children(pid: u32) -> Vec<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    // SAFETY: a snapshot handle we own and close, and an entry sized as the
    // API requires.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            if e.th32ParentProcessID == pid {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                out.push(String::from_utf16_lossy(&e.szExeFile[..len]));
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Watch `pid`'s children until dropped, remembering any `agwintermctl`.
struct ChildWatch {
    stop: Arc<AtomicBool>,
    seen: std::thread::JoinHandle<Vec<String>>,
}

impl ChildWatch {
    fn start(pid: u32) -> ChildWatch {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let seen = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !flag.load(Ordering::Relaxed) {
                for name in children(pid) {
                    if name.to_ascii_lowercase().starts_with("agwintermctl")
                        && !seen.contains(&name)
                    {
                        seen.push(name);
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            seen
        });
        ChildWatch { stop, seen }
    }

    fn finish(self) -> Vec<String> {
        self.stop.store(true, Ordering::Relaxed);
        self.seen.join().unwrap()
    }
}

/// Connect, check the name, edit a plan while watching for `agwintermctl`, and
/// quit.
fn exercise(mut child: Child, sandbox: &Path, how: &str) {
    let pid = child.id();
    let driver = match Driver::connect(&uiharness::driver::control_dir(sandbox), None) {
        Ok(d) => d,
        Err(e) => {
            let _ = child.kill();
            panic!("{how}: the suite never became ready: {e}");
        }
    };
    let ping = call(&driver, "ping", vec![]);
    assert_eq!(
        ping.get_str("instance"),
        Some(format!("suite-{pid}").as_str()),
        "{how}: {ping}"
    );
    let watch = ChildWatch::start(pid);
    call(&driver, "proj.new", vec![]);
    for name in ["First", "Second"] {
        call(
            &driver,
            "task.add",
            vec![
                ("name", Json::Str(name.into())),
                ("duration", Json::Str("2d".into())),
            ],
        );
        // Long enough for a spawned CLI to show up in a snapshot.
        std::thread::sleep(Duration::from_millis(300));
    }
    let seen = watch.finish();
    assert!(
        seen.is_empty(),
        "{how}: the harness instance spawned {seen:?}"
    );
    let _ = driver.call("quit", Json::obj(vec![]));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Through the launcher: this process's environment plus the fakes, as the
/// launcher would see it in a pane — every agwinterm variable removed.
#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn a_launched_instance_ignores_the_pane_it_was_started_from() {
    let exe = launch::find_suite(None).unwrap();
    let sandbox = sandbox("launched");
    let parent = std::env::vars_os().chain(FAKES.map(|(k, v)| (k.into(), v.into())));
    let child = launch::command(&exe, &sandbox, parent).spawn().unwrap();
    exercise(child, &sandbox, "launched");
}

/// Not through the launcher (the spec repos' `desk_launch.ps1` inherits the
/// whole environment): the fakes reach the suite, which must ignore them.
#[test]
#[ignore = "requires a built suite and an interactive desktop"]
fn an_instance_that_inherits_the_pane_variables_ignores_them() {
    let exe = launch::find_suite(None).unwrap();
    let sandbox = sandbox("inherited");
    let child = Command::new(&exe)
        .arg(launch::HARNESS_FLAG)
        .env(launch::CONFIG_DIR_ENV, &sandbox)
        .envs(FAKES)
        .current_dir(&sandbox)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    exercise(child, &sandbox, "inherited");
}
