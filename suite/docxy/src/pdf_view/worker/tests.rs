//! The worker through a real child process (#2245): this test binary run
//! as the child (`--exact pdf_view::worker::tests::child_entry`), the way
//! `convert_child`'s tests run it. Misbehaving children (hang, abort, exit,
//! nonsense) show a page fails and the next question starts a new child;
//! the corpus and mutated copies of it show no file crashes or hangs the
//! parent.

use super::*;
use crate::open_mode_tests::Scratch;
use crate::pdf_view::PdfView;
use crate::pdf_view::tests::{
    CORPUS, ENCRYPTED, HARD, UNREADABLE, UNREFERENCED, corpus_path, user_password,
};
use std::path::Path;
use std::time::{Duration, Instant};

/// Set to `<file>|<timeout ms>` to make the test binary a worker child.
const CHILD_ENV: &str = "DOCXY_PDF_WORKER_TEST_CHILD";
/// How the child misbehaves: `hang-pixels`, `abort-page-2`,
/// `garbage-on-text`; `abort-on-text` and `abort-on-replay` (#2255) abort
/// on any text and on the form values given again, and a mode may name
/// both; `hog` (#2256) asks for more memory than the child may have when
/// asked for page 1's pixels, `check-secret` aborts when the password
/// an `unlock` gives is in its command line or environment, `limits`
/// aborts on `info` unless its data limit is `MEMORY_LIMIT` and its core
/// limit 0, and `abort-on-unlock` aborts on any `unlock`; `slow-print`
/// (#2258) takes 1.5 × the child's timeout over a print, and `hang-print`
/// never answers one.
const MODE_ENV: &str = "DOCXY_PDF_WORKER_TEST_MODE";

/// The child half of the tests below: the same `run` the suite's child
/// runs, misbehaving by `MODE_ENV`.
#[test]
fn child_entry() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let (path, ms) = spec.rsplit_once('|').expect("<file>|<ms>");
    let mode = std::env::var(MODE_ENV).unwrap_or_default();
    let code = run(
        Path::new(path),
        Duration::from_millis(ms.parse().expect("ms")),
        move |op| match (mode.as_str(), op) {
            (m, Op::Text { .. }) if m.contains("abort-on-text") => std::process::abort(),
            (m, Op::SetFields { .. }) if m.contains("abort-on-replay") => std::process::abort(),
            ("hog", Op::Pixels { page: 0, .. }) => {
                let v: Vec<u8> = Vec::with_capacity(crate::convert_child::MEMORY_LIMIT + (1 << 30));
                std::hint::black_box(&v);
            }
            ("abort-on-unlock", Op::Unlock { .. }) => std::process::abort(),
            ("limits", Op::Info) => {
                let limits = std::fs::read_to_string("/proc/self/limits").unwrap_or_default();
                let soft = |name: &str| {
                    limits
                        .lines()
                        .find_map(|l| l.strip_prefix(name))
                        .and_then(|rest| rest.split_whitespace().next())
                        .map(str::to_owned)
                };
                let data = crate::convert_child::MEMORY_LIMIT.to_string();
                if soft("Max core file size").as_deref() != Some("0")
                    || soft("Max data size").as_deref() != Some(data.as_str())
                {
                    std::process::abort();
                }
            }
            ("check-secret", Op::Unlock { password }) => {
                let pw = std::ffi::OsStr::new(password.as_str());
                let has = |s: &std::ffi::OsStr| {
                    s.as_encoded_bytes()
                        .windows(pw.len())
                        .any(|w| w == pw.as_encoded_bytes())
                };
                if std::env::args_os().any(|a| has(&a))
                    || std::env::vars_os().any(|(k, v)| has(&k) || has(&v))
                {
                    std::process::abort();
                }
            }
            ("slow-print", Op::Print { .. }) => {
                std::thread::sleep(Duration::from_millis(
                    ms.parse::<u64>().expect("ms") * 3 / 2,
                ));
            }
            ("hang-print", Op::Print { .. }) => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            ("hang-pixels", Op::Pixels { .. }) => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            ("abort-page-2", Op::Pixels { page: 1, .. }) => std::process::abort(),
            ("garbage-on-text", Op::Text { .. }) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(b"this is not an answer\n");
                let _ = out.flush();
            }
            _ => {}
        },
    );
    std::process::exit(code);
}

/// A spawn that runs this test binary as a worker child on `path`, its own
/// watchdog at twice `child_timeout`.
pub(crate) fn test_child(
    path: &Path,
    child_timeout: Duration,
    mode: Option<&'static str>,
) -> Spawn {
    let spec = format!("{}|{}", path.display(), child_timeout.as_millis());
    Spawn::Command(Arc::new(move || {
        let mut cmd = Command::new(std::env::current_exe().expect("the test binary"));
        cmd.args([
            "--exact",
            "pdf_view::worker::tests::child_entry",
            "--nocapture",
        ])
        .env(CHILD_ENV, &spec);
        if let Some(mode) = mode {
            cmd.env(MODE_ENV, mode);
        }
        cmd
    }))
}

