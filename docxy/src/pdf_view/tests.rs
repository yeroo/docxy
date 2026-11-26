//! Terminal docxy's PDF viewer (#2259): what it asks, what it shows, its
//! keys and verbs with fake answers; its child client against a fake child
//! (this test binary, run again as one); and, ignored unless `DOCXY_PDF`
//! names a built `docxy-pdf`, against the real child.

use super::child::{self, Ans, Child, Meta, PageMeta, Raster, Req, Source, TextOnly};
use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::time::Duration;
use unicode_width::UnicodeWidthChar;

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../corpus/pdf-view/{name}.pdf"))
}

fn meta(n: usize) -> Meta {
    Meta {
        pages: (0..n)
            .map(|i| PageMeta {
                w: 600.,
                h: 800.,
                label: if i == 0 { "i".into() } else { format!("{i}") },
            })
            .collect(),
        outline: vec![("Start".into(), Some(0), 0)],
    }
}

/// A halfblocks picker with 10 x 20 pixel cells: a terminal with no
/// picture protocol.
fn plain() -> Picker {
    Picker::from_fontsize((10, 20))
}

/// The same cells, with kitty's protocol.
fn graphics() -> Picker {
    let mut p = Picker::from_fontsize((10, 20));
    p.set_protocol_type(ProtocolType::Kitty);
    p
}

/// A viewer that has its `n` pages, drawn once in an 80 x 24 terminal.
fn opened(picker: Picker, n: usize) -> (Viewer, Terminal<TestBackend>) {
    let mut v = Viewer::new("doc.pdf", Some(picker), None);
    assert_eq!(v.take_next(), Some(Req::Info));
    assert_eq!(v.on_answer(Req::Info, Ans::Info(meta(n))), Next::Nothing);
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    t.draw(|f| v.draw(f)).unwrap();
    (v, t)
}

