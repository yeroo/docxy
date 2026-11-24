//! Links, annotations and forms in the PDF view (#2255): what the worker
//! reads of the slice 2 corpus (`links`, `annots`, `annots-noap`, `forms`,
//! `signed`), filling and saving through its protocol (in process: the same
//! `serve` as the child), the saved file read back by a new worker and by
//! `lopdf`, and the view's state for links, comments and fields.

use super::form::FormFocus;
use super::worker::{
    Answer, CHANGED, DocMeta, Done, FieldKind, FieldValue, FormState, LinkGo, Op, Spawn, Worker,
};
use super::*;
use crate::open_mode_tests::Scratch;
use std::time::Duration;

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../corpus/pdf-view/{name}.pdf"))
}

fn worker_on(path: &Path) -> Worker {
    Worker::new(path, Spawn::InProcess, Duration::from_secs(60))
}

fn info_of(w: &mut Worker) -> DocMeta {
    match w.ask(&Op::Info) {
        Answer::Info(m) => m,
        other => panic!("{other:?}"),
    }
}

/// A copy of corpus file `name` in `dir`, to fill and save.
fn copy(dir: &Scratch, name: &str) -> PathBuf {
    let p = dir.path(&format!("{name}.pdf"));
    std::fs::copy(corpus(name), &p).unwrap();
    p
}

fn set(w: &mut Worker, name: &str, value: FieldValue) -> FormState {
    match w.ask(&Op::SetField {
        name: name.into(),
        value,
    }) {
        Answer::Form(st) => st,
        other => panic!("{name}: {other:?}"),
    }
}

fn text(t: &str) -> FieldValue {
    FieldValue::Text { text: t.into() }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

fn near4(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| near(*x, y))
}

/// The field named `name`'s value as `pdf-read` reports it.
fn value(m: &[worker::FieldMeta], name: &str) -> String {
    let f = m
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no {name}"));
    field_value_text(f)
}

// ---- links -------------------------------------------------------------------------

/// Criterion 1: every kind of link, in view space (points, y down), the
/// rotated page's turned, with where each goes.
#[test]
fn info_reads_links_in_view_space() {
    let mut w = worker_on(&corpus("links"));
    let m = info_of(&mut w);
    let p1: Vec<_> = m.links.iter().filter(|l| l.page == 0).collect();
    assert_eq!(p1.len(), 10, "{p1:#?}");
    // Row i of page 1 is [72, 740 - 30 i, 330, 758 - 30 i] in user space.
    let row = |i: f32| [72., 842. - 758. + 30. * i, 330., 842. - 740. + 30. * i];
    for (i, l) in p1.iter().take(9).enumerate() {
        assert!(near4(l.rect, row(i as f32)), "link {i}: {:?}", l.rect);
    }
    assert_eq!(
        p1[0].go,
        LinkGo::Uri {
            uri: "https://example.com/docxy".into()
        }
    );
    assert_eq!(
        p1[1].go,
        LinkGo::Uri {
            uri: "mailto:docs@example.com".into()
        }
    );
    // /XYZ 72 600 2 on page 2: its point and zoom.
    match &p1[2].go {
        LinkGo::Page {
            page: 1,
            x: Some(x),
            y: Some(y),
            zoom: Some(z),
            fit_width: false,
        } => assert!(near(*x, 72.) && near(*y, 242.) && near(*z, 2.)),
        other => panic!("{other:?}"),
    }
    // A named destination in the /Names tree: /FitH 400 on the turned page
    // 3, where the user y is the displayed x.
    match &p1[3].go {
        LinkGo::Page {
            page: 2,
            x: Some(x),
            y: None,
            fit_width: true,
            ..
        } => assert!(near(*x, 400.)),
        other => panic!("{other:?}"),
    }
    // A GoTo naming a /Dests entry: /FitR's top-left corner.
    match &p1[4].go {
        LinkGo::Page {
            page: 1,
            x: Some(x),
            y: Some(y),
            ..
        } => assert!(near(*x, 100.) && near(*y, 542.)),
        other => panic!("{other:?}"),
    }
    // A GoTo with an explicit /Fit: the page's top.
    assert!(matches!(
        &p1[5].go,
        LinkGo::Page { page: 1, x: None, y: Some(y), .. } if near(*y, 0.)
    ));
    assert_eq!(
        p1[6].go,
        LinkGo::Other {
            what: "a script".into()
        }
    );
    assert!(matches!(&p1[7].go, LinkGo::Other { what } if what.contains("Launch")));
    assert!(matches!(&p1[8].go, LinkGo::Uri { uri } if uri.starts_with("javascript:")));
    // The quad link: active over its quadrilateral, in view space.
    assert_eq!(p1[9].quads.len(), 1);
    let q = p1[9].quads[0];
    assert!(
        near4([q[0], q[1], q[2], q[3]], [72., 392., 250., 392.]),
        "{q:?}"
    );
    // Page 3 is /Rotate 90: user (x, y) shows at (y, x).
    let p3: Vec<_> = m.links.iter().filter(|l| l.page == 2).collect();
    assert_eq!(p3.len(), 1);
    assert!(
        near4(p3[0].rect, [700., 72., 716., 300.]),
        "{:?}",
        p3[0].rect
    );
    assert_eq!(
        p3[0].go,
        LinkGo::Page {
            page: 0,
            x: None,
            y: None,
            zoom: None,
            fit_width: false
        }
    );
}

fn links_view() -> PdfView {
    let mut w = worker_on(&corpus("links"));
    PdfView::new(&corpus("links"), info_of(&mut w), None)
}

