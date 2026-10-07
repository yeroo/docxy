//! The four OOXML word-processing file types (#636): `.docx`, `.docm`,
//! `.dotx` and `.dotm` differ only in the main document part's content type
//! and whether a VBA project may ride along. Word refuses a file whose
//! content type disagrees with its extension, so a save names the type its
//! target's extension calls for ([`Package::set_main_kind`]).

use super::{Package, part_rels_name, tag_attr};
use crate::load::start_tags;

/// The relationship type that names a document's VBA project.
const VBA_PROJECT_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";

/// A word-processing package's file type, from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// `.docx`
    Document,
    /// `.docm`
    MacroDocument,
    /// `.dotx`
    Template,
    /// `.dotm`
    MacroTemplate,
}

impl DocKind {
    /// The kind a path's extension names (any case), or `None` for any other
    /// extension.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let ext = path
            .as_ref()
            .extension()?
            .to_string_lossy()
            .to_ascii_lowercase();
        Self::from_extension(&ext)
    }

    /// The kind an extension (no dot, lower case) names.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext {
            "docx" => Self::Document,
            "docm" => Self::MacroDocument,
            "dotx" => Self::Template,
            "dotm" => Self::MacroTemplate,
            _ => return None,
        })
    }

    /// The main document part's content type.
    pub fn main_content_type(self) -> &'static str {
        match self {
            Self::Document => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            }
            Self::MacroDocument => "application/vnd.ms-word.document.macroEnabled.main+xml",
            Self::Template => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml"
            }
            Self::MacroTemplate => "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
        }
    }

    /// Whether a VBA project may be kept in this type.
    pub fn allows_macros(self) -> bool {
        matches!(self, Self::MacroDocument | Self::MacroTemplate)
    }

    /// Whether this is a template (`.dotx`, `.dotm`).
    pub fn is_template(self) -> bool {
        matches!(self, Self::Template | Self::MacroTemplate)
    }

    /// The kind a new document made from a template of this kind is: `.dotx`
    /// gives a `.docx`, `.dotm` a `.docm`; a document kind is itself.
    pub fn document_kind(self) -> Self {
        match self {
            Self::Template => Self::Document,
            Self::MacroTemplate => Self::MacroDocument,
            k => k,
        }
    }

    /// The extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Document => "docx",
            Self::MacroDocument => "docm",
            Self::Template => "dotx",
            Self::MacroTemplate => "dotm",
        }
    }
}

/// What [`Package::set_main_kind`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KindChange {
    /// The main part's content type was rewritten.
    pub retyped: bool,
    /// A VBA project was removed (a macro-free target).
    pub macros_dropped: bool,
}

impl Package {
    /// The main document part's content type, as `[Content_Types].xml`
    /// declares it.
    pub fn main_content_type(&self) -> Option<String> {
        let part_name = format!("/{}", self.parts.get(self.doc_index)?.0);
        let types = self.part_text("[Content_Types].xml")?;
        start_tags(&types, "Override")
            .into_iter()
            .find(|(_, tag)| override_names(tag, &part_name))
            .and_then(|(_, tag)| tag_attr(tag, "ContentType"))
    }

    /// Make this package the file type `kind`: the main part's content type
    /// says so, and a macro-free kind holds no VBA project (its part, the
    /// parts its own relationships name, the relationship to it and their
    /// overrides go). Only what disagrees is touched, so a package already of
    /// `kind` is left byte for byte as it is.
    pub fn set_main_kind(&mut self, kind: DocKind) -> KindChange {
        let mut change = KindChange::default();
        if !kind.allows_macros() {
            change.macros_dropped = self.strip_vba_project();
        }
        change.retyped = self.set_main_content_type(kind.main_content_type());
        change
    }

    fn set_main_content_type(&mut self, content_type: &str) -> bool {
        let Some(part_name) = self.parts.get(self.doc_index).map(|p| format!("/{}", p.0)) else {
            return false;
        };
        let Some(types) = self.part_text("[Content_Types].xml") else {
            return false;
        };
        let found = start_tags(&types, "Override")
            .into_iter()
            .find(|(_, tag)| override_names(tag, &part_name));
        let new = match found {
            Some((_, tag)) if tag_attr(tag, "ContentType").as_deref() == Some(content_type) => {
                return false;
            }
            Some((start, tag)) => {
                let retyped = super::set_attr_value(tag, "ContentType", content_type);
                format!(
                    "{}{}{}",
                    &types[..start],
                    retyped,
                    &types[start + tag.len()..]
                )
            }
            // No override at all: the part would take the `xml` default,
            // which is no document type. Add one.
            None => {
                let Some(close) = types.rfind("</Types>") else {
                    return false;
                };
                format!(
                    "{}<Override PartName=\"{part_name}\" ContentType=\"{content_type}\"/>{}",
                    &types[..close],
                    &types[close..]
                )
            }
        };
        self.set_part_text("[Content_Types].xml", &new)
    }