fn screen(t: &Terminal<TestBackend>) -> String {
    let buf = t.backend().buffer();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn raster(w: u32, h: u32) -> Raster {
    let mut rgba = vec![255; (w * h * 4) as usize];
    // Something on it.
    rgba[0] = 0;
    Raster {
        width: w,
        height: h,
        rgba,
    }
}

// ---- opening -----------------------------------------------------------------

#[test]
fn a_pdf_opens_by_its_bytes_never_a_markdown_file_that_mentions_one() {
    let dir = std::env::temp_dir().join(format!("docxy-pdf-sniff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = std::fs::read(corpus("text")).unwrap();
    let named_docx = dir.join("report.docx");
    std::fs::write(&named_docx, &pdf).unwrap();
    let no_pdf = dir.join("fake.pdf");
    std::fs::write(&no_pdf, b"just text").unwrap();
    let md = dir.join("notes.md");
    std::fs::write(&md, b"# Notes\n\nA PDF starts with %PDF-1.7.\n").unwrap();
    let page = dir.join("page.html");
    std::fs::write(&page, b"<html><body>%PDF-1.7</body></html>").unwrap();
    let s = |p: &Path| p.to_str().unwrap().to_string();
    assert!(opens_as_pdf(&s(&corpus("text"))));
    assert!(opens_as_pdf(&s(&named_docx)), "a PDF named .docx");
    assert!(
        !opens_as_pdf(&s(&no_pdf)),
        "a .pdf that is no PDF goes the old way"
    );
    assert!(!opens_as_pdf(&s(&md)));
    assert!(!opens_as_pdf(&s(&page)));
    assert!(!opens_as_pdf(&s(&dir.join("missing.pdf"))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn headless_conversion_of_a_pdf_is_refused_with_why() {
    let why = headless_refusal("a.pdf");
    assert!(why.starts_with("a.pdf is a PDF"), "{why}");
    assert!(why.contains("docxy suite"));
}

#[test]
fn a_terminal_without_pictures_starts_in_text_and_one_with_them_in_pages() {
    assert_eq!(Viewer::new("a", Some(plain()), None).mode, Mode::Text);
    assert_eq!(Viewer::new("a", Some(graphics()), None).mode, Mode::Image);
    assert_eq!(Viewer::new("a", None, None).mode, Mode::Text);
}

// ---- what it asks ---------------------------------------------------------------

#[test]
fn info_first_then_the_shown_page_then_the_next_one_at_a_time() {
    let mut v = Viewer::new("doc.pdf", Some(plain()), None);
    assert_eq!(v.take_next(), Some(Req::Info));
    assert_eq!(v.take_next(), None, "one question at a time");
    v.on_answer(Req::Info, Ans::Info(meta(3)));
    assert_eq!(v.take_next(), Some(Req::Text { page: 0 }));
    v.on_answer(Req::Text { page: 0 }, Ans::Text("one".into()));
    assert_eq!(
        v.take_next(),
        Some(Req::Text { page: 1 }),
        "the next page ahead"
    );
    v.on_answer(Req::Text { page: 1 }, Ans::Text("two".into()));
    assert_eq!(v.take_next(), None);
    v.on_key(key(KeyCode::End));
    assert_eq!(v.take_next(), Some(Req::Text { page: 2 }));
}

#[test]
fn pages_are_asked_at_the_body_size_fitted_to_the_page_or_the_width() {
    let mut v = Viewer::new("doc.pdf", Some(graphics()), None);
    v.take_next();
    v.on_answer(Req::Info, Ans::Info(meta(2)));
    assert_eq!(v.take_next(), None, "no size before the first frame");
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    t.draw(|f| v.draw(f)).unwrap();
    // The body is 80 x 22 cells of 10 x 20 px: 800 x 440 px; a 600 x 800 pt
    // page fits its height.
    let Some(Req::Pixels { page: 0, scale }) = v.take_next() else {
        panic!("pixels")
    };
    assert!((scale - 440. / 800.).abs() < 1e-4, "{scale}");
    v.on_answer(
        Req::Pixels { page: 0, scale },
        Ans::Pixels(raster(330, 440)),
    );
    t.draw(|f| v.draw(f)).unwrap();
    assert!(v.read().get("drawn").unwrap().as_bool().unwrap());
    // Fit width: the body's width.
    v.on_key(key(KeyCode::Char('w')));
    let Some(Req::Pixels { page: 0, scale }) = v.take_next() else {
        panic!("pixels")
    };
    assert!((scale - 800. / 600.).abs() < 1e-4, "{scale}");
}

#[test]
fn a_page_the_child_fails_on_says_why_and_is_never_asked_again() {
    let (mut v, mut t) = opened(graphics(), 2);
    let Some(req @ Req::Pixels { page: 0, .. }) = v.take_next() else {
        panic!("pixels")
    };
    v.on_answer(req, Ans::Failed(child::TOO_LONG.into()));
    t.draw(|f| v.draw(f)).unwrap();
    assert!(
        screen(&t).contains("This page can't be drawn: it took too long to draw"),
        "{}",
        screen(&t)
    );
    // The next page is asked, never page 1 again.
    assert!(matches!(v.take_next(), Some(Req::Pixels { page: 1, .. })));
    let r = v.read();
    assert_eq!(r.get("failed").unwrap().get_str("1"), Some(child::TOO_LONG));
}

#[test]
fn an_answer_to_another_question_is_ignored() {
    let (mut v, _) = opened(plain(), 2);
    assert_eq!(v.take_next(), Some(Req::Text { page: 0 }));
    v.on_answer(Req::Text { page: 1 }, Ans::Text("stale".into()));
    assert_eq!(v.take_next(), None, "still waiting for page 1's");
    v.on_answer(Req::Text { page: 0 }, Ans::Text("fresh".into()));
    assert_eq!(v.take_next(), Some(Req::Text { page: 1 }));
}

// ---- falling back and failing ------------------------------------------------------

#[test]
fn without_the_child_the_text_importer_shows_the_text_and_says_why() {
    let mut v = Viewer::new(
        "text.pdf",
        Some(graphics()),
        Some("docxy-pdf was not found".into()),
    );
    assert!(v.text_only);
    assert_eq!(v.mode, Mode::Text);
    assert!(v.status.contains("docxy-pdf was not found"), "{}", v.status);
    assert!(v.status.contains(HOW_TO_GET_PAGES));
    let mut source = TextOnly::new(corpus("text"));
    let req = v.take_next().unwrap();
    v.on_answer(req.clone(), source.ask(&req));
    let req = v.take_next().unwrap();
    assert_eq!(req, Req::Text { page: 0 });
    v.on_answer(req.clone(), source.ask(&req));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| v.draw(f)).unwrap();
    let s = screen(&t);
    assert!(s.contains("Привет, мир!"), "{s}");
    assert!(s.contains("text.pdf · text only"), "{s}");
    // `t` cannot show pages: it says how to get them.
    v.status.clear();
    v.on_key(key(KeyCode::Char('t')));
    assert_eq!(v.mode, Mode::Text);
    assert_eq!(
        v.status,
        format!("Text only: docxy-pdf was not found; {HOW_TO_GET_PAGES}")
    );
}

#[test]
fn a_child_that_cannot_be_started_falls_back_saying_how_to_get_one() {
    let mut v = Viewer::new("x.pdf", Some(plain()), None);
    v.take_next();
    assert_eq!(
        v.on_answer(Req::Info, Ans::NoChild("cannot exec".into())),
        Next::TextOnly {
            why: "cannot exec".into(),
            missing: true
        }
    );
    v.fall_back("cannot exec", true);
    assert!(v.status.contains(HOW_TO_GET_PAGES), "{}", v.status);
}

/// A file the child died on or ran out of time with is not read again in
/// docxy's own process: no text importer, the reason only.
#[test]
fn a_child_that_dies_or_times_out_opening_the_file_shows_no_text() {
    for why in [child::DIED, child::TOO_LONG] {
        let mut v = Viewer::new("x.pdf", Some(plain()), None);
        v.take_next();
        assert_eq!(
            v.on_answer(Req::Info, Ans::Failed(why.into())),
            Next::Nothing
        );
        assert!(!v.text_only);
        assert_eq!(
            v.unopened.as_deref(),
            Some(format!("This PDF can't be opened: {why}").as_str())
        );
        assert_eq!(v.take_next(), None, "nothing more is asked");
        let mut t = Terminal::new(TestBackend::new(80, 10)).unwrap();
        t.draw(|f| v.draw(f)).unwrap();
        assert!(
            screen(&t).contains("This PDF can't be opened"),
            "{}",
            screen(&t)
        );
    }
}

/// No control character of a page's text (or of a reason, or of the file
/// name) reaches a cell: an OSC or CSI in a CMap would act on the terminal.
#[test]
fn control_characters_in_the_text_never_reach_a_cell() {
    let nasty = "a\x1b]0;x\x07b\u{9b}31mc\td\r\ne\u{202E}f\u{200D}g\x7f";
    assert_eq!(
        clean_text(nasty),
        "a\u{FFFD}]0;x\u{FFFD}b\u{FFFD}31mc   d\ne\u{FFFD}f\u{200D}g\u{FFFD}"
            .replace("e\u{FFFD}f", "ef")
    );
    assert_eq!(clean_text("\tx\rab\ty"), "        x\nab      y");
    let mut v = Viewer::new("evil\x1b[2J.pdf", Some(plain()), None);
    v.take_next();
    v.on_answer(Req::Info, Ans::Info(meta(2)));
    let req = v.take_next().unwrap();
    v.on_answer(req, Ans::Text(format!("{nasty} {}", "word ".repeat(30))));
    v.status = "child said \x1b]52;c;aGk=\x07".into();
    let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
    t.draw(|f| v.draw(f)).unwrap();
    let buf = t.backend().buffer();
    for cell in buf.content() {
        assert!(
            !cell.symbol().chars().any(|c| c.is_control()),
            "{:?} in a cell",
            cell.symbol()
        );
    }
    for line in &v.shown {
        assert!(
            line.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() <= 40,
            "{line:?} is wider than the body"
        );
    }
    // A page that failed with such a reason is cleaned too.
    v.on_key(key(KeyCode::PageDown));
    let req = v.take_next().unwrap();
    v.on_answer(req, Ans::Failed("bad\x1b[31m".into()));
    t.draw(|f| v.draw(f)).unwrap();
    assert!(
        t.backend()
            .buffer()
            .content()
            .iter()
            .all(|c| !c.symbol().chars().any(|c| c.is_control()))
    );
}

#[test]
fn a_child_that_cannot_open_the_file_falls_back_to_its_text() {
    let mut v = Viewer::new("x.pdf", Some(plain()), None);
    v.take_next();
    assert_eq!(
        v.on_answer(Req::Info, Ans::Failed("no page tree".into())),
        Next::TextOnly {
            why: "no page tree".into(),
            missing: false
        }
    );
    v.fall_back("no page tree", false);
    assert!(v.text_only);
    // The child is there: the status gives its reason, not how to get it.
    assert_eq!(v.status, "Text only: no page tree");
    v.on_key(key(KeyCode::Char('t')));
    assert_eq!(v.status, "Text only: no page tree", "t repeats the reason");
    assert_eq!(
        v.take_next(),
        Some(Req::Info),
        "the importer is asked again"
    );
    // The importer failing too says why in the body.
    v.on_answer(Req::Info, Ans::Failed("not a PDF".into()));
    let mut t = Terminal::new(TestBackend::new(80, 10)).unwrap();
    t.draw(|f| v.draw(f)).unwrap();
    assert!(screen(&t).contains("This PDF can't be opened: not a PDF"));
    assert_eq!(v.take_next(), None);
}

#[test]
fn a_locked_file_says_to_open_it_in_the_suite() {
    let mut v = Viewer::new("x.pdf", Some(plain()), None);
    v.take_next();
    assert_eq!(v.on_answer(Req::Info, Ans::Locked), Next::Nothing);
    let mut t = Terminal::new(TestBackend::new(100, 10)).unwrap();
    t.draw(|f| v.draw(f)).unwrap();
    assert!(
        screen(&t).contains("protected by a password"),
        "{}",
        screen(&t)
    );
    assert_eq!(v.take_next(), None);
}

// ---- keys -------------------------------------------------------------------------------

#[test]
fn keys_turn_pages_within_the_document() {
    let (mut v, _) = opened(plain(), 3);
    for (k, page) in [
        (KeyCode::PageDown, 1),
        (KeyCode::Char(' '), 2),
        (KeyCode::Right, 2),
        (KeyCode::Char('p'), 1),
        (KeyCode::Left, 0),
        (KeyCode::PageUp, 0),
        (KeyCode::End, 2),
        (KeyCode::Home, 0),
        (KeyCode::Char('n'), 1),
    ] {
        assert!(!v.on_key(key(k)));
        assert_eq!(v.page, page, "{k:?}");
    }
    assert!(v.on_key(key(KeyCode::Char('q'))));
    assert!(v.on_key(key(KeyCode::Esc)));
    assert!(v.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)));
}

#[test]
fn g_goes_to_a_typed_page_and_says_when_there_is_none() {
    let (mut v, mut t) = opened(plain(), 12);
    v.on_key(key(KeyCode::Char('g')));
    assert_eq!(v.goto.as_deref(), Some(""));
    for c in ['1', 'x', '2'] {
        v.on_key(key(KeyCode::Char(c)));
    }
    assert_eq!(v.goto.as_deref(), Some("12"), "digits only");
    t.draw(|f| v.draw(f)).unwrap();
    assert!(screen(&t).contains("Go to page: 12"));
    let read = v
        .dispatch(
            "field-read",
            &Json::obj(vec![("input", Json::Str("docxy-pdf-page".into()))]),
        )
        .unwrap();
    assert_eq!(read.get_str("value"), Some("12"));
    assert_eq!(read.get("focused").and_then(Json::as_bool), Some(true));
    assert!(read.get("at").is_some_and(|a| *a != Json::Null));
    v.on_key(key(KeyCode::Enter));
    assert_eq!((v.page, v.goto.as_deref()), (11, None));
    v.on_key(key(KeyCode::Char('g')));
    v.on_key(key(KeyCode::Char('4')));
    v.on_key(key(KeyCode::Char('0')));
    v.on_key(key(KeyCode::Enter));
    assert_eq!(v.page, 11);
    assert_eq!(v.status, "There is no page 40: the PDF has 12");
    v.on_key(key(KeyCode::Char('g')));
    v.on_key(key(KeyCode::Char('3')));
    v.on_key(key(KeyCode::Esc));
    assert_eq!((v.page, v.goto.as_deref()), (11, None), "Esc closes it");
}

#[test]
fn t_switches_between_the_page_and_its_text_and_w_the_fit() {
    let (mut v, _) = opened(graphics(), 1);
    v.on_key(key(KeyCode::Char('t')));
    assert_eq!(v.mode, Mode::Text);
    v.on_key(key(KeyCode::Char('t')));
    assert_eq!(v.mode, Mode::Image);
    v.on_key(key(KeyCode::Char('w')));
    assert_eq!(v.fit, Fit::Width);
    v.on_key(key(KeyCode::Char('w')));
    assert_eq!(v.fit, Fit::Page);
}

#[test]
fn text_scrolls_within_its_lines_and_a_new_page_starts_at_the_top() {
    let (mut v, mut t) = opened(plain(), 2);
    let req = v.take_next().unwrap();
    let text: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    v.on_answer(req, Ans::Text(text));
    for _ in 0..100 {
        v.on_key(key(KeyCode::Down));
    }
    t.draw(|f| v.draw(f)).unwrap();
    // 41 lines (the last empty) in a 22-row body: at most 19 scrolled.
    assert_eq!(v.scroll, 19);
    assert!(screen(&t).contains("line 40"));
    v.on_key(key(KeyCode::PageDown));
    assert_eq!(v.scroll, 0);
}

#[test]
fn the_header_shows_the_name_the_page_and_its_label() {
    let (mut v, mut t) = opened(plain(), 3);
    t.draw(|f| v.draw(f)).unwrap();
    assert!(
        screen(&t).contains("doc.pdf · Page i (1 of 3) · text"),
        "{}",
        screen(&t)
    );
    v.on_key(key(KeyCode::PageDown));
    t.draw(|f| v.draw(f)).unwrap();
    // The label "1" differs from the number 2.
    assert!(screen(&t).contains("Page 1 (2 of 3)"));
}

// ---- verbs ------------------------------------------------------------------------------

#[test]
fn field_read_and_pdf_read_answer_and_other_verbs_are_refused() {
    let (mut v, mut t) = opened(plain(), 2);
    v.counts.keys = 3;
    t.draw(|f| v.draw(f)).unwrap();
    let all = v.dispatch("field-read", &Json::obj(vec![])).unwrap();
    assert_eq!(all.get_usize("keys"), Some(3));
    assert_eq!(all.get_usize("frames"), Some(2));
    let closed = v
        .dispatch(
            "field-read",
            &Json::obj(vec![("input", Json::Str("docxy-pdf-page".into()))]),
        )
        .unwrap();
    assert_eq!(closed.get("value"), Some(&Json::Null));
    let r = v.dispatch("pdf-read", &Json::obj(vec![])).unwrap();
    assert_eq!(r.get_usize("pages"), Some(2));
    assert_eq!(r.get_usize("page"), Some(1));
    assert_eq!(r.get_str("label"), Some("i"));
    assert_eq!(r.get_str("mode"), Some("text"));
    assert_eq!(r.get_str("source"), Some("child"));
    assert_eq!(
        r.get("outline").and_then(Json::as_array).map(<[Json]>::len),
        Some(1)
    );
    assert!(v.dispatch("doc.insert", &Json::obj(vec![])).is_err());
}

#[test]
fn wrap_breaks_at_spaces_by_display_width() {
    assert_eq!(wrap("one two three", 7), ["one two", "three"]);
    assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    // Wide characters take two columns.
    assert_eq!(wrap("日本語の文", 4), ["日本", "語の", "文"]);
    assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
    assert_eq!(wrap("", 5), [""]);
}

// ---- the child client ---------------------------------------------------------------------

#[test]
fn the_child_is_found_by_its_variable_beside_docxy_or_on_path() {
    let dir = std::env::temp_dir().join(format!("docxy-pdf-find-{}", std::process::id()));
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let name = format!("docxy-pdf{}", std::env::consts::EXE_SUFFIX);
    std::fs::write(bin.join(&name), b"").unwrap();
    let exe = Some(bin.join("docxy"));
    let other = Some(dir.join("elsewhere/docxy"));
    let path = Some(std::env::join_paths([dir.join("none"), bin.clone()]).unwrap());
    assert_eq!(
        child::find_child_in(None, exe, None).unwrap(),
        bin.join(&name)
    );
    assert_eq!(
        child::find_child_in(None, other.clone(), path).unwrap(),
        bin.join(&name)
    );
    assert_eq!(
        child::find_child_in(Some(bin.join(&name).into()), other.clone(), None).unwrap(),
        bin.join(&name)
    );
    let missing = child::find_child_in(Some(dir.join("nope").into()), other.clone(), None);
    assert!(missing.unwrap_err().contains("which is not there"));
    assert_eq!(
        child::find_child_in(None, other, None).unwrap_err(),
        "docxy-pdf was not found"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// This test binary run as a fake child: `fake_child` reads its mode from
/// its arguments (libtest takes them as more name filters, which match
/// nothing else).
fn fake(mode: &str, timeout: Duration) -> Child {
    Child::with(
        std::env::current_exe().unwrap(),
        vec![
            "pdf_view::tests::fake_child".into(),
            "--exact".into(),
            "--ignored".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ],
        PathBuf::from(format!("mode:{mode}")),
        timeout,
    )
}

/// The fake child: run only as one (`--ignored`, a `mode:` argument).
#[test]
#[ignore = "run as a child by the client's tests"]
fn fake_child() {
    use std::io::{BufRead, Write};
    let Some(mode) = std::env::args().find_map(|a| a.strip_prefix("mode:").map(String::from))
    else {
        return;
    };
    // `hang-once:<file>`: hang the first time (the file is absent), then serve.
    let mode = match mode.strip_prefix("hang-once:") {
        Some(flag) if Path::new(flag).exists() => "ok".to_string(),
        Some(flag) => {
            std::fs::write(flag, b"started").unwrap();
            "hang".to_string()
        }
        None => mode,
    };
    let out = std::io::stdout();
    let mut out = out.lock();
    if mode == "chatter" {
        // Lines forever, never the hello.
        loop {
            if writeln!(out, "not the hello")
                .and_then(|()| out.flush())
                .is_err()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    writeln!(out, "a line before the hello").unwrap();
    writeln!(out, r#"{{"hello":"docxy-pdf","protocol":1}}"#).unwrap();
    out.flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        match mode.as_str() {
            "hang" => std::thread::sleep(Duration::from_secs(600)),
            "die" => std::process::exit(9),
            "nonsense" => writeln!(out, "this is no answer").unwrap(),
            _ => {
                if line.contains("info") {
                    writeln!(
                        out,
                        r#"{{"ok":true,"pages":[{{"w":100,"h":50,"label":"1"}}],"outline":[]}}"#
                    )
                    .unwrap();
                } else if line.contains("pixels") {
                    writeln!(out, r#"{{"ok":true,"width":2,"height":1,"len":8}}"#).unwrap();
                    out.write_all(&[1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
                } else {
                    writeln!(out, r#"{{"ok":false,"error":"no text here"}}"#).unwrap();
                }
            }
        }
        out.flush().unwrap();
    }
}

#[test]
fn the_client_skips_lines_before_the_hello_and_reads_each_answer() {
    let mut c = fake("ok", Duration::from_secs(30));
    assert_eq!(
        c.ask(&Req::Info),
        Ans::Info(Meta {
            pages: vec![PageMeta {
                w: 100.,
                h: 50.,
                label: "1".into()
            }],
            outline: vec![],
        })
    );
    assert_eq!(
        c.ask(&Req::Pixels { page: 0, scale: 1. }),
        Ans::Pixels(Raster {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 255]
        })
    );
    assert_eq!(
        c.ask(&Req::Text { page: 0 }),
        Ans::Failed("no text here".into())
    );
}

#[test]
fn a_child_past_its_timeout_is_killed_and_the_next_question_starts_another() {
    let flag = std::env::temp_dir().join(format!("docxy-pdf-hang-once-{}", std::process::id()));
    let _ = std::fs::remove_file(&flag);
    let mut c = fake(
        &format!("hang-once:{}", flag.display()),
        Duration::from_secs(3),
    );
    let start = std::time::Instant::now();
    assert_eq!(c.ask(&Req::Info), Ans::Failed(child::TOO_LONG.into()));
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "{:?}",
        start.elapsed()
    );
    assert!(
        matches!(c.ask(&Req::Info), Ans::Info(_)),
        "a new child serves"
    );
    let _ = std::fs::remove_file(&flag);
}

#[test]
fn a_child_that_dies_or_answers_nonsense_fails_the_question() {
    let mut c = fake("die", Duration::from_secs(30));
    assert_eq!(c.ask(&Req::Info), Ans::Failed(child::DIED.into()));
    let mut c = fake("nonsense", Duration::from_secs(30));
    assert_eq!(
        c.ask(&Req::Text { page: 0 }),
        Ans::Failed(child::DIED.into())
    );
    let mut c = Child::with(
        PathBuf::from("/nonexistent/docxy-pdf"),
        vec![],
        corpus("text"),
        Duration::from_secs(5),
    );
    let Ans::NoChild(why) = c.ask(&Req::Info) else {
        panic!("no child")
    };
    assert!(why.contains("could not be started"), "{why}");
}

#[test]
fn a_source_that_panics_answers_died_and_the_engine_goes_on() {
    struct Panics(usize);
    impl Source for Panics {
        fn ask(&mut self, _: &Req) -> Ans {
            self.0 += 1;
            if self.0 == 1 {
                panic!("a bad page");
            }
            Ans::Text("fine".into())
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let engine = Engine::start(Box::new(Panics(0)), move |req, ans| {
        tx.send((req, ans)).is_ok()
    });
    engine.ask(Req::Text { page: 0 });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        (Req::Text { page: 0 }, Ans::Failed(child::DIED.into()))
    );
    engine.ask(Req::Text { page: 1 });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        (Req::Text { page: 1 }, Ans::Text("fine".into()))
    );
}

/// A quit while the child works on a page ends the child at once, not at
/// its watchdog.
#[test]
fn killing_the_engine_ends_a_child_that_hangs() {
    let child = fake("hang", Duration::from_secs(60));
    let killer = child.killer().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let engine = Engine::start(Box::new(child), move |req, ans| tx.send((req, ans)).is_ok());
    engine.ask(Req::Info);
    // Started, and its hello read: the question is with it now.
    let start = std::time::Instant::now();
    let pid = loop {
        if let Some(pid) = killer.pid() {
            break pid;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the child never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    std::thread::sleep(Duration::from_millis(300));
    let killed = std::time::Instant::now();
    engine.kill();
    let (_, ans) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(ans, Ans::Failed(child::DIED.into()));
    assert!(killed.elapsed() < Duration::from_secs(10));
    assert_eq!(killer.pid(), None);
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the child {pid} still runs"
    );
    let _ = pid;
}

#[test]
fn a_child_that_chatters_and_never_says_hello_is_given_up_in_time() {
    let mut c = fake("chatter", Duration::from_secs(2));
    let start = std::time::Instant::now();
    assert_eq!(c.ask(&Req::Info), Ans::Failed(child::TOO_LONG.into()));
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn the_engine_answers_on_its_own_thread() {
    struct Slow;
    impl Source for Slow {
        fn ask(&mut self, req: &Req) -> Ans {
            std::thread::sleep(Duration::from_millis(200));
            match req {
                Req::Text { page } => Ans::Text(format!("page {page}")),
                _ => Ans::Failed("no".into()),
            }
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let engine = Engine::start(Box::new(Slow), move |req, ans| tx.send((req, ans)).is_ok());
    let start = std::time::Instant::now();
    engine.ask(Req::Text { page: 4 });
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "ask never waits"
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        (Req::Text { page: 4 }, Ans::Text("page 4".into()))
    );
}

// ---- the real child ---------------------------------------------------------------------

/// `$DOCXY_PDF`, which the ignored tests need (CI's suite-linux job builds
/// it and runs them with `-- --ignored pdf_view::`).
fn real_child(name: &str) -> Child {
    let exe = std::env::var_os("DOCXY_PDF")
        .expect("DOCXY_PDF names a built docxy-pdf (cargo build --manifest-path suite/Cargo.toml -p pdfpages --bin docxy-pdf)");
    Child::new(PathBuf::from(exe), corpus(name))
}

#[test]
#[ignore = "needs DOCXY_PDF: a built docxy-pdf"]
fn real_child_reads_the_slides_and_draws_a_page() {
    let mut c = real_child("slides");
    let Ans::Info(m) = c.ask(&Req::Info) else {
        panic!("info")
    };
    assert_eq!(m.pages.len(), 3);
    for p in &m.pages {
        assert_eq!((p.w.round(), p.h.round()), (960., 540.));
    }
    assert!(
        m.outline
            .iter()
            .any(|b| b.0 == "Revenue by region" && b.1 == Some(1))
    );
    let Ans::Pixels(r) = c.ask(&Req::Pixels {
        page: 0,
        scale: 0.5,
    }) else {
        panic!("pixels")
    };
    assert_eq!((r.width, r.height), (480, 270));
    assert!(
        r.rgba
            .chunks(4)
            .any(|p| p[0] < 200 || p[1] < 200 || p[2] < 200),
        "not blank"
    );
}

#[test]
#[ignore = "needs DOCXY_PDF: a built docxy-pdf"]
fn real_child_reads_text_in_its_scripts_and_says_when_it_is_locked() {
    let mut c = real_child("text");
    let Ans::Text(t) = c.ask(&Req::Text { page: 0 }) else {
        panic!("text")
    };
    assert!(
        t.contains("Привет, мир! Съешь же ещё этих мягких французских булок,"),
        "{t}"
    );
    assert!(t.contains("Καλημέρα κόσμε."), "{t}");
    assert_eq!(real_child("enc-aes-256").ask(&Req::Info), Ans::Locked);
    let Ans::Failed(why) = real_child("damaged-unreadable").ask(&Req::Info) else {
        panic!("failed")
    };
    assert!(!why.is_empty());
}