/// The middle of link `i` of page 1, as content coordinates.
fn link_at(v: &PdfView, i: usize) -> (f32, f32) {
    let r = v.links[0][i].rect;
    let pr = v.layout().pages[0];
    let k = v.zoom * PX_PER_PT;
    (pr.x + (r[0] + r[2]) / 2. * k, pr.y + (r[1] + r[3]) / 2. * k)
}

/// Criterion 2: a click follows a link, a drag from one selects; only
/// http, https and mailto open; an internal link goes to its place.
#[test]
fn a_click_follows_a_link_and_a_drag_from_one_does_not() {
    let mut v = links_view();
    v.frame(900., 700., 0., 1.);
    let (x, y) = link_at(&v, 0);
    assert_eq!(v.under_at(x, y), Under::Link(0, 0));
    v.press_at(x, y, 1, false, false);
    v.release_at();
    assert_eq!(
        v.open_url.take().as_deref(),
        Some("https://example.com/docxy")
    );
    // Pressed, then dragged past the slop: no link, a drag.
    v.press_at(x, y, 1, false, false);
    v.drag_at(x + 30., y);
    v.release_at();
    assert_eq!(v.open_url, None);
    // A javascript: address and a script are not followed, and say so.
    let (x, y) = link_at(&v, 8);
    v.press_at(x, y, 1, false, false);
    v.release_at();
    assert_eq!(v.open_url, None);
    assert!(
        v.notice
            .take()
            .unwrap()
            .contains("not a web or mail address")
    );
    v.follow_link(0, 6);
    assert!(v.notice.take().unwrap().contains("a script"));
    v.follow_link(0, 1);
    assert_eq!(
        v.open_url.take().as_deref(),
        Some("mailto:docs@example.com")
    );
    assert_eq!(link_tip_of(&v, 2), "Go to page 2");
    assert_eq!(link_tip_of(&v, 0), "https://example.com/docxy");
}

fn link_tip_of(v: &PdfView, i: usize) -> String {
    form::link_tip(&v.links[0][i].go, &v.meta.pages)
}

/// Criterion 2: an /XYZ link sets its zoom and puts its point at the
/// top-left; /FitH fits the width.
#[test]
fn an_internal_link_goes_to_its_place() {
    let mut v = links_view();
    v.frame(900., 700., 0., 1.);
    v.follow_link(0, 2);
    assert!(near(v.zoom, 2.));
    let pr = v.layout().pages[1];
    let k = v.zoom * PX_PER_PT;
    let (_, y) = v.scroll_to.get().expect("a scroll");
    assert!(near(y, pr.y + 242. * k), "{y} vs {}", pr.y + 242. * k);
    assert_eq!(v.current(), 1);
    // FitH on page 3: the width fits, page 3 shows.
    v.follow_link(0, 3);
    assert!(near(v.zoom, fit_width(&v.meta.pages, 900.)));
    assert_eq!(v.current(), 2);
}

/// A link active over its quadrilateral only: a point in its rectangle
/// but outside the quad is no link.
#[test]
fn a_quad_link_is_active_over_its_quads_only() {
    let mut v = links_view();
    v.frame(900., 700., 0., 1.);
    let pr = v.layout().pages[0];
    let k = v.zoom * PX_PER_PT;
    let at = |x: f32, y: f32| (pr.x + x * k, pr.y + y * k);
    let (x, y) = at(100., 400.);
    assert_eq!(v.under_at(x, y), Under::Link(0, 9));
    let (x, y) = at(350., 430.);
    assert_eq!(v.under_at(x, y), Under::Nothing);
}

// ---- annotations -------------------------------------------------------------------

/// Criterion 4: the comments with text, by where they show, not the hidden
/// one or a free text's; their author and date.
#[test]
fn notes_are_the_comments_with_text_not_hidden_ones() {
    let mut w = worker_on(&corpus("annots"));
    let m = info_of(&mut w);
    let kinds: Vec<&str> = m.notes.iter().map(|n| n.subtype.as_str()).collect();
    assert_eq!(kinds, ["Highlight", "Text", "StrikeOut"]);
    let note = &m.notes[1];
    assert_eq!(note.contents, "A sticky note");
    assert_eq!(note.author.as_deref(), Some("Reviewer"));
    assert_eq!(note.date.as_deref(), Some("2026-11-24 12:00"));
    assert!(near4(note.rect, [320., 78., 344., 102.]), "{:?}", note.rect);
    assert!(!m.notes.iter().any(|n| n.contents.contains("Hidden")));
}

/// Criterion 4: hovering a comment shows its pop-up, leaving hides it; a
/// click pins it open until a second click.
#[test]
fn a_comments_popup_shows_on_hover_and_pins_on_a_click() {
    let mut w = worker_on(&corpus("annots"));
    let mut v = PdfView::new(&corpus("annots"), info_of(&mut w), None);
    v.frame(900., 700., 0., 1.);
    let pr = v.layout().pages[0];
    let k = v.zoom * PX_PER_PT;
    let (x, y) = (pr.x + 332. * k, pr.y + 90. * k);
    assert!(v.hover(x, y));
    assert_eq!(
        v.popup,
        Some(NotePopup {
            page: 0,
            note: 1,
            pinned: false
        })
    );
    v.hover(x, y + 300.);
    assert_eq!(v.popup, None);
    v.press_at(x, y, 1, false, false);
    assert!(v.popup.is_some_and(|p| p.pinned));
    v.hover(x, y + 300.);
    assert!(v.popup.is_some(), "pinned stays");
    v.press_at(x, y, 1, false, false);
    assert_eq!(v.popup, None);
}

