//! Print and export as drawn (#2258): the dialogs' choices and what they
//! become, then jobs and exports through the worker's protocol (in
//! process: the same `serve` as the child) on the corpus, the sheets and
//! pictures read back and drawn by a worker of their own, against the
//! corpus's `pdftoppm` references where the page is drawn as it is.

use super::*;
use crate::open_mode_tests::Scratch;
use crate::pdf_view::tests::{SHARE, corpus_path, differing};
use crate::pdf_view::worker::{
    Answer, DocMeta, Exported, FieldValue, Op, Printed, Spawn, Worker, png_names,
};
use std::time::Duration;

fn worker_on(path: &Path) -> Worker {
    Worker::new(path, Spawn::InProcess, Duration::from_secs(60))
}

fn info_of(w: &mut Worker) -> DocMeta {
    match w.ask(&Op::Info) {
        Answer::Info(m) => m,
        other => panic!("{other:?}"),
    }
}

/// A job of `pages` on A4, portrait-or-landscape as the page, everything
/// printed, to the test printer: the dialog's defaults.
pub(crate) fn job(pages: Vec<usize>, sizing: Sizing, spool: Spool) -> PrintJob {
    PrintJob {
        pages,
        paper_name: Some("A4".into()),
        paper: pdfcraft_print::PAPERS[4].1,
        sizing,
        orientation: Orient::Auto,
        content: PrintContent::DocumentAndMarkups,
        printer: Some("Test Printer".into()),
        copies: 1,
        collate: true,
        title: "test.pdf".into(),
        spool,
    }
}

fn print(w: &mut Worker, job: &PrintJob) -> Printed {
    match w.ask(&Op::Print { job: job.clone() }) {
        Answer::Printed(p) => p,
        other => panic!("{other:?}"),
    }
}

/// The job file `printed` wrote in `dir`, and its JSON.
fn job_files(dir: &Path, p: &Printed) -> (PathBuf, serde_json::Value) {
    let json = std::fs::read_to_string(dir.join(format!("{}.json", p.job))).unwrap();
    (
        dir.join(format!("{}.pdf", p.job)),
        serde_json::from_str(&json).unwrap(),
    )
}

/// Print `job` of `name` into `dir`: the sheets' PDF, and a worker on it.
fn printed(name: &str, job: &PrintJob) -> (Worker, DocMeta) {
    let mut w = worker_on(&corpus_path(name));
    info_of(&mut w);
    let p = print(&mut w, job);
    let Spool::Dir { dir } = &job.spool else {
        unreachable!()
    };
    let (pdf, _) = job_files(dir, &p);
    let mut sheets = worker_on(&pdf);
    let m = info_of(&mut sheets);
    assert_eq!(m.pages.len(), p.sheets, "one sheet a page");
    (sheets, m)
}

fn pixels(w: &mut Worker, page: usize, scale: f32) -> crate::pdf_view::worker::Raster {
    match w.ask(&Op::Pixels { page, scale }) {
        Answer::Pixels(r) => r,
        other => panic!("{other:?}"),
    }
}

fn text(w: &mut Worker, page: usize) -> String {
    match w.ask(&Op::Text { page }) {
        Answer::Text(t) => t.plain_text(),
        other => panic!("{other:?}"),
    }
}

