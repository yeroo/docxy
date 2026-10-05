//! Display for Review (#625): how a document with tracked changes is shown.
//!
//! A view is derived on a clone and only ever rendered; the document itself,
//! its revision list and what a save writes never depend on it.

use crate::model::{Block, Document, Inline, RevisionKind, RunProps};
use std::borrow::Cow;

/// Word's Display for Review modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkupView {
    /// Every tracked change marked: inserts underlined, deletions struck.
    #[default]
    All,
    /// The final text with the deletions hidden and the inserts unmarked.
    Simple,
    /// The text as if every change were accepted.
    NoMarkup,
    /// The text as if every change were rejected.
    Original,
}

impl MarkupView {
    pub const ALL_VIEWS: [MarkupView; 4] = [
        MarkupView::All,
        MarkupView::Simple,
        MarkupView::NoMarkup,
        MarkupView::Original,
    ];

    /// Word's name for the mode.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All Markup",
            Self::Simple => "Simple Markup",
            Self::NoMarkup => "No Markup",
            Self::Original => "Original",
        }
    }

    /// The stable name control surfaces use.
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Simple => "simple",
            Self::NoMarkup => "none",
            Self::Original => "original",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL_VIEWS.into_iter().find(|v| v.name() == name)
    }

    /// The mode after this one, wrapping: how a single button steps through.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Simple,
            Self::Simple => Self::NoMarkup,
            Self::NoMarkup => Self::Original,
            Self::Original => Self::All,
        }
    }

    /// Whether the caret and edits address the text this mode shows. All and
    /// Simple Markup keep the document's own paragraphs and text offsets (a
    /// deletion is zero-width to the editor, an insertion is live text); No
    /// Markup and Original merge, drop and restore text, so their offsets are
    /// not the document's and the mode is view-only.
    pub fn is_editable(self) -> bool {
        matches!(self, Self::All | Self::Simple)
    }
}

impl Document {
    /// The document as `view` shows it. [`MarkupView::All`] is the document
    /// itself; the others are a transformed clone, never to be saved.
    pub fn markup_view(&self, view: MarkupView) -> Cow<'_, Document> {
        match view {
            MarkupView::All => Cow::Borrowed(self),
            MarkupView::Simple => {
                let mut doc = self.clone();
                simplify_blocks(&mut doc.body);
                Cow::Owned(doc)
            }
            MarkupView::NoMarkup => {
                let mut doc = self.clone();
                doc.accept_all_revisions();
                Cow::Owned(doc)
            }
            MarkupView::Original => {
                let mut doc = self.clone();
                doc.reject_all_revisions();
                Cow::Owned(doc)
            }
        }
    }
}

/// Simple Markup over `blocks`: deletions go, inserts keep their place and
/// lose their review cue. Paragraphs and their marks are left alone.
fn simplify_blocks(blocks: &mut [Block]) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => simplify_inlines(&mut p.content),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        simplify_blocks(&mut cell.blocks);
                    }
                }
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
}

fn simplify_inlines(content: &mut Vec<Inline>) {
    // Runs recorded as tracked insertions carry their cue on the run itself.
    content.iter_mut().for_each(|i| {
        let recorded = match i {
            Inline::Run(r) => r.props.tracked_insert.is_some(),
            Inline::Tab(p) | Inline::Break(_, p) => p.tracked_insert.is_some(),
            _ => false,
        };
        if recorded {
            clear_cues(i);
        }
    });
    content.retain(|i| {
        !matches!(
            i,
            Inline::Revision {
                kind: RevisionKind::Delete,
                ..
            }
        )
    });
    for inline in content {
        match inline {
            Inline::Revision { content, .. } => {
                simplify_inlines(content);
                content.iter_mut().for_each(clear_cues);
            }
            Inline::Hyperlink(link) => simplify_inlines(&mut link.content),
            Inline::TextBox { blocks, .. } => simplify_blocks(blocks),
            _ => {}
        }
    }
}

