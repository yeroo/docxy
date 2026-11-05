//! Converting a file in a child process (#633 r3): an RTF, a Web Page, a
//! PDF, or the text recovered from a damaged or any file.
//!
//! The importers bound their own cost (docxcore's import budget), but a
//! bound that is wrong, or a bug, would abort or hang the whole suite and
//! lose every unsaved tab. So the suite runs each conversion in a copy of
//! itself, `docxy --convert-import <what> <in> <out.docx>`, handled at the
//! very top of `main` before any window: the child runs the importer and
//! writes the document as the tab would save it (the Markdown package, which
//! defines its styles). The parent waits at most [`TIMEOUT`], polling, and
//! kills the child past it; on Windows the child runs in a Job object with a
//! [`MEMORY_LIMIT`] and kill-on-close. A child that fails, dies or runs out
//! of time is a load error; the suite carries on.
//!
//! A Web Page's images (#1880) come along: a `data:` src from the page, a
//! file beside it through [`read_local_image`]; every other src (`cid:`,
//! `http:`, a network path) is a placeholder, so opening a page never
//! reaches the network.
//!
//! The ordinary `.docx`, Markdown and bundle loads stay in process. Unit
//! tests convert in process (`cfg(test)`), as does a run with
//! `DOCXY_CONVERT_IN_PROCESS=1`; the tests below drive the child path end to
//! end by running this test binary as the child.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The hidden flag that makes the suite a converting child.
pub(crate) const FLAG: &str = "--convert-import";
/// The longest a conversion may take. The suite waits on its UI thread, so
/// this is how long a file that defeats the import budget can freeze it:
/// the budgets make a real conversion take seconds (the Word fixture PDF
/// converts in well under one), and this leaves room for a slow machine.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(20);
/// The most memory a converting child may commit (Windows only).
pub(crate) const MEMORY_LIMIT: usize = 2 << 30;
/// Set to `1` to convert in the suite's own process instead.
pub(crate) const IN_PROCESS_ENV: &str = "DOCXY_CONVERT_IN_PROCESS";

/// The child exits with this when the conversion failed for a reason it
/// wrote to stderr (a load error the person should read).
const EXIT_FAILED: i32 = 2;
/// The child exits with this when recovery found no text at all.
const EXIT_NOTHING: i32 = 3;

pub(crate) const TOO_LONG: &str = "the conversion took too long";
pub(crate) const DIED: &str = "the file could not be converted";

/// What to convert a file as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum What {
    Rtf,
    Html,
    Pdf,
    /// The text of a damaged Word package (nothing when it has none).
    Recover,
    /// Recover Text from Any File: a package's text, else any text.
    RecoverText,
}

impl What {
    fn arg(self) -> &'static str {
        match self {
            Self::Rtf => "rtf",
            Self::Html => "html",
            Self::Pdf => "pdf",
            Self::Recover => "recover",
            Self::RecoverText => "recover-text",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rtf" => Self::Rtf,
            "html" => Self::Html,
            "pdf" => Self::Pdf,
            "recover" => Self::Recover,
            "recover-text" => Self::RecoverText,
            _ => return None,
        })
    }
}

/// What a conversion gave.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The converted document, as `.docx` bytes.
    Converted(Vec<u8>),
    /// A load error to show (the importer's, or the child's fate).
    Failed(String),
    /// Recovery found no text: the caller keeps its own load error.
    NothingRecovered,
}

/// Whether conversions run in the suite's own process.
pub(crate) fn in_process() -> bool {
    cfg!(test) || std::env::var_os(IN_PROCESS_ENV).is_some_and(|v| v == "1")
}

/// How a conversion is run: `(what, path, its bytes) -> Outcome`. Both kinds
/// hand back the same `.docx`, so a tab is built the same way from either.
pub(crate) type Runner = fn(What, &Path, &[u8]) -> Outcome;

