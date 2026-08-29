//! Open/save a whole `.docx` while preserving everything we don't model.
//!
//! The save strategy that keeps documents from being corrupted: keep **every**
//! original ZIP part byte-for-byte, and on save rewrite only `word/document.xml`
//! from the [`Document`] model. The trailing section properties (`w:sectPr`,
//! which carry page size/margins/orientation and tracked property changes) are
//! modeled and round-tripped.
//!
//! Known limitations (documented, not silent): body content other than
//! paragraphs/tables — e.g. bookmarks, mid-document
//! section breaks, comments anchors — is not reconstructed by the serializer and
//! is dropped on save. Full raw-node preservation is a later refinement.

use crate::load::{LoadError, parse_document_xml, parse_rels_xml};
use crate::model::{Block, Document, PropertyScope, SectionProperties};
use crate::serialize::document_to_xml;
use crate::xml::{Event, XmlParser};
use crate::zip::ZipArchive;
use crate::zipwrite::write_zip;
use std::borrow::Cow;

const OLE2: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

fn decode_xml_entities(s: &str) -> String {
    let mut decoded = String::new();
    XmlParser::append_decoded(s, &mut decoded);
    decoded
}

/// Decode an OPC XML part in one of the encodings XML processors must
/// recognize without an external declaration. OOXML normally uses UTF-8, but
/// valid packages may use UTF-16LE/BE for individual XML parts.
fn decode_xml_part(bytes: &[u8]) -> Option<Cow<'_, str>> {
    if let Some(utf8) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return std::str::from_utf8(utf8).ok().map(Cow::Borrowed);
    }

    let utf16 = if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        Some((rest, true))
    } else if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        Some((rest, false))
    } else if bytes.starts_with(&[b'<', 0, b'?', 0]) {
        Some((bytes, true))
    } else if bytes.starts_with(&[0, b'<', 0, b'?']) {
        Some((bytes, false))
    } else {
        None
    };

    if let Some((encoded, little_endian)) = utf16 {
        let mut chunks = encoded.chunks_exact(2);
        let units = chunks
            .by_ref()
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        if !chunks.remainder().is_empty() {
            return None;
        }
        return String::from_utf16(&units).ok().map(Cow::Owned);
    }

    std::str::from_utf8(bytes).ok().map(Cow::Borrowed)
}

/// Whether `w:documentProtection` is actually enforced.
///
/// Keeping an absent value distinct from an explicit false value preserves the
/// source information needed to explain why a protection declaration is not
/// being applied. Both states are non-enforcing per OOXML; an unrecognized
/// present value is represented as enforced with an unknown mode so policy can
/// fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProtectionEnforcement {
    #[default]
    Absent,
    Disabled,
    Enforced,
}

/// The edit restriction declared by `w:documentProtection/@w:edit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionEditMode {
    Unrestricted,
    ReadOnly,
    Comments,
    TrackedChanges,
    Forms,
    /// Preserve a future or producer-specific value so policy can fail closed
    /// without losing the value needed for a useful explanation.
    Unknown(String),
}

/// Raw settings metadata retained alongside the normalized protection model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProtectionSource {
    pub settings_part_present: bool,
    pub document_protection_present: bool,
    pub write_protection_present: bool,
    pub enforcement_value: Option<String>,
    pub edit_value: Option<String>,
    pub formatting_value: Option<String>,
    pub write_recommended_value: Option<String>,
    /// Whether `w:writeProtection` carried a password verifier (`password`,
    /// `hash`, or `hashValue`) rather than only the advisory UI flag.
    pub write_credential_present: bool,
}

/// Structured protection metadata from `word/settings.xml`.
///
/// Enforced document restrictions and recommendation-only `w:writeProtection`
/// deliberately coexist in this model. Password-backed write protection is a
/// real write lock; only the recommendation-only form remains advisory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Protection {
    pub enforcement: ProtectionEnforcement,
    pub edit_mode: Option<ProtectionEditMode>,
    pub formatting_locked: bool,
    pub advisory_write_protection: bool,
    pub enforced_write_protection: bool,
    pub source: ProtectionSource,
}

impl Protection {
    pub fn is_enforced(&self) -> bool {
        self.enforcement == ProtectionEnforcement::Enforced || self.enforced_write_protection
    }

    /// Compatibility label for status text. Authorization must inspect the
    /// structured fields above instead of comparing this human-readable value.
    pub fn label(&self) -> Option<&'static str> {
        if self.enforced_write_protection {
            return Some("read-only");
        }
        if self.is_enforced() {
            match self.edit_mode.as_ref() {
                Some(ProtectionEditMode::ReadOnly) => return Some("read-only"),
                Some(ProtectionEditMode::Comments) => return Some("comments only"),
                Some(ProtectionEditMode::TrackedChanges) => {
                    return Some("tracked changes only");
                }
                Some(ProtectionEditMode::Forms) => return Some("form fields only"),
                Some(ProtectionEditMode::Unknown(_)) => return Some("restricted editing"),
                Some(ProtectionEditMode::Unrestricted) | None => {}
            }
            if self.formatting_locked {
                return Some("formatting locked");
            }
        }
        self.advisory_write_protection
            .then_some("read-only (recommended)")
    }
}

/// Header variant, and therefore the page class, carrying a watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderVariant {
    Default,
    First,
    Even,
}

impl HeaderVariant {
    fn as_ooxml(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::First => "first",
            Self::Even => "even",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Default => 0,
            Self::First => 1,
            Self::Even => 2,
        }
    }
}

/// The terminal-relevant kind of a watermark found in an applied header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatermarkKind {
    Text(String),
    Picture,
    Unknown,
}

/// Relationship and section metadata describing where a watermark applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatermarkHeader {
    /// Zero-based section index in document order.
    pub section_index: usize,
    pub variant: HeaderVariant,
    pub relationship_id: String,
    pub part_name: String,
    /// True when this section inherits the header reference from an earlier one.
    pub inherited: bool,
}

/// A watermark plus the applied header that supplies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    pub kind: WatermarkKind,
    pub header: WatermarkHeader,
}

