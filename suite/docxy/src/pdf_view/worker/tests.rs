//! The worker through a real child process (#2245): this test binary run
//! as the child (`--exact pdf_view::worker::tests::child_entry`), the way
//! `convert_child`'s tests run it. Misbehaving children (hang, abort, exit,
//! nonsense) show a page fails and the next question starts a new child;
//! the corpus and mutated copies of it show no file crashes or hangs the
//! parent.

use super::*;
use crate::open_mode_tests::Scratch;
use crate::pdf_view::PdfView;
use crate::pdf_view::tests::{CORPUS, UNREFERENCED, corpus_path};
use std::path::Path;
use std::time::{Duration, Instant};

/// Set to `<file>|<timeout ms>` to make the test binary a worker child.
const CHILD_ENV: &str = "DOCXY_PDF_WORKER_TEST_CHILD";
/// How the child misbehaves: `hang-pixels`, `abort-page-2`,
/// `garbage-on-text`; `abort-on-text` and `abort-on-replay` (#2255) abort
/// on any text and on the form values given again, and a mode may name
/// both.
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
fn test_child(path: &Path, child_timeout: Duration, mode: Option<&'static str>) -> Spawn {
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
/// The child's and the parent's timeout for the mutated files.
const FUZZ_TIMEOUT: Duration = Duration::from_secs(5);

/// Criterion 11: every corpus file and 21 seeded mutations of each through
/// a real child: `info`, then the first three pages' pixels and text. Each
/// answer is an answer or a failure that says why, within the timeout,
/// and the parent never panics.
#[test]
fn no_corpus_file_or_mutation_crashes_or_hangs_the_parent() {
    let dir = Scratch::new();
    let mut files = Vec::new();
    for (n, name) in CORPUS.iter().chain(&UNREFERENCED).enumerate() {
        let src = corpus_path(name);
        let bytes = std::fs::read(&src).unwrap();
        files.push(src);
        let mut rng = Rng(0x2245_0000 + n as u64 * 7919 + 1);
        for k in 0..MUTATIONS {
            let p = dir.path(&format!("{name}-{k}.pdf"));
            std::fs::write(&p, mutate(&bytes, k, &mut rng)).unwrap();
            files.push(p);
        }
    }
    // Saves go here, never next to a corpus file.
    let saves = dir.path("saved");
    std::fs::create_dir_all(&saves).unwrap();
    let files = Arc::new(Mutex::new(files));
    let tally = Arc::new(Mutex::new((0usize, 0usize, Vec::<String>::new())));
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let (files, tally, saves) = (files.clone(), tally.clone(), saves.clone());
            std::thread::spawn(move || {
                loop {
                    let Some(path) = files.lock().unwrap().pop() else {
                        return;
                    };
                    let mut w =
                        Worker::new(&path, test_child(&path, FUZZ_TIMEOUT, None), FUZZ_TIMEOUT);
                    let mut ask = |op: Op| {
                        let t0 = Instant::now();
                        let a = w.ask(&op);
                        let took = t0.elapsed();
                        let mut t = tally.lock().unwrap();
                        if took > FUZZ_TIMEOUT + Duration::from_secs(5) {
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
                    let Answer::Info(m) = ask(Op::Info) else {
                        continue;
                    };
                    for page in 0..m.pages.len().min(3) {
                        ask(Op::Pixels { page, scale: 0.5 });
                        ask(Op::Text { page });
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