/// The 13 kinds' rectangles (user space) in `annots-noap.pdf`, as
/// `scripts/make-pdf-view-corpus.py` writes them.
const NOAP_RECTS: [(&str, [f32; 4]); 13] = [
    ("Highlight", [72., 758., 272., 774.]),
    ("Underline", [72., 724., 272., 742.]),
    ("StrikeOut", [72., 694., 272., 712.]),
    ("Squiggly", [72., 664., 272., 682.]),
    ("Text", [320., 740., 344., 764.]),
    ("FreeText", [320., 650., 520., 700.]),
    ("Ink", [72., 560., 272., 640.]),
    ("Stamp", [320., 560., 520., 630.]),
    ("Square", [72., 460., 172., 540.]),
    ("Circle", [200., 460., 300., 540.]),
    ("Line", [320., 470., 520., 530.]),
    ("Polygon", [72., 340., 272., 440.]),
    ("PolyLine", [320., 340., 520., 440.]),
];

/// Pixels in a user-space rectangle of an A4 page (BGRA at one pixel a
/// point) that are not white.
fn inked(bgra: &[u8], w: u32, r: [f32; 4]) -> usize {
    let (x0, x1) = (r[0] as u32, r[2] as u32);
    let (y0, y1) = ((842. - r[3]) as u32, (842. - r[1]) as u32);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            if bgra[i..i + 3].iter().any(|c| *c < 200) {
                n += 1;
            }
        }
    }
    n
}

/// Criterion 3(b): every annotation kind without an appearance stream is
/// drawn (PdfCraft's appearance), and the same rectangles are blank when the
/// file is drawn as it is, so the check can fail; but a highlight, which the
/// renderer draws by itself. 3(a), the kinds with
/// their appearance streams against poppler, is `annots` in the reference
/// test (`every_corpus_page_matches_its_reference_rendering`): a check that
/// slice 1 already passes.
#[test]
fn every_annotation_kind_without_an_appearance_is_drawn() {
    let path = corpus("annots-noap");
    let mut w = worker_on(&path);
    let r = match w.ask(&Op::Pixels { page: 0, scale: 1. }) {
        Answer::Pixels(r) => r,
        other => panic!("{other:?}"),
    };
    let bytes = Arc::new(std::fs::read(&path).unwrap());
    let mut plain =
        pdfcraft_render::PageRenderer::new(bytes, pdfcraft_render::RenderConfig::default());
    let raw = plain.render(pdfcraft_render::RenderRequest {
        page: 0,
        scale: 1.,
        ..Default::default()
    });
    let raw = worker::straight_bgra(&raw.rgba);
    for (kind, rect) in NOAP_RECTS {
        let ours = inked(&r.bgra, r.w, rect);
        let control = inked(&raw, r.w, rect);
        assert!(ours >= 8, "{kind}: {ours} pixels drawn");
        // hayro draws a highlight without an appearance by itself (measured:
        // the whole rectangle, with or without); every other kind is blank.
        if kind == "Highlight" {
            assert!(control >= 8, "{kind}: hayro no longer draws it alone");
        } else {
            assert_eq!(control, 0, "{kind}: drawn without the missing appearance");
        }
    }
}

// ---- the form ------------------------------------------------------------------------

/// Criterion 5: every field and widget, with its flags, value, options and
/// place; a signed PDF says so.
#[test]
fn info_reads_fields_and_widgets() {
    let mut w = worker_on(&corpus("forms"));
    let m = info_of(&mut w);
    let f = &m.form.fields;
    let names: Vec<&str> = f.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "name", "email", "notes", "pin", "code", "id", "agree", "colour", "size", "city",
            "fruit", "tags", "page2"
        ]
    );
    let by = |n: &str| f.iter().find(|f| f.name == n).unwrap();
    assert_eq!(by("name").max_len, Some(20));
    assert_eq!(by("name").tooltip.as_deref(), Some("Your name"));
    assert!(near4(by("name").widgets[0].rect, [200., 72., 450., 92.]));
    assert!(by("email").required);
    assert!(by("notes").multiline);
    assert!(by("pin").password);
    assert!(by("code").comb);
    assert!(by("id").read_only && by("id").value == ["A-17"]);
    assert!(by("id").widgets[0].read_only);
    assert_eq!(by("agree").kind, FieldKind::Check);
    assert_eq!(by("agree").widgets[0].on_state.as_deref(), Some("Yes"));
    let colour = by("colour");
    assert_eq!(colour.kind, FieldKind::Radio);
    let states: Vec<_> = colour
        .widgets
        .iter()
        .map(|w| w.on_state.clone().unwrap())
        .collect();
    assert_eq!(states, ["Red", "Green", "Blue"]);
    assert!(colour.widgets.iter().all(|w| !w.on));
    assert_eq!(by("size").kind, FieldKind::Combo);
    assert!(!by("size").edit && by("city").edit);
    assert_eq!(by("fruit").kind, FieldKind::List);
    assert!(by("fruit").widgets[0].list.is_some());
    assert!(by("tags").multi);
    assert_eq!(by("tags").options[0], ("r".into(), "Red".into()));
    assert_eq!(by("page2").widgets[0].page, Some(1));
    assert!(!m.form.signed && m.form.incremental);
    let mut w = worker_on(&corpus("signed"));
    let m = info_of(&mut w);
    assert!(m.form.signed);
    assert_eq!(m.form.fields.len(), 2);
}