/// The runner this process uses: in process for unit tests and
/// `DOCXY_CONVERT_IN_PROCESS=1`, else a child.
pub(crate) fn runner() -> Runner {
    if in_process() {
        in_process_runner
    } else {
        child_runner
    }
}

/// Convert in this process, ending as the child does: the document written
/// with `doc_to_docx_styled(.., converted = true)`.
pub(crate) fn in_process_runner(what: What, path: &Path, bytes: &[u8]) -> Outcome {
    match convert_bytes(what, bytes, Some(path)) {
        Ok(Some(c)) => Outcome::Converted(c.docx()),
        Ok(None) => Outcome::NothingRecovered,
        Err(e) => Outcome::Failed(e),
    }
}

/// Convert in a child process ([`convert`]).
pub(crate) fn child_runner(what: What, path: &Path, _bytes: &[u8]) -> Outcome {
    convert(what, path)
}

/// What a conversion that could not even start says, with the system's
/// reason: not the file's fault.
fn cannot_start(e: impl std::fmt::Display) -> Outcome {
    Outcome::Failed(format!("cannot start the conversion: {e}"))
}

/// The child's side: when the command line asks for a conversion, do it
/// and return the exit code. Called first thing in `main`.
pub(crate) fn child_main(args: &[OsString]) -> Option<i32> {
    if args.get(1).and_then(|a| a.to_str()) != Some(FLAG) {
        return None;
    }
    let (Some(what), Some(input), Some(output)) = (
        args.get(2).and_then(|a| a.to_str()).and_then(What::parse),
        args.get(3),
        args.get(4),
    ) else {
        eprintln!("docxy: {FLAG} <rtf|html|pdf|recover|recover-text> <in> <out.docx>");
        return Some(EXIT_FAILED);
    };
    Some(run(what, Path::new(input), Path::new(output)))
}

/// Convert `input` as `what` into `output`, in this process: the importer,
/// then the document as the tab would save it. The exit code says how it
/// went; a failure's message goes to stderr.
pub(crate) fn run(what: What, input: &Path, output: &Path) -> i32 {
    let bytes = match std::fs::read(input) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("read error: {e}");
            return EXIT_FAILED;
        }
    };
    let converted = match convert_bytes(what, &bytes, Some(input)) {
        Ok(Some(c)) => c,
        Ok(None) => return EXIT_NOTHING,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_FAILED;
        }
    };
    match std::fs::write(output, converted.docx()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cannot write the converted document: {e}");
            EXIT_FAILED
        }
    }
}

/// What an importer made: the document, and the package holding its media
/// when it has any (a Web Page's images).
pub(crate) struct ConvertedDoc {
    pub(crate) doc: docxcore::model::Document,
    pub(crate) base: Option<docxcore::package::Package>,
}

impl ConvertedDoc {
    fn plain(doc: docxcore::model::Document) -> Self {
        ConvertedDoc { doc, base: None }
    }

    /// The document as the tab would save it: `doc_to_docx_styled(..,
    /// converted = true)`, over the package with its media when there is one.
    pub(crate) fn docx(&self) -> Vec<u8> {
        crate::doc_to_docx_styled(&self.doc, &[], self.base.as_ref(), true)
    }
}

/// The importer `what` names, over `bytes` read from `path` (a Web Page's
/// images are looked for beside it; without one, only `data:` images come
/// along): `Ok(None)` only when recovery found nothing.
pub(crate) fn convert_bytes(
    what: What,
    bytes: &[u8],
    path: Option<&Path>,
) -> Result<Option<ConvertedDoc>, String> {
    use docxcore::import;
    let plain =
        |r: Result<docxcore::model::Document, String>| r.map(|d| Some(ConvertedDoc::plain(d)));
    match what {
        What::Rtf => plain(import::import_rtf(bytes)),
        What::Html => {
            let dir = page_dir(path);
            let mut resolve = |src: &str, limit: usize| {
                dir.as_deref()
                    .and_then(|dir| read_local_image(dir, src, limit))
            };
            let imported = import::import_html_with(bytes, &mut resolve)?;
            let pkg = imported.package();
            Ok(Some(ConvertedDoc {
                doc: pkg.document.clone(),
                base: Some(pkg),
            }))
        }
        What::Pdf => plain(import::import_pdf(bytes)),
        What::Recover => Ok(import::recover_docx_text(bytes)?.map(ConvertedDoc::plain)),
        What::RecoverText => match import::recover_docx_text(bytes)? {
            Some(doc) => Ok(Some(ConvertedDoc::plain(doc))),
            None => plain(import::recover_any_text(bytes)),
        },
    }
}

