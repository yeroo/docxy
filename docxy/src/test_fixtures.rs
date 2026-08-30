//! Reusable DOCX package fixtures for end-to-end protection and watermark tests.
//!
//! The fixtures are assembled as complete OPC containers instead of injecting
//! metadata into `App` fields. This makes every consuming test exercise the ZIP
//! reader, OOXML parsers, package preservation, and the UI/control policy wiring.

use docxcore::package::{Package, load_package};
use docxcore::zipwrite::write_zip;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PR: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtectionFixture {
    Unrestricted,
    ReadOnly,
    Comments,
    Forms,
    TrackedChanges,
    FormattingOnly,
    Advisory,
    PasswordWrite,
    Unknown,
}

impl ProtectionFixture {
    pub(crate) const ALL: [Self; 9] = [
        Self::Unrestricted,
        Self::ReadOnly,
        Self::Comments,
        Self::Forms,
        Self::TrackedChanges,
        Self::FormattingOnly,
        Self::Advisory,
        Self::PasswordWrite,
        Self::Unknown,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Unrestricted => "unrestricted",
            Self::ReadOnly => "read-only",
            Self::Comments => "comments",
            Self::Forms => "forms",
            Self::TrackedChanges => "tracked-changes",
            Self::FormattingOnly => "formatting-only",
            Self::Advisory => "advisory-write-protection",
            Self::PasswordWrite => "password-write-protection",
            Self::Unknown => "unknown-protection-mode",
        }
    }

    pub(crate) fn package(self) -> Package {
        load_package(&self.bytes()).unwrap_or_else(|error| {
            panic!("{} protection fixture did not load: {error:?}", self.name())
        })
    }

    pub(crate) fn bytes(self) -> Vec<u8> {
        let restriction = match self {
            Self::Unrestricted => r#"<w:documentProtection w:edit="none" w:enforcement="1"/>"#,
            Self::ReadOnly => r#"<w:documentProtection w:edit="readOnly" w:enforcement="1"/>"#,
            Self::Comments => r#"<w:documentProtection w:edit="comments" w:enforcement="1"/>"#,
            Self::Forms => r#"<w:documentProtection w:edit="forms" w:enforcement="1"/>"#,
            Self::TrackedChanges => {
                r#"<w:documentProtection w:edit="trackedChanges" w:enforcement="1"/>"#
            }
            Self::FormattingOnly => {
                r#"<w:documentProtection w:edit="none" w:formatting="1" w:enforcement="1"/>"#
            }
            Self::Advisory => r#"<w:writeProtection w:recommended="true"/>"#,
            Self::PasswordWrite => r#"<w:writeProtection w:hashValue="YWJjZA=="/>"#,
            Self::Unknown => {
                r#"<w:documentProtection w:edit="producerSpecific" w:enforcement="1"/>"#
            }
        };
        let settings = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="{W}">{restriction}</w:settings>"#
        );
        package_bytes(
            self.name(),
            &single_section_document(None),
            &settings,
            &empty_document_relationships(),
            &[],
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatermarkFixture {
    Text,
    Picture,
    InheritedText,
}

impl WatermarkFixture {
    pub(crate) const ALL: [Self; 3] = [Self::Text, Self::Picture, Self::InheritedText];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Text => "text-watermark",
            Self::Picture => "picture-watermark",
            Self::InheritedText => "inherited-section-watermark",
        }
    }

    pub(crate) fn package(self) -> Package {
        load_package(&self.bytes())
            .unwrap_or_else(|error| panic!("{} fixture did not load: {error:?}", self.name()))
    }

    pub(crate) fn bytes(self) -> Vec<u8> {
        let settings = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="{W}"/>"#
        );
        let rels = document_relationships("rIdWatermark", "header1.xml");
        match self {
            Self::Text => package_bytes(
                self.name(),
                &single_section_document(Some("rIdWatermark")),
                &settings,
                &rels,
                &[("header1.xml", text_header("CONFIDENTIAL &amp; REVIEW"))],
            ),
            Self::Picture => package_bytes(
                self.name(),
                &single_section_document(Some("rIdWatermark")),
                &settings,
                &rels,
                &[("header1.xml", picture_header())],
            ),
            Self::InheritedText => package_bytes(
                self.name(),
                &inherited_section_document(),
                &settings,
                &rels,
                &[("header1.xml", text_header("INHERITED DRAFT"))],
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BidiFixture {
    Hebrew,
    Arabic,
    MixedLatinNumbersNeutrals,
    ExplicitRunOverride,
    List,
    Table,
    Header,
    TrackedRevisions,
}

impl BidiFixture {
    pub(crate) const ALL: [Self; 8] = [
        Self::Hebrew,
        Self::Arabic,
        Self::MixedLatinNumbersNeutrals,
        Self::ExplicitRunOverride,
        Self::List,
        Self::Table,
        Self::Header,
        Self::TrackedRevisions,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Hebrew => "bidi-hebrew",
            Self::Arabic => "bidi-arabic",
            Self::MixedLatinNumbersNeutrals => "bidi-mixed-latin-numbers-neutrals",
            Self::ExplicitRunOverride => "bidi-explicit-run-override",
            Self::List => "bidi-list",
            Self::Table => "bidi-table",
            Self::Header => "bidi-header",
            Self::TrackedRevisions => "bidi-tracked-revisions",
        }
    }

    pub(crate) fn package(self) -> Package {
        load_package(&self.bytes())
            .unwrap_or_else(|error| panic!("{} fixture did not load: {error:?}", self.name()))
    }

    pub(crate) fn bytes(self) -> Vec<u8> {
        let settings = empty_settings();
        match self {
            Self::Hebrew => package_bytes(
                self.name(),
                &bidi_document(
                    r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t>שלום</w:t></w:r></w:p>"#,
                    None,
                ),
                &settings,
                &empty_document_relationships(),
                &[],
            ),
            Self::Arabic => package_bytes(
                self.name(),
                &bidi_document(
                    r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t>مرحبا 123.</w:t></w:r></w:p>"#,
                    None,
                ),
                &settings,
                &empty_document_relationships(),
                &[],
            ),
            Self::MixedLatinNumbersNeutrals => package_bytes(
                self.name(),
                &bidi_document(
                    r#"<w:p><w:r><w:t>abc 123 אבג, def?</w:t></w:r></w:p>"#,
                    None,
                ),
                &settings,
                &empty_document_relationships(),
                &[],
            ),
            Self::ExplicitRunOverride => package_bytes(
                self.name(),
                &bidi_document(
                    "<w:p><w:r><w:t>A </w:t></w:r><w:r><w:rPr><w:rtl/></w:rPr><w:t>אב 12</w:t></w:r><w:r><w:t> </w:t></w:r><w:r><w:t>RLO \u{202e}abc\u{202c}</w:t></w:r></w:p>",
                    None,
                ),
                &settings,
                &empty_document_relationships(),
                &[],
            ),
            Self::List => package_bytes_with_extra(
                self.name(),
                &bidi_document(
                    r#"<w:p><w:pPr><w:bidi/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>פריט 123</w:t></w:r></w:p>"#,
                    None,
                ),
                &settings,
                &relationships(&[(
                    "rIdNumbering",
                    "numbering",
                    "numbering.xml",
                )]),
                &[],
                &[(
                    "word/numbering.xml",
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
                    numbering_xml(),
                )],
            ),
            Self::Table => package_bytes(
                self.name(),
                &bidi_document(
                    r#"<w:tbl><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>cell אבג 45</w:t></w:r></w:p></w:tc><w:tc><w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t>שלום</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
                    None,
                ),
                &settings,
                &empty_document_relationships(),
                &[],
            ),
            Self::Header => package_bytes(
                self.name(),
                &bidi_document(
                    r#"<w:p><w:r><w:t>body אבג</w:t></w:r></w:p>"#,
                    Some(r#"<w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr>"#),
                ),
                &settings,
                &relationships(&[("rIdHeader", "header", "header1.xml")]),
                &[(
                    "header1.xml",
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t>כותרת 77</w:t></w:r></w:p></w:hdr>"#.to_string(),
                )],
            ),
            Self::TrackedRevisions => package_bytes(
                self.name(),
                &bidi_document(
                    r#"<w:p><w:pPr><w:bidi/></w:pPr><w:ins w:id="7" w:author="Bidi Reviewer" w:date="2026-08-29T12:00:00Z"><w:r><w:t>חדש 123</w:t></w:r></w:ins><w:r><w:t> </w:t></w:r><w:del w:id="8" w:author="Bidi Reviewer" w:date="2026-08-29T12:01:00Z"><w:r><w:rPr><w:rtl/></w:rPr><w:delText>ישן 45</w:delText></w:r></w:del></w:p>"#,
                    None,
                ),
                &settings,
                &empty_document_relationships(),
                &[],
            ),
        }
    }

    pub(crate) const fn logical_text(self) -> &'static str {
        match self {
            Self::Hebrew => "שלום",
            Self::Arabic => "مرحبا 123.",
            Self::MixedLatinNumbersNeutrals => "abc 123 אבג, def?",
            Self::ExplicitRunOverride => "A אב 12 RLO \u{202e}abc\u{202c}",
            Self::List => "פריט 123",
            Self::Table => "cell אבג 45\tשלום",
            Self::Header => "body אבג",
            Self::TrackedRevisions => "חדש 123 ישן 45",
        }
    }

    pub(crate) const fn required_document_markers(self) -> &'static [&'static str] {
        match self {
            Self::Hebrew | Self::Arabic => &["<w:bidi/>"],
            Self::MixedLatinNumbersNeutrals => &[],
            Self::ExplicitRunOverride => &["<w:rtl/>", "\u{202e}", "\u{202c}"],
            Self::List => &["<w:bidi/>", "<w:numPr>"],
            Self::Table => &["<w:tbl>", "<w:bidi/>"],
            Self::Header => &["<w:headerReference w:type=\"default\" r:id=\"rIdHeader\"/>"],
            Self::TrackedRevisions => &["<w:bidi/>", "<w:ins ", "<w:del ", "<w:rtl/>"],
        }
    }

    pub(crate) const fn required_part_markers(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Header => &[("word/header1.xml", "<w:bidi/>")],
            Self::List => &[("word/numbering.xml", "<w:numbering")],
            _ => &[],
        }
    }
}