fn child_worker(
    name: &str,
    mode: Option<&'static str>,
    parent: Duration,
    child: Duration,
) -> Worker {
    let path = corpus_path(name);
    Worker::new(&path, test_child(&path, child, mode), parent)
}

const LONG: Duration = Duration::from_secs(60);

#[test]
fn the_hidden_flag_is_only_taken_in_first_place() {
    let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(child_main(&args(&["docxy", "file.pdf"])), None);
    assert_eq!(child_main(&args(&["docxy", "x", FLAG])), None);
    assert_eq!(child_main(&args(&["docxy", FLAG])), Some(EXIT_USAGE));
}

#[test]
fn pixels_go_over_as_straight_bgra() {
    // Opaque: red and blue swap.
    assert_eq!(straight_bgra(&[10, 20, 30, 255]), vec![30, 20, 10, 255]);
    // Half covered: un-premultiplied.
    assert_eq!(straight_bgra(&[50, 0, 100, 128]), vec![199, 0, 100, 128]);
    assert_eq!(straight_bgra(&[0, 0, 0, 0]), vec![0, 0, 0, 0]);
}

#[test]
fn a_child_answers_info_pixels_and_text() {
    let mut w = child_worker("source", None, LONG, LONG);
    let Answer::Info(m) = w.ask(&Op::Info) else {
        panic!("no info")
    };
    assert!(!m.pages.is_empty());
    let Answer::Pixels(r) = w.ask(&Op::Pixels { page: 0, scale: 1. }) else {
        panic!("no pixels")
    };
    assert_eq!(r.w, m.pages[0].w.round() as u32);
    assert_eq!(r.h, m.pages[0].h.round() as u32);
    assert_eq!(r.bgra.len(), (r.w * r.h * 4) as usize);
    let Answer::Text(t) = w.ask(&Op::Text { page: 0 }) else {
        panic!("no text")
    };
    assert!(t.plain_text().contains("Привет"));
    // A page past the end is the file's answer, not the child's death.
    match w.ask(&Op::Pixels {
        page: 99,
        scale: 1.,
    }) {
        Answer::Failed(why) => assert!(why.contains("does not exist"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.started, 1);
}

#[test]
fn a_question_past_the_timeout_kills_the_child_and_the_next_starts_another() {
    let mut w = child_worker(
        "text",
        Some("hang-pixels"),
        Duration::from_millis(800),
        LONG,
    );
    let t0 = Instant::now();
    match w.ask(&Op::Pixels { page: 0, scale: 1. }) {
        Answer::Failed(why) => assert_eq!(why, TOO_LONG),
        other => panic!("{other:?}"),
    }
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    // Only the hanging question is bounded short: the next one starts a new
    // child and reads text, which a slow machine may take longer over.
    w.timeout = LONG;
    assert!(matches!(w.ask(&Op::Text { page: 0 }), Answer::Text(_)));
    assert_eq!(w.started, 2);
}

#[test]
fn a_child_that_dies_fails_only_that_question() {
    let mut w = child_worker("text", Some("abort-page-2"), LONG, LONG);
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    match w.ask(&Op::Pixels {
        page: 1,
        scale: 0.5,
    }) {
        Answer::Failed(why) => assert_eq!(why, DIED),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    assert_eq!(w.started, 2);
}

#[test]
fn nonsense_from_a_child_is_a_dead_child() {
    let mut w = child_worker("text", Some("garbage-on-text"), LONG, LONG);
    match w.ask(&Op::Text { page: 0 }) {
        Answer::Failed(why) => assert_eq!(why, DIED),
        other => panic!("{other:?}"),
    }
    assert!(matches!(w.ask(&Op::Info), Answer::Info(_)));
    assert_eq!(w.started, 2);
}

/// A child spinning on a page that nobody waits for any more ends itself:
/// the parent here waits far longer than the child's watchdog allows.
#[test]
fn a_hanging_child_ends_itself() {
    let mut w = child_worker(
        "text",
        Some("hang-pixels"),
        LONG,
        Duration::from_millis(300),
    );
    let t0 = Instant::now();
    match w.ask(&Op::Pixels { page: 0, scale: 1. }) {
        Answer::Failed(why) => assert_eq!(why, DIED),
        other => panic!("{other:?}"),
    }
    assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
}

/// The view over a real child: the page the child dies on is failed with
/// its reason, every other page is drawn and read.
#[test]
fn the_view_fails_only_the_page_its_child_died_on() {
    let path = corpus_path("text");
    let mut v = PdfView::open(&path, test_child(&path, LONG, Some("abort-page-2"))).unwrap();
    v.frame(900., 1.0e5, 0., 1.);
    let t0 = Instant::now();
    loop {
        v.pump();
        if !v.busy() && v.next_job().is_none() {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the view never settled"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(v.failed.keys().copied().collect::<Vec<_>>(), vec![1]);
    assert_eq!(v.page_state(1), format!("failed: {DIED}"));
    assert_eq!(v.page_state(0), "drawn");
    assert!(
        v.texts.iter().all(Option::is_some),
        "both pages' text is read"
    );
}

fn forms_worker(mode: &'static str) -> Worker {
    let path = corpus_path("forms");
    Worker::new(&path, test_child(&path, LONG, Some(mode)), LONG)
}

fn name_of(w: &mut Worker) -> String {
    match w.ask(&Op::Fields) {
        Answer::Form(st) => st
            .fields
            .iter()
            .find(|f| f.name == "name")
            .map(|f| f.value.join(""))
            .unwrap_or_default(),
        other => panic!("{other:?}"),
    }
}

/// Criterion 10: a child that dies after values were set is replaced, and
/// the new one is given them before it answers anything: its fields and its
/// pages have them.
#[test]
fn a_new_child_is_given_the_values_set_so_far() {
    let mut w = forms_worker("abort-on-text");
    assert!(matches!(w.ask(&Op::Info), Answer::Info(_)));
    let set = Op::SetField {
        name: "name".into(),
        value: FieldValue::Text { text: "Ada".into() },
    };
    assert!(matches!(w.ask(&set), Answer::Form(_)));
    match w.ask(&Op::Text { page: 0 }) {
        Answer::Failed(why) => assert_eq!(why, DIED),
        other => panic!("{other:?}"),
    }
    assert_eq!(name_of(&mut w), "Ada");
    assert!(!w.take_lost());
    assert_eq!(w.started, 2);
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
}

/// Criterion 10: a new child that dies taking the values again drops them,
/// says so once, and is replaced by a clean one: nothing loops, the next
/// page draws.
#[test]
fn values_a_new_child_cannot_take_are_dropped_and_never_tried_again() {
    let mut w = forms_worker("abort-on-text abort-on-replay");
    assert!(matches!(w.ask(&Op::Info), Answer::Info(_)));
    let set = Op::SetField {
        name: "name".into(),
        value: FieldValue::Text { text: "Ada".into() },
    };
    assert!(matches!(w.ask(&set), Answer::Form(_)));
    assert!(matches!(w.ask(&Op::Text { page: 0 }), Answer::Failed(_)));
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    assert!(w.take_lost(), "said once");
    assert!(!w.take_lost());
    assert!(w.edits().is_empty());
    assert_eq!(name_of(&mut w), "");
    // The child that died on the replay, then a clean one: no more.
    assert_eq!(w.started, 3);
}

/// #2255 r2: a child that dies after values were set, when the file was
/// changed on disk meanwhile, gives the new one no values (they were filled
/// into the file as it was): they are dropped and said once, and a save in
/// place then leaves the other program's bytes as they are.
#[test]
fn a_new_child_on_a_changed_file_gets_no_values() {
    let dir = Scratch::new();
    let path = dir.path("forms.pdf");
    std::fs::copy(corpus_path("forms"), &path).unwrap();
    let mut w = Worker::new(&path, test_child(&path, LONG, Some("abort-on-text")), LONG);
    assert!(matches!(w.ask(&Op::Info), Answer::Info(_)));
    let set = Op::SetField {
        name: "name".into(),
        value: FieldValue::Text { text: "Ada".into() },
    };
    assert!(matches!(w.ask(&set), Answer::Form(_)));
    let mut changed = std::fs::read(&path).unwrap();
    changed.extend_from_slice(b"\n% another program was here\n");
    std::fs::write(&path, &changed).unwrap();
    assert!(matches!(w.ask(&Op::Text { page: 0 }), Answer::Failed(_)));
    assert_eq!(name_of(&mut w), "", "nothing replayed");
    assert!(w.take_lost());
    assert!(w.edits().is_empty());
    // The new child read the changed file: a save in place writes it as it
    // is, never the values filled into the old one.
    assert!(matches!(
        w.ask(&Op::SaveTo { path: path.clone() }),
        Answer::Saved(_)
    ));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        changed,
        "the other program's bytes"
    );
}

/// A small, seeded generator: the mutated set is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Mutation `k` of `bytes`: truncated, bytes flipped, a span zeroed.
fn mutate(bytes: &[u8], k: usize, rng: &mut Rng) -> Vec<u8> {
    let mut b = bytes.to_vec();
    match k % 3 {
        0 => b.truncate(1 + rng.below(b.len() - 1)),
        1 => {
            for _ in 0..16 {
                let i = rng.below(b.len());
                b[i] ^= 1 + rng.below(255) as u8;
            }
        }
        _ => {
            let len = 64 + rng.below(960);
            let at = rng.below(b.len());
            let end = (at + len).min(b.len());
            b[at..end].fill(0);
        }
    }
    b
}

/// Mutated copies of each corpus file.
const MUTATIONS: usize = 21;
/// How many of each file's mutations are also printed and exported
/// (#2258): a third, and every original and structural mutation. Measured
/// on this machine (22 cores, debug build, 6 threads): the test takes 4.9 s
/// with none, 7.0 s with 7 and 12.7 s with all 21.
const OUTPUT_MUTATIONS: usize = 7;
/// The child's and the parent's timeout for the mutated files.
const FUZZ_TIMEOUT: Duration = Duration::from_secs(5);

/// The structural mutations of `bytes` (#2256), by name: every xref
/// offset shifted, `startxref` moved, the `/Encrypt` dictionary's bytes
/// flipped, an object stream's `/N` and `/First` and a `/Length` huge.
/// Only those the file has something for.
fn structural(bytes: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let find = |pat: &[u8]| bytes.windows(pat.len()).position(|w| w == pat);
    let mut out = Vec::new();
    // Every "nnnnnnnnnn ggggg n" entry 3 bytes on.
    let mut shifted = bytes.to_vec();
    let mut any = false;
    let mut i = 0;
    while i + 18 <= shifted.len() {
        let e = &shifted[i..i + 18];
        if e[..10].iter().all(u8::is_ascii_digit)
            && e[10] == b' '
            && e[11..16].iter().all(u8::is_ascii_digit)
            && &e[16..18] == b" n"
        {
            let o: u64 = text(&e[..10]).parse().unwrap_or(0) + 3;
            shifted[i..i + 10].copy_from_slice(format!("{o:010}").as_bytes());
            any = true;
            i += 18;
        } else {
            i += 1;
        }
    }
    if any {
        out.push(("xref-shifted", shifted));
    }
    if let Some(at) = bytes.windows(10).rposition(|w| w == b"startxref\n") {
        let start = at + 10;
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
        let n: u64 = text(&bytes[start..end]).parse().unwrap_or(0);
        let mut b = bytes[..start].to_vec();
        b.extend((n + 17).to_string().as_bytes());
        b.extend(&bytes[end..]);
        out.push(("startxref-moved", b));
    }
    if let Some(at) = find(b"/Filter /Standard") {
        let mut b = bytes.to_vec();
        let end = (at + 400).min(b.len());
        for j in (at + 17..end).step_by(23) {
            b[j] ^= 0x5A;
        }
        out.push(("encrypt-flipped", b));
    }
    for (key, name) in [(&b"/N "[..], "objstm-n"), (&b"/First "[..], "objstm-first")] {
        if find(b"/Type /ObjStm").is_some()
            && let Some(at) = find(key)
        {
            let mut b = bytes[..at + key.len()].to_vec();
            b.extend(b"999999999999");
            let rest = &bytes[at + key.len()..];
            b.extend(&rest[rest.iter().take_while(|c| c.is_ascii_digit()).count()..]);
            out.push((name, b));
        }
    }
    if let Some(at) = find(b"/Length ") {
        let mut b = bytes[..at + 8].to_vec();
        b.extend(b"99999999999");
        let rest = &bytes[at + 8..];
        b.extend(&rest[rest.iter().take_while(|c| c.is_ascii_digit()).count()..]);
        out.push(("length-huge", b));
    }
    out
}

/// #2256: an encrypted file whose handler PdfCraft does not support (here
/// revision 9) answers why, never the password prompt: only a file that
/// needs a password, or a wrong one, is locked.
#[test]
fn an_unsupported_encryption_is_a_failure_not_a_prompt() {
    let dir = Scratch::new();
    let bytes = std::fs::read(corpus_path("enc-aes-128")).unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(text.contains("/V 4 /R 4"));
    let path = dir.path("unsupported.pdf");
    let changed: Vec<u8> = {
        let at = bytes.windows(9).position(|w| w == b"/V 4 /R 4").unwrap();
        let mut b = bytes.clone();
        b[at..at + 9].copy_from_slice(b"/V 4 /R 9");
        b
    };
    std::fs::write(&path, changed).unwrap();
    let mut w = Worker::new(&path, Spawn::InProcess, LONG);
    match w.ask(&Op::Info) {
        Answer::Failed(why) => assert!(!why.trim().is_empty()),
        other => panic!("{other:?}"),
    }
}

/// Criterion 11: every corpus file and 21 seeded mutations of each through
/// a real child: `info`, then the first three pages' pixels and text. Each
/// answer is an answer or a failure that says why, within the timeout,
/// and the parent never panics. The slice 3 files (#2256) too, an
/// encrypted one and its mutations unlocked with its password, and each
/// file's structural mutations ([`structural`]).
#[test]
fn no_corpus_file_or_mutation_crashes_or_hangs_the_parent() {
    let dir = Scratch::new();
    let mut files = Vec::new();
    let names = CORPUS
        .iter()
        .chain(&UNREFERENCED)
        .chain(&HARD)
        .chain(std::iter::once(&UNREADABLE));
    for (n, name) in names.enumerate() {
        let src = corpus_path(name);
        let bytes = std::fs::read(&src).unwrap();
        let pw = user_password(name);
        files.push((src, pw, true));
        let mut rng = Rng(0x2245_0000 + n as u64 * 7919 + 1);
        for k in 0..MUTATIONS {
            let p = dir.path(&format!("{name}-{k}.pdf"));
            std::fs::write(&p, mutate(&bytes, k, &mut rng)).unwrap();
            files.push((p, pw, k < OUTPUT_MUTATIONS));
        }
        for (what, b) in structural(&bytes) {
            let p = dir.path(&format!("{name}-{what}.pdf"));
            std::fs::write(&p, b).unwrap();
            files.push((p, pw, true));
        }
    }
    // Saves go here, never next to a corpus file; and print jobs (#2258).
    let saves = dir.path("saved");
    std::fs::create_dir_all(&saves).unwrap();
    let jobs = dir.path("jobs");
    let files = Arc::new(Mutex::new(files));
    let tally = Arc::new(Mutex::new((0usize, 0usize, Vec::<String>::new())));
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let (files, tally, saves, jobs) =
                (files.clone(), tally.clone(), saves.clone(), jobs.clone());
            std::thread::spawn(move || {
                loop {
                    let Some((path, pw, outputs)) = files.lock().unwrap().pop() else {
                        return;
                    };
                    let mut w =
                        Worker::new(&path, test_child(&path, FUZZ_TIMEOUT, None), FUZZ_TIMEOUT);
                    let mut ask = |op: Op| {
                        let t0 = Instant::now();
                        let a = match &op {
                            Op::Unlock { password } => w.unlock(password.clone()),
                            op => w.ask(op),
                        };
                        let took = t0.elapsed();
                        let mut t = tally.lock().unwrap();
                        if took > op.timeout(FUZZ_TIMEOUT) + Duration::from_secs(5) {
                            t.2.push(format!("{}: {op:?} took {took:?}", path.display()));
                        }
                        match &a {
                            Answer::Failed(why) if why.trim().is_empty() => {
                                t.2.push(format!(
                                    "{}: {op:?} failed saying nothing",
                                    path.display()
                                ));
                            }
                            Answer::Failed(_) => t.1 += 1,
                            _ => t.0 += 1,
                        }
                        a
                    };
                    let m = match ask(Op::Info) {
                        Answer::Info(m) => m,
                        // Encrypted (#2256): its password, if it has one.
                        Answer::Locked { .. } => {
                            let Some(pw) = pw else {
                                tally.lock().unwrap().2.push(format!(
                                    "{}: locked, and not an encrypted file",
                                    path.display()
                                ));
                                continue;
                            };
                            match ask(Op::Unlock {
                                password: Secret::new(pw.into()),
                            }) {
                                Answer::Info(m) => m,
                                _ => continue,
                            }
                        }
                        _ => continue,
                    };
                    for page in 0..m.pages.len().min(3) {
                        ask(Op::Pixels { page, scale: 0.5 });
                        ask(Op::Text { page });
                    }
                    // Printed (to a folder) and exported, its first page
                    // as a PNG and as a PDF (#2258): the originals and the
                    // first `OUTPUT_MUTATIONS` mutations of each.
                    if outputs {
                        ask(Op::Print {
                            job: crate::pdf_view::print::tests::job(
                                vec![0],
                                Sizing::Fit,
                                Spool::Dir { dir: jobs.clone() },
                            ),
                        });
                        ask(Op::Png { page: 0, dpi: 72. });
                        ask(Op::ExtractPages { pages: vec![0] });
                    }
                    // A form's mutations are filled and saved too (#2255),
                    // to a file of their own in the scratch folder: the
                    // bytes, whatever they are.
                    if let Some(f) = m.form.fields.iter().find(|f| f.fillable()) {
                        let value = match f.kind {
                            FieldKind::Check | FieldKind::Radio => FieldValue::Widget { index: 0 },
                            FieldKind::Combo | FieldKind::List => {
                                FieldValue::Choice { picked: vec![0] }
                            }
                            _ => FieldValue::Text { text: "x".into() },
                        };
                        ask(Op::SetField {
                            name: f.name.clone(),
                            value,
                        });
                        ask(Op::SaveTo {
                            path: saves.join(path.file_name().unwrap_or_default()),
                        });
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("a fuzz thread panicked");
    }
    let (ok, failed, problems) = &*tally.lock().unwrap();
    eprintln!("mutated corpus: {ok} answered, {failed} failed with a reason");
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(*ok > 0);
}

// ---- encrypted, incremental and damaged files (#2256) -----------------------------

fn in_process(name: &str) -> Worker {
    Worker::new(&corpus_path(name), Spawn::InProcess, LONG)
}

fn info_of(w: &mut Worker) -> DocMeta {
    match w.ask(&Op::Info) {
        Answer::Info(m) => m,
        other => panic!("{other:?}"),
    }
}

fn unlocked(w: &mut Worker, password: &str) -> DocMeta {
    match w.unlock(Secret::new(password.into())) {
        Answer::Info(m) => m,
        other => panic!("{password}: {other:?}"),
    }
}

/// The page drawn and its text read, after an unlock.
fn reads(w: &mut Worker, want: &str) {
    match w.ask(&Op::Pixels {
        page: 0,
        scale: 0.5,
    }) {
        Answer::Pixels(r) => assert!(r.bgra.iter().any(|b| *b < 128), "a blank page"),
        other => panic!("{other:?}"),
    }
    match w.ask(&Op::Text { page: 0 }) {
        Answer::Text(t) => assert!(t.plain_text().contains(want), "{:?}", t.plain_text()),
        other => panic!("{other:?}"),
    }
}

/// Criterion 1: each cipher's file answers locked without a password and
/// with a wrong one, and opens with its user password: its page drawn and
/// read, its cipher named, what it withholds known.
#[test]
fn an_encrypted_file_is_locked_until_its_password() {
    for (name, user, _, method, label) in ENCRYPTED.iter().filter(|e| !e.1.is_empty()) {
        let mut w = in_process(name);
        match w.ask(&Op::Info) {
            Answer::Locked { wrong: false } => {}
            other => panic!("{name}: {other:?}"),
        }
        // Nothing but its password reads it.
        match w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5,
        }) {
            Answer::Failed(_) => {}
            other => panic!("{name}: drawn without a password: {other:?}"),
        }
        match w.unlock(Secret::new("not-the-password".into())) {
            Answer::Locked { wrong: true } => {}
            other => panic!("{name}: {other:?}"),
        }
        let m = unlocked(&mut w, user);
        assert_eq!(m.pages.len(), 1, "{name}");
        let sec = m.security.expect("its security");
        assert_eq!(&sec.method, method, "{name}");
        assert!(!sec.owner);
        assert_eq!(
            sec.restrictions(),
            vec!["printing", "copying", "changes"],
            "{name}"
        );
        reads(&mut w, label);
    }
}

/// Criterion 2: the owner password opens each file with nothing withheld:
/// RC4 and AES-128 (revisions 2-4) through the user password owner
/// authentication recovers, which is all PdfCraft's renderer takes for
/// them; AES-256 (revision 6) by the renderer's own owner path.
#[test]
fn the_owner_password_lifts_the_restrictions() {
    for (name, _, owner, _, label) in ENCRYPTED.iter().filter(|e| !e.1.is_empty()) {
        let mut w = in_process(name);
        let m = unlocked(&mut w, owner);
        let sec = m.security.expect("its security");
        assert!(sec.owner, "{name}");
        assert!(sec.print && sec.copy && sec.modify, "{name}");
        assert!(sec.restrictions().is_empty(), "{name}");
        reads(&mut w, label);
    }
}

/// Criterion 2: a file with an owner password only opens with no prompt,
/// and says what it withholds.
#[test]
fn an_owner_only_file_opens_with_its_permissions() {
    let mut w = in_process("enc-owner-only");
    let m = info_of(&mut w);
    let sec = m.security.expect("its security");
    assert_eq!(sec.method, "AES 128-bit");
    assert!(!sec.owner);
    assert!(!sec.print && !sec.copy && !sec.modify);
    assert!(sec.annotate && sec.fill_forms);
    reads(&mut w, "owner password only");
    // An unencrypted file has none.
    assert_eq!(info_of(&mut in_process("text")).security, None);
}

/// Criterion 3: the password goes over stdin only: the child finds it in
/// neither its command line nor its environment (it aborts if it does),
/// and no `Debug` print of a question or of the worker's answers holds it.
#[test]
fn the_password_is_never_in_the_childs_arguments_or_a_debug_print() {
    let (name, user, ..) = ENCRYPTED[3];
    let mut w = child_worker(name, Some("check-secret"), LONG, LONG);
    assert!(matches!(w.ask(&Op::Info), Answer::Locked { wrong: false }));
    let m = unlocked(&mut w, user);
    assert_eq!(w.started, 1, "the child did not abort");
    let op = Op::Unlock {
        password: Secret::new(user.into()),
    };
    assert_eq!(format!("{op:?}"), "Unlock { password: Secret(..) }");
    assert!(!format!("{m:?}").contains(user));
    // The wire has it, as a string.
    let line = serde_json::to_string(&Request { id: 1, op }).unwrap();
    assert!(line.contains(&format!("\"password\":\"{user}\"")), "{line}");
}

/// The forms corpus file encrypted (AES-128) with these passwords, in `dir`.
fn encrypted_forms(dir: &Scratch, user: &str, owner: &str) -> std::path::PathBuf {
    let path = dir.path("enc-forms.pdf");
    crate::pdf_view::form_tests::encrypted_copy(&corpus_path("forms"), &path, user, owner, -4);
    path
}

fn fill_name(w: &mut Worker) {
    let set = Op::SetField {
        name: "name".into(),
        value: FieldValue::Text { text: "Ada".into() },
    };
    assert!(matches!(w.ask(&set), Answer::Form(_)));
}

/// Criterion 4: a child killed after the unlock is replaced, and the new
/// one is unlocked before the form values are given again: they take (a
/// locked file would refuse them), and its page draws.
#[test]
fn a_new_child_is_unlocked_before_the_form_replay() {
    let dir = Scratch::new();
    let path = encrypted_forms(&dir, "user-form", "owner-form");
    let mut w = Worker::new(&path, test_child(&path, LONG, Some("abort-on-text")), LONG);
    assert!(matches!(w.ask(&Op::Info), Answer::Locked { wrong: false }));
    unlocked(&mut w, "user-form");
    fill_name(&mut w);
    assert!(matches!(w.ask(&Op::Text { page: 0 }), Answer::Failed(_)));
    assert_eq!(name_of(&mut w), "Ada", "replayed into an unlocked child");
    assert!(!w.take_lost());
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    assert_eq!(w.started, 2);
}

/// Criterion 4: when the new child dies taking the values again, the clean
/// child started after it is unlocked too: the next page draws.
#[test]
fn the_clean_child_after_a_failed_replay_is_unlocked_too() {
    let dir = Scratch::new();
    let path = encrypted_forms(&dir, "user-form", "owner-form");
    let mode = Some("abort-on-text abort-on-replay");
    let mut w = Worker::new(&path, test_child(&path, LONG, mode), LONG);
    unlocked(&mut w, "user-form");
    fill_name(&mut w);
    assert!(matches!(w.ask(&Op::Text { page: 0 }), Answer::Failed(_)));
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    assert!(w.take_lost());
    assert_eq!(w.started, 3);
}

/// Criterion 4, with no form: an encrypted page's child dies, and the next
/// question's child is unlocked from the held password, never asking again.
#[test]
fn a_child_killed_after_the_unlock_is_replaced_unlocked() {
    let (name, user, ..) = ENCRYPTED[2];
    let mut w = child_worker(name, Some("abort-on-text"), LONG, LONG);
    unlocked(&mut w, user);
    assert!(matches!(w.ask(&Op::Text { page: 0 }), Answer::Failed(_)));
    match w.ask(&Op::Pixels {
        page: 0,
        scale: 0.5,
    }) {
        Answer::Pixels(_) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(w.started, 2);
}

/// Criterion 11: on Linux the child limits its own memory: past
/// `MEMORY_LIMIT` an allocation fails and the child aborts (no core); the
/// parent answers that page with why, and the next page draws in a new
/// child. Without the limit the allocation (never touched) would succeed
/// and the page would draw.
#[cfg(target_os = "linux")]
#[test]
fn a_child_past_its_memory_budget_fails_with_a_reason() {
    let mut w = child_worker("text", Some("hog"), LONG, LONG);
    match w.ask(&Op::Pixels {
        page: 0,
        scale: 0.5,
    }) {
        Answer::Failed(why) => assert_eq!(why, DIED),
        other => panic!("the allocation was not refused: {other:?}"),
    }
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 1,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    assert_eq!(w.started, 2);
}

/// Criterion 11: the child's own limits, as its `run` set them: no core
/// file, and `MEMORY_LIMIT` of data (it aborts on `info` otherwise). It is
/// started with a core limit of its own (a shell's `ulimit`), as a desktop
/// session may give one: many default to 0 already, which would prove
/// nothing.
#[cfg(target_os = "linux")]
#[test]
fn the_child_runs_with_no_core_and_its_data_limit() {
    let path = corpus_path("text");
    let spec = format!("{}|{}", path.display(), LONG.as_millis());
    let spawn = Spawn::Command(Arc::new(move || {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("ulimit -S -c 1024 2>/dev/null || ulimit -c 1024; exec \"$0\" \"$@\"")
            .arg(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "pdf_view::worker::tests::child_entry",
                "--nocapture",
            ])
            .env(CHILD_ENV, &spec)
            .env(MODE_ENV, "limits");
        cmd
    }));
    let mut w = Worker::new(&path, spawn, LONG);
    assert!(matches!(w.ask(&Op::Info), Answer::Info(_)));
    assert_eq!(w.started, 1);
}

/// A zlib stream of `len` zero bytes, about 600 times smaller (one fixed
/// Huffman block of 258-byte copies, as a Flate bomb is made).
fn zero_bomb(len: usize) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl Bits {
        fn put(&mut self, v: u64, n: u32) {
            self.acc |= v << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
        /// A Huffman code: its most significant bit first.
        fn code(&mut self, c: u32, n: u32) {
            let r = (0..n).fold(0u32, |r, i| r | (((c >> i) & 1) << (n - 1 - i)));
            self.put(u64::from(r), n);
        }
    }
    let mut b = Bits {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    b.put(1, 1); // the last block
    b.put(1, 2); // fixed Huffman codes
    b.code(0x30, 8); // a literal 0
    let mut left = len - 1;
    while left >= 258 {
        b.code(0b1100_0101, 8); // length 258 (symbol 285)
        b.code(0, 5); // distance 1
        left -= 258;
    }
    for _ in 0..left {
        b.code(0x30, 8);
    }
    b.code(0, 7); // end of block
    if b.n > 0 {
        b.out.push(b.acc as u8);
    }
    let mut z = vec![0x78, 0x01];
    z.extend(b.out);
    // Adler-32 of zeros: a stays 1, b counts them.
    z.extend(((((len % 65521) as u32) << 16) | 1).to_be_bytes());
    z
}

/// A PDF of `objs` (each body without its `n 0 obj`), object 1 its
/// catalog, with a classic cross-reference table.
fn raw_pdf(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend(data);
    o.extend(b"\nendstream");
    o
}

/// Criterion 11: Flate bombs (a few MB inflating past every decode limit,
/// `LOAD_STREAM_LIMIT` and hayro's 256 MB) as a page's content and as an
/// object stream, through a real child: each answer comes within its time,
/// a page or why not.
#[test]
fn a_flate_bomb_answers_in_time() {
    let dir = Scratch::new();
    let bomb = zero_bomb(600 << 20);
    assert!(bomb.len() < 5 << 20, "{}", bomb.len());
    let content = raw_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        stream("/Filter /FlateDecode", &bomb),
    ]);
    // The catalog in an object stream that inflates past the limit, found
    // through a cross-reference stream.
    let mut objstm = b"%PDF-1.5\n".to_vec();
    let at_pages = objstm.len();
    objstm.extend(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let at_page = objstm.len();
    objstm.extend(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n");
    let at_stm = objstm.len();
    objstm.extend(b"4 0 obj\n");
    objstm.extend(stream(
        "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode",
        &bomb,
    ));
    objstm.extend(b"\nendobj\n");
    let at_xref = objstm.len();
    let mut rows = Vec::new();
    for (kind, a, b) in [
        (0u8, 0u32, 65535u16),
        (2, 4, 0),
        (1, at_pages as u32, 0),
        (1, at_page as u32, 0),
        (1, at_stm as u32, 0),
        (1, at_xref as u32, 0),
    ] {
        rows.push(kind);
        rows.extend(a.to_be_bytes());
        rows.extend(b.to_be_bytes());
    }
    objstm.extend(b"5 0 obj\n");
    objstm.extend(stream("/Type /XRef /Size 6 /W [1 4 2] /Root 1 0 R", &rows));
    objstm.extend(format!("\nendobj\nstartxref\n{at_xref}\n%%EOF\n").as_bytes());
    let budget = Duration::from_secs(20);
    for (name, bytes) in [("content-bomb.pdf", content), ("objstm-bomb.pdf", objstm)] {
        let path = dir.path(name);
        std::fs::write(&path, bytes).unwrap();
        let mut w = Worker::new(&path, test_child(&path, budget, None), budget);
        for op in [
            Op::Info,
            Op::Pixels { page: 0, scale: 1. },
            Op::Text { page: 0 },
        ] {
            let t0 = Instant::now();
            let a = w.ask(&op);
            assert!(
                t0.elapsed() < budget + Duration::from_secs(5),
                "{name}: {op:?} took {:?}",
                t0.elapsed()
            );
            if let Answer::Failed(why) = &a {
                assert!(!why.trim().is_empty(), "{name}: {op:?}");
            }
        }
    }
}

// ---- print and export (#2258) -------------------------------------------------

/// Criterion 7: a print gets its own budget through a real child: one that
/// takes longer than a page may (1.5 × the child's timeout) is answered,
/// not killed, while one that never answers is given up on at its own
/// deadline, and the next question starts a new child that answers.
#[test]
fn a_print_past_a_pages_timeout_is_answered_and_a_hung_one_given_up() {
    let dir = Scratch::new();
    let spool = Spool::Dir {
        dir: dir.path("jobs"),
    };
    // Wide enough for a child that starts slowly (Windows): the slow print
    // sleeps 1.5 t of the 3 t it may take.
    let t = Duration::from_secs(3);
    let mut w = child_worker("text", Some("slow-print"), t, t);
    let job = crate::pdf_view::print::tests::job(vec![0], Sizing::Fit, spool.clone());
    let t0 = Instant::now();
    match w.ask(&Op::Print { job: job.clone() }) {
        Answer::Printed(p) => assert_eq!(p.sheets, 1),
        other => panic!("{other:?}"),
    }
    assert!(t0.elapsed() > t, "it ran past a page's timeout");
    assert_eq!(w.started, 1, "not killed");
    let mut w = child_worker("text", Some("hang-print"), t, t);
    let t0 = Instant::now();
    match w.ask(&Op::Print { job }) {
        Answer::Failed(why) => assert_eq!(why, TOO_LONG),
        other => panic!("{other:?}"),
    }
    let took = t0.elapsed();
    assert!(
        took >= t * 3 && took < t * 3 + Duration::from_secs(5),
        "{took:?}"
    );
    // Only the hung print is bounded: the new child may start slowly.
    w.timeout = LONG;
    assert!(matches!(
        w.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    assert_eq!(w.started, 2);
}