/// The width of what is drawn on a raster: its leftmost to its rightmost
/// pixel darker than near white, in pixels.
fn ink_width(r: &crate::pdf_view::worker::Raster) -> f32 {
    let (mut lo, mut hi) = (u32::MAX, 0);
    for y in 0..r.h {
        for x in 0..r.w {
            let i = ((y * r.w + x) * 4) as usize;
            if r.bgra[i..i + 3].iter().any(|c| *c < 200) {
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
    }
    assert!(lo <= hi, "nothing drawn");
    (hi - lo + 1) as f32
}

fn dark(r: &crate::pdf_view::worker::Raster) -> usize {
    r.bgra
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3].iter().any(|c| *c < 200))
        .count()
}

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn reference(name: &str, page: usize) -> image::RgbaImage {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../corpus/pdf-view/ref/{name}-{}.png", page + 1));
    image::open(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .to_rgba8()
}

// ---- the dialogs ----------------------------------------------------------------

fn facts(count: usize) -> ViewFacts {
    ViewFacts {
        labels: (1..=count).map(|n| n.to_string()).collect(),
        current: 1,
        first_page: Some((595.28, 841.89)),
        security: None,
    }
}

fn set(d: &mut Dialog, name: &str, v: Value) {
    let i = d.controls.iter().position(|c| c.name == name).unwrap();
    // As the dialog does: the value set, then the reaction told the one
    // before.
    let before = std::mem::replace(&mut d.controls[i].value, v);
    if let Some(r) = d.react {
        (r.0)(d, i, &before);
    }
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

/// Criterion 2: the Print dialog is catalogued, Acrobat's controls in
/// Acrobat's order, the default printer chosen, the paper by the rule.
#[test]
fn the_print_dialog_has_acrobats_choices() {
    let printers = harness_printers();
    let d = print_dialog(&facts(3), &printers);
    assert_eq!(d.id, catalog::PDF_PRINT);
    assert_eq!(d.owner, DialogOwner::PdfPrint);
    let names: Vec<&str> = d.controls.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "printer",
            "copies",
            "pages",
            "range",
            "sizing",
            "percent",
            "orientation",
            "paper",
            "content"
        ]
    );
    let items = |n: &str| {
        d.controls
            .iter()
            .find(|c| c.name == n)
            .unwrap()
            .items
            .clone()
    };
    assert_eq!(items("printer"), ["Test Printer", "Second Printer"]);
    assert_eq!(items("sizing"), SIZING);
    assert_eq!(items("content"), CONTENT);
    assert_eq!(items("paper").len(), pdfcraft_print::PAPERS.len());
    // A4 first page, printers with no default paper: A4.
    assert_eq!(d.value("paper"), Some(&Value::Choice(Some(4))));
    assert_eq!(d.value("printer"), Some(&Value::Choice(Some(0))));
    let roles: Vec<(&str, ButtonRole)> = d
        .buttons
        .iter()
        .map(|b| (b.label.as_str(), b.role))
        .collect();
    assert_eq!(
        roles,
        [
            ("Print", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel)
        ]
    );
    // Enter presses Print, and Save… on an export, as OK; Escape cancels.
    for d in [
        print_dialog(&facts(3), &printers),
        png_dialog(),
        pdf_dialog(),
    ] {
        let accept = d.buttons[0].label.clone();
        let mut stack = crate::dialog::DialogStack::default();
        stack.push(d);
        assert_eq!(stack.key_button("enter", true), Some(accept));
        assert_eq!(stack.key_button("escape", true).as_deref(), Some("Cancel"));
    }
    // None found: the list says so, and OK refuses.
    let d = print_dialog(&facts(3), &[]);
    assert_eq!(d.controls[0].items, [NO_PRINTERS.to_string()]);
    assert_eq!(
        print_job(&d, &facts(3), &[], "t", Spool::System).unwrap_err(),
        NO_PRINTERS
    );
    // The export dialogs.
    let d = png_dialog();
    assert_eq!(
        (d.id, d.owner),
        (catalog::PDF_EXPORT_PNG, DialogOwner::PdfExportPng)
    );
    assert_eq!(d.value("dpi"), Some(&Value::Choice(Some(DEFAULT_DPI))));
    assert_eq!(DPIS[DEFAULT_DPI], 150);
    let d = pdf_dialog();
    assert_eq!(
        (d.id, d.owner),
        (catalog::PDF_EXPORT_PDF, DialogOwner::PdfExportPdf)
    );
}

/// Criterion 2: the paper the dialog starts at: the printer's default
/// paper, else the one the first page matches (either way round), else US
/// Letter.
#[test]
fn the_paper_starts_at_the_printers_then_the_pages_then_letter() {
    let mut p = harness_printers().remove(0);
    p.paper = Some(3);
    assert_eq!(starting_paper(Some(&p), Some((595.28, 841.89))), 3);
    assert_eq!(starting_paper(None, Some((595.28, 841.89))), 4, "A4");
    assert_eq!(starting_paper(None, Some((842., 595.))), 4, "A4 turned");
    assert_eq!(
        starting_paper(None, Some((960., 540.))),
        0,
        "no match: Letter"
    );
    assert_eq!(starting_paper(None, None), 0);
}

/// Criterion 2: the pages, parsed as PdfCraft's Print parses them: All,
/// the page showing, numbers and labels, open ranges, any order; a bad
/// range refuses in PdfCraft's words, and the dialog stays.
#[test]
fn the_pages_are_parsed_in_the_parent_as_pdfcraft_parses_them() {
    let f = facts(5);
    let mut d = print_dialog(&f, &harness_printers());
    assert_eq!(chosen_pages(&d, &f).unwrap(), [0, 1, 2, 3, 4]);
    set(&mut d, "pages", Value::Choice(Some(1)));
    assert_eq!(chosen_pages(&d, &f).unwrap(), [1], "the page showing");
    // Typing a range picks Pages.
    set(&mut d, "range", t("2-3"));
    assert_eq!(d.value("pages"), Some(&Value::Choice(Some(2))));
    for (range, want) in [
        ("2-3", vec![1, 2]),
        ("3,1", vec![2, 0]),
        ("4-", vec![3, 4]),
        ("1-2, 5", vec![0, 1, 4]),
    ] {
        set(&mut d, "range", t(range));
        assert_eq!(chosen_pages(&d, &f).unwrap(), want, "{range}");
    }
    set(&mut d, "range", t("7"));
    assert_eq!(
        chosen_pages(&d, &f).unwrap_err(),
        "page 7 is out of range (1–5)"
    );
    set(&mut d, "range", t("x"));
    assert_eq!(
        chosen_pages(&d, &f).unwrap_err(),
        "\"x\" is not a page number or label"
    );
    set(&mut d, "range", t(""));
    assert!(chosen_pages(&d, &f).is_err(), "Pages with nothing typed");
    // Labels: a document numbered i, ii, then 1.
    let labelled = ViewFacts {
        labels: vec!["i".into(), "ii".into(), "1".into()],
        ..facts(3)
    };
    assert_eq!(select(3, "ii-1", &labelled.labels).unwrap(), [1, 2]);
}

