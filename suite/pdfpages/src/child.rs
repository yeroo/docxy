//! `docxy-pdf <file> [<ms>]`: the child terminal docxy draws and reads a
//! PDF's pages in (#2259), as the suite draws in its `--pdf-worker` child.
//!
//! Protocol 1, one JSON line per request on stdin and one per answer on
//! stdout:
//!
//! - first the child writes `{"hello":"docxy-pdf","protocol":1}`;
//! - `{"op":"info"}` answers [`crate::Doc::info_json`];
//! - `{"op":"pixels","page":0,"scale":1.5}` answers
//!   `{"ok":true,"width":W,"height":H,"len":N}` and then N bytes of
//!   straight RGBA, or `{"ok":false,"error"}`;
//! - `{"op":"text","page":0}` answers [`crate::Doc::text_json`];
//! - anything else answers `{"ok":false,"error"}`: a bad request never
//!   ends the child.
//!
//! It ends at the end of its input (the parent closed it, or died), and by
//! its own watchdog when one request runs past twice its timeout. On Linux
//! it limits its own memory and asks the OOM killer to take it first, as
//! the suite's worker does.

use crate::Doc;
use serde::Deserialize;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The first word of the hello line.
pub const HELLO: &str = "docxy-pdf";
/// The protocol's revision, in the hello line.
pub const PROTOCOL: u32 = 1;
/// How long one request may take by default: the suite's
/// `convert_child::TIMEOUT`. The parent waits this long; the watchdog
/// twice that.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// The child's data limit on Linux: the suite's `convert_child::MEMORY_LIMIT`.
pub const MEMORY_LIMIT: usize = 2 << 30;
/// The exit code when the watchdog ends the child.
pub const EXIT_WATCHDOG: i32 = 4;
/// The exit code on a bad command line.
pub const EXIT_USAGE: i32 = 2;

/// One request.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum Request {
    Info,
    Pixels { page: usize, scale: f32 },
    Text { page: usize },
}

/// Answer requests from `input` on `output` until `input` ends: the hello
/// line, then one answer per request. A file that could not be read
/// answers every request with why. `busy`, when given, is told when each
/// request starts and ends (the watchdog's clock).
pub fn serve(
    bytes: Result<Vec<u8>, String>,
    mut input: impl BufRead,
    mut output: impl Write,
    busy: Option<&Busy>,
) {
    let hello = serde_json::json!({"hello": HELLO, "protocol": PROTOCOL});
    if writeln!(output, "{hello}").is_err() || output.flush().is_err() {
        return;
    }
    let mut doc = bytes.map(Doc::new);
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(b) = busy {
            b.start();
        }
        let (head, body) = answer(&mut doc, line.trim());
        if let Some(b) = busy {
            b.stop();
        }
        if writeln!(output, "{head}").is_err()
            || output.write_all(&body).is_err()
            || output.flush().is_err()
        {
            return;
        }
    }
}

fn answer(doc: &mut Result<Doc, String>, line: &str) -> (String, Vec<u8>) {
    let req = match serde_json::from_str::<Request>(line) {
        Ok(req) => req,
        Err(e) => {
            return (
                crate::failure_json(&format!("a request the reader does not know: {e}"), false),
                vec![],
            );
        }
    };
    let doc = match doc {
        Ok(doc) => doc,
        Err(why) => return (crate::failure_json(why, false), vec![]),
    };
    match req {
        Request::Info => (doc.info_json(), vec![]),
        Request::Text { page } => (doc.text_json(page), vec![]),
        Request::Pixels { page, scale } => match doc.pixels(page, scale) {
            Ok(r) => (
                serde_json::json!({
                    "ok": true,
                    "width": r.width,
                    "height": r.height,
                    "len": r.rgba.len(),
                })
                .to_string(),
                r.rgba,
            ),
            Err(e) => (crate::failure_json(&e, false), vec![]),
        },
    }
}

/// Serve `path` on this process's stdin and stdout, under a watchdog that
/// ends the process when one request runs past twice `timeout`.
pub fn run(path: &std::path::Path, timeout: Duration) -> i32 {
    first_to_go();
    limit_memory();
    let bytes = std::fs::read(path).map_err(|e| format!("the file could not be read: {e}"));
    let busy = Arc::new(Busy::default());
    watchdog(busy.clone(), timeout.saturating_mul(2));
    let stdin = std::io::stdin().lock();
    let stdout = std::io::BufWriter::new(std::io::stdout().lock());
    serve(bytes, stdin, stdout, Some(&busy));
    0
}

/// On Linux the child asks to be the first process the OOM killer takes,
/// before the editor that started it.
fn first_to_go() {
    #[cfg(target_os = "linux")]
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
}

/// On Linux the child limits its own data to [`MEMORY_LIMIT`]: past it an
/// allocation fails and the child aborts, and the parent fails the page.
/// `RLIMIT_DATA`, not `RLIMIT_AS` (glibc's arenas reserve address space
/// they never use), and no core for that abort (`RLIMIT_CORE` 0). The
/// suite's worker's `limit_memory`, which this crate cannot link.
fn limit_memory() {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::{c_int, c_ulong};
        #[repr(C)]
        struct Rlimit {
            cur: c_ulong,
            max: c_ulong,
        }
        // The asm-generic numbering, which x86_64 and aarch64 Linux use.
        const RLIMIT_DATA: c_int = 2;
        const RLIMIT_CORE: c_int = 4;
        unsafe extern "C" {
            fn setrlimit(resource: c_int, rlim: *const Rlimit) -> c_int;
        }
        let set = |resource, n: usize| {
            let n = c_ulong::try_from(n).unwrap_or(c_ulong::MAX);
            let r = Rlimit { cur: n, max: n };
            // SAFETY: `r` is a valid `struct rlimit` for the call.
            unsafe { setrlimit(resource, &r) };
        };
        set(RLIMIT_CORE, 0);
        set(RLIMIT_DATA, MEMORY_LIMIT);
    }
}

/// When the request being answered started (ms since `base`; 0: none).
#[derive(Default)]
pub struct Busy {
    since: AtomicU64,
    base: std::sync::OnceLock<Instant>,
}

impl Busy {
    fn now(&self) -> u64 {
        let base = self.base.get_or_init(Instant::now);
        // Never 0, which means idle.
        base.elapsed().as_millis() as u64 + 1
    }

    fn start(&self) {
        self.since.store(self.now(), Ordering::SeqCst);
    }

    fn stop(&self) {
        self.since.store(0, Ordering::SeqCst);
    }

    /// How long the current request has run.
    fn running(&self) -> Option<Duration> {
        let since = self.since.load(Ordering::SeqCst);
        (since != 0).then(|| Duration::from_millis(self.now().saturating_sub(since)))
    }
}

/// End the process when one request runs past `limit`: a child spinning
/// in a page whose parent no longer waits never stops otherwise.
fn watchdog(busy: Arc<Busy>, limit: Duration) {
    let step = (limit / 8).clamp(Duration::from_millis(10), Duration::from_millis(500));
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(step);
            if busy.running().is_some_and(|ran| ran > limit) {
                std::process::exit(EXIT_WATCHDOG);
            }
        }
    });
}
