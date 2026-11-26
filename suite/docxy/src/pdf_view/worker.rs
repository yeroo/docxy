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
/// The protocol's revision, in the hello line: 2 adds links, notes and the
/// form to `info`, and the form's operations (#2255); 3 the password
/// (`unlock`), a locked file's answer and the file's security (#2256).
const PROTOCOL: u32 = 3;
/// The longest answer line read (a page's text as JSON).
const MAX_LINE: u64 = 256 << 20;
/// The most pixel bytes one answer may carry (8192 × 8192 BGRA).
const MAX_PIXEL_BYTES: usize = 8192 * 8192 * 4;
/// The most bookmarks the outline keeps.
const MAX_BOOKMARKS: usize = 10_000;
/// The largest file a save hands over (bytes after its line).
const MAX_SAVE_BYTES: usize = 1 << 31;
/// What a save onto a file another program changed says (#2255).
pub(crate) const CHANGED: &str = "the file was changed by another program since it was opened; \
     use Save As to keep your values in another file";
/// What a view says when the unsaved form values could not be set again in
/// a new reader (#2255).
pub(crate) const VALUES_LOST: &str = "the PDF reader stopped and the form values \
     not yet saved could not be restored";

/// A PDF's password (#2256): never printed (`Debug` shows `Secret(..)`),
/// cleared from memory when dropped. It goes to the child on its stdin
/// only, never on its command line or in its environment.
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct Secret(zeroize::Zeroizing<String>);

impl Secret {
    pub(crate) fn new(password: String) -> Secret {
        Secret(zeroize::Zeroizing::new(password))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        String::deserialize(d).map(Secret::new)
    }
}

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
    /// Fill the form field `name` (#2255): the answer is the form after it,
    /// and the pages whose fields show something else now.
    SetField { name: String, value: FieldValue },
    /// The fills of [`Op::SetField`]s again, at once, in a new child: the
    /// [`Worker`] sends it, never the view.
    SetFields { values: Vec<(String, FieldValue)> },
    /// The form's fields as they are now.
    Fields,
    /// The child: the bytes a save writes (an incremental update when it
    /// can be one), after their line. The [`Worker`] sends it.
    Save,
    /// The child: the bytes the last `save` gave were written; take them as
    /// the file. The [`Worker`] sends it.
    Saved,
    /// The parent: save to `path` (the file itself, or Save As). The
    /// [`Worker`] asks the child for the bytes and writes them; never sent
    /// to the child.
    SaveTo { path: PathBuf },
    /// Open an encrypted file with `password` (#2256), user or owner: the
    /// answer is `info`'s, or locked (`wrong`) when it does not open it.
    /// The [`Worker`] sends it again first to every new child.
    Unlock { password: Secret },
}

impl Op {
    /// The form's operations, which the view sends before any page.
    pub(crate) fn is_form(&self) -> bool {
        matches!(
            self,
            Op::SetField { .. } | Op::SetFields { .. } | Op::Fields | Op::SaveTo { .. }
        )
    }
}

/// A form field's new value on the wire (#2255).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum FieldValue {
    /// A text field's text, or an editable combo box's typed text.
    Text { text: String },
    /// A check box on or off.
    Check { on: bool },
    /// Turn on widget `index` of a check box or radio group.
    Widget { index: usize },
    /// A combo or list box's options, by position in its options.
    Choice { picked: Vec<usize> },
}

/// A link on a page (#2255): where it is in view space (points, y down,
/// after `/Rotate`, as the text layer), and where it goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LinkMeta {
    pub page: usize,
    pub rect: [f32; 4],
    /// The areas it is active over when not the whole of `rect`, each four
    /// corners (`x y` four times).
    #[serde(default)]
    pub quads: Vec<[f32; 8]>,
    pub go: LinkGo,
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum LinkGo {
    /// A web or mail address (the view opens only `http`, `https` and
    /// `mailto`).
    Uri { uri: String },
    /// A place in this document: page `page`, its view-space point (a
    /// coordinate the destination leaves as it is is `None`), the zoom an
    /// `/XYZ` asks for, and whether it fits the page's width (`/FitH`).
    Page {
        page: usize,
        x: Option<f32>,
        y: Option<f32>,
        zoom: Option<f32>,
        fit_width: bool,
    },
    /// What the view does not do (a script, a launch, a layer change, another
    /// file), as the status line names it.
    Other { what: String },
}

