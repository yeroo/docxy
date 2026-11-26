//! The PDF view's state without a window (#2245): layout, the scheduler,
//! the raster budget, selection, Find and the `pdf-read` JSON over made-up
//! pages; then the corpus (`corpus/pdf-view`) through the worker's
//! protocol: each page against its reference rendering, the text and a
//! search on each file, and what `info` reads.

use super::worker::{Answer, Bookmark, DocMeta, Done, Op, PageMeta, Raster, Spawn, Worker};
use super::*;
use pdfcraft_render::TextGlyph;
use std::path::PathBuf;
use std::time::Duration;

fn a4() -> PageMeta {
    PageMeta {
        w: 595.,
        h: 842.,
        rotation: 0,
        label: String::new(),
    }
}

fn meta(n: usize) -> DocMeta {
    DocMeta {
        pages: (0..n)
            .map(|i| PageMeta {
                label: format!("{}", i + 1),
                ..a4()
            })
            .collect(),
        ..DocMeta::default()
    }
}

fn view(n: usize) -> PdfView {
    PdfView::new(Path::new("made-up.pdf"), meta(n), None)
}

/// Text laid out as a page shows it: a glyph per character but spaces
/// (a word break is `space_before`), 6 pt wide, a line per item, 20 pt
/// apart from y = 100.
fn page_text(lines: &[&str]) -> Arc<PageText> {
    let mut t = PageText::default();
    for (l, line) in lines.iter().enumerate() {
        let y = 100. + l as f32 * 20.;
        let mut x = 72.;
        let mut space = false;
        for c in line.chars() {
            if c == ' ' {
                space = true;
                x += 6.;
                continue;
            }
            t.glyphs.push(TextGlyph {
                text: c.to_string(),
                rect: [x, y, x + 6., y + 10.],
                quad: [[x, y + 10.], [x + 6., y + 10.], [x + 6., y], [x, y]],
                direction: [1., 0.],
            });
            t.line_of.push(l as u32);
            t.space_before.push(space);
            space = false;
            x += 6.;
        }
    }
    Arc::new(t)
}

fn raster(bytes: usize) -> Raster {
    let w = (bytes / 4).max(1) as u32;
    Raster {
        w,
        h: 1,
        bgra: vec![255; w as usize * 4],
    }
}

fn drawn(v: &mut PdfView, page: usize, bytes: usize) {
    let scale = raster_scale(&v.meta.pages[page], v.zoom, v.scale_factor);
    v.absorb(Done {
        op: Op::Pixels { page, scale },
        answer: Answer::Pixels(raster(bytes)),
        lost: false,
    });
}

/// Content coordinates of point `(x, y)` (points) on `page`.
fn at(v: &PdfView, page: usize, x: f32, y: f32) -> (f32, f32) {
    let r = v.layout().pages[page];
    let k = v.zoom * PX_PER_PT;
    (r.x + x * k, r.y + y * k)
}

// ---- layout -------------------------------------------------------------------

#[test]
fn pages_stack_in_one_centred_column_at_their_size() {
    let l = layout(&meta(2).pages, 1., 1000.);
    let (w, h) = (595. * PX_PER_PT, 842. * PX_PER_PT);
    assert_eq!(l.width, 1000.);
    assert_eq!(l.pages[0].w, w);
    assert_eq!(l.pages[0].h, h);
    assert_eq!(l.pages[0].x, ((1000. - w) / 2.).floor());
    assert_eq!(l.pages[0].y, GAP);
    assert_eq!(l.pages[1].y, GAP + h + GAP);
    assert_eq!(l.height, GAP + 2. * (h + GAP));
    // Narrower than a page: the column is the page and its gaps.
    let narrow = layout(&meta(1).pages, 2., 300.);
    assert_eq!(narrow.width, 595. * 2. * PX_PER_PT + 2. * GAP);
    assert_eq!(narrow.pages[0].x, GAP);
}

#[test]
fn a_landscape_page_keeps_its_own_size_in_the_column() {
    let pages = vec![
        a4(),
        PageMeta {
            w: 842.,
            h: 595.,
            rotation: 90,
            label: "2".into(),
        },
    ];
    let l = layout(&pages, 1., 2000.);
    assert_eq!(l.pages[1].w, 842. * PX_PER_PT);
    assert_eq!(l.pages[1].h, 595. * PX_PER_PT);
    assert!(l.pages[1].x < l.pages[0].x, "the wider page is centred too");
}

