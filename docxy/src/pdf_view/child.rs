//! The PDF viewer's sources of pages (#2259): `docxy-pdf`, the child that
//! draws and reads them with the desktop suite's renderer (suite/pdfpages),
//! and the text importer when there is no child.
//!
//! The child speaks protocol 1 (`suite/pdfpages/src/child.rs`): a hello
//! line, then one JSON line per request and per answer, a raster's RGBA
//! after its line. One question at a time, each waited for at most
//! [`TIMEOUT`]; past it, or when the child dies or answers nonsense, it is
//! killed, the question answers why, and the next question starts another.

use ctlcore::json::Json;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// How long one question may take: the suite's (`convert_child::TIMEOUT`).
pub(crate) const TIMEOUT: Duration = Duration::from_secs(20);
/// What a question past its timeout answers.
pub(crate) const TOO_LONG: &str = "it took too long to draw";
/// What a question answers when the child died on it or answered nonsense.
pub(crate) const DIED: &str = "the PDF reader stopped on it";
/// The child's file name.
pub(crate) const EXE: &str = "docxy-pdf";
/// The hello line's words.
const HELLO: &str = "docxy-pdf";
const PROTOCOL: f64 = 1.;
/// The longest answer line read (a page's text as JSON).
const MAX_LINE: u64 = 64 << 20;
/// The most pixel bytes one answer may carry (8192 x 8192 RGBA).
const MAX_PIXEL_BYTES: usize = 8192 * 8192 * 4;

/// A question for a source.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Req {
    Info,
    Pixels { page: usize, scale: f32 },
    Text { page: usize },
}

/// One page: its displayed size in points and its label.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PageMeta {
    pub w: f32,
    pub h: f32,
    pub label: String,
}

/// A document's pages and outline.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Meta {
    pub pages: Vec<PageMeta>,
    /// (title, page, depth), in outline order.
    pub outline: Vec<(String, Option<usize>, usize)>,
}

/// A page's straight RGBA.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An answer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ans {
    Info(Meta),
    /// The file needs a password.
    Locked,
    Pixels(Raster),
    Text(String),
    Failed(String),
    /// The child could not be started at all (not a failure of the file).
    NoChild(String),
}

/// Something that answers questions about one PDF.
pub(crate) trait Source: Send {
    fn ask(&mut self, req: &Req) -> Ans;

    /// What ends the source's child process from another thread, if it has
    /// one.
    fn killer(&self) -> Option<Killer> {
        None
    }
}

/// The child process of a [`Child`], shared so the viewer's loop can end it
/// while the engine thread waits on it (a quit during a slow page).
#[derive(Clone, Default)]
pub(crate) struct Killer(Arc<Mutex<Option<std::process::Child>>>);

impl Killer {
    fn put(&self, child: std::process::Child) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    }

    /// End the running process, if there is one, and reap it.
    pub(crate) fn kill(&self) {
        let taken = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = taken {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The running process's id (the tests').
    #[cfg(test)]
    pub(crate) fn pid(&self) -> Option<u32> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(std::process::Child::id)
    }
}

// ---- finding the child ---------------------------------------------------------

/// Where `docxy-pdf` is: `$DOCXY_PDF` when set (whether or not a file is
/// there, so the status can name it), else next to this executable, else
/// on `PATH`.
pub(crate) fn find_child() -> Result<PathBuf, String> {
    find_child_in(
        std::env::var_os("DOCXY_PDF"),
        std::env::current_exe().ok(),
        std::env::var_os("PATH"),
    )
}

pub(crate) fn find_child_in(
    var: Option<std::ffi::OsString>,
    exe: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    let name = format!("{EXE}{}", std::env::consts::EXE_SUFFIX);
    if let Some(var) = var.filter(|v| !v.is_empty()) {
        let p = PathBuf::from(var);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "DOCXY_PDF names {}, which is not there",
                p.display()
            ))
        };
    }
    let beside = exe
        .as_deref()
        .and_then(Path::parent)
        .map(|d| d.join(&name))
        .filter(|p| p.is_file());
    if let Some(p) = beside {
        return Ok(p);
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(&name);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    Err(format!("{EXE} was not found"))
}