/// Criterion 6: every kind is filled, and the answer names the pages whose
/// fields changed (page 2's field: page 2).
#[test]
fn set_field_fills_every_kind_and_names_changed_pages() {
    let dir = Scratch::new();
    let path = copy(&dir, "forms");
    let mut w = worker_on(&path);
    info_of(&mut w);
    let st = set(&mut w, "name", text("Ada"));
    assert_eq!(st.pages, [0]);
    assert_eq!(value(&st.fields, "name"), "Ada");
    let st = set(&mut w, "agree", FieldValue::Check { on: true });
    assert_eq!(value(&st.fields, "agree"), "Yes");
    let st = set(&mut w, "colour", FieldValue::Widget { index: 1 });
    assert_eq!(value(&st.fields, "colour"), "Green");
    let st = set(&mut w, "size", FieldValue::Choice { picked: vec![2] });
    assert_eq!(value(&st.fields, "size"), "Large");
    let st = set(&mut w, "city", text("Oslo"));
    assert_eq!(value(&st.fields, "city"), "Oslo");
    let st = set(&mut w, "fruit", FieldValue::Choice { picked: vec![1] });
    assert_eq!(value(&st.fields, "fruit"), "Banana");
    let st = set(&mut w, "tags", FieldValue::Choice { picked: vec![0, 2] });
    assert_eq!(value(&st.fields, "tags"), "Red, Blue");
    let st = set(&mut w, "notes", text("line one\nline two"));
    assert!(value(&st.fields, "notes").contains("line two"));
    let st = set(&mut w, "page2", text("P2"));
    assert_eq!(st.pages, [1]);
    // The page shows it: drawn again, its text reads the value.
    match w.ask(&Op::Text { page: 1 }) {
        Answer::Text(t) => assert!(t.plain_text().contains("P2"), "{}", t.plain_text()),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.edits().len(), 9);
}

