//! The PDF view's worker (#2245): every byte of a PDF is parsed, drawn and
//! read for text in a child process, `docxy --pdf-worker <file> [<ms>]`,
//! handled at the very top of `run` before any window, as the converting
//! child is (`convert_child`). A file that crashes, aborts, overflows the
//! stack or eats memory there costs a page placeholder, never the suite.
//!
//! The protocol is a line of JSON per request on the child's stdin and a
//! line of JSON per answer on its stdout, a raster's pixels following their
//! line as `len` raw bytes. The child first writes a hello line; the parent
//! skips anything before it (a test binary run as the child prints its own
//! lines first). The child reads its file once and keeps one
//! `PageRenderer`; it ends at the end of its input (the suite closed it, or
//! died), or by its own watchdog when one request runs past twice its
//! timeout (a spin the suite can no longer kill).
//!
//! The parent's [`Worker`] asks one question at a time and waits at most
//! its timeout; past it, or when the child dies or answers nonsense, the
//! child is killed and the next question starts a new one. [`Engine`] runs a
//! worker on a thread of its own, so the UI thread never waits on a page.
//! Unit tests and `DOCXY_CONVERT_IN_PROCESS=1` serve in this process over a
//! pipe pair ([`Spawn::InProcess`]): the same [`serve`] code, no isolation.

use pdfcraft_render::{PageText, TextGlyph};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The hidden flag that makes the suite a PDF worker.
pub(crate) const FLAG: &str = "--pdf-worker";
/// The longest one request may take: the conversion's bound.
pub(crate) const TIMEOUT: Duration = crate::convert_child::TIMEOUT;
/// What a page the worker did not draw in time says.
pub(crate) const TOO_LONG: &str = "it took too long to draw";
/// What a page says when the worker died on it, or answered nonsense.
pub(crate) const DIED: &str = "the PDF reader stopped on it";
/// The child's exit code when its watchdog ends it.
const EXIT_WATCHDOG: i32 = 4;
/// The child's exit code on a bad command line.
const EXIT_USAGE: i32 = 2;
/// The first line a worker writes.
const HELLO: &str = "docxy-pdf-worker";
/// The protocol's revision, in the hello line.
const PROTOCOL: u32 = 1;
/// The longest answer line read (a page's text as JSON).
const MAX_LINE: u64 = 256 << 20;
/// The most pixel bytes one answer may carry (8192 × 8192 BGRA).
const MAX_PIXEL_BYTES: usize = 8192 * 8192 * 4;
/// The most bookmarks the outline keeps.
const MAX_BOOKMARKS: usize = 10_000;

/// What the worker is asked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub(crate) enum Op {
    /// The pages (size, rotation, label), the outline and the document's
    /// title and author.
    Info,
    /// Page `page` (0-based) drawn at `scale` device pixels per point.
    Pixels { page: usize, scale: f32 },
    /// Page `page`'s text in reading order.
    Text { page: usize },
}

#[derive(Serialize, Deserialize)]
struct Request {
    id: u64,
    #[serde(flatten)]
    op: Op,
}

/// One page as `inspect` reads it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PageMeta {
    /// Displayed size in points, after `/Rotate` and `/UserUnit`.
    pub w: f32,
    pub h: f32,
    /// Clockwise, in degrees.
    pub rotation: u16,
    pub label: String,
}

/// One bookmark of the outline, flattened in reading order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Bookmark {
    pub title: String,
    /// 0-based; `None` for a bookmark that goes to no page here.
    pub page: Option<usize>,
    /// 0 for a top-level bookmark.
    pub depth: usize,
}

/// What `info` answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct DocMeta {
    pub pages: Vec<PageMeta>,
    pub outline: Vec<Bookmark>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub version: String,
}

/// A glyph on the wire: [`TextGlyph`] with short names.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct GlyphWire {
    t: String,
    r: [f32; 4],
    q: [[f32; 2]; 4],
    d: [f32; 2],
}