// ---- the child -------------------------------------------------------------------

/// What the child wrote: a JSON line and the bytes after it, or a line
/// that is no JSON.
enum Said {
    Json(Json, Vec<u8>),
    Other,
}

/// A running child: its stdin, its output read on a thread of its own so a
/// wait can time out, and its process (in the shared [`Killer`]).
struct Running {
    stdin: ChildStdin,
    said: mpsc::Receiver<Said>,
    process: Killer,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.process.kill();
    }
}

/// `docxy-pdf` serving one file, started again after each failure.
pub(crate) struct Child {
    exe: PathBuf,
    args: Vec<std::ffi::OsString>,
    file: PathBuf,
    timeout: Duration,
    running: Option<Running>,
    process: Killer,
}

impl Child {
    pub(crate) fn new(exe: PathBuf, file: PathBuf) -> Child {
        Child::with(exe, Vec::new(), file, TIMEOUT)
    }

    /// `exe` run with `args` before the file (the tests' fake child) and
    /// `timeout` a question.
    pub(crate) fn with(
        exe: PathBuf,
        args: Vec<std::ffi::OsString>,
        file: PathBuf,
        timeout: Duration,
    ) -> Child {
        Child {
            exe,
            args,
            file,
            timeout,
            running: None,
            process: Killer::default(),
        }
    }

    /// The running child, started (and its hello read) when there is none.
    fn connect(&mut self) -> Result<&mut Running, Ans> {
        if self.running.is_none() {
            self.running = Some(self.spawn()?);
        }
        Ok(self.running.as_mut().expect("started above"))
    }

    fn spawn(&self) -> Result<Running, Ans> {
        let mut child = Command::new(&self.exe)
            .args(&self.args)
            .arg(&self.file)
            .arg(self.timeout.as_millis().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Anything it prints there would be drawn over the viewer.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                Ans::NoChild(format!("{} could not be started: {e}", self.exe.display()))
            })?;
        let died = || Ans::Failed(DIED.into());
        let stdin = child.stdin.take().ok_or_else(died)?;
        let stdout = child.stdout.take().ok_or_else(died)?;
        self.process.put(child);
        let (tx, said) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("docxy-pdf-reader".into())
            .spawn(move || read_answers(BufReader::new(stdout), tx));
        let running = Running {
            stdin,
            said,
            process: self.process.clone(),
        };
        // Anything before the hello is skipped (a test binary run as the
        // child prints its own lines first), all within one timeout: a child
        // that chatters and never says hello is not waited on forever.
        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match running.said.recv_timeout(left) {
                Ok(Said::Json(j, _)) if is_hello(&j) => return Ok(running),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(Ans::Failed(TOO_LONG.into())),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(died()),
            }
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Ends the child: `Running`'s drop kills it.
        self.running = None;
        self.process.kill();
    }
}

fn is_hello(j: &Json) -> bool {
    j.get_str("hello") == Some(HELLO) && j.get("protocol").and_then(Json::as_f64) == Some(PROTOCOL)
}

/// Read the child's lines (and the bytes after each) until its output ends.
fn read_answers(mut out: impl BufRead, tx: mpsc::Sender<Said>) {
    loop {
        let mut line = String::new();
        match out.by_ref().take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            // Past MAX_LINE, or cut short: nothing after it can be trusted.
            let _ = tx.send(Said::Other);
            return;
        }
        let said = match Json::parse(line.trim()) {
            Ok(j) => {
                let len = j.get_usize("len").unwrap_or(0);
                if len > MAX_PIXEL_BYTES {
                    let _ = tx.send(Said::Other);
                    return;
                }
                let mut body = vec![0; len];
                if out.read_exact(&mut body).is_err() {
                    return;
                }
                Said::Json(j, body)
            }
            Err(_) => Said::Other,
        };
        if tx.send(said).is_err() {
            return;
        }
    }
}

impl Source for Child {
    fn killer(&self) -> Option<Killer> {
        Some(self.process.clone())
    }