/// Criterion 6: a read-only field takes no value, in the worker and in the
/// view (no focus, nothing sent).
#[test]
fn a_read_only_field_refuses_a_value() {
    let mut w = worker_on(&corpus("forms"));
    let m = info_of(&mut w);
    match w.ask(&Op::SetField {
        name: "id".into(),
        value: text("B-2"),
    }) {
        Answer::Failed(why) => assert!(why.contains("read-only"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(w.edits().is_empty());
    let mut v = PdfView::new(&corpus("forms"), m, None);
    let id = v.form.fields.iter().position(|f| f.name == "id").unwrap();
    assert!(!v.focus_widget(id, 0));
    v.fill(id, text("B-2"));
    assert!(v.form_ops.is_empty());
    assert!(v.notice.take().unwrap().contains("read-only"));
    assert!(!v.tab_order().iter().any(|(f, _)| *f == id));
}

/// The value of field `name` in a file, as `lopdf` (not PdfCraft) reads it.
fn lopdf_value(bytes: &[u8], name: &str) -> Option<String> {
    let doc = lopdf::Document::load_mem(bytes).expect("lopdf reads it");
    let text = |o: &lopdf::Object| match o {
        lopdf::Object::String(b, _) if b.starts_with(&[0xFE, 0xFF]) => {
            Some(String::from_utf16_lossy(
                &b[2..]
                    .chunks(2)
                    .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                    .collect::<Vec<_>>(),
            ))
        }
        lopdf::Object::String(b, _) | lopdf::Object::Name(b) => {
            Some(String::from_utf8_lossy(b).into_owned())
        }
        _ => None,
    };
    for obj in doc.objects.values() {
        let lopdf::Object::Dictionary(d) = obj else {
            continue;
        };
        if d.get(b"T").ok().and_then(text).as_deref() != Some(name) {
            continue;
        }
        let v = d.get(b"V").ok()?;
        let v = match v {
            lopdf::Object::Reference(id) => doc.get_object(*id).ok()?,
            v => v,
        };
        return match v {
            lopdf::Object::Array(items) => {
                Some(items.iter().filter_map(text).collect::<Vec<_>>().join(","))
            }
            v => text(v),
        };
    }
    None
}

/// Criteria 2 and 8: fill every kind, save in place: the saved bytes are
/// the file as read with an update after it; a new worker reads every value
/// back, and so does `lopdf`; a password is never written, and the open
/// document keeps it.
#[test]
fn a_save_is_an_incremental_update_that_reopens_with_the_values() {
    let dir = Scratch::new();
    let path = copy(&dir, "forms");
    let original = std::fs::read(&path).unwrap();
    let mut w = worker_on(&path);
    info_of(&mut w);
    set(&mut w, "name", text("Ada"));
    set(&mut w, "pin", text("1234"));
    set(&mut w, "agree", FieldValue::Check { on: true });
    set(&mut w, "colour", FieldValue::Widget { index: 1 });
    set(&mut w, "size", FieldValue::Choice { picked: vec![2] });
    set(&mut w, "city", text("Oslo"));
    set(&mut w, "fruit", FieldValue::Choice { picked: vec![1] });
    set(&mut w, "tags", FieldValue::Choice { picked: vec![0, 2] });
    set(&mut w, "notes", text("line one\nline two"));
    set(&mut w, "page2", text("P2"));
    match w.ask(&Op::SaveTo { path: path.clone() }) {
        Answer::Saved(p) => assert_eq!(p, path),
        other => panic!("{other:?}"),
    }
    assert!(w.edits().is_empty());
    let saved = std::fs::read(&path).unwrap();
    assert!(saved.len() > original.len());
    assert!(saved.starts_with(&original), "an incremental update");
    // The tab keeps the password typed.
    match w.ask(&Op::Fields) {
        Answer::Form(st) => assert_eq!(value(&st.fields, "pin"), "1234"),
        other => panic!("{other:?}"),
    }
    // A new worker reads every value back, but the password.
    let mut again = worker_on(&path);
    let m = info_of(&mut again);
    let f = &m.form.fields;
    for (name, want) in [
        ("name", "Ada"),
        ("agree", "Yes"),
        ("colour", "Green"),
        ("size", "Large"),
        ("city", "Oslo"),
        ("fruit", "Banana"),
        ("tags", "Red, Blue"),
        ("page2", "P2"),
    ] {
        assert_eq!(value(f, name), want, "{name}");
    }
    assert!(value(f, "notes").contains("line two"));
    assert_eq!(value(f, "pin"), "", "a password is never saved");
    assert!(matches!(
        again.ask(&Op::Pixels {
            page: 0,
            scale: 0.5
        }),
        Answer::Pixels(_)
    ));
    // Another reader.
    for (name, want) in [
        ("name", "Ada"),
        ("agree", "Yes"),
        ("colour", "Green"),
        ("size", "Large"),
        ("city", "Oslo"),
        ("fruit", "Banana"),
        ("tags", "r,b"),
        ("page2", "P2"),
    ] {
        assert_eq!(lopdf_value(&saved, name).as_deref(), Some(want), "{name}");
    }
    assert!(lopdf_value(&saved, "notes").unwrap().contains("line two"));
    assert!(lopdf_value(&saved, "pin").unwrap_or_default().is_empty());
}

/// Criterion 8: a second save appends to the first; Save As writes a new
/// file and the worker follows it; a save with nothing filled writes the
/// file as it is.
#[test]
fn a_second_save_appends_and_save_as_follows_the_new_file() {
    let dir = Scratch::new();
    let path = copy(&dir, "forms");
    let mut w = worker_on(&path);
    info_of(&mut w);
    set(&mut w, "name", text("Ada"));
    assert!(matches!(
        w.ask(&Op::SaveTo { path: path.clone() }),
        Answer::Saved(_)
    ));
    let first = std::fs::read(&path).unwrap();
    set(&mut w, "email", text("ada@example.com"));
    assert!(matches!(
        w.ask(&Op::SaveTo { path: path.clone() }),
        Answer::Saved(_)
    ));
    let second = std::fs::read(&path).unwrap();
    assert!(second.starts_with(&first) && second.len() > first.len());
    let other = dir.path("copy.pdf");
    assert!(matches!(
        w.ask(&Op::SaveTo {
            path: other.clone()
        }),
        Answer::Saved(_)
    ));
    assert_eq!(
        std::fs::read(&other).unwrap(),
        second,
        "nothing new: the same bytes"
    );
    set(&mut w, "city", text("Oslo"));
    assert!(matches!(
        w.ask(&Op::SaveTo {
            path: other.clone()
        }),
        Answer::Saved(_)
    ));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        second,
        "the first file is left"
    );
    assert_eq!(
        lopdf_value(&std::fs::read(&other).unwrap(), "city").as_deref(),
        Some("Oslo")
    );
}

/// Criterion 8: a damaged file (its cross-reference table unusable) cannot
/// take an update: it is written in full, and reopens with its values.
#[test]
fn a_damaged_file_is_written_in_full_and_reopens_with_its_values() {
    let dir = Scratch::new();
    let path = dir.path("damaged.pdf");
    let mut bytes = std::fs::read(corpus("forms")).unwrap();
    let at = bytes.windows(9).rposition(|w| w == b"startxref").unwrap();
    bytes.truncate(at);
    bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
    std::fs::write(&path, &bytes).unwrap();
    let mut w = worker_on(&path);
    let m = info_of(&mut w);
    assert!(!m.form.incremental, "a rebuilt file is not appended to");
    set(&mut w, "name", text("Ada"));
    assert!(matches!(
        w.ask(&Op::SaveTo { path: path.clone() }),
        Answer::Saved(_)
    ));
    let saved = std::fs::read(&path).unwrap();
    assert!(!saved.starts_with(&bytes), "written in full");
    let mut again = worker_on(&path);
    assert_eq!(value(&info_of(&mut again).form.fields, "name"), "Ada");
    assert_eq!(lopdf_value(&saved, "name").as_deref(), Some("Ada"));
}

/// Criterion 8: a save onto a file another program changed refuses and
/// keeps the values; a write that fails says why and keeps them too.
#[test]
fn a_save_onto_a_changed_file_or_that_cannot_write_keeps_the_values() {
    let dir = Scratch::new();
    let path = copy(&dir, "forms");
    let mut w = worker_on(&path);
    info_of(&mut w);
    set(&mut w, "name", text("Ada"));
    let mut changed = std::fs::read(&path).unwrap();
    changed.extend_from_slice(b"\n% another program was here\n");
    std::fs::write(&path, &changed).unwrap();
    match w.ask(&Op::SaveTo { path: path.clone() }) {
        Answer::Failed(why) => assert_eq!(why, CHANGED),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), changed, "left as it is");
    assert_eq!(w.edits().len(), 1);
    let nowhere = dir.path("no such folder/out.pdf");
    match w.ask(&Op::SaveTo { path: nowhere }) {
        Answer::Failed(why) => assert!(why.contains("could not be written"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.edits().len(), 1);
    // Save As still works.
    let other = dir.path("kept.pdf");
    assert!(matches!(
        w.ask(&Op::SaveTo {
            path: other.clone()
        }),
        Answer::Saved(_)
    ));
    assert_eq!(
        lopdf_value(&std::fs::read(&other).unwrap(), "name").as_deref(),
        Some("Ada")
    );
}

// ---- the view's form state -------------------------------------------------------------

fn forms_view() -> PdfView {
    let mut w = worker_on(&corpus("forms"));
    let mut v = PdfView::new(&corpus("forms"), info_of(&mut w), None);
    v.frame(900., 1.0e5, 0., 1.);
    v
}

fn field_index(v: &PdfView, name: &str) -> usize {
    v.form.fields.iter().position(|f| f.name == name).unwrap()
}

/// The middle of field `name`'s widget `w`, as content coordinates.
fn widget_at(v: &PdfView, name: &str, w: usize) -> (f32, f32) {
    let f = field_index(v, name);
    let widget = &v.form.fields[f].widgets[w];
    let pr = v.layout().pages[widget.page.unwrap()];
    let k = v.zoom * PX_PER_PT;
    let r = widget.rect;
    (pr.x + (r[0] + r[2]) / 2. * k, pr.y + (r[1] + r[3]) / 2. * k)
}

fn sent_fills(v: &PdfView) -> Vec<(String, FieldValue)> {
    v.form_ops
        .iter()
        .filter_map(|op| match op {
            Op::SetField { name, value } => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

/// Criterion 6: typing into a text field (its value selected first, as Tab
/// enters it), MaxLen kept, Esc dropping the typing, Tab committing and
/// going on in the document's tab order.
#[test]
fn a_text_field_types_commits_on_tab_and_cancels_on_esc() {
    let mut v = forms_view();
    let (x, y) = widget_at(&v, "name", 0);
    v.press_at(x, y, 1, false, false);
    let name = field_index(&v, "name");
    assert_eq!(v.focus.as_ref().map(|f| f.field), Some(name));
    v.edit_text(crate::field_edit::Op::Insert(
        "Ada Lovelace, Countess of Lovelace",
    ));
    let typed = v
        .focus
        .as_ref()
        .unwrap()
        .edit
        .as_ref()
        .unwrap()
        .text
        .clone();
    assert_eq!(typed.chars().count(), 20, "MaxLen 20: {typed}");
    v.cancel_focus();
    assert!(v.focus.is_none() && v.form_ops.is_empty(), "Esc drops it");
    v.press_at(x, y, 1, false, false);
    v.edit_text(crate::field_edit::Op::Insert("Ada"));
    assert!(v.tab_field(false));
    assert_eq!(sent_fills(&v), [("name".to_string(), text("Ada"))]);
    let email = field_index(&v, "email");
    assert_eq!(v.focus.as_ref().map(|f| f.field), Some(email));
    // Shift+Tab goes back; nothing typed, nothing sent.
    assert!(v.tab_field(true));
    assert_eq!(v.focus.as_ref().map(|f| f.field), Some(name));
    assert_eq!(sent_fills(&v).len(), 1);
}

/// Criterion 6: the tab order follows the document's, skips the read-only
/// field, and takes a radio group as one stop.
#[test]
fn the_tab_order_skips_read_only_fields_and_stops_once_on_a_radio_group() {
    let v = forms_view();
    let names: Vec<String> = v
        .tab_order()
        .iter()
        .map(|(f, w)| format!("{}:{w}", v.form.fields[*f].name))
        .collect();
    assert_eq!(
        names,
        [
            "name:0", "email:0", "notes:0", "pin:0", "code:0", "agree:0", "colour:0", "size:0",
            "city:0", "fruit:0", "tags:0", "page2:0"
        ]
    );
}

/// Criterion 6: Enter in a multi-line field is a newline, Ctrl+Enter
/// commits it.
#[test]
fn enter_is_a_newline_in_a_multi_line_field() {
    let mut v = forms_view();
    let notes = field_index(&v, "notes");
    v.focus_widget(notes, 0);
    v.edit_text(crate::field_edit::Op::Insert("one"));
    v.form_enter(false);
    v.edit_text(crate::field_edit::Op::Insert("two"));
    assert!(v.form_ops.is_empty());
    v.form_enter(true);
    assert_eq!(sent_fills(&v), [("notes".to_string(), text("one\ntwo"))]);
}

/// Criterion 6: check boxes and radio buttons by click and Space; combo
/// boxes by click, Up/Down and Enter; list boxes single and multi.
#[test]
fn check_radio_combo_and_list_fill_by_mouse_and_keys() {
    let mut v = forms_view();
    let (x, y) = widget_at(&v, "agree", 0);
    v.press_at(x, y, 1, false, false);
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("agree".to_string(), FieldValue::Widget { index: 0 })
    );
    let (x, y) = widget_at(&v, "colour", 2);
    v.press_at(x, y, 1, false, false);
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("colour".to_string(), FieldValue::Widget { index: 2 })
    );
    let colour = field_index(&v, "colour");
    v.focus_widget(colour, 1);
    assert!(v.form_key("space", false, false));
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("colour".to_string(), FieldValue::Widget { index: 1 })
    );
    // A plain combo box: a click opens its list, Down moves, Enter picks.
    let (x, y) = widget_at(&v, "size", 0);
    v.press_at(x, y, 1, false, false);
    assert!(v.focus.as_ref().unwrap().open);
    assert!(v.form_key("down", false, false));
    assert!(v.form_key("down", false, false));
    v.form_enter(false);
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("size".to_string(), FieldValue::Choice { picked: vec![2] })
    );
    assert!(!v.focus.as_ref().unwrap().open);
    // Closed, Up picks the one before at once.
    assert!(v.form_key("up", false, false));
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("size".to_string(), FieldValue::Choice { picked: vec![1] })
    );
    // An editable combo box types, and a picked option fills its text.
    let city = field_index(&v, "city");
    v.focus_widget(city, 0);
    v.edit_text(crate::field_edit::Op::Insert("Oslo"));
    v.form_enter(false);
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("city".to_string(), text("Oslo"))
    );
    // A list box: a click on its second row picks Banana.
    let fruit = field_index(&v, "fruit");
    let widget = v.form.fields[fruit].widgets[0].clone();
    let rows = widget.list.unwrap();
    let pr = v.layout().pages[0];
    let k = v.zoom * PX_PER_PT;
    let row_y = widget.rect[1] + rows.inset + rows.row * 1.5;
    v.press_at(pr.x + 300. * k, pr.y + row_y * k, 1, false, false);
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &("fruit".to_string(), FieldValue::Choice { picked: vec![1] })
    );
    // A multi-pick list: Ctrl adds to the picks, Space at the cursor too.
    let tags = field_index(&v, "tags");
    v.focus_widget(tags, 0);
    v.form.fields[tags].picked = vec![0];
    v.pick(2, true);
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &(
            "tags".to_string(),
            FieldValue::Choice { picked: vec![0, 2] }
        )
    );
    v.form.fields[tags].picked = vec![0, 2];
    assert!(v.form_key("up", false, false), "moves the cursor only");
    assert_eq!(v.focus.as_ref().unwrap().cursor, 1);
    assert!(v.form_key("space", false, false));
    assert_eq!(
        sent_fills(&v).last().unwrap(),
        &(
            "tags".to_string(),
            FieldValue::Choice {
                picked: vec![0, 1, 2]
            }
        )
    );
}