/// The folder a page's relative image srcs are read from: its file's
/// folder (`.` for a bare file name); none without a path.
fn page_dir(path: Option<&Path>) -> Option<PathBuf> {
    let parent = path?.parent()?;
    Some(if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    })
}

/// The bytes of the local file `src` names beside a page in `page_dir`
/// ([`local_image_path`]): a regular file of at most `limit` bytes, its
/// size read before it is; `None` for anything else.
pub(crate) fn read_local_image(page_dir: &Path, src: &str, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let path = local_image_path(page_dir, src)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(&path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// The local file an `<img src>` names, for a page in `page_dir`; `None`
/// for anything that is not one, so the network is never reached:
///
/// - a src with a URI scheme other than `file:` (`cid:`, `http:`, …);
/// - a network or device path: `\\host\share`, `//host/share`,
///   `\\?\…`, `\\.\…`, `file://host/…` (any host but `localhost`), and the
///   same spelled with escapes (`file:///%5C%5Chost`);
/// - a Windows drive-relative path (`C:x.png`, relative to that drive's
///   current folder, not the page's).
///
/// A `file:` URI (`file:///x`, `file:///C:/x`, `file://localhost/x`)
/// names its path; any other src is a path, absolute or relative to
/// `page_dir`. Escapes are decoded, the query and fragment dropped, `\`
/// and `/` both separators.
pub(crate) fn local_image_path(page_dir: &Path, src: &str) -> Option<PathBuf> {
    let src = src.trim();
    if src.is_empty() || network_path(src) {
        return None;
    }
    let path = match uri_scheme(src) {
        Some(scheme) if scheme.eq_ignore_ascii_case("file") => {
            let rest = &src[scheme.len() + 1..];
            let path = match rest.strip_prefix("//") {
                Some(after) => {
                    let (host, path) =
                        after.split_at(after.find(['/', '\\']).unwrap_or(after.len()));
                    if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                        return None;
                    }
                    path
                }
                None => rest,
            };
            let path = decoded_path(path)?;
            // `file:///C:/x` names `C:/x`.
            let path = match path.strip_prefix(['/', '\\']) {
                Some(p) if drive_letter(p) => p.to_string(),
                _ => path,
            };
            if network_path(&path) || !is_absolute_local(&path) {
                return None;
            }
            path
        }
        Some(_) => return None,
        None => {
            let path = decoded_path(src)?;
            if network_path(&path) {
                return None;
            }
            path
        }
    };
    if drive_letter(&path) && !path[2..].starts_with(['/', '\\']) {
        return None;
    }
    // On Windows `\` is a separator already; elsewhere it is one too here.
    let path = if cfg!(windows) {
        path
    } else {
        path.replace('\\', "/")
    };
    let path = PathBuf::from(path);
    Some(if path.is_absolute() {
        path
    } else {
        page_dir.join(path)
    })
}

/// A src's URI scheme: two or more of a letter then letters, digits, `+`,
/// `-` or `.`, before a `:` (so `C:` is a drive).
fn uri_scheme(src: &str) -> Option<&str> {
    let colon = src.find(':')?;
    let scheme = &src[..colon];
    let mut chars = scheme.chars();
    (scheme.len() >= 2
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// Whether `p` starts with a drive letter and its colon.
fn drive_letter(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// A UNC, device or other network path: two separators first.
fn network_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && matches!(b[0], b'/' | b'\\') && matches!(b[1], b'/' | b'\\')
}

/// What a `file:` URI's path must be: a drive path on Windows, rooted
/// elsewhere.
fn is_absolute_local(p: &str) -> bool {
    if cfg!(windows) {
        drive_letter(p) && p[2..].starts_with(['/', '\\'])
    } else {
        p.starts_with('/')
    }
}

/// A src path without its query and fragment, its `%XX` escapes decoded;
/// `None` when that is not UTF-8 or holds a NUL.
fn decoded_path(src: &str) -> Option<String> {
    let path = src.split(['?', '#']).next().unwrap_or("");
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    let path = String::from_utf8(out).ok()?;
    (!path.is_empty() && !path.contains('\0')).then_some(path)
}

/// The parent's side: convert `path` as `what` in a child process.
pub(crate) fn convert(what: What, path: &Path) -> Outcome {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return cannot_start(e),
    };
    let dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(e) => return cannot_start(e),
    };
    let out = dir.0.join("converted.docx");
    let mut cmd = Command::new(exe);
    cmd.arg(FLAG).arg(what.arg()).arg(path).arg(&out);
    run_child(cmd, &dir.0, &out, TIMEOUT, MEMORY_LIMIT)
}

/// Run a converting child: wait at most `timeout` (killing it past that),
/// under a `memory` limit on Windows, then read what it wrote to `out`, or
/// why it failed from its stderr (kept in `dir`).
fn run_child(
    mut cmd: Command,
    dir: &Path,
    out: &Path,
    timeout: Duration,
    memory: usize,
) -> Outcome {
    let err_path = dir.join("stderr.txt");
    let err_file = match std::fs::File::create(&err_path) {
        Ok(f) => f,
        Err(e) => return cannot_start(e),
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return cannot_start(e),
    };
    // ⚠️ The child is put in its job just after it starts, so it runs a
    // moment unlimited; the import has not started by then (it reads its
    // file first), and closing the job kills it either way.
    #[cfg(windows)]
    let _job = job::limit(&child, memory);
    #[cfg(not(windows))]
    let _ = memory;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Outcome::Failed(TOO_LONG.into());
            }
        }
    };
    match status.code() {
        Some(0) => match std::fs::read(out) {
            Ok(docx) => Outcome::Converted(docx),
            Err(_) => Outcome::Failed(DIED.into()),
        },
        Some(EXIT_FAILED) => {
            let why = std::fs::read_to_string(&err_path).unwrap_or_default();
            let why = why.lines().map(str::trim).rfind(|l| !l.is_empty());
            Outcome::Failed(why.unwrap_or(DIED).to_string())
        }
        Some(EXIT_NOTHING) => Outcome::NothingRecovered,
        // A panic, an abort, out of memory, killed by its job.
        _ => Outcome::Failed(DIED.into()),
    }
}

