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
    Unknown,
}

impl ProtectionFixture {
    pub(crate) const ALL: [Self; 8] = [
        Self::Unrestricted,
        Self::ReadOnly,
        Self::Comments,
        Self::Forms,
        Self::TrackedChanges,
        Self::FormattingOnly,
        Self::Advisory,
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
    format!(
        r#"<?xml version="1.0"?><Relationships xmlns="{PR}"><Relationship Id="{id}" Type="{R}/header" Target="{target}"/></Relationships>"#
    )
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
    let header_overrides = headers
        .iter()
        .map(|(header, _)| {
            format!(
                r#"<Override PartName="/word/{header}" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#
            )
        })
        .collect::<String>();
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>{header_overrides}</Types>"#
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
    write_zip(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::package::{
        HeaderVariant, ProtectionEditMode, ProtectionEnforcement, WatermarkKind, save_package,
    };

    #[test]
    fn protection_catalog_contains_every_supported_mode_and_advisory_case() {
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
        assert_eq!(
            parsed[7].1.edit_mode,
            Some(ProtectionEditMode::Unknown("producerSpecific".to_string()))
        );
        assert_eq!(parsed[7].1.label(), Some("restricted editing"));
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