/// Criterion 6: a required field's tooltip says so; a field with nothing
/// in it is empty, one filled is not.
#[test]
fn a_required_fields_tooltip_says_so_and_empty_is_known() {
    let mut v = forms_view();
    let (x, y) = widget_at(&v, "email", 0);
    v.hover(x, y);
    assert_eq!(v.tooltip().as_deref(), Some("Email address · Required"));
    let email = field_index(&v, "email");
    assert!(v.form.fields[email].is_empty());
    v.form.fields[email].value = vec!["a@b".into()];
    assert!(!v.form.fields[email].is_empty());
    let (x, y) = widget_at(&v, "name", 0);
    v.hover(x, y);
    assert_eq!(v.tooltip().as_deref(), Some("Your name"));
}

fn state_with(v: &PdfView, name: &str, val: &str, pages: Vec<usize>) -> FormState {
    let mut fields = v.form.fields.clone();
    let f = fields.iter_mut().find(|f| f.name == name).unwrap();
    f.value = vec![val.into()];
    FormState { fields, pages }
}

/// Criteria 7 and 8: a value accepted bumps its page, which is drawn and
/// read again with its old raster shown meanwhile; the tab is unsaved
/// until a save answers, and a value accepted after the save was sent keeps
/// it unsaved. Form work goes before any page.
#[test]
fn a_filled_page_is_drawn_again_and_the_tab_is_unsaved_until_saved() {
    let mut v = forms_view();
    v.frame(900., 900., 0., 1.);
    let scale = raster_scale(&v.meta.pages[0], v.zoom, v.scale_factor);
    for page in 0..2 {
        v.absorb(Done {
            op: Op::Pixels { page, scale },
            answer: Answer::Pixels(worker::Raster {
                w: 1,
                h: 1,
                bgra: vec![255; 4],
            }),
            lost: false,
        });
        v.absorb(Done {
            op: Op::Text { page },
            answer: Answer::Text(Arc::new(pdfcraft_render::PageText::default())),
            lost: false,
        });
    }
    assert!(!matches!(v.next_job(), Some(Op::Pixels { page: 0, .. })));
    let name = field_index(&v, "name");
    v.fill(name, text("Ada"));
    assert!(
        matches!(v.next_job(), Some(Op::SetField { .. })),
        "form first"
    );
    let op = v.form_ops.pop_front().unwrap();
    let st = state_with(&v, "name", "Ada", vec![0]);
    v.absorb(Done {
        op,
        answer: Answer::Form(st),
        lost: false,
    });
    assert!(v.unsaved());
    assert!(v.rasters[0].is_some(), "the old raster stays");
    assert!(matches!(v.next_job(), Some(Op::Pixels { page: 0, .. })));
    // Page 2 is not asked again.
    let scale1 = raster_scale(&v.meta.pages[0], v.zoom, v.scale_factor);
    v.absorb(Done {
        op: Op::Pixels {
            page: 0,
            scale: scale1,
        },
        answer: Answer::Pixels(worker::Raster {
            w: 1,
            h: 1,
            bgra: vec![255; 4],
        }),
        lost: false,
    });
    assert_eq!(v.next_job(), Some(Op::Text { page: 0 }));
    // A save, then a value accepted after it was sent.
    let path = v.path.clone();
    v.form_ops.push_back(Op::SaveTo { path: path.clone() });
    v.pending_save = Some(PendingSave {
        after: None,
        outcome: None,
    });
    v.fill(name, text("Bea"));
    let save = v.form_ops.pop_front().unwrap();
    v.absorb(Done {
        op: save,
        answer: Answer::Saved(path),
        lost: false,
    });
    assert!(!v.unsaved());
    assert!(
        v.pending_save
            .as_ref()
            .unwrap()
            .outcome
            .as_ref()
            .unwrap()
            .is_ok()
    );
    let op = v.form_ops.pop_front().unwrap();
    let st = state_with(&v, "name", "Bea", vec![0]);
    v.absorb(Done {
        op,
        answer: Answer::Form(st),
        lost: false,
    });
    assert!(v.unsaved(), "accepted after the save went");
}