/// Criterion 2: the rest of the job: copies 1–999, a custom scale 1–1000 %
/// (typing it picks Custom scale), orientation, paper, comments and forms,
/// the printer by its spooler name.
#[test]
fn the_dialogs_choices_become_the_job() {
    let f = facts(2);
    let printers = harness_printers();
    let mut d = print_dialog(&f, &printers);
    set(&mut d, "printer", Value::Choice(Some(1)));
    set(&mut d, "copies", t("3"));
    set(&mut d, "percent", t("50"));
    assert_eq!(d.value("sizing"), Some(&Value::Choice(Some(3))));
    set(&mut d, "orientation", Value::Choice(Some(2)));
    set(&mut d, "paper", Value::Choice(Some(0)));
    set(&mut d, "content", Value::Choice(Some(2)));
    let job = print_job(&d, &f, &printers, "a.pdf", Spool::System).unwrap();
    assert_eq!(job.printer.as_deref(), Some("Second Printer"));
    assert_eq!(job.copies, 3);
    assert_eq!(job.sizing, Sizing::Custom { percent: 50. });
    assert_eq!(job.orientation, Orient::Landscape);
    assert_eq!(job.paper_name.as_deref(), Some("US Letter"));
    assert_eq!(job.paper, (612., 792.));
    assert_eq!(job.content, PrintContent::FormFieldsOnly);
    assert_eq!(job.pages, [0, 1]);
    assert_eq!(job.title, "a.pdf");
    for (field, bad, why) in [
        ("copies", "0", "Copies"),
        ("copies", "1000", "Copies"),
        ("percent", "0", "Custom scale"),
        ("percent", "1001", "Custom scale"),
    ] {
        let mut d = d.clone();
        set(&mut d, field, t(bad));
        let e = print_job(&d, &f, &printers, "a.pdf", Spool::System).unwrap_err();
        assert!(e.starts_with(why), "{field} {bad}: {e}");
    }
    for (i, sizing) in [Sizing::Fit, Sizing::Actual, Sizing::Shrink]
        .into_iter()
        .enumerate()
    {
        let mut d = d.clone();
        set(&mut d, "sizing", Value::Choice(Some(i)));
        assert_eq!(
            print_job(&d, &f, &printers, "a", Spool::System)
                .unwrap()
                .sizing,
            sizing
        );
    }
}

/// Criterion 6: the dialogs refuse up front what the file's permissions
/// withhold (the child refuses too); the owner password lifts it.
#[test]
fn the_dialogs_refuse_what_the_permissions_withhold() {
    let mut f = facts(1);
    f.security = Some(SecurityMeta {
        print: false,
        copy: false,
        extract: false,
        ..SecurityMeta::default()
    });
    let printers = harness_printers();
    let d = print_dialog(&f, &printers);
    let e = print_job(&d, &f, &printers, "a", Spool::System).unwrap_err();
    assert!(e.contains("do not allow printing"), "{e}");
    assert!(
        export_of(&png_dialog(), &f)
            .unwrap_err()
            .contains("copying")
    );
    assert!(
        export_of(&pdf_dialog(), &f)
            .unwrap_err()
            .contains("another file")
    );
    f.security.as_mut().unwrap().owner = true;
    assert!(print_job(&d, &f, &printers, "a", Spool::System).is_ok());
    assert!(export_of(&png_dialog(), &f).is_ok());
}

