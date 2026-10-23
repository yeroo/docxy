//! Picture watermarks in PDF export (#797): a header's VML or DrawingML
//! watermark picture is written as an image XObject, washed out, on the pages
//! that apply that header and on no other.

use std::rc::Rc;

use docxcore::deflate::zlib;
use docxcore::export::{PdfOptions, to_pdf};
use docxcore::package::load_package;
use docxcore::styles::StyleSheet;
use docxcore::zipwrite::write_zip;

#[path = "common/pdf_read.rs"]
mod pdf_read;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn png(color: u8, depth: u8, interlace: u8, row: &[u8], palette: Option<&[u8]>) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(kind);
        out.extend(body);
        out.extend([0, 0, 0, 0]);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(2u32.to_be_bytes());
    ihdr.extend(2u32.to_be_bytes());
    ihdr.extend([depth, color, 0, 0, interlace]);
    chunk(&mut out, b"IHDR", &ihdr);
    if let Some(p) = palette {
        chunk(&mut out, b"PLTE", p);
    }
    let mut raw = Vec::new();
    for _ in 0..2 {
        raw.push(0);
        raw.extend(row);
    }
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn jpeg() -> Vec<u8> {
    let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 17, 8];
    b.extend(50u16.to_be_bytes());
    b.extend(100u16.to_be_bytes());
    b.push(3);
    for c in 1..=3u8 {
        b.extend([c, 0x11, 0]);
    }
    b.extend([0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
    b
}

const VML: &str = r##"<w:p><w:r><w:pict><v:shape id="WordPictureWatermark1" type="#_x0000_t75" style="position:absolute;width:200pt;height:100pt;rotation:315"><v:imagedata r:id="rIdI" o:title="wm"/></v:shape></w:pict></w:r></w:p>"##;
const DRAWINGML: &str = r#"<w:p><w:r><w:drawing><wp:anchor><wp:extent cx="2540000" cy="1270000"/><wp:docPr id="1" name="WordPictureWatermark2"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rIdI"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#;

/// Two sections, one page each: the first applies a header holding
/// `watermark` (its picture is `media`, named `word/media/wm.<ext>`), the
/// second a plain header.
fn docx(watermark: &str, ext: &str, media: &[u8]) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
    );
    let document_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH1" Type="{REL}/header" Target="header1.xml"/><Relationship Id="rIdH2" Type="{REL}/header" Target="header2.xml"/></Relationships>"#
    );
    let header_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdI" Type="{REL}/image" Target="media/wm.{ext}"/></Relationships>"#
    );
    let ns = format!(
        r#"xmlns:w="{W}" xmlns:r="{R}" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {ns}><w:body>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/></w:sectPr></w:pPr><w:r><w:t>One</w:t></w:r></w:p>
<w:p><w:r><w:t>Two</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rIdH2"/></w:sectPr>
</w:body></w:document>"#
    );
    let header1 = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr {ns}>{watermark}</w:hdr>"#
    );
    let header2 = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr {ns}><w:p><w:r><w:t>Plain</w:t></w:r></w:p></w:hdr>"#
    );
    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.into_bytes()),
        ("word/document.xml".to_string(), document.into_bytes()),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels.into_bytes(),
        ),
        ("word/header1.xml".to_string(), header1.into_bytes()),
        (
            "word/_rels/header1.xml.rels".to_string(),
            header_rels.into_bytes(),
        ),
        ("word/header2.xml".to_string(), header2.into_bytes()),
        (format!("word/media/wm.{ext}"), media.to_vec()),
    ])
}

/// The PDF of `docx`, validated, and each page's dictionary and content.
fn print(docx: &[u8]) -> (Vec<u8>, Vec<(String, String)>) {
    let pkg = load_package(docx).expect("load");
    let opts = PdfOptions::from_package(&pkg, Rc::new(StyleSheet::default()));
    let pdf = to_pdf(&pkg.document, &opts);
    let objs = pdf_read::validate(&pdf);
    let pages = objs
        .values()
        .filter(|o| o.dict.starts_with("<< /Type /Page /Parent"))
        .map(|page| {
            let id: usize = page
                .dict
                .split("/Contents ")
                .nth(1)
                .and_then(|r| r.split(' ').next())
                .and_then(|n| n.parse().ok())
                .expect("contents");
            let content = String::from_utf8_lossy(&pdf_read::decoded(&objs[&id])).into_owned();
            (page.dict.clone(), content)
        })
        .collect();
    (pdf, pages)
}

