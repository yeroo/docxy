//! Open/save a whole `.docx` while preserving everything we don't model.
//!
//! The save strategy that keeps documents from being corrupted: keep **every**
//! original ZIP part byte-for-byte, and on save rewrite only `word/document.xml`
//! from the [`Document`] model. The trailing section properties (`w:sectPr`,
//! which carry page size/margins/orientation) are captured verbatim and
//! re-inserted, so page geometry survives a round-trip even though it isn't
//! modeled.
//!
//! Known limitations (documented, not silent): body content other than
//! paragraphs/tables and the final `sectPr` — e.g. bookmarks, mid-document
//! section breaks, comments anchors — is not reconstructed by the serializer and
//! is dropped on save. Full raw-node preservation is a later refinement.

use crate::load::{LoadError, parse_document_xml, parse_rels_xml};
use crate::model::Document;
use crate::serialize::document_to_xml;
use crate::zip::ZipArchive;
use crate::zipwrite::write_zip;

const OLE2: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// A loaded `.docx`: the editable [`Document`] plus all original parts so save
/// can preserve what isn't modeled.
#[derive(Debug, Clone)]
pub struct Package {
    parts: Vec<(String, Vec<u8>)>,
    doc_index: usize,
    sect_pr: String,
    /// The editable document. Mutate this, then [`save_package`].
    pub document: Document,
}

impl Package {
    /// Names of all parts in the container (for inspection/tests).
    pub fn part_names(&self) -> Vec<&str> {
        self.parts.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// The captured trailing section properties (`w:sectPr`) XML, which carries
    /// the header/footer references and page geometry.
    pub fn sect_pr(&self) -> &str {
        &self.sect_pr
    }

    /// Replace the trailing section properties (e.g. to change page orientation).
    pub fn set_sect_pr(&mut self, xml: String) {
        self.sect_pr = xml;
    }

    /// The raw bytes of a part by name.
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    /// Replace the bytes of an existing part (e.g. an edited header/footer).
    /// Returns false if no such part exists.
    pub fn set_part(&mut self, name: &str, bytes: Vec<u8>) -> bool {
        match self.parts.iter_mut().find(|(n, _)| n == name) {
            Some(e) => {
                e.1 = bytes;
                true
            }
            None => false,
        }
    }

    /// Create a new, empty default header (`is_header`) or footer part and wire it
    /// up: add the part, a `[Content_Types].xml` override, a relationship in
    /// `document.xml.rels`, and a `<w:headerReference>`/`<w:footerReference>` in
    /// the section properties. Returns the new part name.
    pub fn create_hf(&mut self, is_header: bool) -> Option<String> {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let (kind, tag, ct, reltype) = if is_header {
            (
                "header",
                "w:hdr",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
                "header",
            )
        } else {
            (
                "footer",
                "w:ftr",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
                "footer",
            )
        };
        // Unused part name word/{kind}{n}.xml.
        let mut n = 1;
        while self.part(&format!("word/{kind}{n}.xml")).is_some() {
            n += 1;
        }
        let target = format!("{kind}{n}.xml");
        let part_name = format!("word/{target}");

        // A fresh relationship id from document.xml.rels.
        let rels_name = "word/_rels/document.xml.rels";
        let rels_xml = String::from_utf8_lossy(self.part(rels_name)?).into_owned();
        let rid = next_rid(&rels_xml);

        // The part itself (one empty paragraph).
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<{tag} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\"><w:p/></{tag}>"
        );
        self.parts.push((part_name.clone(), body.into_bytes()));

        // Relationship.
        let rel =
            format!("<Relationship Id=\"{rid}\" Type=\"{R_NS}/{reltype}\" Target=\"{target}\"/>");
        let new_rels = rels_xml.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1);
        self.set_part(rels_name, new_rels.into_bytes());

