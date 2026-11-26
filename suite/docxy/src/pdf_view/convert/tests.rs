//! Convert to editable document with the layout (#2257), on the corpus:
//! `convert.pdf` (made for it: running header and footer, a picture between
//! paragraphs, a ruled table, a heading over two columns, base-14 fonts, a
//! red word) and the cairo files `columns.pdf` and `scanned.pdf`.

use super::*;
use crate::pdf_view::tests::{ENCRYPTED, UNREADABLE, corpus_path, user_password};
use docxcore::model::{Document, PageGeom};
use docxcore::package::Package;
use std::time::Instant;

fn corpus(name: &str) -> Vec<u8> {
    std::fs::read(corpus_path(name)).unwrap()
}

fn converted(name: &str) -> ConvertedDoc {
    layout(&corpus(name)).unwrap().expect("a document")
}

/// The document as the tab saves it, and that loaded again.
fn saved(c: &ConvertedDoc) -> (Vec<u8>, Package) {
    let bytes = c.docx();
    let pkg = docxcore::package::load_package(&bytes).unwrap();
    (bytes, pkg)
}

/// Every XML part of a saved package, by name.
fn xml_parts(docx: &[u8]) -> Vec<(String, String)> {
    let zip = opccore::zip::ZipArchive::open(docx).unwrap();
    zip.entries()
        .iter()
        .filter(|e| e.name.ends_with(".xml") || e.name.ends_with(".rels"))
        .map(|e| {
            let text = String::from_utf8(zip.extract(e).unwrap()).unwrap();
            (e.name.clone(), text)
        })
        .collect()
}

/// A paragraph's text, through text boxes too.
fn text(b: &Block) -> String {
    match b {
        Block::Paragraph(p) => p
            .content
            .iter()
            .map(|i| match i {
                Inline::TextBox { blocks, .. } => blocks.iter().map(text).collect(),
                other => other.text(),
            })
            .collect(),
        other => other.plain_text(),
    }
}

/// Every run in the body, tables and text boxes: its text and properties.
fn runs(blocks: &[Block]) -> Vec<(String, RunProps)> {
    fn inlines(items: &[Inline], out: &mut Vec<(String, RunProps)>) {
        for i in items {
            match i {
                Inline::Run(r) => out.push((r.text.clone(), r.props.clone())),
                Inline::Hyperlink(h) => {
                    out.extend(h.runs.iter().map(|r| (r.text.clone(), r.props.clone())));
                    inlines(&h.content, out);
                }
                Inline::TextBox { blocks, .. } => out.extend(runs(blocks)),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => inlines(&p.content, &mut out),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        out.extend(runs(&cell.blocks));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The first run whose text contains `needle`.
fn run_with(doc: &Document, needle: &str) -> RunProps {
    runs(&doc.body)
        .into_iter()
        .find(|(t, _)| t.contains(needle))
        .unwrap_or_else(|| panic!("no run with {needle:?}"))
        .1
}

/// The body's section properties in order, each with the body index it
/// ends at.
fn sections_of(doc: &Document) -> Vec<(usize, String)> {
    doc.body
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            Block::Paragraph(p) => p.props.section_break.clone().map(|s| (i, s)),
            Block::SectionProperties(s) => Some((i, s.raw.clone())),
            _ => None,
        })
        .collect()
}

/// The index of the first top-level block whose text contains `needle`.
fn at(doc: &Document, needle: &str) -> usize {
    doc.body
        .iter()
        .position(|b| text(b).contains(needle))
        .unwrap_or_else(|| panic!("no block with {needle:?}"))
}

fn is_picture(b: &Block) -> bool {
    matches!(b, Block::Paragraph(p) if p.content.iter().any(|i| matches!(i, Inline::Raw(r) if r.contains("a:blip"))))
}

fn media(pkg: &Package) -> Vec<String> {
    pkg.part_names()
        .into_iter()
        .filter(|n| n.starts_with("word/media/"))
        .map(str::to_string)
        .collect()
}

// ---- convert.pdf, criterion 1; each check also runs on the saved and
// reloaded document (criterion 2) ----

fn check_picture(doc: &Document, pkg: &Package) {
    let pictures: Vec<usize> = (0..doc.body.len())
        .filter(|&i| is_picture(&doc.body[i]))
        .collect();
    assert_eq!(pictures.len(), 1, "one inline picture");
    let p = pictures[0];
    assert!(
        at(doc, "Each visit followed") < p,
        "after the paragraph before it"
    );
    assert!(
        p < at(doc, "The table below"),
        "before the paragraph after it"
    );
    let first_section_end = sections_of(doc)[0].0;
    assert!(p < first_section_end, "on page 1's section");
    assert_eq!(media(pkg).len(), 1, "its media part");
}

fn check_table(doc: &Document) {
    let tables: Vec<&docxcore::model::Table> = doc
        .body
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(
        tables.len(),
        1,
        "the ruled table, and the columns are no table"
    );
    let cells: Vec<Vec<String>> = tables[0]
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    c.blocks
                        .iter()
                        .map(text)
                        .collect::<String>()
                        .trim()
                        .to_string()
                })
                .collect()
        })
        .collect();
    assert_eq!(
        cells,
        vec![
            vec!["Site", "Visits", "Kinds"],
            vec!["Meadow", "42", "31"],
            vec!["Marsh", "38", "27"]
        ]
    );
}

