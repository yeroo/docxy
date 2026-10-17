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
    styled(
        fs_type,
        Style {
            family: Some(family.into()),
            bold,
            italic,
            cff,
            ..Style::default()
        },
    )
}

fn styled(fs_type: u16, style: Style) -> Vec<u8> {
    build_with(
        &[glyph('A', 600), glyph('i', 250), glyph('Ж', 700)],
        1000,
        fs_type,
        &style,
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
fn faces_named_on_the_mac_platform_only_are_indexed() {
    let d = Dir::new("macnames");
    d.write(
        "helv.ttf",
        &styled(
            0,
            Style {
                family: Some("Helvetica".into()),
                mac_names: true,
                ..Style::default()
            },
        ),
    );
    let set = d.set();
    assert_eq!(set.families(), ["Helvetica"]);
    let face = set.face("Helvetica", false, false).unwrap();
    assert_eq!(face.ps_name(), "Helvetica-Regular", "PostScript name too");
}

#[test]
fn a_plain_request_takes_the_weight_nearest_regular_not_the_first_file() {
    let d = Dir::new("weights");
    let weight = |name: &str, w: u16, bold: bool| {
        styled(
            0,
            Style {
                family: Some(format!("Avenir Next {name}").trim().into()),
                typographic: Some("Avenir Next".into()),
                weight: w,
                bold,
                ..Style::default()
            },
        )
    };
    // The Demi Bold file sorts first, as Avenir Next.ttc's face 2 does.
    d.write("a-demi.ttf", &weight("Demi Bold", 600, false));
    d.write("b-regular.ttf", &weight("", 400, false));
    d.write("c-black.ttf", &weight("Heavy", 900, false));
    d.write("d-bold.ttf", &weight("Bold", 700, true));
    let set = d.set();
    let name = |bold| set.face("Avenir Next", bold, false).unwrap().ps_name();
    assert_eq!(name(false), "AvenirNext-Regular");
    assert_eq!(name(true), "AvenirNextBold-Bold");
    // A document that names the legacy family finds that face.
    assert_eq!(
        set.face("Avenir Next Demi Bold", false, false)
            .unwrap()
            .ps_name(),
        "AvenirNextDemiBold-Regular"
    );
    assert!(set.families().contains(&"Avenir Next Heavy".to_string()));
}

/// A face of family "Fam" (legacy name `legacy`) with the given slant bits.
fn variant(legacy: &str, typographic: bool, style: Style) -> Vec<u8> {
    styled(
        0,
        Style {
            family: Some(legacy.into()),
            typographic: typographic.then(|| "Fam".to_string()),
            ..style
        },
    )
}

#[test]
fn italic_comes_from_fs_selection_when_mac_style_is_clear() {
    let d = Dir::new("italic");
    // Apple's italics: macStyle 0, fsSelection italic; it sorts before the regular.
    d.write(
        "a-italic.ttf",
        &variant(
            "Fam Italic",
            true,
            Style {
                fs_selection: 0x0001,
                ..Style::default()
            },
        ),
    );
    d.write(
        "b-oblique.ttf",
        &variant(
            "Fam Oblique",
            true,
            Style {
                fs_selection: 0x0200,
                weight: 700,
                bold: true,
                ..Style::default()
            },
        ),
    );
    d.write(
        "c-regular.ttf",
        &variant("Fam Regular", true, Style::default()),
    );
    let set = d.set();
    assert_eq!(
        set.face("Fam", false, false).unwrap().ps_name(),
        "FamRegular-Regular"
    );
    assert_eq!(
        set.face("Fam", false, true).unwrap().ps_name(),
        "FamItalic-Regular"
    );
    assert_eq!(
        set.face("Fam", true, true).unwrap().ps_name(),
        "FamOblique-Bold",
        "bold italic: the oblique at 700"
    );
}

#[test]
fn a_same_weight_width_variant_does_not_win_a_plain_request() {
    let d = Dir::new("width");
    d.write(
        "a-condensed.ttf",
        &variant(
            "Fam Condensed",
            true,
            Style {
                width_class: 3,
                ..Style::default()
            },
        ),
    );
    d.write(
        "b-regular.ttf",
        &variant("Fam Regular", true, Style::default()),
    );
    assert_eq!(
        d.set().face("Fam", false, false).unwrap().ps_name(),
        "FamRegular-Regular"
    );
}

#[test]
fn a_face_whose_own_name_is_the_request_beats_a_sibling_in_the_typographic_family() {
    let d = Dir::new("ornaments");
    // Hoefler Text Ornaments shares the typographic family and sorts first.
    d.write(
        "a-ornaments.ttf",
        &variant("Fam Ornaments", true, Style::default()),
    );
    d.write("b-text.ttf", &variant("Fam", true, Style::default()));
    assert_eq!(
        d.set().face("Fam", false, false).unwrap().ps_name(),
        "Fam-Regular"
    );
}

#[test]
fn an_english_mac_name_is_found_beside_localized_windows_names() {
    // Geeza Pro's shape: Windows records only in other languages, the English
    // family on the Macintosh platform.
    let d = Dir::new("geeza");
    d.write(
        "geeza.ttf",
        &styled(
            0,
            Style {
                family: Some("\u{67}\u{64}\u{627}".into()),
                mac_family: Some("Geeza Pro".into()),
                ..Style::default()
            },
        ),
    );
    let set = d.set();
    assert!(set.face("Geeza Pro", false, false).is_some());
    assert!(set.families().contains(&"Geeza Pro".to_string()));
}

#[test]
fn italic_also_comes_from_the_post_angle_and_the_style_name() {
    let d = Dir::new("italicangle");
    let regular = variant("Fam Regular", true, Style::default());
    // Neither macStyle nor fsSelection, as Apple's Ultra Light Italic.
    let angle = variant(
        "Fam Light Italic",
        true,
        Style {
            italic_angle: -10,
            ..Style::default()
        },
    );
    let named = variant(
        "Fam Heavy Italic",
        true,
        Style {
            subfamily: Some("Heavy Italic".into()),
            weight: 900,
            ..Style::default()
        },
    );
    d.write("a-angle.ttf", &angle);
    d.write("b-named.ttf", &named);
    d.write("c-regular.ttf", &regular);
    let set = d.set();
    assert_eq!(
        set.face("Fam", false, false).unwrap().ps_name(),
        "FamRegular-Regular"
    );
    // Both are italic; the weight nearest 400 wins between them.
    assert_eq!(
        set.face("Fam", false, true).unwrap().ps_name(),
        "FamLightItalic-Regular"
    );
    assert_eq!(
        set.face("Fam", true, true).unwrap().ps_name(),
        "FamHeavyItalic-HeavyItalic"
    );
}

#[test]
fn embedding_bits_decide_which_faces_are_offered() {
    let d = Dir::new("fstype");
    for (file, family, fs) in [
        ("installable.ttf", "Installable", 0x0000),
        ("restricted.ttf", "Restricted", 0x0002),
        ("preview.ttf", "Preview", 0x0004),
        ("editable.ttf", "Editable", 0x0008),
        // Old OS/2 tables set 0x0002 beside a lifting bit.
        ("lifted.ttf", "Lifted", 0x000A),
        ("nosubset.ttf", "NoSubset", 0x0100),
        ("bitmaponly.ttf", "BitmapOnly", 0x0200),
    ] {
        d.write(file, &font(family, false, false, fs, false));
    }
    assert_eq!(
        d.set().families(),
        ["Editable", "Installable", "Lifted", "Preview"]
    );
}

#[test]
fn a_face_the_subsetter_cannot_read_is_not_offered() {
    let d = Dir::new("badloca");
    let mut bytes = font("Broken", false, false, 0, false);
    // loca entries past the end of glyf.
    let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    for t in 0..tables {
        let at = 12 + 16 * t;
        if &bytes[at..at + 4] == b"loca" {
            let off = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
            bytes[off + 8..off + 12].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
        }
    }
    d.write("broken.ttf", &bytes);
    d.write("fine.ttf", &font("Fine", false, false, 0, false));
    assert_eq!(d.set().families(), ["Fine"]);
}

#[test]
fn a_short_loca_face_is_offered() {
    let d = Dir::new("shortloca");
    d.write(
        "short.ttf",
        &styled(
            0,
            Style {
                family: Some("Short".into()),
                short_loca: true,
                ..Style::default()
            },
        ),
    );
    assert_eq!(d.set().families(), ["Short"]);
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
fn this_machines_fallbacks_reach_cyrillic_when_it_has_such_a_face() {
    let set = FontSet::shared();
    // Skip only when no face on the machine covers Cyrillic at all.
    let has_any = set.families().iter().any(|f| {
        [false, true].iter().any(|&b| {
            set.face(f, b, false)
                .is_some_and(|face| face.glyph('Ж').is_some())
        })
    });
    if !has_any {
        eprintln!(
            "this_machines_fallbacks_reach_cyrillic_when_it_has_such_a_face: skipped, no face with Cyrillic"
        );
        return;
    }
    let reach = set
        .fallbacks()
        .iter()
        .chain(set.face("Arial", false, false).iter())
        .any(|f| f.glyph('Ж').is_some());
    assert!(
        reach,
        "a Cyrillic face exists but neither the default nor a fallback has it"
    );
}
