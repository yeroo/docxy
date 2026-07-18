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

/// Read the value of attribute `attr` (given as `name="`) on the first `elem`
/// element (given as its opening `<w:tag` prefix). Scoped to that one tag so an
/// attribute of a later element can't be picked up by mistake.
fn attr_in(hay: &str, elem: &str, attr: &str) -> Option<String> {
    let start = hay.find(elem)?;
    let tag = &hay[start..];
    let end = tag.find('>').unwrap_or(tag.len());
    let tag = &tag[..end];
    let a = tag.find(attr)? + attr.len();
    let rest = &tag[a..];
    let q = rest.find('"')?;
    Some(rest[..q].to_string())
}

/// Decode the handful of XML entities a watermark phrase might carry.
fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

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

    /// The document's protection state from `word/settings.xml`, as a short human
    /// label (`read-only`, `comments only`, `tracked changes only`, `form fields
    /// only`), or `None` when the document isn't protected. Surfaced so the reader
    /// knows Word would restrict editing; docxy doesn't itself enforce it.
    pub fn protection(&self) -> Option<String> {
        let xml = self
            .part("word/settings.xml")
            .and_then(|b| std::str::from_utf8(b).ok())?;
        // An enforced restriction (`w:documentProtection`). `w:edit` limits editing;
        // `w:formatting="1"` (which can appear alone) locks styles/formatting.
        if xml.contains("<w:documentProtection") {
            let enforced = attr_in(xml, "<w:documentProtection", "w:enforcement=\"")
                .map(|e| matches!(e.as_str(), "1" | "on" | "true"))
                .unwrap_or(true);
            if enforced {
                let edit = attr_in(xml, "<w:documentProtection", "w:edit=\"");
                let label = match edit.as_deref() {
                    Some("readOnly") => Some("read-only"),
                    Some("comments") => Some("comments only"),
                    Some("trackedChanges") => Some("tracked changes only"),
                    Some("forms") => Some("form fields only"),
                    _ if attr_in(xml, "<w:documentProtection", "w:formatting=\"").as_deref()
                        == Some("1") =>
                    {
                        Some("formatting locked")
                    }
                    _ => None,
                };
                if let Some(l) = label {
                    return Some(l.to_string());
                }
            }
        }
        // A "recommend read-only on open" flag (`w:writeProtection`).
        if xml.contains("<w:writeProtection") {
            return Some("read-only (recommended)".to_string());
        }
        None
    }

    /// The watermark text, if a header carries a VML WordArt watermark (Word's
    /// text watermarks store the phrase in `<v:textpath string="…">`). Picture
    /// watermarks return `None` (no text). Surfaced by docxy as an indicator.
    pub fn watermark(&self) -> Option<String> {
        for (name, bytes) in &self.parts {
            if !name.contains("/header") {
                continue;
            }
            let Ok(xml) = std::str::from_utf8(bytes) else {
                continue;
            };
            // A header can hold several <v:textpath> (the shapetype's template one
            // has no `string`); return the first that carries the watermark text.
            let mut rest = xml;
            while let Some(i) = rest.find("<v:textpath") {
                rest = &rest[i..];
                let end = rest.find('>').unwrap_or(rest.len());
                if let Some(s) = attr_in(&rest[..end], "<v:textpath", "string=\"") {
                    let t = decode_xml_entities(&s);
                    if !t.trim().is_empty() {
                        return Some(t);
                    }
                }
                rest = &rest[end..];
            }
        }
        None
    }

    /// Whether the document defines page borders (`w:pgBorders` in any section).
    /// Surfaced as an indicator; a terminal doesn't draw the page frame itself.
    pub fn has_page_borders(&self) -> bool {
        self.sect_pr.contains("<w:pgBorders")
            || self.document.body.iter().any(|b| {
                matches!(b, crate::model::Block::Paragraph(p)
                    if p.props.section_break.as_deref().is_some_and(|s| s.contains("<w:pgBorders")))
            })
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

    /// Add a `<w:comment>` to `comments.xml`, creating the part + relationship +
    /// content-type if absent. `text` is the comment body (XML-escaped here).
    pub fn add_comment(&mut self, id: i32, author: &str, initials: &str, date: &str, text: &str) {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let esc = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let comment = format!(
            "<w:comment w:id=\"{id}\" w:author=\"{}\" w:initials=\"{}\" w:date=\"{}\">\
             <w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:comment>",
            esc(author),
            esc(initials),
            esc(date),
            esc(text),
        );
        let name = "word/comments.xml";
        if let Some(b) = self.part(name) {
            let xml = String::from_utf8_lossy(b).into_owned();
            self.set_part(
                name,
                xml.replacen("</w:comments>", &format!("{comment}</w:comments>"), 1)
                    .into_bytes(),
            );
            return;
        }
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:comments xmlns:w=\"{W_NS}\">{comment}</w:comments>"
        );
        self.parts.push((name.to_string(), body.into_bytes()));
        if let Some(b) = self.part("[Content_Types].xml") {
            let ct = String::from_utf8_lossy(b).into_owned();
            if !ct.contains("comments+xml") {
                let ov = "<Override PartName=\"/word/comments.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml\"/>";
                self.set_part(
                    "[Content_Types].xml",
                    ct.replacen("</Types>", &format!("{ov}</Types>"), 1)
                        .into_bytes(),
                );
            }
        }
        let rels_name = "word/_rels/document.xml.rels";
        if let Some(b) = self.part(rels_name) {
            let rels = String::from_utf8_lossy(b).into_owned();
            if !rels.contains("comments.xml") {
                let rid = next_rid(&rels);
                let rel = format!(
                    "<Relationship Id=\"{rid}\" Type=\"{R_NS}/comments\" Target=\"comments.xml\"/>"
                );
                self.set_part(
                    rels_name,
                    rels.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
                        .into_bytes(),
                );
            }
        }
    }

    /// Remove the `<w:comment w:id="id">…</w:comment>` from `comments.xml`.
    pub fn remove_comment(&mut self, id: i32) {
        let name = "word/comments.xml";
        let Some(b) = self.part(name) else {
            return;
        };
        let xml = String::from_utf8_lossy(b).into_owned();
        let open = format!("<w:comment w:id=\"{id}\"");
        if let Some(start) = xml.find(&open) {
            if let Some(rel_end) = xml[start..].find("</w:comment>") {
                let end = start + rel_end + "</w:comment>".len();
                let mut out = xml.clone();
                out.replace_range(start..end, "");
                self.set_part(name, out.into_bytes());
            }
        }
    }

    /// Ensure `numbering.xml` defines a simple bullet (or decimal) list and return
    /// its `numId`, creating the part + relationship + content-type if absent. Used
    /// by the Bullets/Numbering ribbon commands so applied lists render and save.
    pub fn ensure_list(&mut self, bullet: bool) -> i32 {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        // Reserved high ids, unlikely to collide with a document's own lists.
        let (num_id, abs_id, fmt, text) = if bullet {
            (9990, 9990, "bullet", "•")
        } else {
            (9991, 9991, "decimal", "%1.")
        };
        let abstract_xml = format!(
            "<w:abstractNum w:abstractNumId=\"{abs_id}\"><w:lvl w:ilvl=\"0\">\
             <w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/>\
             <w:lvlJc w:val=\"left\"/></w:lvl></w:abstractNum>"
        );
        let num_xml =
            format!("<w:num w:numId=\"{num_id}\"><w:abstractNumId w:val=\"{abs_id}\"/></w:num>");
        let name = "word/numbering.xml";
        let marker = format!("w:numId=\"{num_id}\"");

        if let Some(b) = self.part(name) {
            let xml = String::from_utf8_lossy(b).into_owned();
            if xml.contains(&marker) {
                return num_id; // already defined
            }
            // abstractNum first (after the opening tag), num last (before the close).
            let xml = match xml.find("<w:numbering").and_then(|s| xml[s..].find('>')) {
                Some(_) => {
                    let open_end = xml.find("<w:numbering").unwrap();
                    let gt = xml[open_end..].find('>').unwrap() + open_end + 1;
                    format!("{}{abstract_xml}{}", &xml[..gt], &xml[gt..])
                }
                None => xml,
            };
            let xml = xml.replacen("</w:numbering>", &format!("{num_xml}</w:numbering>"), 1);
            self.set_part(name, xml.into_bytes());
            return num_id;
        }

        // Create numbering.xml from scratch + wire content-type and relationship.
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:numbering xmlns:w=\"{W_NS}\">{abstract_xml}{num_xml}</w:numbering>"
        );
        self.parts.push((name.to_string(), body.into_bytes()));
        if let Some(b) = self.part("[Content_Types].xml") {
            let ct = String::from_utf8_lossy(b).into_owned();
            if !ct.contains("numbering+xml") {
                let ov = "<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>";
                self.set_part(
                    "[Content_Types].xml",
                    ct.replacen("</Types>", &format!("{ov}</Types>"), 1)
                        .into_bytes(),
                );
            }
        }
        let rels_name = "word/_rels/document.xml.rels";
        if let Some(b) = self.part(rels_name) {
            let rels = String::from_utf8_lossy(b).into_owned();
            if !rels.contains("numbering.xml") {
                let rid = next_rid(&rels);
                let rel = format!(
                    "<Relationship Id=\"{rid}\" Type=\"{R_NS}/numbering\" Target=\"numbering.xml\"/>"
                );
                self.set_part(
                    rels_name,
                    rels.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
                        .into_bytes(),
                );
            }
        }
        num_id
    }

    /// Ensure `styles.xml` defines each style id in `ids` that Markdown-sourced
    /// content might reference (`HeadingN` for `N` in `1..=6`, `Quote`,
    /// `SourceCode`, `Code` — the exact set [`markdown_styles_xml`] defines for
    /// a fresh markdown package; any other id is silently ignored). Strictly
    /// additive, mirroring [`Package::ensure_list`]'s idiom: a style id already
    /// defined in the package — e.g. a third-party document's own `Heading1` —
    /// is left byte-untouched; only ids genuinely ABSENT from `styles.xml` get
    /// a definition appended. Creates the part from scratch in the (practically
    /// unreachable, since `new_package`/`load_package` always carry one)
    /// case a package has no `styles.xml` at all.
    ///
    /// Without this, a `<w:pStyle w:val="HeadingN"/>` (or `Quote`/`SourceCode`)
    /// referencing a style the target package never defined renders as plain
    /// Normal text in Word — the same problem [`markdown_styles_xml`]'s doc
    /// comment describes for a *fresh* markdown package, here fixed for
    /// splicing into an *existing* one.
    pub fn ensure_styles(&mut self, ids: &[&str]) {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let name = "word/styles.xml";
        let existing = self
            .part(name)
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let mut additions = String::new();
        for &id in ids {
            let marker = format!("w:styleId=\"{id}\"");
            let already_present = existing.as_deref().is_some_and(|xml| xml.contains(&marker))
                || additions.contains(&marker);
            if already_present {
                continue;
            }
            if let Some(def) = markdown_style_def(id) {
                additions.push_str(&def);
            }
        }
        if additions.is_empty() {
            return; // every requested id was already defined (or unknown)
        }
        match existing {
            Some(xml) => {
                let xml = xml.replacen("</w:styles>", &format!("{additions}</w:styles>"), 1);
                self.set_part(name, xml.into_bytes());
            }
            None => {
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                     <w:styles xmlns:w=\"{W_NS}\">{additions}</w:styles>"
                );
                self.parts.push((name.to_string(), body.into_bytes()));
            }
        }
    }
}