/// A comment whose text shows in a pop-up on hover (#2255).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct NoteMeta {
    pub page: usize,
    /// Where it shows on the page, view space.
    pub rect: [f32; 4],
    pub subtype: String,
    pub author: Option<String>,
    /// Its `/M`, as `inspect` writes it out.
    pub date: Option<String>,
    pub contents: String,
}

/// What kind of form field (#2255).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FieldKind {
    Text,
    Check,
    Radio,
    Button,
    Combo,
    List,
    Signature,
}

/// A list box's rows on its widget, as PdfCraft draws them (points).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ListRowsMeta {
    /// The first option shown.
    pub top: usize,
    /// From the widget's top edge to the first row.
    pub inset: f32,
    pub row: f32,
}

/// One widget of a field: where it shows (view space), and for a check box
/// or radio button its on state and whether it is on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WidgetMeta {
    pub page: Option<usize>,
    pub rect: [f32; 4],
    pub on_state: Option<String>,
    pub on: bool,
    pub hidden: bool,
    pub read_only: bool,
    /// Its place in the document's tab order.
    pub tab: usize,
    pub list: Option<ListRowsMeta>,
}

/// A terminal form field (#2255).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FieldMeta {
    pub name: String,
    pub kind: FieldKind,
    /// Text: its text; check box and radio: the on state that is on;
    /// choice: the picked export values.
    pub value: Vec<String>,
    /// Choice: the picked options, by position.
    #[serde(default)]
    pub picked: Vec<usize>,
    /// Choice: (export value, text shown).
    #[serde(default)]
    pub options: Vec<(String, String)>,
    pub read_only: bool,
    pub required: bool,
    pub multiline: bool,
    pub password: bool,
    pub comb: bool,
    /// A combo box that takes typed text.
    pub edit: bool,
    pub multi: bool,
    pub max_len: Option<usize>,
    pub tooltip: Option<String>,
    /// The text size its `/DA` asks for, points (0: fit the widget).
    pub font_size: f32,
    pub widgets: Vec<WidgetMeta>,
}

impl FieldMeta {
    /// Nothing filled in: what a required field's red outline shows.
    pub(crate) fn is_empty(&self) -> bool {
        match self.kind {
            FieldKind::Check | FieldKind::Radio => !self.widgets.iter().any(|w| w.on),
            FieldKind::Combo | FieldKind::List => {
                self.picked.is_empty() && self.value.iter().all(|v| v.is_empty())
            }
            _ => self.value.iter().all(|v| v.is_empty()),
        }
    }

    /// Whether a person can fill it: not read-only, and a kind the view
    /// fills (push buttons and signatures are not).
    pub(crate) fn fillable(&self) -> bool {
        !self.read_only && !matches!(self.kind, FieldKind::Button | FieldKind::Signature)
    }
}

/// The form as a view needs it (#2255).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct FormMeta {
    pub fields: Vec<FieldMeta>,
    /// A signature field holds a signature: saving changes invalidates it.
    pub signed: bool,
    /// A save appends to the file (an incremental update) rather than
    /// writing it anew, as a damaged file is.
    pub incremental: bool,
}

/// What a form operation answers: the form now, and the pages whose
/// fields show something else (to draw again).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct FormState {
    pub fields: Vec<FieldMeta>,
    #[serde(default)]
    pub pages: Vec<usize>,
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
    /// Every page's links, in view space (#2255).
    #[serde(default)]
    pub links: Vec<LinkMeta>,
    /// The comments with text to show on hover.
    #[serde(default)]
    pub notes: Vec<NoteMeta>,
    #[serde(default)]
    pub form: FormMeta,
    /// An encrypted file's security handler and what its password allows
    /// (#2256); `None` for a file that is not encrypted.
    #[serde(default)]
    pub security: Option<SecurityMeta>,
}

/// What an encrypted file allows (#2256), as the password it was opened
/// with reads `/P`: the owner password allows everything.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SecurityMeta {
    /// The cipher, as the status line names it: `RC4 40-bit`, `AES 256-bit`.
    pub method: String,
    /// Opened with the owner password.
    pub owner: bool,
    pub print: bool,
    pub copy: bool,
    pub modify: bool,
    pub annotate: bool,
    pub fill_forms: bool,
}