/// [`PageText`] on the wire.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct TextWire {
    glyphs: Vec<GlyphWire>,
    line_of: Vec<u32>,
    space_before: Vec<bool>,
    truncated: bool,
}

/// An answer's line.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Head {
    id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    info: Option<DocMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<TextWire>,
    /// A raster's size; `len` pixel bytes follow the line.
    #[serde(default)]
    w: u32,
    #[serde(default)]
    h: u32,
    #[serde(default)]
    len: usize,
}

#[derive(Serialize, Deserialize)]
struct Hello {
    hello: String,
    protocol: u32,
}

/// A page drawn: straight-alpha BGRA rows, as gpui's `RenderImage` takes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Raster {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// What the worker answered.
#[derive(Clone, Debug)]
pub(crate) enum Answer {
    Info(DocMeta),
    Pixels(Raster),
    Text(Arc<PageText>),
    /// Why it could not: the file's fault, or the child's fate.
    Failed(String),
}

// ---- the child --------------------------------------------------------------

/// The child's side: when the command line asks for a worker, serve and
/// return the exit code. Called first thing in `run`.
pub(crate) fn child_main(args: &[OsString]) -> Option<i32> {
    if args.get(1).and_then(|a| a.to_str()) != Some(FLAG) {
        return None;
    }
    let Some(path) = args.get(2) else {
        eprintln!("docxy: {FLAG} <file.pdf> [<timeout ms>]");
        return Some(EXIT_USAGE);
    };
    let timeout = args
        .get(3)
        .and_then(|a| a.to_str())
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(TIMEOUT, Duration::from_millis);
    Some(run(Path::new(path), timeout, |_| {}))
}

/// Serve `path` on this process's stdin and stdout, under a watchdog that
/// ends the process when one request runs past twice `timeout`. `before`
/// sees each request first (the tests' misbehaving children).
pub(crate) fn run(path: &Path, timeout: Duration, before: impl FnMut(&Op)) -> i32 {
    first_to_go();
    let bytes = std::fs::read(path)
        .map(Arc::new)
        .map_err(|e| format!("the file could not be read: {e}"));
    let busy = Arc::new(Busy::default());
    watchdog(busy.clone(), timeout.saturating_mul(2));
    let stdin = std::io::stdin().lock();
    let stdout = std::io::BufWriter::new(std::io::stdout().lock());
    serve(bytes, stdin, stdout, Some(&busy), before);
    0
}

/// On Linux, the child asks to be the first process the OOM killer takes,
/// before the suite that started it. Nothing to do elsewhere: Windows runs
/// it in a memory-limited job, and macOS has no such knob.
fn first_to_go() {
    #[cfg(target_os = "linux")]
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
}

/// When the request being answered started (ms since `base`, 0: none).
#[derive(Default)]
pub(crate) struct Busy {
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

/// End the process when one request runs past `limit`: a child spinning in
/// a page whose parent is gone, or no longer waits, never stops otherwise.
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

/// Answer requests from `input` on `output` until `input` ends: the hello
/// line first, then one answer per request. A file that could not be read
/// answers every request with why.
pub(crate) fn serve(
    bytes: Result<Arc<Vec<u8>>, String>,
    mut input: impl BufRead,
    mut output: impl Write,
    busy: Option<&Busy>,
    mut before: impl FnMut(&Op),
) {
    let hello = Hello {
        hello: HELLO.into(),
        protocol: PROTOCOL,
    };
    if write_line(&mut output, &hello).is_err() || output.flush().is_err() {
        return;
    }
    let mut renderer: Option<pdfcraft_render::PageRenderer> = None;
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let (head, body) = match serde_json::from_str::<Request>(line.trim()) {
            Ok(req) => {
                if let Some(b) = busy {
                    b.start();
                }
                before(&req.op);
                let answered = match &bytes {
                    Ok(bytes) => answer(bytes, &mut renderer, req.id, &req.op),
                    Err(why) => (failed(req.id, why.clone()), Vec::new()),
                };
                if let Some(b) = busy {
                    b.stop();
                }
                answered
            }
            Err(e) => (
                failed(0, format!("a request the reader does not know: {e}")),
                Vec::new(),
            ),
        };
        if write_line(&mut output, &head).is_err()
            || output.write_all(&body).is_err()
            || output.flush().is_err()
        {
            return;
        }
    }
}

