//! `docxy-pdf` end to end (#2259, criterion 3): the built binary run as the
//! child terminal docxy spawns, on the PDF corpus and on mutations of it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../corpus/pdf-view/{name}.pdf"))
}

/// What the child said: an answer line and the bytes after it.
type Said = (serde_json::Value, Vec<u8>);

/// The child, its stdin, and its answers read on a thread of their own so
/// a wait can time out.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    answers: mpsc::Receiver<Result<Said, String>>,
}

impl Running {
    fn start(file: &Path, timeout_ms: u64) -> Running {
        let mut child = Command::new(env!("CARGO_BIN_EXE_docxy-pdf"))
            .arg(file)
            .arg(timeout_ms.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("docxy-pdf starts");
        let stdin = child.stdin.take();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (tx, answers) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut line = String::new();
                match out.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.send(Err("end of output".into()));
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                    Ok(_) => {}
                }
                let head: serde_json::Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(Err(format!("not JSON: {e}: {line:?}")));
                        return;
                    }
                };
                let len = head["len"].as_u64().unwrap_or(0) as usize;
                let mut body = vec![0; len];
                if out.read_exact(&mut body).is_err() {
                    let _ = tx.send(Err("cut short".into()));
                    return;
                }
                if tx.send(Ok((head, body))).is_err() {
                    return;
                }
            }
        });
        Running {
            child,
            stdin,
            answers,
        }
    }

    fn next(&self, within: Duration) -> Result<Said, String> {
        self.answers
            .recv_timeout(within)
            .map_err(|_| "no answer in time".to_string())?
    }

    fn ask(&mut self, req: &str, within: Duration) -> Result<Said, String> {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{req}").map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        self.next(within)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const SOON: Duration = Duration::from_secs(60);

#[test]
fn the_child_says_hello_and_answers_info_pixels_and_text() {
    let mut c = Running::start(&corpus("slides"), 20_000);
    let (hello, _) = c.next(SOON).unwrap();
    assert_eq!(
        hello,
        serde_json::json!({"hello": "docxy-pdf", "protocol": 1})
    );

    let (info, _) = c.ask(r#"{"op":"info"}"#, SOON).unwrap();
    assert_eq!(info["ok"], true, "{info}");
    assert_eq!(info["pages"].as_array().unwrap().len(), 3);
    assert_eq!(info["pages"][2]["h"].as_f64().unwrap().round(), 540.);
    assert!(
        info["outline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["title"] == "Next steps")
    );

    let (px, bytes) = c
        .ask(r#"{"op":"pixels","page":0,"scale":0.5}"#, SOON)
        .unwrap();
    assert_eq!(
        (px["width"].as_u64(), px["height"].as_u64()),
        (Some(480), Some(270))
    );
    assert_eq!(bytes.len(), 480 * 270 * 4);
    assert!(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[..3] != [255, 255, 255]),
        "not blank"
    );

    let (text, _) = c.ask(r#"{"op":"text","page":1}"#, SOON).unwrap();
    assert!(
        text["text"].as_str().unwrap().contains("Revenue by region"),
        "{text}"
    );

    // A bad page, a bad request and an unknown op answer why; the child goes on.
    let (bad, _) = c
        .ask(r#"{"op":"pixels","page":3,"scale":1}"#, SOON)
        .unwrap();
    assert_eq!(bad["error"], "the PDF has no page 4");
    let (bad, _) = c.ask("not json", SOON).unwrap();
    assert_eq!(bad["ok"], false);
    let (bad, _) = c.ask(r#"{"op":"print"}"#, SOON).unwrap();
    assert_eq!(bad["ok"], false);
    let (text, _) = c.ask(r#"{"op":"text","page":0}"#, SOON).unwrap();
    assert_eq!(text["ok"], true);

    // The end of its input ends it.
    drop(c.stdin.take());
    let start = Instant::now();
    loop {
        if let Some(status) = c.child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(start.elapsed() < SOON, "the child did not end");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_locked_or_missing_file_answers_why() {
    let mut c = Running::start(&corpus("enc-aes-256"), 20_000);
    c.next(SOON).unwrap();
    let (info, _) = c.ask(r#"{"op":"info"}"#, SOON).unwrap();
    assert_eq!(info["locked"], true, "{info}");

    let mut c = Running::start(Path::new("/nonexistent/docxy-pdf-test.pdf"), 20_000);
    c.next(SOON).unwrap();
    let (info, _) = c.ask(r#"{"op":"info"}"#, SOON).unwrap();
    assert_eq!(info["ok"], false);
    assert!(
        info["error"]
            .as_str()
            .unwrap()
            .starts_with("the file could not be read"),
        "{info}"
    );
}

#[test]
fn a_bad_command_line_says_how_to_use_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_docxy-pdf"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: docxy-pdf"));
}

/// On Linux the child sets its own data and core limits, as the suite's
/// worker does: read back from `/proc/<pid>/limits` once it has said hello.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_the_child_limits_its_memory_and_writes_no_core() {
    let c = Running::start(&corpus("text"), 20_000);
    c.next(SOON).unwrap();
    let limits = std::fs::read_to_string(format!("/proc/{}/limits", c.child.id())).unwrap();
    let row = |name: &str| {
        limits
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("{name} in {limits}"))
            .split_whitespace()
            .rev()
            .skip(1)
            .take(2)
            .collect::<Vec<_>>()
    };
    let data = (2u64 << 30).to_string();
    assert_eq!(row("Max data size"), vec![data.as_str(), data.as_str()]);
    assert_eq!(row("Max core file size"), vec!["0", "0"]);
    let adj = std::fs::read_to_string(format!("/proc/{}/oom_score_adj", c.child.id())).unwrap();
    assert_eq!(adj.trim(), "1000");
}

/// A small deterministic generator, so a failure names its mutation.
struct Seeded(u64);

impl Seeded {
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

/// Mutation `k` of `bytes`: truncated, bytes flipped, or a span zeroed.
fn mutate(bytes: &[u8], k: u64) -> Vec<u8> {
    let mut rng = Seeded(0x9E37_79B9_7F4A_7C15 ^ (k + 1).wrapping_mul(0x2545_F491_4F6C_DD1D));
    let mut out = bytes.to_vec();
    match k % 3 {
        0 => out.truncate(rng.below(bytes.len())),
        1 => {
            for _ in 0..1 + rng.below(16) {
                let i = rng.below(out.len());
                out[i] ^= 1 << rng.below(8);
            }
        }
        _ => {
            let at = rng.below(out.len());
            let n = 1 + rng.below(512);
            let end = (at + n).min(out.len());
            out[at..end].fill(0);
        }
    }
    out
}

/// Criterion 3: each corpus file and seeded mutations of it, through a real
/// child: `info`, page 1's pixels and its text are each an answer or a
/// failure with a reason within the parent's wait; a child that dies is a
/// failure too, never a hang.
#[test]
fn no_corpus_file_or_mutation_hangs_or_answers_nonsense() {
    const MUTATIONS: u64 = 21;
    let dir = std::env::temp_dir().join(format!("pdfpages-mutations-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(corpus("text").parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .collect();
    files.sort();
    assert!(files.len() >= 20, "the corpus: {files:?}");
    let wait = Duration::from_secs(25);
    let mut report = Vec::new();
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let stem = file.file_stem().unwrap().to_string_lossy().into_owned();
        for k in 0..=MUTATIONS {
            let (path, what) = if k == 0 {
                (file.clone(), stem.clone())
            } else {
                let p = dir.join(format!("{stem}-{k}.pdf"));
                std::fs::write(&p, mutate(&bytes, k - 1)).unwrap();
                (p, format!("{stem} mutation {}", k - 1))
            };
            let start = Instant::now();
            let mut c = Running::start(&path, 10_000);
            c.next(wait)
                .unwrap_or_else(|e| panic!("{what}: hello: {e}"));
            let mut outcome = Vec::new();
            for req in [
                r#"{"op":"info"}"#,
                r#"{"op":"pixels","page":0,"scale":1}"#,
                r#"{"op":"text","page":0}"#,
            ] {
                match c.ask(req, wait) {
                    Ok((head, body)) => {
                        let ok = head["ok"].as_bool();
                        assert!(ok.is_some(), "{what}: {req}: no ok in {head}");
                        if ok == Some(false) {
                            assert!(
                                head["error"].as_str().is_some_and(|e| !e.is_empty()),
                                "{what}: {req}: a failure without why: {head}"
                            );
                        }
                        if req.contains("pixels") && ok == Some(true) {
                            let (w, h) = (head["width"].as_u64(), head["height"].as_u64());
                            assert_eq!(
                                Some(body.len() as u64),
                                w.zip(h).map(|(w, h)| w * h * 4),
                                "{what}: the pixels and their size"
                            );
                        }
                        outcome.push(if ok == Some(true) { "ok" } else { "failed" });
                    }
                    Err(e) if e == "no answer in time" => panic!("{what}: {req}: hung"),
                    // Output that is no answer is the child's bug, not a death.
                    Err(e) if e.starts_with("not JSON") || e == "cut short" => {
                        panic!("{what}: {req}: {e}")
                    }
                    // The child died (an abort past its memory limit): its
                    // output ended, or reading it failed. The parent fails
                    // the page; the next request starts another.
                    Err(_) => {
                        outcome.push("died");
                        break;
                    }
                }
            }
            report.push(format!("{what}: {outcome:?} in {:?}", start.elapsed()));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{}", report.join("\n"));
}
