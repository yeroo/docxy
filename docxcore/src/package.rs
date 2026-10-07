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

use crate::load::{
    LoadError, Relationships, parse_document_xml, parse_rels_xml, start_tags, xml_attr_value,
};
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
pub(crate) fn decode_xml_part(bytes: &[u8]) -> Option<Cow<'_, str>> {
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
        let (pairs, remainder) = encoded.as_chunks::<2>();
        if !remainder.is_empty() {
            return None;
        }
        let units = pairs
            .iter()
            .map(|&pair| {
                if little_endian {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect::<Vec<_>>();
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
    /// Every variant, in [`HeaderVariant::index`] order.
    pub(crate) const ALL: [HeaderVariant; 3] = [Self::Default, Self::First, Self::Even];

    /// The `w:type` of a `w:headerReference`/`w:footerReference`.
    pub fn as_ooxml(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::First => "first",
            Self::Even => "even",
        }
    }

    /// The slot of this variant in [`SectionParts`].
    pub(crate) fn index(self) -> usize {
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

/// A VML text watermark's text and how it is drawn, for renderers that draw
/// it (PDF export).
#[derive(Debug, Clone, PartialEq)]
pub struct TextWatermark {
    pub text: String,
    /// Clockwise rotation in degrees (the shape style's `rotation`), 0 when
    /// absent. Word's diagonal watermark is 315.
    pub rotation: f32,
    /// The shape's `fillcolor`, when it is a colour we know.
    pub fill: Option<(u8, u8, u8)>,
    /// The shape's width in points (its style's `width`), which Word stretches
    /// an "Auto"-sized text to.
    pub width_pt: Option<f32>,
    /// The text path's `font-size` in points; `None` when absent or `1pt`,
    /// which is how Word writes "Auto".
    pub font_size_pt: Option<f32>,
    /// The text path's `font-family`, without its quotes.
    pub font: Option<String>,
    /// The shape's `v:fill` opacity, 0..1; `None` when it has none (opaque).
    /// Word's Semitransparent writes `.5`.
    pub opacity: Option<f32>,
}

/// The relationship changes an edited header or footer part needs for its
/// hyperlinks, computed by [`Package::link_part_hyperlinks`] and written by
/// [`Package::apply_part_rels`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartRelsUpdate {
    rels_name: String,
    rels: Vec<u8>,
    /// `[Content_Types].xml` with a `rels` Default added, when it lacked one
    /// and the rels part is new.
    content_types: Option<Vec<u8>>,
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

/// The header or footer part a section applies for one variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPart {
    pub relationship_id: String,
    pub part_name: String,
    /// True when this section inherits the reference from an earlier one
    /// (Word's "Link to Previous").
    pub inherited: bool,
    /// The section whose own reference is applied: this section's index when
    /// not `inherited`, else the earlier section it links back to.
    pub from_section: usize,
}

/// The header and footer parts one section applies, indexed by
/// [`HeaderVariant::index`] (default, first, even), after link-to-previous
/// inheritance. Whether a first/even variant is shown depends on `w:titlePg`
/// and `w:evenAndOddHeaders`, which the caller checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionParts {
    pub headers: [Option<AppliedPart>; 3],
    pub footers: [Option<AppliedPart>; 3],
}

impl SectionParts {
    /// The header (`is_header`) or footer part applied for `variant`.
    pub fn get(&self, is_header: bool, variant: HeaderVariant) -> Option<&AppliedPart> {
        let slots = if is_header {
            &self.headers
        } else {
            &self.footers
        };
        slots[variant.index()].as_ref()
    }
}

/// Resolve every section's header/footer parts. `sect_prs` holds each section's
/// `w:sectPr` XML in document order (the trailing body sectPr last). A section
/// without a reference of some variant inherits the previous section's; a
/// reference that doesn't resolve through `rels` clears the variant.
///
/// Callers editing a document pass the sectPrs they are editing (an editor's
/// [`crate::editor::Editor::sections`]), not the package's saved ones, so an
/// unsaved link change resolves as it will save.
pub fn section_header_parts<S: AsRef<str>>(
    sect_prs: &[S],
    rels: &Relationships,
) -> Vec<SectionParts> {
    const VARIANTS: [HeaderVariant; 3] = HeaderVariant::ALL;
    let mut current = SectionParts::default();
    let mut out = Vec::with_capacity(sect_prs.len());
    for (section, sect_pr) in sect_prs.iter().enumerate() {
        let sect_pr = sect_pr.as_ref();
        for (kind, slots) in [
            ("headerReference", &mut current.headers),
            ("footerReference", &mut current.footers),
        ] {
            for variant in VARIANTS {
                let slot = &mut slots[variant.index()];
                if let Some(relationship_id) =
                    crate::load::header_footer_ref_rid(sect_pr, kind, variant.as_ooxml())
                {
                    *slot = rels.target(&relationship_id).and_then(|target| {
                        Some(AppliedPart {
                            relationship_id,
                            part_name: resolve_document_relationship_target(target)?,
                            inherited: false,
                            from_section: section,
                        })
                    });
                } else if let Some(part) = slot {
                    part.inherited = true;
                }
            }
        }
        out.push(current.clone());
    }
    out
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

/// `word/header1.xml` -> `word/_rels/header1.xml.rels`.
fn part_rels_name(part: &str) -> Option<String> {
    let (dir, file) = part.rsplit_once('/')?;
    Some(format!("{dir}/_rels/{file}.rels"))
}

/// The package part name a `word/_rels/document.xml.rels` target names
/// (`header1.xml` -> `word/header1.xml`), or `None` for an external or
/// malformed target.
pub(crate) fn resolve_document_relationship_target(target: &str) -> Option<String> {
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

/// Whether `xml` holds a watermark the readers would report: a VML shape or
/// a DrawingML `docPr` whose id, name, title or description names one (a
/// `PowerPlusWaterMarkObject` text, Word's `WordPictureWatermark`, …). The
/// writer strips by the same test (#651).
pub(crate) fn holds_watermark(xml: &str) -> bool {
    !watermark_shapes(xml).is_empty()
}

fn watermark_kinds(xml: &str) -> Vec<WatermarkKind> {
    watermark_shapes(xml)
        .into_iter()
        .map(|shape| shape.kind)
        .collect()
}

/// The VML text watermarks in a header part's XML, in document order.
pub fn text_watermarks(xml: &str) -> Vec<TextWatermark> {
    watermark_shapes(xml)
        .into_iter()
        .filter_map(|shape| {
            let WatermarkKind::Text(text) = shape.kind else {
                return None;
            };
            let look = shape.look.unwrap_or_default();
            let style = look.style.as_deref().unwrap_or("");
            Some(TextWatermark {
                text,
                rotation: css_prop(style, "rotation")
                    .and_then(leading_number)
                    .unwrap_or(0.0),
                fill: look.fill.as_deref().and_then(vml_color),
                width_pt: css_prop(style, "width").and_then(css_length_pt),
                font_size_pt: look
                    .text_style
                    .as_deref()
                    .and_then(|s| css_prop(s, "font-size"))
                    .and_then(css_length_pt)
                    .filter(|&pt| (pt - 1.0).abs() > 0.001),
                font: look
                    .text_style
                    .as_deref()
                    .and_then(|s| css_prop(s, "font-family"))
                    .map(|f| f.trim_matches(['"', '\'', ' ']).to_string())
                    .filter(|f| !f.is_empty()),
                opacity: look.opacity.as_deref().and_then(vml_fraction),
            })
        })
        .collect()
}

/// The raw look of a watermark shape: its `style` and `fillcolor`, and its
/// text path's `style`.
#[derive(Debug, Clone, Default)]
struct ShapeLook {
    style: Option<String>,
    fill: Option<String>,
    text_style: Option<String>,
    /// Its `v:fill`'s `opacity`.
    opacity: Option<String>,
}

/// A watermark found in a header part, with the look of the VML shape that
/// holds it (`None` for a DrawingML picture).
struct WatermarkShape {
    kind: WatermarkKind,
    look: Option<ShapeLook>,
}

/// One walk over a header part's watermark shapes, shared by
/// [`watermark_kinds`] and [`text_watermarks`].
fn watermark_shapes(xml: &str) -> Vec<WatermarkShape> {
    #[derive(Default)]
    struct Shape {
        marked: bool,
        texts: Vec<String>,
        picture: bool,
        look: ShapeLook,
    }

    let mut parser = XmlParser::new(xml);
    let mut shapes: Vec<Shape> = Vec::new();
    let mut out = Vec::new();
    loop {
        match parser.next() {
            Event::Start if local_name(parser.name()) == "shape" => {
                shapes.push(Shape {
                    marked: marker_in_attrs(&parser),
                    look: ShapeLook {
                        style: decoded_attr_by_local(&parser, "style"),
                        fill: decoded_attr_by_local(&parser, "fillcolor"),
                        text_style: None,
                        opacity: None,
                    },
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
                    if shape.look.text_style.is_none() {
                        shape.look.text_style = decoded_attr_by_local(&parser, "style");
                    }
                }
            }
            Event::Start if local_name(parser.name()) == "fill" => {
                if let Some(shape) = shapes.last_mut() {
                    shape.look.opacity = decoded_attr_by_local(&parser, "opacity");
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
                    out.push(WatermarkShape {
                        kind: WatermarkKind::Picture,
                        look: None,
                    });
                }
            }
            Event::End if local_name(parser.name()) == "shape" => {
                let Some(shape) = shapes.pop() else {
                    continue;
                };
                let look = Some(shape.look);
                if shape.marked && !shape.texts.is_empty() {
                    out.extend(shape.texts.into_iter().map(|text| WatermarkShape {
                        kind: WatermarkKind::Text(text),
                        look: look.clone(),
                    }));
                } else if shape.marked && shape.picture {
                    out.push(WatermarkShape {
                        kind: WatermarkKind::Picture,
                        look,
                    });
                } else if shape.marked {
                    out.push(WatermarkShape {
                        kind: WatermarkKind::Unknown,
                        look,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// A CSS-style declaration's value in a VML `style` (`a:1;b:2`), by name.
fn css_prop<'a>(style: &'a str, name: &str) -> Option<&'a str> {
    style.split(';').find_map(|decl| {
        let (key, value) = decl.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

/// The number a value starts with (`315`, `-45.5fd`).
fn leading_number(value: &str) -> Option<f32> {
    let end = value
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && matches!(c, '-' | '+'))))
        .map_or(value.len(), |(i, _)| i);
    value[..end].parse().ok().filter(|v: &f32| v.is_finite())
}

/// A VML fraction (`.5`, `50%`, or `32768f` in 65536ths), clamped to 0..1.
fn vml_fraction(value: &str) -> Option<f32> {
    let v = value.trim();
    let n = leading_number(v)?;
    let f = if v.ends_with('f') {
        n / 65536.0
    } else if v.ends_with('%') {
        n / 100.0
    } else {
        n
    };
    Some(f.clamp(0.0, 1.0))
}

/// A CSS length in points: `pt`, `in`, `cm`, `mm` or `px`; anything else is
/// unknown.
fn css_length_pt(value: &str) -> Option<f32> {
    let value = value.trim();
    let n = leading_number(value)?;
    let unit = value
        .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
        .trim();
    let pt = match unit.to_ascii_lowercase().as_str() {
        "pt" => n,
        "in" => n * 72.0,
        "cm" => n * 72.0 / 2.54,
        "mm" => n * 72.0 / 25.4,
        "px" => n * 0.75,
        _ => return None,
    };
    Some(pt)
}

/// A VML colour: `#RRGGBB`, `#RGB` or one of the names Word writes, with any
/// trailing ` [index]` ignored.
fn vml_color(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.split_whitespace().next()?;
    if let Some(hex) = value.strip_prefix('#') {
        let hex = match hex.len() {
            3 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 => hex.to_string(),
            _ => return None,
        };
        let n = u32::from_str_radix(&hex, 16).ok()?;
        return Some(((n >> 16) as u8, (n >> 8) as u8, n as u8));
    }
    Some(match value.to_ascii_lowercase().as_str() {
        "silver" => (0xc0, 0xc0, 0xc0),
        "gray" | "grey" => (0x80, 0x80, 0x80),
        "black" => (0, 0, 0),
        "white" => (0xff, 0xff, 0xff),
        "red" => (0xff, 0, 0),
        "green" => (0, 0x80, 0),
        "blue" => (0, 0, 0xff),
        "yellow" => (0xff, 0xff, 0),
        _ => return None,
    })
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

/// The numbering id (and abstract id) [`Package::ensure_list`] provisions for
/// the bullet list that list commands toggle. A reserved high id, unlikely to
/// collide with a document's own lists.
pub const BULLET_LIST_NUM_ID: i32 = 9990;
/// The decimal-list counterpart of [`BULLET_LIST_NUM_ID`].
pub const NUMBER_LIST_NUM_ID: i32 = 9991;
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

    /// Declare on this package's `<w:document>` root every namespace prefix
    /// `other`'s root binds and this one lacks, and add `other`'s
    /// `mc:Ignorable` tokens, so XML carried over from `other`'s body (as
    /// Compare does with deleted content) keeps its prefixes bound when saved.
    /// Returns the prefixes the two roots bind to different namespaces; XML
    /// using them cannot be carried over.
    pub(crate) fn adopt_root_namespaces(&mut self, other: &Package) -> Vec<String> {
        let own = String::from_utf8_lossy(&self.parts[self.doc_index].1).into_owned();
        let theirs = String::from_utf8_lossy(&other.parts[other.doc_index].1).into_owned();
        let (Some(mut attrs), Some(their_attrs)) = (
            xml_root_attrs(&own, "w:document"),
            xml_root_attrs(&theirs, "w:document"),
        ) else {
            return Vec::new();
        };
        let mut conflicts = Vec::new();
        for (name, value) in &their_attrs {
            let Some(prefix) = name.strip_prefix("xmlns:") else {
                continue;
            };
            match attrs.iter().find(|(key, _)| key == name) {
                Some((_, bound)) if bound != value => conflicts.push(prefix.to_string()),
                Some(_) => {}
                None => attrs.push((name.clone(), value.clone())),
            }
        }
        if let Some((_, ignorable)) = their_attrs.iter().find(|(k, _)| k == "mc:Ignorable") {
            let tokens = ignorable
                .split_whitespace()
                .filter(|token| !conflicts.iter().any(|c| c == token));
            match attrs.iter_mut().find(|(k, _)| k == "mc:Ignorable") {
                Some((_, existing)) => {
                    for token in tokens {
                        if !existing.split_whitespace().any(|t| t == token) {
                            if !existing.is_empty() {
                                existing.push(' ');
                            }
                            existing.push_str(token);
                        }
                    }
                }
                None => {
                    let tokens: Vec<&str> = tokens.collect();
                    if !tokens.is_empty() {
                        attrs.push(("mc:Ignorable".to_string(), tokens.join(" ")));
                    }
                }
            }
        }
        let mut parser = XmlParser::new(&own);
        let (start, end) = loop {
            match parser.next() {
                Event::Start if parser.name() == "w:document" => {
                    break (parser.start_pos(), parser.pos());
                }
                Event::Eof => return conflicts,
                _ => {}
            }
        };
        let root = attrs
            .iter()
            .map(|(name, value)| format!("{name}=\"{}\"", esc_xml_attr(value)))
            .collect::<Vec<_>>()
            .join(" ");
        let close = if own[start..end].trim_end().ends_with("/>") {
            "/>"
        } else {
            ">"
        };
        let rebuilt = format!("{}<w:document {root}{close}{}", &own[..start], &own[end..]);
        self.parts[self.doc_index].1 = rebuilt.into_bytes();
        conflicts
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
        self.document
            .set_trailing_section_properties(SectionProperties {
                raw,
                property_change,
            });
        self.sect_pr = xml;
    }

    /// Keep the parts whose name `keep` accepts, and always the main document
    /// part, still found at `doc_index` when a part listed before it goes
    /// (#1107).
    fn retain_parts(&mut self, mut keep: impl FnMut(&str) -> bool) {
        let doc_name = self.parts[self.doc_index].0.clone();
        self.parts.retain(|(n, _)| *n == doc_name || keep(n));
        self.doc_index = self
            .parts
            .iter()
            .position(|(n, _)| *n == doc_name)
            .expect("the main document part is kept");
    }

    /// The final section as split section properties: the document's own
    /// trailing sectPr, else the captured one (an empty
    /// `<w:sectPr></w:sectPr>` when the package has none).
    pub fn final_section(&self) -> SectionProperties {
        final_section_of(&self.document, &self.sect_pr)
    }

    /// The body the stored main document part encodes, parsed as
    /// [`load_package`] parses it and ending in its [`Package::final_section`]:
    /// what a fresh load of this package shows. Unlike [`Package::document`]
    /// (which [`Package::set_sect_pr`] and others change), only rewriting the
    /// part changes it, so a body equal to it can keep the part's bytes
    /// (#1107). `None` when the part is not UTF-8.
    pub fn stored_document(&self) -> Option<Document> {
        let (mut document, sect_pr) = parse_main_document(&self.parts, self.doc_index).ok()?;
        if document.trailing_section_properties().is_none() {
            let section = final_section_of(&document, &sect_pr);
            document.set_trailing_section_properties(section);
        }
        Some(document)
    }

    /// Whether `doc` is the body the stored main document part encodes
    /// ([`Package::stored_document`]), so a save may keep the part's bytes
    /// ([`save_package_keeping_document`]). A merge preview's record in the
    /// merge fields is display only, never written, so it is not a change.
    pub fn stores_document(&self, doc: &Document) -> bool {
        let Some(mut stored) = self.stored_document() else {
            return false;
        };
        if stored == *doc {
            return true;
        }
        let mut doc = doc.clone();
        crate::merge::preview::apply_preview(&mut doc, None);
        crate::merge::preview::apply_preview(&mut stored, None);
        stored == doc
    }

    /// Replace the trailing section properties with an already-split section
    /// (an editor's copy, tracked change included).
    pub fn set_trailing_section(&mut self, section: SectionProperties) {
        self.sect_pr = section.raw.clone();
        self.document.set_trailing_section_properties(section);
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

    /// Each section's `w:sectPr` XML in document order: every paragraph section
    /// break, then the trailing body sectPr. Even an empty trailing value stands
    /// for the one implicit section of a document without sectPr.
    fn section_sect_prs(&self) -> Vec<&str> {
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
        sections.push(self.sect_pr());
        sections
    }

    /// The main document relationships (`word/_rels/document.xml.rels`).
    pub fn document_rels(&self) -> Relationships {
        self.part("word/_rels/document.xml.rels")
            .and_then(decode_xml_part)
            .map(|xml| parse_rels_xml(&xml))
            .unwrap_or_default()
    }

    /// Watermarks in headers that are actually applied by document section
    /// relationships. Each inherited header is associated with every section in
    /// which it remains effective rather than merely scanning orphan header parts.
    pub fn watermarks(&self) -> Vec<Watermark> {
        const VARIANTS: [HeaderVariant; 3] = HeaderVariant::ALL;

        let even_and_odd = self.has_even_odd();
        let sections = self.section_sect_prs();
        let applied_parts = section_header_parts(&sections, &self.document_rels());

        let mut out = Vec::new();
        for (section_index, (sect_pr, parts)) in sections.into_iter().zip(applied_parts).enumerate()
        {
            let applied = parts.headers;
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

    /// The main document part's raw bytes (`word/document.xml` or wherever the
    /// package relationship points).
    pub(crate) fn document_part(&self) -> Option<&[u8]> {
        self.parts
            .get(self.doc_index)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// A header or footer part's blocks, with its relationships (hyperlinks,
    /// images) resolved through the part's own `_rels`, not the document's.
    /// `None` when the part is missing or unreadable.
    pub fn header_footer_blocks(&self, part: &str) -> Option<Vec<Block>> {
        let xml = self.part(part).and_then(decode_xml_part)?;
        let rels = part_rels_name(part)
            .and_then(|name| self.part(&name))
            .and_then(decode_xml_part)
            .map(|xml| parse_rels_xml(&xml))
            .unwrap_or_default();
        Some(crate::load::parse_header_footer(&xml, &rels))
    }

    /// The raw bytes of a part by name.
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    /// An XML part's text, decoded the way the loader reads it (UTF-8, or
    /// UTF-16 with or without a byte-order mark). `None` when the part is
    /// missing or isn't decodable XML text.
    pub fn part_text(&self, name: &str) -> Option<String> {
        self.part(name)
            .and_then(decode_xml_part)
            .map(Cow::into_owned)
    }

    /// Replace an existing XML part's text, encoded the way the part was: a
    /// UTF-16 part stays UTF-16 (so its declaration stays true), a BOM stays.
    /// Returns false if no such part exists.
    pub fn set_part_text(&mut self, name: &str, text: &str) -> bool {
        match self.parts.iter_mut().find(|(n, _)| n == name) {
            Some(e) => {
                e.1 = encode_like(&e.1, text);
                true
            }
            None => false,
        }
    }

    /// Give every external hyperlink in `blocks`, the new content of header or
    /// footer `part`, a relationship in the part's own `_rels`: links created
    /// in the editor have none, and a link pasted from the body keeps the
    /// body's id, which means nothing (or something else) in the part's rels.
    /// A link whose id already names an external relationship to its target
    /// is left as is. Each other link gets a fresh `rIdN`, set as its
    /// `rel_id` and written into its preserved opening tag, so its other
    /// attributes (tooltip, target frame) survive.
    ///
    /// The package isn't changed: the returned update, `None` when no link
    /// needed one, is written with [`Package::apply_part_rels`] once the part
    /// itself has been written. When a link needs a relationship but the
    /// part's rels can't be read or extended, the error says why and neither
    /// the links nor the package are touched.
    pub fn link_part_hyperlinks(
        &self,
        part: &str,
        blocks: &mut [crate::model::Block],
    ) -> Result<Option<PartRelsUpdate>, &'static str> {
        let mut links = Vec::new();
        collect_external_links(blocks, &mut links);
        if links.is_empty() {
            return Ok(None);
        }
        let rels_name = part_rels_name(part).ok_or("the part has no relationships part name")?;
        let existing = self.part(&rels_name);
        let text = match existing {
            Some(bytes) => decode_xml_part(bytes)
                .ok_or("its relationships part isn't readable XML")?
                .into_owned(),
            None => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"{PACKAGE_RELATIONSHIPS_NS}\"></Relationships>"
            ),
        };
        let rels = parse_rels_xml(&text);
        // Work out every new id first; the links change only once the rels do.
        let mut next = next_rid_num(&text);
        let mut added = String::new();
        let mut minted = Vec::new();
        for (i, h) in links.iter().enumerate() {
            let target = h.target.as_deref().unwrap_or_default();
            let linked = h.rel_id.as_deref().is_some_and(|id| {
                rels.iter()
                    .any(|(rid, t, external)| rid == id && t == target && external)
            });
            if linked {
                continue;
            }
            // `next_rid_num` only sees `Id="rIdN"`; skip ids spelled otherwise.
            while rels.target(&format!("rId{next}")).is_some() {
                next += 1;
            }
            let rid = format!("rId{next}");
            next += 1;
            added.push_str(&format!(
                "<Relationship Id=\"{rid}\" Type=\"{HYPERLINK_REL}\" Target=\"{}\" TargetMode=\"External\"/>",
                esc_xml_attr(target)
            ));
            minted.push((i, rid));
        }
        if minted.is_empty() {
            return Ok(None);
        }
        let updated = append_relationships(&text, &added)
            .ok_or("its relationships part has no Relationships root")?;
        for (i, rid) in minted {
            let h = &mut links[i];
            if let Some(raw) = &mut h.raw {
                *raw = with_opener_rel_id(raw, &rid);
            }
            h.rel_id = Some(rid);
        }
        let rels = match existing {
            Some(bytes) => encode_like(bytes, &updated),
            None => updated.into_bytes(),
        };
        let content_types = if existing.is_none() {
            self.content_types_with_rels_default()
        } else {
            None
        };
        Ok(Some(PartRelsUpdate {
            rels_name,
            rels,
            content_types,
        }))
    }

    /// `[Content_Types].xml` with a `rels` Default added, or `None` when it
    /// already has one (or can't be read).
    fn content_types_with_rels_default(&self) -> Option<Vec<u8>> {
        let bytes = self.part("[Content_Types].xml")?;
        let xml = decode_xml_part(bytes)?;
        let has_default = start_tags(&xml, "Default").into_iter().any(|(_, el)| {
            xml_attr_value(el, "Extension").is_some_and(|ext| ext.eq_ignore_ascii_case("rels"))
        });
        if has_default {
            return None;
        }
        let default = "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>";
        let (start, el) = start_tags(&xml, "Types").into_iter().next()?;
        let at = start + el.len();
        let updated = if el.ends_with("/>") {
            format!("{}>{default}</Types>{}", &xml[..at - 2], &xml[at..])
        } else {
            format!("{}{default}{}", &xml[..at], &xml[at..])
        };
        Some(encode_like(bytes, &updated))
    }

    /// Write a [`Package::link_part_hyperlinks`] update: the part's rels
    /// (created when new) and the content types.
    pub fn apply_part_rels(&mut self, update: PartRelsUpdate) {
        let PartRelsUpdate {
            rels_name,
            rels,
            content_types,
        } = update;
        if !self.set_part(&rels_name, rels.clone()) {
            self.parts.push((rels_name, rels));
        }
        if let Some(ct) = content_types {
            self.set_part("[Content_Types].xml", ct);
        }
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
        let (rid, part_name) = self.create_hf_part(is_header, "<w:p/>")?;
        let section = crate::sect::set_hf_reference(self.sect_pr(), is_header, hf_type, Some(&rid));
        self.set_current_sect_pr_raw(section);
        Some(part_name)
    }

    /// The header parts `sect_prs` show, once each, in section order: every
    /// section's default header, its first-page header when it has
    /// `w:titlePg`, and its even-page header when the document has
    /// `w:evenAndOddHeaders`. A slot that resolves to no part gets a new,
    /// empty one, referenced from that section's sectPr (later sections
    /// inherit it). When a new part cannot be added, or its reference does
    /// not resolve, the remaining empty slots stay empty.
    fn shown_header_parts(&mut self, sect_prs: &mut [String]) -> Vec<String> {
        // A new part changes what later sections inherit: look again after
        // each, and stop as soon as one makes no progress.
        let empty_slot = |pkg: &Self, sect_prs: &[String]| {
            pkg.shown_header_slots(sect_prs)
                .into_iter()
                .find_map(|(k, variant, part)| part.is_none().then_some((k, variant)))
        };
        while let Some((k, variant)) = empty_slot(self, sect_prs) {
            let Some((rid, _)) = self.create_hf_part(true, "<w:p/>") else {
                break;
            };
            self.ensure_styles(&["Header"]);
            let linked =
                crate::sect::set_hf_reference(&sect_prs[k], true, variant.as_ooxml(), Some(&rid));
            let before = std::mem::replace(&mut sect_prs[k], linked);
            if empty_slot(self, sect_prs) == Some((k, variant)) {
                // The part is in the package but its reference does not
                // resolve: leave the section as it was.
                sect_prs[k] = before;
                break;
            }
        }
        let mut out: Vec<String> = Vec::new();
        for (_, _, part) in self.shown_header_slots(sect_prs) {
            if let Some(name) = part.filter(|n| !out.contains(n)) {
                out.push(name);
            }
        }
        out
    }

    /// Every header slot `sect_prs` show, in order: each section's default
    /// header, its first-page header when it has `w:titlePg`, and its
    /// even-page header when the document has `w:evenAndOddHeaders`, with
    /// the part it resolves to (inherited included), `None` when it has none.
    /// The one rule the watermark writer and reader share.
    fn shown_header_slots<S: AsRef<str>>(
        &self,
        sect_prs: &[S],
    ) -> Vec<(usize, HeaderVariant, Option<String>)> {
        let even = self.has_even_odd();
        let applied = section_header_parts(sect_prs, &self.document_rels());
        let mut out = Vec::new();
        for (k, (sect, parts)) in sect_prs.iter().zip(&applied).enumerate() {
            let title = crate::sect::has_flag(sect.as_ref(), "w:titlePg");
            for variant in HeaderVariant::ALL {
                if (variant == HeaderVariant::First && !title)
                    || (variant == HeaderVariant::Even && !even)
                {
                    continue;
                }
                let part = parts.get(true, variant).map(|p| p.part_name.clone());
                out.push((k, variant, part));
            }
        }
        out
    }

    /// Every header part `sect_prs` reference, of any variant, shown or not
    /// (a first-page header while `w:titlePg` is off), once each.
    fn referenced_header_parts<S: AsRef<str>>(&self, sect_prs: &[S]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for parts in section_header_parts(sect_prs, &self.document_rels()) {
            for part in parts.headers.into_iter().flatten() {
                if !out.contains(&part.part_name) {
                    out.push(part.part_name);
                }
            }
        }
        out
    }

    /// Put `spec`'s text watermark into every header `sect_prs` show,
    /// replacing any watermark there, or remove every watermark with `None`
    /// (Remove Watermark, #651). Old watermarks go from every header the
    /// sections reference, shown or not, so turning on a distinct first page
    /// later cannot bring one back. A shown header slot with no part gets a
    /// new header part (only when adding), and its reference is written into
    /// that section's entry of `sect_prs`: the caller applies those (an
    /// editor's sections, see [`crate::editor::Editor::replace_sections`]).
    /// Whether any part changed.
    pub fn set_text_watermark(
        &mut self,
        spec: Option<&crate::watermark::TextWatermarkSpec>,
        sect_prs: &mut [String],
    ) -> bool {
        // Number shapes past any the headers already hold, so ids stay unique.
        let mut n = self
            .parts
            .iter()
            .filter(|(name, _)| name.starts_with("word/header"))
            .filter_map(|(_, b)| decode_xml_part(b).map(Cow::into_owned))
            .flat_map(|xml| {
                xml.match_indices(crate::watermark::SHAPE_ID)
                    .filter_map(|(i, m)| {
                        let digits: String = xml[i + m.len()..]
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect();
                        digits.parse::<u32>().ok()
                    })
                    .collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0);
        let referenced = self.referenced_header_parts(sect_prs);
        let shown = match spec {
            Some(_) => self.shown_header_parts(sect_prs),
            None => Vec::new(),
        };
        let mut visit: Vec<&String> = Vec::new();
        for name in referenced.iter().chain(&shown) {
            if !visit.contains(&name) {
                visit.push(name);
            }
        }
        let mut changed = false;
        for name in visit {
            let Some(xml) = self.part_text(name) else {
                continue;
            };
            let new = match spec {
                Some(spec) if shown.contains(name) => {
                    // The same watermark again, under its own number, is no
                    // change: the part stays as it is.
                    let own = xml.find(crate::watermark::SHAPE_ID).and_then(|i| {
                        xml[i + crate::watermark::SHAPE_ID.len()..]
                            .split(|c: char| !c.is_ascii_digit())
                            .next()?
                            .parse::<u32>()
                            .ok()
                    });
                    match own.map(|m| crate::watermark::insert_watermark(&xml, spec, m)) {
                        Some(same) if same == xml => same,
                        _ => {
                            n += 1;
                            crate::watermark::insert_watermark(&xml, spec, n)
                        }
                    }
                }
                _ => crate::watermark::strip_watermarks(&xml),
            };
            if new != xml {
                self.set_part_text(name, &new);
                changed = true;
            }
        }
        changed
    }

    /// [`Package::set_text_watermark`] over the package's own document: its
    /// sections' sectPrs are read from and written back to
    /// [`Package::document`]. The tests' stand-in for an editor.
    #[cfg(test)]
    fn apply_text_watermark(&mut self, spec: Option<&crate::watermark::TextWatermarkSpec>) -> bool {
        let mut sect_prs: Vec<String> = self
            .section_sect_prs()
            .into_iter()
            .map(str::to_string)
            .collect();
        let before = sect_prs.clone();
        let changed = self.set_text_watermark(spec, &mut sect_prs);
        if sect_prs != before {
            let trailing = sect_prs.pop().unwrap_or_default();
            let mut breaks = sect_prs.into_iter();
            for block in &mut self.document.body {
                if let Block::Paragraph(p) = block {
                    if p.props.section_break.is_some() {
                        if let Some(raw) = breaks.next() {
                            p.props.section_break = Some(raw);
                        }
                    }
                }
            }
            self.set_current_sect_pr_raw(trailing);
        }
        changed
    }

    /// The text watermarks in the headers `sect_prs` show (each part once):
    /// what Design > Watermark reads as current while sections are being
    /// edited, unlike [`Package::watermarks`], which reads the loaded ones.
    pub fn shown_text_watermarks<S: AsRef<str>>(&self, sect_prs: &[S]) -> Vec<TextWatermark> {
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (_, _, part) in self.shown_header_slots(sect_prs) {
            let Some(name) = part.filter(|n| !seen.contains(n)) else {
                continue;
            };
            if let Some(xml) = self.part(&name).and_then(decode_xml_part) {
                out.extend(text_watermarks(&xml));
            }
            seen.push(name);
        }
        out
    }

    /// Add a header (`is_header`) or footer part holding `content_xml` (the
    /// block XML inside `w:hdr`/`w:ftr`), with its `[Content_Types].xml`
    /// override and a `document.xml.rels` relationship, but no section
    /// reference: the caller decides which section references it (see
    /// [`crate::sect::set_hf_reference`]). The relationship id and part name;
    /// `None` when the package cannot reference a new part (see
    /// [`Package::add_hf_part`]), and then nothing is added.
    pub fn create_hf_part(
        &mut self,
        is_header: bool,
        content_xml: &str,
    ) -> Option<(String, String)> {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
        let tag = if is_header { "w:hdr" } else { "w:ftr" };
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
<{tag} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\">{content_xml}</{tag}>"
        );
        self.add_hf_part(is_header, body.into_bytes(), None)
    }

    /// Copy an existing header or footer part into a new one (Link to
    /// Previous turned off): the bytes as they are, and its own `_rels` part
    /// when it has one, so the copy's pictures and links resolve through the
    /// same relationship ids. The kind follows the source's root element. The
    /// new relationship id and part name; `None` when `src` is missing or
    /// the package cannot reference a new part (see [`Package::add_hf_part`]),
    /// and then nothing is added.
    pub fn copy_hf_part(&mut self, src: &str) -> Option<(String, String)> {
        let bytes = self.part(src)?.to_vec();
        let is_header = decode_xml_part(&bytes)
            .map(|xml| !xml.contains("<w:ftr"))
            .unwrap_or(true);
        let rels = part_rels_name(src).and_then(|name| self.part(&name).map(<[u8]>::to_vec));
        self.add_hf_part(is_header, bytes, rels)
    }

    /// Store a header/footer part under a fresh `word/{header|footer}N.xml`
    /// name, with its optional own `_rels`, a content-type override and a
    /// document relationship. A missing `document.xml.rels` is created (as
    /// [`Package::add_media_part`] does). `None`, with nothing added, when
    /// the package cannot reference a new part: `document.xml.rels` has no
    /// `Relationships` root, or `[Content_Types].xml` (when present) has no
    /// `Types` root.
    fn add_hf_part(
        &mut self,
        is_header: bool,
        bytes: Vec<u8>,
        own_rels: Option<Vec<u8>>,
    ) -> Option<(String, String)> {
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let (kind, ct) = if is_header {
            (
                "header",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
            )
        } else {
            (
                "footer",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
            )
        };
        // Unused part name word/{kind}{n}.xml.
        let mut n = 1;
        while self.part(&format!("word/{kind}{n}.xml")).is_some() {
            n += 1;
        }
        let target = format!("{kind}{n}.xml");
        let part_name = format!("word/{target}");

        // A fresh relationship id from document.xml.rels, and the
        // relationship and content-type override, worked out before anything
        // is added: a part the package cannot reference is not added at all.
        const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
        let rels_name = "word/_rels/document.xml.rels";
        let rels_xml = match self.part(rels_name) {
            Some(b) => String::from_utf8_lossy(b).into_owned(),
            None => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
                 <Relationships xmlns=\"{RELS_NS}\"></Relationships>"
            ),
        };
        let rid = next_rid(&rels_xml);
        let rel =
            format!("<Relationship Id=\"{rid}\" Type=\"{R_NS}/{kind}\" Target=\"{target}\"/>");
        let new_rels = append_relationships(&rels_xml, &rel)?;
        let new_ct = match self.part("[Content_Types].xml") {
            Some(b) => {
                let ct_xml = String::from_utf8_lossy(b).into_owned();
                let ov = format!("<Override PartName=\"/{part_name}\" ContentType=\"{ct}\"/>");
                Some(append_to_root(&ct_xml, "Types", &ov)?)
            }
            None => None,
        };

        self.parts.push((part_name.clone(), bytes));
        if let (Some(rels), Some(name)) = (own_rels, part_rels_name(&part_name)) {
            self.retain_parts(|n| n != name);
            self.parts.push((name, rels));
        }

        if !self.set_part(rels_name, new_rels.clone().into_bytes()) {
            self.parts
                .push((rels_name.to_string(), new_rels.into_bytes()));
        }
        if let Some(new_ct) = new_ct {
            self.set_part("[Content_Types].xml", new_ct.into_bytes());
        }
        Some((rid, part_name))
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
    pub(crate) fn settings_flag(&self, elem: &str) -> Option<bool> {
        let name = self.settings_part_name().ok()??;
        let b = self.part(&name)?;
        let xml = decode_xml_part(b)?;
        settings_flag_of(&xml, elem)
    }

    /// Whether even pages mirror the side margins (`<w:mirrorMargins/>`).
    pub fn has_mirror_margins(&self) -> bool {
        self.settings_flag("w:mirrorMargins").unwrap_or(false)
    }

    /// Turn mirrored margins on or off (`<w:mirrorMargins/>` in settings).
    pub fn set_mirror_margins(&mut self, on: bool) {
        self.set_settings_flag("w:mirrorMargins", on);
    }

    /// Whether the gutter sits at the top of the page (`<w:gutterAtTop/>`).
    pub fn has_gutter_at_top(&self) -> bool {
        self.settings_flag("w:gutterAtTop").unwrap_or(false)
    }

    /// Put the gutter at the top or the left (`<w:gutterAtTop/>` in settings).
    pub fn set_gutter_at_top(&mut self, on: bool) {
        self.set_settings_flag("w:gutterAtTop", on);
    }

    /// Toggle automatic hyphenation for the document (`<w:autoHyphenation/>`).
    /// docxy doesn't hyphenate its own on-screen layout, but Word honours the
    /// flag when it lays the document out for print.
    pub fn set_auto_hyphenation(&mut self, on: bool) {
        self.set_settings_flag("w:autoHyphenation", on);
    }

    /// The page colour (Design > Page Color, #651): the document part's
    /// `w:background`. Cheap enough to call every frame: the part is borrowed (a UTF-8 part
    /// is not copied) and only the prolog before `<w:body` is read.
    pub fn page_background(&self) -> Option<crate::page_bg::PageBackground> {
        let doc = decode_xml_part(&self.parts.get(self.doc_index)?.1)?;
        let prolog = doc.find("<w:body").map_or(&doc[..], |at| &doc[..at]);
        crate::page_bg::page_background(prolog)
    }

    /// Set the page colour, or remove it with `None` (No Color): the
    /// document part's `w:background`, and `w:displayBackgroundShape` in the
    /// settings, without which Word does not show it. Whether anything
    /// changed.
    pub fn set_page_background(&mut self, bg: Option<&crate::page_bg::PageBackground>) -> bool {
        let Some(name) = self.parts.get(self.doc_index).map(|p| p.0.clone()) else {
            return false;
        };
        let Some(doc) = self.part_text(&name) else {
            return false;
        };
        let new = crate::page_bg::set_page_background(&doc, bg);
        let changed = new != doc;
        if changed {
            self.set_part_text(&name, &new);
        }
        let settings = self.set_display_background_shape(bg.is_some());
        changed || settings
    }

    /// Whether the settings part asks Word to show the page colour.
    pub fn has_display_background_shape(&self) -> bool {
        self.settings_flag("w:displayBackgroundShape")
            .unwrap_or(false)
    }

    /// Add `w:displayBackgroundShape` at its `CT_Settings` position, or
    /// remove it. Whether the settings changed.
    fn set_display_background_shape(&mut self, on: bool) -> bool {
        const ELEM: &str = "w:displayBackgroundShape";
        if self.has_display_background_shape() == on {
            return false;
        }
        let name = if on {
            self.ensure_settings_part()
        } else {
            self.settings_part_name().ok().flatten()
        };
        let Some(name) = name else {
            return false;
        };
        let Some(xml) = self.part_text(&name) else {
            return false;
        };
        // An explicit `w:val="false"` is replaced, not kept beside a new one.
        let mut xml = xml;
        while crate::sect::find_element(&xml, ELEM).is_some() {
            xml = crate::sect::remove_element(&xml, ELEM);
        }
        if on {
            let before = SETTINGS_BEFORE_MAIL_MERGE
                .iter()
                .position(|n| *n == ELEM)
                .map_or(&[][..], |i| &SETTINGS_BEFORE_MAIL_MERGE[..i]);
            xml = insert_settings_child(&xml, &format!("<{ELEM}/>"), before);
        }
        self.set_part_text(&name, &xml)
    }

    /// Whether the settings ask for Track Changes (`w:trackRevisions`), so a
    /// document saved with it on opens with it on (#624).
    pub fn track_revisions(&self) -> bool {
        self.settings_flag("w:trackRevisions").unwrap_or(false)
    }

    /// Turn Track Changes on or off in the settings: `w:trackRevisions` at its
    /// `CT_Settings` position (after `w:revisionView`, before
    /// `w:doNotTrackMoves` and the rest), creating the settings part when
    /// needed; off removes the element, an explicit `w:val="false"` included.
    /// Whether the settings changed.
    pub fn set_track_revisions(&mut self, on: bool) -> bool {
        const ELEM: &str = "w:trackRevisions";
        if self.track_revisions() == on && (on || self.settings_flag(ELEM).is_none()) {
            return false;
        }
        let name = if on {
            self.ensure_settings_part()
        } else {
            self.settings_part_name().ok().flatten()
        };
        let Some(name) = name else {
            return false;
        };
        let Some(mut xml) = self.part_text(&name) else {
            return false;
        };
        while crate::sect::find_element(&xml, ELEM).is_some() {
            xml = crate::sect::remove_element(&xml, ELEM);
        }
        if on {
            let mut before = SETTINGS_BEFORE_MAIL_MERGE.to_vec();
            before.extend(["w:mailMerge", "w:revisionView"]);
            xml = insert_settings_child(&xml, &format!("<{ELEM}/>"), &before);
        }
        self.set_part_text(&name, &xml)
    }

    /// Add or remove a boolean flag element (e.g. `w:evenAndOddHeaders`,
    /// `w:autoHyphenation`) in `word/settings.xml`, creating the part (+ its
    /// content-type and relationship) if it doesn't exist yet.
    fn set_settings_flag(&mut self, elem: &str, on: bool) {
        match self.settings_part_name() {
            Ok(Some(_)) => {}
            // Nothing to turn off, and nothing to create it in.
            Ok(None) if !on => return,
            Ok(None) => {}
            Err(_) => return,
        }
        let Some(name) = self.ensure_settings_part() else {
            return;
        };
        let name = name.as_str();
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
                crate::sect::remove_element(&xml, elem)
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
        }
    }

    /// The settings part's name, creating an empty `word/settings.xml` (with
    /// its content-type override and document relationship) when there is
    /// none. `None` when the document's settings relationship is broken.
    fn ensure_settings_part(&mut self) -> Option<String> {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        if let Some(name) = self.settings_part_name().ok()? {
            return Some(name);
        }
        let name = "word/settings.xml";
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:settings xmlns:w=\"{W_NS}\"/>"
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
        Some(name.to_string())
    }

    /// The document's mail-merge setup (`w:mailMerge` in the settings part):
    /// its main-document type and, when it names one, the data source file
    /// (the target of its `mailMergeSource` relationship, or the `w:query`'s
    /// file when there is no relationship). Recorded only: nothing reads the
    /// data source here (#628).
    pub fn mail_merge(&self) -> Option<MailMerge> {
        let name = self.settings_part_name().ok()??;
        let xml = decode_xml_part(self.part(&name)?)?.into_owned();
        let (a, b) = crate::sect::find_element(&xml, "w:mailMerge")?;
        let mm = &xml[a..b];
        let val = |child: &str| {
            crate::load::start_tags(mm, child)
                .into_iter()
                .find(|(_, el)| el[child.len() + 1..].starts_with([' ', '/', '>']))
                .and_then(|(_, el)| crate::load::xml_attr_value(el, "w:val"))
                .map(|v| decode_xml_entities(&v))
        };
        let doc_type = val("w:mainDocumentType")
            .and_then(|v| MainDocType::from_ooxml(&v))
            .unwrap_or_default();
        let rid = crate::load::start_tags(mm, "w:dataSource")
            .into_iter()
            .find_map(|(_, el)| crate::load::xml_attr_value(el, "r:id"));
        let by_rel = rid.and_then(|rid| {
            let rels = decode_xml_part(self.part(&part_rels_name(&name)?)?)?.into_owned();
            crate::load::start_tags(&rels, "Relationship")
                .into_iter()
                .find(|(_, el)| {
                    crate::load::xml_attr_value(el, "Id").as_deref() == Some(rid.as_str())
                })
                .and_then(|(_, el)| crate::load::xml_attr_value(el, "Target"))
                .map(|t| file_url_to_path(&decode_xml_entities(&t)))
        });
        let source = by_rel.or_else(|| {
            let q = val("w:query")?;
            let from = q.to_ascii_uppercase().find(" FROM ")? + " FROM ".len();
            let path = q[from..].trim().trim_matches(['`', '\'', '"', '[', ']']);
            (!path.is_empty()).then(|| path.to_string())
        });
        Some(MailMerge { doc_type, source })
    }

    /// The data source file the document's mail merge names, if any.
    #[cfg(test)]
    fn mail_merge_source(&self) -> Option<String> {
        self.mail_merge()?.source
    }

    /// Write `w:mailMerge` (at its `CT_Settings` position, replacing any
    /// there) and its `mailMergeSource` relationship, creating the settings
    /// part and its relationships part when missing; `None` removes both
    /// ("Normal Word Document").
    pub fn set_mail_merge(&mut self, mm: Option<&MailMerge>) {
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        const PKG_RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
        let name = match mm {
            Some(_) => self.ensure_settings_part(),
            None => self.settings_part_name().ok().flatten(),
        };
        let Some(name) = name else {
            return;
        };
        let Some(xml) = self
            .part(&name)
            .and_then(decode_xml_part)
            .map(Cow::into_owned)
        else {
            return;
        };
        // Drop the old element and every mailMergeSource relationship.
        let mut xml = crate::sect::remove_element(&xml, "w:mailMerge");
        let Some(rels_name) = part_rels_name(&name) else {
            return;
        };
        let mut rels = self
            .part(&rels_name)
            .and_then(decode_xml_part)
            .map(Cow::into_owned);
        if let Some(r) = rels.as_mut() {
            let spans: Vec<(usize, usize)> = crate::load::start_tags(r, "Relationship")
                .into_iter()
                .filter(|(_, el)| {
                    let ty = crate::load::xml_attr_value(el, "Type").unwrap_or_default();
                    ty.ends_with("/mailMergeSource") || ty.ends_with("/recipientData")
                })
                .map(|(start, el)| {
                    let end = if el.ends_with("/>") {
                        start + el.len()
                    } else {
                        r[start..]
                            .find("</Relationship>")
                            .map_or(start + el.len(), |e| start + e + "</Relationship>".len())
                    };
                    (start, end)
                })
                .collect();
            for (start, end) in spans.into_iter().rev() {
                r.replace_range(start..end, "");
            }
        }
        if let Some(mm) = mm {
            let mut el = String::from("<w:mailMerge><w:mainDocumentType w:val=\"");
            el.push_str(mm.doc_type.as_ooxml());
            el.push_str("\"/>");
            if let Some(src) = &mm.source {
                let r = rels.get_or_insert_with(|| {
                    format!(
                        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                         <Relationships xmlns=\"{PKG_RELS}\"></Relationships>"
                    )
                });
                let rid = next_rid(r);
                let mut rel =
                    format!("<Relationship Id=\"{rid}\" Type=\"{R_NS}/mailMergeSource\" Target=\"");
                crate::serialize::esc_attr(&path_to_file_url(src), &mut rel);
                rel.push_str("\" TargetMode=\"External\"/>");
                if let Some(appended) = append_relationships(r, &rel) {
                    *r = appended;
                }
                el.push_str("<w:linkToQuery/><w:dataType w:val=\"textFile\"/>");
                el.push_str("<w:connectString w:val=\"\"/><w:query w:val=\"");
                crate::serialize::esc_attr(&format!("SELECT * FROM {src}"), &mut el);
                el.push_str("\"/><w:dataSource xmlns:r=\"");
                el.push_str(R_NS);
                el.push_str("\" r:id=\"");
                el.push_str(&rid);
                el.push_str("\"/>");
            } else {
                el.push_str("<w:dataType w:val=\"textFile\"/>");
            }
            el.push_str("</w:mailMerge>");
            xml = insert_mail_merge(&xml, &el);
        }
        self.set_part(&name, xml.into_bytes());
        if let Some(r) = rels {
            if !self.set_part(&rels_name, r.clone().into_bytes()) {
                self.parts.push((rels_name, r.into_bytes()));
            }
        }
    }

    /// The Word version whose layout rules the document asks for
    /// (`w:compat/w:compatSetting[@w:name="compatibilityMode"]`): 15 for a
    /// current document, 11 for one kept in Word 2003's Compatibility Mode,
    /// `None` when the settings don't say.
    pub fn compatibility_mode(&self) -> Option<u32> {
        let name = self.settings_part_name().ok()??;
        let xml = decode_xml_part(self.part(&name)?)?;
        compatibility_mode_setting(&xml).and_then(|(_, el)| {
            crate::load::xml_attr_value(el, "w:val").and_then(|v| v.trim().parse().ok())
        })
    }

    /// Set `compatibilityMode` to `mode`, replacing the value already there or
    /// adding the setting (and its `w:compat`, and the settings part) when
    /// missing. Word's Convert writes 15; a document imported from a Word
    /// 97-2003 file is 11 until it is converted.
    pub fn set_compatibility_mode(&mut self, mode: u32) {
        let Some(name) = self.ensure_settings_part() else {
            return;
        };
        let Some(xml) = self
            .part(&name)
            .and_then(decode_xml_part)
            .map(Cow::into_owned)
        else {
            return;
        };
        let setting = format!(
            "<w:compatSetting w:name=\"compatibilityMode\" \
             w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"{mode}\"/>"
        );
        let xml = if let Some((start, el)) = compatibility_mode_setting(&xml) {
            let end = start + el.len();
            // `<w:compatSetting …>` with content is not Word's, but keep the
            // part well formed if a writer used one.
            let end = if el.ends_with("/>") {
                end
            } else {
                xml[end..]
                    .find("</w:compatSetting>")
                    .map_or(end, |e| end + e + "</w:compatSetting>".len())
            };
            format!("{}{setting}{}", &xml[..start], &xml[end..])
        } else if let Some((a, b)) = crate::sect::find_element(&xml, "w:compat") {
            // `w:compatSetting` is the last child `CT_Compat` allows.
            if xml[a..b].ends_with("/>") {
                let open = xml[a..b - 2].trim_end();
                format!("{}{open}>{setting}</w:compat>{}", &xml[..a], &xml[b..])
            } else {
                let close = b - "</w:compat>".len();
                format!("{}{setting}{}", &xml[..close], &xml[close..])
            }
        } else {
            insert_compat(&xml, &format!("<w:compat>{setting}</w:compat>"))
        };
        self.set_part(&name, xml.into_bytes());
    }

    /// The number of newspaper columns in the body section (`w:cols w:num`).
    pub fn columns(&self) -> i32 {
        self.page_geom().cols
    }

    /// Set the number of newspaper columns (`w:cols w:num`) in the body section,
    /// with an equal gap. docxy still renders a single column on screen, but the
    /// column layout round-trips and Word lays it out in columns.
    pub fn set_columns(&mut self, num: i32) {
        let sect = self.sect_pr().to_string();
        let mut setup = crate::sect::SectionSetup::parse(&sect);
        setup.columns = setup.columns.equal(num, 720);
        self.set_current_sect_pr_raw(setup.apply(&sect));
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
        let sect = self.sect_pr().to_string();
        let mut setup = crate::sect::SectionSetup::parse(&sect);
        (
            setup.margins.top,
            setup.margins.right,
            setup.margins.bottom,
            setup.margins.left,
        ) = (top, right, bottom, left);
        self.set_current_sect_pr_raw(setup.apply(&sect));
    }

    /// Add a `<w:comment>` to `comments.xml`, creating the part + relationship +
    /// content-type if absent. `text` is the comment body (XML-escaped here).
    pub fn add_comment(&mut self, id: i32, author: &str, initials: &str, date: &str, text: &str) {
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
        self.insert_comment_xml(&comment);
    }

    /// Append `comment`, one whole `<w:comment>…</w:comment>` element (as
    /// [`Package::comment_xml`] returns it), to `comments.xml`, creating the
    /// part + relationship + content-type if absent.
    pub fn insert_comment_xml(&mut self, comment: &str) {
        self.insert_comment_xml_rooted(comment, None);
    }

    /// The start tag of `comments.xml`'s root, with every namespace it
    /// declares: what a comment written under it may rely on, kept to rebuild
    /// the part under it ([`Package::insert_comment_xml_rooted`]).
    pub fn comments_root_tag(&self) -> Option<String> {
        let xml = self.part_text("word/comments.xml")?;
        crate::load::start_tags(&xml, "w:comments")
            .into_iter()
            .next()
            .map(|(_, tag)| tag.to_string())
    }

    /// [`Package::insert_comment_xml`], creating a missing part under `root`
    /// (the start tag [`Package::comments_root_tag`] returned) when given, so
    /// the comment's own prefixes (`r:`, `w16du:`, a drawing's…) stay declared.
    pub fn insert_comment_xml_rooted(&mut self, comment: &str, root: Option<&str>) {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let name = "word/comments.xml";
        if self.part(name).is_some() {
            // Decoded and written back in the part's own encoding: a
            // UTF-16 comments.xml stays UTF-16 (#971).
            if let Some(xml) = self.part_text(name) {
                let xml = xml.replacen("</w:comments>", &format!("{comment}</w:comments>"), 1);
                self.set_part_text(name, &xml);
            }
            return;
        }
        // The namespaces a comment written by Word uses (`w14:paraId` in its
        // paragraphs), declared and ignorable for readers that do not know them.
        let body = match root.filter(|r| r.starts_with("<w:comments") && !r.ends_with("/>")) {
            Some(root) => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                 {root}{comment}</w:comments>"
            ),
            None => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                 <w:comments xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" \
                 xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" \
                 xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" \
                 xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\" \
                 mc:Ignorable=\"w14 w15\">{comment}</w:comments>"
            ),
        };
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

    /// Remove the `<w:comment>` whose `w:id` is `id` from `comments.xml`,
    /// whatever the order of its attributes.
    pub fn remove_comment(&mut self, id: i32) {
        self.remove_comment_id(&id.to_string());
    }

    /// [`Package::remove_comment`] by the `w:id` exactly as written: `03`
    /// is not `3`.
    pub fn remove_comment_id(&mut self, id: &str) {
        let name = "word/comments.xml";
        let Some(xml) = self.part_text(name) else {
            return;
        };
        if let Some(range) = comment_range(&xml, id) {
            let para_id = crate::comments::parse_comments_xml(&xml)
                .into_iter()
                .find(|c| c.id == id)
                .and_then(|c| c.para_id);
            let mut out = xml;
            out.replace_range(range, "");
            self.set_part_text(name, &out);
            if let Some(para_id) = para_id {
                self.remove_comment_extras(&para_id);
            }
        }
    }

    /// The `commentsExtended`, `commentsIds` and `commentsExtensible` entries
    /// of comment `id` (each whole element, children included), with the root
    /// start tag of the part it came from, for
    /// [`Package::restore_comment_extras`]: what a removal takes (the reply
    /// link `w15:paraIdParent`, the durable id, its UTC date) and the comment's
    /// own XML does not carry.
    pub fn comment_extras(&self, id: &str) -> Vec<CommentExtra> {
        let Some(xml) = self.part_text("word/comments.xml") else {
            return Vec::new();
        };
        let Some(para_id) = crate::comments::parse_comments_xml(&xml)
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.para_id)
        else {
            return Vec::new();
        };
        // Every element of `part` named `tag` whose `attr` is one of `keys`.
        let entries = |part: &str, tag: &str, attr: &str, keys: &[String]| {
            let mut out = Vec::new();
            let Some(xml) = self.part_text(part) else {
                return out;
            };
            let root = comment_part_root(&xml, part).unwrap_or_default();
            let mut from = 0;
            while let Some((a, b, _)) = crate::inspect::find_element_from(&xml, tag, from) {
                let el = &xml[a..b];
                if crate::load::xml_attr_value(el, attr).is_some_and(|v| keys.contains(&v)) {
                    out.push(CommentExtra {
                        part: part.to_string(),
                        element: el.to_string(),
                        root: root.clone(),
                    });
                }
                from = b;
            }
            out
        };
        let mut out = Vec::new();
        for (part, tag, attr) in COMMENT_EXTRAS {
            out.extend(entries(part, tag, attr, std::slice::from_ref(&para_id)));
        }
        // `commentsExtensible` is keyed by the durable id `commentsIds` gave it.
        let durable: Vec<String> = out
            .iter()
            .filter_map(|e| crate::load::xml_attr_value(&e.element, "w16cid:durableId"))
            .collect();
        let (part, tag, attr) = COMMENTS_EXTENSIBLE;
        out.extend(entries(part, tag, attr, &durable));
        out
    }

    /// Put back entries [`Package::comment_extras`] returned, creating a part
    /// the removal dropped (with its content-type override and relationship)
    /// under the root it had, so the namespaces its entries use stay declared.
    pub fn restore_comment_extras(&mut self, extras: &[CommentExtra]) {
        for extra in extras {
            let part = extra.part.as_str();
            let Some((_, tag, attr)) = COMMENT_EXTRAS
                .iter()
                .chain([&COMMENTS_EXTENSIBLE])
                .find(|(p, ..)| *p == part)
            else {
                continue;
            };
            let key = crate::load::xml_attr_value(&extra.element, attr);
            let root = match *tag {
                "w15:commentEx" => "w15:commentsEx",
                "w16cex:commentExtensible" => "w16cex:commentsExtensible",
                _ => "w16cid:commentsIds",
            };
            match self.part_text(part) {
                Some(xml) => {
                    let mut from = 0;
                    let mut present = false;
                    while let Some((a, b, _)) = crate::inspect::find_element_from(&xml, tag, from) {
                        present |= crate::load::xml_attr_value(&xml[a..b], attr) == key;
                        from = b;
                    }
                    if present {
                        continue;
                    }
                    if let Some(close) = xml.rfind(&format!("</{root}>")) {
                        let out = format!("{}{}{}", &xml[..close], extra.element, &xml[close..]);
                        self.set_part_text(part, &out);
                    }
                }
                None => {
                    let (ns, ct, rel) = match root {
                        "w15:commentsEx" => (
                            "xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"",
                            "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml",
                            "http://schemas.microsoft.com/office/2011/relationships/commentsExtended",
                        ),
                        "w16cex:commentsExtensible" => (
                            "xmlns:w16cex=\"http://schemas.microsoft.com/office/word/2018/wordml/cex\"",
                            "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtensible+xml",
                            "http://schemas.microsoft.com/office/2018/08/relationships/commentsExtensible",
                        ),
                        _ => (
                            "xmlns:w16cid=\"http://schemas.microsoft.com/office/word/2016/wordml/cid\"",
                            "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsIds+xml",
                            "http://schemas.microsoft.com/office/2016/09/relationships/commentsIds",
                        ),
                    };
                    let open = if extra.root.starts_with(&format!("<{root}")) {
                        extra.root.clone()
                    } else {
                        format!("<{root} {ns}>")
                    };
                    let body = format!(
                        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                         {open}{}</{root}>",
                        extra.element
                    );
                    self.add_part_with_rel(part, ct, rel, body);
                }
            }
        }
    }

    /// Drop the `commentsExtended` / `commentsIds` entries of a removed
    /// comment's `para_id`.
    fn remove_comment_extras(&mut self, para_id: &str) {
        for (part, tag, attr) in COMMENT_EXTRAS {
            let Some(xml) = self.part_text(part) else {
                continue;
            };
            let out = remove_tags_matching(&xml, tag, |t| {
                crate::load::xml_attr_value(t, attr).as_deref() == Some(para_id)
            });
            if out != xml {
                self.set_part_text(part, &out);
            }
        }
    }

    /// Mark the comment whose `w:id` is `id` (as written) resolved or
    /// reopened: `w15:done` on its `w15:commentEx` in
    /// `word/commentsExtended.xml`. The part (with its content-type override
    /// and relationship) and the `w14:paraId` on the comment's last paragraph
    /// are created when absent; an existing entry is patched in place, so
    /// its other attributes (`w15:paraIdParent` of a reply) survive. False
    /// when there is no such comment or it has no paragraph to key on.
    pub fn set_comment_resolved(&mut self, id: &str, resolved: bool) -> bool {
        let name = "word/comments.xml";
        let Some(xml) = self.part_text(name) else {
            return false;
        };
        let Some(range) = comment_range(&xml, id) else {
            return false;
        };
        let el = &xml[range.clone()];
        let para_id = match crate::comments::parse_comments_xml(&xml)
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.para_id)
        {
            Some(p) => p,
            None => {
                let fresh = self.fresh_para_id(id);
                let Some(patched) = add_last_para_id(el, &fresh) else {
                    return false;
                };
                let mut out = xml.clone();
                out.replace_range(range, &patched);
                self.set_part_text(name, &ensure_w14_root(&out));
                fresh
            }
        };
        self.set_comment_done(&para_id, resolved);
        true
    }

    /// An unused `w14:paraId` (eight hex digits below 0x80000000), seeded
    /// from the comment id so the result is deterministic.
    fn fresh_para_id(&self, seed: &str) -> String {
        let used: String = ["word/comments.xml", "word/document.xml"]
            .iter()
            .filter_map(|n| self.part_text(n))
            .collect();
        let mut n: u32 = 0x1000_0000
            + seed
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)))
                % 0x1000_0000;
        loop {
            let cand = format!("{n:08X}");
            if !used.contains(&format!("\"{cand}\"")) {
                return cand;
            }
            n += 1;
        }
    }

    fn set_comment_done(&mut self, para_id: &str, resolved: bool) {
        const W15_NS: &str = "http://schemas.microsoft.com/office/word/2012/wordml";
        let name = crate::comments::COMMENTS_EXTENDED_PART;
        let done = if resolved { "1" } else { "0" };
        let entry = format!("<w15:commentEx w15:paraId=\"{para_id}\" w15:done=\"{done}\"/>");
        let Some(xml) = self.part_text(name) else {
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                 <w15:commentsEx xmlns:w15=\"{W15_NS}\">{entry}</w15:commentsEx>"
            );
            self.add_part_with_rel(
                name,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml",
                "http://schemas.microsoft.com/office/2011/relationships/commentsExtended",
                body,
            );
            return;
        };
        let existing = crate::load::start_tags(&xml, "w15:commentEx")
            .into_iter()
            .find(|(_, tag)| {
                crate::load::xml_attr_value(tag, "w15:paraId").as_deref() == Some(para_id)
            });
        let out = match existing {
            Some((at, tag)) => {
                let new_tag = set_attr_value(tag, "w15:done", done);
                format!("{}{}{}", &xml[..at], new_tag, &xml[at + tag.len()..])
            }
            None => match xml.rfind("</w15:commentsEx>") {
                Some(close) => format!("{}{}{}", &xml[..close], entry, &xml[close..]),
                None => return,
            },
        };
        self.set_part_text(name, &out);
    }

    /// Add a new part with its content-type override and a relationship from
    /// the main document part.
    fn add_part_with_rel(&mut self, name: &str, content_type: &str, rel_type: &str, body: String) {
        self.parts.push((name.to_string(), body.into_bytes()));
        // Read and written back the way each part is encoded (UTF-16 stays).
        if let Some(ct) = self.part_text("[Content_Types].xml") {
            if !ct.contains(&format!("/{name}\"")) {
                let ov = format!("<Override PartName=\"/{name}\" ContentType=\"{content_type}\"/>");
                self.set_part_text(
                    "[Content_Types].xml",
                    &ct.replacen("</Types>", &format!("{ov}</Types>"), 1),
                );
            }
        }
        let rels_name = "word/_rels/document.xml.rels";
        if let Some(rels) = self.part_text(rels_name) {
            let target = name.strip_prefix("word/").unwrap_or(name);
            if !rels.contains(&format!("Target=\"{target}\"")) {
                let rid = next_rid(&rels);
                let rel =
                    format!("<Relationship Id=\"{rid}\" Type=\"{rel_type}\" Target=\"{target}\"/>");
                self.set_part_text(
                    rels_name,
                    &rels.replacen("</Relationships>", &format!("{rel}</Relationships>"), 1),
                );
            }
        }
    }

    /// Drop the comment parts once `comments.xml` holds no comment.
    pub fn drop_empty_comment_parts(&mut self) {
        const PARTS: [&str; 4] = [
            "word/comments.xml",
            "word/commentsExtended.xml",
            "word/commentsIds.xml",
            "word/commentsExtensible.xml",
        ];
        if self.part("word/comments.xml").is_none() || !self.comment_ids().is_empty() {
            return;
        }
        self.retain_parts(|n| !PARTS.contains(&n));
        if let Some(ct) = self.part_text("[Content_Types].xml") {
            let out = remove_tags_matching(&ct, "Override", |tag| {
                PARTS
                    .iter()
                    .any(|p| tag.contains(&format!("PartName=\"/{p}\"")))
            });
            self.set_part_text("[Content_Types].xml", &out);
        }
        let rels_name = "word/_rels/document.xml.rels";
        if let Some(rels) = self.part_text(rels_name) {
            let out = remove_tags_matching(&rels, "Relationship", |tag| {
                PARTS.iter().any(|p| {
                    let target = p.strip_prefix("word/").unwrap_or(p);
                    tag.contains(&format!("Target=\"{target}\""))
                })
            });
            self.set_part_text(rels_name, &out);
        }
    }

    /// The `<w:comment>…</w:comment>` element whose `w:id` is `id`, exactly
    /// as `comments.xml` holds it: what [`Package::insert_comment_xml`]
    /// writes back losslessly.
    /// `id` is the `w:id` as written (`03` is not `3`).
    pub fn comment_xml(&self, id: &str) -> Option<String> {
        let xml = self.part_text("word/comments.xml")?;
        comment_range(&xml, id).map(|range| xml[range].to_string())
    }

    /// The `w:id` of every `<w:comment>` in `comments.xml`, exactly as
    /// written and in order, read the way the loader decodes the part
    /// (UTF-16 included).
    pub fn comment_ids(&self) -> Vec<String> {
        let Some(xml) = self.part_text("word/comments.xml") else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        let mut from = 0;
        while let Some((start, end, _)) = crate::inspect::find_element_from(&xml, "w:comment", from)
        {
            if let Some(id) = comment_id_at(&xml, start) {
                ids.push(id);
            }
            from = end;
        }
        ids
    }

    /// Ensure `numbering.xml` defines a simple bullet (or decimal) list and return
    /// its `numId` ([`BULLET_LIST_NUM_ID`] or [`NUMBER_LIST_NUM_ID`]), creating
    /// the part + relationship + content-type if absent. Used by the
    /// Bullets/Numbering ribbon commands so applied lists render and save.
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
        let (num_id, abs_id) = if bullet {
            (BULLET_LIST_NUM_ID, BULLET_LIST_NUM_ID)
        } else {
            (NUMBER_LIST_NUM_ID, NUMBER_LIST_NUM_ID)
        };
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
    /// a fresh markdown package — plus Word's `Header` and `Footer`, which new
    /// header/footer parts use; any other id is silently ignored). Strictly
    /// additive, mirroring [`Package::ensure_list`]'s idiom: a style id already
    /// defined in the package — e.g. a third-party document's own `Heading1` —
    /// is left byte-untouched; only ids genuinely ABSENT from `styles.xml` get
    /// a definition appended. A package with no `styles.xml` at all gets one,
    /// with its document relationship and content-type override, so Word
    /// reads it.
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
                add_styles_part(&mut self.parts, body);
            }
        }
    }
}