fn image_dicts(pdf: &[u8]) -> Vec<String> {
    pdf_read::objects(pdf)
        .into_values()
        .filter(|o| o.dict.contains("/Subtype /Image"))
        .map(|o| o.dict)
        .collect()
}

#[test]
fn a_vml_jpeg_watermark_is_drawn_washed_out_on_its_header_s_pages_only_797() {
    let (pdf, pages) = print(&docx(VML, "jpeg", &jpeg()));
    assert_eq!(pages.len(), 2);
    let (dict, content) = &pages[0];
    assert!(dict.contains("/XObject << /Im0 "), "{dict}");
    assert!(
        dict.contains("/ExtGState << /GS50 << /ca 0.50 >> >>"),
        "{dict}"
    );
    // 200 x 100 pt shape, centred, 45 degrees counter-clockwise.
    assert!(
        content.contains("/GS50 gs\n0.7071 0.7071 -0.7071 0.7071 306.00 396.00 cm\n200.00 0 0 100.00 -100.00 -50.00 cm\n/Im0 Do"),
        "{content}"
    );
    let (dict, content) = &pages[1];
    assert!(
        !content.contains(" Do") && !dict.contains("XObject"),
        "{dict}{content}"
    );
    let images = image_dicts(&pdf);
    assert_eq!(images.len(), 1);
    assert!(
        images[0].contains("/Width 100 /Height 50")
            && images[0].contains("/DeviceRGB")
            && images[0].contains("/DCTDecode"),
        "{}",
        images[0]
    );
}

#[test]
fn a_drawingml_png_watermark_uses_its_extent_and_a_png_predictor_797() {
    let rgb = png(2, 8, 0, &[255, 0, 0, 0, 255, 0], None);
    let (pdf, pages) = print(&docx(DRAWINGML, "png", &rgb));
    // 2540000 x 1270000 EMU = 200 x 100 pt.
    assert!(
        pages[0].1.contains("200.00 0 0 100.00 -100.00 -50.00 cm"),
        "{}",
        pages[0].1
    );
    let images = image_dicts(&pdf);
    assert_eq!(images.len(), 1);
    assert!(
        images[0].contains("/FlateDecode")
            && images[0].contains("/Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns 2"),
        "{}",
        images[0]
    );
}

#[test]
fn an_indexed_png_gets_an_indexed_colour_space_797() {
    let pal = png(3, 8, 0, &[0, 1], Some(&[255, 0, 0, 0, 0, 255]));
    let (pdf, pages) = print(&docx(VML, "png", &pal));
    assert!(pages[0].1.contains("/Im0 Do"));
    assert!(image_dicts(&pdf)[0].contains("[/Indexed /DeviceRGB 1 <ff00000000ff>]"));
}

#[test]
fn unsupported_pictures_draw_nothing_and_the_file_stays_valid_797() {
    // Alpha, interlaced, 16-bit, and a format we cannot pass through.
    let cases = [
        ("png", png(6, 8, 0, &[0; 8], None)),
        ("png", png(2, 8, 1, &[0; 6], None)),
        ("png", png(2, 16, 0, &[0; 12], None)),
        ("png", b"GIF89a not a png".to_vec()),
    ];
    for (ext, media) in cases {
        let (pdf, pages) = print(&docx(VML, ext, &media));
        assert_eq!(pages.len(), 2);
        assert!(
            pages
                .iter()
                .all(|(d, c)| !c.contains(" Do") && !d.contains("XObject"))
        );
        assert!(image_dicts(&pdf).is_empty());
        assert!(!String::from_utf8_lossy(&pdf).contains("/ExtGState"));
    }
}

#[test]
fn a_sizeless_picture_is_its_pixel_size_at_96_dpi_797() {
    let hdr = r#"<w:p><w:r><w:pict><v:shape id="WordPictureWatermark1"><v:imagedata r:id="rIdI"/></v:shape></w:pict></w:r></w:p>"#;
    let (_, pages) = print(&docx(hdr, "jpeg", &jpeg()));
    // 100 x 50 px = 75 x 37.5 pt.
    assert!(
        pages[0].1.contains("75.00 0 0 37.50 -37.50 -18.75 cm"),
        "{}",
        pages[0].1
    );
}
