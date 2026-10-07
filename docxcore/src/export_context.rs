//! What the Plain Text and Rich Text writers (#635) need beside the
//! document: its lists' definitions and its styles, as the package that a
//! save of it is written into defines them, and the document's final view.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::model::Document;
use crate::numbering::{Numbering, compute_markers, parse_numbering_xml};
use crate::package::Package;
use crate::styles::{StyleSheet, parse_styles_xml};

/// A document's numbering and styles, for an exporter.
#[derive(Debug, Clone, Default)]
pub struct ExportContext {
    pub numbering: Numbering,
    pub styles: StyleSheet,
}

impl ExportContext {
    /// `pkg`'s `word/numbering.xml` and `word/styles.xml`. A part the
    /// package lacks (or no package at all: a new document) is the one a new
    /// Markdown package defines (`numId` 1 bullets and 2 decimal, the heading
    /// styles), the package such a document is saved into.
    pub fn for_package(pkg: Option<&Package>) -> Self {
        let own = |name: &str| pkg.and_then(|p| p.part_text(name));
        let fallback = || crate::package::new_markdown_package(Document::default());
        let part = |name: &str| {
            own(name)
                .or_else(|| fallback().part_text(name))
                .unwrap_or_default()
        };
        ExportContext {
            numbering: parse_numbering_xml(&part("word/numbering.xml")),
            styles: parse_styles_xml(&part("word/styles.xml")),
        }
    }

    /// The marker of every list paragraph of `doc`, by tree path
    /// ([`compute_markers`]).
    pub fn markers(&self, doc: &Document) -> HashMap<Vec<usize>, String> {
        compute_markers(doc, &self.numbering)
    }
}

/// `doc` as it reads with its tracked changes accepted: what Plain Text and
/// Rich Text write, a deleted paragraph mark joining its paragraph to the
/// next included. A copy, made only when `doc` has a tracked change: `doc`
/// itself is left as it is. A revision the review module cannot accept (a
/// tracked move, a custom-XML or conflict range) stays in the copy, and both
/// writers then drop it whole, what it inserts included.
pub fn final_view(doc: &Document) -> Cow<'_, Document> {
    if doc.revisions().is_empty() {
        return Cow::Borrowed(doc);
    }
    let mut copy = doc.clone();
    copy.accept_all_revisions();
    Cow::Owned(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document with no tracked change is its own final view: no copy,
    /// no accept pass (FIX r3 #1).
    #[test]
    fn a_document_without_revisions_is_not_copied() {
        let doc = crate::markdown::from_markdown("# Title\n\nBody\n\n- item\n");
        assert!(matches!(final_view(&doc), Cow::Borrowed(d) if std::ptr::eq(d, &doc)));
        let tracked = crate::export_rtf::tests::deleted_mark_doc();
        assert!(matches!(final_view(&tracked), Cow::Owned(_)));
    }
}