/// Add a `word/styles.xml` part holding `body`, and wire it up: its
/// content-type override and the main document's styles relationship, each
/// only when missing.
fn add_styles_part(parts: &mut Vec<(String, Vec<u8>)>, body: String) {
    const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    parts.push(("word/styles.xml".to_string(), body.into_bytes()));
    if let Some((_, ct)) = parts.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
        let xml = String::from_utf8_lossy(ct).into_owned();
        if !xml.contains("/word/styles.xml") {
            let ov = "<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>";
            *ct = xml
                .replacen("</Types>", &format!("{ov}</Types>"), 1)
                .into_bytes();
        }
    }
    if let Some((_, rels)) = parts
        .iter_mut()
        .find(|(n, _)| n == "word/_rels/document.xml.rels")
    {
        let xml = String::from_utf8_lossy(rels).into_owned();
        if !xml.contains("/styles\"") {
            let rid = next_rid(&xml);
            let rel = format!(
                "<Relationship Id=\"{rid}\" Type=\"{R_NS}/styles\" Target=\"styles.xml\"/>"
            );
            *rels = xml
                .replacen("</Relationships>", &format!("{rel}</Relationships>"), 1)
                .into_bytes();
        }
    }
}

/// Define in `styles.xml` every built-in table style the package's tables
/// reference but the part lacks (#648): picking a gallery style writes only
/// `w:tblStyle`, so the definition is added here, at save, strictly
/// additively. Tables in the body and in header, footer and note parts count.
/// The part keeps its encoding (UTF-8, with or without a BOM, or UTF-16); one
/// that cannot be decoded is left alone. A package without a styles part gets
/// one (see [`add_styles_part`]), or Word would draw a new Table Grid table
/// without borders.
fn add_referenced_table_styles(parts: &mut Vec<(String, Vec<u8>)>, document: &Document) {
    let mut ids = crate::table_styles::referenced_table_styles(document);
    for (name, bytes) in parts.iter() {
        let story = name.starts_with("word/header")
            || name.starts_with("word/footer")
            || name == "word/footnotes.xml"
            || name == "word/endnotes.xml";
        if !story {
            continue;
        }
        if let Some(xml) = decode_xml_part(bytes) {
            for id in crate::table_styles::table_style_ids_in_xml(&xml) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    if ids.is_empty() {
        return;
    }
    let Some((_, bytes)) = parts.iter_mut().find(|(n, _)| n == "word/styles.xml") else {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let empty = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <w:styles xmlns:w=\"{W_NS}\"></w:styles>"
        );
        if let Some(body) = crate::table_styles::with_table_styles(&empty, &ids) {
            add_styles_part(parts, body);
        }
        return;
    };
    let Some(xml) = decode_xml_part(bytes) else {
        return;
    };
    if let Some(updated) = crate::table_styles::with_table_styles(&xml, &ids) {
        *bytes = encode_like(bytes, &updated);
    }
}

/// `text` encoded the way `original`, an XML part [`decode_xml_part`]
/// accepted, was: UTF-16LE/BE (keeping a byte-order mark if it had one), or
/// UTF-8 (keeping a BOM if it had one).
fn encode_like(original: &[u8], text: &str) -> Vec<u8> {
    let utf16 = |le: bool, bom: bool| {
        let mut out = Vec::with_capacity(text.len() * 2 + 2);
        if bom {
            out.extend_from_slice(if le { &[0xff, 0xfe] } else { &[0xfe, 0xff] });
        }
        for u in text.encode_utf16() {
            out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        out
    };
    if original.starts_with(&[0xff, 0xfe]) {
        utf16(true, true)
    } else if original.starts_with(&[0xfe, 0xff]) {
        utf16(false, true)
    } else if original.starts_with(&[b'<', 0, b'?', 0]) {
        utf16(true, false)
    } else if original.starts_with(&[0, b'<', 0, b'?']) {
        utf16(false, false)
    } else if original.starts_with(&[0xef, 0xbb, 0xbf]) {
        let mut out = vec![0xef, 0xbb, 0xbf];
        out.extend_from_slice(text.as_bytes());
        out
    } else {
        text.as_bytes().to_vec()
    }
}

/// The `<w:style>` XML definition for one of the styles Markdown maps onto
/// (`HeadingN` for `N` in `1..=6`, `Quote`, `SourceCode`, `Code`) or of Word's
/// `Header`/`Footer`, or `None` for any other id. Shared by [`markdown_styles_xml`] (which defines the full
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
        // Word's built-in Header and Footer styles: a centre tab at the middle
        // and a right tab at the right margin of a Letter page with 1" margins
        // (3.25" and 6.5"), no space after.
        "Header" | "Footer" => Some(format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"{id}\">\
             <w:name w:val=\"{name}\"/><w:basedOn w:val=\"Normal\"/>\
             <w:uiPriority w:val=\"99\"/><w:unhideWhenUsed/>\
             <w:pPr><w:tabs><w:tab w:val=\"center\" w:pos=\"4680\"/>\
             <w:tab w:val=\"right\" w:pos=\"9360\"/></w:tabs>\
             <w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr></w:style>",
            name = id.to_ascii_lowercase()
        )),
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

/// A mail-merge main document's type (`w:mainDocumentType`): what Finish &
/// Merge makes of it and how its copies are separated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MainDocType {
    #[default]
    Letters,
    Email,
    Envelopes,
    Labels,
    Directory,
}