/// Criterion 5: what the exports write: one PNG to the name picked,
/// several each to `<stem>_Page_<n>.png` beside it (zero-padded); an
/// extension added, a wrong one refused.
#[test]
fn export_names_and_targets() {
    let p = Path::new("/d/out.png");
    assert_eq!(png_names(p, &[4]), [PathBuf::from("/d/out.png")]);
    assert_eq!(
        png_names(p, &[0, 2]),
        [
            PathBuf::from("/d/out_Page_1.png"),
            PathBuf::from("/d/out_Page_3.png")
        ]
    );
    let names = png_names(p, &[0, 11]);
    assert_eq!(names[0], PathBuf::from("/d/out_Page_01.png"));
    assert_eq!(names[1], PathBuf::from("/d/out_Page_12.png"));
    let png = Export::Png {
        pages: vec![0],
        dpi: 150.,
    };
    let pdf = Export::Pdf { pages: vec![0] };
    assert_eq!(
        export_target("/d/a".into(), &png).unwrap(),
        PathBuf::from("/d/a.png")
    );
    assert_eq!(
        export_target("/d/a.PNG".into(), &png).unwrap(),
        PathBuf::from("/d/a.PNG")
    );
    assert!(export_target("/d/a.pdf".into(), &png).is_err());
    assert_eq!(
        export_target("/d/a".into(), &pdf).unwrap(),
        PathBuf::from("/d/a.pdf")
    );
    assert_eq!(export_name("report.pdf", &png), "report_Page_1.png");
    assert_eq!(export_name("report.pdf", &pdf), "report_pages.pdf");
    let mut d = png_dialog();
    set(&mut d, "range", t("1-2"));
    set(&mut d, "dpi", Value::Choice(Some(2)));
    assert_eq!(
        export_of(&d, &facts(3)).unwrap(),
        Export::Png {
            pages: vec![0, 1],
            dpi: 300.
        }
    );
}

// ---- print as drawn, through the worker -------------------------------------

/// Criteria 3 and 4: a job is one sheet a page, in the order chosen, each
/// the paper chosen (turned for a landscape page with Auto, or always with
/// Landscape); its job file has the printer, copies, title, the paper's
/// CUPS media and the `lp` command; a landscape job tells CUPS so.
#[test]
fn a_job_is_a_sheet_a_page_on_the_paper_with_the_spoolers_options() {
    let dir = Scratch::new();
    let spool = Spool::Dir {
        dir: dir.path("jobs"),
    };
    let mut j = job(vec![2, 0], Sizing::Fit, spool.clone());
    j.copies = 2;
    j.title = "slides.pdf".into();
    let mut w = worker_on(&corpus_path("slides"));
    info_of(&mut w);
    let p = print(&mut w, &j);
    assert_eq!(p.sheets, 2);
    let (pdf, json) = job_files(&dir.path("jobs"), &p);
    assert_eq!(json["printer"], "Test Printer");
    assert_eq!(json["copies"], 2);
    assert_eq!(json["title"], "slides.pdf");
    assert_eq!(json["pages"], serde_json::json!([3, 1]));
    assert_eq!(json["sheets"], 2);
    let lp: Vec<String> = serde_json::from_value(json["lp_args"].clone()).unwrap();
    let has = |a: &str, b: &str| lp.windows(2).any(|w| w[0] == a && w[1] == b);
    assert!(has("-d", "Test Printer"), "{lp:?}");
    assert!(has("-n", "2"), "{lp:?}");
    let media = pdfcraft_print::paper_media_option(Some("A4"), pdfcraft_print::PAPERS[4].1);
    assert_eq!(media.0, "media");
    assert!(has("-o", &format!("media={}", media.1)), "{lp:?}");
    assert_eq!(json["options"][0], serde_json::json!(["media", media.1]));
    // Slides are landscape: Auto turns the A4 sheet, and CUPS is told.
    assert!(has("-o", "orientation-requested=4"), "{lp:?}");
    let mut sheets = worker_on(&pdf);
    let m = info_of(&mut sheets);
    for s in &m.pages {
        assert!(near(s.w, 841.89, 0.5) && near(s.h, 595.28, 0.5), "{s:?}");
    }
    // In the order chosen: the third slide first.
    let mut src = worker_on(&corpus_path("slides"));
    info_of(&mut src);
    let first = text(&mut src, 2);
    let sheet1 = text(&mut sheets, 0);
    let word = first.split_whitespace().next().unwrap();
    assert!(sheet1.contains(word), "{sheet1:?} lacks {word:?}");
    // Portrait keeps the sheet upright, and says nothing to CUPS about it.
    let mut j = job(vec![0], Sizing::Fit, spool.clone());
    j.orientation = Orient::Portrait;
    let p = print(&mut w, &j);
    let (pdf, json) = job_files(&dir.path("jobs"), &p);
    let lp: Vec<String> = serde_json::from_value(json["lp_args"].clone()).unwrap();
    assert!(!lp.iter().any(|a| a == "orientation-requested=4"), "{lp:?}");
    let m = info_of(&mut worker_on(&pdf));
    assert!(near(m.pages[0].w, 595.28, 0.5) && near(m.pages[0].h, 841.89, 0.5));
    // A portrait page with Landscape: the sheet turned.
    let (_, m) = printed("text", &{
        let mut j = job(vec![0], Sizing::Fit, spool);
        j.orientation = Orient::Landscape;
        j
    });
    assert!(near(m.pages[0].w, 841.89, 0.5), "{:?}", m.pages[0]);
}