fn empty_settings() -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings xmlns:w="{W}"/>"#)
}

fn bidi_document(body: &str, sect_pr: Option<&str>) -> String {
    let sect_pr = sect_pr.unwrap_or(
        r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr>"#,
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="{R}"><w:body>{body}{sect_pr}</w:body></w:document>"#
    )
}

fn numbering_xml() -> String {
    format!(
        r#"<w:numbering xmlns:w="{W}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    )
}

fn single_section_document(header_rid: Option<&str>) -> String {
    let header = header_rid.map_or_else(String::new, |rid| {
        format!(r#"<w:headerReference w:type="default" r:id="{rid}"/>"#)
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="{R}"><w:body><w:p><w:r><w:t>Fixture body</w:t></w:r></w:p><w:sectPr>{header}<w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:body></w:document>"#
    )
}

fn inherited_section_document() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="{R}"><w:body><w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdWatermark"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:pPr><w:r><w:t>First section</w:t></w:r></w:p><w:p><w:r><w:t>Second section</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:body></w:document>"#
    )
}

fn empty_document_relationships() -> String {
    format!(r#"<?xml version="1.0"?><Relationships xmlns="{PR}"/>"#)
}

fn document_relationships(id: &str, target: &str) -> String {
    relationships(&[(id, "header", target)])
}

fn relationships(items: &[(&str, &str, &str)]) -> String {
    let rels = items
        .iter()
        .map(|(id, kind, target)| {
            format!(r#"<Relationship Id="{id}" Type="{R}/{kind}" Target="{target}"/>"#)
        })
        .collect::<String>();
    format!(r#"<?xml version="1.0"?><Relationships xmlns="{PR}">{rels}</Relationships>"#)
}

fn text_header(text: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="{W}" xmlns:v="urn:schemas-microsoft-com:vml"><w:p><w:r><w:pict><v:shape id="PowerPlusWaterMarkObject"><v:textpath string="{text}"/></v:shape></w:pict></w:r></w:p></w:hdr>"#
    )
}

fn picture_header() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="{W}" xmlns:r="{R}" xmlns:v="urn:schemas-microsoft-com:vml"><w:p><w:r><w:pict><v:shape id="PowerPlusWaterMarkObjectPicture"><v:imagedata r:id="rIdImage"/></v:shape></w:pict></w:r></w:p></w:hdr>"#
    )
}

fn package_bytes(
    name: &str,
    document: &str,
    settings: &str,
    document_rels: &str,
    headers: &[(&str, String)],
) -> Vec<u8> {
    package_bytes_with_extra(name, document, settings, document_rels, headers, &[])
}

fn package_bytes_with_extra(
    name: &str,
    document: &str,
    settings: &str,
    document_rels: &str,
    headers: &[(&str, String)],
    extra_parts: &[(&str, &str, String)],
) -> Vec<u8> {
    let header_overrides = headers
        .iter()
        .map(|(header, _)| {
            format!(
                r#"<Override PartName="/word/{header}" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#
            )
        })
        .collect::<String>();
    let extra_overrides = extra_parts
        .iter()
        .map(|(part_name, content_type, _)| {
            format!(r#"<Override PartName="/{part_name}" ContentType="{content_type}"/>"#)
        })
        .collect::<String>();
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>{header_overrides}{extra_overrides}</Types>"#
    );
    let root_rels = format!(
        r#"<?xml version="1.0"?><Relationships xmlns="{PR}"><Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>"#
    );
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="{W}"/>"#
    );
    let evidence = format!(r#"<?xml version="1.0"?><fixture name="{name}"/>"#);
    let mut parts = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.into_bytes(),
        ),
        ("_rels/.rels".to_string(), root_rels.into_bytes()),
        (
            "word/document.xml".to_string(),
            document.as_bytes().to_vec(),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels.as_bytes().to_vec(),
        ),
        ("word/styles.xml".to_string(), styles.into_bytes()),
        (
            "word/settings.xml".to_string(),
            settings.as_bytes().to_vec(),
        ),
        ("customXml/evidence.xml".to_string(), evidence.into_bytes()),
    ];
    parts.extend(
        headers
            .iter()
            .map(|(name, body)| (format!("word/{name}"), body.as_bytes().to_vec())),
    );
    parts.extend(
        extra_parts
            .iter()
            .map(|(name, _, body)| ((*name).to_string(), body.as_bytes().to_vec())),
    );
    write_zip(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::{Block, Inline, RevisionCategory, RevisionKind};
    use docxcore::package::{
        HeaderVariant, ProtectionEditMode, ProtectionEnforcement, WatermarkKind, save_package,
    };

    fn part_text(package: &Package, name: &str) -> String {
        String::from_utf8_lossy(
            package
                .part(name)
                .unwrap_or_else(|| panic!("fixture has {name}")),
        )
        .into_owned()
    }

    #[test]
    fn protection_catalog_contains_every_supported_mode_and_write_protection_case() {
        let parsed =
            ProtectionFixture::ALL.map(|fixture| (fixture, fixture.package().protection()));

        assert_eq!(
            parsed[0].1.edit_mode,
            Some(ProtectionEditMode::Unrestricted)
        );
        assert_eq!(parsed[1].1.edit_mode, Some(ProtectionEditMode::ReadOnly));
        assert_eq!(parsed[2].1.edit_mode, Some(ProtectionEditMode::Comments));
        assert_eq!(parsed[3].1.edit_mode, Some(ProtectionEditMode::Forms));
        assert_eq!(
            parsed[4].1.edit_mode,
            Some(ProtectionEditMode::TrackedChanges)
        );
        assert!(parsed[5].1.formatting_locked);
        assert!(parsed[6].1.advisory_write_protection);
        assert_eq!(parsed[6].1.enforcement, ProtectionEnforcement::Absent);
        assert!(parsed[7].1.enforced_write_protection);
        assert_eq!(parsed[7].1.label(), Some("read-only"));
        assert_eq!(
            parsed[8].1.edit_mode,
            Some(ProtectionEditMode::Unknown("producerSpecific".to_string()))
        );
        assert_eq!(parsed[8].1.label(), Some("restricted editing"));
    }

    #[test]
    fn watermark_catalog_covers_text_picture_and_section_inheritance() {
        let text = WatermarkFixture::Text.package().watermarks();
        assert_eq!(
            text[0].kind,
            WatermarkKind::Text("CONFIDENTIAL & REVIEW".to_string())
        );

        let picture = WatermarkFixture::Picture.package().watermarks();
        assert_eq!(picture[0].kind, WatermarkKind::Picture);

        let inherited = WatermarkFixture::InheritedText.package().watermarks();
        assert_eq!(inherited.len(), 2);
        assert_eq!(inherited[0].header.variant, HeaderVariant::Default);
        assert!(!inherited[0].header.inherited);
        assert_eq!(inherited[1].header.section_index, 1);
        assert!(inherited[1].header.inherited);
    }

    #[test]
    fn bidi_catalog_covers_package_level_direction_inputs() {
        let hebrew = BidiFixture::Hebrew.package();
        let Block::Paragraph(paragraph) = &hebrew.document.body[0] else {
            panic!("hebrew fixture starts with a paragraph");
        };
        assert!(paragraph.props.rtl);
        assert_eq!(paragraph.plain_text(), "שלום");

        let explicit = BidiFixture::ExplicitRunOverride.package();
        let Block::Paragraph(paragraph) = &explicit.document.body[0] else {
            panic!("explicit fixture starts with a paragraph");
        };
        assert!(
            paragraph.content.iter().any(|inline| {
                matches!(inline, Inline::Run(run) if run.props.rtl && run.text == "אב 12")
            }),
            "explicit run-level rtl was not modeled"
        );
        assert!(paragraph.plain_text().contains('\u{202e}'));

        let list = BidiFixture::List.package();
        let Block::Paragraph(paragraph) = &list.document.body[0] else {
            panic!("list fixture starts with a paragraph");
        };
        assert!(paragraph.props.rtl);
        assert_eq!(paragraph.props.num_id, Some(1));

        let table = BidiFixture::Table.package();
        let Block::Table(table) = &table.document.body[0] else {
            panic!("table fixture starts with a table");
        };
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[0].cells[1].blocks[0].plain_text(), "שלום");

        let header = BidiFixture::Header.package();
        let header_xml = part_text(&header, "word/header1.xml");
        assert!(header_xml.contains("<w:bidi/>"), "{header_xml}");

        let revisions = BidiFixture::TrackedRevisions.package();
        let categories = revisions
            .document
            .revisions()
            .into_iter()
            .map(|revision| revision.category)
            .collect::<Vec<_>>();
        assert_eq!(
            categories,
            vec![
                RevisionCategory::Inline(RevisionKind::Insert),
                RevisionCategory::Inline(RevisionKind::Delete),
            ]
        );
    }

    #[test]
    fn bidi_fixtures_preserve_direction_xml_on_save_reload() {
        for fixture in BidiFixture::ALL {
            let package = fixture.package();
            let original_text = package.document.plain_text();
            let original_document_xml = part_text(&package, "word/document.xml");

            for marker in fixture.required_document_markers() {
                assert!(
                    original_document_xml.contains(marker),
                    "{} original document.xml missing {marker:?}: {original_document_xml}",
                    fixture.name()
                );
            }
            for (part, marker) in fixture.required_part_markers() {
                let original_part = part_text(&package, part);
                assert!(
                    original_part.contains(marker),
                    "{} original {part} missing {marker:?}: {original_part}",
                    fixture.name()
                );
            }

            let saved = save_package(&package);
            let reloaded = load_package(&saved)
                .unwrap_or_else(|error| panic!("reload {}: {error:?}", fixture.name()));
            assert_eq!(reloaded.document, package.document, "{}", fixture.name());
            assert_eq!(
                reloaded.document.plain_text(),
                original_text,
                "{}",
                fixture.name()
            );

            let saved_document_xml = part_text(&reloaded, "word/document.xml");
            for marker in fixture.required_document_markers() {
                assert!(
                    saved_document_xml.contains(marker),
                    "{} saved document.xml missing {marker:?}: {saved_document_xml}",
                    fixture.name()
                );
            }
            for (part, marker) in fixture.required_part_markers() {
                let saved_part = part_text(&reloaded, part);
                assert!(
                    saved_part.contains(marker),
                    "{} saved {part} missing {marker:?}: {saved_part}",
                    fixture.name()
                );
            }
        }
    }

    #[test]
    fn every_fixture_preserves_unmodeled_and_metadata_parts_on_save_reload() {
        let packages = ProtectionFixture::ALL
            .into_iter()
            .map(ProtectionFixture::package)
            .chain(
                WatermarkFixture::ALL
                    .into_iter()
                    .map(WatermarkFixture::package),
            );

        for package in packages {
            let expected_document = package.document.clone();
            let mut expected_parts = package
                .part_names()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            expected_parts.sort();
            let preserved = package
                .part_names()
                .into_iter()
                .filter(|name| *name != "word/document.xml")
                .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
                .collect::<Vec<_>>();

            let reloaded = load_package(&save_package(&package)).expect("reload saved fixture");
            let mut actual_parts = reloaded
                .part_names()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            actual_parts.sort();

            assert_eq!(reloaded.document, expected_document);
            assert_eq!(actual_parts, expected_parts);
            for (name, bytes) in preserved {
                assert_eq!(reloaded.part(&name), Some(bytes.as_slice()), "{name}");
            }
        }
    }
}