/// A directory of its own under the system temp dir, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "docxy-convert-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Windows Job object holding the child: a per-process memory limit, and
/// kill-on-close, so the child cannot outlive the job's handle. Hand-declared
/// kernel32 calls, as gridcore's clock does, rather than a crate.
#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *mut c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    /// The job; closing it (on drop) kills the child if it still runs.
    pub(super) struct Job(Handle);

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Put `child` in a new job limited to `memory` bytes. `None` when the
    /// system refuses (the timeout still bounds the child).
    pub(super) fn limit(child: &std::process::Child, memory: usize) -> Option<Job> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() {
            return None;
        }
        let job = Job(handle);
        let mut info = ExtendedLimitInformation {
            basic: BasicLimitInformation {
                limit_flags: JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            process_memory_limit: memory,
            ..Default::default()
        };
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&mut info as *mut ExtendedLimitInformation).cast(),
                std::mem::size_of::<ExtendedLimitInformation>() as u32,
            )
        };
        if set == 0 {
            return None;
        }
        let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) };
        (assigned != 0).then_some(job)
    }

    #[cfg(test)]
    #[test]
    fn the_limit_structure_has_windows_layout() {
        // x64: 64-byte basic limits, 48-byte IO counters, four SIZE_Ts.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<ExtendedLimitInformation>(), 144);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Set to `what|in|out` (and `MODE_ENV` to misbehave) to make the test
    /// binary a converting child.
    const CHILD_ENV: &str = "DOCXY_CONVERT_TEST_CHILD";
    const MODE_ENV: &str = "DOCXY_CONVERT_TEST_MODE";

    /// The child half of the tests below: the same `run` the suite's child
    /// runs, or, by `MODE_ENV`, a child that aborts, hangs or eats memory.
    #[test]
    fn child_entry() {
        let Some(spec) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        match std::env::var(MODE_ENV).as_deref() {
            Ok("abort") => std::process::abort(),
            Ok("hang") => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            Ok("eat") => {
                let mut hoard: Vec<Vec<u8>> = Vec::new();
                loop {
                    hoard.push(vec![1u8; 64 << 20]);
                }
            }
            _ => {}
        }
        let spec = spec.to_string_lossy().into_owned();
        let parts: Vec<&str> = spec.split('|').collect();
        let what = What::parse(parts[0]).unwrap();
        std::process::exit(run(what, Path::new(parts[1]), Path::new(parts[2])));
    }

    /// A [`Runner`] that runs this test binary as the converting child: what
    /// the suite's own tests use to drive the production path end to end.
    pub(crate) fn test_child_runner(what: What, path: &Path, _bytes: &[u8]) -> Outcome {
        convert_by_test_child(what, path, None, TIMEOUT, MEMORY_LIMIT)
    }

    /// Run this test binary as a converting child of `what` over `input`.
    fn convert_by_test_child(
        what: What,
        input: &Path,
        mode: Option<&str>,
        timeout: Duration,
        memory: usize,
    ) -> Outcome {
        let dir = TempDir::new().unwrap();
        let out = dir.0.join("converted.docx");
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "convert_child::tests::child_entry",
            "--nocapture",
        ])
        .env(
            CHILD_ENV,
            format!("{}|{}|{}", what.arg(), input.display(), out.display()),
        );
        if let Some(mode) = mode {
            cmd.env(MODE_ENV, mode);
        }
        run_child(cmd, &dir.0, &out, timeout, memory)
    }

    fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn the_hidden_flag_is_only_taken_in_first_place() {
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(child_main(&args(&["docxy", "file.rtf"])), None);
        assert_eq!(child_main(&args(&["docxy", "x", FLAG])), None);
        assert_eq!(
            child_main(&args(&["docxy", FLAG, "nonsense", "a", "b"])),
            Some(EXIT_FAILED)
        );
        assert_eq!(What::parse("recover-text"), Some(What::RecoverText));
        assert!(in_process(), "unit tests convert in process");
    }

    #[test]
    fn a_child_converts_an_rtf_into_a_word_document() {
        let dir = TempDir::new().unwrap();
        let rtf = write(
            &dir,
            "in.rtf",
            br"{\rtf1{\stylesheet{\s1 heading 1;}}\pard\s1 Title\par\pard Body text\par}",
        );
        let Outcome::Converted(docx) =
            convert_by_test_child(What::Rtf, &rtf, None, TIMEOUT, MEMORY_LIMIT)
        else {
            panic!("not converted")
        };
        let pkg = docxcore::package::load_package(&docx).unwrap();
        assert_eq!(
            docxcore::import::paragraph_texts(&pkg.document),
            ["Title", "Body text"]
        );
        let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(styles.contains("w:styleId=\"Heading1\""));
    }

    #[test]
    fn an_import_error_comes_back_as_its_message() {
        let dir = TempDir::new().unwrap();
        let empty = write(&dir, "empty.rtf", br"{\rtf1{\info{\title x}}}");
        match convert_by_test_child(What::Rtf, &empty, None, TIMEOUT, MEMORY_LIMIT) {
            Outcome::Failed(why) => assert!(why.contains("holds no text"), "{why}"),
            other => panic!("{other:?}"),
        }
        let junk = write(&dir, "junk.docx", b"not a zip");
        assert!(matches!(
            convert_by_test_child(What::Recover, &junk, None, TIMEOUT, MEMORY_LIMIT),
            Outcome::NothingRecovered
        ));
    }

    /// A child that cannot start is the system's failure, said with its
    /// reason, not "the file could not be converted".
    #[test]
    fn a_child_that_cannot_start_says_why() {
        let dir = TempDir::new().unwrap();
        let out = dir.0.join("converted.docx");
        let cmd = Command::new(dir.0.join("no-such-program.exe"));
        match run_child(cmd, &dir.0, &out, TIMEOUT, MEMORY_LIMIT) {
            Outcome::Failed(why) => {
                assert!(why.starts_with("cannot start the conversion: "), "{why}");
                assert_ne!(why, DIED);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_child_that_dies_is_a_load_error_not_a_crash() {
        let dir = TempDir::new().unwrap();
        let rtf = write(&dir, "in.rtf", br"{\rtf1 x\par}");
        match convert_by_test_child(What::Rtf, &rtf, Some("abort"), TIMEOUT, MEMORY_LIMIT) {
            Outcome::Failed(why) => assert_eq!(why, DIED),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_child_that_runs_too_long_is_killed() {
        let dir = TempDir::new().unwrap();
        let rtf = write(&dir, "in.rtf", br"{\rtf1 x\par}");
        let started = Instant::now();
        match convert_by_test_child(
            What::Rtf,
            &rtf,
            Some("hang"),
            Duration::from_millis(500),
            MEMORY_LIMIT,
        ) {
            Outcome::Failed(why) => assert_eq!(why, TOO_LONG),
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    /// The job's memory limit stops a child that keeps allocating.
    #[cfg(windows)]
    #[test]
    fn a_child_over_its_memory_limit_is_stopped() {
        let dir = TempDir::new().unwrap();
        let rtf = write(&dir, "in.rtf", br"{\rtf1 x\par}");
        match convert_by_test_child(
            What::Rtf,
            &rtf,
            Some("eat"),
            Duration::from_secs(30),
            256 << 20,
        ) {
            Outcome::Failed(why) => assert_eq!(why, DIED),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_image_src_names_a_local_file_or_nothing() {
        let dir = Path::new("pages");
        let at = |src: &str| local_image_path(dir, src);
        // Relative: beside the page; escapes, query and fragment.
        assert_eq!(
            at("a_files/image001.png"),
            Some(dir.join("a_files/image001.png"))
        );
        assert_eq!(at(" My%20Pic.png?v=2#top "), Some(dir.join("My Pic.png")));
        assert_eq!(at("../up.gif"), Some(dir.join("../up.gif")));
        // Schemes other than file: never.
        for src in [
            "cid:logo@example.com",
            "http://example.com/a.png",
            "HTTPS://example.com/a.png",
            "about:blank",
            "javascript:x",
            "data:image/png;base64,AAAA",
            "mailto:a@b",
        ] {
            assert_eq!(at(src), None, "{src}");
        }
        // Network and device paths never, as written or escaped.
        for src in [
            "\\\\host\\share\\x.png",
            "//host/share/x.png",
            "/\\host/share/x.png",
            "\\\\?\\UNC\\host\\share\\x.png",
            "\\\\?\\C:\\x.png",
            "\\\\.\\pipe\\x",
            "file://host/share/x.png",
            "file:////host/share/x.png",
            "file://///host/share/x.png",
            "file:///%5C%5Chost/share/x.png",
            "%5C%5Chost/share/x.png",
            "%2F%2Fhost/share/x.png",
        ] {
            assert_eq!(at(src), None, "{src}");
        }
        // Drive-relative: that drive's current folder, not the page's.
        assert_eq!(at("C:x.png"), None);
        assert_eq!(at(""), None);
        assert_eq!(at("a%00.png"), None);
        assert_eq!(at("%FF.png"), None, "not UTF-8");
    }

    #[cfg(not(windows))]
    #[test]
    fn local_absolute_paths_and_file_uris_on_unix() {
        let dir = Path::new("pages");
        let at = |src: &str| local_image_path(dir, src);
        assert_eq!(at("/srv/a.png"), Some(PathBuf::from("/srv/a.png")));
        assert_eq!(
            at("file:///srv/a%20b.png"),
            Some(PathBuf::from("/srv/a b.png"))
        );
        assert_eq!(
            at("file://localhost/srv/a.png"),
            Some(PathBuf::from("/srv/a.png"))
        );
        assert_eq!(at("FILE:/srv/a.png"), Some(PathBuf::from("/srv/a.png")));
        assert_eq!(at("file:a.png"), None, "a file: URI names an absolute path");
        assert_eq!(at("file:///C:/a.png"), None, "no drives here");
        // A Windows page's separators.
        assert_eq!(
            at("a_files\\image001.png"),
            Some(dir.join("a_files/image001.png"))
        );
        assert_eq!(at("C:\\a.png"), Some(dir.join("C:/a.png")));
    }

    #[cfg(windows)]
    #[test]
    fn local_absolute_paths_and_file_uris_on_windows() {
        let dir = Path::new("C:\\pages");
        let at = |src: &str| local_image_path(dir, src);
        assert_eq!(at("C:\\img\\a.png"), Some(PathBuf::from("C:\\img\\a.png")));
        assert_eq!(at("d:/img/a.png"), Some(PathBuf::from("d:/img/a.png")));
        assert_eq!(
            at("file:///C:/img/a%20b.png"),
            Some(PathBuf::from("C:/img/a b.png"))
        );
        assert_eq!(
            at("file://localhost/C:/a.png"),
            Some(PathBuf::from("C:/a.png"))
        );
        assert_eq!(at("file:///img/a.png"), None, "no drive");
        assert_eq!(at("\\img\\a.png"), Some(PathBuf::from("C:\\img\\a.png")));
        assert_eq!(
            at("a_files\\image001.png"),
            Some(dir.join("a_files\\image001.png"))
        );
    }

    #[test]
    fn only_a_regular_file_within_the_limit_is_read() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a.png", b"0123456789");
        std::fs::create_dir(dir.0.join("sub.png")).unwrap();
        assert_eq!(
            read_local_image(&dir.0, "a.png", 10).as_deref(),
            Some(&b"0123456789"[..])
        );
        assert_eq!(read_local_image(&dir.0, "a.png", 9), None);
        assert_eq!(read_local_image(&dir.0, "sub.png", 100), None);
        assert_eq!(read_local_image(&dir.0, "gone.png", 100), None);
        assert_eq!(read_local_image(&dir.0, "cid:a.png", 100), None);
        #[cfg(unix)]
        assert_eq!(read_local_image(Path::new("/"), "/dev/zero", 100), None);
    }

    #[test]
    fn a_page_without_a_path_takes_only_its_data_images() {
        let page = b"<p>x<img src=\"data:image/gif;base64,R0lGODlhAQABAAAAACw=\"><img src=a.png width=5 height=5></p>";
        let c = convert_bytes(What::Html, page, None).unwrap().unwrap();
        let base = c.base.as_ref().unwrap();
        let pkg = docxcore::package::load_package(&c.docx()).unwrap();
        assert_eq!(page_dir(None), None);
        assert_eq!(page_dir(Some(Path::new("p.htm"))), Some(PathBuf::from(".")));
        let media: Vec<&str> = pkg
            .part_names()
            .into_iter()
            .filter(|n| n.starts_with("word/media/"))
            .collect();
        // The GIF and the placeholder.
        assert_eq!(media.len(), 2, "{media:?}");
        assert_eq!(base.document, pkg.document);
    }
}
