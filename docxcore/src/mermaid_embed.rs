//! Replace a mermaid `Inline::SmartArt` drawing (the native DrawingML shape
//! group [`crate::mermaid::to_drawing`] emits) with a Word-valid **picture** —
//! a `pic:pic` carrying a PNG blip plus an `asvg:svgBlip` fallback — so a
//! rendered mermaid diagram becomes a real embedded image.
//!
//! The mermaid source is preserved in the picture's `descr` using the exact
//! same `mermaid:`-prefixed, [`crate::mermaid::escape_source`]-encoded form
//! [`crate::mermaid::to_drawing`] writes, so [`crate::mermaid::source_of`] —
//! and therefore the Word → Markdown ```` ```mermaid ```` round-trip — still
//! recovers it losslessly. The `Inline::SmartArt` variant itself is kept
//! (only its `raw` field changes): both [`crate::load::parse_document_xml`]
//! and [`crate::markdown::to_markdown`] key off `source_of(raw)`, not the
//! drawing's internal shape, so a picture-backed diagram is indistinguishable
//! from a shape-backed one to the rest of the crate.

use crate::model::{Block, Document, Inline};
use crate::package::Package;

/// A rendered mermaid diagram, ready to embed as a picture.
pub struct MermaidImage {
    /// The mermaid source (unescaped, as written in a ```` ```mermaid ````
    /// fence) — matched against [`crate::mermaid::source_of`] on each
    /// `Inline::SmartArt` already in the document.
    pub source: String,
    /// PNG bytes (the primary blip Word always renders).
    pub png: Vec<u8>,
    /// SVG bytes (carried via the `asvg:svgBlip` extension for crisp
    /// re-render/export in apps that support it; Word itself still displays
    /// the PNG).
    pub svg: Vec<u8>,
    /// Target width, in EMU (`wp:extent`/`a:ext` `cx`).
    pub w_emu: i64,
    /// Target height, in EMU (`wp:extent`/`a:ext` `cy`).
    pub h_emu: i64,
}

/// For each `Inline::SmartArt { raw, .. }` found anywhere in `doc` (paragraphs,
/// and table cells recursively) whose embedded mermaid source matches an
/// entry in `images` (by exact `source` string), add that image's PNG + SVG
/// as media parts in `pkg` (via [`Package::add_media_part`]) and rewrite the
/// inline's `raw` to a picture drawing embedding them. SmartArt with no
/// matching image — including non-mermaid SmartArt, where `source_of` returns
/// `None` — is left completely untouched (byte-identical `raw`).
pub fn embed_images(pkg: &mut Package, doc: &mut Document, images: &[MermaidImage]) {
    for block in &mut doc.body {
        embed_in_block(pkg, block, images);
    }
}

fn embed_in_block(pkg: &mut Package, block: &mut Block, images: &[MermaidImage]) {
    match block {
        Block::Paragraph(p) => embed_in_inlines(pkg, &mut p.content, images),
        Block::Table(t) => {
            for row in &mut t.rows {
                for cell in &mut row.cells {
                    for b in &mut cell.blocks {
                        embed_in_block(pkg, b, images);
                    }
                }
            }
        }
        Block::Raw(_) => {}
    }
}

fn embed_in_inlines(pkg: &mut Package, inlines: &mut [Inline], images: &[MermaidImage]) {
    for inline in inlines {
        let Inline::SmartArt { raw, .. } = inline else {
            continue;
        };
        let Some(source) = crate::mermaid::source_of(raw) else {
            continue; // not a mermaid drawing (or not decodable) — untouched
        };
        let Some(img) = images.iter().find(|i| i.source == source) else {
            continue; // no rendered image supplied for this source — untouched
        };
        let rid_png = pkg.add_media_part(&img.png, "png");
        let rid_svg = pkg.add_media_part(&img.svg, "svg");
        *raw = picture_drawing_xml(&rid_png, &rid_svg, img.w_emu, img.h_emu, &source);
    }
}