impl MainDocType {
    pub fn as_ooxml(self) -> &'static str {
        match self {
            MainDocType::Letters => "formLetters",
            MainDocType::Email => "email",
            MainDocType::Envelopes => "envelopes",
            MainDocType::Labels => "mailingLabels",
            MainDocType::Directory => "catalog",
        }
    }

    pub fn from_ooxml(v: &str) -> Option<MainDocType> {
        Some(match v {
            "formLetters" => MainDocType::Letters,
            "email" => MainDocType::Email,
            "envelopes" => MainDocType::Envelopes,
            "mailingLabels" => MainDocType::Labels,
            "catalog" => MainDocType::Directory,
            _ => return None,
        })
    }
}

/// A document's mail-merge setup: what kind of main document it is and the
/// data source file it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailMerge {
    pub doc_type: MainDocType,
    pub source: Option<String>,
}

/// A file path as Word writes a mailMergeSource target: a drive path as
/// `file:///C:\x.csv`, a POSIX one as `file:///home/x.csv`, a UNC one as
/// `file://server/share/x.csv`. A path that is none of those (relative) is
/// written as it is.
fn path_to_file_url(path: &str) -> String {
    let esc = |p: &str| p.replace('%', "%25").replace(' ', "%20");
    let b = path.as_bytes();
    if let Some(unc) = path
        .strip_prefix("\\\\")
        .or_else(|| path.strip_prefix("//"))
        .filter(|rest| !rest.starts_with(['?', '.']))
    {
        return format!("file://{}", esc(&unc.replace('\\', "/")));
    }
    if b.get(1) == Some(&b':') && b[0].is_ascii_alphabetic() {
        return format!("file:///{}", esc(path));
    }
    if path.starts_with('/') {
        return format!("file://{}", esc(path));
    }
    path.to_string()
}