fn check_columns(doc: &Document) {
    let sects = sections_of(doc);
    let heading = at(doc, "Observations by Site");
    let (k, band) = sects
        .iter()
        .enumerate()
        .find(|(_, (end, _))| *end >= heading)
        .map(|(k, (_, s))| (k, s.clone()))
        .unwrap();
    assert_eq!(
        PageGeom::from_sect_pr(&band).cols,
        1,
        "the heading's own section"
    );
    let cols = &sects[k + 1].1;
    let geom = PageGeom::from_sect_pr(cols);
    assert_eq!(geom.cols, 2, "{cols}");
    assert!(cols.contains("<w:type w:val=\"continuous\"/>"), "{cols}");
    assert_eq!((geom.w, geom.h), (12240, 15840), "the real page");
    assert!(heading > sects[k - 1].0, "the band starts page 2");
    let left = at(doc, "north meadow held");
    let right = at(doc, "south marsh was quieter");
    assert!(heading < left && left < right && right <= sects[k + 1].0);
    // The heading is still a heading, sized against its page.
    let Block::Paragraph(p) = &doc.body[heading] else {
        panic!()
    };
    assert!(
        p.props.heading_level == Some(1) || p.props.style_id.as_deref() == Some("Heading1"),
        "{:?}",
        p.props
    );
}