fn write_line(out: &mut impl Write, value: &impl Serialize) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    out.write_all(b"\n")
}

fn failed(id: u64, why: String) -> Head {
    Head {
        id,
        error: Some(why),
        ..Head::default()
    }
}

/// One request's answer, in this process.
fn answer(
    bytes: &Arc<Vec<u8>>,
    renderer: &mut Option<pdfcraft_render::PageRenderer>,
    id: u64,
    op: &Op,
) -> (Head, Vec<u8>) {
    use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};
    let mut renderer = |req: RenderRequest| {
        renderer
            .get_or_insert_with(|| PageRenderer::new(bytes.clone(), RenderConfig::default()))
            .render(req)
    };
    match *op {
        Op::Info => match pdfcraft_render::inspect(bytes.clone(), None) {
            Ok(info) if info.pages.is_empty() => {
                (failed(id, "the PDF has no pages".into()), vec![])
            }
            Ok(info) => (
                Head {
                    id,
                    info: Some(doc_meta(&info)),
                    ..Head::default()
                },
                vec![],
            ),
            Err(e) => (failed(id, e.to_string()), vec![]),
        },
        Op::Pixels { page, scale } => {
            let scale = if scale.is_finite() && scale > 0. {
                scale
            } else {
                1.
            };
            let r = renderer(RenderRequest {
                page,
                scale,
                ..RenderRequest::default()
            });
            if let Some(e) = r.error {
                return (failed(id, e), vec![]);
            }
            let bgra = straight_bgra(&r.rgba);
            if bgra.len() != r.width as usize * r.height as usize * 4 || bgra.is_empty() {
                return (failed(id, "the page drew nothing".into()), vec![]);
            }
            (
                Head {
                    id,
                    w: r.width,
                    h: r.height,
                    len: bgra.len(),
                    ..Head::default()
                },
                bgra,
            )
        }
        Op::Text { page } => {
            let r = renderer(RenderRequest {
                page,
                kind: RequestKind::Text,
                ..RenderRequest::default()
            });
            match (r.error, r.text) {
                (Some(e), _) => (failed(id, e), vec![]),
                (None, Some(text)) => (
                    Head {
                        id,
                        text: Some(text_wire(&text)),
                        ..Head::default()
                    },
                    vec![],
                ),
                (None, None) => (failed(id, "the page has no text layer".into()), vec![]),
            }
        }
    }
}

/// `inspect`'s answer as the view needs it.
fn doc_meta(info: &pdfcraft_render::DocInfo) -> DocMeta {
    let mut outline = Vec::new();
    flatten(&info.outline, 0, &mut outline);
    DocMeta {
        pages: info
            .pages
            .iter()
            .map(|p| PageMeta {
                w: finite(p.width).max(1.),
                h: finite(p.height).max(1.),
                rotation: p.rotation,
                label: p.label.clone(),
            })
            .collect(),
        outline,
        title: info.title.clone().filter(|t| !t.trim().is_empty()),
        author: info.author.clone().filter(|t| !t.trim().is_empty()),
        version: info.pdf_version.clone(),
    }
}

fn flatten(items: &[pdfcraft_render::OutlineItem], depth: usize, out: &mut Vec<Bookmark>) {
    for item in items {
        if out.len() >= MAX_BOOKMARKS {
            return;
        }
        out.push(Bookmark {
            title: item.title.clone(),
            page: item.page,
            depth,
        });
        flatten(&item.children, depth + 1, out);
    }
}

