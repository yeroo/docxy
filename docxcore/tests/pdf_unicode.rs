//! PDF export with embedded fonts (#1508): a synthetic TrueType built in the
//! test stands in for a system font, so every script, the subset, the
//! ToUnicode map and the bidi order are checked without a font file.

use std::collections::BTreeMap;
use std::rc::Rc;

use docxcore::export::{PdfOptions, to_pdf, to_pdf_report};
use docxcore::package::load_package;
use docxcore::pdf_font::{PdfFace, PdfFontProvider};
use docxcore::render::{
    BidiBaseDirection, BidiInputGlyph, BidiProjector, BidiVisualCluster, BidiVisualLine,
};
use docxcore::styles::StyleSheet;

#[path = "common/docx_build.rs"]
mod docx_build;
#[path = "common/pdf_read.rs"]
mod pdf_read;
#[path = "common/synth_font.rs"]
mod synth_font;

use docx_build::{docx, docx_with_header};
use pdf_read::{body_text, decoded, shown, text_of, validate};
use synth_font::{SynthGlyph, build, glyph};

/// A face over a synthetic font: `chars` map to gids 1.. in order.
#[derive(Debug)]
struct TestFace {
    id: u64,
    data: Rc<[u8]>,
    map: BTreeMap<char, u16>,
    advances: Vec<u16>,
}

impl TestFace {
    /// A face whose `alias` chars share the glyph of the char they map to
    /// (a font that gives U+00A0 the glyph of U+0020), and whose font bytes
    /// are `data` when given.
    fn custom(
        id: u64,
        glyphs: &[(char, u16)],
        alias: &[(char, char)],
        data: Option<Vec<u8>>,
    ) -> Rc<TestFace> {
        let mut face = Rc::try_unwrap(TestFace::new(id, glyphs)).unwrap();
        for &(extra, of) in alias {
            let gid = face.map[&of];
            face.map.insert(extra, gid);
        }
        if let Some(data) = data {
            face.data = data.into();
        }
        Rc::new(face)
    }

    fn new(id: u64, glyphs: &[(char, u16)]) -> Rc<TestFace> {
        let synth: Vec<SynthGlyph> = glyphs.iter().map(|&(c, a)| glyph(c, a)).collect();
        Rc::new(TestFace {
            id,
            data: build(&synth, 1000, 0).into(),
            map: glyphs
                .iter()
                .enumerate()
                .map(|(i, (c, _))| (*c, i as u16 + 1))
                .collect(),
            advances: std::iter::once(500)
                .chain(glyphs.iter().map(|g| g.1))
                .collect(),
        })
    }
}

impl PdfFace for TestFace {
    fn id(&self) -> u64 {
        self.id
    }
    fn ps_name(&self) -> String {
        format!("Synth {}", self.id)
    }
    fn units_per_em(&self) -> u16 {
        1000
    }
    fn glyph(&self, ch: char) -> Option<u16> {
        self.map.get(&ch).copied()
    }
    fn advance(&self, gid: u16) -> u16 {
        self.advances[gid as usize]
    }
    fn ascent(&self) -> i16 {
        800
    }
    fn descent(&self) -> i16 {
        -200
    }
    fn sfnt(&self) -> Rc<[u8]> {
        self.data.clone()
    }
}

#[derive(Debug)]
struct Provider {
    main: Option<Rc<dyn PdfFace>>,
    fallbacks: Vec<Rc<dyn PdfFace>>,
}

impl PdfFontProvider for Provider {
    fn face(&self, _family: &str, _bold: bool, _italic: bool) -> Option<Rc<dyn PdfFace>> {
        self.main.clone()
    }
    fn fallbacks(&self) -> &[Rc<dyn PdfFace>] {
        &self.fallbacks
    }
}

const LATIN: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Every script the issue names (spaces 250, `i` 250, `W` 900).
fn world_glyphs() -> Vec<(char, u16)> {
    let mut g = vec![(' ', 250), ('i', 250), ('W', 900)];
    for c in LATIN.chars().filter(|c| *c != 'i' && *c != 'W') {
        g.push((c, 600));
    }
    for c in "0123456789".chars() {
        g.push((c, 550));
    }
    for c in "ЖДПривет".chars() {
        g.push((c, 650));
    }
    for c in "ΩΣαβγ".chars() {
        g.push((c, 620));
    }
    for c in "中文字".chars() {
        g.push((c, 1000));
    }
    for c in "عربيةسلام".chars() {
        g.push((c, 450));
    }
    for c in "שלוםעבר".chars() {
        g.push((c, 500));
    }
    g
}