/// The `<w:r><w:drawing>…</w:drawing></w:r>` XML for a picture run: a PNG blip
/// (`r:embed="{rid_png}"`) with an `asvg:svgBlip` fallback
/// (`r:embed="{rid_svg}"`) via the standard Office 2016 SVG blip extension
/// (`{96DAC541-7B7A-43D3-8B79-37D633B846F1}`), sized `w`x`h` EMU. Wrapped in a
/// `<w:r>` because [`Inline::SmartArt::raw`] holds the *whole run* XML (see
/// [`crate::load::parse_paragraph`]'s `w:r` case and
/// [`crate::serialize`]'s verbatim `s.push_str(raw)`), not just the drawing —
/// an unwrapped `<w:drawing>` is invalid directly inside `<w:p>`. `descr` (the
/// mermaid-source-carrying attribute) is set on both `wp:docPr` and
/// `pic:cNvPr` — [`crate::mermaid::source_of`] only needs the first `descr=`
/// in document order, but both mirror the shape drawing's convention.
fn picture_drawing_xml(rid_png: &str, rid_svg: &str, w: i64, h: i64, source: &str) -> String {
    let descr = crate::mermaid::xml_escape_attr(&format!(
        "mermaid:{}",
        crate::mermaid::escape_source(source)
    ));
    format!(
        "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\">\
         <wp:extent cx=\"{w}\" cy=\"{h}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
         <wp:docPr id=\"1\" name=\"Mermaid Diagram\" descr=\"{descr}\"/>\
         <wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
         <a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
         <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:nvPicPr><pic:cNvPr id=\"1\" name=\"mermaid.png\" descr=\"{descr}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rid_png}\">\
         <a:extLst><a:ext uri=\"{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}\">\
         <asvg:svgBlip xmlns:asvg=\"http://schemas.microsoft.com/office/drawing/2016/SVG/main\" r:embed=\"{rid_svg}\"/>\
         </a:ext></a:extLst></a:blip>\
         <a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
         </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{from_markdown, to_markdown};
    use crate::package::{load_package, new_markdown_package, save_package};

    /// Find the single `Inline::SmartArt`'s `raw` XML in a freshly-built
    /// mermaid document (one paragraph, one SmartArt inline).
    fn smartart_raw(doc: &Document) -> &str {
        for block in &doc.body {
            let Block::Paragraph(p) = block else { continue };
            for inline in &p.content {
                if let Inline::SmartArt { raw, .. } = inline {
                    return raw;
                }
            }
        }
        panic!("no SmartArt inline found in document");
    }

    #[test]
    fn embeds_picture_with_png_and_svg_blip() {
        let mut doc = from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
        let mut pkg = new_markdown_package(doc.clone());
        let img = MermaidImage {
            source: "flowchart TD\nA-->B".into(),
            png: vec![0x89, 0x50, 0x4E, 0x47],
            svg: b"<svg/>".to_vec(),
            w_emu: 3_000_000,
            h_emu: 1_500_000,
        };
        embed_images(&mut pkg, &mut doc, std::slice::from_ref(&img));

        let raw = smartart_raw(&doc);
        assert!(raw.contains("<pic:pic"), "{raw}");
        assert!(raw.contains("a:blip") && raw.contains("r:embed="), "{raw}");
        assert!(raw.contains("svgBlip"), "svg blip missing: {raw}");
        // Pin the SVG-blip extension GUID: Word only honors `asvg:svgBlip`
        // under the correct `{96DAC541-7B7A-43D3-8B79-37D633B846F1}` extension
        // URI — the similar-looking `useLocalDpi` GUID
        // (`{28A0092B-C50C-407E-A947-70E740481C1C}`) is silently ignored, which
        // degrades the embed to PNG-only and defeats the crisp-SVG purpose.
        assert!(
            raw.contains("{96DAC541-7B7A-43D3-8B79-37D633B846F1}"),
            "wrong (or missing) SVG-blip extension GUID: {raw}"
        );
        assert!(
            raw.contains("cx=\"3000000\"") && raw.contains("cy=\"1500000\""),
            "{raw}"
        );
        // Source preserved for round-trip.
        assert_eq!(
            crate::mermaid::source_of(raw).as_deref(),
            Some("flowchart TD\nA-->B")
        );

        // Media parts + rels + content-types present.
        assert!(pkg.part("word/media/image1.png").is_some());
        assert!(pkg.part("word/media/image2.svg").is_some());
        let rels =
            String::from_utf8_lossy(pkg.part("word/_rels/document.xml.rels").unwrap()).into_owned();
        assert!(rels.contains("/image") && rels.contains(".png") && rels.contains(".svg"));
        let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned();
        assert!(ct.contains("png") && ct.contains("svg"));
    }

    #[test]
    fn no_image_leaves_shapes_untouched() {
        let mut doc = from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
        let mut pkg = new_markdown_package(doc.clone());
        let before = smartart_raw(&doc).to_string();
        embed_images(&mut pkg, &mut doc, &[]); // no images supplied
        assert_eq!(
            smartart_raw(&doc),
            before,
            "unmatched diagram must be unchanged"
        );
    }

    #[test]
    fn embedded_picture_round_trips_to_markdown() {
        let mut doc = from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
        let mut pkg = new_markdown_package(doc.clone());
        let img = MermaidImage {
            source: "flowchart TD\nA-->B".into(),
            png: vec![1, 2, 3],
            svg: b"<svg/>".to_vec(),
            w_emu: 3_000_000,
            h_emu: 1_500_000,
        };
        embed_images(&mut pkg, &mut doc, &[img]);
        pkg.document = doc.clone();
        let bytes = save_package(&pkg);

        // Reload and confirm the mermaid source survives.
        let reloaded = load_package(&bytes).unwrap();
        let md = to_markdown(&reloaded.document);
        assert!(
            md.contains("```mermaid") && md.contains("flowchart TD"),
            "{md}"
        );
    }
}