/// Premultiplied RGBA (the renderer's) as straight BGRA (gpui's images).
/// Pages are drawn over white, so alpha is 255 and only red and blue swap;
/// a pixel with less is un-premultiplied too.
pub(crate) fn straight_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        let [r, g, b, a] = [p[0], p[1], p[2], p[3]];
        let un = |c: u8| match a {
            255 => c,
            0 => 0,
            a => ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8,
        };
        out.extend_from_slice(&[un(b), un(g), un(r), a]);
    }
    out
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0. }
}

/// JSON has no NaN: a broken box is sent as zeros.
fn text_wire(t: &PageText) -> TextWire {
    let f4 = |r: [f32; 4]| r.map(finite);
    TextWire {
        glyphs: t
            .glyphs
            .iter()
            .map(|g| GlyphWire {
                t: g.text.clone(),
                r: f4(g.rect),
                q: g.quad.map(|p| p.map(finite)),
                d: g.direction.map(finite),
            })
            .collect(),
        line_of: t.line_of.clone(),
        space_before: t.space_before.clone(),
        truncated: t.truncated,
    }
}

fn page_text(w: TextWire) -> Result<PageText, String> {
    let n = w.glyphs.len();
    if w.line_of.len() != n || w.space_before.len() != n {
        return Err(DIED.into());
    }
    Ok(PageText {
        glyphs: w
            .glyphs
            .into_iter()
            .map(|g| TextGlyph {
                text: g.t,
                rect: g.r,
                quad: g.q,
                direction: g.d,
            })
            .collect(),
        line_of: w.line_of,
        space_before: w.space_before,
        truncated: w.truncated,
    })
}

// ---- the parent ---------------------------------------------------------------

/// How a worker is started.
#[derive(Clone)]
pub(crate) enum Spawn {
    /// A copy of the suite, `<exe> --pdf-worker <file> <ms>`.
    Child,
    /// [`serve`] on a thread of this process (unit tests, and
    /// `DOCXY_CONVERT_IN_PROCESS=1`): no isolation, the same protocol.
    InProcess,
    /// Any command that ends up serving (the tests' child: the test binary).
    #[cfg_attr(not(test), allow(dead_code))]
    Command(Arc<dyn Fn() -> Command + Send + Sync>),
}

impl Spawn {
    /// The spawn this process uses: in process for unit tests and
    /// `DOCXY_CONVERT_IN_PROCESS=1`, else a child.
    pub(crate) fn default_for_process() -> Spawn {
        if crate::convert_child::in_process() {
            Spawn::InProcess
        } else {
            Spawn::Child
        }
    }
}

type Reply = Result<(Head, Vec<u8>), String>;

/// A live worker: its request pipe and the thread reading its answers.
struct Conn {
    input: Box<dyn Write + Send>,
    replies: mpsc::Receiver<Reply>,
}

/// The child a [`Worker`] runs, shared so an [`Engine`] going away can kill
/// it while its thread still waits on it.
type Slot = Arc<Mutex<Option<crate::convert_child::Limited>>>;

/// The parent's side of one PDF: asks one question at a time.
pub(crate) struct Worker {
    path: PathBuf,
    spawn: Spawn,
    timeout: Duration,
    conn: Option<Conn>,
    slot: Slot,
    next_id: u64,
    /// Children started so far (the tests' respawn check).
    pub(crate) started: usize,
}

impl Worker {
    pub(crate) fn new(path: &Path, spawn: Spawn, timeout: Duration) -> Worker {
        Worker {
            path: path.to_path_buf(),
            spawn,
            timeout,
            conn: None,
            slot: Arc::default(),
            next_id: 0,
            started: 0,
        }
    }