    /// Remove the main document's VBA project, as Excel's side does for a
    /// macro-free workbook (`gridcore::xlsx::save_xlsx_as`). Whether there
    /// was one.
    fn strip_vba_project(&mut self) -> bool {
        let Some(doc_name) = self.parts.get(self.doc_index).map(|p| p.0.clone()) else {
            return false;
        };
        let Some(rels_name) = part_rels_name(&doc_name) else {
            return false;
        };
        let Some(rels) = self.part_text(&rels_name) else {
            return false;
        };
        let dir = doc_name.rsplit_once('/').map_or("", |(d, _)| d);
        let mut doomed: Vec<String> = Vec::new();
        let mut kept = String::with_capacity(rels.len());
        let mut at = 0;
        for (start, tag) in start_tags(&rels, "Relationship") {
            if tag_attr(tag, "Type").as_deref() != Some(VBA_PROJECT_REL) {
                continue;
            }
            let Some(target) = tag_attr(tag, "Target") else {
                continue;
            };
            let end = element_end(&rels, start, tag);
            kept.push_str(&rels[at..start]);
            at = end;
            let project = resolve(dir, &target);
            // What the project's own relationships name (`vbaData.xml`).
            if let Some(own) = part_rels_name(&project) {
                if let Some(xml) = self.part_text(&own) {
                    let pdir = project.rsplit_once('/').map_or("", |(d, _)| d);
                    for (_, t) in start_tags(&xml, "Relationship") {
                        if tag_attr(t, "TargetMode").as_deref() == Some("External") {
                            continue;
                        }
                        if let Some(target) = tag_attr(t, "Target") {
                            doomed.push(resolve(pdir, &target));
                        }
                    }
                }
                doomed.push(own);
            }
            doomed.push(project);
        }
        if doomed.is_empty() {
            return false;
        }
        kept.push_str(&rels[at..]);
        self.set_part_text(&rels_name, &kept);
        if let Some(types) = self.part_text("[Content_Types].xml") {
            let gone: Vec<String> = doomed.iter().map(|d| format!("/{d}")).collect();
            let types = super::remove_tags_matching(&types, "Override", |tag| {
                tag_attr(tag, "PartName")
                    .is_some_and(|n| gone.iter().any(|g| g.eq_ignore_ascii_case(&n)))
            });
            self.set_part_text("[Content_Types].xml", &types);
        }
        self.retain_parts(|n| !doomed.iter().any(|d| d.eq_ignore_ascii_case(n)));
        true
    }
}

/// Whether an `<Override>` start tag is the one for `part_name` (part names
/// compare without regard to ASCII case, as OPC says).
fn override_names(tag: &str, part_name: &str) -> bool {
    tag_attr(tag, "PartName").is_some_and(|n| n.eq_ignore_ascii_case(part_name))
}