        // Content-type override.
        if let Some(b) = self.part("[Content_Types].xml") {
            let ct_xml = String::from_utf8_lossy(b).into_owned();
            let ov = format!("<Override PartName=\"/{part_name}\" ContentType=\"{ct}\"/>");
            let new_ct = ct_xml.replacen("</Types>", &format!("{ov}</Types>"), 1);
            self.set_part("[Content_Types].xml", new_ct.into_bytes());
        }

        // Section reference (must be the first child of sectPr).
        let reference = format!("<w:{kind}Reference w:type=\"default\" r:id=\"{rid}\"/>");
        self.sect_pr = inject_sect_child(&self.sect_pr, &reference);
        Some(part_name)
    }

    /// Page size/margins from the captured (final) `sectPr` (US Letter default).
    pub fn page_geom(&self) -> crate::model::PageGeom {
        crate::model::PageGeom::from_sect_pr(&self.sect_pr)
    }
}

/// The next free relationship id (`rId{max+1}`) for a `.rels` part.
fn next_rid(rels: &str) -> String {
    let mut max = 0u32;
    let mut i = 0;
    while let Some(p) = rels[i..].find("Id=\"rId") {
        let s = i + p + "Id=\"rId".len();
        let num: String = rels[s..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = num.parse::<u32>() {
            max = max.max(n);
        }
        i = s;
    }
    format!("rId{}", max + 1)
}

/// Insert a child element as the first child of `<w:sectPr>` (creating/expanding
/// the element as needed). References must precede other section properties.
fn inject_sect_child(sect: &str, child: &str) -> String {
    if sect.is_empty() {
        return format!("<w:sectPr>{child}</w:sectPr>");
    }
    let Some(gt) = sect.find('>') else {
        return sect.to_string();
    };
    if sect[..gt].ends_with('/') {
        // Self-closing <w:sectPr .../> — expand it.
        return format!("{}>{child}</w:sectPr>", &sect[..gt - 1]);
    }
    let (head, tail) = sect.split_at(gt + 1);
    format!("{head}{child}{tail}")
}

/// Open a `.docx` from bytes, keeping all parts for a lossless-ish save.
pub fn load_package(data: &[u8]) -> Result<Package, LoadError> {
    let zip = match ZipArchive::open(data) {
        Some(z) => z,
        None => {
            if data.len() >= 8 && data[..8] == OLE2 {
                return Err(LoadError::Ole2);
            }
            return Err(LoadError::NotZip);
        }
    };

    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    let mut doc_index = None;
    for e in zip.entries() {
        let bytes = zip.extract(e).ok_or(LoadError::CorruptPart)?;
        if e.name == "word/document.xml" {
            doc_index = Some(parts.len());
        }
        parts.push((e.name.clone(), bytes));
    }
    let doc_index = doc_index.ok_or(LoadError::MissingDocument)?;

    let doc_xml = std::str::from_utf8(&parts[doc_index].1).map_err(|_| LoadError::NotUtf8)?;
    let read_part = |name: &str| {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
    };
    let mut rels = parts
        .iter()
        .find(|(n, _)| n == "word/_rels/document.xml.rels")
        .map(|(_, b)| parse_rels_xml(std::str::from_utf8(b).unwrap_or("")))
        .unwrap_or_default();
    if let Some((_, b)) = parts
        .iter()
        .find(|(n, _)| n == "word/_rels/document.xml.rels")
    {
        let xml = std::str::from_utf8(b).unwrap_or("");
        crate::load::set_diagram_texts(
            &mut rels,
            crate::load::collect_diagram_texts(xml, read_part),
        );
        crate::load::set_equation_texts(
            &mut rels,
            crate::load::collect_equation_texts(xml, read_part),
        );
    }
    let document = parse_document_xml(doc_xml, &rels);
    let sect_pr = extract_sectpr(doc_xml);

    Ok(Package {
        parts,
        doc_index,
        sect_pr,
        document,
    })
}

/// Build a new package around a document, with a minimal valid OPC part set.
/// Used for "create new" and as a save target for an in-memory document.
pub fn new_package(document: Document) -> Package {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"></w:styles>"#;

    let parts = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".to_string(), b"<w:document/>".to_vec()),
        (
            "word/_rels/document.xml.rels".to_string(),
            doc_rels.as_bytes().to_vec(),
        ),
        ("word/styles.xml".to_string(), styles.as_bytes().to_vec()),
    ];
    let doc_index = 2;
    Package {
        parts,
        doc_index,
        sect_pr: String::new(),
        document,
    }
}

