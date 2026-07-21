//! Parse `word/footnotes.xml` / `word/endnotes.xml` into a display list of
//! [`Note`]s, matching the note references in the body
//! ([`crate::model::Inline::FootnoteRef`]).
//!
//! Read-only and display-oriented: it extracts each note's id and body text.
//! The two reserved notes Word always writes — the separator (`id="-1"`) and
//! continuation separator (`id="0"`) — are skipped.

use crate::package::Package;
use crate::xml::{Event, XmlParser};

/// One footnote or endnote, flattened for display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    /// Note id (also the display number for normal documents).
    pub id: i32,
    pub endnote: bool,
    /// The note body as plain text (paragraphs joined by newlines).
    pub text: String,
}

/// Parse every footnote and endnote in `pkg` (footnotes first, then endnotes),
/// each in file order. The reserved separator notes are skipped.
pub fn parse_notes(pkg: &Package) -> Vec<Note> {
    let mut out = Vec::new();
    for (part, endnote, elem) in [
        ("word/footnotes.xml", false, "w:footnote"),
        ("word/endnotes.xml", true, "w:endnote"),
    ] {
        if let Some(xml) = pkg.part(part).and_then(|b| std::str::from_utf8(b).ok()) {
            out.extend(parse_notes_xml(xml, endnote, elem));
        }
    }
    out
}

/// Parse the `<w:footnote>` / `<w:endnote>` entries of a notes part.
pub fn parse_notes_xml(xml: &str, endnote: bool, elem: &str) -> Vec<Note> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == elem => {
                let ty = p.attr("w:type").to_string();
                let id: i32 = p.attr("w:id").parse().unwrap_or(i32::MIN);
                let text = collect_text(&mut p, elem);
                // Skip the reserved separator / continuation-separator notes.
                if ty != "separator" && ty != "continuationSeparator" && id > 0 {
                    out.push(Note { id, endnote, text });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Consume the body of the just-started note element, returning its plain text
/// with paragraph breaks as newlines. Stops at the matching end tag.
fn collect_text(p: &mut XmlParser, elem: &str) -> String {
    let mut s = String::new();
    let mut depth = 1; // inside the note element
    let mut in_t = false;
    loop {
        match p.next() {
            Event::Start => {
                match p.name() {
                    "w:t" | "w:delText" => in_t = true,
                    "w:tab" => s.push('\t'),
                    "w:br" | "w:cr" => s.push('\n'),
                    _ => {}
                }
                depth += 1;
            }
            Event::Text => {
                if in_t {
                    XmlParser::append_decoded(p.text(), &mut s);
                }
            }
            Event::End => {
                match p.name() {
                    "w:t" | "w:delText" => in_t = false,
                    // a paragraph closing inside the note → a line break
                    "w:p" if depth >= 2 => s.push('\n'),
                    _ => {}
                }
                depth -= 1;
                if depth == 0 {
                    let _ = elem;
                    break;
                }
            }
            Event::Eof => break,
        }
    }
    s.trim_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notes_and_skips_separators() {
        let xml = "<w:footnotes>\
            <w:footnote w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>\
            <w:footnote w:type=\"continuationSeparator\" w:id=\"0\"><w:p/></w:footnote>\
            <w:footnote w:id=\"1\"><w:p><w:r><w:footnoteRef/></w:r>\
              <w:r><w:t xml:space=\"preserve\">First note.</w:t></w:r></w:p></w:footnote>\
            <w:footnote w:id=\"2\"><w:p><w:r><w:t>Second </w:t></w:r>\
              <w:r><w:t>note.</w:t></w:r></w:p></w:footnote>\
            </w:footnotes>";
        let notes = parse_notes_xml(xml, false, "w:footnote");
        assert_eq!(notes.len(), 2, "separators should be skipped");
        assert_eq!(notes[0].id, 1);
        assert_eq!(notes[0].text, "First note.");
        assert_eq!(notes[1].id, 2);
        assert_eq!(notes[1].text, "Second note.");
        assert!(!notes[0].endnote);
    }

    /// Full [`parse_notes`] pipeline (footnotes part, then endnotes part)
    /// against a real [`Package`], the shape `docxy`'s `doc.notes` control
    /// verb marshals directly into JSON.
    #[test]
    fn parse_notes_from_package_combines_footnotes_then_endnotes() {
        use crate::package::load_package;
        use crate::zipwrite::write_zip;

        let footnotes = "<w:footnotes xmlns:w=\"x\">\
            <w:footnote w:id=\"1\"><w:p><w:r><w:t>a foot note</w:t></w:r></w:p></w:footnote>\
            </w:footnotes>";
        let endnotes = "<w:endnotes xmlns:w=\"x\">\
            <w:endnote w:id=\"1\"><w:p><w:r><w:t>an end note</w:t></w:r></w:p></w:endnote>\
            </w:endnotes>";
        let document_xml =
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body><w:p/></w:body></w:document>";
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let bytes = write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), rels.into()),
            ("word/document.xml".into(), document_xml.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("word/footnotes.xml".into(), footnotes.into()),
            ("word/endnotes.xml".into(), endnotes.into()),
        ]);
        let pkg = load_package(&bytes).expect("load");
        let ns = parse_notes(&pkg);
        assert_eq!(ns.len(), 2);
        assert_eq!(ns[0].id, 1);
        assert_eq!(ns[0].text, "a foot note");
        assert!(!ns[0].endnote);
        assert_eq!(ns[1].id, 1);
        assert_eq!(ns[1].text, "an end note");
        assert!(ns[1].endnote);
    }
}