/// Criterion 7: a page's text read again replaces its Find hits (a value
/// gone takes its hit with it) and drops a selection on it.
#[test]
fn a_page_read_again_replaces_its_hits_and_drops_its_selection() {
    let mut v = PdfView::new(
        Path::new("made-up.pdf"),
        DocMeta {
            pages: vec![PageMeta {
                w: 595.,
                h: 842.,
                rotation: 0,
                label: "1".into(),
            }],
            ..DocMeta::default()
        },
        None,
    );
    let glyph = |t: &str, x: f32| pdfcraft_render::TextGlyph {
        text: t.into(),
        rect: [x, 100., x + 6., 110.],
        quad: [[x, 110.], [x + 6., 110.], [x + 6., 100.], [x, 100.]],
        direction: [1., 0.],
    };
    let text_of = |s: &str| {
        let mut t = pdfcraft_render::PageText::default();
        for (i, c) in s.chars().enumerate() {
            t.glyphs.push(glyph(&c.to_string(), 72. + 6. * i as f32));
            t.line_of.push(0);
            t.space_before.push(false);
        }
        Arc::new(t)
    };
    v.absorb(Done {
        op: Op::Text { page: 0 },
        answer: Answer::Text(text_of("Ada")),
        lost: false,
    });
    v.find.field.text = "Ada".into();
    v.find_changed();
    assert_eq!(v.find.hits.len(), 1);
    v.select_all();
    v.absorb(Done {
        op: Op::Text { page: 0 },
        answer: Answer::Text(text_of("Bea")),
        lost: false,
    });
    assert!(v.find.hits.is_empty(), "the old hit is gone");
    assert!(v.sel.is_none());
}