    /// Ask `op` and wait for its answer, at most the timeout. A child that
    /// runs out of time, dies or answers nonsense is killed: the answer says
    /// why, and the next question starts a new one.
    pub(crate) fn ask(&mut self, op: &Op) -> Answer {
        match self.ask_raw(op) {
            Ok((head, body)) => decode(op, head, body),
            Err(why) => Answer::Failed(why),
        }
    }

    fn ask_raw(&mut self, op: &Op) -> Reply {
        if self.conn.is_none() {
            let conn = self
                .start()
                .map_err(|e| format!("the PDF reader could not start: {e}"))?;
            self.conn = Some(conn);
            self.started += 1;
        }
        self.next_id += 1;
        let id = self.next_id;
        let deadline = Instant::now() + self.timeout;
        let conn = self.conn.as_mut().expect("started above");
        let line =
            serde_json::to_string(&Request { id, op: op.clone() }).map_err(|e| e.to_string())?;
        if writeln!(conn.input, "{line}")
            .and_then(|()| conn.input.flush())
            .is_err()
        {
            self.stop();
            return Err(DIED.into());
        }
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.conn.as_ref().expect("live").replies.recv_timeout(left) {
                Ok(Ok((head, body))) if head.id == id => return Ok((head, body)),
                // An answer to a question given up on: not this one.
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return Err(DIED.into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.stop();
                    return Err(TOO_LONG.into());
                }
            }
        }
    }

    /// Kill the child (if any) and forget the connection.
    fn stop(&mut self) {
        self.conn = None;
        kill(&self.slot);
    }

    fn start(&mut self) -> std::io::Result<Conn> {
        match &self.spawn {
            Spawn::InProcess => {
                let (req_r, req_w) = std::io::pipe()?;
                let (rep_r, rep_w) = std::io::pipe()?;
                let bytes = std::fs::read(&self.path)
                    .map(Arc::new)
                    .map_err(|e| format!("the file could not be read: {e}"));
                std::thread::Builder::new()
                    .name("pdf-serve".into())
                    .spawn(move || {
                        serve(
                            bytes,
                            BufReader::new(req_r),
                            std::io::BufWriter::new(rep_w),
                            None,
                            |_| {},
                        )
                    })?;
                Ok(Conn::new(Box::new(req_w), Box::new(rep_r)))
            }
            Spawn::Child => {
                let mut cmd = Command::new(std::env::current_exe()?);
                cmd.arg(FLAG)
                    .arg(&self.path)
                    .arg(self.timeout.as_millis().to_string());
                self.start_child(cmd)
            }
            Spawn::Command(make) => {
                let cmd = make();
                self.start_child(cmd)
            }
        }
    }

    fn start_child(&self, mut cmd: Command) -> std::io::Result<Conn> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut limited =
            crate::convert_child::spawn_limited(cmd, crate::convert_child::MEMORY_LIMIT)?;
        let (Some(input), Some(output)) = (limited.child.stdin.take(), limited.child.stdout.take())
        else {
            let _ = limited.child.kill();
            return Err(std::io::Error::other("no pipes to the child"));
        };
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(limited);
        Ok(Conn::new(Box::new(input), Box::new(output)))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn kill(slot: &Slot) {
    if let Some(mut limited) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = limited.child.kill();
        let _ = limited.child.wait();
    }
}