/// `%XX` escapes decoded.
/// Works on bytes: a `%` before a multibyte character (or at the end) is
/// kept as it is, never sliced through.
fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A mailMergeSource target as a path, the inverse of [`path_to_file_url`]:
/// `file:///C:\x` is a drive path, `file:///home/x` a POSIX one,
/// `file://server/share/x` the UNC path `\\server\share\x` (`localhost`
/// names this machine). Anything that is not a `file:` URL is returned as
/// written.
fn file_url_to_path(target: &str) -> String {
    if let Some(rest) = target.strip_prefix("file:///") {
        let b = rest.as_bytes();
        let path = if b.get(1) == Some(&b':') || rest.starts_with(['\\', '/']) {
            rest.to_string()
        } else {
            format!("/{rest}")
        };
        return percent_decode(&path);
    }
    if let Some(rest) = target.strip_prefix("file://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if host.eq_ignore_ascii_case("localhost") {
            return file_url_to_path(&format!("file:///{path}"));
        }
        return percent_decode(&format!("\\\\{host}\\{}", path.replace('/', "\\")));
    }
    target.to_string()
}

/// `CT_Settings` children that come before `w:mailMerge`.
const SETTINGS_BEFORE_MAIL_MERGE: [&str; 29] = [
    "w:writeProtection",
    "w:view",
    "w:zoom",
    "w:removePersonalInformation",
    "w:removeDateAndTime",
    "w:doNotDisplayPageBoundaries",
    "w:displayBackgroundShape",
    "w:printPostScriptOverText",
    "w:printFractionalCharacterWidth",
    "w:printFormsData",
    "w:embedTrueTypeFonts",
    "w:embedSystemFonts",
    "w:saveSubsetFonts",
    "w:saveFormsData",
    "w:mirrorMargins",
    "w:alignBordersAndEdges",
    "w:bordersDoNotSurroundHeader",
    "w:bordersDoNotSurroundFooter",
    "w:gutterAtTop",
    "w:hideSpellingErrors",
    "w:hideGrammaticalErrors",
    "w:activeWritingStyle",
    "w:proofState",
    "w:formsDesign",
    "w:attachedTemplate",
    "w:linkStyles",
    "w:stylePaneFormatFilter",
    "w:stylePaneSortMethod",
    "w:documentType",
];

/// Insert a `w:mailMerge` element into the settings root right after the
/// last element that precedes it in `CT_Settings` (every other child follows
/// it), or first. Expands a self-closing root.
fn insert_mail_merge(xml: &str, child: &str) -> String {
    insert_settings_child(xml, child, &SETTINGS_BEFORE_MAIL_MERGE)
}

/// Insert `child` into the settings root right after the last of the
/// elements `before` (its `CT_Settings` predecessors) present, or first.
/// Expands a self-closing root.
fn insert_settings_child(xml: &str, child: &str, before: &[&str]) -> String {
    let Some(root) = xml.find("<w:settings") else {
        return xml.to_string();
    };
    let Some(gt) = xml[root..].find('>').map(|g| root + g) else {
        return xml.to_string();
    };
    if xml[..gt].ends_with('/') {
        return format!("{}>{child}</w:settings>{}", &xml[..gt - 1], &xml[gt + 1..]);
    }
    let mut at = gt + 1;
    for &name in before {
        let mut from = gt + 1;
        while let Some((a, b)) = crate::sect::find_element(&xml[from..], name) {
            at = at.max(from + b);
            from += b.max(a + 1);
        }
    }
    format!("{}{child}{}", &xml[..at], &xml[at..])
}

/// The `w:compatSetting` start tag naming `compatibilityMode`, with its
/// offset in `xml`.
fn compatibility_mode_setting(xml: &str) -> Option<(usize, &str)> {
    crate::load::start_tags(xml, "w:compatSetting")
        .into_iter()
        .find(|(_, el)| {
            crate::load::xml_attr_value(el, "w:name").as_deref() == Some("compatibilityMode")
        })
}

/// `CT_Settings` children that come after `w:compat`, and the prefixes of
/// the extension elements Word writes at the end of the part.
const SETTINGS_AFTER_COMPAT: [&str; 21] = [
    "<w:docVars",
    "<w:rsids",
    "<m:mathPr",
    "<w:attachedSchema",
    "<w:themeFontLang",
    "<w:clrSchemeMapping",
    "<w:doNotIncludeSubdocsInStats",
    "<w:doNotAutoCompressPictures",
    "<w:forceUpgrade",
    "<w:captions",
    "<w:readModeInkLockDown",
    "<w:smartTagType",
    "<sl:schemaLibrary",
    "<w:shapeDefaults",
    "<w:doNotEmbedSmartTags",
    "<w:decimalSymbol",
    "<w:listSeparator",
    "<w14:",
    "<w15:",
    "<w16",
    "<mc:AlternateContent",
];

/// Insert a `w:compat` element into the settings root before the first
/// element `CT_Settings` puts after it, or last. Expands a self-closing root.
fn insert_compat(xml: &str, child: &str) -> String {
    let Some(root) = xml.find("<w:settings") else {
        return xml.to_string();
    };
    let Some(gt) = xml[root..].find('>').map(|g| root + g) else {
        return xml.to_string();
    };
    if xml[..gt].ends_with('/') {
        return format!("{}>{child}</w:settings>{}", &xml[..gt - 1], &xml[gt + 1..]);
    }
    let body = gt + 1;
    let at = SETTINGS_AFTER_COMPAT
        .iter()
        .filter_map(|open| {
            // `<w:rsids` must not match `<w:rsidsX`; a namespace prefix
            // (`<w14:`, `<w16…:`) matches any of its elements.
            let mut from = body;
            while let Some(rel) = xml[from..].find(open) {
                let at = from + rel;
                let next = xml[at + open.len()..].chars().next();
                if open.ends_with(':')
                    || *open == "<w16"
                    || next.is_some_and(|c| " />\t\r\n".contains(c))
                {
                    return Some(at);
                }
                from = at + open.len();
            }
            None
        })
        .min()
        .or_else(|| xml.rfind("</w:settings>"));
    match at {
        Some(at) => format!("{}{child}{}", &xml[..at], &xml[at..]),
        None => xml.to_string(),
    }
}

/// The next free relationship id (`rId{max+1}`) for a `.rels` part.
fn next_rid(rels: &str) -> String {
    format!("rId{}", next_rid_num(rels))
}