fn provider(glyphs: &[(char, u16)]) -> Rc<Provider> {
    Rc::new(Provider {
        main: Some(TestFace::new(1, glyphs)),
        fallbacks: Vec::new(),
    })
}

fn opts_for(
    bytes: &[u8],
    fonts: Option<Rc<dyn PdfFontProvider>>,
) -> (PdfOptions, docxcore::package::Package) {
    let pkg = load_package(bytes).expect("load");
    let mut opts = PdfOptions::from_package(&pkg, Rc::new(StyleSheet::default()));
    opts.fonts = fonts;
    (opts, pkg)
}

fn export(body: &str, fonts: Option<Rc<dyn PdfFontProvider>>) -> Vec<u8> {
    let bytes = docx(body);
    let (opts, pkg) = opts_for(&bytes, fonts);
    to_pdf(&pkg.document, &opts)
}

fn para(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

const WORLD: &str = "Hello Привет Ωα 中文 عربية שלום 123";

fn strip(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn every_script_is_written_as_identity_h_and_extracts_in_reading_order() {
    // Left-to-right scripts, so reading order is the logical order.
    let text = "Hello Привет Ωα 中文 world 123";
    let pdf = export(&para(text), Some(provider(&world_glyphs())));
    let s = String::from_utf8_lossy(&pdf);
    for needle in [
        "/Subtype /Type0",
        "/Encoding /Identity-H",
        "/CIDFontType2",
        "/CIDToGIDMap /Identity",
        "/FontFile2",
        "/ToUnicode",
        "/FlateDecode",
    ] {
        assert!(s.contains(needle), "{needle} missing");
    }
    validate(&pdf);
    assert_eq!(body_text(&pdf), strip(text));
}

#[test]
fn arabic_and_hebrew_are_written_and_extract_in_logical_order() {
    let text = "عربية שלום 123";
    let pdf = export(&para(text), Some(provider(&world_glyphs())));
    assert_eq!(body_text(&pdf), strip(text));
    assert!(
        !String::from_utf8_lossy(&pdf).contains("(?"),
        "no '?' substitutes"
    );
}

#[test]
fn widths_are_the_faces_advances_in_w_and_in_layout() {
    let pdf = export(&para("iW"), Some(provider(&world_glyphs())));
    let objs = validate(&pdf);
    let w = objs
        .values()
        .find(|o| o.dict.contains("/CIDFontType2"))
        .unwrap();
    // gid 2 = 'i' (250), gid 3 = 'W' (900)
    assert!(w.dict.contains("250 900"), "{}", w.dict);
    let runs = &shown(&pdf)[0];
    assert!(runs.iter().any(|r| r.text.contains("iW")), "{runs:?}");

    // A narrow letter fits more of a line than a wide one.
    let lines = |ch: char| {
        let text: String = std::iter::repeat_n(ch, 170).collect();
        let pdf = export(&para(&text), Some(provider(&world_glyphs())));
        let mut ys: Vec<String> = shown(&pdf)[0]
            .iter()
            .filter(|s| s.font.starts_with('C'))
            .map(|s| format!("{:.1}", s.y))
            .collect();
        ys.dedup();
        ys.len()
    };
    assert!(lines('i') < lines('W'), "{} vs {}", lines('i'), lines('W'));
}

#[test]
fn the_subset_keeps_only_the_glyphs_used() {
    let many: Vec<(char, u16)> = (0..200u32)
        .filter_map(|i| char::from_u32(0x4E00 + i))
        .map(|c| (c, 1000))
        .chain([(' ', 250)])
        .collect();
    let whole = build(
        &many.iter().map(|&(c, a)| glyph(c, a)).collect::<Vec<_>>(),
        1000,
        0,
    );
    let pdf = export(&para("\u{4E00}\u{4E03}"), Some(provider(&many)));
    let objs = validate(&pdf);
    let file = objs.values().find(|o| o.dict.contains("/Length1")).unwrap();
    let font = decoded(file);
    assert!(
        font.len() * 2 < whole.len(),
        "{} vs {}",
        font.len(),
        whole.len()
    );
    let type0 = objs
        .values()
        .find(|o| o.dict.contains("/Subtype /Type0"))
        .unwrap();
    let tag: String = type0
        .dict
        .split("/BaseFont /")
        .nth(1)
        .unwrap()
        .chars()
        .take(7)
        .collect();
    assert!(
        tag.ends_with('+') && tag[..6].chars().all(|c| c.is_ascii_uppercase()),
        "{tag}"
    );
}

#[test]
fn a_line_with_hebrew_is_drawn_in_visual_order_and_extracts_logically() {
    #[derive(Debug)]
    struct Reverse;
    impl BidiProjector for Reverse {
        // Right-to-left base: the groups of Hebrew and of everything else swap
        // places, and the Hebrew reads backwards.
        fn project_line(&self, _: BidiBaseDirection, glyphs: &[BidiInputGlyph]) -> BidiVisualLine {
            let heb = |c: char| ('\u{5d0}'..='\u{5ea}').contains(&c);
            let mut groups: Vec<Vec<usize>> = Vec::new();
            for (i, g) in glyphs.iter().enumerate() {
                match groups.last_mut() {
                    Some(last) if heb(glyphs[last[0]].ch) == heb(g.ch) => last.push(i),
                    _ => groups.push(vec![i]),
                }
            }
            let mut order = Vec::new();
            for g in groups.into_iter().rev() {
                if heb(glyphs[g[0]].ch) {
                    order.extend(g.into_iter().rev());
                } else {
                    order.extend(g);
                }
            }
            BidiVisualLine {
                clusters: order
                    .into_iter()
                    .enumerate()
                    .map(|(k, i)| BidiVisualCluster {
                        glyphs: vec![i],
                        logical: None,
                        level: 0,
                        cells: k..k + 1,
                    })
                    .collect(),
                width: glyphs.len(),
            }
        }
    }
    let body =
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t xml:space="preserve">שלום 123</w:t></w:r></w:p>"#;
    let bytes = docx(body);
    let (mut opts, pkg) = opts_for(&bytes, Some(provider(&world_glyphs())));
    opts.bidi = Some(Rc::new(Reverse));
    let pdf = to_pdf(&pkg.document, &opts);
    let body_runs: Vec<_> = shown(&pdf)[0]
        .iter()
        .filter(|s| s.font.starts_with('C') && s.y > 60.0 && s.y < 735.0)
        .cloned()
        .collect();
    let heb = body_runs.iter().find(|s| s.text.contains('ש')).unwrap();
    let num = body_runs.iter().find(|s| s.text.contains('1')).unwrap();
    assert!(
        heb.tj,
        "right-to-left stretch is positioned glyph by glyph: {heb:?}"
    );
    assert_eq!(heb.text.trim(), "שלום", "stored in logical order");
    assert!(
        num.x < heb.x,
        "the digits sit left of the Hebrew: {num:?} {heb:?}"
    );
    assert!(heb.x > 300.0, "an RTL paragraph sits at the right: {heb:?}");
    assert_eq!(body_text(&pdf), "שלום123");
}

#[test]
fn output_is_deterministic() {
    let a = export(&para(WORLD), Some(provider(&world_glyphs())));
    let b = export(&para(WORLD), Some(provider(&world_glyphs())));
    assert_eq!(a, b);
}

#[test]
fn without_a_provider_nothing_changes_and_nothing_is_missing() {
    let bytes = docx(&para("plain text"));
    let (opts, pkg) = opts_for(&bytes, None);
    let (pdf, report) = to_pdf_report(&pkg.document, &opts);
    assert_eq!(pdf, to_pdf(&pkg.document, &opts));
    assert_eq!(report.missing_chars, 0);
    assert!(!String::from_utf8_lossy(&pdf).contains("/Type0"));
}

#[test]
fn a_provider_with_no_faces_prints_latin_like_the_legacy_file() {
    let empty = Rc::new(Provider {
        main: None,
        fallbacks: Vec::new(),
    });
    let text =
        "Plain café text that wraps over more than one line, because it is rather long indeed.";
    let with = export(&para(text), Some(empty));
    let legacy = export(&para(text), None);
    assert_eq!(with, legacy, "no face used: the Courier file");
}

#[test]
fn characters_no_face_covers_are_counted_and_win_ansi_ones_fall_back_to_courier() {
    // The face has no 'é' and no Cyrillic.
    let latin: Vec<(char, u16)> = vec![(' ', 250), ('a', 600), ('b', 600)];
    let bytes = docx(&para("ab é Ж\u{1D11E}"));
    let (opts, pkg) = opts_for(&bytes, Some(provider(&latin)));
    let (pdf, report) = to_pdf_report(&pkg.document, &opts);
    assert_eq!(report.missing_chars, 2, "Ж and the clef; é is WinAnsi");
    validate(&pdf);
    let objs = validate(&pdf);
    assert!(
        objs.values()
            .filter(|o| o.stream.is_some())
            .any(|o| decoded(o).windows(3).any(|w| w == b"(\xe9)")),
        "e-acute is shown by Courier in WinAnsi"
    );
}

#[test]
fn a_fallback_face_shows_what_the_own_face_lacks() {
    let own = TestFace::new(1, &[(' ', 250), ('a', 600)]);
    let cyr = TestFace::new(2, &[('Ж', 700), ('Д', 700)]);
    let p = Rc::new(Provider {
        main: Some(own),
        fallbacks: vec![cyr],
    });
    let pdf = export(&para("aЖДa"), Some(p));
    let objs = validate(&pdf);
    let fonts = objs
        .values()
        .filter(|o| o.dict.contains("/Subtype /Type0"))
        .count();
    assert_eq!(fonts, 2);
    assert_eq!(body_text(&pdf), "aЖДa");
}

#[test]
fn header_and_footer_fields_print_through_the_same_fonts() {
    let pdf = export(&para("body"), Some(provider(&world_glyphs())));
    let text = text_of(&pdf);
    assert!(text.contains("Runninghead"), "{text}");
    assert!(text.contains("Page1"), "{text}");
}

#[test]
fn content_streams_are_compressed_and_the_file_is_well_formed() {
    let many: String = (0..150)
        .map(|i| para(&format!("Paragraph {i} {WORLD}")))
        .collect();
    let pdf = export(&many, Some(provider(&world_glyphs())));
    let objs = validate(&pdf);
    let flate = objs
        .values()
        .filter(|o| o.stream.is_some() && o.dict.contains("/FlateDecode"))
        .count();
    let streams = objs.values().filter(|o| o.stream.is_some()).count();
    assert_eq!(flate, streams, "every stream is compressed");
    assert!(shown(&pdf).len() >= 2, "several pages");
}

#[test]
fn qpdf_accepts_the_file_when_installed() {
    let Ok(qpdf) = std::process::Command::new("qpdf").arg("--version").output() else {
        eprintln!("qpdf_accepts_the_file_when_installed: skipped, qpdf is not installed");
        return;
    };
    assert!(qpdf.status.success());
    let pdf = export(&para(WORLD), Some(provider(&world_glyphs())));
    let path = std::env::temp_dir().join(format!("docxy-1508-{}.pdf", std::process::id()));
    std::fs::write(&path, &pdf).unwrap();
    let out = std::process::Command::new("qpdf")
        .arg("--check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn right_to_left_rows_are_in_the_provided_order_with_offsets_from_the_advances() {
    // Letters of different widths, so each offset must pair the right two:
    // ש 700, ל 400, ו 250, ם 600 units, at the body's 11 pt.
    let bidi = {
        #[derive(Debug)]
        struct Flip;
        impl BidiProjector for Flip {
            fn project_line(
                &self,
                _: BidiBaseDirection,
                glyphs: &[BidiInputGlyph],
            ) -> BidiVisualLine {
                BidiVisualLine {
                    clusters: (0..glyphs.len())
                        .rev()
                        .enumerate()
                        .map(|(k, i)| BidiVisualCluster {
                            glyphs: vec![i],
                            logical: None,
                            level: 1,
                            cells: k..k + 1,
                        })
                        .collect(),
                    width: glyphs.len(),
                }
            }
        }
        Rc::new(Flip)
    };
    let bytes = docx(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t xml:space="preserve">שלום </w:t></w:r></w:p>"#,
    );
    let glyphs = [(' ', 250), ('ש', 700), ('ל', 400), ('ו', 250), ('ם', 600)];
    let (mut opts, pkg) = opts_for(&bytes, Some(provider(&glyphs)));
    opts.bidi = Some(bidi);
    let pdf = to_pdf(&pkg.document, &opts);
    let run = shown(&pdf)[0]
        .iter()
        .find(|s| s.text.contains('ש'))
        .cloned()
        .unwrap();
    // Logical שלום + the trailing space is trimmed by wrapping: four letters.
    assert_eq!(run.text, "שלום");
    // From one glyph's end to the next one's start, to the left: the two
    // advances summed, in thousandths of the size (units, here).
    assert_eq!(run.adjusts, [1100.0, 650.0, 850.0], "{run:?}");
    assert_eq!(run.size, 11.0);
}

#[test]
fn a_header_watermark_prints_through_the_embedded_fonts() {
    let mark = r#"<w:p><w:r><w:pict><v:shape id="PowerPlusWaterMarkObject"><v:textpath string="Draft Ж"/></v:shape></w:pict></w:r></w:p>"#;
    let bytes = docx_with_header(&para("body"), mark);
    let (opts, pkg) = opts_for(&bytes, Some(provider(&world_glyphs())));
    let (pdf, report) = to_pdf_report(&pkg.document, &opts);
    assert_eq!(report.missing_chars, 0);
    let objs = validate(&pdf);
    let content = objs
        .values()
        .filter(|o| o.stream.is_some() && o.dict.contains("/FlateDecode"))
        .map(|o| String::from_utf8_lossy(&decoded(o)).into_owned())
        .find(|c| c.contains("cm"))
        .expect("a page with a rotated watermark");
    assert_eq!(
        content.matches("\nq\n").count() + content.starts_with("q\n") as usize,
        content.matches("\nQ\n").count(),
        "q/Q balanced: {content}"
    );
    assert!(shown(&pdf)[0].iter().any(|s| s.text == "Draft Ж"));
}

#[test]
fn a_glyph_shared_by_space_and_nbsp_extracts_as_a_space() {
    // A no-break space comes first; the ordinary spaces share its glyph.
    let face = TestFace::custom(
        1,
        &[(' ', 250), ('a', 600), ('b', 600)],
        &[('\u{a0}', ' ')],
        None,
    );
    let p = Rc::new(Provider {
        main: Some(face),
        fallbacks: Vec::new(),
    });
    let pdf = export(&para("a\u{a0}b a b"), Some(p));
    let objs = validate(&pdf);
    let cmap = objs
        .values()
        .filter(|o| o.stream.is_some() && o.dict.contains("/FlateDecode"))
        .map(|o| String::from_utf8_lossy(&decoded(o)).into_owned())
        .find(|c| c.contains("begincmap"))
        .unwrap();
    assert!(cmap.contains("<0001> <0020>"), "{cmap}");
    assert!(!cmap.contains("00A0"), "{cmap}");
}

#[test]
fn a_face_whose_font_file_is_unusable_counts_as_absent() {
    let face = TestFace::custom(
        1,
        &[(' ', 250), ('a', 600), ('Ж', 700)],
        &[],
        Some(b"not a font".to_vec()),
    );
    let p = Rc::new(Provider {
        main: Some(face),
        fallbacks: Vec::new(),
    });
    let bytes = docx(&para("aa Ж"));
    let (opts, pkg) = opts_for(&bytes, Some(p));
    let (pdf, report) = to_pdf_report(&pkg.document, &opts);
    // Latin falls to Courier, Cyrillic is a counted .notdef; no embedded font.
    assert_eq!(report.missing_chars, 1);
    let s = String::from_utf8_lossy(&pdf);
    assert!(!s.contains("/Subtype /Type0") && !s.contains("/FontFile2"));
    assert!(!s.contains("/C0"), "no embedded face is referenced");
    validate(&pdf);
    assert_eq!(body_text(&pdf), "aa?");
}

/// Counts the lines the projector is asked about.
#[derive(Debug, Default)]
struct Counting(std::cell::Cell<usize>);

impl BidiProjector for Counting {
    fn project_line(&self, _: BidiBaseDirection, glyphs: &[BidiInputGlyph]) -> BidiVisualLine {
        self.0.set(self.0.get() + 1);
        BidiVisualLine {
            clusters: (0..glyphs.len())
                .map(|i| BidiVisualCluster {
                    glyphs: vec![i],
                    logical: None,
                    level: 0,
                    cells: i..i + 1,
                })
                .collect(),
            width: glyphs.len(),
        }
    }
}

#[test]
fn a_latin_line_in_a_right_to_left_paragraph_is_still_projected() {
    let calls = |body: &str| {
        let bytes = docx(body);
        let (mut opts, pkg) = opts_for(&bytes, Some(provider(&world_glyphs())));
        let counting = Rc::new(Counting::default());
        opts.bidi = Some(counting.clone());
        to_pdf(&pkg.document, &opts);
        counting.0.get()
    };
    let rtl = r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t>Word 2016.</w:t></w:r></w:p>"#;
    assert!(calls(rtl) > 0, "an RTL base moves the edge neutrals");
    assert_eq!(
        calls(&para("Word 2016.")),
        0,
        "plain Latin needs no projector"
    );
}