/// The end of the element whose start tag `tag` is at `start`: the tag
/// itself when it is empty (`/>`), else just past its closing tag.
fn element_end(xml: &str, start: usize, tag: &str) -> usize {
    let after = start + tag.len();
    if tag.ends_with("/>") {
        return after;
    }
    let name: String = tag[1..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
        .collect();
    let close = format!("</{name}>");
    xml[after..]
        .find(&close)
        .map_or(after, |e| after + e + close.len())
}

/// A relationship target resolved against the directory of the part that
/// holds it (`vbaProject.bin` from `word` is `word/vbaProject.bin`; a
/// leading `/` is package-absolute; `..` steps up).
fn resolve(dir: &str, target: &str) -> String {
    let mut segs: Vec<&str> = if let Some(abs) = target.strip_prefix('/') {
        return normalise(abs.split('/').collect());
    } else if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    segs.extend(target.split('/'));
    normalise(segs)
}

fn normalise(segs: Vec<&str>) -> String {
    let mut out: Vec<&str> = Vec::new();
    for s in segs {
        match s {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{load_package, new_package, save_package_keeping_document};

    fn doc_pkg() -> Package {
        new_package(crate::markdown::from_markdown("Hello"))
    }

    /// A `.docm`: the main part says macro-enabled and a VBA project with
    /// its data part hangs off the document.
    fn macro_pkg() -> Package {
        let mut pkg = doc_pkg();
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        let types = types
            .replace(
                DocKind::Document.main_content_type(),
                DocKind::MacroDocument.main_content_type(),
            )
            .replace(
                "</Types>",
                "<Default Extension=\"bin\" ContentType=\"application/vnd.ms-office.vbaProject\"/><Override PartName=\"/word/vbaData.xml\" ContentType=\"application/vnd.ms-word.vbaData+xml\"/></Types>",
            );
        pkg.set_part_text("[Content_Types].xml", &types);
        let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
        let rels = rels.replace(
            "</Relationships>",
            &format!("<Relationship Id=\"rId9\" Type=\"{VBA_PROJECT_REL}\" Target=\"vbaProject.bin\"/></Relationships>"),
        );
        pkg.set_part_text("word/_rels/document.xml.rels", &rels);
        pkg.parts
            .push(("word/vbaProject.bin".into(), b"VBA".to_vec()));
        pkg.parts.push((
            "word/_rels/vbaProject.bin.rels".into(),
            br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/wordVbaData" Target="vbaData.xml"/></Relationships>"#.to_vec(),
        ));
        pkg.parts
            .push(("word/vbaData.xml".into(), b"<wne:vbaSuppData/>".to_vec()));
        pkg
    }

    fn names(pkg: &Package) -> Vec<String> {
        pkg.parts.iter().map(|p| p.0.clone()).collect()
    }

    #[test]
    fn kinds_come_from_the_extension() {
        assert_eq!(DocKind::from_path("a/b.DOTX"), Some(DocKind::Template));
        assert_eq!(DocKind::from_path("b.dotm"), Some(DocKind::MacroTemplate));
        assert_eq!(DocKind::from_path("b.docm"), Some(DocKind::MacroDocument));
        assert_eq!(DocKind::from_path("b.docx"), Some(DocKind::Document));
        assert_eq!(DocKind::from_path("b.rtf"), None);
        assert_eq!(DocKind::from_path("b"), None);
        assert_eq!(DocKind::Template.document_kind(), DocKind::Document);
        assert_eq!(
            DocKind::MacroTemplate.document_kind(),
            DocKind::MacroDocument
        );
    }

    #[test]
    fn each_kind_writes_its_content_type_and_back() {
        for kind in [
            DocKind::Template,
            DocKind::MacroTemplate,
            DocKind::MacroDocument,
            DocKind::Document,
        ] {
            let mut pkg = doc_pkg();
            pkg.set_main_kind(kind);
            let saved = load_package(&save_package_keeping_document(&pkg)).unwrap();
            assert_eq!(
                saved.main_content_type().as_deref(),
                Some(kind.main_content_type())
            );
        }
        // .docx -> .dotx -> .docx gives back the original bytes.
        let mut pkg = doc_pkg();
        let before = pkg.part("[Content_Types].xml").unwrap().to_vec();
        assert!(pkg.set_main_kind(DocKind::Template).retyped);
        assert_ne!(pkg.part("[Content_Types].xml").unwrap(), before.as_slice());
        assert!(pkg.set_main_kind(DocKind::Document).retyped);
        assert_eq!(pkg.part("[Content_Types].xml").unwrap(), before.as_slice());
    }

    #[test]
    fn a_package_already_of_the_kind_is_untouched() {
        let mut pkg = doc_pkg();
        let before = pkg.parts.clone();
        assert_eq!(pkg.set_main_kind(DocKind::Document), KindChange::default());
        assert_eq!(pkg.parts, before);
        let mut pkg = macro_pkg();
        let before = pkg.parts.clone();
        assert_eq!(
            pkg.set_main_kind(DocKind::MacroDocument),
            KindChange::default()
        );
        assert_eq!(pkg.parts, before);
        // Idempotent.
        pkg.set_main_kind(DocKind::Template);
        let once = pkg.parts.clone();
        assert_eq!(pkg.set_main_kind(DocKind::Template), KindChange::default());
        assert_eq!(pkg.parts, once);
    }

    #[test]
    fn retyping_touches_only_the_content_types() {
        let mut pkg = doc_pkg();
        let before = pkg.parts.clone();
        pkg.set_main_kind(DocKind::Template);
        for ((n, b), (n0, b0)) in pkg.parts.iter().zip(&before) {
            assert_eq!(n, n0);
            if n != "[Content_Types].xml" {
                assert_eq!(b, b0, "{n}");
            }
        }
    }

    /// A single-quoted Override is retyped in place: one ContentType, the
    /// new one (FIX r1 #2).
    #[test]
    fn a_single_quoted_override_keeps_one_content_type() {
        let mut pkg = doc_pkg();
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        let single = types.replace(
            "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
            "<Override PartName='/word/document.xml' ContentType='application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/>",
        );
        assert_ne!(single, types);
        pkg.set_part_text("[Content_Types].xml", &single);
        assert!(pkg.set_main_kind(DocKind::Template).retyped);
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        let (_, tag) = start_tags(&types, "Override")
            .into_iter()
            .find(|(_, t)| t.contains("/word/document.xml"))
            .unwrap();
        assert_eq!(tag.matches("ContentType=").count(), 1, "{tag}");
        assert_eq!(
            tag,
            format!(
                "<Override PartName='/word/document.xml' ContentType='{}'/>",
                DocKind::Template.main_content_type()
            )
        );
        assert_eq!(
            pkg.main_content_type().as_deref(),
            Some(DocKind::Template.main_content_type())
        );
    }

    /// An Override written with space around its `=` is the main part's
    /// (FIX r2 #6): retyped in place, not joined by a second Override.
    #[test]
    fn an_override_with_spaced_attributes_is_retyped_not_duplicated() {
        let mut pkg = doc_pkg();
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        let spaced = types.replace(
            "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
            "<Override PartName = '/word/document.xml' ContentType = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/>",
        );
        assert_ne!(spaced, types);
        pkg.set_part_text("[Content_Types].xml", &spaced);
        assert!(pkg.set_main_kind(DocKind::Template).retyped);
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        assert_eq!(types.matches("/word/document.xml").count(), 1, "{types}");
        assert_eq!(
            pkg.main_content_type().as_deref(),
            Some(DocKind::Template.main_content_type())
        );
    }

    #[test]
    fn a_missing_override_is_added() {
        let mut pkg = doc_pkg();
        let types = pkg.part_text("[Content_Types].xml").unwrap();
        let types = types.replace(
            "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
            "",
        );
        pkg.set_part_text("[Content_Types].xml", &types);
        assert_eq!(pkg.main_content_type(), None);
        assert!(pkg.set_main_kind(DocKind::Template).retyped);
        assert_eq!(
            pkg.main_content_type().as_deref(),
            Some(DocKind::Template.main_content_type())
        );
    }

    #[test]
    fn a_macro_free_kind_drops_the_vba_project() {
        for kind in [DocKind::Document, DocKind::Template] {
            let mut pkg = macro_pkg();
            let change = pkg.set_main_kind(kind);
            assert!(change.macros_dropped && change.retyped, "{kind:?}");
            let names = names(&pkg);
            for gone in [
                "word/vbaProject.bin",
                "word/_rels/vbaProject.bin.rels",
                "word/vbaData.xml",
            ] {
                assert!(!names.contains(&gone.to_string()), "{kind:?}: {gone}");
            }
            let rels = pkg.part_text("word/_rels/document.xml.rels").unwrap();
            assert!(!rels.contains("vbaProject"), "{rels}");
            assert!(rels.contains("styles.xml"), "{rels}");
            let types = pkg.part_text("[Content_Types].xml").unwrap();
            assert!(!types.contains("/word/vbaData.xml"), "{types}");
            assert!(types.contains(kind.main_content_type()), "{types}");
            // The document still loads and is found at its index.
            let back = load_package(&save_package_keeping_document(&pkg)).unwrap();
            assert_eq!(back.document, pkg.document);
        }
    }

    #[test]
    fn a_macro_enabled_kind_keeps_the_vba_project_byte_for_byte() {
        let mut pkg = macro_pkg();
        let before = pkg.parts.clone();
        let change = pkg.set_main_kind(DocKind::MacroTemplate);
        assert!(change.retyped && !change.macros_dropped);
        for ((n, b), (_, b0)) in pkg.parts.iter().zip(&before) {
            if n != "[Content_Types].xml" {
                assert_eq!(b, b0, "{n}");
            }
        }
        assert_eq!(pkg.parts.len(), before.len());
    }

    #[test]
    fn targets_resolve_against_their_part() {
        assert_eq!(resolve("word", "vbaProject.bin"), "word/vbaProject.bin");
        assert_eq!(resolve("word", "/word/x.bin"), "word/x.bin");
        assert_eq!(resolve("word/sub", "../x.bin"), "word/x.bin");
        assert_eq!(resolve("", "x.bin"), "x.bin");
    }
}