/// Criterion 3: page sizing, measured on the sheet as drawn: what is on the
/// page keeps its width at Actual size (a Letter page on A4 is not
/// scaled), is half as wide at 50 %, scales up to Fit a larger paper and
/// down to Fit a smaller one, and Shrink scales only a page larger than
/// the paper.
#[test]
fn page_sizing_scales_as_acrobat_does() {
    let dir = Scratch::new();
    let spool = Spool::Dir {
        dir: dir.path("jobs"),
    };
    let width = |name: &str| {
        let mut w = worker_on(&corpus_path(name));
        info_of(&mut w);
        ink_width(&pixels(&mut w, 0, 1.))
    };
    let on = |name: &str, paper: (f64, f64), sizing: Sizing| {
        let mut j = job(vec![0], sizing, spool.clone());
        j.paper = paper;
        j.paper_name = None;
        let (mut s, _) = printed(name, &j);
        ink_width(&pixels(&mut s, 0, 1.))
    };
    let a4 = pdfcraft_print::PAPERS[4].1;
    let a3 = pdfcraft_print::PAPERS[3].1;
    let a5 = pdfcraft_print::PAPERS[5].1;
    // columns.pdf is Letter: 612 pt wide, A4 595.
    let letter = width("columns");
    let tol = |w: f32| (w * 0.02).max(3.);
    let actual = on("columns", a4, Sizing::Actual);
    assert!(
        near(actual, letter, tol(letter)),
        "actual {actual} vs {letter}"
    );
    let half = on("columns", a4, Sizing::Custom { percent: 50. });
    assert!(
        near(half, letter / 2., tol(letter / 2.)),
        "50 % {half} vs {letter}"
    );
    let page = width("text");
    let up = on("text", a3, Sizing::Fit);
    assert!(up > page * 1.25, "fit to A3 {up} vs {page}");
    let down = on("text", a5, Sizing::Fit);
    assert!(down < page * 0.75, "fit to A5 {down} vs {page}");
    let kept = on("text", a3, Sizing::Shrink);
    assert!(near(kept, page, tol(page)), "shrink on A3 {kept} vs {page}");
    let shrunk = on("text", a5, Sizing::Shrink);
    assert!(shrunk < page * 0.75, "shrink on A5 {shrunk} vs {page}");
}

/// Criterion 3: as drawn, against an independent reference: a page printed
/// at Actual size on paper its own size, the sheet drawn at a pixel a
/// point, matches `pdftoppm`'s rendering of the page (the corpus test's
/// rule): an upright page, and a `/Rotate 90` one printed as the view
/// shows it, landscape.
#[test]
fn a_sheet_at_actual_size_matches_the_pages_reference_rendering() {
    let dir = Scratch::new();
    for name in ["text", "rotated"] {
        let mut src = worker_on(&corpus_path(name));
        let m = info_of(&mut src);
        let size = (f64::from(m.pages[0].w), f64::from(m.pages[0].h));
        // The paper is portrait; Auto turns it for a landscape page.
        let mut j = job(
            vec![0],
            Sizing::Actual,
            Spool::Dir {
                dir: dir.path(name),
            },
        );
        j.paper = (size.0.min(size.1), size.0.max(size.1));
        j.paper_name = None;
        let (mut sheets, sm) = printed(name, &j);
        assert!(
            near(sm.pages[0].w, m.pages[0].w, 0.5),
            "{name}: {:?}",
            sm.pages[0]
        );
        let r = pixels(&mut sheets, 0, 1.);
        let reference = reference(name, 0);
        assert_eq!((r.w, r.h), reference.dimensions(), "{name}");
        let ours = differing(&r.bgra, &reference, false);
        assert!(ours <= SHARE, "{name}: {ours} of blocks differ");
        assert!(
            differing(&r.bgra, &reference, true) > SHARE,
            "{name}: blank passes"
        );
    }
}