fn check_running(doc: &Document, pkg: &Package) {
    let body: String = doc.body.iter().map(text).collect();
    assert!(
        !body.contains("Annual Field Notes"),
        "header text left the body"
    );
    assert!(!body.contains("of 3"), "footer text left the body");
    let sects = sections_of(doc);
    let first = &sects[0].1;
    let header = docxcore::sect::hf_reference(first, true, "default").expect("a header");
    let footer = docxcore::sect::hf_reference(first, false, "default").expect("a footer");
    for (_, s) in &sects[1..] {
        assert!(
            docxcore::sect::hf_reference(s, true, "default").is_none(),
            "inherited"
        );
    }
    assert!(!docxcore::sect::has_flag(first, "w:titlePg"));
    let part = |rid: &str| {
        let rels =
            String::from_utf8(pkg.part("word/_rels/document.xml.rels").unwrap().to_vec()).unwrap();
        let i = rels.find(&format!("Id=\"{rid}\"")).unwrap();
        let rel = &rels[rels[..i].rfind('<').unwrap()..];
        let rel = &rel[..rel.find('>').unwrap()];
        let target = rel
            .split("Target=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        String::from_utf8(pkg.part(&format!("word/{target}")).unwrap().to_vec()).unwrap()
    };
    let header = part(&header);
    assert!(header.contains("Annual Field Notes"), "{header}");
    assert!(header.contains("<w:jc w:val=\"center\"/>"), "{header}");
    let footer = part(&footer);
    assert!(footer.contains("w:instr=\" PAGE \""), "{footer}");
    assert!(footer.contains("w:instr=\" NUMPAGES \""), "{footer}");
    assert!(
        footer.contains(">Page </w:t>") && footer.contains("> of </w:t>"),
        "{footer}"
    );
}

fn check_fonts(doc: &Document) {
    let times = run_with(doc, "survey began");
    assert_eq!(times.font.as_deref(), Some("Times New Roman"));
    assert_eq!(times.size_half_pts, Some(22));
    let title = run_with(doc, "Field Notes for the Year");
    assert_eq!(title.font.as_deref(), Some("Arial"));
    assert!(title.bold);
    assert_eq!(title.size_half_pts, Some(40));
    let cell = run_with(doc, "Meadow");
    assert_eq!(cell.font.as_deref(), Some("Arial"));
    assert!(!cell.bold);
    let courier = run_with(doc, "06:00");
    assert_eq!(courier.font.as_deref(), Some("Courier New"));
    assert_eq!(courier.size_half_pts, Some(20));
}

fn check_colours(doc: &Document) {
    assert_eq!(run_with(doc, "heron").color.as_deref(), Some("FF0000"));
    assert_eq!(run_with(doc, "returned to the lake").color, None);
    assert_eq!(run_with(doc, "survey began").color, None);
}

/// Every `w:color` in every XML part of the saved file: no code colour
/// anywhere, only the red word's.
fn check_no_code(docx: &[u8], allowed: &[&str]) {
    for (name, xml) in xml_parts(docx) {
        for v in xml.split("<w:color w:val=\"").skip(1) {
            let v = &v[..6];
            assert!(allowed.contains(&v), "{name}: colour {v}");
        }
    }
}

#[test]
fn convert_keeps_the_picture_in_place() {
    let c = converted("convert");
    check_picture(&c.doc, c.base.as_ref().unwrap());
}

#[test]
fn convert_detects_the_ruled_table() {
    check_table(&converted("convert").doc);
}

#[test]
fn convert_makes_columns_after_a_full_width_heading() {
    check_columns(&converted("convert").doc);
}

#[test]
fn convert_moves_running_header_and_footer_out_of_the_body() {
    let c = converted("convert");
    check_running(&c.doc, c.base.as_ref().unwrap());
}

#[test]
fn convert_maps_fonts_and_keeps_sizes() {
    check_fonts(&converted("convert").doc);
}

#[test]
fn saved_conversion_has_no_code_colour_and_keeps_red() {
    let c = converted("convert");
    check_colours(&c.doc);
    let (bytes, back) = saved(&c);
    check_no_code(&bytes, &["FF0000"]);
    check_colours(&back.document);
}

/// Criterion 2, the side-effect test: what the tab writes, loaded again.
#[test]
fn converted_layout_survives_save_and_reload() {
    let (bytes, back) = saved(&converted("convert"));
    let doc = &back.document;
    check_picture(doc, &back);
    check_table(doc);
    check_columns(doc);
    check_running(doc, &back);
    check_fonts(doc);
    // Saved twice, nothing moves.
    let again = docxcore::package::load_package(
        &ConvertedDoc {
            doc: back.document.clone(),
            base: Some(back.clone()),
        }
        .docx(),
    )
    .unwrap();
    assert_eq!(again.document.body.len(), doc.body.len());
    assert!(!bytes.is_empty());
}

/// A page the export keeps as positioned text boxes (`rotated.pdf`: its
/// /Rotate turns its text): each box's text, in both of the copies the
/// export writes (DrawingML and the VML fallback), gets its font and no
/// code colour, and keeps them through a save.
#[test]
fn a_positioned_page_keeps_its_text_box_font_through_save() {
    let c = converted("rotated");
    let boxed = runs(&c.doc.body);
    assert!(
        boxed.iter().any(|(t, _)| t.contains("Rotated page")),
        "{boxed:?}"
    );
    // Ghostscript's Times-Roman.
    let font = "Times New Roman";
    assert!(
        boxed.iter().all(|(_, p)| p.font.as_deref() == Some(font)),
        "{boxed:?}"
    );
    let (bytes, back) = saved(&c);
    check_no_code(&bytes, &[]);
    let document = xml_parts(&bytes)
        .into_iter()
        .find(|(n, _)| n == "word/document.xml")
        .unwrap()
        .1;
    for copy in document.split("<mc:Fallback>").skip(1) {
        let copy = &copy[..copy.find("</mc:Fallback>").unwrap()];
        assert!(copy.contains("w:ascii=\"Times New Roman\""), "{copy}");
    }
    assert!(document.contains("<mc:Fallback>"));
    let back_runs = runs(&back.document.body);
    assert!(back_runs.iter().any(|(t, _)| t.contains("Rotated page")));
    assert!(
        back_runs
            .iter()
            .all(|(_, p)| p.font.as_deref() == Some(font) && p.color.is_none())
    );
}

// ---- the cairo files, criterion 3 ----

#[test]
fn columns_pdf_converts_to_a_band_and_two_columns() {
    let c = converted("columns");
    let doc = &c.doc;
    let sects = sections_of(doc);
    assert_eq!(sects.len(), 2, "{sects:?}");
    let heading = at(doc, "Two columns");
    assert!(heading < sects[0].0);
    assert_eq!(PageGeom::from_sect_pr(&sects[0].1).cols, 1);
    let geom = PageGeom::from_sect_pr(&sects[1].1);
    assert_eq!(geom.cols, 2);
    assert_eq!((geom.w, geom.h), (12240, 15840));
    assert!(sects[1].1.contains("continuous"));
    assert!(at(doc, "Left column, first line") < at(doc, "Right column starts here"));
    assert_eq!(
        run_with(doc, "Two columns").font.as_deref(),
        Some("DejaVu Sans")
    );
    assert_eq!(
        run_with(doc, "Left column").font.as_deref(),
        Some("DejaVu Serif")
    );
    assert!(doc.body.iter().all(|b| !matches!(b, Block::Table(_))));
}

#[test]
fn scanned_pdf_keeps_its_pictures_and_caption() {
    let c = converted("scanned");
    assert_eq!(media(c.base.as_ref().unwrap()).len(), 2);
    let all: String = c.doc.body.iter().map(text).collect();
    assert!(all.contains("A scanned page and a picture."), "{all}");
    let (bytes, back) = saved(&c);
    assert_eq!(media(&back).len(), 2);
    assert!(!bytes.is_empty());
}

// ---- criterion 4 ----

#[test]
fn a_single_page_keeps_its_lines_and_one_column() {
    let md =
        docxcore::markdown::from_markdown("# A heading\n\nFirst paragraph.\n\nSecond paragraph.\n");
    let pdf = docxcore::export::to_pdf(&md, &docxcore::export::PdfOptions::default());
    let c = layout(&pdf).unwrap().unwrap();
    let all: Vec<String> = c.doc.body.iter().map(text).collect();
    for want in ["A heading", "First paragraph.", "Second paragraph."] {
        assert!(all.iter().any(|t| t.contains(want)), "{all:?}");
    }
    let sects = sections_of(&c.doc);
    assert_eq!(sects.len(), 1);
    assert_eq!(PageGeom::from_sect_pr(&sects[0].1).cols, 1);
    assert!(docxcore::sect::hf_reference(&sects[0].1, true, "default").is_none());
    let pkg = c.base.as_ref().unwrap();
    assert!(
        !pkg.part_names()
            .iter()
            .any(|n| n.starts_with("word/header") || n.starts_with("word/footer"))
    );
}

// ---- criterion 5 ----

#[test]
fn every_corpus_pdf_converts_or_says_why() {
    let dir = corpus_path("text").parent().unwrap().to_path_buf();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".pdf"))
        .map(|n| n.trim_end_matches(".pdf").to_string())
        .collect();
    names.sort();
    names.push("source".into());
    assert!(names.len() >= 24, "{names:?}");
    // What the layout step keeps nothing of: text only in annotations, or
    // nothing readable at all.
    let without_text = ["annots", "annots-noap", UNREADABLE];
    for name in &names {
        let bytes = corpus(name);
        // The layout step itself, not hidden behind the fallback: never a
        // panic, and a document for every file with text it may read.
        let own = catch_unwind(AssertUnwindSafe(|| layout(&bytes)))
            .unwrap_or_else(|_| panic!("{name}: the layout step panicked"));
        if user_password(name).is_some() || without_text.contains(&name.as_str()) {
            assert!(!matches!(own, Ok(Some(_))), "{name}");
        } else {
            assert!(matches!(own, Ok(Some(_))), "{name}: {:?}", own.err());
        }
        let start = Instant::now();
        let got = convert(&bytes);
        let took = start.elapsed();
        assert!(took < crate::convert_child::TIMEOUT, "{name} took {took:?}");
        let encrypted = ENCRYPTED.iter().any(|e| e.0 == name.as_str());
        match got {
            Ok(c) => {
                assert!(user_password(name).is_none(), "{name} needs its password");
                if encrypted {
                    let all: String = c.doc.body.iter().map(text).collect();
                    assert!(!all.trim().is_empty(), "{name}");
                }
            }
            Err(why) => {
                assert!(!why.trim().is_empty(), "{name}");
                if user_password(name).is_some() {
                    assert!(why.contains("password-protected"), "{name}: {why}");
                }
            }
        }
        eprintln!("{name}: {took:?}");
    }
}