/// The `<w:style>` XML definition for one of the styles Markdown maps onto
/// (`HeadingN` for `N` in `1..=6`, `Quote`, `SourceCode`, `Code`), or `None`
/// for any other id. Shared by [`markdown_styles_xml`] (which defines the full
/// set for a fresh markdown package) and [`Package::ensure_styles`] (which
/// defines only the ids actually referenced, for an existing package), so the
/// two can never drift apart.
fn markdown_style_def(id: &str) -> Option<String> {
    if let Some(n) = id
        .strip_prefix("Heading")
        .and_then(|s| s.parse::<usize>().ok())
    {
        if !(1..=6).contains(&n) {
            return None;
        }
        // Heading sizes in half-points (H1..H6), decreasing.
        let sizes = [36u32, 32, 28, 26, 24, 22];
        let sz = sizes[n - 1];
        let idx = n - 1;
        return Some(format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{n}\">\
             <w:name w:val=\"heading {n}\"/><w:basedOn w:val=\"Normal\"/>\
             <w:next w:val=\"Normal\"/>\
             <w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"60\"/>\
             <w:outlineLvl w:val=\"{idx}\"/></w:pPr>\
             <w:rPr><w:b/><w:sz w:val=\"{sz}\"/></w:rPr></w:style>"
        ));
    }
    match id {
        "Quote" => Some(
            "<w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/>\
             <w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/>\
             <w:pPr><w:ind w:left=\"720\"/></w:pPr><w:rPr><w:i/></w:rPr></w:style>"
                .to_string(),
        ),
        "SourceCode" => Some(
            "<w:style w:type=\"paragraph\" w:styleId=\"SourceCode\">\
             <w:name w:val=\"Source Code\"/><w:basedOn w:val=\"Normal\"/>\
             <w:next w:val=\"Normal\"/>\
             <w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/></w:rPr></w:style>"
                .to_string(),
        ),
        "Code" => Some(
            "<w:style w:type=\"character\" w:styleId=\"Code\"><w:name w:val=\"Code\"/>\
             <w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/></w:rPr></w:style>"
                .to_string(),
        ),
        _ => None,
    }
}