#[test]
fn the_view_shows_the_pages_it_overlaps_and_is_on_the_middle_one() {
    let l = layout(&meta(5).pages, 0.5, 800.);
    let h = l.pages[0].h;
    assert_eq!(visible(&l, 0., h / 2.), 0..1);
    assert_eq!(visible(&l, 0., h + 3. * GAP), 0..2);
    let top = l.pages[2].y + 1.;
    assert_eq!(visible(&l, top, 10.), 2..3);
    assert_eq!(current_page(&l, top, 10.), 2);
    // The middle in the gap after page 1: the next page.
    let gap_mid = l.pages[1].bottom() + GAP / 2.;
    assert_eq!(current_page(&l, gap_mid - 5., 10.), 2);
    assert_eq!(current_page(&l, l.height + 500., 10.), 4);
}

#[test]
fn a_raster_never_passes_the_view_caps() {
    let letter = PageMeta {
        w: 612.,
        h: 792.,
        rotation: 0,
        label: String::new(),
    };
    // 800 % on a 2x display would be 13056 x 16896 px (8 x 96/72 x 2 a point).
    let s = raster_scale(&letter, 8., 2.);
    let (w, h) = (612. * s, 792. * s);
    assert!(w.max(h) <= MAX_SIDE + 0.5, "{w} x {h}");
    assert!(w * h <= MAX_PIXELS * 1.001, "{w} x {h}");
    // Within the caps the scale is the zoom on the display.
    assert_eq!(raster_scale(&letter, 1., 2.), PX_PER_PT * 2.);
}

#[test]
fn zoom_steps_clamps_fits_and_the_page_box() {
    assert_eq!(zoom_step(1., true), 1.25);
    assert_eq!(zoom_step(1., false), 0.75);
    assert_eq!(zoom_step(1.1, false), 1.);
    assert_eq!(zoom_step(8., true), ZOOM_MAX);
    assert_eq!(zoom_step(0.1, false), ZOOM_MIN);
    assert_eq!(clamp_zoom(20.), ZOOM_MAX);
    assert_eq!(clamp_zoom(0.01), ZOOM_MIN);
    assert_eq!(clamp_zoom(f32::NAN), 1.);
    let fit = fit_width(&meta(1).pages, 1000.);
    assert!((595. * fit * PX_PER_PT - (1000. - 2. * GAP - 16.)).abs() < 0.5);
    assert_eq!(parse_page_box("7", 20), Some(6));
    assert_eq!(parse_page_box(" 7 / 20", 20), Some(6));
    assert_eq!(parse_page_box("21", 20), None);
    assert_eq!(parse_page_box("0", 20), None);
    assert_eq!(parse_page_box("x", 20), None);
}

#[test]
fn zooming_keeps_the_view_on_its_page() {
    let mut v = view(10);
    v.viewport.set((900., 700.));
    let page4 = v.layout().pages[4].y;
    v.scroll_to_xy(0., page4 + 50.);
    v.set_zoom(2.);
    let (_, y) = v.scroll_to.get().unwrap();
    let r = v.layout().pages[4];
    assert!((y - (r.y + 100.)).abs() < 1., "{y} vs {}", r.y + 100.);
    assert_eq!(v.current(), 4);
}

// ---- the scheduler and the raster budget -------------------------------------------

/// The plan's scheduler test: renders for pages 1-20 wanted, then a scroll
/// leaves only 18-20 showing: 1-17 are never asked for after it.
#[test]
fn a_page_scrolled_past_is_never_asked_for() {
    let mut v = view(30);
    v.zoom = 0.1;
    v.viewport.set((800., 600.));
    let l = v.layout();
    v.frame(800., l.pages[19].bottom(), 0., 1.);
    assert_eq!(v.showing, 0..20);
    let first = v.next_job().unwrap();
    assert!(matches!(first, Op::Pixels { page: 0, .. }));
    v.in_flight = Some(first.clone());
    // The scroll: pages 18-20 (17..20) show.
    let top = l.pages[17].y;
    v.frame(800., l.pages[19].bottom() - top, top, 1.);
    assert_eq!(v.showing, 17..20);
    v.absorb(Done {
        op: first,
        answer: Answer::Pixels(raster(4)),
        lost: false,
    });
    let mut asked = Vec::new();
    while let Some(op) = v.next_job() {
        asked.push(op.clone());
        let answer = match op {
            Op::Pixels { .. } => Answer::Pixels(raster(4)),
            Op::Text { .. } => Answer::Text(page_text(&["x"])),
            _ => unreachable!(),
        };
        v.absorb(Done {
            op,
            answer,
            lost: false,
        });
    }
    let pages: Vec<usize> = asked
        .iter()
        .map(|op| match op {
            Op::Pixels { page, .. } | Op::Text { page } => *page,
            _ => usize::MAX,
        })
        .collect();
    assert!(pages.iter().all(|p| (17..=20).contains(p)), "{asked:?}");
    // Showing pages' pixels first, then their text, then the page after.
    assert!(matches!(asked[0], Op::Pixels { page: 17, .. }));
    assert!(matches!(asked[3], Op::Text { page: 17 }));
    assert!(matches!(asked.last(), Some(Op::Pixels { page: 20, .. })));
}