/// Compatibility label derived from already-parsed watermark metadata.
/// Renderers can keep the structured records as their single source of truth
/// without asking the package to parse the same header parts a second time.
pub fn watermark_label_from(watermarks: &[Watermark]) -> Option<String> {
    watermarks
        .iter()
        .find_map(|watermark| match &watermark.kind {
            WatermarkKind::Text(text) => Some(text.clone()),
            WatermarkKind::Picture | WatermarkKind::Unknown => None,
        })
        .or_else(|| {
            watermarks
                .iter()
                .find_map(|watermark| match watermark.kind {
                    WatermarkKind::Picture => Some("picture (preview unavailable)".to_string()),
                    WatermarkKind::Unknown => Some("unsupported (preview unavailable)".to_string()),
                    WatermarkKind::Text(_) => None,
                })
        })
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

const WORDPROCESSINGML_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const STRICT_WORDPROCESSINGML_NS: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const PACKAGE_RELATIONSHIPS_NS: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
const SETTINGS_RELATIONSHIP_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
const STRICT_SETTINGS_RELATIONSHIP_TYPE: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/settings";

fn namespace_scope(
    parser: &XmlParser<'_>,
    parent: Option<&[(String, String)]>,
) -> Vec<(String, String)> {
    let mut scope = parent.unwrap_or_default().to_vec();
    for attr in parser.attrs() {
        let prefix = if attr.name == "xmlns" {
            Some("")
        } else {
            attr.name.strip_prefix("xmlns:")
        };
        let Some(prefix) = prefix else {
            continue;
        };
        let value = decode_xml_entities(attr.value);
        if let Some((_, bound)) = scope.iter_mut().find(|(bound, _)| bound == prefix) {
            *bound = value;
        } else {
            scope.push((prefix.to_string(), value));
        }
    }
    scope
}

fn wordprocessingml_name(name: &str, namespaces: &[(String, String)], attribute: bool) -> bool {
    let prefix = name.split_once(':').map(|(prefix, _)| prefix);
    let lookup = |prefix: &str| {
        namespaces
            .iter()
            .rev()
            .find(|(bound, _)| bound == prefix)
            .map(|(_, uri)| uri.as_str())
    };
    let namespace = match prefix {
        Some(prefix) => lookup(prefix),
        None if attribute => None,
        None => lookup(""),
    };
    namespace.is_some_and(|uri| matches!(uri, WORDPROCESSINGML_NS | STRICT_WORDPROCESSINGML_NS))
        || matches!(prefix, Some("w")) && namespace.is_none()
}

fn decoded_wordprocessingml_attr(
    parser: &XmlParser<'_>,
    namespaces: &[(String, String)],
    name: &str,
) -> Option<String> {
    parser
        .attrs()
        .iter()
        .find(|attr| {
            local_name(attr.name) == name && wordprocessingml_name(attr.name, namespaces, true)
        })
        .map(|attr| decode_xml_entities(attr.value))
}

fn decoded_attr_by_local(parser: &XmlParser<'_>, name: &str) -> Option<String> {
    parser
        .attrs()
        .iter()
        .find(|attr| local_name(attr.name) == name)
        .map(|attr| decode_xml_entities(attr.value))
}

fn package_relationships_name(name: &str, namespaces: &[(String, String)]) -> bool {
    let prefix = name.split_once(':').map_or("", |(prefix, _)| prefix);
    namespaces
        .iter()
        .rev()
        .find(|(bound, _)| bound == prefix)
        .is_some_and(|(_, uri)| uri == PACKAGE_RELATIONSHIPS_NS)
}

fn decoded_unqualified_attr(parser: &XmlParser<'_>, name: &str) -> Option<String> {
    parser
        .attrs()
        .iter()
        .find(|attr| attr.name == name)
        .map(|attr| decode_xml_entities(attr.value))
}

fn parse_ooxml_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" => Some(true),
        "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

fn parse_protection(xml: &str) -> Protection {
    let mut protection = Protection::default();
    protection.source.settings_part_present = true;
    let mut parser = XmlParser::new(xml);
    let mut namespace_stack = Vec::<Vec<(String, String)>>::new();
    loop {
        match parser.next() {
            Event::Start => {
                let scope = namespace_scope(&parser, namespace_stack.last().map(Vec::as_slice));
                namespace_stack.push(scope);
                let namespaces = namespace_stack.last().expect("scope was just pushed");

                if local_name(parser.name()) == "documentProtection"
                    && wordprocessingml_name(parser.name(), namespaces, false)
                {
                    protection.source.document_protection_present = true;
                    let enforcement =
                        decoded_wordprocessingml_attr(&parser, namespaces, "enforcement");
                    let edit = decoded_wordprocessingml_attr(&parser, namespaces, "edit");
                    let formatting =
                        decoded_wordprocessingml_attr(&parser, namespaces, "formatting");
                    let parsed_enforcement = enforcement.as_deref().map(parse_ooxml_bool);
                    let parsed_formatting = formatting.as_deref().map(parse_ooxml_bool);

                    let next_enforcement = match parsed_enforcement {
                        None => ProtectionEnforcement::Absent,
                        Some(Some(false)) => ProtectionEnforcement::Disabled,
                        Some(Some(true) | None) => ProtectionEnforcement::Enforced,
                    };
                    let mut next_edit_mode = edit.as_deref().map(|value| match value {
                        "none" => ProtectionEditMode::Unrestricted,
                        "readOnly" => ProtectionEditMode::ReadOnly,
                        "comments" => ProtectionEditMode::Comments,
                        "trackedChanges" => ProtectionEditMode::TrackedChanges,
                        "forms" => ProtectionEditMode::Forms,
                        other => ProtectionEditMode::Unknown(other.to_string()),
                    });
                    let next_formatting_locked = parsed_formatting == Some(Some(true));
                    if parsed_enforcement == Some(None) {
                        next_edit_mode = Some(ProtectionEditMode::Unknown(
                            "invalid enforcement value".to_string(),
                        ));
                    } else if parsed_enforcement == Some(Some(true))
                        && parsed_formatting == Some(None)
                    {
                        next_edit_mode = Some(ProtectionEditMode::Unknown(
                            "invalid formatting value".to_string(),
                        ));
                    }

                    let already_enforced =
                        protection.enforcement == ProtectionEnforcement::Enforced;
                    let next_is_enforced = next_enforcement == ProtectionEnforcement::Enforced;
                    if already_enforced && next_is_enforced {
                        if protection.edit_mode != next_edit_mode
                            || protection.formatting_locked != next_formatting_locked
                        {
                            protection.edit_mode = Some(ProtectionEditMode::Unknown(
                                "conflicting documentProtection declarations".to_string(),
                            ));
                            protection.formatting_locked |= next_formatting_locked;
                        }
                    } else if !already_enforced || next_is_enforced {
                        protection.enforcement = next_enforcement;
                        protection.edit_mode = next_edit_mode;
                        protection.formatting_locked = next_formatting_locked;
                        protection.source.enforcement_value = enforcement;
                        protection.source.edit_value = edit;
                        protection.source.formatting_value = formatting;
                    }
                } else if local_name(parser.name()) == "writeProtection"
                    && wordprocessingml_name(parser.name(), namespaces, false)
                {
                    protection.source.write_protection_present = true;
                    let recommended =
                        decoded_wordprocessingml_attr(&parser, namespaces, "recommended");
                    let credential_present = parser.attrs().iter().any(|attr| {
                        matches!(local_name(attr.name), "password" | "hash" | "hashValue")
                            && wordprocessingml_name(attr.name, namespaces, true)
                            && !attr.value.is_empty()
                    });
                    protection.enforced_write_protection |= credential_present;
                    protection.advisory_write_protection = !protection.enforced_write_protection
                        && (protection.advisory_write_protection
                            || recommended
                                .as_deref()
                                .and_then(parse_ooxml_bool)
                                .unwrap_or(false));
                    if recommended.is_some() {
                        protection.source.write_recommended_value = recommended;
                    }
                    protection.source.write_credential_present |= credential_present;
                }
            }
            Event::End => {
                namespace_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    protection
}

fn resolve_document_relationship_target(target: &str) -> Option<String> {
    if target.contains('\\') || target.contains("://") {
        return None;
    }
    let mut components = if target.starts_with('/') {
        Vec::new()
    } else {
        vec!["word"]
    };
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn settings_part_target(xml: &str) -> Result<Option<String>, ()> {
    let mut parser = XmlParser::new(xml);
    let mut namespace_stack = Vec::<Vec<(String, String)>>::new();
    let mut root_seen = false;
    let mut target = None;
    loop {
        match parser.next() {
            Event::Start => {
                let scope = namespace_scope(&parser, namespace_stack.last().map(Vec::as_slice));
                namespace_stack.push(scope);
                let namespaces = namespace_stack.last().expect("scope was just pushed");
                if !root_seen {
                    root_seen = true;
                    if local_name(parser.name()) != "Relationships"
                        || !package_relationships_name(parser.name(), namespaces)
                    {
                        return Err(());
                    }
                    continue;
                }
                if namespace_stack.len() != 2
                    || local_name(parser.name()) != "Relationship"
                    || !package_relationships_name(parser.name(), namespaces)
                {
                    continue;
                }
                if parser.attrs().iter().any(|attr| {
                    matches!(
                        local_name(attr.name),
                        "Id" | "Type" | "Target" | "TargetMode"
                    ) && !matches!(attr.name, "Id" | "Type" | "Target" | "TargetMode")
                }) {
                    return Err(());
                }
                let relation_type = decoded_unqualified_attr(&parser, "Type");
                if matches!(
                    relation_type.as_deref(),
                    Some(SETTINGS_RELATIONSHIP_TYPE | STRICT_SETTINGS_RELATIONSHIP_TYPE)
                ) {
                    if target.is_some()
                        || decoded_unqualified_attr(&parser, "TargetMode")
                            .as_deref()
                            .is_some_and(|value| value != "Internal")
                    {
                        return Err(());
                    }
                    target = Some(
                        decoded_unqualified_attr(&parser, "Target")
                            .filter(|target| !target.is_empty())
                            .ok_or(())?,
                    );
                }
            }
            Event::End => {
                namespace_stack.pop();
            }
            Event::Eof => return root_seen.then_some(target).ok_or(()),
            _ => {}
        }
    }
}

fn marker_in_attrs(parser: &XmlParser<'_>) -> bool {
    parser.attrs().iter().any(|attr| {
        matches!(local_name(attr.name), "id" | "name" | "title" | "descr")
            && decode_xml_entities(attr.value)
                .to_ascii_lowercase()
                .contains("watermark")
    })
}

fn watermark_kinds(xml: &str) -> Vec<WatermarkKind> {
    #[derive(Default)]
    struct Shape {
        marked: bool,
        texts: Vec<String>,
        picture: bool,
    }

    let mut parser = XmlParser::new(xml);
    let mut shapes: Vec<Shape> = Vec::new();
    let mut out = Vec::new();
    loop {
        match parser.next() {
            Event::Start if local_name(parser.name()) == "shape" => {
                shapes.push(Shape {
                    marked: marker_in_attrs(&parser),
                    ..Shape::default()
                });
            }
            Event::Start if local_name(parser.name()) == "textpath" => {
                let Some(text) = decoded_attr_by_local(&parser, "string") else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                if let Some(shape) = shapes.last_mut() {
                    shape.texts.push(text);
                }
            }
            Event::Start if local_name(parser.name()) == "imagedata" => {
                if let Some(shape) = shapes.last_mut() {
                    shape.picture = true;
                    shape.marked |= marker_in_attrs(&parser);
                }
            }
            Event::Start if local_name(parser.name()) == "docPr" => {
                // DrawingML picture watermarks use a watermark-named docPr rather
                // than VML's PowerPlusWaterMarkObject shape id.
                if marker_in_attrs(&parser) {
                    out.push(WatermarkKind::Picture);
                }
            }
            Event::End if local_name(parser.name()) == "shape" => {
                let Some(shape) = shapes.pop() else {
                    continue;
                };
                if shape.marked && !shape.texts.is_empty() {
                    out.extend(shape.texts.into_iter().map(WatermarkKind::Text));
                } else if shape.marked && shape.picture {
                    out.push(WatermarkKind::Picture);
                } else if shape.marked {
                    out.push(WatermarkKind::Unknown);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
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
    /// Resolve the settings part through the main document relationship. The
    /// conventional name remains a compatibility fallback for older fixtures
    /// and producer packages that omitted the otherwise-required relationship.
    fn settings_part_name(&self) -> Result<Option<String>, &'static str> {
        if let Some(rels) = self.part("word/_rels/document.xml.rels") {
            let xml = decode_xml_part(rels).ok_or("unreadable document relationships XML")?;
            if let Some(target) =
                settings_part_target(&xml).map_err(|()| "invalid settings relationship")?
            {
                let name = resolve_document_relationship_target(&target)
                    .ok_or("invalid settings relationship target")?;
                if self.part(&name).is_none() {
                    return Err("missing related settings part");
                }
                return Ok(Some(name));
            }
        }
        Ok(self
            .part("word/settings.xml")
            .is_some()
            .then(|| "word/settings.xml".to_string()))
    }

    /// Names of all parts in the container (for inspection/tests).
    pub fn part_names(&self) -> Vec<&str> {
        self.parts.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// The captured trailing section properties (`w:sectPr`) XML, which carries
    /// the header/footer references and page geometry.
    pub fn sect_pr(&self) -> &str {
        self.document
            .trailing_section_properties()
            .map(|section| section.raw.as_str())
            .unwrap_or(&self.sect_pr)
    }

    /// Replace the trailing section properties (e.g. to change page orientation).
    pub fn set_sect_pr(&mut self, xml: String) {
        let (raw, property_change) =
            crate::load::split_property_change_container(&xml, PropertyScope::Section);
        let section = SectionProperties {
            raw,
            property_change,
        };
        if let Some(current) = self.document.trailing_section_properties_mut() {
            *current = section;
        } else {
            self.document.body.push(Block::SectionProperties(section));
        }
        self.sect_pr = xml;
    }

    fn set_current_sect_pr_raw(&mut self, raw: String) {
        if let Some(section) = self.document.trailing_section_properties_mut() {
            section.raw = raw.clone();
        } else {
            self.document
                .body
                .push(Block::SectionProperties(SectionProperties {
                    raw: raw.clone(),
                    property_change: None,
                }));
        }
        self.sect_pr = raw;
    }

    /// Structured protection state from the document's related settings part.
    pub fn protection(&self) -> Protection {
        let name = match self.settings_part_name() {
            Ok(Some(name)) => name,
            Ok(None) => return Protection::default(),
            Err(reason) => {
                return Protection {
                    enforcement: ProtectionEnforcement::Enforced,
                    edit_mode: Some(ProtectionEditMode::Unknown(reason.to_string())),
                    ..Protection::default()
                };
            }
        };
        let bytes = self
            .part(&name)
            .expect("settings_part_name verifies that the related part exists");
        let Some(xml) = decode_xml_part(bytes) else {
            return Protection {
                enforcement: ProtectionEnforcement::Enforced,
                edit_mode: Some(ProtectionEditMode::Unknown(
                    "unreadable settings XML".to_string(),
                )),
                source: ProtectionSource {
                    settings_part_present: true,
                    ..ProtectionSource::default()
                },
                ..Protection::default()
            };
        };
        parse_protection(&xml)
    }

    /// Compatibility label for existing status and control surfaces.
    pub fn protection_label(&self) -> Option<&'static str> {
        self.protection().label()
    }

    /// Watermarks in headers that are actually applied by document section
    /// relationships. Each inherited header is associated with every section in
    /// which it remains effective rather than merely scanning orphan header parts.
    pub fn watermarks(&self) -> Vec<Watermark> {
        const VARIANTS: [HeaderVariant; 3] = [
            HeaderVariant::Default,
            HeaderVariant::First,
            HeaderVariant::Even,
        ];

        let rels = self
            .part("word/_rels/document.xml.rels")
            .and_then(decode_xml_part)
            .map(|xml| parse_rels_xml(&xml))
            .unwrap_or_default();
        let even_and_odd = self.has_even_odd();

        let mut sections: Vec<&str> = self
            .document
            .body
            .iter()
            .filter_map(|block| match block {
                crate::model::Block::Paragraph(p) => p.props.section_break.as_deref(),
                crate::model::Block::Table(_)
                | crate::model::Block::SectionProperties(_)
                | crate::model::Block::Raw(_) => None,
            })
            .collect();
        // The trailing body sectPr describes the final section. Even an empty
        // value represents the one implicit section of a document without sectPr.
        sections.push(self.sect_pr());

        #[derive(Clone)]
        struct AppliedHeader {
            relationship_id: String,
            part_name: String,
            inherited: bool,
        }

        let mut applied: [Option<AppliedHeader>; 3] = [None, None, None];
        let mut out = Vec::new();
        for (section_index, sect_pr) in sections.into_iter().enumerate() {
            for variant in VARIANTS {
                let slot = variant.index();
                if let Some(relationship_id) = crate::load::header_footer_ref_rid(
                    sect_pr,
                    "headerReference",
                    variant.as_ooxml(),
                ) {
                    applied[slot] = rels.target(&relationship_id).and_then(|target| {
                        Some(AppliedHeader {
                            relationship_id,
                            part_name: resolve_document_relationship_target(target)?,
                            inherited: false,
                        })
                    });
                } else if let Some(header) = &mut applied[slot] {
                    header.inherited = true;
                }
            }

            let title_page = settings_flag_of(sect_pr, "w:titlePg").unwrap_or(false);
            for variant in VARIANTS {
                if (variant == HeaderVariant::First && !title_page)
                    || (variant == HeaderVariant::Even && !even_and_odd)
                {
                    continue;
                }
                let Some(header) = &applied[variant.index()] else {
                    continue;
                };
                let Some(bytes) = self.part(&header.part_name) else {
                    continue;
                };
                let Some(xml) = decode_xml_part(bytes) else {
                    continue;
                };
                for kind in watermark_kinds(&xml) {
                    out.push(Watermark {
                        kind,
                        header: WatermarkHeader {
                            section_index,
                            variant,
                            relationship_id: header.relationship_id.clone(),
                            part_name: header.part_name.clone(),
                            inherited: header.inherited,
                        },
                    });
                }
            }
        }
        out
    }

    /// Compatibility label for status text while renderers consume [`Watermark`].
    pub fn watermark_label(&self) -> Option<String> {
        let watermarks = self.watermarks();
        watermark_label_from(&watermarks)
    }

    /// Whether the document defines page borders (`w:pgBorders` in any section).
    /// Surfaced as an indicator; a terminal doesn't draw the page frame itself.
    pub fn has_page_borders(&self) -> bool {
        self.sect_pr().contains("<w:pgBorders")
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

    /// Create a new, empty header (`is_header`) or footer part of the given
    /// reference type (`"default"`, `"first"`, or `"even"`) and wire it up: add
    /// the part, a `[Content_Types].xml` override, a relationship in
    /// `document.xml.rels`, and a `<w:headerReference>`/`<w:footerReference>` of
    /// that type in the section properties. Returns the new part name.
    pub fn create_hf(&mut self, is_header: bool, hf_type: &str) -> Option<String> {
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

        // Section reference (must be among the first children of sectPr).
        let reference = format!("<w:{kind}Reference w:type=\"{hf_type}\" r:id=\"{rid}\"/>");
        let section = inject_sect_child(self.sect_pr(), &reference);
        self.set_current_sect_pr_raw(section);
        Some(part_name)
    }

    /// Whether the section has a distinct first-page header/footer (`<w:titlePg/>`).
    pub fn has_title_pg(&self) -> bool {
        self.sect_pr().contains("<w:titlePg")
    }

    /// Toggle a distinct first-page header/footer (`<w:titlePg/>` in the section).
    /// When turning it off, the "first" parts are left in place (as Word does).
    pub fn set_title_pg(&mut self, on: bool) {
        if on == self.has_title_pg() {
            return;
        }
        if on {
            // titlePg belongs near the end of CT_SectPr, so append before the close.
            let section = append_sect_child(self.sect_pr(), "<w:titlePg/>");
            self.set_current_sect_pr_raw(section);
        } else {
            let section = remove_element(self.sect_pr(), "w:titlePg");
            self.set_current_sect_pr_raw(section);
        }
    }

    /// Whether the document uses distinct even/odd page headers/footers
    /// (`<w:evenAndOddHeaders/>` in `word/settings.xml`).
    pub fn has_even_odd(&self) -> bool {
        self.settings_flag("w:evenAndOddHeaders").unwrap_or(false)
    }

    /// Toggle distinct even/odd headers/footers (`<w:evenAndOddHeaders/>`).
    pub fn set_even_odd(&mut self, on: bool) {
        self.set_settings_flag("w:evenAndOddHeaders", on);
    }

    /// Whether automatic hyphenation is on (`<w:autoHyphenation/>` in settings).
    pub fn has_auto_hyphenation(&self) -> bool {
        self.settings_flag("w:autoHyphenation").unwrap_or(false)
    }

    /// A boolean flag element's state in the related settings part: `None` when the
    /// element is absent, otherwise its `w:val` (absent `w:val` means on).
    fn settings_flag(&self, elem: &str) -> Option<bool> {
        let name = self.settings_part_name().ok()??;
        let b = self.part(&name)?;
        let xml = decode_xml_part(b)?;
        settings_flag_of(&xml, elem)
    }

    /// Toggle automatic hyphenation for the document (`<w:autoHyphenation/>`).
    /// docxy doesn't hyphenate its own on-screen layout, but Word honours the
    /// flag when it lays the document out for print.
    pub fn set_auto_hyphenation(&mut self, on: bool) {
        self.set_settings_flag("w:autoHyphenation", on);
    }

    /// Add or remove a boolean flag element (e.g. `w:evenAndOddHeaders`,
    /// `w:autoHyphenation`) in `word/settings.xml`, creating the part (+ its
    /// content-type and relationship) if it doesn't exist yet.
    fn set_settings_flag(&mut self, elem: &str, on: bool) {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let existing_name = match self.settings_part_name() {
            Ok(name) => name,
            Err(_) => return,
        };
        let name = existing_name.as_deref().unwrap_or("word/settings.xml");
        if let Some(b) = self.part(name) {
            let xml = String::from_utf8_lossy(b).into_owned();
            let cur = settings_flag_of(&xml, elem);
            if cur == Some(on) {
                return; // already in the wanted state
            }
            // Word writes an explicit off as `<w:autoHyphenation w:val="false"/>`.
            // Turning the flag ON therefore has to REPLACE that element, not skip
            // because "the tag is already there" — which is what made the toggle
            // look dead until it was pressed twice.
            let xml = if cur.is_some() {
                remove_element(&xml, elem)
            } else {
                xml
            };
            if !on {
                self.set_part(name, xml.into_bytes());
                return;
            }
            // `<w:settings … />` is a legal empty root; splicing after its `>`
            // would append a SECOND root element and make the part unparseable.
            let Some(s) = xml.find("<w:settings") else {
                return; // not a settings part we recognise; leave it alone
            };
            let Some(rel) = xml[s..].find('>') else {
                return;
            };
            let gt = s + rel;
            let new = if xml[..gt].ends_with('/') {
                format!(
                    "{}><{elem}/></w:settings>{}",
                    &xml[..gt - 1],
                    &xml[gt + 1..]
                )
            } else {
                format!("{}<{elem}/>{}", &xml[..gt + 1], &xml[gt + 1..])
            };
            self.set_part(name, new.into_bytes());
            return;
        }
        if !on {
            return; // nothing to turn off
        }
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:settings xmlns:w=\"{W_NS}\"><{elem}/></w:settings>"
        );
        self.parts.push((name.to_string(), body.into_bytes()));
        if let Some(b) = self.part("[Content_Types].xml") {
            let ct = String::from_utf8_lossy(b).into_owned();
            if !ct.contains("settings+xml") {
                let ov = "<Override PartName=\"/word/settings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/>";
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
            if !rels.contains("settings.xml") {
                let rid = next_rid(&rels);
                let rel = format!(
                    "<Relationship Id=\"{rid}\" Type=\"{R_NS}/settings\" Target=\"settings.xml\"/>"
                );
                self.set_part(
                    rels_name,
                    rels.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
                        .into_bytes(),
                );
            }
        }
    }

    /// The number of newspaper columns in the body section (`w:cols w:num`).
    pub fn columns(&self) -> i32 {
        self.page_geom().cols
    }

    /// Set the number of newspaper columns (`w:cols w:num`) in the body section,
    /// with an equal gap. docxy still renders a single column on screen, but the
    /// column layout round-trips and Word lays it out in columns.
    pub fn set_columns(&mut self, num: i32) {
        let num = num.max(1);
        let mut s = self.sect_pr().to_string();
        s = remove_element(&s, "w:cols");
        let child = if num <= 1 {
            "<w:cols w:space=\"720\"/>".to_string()
        } else {
            format!("<w:cols w:num=\"{num}\" w:space=\"720\" w:equalWidth=\"1\"/>")
        };
        // `w:cols` follows `w:pgMar` in CT_SectPr; place it just after when present.
        s = insert_after_element(&s, "w:pgMar", &child);
        self.set_current_sect_pr_raw(s);
    }

    /// Add a new `word/media/imageN.<ext>` part (e.g. a mermaid-rendered PNG/SVG),
    /// wiring up `[Content_Types].xml` (a `Default` for `<ext>`, added if not
    /// already declared) and a `document.xml.rels` relationship of type
    /// `.../relationships/image`. Returns the new relationship id (`rId…`), for
    /// use in a `<a:blip r:embed="…">`. Mirrors [`Package::create_hf`]'s
    /// add-part idiom (unused part name, `.rels` append via `next_rid`,
    /// `[Content_Types].xml` append).
    pub(crate) fn add_media_part(&mut self, bytes: &[u8], ext: &str) -> String {
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

        // Unused word/media/imageN.<ext> part name — N unique across ALL media
        // parts regardless of extension, so a PNG + SVG pair minted together
        // (the mermaid embed case) never collide on the same N.
        let mut max_n = 0u32;
        for (name, _) in &self.parts {
            if let Some(rest) = name.strip_prefix("word/media/image") {
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<u32>() {
                    max_n = max_n.max(n);
                }
            }
        }
        let n = max_n + 1;
        let target = format!("image{n}.{ext}");
        let part_name = format!("word/media/{target}");
        self.parts.push((part_name, bytes.to_vec()));

        // Content-type Default for the extension, if not already declared.
        if let Some(b) = self.part("[Content_Types].xml") {
            let ct = String::from_utf8_lossy(b).into_owned();
            let marker = format!("Extension=\"{ext}\"");
            if !ct.contains(&marker) {
                let content_type = match ext {
                    "svg" => "image/svg+xml".to_string(),
                    other => format!("image/{other}"),
                };
                let default =
                    format!("<Default Extension=\"{ext}\" ContentType=\"{content_type}\"/>");
                let new_ct = ct.replacen("</Types>", &format!("{default}</Types>"), 1);
                self.set_part("[Content_Types].xml", new_ct.into_bytes());
            }
        }

        // Image relationship in document.xml.rels. If the part is missing
        // entirely (unusual, but not guaranteed present), create a minimal
        // valid one first — otherwise the `replacen` below is a no-op against
        // an empty string, `set_part` fails to find the part to update, and
        // the returned rId would be a dangling `r:embed` reference.
        const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
        let rels_name = "word/_rels/document.xml.rels";
        if self.part(rels_name).is_none() {
            let empty = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
                 <Relationships xmlns=\"{RELS_NS}\"></Relationships>"
            );
            self.parts.push((rels_name.to_string(), empty.into_bytes()));
        }
        let rels_xml = self
            .part(rels_name)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let rid = next_rid(&rels_xml);
        let rel =
            format!("<Relationship Id=\"{rid}\" Type=\"{R_NS}/image\" Target=\"media/{target}\"/>");
        let new_rels = rels_xml.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1);
        self.set_part(rels_name, new_rels.into_bytes());

        rid
    }

    /// Page size/margins from the captured (final) `sectPr` (US Letter default).
    pub fn page_geom(&self) -> crate::model::PageGeom {
        crate::model::PageGeom::from_sect_pr(self.sect_pr())
    }

    /// Set the page margins (twips) in the body section's `w:pgMar`, preserving
    /// any header/footer/gutter attributes. Creates the element if absent.
    pub fn set_page_margins(&mut self, top: i32, right: i32, bottom: i32, left: i32) {
        let mut s = self.sect_pr().to_string();
        if !s.contains("<w:pgMar") {
            let mar = format!(
                "<w:pgMar w:top=\"{top}\" w:right=\"{right}\" w:bottom=\"{bottom}\" w:left=\"{left}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>"
            );
            s = inject_sect_child(&s, &mar);
        } else {
            for (k, v) in [
                ("w:top", top),
                ("w:right", right),
                ("w:bottom", bottom),
                ("w:left", left),
            ] {
                s = set_pgmar_attr(&s, k, v);
            }
        }
        self.set_current_sect_pr_raw(s);
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
    ///
    /// Defines all 9 indent levels (`ilvl` 0..9, [`markdown_list_levels`]) — the
    /// same set [`new_markdown_package`] defines for a fresh markdown package —
    /// not just `ilvl=0`. A nested Markdown list (`- a\n  - b`) spliced into an
    /// *existing* package via this call references `ilvl=1`, `2`, etc.; with only
    /// `ilvl=0` defined, [`crate::numbering::Numbering::marker`] falls back to a
    /// stray decimal marker (or Word shows no marker at all) for any nested item.
    pub fn ensure_list(&mut self, bullet: bool) -> i32 {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        // Reserved high ids, unlikely to collide with a document's own lists.
        let (num_id, abs_id) = if bullet { (9990, 9990) } else { (9991, 9991) };
        let levels = markdown_list_levels(bullet);
        let abstract_xml =
            format!("<w:abstractNum w:abstractNumId=\"{abs_id}\">{levels}</w:abstractNum>");
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

/// The concatenated `<w:lvl>` XML for all 9 indent levels (`ilvl` 0..9) of a
/// bullet or decimal list — the depth Markdown lists can nest to. Shared by
/// [`new_markdown_package`] (defines the full set for a fresh markdown
/// package) and [`Package::ensure_list`] (defines the same set when splicing
/// into an existing package), so the two list definitions can never drift
/// apart — mirrors [`markdown_style_def`]'s role for styles.
fn markdown_list_levels(bullet: bool) -> String {
    let mut out = String::new();
    for lvl in 0..9 {
        if bullet {
            out.push_str(&format!(
                "<w:lvl w:ilvl=\"{lvl}\"><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"•\"/></w:lvl>"
            ));
        } else {
            out.push_str(&format!(
                "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/>\
                 <w:lvlText w:val=\"%{}.\"/></w:lvl>",
                lvl + 1
            ));
        }
    }
    out
}

/// The next free relationship id (`rId{max+1}`) for a `.rels` part.
fn next_rid(rels: &str) -> String {
    format!("rId{}", next_rid_num(rels))
}

/// Insert a child element as the first child of `<w:sectPr>` (creating/expanding
/// the element as needed). References must precede other section properties.
/// Replace (or add) a numeric attribute on the section's `<w:pgMar>` element.
fn set_pgmar_attr(sect: &str, key: &str, val: i32) -> String {
    let Some(ts) = sect.find("<w:pgMar") else {
        return sect.to_string();
    };
    let Some(rel) = sect[ts..].find('>') else {
        return sect.to_string();
    };
    let end = ts + rel; // index of '>'
    let el = &sect[ts..end]; // element without the closing '>'
    let k = format!("{key}=\"");
    let new_el = if let Some(ks) = el.find(&k) {
        let vs = ks + k.len();
        let ve = el[vs..].find('"').map(|e| vs + e).unwrap_or(vs);
        format!("{}{}{}", &el[..vs], val, &el[ve..])
    } else {
        let trimmed = el.trim_end_matches('/').trim_end();
        let slash = if el.trim_end().ends_with('/') {
            "/"
        } else {
            ""
        };
        format!("{trimmed} {key}=\"{val}\"{slash}")
    };
    format!("{}{}{}", &sect[..ts], new_el, &sect[end..])
}

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

/// Append a child just before `</w:sectPr>` (for elements like `<w:titlePg/>`
/// that belong near the end of `CT_SectPr`). Expands a self-closing sectPr.
fn append_sect_child(sect: &str, child: &str) -> String {
    if sect.is_empty() {
        return format!("<w:sectPr>{child}</w:sectPr>");
    }
    if let Some(gt) = sect.find('>') {
        if sect[..gt].ends_with('/') {
            return format!("{}>{child}</w:sectPr>", &sect[..gt - 1]);
        }
    }
    match sect.rfind("</w:sectPr>") {
        Some(i) => format!("{}{child}{}", &sect[..i], &sect[i..]),
        None => format!("{sect}{child}"),
    }
}

/// Insert `child` immediately after the `after` element (self-closing or with a
/// close tag). Falls back to appending before `</w:sectPr>` when `after` is
/// absent, keeping the child in a valid `CT_SectPr` position.
fn insert_after_element(sect: &str, after: &str, child: &str) -> String {
    let open = format!("<{after}");
    let Some(start) = sect.find(&open) else {
        return append_sect_child(sect, child);
    };
    let Some(rel_gt) = sect[start..].find('>') else {
        return append_sect_child(sect, child);
    };
    let gt = start + rel_gt;
    let end = if sect[..gt].ends_with('/') {
        gt + 1 // self-closing <after/>
    } else {
        let close = format!("</{after}>");
        match sect[gt..].find(&close) {
            Some(c) => gt + c + close.len(),
            None => gt + 1,
        }
    };
    format!("{}{child}{}", &sect[..end], &sect[end..])
}

/// Remove the first `<name/>`, `<name .../>`, or `<name ...>…</name>` element.
/// An attribute's value out of a raw tag body (`w:val="false"`), either quote
/// style. `None` when the attribute isn't there.
fn tag_attr(attrs: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=");
    let mut from = 0usize;
    while let Some(rel) = attrs[from..].find(&pat) {
        let at = from + rel;
        // `w:val=` must not be the tail of `w:someOtherVal=`.
        let ok = attrs[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        let rest = attrs[at + pat.len()..].trim_start();
        let q = rest.chars().next();
        if ok && matches!(q, Some('"') | Some('\'')) {
            let q = q.unwrap();
            let body = &rest[q.len_utf8()..];
            return body.find(q).map(|e| body[..e].to_string());
        }
        from = at + pat.len();
    }
    None
}

/// A boolean settings element's state in `xml`: `None` when absent, otherwise
/// its `w:val` (an absent `w:val` means on, per the OOXML on/off type).
///
/// A bare `contains("<w:autoHyphenation")` reads Word's explicit
/// `<w:autoHyphenation w:val="false"/>` as ON, and matches the unrelated
/// `<w:autoHyphenationZone>` too.
fn settings_flag_of(xml: &str, elem: &str) -> Option<bool> {
    let open = format!("<{elem}");
    let mut from = 0usize;
    while let Some(rel) = xml[from..].find(&open) {
        let start = from + rel;
        let after = start + open.len();
        let rest = &xml[after..];
        if rest.starts_with([' ', '/', '>', '\t', '\n', '\r']) {
            let end = rest.find('>').map(|e| after + e).unwrap_or(xml.len());
            let val = tag_attr(&xml[after..end], "w:val");
            return Some(!matches!(
                val.as_deref(),
                Some("false") | Some("0") | Some("off")
            ));
        }
        from = after;
    }
    None
}

fn remove_element(xml: &str, name: &str) -> String {
    let open = format!("<{name}");
    let Some(start) = xml.find(&open) else {
        return xml.to_string();
    };
    // Boundary check: the char after the name must end the tag name.
    let after = &xml[start + open.len()..];
    if !after.starts_with([' ', '/', '>', '\t', '\n', '\r']) {
        return xml.to_string();
    }
    let Some(rel_gt) = after.find('>') else {
        return xml.to_string();
    };
    let gt = start + open.len() + rel_gt;
    let end = if xml[..gt].ends_with('/') {
        gt + 1 // self-closing <name/>
    } else {
        let close = format!("</{name}>");
        match xml[gt..].find(&close) {
            Some(c) => gt + c + close.len(),
            None => gt + 1,
        }
    };
    let mut out = String::with_capacity(xml.len());
    out.push_str(&xml[..start]);
    out.push_str(&xml[end..]);
    out
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
    let document_xml = document_to_xml(&document);

    let parts = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document_xml.into_bytes()),
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

    let bullets = markdown_list_levels(true);
    let decimals = markdown_list_levels(false);
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
        document_root_attrs(&original_doc, &xml),
        xml.find("<w:document"),
        xml.find("<w:body>"),
    ) {
        xml = format!(
            "{}<w:document {attrs}>{}",
            &xml[..doc_pos],
            &xml[body_pos..]
        );
    }
    if document.trailing_section_properties().is_none() && !pkg.sect_pr.is_empty() {
        xml = xml.replacen("</w:body>", &format!("{}</w:body>", pkg.sect_pr), 1);
    }
    parts[pkg.doc_index].1 = xml.into_bytes();
    write_zip(&parts)
}

/// Serialize a package without regenerating its main document part. This is the
/// correct same-format save path when the live document has no user-authorized
/// edits: all original OOXML wrappers and cached field results remain byte-for-
/// byte intact while the container itself may be rewritten.
pub fn save_package_preserving_document(pkg: &Package) -> Vec<u8> {
    write_zip(&pkg.parts)
}

/// Merge the original `<w:document …>` attributes with every declaration the
/// semantic serializer requires. Original files often carry additional
/// namespace bindings used by preserved raw XML, while freshly-created package
/// roots carry none. `mc:Ignorable` is token-valued and therefore needs a union
/// rather than first-writer-wins behavior.
fn document_root_attrs(original: &str, generated: &str) -> Option<String> {
    let mut attrs = xml_root_attrs(original, "w:document")?;
    let generated_attrs = xml_root_attrs(generated, "w:document")?;
    for (name, value) in &generated_attrs {
        if name == "xmlns" || name.starts_with("xmlns:") {
            if !attrs.iter().any(|(key, _)| key == name) {
                attrs.push((name.clone(), value.clone()));
            }
            continue;
        }

        let existing_index = attrs.iter().enumerate().find_map(|(index, (key, _))| {
            same_expanded_attribute(&attrs, key, &generated_attrs, name).then_some(index)
        });
        let local_name = name
            .split_once(':')
            .map_or(name.as_str(), |(_, local)| local);
        if local_name == "Ignorable"
            && (existing_index.is_some()
                || attribute_namespace(&generated_attrs, name)
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006"))
        {
            if let Some(index) = existing_index {
                let existing = &mut attrs[index].1;
                for token in value.split_whitespace() {
                    if !existing.split_whitespace().any(|item| item == token) {
                        if !existing.is_empty() {
                            existing.push(' ');
                        }
                        existing.push_str(token);
                    }
                }
            } else {
                attrs.push((name.clone(), value.clone()));
            }
        } else if existing_index.is_none() {
            attrs.push((name.clone(), value.clone()));
        }
    }

    Some(
        attrs
            .into_iter()
            .map(|(name, value)| format!("{name}=\"{}\"", esc_xml_attr(&value)))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn attribute_namespace<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let (prefix, _) = name.split_once(':')?;
    attrs.iter().find_map(|(declaration, value)| {
        (declaration.strip_prefix("xmlns:") == Some(prefix)).then_some(value.as_str())
    })
}

fn same_expanded_attribute(
    left_attrs: &[(String, String)],
    left_name: &str,
    right_attrs: &[(String, String)],
    right_name: &str,
) -> bool {
    let left_local = left_name
        .split_once(':')
        .map_or(left_name, |(_, local)| local);
    let right_local = right_name
        .split_once(':')
        .map_or(right_name, |(_, local)| local);
    left_local == right_local
        && attribute_namespace(left_attrs, left_name)
            == attribute_namespace(right_attrs, right_name)
}

fn xml_root_attrs(xml: &str, root_name: &str) -> Option<Vec<(String, String)>> {
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Start if parser.name() == root_name => {
                return Some(
                    parser
                        .attrs()
                        .iter()
                        .map(|attr| {
                            let mut value = String::new();
                            XmlParser::append_decoded(attr.value, &mut value);
                            (attr.name.to_string(), value)
                        })
                        .collect(),
                );
            }
            Event::Eof => return None,
            _ => {}
        }
    }
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
            Block::SectionProperties(_) | Block::Raw(_) => {}
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

    /// Word writes an explicit off as `w:val="false"`, which a bare `contains`
    /// reads as ON — and then `set_settings_flag(.., true)` no-ops because "the
    /// tag is already there", so the toggle looks dead until pressed twice.
    #[test]
    fn settings_flag_reads_w_val() {
        let f = |x: &str| settings_flag_of(x, "w:autoHyphenation");
        assert_eq!(f("<w:settings/>"), None);
        assert_eq!(
            f("<w:settings><w:autoHyphenation/></w:settings>"),
            Some(true)
        );
        assert_eq!(
            f(r#"<w:settings><w:autoHyphenation w:val="false"/></w:settings>"#),
            Some(false)
        );
        assert_eq!(
            f(r#"<w:settings><w:autoHyphenation w:val="0"/></w:settings>"#),
            Some(false)
        );
        assert_eq!(
            f(r#"<w:settings><w:autoHyphenation w:val="true"/></w:settings>"#),
            Some(true)
        );
        // A longer element that merely starts with the same name is not it.
        assert_eq!(
            f("<w:settings><w:autoHyphenationZone>0</w:autoHyphenationZone></w:settings>"),
            None
        );
    }

    /// Turning a flag on has to REPLACE an explicit `w:val="false"`, and a
    /// self-closing `<w:settings/>` root must be opened rather than having a
    /// second root element appended after it.
    #[test]
    fn set_settings_flag_replaces_explicit_off_and_opens_empty_root() {
        let mut p = load_package(&make_docx("<w:document/>")).unwrap();
        p.set_part(
            "word/settings.xml",
            br#"<w:settings xmlns:w="w"><w:autoHyphenation w:val="false"/></w:settings>"#.to_vec(),
        );
        assert!(!p.has_auto_hyphenation());
        p.set_auto_hyphenation(true);
        assert!(p.has_auto_hyphenation());
        let xml = String::from_utf8(p.part("word/settings.xml").unwrap().to_vec()).unwrap();
        assert!(
            !xml.contains("w:val=\"false\""),
            "stale off left behind: {xml}"
        );
        p.set_auto_hyphenation(false);
        assert!(!p.has_auto_hyphenation());

        // Self-closing root: the flag lands INSIDE it, not after it.
        p.set_part(
            "word/settings.xml",
            br#"<w:settings xmlns:w="w"/>"#.to_vec(),
        );
        p.set_even_odd(true);
        let xml = String::from_utf8(p.part("word/settings.xml").unwrap().to_vec()).unwrap();
        assert_eq!(
            xml,
            r#"<w:settings xmlns:w="w"><w:evenAndOddHeaders/></w:settings>"#
        );
        assert!(p.has_even_odd());
    }

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

    fn make_metadata_docx(
        document_xml: &str,
        settings_xml: Option<&str>,
        document_rels_xml: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Vec<u8> {
        let mut parts = vec![
            (
                "[Content_Types].xml".to_string(),
                br#"<?xml version="1.0"?><Types/>"#.to_vec(),
            ),
            (
                "_rels/.rels".to_string(),
                br#"<?xml version="1.0"?><Relationships/>"#.to_vec(),
            ),
            (
                "word/document.xml".to_string(),
                document_xml.as_bytes().to_vec(),
            ),
            (
                "word/styles.xml".to_string(),
                br#"<?xml version="1.0"?><w:styles/>"#.to_vec(),
            ),
        ];
        if let Some(settings) = settings_xml {
            parts.push((
                "word/settings.xml".to_string(),
                settings.as_bytes().to_vec(),
            ));
        }
        if let Some(rels) = document_rels_xml {
            parts.push((
                "word/_rels/document.xml.rels".to_string(),
                rels.as_bytes().to_vec(),
            ));
        }
        parts.extend(
            headers
                .iter()
                .map(|(name, xml)| (format!("word/{name}"), xml.as_bytes().to_vec())),
        );
        write_zip(&parts)
    }

    const BODY: &str = "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
        <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello</w:t></w:r></w:p>\
        <w:p><w:r><w:t>World</w:t></w:r></w:p>\
        <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr>\
        </w:body></w:document>";

    #[test]
    fn protection_boolean_lexical_forms_and_enforcement_defaults() {
        for value in ["1", "true", "on", "TRUE", "ON", " true ", "\tON\r\n"] {
            let protection = parse_protection(&format!(
                r#"<w:settings><w:documentProtection w:edit="readOnly" w:enforcement="{value}"/></w:settings>"#
            ));
            assert_eq!(
                protection.enforcement,
                ProtectionEnforcement::Enforced,
                "{value}"
            );
            assert!(protection.is_enforced(), "{value}");
        }
        for value in ["0", "false", "off", "FALSE", "OFF"] {
            let protection = parse_protection(&format!(
                r#"<w:settings><w:documentProtection w:edit="readOnly" w:enforcement="{value}"/></w:settings>"#
            ));
            assert_eq!(
                protection.enforcement,
                ProtectionEnforcement::Disabled,
                "{value}"
            );
            assert!(!protection.is_enforced(), "{value}");
            assert_eq!(protection.label(), None, "{value}");
        }

        for value in ["", "maybe", "2"] {
            let protection = parse_protection(&format!(
                r#"<w:settings><w:documentProtection w:edit="readOnly" w:enforcement="{value}"/></w:settings>"#
            ));
            assert_eq!(
                protection.enforcement,
                ProtectionEnforcement::Enforced,
                "{value}"
            );
            assert!(protection.is_enforced(), "{value}");
            assert!(matches!(
                protection.edit_mode,
                Some(ProtectionEditMode::Unknown(ref reason))
                    if reason == "invalid enforcement value"
            ));
            assert_eq!(protection.label(), Some("restricted editing"), "{value}");
        }

        let absent = parse_protection(
            r#"<w:settings><w:documentProtection w:edit="readOnly"/></w:settings>"#,
        );
        assert_eq!(absent.enforcement, ProtectionEnforcement::Absent);
        assert!(!absent.is_enforced());
        assert_eq!(absent.label(), None);
        assert!(absent.source.document_protection_present);
        assert_eq!(absent.source.enforcement_value, None);

        let no_declaration = parse_protection("<w:settings/>");
        assert_eq!(no_declaration.enforcement, ProtectionEnforcement::Absent);
        assert!(!no_declaration.source.document_protection_present);
    }

    #[test]
    fn protection_models_every_edit_mode_and_formatting_only() {
        let cases = [
            ("none", ProtectionEditMode::Unrestricted, None),
            ("readOnly", ProtectionEditMode::ReadOnly, Some("read-only")),
            (
                "comments",
                ProtectionEditMode::Comments,
                Some("comments only"),
            ),
            (
                "trackedChanges",
                ProtectionEditMode::TrackedChanges,
                Some("tracked changes only"),
            ),
            ("forms", ProtectionEditMode::Forms, Some("form fields only")),
        ];
        for (value, expected_mode, expected_label) in cases {
            let protection = parse_protection(&format!(
                r#"<w:settings><w:documentProtection w:enforcement="1" w:edit="{value}"/></w:settings>"#
            ));
            assert_eq!(protection.edit_mode, Some(expected_mode), "{value}");
            assert_eq!(protection.label(), expected_label, "{value}");
            assert_eq!(protection.source.edit_value.as_deref(), Some(value));
        }

        for value in ["1", "true", "on", " true "] {
            let protection = parse_protection(&format!(
                r#"<w:settings><w:documentProtection w:enforcement="1" w:formatting="{value}"/></w:settings>"#
            ));
            assert!(protection.formatting_locked, "{value}");
            assert_eq!(protection.label(), Some("formatting locked"));
        }
        let unlocked = parse_protection(
            r#"<w:settings><w:documentProtection w:enforcement="1" w:formatting="off"/></w:settings>"#,
        );
        assert!(!unlocked.formatting_locked);

        let invalid = parse_protection(
            r#"<w:settings><w:documentProtection w:enforcement="1" w:formatting="maybe"/></w:settings>"#,
        );
        assert!(invalid.is_enforced());
        assert!(!invalid.formatting_locked);
        assert!(matches!(
            invalid.edit_mode,
            Some(ProtectionEditMode::Unknown(ref reason))
                if reason == "invalid formatting value"
        ));
        assert_eq!(invalid.label(), Some("restricted editing"));
    }

    #[test]
    fn write_protection_is_retained_as_advisory_metadata() {
        let protection = parse_protection(
            r#"<w:settings><w:documentProtection w:edit="readOnly" w:enforcement="0"/><w:writeProtection w:recommended="true"/></w:settings>"#,
        );
        assert!(!protection.is_enforced());
        assert!(protection.advisory_write_protection);
        assert!(protection.source.write_protection_present);
        assert_eq!(
            protection.source.write_recommended_value.as_deref(),
            Some("true")
        );
        assert_eq!(protection.label(), Some("read-only (recommended)"));

        for xml in [
            r#"<w:settings><w:writeProtection w:recommended="false"/></w:settings>"#,
            r#"<w:settings><w:writeProtection/></w:settings>"#,
        ] {
            let protection = parse_protection(xml);
            assert!(protection.source.write_protection_present);
            assert!(!protection.advisory_write_protection, "{xml}");
            assert_eq!(protection.label(), None, "{xml}");
        }
    }

    #[test]
    fn password_backed_write_protection_is_enforced_read_only() {
        for credential in [
            r#"w:password="ABCD""#,
            r#"w:hash="YWJjZA==""#,
            r#"w:hashValue="YWJjZA==""#,
        ] {
            let protection = parse_protection(&format!(
                r#"<w:settings><w:writeProtection w:recommended="true" {credential}/></w:settings>"#
            ));
            assert!(protection.is_enforced(), "{credential}");
            assert!(protection.enforced_write_protection, "{credential}");
            assert!(!protection.advisory_write_protection, "{credential}");
            assert!(protection.source.write_credential_present, "{credential}");
            assert_eq!(protection.label(), Some("read-only"), "{credential}");
        }
    }

    #[test]
    fn protection_uses_wordprocessingml_namespaces_and_cannot_be_downgraded() {
        let protection = parse_protection(
            r#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                    xmlns:ext="urn:example-extension">
                <w:documentProtection w:edit="readOnly" w:enforcement="1"/>
                <ext:documentProtection ext:edit="none" ext:enforcement="0"/>
                <w:writeProtection w:password="ABCD"/>
                <ext:writeProtection ext:recommended="false"/>
            </w:settings>"#,
        );
        assert!(protection.is_enforced());
        assert_eq!(protection.edit_mode, Some(ProtectionEditMode::ReadOnly));
        assert!(protection.enforced_write_protection);

        let spoofed_attributes = parse_protection(
            r#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                    xmlns:ext="urn:example-extension">
                <w:documentProtection ext:edit="none" ext:enforcement="0"
                    w:edit="comments" w:enforcement="1"/>
            </w:settings>"#,
        );
        assert!(spoofed_attributes.is_enforced());
        assert_eq!(
            spoofed_attributes.edit_mode,
            Some(ProtectionEditMode::Comments)
        );

        let alternate_prefix = parse_protection(
            r#"<x:settings xmlns:x="http://purl.oclc.org/ooxml/wordprocessingml/main">
                <x:documentProtection x:edit="readOnly" x:enforcement="true"/>
            </x:settings>"#,
        );
        assert!(alternate_prefix.is_enforced());
        assert_eq!(
            alternate_prefix.edit_mode,
            Some(ProtectionEditMode::ReadOnly)
        );

        let duplicate = parse_protection(
            r#"<w:settings>
                <w:documentProtection w:edit="readOnly" w:enforcement="1"/>
                <w:documentProtection w:edit="none" w:enforcement="0"/>
            </w:settings>"#,
        );
        assert!(duplicate.is_enforced());
        assert_eq!(duplicate.edit_mode, Some(ProtectionEditMode::ReadOnly));
    }

    #[test]
    fn protection_resolves_a_nonstandard_related_settings_part_and_fails_closed_if_missing() {
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdSettings" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="metadata/settings2.xml"/></Relationships>"#;
        let bytes = make_metadata_docx(BODY, None, Some(rels), &[]);
        let mut pkg = load_package(&bytes).expect("load");
        pkg.parts.push((
            "word/metadata/settings2.xml".to_string(),
            br#"<w:settings><w:documentProtection w:edit="readOnly" w:enforcement="1"/></w:settings>"#
                .to_vec(),
        ));

        assert_eq!(pkg.protection_label(), Some("read-only"));
        pkg.parts
            .retain(|(name, _)| name != "word/metadata/settings2.xml");
        let missing = pkg.protection();
        assert!(missing.is_enforced());
        assert!(matches!(
            missing.edit_mode,
            Some(ProtectionEditMode::Unknown(ref reason)) if reason == "missing related settings part"
        ));
    }

    #[test]
    fn protection_accepts_only_unambiguous_official_settings_relationships() {
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rIdCustom" Type="urn:vendor/settings" Target="metadata/unprotected.xml"/>
            <Relationship Id="rIdSettings" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>
            </Relationships>"#;
        let protected = r#"<w:settings><w:documentProtection w:edit="readOnly" w:enforcement="1"/></w:settings>"#;
        let bytes = make_metadata_docx(
            BODY,
            Some(protected),
            Some(rels),
            &[("metadata/unprotected.xml", "<w:settings/>")],
        );
        let pkg = load_package(&bytes).expect("load");
        assert_eq!(pkg.protection_label(), Some("read-only"));

        let strict_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rIdSettings" Type="http://purl.oclc.org/ooxml/officeDocument/relationships/settings" Target="metadata/settings2.xml"/>
            </Relationships>"#;
        let bytes = make_metadata_docx(
            BODY,
            None,
            Some(strict_rels),
            &[("metadata/settings2.xml", protected)],
        );
        let pkg = load_package(&bytes).expect("load");
        assert_eq!(pkg.protection_label(), Some("read-only"));

        let duplicate_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rIdSettings1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>
            <Relationship Id="rIdSettings2" Type="http://purl.oclc.org/ooxml/officeDocument/relationships/settings" Target="metadata/unprotected.xml"/>
            </Relationships>"#;
        let bytes = make_metadata_docx(
            BODY,
            Some(protected),
            Some(duplicate_rels),
            &[("metadata/unprotected.xml", "<w:settings/>")],
        );
        let duplicate = load_package(&bytes).expect("load").protection();
        assert!(duplicate.is_enforced());
        assert!(matches!(
            duplicate.edit_mode,
            Some(ProtectionEditMode::Unknown(ref reason))
                if reason == "invalid settings relationship"
        ));

        let wrong_namespace = r#"<Relationships xmlns="urn:vendor-relationships">
            <Relationship Id="rIdSettings" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="metadata/unprotected.xml"/>
            </Relationships>"#;
        let bytes = make_metadata_docx(
            BODY,
            Some(protected),
            Some(wrong_namespace),
            &[("metadata/unprotected.xml", "<w:settings/>")],
        );
        let invalid = load_package(&bytes).expect("load").protection();
        assert!(invalid.is_enforced());
        assert!(matches!(
            invalid.edit_mode,
            Some(ProtectionEditMode::Unknown(ref reason))
                if reason == "invalid settings relationship"
        ));
    }

    #[test]
    fn protection_decodes_utf16_settings_and_fails_closed_when_unreadable() {
        fn utf16le(xml: &str) -> Vec<u8> {
            [0xff, 0xfe]
                .into_iter()
                .chain(xml.encode_utf16().flat_map(u16::to_le_bytes))
                .collect()
        }

        let bytes = make_metadata_docx(BODY, Some("<w:settings/>"), None, &[]);
        let mut pkg = load_package(&bytes).expect("load");
        assert!(pkg.set_part(
            "word/settings.xml",
            utf16le(
                r#"<?xml version="1.0" encoding="UTF-16"?><w:settings><w:documentProtection w:edit="readOnly" w:enforcement="1"/></w:settings>"#,
            ),
        ));
        assert_eq!(
            pkg.protection().edit_mode,
            Some(ProtectionEditMode::ReadOnly)
        );
        assert_eq!(pkg.protection_label(), Some("read-only"));

        assert!(pkg.set_part("word/settings.xml", vec![0xff, 0xfe, 0x00]));
        let unreadable = pkg.protection();
        assert!(unreadable.is_enforced());
        assert!(matches!(
            unreadable.edit_mode,
            Some(ProtectionEditMode::Unknown(ref mode)) if mode == "unreadable settings XML"
        ));
    }

    #[test]
    fn watermarks_decode_text_and_follow_section_header_inheritance() {
        let document = r#"<?xml version="1.0"?><w:document xmlns:w="w" xmlns:r="r"><w:body>
            <w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/></w:sectPr></w:pPr><w:r><w:t>Section one</w:t></w:r></w:p>
            <w:p><w:r><w:t>Section two</w:t></w:r></w:p><w:sectPr/>
            </w:body></w:document>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdHeader" Target="header1.xml"/></Relationships>"#;
        let header = r#"<w:hdr xmlns:w="w" xmlns:v="v"><w:p><w:r><w:pict>
            <v:shape id="PowerPlusWaterMarkObject"><v:textpath string="CONFIDENTIAL &amp; DRAFT &#x2014; &#65;"/></v:shape>
            </w:pict></w:r></w:p></w:hdr>"#;
        let bytes = make_metadata_docx(document, None, Some(rels), &[("header1.xml", header)]);
        let pkg = load_package(&bytes).expect("load");
        let watermarks = pkg.watermarks();
        assert_eq!(watermarks.len(), 2);
        assert_eq!(
            watermarks[0].kind,
            WatermarkKind::Text("CONFIDENTIAL & DRAFT — A".to_string())
        );
        assert_eq!(watermarks[0].header.section_index, 0);
        assert_eq!(watermarks[0].header.variant, HeaderVariant::Default);
        assert!(!watermarks[0].header.inherited);
        assert_eq!(watermarks[0].header.relationship_id, "rIdHeader");
        assert_eq!(watermarks[0].header.part_name, "word/header1.xml");
        assert_eq!(watermarks[1].header.section_index, 1);
        assert!(watermarks[1].header.inherited);
        assert_eq!(
            pkg.watermark_label().as_deref(),
            Some("CONFIDENTIAL & DRAFT — A")
        );

        // An orphan header part is metadata, not an applied document watermark.
        let orphan = make_metadata_docx(BODY, None, None, &[("header1.xml", header)]);
        assert!(load_package(&orphan).unwrap().watermarks().is_empty());
    }

    #[test]
    fn watermark_header_variants_and_picture_fallback_are_structured() {
        let document = r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p/><w:sectPr>
            <w:headerReference w:type="default" r:id="rDefault"/>
            <w:headerReference w:type="first" r:id="rFirst"/>
            <w:headerReference w:type="even" r:id="rEven"/><w:titlePg/>
            </w:sectPr></w:body></w:document>"#;
        let settings = r#"<w:settings><w:evenAndOddHeaders w:val="on"/></w:settings>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rDefault" Target="header1.xml"/>
            <Relationship Id="rFirst" Target="header2.xml"/>
            <Relationship Id="rEven" Target="header3.xml"/>
            </Relationships>"#;
        let text = |value: &str| {
            format!(
                r#"<w:hdr xmlns:w="w" xmlns:v="v"><v:shape id="PowerPlusWaterMarkObject"><v:textpath string="{value}"/></v:shape></w:hdr>"#
            )
        };
        let default = text("DEFAULT");
        let first = text("FIRST");
        let picture = r#"<w:hdr xmlns:w="w" xmlns:v="v"><v:shape id="PowerPlusWaterMarkObject42"><v:imagedata r:id="rImage"/></v:shape></w:hdr>"#;
        let bytes = make_metadata_docx(
            document,
            Some(settings),
            Some(rels),
            &[
                ("header1.xml", &default),
                ("header2.xml", &first),
                ("header3.xml", picture),
            ],
        );
        let pkg = load_package(&bytes).unwrap();
        let watermarks = pkg.watermarks();
        assert_eq!(watermarks.len(), 3);
        assert!(watermarks.iter().any(|w| {
            w.header.variant == HeaderVariant::Default
                && w.kind == WatermarkKind::Text("DEFAULT".to_string())
        }));
        assert!(watermarks.iter().any(|w| {
            w.header.variant == HeaderVariant::First
                && w.kind == WatermarkKind::Text("FIRST".to_string())
        }));
        assert!(watermarks.iter().any(|w| {
            w.header.variant == HeaderVariant::Even && w.kind == WatermarkKind::Picture
        }));

        assert_eq!(
            watermark_kinds(r#"<v:shape id="PowerPlusWaterMarkObject"><v:textpath/></v:shape>"#),
            vec![WatermarkKind::Unknown]
        );
        let only_picture = make_metadata_docx(
            document,
            Some(settings),
            Some(rels),
            &[
                ("header1.xml", picture),
                ("header2.xml", picture),
                ("header3.xml", picture),
            ],
        );
        assert_eq!(
            load_package(&only_picture)
                .unwrap()
                .watermark_label()
                .as_deref(),
            Some("picture (preview unavailable)")
        );
    }

    #[test]
    fn watermark_detection_requires_a_marker_and_covers_drawingml_pictures() {
        assert!(watermark_kinds(
            r#"<v:shape id="DecorativeWordArt"><v:textpath string="Quarterly report"/></v:shape>"#
        )
        .is_empty());
        assert!(watermark_kinds(r#"<v:textpath string="Quarterly report"/>"#).is_empty());
        assert_eq!(
            watermark_kinds(r#"<wp:docPr id="7" name="Watermark picture"/>"#),
            vec![WatermarkKind::Picture]
        );

        let document = r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p/><w:sectPr><w:headerReference w:type="default" r:id="rHeader"/></w:sectPr></w:body></w:document>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rHeader" Target="header1.xml"/></Relationships>"#;
        let ordinary = make_metadata_docx(
            document,
            None,
            Some(rels),
            &[(
                "header1.xml",
                r#"<w:hdr xmlns:w="w" xmlns:v="v"><v:shape id="DecorativeWordArt"><v:textpath string="Quarterly report"/></v:shape></w:hdr>"#,
            )],
        );
        assert!(load_package(&ordinary).unwrap().watermarks().is_empty());

        let drawing = make_metadata_docx(
            document,
            None,
            Some(rels),
            &[(
                "header1.xml",
                r#"<w:hdr xmlns:w="w" xmlns:wp="wp"><wp:docPr id="7" name="Watermark picture"/></w:hdr>"#,
            )],
        );
        assert_eq!(
            load_package(&drawing).unwrap().watermarks()[0].kind,
            WatermarkKind::Picture
        );
    }

    #[test]
    fn surfaces_structured_metadata_and_page_borders() {
        let document = "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
            <w:p><w:r><w:t>Body</w:t></w:r></w:p>\
            <w:sectPr><w:pgBorders w:offsetFrom=\"page\"><w:top w:val=\"single\"/></w:pgBorders>\
            <w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:body></w:document>";
        let settings = "<?xml version=\"1.0\"?><w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
            <w:documentProtection w:edit=\"readOnly\" w:enforcement=\"1\"/></w:settings>";
        let bytes = make_metadata_docx(document, Some(settings), None, &[]);
        let pkg = load_package(&bytes).expect("load");
        assert_eq!(
            pkg.protection().edit_mode,
            Some(ProtectionEditMode::ReadOnly)
        );
        assert_eq!(pkg.protection_label(), Some("read-only"));
        assert!(pkg.has_page_borders());

        let plain = load_package(&make_docx(BODY)).expect("load");
        assert_eq!(plain.protection(), Protection::default());
        assert_eq!(plain.protection_label(), None);
        assert!(plain.watermarks().is_empty());
        assert_eq!(plain.watermark_label(), None);
        assert!(!plain.has_page_borders());
    }

    #[test]
    fn create_header_from_scratch_wires_everything() {
        use crate::model::{Block, Document, Paragraph};
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        let name = pkg.create_hf(true, "default").expect("created header");
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
        let name2 = pkg2.create_hf(false, "default").expect("created footer");
        assert_eq!(name2, "word/footer1.xml");
        assert!(pkg2.sect_pr().contains("footerReference"));
    }

    #[test]
    fn columns_and_hyphenation_round_trip() {
        use crate::model::{Block, Document, Paragraph};
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        assert_eq!(pkg.columns(), 1);
        pkg.set_columns(2);
        assert_eq!(pkg.columns(), 2);
        assert!(pkg.sect_pr().contains("w:num=\"2\""));
        // Changing again replaces (not duplicates) the cols element.
        pkg.set_columns(3);
        assert_eq!(pkg.sect_pr().matches("<w:cols").count(), 1);
        assert_eq!(pkg.columns(), 3);
        pkg.set_columns(1);
        assert_eq!(pkg.columns(), 1);

        assert!(!pkg.has_auto_hyphenation());
        pkg.set_auto_hyphenation(true);
        assert!(pkg.has_auto_hyphenation());
        assert!(pkg.part("word/settings.xml").is_some());
        pkg.set_auto_hyphenation(false);
        assert!(!pkg.has_auto_hyphenation());

        let bytes = save_package(&pkg);
        let re = load_package(&bytes).expect("reload");
        assert_eq!(re.page_geom().cols, 1);
    }

    #[test]
    fn first_page_and_even_odd_toggles() {
        use crate::model::{Block, Document, Paragraph};
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        // First-page header/footer.
        assert!(!pkg.has_title_pg());
        pkg.set_title_pg(true);
        assert!(pkg.has_title_pg() && pkg.sect_pr().contains("<w:titlePg/>"));
        pkg.set_title_pg(true); // idempotent
        assert_eq!(pkg.sect_pr().matches("<w:titlePg").count(), 1);
        let first = pkg.create_hf(true, "first").expect("first header");
        assert!(pkg.sect_pr().contains("w:type=\"first\""));
        assert!(
            crate::load::header_footer_ref_rid(pkg.sect_pr(), "headerReference", "first").is_some()
        );
        pkg.set_title_pg(false);
        assert!(!pkg.has_title_pg() && !pkg.sect_pr().contains("titlePg"));
        assert!(
            pkg.part(&first).is_some(),
            "first part kept when toggled off"
        );

        // Even/odd headers (creates settings.xml from scratch here).
        assert!(!pkg.has_even_odd());
        pkg.set_even_odd(true);
        assert!(pkg.has_even_odd());
        assert!(pkg.part("word/settings.xml").is_some());
        pkg.create_hf(true, "even").expect("even header");
        assert!(pkg.sect_pr().contains("w:type=\"even\""));
        pkg.set_even_odd(false);
        assert!(!pkg.has_even_odd());

        // Everything still saves + reloads cleanly.
        let bytes = save_package(&pkg);
        assert!(load_package(&bytes).is_ok());
    }

    #[test]
    fn roundtrip_preserves_model_parts_and_sectpr() {
        let docx = make_docx(BODY);
        let pkg1 = load_package(&docx).expect("load");
        // The model captures both paragraphs plus the trailing section properties.
        assert_eq!(pkg1.document.body.len(), 3);

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
    fn ensure_list_defines_all_nine_levels_so_nested_items_get_real_markers() {
        use crate::markdown::from_markdown;
        use crate::model::Paragraph;
        use crate::numbering::{compute_markers, parse_numbering_xml};

        // Splice a nested bullet list into a plain, non-markdown-created package
        // — exactly what `docxy::control::prepare_markdown_blocks` does.
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        let parsed = from_markdown("- a\n  - b");
        let bullet_id = pkg.ensure_list(true);

        // The numbering part must define ilvl=1 (and beyond), not just ilvl=0 —
        // Word/`compute_markers` fall back to a plain decimal for an undefined
        // level, which is the bug this fix closes.
        let numbering =
            String::from_utf8_lossy(pkg.part("word/numbering.xml").unwrap()).into_owned();
        assert!(
            numbering.contains("w:ilvl=\"1\""),
            "ensure_list must define ilvl=1 for nested lists: {numbering}"
        );
        for lvl in 0..9 {
            assert!(
                numbering.contains(&format!("w:ilvl=\"{lvl}\"")),
                "missing level {lvl}: {numbering}"
            );
        }

        // Remap the parsed blocks' bare markdown numId (1) onto the reserved
        // ensure_list id, same as `prepare_markdown_blocks` does, then verify
        // the TUI marker path renders a real bullet — not a stray "1." — for
        // the nested (ilvl=1) item.
        let mut body = parsed.body;
        for b in body.iter_mut() {
            if let Block::Paragraph(p) = b {
                if p.props.num_id == Some(1) {
                    p.props.num_id = Some(bullet_id);
                }
            }
        }
        let nested_ilvl = body
            .iter()
            .find_map(|b| match b {
                Block::Paragraph(p) if p.props.num_id == Some(bullet_id) && p.props.ilvl > 0 => {
                    Some(p.props.ilvl)
                }
                _ => None,
            })
            .expect("markdown source has a nested (ilvl>0) item");
        assert_eq!(nested_ilvl, 1);

        let doc = Document { body };
        let num = parse_numbering_xml(&numbering);
        let markers = compute_markers(&doc, &num);
        let nested_marker = doc
            .body
            .iter()
            .enumerate()
            .find_map(|(i, b)| match b {
                Block::Paragraph(p) if p.props.ilvl == 1 => markers.get(&vec![i]).cloned(),
                _ => None,
            })
            .expect("nested item must get a marker");
        assert_eq!(
            nested_marker, "◦",
            "nested bullet must render its own marker char, not a decimal fallback"
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
            ..Hyperlink::default()
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
    fn add_media_part_avoids_collisions_with_existing_media_and_rids() {
        let mut pkg = new_package(Document { body: vec![] });

        // Pre-existing media part (image1.png) that must not be overwritten.
        pkg.parts.push((
            "word/media/image1.png".to_string(),
            vec![0xDE, 0xAD, 0xBE, 0xEF],
        ));

        // A document.xml.rels that already carries a relationship using
        // "rId5" — the new rId must not collide with (or reuse) it.
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/preexisting.png"/></Relationships>"#;
        pkg.set_part("word/_rels/document.xml.rels", rels.as_bytes().to_vec());

        let existing_rids = ["rId1", "rId5"]; // rId1: new_package's styles rel

        let rid = pkg.add_media_part(&[1, 2, 3], "png");

        // (a) a fresh rId, not reusing any existing one.
        assert!(
            !existing_rids.contains(&rid.as_str()),
            "add_media_part must mint a fresh rId, got reused {rid}"
        );

        // (b) image1.png untouched; the new part landed at image2.png.
        assert_eq!(
            pkg.part("word/media/image1.png"),
            Some([0xDE, 0xAD, 0xBE, 0xEF].as_slice()),
            "add_media_part must not overwrite pre-existing media"
        );
        assert_eq!(
            pkg.part("word/media/image2.png"),
            Some([1u8, 2, 3].as_slice()),
            "add_media_part must add the new part under the next free name"
        );

        // (c) the rels part now carries BOTH the old rId5 relationship and
        // the newly minted one — not one replacing the other.
        let rels_after =
            String::from_utf8_lossy(pkg.part("word/_rels/document.xml.rels").unwrap()).into_owned();
        assert!(
            rels_after.contains("Id=\"rId5\"") && rels_after.contains("media/preexisting.png"),
            "pre-existing relationship lost: {rels_after}"
        );
        assert!(
            rels_after.contains(&format!("Id=\"{rid}\""))
                && rels_after.contains("media/image2.png"),
            "new relationship missing: {rels_after}"
        );
    }

    #[test]
    fn add_media_part_creates_rels_part_when_absent() {
        // A package with no word/_rels/document.xml.rels at all (defensive
        // gap: add_media_part must not silently produce a dangling rId).
        let mut pkg = new_package(Document { body: vec![] });
        assert!(pkg.set_part("word/_rels/document.xml.rels", Vec::new())); // sanity: part exists pre-removal
        pkg.parts
            .retain(|(n, _)| n != "word/_rels/document.xml.rels");
        assert!(pkg.part("word/_rels/document.xml.rels").is_none());

        let rid = pkg.add_media_part(&[9, 9, 9], "png");

        let rels = pkg
            .part("word/_rels/document.xml.rels")
            .expect("add_media_part must create the rels part if missing");
        let rels_xml = String::from_utf8_lossy(rels).into_owned();
        assert!(
            rels_xml.contains(&format!("Id=\"{rid}\"")) && rels_xml.contains("media/image1.png"),
            "new relationship missing from freshly-created rels part: {rels_xml}"
        );
        assert!(pkg.part("word/media/image1.png").is_some());
    }

    #[test]
    fn new_package_keeps_repeating_section_namespaces_on_save() {
        use crate::model::{Table, TableRowBoundary};

        let document = Document {
            body: vec![Block::Table(Table {
                row_boundaries: vec![
                    TableRowBoundary::sdt_open(
                        0,
                        "<w:sdt><w:sdtPr><w15:repeatingSection/></w:sdtPr><w:sdtContent>",
                    ),
                    TableRowBoundary::sdt_close(0, "</w:sdtContent></w:sdt>"),
                ],
                ..Default::default()
            })],
        };
        let bytes = save_package(&new_package(document));
        let reloaded = load_package(&bytes).expect("reload repeating-section package");
        let doc_xml = String::from_utf8_lossy(reloaded.part("word/document.xml").unwrap());
        assert!(
            doc_xml.contains("xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"")
        );
        assert!(
            doc_xml.contains(
                "xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\""
            )
        );
        let root_start = doc_xml.find("<w:document").expect("document root");
        let root_end = root_start
            + doc_xml[root_start..]
                .find('>')
                .expect("document root terminator");
        assert!(doc_xml[root_start..root_end].contains("mc:Ignorable=\"w15\""));
        assert!(doc_xml.contains("<w15:repeatingSection/>"));
    }

    #[test]
    fn package_keeps_body_scoped_mc_namespaces_used_only_by_row_metadata() {
        let document = "<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body \
            xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" \
            xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\" \
            xmlns:ux=\"urn:body-extension\" mc:Ignorable=\"w15 ux\"><w:tbl>\
            <w:sdt><w:sdtPr><mc:AlternateContent>\
            <mc:Choice Requires=\"w15\"><w:alias w:val=\"choice\"/></mc:Choice>\
            <mc:Fallback/></mc:AlternateContent><ux:property/></w:sdtPr><w:sdtContent>\
            <w:tr><w:tc><w:p><w:r><w:t>visible</w:t></w:r></w:p></w:tc></w:tr>\
            </w:sdtContent></w:sdt></w:tbl></w:body></w:document>";
        let package = load_package(&make_docx(document)).expect("load body-scoped namespaces");
        let saved = save_package(&package);
        let reloaded = load_package(&saved).expect("reload body-scoped namespaces");
        let xml = String::from_utf8_lossy(reloaded.part("word/document.xml").unwrap());

        assert!(xml.contains(
            "<w:tbl xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\" xmlns:ux=\"urn:body-extension\" mc:Ignorable=\"w15 ux\">"
        ));
        assert!(xml.contains("<mc:AlternateContent>"));
        assert!(xml.contains("<ux:property/>"));
        assert!(xml.contains("mc:Ignorable=\"w15 ux\""));
        assert_eq!(reloaded.document.plain_text(), "visible\n");
    }

    #[test]
    fn document_root_ignorable_tokens_are_merged() {
        let original = "<w:document xmlns:mc=\"urn:mc\" mc:Ignorable=\"w14\"/>";
        let generated =
            "<w:document xmlns:w15=\"urn:w15\" xmlns:mc=\"urn:mc\" mc:Ignorable=\"w15\"/>";
        let attrs = document_root_attrs(original, generated).unwrap();
        assert!(attrs.contains("xmlns:w15=\"urn:w15\""));
        assert!(attrs.contains("mc:Ignorable=\"w14 w15\""));
    }

    #[test]
    fn document_root_ignorable_aliases_are_merged_by_expanded_name() {
        let mc = "http://schemas.openxmlformats.org/markup-compatibility/2006";
        let original = format!("<w:document xmlns:mce=\"{mc}\" mce:Ignorable=\"w14\"/>");
        let generated = format!("<w:document xmlns:mc=\"{mc}\" mc:Ignorable=\"w15\"/>");
        let attrs = document_root_attrs(&original, &generated).unwrap();

        assert!(attrs.contains("mce:Ignorable=\"w14 w15\""));
        assert!(!attrs.contains(" mc:Ignorable="));
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