/// Serialize the package back to `.docx` bytes (STORED ZIP).
pub fn save_package(pkg: &Package) -> Vec<u8> {
    let mut xml = document_to_xml(&pkg.document);
    if !pkg.sect_pr.is_empty() {
        xml = xml.replacen("</w:body>", &format!("{}</w:body>", pkg.sect_pr), 1);
    }
    let mut parts = pkg.parts.clone();
    parts[pkg.doc_index].1 = xml.into_bytes();
    write_zip(&parts)
}

/// Capture the last (body-level) `w:sectPr` element verbatim, if any.
fn extract_sectpr(xml: &str) -> String {
    let Some(start) = xml.rfind("<w:sectPr") else {
        return String::new();
    };
    let tail = &xml[start..];
    if let Some(close) = tail.find("</w:sectPr>") {
        return tail[..close + "</w:sectPr>".len()].to_string();
    }
    // self-closing <w:sectPr .../>
    if let Some(gt) = tail.find('>') {
        let seg = &tail[..gt + 1];
        if seg.ends_with("/>") {
            return seg.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Inline};
    use crate::zipwrite::write_zip;

    /// Build a tiny but valid .docx in memory.
    fn make_docx(document_xml: &str) -> Vec<u8> {
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let styles = r#"<?xml version="1.0"?><w:styles/>"#;
        write_zip(&[
            ("[Content_Types].xml".to_string(), ct.as_bytes().to_vec()),
            ("_rels/.rels".to_string(), rels.as_bytes().to_vec()),
            (
                "word/document.xml".to_string(),
                document_xml.as_bytes().to_vec(),
            ),
            ("word/styles.xml".to_string(), styles.as_bytes().to_vec()),
        ])
    }

    const BODY: &str = "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
        <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello</w:t></w:r></w:p>\
        <w:p><w:r><w:t>World</w:t></w:r></w:p>\
        <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr>\
        </w:body></w:document>";

    #[test]
    fn create_header_from_scratch_wires_everything() {
        use crate::model::{Block, Document, Paragraph};
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        let name = pkg.create_hf(true).expect("created header");
        assert_eq!(name, "word/header1.xml");
        assert!(pkg.part(&name).is_some(), "header part missing");
        let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned();
        assert!(
            ct.contains("/word/header1.xml") && ct.contains("header+xml"),
            "no content type: {ct}"
        );
        let rels =
            String::from_utf8_lossy(pkg.part("word/_rels/document.xml.rels").unwrap()).into_owned();
        assert!(
            rels.contains("Target=\"header1.xml\""),
            "no relationship: {rels}"
        );
        assert!(
            pkg.sect_pr().contains("headerReference"),
            "no sectPr ref: {}",
            pkg.sect_pr()
        );

        // Survives a save + reload, and the reference lands in the saved document.
        let bytes = save_package(&pkg);
        let re = load_package(&bytes).expect("reload");
        assert!(re.part("word/header1.xml").is_some());
        let doc_xml = String::from_utf8_lossy(re.part("word/document.xml").unwrap()).into_owned();
        assert!(
            doc_xml.contains("w:headerReference"),
            "ref not saved: {doc_xml}"
        );

        // A second create picks the next name and id.
        let mut pkg2 = pkg;
        let name2 = pkg2.create_hf(false).expect("created footer");
        assert_eq!(name2, "word/footer1.xml");
        assert!(pkg2.sect_pr().contains("footerReference"));
    }

    #[test]
    fn roundtrip_preserves_model_parts_and_sectpr() {
        let docx = make_docx(BODY);
        let pkg1 = load_package(&docx).expect("load");
        // model captured both paragraphs
        assert_eq!(pkg1.document.body.len(), 2);

        let saved = save_package(&pkg1);
        let pkg2 = load_package(&saved).expect("reload saved");

        // model is identical after a save round-trip
        assert_eq!(pkg1.document, pkg2.document);
        // all original parts are still present
        let mut names = pkg2.part_names();
        names.sort();
        assert!(names.contains(&"word/styles.xml"));
        assert!(names.contains(&"[Content_Types].xml"));
        assert!(names.contains(&"word/document.xml"));
        // sectPr (page size) survived into the saved document.xml
        let doc_xml = pkg2
            .part_names()
            .iter()
            .position(|n| *n == "word/document.xml")
            .map(|i| String::from_utf8_lossy(&pkg2.parts[i].1).into_owned())
            .unwrap();
        assert!(doc_xml.contains("<w:sectPr"));
        assert!(doc_xml.contains("w:w=\"11906\""));
    }

    #[test]
    fn edit_text_then_save_persists() {
        let docx = make_docx(BODY);
        let mut pkg = load_package(&docx).expect("load");

        // Edit: change the text of the first run of the first paragraph.
        if let Block::Paragraph(p) = &mut pkg.document.body[0] {
            if let Inline::Run(r) = &mut p.content[0] {
                r.text = "Goodbye".to_string();
            }
        }
        let saved = save_package(&pkg);
        let reloaded = load_package(&saved).expect("reload");
        assert_eq!(
            reloaded.document.plain_text().lines().next().unwrap(),
            "Goodbye"
        );
        // the second paragraph is untouched
        assert!(reloaded.document.plain_text().contains("World"));
    }

    #[test]
    fn rejects_non_docx() {
        assert_eq!(load_package(b"nope").unwrap_err(), LoadError::NotZip);
    }

    #[test]
    fn save_preserves_unmodeled_content() {
        let body = "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
            <w:p><w:bookmarkStart w:id=\"0\" w:name=\"bm\"/><w:r><w:t>hi</w:t></w:r><w:bookmarkEnd w:id=\"0\"/></w:p>\
            <w:p><w:r><w:drawing><inline>IMG</inline></w:drawing></w:r></w:p>\
            <w:sdt><w:sdtContent><w:p><w:r><w:t>ctrl</w:t></w:r></w:p></w:sdtContent></w:sdt>\
            </w:body></w:document>";
        let docx = make_docx(body);
        let pkg1 = load_package(&docx).expect("load");

        // Bookmarks/drawings stay Raw; the block-level sdt is unwrapped so its
        // content (the "ctrl" paragraph) is visible.
        assert_eq!(pkg1.document.body.len(), 3);
        assert_eq!(pkg1.document.body[2].plain_text(), "ctrl");
        if let Block::Paragraph(p) = &pkg1.document.body[1] {
            assert!(matches!(p.content[0], Inline::Raw(_))); // the drawing run
        } else {
            panic!();
        }

        let saved = save_package(&pkg1);
        let text = String::from_utf8_lossy(&saved);
        assert!(text.contains("w:bookmarkStart"), "bookmark lost");
        assert!(text.contains("<w:drawing>"), "drawing lost");
        assert!(text.contains("ctrl"), "sdt content lost");

        // And a full round-trip is stable (the unwrapped content stays unwrapped).
        let pkg2 = load_package(&saved).expect("reload");
        assert_eq!(pkg1.document, pkg2.document);
    }

    #[test]
    fn new_package_saves_and_reloads() {
        use crate::model::{Inline, ParProps, Paragraph, Run, RunProps};
        let document = Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "Fresh document".to_string(),
                    props: RunProps::default(),
                })],
            })],
        };
        let pkg = new_package(document.clone());
        let bytes = save_package(&pkg);
        let reloaded = load_package(&bytes).expect("reload new doc");
        assert_eq!(reloaded.document, document);
        assert!(reloaded.part_names().contains(&"word/styles.xml"));
    }
}