/// Criterion 3: what is printed is the page as drawn: a value filled in and
/// not saved prints; Form fields only prints the fields and no page
/// content; Document prints the fields but no comments.
#[test]
fn unsaved_values_print_and_comments_and_forms_choose_what() {
    let dir = Scratch::new();
    let mut w = worker_on(&corpus_path("forms"));
    let m = info_of(&mut w);
    assert!(matches!(
        w.ask(&Op::SetField {
            name: "name".into(),
            value: FieldValue::Text {
                text: "Printed value".into()
            },
        }),
        Answer::Form(_)
    ));
    let source = text(&mut w, 0);
    let mut print_as = |content: PrintContent| {
        let mut j = job(
            vec![0],
            Sizing::Actual,
            Spool::Dir {
                dir: dir.path("jobs"),
            },
        );
        j.content = content;
        let p = print(&mut w, &j);
        let (pdf, _) = job_files(&dir.path("jobs"), &p);
        let mut s = worker_on(&pdf);
        info_of(&mut s);
        (text(&mut s, 0), pixels(&mut s, 0, 0.5))
    };
    let (all, _) = print_as(PrintContent::DocumentAndMarkups);
    assert!(all.contains("Printed value"), "{all:?}");
    // A word the page itself prints (not a field's).
    let page_word = source
        .split_whitespace()
        .find(|w| w.len() > 3 && !all.split_whitespace().all(|a| a != *w) && *w != "Printed")
        .expect("a page word");
    let (fields, _) = print_as(PrintContent::FormFieldsOnly);
    assert!(fields.contains("Printed value"), "{fields:?}");
    assert!(
        !fields.contains(page_word),
        "{page_word:?} printed with Form fields only: {fields:?}"
    );
    let (doc, _) = print_as(PrintContent::Document);
    assert!(
        doc.contains("Printed value") && doc.contains(page_word),
        "{doc:?}"
    );
    assert_eq!(m.pages.len(), 2);

    // Comments: annots.pdf's every kind; Document leaves them out.
    let mut w = worker_on(&corpus_path("annots"));
    info_of(&mut w);
    let mut ink = |content| {
        let mut j = job(
            vec![0],
            Sizing::Actual,
            Spool::Dir {
                dir: dir.path("annots"),
            },
        );
        j.content = content;
        let p = print(&mut w, &j);
        let (pdf, _) = job_files(&dir.path("annots"), &p);
        let mut s = worker_on(&pdf);
        info_of(&mut s);
        dark(&pixels(&mut s, 0, 0.5))
    };
    let markups = ink(PrintContent::DocumentAndMarkups);
    let document = ink(PrintContent::Document);
    assert!(
        markups > document + 500,
        "markups {markups}, document {document}"
    );
}

/// Criterion 6: printing, a PNG and pages into a PDF are refused as the
/// password the file was opened with withholds them, and allowed with the
/// owner password.
#[test]
fn the_child_refuses_what_the_permissions_withhold() {
    use crate::pdf_view::worker::Secret;
    let dir = Scratch::new();
    let spool = Spool::Dir {
        dir: dir.path("jobs"),
    };
    let outputs = |w: &mut Worker| -> [Answer; 3] {
        [
            w.ask(&Op::Print {
                job: job(vec![0], Sizing::Fit, spool.clone()),
            }),
            w.ask(&Op::Png { page: 0, dpi: 72. }),
            w.ask(&Op::ExtractPages { pages: vec![0] }),
        ]
    };
    let refused = |a: &[Answer; 3], what: &str| {
        let (p, c, e) = crate::pdf_view::worker::doc_refusals();
        for (answer, why) in a.iter().zip([p, c, e]) {
            match answer {
                Answer::Failed(w) => assert_eq!(w, why, "{what}"),
                other => panic!("{what}: {other:?}"),
            }
        }
    };
    let mut w = worker_on(&corpus_path("enc-owner-only"));
    let m = info_of(&mut w);
    assert!(!m.security.as_ref().unwrap().extract);
    refused(&outputs(&mut w), "owner-only");
    let mut w = worker_on(&corpus_path("enc-aes-128"));
    assert!(matches!(
        w.unlock(Secret::new("user-aes-128".into())),
        Answer::Info(_)
    ));
    refused(&outputs(&mut w), "user password");
    let mut w = worker_on(&corpus_path("enc-aes-128"));
    match w.unlock(Secret::new("owner-aes-128".into())) {
        Answer::Info(m) => assert!(m.security.unwrap().extract),
        other => panic!("{other:?}"),
    }
    let [p, png, pdf] = outputs(&mut w);
    assert!(matches!(p, Answer::Printed(_)), "{p:?}");
    assert!(matches!(png, Answer::Png { .. }), "{png:?}");
    match pdf {
        // Never encrypted: a new worker reads it with no password.
        Answer::Bytes(b) => {
            let out = dir.path("owner.pdf");
            std::fs::write(&out, b).unwrap();
            let m = info_of(&mut worker_on(&out));
            assert!(m.security.is_none());
        }
        other => panic!("{other:?}"),
    }
}

/// A page no file has, and a job with no pages, are refused with a reason.
#[test]
fn pages_out_of_range_are_refused_in_the_child_too() {
    let dir = Scratch::new();
    let mut w = worker_on(&corpus_path("text"));
    info_of(&mut w);
    let spool = Spool::Dir {
        dir: dir.path("jobs"),
    };
    for op in [
        Op::Print {
            job: job(vec![0, 5], Sizing::Fit, spool.clone()),
        },
        Op::Print {
            job: job(vec![], Sizing::Fit, spool),
        },
        Op::Png { page: 2, dpi: 72. },
        Op::ExtractPages { pages: vec![9] },
    ] {
        match w.ask(&op) {
            Answer::Failed(why) => assert!(!why.is_empty()),
            other => panic!("{op:?}: {other:?}"),
        }
    }
}

// ---- export -------------------------------------------------------------------

fn decode(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .unwrap()
        .to_rgba8()
}