impl SecurityMeta {
    /// What the file withholds, as the status line lists it: empty when
    /// nothing is (the owner password, or a file that allows everything).
    pub(crate) fn restrictions(&self) -> Vec<&'static str> {
        if self.owner {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (allowed, what) in [
            (self.print, "printing"),
            (self.copy, "copying"),
            (self.modify, "changes"),
            (self.annotate, "comments"),
            (self.fill_forms, "filling in forms"),
        ] {
            if !allowed {
                out.push(what);
            }
        }
        out
    }
}

/// Why `info` or `unlock` did not open the file (#2256).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Locked {
    /// Encrypted: it needs a password.
    Needs,
    /// The password given does not open it.
    Wrong,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    form: Option<FormState>,
    /// A save's file: `bytes` bytes follow the line.
    #[serde(default, skip_serializing_if = "is_zero")]
    bytes: usize,
    /// The child goes after this answer (a form operation panicked, and its
    /// working document may be half changed): the parent starts another.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    fatal: bool,
    /// The file needs a password, or the one given is wrong (#2256).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    locked: Option<Locked>,
    /// A raster's size; `len` pixel bytes follow the line.
    #[serde(default)]
    w: u32,
    #[serde(default)]
    h: u32,
    #[serde(default)]
    len: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
    /// A form operation's answer (#2255).
    Form(FormState),
    /// A save's file, from the child.
    Bytes(Vec<u8>),
    /// The file was written to this path ([`Op::SaveTo`]).
    Saved(PathBuf),
    /// Done, nothing to say ([`Op::Saved`]).
    Ok,
    /// The file is encrypted (#2256): it needs a password (`wrong`: the one
    /// given does not open it).
    Locked {
        wrong: bool,
    },
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
    limit_memory();
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