/// The next free relationship id (`rId{max+1}`) for a `.rels` part.
fn next_rid(rels: &str) -> String {
    format!("rId{}", next_rid_num(rels))
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
        crate::load::set_chart_data(&mut rels, crate::load::collect_chart_data(xml, read_part));
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

/// Build a package for a Markdown-backed document: like [`new_package`] but with
/// a `word/numbering.xml` that defines `numId` 1 (bullets) and `numId` 2 (decimal)
/// across nine levels — the two ids [`crate::markdown::from_markdown`] emits. This
/// makes Markdown list paragraphs render real markers in the TUI and survive a
/// save to `.docx` (Word picks up the numbering part too).
pub fn new_markdown_package(document: Document) -> Package {
    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let mut pkg = new_package(document);

    let mut bullets = String::new();
    let mut decimals = String::new();
    for lvl in 0..9 {
        bullets.push_str(&format!(
            "<w:lvl w:ilvl=\"{lvl}\"><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"•\"/></w:lvl>"
        ));
        decimals.push_str(&format!(
            "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%{}.\"/></w:lvl>",
            lvl + 1
        ));
    }
    let numbering = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:numbering xmlns:w=\"{W}\">\
         <w:abstractNum w:abstractNumId=\"100\">{bullets}</w:abstractNum>\
         <w:abstractNum w:abstractNumId=\"101\">{decimals}</w:abstractNum>\
         <w:num w:numId=\"1\"><w:abstractNumId w:val=\"100\"/></w:num>\
         <w:num w:numId=\"2\"><w:abstractNumId w:val=\"101\"/></w:num>\
         </w:numbering>"
    );
    pkg.parts
        .push(("word/numbering.xml".to_string(), numbering.into_bytes()));

    // Content-type override for the new part.
    if let Some(b) = pkg.part("[Content_Types].xml") {
        let ct = String::from_utf8_lossy(b).into_owned();
        let ov = "<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>";
        pkg.set_part(
            "[Content_Types].xml",
            ct.replacen("</Types>", &format!("{ov}</Types>"), 1)
                .into_bytes(),
        );
    }
    // Relationship from document.xml to the numbering part.
    let rels_name = "word/_rels/document.xml.rels";
    if let Some(b) = pkg.part(rels_name) {
        let rels = String::from_utf8_lossy(b).into_owned();
        let rid = next_rid(&rels);
        let rel = format!(
            "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>"
        );
        pkg.set_part(
            rels_name,
            rels.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
                .into_bytes(),
        );
    }

    // Define the styles that Markdown maps onto, so Word (and our renderer)
    // actually format them. A `<w:pStyle w:val="Heading1"/>` with no matching
    // definition in styles.xml renders as plain Normal text — that is why a
    // `# heading` looked unstyled in Word.
    pkg.set_part("word/styles.xml", markdown_styles_xml().into_bytes());
    pkg
}