fn bgra(img: &image::RgbaImage) -> Vec<u8> {
    img.pixels()
        .flat_map(|p| [p.0[2], p.0[1], p.0[0], p.0[3]])
        .collect()
}

/// Criterion 5: a page's PNG is the page's size at the resolution (a
/// turned page turned), at 72 dpi matches `pdftoppm`'s rendering, and is
/// capped where the renderer caps a page, saying the resolution drawn.
#[test]
fn a_pages_png_is_its_size_at_the_resolution_and_drawn_as_the_reference() {
    for name in ["text", "rotated"] {
        let mut w = worker_on(&corpus_path(name));
        let m = info_of(&mut w);
        let (pw, ph) = (m.pages[0].w, m.pages[0].h);
        for dpi in [72., 150.] {
            let (png, used) = match w.ask(&Op::Png { page: 0, dpi }) {
                Answer::Png { png, dpi } => (png, dpi),
                other => panic!("{other:?}"),
            };
            assert!(near(used as f32, dpi as f32, 0.01), "{name} {dpi}: {used}");
            let img = decode(&png);
            let scale = (dpi / 72.) as f32;
            assert_eq!(
                img.dimensions(),
                (
                    pdfcraft_render::device_pixels(pw, scale),
                    pdfcraft_render::device_pixels(ph, scale)
                ),
                "{name} at {dpi}"
            );
            if dpi == 72. {
                let reference = reference(name, 0);
                let ours = differing(&bgra(&img), &reference, false);
                assert!(ours <= SHARE, "{name}: {ours}");
            }
        }
        if name == "rotated" {
            assert!(pw > ph, "the turned page is landscape");
        }
    }
    // 1200 dpi of an A4 page is past the renderer's caps: drawn smaller,
    // and said.
    let mut w = worker_on(&corpus_path("text"));
    info_of(&mut w);
    match w.ask(&Op::Png {
        page: 0,
        dpi: 1200.,
    }) {
        Answer::Png { png, dpi } => {
            assert!(dpi < 1200., "{dpi}");
            let (iw, ih) = decode(&png).dimensions();
            assert!(iw.max(ih) <= 8192, "{iw} x {ih}");
        }
        other => panic!("{other:?}"),
    }
}