/// Remove the first `<name/>`, `<name .../>`, or `<name ...>…</name>` element.
/// An attribute's value out of a raw tag body (`w:val="false"`), either quote
/// style. `None` when the attribute isn't there.
pub(crate) fn tag_attr(attrs: &str, name: &str) -> Option<String> {
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
pub(crate) fn settings_flag_of(xml: &str, elem: &str) -> Option<bool> {
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
    let (document, sect_pr) = parse_main_document(&parts, doc_index)?;

    Ok(Package {
        parts,
        doc_index,
        sect_pr,
        document,
    })
}

/// The document and final `w:sectPr` that `parts[doc_index]` encodes, with
/// the relationships (and the diagram, equation and chart data they lead to)
/// its parse needs.
fn parse_main_document(
    parts: &[(String, Vec<u8>)],
    doc_index: usize,
) -> Result<(Document, String), LoadError> {
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
    Ok((document, sect_pr))
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

/// [`Package::final_section`] of a document and its captured `w:sectPr`.
fn final_section_of(document: &Document, sect_pr: &str) -> SectionProperties {
    if let Some(section) = document.trailing_section_properties() {
        return section.clone();
    }
    let xml = if sect_pr.trim().is_empty() {
        "<w:sectPr></w:sectPr>"
    } else {
        sect_pr
    };
    let (raw, property_change) =
        crate::load::split_property_change_container(xml, PropertyScope::Section);
    SectionProperties {
        raw,
        property_change,
    }
}

/// Serialize the package back to `.docx` bytes (STORED ZIP).
pub fn save_package(pkg: &Package) -> Vec<u8> {
    // External hyperlinks need a relationship (`r:id` → `.rels` Target) or their
    // URL is lost. Links we modelled from a loaded `.docx` already carry `rel_id`;
    // links created in-app or from Markdown have a `target` but no `rel_id`. Mint
    // a relationship for each before serializing so the URL survives the save.
    let mut document = pkg.document.clone();
    let mut parts = pkg.parts.clone();
    add_referenced_table_styles(&mut parts, &document);
    let rels_name = "word/_rels/document.xml.rels";
    if let Some((_, rels_bytes)) = parts.iter().find(|(n, _)| n == rels_name) {
        let mut new_rels = String::new();
        let mut next = next_rid_num(&String::from_utf8_lossy(rels_bytes));
        let mut links = Vec::new();
        collect_external_links(&mut document.body, &mut links);
        links.retain(|h| h.rel_id.is_none());
        for h in links {
            let rid = format!("rId{next}");
            next += 1;
            let target = h.target.as_deref().unwrap_or_default();
            new_rels.push_str(&format!(
                "<Relationship Id=\"{rid}\" Type=\"{HYPERLINK_REL}\" Target=\"{}\" TargetMode=\"External\"/>",
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
        // Content before `<w:body>` (in practice `w:background`, Word's page
        // colour, with its VML fill) is not modelled. Carry it over verbatim;
        // only here, where the original root declarations are re-emitted, so
        // its `v:`/`o:` prefixes stay bound.
        let prolog = document_prolog(&original_doc).unwrap_or_default();
        xml = format!(
            "{}<w:document {attrs}>{prolog}{}",
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

/// The raw XML between the `<w:document …>` start tag and `<w:body`, such as
/// `w:background`. `None` when there is no body or the slice is whitespace.
fn document_prolog(original: &str) -> Option<&str> {
    let mut parser = XmlParser::new(original);
    let mut root_end = None;
    loop {
        match parser.next() {
            Event::Start if root_end.is_none() && parser.name() == "w:document" => {
                root_end = Some(parser.pos());
            }
            Event::Start if parser.name() == "w:body" => {
                let prolog = parser.raw_slice(root_end?, parser.start_pos());
                return (!prolog.trim().is_empty()).then_some(prolog);
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Serialize a package without regenerating its main document part. This is the
/// correct same-format save path when the live document has no user-authorized
/// edits: all original OOXML wrappers and cached field results remain byte-for-
/// byte intact while the container itself may be rewritten.
pub fn save_package_preserving_document(pkg: &Package) -> Vec<u8> {
    write_zip(&pkg.parts)
}

/// [`save_package_preserving_document`] for a package whose other parts may
/// have been edited (a header or footer, say) while its body still is the
/// stored one ([`Package::stores_document`]): the main document part keeps
/// its bytes, and as in [`save_package`] the styles part gains the table
/// styles the body and the other stories reference (#1107).
pub fn save_package_keeping_document(pkg: &Package) -> Vec<u8> {
    let mut parts = pkg.parts.clone();
    add_referenced_table_styles(&mut parts, &pkg.document);
    write_zip(&parts)
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

const HYPERLINK_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

/// `rels` (a Relationships part's text) with `added` relationships appended to
/// its root, or `None` without a `Relationships` root.
fn append_relationships(rels: &str, added: &str) -> Option<String> {
    append_to_root(rels, "Relationships", added)
}

/// `added` as the last children of the `root` element (`Relationships`,
/// `Types`), expanding a self-closing root; `None` when there is no such
/// root.
fn append_to_root(xml: &str, root: &str, added: &str) -> Option<String> {
    let close = format!("</{root}>");
    if let Some(at) = xml.rfind(&close) {
        return Some(format!("{}{added}{}", &xml[..at], &xml[at..]));
    }
    let (start, el) = start_tags(xml, root).into_iter().next()?;
    let at = start + el.len();
    el.ends_with("/>")
        .then(|| format!("{}>{added}{close}{}", &xml[..at - 2], &xml[at..]))
}

/// A preserved `<w:hyperlink …>` element with its opening tag's `r:id` set to
/// `rid` (added when it had none); its children are untouched.
fn with_opener_rel_id(raw: &str, rid: &str) -> String {
    let mut parser = XmlParser::new(raw);
    if parser.next() != Event::Start || parser.name() != "w:hyperlink" {
        return raw.to_string();
    }
    // The value's span in `raw`, from the parser's own view of the start tag,
    // so any spacing around the attribute or its `=` is handled.
    let Some(attr) = parser.attrs().iter().find(|attr| attr.name == "r:id") else {
        return raw.replacen("<w:hyperlink", &format!("<w:hyperlink r:id=\"{rid}\""), 1);
    };
    // The parser's values are slices of `raw` (a valueless attribute isn't).
    let start = (attr.value.as_ptr() as usize).wrapping_sub(raw.as_ptr() as usize);
    let end = start.wrapping_add(attr.value.len());
    if end > raw.len() || start > end || !raw.is_char_boundary(start) {
        return raw.to_string();
    }
    format!("{}{rid}{}", &raw[..start], &raw[end..])
}

/// Collect `&mut` references to every external hyperlink (`target` set),
/// walking paragraphs and table cells recursively.
fn collect_external_links<'a>(
    blocks: &'a mut [crate::model::Block],
    out: &mut Vec<&'a mut crate::model::Hyperlink>,
) {
    use crate::model::{Block, Inline};
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                for inl in &mut p.content {
                    if let Inline::Hyperlink(h) = inl
                        && h.target.is_some()
                    {
                        out.push(h);
                    }
                }
            }
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        collect_external_links(&mut cell.blocks, out);
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

/// The parts that hold a per-comment entry keyed by `w14:paraId`: (part, entry
/// element, its paraId attribute).
const COMMENT_EXTRAS: [(&str, &str, &str); 2] = [
    ("word/commentsExtended.xml", "w15:commentEx", "w15:paraId"),
    ("word/commentsIds.xml", "w16cid:commentId", "w16cid:paraId"),
];

/// The root start tag of one of the comment parts [`Package::comment_extras`]
/// reads (`w15:commentsEx`, `w16cid:commentsIds`, `w16cex:commentsExtensible`).
fn comment_part_root(xml: &str, part: &str) -> Option<String> {
    let root = match part {
        "word/commentsExtended.xml" => "w15:commentsEx",
        "word/commentsExtensible.xml" => "w16cex:commentsExtensible",
        _ => "w16cid:commentsIds",
    };
    crate::load::start_tags(xml, root)
        .into_iter()
        .next()
        .map(|(_, t)| t.to_string())
}

/// One per-comment entry taken from a comment part, with that part's root: see
/// [`Package::comment_extras`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentExtra {
    pub part: String,
    pub element: String,
    pub root: String,
}

/// The part of per-comment entries keyed by the durable id (`commentsIds`)
/// rather than the paragraph id: (part, entry element, its durableId attribute).
const COMMENTS_EXTENSIBLE: (&str, &str, &str) = (
    "word/commentsExtensible.xml",
    "w16cex:commentExtensible",
    "w16cex:durableId",
);

/// `xml` without each self-closing `<name …/>` element `drop` accepts.
fn remove_tags_matching(xml: &str, name: &str, drop: impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut at = 0;
    for (start, tag) in crate::load::start_tags(xml, name) {
        if tag.ends_with("/>") && drop(tag) {
            out.push_str(&xml[at..start]);
            at = start + tag.len();
        }
    }
    out.push_str(&xml[at..]);
    out
}

/// `tag` (a start tag) with attribute `name` set to `value`, added before
/// the closing `>` or `/>` when it is absent.
fn set_attr_value(tag: &str, name: &str, value: &str) -> String {
    let key = format!("{name}=\"");
    if let Some(i) = tag.find(&key) {
        let v = i + key.len();
        if let Some(e) = tag[v..].find('"') {
            return format!("{}{}{}", &tag[..v], value, &tag[v + e..]);
        }
    }
    let cut = tag.len() - if tag.ends_with("/>") { 2 } else { 1 };
    format!("{} {key}{value}\"{}", tag[..cut].trim_end(), &tag[cut..])
}

/// `comment` (one `<w:comment>` element) with `w14:paraId` on its last
/// `<w:p>` start tag.
fn add_last_para_id(comment: &str, para_id: &str) -> Option<String> {
    let (at, tag) = crate::load::start_tags(comment, "w:p").into_iter().last()?;
    let patched = set_attr_value(tag, "w14:paraId", para_id);
    Some(format!(
        "{}{}{}",
        &comment[..at],
        patched,
        &comment[at + tag.len()..]
    ))
}

/// `comments.xml` with its root declaring `w14` and ignoring it for
/// consumers that do not know the namespace.
fn ensure_w14_root(xml: &str) -> String {
    const W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
    const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
    let Some((at, tag)) = crate::load::start_tags(xml, "w:comments")
        .into_iter()
        .next()
    else {
        return xml.to_string();
    };
    let mut new_tag = tag.to_string();
    let cut = |t: &str| t.len() - if t.ends_with("/>") { 2 } else { 1 };
    if !new_tag.contains("xmlns:w14=") {
        let c = cut(&new_tag);
        new_tag.insert_str(c, &format!(" xmlns:w14=\"{W14_NS}\""));
    }
    if let Some(i) = new_tag.find("mc:Ignorable=\"") {
        let v = i + "mc:Ignorable=\"".len();
        let end = new_tag[v..].find('"').map_or(v, |e| v + e);
        if !new_tag[v..end].split_whitespace().any(|t| t == "w14") {
            new_tag.insert_str(v, "w14 ");
        }
    } else {
        if !new_tag.contains("xmlns:mc=") {
            let c = cut(&new_tag);
            new_tag.insert_str(c, &format!(" xmlns:mc=\"{MC_NS}\""));
        }
        let c = cut(&new_tag);
        new_tag.insert_str(c, " mc:Ignorable=\"w14\"");
    }
    format!("{}{}{}", &xml[..at], new_tag, &xml[at + tag.len()..])
}

/// The byte range of the `<w:comment>` element in `xml` (a `comments.xml`)
/// whose `w:id` attribute is `id` as written, wherever that attribute sits
/// in its opening tag. Not `<w:comments>` or a `<w:commentRangeStart>`, and
/// `3` never matches `w:id="30"` or `w:id="03"`.
fn comment_range(xml: &str, id: &str) -> Option<std::ops::Range<usize>> {
    let mut from = 0;
    while let Some((start, end, _)) = crate::inspect::find_element_from(xml, "w:comment", from) {
        if comment_id_at(xml, start).as_deref() == Some(id) {
            return Some(start..end);
        }
        from = end;
    }
    None
}

/// The `w:id` of the `<w:comment>` opening tag at `start` in `xml`.
fn comment_id_at(xml: &str, start: usize) -> Option<String> {
    let head = crate::inspect::tag_end(&xml[start..])?;
    let mut p = XmlParser::new(&xml[start..start + head]);
    (p.next() == Event::Start).then(|| p.attr("w:id").to_string())
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

    fn mail_merge_of(pkg: &Package) -> (String, String) {
        let settings = pkg.part_text("word/settings.xml").unwrap_or_default();
        let rels = pkg
            .part_text("word/_rels/settings.xml.rels")
            .unwrap_or_default();
        (settings, rels)
    }

    const DOC_RELS: &str = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings\" Target=\"settings.xml\"/>\
        </Relationships>";

    /// #628: attaching a list writes `w:mailMerge` where CT_Settings puts
    /// it (after `w:proofState`/`w:documentType`, before `w:defaultTabStop`)
    /// and an External mailMergeSource relationship; attaching again
    /// replaces both; "Normal Word Document" removes both.
    #[test]
    fn mail_merge_settings_order_replace_and_remove_628() {
        let settings = "<?xml version=\"1.0\"?><w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
            <w:zoom w:percent=\"100\"/><w:proofState w:spelling=\"clean\"/><w:defaultTabStop w:val=\"720\"/>\
            <w:compat/></w:settings>";
        let mut pkg = load_package(&make_metadata_docx(
            BODY,
            Some(settings),
            Some(DOC_RELS),
            &[],
        ))
        .unwrap();
        assert_eq!(pkg.mail_merge(), None);
        pkg.set_mail_merge(Some(&MailMerge {
            doc_type: MainDocType::Letters,
            source: Some("C:\\Data\\My List.csv".into()),
        }));
        let (xml, rels) = mail_merge_of(&pkg);
        let proof = xml.find("<w:proofState").unwrap();
        let mm = xml.find("<w:mailMerge>").unwrap();
        let tab = xml.find("<w:defaultTabStop").unwrap();
        assert!(proof < mm && mm < tab, "{xml}");
        assert!(
            xml.contains(
                "<w:mailMerge><w:mainDocumentType w:val=\"formLetters\"/><w:linkToQuery/>\
             <w:dataType w:val=\"textFile\"/><w:connectString w:val=\"\"/>\
             <w:query w:val=\"SELECT * FROM C:\\Data\\My List.csv\"/>"
            ),
            "{xml}"
        );
        assert!(rels.contains("/mailMergeSource\" Target=\"file:///C:\\Data\\My%20List.csv\" TargetMode=\"External\""), "{rels}");
        let rid = crate::load::start_tags(&xml, "w:dataSource")
            .first()
            .and_then(|(_, el)| crate::load::xml_attr_value(el, "r:id"))
            .unwrap();
        assert!(rels.contains(&format!("Id=\"{rid}\"")), "{rels}");
        assert_eq!(
            pkg.mail_merge(),
            Some(MailMerge {
                doc_type: MainDocType::Letters,
                source: Some("C:\\Data\\My List.csv".into()),
            })
        );

        // Again, as labels from another file: one element, one relationship.
        pkg.set_mail_merge(Some(&MailMerge {
            doc_type: MainDocType::Labels,
            source: Some("/home/me/b.csv".into()),
        }));
        let (xml, rels) = mail_merge_of(&pkg);
        assert_eq!(xml.matches("<w:mailMerge>").count(), 1, "{xml}");
        assert_eq!(rels.matches("mailMergeSource").count(), 1, "{rels}");
        assert_eq!(pkg.mail_merge_source().as_deref(), Some("/home/me/b.csv"));
        assert_eq!(pkg.mail_merge().unwrap().doc_type, MainDocType::Labels);

        // A save and reload keeps it.
        let back = load_package(&save_package(&pkg)).unwrap();
        assert_eq!(back.mail_merge_source().as_deref(), Some("/home/me/b.csv"));

        pkg.set_mail_merge(None);
        let (xml, rels) = mail_merge_of(&pkg);
        assert!(!xml.contains("mailMerge"), "{xml}");
        assert!(!rels.contains("mailMergeSource"), "{rels}");
        assert!(xml.contains("<w:defaultTabStop"), "{xml}");
        assert_eq!(pkg.mail_merge(), None);
    }

    /// #634: `compatibilityMode` reads back what was set, whether the
    /// package had no settings part, settings without `w:compat`, a bare
    /// `<w:compat/>`, or a value already there (replaced, not duplicated).
    #[test]
    fn compatibility_mode_set_and_replace_634() {
        let count = |pkg: &Package| {
            pkg.part_text("word/settings.xml")
                .unwrap_or_default()
                .matches("compatibilityMode")
                .count()
        };
        // No settings part: one is created, with its relationship.
        let mut pkg = new_package(Document::default());
        assert_eq!(pkg.compatibility_mode(), None);
        pkg.set_compatibility_mode(11);
        assert_eq!(pkg.compatibility_mode(), Some(11));
        pkg.set_compatibility_mode(15);
        assert_eq!(pkg.compatibility_mode(), Some(15));
        assert_eq!(count(&pkg), 1);
        let reloaded = load_package(&save_package(&pkg)).unwrap();
        assert_eq!(reloaded.compatibility_mode(), Some(15));

        let with = |settings: &str| {
            load_package(&make_metadata_docx(
                BODY,
                Some(settings),
                Some(DOC_RELS),
                &[],
            ))
            .unwrap()
        };
        const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";
        // Settings without w:compat: it goes before w:rsids, as CT_Settings orders it.
        let mut pkg = with(&format!(
            "<w:settings {W}><w:zoom w:percent=\"100\"/><w:rsids><w:rsidRoot w:val=\"1\"/></w:rsids></w:settings>"
        ));
        pkg.set_compatibility_mode(15);
        let xml = pkg.part_text("word/settings.xml").unwrap();
        let (zoom, compat, rsids) = (
            xml.find("<w:zoom").unwrap(),
            xml.find("<w:compat>").unwrap(),
            xml.find("<w:rsids>").unwrap(),
        );
        assert!(zoom < compat && compat < rsids, "{xml}");
        assert_eq!(pkg.compatibility_mode(), Some(15));

        // A bare `<w:compat/>` is expanded around the setting.
        let mut pkg = with(&format!("<w:settings {W}><w:compat/></w:settings>"));
        pkg.set_compatibility_mode(11);
        let xml = pkg.part_text("word/settings.xml").unwrap();
        assert!(
            xml.contains("<w:compat><w:compatSetting w:name=\"compatibilityMode\""),
            "{xml}"
        );
        assert_eq!(pkg.compatibility_mode(), Some(11));

        // An existing value is replaced in place; other compat settings stay.
        let mut pkg = with(&format!(
            "<w:settings {W}><w:compat><w:compatSetting w:name=\"compatibilityMode\" \
             w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"14\"/>\
             <w:compatSetting w:name=\"overrideTableStyleFontSizeAndJustification\" \
             w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"1\"/></w:compat></w:settings>"
        ));
        assert_eq!(pkg.compatibility_mode(), Some(14));
        pkg.set_compatibility_mode(15);
        assert_eq!(pkg.compatibility_mode(), Some(15));
        assert_eq!(count(&pkg), 1);
        let xml = pkg.part_text("word/settings.xml").unwrap();
        assert!(
            xml.contains("overrideTableStyleFontSizeAndJustification"),
            "{xml}"
        );
    }

    /// #628: with no settings part at all, attaching creates it, its content
    /// type, its document relationship and its relationships part.
    #[test]
    fn mail_merge_creates_the_settings_part_628() {
        let ct = "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
            <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/></Types>";
        let rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>";
        let mut pkg = load_package(&write_zip(&[
            ("[Content_Types].xml".to_string(), ct.as_bytes().to_vec()),
            ("word/document.xml".to_string(), BODY.as_bytes().to_vec()),
            (
                "word/_rels/document.xml.rels".to_string(),
                rels.as_bytes().to_vec(),
            ),
        ]))
        .unwrap();
        assert!(pkg.part("word/settings.xml").is_none());
        pkg.set_mail_merge(Some(&MailMerge {
            doc_type: MainDocType::Directory,
            source: Some("C:\\a.csv".into()),
        }));
        let (xml, srels) = mail_merge_of(&pkg);
        assert!(
            xml.contains("<w:mailMerge><w:mainDocumentType w:val=\"catalog\"/>"),
            "{xml}"
        );
        assert!(srels.contains("TargetMode=\"External\""), "{srels}");
        assert!(
            pkg.part_text("[Content_Types].xml")
                .unwrap()
                .contains("settings+xml")
        );
        assert!(
            pkg.part_text("word/_rels/document.xml.rels")
                .unwrap()
                .contains("Target=\"settings.xml\"")
        );
        let back = load_package(&save_package(&pkg)).unwrap();
        assert_eq!(back.mail_merge_source().as_deref(), Some("C:\\a.csv"));
        // Removing from a document that never had one creates nothing.
        let mut bare = load_package(&make_docx(BODY)).unwrap();
        bare.set_mail_merge(None);
        assert!(bare.part("word/settings.xml").is_none());
    }

    /// Every kind of path a mailMergeSource target holds comes back as it
    /// was written (r1 m3).
    #[test]
    fn mail_merge_source_urls_round_trip_628() {
        for (path, url) in [
            ("C:\\Data\\My List.csv", "file:///C:\\Data\\My%20List.csv"),
            ("/home/me/b.csv", "file:///home/me/b.csv"),
            (
                "\\\\server\\share\\l s.csv",
                "file://server/share/l%20s.csv",
            ),
            ("list.csv", "list.csv"),
            ("C:\\100%.csv", "file:///C:\\100%25.csv"),
        ] {
            assert_eq!(path_to_file_url(path), url, "{path}");
            assert_eq!(file_url_to_path(url), path, "{url}");
        }
        assert_eq!(file_url_to_path("file://localhost/C:/x.csv"), "C:/x.csv");
        // r2 C1: a `%` that is not an escape, before a multibyte character
        // or at the end, is kept; nothing panics.
        for (url, path) in [
            ("file:///C:/%a\u{e9}.csv", "C:/%a\u{e9}.csv"),
            ("file:///C:/50%\u{e9}.csv", "C:/50%\u{e9}.csv"),
            ("file:///C:/x%", "C:/x%"),
            ("file:///C:/x%4", "C:/x%4"),
            ("file:///C:/x%41", "C:/xA"),
            ("file:///C:/%zz.csv", "C:/%zz.csv"),
        ] {
            assert_eq!(file_url_to_path(url), path, "{url}");
        }
        // What the first version wrote for a UNC path is left alone.
        assert_eq!(
            file_url_to_path("file:///\\\\server\\s\\x.csv"),
            "\\\\server\\s\\x.csv"
        );
    }

    /// r2 C1: a document whose mailMergeSource target has a `%` before a
    /// multibyte character opens: reading its merge setup does not panic.
    #[test]
    fn mail_merge_source_with_a_stray_percent_does_not_panic_628() {
        let settings = "<w:settings xmlns:w=\"w\"><w:mailMerge><w:mainDocumentType w:val=\"formLetters\"/>\
            <w:dataSource r:id=\"rId1\"/></w:mailMerge></w:settings>";
        let srels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/mailMergeSource\" \
            Target=\"file:///C:/%a\u{e9}.csv\" TargetMode=\"External\"/></Relationships>";
        let mut docx = load_package(&make_metadata_docx(
            BODY,
            Some(settings),
            Some(DOC_RELS),
            &[],
        ))
        .unwrap();
        docx.parts.push((
            "word/_rels/settings.xml.rels".into(),
            srels.as_bytes().to_vec(),
        ));
        let pkg = load_package(&save_package(&docx)).unwrap();
        assert_eq!(pkg.mail_merge_source().as_deref(), Some("C:/%a\u{e9}.csv"));
    }

    /// A data source named only by `w:query` (no relationship) is still read.
    #[test]
    fn mail_merge_source_from_the_query_628() {
        let settings = "<w:settings xmlns:w=\"w\"><w:mailMerge><w:mainDocumentType w:val=\"email\"/>\
            <w:query w:val=\"SELECT * FROM `C:\\x\\list.csv` \"/></w:mailMerge></w:settings>";
        let pkg = load_package(&make_metadata_docx(
            BODY,
            Some(settings),
            Some(DOC_RELS),
            &[],
        ))
        .unwrap();
        assert_eq!(
            pkg.mail_merge(),
            Some(MailMerge {
                doc_type: MainDocType::Email,
                source: Some("C:\\x\\list.csv".into()),
            })
        );
    }

    /// #628: Word's envelope delivery address carries `w:wrap="auto"`; a
    /// save used to rewrite it as an empty `<w:framePr/>`. Every CT_FramePr
    /// attribute survives too.
    #[test]
    fn frame_pr_attributes_survive_save_628() {
        const ALL: &str = "<w:framePr w:dropCap=\"drop\" w:lines=\"3\" w:w=\"4320\" \
            w:h=\"1440\" w:vSpace=\"10\" w:hSpace=\"180\" w:wrap=\"around\" w:hAnchor=\"page\" \
            w:vAnchor=\"text\" w:x=\"100\" w:xAlign=\"center\" w:y=\"200\" w:yAlign=\"bottom\" \
            w:hRule=\"exact\" w:anchorLock=\"1\"/>";
        let doc = format!(
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
             <w:p><w:pPr><w:framePr w:wrap=\"auto\"/></w:pPr><w:r><w:t>A</w:t></w:r></w:p>\
             <w:p><w:pPr>{ALL}</w:pPr><w:r><w:t>B</w:t></w:r></w:p>\
             <w:sectPr/></w:body></w:document>"
        );
        let mut pkg = load_package(&make_docx(&doc)).unwrap();
        // A real edit, so the body is re-serialized rather than kept verbatim.
        pkg.document
            .body
            .insert(0, crate::model::Block::Paragraph(Default::default()));
        let saved = load_package(&save_package(&pkg)).unwrap();
        let xml = saved.part_text("word/document.xml").unwrap();
        assert!(xml.contains("<w:framePr w:wrap=\"auto\"/>"), "{xml}");
        assert!(xml.contains(ALL), "{xml}");
    }

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
    fn section_setters_keep_schema_order() {
        use crate::model::{Block, Document, Paragraph};
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        pkg.set_sect_pr(
            "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgNumType w:start=\"1\"/>             <w:docGrid w:linePitch=\"360\"/></w:sectPr>"
                .into(),
        );
        pkg.set_columns(2);
        pkg.set_page_margins(720, 720, 720, 720);
        pkg.set_sect_pr(crate::sect::set_flag(pkg.sect_pr(), "w:titlePg", true));
        let s = pkg.sect_pr();
        let at = |n: &str| s.find(n).unwrap_or_else(|| panic!("{n} missing: {s}"));
        assert!(at("<w:pgSz") < at("<w:pgMar"), "{s}");
        assert!(at("<w:pgNumType") < at("<w:cols"), "{s}");
        assert!(at("<w:cols") < at("<w:titlePg"), "{s}");
        assert!(at("<w:titlePg") < at("<w:docGrid"), "{s}");
        assert_eq!(pkg.page_geom().ml, 720);

        assert!(!pkg.has_mirror_margins());
        pkg.set_mirror_margins(true);
        pkg.set_gutter_at_top(true);
        assert!(pkg.has_mirror_margins() && pkg.has_gutter_at_top());
        pkg.set_mirror_margins(false);
        assert!(!pkg.has_mirror_margins() && pkg.has_gutter_at_top());
    }

    #[test]
    fn set_columns_keeps_the_line_between() {
        use crate::model::{Block, Document, Paragraph};
        let mut pkg = new_package(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        pkg.set_sect_pr(
            "<w:sectPr><w:cols w:num=\"2\" w:sep=\"1\" w:space=\"720\" w:equalWidth=\"0\">             <w:col w:w=\"3000\" w:space=\"720\"/><w:col w:w=\"5640\"/></w:cols></w:sectPr>"
                .into(),
        );
        pkg.set_columns(3);
        assert!(pkg.sect_pr().contains("w:sep=\"1\""), "{}", pkg.sect_pr());
        assert!(!pkg.sect_pr().contains("<w:col "), "{}", pkg.sect_pr());
        assert_eq!(pkg.columns(), 3);
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
        assert!(!crate::sect::has_flag(pkg.sect_pr(), "w:titlePg"));
        pkg.set_sect_pr(crate::sect::set_flag(pkg.sect_pr(), "w:titlePg", true));
        assert!(
            crate::sect::has_flag(pkg.sect_pr(), "w:titlePg")
                && pkg.sect_pr().contains("<w:titlePg/>")
        );
        pkg.set_sect_pr(crate::sect::set_flag(pkg.sect_pr(), "w:titlePg", true)); // idempotent
        assert_eq!(pkg.sect_pr().matches("<w:titlePg").count(), 1);
        let first = pkg.create_hf(true, "first").expect("first header");
        assert!(pkg.sect_pr().contains("w:type=\"first\""));
        assert!(
            crate::load::header_footer_ref_rid(pkg.sect_pr(), "headerReference", "first").is_some()
        );
        pkg.set_sect_pr(crate::sect::set_flag(pkg.sect_pr(), "w:titlePg", false));
        assert!(
            !crate::sect::has_flag(pkg.sect_pr(), "w:titlePg")
                && !pkg.sect_pr().contains("titlePg")
        );
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

    const PAGE_COLOR: &str = r#"<w:background w:color="FFF2CC"/>"#;
    const PAGE_GRADIENT: &str = r##"<w:background w:color="FFF2CC"><v:background id="_x0000_s1025" o:bwmode="white" o:targetscreensize="1024,768"><v:fill color2="#9DC3E6" type="gradient"/></v:background></w:background>"##;

    /// A one-paragraph document with `prolog` between the root and `<w:body>`.
    fn background_doc(prolog: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office">{prolog}<w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#
        )
    }

    fn saved_document_xml(bytes: &[u8]) -> String {
        let pkg = load_package(bytes).expect("reload");
        String::from_utf8_lossy(pkg.part("word/document.xml").unwrap()).into_owned()
    }

    /// The text between the end of the `<w:document …>` start tag and `<w:body`.
    fn prolog_of(doc_xml: &str) -> &str {
        let root = doc_xml.find("<w:document").unwrap();
        let root_end = root + doc_xml[root..].find('>').unwrap() + 1;
        &doc_xml[root_end..doc_xml.find("<w:body").unwrap()]
    }

    #[test]
    fn save_preserves_page_background_color() {
        let pkg = load_package(&make_docx(&background_doc(PAGE_COLOR))).expect("load");
        let once = save_package(&pkg);
        assert_eq!(prolog_of(&saved_document_xml(&once)), PAGE_COLOR);

        // A second round-trip keeps exactly one copy.
        let twice = save_package(&load_package(&once).expect("reload"));
        let doc_xml = saved_document_xml(&twice);
        assert_eq!(prolog_of(&doc_xml), PAGE_COLOR);
        assert_eq!(doc_xml.matches("<w:background").count(), 1, "{doc_xml}");
    }

    #[test]
    fn save_preserves_gradient_page_background() {
        let pkg = load_package(&make_docx(&background_doc(PAGE_GRADIENT))).expect("load");
        let doc_xml = saved_document_xml(&save_package(&pkg));
        assert_eq!(prolog_of(&doc_xml), PAGE_GRADIENT);
        // The VML prefixes the fill uses are still declared on the root.
        let root = &doc_xml[doc_xml.find("<w:document").unwrap()..];
        let root = &root[..root.find('>').unwrap()];
        assert!(
            root.contains(r#"xmlns:v="urn:schemas-microsoft-com:vml""#),
            "{root}"
        );
        assert!(
            root.contains(r#"xmlns:o="urn:schemas-microsoft-com:office:office""#),
            "{root}"
        );
    }

    #[test]
    fn save_without_background_adds_no_prolog() {
        let pkg = load_package(&make_docx(&background_doc(""))).expect("load");
        assert_eq!(prolog_of(&saved_document_xml(&save_package(&pkg))), "");

        // Whitespace-only prolog is not carried over either.
        let pkg = load_package(&make_docx(&background_doc("\n  "))).expect("load");
        assert_eq!(prolog_of(&saved_document_xml(&save_package(&pkg))), "");

        let doc = crate::markdown::from_markdown("Hello");
        let bytes = save_package(&new_markdown_package(doc));
        assert_eq!(prolog_of(&saved_document_xml(&bytes)), "");
    }

    #[test]
    fn save_keeps_toc_page_numbers_visible_916() {
        // A saved TOC entry's webHidden tab and page number must not become
        // hidden text when Word reads the file back.
        let doc = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:hyperlink w:anchor="_Toc1"><w:r><w:t>Intro</w:t></w:r><w:r><w:rPr><w:webHidden/></w:rPr><w:tab/></w:r><w:r><w:rPr><w:webHidden/></w:rPr><w:t>3</w:t></w:r></w:hyperlink></w:p></w:body></w:document>"#;
        let pkg = load_package(&make_docx(doc)).expect("load");
        let saved = saved_document_xml(&save_package(&pkg));
        assert!(!saved.contains("w:vanish"), "{saved}");
        assert_eq!(saved.matches("<w:webHidden/>").count(), 2, "{saved}");
    }

    #[test]
    fn save_keeps_background_after_body_edit() {
        let mut pkg = load_package(&make_docx(&background_doc(PAGE_COLOR))).expect("load");
        if let Block::Paragraph(p) = &mut pkg.document.body[0] {
            if let Inline::Run(r) = &mut p.content[0] {
                r.text = "Goodbye".to_string();
            }
        }
        let saved = save_package(&pkg);
        assert_eq!(prolog_of(&saved_document_xml(&saved)), PAGE_COLOR);
        let reloaded = load_package(&saved).expect("reload");
        assert_eq!(reloaded.document.plain_text().trim(), "Goodbye");
    }

    /// #651: Page Color writes `w:background` first in the document and
    /// `w:displayBackgroundShape` at its CT_Settings position; No Color
    /// removes both; the result survives save and reload.
    #[test]
    fn set_page_background_writes_settings_in_order_and_round_trips() {
        use crate::page_bg::{Gradient, GradientStyle, PageBackground};
        let doc = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#;
        let settings = "<?xml version=\"1.0\"?><w:settings xmlns:w=\"x\"><w:zoom w:percent=\"100\"/><w:proofState w:spelling=\"clean\"/><w:defaultTabStop w:val=\"720\"/></w:settings>";
        let bytes = make_metadata_docx(doc, Some(settings), Some(DOC_RELS), &[]);
        let mut pkg = load_package(&bytes).expect("load");
        assert_eq!(pkg.page_background(), None);
        let bg = PageBackground {
            color: 0xFFF2CC,
            gradient: None,
        };
        assert!(pkg.set_page_background(Some(&bg)));
        let s = pkg.part_text("word/settings.xml").unwrap();
        assert!(
            s.contains("<w:zoom w:percent=\"100\"/><w:displayBackgroundShape/><w:proofState"),
            "{s}"
        );
        assert!(
            !pkg.set_page_background(Some(&bg)),
            "the same colour changes nothing"
        );
        let saved = save_package(&pkg);
        assert_eq!(prolog_of(&saved_document_xml(&saved)), PAGE_COLOR);
        let mut back = load_package(&saved).expect("reload");
        assert_eq!(back.page_background(), Some(bg));
        assert!(back.has_display_background_shape());

        // A gradient declares the VML prefixes it uses and survives save.
        let grad = PageBackground {
            color: 0x0070C0,
            gradient: Some(Gradient {
                color2: 0xFFFFFF,
                style: GradientStyle::DiagonalDown,
            }),
        };
        back.set_page_background(Some(&grad));
        let saved = save_package(&back);
        let doc_xml = saved_document_xml(&saved);
        assert!(
            doc_xml.contains("xmlns:v=\"urn:schemas-microsoft-com:vml\""),
            "{doc_xml}"
        );
        let mut back = load_package(&saved).expect("reload");
        assert_eq!(back.page_background(), Some(grad));

        // No Color removes the element and the flag.
        assert!(back.set_page_background(None));
        assert_eq!(back.page_background(), None);
        assert!(!back.has_display_background_shape());
        let s = back.part_text("word/settings.xml").unwrap();
        assert!(!s.contains("displayBackgroundShape"), "{s}");
        let saved = save_package(&back);
        assert_eq!(prolog_of(&saved_document_xml(&saved)), "");
    }

    /// A document without a settings part gets one for the flag; an
    /// explicit off is replaced rather than kept beside it.
    #[test]
    fn set_page_background_creates_settings_and_replaces_explicit_off() {
        use crate::page_bg::PageBackground;
        let bg = PageBackground {
            color: 0xFF0000,
            gradient: None,
        };
        let mut pkg = load_package(&make_docx(&background_doc(""))).expect("load");
        assert!(pkg.set_page_background(Some(&bg)));
        assert!(pkg.has_display_background_shape());
        let off = "<?xml version=\"1.0\"?><w:settings xmlns:w=\"x\"><w:displayBackgroundShape w:val=\"false\"/></w:settings>";
        let mut pkg = load_package(&make_metadata_docx(
            &background_doc(""),
            Some(off),
            Some(DOC_RELS),
            &[],
        ))
        .expect("load");
        pkg.set_page_background(Some(&bg));
        let s = pkg.part_text("word/settings.xml").unwrap();
        assert_eq!(s.matches("displayBackgroundShape").count(), 1, "{s}");
        assert!(pkg.has_display_background_shape());
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

    fn styled_table_doc(style: Option<&str>) -> Document {
        let mut t = crate::table::new_table(1, 1, 9000, crate::table::AutoFit::Default);
        if let Some(id) = style {
            crate::table::edit_table_props(&mut t, |p| {
                p.set(&format!("<w:tblStyle w:val=\"{id}\"/>"))
            });
        }
        Document {
            body: vec![Block::Table(t), Block::Paragraph(Default::default())],
        }
    }

    fn utf16le(text: &str) -> Vec<u8> {
        let mut out = vec![0xff, 0xfe];
        for u in text.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    #[test]
    fn table_styles_are_added_to_a_utf16_styles_part_in_its_encoding() {
        let mut pkg = new_package(styled_table_doc(Some("GridTable4-Accent1")));
        let styles = String::from_utf8(pkg.part("word/styles.xml").unwrap().to_vec()).unwrap();
        let styles = styles.replace("encoding=\"UTF-8\"", "encoding=\"UTF-16\"");
        assert!(pkg.set_part("word/styles.xml", utf16le(&styles)));
        let saved = load_package(&save_package(&pkg)).unwrap();
        let bytes = saved.part("word/styles.xml").unwrap();
        assert_eq!(&bytes[..2], &[0xff, 0xfe], "still UTF-16LE with its BOM");
        let xml = decode_xml_part(bytes).unwrap();
        assert!(xml.contains("w:styleId=\"GridTable4-Accent1\""));
        assert!(xml.trim_end().ends_with("</w:styles>"));
        // Already defined: the part is left byte for byte.
        let again = load_package(&save_package(&saved)).unwrap();
        assert_eq!(again.part("word/styles.xml").unwrap(), bytes);
    }

    #[test]
    fn a_styles_part_without_a_closing_tag_is_left_alone_and_a_self_closing_one_opens() {
        let mut pkg = new_package(styled_table_doc(Some("PlainTable1")));
        pkg.set_part("word/styles.xml", b"<w:styles xmlns:w=\"x\"".to_vec());
        let saved = load_package(&save_package(&pkg)).unwrap();
        assert_eq!(
            saved.part("word/styles.xml").unwrap(),
            b"<w:styles xmlns:w=\"x\""
        );
        pkg.set_part(
            "word/styles.xml",
            b"<?xml version=\"1.0\"?><w:styles xmlns:w=\"x\" />".to_vec(),
        );
        let saved = load_package(&save_package(&pkg)).unwrap();
        let xml = String::from_utf8(saved.part("word/styles.xml").unwrap().to_vec()).unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\"?><w:styles xmlns:w=\"x\"><w:style"));
        assert!(xml.ends_with("</w:style></w:styles>"));
        assert!(xml.contains("w:styleId=\"PlainTable1\""));
    }

    #[test]
    fn a_package_without_a_styles_part_gets_one_wired_up() {
        let mut pkg = new_package(styled_table_doc(Some("TableGrid")));
        pkg.parts.retain(|(n, _)| n != "word/styles.xml");
        for (name, strip) in [
            (
                "[Content_Types].xml",
                "<Override PartName=\"/word/styles.xml\"",
            ),
            ("word/_rels/document.xml.rels", "<Relationship"),
        ] {
            let xml = String::from_utf8(pkg.part(name).unwrap().to_vec()).unwrap();
            // Drop the styles entry (the element holding `strip` that names styles).
            let start = xml
                .match_indices(strip)
                .map(|(i, _)| i)
                .find(|&i| xml[i..].split("/>").next().unwrap().contains("styles"))
                .unwrap();
            let end = start + xml[start..].find("/>").unwrap() + 2;
            let xml = format!("{}{}", &xml[..start], &xml[end..]);
            assert!(!xml.contains("styles.xml"), "{xml}");
            pkg.set_part(name, xml.into_bytes());
        }
        let saved = load_package(&save_package(&pkg)).unwrap();
        let styles = String::from_utf8(saved.part("word/styles.xml").unwrap().to_vec()).unwrap();
        assert!(styles.contains("w:styleId=\"TableGrid\""));
        assert!(styles.contains("w:styleId=\"TableNormal\""));
        let ct = String::from_utf8(saved.part("[Content_Types].xml").unwrap().to_vec()).unwrap();
        assert!(ct.contains("PartName=\"/word/styles.xml\""));
        let rels = String::from_utf8(saved.part("word/_rels/document.xml.rels").unwrap().to_vec())
            .unwrap();
        assert!(rels.contains("Target=\"styles.xml\""));
    }

    #[test]
    fn a_table_style_used_only_in_a_header_is_defined_too() {
        let mut pkg = new_package(styled_table_doc(None));
        let header = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:hdr xmlns:w=\"x\">                      <w:tbl><w:tblPr><w:tblStyle w:val=\"ListTable3-Accent1\"/></w:tblPr></w:tbl></w:hdr>";
        pkg.parts
            .push(("word/header1.xml".into(), header.as_bytes().to_vec()));
        let saved = load_package(&save_package(&pkg)).unwrap();
        let xml = String::from_utf8(saved.part("word/styles.xml").unwrap().to_vec()).unwrap();
        assert!(xml.contains("w:styleId=\"ListTable3-Accent1\""));
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
    fn stored_document_follows_the_part_not_the_live_document_1107() {
        let source = new_package(crate::markdown::from_markdown("Hello"));
        let mut pkg = load_package(&save_package(&source)).unwrap();
        // What a fresh load shows: the body with its final section.
        let mut loaded = pkg.document.clone();
        if loaded.trailing_section_properties().is_none() {
            loaded.set_trailing_section_properties(pkg.final_section());
        }
        assert_eq!(pkg.stored_document().as_ref(), Some(&loaded));
        // A section edit changes the live document, not the stored part.
        pkg.set_sect_pr("<w:sectPr><w:cols w:num=\"2\"/></w:sectPr>".into());
        assert_ne!(pkg.document, loaded);
        assert_eq!(pkg.stored_document().as_ref(), Some(&loaded));
        // Saving rewrites the part, and a fresh load of it agrees.
        let saved = load_package(&save_package(&pkg)).unwrap();
        let stored = saved.stored_document().unwrap();
        assert!(stored.plain_text().starts_with("Hello"));
        assert!(
            stored
                .trailing_section_properties()
                .is_some_and(|s| s.raw.contains("w:num=\"2\""))
        );
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

    #[test]
    fn section_header_parts_inherit_headers_and_footers_per_variant() {
        let rels = parse_rels_xml(
            r#"<Relationships>
                <Relationship Id="rH1" Target="header1.xml"/>
                <Relationship Id="rH2" Target="header2.xml"/>
                <Relationship Id="rF1" Target="footer1.xml"/>
                <Relationship Id="rFirst" Target="header3.xml"/>
            </Relationships>"#,
        );
        let s1 = r#"<w:sectPr><w:headerReference w:type="default" r:id="rH1"/><w:footerReference w:type="default" r:id="rF1"/><w:headerReference w:type="first" r:id="rFirst"/></w:sectPr>"#;
        let s2 = r#"<w:sectPr><w:headerReference w:type="default" r:id="rH2"/></w:sectPr>"#;
        let s3 = r#"<w:sectPr><w:footerReference w:type="default" r:id="rMissing"/></w:sectPr>"#;
        let parts = section_header_parts(&[s1, s2, s3], &rels);
        assert_eq!(parts.len(), 3);
        let name = |p: &Option<AppliedPart>| p.as_ref().map(|p| (p.part_name.clone(), p.inherited));
        assert_eq!(
            name(&parts[0].headers[0]),
            Some(("word/header1.xml".into(), false))
        );
        assert_eq!(
            name(&parts[0].headers[1]),
            Some(("word/header3.xml".into(), false))
        );
        assert_eq!(
            name(&parts[0].footers[0]),
            Some(("word/footer1.xml".into(), false))
        );
        assert_eq!(name(&parts[0].headers[2]), None);
        // Section 2 replaces the default header and links the rest to section 1.
        assert_eq!(
            name(&parts[1].headers[0]),
            Some(("word/header2.xml".into(), false))
        );
        assert_eq!(
            name(&parts[1].headers[1]),
            Some(("word/header3.xml".into(), true))
        );
        assert_eq!(
            name(&parts[1].footers[0]),
            Some(("word/footer1.xml".into(), true))
        );
        // An unresolvable reference clears the variant instead of inheriting.
        assert_eq!(name(&parts[2].footers[0]), None);
        assert_eq!(
            name(&parts[2].headers[0]),
            Some(("word/header2.xml".into(), true))
        );
        // `from_section` names the section whose reference applies.
        let from = |p: &Option<AppliedPart>| p.as_ref().map(|p| p.from_section);
        assert_eq!(from(&parts[0].headers[0]), Some(0));
        assert_eq!(from(&parts[1].headers[0]), Some(1));
        assert_eq!(from(&parts[2].headers[0]), Some(1));
        assert_eq!(from(&parts[1].footers[0]), Some(0));
    }

    /// A fresh package with a document rels part (`new_package` has one).
    fn hf_pkg() -> Package {
        new_package(Document {
            body: vec![Block::Paragraph(crate::model::Paragraph::default())],
        })
    }

    /// A two-paragraph document whose first paragraph ends a section with a
    /// distinct first page (`w:titlePg`); the trailing section has none.
    fn two_section_pkg() -> Package {
        let mut first = crate::model::Paragraph::default();
        first.props.section_break =
            Some("<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:titlePg/></w:sectPr>".into());
        let mut pkg = new_package(Document {
            body: vec![
                Block::Paragraph(first),
                Block::Paragraph(crate::model::Paragraph::default()),
            ],
        });
        pkg.set_sect_pr("<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr>".into());
        pkg
    }

    fn watermark_slots(pkg: &Package) -> Vec<(usize, HeaderVariant, String)> {
        pkg.watermarks()
            .into_iter()
            .map(|w| {
                let WatermarkKind::Text(t) = w.kind else {
                    panic!("text watermark expected");
                };
                (w.header.section_index, w.header.variant, t)
            })
            .collect()
    }

    /// #651: a watermark goes into every header any section shows, creating
    /// headers where a section has none, and survives save and reload.
    #[test]
    fn text_watermark_reaches_every_shown_header_and_round_trips() {
        use crate::watermark::TextWatermarkSpec;
        let mut pkg = two_section_pkg();
        assert!(pkg.watermarks().is_empty());
        assert!(pkg.apply_text_watermark(Some(&TextWatermarkSpec::preset("DRAFT", true))));
        // Section 1 has its own default and first-page headers now; section
        // 2 inherits the default one (no title page there).
        let expect = vec![
            (0, HeaderVariant::Default, "DRAFT".to_string()),
            (0, HeaderVariant::First, "DRAFT".to_string()),
            (1, HeaderVariant::Default, "DRAFT".to_string()),
        ];
        assert_eq!(watermark_slots(&pkg), expect);
        let headers = pkg
            .part_names()
            .iter()
            .filter(|n| n.starts_with("word/header"))
            .count();
        assert_eq!(headers, 2, "one default and one first-page header");
        let sects = pkg.section_sect_prs();
        assert!(sects[0].contains("w:type=\"first\""), "{}", sects[0]);
        assert!(
            !sects[1].contains("headerReference"),
            "inherits: {}",
            sects[1]
        );
        let reloaded = load_package(&save_package(&pkg)).expect("reload");
        assert_eq!(watermark_slots(&reloaded), expect);
        let texts: Vec<String> = reloaded
            .shown_text_watermarks(&reloaded.section_sect_prs())
            .into_iter()
            .map(|w| w.text)
            .collect();
        assert_eq!(texts, ["DRAFT", "DRAFT"], "each shown part once");
    }

    /// A second watermark replaces the first in every header (never two),
    /// Remove takes them all out, and other header content stays.
    #[test]
    fn text_watermark_replaces_and_removes_keeping_header_text() {
        use crate::watermark::TextWatermarkSpec;
        let mut pkg = two_section_pkg();
        // Each section has its own default header with text.
        let mut sects: Vec<String> = pkg
            .section_sect_prs()
            .into_iter()
            .map(str::to_string)
            .collect();
        for (k, text) in ["Left", "Right"].iter().enumerate() {
            let (rid, _) = pkg
                .create_hf_part(true, &format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"))
                .unwrap();
            sects[k] = crate::sect::set_hf_reference(&sects[k], true, "default", Some(&rid));
        }
        let mut changed = sects.clone();
        pkg.set_text_watermark(
            Some(&TextWatermarkSpec::preset("CONFIDENTIAL", true)),
            &mut changed,
        );
        // Only the first-page slot needed a new header.
        assert_eq!(changed[1], sects[1]);
        assert!(changed[0].contains("w:type=\"first\""));
        pkg.set_text_watermark(
            Some(&TextWatermarkSpec::preset("SAMPLE", false)),
            &mut changed,
        );
        let marks = pkg.shown_text_watermarks(&changed);
        assert_eq!(marks.len(), 3, "{marks:?}");
        assert!(
            marks
                .iter()
                .all(|m| m.text == "SAMPLE" && m.rotation == 0.0)
        );
        let all: String = pkg
            .part_names()
            .iter()
            .filter(|n| n.starts_with("word/header"))
            .map(|n| pkg.part_text(n).unwrap())
            .collect();
        assert_eq!(
            all.matches(crate::watermark::SHAPE_ID).count(),
            3,
            "never two in a part"
        );
        // Shape ids are unique across the parts.
        let mut ids: Vec<&str> = all
            .match_indices("id=\"PowerPlusWaterMarkObject")
            .map(|(i, _)| &all[i..i + 32])
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 3, "{ids:?}");

        let before = changed.clone();
        assert!(pkg.set_text_watermark(None, &mut changed));
        assert_eq!(changed, before, "removing creates no header");
        assert!(pkg.shown_text_watermarks(&changed).is_empty());
        let all: String = pkg
            .part_names()
            .iter()
            .filter(|n| n.starts_with("word/header"))
            .map(|n| pkg.part_text(n).unwrap())
            .collect();
        assert!(all.contains("<w:t>Left</w:t>") && all.contains("<w:t>Right</w:t>"));
        assert!(
            !pkg.set_text_watermark(None, &mut changed),
            "nothing left to remove"
        );
    }

    /// A first-page header that is not shown (no `w:titlePg`) still loses an
    /// old watermark on replace and on remove, so turning the distinct first
    /// page on later shows no stale one; nothing new goes into it.
    #[test]
    fn text_watermark_strips_headers_that_are_referenced_but_not_shown() {
        use crate::watermark::{TextWatermarkSpec, insert_watermark};
        let mut pkg = hf_pkg();
        let (rid, first) = pkg.create_hf_part(true, "<w:p/>").unwrap();
        let old = insert_watermark(
            &pkg.part_text(&first).unwrap(),
            &TextWatermarkSpec::preset("DRAFT", true),
            1,
        );
        pkg.set_part_text(&first, &old);
        let mut sects = vec![crate::sect::set_hf_reference(
            pkg.sect_pr(),
            true,
            "first",
            Some(&rid),
        )];
        pkg.set_text_watermark(Some(&TextWatermarkSpec::preset("SAMPLE", true)), &mut sects);
        assert!(text_watermarks(&pkg.part_text(&first).unwrap()).is_empty());
        assert_eq!(
            pkg.shown_text_watermarks(&sects)
                .into_iter()
                .map(|w| w.text)
                .collect::<Vec<_>>(),
            ["SAMPLE"],
            "the default header got it"
        );
        // Remove takes the hidden one's out too.
        pkg.set_part_text(&first, &old);
        assert!(pkg.set_text_watermark(None, &mut sects));
        assert!(text_watermarks(&pkg.part_text(&first).unwrap()).is_empty());
    }

    /// Word's picture watermark (VML or DrawingML, in its gallery control)
    /// is replaced by a text one, never kept beside it, and Remove takes it.
    #[test]
    fn text_watermark_replaces_and_removes_picture_watermarks() {
        use crate::watermark::TextWatermarkSpec;
        for run in [
            "<w:pict><v:shape id=\"WordPictureWatermark1\"><v:imagedata r:id=\"rImg\"/></v:shape></w:pict>",
            "<w:drawing><wp:anchor><wp:docPr id=\"1\" name=\"Watermark\"/></wp:anchor></w:drawing>",
        ] {
            let mut pkg = hf_pkg();
            let gallery = format!(
                "<w:sdt><w:sdtPr><w:docPartObj><w:docPartGallery w:val=\"Watermarks\"/></w:docPartObj>\
                 </w:sdtPr><w:sdtContent><w:p><w:r>{run}</w:r></w:p></w:sdtContent></w:sdt><w:p/>"
            );
            let (rid, _) = pkg.create_hf_part(true, &gallery).unwrap();
            let sect = crate::sect::set_hf_reference(pkg.sect_pr(), true, "default", Some(&rid));
            pkg.set_sect_pr(sect);
            assert_eq!(pkg.watermarks().len(), 1);
            assert!(pkg.apply_text_watermark(Some(&TextWatermarkSpec::preset("DRAFT", true))));
            let kinds: Vec<WatermarkKind> = pkg.watermarks().into_iter().map(|w| w.kind).collect();
            assert_eq!(kinds, [WatermarkKind::Text("DRAFT".into())], "{run}");
            // Back to the picture, then Remove.
            pkg.set_part_text(
                &pkg.watermarks()[0].header.part_name.clone(),
                &format!("<w:hdr xmlns:w=\"W\">{gallery}</w:hdr>"),
            );
            assert!(pkg.apply_text_watermark(None));
            assert!(pkg.watermarks().is_empty(), "{run}");
        }
    }

    /// A self-closing document relationships part takes the new header's
    /// relationship (the loop that adds headers ends, with one part); a
    /// relationships part with no Relationships root takes none, and no
    /// part is added.
    #[test]
    fn text_watermark_on_odd_relationship_parts_terminates() {
        use crate::watermark::TextWatermarkSpec;
        const RELS: &str = "word/_rels/document.xml.rels";
        let headers = |pkg: &Package| {
            pkg.part_names()
                .iter()
                .filter(|n| n.starts_with("word/header"))
                .count()
        };
        let mut pkg = hf_pkg();
        pkg.set_part_text(
            RELS,
            "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>",
        );
        assert!(pkg.apply_text_watermark(Some(&TextWatermarkSpec::preset("DRAFT", true))));
        assert_eq!(headers(&pkg), 1);
        assert_eq!(pkg.watermarks().len(), 1, "the reference resolves");

        let mut pkg = hf_pkg();
        pkg.set_part_text(RELS, "<?xml version=\"1.0\"?><Other/>");
        let mut sects = vec![pkg.sect_pr().to_string()];
        assert!(
            !pkg.set_text_watermark(Some(&TextWatermarkSpec::preset("DRAFT", true)), &mut sects)
        );
        assert_eq!(headers(&pkg), 0);
        assert!(!sects[0].contains("headerReference"));
        assert!(pkg.create_hf_part(true, "<w:p/>").is_none());
    }

    /// A document with no `document.xml.rels` gets one with the header's
    /// relationship, so its watermark resolves.
    #[test]
    fn text_watermark_creates_missing_document_relationships() {
        use crate::watermark::TextWatermarkSpec;
        let mut pkg = hf_pkg();
        pkg.parts
            .retain(|(n, _)| n != "word/_rels/document.xml.rels");
        assert!(pkg.apply_text_watermark(Some(&TextWatermarkSpec::preset("DRAFT", true))));
        assert_eq!(pkg.watermarks().len(), 1);
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Target=\"header1.xml\""), "{rels}");
    }

    /// A self-closing `[Content_Types].xml` root takes the override.
    #[test]
    fn create_hf_part_expands_a_self_closing_types_root() {
        let mut pkg = hf_pkg();
        pkg.set_part_text(
            "[Content_Types].xml",
            "<?xml version=\"1.0\"?><Types xmlns=\"T\"/>",
        );
        let (_, name) = pkg.create_hf_part(true, "<w:p/>").unwrap();
        let ct = pkg.part_text("[Content_Types].xml").unwrap();
        assert!(
            ct.contains(&format!("<Override PartName=\"/{name}\"")),
            "{ct}"
        );
        assert!(ct.ends_with("</Types>"), "{ct}");
    }

    /// With different odd and even pages, the even-page header gets one too.
    #[test]
    fn text_watermark_covers_even_page_headers() {
        use crate::watermark::TextWatermarkSpec;
        let mut pkg = hf_pkg();
        pkg.set_even_odd(true);
        pkg.apply_text_watermark(Some(&TextWatermarkSpec::preset("URGENT", true)));
        let variants: Vec<HeaderVariant> = pkg
            .watermarks()
            .into_iter()
            .map(|w| w.header.variant)
            .collect();
        assert_eq!(variants, [HeaderVariant::Default, HeaderVariant::Even]);
    }

    #[test]
    fn create_hf_part_adds_part_rel_and_content_type_but_no_reference() {
        let mut pkg = hf_pkg();
        let sect = pkg.sect_pr().to_string();
        let (rid, name) = pkg
            .create_hf_part(true, "<w:p><w:r><w:t>H</w:t></w:r></w:p>")
            .unwrap();
        assert_eq!(pkg.sect_pr(), sect, "no section reference");
        let xml = String::from_utf8_lossy(pkg.part(&name).unwrap()).into_owned();
        assert!(
            xml.contains("<w:hdr") && xml.contains("<w:t>H</w:t>"),
            "{xml}"
        );
        let rels = pkg.document_rels();
        let target = rels.target(&rid).unwrap();
        assert_eq!(
            resolve_document_relationship_target(target).as_deref(),
            Some(name.as_str())
        );
        let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned();
        assert!(ct.contains(&format!("PartName=\"/{name}\"")), "{ct}");
        // A second one gets a fresh name and id.
        let (rid2, name2) = pkg.create_hf_part(false, "<w:p/>").unwrap();
        assert_ne!(rid, rid2);
        assert!(name2.starts_with("word/footer"));
    }

    #[test]
    fn copy_hf_part_copies_bytes_and_own_rels_so_picture_ids_resolve() {
        let mut pkg = hf_pkg();
        let (_, src) = pkg
            .create_hf_part(
                true,
                "<w:p><w:r><w:drawing><a:blip r:embed=\"rId9\"/></w:drawing></w:r></w:p>",
            )
            .unwrap();
        let src_rels = part_rels_name(&src).unwrap();
        let rels_xml = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/></Relationships>";
        pkg.parts.push((src_rels, rels_xml.as_bytes().to_vec()));
        let (rid, copy) = pkg.copy_hf_part(&src).unwrap();
        assert_ne!(copy, src);
        assert!(copy.starts_with("word/header"));
        assert_eq!(pkg.part(&copy), pkg.part(&src));
        let copy_rels = pkg.part(&part_rels_name(&copy).unwrap()).unwrap();
        let rels = parse_rels_xml(&String::from_utf8_lossy(copy_rels));
        assert_eq!(rels.target("rId9"), Some("media/image1.png"));
        assert!(pkg.document_rels().target(&rid).is_some());
        let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned();
        assert!(ct.contains(&format!("PartName=\"/{copy}\"")));
        // A footer copies as a footer.
        let (_, f) = pkg.create_hf_part(false, "<w:p/>").unwrap();
        assert!(pkg.copy_hf_part(&f).unwrap().1.starts_with("word/footer"));
        assert_eq!(pkg.copy_hf_part("word/missing.xml"), None);
    }

    #[test]
    fn create_hf_still_references_the_trailing_section() {
        let mut pkg = hf_pkg();
        let name = pkg.create_hf(false, "first").unwrap();
        let rid = crate::sect::hf_reference(pkg.sect_pr(), false, "first").unwrap();
        let target = pkg.document_rels().target(&rid).map(str::to_owned).unwrap();
        assert_eq!(resolve_document_relationship_target(&target), Some(name));
    }

    #[test]
    fn ensure_styles_adds_word_header_and_footer_styles_with_their_tabs() {
        use crate::model::TabAlign;
        let mut pkg = hf_pkg();
        pkg.ensure_styles(&["Header", "Footer"]);
        let xml = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
        let sheet = crate::styles::parse_styles_xml(&xml);
        for id in ["Header", "Footer"] {
            let tabs = sheet.effective_tabs(Some(id));
            let got: Vec<(i32, TabAlign)> = tabs.iter().map(|t| (t.pos, t.align)).collect();
            assert_eq!(
                got,
                vec![(4680, TabAlign::Center), (9360, TabAlign::Right)],
                "{id}"
            );
        }
        // An existing definition is left alone.
        let before = pkg.part("word/styles.xml").unwrap().to_vec();
        pkg.ensure_styles(&["Header"]);
        assert_eq!(pkg.part("word/styles.xml").unwrap(), before.as_slice());
    }

    #[test]
    fn text_watermarks_read_rotation_fill_and_size() {
        let header = r##"<w:hdr xmlns:w="w" xmlns:v="v"><w:p><w:r><w:pict>
            <v:shape id="PowerPlusWaterMarkObject357" style="position:absolute;margin-left:0;margin-top:0;width:468pt;height:117pt;rotation:315;z-index:-251655168;mso-position-horizontal:center" fillcolor="silver" stroked="f"><v:textpath style="font-family:&quot;Calibri&quot;;font-size:1pt" string="DRAFT"/></v:shape>
            <v:shape id="PowerPlusWaterMarkObject358" style="width:6.5in;rotation:-30.5" fillcolor="#FF0000 [3204]"><v:textpath style="font-size:36pt" string="SECRET"/></v:shape>
            <v:shape id="PowerPlusWaterMarkObject359" style="width:wide" fillcolor="#abc"><v:textpath string="ODD"/></v:shape>
            <v:shape id="PowerPlusWaterMarkObject360"><v:imagedata r:id="rImg"/></v:shape>
            <v:shape id="ordinary" style="rotation:90"><v:textpath string="NOT A WATERMARK"/></v:shape>
            </w:pict></w:r></w:p></w:hdr>"##;
        let marks = text_watermarks(header);
        assert_eq!(
            marks,
            vec![
                TextWatermark {
                    text: "DRAFT".to_string(),
                    rotation: 315.0,
                    fill: Some((0xc0, 0xc0, 0xc0)),
                    width_pt: Some(468.0),
                    font_size_pt: None,
                    font: Some("Calibri".to_string()),
                    opacity: None,
                },
                TextWatermark {
                    text: "SECRET".to_string(),
                    rotation: -30.5,
                    fill: Some((0xff, 0, 0)),
                    width_pt: Some(468.0),
                    font_size_pt: Some(36.0),
                    font: None,
                    opacity: None,
                },
                TextWatermark {
                    text: "ODD".to_string(),
                    rotation: 0.0,
                    fill: Some((0xaa, 0xbb, 0xcc)),
                    width_pt: None,
                    font_size_pt: None,
                    font: None,
                    opacity: None,
                },
            ]
        );
        // The kinds come from the same walk and are unchanged.
        assert_eq!(
            watermark_kinds(header),
            vec![
                WatermarkKind::Text("DRAFT".to_string()),
                WatermarkKind::Text("SECRET".to_string()),
                WatermarkKind::Text("ODD".to_string()),
                WatermarkKind::Picture,
            ]
        );
    }

    #[test]
    fn set_part_text_keeps_utf16() {
        let mut pkg = new_package(Document::default());
        let text = "<?xml version=\"1.0\" encoding=\"UTF-16\"?><w:hdr>é</w:hdr>";
        let mut be = vec![0xfe, 0xff];
        for unit in text.encode_utf16() {
            be.extend_from_slice(&unit.to_be_bytes());
        }
        pkg.parts.push(("word/header1.xml".to_string(), be));
        assert_eq!(pkg.part_text("word/header1.xml").as_deref(), Some(text));
        let edited = text.replace('é', "ü");
        assert!(pkg.set_part_text("word/header1.xml", &edited));
        let bytes = pkg.part("word/header1.xml").unwrap();
        assert!(
            bytes.starts_with(&[0xfe, 0xff, 0, b'<']),
            "UTF-16BE with its BOM"
        );
        assert_eq!(
            pkg.part_text("word/header1.xml").as_deref(),
            Some(edited.as_str())
        );
        assert!(!pkg.set_part_text("word/missing.xml", "x"));
        assert_eq!(pkg.part_text("word/missing.xml"), None);
    }

    fn link(target: Option<&str>, rel_id: Option<&str>, raw: Option<&str>) -> crate::model::Inline {
        crate::model::Inline::Hyperlink(crate::model::Hyperlink {
            target: target.map(str::to_string),
            anchor: target.is_none().then(|| "bm".to_string()),
            rel_id: rel_id.map(str::to_string),
            runs: vec![crate::model::Run {
                text: "link".to_string(),
                props: Default::default(),
            }],
            raw: raw.map(str::to_string),
            ..Default::default()
        })
    }

    fn link_blocks(links: Vec<crate::model::Inline>) -> Vec<Block> {
        vec![Block::Paragraph(crate::model::Paragraph {
            props: Default::default(),
            content: links,
        })]
    }

    fn rel_ids(blocks: &[Block]) -> Vec<Option<String>> {
        let Block::Paragraph(p) = &blocks[0] else {
            panic!()
        };
        p.content
            .iter()
            .map(|inl| match inl {
                crate::model::Inline::Hyperlink(h) => h.rel_id.clone(),
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn link_part_hyperlinks_mints_rels_and_creates_part() {
        let mut pkg = new_package(Document::default());
        pkg.parts
            .push(("word/header1.xml".to_string(), b"<w:hdr/>".to_vec()));
        let mut blocks = link_blocks(vec![
            link(Some("https://a.example/?x=1&y=2"), None, None),
            link(None, None, None),
        ]);
        let before = pkg.clone();
        let update = pkg
            .link_part_hyperlinks("word/header1.xml", &mut blocks)
            .unwrap()
            .expect("a link needs a relationship");
        assert_eq!(pkg.parts, before.parts, "computing changes nothing");
        assert_eq!(rel_ids(&blocks), [Some("rId1".to_string()), None]);
        assert_eq!(
            update.content_types, None,
            "new packages have the rels Default"
        );
        pkg.apply_part_rels(update);
        let rels = pkg
            .part_text("word/_rels/header1.xml.rels")
            .expect("rels created");
        assert!(
            rels.starts_with("<?xml") && rels.contains(PACKAGE_RELATIONSHIPS_NS),
            "{rels}"
        );
        assert!(
            rels.contains(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://a.example/?x=1&amp;y=2" TargetMode="External"/>"#),
            "{rels}"
        );
        assert_eq!(
            parse_rels_xml(&rels).target("rId1"),
            Some("https://a.example/?x=1&y=2")
        );
        // Nothing left to link: no update.
        assert_eq!(
            pkg.link_part_hyperlinks("word/header1.xml", &mut blocks),
            Ok(None)
        );
    }

    #[test]
    fn link_part_hyperlinks_remints_foreign_rel_id() {
        let mut pkg = new_package(Document::default());
        let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId3" Type="h" Target="https://b.example/" TargetMode="External"/><Relationship Id="rId7" Type="i" Target="media/image1.png"/></Relationships>"#;
        let mut utf16 = vec![0xff, 0xfe];
        for unit in rels.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        pkg.parts
            .push(("word/_rels/header1.xml.rels".to_string(), utf16));
        let pasted = r#"<w:hyperlink r:id="rId5" w:tooltip="t" w:history="1"><w:r><w:t>link</w:t></w:r></w:hyperlink>"#;
        let mut blocks = link_blocks(vec![
            // Already linked to its target in this part: kept.
            link(Some("https://b.example/"), Some("rId3"), None),
            // A body id that means nothing here, with preserved markup.
            link(Some("https://c.example/"), Some("rId5"), Some(pasted)),
            // An id naming something else here.
            link(Some("https://d.example/"), Some("rId7"), None),
        ]);
        let update = pkg
            .link_part_hyperlinks("word/header1.xml", &mut blocks)
            .unwrap()
            .unwrap();
        pkg.apply_part_rels(update);
        assert_eq!(
            rel_ids(&blocks),
            [
                Some("rId3".to_string()),
                Some("rId8".to_string()),
                Some("rId9".to_string())
            ]
        );
        let bytes = pkg.part("word/_rels/header1.xml.rels").unwrap();
        assert!(
            bytes.starts_with(&[0xff, 0xfe]),
            "the rels part keeps its encoding"
        );
        let parsed = parse_rels_xml(&pkg.part_text("word/_rels/header1.xml.rels").unwrap());
        assert_eq!(parsed.target("rId8"), Some("https://c.example/"));
        assert_eq!(parsed.target("rId9"), Some("https://d.example/"));
        assert_eq!(parsed.target("rId7"), Some("media/image1.png"));
        let xml = crate::serialize::blocks_to_xml(&blocks);
        assert!(
            xml.contains(r#"<w:hyperlink r:id="rId8" w:tooltip="t" w:history="1">"#),
            "{xml}"
        );
        assert!(
            !xml.contains("rId5") && xml.contains(r#"r:id="rId9""#),
            "{xml}"
        );
    }

    #[test]
    fn with_opener_rel_id_sets_or_adds_the_id() {
        assert_eq!(
            with_opener_rel_id(
                r#"<w:hyperlink w:anchor="a" r:id='x'><w:r/></w:hyperlink>"#,
                "rId2"
            ),
            r#"<w:hyperlink w:anchor="a" r:id='rId2'><w:r/></w:hyperlink>"#
        );
        assert_eq!(
            with_opener_rel_id(
                r#"<w:hyperlink w:tooltip="t"><w:r r:id="keep"/></w:hyperlink>"#,
                "rId2"
            ),
            r#"<w:hyperlink r:id="rId2" w:tooltip="t"><w:r r:id="keep"/></w:hyperlink>"#
        );
    }

    #[test]
    fn a_header_rels_part_adds_the_rels_default_when_missing() {
        let bytes = make_metadata_docx(BODY, None, None, &[("header1.xml", "<w:hdr/>")]);
        let mut pkg = load_package(&bytes).unwrap();
        let mut blocks = link_blocks(vec![link(Some("https://a.example/"), None, None)]);
        let update = pkg
            .link_part_hyperlinks("word/header1.xml", &mut blocks)
            .unwrap()
            .unwrap();
        assert!(update.content_types.is_some());
        pkg.apply_part_rels(update);
        let ct = pkg.part_text("[Content_Types].xml").unwrap();
        assert!(
            ct.contains(r#"<Types><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/></Types>"#),
            "{ct}"
        );
        // Once there, it isn't added again.
        pkg.parts
            .retain(|(n, _)| n != "word/_rels/header1.xml.rels");
        let mut blocks = link_blocks(vec![link(Some("https://a.example/"), None, None)]);
        let update = pkg
            .link_part_hyperlinks("word/header1.xml", &mut blocks)
            .unwrap()
            .unwrap();
        assert_eq!(update.content_types, None);
    }

    #[test]
    fn link_part_hyperlinks_skips_ids_however_they_are_quoted() {
        let mut pkg = new_package(Document::default());
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id='rId1' Type="i" Target="media/image1.png"/><Relationship Id = "rId2" Type="i" Target="media/image2.png"/></Relationships>"#;
        pkg.parts.push((
            "word/_rels/header1.xml.rels".to_string(),
            rels.as_bytes().to_vec(),
        ));
        let mut blocks = link_blocks(vec![link(Some("https://a.example/"), None, None)]);
        let update = pkg
            .link_part_hyperlinks("word/header1.xml", &mut blocks)
            .unwrap()
            .unwrap();
        pkg.apply_part_rels(update);
        assert_eq!(rel_ids(&blocks), [Some("rId3".to_string())]);
        let text = pkg.part_text("word/_rels/header1.xml.rels").unwrap();
        let mut ids = Vec::new();
        let mut parser = XmlParser::new(&text);
        loop {
            match parser.next() {
                Event::Start if parser.name() == "Relationship" => {
                    ids.push(parser.attr("Id").to_string());
                }
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(
            ids,
            ["rId1", "rId2", "rId3"],
            "one relationship per id: {text}"
        );
        let parsed = parse_rels_xml(&text);
        assert_eq!(parsed.target("rId1"), Some("media/image1.png"));
        assert_eq!(parsed.target("rId3"), Some("https://a.example/"));
    }

    #[test]
    fn with_opener_rel_id_handles_any_attribute_spacing() {
        let body = "<w:r><w:t>x</w:t></w:r></w:hyperlink>";
        for opener in [
            "<w:hyperlink w:history=\"1\"\tr:id=\"rId5\">",
            "<w:hyperlink w:history=\"1\"\n  r:id=\"rId5\" w:tooltip=\"t\">",
            "<w:hyperlink r:id = \"rId5\">",
            "<w:hyperlink r:id\n=\n'rId5'>",
            "<w:hyperlink\r\nr:id=\"rId5\"/>",
        ] {
            let raw = format!("{opener}{body}");
            let out = with_opener_rel_id(&raw, "rId9");
            let parsed_opener = {
                let mut parser = XmlParser::new(&out);
                assert_eq!(parser.next(), Event::Start, "{out}");
                let ids: Vec<&str> = parser
                    .attrs()
                    .iter()
                    .filter(|a| a.name == "r:id")
                    .map(|a| a.value)
                    .collect();
                assert_eq!(ids, ["rId9"], "{out}");
                out[..parser.pos()].to_string()
            };
            assert_eq!(parsed_opener.matches("r:id").count(), 1, "{out}");
            assert!(!out.contains("rId5"), "{out}");
            assert_eq!(out.len(), raw.len(), "only the value changed: {out}");
        }
    }

    #[test]
    fn link_part_hyperlinks_on_unreadable_rels_changes_no_link() {
        for rels in [b"<Other/>".to_vec(), vec![0xff, 0xfe, b'<']] {
            let mut pkg = new_package(Document::default());
            pkg.parts
                .push(("word/_rels/header1.xml.rels".to_string(), rels));
            let pasted = r#"<w:hyperlink r:id="rId5"><w:r><w:t>link</w:t></w:r></w:hyperlink>"#;
            let mut blocks = link_blocks(vec![
                link(Some("https://a.example/"), None, None),
                link(Some("https://b.example/"), Some("rId5"), Some(pasted)),
            ]);
            let before = blocks.clone();
            assert!(
                pkg.link_part_hyperlinks("word/header1.xml", &mut blocks)
                    .is_err()
            );
            assert_eq!(blocks, before, "no link was changed");
        }
        // Without external links there's nothing to write, readable or not.
        let mut pkg = new_package(Document::default());
        pkg.parts.push((
            "word/_rels/header1.xml.rels".to_string(),
            b"<Other/>".to_vec(),
        ));
        let mut blocks = link_blocks(vec![link(None, None, None)]);
        assert_eq!(
            pkg.link_part_hyperlinks("word/header1.xml", &mut blocks),
            Ok(None)
        );
    }

    /// A package whose `comments.xml` is `comments` (#971).
    fn with_comments(comments: &str) -> Package {
        let mut p = load_package(&make_docx("<w:document/>")).unwrap();
        p.parts.push((
            "word/comments.xml".to_string(),
            comments.as_bytes().to_vec(),
        ));
        p
    }

    const COMMENTS_OPEN: &str = "<?xml version=\"1.0\"?><w:comments xmlns:w=\"w\">";

    /// #971: a producer that writes `w:id` after other attributes still has
    /// its comment removed (the suite's Remove All relies on it).
    #[test]
    fn remove_comment_ignores_attribute_order() {
        let mut p = with_comments(&format!(
            "{COMMENTS_OPEN}<w:comment w:author=\"A\" w:id=\"3\"><w:p/></w:comment>\
             <w:comment w:id=\"4\" w:author=\"B\"><w:p/></w:comment></w:comments>"
        ));
        p.remove_comment(3);
        assert_eq!(
            p.part_text("word/comments.xml").unwrap(),
            format!(
                "{COMMENTS_OPEN}<w:comment w:id=\"4\" w:author=\"B\"><w:p/></w:comment></w:comments>"
            )
        );
    }

    /// #971: id 3 is not `w:id="30"`, nor a `w14:paraId`, and the root
    /// `<w:comments>` is never taken for a comment.
    #[test]
    fn remove_comment_matches_whole_id() {
        let xml = format!(
            "{COMMENTS_OPEN}<w:comment w:id=\"30\" w14:paraId=\"3\"><w:p/></w:comment></w:comments>"
        );
        let mut p = with_comments(&xml);
        p.remove_comment(3);
        assert_eq!(p.part_text("word/comments.xml").unwrap(), xml);
        assert_eq!(p.comment_xml("3"), None);
        p.remove_comment(30);
        assert_eq!(
            p.part_text("word/comments.xml").unwrap(),
            format!("{COMMENTS_OPEN}</w:comments>")
        );
    }

    /// #971: a comment's XML comes back byte-for-byte through
    /// `comment_xml` → `remove_comment` → `insert_comment_xml`.
    #[test]
    fn comment_xml_round_trips_through_insert() {
        let c2 = "<w:comment w:author=\"Ann &amp; Bob\" w:id=\"2\" w14:paraId=\"1A\">\
                  <w:p><w:r><w:rPr><w:b/></w:rPr><w:t>one</w:t></w:r></w:p>\
                  <w:p><w:r><w:t>two</w:t></w:r></w:p></w:comment>";
        let mut p = with_comments(&format!("{COMMENTS_OPEN}{c2}</w:comments>"));
        let raw = p.comment_xml("2").unwrap();
        assert_eq!(raw, c2);
        p.remove_comment(2);
        assert_eq!(p.comment_xml("2"), None);
        p.insert_comment_xml(&raw);
        assert_eq!(
            p.part_text("word/comments.xml").unwrap(),
            format!("{COMMENTS_OPEN}{c2}</w:comments>")
        );
    }

    /// #971: inserting into a package with no comments part creates it, its
    /// relationship and its content type, as `add_comment` does.
    #[test]
    fn insert_comment_xml_creates_the_part() {
        let mut p = load_package(&make_docx("<w:document/>")).unwrap();
        p.parts.push((
            "word/_rels/document.xml.rels".to_string(),
            br#"<Relationships><Relationship Id="rId1" Target="styles.xml"/></Relationships>"#
                .to_vec(),
        ));
        p.set_part(
            "[Content_Types].xml",
            br#"<?xml version="1.0"?><Types></Types>"#.to_vec(),
        );
        let c = "<w:comment w:id=\"5\"><w:p/></w:comment>";
        p.insert_comment_xml(c);
        assert_eq!(p.comment_xml("5").as_deref(), Some(c));
        let ct = p.part_text("[Content_Types].xml").unwrap();
        assert!(ct.contains("/word/comments.xml"), "{ct}");
        let rels = p.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Target=\"comments.xml\""), "{rels}");
        let reloaded = load_package(&save_package(&p)).unwrap();
        assert_eq!(reloaded.comment_xml("5").as_deref(), Some(c));
    }

    /// #971 FIX r1 M1: ids are matched as written, so a producer's `03` is
    /// found (and listed) as `03`, never as `3`, and the reverse.
    #[test]
    fn comment_ids_and_removal_use_the_id_as_written() {
        let three = "<w:comment w:id=\"3\"><w:p/></w:comment>";
        let mut p = with_comments(&format!(
            "{COMMENTS_OPEN}<w:comment w:author=\"A\" w:id=\"03\"><w:p/></w:comment>{three}</w:comments>"
        ));
        assert_eq!(p.comment_ids(), ["03", "3"]);
        p.remove_comment_id("03");
        assert_eq!(
            p.part_text("word/comments.xml").unwrap(),
            format!("{COMMENTS_OPEN}{three}</w:comments>")
        );
        p.remove_comment_id("03");
        assert_eq!(p.comment_ids(), ["3"], "3 is not 03");
    }

    /// #971 FIX r1 M1: a UTF-16 comments.xml is listed and edited in place,
    /// and stays UTF-16.
    #[test]
    fn comment_ids_read_a_utf16_part() {
        let xml = format!(
            "{COMMENTS_OPEN}<w:comment w:id=\"1\"><w:p/></w:comment>\
             <w:comment w:id=\"2\"><w:p/></w:comment></w:comments>"
        );
        let mut bytes = vec![0xff, 0xfe];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let mut p = load_package(&make_docx("<w:document/>")).unwrap();
        p.parts.push(("word/comments.xml".to_string(), bytes));
        assert_eq!(p.comment_ids(), ["1", "2"]);
        p.remove_comment_id("1");
        assert_eq!(p.comment_ids(), ["2"]);
        assert!(
            p.part("word/comments.xml")
                .unwrap()
                .starts_with(&[0xff, 0xfe])
        );
    }

    /// #971 FIX r2 f2: appending to a UTF-16 comments.xml keeps it UTF-16
    /// (BOM included) with both comments readable.
    #[test]
    fn insert_comment_xml_keeps_a_utf16_part() {
        let one = "<w:comment w:id=\"1\"><w:p/></w:comment>";
        let xml = format!("{COMMENTS_OPEN}{one}</w:comments>");
        let mut bytes = vec![0xff, 0xfe];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let mut p = load_package(&make_docx("<w:document/>")).unwrap();
        p.parts.push(("word/comments.xml".to_string(), bytes));
        let two = "<w:comment w:id=\"2\"><w:p/></w:comment>";
        p.insert_comment_xml(two);
        assert!(
            p.part("word/comments.xml")
                .unwrap()
                .starts_with(&[0xff, 0xfe, b'<', 0])
        );
        assert_eq!(p.comment_ids(), ["1", "2"]);
        assert_eq!(p.comment_xml("2").as_deref(), Some(two));
    }

    // ---- resolve / reopen / delete all (#621) -----------------------------

    /// `with_comments` over a package with content types and a document
    /// relationships part, as Word writes them.
    fn with_comment_plumbing(xml: &str) -> Package {
        let mut p = with_comments(xml);
        p.set_part(
            "[Content_Types].xml",
            br#"<?xml version="1.0"?><Types xmlns="t"><Override PartName="/word/comments.xml" ContentType="c"/></Types>"#
                .to_vec(),
        );
        p.parts.push((
            "word/_rels/document.xml.rels".to_string(),
            br#"<?xml version="1.0"?><Relationships xmlns="r"><Relationship Id="rId1" Type="t/comments" Target="comments.xml"/></Relationships>"#
                .to_vec(),
        ));
        p
    }

    fn resolved_of(p: &Package, id: &str) -> bool {
        crate::comments::parse_comments(p)
            .into_iter()
            .find(|c| c.id == id)
            .unwrap()
            .resolved
    }

    #[test]
    fn set_comment_resolved_creates_part_paraid_and_round_trips() {
        let mut p = with_comment_plumbing(&format!(
            "{COMMENTS_OPEN}<w:comment w:id=\"1\" w:author=\"A\"><w:p><w:r><w:t>hi</w:t></w:r></w:p></w:comment></w:comments>"
        ));
        assert!(!resolved_of(&p, "1"));
        assert!(p.set_comment_resolved("1", true));
        let comments = p.part_text("word/comments.xml").unwrap();
        assert!(comments.contains("<w:p w14:paraId=\""), "{comments}");
        assert!(comments.contains("xmlns:w14="), "{comments}");
        assert!(comments.contains("mc:Ignorable=\"w14\""), "{comments}");
        let ext = p.part_text("word/commentsExtended.xml").unwrap();
        assert!(ext.contains("w15:done=\"1\""), "{ext}");
        let ct = p.part_text("[Content_Types].xml").unwrap();
        assert!(ct.contains("/word/commentsExtended.xml"), "{ct}");
        let rels = p.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Target=\"commentsExtended.xml\""), "{rels}");
        assert!(resolved_of(&p, "1"));
        assert!(p.set_comment_resolved("1", false));
        let ext = p.part_text("word/commentsExtended.xml").unwrap();
        assert!(ext.contains("w15:done=\"0\""), "{ext}");
        assert_eq!(ext.matches("<w15:commentEx").count(), 1, "{ext}");
        assert!(!resolved_of(&p, "1"));
        assert!(!p.set_comment_resolved("9", true));
    }

    #[test]
    fn set_comment_resolved_patches_existing_entry_and_keeps_threads() {
        let mut p = with_comments(&format!(
            "{COMMENTS_OPEN}<w:comment w:id=\"1\"><w:p w14:paraId=\"0000AAAA\"/></w:comment>\
             <w:comment w:id=\"2\"><w:p w14:paraId=\"0000BBBB\"/></w:comment></w:comments>"
        ));
        let ext = "<w15:commentsEx xmlns:w15=\"x\" w15:keep=\"k\">\
                   <w15:commentEx w15:paraId=\"0000AAAA\" w15:done=\"0\"/>\
                   <w15:commentEx w15:paraId=\"0000BBBB\" w15:paraIdParent=\"0000AAAA\" w15:done=\"0\" w15:x=\"y\"/>\
                   </w15:commentsEx>";
        p.parts.push((
            "word/commentsExtended.xml".to_string(),
            ext.as_bytes().to_vec(),
        ));
        assert!(p.set_comment_resolved("2", true));
        let out = p.part_text("word/commentsExtended.xml").unwrap();
        assert_eq!(
            out,
            ext.replace(
                "w15:paraIdParent=\"0000AAAA\" w15:done=\"0\"",
                "w15:paraIdParent=\"0000AAAA\" w15:done=\"1\""
            )
        );
        assert!(resolved_of(&p, "2"));
        assert!(!resolved_of(&p, "1"));
        // Removing a comment drops its entry only.
        p.remove_comment_id("1");
        let out = p.part_text("word/commentsExtended.xml").unwrap();
        assert!(!out.contains("w15:paraId=\"0000AAAA\""), "{out}");
        assert!(out.contains("w15:x=\"y\""), "{out}");
    }

    #[test]
    fn dropping_empty_comment_parts_removes_every_part_and_relationship() {
        let mut p = with_comment_plumbing(&format!(
            "{COMMENTS_OPEN}<w:comment w:id=\"1\"><w:p/></w:comment>\
             <w:comment w:id=\"2\"><w:p/></w:comment></w:comments>"
        ));
        assert!(p.set_comment_resolved("1", true));
        for id in p.comment_ids() {
            p.remove_comment_id(&id);
        }
        p.drop_empty_comment_parts();
        for part in [
            "word/comments.xml",
            "word/commentsExtended.xml",
            "word/commentsIds.xml",
        ] {
            assert!(p.part(part).is_none(), "{part}");
        }
        let ct = p.part_text("[Content_Types].xml").unwrap();
        assert!(!ct.contains("comments"), "{ct}");
        let rels = p.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(!rels.contains("comments"), "{rels}");
        assert!(crate::comments::parse_comments(&p).is_empty());
    }

    /// A comments part listed before the document part: dropping it must
    /// not leave `doc_index` on the part after the document (#1107).
    #[test]
    fn dropping_comment_parts_listed_before_the_document_keeps_it_1107() {
        const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let parts = vec![
            (
                "[Content_Types].xml".to_string(),
                br#"<?xml version="1.0"?><Types/>"#.to_vec(),
            ),
            (
                "word/comments.xml".to_string(),
                format!("<w:comments xmlns:w=\"{W_NS}\"></w:comments>").into_bytes(),
            ),
            (
                "word/document.xml".to_string(),
                format!(
                    "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p><w:r><w:t>Hi</w:t></w:r></w:p></w:body></w:document>"
                )
                .into_bytes(),
            ),
            (
                "word/_rels/document.xml.rels".to_string(),
                br#"<?xml version="1.0"?><Relationships/>"#.to_vec(),
            ),
        ];
        let mut p = load_package(&write_zip(&parts)).unwrap();
        p.drop_empty_comment_parts();
        assert!(p.part("word/comments.xml").is_none());
        p.document
            .body
            .push(Block::Paragraph(crate::model::Paragraph::default()));
        let saved = load_package(&save_package(&p)).unwrap();
        assert!(saved.document.plain_text().starts_with("Hi"));
        assert_eq!(saved.document.body.len(), 2);
        let rels = saved.part_text("word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Relationships"), "{rels}");
    }

    // ---- Track Changes setting (#624) --------------------------------------

    fn settings_package(settings: &str) -> Package {
        load_package(&make_metadata_docx(
            "<w:document/>",
            Some(settings),
            None,
            &[],
        ))
        .unwrap()
    }

    #[test]
    fn track_revisions_sits_at_its_schema_position() {
        let mut p = settings_package(
            "<w:settings xmlns:w=\"w\"><w:zoom w:percent=\"100\"/><w:revisionView w:markup=\"0\"/>\
             <w:defaultTabStop w:val=\"720\"/><w:autoHyphenation/></w:settings>",
        );
        assert!(!p.track_revisions());
        assert!(p.set_track_revisions(true));
        assert!(p.track_revisions());
        let xml = p.part_text("word/settings.xml").unwrap();
        let at = |n: &str| xml.find(n).unwrap_or_else(|| panic!("{n} in {xml}"));
        assert!(
            at("<w:revisionView") < at("<w:trackRevisions/>")
                && at("<w:trackRevisions/>") < at("<w:defaultTabStop"),
            "{xml}"
        );
        assert!(!p.set_track_revisions(true), "already on");
        assert!(p.set_track_revisions(false));
        assert!(!p.track_revisions());
        assert!(
            !p.part_text("word/settings.xml")
                .unwrap()
                .contains("trackRevisions")
        );
    }

    #[test]
    fn track_revisions_replaces_an_explicit_off_and_creates_the_part() {
        let mut p = settings_package(
            "<w:settings xmlns:w=\"w\"><w:trackRevisions w:val=\"false\"/></w:settings>",
        );
        assert!(!p.track_revisions());
        assert!(p.set_track_revisions(true));
        let xml = p.part_text("word/settings.xml").unwrap();
        assert_eq!(xml.matches("trackRevisions").count(), 1, "{xml}");
        assert!(p.track_revisions());
        // Off over an explicit off removes it; no part: nothing to turn off.
        let mut off = settings_package(
            "<w:settings xmlns:w=\"w\"><w:trackRevisions w:val=\"0\"/></w:settings>",
        );
        assert!(off.set_track_revisions(false));
        assert!(
            !off.part_text("word/settings.xml")
                .unwrap()
                .contains("track")
        );
        let mut none = load_package(&make_metadata_docx("<w:document/>", None, None, &[])).unwrap();
        assert!(!none.set_track_revisions(false));
        assert!(none.part("word/settings.xml").is_none());
        // And turning it on creates the part with the flag in it.
        assert!(none.set_track_revisions(true));
        assert!(none.track_revisions());
    }
}