/// A `styles.xml` defining the built-in styles Markdown uses: Normal, Title,
/// Heading1–6, Quote, the SourceCode paragraph style, and the Code character
/// style. Word recognizes the headings by their `styleId`/`name`.
fn markdown_styles_xml() -> String {
    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    // Heads/Quote/SourceCode/Code definitions come from `markdown_style_def`,
    // the single source of truth also used by `Package::ensure_styles` — so a
    // fresh markdown package and a splice into an existing one can never
    // define these styles differently.
    let mut heads = String::new();
    for n in 1..=6 {
        heads.push_str(&markdown_style_def(&format!("Heading{n}")).unwrap());
    }
    let quote = markdown_style_def("Quote").unwrap();
    let source_code = markdown_style_def("SourceCode").unwrap();
    let code = markdown_style_def("Code").unwrap();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:styles xmlns:w=\"{W}\">\
         <w:docDefaults><w:rPrDefault><w:rPr>\
         <w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\"/><w:sz w:val=\"22\"/>\
         </w:rPr></w:rPrDefault></w:docDefaults>\
         <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\">\
         <w:name w:val=\"Normal\"/></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/>\
         <w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/>\
         <w:rPr><w:b/><w:sz w:val=\"56\"/></w:rPr></w:style>\
         {heads}\
         {quote}\
         {source_code}\
         {code}\
         </w:styles>"
    )
}