/// Criterion 5: a PNG export writes exactly the pages asked, each to its
/// own name, or nothing when a name it would write (but the one picked)
/// is taken; a single page to the name picked.
#[test]
fn a_png_export_writes_exactly_its_pages_or_nothing() {
    let dir = Scratch::new();
    let mut w = worker_on(&corpus_path("slides"));
    info_of(&mut w);
    let target = dir.path("out.png");
    let export = Export::Png {
        pages: vec![0, 2],
        dpi: 72.,
    };
    match w.ask(&Op::ExportTo {
        export: export.clone(),
        path: target.clone(),
    }) {
        Answer::Exported(Exported { files, dpi_used }) => {
            assert_eq!(
                files,
                [dir.path("out_Page_1.png"), dir.path("out_Page_3.png")]
            );
            assert_eq!(dpi_used, None);
            for f in &files {
                assert_eq!(decode(&std::fs::read(f).unwrap()).dimensions(), (960, 540));
            }
        }
        other => panic!("{other:?}"),
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path(""))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["out_Page_1.png", "out_Page_3.png"],
        "nothing else, no temporary file"
    );
    // Page 2's name is taken: nothing is written, the file there kept.
    std::fs::write(dir.path("out_Page_2.png"), b"mine").unwrap();
    std::fs::remove_file(dir.path("out_Page_1.png")).unwrap();
    match w.ask(&Op::ExportTo {
        export: Export::Png {
            pages: vec![0, 1],
            dpi: 72.,
        },
        path: target.clone(),
    }) {
        Answer::Failed(why) => assert!(why.contains("out_Page_2.png"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.path("out_Page_1.png").exists());
    assert_eq!(std::fs::read(dir.path("out_Page_2.png")).unwrap(), b"mine");
    // One page: the name picked, replacing it (the dialog asked).
    std::fs::write(&target, b"old").unwrap();
    match w.ask(&Op::ExportTo {
        export: Export::Png {
            pages: vec![1],
            dpi: 72.,
        },
        path: target.clone(),
    }) {
        Answer::Exported(e) => assert_eq!(e.files, std::slice::from_ref(&target)),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        decode(&std::fs::read(&target).unwrap()).dimensions(),
        (960, 540)
    );
}

/// Criterion 5: a PDF export is the pages chosen, as drawn (a value filled
/// and not saved in it), their sizes, `/Rotate` and bookmarks kept; a new
/// worker opens it.
#[test]
fn a_pdf_export_is_the_pages_as_drawn_with_their_bookmarks() {
    let dir = Scratch::new();
    let export = |w: &mut Worker, pages: Vec<usize>, name: &str| -> DocMeta {
        let path = dir.path(name);
        match w.ask(&Op::ExportTo {
            export: Export::Pdf { pages },
            path: path.clone(),
        }) {
            Answer::Exported(e) => assert_eq!(e.files, std::slice::from_ref(&path)),
            other => panic!("{other:?}"),
        }
        info_of(&mut worker_on(&path))
    };
    let mut w = worker_on(&corpus_path("slides"));
    let src = info_of(&mut w);
    let m = export(&mut w, vec![0, 1, 2], "slides-all.pdf");
    assert_eq!(m.pages.len(), 3);
    assert!(near(m.pages[0].w, 960., 0.5) && near(m.pages[0].h, 540., 0.5));
    assert!(
        !m.outline.is_empty() && m.outline.len() == src.outline.len(),
        "{:?}",
        m.outline
    );
    let m = export(&mut w, vec![2], "slides-3.pdf");
    assert_eq!(m.pages.len(), 1);

    let mut w = worker_on(&corpus_path("rotated"));
    info_of(&mut w);
    let m = export(&mut w, vec![0], "rotated.pdf");
    assert_eq!(m.pages[0].rotation, 90);
    assert!(
        near(m.pages[0].w, 842., 1.) && near(m.pages[0].h, 595., 1.),
        "{:?}",
        m.pages[0]
    );

    let mut w = worker_on(&corpus_path("forms"));
    info_of(&mut w);
    assert!(matches!(
        w.ask(&Op::SetField {
            name: "name".into(),
            value: FieldValue::Text {
                text: "Exported value".into()
            },
        }),
        Answer::Form(_)
    ));
    let path = dir.path("forms-1.pdf");
    assert!(matches!(
        w.ask(&Op::ExportTo {
            export: Export::Pdf { pages: vec![0] },
            path: path.clone()
        }),
        Answer::Exported(_)
    ));
    let mut out = worker_on(&path);
    let m = info_of(&mut out);
    assert_eq!(m.pages.len(), 1);
    assert!(text(&mut out, 0).contains("Exported value"));
    let value = m
        .form
        .fields
        .iter()
        .find(|f| f.name == "name")
        .map(|f| f.value.clone());
    assert_eq!(value, Some(vec!["Exported value".to_string()]));
}

// ---- the view -------------------------------------------------------------------

/// Criteria 4 and 7: a print and an export go before any page, after the
/// fills before them; their answers are the status line's, a failure too.
#[test]
fn the_view_sends_prints_before_pages_and_says_how_they_went() {
    use crate::pdf_view::worker::Done;
    let path = corpus_path("text");
    let mut w = worker_on(&path);
    let meta = info_of(&mut w);
    let mut v = crate::pdf_view::PdfView::new(&path, meta, None);
    let j = job(vec![0, 1], Sizing::Fit, Spool::System);
    v.form_ops.push_back(Op::Print { job: j.clone() });
    assert_eq!(v.next_job(), Some(Op::Print { job: j.clone() }));
    assert!(v.busy());
    v.absorb(Done {
        op: Op::Print { job: j.clone() },
        answer: Answer::Printed(Printed {
            sheets: 2,
            job: "42".into(),
        }),
        lost: false,
    });
    assert_eq!(
        v.notice.take().as_deref(),
        Some("Sent 2 pages to Test Printer")
    );
    v.absorb(Done {
        op: Op::Print { job: j },
        answer: Answer::Failed("lp: no such printer".into()),
        lost: false,
    });
    assert_eq!(
        v.notice.take().as_deref(),
        Some("Not printed: lp: no such printer")
    );
    let export = Op::ExportTo {
        export: Export::Png {
            pages: vec![0, 1],
            dpi: 600.,
        },
        path: "/d/x.png".into(),
    };
    v.absorb(Done {
        op: export,
        answer: Answer::Exported(Exported {
            files: vec!["/d/x_Page_1.png".into(), "/d/x_Page_2.png".into()],
            dpi_used: Some(491.5),
        }),
        lost: false,
    });
    assert_eq!(v.exported.len(), 2);
    let n = v.notice.take().unwrap();
    assert!(
        n.starts_with("Saved 2 pictures in /d") && n.contains("491 dpi"),
        "{n}"
    );
}

/// Criterion 7: a print gets the whole-document budget, three times a
/// page's, and its watchdog twice that.
#[test]
fn a_print_or_extract_has_a_budget_of_its_own() {
    let t = Duration::from_secs(20);
    let dir = Spool::System;
    assert_eq!(
        Op::Print {
            job: job(vec![0], Sizing::Fit, dir)
        }
        .timeout(t),
        t * 3
    );
    assert_eq!(Op::ExtractPages { pages: vec![0] }.timeout(t), t * 3);
    assert_eq!(Op::Png { page: 0, dpi: 72. }.timeout(t), t);
    assert_eq!(Op::Pixels { page: 0, scale: 1. }.timeout(t), t);
}