#[test]
fn a_new_zoom_asks_again_and_a_failed_page_never_does() {
    let mut v = view(2);
    v.frame(900., 2000., 0., 1.);
    drawn(&mut v, 0, 4);
    v.absorb(Done {
        op: Op::Pixels { page: 1, scale: 1. },
        answer: Answer::Failed(worker::TOO_LONG.into()),
        lost: false,
    });
    v.texts = vec![Some(page_text(&["a"])), Some(page_text(&["b"]))];
    assert_eq!(v.next_job(), None);
    assert_eq!(v.page_state(0), "drawn");
    assert_eq!(v.page_state(1), format!("failed: {}", worker::TOO_LONG));
    assert_eq!(
        v.placeholder(1).unwrap(),
        format!("this page could not be drawn: {}", worker::TOO_LONG)
    );
    v.set_zoom(2.);
    v.frame(900., 2000., 0., 1.);
    // The old raster stays (drawn stretched) until the new one is in.
    assert!(v.rasters[0].is_some());
    match v.next_job() {
        Some(Op::Pixels { page: 0, scale }) => assert_eq!(scale, 2. * PX_PER_PT),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rasters_past_the_budget_go_least_recently_shown_first_never_a_wanted_one() {
    let mut v = view(6);
    v.viewport.set((900., 300.));
    for p in 0..6 {
        let top = v.layout().pages[p].y;
        v.frame(900., 300., top, 1.);
        drawn(&mut v, p, 100);
    }
    // Now showing 5 (and wanting it): budget for two rasters.
    v.evict(200);
    let kept: Vec<usize> = (0..6).filter(|p| v.rasters[*p].is_some()).collect();
    assert_eq!(kept, vec![4, 5]);
    // Every image let go of is handed back for the GPU to free.
    assert_eq!(v.released.len(), 4);
    // A budget below what is wanted keeps what is wanted.
    v.evict(0);
    assert!(v.rasters[5].is_some());
    // A new raster of a page drops the old one's image too.
    drawn(&mut v, 5, 100);
    assert_eq!(v.released.len(), 6);
}

// ---- selection ------------------------------------------------------------------

fn two_pages() -> PdfView {
    let mut v = view(2);
    v.viewport.set((900., 2000.));
    v.texts = vec![
        Some(page_text(&["hello world", "second line here"])),
        Some(page_text(&["next page"])),
    ];
    v
}

#[test]
fn a_drag_selects_in_reading_order_within_and_across_pages() {
    let mut v = two_pages();
    let (x, y) = at(&v, 0, 73., 105.);
    v.press(x, y, 1, false);
    assert_eq!(v.selection_text(), None, "a press alone selects nothing");
    let (x, y) = at(&v, 0, 72. + 6. * 9. + 1., 105.);
    v.drag_to(x, y);
    // To the "l" under the pointer, inclusive.
    assert_eq!(v.selection_text().as_deref(), Some("hello worl"));
    // Backwards from the anchor.
    let (x, y) = at(&v, 0, 73., 125.);
    v.drag_to(x, y);
    // From "s" of "second" up to the anchor "h".
    assert_eq!(v.selection_text().as_deref(), Some("hello world\ns"));
    // Onto the next page: the pages joined by a line break.
    let (x, y) = at(&v, 1, 72. + 6. * 5. + 1., 105.);
    v.drag_to(x, y);
    assert_eq!(
        v.selection_text().as_deref(),
        Some("hello world\nsecond line here\nnext p")
    );
    v.release();
    assert!(v.drag.is_none());
}

#[test]
fn a_double_press_takes_a_word_and_a_triple_a_line() {
    let mut v = two_pages();
    let (x, y) = at(&v, 0, 72. + 6. * 7., 105.);
    v.press(x, y, 2, false);
    assert_eq!(v.selection_text().as_deref(), Some("world"));
    let (x, y) = at(&v, 0, 73., 125.);
    v.press(x, y, 3, false);
    assert_eq!(v.selection_text().as_deref(), Some("second line here"));
    // Shift extends from the anchor.
    let (x, y) = at(&v, 1, 73., 105.);
    v.press(x, y, 1, true);
    assert_eq!(v.selection_text().as_deref(), Some("second line here\nn"));
}

#[test]
fn select_all_waits_for_every_pages_text() {
    let mut v = two_pages();
    v.texts[1] = None;
    v.select_all();
    assert!(v.want_all_text);
    assert_eq!(v.selection_text(), None, "page 2's text is not in yet");
    assert_eq!(v.next_job(), Some(Op::Text { page: 1 }));
    v.texts[1] = Some(page_text(&["next page"]));
    assert_eq!(
        v.selection_text().as_deref(),
        Some("hello world\nsecond line here\nnext page")
    );
    // A page whose text could not be read adds nothing.
    v.texts[1] = None;
    v.text_failed.insert(1, "bad".into());
    assert_eq!(
        v.selection_text().as_deref(),
        Some("hello world\nsecond line here")
    );
}

/// FIX r1: a Ctrl+C that waits for text copies the selection it was pressed
/// on once the text is in, and nothing when the selection changed first.
#[test]
fn a_waiting_copy_copies_its_own_selection_or_nothing() {
    // It waits, then the text comes: the whole selection, once.
    let mut v = two_pages();
    v.texts[1] = None;
    v.select_all();
    assert_eq!(v.copy(), None);
    assert!(v.copy_waits());
    assert_eq!(v.take_pending_copy(), None, "page 2's text is not in yet");
    assert!(v.copy_waits());
    v.texts[1] = Some(page_text(&["next page"]));
    assert_eq!(
        v.take_pending_copy().as_deref(),
        Some("hello world\nsecond line here\nnext page")
    );
    assert!(!v.copy_waits());
    assert_eq!(v.take_pending_copy(), None);

    // Escape lets the selection go: the copy goes with it.
    let mut v = two_pages();
    v.texts[1] = None;
    v.select_all();
    assert_eq!(v.copy(), None);
    v.sel = None;
    assert!(!v.copy_waits());
    v.texts[1] = Some(page_text(&["next page"]));
    assert_eq!(v.take_pending_copy(), None);
    assert_eq!(v.copy_pending, None, "dropped, so no poll stays due");

    // A new selection before the text comes: it is never copied unasked.
    let mut v = two_pages();
    v.texts[1] = None;
    v.select_all();
    assert_eq!(v.copy(), None);
    let (x, y) = at(&v, 0, 72. + 6. * 7., 105.);
    v.press(x, y, 2, false);
    assert_eq!(v.selection_text().as_deref(), Some("world"));
    assert!(!v.copy_waits());
    assert_eq!(v.take_pending_copy(), None);
    // Its own Ctrl+C copies at once.
    assert_eq!(v.copy().as_deref(), Some("world"));
}

/// FIX r3: whether a selection waits for text is told without building it,
/// and agrees with `selection_text`: a failed page counts as read in both.
#[test]
fn a_selection_waits_exactly_when_its_text_is_not_all_in() {
    let mut v = two_pages();
    assert!(!v.selection_waits(), "no selection");
    v.select_all();
    assert!(!v.selection_waits());
    assert!(v.selection_text().is_some());
    v.texts[1] = None;
    assert!(v.selection_waits());
    assert!(v.selection_text().is_none());
    v.text_failed.insert(1, "bad".into());
    assert!(!v.selection_waits());
    assert!(v.selection_text().is_some());
    // A selection on page 1 alone does not wait on page 2.
    v.text_failed.clear();
    let (x, y) = at(&v, 0, 73., 105.);
    v.press(x, y, 2, false);
    assert!(!v.selection_waits());
    assert_eq!(v.selection_text().as_deref(), Some("hello"));
}

// ---- Find -------------------------------------------------------------------------

#[test]
fn find_counts_steps_and_wraps_over_pages() {
    let mut v = two_pages();
    v.texts[1] = Some(page_text(&["Hello again", "say hello"]));
    v.find.field.text = "hello".into();
    v.find_changed();
    v.refind();
    assert_eq!(v.find.hits.len(), 3);
    assert_eq!(v.find_status(), "3 found");
    v.find_step(true);
    assert_eq!(v.find_status(), "1 of 3");
    assert_eq!(v.selection_text().as_deref(), Some("hello"));
    v.find_step(true);
    v.find_step(true);
    assert_eq!(v.find.hits[v.find.current.unwrap()].page, 1);
    assert_eq!(v.find_status(), "3 of 3");
    v.find_step(true);
    assert_eq!(v.find_status(), "1 of 3", "wraps to the first");
    v.find_step(false);
    assert_eq!(v.find_status(), "3 of 3", "and back");
    // Match case.
    v.find.case = true;
    v.find_changed();
    v.refind();
    assert_eq!(v.find.hits.len(), 2);
    // Whole words.
    v.find.case = false;
    v.find.whole = true;
    v.find.field.text = "hell".into();
    v.find_changed();
    v.refind();
    assert_eq!(v.find_status(), "No matches");
}

/// FIX r2: a page's text coming in searches that page alone; the hits end
/// in page order as a full search's, and the current hit stays the same.
#[test]
fn a_page_text_coming_in_searches_that_page_alone() {
    let mut v = view(4);
    v.find.field.text = "hello".into();
    v.find_changed();
    let texts = [
        page_text(&["hello one", "hello two"]),
        page_text(&["nothing"]),
        page_text(&["hello three"]),
        page_text(&["say hello", "hello again"]),
    ];
    let arrive = |v: &mut PdfView, page: usize| {
        let before = v.find.searched;
        v.absorb(Done {
            op: Op::Text { page },
            answer: Answer::Text(texts[page].clone()),
            lost: false,
        });
        assert_eq!(v.find.searched, before + 1, "page {page} alone");
    };
    arrive(&mut v, 2);
    v.find_step(true);
    let current = v.find.hits[v.find.current.unwrap()].clone();
    assert_eq!(current.page, 2);
    for page in [3, 0, 1] {
        arrive(&mut v, page);
        assert_eq!(v.find.hits[v.find.current.unwrap()], current, "same hit");
    }
    let spliced = v.find.hits.clone();
    v.refind();
    assert_eq!(spliced, v.find.hits);
    assert_eq!(v.find.hits.len(), 5);
    assert_eq!(v.find.hits[v.find.current.unwrap()], current);
}

#[test]
fn find_says_what_it_has_not_read_and_what_is_incomplete() {
    let mut v = view(3);
    v.texts[0] = Some(page_text(&["alpha beta"]));
    v.find.field.text = "beta".into();
    v.refind();
    assert_eq!(v.find_status(), "1 found · reading 1 of 3 pages");
    // Find asks for the pages it has not read.
    v.frame(900., 10., 0., 1.);
    drawn(&mut v, 0, 4);
    assert_eq!(v.next_job(), Some(Op::Text { page: 1 }));
    let mut cut = (*page_text(&["beta"])).clone();
    cut.truncated = true;
    v.texts[1] = Some(Arc::new(cut));
    v.text_failed.insert(2, "bad".into());
    v.refind();
    assert_eq!(
        v.find_status(),
        "2 found · page 2's text is incomplete (and 1 more)"
    );
}

#[test]
fn pdf_read_reports_pages_outline_selection_and_find() {
    let mut v = two_pages();
    v.meta.outline = vec![
        Bookmark {
            title: "Top".into(),
            page: Some(0),
            depth: 0,
        },
        Bookmark {
            title: "Nowhere".into(),
            page: None,
            depth: 1,
        },
    ];
    v.meta.title = Some("A title".into());
    drawn(&mut v, 0, 4);
    v.find.field.text = "line".into();
    v.refind();
    v.find_step(true);
    let j = v.read_json();
    assert_eq!(j.get_usize("page_count"), Some(2));
    assert_eq!(j.get_str("page_box"), Some("1 / 2"));
    assert_eq!(j.get_usize("zoom"), Some(100));
    assert_eq!(j.get_str("selection"), Some("line"));
    assert_eq!(j.get_str("find_status"), Some("1 of 1"));
    assert_eq!(j.get_str("title"), Some("A title"));
    let pages = j.get("pages").unwrap().as_array().unwrap();
    assert_eq!(pages[0].get_str("state"), Some("drawn"));
    assert_eq!(pages[1].get_str("state"), Some("pending"));
    assert_eq!(pages[0].get_str("text"), Some("read"));
    let outline = j.get("outline").unwrap().as_array().unwrap();
    assert_eq!(outline[0].get_usize("page"), Some(1));
    assert_eq!(outline[1].get("page"), Some(&Json::Null));
    assert_eq!(outline[1].get_usize("depth"), Some(1));
}

// ---- the corpus, through the worker's protocol -------------------------------------

fn corpus(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    if name == "source" {
        root.join("word-import/source.pdf")
    } else {
        root.join(format!("pdf-view/{name}.pdf"))
    }
}

/// Every corpus file, by its reference name.
pub(crate) const CORPUS: [&str; 8] = [
    "text", "columns", "vector", "scanned", "rotated", "slides", "source", "annots",
];

/// The slice 2 files without a reference rendering (#2255): checked by what
/// they hold (`form_tests`), and in the mutation test.
pub(crate) const UNREFERENCED: [&str; 4] = ["links", "annots-noap", "forms", "signed"];

/// The slice 3 files (#2256), each with a reference rendering: encrypted,
/// an incremental update, object and cross-reference streams, damaged.
/// The damaged files' references are of the files before the damage.
pub(crate) const HARD: [&str; 10] = [
    "enc-rc4-40",
    "enc-rc4-128",
    "enc-aes-128",
    "enc-aes-256",
    "enc-owner-only",
    "incremental",
    "objstm",
    "damaged-offsets",
    "damaged-noxref",
    "damaged-truncated",
];

/// A file nothing can be read from (#2256): a header and no objects.
pub(crate) const UNREADABLE: &str = "damaged-unreadable";

/// The encrypted files (#2256): their made-up user and owner passwords
/// (corpus/pdf-view/README.md; the generator's `ENCRYPTED`), their cipher
/// as the status line names it, and what their page says.
pub(crate) const ENCRYPTED: [(&str, &str, &str, &str, &str); 5] = [
    (
        "enc-rc4-40",
        "user-rc4-40",
        "owner-rc4-40",
        "RC4 40-bit",
        "Encrypted with RC4 40",
    ),
    (
        "enc-rc4-128",
        "user-rc4-128",
        "owner-rc4-128",
        "RC4 128-bit",
        "Encrypted with RC4 128",
    ),
    (
        "enc-aes-128",
        "user-aes-128",
        "owner-aes-128",
        "AES 128-bit",
        "Encrypted with AES 128",
    ),
    (
        "enc-aes-256",
        "user-aes-256",
        "owner-aes-256",
        "AES 256-bit",
        "Encrypted with AES 256",
    ),
    (
        "enc-owner-only",
        "",
        "owner-only",
        "AES 128-bit",
        "Encrypted with AES 128, owner password only",
    ),
];

/// The user password a corpus file opens with (`None`: it needs none).
pub(crate) fn user_password(name: &str) -> Option<&'static str> {
    ENCRYPTED
        .iter()
        .find(|e| e.0 == name)
        .map(|e| e.1)
        .filter(|p| !p.is_empty())
}

pub(crate) fn corpus_path(name: &str) -> PathBuf {
    corpus(name)
}

fn worker(name: &str) -> Worker {
    Worker::new(&corpus(name), Spawn::InProcess, Duration::from_secs(60))
}

/// `info`, unlocked with the file's user password when it needs one.
fn info(w: &mut Worker) -> DocMeta {
    match w.ask(&Op::Info) {
        Answer::Info(m) => m,
        Answer::Locked { wrong: false } => {
            let name = w
                .path_for_tests()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let pw = user_password(&name).unwrap_or_else(|| panic!("{name}: locked"));
            match w.unlock(Secret::new(pw.into())) {
                Answer::Info(m) => m,
                other => panic!("{name}: {other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

fn text_of(w: &mut Worker, page: usize) -> Arc<PageText> {
    match w.ask(&Op::Text { page }) {
        Answer::Text(t) => t,
        other => panic!("page {}: {other:?}", page + 1),
    }
}

/// The comparison is of 3 x 3 pixel blocks: each block's mean colour, ours
/// against the reference's. Renderers anti-alias differently and place
/// text at fractional positions differently (hayro against poppler's
/// Splash), so single pixels along every edge differ; a missing glyph,
/// line or picture still changes its blocks. A block differs when one of
/// its mean channels differs by more than [`CHANNEL`]; at most [`SHARE`] of
/// a page's blocks may differ. Measured on the corpus before choosing
/// (rev eeee51f, poppler 26.01): every page's own render differs on at most
/// 0.48 % of blocks (`scanned`), a blank page on at least 1.47 %
/// (`source`, a sparse page). Single pixels separate them far worse: a
/// dense page with 14.85 pt line pitch differs on 7.6 % of its pixels, a
/// blank one on 11.7 %.
const CHANNEL: u32 = 48;
const BLOCK: u32 = 3;
const SHARE: f64 = 0.01;

/// The share of `BLOCK`-square blocks of `ours` (BGRA, or blank white)
/// that differ from `reference`'s.
fn differing(ours: &[u8], reference: &image::RgbaImage, white: bool) -> f64 {
    let (w, h) = reference.dimensions();
    let (bw, bh) = (w / BLOCK, h / BLOCK);
    let mut bad = 0usize;
    for by in 0..bh {
        for bx in 0..bw {
            let (mut o, mut r) = ([0u32; 3], [0u32; 3]);
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let p = reference.get_pixel(x, y).0;
                    let i = ((y * w + x) * 4) as usize;
                    // Ours are BGRA.
                    let q = if white {
                        [255, 255, 255]
                    } else {
                        [ours[i + 2], ours[i + 1], ours[i]]
                    };
                    for c in 0..3 {
                        o[c] += u32::from(q[c]);
                        r[c] += u32::from(p[c]);
                    }
                }
            }
            let n = BLOCK * BLOCK;
            if (0..3).any(|c| (o[c] / n).abs_diff(r[c] / n) > CHANNEL) {
                bad += 1;
            }
        }
    }
    bad as f64 / (bw * bh) as f64
}

/// Criterion 9: each page drawn at one pixel a point matches `pdftoppm -r
/// 72`'s within the stated tolerance ([`differing`]), and a blank page does
/// not (so each reference has enough on it for the comparison to mean
/// something).
#[test]
fn every_corpus_page_matches_its_reference_rendering() {
    let refs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/pdf-view/ref");
    let mut report = Vec::new();
    for name in CORPUS.iter().chain(&HARD) {
        let mut w = worker(name);
        let m = info(&mut w);
        for page in 0..m.pages.len() {
            let path = refs.join(format!("{name}-{}.png", page + 1));
            let reference = image::open(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                .to_rgba8();
            let r = match w.ask(&Op::Pixels { page, scale: 1. }) {
                Answer::Pixels(r) => r,
                other => panic!("{name} page {}: {other:?}", page + 1),
            };
            assert_eq!(
                (r.w, r.h),
                reference.dimensions(),
                "{name} page {}: size",
                page + 1
            );
            let ours = differing(&r.bgra, &reference, false);
            let blank = differing(&r.bgra, &reference, true);
            report.push(format!(
                "{name}-{}: {:.2} % differ, blank {:.2} %",
                page + 1,
                ours * 100.,
                blank * 100.
            ));
            assert!(ours <= SHARE, "{name} page {}: {ours} > {SHARE}", page + 1);
            assert!(
                blank > SHARE,
                "{name} page {}: a blank page passes ({blank}); the reference has too little on it",
                page + 1
            );
        }
    }
    eprintln!("{}", report.join("\n"));
}

/// Criterion 10: the text in reading order, Unicode included, and a search
/// on each file, as the worker hands them over.
#[test]
fn every_corpus_file_reads_and_searches_its_text() {
    let mut w = worker("text");
    let p1 = text_of(&mut w, 0).plain_text();
    for s in [
        "The PDF view corpus",
        "Hello from the first page.",
        "Привет, мир! Съешь же ещё этих мягких французских булок,",
        "Широкая электрификация южных губерний.",
        "Καλημέρα κόσμε. Ξεσκεπάζω την ψυχοφθόρα βδελυγμία.",
    ] {
        assert!(p1.contains(s), "{s:?} not in {p1:?}");
    }
    assert_eq!(
        text_of(&mut w, 0).find("привет").len(),
        1,
        "case folds Cyrillic"
    );
    assert_eq!(text_of(&mut w, 0).find("ΚΑΛΗΜΈΡΑ").len(), 1, "and Greek");
    assert_eq!(text_of(&mut w, 0).find("wizards").len(), 1);
    assert_eq!(text_of(&mut w, 1).find("wizards").len(), 1);
    assert_eq!(text_of(&mut w, 1).find_opts("wizard", false, true).len(), 1);

    // Two columns: the whole left one, then the right one.
    let mut w = worker("columns");
    let cols = text_of(&mut w, 0).plain_text();
    let alpha = cols.rfind("alpha").expect("left column");
    let right = cols.find("Right column starts here").expect("right column");
    assert!(alpha < right, "the left column is read first: {cols}");
    assert!(cols.contains("first line of text.\nThe left column is read first,"));

    for (name, page, s) in [
        ("vector", 0, "Vector diagram"),
        ("scanned", 0, "A scanned page and a picture."),
        ("rotated", 0, "Rotated page"),
        ("slides", 1, "Revenue by region"),
        ("source", 0, "Привет"),
        ("source", 0, "€"),
    ] {
        let mut w = worker(name);
        let t = text_of(&mut w, page);
        assert!(
            !t.find(s).is_empty(),
            "{name}: {s:?} not found in {:?}",
            t.plain_text()
        );
    }
}

/// #2256 criteria 1, 6-8: each slice 3 file reads its text, unlocked with
/// its password when it needs one.
#[test]
fn every_hard_corpus_file_reads_its_text() {
    let label = |name: &str| ENCRYPTED.iter().find(|e| e.0 == name).map(|e| e.4);
    for name in HARD {
        let mut w = worker(name);
        let m = info(&mut w);
        let want = match name {
            "incremental" => "Latest revision",
            "objstm" => "Object and cross-reference streams",
            "damaged-offsets" => "Damaged: every offset is wrong",
            "damaged-noxref" => "Damaged: no cross-reference table",
            "damaged-truncated" => "Damaged: the file is cut short",
            name => label(name).expect("an encrypted file"),
        };
        let t = text_of(&mut w, 0).plain_text();
        assert!(t.contains(want), "{name}: {want:?} not in {t:?}");
        assert!(!m.pages.is_empty(), "{name}");
    }
}

/// #2256 criterion 6: an incremental update's latest revision: the page
/// it rewrote and the page it added. A reader of the first revision would
/// show one page saying "First revision".
#[test]
fn incremental_shows_the_latest_revision() {
    let mut w = worker("incremental");
    let m = info(&mut w);
    assert_eq!(m.pages.len(), 2);
    let p1 = text_of(&mut w, 0).plain_text();
    assert!(p1.contains("Latest revision"), "{p1:?}");
    assert!(!p1.contains("First revision"), "{p1:?}");
    assert!(
        text_of(&mut w, 1)
            .plain_text()
            .contains("Added in the update")
    );
}

/// #2256 criterion 8: a file whose cross-reference table was rebuilt from
/// its objects (none left, or cut off) is saved anew, never appended to
/// (its chain is broken): `info` says so. PdfCraft finds the objects of
/// one whose offsets are only wrong without a rebuild, and appends to it.
#[test]
fn a_rebuilt_file_is_saved_in_full() {
    for name in ["damaged-noxref", "damaged-truncated"] {
        let mut w = worker(name);
        assert!(!info(&mut w).form.incremental, "{name}");
    }
    for name in ["damaged-offsets", "objstm", "incremental"] {
        let mut w = worker(name);
        assert!(info(&mut w).form.incremental, "{name}");
    }
}

/// #2256 criterion 9: a file nothing can be read from is a failed PDF tab
/// whose body says why, not a blank view. This checks the tab and the
/// reason `pdf_failed_reason` gives; the drawing is `main.rs`'s
/// `pdf_failed_body` arm, which no test renders.
#[test]
fn unreadable_pdf_says_why_in_its_body() {
    let path = corpus(UNREADABLE);
    let tab =
        crate::pdf_view::host::pdf_view_tab_with(&path, None, Spawn::InProcess).expect("a PDF tab");
    assert!(tab.kind == crate::Kind::Pdf);
    assert!(tab.load_failed);
    assert!(matches!(tab.surface, crate::Surface::Placeholder));
    let why = crate::pdf_view::host::pdf_failed_reason(&tab).expect("a reason");
    assert!(why.contains("not a readable PDF"), "{why}");
    assert!(!why.starts_with("load error"), "{why}");
    // FIX r1 2: a Save on it refuses without replacing that reason.
    let mut tab = tab;
    let step = crate::pdf_view::host::pdf_save_step(&mut tab, None);
    let crate::pdf_view::host::PdfSaveStep::Nothing(refusal) = step else {
        panic!("{step:?}")
    };
    crate::pdf_view::host::note_save_refusal(&mut tab, refusal);
    assert_eq!(
        crate::pdf_view::host::pdf_failed_reason(&tab).as_deref(),
        Some(why.as_str())
    );
    // A tab that opened takes the refusal on its status line.
    let mut ok =
        crate::pdf_view::host::pdf_view_tab_with(&corpus("text"), None, Spawn::InProcess).unwrap();
    crate::pdf_view::host::note_save_refusal(&mut ok, crate::pdf_view::host::PDF_NO_CHANGES);
    assert_eq!(ok.status.as_ref(), crate::pdf_view::host::PDF_NO_CHANGES);
    // A tab that opened has none.
    let tab =
        crate::pdf_view::host::pdf_view_tab_with(&corpus("text"), None, Spawn::InProcess).unwrap();
    assert_eq!(crate::pdf_view::host::pdf_failed_reason(&tab), None);
}

#[test]
fn info_reads_sizes_rotation_outline_and_properties() {
    let mut w = worker("rotated");
    let m = info(&mut w);
    assert_eq!(m.pages.len(), 1);
    assert_eq!(
        (m.pages[0].w, m.pages[0].h, m.pages[0].rotation),
        (842., 595., 90)
    );
    let mut w = worker("slides");
    let m = info(&mut w);
    assert_eq!(m.pages.len(), 3);
    assert_eq!((m.pages[0].w, m.pages[0].h), (960., 540.));
    let outline: Vec<(&str, Option<usize>, usize)> = m
        .outline
        .iter()
        .map(|b| (b.title.as_str(), b.page, b.depth))
        .collect();
    assert_eq!(
        outline,
        vec![
            ("Review", Some(0), 0),
            ("Quarterly review", Some(0), 1),
            ("Revenue by region", Some(1), 1),
            ("Next steps", Some(2), 0),
        ]
    );
    assert_eq!(m.title.as_deref(), Some("Slides corpus"));
    let mut w = worker("text");
    let m = info(&mut w);
    assert_eq!(m.pages.len(), 2);
    assert_eq!(m.author.as_deref(), Some("docxy"));
    assert!(m.outline.is_empty());
}

/// A drawn page's rotation: the stored-upright page shows turned a quarter
/// right, so the raster is landscape.
#[test]
fn a_rotated_page_is_drawn_turned() {
    let mut w = worker("rotated");
    match w.ask(&Op::Pixels {
        page: 0,
        scale: 0.5,
    }) {
        Answer::Pixels(r) => assert!(r.w > r.h, "{} x {}", r.w, r.h),
        other => panic!("{other:?}"),
    }
}