impl Conn {
    fn new(input: Box<dyn Write + Send>, output: Box<dyn Read + Send>) -> Conn {
        let (tx, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("pdf-replies".into())
            .spawn(move || read_replies(BufReader::new(output), tx))
            .expect("a thread for the PDF reader's answers");
        Conn { input, replies }
    }
}

/// The answer reader: skips to the hello line, then sends each answer (its
/// pixels read too) until the output ends or makes no sense.
fn read_replies(mut out: impl BufRead, tx: mpsc::Sender<Reply>) {
    let mut line = String::new();
    let mut greeted = false;
    loop {
        line.clear();
        let read = (&mut out).take(MAX_LINE).read_line(&mut line);
        match read {
            Ok(0) | Err(_) => {
                let _ = tx.send(Err(DIED.into()));
                return;
            }
            Ok(_) => {}
        }
        // Anything before the hello is not the worker's (a test binary run
        // as the child prints its own words, maybe on the hello's line).
        if !greeted {
            greeted = line.find("{\"hello\"").is_some_and(|at| {
                serde_json::from_str::<Hello>(line[at..].trim())
                    .is_ok_and(|h| h.hello == HELLO && h.protocol == PROTOCOL)
            });
            continue;
        }
        let reply = match serde_json::from_str::<Head>(line.trim()) {
            Ok(head) if head.len == 0 => Ok((head, Vec::new())),
            Ok(head)
                if head.len <= MAX_PIXEL_BYTES
                    && head.len == head.w as usize * head.h as usize * 4 =>
            {
                let mut body = vec![0; head.len];
                match out.read_exact(&mut body) {
                    Ok(()) => Ok((head, body)),
                    Err(_) => Err(DIED.to_string()),
                }
            }
            _ => Err(DIED.to_string()),
        };
        let bad = reply.is_err();
        if tx.send(reply).is_err() || bad {
            return;
        }
    }
}

/// An answer's line and bytes as what `op` asked for.
fn decode(op: &Op, head: Head, body: Vec<u8>) -> Answer {
    if let Some(why) = head.error {
        return Answer::Failed(why);
    }
    match op {
        Op::Info => head
            .info
            .map_or_else(|| Answer::Failed(DIED.into()), Answer::Info),
        Op::Pixels { .. } if head.len > 0 => Answer::Pixels(Raster {
            w: head.w,
            h: head.h,
            bgra: body,
        }),
        Op::Pixels { .. } => Answer::Failed(DIED.into()),
        Op::Text { .. } => match head.text.map(page_text) {
            Some(Ok(t)) => Answer::Text(Arc::new(t)),
            Some(Err(why)) => Answer::Failed(why),
            None => Answer::Failed(DIED.into()),
        },
    }
}

// ---- the engine ---------------------------------------------------------------

/// A question answered, as the [`Engine`] hands it back.
#[derive(Clone, Debug)]
pub(crate) struct Done {
    pub op: Op,
    pub answer: Answer,
}

/// A [`Worker`] on a thread of its own: the view sends a question and
/// collects the answers when they are in. One question is out at a time
/// (the view's scheduler sends the next one when the last is answered).
pub(crate) struct Engine {
    jobs: Option<mpsc::Sender<Op>>,
    inbox: Arc<Mutex<VecDeque<Done>>>,
    slot: Slot,
}

impl Engine {
    pub(crate) fn new(worker: Worker) -> Engine {
        let (jobs, rx) = mpsc::channel::<Op>();
        let inbox: Arc<Mutex<VecDeque<Done>>> = Arc::default();
        let slot = worker.slot.clone();
        let theirs = inbox.clone();
        let spawned = std::thread::Builder::new()
            .name("pdf-worker".into())
            .spawn(move || {
                let mut worker = worker;
                for op in rx {
                    let answer = worker.ask(&op);
                    theirs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(Done { op, answer });
                }
            });
        Engine {
            jobs: spawned.is_ok().then_some(jobs),
            inbox,
            slot,
        }
    }

    /// Hand `op` to the worker's thread. `false` when the thread is gone.
    pub(crate) fn send(&self, op: Op) -> bool {
        self.jobs.as_ref().is_some_and(|j| j.send(op).is_ok())
    }

    /// The answers in since the last call, oldest first.
    pub(crate) fn take(&self) -> Vec<Done> {
        self.inbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    /// Whether an answer waits to be taken.
    pub(crate) fn has_answers(&self) -> bool {
        !self
            .inbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // The thread ends when its queue closes; a child busy on a page is
        // killed now rather than when its timeout runs out.
        self.jobs = None;
        kill(&self.slot);
    }
}

#[cfg(test)]
pub(crate) mod tests;