/// Criterion 10: values a new reader could not take are gone: the form is
/// read again first, every page with a field drawn again, nothing unsaved,
/// and the host is told.
#[test]
fn lost_values_read_the_form_again_and_leave_nothing_unsaved() {
    let mut v = forms_view();
    v.form_rev = 3;
    v.focus = Some(FormFocus {
        field: 0,
        widget: 0,
        edit: None,
        open: false,
        cursor: 0,
    });
    v.absorb(Done {
        op: Op::Text { page: 1 },
        answer: Answer::Failed(worker::DIED.into()),
        lost: true,
    });
    assert!(!v.unsaved());
    assert!(v.values_lost && v.focus.is_none());
    assert_eq!(v.next_job(), Some(Op::Fields));
    assert_eq!(v.page_rev, vec![1, 1]);
}

/// Criteria 8 and 9: what Save does on a PDF tab: nothing when nothing was
/// filled (Save As writes the copy), the typed text committed first, a
/// signed PDF asked first (in other words when the file is written anew),
/// a tab not open says so.
#[test]
fn what_save_does_on_a_pdf_tab() {
    use super::host::{
        PDF_NO_CHANGES, PDF_NOT_OPEN, PdfSaveStep, SIGNED_LOST_QUESTION, SIGNED_QUESTION,
        pdf_save_step,
    };
    let dir = Scratch::new();
    let path = copy(&dir, "forms");
    let mut tab = host::pdf_view_tab_with(&path, None, Spawn::InProcess).unwrap();
    assert_eq!(
        pdf_save_step(&mut tab, None),
        PdfSaveStep::Nothing(PDF_NO_CHANGES)
    );
    let other = dir.path("as.pdf");
    assert_eq!(
        pdf_save_step(&mut tab, Some(other.clone())),
        PdfSaveStep::Go(other)
    );
    let crate::Surface::Pdf(v) = &mut tab.surface else {
        unreachable!()
    };
    let name = field_index(v, "name");
    v.focus_widget(name, 0);
    v.edit_text(crate::field_edit::Op::Insert("Ada"));
    assert_eq!(pdf_save_step(&mut tab, None), PdfSaveStep::Go(path.clone()));
    let crate::Surface::Pdf(v) = &tab.surface else {
        unreachable!()
    };
    assert_eq!(
        sent_fills(v),
        [("name".to_string(), text("Ada"))],
        "committed first"
    );
    let signed = copy(&dir, "signed");
    let mut tab = host::pdf_view_tab_with(&signed, None, Spawn::InProcess).unwrap();
    let crate::Surface::Pdf(v) = &mut tab.surface else {
        unreachable!()
    };
    v.fill(0, text("x"));
    assert_eq!(
        pdf_save_step(&mut tab, None),
        PdfSaveStep::Ask {
            path: signed.clone(),
            question: SIGNED_QUESTION
        }
    );
    let crate::Surface::Pdf(v) = &mut tab.surface else {
        unreachable!()
    };
    v.form.incremental = false;
    assert!(matches!(
        pdf_save_step(&mut tab, None),
        PdfSaveStep::Ask {
            question: SIGNED_LOST_QUESTION,
            ..
        }
    ));
    let mut pending = host::pending_pdf_tab(&path, "forms.pdf", host::PdfAt { page: 0, zoom: 1. });
    assert_eq!(
        pdf_save_step(&mut pending, None),
        PdfSaveStep::Nothing(PDF_NOT_OPEN)
    );
}

/// Criteria 8 and 9 through the tab: the signed PDF's question is the
/// catalogued box, Save and Cancel; a save that is over follows Save As and
/// clears the unsaved mark, one that failed keeps it and says why; a dirty
/// PDF tab is what a quit without asking stops for.
#[test]
fn a_pdf_tabs_save_is_finished_on_the_tab() {
    use crate::dialog::{ButtonRole, DialogOwner};
    let d = host::signed_dialog(host::SIGNED_QUESTION);
    assert_eq!(d.id, crate::dialog::catalog::PDF_SIGNED_SAVE);
    assert_eq!(d.owner, DialogOwner::PdfSignedSave);
    let roles: Vec<(&str, ButtonRole)> = d
        .buttons
        .iter()
        .map(|b| (b.label.as_str(), b.role))
        .collect();
    assert_eq!(
        roles,
        [("Save", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)]
    );

    let dir = Scratch::new();
    let path = copy(&dir, "forms");
    let mut tab = host::pdf_view_tab_with(&path, None, Spawn::InProcess).unwrap();
    tab.dirty = true;
    assert_eq!(host::unsaved_pdf(std::slice::from_ref(&tab)), Some(0));
    host::finish_pdf_save(&mut tab, Err("disk full".into()));
    assert!(tab.status.contains("disk full"));
    assert!(tab.dirty, "kept");
    let other = dir.path("other.pdf");
    host::finish_pdf_save(&mut tab, Ok(other.clone()));
    assert_eq!(tab.path.as_deref(), Some(other.as_path()));
    assert_eq!(tab.title.as_ref(), "other.pdf");
    assert!(!tab.dirty);
    assert_eq!(host::unsaved_pdf(std::slice::from_ref(&tab)), None);
}