// ---- criterion 6 ----

fn simple_pdf() -> Vec<u8> {
    let md = docxcore::markdown::from_markdown("Words the text importer reads.\n");
    docxcore::export::to_pdf(&md, &docxcore::export::PdfOptions::default())
}

#[test]
fn a_failing_or_panicking_pdfcraft_step_falls_back() {
    let pdf = simple_pdf();
    let texts = |c: ConvertedDoc| c.doc.body.iter().map(text).collect::<String>();
    let failed = convert_with(&pdf, |_| Err("PdfCraft refused it".into())).unwrap();
    assert!(failed.base.is_none(), "the text importer's document");
    assert!(texts(failed).contains("Words the text importer reads."));
    let panicked = convert_with(&pdf, |_| panic!("a bug in the layout step")).unwrap();
    assert!(texts(panicked).contains("Words the text importer reads."));
    // The layout step's own document when it has one.
    let own = convert_with(&pdf, layout).unwrap();
    assert!(own.base.is_some());
}

/// The layout step stops at its deadline, so the text importer still has
/// time in the same child (#2257 r1).
#[test]
fn a_layout_past_its_deadline_falls_back_to_the_text_importer() {
    let bytes = corpus("convert");
    let past = Instant::now();
    assert_eq!(
        layout_until(&bytes, past).err().as_deref(),
        Some(LAYOUT_TOO_LONG)
    );
    let c = convert_with(&bytes, |b| layout_until(b, past)).unwrap();
    assert!(c.base.is_none(), "the text importer's document");
    let all: String = c.doc.body.iter().map(text).collect();
    assert!(all.contains("spoonbills"), "{all}");
    // Half the child's time, the other half left to the fallback.
    assert_eq!(LAYOUT_TIME * 2, crate::convert_child::TIMEOUT);
    assert!(
        layout_until(&bytes, Instant::now() + LAYOUT_TIME)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_conversion_that_keeps_nothing_falls_back() {
    let blank = docxcore::export::to_pdf(
        &docxcore::markdown::from_markdown(""),
        &docxcore::export::PdfOptions::default(),
    );
    assert!(layout(&blank).unwrap().is_none());
    let why = convert(&blank).err().expect("the text importer's error");
    assert_eq!(why, docxcore::import::import_pdf(&blank).err().unwrap());
    let kept_nothing = convert_with(&simple_pdf(), |_| Ok(None)).unwrap();
    assert!(kept_nothing.base.is_none());
}

#[test]
fn code_colours_are_exact_and_never_black() {
    for k in [1, 2, 255, 256, 65_535, 65_536, MAX_CODES] {
        let hex = hex(code_color(k)).unwrap();
        assert_eq!(code_of(&hex, MAX_CODES), Some(k), "{hex}");
    }
    assert_eq!(code_of("000000", 10), None);
    assert_eq!(code_of("00000B", 10), None);
    assert_eq!(code_of("FF0000", 10), None);
}

#[test]
fn twin_copies_are_the_identical_text_box_contents() {
    let raw = "<w:r><mc:AlternateContent><mc:Choice><w:txbxContent><w:p>a</w:p></w:txbxContent>\
               </mc:Choice><mc:Fallback><w:txbxContent><w:p>a</w:p></w:txbxContent>\
               <w:txbxContent><w:p>b</w:p></w:txbxContent></mc:Fallback></mc:AlternateContent></w:r>";
    let twins = twin_copies(raw);
    assert_eq!(twins.len(), 2);
    assert!(twins.iter().all(|r| &raw[r.clone()] == "<w:p>a</w:p>"));
    let nested = "<w:txbxContent><w:txbxContent>x</w:txbxContent></w:txbxContent>";
    assert!(twin_copies(nested).is_empty());
}