/// Take the underline/strike a revision wrapper added for display off every
/// run under `inline`; formatting the author set stays.
fn clear_cues(inline: &mut Inline) {
    let clear = |props: &mut RunProps| {
        if props.revision_cues.underline_added {
            props.underline = false;
        }
        if props.revision_cues.strike_added {
            props.strike = false;
        }
        props.revision_cues = Default::default();
    };
    match inline {
        Inline::Run(run) => clear(&mut run.props),
        Inline::Tab(props) | Inline::Break(_, props) => clear(props),
        Inline::Hyperlink(link) => {
            link.runs.iter_mut().for_each(|r| clear(&mut r.props));
            link.content.iter_mut().for_each(clear_cues);
        }
        Inline::Revision { content, .. } => content.iter_mut().for_each(clear_cues),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::serialize::document_to_xml;

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    /// `Alpha beta gamma delta.` with `new ` inserted and `beta ` deleted.
    fn fixture() -> Document {
        let xml = format!(
            "<w:document {W}><w:body><w:p>\
             <w:r><w:t xml:space=\"preserve\">Alpha </w:t></w:r>\
             <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t xml:space=\"preserve\">new </w:t></w:r></w:ins>\
             <w:del w:id=\"2\" w:author=\"A\"><w:r><w:delText xml:space=\"preserve\">beta </w:delText></w:r></w:del>\
             <w:r><w:t>gamma delta.</w:t></w:r></w:p></w:body></w:document>"
        );
        parse_document_xml(&xml, &Relationships::default())
    }

    fn text(doc: &Document) -> String {
        doc.plain_text().trim().to_string()
    }

    fn underlined_or_struck(doc: &Document) -> bool {
        fn walk(inlines: &[Inline]) -> bool {
            inlines.iter().any(|i| match i {
                Inline::Run(r) => r.props.underline || r.props.strike,
                Inline::Revision { content, .. } => walk(content),
                _ => false,
            })
        }
        doc.body.iter().any(|b| match b {
            Block::Paragraph(p) => walk(&p.content),
            _ => false,
        })
    }

    #[test]
    fn the_four_views_show_the_fixture_as_word_does() {
        let doc = fixture();
        assert_eq!(
            text(&doc.markup_view(MarkupView::NoMarkup)),
            "Alpha new gamma delta."
        );
        assert_eq!(
            text(&doc.markup_view(MarkupView::Original)),
            "Alpha beta gamma delta."
        );
        // All Markup is the document, cues and all.
        assert!(matches!(doc.markup_view(MarkupView::All), Cow::Borrowed(_)));
        assert!(underlined_or_struck(&doc));
    }

    #[test]
    fn simple_markup_hides_deletions_and_unmarks_inserts_in_place() {
        let doc = fixture();
        let simple = doc.markup_view(MarkupView::Simple);
        assert!(!underlined_or_struck(&simple), "no cue is left");
        assert_eq!(text(&simple), "Alpha new gamma delta.");
        // Same paragraphs, and the insert is still a wrapper: offsets stay the
        // document's own, which is why the mode can be edited.
        assert_eq!(simple.body.len(), doc.body.len());
        assert!(!simple.revisions().is_empty());
    }

    #[test]
    fn views_leave_the_document_and_its_save_untouched() {
        let doc = fixture();
        let before = document_to_xml(&doc);
        let revisions = doc.revisions().len();
        for view in MarkupView::ALL_VIEWS {
            let _ = doc.markup_view(view);
            assert_eq!(document_to_xml(&doc), before, "{view:?}");
            assert_eq!(doc.revisions().len(), revisions, "{view:?}");
        }
    }

    #[test]
    fn only_all_and_simple_are_editable_and_names_round_trip() {
        let editable: Vec<_> = MarkupView::ALL_VIEWS
            .into_iter()
            .filter(|v| v.is_editable())
            .collect();
        assert_eq!(editable, [MarkupView::All, MarkupView::Simple]);
        for v in MarkupView::ALL_VIEWS {
            assert_eq!(MarkupView::from_name(v.name()), Some(v));
        }
        assert_eq!(MarkupView::from_name("bogus"), None);
        let mut v = MarkupView::All;
        for _ in 0..4 {
            v = v.next();
        }
        assert_eq!(v, MarkupView::All);
    }
}