/// Serialize the package back to `.docx` bytes (STORED ZIP).
pub fn save_package(pkg: &Package) -> Vec<u8> {
    // External hyperlinks need a relationship (`r:id` → `.rels` Target) or their
    // URL is lost. Links we modelled from a loaded `.docx` already carry `rel_id`;
    // links created in-app or from Markdown have a `target` but no `rel_id`. Mint
    // a relationship for each before serializing so the URL survives the save.
    let mut document = pkg.document.clone();
    let mut parts = pkg.parts.clone();
    let rels_name = "word/_rels/document.xml.rels";
    if let Some((_, rels_bytes)) = parts.iter().find(|(n, _)| n == rels_name) {
        let mut new_rels = String::new();
        let mut next = next_rid_num(&String::from_utf8_lossy(rels_bytes));
        let mut links = Vec::new();
        collect_unlinked_externals(&mut document.body, &mut links);
        for h in links {
            let rid = format!("rId{next}");
            next += 1;
            let target = h.target.as_deref().unwrap_or_default();
            new_rels.push_str(&format!(
                "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\" TargetMode=\"External\"/>",
                esc_xml_attr(target)
            ));
            h.rel_id = Some(rid);
        }
        if !new_rels.is_empty() {
            let rels = String::from_utf8_lossy(rels_bytes).into_owned();
            let updated = rels.replacen(
                "</Relationships>",
                &format!("{new_rels}</Relationships>"),
                1,
            );
            if let Some(p) = parts.iter_mut().find(|(n, _)| n == rels_name) {
                p.1 = updated.into_bytes();
            }
        }
    }

    // The original `<w:document …>` element declares every namespace the file
    // uses (w14, mc, v, o, wp, …). Preserved raw property slices may reference
    // those prefixes, so re-emit the original declarations rather than our
    // minimal three — otherwise Word rejects the file with "unbound prefix".
    let original_doc = String::from_utf8_lossy(&parts[pkg.doc_index].1).into_owned();
    let mut xml = document_to_xml(&document);
    if let (Some(attrs), Some(doc_pos), Some(body_pos)) = (
        document_root_attrs(&original_doc),
        xml.find("<w:document"),
        xml.find("<w:body>"),
    ) {
        xml = format!(
            "{}<w:document {attrs}>{}",
            &xml[..doc_pos],
            &xml[body_pos..]
        );
    }
    if !pkg.sect_pr.is_empty() {
        xml = xml.replacen("</w:body>", &format!("{}</w:body>", pkg.sect_pr), 1);
    }
    parts[pkg.doc_index].1 = xml.into_bytes();
    write_zip(&parts)
}

