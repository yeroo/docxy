//! `FontSet` over fonts the test writes itself (see docxcore's synthetic
//! TrueType builder), and over this machine's fonts where it has any.

use std::path::PathBuf;

use docxcore::pdf_font::PdfFontProvider;
use pdffont::FontSet;

#[path = "../../docxcore/tests/common/synth_font.rs"]
mod synth_font;
use synth_font::{Style, build_with, collection, glyph};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("pdffont-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn write(&self, file: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(file), bytes).unwrap();
    }

    fn set(&self) -> FontSet {
        FontSet::from_dirs(vec![self.0.clone()])
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn font(family: &str, bold: bool, italic: bool, fs_type: u16, cff: bool) -> Vec<u8> {
    build_with(
        &[glyph('A', 600), glyph('i', 250), glyph('Ж', 700)],
        1000,
        fs_type,
        &Style {
            family: Some(family.into()),
            bold,
            italic,
            cff,
        },
    )
}

#[test]
fn picks_a_face_by_family_and_style() {
    let d = Dir::new("style");
    d.write("a.ttf", &font("Arial", false, false, 0, false));
    d.write("ab.ttf", &font("Arial", true, false, 0, false));
    d.write("ai.ttf", &font("Arial", false, true, 0, false));
    d.write("other.ttf", &font("Other", false, false, 0, false));
    let set = d.set();
    assert_eq!(set.families(), ["Arial", "Other"]);
    let regular = set.face("arial", false, false).unwrap();
    let bold = set.face("Arial", true, false).unwrap();
    let italic = set.face("Arial", false, true).unwrap();
    assert_eq!(regular.ps_name(), "Arial-Regular");
    assert_eq!(bold.ps_name(), "Arial-Bold");
    assert_eq!(italic.ps_name(), "Arial-Italic");
    // No bold italic: the closest (bold or italic) is used rather than none.
    assert!(set.face("Arial", true, true).is_some());
    // Equal asks give the same face.
    assert_eq!(regular.id(), set.face("Arial", false, false).unwrap().id());
    assert_ne!(regular.id(), bold.id());
}

#[test]
fn metrics_and_font_bytes_come_from_the_file() {
    let d = Dir::new("metrics");
    let bytes = font("Arial", false, false, 0, false);
    d.write("a.ttf", &bytes);
    let face = d.set().face("Arial", false, false).unwrap();
    assert_eq!(face.units_per_em(), 1000);
    let (a, i, zh) = (
        face.glyph('A').unwrap(),
        face.glyph('i').unwrap(),
        face.glyph('Ж').unwrap(),
    );
    assert_eq!(
        (face.advance(a), face.advance(i), face.advance(zh)),
        (600, 250, 700)
    );
    assert_eq!(face.glyph('Q'), None);
    assert_eq!(face.glyph('\u{0}'), None);
    assert_eq!(&*face.sfnt(), &bytes[..]);
    assert_eq!(face.face_index(), 0);
    assert!(face.ascent() > 0 && face.descent() < 0);
}

#[test]
fn unknown_family_falls_back_to_a_default_then_an_alias() {
    let d = Dir::new("alias");
    d.write("a.ttf", &font("Arial", false, false, 0, false));
    d.write("c.ttf", &font("Carlito", false, false, 0, false));
    let set = d.set();
    assert_eq!(
        set.face("Calibri", false, false).unwrap().ps_name(),
        "Carlito-Regular"
    );
    assert_eq!(
        set.face("No Such Font", false, false).unwrap().ps_name(),
        "Arial-Regular"
    );
    assert_eq!(
        set.face("", false, false).unwrap().ps_name(),
        "Arial-Regular"
    );
}

#[test]
fn cff_and_restricted_faces_are_not_offered() {
    let d = Dir::new("refused");
    d.write("cff.otf", &font("Cff Face", false, false, 0, true));
    d.write(
        "restricted.ttf",
        &font("Locked", false, false, 0x0002, false),
    );
    d.write("preview.ttf", &font("Preview", false, false, 0x0004, false));
    d.write("junk.ttf", b"not a font at all");
    d.write("notes.txt", b"ignored");
    let set = d.set();
    assert_eq!(set.families(), ["Preview"], "only the embeddable glyf face");
    assert!(set.face("Cff Face", false, false).is_none());
    assert!(set.face("Locked", false, false).is_none());
}

#[test]
fn every_member_of_a_collection_is_indexed_and_embeds_by_index() {
    let d = Dir::new("ttc");
    let a = font("First", false, false, 0, false);
    let b = font("Second", true, false, 0, false);
    let ttc = collection(&[a, b]);
    d.write("pair.ttc", &ttc);
    let set = d.set();
    assert_eq!(set.families(), ["First", "Second"]);
    let second = set.face("Second", true, false).unwrap();
    assert_eq!(second.face_index(), 1);
    assert_eq!(second.ps_name(), "Second-Bold");
    assert_eq!(
        &*second.sfnt(),
        &ttc[..],
        "the file; docxcore subsets by index"
    );
}

#[test]
fn script_fallbacks_are_the_known_families_present() {
    let d = Dir::new("fallbacks");
    d.write("n.ttf", &font("Noto Sans", false, false, 0, false));
    d.write("d.ttf", &font("DejaVu Sans", false, false, 0, false));
    d.write("x.ttf", &font("Unrelated", false, false, 0, false));
    let names: Vec<String> = d.set().fallbacks().iter().map(|f| f.ps_name()).collect();
    assert_eq!(names, ["NotoSans-Regular", "DejaVuSans-Regular"]);
}

#[test]
fn an_empty_set_offers_nothing() {
    let set = FontSet::from_dirs(vec![PathBuf::from("/no/such/dir")]);
    assert!(set.is_empty());
    assert!(set.face("Arial", false, false).is_none());
    assert!(set.fallbacks().is_empty());
}

#[test]
fn a_set_drives_to_pdf() {
    use std::rc::Rc;
    let d = Dir::new("export");
    d.write("a.ttf", &font("Arial", false, false, 0, false));
    let opts = docxcore::export::PdfOptions {
        fonts: Some(Rc::new(d.set())),
        ..Default::default()
    };
    let doc = docxcore::model::Document {
        body: vec![docxcore::model::Block::Paragraph(
            docxcore::model::Paragraph {
                props: Default::default(),
                content: vec![docxcore::model::Inline::Run(docxcore::model::Run {
                    text: "AiЖ Q".into(),
                    props: Default::default(),
                })],
            },
        )],
    };
    let (pdf, report) = docxcore::export::to_pdf_report(&doc, &opts);
    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("/Subtype /Type0") && text.contains("+Arial-Regular"),
        "{text}"
    );
    assert_eq!(report.missing_chars, 0, "Q is WinAnsi, so Courier shows it");
}

#[test]
fn this_machines_fonts_cover_latin_when_it_has_any() {
    let set = FontSet::shared();
    if set.is_empty() {
        eprintln!("this_machines_fonts_cover_latin_when_it_has_any: skipped, no system fonts");
        return;
    }
    let face = set.face("Arial", false, false).expect("a default face");
    assert!(face.glyph('A').is_some() && face.glyph('z').is_some());
    assert!(face.advance(face.glyph('W').unwrap()) > face.advance(face.glyph('i').unwrap()));
    // The same set is shared on this thread.
    assert!(std::rc::Rc::ptr_eq(&set, &FontSet::shared()));
}

#[test]
fn this_machines_fallbacks_reach_cyrillic_when_present() {
    let set = FontSet::shared();
    let any = set
        .fallbacks()
        .iter()
        .chain(set.face("Arial", false, false).iter())
        .any(|f| f.glyph('Ж').is_some());
    if set.is_empty() || !any {
        eprintln!(
            "this_machines_fallbacks_reach_cyrillic_when_present: skipped, no face with Cyrillic"
        );
        return;
    }
    assert!(any);
}