    fn ask(&mut self, req: &Req) -> Ans {
        let timeout = self.timeout;
        let running = match self.connect() {
            Ok(r) => r,
            Err(ans) => {
                self.running = None;
                return ans;
            }
        };
        let line = match req {
            Req::Info => r#"{"op":"info"}"#.to_string(),
            Req::Pixels { page, scale } => {
                let scale = if scale.is_finite() { *scale } else { 1. };
                format!(r#"{{"op":"pixels","page":{page},"scale":{scale}}}"#)
            }
            Req::Text { page } => format!(r#"{{"op":"text","page":{page}}}"#),
        };
        let sent = writeln!(running.stdin, "{line}").and_then(|()| running.stdin.flush());
        let answer = match sent {
            Err(_) => Err(DIED.to_string()),
            Ok(()) => match running.said.recv_timeout(timeout) {
                Ok(Said::Json(j, body)) => decode(req, &j, body).ok_or_else(|| DIED.to_string()),
                Ok(Said::Other) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(DIED.to_string())
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Err(TOO_LONG.to_string()),
            },
        };
        match answer {
            Ok(a) => a,
            Err(why) => {
                // Killed; the next question starts another.
                self.running = None;
                Ans::Failed(why)
            }
        }
    }
}

/// The answer to `req` in `j` (and `body`); `None` for nonsense.
fn decode(req: &Req, j: &Json, body: Vec<u8>) -> Option<Ans> {
    match j.get("ok").and_then(Json::as_bool)? {
        false => {
            if matches!(req, Req::Info) && j.get("locked").and_then(Json::as_bool) == Some(true) {
                return Some(Ans::Locked);
            }
            let why = j.get_str("error").filter(|e| !e.is_empty())?;
            Some(Ans::Failed(why.to_string()))
        }
        true => match req {
            Req::Info => {
                let pages = j
                    .get("pages")?
                    .as_array()?
                    .iter()
                    .map(|p| {
                        Some(PageMeta {
                            w: p.get("w")?.as_f64()? as f32,
                            h: p.get("h")?.as_f64()? as f32,
                            label: p.get_str("label").unwrap_or_default().to_string(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                if pages.is_empty() {
                    return None;
                }
                let outline = j
                    .get("outline")
                    .and_then(Json::as_array)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|b| {
                        Some((
                            b.get_str("title")?.to_string(),
                            b.get("page").and_then(Json::as_usize),
                            b.get_usize("depth").unwrap_or(0),
                        ))
                    })
                    .collect();
                Some(Ans::Info(Meta { pages, outline }))
            }
            Req::Text { .. } => Some(Ans::Text(j.get_str("text")?.to_string())),
            Req::Pixels { .. } => {
                let width = u32::try_from(j.get_usize("width")?).ok()?;
                let height = u32::try_from(j.get_usize("height")?).ok()?;
                if width == 0 || body.len() != width as usize * height as usize * 4 {
                    return None;
                }
                Some(Ans::Pixels(Raster {
                    width,
                    height,
                    rgba: body,
                }))
            }
        },
    }
}

// ---- the text importer -------------------------------------------------------------

/// No child: docxcore's text importer (#951) reads the file's text, shown
/// as one stream.
pub(crate) struct TextOnly {
    pub file: PathBuf,
    text: Option<Result<String, String>>,
}

impl TextOnly {
    pub(crate) fn new(file: PathBuf) -> TextOnly {
        TextOnly { file, text: None }
    }

    fn read(&mut self) -> Result<String, String> {
        if self.text.is_none() {
            let read = std::fs::read(&self.file)
                .map_err(|e| format!("the file could not be read: {e}"))
                .and_then(|bytes| docxcore::import::import_pdf(&bytes))
                .map(|doc| doc.plain_text());
            self.text = Some(read);
        }
        self.text.clone().expect("read above")
    }
}

impl Source for TextOnly {
    fn ask(&mut self, req: &Req) -> Ans {
        match (req, self.read()) {
            (_, Err(why)) => Ans::Failed(why),
            (Req::Info, Ok(_)) => Ans::Info(Meta {
                pages: vec![PageMeta {
                    w: 0.,
                    h: 0.,
                    label: "1".into(),
                }],
                outline: Vec::new(),
            }),
            (Req::Text { page: 0 }, Ok(text)) => Ans::Text(text),
            _ => Ans::Failed("the text importer has no pages".into()),
        }
    }
}