/// The attributes of the original `<w:document …>` element — all the `xmlns:*`
/// declarations Word wrote — so the regenerated body's preserved raw slices stay
/// namespace-bound. Ensures the `w`/`r`/`m` prefixes our serializer emits are
/// present even if the original omitted them (e.g. a freshly created document's
/// minimal template root).
fn document_root_attrs(original: &str) -> Option<String> {
    let start = original.find("<w:document")?;
    let rest = &original[start + "<w:document".len()..];
    let end = rest.find('>')?;
    let mut attrs = rest[..end].trim().trim_end_matches('/').trim().to_string();
    for (prefix, uri) in [
        (
            "xmlns:w",
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
        ),
        (
            "xmlns:r",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        ),
        (
            "xmlns:m",
            "http://schemas.openxmlformats.org/officeDocument/2006/math",
        ),
    ] {
        if !attrs.contains(prefix) {
            if !attrs.is_empty() {
                attrs.push(' ');
            }
            attrs.push_str(&format!("{prefix}=\"{uri}\""));
        }
    }
    Some(attrs)
}

/// Highest `rIdN` number in a `.rels` string, plus one (the next free id).
fn next_rid_num(rels: &str) -> u32 {
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
    max + 1
}

/// Collect `&mut` references to every external hyperlink (`target` set) that has
/// no relationship id yet, walking paragraphs and table cells recursively.
fn collect_unlinked_externals<'a>(
    blocks: &'a mut [crate::model::Block],
    out: &mut Vec<&'a mut crate::model::Hyperlink>,
) {
    use crate::model::{Block, Inline};
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                for inl in &mut p.content {
                    if let Inline::Hyperlink(h) = inl {
                        if h.target.is_some() && h.rel_id.is_none() {
                            out.push(h);
                        }
                    }
                }
            }
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        collect_unlinked_externals(&mut cell.blocks, out);
                    }
                }
            }
            Block::Raw(_) => {}
        }
    }
}