/// On Linux, the child limits its own memory (#2256), as Windows' job
/// limits it from outside: past [`crate::convert_child::MEMORY_LIMIT`] of
/// data an allocation fails and the child aborts, and the parent answers
/// the page with why. `RLIMIT_DATA`, not `RLIMIT_AS`: glibc's per-thread
/// arenas reserve address space they never use, which an address-space
/// limit counts. No core is written for that abort (`RLIMIT_CORE` 0).
/// Only ever in a child process: the worker's own `run`, and
/// `convert_child::child_main` for a PDF conversion (#2257, whose parent
/// retries a child that died by the PDF's text). Never in [`serve`], which
/// also runs in the suite's process ([`Spawn::InProcess`]), nor anywhere
/// else in the suite's process.
pub(crate) fn limit_memory() {
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
            // SAFETY: `r` is a valid `struct rlimit` for the call's length.
            unsafe { setrlimit(resource, &r) };
        };
        set(RLIMIT_CORE, 0);
        set(RLIMIT_DATA, crate::convert_child::MEMORY_LIMIT);
    }
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
    let mut served: Option<doc::Served> = None;
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
                    Ok(bytes) => served
                        .get_or_insert_with(|| doc::Served::new(bytes.clone()))
                        .answer(req.id, &req.op),
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
        // A form operation panicked: its working copy may be half changed,
        // so this child serves nothing more (#2255).
        if head.fatal {
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
        ..DocMeta::default()
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
    for &[r, g, b, a] in rgba.as_chunks::<4>().0 {
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
    /// The form values filled since the file was last read or saved, in
    /// order: a new child is given them again before anything else
    /// (#2255).
    edits: Vec<(String, FieldValue)>,
    /// The file's size and time when its child last read it: a new child
    /// that would read another file gets no values, and a save in place
    /// onto one refuses ([`CHANGED`]).
    stamp: Option<Stamp>,
    /// A new child could not be given the values: they are gone, and the
    /// next answer says so ([`Self::take_lost`]).
    lost: bool,
    /// The password that opened the file (#2256): every new child is
    /// unlocked with it first, so a child killed after the prompt is
    /// replaced without asking again. Held only here, for this view.
    password: Option<Secret>,
    /// Children started so far (the tests' respawn check).
    #[cfg(test)]
    pub(crate) started: usize,
}

/// What tells a file apart from another at its path: its length and time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: m.len(),
        modified: m.modified().ok(),
    })
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
            edits: Vec::new(),
            stamp: None,
            lost: false,
            password: None,
            #[cfg(test)]
            started: 0,
        }
    }

    /// Ask `op` and wait for its answer, at most the timeout. A child that
    /// runs out of time, dies or answers nonsense is killed: the answer says
    /// why, and the next question starts a new one, given the form values
    /// filled so far first. [`Op::SaveTo`] is answered here: the child's
    /// bytes, written.
    pub(crate) fn ask(&mut self, op: &Op) -> Answer {
        if let Op::SaveTo { path } = op {
            return self.save_to(path);
        }
        let answer = match self.ask_raw(op) {
            Ok((head, body)) => decode(op, head, body),
            Err(why) => Answer::Failed(why),
        };
        if let (Op::SetField { name, value }, Answer::Form(_)) = (op, &answer) {
            self.edits.push((name.clone(), value.clone()));
        }
        answer
    }

    /// Try `password` on the file (#2256): its `info` when it opens it (and
    /// the password is kept, for every new child), else
    /// [`Answer::Locked`] or why not.
    pub(crate) fn unlock(&mut self, password: Secret) -> Answer {
        let op = Op::Unlock { password };
        let answer = match self.ask_raw(&op) {
            Ok((head, body)) => decode(&op, head, body),
            Err(why) => Answer::Failed(why),
        };
        if let (Answer::Info(_), Op::Unlock { password }) = (&answer, op) {
            self.password = Some(password);
        }
        answer
    }

    /// Whether the form values filled were lost since the last call: a new
    /// child could not take them again (#2255).
    pub(crate) fn take_lost(&mut self) -> bool {
        std::mem::take(&mut self.lost)
    }

    /// The form values a new child is given (the tests' look).
    #[cfg(test)]
    pub(crate) fn edits(&self) -> &[(String, FieldValue)] {
        &self.edits
    }

    /// The file this worker reads (the tests' look).
    #[cfg(test)]
    pub(crate) fn path_for_tests(&self) -> &Path {
        &self.path
    }

    fn ask_raw(&mut self, op: &Op) -> Reply {
        self.ensure()?;
        self.exchange(op)
    }

    /// A new child, unlocked with the file's password first when it has
    /// one (#2256): before the form values are given again, and before any
    /// question. A child the password does not open fails, with why.
    fn connect(&mut self) -> Result<(), String> {
        let conn = self
            .start()
            .map_err(|e| format!("the PDF reader could not start: {e}"))?;
        self.conn = Some(conn);
        #[cfg(test)]
        {
            self.started += 1;
        }
        if let Some(password) = self.password.clone() {
            let op = Op::Unlock { password };
            let why = match self.exchange(&op) {
                Ok((head, body)) => match decode(&op, head, body) {
                    Answer::Info(_) => return Ok(()),
                    Answer::Locked { .. } => "the PDF's password no longer opens it".to_string(),
                    Answer::Failed(why) => why,
                    _ => DIED.to_string(),
                },
                Err(why) => why,
            };
            self.stop();
            return Err(why);
        }
        Ok(())
    }

    /// A live child, given the form values filled so far. When it cannot
    /// take them (an error, death, a timeout), or its file is no longer the
    /// one they were filled into, they are dropped and never tried again:
    /// the child is replaced by a clean one, and [`Self::take_lost`] says so.
    fn ensure(&mut self) -> Result<(), String> {
        if self.conn.is_some() {
            return Ok(());
        }
        let now = stamp_of(&self.path);
        let changed = self.stamp.is_some() && now != self.stamp;
        self.connect()?;
        if !self.edits.is_empty() {
            let replayed = !changed
                && {
                    let op = Op::SetFields {
                        values: self.edits.clone(),
                    };
                    matches!(self.exchange(&op), Ok((head, _)) if head.error.is_none() && head.form.is_some())
                };
            if !replayed {
                self.edits.clear();
                self.lost = true;
                self.stop();
                self.connect()?;
            }
        }
        self.stamp = now;
        Ok(())
    }

    /// Save to `target`: the child's bytes, written atomically; then the
    /// child takes them as its file, and they are the file a new child
    /// reads. A save in place onto a file another program changed refuses.
    fn save_to(&mut self, target: &Path) -> Answer {
        if let Err(why) = self.ensure() {
            return Answer::Failed(why);
        }
        if self.lost {
            return Answer::Failed(VALUES_LOST.into());
        }
        if target == self.path && self.stamp.is_some() && stamp_of(target) != self.stamp {
            return Answer::Failed(CHANGED.into());
        }
        let bytes = match self.exchange(&Op::Save) {
            Ok((head, body)) => match decode(&Op::Save, head, body) {
                Answer::Bytes(b) => b,
                Answer::Failed(why) => return Answer::Failed(why),
                _ => return Answer::Failed(DIED.into()),
            },
            Err(why) => return Answer::Failed(why),
        };
        if let Err(e) = opccore::fsio::write_atomic(target, &bytes) {
            return Answer::Failed(format!("the file could not be written: {e}"));
        }
        // A child that cannot take them in goes: the next one reads them.
        if !matches!(self.exchange(&Op::Saved), Ok((head, _)) if head.error.is_none()) {
            self.stop();
        }
        self.path = target.to_path_buf();
        self.edits.clear();
        self.stamp = stamp_of(target);
        Answer::Saved(target.to_path_buf())
    }

    /// Send `op` to the live child and wait for its answer.
    fn exchange(&mut self, op: &Op) -> Reply {
        if self.conn.is_none() {
            return Err(DIED.into());
        }
        self.next_id += 1;
        let id = self.next_id;
        let deadline = Instant::now() + self.timeout;
        let conn = self.conn.as_mut().expect("started above");
        let mut line = zeroize::Zeroizing::new(
            serde_json::to_string(&Request { id, op: op.clone() }).map_err(|e| e.to_string())?,
        );
        line.push('\n');
        // The line is cleared when dropped: an `unlock`'s holds the password.
        if conn
            .input
            .write_all(line.as_bytes())
            .and_then(|()| conn.input.flush())
            .is_err()
        {
            self.stop();
            return Err(DIED.into());
        }
        drop(line);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.conn.as_ref().expect("live").replies.recv_timeout(left) {
                Ok(Ok((head, body))) if head.id == id => {
                    // Its form operation panicked: it serves nothing more.
                    if head.fatal {
                        self.stop();
                    }
                    return Ok((head, body));
                }
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
                Conn::new(Box::new(req_w), Box::new(rep_r))
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
        let conn = match Conn::new(Box::new(input), Box::new(output)) {
            Ok(conn) => conn,
            Err(e) => {
                let _ = limited.child.kill();
                let _ = limited.child.wait();
                return Err(e);
            }
        };
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(limited);
        Ok(conn)
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
    /// A connection over the child's pipes, its answers read on a thread of
    /// their own. `Err` when that thread cannot start.
    fn new(input: Box<dyn Write + Send>, output: Box<dyn Read + Send>) -> std::io::Result<Conn> {
        let (tx, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("pdf-replies".into())
            .spawn(move || read_replies(BufReader::new(output), tx))?;
        Ok(Conn { input, replies })
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
            Ok(head) if head.bytes > 0 && head.len == 0 && head.bytes <= MAX_SAVE_BYTES => {
                let mut body = vec![0; head.bytes];
                match out.read_exact(&mut body) {
                    Ok(()) => Ok((head, body)),
                    Err(_) => Err(DIED.to_string()),
                }
            }
            Ok(head) if head.len == 0 && head.bytes == 0 => Ok((head, Vec::new())),
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
    if let Some(locked) = head.locked {
        return Answer::Locked {
            wrong: locked == Locked::Wrong,
        };
    }
    if let Some(why) = head.error {
        return Answer::Failed(why);
    }
    match op {
        Op::Info | Op::Unlock { .. } => head
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
        Op::SetField { .. } | Op::SetFields { .. } | Op::Fields => head
            .form
            .map_or_else(|| Answer::Failed(DIED.into()), Answer::Form),
        Op::Save if head.bytes > 0 => Answer::Bytes(body),
        Op::Save => Answer::Failed(DIED.into()),
        Op::Saved => Answer::Ok,
        // Never sent: the Worker answers it.
        Op::SaveTo { .. } => Answer::Failed(DIED.into()),
    }
}

// ---- the engine ---------------------------------------------------------------

/// A question answered, as the [`Engine`] hands it back.
#[derive(Clone, Debug)]
pub(crate) struct Done {
    pub op: Op,
    pub answer: Answer,
    /// The form values filled were lost on the way: a new child could not
    /// take them (#2255).
    pub lost: bool,
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
                    let lost = worker.take_lost();
                    theirs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(Done { op, answer, lost });
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

mod doc;
#[cfg(test)]
pub(crate) mod tests;

/// What a fill of a form its permissions keep from being filled answers
/// (the tests' look).
#[cfg(test)]
pub(crate) fn doc_no_filling() -> &'static str {
    doc::NO_FILLING
}