/// Minimal XML attribute escaping for relationship targets (URLs).
fn esc_xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Capture the last body-level `w:sectPr` element verbatim, if any.
///
/// A depth-tracking scan, not `rfind`, because a section edited with tracked
/// changes nests the *previous* properties in `<w:sectPrChange><w:sectPr>…` — a
/// naive last-match would capture that stale inner element and drop the current
/// page setup on save. Only the outermost (depth-0) `<w:sectPr>` is a real body
/// section; `<w:sectPrChange>` is skipped (it isn't a `<w:sectPr>` element).
fn extract_sectpr(xml: &str) -> String {
    const OPEN: &str = "<w:sectPr";
    const CLOSE: &str = "</w:sectPr>";
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0usize;
    while i < xml.len() {
        let rest = &xml[i..];
        if rest.starts_with(CLOSE) {
            if depth > 0 {
                depth -= 1;
                if depth == 0 {
                    best = Some((start, i + CLOSE.len()));
                }
            }
            i += CLOSE.len();
        } else if rest.starts_with(OPEN)
            // Distinguish `<w:sectPr` from `<w:sectPrChange`.
            && matches!(
                rest[OPEN.len()..].chars().next(),
                Some('>' | ' ' | '/' | '\t' | '\r' | '\n')
            )
        {
            let gt = match rest.find('>') {
                Some(g) => g,
                None => break, // malformed
            };
            if rest[..gt + 1].ends_with("/>") {
                // Self-closing empty section (rare) at body level.
                if depth == 0 {
                    best = Some((i, i + gt + 1));
                }
            } else {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            i += gt + 1;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    best.map_or_else(String::new, |(s, e)| xml[s..e].to_string())
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
    fn surfaces_protection_watermark_and_page_borders() {
        let document = "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
            <w:p><w:r><w:t>Body</w:t></w:r></w:p>\
            <w:sectPr><w:pgBorders w:offsetFrom=\"page\"><w:top w:val=\"single\"/></w:pgBorders>\
            <w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:body></w:document>";
        let settings = "<?xml version=\"1.0\"?><w:settings xmlns:w=\"x\">\
            <w:documentProtection w:edit=\"readOnly\" w:enforcement=\"1\"/></w:settings>";
        let header = "<?xml version=\"1.0\"?><w:hdr xmlns:w=\"x\" xmlns:v=\"y\"><w:p><w:r><w:pict>\
            <v:shape id=\"PowerPlusWaterMarkObject\"><v:textpath string=\"CONFIDENTIAL &amp; DRAFT\"/>\
            </v:shape></w:pict></w:r></w:p></w:hdr>";
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let bytes = write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), rels.into()),
            ("word/document.xml".into(), document.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("word/settings.xml".into(), settings.into()),
            ("word/header1.xml".into(), header.into()),
        ]);
        let pkg = load_package(&bytes).expect("load");
        assert_eq!(pkg.protection().as_deref(), Some("read-only"));
        assert_eq!(pkg.watermark().as_deref(), Some("CONFIDENTIAL & DRAFT"));
        assert!(pkg.has_page_borders());

        // An unprotected, plain doc surfaces nothing.
        let plain = load_package(&make_docx(BODY)).expect("load");
        assert!(plain.protection().is_none());
        assert!(plain.watermark().is_none());
        assert!(!plain.has_page_borders());
    }

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
    fn markdown_package_defines_heading_styles() {
        use crate::markdown::from_markdown;
        // The styles a `# heading` references must be defined, or Word renders it
        // as plain Normal text.
        let pkg = new_markdown_package(from_markdown("# Title\n\n## Sub"));
        let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(styles.contains("w:styleId=\"Heading1\""), "{styles}");
        assert!(styles.contains("w:val=\"heading 1\""), "{styles}");
        assert!(styles.contains("w:styleId=\"Heading2\""), "{styles}");
        // And the document still references the style.
        let doc_xml = save_package(&pkg);
        let re = load_package(&doc_xml).expect("reload");
        let dx = String::from_utf8_lossy(re.part("word/document.xml").unwrap()).into_owned();
        assert!(dx.contains("w:pStyle w:val=\"Heading1\""), "{dx}");
    }

    #[test]
    fn ensure_styles_adds_absent_ids_and_leaves_existing_ones_byte_untouched() {
        let mut pkg = new_package(Document { body: vec![] });
        // A third-party Heading1, visibly different from ours (custom name +
        // color), already defined.
        let custom_styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="ThirdPartyHeading"/><w:rPr><w:color w:val="FF0000"/></w:rPr></w:style></w:styles>"#;
        pkg.set_part("word/styles.xml", custom_styles.as_bytes().to_vec());

        pkg.ensure_styles(&["Heading1", "Quote"]);

        let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        // The third-party Heading1 definition is byte-for-byte untouched —
        // not merged, not replaced, not duplicated.
        assert!(
            styles.contains(
                r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="ThirdPartyHeading"/><w:rPr><w:color w:val="FF0000"/></w:rPr></w:style>"#
            ),
            "{styles}"
        );
        assert_eq!(
            styles.matches("w:styleId=\"Heading1\"").count(),
            1,
            "ensure_styles must not append a second, competing Heading1: {styles}"
        );
        // Quote, genuinely absent, was appended.
        assert!(styles.contains("w:styleId=\"Quote\""), "{styles}");
    }

    #[test]
    fn ensure_styles_is_idempotent_and_ignores_unknown_ids() {
        let mut pkg = new_package(Document { body: vec![] });
        pkg.ensure_styles(&["SourceCode", "NotARealStyle"]);
        pkg.ensure_styles(&["SourceCode"]); // second call: already defined
        let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        assert_eq!(
            styles.matches("w:styleId=\"SourceCode\"").count(),
            1,
            "a repeat call must not duplicate the definition: {styles}"
        );
        assert!(
            !styles.contains("NotARealStyle"),
            "an id outside the Markdown-mapped set must be silently ignored: {styles}"
        );
    }

    #[test]
    fn markdown_styles_survive_docx_round_trip() {
        use crate::markdown::{from_markdown, to_markdown};
        // Inline code, blockquote, and a fenced code block.
        let src = "para with `code`\n\n> a quote\n\n```\nline one\nline two\n```";
        let pkg = new_markdown_package(from_markdown(src));
        let reloaded = load_package(&save_package(&pkg)).expect("reload");
        let md = to_markdown(&reloaded.document);
        assert!(md.contains("`code`"), "inline code lost: {md}");
        assert!(md.contains("> a quote"), "blockquote lost: {md}");
        assert!(
            md.contains("```\nline one\nline two\n```"),
            "fenced code lost: {md}"
        );
    }

    #[test]
    fn save_mints_relationship_for_target_only_hyperlink() {
        use crate::model::{Document, Hyperlink, Paragraph, Run};
        // A link created in-app / from Markdown: a target but no rel_id.
        let link = Inline::Hyperlink(Hyperlink {
            target: Some("https://example.com/a?x=1&y=2".to_string()),
            anchor: None,
            rel_id: None,
            runs: vec![Run {
                text: "docs".to_string(),
                ..Run::default()
            }],
        });
        let pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![link],
                ..Paragraph::default()
            })],
        });
        let saved = save_package(&pkg);
        let re = load_package(&saved).expect("reload");
        // The URL survived: load resolves r:id back to the external target,
        // including the escaped `&`.
        let h = re
            .document
            .body
            .iter()
            .find_map(|b| match b {
                Block::Paragraph(p) => p.content.iter().find_map(|i| match i {
                    Inline::Hyperlink(h) => Some(h),
                    _ => None,
                }),
                _ => None,
            })
            .expect("a hyperlink");
        assert_eq!(h.target.as_deref(), Some("https://example.com/a?x=1&y=2"));
        assert!(h.rel_id.is_some(), "should have been assigned a rel id");
    }

    #[test]
    fn extract_sectpr_ignores_tracked_change_revision() {
        // A plain trailing sectPr is captured whole.
        let plain = "<w:body><w:p/><w:sectPr><w:pgSz w:w=\"11906\"/></w:sectPr></w:body>";
        assert_eq!(
            extract_sectpr(plain),
            "<w:sectPr><w:pgSz w:w=\"11906\"/></w:sectPr>"
        );
        // With a <w:sectPrChange> revision nesting the OLD props, the current
        // (outer) sectPr must be captured — not the stale inner one.
        let revised = "<w:body><w:p/><w:sectPr><w:pgSz w:w=\"16838\" w:h=\"11906\" w:orient=\"landscape\"/>\
            <w:sectPrChange w:id=\"1\"><w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:sectPrChange>\
            </w:sectPr></w:body>";
        let got = extract_sectpr(revised);
        assert!(got.contains("landscape"), "captured stale props: {got}");
        assert!(
            got.contains("<w:sectPrChange"),
            "outer sectPr truncated: {got}"
        );
        assert!(got.ends_with("</w:sectPr>"));
        // A self-closing empty section still works.
        assert_eq!(
            extract_sectpr("<w:body><w:sectPr/></w:body>"),
            "<w:sectPr/>"
        );
        // No section → empty.
        assert_eq!(extract_sectpr("<w:body><w:p/></w:body>"), "");
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

        // Bookmarks/drawings stay Raw. The block-level sdt keeps its content (the
        // "ctrl" paragraph) visible, now wrapped between two Raw wrapper
        // boundaries so the control survives the round-trip:
        // [bm para, drawing para, <w:sdt>…<w:sdtContent>, ctrl para, </…></w:sdt>].
        assert_eq!(pkg1.document.body.len(), 5);
        assert_eq!(pkg1.document.body[3].plain_text(), "ctrl");
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
        assert!(text.contains("<w:sdt>"), "content-control wrapper lost");
        assert!(text.contains("<w:sdtContent>"), "sdtContent wrapper lost");

        // And a full round-trip is stable (the preserved wrapper stays put).
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
        // The saved document root must declare w/r/m or Word rejects it as
        // "unbound prefix" — even for a freshly created doc whose template root
        // is minimal (regression guard for document_root_attrs).
        let doc_xml = String::from_utf8_lossy(reloaded.part("word/document.xml").unwrap());
        for ns in ["xmlns:w=", "xmlns:r=", "xmlns:m="] {
            assert!(doc_xml.contains(ns), "new-doc root missing {ns}");
        }
    }
}
